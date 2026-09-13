//! Native acquisition runs outside the comparison process. Stored fields and
//! page metadata survive an independent glyph-extraction failure.

use std::{
    collections::BTreeSet,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, EvidenceFailure, EvidenceIssue, EvidenceLimits,
        EvidenceStore, PageEvidence, StructuredValue,
    },
    model::{Document, PageId},
    pdf::{ObjectRef, PageRef, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ExtractionOutcome},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    extraction_cache::{CeilingWriter, ExtractionCache, cache_key},
    fs::{lowercase_hex, parse_external_font_identities, parse_lopdf},
};

const MAX_HEADER: usize = 256 * 1024;
const MAX_RESPONSE: usize = 128 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Job {
    Metadata {
        forms: bool,
    },
    Content {
        structure: bool,
        first_structure_id: u64,
    },
}

#[derive(Serialize, Deserialize)]
struct Request {
    job: Job,
    password: Option<String>,
    font_identities: Vec<String>,
    cache_dir: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Acquisition {
    store: EvidenceStore,
    page_refs: Vec<ObjectRef>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Failure {
    kind: EvidenceFailure,
    reason: String,
    fatal: bool,
}

impl Failure {
    fn backend(reason: impl Into<String>) -> Self {
        Self {
            kind: EvidenceFailure::BackendFailure,
            reason: reason.into(),
            fatal: false,
        }
    }

    fn core(error: pdfdelta_core::Error) -> Self {
        let kind = match &error {
            pdfdelta_core::Error::LimitExceeded { .. } => EvidenceFailure::ResourceLimit,
            pdfdelta_core::Error::Unsupported(_) => EvidenceFailure::Unsupported,
            pdfdelta_core::Error::Backend(_) => EvidenceFailure::BackendFailure,
            _ => EvidenceFailure::Unresolved,
        };
        Self {
            kind,
            reason: error.to_string(),
            fatal: false,
        }
    }
}

fn backend() -> BackendIdentity {
    BackendIdentity {
        kind: BackendKind::NativeParser,
        name: "pdfdelta-native".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        profile: "content-stream-v2-paint-order-worker-v1".into(),
        model: None,
    }
}

fn revision(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

fn issue(store: &mut EvidenceStore, failure: &Failure, channels: &[Channel]) {
    store
        .issues
        .extend(channels.iter().map(|channel| EvidenceIssue {
            page: None,
            channel: *channel,
            sources: Vec::new(),
            kind: failure.kind,
            reason: format!("native acquisition: {}", failure.reason),
        }));
}

/// Acquires fields separately so a failed content worker cannot erase them.
///
/// # Errors
/// Returns invalid user configuration or fatal input failures without usable
/// metadata. Other acquisition failures remain explicit evidence obligations.
pub fn collect(
    bytes: &[u8],
    password: Option<&str>,
    font_identities: &[String],
    cache_dir: Option<&Path>,
    channels: &BTreeSet<Channel>,
) -> Result<(EvidenceStore, Vec<PageRef>), String> {
    // Validate user configuration before converting a child failure to evidence.
    parse_external_font_identities(font_identities)?;
    let hash = revision(bytes);
    let request = |job| Request {
        job,
        password: password.map(str::to_owned),
        font_identities: font_identities.to_vec(),
        cache_dir: cache_dir.map(Path::to_path_buf),
    };
    let forms = channels.contains(&Channel::Forms) || channels.contains(&Channel::Relations);
    let mut metadata = forms.then(|| invoke(bytes, &request(Job::Metadata { forms }), &hash));
    let first_structure_id = metadata
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map_or(0, |result| result.store.structured.len() as u64);
    let content = invoke(
        bytes,
        &request(Job::Content {
            structure: channels.contains(&Channel::Text) || channels.contains(&Channel::Relations),
            first_structure_id,
        }),
        &hash,
    );
    if metadata.is_none() && content.is_err() {
        metadata = Some(invoke(
            bytes,
            &request(Job::Metadata { forms: false }),
            &hash,
        ));
    }
    combine(hash, metadata, content, forms)
}

fn combine(
    hash: String,
    metadata: Option<Result<Acquisition, Failure>>,
    content: Result<Acquisition, Failure>,
    forms: bool,
) -> Result<(EvidenceStore, Vec<PageRef>), String> {
    let acquisition = match (metadata, content) {
        (Some(Ok(mut base)), Ok(content)) => {
            if base.store.pages != content.store.pages || base.page_refs != content.page_refs {
                issue(
                    &mut base.store,
                    &Failure::backend("workers disagree on page identity or geometry"),
                    &[Channel::Text, Channel::Relations],
                );
            } else {
                let fields = base.store.structured.len();
                let inventories = base.store.inventories.len();
                let issues = base.store.issues.len();
                base.store.native = content.store.native;
                base.store.structured.extend(content.store.structured);
                base.store.inventories.extend(content.store.inventories);
                base.store.issues.extend(content.store.issues);
                if let Err(error) = base.store.validate(EvidenceLimits::default()) {
                    base.store.native = Document::new(Vec::new());
                    base.store.structured.truncate(fields);
                    base.store.inventories.truncate(inventories);
                    base.store.issues.truncate(issues);
                    issue(
                        &mut base.store,
                        &Failure::core(error),
                        &[Channel::Text, Channel::Relations],
                    );
                }
            }
            base
        }
        (Some(Ok(mut base)), Err(failure)) => {
            issue(
                &mut base.store,
                &failure,
                &[Channel::Text, Channel::Relations],
            );
            base
        }
        (Some(Err(failure)), Ok(mut content)) => {
            if forms {
                issue(
                    &mut content.store,
                    &failure,
                    &[Channel::Forms, Channel::Relations],
                );
            }
            content
        }
        (None, Ok(content)) => content,
        (metadata, Err(failure)) => {
            if failure.fatal {
                return Err(failure.reason);
            }
            let mut store = EvidenceStore::from_native(
                hash,
                backend(),
                Vec::new(),
                ExtractionOutcome::complete(Document::new(Vec::new())),
                EvidenceLimits::default(),
            )
            .map_err(|error| error.to_string())?;
            issue(&mut store, &failure, &[Channel::Text, Channel::Relations]);
            if let Some(Err(metadata)) = metadata {
                issue(&mut store, &metadata, &[Channel::Forms, Channel::Visual]);
            }
            Acquisition {
                store,
                page_refs: Vec::new(),
            }
        }
    };
    acquisition
        .store
        .validate(EvidenceLimits::default())
        .map_err(|error| error.to_string())?;
    Ok((
        acquisition.store,
        acquisition.page_refs.into_iter().map(PageRef).collect(),
    ))
}

fn invoke(bytes: &[u8], request: &Request, hash: &str) -> Result<Acquisition, Failure> {
    let mut input =
        serde_json::to_vec(request).map_err(|error| Failure::backend(error.to_string()))?;
    if input.len() >= MAX_HEADER || bytes.len() > ParseLimits::default().max_input_bytes {
        return Err(Failure {
            kind: EvidenceFailure::ResourceLimit,
            reason: "native request exceeds its byte budget".into(),
            fatal: false,
        });
    }
    input.push(b'\n');
    input.extend_from_slice(bytes);
    let mut command =
        Command::new(std::env::current_exe().map_err(|error| Failure::backend(error.to_string()))?);
    command.arg("acquire-native");
    let output = crate::render::run_bounded(&mut command, &input, MAX_RESPONSE, TIMEOUT).map_err(
        |(kind, reason)| Failure {
            kind,
            reason,
            fatal: false,
        },
    )?;
    let acquisition: Result<Acquisition, Failure> = serde_json::from_slice(&output)
        .map_err(|error| Failure::backend(format!("invalid native response: {error}")))?;
    let acquisition = acquisition?;
    validate_response(&acquisition, request.job, hash)?;
    Ok(acquisition)
}

fn validate_response(value: &Acquisition, job: Job, hash: &str) -> Result<(), Failure> {
    let store = &value.store;
    store
        .validate(EvidenceLimits::default())
        .map_err(Failure::core)?;
    if store.revision != hash
        || store.backends != [backend()]
        || !store.rendered.is_empty()
        || value.page_refs.len() != store.pages.len()
        || value
            .page_refs
            .iter()
            .any(|reference| reference.object_number == 0)
        || store
            .pages
            .iter()
            .enumerate()
            .any(|(index, page)| page.page.0 as usize != index)
    {
        return Err(Failure::backend(
            "native response has inconsistent input, backend, or page identity",
        ));
    }
    let invalid_role =
        match job {
            Job::Metadata { .. } => {
                !store.native.items().is_empty()
                    || !store.native.vector_lines().is_empty()
                    || !store.native.marked_content().is_empty()
                    || !store.native.last_non_text_paint().is_empty()
                    || store
                        .inventories
                        .iter()
                        .any(|inventory| inventory.channel != Channel::Forms)
                    || store
                        .structured
                        .iter()
                        .any(|value| !matches!(value.value, StructuredValue::FormField { .. }))
            }
            Job::Content {
                first_structure_id, ..
            } => {
                store.inventories.iter().any(|inventory| {
                    !matches!(inventory.channel, Channel::Text | Channel::Relations)
                }) || store.structured.iter().any(|value| {
                    value.id < first_structure_id
                        || matches!(
                            value.value,
                            StructuredValue::FormField { .. }
                                | StructuredValue::RecognizedText { .. }
                        )
                })
            }
        };
    if invalid_role {
        return Err(Failure::backend(
            "native response exceeds its acquisition role",
        ));
    }
    Ok(())
}

/// Reads one framed request after installing process limits and writes a bounded
/// neutral result. Passwords never appear in the response.
///
/// # Errors
/// Returns exit code 2 for invalid framing or I/O, 3 for byte limits, and 4 when
/// process restrictions cannot be installed. Acquisition errors are typed replies.
pub fn worker() -> Result<(), u8> {
    crate::render::restrict_process(30)?;
    let mut input = std::io::BufReader::new(std::io::stdin().lock());
    let mut header = Vec::new();
    input
        .by_ref()
        .take(MAX_HEADER as u64 + 1)
        .read_until(b'\n', &mut header)
        .map_err(|_| 2)?;
    if header.len() > MAX_HEADER {
        return Err(3);
    }
    if header.last() != Some(&b'\n') {
        return Err(2);
    }
    let request: Request = serde_json::from_slice(&header).map_err(|_| 2)?;
    let mut bytes = Vec::new();
    let limit = ParseLimits::default().max_input_bytes;
    input
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| 2)?;
    if bytes.len() > limit {
        return Err(3);
    }
    let response = acquire(Arc::from(bytes), request);
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    let mut bounded = CeilingWriter::new(&mut stdout, MAX_RESPONSE);
    if serde_json::to_writer(&mut bounded, &response).is_err() {
        return Err(if bounded.oversized() { 3 } else { 2 });
    }
    stdout.flush().map_err(|_| 2)
}

fn acquire(bytes: Arc<[u8]>, request: Request) -> Result<Acquisition, Failure> {
    let limits = EvidenceLimits::default();
    if matches!(request.job, Job::Content { first_structure_id, .. } if first_structure_id > limits.max_items as u64)
    {
        return Err(Failure::backend(
            "native structure id exceeds the evidence budget",
        ));
    }
    let fonts =
        parse_external_font_identities(&request.font_identities).map_err(Failure::backend)?;
    let parsed = parse_lopdf(
        bytes.clone(),
        ParseLimits::default(),
        request.password.as_deref(),
    )
    .map_err(|error| {
        let mut failure = Failure::core(error);
        failure.fatal = failure.kind != EvidenceFailure::ResourceLimit;
        failure
    })?;
    let extractor = ContentStreamGlyphExtractor;
    let frames = extractor
        .page_frames(
            parsed.as_ref(),
            ExtractionLimits::default(),
            limits.max_pages,
        )
        .map_err(Failure::core)?;
    let pages = frames
        .iter()
        .enumerate()
        .map(|(index, frame)| PageEvidence {
            page: PageId(index as u32),
            bounds: frame.as_ref().ok().map(|frame| frame.canonical_bounds()),
        })
        .collect();
    let page_refs = parsed
        .pages()
        .map_err(Failure::core)?
        .into_iter()
        .map(|page| page.0)
        .collect();
    let extraction = match request.job {
        Job::Metadata { .. } => ExtractionOutcome::complete(Document::new(Vec::new())),
        Job::Content { .. } => {
            let cache = request.cache_dir.as_deref().map(ExtractionCache::new);
            let key = cache_key(
                &bytes,
                &ParseLimits::default(),
                &ExtractionLimits::default(),
                request.password.as_deref(),
                &fonts,
            );
            if let Some(outcome) = cache
                .as_ref()
                .and_then(|cache| cache.load(&key, &ExtractionLimits::default()))
            {
                outcome
            } else {
                let outcome = extractor
                    .extract_outcome_with_external_font_identities(
                        parsed.as_ref(),
                        ExtractionLimits::default(),
                        &fonts,
                    )
                    .map_err(Failure::core)?;
                if let Some(cache) = cache {
                    cache.store(&key, &outcome);
                }
                outcome
            }
        }
    };
    let mut store =
        EvidenceStore::from_native(revision(&bytes), backend(), pages, extraction, limits)
            .map_err(Failure::core)?;
    match request.job {
        Job::Metadata { forms } => {
            store.inventories.clear();
            if forms {
                match pdfdelta_core::document::extract_form_evidence(
                    parsed.as_ref(),
                    0,
                    0,
                    Default::default(),
                ) {
                    Ok(forms) => {
                        store.structured = forms.fields;
                        store.issues.extend(forms.issues);
                        store.inventories.push(forms.inventory);
                    }
                    Err(error) => issue(&mut store, &Failure::core(error), &[Channel::Forms]),
                }
            }
        }
        Job::Content {
            structure,
            first_structure_id,
        } => {
            if structure {
                match pdfdelta_core::document::extract_structure_evidence(
                    parsed.as_ref(),
                    &store.native,
                    0,
                    first_structure_id,
                    Default::default(),
                ) {
                    Ok(structure) => {
                        store.structured = structure.elements;
                        store.issues.extend(structure.issues);
                        store.inventories.push(structure.inventory);
                    }
                    Err(error) => issue(&mut store, &Failure::core(error), &[Channel::Relations]),
                }
            }
        }
    }
    store.validate(limits).map_err(Failure::core)?;
    Ok(Acquisition { store, page_refs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> Acquisition {
        let mut store = EvidenceStore::from_native(
            "fixture".into(),
            backend(),
            vec![PageEvidence {
                page: PageId(0),
                bounds: None,
            }],
            ExtractionOutcome::complete(Document::new(Vec::new())),
            EvidenceLimits::default(),
        )
        .expect("metadata fixture");
        store.inventories.clear();
        Acquisition {
            store,
            page_refs: vec![ObjectRef {
                object_number: 1,
                generation: 0,
            }],
        }
    }

    #[test]
    fn responses_cannot_forge_input_page_or_acquisition_scope() {
        let job = Job::Metadata { forms: false };
        assert!(validate_response(&metadata(), job, "fixture").is_ok());
        for mutation in 0..4 {
            let mut response = metadata();
            match mutation {
                0 => response.store.revision = "other input".into(),
                1 => response.page_refs.clear(),
                2 => response.store.pages[0].page = PageId(1),
                3 => response
                    .store
                    .inventories
                    .push(pdfdelta_core::document::ChannelInventory {
                        page: None,
                        channel: Channel::Text,
                        backend: 0,
                        sources: Vec::new(),
                        complete: true,
                    }),
                _ => unreachable!(),
            }
            assert!(validate_response(&response, job, "fixture").is_err());
        }
    }

    #[test]
    fn inconsistent_workers_do_not_attach_content_to_different_pages() {
        let mut content = metadata();
        content.page_refs[0].object_number = 2;
        let (store, pages) = combine("fixture".into(), Some(Ok(metadata())), Ok(content), false)
            .expect("independent metadata remains available");
        assert_eq!(pages[0].0.object_number, 1);
        assert!(!store.inventory_complete(Some(PageId(0)), Channel::Text));
        assert!(
            store
                .issues
                .iter()
                .any(|issue| issue.reason.contains("disagree on page identity"))
        );
    }
}
