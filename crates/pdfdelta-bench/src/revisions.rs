//! Real-world revision-pair benchmark track.
//!
//! Unlike the synthetic acceptance matrix, this track compares two genuinely
//! different public revisions of the same document. Documents are never
//! vendored: the manifest records stable URLs, capture dates, byte sizes, and
//! SHA-256 checksums, and operators download the files into a local cache
//! directory with `benchmark/realworld/fetch.sh`.

use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
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
    diff::{
        ChangeKind, Comparison, ExactSegmentRelation, KnownSpanSentenceShadowMetrics,
        NearRelationStopReason, NearSearchScopeMetrics, NearSearchWorkMetrics,
        RecoveryWatchDiagnostics, RecoveryWatchGranularPairEvidence, RecoveryWatchGranularRelation,
        RecoveryWatchGranularStopReason, RecoveryWatchGranularUnitEvidence, RecoveryWatchNearScope,
        RecoveryWatchOccurrence, RecoveryWatchOccurrenceEvidence, RecoveryWatchPairEvidence,
        RecoveryWatchQuery, RecoveryWatchRelation, RecoveryWatchSegmentPairEvidence,
        RecoveryWatchUnitKind, RunSignatureStopReason, SegmentStopReason,
        SentenceEdgeGateShadowMetrics, SentenceEdgeGateShadowStopReason, SentenceRecoveryMetrics,
        TextSpan,
    },
    layout::BlockRole,
    model::Document,
    model::Rect,
    normalize::{BlockText, ComparableToken},
    pdf::{LopdfParser, ParseLimits},
    pipeline::{
        PipelineDiagnostics, PipelineOptions, PipelinePhase, PipelinePhaseStatus,
        compare_extraction_outcomes_with_known_span_sentence_shadow_diagnostics,
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

use revision_diagnostics::{ComparisonDiagnosticInput, evaluate_reviewed_diagnostics};
use revision_scopes::{
    SCOPED_CHANGE_INDETERMINATE, classify_scoped_changes, evaluate_scoped_token_metrics,
    resolve_revision_scopes, validate_scoped_expected_changes,
};

pub const QUALITY_SKIP_RESOURCE_LIMIT: &str = "comparison stopped at a resource limit";
pub const QUALITY_SKIP_INCOMPLETE_EXTRACTION: &str =
    "extraction was incomplete so reported diffs are suppressed";
pub const QUALITY_SKIP_NO_ANNOTATIONS: &str = "no expected annotations are recorded for this pair";
pub const QUALITY_SKIP_MATCHING_RESOURCE_LIMIT: &str =
    "change matching exceeded benchmark resource limits";

/// Matching retains at most this many expectation-to-event candidate edges.
const MAX_MATCH_CANDIDATE_EDGES: usize = 100_000;
/// Matching evaluates at most this many expectation-to-event pairs.
const MAX_MATCH_EDGE_CHECKS: usize = 1_000_000;
/// Matching accepts at most this many expected or actual semantic events.
const MAX_MATCH_EVENTS_PER_SIDE: usize = 16_384;
/// Human-reviewed JSON annotations accept at most this many expected changes.
const MAX_EXPECTED_DOCUMENT_CHANGES: usize = 4_096;
/// Matching examines at most this many residual edges across all searches.
const MAX_MATCH_RESIDUAL_EDGE_VISITS: usize = 5_000_000;
/// Matching performs at most this many augmentations.
const MAX_MATCH_AUGMENTATIONS: usize = 4_096;
/// Matching examines at most this many semantic change occurrences.
const MAX_MATCH_OCCURRENCE_VISITS: usize = 5_000_000;
/// Matching examines at most this many bytes of normalized occurrence text.
const MAX_MATCH_TEXT_BYTES: usize = 256 * 1024 * 1024;
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
pub(super) const MAX_EXPECTED_CHANGE_DIAGNOSTICS: usize = 4_096;

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
    OccurrenceCountMismatch { expected: usize, actual: usize },
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
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExpectedChangeDiagnostics {
    pub complete: bool,
    pub failures: Vec<ExpectedChangeFailure>,
    pub recovery_watch: Option<RecoveryWatchDiagnosticsReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecoveryWatchDiagnosticsReport {
    pub complete: bool,
    pub candidate_generation_complete: bool,
    pub near_relation_complete: bool,
    pub near_relation_stop_reason: Option<NearRelationStopReasonReport>,
    pub segment_candidates: usize,
    pub segment_hash_matches: usize,
    pub segment_token_verified_matches: usize,
    pub segment_unique_pairs: usize,
    pub segment_duplicate_pairs: usize,
    pub segment_monotone_pairs: usize,
    pub segment_crossing_pairs: usize,
    pub segment_overlap_vetoes: usize,
    pub segment_stop_reason: Option<SegmentStopReasonReport>,
    pub granular_complete: bool,
    pub granular_old_units: usize,
    pub granular_new_units: usize,
    pub granular_pair_comparisons: usize,
    pub granular_stop_reason: Option<RecoveryWatchGranularStopReasonReport>,
    pub records: Vec<ExpectedChangeRecoveryWatchRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExpectedChangeRecoveryWatchRecord {
    pub expected_id: String,
    pub old: RecoveryWatchOccurrenceReport,
    pub new: RecoveryWatchOccurrenceReport,
    pub pair: Option<RecoveryWatchPairEvidenceReport>,
    pub segment_pair: Option<RecoveryWatchSegmentPairEvidenceReport>,
    pub granular_pair: Option<RecoveryWatchGranularPairEvidenceReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecoveryWatchOccurrenceReport {
    NotQueried,
    Unfound,
    Ambiguous,
    Unavailable,
    Found {
        #[serde(flatten)]
        occurrence: RecoveryWatchFoundOccurrenceReport,
    },
    Occurrences {
        occurrence_count: usize,
        complete: bool,
        truncated: bool,
        occurrences: Vec<RecoveryWatchFoundOccurrenceReport>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecoveryWatchFoundOccurrenceReport {
    pub span_index: Option<usize>,
    pub trusted_run_descriptor_index: Option<usize>,
    pub ordinal: Option<usize>,
    pub end_ordinal: Option<usize>,
    pub unit_count: Option<usize>,
    pub token_count: Option<usize>,
    pub recovery_location_available: bool,
    pub fully_contained: bool,
    pub page: Option<u32>,
    pub bbox: Option<RecoveryWatchRectReport>,
    pub role: Option<RecoveryWatchBlockRoleReport>,
    pub kind: RecoveryWatchUnitKindReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RecoveryWatchRectReport {
    pub min: RecoveryWatchPointReport,
    pub max: RecoveryWatchPointReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RecoveryWatchPointReport {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryWatchBlockRoleReport {
    Body,
    RepeatedHeader,
    RepeatedFooter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryWatchUnitKindReport {
    Sentence,
    Line,
    Segment,
    Clause,
    ListItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryWatchGranularPairEvidenceReport {
    pub old_units: Vec<RecoveryWatchGranularUnitEvidenceReport>,
    pub new_units: Vec<RecoveryWatchGranularUnitEvidenceReport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryWatchGranularUnitEvidenceReport {
    pub kind: RecoveryWatchUnitKindReport,
    pub byte_start: usize,
    pub byte_end: usize,
    pub token_count: usize,
    pub page: Option<u32>,
    pub role: Option<RecoveryWatchBlockRoleReport>,
    pub recovery_location_available: bool,
    pub relation: RecoveryWatchGranularRelationReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryWatchGranularRelationReport {
    pub available: bool,
    pub best_score: u16,
    pub second_score: u16,
    pub partner_index: Option<usize>,
    pub exact: bool,
    pub reciprocal: bool,
    pub tied_for_best: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryWatchGranularStopReasonReport {
    UnitCountLimit,
    TokenByteLimit,
    ComparisonLimit,
    OutputLimit,
    AuxiliaryLimit,
    AllocationFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryWatchPairEvidenceReport {
    pub same_span: bool,
    pub exact_shared_units: usize,
    pub exact_shared_units_available: bool,
    pub near_candidate_examined: bool,
    pub near_score: Option<u16>,
    pub near_scope: Option<RecoveryWatchNearScopeReport>,
    pub old_relation: RecoveryWatchRelationReport,
    pub new_relation: RecoveryWatchRelationReport,
    pub reciprocal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryWatchSegmentPairEvidenceReport {
    pub old_start_ordinal: usize,
    pub old_end_ordinal: usize,
    pub new_start_ordinal: usize,
    pub new_end_ordinal: usize,
    pub old_unit_count: usize,
    pub new_unit_count: usize,
    pub old_token_count: usize,
    pub new_token_count: usize,
    pub exact: bool,
    pub old_occurrence_count: usize,
    pub new_occurrence_count: usize,
    pub role_compatible: bool,
    pub overlaps_existing_recovery: bool,
    pub crossing_anchor_count: usize,
    pub relation: ExactSegmentRelationReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactSegmentRelationReport {
    NonExact,
    Duplicate,
    ExactUniqueTopologyUnknown,
    ExactUniqueMonotone,
    ExactUniqueCrossing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStopReasonReport {
    CandidateCountLimit,
    HashPairVisitLimit,
    TokenVerificationLimit,
    AllocationFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryWatchNearScopeReport {
    SameSpan,
    CrossSpan,
    PairedStream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryWatchRelationReport {
    pub available: bool,
    pub best_score: u16,
    pub second_score: u16,
    pub watched_partner_is_best: bool,
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
    pub run_signature_available: bool,
    pub run_signature_complete: bool,
    pub old_run_signature_unique_units: usize,
    pub new_run_signature_unique_units: usize,
    pub old_run_signature_duplicate_units: usize,
    pub new_run_signature_duplicate_units: usize,
    pub run_signature_shared_unit_keys: usize,
    pub run_signature_largest_posting: usize,
    pub run_signature_posting_visits_attempted: usize,
    pub run_signature_posting_visits_examined: usize,
    pub run_signature_token_verifications_attempted: usize,
    pub run_signature_token_verifications_examined: usize,
    pub run_signature_candidate_pairs: usize,
    pub run_signature_globally_anchored_runs_skipped: usize,
    pub run_signature_reciprocal_unique_pairs: usize,
    pub run_signature_margin_qualified_pairs: usize,
    pub run_signature_margin_veto_pairs: usize,
    pub run_signature_monotone_pairs: usize,
    pub run_signature_crossing_veto_pairs: usize,
    pub run_signature_max_shared_units: usize,
    pub run_signature_stop_reason: Option<RunSignatureStopReasonReport>,
    pub exact_shared_units: usize,
    pub old_exact_one_sided_units: usize,
    pub new_exact_one_sided_units: usize,
    pub near_relation_complete: bool,
    pub relation_floor_pairs_considered: usize,
    pub relation_floor_word_scans: usize,
    pub relation_floor_stop_opportunities: usize,
    pub relation_floor_potential_saved_word_comparisons: usize,
    pub near_sentence_work: NearSearchWorkMetricsReport,
    pub near_line_work: NearSearchWorkMetricsReport,
    pub near_paired_interval_work: NearSearchScopeMetricsReport,
    pub near_paired_cross_interval_veto_work: NearSearchScopeMetricsReport,
    pub near_same_or_ambiguous_span_work: NearSearchScopeMetricsReport,
    pub near_same_known_span_work: NearSearchScopeMetricsReport,
    pub near_ambiguous_span_work: NearSearchScopeMetricsReport,
    pub near_same_or_ambiguous_shared_query_work: NearSearchScopeMetricsReport,
    pub near_cross_span_work: NearSearchScopeMetricsReport,
    pub known_span_sentence_shadow: Option<KnownSpanSentenceShadowMetricsReport>,
    pub sentence_edge_gate_shadow: Option<SentenceEdgeGateShadowMetricsReport>,
    pub near_pair_visits_examined: usize,
    pub near_pair_visits_attempted: usize,
    pub near_similarity_comparisons_examined: usize,
    pub near_similarity_comparisons_attempted: usize,
    pub near_candidate_posting_visits_examined: usize,
    pub near_candidate_posting_visits_attempted: usize,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct KnownSpanSentenceShadowMetricsReport {
    pub complete: bool,
    pub pairs_considered: usize,
    pub pairs_retained: usize,
    pub pairs_rejected: usize,
    pub cross_span_pairs_considered: usize,
    pub same_paired_anchor_interval_pairs: usize,
    pub same_paired_stream_other_interval_pairs: usize,
    pub same_page_only_pairs: usize,
    pub unclassified_pairs: usize,
    pub old_relation_mismatches: usize,
    pub new_relation_mismatches: usize,
    pub best_partner_mismatches: usize,
    pub best_score_mismatches: usize,
    pub second_score_mismatches: usize,
    pub veto_mismatches: usize,
    pub unique_partner_mismatches: usize,
    pub reciprocal_pair_mismatches: usize,
    pub exact_relation_parity: bool,
}

impl From<KnownSpanSentenceShadowMetrics> for KnownSpanSentenceShadowMetricsReport {
    fn from(metrics: KnownSpanSentenceShadowMetrics) -> Self {
        Self {
            complete: metrics.complete,
            pairs_considered: metrics.pairs_considered,
            pairs_retained: metrics.pairs_retained,
            pairs_rejected: metrics.pairs_rejected,
            cross_span_pairs_considered: metrics.cross_span_pairs_considered,
            same_paired_anchor_interval_pairs: metrics.same_paired_anchor_interval_pairs,
            same_paired_stream_other_interval_pairs: metrics
                .same_paired_stream_other_interval_pairs,
            same_page_only_pairs: metrics.same_page_only_pairs,
            unclassified_pairs: metrics.unclassified_pairs,
            old_relation_mismatches: metrics.old_relation_mismatches,
            new_relation_mismatches: metrics.new_relation_mismatches,
            best_partner_mismatches: metrics.best_partner_mismatches,
            best_score_mismatches: metrics.best_score_mismatches,
            second_score_mismatches: metrics.second_score_mismatches,
            veto_mismatches: metrics.veto_mismatches,
            unique_partner_mismatches: metrics.unique_partner_mismatches,
            reciprocal_pair_mismatches: metrics.reciprocal_pair_mismatches,
            exact_relation_parity: metrics.exact_relation_parity,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SentenceEdgeGateShadowMetricsReport {
    pub complete: bool,
    pub stop_reason: Option<SentenceEdgeGateShadowStopReasonReport>,
    pub pairs_considered: usize,
    pub pairs_retained: usize,
    pub pairs_rejected: usize,
    pub same_known_rejected: usize,
    pub ambiguous_rejected: usize,
    pub cross_span_rejected: usize,
    pub unclassified_rejected: usize,
    pub projected_pair_visits: usize,
    pub projected_similarity_comparisons: usize,
    pub rejected_max_production_score: u16,
    pub threshold_violations: usize,
    pub veto_mismatches: usize,
    pub unique_partner_mismatches: usize,
    pub reciprocal_pair_mismatches: usize,
    pub adopted_replacement_mismatches: usize,
    pub insertion_deletion_veto_mismatches: usize,
}

impl From<SentenceEdgeGateShadowMetrics> for SentenceEdgeGateShadowMetricsReport {
    fn from(metrics: SentenceEdgeGateShadowMetrics) -> Self {
        Self {
            complete: metrics.complete,
            stop_reason: metrics.stop_reason.map(Into::into),
            pairs_considered: metrics.pairs_considered,
            pairs_retained: metrics.pairs_retained,
            pairs_rejected: metrics.pairs_rejected,
            same_known_rejected: metrics.same_known_rejected,
            ambiguous_rejected: metrics.ambiguous_rejected,
            cross_span_rejected: metrics.cross_span_rejected,
            unclassified_rejected: metrics.unclassified_rejected,
            projected_pair_visits: metrics.projected_pair_visits,
            projected_similarity_comparisons: metrics.projected_similarity_comparisons,
            rejected_max_production_score: metrics.rejected_max_production_score,
            threshold_violations: metrics.threshold_violations,
            veto_mismatches: metrics.veto_mismatches,
            unique_partner_mismatches: metrics.unique_partner_mismatches,
            reciprocal_pair_mismatches: metrics.reciprocal_pair_mismatches,
            adopted_replacement_mismatches: metrics.adopted_replacement_mismatches,
            insertion_deletion_veto_mismatches: metrics.insertion_deletion_veto_mismatches,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NearSearchWorkMetricsReport {
    pub edge_posting_visits_examined: usize,
    pub edge_posting_visits_attempted: usize,
    pub line_trigram_posting_visits_examined: usize,
    pub line_trigram_posting_visits_attempted: usize,
    pub edge_query_union_candidates: usize,
    pub line_trigram_only_query_union_candidates: usize,
    pub filtered_candidates: usize,
    pub pair_visits_examined: usize,
    pub pair_visits_attempted: usize,
    pub similarity_comparisons_examined: usize,
    pub similarity_comparisons_attempted: usize,
}

impl From<NearSearchWorkMetrics> for NearSearchWorkMetricsReport {
    fn from(metrics: NearSearchWorkMetrics) -> Self {
        Self {
            edge_posting_visits_examined: metrics.edge_posting_visits_examined,
            edge_posting_visits_attempted: metrics.edge_posting_visits_attempted,
            line_trigram_posting_visits_examined: metrics.line_trigram_posting_visits_examined,
            line_trigram_posting_visits_attempted: metrics.line_trigram_posting_visits_attempted,
            edge_query_union_candidates: metrics.edge_query_union_candidates,
            line_trigram_only_query_union_candidates: metrics
                .line_trigram_only_query_union_candidates,
            filtered_candidates: metrics.filtered_candidates,
            pair_visits_examined: metrics.pair_visits_examined,
            pair_visits_attempted: metrics.pair_visits_attempted,
            similarity_comparisons_examined: metrics.similarity_comparisons_examined,
            similarity_comparisons_attempted: metrics.similarity_comparisons_attempted,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NearSearchScopeMetricsReport {
    pub sentence_work: NearSearchWorkMetricsReport,
    pub line_work: NearSearchWorkMetricsReport,
}

impl From<NearSearchScopeMetrics> for NearSearchScopeMetricsReport {
    fn from(metrics: NearSearchScopeMetrics) -> Self {
        Self {
            sentence_work: metrics.sentence_work.into(),
            line_work: metrics.line_work.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NearRelationStopReasonReport {
    CandidatePostingVisitLimit,
    PairVisitLimit,
    SimilarityComparisonLimit,
    CandidateCountLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSignatureStopReasonReport {
    PostingVisitLimit,
    TokenVerificationLimit,
    CandidatePairLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SentenceEdgeGateShadowStopReasonReport {
    CandidatePostingVisitLimit,
    PairVisitLimit,
    SimilarityComparisonLimit,
    CandidateCountLimit,
    AllocationFailure,
    CounterOverflow,
    DiagnosticFailure,
}

impl From<SentenceEdgeGateShadowStopReason> for SentenceEdgeGateShadowStopReasonReport {
    fn from(reason: SentenceEdgeGateShadowStopReason) -> Self {
        match reason {
            SentenceEdgeGateShadowStopReason::CandidatePostingVisitLimit => {
                Self::CandidatePostingVisitLimit
            }
            SentenceEdgeGateShadowStopReason::PairVisitLimit => Self::PairVisitLimit,
            SentenceEdgeGateShadowStopReason::SimilarityComparisonLimit => {
                Self::SimilarityComparisonLimit
            }
            SentenceEdgeGateShadowStopReason::CandidateCountLimit => Self::CandidateCountLimit,
            SentenceEdgeGateShadowStopReason::AllocationFailure => Self::AllocationFailure,
            SentenceEdgeGateShadowStopReason::CounterOverflow => Self::CounterOverflow,
            SentenceEdgeGateShadowStopReason::DiagnosticFailure => Self::DiagnosticFailure,
        }
    }
}

impl From<NearRelationStopReason> for NearRelationStopReasonReport {
    fn from(reason: NearRelationStopReason) -> Self {
        match reason {
            NearRelationStopReason::CandidatePostingVisitLimit => Self::CandidatePostingVisitLimit,
            NearRelationStopReason::PairVisitLimit => Self::PairVisitLimit,
            NearRelationStopReason::SimilarityComparisonLimit => Self::SimilarityComparisonLimit,
            NearRelationStopReason::CandidateCountLimit => Self::CandidateCountLimit,
        }
    }
}

impl From<RecoveryWatchOccurrenceEvidence> for RecoveryWatchOccurrenceReport {
    fn from(evidence: RecoveryWatchOccurrenceEvidence) -> Self {
        match evidence {
            RecoveryWatchOccurrenceEvidence::NotQueried => Self::NotQueried,
            RecoveryWatchOccurrenceEvidence::Unfound => Self::Unfound,
            RecoveryWatchOccurrenceEvidence::Ambiguous => Self::Ambiguous,
            RecoveryWatchOccurrenceEvidence::Unavailable => Self::Unavailable,
            RecoveryWatchOccurrenceEvidence::Found(occurrence) => Self::Found {
                occurrence: occurrence.into(),
            },
            RecoveryWatchOccurrenceEvidence::Occurrences(occurrences) => Self::Occurrences {
                occurrence_count: occurrences.occurrence_count,
                complete: occurrences.complete,
                truncated: !occurrences.complete,
                occurrences: occurrences
                    .occurrences
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
        }
    }
}

impl From<RecoveryWatchOccurrence> for RecoveryWatchFoundOccurrenceReport {
    fn from(occurrence: RecoveryWatchOccurrence) -> Self {
        Self {
            span_index: occurrence.span_index,
            trusted_run_descriptor_index: occurrence.trusted_run_descriptor_index,
            ordinal: occurrence.ordinal,
            end_ordinal: occurrence.end_ordinal,
            unit_count: occurrence.unit_count,
            token_count: occurrence.token_count,
            recovery_location_available: occurrence.recovery_location_available,
            fully_contained: occurrence.fully_contained,
            page: occurrence.page,
            bbox: occurrence.bbox.map(Into::into),
            role: occurrence.role.map(Into::into),
            kind: occurrence.kind.into(),
        }
    }
}

impl From<Rect> for RecoveryWatchRectReport {
    fn from(rect: Rect) -> Self {
        Self {
            min: RecoveryWatchPointReport {
                x: rect.min.x,
                y: rect.min.y,
            },
            max: RecoveryWatchPointReport {
                x: rect.max.x,
                y: rect.max.y,
            },
        }
    }
}

impl From<BlockRole> for RecoveryWatchBlockRoleReport {
    fn from(role: BlockRole) -> Self {
        match role {
            BlockRole::Body => Self::Body,
            BlockRole::RepeatedHeader => Self::RepeatedHeader,
            BlockRole::RepeatedFooter => Self::RepeatedFooter,
        }
    }
}

impl From<RecoveryWatchUnitKind> for RecoveryWatchUnitKindReport {
    fn from(kind: RecoveryWatchUnitKind) -> Self {
        match kind {
            RecoveryWatchUnitKind::Sentence => Self::Sentence,
            RecoveryWatchUnitKind::Line => Self::Line,
            RecoveryWatchUnitKind::Segment => Self::Segment,
            RecoveryWatchUnitKind::Clause => Self::Clause,
            RecoveryWatchUnitKind::ListItem => Self::ListItem,
        }
    }
}

impl From<RecoveryWatchGranularPairEvidence> for RecoveryWatchGranularPairEvidenceReport {
    fn from(pair: RecoveryWatchGranularPairEvidence) -> Self {
        Self {
            old_units: pair.old_units.into_iter().map(Into::into).collect(),
            new_units: pair.new_units.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<RecoveryWatchGranularUnitEvidence> for RecoveryWatchGranularUnitEvidenceReport {
    fn from(unit: RecoveryWatchGranularUnitEvidence) -> Self {
        Self {
            kind: unit.kind.into(),
            byte_start: unit.byte_start,
            byte_end: unit.byte_end,
            token_count: unit.token_count,
            page: unit.page,
            role: unit.role.map(Into::into),
            recovery_location_available: unit.recovery_location_available,
            relation: unit.relation.into(),
        }
    }
}

impl From<RecoveryWatchGranularRelation> for RecoveryWatchGranularRelationReport {
    fn from(relation: RecoveryWatchGranularRelation) -> Self {
        Self {
            available: relation.available,
            best_score: relation.best_score,
            second_score: relation.second_score,
            partner_index: relation.partner_index,
            exact: relation.exact,
            reciprocal: relation.reciprocal,
            tied_for_best: relation.tied_for_best,
        }
    }
}

impl From<RecoveryWatchGranularStopReason> for RecoveryWatchGranularStopReasonReport {
    fn from(reason: RecoveryWatchGranularStopReason) -> Self {
        match reason {
            RecoveryWatchGranularStopReason::UnitCountLimit => Self::UnitCountLimit,
            RecoveryWatchGranularStopReason::TokenByteLimit => Self::TokenByteLimit,
            RecoveryWatchGranularStopReason::ComparisonLimit => Self::ComparisonLimit,
            RecoveryWatchGranularStopReason::OutputLimit => Self::OutputLimit,
            RecoveryWatchGranularStopReason::AuxiliaryLimit => Self::AuxiliaryLimit,
            RecoveryWatchGranularStopReason::AllocationFailure => Self::AllocationFailure,
        }
    }
}

impl From<RecoveryWatchPairEvidence> for RecoveryWatchPairEvidenceReport {
    fn from(pair: RecoveryWatchPairEvidence) -> Self {
        Self {
            same_span: pair.same_span,
            exact_shared_units: pair.exact_shared_units,
            exact_shared_units_available: pair.exact_shared_units_available,
            near_candidate_examined: pair.near_candidate_examined,
            near_score: pair.near_score,
            near_scope: pair.near_scope.map(Into::into),
            old_relation: pair.old_relation.into(),
            new_relation: pair.new_relation.into(),
            reciprocal: pair.reciprocal,
        }
    }
}

impl From<RecoveryWatchSegmentPairEvidence> for RecoveryWatchSegmentPairEvidenceReport {
    fn from(pair: RecoveryWatchSegmentPairEvidence) -> Self {
        Self {
            old_start_ordinal: pair.old_start_ordinal,
            old_end_ordinal: pair.old_end_ordinal,
            new_start_ordinal: pair.new_start_ordinal,
            new_end_ordinal: pair.new_end_ordinal,
            old_unit_count: pair.old_unit_count,
            new_unit_count: pair.new_unit_count,
            old_token_count: pair.old_token_count,
            new_token_count: pair.new_token_count,
            exact: pair.exact,
            old_occurrence_count: pair.old_occurrence_count,
            new_occurrence_count: pair.new_occurrence_count,
            role_compatible: pair.role_compatible,
            overlaps_existing_recovery: pair.overlaps_existing_recovery,
            crossing_anchor_count: pair.crossing_anchor_count,
            relation: pair.relation.into(),
        }
    }
}

impl From<ExactSegmentRelation> for ExactSegmentRelationReport {
    fn from(relation: ExactSegmentRelation) -> Self {
        match relation {
            ExactSegmentRelation::NonExact => Self::NonExact,
            ExactSegmentRelation::Duplicate => Self::Duplicate,
            ExactSegmentRelation::ExactUniqueTopologyUnknown => Self::ExactUniqueTopologyUnknown,
            ExactSegmentRelation::ExactUniqueMonotone => Self::ExactUniqueMonotone,
            ExactSegmentRelation::ExactUniqueCrossing => Self::ExactUniqueCrossing,
        }
    }
}

impl From<SegmentStopReason> for SegmentStopReasonReport {
    fn from(reason: SegmentStopReason) -> Self {
        match reason {
            SegmentStopReason::CandidateCountLimit => Self::CandidateCountLimit,
            SegmentStopReason::HashPairVisitLimit => Self::HashPairVisitLimit,
            SegmentStopReason::TokenVerificationLimit => Self::TokenVerificationLimit,
            SegmentStopReason::AllocationFailure => Self::AllocationFailure,
        }
    }
}

impl From<RecoveryWatchNearScope> for RecoveryWatchNearScopeReport {
    fn from(scope: RecoveryWatchNearScope) -> Self {
        match scope {
            RecoveryWatchNearScope::SameSpan => Self::SameSpan,
            RecoveryWatchNearScope::CrossSpan => Self::CrossSpan,
            RecoveryWatchNearScope::PairedStream => Self::PairedStream,
        }
    }
}

impl From<RecoveryWatchRelation> for RecoveryWatchRelationReport {
    fn from(relation: RecoveryWatchRelation) -> Self {
        Self {
            available: relation.available,
            best_score: relation.best_score,
            second_score: relation.second_score,
            watched_partner_is_best: relation.watched_partner_is_best,
        }
    }
}

fn completed_empty_recovery_watch_report() -> RecoveryWatchDiagnosticsReport {
    RecoveryWatchDiagnosticsReport {
        complete: true,
        candidate_generation_complete: true,
        near_relation_complete: true,
        near_relation_stop_reason: None,
        segment_candidates: 0,
        segment_hash_matches: 0,
        segment_token_verified_matches: 0,
        segment_unique_pairs: 0,
        segment_duplicate_pairs: 0,
        segment_monotone_pairs: 0,
        segment_crossing_pairs: 0,
        segment_overlap_vetoes: 0,
        segment_stop_reason: None,
        granular_complete: true,
        granular_old_units: 0,
        granular_new_units: 0,
        granular_pair_comparisons: 0,
        granular_stop_reason: None,
        records: Vec::new(),
    }
}

fn recovery_watch_report(
    expected: &[ExpectedChange],
    queries: &RecoveryWatchQuerySet,
    diagnostics: RecoveryWatchDiagnostics,
) -> RecoveryWatchDiagnosticsReport {
    let mut records_by_id = HashMap::new();
    let mut join_complete = true;
    for record in diagnostics.records {
        match records_by_id.entry(record.id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(record);
            }
            Entry::Occupied(_) => join_complete = false,
        }
    }
    let records = queries
        .ids
        .iter()
        .zip(&queries.expected_indices)
        .map(|(id, &expected_index)| {
            let expected_id = expected
                .get(expected_index)
                .map(|change| change.id.clone())
                .unwrap_or_else(|| {
                    join_complete = false;
                    id.clone()
                });
            match records_by_id.remove(id) {
                Some(record) => ExpectedChangeRecoveryWatchRecord {
                    expected_id,
                    old: record.old.into(),
                    new: record.new.into(),
                    pair: record.pair.map(Into::into),
                    segment_pair: record.segment_pair.map(Into::into),
                    granular_pair: record.granular_pair.map(Into::into),
                },
                None => {
                    join_complete = false;
                    let change = expected.get(expected_index);
                    ExpectedChangeRecoveryWatchRecord {
                        expected_id,
                        old: missing_recovery_watch_side(
                            change.and_then(|change| change.old_quote.as_deref()),
                        ),
                        new: missing_recovery_watch_side(
                            change.and_then(|change| change.new_quote.as_deref()),
                        ),
                        pair: None,
                        segment_pair: None,
                        granular_pair: None,
                    }
                }
            }
        })
        .collect();
    join_complete &= records_by_id.is_empty();
    RecoveryWatchDiagnosticsReport {
        complete: diagnostics.complete && join_complete,
        candidate_generation_complete: diagnostics.candidate_generation_complete,
        near_relation_complete: diagnostics.near_relation_complete,
        near_relation_stop_reason: diagnostics.near_relation_stop_reason.map(Into::into),
        segment_candidates: diagnostics.segment_candidates,
        segment_hash_matches: diagnostics.segment_hash_matches,
        segment_token_verified_matches: diagnostics.segment_token_verified_matches,
        segment_unique_pairs: diagnostics.segment_unique_pairs,
        segment_duplicate_pairs: diagnostics.segment_duplicate_pairs,
        segment_monotone_pairs: diagnostics.segment_monotone_pairs,
        segment_crossing_pairs: diagnostics.segment_crossing_pairs,
        segment_overlap_vetoes: diagnostics.segment_overlap_vetoes,
        segment_stop_reason: diagnostics.segment_stop_reason.map(Into::into),
        granular_complete: diagnostics.granular_complete,
        granular_old_units: diagnostics.granular_old_units,
        granular_new_units: diagnostics.granular_new_units,
        granular_pair_comparisons: diagnostics.granular_pair_comparisons,
        granular_stop_reason: diagnostics.granular_stop_reason.map(Into::into),
        records,
    }
}

fn missing_recovery_watch_side(quote: Option<&str>) -> RecoveryWatchOccurrenceReport {
    if quote.is_some() {
        RecoveryWatchOccurrenceReport::Unavailable
    } else {
        RecoveryWatchOccurrenceReport::NotQueried
    }
}

impl From<RunSignatureStopReason> for RunSignatureStopReasonReport {
    fn from(reason: RunSignatureStopReason) -> Self {
        match reason {
            RunSignatureStopReason::PostingVisitLimit => Self::PostingVisitLimit,
            RunSignatureStopReason::TokenVerificationLimit => Self::TokenVerificationLimit,
            RunSignatureStopReason::CandidatePairLimit => Self::CandidatePairLimit,
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
            run_signature_available: metrics.run_signature_available,
            run_signature_complete: metrics.run_signature_complete,
            old_run_signature_unique_units: metrics.old_run_signature_unique_units,
            new_run_signature_unique_units: metrics.new_run_signature_unique_units,
            old_run_signature_duplicate_units: metrics.old_run_signature_duplicate_units,
            new_run_signature_duplicate_units: metrics.new_run_signature_duplicate_units,
            run_signature_shared_unit_keys: metrics.run_signature_shared_unit_keys,
            run_signature_largest_posting: metrics.run_signature_largest_posting,
            run_signature_posting_visits_attempted: metrics.run_signature_posting_visits_attempted,
            run_signature_posting_visits_examined: metrics.run_signature_posting_visits_examined,
            run_signature_token_verifications_attempted: metrics
                .run_signature_token_verifications_attempted,
            run_signature_token_verifications_examined: metrics
                .run_signature_token_verifications_examined,
            run_signature_candidate_pairs: metrics.run_signature_candidate_pairs,
            run_signature_globally_anchored_runs_skipped: metrics
                .run_signature_globally_anchored_runs_skipped,
            run_signature_reciprocal_unique_pairs: metrics.run_signature_reciprocal_unique_pairs,
            run_signature_margin_qualified_pairs: metrics.run_signature_margin_qualified_pairs,
            run_signature_margin_veto_pairs: metrics.run_signature_margin_veto_pairs,
            run_signature_monotone_pairs: metrics.run_signature_monotone_pairs,
            run_signature_crossing_veto_pairs: metrics.run_signature_crossing_veto_pairs,
            run_signature_max_shared_units: metrics.run_signature_max_shared_units,
            run_signature_stop_reason: metrics.run_signature_stop_reason.map(Into::into),
            exact_shared_units: metrics.exact_shared_units,
            old_exact_one_sided_units: metrics.old_exact_one_sided_units,
            new_exact_one_sided_units: metrics.new_exact_one_sided_units,
            near_relation_complete: metrics.near_relation_complete,
            relation_floor_pairs_considered: metrics.relation_floor_pairs_considered,
            relation_floor_word_scans: metrics.relation_floor_word_scans,
            relation_floor_stop_opportunities: metrics.relation_floor_stop_opportunities,
            relation_floor_potential_saved_word_comparisons: metrics
                .relation_floor_potential_saved_word_comparisons,
            near_sentence_work: metrics.near_sentence_work.into(),
            near_line_work: metrics.near_line_work.into(),
            near_paired_interval_work: metrics.near_paired_interval_work.into(),
            near_paired_cross_interval_veto_work: metrics
                .near_paired_cross_interval_veto_work
                .into(),
            near_same_or_ambiguous_span_work: metrics.near_same_or_ambiguous_span_work.into(),
            near_same_known_span_work: metrics.near_same_known_span_work.into(),
            near_ambiguous_span_work: metrics.near_ambiguous_span_work.into(),
            near_same_or_ambiguous_shared_query_work: metrics
                .near_same_or_ambiguous_shared_query_work
                .into(),
            near_cross_span_work: metrics.near_cross_span_work.into(),
            known_span_sentence_shadow: metrics.known_span_sentence_shadow.map(Into::into),
            sentence_edge_gate_shadow: metrics.sentence_edge_gate_shadow.map(Into::into),
            near_pair_visits_examined: metrics.near_pair_visits_examined,
            near_pair_visits_attempted: metrics.near_pair_visits_attempted,
            near_similarity_comparisons_examined: metrics.near_similarity_comparisons_examined,
            near_similarity_comparisons_attempted: metrics.near_similarity_comparisons_attempted,
            near_candidate_posting_visits_examined: metrics.near_candidate_posting_visits_examined,
            near_candidate_posting_visits_attempted: metrics
                .near_candidate_posting_visits_attempted,
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
pub enum ScopeCompleteness {
    Complete,
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

    fn has_recovery_watch_quotes(self, old_quote: Option<&str>, new_quote: Option<&str>) -> bool {
        match self {
            Self::Replacement | Self::Move => old_quote.is_some() && new_quote.is_some(),
            Self::Insertion => old_quote.is_none() && new_quote.is_some(),
            Self::Deletion => old_quote.is_some() && new_quote.is_none(),
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
    /// Exact number of occurrences expected in one semantic change event.
    #[serde(default)]
    pub occurrence_count: Option<usize>,
    /// Within a complete scope, this is the exact expected changed span on
    /// the old side, not surrounding context used only for identification.
    #[serde(default)]
    pub old_quote: Option<String>,
    /// Within a complete scope, this is the exact expected changed span on
    /// the new side, not surrounding context used only for identification.
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
    #[serde(default)]
    pub completeness: Option<ScopeCompleteness>,
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

#[derive(Debug, Default)]
struct RecoveryWatchQuerySet {
    ids: Vec<String>,
    expected_indices: Vec<usize>,
}

impl RecoveryWatchQuerySet {
    fn new(document: Option<&ExpectedDocument>) -> Self {
        let Some(document) = document.filter(|document| {
            matches!(
                document.annotation,
                Annotation::Complete | Annotation::Partial
            )
        }) else {
            return Self::default();
        };
        let expected_indices = document
            .changes
            .iter()
            .take(MAX_EXPECTED_CHANGE_DIAGNOSTICS)
            .enumerate()
            .filter_map(|(index, change)| {
                change
                    .kind
                    .has_recovery_watch_quotes(
                        change.old_quote.as_deref(),
                        change.new_quote.as_deref(),
                    )
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let ids = expected_indices
            .iter()
            .map(|index| format!("expected-change-{index}"))
            .collect();
        Self {
            ids,
            expected_indices,
        }
    }

    fn queries<'a>(&'a self, changes: &'a [ExpectedChange]) -> Vec<RecoveryWatchQuery<'a>> {
        self.ids
            .iter()
            .zip(&self.expected_indices)
            .map(|(id, &index)| {
                let change = &changes[index];
                RecoveryWatchQuery {
                    id,
                    old_quote: change.old_quote.as_deref(),
                    new_quote: change.new_quote.as_deref(),
                }
            })
            .collect()
    }
}

struct MatchOutcome {
    matched: usize,
    kind_agreements: usize,
    claimed_actuals: HashSet<usize>,
    claimed_actual_by_expected: Vec<Option<usize>>,
    occurrence_count_mismatch_by_expected: Vec<Option<(usize, usize)>>,
}

#[derive(Clone, Copy, Debug)]
struct MatchingLimits {
    max_candidate_edges: usize,
    max_edge_checks: usize,
    max_events_per_side: usize,
    max_residual_edge_visits: usize,
    max_augmentations: usize,
    max_occurrence_visits: usize,
    max_text_bytes: usize,
}

impl Default for MatchingLimits {
    fn default() -> Self {
        Self {
            max_candidate_edges: MAX_MATCH_CANDIDATE_EDGES,
            max_edge_checks: MAX_MATCH_EDGE_CHECKS,
            max_events_per_side: MAX_MATCH_EVENTS_PER_SIDE,
            max_residual_edge_visits: MAX_MATCH_RESIDUAL_EDGE_VISITS,
            max_augmentations: MAX_MATCH_AUGMENTATIONS,
            max_occurrence_visits: MAX_MATCH_OCCURRENCE_VISITS,
            max_text_bytes: MAX_MATCH_TEXT_BYTES,
        }
    }
}

#[derive(Default)]
struct MatchingScanBudget {
    occurrence_visits: usize,
    text_bytes: usize,
}

impl MatchingScanBudget {
    fn charge_expected_quotes(
        &mut self,
        change: &ExpectedChange,
        limits: MatchingLimits,
    ) -> MatchingResult<()> {
        self.charge_optional_texts(
            change.old_quote.as_deref(),
            change.new_quote.as_deref(),
            limits,
        )
    }

    fn charge_occurrence(
        &mut self,
        occurrence: &ActualChangeOccurrence,
        limits: MatchingLimits,
    ) -> MatchingResult<()> {
        self.occurrence_visits = self
            .occurrence_visits
            .checked_add(1)
            .filter(|visits| *visits <= limits.max_occurrence_visits)
            .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        self.charge_optional_texts(
            occurrence.old_text.as_deref(),
            occurrence.new_text.as_deref(),
            limits,
        )
    }

    fn charge_optional_texts(
        &mut self,
        old: Option<&str>,
        new: Option<&str>,
        limits: MatchingLimits,
    ) -> MatchingResult<()> {
        let bytes = old
            .map_or(0, str::len)
            .checked_add(new.map_or(0, str::len))
            .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        self.charge_text(bytes, limits)
    }

    fn charge_text(&mut self, bytes: usize, limits: MatchingLimits) -> MatchingResult<()> {
        self.text_bytes = self
            .text_bytes
            .checked_add(bytes)
            .filter(|total| *total <= limits.max_text_bytes)
            .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        Ok(())
    }
}

struct NormalizedExpectedQuotes {
    old: Option<String>,
    new: Option<String>,
}

type MatchingResult<T> = std::result::Result<T, &'static str>;

/// Lexicographic assignment cost: kind mismatch, unconstrained occurrence
/// count, crossing distance, expected order, then actual order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct MatchCost([i128; 5]);

impl MatchCost {
    fn add(self, other: Self) -> Self {
        Self(std::array::from_fn(|index| self.0[index] + other.0[index]))
    }

    fn negated(self) -> Self {
        Self(self.0.map(|value| -value))
    }
}

#[derive(Clone, Copy, Debug)]
struct MatchFlowEdge {
    to: usize,
    reverse: usize,
    available: bool,
    cost: MatchCost,
}

fn add_match_flow_edge(
    graph: &mut [Vec<MatchFlowEdge>],
    from: usize,
    to: usize,
    cost: MatchCost,
) -> MatchingResult<()> {
    graph[from]
        .try_reserve(1)
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    graph[to]
        .try_reserve(1)
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    let forward_reverse = graph[to].len();
    let reverse_reverse = graph[from].len();
    graph[from].push(MatchFlowEdge {
        to,
        reverse: forward_reverse,
        available: true,
        cost,
    });
    graph[to].push(MatchFlowEdge {
        to: from,
        reverse: reverse_reverse,
        available: false,
        cost: cost.negated(),
    });
    Ok(())
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
        Annotation::Complete if !document.scopes.is_empty() => {
            return Err(BenchError::InvalidInput(
                "expected-revision JSON complete annotations forbid scopes".to_owned(),
            ));
        }
        Annotation::Partial
            if document
                .scopes
                .iter()
                .any(|scope| scope.completeness != Some(ScopeCompleteness::Complete)) =>
        {
            return Err(BenchError::InvalidInput(
                "expected-revision JSON partial annotation scopes require explicit \"completeness\": \"complete\""
                    .to_owned(),
            ));
        }
        _ => {}
    }
    let mut seen_ids = HashSet::new();
    if document.changes.len() > MAX_EXPECTED_DOCUMENT_CHANGES {
        return Err(BenchError::InvalidInput(format!(
            "expected-revision JSON has {} changes; at most {MAX_EXPECTED_DOCUMENT_CHANGES} are supported",
            document.changes.len()
        )));
    }
    for change in &document.changes {
        if change.id.trim().is_empty() || !seen_ids.insert(change.id.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON change id {:?} is blank or duplicated",
                change.id
            )));
        }
        if change.occurrence_count == Some(0) {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON change {} has occurrence_count 0; a positive count is required",
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
            Annotation::Partial => match change.scope.as_deref() {
                Some(scope) if scope_ids.contains(scope) => {}
                Some(scope) => {
                    return Err(BenchError::InvalidInput(format!(
                        "expected-revision JSON change {} references unknown scope {scope:?}",
                        change.id
                    )));
                }
                None => {}
            },
            Annotation::Complete if change.scope.is_some() => {
                return Err(BenchError::InvalidInput(format!(
                    "expected-revision JSON change {} cannot reference a scope for complete annotation",
                    change.id
                )));
            }
            Annotation::Complete => {}
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

fn load_pair_expected_document(
    pair: &RevisionPair,
    manifest_dir: &Path,
) -> std::result::Result<Option<ExpectedDocument>, String> {
    let Some(file) = pair.expected_file.as_ref() else {
        return Ok(None);
    };
    let path = manifest_dir.join(file);
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read expected annotations {}: {error}",
            path.display()
        )
    })?;
    let document = load_expected_document(&text).map_err(|error| error.to_string())?;
    if document.pair != pair.pair_id {
        return Err(format!(
            "expected annotations {} describe pair {:?} but this manifest row is {:?}",
            path.display(),
            document.pair,
            pair.pair_id
        ));
    }
    Ok(Some(document))
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

fn scope_matches(
    expected: &ExpectedChange,
    actual_index: usize,
    actual_scopes: Option<&[Option<String>]>,
) -> bool {
    !expected.scope.as_deref().is_some_and(|expected_scope| {
        actual_scopes.is_some_and(|scopes| {
            scopes.get(actual_index).and_then(Option::as_deref) != Some(expected_scope)
        })
    })
}

fn occurrence_matches_quotes(
    occurrence: &ActualChangeOccurrence,
    needle_old: Option<&str>,
    needle_new: Option<&str>,
) -> bool {
    occurrence.resolvable
        && contains_needle(occurrence.old_text.as_deref(), needle_old)
        && contains_needle(occurrence.new_text.as_deref(), needle_new)
}

fn normalized_expected_quotes(change: &ExpectedChange) -> NormalizedExpectedQuotes {
    NormalizedExpectedQuotes {
        old: change.old_quote.as_deref().map(collapse_whitespace),
        new: change.new_quote.as_deref().map(collapse_whitespace),
    }
}

fn actual_matches_quotes(
    actual: &ActualChange,
    needles: &NormalizedExpectedQuotes,
    all: bool,
    budget: &mut MatchingScanBudget,
    limits: MatchingLimits,
) -> MatchingResult<bool> {
    if all {
        if actual.occurrences.is_empty() {
            return Ok(false);
        }
        for occurrence in &actual.occurrences {
            budget.charge_occurrence(occurrence, limits)?;
            if !occurrence_matches_quotes(
                occurrence,
                needles.old.as_deref(),
                needles.new.as_deref(),
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    } else {
        for occurrence in &actual.occurrences {
            budget.charge_occurrence(occurrence, limits)?;
            if occurrence_matches_quotes(occurrence, needles.old.as_deref(), needles.new.as_deref())
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn actual_matches_expected(
    change: &ExpectedChange,
    needles: &NormalizedExpectedQuotes,
    actual: &ActualChange,
    budget: &mut MatchingScanBudget,
    limits: MatchingLimits,
) -> MatchingResult<(bool, bool)> {
    match change.occurrence_count {
        Some(count) => {
            let quotes_match = actual_matches_quotes(actual, needles, true, budget, limits)?;
            Ok((
                quotes_match && actual.occurrences.len() == count,
                quotes_match,
            ))
        }
        None => Ok((
            actual_matches_quotes(actual, needles, false, budget, limits)?,
            false,
        )),
    }
}

fn preferred_maximum_matching(
    edges: &[Vec<(usize, MatchCost)>],
    actual_count: usize,
    limits: MatchingLimits,
) -> MatchingResult<Vec<Option<usize>>> {
    let source = 0;
    let expected_start = 1;
    let actual_start = expected_start + edges.len();
    let sink = actual_start + actual_count;
    let mut graph = Vec::new();
    graph
        .try_reserve_exact(sink + 1)
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    graph.resize_with(sink + 1, Vec::new);
    for (expected_index, expected_edges) in edges.iter().enumerate() {
        add_match_flow_edge(
            &mut graph,
            source,
            expected_start + expected_index,
            MatchCost::default(),
        )?;
        for &(actual_index, cost) in expected_edges {
            add_match_flow_edge(
                &mut graph,
                expected_start + expected_index,
                actual_start + actual_index,
                cost,
            )?;
        }
    }
    for actual_index in 0..actual_count {
        add_match_flow_edge(
            &mut graph,
            actual_start + actual_index,
            sink,
            MatchCost::default(),
        )?;
    }

    let mut residual_edge_visits = 0_usize;
    let mut augmentations = 0_usize;
    loop {
        let mut distances = Vec::new();
        distances
            .try_reserve_exact(graph.len())
            .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        distances.resize(graph.len(), None);
        let mut parents = Vec::new();
        parents
            .try_reserve_exact(graph.len())
            .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        parents.resize(graph.len(), None);
        let mut queued = Vec::new();
        queued
            .try_reserve_exact(graph.len())
            .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        queued.resize(graph.len(), false);
        let mut queue = VecDeque::new();
        queue
            .try_reserve(graph.len())
            .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        queue.push_back(source);
        distances[source] = Some(MatchCost::default());
        queued[source] = true;
        while let Some(node) = queue.pop_front() {
            queued[node] = false;
            let distance = distances[node].expect("queued nodes have a distance");
            for (edge_index, edge) in graph[node].iter().enumerate() {
                residual_edge_visits = residual_edge_visits
                    .checked_add(1)
                    .filter(|visits| *visits <= limits.max_residual_edge_visits)
                    .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
                if !edge.available {
                    continue;
                }
                let candidate = distance.add(edge.cost);
                if distances[edge.to].is_none_or(|current| candidate < current) {
                    distances[edge.to] = Some(candidate);
                    parents[edge.to] = Some((node, edge_index));
                    if !queued[edge.to] {
                        queue.push_back(edge.to);
                        queued[edge.to] = true;
                    }
                }
            }
        }
        if distances[sink].is_none() {
            break;
        }
        augmentations = augmentations
            .checked_add(1)
            .filter(|count| *count <= limits.max_augmentations)
            .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
        let mut node = sink;
        while node != source {
            let (parent, edge_index) = parents[node].expect("reachable nodes have a parent");
            let reverse = graph[parent][edge_index].reverse;
            graph[parent][edge_index].available = false;
            graph[node][reverse].available = true;
            node = parent;
        }
    }

    let mut matching = Vec::new();
    matching
        .try_reserve_exact(edges.len())
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    matching.extend((0..edges.len()).map(|expected_index| {
        graph[expected_start + expected_index]
            .iter()
            .find(|edge| edge.to >= actual_start && edge.to < sink && !edge.available)
            .map(|edge| edge.to - actual_start)
    }));
    Ok(matching)
}

fn match_changes(expected: &[ExpectedChange], actuals: &[ActualChange]) -> MatchOutcome {
    match_changes_with_scopes(expected, actuals, None)
}

fn match_changes_with_scopes(
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: Option<&[Option<String>]>,
) -> MatchOutcome {
    try_match_changes_with_scopes(expected, actuals, actual_scopes)
        .expect("test and fixture matching stays within default resource limits")
}

fn try_match_changes_with_scopes(
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: Option<&[Option<String>]>,
) -> MatchingResult<MatchOutcome> {
    match_changes_with_limits(expected, actuals, actual_scopes, MatchingLimits::default())
}

fn match_changes_with_limits(
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: Option<&[Option<String>]>,
    limits: MatchingLimits,
) -> MatchingResult<MatchOutcome> {
    if expected.len() > limits.max_events_per_side || actuals.len() > limits.max_events_per_side {
        return Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT);
    }
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(expected.len())
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    let mut edge_checks = 0_usize;
    let mut candidate_edges = 0_usize;
    let mut scan_budget = MatchingScanBudget::default();
    let mut mismatch_candidates = Vec::new();
    mismatch_candidates
        .try_reserve_exact(expected.len())
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    for (expected_index, change) in expected.iter().enumerate() {
        scan_budget.charge_expected_quotes(change, limits)?;
        let needles = normalized_expected_quotes(change);
        let mut expected_edges = Vec::new();
        let mut expected_mismatches = Vec::new();
        for (actual_index, actual) in actuals.iter().enumerate() {
            edge_checks = edge_checks
                .checked_add(1)
                .filter(|checks| *checks <= limits.max_edge_checks)
                .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
            if !scope_matches(change, actual_index, actual_scopes) {
                continue;
            }
            let (matches, quotes_match) =
                actual_matches_expected(change, &needles, actual, &mut scan_budget, limits)?;
            if matches {
                candidate_edges = candidate_edges
                    .checked_add(1)
                    .filter(|edges| *edges <= limits.max_candidate_edges)
                    .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
                expected_edges
                    .try_reserve(1)
                    .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
                expected_edges.push({
                    let distance = expected_index.abs_diff(actual_index) as i128;
                    (
                        actual_index,
                        MatchCost([
                            i128::from(!change.kind.agrees_with(actual.kind)),
                            i128::from(change.occurrence_count.is_none()),
                            distance,
                            expected_index as i128,
                            actual_index as i128,
                        ]),
                    )
                });
            } else if quotes_match
                && change.kind.agrees_with(actual.kind)
                && change.occurrence_count != Some(actual.occurrences.len())
            {
                candidate_edges = candidate_edges
                    .checked_add(1)
                    .filter(|edges| *edges <= limits.max_candidate_edges)
                    .ok_or(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
                expected_mismatches
                    .try_reserve(1)
                    .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
                expected_mismatches.push((actual_index, actual.occurrences.len()));
            }
        }
        edges.push(expected_edges);
        mismatch_candidates.push(expected_mismatches);
    }
    let claimed_actual_by_expected = preferred_maximum_matching(&edges, actuals.len(), limits)?;
    let mut claimed_actuals = HashSet::new();
    claimed_actuals
        .try_reserve(claimed_actual_by_expected.len())
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    claimed_actuals.extend(claimed_actual_by_expected.iter().flatten().copied());
    let kind_agreements = claimed_actual_by_expected
        .iter()
        .enumerate()
        .filter(|(expected_index, actual_index)| {
            actual_index.is_some_and(|actual_index| {
                expected[*expected_index]
                    .kind
                    .agrees_with(actuals[actual_index].kind)
            })
        })
        .count();
    let mut occurrence_count_mismatch_by_expected = Vec::new();
    occurrence_count_mismatch_by_expected
        .try_reserve_exact(expected.len())
        .map_err(|_| QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)?;
    occurrence_count_mismatch_by_expected.extend(expected.iter().zip(mismatch_candidates).map(
        |(change, candidates)| {
            let expected_count = change.occurrence_count?;
            candidates
                .into_iter()
                .find(|(actual_index, _)| !claimed_actuals.contains(actual_index))
                .map(|(_, actual_count)| (expected_count, actual_count))
        },
    ));
    Ok(MatchOutcome {
        matched: claimed_actuals.len(),
        kind_agreements,
        claimed_actuals,
        claimed_actual_by_expected,
        occurrence_count_mismatch_by_expected,
    })
}

#[cfg(test)]
fn scoped_quality(
    reviewed_scope_count: usize,
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: &[Option<String>],
) -> (QualityMetrics, ScopedEventMetrics) {
    try_scoped_quality(reviewed_scope_count, expected, actuals, actual_scopes)
        .expect("test and fixture matching stays within default resource limits")
}

fn try_scoped_quality(
    reviewed_scope_count: usize,
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    actual_scopes: &[Option<String>],
) -> MatchingResult<(QualityMetrics, ScopedEventMetrics)> {
    let outcome = try_match_changes_with_scopes(expected, actuals, Some(actual_scopes))?;
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
    Ok((
        quality,
        ScopedEventMetrics {
            reviewed_scope_count,
            precision,
            recall,
            f1,
        },
    ))
}

struct CompleteScopeEvaluation {
    quality: QualityMetrics,
    event_metrics: ScopedEventMetrics,
    token_metrics: ScopedTokenMetrics,
    actual_scopes: Vec<Option<String>>,
}

fn evaluate_complete_scopes(
    document: &ExpectedDocument,
    comparison: &Comparison,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    actuals: &[ActualChange],
) -> std::result::Result<CompleteScopeEvaluation, String> {
    let scopes = resolve_revision_scopes(&document.scopes, old_blocks, new_blocks)?;
    let expected = document
        .changes
        .iter()
        .filter(|change| change.scope.is_some())
        .cloned()
        .collect::<Vec<_>>();
    let expected_tokens =
        validate_scoped_expected_changes(&expected, &scopes, old_blocks, new_blocks)?;
    let changes = classify_scoped_changes(&comparison.changes, &scopes, old_blocks, new_blocks)?;
    let token_metrics = evaluate_scoped_token_metrics(
        &comparison.changes,
        &changes,
        expected_tokens,
        &scopes,
        old_blocks,
        new_blocks,
    )?;
    let mut all_actual_scopes = vec![None; actuals.len()];
    let mut scoped_actuals = Vec::with_capacity(changes.len());
    let mut scoped_actual_scopes = Vec::with_capacity(changes.len());
    for change in &changes {
        let Some(actual) = actuals.get(change.change_index) else {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        };
        all_actual_scopes[change.change_index] = Some(change.scope_id.clone());
        scoped_actuals.push(actual.clone());
        scoped_actual_scopes.push(Some(change.scope_id.clone()));
    }
    let (quality, event_metrics) = try_scoped_quality(
        scopes.len(),
        &expected,
        &scoped_actuals,
        &scoped_actual_scopes,
    )
    .map_err(str::to_owned)?;
    Ok(CompleteScopeEvaluation {
        quality,
        event_metrics,
        token_metrics,
        actual_scopes: all_actual_scopes,
    })
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

    let expected = match load_pair_expected_document(pair, context.manifest_dir) {
        Ok(document) => document,
        Err(reason) => {
            record.failure = Some(reason);
            None
        }
    };
    let recovery_watch_queries = RecoveryWatchQuerySet::new(expected.as_ref());
    let queries = expected
        .as_ref()
        .map(|document| recovery_watch_queries.queries(&document.changes))
        .unwrap_or_default();

    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let (outcome, alignment, recovery_watch_diagnostics) = match run_extraction_and_comparison(
        &source,
        &old_path,
        &new_path,
        effective_scale,
        &queries,
    ) {
        Ok(ComparisonWithMetrics {
            outcome,
            alignment,
            metrics,
            sentence_recovery_metrics,
            pressure,
            recovery_watch_diagnostics,
        }) => {
            metrics.apply_to(&mut record);
            record.sentence_recovery_metrics = sentence_recovery_metrics;
            record.candidate_visit_pressure = pressure;
            (outcome, alignment, recovery_watch_diagnostics)
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
            match evaluate_complete_scopes(
                &document,
                &outcome.comparison,
                &outcome.old_blocks,
                &outcome.new_blocks,
                actuals.as_deref().unwrap_or_default(),
            ) {
                Ok(scoped) => {
                    record.quality = Some(scoped.quality);
                    record.scoped_event_metrics = Some(scoped.event_metrics);
                    record.scoped_token_metrics = Some(scoped.token_metrics);
                }
                Err(reason) => record.quality_skipped_reason = Some(reason),
            }
        }
        (Some(document), true) => {
            let actuals = actuals.unwrap_or_default();
            let needs_scopes =
                document.annotation == Annotation::Partial && !document.scopes.is_empty();
            let scoped = needs_scopes
                .then(|| {
                    evaluate_complete_scopes(
                        &document,
                        &outcome.comparison,
                        &outcome.old_blocks,
                        &outcome.new_blocks,
                        &actuals,
                    )
                })
                .transpose();
            let matched = scoped.and_then(|scoped| {
                let match_outcome = try_match_changes_with_scopes(
                    &document.changes,
                    &actuals,
                    scoped
                        .as_ref()
                        .map(|evaluation| evaluation.actual_scopes.as_slice()),
                )
                .map_err(str::to_owned)?;
                Ok((scoped, match_outcome))
            });
            match matched {
                Ok((scoped, match_outcome)) => {
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
                        ComparisonDiagnosticInput {
                            alignment: alignment.as_ref(),
                            comparison: &outcome.comparison,
                            actual_scopes: scoped
                                .as_ref()
                                .map(|evaluation| evaluation.actual_scopes.as_slice()),
                        },
                        &actuals,
                        &match_outcome,
                    ) {
                        Ok(diagnostics) => {
                            record.candidate_recall = diagnostics.candidate_recall;
                            let mut expected_diagnostics = diagnostics.expected_change_diagnostics;
                            expected_diagnostics.recovery_watch = match recovery_watch_diagnostics {
                                Some(watch) => Some(recovery_watch_report(
                                    &document.changes,
                                    &recovery_watch_queries,
                                    watch,
                                )),
                                None if recovery_watch_queries.ids.is_empty()
                                    && expected_diagnostics.complete =>
                                {
                                    Some(completed_empty_recovery_watch_report())
                                }
                                None => None,
                            };
                            record.expected_change_diagnostics = Some(expected_diagnostics);
                        }
                        Err(reason) if record.failure.is_none() => {
                            record.failure = Some(format!("reviewed diagnostics failed: {reason}"));
                        }
                        Err(_) => {}
                    }
                    if let Some(scoped) = scoped {
                        record.scoped_event_metrics = Some(scoped.event_metrics);
                        record.scoped_token_metrics = Some(scoped.token_metrics);
                    }
                }
                Err(reason) => record.quality_skipped_reason = Some(reason),
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
    validate_run_signature_metrics(metrics)?;
    validate_near_search_work_metrics(metrics)?;
    validate_known_span_sentence_shadow_metrics(metrics)?;
    validate_sentence_edge_gate_shadow_metrics(metrics)?;
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
    if metrics.near_candidate_posting_visits_examined
        > metrics.near_candidate_posting_visits_attempted
    {
        return Err(format!(
            "examined near candidate posting visits {} exceed attempted visits {}",
            metrics.near_candidate_posting_visits_examined,
            metrics.near_candidate_posting_visits_attempted
        ));
    }
    let pair_visit_deficit = metrics.near_pair_visits_examined < metrics.near_pair_visits_attempted;
    let comparison_deficit = metrics.near_similarity_comparisons_examined
        < metrics.near_similarity_comparisons_attempted;
    let posting_visit_deficit = metrics.near_candidate_posting_visits_examined
        < metrics.near_candidate_posting_visits_attempted;
    if usize::from(pair_visit_deficit)
        + usize::from(comparison_deficit)
        + usize::from(posting_visit_deficit)
        > 1
    {
        return Err("near relation has multiple unfinished work counters".to_owned());
    }
    if metrics.near_relation_complete {
        if metrics.near_candidate_count_truncated {
            return Err("complete near relation has truncated candidates".to_owned());
        }
        if pair_visit_deficit || comparison_deficit || posting_visit_deficit {
            return Err("complete near relation has unexamined work".to_owned());
        }
        if metrics.near_relation_stop_reason.is_some() {
            return Err("complete near relation has a stop reason".to_owned());
        }
    }
    match (
        metrics.near_relation_stop_reason,
        posting_visit_deficit,
        pair_visit_deficit,
        comparison_deficit,
    ) {
        (None, true, _, _) | (None, _, true, _) | (None, _, _, true) => {
            return Err(
                "incomplete near relation has unexamined work without a stop reason".to_owned(),
            );
        }
        (Some(NearRelationStopReason::CandidatePostingVisitLimit), true, false, false) => {}
        (Some(NearRelationStopReason::CandidatePostingVisitLimit), _, _, _) => {
            return Err(
                "candidate posting visit stop reason does not match unfinished work".to_owned(),
            );
        }
        (Some(NearRelationStopReason::PairVisitLimit), false, true, false) => {}
        (Some(NearRelationStopReason::PairVisitLimit), _, _, _) => {
            return Err("pair visit stop reason does not match unfinished work".to_owned());
        }
        (Some(NearRelationStopReason::SimilarityComparisonLimit), false, false, true) => {}
        (Some(NearRelationStopReason::SimilarityComparisonLimit), _, _, _) => {
            return Err(
                "similarity comparison stop reason does not match unfinished work".to_owned(),
            );
        }
        (Some(NearRelationStopReason::CandidateCountLimit), false, false, false)
            if metrics.near_candidate_count_truncated => {}
        (Some(NearRelationStopReason::CandidateCountLimit), _, _, _) => {
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
    if metrics.relation_floor_word_scans > metrics.relation_floor_pairs_considered {
        return Err(format!(
            "relation-floor word scans {} exceed considered pairs {}",
            metrics.relation_floor_word_scans, metrics.relation_floor_pairs_considered
        ));
    }
    if metrics.relation_floor_stop_opportunities > metrics.relation_floor_word_scans {
        return Err(format!(
            "relation-floor stop opportunities {} exceed word scans {}",
            metrics.relation_floor_stop_opportunities, metrics.relation_floor_word_scans
        ));
    }
    if metrics.relation_floor_stop_opportunities == 0
        && metrics.relation_floor_potential_saved_word_comparisons != 0
    {
        return Err("relation-floor saved word comparisons require a stop opportunity".to_owned());
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

fn validate_known_span_sentence_shadow_metrics(
    metrics: SentenceRecoveryMetrics,
) -> std::result::Result<(), String> {
    let Some(shadow) = metrics.known_span_sentence_shadow else {
        return Ok(());
    };
    if shadow.pairs_retained.checked_add(shadow.pairs_rejected) != Some(shadow.pairs_considered) {
        return Err(format!(
            "known-span sentence shadow pair counters do not sum: retained {} + rejected {} != considered {}",
            shadow.pairs_retained, shadow.pairs_rejected, shadow.pairs_considered
        ));
    }
    if shadow.cross_span_pairs_considered > shadow.pairs_considered {
        return Err(format!(
            "known-span sentence shadow cross-span pairs {} exceed considered pairs {}",
            shadow.cross_span_pairs_considered, shadow.pairs_considered
        ));
    }
    if shadow
        .same_paired_anchor_interval_pairs
        .checked_add(shadow.same_paired_stream_other_interval_pairs)
        .and_then(|pairs| pairs.checked_add(shadow.same_page_only_pairs))
        .and_then(|pairs| pairs.checked_add(shadow.unclassified_pairs))
        != Some(shadow.cross_span_pairs_considered)
    {
        return Err("known-span sentence shadow locality counters do not sum".to_owned());
    }
    let exact_parity = shadow.old_relation_mismatches == 0 && shadow.new_relation_mismatches == 0;
    if shadow.exact_relation_parity != exact_parity {
        return Err(
            "known-span sentence shadow exact parity contradicts relation mismatches".to_owned(),
        );
    }
    Ok(())
}

fn validate_sentence_edge_gate_shadow_metrics(
    metrics: SentenceRecoveryMetrics,
) -> std::result::Result<(), String> {
    let Some(shadow) = metrics.sentence_edge_gate_shadow else {
        return Ok(());
    };
    if shadow.pairs_retained.checked_add(shadow.pairs_rejected) != Some(shadow.pairs_considered) {
        return Err(format!(
            "sentence-edge gate shadow pair counters do not sum: retained {} + rejected {} != considered {}",
            shadow.pairs_retained, shadow.pairs_rejected, shadow.pairs_considered
        ));
    }
    if shadow
        .same_known_rejected
        .checked_add(shadow.ambiguous_rejected)
        .and_then(|count| count.checked_add(shadow.cross_span_rejected))
        .and_then(|count| count.checked_add(shadow.unclassified_rejected))
        != Some(shadow.pairs_rejected)
    {
        return Err("sentence-edge gate shadow rejection counters do not sum".to_owned());
    }
    if shadow.projected_pair_visits != shadow.pairs_retained {
        return Err(format!(
            "sentence-edge gate shadow projected pair visits {} differ from retained pairs {}",
            shadow.projected_pair_visits, shadow.pairs_retained
        ));
    }
    if shadow.projected_similarity_comparisons > metrics.near_similarity_comparisons_examined {
        return Err(format!(
            "sentence-edge gate shadow projected similarity comparisons {} exceed examined comparisons {}",
            shadow.projected_similarity_comparisons, metrics.near_similarity_comparisons_examined
        ));
    }
    if shadow.complete != shadow.stop_reason.is_none() {
        return Err("sentence-edge gate shadow completeness contradicts stop reason".to_owned());
    }
    Ok(())
}

fn validate_near_search_work_metrics(
    metrics: SentenceRecoveryMetrics,
) -> std::result::Result<(), String> {
    for (kind, work, allow_line_trigram) in [
        ("sentence", metrics.near_sentence_work, false),
        ("line", metrics.near_line_work, true),
    ] {
        validate_near_search_work_entry(kind, work, allow_line_trigram)?;
    }

    let scopes = [
        ("paired interval", metrics.near_paired_interval_work),
        (
            "paired cross-interval veto",
            metrics.near_paired_cross_interval_veto_work,
        ),
        (
            "same or ambiguous span",
            metrics.near_same_or_ambiguous_span_work,
        ),
        ("cross span", metrics.near_cross_span_work),
    ];
    for (scope, work) in scopes {
        validate_near_search_work_entry(&format!("{scope} sentence"), work.sentence_work, false)?;
        validate_near_search_work_entry(&format!("{scope} line"), work.line_work, true)?;
    }
    let same_or_ambiguous_subscopes = [
        ("same known span", metrics.near_same_known_span_work),
        ("ambiguous span", metrics.near_ambiguous_span_work),
        (
            "same or ambiguous shared query",
            metrics.near_same_or_ambiguous_shared_query_work,
        ),
    ];
    for (scope, work) in same_or_ambiguous_subscopes {
        validate_near_search_work_entry(&format!("{scope} sentence"), work.sentence_work, false)?;
        validate_near_search_work_entry(&format!("{scope} line"), work.line_work, true)?;
    }
    let same_or_ambiguous_sentence_sum = sum_near_search_work_metrics(
        same_or_ambiguous_subscopes.map(|(_, work)| work.sentence_work),
        "same or ambiguous sentence subscope",
    )?;
    let same_or_ambiguous_line_sum = sum_near_search_work_metrics(
        same_or_ambiguous_subscopes.map(|(_, work)| work.line_work),
        "same or ambiguous line subscope",
    )?;
    if same_or_ambiguous_sentence_sum != metrics.near_same_or_ambiguous_span_work.sentence_work
        || same_or_ambiguous_line_sum != metrics.near_same_or_ambiguous_span_work.line_work
    {
        return Err(
            "near-search same-or-ambiguous subscope counters disagree with parent counters"
                .to_owned(),
        );
    }
    let sentence_scope_sum =
        sum_near_search_work_metrics(scopes.map(|(_, work)| work.sentence_work), "sentence scope")?;
    let line_scope_sum =
        sum_near_search_work_metrics(scopes.map(|(_, work)| work.line_work), "line scope")?;
    if sentence_scope_sum != metrics.near_sentence_work || line_scope_sum != metrics.near_line_work
    {
        return Err("near-search scope counters disagree with kind counters".to_owned());
    }

    let sum = |values: [usize; 2], label: &str| {
        values[0]
            .checked_add(values[1])
            .ok_or_else(|| format!("near-search {label} counters overflow"))
    };
    let posting_examined = sum(
        [
            metrics.near_sentence_work.edge_posting_visits_examined,
            metrics
                .near_line_work
                .edge_posting_visits_examined
                .checked_add(metrics.near_line_work.line_trigram_posting_visits_examined)
                .ok_or_else(|| "near-search posting counters overflow".to_owned())?,
        ],
        "posting",
    )?;
    let posting_attempted = sum(
        [
            metrics.near_sentence_work.edge_posting_visits_attempted,
            metrics
                .near_line_work
                .edge_posting_visits_attempted
                .checked_add(metrics.near_line_work.line_trigram_posting_visits_attempted)
                .ok_or_else(|| "near-search posting counters overflow".to_owned())?,
        ],
        "posting",
    )?;
    let pair_examined = sum(
        [
            metrics.near_sentence_work.pair_visits_examined,
            metrics.near_line_work.pair_visits_examined,
        ],
        "pair visit",
    )?;
    let pair_attempted = sum(
        [
            metrics.near_sentence_work.pair_visits_attempted,
            metrics.near_line_work.pair_visits_attempted,
        ],
        "pair visit",
    )?;
    let comparison_examined = sum(
        [
            metrics.near_sentence_work.similarity_comparisons_examined,
            metrics.near_line_work.similarity_comparisons_examined,
        ],
        "similarity comparison",
    )?;
    let comparison_attempted = sum(
        [
            metrics.near_sentence_work.similarity_comparisons_attempted,
            metrics.near_line_work.similarity_comparisons_attempted,
        ],
        "similarity comparison",
    )?;
    if posting_examined != metrics.near_candidate_posting_visits_examined
        || posting_attempted != metrics.near_candidate_posting_visits_attempted
        || pair_examined != metrics.near_pair_visits_examined
        || pair_attempted != metrics.near_pair_visits_attempted
        || comparison_examined != metrics.near_similarity_comparisons_examined
        || comparison_attempted != metrics.near_similarity_comparisons_attempted
    {
        return Err("near-search kind counters disagree with aggregate counters".to_owned());
    }
    Ok(())
}

fn validate_near_search_work_entry(
    label: &str,
    work: NearSearchWorkMetrics,
    allow_line_trigram: bool,
) -> std::result::Result<(), String> {
    if work.edge_posting_visits_examined > work.edge_posting_visits_attempted
        || work.line_trigram_posting_visits_examined > work.line_trigram_posting_visits_attempted
        || work.pair_visits_examined > work.pair_visits_attempted
        || work.similarity_comparisons_examined > work.similarity_comparisons_attempted
    {
        return Err(format!(
            "{label} near-search examined work exceeds attempted work"
        ));
    }
    if !allow_line_trigram
        && (work.line_trigram_posting_visits_examined != 0
            || work.line_trigram_posting_visits_attempted != 0
            || work.line_trigram_only_query_union_candidates != 0)
    {
        return Err(format!("{label} near-search has line-trigram work"));
    }
    Ok(())
}

fn sum_near_search_work_metrics<const N: usize>(
    metrics: [NearSearchWorkMetrics; N],
    label: &str,
) -> std::result::Result<NearSearchWorkMetrics, String> {
    metrics
        .into_iter()
        .try_fold(NearSearchWorkMetrics::default(), |sum, work| {
            let add = |left: usize, right: usize| {
                left.checked_add(right)
                    .ok_or_else(|| format!("near-search {label} counters overflow"))
            };
            Ok(NearSearchWorkMetrics {
                edge_posting_visits_examined: add(
                    sum.edge_posting_visits_examined,
                    work.edge_posting_visits_examined,
                )?,
                edge_posting_visits_attempted: add(
                    sum.edge_posting_visits_attempted,
                    work.edge_posting_visits_attempted,
                )?,
                line_trigram_posting_visits_examined: add(
                    sum.line_trigram_posting_visits_examined,
                    work.line_trigram_posting_visits_examined,
                )?,
                line_trigram_posting_visits_attempted: add(
                    sum.line_trigram_posting_visits_attempted,
                    work.line_trigram_posting_visits_attempted,
                )?,
                edge_query_union_candidates: add(
                    sum.edge_query_union_candidates,
                    work.edge_query_union_candidates,
                )?,
                line_trigram_only_query_union_candidates: add(
                    sum.line_trigram_only_query_union_candidates,
                    work.line_trigram_only_query_union_candidates,
                )?,
                filtered_candidates: add(sum.filtered_candidates, work.filtered_candidates)?,
                pair_visits_examined: add(sum.pair_visits_examined, work.pair_visits_examined)?,
                pair_visits_attempted: add(sum.pair_visits_attempted, work.pair_visits_attempted)?,
                similarity_comparisons_examined: add(
                    sum.similarity_comparisons_examined,
                    work.similarity_comparisons_examined,
                )?,
                similarity_comparisons_attempted: add(
                    sum.similarity_comparisons_attempted,
                    work.similarity_comparisons_attempted,
                )?,
            })
        })
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

fn validate_run_signature_metrics(
    metrics: SentenceRecoveryMetrics,
) -> std::result::Result<(), String> {
    let counters = [
        metrics.old_run_signature_unique_units,
        metrics.new_run_signature_unique_units,
        metrics.old_run_signature_duplicate_units,
        metrics.new_run_signature_duplicate_units,
        metrics.run_signature_shared_unit_keys,
        metrics.run_signature_largest_posting,
        metrics.run_signature_posting_visits_attempted,
        metrics.run_signature_posting_visits_examined,
        metrics.run_signature_token_verifications_attempted,
        metrics.run_signature_token_verifications_examined,
        metrics.run_signature_candidate_pairs,
        metrics.run_signature_globally_anchored_runs_skipped,
        metrics.run_signature_reciprocal_unique_pairs,
        metrics.run_signature_margin_qualified_pairs,
        metrics.run_signature_margin_veto_pairs,
        metrics.run_signature_monotone_pairs,
        metrics.run_signature_crossing_veto_pairs,
        metrics.run_signature_max_shared_units,
    ];
    if !metrics.run_signature_available {
        if metrics.run_signature_complete
            || metrics.run_signature_stop_reason.is_some()
            || counters.iter().any(|counter| *counter != 0)
        {
            return Err("unavailable run signature has diagnostic state".to_owned());
        }
        return Ok(());
    }
    if metrics.run_signature_posting_visits_examined
        > metrics.run_signature_posting_visits_attempted
        || metrics.run_signature_token_verifications_examined
            > metrics.run_signature_token_verifications_attempted
    {
        return Err("run signature examined work exceeds attempted work".to_owned());
    }
    let posting_deficit = metrics.run_signature_posting_visits_examined
        < metrics.run_signature_posting_visits_attempted;
    let verification_deficit = metrics.run_signature_token_verifications_examined
        < metrics.run_signature_token_verifications_attempted;
    if metrics.run_signature_complete {
        if posting_deficit || verification_deficit || metrics.run_signature_stop_reason.is_some() {
            return Err("complete run signature has unfinished work".to_owned());
        }
    } else {
        if metrics.run_signature_stop_reason.is_none() {
            return Err("incomplete run signature has no stop reason".to_owned());
        }
        if [
            metrics.run_signature_reciprocal_unique_pairs,
            metrics.run_signature_margin_qualified_pairs,
            metrics.run_signature_margin_veto_pairs,
            metrics.run_signature_monotone_pairs,
            metrics.run_signature_crossing_veto_pairs,
            metrics.run_signature_max_shared_units,
        ]
        .iter()
        .any(|counter| *counter != 0)
        {
            return Err("incomplete run signature exposes pair results".to_owned());
        }
    }
    match metrics.run_signature_stop_reason {
        Some(RunSignatureStopReason::PostingVisitLimit) if posting_deficit => {}
        Some(RunSignatureStopReason::TokenVerificationLimit) if verification_deficit => {}
        Some(RunSignatureStopReason::CandidatePairLimit) if !verification_deficit => {}
        Some(_) => {
            return Err("run signature stop reason disagrees with unfinished work".to_owned());
        }
        None => {}
    }
    let margin_partition = metrics
        .run_signature_margin_qualified_pairs
        .checked_add(metrics.run_signature_margin_veto_pairs)
        .ok_or_else(|| "run signature margin partition overflows".to_owned())?;
    if margin_partition != metrics.run_signature_reciprocal_unique_pairs {
        return Err("run signature margin partition is inconsistent".to_owned());
    }
    if metrics.run_signature_reciprocal_unique_pairs > metrics.run_signature_candidate_pairs {
        return Err("run signature reciprocal pairs exceed candidate pairs".to_owned());
    }
    let order_partition = metrics
        .run_signature_monotone_pairs
        .checked_add(metrics.run_signature_crossing_veto_pairs)
        .ok_or_else(|| "run signature order partition overflows".to_owned())?;
    if order_partition != metrics.run_signature_margin_qualified_pairs {
        return Err("run signature order partition is inconsistent".to_owned());
    }
    if (metrics.run_signature_shared_unit_keys == 0) != (metrics.run_signature_largest_posting == 0)
    {
        return Err("run signature largest posting disagrees with shared keys".to_owned());
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
    recovery_watch_diagnostics: Option<RecoveryWatchDiagnostics>,
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
    recovery_watch_queries: &[RecoveryWatchQuery<'_>],
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
    let result = compare_extraction_outcomes_with_known_span_sentence_shadow_diagnostics(
        old,
        new,
        options,
        &mut diagnostics,
        recovery_watch_queries,
    )
    .map(|watched| (watched.outcome, watched.alignment, watched.diagnostics));
    let metrics = alignment_visit_metrics(&diagnostics).map_err(|message| {
        RevisionRunError::Other("alignment metrics contract violation", message)
    })?;
    let sentence_recovery_metrics = sentence_recovery_metrics(&diagnostics).map_err(|message| {
        RevisionRunError::Other("sentence recovery metrics contract violation", message)
    })?;
    let pressure = resolve_pressure(metrics.candidate_visits, pressure_required, pressure_result)?;
    let (outcome, alignment, recovery_watch_diagnostics) = result.map_err(|error| match error {
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
        recovery_watch_diagnostics,
    })
}

fn run_extraction_and_comparison(
    source: &ParserBackedGlyphSource<LopdfParser, ContentStreamGlyphExtractor>,
    old_path: &Path,
    new_path: &Path,
    limit_scale: f64,
    recovery_watch_queries: &[RecoveryWatchQuery<'_>],
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
    compare_outcomes_with_metrics(old_outcome, new_outcome, options, recovery_watch_queries)
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
    pub const SCHEMA_VERSION: u32 = 24;

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

    fn expected_document(changes: Vec<ExpectedChange>) -> ExpectedDocument {
        ExpectedDocument {
            version: 1,
            pair: "pair".to_owned(),
            reviewed_on: "2026-08-30".to_owned(),
            annotation: Annotation::Complete,
            notes: String::new(),
            scopes: Vec::new(),
            changes,
        }
    }

    fn recovery_watch_expected_change(id: &str, kind: ExpectedKind) -> ExpectedChange {
        ExpectedChange {
            id: id.to_owned(),
            kind,
            scope: None,
            occurrence_count: None,
            old_quote: Some(format!("old {id}")),
            new_quote: Some(format!("new {id}")),
            note: String::new(),
        }
    }

    #[test]
    fn recovery_watch_queries_are_capped_filtered_and_ordered() {
        let mut changes = (0..MAX_EXPECTED_CHANGE_DIAGNOSTICS + 2)
            .map(|index| {
                recovery_watch_expected_change(&index.to_string(), ExpectedKind::Replacement)
            })
            .collect::<Vec<_>>();
        changes[1].kind = ExpectedKind::Insertion;
        changes[3].new_quote = None;
        let document = expected_document(changes);

        let query_set = RecoveryWatchQuerySet::new(Some(&document));
        let queries = query_set.queries(&document.changes);

        assert_eq!(queries.len(), MAX_EXPECTED_CHANGE_DIAGNOSTICS - 2);
        assert_eq!(query_set.expected_indices[..3], [0, 2, 4]);
        assert_eq!(
            query_set.expected_indices.last(),
            Some(&(MAX_EXPECTED_CHANGE_DIAGNOSTICS - 1))
        );
        assert_eq!(queries[0].old_quote, Some("old 0"));
        assert_eq!(queries[1].old_quote, Some("old 2"));
    }

    #[test]
    fn recovery_watch_queries_accept_only_kind_appropriate_quote_shapes() {
        let mut insertion = recovery_watch_expected_change("insertion", ExpectedKind::Insertion);
        insertion.old_quote = None;
        let mut deletion = recovery_watch_expected_change("deletion", ExpectedKind::Deletion);
        deletion.new_quote = None;
        let mut invalid_insertion =
            recovery_watch_expected_change("invalid-insertion", ExpectedKind::Insertion);
        invalid_insertion.new_quote = None;
        let mut invalid_deletion =
            recovery_watch_expected_change("invalid-deletion", ExpectedKind::Deletion);
        invalid_deletion.old_quote = None;
        let document = expected_document(vec![
            insertion,
            deletion,
            invalid_insertion,
            invalid_deletion,
        ]);

        let query_set = RecoveryWatchQuerySet::new(Some(&document));
        let queries = query_set.queries(&document.changes);

        assert_eq!(query_set.expected_indices, [0, 1]);
        assert_eq!(queries[0].old_quote, None);
        assert_eq!(queries[0].new_quote, Some("new insertion"));
        assert_eq!(queries[1].old_quote, Some("old deletion"));
        assert_eq!(queries[1].new_quote, None);
    }

    #[test]
    fn recovery_watch_join_is_ordinal_safe_with_duplicate_and_missing_expected_ids() {
        let mut insertion = recovery_watch_expected_change("insertion", ExpectedKind::Insertion);
        insertion.old_quote = None;
        let document = expected_document(vec![
            recovery_watch_expected_change("duplicate", ExpectedKind::Replacement),
            recovery_watch_expected_change("duplicate", ExpectedKind::Move),
            insertion,
        ]);
        let query_set = RecoveryWatchQuerySet::new(Some(&document));
        let diagnostics = RecoveryWatchDiagnostics {
            complete: true,
            candidate_generation_complete: true,
            near_relation_complete: true,
            near_relation_stop_reason: None,
            records: vec![
                pdfdelta_core::diff::RecoveryWatchRecord {
                    id: query_set.ids[0].clone(),
                    old: RecoveryWatchOccurrenceEvidence::Unfound,
                    new: RecoveryWatchOccurrenceEvidence::Ambiguous,
                    pair: None,
                    segment_pair: None,
                    granular_pair: None,
                },
                pdfdelta_core::diff::RecoveryWatchRecord {
                    id: query_set.ids[0].clone(),
                    old: RecoveryWatchOccurrenceEvidence::Unavailable,
                    new: RecoveryWatchOccurrenceEvidence::Unavailable,
                    pair: None,
                    segment_pair: None,
                    granular_pair: None,
                },
                pdfdelta_core::diff::RecoveryWatchRecord {
                    id: "unknown".to_owned(),
                    old: RecoveryWatchOccurrenceEvidence::Unfound,
                    new: RecoveryWatchOccurrenceEvidence::Unfound,
                    pair: None,
                    segment_pair: None,
                    granular_pair: None,
                },
            ],
            ..RecoveryWatchDiagnostics::default()
        };

        let report = recovery_watch_report(&document.changes, &query_set, diagnostics);

        assert!(!report.complete);
        assert_eq!(report.records.len(), 3);
        assert_eq!(report.records[0].expected_id, "duplicate");
        assert_eq!(
            report.records[0].old,
            RecoveryWatchOccurrenceReport::Unfound
        );
        assert_eq!(
            report.records[0].new,
            RecoveryWatchOccurrenceReport::Ambiguous
        );
        assert_eq!(report.records[1].expected_id, "duplicate");
        assert_eq!(
            report.records[1].old,
            RecoveryWatchOccurrenceReport::Unavailable
        );
        assert_eq!(
            report.records[2].old,
            RecoveryWatchOccurrenceReport::NotQueried
        );
        assert_eq!(
            report.records[2].new,
            RecoveryWatchOccurrenceReport::Unavailable
        );
    }

    #[test]
    fn recovery_watch_report_serializes_occurrences_pair_evidence_and_stop_state() {
        let document = expected_document(
            [
                "unfound",
                "ambiguous",
                "unavailable",
                "found",
                "not-queried",
                "occurrences",
            ]
            .map(|id| recovery_watch_expected_change(id, ExpectedKind::Replacement))
            .to_vec(),
        );
        let query_set = RecoveryWatchQuerySet::new(Some(&document));
        let mut records = [
            RecoveryWatchOccurrenceEvidence::Unfound,
            RecoveryWatchOccurrenceEvidence::Ambiguous,
            RecoveryWatchOccurrenceEvidence::Unavailable,
        ]
        .into_iter()
        .enumerate()
        .map(
            |(index, occurrence)| pdfdelta_core::diff::RecoveryWatchRecord {
                id: query_set.ids[index].clone(),
                old: occurrence,
                new: RecoveryWatchOccurrenceEvidence::Unfound,
                pair: None,
                segment_pair: None,
                granular_pair: None,
            },
        )
        .collect::<Vec<_>>();
        records.push(pdfdelta_core::diff::RecoveryWatchRecord {
            id: query_set.ids[3].clone(),
            old: RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                span_index: Some(2),
                trusted_run_descriptor_index: Some(3),
                ordinal: Some(4),
                end_ordinal: Some(6),
                unit_count: Some(2),
                token_count: Some(12),
                recovery_location_available: true,
                fully_contained: false,
                page: Some(5),
                bbox: Some(Rect {
                    min: Vec2 { x: 1.0, y: 2.0 },
                    max: Vec2 { x: 3.0, y: 4.0 },
                }),
                role: Some(BlockRole::Body),
                kind: RecoveryWatchUnitKind::Segment,
            }),
            new: RecoveryWatchOccurrenceEvidence::Unavailable,
            pair: Some(RecoveryWatchPairEvidence {
                same_span: false,
                exact_shared_units: 1,
                exact_shared_units_available: true,
                near_candidate_examined: true,
                near_score: Some(700),
                near_scope: Some(RecoveryWatchNearScope::CrossSpan),
                old_relation: RecoveryWatchRelation {
                    available: true,
                    best_score: 700,
                    second_score: 600,
                    watched_partner_is_best: true,
                },
                new_relation: RecoveryWatchRelation {
                    available: false,
                    best_score: 0,
                    second_score: 0,
                    watched_partner_is_best: false,
                },
                reciprocal: false,
            }),
            segment_pair: Some(RecoveryWatchSegmentPairEvidence {
                old_start_ordinal: 4,
                old_end_ordinal: 6,
                new_start_ordinal: 7,
                new_end_ordinal: 9,
                old_unit_count: 2,
                new_unit_count: 2,
                old_token_count: 12,
                new_token_count: 13,
                exact: true,
                old_occurrence_count: 1,
                new_occurrence_count: 1,
                role_compatible: true,
                overlaps_existing_recovery: false,
                crossing_anchor_count: 0,
                relation: ExactSegmentRelation::ExactUniqueMonotone,
            }),
            granular_pair: Some(RecoveryWatchGranularPairEvidence {
                old_units: vec![RecoveryWatchGranularUnitEvidence {
                    kind: RecoveryWatchUnitKind::Clause,
                    byte_start: 2,
                    byte_end: 14,
                    token_count: 10,
                    page: Some(5),
                    role: Some(BlockRole::Body),
                    recovery_location_available: true,
                    relation: RecoveryWatchGranularRelation {
                        available: true,
                        best_score: 900,
                        second_score: 700,
                        partner_index: Some(0),
                        exact: false,
                        reciprocal: true,
                        tied_for_best: false,
                    },
                }],
                new_units: vec![RecoveryWatchGranularUnitEvidence {
                    kind: RecoveryWatchUnitKind::ListItem,
                    byte_start: 3,
                    byte_end: 16,
                    token_count: 11,
                    page: Some(6),
                    role: Some(BlockRole::Body),
                    recovery_location_available: true,
                    relation: RecoveryWatchGranularRelation {
                        available: true,
                        best_score: 900,
                        second_score: 650,
                        partner_index: Some(0),
                        exact: false,
                        reciprocal: true,
                        tied_for_best: false,
                    },
                }],
            }),
        });
        records.push(pdfdelta_core::diff::RecoveryWatchRecord {
            id: query_set.ids[4].clone(),
            old: RecoveryWatchOccurrenceEvidence::NotQueried,
            new: RecoveryWatchOccurrenceEvidence::Unavailable,
            pair: None,
            segment_pair: None,
            granular_pair: None,
        });
        let occurrence = RecoveryWatchOccurrence {
            span_index: Some(10),
            trusted_run_descriptor_index: Some(11),
            ordinal: Some(12),
            end_ordinal: None,
            unit_count: None,
            token_count: Some(16),
            recovery_location_available: true,
            fully_contained: true,
            page: Some(13),
            bbox: None,
            role: Some(BlockRole::RepeatedFooter),
            kind: RecoveryWatchUnitKind::Line,
        };
        records.push(pdfdelta_core::diff::RecoveryWatchRecord {
            id: query_set.ids[5].clone(),
            old: RecoveryWatchOccurrenceEvidence::Occurrences(
                pdfdelta_core::diff::RecoveryWatchOccurrences {
                    occurrence_count: 3,
                    complete: false,
                    occurrences: vec![occurrence.clone(), occurrence],
                },
            ),
            new: RecoveryWatchOccurrenceEvidence::NotQueried,
            pair: None,
            segment_pair: None,
            granular_pair: None,
        });
        let report = recovery_watch_report(
            &document.changes,
            &query_set,
            RecoveryWatchDiagnostics {
                complete: false,
                candidate_generation_complete: true,
                near_relation_complete: false,
                near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
                segment_candidates: 8,
                segment_hash_matches: 6,
                segment_token_verified_matches: 5,
                segment_unique_pairs: 3,
                segment_duplicate_pairs: 2,
                segment_monotone_pairs: 2,
                segment_crossing_pairs: 1,
                segment_overlap_vetoes: 1,
                segment_stop_reason: Some(SegmentStopReason::TokenVerificationLimit),
                granular_complete: false,
                granular_old_units: 3,
                granular_new_units: 4,
                granular_pair_comparisons: 12,
                granular_stop_reason: Some(RecoveryWatchGranularStopReason::OutputLimit),
                records,
            },
        );

        let value = serde_json::to_value(report).expect("report serializes");
        assert_eq!(
            value
                .as_object()
                .expect("watch report object")
                .keys()
                .map(String::as_str)
                .collect::<HashSet<_>>(),
            HashSet::from([
                "complete",
                "candidate_generation_complete",
                "near_relation_complete",
                "near_relation_stop_reason",
                "segment_candidates",
                "segment_hash_matches",
                "segment_token_verified_matches",
                "segment_unique_pairs",
                "segment_duplicate_pairs",
                "segment_monotone_pairs",
                "segment_crossing_pairs",
                "segment_overlap_vetoes",
                "segment_stop_reason",
                "granular_complete",
                "granular_old_units",
                "granular_new_units",
                "granular_pair_comparisons",
                "granular_stop_reason",
                "records",
            ])
        );
        assert_eq!(value["complete"], false);
        assert_eq!(value["near_relation_stop_reason"], "pair_visit_limit");
        assert_eq!(value["segment_candidates"], 8);
        assert_eq!(value["segment_hash_matches"], 6);
        assert_eq!(value["segment_token_verified_matches"], 5);
        assert_eq!(value["segment_unique_pairs"], 3);
        assert_eq!(value["segment_duplicate_pairs"], 2);
        assert_eq!(value["segment_monotone_pairs"], 2);
        assert_eq!(value["segment_crossing_pairs"], 1);
        assert_eq!(value["segment_overlap_vetoes"], 1);
        assert_eq!(value["segment_stop_reason"], "token_verification_limit");
        assert_eq!(value["granular_complete"], false);
        assert_eq!(value["granular_old_units"], 3);
        assert_eq!(value["granular_new_units"], 4);
        assert_eq!(value["granular_pair_comparisons"], 12);
        assert_eq!(value["granular_stop_reason"], "output_limit");
        assert_eq!(value["records"][0]["old"]["status"], "unfound");
        assert_eq!(value["records"][1]["old"]["status"], "ambiguous");
        assert_eq!(value["records"][2]["old"]["status"], "unavailable");
        assert_eq!(value["records"][3]["old"]["status"], "found");
        assert_eq!(value["records"][4]["old"]["status"], "not_queried");
        assert_eq!(value["records"][5]["old"]["status"], "occurrences");
        assert_eq!(value["records"][5]["old"]["occurrence_count"], 3);
        assert_eq!(value["records"][5]["old"]["complete"], false);
        assert_eq!(value["records"][5]["old"]["truncated"], true);
        assert_eq!(
            value["records"][5]["old"]["occurrences"]
                .as_array()
                .expect("retained occurrences array")
                .len(),
            2
        );
        assert_eq!(value["records"][5]["new"]["status"], "not_queried");
        assert_eq!(
            value["records"][3]["granular_pair"],
            serde_json::json!({
                "old_units": [{
                    "kind": "clause",
                    "byte_start": 2,
                    "byte_end": 14,
                    "token_count": 10,
                    "page": 5,
                    "role": "body",
                    "recovery_location_available": true,
                    "relation": {
                        "available": true,
                        "best_score": 900,
                        "second_score": 700,
                        "partner_index": 0,
                        "exact": false,
                        "reciprocal": true,
                        "tied_for_best": false
                    }
                }],
                "new_units": [{
                    "kind": "list_item",
                    "byte_start": 3,
                    "byte_end": 16,
                    "token_count": 11,
                    "page": 6,
                    "role": "body",
                    "recovery_location_available": true,
                    "relation": {
                        "available": true,
                        "best_score": 900,
                        "second_score": 650,
                        "partner_index": 0,
                        "exact": false,
                        "reciprocal": true,
                        "tied_for_best": false
                    }
                }]
            })
        );
        assert_eq!(
            value["records"][3]
                .as_object()
                .expect("watch record object")
                .keys()
                .map(String::as_str)
                .collect::<HashSet<_>>(),
            HashSet::from([
                "expected_id",
                "old",
                "new",
                "pair",
                "segment_pair",
                "granular_pair",
            ])
        );
        assert_eq!(
            value["records"][3]["old"]
                .as_object()
                .expect("found occurrence object")
                .keys()
                .map(String::as_str)
                .collect::<HashSet<_>>(),
            HashSet::from([
                "status",
                "span_index",
                "trusted_run_descriptor_index",
                "ordinal",
                "end_ordinal",
                "unit_count",
                "token_count",
                "recovery_location_available",
                "fully_contained",
                "page",
                "bbox",
                "role",
                "kind",
            ])
        );
        assert_eq!(value["records"][3]["old"]["kind"], "segment");
        assert_eq!(value["records"][3]["old"]["end_ordinal"], 6);
        assert_eq!(value["records"][3]["old"]["unit_count"], 2);
        assert_eq!(value["records"][3]["old"]["token_count"], 12);
        assert_eq!(value["records"][3]["old"]["bbox"]["max"]["x"], 3.0);
        assert_eq!(value["records"][3]["pair"]["near_scope"], "cross_span");
        assert_eq!(
            value["records"][3]["pair"]["exact_shared_units_available"],
            true
        );
        assert_eq!(
            value["records"][3]["pair"]["old_relation"]["best_score"],
            700
        );
        assert_eq!(value["records"][3]["pair"]["reciprocal"], false);
        assert_eq!(
            value["records"][3]["segment_pair"],
            serde_json::json!({
                "old_start_ordinal": 4,
                "old_end_ordinal": 6,
                "new_start_ordinal": 7,
                "new_end_ordinal": 9,
                "old_unit_count": 2,
                "new_unit_count": 2,
                "old_token_count": 12,
                "new_token_count": 13,
                "exact": true,
                "old_occurrence_count": 1,
                "new_occurrence_count": 1,
                "role_compatible": true,
                "overlaps_existing_recovery": false,
                "crossing_anchor_count": 0,
                "relation": "exact_unique_monotone"
            })
        );
    }

    #[test]
    fn recovery_watch_occurrence_report_complements_completion_flags() {
        for complete in [false, true] {
            let value = serde_json::to_value(RecoveryWatchOccurrenceReport::from(
                RecoveryWatchOccurrenceEvidence::Occurrences(
                    pdfdelta_core::diff::RecoveryWatchOccurrences {
                        occurrence_count: 0,
                        complete,
                        occurrences: Vec::new(),
                    },
                ),
            ))
            .expect("occurrence evidence serializes");

            assert_eq!(value["complete"], complete);
            assert_eq!(value["truncated"], !complete);
        }
    }

    #[test]
    fn segment_diagnostic_enums_use_exact_snake_case_tags() {
        let relations = [
            (ExactSegmentRelation::NonExact, "non_exact"),
            (ExactSegmentRelation::Duplicate, "duplicate"),
            (
                ExactSegmentRelation::ExactUniqueTopologyUnknown,
                "exact_unique_topology_unknown",
            ),
            (
                ExactSegmentRelation::ExactUniqueMonotone,
                "exact_unique_monotone",
            ),
            (
                ExactSegmentRelation::ExactUniqueCrossing,
                "exact_unique_crossing",
            ),
        ];
        for (relation, expected) in relations {
            assert_eq!(
                serde_json::to_value(ExactSegmentRelationReport::from(relation))
                    .expect("segment relation serializes"),
                expected
            );
        }

        let stop_reasons = [
            (
                SegmentStopReason::CandidateCountLimit,
                "candidate_count_limit",
            ),
            (
                SegmentStopReason::HashPairVisitLimit,
                "hash_pair_visit_limit",
            ),
            (
                SegmentStopReason::TokenVerificationLimit,
                "token_verification_limit",
            ),
            (SegmentStopReason::AllocationFailure, "allocation_failure"),
        ];
        for (reason, expected) in stop_reasons {
            assert_eq!(
                serde_json::to_value(SegmentStopReasonReport::from(reason))
                    .expect("segment stop reason serializes"),
                expected
            );
        }

        let granular_stop_reasons = [
            (
                RecoveryWatchGranularStopReason::UnitCountLimit,
                "unit_count_limit",
            ),
            (
                RecoveryWatchGranularStopReason::TokenByteLimit,
                "token_byte_limit",
            ),
            (
                RecoveryWatchGranularStopReason::ComparisonLimit,
                "comparison_limit",
            ),
            (RecoveryWatchGranularStopReason::OutputLimit, "output_limit"),
            (
                RecoveryWatchGranularStopReason::AuxiliaryLimit,
                "auxiliary_limit",
            ),
            (
                RecoveryWatchGranularStopReason::AllocationFailure,
                "allocation_failure",
            ),
        ];
        for (reason, expected) in granular_stop_reasons {
            assert_eq!(
                serde_json::to_value(RecoveryWatchGranularStopReasonReport::from(reason))
                    .expect("granular stop reason serializes"),
                expected
            );
        }
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
    fn expected_documents_validate_optional_occurrence_count() {
        let positive = r#"{"version":1,"pair":"p","reviewed_on":"x","annotation":"partial","changes":[{"id":"c","kind":"deletion","occurrence_count":2,"old_quote":"removed"}]}"#;
        let document = load_expected_document(positive).expect("positive count is valid");
        assert_eq!(document.changes[0].occurrence_count, Some(2));

        let absent = positive.replace(",\"occurrence_count\":2", "");
        let document = load_expected_document(&absent).expect("count remains optional");
        assert_eq!(document.changes[0].occurrence_count, None);

        let zero = positive.replace(r#""occurrence_count":2"#, r#""occurrence_count":0"#);
        assert!(load_expected_document(&zero).is_err());
    }

    #[test]
    fn expected_documents_bound_the_number_of_changes() {
        let changes = (0..=MAX_EXPECTED_DOCUMENT_CHANGES)
            .map(|index| {
                serde_json::json!({
                    "id": format!("change-{index}"),
                    "kind": "deletion",
                    "old_quote": "removed"
                })
            })
            .collect::<Vec<_>>();
        let document = serde_json::json!({
            "version": 1,
            "pair": "p",
            "reviewed_on": "x",
            "annotation": "partial",
            "changes": changes
        });

        assert!(load_expected_document(&document.to_string()).is_err());
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
        assert_eq!(document.scopes[0].completeness, None);
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
    fn annotation_modes_validate_scope_references() {
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

        let mixed_partial = r#"{
            "version":1,"pair":"p","reviewed_on":"x","annotation":"partial",
            "scopes":[{
                "id":"s","completeness":"complete",
                "old":{"start_quote":"a","end_quote":"b"},
                "new":{"start_quote":"c","end_quote":"d"}
            }],
            "changes":[
                {"id":"scoped","kind":"deletion","scope":"s","old_quote":"removed"},
                {"id":"global","kind":"insertion","new_quote":"added"}
            ]
        }"#;
        let document = load_expected_document(mixed_partial).expect("valid mixed annotation");
        assert_eq!(document.annotation, Annotation::Partial);
        assert_eq!(
            document.scopes[0].completeness,
            Some(ScopeCompleteness::Complete)
        );
        assert_eq!(document.changes[0].scope.as_deref(), Some("s"));
        assert_eq!(document.changes[1].scope, None);

        assert!(
            load_expected_document(&mixed_partial.replace(r#","completeness":"complete""#, ""))
                .is_err()
        );
        assert!(
            load_expected_document(
                &mixed_partial.replace(r#""scope":"s""#, r#""scope":"missing""#)
            )
            .is_err()
        );
        assert!(
            load_expected_document(
                &mixed_partial.replace(r#""annotation":"partial""#, r#""annotation":"complete""#)
            )
            .is_err()
        );
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
            occurrence_count: None,
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
                old_text: old_text.map(collapse_whitespace),
                new_text: new_text.map(collapse_whitespace),
                old_comparable_len: old_len,
                new_comparable_len: new_len,
                resolvable: true,
            }],
        }
    }

    fn repeated_replacement(occurrences: &[(&str, &str)]) -> ActualChange {
        ActualChange {
            kind: ChangeKind::Replacement,
            occurrences: occurrences
                .iter()
                .map(|(old, new)| ActualChangeOccurrence {
                    old_text: Some(collapse_whitespace(old)),
                    new_text: Some(collapse_whitespace(new)),
                    old_comparable_len: Some(old.chars().count()),
                    new_comparable_len: Some(new.chars().count()),
                    resolvable: true,
                })
                .collect(),
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
                ExpectedChangeFailureReason::OccurrenceCountMismatch {
                    expected: 2,
                    actual: 1,
                },
                serde_json::json!({"expected_id":"c","reason":"occurrence_count_mismatch","expected":2,"actual":1}),
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
            recovery_watch: Some(RecoveryWatchDiagnosticsReport {
                complete: true,
                candidate_generation_complete: true,
                near_relation_complete: true,
                near_relation_stop_reason: None,
                segment_candidates: 0,
                segment_hash_matches: 0,
                segment_token_verified_matches: 0,
                segment_unique_pairs: 0,
                segment_duplicate_pairs: 0,
                segment_monotone_pairs: 0,
                segment_crossing_pairs: 0,
                segment_overlap_vetoes: 0,
                segment_stop_reason: None,
                granular_complete: true,
                granular_old_units: 0,
                granular_new_units: 0,
                granular_pair_comparisons: 0,
                granular_stop_reason: None,
                records: Vec::new(),
            }),
        });
        let completed = RevisionSummaryReport::from_reports(&[report]);
        let completed = serde_json::to_value(completed).expect("summary serializes");
        assert_eq!(completed["schema_version"], 24);
        assert_eq!(completed["records"][0]["candidate_recall"]["top_k"], 32);
        assert_eq!(
            completed["records"][0]["candidate_recall"]["recall_at_k"],
            serde_json::Value::Null
        );
        assert_eq!(
            completed["records"][0]["expected_change_diagnostics"],
            serde_json::json!({
                "complete": true,
                "failures": [],
                "recovery_watch": {
                    "complete": true,
                    "candidate_generation_complete": true,
                    "near_relation_complete": true,
                    "near_relation_stop_reason": null,
                    "segment_candidates": 0,
                    "segment_hash_matches": 0,
                    "segment_token_verified_matches": 0,
                    "segment_unique_pairs": 0,
                    "segment_duplicate_pairs": 0,
                    "segment_monotone_pairs": 0,
                    "segment_crossing_pairs": 0,
                    "segment_overlap_vetoes": 0,
                    "segment_stop_reason": null,
                    "granular_complete": true,
                    "granular_old_units": 0,
                    "granular_new_units": 0,
                    "granular_pair_comparisons": 0,
                    "granular_stop_reason": null,
                    "records": []
                }
            })
        );
    }

    #[test]
    fn scoped_metrics_are_compact_only_and_omitted_when_unavailable() {
        let legacy = record(PairRunStatus::Ok);
        let legacy_full = serde_json::to_value(&legacy).expect("full report serializes");
        assert!(legacy_full.get("scoped_event_metrics").is_none());
        let legacy_summary = serde_json::to_value(RevisionSummaryReport::from_reports(&[legacy]))
            .expect("summary serializes");
        assert_eq!(legacy_summary["schema_version"], 24);
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
    fn occurrence_count_matches_the_exact_number_of_repeated_quotes() {
        let mut expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        expected.occurrence_count = Some(2);
        let actual = repeated_replacement(&[
            ("first old quote", "first new quote"),
            ("second old quote", "second new quote"),
        ]);

        assert_eq!(match_changes(&[expected], &[actual]).matched, 1);
    }

    #[test]
    fn occurrence_count_rejects_a_missing_occurrence() {
        let mut expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        expected.occurrence_count = Some(2);
        let actual = repeated_replacement(&[("old quote", "new quote")]);

        assert_eq!(match_changes(&[expected], &[actual]).matched, 0);
    }

    #[test]
    fn occurrence_count_rejects_a_surplus_occurrence() {
        let mut expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        expected.occurrence_count = Some(2);
        let actual = repeated_replacement(&[
            ("first old quote", "first new quote"),
            ("second old quote", "second new quote"),
            ("third old quote", "third new quote"),
        ]);

        assert_eq!(match_changes(&[expected], &[actual]).matched, 0);
    }

    #[test]
    fn occurrence_count_rejects_one_wrong_quote() {
        let mut expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        expected.occurrence_count = Some(2);
        let actual = repeated_replacement(&[
            ("first old quote", "first new quote"),
            ("second old quote", "unexpected replacement"),
        ]);

        assert_eq!(match_changes(&[expected], &[actual]).matched, 0);
    }

    #[test]
    fn absent_occurrence_count_keeps_any_occurrence_matching() {
        let expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        let actual = repeated_replacement(&[
            ("unrelated old text", "unrelated new text"),
            ("matching old quote", "matching new quote"),
        ]);

        assert_eq!(match_changes(&[expected], &[actual]).matched, 1);
    }

    #[test]
    fn maximum_matching_preserves_a_repeated_event_for_the_counted_expectation() {
        let any = expected_change(
            "any",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        let mut repeated = any.clone();
        repeated.id = "repeated".to_owned();
        repeated.occurrence_count = Some(2);
        let repeated_actual = repeated_replacement(&[
            ("first old quote", "first new quote"),
            ("second old quote", "second new quote"),
        ]);
        let single_actual = repeated_replacement(&[("old quote", "new quote")]);

        let outcome = match_changes(&[any, repeated], &[repeated_actual, single_actual]);

        assert_eq!(outcome.claimed_actual_by_expected, [Some(1), Some(0)]);
    }

    #[test]
    fn maximum_matching_follows_crossing_quote_edges() {
        let broad = expected_change("broad", ExpectedKind::Replacement, Some("old"), Some("new"));
        let specific = expected_change(
            "specific",
            ExpectedKind::Replacement,
            Some("specific old"),
            Some("specific new"),
        );
        let specific_actual = repeated_replacement(&[("specific old", "specific new")]);
        let broad_actual = repeated_replacement(&[("other old", "other new")]);

        let outcome = match_changes(&[broad, specific], &[specific_actual, broad_actual]);

        assert_eq!(outcome.claimed_actual_by_expected, [Some(1), Some(0)]);
    }

    #[test]
    fn duplicate_edges_use_stable_expected_and_actual_order() {
        let expected = [
            expected_change("first", ExpectedKind::Replacement, Some("old"), Some("new")),
            expected_change(
                "second",
                ExpectedKind::Replacement,
                Some("old"),
                Some("new"),
            ),
        ];
        let actuals = [
            repeated_replacement(&[("old", "new")]),
            repeated_replacement(&[("old", "new")]),
        ];

        let outcome = match_changes(&expected, &actuals);

        assert_eq!(outcome.claimed_actual_by_expected, [Some(0), Some(1)]);
    }

    #[test]
    fn equal_cardinality_prefers_counted_expectations() {
        let any = expected_change("any", ExpectedKind::Replacement, Some("old"), Some("new"));
        let mut counted = any.clone();
        counted.id = "counted".to_owned();
        counted.occurrence_count = Some(1);
        let actual = repeated_replacement(&[("old", "new")]);

        let outcome = match_changes(&[any, counted], &[actual]);

        assert_eq!(outcome.claimed_actual_by_expected, [None, Some(0)]);
    }

    #[test]
    fn equal_cardinality_prefers_kind_agreement() {
        let replacement = expected_change(
            "replacement",
            ExpectedKind::Replacement,
            Some("old"),
            Some("new"),
        );
        let r#move = expected_change("move", ExpectedKind::Move, Some("old"), Some("new"));
        let actuals = [
            actual_change(ChangeKind::Move, Some("old"), Some("new"), Some(3), Some(3)),
            actual_change(
                ChangeKind::Replacement,
                Some("old"),
                Some("new"),
                Some(3),
                Some(3),
            ),
        ];

        let outcome = match_changes(&[replacement, r#move], &actuals);

        assert_eq!(outcome.claimed_actual_by_expected, [Some(1), Some(0)]);
        assert_eq!(outcome.kind_agreements, 2);
    }

    #[test]
    fn matching_candidate_edge_limit_returns_no_partial_outcome() {
        let expected = [expected_change(
            "expected",
            ExpectedKind::Replacement,
            Some("old"),
            Some("new"),
        )];
        let actuals = [repeated_replacement(&[("old", "new")])];

        let result = match_changes_with_limits(
            &expected,
            &actuals,
            None,
            MatchingLimits {
                max_candidate_edges: 0,
                ..MatchingLimits::default()
            },
        );

        assert!(matches!(result, Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)));
    }

    #[test]
    fn matching_occurrence_visit_limit_returns_no_partial_outcome() {
        let mut expected = expected_change(
            "expected",
            ExpectedKind::Replacement,
            Some("old"),
            Some("new"),
        );
        expected.occurrence_count = Some(2);
        let actuals = [repeated_replacement(&[("old", "new"), ("old", "new")])];

        let result = match_changes_with_limits(
            &[expected],
            &actuals,
            None,
            MatchingLimits {
                max_occurrence_visits: 1,
                ..MatchingLimits::default()
            },
        );

        assert!(matches!(result, Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)));
    }

    #[test]
    fn repeated_mismatch_evidence_limit_returns_no_partial_outcome() {
        let mut template =
            expected_change("first", ExpectedKind::Replacement, Some("old"), Some("new"));
        template.occurrence_count = Some(65);
        let expected = (0..4)
            .map(|index| {
                let mut change = template.clone();
                change.id = format!("expected-{index}");
                change
            })
            .collect::<Vec<_>>();
        let occurrences = vec![("old", "new"); 64];
        let actuals = [repeated_replacement(&occurrences)];

        let result = match_changes_with_limits(
            &expected,
            &actuals,
            None,
            MatchingLimits {
                max_occurrence_visits: 100,
                ..MatchingLimits::default()
            },
        );

        assert!(matches!(result, Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)));
    }

    #[test]
    fn matching_text_byte_limit_returns_no_partial_outcome() {
        let expected = [expected_change(
            "expected",
            ExpectedKind::Replacement,
            Some("old"),
            Some("new"),
        )];
        let actuals = [repeated_replacement(&[("old", "new")])];

        let result = match_changes_with_limits(
            &expected,
            &actuals,
            None,
            MatchingLimits {
                max_text_bytes: "oldnew".len() * 2 - 1,
                ..MatchingLimits::default()
            },
        );

        assert!(matches!(result, Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)));
    }

    #[test]
    fn matching_residual_visit_limit_returns_no_partial_outcome() {
        let expected = [expected_change(
            "expected",
            ExpectedKind::Replacement,
            Some("old"),
            Some("new"),
        )];
        let actuals = [repeated_replacement(&[("old", "new")])];

        let result = match_changes_with_limits(
            &expected,
            &actuals,
            None,
            MatchingLimits {
                max_residual_edge_visits: 0,
                ..MatchingLimits::default()
            },
        );

        assert!(matches!(result, Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)));
    }

    #[test]
    fn matching_augmentation_limit_returns_no_partial_outcome() {
        let expected = [expected_change(
            "expected",
            ExpectedKind::Replacement,
            Some("old"),
            Some("new"),
        )];
        let actuals = [repeated_replacement(&[("old", "new")])];

        let result = match_changes_with_limits(
            &expected,
            &actuals,
            None,
            MatchingLimits {
                max_augmentations: 0,
                ..MatchingLimits::default()
            },
        );

        assert!(matches!(result, Err(QUALITY_SKIP_MATCHING_RESOURCE_LIMIT)));
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
        let scopes = [Some("outside".to_owned()), Some("reviewed".to_owned())];
        let (quality, metrics) = scoped_quality(1, &expected, &actuals, &scopes);

        assert_eq!(quality.expected_changes, 1);
        assert_eq!(quality.reported_changes, 2);
        assert_eq!(quality.recall, Some(0.0));
        assert_eq!(quality.precision, Some(0.0));
        assert_eq!(metrics.f1, 0.0);

        let scoped_actuals = [actuals[0].clone(), actuals[1].clone()];
        let scoped_ids = [Some("reviewed".to_owned()), Some("reviewed".to_owned())];
        let (quality, metrics) = scoped_quality(1, &expected, &scoped_actuals, &scoped_ids);
        assert_eq!(quality.recall, Some(1.0));
        assert_eq!(quality.precision, Some(0.5));
        assert!((metrics.f1 - 2.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn mixed_scope_matching_constrains_only_scoped_expected_changes() {
        let actuals = [
            actual_change(
                ChangeKind::Replacement,
                Some("old quote"),
                Some("new quote"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Replacement,
                Some("old quote"),
                Some("new quote"),
                Some(2),
                Some(2),
            ),
        ];
        let scopes = [None, Some("reviewed".to_owned())];
        let scoped = [scoped_expected_change(
            "scoped",
            "reviewed",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        )];
        let scoped_outcome = match_changes_with_scopes(&scoped, &actuals, Some(&scopes));
        assert_eq!(scoped_outcome.claimed_actual_by_expected, [Some(1)]);

        let unscoped = [expected_change(
            "unscoped",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        )];
        let unscoped_outcome = match_changes_with_scopes(&unscoped, &actuals, Some(&scopes));
        assert_eq!(unscoped_outcome.claimed_actual_by_expected, [Some(0)]);
    }

    #[test]
    fn mixed_scope_matching_prioritizes_scoped_expected_changes_deterministically() {
        let actuals = [
            actual_change(
                ChangeKind::Replacement,
                Some("old quote"),
                Some("new quote"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Replacement,
                Some("old quote"),
                Some("new quote"),
                Some(2),
                Some(2),
            ),
        ];
        let scopes = [Some("reviewed".to_owned()), None];
        let unscoped = expected_change(
            "unscoped",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );
        let scoped = scoped_expected_change(
            "scoped",
            "reviewed",
            ExpectedKind::Replacement,
            Some("old quote"),
            Some("new quote"),
        );

        let unscoped_first = [unscoped.clone(), scoped.clone()];
        let first_outcome = match_changes_with_scopes(&unscoped_first, &actuals, Some(&scopes));
        assert_eq!(first_outcome.claimed_actual_by_expected, [Some(1), Some(0)]);

        let scoped_first = [scoped, unscoped];
        let second_outcome = match_changes_with_scopes(&scoped_first, &actuals, Some(&scopes));
        assert_eq!(
            second_outcome.claimed_actual_by_expected,
            [Some(0), Some(1)]
        );
        assert_eq!(
            quality_from_match_outcome(
                Annotation::Partial,
                &unscoped_first,
                &actuals,
                &first_outcome,
            ),
            quality_from_match_outcome(
                Annotation::Partial,
                &scoped_first,
                &actuals,
                &second_outcome,
            )
        );
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
            recovery_watch_diagnostics: _,
        } = compare_outcomes_with_metrics(old, new, PipelineOptions::default(), &[])
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
        } = compare_outcomes_with_metrics(old, new, PipelineOptions::default(), &[])
            .expect("empty comparison succeeds");

        assert_eq!(
            sentence_recovery_metrics,
            Some(SentenceRecoveryMetricsReport {
                structural_pairing_available: true,
                near_relation_complete: true,
                sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetricsReport {
                    complete: true,
                    ..SentenceEdgeGateShadowMetricsReport::default()
                }),
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
            recovery_watch_diagnostics: _,
        } = compare_outcomes_with_metrics(
            old.clone(),
            new.clone(),
            PipelineOptions::default(),
            &[],
        )
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
        let error = compare_outcomes_with_metrics(old, new, options, &[])
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

        let error = compare_outcomes_with_metrics(old, new, options, &[])
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
            recovery_watch_diagnostics: _,
        } = compare_outcomes_with_metrics(incomplete, complete, PipelineOptions::default(), &[])
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

        assert_eq!(json["schema_version"], 24);
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
            run_signature_available: true,
            run_signature_complete: true,
            old_run_signature_unique_units: 3,
            new_run_signature_unique_units: 4,
            run_signature_shared_unit_keys: 2,
            run_signature_largest_posting: 1,
            run_signature_posting_visits_attempted: 2,
            run_signature_posting_visits_examined: 2,
            run_signature_token_verifications_attempted: 10,
            run_signature_token_verifications_examined: 10,
            run_signature_candidate_pairs: 1,
            run_signature_reciprocal_unique_pairs: 1,
            run_signature_margin_qualified_pairs: 1,
            run_signature_monotone_pairs: 1,
            run_signature_max_shared_units: 2,
            near_relation_complete: true,
            near_pair_visits_examined: 20,
            near_pair_visits_attempted: 20,
            near_similarity_comparisons_examined: 8,
            near_similarity_comparisons_attempted: 8,
            near_candidate_posting_visits_examined: 14,
            near_candidate_posting_visits_attempted: 14,
            near_sentence_work: NearSearchWorkMetrics {
                edge_posting_visits_examined: 6,
                edge_posting_visits_attempted: 6,
                pair_visits_examined: 12,
                pair_visits_attempted: 12,
                similarity_comparisons_examined: 3,
                similarity_comparisons_attempted: 3,
                ..NearSearchWorkMetrics::default()
            },
            near_line_work: NearSearchWorkMetrics {
                edge_posting_visits_examined: 3,
                edge_posting_visits_attempted: 3,
                line_trigram_posting_visits_examined: 5,
                line_trigram_posting_visits_attempted: 5,
                pair_visits_examined: 8,
                pair_visits_attempted: 8,
                similarity_comparisons_examined: 5,
                similarity_comparisons_attempted: 5,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                sentence_work: NearSearchWorkMetrics {
                    edge_posting_visits_examined: 6,
                    edge_posting_visits_attempted: 6,
                    pair_visits_examined: 12,
                    pair_visits_attempted: 12,
                    similarity_comparisons_examined: 3,
                    similarity_comparisons_attempted: 3,
                    ..NearSearchWorkMetrics::default()
                },
                line_work: NearSearchWorkMetrics {
                    edge_posting_visits_examined: 3,
                    edge_posting_visits_attempted: 3,
                    line_trigram_posting_visits_examined: 5,
                    line_trigram_posting_visits_attempted: 5,
                    pair_visits_examined: 8,
                    pair_visits_attempted: 8,
                    similarity_comparisons_examined: 5,
                    similarity_comparisons_attempted: 5,
                    ..NearSearchWorkMetrics::default()
                },
            },
            near_largest_edge_posting: 12,
            near_largest_edge_query_union: 9,
            near_largest_filtered_candidate_set: 4,
            relation_floor_pairs_considered: 4,
            relation_floor_word_scans: 3,
            relation_floor_stop_opportunities: 2,
            relation_floor_potential_saved_word_comparisons: 7,
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
        assert!(validated.run_signature_complete);
        assert_eq!(validated.run_signature_margin_qualified_pairs, 1);
        assert!(validated.near_relation_complete);
        assert_eq!(validated.near_pair_visits_examined, 20);
        assert_eq!(validated.near_similarity_comparisons_attempted, 8);
        assert_eq!(validated.near_candidate_posting_visits_examined, 14);
        assert_eq!(validated.near_largest_edge_posting, 12);
        assert_eq!(validated.relation_floor_pairs_considered, 4);
        assert_eq!(validated.relation_floor_stop_opportunities, 2);
        assert_eq!(validated.relation_floor_potential_saved_word_comparisons, 7);
        assert_eq!(validated.recovered_replacement_new_tokens, 12);
        assert_eq!(validated.vetoed_near_pairs, 1);

        let candidate_stop = SentenceRecoveryMetrics {
            run_signature_available: true,
            run_signature_shared_unit_keys: 1,
            run_signature_largest_posting: 2,
            run_signature_posting_visits_attempted: 4,
            run_signature_posting_visits_examined: 2,
            run_signature_token_verifications_attempted: 2,
            run_signature_token_verifications_examined: 2,
            run_signature_candidate_pairs: 1,
            run_signature_stop_reason: Some(RunSignatureStopReason::CandidatePairLimit),
            ..SentenceRecoveryMetrics::default()
        };
        let validated = validate_sentence_recovery_metrics(candidate_stop)
            .expect("candidate pair stop may leave an admitted posting partially visited");
        assert_eq!(
            validated.run_signature_stop_reason,
            Some(RunSignatureStopReasonReport::CandidatePairLimit)
        );
    }

    #[test]
    fn rejects_invalid_sentence_recovery_metrics() {
        let unavailable_run_signature = SentenceRecoveryMetrics {
            run_signature_candidate_pairs: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(unavailable_run_signature).is_err());

        let complete_run_signature_with_deficit = SentenceRecoveryMetrics {
            run_signature_available: true,
            run_signature_complete: true,
            run_signature_posting_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(complete_run_signature_with_deficit).is_err());

        let incomplete_run_signature_without_reason = SentenceRecoveryMetrics {
            run_signature_available: true,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(
            validate_sentence_recovery_metrics(incomplete_run_signature_without_reason).is_err()
        );

        let invalid_run_signature_partition = SentenceRecoveryMetrics {
            run_signature_available: true,
            run_signature_complete: true,
            run_signature_reciprocal_unique_pairs: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_run_signature_partition).is_err());

        let reciprocal_exceeds_candidates = SentenceRecoveryMetrics {
            run_signature_available: true,
            run_signature_complete: true,
            run_signature_reciprocal_unique_pairs: 1,
            run_signature_margin_veto_pairs: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(reciprocal_exceeds_candidates).is_err());

        let candidate_stop_with_verification_deficit = SentenceRecoveryMetrics {
            run_signature_available: true,
            run_signature_shared_unit_keys: 1,
            run_signature_largest_posting: 2,
            run_signature_posting_visits_attempted: 4,
            run_signature_posting_visits_examined: 2,
            run_signature_token_verifications_attempted: 2,
            run_signature_token_verifications_examined: 1,
            run_signature_candidate_pairs: 1,
            run_signature_stop_reason: Some(RunSignatureStopReason::CandidatePairLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(
            validate_sentence_recovery_metrics(candidate_stop_with_verification_deficit).is_err()
        );

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

        let too_many_relation_floor_scans = SentenceRecoveryMetrics {
            relation_floor_pairs_considered: 1,
            relation_floor_word_scans: 2,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(too_many_relation_floor_scans).is_err());

        let too_many_relation_floor_stops = SentenceRecoveryMetrics {
            relation_floor_pairs_considered: 1,
            relation_floor_stop_opportunities: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(too_many_relation_floor_stops).is_err());

        let saved_without_relation_floor_stop = SentenceRecoveryMetrics {
            relation_floor_pairs_considered: 1,
            relation_floor_word_scans: 1,
            relation_floor_potential_saved_word_comparisons: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(saved_without_relation_floor_stop).is_err());

        let invalid_recovered_total = SentenceRecoveryMetrics {
            recovered_exact_match_old_tokens: usize::MAX,
            unresolved_remainder_old_source_tokens: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_recovered_total).is_err());

        let invalid_kind_work = SentenceRecoveryMetrics {
            near_sentence_work: NearSearchWorkMetrics {
                edge_posting_visits_examined: 2,
                edge_posting_visits_attempted: 1,
                ..NearSearchWorkMetrics::default()
            },
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_kind_work).is_err());

        let sentence_trigram_work = SentenceRecoveryMetrics {
            near_candidate_posting_visits_examined: 1,
            near_candidate_posting_visits_attempted: 1,
            near_sentence_work: NearSearchWorkMetrics {
                line_trigram_posting_visits_examined: 1,
                line_trigram_posting_visits_attempted: 1,
                ..NearSearchWorkMetrics::default()
            },
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(sentence_trigram_work).is_err());

        let mismatched_kind_sum = SentenceRecoveryMetrics {
            near_pair_visits_examined: 1,
            near_pair_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(mismatched_kind_sum).is_err());

        let mismatched_scope_sum = SentenceRecoveryMetrics {
            near_sentence_work: NearSearchWorkMetrics {
                filtered_candidates: 1,
                ..NearSearchWorkMetrics::default()
            },
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(mismatched_scope_sum).is_err());

        let mismatched_same_or_ambiguous_subscope_sum = SentenceRecoveryMetrics {
            near_sentence_work: NearSearchWorkMetrics {
                filtered_candidates: 1,
                ..NearSearchWorkMetrics::default()
            },
            near_same_or_ambiguous_span_work: NearSearchScopeMetrics {
                sentence_work: NearSearchWorkMetrics {
                    filtered_candidates: 1,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
            ..SentenceRecoveryMetrics::default()
        };
        assert!(
            validate_sentence_recovery_metrics(mismatched_same_or_ambiguous_subscope_sum).is_err()
        );

        let shared_query_work = NearSearchWorkMetrics {
            edge_query_union_candidates: 2,
            ..NearSearchWorkMetrics::default()
        };
        let valid_shared_query_subscope = SentenceRecoveryMetrics {
            near_sentence_work: shared_query_work,
            near_same_or_ambiguous_span_work: NearSearchScopeMetrics {
                sentence_work: shared_query_work,
                ..NearSearchScopeMetrics::default()
            },
            near_same_or_ambiguous_shared_query_work: NearSearchScopeMetrics {
                sentence_work: shared_query_work,
                ..NearSearchScopeMetrics::default()
            },
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(valid_shared_query_subscope).is_ok());

        let scope_sentence_trigram_work = SentenceRecoveryMetrics {
            near_paired_interval_work: NearSearchScopeMetrics {
                sentence_work: NearSearchWorkMetrics {
                    line_trigram_only_query_union_candidates: 1,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(scope_sentence_trigram_work).is_err());

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

        let examined_more_postings_than_attempted = SentenceRecoveryMetrics {
            near_candidate_posting_visits_examined: 2,
            near_candidate_posting_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(examined_more_postings_than_attempted).is_err());

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

        let posting_deficit_without_reason = SentenceRecoveryMetrics {
            near_candidate_posting_visits_attempted: 1,
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(posting_deficit_without_reason).is_err());

        let multiple_deficits = SentenceRecoveryMetrics {
            near_pair_visits_attempted: 1,
            near_similarity_comparisons_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::PairVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(multiple_deficits).is_err());

        let posting_limit_without_deficit = SentenceRecoveryMetrics {
            near_candidate_posting_visits_attempted: 1,
            near_candidate_posting_visits_examined: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::CandidatePostingVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(posting_limit_without_deficit).is_err());

        let posting_limit_with_other_deficit = SentenceRecoveryMetrics {
            near_pair_visits_attempted: 1,
            near_relation_stop_reason: Some(NearRelationStopReason::CandidatePostingVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(posting_limit_with_other_deficit).is_err());

        let incomplete_posting_stop = SentenceRecoveryMetrics {
            near_candidate_posting_visits_examined: 6,
            near_candidate_posting_visits_attempted: 7,
            near_line_work: NearSearchWorkMetrics {
                edge_posting_visits_examined: 6,
                edge_posting_visits_attempted: 7,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                line_work: NearSearchWorkMetrics {
                    edge_posting_visits_examined: 6,
                    edge_posting_visits_attempted: 7,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
            near_relation_stop_reason: Some(NearRelationStopReason::CandidatePostingVisitLimit),
            ..SentenceRecoveryMetrics::default()
        };
        let validated = validate_sentence_recovery_metrics(incomplete_posting_stop)
            .expect("posting visit limit explains an unexamined posting visit");
        assert_eq!(
            validated.near_relation_stop_reason,
            Some(NearRelationStopReasonReport::CandidatePostingVisitLimit)
        );

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
            near_sentence_work: NearSearchWorkMetrics {
                pair_visits_examined: 4,
                pair_visits_attempted: 5,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                sentence_work: NearSearchWorkMetrics {
                    pair_visits_examined: 4,
                    pair_visits_attempted: 5,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
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
            near_sentence_work: NearSearchWorkMetrics {
                similarity_comparisons_examined: 6,
                similarity_comparisons_attempted: 7,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                sentence_work: NearSearchWorkMetrics {
                    similarity_comparisons_examined: 6,
                    similarity_comparisons_attempted: 7,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
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
            near_line_work: NearSearchWorkMetrics {
                pair_visits_examined: 4,
                pair_visits_attempted: 4,
                similarity_comparisons_examined: 2,
                similarity_comparisons_attempted: 2,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                line_work: NearSearchWorkMetrics {
                    pair_visits_examined: 4,
                    pair_visits_attempted: 4,
                    similarity_comparisons_examined: 2,
                    similarity_comparisons_attempted: 2,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
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
            near_line_work: NearSearchWorkMetrics {
                pair_visits_examined: 4,
                pair_visits_attempted: 5,
                similarity_comparisons_examined: 2,
                similarity_comparisons_attempted: 2,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                line_work: NearSearchWorkMetrics {
                    pair_visits_examined: 4,
                    pair_visits_attempted: 5,
                    similarity_comparisons_examined: 2,
                    similarity_comparisons_attempted: 2,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
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
            near_line_work: NearSearchWorkMetrics {
                pair_visits_examined: 4,
                pair_visits_attempted: 4,
                similarity_comparisons_examined: 2,
                similarity_comparisons_attempted: 3,
                ..NearSearchWorkMetrics::default()
            },
            near_paired_interval_work: NearSearchScopeMetrics {
                line_work: NearSearchWorkMetrics {
                    pair_visits_examined: 4,
                    pair_visits_attempted: 4,
                    similarity_comparisons_examined: 2,
                    similarity_comparisons_attempted: 3,
                    ..NearSearchWorkMetrics::default()
                },
                ..NearSearchScopeMetrics::default()
            },
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
            serde_json::to_value(NearRelationStopReasonReport::CandidatePostingVisitLimit)
                .expect("stop reason serializes"),
            "candidate_posting_visit_limit"
        );
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
    fn validates_known_span_sentence_shadow_invariants() {
        let valid = SentenceRecoveryMetrics {
            known_span_sentence_shadow: Some(KnownSpanSentenceShadowMetrics {
                complete: true,
                pairs_considered: 5,
                pairs_retained: 3,
                pairs_rejected: 2,
                cross_span_pairs_considered: 5,
                same_paired_anchor_interval_pairs: 3,
                unclassified_pairs: 2,
                exact_relation_parity: true,
                ..KnownSpanSentenceShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        let report = validate_sentence_recovery_metrics(valid).expect("valid shadow metrics pass");
        assert_eq!(
            report
                .known_span_sentence_shadow
                .expect("shadow report is present")
                .pairs_rejected,
            2
        );
        let json = serde_json::to_value(report).expect("shadow report serializes");
        assert_eq!(json["known_span_sentence_shadow"]["pairs_considered"], 5);
        assert_eq!(
            json["known_span_sentence_shadow"]["exact_relation_parity"],
            true
        );

        let invalid_pair_sum = SentenceRecoveryMetrics {
            known_span_sentence_shadow: Some(KnownSpanSentenceShadowMetrics {
                pairs_considered: 5,
                pairs_retained: 2,
                pairs_rejected: 2,
                cross_span_pairs_considered: 5,
                unclassified_pairs: 5,
                exact_relation_parity: true,
                ..KnownSpanSentenceShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_pair_sum).is_err());

        let invalid_locality_sum = SentenceRecoveryMetrics {
            known_span_sentence_shadow: Some(KnownSpanSentenceShadowMetrics {
                pairs_considered: 5,
                pairs_retained: 5,
                cross_span_pairs_considered: 5,
                unclassified_pairs: 4,
                exact_relation_parity: true,
                ..KnownSpanSentenceShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_locality_sum).is_err());

        let invalid_cross_span_total = SentenceRecoveryMetrics {
            known_span_sentence_shadow: Some(KnownSpanSentenceShadowMetrics {
                pairs_considered: 5,
                pairs_retained: 5,
                cross_span_pairs_considered: 6,
                unclassified_pairs: 6,
                exact_relation_parity: true,
                ..KnownSpanSentenceShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_cross_span_total).is_err());

        let invalid_parity = SentenceRecoveryMetrics {
            known_span_sentence_shadow: Some(KnownSpanSentenceShadowMetrics {
                old_relation_mismatches: 1,
                exact_relation_parity: true,
                ..KnownSpanSentenceShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(invalid_parity).is_err());
    }

    #[test]
    fn converts_and_validates_sentence_edge_gate_shadow_metrics() {
        let shadow = SentenceEdgeGateShadowMetrics {
            complete: false,
            stop_reason: Some(SentenceEdgeGateShadowStopReason::AllocationFailure),
            pairs_considered: 4,
            pairs_retained: 1,
            pairs_rejected: 3,
            same_known_rejected: 1,
            ambiguous_rejected: 1,
            cross_span_rejected: 1,
            projected_pair_visits: 1,
            projected_similarity_comparisons: 2,
            rejected_max_production_score: 3_000,
            threshold_violations: 2,
            veto_mismatches: 3,
            unique_partner_mismatches: 4,
            reciprocal_pair_mismatches: 5,
            adopted_replacement_mismatches: 6,
            insertion_deletion_veto_mismatches: 7,
            ..SentenceEdgeGateShadowMetrics::default()
        };
        assert_eq!(
            SentenceEdgeGateShadowMetricsReport::from(shadow),
            SentenceEdgeGateShadowMetricsReport {
                complete: false,
                stop_reason: Some(SentenceEdgeGateShadowStopReasonReport::AllocationFailure),
                pairs_considered: 4,
                pairs_retained: 1,
                pairs_rejected: 3,
                same_known_rejected: 1,
                ambiguous_rejected: 1,
                cross_span_rejected: 1,
                projected_pair_visits: 1,
                projected_similarity_comparisons: 2,
                rejected_max_production_score: 3_000,
                threshold_violations: 2,
                veto_mismatches: 3,
                unique_partner_mismatches: 4,
                reciprocal_pair_mismatches: 5,
                adopted_replacement_mismatches: 6,
                insertion_deletion_veto_mismatches: 7,
                ..SentenceEdgeGateShadowMetricsReport::default()
            }
        );

        let valid = SentenceRecoveryMetrics {
            sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetrics {
                complete: true,
                pairs_considered: 2,
                pairs_rejected: 2,
                same_known_rejected: 1,
                ambiguous_rejected: 1,
                rejected_max_production_score: 3_000,
                threshold_violations: 1,
                ..SentenceEdgeGateShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(valid).is_ok());

        let invalid = [
            SentenceEdgeGateShadowMetrics {
                pairs_considered: 2,
                pairs_rejected: 1,
                ..valid.sentence_edge_gate_shadow.expect("shadow is present")
            },
            SentenceEdgeGateShadowMetrics {
                same_known_rejected: 0,
                ..valid.sentence_edge_gate_shadow.expect("shadow is present")
            },
            SentenceEdgeGateShadowMetrics {
                pairs_retained: 1,
                pairs_rejected: 1,
                projected_pair_visits: 0,
                ..valid.sentence_edge_gate_shadow.expect("shadow is present")
            },
            SentenceEdgeGateShadowMetrics {
                projected_similarity_comparisons: 1,
                ..valid.sentence_edge_gate_shadow.expect("shadow is present")
            },
            SentenceEdgeGateShadowMetrics {
                complete: false,
                ..valid.sentence_edge_gate_shadow.expect("shadow is present")
            },
            SentenceEdgeGateShadowMetrics {
                stop_reason: Some(SentenceEdgeGateShadowStopReason::DiagnosticFailure),
                ..valid.sentence_edge_gate_shadow.expect("shadow is present")
            },
        ];
        for shadow in invalid {
            assert!(
                validate_sentence_recovery_metrics(SentenceRecoveryMetrics {
                    sentence_edge_gate_shadow: Some(shadow),
                    ..SentenceRecoveryMetrics::default()
                })
                .is_err()
            );
        }
        let incomplete = SentenceRecoveryMetrics {
            sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetrics {
                complete: false,
                stop_reason: Some(SentenceEdgeGateShadowStopReason::CounterOverflow),
                ..SentenceEdgeGateShadowMetrics::default()
            }),
            ..SentenceRecoveryMetrics::default()
        };
        assert!(validate_sentence_recovery_metrics(incomplete).is_ok());
    }

    #[test]
    fn serializes_sentence_edge_gate_shadow_stop_reasons_as_snake_case() {
        for (reason, expected) in [
            (
                SentenceEdgeGateShadowStopReasonReport::CandidatePostingVisitLimit,
                "candidate_posting_visit_limit",
            ),
            (
                SentenceEdgeGateShadowStopReasonReport::PairVisitLimit,
                "pair_visit_limit",
            ),
            (
                SentenceEdgeGateShadowStopReasonReport::SimilarityComparisonLimit,
                "similarity_comparison_limit",
            ),
            (
                SentenceEdgeGateShadowStopReasonReport::CandidateCountLimit,
                "candidate_count_limit",
            ),
            (
                SentenceEdgeGateShadowStopReasonReport::AllocationFailure,
                "allocation_failure",
            ),
            (
                SentenceEdgeGateShadowStopReasonReport::CounterOverflow,
                "counter_overflow",
            ),
            (
                SentenceEdgeGateShadowStopReasonReport::DiagnosticFailure,
                "diagnostic_failure",
            ),
        ] {
            assert_eq!(
                serde_json::to_value(reason).expect("stop reason serializes"),
                expected
            );
        }
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
                    sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetricsReport {
                        complete: true,
                        pairs_considered: 2,
                        pairs_retained: 1,
                        pairs_rejected: 1,
                        same_known_rejected: 1,
                        projected_pair_visits: 1,
                        projected_similarity_comparisons: 1,
                        rejected_max_production_score: 2_999,
                        ..SentenceEdgeGateShadowMetricsReport::default()
                    }),
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
        assert_eq!(value["schema_version"], 24);

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
            "run_signature_available".to_owned(),
            "run_signature_complete".to_owned(),
            "old_run_signature_unique_units".to_owned(),
            "new_run_signature_unique_units".to_owned(),
            "old_run_signature_duplicate_units".to_owned(),
            "new_run_signature_duplicate_units".to_owned(),
            "run_signature_shared_unit_keys".to_owned(),
            "run_signature_largest_posting".to_owned(),
            "run_signature_posting_visits_attempted".to_owned(),
            "run_signature_posting_visits_examined".to_owned(),
            "run_signature_token_verifications_attempted".to_owned(),
            "run_signature_token_verifications_examined".to_owned(),
            "run_signature_candidate_pairs".to_owned(),
            "run_signature_globally_anchored_runs_skipped".to_owned(),
            "run_signature_reciprocal_unique_pairs".to_owned(),
            "run_signature_margin_qualified_pairs".to_owned(),
            "run_signature_margin_veto_pairs".to_owned(),
            "run_signature_monotone_pairs".to_owned(),
            "run_signature_crossing_veto_pairs".to_owned(),
            "run_signature_max_shared_units".to_owned(),
            "run_signature_stop_reason".to_owned(),
            "exact_shared_units".to_owned(),
            "old_exact_one_sided_units".to_owned(),
            "new_exact_one_sided_units".to_owned(),
            "near_relation_complete".to_owned(),
            "relation_floor_pairs_considered".to_owned(),
            "relation_floor_word_scans".to_owned(),
            "relation_floor_stop_opportunities".to_owned(),
            "relation_floor_potential_saved_word_comparisons".to_owned(),
            "near_sentence_work".to_owned(),
            "near_line_work".to_owned(),
            "near_paired_interval_work".to_owned(),
            "near_paired_cross_interval_veto_work".to_owned(),
            "near_same_or_ambiguous_span_work".to_owned(),
            "near_same_known_span_work".to_owned(),
            "near_ambiguous_span_work".to_owned(),
            "near_same_or_ambiguous_shared_query_work".to_owned(),
            "near_cross_span_work".to_owned(),
            "known_span_sentence_shadow".to_owned(),
            "sentence_edge_gate_shadow".to_owned(),
            "near_pair_visits_examined".to_owned(),
            "near_pair_visits_attempted".to_owned(),
            "near_similarity_comparisons_examined".to_owned(),
            "near_similarity_comparisons_attempted".to_owned(),
            "near_candidate_posting_visits_examined".to_owned(),
            "near_candidate_posting_visits_attempted".to_owned(),
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
        let expected_edge_gate_shadow_keys = HashSet::from([
            "complete".to_owned(),
            "stop_reason".to_owned(),
            "pairs_considered".to_owned(),
            "pairs_retained".to_owned(),
            "pairs_rejected".to_owned(),
            "same_known_rejected".to_owned(),
            "ambiguous_rejected".to_owned(),
            "cross_span_rejected".to_owned(),
            "unclassified_rejected".to_owned(),
            "projected_pair_visits".to_owned(),
            "projected_similarity_comparisons".to_owned(),
            "rejected_max_production_score".to_owned(),
            "threshold_violations".to_owned(),
            "veto_mismatches".to_owned(),
            "unique_partner_mismatches".to_owned(),
            "reciprocal_pair_mismatches".to_owned(),
            "adopted_replacement_mismatches".to_owned(),
            "insertion_deletion_veto_mismatches".to_owned(),
        ]);
        let edge_gate_shadow_keys =
            records[0]["sentence_recovery_metrics"]["sentence_edge_gate_shadow"]
                .as_object()
                .expect("sentence edge gate shadow object")
                .keys()
                .cloned()
                .collect::<HashSet<_>>();
        assert_eq!(edge_gate_shadow_keys, expected_edge_gate_shadow_keys);
        assert_eq!(
            records[0]["sentence_recovery_metrics"]["near_sentence_work"],
            serde_json::to_value(NearSearchWorkMetricsReport::default())
                .expect("near-search metrics serialize")
        );
        assert_eq!(
            records[0]["sentence_recovery_metrics"]["near_line_work"],
            serde_json::to_value(NearSearchWorkMetricsReport::default())
                .expect("near-search metrics serialize")
        );
        for scope in [
            "near_paired_interval_work",
            "near_paired_cross_interval_veto_work",
            "near_same_or_ambiguous_span_work",
            "near_same_known_span_work",
            "near_ambiguous_span_work",
            "near_same_or_ambiguous_shared_query_work",
            "near_cross_span_work",
        ] {
            assert_eq!(
                records[0]["sentence_recovery_metrics"][scope],
                serde_json::to_value(NearSearchScopeMetricsReport::default())
                    .expect("near-search scope metrics serialize")
            );
        }

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
                recovery_watch: None,
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
