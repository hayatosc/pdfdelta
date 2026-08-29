//! Real-world revision-pair benchmark track.
//!
//! Unlike the synthetic acceptance matrix, this track compares two genuinely
//! different public revisions of the same document. Documents are never
//! vendored: the manifest records stable URLs, capture dates, byte sizes, and
//! SHA-256 checksums, and operators download the files into a local cache
//! directory with `benchmark/realworld/fetch.sh`.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use pdfdelta_core::{
    alignment::{Alignment, BlockSeparator},
    diff::{ChangeKind, Comparison, NearRelationStopReason, SentenceRecoveryMetrics, TextSpan},
    model::Document,
    normalize::{BlockText, ComparableToken},
    pdf::{LopdfParser, ParseLimits},
    pipeline::{
        PipelineDiagnostics, PipelineOptions, PipelinePhase, PipelinePhaseStatus,
        compare_extraction_outcomes_with_alignment_diagnostics,
        validate_limit_scale as validate_pipeline_limit_scale,
    },
    report::{self, DocumentSide, summarize},
    source::{
        ContentStreamGlyphExtractor, ExtractionIssue, ExtractionIssueKind, ExtractionLimits,
        ExtractionOutcome, ExtractionScope, ParserBackedGlyphSource,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BenchError, Result,
    candidate_eval::{CandidateVisitPressure, evaluate_candidate_visit_pressure},
};

#[path = "revision_diagnostics.rs"]
mod revision_diagnostics;
#[path = "revision_scopes.rs"]
mod revision_scopes;

use revision_diagnostics::evaluate_reviewed_diagnostics;
use revision_scopes::{
    classify_scoped_changes, evaluate_scoped_token_metrics, resolve_revision_scopes,
    validate_scoped_expected_changes,
};

pub const QUALITY_SKIP_RESOURCE_LIMIT: &str = "comparison stopped at a resource limit";
pub const QUALITY_SKIP_INCOMPLETE_EXTRACTION: &str =
    "extraction was incomplete so reported diffs are suppressed";
pub const QUALITY_SKIP_NO_ANNOTATIONS: &str = "no expected annotations are recorded for this pair";

/// Column order of `benchmark/realworld/manifest.tsv`.
pub const MANIFEST_HEADER: [&str; 17] = [
    "pair_id",
    "set",
    "role",
    "document_type",
    "layout",
    "in_scope",
    "known_issues",
    "captured",
    "expected_extraction",
    "limit_scale_hint",
    "expected_file",
    "old_url",
    "old_byte_count",
    "old_sha256",
    "new_url",
    "new_byte_count",
    "new_sha256",
];

const SHA256_HEX_LEN: usize = 64;

/// One reported semantic change and every location where it occurs.
#[derive(Clone, Debug)]
pub struct ActualChange {
    pub kind: ChangeKind,
    pub occurrences: Vec<ActualChangeOccurrence>,
}

#[derive(Clone, Debug)]
pub struct ActualChangeOccurrence {
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub old_comparable_len: Option<usize>,
    pub new_comparable_len: Option<usize>,
    pub resolvable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualityMetrics {
    pub annotation: Annotation,
    pub expected_changes: usize,
    pub reported_changes: usize,
    pub recall: Option<f64>,
    pub precision: Option<f64>,
    pub kind_accuracy: Option<f64>,
    /// Reported hunks divided by quote-matched expected changes. Under
    /// `Partial` annotations the denominator is only the reviewed subset,
    /// so this overstates fragmentation and is not comparable with the
    /// complete-annotation `review_hunks_per_expected_change`.
    pub reported_hunks_per_matched_change: Option<f64>,
    /// Reported hunks per expected semantic change; computed only for
    /// `Complete` annotations where every semantic change was reviewed.
    pub review_hunks_per_expected_change: Option<f64>,
    pub unmatched_tiny_changes: usize,
    /// Occurrences with at least one existing side that could not be resolved.
    pub unresolvable_reported_spans: usize,
}

/// Event-level quality over fully reviewed scopes only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ScopedEventMetrics {
    pub reviewed_scope_count: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

/// Comparable-token overlap quality over fully reviewed scopes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ScopedTokenMetrics {
    pub expected_changed_tokens: usize,
    pub reported_changed_tokens: usize,
    pub true_positive_tokens: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub span_iou: f64,
    pub false_positive_tokens_per_10k_unchanged: Option<f64>,
}

/// Recall of human-reviewed replacement and move counterparts in the
/// production inverted-index candidate generator's top-K output.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CandidateRecallMetrics {
    pub top_k: usize,
    pub annotated_counterparts: usize,
    pub evaluable_counterparts: usize,
    pub recalled_counterparts: usize,
    pub unavailable_counterparts: usize,
    pub recall_at_k: Option<f64>,
}

/// Document side on which an expected quote could not be diagnosed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissSide {
    Old,
    New,
    Both,
}

/// Most specific evidence-backed reason one reviewed expected change failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ExpectedChangeFailureReason {
    QuoteNotExtracted { side: MissSide },
    UnitSegmentationFailure { side: MissSide },
    CandidateNotGenerated,
    CandidateScoringRejected,
    AlignmentAmbiguous,
    AlignmentSpanMismatch,
    ReadingOrderUnresolved { side: MissSide },
    DiffEditDistanceExceeded,
    DiffRejectedAsImplausible,
    WrongChangeKind { expected: String, actual: String },
    FragmentedAcrossHunks { old_hunks: usize, new_hunks: usize },
    AlignmentOrCandidate { diagnostic_limited: bool },
}

/// Failure diagnosis for one reviewed expected change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExpectedChangeFailure {
    pub expected_id: String,
    #[serde(flatten)]
    pub reason: ExpectedChangeFailureReason,
}

/// Bounded failure diagnostics for reviewed expected changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExpectedChangeDiagnostics {
    pub complete: bool,
    pub failures: Vec<ExpectedChangeFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IssueLine {
    pub side: &'static str,
    pub kind: &'static str,
    pub scope: String,
    pub description: String,
}

/// Truncated span texts of one reported change, giving human reviewers the
/// evidence needed to maintain expected-annotation files without a separate
/// inspection pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReportedChangeText {
    pub kind: &'static str,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
}

/// Outcome of one pair run. `Limit` records a comparison that stopped at a
/// documented resource budget before producing a diff; it is distinct from
/// `Ok` and `Failed` so incomplete measurements never masquerade as healthy
/// results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairRunStatus {
    Ok,
    Limit,
    Failed,
}

impl PairRunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Limit => "LIMIT",
            Self::Failed => "FAIL",
        }
    }
}

const CHANGE_TEXT_PREVIEW_CHARS: usize = 240;

fn truncate_preview(text: &str) -> String {
    text.chars().take(CHANGE_TEXT_PREVIEW_CHARS).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SentenceRecoveryMetricsReport {
    pub old_trusted_run_source_tokens: usize,
    pub new_trusted_run_source_tokens: usize,
    pub structural_pairing_available: bool,
    pub old_structural_descriptors: usize,
    pub new_structural_descriptors: usize,
    pub old_structural_eligible_descriptors: usize,
    pub new_structural_eligible_descriptors: usize,
    pub old_structural_mixed_descriptors: usize,
    pub new_structural_mixed_descriptors: usize,
    pub old_structural_split_descriptors: usize,
    pub new_structural_split_descriptors: usize,
    pub structural_shared_profiles: usize,
    pub structural_candidate_pairs: usize,
    pub structural_largest_posting: usize,
    pub structural_duplicate_pairs: usize,
    pub structural_unique_reciprocal_pairs: usize,
    pub structural_unique_no_anchor_pairs: usize,
    pub structural_unique_monotone_anchor_pairs: usize,
    pub structural_unique_crossing_veto_pairs: usize,
    pub exact_shared_units: usize,
    pub old_exact_one_sided_units: usize,
    pub new_exact_one_sided_units: usize,
    pub near_relation_complete: bool,
    pub near_pair_visits_examined: usize,
    pub near_pair_visits_attempted: usize,
    pub near_similarity_comparisons_examined: usize,
    pub near_similarity_comparisons_attempted: usize,
    pub near_largest_edge_posting: usize,
    pub near_largest_edge_query_union: usize,
    pub near_largest_filtered_candidate_set: usize,
    pub near_candidate_count_truncated: bool,
    pub near_relation_stop_reason: Option<NearRelationStopReasonReport>,
    pub near_pair_candidates: usize,
    pub vetoed_near_pairs: usize,
    pub recovered_exact_match_old_tokens: usize,
    pub recovered_exact_match_new_tokens: usize,
    pub recovered_replacement_old_tokens: usize,
    pub recovered_replacement_new_tokens: usize,
    pub recovered_deletion_tokens: usize,
    pub recovered_insertion_tokens: usize,
    pub unresolved_remainder_old_source_tokens: usize,
    pub unresolved_remainder_new_source_tokens: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NearRelationStopReasonReport {
    PairVisitLimit,
    SimilarityComparisonLimit,
    CandidateCountLimit,
}

impl From<NearRelationStopReason> for NearRelationStopReasonReport {
    fn from(reason: NearRelationStopReason) -> Self {
        match reason {
            NearRelationStopReason::PairVisitLimit => Self::PairVisitLimit,
            NearRelationStopReason::SimilarityComparisonLimit => Self::SimilarityComparisonLimit,
            NearRelationStopReason::CandidateCountLimit => Self::CandidateCountLimit,
        }
    }
}

impl From<SentenceRecoveryMetrics> for SentenceRecoveryMetricsReport {
    fn from(metrics: SentenceRecoveryMetrics) -> Self {
        Self {
            old_trusted_run_source_tokens: metrics.old_trusted_run_source_tokens,
            new_trusted_run_source_tokens: metrics.new_trusted_run_source_tokens,
            structural_pairing_available: metrics.structural_pairing_available,
            old_structural_descriptors: metrics.old_structural_descriptors,
            new_structural_descriptors: metrics.new_structural_descriptors,
            old_structural_eligible_descriptors: metrics.old_structural_eligible_descriptors,
            new_structural_eligible_descriptors: metrics.new_structural_eligible_descriptors,
            old_structural_mixed_descriptors: metrics.old_structural_mixed_descriptors,
            new_structural_mixed_descriptors: metrics.new_structural_mixed_descriptors,
            old_structural_split_descriptors: metrics.old_structural_split_descriptors,
            new_structural_split_descriptors: metrics.new_structural_split_descriptors,
            structural_shared_profiles: metrics.structural_shared_profiles,
            structural_candidate_pairs: metrics.structural_candidate_pairs,
            structural_largest_posting: metrics.structural_largest_posting,
            structural_duplicate_pairs: metrics.structural_duplicate_pairs,
            structural_unique_reciprocal_pairs: metrics.structural_unique_reciprocal_pairs,
            structural_unique_no_anchor_pairs: metrics.structural_unique_no_anchor_pairs,
            structural_unique_monotone_anchor_pairs: metrics
                .structural_unique_monotone_anchor_pairs,
            structural_unique_crossing_veto_pairs: metrics.structural_unique_crossing_veto_pairs,
            exact_shared_units: metrics.exact_shared_units,
            old_exact_one_sided_units: metrics.old_exact_one_sided_units,
            new_exact_one_sided_units: metrics.new_exact_one_sided_units,
            near_relation_complete: metrics.near_relation_complete,
            near_pair_visits_examined: metrics.near_pair_visits_examined,
            near_pair_visits_attempted: metrics.near_pair_visits_attempted,
            near_similarity_comparisons_examined: metrics.near_similarity_comparisons_examined,
            near_similarity_comparisons_attempted: metrics.near_similarity_comparisons_attempted,
            near_largest_edge_posting: metrics.near_largest_edge_posting,
            near_largest_edge_query_union: metrics.near_largest_edge_query_union,
            near_largest_filtered_candidate_set: metrics.near_largest_filtered_candidate_set,
            near_candidate_count_truncated: metrics.near_candidate_count_truncated,
            near_relation_stop_reason: metrics.near_relation_stop_reason.map(Into::into),
            near_pair_candidates: metrics.near_pair_candidates,
            vetoed_near_pairs: metrics.vetoed_near_pairs,
            recovered_exact_match_old_tokens: metrics.recovered_exact_match_old_tokens,
            recovered_exact_match_new_tokens: metrics.recovered_exact_match_new_tokens,
            recovered_replacement_old_tokens: metrics.recovered_replacement_old_tokens,
            recovered_replacement_new_tokens: metrics.recovered_replacement_new_tokens,
            recovered_deletion_tokens: metrics.recovered_deletion_tokens,
            recovered_insertion_tokens: metrics.recovered_insertion_tokens,
            unresolved_remainder_old_source_tokens: metrics.unresolved_remainder_old_source_tokens,
            unresolved_remainder_new_source_tokens: metrics.unresolved_remainder_new_source_tokens,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PairRunReport {
    pub pair_id: String,
    pub set: &'static str,
    pub role: &'static str,
    pub document_type: String,
    pub in_scope: bool,
    pub status: PairRunStatus,
    pub provenance_verified: bool,
    pub compared: bool,
    /// Both sides produced glyph evidence without document- or page-scoped
    /// extraction issues; independent of alignment quality.
    pub extraction_complete: Option<bool>,
    /// No unresolved regions and full comparable-token coverage on both
    /// sides; false does not imply an extraction failure.
    pub comparison_complete: Option<bool>,
    pub extraction_issues: Vec<IssueLine>,
    pub coverage_old: Option<f64>,
    pub coverage_new: Option<f64>,
    pub coverage_comparison: Option<f64>,
    pub unresolved_regions: Option<usize>,
    pub unresolved_old_token_share: Option<f64>,
    pub unresolved_new_token_share: Option<f64>,
    pub reported_content_changes: Option<usize>,
    pub formatting_only_changes: Option<usize>,
    /// Count of low-confidence changes identified during comparison.
    /// Skipped in serde serialization (`#[serde(skip)]`) to preserve the
    /// byte-for-byte schema and key set of full `--json-output` v1 reports,
    /// while allowing compact summary JSON to expose this computed evidence
    /// via `reported_uncertain_changes`.
    #[serde(skip)]
    pub uncertain_changes: Option<usize>,
    pub reported_changes_preview: Vec<ReportedChangeText>,
    pub quality: Option<QualityMetrics>,
    pub quality_skipped_reason: Option<String>,
    /// Scoped event quality is published only in compact summaries;
    /// the unversioned full-report v1 key set remains unchanged.
    #[serde(skip)]
    pub scoped_event_metrics: Option<ScopedEventMetrics>,
    /// Scoped token quality is published in compact summary schema v7 and later.
    #[serde(skip)]
    pub scoped_token_metrics: Option<ScopedTokenMetrics>,
    /// Reviewed candidate recall is published only in compact summary schema
    /// v3 so the full report v1 key set remains unchanged.
    #[serde(skip)]
    pub candidate_recall: Option<CandidateRecallMetrics>,
    /// Expected-change failure reasons are published only in compact summary
    /// schema v3 so the full report v1 key set remains unchanged.
    #[serde(skip)]
    pub expected_change_diagnostics: Option<ExpectedChangeDiagnostics>,
    pub resource_limit_failure: Option<String>,
    /// Sum of `CandidateGenerator::estimated_visits` charged against
    /// `max_candidate_visits` for non-anchor old blocks; on a candidate
    /// limit stop this is the attempted cumulative charge including the
    /// exceeding block. `None` when alignment was never reached (e.g. an
    /// earlier n-gram or diff budget limit).
    pub candidate_visits: Option<usize>,
    /// Checked sum of `CandidateGenerator::estimated_visits` over every
    /// non-anchor old block, independent of the budget: the full candidate
    /// work the alignment would need. `Some` when the full sum completed
    /// (including on a candidate limit stop); `None` when an estimate error
    /// or overflow made the sum unavailable, or the candidate preflight was
    /// never reached (e.g. an earlier n-gram or diff budget limit).
    pub candidate_visits_required: Option<usize>,
    /// Exact-match posting visits of the required candidate sum; `Some`
    /// only when every non-anchor old block reported a breakdown and every
    /// component sum completed.
    pub candidate_visits_required_exact: Option<usize>,
    /// N-gram posting visits of the required candidate sum; `Some` under
    /// the same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_ngram: Option<usize>,
    /// Short-block fallback visits of the required candidate sum; `Some`
    /// under the same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_short_fallback: Option<usize>,
    /// The `AlignmentOptions::max_candidate_visits` budget the charge was
    /// compared against.
    pub max_candidate_visits: Option<usize>,
    /// Kept out of the unversioned full-report v1 key set. The versioned
    /// compact summary exposes these diagnostics starting with schema v2.
    #[serde(skip)]
    pub sentence_recovery_metrics: Option<SentenceRecoveryMetricsReport>,
    /// All-old-block candidate visit pressure measured from the extracted
    /// documents; `None` when either side's extraction is incomplete or the
    /// comparison stopped at a pre-alignment resource limit/error.
    pub candidate_visit_pressure: Option<CandidateVisitPressure>,
    pub runtime_ms: u128,
    pub limit_scale_used: f64,
    pub failure: Option<String>,
}

impl PairRunReport {
    /// Only `Ok` records are healthy: a resource-limit stop is not a failure,
    /// but it also did not measure anything and must not be counted as done.
    pub fn healthy(&self) -> bool {
        self.status == PairRunStatus::Ok
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairSet {
    Dev,
    Holdout,
}

impl PairSet {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "dev" => Some(Self::Dev),
            "holdout" => Some(Self::Holdout),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairRole {
    Standard,
    Stress,
}

impl PairRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Stress => "stress",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            "stress" => Some(Self::Stress),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedExtraction {
    Complete,
    Incomplete,
}

impl ExpectedExtraction {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "complete" => Some(Self::Complete),
            "incomplete" => Some(Self::Incomplete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideProvenance {
    pub url: String,
    pub byte_count: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RevisionPair {
    pub pair_id: String,
    pub set: PairSet,
    pub role: PairRole,
    pub document_type: String,
    pub layout: String,
    pub in_scope: bool,
    pub known_issues: Option<String>,
    pub captured: String,
    pub expected_extraction: ExpectedExtraction,
    pub limit_scale_hint: f64,
    pub expected_file: Option<String>,
    pub old: SideProvenance,
    pub new: SideProvenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Annotation {
    /// Every semantic change visible in review was annotated, so precision is
    /// meaningful.
    Complete,
    /// Only representative changes were annotated; recall and kind accuracy
    /// remain meaningful while precision does not.
    Partial,
    /// Every semantic change within each declared scope was annotated.
    #[serde(rename = "scoped_complete")]
    ScopedComplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedKind {
    Replacement,
    Insertion,
    Deletion,
    Move,
}

impl ExpectedKind {
    fn agrees_with(self, kind: ChangeKind) -> bool {
        matches!(
            (self, kind),
            (Self::Replacement, ChangeKind::Replacement)
                | (Self::Insertion, ChangeKind::Insertion)
                | (Self::Deletion, ChangeKind::Deletion)
                | (Self::Move, ChangeKind::Move)
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Replacement => "replacement",
            Self::Insertion => "insertion",
            Self::Deletion => "deletion",
            Self::Move => "move",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedChange {
    pub id: String,
    pub kind: ExpectedKind,
    #[serde(default)]
    pub scope: Option<String>,
    /// Under `ScopedComplete`, this is the exact expected changed span on the
    /// old side, not surrounding context used only for identification.
    #[serde(default)]
    pub old_quote: Option<String>,
    /// Under `ScopedComplete`, this is the exact expected changed span on the
    /// new side, not surrounding context used only for identification.
    #[serde(default)]
    pub new_quote: Option<String>,
    #[serde(default)]
    pub note: String,
}

/// Inclusive quote anchors delimiting one reviewed region on a document side.
///
/// Both the start and end quotes are part of the scoped region. Loading an
/// annotation validates only this syntax; anchor resolution is performed by a
/// later benchmark stage.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteScope {
    pub start_quote: String,
    pub end_quote: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedScope {
    pub id: String,
    pub old: QuoteScope,
    pub new: QuoteScope,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedDocument {
    pub version: u32,
    pub pair: String,
    pub reviewed_on: String,
    pub annotation: Annotation,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub scopes: Vec<ExpectedScope>,
    #[serde(default)]
    pub changes: Vec<ExpectedChange>,
}

struct MatchOutcome {
    matched: usize,
    kind_agreements: usize,
    claimed_actuals: HashSet<usize>,
    claimed_actual_by_expected: Vec<Option<usize>>,
}

pub fn parse_manifest(manifest: &str) -> Result<Vec<RevisionPair>> {
    let mut pairs = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut header_seen = false;
    for (index, raw_line) in manifest.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<&str> = line.split('\t').collect();
        if !header_seen {
            validate_header(&columns, line_number)?;
            header_seen = true;
            continue;
        }
        if columns.len() != MANIFEST_HEADER.len() {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has {} columns, expected {}",
                columns.len(),
                MANIFEST_HEADER.len()
            )));
        }
        let pair_id = require_nonblank(columns[0], "pair_id", line_number)?;
        if !seen_ids.insert(pair_id.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} duplicates pair_id {pair_id}"
            )));
        }
        let set = PairSet::parse(columns[1])
            .ok_or_else(|| invalid_row(line_number, "set", columns[1]))?;
        let role = PairRole::parse(columns[2])
            .ok_or_else(|| invalid_row(line_number, "role", columns[2]))?;
        let in_scope = parse_bool(columns[5], "in_scope", line_number)?;
        let expected_extraction = ExpectedExtraction::parse(columns[8])
            .ok_or_else(|| invalid_row(line_number, "expected_extraction", columns[8]))?;
        let limit_scale_hint = parse_limit_scale_hint(columns[9], line_number)?;
        let old = parse_side_provenance(columns[11], columns[12], columns[13], "old", line_number)?;
        let new = parse_side_provenance(columns[14], columns[15], columns[16], "new", line_number)?;
        if old.sha256 == new.sha256 {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} records byte-identical old and new PDFs"
            )));
        }
        pairs.push(RevisionPair {
            pair_id,
            set,
            role,
            document_type: require_nonblank(columns[3], "document_type", line_number)?,
            layout: require_nonblank(columns[4], "layout", line_number)?,
            in_scope,
            known_issues: optional_text(columns[6]),
            captured: require_nonblank(columns[7], "captured", line_number)?,
            expected_extraction,
            limit_scale_hint,
            expected_file: optional_text(columns[10]),
            old,
            new,
        });
    }
    if !header_seen {
        return Err(BenchError::InvalidInput(
            "revision manifest is missing its header row".to_owned(),
        ));
    }
    if pairs.is_empty() {
        return Err(BenchError::InvalidInput(
            "revision manifest records no pairs".to_owned(),
        ));
    }
    Ok(pairs)
}

fn validate_header(columns: &[&str], line_number: usize) -> Result<()> {
    if columns != MANIFEST_HEADER.as_slice() {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest header on line {line_number} does not match the documented column order"
        )));
    }
    Ok(())
}

fn invalid_row(line_number: usize, column: &str, value: &str) -> BenchError {
    BenchError::InvalidInput(format!(
        "revision manifest row {line_number} has invalid {column} {value:?}"
    ))
}

fn require_nonblank(value: &str, column: &str, line_number: usize) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has blank {column}"
        )));
    }
    Ok(trimmed.to_owned())
}

fn optional_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "-" {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn parse_bool(value: &str, column: &str, line_number: usize) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {column} {other:?}: expected true or false"
        ))),
    }
}

fn parse_limit_scale_hint(value: &str, line_number: usize) -> Result<f64> {
    let parsed: f64 = value.trim().parse().map_err(|_| {
        BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid limit_scale_hint {value:?}"
        ))
    })?;
    if !parsed.is_finite() || parsed < 1.0 {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has limit_scale_hint {parsed} below 1.0"
        )));
    }
    Ok(parsed)
}

fn parse_side_provenance(
    url: &str,
    byte_count: &str,
    sha256: &str,
    side: &'static str,
    line_number: usize,
) -> Result<SideProvenance> {
    let byte_count: u64 = byte_count.trim().parse().map_err(|_| {
        BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {side}_byte_count {byte_count:?}"
        ))
    })?;
    Ok(SideProvenance {
        url: require_nonblank(url, &format!("{side}_url"), line_number)?,
        byte_count,
        sha256: validate_sha256(sha256, side, line_number)?,
    })
}

fn validate_sha256(value: &str, side: &str, line_number: usize) -> Result<String> {
    let trimmed = value.trim();
    let valid = trimmed.len() == SHA256_HEX_LEN
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {side}_sha256 {trimmed:?}: expected 64 lowercase hex characters"
        )));
    }
    Ok(trimmed.to_owned())
}

pub fn load_expected_document(expected_json: &str) -> Result<ExpectedDocument> {
    let document: ExpectedDocument = serde_json::from_str(expected_json).map_err(|error| {
        BenchError::InvalidInput(format!("invalid expected-revision JSON: {error}"))
    })?;
    if document.version != 1 {
        return Err(BenchError::InvalidInput(format!(
            "expected-revision JSON has unsupported version {}; version 1 is required",
            document.version
        )));
    }
    if document.pair.trim().is_empty() || document.reviewed_on.trim().is_empty() {
        return Err(BenchError::InvalidInput(
            "expected-revision JSON requires nonblank pair and reviewed_on values".to_owned(),
        ));
    }
    let mut scope_ids = HashSet::new();
    for scope in &document.scopes {
        if scope.id.trim().is_empty() || !scope_ids.insert(scope.id.as_str()) {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON scope id {:?} is blank or duplicated",
                scope.id
            )));
        }
        for (side, anchors) in [("old", &scope.old), ("new", &scope.new)] {
            if collapse_whitespace(&anchors.start_quote).is_empty()
                || collapse_whitespace(&anchors.end_quote).is_empty()
            {
                return Err(BenchError::InvalidInput(format!(
                    "expected-revision JSON scope {} has blank {side} anchor quotes",
                    scope.id
                )));
            }
        }
    }
    match document.annotation {
        Annotation::ScopedComplete if document.scopes.is_empty() => {
            return Err(BenchError::InvalidInput(
                "expected-revision JSON scoped_complete annotation requires at least one scope"
                    .to_owned(),
            ));
        }
        Annotation::Complete | Annotation::Partial if !document.scopes.is_empty() => {
            return Err(BenchError::InvalidInput(
                "expected-revision JSON complete and partial annotations forbid scopes".to_owned(),
            ));
        }
        _ => {}
    }
    let mut seen_ids = HashSet::new();
    for change in &document.changes {
        if change.id.trim().is_empty() || !seen_ids.insert(change.id.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON change id {:?} is blank or duplicated",
                change.id
            )));
        }
        match document.annotation {
            Annotation::ScopedComplete => match change.scope.as_deref() {
                Some(scope) if scope_ids.contains(scope) => {}
                Some(scope) => {
                    return Err(BenchError::InvalidInput(format!(
                        "expected-revision JSON change {} references unknown scope {scope:?}",
                        change.id
                    )));
                }
                None => {
                    return Err(BenchError::InvalidInput(format!(
                        "expected-revision JSON change {} requires a scope for scoped_complete annotation",
                        change.id
                    )));
                }
            },
            Annotation::Complete | Annotation::Partial if change.scope.is_some() => {
                return Err(BenchError::InvalidInput(format!(
                    "expected-revision JSON change {} cannot reference a scope for complete or partial annotation",
                    change.id
                )));
            }
            Annotation::Complete | Annotation::Partial => {}
        }
        let old_present = change
            .old_quote
            .as_deref()
            .is_some_and(|quote| !collapse_whitespace(quote).is_empty());
        let new_present = change
            .new_quote
            .as_deref()
            .is_some_and(|quote| !collapse_whitespace(quote).is_empty());
        let shape_valid = match change.kind {
            ExpectedKind::Replacement | ExpectedKind::Move => old_present && new_present,
            ExpectedKind::Insertion => new_present && !old_present,
            ExpectedKind::Deletion => old_present && !new_present,
        };
        if !shape_valid {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON change {} of kind {} violates the quote rules: \
                 replacement and move require both quotes, insertion only new_quote, deletion only old_quote",
                change.id,
                change.kind.name()
            )));
        }
    }
    Ok(document)
}

pub fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn validate_limit_scale(scale: f64) -> Result<f64> {
    validate_pipeline_limit_scale(scale)
        .map_err(|error| BenchError::InvalidInput(error.to_string()))
}

fn side_cache_path(cache_dir: &Path, pair_id: &str, side: &str) -> PathBuf {
    cache_dir.join(format!("{pair_id}-{side}.pdf"))
}

fn verify_provenance(
    provenance: &SideProvenance,
    path: &Path,
    side: &str,
) -> std::result::Result<(), String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "missing or unreadable {side} download {}: {error}",
            path.display()
        )
    })?;
    if bytes.len() as u64 != provenance.byte_count {
        return Err(format!(
            "{side} download {} has {} bytes but the manifest captured {} bytes",
            path.display(),
            bytes.len(),
            provenance.byte_count
        ));
    }
    let digest = Sha256::digest(&bytes);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != provenance.sha256 {
        return Err(format!(
            "{side} download {} has sha256 {actual} but the manifest captured {}",
            path.display(),
            provenance.sha256
        ));
    }
    Ok(())
}

fn build_block_map(blocks: &[BlockText]) -> HashMap<u64, &BlockText> {
    blocks.iter().map(|block| (block.block.0, block)).collect()
}

fn resolve_span(map: &HashMap<u64, &BlockText>, span: &TextSpan) -> Option<(String, usize)> {
    if span.blocks.is_empty() {
        return None;
    }
    let mut tokens: Vec<ComparableToken> = Vec::new();
    for (position, block_id) in span.blocks.iter().enumerate() {
        let next = map.get(&block_id.0)?.canonical.comparable_tokens().ok()?;
        if position == 0 {
            tokens.extend(next);
            continue;
        }
        append_with_separator(&mut tokens, span.separator? == BlockSeparator::Space, &next);
    }
    if span.comparable_range.end > tokens.len()
        || span.comparable_range.start > span.comparable_range.end
    {
        return None;
    }
    let comparable_len = span.comparable_range.end - span.comparable_range.start;
    let mut text = String::new();
    for token in &tokens {
        if let ComparableToken::Scalar(scalar) = token {
            text.push(*scalar);
        }
    }
    if span.canonical_range.start > span.canonical_range.end
        || span.canonical_range.end > text.chars().count()
    {
        return None;
    }
    let selected: String = text
        .chars()
        .skip(span.canonical_range.start)
        .take(span.canonical_range.end - span.canonical_range.start)
        .collect();
    Some((selected, comparable_len))
}

/// Mirrors the core block-separator concatenation rule without exposing the
/// `pub(crate)` implementation detail.
fn append_with_separator(
    tokens: &mut Vec<ComparableToken>,
    space_separator: bool,
    next: &[ComparableToken],
) {
    let insert_space = space_separator
        && !tokens.last().is_some_and(is_space_token)
        && !next.first().is_some_and(is_space_token);
    if insert_space {
        tokens.push(ComparableToken::Scalar(' '));
    }
    tokens.extend_from_slice(next);
}

fn is_space_token(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn flatten_actual_changes(
    comparison: &Comparison,
    blocks_by_side: [&HashMap<u64, &BlockText>; 2],
) -> Vec<ActualChange> {
    comparison
        .changes
        .iter()
        .map(|change| {
            let occurrences = change
                .occurrences
                .iter()
                .map(|occurrence| {
                    let old_resolved = occurrence
                        .old_span
                        .as_ref()
                        .map(|span| resolve_span(blocks_by_side[0], span));
                    let new_resolved = occurrence
                        .new_span
                        .as_ref()
                        .map(|span| resolve_span(blocks_by_side[1], span));
                    let resolvable = old_resolved.as_ref().is_none_or(Option::is_some)
                        && new_resolved.as_ref().is_none_or(Option::is_some);
                    let unwrap_resolved = |resolved: Option<Option<(String, usize)>>| match resolved
                    {
                        Some(Some((text, len))) => (Some(collapse_whitespace(&text)), Some(len)),
                        _ => (None, None),
                    };
                    let (old_text, old_comparable_len) = unwrap_resolved(old_resolved);
                    let (new_text, new_comparable_len) = unwrap_resolved(new_resolved);
                    ActualChangeOccurrence {
                        old_text,
                        new_text,
                        old_comparable_len,
                        new_comparable_len,
                        resolvable,
                    }
                })
                .collect();
            ActualChange {
                kind: change.kind,
                occurrences,
            }
        })
        .collect()
}

fn contains_needle(haystack: Option<&str>, needle: Option<&str>) -> bool {
    match (haystack, needle) {
        (_, None) => true,
        (Some(haystack), Some(needle)) => haystack.contains(needle),
        (None, Some(_)) => false,
    }
}

fn match_changes(expected: &[ExpectedChange], actuals: &[ActualChange]) -> MatchOutcome {
    match_changes_with_scopes(expected, actuals, None)
}

fn match_changes_with_scopes(
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: Option<&[String]>,
) -> MatchOutcome {
    let mut claimed_actuals = HashSet::new();
    let mut claimed_actual_by_expected = vec![None; expected.len()];
    let mut kind_agreements = 0_usize;
    for (expected_index, change) in expected.iter().enumerate() {
        let needle_old = change.old_quote.as_deref().map(collapse_whitespace);
        let needle_new = change.new_quote.as_deref().map(collapse_whitespace);
        for (index, actual) in actuals.iter().enumerate() {
            if claimed_actuals.contains(&index) {
                continue;
            }
            if actual_scopes.is_some_and(|scopes| {
                scopes.get(index).map(String::as_str) != change.scope.as_deref()
            }) {
                continue;
            }
            let matches_occurrence = actual.occurrences.iter().any(|occurrence| {
                let old = occurrence.old_text.as_deref().map(collapse_whitespace);
                let new = occurrence.new_text.as_deref().map(collapse_whitespace);
                occurrence.resolvable
                    && contains_needle(old.as_deref(), needle_old.as_deref())
                    && contains_needle(new.as_deref(), needle_new.as_deref())
            });
            if matches_occurrence {
                claimed_actuals.insert(index);
                claimed_actual_by_expected[expected_index] = Some(index);
                if change.kind.agrees_with(actuals[index].kind) {
                    kind_agreements += 1;
                }
                break;
            }
        }
    }
    MatchOutcome {
        matched: claimed_actuals.len(),
        kind_agreements,
        claimed_actuals,
        claimed_actual_by_expected,
    }
}

fn scoped_quality(
    reviewed_scope_count: usize,
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: &[String],
) -> (QualityMetrics, ScopedEventMetrics) {
    let outcome = match_changes_with_scopes(expected, actuals, Some(actual_scopes));
    let mut quality =
        quality_from_match_outcome(Annotation::ScopedComplete, expected, actuals, &outcome);
    let precision = if actuals.is_empty() {
        1.0
    } else {
        outcome.matched as f64 / actuals.len() as f64
    };
    let recall = if expected.is_empty() {
        1.0
    } else {
        outcome.matched as f64 / expected.len() as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    quality.precision = Some(precision);
    quality.recall = Some(recall);
    (
        quality,
        ScopedEventMetrics {
            reviewed_scope_count,
            precision,
            recall,
            f1,
        },
    )
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

pub fn compute_quality(
    annotation: Annotation,
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
) -> QualityMetrics {
    let outcome = match_changes(expected, actuals);
    quality_from_match_outcome(annotation, expected, actuals, &outcome)
}

fn quality_from_match_outcome(
    annotation: Annotation,
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    outcome: &MatchOutcome,
) -> QualityMetrics {
    let reported = actuals.len();
    let reported_hunks = reported_hunk_count(actuals);
    let unmatched_tiny = actuals
        .iter()
        .enumerate()
        .filter(|(index, change)| !outcome.claimed_actuals.contains(index) && is_tiny(change))
        .count();
    QualityMetrics {
        annotation,
        expected_changes: expected.len(),
        reported_changes: reported,
        recall: ratio(outcome.matched, expected.len()),
        precision: (annotation != Annotation::Partial)
            .then(|| ratio(outcome.matched, reported))
            .flatten(),
        kind_accuracy: (outcome.matched > 0)
            .then(|| ratio(outcome.kind_agreements, outcome.matched))
            .flatten(),
        reported_hunks_per_matched_change: (outcome.matched > 0)
            .then(|| reported_hunks as f64 / outcome.matched as f64),
        review_hunks_per_expected_change: (annotation != Annotation::Partial)
            .then(|| ratio(reported_hunks, expected.len()))
            .flatten(),
        unmatched_tiny_changes: unmatched_tiny,
        unresolvable_reported_spans: actuals
            .iter()
            .flat_map(|change| &change.occurrences)
            .filter(|occurrence| !occurrence.resolvable)
            .count(),
    }
}

fn reported_hunk_count(actuals: &[ActualChange]) -> usize {
    actuals.iter().fold(0, |total, change| {
        total.saturating_add(change.occurrences.len())
    })
}

/// One- or two-token edits on every existing side, the shape of changes bad
/// alignment tends to fabricate. Changes without any resolved span length
/// (unresolvable spans) are never tiny: they are counted separately as
/// unresolvable and must not inflate the suspicious-edit signal.
fn is_tiny(change: &ActualChange) -> bool {
    !change.occurrences.is_empty()
        && change.occurrences.iter().all(|occurrence| {
            if !occurrence.resolvable {
                return false;
            }
            [occurrence.old_comparable_len, occurrence.new_comparable_len]
                .into_iter()
                .flatten()
                .max()
                .is_some_and(|longest| longest <= 2)
        })
}

fn unresolved_token_shares(comparison: &Comparison) -> (Option<f64>, Option<f64>) {
    let region_tokens = |side: DocumentSide| -> usize {
        comparison
            .unresolved_regions
            .iter()
            .filter_map(|region| match side {
                DocumentSide::Old => region.old_span.as_ref(),
                DocumentSide::New => region.new_span.as_ref(),
            })
            .map(|span| {
                span.comparable_range
                    .end
                    .saturating_sub(span.comparable_range.start)
            })
            .sum()
    };
    let share = |total: usize, sum: usize| (total > 0).then(|| sum as f64 / total as f64);
    (
        share(
            comparison.old_coverage.total_tokens,
            region_tokens(DocumentSide::Old),
        ),
        share(
            comparison.new_coverage.total_tokens,
            region_tokens(DocumentSide::New),
        ),
    )
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}

fn issue_lines(extraction: &report::ExtractionStatus) -> Vec<IssueLine> {
    extraction
        .issues
        .iter()
        .map(|issue| IssueLine {
            side: match issue.side {
                DocumentSide::Old => "old",
                DocumentSide::New => "new",
            },
            kind: match issue.kind {
                ExtractionIssueKind::Unsupported => "unsupported",
                ExtractionIssueKind::Unresolved => "unresolved",
            },
            scope: match issue.scope {
                ExtractionScope::Document => "document".to_owned(),
                ExtractionScope::Page(page) => format!("page {}", page.0),
                ExtractionScope::PageGap { retained_before } => {
                    format!("page gap after {retained_before} retained pages")
                }
                ExtractionScope::GlyphGap { retained_before } => {
                    format!("glyph gap after {retained_before} retained glyphs")
                }
                _ => "unknown".to_owned(),
            },
            description: issue.description.clone(),
        })
        .collect()
}

struct PairRunContext<'a> {
    cache_dir: &'a Path,
    manifest_dir: &'a Path,
    /// Uniform override; when unset every pair uses its manifest hint.
    limit_scale_override: Option<f64>,
    compare: bool,
}

fn run_pair(pair: &RevisionPair, context: &PairRunContext<'_>) -> PairRunReport {
    let started = Instant::now();
    let effective_scale = context
        .limit_scale_override
        .unwrap_or(pair.limit_scale_hint);
    let old_path = side_cache_path(context.cache_dir, &pair.pair_id, "old");
    let new_path = side_cache_path(context.cache_dir, &pair.pair_id, "new");
    let mut record = PairRunReport {
        pair_id: pair.pair_id.clone(),
        set: pair.set.label(),
        role: pair.role.label(),
        document_type: pair.document_type.clone(),
        in_scope: pair.in_scope,
        provenance_verified: false,
        compared: false,
        extraction_complete: None,
        comparison_complete: None,
        extraction_issues: Vec::new(),
        coverage_old: None,
        coverage_new: None,
        coverage_comparison: None,
        unresolved_regions: None,
        unresolved_old_token_share: None,
        unresolved_new_token_share: None,
        reported_content_changes: None,
        formatting_only_changes: None,
        uncertain_changes: None,
        reported_changes_preview: Vec::new(),
        quality: None,
        quality_skipped_reason: None,
        scoped_event_metrics: None,
        scoped_token_metrics: None,
        candidate_recall: None,
        expected_change_diagnostics: None,
        resource_limit_failure: None,
        candidate_visits: None,
        candidate_visits_required: None,
        candidate_visits_required_exact: None,
        candidate_visits_required_ngram: None,
        candidate_visits_required_short_fallback: None,
        max_candidate_visits: None,
        sentence_recovery_metrics: None,
        candidate_visit_pressure: None,
        runtime_ms: 0,
        limit_scale_used: effective_scale,
        status: PairRunStatus::Ok,
        failure: None,
    };

    let verification = verify_provenance(&pair.old, &old_path, "old")
        .and_then(|()| verify_provenance(&pair.new, &new_path, "new"));
    if let Err(reason) = verification {
        record.failure = Some(reason);
        return finish(record, started);
    }
    record.provenance_verified = true;
    if !context.compare {
        return finish(record, started);
    }

    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let (outcome, alignment) =
        match run_extraction_and_comparison(&source, &old_path, &new_path, effective_scale) {
            Ok(ComparisonWithMetrics {
                outcome,
                alignment,
                metrics,
                sentence_recovery_metrics,
                pressure,
            }) => {
                metrics.apply_to(&mut record);
                record.sentence_recovery_metrics = sentence_recovery_metrics;
                record.candidate_visit_pressure = pressure;
                (outcome, alignment)
            }
            Err(RevisionRunError::Read(reason)) => {
                record.failure = Some(reason);
                return finish(record, started);
            }
            Err(RevisionRunError::Limit {
                message,
                metrics,
                pressure,
            }) => {
                record.resource_limit_failure = Some(message);
                metrics.apply_to(&mut record);
                record.candidate_visit_pressure = pressure.map(|pressure| *pressure);
                record.quality_skipped_reason = Some(QUALITY_SKIP_RESOURCE_LIMIT.to_owned());
                return finish(record, started);
            }
            Err(RevisionRunError::Other(stage, message)) => {
                record.failure = Some(format!("{stage}: {message}"));
                return finish(record, started);
            }
        };
    record.compared = true;

    let summary = match summarize(&outcome.comparison, &outcome.extraction) {
        Ok(summary) => summary,
        Err(error) => {
            record.failure = Some(format!("summary failed: {error}"));
            return finish(record, started);
        }
    };
    let extraction_complete = summary.old_extraction_complete && summary.new_extraction_complete;
    record.extraction_complete = Some(extraction_complete);
    record.comparison_complete = Some(summary.comparison_complete);
    record.extraction_issues = issue_lines(&outcome.extraction);
    record.coverage_old = summary.old_alignment_coverage;
    record.coverage_new = summary.new_alignment_coverage;
    record.coverage_comparison = summary.comparison_coverage;
    record.unresolved_regions = Some(summary.unresolved_regions);
    let (old_share, new_share) = unresolved_token_shares(&outcome.comparison);
    record.unresolved_old_token_share = old_share;
    record.unresolved_new_token_share = new_share;
    record.reported_content_changes = Some(summary.content_changes);
    record.formatting_only_changes = Some(summary.formatting_only_changes);
    record.uncertain_changes = Some(summary.uncertain_changes);

    match (pair.expected_extraction, extraction_complete) {
        (ExpectedExtraction::Complete, true) | (ExpectedExtraction::Incomplete, false) => {}
        (ExpectedExtraction::Complete, false) => {
            record.failure = Some(
                "manifest expected complete extraction but extraction was incomplete".to_owned(),
            );
        }
        (ExpectedExtraction::Incomplete, true) => {
            record.failure =
                Some("manifest expected incomplete extraction but extraction completed".to_owned());
        }
    }

    let expected_document = pair.expected_file.as_ref().map(|file| {
        let path = context.manifest_dir.join(file);
        fs::read_to_string(&path)
            .map_err(|error| {
                format!(
                    "cannot read expected annotations {}: {error}",
                    path.display()
                )
            })
            .and_then(|text| load_expected_document(&text).map_err(|error| error.to_string()))
            .and_then(|document| {
                if document.pair == pair.pair_id {
                    Ok(document)
                } else {
                    Err(format!(
                        "expected annotations {} describe pair {:?} but this manifest row is {:?}",
                        path.display(),
                        document.pair,
                        pair.pair_id
                    ))
                }
            })
    });

    let expected = match expected_document {
        None => None,
        Some(Ok(document)) => Some(document),
        Some(Err(reason)) => {
            if record.failure.is_none() {
                record.failure = Some(reason);
            }
            None
        }
    };

    let actuals = if extraction_complete {
        let old_map = build_block_map(&outcome.old_blocks);
        let new_map = build_block_map(&outcome.new_blocks);
        Some(flatten_actual_changes(
            &outcome.comparison,
            [&old_map, &new_map],
        ))
    } else {
        None
    };

    if let Some(actuals) = &actuals {
        record.reported_changes_preview = actuals
            .iter()
            .map(|change| ReportedChangeText {
                kind: change_kind_name(change.kind),
                old_text: occurrence_preview(change, |occurrence| occurrence.old_text.as_deref()),
                new_text: occurrence_preview(change, |occurrence| occurrence.new_text.as_deref()),
            })
            .collect();
    }

    match (expected, extraction_complete) {
        (Some(document), true) if document.annotation == Annotation::ScopedComplete => {
            let scoped =
                resolve_revision_scopes(&document.scopes, &outcome.old_blocks, &outcome.new_blocks)
                    .and_then(|scopes| {
                        let expected_tokens = validate_scoped_expected_changes(
                            &document.changes,
                            &scopes,
                            &outcome.old_blocks,
                            &outcome.new_blocks,
                        )?;
                        let changes = classify_scoped_changes(
                            &outcome.comparison.changes,
                            &scopes,
                            &outcome.old_blocks,
                            &outcome.new_blocks,
                        )?;
                        let token_metrics = evaluate_scoped_token_metrics(
                            &outcome.comparison.changes,
                            &changes,
                            expected_tokens,
                            &scopes,
                            &outcome.old_blocks,
                            &outcome.new_blocks,
                        )?;
                        Ok((scopes, changes, token_metrics))
                    });
            match scoped {
                Ok((scopes, changes, token_metrics)) => {
                    let all_actuals = actuals.unwrap_or_default();
                    let scoped_actuals = changes
                        .iter()
                        .map(|change| all_actuals[change.change_index].clone())
                        .collect::<Vec<_>>();
                    let actual_scopes = changes
                        .iter()
                        .map(|change| change.scope_id.clone())
                        .collect::<Vec<_>>();
                    let (quality, scoped_event_metrics) = scoped_quality(
                        scopes.len(),
                        &document.changes,
                        &scoped_actuals,
                        &actual_scopes,
                    );
                    record.quality = Some(quality);
                    record.scoped_event_metrics = Some(scoped_event_metrics);
                    record.scoped_token_metrics = Some(token_metrics);
                }
                Err(reason) => record.quality_skipped_reason = Some(reason),
            }
        }
        (Some(document), true) => {
            let actuals = actuals.unwrap_or_default();
            let match_outcome = match_changes(&document.changes, &actuals);
            record.quality = Some(quality_from_match_outcome(
                document.annotation,
                &document.changes,
                &actuals,
                &match_outcome,
            ));
            match evaluate_reviewed_diagnostics(
                &document.changes,
                &outcome.old_blocks,
                &outcome.new_blocks,
                alignment.as_ref(),
                &outcome.comparison,
                &actuals,
                &match_outcome,
            ) {
                Ok(diagnostics) => {
                    record.candidate_recall = diagnostics.candidate_recall;
                    record.expected_change_diagnostics =
                        Some(diagnostics.expected_change_diagnostics);
                }
                Err(reason) if record.failure.is_none() => {
                    record.failure = Some(format!("reviewed diagnostics failed: {reason}"));
                }
                Err(_) => {}
            }
        }
        (Some(_), false) => {
            record.quality_skipped_reason = Some(QUALITY_SKIP_INCOMPLETE_EXTRACTION.to_owned());
        }
        (None, _) if pair.expected_file.is_some() => {}
        (None, _) => {
            record.quality_skipped_reason = Some(QUALITY_SKIP_NO_ANNOTATIONS.to_owned());
        }
    }

    finish(record, started)
}

fn occurrence_preview<'a>(
    change: &'a ActualChange,
    text: impl Fn(&'a ActualChangeOccurrence) -> Option<&'a str>,
) -> Option<String> {
    let mut unique = Vec::new();
    for value in change.occurrences.iter().filter_map(text) {
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    (!unique.is_empty()).then(|| truncate_preview(&unique.join(" | ")))
}

/// Single exit point that derives the final status: an explicit failure wins,
/// then a resource-limit stop; everything else stays `Ok`.
fn finish(mut record: PairRunReport, started: Instant) -> PairRunReport {
    record.status = if record.failure.is_some() {
        PairRunStatus::Failed
    } else if record.resource_limit_failure.is_some() {
        PairRunStatus::Limit
    } else {
        PairRunStatus::Ok
    };
    record.runtime_ms = started.elapsed().as_millis();
    record
}

#[derive(Debug)]
enum RevisionRunError {
    Read(String),
    Limit {
        message: String,
        metrics: Box<VisitMetrics>,
        pressure: Option<Box<CandidateVisitPressure>>,
    },
    Other(&'static str, String),
}

type RevisionOutcome = pdfdelta_core::pipeline::ComparisonOutcome;

/// Alignment candidate visit metrics: attempted charge, required full sum,
/// its exact/ngram/short-fallback components, and the budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct VisitMetrics {
    candidate_visits: Option<usize>,
    candidate_visits_required: Option<usize>,
    candidate_visits_required_exact: Option<usize>,
    candidate_visits_required_ngram: Option<usize>,
    candidate_visits_required_short_fallback: Option<usize>,
    max_candidate_visits: Option<usize>,
}

impl VisitMetrics {
    fn apply_to(self, record: &mut PairRunReport) {
        record.candidate_visits = self.candidate_visits;
        record.candidate_visits_required = self.candidate_visits_required;
        record.candidate_visits_required_exact = self.candidate_visits_required_exact;
        record.candidate_visits_required_ngram = self.candidate_visits_required_ngram;
        record.candidate_visits_required_short_fallback =
            self.candidate_visits_required_short_fallback;
        record.max_candidate_visits = self.max_candidate_visits;
    }
}

/// Validates one alignment metrics record. Valid states: alignment reached
/// with the required sum completed and either a full component breakdown
/// (inverted index) or no breakdown (generic generator); a limit stop where
/// the required sum is unavailable; and alignment never reached. Any other
/// partial state, or a component sum that disagrees with the required
/// total, is a contract violation and is reported as an error rather than
/// silently treated as a measurement.
fn validate_visit_metrics(metrics: VisitMetrics) -> std::result::Result<VisitMetrics, String> {
    let VisitMetrics {
        candidate_visits,
        candidate_visits_required,
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
        max_candidate_visits,
    } = metrics;
    let components = (
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
    );
    let valid = match (
        candidate_visits,
        candidate_visits_required,
        max_candidate_visits,
    ) {
        (Some(_), Some(_), Some(_)) => {
            matches!(components, (Some(_), Some(_), Some(_)) | (None, None, None))
        }
        (Some(_), None, Some(_)) | (None, None, None) => matches!(components, (None, None, None)),
        _ => false,
    };
    if !valid {
        return Err(format!(
            "alignment metrics contract violation: candidate_visits={candidate_visits:?}, candidate_visits_required={candidate_visits_required:?}, candidate_visits_required_exact={candidate_visits_required_exact:?}, candidate_visits_required_ngram={candidate_visits_required_ngram:?}, candidate_visits_required_short_fallback={candidate_visits_required_short_fallback:?}, max_candidate_visits={max_candidate_visits:?}"
        ));
    }
    if let (Some(required), Some(exact), Some(ngram), Some(short_fallback)) = (
        candidate_visits_required,
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
    ) {
        let components_sum = exact
            .checked_add(ngram)
            .and_then(|sum| sum.checked_add(short_fallback));
        if components_sum != Some(required) {
            return Err(format!(
                "alignment metrics contract violation: required candidate components {exact}+{ngram}+{short_fallback} do not sum to {required}"
            ));
        }
    }
    Ok(metrics)
}

/// Extracts the alignment candidate visit metrics from the diagnostics.
/// Returns all-`None` metrics when alignment was never reached or recorded
/// no charge; a partial record is a contract violation and is reported as
/// an error.
fn alignment_visit_metrics(
    diagnostics: &PipelineDiagnostics,
) -> std::result::Result<VisitMetrics, String> {
    let Some(record) = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
    else {
        return Ok(VisitMetrics::default());
    };
    validate_visit_metrics(VisitMetrics {
        candidate_visits: record.metrics.candidate_visits,
        candidate_visits_required: record.metrics.candidate_visits_required,
        candidate_visits_required_exact: record.metrics.candidate_visits_required_exact,
        candidate_visits_required_ngram: record.metrics.candidate_visits_required_ngram,
        candidate_visits_required_short_fallback: record
            .metrics
            .candidate_visits_required_short_fallback,
        max_candidate_visits: record.metrics.max_candidate_visits,
    })
}

fn validate_sentence_recovery_metrics(
    metrics: SentenceRecoveryMetrics,
) -> std::result::Result<SentenceRecoveryMetricsReport, String> {
    validate_structural_pairing_metrics(metrics)?;
    if metrics.near_pair_visits_examined > metrics.near_pair_visits_attempted {
        return Err(format!(
            "examined near pair visits {} exceed attempted visits {}",
            metrics.near_pair_visits_examined, metrics.near_pair_visits_attempted
        ));
    }
    if metrics.near_similarity_comparisons_examined > metrics.near_similarity_comparisons_attempted
    {
        return Err(format!(
            "examined near similarity comparisons {} exceed attempted comparisons {}",
            metrics.near_similarity_comparisons_examined,
            metrics.near_similarity_comparisons_attempted
        ));
    }
    let pair_visit_deficit = metrics.near_pair_visits_examined < metrics.near_pair_visits_attempted;
    let comparison_deficit = metrics.near_similarity_comparisons_examined
        < metrics.near_similarity_comparisons_attempted;
    if pair_visit_deficit && comparison_deficit {
        return Err("near relation has multiple unfinished work counters".to_owned());
    }
    if metrics.near_relation_complete {
        if metrics.near_candidate_count_truncated {
            return Err("complete near relation has truncated candidates".to_owned());
        }
        if pair_visit_deficit || comparison_deficit {
            return Err("complete near relation has unexamined work".to_owned());
        }
        if metrics.near_relation_stop_reason.is_some() {
            return Err("complete near relation has a stop reason".to_owned());
        }
    }
    match (
        metrics.near_relation_stop_reason,
        pair_visit_deficit,
        comparison_deficit,
    ) {
        (None, true, _) | (None, _, true) => {
            return Err(
                "incomplete near relation has unexamined work without a stop reason".to_owned(),
            );
        }
        (Some(NearRelationStopReason::PairVisitLimit), true, false) => {}
        (Some(NearRelationStopReason::PairVisitLimit), _, _) => {
            return Err("pair visit stop reason does not match unfinished work".to_owned());
        }
        (Some(NearRelationStopReason::SimilarityComparisonLimit), false, true) => {}
        (Some(NearRelationStopReason::SimilarityComparisonLimit), _, _) => {
            return Err(
                "similarity comparison stop reason does not match unfinished work".to_owned(),
            );
        }
        (Some(NearRelationStopReason::CandidateCountLimit), false, false)
            if metrics.near_candidate_count_truncated => {}
        (Some(NearRelationStopReason::CandidateCountLimit), _, _) => {
            return Err(
                "candidate count stop reason requires truncation without unfinished search work"
                    .to_owned(),
            );
        }
        _ => {}
    }
    if metrics.near_candidate_count_truncated && metrics.near_relation_stop_reason.is_none() {
        return Err("candidate truncation has no stop reason".to_owned());
    }
    if metrics.vetoed_near_pairs > metrics.near_pair_candidates {
        return Err(format!(
            "vetoed near pairs {} exceed near pair candidates {}",
            metrics.vetoed_near_pairs, metrics.near_pair_candidates
        ));
    }
    let recovered_old = metrics
        .recovered_exact_match_old_tokens
        .checked_add(metrics.recovered_replacement_old_tokens)
        .and_then(|tokens| tokens.checked_add(metrics.recovered_deletion_tokens))
        .ok_or_else(|| "old recovered token counters overflow".to_owned())?;
    let recovered_new = metrics
        .recovered_exact_match_new_tokens
        .checked_add(metrics.recovered_replacement_new_tokens)
        .and_then(|tokens| tokens.checked_add(metrics.recovered_insertion_tokens))
        .ok_or_else(|| "new recovered token counters overflow".to_owned())?;
    recovered_old
        .checked_add(metrics.unresolved_remainder_old_source_tokens)
        .ok_or_else(|| "old eligible source token counters overflow".to_owned())?;
    recovered_new
        .checked_add(metrics.unresolved_remainder_new_source_tokens)
        .ok_or_else(|| "new eligible source token counters overflow".to_owned())?;
    Ok(metrics.into())
}

fn validate_structural_pairing_metrics(
    metrics: SentenceRecoveryMetrics,
) -> std::result::Result<(), String> {
    let structural_counters = [
        metrics.old_structural_descriptors,
        metrics.new_structural_descriptors,
        metrics.old_structural_eligible_descriptors,
        metrics.new_structural_eligible_descriptors,
        metrics.old_structural_mixed_descriptors,
        metrics.new_structural_mixed_descriptors,
        metrics.old_structural_split_descriptors,
        metrics.new_structural_split_descriptors,
        metrics.structural_shared_profiles,
        metrics.structural_candidate_pairs,
        metrics.structural_largest_posting,
        metrics.structural_duplicate_pairs,
        metrics.structural_unique_reciprocal_pairs,
        metrics.structural_unique_no_anchor_pairs,
        metrics.structural_unique_monotone_anchor_pairs,
        metrics.structural_unique_crossing_veto_pairs,
    ];
    if !metrics.structural_pairing_available {
        if structural_counters.iter().any(|counter| *counter != 0) {
            return Err("unavailable structural pairing has nonzero counters".to_owned());
        }
        return Ok(());
    }
    let old_partition = metrics
        .old_structural_eligible_descriptors
        .checked_add(metrics.old_structural_mixed_descriptors)
        .and_then(|count| count.checked_add(metrics.old_structural_split_descriptors))
        .ok_or_else(|| "old structural descriptor partition overflows".to_owned())?;
    if old_partition != metrics.old_structural_descriptors {
        return Err("old structural descriptor partition is inconsistent".to_owned());
    }
    let new_partition = metrics
        .new_structural_eligible_descriptors
        .checked_add(metrics.new_structural_mixed_descriptors)
        .and_then(|count| count.checked_add(metrics.new_structural_split_descriptors))
        .ok_or_else(|| "new structural descriptor partition overflows".to_owned())?;
    if new_partition != metrics.new_structural_descriptors {
        return Err("new structural descriptor partition is inconsistent".to_owned());
    }
    let pair_partition = metrics
        .structural_duplicate_pairs
        .checked_add(metrics.structural_unique_reciprocal_pairs)
        .ok_or_else(|| "structural candidate pair partition overflows".to_owned())?;
    if pair_partition != metrics.structural_candidate_pairs {
        return Err("structural candidate pair partition is inconsistent".to_owned());
    }
    let anchor_partition = metrics
        .structural_unique_no_anchor_pairs
        .checked_add(metrics.structural_unique_monotone_anchor_pairs)
        .and_then(|count| count.checked_add(metrics.structural_unique_crossing_veto_pairs))
        .ok_or_else(|| "structural anchor class partition overflows".to_owned())?;
    if anchor_partition != metrics.structural_unique_reciprocal_pairs {
        return Err("structural anchor class partition is inconsistent".to_owned());
    }
    if metrics.structural_shared_profiles
        > metrics
            .old_structural_eligible_descriptors
            .min(metrics.new_structural_eligible_descriptors)
    {
        return Err("shared structural profiles exceed eligible descriptors".to_owned());
    }
    if (metrics.structural_shared_profiles == 0) != (metrics.structural_largest_posting == 0) {
        return Err("structural largest posting disagrees with shared profiles".to_owned());
    }
    Ok(())
}

/// Extracts the optional nested sentence-recovery metrics from the completed
/// exact-diff diagnostic record. Real zero measurements remain present.
fn sentence_recovery_metrics(
    diagnostics: &PipelineDiagnostics,
) -> std::result::Result<Option<SentenceRecoveryMetricsReport>, String> {
    let mut records = diagnostics
        .records()
        .iter()
        .filter(|record| record.phase == PipelinePhase::ExactDiff);
    let Some(record) = records.next() else {
        return Ok(None);
    };
    if records.next().is_some() {
        return Err("diagnostics contain multiple exact-diff records".to_owned());
    }
    let Some(metrics) = record.metrics.sentence_recovery_metrics else {
        return Ok(None);
    };
    if record.status != PipelinePhaseStatus::Completed {
        return Err("incomplete exact-diff record contains sentence recovery metrics".to_owned());
    }
    validate_sentence_recovery_metrics(metrics).map(Some)
}

/// Resolves the supplementary pressure from the alignment charge and the
/// pressure attempt. Complete extraction requires a successful attempt once
/// alignment is reached; incomplete extraction intentionally has no attempt.
fn resolve_pressure(
    candidate_visits: Option<usize>,
    pressure_required: bool,
    pressure_result: Option<Result<CandidateVisitPressure>>,
) -> std::result::Result<Option<CandidateVisitPressure>, RevisionRunError> {
    if candidate_visits.is_none() || !pressure_required {
        return Ok(None);
    }
    match (candidate_visits, pressure_result) {
        (Some(_), Some(Ok(pressure))) => Ok(Some(pressure)),
        (Some(_), Some(Err(error))) => Err(RevisionRunError::Other(
            "candidate visit pressure",
            error.to_string(),
        )),
        (Some(_), None) => Err(RevisionRunError::Other(
            "candidate visit pressure contract violation",
            "alignment reached without a pressure attempt".to_owned(),
        )),
        (None, _) => Ok(None),
    }
}

/// Comparison outcome plus the alignment candidate visit metrics and
/// supplementary pressure.
#[derive(Debug)]
struct ComparisonWithMetrics {
    outcome: RevisionOutcome,
    alignment: Option<Alignment>,
    metrics: VisitMetrics,
    sentence_recovery_metrics: Option<SentenceRecoveryMetricsReport>,
    pressure: Option<CandidateVisitPressure>,
}

/// Compares two extracted outcomes and returns the alignment candidate
/// visit metrics and the supplementary all-old-block pressure alongside the
/// outcome; a candidate limit stop carries the metrics and the pressure in
/// the error. A metrics contract violation takes priority over the core
/// result. Once alignment is reached, a pressure measurement failure is a
/// benchmark instrumentation failure, not missing supplementary
/// information: it takes priority over the core Ok/Limit result and
/// surfaces as `Failed`.
fn compare_outcomes_with_metrics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
) -> std::result::Result<ComparisonWithMetrics, RevisionRunError> {
    // Measure the supplementary pressure from the borrowed documents before
    // the outcomes are consumed; whether it is required depends on whether
    // alignment is reached below.
    let pressure_required = old.is_complete() && new.is_complete();
    let pressure_result = if pressure_required {
        Some(evaluate_candidate_visit_pressure(
            old.document(),
            new.document(),
            options,
        ))
    } else {
        None
    };
    let mut diagnostics = PipelineDiagnostics::new();
    let result =
        compare_extraction_outcomes_with_alignment_diagnostics(old, new, options, &mut diagnostics);
    let metrics = alignment_visit_metrics(&diagnostics).map_err(|message| {
        RevisionRunError::Other("alignment metrics contract violation", message)
    })?;
    let sentence_recovery_metrics = sentence_recovery_metrics(&diagnostics).map_err(|message| {
        RevisionRunError::Other("sentence recovery metrics contract violation", message)
    })?;
    let pressure = resolve_pressure(metrics.candidate_visits, pressure_required, pressure_result)?;
    let (outcome, alignment) = result.map_err(|error| match error {
        pdfdelta_core::Error::LimitExceeded { .. } => RevisionRunError::Limit {
            message: error.to_string(),
            metrics: Box::new(metrics),
            pressure: pressure.map(Box::new),
        },
        other => RevisionRunError::Other("comparison failed", other.to_string()),
    })?;
    Ok(ComparisonWithMetrics {
        outcome,
        alignment,
        metrics,
        sentence_recovery_metrics,
        pressure,
    })
}

fn run_extraction_and_comparison(
    source: &ParserBackedGlyphSource<LopdfParser, ContentStreamGlyphExtractor>,
    old_path: &Path,
    new_path: &Path,
    limit_scale: f64,
) -> std::result::Result<ComparisonWithMetrics, RevisionRunError> {
    let read = |path: &Path| {
        fs::read(path).map_err(|error| {
            RevisionRunError::Read(format!("cannot read {}: {error}", path.display()))
        })
    };
    let old_bytes = read(old_path)?;
    let new_bytes = read(new_path)?;
    let extract = |bytes: Vec<u8>| {
        // Backend failures are fatal in core by design; the benchmark records
        // them as document-scoped unresolved outcomes so manifest
        // expectations can classify the pair instead of aborting the run.
        match source.extract_outcome(
            Arc::from(bytes),
            ParseLimits::default(),
            ExtractionLimits::default(),
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let issue = ExtractionIssue::new(
                    ExtractionIssueKind::Unresolved,
                    ExtractionScope::Document,
                    format!("backend failure: {error}"),
                )
                .expect("backend failure description is never blank");
                ExtractionOutcome::new(Document::new(Vec::new()), vec![issue])
                    .expect("a single document-scoped issue is always a valid outcome")
            }
        }
    };
    let old_outcome = extract(old_bytes);
    let new_outcome = extract(new_bytes);
    let options = PipelineOptions::default()
        .scaled_limits(limit_scale)
        .map_err(|error| RevisionRunError::Other("limit scaling failed", error.to_string()))?;
    compare_outcomes_with_metrics(old_outcome, new_outcome, options)
}

pub fn run_revision_benchmark(
    manifest_path: &Path,
    cache_dir: &Path,
    set_filter: Option<PairSet>,
    pair_filter: Option<&str>,
    limit_scale: Option<f64>,
    compare: bool,
) -> Result<Vec<PairRunReport>> {
    if let Some(scale) = limit_scale {
        validate_limit_scale(scale)?;
    }
    let manifest_text = fs::read_to_string(manifest_path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "cannot read revision manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let mut pairs = parse_manifest(&manifest_text)?;
    if let Some(set_filter) = set_filter {
        pairs.retain(|pair| pair.set == set_filter);
    }
    if let Some(pair_filter) = pair_filter {
        if !pairs.iter().any(|pair| pair.pair_id == pair_filter) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest {} records no pair {pair_filter:?}",
                manifest_path.display()
            )));
        }
        pairs.retain(|pair| pair.pair_id == pair_filter);
    }
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let context = PairRunContext {
        cache_dir,
        manifest_dir,
        limit_scale_override: limit_scale,
        compare,
    };
    Ok(pairs.iter().map(|pair| run_pair(pair, &context)).collect())
}

pub fn summarize_reports(reports: &[PairRunReport]) -> String {
    let healthy = reports.iter().filter(|record| record.healthy()).count();
    let limited = reports
        .iter()
        .filter(|record| record.status == PairRunStatus::Limit)
        .count();
    let failed = reports.len() - healthy - limited;
    format!(
        "{healthy}/{} revision pairs healthy; {limited} stopped at resource limits; {failed} failed",
        reports.len()
    )
}

/// Resolves a path to its canonical parent directory and file name, rejecting
/// missing parent directories and paths without a file name.
pub fn normalize_output_destination(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let canonical_parent = parent.canonicalize().map_err(|error| {
        BenchError::InvalidInput(format!(
            "destination directory does not exist or cannot be resolved {}: {error}",
            parent.display()
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "destination path {} must have a file name",
            path.display()
        ))
    })?;
    Ok(canonical_parent.join(file_name))
}

static TEMP_PUBLISH_COUNTER: AtomicU64 = AtomicU64::new(0);
pub(crate) const MAX_TEMP_CREATE_ATTEMPTS: usize = 64;

fn create_temp_artifact_with_counter(
    parent: &Path,
    counter: &AtomicU64,
) -> Result<(fs::File, PathBuf)> {
    let pid = std::process::id();
    for _ in 0..MAX_TEMP_CREATE_ATTEMPTS {
        let seq = counter.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".pdfbench-artifact-{pid}-{seq}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((file, temp_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(error) => {
                return Err(BenchError::Publication(format!(
                    "cannot create temporary artifact {}: {error}",
                    temp_path.display()
                )));
            }
        }
    }
    Err(BenchError::Publication(format!(
        "exhausted {MAX_TEMP_CREATE_ATTEMPTS} attempts creating unique temporary artifact in {}",
        parent.display()
    )))
}

/// Atomically publishes `bytes` to a new file at `path`, refusing to overwrite
/// any existing destination file, symlink, or directory. Cleans up temporary
/// files on failure.
pub fn publish_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_new_file_with_counter(path, bytes, &TEMP_PUBLISH_COUNTER)
}

pub(crate) fn publish_new_file_with_counter(
    path: &Path,
    bytes: &[u8],
    counter: &AtomicU64,
) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if !parent.exists() {
        return Err(BenchError::Publication(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }

    if path.symlink_metadata().is_ok() {
        return Err(BenchError::Publication(format!(
            "destination path already exists: {}",
            path.display()
        )));
    }

    let (mut temp_file, temp_path) = create_temp_artifact_with_counter(parent, counter)?;

    let write_result = (|| -> Result<()> {
        temp_file.write_all(bytes).map_err(|error| {
            BenchError::Publication(format!(
                "cannot write temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        temp_file.flush().map_err(|error| {
            BenchError::Publication(format!(
                "cannot flush temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        temp_file.sync_all().map_err(|error| {
            BenchError::Publication(format!(
                "cannot sync temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    let link_result = fs::hard_link(&temp_path, path);
    let _ = fs::remove_file(&temp_path);

    link_result.map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            BenchError::Publication(format!(
                "destination path already exists: {}",
                path.display()
            ))
        } else {
            BenchError::Publication(format!(
                "cannot publish artifact to {}: {error}",
                path.display()
            ))
        }
    })
}

/// Writes every record as a pretty-printed JSON array to a new file, refusing to
/// overwrite an existing destination path via atomic publication.
pub fn write_reports_json(path: &Path, reports: &[PairRunReport]) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(reports).map_err(|error| {
        BenchError::Publication(format!("cannot serialize revision JSON report: {error}"))
    })?;
    publish_new_file(path, &bytes)
}

/// Compact machine-readable revision summary document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RevisionSummaryReport {
    pub schema_version: u32,
    pub records: Vec<RevisionSummaryRecord>,
}

impl RevisionSummaryReport {
    pub const SCHEMA_VERSION: u32 = 9;

    pub fn from_reports(reports: &[PairRunReport]) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            records: reports
                .iter()
                .map(RevisionSummaryRecord::from_pair_report)
                .collect(),
        }
    }
}

/// A compact, stable machine-readable record for one revision pair evaluation.
/// Contains all evidence fields needed to reproduce benchmark claims without
/// runtime measurements, raw text previews, or host-specific paths. Detailed
/// error diagnostics remain available in the full report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RevisionSummaryRecord {
    pub pair_id: String,
    pub set: String,
    pub role: String,
    pub in_scope: bool,
    pub status: PairRunStatus,
    pub provenance_verified: bool,
    pub compared: bool,
    pub extraction_complete: Option<bool>,
    pub comparison_complete: Option<bool>,
    pub limit_scale_used: f64,
    pub resource_limit_failure: Option<String>,
    pub coverage_old: Option<f64>,
    pub coverage_new: Option<f64>,
    pub coverage_comparison: Option<f64>,
    pub unresolved_regions: Option<usize>,
    pub unresolved_old_token_share: Option<f64>,
    pub unresolved_new_token_share: Option<f64>,
    pub reported_content_changes: Option<usize>,
    pub reported_formatting_changes: Option<usize>,
    pub reported_uncertain_changes: Option<usize>,
    pub sentence_recovery_metrics: Option<SentenceRecoveryMetricsReport>,
    pub quality: Option<QualityMetrics>,
    pub quality_skipped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoped_event_metrics: Option<ScopedEventMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoped_token_metrics: Option<ScopedTokenMetrics>,
    pub candidate_recall: Option<CandidateRecallMetrics>,
    pub expected_change_diagnostics: Option<ExpectedChangeDiagnostics>,
}

impl RevisionSummaryRecord {
    pub fn from_pair_report(report: &PairRunReport) -> Self {
        Self {
            pair_id: report.pair_id.clone(),
            set: report.set.to_owned(),
            role: report.role.to_owned(),
            in_scope: report.in_scope,
            status: report.status,
            provenance_verified: report.provenance_verified,
            compared: report.compared,
            extraction_complete: report.extraction_complete,
            comparison_complete: report.comparison_complete,
            limit_scale_used: report.limit_scale_used,
            resource_limit_failure: report.resource_limit_failure.clone(),
            coverage_old: report.coverage_old,
            coverage_new: report.coverage_new,
            coverage_comparison: report.coverage_comparison,
            unresolved_regions: report.unresolved_regions,
            unresolved_old_token_share: report.unresolved_old_token_share,
            unresolved_new_token_share: report.unresolved_new_token_share,
            reported_content_changes: report.reported_content_changes,
            reported_formatting_changes: report.formatting_only_changes,
            reported_uncertain_changes: report.uncertain_changes,
            sentence_recovery_metrics: report.sentence_recovery_metrics,
            quality: report.quality,
            quality_skipped_reason: report.quality_skipped_reason.clone(),
            scoped_event_metrics: report.scoped_event_metrics,
            scoped_token_metrics: report.scoped_token_metrics,
            candidate_recall: report.candidate_recall,
            expected_change_diagnostics: report.expected_change_diagnostics.clone(),
        }
    }
}

/// Writes a compact summary of every record as pretty-printed JSON with a trailing
/// newline to a new file, refusing to overwrite an existing destination path via
/// atomic publication.
pub fn write_summary_json(path: &Path, reports: &[PairRunReport]) -> Result<()> {
    let summary = RevisionSummaryReport::from_reports(reports);
    let mut bytes = serde_json::to_vec_pretty(&summary).map_err(|error| {
        BenchError::Publication(format!(
            "cannot serialize revision summary JSON report: {error}"
        ))
    })?;
    bytes.push(b'\n');
    publish_new_file(path, &bytes)
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        alignment::AlignmentOptions,
        model::{
            DecodedText, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
            GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
        },
        pdf::ObjectRef,
    };

    use super::*;

    fn manifest_row(
        pair_id: &str,
        set: &str,
        role: &str,
        in_scope: &str,
        extraction: &str,
    ) -> String {
        format!(
            "{pair_id}\t{set}\t{role}\tgovernment-guidance\tsingle-column\t{in_scope}\t-\t2026-08-24\t{extraction}\t1.0\texpected/{pair_id}.json\t\
             https://example.test/{pair_id}-old.pdf\t100\t{}\thttps://example.test/{pair_id}-new.pdf\t200\t{}",
            "a".repeat(64),
            "b".repeat(64)
        )
    }

    fn full_manifest() -> String {
        format!(
            "# captured fixtures\n{}\n{}\n{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row("alpha", "dev", "standard", "true", "complete"),
            manifest_row("beta", "holdout", "stress", "false", "incomplete"),
            manifest_row("gamma", "holdout", "standard", "true", "complete")
        )
    }

    #[test]
    fn manifest_rows_round_trip_through_the_parser() {
        let pairs = parse_manifest(&full_manifest()).expect("valid manifest");
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].set, PairSet::Dev);
        assert_eq!(pairs[1].role, PairRole::Stress);
        assert!(!pairs[1].in_scope);
        assert_eq!(pairs[1].known_issues, None);
        assert_eq!(pairs[0].expected_extraction, ExpectedExtraction::Complete);
        assert_eq!(
            pairs[0].expected_file.as_deref(),
            Some("expected/alpha.json")
        );
        assert_eq!(pairs[0].limit_scale_hint, 1.0);
        assert_eq!(pairs[0].old.sha256, "a".repeat(64));
        assert_eq!(pairs[0].new.byte_count, 200);
    }

    #[test]
    fn manifest_parser_rejects_malformed_input() {
        let header = MANIFEST_HEADER.join("\t");
        assert!(
            parse_manifest(&header).is_err(),
            "header alone records no pairs"
        );
        assert!(parse_manifest(&format!("{header}\nshort\trow\n")).is_err());
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "maybe", "complete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete")
                    .replace(&"a".repeat(64), "zz")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete"),
                manifest_row("alpha", "holdout", "stress", "true", "incomplete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete").replace('\t', "|")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&manifest_row(
                "alpha", "dev", "standard", "true", "complete"
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "sideways", "standard", "true", "complete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "huge", "true", "complete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "sometimes")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete")
                    .replace("\t1.0\t", "\t0.5\t")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete")
                    .replace(&"b".repeat(64), &"a".repeat(64))
            ))
            .is_err()
        );
    }

    #[test]
    fn expected_documents_enforce_version_and_quote_shapes() {
        let template = r#"{"version":1,"pair":"p","reviewed_on":"2026-08-24","annotation":"partial","changes":[CHANGES]}"#;
        let replacement =
            r#"{"id":"c1","kind":"replacement","old_quote":"ten days","new_quote":"twenty days"}"#;
        let insertion_bad =
            r#"{"id":"c2","kind":"insertion","old_quote":"leftover","new_quote":"added"}"#;
        let insertion_good = r#"{"id":"c2","kind":"insertion","new_quote":"added"}"#;
        let deletion_good = r#"{"id":"c3","kind":"deletion","old_quote":"removed"}"#;
        let with_changes = |changes: &str| template.replace("[CHANGES]", &format!("[{changes}]"));

        assert!(load_expected_document(&with_changes(replacement)).is_ok());
        assert!(load_expected_document(&with_changes(insertion_bad)).is_err());
        assert!(load_expected_document(&with_changes(insertion_good)).is_ok());
        assert!(load_expected_document(&with_changes(deletion_good)).is_ok());
        assert!(
            load_expected_document(
                &with_changes(replacement).replace(r#""version":1"#, r#""version":2"#)
            )
            .is_err()
        );

        let duplicate_ids = r#"{"version":1,"pair":"p","reviewed_on":"x","annotation":"complete","changes":[{"id":"c1","kind":"deletion","old_quote":"a"},{"id":"c1","kind":"deletion","old_quote":"b"}]}"#;
        assert!(load_expected_document(duplicate_ids).is_err());

        let unknown_field =
            r#"{"version":1,"pair":"p","reviewed_on":"x","annotation":"partial","surprise":true}"#;
        assert!(load_expected_document(unknown_field).is_err());
    }

    #[test]
    fn scoped_complete_uses_exact_tag_and_loads_valid_model() {
        assert_eq!(
            serde_json::to_string(&Annotation::ScopedComplete).expect("annotation serializes"),
            r#""scoped_complete""#
        );
        let document = load_expected_document(
            r#"{
                "version": 1,
                "pair": "p",
                "reviewed_on": "2026-08-29",
                "annotation": "scoped_complete",
                "scopes": [{
                    "id": "body",
                    "old": {"start_quote": "Old start", "end_quote": "Old end"},
                    "new": {"start_quote": "New start", "end_quote": "New end"}
                }],
                "changes": [{
                    "id": "c1",
                    "kind": "replacement",
                    "scope": "body",
                    "old_quote": "before",
                    "new_quote": "after"
                }]
            }"#,
        )
        .expect("valid scoped-complete annotation");

        assert_eq!(document.annotation, Annotation::ScopedComplete);
        assert_eq!(document.scopes.len(), 1);
        assert_eq!(document.scopes[0].id, "body");
        assert_eq!(document.scopes[0].old.start_quote, "Old start");
        assert_eq!(document.changes[0].scope.as_deref(), Some("body"));
    }

    #[test]
    fn scoped_complete_rejects_invalid_scope_syntax() {
        let valid = r#"{
            "version": 1,
            "pair": "p",
            "reviewed_on": "2026-08-29",
            "annotation": "scoped_complete",
            "scopes": [{
                "id": "body",
                "old": {"start_quote": "Old start", "end_quote": "Old end"},
                "new": {"start_quote": "New start", "end_quote": "New end"}
            }],
            "changes": [{
                "id": "c1",
                "kind": "deletion",
                "scope": "body",
                "old_quote": "removed"
            }]
        }"#;

        assert!(load_expected_document(&valid.replace(r#""id": "body","#, "")).is_err());
        assert!(
            load_expected_document(&valid.replace(r#""id": "body""#, r#""id": "   ""#)).is_err()
        );
        let duplicate = valid.replace(
            r#"}],
            "changes""#,
            r#"}, {
                "id": "body",
                "old": {"start_quote": "Other old start", "end_quote": "Other old end"},
                "new": {"start_quote": "Other new start", "end_quote": "Other new end"}
            }],
            "changes""#,
        );
        assert!(load_expected_document(&duplicate).is_err());
        assert!(load_expected_document(&valid.replace("Old start", r" \n\t ")).is_err());
        assert!(load_expected_document(&valid.replace("New end", r" \n\t ")).is_err());
        assert!(
            load_expected_document(&valid.replace(r#""scope": "body""#, r#""scope": "other""#))
                .is_err()
        );
        assert!(load_expected_document(&valid.replace("scoped_complete", "complete")).is_err());
    }

    #[test]
    fn annotation_modes_reject_missing_or_mixed_scope_references() {
        let scoped_without_scopes = r#"{
            "version":1,"pair":"p","reviewed_on":"x","annotation":"scoped_complete",
            "changes":[]
        }"#;
        assert!(load_expected_document(scoped_without_scopes).is_err());

        let scoped_without_reference = r#"{
            "version":1,"pair":"p","reviewed_on":"x","annotation":"scoped_complete",
            "scopes":[{
                "id":"s",
                "old":{"start_quote":"a","end_quote":"b"},
                "new":{"start_quote":"c","end_quote":"d"}
            }],
            "changes":[{"id":"c","kind":"deletion","old_quote":"removed"}]
        }"#;
        assert!(load_expected_document(scoped_without_reference).is_err());

        let legacy_with_reference = r#"{
            "version":1,"pair":"p","reviewed_on":"x","annotation":"partial",
            "changes":[{"id":"c","kind":"deletion","scope":"s","old_quote":"removed"}]
        }"#;
        assert!(load_expected_document(legacy_with_reference).is_err());
    }

    fn expected_change(
        id: &str,
        kind: ExpectedKind,
        old: Option<&str>,
        new: Option<&str>,
    ) -> ExpectedChange {
        ExpectedChange {
            id: id.to_owned(),
            kind,
            scope: None,
            old_quote: old.map(str::to_owned),
            new_quote: new.map(str::to_owned),
            note: String::new(),
        }
    }

    fn actual_change(
        kind: ChangeKind,
        old_text: Option<&str>,
        new_text: Option<&str>,
        old_len: Option<usize>,
        new_len: Option<usize>,
    ) -> ActualChange {
        ActualChange {
            kind,
            occurrences: vec![ActualChangeOccurrence {
                old_text: old_text.map(str::to_owned),
                new_text: new_text.map(str::to_owned),
                old_comparable_len: old_len,
                new_comparable_len: new_len,
                resolvable: true,
            }],
        }
    }

    fn scoped_expected_change(
        id: &str,
        scope: &str,
        kind: ExpectedKind,
        old: Option<&str>,
        new: Option<&str>,
    ) -> ExpectedChange {
        let mut change = expected_change(id, kind, old, new);
        change.scope = Some(scope.to_owned());
        change
    }

    #[test]
    fn diagnostic_json_uses_exact_stable_tags_without_internal_evidence() {
        let cases = [
            (
                ExpectedChangeFailureReason::QuoteNotExtracted {
                    side: MissSide::Old,
                },
                serde_json::json!({"expected_id":"c","reason":"quote_not_extracted","side":"old"}),
            ),
            (
                ExpectedChangeFailureReason::UnitSegmentationFailure {
                    side: MissSide::Both,
                },
                serde_json::json!({"expected_id":"c","reason":"unit_segmentation_failure","side":"both"}),
            ),
            (
                ExpectedChangeFailureReason::CandidateNotGenerated,
                serde_json::json!({"expected_id":"c","reason":"candidate_not_generated"}),
            ),
            (
                ExpectedChangeFailureReason::CandidateScoringRejected,
                serde_json::json!({"expected_id":"c","reason":"candidate_scoring_rejected"}),
            ),
            (
                ExpectedChangeFailureReason::AlignmentAmbiguous,
                serde_json::json!({"expected_id":"c","reason":"alignment_ambiguous"}),
            ),
            (
                ExpectedChangeFailureReason::ReadingOrderUnresolved {
                    side: MissSide::New,
                },
                serde_json::json!({"expected_id":"c","reason":"reading_order_unresolved","side":"new"}),
            ),
            (
                ExpectedChangeFailureReason::DiffEditDistanceExceeded,
                serde_json::json!({"expected_id":"c","reason":"diff_edit_distance_exceeded"}),
            ),
            (
                ExpectedChangeFailureReason::DiffRejectedAsImplausible,
                serde_json::json!({"expected_id":"c","reason":"diff_rejected_as_implausible"}),
            ),
            (
                ExpectedChangeFailureReason::WrongChangeKind {
                    expected: "replacement".to_owned(),
                    actual: "move".to_owned(),
                },
                serde_json::json!({"expected_id":"c","reason":"wrong_change_kind","expected":"replacement","actual":"move"}),
            ),
            (
                ExpectedChangeFailureReason::FragmentedAcrossHunks {
                    old_hunks: 2,
                    new_hunks: 3,
                },
                serde_json::json!({"expected_id":"c","reason":"fragmented_across_hunks","old_hunks":2,"new_hunks":3}),
            ),
            (
                ExpectedChangeFailureReason::AlignmentOrCandidate {
                    diagnostic_limited: true,
                },
                serde_json::json!({"expected_id":"c","reason":"alignment_or_candidate","diagnostic_limited":true}),
            ),
        ];
        for (reason, expected) in cases {
            let value = serde_json::to_value(ExpectedChangeFailure {
                expected_id: "c".to_owned(),
                reason,
            })
            .expect("failure serializes");
            assert_eq!(value, expected);
            let object = value.as_object().expect("failure object");
            for forbidden in ["quote", "block", "candidate", "old_quote", "new_quote"] {
                assert!(!object.contains_key(forbidden));
            }
        }
    }

    #[test]
    fn summary_distinguishes_unavailable_diagnostics_from_completed_empty_diagnostics() {
        let unavailable = RevisionSummaryReport::from_reports(&[record(PairRunStatus::Ok)]);
        let unavailable = serde_json::to_value(unavailable).expect("summary serializes");
        assert_eq!(
            unavailable["records"][0]["candidate_recall"],
            serde_json::Value::Null
        );
        assert_eq!(
            unavailable["records"][0]["expected_change_diagnostics"],
            serde_json::Value::Null
        );

        let mut report = record(PairRunStatus::Ok);
        report.candidate_recall = Some(CandidateRecallMetrics {
            top_k: 32,
            annotated_counterparts: 0,
            evaluable_counterparts: 0,
            recalled_counterparts: 0,
            unavailable_counterparts: 0,
            recall_at_k: None,
        });
        report.expected_change_diagnostics = Some(ExpectedChangeDiagnostics {
            complete: true,
            failures: Vec::new(),
        });
        let completed = RevisionSummaryReport::from_reports(&[report]);
        let completed = serde_json::to_value(completed).expect("summary serializes");
        assert_eq!(completed["schema_version"], 9);
        assert_eq!(completed["records"][0]["candidate_recall"]["top_k"], 32);
        assert_eq!(
            completed["records"][0]["candidate_recall"]["recall_at_k"],
            serde_json::Value::Null
        );
        assert_eq!(
            completed["records"][0]["expected_change_diagnostics"],
            serde_json::json!({"complete":true,"failures":[]})
        );
    }

    #[test]
    fn scoped_metrics_are_compact_only_and_omitted_when_unavailable() {
        let legacy = record(PairRunStatus::Ok);
        let legacy_full = serde_json::to_value(&legacy).expect("full report serializes");
        assert!(legacy_full.get("scoped_event_metrics").is_none());
        let legacy_summary = serde_json::to_value(RevisionSummaryReport::from_reports(&[legacy]))
            .expect("summary serializes");
        assert_eq!(legacy_summary["schema_version"], 9);
        assert!(
            legacy_summary["records"][0]
                .get("scoped_event_metrics")
                .is_none()
        );

        let mut scoped = record(PairRunStatus::Ok);
        scoped.scoped_event_metrics = Some(ScopedEventMetrics {
            reviewed_scope_count: 2,
            precision: 0.5,
            recall: 1.0,
            f1: 2.0 / 3.0,
        });
        scoped.scoped_token_metrics = Some(ScopedTokenMetrics {
            expected_changed_tokens: 8,
            reported_changed_tokens: 10,
            true_positive_tokens: 6,
            precision: 0.6,
            recall: 0.75,
            f1: 2.0 / 3.0,
            span_iou: 0.5,
            false_positive_tokens_per_10k_unchanged: Some(20.0),
        });
        let scoped_full = serde_json::to_value(&scoped).expect("full report serializes");
        assert!(scoped_full.get("scoped_event_metrics").is_none());
        assert!(scoped_full.get("scoped_token_metrics").is_none());
        let summary = serde_json::to_value(RevisionSummaryReport::from_reports(&[scoped]))
            .expect("summary serializes");
        assert_eq!(
            summary["records"][0]["scoped_event_metrics"]["reviewed_scope_count"],
            2
        );
        assert_eq!(
            summary["records"][0]["scoped_token_metrics"],
            serde_json::json!({
                "expected_changed_tokens": 8,
                "reported_changed_tokens": 10,
                "true_positive_tokens": 6,
                "precision": 0.6,
                "recall": 0.75,
                "f1": 2.0 / 3.0,
                "span_iou": 0.5,
                "false_positive_tokens_per_10k_unchanged": 20.0
            })
        );
    }

    #[test]
    fn matching_is_one_to_one_and_tracks_kind_agreement() {
        let expected = vec![
            expected_change(
                "c1",
                ExpectedKind::Replacement,
                Some("ten days"),
                Some("twenty days"),
            ),
            expected_change("c2", ExpectedKind::Deletion, Some("obsolete section"), None),
        ];
        let actuals = vec![
            actual_change(
                ChangeKind::Replacement,
                Some("within ten days of receipt"),
                Some("within twenty days of receipt"),
                Some(26),
                Some(30),
            ),
            actual_change(
                ChangeKind::Insertion,
                Some("before the obsolete section"),
                Some("before"),
                Some(26),
                Some(7),
            ),
        ];
        let partial = compute_quality(Annotation::Partial, &expected, &actuals);
        assert_eq!(partial.expected_changes, 2);
        assert_eq!(partial.reported_changes, 2);
        assert_eq!(partial.recall, Some(1.0));
        assert_eq!(partial.precision, None);
        assert_eq!(partial.kind_accuracy, Some(0.5));
        assert_eq!(partial.unmatched_tiny_changes, 0);
    }

    #[test]
    fn scoped_event_quality_matches_only_the_same_scope_and_counts_false_positives() {
        let expected = [scoped_expected_change(
            "c1",
            "reviewed",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        )];
        let actuals = [
            actual_change(
                ChangeKind::Replacement,
                Some("old quote"),
                Some("new quote"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Insertion,
                None,
                Some("false positive"),
                None,
                Some(2),
            ),
        ];
        let scopes = ["outside".to_owned(), "reviewed".to_owned()];
        let (quality, metrics) = scoped_quality(1, &expected, &actuals, &scopes);

        assert_eq!(quality.expected_changes, 1);
        assert_eq!(quality.reported_changes, 2);
        assert_eq!(quality.recall, Some(0.0));
        assert_eq!(quality.precision, Some(0.0));
        assert_eq!(metrics.f1, 0.0);

        let scoped_actuals = [actuals[0].clone(), actuals[1].clone()];
        let scoped_ids = ["reviewed".to_owned(), "reviewed".to_owned()];
        let (quality, metrics) = scoped_quality(1, &expected, &scoped_actuals, &scoped_ids);
        assert_eq!(quality.recall, Some(1.0));
        assert_eq!(quality.precision, Some(0.5));
        assert!((metrics.f1 - 2.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn change_free_reviewed_scope_has_vacuous_perfect_event_quality() {
        let (quality, metrics) = scoped_quality(1, &[], &[], &[]);

        assert_eq!(quality.expected_changes, 0);
        assert_eq!(quality.reported_changes, 0);
        assert_eq!(quality.precision, Some(1.0));
        assert_eq!(quality.recall, Some(1.0));
        assert_eq!(metrics.precision, 1.0);
        assert_eq!(metrics.recall, 1.0);
        assert_eq!(metrics.f1, 1.0);
    }

    #[test]
    fn whitespace_collapsing_allows_quotes_across_wrapped_blocks() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Replacement,
            Some("annual fee  of fifty dollars"),
            Some("annual fee of sixty dollars"),
        )];
        let actuals = vec![actual_change(
            ChangeKind::Replacement,
            Some("pay an annual\nfee  of fifty dollars to"),
            Some("pay an annual fee of sixty dollars to"),
            Some(36),
            Some(37),
        )];
        let quality = compute_quality(Annotation::Complete, &expected, &actuals);
        assert_eq!(quality.precision, Some(1.0));
        assert_eq!(quality.review_hunks_per_expected_change, Some(1.0));
    }

    #[test]
    fn quote_matching_considers_every_occurrence_of_one_semantic_change() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Replacement,
            Some("target old text"),
            Some("target new text"),
        )];
        let mut actual = actual_change(
            ChangeKind::Replacement,
            Some("unrelated old text"),
            Some("unrelated new text"),
            Some(18),
            Some(18),
        );
        actual.occurrences.push(ActualChangeOccurrence {
            old_text: Some("prefix target old text suffix".to_owned()),
            new_text: Some("prefix target new text suffix".to_owned()),
            old_comparable_len: Some(29),
            new_comparable_len: Some(29),
            resolvable: true,
        });

        let quality = compute_quality(Annotation::Complete, &expected, &[actual]);

        assert_eq!(quality.recall, Some(1.0));
        assert_eq!(quality.precision, Some(1.0));
        assert_eq!(quality.reported_changes, 1);
        assert_eq!(quality.reported_hunks_per_matched_change, Some(2.0));
        assert_eq!(quality.review_hunks_per_expected_change, Some(2.0));
    }

    #[test]
    fn repeated_occurrences_count_once_semantically_but_all_resolution_failures_count() {
        let mut repeated = actual_change(
            ChangeKind::Replacement,
            Some("a"),
            Some("b"),
            Some(1),
            Some(1),
        );
        repeated.occurrences.push(repeated.occurrences[0].clone());
        let quality = compute_quality(Annotation::Complete, &[], &[repeated]);
        assert_eq!(quality.reported_changes, 1);
        assert_eq!(quality.unmatched_tiny_changes, 1);

        let mut unresolved = actual_change(ChangeKind::Replacement, None, None, None, None);
        unresolved.occurrences[0].resolvable = false;
        unresolved
            .occurrences
            .push(unresolved.occurrences[0].clone());
        let quality = compute_quality(Annotation::Complete, &[], &[unresolved]);
        assert_eq!(quality.reported_changes, 1);
        assert_eq!(quality.unresolvable_reported_spans, 2);
        assert_eq!(quality.unmatched_tiny_changes, 0);
    }

    #[test]
    fn tiny_unmatched_edits_are_counted_as_false_positive_candidates() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Replacement,
            Some("meaningful replaced sentence"),
            Some("other meaningful replaced sentence"),
        )];
        let actuals = vec![
            actual_change(
                ChangeKind::Replacement,
                Some("ab"),
                Some("cd"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Replacement,
                Some("ef"),
                Some("gh"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Replacement,
                Some("meaningful replaced sentence"),
                Some("other meaningful replaced sentence"),
                Some(28),
                Some(34),
            ),
        ];
        let quality = compute_quality(Annotation::Complete, &expected, &actuals);
        assert_eq!(quality.recall, Some(1.0));
        assert_eq!(quality.precision, Some(1.0 / 3.0));
        assert_eq!(quality.unmatched_tiny_changes, 2);
        assert_eq!(quality.reported_hunks_per_matched_change, Some(3.0));
        assert_eq!(quality.review_hunks_per_expected_change, Some(3.0));
    }

    #[test]
    fn long_one_sided_spans_are_not_tiny() {
        let change = actual_change(ChangeKind::Insertion, None, Some("ab"), None, Some(2));
        assert!(is_tiny(&change));
        let change = actual_change(
            ChangeKind::Insertion,
            None,
            Some("much longer inserted content"),
            None,
            Some(28),
        );
        assert!(!is_tiny(&change));
    }

    #[test]
    fn unresolvable_spans_never_count_as_suspicious_tiny_edits() {
        // A change whose spans failed text resolution carries no comparable
        // lengths; previously the 0-initialized maximum made such changes
        // look like two-token edits.
        let mut both_unresolved = actual_change(ChangeKind::Replacement, None, None, None, None);
        both_unresolved.occurrences[0].resolvable = false;
        // A partially resolved change keeps one short length while still
        // being unresolvable overall.
        let mut partially_resolved =
            actual_change(ChangeKind::Replacement, Some("ab"), None, Some(2), None);
        partially_resolved.occurrences[0].resolvable = false;
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Deletion,
            Some("gone text"),
            None,
        )];
        let quality = compute_quality(
            Annotation::Partial,
            &expected,
            &[partially_resolved, both_unresolved],
        );
        assert_eq!(quality.unmatched_tiny_changes, 0);
        assert_eq!(quality.unresolvable_reported_spans, 2);
        assert_eq!(quality.recall, Some(0.0));
    }

    fn record(status: PairRunStatus) -> PairRunReport {
        PairRunReport {
            pair_id: "p".to_owned(),
            set: PairSet::Dev.label(),
            role: PairRole::Standard.label(),
            document_type: "t".to_owned(),
            in_scope: true,
            status,
            provenance_verified: true,
            compared: false,
            extraction_complete: None,
            comparison_complete: None,
            extraction_issues: Vec::new(),
            coverage_old: None,
            coverage_new: None,
            coverage_comparison: None,
            unresolved_regions: None,
            unresolved_old_token_share: None,
            unresolved_new_token_share: None,
            reported_content_changes: None,
            formatting_only_changes: None,
            uncertain_changes: None,
            reported_changes_preview: Vec::new(),
            quality: None,
            quality_skipped_reason: None,
            scoped_event_metrics: None,
            scoped_token_metrics: None,
            candidate_recall: None,
            expected_change_diagnostics: None,
            resource_limit_failure: None,
            candidate_visits: None,
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            max_candidate_visits: None,
            sentence_recovery_metrics: None,
            candidate_visit_pressure: None,
            runtime_ms: 0,
            limit_scale_used: 1.0,
            failure: None,
        }
    }

    #[test]
    fn status_summary_reports_healthy_limit_and_failed_totals_accurately() {
        let reports = vec![
            record(PairRunStatus::Ok),
            record(PairRunStatus::Ok),
            record(PairRunStatus::Limit),
            record(PairRunStatus::Failed),
        ];
        assert!(reports[0].healthy());
        assert!(!reports[2].healthy());
        assert!(!reports[3].healthy());
        assert_eq!(
            summarize_reports(&reports),
            "2/4 revision pairs healthy; 1 stopped at resource limits; 1 failed"
        );
    }

    #[test]
    fn partial_annotations_skip_precision_but_keep_recall() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Insertion,
            None,
            Some("new paragraph"),
        )];
        let actuals = vec![
            actual_change(
                ChangeKind::Insertion,
                None,
                Some("a brand new paragraph"),
                None,
                Some(21),
            ),
            actual_change(
                ChangeKind::Deletion,
                Some("removed words"),
                None,
                Some(13),
                None,
            ),
        ];
        let partial = compute_quality(Annotation::Partial, &expected, &actuals);
        assert_eq!(partial.recall, Some(1.0));
        assert_eq!(partial.precision, None);
        assert_eq!(partial.kind_accuracy, Some(1.0));
        assert_eq!(partial.reported_hunks_per_matched_change, Some(2.0));
        let complete = compute_quality(Annotation::Complete, &expected, &actuals);
        assert_eq!(complete.precision, Some(0.5));
        assert_eq!(complete.review_hunks_per_expected_change, Some(2.0));
    }

    #[test]
    fn unresolvable_spans_are_excluded_from_matching_and_counted() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Deletion,
            Some("gone text"),
            None,
        )];
        let mut unresolvable =
            actual_change(ChangeKind::Deletion, Some("gone text"), None, Some(9), None);
        unresolvable.occurrences[0].resolvable = false;
        let quality = compute_quality(
            Annotation::Partial,
            &expected,
            std::slice::from_ref(&unresolvable),
        );
        assert_eq!(quality.recall, Some(0.0));
        assert_eq!(quality.unresolvable_reported_spans, 1);
    }

    #[test]
    fn limit_scale_validation_rejects_lowering_and_nonfinite_values() {
        assert_eq!(validate_limit_scale(1.0).unwrap_or_default(), 1.0);
        assert_eq!(validate_limit_scale(16.0).unwrap_or_default(), 16.0);
        assert!(validate_limit_scale(0.999).is_err());
        assert!(validate_limit_scale(f64::NAN).is_err());
        assert!(validate_limit_scale(f64::INFINITY).is_err());
    }

    #[test]
    fn scaled_options_only_raise_budgets() {
        let baseline = PipelineOptions::default();
        let scaled = baseline
            .scaled_limits(4.0)
            .expect("a finite scale above one should be valid");
        assert!(scaled.max_ngram_token_elements >= baseline.max_ngram_token_elements);
        assert!(scaled.alignment.max_candidate_visits >= baseline.alignment.max_candidate_visits);
        assert!(scaled.alignment.max_dp_cells >= baseline.alignment.max_dp_cells);
        assert!(scaled.diff.max_tokens >= baseline.diff.max_tokens);
        assert!(scaled.diff.max_edit_distance >= baseline.diff.max_edit_distance);
        assert_eq!(
            baseline
                .scaled_limits(1.0)
                .expect("a scale of one should be valid")
                .alignment
                .max_candidate_visits,
            baseline.alignment.max_candidate_visits
        );
    }

    #[test]
    fn collapse_whitespace_normalizes_line_breaks_and_runs() {
        assert_eq!(collapse_whitespace("a\n b   c\t d"), "a b c d");
        assert_eq!(collapse_whitespace("   "), "");
    }

    fn glyph_document(text: &str) -> Document<Glyph> {
        let glyphs = text
            .chars()
            .enumerate()
            .filter(|(_, character)| *character != ' ')
            .map(|(index, character)| Glyph {
                id: GlyphId(index as u64 + 1),
                text: DecodedText::Mapped(character.to_string()),
                raw_code: character.to_string().into_bytes(),
                page: PageId(0),
                bbox: Rect {
                    min: Vec2 {
                        x: index as f64 * 6.0,
                        y: 100.0,
                    },
                    max: Vec2 {
                        x: index as f64 * 6.0 + 5.0,
                        y: 110.0,
                    },
                },
                baseline: Vec2 {
                    x: index as f64 * 6.0,
                    y: 100.0,
                },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(1),
                font_size: 10.0,
                render_order: u32::try_from(index).expect("fixture glyph index fits in u32"),
                render_mode: TextRenderMode::Fill,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: 1,
                        generation: 0,
                    },
                    operator_index: u32::try_from(index).expect("fixture glyph index fits in u32"),
                },
            })
            .collect();
        Document::new(glyphs)
    }

    #[test]
    fn compare_outcomes_with_metrics_records_candidate_visits_on_success() {
        let old =
            ExtractionOutcome::complete(glyph_document("Stable old paragraph remains visible"));
        let new =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));

        let ComparisonWithMetrics {
            outcome,
            alignment,
            metrics,
            sentence_recovery_metrics,
            pressure,
        } = compare_outcomes_with_metrics(old, new, PipelineOptions::default())
            .expect("comparison succeeds");

        assert!(!outcome.comparison.changes.is_empty());
        assert!(alignment.is_some());
        let sentence_recovery_metrics = sentence_recovery_metrics
            .expect("completed exact diff records sentence recovery metrics");
        assert!(sentence_recovery_metrics.old_trusted_run_source_tokens > 0);
        assert!(sentence_recovery_metrics.new_trusted_run_source_tokens > 0);
        let visits = metrics.candidate_visits.expect("candidate visits recorded");
        assert!(visits > 0, "non-anchor old blocks must be charged");
        assert_eq!(
            metrics.candidate_visits_required,
            Some(visits),
            "attempted charge must equal the required sum on success"
        );
        let (exact, ngram, short_fallback) = (
            metrics
                .candidate_visits_required_exact
                .expect("inverted index reports a breakdown"),
            metrics
                .candidate_visits_required_ngram
                .expect("inverted index reports a breakdown"),
            metrics
                .candidate_visits_required_short_fallback
                .expect("inverted index reports a breakdown"),
        );
        assert_eq!(
            exact + ngram + short_fallback,
            visits,
            "required components must sum to the required total"
        );
        assert_eq!(
            metrics.max_candidate_visits,
            Some(AlignmentOptions::default().max_candidate_visits)
        );
        let pressure = pressure.expect("pressure recorded for complete extraction");
        assert_eq!(
            pressure.max_candidate_visits,
            AlignmentOptions::default().max_candidate_visits
        );
    }

    #[test]
    fn compare_outcomes_with_metrics_preserves_present_all_zero_sentence_metrics() {
        let old = ExtractionOutcome::complete(Document::<Glyph>::new(Vec::new()));
        let new = ExtractionOutcome::complete(Document::<Glyph>::new(Vec::new()));

        let ComparisonWithMetrics {
            sentence_recovery_metrics,
            ..
        } = compare_outcomes_with_metrics(old, new, PipelineOptions::default())
            .expect("empty comparison succeeds");

        assert_eq!(
            sentence_recovery_metrics,
            Some(SentenceRecoveryMetricsReport {
                structural_pairing_available: true,
                near_relation_complete: true,
                ..SentenceRecoveryMetricsReport::default()
            })
        );
    }

    #[test]
    fn compare_outcomes_with_metrics_keeps_attempted_charge_on_candidate_limit() {
        let old =
            ExtractionOutcome::complete(glyph_document("Stable old paragraph remains visible"));
        let new =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));

        let ComparisonWithMetrics {
            outcome: _,
            alignment: _,
            metrics,
            sentence_recovery_metrics: _,
            pressure: _,
        } = compare_outcomes_with_metrics(old.clone(), new.clone(), PipelineOptions::default())
            .expect("baseline comparison succeeds");
        let charge = metrics.candidate_visits.expect("baseline charge recorded");
        assert!(charge > 1, "fixture must charge at least two visits");

        let options = PipelineOptions {
            alignment: AlignmentOptions {
                max_candidate_visits: charge - 1,
                ..AlignmentOptions::default()
            },
            ..PipelineOptions::default()
        };
        let error = compare_outcomes_with_metrics(old, new, options)
            .expect_err("candidate limit must fail");
        match error {
            RevisionRunError::Limit {
                message,
                metrics,
                pressure,
            } => {
                assert!(message.contains("alignment candidate visits"));
                assert_eq!(metrics.candidate_visits, Some(charge));
                assert_eq!(
                    metrics.candidate_visits_required,
                    Some(charge),
                    "the full required sum completes when no later estimate errors"
                );
                let (exact, ngram, short_fallback) = (
                    metrics
                        .candidate_visits_required_exact
                        .expect("inverted index reports a breakdown"),
                    metrics
                        .candidate_visits_required_ngram
                        .expect("inverted index reports a breakdown"),
                    metrics
                        .candidate_visits_required_short_fallback
                        .expect("inverted index reports a breakdown"),
                );
                assert_eq!(
                    exact + ngram + short_fallback,
                    charge,
                    "required components must sum to the required total"
                );
                assert_eq!(metrics.max_candidate_visits, Some(charge - 1));
                let pressure = pressure.expect("pressure recorded for complete extraction");
                assert_eq!(pressure.max_candidate_visits, charge - 1);
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    #[test]
    fn compare_outcomes_with_metrics_returns_none_for_pre_alignment_limit() {
        let old =
            ExtractionOutcome::complete(glyph_document("Stable old paragraph remains visible"));
        let new =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));
        let options = PipelineOptions {
            max_ngram_token_elements: 1,
            ..PipelineOptions::default()
        };

        let error = compare_outcomes_with_metrics(old, new, options)
            .expect_err("ngram budget must fail before alignment");
        match error {
            RevisionRunError::Limit {
                metrics, pressure, ..
            } => {
                assert_eq!(metrics.candidate_visits, None);
                assert_eq!(metrics.candidate_visits_required, None);
                assert_eq!(metrics.candidate_visits_required_exact, None);
                assert_eq!(metrics.candidate_visits_required_ngram, None);
                assert_eq!(metrics.candidate_visits_required_short_fallback, None);
                assert_eq!(metrics.max_candidate_visits, None);
                assert_eq!(pressure, None);
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    #[test]
    fn compare_outcomes_with_metrics_skips_pressure_for_incomplete_extraction() {
        let incomplete = ExtractionOutcome::new(
            glyph_document("Stable old paragraph remains visible"),
            vec![
                ExtractionIssue::new(
                    ExtractionIssueKind::Unresolved,
                    ExtractionScope::Document,
                    "document evidence is incomplete",
                )
                .expect("valid issue"),
            ],
        )
        .expect("valid outcome");
        let complete =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));

        let ComparisonWithMetrics {
            outcome: _,
            alignment,
            metrics,
            sentence_recovery_metrics,
            pressure,
        } = compare_outcomes_with_metrics(incomplete, complete, PipelineOptions::default())
            .expect("incomplete comparison is not an error");

        assert_eq!(metrics.candidate_visits, None);
        assert_eq!(metrics.candidate_visits_required, None);
        assert_eq!(metrics.candidate_visits_required_exact, None);
        assert_eq!(metrics.candidate_visits_required_ngram, None);
        assert_eq!(metrics.candidate_visits_required_short_fallback, None);
        assert_eq!(sentence_recovery_metrics, None);
        assert_eq!(pressure, None);
        assert!(alignment.is_none());
    }

    #[test]
    fn pair_report_json_includes_candidate_visit_fields() {
        let mut report = record(PairRunStatus::Ok);
        report.candidate_visits = Some(42);
        report.candidate_visits_required = Some(84);
        report.candidate_visits_required_exact = Some(20);
        report.candidate_visits_required_ngram = Some(40);
        report.candidate_visits_required_short_fallback = Some(24);
        report.max_candidate_visits = Some(1_000_000);
        report.sentence_recovery_metrics = Some(SentenceRecoveryMetricsReport {
            old_trusted_run_source_tokens: 42,
            near_pair_candidates: 3,
            vetoed_near_pairs: 2,
            recovered_deletion_tokens: 18,
            unresolved_remainder_old_source_tokens: 24,
            ..SentenceRecoveryMetricsReport::default()
        });
        report.candidate_visit_pressure = Some(CandidateVisitPressure {
            estimated_visits_p50: 1,
            estimated_visits_p95: 2,
            estimated_visits_max: 3,
            estimated_visits_upper_bound_total: 9,
            max_candidate_visits: 8,
            estimated_visits_upper_bound_exceeds_limit: true,
            ngram_posting_visits_total: 9,
            dominant_ngram_visits: 9,
            dominant_ngram_df: 3,
            shared_ngram_count: 1,
            top_10_ngram_visits: 9,
            ngrams_for_50_percent_visits: 1,
            ngrams_for_90_percent_visits: 1,
            shared_ngram_df_p50: 3,
            shared_ngram_df_p95: 3,
            shared_ngram_df_max: 3,
        });

        let json = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(json["candidate_visits"], 42);
        assert_eq!(json["candidate_visits_required"], 84);
        assert_eq!(json["candidate_visits_required_exact"], 20);
        assert_eq!(json["candidate_visits_required_ngram"], 40);
        assert_eq!(json["candidate_visits_required_short_fallback"], 24);
        assert_eq!(json["max_candidate_visits"], 1_000_000);
        assert!(json.get("sentence_recovery_metrics").is_none());
        assert_eq!(
            json["candidate_visit_pressure"]["estimated_visits_upper_bound_total"],
            9
        );
        assert_eq!(json["candidate_visit_pressure"]["dominant_ngram_df"], 3);
        assert_eq!(json["candidate_visit_pressure"]["shared_ngram_count"], 1);
        assert_eq!(
            json["candidate_visit_pressure"]["ngrams_for_90_percent_visits"],
            1
        );
    }

    #[test]
    fn summary_json_uses_null_for_unavailable_sentence_metrics() {
        let summary = RevisionSummaryReport::from_reports(&[record(PairRunStatus::Ok)]);
        let json = serde_json::to_value(summary).expect("summary serializes");

        assert_eq!(json["schema_version"], 9);
        assert_eq!(
            json["records"][0]["sentence_recovery_metrics"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn validates_zero_and_populated_sentence_recovery_metrics() {
        assert_eq!(
            validate_sentence_recovery_metrics(SentenceRecoveryMetrics::default()),
            Ok(SentenceRecoveryMetricsReport::default())
        );

        let populated = SentenceRecoveryMetrics {
            old_trusted_run_source_tokens: 30,
            new_trusted_run_source_tokens: 40,
            structural_pairing_available: true,
            old_structural_descriptors: 4,
            new_structural_descriptors: 3,
            old_structural_eligible_descriptors: 2,
            new_structural_eligible_descriptors: 2,
            old_structural_mixed_descriptors: 1,
            new_structural_mixed_descriptors: 1,
            old_structural_split_descriptors: 1,
            structural_shared_profiles: 1,
            structural_candidate_pairs: 2,
            structural_largest_posting: 2,
            structural_duplicate_pairs: 1,
            structural_unique_reciprocal_pairs: 1,
            structural_unique_monotone_anchor_pairs: 1,
            near_relation_complete: true,
            near_pair_visits_examined: 20,
            near_pair_visits_attempted: 20,
            near_similarity_comparisons_examined: 8,
            near_similarity_comparisons_attempted: 8,
            near_largest_edge_posting: 12,
            near_largest_edge_query_union: 9,
            near_largest_filtered_candidate_set: 4,
            near_pair_candidates: 2,
            vetoed_near_pairs: 1,
            recovered_replacement_old_tokens: 10,
            recovered_replacement_new_tokens: 12,
            recovered_deletion_tokens: 5,
            recovered_insertion_tokens: 6,
            unresolved_remainder_old_source_tokens: 15,
            unresolved_remainder_new_source_tokens: 22,
            ..SentenceRecoveryMetrics::default()
        };
        let validated = validate_sentence_recovery_metrics(populated)
            .expect("populated metrics satisfy the contract");
        assert_eq!(validated.old_trusted_run_source_tokens, 30);
        assert!(validated.structural_pairing_available);
        assert_eq!(validated.structural_candidate_pairs, 2);
        assert_eq!(validated.structural_unique_monotone_anchor_pairs, 1);
        assert!(validated.near_relation_complete);
        assert_eq!(validated.near_pair_visits_examined, 20);
        assert_eq!(validated.near_similarity_comparisons_attempted, 8);
        assert_eq!(validated.near_largest_edge_posting, 12);
        assert_eq!(validated.recovered_replacement_new_tokens, 12);
        assert_eq!(validated.vetoed_near_pairs, 1);
    }

    #[test]
    fn rejects_invalid_sentence_recovery_metrics() {
        let unavailable_structural = SentenceRecoveryMetrics {
            structural_candidate_pairs: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(unavailable_structural).is_err());

        let invalid_structural_partition = SentenceRecoveryMetrics {
            structural_pairing_available: true,
            old_structural_descriptors: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_structural_partition).is_err());

        let invalid_anchor_partition = SentenceRecoveryMetrics {
            structural_pairing_available: true,
            structural_shared_profiles: 1,
            structural_candidate_pairs: 1,
            structural_largest_posting: 1,
            structural_unique_reciprocal_pairs: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_anchor_partition).is_err());

        let invalid_veto = SentenceRecoveryMetrics {
            near_pair_candidates: 1,
            vetoed_near_pairs: 2,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_veto).is_err());

        let invalid_recovered_total = SentenceRecoveryMetrics {
            recovered_exact_match_old_tokens: usize::MAX,
            unresolved_remainder_old_source_tokens: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_recovered_total).is_err());

        let examined_more_pairs_than_attempted = SentenceRecoveryMetrics {
            near_pair_visits_examined: 2,
            near_pair_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(examined_more_pairs_than_attempted).is_err());

        let examined_more_comparisons_than_attempted = SentenceRecoveryMetrics {
            near_similarity_comparisons_examined: 2,
            near_similarity_comparisons_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(
            validate_sentence_recovery_metrics(examined_more_comparisons_than_attempted).is_err()
        );

        let pair_deficit_without_reason = SentenceRecoveryMetrics {
            near_pair_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(pair_deficit_without_reason).is_err());

        let comparison_deficit_without_reason = SentenceRecoveryMetrics {
            near_similarity_comparisons_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(comparison_deficit_without_reason).is_err());

        let multiple_deficits = SentenceRecoveryMetrics {
            near_pair_visits_attempted: 1,
            near_similarity_comparisons_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(multiple_deficits).is_err());

        let mismatched_pair_reason = SentenceRecoveryMetrics {
            near_similarity_comparisons_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(mismatched_pair_reason).is_err());

        let mismatched_comparison_reason = SentenceRecoveryMetrics {
            near_pair_visits_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(mismatched_comparison_reason).is_err());

        let incomplete_pair_visit_stop = SentenceRecoveryMetrics {
            near_pair_visits_examined: 4,
            near_pair_visits_attempted: 5,
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        let validated = validate_sentence_recovery_metrics(incomplete_pair_visit_stop)
            .expect("pair visit limit explains unexamined pair work");
        assert_eq!(
            validated.near_relation_stop_reason,
            Some(NearRelationStopReasonReport::PairVisitLimit)
        );

        let incomplete_similarity_stop = SentenceRecoveryMetrics {
            near_similarity_comparisons_examined: 6,
            near_similarity_comparisons_attempted: 7,
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            ..SentenceRecoveryMetrics::default()
        };
        let validated = validate_sentence_recovery_metrics(incomplete_similarity_stop)
            .expect("similarity limit explains an unexamined comparison");
        assert_eq!(
            validated.near_relation_stop_reason,
            Some(NearRelationStopReasonReport::SimilarityComparisonLimit)
        );

        let candidate_count_stop = SentenceRecoveryMetrics {
            near_pair_visits_examined: 4,
            near_pair_visits_attempted: 4,
            near_similarity_comparisons_examined: 2,
            near_similarity_comparisons_attempted: 2,
            near_candidate_count_truncated: true,
            near_relation_stop_reason: Some(NearRelationStopReason::CandidateCountLimit),
            ..SentenceRecoveryMetrics::default()
        };
        let validated = validate_sentence_recovery_metrics(candidate_count_stop)
            .expect("candidate count truncation stops before search work is charged");
        assert_eq!(
            validated.near_relation_stop_reason,
            Some(NearRelationStopReasonReport::CandidateCountLimit)
        );
        assert!(validated.near_candidate_count_truncated);

        let truncated_then_pair_stop = SentenceRecoveryMetrics {
            near_candidate_count_truncated: true,
            near_pair_visits_examined: 4,
            near_pair_visits_attempted: 5,
            near_similarity_comparisons_examined: 2,
            near_similarity_comparisons_attempted: 2,
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(truncated_then_pair_stop).is_ok());

        let truncated_then_comparison_stop = SentenceRecoveryMetrics {
            near_candidate_count_truncated: true,
            near_pair_visits_examined: 4,
            near_pair_visits_attempted: 4,
            near_similarity_comparisons_examined: 2,
            near_similarity_comparisons_attempted: 3,
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(truncated_then_comparison_stop).is_ok());

        let candidate_count_stop_without_truncation = SentenceRecoveryMetrics {
            near_relation_stop_reason: Some(NearRelationStopReason::CandidateCountLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(
            validate_sentence_recovery_metrics(candidate_count_stop_without_truncation).is_err()
        );

        let truncation_without_reason = SentenceRecoveryMetrics {
            near_candidate_count_truncated: true,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(truncation_without_reason).is_err());

        let complete_with_truncation = SentenceRecoveryMetrics {
            near_relation_complete: true,
            near_candidate_count_truncated: true,
            near_relation_stop_reason: Some(NearRelationStopReason::CandidateCountLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(complete_with_truncation).is_err());

        let candidate_count_stop_with_pair_deficit = SentenceRecoveryMetrics {
            near_candidate_count_truncated: true,
            near_pair_visits_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::CandidateCountLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(
            validate_sentence_recovery_metrics(candidate_count_stop_with_pair_deficit).is_err()
        );

        let complete_with_unexamined_work = SentenceRecoveryMetrics {
            near_relation_complete: true,
            near_pair_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(complete_with_unexamined_work).is_err());

        let complete_with_stop_reason = SentenceRecoveryMetrics {
            near_relation_complete: true,
            near_pair_visits_examined: 1,
            near_pair_visits_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(complete_with_stop_reason).is_err());

        let stop_without_pair_deficit = SentenceRecoveryMetrics {
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(stop_without_pair_deficit).is_err());

        let stop_without_comparison_deficit = SentenceRecoveryMetrics {
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(stop_without_comparison_deficit).is_err());
    }

    #[test]
    fn serializes_near_relation_stop_reasons_as_snake_case() {
        assert_eq!(
            serde_json::to_value(NearRelationStopReasonReport::PairVisitLimit)
                .expect("stop reason serializes"),
            "pair_visit_limit"
        );
        assert_eq!(
            serde_json::to_value(NearRelationStopReasonReport::SimilarityComparisonLimit)
                .expect("stop reason serializes"),
            "similarity_comparison_limit"
        );
        assert_eq!(
            serde_json::to_value(NearRelationStopReasonReport::CandidateCountLimit)
                .expect("stop reason serializes"),
            "candidate_count_limit"
        );
    }

    #[test]
    fn validate_visit_metrics_accepts_complete_absent_and_unavailable_required() {
        let complete = VisitMetrics {
            candidate_visits: Some(42),
            candidate_visits_required: Some(84),
            candidate_visits_required_exact: Some(20),
            candidate_visits_required_ngram: Some(40),
            candidate_visits_required_short_fallback: Some(24),
            max_candidate_visits: Some(1_000_000),
        };
        assert_eq!(validate_visit_metrics(complete), Ok(complete));
        let generic = VisitMetrics {
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            ..complete
        };
        assert_eq!(validate_visit_metrics(generic), Ok(generic));
        let unavailable = VisitMetrics {
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            ..complete
        };
        assert_eq!(validate_visit_metrics(unavailable), Ok(unavailable));
        assert_eq!(
            validate_visit_metrics(VisitMetrics::default()),
            Ok(VisitMetrics::default())
        );
    }

    #[test]
    fn validate_visit_metrics_rejects_every_partial_direction() {
        for metrics in [
            VisitMetrics {
                candidate_visits: Some(42),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits_required: Some(84),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                max_candidate_visits: Some(1_000_000),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits: Some(42),
                candidate_visits_required: Some(84),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits_required: Some(84),
                max_candidate_visits: Some(1_000_000),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits: Some(42),
                candidate_visits_required: Some(84),
                candidate_visits_required_exact: Some(20),
                candidate_visits_required_ngram: Some(40),
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: Some(1_000_000),
            },
            VisitMetrics {
                candidate_visits: Some(42),
                candidate_visits_required: None,
                candidate_visits_required_exact: Some(20),
                candidate_visits_required_ngram: Some(40),
                candidate_visits_required_short_fallback: Some(24),
                max_candidate_visits: Some(1_000_000),
            },
        ] {
            let error = validate_visit_metrics(metrics)
                .expect_err("partial metrics must be a contract violation");
            assert!(error.contains("alignment metrics contract violation"));
        }
    }

    #[test]
    fn validate_visit_metrics_rejects_component_sum_mismatch() {
        let metrics = VisitMetrics {
            candidate_visits: Some(42),
            candidate_visits_required: Some(84),
            candidate_visits_required_exact: Some(20),
            candidate_visits_required_ngram: Some(40),
            candidate_visits_required_short_fallback: Some(23),
            max_candidate_visits: Some(1_000_000),
        };
        let error = validate_visit_metrics(metrics)
            .expect_err("component sum mismatch must be a contract violation");
        assert!(error.contains("do not sum to"));
    }

    #[test]
    fn resolve_pressure_rejects_alignment_without_a_pressure_attempt() {
        let error = resolve_pressure(Some(42), true, None)
            .expect_err("alignment reached without a pressure attempt must fail");
        match error {
            RevisionRunError::Other(stage, message) => {
                assert_eq!(stage, "candidate visit pressure contract violation");
                assert!(message.contains("without a pressure attempt"));
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn resolve_pressure_allows_incomplete_extraction_without_an_attempt() {
        assert!(
            resolve_pressure(Some(42), false, None)
                .expect("incomplete extraction does not require pressure")
                .is_none()
        );
    }

    #[test]
    fn publish_new_file_refuses_to_overwrite_existing_file_and_handles_temp_collisions_deterministically()
     {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-publish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(&base_path).expect("create test dir");

        let dest = base_path.join("output.json");
        let bytes = b"hello\n";

        // Pre-create the exact first candidate temp file for a local counter
        let local_counter = AtomicU64::new(42);
        let pid = std::process::id();
        let collision_temp = base_path.join(format!(".pdfbench-artifact-{pid}-42"));
        fs::write(&collision_temp, b"pre-existing foreign temp file")
            .expect("write collision temp");

        // Success on new file despite exact pre-existing temp file
        publish_new_file_with_counter(&dest, bytes, &local_counter).expect("publish succeeds");
        assert_eq!(fs::read(&dest).expect("read dest"), bytes);

        // Pre-existing collision temp file was not modified or deleted
        assert_eq!(
            fs::read(&collision_temp).expect("read collision temp"),
            b"pre-existing foreign temp file"
        );
        let _ = fs::remove_file(&collision_temp);

        // Owned temp file (.pdfbench-artifact-{pid}-43) was cleaned up after publish
        let entries = fs::read_dir(&base_path)
            .expect("read dir")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec!["output.json"]);

        // Refuses to overwrite existing file
        let error = publish_new_file(&dest, b"overwrite\n").expect_err("must refuse overwrite");
        assert!(
            error
                .to_string()
                .contains("destination path already exists")
        );
        assert_eq!(fs::read(&dest).expect("dest unchanged"), bytes);

        // Refuses non-existent parent directory
        let invalid_parent = base_path.join("nonexistent").join("output.json");
        let error = publish_new_file(&invalid_parent, bytes).expect_err("must fail missing dir");
        assert!(
            error
                .to_string()
                .contains("destination directory does not exist")
        );

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn publish_new_file_handles_long_destination_names_without_name_max_overflow() {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-long-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(&base_path).expect("create test dir");

        // Long destination name (240 chars)
        let long_name = format!("{}.json", "a".repeat(235));
        let dest = base_path.join(long_name);
        let bytes = b"long destination test bytes\n";

        publish_new_file(&dest, bytes).expect("publish with long name succeeds");
        assert_eq!(fs::read(&dest).expect("read long dest"), bytes);

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn publish_new_file_exhaustion_reports_truthful_publication_error() {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-exhaust-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(&base_path).expect("create test dir");

        let pid = std::process::id();
        let local_counter = AtomicU64::new(1000);

        // Pre-create all 64 candidate temp files
        for i in 0..MAX_TEMP_CREATE_ATTEMPTS {
            let seq = 1000 + i as u64;
            let path = base_path.join(format!(".pdfbench-artifact-{pid}-{seq}"));
            fs::write(&path, b"existing").expect("write temp");
        }

        let dest = base_path.join("output.json");
        let error = publish_new_file_with_counter(&dest, b"bytes", &local_counter)
            .expect_err("must exhaust attempts");
        assert!(error.to_string().contains("exhausted 64 attempts"));
        assert!(!dest.exists(), "destination must not be created");

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn normalize_output_destination_resolves_aliases_and_rejects_missing_parents() {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-norm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(base_path.join("real_dir")).expect("create real dir");

        let file_direct = base_path.join("real_dir").join("report.json");
        let file_relative = base_path.join("real_dir").join(".").join("report.json");
        let norm_direct = normalize_output_destination(&file_direct).expect("normalize direct");
        let norm_relative =
            normalize_output_destination(&file_relative).expect("normalize relative");
        assert_eq!(norm_direct, norm_relative);

        // Symlink parent alias if symlink creation succeeds
        let symlink_dir = base_path.join("symlink_dir");
        #[cfg(unix)]
        if std::os::unix::fs::symlink(base_path.join("real_dir"), &symlink_dir).is_ok() {
            let file_symlink = symlink_dir.join("report.json");
            let norm_symlink =
                normalize_output_destination(&file_symlink).expect("normalize symlink");
            assert_eq!(norm_direct, norm_symlink);
        }

        // Missing parent directory is rejected
        let missing = base_path.join("missing_parent").join("report.json");
        let error = normalize_output_destination(&missing).expect_err("must reject missing parent");
        assert!(
            error
                .to_string()
                .contains("destination directory does not exist")
        );

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn summary_json_exact_key_set_and_nested_schema_equality() {
        let reports = vec![
            PairRunReport {
                pair_id: "ok-pair".to_owned(),
                set: "dev",
                role: "standard",
                document_type: "single-column".to_owned(),
                in_scope: true,
                status: PairRunStatus::Ok,
                provenance_verified: true,
                compared: true,
                extraction_complete: Some(true),
                comparison_complete: Some(false),
                extraction_issues: Vec::new(),
                coverage_old: Some(0.5),
                coverage_new: Some(0.6),
                coverage_comparison: Some(0.5),
                unresolved_regions: Some(0),
                unresolved_old_token_share: Some(0.0),
                unresolved_new_token_share: Some(0.0),
                reported_content_changes: Some(3),
                formatting_only_changes: Some(0),
                uncertain_changes: Some(0),
                reported_changes_preview: Vec::new(),
                quality: Some(QualityMetrics {
                    annotation: Annotation::Partial,
                    expected_changes: 2,
                    reported_changes: 3,
                    recall: Some(1.0),
                    precision: None,
                    kind_accuracy: Some(1.0),
                    reported_hunks_per_matched_change: Some(1.5),
                    review_hunks_per_expected_change: None,
                    unmatched_tiny_changes: 0,
                    unresolvable_reported_spans: 0,
                }),
                quality_skipped_reason: None,
                scoped_event_metrics: None,
                scoped_token_metrics: None,
                candidate_recall: None,
                expected_change_diagnostics: None,
                resource_limit_failure: None,
                candidate_visits: None,
                candidate_visits_required: None,
                candidate_visits_required_exact: None,
                candidate_visits_required_ngram: None,
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: None,
                sentence_recovery_metrics: Some(SentenceRecoveryMetricsReport {
                    old_trusted_run_source_tokens: 42,
                    near_relation_complete: true,
                    near_pair_candidates: 3,
                    vetoed_near_pairs: 2,
                    recovered_deletion_tokens: 18,
                    unresolved_remainder_old_source_tokens: 24,
                    ..SentenceRecoveryMetricsReport::default()
                }),
                candidate_visit_pressure: None,
                runtime_ms: 50,
                limit_scale_used: 1.0,
                failure: None,
            },
            PairRunReport {
                pair_id: "limit-pair".to_owned(),
                set: "dev",
                role: "standard",
                document_type: "single-column".to_owned(),
                in_scope: true,
                status: PairRunStatus::Limit,
                provenance_verified: true,
                compared: true,
                extraction_complete: Some(true),
                comparison_complete: None,
                extraction_issues: Vec::new(),
                coverage_old: None,
                coverage_new: None,
                coverage_comparison: None,
                unresolved_regions: None,
                unresolved_old_token_share: None,
                unresolved_new_token_share: None,
                reported_content_changes: None,
                formatting_only_changes: None,
                uncertain_changes: None,
                reported_changes_preview: Vec::new(),
                quality: None,
                quality_skipped_reason: Some(QUALITY_SKIP_RESOURCE_LIMIT.to_owned()),
                scoped_event_metrics: None,
                scoped_token_metrics: None,
                candidate_recall: None,
                expected_change_diagnostics: None,
                resource_limit_failure: Some(
                    "alignment candidate visit budget exceeded".to_owned(),
                ),
                candidate_visits: None,
                candidate_visits_required: None,
                candidate_visits_required_exact: None,
                candidate_visits_required_ngram: None,
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: None,
                sentence_recovery_metrics: None,
                candidate_visit_pressure: None,
                runtime_ms: 100,
                limit_scale_used: 1.0,
                failure: None,
            },
            PairRunReport {
                pair_id: "fail-pair".to_owned(),
                set: "holdout",
                role: "standard",
                document_type: "single-column".to_owned(),
                in_scope: true,
                status: PairRunStatus::Failed,
                provenance_verified: true,
                compared: true,
                extraction_complete: Some(false),
                comparison_complete: None,
                extraction_issues: vec![IssueLine {
                    side: "old",
                    kind: "unsupported",
                    scope: "document".to_owned(),
                    description: "unsupported content stream operator".to_owned(),
                }],
                coverage_old: None,
                coverage_new: None,
                coverage_comparison: None,
                unresolved_regions: None,
                unresolved_old_token_share: None,
                unresolved_new_token_share: None,
                reported_content_changes: None,
                formatting_only_changes: None,
                uncertain_changes: None,
                reported_changes_preview: Vec::new(),
                quality: None,
                quality_skipped_reason: Some(QUALITY_SKIP_INCOMPLETE_EXTRACTION.to_owned()),
                scoped_event_metrics: None,
                scoped_token_metrics: None,
                candidate_recall: None,
                expected_change_diagnostics: None,
                resource_limit_failure: None,
                candidate_visits: None,
                candidate_visits_required: None,
                candidate_visits_required_exact: None,
                candidate_visits_required_ngram: None,
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: None,
                sentence_recovery_metrics: None,
                candidate_visit_pressure: None,
                runtime_ms: 20,
                limit_scale_used: 1.0,
                failure: Some("/path/to/doc.pdf: extraction failed".to_owned()),
            },
        ];

        let summary = RevisionSummaryReport::from_reports(&reports);
        let value = serde_json::to_value(&summary).expect("serialize value");

        // Top-level schema key-set equality
        let top_keys = value
            .as_object()
            .expect("top object")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let expected_top_keys = HashSet::from(["schema_version".to_owned(), "records".to_owned()]);
        assert_eq!(top_keys, expected_top_keys);
        assert_eq!(value["schema_version"], 9);

        let records = value["records"].as_array().expect("records array");
        assert_eq!(records.len(), 3);

        // Record key-set equality
        let expected_record_keys = HashSet::from([
            "pair_id".to_owned(),
            "set".to_owned(),
            "role".to_owned(),
            "in_scope".to_owned(),
            "status".to_owned(),
            "provenance_verified".to_owned(),
            "compared".to_owned(),
            "extraction_complete".to_owned(),
            "comparison_complete".to_owned(),
            "limit_scale_used".to_owned(),
            "resource_limit_failure".to_owned(),
            "coverage_old".to_owned(),
            "coverage_new".to_owned(),
            "coverage_comparison".to_owned(),
            "unresolved_regions".to_owned(),
            "unresolved_old_token_share".to_owned(),
            "unresolved_new_token_share".to_owned(),
            "reported_content_changes".to_owned(),
            "reported_formatting_changes".to_owned(),
            "reported_uncertain_changes".to_owned(),
            "sentence_recovery_metrics".to_owned(),
            "quality".to_owned(),
            "quality_skipped_reason".to_owned(),
            "candidate_recall".to_owned(),
            "expected_change_diagnostics".to_owned(),
        ]);

        for rec in records {
            let keys = rec
                .as_object()
                .expect("record object")
                .keys()
                .cloned()
                .collect::<HashSet<_>>();
            assert_eq!(keys, expected_record_keys);
            assert!(!keys.contains("failure"), "failure must be excluded");
            assert!(!keys.contains("runtime_ms"), "runtime_ms must be excluded");
        }

        // Quality key-set equality
        let expected_quality_keys = HashSet::from([
            "annotation".to_owned(),
            "expected_changes".to_owned(),
            "reported_changes".to_owned(),
            "recall".to_owned(),
            "precision".to_owned(),
            "kind_accuracy".to_owned(),
            "reported_hunks_per_matched_change".to_owned(),
            "review_hunks_per_expected_change".to_owned(),
            "unmatched_tiny_changes".to_owned(),
            "unresolvable_reported_spans".to_owned(),
        ]);
        let quality_keys = records[0]["quality"]
            .as_object()
            .expect("quality object")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        assert_eq!(quality_keys, expected_quality_keys);

        let expected_sentence_recovery_keys = HashSet::from([
            "old_trusted_run_source_tokens".to_owned(),
            "new_trusted_run_source_tokens".to_owned(),
            "structural_pairing_available".to_owned(),
            "old_structural_descriptors".to_owned(),
            "new_structural_descriptors".to_owned(),
            "old_structural_eligible_descriptors".to_owned(),
            "new_structural_eligible_descriptors".to_owned(),
            "old_structural_mixed_descriptors".to_owned(),
            "new_structural_mixed_descriptors".to_owned(),
            "old_structural_split_descriptors".to_owned(),
            "new_structural_split_descriptors".to_owned(),
            "structural_shared_profiles".to_owned(),
            "structural_candidate_pairs".to_owned(),
            "structural_largest_posting".to_owned(),
            "structural_duplicate_pairs".to_owned(),
            "structural_unique_reciprocal_pairs".to_owned(),
            "structural_unique_no_anchor_pairs".to_owned(),
            "structural_unique_monotone_anchor_pairs".to_owned(),
            "structural_unique_crossing_veto_pairs".to_owned(),
            "exact_shared_units".to_owned(),
            "old_exact_one_sided_units".to_owned(),
            "new_exact_one_sided_units".to_owned(),
            "near_relation_complete".to_owned(),
            "near_pair_visits_examined".to_owned(),
            "near_pair_visits_attempted".to_owned(),
            "near_similarity_comparisons_examined".to_owned(),
            "near_similarity_comparisons_attempted".to_owned(),
            "near_largest_edge_posting".to_owned(),
            "near_largest_edge_query_union".to_owned(),
            "near_largest_filtered_candidate_set".to_owned(),
            "near_candidate_count_truncated".to_owned(),
            "near_relation_stop_reason".to_owned(),
            "near_pair_candidates".to_owned(),
            "vetoed_near_pairs".to_owned(),
            "recovered_exact_match_old_tokens".to_owned(),
            "recovered_exact_match_new_tokens".to_owned(),
            "recovered_replacement_old_tokens".to_owned(),
            "recovered_replacement_new_tokens".to_owned(),
            "recovered_deletion_tokens".to_owned(),
            "recovered_insertion_tokens".to_owned(),
            "unresolved_remainder_old_source_tokens".to_owned(),
            "unresolved_remainder_new_source_tokens".to_owned(),
        ]);
        let sentence_recovery_keys = records[0]["sentence_recovery_metrics"]
            .as_object()
            .expect("sentence recovery metrics object")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        assert_eq!(sentence_recovery_keys, expected_sentence_recovery_keys);

        // Check truthfulness of values
        let ok_rec = &records[0];
        assert_eq!(ok_rec["pair_id"], "ok-pair");
        assert_eq!(ok_rec["status"], "ok");
        assert_eq!(ok_rec["reported_formatting_changes"], 0);
        assert_eq!(ok_rec["reported_uncertain_changes"], 0);
        assert_eq!(
            ok_rec["sentence_recovery_metrics"]["recovered_deletion_tokens"],
            18
        );
        assert_eq!(
            ok_rec["sentence_recovery_metrics"]["near_relation_complete"],
            true
        );
        assert_eq!(ok_rec["quality"]["unmatched_tiny_changes"], 0);
        assert_eq!(ok_rec["quality"]["precision"], serde_json::Value::Null);
        assert_eq!(ok_rec["quality"]["recall"], 1.0);
        assert_eq!(ok_rec["resource_limit_failure"], serde_json::Value::Null);

        let limit_rec = &records[1];
        assert_eq!(limit_rec["pair_id"], "limit-pair");
        assert_eq!(limit_rec["status"], "limit");
        assert_eq!(limit_rec["coverage_old"], serde_json::Value::Null);
        assert_eq!(limit_rec["quality"], serde_json::Value::Null);
        assert_eq!(
            limit_rec["resource_limit_failure"],
            "alignment candidate visit budget exceeded"
        );
        assert_eq!(
            limit_rec["reported_uncertain_changes"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn full_json_v1_schema_excludes_new_diagnostic_fields() {
        let report = PairRunReport {
            pair_id: "regression-check".to_owned(),
            set: "dev",
            role: "standard",
            document_type: "single-column".to_owned(),
            in_scope: true,
            status: PairRunStatus::Ok,
            provenance_verified: true,
            compared: true,
            extraction_complete: Some(true),
            comparison_complete: Some(true),
            extraction_issues: Vec::new(),
            coverage_old: Some(1.0),
            coverage_new: Some(1.0),
            coverage_comparison: Some(1.0),
            unresolved_regions: Some(0),
            unresolved_old_token_share: Some(0.0),
            unresolved_new_token_share: Some(0.0),
            reported_content_changes: Some(0),
            formatting_only_changes: Some(0),
            uncertain_changes: Some(0),
            reported_changes_preview: Vec::new(),
            quality: None,
            quality_skipped_reason: None,
            scoped_event_metrics: None,
            scoped_token_metrics: None,
            candidate_recall: Some(CandidateRecallMetrics {
                top_k: 32,
                annotated_counterparts: 1,
                evaluable_counterparts: 1,
                recalled_counterparts: 1,
                unavailable_counterparts: 0,
                recall_at_k: Some(1.0),
            }),
            expected_change_diagnostics: Some(ExpectedChangeDiagnostics {
                complete: true,
                failures: vec![ExpectedChangeFailure {
                    expected_id: "change-1".to_owned(),
                    reason: ExpectedChangeFailureReason::CandidateNotGenerated,
                }],
            }),
            resource_limit_failure: None,
            candidate_visits: None,
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            max_candidate_visits: None,
            sentence_recovery_metrics: Some(SentenceRecoveryMetricsReport {
                old_trusted_run_source_tokens: 1,
                ..SentenceRecoveryMetricsReport::default()
            }),
            candidate_visit_pressure: None,
            runtime_ms: 10,
            limit_scale_used: 1.0,
            failure: None,
        };

        let full_bytes = serde_json::to_vec_pretty(&[report]).expect("serialize full");
        let full_val: serde_json::Value = serde_json::from_slice(&full_bytes).expect("parse full");
        let full_obj = full_val[0].as_object().expect("first report");

        let keys = full_obj.keys().cloned().collect::<HashSet<_>>();
        assert_eq!(
            keys,
            HashSet::from([
                "pair_id".to_owned(),
                "set".to_owned(),
                "role".to_owned(),
                "document_type".to_owned(),
                "in_scope".to_owned(),
                "status".to_owned(),
                "provenance_verified".to_owned(),
                "compared".to_owned(),
                "extraction_complete".to_owned(),
                "comparison_complete".to_owned(),
                "extraction_issues".to_owned(),
                "coverage_old".to_owned(),
                "coverage_new".to_owned(),
                "coverage_comparison".to_owned(),
                "unresolved_regions".to_owned(),
                "unresolved_old_token_share".to_owned(),
                "unresolved_new_token_share".to_owned(),
                "reported_content_changes".to_owned(),
                "formatting_only_changes".to_owned(),
                "reported_changes_preview".to_owned(),
                "quality".to_owned(),
                "quality_skipped_reason".to_owned(),
                "resource_limit_failure".to_owned(),
                "candidate_visits".to_owned(),
                "candidate_visits_required".to_owned(),
                "candidate_visits_required_exact".to_owned(),
                "candidate_visits_required_ngram".to_owned(),
                "candidate_visits_required_short_fallback".to_owned(),
                "max_candidate_visits".to_owned(),
                "candidate_visit_pressure".to_owned(),
                "runtime_ms".to_owned(),
                "limit_scale_used".to_owned(),
                "failure".to_owned(),
            ])
        );

        assert!(
            !full_obj.contains_key("uncertain_changes"),
            "full JSON schema must not gain uncertain_changes"
        );
        assert!(
            !full_obj.contains_key("sentence_recovery_metrics"),
            "full JSON v1 schema must not gain sentence recovery diagnostics"
        );
        assert!(!full_obj.contains_key("candidate_recall"));
        assert!(!full_obj.contains_key("expected_change_diagnostics"));
    }

    #[test]
    fn summary_json_is_deterministic_and_excludes_forbidden_keys_and_values() {
        let base = PairRunReport {
            pair_id: "deterministic-pair".to_owned(),
            set: "dev",
            role: "standard",
            document_type: "single-column".to_owned(),
            in_scope: true,
            status: PairRunStatus::Ok,
            provenance_verified: true,
            compared: true,
            extraction_complete: Some(true),
            comparison_complete: Some(false),
            extraction_issues: vec![IssueLine {
                side: "old",
                kind: "unsupported",
                scope: "page".to_owned(),
                description: "Injected /tmp/path/document.pdf extraction issue".to_owned(),
            }],
            coverage_old: Some(0.4),
            coverage_new: Some(0.5),
            coverage_comparison: Some(0.4),
            unresolved_regions: Some(1),
            unresolved_old_token_share: Some(0.02),
            unresolved_new_token_share: Some(0.03),
            reported_content_changes: Some(5),
            formatting_only_changes: Some(0),
            uncertain_changes: Some(0),
            reported_changes_preview: vec![ReportedChangeText {
                kind: "replacement",
                old_text: Some("Sensitive text /home/user/cache/doc.pdf".to_owned()),
                new_text: Some("Sensitive text replacement".to_owned()),
            }],
            quality: None,
            quality_skipped_reason: None,
            scoped_event_metrics: None,
            scoped_token_metrics: None,
            candidate_recall: None,
            expected_change_diagnostics: None,
            resource_limit_failure: None,
            candidate_visits: Some(10),
            candidate_visits_required: Some(20),
            candidate_visits_required_exact: Some(5),
            candidate_visits_required_ngram: Some(10),
            candidate_visits_required_short_fallback: Some(5),
            max_candidate_visits: Some(500),
            sentence_recovery_metrics: None,
            candidate_visit_pressure: None,
            runtime_ms: 100,
            limit_scale_used: 1.0,
            failure: Some("Failure with /home/hayato/cache/old.pdf".to_owned()),
        };

        let mut report_a = base.clone();
        report_a.runtime_ms = 42;
        report_a.candidate_visits = Some(999);

        let mut report_b = base;
        report_b.runtime_ms = 888888;
        report_b.candidate_visits = Some(12345);

        let summary_a = RevisionSummaryReport::from_reports(&[report_a]);
        let summary_b = RevisionSummaryReport::from_reports(&[report_b]);

        let bytes_a = serde_json::to_vec_pretty(&summary_a).expect("serialize A");
        let bytes_b = serde_json::to_vec_pretty(&summary_b).expect("serialize B");

        assert_eq!(
            bytes_a, bytes_b,
            "summaries must be byte-for-byte deterministic"
        );

        let json_str = String::from_utf8(bytes_a).expect("utf8 string");
        let forbidden = [
            "/home/",
            "/tmp/",
            "Sensitive text",
            "runtime_ms",
            "\"failure\":",
            "candidate_visits",
            "extraction_issues",
            "candidate_visit_pressure",
            "reported_changes_preview",
        ];
        for term in forbidden {
            assert!(
                !json_str.contains(term),
                "summary JSON must not contain forbidden term {term:?}"
            );
        }
    }
}
