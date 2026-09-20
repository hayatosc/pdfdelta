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
    io::{self, Write},
    path::{Path, PathBuf},
};

use pdfdelta_core::review::{
    AgentReviewManifest, CaseCompleteness, CaseId, Cursor, Detail, EngineClass, EngineOutcome,
    Hypothesis, MAX_CURSOR_BYTES, RequiredEvidence, RetrievalAction, ReviewCase, ReviewPlan,
    ReviewQuestion, ReviewReason,
};
use serde::{Deserialize, Serialize};

use crate::review::bundle::{self, Artifact, MAX_BYTES};

/// The exact acquired bytes of one input, published beside the packets.
pub(crate) struct SourceDocument<'a> {
    pub name: &'a Path,
    pub bytes: &'a [u8],
}

/// Written last; a bundle without it is incomplete and must not be read.
const COMPLETION_MARKER: &str = "manifest.json";
const CASE_INDEX: &str = "cases/index.json";

/// Smallest response budget that can carry an envelope plus one record.
pub(crate) const MIN_OUTPUT_BYTES: usize = 512;

/// Detail levels this build can actually serve.
///
/// The planner describes what a case could be asked for in principle. This list
/// is what the command line answers today, and the manifest advertises exactly
/// this, so a caller is never invited to make a request that cannot be served.
const SERVED_DETAILS: [Detail; 3] = [Detail::Index, Detail::Text, Detail::Alternatives];

fn served(action: &RetrievalAction) -> bool {
    match action {
        RetrievalAction::List { .. } => true,
        RetrievalAction::Show { detail, .. } => SERVED_DETAILS.contains(detail),
        // Local rendering is not part of this command surface yet.
        RetrievalAction::Render { .. } => false,
    }
}

/// One case as the index lists it: enough to choose what to retrieve next, and
/// nothing that would make the index grow with document size.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexRecord {
    case: CaseId,
    question: ReviewQuestion,
    engine_class: EngineClass,
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
    alternatives_returned: usize,
    next: RetrievalAction,
}

impl IndexRecord {
    fn new(case: &ReviewCase) -> Self {
        Self {
            case: case.case_id.clone(),
            question: case.question,
            engine_class: case.engine_class,
            old_page: case.old.as_ref().and_then(|side| side.page_number),
            new_page: case.new.as_ref().and_then(|side| side.page_number),
            reasons: case.reasons.iter().map(|reason| reason.reason).collect(),
            completeness: case.completeness,
            required_evidence: case.required_evidence.clone(),
            alternatives_total: case.alternatives_total,
            alternatives_returned: case.alternatives_returned,
            next: RetrievalAction::Show {
                case: case.case_id.clone(),
                detail: Detail::Text,
                cursor: None,
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
        let mut records = Vec::with_capacity(plan.cases.len());
        for case in &plan.cases {
            let mut case = case.clone();
            case.available_actions.retain(served);
            records.push(IndexRecord::new(&case));
            // The identifier is validated ASCII without separators, so it
            // cannot escape the bundle directory.
            let name = format!("cases/{}.json", case.case_id);
            artifacts.push(bundle::file(directory, &name, &mut remaining, |out| {
                serde_json::to_writer(out, &case).map_err(io::Error::other)
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
            capability.available &= SERVED_DETAILS.contains(&capability.detail);
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
}

impl QueryError {
    fn new(error: &'static str, detail: impl Into<String>) -> Self {
        Self {
            schema: "agent-review-error/v1",
            error,
            detail: detail.into(),
            required_bytes: None,
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
    let envelope = serde_json::json!({
        "schema": pdfdelta_core::review::REVIEW_SCHEMA,
        "view": format!("{detail:?}").to_lowercase(),
        "bundle_id": manifest.bundle_id,
        "engine": manifest.engine,
        "case_id": stored.case_id,
        "question": stored.question,
        "engine_class": stored.engine_class,
        "pipeline": stored.pipeline,
        "completeness": stored.completeness,
        "required_evidence": stored.required_evidence,
        "available_actions": stored.available_actions,
        "alternatives_total": stored.alternatives_total,
        "alternatives_returned": 0,
        "omitted": 0,
        "next_cursor": serde_json::Value::Null,
    });
    match detail {
        Detail::Index => finish(envelope, cap),
        Detail::Text => text_view(envelope, &stored, cap),
        Detail::Alternatives => alternatives(envelope, &stored, &manifest.bundle_id, cursor, cap),
        Detail::Context | Detail::Visual => Err(QueryError::new(
            "unsupported_detail",
            format!("{detail:?} retrieval is not implemented"),
        )),
    }
}

/// Answers `--detail text` inside the byte cap.
///
/// Required metadata is placed first, then both sides' text shortened at a
/// sentence boundary with an explicit omission, and finally as many evidence
/// references as still fit. Nothing is cut in the middle, and a budget that
/// cannot carry even the metadata is refused with the size it would need.
fn text_view(
    mut envelope: serde_json::Value,
    case: &ReviewCase,
    cap: usize,
) -> Result<Vec<u8>, QueryError> {
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
        let expand = RetrievalAction::Show {
            case: case.case_id.clone(),
            detail: Detail::Text,
            cursor: None,
        };
        // A side's own reference list repeats the case-level evidence, which
        // is filled separately below; carrying both would spend the budget on
        // the same references twice.
        let mut text = text.clone();
        text.sources = Vec::new();
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
    let encoded: Vec<serde_json::Value> = case
        .evidence
        .iter()
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
    envelope["omitted"] = (encoded.len() - taken).into();
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

    #[test]
    fn unserved_retrieval_actions_are_not_advertised() {
        let case = CaseId::new("R0").expect("case id");
        assert!(served(&RetrievalAction::Show {
            case: case.clone(),
            detail: Detail::Text,
            cursor: None,
        }));
        assert!(!served(&RetrievalAction::Render { case }));
    }
}
