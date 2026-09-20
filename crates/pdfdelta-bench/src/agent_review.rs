//! Rust-only audit of an exported agent review bundle.
//!
//! The audit checks the invariants a packet consumer depends on and measures
//! what a retrieval loop would have to read, against the baselines a host would
//! otherwise use. It runs no model, calls no external analysis service, and
//! needs no network, so it can gate changes to the packet contract in ordinary
//! CI.
//!
//! Costs are reported in **bytes**, which are exact. Tokens are not measured
//! here: a token count depends on a tokenizer and its version, and quoting one
//! without naming them would invite comparing figures that were produced
//! differently. A host's own usage, when supplied, is carried through verbatim
//! in its own section and is never mixed with these measurements.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::Arc,
};

use pdfdelta_core::{
    model::{DecodedText, Document, Glyph},
    pdf::ParseLimits,
    review::{CaseContext, ReviewCase, Side},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BenchError, Result};

/// Audit schema, versioned independently of the packets it inspects.
pub const AUDIT_SCHEMA: &str = "agent-review-audit/v1";

/// What an audit found wrong.
///
/// A finding is a violated invariant, not a judgement about review quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A published file does not match the digest the manifest records.
    ArtifactDigestMismatch,
    /// A file the manifest names is missing.
    MissingArtifact,
    /// A file exists in the bundle that the manifest does not name.
    UnlistedArtifact,
    /// The case index and a case packet disagree.
    IndexShardMismatch,
    /// Two cases share one identifier.
    DuplicateCaseId,
    /// A context shard names a case the bundle does not contain.
    OrphanContext,
    /// A case names a page whose raster was never published, while the bundle
    /// still offers it a rendered view.
    UnpublishedRegionPage,
    /// A case returned more alternatives than it says exist.
    AlternativesAccounting,
    /// A case claims a closed enumeration without a total, or the reverse.
    CompletenessAccounting,
    /// A case carries no reason at all, so nothing explains why it is open.
    MissingReason,
}

/// One violated invariant, with enough detail to locate it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub detail: String,
}

impl Finding {
    fn new(kind: FindingKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// What a retrieval loop reads, and what it is being compared against.
///
/// `packets` is the upper bound of the loop: the index plus every case packet.
/// A real loop reads fewer, because it stops once it can decide.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// The unit every figure here is in.
    pub measured_in: &'static str,
    /// Named only when a tokenizer actually produced a figure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<String>,
    pub bundle_bytes: usize,
    pub index_bytes: usize,
    pub case_packet_bytes: usize,
    pub context_bytes: usize,
    pub page_raster_bytes: usize,
    pub source_pdf_bytes: usize,
    /// Index plus every case packet. This is what the export occupies, not
    /// what a review reads: a query's answer is capped by its own output
    /// budget, and a loop stops once it can decide.
    pub packets_bytes: usize,
    /// What one case costs to read, before a response budget trims it.
    pub case_bytes_median: usize,
    pub case_bytes_p95: usize,
    pub case_bytes_max: usize,
    /// Both documents' extracted text, as a host would paste it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_full_text_bytes: Option<usize>,
    /// The engine's own JSON report, when one was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_report_bytes: Option<usize>,
    /// Share of the full-text baseline avoided by reading the index and one
    /// median case instead, in per-mille.
    ///
    /// This models the start of a retrieval loop: what it costs to see every
    /// open question and then open one of them. It is an integer per-mille so
    /// the record stays exactly comparable between runs, and it is a byte
    /// ratio — not a token ratio, and not a measurement of review quality.
    ///
    /// A negative figure means the loop's first step already costs more than
    /// handing over the whole text, which is the honest outcome when the
    /// comparison resolved little.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_read_reduction_per_mille: Option<i64>,
}

/// A host's own reported usage, carried through unchanged.
///
/// This is the host's measurement of its own run. It is stored beside the byte
/// figures rather than merged into them, because it was produced by a different
/// instrument.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostUsage {
    #[serde(flatten)]
    pub reported: serde_json::Value,
}

/// The audit of one bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BundleAudit {
    pub schema: &'static str,
    pub bundle_id: String,
    pub pipeline: String,
    pub cases: usize,
    pub unlocalized_gaps: usize,
    pub export_complete: bool,
    pub comparison_complete: bool,
    pub findings: Vec<Finding>,
    pub cost: Cost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_usage: Option<HostUsage>,
}

impl BundleAudit {
    /// Whether every checked invariant held.
    #[must_use]
    pub fn sound(&self) -> bool {
        self.findings.is_empty()
    }
}

#[derive(Deserialize)]
struct Manifest {
    bundle_id: String,
    pipeline: String,
    engine: EngineSection,
    export: ExportSection,
    #[serde(default)]
    unlocalized_gaps: Vec<serde_json::Value>,
    #[serde(default)]
    files: Vec<ArtifactRecord>,
}

#[derive(Deserialize)]
struct EngineSection {
    comparison_complete: bool,
}

#[derive(Deserialize)]
struct ExportSection {
    export_complete: bool,
}

#[derive(Deserialize)]
struct ArtifactRecord {
    name: String,
    bytes: usize,
    sha256: String,
}

#[derive(Deserialize)]
struct CaseIndex {
    bundle_id: String,
    records: Vec<IndexRecord>,
}

#[derive(Deserialize)]
struct IndexRecord {
    case: String,
    question: String,
    /// Absent from a listing that returned none, so a record that omits it
    /// reads as zero rather than as a malformed index.
    #[serde(default)]
    alternatives_returned: usize,
    #[serde(default)]
    alternatives_total: Option<usize>,
}

#[derive(Deserialize)]
struct PageIndex {
    #[serde(default)]
    pages: Vec<PageRecord>,
}

#[derive(Deserialize)]
struct PageRecord {
    side: Side,
    page_index: pdfdelta_core::model::PageId,
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|error| {
        BenchError::InvalidInput(format!("cannot read {}: {error}", path.display()))
    })
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|error| {
        BenchError::InvalidInput(format!("cannot parse {}: {error}", path.display()))
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Extracted glyph text of one document, as a host would paste it.
fn extracted_text(bytes: Vec<u8>) -> String {
    let source =
        ParserBackedGlyphSource::new(pdfdelta_core::pdf::LopdfParser, ContentStreamGlyphExtractor);
    let Ok(outcome) = source.extract_outcome(
        Arc::from(bytes),
        ParseLimits::default(),
        ExtractionLimits::default(),
    ) else {
        return String::new();
    };
    let document: &Document<Glyph> = outcome.document();
    let mut text = String::new();
    for glyph in document.items() {
        match &glyph.text {
            DecodedText::Mapped(mapped) => text.push_str(mapped),
            // An unmapped glyph still occupies a reader's attention; its raw
            // codes are what a host would have to show.
            DecodedText::Unmapped { .. } => {
                for byte in &glyph.raw_code {
                    use std::fmt::Write as _;
                    let _ = write!(text, "\\x{byte:02x}");
                }
            }
        }
    }
    text
}

/// Audits one exported bundle.
///
/// `report` is the engine's own JSON report for the same comparison, used as a
/// size baseline. `host_usage` is a host's own record of what a review run
/// cost it, carried through unchanged.
///
/// # Errors
/// Returns an error when the bundle cannot be read or parsed at all. A bundle
/// that parses but violates an invariant is returned with findings, because the
/// point of the audit is to report them rather than to stop.
pub fn audit_bundle(
    directory: &Path,
    report: Option<&Path>,
    host_usage: Option<&Path>,
) -> Result<BundleAudit> {
    audit_bundle_with_baseline(directory, report, host_usage, None)
}

/// Audits one bundle and optionally writes the baseline text it measured.
///
/// `baseline_text` receives `old.txt` and `new.txt`: the same extraction the
/// comparison used, written out so an external tokenizer can count the very
/// text a host would otherwise paste. Writing it here keeps the baseline and
/// the packets on one extractor; counting a different library's reading would
/// compare two documents rather than two ways of reviewing one.
///
/// # Errors
/// As [`audit_bundle`], plus a failure to publish the baseline text.
pub fn audit_bundle_with_baseline(
    directory: &Path,
    report: Option<&Path>,
    host_usage: Option<&Path>,
    baseline_text: Option<&Path>,
) -> Result<BundleAudit> {
    let manifest_path = directory.join("manifest.json");
    let manifest_bytes = read(&manifest_path)?;
    let manifest: Manifest = parse(&manifest_path, &manifest_bytes)?;
    let mut findings = Vec::new();

    let mut listed: BTreeMap<String, &ArtifactRecord> = BTreeMap::new();
    let mut bundle_bytes = manifest_bytes.len();
    let mut index_bytes = 0;
    let mut case_packet_bytes = 0;
    let mut context_bytes = 0;
    let mut page_raster_bytes = 0;
    let mut source_pdf_bytes = 0;
    let mut case_sizes: Vec<usize> = Vec::new();
    for artifact in &manifest.files {
        listed.insert(artifact.name.clone(), artifact);
        let path = directory.join(&artifact.name);
        let Ok(bytes) = fs::read(&path) else {
            findings.push(Finding::new(
                FindingKind::MissingArtifact,
                artifact.name.clone(),
            ));
            continue;
        };
        if hex(&Sha256::digest(&bytes)) != artifact.sha256 || bytes.len() != artifact.bytes {
            findings.push(Finding::new(
                FindingKind::ArtifactDigestMismatch,
                artifact.name.clone(),
            ));
        }
        bundle_bytes += bytes.len();
        if artifact.name == "cases/index.json" {
            index_bytes += bytes.len();
        } else if artifact.name.ends_with(".context.json") {
            context_bytes += bytes.len();
        } else if artifact.name.starts_with("cases/") {
            case_packet_bytes += bytes.len();
            case_sizes.push(bytes.len());
        } else if artifact.name.starts_with("pages/") && extension(&artifact.name) == Some("png") {
            page_raster_bytes += bytes.len();
        } else if extension(&artifact.name) == Some("pdf") {
            source_pdf_bytes += bytes.len();
        }
    }

    // Files nobody vouched for cannot be verified, so they are reported.
    for entry in walk(directory) {
        let name = entry
            .strip_prefix(directory)
            .unwrap_or(&entry)
            .to_string_lossy()
            .replace('\\', "/");
        if name == "manifest.json" || listed.contains_key(&name) {
            continue;
        }
        findings.push(Finding::new(FindingKind::UnlistedArtifact, name));
    }

    let index_path = directory.join("cases/index.json");
    let index: CaseIndex = match fs::read(&index_path) {
        Ok(bytes) => parse(&index_path, &bytes)?,
        Err(_) => CaseIndex {
            bundle_id: manifest.bundle_id.clone(),
            records: Vec::new(),
        },
    };
    if index.bundle_id != manifest.bundle_id {
        findings.push(Finding::new(
            FindingKind::IndexShardMismatch,
            "the case index names another bundle",
        ));
    }

    let published: BTreeSet<(Side, pdfdelta_core::model::PageId)> = {
        let path = directory.join("pages/index.json");
        match fs::read(&path) {
            Ok(bytes) => parse::<PageIndex>(&path, &bytes)?
                .pages
                .into_iter()
                .map(|record| (record.side, record.page_index))
                .collect(),
            Err(_) => BTreeSet::new(),
        }
    };

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for record in &index.records {
        if !seen.insert(record.case.clone()) {
            findings.push(Finding::new(
                FindingKind::DuplicateCaseId,
                record.case.clone(),
            ));
        }
        let path = directory.join(format!("cases/{}.json", record.case));
        let Ok(bytes) = fs::read(&path) else {
            findings.push(Finding::new(
                FindingKind::MissingArtifact,
                format!("cases/{}.json", record.case),
            ));
            continue;
        };
        let case: ReviewCase = parse(&path, &bytes)?;
        if case.case_id.as_str() != record.case {
            findings.push(Finding::new(
                FindingKind::IndexShardMismatch,
                format!("{} holds {}", record.case, case.case_id),
            ));
        }
        if format!("{:?}", case.question)
            .to_lowercase()
            .replace('_', "")
            != record.question.replace('_', "")
        {
            findings.push(Finding::new(
                FindingKind::IndexShardMismatch,
                format!("{} lists a different question", record.case),
            ));
        }
        if record.alternatives_returned != case.alternatives_returned
            || record.alternatives_total != case.alternatives_total
        {
            findings.push(Finding::new(
                FindingKind::IndexShardMismatch,
                format!("{} lists a different alternative count", record.case),
            ));
        }
        if let Some(total) = case.alternatives_total
            && total < case.alternatives_returned
        {
            findings.push(Finding::new(
                FindingKind::AlternativesAccounting,
                format!(
                    "{} returned {} of a declared {total}",
                    record.case, case.alternatives_returned
                ),
            ));
        }
        if case.completeness.candidate_enumeration
            == pdfdelta_core::review::Completeness::Incomplete
            && case.alternatives_total.is_some()
        {
            findings.push(Finding::new(
                FindingKind::CompletenessAccounting,
                format!(
                    "{} reports a total for an unfinished enumeration",
                    record.case
                ),
            ));
        }
        if case.reasons.is_empty() {
            findings.push(Finding::new(
                FindingKind::MissingReason,
                record.case.clone(),
            ));
        }
        for region in &case.regions {
            let offers_render = case.available_actions.iter().any(|action| {
                matches!(
                    action,
                    pdfdelta_core::review::RetrievalAction::Render { .. }
                )
            });
            if offers_render && !published.contains(&(region.side, region.page_index)) {
                findings.push(Finding::new(
                    FindingKind::UnpublishedRegionPage,
                    format!(
                        "{} offers a picture of {:?} page {}",
                        record.case, region.side, region.page_number
                    ),
                ));
            }
        }
    }

    for artifact in manifest
        .files
        .iter()
        .filter(|artifact| artifact.name.ends_with(".context.json"))
    {
        let path = directory.join(&artifact.name);
        let Ok(bytes) = fs::read(&path) else { continue };
        let context: CaseContext = parse(&path, &bytes)?;
        if !seen.contains(context.case_id.as_str()) {
            findings.push(Finding::new(
                FindingKind::OrphanContext,
                artifact.name.clone(),
            ));
        }
    }

    if let Some(destination) = baseline_text {
        fs::create_dir_all(destination).map_err(|error| {
            BenchError::InvalidInput(format!(
                "cannot create baseline text directory {}: {error}",
                destination.display()
            ))
        })?;
    }
    let baseline_full_text_bytes = ["old.pdf", "new.pdf"]
        .iter()
        .map(|name| {
            let text = fs::read(directory.join(name)).map(extracted_text).ok()?;
            if let Some(destination) = baseline_text {
                fs::write(destination.join(name.replace(".pdf", ".txt")), &text).ok()?;
            }
            Some(text.len())
        })
        .try_fold(0, |total, side| side.map(|bytes| total + bytes));
    let baseline_report_bytes = report
        .map(|path| read(path).map(|bytes| bytes.len()))
        .transpose()?;
    let packets_bytes = index_bytes + case_packet_bytes;
    case_sizes.sort_unstable();
    let percentile = |fraction: f64| -> usize {
        if case_sizes.is_empty() {
            return 0;
        }
        let position = ((case_sizes.len() as f64 - 1.0) * fraction).round() as usize;
        case_sizes[position.min(case_sizes.len() - 1)]
    };
    let case_bytes_median = percentile(0.5);
    let case_bytes_p95 = percentile(0.95);
    let case_bytes_max = case_sizes.last().copied().unwrap_or(0);
    let first_read_reduction_per_mille = baseline_full_text_bytes.and_then(|baseline| {
        (baseline > 0).then(|| {
            let kept = i64::try_from(index_bytes + case_bytes_median).unwrap_or(i64::MAX);
            let total = i64::try_from(baseline).unwrap_or(i64::MAX);
            1_000 - (kept.saturating_mul(1_000) / total)
        })
    });

    let host_usage = host_usage
        .map(|path| -> Result<HostUsage> {
            let bytes = read(path)?;
            Ok(HostUsage {
                reported: parse(path, &bytes)?,
            })
        })
        .transpose()?;

    findings.sort();
    findings.dedup();
    Ok(BundleAudit {
        schema: AUDIT_SCHEMA,
        bundle_id: manifest.bundle_id,
        pipeline: manifest.pipeline,
        cases: index.records.len(),
        unlocalized_gaps: manifest.unlocalized_gaps.len(),
        export_complete: manifest.export.export_complete,
        comparison_complete: manifest.engine.comparison_complete,
        findings,
        cost: Cost {
            measured_in: "bytes",
            tokenizer: None,
            bundle_bytes,
            index_bytes,
            case_packet_bytes,
            context_bytes,
            page_raster_bytes,
            source_pdf_bytes,
            packets_bytes,
            case_bytes_median,
            case_bytes_p95,
            case_bytes_max,
            baseline_full_text_bytes,
            baseline_report_bytes,
            first_read_reduction_per_mille,
        },
        host_usage,
    })
}

impl PartialOrd for Finding {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Finding {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.kind, &self.detail).cmp(&(other.kind, &other.detail))
    }
}

/// The published name's extension, which this program always writes in lower
/// case because it generates every published path itself.
fn extension(name: &str) -> Option<&str> {
    Path::new(name).extension().and_then(|kind| kind.to_str())
}

/// Every regular file under a directory, in a stable order.
fn walk(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_order_is_stable_for_reporting() {
        let mut findings = [
            Finding::new(FindingKind::MissingReason, "R2"),
            Finding::new(FindingKind::ArtifactDigestMismatch, "cases/R1.json"),
            Finding::new(FindingKind::MissingReason, "R1"),
        ];
        findings.sort();
        assert_eq!(findings[0].kind, FindingKind::ArtifactDigestMismatch);
        assert_eq!(findings[1].detail, "R1");
    }

    #[test]
    fn a_missing_bundle_is_an_error_rather_than_an_empty_audit() {
        let audit = audit_bundle(Path::new("/nonexistent-bundle"), None, None);
        assert!(audit.is_err());
    }
}
