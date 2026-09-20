//! The local agent review bundle: publishing it, and answering bounded queries
//! against it.
//!
//! The bundle is written once, by the same run that produced the comparison, so
//! its evidence references point at that run's material. Queries then read only
//! the shards a particular decision needs; no query parses the whole bundle.
//!
//! Every response is a complete JSON document within an explicit byte cap. The
//! cap is a hard limit on the encoded document, including its metadata, escaped
//! strings, and any continuation cursor. Records that do not fit are omitted
//! with a cursor that advances, never cut in half.

use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::{Path, PathBuf},
};

use pdfdelta_core::{
    model::PageId,
    review::{
        AgentDecision, AgentReviewManifest, CaseCompleteness, CaseContext, CaseFinding, CaseId,
        Cursor, DecisionStatus, Detail, EngineClass, EngineOutcome, Hypothesis, MAX_CURSOR_BYTES,
        RequiredEvidence, RetrievalAction, ReviewCase, ReviewPlan, ReviewQuestion, ReviewReason,
        ReviewText, Side,
    },
};
use serde::{Deserialize, Serialize};

use crate::review::bundle::{self, Artifact, MAX_BYTES};

/// The exact acquired bytes of one input, published beside the packets.
pub(crate) struct SourceDocument<'a> {
    pub name: &'a Path,
    pub bytes: &'a [u8],
}

/// One composited page raster a case might need a picture of.
///
/// These are the rasters the comparison itself retained, in the profile it
/// declared. Publishing them keeps a rendered view an observation of that run
/// rather than a fresh rendering that might differ.
pub(crate) struct PageRaster<'a> {
    pub side: Side,
    pub page: PageId,
    /// The page box the renderer used, in PDF user space.
    pub page_bounds: Option<[f64; 4]>,
    pub width: u32,
    pub height: u32,
    pub rgb: &'a [u8],
    pub backend: String,
}

/// A published page raster, as the bundle records it.
#[derive(Clone, Serialize, Deserialize)]
struct PageRecord {
    side: Side,
    page_number: u32,
    page_index: PageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_bounds: Option<[f64; 4]>,
    width: u32,
    height: u32,
    /// Bundle-relative path of the published PNG.
    file: String,
    backend: String,
}

#[derive(Serialize, Deserialize)]
struct PageIndex {
    bundle_id: String,
    pages: Vec<PageRecord>,
    /// False when a page a case needs was not published.
    complete: bool,
}

/// Published page rasters, at most this many per bundle.
const MAX_PUBLISHED_PAGES: usize = 1_024;
const PAGE_INDEX: &str = "pages/index.json";

/// Written last; a bundle without it is incomplete and must not be read.
const COMPLETION_MARKER: &str = "manifest.json";
const CASE_INDEX: &str = "cases/index.json";

/// Smallest response budget that can carry an envelope plus one record.
pub(crate) const MIN_OUTPUT_BYTES: usize = 512;

/// Evidence references a text answer carries.
///
/// A reviewer decides from the quoted text, not from a list of glyph
/// identifiers; a decision cites references the case holds, which validation
/// checks against the stored case rather than against what a response showed.
/// Filling the remaining budget with identifiers would spend it on the least
/// decision-relevant field in the answer, so the list is capped and the
/// remainder is declared.
const MAX_TEXT_VIEW_EVIDENCE: usize = 16;

/// Detail levels this build can actually serve.
///
/// The planner describes what a case could be asked for in principle. This list
/// is what the command line answers today, and the manifest advertises exactly
/// this, so a caller is never invited to make a request that cannot be served.
const SERVED_DETAILS: [Detail; 5] = [
    Detail::Index,
    Detail::Text,
    Detail::Quote,
    Detail::Context,
    Detail::Alternatives,
];

/// Whether this build can answer one proposed retrieval.
///
/// A render action is offered only when the bundle actually published a page
/// raster for the case, so a caller is never invited to ask for a picture that
/// does not exist.
fn served(action: &RetrievalAction, rendered_pages: &BTreeSet<(Side, PageId)>) -> bool {
    match action {
        RetrievalAction::List { .. } => true,
        RetrievalAction::Show { detail, .. } => SERVED_DETAILS.contains(detail),
        RetrievalAction::Render { .. } => !rendered_pages.is_empty(),
    }
}

/// One case as the index lists it: enough to choose what to retrieve next, and
/// nothing that would make the index grow with document size.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexRecord {
    case: CaseId,
    question: ReviewQuestion,
    engine_class: EngineClass,
    /// What the engine established here, so a caller can tell a settled
    /// difference from material nothing reached.
    finding: CaseFinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_page: Option<u32>,
    /// Classifications only. The engine's own wording is carried by the case.
    reasons: Vec<ReviewReason>,
    completeness: CaseCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required_evidence: Vec<RequiredEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alternatives_total: Option<usize>,
    #[serde(default, skip_serializing_if = "is_zero")]
    alternatives_returned: usize,
    /// Scalars of material the case covers, for material no comparison
    /// examined. A listing carries the size rather than the text, so a
    /// reviewer can judge whether a page is worth opening without opening it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unexamined_scalars: Option<usize>,
    /// The detail level worth requesting for this case, to be read with
    /// [`IndexRecord::case`]. Both are closed vocabularies, so a host composes
    /// the retrieval without handling any text the document supplied.
    next: Detail,
}

const fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl IndexRecord {
    fn new(case: &ReviewCase) -> Self {
        let examined = case.finding.examined();
        // Only the sides a text answer would actually withhold: the rest is
        // quoted there, so naming its size here would double-count it.
        let scalars = [case.old_text.as_ref(), case.new_text.as_ref()]
            .into_iter()
            .flatten()
            .filter(|text| !examined && worth_locating(text, &case.case_id))
            .map(|text| text.text.chars().count())
            .sum::<usize>();
        Self {
            case: case.case_id.clone(),
            question: case.question,
            engine_class: case.engine_class,
            finding: case.finding,
            old_page: case.old.as_ref().and_then(|side| side.page_number),
            new_page: case.new.as_ref().and_then(|side| side.page_number),
            reasons: case.reasons.iter().map(|reason| reason.reason).collect(),
            completeness: case.completeness,
            required_evidence: case.required_evidence.clone(),
            alternatives_total: case.alternatives_total,
            alternatives_returned: case.alternatives_returned,
            unexamined_scalars: (!examined && scalars > 0).then_some(scalars),
            // A text answer for unexamined material repeats what this record
            // already carries, so the listing names the retrieval that adds
            // something instead: the quote a reviewer asks for when they
            // decide to examine the page.
            next: if examined || scalars == 0 {
                Detail::Text
            } else {
                Detail::Quote
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CaseIndex {
    bundle_id: String,
    records: Vec<IndexRecord>,
}

/// Publishes a plan as a bundle.
///
/// Files are written in dependency order and the manifest last, so a reader
/// that finds the manifest knows every artifact it names is already present.
pub(crate) fn write(
    directory: &Path,
    plan: &ReviewPlan,
    old: SourceDocument<'_>,
    new: SourceDocument<'_>,
    rasters: &[PageRaster<'_>],
) -> Result<(), String> {
    bundle::create_directory(directory)?;
    let result = (|| {
        let mut remaining = MAX_BYTES;
        let mut artifacts = vec![
            bundle::file(directory, "old.pdf", &mut remaining, |out| {
                out.write_all(old.bytes)
            })?,
            bundle::file(directory, "new.pdf", &mut remaining, |out| {
                out.write_all(new.bytes)
            })?,
        ];
        // Only pages some case could ask to see are published; the rest would
        // enlarge the bundle without answering any question in it.
        let needed: BTreeSet<(Side, PageId)> = plan
            .cases
            .iter()
            .flat_map(|case| &case.regions)
            .map(|region| (region.side, region.page_index))
            .collect();
        let mut pages = Vec::new();
        let mut pages_complete = true;
        for raster in rasters
            .iter()
            .filter(|raster| needed.contains(&(raster.side, raster.page)))
        {
            if pages.len() >= MAX_PUBLISHED_PAGES {
                pages_complete = false;
                break;
            }
            let file = format!("pages/{}-{}.png", raster.side.label(), raster.page.0);
            artifacts.push(bundle::file(directory, &file, &mut remaining, |out| {
                bundle::write_png(out, raster.width, raster.height, raster.rgb)
            })?);
            pages.push(PageRecord {
                side: raster.side,
                page_number: raster.page.0.saturating_add(1),
                page_index: raster.page,
                page_bounds: raster.page_bounds,
                width: raster.width,
                height: raster.height,
                file,
                backend: raster.backend.clone(),
            });
        }
        let published: BTreeSet<(Side, PageId)> = pages
            .iter()
            .map(|record| (record.side, record.page_index))
            .collect();
        if !pages.is_empty() || !pages_complete {
            let index = PageIndex {
                bundle_id: plan.manifest.bundle_id.as_str().to_owned(),
                pages,
                complete: pages_complete,
            };
            artifacts.push(bundle::file(
                directory,
                PAGE_INDEX,
                &mut remaining,
                |out| serde_json::to_writer(out, &index).map_err(io::Error::other),
            )?);
        }
        let mut records = Vec::with_capacity(plan.cases.len());
        for case in &plan.cases {
            let mut case = case.clone();
            let available: BTreeSet<(Side, PageId)> = case
                .regions
                .iter()
                .map(|region| (region.side, region.page_index))
                .filter(|key| published.contains(key))
                .collect();
            case.available_actions
                .retain(|action| served(action, &available));
            records.push(IndexRecord::new(&case));
            // The identifier is validated ASCII without separators, so it
            // cannot escape the bundle directory.
            let name = format!("cases/{}.json", case.case_id);
            artifacts.push(bundle::file(directory, &name, &mut remaining, |out| {
                serde_json::to_writer(out, &case).map_err(io::Error::other)
            })?);
        }
        for context in &plan.contexts {
            let name = format!("cases/{}.context.json", context.case_id);
            artifacts.push(bundle::file(directory, &name, &mut remaining, |out| {
                serde_json::to_writer(out, context).map_err(io::Error::other)
            })?);
        }
        let index = CaseIndex {
            bundle_id: plan.manifest.bundle_id.as_str().to_owned(),
            records,
        };
        artifacts.push(bundle::file(
            directory,
            CASE_INDEX,
            &mut remaining,
            |out| serde_json::to_writer(out, &index).map_err(io::Error::other),
        )?);
        let old_name = old.name.display().to_string();
        let new_name = new.name.display().to_string();
        let mut manifest = plan.manifest.clone();
        for capability in &mut manifest.capabilities {
            capability.available &= match capability.detail {
                Detail::Visual => !published.is_empty(),
                detail => SERVED_DETAILS.contains(&detail),
            };
        }
        bundle::file(directory, COMPLETION_MARKER, &mut remaining, |out| {
            serde_json::to_writer_pretty(
                out,
                &PublishedManifest::new(&manifest, &old_name, &new_name, &artifacts),
            )
            .map_err(io::Error::other)
        })?;
        Ok(())
    })();
    result.map_err(|error: String| {
        format!(
            "{error}; review directory {} is incomplete without {COMPLETION_MARKER}",
            directory.display()
        )
    })
}

#[derive(Serialize)]
struct PublishedManifest<'a> {
    #[serde(flatten)]
    manifest: &'a AgentReviewManifest,
    completion_marker: &'static str,
    /// Input names as given on the command line. They are caller-supplied text
    /// and are never used to choose an artifact path.
    old_document: &'a str,
    new_document: &'a str,
    files: &'a [Artifact],
}

impl PublishedManifest<'_> {
    fn new<'a>(
        manifest: &'a AgentReviewManifest,
        old_document: &'a str,
        new_document: &'a str,
        files: &'a [Artifact],
    ) -> PublishedManifest<'a> {
        PublishedManifest {
            manifest,
            completion_marker: COMPLETION_MARKER,
            old_document,
            new_document,
            files,
        }
    }
}

/// A stored manifest, read back for a query.
#[derive(Deserialize)]
struct StoredManifest {
    bundle_id: String,
    engine: EngineOutcome,
    export: StoredExport,
    cases: StoredCensus,
    #[serde(default)]
    unlocalized_gaps: Vec<serde_json::Value>,
    #[serde(default)]
    files: Vec<StoredArtifact>,
}

#[derive(Deserialize)]
struct StoredArtifact {
    name: String,
    sha256: String,
}

impl StoredManifest {
    /// Whether the manifest published an artifact under this name.
    fn lists(&self, name: &str) -> bool {
        self.files.iter().any(|artifact| artifact.name == name)
    }

    /// Reads one artifact and checks it against the digest the manifest
    /// recorded when the bundle was published.
    ///
    /// This detects a bundle that drifted or was modified after publication.
    /// It is not a defence against rewriting the manifest itself, which would
    /// require replacing the whole bundle.
    fn read_verified<T: serde::de::DeserializeOwned>(
        &self,
        directory: &Path,
        name: &str,
    ) -> Result<T, QueryError> {
        let path = artifact(directory, name);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            QueryError::new("unreadable_bundle", format!("{}: {error}", path.display()))
        })?;
        if !metadata.is_file() {
            return Err(QueryError::new(
                "unreadable_bundle",
                format!("{} is not a regular file", path.display()),
            ));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            QueryError::new("unreadable_bundle", format!("{}: {error}", path.display()))
        })?;
        let Some(expected) = self
            .files
            .iter()
            .find(|artifact| artifact.name == name)
            .map(|artifact| artifact.sha256.as_str())
        else {
            return Err(QueryError::new(
                "unlisted_artifact",
                format!("{name} is not listed in the manifest"),
            ));
        };
        use sha2::{Digest, Sha256};
        let found = crate::fs::lowercase_hex(&Sha256::digest(&bytes));
        if found != expected {
            return Err(QueryError::new(
                "tampered_bundle",
                format!("{name} does not match the digest the manifest records"),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            QueryError::new("malformed_bundle", format!("{}: {error}", path.display()))
        })
    }
}

#[derive(Deserialize, Serialize, Clone)]
struct StoredExport {
    export_complete: bool,
    #[serde(default)]
    omissions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct StoredCensus {
    total: usize,
}

/// Why a query could not be answered.
///
/// A query failure is not a comparison result: it never turns into an empty
/// answer or a silently shorter one.
#[derive(Debug, Serialize)]
pub(crate) struct QueryError {
    pub schema: &'static str,
    pub error: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_bytes: Option<usize>,
    /// Per-decision refusals, when the failure is a rejected submission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejections: Option<serde_json::Value>,
}

impl QueryError {
    fn new(error: &'static str, detail: impl Into<String>) -> Self {
        Self {
            schema: "agent-review-error/v1",
            error,
            detail: detail.into(),
            required_bytes: None,
            rejections: None,
        }
    }

    fn budget(detail: impl Into<String>, required: usize) -> Self {
        Self {
            required_bytes: Some(required),
            ..Self::new("budget_too_small", detail)
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, QueryError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        QueryError::new("unreadable_bundle", format!("{}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(QueryError::new(
            "unreadable_bundle",
            format!("{} is not a regular file", path.display()),
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        QueryError::new("unreadable_bundle", format!("{}: {error}", path.display()))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        QueryError::new("malformed_bundle", format!("{}: {error}", path.display()))
    })
}

/// Resolves a bundle-relative artifact path.
///
/// Only names this program generates are accepted, and the caller-supplied case
/// identifier is validated before it reaches the filesystem, so a query cannot
/// reach outside the bundle.
fn artifact(directory: &Path, name: &str) -> PathBuf {
    directory.join(name)
}

/// Opaque continuation, bound to one bundle and one query shape.
fn cursor_for(bundle: &str, query: &str, after: &str) -> Result<Cursor, QueryError> {
    Cursor::new(format!("{bundle}.{query}.{after}"))
        .map_err(|error| QueryError::new("invalid_cursor", error.to_string()))
}

fn cursor_position(cursor: &Cursor, bundle: &str, query: &str) -> Result<String, QueryError> {
    let mut parts = cursor.as_str().splitn(3, '.');
    let (found_bundle, found_query, after) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    if found_bundle != bundle {
        return Err(QueryError::new(
            "stale_cursor",
            "the cursor belongs to another bundle",
        ));
    }
    if found_query != query {
        return Err(QueryError::new(
            "stale_cursor",
            "the cursor belongs to another query",
        ));
    }
    Ok(after.to_owned())
}

/// Digest of the query shape a cursor is valid for.
fn query_digest(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    crate::fs::lowercase_hex(&hasher.finalize())[..12].to_owned()
}

/// Fills an envelope with as many records as the byte cap allows.
///
/// The envelope is serialized first with an empty record list and a
/// worst-case cursor reserved, so metadata is never displaced by records. A
/// record is either included whole or omitted whole.
fn fit_records(
    envelope: &serde_json::Value,
    records: &[serde_json::Value],
    cap: usize,
) -> Result<usize, QueryError> {
    let baseline = serde_json::to_vec(envelope)
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
        .len()
        // A continuation cursor may be added after the records are chosen.
        + MAX_CURSOR_BYTES
        + 16;
    if baseline >= cap {
        return Err(QueryError::budget(
            "the response metadata alone exceeds the output budget",
            baseline + 64,
        ));
    }
    let mut used = baseline;
    let mut taken = 0;
    for record in records {
        let encoded = serde_json::to_vec(record)
            .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
            .len()
            // One separator per additional record.
            + 1;
        if used + encoded > cap {
            break;
        }
        used += encoded;
        taken += 1;
    }
    if taken == 0 && !records.is_empty() {
        let smallest = serde_json::to_vec(&records[0])
            .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
            .len();
        return Err(QueryError::budget(
            "no record fits the output budget; raise it or request a smaller view",
            baseline + smallest + 1,
        ));
    }
    Ok(taken)
}

/// Answers `review list`.
pub(crate) fn list(
    directory: &Path,
    cursor: Option<&str>,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
    let manifest: StoredManifest = read_json(&artifact(directory, COMPLETION_MARKER))?;
    let index: CaseIndex = manifest.read_verified(directory, CASE_INDEX)?;
    if index.bundle_id != manifest.bundle_id {
        return Err(QueryError::new(
            "malformed_bundle",
            "the case index belongs to another bundle",
        ));
    }
    let query = query_digest(&["list"]);
    let start = match cursor {
        Some(cursor) => {
            let cursor = Cursor::new(cursor)
                .map_err(|error| QueryError::new("invalid_cursor", error.to_string()))?;
            let after = cursor_position(&cursor, &manifest.bundle_id, &query)?;
            index
                .records
                .iter()
                .position(|record| record.case.as_str() == after)
                .map(|position| position + 1)
                .ok_or_else(|| {
                    QueryError::new("stale_cursor", "the cursor names a case this bundle lacks")
                })?
        }
        None => 0,
    };
    let remaining = &index.records[start.min(index.records.len())..];
    let encoded: Vec<serde_json::Value> = remaining
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    let mut envelope = serde_json::json!({
        "schema": pdfdelta_core::review::REVIEW_SCHEMA,
        "view": "index",
        "bundle_id": manifest.bundle_id,
        "engine": manifest.engine,
        "export": manifest.export,
        "cases_total": manifest.cases.total,
        "unlocalized_gaps": manifest.unlocalized_gaps.len(),
        "returned": 0,
        "omitted": remaining.len(),
        "cases": [],
        "next_cursor": serde_json::Value::Null,
    });
    let taken = fit_records(&envelope, &encoded, cap)?;
    envelope["returned"] = taken.into();
    envelope["omitted"] = (remaining.len() - taken).into();
    envelope["cases"] = serde_json::Value::Array(encoded[..taken].to_vec());
    if taken < remaining.len() {
        let after = remaining[taken - 1].case.as_str();
        envelope["next_cursor"] = cursor_for(&manifest.bundle_id, &query, after)?
            .as_str()
            .into();
    }
    finish(envelope, cap)
}

/// Answers `review show`.
pub(crate) fn show(
    directory: &Path,
    case: &str,
    detail: Detail,
    cursor: Option<&str>,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
    let manifest: StoredManifest = read_json(&artifact(directory, COMPLETION_MARKER))?;
    let case_id =
        CaseId::new(case).map_err(|error| QueryError::new("invalid_case", error.to_string()))?;
    let stored: ReviewCase = manifest
        .read_verified(directory, &format!("cases/{case_id}.json"))
        .map_err(|error| {
            if matches!(error.error, "unreadable_bundle" | "unlisted_artifact") {
                QueryError::new("unknown_case", format!("{case_id} is not in this bundle"))
            } else {
                error
            }
        })?;
    // The run's own outcome belongs to the listing, which a review reads
    // first and which repeats it on every page. Carrying it again in each of
    // several hundred case answers charges for the same six numbers once per
    // case; the case's finding, engine class, and completeness are what a case
    // answer is asked for.
    let envelope = serde_json::json!({
        "schema": pdfdelta_core::review::REVIEW_SCHEMA,
        "view": format!("{detail:?}").to_lowercase(),
        "bundle_id": manifest.bundle_id,
        "case_id": stored.case_id,
        "question": stored.question,
        "engine_class": stored.engine_class,
        "finding": stored.finding,
        "pipeline": stored.pipeline,
        "completeness": stored.completeness,
        "required_evidence": stored.required_evidence,
        "available_actions": offered_actions(&stored)?,
        "alternatives_total": stored.alternatives_total,
        "alternatives_returned": 0,
        "omitted": 0,
        "next_cursor": serde_json::Value::Null,
    });
    match detail {
        Detail::Index => finish(envelope, cap),
        Detail::Text => text_view(envelope, &stored, cap, Quoting::AsExamined),
        Detail::Quote => text_view(envelope, &stored, cap, Quoting::Always),
        Detail::Alternatives => alternatives(envelope, &stored, &manifest.bundle_id, cursor, cap),
        Detail::Context => context_view(
            envelope,
            &manifest,
            directory,
            &stored,
            &manifest.bundle_id,
            cursor,
            cap,
        ),
        Detail::Visual => Err(QueryError::new(
            "unsupported_detail",
            format!("{detail:?} retrieval is not implemented"),
        )),
    }
}

/// The retrievals a case answer offers, without repeating the case identifier
/// the answer already carries in `case_id`.
///
/// Every action a stored case holds names that same case, so spelling it out
/// once per action charges for the identifier four or five times in every
/// answer. An action that names another case, if one is ever produced, is
/// passed through whole rather than being flattened onto the wrong identity.
fn offered_actions(case: &ReviewCase) -> Result<Vec<serde_json::Value>, QueryError> {
    case.available_actions
        .iter()
        .map(|action| {
            let compact = match action {
                RetrievalAction::Show {
                    case: named,
                    detail,
                    cursor: None,
                } if *named == case.case_id => {
                    Some(serde_json::json!({ "action": "show", "detail": detail }))
                }
                RetrievalAction::Render { case: named } if *named == case.case_id => {
                    Some(serde_json::json!({ "action": "render" }))
                }
                _ => None,
            };
            match compact {
                Some(value) => Ok(value),
                None => serde_json::to_value(action)
                    .map_err(|error| QueryError::new("encoding_failed", error.to_string())),
            }
        })
        .collect()
}

/// The action that returns a case's withheld text.
fn quote_action(case: &CaseId) -> RetrievalAction {
    RetrievalAction::Show {
        case: case.clone(),
        detail: Detail::Quote,
        cursor: None,
    }
}

/// Whether locating one side's text costs less than quoting it.
///
/// A locator is a fixed-size record — an interval, a length, and the retrieval
/// that returns the run — so for short material it is the more expensive of
/// the two. Withholding text that is cheaper to quote would spend a response's
/// budget to say less, so the two encodings are compared and the smaller one
/// is served. Nothing is hidden either way: both forms declare what they hold.
fn worth_locating(text: &ReviewText, case: &CaseId) -> bool {
    let encoded =
        |value: &ReviewText| serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len());
    encoded(&pdfdelta_core::review::withhold_text(
        text,
        Some(quote_action(case)),
    )) < encoded(text)
}

/// Whether a view quotes material the comparison never examined.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoting {
    /// Quote only what a comparison actually looked at. Unexamined material is
    /// located instead, with the action that quotes it.
    AsExamined,
    /// Quote whatever the case holds, because the caller asked for it.
    Always,
}

/// Answers `--detail text` and `--detail quote` inside the byte cap.
///
/// Required metadata is placed first, then both sides' text shortened at a
/// sentence boundary with an explicit omission, and finally as many evidence
/// references as still fit. Nothing is cut in the middle, and a budget that
/// cannot carry even the metadata is refused with the size it would need.
///
/// A case whose material no comparison reached is located rather than quoted
/// at [`Detail::Text`]: its text is the document, not an answer to the
/// question the case asks, and a reviewer reading every case would pay for the
/// whole document to learn nothing. The withheld run carries its length and
/// the [`Detail::Quote`] action that returns it, and the same references are
/// summarized by count instead of sampled, because sixteen of several thousand
/// identifiers for a page nothing examined name no evidence a reviewer can act
/// on.
fn text_view(
    mut envelope: serde_json::Value,
    case: &ReviewCase,
    cap: usize,
    quoting: Quoting,
) -> Result<Vec<u8>, QueryError> {
    let quote = quoting == Quoting::Always || case.finding.examined();
    let mut located = false;
    for (key, value) in [
        ("reasons", serde_json::to_value(&case.reasons)),
        ("assumptions", serde_json::to_value(&case.assumptions)),
        ("old", serde_json::to_value(&case.old)),
        ("new", serde_json::to_value(&case.new)),
    ] {
        envelope[key] =
            value.map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    }
    envelope["evidence"] = serde_json::Value::Array(Vec::new());
    envelope["old_text"] = serde_json::Value::Null;
    envelope["new_text"] = serde_json::Value::Null;
    let baseline = serde_json::to_vec(&envelope)
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
        .len();
    if baseline > cap {
        return Err(QueryError::budget(
            "the case metadata alone exceeds the output budget; request --detail index",
            baseline,
        ));
    }
    // Share what remains between the two sides, then keep whichever fits.
    let share = (cap - baseline) / 2;
    for (key, text) in [("old_text", &case.old_text), ("new_text", &case.new_text)] {
        let Some(text) = text else { continue };
        // A side's own reference list repeats the case-level evidence, which
        // is filled separately below; carrying both would spend the budget on
        // the same references twice.
        let mut text = text.clone();
        text.sources = Vec::new();
        if !quote && worth_locating(&text, &case.case_id) {
            located = true;
            let withheld =
                pdfdelta_core::review::withhold_text(&text, Some(quote_action(&case.case_id)));
            envelope[key] = serde_json::to_value(&withheld)
                .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
            continue;
        }
        let expand = quote_action(&case.case_id);
        let mut scalars = text.text.chars().count();
        let fitted;
        // Halve the retained length until the encoded side fits its share.
        loop {
            let candidate =
                pdfdelta_core::review::retain_text(&text, scalars, Some(expand.clone()));
            let encoded = serde_json::to_value(&candidate)
                .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
            let size = serde_json::to_vec(&encoded)
                .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
                .len();
            if size <= share || scalars == 0 {
                fitted = encoded;
                break;
            }
            scalars /= 2;
        }
        envelope[key] = fitted;
    }
    // References are summarized by count only where the text was withheld: an
    // answer that quotes its material is an answer a decision can cite.
    let sample = if located { 0 } else { MAX_TEXT_VIEW_EVIDENCE };
    let encoded: Vec<serde_json::Value> = case
        .evidence
        .iter()
        .take(sample)
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    let used = serde_json::to_vec(&envelope)
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
        .len();
    let mut taken = 0;
    let mut total = used;
    for record in &encoded {
        let size = serde_json::to_vec(record)
            .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?
            .len()
            + 1;
        if total + size > cap {
            break;
        }
        total += size;
        taken += 1;
    }
    envelope["evidence"] = serde_json::Value::Array(encoded[..taken].to_vec());
    envelope["evidence_total"] = case.evidence.len().into();
    envelope["omitted"] = (case.evidence.len() - taken).into();
    finish(envelope, cap)
}

/// Answers `--detail context` inside the byte cap.
///
/// A case with no gathered context answers with an empty, complete list rather
/// than an error: the absence of surrounding structure is itself the answer.
fn context_view(
    mut envelope: serde_json::Value,
    manifest: &StoredManifest,
    directory: &Path,
    case: &ReviewCase,
    bundle: &str,
    cursor: Option<&str>,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
    let name = format!("cases/{}.context.json", case.case_id);
    let stored: CaseContext = if manifest.lists(&name) {
        manifest.read_verified(directory, &name)?
    } else {
        CaseContext {
            case_id: case.case_id.clone(),
            items: Vec::new(),
            complete: true,
        }
    };
    let query = query_digest(&["context", case.case_id.as_str()]);
    let start = match cursor {
        Some(cursor) => {
            let cursor = Cursor::new(cursor)
                .map_err(|error| QueryError::new("invalid_cursor", error.to_string()))?;
            let after = cursor_position(&cursor, bundle, &query)?;
            after
                .parse::<usize>()
                .ok()
                .filter(|position| *position < stored.items.len())
                .map(|position| position + 1)
                .ok_or_else(|| {
                    QueryError::new("stale_cursor", "the cursor names an item this case lacks")
                })?
        }
        None => 0,
    };
    let remaining = &stored.items[start.min(stored.items.len())..];
    let encoded: Vec<serde_json::Value> = remaining
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    envelope["context_complete"] = stored.complete.into();
    envelope["context"] = serde_json::Value::Array(Vec::new());
    let taken = if encoded.is_empty() {
        0
    } else {
        fit_records(&envelope, &encoded, cap)?
    };
    envelope["context"] = serde_json::Value::Array(encoded[..taken].to_vec());
    envelope["omitted"] = (remaining.len() - taken).into();
    if taken < remaining.len() {
        envelope["next_cursor"] = cursor_for(bundle, &query, &(start + taken - 1).to_string())?
            .as_str()
            .into();
    }
    finish(envelope, cap)
}

fn alternatives(
    mut envelope: serde_json::Value,
    case: &ReviewCase,
    bundle: &str,
    cursor: Option<&str>,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
    let query = query_digest(&["alternatives", case.case_id.as_str()]);
    let start = match cursor {
        Some(cursor) => {
            let cursor = Cursor::new(cursor)
                .map_err(|error| QueryError::new("invalid_cursor", error.to_string()))?;
            let after = cursor_position(&cursor, bundle, &query)?;
            case.hypotheses
                .iter()
                .position(|hypothesis| hypothesis.id.as_str() == after)
                .map(|position| position + 1)
                .ok_or_else(|| {
                    QueryError::new(
                        "stale_cursor",
                        "the cursor names a hypothesis this case lacks",
                    )
                })?
        }
        None => 0,
    };
    let remaining: &[Hypothesis] = &case.hypotheses[start.min(case.hypotheses.len())..];
    let encoded: Vec<serde_json::Value> = remaining
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    envelope["hypotheses"] = serde_json::Value::Array(Vec::new());
    envelope["reasons"] = serde_json::to_value(&case.reasons)
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    let taken = fit_records(&envelope, &encoded, cap)?;
    envelope["alternatives_returned"] = taken.into();
    envelope["omitted"] = (remaining.len() - taken).into();
    envelope["hypotheses"] = serde_json::Value::Array(encoded[..taken].to_vec());
    if taken < remaining.len() {
        let after = remaining[taken - 1].id.as_str();
        envelope["next_cursor"] = cursor_for(bundle, &query, after)?.as_str().into();
    }
    finish(envelope, cap)
}

/// Encodes a response and proves it is inside the cap.
///
/// The check is fail-closed: an encoding that would exceed the cap is reported
/// as a budget error rather than truncated into invalid JSON.
fn finish(envelope: serde_json::Value, cap: usize) -> Result<Vec<u8>, QueryError> {
    let encoded = serde_json::to_vec(&envelope)
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    if encoded.len() > cap {
        return Err(QueryError::budget(
            "the response does not fit the output budget",
            encoded.len(),
        ));
    }
    Ok(encoded)
}

/// One produced image, as the render answer reports it.
#[derive(Serialize)]
struct RenderedImage {
    /// Filesystem path of the written PNG.
    path: String,
    sha256: String,
    width: u32,
    height: u32,
    side: Side,
    page_number: u32,
    page_index: PageId,
    purpose: RenderPurpose,
    /// The rendering profile the published page was produced with.
    backend: String,
    /// Pixel box this image covers inside the published page.
    pixel_bounds: [u32; 4],
}

/// What a produced image shows.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RenderPurpose {
    /// A crop around the case's own material, with margin for its surroundings.
    Crop,
    /// The whole page, because no usable box for the material was established.
    PageOverview,
}

/// Why a case's page could not be pictured.
#[derive(Serialize)]
struct UnavailableRegion {
    side: Side,
    page_number: u32,
    page_index: PageId,
    reason: &'static str,
}

/// Maps a PDF-space box onto the published raster.
///
/// The raster was produced from the page box at one sample per point, so the
/// mapping is a translation and a vertical flip. When the raster's dimensions
/// do not match that page box — a rotated page, or a different profile — no
/// mapping is established and the caller falls back to the whole page rather
/// than cropping at a guessed offset.
fn pixel_bounds(record: &PageRecord, bounds: [f64; 4]) -> Option<[u32; 4]> {
    let page = record.page_bounds?;
    let (page_width, page_height) = (page[2] - page[0], page[3] - page[1]);
    if !page_width.is_finite()
        || !page_height.is_finite()
        || page_width <= 0.0
        || page_height <= 0.0
    {
        return None;
    }
    let matches = |declared: f64, raster: u32| (declared.ceil() - f64::from(raster)).abs() <= 1.0;
    if !matches(page_width, record.width) || !matches(page_height, record.height) {
        return None;
    }
    // Margin scales with the material's own height, so small text keeps its
    // surrounding line and a large block is not swamped by white space.
    let margin = ((bounds[3] - bounds[1]).abs() * 0.75).clamp(4.0, 72.0);
    let left = (bounds[0] - page[0] - margin).max(0.0);
    let right = (bounds[2] - page[0] + margin).min(page_width);
    let top = (page[3] - bounds[3] - margin).max(0.0);
    let bottom = (page[3] - bounds[1] + margin).min(page_height);
    if !(left < right && top < bottom) {
        return None;
    }
    Some([
        left as u32,
        top as u32,
        right.ceil() as u32,
        bottom.ceil() as u32,
    ])
}

/// Produces local images for one case.
///
/// Images are cut from the page rasters the comparison retained, not from a
/// fresh rendering, so what a reviewer sees is the observation the engine had.
/// A page whose raster was never published, or whose geometry cannot be mapped,
/// is reported as unavailable instead of being approximated.
pub(crate) fn render(
    directory: &Path,
    case: &str,
    output: &Path,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
    let manifest: StoredManifest = read_json(&artifact(directory, COMPLETION_MARKER))?;
    let case_id =
        CaseId::new(case).map_err(|error| QueryError::new("invalid_case", error.to_string()))?;
    let stored: ReviewCase = manifest
        .read_verified(directory, &format!("cases/{case_id}.json"))
        .map_err(|error| {
            if matches!(error.error, "unreadable_bundle" | "unlisted_artifact") {
                QueryError::new("unknown_case", format!("{case_id} is not in this bundle"))
            } else {
                error
            }
        })?;
    let pages: PageIndex = if manifest.lists(PAGE_INDEX) {
        manifest.read_verified(directory, PAGE_INDEX)?
    } else {
        PageIndex {
            bundle_id: manifest.bundle_id.clone(),
            pages: Vec::new(),
            complete: true,
        }
    };
    bundle::validate_destination(output, &[])
        .map_err(|error| QueryError::new("invalid_destination", error))?;
    bundle::create_directory(output)
        .map_err(|error| QueryError::new("invalid_destination", error))?;

    let mut produced = Vec::new();
    let mut unavailable = Vec::new();
    let mut remaining = MAX_BYTES;
    for region in &stored.regions {
        let Some(record) = pages
            .pages
            .iter()
            .find(|record| record.side == region.side && record.page_index == region.page_index)
        else {
            unavailable.push(UnavailableRegion {
                side: region.side,
                page_number: region.page_number,
                page_index: region.page_index,
                reason: "no page raster was retained for this page",
            });
            continue;
        };
        let raster = decode_page(directory, &manifest, record)?;
        let (bounds, purpose) = match region
            .bounds
            .and_then(|bounds| pixel_bounds(record, bounds))
        {
            Some(bounds) => (bounds, RenderPurpose::Crop),
            None => (
                [0, 0, record.width, record.height],
                RenderPurpose::PageOverview,
            ),
        };
        let (width, height) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
        let mut cropped = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for row in bounds[1]..bounds[3] {
            let start = ((row as usize) * (record.width as usize) + bounds[0] as usize) * 3;
            let end = start + (width as usize) * 3;
            cropped.extend_from_slice(&raster[start..end]);
        }
        let name = format!(
            "{}-page-{}-{}.png",
            region.side.label(),
            region.page_index.0,
            match purpose {
                RenderPurpose::Crop => "crop",
                RenderPurpose::PageOverview => "overview",
            }
        );
        let artifact = bundle::file(output, &name, &mut remaining, |out| {
            bundle::write_png(out, width, height, &cropped)
        })
        .map_err(|error| QueryError::new("render_failed", error))?;
        produced.push(RenderedImage {
            path: output.join(&name).display().to_string(),
            sha256: artifact.sha256,
            width,
            height,
            side: region.side,
            page_number: region.page_number,
            page_index: region.page_index,
            purpose,
            backend: record.backend.clone(),
            pixel_bounds: bounds,
        });
    }

    let envelope = serde_json::json!({
        "schema": pdfdelta_core::review::REVIEW_SCHEMA,
        "view": "visual",
        "bundle_id": manifest.bundle_id,
        "case_id": stored.case_id,
        "images": produced,
        "unavailable": unavailable,
        "page_rasters_complete": pages.complete,
        "note": "Paths are written files. A model has not seen an image until the host loads it; a path alone is not evidence. Pixel differences show where samples differ, not what the words are.",
    });
    finish(envelope, cap)
}

/// Reads one published page back as raw RGB samples.
fn decode_page(
    directory: &Path,
    manifest: &StoredManifest,
    record: &PageRecord,
) -> Result<Vec<u8>, QueryError> {
    if !manifest.lists(&record.file) {
        return Err(QueryError::new(
            "unlisted_artifact",
            format!("{} is not listed in the manifest", record.file),
        ));
    }
    let path = artifact(directory, &record.file);
    let bytes = std::fs::read(&path).map_err(|error| {
        QueryError::new("unreadable_bundle", format!("{}: {error}", path.display()))
    })?;
    use sha2::{Digest, Sha256};
    let expected = manifest
        .files
        .iter()
        .find(|artifact| artifact.name == record.file)
        .map(|artifact| artifact.sha256.as_str())
        .unwrap_or_default();
    if crate::fs::lowercase_hex(&Sha256::digest(&bytes)) != expected {
        return Err(QueryError::new(
            "tampered_bundle",
            format!(
                "{} does not match the digest the manifest records",
                record.file
            ),
        ));
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|error| QueryError::new("malformed_bundle", error.to_string()))?;
    let mut samples = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut samples)
        .map_err(|error| QueryError::new("malformed_bundle", error.to_string()))?;
    if info.width != record.width || info.height != record.height {
        return Err(QueryError::new(
            "malformed_bundle",
            "the published page does not match its recorded dimensions",
        ));
    }
    samples.truncate(info.buffer_size());
    Ok(samples)
}

/// A submitted set of external assessments.
///
/// Either a bare array of decisions or an object carrying them, so a host can
/// add its own identity without changing the decisions themselves.
#[derive(Deserialize)]
#[serde(untagged)]
enum SubmittedDecisions {
    List(Vec<AgentDecision>),
    Envelope {
        #[serde(default)]
        agent: Option<serde_json::Value>,
        decisions: Vec<AgentDecision>,
    },
}

impl SubmittedDecisions {
    fn into_parts(self) -> (Option<serde_json::Value>, Vec<AgentDecision>) {
        match self {
            Self::List(decisions) => (None, decisions),
            Self::Envelope { agent, decisions } => (agent, decisions),
        }
    }
}

/// Stores validated external assessments as a new artifact.
///
/// The bundle is never modified. The result keeps the engine's own outcome and
/// the external answers in separate sections, because one is a comparison and
/// the other is an interpretation of it. A set containing any decision that
/// fails validation is refused as a whole: accepting the rest would silently
/// publish a partial review as a complete one.
pub(crate) fn import(
    directory: &Path,
    decisions: &Path,
    output: &Path,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
    let manifest: StoredManifest = read_json(&artifact(directory, COMPLETION_MARKER))?;
    let index: CaseIndex = manifest.read_verified(directory, CASE_INDEX)?;
    let submitted: SubmittedDecisions = read_json(decisions)?;
    let (agent, submitted) = submitted.into_parts();
    let bundle = pdfdelta_core::review::BundleId::new(manifest.bundle_id.clone())
        .map_err(|error| QueryError::new("malformed_bundle", error.to_string()))?;

    // Only the cases a decision names are loaded; a submission never forces a
    // read of the whole bundle.
    let mut cases = Vec::new();
    for decision in &submitted {
        if cases
            .iter()
            .any(|case: &ReviewCase| case.case_id == decision.case_id)
        {
            continue;
        }
        let name = format!("cases/{}.json", decision.case_id);
        if !manifest.lists(&name) {
            continue;
        }
        cases.push(manifest.read_verified(directory, &name)?);
    }
    let outcomes = pdfdelta_core::review::validate_decisions(&submitted, &cases, &bundle);
    let rejected: Vec<_> = outcomes
        .iter()
        .filter(|outcome| !outcome.rejections.is_empty())
        .map(|outcome| {
            serde_json::json!({
                "index": outcome.index,
                "case_id": outcome.case_id,
                "rejections": outcome
                    .rejections
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    if !rejected.is_empty() {
        return Err(QueryError {
            rejections: Some(serde_json::Value::Array(rejected)),
            ..QueryError::new(
                "rejected_decisions",
                "no assessment was stored; every submitted decision must validate",
            )
        });
    }

    let answered: BTreeSet<_> = submitted
        .iter()
        .map(|decision| decision.case_id.clone())
        .collect();
    let count = |status: DecisionStatus| {
        submitted
            .iter()
            .filter(|decision| decision.status == status)
            .count()
    };
    let reviewed = count(DecisionStatus::Changed) + count(DecisionStatus::UnchangedInScope);
    let stored = serde_json::json!({
        "schema": "agent-review-result/v1",
        "bundle_id": manifest.bundle_id,
        // The engine's own result, copied without reinterpretation.
        "engine": manifest.engine,
        "export": manifest.export,
        "external": {
            "origin": "external_agent",
            "agent": agent,
            "decisions": submitted,
        },
        "counts": {
            "cases_total": index.records.len(),
            "reviewed_cases": reviewed,
            "undetermined_cases": count(DecisionStatus::Undetermined),
            "need_more_evidence_cases": count(DecisionStatus::NeedMoreEvidence),
            "unanswered_cases": index
                .records
                .iter()
                .filter(|record| !answered.contains(&record.case))
                .count(),
            "unavailable_scopes": manifest.unlocalized_gaps.len(),
        },
        "note": "External assessments are review notes. They do not change the comparison, its coverage, or its exit status, and a validated decision is not a proof.",
    });
    let encoded = serde_json::to_vec_pretty(&stored)
        .map_err(|error| QueryError::new("encoding_failed", error.to_string()))?;
    crate::fs::write_output_atomically(output, "review result", |writer| {
        writer
            .write_all(&encoded)
            .map_err(|error| format!("cannot write review result: {error}"))
    })
    .map_err(|error| QueryError::new("unwritable_output", error))?;

    finish(
        serde_json::json!({
            "schema": pdfdelta_core::review::REVIEW_SCHEMA,
            "view": "import",
            "bundle_id": manifest.bundle_id,
            "output": output.display().to_string(),
            "accepted": submitted.len(),
            "counts": stored["counts"],
        }),
        cap,
    )
}

/// Writes one query answer to standard output.
pub(crate) fn emit(payload: &[u8]) -> Result<(), String> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout
        .write_all(payload)
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> serde_json::Value {
        serde_json::json!({ "schema": "agent-review/v1", "cases": [] })
    }

    #[test]
    fn a_budget_that_cannot_carry_metadata_is_an_explicit_error() {
        let error = fit_records(&envelope(), &[], 8).expect_err("rejected");
        assert_eq!(error.error, "budget_too_small");
        assert!(error.required_bytes.is_some_and(|bytes| bytes > 8));
    }

    #[test]
    fn a_budget_that_cannot_carry_one_record_names_the_required_size() {
        let records = vec![serde_json::json!({ "case": "R0123456789ab" })];
        let error = fit_records(&envelope(), &records, 200).expect_err("rejected");
        assert_eq!(error.error, "budget_too_small");
        let required = error.required_bytes.expect("a required size");
        assert!(required > 200, "{required}");
        // Raising the budget to what the error names must actually work.
        assert_eq!(
            fit_records(&envelope(), &records, required).expect("fits"),
            1
        );
    }

    #[test]
    fn a_cursor_is_refused_for_another_bundle_or_query() {
        let cursor = cursor_for("b0", "q0", "R1").expect("cursor");
        assert!(cursor_position(&cursor, "b0", "q0").is_ok());
        assert_eq!(
            cursor_position(&cursor, "b1", "q0")
                .expect_err("stale")
                .error,
            "stale_cursor"
        );
        assert_eq!(
            cursor_position(&cursor, "b0", "q1")
                .expect_err("stale")
                .error,
            "stale_cursor"
        );
    }

    fn page(width: u32, height: u32, bounds: Option<[f64; 4]>) -> PageRecord {
        PageRecord {
            side: Side::Old,
            page_number: 1,
            page_index: PageId(0),
            page_bounds: bounds,
            width,
            height,
            file: "pages/old-0.png".into(),
            backend: "fixture/1/page-rgb-white-72dpi".into(),
        }
    }

    #[test]
    fn a_crop_is_only_mapped_when_the_raster_matches_the_page_box() {
        let upright = page(612, 792, Some([0.0, 0.0, 612.0, 792.0]));
        let bounds = pixel_bounds(&upright, [100.0, 700.0, 300.0, 712.0]).expect("mapped");
        // PDF space is y-up and the raster is y-down, so a box near the top of
        // the page maps to a small row index.
        assert!(bounds[1] < 100, "{bounds:?}");
        assert!(bounds[0] < 100 && bounds[2] > 300, "{bounds:?}");

        // A rotated page renders to transposed dimensions, so no mapping is
        // established and the caller falls back to the whole page.
        let rotated = page(792, 612, Some([0.0, 0.0, 612.0, 792.0]));
        assert_eq!(pixel_bounds(&rotated, [100.0, 700.0, 300.0, 712.0]), None);

        // Without a page box there is nothing to map against.
        let unknown = page(612, 792, None);
        assert_eq!(pixel_bounds(&unknown, [100.0, 700.0, 300.0, 712.0]), None);
    }

    #[test]
    fn a_crop_stays_inside_the_published_raster() {
        let record = page(612, 792, Some([0.0, 0.0, 612.0, 792.0]));
        // Material at the very edge of the page still produces a box inside
        // the raster once the margin is clamped.
        let bounds = pixel_bounds(&record, [0.0, 0.0, 612.0, 792.0]).expect("mapped");
        assert_eq!(bounds[0], 0);
        assert_eq!(bounds[1], 0);
        assert!(bounds[2] <= record.width, "{bounds:?}");
        assert!(bounds[3] <= record.height, "{bounds:?}");
    }

    #[test]
    fn unserved_retrieval_actions_are_not_advertised() {
        let case = CaseId::new("R0").expect("case id");
        let none = BTreeSet::new();
        let some = BTreeSet::from([(Side::Old, PageId(0))]);
        assert!(served(
            &RetrievalAction::Show {
                case: case.clone(),
                detail: Detail::Text,
                cursor: None,
            },
            &none
        ));
        // A picture is offered only when the bundle published one to cut it from.
        assert!(!served(
            &RetrievalAction::Render { case: case.clone() },
            &none
        ));
        assert!(served(&RetrievalAction::Render { case }, &some));
    }
}
