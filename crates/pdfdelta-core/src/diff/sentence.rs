use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use sha2::{Digest, Sha256};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Result,
    alignment::{Alignment, AlignmentEvidence, AlignmentKind, BlockSeparator},
    layout::{
        BlockId, BlockRole, RegionId, RegionRelation, TrustedRegionEdge, TrustedRunDescriptor,
        TrustedRunId, TrustedRunInterval,
    },
    model::PageId,
    normalize::{ComparableToken, ScalarRange},
};

use super::recovery::candidate::{
    CandidatePostingBucket, CandidatePostingIndexScope, CandidatePostingKind, NearSearchWorkSplit,
    PairedInterval, SentenceEdgeSignatureDepthBand, SentenceEdgeSignatureIndex,
    SentenceEdgeSignatureIndexBuildError, SentenceEdgeSignatureIndexBuildLimits,
    SentenceEdgeSignatureIndexError, SentenceEdgeSignatureIndexMetrics,
    SentenceEdgeSignatureQueryMetrics, UnitCandidateIndex, UnitCandidateQueryMetrics,
    same_or_ambiguous_work_class, split_query_candidates,
};
pub(in crate::diff) use super::recovery::candidate::{NearSearchScope, NearSearchWorkClass};
use super::recovery::score::{
    CachedSentenceEdgeEvidence, MIN_NEAR_SCORE, MIN_WORD_SCORE_EDGE_EVIDENCE, RelationFloorProbe,
    basis_points, cached_sentence_edge_evidence, cached_sentence_edge_evidence_from_aligned_facts,
    sentence_edge_evidence, sentence_edge_evidence_from_cache,
    sentence_similarity_in_scope_attributed_from_edge_evidence,
    sentence_similarity_in_scope_attributed_from_edge_evidence_with_probe,
};
#[cfg(test)]
use super::recovery::{
    candidate::LineTrigramPosting,
    score::{
        LINE_NGRAM_SIZE, SentenceEdgeEvidence, line_trigram_candidate_meets_threshold,
        sentence_similarity_in_scope, sentence_similarity_in_scope_attributed,
        sentence_similarity_in_scope_attributed_with_probe, word_multiset_dice,
        word_multiset_dice_with_probe,
    },
};
use super::{
    ExactSegmentRelation, KnownSpanSentenceShadowMetrics, MAX_SENTENCE_RECOVERY_OUTPUT_BYTES,
    MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS, NearRelationStopReason, NearSearchScopeMetrics,
    NearSearchWorkMetrics, RecoveryWatchDiagnostics, RecoveryWatchGranularPairEvidence,
    RecoveryWatchGranularRelation, RecoveryWatchGranularStopReason,
    RecoveryWatchGranularUnitEvidence, RecoveryWatchNearScope, RecoveryWatchOccurrence,
    RecoveryWatchOccurrenceEvidence, RecoveryWatchOccurrences, RecoveryWatchPairEvidence,
    RecoveryWatchQuery, RecoveryWatchRecord, RecoveryWatchRelation,
    RecoveryWatchSegmentPairEvidence, RecoveryWatchUnitKind, RunSignatureStopReason,
    SegmentStopReason, SentenceEdgeFilterStopReason, SentenceEdgeGateShadowMetrics,
    SentenceEdgeGateShadowStopReason, SentenceEdgeSignatureDirectExecution,
    SentenceEdgeSignatureDirectShadowMetrics, SentenceEdgeSignatureDirectShadowStopReason,
    SentenceEdgeSignatureShadowMetrics, SentenceEdgeSignatureShadowStopReason,
    SentenceRecoveryCommittedTokens, SentenceRecoveryInput, SentenceRecoveryMetrics, Side,
    TokenRange, TrustedRunRecoveryInput,
};
#[cfg(test)]
use super::{
    SentenceEdgeSignatureReferenceOracleMetrics, SentenceEdgeSignatureReferenceOracleStopReason,
};

pub(super) const MAX_SENTENCE_RECOVERY_RANGES: usize = 8_192;
const MIN_NEAR_SCORE_MARGIN: u16 = 500;
const MIN_PAIRED_STREAM_EXACT_TOKENS: usize = 4;
const MIN_PAIRED_STREAM_NEAR_TOKENS: usize = 4;
const MAX_UNTRUSTED_LINE_NEAR_CANDIDATES: usize = 256;
pub(super) const MAX_RECOVERY_WATCH_QUERIES: usize = 4_096;
// One retained diagnostic consumes one of sixteen shares of the existing
// recovery output item budget. The cap is global across all watch queries.
const MAX_RECOVERY_WATCH_RETAINED_OCCURRENCES: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 16;
/// Larger single-line blocks are likely collapsed page or form regions rather
/// than independently comparable lines and remain unresolved.
pub(super) const MAX_UNTRUSTED_LINE_TOKENS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LocalSentenceRange {
    pub block: BlockId,
    pub canonical: ScalarRange,
    pub comparable: TokenRange,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RecoveredSentence {
    pub span_index: usize,
    pub kind: RecoveryUnitKind,
    pub role: OccurrenceRole,
    pub blocks: Vec<BlockId>,
    pub separator: Option<BlockSeparator>,
    pub canonical: ScalarRange,
    pub comparable: TokenRange,
    pub source_tokens: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RecoveredReplacement {
    pub old: RecoveredSentence,
    pub new: RecoveredSentence,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RecoveredExactMatch {
    pub old: RecoveredSentence,
    pub new: RecoveredSentence,
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct SentenceRecoveryPlan {
    pub matches: Vec<RecoveredExactMatch>,
    pub cross_span_match_old: Vec<RecoveredSentence>,
    pub cross_span_match_new: Vec<RecoveredSentence>,
    pub cross_span_replacement_new_spans: Vec<usize>,
    pub deletions: Vec<RecoveredSentence>,
    pub insertions: Vec<RecoveredSentence>,
    pub replacements: Vec<RecoveredReplacement>,
    pub deletion_consumed: Vec<LocalSentenceRange>,
    pub insertion_consumed: Vec<LocalSentenceRange>,
}

impl SentenceRecoveryPlan {
    fn has_exact_matches(&self) -> bool {
        !self.matches.is_empty()
            || !self.cross_span_match_old.is_empty()
            || !self.cross_span_match_new.is_empty()
    }

    pub fn has_cross_span_recovery(&self) -> bool {
        !self.cross_span_match_old.is_empty()
            || !self.cross_span_match_new.is_empty()
            || !self.cross_span_replacement_new_spans.is_empty()
    }

    pub fn has_repeated_recovery_candidates(&self) -> bool {
        [&self.deletions, &self.insertions]
            .into_iter()
            .any(|recoveries| {
                let mut seen = [false; 4];
                recoveries.iter().any(|recovery| {
                    let index = match (recovery.kind, recovery.role) {
                        (_, OccurrenceRole::Body) => return false,
                        (RecoveryUnitKind::Sentence, OccurrenceRole::RepeatedHeader) => 0,
                        (RecoveryUnitKind::Sentence, OccurrenceRole::RepeatedFooter) => 1,
                        (RecoveryUnitKind::Line, OccurrenceRole::RepeatedHeader) => 2,
                        (RecoveryUnitKind::Line, OccurrenceRole::RepeatedFooter) => 3,
                    };
                    std::mem::replace(&mut seen[index], true)
                })
            })
    }

    pub fn has_recovery(&self, span_index: usize) -> bool {
        !matches_for_span(&self.matches, span_index).is_empty()
            || !recoveries_for_span(&self.cross_span_match_old, span_index).is_empty()
            || !recoveries_for_span(&self.cross_span_match_new, span_index).is_empty()
            || !recoveries_for_span(&self.deletions, span_index).is_empty()
            || !recoveries_for_span(&self.insertions, span_index).is_empty()
            || !replacements_for_span(&self.replacements, span_index).is_empty()
            || self
                .cross_span_replacement_new_spans
                .binary_search(&span_index)
                .is_ok()
    }
}

#[derive(Default)]
pub(super) struct SentenceRecoveryBuildOutcome {
    pub plan: Option<SentenceRecoveryPlan>,
    diagnostics: Option<SentenceRecoveryDiagnostics>,
    watch_diagnostics: Option<RecoveryWatchDiagnostics>,
    fragment_veto_complete: bool,
    fragment_veto_stop_reason: Option<FragmentVetoStopReason>,
    fragment_veto_pair_visits_examined: usize,
    fragment_veto_pair_visits_attempted: usize,
    fragment_veto_comparisons_examined: usize,
    fragment_veto_comparisons_attempted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FragmentVetoStopReason {
    PairVisitLimit,
    SimilarityComparisonLimit,
    AllocationFailure,
    CounterOverflow,
    InvalidEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SentenceEdgeFilterMode {
    Filtered,
    /// Disables only production pruning. Diagnostic shadow replay remains
    /// controlled by [`SentenceRecoveryInput::enable_sentence_edge_gate_shadow`].
    Legacy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SentenceEdgeSignatureFilterMode {
    #[default]
    Disabled,
    PostUnion,
    Direct,
    /// Observes broad legacy candidates without consulting or pruning through
    /// the sentence-edge signature index.
    ReferenceObserve,
}

#[derive(Clone, Copy)]
struct SentenceEdgeFilterAttemptMetrics {
    complete: bool,
    pairs_examined: usize,
    pairs_attempted: usize,
    comparisons_examined: usize,
    comparisons_attempted: usize,
    pairs_retained: usize,
    pairs_rejected: usize,
    stop_reason: Option<SentenceEdgeFilterStopReason>,
    near_pair_visits_examined: usize,
    near_pair_visits_attempted: usize,
    near_similarity_comparisons_examined: usize,
    near_similarity_comparisons_attempted: usize,
    near_candidate_posting_visits_examined: usize,
    near_candidate_posting_visits_attempted: usize,
}

impl SentenceEdgeFilterAttemptMetrics {
    fn from_metrics(metrics: &SentenceRecoveryMetrics) -> Self {
        Self {
            complete: metrics.sentence_edge_filter_complete,
            pairs_examined: metrics.sentence_edge_filter_pairs_examined,
            pairs_attempted: metrics.sentence_edge_filter_pairs_attempted,
            comparisons_examined: metrics.sentence_edge_filter_similarity_comparisons_examined,
            comparisons_attempted: metrics.sentence_edge_filter_similarity_comparisons_attempted,
            pairs_retained: metrics.sentence_edge_filter_pairs_retained,
            pairs_rejected: metrics.sentence_edge_filter_pairs_rejected,
            stop_reason: metrics.sentence_edge_filter_stop_reason,
            near_pair_visits_examined: metrics.near_pair_visits_examined,
            near_pair_visits_attempted: metrics.near_pair_visits_attempted,
            near_similarity_comparisons_examined: metrics.near_similarity_comparisons_examined,
            near_similarity_comparisons_attempted: metrics.near_similarity_comparisons_attempted,
            near_candidate_posting_visits_examined: metrics.near_candidate_posting_visits_examined,
            near_candidate_posting_visits_attempted: metrics
                .near_candidate_posting_visits_attempted,
        }
    }

    fn apply(self, metrics: &mut SentenceRecoveryMetrics) {
        metrics.sentence_edge_filter_complete = self.complete;
        metrics.sentence_edge_filter_pairs_examined = self.pairs_examined;
        metrics.sentence_edge_filter_pairs_attempted = self.pairs_attempted;
        metrics.sentence_edge_filter_similarity_comparisons_examined = self.comparisons_examined;
        metrics.sentence_edge_filter_similarity_comparisons_attempted = self.comparisons_attempted;
        metrics.sentence_edge_filter_pairs_retained = self.pairs_retained;
        metrics.sentence_edge_filter_pairs_rejected = self.pairs_rejected;
        metrics.sentence_edge_filter_stop_reason = self.stop_reason;
        metrics.sentence_edge_filter_full_build_fallback_used = true;
        metrics.sentence_edge_filter_discarded_near_pair_visits_examined =
            self.near_pair_visits_examined;
        metrics.sentence_edge_filter_discarded_near_pair_visits_attempted =
            self.near_pair_visits_attempted;
        metrics.sentence_edge_filter_discarded_near_similarity_comparisons_examined =
            self.near_similarity_comparisons_examined;
        metrics.sentence_edge_filter_discarded_near_similarity_comparisons_attempted =
            self.near_similarity_comparisons_attempted;
        metrics.sentence_edge_filter_discarded_near_candidate_posting_visits_examined =
            self.near_candidate_posting_visits_examined;
        metrics.sentence_edge_filter_discarded_near_candidate_posting_visits_attempted =
            self.near_candidate_posting_visits_attempted;
    }
}

#[derive(Clone, Copy)]
struct SentenceRecoveryDiagnostics {
    metrics: SentenceRecoveryMetrics,
    eligible_old_source_tokens: usize,
    eligible_new_source_tokens: usize,
    signature_retained_fingerprint: SentenceEdgeRetainedFingerprint,
    signature_retained_fingerprint_valid: bool,
}

impl SentenceRecoveryBuildOutcome {
    pub(super) fn record_committed(&mut self, committed: SentenceRecoveryCommittedTokens) {
        let Some(diagnostics) = self.diagnostics.as_mut() else {
            return;
        };
        let Some(metrics) = checked_committed_metrics(diagnostics.metrics, committed) else {
            self.diagnostics = None;
            return;
        };
        diagnostics.metrics = metrics;
    }

    pub(super) fn finish_diagnostics(
        self,
    ) -> (
        Option<SentenceRecoveryMetrics>,
        Option<RecoveryWatchDiagnostics>,
    ) {
        let metrics = self.diagnostics.and_then(|diagnostics| {
            let mut metrics = diagnostics.metrics;
            let recovered_old = metrics
                .recovered_exact_match_old_tokens
                .checked_add(metrics.recovered_replacement_old_tokens)?
                .checked_add(metrics.recovered_deletion_tokens)?;
            let recovered_new = metrics
                .recovered_exact_match_new_tokens
                .checked_add(metrics.recovered_replacement_new_tokens)?
                .checked_add(metrics.recovered_insertion_tokens)?;
            metrics.unresolved_remainder_old_source_tokens = diagnostics
                .eligible_old_source_tokens
                .checked_sub(recovered_old)?;
            metrics.unresolved_remainder_new_source_tokens = diagnostics
                .eligible_new_source_tokens
                .checked_sub(recovered_new)?;
            Some(metrics)
        });
        (metrics, self.watch_diagnostics)
    }
}

fn checked_committed_metrics(
    mut metrics: SentenceRecoveryMetrics,
    committed: SentenceRecoveryCommittedTokens,
) -> Option<SentenceRecoveryMetrics> {
    metrics.recovered_exact_match_old_tokens = metrics
        .recovered_exact_match_old_tokens
        .checked_add(committed.exact_match_old)?;
    metrics.recovered_exact_match_new_tokens = metrics
        .recovered_exact_match_new_tokens
        .checked_add(committed.exact_match_new)?;
    metrics.recovered_replacement_old_tokens = metrics
        .recovered_replacement_old_tokens
        .checked_add(committed.replacement_old)?;
    metrics.recovered_replacement_new_tokens = metrics
        .recovered_replacement_new_tokens
        .checked_add(committed.replacement_new)?;
    metrics.recovered_deletion_tokens = metrics
        .recovered_deletion_tokens
        .checked_add(committed.deletion)?;
    metrics.recovered_insertion_tokens = metrics
        .recovered_insertion_tokens
        .checked_add(committed.insertion)?;
    Some(metrics)
}

pub(super) fn recoveries_for_span(
    recoveries: &[RecoveredSentence],
    span_index: usize,
) -> &[RecoveredSentence] {
    let start = recoveries.partition_point(|recovery| recovery.span_index < span_index);
    let end =
        recoveries[start..].partition_point(|recovery| recovery.span_index == span_index) + start;
    &recoveries[start..end]
}

pub(super) fn replacements_for_span(
    replacements: &[RecoveredReplacement],
    span_index: usize,
) -> &[RecoveredReplacement] {
    let start = replacements.partition_point(|replacement| replacement.old.span_index < span_index);
    let end = replacements[start..]
        .partition_point(|replacement| replacement.old.span_index == span_index)
        + start;
    &replacements[start..end]
}

pub(super) fn matches_for_span(
    matches: &[RecoveredExactMatch],
    span_index: usize,
) -> &[RecoveredExactMatch] {
    let start = matches.partition_point(|matched| matched.old.span_index < span_index);
    let end =
        matches[start..].partition_point(|matched| matched.old.span_index == span_index) + start;
    &matches[start..end]
}

pub(super) fn ranges_for_block(
    ranges: &[LocalSentenceRange],
    block: BlockId,
) -> &[LocalSentenceRange] {
    let start = ranges.partition_point(|range| range.block < block);
    let end = ranges[start..].partition_point(|range| range.block == block) + start;
    &ranges[start..end]
}

#[derive(Clone, Copy)]
enum OccurrenceSide {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum SentenceEvidenceToken {
    Scalar(char),
    Unmapped {
        font_fingerprint: u64,
        glyph_id: u16,
    },
}

impl SentenceEvidenceToken {
    fn is_scalar(self) -> bool {
        matches!(self, Self::Scalar(_))
    }

    fn is_space(self) -> bool {
        matches!(self, Self::Scalar(scalar) if scalar.is_whitespace())
    }
}

impl From<&ComparableToken> for SentenceEvidenceToken {
    fn from(token: &ComparableToken) -> Self {
        match token {
            ComparableToken::Scalar(scalar) => Self::Scalar(*scalar),
            ComparableToken::Unmapped {
                font_hash,
                glyph_id,
            } => Self::Unmapped {
                font_fingerprint: fingerprint_font_program(&font_hash.0),
                glyph_id: *glyph_id,
            },
        }
    }
}

/// Fingerprints font bytes without retaining or allocating their owned hash.
///
/// Collisions can only make unrelated unmapped evidence compare equal during
/// near-match vetoing. That over-vetoes recovery and therefore fails closed.
fn fingerprint_font_program(bytes: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut fingerprint = FNV_OFFSET_BASIS;
    for byte in bytes {
        fingerprint ^= u64::from(*byte);
        fingerprint = fingerprint.wrapping_mul(FNV_PRIME);
    }
    fingerprint
}

pub(super) struct SentenceOccurrence {
    pub(super) key: String,
    pub(super) tokens: Vec<SentenceEvidenceToken>,
    pub(super) word_ranges: Vec<Range<usize>>,
    pub(super) kind: RecoveryUnitKind,
    pub(super) role: Option<BlockRole>,
    location: Option<SentenceLocation>,
    pub(super) span_index: Option<usize>,
    trusted_position: Option<TrustedStreamPosition>,
    run_descriptor_index: Option<usize>,
    /// Source page when every contributing block names the same single page.
    page: Option<u32>,
    evidence_block_index: Option<usize>,
}

struct SentenceFragment {
    tokens: Vec<SentenceEvidenceToken>,
    span_index: usize,
    role: BlockRole,
    uncertain: bool,
}

struct FragmentIndex<'a> {
    uncertain_spans: HashSet<(usize, OccurrenceRole)>,
    clean_by_span_and_len: HashMap<(usize, usize, OccurrenceRole), Vec<&'a SentenceFragment>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum RecoveryUnitKind {
    Sentence,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum OccurrenceRole {
    Body,
    RepeatedHeader,
    RepeatedFooter,
}

impl From<BlockRole> for OccurrenceRole {
    fn from(role: BlockRole) -> Self {
        match role {
            BlockRole::Body => Self::Body,
            BlockRole::RepeatedHeader => Self::RepeatedHeader,
            BlockRole::RepeatedFooter => Self::RepeatedFooter,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct TrustedStreamPosition {
    stream_index: usize,
    ordinal: usize,
}

struct SentenceLocation {
    recovery: RecoveredSentence,
    consumed: Vec<LocalSentenceRange>,
}

#[derive(Default)]
struct OccurrenceCount {
    old: usize,
    new: usize,
    old_index: Option<usize>,
    new_index: Option<usize>,
}

type OccurrenceKey<'a> = (&'a str, RecoveryUnitKind, OccurrenceRole);

struct RecoveryCandidate {
    occurrence_index: usize,
    span_index: usize,
}

struct RecoveryCandidates {
    values: Vec<RecoveryCandidate>,
    complete: bool,
}

#[derive(Default)]
struct PairedNearCandidates {
    recoveries: Vec<RecoveryCandidate>,
    intervals: Vec<PairedInterval>,
    ordinals: Vec<usize>,
}

#[derive(Default)]
struct PairedNearVetoes {
    old_occurrences: Vec<usize>,
    new_occurrences: Vec<usize>,
}

struct ExactMatchCandidate {
    old_span_index: usize,
    new_span_index: usize,
    old_occurrence_index: usize,
    new_occurrence_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ExactHash(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RecoverySegmentKey {
    stream_index: usize,
    start_ordinal: usize,
    end_ordinal: usize,
}

#[derive(Clone, Copy)]
struct RecoverySegment {
    key: RecoverySegmentKey,
    _span_index: usize,
    occurrence_indices: [usize; 8],
    unit_count: usize,
    role: BlockRole,
    _source_token_count: usize,
    token_count: usize,
    exact_hash: ExactHash,
}

#[derive(Default)]
struct SegmentDiagnosticAnalysis {
    old: Vec<RecoverySegment>,
    new: Vec<RecoverySegment>,
    old_index: HashMap<ExactHash, Vec<usize>>,
    new_index: HashMap<ExactHash, Vec<usize>>,
    exact_pairs: Vec<(usize, usize)>,
    old_cross_counts: Vec<usize>,
    new_cross_counts: Vec<usize>,
    old_consumed: HashMap<usize, Vec<LocalSentenceRange>>,
    new_consumed: HashMap<usize, Vec<LocalSentenceRange>>,
    unique_pairs: Vec<(usize, usize)>,
    budget: SegmentDiagnosticBudget,
    segment_candidates: usize,
    segment_hash_matches: usize,
    segment_token_verified_matches: usize,
    segment_unique_pairs: usize,
    segment_duplicate_pairs: usize,
    segment_monotone_pairs: usize,
    segment_crossing_pairs: usize,
}

#[derive(Clone, Copy, Default)]
struct SegmentDiagnosticBudget {
    descriptors: usize,
    descriptor_bytes: usize,
    hash_pair_visits: usize,
    token_elements: usize,
    topology_anchor_scans: usize,
    output_evidence: usize,
    retained_location_items: usize,
    retained_location_bytes: usize,
    auxiliary_items: usize,
    auxiliary_bytes: usize,
    overlap_comparisons: usize,
}

impl SegmentDiagnosticBudget {
    const DESCRIPTOR_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 4;
    const DESCRIPTOR_BYTE_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_BYTES / 4;
    const HASH_PAIR_VISIT_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS * 2;
    const TOKEN_ELEMENT_LIMIT: usize =
        MAX_SENTENCE_RECOVERY_OUTPUT_BYTES / std::mem::size_of::<SentenceEvidenceToken>();
    const TOPOLOGY_ANCHOR_SCAN_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS * 8;
    const AUXILIARY_ITEM_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 2;
    const AUXILIARY_BYTE_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_BYTES / 4;
    const OVERLAP_COMPARISON_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS * 8;

    fn charge_descriptor(&mut self) -> std::result::Result<(), SegmentStopReason> {
        self.descriptors = self
            .descriptors
            .checked_add(1)
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        self.descriptor_bytes = self
            .descriptor_bytes
            .checked_add(std::mem::size_of::<RecoverySegment>())
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        if self.descriptors > Self::DESCRIPTOR_LIMIT
            || self.descriptor_bytes > Self::DESCRIPTOR_BYTE_LIMIT
        {
            return Err(SegmentStopReason::CandidateCountLimit);
        }
        Ok(())
    }

    fn charge_hash_pairs(&mut self, amount: usize) -> std::result::Result<(), SegmentStopReason> {
        self.hash_pair_visits = self
            .hash_pair_visits
            .checked_add(amount)
            .ok_or(SegmentStopReason::HashPairVisitLimit)?;
        if self.hash_pair_visits > Self::HASH_PAIR_VISIT_LIMIT {
            return Err(SegmentStopReason::HashPairVisitLimit);
        }
        Ok(())
    }

    fn charge_token_elements(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), SegmentStopReason> {
        self.token_elements = self
            .token_elements
            .checked_add(amount)
            .ok_or(SegmentStopReason::TokenVerificationLimit)?;
        if self.token_elements > Self::TOKEN_ELEMENT_LIMIT {
            return Err(SegmentStopReason::TokenVerificationLimit);
        }
        Ok(())
    }

    fn charge_topology_anchor_scans(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), SegmentStopReason> {
        self.topology_anchor_scans = self
            .topology_anchor_scans
            .checked_add(amount)
            .ok_or(SegmentStopReason::HashPairVisitLimit)?;
        if self.topology_anchor_scans > Self::TOPOLOGY_ANCHOR_SCAN_LIMIT {
            return Err(SegmentStopReason::HashPairVisitLimit);
        }
        Ok(())
    }

    fn charge_output(&mut self) -> std::result::Result<(), SegmentStopReason> {
        self.output_evidence = self
            .output_evidence
            .checked_add(1)
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        if self.output_evidence > MAX_RECOVERY_WATCH_QUERIES {
            return Err(SegmentStopReason::CandidateCountLimit);
        }
        Ok(())
    }

    fn charge_retained_locations(
        &mut self,
        items: usize,
    ) -> std::result::Result<(), SegmentStopReason> {
        self.retained_location_items = self
            .retained_location_items
            .checked_add(items)
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        self.retained_location_bytes = self
            .retained_location_bytes
            .checked_add(
                items
                    .checked_mul(std::mem::size_of::<LocalSentenceRange>())
                    .ok_or(SegmentStopReason::CandidateCountLimit)?,
            )
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        if self.retained_location_items > MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 2
            || self.retained_location_bytes > MAX_SENTENCE_RECOVERY_OUTPUT_BYTES / 4
        {
            return Err(SegmentStopReason::CandidateCountLimit);
        }
        Ok(())
    }

    fn charge_auxiliary<T>(&mut self, items: usize) -> std::result::Result<(), SegmentStopReason> {
        self.auxiliary_items = self
            .auxiliary_items
            .checked_add(items)
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        self.auxiliary_bytes = self
            .auxiliary_bytes
            .checked_add(
                items
                    .checked_mul(std::mem::size_of::<T>())
                    .ok_or(SegmentStopReason::CandidateCountLimit)?,
            )
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        if self.auxiliary_items > Self::AUXILIARY_ITEM_LIMIT
            || self.auxiliary_bytes > Self::AUXILIARY_BYTE_LIMIT
        {
            return Err(SegmentStopReason::CandidateCountLimit);
        }
        Ok(())
    }

    fn charge_overlap_comparison(&mut self) -> std::result::Result<(), SegmentStopReason> {
        self.overlap_comparisons = self
            .overlap_comparisons
            .checked_add(1)
            .ok_or(SegmentStopReason::HashPairVisitLimit)?;
        if self.overlap_comparisons > Self::OVERLAP_COMPARISON_LIMIT {
            return Err(SegmentStopReason::HashPairVisitLimit);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CandidateNearRelation {
    best_score: u16,
    second_score: u16,
    best_partner: Option<usize>,
}

struct RecoveryWatchState {
    complete: bool,
    candidate_generation_complete: bool,
    near_relation_complete: bool,
    near_relation_stop_reason: Option<NearRelationStopReason>,
    segment_analysis: SegmentDiagnosticAnalysis,
    segment_stop_reason: Option<SegmentStopReason>,
    segment_overlap_vetoes: usize,
    granular_complete: bool,
    granular_old_units: usize,
    granular_new_units: usize,
    granular_pair_comparisons: usize,
    granular_stop_reason: Option<RecoveryWatchGranularStopReason>,
    records: Vec<RecoveryWatchStateRecord>,
    pair_by_occurrences: HashMap<(usize, usize), Vec<usize>>,
    old_pair_partners: HashMap<usize, Vec<usize>>,
    new_pair_partners: HashMap<usize, Vec<usize>>,
    scan_work: usize,
    scan_limit: usize,
    retained_one_sided_occurrences: usize,
}

struct RecoveryWatchStateRecord {
    output: RecoveryWatchRecord,
    old_occurrence: Option<usize>,
    new_occurrence: Option<usize>,
    old_segment: Option<usize>,
    new_segment: Option<usize>,
}

struct RecoveryWatchLookup {
    evidence: RecoveryWatchOccurrenceEvidence,
    occurrence_index: Option<usize>,
    span_index: Option<usize>,
    segment_key: Option<RecoverySegmentKey>,
}

fn push_unique_watch_partner(
    partners: &mut HashMap<usize, Vec<usize>>,
    occurrence: usize,
    partner: usize,
) -> Option<()> {
    let values = partners.entry(occurrence).or_default();
    if !values.contains(&partner) {
        values.try_reserve(1).ok()?;
        values.push(partner);
    }
    Some(())
}

impl RecoveryWatchLookup {
    fn not_queried() -> Self {
        Self {
            evidence: RecoveryWatchOccurrenceEvidence::NotQueried,
            occurrence_index: None,
            span_index: None,
            segment_key: None,
        }
    }

    fn unfound() -> Self {
        Self {
            evidence: RecoveryWatchOccurrenceEvidence::Unfound,
            occurrence_index: None,
            span_index: None,
            segment_key: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            evidence: RecoveryWatchOccurrenceEvidence::Unavailable,
            occurrence_index: None,
            span_index: None,
            segment_key: None,
        }
    }
}

fn recovery_watch_occurrence(
    occurrence: &SentenceOccurrence,
    descriptor: Option<&TrustedRunDescriptor>,
    fully_contained: bool,
    min_tokens: usize,
) -> RecoveryWatchOccurrence {
    RecoveryWatchOccurrence {
        span_index: occurrence.span_index,
        trusted_run_descriptor_index: occurrence.run_descriptor_index,
        ordinal: occurrence.trusted_position.map(|position| position.ordinal),
        end_ordinal: None,
        unit_count: None,
        token_count: None,
        recovery_location_available: occurrence.location.is_some()
            && occurrence.tokens.len() >= min_tokens,
        fully_contained,
        page: descriptor
            .map(|descriptor| descriptor.page.0)
            .or(occurrence.page),
        bbox: descriptor.map(|descriptor| descriptor.bbox),
        role: occurrence.role,
        kind: match occurrence.kind {
            RecoveryUnitKind::Sentence => RecoveryWatchUnitKind::Sentence,
            RecoveryUnitKind::Line => RecoveryWatchUnitKind::Line,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RecoveryWatchSegmentHit {
    stream_index: usize,
    start_ordinal: usize,
    end_ordinal: usize,
    unit_count: usize,
    token_count: usize,
    byte_start: usize,
    byte_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RecoveryWatchSegmentDedupeKey {
    stream_index: usize,
    start_ordinal: usize,
    byte_start: usize,
    byte_end: usize,
}

struct RecoveryWatchBuildContext<'a> {
    old_evidence: Option<&'a RunRecoveryEvidence<'a>>,
    new_evidence: Option<&'a RunRecoveryEvidence<'a>>,
    old_fully_contained: Option<&'a [bool]>,
    new_fully_contained: Option<&'a [bool]>,
    min_tokens: usize,
    max_tokens: usize,
}

fn exact_segment_hash(tokens: &[SentenceEvidenceToken]) -> ExactHash {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    let mut hash = ExactHash(OFFSET);
    for token in tokens {
        extend_exact_segment_hash(&mut hash, *token);
    }
    hash
}

fn extend_exact_segment_hash(hash: &mut ExactHash, token: SentenceEvidenceToken) {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    match token {
        SentenceEvidenceToken::Scalar(scalar) => {
            hash.0 ^= 0;
            hash.0 = hash.0.wrapping_mul(PRIME);
            for byte in u32::from(scalar).to_le_bytes() {
                hash.0 ^= u64::from(byte);
                hash.0 = hash.0.wrapping_mul(PRIME);
            }
        }
        SentenceEvidenceToken::Unmapped {
            font_fingerprint,
            glyph_id,
        } => {
            hash.0 ^= 1;
            hash.0 = hash.0.wrapping_mul(PRIME);
            for byte in font_fingerprint
                .to_le_bytes()
                .into_iter()
                .chain(glyph_id.to_le_bytes())
            {
                hash.0 ^= u64::from(byte);
                hash.0 = hash.0.wrapping_mul(PRIME);
            }
        }
    }
}

fn segment_tokens<'a>(
    segment: &'a RecoverySegment,
    occurrences: &'a [SentenceOccurrence],
) -> impl Iterator<Item = &'a SentenceEvidenceToken> + 'a {
    segment.occurrence_indices[..segment.unit_count]
        .iter()
        .filter_map(|index| occurrences.get(*index))
        .flat_map(|occurrence| occurrence.tokens.iter())
}

fn exact_segment_tokens_match(
    old: &RecoverySegment,
    new: &RecoverySegment,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
) -> bool {
    old.exact_hash == new.exact_hash
        && old.token_count == new.token_count
        && segment_tokens(old, old_occurrences).eq(segment_tokens(new, new_occurrences))
}

fn collect_recovery_segments_with_budget(
    occurrences: &[SentenceOccurrence],
    min_tokens: usize,
    budget: &mut SegmentDiagnosticBudget,
) -> std::result::Result<Vec<RecoverySegment>, SegmentStopReason> {
    const MAX_SEGMENT_UNITS: usize = 8;

    let mut by_position = Vec::<(TrustedStreamPosition, Option<usize>)>::new();
    by_position
        .try_reserve(
            occurrences
                .len()
                .min(SegmentDiagnosticBudget::AUXILIARY_ITEM_LIMIT),
        )
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    for (index, occurrence) in occurrences.iter().enumerate() {
        let Some(position) = occurrence.trusted_position else {
            continue;
        };
        budget.charge_auxiliary::<(TrustedStreamPosition, Option<usize>)>(1)?;
        by_position.push((position, Some(index)));
    }
    by_position.sort_unstable_by_key(|entry| entry.0);
    let mut read = 0usize;
    let mut write = 0usize;
    while read < by_position.len() {
        let position = by_position[read].0;
        let end = read + by_position[read..].partition_point(|entry| entry.0 == position);
        by_position[write] = (
            position,
            if end - read == 1 {
                by_position[read].1
            } else {
                None
            },
        );
        write += 1;
        read = end;
    }
    by_position.truncate(write);

    let position_index = |position: TrustedStreamPosition| {
        by_position
            .binary_search_by_key(&position, |entry| entry.0)
            .ok()
            .and_then(|index| by_position.get(index)?.1)
    };

    let mut segments = Vec::new();
    segments
        .try_reserve(
            occurrences
                .len()
                .min(SegmentDiagnosticBudget::DESCRIPTOR_LIMIT),
        )
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    for (first_index, first) in occurrences.iter().enumerate() {
        let (Some(position), Some(span_index), Some(role), Some(first_location)) = (
            first.trusted_position,
            first.span_index,
            first.role,
            first.location.as_ref(),
        ) else {
            continue;
        };
        if first.tokens.iter().any(|token| !token.is_scalar()) {
            continue;
        }
        let mut occurrence_indices = [usize::MAX; MAX_SEGMENT_UNITS];
        let mut token_count = first.tokens.len();
        let mut source_token_count = first_location.recovery.source_tokens;
        occurrence_indices[0] = first_index;
        budget.charge_token_elements(first.tokens.len())?;
        let mut exact_hash = exact_segment_hash(&first.tokens);

        for offset in 1..MAX_SEGMENT_UNITS {
            let ordinal = position
                .ordinal
                .checked_add(offset)
                .ok_or(SegmentStopReason::CandidateCountLimit)?;
            let Some(next_index) = position_index(TrustedStreamPosition {
                stream_index: position.stream_index,
                ordinal,
            }) else {
                break;
            };
            let next = occurrences
                .get(next_index)
                .ok_or(SegmentStopReason::CandidateCountLimit)?;
            let Some(next_location) = next.location.as_ref() else {
                break;
            };
            if next.span_index != Some(span_index)
                || next.role != Some(role)
                || next.tokens.iter().any(|token| !token.is_scalar())
            {
                break;
            }
            occurrence_indices[offset] = next_index;
            token_count = token_count
                .checked_add(next.tokens.len())
                .ok_or(SegmentStopReason::TokenVerificationLimit)?;
            budget.charge_token_elements(next.tokens.len())?;
            for token in &next.tokens {
                extend_exact_segment_hash(&mut exact_hash, *token);
            }
            source_token_count = source_token_count
                .checked_add(next_location.recovery.source_tokens)
                .ok_or(SegmentStopReason::CandidateCountLimit)?;
            if token_count < min_tokens {
                continue;
            }
            budget.charge_descriptor()?;
            let end_ordinal = ordinal
                .checked_add(1)
                .ok_or(SegmentStopReason::CandidateCountLimit)?;
            segments.push(RecoverySegment {
                key: RecoverySegmentKey {
                    stream_index: position.stream_index,
                    start_ordinal: position.ordinal,
                    end_ordinal,
                },
                _span_index: span_index,
                occurrence_indices,
                unit_count: offset + 1,
                role,
                _source_token_count: source_token_count,
                token_count,
                exact_hash,
            });
        }
    }
    Ok(segments)
}

#[cfg(test)]
fn collect_recovery_segments(
    occurrences: &[SentenceOccurrence],
    min_tokens: usize,
    _candidate_limit: usize,
) -> std::result::Result<Vec<RecoverySegment>, SegmentStopReason> {
    collect_recovery_segments_with_budget(
        occurrences,
        min_tokens,
        &mut SegmentDiagnosticBudget::default(),
    )
}

fn snapshot_segment_consumed_ranges(
    occurrences: &[SentenceOccurrence],
    segments: &[RecoverySegment],
    budget: &mut SegmentDiagnosticBudget,
) -> std::result::Result<HashMap<usize, Vec<LocalSentenceRange>>, SegmentStopReason> {
    let referenced_limit = segments
        .len()
        .checked_mul(8)
        .ok_or(SegmentStopReason::CandidateCountLimit)?
        .min(SegmentDiagnosticBudget::AUXILIARY_ITEM_LIMIT);
    let mut snapshots = HashMap::new();
    snapshots
        .try_reserve(referenced_limit)
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    for segment in segments {
        for index in &segment.occurrence_indices[..segment.unit_count] {
            if snapshots.contains_key(index) {
                continue;
            }
            budget.charge_auxiliary::<(usize, Vec<LocalSentenceRange>)>(1)?;
            let consumed = &occurrences
                .get(*index)
                .and_then(|occurrence| occurrence.location.as_ref())
                .ok_or(SegmentStopReason::CandidateCountLimit)?
                .consumed;
            budget.charge_retained_locations(consumed.len())?;
            let mut snapshot = Vec::new();
            snapshot
                .try_reserve_exact(consumed.len())
                .map_err(|_| SegmentStopReason::AllocationFailure)?;
            snapshot.extend_from_slice(consumed);
            snapshots.insert(*index, snapshot);
        }
    }
    Ok(snapshots)
}

fn segment_topology_with_budget(
    old: &RecoverySegment,
    new: &RecoverySegment,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
    budget: &mut SegmentDiagnosticBudget,
) -> std::result::Result<(ExactSegmentRelation, usize), SegmentStopReason> {
    let mut old_partners = HashSet::new();
    let mut new_partners = HashSet::new();
    let mut anchors = Vec::new();
    old_partners
        .try_reserve(2)
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    new_partners
        .try_reserve(2)
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    anchors
        .try_reserve(exact_candidates.len())
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    for candidate in exact_candidates {
        budget.charge_topology_anchor_scans(1)?;
        let (Some(old_position), Some(new_position)) = (
            old_occurrences
                .get(candidate.old_occurrence_index)
                .and_then(|occurrence| occurrence.trusted_position),
            new_occurrences
                .get(candidate.new_occurrence_index)
                .and_then(|occurrence| occurrence.trusted_position),
        ) else {
            continue;
        };
        if old_position.stream_index == old.key.stream_index {
            old_partners.insert(new_position.stream_index);
        }
        if new_position.stream_index == new.key.stream_index {
            new_partners.insert(old_position.stream_index);
        }
        if old_position.stream_index == old.key.stream_index
            && new_position.stream_index == new.key.stream_index
        {
            anchors.push((old_position.ordinal, new_position.ordinal));
        }
    }
    if old_partners.len() != 1
        || new_partners.len() != 1
        || !old_partners.contains(&new.key.stream_index)
        || !new_partners.contains(&old.key.stream_index)
        || anchors.is_empty()
    {
        return Ok((ExactSegmentRelation::ExactUniqueTopologyUnknown, 0));
    }
    anchors.sort_unstable();
    if !anchors
        .windows(2)
        .all(|pair| pair[0].0 < pair[1].0 && pair[0].1 < pair[1].1)
    {
        return Ok((ExactSegmentRelation::ExactUniqueTopologyUnknown, 0));
    }
    let mut crossings = 0usize;
    let mut usable_anchors = 0usize;
    for (old_anchor, new_anchor) in anchors {
        let old_inside = old_anchor >= old.key.start_ordinal && old_anchor < old.key.end_ordinal;
        let new_inside = new_anchor >= new.key.start_ordinal && new_anchor < new.key.end_ordinal;
        if old_inside || new_inside {
            if old_inside == new_inside {
                continue;
            }
            return Ok((ExactSegmentRelation::ExactUniqueTopologyUnknown, 0));
        }
        usable_anchors += 1;
        let old_side = if old_anchor < old.key.start_ordinal {
            -1i8
        } else {
            1
        };
        let new_side = if new_anchor < new.key.start_ordinal {
            -1i8
        } else {
            1
        };
        if old_side != new_side {
            crossings += 1;
        }
    }
    if usable_anchors == 0 {
        return Ok((ExactSegmentRelation::ExactUniqueTopologyUnknown, 0));
    }
    Ok(if crossings == 0 {
        (ExactSegmentRelation::ExactUniqueMonotone, 0)
    } else {
        (ExactSegmentRelation::ExactUniqueCrossing, crossings)
    })
}

#[cfg(test)]
fn segment_topology(
    old: &RecoverySegment,
    new: &RecoverySegment,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
) -> std::result::Result<(ExactSegmentRelation, usize), SegmentStopReason> {
    segment_topology_with_budget(
        old,
        new,
        old_occurrences,
        new_occurrences,
        exact_candidates,
        &mut SegmentDiagnosticBudget::default(),
    )
}

fn analyze_recovery_segments(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
    min_tokens: usize,
    _max_tokens: usize,
) -> std::result::Result<SegmentDiagnosticAnalysis, SegmentStopReason> {
    let mut budget = SegmentDiagnosticBudget::default();
    let old = collect_recovery_segments_with_budget(old_occurrences, min_tokens, &mut budget)?;
    let new = collect_recovery_segments_with_budget(new_occurrences, min_tokens, &mut budget)?;
    let old_consumed = snapshot_segment_consumed_ranges(old_occurrences, &old, &mut budget)?;
    let new_consumed = snapshot_segment_consumed_ranges(new_occurrences, &new, &mut budget)?;
    let segment_candidates = old
        .len()
        .checked_add(new.len())
        .ok_or(SegmentStopReason::CandidateCountLimit)?;

    let mut old_index = HashMap::<ExactHash, Vec<usize>>::new();
    let mut new_index = HashMap::<ExactHash, Vec<usize>>::new();
    old_index
        .try_reserve(old.len())
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    new_index
        .try_reserve(new.len())
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    for (index, segment) in old.iter().enumerate() {
        let posting = old_index.entry(segment.exact_hash).or_default();
        posting
            .try_reserve(1)
            .map_err(|_| SegmentStopReason::AllocationFailure)?;
        posting.push(index);
    }
    for (index, segment) in new.iter().enumerate() {
        let posting = new_index.entry(segment.exact_hash).or_default();
        posting
            .try_reserve(1)
            .map_err(|_| SegmentStopReason::AllocationFailure)?;
        posting.push(index);
    }

    let mut exact_pairs = Vec::new();
    exact_pairs
        .try_reserve(segment_candidates.min(SegmentDiagnosticBudget::HASH_PAIR_VISIT_LIMIT))
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    let mut old_cross_counts = Vec::new();
    let mut new_cross_counts = Vec::new();
    old_cross_counts
        .try_reserve_exact(old.len())
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    new_cross_counts
        .try_reserve_exact(new.len())
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    old_cross_counts.resize(old.len(), 0usize);
    new_cross_counts.resize(new.len(), 0usize);
    let mut segment_hash_matches = 0usize;
    let mut segment_token_verified_matches = 0usize;
    for (hash, old_posting) in &old_index {
        let Some(new_posting) = new_index.get(hash) else {
            continue;
        };
        let visits = old_posting
            .len()
            .checked_mul(new_posting.len())
            .ok_or(SegmentStopReason::HashPairVisitLimit)?;
        budget.charge_hash_pairs(visits)?;
        for old_index in old_posting {
            for new_index in new_posting {
                segment_hash_matches = segment_hash_matches
                    .checked_add(1)
                    .ok_or(SegmentStopReason::HashPairVisitLimit)?;
                let old_segment = old
                    .get(*old_index)
                    .ok_or(SegmentStopReason::CandidateCountLimit)?;
                let new_segment = new
                    .get(*new_index)
                    .ok_or(SegmentStopReason::CandidateCountLimit)?;
                budget.charge_token_elements(
                    old_segment
                        .token_count
                        .checked_add(new_segment.token_count)
                        .ok_or(SegmentStopReason::TokenVerificationLimit)?,
                )?;
                if !exact_segment_tokens_match(
                    old_segment,
                    new_segment,
                    old_occurrences,
                    new_occurrences,
                ) {
                    continue;
                }
                segment_token_verified_matches = segment_token_verified_matches
                    .checked_add(1)
                    .ok_or(SegmentStopReason::TokenVerificationLimit)?;
                exact_pairs
                    .try_reserve(1)
                    .map_err(|_| SegmentStopReason::AllocationFailure)?;
                exact_pairs.push((*old_index, *new_index));
                old_cross_counts[*old_index] += 1;
                new_cross_counts[*new_index] += 1;
            }
        }
    }
    exact_pairs.sort_unstable();
    let mut unique_pairs = Vec::new();
    unique_pairs
        .try_reserve(exact_pairs.len().min(segment_candidates))
        .map_err(|_| SegmentStopReason::AllocationFailure)?;
    let mut segment_unique_pairs = 0usize;
    let mut segment_duplicate_pairs = 0usize;
    let mut segment_monotone_pairs = 0usize;
    let mut segment_crossing_pairs = 0usize;
    for (old_index, new_index) in &exact_pairs {
        let old_segment = &old[*old_index];
        let new_segment = &new[*new_index];
        if !old_segment.role.is_alignment_compatible(new_segment.role) {
            continue;
        }
        let old_occurrence_count = new_cross_counts[*new_index];
        let new_occurrence_count = old_cross_counts[*old_index];
        if old_occurrence_count != 1 || new_occurrence_count != 1 {
            segment_duplicate_pairs += 1;
            continue;
        }
        segment_unique_pairs += 1;
        let topology = segment_topology_with_budget(
            old_segment,
            new_segment,
            old_occurrences,
            new_occurrences,
            exact_candidates,
            &mut budget,
        )?;
        match topology.0 {
            ExactSegmentRelation::ExactUniqueMonotone => segment_monotone_pairs += 1,
            ExactSegmentRelation::ExactUniqueCrossing => segment_crossing_pairs += 1,
            _ => {}
        }
        unique_pairs.push((*old_index, *new_index));
    }
    Ok(SegmentDiagnosticAnalysis {
        old,
        new,
        old_index,
        new_index,
        exact_pairs,
        old_cross_counts,
        new_cross_counts,
        old_consumed,
        new_consumed,
        unique_pairs,
        budget,
        segment_candidates,
        segment_hash_matches,
        segment_token_verified_matches,
        segment_unique_pairs,
        segment_duplicate_pairs,
        segment_monotone_pairs,
        segment_crossing_pairs,
    })
}

fn watched_side_occurrence_count(
    segment: &RecoverySegment,
    segments: &[RecoverySegment],
    postings: &HashMap<ExactHash, Vec<usize>>,
    occurrences: &[SentenceOccurrence],
    budget: &mut SegmentDiagnosticBudget,
) -> std::result::Result<usize, SegmentStopReason> {
    let Some(posting) = postings.get(&segment.exact_hash) else {
        return Ok(0);
    };
    budget.charge_hash_pairs(posting.len())?;
    let mut count = 0usize;
    for index in posting {
        let candidate = segments
            .get(*index)
            .ok_or(SegmentStopReason::CandidateCountLimit)?;
        budget.charge_token_elements(
            segment
                .token_count
                .checked_add(candidate.token_count)
                .ok_or(SegmentStopReason::TokenVerificationLimit)?,
        )?;
        if exact_segment_tokens_match(segment, candidate, occurrences, occurrences) {
            count += 1;
        }
    }
    Ok(count)
}

fn watched_segment_pair_evidence(
    analysis: &mut SegmentDiagnosticAnalysis,
    old_index: usize,
    new_index: usize,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
) -> std::result::Result<RecoveryWatchSegmentPairEvidence, SegmentStopReason> {
    analysis.budget.charge_output()?;
    let old = *analysis
        .old
        .get(old_index)
        .ok_or(SegmentStopReason::CandidateCountLimit)?;
    let new = *analysis
        .new
        .get(new_index)
        .ok_or(SegmentStopReason::CandidateCountLimit)?;
    let exact = analysis
        .exact_pairs
        .binary_search(&(old_index, new_index))
        .is_ok();
    let (old_occurrence_count, new_occurrence_count) = if exact {
        (
            *analysis
                .new_cross_counts
                .get(new_index)
                .ok_or(SegmentStopReason::CandidateCountLimit)?,
            *analysis
                .old_cross_counts
                .get(old_index)
                .ok_or(SegmentStopReason::CandidateCountLimit)?,
        )
    } else {
        (
            watched_side_occurrence_count(
                &old,
                &analysis.old,
                &analysis.old_index,
                old_occurrences,
                &mut analysis.budget,
            )?,
            watched_side_occurrence_count(
                &new,
                &analysis.new,
                &analysis.new_index,
                new_occurrences,
                &mut analysis.budget,
            )?,
        )
    };
    let role_compatible = old.role.is_alignment_compatible(new.role);
    let (relation, crossing_anchor_count) = if !exact || !role_compatible {
        (ExactSegmentRelation::NonExact, 0)
    } else if old_occurrence_count != 1 || new_occurrence_count != 1 {
        (ExactSegmentRelation::Duplicate, 0)
    } else {
        segment_topology_with_budget(
            &old,
            &new,
            old_occurrences,
            new_occurrences,
            exact_candidates,
            &mut analysis.budget,
        )?
    };
    Ok(RecoveryWatchSegmentPairEvidence {
        old_start_ordinal: old.key.start_ordinal,
        old_end_ordinal: old.key.end_ordinal,
        new_start_ordinal: new.key.start_ordinal,
        new_end_ordinal: new.key.end_ordinal,
        old_unit_count: old.unit_count,
        new_unit_count: new.unit_count,
        old_token_count: old.token_count,
        new_token_count: new.token_count,
        exact,
        old_occurrence_count,
        new_occurrence_count,
        role_compatible,
        overlaps_existing_recovery: false,
        crossing_anchor_count,
        relation,
    })
}

#[derive(Clone, Copy)]
struct GranularBoundary {
    kind: RecoveryWatchUnitKind,
    byte_start: usize,
    byte_end: usize,
}

struct GranularUnit {
    kind: RecoveryWatchUnitKind,
    byte_start: usize,
    byte_end: usize,
    text: String,
    tokens: Vec<SentenceEvidenceToken>,
    page: Option<u32>,
    role: Option<BlockRole>,
    recovery_location_available: bool,
}

#[derive(Default)]
struct GranularDiagnosticBudget {
    units: usize,
    token_bytes: usize,
    comparisons: usize,
    outputs: usize,
    auxiliary_items: usize,
    auxiliary_bytes: usize,
}

impl GranularDiagnosticBudget {
    const UNIT_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 16;
    const TOKEN_BYTE_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_BYTES / 16;
    const COMPARISON_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS * 16;
    const OUTPUT_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 16;
    const AUXILIARY_ITEM_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 4;
    const AUXILIARY_BYTE_LIMIT: usize = MAX_SENTENCE_RECOVERY_OUTPUT_BYTES / 8;

    fn charge_units(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
        self.units = self
            .units
            .checked_add(amount)
            .ok_or(RecoveryWatchGranularStopReason::UnitCountLimit)?;
        (self.units <= Self::UNIT_LIMIT)
            .then_some(())
            .ok_or(RecoveryWatchGranularStopReason::UnitCountLimit)
    }

    fn charge_token_bytes(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
        self.token_bytes = self
            .token_bytes
            .checked_add(amount)
            .ok_or(RecoveryWatchGranularStopReason::TokenByteLimit)?;
        (self.token_bytes <= Self::TOKEN_BYTE_LIMIT)
            .then_some(())
            .ok_or(RecoveryWatchGranularStopReason::TokenByteLimit)
    }

    fn charge_comparisons(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
        self.comparisons = self
            .comparisons
            .checked_add(amount)
            .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?;
        (self.comparisons <= Self::COMPARISON_LIMIT)
            .then_some(())
            .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)
    }

    fn charge_outputs(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
        self.outputs = self
            .outputs
            .checked_add(amount)
            .ok_or(RecoveryWatchGranularStopReason::OutputLimit)?;
        (self.outputs <= Self::OUTPUT_LIMIT)
            .then_some(())
            .ok_or(RecoveryWatchGranularStopReason::OutputLimit)
    }

    fn charge_auxiliary<T>(
        &mut self,
        amount: usize,
    ) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
        self.auxiliary_items = self
            .auxiliary_items
            .checked_add(amount)
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?;
        self.auxiliary_bytes = self
            .auxiliary_bytes
            .checked_add(
                amount
                    .checked_mul(std::mem::size_of::<T>())
                    .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?,
            )
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?;
        if self.auxiliary_items > Self::AUXILIARY_ITEM_LIMIT
            || self.auxiliary_bytes > Self::AUXILIARY_BYTE_LIMIT
        {
            return Err(RecoveryWatchGranularStopReason::AuxiliaryLimit);
        }
        Ok(())
    }
}

struct CollapsedGranularText {
    text: String,
    /// Original byte offset for every byte boundary in `text`.
    original_boundaries: Vec<usize>,
}

fn collapse_granular_whitespace(
    value: &str,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<CollapsedGranularText, RecoveryWatchGranularStopReason> {
    budget.charge_token_bytes(value.len())?;
    budget.charge_comparisons(value.len())?;
    budget.charge_auxiliary::<usize>(
        value
            .len()
            .checked_add(1)
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?,
    )?;
    let mut output = String::new();
    let mut original_boundaries = Vec::new();
    output
        .try_reserve(value.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    original_boundaries
        .try_reserve(
            value
                .len()
                .checked_add(1)
                .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?,
        )
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    let mut part_start = None;
    for (offset, character) in value
        .char_indices()
        .chain(std::iter::once((value.len(), ' ')))
    {
        if !character.is_whitespace() {
            part_start.get_or_insert(offset);
            continue;
        }
        let Some(start) = part_start.take() else {
            continue;
        };
        let part = value
            .get(start..offset)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        if !output.is_empty() {
            output.push(' ');
            original_boundaries.push(start);
        } else {
            original_boundaries.push(start);
        }
        output.push_str(part);
        original_boundaries.extend((1..=part.len()).map(|relative| start + relative));
    }
    if output.is_empty() {
        original_boundaries.push(value.len());
    }
    Ok(CollapsedGranularText {
        text: output,
        original_boundaries,
    })
}

fn watched_quote_range_for_granular(
    quote: &str,
    occurrence: &SentenceOccurrence,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<Option<Range<usize>>, RecoveryWatchGranularStopReason> {
    let needle = collapse_granular_whitespace(quote, budget)?;
    if needle.text.is_empty() {
        return Ok(None);
    }
    let haystack = collapse_granular_whitespace(&occurrence.key, budget)?;
    budget.charge_comparisons(haystack.text.len())?;
    let mut matches = haystack.text.match_indices(&needle.text);
    let Some((start, matched)) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Ok(None);
    }
    let end = start
        .checked_add(matched.len())
        .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?;
    let original_start = *haystack
        .original_boundaries
        .get(start)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let original_end = *haystack
        .original_boundaries
        .get(end)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    Ok((original_start < original_end).then_some(original_start..original_end))
}

fn push_trimmed_boundary(
    text: &str,
    range: Range<usize>,
    kind: RecoveryWatchUnitKind,
    output: &mut Vec<GranularBoundary>,
) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
    let raw = text
        .get(range.clone())
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let leading = raw.len().saturating_sub(raw.trim_start().len());
    let trailing_end = raw.trim_end().len();
    let start = range
        .start
        .checked_add(leading)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let end = range
        .start
        .checked_add(trailing_end)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    if start >= end {
        return Ok(());
    }
    if text
        .get(start..end)
        .is_none_or(|candidate| candidate.unicode_words().next().is_none())
    {
        return Ok(());
    }
    output
        .try_reserve(1)
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    output.push(GranularBoundary {
        kind,
        byte_start: start,
        byte_end: end,
    });
    Ok(())
}

fn is_leading_subordinate_clause(text: &str) -> bool {
    let first = text
        .split(|character: char| !character.is_alphabetic())
        .find(|word| !word.is_empty())
        .unwrap_or_default();
    [
        "after", "although", "as", "because", "before", "if", "once", "since", "unless", "until",
        "when", "whereas", "while",
    ]
    .iter()
    .any(|marker| first.eq_ignore_ascii_case(marker))
}

fn clause_boundaries(
    text: &str,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<Vec<GranularBoundary>, RecoveryWatchGranularStopReason> {
    budget.charge_auxiliary::<usize>(64)?;
    budget.charge_comparisons(text.len())?;
    let mut cuts = Vec::new();
    cuts.try_reserve(64)
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    cuts.push(0usize);

    if is_leading_subordinate_clause(text)
        && let Some(comma) = text.find(',')
        && text
            .get(comma + 1..)
            .is_some_and(|tail| tail.unicode_words().count() >= 2)
    {
        cuts.push(comma);
        cuts.push(comma + 1);
    }
    let contains_url = text.contains("://");
    for (offset, character) in text.char_indices() {
        if cuts.len() >= 64 {
            return Err(RecoveryWatchGranularStopReason::UnitCountLimit);
        }
        let width = character.len_utf8();
        match character {
            ';' => {
                cuts.push(offset);
                cuts.push(offset + width);
            }
            ':' => {
                budget.charge_comparisons(text.len())?;
                let left = text.get(..offset).unwrap_or_default();
                let right = text.get(offset + width..).unwrap_or_default();
                let numeric_colon = left.ends_with(|character: char| character.is_ascii_digit())
                    && right.starts_with(|character: char| character.is_ascii_digit());
                if !contains_url
                    && !numeric_colon
                    && left.unicode_words().count() >= 2
                    && right.unicode_words().count() >= 2
                {
                    cuts.push(offset);
                    cuts.push(offset + width);
                }
            }
            '\u{2014}' => {
                cuts.push(offset);
                cuts.push(offset + width);
            }
            _ => {}
        }
    }
    cuts.push(text.len());
    budget.charge_comparisons(
        cuts.len()
            .checked_mul(cuts.len())
            .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?,
    )?;
    cuts.sort_unstable();
    cuts.dedup();
    let mut boundaries = Vec::new();
    for pair in cuts.windows(2) {
        push_trimmed_boundary(
            text,
            pair[0]..pair[1],
            RecoveryWatchUnitKind::Clause,
            &mut boundaries,
        )?;
    }
    if boundaries.len() < 2 {
        boundaries.clear();
    }
    Ok(boundaries)
}

fn explicit_list_item_start(text: &str) -> Option<usize> {
    let trimmed_start = text.len().checked_sub(text.trim_start().len())?;
    let trimmed = text.get(trimmed_start..)?;
    for marker in ["- ", "* ", "\u{2022} ", "\u{25e6} "] {
        if trimmed.starts_with(marker) {
            return trimmed_start.checked_add(marker.len());
        }
    }
    let marker_end = trimmed
        .char_indices()
        .take(8)
        .find_map(|(index, character)| matches!(character, '.' | ')').then_some(index))?;
    let marker = trimmed.get(..marker_end)?;
    let valid = (!marker.is_empty() && marker.chars().all(|character| character.is_ascii_digit()))
        || marker.get(1..).is_some_and(|inner| {
            marker.starts_with('(')
                && !inner.is_empty()
                && inner
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        });
    let after = marker_end.checked_add(1)?;
    (valid && trimmed.get(after..)?.starts_with(char::is_whitespace)).then(|| {
        trimmed_start + after + trimmed[after..].len() - trimmed[after..].trim_start().len()
    })
}

fn short_enumeration_boundaries(
    text: &str,
    output: &mut Vec<GranularBoundary>,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<(), RecoveryWatchGranularStopReason> {
    let Some((delimiter, width)) = text
        .char_indices()
        .find(|(_, character)| matches!(character, ':' | '\u{2014}'))
        .map(|(offset, character)| (offset, character.len_utf8()))
    else {
        return Ok(());
    };
    let body_start = delimiter
        .checked_add(width)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let tail = text
        .get(body_start..)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let body_end = tail
        .find('\u{2014}')
        .or_else(|| tail.rfind('.'))
        .unwrap_or(tail.len());
    let body = tail
        .get(..body_end)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    budget.charge_auxiliary::<Range<usize>>(16)?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve(8)
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    let mut start = 0usize;
    for (offset, character) in body.char_indices() {
        if character == ',' {
            if ranges.len() == 8 {
                return Ok(());
            }
            ranges.push(start..offset);
            start = offset + 1;
        }
    }
    ranges.push(start..body.len());
    if !(2..=8).contains(&ranges.len()) {
        return Ok(());
    }
    let mut adjusted = Vec::new();
    adjusted
        .try_reserve_exact(ranges.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    for range in ranges {
        let item = body
            .get(range.clone())
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        let leading = item.len().saturating_sub(item.trim_start().len());
        let mut item_start = range.start + leading;
        let item_end = range.start + item.trim_end().len();
        let trimmed = body
            .get(item_start..item_end)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        for conjunction in ["and ", "or "] {
            if trimmed
                .get(..conjunction.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(conjunction))
            {
                item_start += conjunction.len();
                break;
            }
        }
        budget.charge_comparisons(range.len())?;
        let words = body
            .get(item_start..item_end)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?
            .unicode_words()
            .count();
        if !(1..=3).contains(&words) {
            return Ok(());
        }
        adjusted.push(item_start..item_end);
    }
    for range in adjusted {
        push_trimmed_boundary(
            text,
            body_start + range.start..body_start + range.end,
            RecoveryWatchUnitKind::ListItem,
            output,
        )?;
    }
    Ok(())
}

fn granular_boundaries(
    occurrence: &SentenceOccurrence,
    quote_range: Range<usize>,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<Vec<GranularBoundary>, RecoveryWatchGranularStopReason> {
    let text = occurrence
        .key
        .get(quote_range.clone())
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let mut boundaries = clause_boundaries(text, budget)?;
    if occurrence.kind == RecoveryUnitKind::Line
        && let Some(start) = explicit_list_item_start(text)
    {
        push_trimmed_boundary(
            text,
            start..text.len(),
            RecoveryWatchUnitKind::ListItem,
            &mut boundaries,
        )?;
    }
    short_enumeration_boundaries(text, &mut boundaries, budget)?;
    budget.charge_comparisons(
        boundaries
            .len()
            .checked_mul(boundaries.len())
            .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?,
    )?;
    boundaries.sort_unstable_by_key(|boundary| {
        (
            boundary.byte_start,
            boundary.byte_end,
            match boundary.kind {
                RecoveryWatchUnitKind::Clause => 0,
                RecoveryWatchUnitKind::ListItem => 1,
                _ => 2,
            },
        )
    });
    boundaries.dedup_by_key(|boundary| (boundary.kind, boundary.byte_start, boundary.byte_end));
    for boundary in &mut boundaries {
        boundary.byte_start = boundary
            .byte_start
            .checked_add(quote_range.start)
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?;
        boundary.byte_end = boundary
            .byte_end
            .checked_add(quote_range.start)
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?;
    }
    Ok(boundaries)
}

fn build_granular_units(
    occurrence: &SentenceOccurrence,
    quote_range: Range<usize>,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<Vec<GranularUnit>, RecoveryWatchGranularStopReason> {
    if occurrence.location.is_none() || occurrence.tokens.iter().any(|token| !token.is_scalar()) {
        return Ok(Vec::new());
    }
    let boundaries = granular_boundaries(occurrence, quote_range, budget)?;
    budget.charge_units(boundaries.len())?;
    budget.charge_outputs(boundaries.len())?;
    budget.charge_auxiliary::<usize>(
        occurrence
            .tokens
            .len()
            .checked_add(1)
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?,
    )?;
    let scalar_to_token = scalar_to_token_boundaries(&occurrence.tokens)
        .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
    let mut units = Vec::new();
    units
        .try_reserve_exact(boundaries.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    for boundary in boundaries {
        let text = occurrence
            .key
            .get(boundary.byte_start..boundary.byte_end)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        let scalar_start = occurrence
            .key
            .get(..boundary.byte_start)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?
            .chars()
            .count();
        let scalar_end = scalar_start
            .checked_add(text.chars().count())
            .ok_or(RecoveryWatchGranularStopReason::TokenByteLimit)?;
        let token_start = *scalar_to_token
            .get(scalar_start)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        let token_end = *scalar_to_token
            .get(scalar_end)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        let tokens = occurrence
            .tokens
            .get(token_start..token_end)
            .ok_or(RecoveryWatchGranularStopReason::AllocationFailure)?;
        budget.charge_token_bytes(
            text.len()
                .checked_add(
                    tokens
                        .len()
                        .checked_mul(std::mem::size_of::<SentenceEvidenceToken>())
                        .ok_or(RecoveryWatchGranularStopReason::TokenByteLimit)?,
                )
                .ok_or(RecoveryWatchGranularStopReason::TokenByteLimit)?,
        )?;
        let mut owned_text = String::new();
        owned_text
            .try_reserve_exact(text.len())
            .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
        owned_text.push_str(text);
        let mut owned_tokens = Vec::new();
        owned_tokens
            .try_reserve_exact(tokens.len())
            .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
        owned_tokens.extend_from_slice(tokens);
        units.push(GranularUnit {
            kind: boundary.kind,
            byte_start: boundary.byte_start,
            byte_end: boundary.byte_end,
            text: owned_text,
            tokens: owned_tokens,
            page: occurrence.page,
            role: occurrence.role,
            recovery_location_available: occurrence.location.is_some(),
        });
    }
    Ok(units)
}

fn granular_similarity(
    old: &GranularUnit,
    new: &GranularUnit,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<u16, RecoveryWatchGranularStopReason> {
    if old.kind != new.kind
        || !matches!((old.role, new.role), (Some(old), Some(new)) if old.is_alignment_compatible(new))
    {
        return Ok(0);
    }
    let shorter = old.tokens.len().min(new.tokens.len());
    if shorter == 0 {
        return Ok(0);
    }
    let mut prefix = 0usize;
    while prefix < shorter {
        budget.charge_comparisons(1)?;
        if old.tokens[prefix] != new.tokens[prefix] {
            break;
        }
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < shorter - prefix {
        budget.charge_comparisons(1)?;
        if old.tokens[old.tokens.len() - suffix - 1] != new.tokens[new.tokens.len() - suffix - 1] {
            break;
        }
        suffix += 1;
    }
    let edge = basis_points(
        prefix
            .checked_add(suffix)
            .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?,
        shorter,
    )
    .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?;
    if edge < MIN_WORD_SCORE_EDGE_EVIDENCE {
        return Ok(edge);
    }
    let old_word_count = old.text.unicode_words().count();
    let new_word_count = new.text.unicode_words().count();
    let mut shared = 0usize;
    for (old_index, old_word) in old.text.unicode_words().enumerate() {
        let mut prior_old = 0usize;
        for candidate in old.text.unicode_words().take(old_index) {
            budget.charge_comparisons(1)?;
            prior_old += usize::from(candidate == old_word);
        }
        let mut matching_new = 0usize;
        for candidate in new.text.unicode_words() {
            budget.charge_comparisons(1)?;
            matching_new += usize::from(candidate == old_word);
        }
        shared += usize::from(prior_old < matching_new);
    }
    let total = old_word_count
        .checked_add(new_word_count)
        .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?;
    let word = if total == 0 {
        0
    } else {
        basis_points(
            shared
                .checked_mul(2)
                .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?,
            total,
        )
        .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?
    };
    Ok(edge.max(word))
}

fn granular_relation(scores: impl IntoIterator<Item = u16>) -> RecoveryWatchGranularRelation {
    let mut relation = RecoveryWatchGranularRelation {
        available: true,
        ..RecoveryWatchGranularRelation::default()
    };
    for (partner, score) in scores.into_iter().enumerate() {
        if score > relation.best_score {
            relation.second_score = relation.best_score;
            relation.best_score = score;
            relation.partner_index = Some(partner);
            relation.tied_for_best = false;
        } else if score == relation.best_score {
            relation.second_score = score;
            relation.tied_for_best = true;
        } else {
            relation.second_score = relation.second_score.max(score);
        }
    }
    if relation.best_score == 0 {
        relation.available = false;
        relation.partner_index = None;
        relation.tied_for_best = false;
    }
    relation
}

fn granular_pair_evidence(
    old_quote: &str,
    new_quote: &str,
    old_occurrence: &SentenceOccurrence,
    new_occurrence: &SentenceOccurrence,
    budget: &mut GranularDiagnosticBudget,
) -> std::result::Result<Option<RecoveryWatchGranularPairEvidence>, RecoveryWatchGranularStopReason>
{
    let (Some(old_quote_range), Some(new_quote_range)) = (
        watched_quote_range_for_granular(old_quote, old_occurrence, budget)?,
        watched_quote_range_for_granular(new_quote, new_occurrence, budget)?,
    ) else {
        return Ok(None);
    };
    let old = build_granular_units(old_occurrence, old_quote_range, budget)?;
    let new = build_granular_units(new_occurrence, new_quote_range, budget)?;
    if old.is_empty() || new.is_empty() {
        return Ok(None);
    }
    let pair_count = old
        .len()
        .checked_mul(new.len())
        .ok_or(RecoveryWatchGranularStopReason::ComparisonLimit)?;
    budget.charge_comparisons(pair_count)?;
    budget.charge_auxiliary::<u16>(pair_count)?;
    let mut scores = Vec::new();
    scores
        .try_reserve_exact(pair_count)
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    for old_unit in &old {
        for new_unit in &new {
            scores.push(granular_similarity(old_unit, new_unit, budget)?);
        }
    }
    let mut old_relations = Vec::new();
    let mut new_relations = Vec::new();
    budget.charge_auxiliary::<RecoveryWatchGranularRelation>(
        old.len()
            .checked_add(new.len())
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?,
    )?;
    old_relations
        .try_reserve_exact(old.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    new_relations
        .try_reserve_exact(new.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    for row in scores.chunks_exact(new.len()) {
        old_relations.push(granular_relation(row.iter().copied()));
    }
    for new_index in 0..new.len() {
        new_relations.push(granular_relation(
            (0..old.len()).map(|old_index| scores[old_index * new.len() + new_index]),
        ));
    }
    for (old_index, relation) in old_relations.iter_mut().enumerate() {
        let Some(new_index) = relation.partner_index else {
            continue;
        };
        relation.exact = old[old_index].tokens == new[new_index].tokens;
        relation.reciprocal = !relation.tied_for_best
            && new_relations.get(new_index).is_some_and(|new_relation| {
                !new_relation.tied_for_best && new_relation.partner_index == Some(old_index)
            });
    }
    for (new_index, relation) in new_relations.iter_mut().enumerate() {
        let Some(old_index) = relation.partner_index else {
            continue;
        };
        relation.exact = new[new_index].tokens == old[old_index].tokens;
        relation.reciprocal = !relation.tied_for_best
            && old_relations.get(old_index).is_some_and(|old_relation| {
                !old_relation.tied_for_best && old_relation.partner_index == Some(new_index)
            });
    }
    let mut old_units = Vec::new();
    let mut new_units = Vec::new();
    budget.charge_auxiliary::<RecoveryWatchGranularUnitEvidence>(
        old.len()
            .checked_add(new.len())
            .ok_or(RecoveryWatchGranularStopReason::AuxiliaryLimit)?,
    )?;
    old_units
        .try_reserve_exact(old.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    new_units
        .try_reserve_exact(new.len())
        .map_err(|_| RecoveryWatchGranularStopReason::AllocationFailure)?;
    for (unit, relation) in old.into_iter().zip(old_relations) {
        old_units.push(RecoveryWatchGranularUnitEvidence {
            kind: unit.kind,
            byte_start: unit.byte_start,
            byte_end: unit.byte_end,
            token_count: unit.tokens.len(),
            page: unit.page,
            role: unit.role,
            recovery_location_available: unit.recovery_location_available,
            relation,
        });
    }
    for (unit, relation) in new.into_iter().zip(new_relations) {
        new_units.push(RecoveryWatchGranularUnitEvidence {
            kind: unit.kind,
            byte_start: unit.byte_start,
            byte_end: unit.byte_end,
            token_count: unit.tokens.len(),
            page: unit.page,
            role: unit.role,
            recovery_location_available: unit.recovery_location_available,
            relation,
        });
    }
    Ok(Some(RecoveryWatchGranularPairEvidence {
        old_units,
        new_units,
    }))
}

impl RecoveryWatchState {
    fn new(
        queries: &[RecoveryWatchQuery<'_>],
        old_occurrences: &[SentenceOccurrence],
        new_occurrences: &[SentenceOccurrence],
        exact_candidates: &[ExactMatchCandidate],
        context: RecoveryWatchBuildContext<'_>,
    ) -> Option<Self> {
        if queries.is_empty() {
            return None;
        }
        let processed = queries.len().min(MAX_RECOVERY_WATCH_QUERIES);
        let (segment_analysis, segment_stop_reason) = match analyze_recovery_segments(
            old_occurrences,
            new_occurrences,
            exact_candidates,
            context.min_tokens,
            context.max_tokens,
        ) {
            Ok(analysis) => (analysis, None),
            Err(reason) => (SegmentDiagnosticAnalysis::default(), Some(reason)),
        };
        let mut state = Self {
            complete: queries.len() <= MAX_RECOVERY_WATCH_QUERIES,
            candidate_generation_complete: false,
            near_relation_complete: false,
            near_relation_stop_reason: None,
            segment_analysis,
            segment_stop_reason,
            segment_overlap_vetoes: 0,
            granular_complete: true,
            granular_old_units: 0,
            granular_new_units: 0,
            granular_pair_comparisons: 0,
            granular_stop_reason: None,
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
            old_pair_partners: HashMap::new(),
            new_pair_partners: HashMap::new(),
            scan_work: 0,
            scan_limit: context.max_tokens.checked_mul(16)?,
            retained_one_sided_occurrences: 0,
        };
        let mut granular_budget = GranularDiagnosticBudget::default();
        state.records.try_reserve_exact(processed).ok()?;
        state.pair_by_occurrences.try_reserve(processed).ok()?;
        state.old_pair_partners.try_reserve(processed).ok()?;
        state.new_pair_partners.try_reserve(processed).ok()?;
        for query in queries.iter().take(processed) {
            let paired = query.old_quote.is_some() && query.new_quote.is_some();
            let valid = query.old_quote.is_some() || query.new_quote.is_some();
            if !valid {
                state.complete = false;
            }
            let old = match query.old_quote {
                Some(quote) if paired => state.locate(
                    quote,
                    old_occurrences,
                    context.old_evidence,
                    context.old_fully_contained,
                    context.min_tokens,
                ),
                Some(quote) => state.locate_one_sided(
                    quote,
                    old_occurrences,
                    context.old_evidence,
                    context.old_fully_contained,
                    context.min_tokens,
                ),
                None if valid => Some(RecoveryWatchLookup::not_queried()),
                None => Some(RecoveryWatchLookup::unavailable()),
            };
            let new = match query.new_quote {
                Some(quote) if paired => state.locate(
                    quote,
                    new_occurrences,
                    context.new_evidence,
                    context.new_fully_contained,
                    context.min_tokens,
                ),
                Some(quote) => state.locate_one_sided(
                    quote,
                    new_occurrences,
                    context.new_evidence,
                    context.new_fully_contained,
                    context.min_tokens,
                ),
                None if valid => Some(RecoveryWatchLookup::not_queried()),
                None => Some(RecoveryWatchLookup::unavailable()),
            };
            let old = old.unwrap_or_else(|| {
                state.complete = false;
                RecoveryWatchLookup::unavailable()
            });
            let new = new.unwrap_or_else(|| {
                state.complete = false;
                RecoveryWatchLookup::unavailable()
            });
            let pair = match (paired, old.span_index, new.span_index) {
                (true, Some(old_span), Some(new_span)) => Some(RecoveryWatchPairEvidence {
                    same_span: old_span == new_span,
                    exact_shared_units: 0,
                    exact_shared_units_available: false,
                    near_candidate_examined: false,
                    near_score: None,
                    near_scope: None,
                    old_relation: RecoveryWatchRelation::default(),
                    new_relation: RecoveryWatchRelation::default(),
                    reciprocal: false,
                }),
                _ => None,
            };
            let old_occurrence = old.occurrence_index;
            let new_occurrence = new.occurrence_index;
            let mut old_segment = old.segment_key.and_then(|key| {
                state
                    .segment_analysis
                    .old
                    .iter()
                    .position(|segment| segment.key == key)
            });
            let mut new_segment = new.segment_key.and_then(|key| {
                state
                    .segment_analysis
                    .new
                    .iter()
                    .position(|segment| segment.key == key)
            });
            let segment_pair = match (paired, old_segment, new_segment, state.segment_stop_reason) {
                (true, Some(old_index), Some(new_index), None) => {
                    match watched_segment_pair_evidence(
                        &mut state.segment_analysis,
                        old_index,
                        new_index,
                        old_occurrences,
                        new_occurrences,
                        exact_candidates,
                    ) {
                        Ok(evidence) => Some(evidence),
                        Err(reason) => {
                            state.segment_analysis = SegmentDiagnosticAnalysis::default();
                            state.segment_stop_reason = Some(reason);
                            state.segment_overlap_vetoes = 0;
                            for record in &mut state.records {
                                record.output.segment_pair = None;
                                record.old_segment = None;
                                record.new_segment = None;
                            }
                            old_segment = None;
                            new_segment = None;
                            None
                        }
                    }
                }
                _ => None,
            };
            let granular_pair = match (
                query.old_quote,
                query.new_quote,
                old_occurrence,
                new_occurrence,
                state.granular_stop_reason,
            ) {
                (Some(old_quote), Some(new_quote), Some(old_index), Some(new_index), None) => {
                    match granular_pair_evidence(
                        old_quote,
                        new_quote,
                        old_occurrences.get(old_index)?,
                        new_occurrences.get(new_index)?,
                        &mut granular_budget,
                    ) {
                        Ok(evidence) => evidence,
                        Err(reason) => {
                            state.granular_complete = false;
                            state.granular_stop_reason = Some(reason);
                            state.granular_old_units = 0;
                            state.granular_new_units = 0;
                            state.granular_pair_comparisons = 0;
                            for record in &mut state.records {
                                record.output.granular_pair = None;
                            }
                            None
                        }
                    }
                }
                _ => None,
            };
            if state.granular_stop_reason.is_none()
                && let Some(evidence) = granular_pair.as_ref()
            {
                state.granular_old_units = state
                    .granular_old_units
                    .checked_add(evidence.old_units.len())?;
                state.granular_new_units = state
                    .granular_new_units
                    .checked_add(evidence.new_units.len())?;
                state.granular_pair_comparisons = granular_budget.comparisons;
            }
            let index = state.records.len();
            let mut id = String::new();
            id.try_reserve_exact(query.id.len()).ok()?;
            id.push_str(query.id);
            state.records.push(RecoveryWatchStateRecord {
                output: RecoveryWatchRecord {
                    id,
                    old: old.evidence,
                    new: new.evidence,
                    pair,
                    segment_pair,
                    granular_pair,
                },
                old_occurrence,
                new_occurrence,
                old_segment,
                new_segment,
            });
            if paired && let (Some(old), Some(new)) = (old_occurrence, new_occurrence) {
                let indices = state.pair_by_occurrences.entry((old, new)).or_default();
                indices.try_reserve(1).ok()?;
                indices.push(index);
                push_unique_watch_partner(&mut state.old_pair_partners, old, new)?;
                push_unique_watch_partner(&mut state.new_pair_partners, new, old)?;
            }
        }
        Some(state)
    }

    fn locate_one_sided(
        &mut self,
        quote: &str,
        occurrences: &[SentenceOccurrence],
        evidence: Option<&RunRecoveryEvidence<'_>>,
        fully_contained: Option<&[bool]>,
        min_tokens: usize,
    ) -> Option<RecoveryWatchLookup> {
        let needle = collapse_watch_whitespace(quote)?;
        if needle.is_empty() {
            return Some(RecoveryWatchLookup::unfound());
        }
        let available = MAX_RECOVERY_WATCH_RETAINED_OCCURRENCES
            .checked_sub(self.retained_one_sided_occurrences)?;
        let mut outputs = Vec::new();
        let mut occurrence_count = 0usize;
        for occurrence in occurrences {
            let haystack = collapse_watch_whitespace(&occurrence.key)?;
            self.scan_work = self.scan_work.checked_add(haystack.chars().count())?;
            if self.scan_work > self.scan_limit {
                return None;
            }
            let matches = haystack.match_indices(&needle).count();
            if matches == 0 {
                continue;
            }
            if occurrence.tokens.iter().any(|token| !token.is_scalar()) {
                return Some(RecoveryWatchLookup::unavailable());
            }
            occurrence_count = occurrence_count.checked_add(matches)?;
            let retained = available.saturating_sub(outputs.len()).min(matches);
            if retained == 0 {
                continue;
            }
            let descriptor = occurrence
                .run_descriptor_index
                .and_then(|index| evidence?.descriptors.get(index));
            let contained = occurrence
                .run_descriptor_index
                .and_then(|index| fully_contained?.get(index))
                .copied()
                .unwrap_or(false);
            let output = recovery_watch_occurrence(occurrence, descriptor, contained, min_tokens);
            outputs.try_reserve_exact(retained).ok()?;
            outputs.extend(std::iter::repeat_n(output, retained));
        }
        if self.collect_one_sided_segment_occurrences(
            &needle,
            occurrences,
            evidence,
            fully_contained,
            min_tokens,
            available,
            &mut occurrence_count,
            &mut outputs,
        )? {
            return Some(RecoveryWatchLookup::unavailable());
        }
        if occurrence_count == 0 {
            return Some(RecoveryWatchLookup::unfound());
        }
        let complete = occurrence_count == outputs.len();
        self.retained_one_sided_occurrences = self
            .retained_one_sided_occurrences
            .checked_add(outputs.len())?;
        if !complete {
            self.complete = false;
        }
        Some(RecoveryWatchLookup {
            evidence: RecoveryWatchOccurrenceEvidence::Occurrences(RecoveryWatchOccurrences {
                occurrence_count,
                complete,
                occurrences: outputs,
            }),
            occurrence_index: None,
            span_index: None,
            segment_key: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_one_sided_segment_occurrences(
        &mut self,
        needle: &str,
        occurrences: &[SentenceOccurrence],
        evidence: Option<&RunRecoveryEvidence<'_>>,
        fully_contained: Option<&[bool]>,
        min_tokens: usize,
        available: usize,
        occurrence_count: &mut usize,
        outputs: &mut Vec<RecoveryWatchOccurrence>,
    ) -> Option<bool> {
        const MAX_SEGMENT_UNITS: usize = 8;

        if occurrences.len() > SegmentDiagnosticBudget::AUXILIARY_ITEM_LIMIT {
            return None;
        }
        let mut by_position = HashMap::new();
        by_position.try_reserve(occurrences.len()).ok()?;
        for (index, occurrence) in occurrences.iter().enumerate() {
            if let Some(position) = occurrence.trusted_position
                && by_position.insert(position, index).is_some()
            {
                return None;
            }
        }
        for first in occurrences {
            let (Some(position), Some(span_index), Some(role)) =
                (first.trusted_position, first.span_index, first.role)
            else {
                continue;
            };
            let mut joined = collapse_watch_whitespace(&first.key)?;
            let first_end = joined.len();
            let mut second_start = None;
            let mut token_count = first.tokens.len();
            let mut descriptor_index = first.run_descriptor_index;
            let mut page = first.page;
            let mut all_scalar = first.tokens.iter().all(|token| token.is_scalar());
            let mut all_locations_available = first.location.is_some();
            for offset in 1..MAX_SEGMENT_UNITS {
                let ordinal = position.ordinal.checked_add(offset)?;
                let next_index = match by_position.get(&TrustedStreamPosition {
                    stream_index: position.stream_index,
                    ordinal,
                }) {
                    Some(index) => *index,
                    None => break,
                };
                let next = occurrences.get(next_index)?;
                if next.span_index != Some(span_index) || next.role != Some(role) {
                    break;
                }
                let previous_end = joined.len();
                let next_key = collapse_watch_whitespace(&next.key)?;
                joined
                    .try_reserve(1usize.checked_add(next_key.len())?)
                    .ok()?;
                let latest_start = joined.len().checked_add(1)?;
                second_start.get_or_insert(latest_start);
                joined.push(' ');
                joined.push_str(&next_key);
                token_count = token_count.checked_add(next.tokens.len())?;
                all_scalar &= next.tokens.iter().all(|token| token.is_scalar());
                all_locations_available &= next.location.is_some();
                if descriptor_index != next.run_descriptor_index {
                    descriptor_index = None;
                }
                if page != next.page {
                    page = None;
                }
                self.scan_work = self.scan_work.checked_add(joined.chars().count())?;
                if self.scan_work > self.scan_limit {
                    return None;
                }
                for (byte_start, _) in joined.match_indices(needle) {
                    let byte_end = byte_start.checked_add(needle.len())?;
                    if byte_start >= first_end
                        || byte_end <= second_start?
                        || byte_end <= previous_end
                    {
                        continue;
                    }
                    if !all_scalar {
                        return Some(true);
                    }
                    *occurrence_count = occurrence_count.checked_add(1)?;
                    if outputs.len() >= available {
                        continue;
                    }
                    let descriptor =
                        descriptor_index.and_then(|index| evidence?.descriptors.get(index));
                    let contained = descriptor_index
                        .and_then(|index| fully_contained?.get(index))
                        .copied()
                        .unwrap_or(false);
                    outputs.try_reserve_exact(1).ok()?;
                    outputs.push(RecoveryWatchOccurrence {
                        span_index: Some(span_index),
                        trusted_run_descriptor_index: descriptor_index,
                        ordinal: Some(position.ordinal),
                        end_ordinal: Some(ordinal.checked_add(1)?),
                        unit_count: Some(offset.checked_add(1)?),
                        token_count: Some(token_count),
                        recovery_location_available: all_locations_available
                            && token_count >= min_tokens,
                        fully_contained: contained,
                        page: descriptor.map(|descriptor| descriptor.page.0).or(page),
                        bbox: descriptor.map(|descriptor| descriptor.bbox),
                        role: Some(role),
                        kind: RecoveryWatchUnitKind::Segment,
                    });
                }
            }
        }
        Some(false)
    }

    fn locate(
        &mut self,
        quote: &str,
        occurrences: &[SentenceOccurrence],
        evidence: Option<&RunRecoveryEvidence<'_>>,
        fully_contained: Option<&[bool]>,
        min_tokens: usize,
    ) -> Option<RecoveryWatchLookup> {
        let needle = collapse_watch_whitespace(quote)?;
        if needle.is_empty() {
            return Some(RecoveryWatchLookup {
                evidence: RecoveryWatchOccurrenceEvidence::Unfound,
                occurrence_index: None,
                span_index: None,
                segment_key: None,
            });
        }
        let mut found = None;
        let mut ambiguous = false;
        for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
            let haystack = collapse_watch_whitespace(&occurrence.key)?;
            self.scan_work = self.scan_work.checked_add(haystack.chars().count())?;
            if self.scan_work > self.scan_limit {
                return None;
            }
            let mut matches = haystack.match_indices(&needle);
            if matches.next().is_none() {
                continue;
            }
            if matches.next().is_some() || found.replace(occurrence_index).is_some() {
                ambiguous = true;
            }
        }
        if ambiguous {
            return Some(RecoveryWatchLookup {
                evidence: RecoveryWatchOccurrenceEvidence::Ambiguous,
                occurrence_index: None,
                span_index: None,
                segment_key: None,
            });
        }
        let Some(occurrence_index) = found else {
            return self.locate_segment(
                &needle,
                occurrences,
                evidence,
                fully_contained,
                min_tokens,
            );
        };
        let segment =
            self.locate_segment(&needle, occurrences, evidence, fully_contained, min_tokens)?;
        if matches!(
            segment.evidence,
            RecoveryWatchOccurrenceEvidence::Found(_)
                | RecoveryWatchOccurrenceEvidence::Ambiguous
                | RecoveryWatchOccurrenceEvidence::Unavailable
        ) {
            return Some(RecoveryWatchLookup {
                evidence: RecoveryWatchOccurrenceEvidence::Ambiguous,
                occurrence_index: None,
                span_index: None,
                segment_key: None,
            });
        }
        let occurrence = occurrences.get(occurrence_index)?;
        if occurrence.tokens.iter().any(|token| !token.is_scalar()) {
            return Some(RecoveryWatchLookup {
                evidence: RecoveryWatchOccurrenceEvidence::Unavailable,
                occurrence_index: None,
                span_index: None,
                segment_key: None,
            });
        }
        let descriptor = occurrence
            .run_descriptor_index
            .and_then(|index| evidence?.descriptors.get(index));
        let contained = occurrence
            .run_descriptor_index
            .and_then(|index| fully_contained?.get(index))
            .copied()
            .unwrap_or(false);
        let output = recovery_watch_occurrence(occurrence, descriptor, contained, min_tokens);
        Some(RecoveryWatchLookup {
            evidence: RecoveryWatchOccurrenceEvidence::Found(output),
            occurrence_index: Some(occurrence_index),
            span_index: occurrence.span_index,
            segment_key: None,
        })
    }

    fn locate_segment(
        &mut self,
        needle: &str,
        occurrences: &[SentenceOccurrence],
        evidence: Option<&RunRecoveryEvidence<'_>>,
        fully_contained: Option<&[bool]>,
        min_tokens: usize,
    ) -> Option<RecoveryWatchLookup> {
        const MAX_SEGMENT_UNITS: usize = 8;

        if occurrences.len() > SegmentDiagnosticBudget::AUXILIARY_ITEM_LIMIT {
            return None;
        }
        let mut by_position = HashMap::new();
        by_position.try_reserve(occurrences.len()).ok()?;
        for (index, occurrence) in occurrences.iter().enumerate() {
            if let Some(position) = occurrence.trusted_position
                && by_position.insert(position, index).is_some()
            {
                return None;
            }
        }

        let mut hits = HashSet::new();
        hits.try_reserve(2).ok()?;
        let mut found = None;
        for first in occurrences {
            let (Some(position), Some(span_index), Some(role)) =
                (first.trusted_position, first.span_index, first.role)
            else {
                continue;
            };
            let mut joined = collapse_watch_whitespace(&first.key)?;
            let first_end = joined.len();
            let mut second_start = None;
            let mut token_count = first.tokens.len();
            let mut descriptor_index = first.run_descriptor_index;
            let mut page = first.page;
            let mut all_scalar = first.tokens.iter().all(|token| token.is_scalar());
            let mut all_locations_available = first.location.is_some();
            for offset in 1..MAX_SEGMENT_UNITS {
                let ordinal = position.ordinal.checked_add(offset)?;
                let Some(next_index) = by_position
                    .get(&TrustedStreamPosition {
                        stream_index: position.stream_index,
                        ordinal,
                    })
                    .copied()
                else {
                    break;
                };
                let next = occurrences.get(next_index)?;
                if next.span_index != Some(span_index) || next.role != Some(role) {
                    break;
                }
                let next_key = collapse_watch_whitespace(&next.key)?;
                joined
                    .try_reserve(1usize.checked_add(next_key.len())?)
                    .ok()?;
                let latest_start = joined.len().checked_add(1)?;
                second_start.get_or_insert(latest_start);
                joined.push(' ');
                joined.push_str(&next_key);
                token_count = token_count.checked_add(next.tokens.len())?;
                all_scalar &= next.tokens.iter().all(|token| token.is_scalar());
                all_locations_available &= next.location.is_some();
                if descriptor_index != next.run_descriptor_index {
                    descriptor_index = None;
                }
                if page != next.page {
                    page = None;
                }
                self.scan_work = self.scan_work.checked_add(joined.chars().count())?;
                if self.scan_work > self.scan_limit {
                    return None;
                }
                let mut search_from = 0usize;
                while let Some(relative_start) = joined.get(search_from..)?.find(needle) {
                    let byte_start = search_from.checked_add(relative_start)?;
                    let byte_end = byte_start.checked_add(needle.len())?;
                    if byte_start < first_end && byte_end > second_start? {
                        let hit = RecoveryWatchSegmentHit {
                            stream_index: position.stream_index,
                            start_ordinal: position.ordinal,
                            end_ordinal: ordinal.checked_add(1)?,
                            unit_count: offset.checked_add(1)?,
                            token_count,
                            byte_start,
                            byte_end,
                        };
                        let dedupe = RecoveryWatchSegmentDedupeKey {
                            stream_index: hit.stream_index,
                            start_ordinal: hit.start_ordinal,
                            byte_start: hit.byte_start,
                            byte_end: hit.byte_end,
                        };
                        if hits.insert(dedupe)
                            && found
                                .replace((
                                    span_index,
                                    role,
                                    position,
                                    hit,
                                    byte_start == 0 && byte_end == joined.len(),
                                    descriptor_index,
                                    page,
                                    all_scalar,
                                    all_locations_available,
                                ))
                                .is_some()
                        {
                            return Some(RecoveryWatchLookup {
                                evidence: RecoveryWatchOccurrenceEvidence::Ambiguous,
                                occurrence_index: None,
                                span_index: None,
                                segment_key: None,
                            });
                        }
                    }
                    let next = joined.get(byte_start..)?.chars().next()?.len_utf8();
                    search_from = byte_start.checked_add(next)?;
                }
            }
        }

        let Some((
            span_index,
            role,
            position,
            hit,
            full_candidate,
            descriptor_index,
            page,
            all_scalar,
            all_locations_available,
        )) = found
        else {
            return Some(RecoveryWatchLookup {
                evidence: RecoveryWatchOccurrenceEvidence::Unfound,
                occurrence_index: None,
                span_index: None,
                segment_key: None,
            });
        };
        if !all_scalar {
            return Some(RecoveryWatchLookup {
                evidence: RecoveryWatchOccurrenceEvidence::Unavailable,
                occurrence_index: None,
                span_index: None,
                segment_key: None,
            });
        }
        let descriptor = descriptor_index.and_then(|index| evidence?.descriptors.get(index));
        let contained = descriptor_index
            .and_then(|index| fully_contained?.get(index))
            .copied()
            .unwrap_or(false);
        Some(RecoveryWatchLookup {
            evidence: RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                span_index: Some(span_index),
                trusted_run_descriptor_index: descriptor_index,
                ordinal: Some(position.ordinal),
                end_ordinal: Some(hit.end_ordinal),
                unit_count: Some(hit.unit_count),
                token_count: Some(hit.token_count),
                recovery_location_available: all_locations_available
                    && hit.token_count >= min_tokens,
                fully_contained: contained,
                page: descriptor.map(|descriptor| descriptor.page.0).or(page),
                bbox: descriptor.map(|descriptor| descriptor.bbox),
                role: Some(role),
                kind: RecoveryWatchUnitKind::Segment,
            }),
            occurrence_index: None,
            span_index: Some(span_index),
            segment_key: full_candidate.then_some(RecoverySegmentKey {
                stream_index: hit.stream_index,
                start_ordinal: hit.start_ordinal,
                end_ordinal: hit.end_ordinal,
            }),
        })
    }

    fn record_exact_candidates(
        &mut self,
        candidates: &[ExactMatchCandidate],
        old_occurrences: &[SentenceOccurrence],
        new_occurrences: &[SentenceOccurrence],
    ) -> Option<()> {
        for record in &mut self.records {
            let (Some(old), Some(new), Some(pair)) = (
                record.old_occurrence,
                record.new_occurrence,
                record.output.pair.as_mut(),
            ) else {
                continue;
            };
            let old_descriptor = old_occurrences.get(old)?.run_descriptor_index;
            let new_descriptor = new_occurrences.get(new)?.run_descriptor_index;
            if old_descriptor.is_none() || new_descriptor.is_none() {
                continue;
            }
            let exact_shared_units = candidates.iter().try_fold(0usize, |count, candidate| {
                let candidate_old = old_occurrences
                    .get(candidate.old_occurrence_index)?
                    .run_descriptor_index;
                let candidate_new = new_occurrences
                    .get(candidate.new_occurrence_index)?
                    .run_descriptor_index;
                if old_descriptor.is_some()
                    && old_descriptor == candidate_old
                    && new_descriptor.is_some()
                    && new_descriptor == candidate_new
                {
                    count.checked_add(1)
                } else {
                    Some(count)
                }
            })?;
            pair.exact_shared_units = exact_shared_units;
            pair.exact_shared_units_available = true;
        }
        Some(())
    }

    fn record_near(
        &mut self,
        old_occurrence: usize,
        new_occurrence: usize,
        score: u16,
        scope: RecoveryWatchNearScope,
    ) {
        let Some(record_indices) = self
            .pair_by_occurrences
            .get(&(old_occurrence, new_occurrence))
        else {
            return;
        };
        for record_index in record_indices {
            let Some(pair) = self
                .records
                .get_mut(*record_index)
                .and_then(|record| record.output.pair.as_mut())
            else {
                self.complete = false;
                return;
            };
            pair.near_candidate_examined = true;
            pair.near_score = Some(score);
            pair.near_scope = Some(scope);
        }
    }

    fn partners(&self, side: OccurrenceSide, occurrence: usize) -> &[usize] {
        match side {
            OccurrenceSide::Old => self.old_pair_partners.get(&occurrence),
            OccurrenceSide::New => self.new_pair_partners.get(&occurrence),
        }
        .map_or(&[], Vec::as_slice)
    }

    fn near_was_examined(&self, old_occurrence: usize, new_occurrence: usize) -> bool {
        self.pair_by_occurrences
            .get(&(old_occurrence, new_occurrence))
            .into_iter()
            .flatten()
            .any(|index| {
                self.records
                    .get(*index)
                    .and_then(|record| record.output.pair.as_ref())
                    .is_some_and(|pair| pair.near_candidate_examined)
            })
    }

    fn record_relations(
        &mut self,
        old_candidates: &[RecoveryCandidate],
        new_candidates: &[RecoveryCandidate],
        relations: &ModifiedSentenceRelations,
    ) -> Option<()> {
        let mut old_by_occurrence = HashMap::new();
        let mut new_by_occurrence = HashMap::new();
        old_by_occurrence.try_reserve(old_candidates.len()).ok()?;
        new_by_occurrence.try_reserve(new_candidates.len()).ok()?;
        for (index, candidate) in old_candidates.iter().enumerate() {
            old_by_occurrence.insert(candidate.occurrence_index, index);
        }
        for (index, candidate) in new_candidates.iter().enumerate() {
            new_by_occurrence.insert(candidate.occurrence_index, index);
        }
        for record in &mut self.records {
            let (Some(old_occurrence), Some(new_occurrence), Some(pair)) = (
                record.old_occurrence,
                record.new_occurrence,
                record.output.pair.as_mut(),
            ) else {
                continue;
            };
            let old_index = old_by_occurrence.get(&old_occurrence).copied();
            let new_index = new_by_occurrence.get(&new_occurrence).copied();
            if let Some(old_index) = old_index {
                let old = *relations.old.get(old_index)?;
                pair.old_relation = RecoveryWatchRelation {
                    available: true,
                    best_score: old.best_score,
                    second_score: old.second_score,
                    watched_partner_is_best: new_index
                        .is_some_and(|new_index| old.best_partner == Some(new_index)),
                };
            }
            if let Some(new_index) = new_index {
                let new = *relations.new.get(new_index)?;
                pair.new_relation = RecoveryWatchRelation {
                    available: true,
                    best_score: new.best_score,
                    second_score: new.second_score,
                    watched_partner_is_best: old_index
                        .is_some_and(|old_index| new.best_partner == Some(old_index)),
                };
            }
            if old_index.is_some() || new_index.is_some() {
                pair.reciprocal = match (old_index, new_index) {
                    (Some(old_index), Some(new_index)) => {
                        let old = *relations.old.get(old_index)?;
                        let new = *relations.new.get(new_index)?;
                        old.unique_partner() == Some(new_index)
                            && new.unique_partner() == Some(old_index)
                    }
                    _ => false,
                };
            }
        }
        Some(())
    }

    fn record_near_search_state(
        &mut self,
        candidate_generation_complete: bool,
        near_relation_complete: bool,
        stop_reason: Option<NearRelationStopReason>,
    ) {
        self.candidate_generation_complete = candidate_generation_complete;
        self.near_relation_complete = near_relation_complete;
        self.near_relation_stop_reason = stop_reason;
    }

    fn finish(mut self, plan: Option<&SentenceRecoveryPlan>) -> RecoveryWatchDiagnostics {
        if self.segment_stop_reason.is_none()
            && let Err(reason) = self.record_segment_overlaps(plan)
        {
            self.segment_analysis = SegmentDiagnosticAnalysis::default();
            self.segment_overlap_vetoes = 0;
            self.segment_stop_reason = Some(reason);
            for record in &mut self.records {
                record.output.segment_pair = None;
                record.old_segment = None;
                record.new_segment = None;
            }
        }
        RecoveryWatchDiagnostics {
            complete: self.complete
                && self.candidate_generation_complete
                && self.near_relation_complete
                && self.near_relation_stop_reason.is_none()
                && self.segment_stop_reason.is_none(),
            candidate_generation_complete: self.candidate_generation_complete,
            near_relation_complete: self.near_relation_complete,
            near_relation_stop_reason: self.near_relation_stop_reason,
            segment_candidates: self.segment_analysis.segment_candidates,
            segment_hash_matches: self.segment_analysis.segment_hash_matches,
            segment_token_verified_matches: self.segment_analysis.segment_token_verified_matches,
            segment_unique_pairs: self.segment_analysis.segment_unique_pairs,
            segment_duplicate_pairs: self.segment_analysis.segment_duplicate_pairs,
            segment_monotone_pairs: self.segment_analysis.segment_monotone_pairs,
            segment_crossing_pairs: self.segment_analysis.segment_crossing_pairs,
            segment_overlap_vetoes: self.segment_overlap_vetoes,
            segment_stop_reason: self.segment_stop_reason,
            granular_complete: self.granular_complete,
            granular_old_units: self.granular_old_units,
            granular_new_units: self.granular_new_units,
            granular_pair_comparisons: self.granular_pair_comparisons,
            granular_stop_reason: self.granular_stop_reason,
            records: self
                .records
                .into_iter()
                .map(|record| record.output)
                .collect(),
        }
    }

    fn record_segment_overlaps(
        &mut self,
        plan: Option<&SentenceRecoveryPlan>,
    ) -> std::result::Result<(), SegmentStopReason> {
        let Some(plan) = plan else { return Ok(()) };
        for (old_index, new_index) in &self.segment_analysis.unique_pairs {
            let (Some(old), Some(new)) = (
                self.segment_analysis.old.get(*old_index),
                self.segment_analysis.new.get(*new_index),
            ) else {
                continue;
            };
            let overlaps = segment_consumed_ranges_overlap(
                old,
                &self.segment_analysis.old_consumed,
                &plan.deletion_consumed,
                &mut self.segment_analysis.budget,
            )? || segment_consumed_ranges_overlap(
                new,
                &self.segment_analysis.new_consumed,
                &plan.insertion_consumed,
                &mut self.segment_analysis.budget,
            )?;
            if !overlaps {
                continue;
            }
            self.segment_overlap_vetoes += 1;
        }
        for record in &mut self.records {
            let (Some(old), Some(new)) = (record.old_segment, record.new_segment) else {
                continue;
            };
            let old_overlap = if let Some(segment) = self.segment_analysis.old.get(old) {
                segment_consumed_ranges_overlap(
                    segment,
                    &self.segment_analysis.old_consumed,
                    &plan.deletion_consumed,
                    &mut self.segment_analysis.budget,
                )?
            } else {
                false
            };
            let new_overlap = if let Some(segment) = self.segment_analysis.new.get(new) {
                segment_consumed_ranges_overlap(
                    segment,
                    &self.segment_analysis.new_consumed,
                    &plan.insertion_consumed,
                    &mut self.segment_analysis.budget,
                )?
            } else {
                false
            };
            if let Some(evidence) = record.output.segment_pair.as_mut() {
                evidence.overlaps_existing_recovery = old_overlap || new_overlap;
            }
        }
        Ok(())
    }
}

fn segment_consumed_ranges_overlap(
    segment: &RecoverySegment,
    snapshots: &HashMap<usize, Vec<LocalSentenceRange>>,
    committed: &[LocalSentenceRange],
    budget: &mut SegmentDiagnosticBudget,
) -> std::result::Result<bool, SegmentStopReason> {
    for index in &segment.occurrence_indices[..segment.unit_count] {
        let Some(candidates) = snapshots.get(index) else {
            continue;
        };
        for candidate in candidates {
            for committed in committed {
                budget.charge_overlap_comparison()?;
                if candidate.block == committed.block
                    && candidate.canonical.start < committed.canonical.end
                    && committed.canonical.start < candidate.canonical.end
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn collapse_watch_whitespace(value: &str) -> Option<String> {
    let mut output = String::new();
    output.try_reserve(value.len()).ok()?;
    for part in value.split_whitespace() {
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(part);
    }
    Some(output)
}

impl CandidateNearRelation {
    fn record_eligible(&mut self, partner: usize, score: u16) {
        if score > self.best_score {
            self.second_score = self.best_score;
            self.best_score = score;
            self.best_partner = Some(partner);
        } else {
            self.second_score = self.second_score.max(score);
        }
    }

    fn record_disqualifying(&mut self, score: u16) {
        if score > self.best_score {
            self.second_score = self.best_score;
            self.best_score = score;
            self.best_partner = None;
        } else {
            self.second_score = self.second_score.max(score);
        }
    }

    fn vetoed(self) -> bool {
        self.best_score >= MIN_NEAR_SCORE
    }

    fn unique_partner(self) -> Option<usize> {
        let margin = self.best_score.checked_sub(self.second_score)?;
        (self.best_score >= MIN_NEAR_SCORE && margin >= MIN_NEAR_SCORE_MARGIN)
            .then_some(self.best_partner?)
    }

    fn veto_without_partner(&mut self) {
        self.best_score = self.best_score.max(MIN_NEAR_SCORE);
        self.best_partner = None;
    }
}

struct ModifiedSentenceRelations {
    old: Vec<CandidateNearRelation>,
    new: Vec<CandidateNearRelation>,
    complete: bool,
    edge_gate_shadow: Option<SentenceEdgeGateShadow>,
    edge_signature_shadow: Option<SentenceEdgeSignatureShadow>,
}

#[derive(Clone, Copy)]
struct SentenceEdgeSignatureShadow {
    metrics: SentenceEdgeSignatureShadowMetrics,
    direct_metrics: SentenceEdgeSignatureDirectShadowMetrics,
    posting_limit: usize,
    query_limit: usize,
    distinct_key_limit: usize,
    estimated_byte_limit: usize,
    query_count_limit: usize,
    candidate_union_limit: usize,
    active: bool,
    mode: SentenceEdgeSignatureFilterMode,
    retained_fingerprint: SentenceEdgeRetainedFingerprint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SentenceEdgeRetainedFingerprint {
    count: usize,
    set_xor: [u8; 32],
    set_sum: [u8; 32],
    order: [u8; 32],
    legacy_sentence_edge_pairs_examined: usize,
    legacy_sentence_edge_pairs_attempted: usize,
    legacy_sentence_edge_pairs_retained: usize,
    legacy_sentence_edge_pairs_rejected: usize,
}

impl SentenceEdgeRetainedFingerprint {
    fn record(
        &mut self,
        scope: NearSearchScope,
        query_side: OccurrenceSide,
        old_occurrence: usize,
        new_occurrence: usize,
    ) -> Option<()> {
        let scope = match scope {
            NearSearchScope::PairedInterval => 1usize,
            NearSearchScope::PairedCrossIntervalVeto => 2,
            NearSearchScope::SameOrAmbiguousSpan => 3,
            NearSearchScope::CrossSpan => 4,
        };
        let direction = match query_side {
            OccurrenceSide::Old => 1usize,
            OccurrenceSide::New => 2,
        };
        let mut hasher = Sha256::new();
        for value in [scope, direction, old_occurrence, new_occurrence] {
            hasher.update(value.to_be_bytes());
        }
        let item: [u8; 32] = hasher.finalize().into();
        self.count = self.count.checked_add(1)?;
        for (accumulator, byte) in self.set_xor.iter_mut().zip(item) {
            *accumulator ^= byte;
        }
        add_digest_modulo_256(&mut self.set_sum, item);
        let mut order = Sha256::new();
        order.update(self.order);
        order.update(item);
        self.order = order.finalize().into();
        Some(())
    }

    #[cfg(test)]
    fn same_set(self, other: Self) -> bool {
        self.count == other.count && self.set_xor == other.set_xor && self.set_sum == other.set_sum
    }

    #[cfg(test)]
    fn same_order(self, other: Self) -> bool {
        self.count == other.count && self.order == other.order
    }

    fn record_reference_filter_progress(&mut self, budget: &RecoveryBudget) -> Option<()> {
        self.legacy_sentence_edge_pairs_examined = budget
            .sentence_edge_filter_pairs_retained
            .checked_add(budget.sentence_edge_filter_pairs_rejected)?;
        self.legacy_sentence_edge_pairs_attempted = budget.sentence_edge_filter_pairs_attempted;
        self.legacy_sentence_edge_pairs_retained = budget.sentence_edge_filter_pairs_retained;
        self.legacy_sentence_edge_pairs_rejected = budget.sentence_edge_filter_pairs_rejected;
        Some(())
    }

    #[cfg(test)]
    fn record_reference_attempt(&mut self) -> Option<()> {
        self.legacy_sentence_edge_pairs_attempted =
            self.legacy_sentence_edge_pairs_attempted.checked_add(1)?;
        Some(())
    }
}

fn add_digest_modulo_256(accumulator: &mut [u8; 32], value: [u8; 32]) {
    let mut carry = 0u16;
    for index in (0..accumulator.len()).rev() {
        let sum = u16::from(accumulator[index]) + u16::from(value[index]) + carry;
        accumulator[index] = sum as u8;
        carry = sum >> 8;
    }
}

impl SentenceEdgeSignatureShadow {
    fn new(
        token_limit: usize,
        metrics: SentenceEdgeSignatureShadowMetrics,
        mode: SentenceEdgeSignatureFilterMode,
    ) -> Option<Self> {
        let work_limit = token_limit.checked_mul(4)?;
        Self::new_with_work_limit(token_limit, work_limit, metrics, mode)
    }

    fn new_with_work_limit(
        token_limit: usize,
        work_limit: usize,
        metrics: SentenceEdgeSignatureShadowMetrics,
        mode: SentenceEdgeSignatureFilterMode,
    ) -> Option<Self> {
        Some(Self {
            metrics,
            direct_metrics: SentenceEdgeSignatureDirectShadowMetrics::default(),
            posting_limit: work_limit,
            query_limit: work_limit,
            distinct_key_limit: work_limit,
            // The diagnostic index shares recovery's existing hard allocation
            // ceiling instead of introducing an unbounded auxiliary budget.
            estimated_byte_limit: MAX_SENTENCE_RECOVERY_OUTPUT_BYTES,
            query_count_limit: token_limit,
            candidate_union_limit: work_limit,
            active: true,
            mode,
            retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
        })
    }

    fn stop(&mut self, reason: SentenceEdgeSignatureShadowStopReason) {
        self.active = false;
        self.metrics.complete = false;
        self.metrics.stop_reason.get_or_insert(reason);
    }

    fn stop_direct(&mut self, reason: SentenceEdgeSignatureDirectShadowStopReason) {
        self.active = false;
        self.direct_metrics.complete = false;
        self.direct_metrics.stop_reason.get_or_insert(reason);
    }
}

fn signature_stop_reason(
    error: SentenceEdgeSignatureIndexError,
) -> SentenceEdgeSignatureShadowStopReason {
    match error {
        SentenceEdgeSignatureIndexError::PostingLimit { .. } => {
            SentenceEdgeSignatureShadowStopReason::IndexPostingLimit
        }
        SentenceEdgeSignatureIndexError::QueryVisitLimit { .. } => {
            SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit
        }
        SentenceEdgeSignatureIndexError::AllocationFailure { .. } => {
            SentenceEdgeSignatureShadowStopReason::AllocationFailure
        }
        SentenceEdgeSignatureIndexError::CounterOverflow { .. } => {
            SentenceEdgeSignatureShadowStopReason::CounterOverflow
        }
        SentenceEdgeSignatureIndexError::InvalidScope { .. } => {
            SentenceEdgeSignatureShadowStopReason::DiagnosticFailure
        }
    }
}

fn record_signature_index_error(
    shadow: &mut SentenceEdgeSignatureShadow,
    error: SentenceEdgeSignatureIndexError,
) {
    let (examined, attempted) = error.work();
    let totals = shadow
        .metrics
        .index_posting_items_examined
        .checked_add(examined)
        .zip(
            shadow
                .metrics
                .index_posting_items_attempted
                .checked_add(attempted),
        );
    if let Some((examined, attempted)) = totals {
        shadow.metrics.index_posting_items_examined = examined;
        shadow.metrics.index_posting_items_attempted = attempted;
        shadow.stop(signature_stop_reason(error));
    } else {
        shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
    }
}

fn record_signature_query_error(
    shadow: &mut SentenceEdgeSignatureShadow,
    error: SentenceEdgeSignatureIndexError,
) {
    let (examined, attempted) = error.work();
    let totals = shadow
        .metrics
        .query_posting_visits_examined
        .checked_add(examined)
        .zip(
            shadow
                .metrics
                .query_posting_visits_attempted
                .checked_add(attempted),
        );
    if let Some((examined, attempted)) = totals {
        shadow.metrics.query_posting_visits_examined = examined;
        shadow.metrics.query_posting_visits_attempted = attempted;
        shadow.stop(signature_stop_reason(error));
    } else {
        shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
    }
}

fn record_direct_signature_build_error(
    shadow: &mut SentenceEdgeSignatureShadow,
    error: SentenceEdgeSignatureIndexBuildError,
) {
    let previous_distinct = shadow.direct_metrics.signature_index_distinct_keys_examined;
    let previous_bytes = shadow
        .direct_metrics
        .signature_index_estimated_logical_bytes_examined;
    let progress = error.progress();
    let distinct_work = match error {
        SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
            examined,
            attempted,
            ..
        } => Some((examined, attempted)),
        _ => None,
    };
    let byte_work = match error {
        SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined,
            attempted,
            ..
        }
        | SentenceEdgeSignatureIndexBuildError::AllocationFailure {
            examined,
            attempted,
            ..
        } => Some((examined, attempted)),
        _ => None,
    };
    let posting_work = match error {
        SentenceEdgeSignatureIndexBuildError::PostingLimit {
            examined,
            attempted,
        }
        | SentenceEdgeSignatureIndexBuildError::Index(
            SentenceEdgeSignatureIndexError::PostingLimit {
                examined,
                attempted,
            }
            | SentenceEdgeSignatureIndexError::AllocationFailure {
                examined,
                attempted,
            },
        ) => Some((examined, attempted)),
        _ => None,
    };
    let reason = match error {
        SentenceEdgeSignatureIndexBuildError::PostingLimit { .. } => {
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexPostingLimit
        }
        SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit { .. } => {
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexDistinctKeyLimit
        }
        SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit { .. } => {
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexEstimatedByteLimit
        }
        SentenceEdgeSignatureIndexBuildError::AllocationFailure { .. } => {
            SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure
        }
        SentenceEdgeSignatureIndexBuildError::Index(error) => match error {
            SentenceEdgeSignatureIndexError::AllocationFailure { .. } => {
                SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure
            }
            SentenceEdgeSignatureIndexError::CounterOverflow { .. } => {
                SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow
            }
            SentenceEdgeSignatureIndexError::InvalidScope { .. } => {
                SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure
            }
            SentenceEdgeSignatureIndexError::PostingLimit { .. } => {
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexPostingLimit
            }
            SentenceEdgeSignatureIndexError::QueryVisitLimit { .. } => {
                SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryPostingVisitLimit
            }
        },
    };
    let totals = progress
        .map_or(Some(()), |progress| {
            add_direct_signature_index_metrics(&mut shadow.direct_metrics, progress)
        })
        .and_then(|()| {
            posting_work.map_or(Some(()), |(examined, attempted)| {
                shadow.direct_metrics.signature_index_items_examined = shadow
                    .direct_metrics
                    .signature_index_items_examined
                    .checked_add(examined)?;
                shadow.direct_metrics.signature_index_items_attempted = shadow
                    .direct_metrics
                    .signature_index_items_attempted
                    .checked_add(attempted)?;
                Some(())
            })
        });
    let resource_totals = totals.and_then(|()| {
        if let Some((examined, attempted)) = distinct_work {
            shadow.direct_metrics.signature_index_distinct_keys_examined =
                previous_distinct.checked_add(examined)?;
            shadow
                .direct_metrics
                .signature_index_distinct_keys_attempted =
                previous_distinct.checked_add(attempted)?;
        }
        if let Some((examined, attempted)) = byte_work {
            shadow
                .direct_metrics
                .signature_index_estimated_logical_bytes_examined =
                previous_bytes.checked_add(examined)?;
            shadow
                .direct_metrics
                .signature_index_estimated_logical_bytes_attempted =
                previous_bytes.checked_add(attempted)?;
        }
        Some(())
    });
    if resource_totals.is_some() {
        shadow.stop_direct(reason);
    } else {
        shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
    }
}

fn add_direct_signature_index_metrics(
    target: &mut SentenceEdgeSignatureDirectShadowMetrics,
    source: SentenceEdgeSignatureIndexMetrics,
) -> Option<()> {
    macro_rules! add {
        ($field:ident) => {
            target.$field = target.$field.checked_add(source.$field)?;
        };
        ($target:ident, $source:ident) => {
            target.$target = target.$target.checked_add(source.$source)?;
        };
    }
    add!(signature_index_items_examined, posting_items);
    add!(signature_index_items_attempted, posting_items);
    add!(signature_index_own_distinct_keys, own_distinct_keys);
    add!(signature_index_all_distinct_keys, all_distinct_keys);
    let distinct_keys = source
        .own_distinct_keys
        .checked_add(source.all_distinct_keys)?;
    target.signature_index_distinct_keys_examined = target
        .signature_index_distinct_keys_examined
        .checked_add(distinct_keys)?;
    target.signature_index_distinct_keys_attempted = target
        .signature_index_distinct_keys_attempted
        .checked_add(distinct_keys)?;
    add!(signature_index_own_key_capacity, own_key_capacity);
    add!(signature_index_all_key_capacity, all_key_capacity);
    add!(signature_index_own_posting_items, own_posting_items);
    add!(signature_index_all_posting_items, all_posting_items);
    add!(
        signature_index_posting_capacity_items,
        posting_capacity_items
    );
    add!(
        signature_index_estimated_logical_bytes,
        estimated_logical_bytes
    );
    target.signature_index_estimated_logical_bytes_attempted = target
        .signature_index_estimated_logical_bytes_attempted
        .checked_add(source.estimated_logical_bytes)?;
    target.signature_index_estimated_logical_bytes_examined = target
        .signature_index_estimated_logical_bytes_examined
        .checked_add(source.estimated_logical_bytes)?;
    add!(signature_index_depth_1_posting_items, depth_1_posting_items);
    add!(
        signature_index_depth_2_to_3_posting_items,
        depth_2_to_3_posting_items
    );
    add!(
        signature_index_depth_4_plus_posting_items,
        depth_4_plus_posting_items
    );
    target.signature_index_largest_posting = target
        .signature_index_largest_posting
        .max(source.largest_posting);
    Some(())
}

fn build_signature_index(
    shadow: Option<&mut SentenceEdgeSignatureShadow>,
    occurrences: &[SentenceOccurrence],
    scope: CandidatePostingIndexScope<'_>,
) -> Option<SentenceEdgeSignatureIndex> {
    let shadow = shadow.filter(|shadow| shadow.active)?;
    if shadow.mode == SentenceEdgeSignatureFilterMode::ReferenceObserve {
        return None;
    }
    if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
        let remaining_limits = (|| {
            Some(SentenceEdgeSignatureIndexBuildLimits {
                posting_items: shadow
                    .posting_limit
                    .checked_sub(shadow.direct_metrics.signature_index_items_attempted)?,
                distinct_keys: shadow.distinct_key_limit.checked_sub(
                    shadow
                        .direct_metrics
                        .signature_index_own_distinct_keys
                        .checked_add(shadow.direct_metrics.signature_index_all_distinct_keys)?,
                )?,
                estimated_logical_bytes: shadow.estimated_byte_limit.checked_sub(
                    shadow
                        .direct_metrics
                        .signature_index_estimated_logical_bytes,
                )?,
            })
        })();
        let Some(limits) = remaining_limits else {
            shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            return None;
        };
        return match SentenceEdgeSignatureIndex::new_with_limits(occurrences, scope, limits) {
            Ok(index) => {
                if add_direct_signature_index_metrics(&mut shadow.direct_metrics, index.metrics())
                    .is_some()
                {
                    Some(index)
                } else {
                    shadow
                        .stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
                    None
                }
            }
            Err(error) => {
                record_direct_signature_build_error(shadow, error);
                None
            }
        };
    }
    let remaining = shadow
        .posting_limit
        .checked_sub(shadow.metrics.index_posting_items_attempted)?;
    match SentenceEdgeSignatureIndex::new_bounded(occurrences, scope, remaining) {
        Ok(index) => {
            let actual = index.metrics().posting_items;
            let attempted = shadow
                .metrics
                .index_posting_items_attempted
                .checked_add(actual);
            let examined = shadow
                .metrics
                .index_posting_items_examined
                .checked_add(actual);
            match (attempted, examined) {
                (Some(attempted), Some(examined)) => {
                    shadow.metrics.index_posting_items_attempted = attempted;
                    shadow.metrics.index_posting_items_examined = examined;
                    Some(index)
                }
                _ => {
                    shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
                    None
                }
            }
        }
        Err(error) => {
            record_signature_index_error(shadow, error);
            None
        }
    }
}

fn sentence_signature_depth(token_count: usize) -> Option<usize> {
    let required = token_count.checked_mul(3)?.checked_add(9)? / 10;
    required.checked_add(1).map(|value| value / 2)
}

fn is_cross_orientation_only(
    query: &SentenceOccurrence,
    candidate: &SentenceOccurrence,
) -> Option<bool> {
    let depth = sentence_signature_depth(query.tokens.len())?
        .min(sentence_signature_depth(candidate.tokens.len())?);
    if depth == 0 || query.tokens.len() < depth || candidate.tokens.len() < depth {
        return Some(false);
    }
    let query_prefix = &query.tokens[..depth];
    let query_suffix = &query.tokens[query.tokens.len() - depth..];
    let candidate_prefix = &candidate.tokens[..depth];
    let candidate_suffix = &candidate.tokens[candidate.tokens.len() - depth..];
    let same = query_prefix == candidate_prefix || query_suffix == candidate_suffix;
    let cross = query_prefix == candidate_suffix || query_suffix == candidate_prefix;
    Some(cross && !same)
}

fn record_direct_signature_query_metrics(
    shadow: &mut SentenceEdgeSignatureShadow,
    query: SentenceEdgeSignatureQueryMetrics,
    candidate_union: usize,
    candidates: &[usize],
) -> Option<()> {
    if query.candidate_union < candidate_union || candidates.len() != candidate_union {
        shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
        return None;
    }
    let metrics = &mut shadow.direct_metrics;
    metrics.signature_query_visits_examined = metrics
        .signature_query_visits_examined
        .checked_add(query.posting_visits)?;
    metrics.signature_query_visits_attempted = metrics
        .signature_query_visits_attempted
        .checked_add(query.posting_visits)?;
    let attempted_union = metrics
        .signature_candidate_union_attempted
        .checked_add(candidate_union)?;
    metrics.signature_candidate_union_attempted = attempted_union;
    if attempted_union > shadow.candidate_union_limit {
        shadow
            .stop_direct(SentenceEdgeSignatureDirectShadowStopReason::SignatureCandidateUnionLimit);
        return None;
    }
    metrics.direct_candidates = metrics.direct_candidates.checked_add(candidate_union)?;
    let (queries, unions) = match query.depth_band {
        Some(SentenceEdgeSignatureDepthBand::One) => (
            &mut metrics.signature_depth_1_queries,
            &mut metrics.signature_depth_1_candidate_union,
        ),
        Some(SentenceEdgeSignatureDepthBand::TwoToThree) => (
            &mut metrics.signature_depth_2_to_3_queries,
            &mut metrics.signature_depth_2_to_3_candidate_union,
        ),
        Some(SentenceEdgeSignatureDepthBand::FourOrMore) => (
            &mut metrics.signature_depth_4_plus_queries,
            &mut metrics.signature_depth_4_plus_candidate_union,
        ),
        None => return None,
    };
    *queries = queries.checked_add(1)?;
    *unions = unions.checked_add(candidate_union)?;
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn apply_signature_filter(
    shadow: Option<&mut SentenceEdgeSignatureShadow>,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    index: Option<&SentenceEdgeSignatureIndex>,
    query: &SentenceOccurrence,
    candidates: &[SentenceOccurrence],
    plausible: &mut Vec<usize>,
    bucket: CandidatePostingBucket,
    additional_bucket: Option<CandidatePostingBucket>,
    scope: NearSearchScope,
    mut include: impl FnMut(usize) -> bool,
) -> Option<()> {
    plausible.retain(|index| include(*index));
    if query.kind != RecoveryUnitKind::Sentence {
        return Some(());
    }
    let Some(shadow) = shadow else {
        return Some(());
    };
    if shadow.mode == SentenceEdgeSignatureFilterMode::ReferenceObserve {
        return Some(());
    }
    if !shadow.active {
        return (shadow.mode != SentenceEdgeSignatureFilterMode::Direct).then_some(());
    };
    let Some(index) = index else {
        if shadow.mode == SentenceEdgeSignatureFilterMode::Direct
            && shadow.direct_metrics.stop_reason.is_none()
        {
            shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
        } else {
            shadow.stop(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure);
        }
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
        return (shadow.mode != SentenceEdgeSignatureFilterMode::Direct).then_some(());
    };
    let query_attempted = if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
        shadow.direct_metrics.signature_query_visits_attempted
    } else {
        shadow.metrics.query_posting_visits_attempted
    };
    let Some(remaining) = shadow.query_limit.checked_sub(query_attempted) else {
        shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
        return (shadow.mode != SentenceEdgeSignatureFilterMode::Direct).then_some(());
    };
    let mut signature = Vec::new();
    if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
        let Some(attempted) = shadow
            .direct_metrics
            .signature_queries_attempted
            .checked_add(1)
        else {
            shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
            return None;
        };
        shadow.direct_metrics.signature_queries_attempted = attempted;
        if attempted > shadow.query_count_limit {
            shadow
                .stop_direct(SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryCountLimit);
            checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
            return None;
        }
        shadow.direct_metrics.signature_queries =
            shadow.direct_metrics.signature_queries.checked_add(1)?;
    }
    let query_metrics = match index.collect_plausible_occurrences_bounded(
        &mut signature,
        query,
        bucket,
        additional_bucket,
        remaining,
    ) {
        Ok(metrics) => metrics,
        Err(error) => {
            if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
                let (examined, attempted) = error.work();
                let totals = shadow
                    .direct_metrics
                    .signature_query_visits_examined
                    .checked_add(examined)
                    .zip(
                        shadow
                            .direct_metrics
                            .signature_query_visits_attempted
                            .checked_add(attempted),
                    );
                if let Some((examined, attempted)) = totals {
                    shadow.direct_metrics.signature_query_visits_examined = examined;
                    shadow.direct_metrics.signature_query_visits_attempted = attempted;
                    shadow.stop_direct(match error {
                        SentenceEdgeSignatureIndexError::QueryVisitLimit { .. } => {
                            SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryPostingVisitLimit
                        }
                        SentenceEdgeSignatureIndexError::AllocationFailure { .. } => {
                            SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure
                        }
                        _ => SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow,
                    });
                } else {
                    shadow
                        .stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
                }
            } else {
                record_signature_query_error(shadow, error);
            }
            checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
            return (shadow.mode != SentenceEdgeSignatureFilterMode::Direct).then_some(());
        }
    };
    let Some(visits) = shadow
        .metrics
        .query_posting_visits_attempted
        .checked_add(query_metrics.posting_visits)
    else {
        shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
        return (shadow.mode != SentenceEdgeSignatureFilterMode::Direct).then_some(());
    };
    if shadow.mode != SentenceEdgeSignatureFilterMode::Direct {
        shadow.metrics.query_posting_visits_attempted = visits;
        shadow.metrics.query_posting_visits_examined = visits;
    }
    signature.retain(|index| include(*index));
    signature.sort_unstable();
    signature.dedup();
    let direct = shadow.mode == SentenceEdgeSignatureFilterMode::Direct;
    let outside = if direct {
        0
    } else {
        signature
            .iter()
            .filter(|index| plausible.binary_search(index).is_err())
            .count()
    };
    let signature_count = if direct {
        signature.len()
    } else {
        plausible
            .iter()
            .filter(|index| signature.binary_search(index).is_ok())
            .count()
    };
    if direct
        && record_direct_signature_query_metrics(shadow, query_metrics, signature_count, &signature)
            .is_none()
    {
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
        return None;
    }
    let considered = if direct {
        signature.len()
    } else {
        plausible.len()
    };
    let observation = (|| {
        shadow.metrics.pairs_considered =
            shadow.metrics.pairs_considered.checked_add(considered)?;
        shadow.metrics.signature_candidates = shadow
            .metrics
            .signature_candidates
            .checked_add(signature_count)?;
        shadow.metrics.projected_pairs_pruned = shadow
            .metrics
            .projected_pairs_pruned
            .checked_add(considered.checked_sub(signature_count)?)?;
        shadow.metrics.signature_not_in_edge_union = shadow
            .metrics
            .signature_not_in_edge_union
            .checked_add(outside)?;
        shadow.metrics.largest_signature_candidate_set = shadow
            .metrics
            .largest_signature_candidate_set
            .max(signature_count);
        match scope {
            NearSearchScope::PairedInterval => {
                shadow.metrics.paired_interval_pairs = shadow
                    .metrics
                    .paired_interval_pairs
                    .checked_add(considered)?;
            }
            NearSearchScope::PairedCrossIntervalVeto => {
                shadow.metrics.paired_cross_interval_pairs = shadow
                    .metrics
                    .paired_cross_interval_pairs
                    .checked_add(considered)?;
            }
            NearSearchScope::CrossSpan => {
                shadow.metrics.cross_span_shared_pairs = shadow
                    .metrics
                    .cross_span_shared_pairs
                    .checked_add(considered)?;
            }
            NearSearchScope::SameOrAmbiguousSpan => {
                let scoped_candidates = if direct { &signature } else { &*plausible };
                for index in scoped_candidates.iter().copied() {
                    let same_known = query.span_index.is_some()
                        && candidates
                            .get(index)
                            .and_then(|candidate| candidate.span_index)
                            == query.span_index;
                    let counter = if same_known {
                        &mut shadow.metrics.same_known_pairs
                    } else {
                        &mut shadow.metrics.ambiguous_pairs
                    };
                    *counter = counter.checked_add(1)?;
                }
            }
        }
        Some(())
    })();
    if observation.is_none() {
        shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
        return (shadow.mode != SentenceEdgeSignatureFilterMode::Direct).then_some(());
    }
    if direct {
        *plausible = signature;
    } else {
        plausible.retain(|index| signature.binary_search(index).is_ok());
    }
    checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn collect_unit_candidates_unless_direct(
    index: &UnitCandidateIndex,
    plausible: &mut Vec<usize>,
    query: &SentenceOccurrence,
    candidates: &[SentenceOccurrence],
    bucket: CandidatePostingBucket,
    additional_bucket: Option<CandidatePostingBucket>,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
) -> Option<UnitCandidateQueryMetrics> {
    if budget.sentence_edge_signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct
        && query.kind == RecoveryUnitKind::Sentence
    {
        plausible.clear();
        return Some(UnitCandidateQueryMetrics::default());
    }
    index.collect_plausible_occurrences_in_scope(
        plausible,
        query,
        candidates,
        bucket,
        additional_bucket,
        budget,
        scope,
    )
}

#[allow(clippy::too_many_arguments)]
fn probe_direct_watch_pairs(
    watch: Option<&mut RecoveryWatchState>,
    budget: &mut RecoveryBudget,
    legacy_index: &UnitCandidateIndex,
    query_side: OccurrenceSide,
    query_occurrence_index: usize,
    query: &SentenceOccurrence,
    candidates: &[SentenceOccurrence],
    signature_candidates: &[usize],
    bucket: CandidatePostingBucket,
    additional_bucket: Option<CandidatePostingBucket>,
    near_scope: RecoveryWatchNearScope,
    mut include: impl FnMut(usize) -> bool,
) -> Option<()> {
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Direct
        || query.kind != RecoveryUnitKind::Sentence
    {
        return Some(());
    }
    let Some(watch) = watch else {
        return Some(());
    };
    let mut partners = Vec::new();
    let watched = watch.partners(query_side, query_occurrence_index);
    if partners.try_reserve_exact(watched.len()).is_err() {
        budget
            .watch_probe_stop_reason
            .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure);
        return None;
    }
    partners.extend_from_slice(watched);
    for candidate_index in partners {
        let (old_occurrence_index, new_occurrence_index) = match query_side {
            OccurrenceSide::Old => (query_occurrence_index, candidate_index),
            OccurrenceSide::New => (candidate_index, query_occurrence_index),
        };
        if signature_candidates.binary_search(&candidate_index).is_ok()
            || watch.near_was_examined(old_occurrence_index, new_occurrence_index)
            || !include(candidate_index)
            || !legacy_index.contains_sentence_edge_candidate(
                query,
                candidate_index,
                bucket,
                additional_bucket,
            )
        {
            continue;
        }
        if !budget.charge_watch_probe_pair() {
            return None;
        }
        let Some(missing) = budget
            .watch_probe_missing_signature_candidates
            .checked_add(1)
        else {
            budget
                .watch_probe_stop_reason
                .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            return None;
        };
        budget.watch_probe_missing_signature_candidates = missing;
        let candidate = candidates.get(candidate_index)?;
        let evidence = cached_sentence_edge_evidence(query, candidate, || {
            budget.charge_watch_probe_comparison()
        })?;
        if evidence.edge_score() >= MIN_WORD_SCORE_EDGE_EVIDENCE {
            let Some(violations) = budget.watch_probe_invariant_violations.checked_add(1) else {
                budget
                    .watch_probe_stop_reason
                    .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
                return None;
            };
            budget.watch_probe_invariant_violations = violations;
            budget.watch_probe_stop_reason.get_or_insert(
                SentenceEdgeSignatureDirectShadowStopReason::WatchProbeInvariantViolation,
            );
            return None;
        }
        watch.record_near(
            old_occurrence_index,
            new_occurrence_index,
            evidence.edge_score(),
            near_scope,
        );
    }
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn record_signature_exact_retained(
    shadow: Option<&mut SentenceEdgeSignatureShadow>,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    budget: &RecoveryBudget,
    edge_filter: &SentenceEdgeFilterQuery,
    plausible: &[usize],
    query: &SentenceOccurrence,
    candidates: &[SentenceOccurrence],
    query_side: OccurrenceSide,
    query_occurrence: usize,
    scope: NearSearchScope,
) -> Option<()> {
    let Some(shadow) = shadow.filter(|shadow| shadow.active) else {
        return Some(());
    };
    if shadow.mode == SentenceEdgeSignatureFilterMode::ReferenceObserve
        && query.kind == RecoveryUnitKind::Sentence
    {
        if shadow
            .retained_fingerprint
            .record_reference_filter_progress(budget)
            .is_none()
        {
            shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
            return finish_signature_recording_failure(
                shadow,
                diagnostics,
                signature_checkpoint,
                budget,
            );
        }
        if let Some(reason) = budget.sentence_edge_filter_stop_reason {
            shadow.stop(reference_edge_filter_shadow_stop_reason(reason));
            return finish_signature_recording_failure(
                shadow,
                diagnostics,
                signature_checkpoint,
                budget,
            );
        }
    }
    let retained = edge_filter.retained_occurrence_count();
    if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
        let Some(cross_only) = edge_filter.cross_orientation_only_count(query, candidates) else {
            shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
            return Some(());
        };
        let Some(next) = shadow
            .direct_metrics
            .cross_orientation_only_candidates
            .checked_add(cross_only)
        else {
            shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
            return Some(());
        };
        shadow.direct_metrics.cross_orientation_only_candidates = next;
    }
    let Some(next) = shadow
        .metrics
        .exact_edge_retained_pairs
        .checked_add(retained)
    else {
        shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
        return finish_signature_recording_failure(
            shadow,
            diagnostics,
            signature_checkpoint,
            budget,
        );
    };
    shadow.metrics.exact_edge_retained_pairs = next;
    for pair_index in 0..retained {
        let Some((candidate_occurrence, _)) = edge_filter.pair(plausible, pair_index) else {
            shadow.stop(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure);
            return finish_signature_recording_failure(
                shadow,
                diagnostics,
                signature_checkpoint,
                budget,
            );
        };
        let (old_occurrence, new_occurrence) = match query_side {
            OccurrenceSide::Old => (query_occurrence, candidate_occurrence),
            OccurrenceSide::New => (candidate_occurrence, query_occurrence),
        };
        if shadow
            .retained_fingerprint
            .record(scope, query_side, old_occurrence, new_occurrence)
            .is_none()
        {
            if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
                shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            } else {
                shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
            }
            return finish_signature_recording_failure(
                shadow,
                diagnostics,
                signature_checkpoint,
                budget,
            );
        }
    }
    checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    Some(())
}

fn finish_signature_recording_failure(
    shadow: &SentenceEdgeSignatureShadow,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    budget: &RecoveryBudget,
) -> Option<()> {
    let reference_observer = shadow.mode == SentenceEdgeSignatureFilterMode::ReferenceObserve;
    if reference_observer {
        record_near_search_metrics(diagnostics, budget);
    }
    checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    (!reference_observer).then_some(())
}

#[derive(Clone, Copy)]
struct ClassifiedSentenceEdgeFilter {
    occurrence_index: usize,
    evidence: CachedSentenceEdgeEvidence,
}

struct SentenceEdgeGateShadow {
    old: Vec<CandidateNearRelation>,
    new: Vec<CandidateNearRelation>,
    metrics: SentenceEdgeGateShadowMetrics,
    active: bool,
}

impl SentenceEdgeGateShadow {
    fn disabled(reason: SentenceEdgeGateShadowStopReason) -> Self {
        Self {
            old: Vec::new(),
            new: Vec::new(),
            metrics: SentenceEdgeGateShadowMetrics {
                complete: false,
                stop_reason: Some(reason),
                ..SentenceEdgeGateShadowMetrics::default()
            },
            active: false,
        }
    }

    fn disable(&mut self, reason: SentenceEdgeGateShadowStopReason) {
        self.old.clear();
        self.new.clear();
        self.metrics.complete = false;
        self.metrics.stop_reason.get_or_insert(reason);
        self.active = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn score_and_record_sentence_edge_gate_shadow(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
    probe: Option<&mut RelationFloorProbe>,
    shadow: Option<&mut SentenceEdgeGateShadow>,
    old_candidate_index: Option<usize>,
    new_candidate_index: Option<usize>,
    cached: Option<CachedSentenceEdgeEvidence>,
) -> Option<u16> {
    let comparisons_before = budget.comparisons;
    let evidence = match cached {
        Some(cached) => sentence_edge_evidence_from_cache(old, new, budget, scope, class, cached)?,
        None => sentence_edge_evidence(old, new, budget, scope, class)?,
    };
    let edge_score = evidence.edge_score();
    let score = match probe {
        Some(probe) => {
            sentence_similarity_in_scope_attributed_from_edge_evidence_with_probe(evidence, probe)?
        }
        None => sentence_similarity_in_scope_attributed_from_edge_evidence(evidence)?,
    };
    let Some(shadow) = shadow.filter(|shadow| shadow.active) else {
        return Some(score);
    };
    let retained = old.kind == RecoveryUnitKind::Line
        || new.kind == RecoveryUnitKind::Line
        || edge_score >= MIN_WORD_SCORE_EDGE_EVIDENCE;
    let observation = (|| {
        if old.kind == RecoveryUnitKind::Sentence && new.kind == RecoveryUnitKind::Sentence {
            shadow.metrics.pairs_considered = shadow
                .metrics
                .pairs_considered
                .checked_add(1)
                .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
            let counter = if retained {
                &mut shadow.metrics.pairs_retained
            } else {
                &mut shadow.metrics.pairs_rejected
            };
            *counter = counter
                .checked_add(1)
                .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
            if retained {
                shadow.metrics.projected_pair_visits = shadow
                    .metrics
                    .projected_pair_visits
                    .checked_add(1)
                    .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
                let comparisons = budget
                    .comparisons
                    .checked_sub(comparisons_before)
                    .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
                shadow.metrics.projected_similarity_comparisons = shadow
                    .metrics
                    .projected_similarity_comparisons
                    .checked_add(comparisons)
                    .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
            } else {
                record_sentence_edge_gate_rejection(&mut shadow.metrics, scope, old, new, score)?;
            }
        }
        if retained {
            match (old_candidate_index, new_candidate_index) {
                (Some(old_index), Some(new_index)) => {
                    shadow
                        .old
                        .get_mut(old_index)
                        .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                        .record_eligible(new_index, score);
                    shadow
                        .new
                        .get_mut(new_index)
                        .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                        .record_eligible(old_index, score);
                }
                (Some(old_index), None) => shadow
                    .old
                    .get_mut(old_index)
                    .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                    .record_disqualifying(score),
                (None, Some(new_index)) => shadow
                    .new
                    .get_mut(new_index)
                    .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                    .record_disqualifying(score),
                (None, None) => return Err(SentenceEdgeGateShadowStopReason::DiagnosticFailure),
            }
        }
        Ok(())
    })();
    if let Err(reason) = observation {
        shadow.disable(reason);
    }
    Some(score)
}

enum SentenceEdgeFilterQuery {
    Legacy,
    Filtered {
        retained: Vec<ClassifiedSentenceEdgeFilter>,
        rejected: Option<Vec<ClassifiedSentenceEdgeFilter>>,
        cross_orientation_only: usize,
    },
}

impl SentenceEdgeFilterQuery {
    fn len(&self, legacy: &[usize]) -> usize {
        match self {
            Self::Legacy => legacy.len(),
            Self::Filtered { retained, .. } => retained.len(),
        }
    }

    fn pair(
        &self,
        legacy: &[usize],
        index: usize,
    ) -> Option<(usize, Option<CachedSentenceEdgeEvidence>)> {
        match self {
            Self::Legacy => Some((*legacy.get(index)?, None)),
            Self::Filtered { retained, .. } => {
                let retained = retained.get(index)?;
                Some((retained.occurrence_index, Some(retained.evidence)))
            }
        }
    }

    fn retained_occurrence_count(&self) -> usize {
        match self {
            Self::Legacy => 0,
            Self::Filtered { retained, .. } => retained.len(),
        }
    }

    fn cross_orientation_only_count(
        &self,
        query: &SentenceOccurrence,
        occurrences: &[SentenceOccurrence],
    ) -> Option<usize> {
        let Self::Filtered {
            rejected,
            cross_orientation_only,
            ..
        } = self
        else {
            return Some(0);
        };
        let Some(rejected) = rejected else {
            return Some(*cross_orientation_only);
        };
        rejected.iter().try_fold(0usize, |count, rejected| {
            let occurrence = occurrences.get(rejected.occurrence_index)?;
            count.checked_add(usize::from(is_cross_orientation_only(query, occurrence)?))
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn record_rejections(
        &self,
        query_occurrence_index: usize,
        query_side: OccurrenceSide,
        query: &SentenceOccurrence,
        occurrences: &[SentenceOccurrence],
        scope: NearSearchScope,
        shadow: Option<&mut SentenceEdgeGateShadow>,
        mut watch: Option<&mut RecoveryWatchState>,
        watch_scope: RecoveryWatchNearScope,
    ) {
        let Self::Filtered {
            rejected: Some(rejected),
            ..
        } = self
        else {
            return;
        };
        let mut shadow = shadow;
        for rejected in rejected {
            let Some(occurrence) = occurrences.get(rejected.occurrence_index) else {
                if let Some(shadow) = shadow.as_deref_mut() {
                    shadow.disable(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
                }
                return;
            };
            let observation = (|| {
                let shadow = shadow.as_deref_mut().filter(|shadow| shadow.active);
                let Some(shadow) = shadow else {
                    return Ok(());
                };
                shadow.metrics.pairs_considered = shadow
                    .metrics
                    .pairs_considered
                    .checked_add(1)
                    .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
                shadow.metrics.pairs_rejected = shadow
                    .metrics
                    .pairs_rejected
                    .checked_add(1)
                    .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
                record_sentence_edge_gate_rejection(
                    &mut shadow.metrics,
                    scope,
                    query,
                    occurrence,
                    rejected.evidence.edge_score(),
                )
            })();
            if let Err(reason) = observation
                && let Some(shadow) = shadow.as_deref_mut()
            {
                shadow.disable(reason);
            }
            if let Some(watch) = watch.as_deref_mut() {
                let (old_index, new_index) = match query_side {
                    OccurrenceSide::Old => (query_occurrence_index, rejected.occurrence_index),
                    OccurrenceSide::New => (rejected.occurrence_index, query_occurrence_index),
                };
                watch.record_near(
                    old_index,
                    new_index,
                    rejected.evidence.edge_score(),
                    watch_scope,
                );
            }
        }
    }
}

struct SentenceEdgeRejectionObserver<'a> {
    query_occurrence_index: usize,
    query_side: OccurrenceSide,
    scope: NearSearchScope,
    shadow: Option<&'a mut SentenceEdgeGateShadow>,
    watch: Option<&'a mut RecoveryWatchState>,
    watch_scope: RecoveryWatchNearScope,
}

impl SentenceEdgeRejectionObserver<'_> {
    fn record(
        &mut self,
        query: &SentenceOccurrence,
        occurrence_index: usize,
        occurrence: &SentenceOccurrence,
        evidence: CachedSentenceEdgeEvidence,
        budget: &mut RecoveryBudget,
    ) -> Option<()> {
        if let Some(shadow) = self.shadow.as_deref_mut().filter(|shadow| shadow.active) {
            let observation = (|| {
                shadow.metrics.pairs_considered = shadow
                    .metrics
                    .pairs_considered
                    .checked_add(1)
                    .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
                shadow.metrics.pairs_rejected = shadow
                    .metrics
                    .pairs_rejected
                    .checked_add(1)
                    .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
                record_sentence_edge_gate_rejection(
                    &mut shadow.metrics,
                    self.scope,
                    query,
                    occurrence,
                    evidence.edge_score(),
                )
            })();
            if let Err(reason) = observation {
                shadow.disable(reason);
                budget.record_watch_diagnostic_failure();
                return None;
            }
        }
        if let Some(watch) = self.watch.as_deref_mut() {
            if !watch.complete {
                budget.record_watch_diagnostic_failure();
                return None;
            }
            let (old_index, new_index) = match self.query_side {
                OccurrenceSide::Old => (self.query_occurrence_index, occurrence_index),
                OccurrenceSide::New => (occurrence_index, self.query_occurrence_index),
            };
            watch.record_near(
                old_index,
                new_index,
                evidence.edge_score(),
                self.watch_scope,
            );
            if !watch.complete {
                budget.record_watch_diagnostic_failure();
                return None;
            }
        }
        Some(())
    }
}

fn mark_edge_gate_shadow_for_filter_stop(
    shadow: Option<&mut SentenceEdgeGateShadow>,
    reason: Option<SentenceEdgeFilterStopReason>,
) {
    if let (Some(shadow), Some(reason)) = (shadow, reason) {
        shadow.disable(reason.into());
    }
}

#[cfg(test)]
fn classify_sentence_edge_filter_query(
    query: &SentenceOccurrence,
    occurrences: &[SentenceOccurrence],
    plausible: &[usize],
    budget: &mut RecoveryBudget,
    relevant: impl FnMut(usize) -> bool,
) -> SentenceEdgeFilterQuery {
    classify_sentence_edge_filter_query_with_index(
        query,
        occurrences,
        plausible,
        budget,
        None,
        None,
        relevant,
    )
}

#[allow(clippy::too_many_arguments)]
fn classify_sentence_edge_filter_query_with_index(
    query: &SentenceOccurrence,
    occurrences: &[SentenceOccurrence],
    plausible: &[usize],
    budget: &mut RecoveryBudget,
    candidate_index: Option<(
        &UnitCandidateIndex,
        CandidatePostingBucket,
        Option<CandidatePostingBucket>,
    )>,
    mut rejection_observer: Option<SentenceEdgeRejectionObserver<'_>>,
    mut relevant: impl FnMut(usize) -> bool,
) -> SentenceEdgeFilterQuery {
    if query.kind != RecoveryUnitKind::Sentence || !budget.sentence_edge_filter_active {
        return SentenceEdgeFilterQuery::Legacy;
    }
    let mut retained = Vec::new();
    let direct =
        budget.sentence_edge_signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct;
    let mut rejected = (!direct).then(Vec::new);
    if retained.try_reserve_exact(plausible.len()).is_err()
        || rejected
            .as_mut()
            .is_some_and(|rejected| rejected.try_reserve_exact(plausible.len()).is_err())
    {
        budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::AllocationFailure);
        return SentenceEdgeFilterQuery::Legacy;
    }
    let mut retained_count = 0usize;
    let mut rejected_count = 0usize;
    let mut cross_orientation_only = 0usize;
    for &occurrence_index in plausible {
        if !relevant(occurrence_index) {
            continue;
        }
        if !budget.charge_sentence_edge_filter_pair() {
            return SentenceEdgeFilterQuery::Legacy;
        }
        let Some(occurrence) = occurrences.get(occurrence_index) else {
            budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
            return SentenceEdgeFilterQuery::Legacy;
        };
        let aligned_facts = (budget.sentence_edge_signature_filter_mode
            == SentenceEdgeSignatureFilterMode::ReferenceObserve)
            .then(|| {
                let (index, bucket, additional_bucket) = candidate_index?;
                index.aligned_sentence_edge_facts(
                    query,
                    occurrence_index,
                    bucket,
                    additional_bucket,
                )
            })
            .flatten();
        let evidence = match aligned_facts {
            Some(facts) => cached_sentence_edge_evidence_from_aligned_facts(
                query,
                occurrence,
                facts.prefix_equal(),
                facts.suffix_equal(),
                || budget.charge_sentence_edge_filter_comparison(),
            ),
            None => cached_sentence_edge_evidence(query, occurrence, || {
                budget.charge_sentence_edge_filter_comparison()
            }),
        };
        let Some(evidence) = evidence else {
            if budget.sentence_edge_filter_active {
                budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
            }
            return SentenceEdgeFilterQuery::Legacy;
        };
        if evidence.edge_score() >= MIN_WORD_SCORE_EDGE_EVIDENCE {
            let Some(next) = retained_count.checked_add(1) else {
                budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
                return SentenceEdgeFilterQuery::Legacy;
            };
            retained_count = next;
            retained.push(ClassifiedSentenceEdgeFilter {
                occurrence_index,
                evidence,
            });
        } else {
            let Some(next) = rejected_count.checked_add(1) else {
                budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
                return SentenceEdgeFilterQuery::Legacy;
            };
            rejected_count = next;
            if direct {
                let Some(next) =
                    is_cross_orientation_only(query, occurrence).and_then(|cross_only| {
                        cross_orientation_only.checked_add(usize::from(cross_only))
                    })
                else {
                    budget.record_watch_diagnostic_failure();
                    return SentenceEdgeFilterQuery::Legacy;
                };
                cross_orientation_only = next;
                if rejection_observer.as_mut().is_some_and(|observer| {
                    observer
                        .record(query, occurrence_index, occurrence, evidence, budget)
                        .is_none()
                }) {
                    return SentenceEdgeFilterQuery::Legacy;
                }
            } else if let Some(rejected) = rejected.as_mut() {
                rejected.push(ClassifiedSentenceEdgeFilter {
                    occurrence_index,
                    evidence,
                });
            }
        }
    }
    let Some(next_retained) = budget
        .sentence_edge_filter_pairs_retained
        .checked_add(retained_count)
    else {
        budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
        return SentenceEdgeFilterQuery::Legacy;
    };
    let Some(next_rejected) = budget
        .sentence_edge_filter_pairs_rejected
        .checked_add(rejected_count)
    else {
        budget.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
        return SentenceEdgeFilterQuery::Legacy;
    };
    budget.sentence_edge_filter_pairs_retained = next_retained;
    budget.sentence_edge_filter_pairs_rejected = next_rejected;
    SentenceEdgeFilterQuery::Filtered {
        retained,
        rejected,
        cross_orientation_only,
    }
}

fn split_retained_sentence_edge_filters(
    retained: &[ClassifiedSentenceEdgeFilter],
    query_span: Option<usize>,
    occurrences: &[SentenceOccurrence],
) -> Option<NearSearchWorkSplit> {
    let mut split = NearSearchWorkSplit::default();
    for retained in retained {
        let occurrence = occurrences.get(retained.occurrence_index)?;
        let counter = match same_or_ambiguous_work_class(query_span, occurrence.span_index) {
            NearSearchWorkClass::SameKnown => &mut split.same_known,
            NearSearchWorkClass::Ambiguous => &mut split.ambiguous,
            NearSearchWorkClass::Shared => &mut split.shared,
        };
        *counter = counter.checked_add(1)?;
    }
    Some(split)
}

fn record_sentence_edge_gate_rejection(
    metrics: &mut SentenceEdgeGateShadowMetrics,
    scope: NearSearchScope,
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    score: u16,
) -> std::result::Result<(), SentenceEdgeGateShadowStopReason> {
    metrics.rejected_max_production_score = metrics.rejected_max_production_score.max(score);
    metrics.threshold_violations = metrics
        .threshold_violations
        .checked_add(usize::from(score >= MIN_WORD_SCORE_EDGE_EVIDENCE))
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    let counter = match scope {
        NearSearchScope::CrossSpan | NearSearchScope::PairedCrossIntervalVeto => {
            &mut metrics.cross_span_rejected
        }
        _ if old.span_index.is_some() && old.span_index == new.span_index => {
            &mut metrics.same_known_rejected
        }
        _ if old.span_index.is_none() || new.span_index.is_none() => {
            &mut metrics.ambiguous_rejected
        }
        _ => &mut metrics.unclassified_rejected,
    };
    *counter = counter
        .checked_add(1)
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    Ok(())
}

struct KnownSpanSentenceReplay<'a> {
    relations: &'a mut ModifiedSentenceRelations,
    metrics: &'a mut KnownSpanSentenceShadowMetrics,
    old_intervals: &'a [Option<PairedInterval>],
    new_intervals: &'a [Option<PairedInterval>],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SentencePairLocality {
    SamePairedAnchorInterval,
    SamePairedStreamOtherInterval,
    SamePageOnly,
    Unclassified,
}

struct StreamPlan {
    block_indices: Vec<usize>,
    trusted: bool,
    run_id: Option<TrustedRunId>,
}

#[derive(Clone, Copy)]
struct IntervalBlock {
    block_index: usize,
    start: usize,
    end: usize,
}

enum StreamPlanGroup {
    Trusted {
        run_id: TrustedRunId,
        blocks: Vec<IntervalBlock>,
    },
    Untrusted(usize),
}

struct RunRecoveryEvidence<'a> {
    descriptors: &'a [TrustedRunDescriptor],
    raw_region_edges: &'a [TrustedRegionEdge],
    descriptor_by_id: HashMap<TrustedRunId, usize>,
    descriptor_indices_by_block: HashMap<usize, Vec<usize>>,
}

impl<'a> RunRecoveryEvidence<'a> {
    fn new(input: TrustedRunRecoveryInput<'a>) -> Option<Self> {
        let mut descriptor_by_id = HashMap::new();
        descriptor_by_id.try_reserve(input.descriptors.len()).ok()?;
        let membership_count = input
            .descriptors
            .iter()
            .try_fold(0usize, |count, descriptor| {
                count.checked_add(descriptor.block_indices.len())
            })?;
        let mut descriptor_indices_by_block = HashMap::<usize, Vec<usize>>::new();
        descriptor_indices_by_block
            .try_reserve(membership_count)
            .ok()?;
        for (index, descriptor) in input.descriptors.iter().enumerate() {
            if descriptor_by_id.insert(descriptor.id, index).is_some() {
                return None;
            }
            for block_index in &descriptor.block_indices {
                let indices = descriptor_indices_by_block.entry(*block_index).or_default();
                indices.try_reserve(1).ok()?;
                indices.push(index);
            }
        }
        Some(Self {
            descriptors: input.descriptors,
            raw_region_edges: input.raw_region_edges,
            descriptor_by_id,
            descriptor_indices_by_block,
        })
    }

    fn descriptor_index(&self, run_id: TrustedRunId) -> Option<usize> {
        self.descriptor_by_id.get(&run_id).copied()
    }

    fn descriptor_for_occurrence(
        &self,
        occurrence: &SentenceOccurrence,
    ) -> Option<&TrustedRunDescriptor> {
        self.descriptors.get(occurrence.run_descriptor_index?)
    }

    fn descriptor_indices_for_untrusted_occurrence(
        &self,
        occurrence: &SentenceOccurrence,
    ) -> Option<&[usize]> {
        self.descriptor_indices_by_block
            .get(&occurrence.evidence_block_index?)
            .map(Vec::as_slice)
    }

    fn raw_region_edges(&self) -> &[TrustedRegionEdge] {
        self.raw_region_edges
    }
}

struct RecoveryStructuralEvidence<'a> {
    old: Option<RunRecoveryEvidence<'a>>,
    new: Option<RunRecoveryEvidence<'a>>,
}

impl<'a> RecoveryStructuralEvidence<'a> {
    fn new(input: SentenceRecoveryInput<'a>) -> Option<Self> {
        Some(Self {
            old: match input.old_trusted_run_evidence {
                Some(input) => Some(RunRecoveryEvidence::new(input)?),
                None => None,
            },
            new: match input.new_trusted_run_evidence {
                Some(input) => Some(RunRecoveryEvidence::new(input)?),
                None => None,
            },
        })
    }
}

const STRUCTURAL_EDGE_HISTOGRAM_BINS: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct StructuralProfile {
    role: BlockRole,
    source_region_count: usize,
    edge_histogram: [usize; STRUCTURAL_EDGE_HISTOGRAM_BINS],
}

#[derive(Default)]
struct StructuralSideProfiles {
    descriptor_count: usize,
    eligible_count: usize,
    mixed_count: usize,
    split_count: usize,
    eligible: Vec<bool>,
    postings: HashMap<StructuralProfile, Vec<usize>>,
}

#[derive(Default)]
struct StructuralPairingPlan {
    old: StructuralSideProfiles,
    new: StructuralSideProfiles,
    shared_profiles: usize,
    candidate_pairs: usize,
    largest_posting: usize,
    duplicate_pairs: usize,
    unique_pairs: Vec<(usize, usize)>,
}

fn structural_pairing_plan(
    evidence: &RecoveryStructuralEvidence<'_>,
    old_intervals: &[Option<TrustedRunInterval>],
    new_intervals: &[Option<TrustedRunInterval>],
) -> Option<StructuralPairingPlan> {
    let old_evidence = evidence.old.as_ref()?;
    let new_evidence = evidence.new.as_ref()?;
    let old = structural_side_profiles(old_evidence, old_intervals)?;
    let new = structural_side_profiles(new_evidence, new_intervals)?;
    let shared_capacity = old.postings.len().min(new.postings.len());
    let mut unique_pairs = Vec::new();
    unique_pairs.try_reserve_exact(shared_capacity).ok()?;
    let mut shared_profiles = 0usize;
    let mut candidate_pairs = 0usize;
    let mut largest_posting = 0usize;
    let mut duplicate_pairs = 0usize;
    for (profile, old_posting) in &old.postings {
        let Some(new_posting) = new.postings.get(profile) else {
            continue;
        };
        shared_profiles = shared_profiles.checked_add(1)?;
        let pairs = old_posting.len().checked_mul(new_posting.len())?;
        candidate_pairs = candidate_pairs.checked_add(pairs)?;
        largest_posting = largest_posting
            .max(old_posting.len())
            .max(new_posting.len());
        if let ([old_index], [new_index]) = (old_posting.as_slice(), new_posting.as_slice()) {
            unique_pairs.push((*old_index, *new_index));
        } else {
            duplicate_pairs = duplicate_pairs.checked_add(pairs)?;
        }
    }
    Some(StructuralPairingPlan {
        old,
        new,
        shared_profiles,
        candidate_pairs,
        largest_posting,
        duplicate_pairs,
        unique_pairs,
    })
}

fn structural_side_profiles(
    evidence: &RunRecoveryEvidence<'_>,
    intervals: &[Option<TrustedRunInterval>],
) -> Option<StructuralSideProfiles> {
    let plans = stream_plans(intervals)?;
    let mut plans_by_run = HashMap::<TrustedRunId, Vec<usize>>::new();
    plans_by_run.try_reserve(plans.len()).ok()?;
    for (plan_index, plan) in plans.iter().enumerate() {
        let Some(run_id) = plan.run_id else { continue };
        let indices = plans_by_run.entry(run_id).or_default();
        indices.try_reserve(1).ok()?;
        indices.push(plan_index);
    }

    let descriptor_count = evidence.descriptors.len();
    let mut eligible = Vec::new();
    eligible.try_reserve_exact(descriptor_count).ok()?;
    let mut eligible_count = 0usize;
    let mut mixed_count = 0usize;
    let mut split_count = 0usize;
    for descriptor in evidence.descriptors {
        let is_eligible = if descriptor.role.is_none() {
            mixed_count = mixed_count.checked_add(1)?;
            false
        } else if descriptor.trusted_block_indices.is_empty()
            || descriptor.block_indices != descriptor.trusted_block_indices
        {
            split_count = split_count.checked_add(1)?;
            false
        } else {
            let matching_plan = plans_by_run
                .get(&descriptor.id)
                .filter(|indices| indices.len() == 1)
                .and_then(|indices| plans.get(indices[0]));
            if matching_plan.is_some_and(|plan| {
                plan.trusted && plan.block_indices == descriptor.trusted_block_indices
            }) {
                eligible_count = eligible_count.checked_add(1)?;
                true
            } else {
                split_count = split_count.checked_add(1)?;
                false
            }
        };
        eligible.push(is_eligible);
    }
    if descriptor_count
        != eligible_count
            .checked_add(mixed_count)?
            .checked_add(split_count)?
    {
        return None;
    }

    let membership_count = evidence.descriptors.iter().zip(&eligible).try_fold(
        0usize,
        |count, (descriptor, eligible)| {
            if *eligible {
                count.checked_add(descriptor.source_region_ids.len())
            } else {
                Some(count)
            }
        },
    )?;
    let mut descriptors_by_region = HashMap::<(PageId, RegionId), Vec<usize>>::new();
    descriptors_by_region.try_reserve(membership_count).ok()?;
    let mut membership = HashSet::<(usize, PageId, RegionId)>::new();
    membership.try_reserve(membership_count).ok()?;
    for (descriptor_index, (descriptor, is_eligible)) in
        evidence.descriptors.iter().zip(&eligible).enumerate()
    {
        if !is_eligible {
            continue;
        }
        for region_id in &descriptor.source_region_ids {
            if !membership.insert((descriptor_index, descriptor.page, *region_id)) {
                return None;
            }
            let posting = descriptors_by_region
                .entry((descriptor.page, *region_id))
                .or_default();
            posting.try_reserve(1).ok()?;
            posting.push(descriptor_index);
        }
    }

    let mut histograms = Vec::new();
    histograms.try_reserve_exact(descriptor_count).ok()?;
    histograms.resize(descriptor_count, [0usize; STRUCTURAL_EDGE_HISTOGRAM_BINS]);
    for edge in evidence.raw_region_edges {
        if let Some(posting) = descriptors_by_region.get(&(edge.page, edge.source)) {
            for descriptor_index in posting {
                let internal = membership.contains(&(*descriptor_index, edge.page, edge.target));
                increment_histogram(
                    &mut histograms[*descriptor_index],
                    edge.relation,
                    false,
                    internal,
                )?;
            }
        }
        if let Some(posting) = descriptors_by_region.get(&(edge.page, edge.target)) {
            for descriptor_index in posting {
                let internal = membership.contains(&(*descriptor_index, edge.page, edge.source));
                increment_histogram(
                    &mut histograms[*descriptor_index],
                    edge.relation,
                    true,
                    internal,
                )?;
            }
        }
    }

    let mut postings = HashMap::<StructuralProfile, Vec<usize>>::new();
    postings.try_reserve(eligible_count).ok()?;
    for (descriptor_index, (descriptor, is_eligible)) in
        evidence.descriptors.iter().zip(&eligible).enumerate()
    {
        if !*is_eligible {
            continue;
        }
        let posting = postings
            .entry(StructuralProfile {
                role: descriptor.role?,
                source_region_count: descriptor.source_region_ids.len(),
                edge_histogram: histograms[descriptor_index],
            })
            .or_default();
        posting.try_reserve(1).ok()?;
        posting.push(descriptor_index);
    }
    Some(StructuralSideProfiles {
        descriptor_count,
        eligible_count,
        mixed_count,
        split_count,
        eligible,
        postings,
    })
}

fn descriptor_recovery_eligibility(
    profiles: &StructuralSideProfiles,
    evidence: &RunRecoveryEvidence<'_>,
    side: &Side<'_>,
    span_by_block: &HashMap<BlockId, usize>,
    recovery_spans: &[bool],
) -> Option<Vec<bool>> {
    if profiles.eligible.len() != evidence.descriptors.len() {
        return None;
    }
    let mut eligible = Vec::new();
    eligible.try_reserve_exact(profiles.eligible.len()).ok()?;
    for (descriptor, structurally_eligible) in evidence.descriptors.iter().zip(&profiles.eligible) {
        let mut fully_contained = *structurally_eligible && !descriptor.block_indices.is_empty();
        for block_index in &descriptor.block_indices {
            let block = side.blocks.get(*block_index)?;
            let in_recovery_span = span_by_block
                .get(&block.block)
                .and_then(|span_index| recovery_spans.get(*span_index))
                .copied()
                .unwrap_or(false);
            fully_contained &= in_recovery_span;
        }
        eligible.push(fully_contained);
    }
    Some(eligible)
}

fn increment_histogram(
    histogram: &mut [usize; STRUCTURAL_EDGE_HISTOGRAM_BINS],
    relation: RegionRelation,
    incoming: bool,
    internal: bool,
) -> Option<()> {
    let relation_index = match relation {
        RegionRelation::Above => 0,
        RegionRelation::Below => 1,
        RegionRelation::LeftOf => 2,
        RegionRelation::RightOf => 3,
        RegionRelation::Aligned => 4,
        RegionRelation::SameColumn => 5,
    };
    let index = relation_index * 4 + usize::from(incoming) * 2 + usize::from(internal);
    histogram[index] = histogram[index].checked_add(1)?;
    Some(())
}

fn record_structural_pairing_plan(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    plan: Option<&StructuralPairingPlan>,
) {
    let (Some(diagnostics), Some(plan)) = (diagnostics.as_mut(), plan) else {
        return;
    };
    let metrics = &mut diagnostics.metrics;
    metrics.structural_pairing_available = true;
    metrics.old_structural_descriptors = plan.old.descriptor_count;
    metrics.new_structural_descriptors = plan.new.descriptor_count;
    metrics.old_structural_eligible_descriptors = plan.old.eligible_count;
    metrics.new_structural_eligible_descriptors = plan.new.eligible_count;
    metrics.old_structural_mixed_descriptors = plan.old.mixed_count;
    metrics.new_structural_mixed_descriptors = plan.new.mixed_count;
    metrics.old_structural_split_descriptors = plan.old.split_count;
    metrics.new_structural_split_descriptors = plan.new.split_count;
    metrics.structural_shared_profiles = plan.shared_profiles;
    metrics.structural_candidate_pairs = plan.candidate_pairs;
    metrics.structural_largest_posting = plan.largest_posting;
    metrics.structural_duplicate_pairs = plan.duplicate_pairs;
    metrics.structural_unique_reciprocal_pairs = plan.unique_pairs.len();
    metrics.structural_unique_no_anchor_pairs = plan.unique_pairs.len();
}

fn classify_structural_pair_anchors(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    plan: Option<&StructuralPairingPlan>,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
) {
    let Some(plan) = plan else { return };
    let Some((no_anchor, monotone, crossing)) =
        structural_anchor_classes(plan, old_occurrences, new_occurrences, exact_candidates)
    else {
        clear_structural_pairing_metrics(diagnostics);
        return;
    };
    let Some(metrics) = diagnostics
        .as_mut()
        .map(|diagnostics| &mut diagnostics.metrics)
    else {
        return;
    };
    metrics.structural_unique_no_anchor_pairs = no_anchor;
    metrics.structural_unique_monotone_anchor_pairs = monotone;
    metrics.structural_unique_crossing_veto_pairs = crossing;
}

fn structural_anchor_classes(
    plan: &StructuralPairingPlan,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
) -> Option<(usize, usize, usize)> {
    let mut eligible_pairs = HashSet::new();
    eligible_pairs.try_reserve(plan.unique_pairs.len()).ok()?;
    eligible_pairs.extend(plan.unique_pairs.iter().copied());
    let mut anchors = HashMap::<(usize, usize), Vec<(usize, usize)>>::new();
    anchors.try_reserve(plan.unique_pairs.len()).ok()?;
    for candidate in exact_candidates {
        let old = old_occurrences.get(candidate.old_occurrence_index)?;
        let new = new_occurrences.get(candidate.new_occurrence_index)?;
        let (Some(old_descriptor), Some(new_descriptor)) =
            (old.run_descriptor_index, new.run_descriptor_index)
        else {
            continue;
        };
        if !eligible_pairs.contains(&(old_descriptor, new_descriptor)) {
            continue;
        }
        let (Some(old_position), Some(new_position)) = (old.trusted_position, new.trusted_position)
        else {
            continue;
        };
        let posting = anchors.entry((old_descriptor, new_descriptor)).or_default();
        posting.try_reserve(1).ok()?;
        posting.push((old_position.ordinal, new_position.ordinal));
    }

    let mut no_anchor = 0usize;
    let mut monotone = 0usize;
    let mut crossing = 0usize;
    for pair in &plan.unique_pairs {
        let Some(posting) = anchors.get_mut(pair) else {
            no_anchor = no_anchor.checked_add(1)?;
            continue;
        };
        posting.sort_unstable();
        if posting
            .windows(2)
            .all(|window| window[0].0 < window[1].0 && window[0].1 < window[1].1)
        {
            monotone = monotone.checked_add(1)?;
        } else {
            crossing = crossing.checked_add(1)?;
        }
    }
    if plan.unique_pairs.len() != no_anchor.checked_add(monotone)?.checked_add(crossing)? {
        return None;
    }
    Some((no_anchor, monotone, crossing))
}

fn clear_structural_pairing_metrics(diagnostics: &mut Option<SentenceRecoveryDiagnostics>) {
    let Some(metrics) = diagnostics
        .as_mut()
        .map(|diagnostics| &mut diagnostics.metrics)
    else {
        return;
    };
    metrics.structural_pairing_available = false;
    metrics.old_structural_descriptors = 0;
    metrics.new_structural_descriptors = 0;
    metrics.old_structural_eligible_descriptors = 0;
    metrics.new_structural_eligible_descriptors = 0;
    metrics.old_structural_mixed_descriptors = 0;
    metrics.new_structural_mixed_descriptors = 0;
    metrics.old_structural_split_descriptors = 0;
    metrics.new_structural_split_descriptors = 0;
    metrics.structural_shared_profiles = 0;
    metrics.structural_candidate_pairs = 0;
    metrics.structural_largest_posting = 0;
    metrics.structural_duplicate_pairs = 0;
    metrics.structural_unique_reciprocal_pairs = 0;
    metrics.structural_unique_no_anchor_pairs = 0;
    metrics.structural_unique_monotone_anchor_pairs = 0;
    metrics.structural_unique_crossing_veto_pairs = 0;
}

#[derive(Clone, Copy)]
struct RunUnitPosting {
    descriptor_index: usize,
    occurrence_index: usize,
    ordinal: usize,
}

enum RunLocalUnitState {
    Unique(usize),
    Duplicate,
}

struct RunUnitIndex<'a> {
    postings: HashMap<OccurrenceKey<'a>, Vec<RunUnitPosting>>,
    unique_units: usize,
    duplicate_units: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct RunSignatureScore {
    shared_units: usize,
    shared_tokens: usize,
}

#[derive(Default)]
struct RunPairScore {
    score: RunSignatureScore,
    ordinals: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, Default)]
struct RunBest {
    score: RunSignatureScore,
    second: RunSignatureScore,
    partner: Option<usize>,
    ambiguous: bool,
}

impl RunBest {
    fn record(&mut self, partner: usize, score: RunSignatureScore) {
        if score > self.score {
            self.second = self.score;
            self.score = score;
            self.partner = Some(partner);
            self.ambiguous = false;
        } else if score == self.score {
            self.second = self.second.max(score);
            self.partner = None;
            self.ambiguous = true;
        } else {
            self.second = self.second.max(score);
        }
    }

    fn unique_partner(self) -> Option<usize> {
        (!self.ambiguous && self.score > self.second).then_some(self.partner?)
    }
}

#[derive(Clone, Copy)]
struct RunSignatureBudget {
    posting_limit: usize,
    verification_limit: usize,
    candidate_pair_limit: usize,
    posting_attempted: usize,
    posting_examined: usize,
    verification_attempted: usize,
    verification_examined: usize,
    stop_reason: Option<RunSignatureStopReason>,
}

#[derive(Clone, Copy)]
struct RunSignatureLimits {
    min_tokens: usize,
    old_tokens: usize,
    new_tokens: usize,
    candidate_pairs: usize,
}

#[derive(Clone, Copy)]
struct RunSignatureEligibility<'a> {
    old: &'a [bool],
    new: &'a [bool],
}

impl RunSignatureBudget {
    fn new(old_tokens: usize, new_tokens: usize, candidate_pair_limit: usize) -> Option<Self> {
        let posting_limit = old_tokens.checked_add(new_tokens)?;
        Some(Self {
            posting_limit,
            verification_limit: posting_limit.checked_mul(4)?,
            candidate_pair_limit,
            posting_attempted: 0,
            posting_examined: 0,
            verification_attempted: 0,
            verification_examined: 0,
            stop_reason: None,
        })
    }

    fn admit_posting_product(&mut self, amount: usize) -> Option<bool> {
        self.posting_attempted = self.posting_attempted.checked_add(amount)?;
        if self.posting_examined.checked_add(amount)? > self.posting_limit {
            self.stop_reason = Some(RunSignatureStopReason::PostingVisitLimit);
            return Some(false);
        }
        Some(true)
    }

    fn record_posting_visit(&mut self) -> Option<()> {
        self.posting_examined = self.posting_examined.checked_add(1)?;
        (self.posting_examined <= self.posting_attempted).then_some(())
    }

    fn charge_verification(&mut self, amount: usize) -> Option<bool> {
        Self::charge(
            &mut self.verification_attempted,
            &mut self.verification_examined,
            amount,
            self.verification_limit,
            RunSignatureStopReason::TokenVerificationLimit,
            &mut self.stop_reason,
        )
    }

    fn admit_candidate_pair(&mut self, current_pairs: usize) -> bool {
        if current_pairs < self.candidate_pair_limit {
            true
        } else {
            self.stop_reason = Some(RunSignatureStopReason::CandidatePairLimit);
            false
        }
    }

    fn charge(
        attempted: &mut usize,
        examined: &mut usize,
        amount: usize,
        limit: usize,
        reason: RunSignatureStopReason,
        stop_reason: &mut Option<RunSignatureStopReason>,
    ) -> Option<bool> {
        *attempted = attempted.checked_add(amount)?;
        let next_examined = examined.checked_add(amount)?;
        if next_examined > limit {
            *stop_reason = Some(reason);
            return Some(false);
        }
        *examined = next_examined;
        Some(true)
    }
}

fn run_signature_metrics(
    eligibility: RunSignatureEligibility<'_>,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    exact_candidates: &[ExactMatchCandidate],
    limits: RunSignatureLimits,
) -> Option<SentenceRecoveryMetrics> {
    let (old_anchored, new_anchored) = globally_anchored_descriptors(
        exact_candidates,
        old_occurrences,
        new_occurrences,
        eligibility.old,
        eligibility.new,
    )?;
    let globally_anchored_runs_skipped = old_anchored.len().checked_add(new_anchored.len())?;
    let old_index = run_unit_index(old_occurrences, eligibility.old, &old_anchored)?;
    let new_index = run_unit_index(new_occurrences, eligibility.new, &new_anchored)?;
    let mut budget =
        RunSignatureBudget::new(limits.old_tokens, limits.new_tokens, limits.candidate_pairs)?;
    let mut shared_keys = Vec::new();
    shared_keys
        .try_reserve(old_index.postings.len().min(new_index.postings.len()))
        .ok()?;
    let mut largest_posting = 0usize;
    for (key, old_posting) in &old_index.postings {
        let Some(new_posting) = new_index.postings.get(key) else {
            continue;
        };
        let product = old_posting.len().checked_mul(new_posting.len())?;
        largest_posting = largest_posting
            .max(old_posting.len())
            .max(new_posting.len());
        shared_keys.push((*key, product));
    }
    shared_keys.sort_unstable_by(|(left_key, left_product), (right_key, right_product)| {
        left_product
            .cmp(right_product)
            .then_with(|| run_unit_sort_key(*left_key).cmp(&run_unit_sort_key(*right_key)))
    });

    let mut pairs = HashMap::<(usize, usize), RunPairScore>::new();
    'keys: for (key, product) in &shared_keys {
        if !budget.admit_posting_product(*product)? {
            break;
        }
        let old_posting = old_index.postings.get(key)?;
        let new_posting = new_index.postings.get(key)?;
        for old_unit in old_posting {
            for new_unit in new_posting {
                budget.record_posting_visit()?;
                let old_occurrence = old_occurrences.get(old_unit.occurrence_index)?;
                let new_occurrence = new_occurrences.get(new_unit.occurrence_index)?;
                let verification_tokens =
                    old_occurrence.tokens.len().max(new_occurrence.tokens.len());
                if !budget.charge_verification(verification_tokens)? {
                    break 'keys;
                }
                if old_occurrence.tokens != new_occurrence.tokens {
                    continue;
                }
                let pair_key = (old_unit.descriptor_index, new_unit.descriptor_index);
                if !pairs.contains_key(&pair_key) {
                    if !budget.admit_candidate_pair(pairs.len()) {
                        break 'keys;
                    }
                    pairs.try_reserve(1).ok()?;
                    pairs.insert(pair_key, RunPairScore::default());
                }
                let pair = pairs.get_mut(&pair_key)?;
                pair.score.shared_units = pair.score.shared_units.checked_add(1)?;
                pair.score.shared_tokens = pair
                    .score
                    .shared_tokens
                    .checked_add(old_occurrence.tokens.len())?;
                pair.ordinals.try_reserve(1).ok()?;
                pair.ordinals.push((old_unit.ordinal, new_unit.ordinal));
            }
        }
    }

    let complete = budget.stop_reason.is_none();
    let mut metrics = SentenceRecoveryMetrics {
        run_signature_available: true,
        run_signature_complete: complete,
        old_run_signature_unique_units: old_index.unique_units,
        new_run_signature_unique_units: new_index.unique_units,
        old_run_signature_duplicate_units: old_index.duplicate_units,
        new_run_signature_duplicate_units: new_index.duplicate_units,
        run_signature_shared_unit_keys: shared_keys.len(),
        run_signature_largest_posting: largest_posting,
        run_signature_posting_visits_attempted: budget.posting_attempted,
        run_signature_posting_visits_examined: budget.posting_examined,
        run_signature_token_verifications_attempted: budget.verification_attempted,
        run_signature_token_verifications_examined: budget.verification_examined,
        run_signature_candidate_pairs: pairs.len(),
        run_signature_globally_anchored_runs_skipped: globally_anchored_runs_skipped,
        run_signature_stop_reason: budget.stop_reason,
        ..SentenceRecoveryMetrics::default()
    };
    if complete {
        classify_run_signature_pairs(&mut metrics, &mut pairs, limits.min_tokens)?;
    }
    Some(metrics)
}

fn run_unit_sort_key(key: OccurrenceKey<'_>) -> (&str, u8, u8) {
    let kind = match key.1 {
        RecoveryUnitKind::Sentence => 0,
        RecoveryUnitKind::Line => 1,
    };
    let role = match key.2 {
        OccurrenceRole::Body => 0,
        OccurrenceRole::RepeatedHeader => 1,
        OccurrenceRole::RepeatedFooter => 2,
    };
    (key.0, kind, role)
}

fn globally_anchored_descriptors(
    candidates: &[ExactMatchCandidate],
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_eligible: &[bool],
    new_eligible: &[bool],
) -> Option<(HashSet<usize>, HashSet<usize>)> {
    let mut old = HashSet::new();
    let mut new = HashSet::new();
    old.try_reserve(candidates.len()).ok()?;
    new.try_reserve(candidates.len()).ok()?;
    for candidate in candidates {
        if let Some(index) = old_occurrences
            .get(candidate.old_occurrence_index)?
            .run_descriptor_index
            .filter(|index| old_eligible.get(*index).copied().unwrap_or(false))
        {
            old.insert(index);
        }
        if let Some(index) = new_occurrences
            .get(candidate.new_occurrence_index)?
            .run_descriptor_index
            .filter(|index| new_eligible.get(*index).copied().unwrap_or(false))
        {
            new.insert(index);
        }
    }
    Some((old, new))
}

fn run_unit_index<'a>(
    occurrences: &'a [SentenceOccurrence],
    eligible: &[bool],
    anchored: &HashSet<usize>,
) -> Option<RunUnitIndex<'a>> {
    let mut local = HashMap::<(usize, OccurrenceKey<'a>), RunLocalUnitState>::new();
    local.try_reserve(occurrences.len()).ok()?;
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        let (Some(descriptor_index), Some(position), Some(role)) = (
            occurrence.run_descriptor_index,
            occurrence.trusted_position,
            occurrence.role,
        ) else {
            continue;
        };
        if !eligible.get(descriptor_index).copied().unwrap_or(false)
            || anchored.contains(&descriptor_index)
            || occurrence
                .tokens
                .iter()
                .any(|token| matches!(token, SentenceEvidenceToken::Unmapped { .. }))
        {
            continue;
        }
        let key = (occurrence.key.as_str(), occurrence.kind, role.into());
        match local.entry((descriptor_index, key)) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(RunLocalUnitState::Unique(occurrence_index));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                *entry.get_mut() = RunLocalUnitState::Duplicate;
            }
        }
        let _ = position;
    }

    let mut postings = HashMap::<OccurrenceKey<'a>, Vec<RunUnitPosting>>::new();
    postings.try_reserve(local.len()).ok()?;
    let mut unique_units = 0usize;
    let mut duplicate_units = 0usize;
    for ((descriptor_index, key), state) in local {
        let RunLocalUnitState::Unique(occurrence_index) = state else {
            duplicate_units = duplicate_units.checked_add(1)?;
            continue;
        };
        let occurrence = occurrences.get(occurrence_index)?;
        let ordinal = occurrence.trusted_position?.ordinal;
        let posting = postings.entry(key).or_default();
        posting.try_reserve(1).ok()?;
        posting.push(RunUnitPosting {
            descriptor_index,
            occurrence_index,
            ordinal,
        });
        unique_units = unique_units.checked_add(1)?;
    }
    for posting in postings.values_mut() {
        posting.sort_unstable_by_key(|unit| unit.descriptor_index);
    }
    Some(RunUnitIndex {
        postings,
        unique_units,
        duplicate_units,
    })
}

fn classify_run_signature_pairs(
    metrics: &mut SentenceRecoveryMetrics,
    pairs: &mut HashMap<(usize, usize), RunPairScore>,
    min_tokens: usize,
) -> Option<()> {
    let mut old_best = HashMap::<usize, RunBest>::new();
    let mut new_best = HashMap::<usize, RunBest>::new();
    old_best.try_reserve(pairs.len()).ok()?;
    new_best.try_reserve(pairs.len()).ok()?;
    for ((old_run, new_run), pair) in pairs.iter() {
        old_best
            .entry(*old_run)
            .or_default()
            .record(*new_run, pair.score);
        new_best
            .entry(*new_run)
            .or_default()
            .record(*old_run, pair.score);
        metrics.run_signature_max_shared_units = metrics
            .run_signature_max_shared_units
            .max(pair.score.shared_units);
    }
    for ((old_run, new_run), pair) in pairs.iter_mut() {
        let Some(old_relation) = old_best.get(old_run).copied() else {
            continue;
        };
        let Some(new_relation) = new_best.get(new_run).copied() else {
            continue;
        };
        if old_relation.unique_partner() != Some(*new_run)
            || new_relation.unique_partner() != Some(*old_run)
        {
            continue;
        }
        metrics.run_signature_reciprocal_unique_pairs = metrics
            .run_signature_reciprocal_unique_pairs
            .checked_add(1)?;
        let old_margin = pair
            .score
            .shared_units
            .checked_sub(old_relation.second.shared_units)?;
        let new_margin = pair
            .score
            .shared_units
            .checked_sub(new_relation.second.shared_units)?;
        if pair.score.shared_units < 2
            || old_margin < 1
            || new_margin < 1
            || pair.score.shared_tokens < min_tokens
        {
            metrics.run_signature_margin_veto_pairs =
                metrics.run_signature_margin_veto_pairs.checked_add(1)?;
            continue;
        }
        metrics.run_signature_margin_qualified_pairs = metrics
            .run_signature_margin_qualified_pairs
            .checked_add(1)?;
        pair.ordinals.sort_unstable();
        if pair
            .ordinals
            .windows(2)
            .all(|window| window[0].0 < window[1].0 && window[0].1 < window[1].1)
        {
            metrics.run_signature_monotone_pairs =
                metrics.run_signature_monotone_pairs.checked_add(1)?;
        } else {
            metrics.run_signature_crossing_veto_pairs =
                metrics.run_signature_crossing_veto_pairs.checked_add(1)?;
        }
    }
    Some(())
}

fn record_run_signature_metrics(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    run_metrics: Option<SentenceRecoveryMetrics>,
) {
    let (Some(diagnostics), Some(run_metrics)) = (diagnostics.as_mut(), run_metrics) else {
        return;
    };
    let metrics = &mut diagnostics.metrics;
    metrics.run_signature_available = run_metrics.run_signature_available;
    metrics.run_signature_complete = run_metrics.run_signature_complete;
    metrics.old_run_signature_unique_units = run_metrics.old_run_signature_unique_units;
    metrics.new_run_signature_unique_units = run_metrics.new_run_signature_unique_units;
    metrics.old_run_signature_duplicate_units = run_metrics.old_run_signature_duplicate_units;
    metrics.new_run_signature_duplicate_units = run_metrics.new_run_signature_duplicate_units;
    metrics.run_signature_shared_unit_keys = run_metrics.run_signature_shared_unit_keys;
    metrics.run_signature_largest_posting = run_metrics.run_signature_largest_posting;
    metrics.run_signature_posting_visits_attempted =
        run_metrics.run_signature_posting_visits_attempted;
    metrics.run_signature_posting_visits_examined =
        run_metrics.run_signature_posting_visits_examined;
    metrics.run_signature_token_verifications_attempted =
        run_metrics.run_signature_token_verifications_attempted;
    metrics.run_signature_token_verifications_examined =
        run_metrics.run_signature_token_verifications_examined;
    metrics.run_signature_candidate_pairs = run_metrics.run_signature_candidate_pairs;
    metrics.run_signature_globally_anchored_runs_skipped =
        run_metrics.run_signature_globally_anchored_runs_skipped;
    metrics.run_signature_reciprocal_unique_pairs =
        run_metrics.run_signature_reciprocal_unique_pairs;
    metrics.run_signature_margin_qualified_pairs = run_metrics.run_signature_margin_qualified_pairs;
    metrics.run_signature_margin_veto_pairs = run_metrics.run_signature_margin_veto_pairs;
    metrics.run_signature_monotone_pairs = run_metrics.run_signature_monotone_pairs;
    metrics.run_signature_crossing_veto_pairs = run_metrics.run_signature_crossing_veto_pairs;
    metrics.run_signature_max_shared_units = run_metrics.run_signature_max_shared_units;
    metrics.run_signature_stop_reason = run_metrics.run_signature_stop_reason;
}

struct StreamBlock {
    side_index: usize,
    scalar_range: Range<usize>,
    scalar_to_token: Vec<usize>,
}

struct Stream {
    text: String,
    tokens: Vec<SentenceEvidenceToken>,
    scalar_to_token: Vec<usize>,
    blocks: Vec<StreamBlock>,
    forced_sentence_boundaries: Vec<ForcedSentenceBoundary>,
    atomic_line: bool,
    trusted: bool,
}

#[derive(Clone, Copy)]
struct ForcedSentenceBoundary {
    byte_offset: usize,
    scalar_offset: usize,
    include_trailing_unit: bool,
}

struct SpanMembership {
    old: HashMap<BlockId, usize>,
    new: HashMap<BlockId, usize>,
    recovery_spans: Vec<bool>,
}

#[derive(Clone, Copy)]
struct SentenceBoundary {
    byte_start: usize,
    byte_end: usize,
    scalar_start: usize,
    scalar_end: usize,
}

#[derive(Clone, Copy)]
pub(super) struct RecoveryBudget {
    token_limit: usize,
    pair_visit_limit: usize,
    reference_limits_active: bool,
    // Joined evidence may own synthetic separators. The caller's token budget
    // bounds them without counting them as source or recovered output tokens.
    evidence_token_limit: usize,
    key_byte_limit: usize,
    comparison_limit: usize,
    // Edge classification is isolated from production scoring. Filter pair
    // work is bounded by four times the token limit and production pair work
    // by the token limit; filter and production comparison work are each
    // bounded by four times the token limit.
    sentence_edge_filter_pair_limit: usize,
    sentence_edge_filter_comparison_limit: usize,
    candidate_posting_visit_limit: usize,
    output_range_limit: usize,
    occurrences: usize,
    key_bytes: usize,
    pair_visits: usize,
    comparisons: usize,
    pair_visits_attempted: usize,
    comparisons_attempted: usize,
    sentence_edge_filter_pairs: usize,
    sentence_edge_filter_pairs_attempted: usize,
    sentence_edge_filter_comparisons: usize,
    sentence_edge_filter_comparisons_attempted: usize,
    sentence_edge_filter_pairs_retained: usize,
    sentence_edge_filter_pairs_rejected: usize,
    sentence_edge_filter_active: bool,
    sentence_edge_filter_stop_reason: Option<SentenceEdgeFilterStopReason>,
    signature_exact_rechecks: usize,
    signature_exact_rechecks_attempted: usize,
    signature_exact_recheck_comparisons: usize,
    signature_exact_recheck_comparisons_attempted: usize,
    signature_exact_recheck_limit: usize,
    signature_exact_recheck_comparison_limit: usize,
    signature_exact_recheck_stop_reason: Option<SentenceEdgeSignatureDirectShadowStopReason>,
    watch_probe_pairs: usize,
    watch_probe_pairs_attempted: usize,
    watch_probe_comparisons: usize,
    watch_probe_comparisons_attempted: usize,
    watch_probe_pair_limit: usize,
    watch_probe_comparison_limit: usize,
    watch_probe_missing_signature_candidates: usize,
    watch_probe_invariant_violations: usize,
    watch_probe_stop_reason: Option<SentenceEdgeSignatureDirectShadowStopReason>,
    candidate_posting_visits: usize,
    candidate_posting_visits_attempted: usize,
    sentence_work: NearSearchWorkMetrics,
    line_work: NearSearchWorkMetrics,
    paired_interval_work: NearSearchScopeMetrics,
    paired_cross_interval_veto_work: NearSearchScopeMetrics,
    same_or_ambiguous_span_work: NearSearchScopeMetrics,
    same_known_span_work: NearSearchScopeMetrics,
    ambiguous_span_work: NearSearchScopeMetrics,
    same_or_ambiguous_shared_query_work: NearSearchScopeMetrics,
    cross_span_work: NearSearchScopeMetrics,
    largest_edge_posting: usize,
    largest_edge_query_union: usize,
    largest_filtered_candidate_set: usize,
    candidate_count_truncated: bool,
    near_relation_stop_reason: Option<NearRelationStopReason>,
    near_metrics_available: bool,
    evidence_tokens: usize,
    output_ranges: usize,
    output_tokens: usize,
    location_items: usize,
    location_bytes: usize,
    word_ranges: usize,
    enable_known_span_sentence_shadow: bool,
    enable_sentence_edge_gate_shadow: bool,
    sentence_edge_signature_filter_mode: SentenceEdgeSignatureFilterMode,
}

impl RecoveryBudget {
    fn new(
        old_tokens: usize,
        new_tokens: usize,
        max_tokens: usize,
        min_tokens: usize,
    ) -> Option<Self> {
        let token_limit = old_tokens.checked_add(new_tokens)?;
        if token_limit > max_tokens || min_tokens == 0 {
            return None;
        }
        let scaled_limit = token_limit.checked_mul(4)?;
        Some(Self {
            token_limit,
            pair_visit_limit: token_limit,
            reference_limits_active: false,
            evidence_token_limit: max_tokens,
            key_byte_limit: scaled_limit,
            comparison_limit: scaled_limit,
            sentence_edge_filter_pair_limit: scaled_limit,
            sentence_edge_filter_comparison_limit: scaled_limit,
            candidate_posting_visit_limit: scaled_limit,
            output_range_limit: (token_limit / min_tokens).min(MAX_SENTENCE_RECOVERY_RANGES),
            occurrences: 0,
            key_bytes: 0,
            pair_visits: 0,
            comparisons: 0,
            pair_visits_attempted: 0,
            comparisons_attempted: 0,
            sentence_edge_filter_pairs: 0,
            sentence_edge_filter_pairs_attempted: 0,
            sentence_edge_filter_comparisons: 0,
            sentence_edge_filter_comparisons_attempted: 0,
            sentence_edge_filter_pairs_retained: 0,
            sentence_edge_filter_pairs_rejected: 0,
            sentence_edge_filter_active: true,
            sentence_edge_filter_stop_reason: None,
            signature_exact_rechecks: 0,
            signature_exact_rechecks_attempted: 0,
            signature_exact_recheck_comparisons: 0,
            signature_exact_recheck_comparisons_attempted: 0,
            signature_exact_recheck_limit: scaled_limit,
            signature_exact_recheck_comparison_limit: scaled_limit,
            signature_exact_recheck_stop_reason: None,
            watch_probe_pairs: 0,
            watch_probe_pairs_attempted: 0,
            watch_probe_comparisons: 0,
            watch_probe_comparisons_attempted: 0,
            watch_probe_pair_limit: MAX_RECOVERY_WATCH_QUERIES,
            watch_probe_comparison_limit: scaled_limit,
            watch_probe_missing_signature_candidates: 0,
            watch_probe_invariant_violations: 0,
            watch_probe_stop_reason: None,
            candidate_posting_visits: 0,
            candidate_posting_visits_attempted: 0,
            sentence_work: NearSearchWorkMetrics::default(),
            line_work: NearSearchWorkMetrics::default(),
            paired_interval_work: NearSearchScopeMetrics::default(),
            paired_cross_interval_veto_work: NearSearchScopeMetrics::default(),
            same_or_ambiguous_span_work: NearSearchScopeMetrics::default(),
            same_known_span_work: NearSearchScopeMetrics::default(),
            ambiguous_span_work: NearSearchScopeMetrics::default(),
            same_or_ambiguous_shared_query_work: NearSearchScopeMetrics::default(),
            cross_span_work: NearSearchScopeMetrics::default(),
            largest_edge_posting: 0,
            largest_edge_query_union: 0,
            largest_filtered_candidate_set: 0,
            candidate_count_truncated: false,
            near_relation_stop_reason: None,
            near_metrics_available: true,
            evidence_tokens: 0,
            output_ranges: 0,
            output_tokens: 0,
            location_items: 0,
            location_bytes: 0,
            word_ranges: 0,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
            sentence_edge_signature_filter_mode: SentenceEdgeSignatureFilterMode::Disabled,
        })
    }

    fn apply_reference_limits(&mut self, limits: ReferenceBudgetLimits) {
        self.reference_limits_active = true;
        self.pair_visit_limit = limits.pair_visits;
        self.comparison_limit = limits.comparisons;
        self.sentence_edge_filter_pair_limit = limits.comparisons;
        self.sentence_edge_filter_comparison_limit = limits.comparisons;
        self.candidate_posting_visit_limit = limits.candidate_posting_visits;
    }

    fn charge_occurrences(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.occurrences, amount, self.token_limit)
    }

    fn charge_sentence_edge_filter_pair(&mut self) -> bool {
        if !self.charge_signature_exact_recheck(true) {
            return false;
        }
        self.charge_sentence_edge_filter_work(true, SentenceEdgeFilterStopReason::PairVisitLimit)
    }

    fn charge_watch_probe_pair(&mut self) -> bool {
        Self::charge_direct_watch_probe(
            &mut self.watch_probe_pairs,
            &mut self.watch_probe_pairs_attempted,
            self.watch_probe_pair_limit,
            &mut self.watch_probe_stop_reason,
            SentenceEdgeSignatureDirectShadowStopReason::WatchProbePairLimit,
        )
    }

    fn charge_watch_probe_comparison(&mut self) -> bool {
        Self::charge_direct_watch_probe(
            &mut self.watch_probe_comparisons,
            &mut self.watch_probe_comparisons_attempted,
            self.watch_probe_comparison_limit,
            &mut self.watch_probe_stop_reason,
            SentenceEdgeSignatureDirectShadowStopReason::WatchProbeSimilarityComparisonLimit,
        )
    }

    fn record_watch_diagnostic_failure(&mut self) {
        if self.sentence_edge_signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct {
            self.watch_probe_stop_reason
                .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
        }
    }

    fn charge_direct_watch_probe(
        examined: &mut usize,
        attempted: &mut usize,
        limit: usize,
        stop_reason: &mut Option<SentenceEdgeSignatureDirectShadowStopReason>,
        limit_reason: SentenceEdgeSignatureDirectShadowStopReason,
    ) -> bool {
        let Some(next_attempted) = attempted.checked_add(1) else {
            stop_reason.get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            return false;
        };
        *attempted = next_attempted;
        let Some(next_examined) = examined.checked_add(1) else {
            stop_reason.get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            return false;
        };
        if next_examined > limit {
            stop_reason.get_or_insert(limit_reason);
            return false;
        }
        *examined = next_examined;
        true
    }

    fn charge_sentence_edge_filter_comparison(&mut self) -> bool {
        if !self.charge_signature_exact_recheck(false) {
            return false;
        }
        self.charge_sentence_edge_filter_work(
            false,
            SentenceEdgeFilterStopReason::SimilarityComparisonLimit,
        )
    }

    fn charge_signature_exact_recheck(&mut self, pair: bool) -> bool {
        if self.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Direct {
            return true;
        }
        let (examined, attempted, limit) = if pair {
            (
                &mut self.signature_exact_rechecks,
                &mut self.signature_exact_rechecks_attempted,
                self.signature_exact_recheck_limit,
            )
        } else {
            (
                &mut self.signature_exact_recheck_comparisons,
                &mut self.signature_exact_recheck_comparisons_attempted,
                self.signature_exact_recheck_comparison_limit,
            )
        };
        let Some(next_attempted) = attempted.checked_add(1) else {
            self.signature_exact_recheck_stop_reason
                .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            self.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
            return false;
        };
        *attempted = next_attempted;
        let Some(next_examined) = examined.checked_add(1) else {
            self.signature_exact_recheck_stop_reason
                .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
            self.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
            return false;
        };
        if next_examined > limit {
            self.signature_exact_recheck_stop_reason.get_or_insert(
                SentenceEdgeSignatureDirectShadowStopReason::SignatureExactEdgeRecheckLimit,
            );
            self.disable_sentence_edge_filter(if pair {
                SentenceEdgeFilterStopReason::PairVisitLimit
            } else {
                SentenceEdgeFilterStopReason::SimilarityComparisonLimit
            });
            return false;
        }
        *examined = next_examined;
        true
    }

    fn charge_sentence_edge_filter_work(
        &mut self,
        pair: bool,
        limit_reason: SentenceEdgeFilterStopReason,
    ) -> bool {
        let (examined, attempted, limit) = if pair {
            (
                &mut self.sentence_edge_filter_pairs,
                &mut self.sentence_edge_filter_pairs_attempted,
                self.sentence_edge_filter_pair_limit,
            )
        } else {
            (
                &mut self.sentence_edge_filter_comparisons,
                &mut self.sentence_edge_filter_comparisons_attempted,
                self.sentence_edge_filter_comparison_limit,
            )
        };
        let Some(next_attempted) = attempted.checked_add(1) else {
            self.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
            return false;
        };
        *attempted = next_attempted;
        let Some(next_examined) = examined.checked_add(1) else {
            self.disable_sentence_edge_filter(SentenceEdgeFilterStopReason::CounterOverflow);
            return false;
        };
        if next_examined > limit {
            self.disable_sentence_edge_filter(limit_reason);
            return false;
        }
        *examined = next_examined;
        true
    }

    fn disable_sentence_edge_filter(&mut self, reason: SentenceEdgeFilterStopReason) {
        self.sentence_edge_filter_active = false;
        self.sentence_edge_filter_stop_reason.get_or_insert(reason);
    }

    fn charge_key_bytes(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.key_bytes, amount, self.key_byte_limit)
    }

    #[cfg(test)]
    fn charge_pair_visits(&mut self, amount: usize, kind: RecoveryUnitKind) -> bool {
        self.charge_pair_visits_in_scope(amount, kind, NearSearchScope::SameOrAmbiguousSpan)
    }

    fn charge_pair_visits_in_scope(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        scope: NearSearchScope,
    ) -> bool {
        self.charge_pair_visits_in_scope_split(
            amount,
            kind,
            scope,
            NearSearchWorkSplit::shared(amount),
        )
    }

    fn charge_pair_visits_in_scope_split(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        scope: NearSearchScope,
        split: NearSearchWorkSplit,
    ) -> bool {
        let examined = self.pair_visits;
        let attempted = self.pair_visits_attempted;
        let charged = Self::charge_near_search(
            &mut self.pair_visits,
            &mut self.pair_visits_attempted,
            amount,
            if self.reference_limits_active {
                self.pair_visit_limit
            } else {
                self.token_limit
            },
            NearRelationStopReason::PairVisitLimit,
            &mut self.near_relation_stop_reason,
            &mut self.near_metrics_available,
        );
        let examined =
            Self::diagnostic_delta(self.pair_visits, examined, &mut self.near_metrics_available);
        let attempted = Self::diagnostic_delta(
            self.pair_visits_attempted,
            attempted,
            &mut self.near_metrics_available,
        );
        let kind_recorded = Self::record_pair_work(self.work_mut(kind), examined, attempted);
        let scope_recorded =
            Self::record_pair_work(self.scope_kind_work_mut(scope, kind), examined, attempted);
        let subdivision_recorded = self.record_same_or_ambiguous_split(
            scope,
            kind,
            amount,
            split,
            charged,
            Self::record_pair_work,
        );
        if !kind_recorded || !scope_recorded || !subdivision_recorded {
            self.near_metrics_available = false;
        }
        charged
    }

    #[cfg(test)]
    fn charge_candidate_posting_visits(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        posting_kind: CandidatePostingKind,
    ) -> bool {
        self.charge_candidate_posting_visits_in_scope(
            amount,
            kind,
            posting_kind,
            NearSearchScope::SameOrAmbiguousSpan,
        )
    }

    #[cfg(test)]
    fn charge_candidate_posting_visits_in_scope(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        posting_kind: CandidatePostingKind,
        scope: NearSearchScope,
    ) -> bool {
        self.charge_candidate_posting_visits_in_scope_split(
            amount,
            kind,
            posting_kind,
            scope,
            NearSearchWorkSplit::shared(amount),
        )
    }

    pub(super) fn charge_candidate_posting_visits_in_scope_split(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        posting_kind: CandidatePostingKind,
        scope: NearSearchScope,
        split: NearSearchWorkSplit,
    ) -> bool {
        let examined = self.candidate_posting_visits;
        let attempted = self.candidate_posting_visits_attempted;
        let charged = Self::charge_near_search(
            &mut self.candidate_posting_visits,
            &mut self.candidate_posting_visits_attempted,
            amount,
            self.candidate_posting_visit_limit,
            NearRelationStopReason::CandidatePostingVisitLimit,
            &mut self.near_relation_stop_reason,
            &mut self.near_metrics_available,
        );
        let examined = Self::diagnostic_delta(
            self.candidate_posting_visits,
            examined,
            &mut self.near_metrics_available,
        );
        let attempted = Self::diagnostic_delta(
            self.candidate_posting_visits_attempted,
            attempted,
            &mut self.near_metrics_available,
        );
        let kind_recorded =
            Self::record_posting_work(self.work_mut(kind), posting_kind, examined, attempted);
        let scope_recorded = Self::record_posting_work(
            self.scope_kind_work_mut(scope, kind),
            posting_kind,
            examined,
            attempted,
        );
        let subdivision_recorded = self.record_same_or_ambiguous_split(
            scope,
            kind,
            amount,
            split,
            charged,
            |work, examined, attempted| {
                Self::record_posting_work(work, posting_kind, examined, attempted)
            },
        );
        if !kind_recorded || !scope_recorded || !subdivision_recorded {
            self.near_metrics_available = false;
        }
        charged
    }

    #[cfg(test)]
    fn charge_comparisons(&mut self, amount: usize, kind: RecoveryUnitKind) -> bool {
        self.charge_comparisons_in_scope(amount, kind, NearSearchScope::SameOrAmbiguousSpan)
    }

    #[cfg(test)]
    fn charge_comparisons_in_scope(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        scope: NearSearchScope,
    ) -> bool {
        self.charge_comparisons_in_scope_split(
            amount,
            kind,
            scope,
            NearSearchWorkSplit::shared(amount),
        )
    }

    pub(super) fn charge_comparisons_in_scope_split(
        &mut self,
        amount: usize,
        kind: RecoveryUnitKind,
        scope: NearSearchScope,
        split: NearSearchWorkSplit,
    ) -> bool {
        let examined = self.comparisons;
        let attempted = self.comparisons_attempted;
        let charged = Self::charge_near_search(
            &mut self.comparisons,
            &mut self.comparisons_attempted,
            amount,
            self.comparison_limit,
            NearRelationStopReason::SimilarityComparisonLimit,
            &mut self.near_relation_stop_reason,
            &mut self.near_metrics_available,
        );
        let examined =
            Self::diagnostic_delta(self.comparisons, examined, &mut self.near_metrics_available);
        let attempted = Self::diagnostic_delta(
            self.comparisons_attempted,
            attempted,
            &mut self.near_metrics_available,
        );
        let kind_recorded = Self::record_comparison_work(self.work_mut(kind), examined, attempted);
        let scope_recorded = Self::record_comparison_work(
            self.scope_kind_work_mut(scope, kind),
            examined,
            attempted,
        );
        let subdivision_recorded = self.record_same_or_ambiguous_split(
            scope,
            kind,
            amount,
            split,
            charged,
            Self::record_comparison_work,
        );
        if !kind_recorded || !scope_recorded || !subdivision_recorded {
            self.near_metrics_available = false;
        }
        charged
    }

    #[cfg(test)]
    fn record_candidate_query(
        &mut self,
        query: UnitCandidateQueryMetrics,
        kind: RecoveryUnitKind,
        filtered_candidate_count: usize,
    ) {
        self.record_candidate_query_in_scope(
            query,
            kind,
            filtered_candidate_count,
            NearSearchScope::SameOrAmbiguousSpan,
        );
    }

    fn record_candidate_query_in_scope(
        &mut self,
        query: UnitCandidateQueryMetrics,
        kind: RecoveryUnitKind,
        filtered_candidate_count: usize,
        scope: NearSearchScope,
    ) {
        self.largest_edge_posting = self.largest_edge_posting.max(query.largest_edge_posting);
        self.largest_edge_query_union = self.largest_edge_query_union.max(query.edge_query_union);
        self.largest_filtered_candidate_set = self
            .largest_filtered_candidate_set
            .max(filtered_candidate_count);
        let kind_recorded = Self::record_query_work(self.work_mut(kind), query);
        let scope_recorded = Self::record_query_work(self.scope_kind_work_mut(scope, kind), query);
        let subdivision_recorded = if scope == NearSearchScope::SameOrAmbiguousSpan {
            let Some(classified_edge) = query
                .same_known_edge_query_union
                .checked_add(query.ambiguous_edge_query_union)
            else {
                self.near_metrics_available = false;
                return;
            };
            let Some(shared_edge) = query.edge_query_union.checked_sub(classified_edge) else {
                self.near_metrics_available = false;
                return;
            };
            let Some(classified_trigram) = query
                .same_known_line_trigram_only_query_union
                .checked_add(query.ambiguous_line_trigram_only_query_union)
            else {
                self.near_metrics_available = false;
                return;
            };
            let Some(shared_trigram) = query
                .line_trigram_only_query_union
                .checked_sub(classified_trigram)
            else {
                self.near_metrics_available = false;
                return;
            };
            let same = UnitCandidateQueryMetrics {
                edge_query_union: query.same_known_edge_query_union,
                line_trigram_only_query_union: query.same_known_line_trigram_only_query_union,
                ..UnitCandidateQueryMetrics::default()
            };
            let ambiguous = UnitCandidateQueryMetrics {
                edge_query_union: query.ambiguous_edge_query_union,
                line_trigram_only_query_union: query.ambiguous_line_trigram_only_query_union,
                ..UnitCandidateQueryMetrics::default()
            };
            let shared = UnitCandidateQueryMetrics {
                edge_query_union: shared_edge,
                line_trigram_only_query_union: shared_trigram,
                ..UnitCandidateQueryMetrics::default()
            };
            Self::record_query_work(
                self.subdivision_kind_work_mut(kind, NearSearchWorkClass::SameKnown),
                same,
            ) && Self::record_query_work(
                self.subdivision_kind_work_mut(kind, NearSearchWorkClass::Ambiguous),
                ambiguous,
            ) && Self::record_query_work(
                self.subdivision_kind_work_mut(kind, NearSearchWorkClass::Shared),
                shared,
            )
        } else {
            true
        };
        if !kind_recorded || !scope_recorded || !subdivision_recorded {
            self.near_metrics_available = false;
        }
    }

    fn record_same_or_ambiguous_split(
        &mut self,
        scope: NearSearchScope,
        kind: RecoveryUnitKind,
        amount: usize,
        split: NearSearchWorkSplit,
        charged: bool,
        mut record: impl FnMut(&mut NearSearchWorkMetrics, usize, usize) -> bool,
    ) -> bool {
        if scope != NearSearchScope::SameOrAmbiguousSpan {
            return true;
        }
        let Some(total) = split.total().filter(|total| *total == amount) else {
            return false;
        };
        if total == 0 {
            return true;
        }
        for (class, amount) in [
            (NearSearchWorkClass::SameKnown, split.same_known),
            (NearSearchWorkClass::Ambiguous, split.ambiguous),
            (NearSearchWorkClass::Shared, split.shared),
        ] {
            if amount == 0 {
                continue;
            }
            let class_examined = if charged { amount } else { 0 };
            if !record(
                self.subdivision_kind_work_mut(kind, class),
                class_examined,
                amount,
            ) {
                return false;
            }
        }
        true
    }

    fn record_pair_work(
        work: &mut NearSearchWorkMetrics,
        examined: usize,
        attempted: usize,
    ) -> bool {
        let Some(next) = work
            .pair_visits_examined
            .checked_add(examined)
            .zip(work.pair_visits_attempted.checked_add(attempted))
            .zip(work.filtered_candidates.checked_add(attempted))
        else {
            return false;
        };
        work.pair_visits_examined = next.0.0;
        work.pair_visits_attempted = next.0.1;
        work.filtered_candidates = next.1;
        true
    }

    fn record_posting_work(
        work: &mut NearSearchWorkMetrics,
        posting_kind: CandidatePostingKind,
        examined: usize,
        attempted: usize,
    ) -> bool {
        let counters = match posting_kind {
            CandidatePostingKind::Edge => (
                &mut work.edge_posting_visits_examined,
                &mut work.edge_posting_visits_attempted,
            ),
            CandidatePostingKind::LineTrigram => (
                &mut work.line_trigram_posting_visits_examined,
                &mut work.line_trigram_posting_visits_attempted,
            ),
        };
        let Some((next_examined, next_attempted)) = counters
            .0
            .checked_add(examined)
            .zip(counters.1.checked_add(attempted))
        else {
            return false;
        };
        *counters.0 = next_examined;
        *counters.1 = next_attempted;
        true
    }

    fn record_comparison_work(
        work: &mut NearSearchWorkMetrics,
        examined: usize,
        attempted: usize,
    ) -> bool {
        let Some((next_examined, next_attempted)) = work
            .similarity_comparisons_examined
            .checked_add(examined)
            .zip(work.similarity_comparisons_attempted.checked_add(attempted))
        else {
            return false;
        };
        work.similarity_comparisons_examined = next_examined;
        work.similarity_comparisons_attempted = next_attempted;
        true
    }

    fn record_query_work(
        work: &mut NearSearchWorkMetrics,
        query: UnitCandidateQueryMetrics,
    ) -> bool {
        let Some((edge, trigram)) = work
            .edge_query_union_candidates
            .checked_add(query.edge_query_union)
            .zip(
                work.line_trigram_only_query_union_candidates
                    .checked_add(query.line_trigram_only_query_union),
            )
        else {
            return false;
        };
        work.edge_query_union_candidates = edge;
        work.line_trigram_only_query_union_candidates = trigram;
        true
    }

    fn work_mut(&mut self, kind: RecoveryUnitKind) -> &mut NearSearchWorkMetrics {
        match kind {
            RecoveryUnitKind::Sentence => &mut self.sentence_work,
            RecoveryUnitKind::Line => &mut self.line_work,
        }
    }

    fn scope_kind_work_mut(
        &mut self,
        scope: NearSearchScope,
        kind: RecoveryUnitKind,
    ) -> &mut NearSearchWorkMetrics {
        let work = match scope {
            NearSearchScope::PairedInterval => &mut self.paired_interval_work,
            NearSearchScope::PairedCrossIntervalVeto => &mut self.paired_cross_interval_veto_work,
            NearSearchScope::SameOrAmbiguousSpan => &mut self.same_or_ambiguous_span_work,
            NearSearchScope::CrossSpan => &mut self.cross_span_work,
        };
        match kind {
            RecoveryUnitKind::Sentence => &mut work.sentence_work,
            RecoveryUnitKind::Line => &mut work.line_work,
        }
    }

    fn subdivision_kind_work_mut(
        &mut self,
        kind: RecoveryUnitKind,
        class: NearSearchWorkClass,
    ) -> &mut NearSearchWorkMetrics {
        let work = match class {
            NearSearchWorkClass::SameKnown => &mut self.same_known_span_work,
            NearSearchWorkClass::Ambiguous => &mut self.ambiguous_span_work,
            NearSearchWorkClass::Shared => &mut self.same_or_ambiguous_shared_query_work,
        };
        match kind {
            RecoveryUnitKind::Sentence => &mut work.sentence_work,
            RecoveryUnitKind::Line => &mut work.line_work,
        }
    }

    fn diagnostic_delta(after: usize, before: usize, metrics_available: &mut bool) -> usize {
        let Some(delta) = after.checked_sub(before) else {
            *metrics_available = false;
            return 0;
        };
        delta
    }

    fn record_candidate_count_limit(&mut self) {
        self.candidate_count_truncated = true;
        self.near_relation_stop_reason
            .get_or_insert(NearRelationStopReason::CandidateCountLimit);
    }

    fn commit_near_search_spend_from(&mut self, other: Self) {
        self.candidate_posting_visits = other.candidate_posting_visits;
        self.candidate_posting_visits_attempted = other.candidate_posting_visits_attempted;
        self.sentence_work = other.sentence_work;
        self.line_work = other.line_work;
        self.paired_interval_work = other.paired_interval_work;
        self.paired_cross_interval_veto_work = other.paired_cross_interval_veto_work;
        self.same_or_ambiguous_span_work = other.same_or_ambiguous_span_work;
        self.same_known_span_work = other.same_known_span_work;
        self.ambiguous_span_work = other.ambiguous_span_work;
        self.same_or_ambiguous_shared_query_work = other.same_or_ambiguous_shared_query_work;
        self.cross_span_work = other.cross_span_work;
        self.pair_visits = other.pair_visits;
        self.comparisons = other.comparisons;
        self.pair_visits_attempted = other.pair_visits_attempted;
        self.comparisons_attempted = other.comparisons_attempted;
        self.sentence_edge_filter_pairs = other.sentence_edge_filter_pairs;
        self.sentence_edge_filter_pairs_attempted = other.sentence_edge_filter_pairs_attempted;
        self.sentence_edge_filter_comparisons = other.sentence_edge_filter_comparisons;
        self.sentence_edge_filter_comparisons_attempted =
            other.sentence_edge_filter_comparisons_attempted;
        self.sentence_edge_filter_pairs_retained = other.sentence_edge_filter_pairs_retained;
        self.sentence_edge_filter_pairs_rejected = other.sentence_edge_filter_pairs_rejected;
        self.sentence_edge_filter_active = other.sentence_edge_filter_active;
        self.sentence_edge_filter_stop_reason = other.sentence_edge_filter_stop_reason;
        self.signature_exact_rechecks = other.signature_exact_rechecks;
        self.signature_exact_rechecks_attempted = other.signature_exact_rechecks_attempted;
        self.signature_exact_recheck_comparisons = other.signature_exact_recheck_comparisons;
        self.signature_exact_recheck_comparisons_attempted =
            other.signature_exact_recheck_comparisons_attempted;
        if self.signature_exact_recheck_stop_reason.is_none() {
            self.signature_exact_recheck_stop_reason = other.signature_exact_recheck_stop_reason;
        }
        self.watch_probe_pairs = other.watch_probe_pairs;
        self.watch_probe_pairs_attempted = other.watch_probe_pairs_attempted;
        self.watch_probe_comparisons = other.watch_probe_comparisons;
        self.watch_probe_comparisons_attempted = other.watch_probe_comparisons_attempted;
        self.watch_probe_missing_signature_candidates =
            other.watch_probe_missing_signature_candidates;
        self.watch_probe_invariant_violations = other.watch_probe_invariant_violations;
        if self.watch_probe_stop_reason.is_none() {
            self.watch_probe_stop_reason = other.watch_probe_stop_reason;
        }
        self.largest_edge_posting = other.largest_edge_posting;
        self.largest_edge_query_union = other.largest_edge_query_union;
        self.largest_filtered_candidate_set = other.largest_filtered_candidate_set;
        self.candidate_count_truncated = other.candidate_count_truncated;
        self.near_relation_stop_reason = other.near_relation_stop_reason;
        self.near_metrics_available = other.near_metrics_available;
    }

    fn charge_evidence_tokens(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.evidence_tokens, amount, self.evidence_token_limit)
    }

    fn charge_word_ranges(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.word_ranges, amount, self.token_limit)
    }

    #[cfg(test)]
    fn charge_output(&mut self, token_count: usize) -> bool {
        self.charge_outputs(1, token_count)
    }

    fn charge_outputs(&mut self, range_count: usize, token_count: usize) -> bool {
        let Some(output_ranges) = self.output_ranges.checked_add(range_count) else {
            return false;
        };
        let Some(output_tokens) = self.output_tokens.checked_add(token_count) else {
            return false;
        };
        if output_ranges > self.output_range_limit || output_tokens > self.token_limit {
            return false;
        }
        self.output_ranges = output_ranges;
        self.output_tokens = output_tokens;
        true
    }

    fn charge_location_metadata(&mut self, block_count: usize, consumed_count: usize) -> bool {
        let Some(additional_items) = block_count.checked_add(consumed_count) else {
            return false;
        };
        let Some(additional_bytes) = block_count
            .checked_mul(std::mem::size_of::<BlockId>())
            .and_then(|bytes| {
                consumed_count
                    .checked_mul(std::mem::size_of::<LocalSentenceRange>())
                    .and_then(|consumed_bytes| bytes.checked_add(consumed_bytes))
            })
            .and_then(|bytes| bytes.checked_mul(2))
        else {
            return false;
        };
        let Some(items) = self.location_items.checked_add(additional_items) else {
            return false;
        };
        let Some(bytes) = self.location_bytes.checked_add(additional_bytes) else {
            return false;
        };
        if items > MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS || bytes > MAX_SENTENCE_RECOVERY_OUTPUT_BYTES
        {
            return false;
        }
        self.location_items = items;
        self.location_bytes = bytes;
        true
    }

    fn charge(current: &mut usize, amount: usize, limit: usize) -> bool {
        let Some(next) = current.checked_add(amount) else {
            return false;
        };
        if next > limit {
            return false;
        }
        *current = next;
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn charge_near_search(
        examined: &mut usize,
        attempted: &mut usize,
        amount: usize,
        limit: usize,
        limit_reason: NearRelationStopReason,
        stop_reason: &mut Option<NearRelationStopReason>,
        metrics_available: &mut bool,
    ) -> bool {
        let Some(next_attempted) = attempted.checked_add(amount) else {
            *metrics_available = false;
            return false;
        };
        *attempted = next_attempted;
        let Some(next_examined) = examined.checked_add(amount) else {
            *metrics_available = false;
            return false;
        };
        if next_examined > limit {
            if stop_reason.is_none()
                || *stop_reason == Some(NearRelationStopReason::CandidateCountLimit)
            {
                *stop_reason = Some(limit_reason);
            }
            return false;
        }
        *examined = next_examined;
        true
    }
}

#[cfg(test)]
const REFERENCE_PAIR_WORK_CAP: usize = 8_000_000;
#[cfg(test)]
const REFERENCE_COMPARISON_WORK_CAP: usize = 32_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReferenceBudgetLimits {
    pair_visits: usize,
    comparisons: usize,
    candidate_posting_visits: usize,
}

impl ReferenceBudgetLimits {
    #[cfg(test)]
    fn derive(old_tokens: usize, new_tokens: usize) -> Option<Self> {
        let tokens = old_tokens.checked_add(new_tokens)?;
        Some(Self {
            pair_visits: tokens.checked_mul(8)?.min(REFERENCE_PAIR_WORK_CAP),
            comparisons: tokens.checked_mul(32)?.min(REFERENCE_COMPARISON_WORK_CAP),
            candidate_posting_visits: tokens.checked_mul(32)?.min(REFERENCE_COMPARISON_WORK_CAP),
        })
    }
}

#[derive(Clone, Copy)]
struct FragmentVetoBudget {
    pair_visit_limit: usize,
    comparison_limit: usize,
    pair_visits: usize,
    pair_visits_attempted: usize,
    comparisons: usize,
    comparisons_attempted: usize,
    stop_reason: Option<FragmentVetoStopReason>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(in crate::diff) struct FragmentVetoTestLimits {
    pub pair_visits: usize,
    pub comparisons: usize,
}

#[cfg(test)]
thread_local! {
    static NEXT_FRAGMENT_VETO_TEST_LIMITS: std::cell::Cell<Option<FragmentVetoTestLimits>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(in crate::diff) fn with_next_fragment_veto_test_limits<T>(
    limits: FragmentVetoTestLimits,
    operation: impl FnOnce() -> T,
) -> T {
    struct ResetFragmentVetoTestLimits;

    impl Drop for ResetFragmentVetoTestLimits {
        fn drop(&mut self) {
            NEXT_FRAGMENT_VETO_TEST_LIMITS.with(|slot| slot.set(None));
        }
    }

    NEXT_FRAGMENT_VETO_TEST_LIMITS.with(|slot| {
        assert!(
            slot.get().is_none(),
            "fragment-veto test limits are already installed"
        );
        slot.set(Some(limits));
    });
    let _reset = ResetFragmentVetoTestLimits;
    let output = operation();
    assert!(
        NEXT_FRAGMENT_VETO_TEST_LIMITS.with(|slot| slot.get().is_none()),
        "fragment-veto test limits were not consumed"
    );
    output
}

impl FragmentVetoBudget {
    fn new(old_tokens: usize, new_tokens: usize) -> Option<Self> {
        let token_limit = old_tokens.checked_add(new_tokens)?;
        let comparison_limit = token_limit.checked_mul(4)?;
        // Fragment veto analysis is isolated so it cannot consume recovery's
        // remaining pair/comparison budget. Each limit is no larger than the
        // corresponding RecoveryBudget limit, so running both analyses can at
        // most double their combined pair/comparison work.
        let budget = Self {
            pair_visit_limit: token_limit,
            comparison_limit,
            pair_visits: 0,
            pair_visits_attempted: 0,
            comparisons: 0,
            comparisons_attempted: 0,
            stop_reason: None,
        };
        #[cfg(test)]
        {
            let mut budget = budget;
            NEXT_FRAGMENT_VETO_TEST_LIMITS.with(|slot| {
                if let Some(limits) = slot.take() {
                    budget.pair_visit_limit = limits.pair_visits;
                    budget.comparison_limit = limits.comparisons;
                }
            });
            Some(budget)
        }
        #[cfg(not(test))]
        {
            Some(budget)
        }
    }

    fn charge_pair_visits(&mut self, amount: usize) -> bool {
        Self::charge(
            &mut self.pair_visits,
            &mut self.pair_visits_attempted,
            amount,
            self.pair_visit_limit,
            &mut self.stop_reason,
            FragmentVetoStopReason::PairVisitLimit,
        )
    }

    fn charge_comparisons(&mut self, amount: usize) -> bool {
        Self::charge(
            &mut self.comparisons,
            &mut self.comparisons_attempted,
            amount,
            self.comparison_limit,
            &mut self.stop_reason,
            FragmentVetoStopReason::SimilarityComparisonLimit,
        )
    }

    fn apply_reference_limits(&mut self, limits: ReferenceBudgetLimits) {
        self.pair_visit_limit = limits.pair_visits;
        self.comparison_limit = limits.comparisons;
    }

    fn stop_reason(self) -> FragmentVetoStopReason {
        self.stop_reason
            .unwrap_or(FragmentVetoStopReason::InvalidEvidence)
    }

    fn record_allocation_failure(&mut self) {
        self.stop_reason
            .get_or_insert(FragmentVetoStopReason::AllocationFailure);
    }

    fn charge(
        examined: &mut usize,
        attempted: &mut usize,
        amount: usize,
        limit: usize,
        stop_reason: &mut Option<FragmentVetoStopReason>,
        limit_reason: FragmentVetoStopReason,
    ) -> bool {
        let Some(next_attempted) = attempted.checked_add(amount) else {
            stop_reason.get_or_insert(FragmentVetoStopReason::CounterOverflow);
            return false;
        };
        *attempted = next_attempted;
        let Some(next_examined) = examined.checked_add(amount) else {
            stop_reason.get_or_insert(FragmentVetoStopReason::CounterOverflow);
            return false;
        };
        if next_examined > limit {
            stop_reason.get_or_insert(limit_reason);
            return false;
        }
        *examined = next_examined;
        true
    }
}

pub(super) fn build_sentence_recovery_plan(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
    input: SentenceRecoveryInput<'_>,
    max_tokens: usize,
    watch_queries: &[RecoveryWatchQuery<'_>],
) -> Result<SentenceRecoveryBuildOutcome> {
    let mut accepted = build_sentence_recovery_plan_with_atomic_fallback(
        (
            SentenceEdgeFilterMode::Filtered,
            SentenceEdgeSignatureFilterMode::Direct,
        ),
        (
            SentenceEdgeFilterMode::Legacy,
            SentenceEdgeSignatureFilterMode::Disabled,
        ),
        |edge_filter_mode, signature_filter_mode| {
            build_sentence_recovery_plan_inner(
                old,
                new,
                alignment,
                input,
                max_tokens,
                watch_queries,
                edge_filter_mode,
                signature_filter_mode,
                None,
            )
        },
    )?;
    if input.enable_sentence_edge_gate_shadow {
        let replay_input = SentenceRecoveryInput {
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
            ..input
        };
        let replay = build_sentence_recovery_plan_inner(
            old,
            new,
            alignment,
            replay_input,
            max_tokens,
            &[],
            SentenceEdgeFilterMode::Filtered,
            SentenceEdgeSignatureFilterMode::PostUnion,
            None,
        );
        match replay {
            Ok(replay) => record_sentence_edge_signature_replay(&mut accepted, replay),
            Err(_) => record_sentence_edge_signature_replay_failure(&mut accepted),
        }
    }
    Ok(accepted)
}

#[cfg(test)]
fn record_sentence_edge_signature_direct_replay_failure(
    accepted: &mut SentenceRecoveryBuildOutcome,
) {
    if let Some(diagnostics) = accepted.diagnostics.as_mut() {
        diagnostics.metrics.sentence_edge_signature_direct_shadow =
            Some(SentenceEdgeSignatureDirectShadowMetrics {
                complete: false,
                stop_reason: Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure),
                ..SentenceEdgeSignatureDirectShadowMetrics::default()
            });
        diagnostics.metrics.sentence_edge_signature_direct_execution =
            Some(SentenceEdgeSignatureDirectExecution::ShadowReplay);
    }
}

#[cfg(test)]
fn accepted_recovery_is_complete(accepted: &SentenceRecoveryBuildOutcome) -> bool {
    accepted.fragment_veto_complete
        && accepted.diagnostics.as_ref().is_some_and(|diagnostics| {
            diagnostics.metrics.near_relation_complete
                && diagnostics.metrics.sentence_edge_filter_complete
        })
}

#[cfg(test)]
fn record_reference_oracle_direct_incomplete(accepted: &mut SentenceRecoveryBuildOutcome) {
    if accepted_recovery_is_complete(accepted) {
        return;
    }
    if let Some(diagnostics) = accepted.diagnostics.as_mut() {
        diagnostics.metrics.sentence_edge_signature_reference_oracle =
            Some(SentenceEdgeSignatureReferenceOracleMetrics {
                stop_reason: Some(
                    SentenceEdgeSignatureReferenceOracleStopReason::DirectReplayIncomplete,
                ),
                ..SentenceEdgeSignatureReferenceOracleMetrics::default()
            });
    }
}

#[cfg(test)]
fn attach_reference_oracle_metrics(
    accepted: &mut SentenceRecoveryBuildOutcome,
    metrics: SentenceEdgeSignatureReferenceOracleMetrics,
) {
    if let Some(diagnostics) = accepted.diagnostics.as_mut() {
        diagnostics.metrics.sentence_edge_signature_reference_oracle = Some(metrics);
    }
}

fn direct_signature_stop_reason(
    reason: SentenceEdgeSignatureShadowStopReason,
) -> SentenceEdgeSignatureDirectShadowStopReason {
    match reason {
        SentenceEdgeSignatureShadowStopReason::IndexPostingLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexPostingLimit
        }
        SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryPostingVisitLimit
        }
        SentenceEdgeSignatureShadowStopReason::CandidatePostingVisitLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::CandidatePostingVisitLimit
        }
        SentenceEdgeSignatureShadowStopReason::PairVisitLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::PairVisitLimit
        }
        SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::SimilarityComparisonLimit
        }
        SentenceEdgeSignatureShadowStopReason::CandidateCountLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::CandidateCountLimit
        }
        SentenceEdgeSignatureShadowStopReason::AllocationFailure => {
            SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure
        }
        SentenceEdgeSignatureShadowStopReason::CounterOverflow => {
            SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow
        }
        SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete => {
            SentenceEdgeSignatureDirectShadowStopReason::ProductionTraversalIncomplete
        }
        SentenceEdgeSignatureShadowStopReason::DiagnosticFailure => {
            SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure
        }
    }
}

fn direct_edge_stop_reason(
    reason: SentenceEdgeFilterStopReason,
) -> SentenceEdgeSignatureDirectShadowStopReason {
    match reason {
        SentenceEdgeFilterStopReason::PairVisitLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::DirectEdgePairVisitLimit
        }
        SentenceEdgeFilterStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::DirectEdgeSimilarityComparisonLimit
        }
        SentenceEdgeFilterStopReason::AllocationFailure => {
            SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure
        }
        SentenceEdgeFilterStopReason::CounterOverflow => {
            SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow
        }
    }
}

fn reference_edge_filter_shadow_stop_reason(
    reason: SentenceEdgeFilterStopReason,
) -> SentenceEdgeSignatureShadowStopReason {
    match reason {
        SentenceEdgeFilterStopReason::PairVisitLimit => {
            SentenceEdgeSignatureShadowStopReason::PairVisitLimit
        }
        SentenceEdgeFilterStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit
        }
        SentenceEdgeFilterStopReason::AllocationFailure => {
            SentenceEdgeSignatureShadowStopReason::AllocationFailure
        }
        SentenceEdgeFilterStopReason::CounterOverflow => {
            SentenceEdgeSignatureShadowStopReason::CounterOverflow
        }
    }
}

fn select_direct_signature_stop_reason(
    signature: SentenceEdgeSignatureShadowMetrics,
    recovery: SentenceRecoveryMetrics,
) -> Option<SentenceEdgeSignatureDirectShadowStopReason> {
    let signature_primary = signature.stop_reason.and_then(|reason| {
        matches!(
            reason,
            SentenceEdgeSignatureShadowStopReason::IndexPostingLimit
                | SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit
                | SentenceEdgeSignatureShadowStopReason::AllocationFailure
                | SentenceEdgeSignatureShadowStopReason::CounterOverflow
                | SentenceEdgeSignatureShadowStopReason::DiagnosticFailure
        )
        .then(|| direct_signature_stop_reason(reason))
    });
    signature_primary
        .or_else(|| {
            recovery
                .sentence_edge_filter_stop_reason
                .map(direct_edge_stop_reason)
        })
        .or_else(|| {
            recovery
                .near_relation_stop_reason
                .map(|reason| match reason {
                    NearRelationStopReason::CandidatePostingVisitLimit => {
                        SentenceEdgeSignatureDirectShadowStopReason::CandidatePostingVisitLimit
                    }
                    NearRelationStopReason::PairVisitLimit => {
                        SentenceEdgeSignatureDirectShadowStopReason::PairVisitLimit
                    }
                    NearRelationStopReason::SimilarityComparisonLimit => {
                        SentenceEdgeSignatureDirectShadowStopReason::SimilarityComparisonLimit
                    }
                    NearRelationStopReason::CandidateCountLimit => {
                        SentenceEdgeSignatureDirectShadowStopReason::CandidateCountLimit
                    }
                })
        })
        .or_else(|| signature.stop_reason.map(direct_signature_stop_reason))
}

#[cfg(test)]
fn reference_near_stop_reason(
    reason: NearRelationStopReason,
) -> SentenceEdgeSignatureReferenceOracleStopReason {
    match reason {
        NearRelationStopReason::CandidatePostingVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::CandidatePostingVisitLimit
        }
        NearRelationStopReason::PairVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::PairVisitLimit
        }
        NearRelationStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::SimilarityComparisonLimit
        }
        NearRelationStopReason::CandidateCountLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::CandidateCountLimit
        }
    }
}

#[cfg(test)]
fn reference_fragment_stop_reason(
    reason: FragmentVetoStopReason,
) -> SentenceEdgeSignatureReferenceOracleStopReason {
    match reason {
        FragmentVetoStopReason::PairVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::FragmentVetoPairVisitLimit
        }
        FragmentVetoStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::FragmentVetoSimilarityComparisonLimit
        }
        FragmentVetoStopReason::AllocationFailure => {
            SentenceEdgeSignatureReferenceOracleStopReason::AllocationFailure
        }
        FragmentVetoStopReason::CounterOverflow => {
            SentenceEdgeSignatureReferenceOracleStopReason::CounterOverflow
        }
        FragmentVetoStopReason::InvalidEvidence => {
            SentenceEdgeSignatureReferenceOracleStopReason::FragmentVetoIncomplete
        }
    }
}

fn direct_fragment_stop_reason(
    reason: FragmentVetoStopReason,
) -> SentenceEdgeSignatureDirectShadowStopReason {
    match reason {
        FragmentVetoStopReason::PairVisitLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoPairVisitLimit
        }
        FragmentVetoStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoSimilarityComparisonLimit
        }
        FragmentVetoStopReason::AllocationFailure => {
            SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure
        }
        FragmentVetoStopReason::CounterOverflow => {
            SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow
        }
        FragmentVetoStopReason::InvalidEvidence => {
            SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoIncomplete
        }
    }
}

#[cfg(test)]
fn evaluate_reference_oracle(
    direct: &SentenceRecoveryBuildOutcome,
    reference: &SentenceRecoveryBuildOutcome,
) -> SentenceEdgeSignatureReferenceOracleMetrics {
    let Some(reference_diagnostics) = reference.diagnostics.as_ref() else {
        return SentenceEdgeSignatureReferenceOracleMetrics {
            stop_reason: Some(SentenceEdgeSignatureReferenceOracleStopReason::DiagnosticFailure),
            direct_complete: true,
            ..SentenceEdgeSignatureReferenceOracleMetrics::default()
        };
    };
    let recovery = reference_diagnostics.metrics;
    let reference_edges = reference_diagnostics.signature_retained_fingerprint;
    let mut metrics = SentenceEdgeSignatureReferenceOracleMetrics {
        direct_complete: true,
        legacy_sentence_edge_pairs_examined: reference_edges.legacy_sentence_edge_pairs_examined,
        legacy_sentence_edge_pairs_attempted: reference_edges.legacy_sentence_edge_pairs_attempted,
        legacy_sentence_edge_pairs_retained: reference_edges.legacy_sentence_edge_pairs_retained,
        legacy_sentence_edge_pairs_rejected: reference_edges.legacy_sentence_edge_pairs_rejected,
        candidate_posting_visits_examined: recovery.near_candidate_posting_visits_examined,
        candidate_posting_visits_attempted: recovery.near_candidate_posting_visits_attempted,
        edge_filter_pairs_examined: recovery.sentence_edge_filter_pairs_examined,
        edge_filter_pairs_attempted: recovery.sentence_edge_filter_pairs_attempted,
        edge_filter_similarity_comparisons_examined: recovery
            .sentence_edge_filter_similarity_comparisons_examined,
        edge_filter_similarity_comparisons_attempted: recovery
            .sentence_edge_filter_similarity_comparisons_attempted,
        pair_visits_examined: recovery.near_pair_visits_examined,
        pair_visits_attempted: recovery.near_pair_visits_attempted,
        similarity_comparisons_examined: recovery.near_similarity_comparisons_examined,
        similarity_comparisons_attempted: recovery.near_similarity_comparisons_attempted,
        fragment_veto_pair_visits_examined: reference.fragment_veto_pair_visits_examined,
        fragment_veto_pair_visits_attempted: reference.fragment_veto_pair_visits_attempted,
        fragment_veto_similarity_comparisons_examined: reference.fragment_veto_comparisons_examined,
        fragment_veto_similarity_comparisons_attempted: reference
            .fragment_veto_comparisons_attempted,
        candidate_count_truncated: recovery.near_candidate_count_truncated,
        ..SentenceEdgeSignatureReferenceOracleMetrics::default()
    };
    metrics.stop_reason = recovery
        .sentence_edge_filter_stop_reason
        .map(reference_edge_stop_reason)
        .or_else(|| {
            recovery
                .near_relation_stop_reason
                .map(reference_near_stop_reason)
        });
    if metrics.stop_reason.is_none()
        && let Some(reason) = recovery
            .sentence_edge_signature_shadow
            .and_then(|shadow| shadow.stop_reason)
    {
        metrics.stop_reason = Some(reference_observer_stop_reason(reason));
    }
    metrics.complete = reference.plan.is_some()
        && recovery.sentence_edge_filter_complete
        && recovery.near_relation_complete
        && !recovery.near_candidate_count_truncated
        && metrics.legacy_sentence_edge_pairs_examined
            == metrics.legacy_sentence_edge_pairs_attempted
        && metrics
            .legacy_sentence_edge_pairs_retained
            .checked_add(metrics.legacy_sentence_edge_pairs_rejected)
            == Some(metrics.legacy_sentence_edge_pairs_examined)
        && reference.fragment_veto_complete
        && reference.fragment_veto_pair_visits_examined
            == reference.fragment_veto_pair_visits_attempted
        && reference.fragment_veto_comparisons_examined
            == reference.fragment_veto_comparisons_attempted
        && reference_diagnostics.signature_retained_fingerprint_valid
        && metrics.stop_reason.is_none();
    if !metrics.complete {
        metrics
            .stop_reason
            .get_or_insert(if recovery.near_candidate_count_truncated {
                SentenceEdgeSignatureReferenceOracleStopReason::CandidateCountLimit
            } else if let Some(reason) = reference.fragment_veto_stop_reason {
                reference_fragment_stop_reason(reason)
            } else {
                SentenceEdgeSignatureReferenceOracleStopReason::ProductionTraversalIncomplete
            });
        return metrics;
    }
    let (Some(direct_plan), Some(reference_plan), Some(direct_diagnostics)) = (
        direct.plan.as_ref(),
        reference.plan.as_ref(),
        direct.diagnostics.as_ref(),
    ) else {
        metrics.complete = false;
        metrics.stop_reason =
            Some(SentenceEdgeSignatureReferenceOracleStopReason::DiagnosticFailure);
        return metrics;
    };
    metrics.plan_parity_evaluable = true;
    metrics.plan_parity = direct_plan == reference_plan;
    metrics.fingerprint_evaluable = true;
    let direct_fingerprint = direct_diagnostics.signature_retained_fingerprint;
    let reference_fingerprint = reference_diagnostics.signature_retained_fingerprint;
    metrics.retained_pair_misses = reference_fingerprint
        .count
        .saturating_sub(direct_fingerprint.count);
    metrics.retained_pair_count_mismatches =
        usize::from(reference_fingerprint.count != direct_fingerprint.count);
    metrics.retained_pair_set_mismatches =
        usize::from(!reference_fingerprint.same_set(direct_fingerprint));
    metrics.retained_pair_order_mismatches =
        usize::from(!reference_fingerprint.same_order(direct_fingerprint));
    metrics
}

#[cfg(test)]
fn reference_observer_stop_reason(
    reason: SentenceEdgeSignatureShadowStopReason,
) -> SentenceEdgeSignatureReferenceOracleStopReason {
    match reason {
        SentenceEdgeSignatureShadowStopReason::CandidatePostingVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::CandidatePostingVisitLimit
        }
        SentenceEdgeSignatureShadowStopReason::PairVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::PairVisitLimit
        }
        SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::SimilarityComparisonLimit
        }
        SentenceEdgeSignatureShadowStopReason::CandidateCountLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::CandidateCountLimit
        }
        SentenceEdgeSignatureShadowStopReason::AllocationFailure => {
            SentenceEdgeSignatureReferenceOracleStopReason::AllocationFailure
        }
        SentenceEdgeSignatureShadowStopReason::CounterOverflow => {
            SentenceEdgeSignatureReferenceOracleStopReason::CounterOverflow
        }
        SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete => {
            SentenceEdgeSignatureReferenceOracleStopReason::ProductionTraversalIncomplete
        }
        SentenceEdgeSignatureShadowStopReason::DiagnosticFailure
        | SentenceEdgeSignatureShadowStopReason::IndexPostingLimit
        | SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::DiagnosticFailure
        }
    }
}

#[cfg(test)]
fn reference_edge_stop_reason(
    reason: SentenceEdgeFilterStopReason,
) -> SentenceEdgeSignatureReferenceOracleStopReason {
    match reason {
        SentenceEdgeFilterStopReason::PairVisitLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::EdgeFilterPairVisitLimit
        }
        SentenceEdgeFilterStopReason::SimilarityComparisonLimit => {
            SentenceEdgeSignatureReferenceOracleStopReason::EdgeFilterSimilarityComparisonLimit
        }
        SentenceEdgeFilterStopReason::AllocationFailure => {
            SentenceEdgeSignatureReferenceOracleStopReason::AllocationFailure
        }
        SentenceEdgeFilterStopReason::CounterOverflow => {
            SentenceEdgeSignatureReferenceOracleStopReason::CounterOverflow
        }
    }
}

fn direct_scope_candidate_total(
    metrics: &SentenceEdgeSignatureDirectShadowMetrics,
) -> Option<usize> {
    metrics
        .paired_interval_candidates
        .checked_add(metrics.paired_cross_interval_candidates)
        .and_then(|total| total.checked_add(metrics.same_known_candidates))
        .and_then(|total| total.checked_add(metrics.ambiguous_candidates))
        .and_then(|total| total.checked_add(metrics.cross_span_candidates))
}

fn direct_signature_metrics_are_consistent(
    metrics: &SentenceEdgeSignatureDirectShadowMetrics,
) -> bool {
    let posting_items = metrics
        .signature_index_own_posting_items
        .checked_add(metrics.signature_index_all_posting_items);
    let distinct_keys = metrics
        .signature_index_own_distinct_keys
        .checked_add(metrics.signature_index_all_distinct_keys);
    let depth_postings = metrics
        .signature_index_depth_1_posting_items
        .checked_add(metrics.signature_index_depth_2_to_3_posting_items)
        .and_then(|total| total.checked_add(metrics.signature_index_depth_4_plus_posting_items));
    let queries = metrics
        .signature_depth_1_queries
        .checked_add(metrics.signature_depth_2_to_3_queries)
        .and_then(|total| total.checked_add(metrics.signature_depth_4_plus_queries));
    let unions = metrics
        .signature_depth_1_candidate_union
        .checked_add(metrics.signature_depth_2_to_3_candidate_union)
        .and_then(|total| total.checked_add(metrics.signature_depth_4_plus_candidate_union));
    let rechecks = metrics
        .exact_edge_retained_pairs
        .checked_add(metrics.exact_edge_rejected_pairs);
    posting_items == Some(metrics.signature_index_items_examined)
        && posting_items == Some(metrics.signature_index_items_attempted)
        && metrics.signature_query_visits_examined == metrics.signature_query_visits_attempted
        && depth_postings == Some(metrics.signature_index_items_examined)
        && distinct_keys == Some(metrics.signature_index_distinct_keys_examined)
        && metrics.signature_index_distinct_keys_attempted
            == metrics.signature_index_distinct_keys_examined
        && metrics.signature_index_estimated_logical_bytes_examined
            == metrics.signature_index_estimated_logical_bytes
        && metrics.signature_index_estimated_logical_bytes_attempted
            == metrics.signature_index_estimated_logical_bytes
        && metrics.signature_index_posting_capacity_items >= metrics.signature_index_items_examined
        && queries == Some(metrics.signature_queries)
        && metrics.signature_queries_attempted == metrics.signature_queries
        && unions == Some(metrics.direct_candidates)
        && metrics.signature_candidate_union_attempted == metrics.direct_candidates
        && rechecks == Some(metrics.exact_edge_rechecks)
        && metrics.exact_edge_rechecks_attempted == metrics.exact_edge_rechecks
        && metrics.exact_edge_recheck_comparisons_examined
            == metrics.exact_edge_recheck_comparisons_attempted
        && metrics.edge_filter_pairs_examined == metrics.edge_filter_pairs_attempted
        && metrics.edge_filter_comparisons_examined == metrics.edge_filter_comparisons_attempted
        && metrics.downstream_candidate_postings_examined
            == metrics.downstream_candidate_postings_attempted
        && metrics.downstream_pair_visits_examined == metrics.downstream_pair_visits_attempted
        && metrics.downstream_similarity_comparisons_examined
            == metrics.downstream_similarity_comparisons_attempted
        && metrics.cross_orientation_only_candidates <= metrics.exact_edge_rejected_pairs
        && metrics.watch_probe_pairs_examined == metrics.watch_probe_pairs_attempted
        && metrics.watch_probe_similarity_comparisons_examined
            == metrics.watch_probe_similarity_comparisons_attempted
        && metrics.watch_probe_missing_signature_candidates == metrics.watch_probe_pairs_examined
        && metrics.watch_probe_invariant_violations == 0
}

#[cfg(test)]
fn compare_direct_watch_diagnostics(
    metrics: &mut SentenceEdgeSignatureDirectShadowMetrics,
    accepted: Option<&RecoveryWatchDiagnostics>,
    replay: Option<&RecoveryWatchDiagnostics>,
    direct_internally_complete: bool,
    accepted_recovery_complete: bool,
) {
    if !direct_internally_complete {
        return;
    }
    metrics.watch_preservation_evaluable = true;
    if accepted_recovery_complete {
        metrics.watch_exact_parity_evaluable = true;
        metrics.watch_exact_parity = accepted == replay;
        metrics.watch_evidence_preserved = metrics.watch_exact_parity;
    } else {
        metrics.watch_evidence_preserved = match (accepted, replay) {
            (None, None) => true,
            (Some(accepted), Some(replay)) => watch_fixed_evidence_is_preserved(accepted, replay),
            _ => false,
        };
    }
    metrics.watch_preservation_mismatches = usize::from(!metrics.watch_evidence_preserved);
    if !metrics.watch_evidence_preserved {
        metrics.complete = false;
        metrics
            .stop_reason
            .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::WatchDiagnosticsMismatch);
    }
}

#[cfg(test)]
fn watch_fixed_evidence_is_preserved(
    accepted: &RecoveryWatchDiagnostics,
    direct: &RecoveryWatchDiagnostics,
) -> bool {
    accepted.candidate_generation_complete == direct.candidate_generation_complete
        && direct.near_relation_complete
        && direct.near_relation_stop_reason.is_none()
        && accepted.segment_candidates == direct.segment_candidates
        && accepted.segment_hash_matches == direct.segment_hash_matches
        && accepted.segment_token_verified_matches == direct.segment_token_verified_matches
        && accepted.segment_unique_pairs == direct.segment_unique_pairs
        && accepted.segment_duplicate_pairs == direct.segment_duplicate_pairs
        && accepted.segment_monotone_pairs == direct.segment_monotone_pairs
        && accepted.segment_crossing_pairs == direct.segment_crossing_pairs
        && accepted.segment_stop_reason == direct.segment_stop_reason
        && accepted.granular_complete == direct.granular_complete
        && accepted.granular_old_units == direct.granular_old_units
        && accepted.granular_new_units == direct.granular_new_units
        && accepted.granular_pair_comparisons == direct.granular_pair_comparisons
        && accepted.granular_stop_reason == direct.granular_stop_reason
        && accepted.records.len() == direct.records.len()
        && accepted
            .records
            .iter()
            .zip(&direct.records)
            .all(|(accepted, direct)| watch_record_fixed_evidence_is_preserved(accepted, direct))
}

#[cfg(test)]
fn watch_record_fixed_evidence_is_preserved(
    accepted: &RecoveryWatchRecord,
    direct: &RecoveryWatchRecord,
) -> bool {
    accepted.id == direct.id
        && accepted.old == direct.old
        && accepted.new == direct.new
        && watch_pair_fixed_evidence_is_preserved(accepted.pair.as_ref(), direct.pair.as_ref())
        && watch_segment_fixed_evidence_is_preserved(
            accepted.segment_pair.as_ref(),
            direct.segment_pair.as_ref(),
        )
        && accepted.granular_pair == direct.granular_pair
}

#[cfg(test)]
fn watch_pair_fixed_evidence_is_preserved(
    accepted: Option<&RecoveryWatchPairEvidence>,
    direct: Option<&RecoveryWatchPairEvidence>,
) -> bool {
    match (accepted, direct) {
        (None, None) => true,
        (Some(accepted), Some(direct)) => {
            accepted.same_span == direct.same_span
                && accepted.exact_shared_units == direct.exact_shared_units
                && accepted.exact_shared_units_available == direct.exact_shared_units_available
                && (!accepted.near_candidate_examined
                    || (direct.near_candidate_examined && accepted.near_score == direct.near_score))
        }
        _ => false,
    }
}

#[cfg(test)]
fn watch_segment_fixed_evidence_is_preserved(
    accepted: Option<&RecoveryWatchSegmentPairEvidence>,
    direct: Option<&RecoveryWatchSegmentPairEvidence>,
) -> bool {
    match (accepted, direct) {
        (None, None) => true,
        (Some(accepted), Some(direct)) => {
            accepted.old_start_ordinal == direct.old_start_ordinal
                && accepted.old_end_ordinal == direct.old_end_ordinal
                && accepted.new_start_ordinal == direct.new_start_ordinal
                && accepted.new_end_ordinal == direct.new_end_ordinal
                && accepted.old_unit_count == direct.old_unit_count
                && accepted.new_unit_count == direct.new_unit_count
                && accepted.old_token_count == direct.old_token_count
                && accepted.new_token_count == direct.new_token_count
                && accepted.exact == direct.exact
                && accepted.old_occurrence_count == direct.old_occurrence_count
                && accepted.new_occurrence_count == direct.new_occurrence_count
                && accepted.role_compatible == direct.role_compatible
                && accepted.crossing_anchor_count == direct.crossing_anchor_count
                && accepted.relation == direct.relation
        }
        _ => false,
    }
}

#[cfg(test)]
fn compare_direct_retained_fingerprints(
    metrics: &mut SentenceEdgeSignatureDirectShadowMetrics,
    baseline: SentenceEdgeRetainedFingerprint,
    direct: SentenceEdgeRetainedFingerprint,
) {
    metrics.retained_pair_count_mismatches = usize::from(baseline.count != direct.count);
    metrics.retained_pair_set_mismatches = usize::from(!baseline.same_set(direct));
    metrics.retained_pair_order_mismatches = usize::from(!baseline.same_order(direct));
    metrics.verification_evaluable = true;
    metrics.retained_pair_misses = baseline.count.saturating_sub(direct.count);
}

fn finalized_direct_metrics(
    outcome: &SentenceRecoveryBuildOutcome,
) -> Option<SentenceEdgeSignatureDirectShadowMetrics> {
    let diagnostics = outcome.diagnostics.as_ref()?;
    let recovery = diagnostics.metrics;
    let signature = recovery.sentence_edge_signature_shadow.unwrap_or_else(|| {
        SentenceEdgeSignatureShadowMetrics {
            complete: false,
            stop_reason: Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete),
            ..SentenceEdgeSignatureShadowMetrics::default()
        }
    });
    let mut metrics = recovery
        .sentence_edge_signature_direct_shadow
        .unwrap_or_default();
    metrics.paired_interval_candidates = signature.paired_interval_pairs;
    metrics.paired_cross_interval_candidates = signature.paired_cross_interval_pairs;
    metrics.same_known_candidates = signature.same_known_pairs;
    metrics.ambiguous_candidates = signature.ambiguous_pairs;
    metrics.cross_span_candidates = signature.cross_span_shared_pairs;
    metrics.edge_filter_pairs_examined = recovery.sentence_edge_filter_pairs_examined;
    metrics.edge_filter_pairs_attempted = recovery.sentence_edge_filter_pairs_attempted;
    metrics.edge_filter_comparisons_examined =
        recovery.sentence_edge_filter_similarity_comparisons_examined;
    metrics.edge_filter_comparisons_attempted =
        recovery.sentence_edge_filter_similarity_comparisons_attempted;
    metrics.sentence_broad_edge_postings_examined =
        recovery.near_sentence_work.edge_posting_visits_examined;
    metrics.sentence_broad_edge_postings_attempted =
        recovery.near_sentence_work.edge_posting_visits_attempted;
    metrics.downstream_candidate_postings_examined =
        recovery.near_candidate_posting_visits_examined;
    metrics.downstream_candidate_postings_attempted =
        recovery.near_candidate_posting_visits_attempted;
    metrics.downstream_pair_visits_examined = recovery.near_pair_visits_examined;
    metrics.downstream_pair_visits_attempted = recovery.near_pair_visits_attempted;
    metrics.downstream_similarity_comparisons_examined =
        recovery.near_similarity_comparisons_examined;
    metrics.downstream_similarity_comparisons_attempted =
        recovery.near_similarity_comparisons_attempted;
    metrics.fragment_veto_pair_visits_examined = outcome.fragment_veto_pair_visits_examined;
    metrics.fragment_veto_pair_visits_attempted = outcome.fragment_veto_pair_visits_attempted;
    metrics.fragment_veto_similarity_comparisons_examined =
        outcome.fragment_veto_comparisons_examined;
    metrics.fragment_veto_similarity_comparisons_attempted =
        outcome.fragment_veto_comparisons_attempted;
    metrics.candidate_count_truncated = recovery.near_candidate_count_truncated;
    metrics.parity_evaluable = false;
    metrics.plan_parity = false;
    metrics.verification_evaluable = false;
    metrics.retained_pair_misses = 0;
    metrics.retained_pair_count_mismatches = 0;
    metrics.retained_pair_set_mismatches = 0;
    metrics.retained_pair_order_mismatches = 0;
    metrics.watch_preservation_evaluable = false;
    metrics.watch_evidence_preserved = false;
    metrics.watch_preservation_mismatches = 0;
    metrics.watch_exact_parity_evaluable = false;
    metrics.watch_exact_parity = false;
    metrics.stop_reason = metrics
        .stop_reason
        .or_else(|| select_direct_signature_stop_reason(signature, recovery));
    if !outcome.fragment_veto_complete {
        metrics.stop_reason.get_or_insert_with(|| {
            direct_fragment_stop_reason(
                outcome
                    .fragment_veto_stop_reason
                    .unwrap_or(FragmentVetoStopReason::InvalidEvidence),
            )
        });
    }
    if outcome.fragment_veto_complete
        && (outcome.fragment_veto_pair_visits_examined
            != outcome.fragment_veto_pair_visits_attempted
            || outcome.fragment_veto_comparisons_examined
                != outcome.fragment_veto_comparisons_attempted)
    {
        metrics
            .stop_reason
            .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
    }
    if metrics.stop_reason.is_none()
        && (direct_scope_candidate_total(&metrics) != Some(metrics.direct_candidates)
            || !direct_signature_metrics_are_consistent(&metrics))
    {
        metrics.stop_reason = Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
    }
    let no_recovery_work = metrics.direct_candidates == 0
        && signature.pairs_considered == 0
        && recovery.near_pair_candidates == 0;
    metrics.complete = signature.complete
        && metrics.stop_reason.is_none()
        && recovery.sentence_edge_filter_complete
        && recovery.near_relation_complete
        && !recovery.near_candidate_count_truncated
        && outcome.fragment_veto_complete
        && (outcome.plan.is_some() || no_recovery_work);
    if !metrics.complete && metrics.stop_reason.is_none() {
        metrics.stop_reason = Some(if recovery.near_candidate_count_truncated {
            SentenceEdgeSignatureDirectShadowStopReason::CandidateCountLimit
        } else {
            SentenceEdgeSignatureDirectShadowStopReason::ProductionTraversalIncomplete
        });
    }
    Some(metrics)
}

#[cfg(test)]
fn record_sentence_edge_signature_direct_replay(
    accepted: &mut SentenceRecoveryBuildOutcome,
    replay: &SentenceRecoveryBuildOutcome,
) {
    let Some(replay_diagnostics) = replay.diagnostics.as_ref() else {
        record_sentence_edge_signature_direct_replay_failure(accepted);
        return;
    };
    let Some(mut metrics) = finalized_direct_metrics(replay) else {
        record_sentence_edge_signature_direct_replay_failure(accepted);
        return;
    };
    let direct_internally_complete = metrics.complete;
    let accepted_complete = accepted_recovery_is_complete(accepted);
    metrics.parity_evaluable = metrics.complete && accepted_complete;
    metrics.plan_parity = metrics.parity_evaluable
        && sentence_recovery_plan_parity(accepted.plan.as_ref(), replay.plan.as_ref());
    let accepted_fingerprint = accepted
        .diagnostics
        .as_ref()
        .filter(|diagnostics| diagnostics.signature_retained_fingerprint_valid)
        .map(|diagnostics| diagnostics.signature_retained_fingerprint);
    let direct_fingerprint = replay_diagnostics.signature_retained_fingerprint;
    if metrics.parity_evaluable
        && let Some(accepted_fingerprint) = accepted_fingerprint
    {
        compare_direct_retained_fingerprints(
            &mut metrics,
            accepted_fingerprint,
            direct_fingerprint,
        );
    }
    let decision_mismatch = metrics.parity_evaluable && !metrics.plan_parity;
    let fingerprint_mismatch = metrics.verification_evaluable
        && (metrics.retained_pair_count_mismatches != 0
            || metrics.retained_pair_set_mismatches != 0
            || metrics.retained_pair_order_mismatches != 0);
    if decision_mismatch || fingerprint_mismatch {
        metrics.complete = false;
        metrics.stop_reason = Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
    }
    compare_direct_watch_diagnostics(
        &mut metrics,
        accepted.watch_diagnostics.as_ref(),
        replay.watch_diagnostics.as_ref(),
        direct_internally_complete,
        accepted_complete,
    );
    if metrics.complete
        && (!metrics.watch_preservation_evaluable
            || !metrics.watch_evidence_preserved
            || (accepted_complete
                && (!metrics.watch_exact_parity_evaluable || !metrics.watch_exact_parity)))
    {
        metrics.complete = false;
        metrics
            .stop_reason
            .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
    }
    if let Some(diagnostics) = accepted.diagnostics.as_mut() {
        diagnostics.metrics.sentence_edge_signature_direct_shadow = Some(metrics);
        diagnostics.metrics.sentence_edge_signature_direct_execution =
            Some(SentenceEdgeSignatureDirectExecution::ShadowReplay);
    }
}

fn record_sentence_edge_signature_replay_failure(accepted: &mut SentenceRecoveryBuildOutcome) {
    if let Some(diagnostics) = accepted.diagnostics.as_mut() {
        diagnostics.metrics.sentence_edge_signature_shadow =
            Some(SentenceEdgeSignatureShadowMetrics {
                complete: false,
                stop_reason: Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure),
                ..SentenceEdgeSignatureShadowMetrics::default()
            });
    }
}

fn record_sentence_edge_signature_replay(
    accepted: &mut SentenceRecoveryBuildOutcome,
    replay: SentenceRecoveryBuildOutcome,
) {
    let accepted_filter = accepted.diagnostics.as_ref().map(|diagnostics| {
        (
            diagnostics.metrics.sentence_edge_filter_complete,
            diagnostics.metrics.sentence_edge_filter_pairs_retained,
        )
    });
    let replay_filter_complete = replay
        .diagnostics
        .as_ref()
        .is_some_and(|diagnostics| diagnostics.metrics.sentence_edge_filter_complete);
    let near_relations_complete = accepted
        .diagnostics
        .as_ref()
        .is_some_and(|diagnostics| diagnostics.metrics.near_relation_complete)
        && replay
            .diagnostics
            .as_ref()
            .is_some_and(|diagnostics| diagnostics.metrics.near_relation_complete);
    let mut metrics = replay
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.metrics.sentence_edge_signature_shadow)
        .unwrap_or_else(|| SentenceEdgeSignatureShadowMetrics {
            complete: false,
            stop_reason: Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete),
            ..SentenceEdgeSignatureShadowMetrics::default()
        });
    let parity_evaluable = metrics.complete
        && metrics.stop_reason.is_none()
        && near_relations_complete
        && replay_filter_complete
        && accepted_filter.is_some_and(|(complete, _)| complete);
    let parity = parity_evaluable
        && sentence_recovery_plan_parity(accepted.plan.as_ref(), replay.plan.as_ref());
    metrics.parity_evaluable = parity_evaluable;
    metrics.plan_parity = parity;
    if metrics.complete
        && parity_evaluable
        && replay_filter_complete
        && let Some((true, accepted_retained)) = accepted_filter
    {
        if let Some(misses) = accepted_retained.checked_sub(metrics.exact_edge_retained_pairs) {
            metrics.verification_evaluable = true;
            metrics.retained_pair_misses = misses;
        } else {
            metrics.complete = false;
            metrics.stop_reason = Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure);
        }
    }
    if let Some(diagnostics) = accepted.diagnostics.as_mut() {
        diagnostics.metrics.sentence_edge_signature_shadow = Some(metrics);
        diagnostics.signature_retained_fingerprint = replay
            .diagnostics
            .as_ref()
            .map_or_else(SentenceEdgeRetainedFingerprint::default, |diagnostics| {
                diagnostics.signature_retained_fingerprint
            });
        diagnostics.signature_retained_fingerprint_valid = metrics.complete
            && metrics.stop_reason.is_none()
            && metrics.verification_evaluable
            && metrics.retained_pair_misses == 0;
    }
}

fn sentence_recovery_plan_parity(
    left: Option<&SentenceRecoveryPlan>,
    right: Option<&SentenceRecoveryPlan>,
) -> bool {
    left == right
}

fn build_sentence_recovery_plan_with_atomic_fallback(
    first_modes: (SentenceEdgeFilterMode, SentenceEdgeSignatureFilterMode),
    retry_modes: (SentenceEdgeFilterMode, SentenceEdgeSignatureFilterMode),
    mut build: impl FnMut(
        SentenceEdgeFilterMode,
        SentenceEdgeSignatureFilterMode,
    ) -> Result<SentenceRecoveryBuildOutcome>,
) -> Result<SentenceRecoveryBuildOutcome> {
    let mut filtered = build(first_modes.0, first_modes.1)?;
    let direct_metrics = (first_modes.1 == SentenceEdgeSignatureFilterMode::Direct).then(|| {
        finalized_direct_metrics(&filtered).unwrap_or_else(|| {
            SentenceEdgeSignatureDirectShadowMetrics {
                stop_reason: Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure),
                ..SentenceEdgeSignatureDirectShadowMetrics::default()
            }
        })
    });
    let first_complete = match direct_metrics {
        Some(metrics) => metrics.complete,
        None => filtered
            .diagnostics
            .as_ref()
            .is_some_and(|diagnostics| diagnostics.metrics.near_relation_complete),
    };
    if first_complete {
        if let (Some(metrics), Some(diagnostics)) = (direct_metrics, filtered.diagnostics.as_mut())
        {
            diagnostics.metrics.sentence_edge_signature_direct_shadow = Some(metrics);
            diagnostics.metrics.sentence_edge_signature_direct_execution =
                Some(SentenceEdgeSignatureDirectExecution::ProductionAccepted);
        }
        return Ok(filtered);
    }

    let filter_attempt = filtered
        .diagnostics
        .as_ref()
        .map(|diagnostics| SentenceEdgeFilterAttemptMetrics::from_metrics(&diagnostics.metrics));
    drop(filtered);
    // Each build retains the existing per-document limits. The fallback can
    // therefore perform at most two independently bounded recovery builds,
    // and the discarded build is dropped before allocating the retry.
    let mut legacy = build(retry_modes.0, retry_modes.1)?;
    match (filter_attempt, legacy.diagnostics.as_mut()) {
        (Some(filter_attempt), Some(diagnostics)) => {
            filter_attempt.apply(&mut diagnostics.metrics);
        }
        (None, Some(diagnostics)) if direct_metrics.is_some() => {
            // A Direct setup failure can precede the detailed filter
            // checkpoint. Its typed Direct stop still proves that this
            // accepted plan came from a full legacy retry.
            diagnostics
                .metrics
                .sentence_edge_filter_full_build_fallback_used = true;
        }
        (None, Some(_)) => {
            // Without the first attempt's counters, publishing the disabled
            // legacy build as a completed filter run would be misleading.
            legacy.diagnostics = None;
        }
        _ => {}
    }
    if let (Some(metrics), Some(diagnostics)) = (direct_metrics, legacy.diagnostics.as_mut()) {
        diagnostics.metrics.sentence_edge_signature_direct_shadow = Some(metrics);
        diagnostics.metrics.sentence_edge_signature_direct_execution =
            Some(SentenceEdgeSignatureDirectExecution::ProductionDiscarded);
    }
    Ok(legacy)
}

#[allow(clippy::too_many_arguments)]
fn build_sentence_recovery_plan_inner(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
    input: SentenceRecoveryInput<'_>,
    max_tokens: usize,
    watch_queries: &[RecoveryWatchQuery<'_>],
    edge_filter_mode: SentenceEdgeFilterMode,
    signature_filter_mode: SentenceEdgeSignatureFilterMode,
    reference_limits: Option<ReferenceBudgetLimits>,
) -> Result<SentenceRecoveryBuildOutcome> {
    let mut signature_checkpoint = None;
    let outcome = build_sentence_recovery_plan_inner_impl(
        old,
        new,
        alignment,
        input,
        max_tokens,
        watch_queries,
        edge_filter_mode,
        signature_filter_mode,
        reference_limits,
        &mut signature_checkpoint,
    );
    finalize_signature_replay_outcome(outcome, signature_filter_mode, signature_checkpoint)
}

fn finalize_signature_replay_outcome(
    outcome: Result<SentenceRecoveryBuildOutcome>,
    signature_filter_mode: SentenceEdgeSignatureFilterMode,
    signature_checkpoint: Option<SentenceRecoveryDiagnostics>,
) -> Result<SentenceRecoveryBuildOutcome> {
    match outcome {
        Ok(mut outcome) => {
            if signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled
                && outcome.diagnostics.is_none()
            {
                outcome.diagnostics = signature_checkpoint;
            }
            Ok(outcome)
        }
        Err(_)
            if signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled
                && signature_checkpoint.is_some() =>
        {
            Ok(SentenceRecoveryBuildOutcome {
                diagnostics: signature_checkpoint,
                ..SentenceRecoveryBuildOutcome::default()
            })
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_sentence_recovery_plan_inner_impl(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
    input: SentenceRecoveryInput<'_>,
    max_tokens: usize,
    watch_queries: &[RecoveryWatchQuery<'_>],
    edge_filter_mode: SentenceEdgeFilterMode,
    signature_filter_mode: SentenceEdgeSignatureFilterMode,
    reference_limits: Option<ReferenceBudgetLimits>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
) -> Result<SentenceRecoveryBuildOutcome> {
    let Some(structural_evidence) = RecoveryStructuralEvidence::new(input) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(mut budget) = RecoveryBudget::new(
        old.total_tokens,
        new.total_tokens,
        max_tokens,
        input.min_tokens,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(mut fragment_veto_budget) =
        FragmentVetoBudget::new(old.total_tokens, new.total_tokens)
    else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    if let Some(reference_limits) = reference_limits {
        budget.apply_reference_limits(reference_limits);
        fragment_veto_budget.apply_reference_limits(reference_limits);
    }
    budget.enable_known_span_sentence_shadow = input.enable_known_span_sentence_shadow;
    budget.enable_sentence_edge_gate_shadow = input.enable_sentence_edge_gate_shadow;
    budget.sentence_edge_signature_filter_mode = signature_filter_mode;
    budget.sentence_edge_filter_active = edge_filter_mode != SentenceEdgeFilterMode::Legacy;
    let Some(membership) = span_membership(alignment) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let mut diagnostics = sentence_recovery_diagnostics(
        old,
        new,
        alignment,
        input.old_trusted_run_intervals,
        input.new_trusted_run_intervals,
        &membership.recovery_spans,
    );
    let structural_pairing = structural_pairing_plan(
        &structural_evidence,
        input.old_trusted_run_intervals,
        input.new_trusted_run_intervals,
    );
    let run_signature_eligibility = structural_pairing.as_ref().and_then(|structural| {
        Some((
            descriptor_recovery_eligibility(
                &structural.old,
                structural_evidence.old.as_ref()?,
                old,
                &membership.old,
                &membership.recovery_spans,
            )?,
            descriptor_recovery_eligibility(
                &structural.new,
                structural_evidence.new.as_ref()?,
                new,
                &membership.new,
                &membership.recovery_spans,
            )?,
        ))
    });
    record_structural_pairing_plan(&mut diagnostics, structural_pairing.as_ref());
    if !membership.recovery_spans.iter().any(|eligible| *eligible) {
        if input.enable_sentence_edge_gate_shadow
            && let Some(diagnostics) = diagnostics.as_mut()
        {
            diagnostics.metrics.sentence_edge_gate_shadow = Some(SentenceEdgeGateShadowMetrics {
                complete: true,
                ..SentenceEdgeGateShadowMetrics::default()
            });
        }
        if signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled
            && let Some(diagnostics) = diagnostics.as_mut()
        {
            diagnostics.metrics.sentence_edge_signature_shadow =
                Some(SentenceEdgeSignatureShadowMetrics {
                    complete: true,
                    ..SentenceEdgeSignatureShadowMetrics::default()
                });
        }
        return Ok(SentenceRecoveryBuildOutcome {
            plan: None,
            diagnostics,
            watch_diagnostics: None,
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        });
    }

    let Some((mut old_occurrences, old_fragments)) = collect_occurrences(
        old,
        input.old_trusted_run_intervals,
        structural_evidence.old.as_ref(),
        &membership.old,
        &membership.recovery_spans,
        &mut budget,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some((mut new_occurrences, new_fragments)) = collect_occurrences(
        new,
        input.new_trusted_run_intervals,
        structural_evidence.new.as_ref(),
        &membership.new,
        &membership.recovery_spans,
        &mut budget,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    if validate_occurrence_evidence(&old_occurrences, structural_evidence.old.as_ref()).is_none()
        || validate_occurrence_evidence(&new_occurrences, structural_evidence.new.as_ref())
            .is_none()
    {
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    let Some(counts) = occurrence_counts(&old_occurrences, &new_occurrences) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(mut exact_match_candidates) = exact_match_candidates(
        &old_occurrences,
        &new_occurrences,
        &counts,
        &membership.recovery_spans,
        input.min_tokens,
        budget.output_range_limit / 2,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(paired_streams) =
        paired_trusted_streams(&old_occurrences, &new_occurrences, &exact_match_candidates)
    else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let initial_exact_candidate_count = exact_match_candidates.len();
    if let Err(reason) = extend_paired_stream_exact_matches(
        &old_occurrences,
        &new_occurrences,
        &mut exact_match_candidates,
        &paired_streams,
        &membership.recovery_spans,
        input.min_tokens.min(MIN_PAIRED_STREAM_EXACT_TOKENS),
        budget.output_range_limit / 2,
    ) {
        mark_sentence_edge_gate_shadow_incomplete_with_reason(
            &mut diagnostics,
            input.enable_sentence_edge_gate_shadow,
            reason,
        );
        // Paired-stream matching is optional enrichment. Reaching its bounded
        // candidate cap must not discard the globally unique exact matches
        // that were already established above.
        exact_match_candidates.truncate(initial_exact_candidate_count);
    }
    let Some(paired_streams) =
        paired_trusted_streams(&old_occurrences, &new_occurrences, &exact_match_candidates)
    else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    classify_structural_pair_anchors(
        &mut diagnostics,
        structural_pairing.as_ref(),
        &old_occurrences,
        &new_occurrences,
        &exact_match_candidates,
    );
    let mut watch = RecoveryWatchState::new(
        watch_queries,
        &old_occurrences,
        &new_occurrences,
        &exact_match_candidates[..initial_exact_candidate_count],
        RecoveryWatchBuildContext {
            old_evidence: structural_evidence.old.as_ref(),
            new_evidence: structural_evidence.new.as_ref(),
            old_fully_contained: run_signature_eligibility
                .as_ref()
                .map(|(old, _)| old.as_slice()),
            new_fully_contained: run_signature_eligibility
                .as_ref()
                .map(|(_, new)| new.as_slice()),
            min_tokens: input.min_tokens,
            max_tokens,
        },
    );
    if !watch_queries.is_empty() && watch.is_none() {
        budget.record_watch_diagnostic_failure();
    }
    if watch.as_mut().is_some_and(|watch| {
        watch
            .record_exact_candidates(&exact_match_candidates, &old_occurrences, &new_occurrences)
            .is_none()
    }) {
        budget.record_watch_diagnostic_failure();
        watch = None;
    }
    let run_metrics =
        run_signature_eligibility
            .as_ref()
            .and_then(|(old_eligible, new_eligible)| {
                run_signature_metrics(
                    RunSignatureEligibility {
                        old: old_eligible,
                        new: new_eligible,
                    },
                    &old_occurrences,
                    &new_occurrences,
                    &exact_match_candidates,
                    RunSignatureLimits {
                        min_tokens: input.min_tokens,
                        old_tokens: old.total_tokens,
                        new_tokens: new.total_tokens,
                        candidate_pairs: budget.output_range_limit,
                    },
                )
            });
    record_run_signature_metrics(&mut diagnostics, run_metrics);
    let Some(old_candidate_outcome) = recovery_candidates(
        &old_occurrences,
        &counts,
        OccurrenceSide::Old,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        mark_sentence_edge_gate_shadow_incomplete(
            &mut diagnostics,
            input.enable_sentence_edge_gate_shadow,
            budget.near_relation_stop_reason,
        );
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(new_candidate_outcome) = recovery_candidates(
        &new_occurrences,
        &counts,
        OccurrenceSide::New,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let candidate_generation_complete =
        old_candidate_outcome.complete && new_candidate_outcome.complete;
    if !candidate_generation_complete {
        budget.record_candidate_count_limit();
    }
    let mut old_candidates = old_candidate_outcome.values;
    let mut new_candidates = new_candidate_outcome.values;
    record_candidate_metrics(
        &mut diagnostics,
        exact_match_candidates.len(),
        &old_candidates,
        &new_candidates,
    );
    drop(counts);
    let mut plan = SentenceRecoveryPlan::default();
    if append_exact_matches(
        &mut plan,
        &mut old_occurrences,
        &mut new_occurrences,
        &exact_match_candidates,
        &mut budget,
    )
    .is_none()
    {
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.metrics.near_relation_complete = false;
    }
    let mut paired_vetoes = PairedNearVetoes::default();
    let mut paired_shadow_stop_reason = None;
    if append_paired_stream_replacements(
        &mut plan,
        &mut old_occurrences,
        &mut new_occurrences,
        &paired_streams,
        &membership.recovery_spans,
        input.min_tokens.min(MIN_PAIRED_STREAM_NEAR_TOKENS),
        &mut budget,
        &mut diagnostics,
        signature_checkpoint,
        &mut paired_vetoes,
        watch.as_mut(),
        &mut paired_shadow_stop_reason,
    )
    .is_none()
    {
        mark_sentence_edge_gate_shadow_incomplete_with_reason(
            &mut diagnostics,
            input.enable_sentence_edge_gate_shadow,
            paired_shadow_stop_reason.unwrap_or_else(|| {
                budget.near_relation_stop_reason.map_or(
                    SentenceEdgeGateShadowStopReason::DiagnosticFailure,
                    Into::into,
                )
            }),
        );
        if let Some(watch) = watch.as_mut() {
            watch.record_near_search_state(
                candidate_generation_complete,
                false,
                budget.near_relation_stop_reason,
            );
        }
        if plan.has_exact_matches()
            && normalize_ranges(&mut plan.deletion_consumed)
            && normalize_ranges(&mut plan.insertion_consumed)
        {
            record_near_search_metrics(&mut diagnostics, &budget);
            let watch_diagnostics = watch.map(|watch| watch.finish(Some(&plan)));
            return Ok(SentenceRecoveryBuildOutcome {
                plan: Some(plan),
                diagnostics,
                watch_diagnostics,
                ..SentenceRecoveryBuildOutcome::default()
            });
        }
        if watch.is_some() {
            if signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct {
                record_near_search_metrics(&mut diagnostics, &budget);
            }
            return Ok(SentenceRecoveryBuildOutcome {
                plan: None,
                diagnostics: (signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct)
                    .then_some(diagnostics)
                    .flatten(),
                watch_diagnostics: watch.map(|watch| watch.finish(None)),
                ..SentenceRecoveryBuildOutcome::default()
            });
        }
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    old_candidates.retain(|candidate| {
        old_occurrences[candidate.occurrence_index]
            .location
            .is_some()
            && paired_vetoes
                .old_occurrences
                .binary_search(&candidate.occurrence_index)
                .is_err()
    });
    new_candidates.retain(|candidate| {
        new_occurrences[candidate.occurrence_index]
            .location
            .is_some()
            && paired_vetoes
                .new_occurrences
                .binary_search(&candidate.occurrence_index)
                .is_err()
    });
    let near_pair_start = diagnostics
        .as_ref()
        .map_or(0, |diagnostics| diagnostics.metrics.near_pair_candidates);
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled {
        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            signature_checkpoint,
            budget.sentence_edge_signature_filter_mode,
        );
    }
    let Some(mut relations) = modified_sentence_relations_tracked(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &paired_streams,
        &mut budget,
        &mut diagnostics,
        signature_checkpoint,
        watch.as_mut(),
    ) else {
        if let Some(watch) = watch.as_mut() {
            watch.record_near_search_state(
                candidate_generation_complete,
                false,
                budget.near_relation_stop_reason,
            );
        }
        if plan.has_exact_matches()
            && normalize_ranges(&mut plan.deletion_consumed)
            && normalize_ranges(&mut plan.insertion_consumed)
        {
            record_near_search_metrics(&mut diagnostics, &budget);
            let watch_diagnostics = watch.map(|watch| watch.finish(Some(&plan)));
            return Ok(SentenceRecoveryBuildOutcome {
                plan: Some(plan),
                diagnostics,
                watch_diagnostics,
                ..SentenceRecoveryBuildOutcome::default()
            });
        }
        if watch.is_some() {
            if signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct {
                record_near_search_metrics(&mut diagnostics, &budget);
            }
            return Ok(SentenceRecoveryBuildOutcome {
                plan: None,
                diagnostics: (signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct)
                    .then_some(diagnostics)
                    .flatten(),
                watch_diagnostics: watch.map(|watch| watch.finish(None)),
                ..SentenceRecoveryBuildOutcome::default()
            });
        }
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let fragment_veto_complete = veto_fragment_completed_replacements(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &old_fragments,
        &new_fragments,
        &mut relations,
        &mut fragment_veto_budget,
    );
    let fragment_veto_stop_reason =
        (!fragment_veto_complete).then(|| fragment_veto_budget.stop_reason());
    record_sentence_edge_gate_shadow(
        &mut diagnostics,
        &relations,
        budget.near_relation_stop_reason,
    );
    if !candidate_generation_complete {
        mark_sentence_edge_gate_shadow_incomplete(
            &mut diagnostics,
            input.enable_sentence_edge_gate_shadow,
            budget
                .near_relation_stop_reason
                .or(Some(NearRelationStopReason::CandidateCountLimit)),
        );
    }
    record_vetoed_near_pairs(&mut diagnostics, &relations, near_pair_start);
    if watch.as_mut().is_some_and(|watch| {
        watch
            .record_relations(&old_candidates, &new_candidates, &relations)
            .is_none()
    }) {
        budget.record_watch_diagnostic_failure();
        watch = None;
    }
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.metrics.near_relation_complete =
            relations.complete && candidate_generation_complete;
    }
    record_near_search_metrics(&mut diagnostics, &budget);
    if let Some(watch) = watch.as_mut() {
        watch.record_near_search_state(
            candidate_generation_complete,
            relations.complete && candidate_generation_complete,
            budget.near_relation_stop_reason,
        );
    }

    if append_replacements(
        &mut plan,
        &mut old_occurrences,
        &mut new_occurrences,
        &old_candidates,
        &new_candidates,
        &relations,
        &mut budget,
    )
    .is_none()
        || append_candidate_recoveries(
            &mut plan.deletions,
            &mut plan.deletion_consumed,
            &mut old_occurrences,
            &old_candidates,
            &relations.old,
            &mut budget,
        )
        .is_none()
        || append_candidate_recoveries(
            &mut plan.insertions,
            &mut plan.insertion_consumed,
            &mut new_occurrences,
            &new_candidates,
            &relations.new,
            &mut budget,
        )
        .is_none()
        || !normalize_ranges(&mut plan.deletion_consumed)
        || !normalize_ranges(&mut plan.insertion_consumed)
    {
        if watch.is_some() {
            return Ok(SentenceRecoveryBuildOutcome {
                plan: None,
                diagnostics: None,
                watch_diagnostics: watch.map(|watch| watch.finish(None)),
                ..SentenceRecoveryBuildOutcome::default()
            });
        }
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    finalize_reference_observer(
        &mut diagnostics,
        relations.edge_signature_shadow.as_ref(),
        fragment_veto_complete,
    );
    let watch_diagnostics = watch.map(|watch| watch.finish(Some(&plan)));
    Ok(SentenceRecoveryBuildOutcome {
        plan: Some(plan),
        diagnostics,
        watch_diagnostics,
        fragment_veto_complete,
        fragment_veto_stop_reason,
        fragment_veto_pair_visits_examined: fragment_veto_budget.pair_visits,
        fragment_veto_pair_visits_attempted: fragment_veto_budget.pair_visits_attempted,
        fragment_veto_comparisons_examined: fragment_veto_budget.comparisons,
        fragment_veto_comparisons_attempted: fragment_veto_budget.comparisons_attempted,
    })
}

fn validate_occurrence_evidence(
    occurrences: &[SentenceOccurrence],
    evidence: Option<&RunRecoveryEvidence<'_>>,
) -> Option<()> {
    let Some(evidence) = evidence else {
        return Some(());
    };
    for occurrence in occurrences {
        if occurrence.run_descriptor_index.is_some() {
            evidence.descriptor_for_occurrence(occurrence)?;
        }
        if let Some(indices) = evidence.descriptor_indices_for_untrusted_occurrence(occurrence)
            && !indices
                .iter()
                .all(|index| evidence.descriptors.get(*index).is_some())
        {
            return None;
        }
    }
    let _raw_region_edges = evidence.raw_region_edges();
    Some(())
}

fn record_near_search_metrics(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    budget: &RecoveryBudget,
) {
    if !budget.near_metrics_available {
        *diagnostics = None;
        return;
    }
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    diagnostics.metrics.near_candidate_posting_visits_examined = budget.candidate_posting_visits;
    diagnostics.metrics.near_candidate_posting_visits_attempted =
        budget.candidate_posting_visits_attempted;
    diagnostics.metrics.near_sentence_work = budget.sentence_work;
    diagnostics.metrics.near_line_work = budget.line_work;
    diagnostics.metrics.near_paired_interval_work = budget.paired_interval_work;
    diagnostics.metrics.near_paired_cross_interval_veto_work =
        budget.paired_cross_interval_veto_work;
    diagnostics.metrics.near_same_or_ambiguous_span_work = budget.same_or_ambiguous_span_work;
    diagnostics.metrics.near_same_known_span_work = budget.same_known_span_work;
    diagnostics.metrics.near_ambiguous_span_work = budget.ambiguous_span_work;
    diagnostics.metrics.near_same_or_ambiguous_shared_query_work =
        budget.same_or_ambiguous_shared_query_work;
    diagnostics.metrics.near_cross_span_work = budget.cross_span_work;
    diagnostics.metrics.near_pair_visits_examined = budget.pair_visits;
    diagnostics.metrics.near_pair_visits_attempted = budget.pair_visits_attempted;
    diagnostics.metrics.near_similarity_comparisons_examined = budget.comparisons;
    diagnostics.metrics.near_similarity_comparisons_attempted = budget.comparisons_attempted;
    diagnostics.metrics.sentence_edge_filter_complete =
        budget.sentence_edge_filter_stop_reason.is_none();
    diagnostics.metrics.sentence_edge_filter_pairs_examined = budget.sentence_edge_filter_pairs;
    diagnostics.metrics.sentence_edge_filter_pairs_attempted =
        budget.sentence_edge_filter_pairs_attempted;
    diagnostics
        .metrics
        .sentence_edge_filter_similarity_comparisons_examined =
        budget.sentence_edge_filter_comparisons;
    diagnostics
        .metrics
        .sentence_edge_filter_similarity_comparisons_attempted =
        budget.sentence_edge_filter_comparisons_attempted;
    diagnostics.metrics.sentence_edge_filter_pairs_retained =
        budget.sentence_edge_filter_pairs_retained;
    diagnostics.metrics.sentence_edge_filter_pairs_rejected =
        budget.sentence_edge_filter_pairs_rejected;
    diagnostics.metrics.sentence_edge_filter_stop_reason = budget.sentence_edge_filter_stop_reason;
    diagnostics.metrics.near_largest_edge_posting = budget.largest_edge_posting;
    diagnostics.metrics.near_largest_edge_query_union = budget.largest_edge_query_union;
    diagnostics.metrics.near_largest_filtered_candidate_set = budget.largest_filtered_candidate_set;
    diagnostics.metrics.near_candidate_count_truncated = budget.candidate_count_truncated;
    diagnostics.metrics.near_relation_stop_reason = budget.near_relation_stop_reason;
    if budget.sentence_edge_signature_filter_mode == SentenceEdgeSignatureFilterMode::Direct {
        let direct = diagnostics
            .metrics
            .sentence_edge_signature_direct_shadow
            .get_or_insert_with(SentenceEdgeSignatureDirectShadowMetrics::default);
        direct.exact_edge_rechecks = budget.signature_exact_rechecks;
        direct.exact_edge_rechecks_attempted = budget.signature_exact_rechecks_attempted;
        direct.exact_edge_recheck_comparisons_examined = budget.signature_exact_recheck_comparisons;
        direct.exact_edge_recheck_comparisons_attempted =
            budget.signature_exact_recheck_comparisons_attempted;
        direct.exact_edge_retained_pairs = budget.sentence_edge_filter_pairs_retained;
        direct.exact_edge_rejected_pairs = budget.sentence_edge_filter_pairs_rejected;
        direct.watch_probe_pairs_examined = budget.watch_probe_pairs;
        direct.watch_probe_pairs_attempted = budget.watch_probe_pairs_attempted;
        direct.watch_probe_similarity_comparisons_examined = budget.watch_probe_comparisons;
        direct.watch_probe_similarity_comparisons_attempted =
            budget.watch_probe_comparisons_attempted;
        direct.watch_probe_missing_signature_candidates =
            budget.watch_probe_missing_signature_candidates;
        direct.watch_probe_invariant_violations = budget.watch_probe_invariant_violations;
        if let Some(reason) = budget.signature_exact_recheck_stop_reason {
            direct.complete = false;
            direct.stop_reason.get_or_insert(reason);
        }
        if let Some(reason) = budget.watch_probe_stop_reason {
            direct.complete = false;
            direct.stop_reason.get_or_insert(reason);
        }
    }
    if let Some(signature) = diagnostics.metrics.sentence_edge_signature_shadow.as_mut()
        && let Some(reason) = budget.near_relation_stop_reason
    {
        signature.complete = false;
        signature.stop_reason.get_or_insert(match reason {
            NearRelationStopReason::CandidatePostingVisitLimit => {
                SentenceEdgeSignatureShadowStopReason::CandidatePostingVisitLimit
            }
            NearRelationStopReason::PairVisitLimit => {
                SentenceEdgeSignatureShadowStopReason::PairVisitLimit
            }
            NearRelationStopReason::SimilarityComparisonLimit => {
                SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit
            }
            NearRelationStopReason::CandidateCountLimit => {
                SentenceEdgeSignatureShadowStopReason::CandidateCountLimit
            }
        });
    }
    enforce_edge_gate_shadow_completion(&mut diagnostics.metrics);
}

fn enforce_edge_gate_shadow_completion(metrics: &mut SentenceRecoveryMetrics) {
    if metrics.near_relation_complete && metrics.near_relation_stop_reason.is_none() {
        return;
    }
    let Some(shadow) = metrics.sentence_edge_gate_shadow.as_mut() else {
        return;
    };
    let reason = metrics
        .near_relation_stop_reason
        .map(Into::into)
        .unwrap_or_else(|| {
            shadow
                .stop_reason
                .unwrap_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)
        });
    shadow.complete = false;
    shadow.stop_reason.get_or_insert(reason);
}

fn sentence_recovery_diagnostics(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
    old_trusted_run_intervals: &[Option<TrustedRunInterval>],
    new_trusted_run_intervals: &[Option<TrustedRunInterval>],
    recovery_spans: &[bool],
) -> Option<SentenceRecoveryDiagnostics> {
    Some(SentenceRecoveryDiagnostics {
        metrics: SentenceRecoveryMetrics {
            old_trusted_run_source_tokens: trusted_run_source_tokens(
                old,
                old_trusted_run_intervals,
            )?,
            new_trusted_run_source_tokens: trusted_run_source_tokens(
                new,
                new_trusted_run_intervals,
            )?,
            near_relation_complete: true,
            sentence_edge_filter_complete: true,
            ..SentenceRecoveryMetrics::default()
        },
        eligible_old_source_tokens: eligible_source_tokens(old, alignment, recovery_spans, true)?,
        eligible_new_source_tokens: eligible_source_tokens(new, alignment, recovery_spans, false)?,
        signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
        signature_retained_fingerprint_valid: false,
    })
}

fn trusted_run_source_tokens(
    side: &Side<'_>,
    trusted_run_intervals: &[Option<TrustedRunInterval>],
) -> Option<usize> {
    if side.blocks.len() != trusted_run_intervals.len() {
        return None;
    }
    trusted_run_intervals
        .iter()
        .enumerate()
        .filter(|(_, interval)| interval.is_some())
        .try_fold(0usize, |tokens, (block_index, _)| {
            tokens.checked_add(side.canonical.get(block_index)?.len())
        })
}

fn eligible_source_tokens(
    side: &Side<'_>,
    alignment: &Alignment,
    recovery_spans: &[bool],
    old_side: bool,
) -> Option<usize> {
    alignment
        .spans
        .iter()
        .zip(recovery_spans)
        .filter(|(_, eligible)| **eligible)
        .try_fold(0usize, |tokens, (span, _)| {
            let blocks = if old_side { &span.old } else { &span.new };
            blocks.iter().try_fold(tokens, |tokens, block| {
                let block_index = *side.index.get(block)?;
                tokens.checked_add(side.canonical.get(block_index)?.len())
            })
        })
}

fn span_membership(alignment: &Alignment) -> Option<SpanMembership> {
    let mut old_count = 0usize;
    let mut new_count = 0usize;
    for span in &alignment.spans {
        old_count = old_count.checked_add(span.old.len())?;
        new_count = new_count.checked_add(span.new.len())?;
    }

    let mut old = HashMap::new();
    let mut new = HashMap::new();
    let mut recovery_spans = Vec::new();
    old.try_reserve(old_count).ok()?;
    new.try_reserve(new_count).ok()?;
    recovery_spans
        .try_reserve_exact(alignment.spans.len())
        .ok()?;
    for (span_index, span) in alignment.spans.iter().enumerate() {
        recovery_spans.push(is_sentence_recovery_span(span.kind, &span.evidence));
        for block in &span.old {
            if old.insert(*block, span_index).is_some() {
                return None;
            }
        }
        for block in &span.new {
            if new.insert(*block, span_index).is_some() {
                return None;
            }
        }
    }
    Some(SpanMembership {
        old,
        new,
        recovery_spans,
    })
}

fn is_sentence_recovery_span(kind: AlignmentKind, evidence: &[AlignmentEvidence]) -> bool {
    kind == AlignmentKind::Unresolved && evidence == [AlignmentEvidence::ReadingOrderUnknown]
}

fn collect_occurrences(
    side: &Side<'_>,
    trusted_run_intervals: &[Option<TrustedRunInterval>],
    run_evidence: Option<&RunRecoveryEvidence<'_>>,
    span_by_block: &HashMap<BlockId, usize>,
    recovery_spans: &[bool],
    budget: &mut RecoveryBudget,
) -> Option<(Vec<SentenceOccurrence>, Vec<SentenceFragment>)> {
    let plans = stream_plans(trusted_run_intervals)?;
    let mut occurrences = Vec::new();
    let mut fragments = Vec::new();
    for (stream_index, plan) in plans.into_iter().enumerate() {
        let run_descriptor_index = match (plan.run_id, run_evidence) {
            (Some(run_id), Some(evidence)) => Some(evidence.descriptor_index(run_id)?),
            _ => None,
        };
        let stream = build_stream(side, &plan)?;
        let sentence_boundaries =
            sentence_boundaries(&stream.text, &stream.forced_sentence_boundaries, budget)?;
        let fragment_boundary = if stream.trusted {
            trailing_fragment_boundary(&stream.text, &sentence_boundaries, budget)?
        } else {
            None
        };
        let (boundaries, kind) = if stream.atomic_line && sentence_boundaries.is_empty() {
            (
                atomic_line_boundaries(&stream.text, budget)?,
                RecoveryUnitKind::Line,
            )
        } else {
            (sentence_boundaries, RecoveryUnitKind::Sentence)
        };
        for (ordinal, boundary) in boundaries.into_iter().enumerate() {
            let key = stream.text.get(boundary.byte_start..boundary.byte_end)?;
            if !budget.charge_key_bytes(key.len()) {
                return None;
            }
            let mut owned_key = String::new();
            owned_key.try_reserve_exact(key.len()).ok()?;
            owned_key.push_str(key);
            let word_ranges = sorted_word_ranges(&owned_key, budget)?;

            let touched_blocks = sentence_stream_block_range(&stream, boundary)?;
            let page = sentence_page(side, &stream, touched_blocks.clone());
            let tokens = sentence_tokens(&stream, boundary, budget)?;
            let role = sentence_role(side, &stream, touched_blocks.clone())?;
            let span_index =
                sentence_span_index(side, &stream, touched_blocks.clone(), span_by_block)?;
            let location = match (span_index, role) {
                (Some(span_index), Some(role)) if recovery_spans.get(span_index).copied()? => {
                    sentence_location(
                        side,
                        &stream,
                        boundary,
                        touched_blocks,
                        span_index,
                        (kind, role),
                        budget,
                    )?
                }
                _ => None,
            };
            occurrences.try_reserve(1).ok()?;
            occurrences.push(SentenceOccurrence {
                key: owned_key,
                tokens,
                word_ranges,
                kind,
                role,
                location,
                span_index,
                trusted_position: stream.trusted.then_some(TrustedStreamPosition {
                    stream_index,
                    ordinal,
                }),
                run_descriptor_index,
                page,
                evidence_block_index: (!plan.trusted)
                    .then(|| plan.block_indices.first().copied())
                    .flatten(),
            });
        }
        if let Some(boundary) = fragment_boundary {
            let touched_blocks = sentence_stream_block_range(&stream, boundary)?;
            let role = sentence_role(side, &stream, touched_blocks.clone())?;
            let span_index =
                sentence_span_index(side, &stream, touched_blocks.clone(), span_by_block)?;
            if let (Some(span_index), Some(role)) = (span_index, role)
                && recovery_spans.get(span_index).copied()?
            {
                let uncertain = stream.blocks.get(touched_blocks)?.iter().try_fold(
                    false,
                    |uncertain, stream_block| {
                        let block = side.blocks.get(stream_block.side_index)?;
                        Some(
                            uncertain
                                || !block.issues.is_empty()
                                || !block.canonical.unmapped.is_empty(),
                        )
                    },
                )?;
                let tokens = sentence_tokens(&stream, boundary, budget)?;
                fragments.try_reserve(1).ok()?;
                fragments.push(SentenceFragment {
                    tokens,
                    span_index,
                    role,
                    uncertain,
                });
            }
        }
    }
    occurrences.sort_unstable_by_key(|occurrence| occurrence.span_index);
    Some((occurrences, fragments))
}

fn sentence_page(side: &Side<'_>, stream: &Stream, touched_blocks: Range<usize>) -> Option<u32> {
    let mut page = None;
    for stream_block in stream.blocks.get(touched_blocks)? {
        let block = side.blocks.get(stream_block.side_index)?;
        let [block_page] = block.pages.as_slice() else {
            return None;
        };
        if page.is_some_and(|page| page != *block_page) {
            return None;
        }
        page = Some(*block_page);
    }
    page
}

fn sentence_role(
    side: &Side<'_>,
    stream: &Stream,
    touched_blocks: Range<usize>,
) -> Option<Option<BlockRole>> {
    let mut roles = stream
        .blocks
        .get(touched_blocks)?
        .iter()
        .map(|stream_block| {
            side.blocks
                .get(stream_block.side_index)
                .map(|block| block.role)
        });
    let role = roles.next()??;
    Some(
        roles
            .all(|candidate| {
                candidate.is_some_and(|candidate| role.is_alignment_compatible(candidate))
            })
            .then_some(role),
    )
}

fn sorted_word_ranges(text: &str, budget: &mut RecoveryBudget) -> Option<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    for (start, word) in text.unicode_word_indices() {
        ranges.try_reserve(1).ok()?;
        ranges.push(start..start.checked_add(word.len())?);
    }
    if !budget.charge_word_ranges(ranges.len()) {
        return None;
    }
    ranges.sort_unstable_by(|left, right| {
        text[left.clone()]
            .cmp(&text[right.clone()])
            .then_with(|| left.start.cmp(&right.start))
    });
    Some(ranges)
}

fn stream_plans(trusted_run_intervals: &[Option<TrustedRunInterval>]) -> Option<Vec<StreamPlan>> {
    let mut groups = Vec::<StreamPlanGroup>::new();
    let mut trusted_positions = HashMap::<TrustedRunId, usize>::new();
    groups.try_reserve(trusted_run_intervals.len()).ok()?;
    trusted_positions
        .try_reserve(trusted_run_intervals.len())
        .ok()?;

    for (block_index, interval) in trusted_run_intervals.iter().copied().enumerate() {
        match interval {
            Some(interval) => {
                if interval.start >= interval.end {
                    return None;
                }
                let block = IntervalBlock {
                    block_index,
                    start: interval.start,
                    end: interval.end,
                };
                if let Some(position) = trusted_positions.get(&interval.run_id).copied() {
                    let StreamPlanGroup::Trusted { blocks, .. } = groups.get_mut(position)? else {
                        return None;
                    };
                    blocks.try_reserve(1).ok()?;
                    blocks.push(block);
                } else {
                    let mut blocks = Vec::new();
                    blocks.try_reserve(1).ok()?;
                    blocks.push(block);
                    trusted_positions.insert(interval.run_id, groups.len());
                    groups.push(StreamPlanGroup::Trusted {
                        run_id: interval.run_id,
                        blocks,
                    });
                }
            }
            None => groups.push(StreamPlanGroup::Untrusted(block_index)),
        }
    }

    let mut plans = Vec::new();
    plans.try_reserve(trusted_run_intervals.len()).ok()?;
    for group in groups {
        match group {
            StreamPlanGroup::Untrusted(block_index) => {
                let mut block_indices = Vec::new();
                block_indices.try_reserve_exact(1).ok()?;
                block_indices.push(block_index);
                plans.push(StreamPlan {
                    block_indices,
                    trusted: false,
                    run_id: None,
                });
            }
            StreamPlanGroup::Trusted { run_id, mut blocks } => {
                blocks.sort_unstable_by_key(|block| (block.start, block.end, block.block_index));
                let mut current = Vec::new();
                let mut previous: Option<IntervalBlock> = None;
                for block in blocks {
                    let split = if let Some(previous) = previous {
                        if block.start < previous.end {
                            return None;
                        }
                        block.start != previous.end
                            || has_untrusted_barrier(
                                trusted_run_intervals,
                                previous.block_index,
                                block.block_index,
                            )?
                    } else {
                        false
                    };
                    if split {
                        plans.push(StreamPlan {
                            block_indices: std::mem::take(&mut current),
                            trusted: true,
                            run_id: Some(run_id),
                        });
                    }
                    current.try_reserve(1).ok()?;
                    current.push(block.block_index);
                    previous = Some(block);
                }
                if !current.is_empty() {
                    plans.push(StreamPlan {
                        block_indices: current,
                        trusted: true,
                        run_id: Some(run_id),
                    });
                }
            }
        }
    }
    Some(plans)
}

fn has_untrusted_barrier(
    trusted_run_intervals: &[Option<TrustedRunInterval>],
    left: usize,
    right: usize,
) -> Option<bool> {
    let start = left.min(right).checked_add(1)?;
    let end = left.max(right);
    Some(
        trusted_run_intervals
            .get(start..end)?
            .iter()
            .any(Option::is_none),
    )
}

fn append_evidence_tokens(
    combined: &mut Vec<SentenceEvidenceToken>,
    next: &[ComparableToken],
    separator: BlockSeparator,
) -> Option<bool> {
    let insert_space = separator == BlockSeparator::Space
        && !combined.last().is_some_and(|token| token.is_space())
        && !next.first().is_some_and(
            |token| matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace()),
        );
    let additional = next.len().checked_add(usize::from(insert_space))?;
    combined.try_reserve_exact(additional).ok()?;
    if insert_space {
        combined.push(SentenceEvidenceToken::Scalar(' '));
    }
    combined.extend(next.iter().map(SentenceEvidenceToken::from));
    Some(insert_space)
}

fn build_stream(side: &Side<'_>, plan: &StreamPlan) -> Option<Stream> {
    let mut text_capacity = plan.block_indices.len().saturating_sub(1);
    let mut token_capacity = plan.block_indices.len().saturating_sub(1);
    for side_index in &plan.block_indices {
        text_capacity =
            text_capacity.checked_add(side.blocks.get(*side_index)?.canonical.text.len())?;
        token_capacity = token_capacity.checked_add(side.canonical.get(*side_index)?.len())?;
    }

    let mut text = String::new();
    let mut tokens = Vec::<SentenceEvidenceToken>::new();
    let mut blocks = Vec::new();
    let mut forced_sentence_boundaries = Vec::<ForcedSentenceBoundary>::new();
    text.try_reserve_exact(text_capacity).ok()?;
    tokens.try_reserve_exact(token_capacity).ok()?;
    blocks.try_reserve_exact(plan.block_indices.len()).ok()?;
    let mut scalar_count = 0usize;
    let mut previous_ended_terminal = false;
    let mut previous_role = None;

    for (position, side_index) in plan.block_indices.iter().copied().enumerate() {
        let block = side.blocks.get(side_index)?;
        let next = side.canonical.get(side_index)?;
        let role_transition =
            previous_role.is_some_and(|role: BlockRole| !role.is_alignment_compatible(block.role));
        if plan.trusted
            && (previous_ended_terminal || role_transition)
            && !text.is_empty()
            && forced_sentence_boundaries
                .last()
                .is_none_or(|boundary| boundary.byte_offset < text.len())
        {
            forced_sentence_boundaries.try_reserve(1).ok()?;
            forced_sentence_boundaries.push(ForcedSentenceBoundary {
                byte_offset: text.len(),
                scalar_offset: scalar_count,
                include_trailing_unit: role_transition
                    && previous_role.is_some_and(|role| role != BlockRole::Body),
            });
        }
        let previous_len = tokens.len();
        let separator = if position == 0 {
            BlockSeparator::Concatenate
        } else {
            BlockSeparator::Space
        };
        let inserted_space = append_evidence_tokens(&mut tokens, next, separator)?;
        let block_token_start = previous_len.checked_add(usize::from(inserted_space))?;
        if inserted_space {
            text.push(' ');
            scalar_count = scalar_count.checked_add(1)?;
        }

        let scalar_start = scalar_count;
        text.push_str(&block.canonical.text);
        scalar_count = scalar_count.checked_add(block.canonical.text.chars().count())?;
        blocks.push(StreamBlock {
            side_index,
            scalar_range: scalar_start..scalar_count,
            scalar_to_token: scalar_to_token_boundaries(tokens.get(block_token_start..)?)?,
        });
        previous_ended_terminal = is_true_sentence_terminal(block.canonical.text.trim());
        previous_role = Some(block.role);
    }

    let scalar_to_token = scalar_to_token_boundaries(&tokens)?;
    let atomic_line = if let [side_index] = plan.block_indices.as_slice() {
        let block = side.blocks.get(*side_index)?;
        !plan.trusted
            && side.canonical.get(*side_index)?.len() <= MAX_UNTRUSTED_LINE_TOKENS
            && block.line_breaks.as_ref().is_some_and(Vec::is_empty)
    } else {
        false
    };
    Some(Stream {
        text,
        tokens,
        scalar_to_token,
        blocks,
        forced_sentence_boundaries,
        atomic_line,
        trusted: plan.trusted,
    })
}

fn scalar_to_token_boundaries(tokens: &[SentenceEvidenceToken]) -> Option<Vec<usize>> {
    let scalar_count = tokens.iter().filter(|token| token.is_scalar()).count();
    let boundary_count = scalar_count.checked_add(1)?;
    let mut boundaries = Vec::new();
    boundaries.try_reserve_exact(boundary_count).ok()?;
    boundaries.resize(boundary_count, usize::MAX);
    let mut scalar = 0usize;
    for (token_index, token) in tokens.iter().enumerate() {
        boundaries[scalar] = boundaries[scalar].min(token_index);
        if token.is_scalar() {
            scalar = scalar.checked_add(1)?;
        }
    }
    boundaries[scalar] = boundaries[scalar].min(tokens.len());
    boundaries
        .iter()
        .all(|boundary| *boundary != usize::MAX)
        .then_some(boundaries)
}

fn sentence_stream_block_range(
    stream: &Stream,
    boundary: SentenceBoundary,
) -> Option<Range<usize>> {
    let overlaps = |block: &StreamBlock| {
        boundary.scalar_start < block.scalar_range.end
            && block.scalar_range.start < boundary.scalar_end
    };
    let first = stream.blocks.iter().position(overlaps)?;
    let last = stream.blocks.iter().rposition(overlaps)?;
    Some(first..last.checked_add(1)?)
}

fn sentence_location(
    side: &Side<'_>,
    stream: &Stream,
    boundary: SentenceBoundary,
    touched_blocks: Range<usize>,
    span_index: usize,
    unit: (RecoveryUnitKind, BlockRole),
    budget: &mut RecoveryBudget,
) -> Option<Option<SentenceLocation>> {
    let (kind, role) = unit;
    if !stream.trusted && kind != RecoveryUnitKind::Line {
        return Some(None);
    }
    let stream_blocks = stream.blocks.get(touched_blocks)?;
    let first = stream_blocks.first()?;
    let canonical_start = boundary
        .scalar_start
        .checked_sub(first.scalar_range.start)?;
    let canonical_end = boundary.scalar_end.checked_sub(first.scalar_range.start)?;
    let token_origin = *stream.scalar_to_token.get(first.scalar_range.start)?;
    let comparable_start = stream
        .scalar_to_token
        .get(boundary.scalar_start)?
        .checked_sub(token_origin)?;
    let comparable_end = stream
        .scalar_to_token
        .get(boundary.scalar_end)?
        .checked_sub(token_origin)?;
    if comparable_start >= comparable_end {
        return None;
    }

    let mut source_tokens = 0usize;
    let mut consumed_count = 0usize;
    for stream_block in stream_blocks {
        let block = side.blocks.get(stream_block.side_index)?;
        let tokens = side.canonical.get(stream_block.side_index)?;
        if !block.issues.is_empty() || !block.canonical.unmapped.is_empty() {
            return Some(None);
        }

        let overlap_start = boundary.scalar_start.max(stream_block.scalar_range.start);
        let overlap_end = boundary.scalar_end.min(stream_block.scalar_range.end);
        if overlap_start >= overlap_end {
            continue;
        }
        let local_scalar_start = overlap_start.checked_sub(stream_block.scalar_range.start)?;
        let local_scalar_end = overlap_end.checked_sub(stream_block.scalar_range.start)?;
        let token_start = *stream_block.scalar_to_token.get(local_scalar_start)?;
        let token_end = *stream_block.scalar_to_token.get(local_scalar_end)?;
        if token_start >= token_end || token_end > tokens.len() {
            return None;
        }
        consumed_count = consumed_count.checked_add(1)?;
        source_tokens = source_tokens.checked_add(token_end.checked_sub(token_start)?)?;
    }
    if source_tokens == 0 {
        return None;
    }
    if !budget.charge_location_metadata(stream_blocks.len(), consumed_count) {
        return None;
    }

    let mut block_ids = Vec::new();
    let mut consumed = Vec::new();
    block_ids.try_reserve_exact(stream_blocks.len()).ok()?;
    consumed.try_reserve_exact(consumed_count).ok()?;
    for stream_block in stream_blocks {
        let block = side.blocks.get(stream_block.side_index)?;
        block_ids.push(block.block);

        let overlap_start = boundary.scalar_start.max(stream_block.scalar_range.start);
        let overlap_end = boundary.scalar_end.min(stream_block.scalar_range.end);
        if overlap_start >= overlap_end {
            continue;
        }
        let local_scalar_start = overlap_start.checked_sub(stream_block.scalar_range.start)?;
        let local_scalar_end = overlap_end.checked_sub(stream_block.scalar_range.start)?;
        let token_start = *stream_block.scalar_to_token.get(local_scalar_start)?;
        let token_end = *stream_block.scalar_to_token.get(local_scalar_end)?;
        consumed.push(LocalSentenceRange {
            block: block.block,
            canonical: ScalarRange {
                start: local_scalar_start,
                end: local_scalar_end,
            },
            comparable: TokenRange {
                start: token_start,
                end: token_end,
            },
        });
    }

    Some(Some(SentenceLocation {
        recovery: RecoveredSentence {
            span_index,
            kind,
            role: role.into(),
            separator: (block_ids.len() > 1).then_some(BlockSeparator::Space),
            blocks: block_ids,
            canonical: ScalarRange {
                start: canonical_start,
                end: canonical_end,
            },
            comparable: TokenRange {
                start: comparable_start,
                end: comparable_end,
            },
            source_tokens,
        },
        consumed,
    }))
}

fn sentence_tokens(
    stream: &Stream,
    boundary: SentenceBoundary,
    budget: &mut RecoveryBudget,
) -> Option<Vec<SentenceEvidenceToken>> {
    let token_start = *stream.scalar_to_token.get(boundary.scalar_start)?;
    let token_end = *stream.scalar_to_token.get(boundary.scalar_end)?;
    let tokens = stream.tokens.get(token_start..token_end)?;
    if !budget.charge_evidence_tokens(tokens.len()) {
        return None;
    }
    let mut owned = Vec::new();
    owned.try_reserve_exact(tokens.len()).ok()?;
    owned.extend_from_slice(tokens);
    Some(owned)
}

fn sentence_span_index(
    side: &Side<'_>,
    stream: &Stream,
    touched_blocks: Range<usize>,
    span_by_block: &HashMap<BlockId, usize>,
) -> Option<Option<usize>> {
    let mut span_index = None;
    let mut ambiguous = false;
    for stream_block in stream.blocks.get(touched_blocks)? {
        let block_id = side.blocks.get(stream_block.side_index)?.block;
        let next_span = *span_by_block.get(&block_id)?;
        match span_index {
            Some(current) if current != next_span => ambiguous = true,
            Some(_) => {}
            None => span_index = Some(next_span),
        }
    }
    let span_index = span_index?;
    Some((!ambiguous).then_some(span_index))
}

fn occurrence_counts<'a>(
    old: &'a [SentenceOccurrence],
    new: &'a [SentenceOccurrence],
) -> Option<HashMap<OccurrenceKey<'a>, OccurrenceCount>> {
    let capacity = old.len().checked_add(new.len())?;
    let mut counts = HashMap::new();
    counts.try_reserve(capacity).ok()?;
    count_occurrences(&mut counts, old, OccurrenceSide::Old)?;
    count_occurrences(&mut counts, new, OccurrenceSide::New)?;
    Some(counts)
}

fn count_occurrences<'a>(
    counts: &mut HashMap<OccurrenceKey<'a>, OccurrenceCount>,
    occurrences: &'a [SentenceOccurrence],
    side: OccurrenceSide,
) -> Option<()> {
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        let Some(role) = occurrence.role else {
            continue;
        };
        let count = counts
            .entry((occurrence.key.as_str(), occurrence.kind, role.into()))
            .or_default();
        match side {
            OccurrenceSide::Old => {
                count.old = count.old.checked_add(1)?;
                count.old_index.get_or_insert(occurrence_index);
            }
            OccurrenceSide::New => {
                count.new = count.new.checked_add(1)?;
                count.new_index.get_or_insert(occurrence_index);
            }
        }
    }
    Some(())
}

fn record_candidate_metrics(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    exact_shared_units: usize,
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
) {
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    diagnostics.metrics.exact_shared_units = exact_shared_units;
    diagnostics.metrics.old_exact_one_sided_units = old_candidates.len();
    diagnostics.metrics.new_exact_one_sided_units = new_candidates.len();
}

fn exact_match_candidates(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    counts: &HashMap<OccurrenceKey<'_>, OccurrenceCount>,
    recovery_spans: &[bool],
    min_tokens: usize,
    max_candidates: usize,
) -> Option<Vec<ExactMatchCandidate>> {
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(counts.len().min(max_candidates))
        .ok()?;
    for phase in [RecoveryUnitKind::Sentence, RecoveryUnitKind::Line] {
        for (old_occurrence_index, old) in old_occurrences.iter().enumerate() {
            let Some(old_role) = old.role else {
                continue;
            };
            let Some(count) = counts.get(&(old.key.as_str(), old.kind, old_role.into())) else {
                continue;
            };
            if count.old != 1 || count.new != 1 || count.old_index != Some(old_occurrence_index) {
                continue;
            }
            let new_occurrence_index = count.new_index?;
            let new = new_occurrences.get(new_occurrence_index)?;
            if !new
                .role
                .is_some_and(|new_role| old_role.is_alignment_compatible(new_role))
            {
                continue;
            }
            let candidate_kind = if old.kind == RecoveryUnitKind::Sentence
                && new.kind == RecoveryUnitKind::Sentence
            {
                RecoveryUnitKind::Sentence
            } else {
                RecoveryUnitKind::Line
            };
            if candidate_kind != phase {
                continue;
            }
            let Some(old_span_index) = old.span_index else {
                continue;
            };
            let Some(new_span_index) = new.span_index else {
                continue;
            };
            if !recovery_spans.get(old_span_index).copied()?
                || !recovery_spans.get(new_span_index).copied()?
                || old.location.is_none()
                || new.location.is_none()
                || old.tokens.len() < min_tokens
                || new.tokens.len() < min_tokens
            {
                continue;
            }
            if candidates.len() == max_candidates {
                if phase == RecoveryUnitKind::Sentence {
                    return None;
                }
                break;
            }
            candidates.push(ExactMatchCandidate {
                old_span_index,
                new_span_index,
                old_occurrence_index,
                new_occurrence_index,
            });
        }
    }
    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.old_span_index,
            candidate.new_span_index,
            candidate.old_occurrence_index,
            candidate.new_occurrence_index,
        )
    });
    Some(candidates)
}

#[derive(Clone, Copy)]
struct TrustedStreamRelation {
    partner: usize,
    ambiguous: bool,
}

#[derive(Default)]
struct PairedStreamOccurrences {
    old: Vec<usize>,
    new: Vec<usize>,
}

struct PairedTrustedStream {
    old_stream: usize,
    new_stream: usize,
    anchors: Vec<(usize, usize)>,
}

struct PairedExactProposal {
    pair_index: usize,
    interval_index: usize,
    old_ordinal: usize,
    new_ordinal: usize,
    old_occurrence_index: usize,
    new_occurrence_index: usize,
}

struct PairedOccurrenceScope<'a> {
    pairs: &'a [PairedTrustedStream],
    pair_by_stream: &'a HashMap<usize, usize>,
    recovery_spans: &'a [bool],
    min_tokens: usize,
    max_occurrences: usize,
    side: OccurrenceSide,
}

type PairedExactExtensionResult<T> = std::result::Result<T, SentenceEdgeGateShadowStopReason>;

fn extend_paired_stream_exact_matches<'a>(
    old_occurrences: &'a [SentenceOccurrence],
    new_occurrences: &'a [SentenceOccurrence],
    candidates: &mut Vec<ExactMatchCandidate>,
    pairs: &[PairedTrustedStream],
    recovery_spans: &[bool],
    min_tokens: usize,
    max_candidates: usize,
) -> PairedExactExtensionResult<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let additional = max_candidates
        .checked_sub(candidates.len())
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    candidates
        .try_reserve(additional)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;

    let mut old_pair_by_stream = HashMap::new();
    let mut new_pair_by_stream = HashMap::new();
    old_pair_by_stream
        .try_reserve(pairs.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    new_pair_by_stream
        .try_reserve(pairs.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for (pair_index, pair) in pairs.iter().enumerate() {
        old_pair_by_stream.insert(pair.old_stream, pair_index);
        new_pair_by_stream.insert(pair.new_stream, pair_index);
    }

    let group_limit = max_candidates
        .checked_mul(2)
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    let mut groups =
        HashMap::<(usize, usize, &'a str, OccurrenceRole), PairedStreamOccurrences>::new();
    groups
        .try_reserve(group_limit)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    append_paired_stream_occurrences(
        &mut groups,
        old_occurrences,
        PairedOccurrenceScope {
            pairs,
            pair_by_stream: &old_pair_by_stream,
            recovery_spans,
            min_tokens,
            max_occurrences: max_candidates,
            side: OccurrenceSide::Old,
        },
    )?;
    append_paired_stream_occurrences(
        &mut groups,
        new_occurrences,
        PairedOccurrenceScope {
            pairs,
            pair_by_stream: &new_pair_by_stream,
            recovery_spans,
            min_tokens,
            max_occurrences: max_candidates,
            side: OccurrenceSide::New,
        },
    )?;

    let mut selected_old = HashSet::new();
    let mut selected_new = HashSet::new();
    selected_old
        .try_reserve(max_candidates)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    selected_new
        .try_reserve(max_candidates)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for candidate in candidates.iter() {
        selected_old.insert(candidate.old_occurrence_index);
        selected_new.insert(candidate.new_occurrence_index);
    }

    let remaining_candidates = max_candidates
        .checked_sub(candidates.len())
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    let mut proposals = Vec::new();
    proposals
        .try_reserve_exact(remaining_candidates)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for ((pair_index, interval_index, _, _), group) in groups.iter_mut() {
        if group.old.len() != group.new.len() || group.old.is_empty() {
            continue;
        }
        group.old.sort_unstable_by_key(|index| {
            old_occurrences[*index]
                .trusted_position
                .map(|position| position.ordinal)
        });
        group.new.sort_unstable_by_key(|index| {
            new_occurrences[*index]
                .trusted_position
                .map(|position| position.ordinal)
        });
        if group.old.iter().any(|index| selected_old.contains(index))
            || group.new.iter().any(|index| selected_new.contains(index))
        {
            continue;
        }
        for (old_index, new_index) in group.old.iter().copied().zip(group.new.iter().copied()) {
            if proposals.len() == remaining_candidates {
                return Err(SentenceEdgeGateShadowStopReason::CandidateCountLimit);
            }
            let old = old_occurrences
                .get(old_index)
                .and_then(|occurrence| occurrence.trusted_position)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
            let new = new_occurrences
                .get(new_index)
                .and_then(|occurrence| occurrence.trusted_position)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
            proposals.push(PairedExactProposal {
                pair_index: *pair_index,
                interval_index: *interval_index,
                old_ordinal: old.ordinal,
                new_ordinal: new.ordinal,
                old_occurrence_index: old_index,
                new_occurrence_index: new_index,
            });
        }
    }
    proposals.sort_unstable_by_key(|proposal| {
        (
            proposal.pair_index,
            proposal.interval_index,
            proposal.old_ordinal,
            proposal.new_ordinal,
        )
    });
    let mut proposal_start = 0usize;
    while proposal_start < proposals.len() {
        let first = proposals
            .get(proposal_start)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let proposal_end = proposal_start
            + proposals[proposal_start..].partition_point(|proposal| {
                proposal.pair_index == first.pair_index
                    && proposal.interval_index == first.interval_index
            });
        let interval = proposals
            .get(proposal_start..proposal_end)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        if interval.windows(2).all(|proposals| {
            proposals[0].old_ordinal < proposals[1].old_ordinal
                && proposals[0].new_ordinal < proposals[1].new_ordinal
        }) {
            for proposal in interval {
                candidates.push(ExactMatchCandidate {
                    old_span_index: old_occurrences
                        .get(proposal.old_occurrence_index)
                        .and_then(|occurrence| occurrence.span_index)
                        .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
                    new_span_index: new_occurrences
                        .get(proposal.new_occurrence_index)
                        .and_then(|occurrence| occurrence.span_index)
                        .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
                    old_occurrence_index: proposal.old_occurrence_index,
                    new_occurrence_index: proposal.new_occurrence_index,
                });
            }
        }
        proposal_start = proposal_end;
    }
    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.old_span_index,
            candidate.new_span_index,
            candidate.old_occurrence_index,
            candidate.new_occurrence_index,
        )
    });
    Ok(())
}

fn paired_trusted_streams(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    candidates: &[ExactMatchCandidate],
) -> Option<Vec<PairedTrustedStream>> {
    let mut old_relations = HashMap::<usize, TrustedStreamRelation>::new();
    let mut new_relations = HashMap::<usize, TrustedStreamRelation>::new();
    old_relations.try_reserve(candidates.len()).ok()?;
    new_relations.try_reserve(candidates.len()).ok()?;
    for candidate in candidates {
        let Some(old_position) = old_occurrences
            .get(candidate.old_occurrence_index)?
            .trusted_position
        else {
            continue;
        };
        let Some(new_position) = new_occurrences
            .get(candidate.new_occurrence_index)?
            .trusted_position
        else {
            continue;
        };
        record_stream_relation(
            &mut old_relations,
            old_position.stream_index,
            new_position.stream_index,
        );
        record_stream_relation(
            &mut new_relations,
            new_position.stream_index,
            old_position.stream_index,
        );
    }

    let mut stream_pairs = Vec::new();
    stream_pairs.try_reserve(old_relations.len()).ok()?;
    for (old_stream, relation) in old_relations {
        if relation.ambiguous {
            continue;
        }
        let Some(reverse) = new_relations.get(&relation.partner) else {
            continue;
        };
        if !reverse.ambiguous && reverse.partner == old_stream {
            stream_pairs.push((old_stream, relation.partner));
        }
    }
    stream_pairs.sort_unstable();

    let mut pair_index_by_old_stream = HashMap::new();
    pair_index_by_old_stream
        .try_reserve(stream_pairs.len())
        .ok()?;
    let mut pairs = Vec::new();
    pairs.try_reserve_exact(stream_pairs.len()).ok()?;
    for (pair_index, (old_stream, new_stream)) in stream_pairs.into_iter().enumerate() {
        pair_index_by_old_stream.insert(old_stream, pair_index);
        pairs.push(PairedTrustedStream {
            old_stream,
            new_stream,
            anchors: Vec::new(),
        });
    }
    for candidate in candidates {
        let Some(old_position) = old_occurrences
            .get(candidate.old_occurrence_index)?
            .trusted_position
        else {
            continue;
        };
        let Some(new_position) = new_occurrences
            .get(candidate.new_occurrence_index)?
            .trusted_position
        else {
            continue;
        };
        let Some(pair_index) = pair_index_by_old_stream
            .get(&old_position.stream_index)
            .copied()
        else {
            continue;
        };
        let pair = pairs.get_mut(pair_index)?;
        if pair.new_stream != new_position.stream_index {
            continue;
        }
        pair.anchors.try_reserve(1).ok()?;
        pair.anchors
            .push((old_position.ordinal, new_position.ordinal));
    }
    pairs.retain_mut(|pair| {
        pair.anchors.sort_unstable();
        pair.anchors
            .windows(2)
            .all(|anchors| anchors[0].0 < anchors[1].0 && anchors[0].1 < anchors[1].1)
    });
    Some(pairs)
}

fn record_stream_relation(
    relations: &mut HashMap<usize, TrustedStreamRelation>,
    stream: usize,
    partner: usize,
) {
    relations
        .entry(stream)
        .and_modify(|relation| relation.ambiguous |= relation.partner != partner)
        .or_insert(TrustedStreamRelation {
            partner,
            ambiguous: false,
        });
}

fn append_paired_stream_occurrences<'a>(
    groups: &mut HashMap<(usize, usize, &'a str, OccurrenceRole), PairedStreamOccurrences>,
    occurrences: &'a [SentenceOccurrence],
    scope: PairedOccurrenceScope<'_>,
) -> PairedExactExtensionResult<()> {
    let mut appended = 0usize;
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        let Some(position) = occurrence.trusted_position else {
            continue;
        };
        let Some(pair_index) = scope.pair_by_stream.get(&position.stream_index).copied() else {
            continue;
        };
        let Some(span_index) = occurrence.span_index else {
            continue;
        };
        let Some(role) = occurrence.role else {
            continue;
        };
        if occurrence.location.is_none()
            || occurrence.tokens.len() < scope.min_tokens
            || !scope
                .recovery_spans
                .get(span_index)
                .copied()
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
        {
            continue;
        }
        appended = appended
            .checked_add(1)
            .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
        if appended > scope.max_occurrences {
            return Err(SentenceEdgeGateShadowStopReason::CandidateCountLimit);
        }
        let pair = scope
            .pairs
            .get(pair_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let interval_index = pair.anchors.partition_point(|(old, new)| {
            let anchor_ordinal = match scope.side {
                OccurrenceSide::Old => *old,
                OccurrenceSide::New => *new,
            };
            anchor_ordinal < position.ordinal
        });
        let group = groups
            .entry((
                pair_index,
                interval_index,
                occurrence.key.as_str(),
                role.into(),
            ))
            .or_default();
        let indices = match scope.side {
            OccurrenceSide::Old => &mut group.old,
            OccurrenceSide::New => &mut group.new,
        };
        indices
            .try_reserve(1)
            .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
        indices.push(occurrence_index);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_paired_stream_replacements<'a>(
    plan: &mut SentenceRecoveryPlan,
    old_occurrences: &'a mut [SentenceOccurrence],
    new_occurrences: &'a mut [SentenceOccurrence],
    pairs: &[PairedTrustedStream],
    recovery_spans: &[bool],
    min_tokens: usize,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    vetoes: &mut PairedNearVetoes,
    mut watch: Option<&mut RecoveryWatchState>,
    shadow_stop_reason: &mut Option<SentenceEdgeGateShadowStopReason>,
) -> Option<()> {
    if pairs.is_empty() {
        return Some(());
    }
    let (old_pair_by_stream, new_pair_by_stream) = match paired_stream_indices_typed(pairs) {
        Ok(indices) => indices,
        Err(reason) => {
            *shadow_stop_reason = Some(reason);
            return None;
        }
    };
    let Some(group_limit) = budget.output_range_limit.checked_mul(2) else {
        *shadow_stop_reason = Some(SentenceEdgeGateShadowStopReason::CounterOverflow);
        return None;
    };
    let mut groups =
        HashMap::<(usize, usize, &'a str, OccurrenceRole), PairedStreamOccurrences>::new();
    if groups.try_reserve(group_limit).is_err() {
        *shadow_stop_reason = Some(SentenceEdgeGateShadowStopReason::AllocationFailure);
        return None;
    }
    if let Err(reason) = append_paired_stream_occurrences(
        &mut groups,
        old_occurrences,
        PairedOccurrenceScope {
            pairs,
            pair_by_stream: &old_pair_by_stream,
            recovery_spans,
            min_tokens,
            max_occurrences: budget.output_range_limit,
            side: OccurrenceSide::Old,
        },
    ) {
        *shadow_stop_reason = Some(reason);
        return None;
    }
    if let Err(reason) = append_paired_stream_occurrences(
        &mut groups,
        new_occurrences,
        PairedOccurrenceScope {
            pairs,
            pair_by_stream: &new_pair_by_stream,
            recovery_spans,
            min_tokens,
            max_occurrences: budget.output_range_limit,
            side: OccurrenceSide::New,
        },
    ) {
        *shadow_stop_reason = Some(reason);
        return None;
    }

    let old_candidates = match paired_near_candidates(
        &groups,
        old_occurrences,
        OccurrenceSide::Old,
        budget.output_range_limit,
    ) {
        Ok(candidates) => candidates,
        Err(reason) => {
            *shadow_stop_reason = Some(reason);
            return None;
        }
    };
    let new_candidates = match paired_near_candidates(
        &groups,
        new_occurrences,
        OccurrenceSide::New,
        budget.output_range_limit,
    ) {
        Ok(candidates) => candidates,
        Err(reason) => {
            *shadow_stop_reason = Some(reason);
            return None;
        }
    };
    if old_candidates.recoveries.is_empty() || new_candidates.recoveries.is_empty() {
        return Some(());
    }

    let near_pair_start = diagnostics
        .as_ref()
        .map_or(0, |diagnostics| diagnostics.metrics.near_pair_candidates);
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled {
        begin_sentence_edge_signature_stage(
            diagnostics,
            signature_checkpoint,
            budget.sentence_edge_signature_filter_mode,
        );
    }
    let Some(mut relations) = paired_modified_sentence_relations_tracked(
        old_occurrences,
        new_occurrences,
        &old_candidates,
        &new_candidates,
        pairs,
        &old_pair_by_stream,
        &new_pair_by_stream,
        budget,
        diagnostics,
        signature_checkpoint,
        watch.as_deref_mut(),
    ) else {
        *shadow_stop_reason = Some(budget.near_relation_stop_reason.map_or(
            SentenceEdgeGateShadowStopReason::DiagnosticFailure,
            Into::into,
        ));
        return None;
    };
    preserve_paired_stage_failure(
        reject_crossing_paired_replacements(&old_candidates, &new_candidates, &mut relations),
        shadow_stop_reason,
    )?;
    record_sentence_edge_gate_shadow(diagnostics, &relations, budget.near_relation_stop_reason);
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled {
        begin_sentence_edge_signature_stage(
            diagnostics,
            signature_checkpoint,
            budget.sentence_edge_signature_filter_mode,
        );
    }
    record_vetoed_near_pairs(diagnostics, &relations, near_pair_start);
    if let Some(watch) = watch.as_mut()
        && watch
            .record_relations(
                &old_candidates.recoveries,
                &new_candidates.recoveries,
                &relations,
            )
            .is_none()
    {
        watch.complete = false;
    }
    preserve_paired_stage_failure(
        collect_paired_near_vetoes(&old_candidates, &new_candidates, &relations, vetoes),
        shadow_stop_reason,
    )?;
    let result = append_replacements_typed(
        plan,
        old_occurrences,
        new_occurrences,
        &old_candidates.recoveries,
        &new_candidates.recoveries,
        &relations,
        budget,
    );
    preserve_paired_stage_failure(result, shadow_stop_reason)
}

fn preserve_paired_stage_failure(
    outcome: PairedExactExtensionResult<()>,
    shadow_stop_reason: &mut Option<SentenceEdgeGateShadowStopReason>,
) -> Option<()> {
    match outcome {
        Ok(()) => Some(()),
        Err(reason) => {
            *shadow_stop_reason = Some(reason);
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn veto_fragment_completed_replacements(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    old_fragments: &[SentenceFragment],
    new_fragments: &[SentenceFragment],
    relations: &mut ModifiedSentenceRelations,
    budget: &mut FragmentVetoBudget,
) -> bool {
    let shadow_budget = *budget;
    let mut shadow = relations.edge_gate_shadow.take();
    let complete = veto_fragment_completed_replacements_inner(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        old_fragments,
        new_fragments,
        relations,
        budget,
    );
    if let Some(shadow) = shadow.as_mut().filter(|shadow| shadow.active) {
        let mut shadow_relations = ModifiedSentenceRelations {
            old: std::mem::take(&mut shadow.old),
            new: std::mem::take(&mut shadow.new),
            complete: relations.complete,
            edge_gate_shadow: None,
            edge_signature_shadow: None,
        };
        let mut budget = shadow_budget;
        let complete = veto_fragment_completed_replacements_inner(
            old_occurrences,
            new_occurrences,
            old_candidates,
            new_candidates,
            old_fragments,
            new_fragments,
            &mut shadow_relations,
            &mut budget,
        );
        shadow.old = shadow_relations.old;
        shadow.new = shadow_relations.new;
        if !complete {
            shadow.disable(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
        }
    }
    relations.edge_gate_shadow = shadow;
    complete
}

#[allow(clippy::too_many_arguments)]
fn veto_fragment_completed_replacements_inner(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    old_fragments: &[SentenceFragment],
    new_fragments: &[SentenceFragment],
    relations: &mut ModifiedSentenceRelations,
    budget: &mut FragmentVetoBudget,
) -> bool {
    let mut tentative_budget = *budget;
    let Some(vetoes) = fragment_completed_replacements(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        old_fragments,
        new_fragments,
        relations,
        &mut tentative_budget,
    ) else {
        *budget = tentative_budget;
        veto_all_mutual_replacements(relations);
        return false;
    };
    if apply_fragment_vetoes(relations, &vetoes).is_none() {
        tentative_budget
            .stop_reason
            .get_or_insert(FragmentVetoStopReason::InvalidEvidence);
        *budget = tentative_budget;
        veto_all_mutual_replacements(relations);
        return false;
    }
    *budget = tentative_budget;
    true
}

#[allow(clippy::too_many_arguments)]
fn fragment_completed_replacements(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    old_fragments: &[SentenceFragment],
    new_fragments: &[SentenceFragment],
    relations: &ModifiedSentenceRelations,
    budget: &mut FragmentVetoBudget,
) -> Option<Vec<(usize, usize)>> {
    let mut proposals = Vec::new();
    if proposals.try_reserve_exact(relations.old.len()).is_err() {
        budget.record_allocation_failure();
        return None;
    }
    for (old_index, relation) in relations.old.iter().copied().enumerate() {
        if let Some(new_index) = mutual_replacement_partner(old_index, relation, &relations.new) {
            proposals.push((old_index, new_index));
        }
    }
    if proposals.is_empty() {
        return Some(Vec::new());
    }

    let old_fragment_index = fragment_index(old_fragments, budget)?;
    let new_fragment_index = fragment_index(new_fragments, budget)?;
    let mut vetoes = Vec::new();
    if vetoes.try_reserve_exact(proposals.len()).is_err() {
        budget.record_allocation_failure();
        return None;
    }
    for (old_index, new_index) in proposals {
        let old = old_occurrences.get(old_candidates.get(old_index)?.occurrence_index)?;
        let new = new_occurrences.get(new_candidates.get(new_index)?.occurrence_index)?;
        let (shorter, longer, shorter_span, fragment_index) =
            match old.tokens.len().cmp(&new.tokens.len()) {
                std::cmp::Ordering::Less => (
                    old,
                    new,
                    old_candidates.get(old_index)?.span_index,
                    &old_fragment_index,
                ),
                std::cmp::Ordering::Greater => (
                    new,
                    old,
                    new_candidates.get(new_index)?.span_index,
                    &new_fragment_index,
                ),
                std::cmp::Ordering::Equal => continue,
            };
        if shorter.span_index != Some(shorter_span) {
            return None;
        }
        let role = shorter.role?;
        let completed = fragment_index
            .uncertain_spans
            .contains(&(shorter_span, role.into()))
            || clean_fragment_completes(
                fragment_index,
                shorter_span,
                role,
                &shorter.tokens,
                &longer.tokens,
                budget,
            )?;
        if completed {
            vetoes.push((old_index, new_index));
        }
    }

    Some(vetoes)
}

fn apply_fragment_vetoes(
    relations: &mut ModifiedSentenceRelations,
    vetoes: &[(usize, usize)],
) -> Option<()> {
    for (old_index, new_index) in vetoes {
        relations.old.get_mut(*old_index)?.veto_without_partner();
        relations.new.get_mut(*new_index)?.veto_without_partner();
    }
    Some(())
}

fn veto_all_mutual_replacements(relations: &mut ModifiedSentenceRelations) {
    for old_index in 0..relations.old.len() {
        let relation = relations.old[old_index];
        let Some(new_index) = mutual_replacement_partner(old_index, relation, &relations.new)
        else {
            continue;
        };
        relations.old[old_index].veto_without_partner();
        relations.new[new_index].veto_without_partner();
    }
}

fn fragment_index<'a>(
    fragments: &'a [SentenceFragment],
    budget: &mut FragmentVetoBudget,
) -> Option<FragmentIndex<'a>> {
    if !budget.charge_pair_visits(fragments.len()) {
        return None;
    }
    let mut uncertain_spans = HashSet::new();
    let mut clean_by_span_and_len =
        HashMap::<(usize, usize, OccurrenceRole), Vec<&SentenceFragment>>::new();
    for fragment in fragments {
        if fragment.uncertain {
            let key = (fragment.span_index, fragment.role.into());
            if !uncertain_spans.contains(&key) {
                if uncertain_spans.try_reserve(1).is_err() {
                    budget.record_allocation_failure();
                    return None;
                }
                uncertain_spans.insert(key);
            }
            continue;
        }
        let key = (
            fragment.span_index,
            fragment.tokens.len(),
            fragment.role.into(),
        );
        if !clean_by_span_and_len.contains_key(&key) {
            if clean_by_span_and_len.try_reserve(1).is_err() {
                budget.record_allocation_failure();
                return None;
            }
            clean_by_span_and_len.insert(key, Vec::new());
        }
        let bucket = clean_by_span_and_len.get_mut(&key)?;
        if bucket.try_reserve(1).is_err() {
            budget.record_allocation_failure();
            return None;
        }
        bucket.push(fragment);
    }
    Some(FragmentIndex {
        uncertain_spans,
        clean_by_span_and_len,
    })
}

fn clean_fragment_completes(
    index: &FragmentIndex<'_>,
    span_index: usize,
    role: BlockRole,
    shorter: &[SentenceEvidenceToken],
    longer: &[SentenceEvidenceToken],
    budget: &mut FragmentVetoBudget,
) -> Option<bool> {
    let missing = longer.len().checked_sub(shorter.len())?;
    for fragment_len in [Some(missing), missing.checked_sub(1)]
        .into_iter()
        .flatten()
    {
        let Some(fragments) =
            index
                .clean_by_span_and_len
                .get(&(span_index, fragment_len, role.into()))
        else {
            continue;
        };
        for fragment in fragments {
            if !budget.charge_pair_visits(1) {
                return None;
            }
            if joined_tokens_equal(&fragment.tokens, shorter, longer, budget)?
                || joined_tokens_equal(shorter, &fragment.tokens, longer, budget)?
            {
                return Some(true);
            }
        }
    }
    Some(false)
}

fn joined_length_matches(
    left: &[SentenceEvidenceToken],
    right: &[SentenceEvidenceToken],
    target_len: usize,
) -> bool {
    let insert_space = !left.last().is_some_and(|token| token.is_space())
        && !right.first().is_some_and(|token| token.is_space());
    left.len()
        .checked_add(usize::from(insert_space))
        .and_then(|len| len.checked_add(right.len()))
        == Some(target_len)
}

fn joined_tokens_equal(
    left: &[SentenceEvidenceToken],
    right: &[SentenceEvidenceToken],
    target: &[SentenceEvidenceToken],
    budget: &mut FragmentVetoBudget,
) -> Option<bool> {
    if !joined_length_matches(left, right, target.len()) {
        return Some(false);
    }
    let insert_space = !left.last().is_some_and(|token| token.is_space())
        && !right.first().is_some_and(|token| token.is_space());

    let mut target_index = 0usize;
    for token in left {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if !token.is_scalar() || target.get(target_index).copied() != Some(*token) {
            return Some(false);
        }
        target_index = target_index.checked_add(1)?;
    }
    if insert_space {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if target.get(target_index).copied() != Some(SentenceEvidenceToken::Scalar(' ')) {
            return Some(false);
        }
        target_index = target_index.checked_add(1)?;
    }
    for token in right {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if !token.is_scalar() || target.get(target_index).copied() != Some(*token) {
            return Some(false);
        }
        target_index = target_index.checked_add(1)?;
    }
    Some(target_index == target.len())
}

fn collect_paired_near_vetoes(
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    relations: &ModifiedSentenceRelations,
    vetoes: &mut PairedNearVetoes,
) -> PairedExactExtensionResult<()> {
    vetoes
        .old_occurrences
        .try_reserve_exact(relations.old.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    vetoes
        .new_occurrences
        .try_reserve_exact(relations.new.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for (index, relation) in relations.old.iter().enumerate() {
        if relation.vetoed() {
            vetoes.old_occurrences.push(
                old_candidates
                    .recoveries
                    .get(index)
                    .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                    .occurrence_index,
            );
        }
    }
    for (index, relation) in relations.new.iter().enumerate() {
        if relation.vetoed() {
            vetoes.new_occurrences.push(
                new_candidates
                    .recoveries
                    .get(index)
                    .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                    .occurrence_index,
            );
        }
    }
    vetoes.old_occurrences.sort_unstable();
    vetoes.old_occurrences.dedup();
    vetoes.new_occurrences.sort_unstable();
    vetoes.new_occurrences.dedup();
    Ok(())
}

fn paired_stream_indices(
    pairs: &[PairedTrustedStream],
) -> Option<(HashMap<usize, usize>, HashMap<usize, usize>)> {
    paired_stream_indices_typed(pairs).ok()
}

fn paired_stream_indices_typed(
    pairs: &[PairedTrustedStream],
) -> PairedExactExtensionResult<(HashMap<usize, usize>, HashMap<usize, usize>)> {
    let mut old = HashMap::new();
    let mut new = HashMap::new();
    old.try_reserve(pairs.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    new.try_reserve(pairs.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for (pair_index, pair) in pairs.iter().enumerate() {
        if old.insert(pair.old_stream, pair_index).is_some()
            || new.insert(pair.new_stream, pair_index).is_some()
        {
            return Err(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
        }
    }
    Ok((old, new))
}

fn paired_near_candidates(
    groups: &HashMap<(usize, usize, &str, OccurrenceRole), PairedStreamOccurrences>,
    occurrences: &[SentenceOccurrence],
    side: OccurrenceSide,
    max_candidates: usize,
) -> PairedExactExtensionResult<PairedNearCandidates> {
    let mut candidates = PairedNearCandidates::default();
    candidates
        .recoveries
        .try_reserve_exact(max_candidates)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    candidates
        .intervals
        .try_reserve_exact(max_candidates)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    candidates
        .ordinals
        .try_reserve_exact(max_candidates)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for ((pair_index, interval_index, _, _), group) in groups {
        let (own, other) = match side {
            OccurrenceSide::Old => (&group.old, &group.new),
            OccurrenceSide::New => (&group.new, &group.old),
        };
        if own.len() != 1 || !other.is_empty() {
            continue;
        }
        if candidates.recoveries.len() == max_candidates {
            return Err(SentenceEdgeGateShadowStopReason::CandidateCountLimit);
        }
        let occurrence_index = own[0];
        let occurrence = occurrences
            .get(occurrence_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        candidates.recoveries.push(RecoveryCandidate {
            occurrence_index,
            span_index: occurrence
                .span_index
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
        });
        candidates.intervals.push(PairedInterval {
            pair_index: *pair_index,
            interval_index: *interval_index,
        });
        candidates.ordinals.push(
            occurrence
                .trusted_position
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                .ordinal,
        );
    }
    let mut order = Vec::new();
    order
        .try_reserve_exact(candidates.recoveries.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    order.extend(0..candidates.recoveries.len());
    order.sort_unstable_by_key(|index| {
        (
            candidates.intervals[*index],
            candidates.ordinals[*index],
            candidates.recoveries[*index].occurrence_index,
        )
    });
    reorder_paired_candidates(candidates, &order)
}

fn reorder_paired_candidates(
    candidates: PairedNearCandidates,
    order: &[usize],
) -> PairedExactExtensionResult<PairedNearCandidates> {
    let mut sorted = PairedNearCandidates::default();
    sorted
        .recoveries
        .try_reserve_exact(order.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    sorted
        .intervals
        .try_reserve_exact(order.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    sorted
        .ordinals
        .try_reserve_exact(order.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for index in order {
        sorted.recoveries.push(RecoveryCandidate {
            occurrence_index: candidates
                .recoveries
                .get(*index)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                .occurrence_index,
            span_index: candidates
                .recoveries
                .get(*index)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
                .span_index,
        });
        sorted.intervals.push(
            *candidates
                .intervals
                .get(*index)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
        );
        sorted.ordinals.push(
            *candidates
                .ordinals
                .get(*index)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
        );
    }
    Ok(sorted)
}

fn recovery_candidates(
    occurrences: &[SentenceOccurrence],
    counts: &HashMap<OccurrenceKey<'_>, OccurrenceCount>,
    side: OccurrenceSide,
    recovery_spans: &[bool],
    min_tokens: usize,
) -> Option<RecoveryCandidates> {
    let mut candidates = Vec::new();
    candidates.try_reserve(occurrences.len()).ok()?;
    let mut line_candidates = 0usize;
    let mut complete = true;
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        if occurrence.location.is_none() {
            continue;
        }
        let Some(span_index) = occurrence.span_index else {
            continue;
        };
        if occurrence.tokens.len() < min_tokens {
            continue;
        }
        if !recovery_spans.get(span_index).copied()? {
            continue;
        }
        let Some(role) = occurrence.role else {
            continue;
        };
        let Some(count) = counts.get(&(occurrence.key.as_str(), occurrence.kind, role.into()))
        else {
            continue;
        };
        let one_sided = match side {
            OccurrenceSide::Old => count.new == 0 && (count.old == 1 || role != BlockRole::Body),
            OccurrenceSide::New => count.old == 0 && (count.new == 1 || role != BlockRole::Body),
        };
        if one_sided {
            if occurrence.kind == RecoveryUnitKind::Line {
                if line_candidates == MAX_UNTRUSTED_LINE_NEAR_CANDIDATES {
                    complete = false;
                    continue;
                }
                line_candidates = line_candidates.checked_add(1)?;
            }
            candidates.push(RecoveryCandidate {
                occurrence_index,
                span_index,
            });
        }
    }
    candidates.sort_unstable_by_key(|candidate| (candidate.span_index, candidate.occurrence_index));
    Some(RecoveryCandidates {
        values: candidates,
        complete,
    })
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn paired_modified_sentence_relations(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    pairs: &[PairedTrustedStream],
    old_pair_by_stream: &HashMap<usize, usize>,
    new_pair_by_stream: &HashMap<usize, usize>,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    watch: Option<&mut RecoveryWatchState>,
) -> Option<ModifiedSentenceRelations> {
    let mut checkpoint = None;
    paired_modified_sentence_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        pairs,
        old_pair_by_stream,
        new_pair_by_stream,
        budget,
        diagnostics,
        &mut checkpoint,
        watch,
    )
}

#[allow(clippy::too_many_arguments)]
fn paired_modified_sentence_relations_tracked(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    pairs: &[PairedTrustedStream],
    old_pair_by_stream: &HashMap<usize, usize>,
    new_pair_by_stream: &HashMap<usize, usize>,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<ModifiedSentenceRelations> {
    let mut relations =
        empty_modified_sentence_relations(&old_candidates.recoveries, &new_candidates.recoveries)?;
    if budget.enable_sentence_edge_gate_shadow {
        enable_sentence_edge_gate_shadow(&mut relations);
    }
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled {
        enable_sentence_edge_signature_shadow(
            &mut relations,
            budget,
            diagnostics,
            signature_checkpoint,
        );
    }
    let old_candidate_by_occurrence =
        candidate_index_by_occurrence(old_occurrences.len(), &old_candidates.recoveries)?;
    let new_candidate_by_occurrence =
        candidate_index_by_occurrence(new_occurrences.len(), &new_candidates.recoveries)?;
    let old_intervals = paired_intervals_for_occurrences(
        old_occurrences,
        pairs,
        old_pair_by_stream,
        OccurrenceSide::Old,
    )?;
    let new_intervals = paired_intervals_for_occurrences(
        new_occurrences,
        pairs,
        new_pair_by_stream,
        OccurrenceSide::New,
    )?;
    let Some(old_index) = UnitCandidateIndex::new(
        old_occurrences,
        CandidatePostingIndexScope::Paired(&old_intervals),
    ) else {
        checkpoint_direct_signature_setup_failure(budget, diagnostics, signature_checkpoint);
        return None;
    };
    let Some(new_index) = UnitCandidateIndex::new(
        new_occurrences,
        CandidatePostingIndexScope::Paired(&new_intervals),
    ) else {
        checkpoint_direct_signature_setup_failure(budget, diagnostics, signature_checkpoint);
        return None;
    };
    let old_signature_index = build_signature_index(
        relations.edge_signature_shadow.as_mut(),
        old_occurrences,
        CandidatePostingIndexScope::Paired(&old_intervals),
    );
    let new_signature_index = build_signature_index(
        relations.edge_signature_shadow.as_mut(),
        new_occurrences,
        CandidatePostingIndexScope::Paired(&new_intervals),
    );
    if let Some(shadow) = relations.edge_signature_shadow.as_ref() {
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    }
    let mut plausible = Vec::new();
    let scope = NearSearchScope::PairedInterval;

    for (old_candidate_index, old_candidate) in old_candidates.recoveries.iter().enumerate() {
        let interval = *old_candidates.intervals.get(old_candidate_index)?;
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        let query = collect_unit_candidates_unless_direct(
            &new_index,
            &mut plausible,
            old_occurrence,
            new_occurrences,
            CandidatePostingBucket::Paired(interval),
            None,
            budget,
            scope,
        )?;
        plausible.retain(|occurrence_index| {
            new_occurrences[*occurrence_index].kind == old_occurrence.kind
                && occurrence_roles_are_compatible(
                    old_occurrence,
                    &new_occurrences[*occurrence_index],
                )
                && new_intervals.get(*occurrence_index).copied().flatten() == Some(interval)
        });
        budget.record_candidate_query_in_scope(query, old_occurrence.kind, plausible.len(), scope);
        apply_signature_filter(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            new_signature_index.as_ref(),
            old_occurrence,
            new_occurrences,
            &mut plausible,
            CandidatePostingBucket::Paired(interval),
            None,
            scope,
            |index| new_intervals.get(index).copied().flatten() == Some(interval),
        )?;
        probe_direct_watch_pairs(
            watch.as_deref_mut(),
            budget,
            &new_index,
            OccurrenceSide::Old,
            old_candidate.occurrence_index,
            old_occurrence,
            new_occurrences,
            &plausible,
            CandidatePostingBucket::Paired(interval),
            None,
            RecoveryWatchNearScope::PairedStream,
            |index| new_intervals.get(index).copied().flatten() == Some(interval),
        )?;
        let edge_filter = classify_sentence_edge_filter_query_with_index(
            old_occurrence,
            new_occurrences,
            &plausible,
            budget,
            Some((&new_index, CandidatePostingBucket::Paired(interval), None)),
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: old_candidate.occurrence_index,
                query_side: OccurrenceSide::Old,
                scope,
                shadow: relations.edge_gate_shadow.as_mut(),
                watch: watch.as_deref_mut(),
                watch_scope: RecoveryWatchNearScope::PairedStream,
            }),
            |_| true,
        );
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        edge_filter.record_rejections(
            old_candidate.occurrence_index,
            OccurrenceSide::Old,
            old_occurrence,
            new_occurrences,
            scope,
            relations.edge_gate_shadow.as_mut(),
            watch.as_deref_mut(),
            RecoveryWatchNearScope::PairedStream,
        );
        record_signature_exact_retained(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            budget,
            &edge_filter,
            &plausible,
            old_occurrence,
            new_occurrences,
            OccurrenceSide::Old,
            old_candidate.occurrence_index,
            scope,
        )?;
        let retained_count = edge_filter.len(&plausible);
        if !budget.charge_pair_visits_in_scope(retained_count, old_occurrence.kind, scope) {
            return None;
        }
        for pair_index in 0..retained_count {
            let (new_occurrence_index, cached) = edge_filter.pair(&plausible, pair_index)?;
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let new_candidate_index = new_candidate_by_occurrence[new_occurrence_index];
            let score = score_and_record_sentence_edge_gate_shadow(
                old_occurrence,
                new_occurrence,
                budget,
                scope,
                NearSearchWorkClass::Shared,
                None,
                relations.edge_gate_shadow.as_mut(),
                Some(old_candidate_index),
                new_candidate_index,
                cached,
            )?;
            if let Some(watch) = watch.as_mut() {
                watch.record_near(
                    old_candidate.occurrence_index,
                    new_occurrence_index,
                    score,
                    RecoveryWatchNearScope::PairedStream,
                );
            }
            if let Some(new_candidate_index) = new_candidate_index
                && new_candidates.intervals.get(new_candidate_index).copied() == Some(interval)
            {
                relations.old[old_candidate_index].record_eligible(new_candidate_index, score);
                relations.new[new_candidate_index].record_eligible(old_candidate_index, score);
            } else {
                relations.old[old_candidate_index].record_disqualifying(score);
                if let Some(new_candidate_index) = new_candidate_by_occurrence[new_occurrence_index]
                {
                    relations.new[new_candidate_index].record_disqualifying(score);
                }
            }
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }

    for (new_candidate_index, new_candidate) in new_candidates.recoveries.iter().enumerate() {
        let interval = *new_candidates.intervals.get(new_candidate_index)?;
        let new_occurrence = new_occurrences.get(new_candidate.occurrence_index)?;
        let query = collect_unit_candidates_unless_direct(
            &old_index,
            &mut plausible,
            new_occurrence,
            old_occurrences,
            CandidatePostingBucket::Paired(interval),
            None,
            budget,
            scope,
        )?;
        plausible.retain(|occurrence_index| {
            old_occurrences[*occurrence_index].kind == new_occurrence.kind
                && occurrence_roles_are_compatible(
                    new_occurrence,
                    &old_occurrences[*occurrence_index],
                )
                && old_intervals.get(*occurrence_index).copied().flatten() == Some(interval)
        });
        budget.record_candidate_query_in_scope(query, new_occurrence.kind, plausible.len(), scope);
        plausible.retain(|index| old_candidate_by_occurrence[*index].is_none());
        apply_signature_filter(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            old_signature_index.as_ref(),
            new_occurrence,
            old_occurrences,
            &mut plausible,
            CandidatePostingBucket::Paired(interval),
            None,
            scope,
            |index| {
                old_candidate_by_occurrence[index].is_none()
                    && old_intervals.get(index).copied().flatten() == Some(interval)
            },
        )?;
        probe_direct_watch_pairs(
            watch.as_deref_mut(),
            budget,
            &old_index,
            OccurrenceSide::New,
            new_candidate.occurrence_index,
            new_occurrence,
            old_occurrences,
            &plausible,
            CandidatePostingBucket::Paired(interval),
            None,
            RecoveryWatchNearScope::PairedStream,
            |index| {
                old_candidate_by_occurrence[index].is_none()
                    && old_intervals.get(index).copied().flatten() == Some(interval)
            },
        )?;
        let edge_filter = classify_sentence_edge_filter_query_with_index(
            new_occurrence,
            old_occurrences,
            &plausible,
            budget,
            Some((&old_index, CandidatePostingBucket::Paired(interval), None)),
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: new_candidate.occurrence_index,
                query_side: OccurrenceSide::New,
                scope,
                shadow: relations.edge_gate_shadow.as_mut(),
                watch: watch.as_deref_mut(),
                watch_scope: RecoveryWatchNearScope::PairedStream,
            }),
            |index| old_candidate_by_occurrence[index].is_none(),
        );
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        edge_filter.record_rejections(
            new_candidate.occurrence_index,
            OccurrenceSide::New,
            new_occurrence,
            old_occurrences,
            scope,
            relations.edge_gate_shadow.as_mut(),
            watch.as_deref_mut(),
            RecoveryWatchNearScope::PairedStream,
        );
        plausible.retain(|index| old_candidate_by_occurrence[*index].is_none());
        record_signature_exact_retained(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            budget,
            &edge_filter,
            &plausible,
            new_occurrence,
            old_occurrences,
            OccurrenceSide::New,
            new_candidate.occurrence_index,
            scope,
        )?;
        let retained_count = edge_filter.len(&plausible);
        if !budget.charge_pair_visits_in_scope(retained_count, new_occurrence.kind, scope) {
            return None;
        }
        for pair_index in 0..retained_count {
            let (old_occurrence_index, cached) = edge_filter.pair(&plausible, pair_index)?;
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let score = score_and_record_sentence_edge_gate_shadow(
                old_occurrence,
                new_occurrence,
                budget,
                scope,
                NearSearchWorkClass::Shared,
                None,
                relations.edge_gate_shadow.as_mut(),
                None,
                Some(new_candidate_index),
                cached,
            )?;
            if let Some(watch) = watch.as_mut() {
                watch.record_near(
                    old_occurrence_index,
                    new_candidate.occurrence_index,
                    score,
                    RecoveryWatchNearScope::PairedStream,
                );
            }
            relations.new[new_candidate_index].record_disqualifying(score);
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }
    record_cross_interval_disqualifying_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        &old_candidate_by_occurrence,
        &new_candidate_by_occurrence,
        &old_intervals,
        &new_intervals,
        &mut relations,
        budget,
        diagnostics,
        signature_checkpoint,
        watch,
    )?;
    relations.complete = true;
    Some(relations)
}

fn paired_intervals_for_occurrences(
    occurrences: &[SentenceOccurrence],
    pairs: &[PairedTrustedStream],
    pair_by_stream: &HashMap<usize, usize>,
    side: OccurrenceSide,
) -> Option<Vec<Option<PairedInterval>>> {
    let mut intervals = Vec::new();
    intervals.try_reserve_exact(occurrences.len()).ok()?;
    for occurrence in occurrences {
        let interval = occurrence.trusted_position.and_then(|position| {
            let pair_index = pair_by_stream.get(&position.stream_index).copied()?;
            let pair = pairs.get(pair_index)?;
            // Exact anchors belong to the following interval when used as near
            // veto evidence for the one-sided candidates after that anchor.
            let interval_index = pair.anchors.partition_point(|(old, new)| {
                let anchor_ordinal = match side {
                    OccurrenceSide::Old => *old,
                    OccurrenceSide::New => *new,
                };
                anchor_ordinal <= position.ordinal
            });
            Some(PairedInterval {
                pair_index,
                interval_index,
            })
        });
        intervals.push(interval);
    }
    Some(intervals)
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn record_cross_interval_disqualifying_relations(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    old_candidate_by_occurrence: &[Option<usize>],
    new_candidate_by_occurrence: &[Option<usize>],
    old_intervals: &[Option<PairedInterval>],
    new_intervals: &[Option<PairedInterval>],
    relations: &mut ModifiedSentenceRelations,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    watch: Option<&mut RecoveryWatchState>,
) -> Option<()> {
    let mut checkpoint = None;
    record_cross_interval_disqualifying_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        old_candidate_by_occurrence,
        new_candidate_by_occurrence,
        old_intervals,
        new_intervals,
        relations,
        budget,
        diagnostics,
        &mut checkpoint,
        watch,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_cross_interval_disqualifying_relations_tracked(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    old_candidate_by_occurrence: &[Option<usize>],
    new_candidate_by_occurrence: &[Option<usize>],
    old_intervals: &[Option<PairedInterval>],
    new_intervals: &[Option<PairedInterval>],
    relations: &mut ModifiedSentenceRelations,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<()> {
    let Some(old_index) = UnitCandidateIndex::new(
        old_occurrences,
        CandidatePostingIndexScope::PairedStream(old_intervals),
    ) else {
        checkpoint_direct_signature_setup_failure(budget, diagnostics, signature_checkpoint);
        return None;
    };
    let Some(new_index) = UnitCandidateIndex::new(
        new_occurrences,
        CandidatePostingIndexScope::PairedStream(new_intervals),
    ) else {
        checkpoint_direct_signature_setup_failure(budget, diagnostics, signature_checkpoint);
        return None;
    };
    let old_signature_index = build_signature_index(
        relations.edge_signature_shadow.as_mut(),
        old_occurrences,
        CandidatePostingIndexScope::PairedStream(old_intervals),
    );
    let new_signature_index = build_signature_index(
        relations.edge_signature_shadow.as_mut(),
        new_occurrences,
        CandidatePostingIndexScope::PairedStream(new_intervals),
    );
    if let Some(shadow) = relations.edge_signature_shadow.as_ref() {
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    }
    let mut plausible = Vec::new();
    let scope = NearSearchScope::PairedCrossIntervalVeto;

    for (old_candidate_index, old_candidate) in old_candidates.recoveries.iter().enumerate() {
        let interval = *old_candidates.intervals.get(old_candidate_index)?;
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        let query = collect_unit_candidates_unless_direct(
            &new_index,
            &mut plausible,
            old_occurrence,
            new_occurrences,
            CandidatePostingBucket::PairedStream(interval.pair_index),
            None,
            budget,
            scope,
        )?;
        plausible.retain(|occurrence_index| {
            new_occurrences[*occurrence_index].kind == old_occurrence.kind
                && occurrence_roles_are_compatible(
                    old_occurrence,
                    &new_occurrences[*occurrence_index],
                )
                && new_intervals
                    .get(*occurrence_index)
                    .copied()
                    .flatten()
                    .is_some_and(|candidate| {
                        candidate.pair_index == interval.pair_index && candidate != interval
                    })
        });
        budget.record_candidate_query_in_scope(query, old_occurrence.kind, plausible.len(), scope);
        apply_signature_filter(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            new_signature_index.as_ref(),
            old_occurrence,
            new_occurrences,
            &mut plausible,
            CandidatePostingBucket::PairedStream(interval.pair_index),
            None,
            scope,
            |index| {
                new_intervals
                    .get(index)
                    .copied()
                    .flatten()
                    .is_some_and(|candidate| {
                        candidate.pair_index == interval.pair_index && candidate != interval
                    })
            },
        )?;
        probe_direct_watch_pairs(
            watch.as_deref_mut(),
            budget,
            &new_index,
            OccurrenceSide::Old,
            old_candidate.occurrence_index,
            old_occurrence,
            new_occurrences,
            &plausible,
            CandidatePostingBucket::PairedStream(interval.pair_index),
            None,
            RecoveryWatchNearScope::PairedStream,
            |index| {
                new_intervals
                    .get(index)
                    .copied()
                    .flatten()
                    .is_some_and(|candidate| {
                        candidate.pair_index == interval.pair_index && candidate != interval
                    })
            },
        )?;
        let edge_filter = classify_sentence_edge_filter_query_with_index(
            old_occurrence,
            new_occurrences,
            &plausible,
            budget,
            Some((
                &new_index,
                CandidatePostingBucket::PairedStream(interval.pair_index),
                None,
            )),
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: old_candidate.occurrence_index,
                query_side: OccurrenceSide::Old,
                scope,
                shadow: relations.edge_gate_shadow.as_mut(),
                watch: watch.as_deref_mut(),
                watch_scope: RecoveryWatchNearScope::PairedStream,
            }),
            |_| true,
        );
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        edge_filter.record_rejections(
            old_candidate.occurrence_index,
            OccurrenceSide::Old,
            old_occurrence,
            new_occurrences,
            scope,
            relations.edge_gate_shadow.as_mut(),
            watch.as_deref_mut(),
            RecoveryWatchNearScope::PairedStream,
        );
        record_signature_exact_retained(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            budget,
            &edge_filter,
            &plausible,
            old_occurrence,
            new_occurrences,
            OccurrenceSide::Old,
            old_candidate.occurrence_index,
            scope,
        )?;
        let retained_count = edge_filter.len(&plausible);
        if !budget.charge_pair_visits_in_scope(retained_count, old_occurrence.kind, scope) {
            return None;
        }
        for pair_index in 0..retained_count {
            let (new_occurrence_index, cached) = edge_filter.pair(&plausible, pair_index)?;
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let new_candidate_index = new_candidate_by_occurrence
                .get(new_occurrence_index)
                .copied()
                .flatten();
            let score = score_and_record_sentence_edge_gate_shadow(
                old_occurrence,
                new_occurrence,
                budget,
                scope,
                NearSearchWorkClass::Shared,
                None,
                relations.edge_gate_shadow.as_mut(),
                Some(old_candidate_index),
                new_candidate_index,
                cached,
            )?;
            if let Some(watch) = watch.as_mut() {
                watch.record_near(
                    old_candidate.occurrence_index,
                    new_occurrence_index,
                    score,
                    RecoveryWatchNearScope::PairedStream,
                );
            }
            relations
                .old
                .get_mut(old_candidate_index)?
                .record_disqualifying(score);
            if let Some(new_candidate_index) = new_candidate_index {
                relations
                    .new
                    .get_mut(new_candidate_index)?
                    .record_disqualifying(score);
            }
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }

    for (new_candidate_index, new_candidate) in new_candidates.recoveries.iter().enumerate() {
        let interval = *new_candidates.intervals.get(new_candidate_index)?;
        let new_occurrence = new_occurrences.get(new_candidate.occurrence_index)?;
        let query = collect_unit_candidates_unless_direct(
            &old_index,
            &mut plausible,
            new_occurrence,
            old_occurrences,
            CandidatePostingBucket::PairedStream(interval.pair_index),
            None,
            budget,
            scope,
        )?;
        plausible.retain(|occurrence_index| {
            old_candidate_by_occurrence
                .get(*occurrence_index)
                .is_some_and(Option::is_none)
                && old_occurrences[*occurrence_index].kind == new_occurrence.kind
                && occurrence_roles_are_compatible(
                    new_occurrence,
                    &old_occurrences[*occurrence_index],
                )
                && old_intervals
                    .get(*occurrence_index)
                    .copied()
                    .flatten()
                    .is_some_and(|candidate| {
                        candidate.pair_index == interval.pair_index && candidate != interval
                    })
        });
        budget.record_candidate_query_in_scope(query, new_occurrence.kind, plausible.len(), scope);
        apply_signature_filter(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            old_signature_index.as_ref(),
            new_occurrence,
            old_occurrences,
            &mut plausible,
            CandidatePostingBucket::PairedStream(interval.pair_index),
            None,
            scope,
            |index| {
                old_candidate_by_occurrence
                    .get(index)
                    .is_some_and(Option::is_none)
                    && old_intervals
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(|candidate| {
                            candidate.pair_index == interval.pair_index && candidate != interval
                        })
            },
        )?;
        probe_direct_watch_pairs(
            watch.as_deref_mut(),
            budget,
            &old_index,
            OccurrenceSide::New,
            new_candidate.occurrence_index,
            new_occurrence,
            old_occurrences,
            &plausible,
            CandidatePostingBucket::PairedStream(interval.pair_index),
            None,
            RecoveryWatchNearScope::PairedStream,
            |index| {
                old_candidate_by_occurrence
                    .get(index)
                    .is_some_and(Option::is_none)
                    && old_intervals
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(|candidate| {
                            candidate.pair_index == interval.pair_index && candidate != interval
                        })
            },
        )?;
        let edge_filter = classify_sentence_edge_filter_query_with_index(
            new_occurrence,
            old_occurrences,
            &plausible,
            budget,
            Some((
                &old_index,
                CandidatePostingBucket::PairedStream(interval.pair_index),
                None,
            )),
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: new_candidate.occurrence_index,
                query_side: OccurrenceSide::New,
                scope,
                shadow: relations.edge_gate_shadow.as_mut(),
                watch: watch.as_deref_mut(),
                watch_scope: RecoveryWatchNearScope::PairedStream,
            }),
            |_| true,
        );
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        edge_filter.record_rejections(
            new_candidate.occurrence_index,
            OccurrenceSide::New,
            new_occurrence,
            old_occurrences,
            scope,
            relations.edge_gate_shadow.as_mut(),
            watch.as_deref_mut(),
            RecoveryWatchNearScope::PairedStream,
        );
        record_signature_exact_retained(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            budget,
            &edge_filter,
            &plausible,
            new_occurrence,
            old_occurrences,
            OccurrenceSide::New,
            new_candidate.occurrence_index,
            scope,
        )?;
        let retained_count = edge_filter.len(&plausible);
        if !budget.charge_pair_visits_in_scope(retained_count, new_occurrence.kind, scope) {
            return None;
        }
        for pair_index in 0..retained_count {
            let (old_occurrence_index, cached) = edge_filter.pair(&plausible, pair_index)?;
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let score = score_and_record_sentence_edge_gate_shadow(
                old_occurrence,
                new_occurrence,
                budget,
                scope,
                NearSearchWorkClass::Shared,
                None,
                relations.edge_gate_shadow.as_mut(),
                None,
                Some(new_candidate_index),
                cached,
            )?;
            if let Some(watch) = watch.as_mut() {
                watch.record_near(
                    old_occurrence_index,
                    new_candidate.occurrence_index,
                    score,
                    RecoveryWatchNearScope::PairedStream,
                );
            }
            relations
                .new
                .get_mut(new_candidate_index)?
                .record_disqualifying(score);
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }
    Some(())
}

fn reject_crossing_paired_replacements(
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    relations: &mut ModifiedSentenceRelations,
) -> PairedExactExtensionResult<()> {
    reject_crossing_paired_replacements_for(
        old_candidates,
        new_candidates,
        &mut relations.old,
        &mut relations.new,
    )?;
    if let Some(shadow) = relations
        .edge_gate_shadow
        .as_mut()
        .filter(|shadow| shadow.active)
        && let Err(reason) = reject_crossing_paired_replacements_for(
            old_candidates,
            new_candidates,
            &mut shadow.old,
            &mut shadow.new,
        )
    {
        shadow.disable(reason);
    }
    Ok(())
}

fn reject_crossing_paired_replacements_for(
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    old_relations: &mut [CandidateNearRelation],
    new_relations: &mut [CandidateNearRelation],
) -> PairedExactExtensionResult<()> {
    let mut proposals = Vec::new();
    proposals
        .try_reserve_exact(old_relations.len())
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    for (old_index, relation) in old_relations.iter().copied().enumerate() {
        let Some(new_index) = mutual_replacement_partner(old_index, relation, new_relations) else {
            continue;
        };
        let interval = *old_candidates
            .intervals
            .get(old_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        if new_candidates
            .intervals
            .get(new_index)
            .copied()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            != interval
        {
            return Err(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
        }
        proposals.push((
            interval,
            *old_candidates
                .ordinals
                .get(old_index)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
            *new_candidates
                .ordinals
                .get(new_index)
                .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?,
            old_index,
            new_index,
        ));
    }
    proposals.sort_unstable();
    let mut start = 0usize;
    while start < proposals.len() {
        let interval = proposals[start].0;
        let end = start
            .checked_add(proposals[start..].partition_point(|proposal| proposal.0 == interval))
            .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
        if !proposals[start..end]
            .windows(2)
            .all(|pair| pair[0].1 < pair[1].1 && pair[0].2 < pair[1].2)
        {
            for proposal in &proposals[start..end] {
                let old_score = old_relations[proposal.3].best_score;
                let new_score = new_relations[proposal.4].best_score;
                old_relations[proposal.3].record_disqualifying(old_score);
                new_relations[proposal.4].record_disqualifying(new_score);
            }
        }
        start = end;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn modified_sentence_relations(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    paired_streams: &[PairedTrustedStream],
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    watch: Option<&mut RecoveryWatchState>,
) -> Option<ModifiedSentenceRelations> {
    let mut checkpoint = None;
    modified_sentence_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        paired_streams,
        budget,
        diagnostics,
        &mut checkpoint,
        watch,
    )
}

#[allow(clippy::too_many_arguments)]
fn modified_sentence_relations_tracked(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    paired_streams: &[PairedTrustedStream],
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<ModifiedSentenceRelations> {
    let diagnostic_budget = budget.enable_known_span_sentence_shadow.then(|| {
        let mut diagnostic = *budget;
        diagnostic.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Disabled;
        diagnostic
    });
    let mut relations = empty_modified_sentence_relations(old_candidates, new_candidates)?;
    if budget.enable_sentence_edge_gate_shadow {
        enable_sentence_edge_gate_shadow(&mut relations);
    }
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled {
        enable_sentence_edge_signature_shadow(
            &mut relations,
            budget,
            diagnostics,
            signature_checkpoint,
        );
    }
    extend_modified_sentence_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        NearRelationScope::SameOrAmbiguous,
        &mut relations,
        budget,
        diagnostics,
        signature_checkpoint,
        watch.as_deref_mut(),
        None,
    )?;

    let Some(mut cross_relations) = try_clone_modified_sentence_relations(&relations) else {
        if diagnostic_budget.is_some()
            && let Some(diagnostics) = diagnostics.as_mut()
        {
            diagnostics.metrics.known_span_sentence_shadow = None;
        }
        return Some(relations);
    };
    propagate_sentence_edge_gate_clone_failure(&mut relations, &cross_relations);
    let mut cross_budget = *budget;
    let mut cross_diagnostics = *diagnostics;
    if extend_modified_sentence_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        NearRelationScope::CrossSpan,
        &mut cross_relations,
        &mut cross_budget,
        &mut cross_diagnostics,
        signature_checkpoint,
        watch,
        None,
    )
    .is_some()
    {
        cross_relations.complete = true;
        *budget = cross_budget;
        *diagnostics = cross_diagnostics;
        if let Some(diagnostic_budget) = diagnostic_budget {
            record_known_span_sentence_shadow(
                diagnostics,
                old_occurrences,
                new_occurrences,
                old_candidates,
                new_candidates,
                paired_streams,
                diagnostic_budget,
                &cross_relations,
            );
        }
        return Some(cross_relations);
    }

    budget.commit_near_search_spend_from(cross_budget);
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Disabled {
        relations.edge_signature_shadow = cross_relations.edge_signature_shadow;
        if let (Some(diagnostics), Some(cross_diagnostics)) =
            (diagnostics.as_mut(), cross_diagnostics.as_ref())
        {
            diagnostics.metrics.sentence_edge_signature_shadow =
                cross_diagnostics.metrics.sentence_edge_signature_shadow;
            diagnostics.metrics.sentence_edge_signature_direct_shadow = cross_diagnostics
                .metrics
                .sentence_edge_signature_direct_shadow;
        }
    }
    commit_relation_floor_diagnostics_from(diagnostics, cross_diagnostics);
    if let Some(diagnostic_budget) = diagnostic_budget {
        record_known_span_sentence_shadow(
            diagnostics,
            old_occurrences,
            new_occurrences,
            old_candidates,
            new_candidates,
            paired_streams,
            diagnostic_budget,
            &relations,
        );
    }
    Some(relations)
}

fn commit_relation_floor_diagnostics_from(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    other: Option<SentenceRecoveryDiagnostics>,
) {
    let Some(mut diagnostics_value) = *diagnostics else {
        return;
    };
    let Some(other) = other else {
        *diagnostics = None;
        return;
    };
    diagnostics_value.metrics.relation_floor_pairs_considered =
        other.metrics.relation_floor_pairs_considered;
    diagnostics_value.metrics.relation_floor_word_scans = other.metrics.relation_floor_word_scans;
    diagnostics_value.metrics.relation_floor_stop_opportunities =
        other.metrics.relation_floor_stop_opportunities;
    diagnostics_value
        .metrics
        .relation_floor_potential_saved_word_comparisons = other
        .metrics
        .relation_floor_potential_saved_word_comparisons;
    *diagnostics = Some(diagnostics_value);
}

#[derive(Clone, Copy)]
enum NearRelationScope {
    SameOrAmbiguous,
    CrossSpan,
}

fn relation_floor_probe(
    scope: NearRelationScope,
    kind: RecoveryUnitKind,
    old_relation: Option<CandidateNearRelation>,
    new_relation: Option<CandidateNearRelation>,
) -> Option<RelationFloorProbe> {
    if !matches!(scope, NearRelationScope::CrossSpan) || kind != RecoveryUnitKind::Sentence {
        return None;
    }
    let ceiling = match (old_relation, new_relation) {
        (Some(old), Some(new)) => old.second_score.min(new.second_score),
        (Some(old), None) => old.second_score,
        (None, Some(new)) => new.second_score,
        (None, None) => return None,
    };
    Some(RelationFloorProbe::new(ceiling.min(MIN_NEAR_SCORE - 1)))
}

fn commit_relation_floor_probe(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    probe: RelationFloorProbe,
) {
    if !probe.complete {
        *diagnostics = None;
        return;
    }
    let Some(current) = diagnostics.as_ref().map(|diagnostics| diagnostics.metrics) else {
        return;
    };
    let Some(metrics) = (|| {
        let mut metrics = current;
        metrics.relation_floor_pairs_considered =
            metrics.relation_floor_pairs_considered.checked_add(1)?;
        metrics.relation_floor_word_scans = metrics
            .relation_floor_word_scans
            .checked_add(probe.word_scans)?;
        metrics.relation_floor_stop_opportunities = metrics
            .relation_floor_stop_opportunities
            .checked_add(probe.stop_opportunities)?;
        metrics.relation_floor_potential_saved_word_comparisons = metrics
            .relation_floor_potential_saved_word_comparisons
            .checked_add(probe.potential_saved_word_comparisons)?;
        Some(metrics)
    })() else {
        *diagnostics = None;
        return;
    };
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.metrics = metrics;
    }
}

impl NearRelationScope {
    fn includes(self, candidate_span: Option<usize>, occurrence_span: Option<usize>) -> bool {
        match self {
            Self::SameOrAmbiguous => occurrence_span.is_none() || occurrence_span == candidate_span,
            Self::CrossSpan => {
                candidate_span.is_some()
                    && occurrence_span.is_some()
                    && occurrence_span != candidate_span
            }
        }
    }

    fn posting_index_scope(self) -> CandidatePostingIndexScope<'static> {
        match self {
            Self::SameOrAmbiguous => CandidatePostingIndexScope::Span,
            Self::CrossSpan => CandidatePostingIndexScope::Global,
        }
    }

    fn posting_buckets(
        self,
        span_index: Option<usize>,
    ) -> (CandidatePostingBucket, Option<CandidatePostingBucket>) {
        match self {
            Self::SameOrAmbiguous => (
                CandidatePostingBucket::Span(span_index),
                span_index.map(|_| CandidatePostingBucket::Span(None)),
            ),
            Self::CrossSpan => (CandidatePostingBucket::Global, None),
        }
    }
}

fn empty_modified_sentence_relations(
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
) -> Option<ModifiedSentenceRelations> {
    let mut old = Vec::new();
    let mut new = Vec::new();
    old.try_reserve_exact(old_candidates.len()).ok()?;
    new.try_reserve_exact(new_candidates.len()).ok()?;
    old.resize(old_candidates.len(), CandidateNearRelation::default());
    new.resize(new_candidates.len(), CandidateNearRelation::default());
    Some(ModifiedSentenceRelations {
        old,
        new,
        complete: false,
        edge_gate_shadow: None,
        edge_signature_shadow: None,
    })
}

fn enable_sentence_edge_signature_shadow(
    relations: &mut ModifiedSentenceRelations,
    budget: &RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
) {
    let mode = budget.sentence_edge_signature_filter_mode;
    let retained_fingerprint = diagnostics
        .as_ref()
        .map_or_else(SentenceEdgeRetainedFingerprint::default, |diagnostics| {
            diagnostics.signature_retained_fingerprint
        });
    let existing = diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.metrics.sentence_edge_signature_shadow);
    let existing_direct = diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.metrics.sentence_edge_signature_direct_shadow);
    let mut metrics = existing.unwrap_or_default();
    let resumable = existing.is_none()
        || metrics.stop_reason
            == Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete)
        || (metrics.complete && metrics.stop_reason.is_none());
    if resumable {
        metrics.complete = false;
        metrics.stop_reason = None;
    }
    let shadow = SentenceEdgeSignatureShadow::new(budget.token_limit, metrics, mode);
    let mut shadow = shadow.unwrap_or_else(|| {
        let mut shadow = SentenceEdgeSignatureShadow {
            metrics,
            direct_metrics: existing_direct.unwrap_or_default(),
            posting_limit: 0,
            query_limit: 0,
            distinct_key_limit: 0,
            estimated_byte_limit: 0,
            query_count_limit: 0,
            candidate_union_limit: 0,
            active: true,
            mode,
            retained_fingerprint,
        };
        if mode == SentenceEdgeSignatureFilterMode::Direct {
            shadow.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow);
        } else {
            shadow.stop(SentenceEdgeSignatureShadowStopReason::CounterOverflow);
        }
        shadow
    });
    if shadow.active {
        shadow.direct_metrics = existing_direct.unwrap_or_default();
    }
    if !resumable {
        shadow.active = false;
    }
    if mode == SentenceEdgeSignatureFilterMode::Direct
        && shadow.direct_metrics.stop_reason.is_some()
    {
        shadow.active = false;
    }
    shadow.retained_fingerprint = retained_fingerprint;
    relations.edge_signature_shadow = Some(shadow);
    if let Some(shadow) = relations.edge_signature_shadow.as_ref() {
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    }
}

fn begin_sentence_edge_signature_stage(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    mode: SentenceEdgeSignatureFilterMode,
) {
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    if mode == SentenceEdgeSignatureFilterMode::ReferenceObserve {
        diagnostics.signature_retained_fingerprint_valid = false;
        *signature_checkpoint = Some(*diagnostics);
        return;
    }
    let mut metrics = diagnostics
        .metrics
        .sentence_edge_signature_shadow
        .unwrap_or_default();
    metrics.complete = false;
    metrics.parity_evaluable = false;
    metrics.verification_evaluable = false;
    metrics.plan_parity = false;
    metrics.retained_pair_misses = 0;
    if metrics.stop_reason.is_none()
        || metrics.stop_reason
            == Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete)
    {
        metrics.stop_reason =
            Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete);
    }
    diagnostics.metrics.sentence_edge_signature_shadow = Some(metrics);
    diagnostics.signature_retained_fingerprint_valid = false;
    *signature_checkpoint = Some(*diagnostics);
}

fn checkpoint_signature_shadow(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    shadow: &SentenceEdgeSignatureShadow,
) {
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    if shadow.mode == SentenceEdgeSignatureFilterMode::ReferenceObserve {
        diagnostics.metrics.sentence_edge_signature_shadow = Some(shadow.metrics);
        diagnostics.signature_retained_fingerprint = shadow.retained_fingerprint;
        diagnostics.signature_retained_fingerprint_valid = false;
        *signature_checkpoint = Some(*diagnostics);
        return;
    }
    let mut metrics = shadow.metrics;
    metrics.complete = false;
    if shadow.active {
        metrics
            .stop_reason
            .get_or_insert(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete);
    }
    diagnostics.metrics.sentence_edge_signature_shadow = Some(metrics);
    if shadow.mode == SentenceEdgeSignatureFilterMode::Direct {
        let mut direct = shadow.direct_metrics;
        direct.complete = false;
        if !shadow.active && direct.stop_reason.is_none() {
            direct.stop_reason =
                Some(SentenceEdgeSignatureDirectShadowStopReason::ProductionTraversalIncomplete);
        }
        diagnostics.metrics.sentence_edge_signature_direct_shadow = Some(direct);
    }
    diagnostics.signature_retained_fingerprint = shadow.retained_fingerprint;
    diagnostics.signature_retained_fingerprint_valid = false;
    *signature_checkpoint = Some(*diagnostics);
}

fn finalize_reference_observer(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    shadow: Option<&SentenceEdgeSignatureShadow>,
    fragment_veto_complete: bool,
) {
    let (Some(diagnostics), Some(shadow)) = (diagnostics.as_mut(), shadow) else {
        return;
    };
    if shadow.mode != SentenceEdgeSignatureFilterMode::ReferenceObserve {
        return;
    }
    diagnostics.signature_retained_fingerprint = shadow.retained_fingerprint;
    diagnostics.signature_retained_fingerprint_valid = shadow.active
        && shadow.metrics.stop_reason.is_none()
        && diagnostics.metrics.sentence_edge_filter_complete
        && diagnostics.metrics.near_relation_complete
        && !diagnostics.metrics.near_candidate_count_truncated
        && fragment_veto_complete;
}

fn checkpoint_direct_signature_setup_failure(
    budget: &RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
) {
    if budget.sentence_edge_signature_filter_mode != SentenceEdgeSignatureFilterMode::Direct {
        return;
    }
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    let mut metrics = diagnostics
        .metrics
        .sentence_edge_signature_shadow
        .unwrap_or_default();
    metrics.complete = false;
    metrics.stop_reason = Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure);
    diagnostics.metrics.sentence_edge_signature_shadow = Some(metrics);
    let direct = diagnostics
        .metrics
        .sentence_edge_signature_direct_shadow
        .get_or_insert_with(SentenceEdgeSignatureDirectShadowMetrics::default);
    direct.complete = false;
    direct
        .stop_reason
        .get_or_insert(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure);
    diagnostics.signature_retained_fingerprint_valid = false;
    *signature_checkpoint = Some(*diagnostics);
}

fn enable_sentence_edge_gate_shadow(relations: &mut ModifiedSentenceRelations) {
    let buffers = (|| {
        let mut old = Vec::new();
        let mut new = Vec::new();
        old.try_reserve_exact(relations.old.len()).ok()?;
        new.try_reserve_exact(relations.new.len()).ok()?;
        old.resize(relations.old.len(), CandidateNearRelation::default());
        new.resize(relations.new.len(), CandidateNearRelation::default());
        Some((old, new))
    })();
    install_sentence_edge_gate_shadow(relations, buffers);
}

fn install_sentence_edge_gate_shadow(
    relations: &mut ModifiedSentenceRelations,
    buffers: Option<(Vec<CandidateNearRelation>, Vec<CandidateNearRelation>)>,
) {
    relations.edge_gate_shadow = Some(match buffers {
        Some((old, new))
            if old.len() == relations.old.len() && new.len() == relations.new.len() =>
        {
            SentenceEdgeGateShadow {
                old,
                new,
                metrics: SentenceEdgeGateShadowMetrics::default(),
                active: true,
            }
        }
        _ => SentenceEdgeGateShadow::disabled(SentenceEdgeGateShadowStopReason::AllocationFailure),
    });
}

fn try_clone_modified_sentence_relations(
    relations: &ModifiedSentenceRelations,
) -> Option<ModifiedSentenceRelations> {
    let mut old = Vec::new();
    let mut new = Vec::new();
    old.try_reserve_exact(relations.old.len()).ok()?;
    new.try_reserve_exact(relations.new.len()).ok()?;
    old.extend_from_slice(&relations.old);
    new.extend_from_slice(&relations.new);
    let edge_gate_shadow = relations.edge_gate_shadow.as_ref().map(|shadow| {
        if !shadow.active {
            return SentenceEdgeGateShadow {
                old: Vec::new(),
                new: Vec::new(),
                metrics: shadow.metrics,
                active: false,
            };
        }
        let mut shadow_old = Vec::new();
        let mut shadow_new = Vec::new();
        let buffers = if shadow_old.try_reserve_exact(shadow.old.len()).is_ok()
            && shadow_new.try_reserve_exact(shadow.new.len()).is_ok()
        {
            shadow_old.resize(shadow.old.len(), CandidateNearRelation::default());
            shadow_new.resize(shadow.new.len(), CandidateNearRelation::default());
            Some((shadow_old, shadow_new))
        } else {
            None
        };
        clone_sentence_edge_gate_shadow(shadow, buffers)
    });
    Some(ModifiedSentenceRelations {
        old,
        new,
        complete: relations.complete,
        edge_gate_shadow,
        edge_signature_shadow: relations.edge_signature_shadow,
    })
}

fn clone_sentence_edge_gate_shadow(
    shadow: &SentenceEdgeGateShadow,
    buffers: Option<(Vec<CandidateNearRelation>, Vec<CandidateNearRelation>)>,
) -> SentenceEdgeGateShadow {
    let Some((mut old, mut new)) = buffers else {
        let mut failed =
            SentenceEdgeGateShadow::disabled(SentenceEdgeGateShadowStopReason::AllocationFailure);
        failed.metrics = shadow.metrics;
        failed.metrics.complete = false;
        failed.metrics.stop_reason = Some(SentenceEdgeGateShadowStopReason::AllocationFailure);
        return failed;
    };
    if old.len() != shadow.old.len() || new.len() != shadow.new.len() {
        let mut failed =
            SentenceEdgeGateShadow::disabled(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
        failed.metrics = shadow.metrics;
        failed.metrics.complete = false;
        failed.metrics.stop_reason = Some(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
        return failed;
    }
    old.copy_from_slice(&shadow.old);
    new.copy_from_slice(&shadow.new);
    SentenceEdgeGateShadow {
        old,
        new,
        metrics: shadow.metrics,
        active: true,
    }
}

fn propagate_sentence_edge_gate_clone_failure(
    source: &mut ModifiedSentenceRelations,
    cloned: &ModifiedSentenceRelations,
) {
    let Some(cloned_shadow) = cloned
        .edge_gate_shadow
        .as_ref()
        .filter(|shadow| !shadow.active)
    else {
        return;
    };
    let Some(source_shadow) = source
        .edge_gate_shadow
        .as_mut()
        .filter(|shadow| shadow.active)
    else {
        return;
    };
    source_shadow.disable(
        cloned_shadow
            .metrics
            .stop_reason
            .unwrap_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure),
    );
}

#[allow(clippy::too_many_arguments)]
fn extend_modified_sentence_relations(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    scope: NearRelationScope,
    relations: &mut ModifiedSentenceRelations,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    watch: Option<&mut RecoveryWatchState>,
    shadow: Option<KnownSpanSentenceReplay<'_>>,
) -> Option<()> {
    let mut checkpoint = None;
    extend_modified_sentence_relations_tracked(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        scope,
        relations,
        budget,
        diagnostics,
        &mut checkpoint,
        watch,
        shadow,
    )
}

#[allow(clippy::too_many_arguments)]
fn extend_modified_sentence_relations_tracked(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    scope: NearRelationScope,
    relations: &mut ModifiedSentenceRelations,
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    signature_checkpoint: &mut Option<SentenceRecoveryDiagnostics>,
    mut watch: Option<&mut RecoveryWatchState>,
    mut shadow: Option<KnownSpanSentenceReplay<'_>>,
) -> Option<()> {
    if relations.old.len() != old_candidates.len() || relations.new.len() != new_candidates.len() {
        return None;
    }
    let old_candidate_by_occurrence =
        candidate_index_by_occurrence(old_occurrences.len(), old_candidates)?;
    let new_candidate_by_occurrence =
        candidate_index_by_occurrence(new_occurrences.len(), new_candidates)?;
    let Some(old_index) = UnitCandidateIndex::new(old_occurrences, scope.posting_index_scope())
    else {
        checkpoint_direct_signature_setup_failure(budget, diagnostics, signature_checkpoint);
        return None;
    };
    let Some(new_index) = UnitCandidateIndex::new(new_occurrences, scope.posting_index_scope())
    else {
        checkpoint_direct_signature_setup_failure(budget, diagnostics, signature_checkpoint);
        return None;
    };
    let old_signature_index = build_signature_index(
        relations.edge_signature_shadow.as_mut(),
        old_occurrences,
        scope.posting_index_scope(),
    );
    let new_signature_index = build_signature_index(
        relations.edge_signature_shadow.as_mut(),
        new_occurrences,
        scope.posting_index_scope(),
    );
    if let Some(shadow) = relations.edge_signature_shadow.as_ref() {
        checkpoint_signature_shadow(diagnostics, signature_checkpoint, shadow);
    }
    let mut plausible = Vec::new();
    let work_scope = match scope {
        NearRelationScope::SameOrAmbiguous => NearSearchScope::SameOrAmbiguousSpan,
        NearRelationScope::CrossSpan => NearSearchScope::CrossSpan,
    };

    for (old_candidate_index, old_candidate) in old_candidates.iter().enumerate() {
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        let (bucket, additional_bucket) = scope.posting_buckets(old_occurrence.span_index);
        let query = collect_unit_candidates_unless_direct(
            &new_index,
            &mut plausible,
            old_occurrence,
            new_occurrences,
            bucket,
            additional_bucket,
            budget,
            work_scope,
        )?;
        plausible.retain(|occurrence_index| {
            new_occurrences[*occurrence_index].kind == old_occurrence.kind
                && occurrence_roles_are_compatible(
                    old_occurrence,
                    &new_occurrences[*occurrence_index],
                )
                && scope.includes(
                    old_occurrence.span_index,
                    new_occurrences[*occurrence_index].span_index,
                )
        });
        budget.record_candidate_query_in_scope(
            query,
            old_occurrence.kind,
            plausible.len(),
            work_scope,
        );
        apply_signature_filter(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            new_signature_index.as_ref(),
            old_occurrence,
            new_occurrences,
            &mut plausible,
            bucket,
            additional_bucket,
            work_scope,
            |index| {
                new_occurrences.get(index).is_some_and(|candidate| {
                    candidate.kind == old_occurrence.kind
                        && occurrence_roles_are_compatible(old_occurrence, candidate)
                        && scope.includes(old_occurrence.span_index, candidate.span_index)
                })
            },
        )?;
        probe_direct_watch_pairs(
            watch.as_deref_mut(),
            budget,
            &new_index,
            OccurrenceSide::Old,
            old_candidate.occurrence_index,
            old_occurrence,
            new_occurrences,
            &plausible,
            bucket,
            additional_bucket,
            match scope {
                NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
            },
            |index| {
                new_occurrences.get(index).is_some_and(|candidate| {
                    candidate.kind == old_occurrence.kind
                        && occurrence_roles_are_compatible(old_occurrence, candidate)
                        && scope.includes(old_occurrence.span_index, candidate.span_index)
                })
            },
        )?;
        let edge_filter = classify_sentence_edge_filter_query_with_index(
            old_occurrence,
            new_occurrences,
            &plausible,
            budget,
            Some((&new_index, bucket, additional_bucket)),
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: old_candidate.occurrence_index,
                query_side: OccurrenceSide::Old,
                scope: work_scope,
                shadow: relations.edge_gate_shadow.as_mut(),
                watch: watch.as_deref_mut(),
                watch_scope: match scope {
                    NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                    NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
                },
            }),
            |_| true,
        );
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        edge_filter.record_rejections(
            old_candidate.occurrence_index,
            OccurrenceSide::Old,
            old_occurrence,
            new_occurrences,
            work_scope,
            relations.edge_gate_shadow.as_mut(),
            watch.as_deref_mut(),
            match scope {
                NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
            },
        );
        record_signature_exact_retained(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            budget,
            &edge_filter,
            &plausible,
            old_occurrence,
            new_occurrences,
            OccurrenceSide::Old,
            old_candidate.occurrence_index,
            work_scope,
        )?;
        let retained_count = edge_filter.len(&plausible);
        let pair_split = if work_scope == NearSearchScope::SameOrAmbiguousSpan {
            match &edge_filter {
                SentenceEdgeFilterQuery::Legacy => {
                    split_query_candidates(&plausible, old_occurrence.span_index, new_occurrences)?
                }
                SentenceEdgeFilterQuery::Filtered { retained, .. } => {
                    split_retained_sentence_edge_filters(
                        retained,
                        old_occurrence.span_index,
                        new_occurrences,
                    )?
                }
            }
        } else {
            NearSearchWorkSplit::shared(retained_count)
        };
        if !budget.charge_pair_visits_in_scope_split(
            retained_count,
            old_occurrence.kind,
            work_scope,
            pair_split,
        ) {
            return None;
        }
        for pair_index in 0..retained_count {
            let (new_occurrence_index, cached) = edge_filter.pair(&plausible, pair_index)?;
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let new_candidate_index = new_candidate_by_occurrence[new_occurrence_index];
            let mut relation_floor_probe = diagnostics.as_ref().and_then(|_| {
                relation_floor_probe(
                    scope,
                    old_occurrence.kind,
                    relations.old.get(old_candidate_index).copied(),
                    new_candidate_index.and_then(|index| relations.new.get(index).copied()),
                )
            });
            let class =
                same_or_ambiguous_work_class(old_occurrence.span_index, new_occurrence.span_index);
            let score = score_and_record_sentence_edge_gate_shadow(
                old_occurrence,
                new_occurrence,
                budget,
                work_scope,
                class,
                relation_floor_probe.as_mut(),
                relations.edge_gate_shadow.as_mut(),
                Some(old_candidate_index),
                new_candidate_index,
                cached,
            )?;
            if let Some(probe) = relation_floor_probe {
                commit_relation_floor_probe(diagnostics, probe);
            }
            if let Some(watch) = watch.as_deref_mut() {
                watch.record_near(
                    old_candidate.occurrence_index,
                    new_occurrence_index,
                    score,
                    match scope {
                        NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                        NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
                    },
                );
            }
            if let Some(new_candidate_index) = new_candidate_index {
                relations.old[old_candidate_index].record_eligible(new_candidate_index, score);
                relations.new[new_candidate_index].record_eligible(old_candidate_index, score);
                record_known_span_replay_pair(
                    shadow.as_mut(),
                    scope,
                    old_candidate.occurrence_index,
                    new_occurrence_index,
                    old_occurrence,
                    new_occurrence,
                    Some(old_candidate_index),
                    Some(new_candidate_index),
                    score,
                )?;
            } else {
                relations.old[old_candidate_index].record_disqualifying(score);
                record_known_span_replay_pair(
                    shadow.as_mut(),
                    scope,
                    old_candidate.occurrence_index,
                    new_occurrence_index,
                    old_occurrence,
                    new_occurrence,
                    Some(old_candidate_index),
                    None,
                    score,
                )?;
            }
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }

    for (new_candidate_index, new_candidate) in new_candidates.iter().enumerate() {
        let new_occurrence = new_occurrences.get(new_candidate.occurrence_index)?;
        let (bucket, additional_bucket) = scope.posting_buckets(new_occurrence.span_index);
        let query = collect_unit_candidates_unless_direct(
            &old_index,
            &mut plausible,
            new_occurrence,
            old_occurrences,
            bucket,
            additional_bucket,
            budget,
            work_scope,
        )?;
        plausible.retain(|occurrence_index| {
            old_occurrences[*occurrence_index].kind == new_occurrence.kind
                && occurrence_roles_are_compatible(
                    new_occurrence,
                    &old_occurrences[*occurrence_index],
                )
                && scope.includes(
                    new_occurrence.span_index,
                    old_occurrences[*occurrence_index].span_index,
                )
        });
        budget.record_candidate_query_in_scope(
            query,
            new_occurrence.kind,
            plausible.len(),
            work_scope,
        );
        plausible.retain(|index| old_candidate_by_occurrence[*index].is_none());
        apply_signature_filter(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            old_signature_index.as_ref(),
            new_occurrence,
            old_occurrences,
            &mut plausible,
            bucket,
            additional_bucket,
            work_scope,
            |index| {
                old_candidate_by_occurrence
                    .get(index)
                    .is_some_and(Option::is_none)
                    && old_occurrences.get(index).is_some_and(|candidate| {
                        candidate.kind == new_occurrence.kind
                            && occurrence_roles_are_compatible(new_occurrence, candidate)
                            && scope.includes(new_occurrence.span_index, candidate.span_index)
                    })
            },
        )?;
        probe_direct_watch_pairs(
            watch.as_deref_mut(),
            budget,
            &old_index,
            OccurrenceSide::New,
            new_candidate.occurrence_index,
            new_occurrence,
            old_occurrences,
            &plausible,
            bucket,
            additional_bucket,
            match scope {
                NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
            },
            |index| {
                old_candidate_by_occurrence
                    .get(index)
                    .is_some_and(Option::is_none)
                    && old_occurrences.get(index).is_some_and(|candidate| {
                        candidate.kind == new_occurrence.kind
                            && occurrence_roles_are_compatible(new_occurrence, candidate)
                            && scope.includes(new_occurrence.span_index, candidate.span_index)
                    })
            },
        )?;
        let edge_filter = classify_sentence_edge_filter_query_with_index(
            new_occurrence,
            old_occurrences,
            &plausible,
            budget,
            Some((&old_index, bucket, additional_bucket)),
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: new_candidate.occurrence_index,
                query_side: OccurrenceSide::New,
                scope: work_scope,
                shadow: relations.edge_gate_shadow.as_mut(),
                watch: watch.as_deref_mut(),
                watch_scope: match scope {
                    NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                    NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
                },
            }),
            |index| old_candidate_by_occurrence[index].is_none(),
        );
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        edge_filter.record_rejections(
            new_candidate.occurrence_index,
            OccurrenceSide::New,
            new_occurrence,
            old_occurrences,
            work_scope,
            relations.edge_gate_shadow.as_mut(),
            watch.as_deref_mut(),
            match scope {
                NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
            },
        );
        plausible.retain(|index| old_candidate_by_occurrence[*index].is_none());
        record_signature_exact_retained(
            relations.edge_signature_shadow.as_mut(),
            diagnostics,
            signature_checkpoint,
            budget,
            &edge_filter,
            &plausible,
            new_occurrence,
            old_occurrences,
            OccurrenceSide::New,
            new_candidate.occurrence_index,
            work_scope,
        )?;
        let retained_count = edge_filter.len(&plausible);
        let pair_split = if work_scope == NearSearchScope::SameOrAmbiguousSpan {
            match &edge_filter {
                SentenceEdgeFilterQuery::Legacy => {
                    split_query_candidates(&plausible, new_occurrence.span_index, old_occurrences)?
                }
                SentenceEdgeFilterQuery::Filtered { retained, .. } => {
                    split_retained_sentence_edge_filters(
                        retained,
                        new_occurrence.span_index,
                        old_occurrences,
                    )?
                }
            }
        } else {
            NearSearchWorkSplit::shared(retained_count)
        };
        if !budget.charge_pair_visits_in_scope_split(
            retained_count,
            new_occurrence.kind,
            work_scope,
            pair_split,
        ) {
            return None;
        }
        for pair_index in 0..retained_count {
            let (old_occurrence_index, cached) = edge_filter.pair(&plausible, pair_index)?;
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let mut relation_floor_probe = diagnostics.as_ref().and_then(|_| {
                relation_floor_probe(
                    scope,
                    old_occurrence.kind,
                    None,
                    relations.new.get(new_candidate_index).copied(),
                )
            });
            let class =
                same_or_ambiguous_work_class(new_occurrence.span_index, old_occurrence.span_index);
            let score = score_and_record_sentence_edge_gate_shadow(
                old_occurrence,
                new_occurrence,
                budget,
                work_scope,
                class,
                relation_floor_probe.as_mut(),
                relations.edge_gate_shadow.as_mut(),
                None,
                Some(new_candidate_index),
                cached,
            )?;
            if let Some(probe) = relation_floor_probe {
                commit_relation_floor_probe(diagnostics, probe);
            }
            if let Some(watch) = watch.as_deref_mut() {
                watch.record_near(
                    old_occurrence_index,
                    new_candidate.occurrence_index,
                    score,
                    match scope {
                        NearRelationScope::SameOrAmbiguous => RecoveryWatchNearScope::SameSpan,
                        NearRelationScope::CrossSpan => RecoveryWatchNearScope::CrossSpan,
                    },
                );
            }
            relations.new[new_candidate_index].record_disqualifying(score);
            record_known_span_replay_pair(
                shadow.as_mut(),
                scope,
                old_occurrence_index,
                new_candidate.occurrence_index,
                old_occurrence,
                new_occurrence,
                None,
                Some(new_candidate_index),
                score,
            )?;
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn record_known_span_replay_pair(
    replay: Option<&mut KnownSpanSentenceReplay<'_>>,
    scope: NearRelationScope,
    old_occurrence_index: usize,
    new_occurrence_index: usize,
    old_occurrence: &SentenceOccurrence,
    new_occurrence: &SentenceOccurrence,
    old_candidate_index: Option<usize>,
    new_candidate_index: Option<usize>,
    score: u16,
) -> Option<bool> {
    let Some(replay) = replay else {
        return Some(true);
    };
    let retained = if old_occurrence.kind == RecoveryUnitKind::Sentence {
        let retained = match scope {
            NearRelationScope::SameOrAmbiguous => {
                old_occurrence.span_index.is_some() && new_occurrence.span_index.is_some()
            }
            NearRelationScope::CrossSpan => {
                let locality = sentence_pair_locality(
                    replay.old_intervals.get(old_occurrence_index).copied()?,
                    replay.new_intervals.get(new_occurrence_index).copied()?,
                    old_occurrence.page,
                    new_occurrence.page,
                );
                replay.metrics.cross_span_pairs_considered =
                    replay.metrics.cross_span_pairs_considered.checked_add(1)?;
                record_sentence_pair_locality(replay.metrics, locality)?;
                locality == SentencePairLocality::SamePairedAnchorInterval
            }
        };
        replay.metrics.pairs_considered = replay.metrics.pairs_considered.checked_add(1)?;
        let counter = if retained {
            &mut replay.metrics.pairs_retained
        } else {
            &mut replay.metrics.pairs_rejected
        };
        *counter = counter.checked_add(1)?;
        retained
    } else {
        true
    };
    if retained {
        match (old_candidate_index, new_candidate_index) {
            (Some(old_index), Some(new_index)) => {
                replay.relations.old[old_index].record_eligible(new_index, score);
                replay.relations.new[new_index].record_eligible(old_index, score);
            }
            (Some(old_index), None) => {
                replay.relations.old[old_index].record_disqualifying(score);
            }
            (None, Some(new_index)) => {
                replay.relations.new[new_index].record_disqualifying(score);
            }
            (None, None) => return None,
        }
    }
    Some(retained)
}

fn sentence_pair_locality(
    old_interval: Option<PairedInterval>,
    new_interval: Option<PairedInterval>,
    old_page: Option<u32>,
    new_page: Option<u32>,
) -> SentencePairLocality {
    match (old_interval, new_interval) {
        (Some(old), Some(new)) if old == new => SentencePairLocality::SamePairedAnchorInterval,
        (Some(old), Some(new)) if old.pair_index == new.pair_index => {
            SentencePairLocality::SamePairedStreamOtherInterval
        }
        (Some(_), Some(_)) => SentencePairLocality::Unclassified,
        _ if old_page.is_some() && old_page == new_page => SentencePairLocality::SamePageOnly,
        _ => SentencePairLocality::Unclassified,
    }
}

fn record_sentence_pair_locality(
    metrics: &mut KnownSpanSentenceShadowMetrics,
    locality: SentencePairLocality,
) -> Option<()> {
    let counter = match locality {
        SentencePairLocality::SamePairedAnchorInterval => {
            &mut metrics.same_paired_anchor_interval_pairs
        }
        SentencePairLocality::SamePairedStreamOtherInterval => {
            &mut metrics.same_paired_stream_other_interval_pairs
        }
        SentencePairLocality::SamePageOnly => &mut metrics.same_page_only_pairs,
        SentencePairLocality::Unclassified => &mut metrics.unclassified_pairs,
    };
    *counter = counter.checked_add(1)?;
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn record_known_span_sentence_shadow(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    paired_streams: &[PairedTrustedStream],
    diagnostic_budget: RecoveryBudget,
    production: &ModifiedSentenceRelations,
) {
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    let Some((baseline_relations, shadow_relations, mut metrics)) =
        replay_known_span_sentence_shadow(
            old_occurrences,
            new_occurrences,
            old_candidates,
            new_candidates,
            paired_streams,
            diagnostic_budget,
        )
    else {
        diagnostics.metrics.known_span_sentence_shadow = None;
        return;
    };
    if baseline_relations.complete != production.complete
        || baseline_relations.old != production.old
        || baseline_relations.new != production.new
    {
        diagnostics.metrics.known_span_sentence_shadow = None;
        return;
    }
    if production.old.len() != shadow_relations.old.len()
        || production.new.len() != shadow_relations.new.len()
    {
        diagnostics.metrics.known_span_sentence_shadow = None;
        return;
    }
    metrics.complete &= production.complete && shadow_relations.complete;
    for (production, shadow_relation) in production.old.iter().zip(&shadow_relations.old) {
        if !compare_known_span_shadow_relation(production, shadow_relation, &mut metrics, true) {
            diagnostics.metrics.known_span_sentence_shadow = None;
            return;
        }
    }
    for (production, shadow_relation) in production.new.iter().zip(&shadow_relations.new) {
        if !compare_known_span_shadow_relation(production, shadow_relation, &mut metrics, false) {
            diagnostics.metrics.known_span_sentence_shadow = None;
            return;
        }
    }
    for (old_index, production_relation) in production.old.iter().copied().enumerate() {
        let production_partner =
            mutual_replacement_partner(old_index, production_relation, &production.new);
        let Some(shadow_relation) = shadow_relations.old.get(old_index).copied() else {
            diagnostics.metrics.known_span_sentence_shadow = None;
            return;
        };
        let shadow_partner =
            mutual_replacement_partner(old_index, shadow_relation, &shadow_relations.new);
        let Some(next) = metrics
            .reciprocal_pair_mismatches
            .checked_add(usize::from(production_partner != shadow_partner))
        else {
            diagnostics.metrics.known_span_sentence_shadow = None;
            return;
        };
        metrics.reciprocal_pair_mismatches = next;
    }
    metrics.exact_relation_parity =
        metrics.old_relation_mismatches == 0 && metrics.new_relation_mismatches == 0;
    diagnostics.metrics.known_span_sentence_shadow = Some(metrics);
}

fn replay_known_span_sentence_shadow(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    paired_streams: &[PairedTrustedStream],
    mut budget: RecoveryBudget,
) -> Option<(
    ModifiedSentenceRelations,
    ModifiedSentenceRelations,
    KnownSpanSentenceShadowMetrics,
)> {
    let (old_pair_by_stream, new_pair_by_stream) = paired_stream_indices(paired_streams)?;
    let old_intervals = paired_intervals_for_occurrences(
        old_occurrences,
        paired_streams,
        &old_pair_by_stream,
        OccurrenceSide::Old,
    )?;
    let new_intervals = paired_intervals_for_occurrences(
        new_occurrences,
        paired_streams,
        &new_pair_by_stream,
        OccurrenceSide::New,
    )?;
    let mut baseline_relations = empty_modified_sentence_relations(old_candidates, new_candidates)?;
    let mut shadow_relations = empty_modified_sentence_relations(old_candidates, new_candidates)?;
    let mut metrics = KnownSpanSentenceShadowMetrics::default();
    let mut diagnostics = None;
    extend_modified_sentence_relations(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        NearRelationScope::SameOrAmbiguous,
        &mut baseline_relations,
        &mut budget,
        &mut diagnostics,
        None,
        Some(KnownSpanSentenceReplay {
            relations: &mut shadow_relations,
            metrics: &mut metrics,
            old_intervals: &old_intervals,
            new_intervals: &new_intervals,
        }),
    )?;

    let mut cross_baseline = try_clone_modified_sentence_relations(&baseline_relations)?;
    let mut cross_shadow = try_clone_modified_sentence_relations(&shadow_relations)?;
    let mut cross_metrics = metrics;
    let mut cross_budget = budget;
    if extend_modified_sentence_relations(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        NearRelationScope::CrossSpan,
        &mut cross_baseline,
        &mut cross_budget,
        &mut diagnostics,
        None,
        Some(KnownSpanSentenceReplay {
            relations: &mut cross_shadow,
            metrics: &mut cross_metrics,
            old_intervals: &old_intervals,
            new_intervals: &new_intervals,
        }),
    )
    .is_some()
    {
        cross_baseline.complete = true;
        cross_shadow.complete = true;
        cross_metrics.complete = true;
        return Some((cross_baseline, cross_shadow, cross_metrics));
    }

    metrics.complete = false;
    Some((baseline_relations, shadow_relations, metrics))
}

fn compare_known_span_shadow_relation(
    production: &CandidateNearRelation,
    shadow: &CandidateNearRelation,
    metrics: &mut KnownSpanSentenceShadowMetrics,
    old_side: bool,
) -> bool {
    if production != shadow {
        let relation_mismatches = if old_side {
            &mut metrics.old_relation_mismatches
        } else {
            &mut metrics.new_relation_mismatches
        };
        let Some(next) = relation_mismatches.checked_add(1) else {
            return false;
        };
        *relation_mismatches = next;
    }
    increment_shadow_mismatch(
        &mut metrics.best_partner_mismatches,
        production.best_partner != shadow.best_partner,
    ) && increment_shadow_mismatch(
        &mut metrics.best_score_mismatches,
        production.best_score != shadow.best_score,
    ) && increment_shadow_mismatch(
        &mut metrics.second_score_mismatches,
        production.second_score != shadow.second_score,
    ) && increment_shadow_mismatch(
        &mut metrics.veto_mismatches,
        production.vetoed() != shadow.vetoed(),
    ) && increment_shadow_mismatch(
        &mut metrics.unique_partner_mismatches,
        production.unique_partner() != shadow.unique_partner(),
    )
}

fn increment_shadow_mismatch(counter: &mut usize, differs: bool) -> bool {
    let Some(next) = counter.checked_add(usize::from(differs)) else {
        return false;
    };
    *counter = next;
    true
}

fn record_sentence_edge_gate_shadow(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    relations: &ModifiedSentenceRelations,
    stop_reason: Option<NearRelationStopReason>,
) {
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    if let Some(shadow) = relations.edge_gate_shadow.as_ref() {
        let mut metrics = shadow.metrics;
        if shadow.active {
            metrics.complete = relations.complete && stop_reason.is_none();
            if let Some(reason) = stop_reason {
                metrics.stop_reason = Some(reason.into());
            } else if !relations.complete {
                metrics.stop_reason = Some(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
            }
            if let Err(reason) =
                compare_sentence_edge_gate_decisions(relations, shadow, &mut metrics)
            {
                metrics.complete = false;
                metrics.stop_reason = Some(reason);
            }
        }
        merge_sentence_edge_gate_shadow_into(&mut diagnostics.metrics, metrics);
    }
    if let Some(shadow) = relations.edge_signature_shadow.as_ref() {
        let mut metrics = shadow.metrics;
        metrics.complete = shadow.active && relations.complete && stop_reason.is_none();
        if let Some(reason) = stop_reason {
            metrics.stop_reason.get_or_insert(match reason {
                NearRelationStopReason::CandidatePostingVisitLimit => {
                    SentenceEdgeSignatureShadowStopReason::CandidatePostingVisitLimit
                }
                NearRelationStopReason::PairVisitLimit => {
                    SentenceEdgeSignatureShadowStopReason::PairVisitLimit
                }
                NearRelationStopReason::SimilarityComparisonLimit => {
                    SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit
                }
                NearRelationStopReason::CandidateCountLimit => {
                    SentenceEdgeSignatureShadowStopReason::CandidateCountLimit
                }
            });
        } else if !relations.complete && metrics.stop_reason.is_none() {
            metrics.stop_reason =
                Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete);
        }
        diagnostics.metrics.sentence_edge_signature_shadow = Some(metrics);
    }
}

fn compare_sentence_edge_gate_decisions(
    relations: &ModifiedSentenceRelations,
    shadow: &SentenceEdgeGateShadow,
    metrics: &mut SentenceEdgeGateShadowMetrics,
) -> std::result::Result<(), SentenceEdgeGateShadowStopReason> {
    if relations.old.len() != shadow.old.len() || relations.new.len() != shadow.new.len() {
        return Err(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
    }
    for (production, projected) in relations
        .old
        .iter()
        .chain(&relations.new)
        .zip(shadow.old.iter().chain(&shadow.new))
    {
        increment_edge_gate_mismatch(
            &mut metrics.veto_mismatches,
            production.vetoed() != projected.vetoed(),
        )?;
        increment_edge_gate_mismatch(
            &mut metrics.insertion_deletion_veto_mismatches,
            production.vetoed() != projected.vetoed(),
        )?;
        increment_edge_gate_mismatch(
            &mut metrics.unique_partner_mismatches,
            production.unique_partner() != projected.unique_partner(),
        )?;
    }
    for (old_index, production) in relations.old.iter().copied().enumerate() {
        let production_partner = mutual_replacement_partner(old_index, production, &relations.new);
        let projected = shadow
            .old
            .get(old_index)
            .copied()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let projected_partner = mutual_replacement_partner(old_index, projected, &shadow.new);
        increment_edge_gate_mismatch(
            &mut metrics.reciprocal_pair_mismatches,
            production_partner != projected_partner,
        )?;
        increment_edge_gate_mismatch(
            &mut metrics.adopted_replacement_mismatches,
            production_partner != projected_partner,
        )?;
    }
    Ok(())
}

fn increment_edge_gate_mismatch(
    counter: &mut usize,
    differs: bool,
) -> std::result::Result<(), SentenceEdgeGateShadowStopReason> {
    *counter = counter
        .checked_add(usize::from(differs))
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    Ok(())
}

fn mark_sentence_edge_gate_shadow_incomplete(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    enabled: bool,
    stop_reason: Option<NearRelationStopReason>,
) {
    let reason = stop_reason.map_or(
        SentenceEdgeGateShadowStopReason::DiagnosticFailure,
        Into::into,
    );
    mark_sentence_edge_gate_shadow_incomplete_with_reason(diagnostics, enabled, reason);
}

fn mark_sentence_edge_gate_shadow_incomplete_with_reason(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    enabled: bool,
    reason: SentenceEdgeGateShadowStopReason,
) {
    if !enabled {
        return;
    }
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    let mut metrics = diagnostics
        .metrics
        .sentence_edge_gate_shadow
        .unwrap_or_default();
    metrics.complete = false;
    metrics.stop_reason.get_or_insert(reason);
    diagnostics.metrics.sentence_edge_gate_shadow = Some(metrics);
}

fn merge_sentence_edge_gate_shadow_into(
    metrics: &mut SentenceRecoveryMetrics,
    incoming: SentenceEdgeGateShadowMetrics,
) {
    let Some(previous) = metrics.sentence_edge_gate_shadow else {
        metrics.sentence_edge_gate_shadow = Some(incoming);
        return;
    };
    metrics.sentence_edge_gate_shadow = Some(
        merge_sentence_edge_gate_shadow(previous, incoming).unwrap_or_else(|| {
            let mut failed = previous;
            failed.complete = false;
            failed.stop_reason = Some(SentenceEdgeGateShadowStopReason::CounterOverflow);
            failed
        }),
    );
}

fn merge_sentence_edge_gate_shadow(
    mut left: SentenceEdgeGateShadowMetrics,
    right: SentenceEdgeGateShadowMetrics,
) -> Option<SentenceEdgeGateShadowMetrics> {
    macro_rules! add {
        ($field:ident) => {
            left.$field = left.$field.checked_add(right.$field)?;
        };
    }
    add!(pairs_considered);
    add!(pairs_retained);
    add!(pairs_rejected);
    add!(same_known_rejected);
    add!(ambiguous_rejected);
    add!(cross_span_rejected);
    add!(unclassified_rejected);
    add!(projected_pair_visits);
    add!(projected_similarity_comparisons);
    add!(threshold_violations);
    add!(veto_mismatches);
    add!(unique_partner_mismatches);
    add!(reciprocal_pair_mismatches);
    add!(adopted_replacement_mismatches);
    add!(insertion_deletion_veto_mismatches);
    left.rejected_max_production_score = left
        .rejected_max_production_score
        .max(right.rejected_max_production_score);
    left.complete &= right.complete;
    left.stop_reason = left.stop_reason.or(right.stop_reason);
    Some(left)
}

fn occurrence_roles_are_compatible(left: &SentenceOccurrence, right: &SentenceOccurrence) -> bool {
    left.role.is_some_and(|left_role| {
        right
            .role
            .is_some_and(|right_role| left_role.is_alignment_compatible(right_role))
    })
}

fn candidate_index_by_occurrence(
    occurrence_count: usize,
    candidates: &[RecoveryCandidate],
) -> Option<Vec<Option<usize>>> {
    let mut indices = Vec::new();
    indices.try_reserve_exact(occurrence_count).ok()?;
    indices.resize(occurrence_count, None);
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let slot = indices.get_mut(candidate.occurrence_index)?;
        if slot.replace(candidate_index).is_some() {
            return None;
        }
    }
    Some(indices)
}

fn record_near_pair(diagnostics: &mut Option<SentenceRecoveryDiagnostics>) {
    let failed = diagnostics.as_mut().is_some_and(|diagnostics| {
        let Some(next) = diagnostics.metrics.near_pair_candidates.checked_add(1) else {
            return true;
        };
        diagnostics.metrics.near_pair_candidates = next;
        false
    });
    if failed {
        *diagnostics = None;
    }
}

fn record_vetoed_near_pairs(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    relations: &ModifiedSentenceRelations,
    near_pair_start: usize,
) {
    let Some(near_pair_candidates) = diagnostics
        .as_ref()
        .map(|diagnostics| diagnostics.metrics.near_pair_candidates)
    else {
        return;
    };
    let adopted = relations.old.iter().copied().enumerate().try_fold(
        0usize,
        |count, (old_candidate_index, relation)| {
            if mutual_replacement_partner(old_candidate_index, relation, &relations.new).is_some() {
                count.checked_add(1)
            } else {
                Some(count)
            }
        },
    );
    let Some(vetoed) = adopted
        .and_then(|adopted| {
            near_pair_candidates
                .checked_sub(near_pair_start)?
                .checked_sub(adopted)
        })
        .and_then(|vetoed| {
            diagnostics
                .as_ref()?
                .metrics
                .vetoed_near_pairs
                .checked_add(vetoed)
        })
    else {
        *diagnostics = None;
        return;
    };
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.metrics.vetoed_near_pairs = vetoed;
    }
}

fn append_exact_matches(
    plan: &mut SentenceRecoveryPlan,
    old_occurrences: &mut [SentenceOccurrence],
    new_occurrences: &mut [SentenceOccurrence],
    candidates: &[ExactMatchCandidate],
    budget: &mut RecoveryBudget,
) -> Option<()> {
    let mut source_tokens = 0usize;
    let mut old_consumed_count = 0usize;
    let mut new_consumed_count = 0usize;
    let mut same_span_count = 0usize;
    let mut cross_span_count = 0usize;
    for candidate in candidates {
        let old_location = old_occurrences
            .get(candidate.old_occurrence_index)?
            .location
            .as_ref()?;
        let new_location = new_occurrences
            .get(candidate.new_occurrence_index)?
            .location
            .as_ref()?;
        if old_location.recovery.span_index != candidate.old_span_index
            || new_location.recovery.span_index != candidate.new_span_index
        {
            return None;
        }
        if candidate.old_span_index == candidate.new_span_index {
            same_span_count = same_span_count.checked_add(1)?;
        } else {
            cross_span_count = cross_span_count.checked_add(1)?;
        }
        source_tokens = source_tokens
            .checked_add(old_location.recovery.source_tokens)?
            .checked_add(new_location.recovery.source_tokens)?;
        old_consumed_count = old_consumed_count.checked_add(old_location.consumed.len())?;
        new_consumed_count = new_consumed_count.checked_add(new_location.consumed.len())?;
    }

    if !budget.charge_outputs(candidates.len().checked_mul(2)?, source_tokens) {
        return None;
    }
    plan.matches.try_reserve_exact(same_span_count).ok()?;
    plan.cross_span_match_old
        .try_reserve_exact(cross_span_count)
        .ok()?;
    plan.cross_span_match_new
        .try_reserve_exact(cross_span_count)
        .ok()?;
    plan.deletion_consumed
        .try_reserve_exact(old_consumed_count)
        .ok()?;
    plan.insertion_consumed
        .try_reserve_exact(new_consumed_count)
        .ok()?;

    for candidate in candidates {
        let old_location = old_occurrences
            .get_mut(candidate.old_occurrence_index)?
            .location
            .take()?;
        let new_location = new_occurrences
            .get_mut(candidate.new_occurrence_index)?
            .location
            .take()?;
        if candidate.old_span_index == candidate.new_span_index {
            plan.matches.push(RecoveredExactMatch {
                old: old_location.recovery,
                new: new_location.recovery,
            });
        } else {
            plan.cross_span_match_old.push(old_location.recovery);
            plan.cross_span_match_new.push(new_location.recovery);
        }
        plan.deletion_consumed.extend(old_location.consumed);
        plan.insertion_consumed.extend(new_location.consumed);
    }
    plan.cross_span_match_old
        .sort_unstable_by_key(|recovery| recovery.span_index);
    plan.cross_span_match_new
        .sort_unstable_by_key(|recovery| recovery.span_index);
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn append_replacements(
    plan: &mut SentenceRecoveryPlan,
    old_occurrences: &mut [SentenceOccurrence],
    new_occurrences: &mut [SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    relations: &ModifiedSentenceRelations,
    budget: &mut RecoveryBudget,
) -> Option<()> {
    append_replacements_typed(
        plan,
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        relations,
        budget,
    )
    .ok()
}

#[allow(clippy::too_many_arguments)]
fn append_replacements_typed(
    plan: &mut SentenceRecoveryPlan,
    old_occurrences: &mut [SentenceOccurrence],
    new_occurrences: &mut [SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    relations: &ModifiedSentenceRelations,
    budget: &mut RecoveryBudget,
) -> PairedExactExtensionResult<()> {
    if old_candidates.len() != relations.old.len() || new_candidates.len() != relations.new.len() {
        return Err(SentenceEdgeGateShadowStopReason::DiagnosticFailure);
    }

    let mut replacement_count = 0usize;
    let mut source_tokens = 0usize;
    let mut old_consumed_count = 0usize;
    let mut new_consumed_count = 0usize;
    for (old_candidate_index, old_relation) in relations.old.iter().copied().enumerate() {
        let Some(new_candidate_index) =
            mutual_replacement_partner(old_candidate_index, old_relation, &relations.new)
        else {
            continue;
        };
        let old_candidate = old_candidates
            .get(old_candidate_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let new_candidate = new_candidates
            .get(new_candidate_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let old_location = old_occurrences
            .get(old_candidate.occurrence_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            .location
            .as_ref()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let new_location = new_occurrences
            .get(new_candidate.occurrence_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            .location
            .as_ref()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        replacement_count = replacement_count
            .checked_add(1)
            .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
        source_tokens = source_tokens
            .checked_add(old_location.recovery.source_tokens)
            .and_then(|count| count.checked_add(new_location.recovery.source_tokens))
            .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
        old_consumed_count = old_consumed_count
            .checked_add(old_location.consumed.len())
            .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
        new_consumed_count = new_consumed_count
            .checked_add(new_location.consumed.len())
            .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    }

    let output_ranges = replacement_count
        .checked_mul(2)
        .ok_or(SentenceEdgeGateShadowStopReason::CounterOverflow)?;
    if !budget.charge_outputs(output_ranges, source_tokens) {
        return Err(SentenceEdgeGateShadowStopReason::CandidateCountLimit);
    }
    plan.replacements
        .try_reserve_exact(replacement_count)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    plan.cross_span_replacement_new_spans
        .try_reserve_exact(replacement_count)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    plan.deletion_consumed
        .try_reserve_exact(old_consumed_count)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;
    plan.insertion_consumed
        .try_reserve_exact(new_consumed_count)
        .map_err(|_| SentenceEdgeGateShadowStopReason::AllocationFailure)?;

    for (old_candidate_index, old_relation) in relations.old.iter().copied().enumerate() {
        let Some(new_candidate_index) =
            mutual_replacement_partner(old_candidate_index, old_relation, &relations.new)
        else {
            continue;
        };
        let old_occurrence_index = old_candidates
            .get(old_candidate_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            .occurrence_index;
        let new_occurrence_index = new_candidates
            .get(new_candidate_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            .occurrence_index;
        let old_location = old_occurrences
            .get_mut(old_occurrence_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            .location
            .take()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        let new_location = new_occurrences
            .get_mut(new_occurrence_index)
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?
            .location
            .take()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        plan.replacements.push(RecoveredReplacement {
            old: old_location.recovery,
            new: new_location.recovery,
        });
        let replacement = plan
            .replacements
            .last()
            .ok_or(SentenceEdgeGateShadowStopReason::DiagnosticFailure)?;
        if replacement.old.span_index != replacement.new.span_index {
            plan.cross_span_replacement_new_spans
                .push(replacement.new.span_index);
        }
        plan.deletion_consumed.extend(old_location.consumed);
        plan.insertion_consumed.extend(new_location.consumed);
    }
    plan.replacements.sort_unstable_by_key(|replacement| {
        (
            replacement.old.span_index,
            replacement.new.span_index,
            replacement.old.comparable.start,
            replacement.new.comparable.start,
        )
    });
    plan.cross_span_replacement_new_spans.sort_unstable();
    plan.cross_span_replacement_new_spans.dedup();
    Ok(())
}

fn mutual_replacement_partner(
    old_candidate_index: usize,
    old_relation: CandidateNearRelation,
    new_relations: &[CandidateNearRelation],
) -> Option<usize> {
    let new_candidate_index = old_relation.unique_partner()?;
    (new_relations.get(new_candidate_index)?.unique_partner()? == old_candidate_index)
        .then_some(new_candidate_index)
}

fn append_candidate_recoveries(
    recoveries: &mut Vec<RecoveredSentence>,
    consumed: &mut Vec<LocalSentenceRange>,
    occurrences: &mut [SentenceOccurrence],
    candidates: &[RecoveryCandidate],
    relations: &[CandidateNearRelation],
    budget: &mut RecoveryBudget,
) -> Option<()> {
    if candidates.len() != relations.len() {
        return None;
    }
    let mut retained_count = 0usize;
    let mut retained_tokens = 0usize;
    let mut consumed_count = 0usize;
    for (candidate, relation) in candidates.iter().zip(relations) {
        if relation.vetoed() {
            continue;
        }
        let occurrence = occurrences.get(candidate.occurrence_index)?;
        if occurrence.kind == RecoveryUnitKind::Line
            && !matches!(
                occurrence.role,
                Some(BlockRole::RepeatedHeader | BlockRole::RepeatedFooter)
            )
        {
            continue;
        }
        let location = occurrence.location.as_ref()?;
        retained_count = retained_count.checked_add(1)?;
        retained_tokens = retained_tokens.checked_add(location.recovery.source_tokens)?;
        consumed_count = consumed_count.checked_add(location.consumed.len())?;
    }
    if !budget.charge_outputs(retained_count, retained_tokens) {
        return None;
    }
    recoveries.try_reserve_exact(retained_count).ok()?;
    consumed.try_reserve_exact(consumed_count).ok()?;
    for (candidate, relation) in candidates.iter().zip(relations) {
        if relation.vetoed() {
            continue;
        }
        let occurrence = occurrences.get_mut(candidate.occurrence_index)?;
        if occurrence.kind == RecoveryUnitKind::Line
            && !matches!(
                occurrence.role,
                Some(BlockRole::RepeatedHeader | BlockRole::RepeatedFooter)
            )
        {
            continue;
        }
        let location = occurrence.location.take()?;
        recoveries.push(location.recovery);
        consumed.extend(location.consumed);
    }
    Some(())
}

fn normalize_ranges(ranges: &mut [LocalSentenceRange]) -> bool {
    ranges
        .sort_unstable_by_key(|range| (range.block, range.comparable.start, range.comparable.end));
    !ranges.windows(2).any(|pair| {
        pair[0].block == pair[1].block && pair[0].comparable.end > pair[1].comparable.start
    })
}

fn sentence_boundaries(
    text: &str,
    forced: &[ForcedSentenceBoundary],
    budget: &mut RecoveryBudget,
) -> Option<Vec<SentenceBoundary>> {
    let mut boundaries = Vec::new();
    let mut byte_offset = 0usize;
    let mut scalar_offset = 0usize;
    for forced_boundary in forced {
        if forced_boundary.byte_offset <= byte_offset
            || forced_boundary.scalar_offset <= scalar_offset
        {
            return None;
        }
        let segment = text.get(byte_offset..forced_boundary.byte_offset)?;
        let scalar_end = scalar_offset.checked_add(segment.chars().count())?;
        if scalar_end != forced_boundary.scalar_offset {
            return None;
        }
        append_sentence_boundaries(segment, byte_offset, scalar_offset, &mut boundaries, budget)?;
        if forced_boundary.include_trailing_unit {
            append_trailing_unit(
                text,
                byte_offset,
                forced_boundary.byte_offset,
                &mut boundaries,
                budget,
            )?;
        }
        byte_offset = forced_boundary.byte_offset;
        scalar_offset = forced_boundary.scalar_offset;
    }
    append_sentence_boundaries(
        text.get(byte_offset..)?,
        byte_offset,
        scalar_offset,
        &mut boundaries,
        budget,
    )?;
    Some(boundaries)
}

fn append_trailing_unit(
    text: &str,
    segment_start: usize,
    segment_end: usize,
    boundaries: &mut Vec<SentenceBoundary>,
    budget: &mut RecoveryBudget,
) -> Option<()> {
    let tail_start = boundaries
        .last()
        .filter(|boundary| boundary.byte_end >= segment_start)
        .map_or(segment_start, |boundary| boundary.byte_end);
    let tail = text.get(tail_start..segment_end)?;
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        return Some(());
    }
    if !budget.charge_occurrences(1) {
        return None;
    }
    let byte_start = tail_start.checked_add(tail.len().checked_sub(tail.trim_start().len())?)?;
    let byte_end = tail_start.checked_add(tail.trim_end().len())?;
    let scalar_start = text.get(..byte_start)?.chars().count();
    let scalar_end = scalar_start.checked_add(trimmed.chars().count())?;
    boundaries.try_reserve(1).ok()?;
    boundaries.push(SentenceBoundary {
        byte_start,
        byte_end,
        scalar_start,
        scalar_end,
    });
    Some(())
}

fn trailing_fragment_boundary(
    text: &str,
    boundaries: &[SentenceBoundary],
    budget: &mut RecoveryBudget,
) -> Option<Option<SentenceBoundary>> {
    let tail_start = boundaries.last().map_or(0, |boundary| boundary.byte_end);
    let tail = text.get(tail_start..)?;
    let trimmed = tail.trim();
    if trimmed.is_empty() || is_true_sentence_terminal(trimmed) {
        return Some(None);
    }
    if !budget.charge_occurrences(1) {
        return None;
    }
    let leading_bytes = tail.len().checked_sub(tail.trim_start().len())?;
    let byte_start = tail_start.checked_add(leading_bytes)?;
    let byte_end = tail_start.checked_add(tail.trim_end().len())?;
    let scalar_start = text.get(..byte_start)?.chars().count();
    let scalar_end = scalar_start.checked_add(trimmed.chars().count())?;
    Some(Some(SentenceBoundary {
        byte_start,
        byte_end,
        scalar_start,
        scalar_end,
    }))
}

fn atomic_line_boundaries(
    text: &str,
    budget: &mut RecoveryBudget,
) -> Option<Vec<SentenceBoundary>> {
    let mut boundaries = Vec::new();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(boundaries);
    }
    if !budget.charge_occurrences(1) {
        return None;
    }
    let byte_start = text.len().checked_sub(text.trim_start().len())?;
    let byte_end = text.trim_end().len();
    let scalar_start = text.get(..byte_start)?.chars().count();
    let scalar_end = scalar_start.checked_add(trimmed.chars().count())?;
    boundaries.try_reserve_exact(1).ok()?;
    boundaries.push(SentenceBoundary {
        byte_start,
        byte_end,
        scalar_start,
        scalar_end,
    });
    Some(boundaries)
}

fn append_sentence_boundaries(
    text: &str,
    byte_offset: usize,
    scalar_offset: usize,
    boundaries: &mut Vec<SentenceBoundary>,
    budget: &mut RecoveryBudget,
) -> Option<()> {
    let mut pending_byte_start = None;
    let mut pending_scalar_start = 0usize;
    let mut scalar_cursor = 0usize;

    for (byte_start, segment) in text.split_sentence_bound_indices() {
        let start = *pending_byte_start.get_or_insert(byte_start);
        if start == byte_start {
            pending_scalar_start = scalar_cursor;
        }
        let byte_end = byte_start.checked_add(segment.len())?;
        let scalar_end = scalar_cursor.checked_add(segment.chars().count())?;
        let raw = text.get(start..byte_end)?;
        let trimmed = raw.trim();
        if !trimmed.is_empty() && is_true_sentence_terminal(trimmed) {
            let leading_bytes = raw.len().checked_sub(raw.trim_start().len())?;
            let trailing_bytes = raw.len().checked_sub(raw.trim_end().len())?;
            let trailing_start = raw.len().checked_sub(trailing_bytes)?;
            if !budget.charge_occurrences(1) {
                return None;
            }
            boundaries.try_reserve(1).ok()?;
            boundaries.push(SentenceBoundary {
                byte_start: byte_offset.checked_add(start)?.checked_add(leading_bytes)?,
                byte_end: byte_offset.checked_add(byte_end.checked_sub(trailing_bytes)?)?,
                scalar_start: scalar_offset
                    .checked_add(pending_scalar_start)?
                    .checked_add(raw.get(..leading_bytes)?.chars().count())?,
                scalar_end: scalar_offset.checked_add(
                    scalar_end.checked_sub(raw.get(trailing_start..)?.chars().count())?,
                )?,
            });
            pending_byte_start = None;
        }
        scalar_cursor = scalar_end;
    }

    Some(())
}

fn is_true_sentence_terminal(text: &str) -> bool {
    let core = text.trim_end_matches(is_closing_punctuation);
    let Some(terminal) = core.chars().next_back() else {
        return false;
    };
    if !matches!(terminal, '.' | '!' | '?' | '。' | '！' | '？') {
        return false;
    }

    let terminal_start = core.len() - terminal.len_utf8();
    let atom = core[..terminal_start]
        .split_whitespace()
        .next_back()
        .unwrap_or_default()
        .trim_start_matches(is_opening_punctuation);
    if is_url_email_or_version_atom(atom) {
        return false;
    }
    if terminal != '.' {
        return true;
    }

    !is_uppercase_initial(atom)
        && !is_structural_abbreviation(atom)
        && !is_short_mixed_case_abbreviation(atom)
        && !atom.contains('.')
}

fn is_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        '"' | '\''
            | '\u{2019}'
            | '\u{201d}'
            | '\u{00bb}'
            | ')'
            | ']'
            | '}'
            | '\u{3009}'
            | '\u{300b}'
            | '\u{300d}'
            | '\u{300f}'
            | '\u{3011}'
            | '\u{3015}'
            | '\u{3017}'
            | '\u{3019}'
            | '\u{301b}'
    )
}

fn is_opening_punctuation(character: char) -> bool {
    matches!(
        character,
        '"' | '\''
            | '\u{2018}'
            | '\u{201c}'
            | '\u{00ab}'
            | '('
            | '['
            | '{'
            | '\u{3008}'
            | '\u{300a}'
            | '\u{300c}'
            | '\u{300e}'
            | '\u{3010}'
            | '\u{3014}'
            | '\u{3016}'
            | '\u{3018}'
            | '\u{301a}'
    )
}

fn is_uppercase_initial(atom: &str) -> bool {
    let mut letters = atom.chars();
    let first = letters.next();
    let second = letters.next();
    let third = letters.next();
    first.is_some_and(|character| character.is_alphabetic() && character.is_uppercase())
        && second.is_none_or(|character| character.is_alphabetic() && character.is_uppercase())
        && third.is_none()
}

fn is_structural_abbreviation(atom: &str) -> bool {
    const ABBREVIATIONS: &[&str] = &[
        "Mr", "Mrs", "Ms", "Dr", "Prof", "Sr", "Jr", "Pt", "Rev", "Cat", "Fig", "No", "Sec", "Vol",
    ];
    ABBREVIATIONS
        .iter()
        .any(|abbreviation| atom.eq_ignore_ascii_case(abbreviation))
}

fn is_short_mixed_case_abbreviation(atom: &str) -> bool {
    let mut count = 0usize;
    let mut has_uppercase = false;
    let mut has_lowercase = false;
    for character in atom.chars() {
        if !character.is_alphabetic() {
            return false;
        }
        count += 1;
        if count > 4 {
            return false;
        }
        has_uppercase |= character.is_uppercase();
        has_lowercase |= character.is_lowercase();
    }
    (2..=4).contains(&count) && has_uppercase && has_lowercase
}

fn is_url_email_or_version_atom(atom: &str) -> bool {
    if atom.contains("://")
        || atom.contains('@')
        || starts_with_ignore_ascii_case(atom, "www.")
        || starts_with_ignore_ascii_case(atom, "mailto:")
    {
        return true;
    }
    let (version, has_version_marker) = atom
        .get(..1)
        .filter(|prefix| prefix.eq_ignore_ascii_case("v"))
        .and_then(|_| atom.get(1..))
        .map_or((atom, false), |version| (version, true));
    (has_version_marker || version.contains(['.', '-', '_']))
        && version.chars().any(|character| character.is_ascii_digit())
        && version
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, '.' | '-' | '_'))
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::{RegionId, RegionRelation},
        model::{FontProgramHash, PageId, Rect, Vec2},
    };

    const TEST_NEAR_SCOPE: NearSearchScope = NearSearchScope::SameOrAmbiguousSpan;

    fn fallback_test_plan(block: u64) -> SentenceRecoveryPlan {
        SentenceRecoveryPlan {
            cross_span_replacement_new_spans: vec![plan_block_marker(block)],
            deletions: vec![RecoveredSentence {
                span_index: block as usize,
                kind: RecoveryUnitKind::Sentence,
                role: OccurrenceRole::Body,
                blocks: vec![BlockId(block)],
                separator: None,
                canonical: ScalarRange { start: 0, end: 1 },
                comparable: TokenRange { start: 0, end: 1 },
                source_tokens: 1,
            }],
            ..SentenceRecoveryPlan::default()
        }
    }

    fn plan_block_marker(block: u64) -> usize {
        block as usize + 1_000
    }

    fn fallback_test_outcome(
        near_relation_complete: bool,
        plan_block: u64,
        watch_segment_candidates: usize,
    ) -> SentenceRecoveryBuildOutcome {
        let metrics = SentenceRecoveryMetrics {
            near_relation_complete,
            near_pair_visits_examined: plan_block as usize,
            near_pair_visits_attempted: plan_block as usize + 1,
            near_similarity_comparisons_examined: plan_block as usize + 2,
            near_similarity_comparisons_attempted: plan_block as usize + 3,
            near_candidate_posting_visits_examined: plan_block as usize + 4,
            near_candidate_posting_visits_attempted: plan_block as usize + 5,
            sentence_edge_filter_pairs_examined: plan_block as usize + 6,
            sentence_edge_filter_pairs_attempted: plan_block as usize + 7,
            sentence_edge_filter_pairs_retained: plan_block as usize + 8,
            sentence_edge_filter_pairs_rejected: plan_block as usize + 9,
            ..SentenceRecoveryMetrics::default()
        };
        SentenceRecoveryBuildOutcome {
            plan: Some(fallback_test_plan(plan_block)),
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics,
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            }),
            watch_diagnostics: Some(RecoveryWatchDiagnostics {
                near_relation_complete,
                segment_candidates: watch_segment_candidates,
                ..RecoveryWatchDiagnostics::default()
            }),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        }
    }

    fn direct_test_outcome(plan_block: Option<u64>) -> SentenceRecoveryBuildOutcome {
        SentenceRecoveryBuildOutcome {
            plan: plan_block.map(fallback_test_plan),
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics {
                    near_relation_complete: true,
                    sentence_edge_filter_complete: true,
                    sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                        complete: true,
                        ..SentenceEdgeSignatureShadowMetrics::default()
                    }),
                    sentence_edge_signature_direct_shadow: Some(
                        SentenceEdgeSignatureDirectShadowMetrics::default(),
                    ),
                    ..SentenceRecoveryMetrics::default()
                },
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            }),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        }
    }

    fn build_with_current_atomic_fallback(
        mut build: impl FnMut(SentenceEdgeFilterMode) -> Result<SentenceRecoveryBuildOutcome>,
    ) -> Result<SentenceRecoveryBuildOutcome> {
        build_sentence_recovery_plan_with_atomic_fallback(
            (
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeSignatureFilterMode::Disabled,
            ),
            (
                SentenceEdgeFilterMode::Legacy,
                SentenceEdgeSignatureFilterMode::Disabled,
            ),
            |edge_mode, signature_mode| {
                assert_eq!(signature_mode, SentenceEdgeSignatureFilterMode::Disabled);
                build(edge_mode)
            },
        )
    }

    #[test]
    fn complete_filtered_disabled_attempt_does_not_retry() {
        let mut modes = Vec::new();
        let outcome = build_with_current_atomic_fallback(|mode| {
            modes.push(mode);
            Ok(fallback_test_outcome(true, 11, 12))
        })
        .expect("complete filtered build succeeds");

        assert_eq!(modes, [SentenceEdgeFilterMode::Filtered]);
        assert_eq!(
            outcome.plan.expect("filtered plan is retained").deletions,
            fallback_test_plan(11).deletions
        );
        assert_eq!(
            outcome
                .watch_diagnostics
                .expect("filtered watch is retained")
                .segment_candidates,
            12
        );
        let metrics = outcome
            .diagnostics
            .expect("filtered metrics remain")
            .metrics;
        assert!(!metrics.sentence_edge_filter_complete);
        assert!(!metrics.sentence_edge_filter_full_build_fallback_used);
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_pair_visits_examined,
            0
        );
    }

    #[test]
    fn atomic_fallback_passes_distinct_first_and_retry_modes() {
        let mut modes = Vec::new();
        let outcome = build_sentence_recovery_plan_with_atomic_fallback(
            (
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeSignatureFilterMode::Direct,
            ),
            (
                SentenceEdgeFilterMode::Legacy,
                SentenceEdgeSignatureFilterMode::Disabled,
            ),
            |edge_mode, signature_mode| {
                modes.push((edge_mode, signature_mode));
                Ok(fallback_test_outcome(
                    edge_mode == SentenceEdgeFilterMode::Legacy,
                    21,
                    22,
                ))
            },
        )
        .expect("legacy retry succeeds");

        assert_eq!(
            modes,
            [
                (
                    SentenceEdgeFilterMode::Filtered,
                    SentenceEdgeSignatureFilterMode::Direct,
                ),
                (
                    SentenceEdgeFilterMode::Legacy,
                    SentenceEdgeSignatureFilterMode::Disabled,
                ),
            ]
        );
        assert!(
            outcome
                .diagnostics
                .expect("legacy metrics remain")
                .metrics
                .sentence_edge_filter_full_build_fallback_used
        );
    }

    #[test]
    fn production_direct_is_accepted_without_parity_claims() {
        let mut modes = Vec::new();
        let outcome = build_sentence_recovery_plan_with_atomic_fallback(
            (
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeSignatureFilterMode::Direct,
            ),
            (
                SentenceEdgeFilterMode::Legacy,
                SentenceEdgeSignatureFilterMode::Disabled,
            ),
            |edge_mode, signature_mode| {
                modes.push((edge_mode, signature_mode));
                Ok(direct_test_outcome(Some(23)))
            },
        )
        .expect("Direct build succeeds");

        assert_eq!(
            modes,
            [(
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeSignatureFilterMode::Direct,
            )]
        );
        let diagnostics = outcome.diagnostics.expect("Direct diagnostics remain");
        let direct = diagnostics
            .metrics
            .sentence_edge_signature_direct_shadow
            .expect("Direct metrics are attached");
        assert!(direct.complete);
        assert!(!direct.parity_evaluable);
        assert!(!direct.verification_evaluable);
        assert_eq!(
            diagnostics.metrics.sentence_edge_signature_direct_execution,
            Some(SentenceEdgeSignatureDirectExecution::ProductionAccepted)
        );
    }

    #[test]
    fn no_work_direct_build_is_complete_without_fallback() {
        let mut calls = 0;
        let outcome = build_sentence_recovery_plan_with_atomic_fallback(
            (
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeSignatureFilterMode::Direct,
            ),
            (
                SentenceEdgeFilterMode::Legacy,
                SentenceEdgeSignatureFilterMode::Disabled,
            ),
            |_, _| {
                calls += 1;
                Ok(direct_test_outcome(None))
            },
        )
        .expect("empty Direct build succeeds");

        assert_eq!(calls, 1);
        assert!(outcome.plan.is_none());
        let metrics = outcome.diagnostics.expect("zero metrics remain").metrics;
        assert!(
            metrics
                .sentence_edge_signature_direct_shadow
                .expect("Direct metrics exist")
                .complete
        );
        assert_eq!(
            metrics.sentence_edge_signature_direct_execution,
            Some(SentenceEdgeSignatureDirectExecution::ProductionAccepted)
        );
        assert!(!metrics.sentence_edge_filter_full_build_fallback_used);
    }

    #[test]
    fn direct_stop_retries_whole_build_and_preserves_typed_provenance() {
        let reasons = [
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexPostingLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexDistinctKeyLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexEstimatedByteLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryCountLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryPostingVisitLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SignatureCandidateUnionLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SignatureExactEdgeRecheckLimit,
            SentenceEdgeSignatureDirectShadowStopReason::DirectEdgePairVisitLimit,
            SentenceEdgeSignatureDirectShadowStopReason::DirectEdgeSimilarityComparisonLimit,
            SentenceEdgeSignatureDirectShadowStopReason::CandidatePostingVisitLimit,
            SentenceEdgeSignatureDirectShadowStopReason::PairVisitLimit,
            SentenceEdgeSignatureDirectShadowStopReason::SimilarityComparisonLimit,
            SentenceEdgeSignatureDirectShadowStopReason::CandidateCountLimit,
            SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoPairVisitLimit,
            SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoSimilarityComparisonLimit,
            SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoIncomplete,
            SentenceEdgeSignatureDirectShadowStopReason::WatchProbePairLimit,
            SentenceEdgeSignatureDirectShadowStopReason::WatchProbeSimilarityComparisonLimit,
            SentenceEdgeSignatureDirectShadowStopReason::WatchProbeInvariantViolation,
            SentenceEdgeSignatureDirectShadowStopReason::WatchDiagnosticsMismatch,
            SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure,
            SentenceEdgeSignatureDirectShadowStopReason::CounterOverflow,
            SentenceEdgeSignatureDirectShadowStopReason::ProductionTraversalIncomplete,
            SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure,
        ];
        for reason in reasons {
            let outcome = build_sentence_recovery_plan_with_atomic_fallback(
                (
                    SentenceEdgeFilterMode::Filtered,
                    SentenceEdgeSignatureFilterMode::Direct,
                ),
                (
                    SentenceEdgeFilterMode::Legacy,
                    SentenceEdgeSignatureFilterMode::Disabled,
                ),
                |edge_mode, _| {
                    if edge_mode == SentenceEdgeFilterMode::Legacy {
                        return Ok(fallback_test_outcome(true, 32, 33));
                    }
                    let mut direct = direct_test_outcome(Some(22));
                    direct
                        .diagnostics
                        .as_mut()
                        .expect("Direct diagnostics exist")
                        .metrics
                        .sentence_edge_signature_direct_shadow
                        .as_mut()
                        .expect("Direct metrics exist")
                        .stop_reason = Some(reason);
                    Ok(direct)
                },
            )
            .expect("legacy retry succeeds");

            assert_eq!(
                outcome
                    .plan
                    .as_ref()
                    .expect("legacy plan remains")
                    .deletions,
                fallback_test_plan(32).deletions
            );
            let metrics = outcome
                .diagnostics
                .expect("legacy diagnostics remain")
                .metrics;
            let discarded = metrics
                .sentence_edge_signature_direct_shadow
                .expect("discarded Direct metrics remain");
            assert!(!discarded.complete);
            assert_eq!(discarded.stop_reason, Some(reason));
            assert_eq!(
                metrics.sentence_edge_signature_direct_execution,
                Some(SentenceEdgeSignatureDirectExecution::ProductionDiscarded)
            );
            assert!(metrics.sentence_edge_filter_full_build_fallback_used);
        }
    }

    #[test]
    fn direct_setup_failure_without_filter_checkpoint_preserves_fallback_provenance() {
        let mut calls = 0;
        let outcome = build_sentence_recovery_plan_with_atomic_fallback(
            (
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeSignatureFilterMode::Direct,
            ),
            (
                SentenceEdgeFilterMode::Legacy,
                SentenceEdgeSignatureFilterMode::Disabled,
            ),
            |edge_mode, _| {
                calls += 1;
                if edge_mode == SentenceEdgeFilterMode::Legacy {
                    return Ok(fallback_test_outcome(true, 41, 42));
                }
                Ok(SentenceRecoveryBuildOutcome::default())
            },
        )
        .expect("legacy retry succeeds");

        assert_eq!(calls, 2);
        let metrics = outcome
            .diagnostics
            .expect("legacy diagnostics remain")
            .metrics;
        assert!(metrics.sentence_edge_filter_full_build_fallback_used);
        assert_eq!(
            metrics.sentence_edge_signature_direct_execution,
            Some(SentenceEdgeSignatureDirectExecution::ProductionDiscarded)
        );
        let discarded = metrics
            .sentence_edge_signature_direct_shadow
            .expect("failed Direct setup remains observable");
        assert!(!discarded.complete);
        assert_eq!(
            discarded.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure)
        );
    }

    #[test]
    fn incomplete_near_relation_reruns_when_filter_itself_completed() {
        let mut modes = Vec::new();
        let outcome = build_with_current_atomic_fallback(|mode| {
            modes.push(mode);
            let mut outcome = match mode {
                SentenceEdgeFilterMode::Filtered => fallback_test_outcome(false, 61, 62),
                SentenceEdgeFilterMode::Legacy => fallback_test_outcome(true, 71, 72),
            };
            outcome
                .diagnostics
                .as_mut()
                .expect("metrics fixture exists")
                .metrics
                .sentence_edge_filter_complete = true;
            Ok(outcome)
        })
        .expect("legacy fallback succeeds");

        assert_eq!(
            modes,
            [
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeFilterMode::Legacy
            ]
        );
        assert_eq!(
            outcome
                .plan
                .expect("legacy plan is retained")
                .cross_span_replacement_new_spans,
            [plan_block_marker(71)]
        );
        assert!(
            outcome
                .diagnostics
                .expect("legacy metrics remain")
                .metrics
                .sentence_edge_filter_full_build_fallback_used
        );
    }

    #[test]
    fn incomplete_filtered_build_adopts_the_entire_legacy_build() {
        let filtered = fallback_test_outcome(false, 21, 22);
        let legacy = fallback_test_outcome(true, 31, 32);
        let expected_watch = legacy.watch_diagnostics.clone();
        let mut modes = Vec::new();
        let outcome = build_with_current_atomic_fallback(|mode| {
            modes.push(mode);
            Ok(match mode {
                SentenceEdgeFilterMode::Filtered => fallback_test_outcome(false, 21, 22),
                SentenceEdgeFilterMode::Legacy => fallback_test_outcome(true, 31, 32),
            })
        })
        .expect("legacy fallback succeeds");

        assert_eq!(
            modes,
            [
                SentenceEdgeFilterMode::Filtered,
                SentenceEdgeFilterMode::Legacy
            ]
        );
        assert_eq!(
            outcome
                .plan
                .as_ref()
                .expect("legacy plan is retained")
                .deletions,
            fallback_test_plan(31).deletions
        );
        assert_eq!(
            outcome
                .plan
                .as_ref()
                .expect("legacy plan is retained")
                .cross_span_replacement_new_spans,
            [plan_block_marker(31)]
        );
        assert_eq!(outcome.watch_diagnostics, expected_watch);
        let metrics = outcome.diagnostics.expect("legacy metrics remain").metrics;
        assert!(metrics.near_relation_complete);
        assert_eq!(metrics.near_pair_visits_examined, 31);
        assert_eq!(metrics.sentence_edge_filter_pairs_examined, 27);
        assert!(metrics.sentence_edge_filter_full_build_fallback_used);
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_pair_visits_examined,
            21
        );
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_pair_visits_attempted,
            22
        );
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_similarity_comparisons_examined,
            23
        );
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_similarity_comparisons_attempted,
            24
        );
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_candidate_posting_visits_examined,
            25
        );
        assert_eq!(
            metrics.sentence_edge_filter_discarded_near_candidate_posting_visits_attempted,
            26
        );
        assert_eq!(
            filtered
                .diagnostics
                .expect("filtered fixture has metrics")
                .metrics
                .sentence_edge_filter_pairs_examined,
            metrics.sentence_edge_filter_pairs_examined
        );
    }

    #[test]
    fn fallback_keeps_legacy_low_score_watch_diagnostics() {
        let outcome = build_with_current_atomic_fallback(|mode| {
            let mut outcome = match mode {
                SentenceEdgeFilterMode::Filtered => fallback_test_outcome(false, 41, 42),
                SentenceEdgeFilterMode::Legacy => fallback_test_outcome(true, 51, 52),
            };
            let near_score = match mode {
                SentenceEdgeFilterMode::Filtered => 2_999,
                SentenceEdgeFilterMode::Legacy => 86,
            };
            outcome
                .watch_diagnostics
                .as_mut()
                .expect("watch fixture exists")
                .records
                .push(RecoveryWatchRecord {
                    id: "low-score".to_owned(),
                    old: RecoveryWatchOccurrenceEvidence::NotQueried,
                    new: RecoveryWatchOccurrenceEvidence::NotQueried,
                    pair: Some(RecoveryWatchPairEvidence {
                        same_span: true,
                        exact_shared_units: 0,
                        exact_shared_units_available: false,
                        near_candidate_examined: true,
                        near_score: Some(near_score),
                        near_scope: Some(RecoveryWatchNearScope::SameSpan),
                        old_relation: RecoveryWatchRelation {
                            available: true,
                            best_score: near_score,
                            second_score: near_score.saturating_sub(1),
                            watched_partner_is_best: true,
                        },
                        new_relation: RecoveryWatchRelation {
                            available: true,
                            best_score: near_score,
                            second_score: near_score.saturating_sub(2),
                            watched_partner_is_best: true,
                        },
                        reciprocal: false,
                    }),
                    segment_pair: None,
                    granular_pair: None,
                });
            Ok(outcome)
        })
        .expect("legacy fallback succeeds");
        let watch = outcome.watch_diagnostics.expect("legacy watch is retained");

        assert!(watch.near_relation_complete);
        assert_eq!(watch.segment_candidates, 52);
        assert_eq!(
            watch.records[0]
                .pair
                .as_ref()
                .expect("pair evidence is retained")
                .near_score,
            Some(86)
        );
        let pair = watch.records[0]
            .pair
            .as_ref()
            .expect("pair evidence is retained");
        assert_eq!(
            (pair.old_relation.best_score, pair.old_relation.second_score),
            (86, 85)
        );
        assert_eq!(
            (pair.new_relation.best_score, pair.new_relation.second_score),
            (86, 84)
        );
    }

    #[test]
    fn fallback_adopts_an_incomplete_legacy_build_without_filtered_plan_leakage() {
        let outcome = build_with_current_atomic_fallback(|mode| {
            Ok(match mode {
                SentenceEdgeFilterMode::Filtered => fallback_test_outcome(false, 81, 82),
                SentenceEdgeFilterMode::Legacy => fallback_test_outcome(false, 91, 92),
            })
        })
        .expect("bounded incomplete legacy fallback succeeds");
        let plan = outcome.plan.expect("legacy partial plan is retained");
        let metrics = outcome.diagnostics.expect("legacy metrics remain").metrics;

        assert_eq!(plan.deletions, fallback_test_plan(91).deletions);
        assert_eq!(
            plan.cross_span_replacement_new_spans,
            [plan_block_marker(91)]
        );
        assert!(!metrics.near_relation_complete);
        assert_eq!(metrics.near_pair_visits_examined, 91);
        assert!(metrics.sentence_edge_filter_full_build_fallback_used);
    }

    #[test]
    fn recovery_watch_distinguishes_candidate_generation_incomplete() {
        let mut watch = RecoveryWatchState {
            complete: true,
            candidate_generation_complete: false,
            near_relation_complete: false,
            near_relation_stop_reason: None,
            segment_analysis: SegmentDiagnosticAnalysis::default(),
            segment_stop_reason: None,
            segment_overlap_vetoes: 0,
            granular_complete: true,
            granular_old_units: 0,
            granular_new_units: 0,
            granular_pair_comparisons: 0,
            granular_stop_reason: None,
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
            old_pair_partners: HashMap::new(),
            new_pair_partners: HashMap::new(),
            scan_work: 0,
            scan_limit: 0,
            retained_one_sided_occurrences: 0,
        };
        watch.record_near_search_state(
            false,
            false,
            Some(NearRelationStopReason::CandidateCountLimit),
        );
        let diagnostics = watch.finish(None);
        assert!(!diagnostics.complete);
        assert!(!diagnostics.candidate_generation_complete);
        assert!(!diagnostics.near_relation_complete);
        assert_eq!(
            diagnostics.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidateCountLimit)
        );
    }

    #[test]
    fn recovery_watch_preserves_one_sided_candidate_relation() {
        let mut relation = CandidateNearRelation::default();
        relation.record_disqualifying(7_500);
        let mut watch = RecoveryWatchState {
            complete: true,
            candidate_generation_complete: true,
            near_relation_complete: true,
            near_relation_stop_reason: None,
            segment_analysis: SegmentDiagnosticAnalysis::default(),
            segment_stop_reason: None,
            segment_overlap_vetoes: 0,
            granular_complete: true,
            granular_old_units: 0,
            granular_new_units: 0,
            granular_pair_comparisons: 0,
            granular_stop_reason: None,
            records: vec![RecoveryWatchStateRecord {
                output: RecoveryWatchRecord {
                    id: "one-sided".to_owned(),
                    old: RecoveryWatchOccurrenceEvidence::Unfound,
                    new: RecoveryWatchOccurrenceEvidence::Unfound,
                    pair: Some(RecoveryWatchPairEvidence {
                        same_span: true,
                        exact_shared_units: 0,
                        exact_shared_units_available: false,
                        near_candidate_examined: true,
                        near_score: Some(7_500),
                        near_scope: Some(RecoveryWatchNearScope::SameSpan),
                        old_relation: RecoveryWatchRelation::default(),
                        new_relation: RecoveryWatchRelation::default(),
                        reciprocal: false,
                    }),
                    segment_pair: None,
                    granular_pair: None,
                },
                old_occurrence: Some(0),
                new_occurrence: Some(1),
                old_segment: None,
                new_segment: None,
            }],
            pair_by_occurrences: HashMap::new(),
            old_pair_partners: HashMap::new(),
            new_pair_partners: HashMap::new(),
            scan_work: 0,
            scan_limit: 0,
            retained_one_sided_occurrences: 0,
        };
        watch
            .record_relations(
                &[RecoveryCandidate {
                    occurrence_index: 0,
                    span_index: 0,
                }],
                &[],
                &ModifiedSentenceRelations {
                    old: vec![relation],
                    new: Vec::new(),
                    complete: true,
                    edge_gate_shadow: None,
                    edge_signature_shadow: None,
                },
            )
            .expect("one-sided relation records");
        let pair = watch.records[0]
            .output
            .pair
            .as_ref()
            .expect("pair evidence remains available");
        assert!(pair.old_relation.available);
        assert_eq!(pair.old_relation.best_score, 7_500);
        assert!(!pair.new_relation.available);
        assert!(!pair.reciprocal);
    }

    fn interval(run_id: u64, start: usize, end: usize) -> Option<TrustedRunInterval> {
        Some(TrustedRunInterval {
            run_id: TrustedRunId(run_id),
            start,
            end,
        })
    }

    fn plan_blocks(plans: &[StreamPlan]) -> Vec<(Vec<usize>, bool)> {
        plans
            .iter()
            .map(|plan| (plan.block_indices.clone(), plan.trusted))
            .collect()
    }

    fn boundary_texts(text: &str, token_limit: usize) -> Vec<&str> {
        let mut budget =
            RecoveryBudget::new(token_limit, 0, token_limit, 1).expect("test budget is valid");
        sentence_boundaries(text, &[], &mut budget)
            .expect("sentence scan stays within budget")
            .iter()
            .map(|range| &text[range.byte_start..range.byte_end])
            .collect()
    }

    fn test_location(range: LocalSentenceRange, span_index: usize) -> SentenceLocation {
        SentenceLocation {
            recovery: RecoveredSentence {
                span_index,
                kind: RecoveryUnitKind::Sentence,
                role: OccurrenceRole::Body,
                blocks: vec![range.block],
                separator: None,
                canonical: range.canonical,
                comparable: range.comparable,
                source_tokens: range.comparable.end - range.comparable.start,
            },
            consumed: vec![range],
        }
    }

    fn positioned_occurrence(
        key: &str,
        block: u64,
        stream_index: usize,
        ordinal: usize,
    ) -> SentenceOccurrence {
        let range = LocalSentenceRange {
            block: BlockId(block),
            canonical: ScalarRange { start: 0, end: 5 },
            comparable: TokenRange { start: 0, end: 5 },
        };
        SentenceOccurrence {
            key: key.to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a'); 5],
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: Some(test_location(range, 0)),
            span_index: Some(0),
            trusted_position: Some(TrustedStreamPosition {
                stream_index,
                ordinal,
            }),
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }
    }

    fn positioned_scalar(
        scalar: char,
        block: u64,
        stream_index: usize,
        ordinal: usize,
    ) -> SentenceOccurrence {
        let mut occurrence =
            positioned_occurrence(&scalar.to_string(), block, stream_index, ordinal);
        occurrence.tokens = vec![SentenceEvidenceToken::Scalar(scalar)];
        occurrence
    }

    fn watch_state() -> RecoveryWatchState {
        RecoveryWatchState {
            complete: true,
            candidate_generation_complete: false,
            near_relation_complete: false,
            near_relation_stop_reason: None,
            segment_analysis: SegmentDiagnosticAnalysis::default(),
            segment_stop_reason: None,
            segment_overlap_vetoes: 0,
            granular_complete: true,
            granular_old_units: 0,
            granular_new_units: 0,
            granular_pair_comparisons: 0,
            granular_stop_reason: None,
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
            old_pair_partners: HashMap::new(),
            new_pair_partners: HashMap::new(),
            scan_work: 0,
            scan_limit: 100_000,
            retained_one_sided_occurrences: 0,
        }
    }

    fn watch_state_for_pair() -> RecoveryWatchState {
        let mut watch = watch_state();
        watch.records.push(RecoveryWatchStateRecord {
            output: RecoveryWatchRecord {
                id: "pair".to_owned(),
                old: RecoveryWatchOccurrenceEvidence::Unfound,
                new: RecoveryWatchOccurrenceEvidence::Unfound,
                pair: Some(RecoveryWatchPairEvidence {
                    same_span: true,
                    exact_shared_units: 0,
                    exact_shared_units_available: false,
                    near_candidate_examined: false,
                    near_score: None,
                    near_scope: None,
                    old_relation: RecoveryWatchRelation::default(),
                    new_relation: RecoveryWatchRelation::default(),
                    reciprocal: false,
                }),
                segment_pair: None,
                granular_pair: None,
            },
            old_occurrence: Some(0),
            new_occurrence: Some(0),
            old_segment: None,
            new_segment: None,
        });
        watch.pair_by_occurrences.insert((0, 0), vec![0]);
        watch.old_pair_partners.insert(0, vec![0]);
        watch.new_pair_partners.insert(0, vec![0]);
        watch
    }

    fn edge_probe_occurrence(tokens: &str) -> SentenceOccurrence {
        let mut occurrence = positioned_occurrence(tokens, 1, 0, 0);
        occurrence.tokens = tokens.chars().map(SentenceEvidenceToken::Scalar).collect();
        occurrence
    }

    #[test]
    fn direct_watch_probe_restores_legacy_low_edge_evidence() {
        let old = [edge_probe_occurrence("abcdefghij")];
        let new = [edge_probe_occurrence("aklmnopqrs")];
        let index = UnitCandidateIndex::new(&new, CandidatePostingIndexScope::Global)
            .expect("candidate index builds");
        let mut watch = watch_state_for_pair();
        let mut budget = RecoveryBudget::new(100, 100, 1_000, 1).expect("budget builds");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;

        probe_direct_watch_pairs(
            Some(&mut watch),
            &mut budget,
            &index,
            OccurrenceSide::Old,
            0,
            &old[0],
            &new,
            &[],
            CandidatePostingBucket::Global,
            None,
            RecoveryWatchNearScope::CrossSpan,
            |_| true,
        )
        .expect("low-score probe succeeds");

        let pair = watch.records[0]
            .output
            .pair
            .as_ref()
            .expect("pair evidence exists");
        assert!(pair.near_candidate_examined);
        assert_eq!(pair.near_score, Some(1_000));
        assert_eq!(pair.near_scope, Some(RecoveryWatchNearScope::CrossSpan));
        assert_eq!(budget.watch_probe_pairs, 1);
        assert_eq!(budget.watch_probe_missing_signature_candidates, 1);
    }

    #[test]
    fn direct_watch_probe_rejects_missing_retained_signature_candidate() {
        let old = [edge_probe_occurrence("abcdefghij")];
        let new = [edge_probe_occurrence("abcklmnopq")];
        let index = UnitCandidateIndex::new(&new, CandidatePostingIndexScope::Global)
            .expect("candidate index builds");
        let mut watch = watch_state_for_pair();
        let mut budget = RecoveryBudget::new(100, 100, 1_000, 1).expect("budget builds");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;

        assert!(
            probe_direct_watch_pairs(
                Some(&mut watch),
                &mut budget,
                &index,
                OccurrenceSide::Old,
                0,
                &old[0],
                &new,
                &[],
                CandidatePostingBucket::Global,
                None,
                RecoveryWatchNearScope::SameSpan,
                |_| true,
            )
            .is_none()
        );
        assert_eq!(budget.watch_probe_invariant_violations, 1);
        assert_eq!(
            budget.watch_probe_stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::WatchProbeInvariantViolation)
        );
        assert!(
            !watch.records[0]
                .output
                .pair
                .as_ref()
                .expect("pair evidence exists")
                .near_candidate_examined
        );
    }

    #[test]
    fn direct_watch_probe_reports_pair_budget_stop() {
        let old = [edge_probe_occurrence("abcdefghij")];
        let new = [edge_probe_occurrence("aklmnopqrs")];
        let index = UnitCandidateIndex::new(&new, CandidatePostingIndexScope::Global)
            .expect("candidate index builds");
        let mut watch = watch_state_for_pair();
        let mut budget = RecoveryBudget::new(100, 100, 1_000, 1).expect("budget builds");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        budget.watch_probe_pair_limit = 0;

        assert!(
            probe_direct_watch_pairs(
                Some(&mut watch),
                &mut budget,
                &index,
                OccurrenceSide::Old,
                0,
                &old[0],
                &new,
                &[],
                CandidatePostingBucket::Global,
                None,
                RecoveryWatchNearScope::SameSpan,
                |_| true,
            )
            .is_none()
        );
        assert_eq!(budget.watch_probe_pairs, 0);
        assert_eq!(budget.watch_probe_pairs_attempted, 1);
        assert_eq!(
            budget.watch_probe_stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::WatchProbePairLimit)
        );
    }

    #[test]
    fn direct_watch_probe_reports_comparison_budget_stop() {
        let old = [edge_probe_occurrence("abcdefghij")];
        let new = [edge_probe_occurrence("aklmnopqrs")];
        let index = UnitCandidateIndex::new(&new, CandidatePostingIndexScope::Global)
            .expect("candidate index builds");
        let mut watch = watch_state_for_pair();
        let mut budget = RecoveryBudget::new(100, 100, 1_000, 1).expect("budget builds");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        budget.watch_probe_comparison_limit = 0;

        assert!(
            probe_direct_watch_pairs(
                Some(&mut watch),
                &mut budget,
                &index,
                OccurrenceSide::Old,
                0,
                &old[0],
                &new,
                &[],
                CandidatePostingBucket::Global,
                None,
                RecoveryWatchNearScope::SameSpan,
                |_| true,
            )
            .is_none()
        );
        assert_eq!(budget.watch_probe_comparisons, 0);
        assert_eq!(budget.watch_probe_comparisons_attempted, 1);
        assert_eq!(
            budget.watch_probe_stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::WatchProbeSimilarityComparisonLimit)
        );
    }

    fn incomplete_watch_pair_diagnostics() -> RecoveryWatchDiagnostics {
        let mut diagnostics = watch_state_for_pair().finish(None);
        diagnostics.candidate_generation_complete = true;
        diagnostics.records[0].segment_pair = Some(RecoveryWatchSegmentPairEvidence {
            old_start_ordinal: 0,
            old_end_ordinal: 2,
            new_start_ordinal: 3,
            new_end_ordinal: 5,
            old_unit_count: 2,
            new_unit_count: 2,
            old_token_count: 10,
            new_token_count: 10,
            exact: true,
            old_occurrence_count: 1,
            new_occurrence_count: 1,
            role_compatible: true,
            overlaps_existing_recovery: false,
            crossing_anchor_count: 0,
            relation: ExactSegmentRelation::ExactUniqueTopologyUnknown,
        });
        diagnostics
    }

    fn complete_direct_watch(accepted: &RecoveryWatchDiagnostics) -> RecoveryWatchDiagnostics {
        let mut direct = accepted.clone();
        direct.candidate_generation_complete = true;
        direct.near_relation_complete = true;
        direct.near_relation_stop_reason = None;
        direct
    }

    #[test]
    fn direct_watch_preservation_allows_new_dynamic_evidence() {
        let accepted = incomplete_watch_pair_diagnostics();
        let mut direct = complete_direct_watch(&accepted);
        direct.segment_overlap_vetoes = 1;
        let segment = direct.records[0]
            .segment_pair
            .as_mut()
            .expect("segment evidence exists");
        segment.overlaps_existing_recovery = true;
        let pair = direct.records[0]
            .pair
            .as_mut()
            .expect("pair evidence exists");
        pair.near_candidate_examined = true;
        pair.near_score = Some(1_000);
        pair.near_scope = Some(RecoveryWatchNearScope::CrossSpan);
        pair.old_relation.best_score = 8_000;
        pair.reciprocal = true;
        let mut metrics = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };

        compare_direct_watch_diagnostics(&mut metrics, Some(&accepted), Some(&direct), true, false);

        assert!(metrics.watch_preservation_evaluable);
        assert!(metrics.watch_evidence_preserved);
        assert_eq!(metrics.watch_preservation_mismatches, 0);
        assert!(!metrics.watch_exact_parity_evaluable);
        assert!(metrics.complete);
        assert!(metrics.stop_reason.is_none());
    }

    #[test]
    fn direct_watch_preservation_rejects_observed_score_drop() {
        let mut accepted = incomplete_watch_pair_diagnostics();
        let accepted_pair = accepted.records[0]
            .pair
            .as_mut()
            .expect("pair evidence exists");
        accepted_pair.near_candidate_examined = true;
        accepted_pair.near_score = Some(1_000);
        let direct = complete_direct_watch(&incomplete_watch_pair_diagnostics());
        let mut metrics = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };

        compare_direct_watch_diagnostics(&mut metrics, Some(&accepted), Some(&direct), true, false);

        assert!(metrics.watch_preservation_evaluable);
        assert!(!metrics.watch_evidence_preserved);
        assert_eq!(metrics.watch_preservation_mismatches, 1);
        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::WatchDiagnosticsMismatch)
        );
    }

    #[test]
    fn direct_watch_preservation_rejects_static_evidence_mutations() {
        let accepted = incomplete_watch_pair_diagnostics();
        let mutations = [
            |direct: &mut RecoveryWatchDiagnostics| {
                direct.candidate_generation_complete = false;
            },
            |direct: &mut RecoveryWatchDiagnostics| {
                direct.records[0].old = RecoveryWatchOccurrenceEvidence::Unavailable;
            },
            |direct: &mut RecoveryWatchDiagnostics| {
                direct.records[0]
                    .pair
                    .as_mut()
                    .expect("pair evidence exists")
                    .exact_shared_units = 1;
            },
            |direct: &mut RecoveryWatchDiagnostics| {
                direct.records[0]
                    .segment_pair
                    .as_mut()
                    .expect("segment evidence exists")
                    .old_token_count += 1;
            },
            |direct: &mut RecoveryWatchDiagnostics| {
                direct.granular_old_units += 1;
            },
        ];
        for mutate in mutations {
            let mut direct = complete_direct_watch(&accepted);
            mutate(&mut direct);
            assert!(!watch_fixed_evidence_is_preserved(&accepted, &direct));
        }
    }

    #[test]
    fn direct_watch_preservation_requires_complete_near_replay() {
        let accepted = incomplete_watch_pair_diagnostics();
        let direct = accepted.clone();
        assert!(!watch_fixed_evidence_is_preserved(&accepted, &direct));
    }

    #[test]
    fn complete_accepted_watch_requires_exact_parity() {
        let accepted = complete_direct_watch(&incomplete_watch_pair_diagnostics());
        let mut metrics = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        compare_direct_watch_diagnostics(
            &mut metrics,
            Some(&accepted),
            Some(&accepted),
            true,
            true,
        );
        assert!(metrics.watch_exact_parity_evaluable);
        assert!(metrics.watch_exact_parity);
        assert!(metrics.watch_evidence_preserved);

        let mut changed = accepted.clone();
        changed.records[0]
            .pair
            .as_mut()
            .expect("pair evidence exists")
            .near_scope = Some(RecoveryWatchNearScope::CrossSpan);
        let mut mismatch = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        compare_direct_watch_diagnostics(
            &mut mismatch,
            Some(&accepted),
            Some(&changed),
            true,
            true,
        );
        assert!(!mismatch.watch_exact_parity);
        assert!(!mismatch.watch_evidence_preserved);
    }

    #[test]
    fn direct_watch_parity_accepts_no_watch_and_rejects_presence_mismatch() {
        let mut no_watch = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            parity_evaluable: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        compare_direct_watch_diagnostics(&mut no_watch, None, None, true, false);
        assert!(no_watch.watch_preservation_evaluable);
        assert!(no_watch.watch_evidence_preserved);
        assert!(no_watch.complete);

        let incomplete = RecoveryWatchDiagnostics::default();
        let mut mismatch = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            parity_evaluable: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        compare_direct_watch_diagnostics(&mut mismatch, Some(&incomplete), None, true, false);
        assert!(!mismatch.watch_evidence_preserved);
        assert_eq!(mismatch.watch_preservation_mismatches, 1);
        assert!(!mismatch.complete);
    }

    fn granular_occurrence(text: &str, kind: RecoveryUnitKind) -> SentenceOccurrence {
        let mut occurrence = positioned_occurrence(text, 1, 0, 0);
        occurrence.tokens = text.chars().map(SentenceEvidenceToken::Scalar).collect();
        occurrence.kind = kind;
        occurrence
    }

    fn granular_texts<'a>(
        text: &'a str,
        boundaries: &[GranularBoundary],
        kind: RecoveryWatchUnitKind,
    ) -> Vec<&'a str> {
        boundaries
            .iter()
            .filter(|boundary| boundary.kind == kind)
            .map(|boundary| &text[boundary.byte_start..boundary.byte_end])
            .collect()
    }

    #[test]
    fn clause_boundaries_split_only_evidenced_commas() {
        let subordinate = "While critical infrastructure operates, organizations adapt.";
        let mut budget = GranularDiagnosticBudget::default();
        let boundaries =
            clause_boundaries(subordinate, &mut budget).expect("bounded clause scan succeeds");
        assert_eq!(
            granular_texts(subordinate, &boundaries, RecoveryWatchUnitKind::Clause),
            [
                "While critical infrastructure operates",
                "organizations adapt."
            ]
        );

        let ordinary = "Organizations identify, protect, and recover systems.";
        assert!(
            clause_boundaries(ordinary, &mut budget)
                .expect("ordinary comma scan succeeds")
                .is_empty()
        );
        assert!(
            clause_boundaries("See https://example.test:8443/v1.2.", &mut budget)
                .expect("URL scan succeeds")
                .is_empty()
        );
    }

    #[test]
    fn clause_boundaries_accept_spaced_and_unspaced_em_dash_parentheticals() {
        for text in [
            "Organizations\u{2014}including public bodies\u{2014}adapt.",
            "Organizations \u{2014} including public bodies \u{2014} adapt.",
        ] {
            let mut budget = GranularDiagnosticBudget::default();
            let boundaries =
                clause_boundaries(text, &mut budget).expect("bounded clause scan succeeds");
            assert_eq!(
                granular_texts(text, &boundaries, RecoveryWatchUnitKind::Clause),
                ["Organizations", "including public bodies", "adapt."]
            );
        }
    }

    #[test]
    fn list_item_boundaries_require_markers_or_bounded_enumerations() {
        let mut marked = granular_occurrence("2) Identify risks", RecoveryUnitKind::Line);
        let mut budget = GranularDiagnosticBudget::default();
        let boundaries = granular_boundaries(&marked, 0..marked.key.len(), &mut budget)
            .expect("marked line scan succeeds");
        assert_eq!(
            granular_texts(&marked.key, &boundaries, RecoveryWatchUnitKind::ListItem),
            ["Identify risks"]
        );

        marked.kind = RecoveryUnitKind::Sentence;
        assert!(
            granular_texts(
                &marked.key,
                &granular_boundaries(&marked, 0..marked.key.len(), &mut budget)
                    .expect("unmarked sentence scan succeeds"),
                RecoveryWatchUnitKind::ListItem,
            )
            .is_empty()
        );

        for text in [
            "Functions\u{2014}Identify, Protect, Detect, Respond, and Recover.",
            "Functions \u{2014} GOVERN, IDENTIFY, PROTECT, DETECT, RESPOND, and RECOVER \u{2014} organize outcomes.",
        ] {
            let occurrence = granular_occurrence(text, RecoveryUnitKind::Sentence);
            let mut budget = GranularDiagnosticBudget::default();
            let boundaries = granular_boundaries(&occurrence, 0..occurrence.key.len(), &mut budget)
                .expect("enumeration scan succeeds");
            let items = granular_texts(text, &boundaries, RecoveryWatchUnitKind::ListItem);
            assert!(items.len() >= 5);
            assert!(items.last().is_some_and(|item| {
                item.eq_ignore_ascii_case("recover") || item.eq_ignore_ascii_case("recover.")
            }));
        }

        for text in [
            ". Empty numeric",
            ") Empty numeric",
            "() Empty parenthesized",
        ] {
            assert_eq!(explicit_list_item_start(text), None);
        }
    }

    #[test]
    fn granular_quote_mapping_preserves_partial_unicode_occurrence_offsets() {
        let occurrence = granular_occurrence(
            "前置き。 While\u{3000}systems operate, organizations adapt. 後置き。",
            RecoveryUnitKind::Sentence,
        );
        let mut budget = GranularDiagnosticBudget::default();
        let range = watched_quote_range_for_granular(
            "While systems operate, organizations adapt.",
            &occurrence,
            &mut budget,
        )
        .expect("bounded quote mapping succeeds")
        .expect("the partial quote is unique");
        assert_eq!(
            &occurrence.key[range.clone()],
            "While\u{3000}systems operate, organizations adapt."
        );
        let units = build_granular_units(&occurrence, range, &mut budget)
            .expect("partial quote units build");
        assert_eq!(units.len(), 2);
        assert_eq!(
            &occurrence.key[units[0].byte_start..units[0].byte_end],
            "While\u{3000}systems operate"
        );
        assert_eq!(
            &occurrence.key[units[1].byte_start..units[1].byte_end],
            "organizations adapt."
        );
    }

    #[test]
    fn granular_quote_mapping_rejects_repeated_matches_in_one_occurrence() {
        let occurrence = granular_occurrence(
            "While systems operate, organizations adapt. While systems operate, organizations adapt.",
            RecoveryUnitKind::Sentence,
        );
        let mut budget = GranularDiagnosticBudget::default();
        assert_eq!(
            watched_quote_range_for_granular(
                "While systems operate, organizations adapt.",
                &occurrence,
                &mut budget,
            )
            .expect("bounded quote mapping succeeds"),
            None
        );
    }

    #[test]
    fn granular_relations_report_exact_reciprocal_partners_and_ties() {
        let old = [granular_occurrence(
            "While systems operate, organizations adapt.",
            RecoveryUnitKind::Sentence,
        )];
        let new = [granular_occurrence(
            "While systems operate, organizations adapt.",
            RecoveryUnitKind::Sentence,
        )];
        let mut budget = GranularDiagnosticBudget::default();
        let evidence = granular_pair_evidence(
            old[0].key.as_str(),
            new[0].key.as_str(),
            &old[0],
            &new[0],
            &mut budget,
        )
        .expect("bounded relation scan succeeds")
        .expect("clause evidence is available");
        assert_eq!(evidence.old_units.len(), 2);
        assert!(evidence.old_units.iter().all(|unit| {
            unit.relation.available
                && unit.relation.best_score == 10_000
                && unit.relation.partner_index.is_some()
                && unit.relation.exact
                && unit.relation.reciprocal
                && !unit.relation.tied_for_best
        }));

        let tie = granular_relation([8_000, 8_000, 2_000]);
        assert_eq!(tie.best_score, 8_000);
        assert_eq!(tie.second_score, 8_000);
        assert_eq!(tie.partner_index, Some(0));
        assert!(tie.tied_for_best);
    }

    #[test]
    fn granular_units_fail_closed_for_missing_location_or_unmapped_tokens() {
        let mut missing_location = similarity_occurrence(
            "While systems operate, organizations adapt."
                .chars()
                .collect::<String>()
                .as_str(),
            "While systems operate, organizations adapt."
                .chars()
                .map(SentenceEvidenceToken::Scalar)
                .collect(),
            RecoveryUnitKind::Sentence,
        );
        let mut budget = GranularDiagnosticBudget::default();
        assert!(
            build_granular_units(
                &missing_location,
                0..missing_location.key.len(),
                &mut budget,
            )
            .expect("missing location fails closed")
            .is_empty()
        );
        missing_location.location = positioned_occurrence("x", 1, 0, 0).location;
        missing_location.tokens[0] = SentenceEvidenceToken::Unmapped {
            font_fingerprint: 1,
            glyph_id: 2,
        };
        assert!(
            build_granular_units(
                &missing_location,
                0..missing_location.key.len(),
                &mut budget,
            )
            .expect("unmapped evidence fails closed")
            .is_empty()
        );
    }

    #[test]
    fn granular_budget_stops_are_typed_and_watchless_builds_skip_the_pool() {
        let mut budget = GranularDiagnosticBudget::default();
        assert_eq!(
            budget.charge_units(GranularDiagnosticBudget::UNIT_LIMIT + 1),
            Err(RecoveryWatchGranularStopReason::UnitCountLimit)
        );
        let mut budget = GranularDiagnosticBudget::default();
        assert_eq!(
            budget.charge_token_bytes(GranularDiagnosticBudget::TOKEN_BYTE_LIMIT + 1),
            Err(RecoveryWatchGranularStopReason::TokenByteLimit)
        );
        let mut budget = GranularDiagnosticBudget::default();
        assert_eq!(
            budget.charge_auxiliary::<usize>(GranularDiagnosticBudget::AUXILIARY_ITEM_LIMIT + 1),
            Err(RecoveryWatchGranularStopReason::AuxiliaryLimit)
        );
        let occurrences = [granular_occurrence(
            "While systems operate, organizations adapt.",
            RecoveryUnitKind::Sentence,
        )];
        assert!(
            RecoveryWatchState::new(
                &[],
                &occurrences,
                &occurrences,
                &[],
                RecoveryWatchBuildContext {
                    old_evidence: None,
                    new_evidence: None,
                    old_fully_contained: None,
                    new_fully_contained: None,
                    min_tokens: 1,
                    max_tokens: 100,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn one_sided_watch_caps_retained_occurrences_and_keeps_total_count() {
        let occurrences = (0..=MAX_RECOVERY_WATCH_RETAINED_OCCURRENCES)
            .map(|ordinal| positioned_occurrence("Repeated overlay.", ordinal as u64, ordinal, 0))
            .collect::<Vec<_>>();
        let mut watch = watch_state();
        watch.scan_limit = usize::MAX;
        let lookup = watch
            .locate_one_sided("Repeated overlay.", &occurrences, None, None, 1)
            .expect("bounded exact occurrence scan succeeds");

        let RecoveryWatchOccurrenceEvidence::Occurrences(found) = lookup.evidence else {
            panic!("one-sided scan returns bounded occurrences");
        };
        assert_eq!(
            found.occurrence_count,
            MAX_RECOVERY_WATCH_RETAINED_OCCURRENCES + 1
        );
        assert_eq!(
            found.occurrences.len(),
            MAX_RECOVERY_WATCH_RETAINED_OCCURRENCES
        );
        assert!(!found.complete);
        assert!(!watch.complete);
        assert_eq!(
            watch.retained_one_sided_occurrences,
            MAX_RECOVERY_WATCH_RETAINED_OCCURRENCES
        );
    }

    #[test]
    fn one_sided_watch_budget_failure_retains_nothing() {
        let occurrences = [positioned_occurrence("Observed deletion.", 1, 0, 0)];
        let mut watch = watch_state();
        watch.scan_limit = 0;

        assert!(
            watch
                .locate_one_sided("Observed deletion.", &occurrences, None, None, 1)
                .is_none()
        );
        assert_eq!(watch.retained_one_sided_occurrences, 0);
    }

    #[test]
    fn one_sided_watch_scan_stop_is_unavailable_not_unfound() {
        let occurrences = [positioned_occurrence("Observed deletion.", 1, 0, 0)];
        let watch = RecoveryWatchState::new(
            &[RecoveryWatchQuery {
                id: "stopped",
                old_quote: Some("Observed deletion."),
                new_quote: None,
            }],
            &occurrences,
            &[],
            &[],
            RecoveryWatchBuildContext {
                old_evidence: None,
                new_evidence: None,
                old_fully_contained: None,
                new_fully_contained: None,
                min_tokens: 1,
                max_tokens: 0,
            },
        )
        .expect("stopped watch retains explicit unavailable evidence");

        assert!(!watch.complete);
        assert!(matches!(
            watch.records[0].output.old,
            RecoveryWatchOccurrenceEvidence::Unavailable
        ));
        assert!(matches!(
            watch.records[0].output.new,
            RecoveryWatchOccurrenceEvidence::NotQueried
        ));
    }

    #[test]
    fn one_sided_watch_collects_repeated_adjacent_segments() {
        let occurrences = [
            positioned_occurrence("Alpha", 1, 0, 0),
            positioned_occurrence("Beta", 2, 0, 1),
            positioned_occurrence("Alpha", 3, 1, 0),
            positioned_occurrence("Beta", 4, 1, 1),
        ];
        let mut watch = watch_state();
        let lookup = watch
            .locate_one_sided("Alpha Beta", &occurrences, None, None, 1)
            .expect("bounded segment scan succeeds");

        let RecoveryWatchOccurrenceEvidence::Occurrences(found) = lookup.evidence else {
            panic!("one-sided segment occurrences are retained");
        };
        assert_eq!(found.occurrence_count, 2);
        assert!(found.complete);
        assert!(found.occurrences.iter().all(|occurrence| {
            occurrence.kind == RecoveryWatchUnitKind::Segment
                && occurrence.unit_count == Some(2)
                && occurrence.end_ordinal == Some(2)
        }));
    }

    #[test]
    fn watch_occurrence_uses_single_page_fallback_without_descriptor() {
        let mut occurrence = positioned_occurrence("Fallback page.", 1, 0, 0);
        occurrence.page = Some(7);

        let output = recovery_watch_occurrence(&occurrence, None, false, 1);

        assert_eq!(output.page, Some(7));
        assert!(output.bbox.is_none());
    }

    #[test]
    fn one_sided_segment_page_fallback_requires_one_common_page() {
        let mut same_page = [
            positioned_occurrence("Alpha", 1, 0, 0),
            positioned_occurrence("Beta", 2, 0, 1),
        ];
        same_page[0].page = Some(5);
        same_page[1].page = Some(5);
        let mut watch = watch_state();
        let same = watch
            .locate_one_sided("Alpha Beta", &same_page, None, None, 1)
            .expect("same-page segment scan succeeds");
        let RecoveryWatchOccurrenceEvidence::Occurrences(same) = same.evidence else {
            panic!("same-page segment is retained");
        };
        assert_eq!(same.occurrences[0].page, Some(5));
        assert!(same.occurrences[0].bbox.is_none());

        same_page[1].page = Some(6);
        let mut watch = watch_state();
        let mixed = watch
            .locate_one_sided("Alpha Beta", &same_page, None, None, 1)
            .expect("mixed-page segment scan succeeds");
        let RecoveryWatchOccurrenceEvidence::Occurrences(mixed) = mixed.evidence else {
            panic!("mixed-page segment is retained without page inference");
        };
        assert_eq!(mixed.occurrences[0].page, None);
    }

    #[test]
    fn query_with_both_sides_omitted_is_unavailable_without_scanning() {
        let occurrences = [positioned_occurrence("Must not be scanned.", 1, 0, 0)];
        let watch = RecoveryWatchState::new(
            &[RecoveryWatchQuery {
                id: "metadata-only",
                old_quote: None,
                new_quote: None,
            }],
            &occurrences,
            &occurrences,
            &[],
            RecoveryWatchBuildContext {
                old_evidence: None,
                new_evidence: None,
                old_fully_contained: None,
                new_fully_contained: None,
                min_tokens: 1,
                max_tokens: 0,
            },
        )
        .expect("omitted sides need no scan budget");

        assert_eq!(watch.scan_work, 0);
        assert!(!watch.complete);
        assert!(matches!(
            watch.records[0].output.old,
            RecoveryWatchOccurrenceEvidence::Unavailable
        ));
        assert!(matches!(
            watch.records[0].output.new,
            RecoveryWatchOccurrenceEvidence::Unavailable
        ));
        assert!(watch.records[0].output.pair.is_none());
        assert!(watch.records[0].output.segment_pair.is_none());
    }

    #[test]
    fn recovery_watch_locates_unique_adjacent_segment() {
        let occurrences = [
            positioned_occurrence("2.", 1, 0, 0),
            positioned_occurrence("Each key pair was generated.", 2, 0, 1),
        ];
        let lookup = watch_state()
            .locate(
                "2. Each key pair was generated.",
                &occurrences,
                None,
                None,
                1,
            )
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                span_index: Some(0),
                ordinal: Some(0),
                end_ordinal: Some(2),
                unit_count: Some(2),
                token_count: Some(10),
                recovery_location_available: true,
                role: Some(BlockRole::Body),
                kind: RecoveryWatchUnitKind::Segment,
                ..
            })
        ));
        assert_eq!(lookup.occurrence_index, None);
    }

    #[test]
    fn recovery_segments_enumerate_two_through_eight_units_only() {
        let occurrences = (0..9)
            .map(|ordinal| positioned_scalar('a', ordinal as u64, 0, ordinal))
            .collect::<Vec<_>>();

        assert_eq!(
            collect_recovery_segments(&occurrences[..2], 1, 100)
                .expect("two-unit enumeration fits")
                .len(),
            1
        );
        let eight = collect_recovery_segments(&occurrences[..8], 1, 100)
            .expect("eight-unit enumeration fits");
        assert_eq!(eight.len(), 28);
        assert!(eight.iter().any(|segment| segment.unit_count == 8));
        let nine = collect_recovery_segments(&occurrences, 1, 100)
            .expect("nine-unit input remains bounded");
        assert_eq!(nine.len(), 35);
        assert!(nine.iter().all(|segment| segment.unit_count <= 8));
    }

    #[test]
    fn recovery_segments_fail_closed_on_ineligible_constituents() {
        let valid = [
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
        ];
        assert_eq!(
            collect_recovery_segments(&valid, 1, 10)
                .expect("valid pair builds")
                .len(),
            1
        );
        for mutation in 0..6 {
            let mut occurrences = [
                positioned_scalar('a', 1, 0, 0),
                positioned_scalar('b', 2, 0, 1),
            ];
            match mutation {
                0 => {
                    occurrences[1]
                        .trusted_position
                        .as_mut()
                        .expect("fixture is positioned")
                        .ordinal = 2
                }
                1 => {
                    occurrences[1]
                        .trusted_position
                        .as_mut()
                        .expect("fixture is positioned")
                        .stream_index = 1
                }
                2 => occurrences[1].span_index = Some(1),
                3 => occurrences[1].role = Some(BlockRole::RepeatedHeader),
                4 => occurrences[1].location = None,
                5 => occurrences[1].tokens.push(SentenceEvidenceToken::Unmapped {
                    font_fingerprint: 1,
                    glyph_id: 2,
                }),
                _ => unreachable!(),
            }
            assert!(
                collect_recovery_segments(&occurrences, 1, 10)
                    .expect("ineligible evidence is skipped")
                    .is_empty()
            );
        }
    }

    #[test]
    fn exact_segment_topology_distinguishes_unknown_monotone_and_crossing() {
        let old = [
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
            positioned_scalar('c', 3, 0, 2),
            positioned_scalar('d', 4, 0, 3),
        ];
        let new = [
            positioned_scalar('a', 11, 1, 0),
            positioned_scalar('b', 12, 1, 1),
            positioned_scalar('c', 13, 1, 2),
            positioned_scalar('d', 14, 1, 3),
        ];
        let segments_old = collect_recovery_segments(&old, 1, 100).expect("old segments build");
        let segments_new = collect_recovery_segments(&new, 1, 100).expect("new segments build");
        let old_target = segments_old
            .iter()
            .find(|segment| segment.key.start_ordinal == 1 && segment.key.end_ordinal == 3)
            .expect("target old segment exists");
        let new_target = segments_new
            .iter()
            .find(|segment| segment.key.start_ordinal == 1 && segment.key.end_ordinal == 3)
            .expect("target new segment exists");
        assert_eq!(
            segment_topology(old_target, new_target, &old, &new, &[]),
            Ok((ExactSegmentRelation::ExactUniqueTopologyUnknown, 0))
        );
        let anchors = [
            ExactMatchCandidate {
                old_span_index: 0,
                new_span_index: 0,
                old_occurrence_index: 0,
                new_occurrence_index: 0,
            },
            ExactMatchCandidate {
                old_span_index: 0,
                new_span_index: 0,
                old_occurrence_index: 3,
                new_occurrence_index: 3,
            },
        ];
        assert_eq!(
            segment_topology(old_target, new_target, &old, &new, &anchors),
            Ok((ExactSegmentRelation::ExactUniqueMonotone, 0))
        );

        let moved_new = [
            positioned_scalar('b', 12, 1, 0),
            positioned_scalar('c', 13, 1, 1),
            positioned_scalar('a', 11, 1, 2),
        ];
        let moved_segments =
            collect_recovery_segments(&moved_new, 1, 100).expect("moved segments build");
        let moved_target = moved_segments
            .iter()
            .find(|segment| segment.key.start_ordinal == 0 && segment.key.end_ordinal == 2)
            .expect("moved target segment exists");
        let crossing_anchor = [ExactMatchCandidate {
            old_span_index: 0,
            new_span_index: 0,
            old_occurrence_index: 0,
            new_occurrence_index: 2,
        }];
        assert_eq!(
            segment_topology(old_target, moved_target, &old, &moved_new, &crossing_anchor),
            Ok((ExactSegmentRelation::ExactUniqueCrossing, 1))
        );
    }

    #[test]
    fn segment_hash_collision_requires_full_token_equality() {
        let old_occurrences = [
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
        ];
        let new_occurrences = [
            positioned_scalar('x', 3, 1, 0),
            positioned_scalar('y', 4, 1, 1),
        ];
        let old = collect_recovery_segments(&old_occurrences, 1, 10)
            .expect("old collision fixture builds")
            .remove(0);
        let mut new = collect_recovery_segments(&new_occurrences, 1, 10)
            .expect("new collision fixture builds")
            .remove(0);
        new.exact_hash = old.exact_hash;

        assert!(!exact_segment_tokens_match(
            &old,
            &new,
            &old_occurrences,
            &new_occurrences
        ));
    }

    #[test]
    fn segment_diagnostics_count_duplicates_and_fail_atomically_at_budget() {
        let old = (0..3)
            .map(|ordinal| positioned_scalar('a', ordinal + 1, 0, ordinal as usize))
            .collect::<Vec<_>>();
        let new = (0..3)
            .map(|ordinal| positioned_scalar('a', ordinal + 11, 1, ordinal as usize))
            .collect::<Vec<_>>();
        let analysis =
            analyze_recovery_segments(&old, &new, &[], 1, 100).expect("duplicate diagnostics fit");
        assert_eq!(analysis.segment_duplicate_pairs, 4);
        assert_eq!(analysis.segment_unique_pairs, 1);
    }

    #[test]
    fn segment_descriptor_and_token_budgets_stop_before_unbounded_growth() {
        let mut occurrence_index_budget = SegmentDiagnosticBudget::default();
        assert!(matches!(
            occurrence_index_budget.charge_auxiliary::<(TrustedStreamPosition, Option<usize>)>(
                SegmentDiagnosticBudget::AUXILIARY_ITEM_LIMIT + 1,
            ),
            Err(SegmentStopReason::CandidateCountLimit)
        ));

        let many = (0..2_400)
            .map(|ordinal| positioned_scalar('a', ordinal + 1, 0, ordinal as usize))
            .collect::<Vec<_>>();
        assert!(matches!(
            analyze_recovery_segments(&many, &[], &[], 1, usize::MAX),
            Err(SegmentStopReason::CandidateCountLimit)
        ));

        let mut long = [
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
        ];
        long[0]
            .tokens
            .resize(1_024, SentenceEvidenceToken::Scalar('a'));
        long[1]
            .tokens
            .resize(1_024, SentenceEvidenceToken::Scalar('b'));
        let mut budget = SegmentDiagnosticBudget {
            token_elements: SegmentDiagnosticBudget::TOKEN_ELEMENT_LIMIT - 1_000,
            ..SegmentDiagnosticBudget::default()
        };
        assert!(matches!(
            collect_recovery_segments_with_budget(&long, 1, &mut budget),
            Err(SegmentStopReason::TokenVerificationLimit)
        ));
    }

    #[test]
    fn segment_topology_skips_exact_candidates_without_stream_positions() {
        let mut old = vec![
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
            positioned_scalar('x', 3, 2, 0),
        ];
        let mut new = vec![
            positioned_scalar('a', 11, 1, 0),
            positioned_scalar('b', 12, 1, 1),
            positioned_scalar('x', 13, 3, 0),
        ];
        old[2].trusted_position = None;
        new[2].trusted_position = None;
        let exact = [ExactMatchCandidate {
            old_span_index: 0,
            new_span_index: 0,
            old_occurrence_index: 2,
            new_occurrence_index: 2,
        }];

        let mut analysis = analyze_recovery_segments(&old, &new, &exact, 1, 100)
            .expect("non-positioned unit anchors are non-applicable");
        assert_eq!(analysis.segment_candidates, 2);
        assert_eq!(analysis.segment_unique_pairs, 1);
        let evidence = watched_segment_pair_evidence(&mut analysis, 0, 0, &old, &new, &exact)
            .expect("watched output fits");
        assert_eq!(
            evidence.relation,
            ExactSegmentRelation::ExactUniqueTopologyUnknown
        );
    }

    #[test]
    fn segment_overlap_is_diagnostic_only_and_counted_once() {
        let old = [
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
        ];
        let new = [
            positioned_scalar('a', 11, 1, 0),
            positioned_scalar('b', 12, 1, 1),
        ];
        let analysis = analyze_recovery_segments(&old, &new, &[], 1, 100)
            .expect("unique segment diagnostics fit");
        let plan = SentenceRecoveryPlan {
            deletions: vec![RecoveredSentence {
                span_index: 0,
                kind: RecoveryUnitKind::Sentence,
                role: OccurrenceRole::Body,
                blocks: vec![BlockId(1)],
                separator: None,
                canonical: ScalarRange { start: 0, end: 1 },
                comparable: TokenRange { start: 0, end: 1 },
                source_tokens: 1,
            }],
            deletion_consumed: old[0]
                .location
                .as_ref()
                .expect("fixture has a location")
                .consumed
                .clone(),
            ..SentenceRecoveryPlan::default()
        };
        let watch = RecoveryWatchState {
            complete: true,
            candidate_generation_complete: true,
            near_relation_complete: true,
            near_relation_stop_reason: None,
            segment_analysis: analysis,
            segment_stop_reason: None,
            segment_overlap_vetoes: 0,
            granular_complete: true,
            granular_old_units: 0,
            granular_new_units: 0,
            granular_pair_comparisons: 0,
            granular_stop_reason: None,
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
            old_pair_partners: HashMap::new(),
            new_pair_partners: HashMap::new(),
            scan_work: 0,
            scan_limit: 0,
            retained_one_sided_occurrences: 0,
        };

        let diagnostics = watch.finish(Some(&plan));
        assert_eq!(diagnostics.segment_overlap_vetoes, 1);
        assert!(diagnostics.complete);
        assert_eq!(plan.deletions.len(), 1);
    }

    #[test]
    fn overlap_work_limit_atomically_discards_segment_claims() {
        let old = [
            positioned_scalar('a', 1, 0, 0),
            positioned_scalar('b', 2, 0, 1),
        ];
        let new = [
            positioned_scalar('a', 11, 1, 0),
            positioned_scalar('b', 12, 1, 1),
        ];
        let mut watch = RecoveryWatchState::new(
            &[RecoveryWatchQuery {
                id: "bounded-overlap",
                old_quote: Some("a b"),
                new_quote: Some("a b"),
            }],
            &old,
            &new,
            &[],
            RecoveryWatchBuildContext {
                old_evidence: None,
                new_evidence: None,
                old_fully_contained: None,
                new_fully_contained: None,
                min_tokens: 1,
                max_tokens: 100,
            },
        )
        .expect("watch state builds");
        assert!(watch.records[0].output.segment_pair.is_some());
        watch.candidate_generation_complete = true;
        watch.near_relation_complete = true;
        watch.segment_analysis.budget.overlap_comparisons =
            SegmentDiagnosticBudget::OVERLAP_COMPARISON_LIMIT;
        let plan = SentenceRecoveryPlan {
            deletion_consumed: vec![LocalSentenceRange {
                block: BlockId(999),
                canonical: ScalarRange { start: 0, end: 1 },
                comparable: TokenRange { start: 0, end: 1 },
            }],
            ..SentenceRecoveryPlan::default()
        };

        let diagnostics = watch.finish(Some(&plan));
        assert_eq!(
            diagnostics.segment_stop_reason,
            Some(SegmentStopReason::HashPairVisitLimit)
        );
        assert_eq!(diagnostics.segment_candidates, 0);
        assert_eq!(diagnostics.segment_unique_pairs, 0);
        assert_eq!(diagnostics.segment_overlap_vetoes, 0);
        assert_eq!(diagnostics.records[0].segment_pair, None);
    }

    #[test]
    fn recovery_watch_segment_quote_may_end_inside_the_latest_unit() {
        let occurrences = [
            positioned_occurrence("2.", 1, 0, 0),
            positioned_occurrence("Each key pair was generated.", 2, 0, 1),
        ];
        let lookup = watch_state()
            .locate("2. Each key pair", &occurrences, None, None, 1)
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                kind: RecoveryWatchUnitKind::Segment,
                ..
            })
        ));
        assert_eq!(lookup.segment_key, None);
    }

    #[test]
    fn partial_segment_quotes_do_not_inherit_enclosing_unit_exactness() {
        let old = [
            positioned_occurrence("2.", 1, 0, 0),
            positioned_occurrence("Each key pair was generated.", 2, 0, 1),
        ];
        let new = [
            positioned_occurrence("2.", 3, 1, 0),
            positioned_occurrence("Each key pair was generated.", 4, 1, 1),
        ];
        let watch = RecoveryWatchState::new(
            &[RecoveryWatchQuery {
                id: "partial-segments",
                old_quote: Some("2. Each key"),
                new_quote: Some("2. Each key pair"),
            }],
            &old,
            &new,
            &[],
            RecoveryWatchBuildContext {
                old_evidence: None,
                new_evidence: None,
                old_fully_contained: None,
                new_fully_contained: None,
                min_tokens: 1,
                max_tokens: 100,
            },
        )
        .expect("partial segment watch builds");

        assert!(matches!(
            watch.records[0].output.old,
            RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                kind: RecoveryWatchUnitKind::Segment,
                ..
            })
        ));
        assert!(matches!(
            watch.records[0].output.new,
            RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                kind: RecoveryWatchUnitKind::Segment,
                ..
            })
        ));
        assert_eq!(watch.records[0].output.segment_pair, None);
    }

    #[test]
    fn recovery_watch_locates_segment_without_recovery_locations() {
        let mut occurrences = [
            positioned_occurrence("Resolved alpha.", 1, 0, 0),
            positioned_occurrence("Resolved beta.", 2, 0, 1),
        ];
        occurrences[0].location = None;
        occurrences[1].location = None;
        let lookup = watch_state()
            .locate(
                "Resolved alpha. Resolved beta.",
                &occurrences,
                None,
                None,
                1,
            )
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                recovery_location_available: false,
                kind: RecoveryWatchUnitKind::Segment,
                ..
            })
        ));
    }

    #[test]
    fn recovery_watch_reports_unique_non_scalar_segment_as_unavailable() {
        let mut occurrences = [
            positioned_occurrence("Unmapped alpha.", 1, 0, 0),
            positioned_occurrence("Unmapped beta.", 2, 0, 1),
        ];
        occurrences[1].tokens.push(SentenceEvidenceToken::Unmapped {
            font_fingerprint: 1,
            glyph_id: 1,
        });
        let lookup = watch_state()
            .locate(
                "Unmapped alpha. Unmapped beta.",
                &occurrences,
                None,
                None,
                1,
            )
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Unavailable
        ));
    }

    #[test]
    fn recovery_watch_counts_unavailable_segment_hits_before_classification() {
        let mut occurrences = [
            positioned_occurrence("Unmapped alpha.", 1, 0, 0),
            positioned_occurrence("Unmapped beta.", 2, 0, 1),
            positioned_occurrence("Unmapped alpha.", 3, 1, 0),
            positioned_occurrence("Unmapped beta.", 4, 1, 1),
        ];
        for occurrence in &mut occurrences {
            occurrence.tokens.push(SentenceEvidenceToken::Unmapped {
                font_fingerprint: 1,
                glyph_id: 1,
            });
        }
        let lookup = watch_state()
            .locate(
                "Unmapped alpha. Unmapped beta.",
                &occurrences,
                None,
                None,
                1,
            )
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Ambiguous
        ));
    }

    #[test]
    fn recovery_watch_segment_is_not_recounted_in_longer_super_windows() {
        let occurrences = [
            positioned_occurrence("Alpha.", 1, 0, 0),
            positioned_occurrence("Beta continues.", 2, 0, 1),
            positioned_occurrence("Gamma follows.", 3, 0, 2),
        ];
        let lookup = watch_state()
            .locate("Alpha. Beta", &occurrences, None, None, 1)
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Found(RecoveryWatchOccurrence {
                kind: RecoveryWatchUnitKind::Segment,
                ..
            })
        ));
    }

    #[test]
    fn recovery_watch_reports_duplicate_adjacent_segments_as_ambiguous() {
        let occurrences = [
            positioned_occurrence("Alpha.", 1, 0, 0),
            positioned_occurrence("Beta.", 2, 0, 1),
            positioned_occurrence("Alpha.", 3, 1, 0),
            positioned_occurrence("Beta.", 4, 1, 1),
        ];
        let lookup = watch_state()
            .locate("Alpha. Beta.", &occurrences, None, None, 1)
            .expect("bounded segment lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Ambiguous
        ));
    }

    #[test]
    fn recovery_watch_rejects_nonconsecutive_and_cross_stream_segments() {
        for occurrences in [
            [
                positioned_occurrence("Alpha.", 1, 0, 0),
                positioned_occurrence("Beta.", 2, 0, 2),
            ],
            [
                positioned_occurrence("Alpha.", 3, 0, 0),
                positioned_occurrence("Beta.", 4, 1, 1),
            ],
        ] {
            let lookup = watch_state()
                .locate("Alpha. Beta.", &occurrences, None, None, 1)
                .expect("bounded segment lookup succeeds");
            assert!(matches!(
                lookup.evidence,
                RecoveryWatchOccurrenceEvidence::Unfound
            ));
        }
    }

    #[test]
    fn recovery_watch_reports_single_and_segment_matches_as_ambiguous() {
        let occurrences = [
            positioned_occurrence("Alpha. Beta.", 1, 0, 0),
            positioned_occurrence("Alpha.", 2, 1, 0),
            positioned_occurrence("Beta.", 3, 1, 1),
        ];
        let lookup = watch_state()
            .locate("Alpha. Beta.", &occurrences, None, None, 1)
            .expect("bounded lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Ambiguous
        ));
        assert_eq!(lookup.occurrence_index, None);
    }

    #[test]
    fn recovery_watch_unavailable_segment_competes_with_single_match() {
        let mut occurrences = [
            positioned_occurrence("Target phrase.", 1, 0, 0),
            positioned_occurrence("Target", 2, 1, 0),
            positioned_occurrence("phrase.", 3, 1, 1),
        ];
        occurrences[2].tokens.push(SentenceEvidenceToken::Unmapped {
            font_fingerprint: 1,
            glyph_id: 1,
        });
        let lookup = watch_state()
            .locate("Target phrase.", &occurrences, None, None, 1)
            .expect("bounded lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Ambiguous
        ));
    }

    #[test]
    fn recovery_watch_counts_overlapping_segment_starts_as_distinct_hits() {
        let occurrences = [
            positioned_occurrence("very very", 1, 0, 0),
            positioned_occurrence("very", 2, 0, 1),
            positioned_occurrence("very", 3, 0, 2),
        ];
        let lookup = watch_state()
            .locate("very very", &occurrences, None, None, 1)
            .expect("bounded overlapping lookup succeeds");

        assert!(matches!(
            lookup.evidence,
            RecoveryWatchOccurrenceEvidence::Ambiguous
        ));
    }

    #[test]
    fn recovery_watch_segment_pair_does_not_claim_exact_count_availability() {
        let old = [
            positioned_occurrence("Old alpha.", 1, 0, 0),
            positioned_occurrence("Old beta.", 2, 0, 1),
        ];
        let new = [
            positioned_occurrence("New alpha.", 3, 0, 0),
            positioned_occurrence("New beta.", 4, 0, 1),
        ];
        let watch = RecoveryWatchState::new(
            &[RecoveryWatchQuery {
                id: "segment-pair",
                old_quote: Some("Old alpha. Old beta."),
                new_quote: Some("New alpha. New beta."),
            }],
            &old,
            &new,
            &[],
            RecoveryWatchBuildContext {
                old_evidence: None,
                new_evidence: None,
                old_fully_contained: None,
                new_fully_contained: None,
                min_tokens: 1,
                max_tokens: 100,
            },
        )
        .expect("segment watch state builds");
        let pair = watch.records[0]
            .output
            .pair
            .as_ref()
            .expect("same-span segments retain pair diagnostics");

        assert!(pair.same_span);
        assert!(!pair.exact_shared_units_available);
        assert!(watch.pair_by_occurrences.is_empty());
    }

    #[test]
    fn recovery_watch_exact_count_is_unavailable_without_run_descriptors() {
        let old = [positioned_occurrence("Old single.", 1, 0, 0)];
        let new = [positioned_occurrence("New single.", 2, 0, 0)];
        let mut watch = RecoveryWatchState::new(
            &[RecoveryWatchQuery {
                id: "untrusted-pair",
                old_quote: Some("Old single."),
                new_quote: Some("New single."),
            }],
            &old,
            &new,
            &[],
            RecoveryWatchBuildContext {
                old_evidence: None,
                new_evidence: None,
                old_fully_contained: None,
                new_fully_contained: None,
                min_tokens: 1,
                max_tokens: 100,
            },
        )
        .expect("single watch state builds");
        watch
            .record_exact_candidates(&[], &old, &new)
            .expect("empty candidate count computes");
        let pair = watch.records[0]
            .output
            .pair
            .as_ref()
            .expect("same-span singles retain pair diagnostics");

        assert_eq!(pair.exact_shared_units, 0);
        assert!(!pair.exact_shared_units_available);
    }

    fn indexed_occurrence(
        tokens: &[char],
        kind: RecoveryUnitKind,
        role: Option<BlockRole>,
    ) -> SentenceOccurrence {
        SentenceOccurrence {
            key: String::new(),
            tokens: tokens
                .iter()
                .copied()
                .map(SentenceEvidenceToken::Scalar)
                .collect(),
            word_ranges: Vec::new(),
            kind,
            role,
            location: None,
            span_index: None,
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }
    }

    #[test]
    fn production_sentence_edge_filter_prunes_before_pair_charge() {
        let mut query = indexed_occurrence(
            &['a', 'b', 'c', 'd'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        query.span_index = Some(0);
        let mut occurrence = indexed_occurrence(
            &['w', 'x', 'y', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        occurrence.span_index = Some(0);
        let occurrences = [occurrence];
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");

        let filtered =
            classify_sentence_edge_filter_query(&query, &occurrences, &[0], &mut budget, |_| true);

        assert_eq!(filtered.len(&[0]), 0);
        assert_eq!(budget.pair_visits, 0);
        assert_eq!(budget.comparisons, 0);
        assert_eq!(budget.sentence_edge_filter_pairs_rejected, 1);
    }

    #[test]
    fn production_sentence_edge_filter_reuses_edge_evidence_and_logically_charges_it() {
        let query = indexed_occurrence(
            &['a', 'b', 'c', 'x'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let occurrences = [indexed_occurrence(
            &['a', 'b', 'c', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let mut legacy_budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
        let legacy_score = sentence_similarity_in_scope(
            &query,
            &occurrences[0],
            &mut legacy_budget,
            TEST_NEAR_SCOPE,
        )
        .expect("legacy score computes");
        let legacy_comparisons = legacy_budget.comparisons;
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
        let filtered =
            classify_sentence_edge_filter_query(&query, &occurrences, &[0], &mut budget, |_| true);
        let (occurrence_index, cached) = filtered.pair(&[0], 0).expect("pair is retained");
        assert!(budget.charge_pair_visits_in_scope(1, query.kind, TEST_NEAR_SCOPE));

        let score = score_and_record_sentence_edge_gate_shadow(
            &query,
            &occurrences[occurrence_index],
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            None,
            None,
            Some(0),
            None,
            cached,
        )
        .expect("cached score computes");

        assert_eq!(score, legacy_score);
        assert_eq!(budget.comparisons, legacy_comparisons);
        assert_eq!(budget.pair_visits, 1);
    }

    #[test]
    fn production_sentence_edge_filter_leaves_lines_on_legacy_path() {
        let query = indexed_occurrence(&['a', 'b'], RecoveryUnitKind::Line, Some(BlockRole::Body));
        let occurrences = [indexed_occurrence(
            &['x', 'y'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let mut budget = RecoveryBudget::new(2, 2, 4, 1).expect("budget is valid");

        let filtered =
            classify_sentence_edge_filter_query(&query, &occurrences, &[0], &mut budget, |_| true);

        assert!(matches!(filtered, SentenceEdgeFilterQuery::Legacy));
        assert_eq!(budget.sentence_edge_filter_pairs, 0);
        assert_eq!(budget.sentence_edge_filter_comparisons, 0);
    }

    #[test]
    fn production_sentence_edge_filter_query_failure_falls_back_atomically() {
        let query = indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let occurrences = [indexed_occurrence(
            &['a', 'x', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let mut budget = RecoveryBudget::new(3, 3, 6, 1).expect("budget is valid");
        budget.sentence_edge_filter_comparison_limit = 1;

        let filtered =
            classify_sentence_edge_filter_query(&query, &occurrences, &[0], &mut budget, |_| true);

        assert!(matches!(filtered, SentenceEdgeFilterQuery::Legacy));
        assert_eq!(budget.pair_visits, 0);
        assert_eq!(budget.comparisons, 0);
        assert_eq!(budget.sentence_edge_filter_pairs_retained, 0);
        assert_eq!(budget.sentence_edge_filter_pairs_rejected, 0);
        assert_eq!(
            budget.sentence_edge_filter_stop_reason,
            Some(SentenceEdgeFilterStopReason::SimilarityComparisonLimit)
        );
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        record_near_search_metrics(&mut diagnostics, &budget);
        let filter_metrics = diagnostics.expect("metrics remain").metrics;
        assert!(!filter_metrics.sentence_edge_filter_complete);
        assert_eq!(
            filter_metrics.sentence_edge_filter_stop_reason,
            Some(SentenceEdgeFilterStopReason::SimilarityComparisonLimit)
        );
        assert!(budget.charge_pair_visits_in_scope(1, query.kind, TEST_NEAR_SCOPE));
        assert!(
            sentence_similarity_in_scope(&query, &occurrences[0], &mut budget, TEST_NEAR_SCOPE,)
                .is_some()
        );
    }

    #[test]
    fn production_sentence_edge_filter_splits_only_retained_pairs() {
        let mut query = indexed_occurrence(
            &['a', 'b', 'c', 'x'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        query.span_index = Some(1);
        let mut same = indexed_occurrence(
            &['a', 'b', 'c', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        same.span_index = Some(1);
        let ambiguous = indexed_occurrence(
            &['a', 'b', 'c', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let mut weak = indexed_occurrence(
            &['q', 'r', 's', 't'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        weak.span_index = Some(1);
        let occurrences = [same, ambiguous, weak];
        let mut budget = RecoveryBudget::new(12, 12, 24, 1).expect("budget is valid");
        let filtered = classify_sentence_edge_filter_query(
            &query,
            &occurrences,
            &[0, 1, 2],
            &mut budget,
            |_| true,
        );
        let SentenceEdgeFilterQuery::Filtered { retained, .. } = filtered else {
            panic!("query should be filtered");
        };

        assert_eq!(
            split_retained_sentence_edge_filters(&retained, query.span_index, &occurrences)
                .expect("split computes"),
            NearSearchWorkSplit {
                same_known: 1,
                ambiguous: 1,
                shared: 0,
            }
        );
    }

    #[test]
    fn production_sentence_edge_filter_rejects_low_edge_ambiguous_competitor() {
        let mut query = indexed_occurrence(
            &['a', 'b', 'c', 'd'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        query.span_index = Some(0);
        let occurrences = [indexed_occurrence(
            &['w', 'x', 'y', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");

        let filtered =
            classify_sentence_edge_filter_query(&query, &occurrences, &[0], &mut budget, |_| true);

        assert_eq!(filtered.len(&[0]), 0);
        assert_eq!(budget.sentence_edge_filter_pairs_rejected, 1);
    }

    #[test]
    fn production_sentence_edge_filter_can_examine_more_pairs_than_production_limit() {
        let mut query =
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        query.span_index = Some(0);
        let mut retained =
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        retained.span_index = Some(0);
        let mut rejected_one =
            indexed_occurrence(&['x'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        rejected_one.span_index = Some(0);
        let mut rejected_two =
            indexed_occurrence(&['y'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        rejected_two.span_index = Some(0);
        let occurrences = [retained, rejected_one, rejected_two];
        let mut budget = RecoveryBudget::new(2, 3, 5, 1).expect("budget is valid");

        for _ in 0..2 {
            let filtered = classify_sentence_edge_filter_query(
                &query,
                &occurrences,
                &[0, 1, 2],
                &mut budget,
                |_| true,
            );
            assert_eq!(filtered.len(&[0, 1, 2]), 1);
            assert!(budget.charge_pair_visits_in_scope(1, query.kind, TEST_NEAR_SCOPE));
        }

        assert_eq!(budget.sentence_edge_filter_pairs, 6);
        assert!(budget.sentence_edge_filter_pairs > budget.token_limit);
        assert_eq!(budget.pair_visits, 2);
        assert!(budget.pair_visits <= budget.token_limit);
        assert!(budget.sentence_edge_filter_stop_reason.is_none());
    }

    #[test]
    fn production_sentence_edge_filter_records_rejected_watch_pairs_in_both_directions() {
        let mut old = positioned_occurrence("Old watched sentence.", 1, 0, 0);
        old.tokens = vec![SentenceEvidenceToken::Scalar('a'); 4];
        let mut new = positioned_occurrence("New watched sentence.", 2, 1, 0);
        new.tokens = vec![SentenceEvidenceToken::Scalar('x'); 4];
        for query_side in [OccurrenceSide::Old, OccurrenceSide::New] {
            let mut watch = RecoveryWatchState::new(
                &[RecoveryWatchQuery {
                    id: "filtered-watch",
                    old_quote: Some("Old watched sentence."),
                    new_quote: Some("New watched sentence."),
                }],
                std::slice::from_ref(&old),
                std::slice::from_ref(&new),
                &[],
                RecoveryWatchBuildContext {
                    old_evidence: None,
                    new_evidence: None,
                    old_fully_contained: None,
                    new_fully_contained: None,
                    min_tokens: 1,
                    max_tokens: 16,
                },
            )
            .expect("watch builds");
            let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
            let (query, occurrences) = match query_side {
                OccurrenceSide::Old => (&old, std::slice::from_ref(&new)),
                OccurrenceSide::New => (&new, std::slice::from_ref(&old)),
            };
            let filtered =
                classify_sentence_edge_filter_query(query, occurrences, &[0], &mut budget, |_| {
                    true
                });
            assert!(matches!(
                filtered,
                SentenceEdgeFilterQuery::Filtered {
                    rejected: Some(_),
                    ..
                }
            ));
            filtered.record_rejections(
                0,
                query_side,
                query,
                occurrences,
                NearSearchScope::CrossSpan,
                None,
                Some(&mut watch),
                RecoveryWatchNearScope::CrossSpan,
            );

            let pair = watch.records[0]
                .output
                .pair
                .as_ref()
                .expect("pair diagnostics exist");
            assert!(pair.near_candidate_examined);
            assert_eq!(pair.near_score, Some(0));
            assert_eq!(pair.near_scope, Some(RecoveryWatchNearScope::CrossSpan));
            assert_eq!(budget.pair_visits, 0);
            assert_eq!(budget.comparisons, 0);
        }
    }

    #[test]
    fn direct_sentence_edge_filter_streams_rejected_watch_without_storing_it() {
        let old = [edge_probe_occurrence("abcdefghij")];
        let new = [edge_probe_occurrence("aklmnopqrs")];
        let mut watch = watch_state_for_pair();
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("budget builds");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;

        let filtered = classify_sentence_edge_filter_query_with_index(
            &old[0],
            &new,
            &[0],
            &mut budget,
            None,
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: 0,
                query_side: OccurrenceSide::Old,
                scope: NearSearchScope::CrossSpan,
                shadow: None,
                watch: Some(&mut watch),
                watch_scope: RecoveryWatchNearScope::CrossSpan,
            }),
            |_| true,
        );

        assert!(matches!(
            filtered,
            SentenceEdgeFilterQuery::Filtered { rejected: None, .. }
        ));
        let pair = watch.records[0]
            .output
            .pair
            .as_ref()
            .expect("pair diagnostics exist");
        assert!(pair.near_candidate_examined);
        assert_eq!(pair.near_score, Some(1_000));
        assert_eq!(pair.near_scope, Some(RecoveryWatchNearScope::CrossSpan));
    }

    #[test]
    fn direct_sentence_edge_rejection_observer_failure_stops_the_build() {
        let old = [edge_probe_occurrence("abcdefghij")];
        let new = [edge_probe_occurrence("aklmnopqrs")];
        let mut watch = watch_state_for_pair();
        watch.complete = false;
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("budget builds");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;

        let filtered = classify_sentence_edge_filter_query_with_index(
            &old[0],
            &new,
            &[0],
            &mut budget,
            None,
            Some(SentenceEdgeRejectionObserver {
                query_occurrence_index: 0,
                query_side: OccurrenceSide::Old,
                scope: NearSearchScope::CrossSpan,
                shadow: None,
                watch: Some(&mut watch),
                watch_scope: RecoveryWatchNearScope::CrossSpan,
            }),
            |_| true,
        );

        assert!(matches!(filtered, SentenceEdgeFilterQuery::Legacy));
        assert_eq!(
            budget.watch_probe_stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure)
        );
        assert_eq!(budget.sentence_edge_filter_pairs_retained, 0);
        assert_eq!(budget.sentence_edge_filter_pairs_rejected, 0);
    }

    #[test]
    fn production_filter_fallback_keeps_edge_gate_shadow_incomplete() {
        let query = indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let occurrences = [indexed_occurrence(
            &['a', 'x', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut relations = empty_modified_sentence_relations(&candidates, &candidates)
            .expect("relations allocate");
        enable_sentence_edge_gate_shadow(&mut relations);
        let mut budget = RecoveryBudget::new(3, 3, 6, 1).expect("budget is valid");
        budget.sentence_edge_filter_comparison_limit = 1;
        let filtered =
            classify_sentence_edge_filter_query(&query, &occurrences, &[0], &mut budget, |_| true);
        assert!(matches!(filtered, SentenceEdgeFilterQuery::Legacy));
        mark_edge_gate_shadow_for_filter_stop(
            relations.edge_gate_shadow.as_mut(),
            budget.sentence_edge_filter_stop_reason,
        );
        relations.complete = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        record_sentence_edge_gate_shadow(&mut diagnostics, &relations, None);

        let shadow = diagnostics
            .expect("metrics remain")
            .metrics
            .sentence_edge_gate_shadow
            .expect("shadow remains");
        assert!(!shadow.complete);
        assert_eq!(
            shadow.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::SimilarityComparisonLimit)
        );
    }

    #[test]
    fn final_metrics_never_publish_complete_shadow_for_incomplete_near_relation() {
        let mut budget = RecoveryBudget::new(2, 2, 4, 1).expect("budget is valid");
        budget.near_relation_stop_reason = Some(NearRelationStopReason::PairVisitLimit);
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                near_relation_complete: false,
                sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetrics {
                    complete: true,
                    ..SentenceEdgeGateShadowMetrics::default()
                }),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        record_near_search_metrics(&mut diagnostics, &budget);

        let shadow = diagnostics
            .expect("metrics remain")
            .metrics
            .sentence_edge_gate_shadow
            .expect("shadow remains");
        assert!(!shadow.complete);
        assert_eq!(
            shadow.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::PairVisitLimit)
        );
    }

    #[test]
    fn sentence_edge_gate_shadow_rejects_weak_sentences_and_retains_lines() {
        let mut sentence_old = indexed_occurrence(
            &['a', 'x', 'y', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let mut sentence_new = indexed_occurrence(
            &['a', 'q', 'r', 's'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        sentence_old.span_index = Some(0);
        sentence_new.span_index = Some(0);
        let line_old = indexed_occurrence(
            &['a', 'x', 'y', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let line_new = indexed_occurrence(
            &['a', 'q', 'r', 's'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let mut budget = RecoveryBudget::new(8, 8, 16, 1).expect("budget is valid");
        let mut relations = empty_modified_sentence_relations(
            &[RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }],
            &[RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }],
        )
        .expect("relations allocate");
        enable_sentence_edge_gate_shadow(&mut relations);

        let sentence_score = score_and_record_sentence_edge_gate_shadow(
            &sentence_old,
            &sentence_new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            None,
            relations.edge_gate_shadow.as_mut(),
            Some(0),
            Some(0),
            None,
        )
        .expect("sentence score computes");
        let line_score = score_and_record_sentence_edge_gate_shadow(
            &line_old,
            &line_new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            None,
            relations.edge_gate_shadow.as_mut(),
            Some(0),
            Some(0),
            None,
        )
        .expect("line score computes");
        assert_eq!(
            relations
                .edge_gate_shadow
                .as_ref()
                .expect("shadow remains")
                .old[0]
                .best_score,
            line_score
        );
        let metrics = relations.edge_gate_shadow.expect("shadow remains").metrics;
        assert_eq!(sentence_score, 2_500);
        assert_eq!(line_score, 2_500);
        assert_eq!(metrics.pairs_considered, 1);
        assert_eq!(metrics.pairs_rejected, 1);
        assert_eq!(metrics.pairs_retained, 0);
        assert_eq!(metrics.rejected_max_production_score, 2_500);
        assert_eq!(metrics.threshold_violations, 0);
        assert_eq!(metrics.same_known_rejected, 1);
        assert_eq!(metrics.projected_pair_visits, 0);
        assert_eq!(metrics.projected_similarity_comparisons, 0);
    }

    #[test]
    fn sentence_edge_gate_shadow_compares_decisions_and_marks_stops() {
        let old = indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        let new = indexed_occurrence(&['b'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        let mut inconsistent = SentenceEdgeGateShadowMetrics::default();
        record_sentence_edge_gate_rejection(
            &mut inconsistent,
            TEST_NEAR_SCOPE,
            &old,
            &new,
            MIN_WORD_SCORE_EDGE_EVIDENCE,
        )
        .expect("inconsistent rejection is recorded");
        assert_eq!(inconsistent.threshold_violations, 1);

        let mut production = CandidateNearRelation::default();
        production.record_eligible(0, MIN_NEAR_SCORE);
        let relations = ModifiedSentenceRelations {
            old: vec![production],
            new: vec![production],
            complete: false,
            edge_gate_shadow: Some(SentenceEdgeGateShadow {
                old: vec![CandidateNearRelation::default()],
                new: vec![CandidateNearRelation::default()],
                metrics: SentenceEdgeGateShadowMetrics::default(),
                active: true,
            }),
            edge_signature_shadow: None,
        };
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        record_sentence_edge_gate_shadow(
            &mut diagnostics,
            &relations,
            Some(NearRelationStopReason::PairVisitLimit),
        );
        let metrics = diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_gate_shadow
            .expect("shadow metrics exist");
        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::PairVisitLimit)
        );
        assert_eq!(metrics.veto_mismatches, 2);
        assert_eq!(metrics.unique_partner_mismatches, 2);
        assert_eq!(metrics.reciprocal_pair_mismatches, 1);
        assert_eq!(metrics.adopted_replacement_mismatches, 1);
        assert_eq!(metrics.insertion_deletion_veto_mismatches, 2);
    }

    #[test]
    fn sentence_edge_gate_shadow_retains_strong_sentence_and_attributes_rejections() {
        let mut old = indexed_occurrence(
            &['a', 'b', 'x'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let mut new = indexed_occurrence(
            &['a', 'b', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        old.span_index = Some(0);
        new.span_index = Some(0);
        let mut budget = RecoveryBudget::new(6, 6, 12, 1).expect("budget is valid");
        let mut relations = empty_modified_sentence_relations(
            &[RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }],
            &[RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }],
        )
        .expect("relations allocate");
        enable_sentence_edge_gate_shadow(&mut relations);
        let score = score_and_record_sentence_edge_gate_shadow(
            &old,
            &new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            None,
            relations.edge_gate_shadow.as_mut(),
            Some(0),
            Some(0),
            None,
        )
        .expect("sentence score computes");
        assert_eq!(score, 6_666);
        let metrics = relations
            .edge_gate_shadow
            .as_ref()
            .expect("shadow exists")
            .metrics;
        assert_eq!(metrics.pairs_retained, 1);
        assert_eq!(metrics.projected_pair_visits, 1);
        assert_eq!(metrics.projected_similarity_comparisons, budget.comparisons);

        new.tokens = vec![
            SentenceEvidenceToken::Scalar('q'),
            SentenceEvidenceToken::Scalar('r'),
            SentenceEvidenceToken::Scalar('s'),
        ];
        new.span_index = Some(1);
        score_and_record_sentence_edge_gate_shadow(
            &old,
            &new,
            &mut budget,
            NearSearchScope::CrossSpan,
            NearSearchWorkClass::Shared,
            None,
            relations.edge_gate_shadow.as_mut(),
            Some(0),
            Some(0),
            None,
        )
        .expect("cross-span score computes");
        assert_eq!(
            relations
                .edge_gate_shadow
                .as_ref()
                .expect("shadow remains")
                .metrics
                .cross_span_rejected,
            1
        );
        let metrics = relations
            .edge_gate_shadow
            .as_ref()
            .expect("shadow remains")
            .metrics;
        assert_eq!(
            metrics.pairs_rejected,
            metrics
                .same_known_rejected
                .checked_add(metrics.ambiguous_rejected)
                .and_then(|count| count.checked_add(metrics.cross_span_rejected))
                .and_then(|count| count.checked_add(metrics.unclassified_rejected))
                .expect("attribution sum fits")
        );
        new.span_index = None;
        score_and_record_sentence_edge_gate_shadow(
            &old,
            &new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            None,
            relations.edge_gate_shadow.as_mut(),
            Some(0),
            Some(0),
            None,
        )
        .expect("ambiguous score computes");
        assert_eq!(
            relations
                .edge_gate_shadow
                .as_ref()
                .expect("shadow remains")
                .metrics
                .ambiguous_rejected,
            1
        );
        let metrics = relations.edge_gate_shadow.expect("shadow remains").metrics;
        assert_eq!(
            metrics.pairs_rejected,
            metrics.same_known_rejected
                + metrics.ambiguous_rejected
                + metrics.cross_span_rejected
                + metrics.unclassified_rejected
        );
    }

    #[test]
    fn sentence_edge_gate_shadow_failures_do_not_change_production_state() {
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut relations = empty_modified_sentence_relations(&candidates, &candidates)
            .expect("relations allocate");
        let production_before = (relations.old.clone(), relations.new.clone());
        install_sentence_edge_gate_shadow(&mut relations, None);
        assert_eq!(relations.old, production_before.0);
        assert_eq!(relations.new, production_before.1);
        let shadow = relations.edge_gate_shadow.as_ref().expect("shadow exists");
        assert!(!shadow.active);
        assert_eq!(
            shadow.metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::AllocationFailure)
        );

        enable_sentence_edge_gate_shadow(&mut relations);
        relations
            .edge_gate_shadow
            .as_mut()
            .expect("shadow exists")
            .metrics
            .pairs_considered = usize::MAX;
        let old = indexed_occurrence(
            &['a', 'x'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let new = indexed_occurrence(
            &['a', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let mut budget = RecoveryBudget::new(2, 2, 4, 1).expect("budget is valid");
        let score = score_and_record_sentence_edge_gate_shadow(
            &old,
            &new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            None,
            relations.edge_gate_shadow.as_mut(),
            Some(0),
            Some(0),
            None,
        );
        assert_eq!(score, Some(5_000));
        assert_eq!(relations.old, production_before.0);
        assert_eq!(relations.new, production_before.1);
        let shadow = relations.edge_gate_shadow.expect("shadow remains");
        assert!(!shadow.active);
        assert_eq!(
            shadow.metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::CounterOverflow)
        );

        let mut active = SentenceEdgeGateShadow {
            old: vec![CandidateNearRelation::default()],
            new: vec![CandidateNearRelation::default()],
            metrics: SentenceEdgeGateShadowMetrics {
                pairs_considered: 7,
                pairs_retained: 4,
                pairs_rejected: 3,
                ..SentenceEdgeGateShadowMetrics::default()
            },
            active: true,
        };
        active.metrics.complete = true;
        let cloned = clone_sentence_edge_gate_shadow(&active, None);
        assert!(!cloned.active);
        assert_eq!(cloned.metrics.pairs_considered, 7);
        assert_eq!(cloned.metrics.pairs_retained, 4);
        assert_eq!(cloned.metrics.pairs_rejected, 3);
        assert!(!cloned.metrics.complete);
        assert_eq!(
            cloned.metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::AllocationFailure)
        );

        let mut source = empty_modified_sentence_relations(&candidates, &candidates)
            .expect("relations allocate");
        enable_sentence_edge_gate_shadow(&mut source);
        source
            .edge_gate_shadow
            .as_mut()
            .expect("shadow exists")
            .metrics
            .pairs_considered = 7;
        source
            .edge_gate_shadow
            .as_mut()
            .expect("shadow exists")
            .metrics
            .complete = true;
        let failed_shadow = clone_sentence_edge_gate_shadow(
            source.edge_gate_shadow.as_ref().expect("shadow exists"),
            None,
        );
        let failed_clone = ModifiedSentenceRelations {
            old: vec![CandidateNearRelation::default()],
            new: vec![CandidateNearRelation::default()],
            complete: false,
            edge_gate_shadow: Some(failed_shadow),
            edge_signature_shadow: None,
        };
        propagate_sentence_edge_gate_clone_failure(&mut source, &failed_clone);
        let source_shadow = source.edge_gate_shadow.expect("shadow remains");
        assert!(!source_shadow.active);
        assert!(source_shadow.old.is_empty());
        assert!(source_shadow.new.is_empty());
        assert_eq!(source_shadow.metrics.pairs_considered, 7);
        assert!(!source_shadow.metrics.complete);
        assert_eq!(
            source_shadow.metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::AllocationFailure)
        );
    }

    #[test]
    fn reference_observer_filters_broad_edges_before_near_relation_work() {
        let mut old = indexed_occurrence(
            &['a', 'b', 'c', 'x', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        old.span_index = Some(0);
        let mut retained = indexed_occurrence(
            &['a', 'b', 'c', 'u', 'v'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        retained.span_index = Some(0);
        let mut rejected = indexed_occurrence(
            &['a', 'q', 'r', 's', 't'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        rejected.span_index = Some(0);
        let old_occurrences = [old];
        let new_occurrences = [retained, rejected];
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let new_candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 0,
            },
        ];
        let mut budget = RecoveryBudget::new(5, 10, 15, 1).expect("budget fits");
        budget.apply_reference_limits(ReferenceBudgetLimits {
            pair_visits: 1,
            comparisons: 60,
            candidate_posting_visits: 60,
        });
        budget.sentence_edge_signature_filter_mode =
            SentenceEdgeSignatureFilterMode::ReferenceObserve;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;
        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::ReferenceObserve,
        );

        let relations = modified_sentence_relations_tracked(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            &mut checkpoint,
            None,
        )
        .expect("legacy relation traversal completes");
        assert!(
            !diagnostics
                .as_ref()
                .expect("observer diagnostics remain")
                .signature_retained_fingerprint_valid
        );
        assert!(
            !checkpoint
                .as_ref()
                .expect("observer checkpoint exists")
                .signature_retained_fingerprint_valid
        );
        if let Some(diagnostics) = diagnostics.as_mut() {
            diagnostics.metrics.near_relation_complete = relations.complete;
        }
        record_near_search_metrics(&mut diagnostics, &budget);
        finalize_reference_observer(
            &mut diagnostics,
            relations.edge_signature_shadow.as_ref(),
            true,
        );

        let diagnostics = diagnostics.expect("observer diagnostics remain");
        assert!(diagnostics.signature_retained_fingerprint_valid);
        assert_eq!(diagnostics.signature_retained_fingerprint.count, 1);
        let mut expected = SentenceEdgeRetainedFingerprint::default();
        expected
            .record(
                NearSearchScope::SameOrAmbiguousSpan,
                OccurrenceSide::Old,
                0,
                0,
            )
            .expect("expected fingerprint records");
        assert!(
            diagnostics
                .signature_retained_fingerprint
                .same_order(expected)
        );
        let direct = reference_test_outcome(SentenceRecoveryPlan::default(), expected);
        let reference = SentenceRecoveryBuildOutcome {
            plan: Some(SentenceRecoveryPlan::default()),
            diagnostics: Some(diagnostics),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        let oracle = evaluate_reference_oracle(&direct, &reference);
        assert!(oracle.complete);
        assert_eq!(oracle.legacy_sentence_edge_pairs_attempted, 2);
        assert_eq!(oracle.legacy_sentence_edge_pairs_examined, 2);
        assert_eq!(oracle.legacy_sentence_edge_pairs_retained, 1);
        assert_eq!(oracle.legacy_sentence_edge_pairs_rejected, 1);
        assert_eq!(oracle.pair_visits_examined, 1);
        assert_eq!(
            oracle.legacy_sentence_edge_pairs_examined,
            oracle.legacy_sentence_edge_pairs_retained + oracle.legacy_sentence_edge_pairs_rejected
        );
        assert!(oracle.plan_parity_evaluable);
        assert!(oracle.plan_parity);
        assert!(oracle.fingerprint_evaluable);
        assert_eq!(oracle.retained_pair_count_mismatches, 0);
        assert_eq!(oracle.retained_pair_set_mismatches, 0);
        assert_eq!(oracle.retained_pair_order_mismatches, 0);
    }

    #[test]
    fn reference_observer_excludes_line_pairs_from_sentence_edge_counts() {
        let line = indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let mut shadow = SentenceEdgeSignatureShadow::new(
            10,
            SentenceEdgeSignatureShadowMetrics::default(),
            SentenceEdgeSignatureFilterMode::ReferenceObserve,
        )
        .expect("observer budget fits");
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;

        let budget = RecoveryBudget::new(3, 3, 6, 1).expect("budget fits");
        record_signature_exact_retained(
            Some(&mut shadow),
            &mut diagnostics,
            &mut checkpoint,
            &budget,
            &SentenceEdgeFilterQuery::Legacy,
            &[],
            &line,
            &[],
            OccurrenceSide::Old,
            0,
            NearSearchScope::SameOrAmbiguousSpan,
        )
        .expect("Line observation remains outside the Sentence edge filter");

        assert_eq!(
            shadow
                .retained_fingerprint
                .legacy_sentence_edge_pairs_attempted,
            0
        );
        assert_eq!(
            shadow
                .retained_fingerprint
                .legacy_sentence_edge_pairs_examined,
            0
        );
    }

    #[test]
    fn reference_observer_invalidates_partial_edge_filter_progress() {
        let mut old = indexed_occurrence(
            &['a', 'b', 'c', 'd'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        old.span_index = Some(0);
        let mut new = indexed_occurrence(
            &['a', 'b', 'x', 'y'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        new.span_index = Some(0);
        let old_occurrences = [old];
        let new_occurrences = [new];
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let new_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget fits");
        budget.sentence_edge_filter_comparison_limit = 1;
        budget.sentence_edge_signature_filter_mode =
            SentenceEdgeSignatureFilterMode::ReferenceObserve;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;
        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::ReferenceObserve,
        );
        let relations = modified_sentence_relations_tracked(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            &mut checkpoint,
            None,
        );
        assert!(relations.is_none());

        let checkpoint = checkpoint.expect("failure checkpoint exists");
        assert_eq!(
            checkpoint
                .metrics
                .sentence_edge_signature_shadow
                .expect("observer metrics exist")
                .stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit)
        );
        assert_eq!(
            checkpoint
                .signature_retained_fingerprint
                .legacy_sentence_edge_pairs_attempted,
            1
        );
        assert_eq!(
            checkpoint
                .signature_retained_fingerprint
                .legacy_sentence_edge_pairs_examined,
            0
        );
        assert!(!checkpoint.signature_retained_fingerprint_valid);
        assert_eq!(checkpoint.metrics.near_pair_visits_attempted, 0);
        assert_eq!(checkpoint.metrics.sentence_edge_filter_pairs_examined, 1);
        assert_eq!(checkpoint.metrics.sentence_edge_filter_pairs_attempted, 1);
        assert_eq!(
            checkpoint
                .metrics
                .sentence_edge_filter_similarity_comparisons_examined,
            1
        );
        assert_eq!(
            checkpoint
                .metrics
                .sentence_edge_filter_similarity_comparisons_attempted,
            2
        );
        assert_eq!(
            checkpoint.metrics.sentence_edge_filter_stop_reason,
            Some(SentenceEdgeFilterStopReason::SimilarityComparisonLimit)
        );
        let direct = reference_test_outcome(
            SentenceRecoveryPlan::default(),
            SentenceEdgeRetainedFingerprint::default(),
        );
        let reference = SentenceRecoveryBuildOutcome {
            diagnostics: Some(checkpoint),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        let oracle = evaluate_reference_oracle(&direct, &reference);
        assert!(!oracle.complete);
        assert_eq!(
            oracle.stop_reason,
            Some(
                SentenceEdgeSignatureReferenceOracleStopReason::EdgeFilterSimilarityComparisonLimit
            )
        );
        assert_eq!(oracle.legacy_sentence_edge_pairs_attempted, 1);
        assert_eq!(oracle.legacy_sentence_edge_pairs_examined, 0);
        assert_eq!(oracle.edge_filter_pairs_examined, 1);
        assert_eq!(oracle.edge_filter_pairs_attempted, 1);
        assert_eq!(oracle.edge_filter_similarity_comparisons_examined, 1);
        assert_eq!(oracle.edge_filter_similarity_comparisons_attempted, 2);
        assert_eq!(oracle.pair_visits_attempted, 0);
        assert!(!oracle.plan_parity_evaluable);
        assert!(!oracle.fingerprint_evaluable);
    }

    #[test]
    fn paired_exact_extension_reports_typed_failures() {
        let old = [
            positioned_occurrence("anchor", 1, 0, 0),
            positioned_occurrence("repeated", 2, 0, 1),
            positioned_occurrence("repeated", 3, 0, 2),
        ];
        let new = [
            positioned_occurrence("anchor", 101, 10, 0),
            positioned_occurrence("repeated", 102, 10, 1),
            positioned_occurrence("repeated", 103, 10, 2),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let mut candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 3)
            .expect("global exact matches fit");
        let pairs = paired_trusted_streams(&old, &new, &candidates).expect("streams pair");
        assert_eq!(
            extend_paired_stream_exact_matches(&old, &new, &mut candidates, &pairs, &[true], 5, 2,),
            Err(SentenceEdgeGateShadowStopReason::CandidateCountLimit)
        );

        let mut overflow_candidates = candidates;
        assert_eq!(
            extend_paired_stream_exact_matches(
                &old,
                &new,
                &mut overflow_candidates,
                &pairs,
                &[true],
                5,
                0,
            ),
            Err(SentenceEdgeGateShadowStopReason::CounterOverflow)
        );

        let mut diagnostic_candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 3)
            .expect("global exact matches fit");
        assert_eq!(
            extend_paired_stream_exact_matches(
                &old,
                &new,
                &mut diagnostic_candidates,
                &pairs,
                &[],
                5,
                2,
            ),
            Err(SentenceEdgeGateShadowStopReason::DiagnosticFailure)
        );

        let mut allocation_candidates = diagnostic_candidates;
        assert_eq!(
            extend_paired_stream_exact_matches(
                &old,
                &new,
                &mut allocation_candidates,
                &pairs,
                &[true],
                5,
                usize::MAX,
            ),
            Err(SentenceEdgeGateShadowStopReason::AllocationFailure)
        );

        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        mark_sentence_edge_gate_shadow_incomplete_with_reason(
            &mut diagnostics,
            true,
            SentenceEdgeGateShadowStopReason::CandidateCountLimit,
        );
        let metrics = diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_gate_shadow
            .expect("shadow metrics are retained");
        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::CandidateCountLimit)
        );
    }

    #[test]
    fn paired_near_candidate_limit_reaches_exact_only_fallback_reason() {
        let mut injected_reason = None;
        assert!(
            preserve_paired_stage_failure(
                Err(SentenceEdgeGateShadowStopReason::AllocationFailure),
                &mut injected_reason,
            )
            .is_none()
        );
        assert_eq!(
            injected_reason,
            Some(SentenceEdgeGateShadowStopReason::AllocationFailure)
        );

        let mut old = [
            positioned_occurrence("anchor", 1, 0, 0),
            positioned_occurrence("old-a", 2, 0, 1),
            positioned_occurrence("old-b", 3, 0, 2),
        ];
        let mut new = [
            positioned_occurrence("anchor", 101, 10, 0),
            positioned_occurrence("new-a", 102, 10, 1),
            positioned_occurrence("new-b", 103, 10, 2),
        ];
        let exact = [ExactMatchCandidate {
            old_span_index: 0,
            new_span_index: 0,
            old_occurrence_index: 0,
            new_occurrence_index: 0,
        }];
        let pairs = paired_trusted_streams(&old, &new, &exact).expect("streams pair");
        let mut budget = RecoveryBudget::new(15, 15, 30, 5).expect("budget is valid");
        budget.output_range_limit = 1;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut plan = SentenceRecoveryPlan::default();
        let mut vetoes = PairedNearVetoes::default();
        let mut reason = None;

        assert!(
            append_paired_stream_replacements(
                &mut plan,
                &mut old,
                &mut new,
                &pairs,
                &[true],
                5,
                &mut budget,
                &mut diagnostics,
                &mut None,
                &mut vetoes,
                None,
                &mut reason,
            )
            .is_none()
        );
        assert_eq!(
            reason,
            Some(SentenceEdgeGateShadowStopReason::CandidateCountLimit)
        );
        assert!(!plan.has_recovery(0));
    }

    #[test]
    fn sentence_edge_gate_shadow_replay_failures_are_isolated() {
        let candidates = paired_test_candidates(
            0,
            PairedInterval {
                pair_index: 0,
                interval_index: 0,
            },
        );
        let mut relations =
            empty_modified_sentence_relations(&candidates.recoveries, &candidates.recoveries)
                .expect("relations allocate");
        enable_sentence_edge_gate_shadow(&mut relations);
        let shadow = relations.edge_gate_shadow.as_mut().expect("shadow exists");
        shadow.old[0].record_eligible(0, MIN_NEAR_SCORE);
        shadow.new[0].record_eligible(0, MIN_NEAR_SCORE);
        let malformed = PairedNearCandidates {
            recoveries: vec![RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }],
            intervals: Vec::new(),
            ordinals: Vec::new(),
        };
        let production_before = (relations.old.clone(), relations.new.clone());
        assert!(
            reject_crossing_paired_replacements(&malformed, &malformed, &mut relations).is_ok()
        );
        assert_eq!(relations.old, production_before.0);
        assert_eq!(relations.new, production_before.1);
        let shadow = relations.edge_gate_shadow.as_ref().expect("shadow remains");
        assert!(!shadow.active);
        assert_eq!(
            shadow.metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::DiagnosticFailure)
        );

        enable_sentence_edge_gate_shadow(&mut relations);
        let shadow = relations.edge_gate_shadow.as_mut().expect("shadow exists");
        shadow.old[0].record_eligible(0, MIN_NEAR_SCORE);
        shadow.new[0].record_eligible(0, MIN_NEAR_SCORE);
        let recovery_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut fragment_budget = FragmentVetoBudget::new(1, 1).expect("budget is valid");
        veto_fragment_completed_replacements(
            &[],
            &[],
            &recovery_candidates,
            &recovery_candidates,
            &[],
            &[],
            &mut relations,
            &mut fragment_budget,
        );
        assert_eq!(relations.old, production_before.0);
        assert_eq!(relations.new, production_before.1);
        let shadow = relations.edge_gate_shadow.expect("shadow remains");
        assert!(!shadow.active);
        assert_eq!(
            shadow.metrics.stop_reason,
            Some(SentenceEdgeGateShadowStopReason::DiagnosticFailure)
        );
    }

    fn paired_test_candidates(
        occurrence_index: usize,
        interval: PairedInterval,
    ) -> PairedNearCandidates {
        PairedNearCandidates {
            recoveries: vec![RecoveryCandidate {
                occurrence_index,
                span_index: 0,
            }],
            intervals: vec![interval],
            ordinals: vec![0],
        }
    }

    #[test]
    fn production_sentence_edge_filter_covers_paired_and_cross_interval_paths() {
        let old = [
            positioned_occurrence("old-candidate", 1, 0, 0),
            positioned_occurrence("old-same-interval", 2, 0, 0),
            indexed_positioned_occurrence(&['a', 'q', 'q', 'q', 'q'], 3, 0, 2),
        ];
        let new = [
            positioned_occurrence("new-candidate", 4, 1, 0),
            indexed_positioned_occurrence(&['a', 'r', 'r', 'r', 'r'], 5, 1, 2),
        ];
        let interval = PairedInterval {
            pair_index: 0,
            interval_index: 0,
        };
        let old_candidates = paired_test_candidates(0, interval);
        let new_candidates = paired_test_candidates(0, interval);
        let pairs = [PairedTrustedStream {
            old_stream: 0,
            new_stream: 1,
            anchors: vec![(1, 1)],
        }];
        let old_pair_by_stream = HashMap::from([(0, 0)]);
        let new_pair_by_stream = HashMap::from([(1, 0)]);
        let mut budget = RecoveryBudget::new(15, 10, 25, 1).expect("budget is valid");
        let mut diagnostics = None;

        let relations = paired_modified_sentence_relations(
            &old,
            &new,
            &old_candidates,
            &new_candidates,
            &pairs,
            &old_pair_by_stream,
            &new_pair_by_stream,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("paired relations compute");

        assert!(relations.complete);
        assert_eq!(
            budget
                .paired_interval_work
                .sentence_work
                .pair_visits_examined,
            2
        );
        assert_eq!(
            budget
                .paired_cross_interval_veto_work
                .sentence_work
                .pair_visits_examined,
            0
        );
        assert_eq!(budget.sentence_edge_filter_pairs_retained, 2);
        assert_eq!(budget.sentence_edge_filter_pairs_rejected, 2);
    }

    fn indexed_positioned_occurrence(
        tokens: &[char],
        block: u64,
        stream_index: usize,
        ordinal: usize,
    ) -> SentenceOccurrence {
        let mut occurrence = positioned_occurrence("edge", block, stream_index, ordinal);
        occurrence.tokens = tokens
            .iter()
            .copied()
            .map(SentenceEvidenceToken::Scalar)
            .collect();
        occurrence
    }

    fn structural_descriptor(
        id: u64,
        page: u32,
        block_indices: Vec<usize>,
        trusted_block_indices: Vec<usize>,
        role: Option<BlockRole>,
        source_region_ids: Vec<u64>,
    ) -> TrustedRunDescriptor {
        TrustedRunDescriptor {
            id: TrustedRunId(id),
            page: PageId(page),
            bbox: Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 { x: 1.0, y: 1.0 },
            },
            block_indices,
            trusted_block_indices,
            role,
            source_region_ids: source_region_ids.into_iter().map(RegionId).collect(),
        }
    }

    fn structural_occurrence(descriptor_index: usize, ordinal: usize) -> SentenceOccurrence {
        let mut occurrence = positioned_occurrence("anchor", ordinal as u64, 0, ordinal);
        occurrence.run_descriptor_index = Some(descriptor_index);
        occurrence
    }

    fn run_occurrence(
        key: &str,
        tokens: &[char],
        descriptor_index: usize,
        ordinal: usize,
    ) -> SentenceOccurrence {
        let mut occurrence = positioned_occurrence(key, ordinal as u64, descriptor_index, ordinal);
        occurrence.tokens = tokens
            .iter()
            .copied()
            .map(SentenceEvidenceToken::Scalar)
            .collect();
        occurrence.run_descriptor_index = Some(descriptor_index);
        occurrence
    }

    fn run_structural_plan(old_runs: usize, new_runs: usize) -> StructuralPairingPlan {
        StructuralPairingPlan {
            old: StructuralSideProfiles {
                descriptor_count: old_runs,
                eligible_count: old_runs,
                eligible: vec![true; old_runs],
                ..StructuralSideProfiles::default()
            },
            new: StructuralSideProfiles {
                descriptor_count: new_runs,
                eligible_count: new_runs,
                eligible: vec![true; new_runs],
                ..StructuralSideProfiles::default()
            },
            ..StructuralPairingPlan::default()
        }
    }

    fn run_limits(
        min_tokens: usize,
        old_tokens: usize,
        new_tokens: usize,
        candidate_pairs: usize,
    ) -> RunSignatureLimits {
        RunSignatureLimits {
            min_tokens,
            old_tokens,
            new_tokens,
            candidate_pairs,
        }
    }

    fn run_eligibility(plan: &StructuralPairingPlan) -> RunSignatureEligibility<'_> {
        RunSignatureEligibility {
            old: &plan.old.eligible,
            new: &plan.new.eligible,
        }
    }

    fn similarity_occurrence(
        key: &str,
        tokens: Vec<SentenceEvidenceToken>,
        kind: RecoveryUnitKind,
    ) -> SentenceOccurrence {
        let mut word_ranges = key
            .unicode_word_indices()
            .map(|(start, word)| start..start + word.len())
            .collect::<Vec<_>>();
        word_ranges.sort_unstable_by(|left, right| {
            key[left.clone()]
                .cmp(&key[right.clone()])
                .then_with(|| left.start.cmp(&right.start))
        });
        SentenceOccurrence {
            key: key.to_owned(),
            tokens,
            word_ranges,
            kind,
            role: Some(BlockRole::Body),
            location: None,
            span_index: Some(0),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }
    }

    fn word_multiset_reference(old: &[u8], new: &[u8]) -> u16 {
        let mut old_index = 0usize;
        let mut new_index = 0usize;
        let mut shared = 0usize;
        while old_index < old.len() && new_index < new.len() {
            match old[old_index].cmp(&new[new_index]) {
                std::cmp::Ordering::Less => old_index += 1,
                std::cmp::Ordering::Greater => new_index += 1,
                std::cmp::Ordering::Equal => {
                    shared += 1;
                    old_index += 1;
                    new_index += 1;
                }
            }
        }
        basis_points(shared * 2, old.len() + new.len()).expect("small multiset score fits")
    }

    fn occurrence_from_word_ids(words: &[u8], kind: RecoveryUnitKind) -> SentenceOccurrence {
        let key = words
            .iter()
            .map(|word| char::from(b'a' + *word).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let tokens = key.chars().map(SentenceEvidenceToken::Scalar).collect();
        similarity_occurrence(&key, tokens, kind)
    }

    fn extend_test_paired_exact_matches(
        old: &[SentenceOccurrence],
        new: &[SentenceOccurrence],
        candidates: &mut Vec<ExactMatchCandidate>,
        max_candidates: usize,
    ) -> Option<()> {
        let pairs = paired_trusted_streams(old, new, candidates)?;
        extend_paired_stream_exact_matches(old, new, candidates, &pairs, &[true], 5, max_candidates)
            .ok()
    }

    #[test]
    fn stream_plans_use_run_ordinals_across_column_major_block_order() {
        let metadata = [interval(1, 1, 2), interval(2, 0, 1), interval(1, 0, 1)];
        let plans = stream_plans(&metadata).expect("valid intervals produce plans");
        assert_eq!(
            plan_blocks(&plans),
            vec![(vec![2, 0], true), (vec![1], true)]
        );

        let reversed = [interval(1, 0, 1), interval(2, 0, 1), interval(1, 1, 2)];
        let plans = stream_plans(&reversed).expect("reversed valid intervals produce plans");
        assert_eq!(
            plan_blocks(&plans),
            vec![(vec![0, 2], true), (vec![1], true)]
        );
    }

    #[test]
    fn stream_plans_split_gaps_and_untrusted_original_order_barriers() {
        let gap = [interval(1, 0, 1), interval(1, 2, 3)];
        let plans = stream_plans(&gap).expect("a gap splits instead of invalidating");
        assert_eq!(plan_blocks(&plans), vec![(vec![0], true), (vec![1], true)]);

        let barrier = [interval(1, 0, 1), None, interval(1, 1, 2)];
        let plans = stream_plans(&barrier).expect("an untrusted block splits the run");
        assert_eq!(
            plan_blocks(&plans),
            vec![(vec![0], true), (vec![2], true), (vec![1], false)]
        );
        assert_eq!(plans[0].run_id, Some(TrustedRunId(1)));
        assert_eq!(plans[1].run_id, Some(TrustedRunId(1)));
        assert_eq!(plans[2].run_id, None);
    }

    #[test]
    fn mixed_untrusted_occurrence_retains_all_descriptor_evidence() {
        let descriptors = [
            TrustedRunDescriptor {
                id: TrustedRunId(10),
                page: PageId(2),
                bbox: Rect {
                    min: Vec2 { x: 10.0, y: 20.0 },
                    max: Vec2 { x: 30.0, y: 40.0 },
                },
                block_indices: vec![7],
                trusted_block_indices: Vec::new(),
                role: Some(BlockRole::Body),
                source_region_ids: vec![RegionId(3)],
            },
            TrustedRunDescriptor {
                id: TrustedRunId(11),
                page: PageId(2),
                bbox: Rect {
                    min: Vec2 { x: 40.0, y: 20.0 },
                    max: Vec2 { x: 60.0, y: 40.0 },
                },
                block_indices: vec![7],
                trusted_block_indices: Vec::new(),
                role: Some(BlockRole::Body),
                source_region_ids: vec![RegionId(4)],
            },
        ];
        let raw_edges = [TrustedRegionEdge {
            page: PageId(2),
            source: RegionId(3),
            target: RegionId(4),
            relation: RegionRelation::LeftOf,
        }];
        let evidence = RunRecoveryEvidence::new(TrustedRunRecoveryInput {
            descriptors: &descriptors,
            raw_region_edges: &raw_edges,
        })
        .expect("bounded descriptor evidence should index");
        let mut occurrence =
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body));
        occurrence.evidence_block_index = Some(7);

        let indices = evidence
            .descriptor_indices_for_untrusted_occurrence(&occurrence)
            .expect("mixed block should retain both run descriptors");

        assert_eq!(indices, [0, 1]);
        assert_eq!(evidence.descriptors[indices[0]].id, TrustedRunId(10));
        assert_eq!(evidence.descriptors[indices[0]].page, PageId(2));
        assert_eq!(evidence.descriptors[indices[0]].bbox, descriptors[0].bbox);
        assert_eq!(evidence.descriptors[indices[0]].role, Some(BlockRole::Body));
        assert!(
            evidence.descriptors[indices[0]]
                .trusted_block_indices
                .is_empty()
        );
        assert_eq!(
            evidence.descriptors[indices[0]].source_region_ids,
            [RegionId(3)]
        );
        assert_eq!(evidence.raw_region_edges(), raw_edges);
        occurrence.run_descriptor_index = Some(0);
        assert_eq!(
            evidence
                .descriptor_for_occurrence(&occurrence)
                .expect("trusted occurrence should resolve its descriptor")
                .id,
            TrustedRunId(10)
        );
    }

    #[test]
    fn structural_profiles_partition_eligible_mixed_and_split_descriptors() {
        let descriptors = [
            structural_descriptor(1, 0, vec![0], vec![0], Some(BlockRole::Body), vec![1]),
            structural_descriptor(2, 0, vec![1], vec![1], None, vec![2]),
            structural_descriptor(3, 0, vec![2, 3], vec![2], Some(BlockRole::Body), vec![3]),
            structural_descriptor(4, 0, vec![4, 6], vec![4, 6], Some(BlockRole::Body), vec![4]),
        ];
        let intervals = [
            interval(1, 0, 1),
            interval(2, 0, 1),
            interval(3, 0, 1),
            interval(3, 1, 2),
            interval(4, 0, 1),
            None,
            interval(4, 1, 2),
        ];
        let evidence = RunRecoveryEvidence::new(TrustedRunRecoveryInput {
            descriptors: &descriptors,
            raw_region_edges: &[],
        })
        .expect("descriptor evidence should index");

        let profiles = structural_side_profiles(&evidence, &intervals)
            .expect("bounded structural profiles should build");

        assert_eq!(profiles.descriptor_count, 4);
        assert_eq!(profiles.eligible_count, 1);
        assert_eq!(profiles.mixed_count, 1);
        assert_eq!(profiles.split_count, 2);
        assert_eq!(
            profiles.descriptor_count,
            profiles.eligible_count + profiles.mixed_count + profiles.split_count
        );
    }

    #[test]
    fn structural_profiles_match_across_page_and_region_identifiers() {
        let old_descriptors = [structural_descriptor(
            1,
            2,
            vec![0],
            vec![0],
            Some(BlockRole::Body),
            vec![10],
        )];
        let new_descriptors = [structural_descriptor(
            9,
            8,
            vec![0],
            vec![0],
            Some(BlockRole::Body),
            vec![90],
        )];
        let old_input = TrustedRunRecoveryInput {
            descriptors: &old_descriptors,
            raw_region_edges: &[],
        };
        let new_input = TrustedRunRecoveryInput {
            descriptors: &new_descriptors,
            raw_region_edges: &[],
        };
        let old_intervals = [interval(1, 0, 1)];
        let new_intervals = [interval(9, 0, 1)];
        let evidence = RecoveryStructuralEvidence::new(SentenceRecoveryInput {
            old_trusted_run_intervals: &old_intervals,
            new_trusted_run_intervals: &new_intervals,
            old_trusted_run_evidence: Some(old_input),
            new_trusted_run_evidence: Some(new_input),
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        })
        .expect("descriptor evidence should index");

        let plan = structural_pairing_plan(&evidence, &old_intervals, &new_intervals)
            .expect("structural profiles should pair");

        assert_eq!(plan.shared_profiles, 1);
        assert_eq!(plan.candidate_pairs, 1);
        assert_eq!(plan.duplicate_pairs, 0);
        assert_eq!(plan.unique_pairs, [(0, 0)]);
    }

    #[test]
    fn duplicate_structural_profiles_are_not_unique_pairs() {
        let old_descriptors = [
            structural_descriptor(1, 0, vec![0], vec![0], Some(BlockRole::Body), vec![1]),
            structural_descriptor(2, 0, vec![1], vec![1], Some(BlockRole::Body), vec![2]),
        ];
        let new_descriptors = [
            structural_descriptor(3, 1, vec![0], vec![0], Some(BlockRole::Body), vec![3]),
            structural_descriptor(4, 1, vec![1], vec![1], Some(BlockRole::Body), vec![4]),
        ];
        let old_evidence = RunRecoveryEvidence::new(TrustedRunRecoveryInput {
            descriptors: &old_descriptors,
            raw_region_edges: &[],
        })
        .expect("old descriptors should index");
        let new_evidence = RunRecoveryEvidence::new(TrustedRunRecoveryInput {
            descriptors: &new_descriptors,
            raw_region_edges: &[],
        })
        .expect("new descriptors should index");
        let evidence = RecoveryStructuralEvidence {
            old: Some(old_evidence),
            new: Some(new_evidence),
        };

        let plan = structural_pairing_plan(
            &evidence,
            &[interval(1, 0, 1), interval(2, 0, 1)],
            &[interval(3, 0, 1), interval(4, 0, 1)],
        )
        .expect("duplicate profiles should remain diagnostic candidates");

        assert_eq!(plan.shared_profiles, 1);
        assert_eq!(plan.candidate_pairs, 4);
        assert_eq!(plan.largest_posting, 2);
        assert_eq!(plan.duplicate_pairs, 4);
        assert!(plan.unique_pairs.is_empty());
        assert_eq!(
            plan.candidate_pairs,
            plan.duplicate_pairs + plan.unique_pairs.len()
        );
    }

    #[test]
    fn structural_edge_histogram_counts_internal_edges_once_per_direction() {
        let descriptors = [structural_descriptor(
            1,
            0,
            vec![0],
            vec![0],
            Some(BlockRole::Body),
            vec![1, 2],
        )];
        let edges = [TrustedRegionEdge {
            page: PageId(0),
            source: RegionId(1),
            target: RegionId(2),
            relation: RegionRelation::LeftOf,
        }];
        let evidence = RunRecoveryEvidence::new(TrustedRunRecoveryInput {
            descriptors: &descriptors,
            raw_region_edges: &edges,
        })
        .expect("descriptor evidence should index");

        let profiles = structural_side_profiles(&evidence, &[interval(1, 0, 1)])
            .expect("histogram should build");
        let profile = profiles.postings.keys().next().expect("one profile");

        assert_eq!(profile.edge_histogram.iter().sum::<usize>(), 2);
        assert_eq!(profile.edge_histogram[2 * 4 + 1], 1);
        assert_eq!(profile.edge_histogram[2 * 4 + 2 + 1], 1);
    }

    #[test]
    fn structural_unique_pairs_classify_exact_anchor_order() {
        let plan = StructuralPairingPlan {
            unique_pairs: vec![(0, 0), (1, 1), (2, 2)],
            ..StructuralPairingPlan::default()
        };
        let old = vec![
            structural_occurrence(1, 1),
            structural_occurrence(1, 3),
            structural_occurrence(2, 1),
            structural_occurrence(2, 3),
        ];
        let new = vec![
            structural_occurrence(1, 2),
            structural_occurrence(1, 4),
            structural_occurrence(2, 4),
            structural_occurrence(2, 2),
        ];
        let exact = vec![
            ExactMatchCandidate {
                old_span_index: 0,
                new_span_index: 0,
                old_occurrence_index: 0,
                new_occurrence_index: 0,
            },
            ExactMatchCandidate {
                old_span_index: 0,
                new_span_index: 0,
                old_occurrence_index: 1,
                new_occurrence_index: 1,
            },
            ExactMatchCandidate {
                old_span_index: 0,
                new_span_index: 0,
                old_occurrence_index: 2,
                new_occurrence_index: 2,
            },
            ExactMatchCandidate {
                old_span_index: 0,
                new_span_index: 0,
                old_occurrence_index: 3,
                new_occurrence_index: 3,
            },
        ];

        assert_eq!(
            structural_anchor_classes(&plan, &old, &new, &exact),
            Some((1, 1, 1))
        );
    }

    #[test]
    fn structural_anchor_classification_ignores_unrelated_exact_candidates() {
        let plan = StructuralPairingPlan::default();
        let old = vec![structural_occurrence(10, 1)];
        let new = vec![structural_occurrence(20, 1)];
        let exact = [ExactMatchCandidate {
            old_span_index: 0,
            new_span_index: 0,
            old_occurrence_index: 0,
            new_occurrence_index: 0,
        }];

        assert_eq!(
            structural_anchor_classes(&plan, &old, &new, &exact),
            Some((0, 0, 0))
        );
    }

    #[test]
    fn run_unit_index_deduplicates_runs_and_excludes_local_duplicates() {
        let distinct = vec![
            run_occurrence("shared", &['a'], 0, 0),
            run_occurrence("shared", &['a'], 1, 0),
        ];
        let index = run_unit_index(&distinct, &[true, true], &HashSet::new())
            .expect("distinct run postings should index");
        assert_eq!(index.unique_units, 2);
        assert_eq!(index.duplicate_units, 0);
        assert_eq!(index.postings.values().next().map(Vec::len), Some(2));

        let duplicate = vec![
            run_occurrence("shared", &['a'], 0, 0),
            run_occurrence("shared", &['a'], 0, 1),
            run_occurrence("shared", &['b'], 1, 0),
            run_occurrence("shared", &['c'], 1, 1),
        ];
        let index = run_unit_index(&duplicate, &[true, true], &HashSet::new())
            .expect("ambiguous run-local keys should fail closed");
        assert_eq!(index.unique_units, 0);
        assert_eq!(index.duplicate_units, 2);
        assert!(index.postings.is_empty());
    }

    #[test]
    fn run_signature_requires_full_token_equality_after_key_lookup() {
        let structural = run_structural_plan(1, 1);
        let old = vec![run_occurrence("same-key", &['a', 'b'], 0, 0)];
        let new = vec![run_occurrence("same-key", &['a', 'c'], 0, 0)];

        let metrics = run_signature_metrics(
            run_eligibility(&structural),
            &old,
            &new,
            &[],
            run_limits(1, 10, 10, 10),
        )
        .expect("bounded signature diagnostics should complete");

        assert!(metrics.run_signature_complete);
        assert_eq!(metrics.run_signature_shared_unit_keys, 1);
        assert_eq!(metrics.run_signature_token_verifications_examined, 2);
        assert_eq!(metrics.run_signature_candidate_pairs, 0);
    }

    #[test]
    fn run_signature_never_uses_unmapped_tokens_as_positive_evidence() {
        let structural = run_structural_plan(1, 1);
        let mut old = run_occurrence("collision-key", &['a'], 0, 0);
        old.tokens = vec![SentenceEvidenceToken::Unmapped {
            font_fingerprint: 7,
            glyph_id: 11,
        }];
        let new = vec![run_occurrence("collision-key", &['a'], 0, 0)];

        let metrics = run_signature_metrics(
            run_eligibility(&structural),
            &[old],
            &new,
            &[],
            run_limits(1, 10, 10, 10),
        )
        .expect("unmapped evidence should be excluded without failing diagnostics");

        assert_eq!(metrics.old_run_signature_unique_units, 0);
        assert_eq!(metrics.new_run_signature_unique_units, 1);
        assert_eq!(metrics.run_signature_shared_unit_keys, 0);
        assert_eq!(metrics.run_signature_posting_visits_attempted, 0);
        assert_eq!(metrics.run_signature_candidate_pairs, 0);
    }

    #[test]
    fn run_signature_processes_rare_keys_before_posting_limit() {
        let structural = run_structural_plan(3, 3);
        let old = vec![
            run_occurrence("rare", &['r'], 0, 0),
            run_occurrence("frequent", &['f'], 1, 0),
            run_occurrence("frequent", &['f'], 2, 0),
        ];
        let new = vec![
            run_occurrence("rare", &['r'], 0, 0),
            run_occurrence("frequent", &['f'], 1, 0),
            run_occurrence("frequent", &['f'], 2, 0),
        ];

        let metrics = run_signature_metrics(
            run_eligibility(&structural),
            &old,
            &new,
            &[],
            run_limits(1, 1, 0, 10),
        )
        .expect("posting limit should remain a diagnostic outcome");

        assert!(!metrics.run_signature_complete);
        assert_eq!(
            metrics.run_signature_stop_reason,
            Some(RunSignatureStopReason::PostingVisitLimit)
        );
        assert_eq!(metrics.run_signature_posting_visits_examined, 1);
        assert_eq!(metrics.run_signature_posting_visits_attempted, 5);
        assert_eq!(metrics.run_signature_candidate_pairs, 1);
        assert_eq!(metrics.run_signature_reciprocal_unique_pairs, 0);
    }

    #[test]
    fn run_signature_reports_verification_and_candidate_pair_limits() {
        let structural = run_structural_plan(1, 1);
        let old = vec![run_occurrence("shared", &['a', 'b', 'c', 'd', 'e'], 0, 0)];
        let new = old
            .iter()
            .map(|occurrence| run_occurrence(&occurrence.key, &['a', 'b', 'c', 'd', 'e'], 0, 0))
            .collect::<Vec<_>>();

        let verification = run_signature_metrics(
            run_eligibility(&structural),
            &old,
            &new,
            &[],
            run_limits(1, 1, 0, 10),
        )
        .expect("verification limit should remain diagnostic");
        assert_eq!(
            verification.run_signature_stop_reason,
            Some(RunSignatureStopReason::TokenVerificationLimit)
        );
        assert_eq!(verification.run_signature_token_verifications_examined, 0);
        assert_eq!(verification.run_signature_token_verifications_attempted, 5);
        assert_eq!(verification.run_signature_posting_visits_attempted, 1);
        assert_eq!(verification.run_signature_posting_visits_examined, 1);

        let candidate = run_signature_metrics(
            run_eligibility(&structural),
            &old,
            &new,
            &[],
            run_limits(1, 10, 10, 0),
        )
        .expect("candidate limit should remain diagnostic");
        assert_eq!(
            candidate.run_signature_stop_reason,
            Some(RunSignatureStopReason::CandidatePairLimit)
        );
        assert_eq!(candidate.run_signature_candidate_pairs, 0);

        let two_by_two = run_structural_plan(2, 2);
        let old = vec![
            run_occurrence("shared", &['a'], 0, 0),
            run_occurrence("shared", &['a'], 1, 0),
        ];
        let new = vec![
            run_occurrence("shared", &['a'], 0, 0),
            run_occurrence("shared", &['a'], 1, 0),
        ];
        let candidate = run_signature_metrics(
            run_eligibility(&two_by_two),
            &old,
            &new,
            &[],
            run_limits(1, 10, 10, 1),
        )
        .expect("candidate stop should preserve bounded work counters");
        assert_eq!(
            candidate.run_signature_stop_reason,
            Some(RunSignatureStopReason::CandidatePairLimit)
        );
        assert_eq!(candidate.run_signature_posting_visits_attempted, 4);
        assert_eq!(candidate.run_signature_posting_visits_examined, 2);
        assert_eq!(candidate.run_signature_token_verifications_attempted, 2);
        assert_eq!(candidate.run_signature_token_verifications_examined, 2);
        assert_eq!(candidate.run_signature_candidate_pairs, 1);
    }

    #[test]
    fn run_signature_requires_two_units_and_margin_for_reciprocal_pair() {
        let structural = run_structural_plan(1, 1);
        let old = vec![
            run_occurrence("first", &['a', 'a'], 0, 0),
            run_occurrence("second", &['b', 'b'], 0, 1),
        ];
        let new = vec![
            run_occurrence("first", &['a', 'a'], 0, 0),
            run_occurrence("second", &['b', 'b'], 0, 1),
        ];
        let qualified = run_signature_metrics(
            run_eligibility(&structural),
            &old,
            &new,
            &[],
            run_limits(4, 20, 20, 10),
        )
        .expect("two exact units should score");
        assert_eq!(qualified.run_signature_reciprocal_unique_pairs, 1);
        assert_eq!(qualified.run_signature_margin_qualified_pairs, 1);
        assert_eq!(qualified.run_signature_margin_veto_pairs, 0);
        assert_eq!(qualified.run_signature_monotone_pairs, 1);

        let one_unit = run_signature_metrics(
            run_eligibility(&structural),
            &old[..1],
            &new[..1],
            &[],
            run_limits(1, 10, 10, 10),
        )
        .expect("one exact unit should remain diagnostic");
        assert_eq!(one_unit.run_signature_reciprocal_unique_pairs, 1);
        assert_eq!(one_unit.run_signature_margin_qualified_pairs, 0);
        assert_eq!(one_unit.run_signature_margin_veto_pairs, 1);
    }

    #[test]
    fn run_signature_rejects_tied_best_and_crossing_ordinals() {
        let tied_structural = run_structural_plan(1, 2);
        let old = vec![
            run_occurrence("first", &['a'], 0, 0),
            run_occurrence("second", &['b'], 0, 1),
        ];
        let tied_new = vec![
            run_occurrence("first", &['a'], 0, 0),
            run_occurrence("second", &['b'], 0, 1),
            run_occurrence("first", &['a'], 1, 0),
            run_occurrence("second", &['b'], 1, 1),
        ];
        let tied = run_signature_metrics(
            run_eligibility(&tied_structural),
            &old,
            &tied_new,
            &[],
            run_limits(1, 20, 20, 10),
        )
        .expect("tied scores should complete");
        assert_eq!(tied.run_signature_reciprocal_unique_pairs, 0);

        let crossing_structural = run_structural_plan(1, 1);
        let crossing_new = vec![
            run_occurrence("first", &['a'], 0, 1),
            run_occurrence("second", &['b'], 0, 0),
        ];
        let crossing = run_signature_metrics(
            run_eligibility(&crossing_structural),
            &old,
            &crossing_new,
            &[],
            run_limits(1, 20, 20, 10),
        )
        .expect("crossing evidence should complete");
        assert_eq!(crossing.run_signature_margin_qualified_pairs, 1);
        assert_eq!(crossing.run_signature_monotone_pairs, 0);
        assert_eq!(crossing.run_signature_crossing_veto_pairs, 1);
    }

    #[test]
    fn run_signature_skips_runs_participating_in_global_exact_candidates() {
        let structural = run_structural_plan(1, 1);
        let old = vec![run_occurrence("shared", &['a'], 0, 0)];
        let new = vec![run_occurrence("shared", &['a'], 0, 0)];
        let exact = [ExactMatchCandidate {
            old_span_index: 0,
            new_span_index: 0,
            old_occurrence_index: 0,
            new_occurrence_index: 0,
        }];

        let metrics = run_signature_metrics(
            run_eligibility(&structural),
            &old,
            &new,
            &exact,
            run_limits(1, 10, 10, 10),
        )
        .expect("anchored runs should be skipped without failing diagnostics");

        assert_eq!(metrics.run_signature_globally_anchored_runs_skipped, 2);
        assert_eq!(metrics.old_run_signature_unique_units, 0);
        assert_eq!(metrics.new_run_signature_unique_units, 0);
        assert_eq!(metrics.run_signature_candidate_pairs, 0);
    }

    #[test]
    fn stream_plans_reject_overlap_duplicate_and_empty_intervals() {
        assert!(stream_plans(&[interval(1, 0, 2), interval(1, 1, 3)]).is_none());
        assert!(stream_plans(&[interval(1, 0, 1), interval(1, 0, 1)]).is_none());
        assert!(stream_plans(&[interval(1, 1, 1)]).is_none());
    }

    #[test]
    fn sentence_span_index_rejects_boundary_without_block_overlap() {
        let side = Side {
            blocks: &[],
            index: HashMap::new(),
            canonical: Vec::new(),
            total_tokens: 0,
        };
        let stream = Stream {
            text: String::new(),
            tokens: Vec::new(),
            scalar_to_token: Vec::new(),
            blocks: Vec::new(),
            forced_sentence_boundaries: Vec::new(),
            atomic_line: false,
            trusted: true,
        };
        let boundary = SentenceBoundary {
            byte_start: 0,
            byte_end: 0,
            scalar_start: 0,
            scalar_end: 1,
        };

        assert!(sentence_stream_block_range(&stream, boundary).is_none());
        assert!(sentence_span_index(&side, &stream, 0..0, &HashMap::new()).is_none());
    }

    #[test]
    fn unmapped_evidence_fingerprint_is_deterministic_and_distinguishes_fields() {
        let first = ComparableToken::Unmapped {
            font_hash: FontProgramHash(vec![1, 2, 3, 4]),
            glyph_id: 7,
        };
        let separately_allocated_equal = ComparableToken::Unmapped {
            font_hash: FontProgramHash(vec![1, 2, 3, 4]),
            glyph_id: 7,
        };
        let different_hash = ComparableToken::Unmapped {
            font_hash: FontProgramHash(vec![1, 2, 3, 5]),
            glyph_id: 7,
        };
        let different_glyph = ComparableToken::Unmapped {
            font_hash: FontProgramHash(vec![1, 2, 3, 4]),
            glyph_id: 8,
        };

        let evidence = SentenceEvidenceToken::from(&first);
        assert_eq!(
            evidence,
            SentenceEvidenceToken::from(&separately_allocated_equal)
        );
        assert_ne!(evidence, SentenceEvidenceToken::from(&different_hash));
        assert_ne!(evidence, SentenceEvidenceToken::from(&different_glyph));
    }

    #[test]
    fn evidence_space_separator_matches_comparable_whitespace_rule() {
        let cases = [
            (Vec::new(), vec![ComparableToken::Scalar('a')]),
            (vec![ComparableToken::Scalar('a')], Vec::new()),
            (
                vec![ComparableToken::Scalar('a')],
                vec![ComparableToken::Scalar('b')],
            ),
            (
                vec![ComparableToken::Scalar('\t')],
                vec![ComparableToken::Scalar('b')],
            ),
            (
                vec![ComparableToken::Scalar('a')],
                vec![ComparableToken::Scalar('\u{2003}')],
            ),
        ];

        for (left, next) in cases {
            let mut expected_source = left.clone();
            BlockSeparator::Space.append(&mut expected_source, &next);
            let mut expected = Vec::new();
            append_evidence_tokens(&mut expected, &expected_source, BlockSeparator::Concatenate)
                .expect("reference evidence conversion fits");

            let mut actual = Vec::new();
            append_evidence_tokens(&mut actual, &left, BlockSeparator::Concatenate)
                .expect("left evidence conversion fits");
            append_evidence_tokens(&mut actual, &next, BlockSeparator::Space)
                .expect("space-separated evidence conversion fits");

            assert_eq!(actual, expected, "left={left:?}, next={next:?}");
        }
    }

    #[test]
    fn unmapped_near_occurrence_vetoes_candidate_without_owned_hash() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<SentenceEvidenceToken>();

        let source = [
            ComparableToken::Scalar('a'),
            ComparableToken::Scalar('b'),
            ComparableToken::Scalar('c'),
            ComparableToken::Scalar('d'),
            ComparableToken::Scalar('e'),
            ComparableToken::Scalar('f'),
            ComparableToken::Scalar('g'),
            ComparableToken::Scalar('h'),
            ComparableToken::Unmapped {
                font_hash: FontProgramHash(vec![9, 8, 7, 6]),
                glyph_id: 42,
            },
            ComparableToken::Scalar('j'),
        ];
        let mut counterpart_tokens = Vec::new();
        append_evidence_tokens(
            &mut counterpart_tokens,
            &source,
            BlockSeparator::Concatenate,
        )
        .expect("counterpart evidence conversion fits");
        drop(source);

        let candidate_source = [
            ComparableToken::Scalar('a'),
            ComparableToken::Scalar('b'),
            ComparableToken::Scalar('c'),
            ComparableToken::Scalar('d'),
            ComparableToken::Scalar('e'),
            ComparableToken::Scalar('f'),
            ComparableToken::Scalar('g'),
            ComparableToken::Scalar('h'),
            ComparableToken::Scalar('i'),
            ComparableToken::Scalar('j'),
        ];
        let mut candidate_tokens = Vec::new();
        append_evidence_tokens(
            &mut candidate_tokens,
            &candidate_source,
            BlockSeparator::Concatenate,
        )
        .expect("candidate evidence conversion fits");
        drop(candidate_source);

        let location = LocalSentenceRange {
            block: BlockId(1),
            canonical: ScalarRange { start: 0, end: 10 },
            comparable: TokenRange { start: 0, end: 10 },
        };
        let old_occurrences = [SentenceOccurrence {
            key: "candidate".to_owned(),
            tokens: candidate_tokens,
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: Some(test_location(location, 0)),
            span_index: Some(0),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }];
        let new_occurrences = [SentenceOccurrence {
            key: "counterpart".to_owned(),
            tokens: counterpart_tokens,
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: None,
            span_index: Some(0),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }];
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("test budget is valid");
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &[],
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("near-match veto stays within budget");
        assert!(relations.old[0].vetoed());
        assert!(relations.old[0].unique_partner().is_none());
        assert!(relations.new.is_empty());
    }

    #[test]
    fn replacement_margin_counts_runner_up_below_adoption_threshold() {
        let mut relation = CandidateNearRelation::default();
        relation.record_eligible(3, 7_200);
        relation.record_eligible(4, 6_800);

        assert!(relation.vetoed());
        assert_eq!(relation.unique_partner(), None);
    }

    #[test]
    fn line_similarity_uses_ngrams_and_rejects_extreme_length_ratios() {
        let occurrence = |text: &str| SentenceOccurrence {
            key: text.to_owned(),
            tokens: text.chars().map(SentenceEvidenceToken::Scalar).collect(),
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Line,
            role: Some(BlockRole::Body),
            location: None,
            span_index: Some(0),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        };
        let old = occurrence("arXiv:1706.03762v6 [cs.CL] 24 Jul 2023");
        let new = occurrence("arXiv:1706.03762v7 [cs.CL] 2 Aug 2023");
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("line similarity budget is valid");

        assert_eq!(
            sentence_similarity_in_scope(&old, &new, &mut budget, TEST_NEAR_SCOPE),
            Some(7_323)
        );

        let tiny = occurrence("a");
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            tiny.tokens.len(),
            old.tokens.len() + tiny.tokens.len(),
            1,
        )
        .expect("length guard budget is valid");
        assert_eq!(
            sentence_similarity_in_scope(&old, &tiny, &mut budget, TEST_NEAR_SCOPE),
            Some(0)
        );
    }

    #[test]
    fn sentence_edge_evidence_reports_prefix_suffix_and_score() {
        let old = similarity_occurrence(
            "old",
            "abcdef"
                .chars()
                .map(SentenceEvidenceToken::Scalar)
                .collect(),
            RecoveryUnitKind::Sentence,
        );
        let new = similarity_occurrence(
            "new",
            "abcxef"
                .chars()
                .map(SentenceEvidenceToken::Scalar)
                .collect(),
            RecoveryUnitKind::Sentence,
        );
        let mut budget = RecoveryBudget::new(6, 6, 12, 1).expect("valid evidence budget");

        {
            let evidence = sentence_edge_evidence(
                &old,
                &new,
                &mut budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            )
            .expect("edge evidence fits the budget");

            assert_eq!(evidence.prefix_tokens(), 3);
            assert_eq!(evidence.suffix_tokens(), 2);
            assert_eq!(evidence.shorter_tokens(), 6);
            assert_eq!(evidence.edge_score(), 8_333);
        }
        assert_eq!(budget.comparisons, 7);
        assert_eq!(budget.comparisons_attempted, 7);
    }

    #[test]
    fn sentence_edge_evidence_does_not_rescan_an_equal_suffix() {
        let old = similarity_occurrence(
            "same",
            "same".chars().map(SentenceEvidenceToken::Scalar).collect(),
            RecoveryUnitKind::Sentence,
        );
        let new = similarity_occurrence("same", old.tokens.clone(), RecoveryUnitKind::Sentence);
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("valid equality budget");

        let evidence = sentence_edge_evidence(
            &old,
            &new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
        )
        .expect("equal edge evidence fits the budget");
        assert_eq!(evidence.prefix_tokens(), 4);
        assert_eq!(evidence.suffix_tokens(), 0);

        assert_eq!(
            sentence_similarity_in_scope_attributed_from_edge_evidence(evidence),
            Some(10_000)
        );
        assert_eq!(budget.comparisons, 4);
    }

    #[test]
    fn aligned_edge_facts_preserve_exhaustive_binary_sentence_evidence() {
        let occurrences = (0usize..=7)
            .flat_map(|length| {
                (0usize..(1usize << length)).map(move |bits| {
                    let tokens = (0..length)
                        .map(|index| {
                            SentenceEvidenceToken::Scalar(if bits & (1usize << index) == 0 {
                                'a'
                            } else {
                                'b'
                            })
                        })
                        .collect();
                    similarity_occurrence("binary", tokens, RecoveryUnitKind::Sentence)
                })
            })
            .collect::<Vec<_>>();

        for old in &occurrences {
            for new in &occurrences {
                let mut baseline_comparisons = 0usize;
                let baseline = cached_sentence_edge_evidence(old, new, || {
                    baseline_comparisons += 1;
                    true
                })
                .expect("baseline edge evidence computes");
                let nonempty = !old.tokens.is_empty() && !new.tokens.is_empty();
                let prefix_equal = nonempty && old.tokens.first() == new.tokens.first();
                let suffix_equal = nonempty && old.tokens.last() == new.tokens.last();
                let mut seeded_comparisons = 0usize;
                let seeded = cached_sentence_edge_evidence_from_aligned_facts(
                    old,
                    new,
                    prefix_equal,
                    suffix_equal,
                    || {
                        seeded_comparisons += 1;
                        true
                    },
                )
                .expect("seeded edge evidence computes");

                assert_eq!(seeded.prefix_tokens(), baseline.prefix_tokens());
                assert_eq!(seeded.suffix_tokens(), baseline.suffix_tokens());
                assert_eq!(seeded.shorter_tokens(), baseline.shorter_tokens());
                assert_eq!(seeded.edge_score(), baseline.edge_score());
                assert_eq!(baseline_comparisons, baseline.comparisons());
                assert_eq!(seeded_comparisons, seeded.comparisons());
                let saved = usize::from(baseline.shorter_tokens() != 0)
                    + usize::from(baseline.prefix_tokens() < baseline.shorter_tokens());
                assert_eq!(
                    seeded_comparisons.checked_add(saved),
                    Some(baseline_comparisons)
                );
            }
        }
    }

    #[test]
    fn sentence_edge_evidence_preserves_prefix_and_suffix_budget_failures() {
        let occurrence = |text: &str| {
            similarity_occurrence(
                text,
                text.chars().map(SentenceEvidenceToken::Scalar).collect(),
                RecoveryUnitKind::Sentence,
            )
        };
        let old = occurrence("ab");
        let new = occurrence("xb");

        let mut prefix_budget = RecoveryBudget::new(2, 2, 4, 1).expect("valid prefix budget");
        prefix_budget.comparison_limit = 0;
        assert!(
            sentence_edge_evidence(
                &old,
                &new,
                &mut prefix_budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            )
            .is_none()
        );
        assert_eq!(prefix_budget.comparisons, 0);
        assert_eq!(prefix_budget.comparisons_attempted, 1);

        let mut suffix_budget = RecoveryBudget::new(2, 2, 4, 1).expect("valid suffix budget");
        suffix_budget.comparison_limit = 1;
        assert!(
            sentence_edge_evidence(
                &old,
                &new,
                &mut suffix_budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            )
            .is_none()
        );
        assert_eq!(suffix_budget.comparisons, 1);
        assert_eq!(suffix_budget.comparisons_attempted, 2);
    }

    #[test]
    fn sentence_edge_evidence_handles_empty_input_without_charges() {
        let empty = similarity_occurrence("", Vec::new(), RecoveryUnitKind::Sentence);
        let nonempty = similarity_occurrence(
            "a",
            vec![SentenceEvidenceToken::Scalar('a')],
            RecoveryUnitKind::Sentence,
        );
        let mut budget = RecoveryBudget::new(0, 1, 1, 1).expect("valid empty-input budget");

        {
            let evidence = sentence_edge_evidence(
                &empty,
                &nonempty,
                &mut budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            )
            .expect("empty evidence has a score");

            assert_eq!(evidence.prefix_tokens(), 0);
            assert_eq!(evidence.suffix_tokens(), 0);
            assert_eq!(evidence.shorter_tokens(), 0);
            assert_eq!(evidence.edge_score(), 0);
        }
        assert_eq!(budget.comparisons, 0);
        assert_eq!(budget.comparisons_attempted, 0);
    }

    #[test]
    fn sentence_edge_evidence_preserves_the_line_length_ratio_guard() {
        let old = similarity_occurrence(
            "abcd",
            "abcd".chars().map(SentenceEvidenceToken::Scalar).collect(),
            RecoveryUnitKind::Line,
        );
        let new = similarity_occurrence(
            "a",
            vec![SentenceEvidenceToken::Scalar('a')],
            RecoveryUnitKind::Line,
        );
        let mut budget = RecoveryBudget::new(4, 1, 5, 1).expect("valid line budget");

        let evidence = sentence_edge_evidence(
            &old,
            &new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
        )
        .expect("guarded line evidence has a score");

        assert_eq!(evidence.shorter_tokens(), 1);
        assert_eq!(evidence.edge_score(), 0);
        assert_eq!(
            sentence_similarity_in_scope_attributed_from_edge_evidence(evidence),
            Some(0)
        );
        assert_eq!(budget.comparisons, 0);
    }

    #[test]
    fn sentence_similarity_consumer_uses_only_evidence_and_matches_the_wrapper() {
        let old = similarity_occurrence(
            "alpha beta",
            "alpha beta"
                .chars()
                .map(SentenceEvidenceToken::Scalar)
                .collect(),
            RecoveryUnitKind::Sentence,
        );
        let new = similarity_occurrence(
            "alpha zeta",
            "alpha zeta"
                .chars()
                .map(SentenceEvidenceToken::Scalar)
                .collect(),
            RecoveryUnitKind::Sentence,
        );
        let mut wrapper_budget = RecoveryBudget::new(10, 10, 20, 1).expect("valid wrapper budget");
        let mut explicit_budget = wrapper_budget;

        let wrapper_score = sentence_similarity_in_scope_attributed(
            &old,
            &new,
            &mut wrapper_budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
        );
        let evidence = sentence_edge_evidence(
            &old,
            &new,
            &mut explicit_budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
        )
        .expect("explicit edge evidence fits the budget");
        let consume: for<'a> fn(SentenceEdgeEvidence<'a>) -> Option<u16> =
            sentence_similarity_in_scope_attributed_from_edge_evidence;
        let explicit_score = consume(evidence);

        assert_eq!(explicit_score, wrapper_score);
        assert_eq!(explicit_budget.comparisons, wrapper_budget.comparisons);
        assert_eq!(
            explicit_budget.comparisons_attempted,
            wrapper_budget.comparisons_attempted
        );
    }

    #[test]
    fn line_similarity_skips_ngrams_when_the_upper_bound_cannot_improve() {
        let occurrence = |text: &str| {
            similarity_occurrence(
                text,
                text.chars().map(SentenceEvidenceToken::Scalar).collect(),
                RecoveryUnitKind::Line,
            )
        };

        let old = occurrence("abcd");
        let same = occurrence("abcd");
        let mut equality_budget = RecoveryBudget::new(4, 4, 8, 1).expect("valid budget");
        assert_eq!(
            sentence_similarity_in_scope(&old, &same, &mut equality_budget, TEST_NEAR_SCOPE),
            Some(10_000)
        );
        assert_eq!(equality_budget.comparisons, 4);

        let longer = occurrence("abXcd");
        let mut unequal_budget = RecoveryBudget::new(4, 5, 9, 1).expect("valid budget");
        assert_eq!(
            sentence_similarity_in_scope(&old, &longer, &mut unequal_budget, TEST_NEAR_SCOPE),
            Some(10_000)
        );
        assert_eq!(unequal_budget.comparisons, 5);
    }

    #[test]
    fn line_similarity_evaluates_ngrams_when_the_upper_bound_is_one_point_higher() {
        let mut new_tokens = vec![SentenceEvidenceToken::Scalar('a'); 10_000];
        new_tokens[5_000] = SentenceEvidenceToken::Scalar('b');
        let old = similarity_occurrence(
            "old",
            vec![SentenceEvidenceToken::Scalar('a'); 10_000],
            RecoveryUnitKind::Line,
        );
        let new = similarity_occurrence("new", new_tokens, RecoveryUnitKind::Line);
        let mut budget = RecoveryBudget::new(10_000, 10_000, 20_000, 1).expect("valid budget");

        assert_eq!(
            sentence_similarity_in_scope(&old, &new, &mut budget, TEST_NEAR_SCOPE),
            Some(9_999)
        );
        assert!(budget.comparisons > 10_000);
    }

    #[test]
    fn sentence_similarity_skips_word_dice_at_its_unequal_count_upper_bound() {
        let old = similarity_occurrence(
            "alpha",
            vec![
                SentenceEvidenceToken::Scalar('a'),
                SentenceEvidenceToken::Scalar('b'),
            ],
            RecoveryUnitKind::Sentence,
        );
        let new = similarity_occurrence(
            "beta delta gamma",
            vec![
                SentenceEvidenceToken::Scalar('a'),
                SentenceEvidenceToken::Scalar('x'),
                SentenceEvidenceToken::Scalar('y'),
                SentenceEvidenceToken::Scalar('z'),
            ],
            RecoveryUnitKind::Sentence,
        );
        let mut budget = RecoveryBudget::new(2, 4, 6, 1).expect("valid budget");

        assert_eq!(
            sentence_similarity_in_scope(&old, &new, &mut budget, TEST_NEAR_SCOPE),
            Some(5_000)
        );
        assert_eq!(budget.comparisons, 3);
    }

    #[test]
    fn sentence_similarity_evaluates_word_dice_when_its_upper_bound_is_one_point_higher() {
        let mut new_tokens = vec![SentenceEvidenceToken::Scalar('a'); 10_000];
        new_tokens[5_000] = SentenceEvidenceToken::Scalar('b');
        let old = similarity_occurrence(
            "alpha",
            vec![SentenceEvidenceToken::Scalar('a'); 10_000],
            RecoveryUnitKind::Sentence,
        );
        let new = similarity_occurrence("alpha", new_tokens, RecoveryUnitKind::Sentence);
        let mut budget = RecoveryBudget::new(10_000, 10_000, 20_000, 1).expect("valid budget");

        assert_eq!(
            sentence_similarity_in_scope(&old, &new, &mut budget, TEST_NEAR_SCOPE),
            Some(10_000)
        );
        assert_eq!(budget.comparisons, 10_002);
    }

    #[test]
    fn word_multiset_early_stop_preserves_reference_scores_with_duplicates() {
        fn enumerate_sorted(values: &mut Vec<u8>, next: u8, output: &mut Vec<Vec<u8>>) {
            output.push(values.clone());
            if values.len() == 4 {
                return;
            }
            for value in next..3 {
                values.push(value);
                enumerate_sorted(values, value, output);
                values.pop();
            }
        }

        let mut multisets = Vec::new();
        enumerate_sorted(&mut Vec::new(), 0, &mut multisets);
        for old_words in &multisets {
            for new_words in &multisets {
                let old = occurrence_from_word_ids(old_words, RecoveryUnitKind::Sentence);
                let new = occurrence_from_word_ids(new_words, RecoveryUnitKind::Sentence);
                let actual = word_multiset_reference(old_words, new_words);
                for floor in [0, 2_500, 5_000, 9_999, 10_000] {
                    let token_count = old.tokens.len() + new.tokens.len();
                    let mut budget =
                        RecoveryBudget::new(old.tokens.len(), new.tokens.len(), token_count, 1)
                            .expect("small multiset budget is valid");
                    assert_eq!(
                        word_multiset_dice(
                            &old,
                            &new,
                            floor,
                            RecoveryUnitKind::Sentence,
                            &mut budget,
                            TEST_NEAR_SCOPE,
                            NearSearchWorkClass::Shared,
                        ),
                        Some(floor.max(actual)),
                        "old={old_words:?}, new={new_words:?}, floor={floor}"
                    );
                }
            }
        }
    }

    #[test]
    fn word_multiset_early_stop_respects_rounded_floor_boundaries() {
        let old = occurrence_from_word_ids(&[0; 10], RecoveryUnitKind::Sentence);
        let new = occurrence_from_word_ids(&[1; 10], RecoveryUnitKind::Sentence);

        let mut equality_budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("equality boundary budget is valid");
        assert_eq!(
            word_multiset_dice(
                &old,
                &new,
                9_000,
                RecoveryUnitKind::Sentence,
                &mut equality_budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            ),
            Some(9_000)
        );
        assert_eq!(equality_budget.comparisons, 1);

        let mut above_budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("one-point boundary budget is valid");
        assert_eq!(
            word_multiset_dice(
                &old,
                &new,
                8_999,
                RecoveryUnitKind::Sentence,
                &mut above_budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            ),
            Some(8_999)
        );
        assert_eq!(above_budget.comparisons, 2);
    }

    #[test]
    fn word_multiset_early_stop_reduces_work_and_preserves_line_counters() {
        let old = occurrence_from_word_ids(&[0; 100], RecoveryUnitKind::Line);
        let new = occurrence_from_word_ids(&[1; 100], RecoveryUnitKind::Line);
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("line multiset budget is valid");

        assert_eq!(
            word_multiset_dice(
                &old,
                &new,
                5_000,
                RecoveryUnitKind::Line,
                &mut budget,
                NearSearchScope::CrossSpan,
                NearSearchWorkClass::Shared,
            ),
            Some(5_000)
        );
        assert_eq!(budget.comparisons, 50);
        assert_eq!(budget.comparisons_attempted, 50);
        assert_eq!(budget.line_work.similarity_comparisons_examined, 50);
        assert_eq!(budget.line_work.similarity_comparisons_attempted, 50);
        assert_eq!(budget.sentence_work.similarity_comparisons_examined, 0);
        assert_eq!(
            budget
                .cross_span_work
                .line_work
                .similarity_comparisons_examined,
            50
        );
        assert_eq!(budget.near_relation_stop_reason, None);
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn word_multiset_budget_failure_keeps_atomic_comparison_counters() {
        let old = occurrence_from_word_ids(&[0; 10], RecoveryUnitKind::Sentence);
        let new = occurrence_from_word_ids(&[0; 10], RecoveryUnitKind::Sentence);
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("multiset budget is valid");
        budget.comparison_limit = 3;

        assert_eq!(
            word_multiset_dice(
                &old,
                &new,
                0,
                RecoveryUnitKind::Sentence,
                &mut budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
            ),
            None
        );
        assert_eq!(budget.comparisons, 3);
        assert_eq!(budget.comparisons_attempted, 4);
        assert_eq!(budget.sentence_work.similarity_comparisons_examined, 3);
        assert_eq!(budget.sentence_work.similarity_comparisons_attempted, 4);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::SimilarityComparisonLimit)
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn relation_floor_uses_current_directional_second_scores_and_safe_ceiling() {
        let relation = |second_score| CandidateNearRelation {
            second_score,
            ..CandidateNearRelation::default()
        };

        let eligible = relation_floor_probe(
            NearRelationScope::CrossSpan,
            RecoveryUnitKind::Sentence,
            Some(relation(6_500)),
            Some(relation(6_200)),
        )
        .expect("cross-span sentence pair is measured");
        assert_eq!(eligible.ceiling, 6_200);

        let forward = relation_floor_probe(
            NearRelationScope::CrossSpan,
            RecoveryUnitKind::Sentence,
            Some(relation(6_400)),
            None,
        )
        .expect("forward disqualifying pair is measured");
        assert_eq!(forward.ceiling, 6_400);

        let reverse = relation_floor_probe(
            NearRelationScope::CrossSpan,
            RecoveryUnitKind::Sentence,
            None,
            Some(relation(6_300)),
        )
        .expect("reverse disqualifying pair is measured");
        assert_eq!(reverse.ceiling, 6_300);

        let capped = relation_floor_probe(
            NearRelationScope::CrossSpan,
            RecoveryUnitKind::Sentence,
            Some(relation(7_000)),
            Some(relation(u16::MAX)),
        )
        .expect("effective near threshold is capped");
        assert_eq!(capped.ceiling, 6_999);
        assert!(
            relation_floor_probe(
                NearRelationScope::SameOrAmbiguous,
                RecoveryUnitKind::Sentence,
                Some(relation(6_000)),
                None,
            )
            .is_none()
        );
        assert!(
            relation_floor_probe(
                NearRelationScope::CrossSpan,
                RecoveryUnitKind::Line,
                Some(relation(6_000)),
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn relation_floor_word_probe_uses_exact_bound_and_excludes_triggering_comparison() {
        let old = occurrence_from_word_ids(&[0; 10], RecoveryUnitKind::Sentence);
        let new = occurrence_from_word_ids(&[1; 10], RecoveryUnitKind::Sentence);
        let run = |floor, ceiling| {
            let mut budget = RecoveryBudget::new(
                old.tokens.len(),
                new.tokens.len(),
                old.tokens.len() + new.tokens.len(),
                1,
            )
            .expect("probe budget is valid");
            let mut probe = RelationFloorProbe::new(ceiling);
            let score = word_multiset_dice_with_probe(
                &old,
                &new,
                floor,
                RecoveryUnitKind::Sentence,
                &mut budget,
                TEST_NEAR_SCOPE,
                NearSearchWorkClass::Shared,
                &mut probe,
            );
            (score, budget.comparisons, probe)
        };

        let (_, _, equality) = run(0, 9_000);
        assert_eq!(equality.stop_opportunities, 1);
        assert_eq!(equality.potential_saved_word_comparisons, 9);

        let (_, _, one_point_lower) = run(0, 8_999);
        assert_eq!(one_point_lower.stop_opportunities, 1);
        assert_eq!(one_point_lower.potential_saved_word_comparisons, 8);

        let (_, comparisons, exact_stop) = run(9_000, 9_000);
        assert_eq!(comparisons, 1);
        assert_eq!(exact_stop.stop_opportunities, 1);
        assert_eq!(exact_stop.potential_saved_word_comparisons, 0);
    }

    #[test]
    fn relation_floor_ignores_word_scan_when_edge_score_exceeds_ceiling() {
        let old = occurrence_from_word_ids(&[0, 1], RecoveryUnitKind::Sentence);
        let new = occurrence_from_word_ids(&[0, 2], RecoveryUnitKind::Sentence);
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("probe budget is valid");
        let mut probe = RelationFloorProbe::new(6_000);

        let score = sentence_similarity_in_scope_attributed_with_probe(
            &old,
            &new,
            &mut budget,
            TEST_NEAR_SCOPE,
            NearSearchWorkClass::Shared,
            &mut probe,
        )
        .expect("similarity completes");

        assert!(score > probe.ceiling);
        assert_eq!(probe.word_scans, 1);
        assert_eq!(probe.stop_opportunities, 0);
        assert_eq!(probe.potential_saved_word_comparisons, 0);
    }

    #[test]
    fn relation_floor_probe_preserves_scores_and_budget_counters() {
        let old = occurrence_from_word_ids(&[0; 10], RecoveryUnitKind::Sentence);
        let new = occurrence_from_word_ids(&[1; 10], RecoveryUnitKind::Sentence);
        let budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("probe budget is valid");
        let mut production_budget = budget;
        let mut diagnostic_budget = budget;
        let production = sentence_similarity_in_scope_attributed(
            &old,
            &new,
            &mut production_budget,
            NearSearchScope::CrossSpan,
            NearSearchWorkClass::Shared,
        );
        let mut probe = RelationFloorProbe::new(6_999);
        let diagnostic = sentence_similarity_in_scope_attributed_with_probe(
            &old,
            &new,
            &mut diagnostic_budget,
            NearSearchScope::CrossSpan,
            NearSearchWorkClass::Shared,
            &mut probe,
        );

        assert_eq!(diagnostic, production);
        assert_eq!(diagnostic_budget.comparisons, production_budget.comparisons);
        assert_eq!(
            diagnostic_budget.comparisons_attempted,
            production_budget.comparisons_attempted
        );
        assert_eq!(
            diagnostic_budget.sentence_work,
            production_budget.sentence_work
        );
        assert_eq!(diagnostic_budget.line_work, production_budget.line_work);
        assert_eq!(
            diagnostic_budget.cross_span_work,
            production_budget.cross_span_work
        );
        assert_eq!(
            diagnostic_budget.near_relation_stop_reason,
            production_budget.near_relation_stop_reason
        );
    }

    #[test]
    fn relation_floor_probe_is_not_committed_after_budget_failure() {
        let old = occurrence_from_word_ids(&[0; 10], RecoveryUnitKind::Sentence);
        let new = occurrence_from_word_ids(&[1; 10], RecoveryUnitKind::Sentence);
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            new.tokens.len(),
            old.tokens.len() + new.tokens.len(),
            1,
        )
        .expect("probe budget is valid");
        budget.comparison_limit = 3;
        let mut probe = RelationFloorProbe::new(9_000);
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let score = word_multiset_dice_with_probe(
            &old,
            &new,
            0,
            RecoveryUnitKind::Sentence,
            &mut budget,
            NearSearchScope::CrossSpan,
            NearSearchWorkClass::Shared,
            &mut probe,
        );
        if score.is_some() {
            commit_relation_floor_probe(&mut diagnostics, probe);
        }

        assert_eq!(score, None);
        assert_eq!(
            diagnostics.map(|diagnostics| diagnostics.metrics),
            Some(SentenceRecoveryMetrics::default())
        );
    }

    #[test]
    fn speculative_relation_floor_diagnostics_commit_atomically() {
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                relation_floor_pairs_considered: 1,
                relation_floor_word_scans: 1,
                relation_floor_stop_opportunities: 1,
                relation_floor_potential_saved_word_comparisons: 1,
                near_pair_candidates: 7,
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let speculative = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                relation_floor_pairs_considered: 5,
                relation_floor_word_scans: 3,
                relation_floor_stop_opportunities: 2,
                relation_floor_potential_saved_word_comparisons: 5,
                near_pair_candidates: 99,
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        commit_relation_floor_diagnostics_from(&mut diagnostics, speculative);

        let metrics = diagnostics.expect("diagnostics remain available").metrics;
        assert_eq!(metrics.relation_floor_pairs_considered, 5);
        assert_eq!(metrics.relation_floor_word_scans, 3);
        assert_eq!(metrics.relation_floor_stop_opportunities, 2);
        assert_eq!(metrics.relation_floor_potential_saved_word_comparisons, 5);
        assert_eq!(metrics.near_pair_candidates, 7);

        let mut unavailable = Some(SentenceRecoveryDiagnostics {
            metrics,
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        commit_relation_floor_diagnostics_from(&mut unavailable, None);
        assert!(unavailable.is_none());

        let mut disabled = None;
        commit_relation_floor_diagnostics_from(&mut disabled, speculative);
        assert!(disabled.is_none());
    }

    #[test]
    fn relation_floor_diagnostics_stay_unchanged_when_first_cross_pair_fails() {
        let occurrence = |words: &[u8], token: char, span_index| {
            let mut occurrence = occurrence_from_word_ids(words, RecoveryUnitKind::Sentence);
            occurrence.tokens = (0..10)
                .map(|index| SentenceEvidenceToken::Scalar(if index < 3 { 'x' } else { token }))
                .collect();
            occurrence.span_index = Some(span_index);
            occurrence
        };
        let old_occurrences = [occurrence(&[0; 10], 'a', 0)];
        let new_occurrences = [occurrence(&[1; 10], 'b', 1)];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut relations = empty_modified_sentence_relations(&candidates, &candidates)
            .expect("relation allocation succeeds");
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("test budget is valid");
        budget.comparison_limit = 0;
        let initial_metrics = SentenceRecoveryMetrics {
            relation_floor_pairs_considered: 4,
            relation_floor_word_scans: 3,
            relation_floor_stop_opportunities: 2,
            relation_floor_potential_saved_word_comparisons: 4,
            ..SentenceRecoveryMetrics::default()
        };
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: initial_metrics,
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        assert!(
            extend_modified_sentence_relations(
                &old_occurrences,
                &new_occurrences,
                &candidates,
                &candidates,
                NearRelationScope::CrossSpan,
                &mut relations,
                &mut budget,
                &mut diagnostics,
                None,
                None,
            )
            .is_none()
        );

        assert_eq!(
            diagnostics.map(|diagnostics| diagnostics.metrics),
            Some(initial_metrics)
        );
    }

    #[test]
    fn relation_floor_metrics_follow_cross_span_relation_traversal() {
        fn run(
            scope: NearRelationScope,
            kind: RecoveryUnitKind,
            diagnostics_enabled: bool,
        ) -> (Option<SentenceRecoveryMetrics>, usize) {
            let occurrence = |words: &[u8], token: char, span_index| {
                let mut occurrence = occurrence_from_word_ids(words, kind);
                occurrence.tokens = (0..10)
                    .map(|index| SentenceEvidenceToken::Scalar(if index < 3 { 'x' } else { token }))
                    .collect();
                occurrence.span_index = Some(span_index);
                occurrence
            };
            let old_occurrences = [occurrence(&[0; 10], 'a', 0), occurrence(&[2; 10], 'c', 0)];
            let new_occurrences = [occurrence(&[1; 10], 'b', 1)];
            let old_candidates = [RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }];
            let new_candidates = [RecoveryCandidate {
                occurrence_index: 0,
                span_index: 1,
            }];
            let mut relations = empty_modified_sentence_relations(&old_candidates, &new_candidates)
                .expect("relation allocation succeeds");
            relations.old[0] = CandidateNearRelation {
                best_score: 8_000,
                second_score: 6_999,
                best_partner: Some(0),
            };
            relations.new[0] = relations.old[0];
            let mut budget = RecoveryBudget::new(20, 10, 30, 1).expect("budget is valid");
            let mut diagnostics = diagnostics_enabled.then_some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics::default(),
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            });

            extend_modified_sentence_relations(
                &old_occurrences,
                &new_occurrences,
                &old_candidates,
                &new_candidates,
                scope,
                &mut relations,
                &mut budget,
                &mut diagnostics,
                None,
                None,
            )
            .expect("relation traversal completes");

            (
                diagnostics.map(|diagnostics| diagnostics.metrics),
                budget.comparisons,
            )
        }

        let (cross_span, diagnostic_comparisons) = run(
            NearRelationScope::CrossSpan,
            RecoveryUnitKind::Sentence,
            true,
        );
        let cross_span = cross_span.expect("diagnostics remain available");
        assert_eq!(cross_span.relation_floor_pairs_considered, 2);
        assert_eq!(cross_span.relation_floor_word_scans, 2);
        assert_eq!(cross_span.relation_floor_stop_opportunities, 2);
        assert_eq!(
            cross_span.relation_floor_potential_saved_word_comparisons,
            6
        );

        let (disabled, production_comparisons) = run(
            NearRelationScope::CrossSpan,
            RecoveryUnitKind::Sentence,
            false,
        );
        assert_eq!(disabled, None);
        assert_eq!(production_comparisons, diagnostic_comparisons);

        let (same_or_ambiguous, _) = run(
            NearRelationScope::SameOrAmbiguous,
            RecoveryUnitKind::Sentence,
            true,
        );
        assert_eq!(
            same_or_ambiguous
                .expect("diagnostics remain available")
                .relation_floor_pairs_considered,
            0
        );

        let (line, _) = run(NearRelationScope::CrossSpan, RecoveryUnitKind::Line, true);
        assert_eq!(
            line.expect("diagnostics remain available")
                .relation_floor_pairs_considered,
            0
        );
    }

    #[test]
    fn atomic_line_boundary_keeps_trimmed_non_sentence_text() {
        let text = "  arXiv:1706.03762v7 [cs.CL] 2 Aug 2023  ";
        let mut budget = RecoveryBudget::new(text.chars().count(), 0, text.chars().count(), 1)
            .expect("line boundary budget is valid");
        let boundaries =
            atomic_line_boundaries(text, &mut budget).expect("line boundary fits budget");

        assert_eq!(boundaries.len(), 1);
        let boundary = boundaries[0];
        assert_eq!(
            &text[boundary.byte_start..boundary.byte_end],
            "arXiv:1706.03762v7 [cs.CL] 2 Aug 2023"
        );
        assert_eq!(boundary.scalar_start, 2);
        assert_eq!(boundary.scalar_end, text.chars().count() - 2);
    }

    #[test]
    fn sentence_boundaries_require_real_terminals_and_keep_closers() {
        let text = "  One sentence. \"Another one!\" trailing";
        assert_eq!(
            boundary_texts(text, text.chars().count()),
            ["One sentence.", "\"Another one!\""]
        );
    }

    #[test]
    fn false_period_boundaries_coalesce_until_a_real_terminal() {
        for text in [
            "Dr. Smith left.",
            "A. Person left.",
            "See Fig. 2 for details.",
            "Use v1.2. Then continue.",
            "Visit https://example.com. Then continue.",
            "Write user@example.com. Then continue.",
            "The U.S. office closed.",
            "Use Alg. Next step.",
            "Visit St. Louis.",
        ] {
            let boundaries = boundary_texts(text, text.chars().count());
            assert_eq!(boundaries.len(), 1, "{text:?}");
            assert_eq!(boundaries[0], text, "{text:?}");
        }
    }

    #[test]
    fn longer_mixed_case_words_remain_normal_terminals() {
        let text = "Alpha. Next step.";
        assert_eq!(
            boundary_texts(text, text.chars().count()),
            ["Alpha.", "Next step."]
        );
    }

    #[test]
    fn recovery_budget_accepts_exact_limits_and_rejects_one_more() {
        let mut budget = RecoveryBudget::new(3, 2, 5, 1).expect("budget is valid");
        assert!(budget.charge_occurrences(5));
        assert!(!budget.charge_occurrences(1));
        assert!(budget.charge_key_bytes(20));
        assert!(!budget.charge_key_bytes(1));
        assert!(budget.charge_pair_visits_in_scope(5, RecoveryUnitKind::Sentence, TEST_NEAR_SCOPE));
        assert!(!budget.charge_pair_visits_in_scope(
            1,
            RecoveryUnitKind::Sentence,
            TEST_NEAR_SCOPE
        ));
        assert!(budget.charge_comparisons_in_scope(
            20,
            RecoveryUnitKind::Sentence,
            TEST_NEAR_SCOPE
        ));
        assert!(!budget.charge_comparisons_in_scope(
            1,
            RecoveryUnitKind::Sentence,
            TEST_NEAR_SCOPE
        ));
        assert_eq!(budget.pair_visits, 5);
        assert_eq!(budget.pair_visits_attempted, 6);
        assert_eq!(budget.comparisons, 20);
        assert_eq!(budget.comparisons_attempted, 21);
        assert_eq!(budget.sentence_work.filtered_candidates, 6);
        assert_near_work_sums_match_aggregates(&budget);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::PairVisitLimit)
        );
        assert!(budget.charge_evidence_tokens(5));
        assert!(!budget.charge_evidence_tokens(1));

        let mut output = RecoveryBudget::new(3, 2, 5, 1).expect("budget is valid");
        for _ in 0..5 {
            assert!(output.charge_output(1));
        }
        assert!(!output.charge_output(1));

        let mut fragment = FragmentVetoBudget::new(3, 2).expect("fragment budget is valid");
        assert!(fragment.charge_pair_visits(5));
        assert!(!fragment.charge_pair_visits(1));
        assert_eq!(
            fragment.stop_reason(),
            FragmentVetoStopReason::PairVisitLimit
        );
        let mut fragment = FragmentVetoBudget::new(3, 2).expect("fragment budget is valid");
        assert!(fragment.charge_comparisons(20));
        assert!(!fragment.charge_comparisons(1));
        assert_eq!(
            fragment.stop_reason(),
            FragmentVetoStopReason::SimilarityComparisonLimit
        );
    }

    #[test]
    fn fragment_completion_uses_scalar_evidence_and_fails_atomically_on_budget_exhaustion() {
        let tokens = |text: &str| {
            text.chars()
                .map(SentenceEvidenceToken::Scalar)
                .collect::<Vec<_>>()
        };
        let mut old = positioned_occurrence("full", 1, 0, 0);
        old.tokens = tokens("前半 後半です。");
        let mut new = positioned_occurrence("suffix", 2, 1, 0);
        new.tokens = tokens("後半です。");
        let fragment = SentenceFragment {
            tokens: tokens("前半"),
            span_index: 0,
            role: BlockRole::Body,
            uncertain: false,
        };
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let new_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut old_relation = CandidateNearRelation::default();
        old_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut new_relation = CandidateNearRelation::default();
        new_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut relations = ModifiedSentenceRelations {
            old: vec![old_relation],
            new: vec![new_relation],
            complete: true,
            edge_gate_shadow: None,
            edge_signature_shadow: None,
        };
        let mut budget = FragmentVetoBudget::new(old.tokens.len(), new.tokens.len())
            .expect("test budget is valid");
        budget.comparison_limit = 1;
        let before = budget;

        veto_fragment_completed_replacements(
            &[old],
            &[new],
            &old_candidates,
            &new_candidates,
            &[],
            &[fragment],
            &mut relations,
            &mut budget,
        );
        assert!(relations.old[0].vetoed());
        assert!(relations.new[0].vetoed());
        assert_eq!(relations.old[0].unique_partner(), None);
        assert_eq!(relations.new[0].unique_partner(), None);
        assert!(budget.pair_visits >= before.pair_visits);
        assert_eq!(budget.pair_visits, budget.pair_visits_attempted);
        assert!(budget.comparisons_attempted > budget.comparisons);
        assert_eq!(
            budget.stop_reason(),
            FragmentVetoStopReason::SimilarityComparisonLimit
        );

        let mut old = positioned_occurrence("full", 3, 0, 0);
        old.tokens = tokens("前半です。 後半");
        let mut new = positioned_occurrence("prefix", 4, 1, 0);
        new.tokens = tokens("前半です。");
        let mut old_relation = CandidateNearRelation::default();
        old_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut new_relation = CandidateNearRelation::default();
        new_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut relations = ModifiedSentenceRelations {
            old: vec![old_relation],
            new: vec![new_relation],
            complete: true,
            edge_gate_shadow: None,
            edge_signature_shadow: None,
        };
        let mut budget = FragmentVetoBudget::new(old.tokens.len(), new.tokens.len())
            .expect("test budget is valid");
        let fragments = [
            SentenceFragment {
                tokens: tokens("誤答"),
                span_index: 0,
                role: BlockRole::Body,
                uncertain: false,
            },
            SentenceFragment {
                tokens: tokens("後半"),
                span_index: 0,
                role: BlockRole::Body,
                uncertain: false,
            },
        ];

        veto_fragment_completed_replacements(
            &[old],
            &[new],
            &old_candidates,
            &new_candidates,
            &[],
            &fragments,
            &mut relations,
            &mut budget,
        );
        assert!(relations.old[0].vetoed());
        assert!(relations.new[0].vetoed());
        assert_eq!(relations.old[0].unique_partner(), None);
        assert_eq!(relations.new[0].unique_partner(), None);

        let unmapped = SentenceEvidenceToken::Unmapped {
            font_fingerprint: 1,
            glyph_id: 1,
        };
        let mut exact_budget = FragmentVetoBudget::new(3, 0).expect("test budget is valid");
        assert_eq!(
            joined_tokens_equal(
                &[unmapped],
                &[SentenceEvidenceToken::Scalar('a')],
                &[
                    SentenceEvidenceToken::Scalar('x'),
                    SentenceEvidenceToken::Scalar(' '),
                    SentenceEvidenceToken::Scalar('a'),
                ],
                &mut exact_budget
            ),
            Some(false)
        );
    }

    #[test]
    fn cross_span_fragments_charge_visits_before_span_rejection() {
        let mut old = positioned_occurrence("full", 1, 0, 0);
        old.tokens = vec![SentenceEvidenceToken::Scalar('a'); 10];
        let mut new = positioned_occurrence("short", 2, 1, 0);
        new.tokens = vec![SentenceEvidenceToken::Scalar('a'); 5];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut old_relation = CandidateNearRelation::default();
        old_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut new_relation = CandidateNearRelation::default();
        new_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut relations = ModifiedSentenceRelations {
            old: vec![old_relation],
            new: vec![new_relation],
            complete: true,
            edge_gate_shadow: None,
            edge_signature_shadow: None,
        };
        let fragments = (0..16)
            .map(|_| SentenceFragment {
                tokens: vec![SentenceEvidenceToken::Scalar('x')],
                span_index: 1,
                role: BlockRole::Body,
                uncertain: true,
            })
            .collect::<Vec<_>>();
        let mut budget = FragmentVetoBudget::new(10, 5).expect("test budget is valid");
        let before = budget;

        veto_fragment_completed_replacements(
            &[old],
            &[new],
            &candidates,
            &candidates,
            &[],
            &fragments,
            &mut relations,
            &mut budget,
        );
        assert!(relations.old[0].vetoed());
        assert!(relations.new[0].vetoed());
        assert!(budget.pair_visits >= before.pair_visits);
        assert!(budget.pair_visits_attempted > budget.pair_visits);
        assert_eq!(budget.comparisons, budget.comparisons_attempted);
    }

    #[test]
    fn same_length_fragment_bucket_scan_is_budgeted_atomically() {
        let mut old = positioned_occurrence("full", 1, 0, 0);
        old.tokens = vec![SentenceEvidenceToken::Scalar('a'); 10];
        let mut new = positioned_occurrence("short", 2, 1, 0);
        new.tokens = vec![SentenceEvidenceToken::Scalar('a'); 5];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut old_relation = CandidateNearRelation::default();
        old_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut new_relation = CandidateNearRelation::default();
        new_relation.record_eligible(0, MIN_NEAR_SCORE);
        let mut relations = ModifiedSentenceRelations {
            old: vec![old_relation],
            new: vec![new_relation],
            complete: true,
            edge_gate_shadow: None,
            edge_signature_shadow: None,
        };
        let fragments = (0..8)
            .map(|_| SentenceFragment {
                tokens: vec![SentenceEvidenceToken::Scalar('x'); 4],
                span_index: 0,
                role: BlockRole::Body,
                uncertain: false,
            })
            .collect::<Vec<_>>();
        let mut budget = FragmentVetoBudget::new(10, 5).expect("test budget is valid");
        let before = budget;

        veto_fragment_completed_replacements(
            &[old],
            &[new],
            &candidates,
            &candidates,
            &[],
            &fragments,
            &mut relations,
            &mut budget,
        );
        assert!(relations.old[0].vetoed());
        assert!(relations.new[0].vetoed());
        assert!(budget.pair_visits >= before.pair_visits);
        assert!(budget.pair_visits_attempted > budget.pair_visits);
        assert_eq!(budget.comparisons, budget.comparisons_attempted);
        assert_eq!(budget.stop_reason(), FragmentVetoStopReason::PairVisitLimit);
    }

    #[test]
    fn fragment_budget_fallback_vetoes_near_pair_and_keeps_one_sided_recovery() {
        let mut old_occurrences = [
            positioned_occurrence("near old", 1, 0, 0),
            positioned_occurrence("deleted", 2, 1, 0),
        ];
        let mut new_occurrences = [
            positioned_occurrence("near new", 3, 2, 0),
            positioned_occurrence("inserted", 4, 3, 0),
        ];
        let candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 0,
            },
        ];
        let mut old_near = CandidateNearRelation::default();
        old_near.record_eligible(0, MIN_NEAR_SCORE);
        let mut new_near = CandidateNearRelation::default();
        new_near.record_eligible(0, MIN_NEAR_SCORE);
        let mut relations = ModifiedSentenceRelations {
            old: vec![old_near, CandidateNearRelation::default()],
            new: vec![new_near, CandidateNearRelation::default()],
            complete: true,
            edge_gate_shadow: None,
            edge_signature_shadow: None,
        };
        let fragments = [
            SentenceFragment {
                tokens: vec![SentenceEvidenceToken::Scalar('x')],
                span_index: 1,
                role: BlockRole::Body,
                uncertain: false,
            },
            SentenceFragment {
                tokens: vec![SentenceEvidenceToken::Scalar('y')],
                span_index: 1,
                role: BlockRole::Body,
                uncertain: false,
            },
        ];
        let mut fragment_budget = FragmentVetoBudget::new(1, 0).expect("test budget is valid");
        let mut recovery_budget = RecoveryBudget::new(10, 10, 20, 1).expect("test budget is valid");
        let recovery_before = recovery_budget;

        veto_fragment_completed_replacements(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &fragments,
            &mut relations,
            &mut fragment_budget,
        );

        assert!(relations.old[0].vetoed());
        assert!(relations.new[0].vetoed());
        assert!(!relations.old[1].vetoed());
        assert!(!relations.new[1].vetoed());
        assert_eq!(recovery_budget.pair_visits, recovery_before.pair_visits);
        assert_eq!(recovery_budget.comparisons, recovery_before.comparisons);

        let mut plan = SentenceRecoveryPlan::default();
        append_replacements(
            &mut plan,
            &mut old_occurrences,
            &mut new_occurrences,
            &candidates,
            &candidates,
            &relations,
            &mut recovery_budget,
        )
        .expect("vetoed replacement output fits");
        append_candidate_recoveries(
            &mut plan.deletions,
            &mut plan.deletion_consumed,
            &mut old_occurrences,
            &candidates,
            &relations.old,
            &mut recovery_budget,
        )
        .expect("one-sided deletion output fits");
        append_candidate_recoveries(
            &mut plan.insertions,
            &mut plan.insertion_consumed,
            &mut new_occurrences,
            &candidates,
            &relations.new,
            &mut recovery_budget,
        )
        .expect("one-sided insertion output fits");

        assert!(plan.replacements.is_empty());
        assert_eq!(plan.deletions.len(), 1);
        assert_eq!(plan.insertions.len(), 1);
        assert_eq!(plan.deletions[0].blocks, [BlockId(2)]);
        assert_eq!(plan.insertions[0].blocks, [BlockId(4)]);
    }

    #[test]
    fn exact_match_budget_failure_keeps_the_plan_and_locations_untouched() {
        let old_range = LocalSentenceRange {
            block: BlockId(1),
            canonical: ScalarRange { start: 0, end: 5 },
            comparable: TokenRange { start: 0, end: 5 },
        };
        let new_range = LocalSentenceRange {
            block: BlockId(2),
            canonical: ScalarRange { start: 0, end: 5 },
            comparable: TokenRange { start: 0, end: 5 },
        };
        let mut old_occurrences = [SentenceOccurrence {
            key: "same".to_owned(),
            tokens: Vec::new(),
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: Some(test_location(old_range, 0)),
            span_index: Some(0),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }];
        let mut new_occurrences = [SentenceOccurrence {
            key: "same".to_owned(),
            tokens: Vec::new(),
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: Some(test_location(new_range, 0)),
            span_index: Some(0),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }];
        let candidates = [ExactMatchCandidate {
            old_span_index: 0,
            new_span_index: 0,
            old_occurrence_index: 0,
            new_occurrence_index: 0,
        }];
        let mut plan = SentenceRecoveryPlan::default();
        let mut budget = RecoveryBudget::new(5, 5, 10, 1).expect("budget is valid");
        budget.output_range_limit = 1;

        assert!(
            append_exact_matches(
                &mut plan,
                &mut old_occurrences,
                &mut new_occurrences,
                &candidates,
                &mut budget,
            )
            .is_none()
        );
        assert!(plan.matches.is_empty());
        assert!(plan.cross_span_match_old.is_empty());
        assert!(plan.cross_span_match_new.is_empty());
        assert!(plan.deletion_consumed.is_empty());
        assert!(plan.insertion_consumed.is_empty());
        assert!(old_occurrences[0].location.is_some());
        assert!(new_occurrences[0].location.is_some());
        assert_eq!(budget.output_ranges, 0);
        assert_eq!(budget.output_tokens, 0);
    }

    #[test]
    fn exact_match_candidates_are_sorted_for_span_partitioning() {
        let occurrence = |key: &str, block: BlockId, span_index: usize| {
            let range = LocalSentenceRange {
                block,
                canonical: ScalarRange { start: 0, end: 5 },
                comparable: TokenRange { start: 0, end: 5 },
            };
            SentenceOccurrence {
                key: key.to_owned(),
                tokens: vec![SentenceEvidenceToken::Scalar('a'); 5],
                word_ranges: Vec::new(),
                kind: RecoveryUnitKind::Sentence,
                role: Some(BlockRole::Body),
                location: Some(test_location(range, span_index)),
                span_index: Some(span_index),
                trusted_position: None,
                run_descriptor_index: None,
                page: None,
                evidence_block_index: None,
            }
        };
        let old = [
            occurrence("late", BlockId(1), 1),
            occurrence("early", BlockId(2), 0),
        ];
        let new = [
            occurrence("early", BlockId(3), 0),
            occurrence("late", BlockId(4), 1),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");

        let candidates =
            exact_match_candidates(&old, &new, &counts, &[true, true], 5, 2).expect("matches fit");

        assert_eq!(candidates.len(), 2);
        assert_eq!(
            (
                candidates[0].old_span_index,
                candidates[0].new_span_index,
                candidates[0].old_occurrence_index,
                candidates[0].new_occurrence_index,
            ),
            (0, 0, 1, 0)
        );
        assert_eq!(
            (
                candidates[1].old_span_index,
                candidates[1].new_span_index,
                candidates[1].old_occurrence_index,
                candidates[1].new_occurrence_index,
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn exact_recovery_is_role_local_and_preserves_same_role_matches() {
        let old = [positioned_occurrence("same", 1, 0, 0)];
        let mut new = [positioned_occurrence("same", 2, 1, 0)];
        new[0].role = Some(BlockRole::RepeatedHeader);
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let cross_role = exact_match_candidates(&old, &new, &counts, &[true], 5, 1)
            .expect("candidate collection fits");
        assert!(cross_role.is_empty());

        new[0].role = Some(BlockRole::Body);
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let same_role = exact_match_candidates(&old, &new, &counts, &[true], 5, 1)
            .expect("candidate collection fits");
        assert_eq!(same_role.len(), 1);
    }

    #[test]
    fn near_recovery_rejects_cross_role_and_preserves_same_role_pairs() {
        let old = [positioned_occurrence("old", 1, 0, 0)];
        let mut new = [positioned_occurrence("new", 2, 1, 0)];
        new[0].role = Some(BlockRole::RepeatedFooter);
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(5, 5, 10, 1).expect("test budget is valid");
        let mut diagnostics = None;
        let cross_role = modified_sentence_relations(
            &old,
            &new,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("relation collection fits");
        assert!(!cross_role.old[0].vetoed());
        assert!(!cross_role.new[0].vetoed());

        new[0].role = Some(BlockRole::Body);
        let mut budget = RecoveryBudget::new(5, 5, 10, 1).expect("test budget is valid");
        let same_role = modified_sentence_relations(
            &old,
            &new,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("relation collection fits");
        assert_eq!(same_role.old[0].unique_partner(), Some(0));
        assert_eq!(same_role.new[0].unique_partner(), Some(0));
    }

    #[test]
    fn exact_anchor_pairs_trusted_streams_and_recovers_repeated_units_in_order() {
        let old = [
            positioned_occurrence("anchor", 1, 0, 0),
            positioned_occurrence("repeated", 2, 0, 1),
            positioned_occurrence("repeated", 3, 0, 2),
        ];
        let new = [
            positioned_occurrence("anchor", 101, 10, 0),
            positioned_occurrence("repeated", 102, 10, 1),
            positioned_occurrence("repeated", 103, 10, 2),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let mut candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 3)
            .expect("global exact matches fit");

        extend_test_paired_exact_matches(&old, &new, &mut candidates, 3)
            .expect("paired-stream exact matches fit");

        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| (
                    candidate.old_occurrence_index,
                    candidate.new_occurrence_index
                ))
                .collect::<Vec<_>>(),
            [(0, 0), (1, 1), (2, 2)]
        );
    }

    #[test]
    fn exact_anchor_does_not_recover_repeated_units_moved_across_it() {
        let old = [
            positioned_occurrence("anchor", 1, 0, 0),
            positioned_occurrence("repeated", 2, 0, 1),
            positioned_occurrence("repeated", 3, 0, 2),
        ];
        let new = [
            positioned_occurrence("repeated", 101, 10, 0),
            positioned_occurrence("repeated", 102, 10, 1),
            positioned_occurrence("anchor", 103, 10, 2),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let mut candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 3)
            .expect("global exact matches fit");

        extend_test_paired_exact_matches(&old, &new, &mut candidates, 3)
            .expect("cross-anchor moves remain unresolved");

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            (
                candidates[0].old_occurrence_index,
                candidates[0].new_occurrence_index
            ),
            (0, 2)
        );
    }

    #[test]
    fn exact_anchor_does_not_recover_crossing_duplicate_groups_inside_an_interval() {
        let old = [
            positioned_occurrence("anchor", 1, 0, 0),
            positioned_occurrence("a", 2, 0, 1),
            positioned_occurrence("a", 3, 0, 2),
            positioned_occurrence("b", 4, 0, 3),
            positioned_occurrence("b", 5, 0, 4),
        ];
        let new = [
            positioned_occurrence("anchor", 101, 10, 0),
            positioned_occurrence("b", 102, 10, 1),
            positioned_occurrence("b", 103, 10, 2),
            positioned_occurrence("a", 104, 10, 3),
            positioned_occurrence("a", 105, 10, 4),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let mut candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 5)
            .expect("global exact matches fit");

        extend_test_paired_exact_matches(&old, &new, &mut candidates, 5)
            .expect("crossing duplicate groups fail closed");

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            (
                candidates[0].old_occurrence_index,
                candidates[0].new_occurrence_index
            ),
            (0, 0)
        );
    }

    #[test]
    fn conflicting_exact_anchors_leave_trusted_streams_unpaired() {
        let old = [
            positioned_occurrence("anchor-a", 1, 0, 0),
            positioned_occurrence("anchor-b", 2, 0, 1),
            positioned_occurrence("repeated", 3, 0, 2),
            positioned_occurrence("repeated", 4, 0, 3),
        ];
        let new = [
            positioned_occurrence("anchor-a", 101, 10, 0),
            positioned_occurrence("repeated", 102, 10, 1),
            positioned_occurrence("anchor-b", 103, 11, 0),
            positioned_occurrence("repeated", 104, 11, 1),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let mut candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 4)
            .expect("global exact matches fit");

        extend_test_paired_exact_matches(&old, &new, &mut candidates, 4)
            .expect("ambiguous streams fail closed");

        assert_eq!(candidates.len(), 2);
        assert!(
            paired_trusted_streams(&old, &new, &candidates)
                .expect("stream pairing fits")
                .is_empty()
        );
    }

    #[test]
    fn crossing_exact_anchors_leave_trusted_streams_unpaired() {
        let old = [
            positioned_occurrence("anchor-a", 1, 0, 0),
            positioned_occurrence("repeated", 2, 0, 1),
            positioned_occurrence("repeated", 3, 0, 2),
            positioned_occurrence("anchor-b", 4, 0, 3),
        ];
        let new = [
            positioned_occurrence("anchor-b", 101, 10, 0),
            positioned_occurrence("repeated", 102, 10, 1),
            positioned_occurrence("repeated", 103, 10, 2),
            positioned_occurrence("anchor-a", 104, 10, 3),
        ];
        let counts = occurrence_counts(&old, &new).expect("occurrence counts fit");
        let mut candidates = exact_match_candidates(&old, &new, &counts, &[true], 5, 4)
            .expect("global exact matches fit");

        extend_test_paired_exact_matches(&old, &new, &mut candidates, 4)
            .expect("crossing anchors fail closed");

        assert_eq!(candidates.len(), 2);
        assert!(
            paired_trusted_streams(&old, &new, &candidates)
                .expect("stream pairing fits")
                .is_empty()
        );
    }

    #[test]
    fn synthetic_evidence_uses_max_token_headroom_but_not_source_output_headroom() {
        let mut budget = RecoveryBudget::new(5, 0, 6, 1).expect("budget is valid");
        assert!(budget.charge_evidence_tokens(6));
        assert!(!budget.charge_evidence_tokens(1));
        assert!(budget.charge_outputs(1, 5));
        assert!(!budget.charge_outputs(1, 1));
    }

    #[test]
    fn location_metadata_budget_rejects_one_item_over_limit_without_allocating() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        let block_count = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS / 2;
        let consumed_count = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS - block_count;
        assert!(budget.charge_location_metadata(block_count, consumed_count));
        assert_eq!(budget.location_items, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS);
        let bytes = budget.location_bytes;

        assert!(!budget.charge_location_metadata(1, 0));
        assert_eq!(budget.location_items, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS);
        assert_eq!(budget.location_bytes, bytes);
    }

    #[test]
    fn location_metadata_exhaustion_aborts_after_an_earlier_recovery() {
        use crate::normalize::{BlockText, MappedText};

        let text = "Alpha.";
        let mapped = MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        };
        let canonical = text
            .chars()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let block = BlockText {
            block: BlockId(1),
            role: crate::layout::BlockRole::Body,
            raw: mapped.clone(),
            canonical: mapped,
            matching: text.to_owned(),
            matching_tokens: canonical.clone(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        };
        let side = Side {
            blocks: std::slice::from_ref(&block),
            index: HashMap::from([(BlockId(1), 0)]),
            canonical: vec![canonical],
            total_tokens: text.chars().count(),
        };
        let boundaries = (0..=text.chars().count()).collect::<Vec<_>>();
        let stream = Stream {
            text: text.to_owned(),
            tokens: text.chars().map(SentenceEvidenceToken::Scalar).collect(),
            scalar_to_token: boundaries.clone(),
            blocks: vec![StreamBlock {
                side_index: 0,
                scalar_range: 0..text.chars().count(),
                scalar_to_token: boundaries,
            }],
            forced_sentence_boundaries: Vec::new(),
            atomic_line: false,
            trusted: true,
        };
        let boundary = SentenceBoundary {
            byte_start: 0,
            byte_end: text.len(),
            scalar_start: 0,
            scalar_end: text.chars().count(),
        };
        let mut budget = RecoveryBudget::new(text.chars().count(), 0, text.chars().count(), 1)
            .expect("budget is valid");
        budget.location_items = MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS - 2;

        assert!(
            sentence_location(
                &side,
                &stream,
                boundary,
                0..1,
                0,
                (RecoveryUnitKind::Sentence, BlockRole::Body),
                &mut budget,
            )
            .is_some_and(|location| location.is_some())
        );
        assert_eq!(budget.location_items, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS);
        let bytes = budget.location_bytes;

        assert!(
            sentence_location(
                &side,
                &stream,
                boundary,
                0..1,
                0,
                (RecoveryUnitKind::Sentence, BlockRole::Body),
                &mut budget,
            )
            .is_none()
        );
        assert_eq!(budget.location_items, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS);
        assert_eq!(budget.location_bytes, bytes);
    }

    #[test]
    fn edge_index_prunes_impossible_pair_visits() {
        let old_occurrences = [
            SentenceOccurrence {
                key: "old-a".to_owned(),
                tokens: vec![SentenceEvidenceToken::Scalar('a')],
                word_ranges: Vec::new(),
                kind: RecoveryUnitKind::Sentence,
                role: Some(BlockRole::Body),
                location: None,
                span_index: Some(0),
                trusted_position: None,
                run_descriptor_index: None,
                page: None,
                evidence_block_index: None,
            },
            SentenceOccurrence {
                key: "old-b".to_owned(),
                tokens: vec![SentenceEvidenceToken::Scalar('b')],
                word_ranges: Vec::new(),
                kind: RecoveryUnitKind::Sentence,
                role: Some(BlockRole::Body),
                location: None,
                span_index: Some(0),
                trusted_position: None,
                run_descriptor_index: None,
                page: None,
                evidence_block_index: None,
            },
        ];
        let new_occurrences = [SentenceOccurrence {
            key: "new".to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a')],
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: None,
            span_index: None,
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        }];
        let old_candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 0,
            },
        ];
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("test budget is valid");
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &[],
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("only the edge-compatible pair is visited");

        assert_eq!(budget.pair_visits, 1);
        assert_eq!(budget.comparisons, 1);
        assert!(relations.old[0].vetoed());
        assert!(!relations.old[1].vetoed());
    }

    #[test]
    fn unit_candidate_index_excludes_cross_kind_and_cross_role_postings() {
        let occurrences = [
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Line, Some(BlockRole::Body)),
            indexed_occurrence(
                &['a'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::RepeatedFooter),
            ),
        ];
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(3, 1, 4, 1).expect("budget is valid");

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[0],
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("query succeeds");

        assert_eq!(plausible, vec![0]);
    }

    #[test]
    fn sentence_edge_signature_index_maps_paired_streams_in_both_query_directions() {
        let occurrences = [
            indexed_occurrence(
                &['a', 'x', 'x', 'x', 'x', 'x', 'x', 'x', 'x', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'q', 'q', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'x', 'x', 'x', 'x', 'x', 'x', 'x', 'x', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        let intervals = [
            Some(PairedInterval {
                pair_index: 7,
                interval_index: 0,
            }),
            Some(PairedInterval {
                pair_index: 7,
                interval_index: 1,
            }),
            Some(PairedInterval {
                pair_index: 8,
                interval_index: 0,
            }),
        ];
        let index = SentenceEdgeSignatureIndex::new(
            &occurrences,
            CandidatePostingIndexScope::PairedStream(&intervals),
        )
        .expect("paired-stream signature index construction succeeds");
        let mut plausible = Vec::new();

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[1],
                CandidatePostingBucket::PairedStream(7),
                None,
            )
            .expect("short-to-long paired-stream query succeeds");
        assert_eq!(plausible, vec![0, 1]);

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[0],
                CandidatePostingBucket::PairedStream(7),
                None,
            )
            .expect("long-to-short paired-stream query succeeds");
        assert_eq!(plausible, vec![0, 1]);
    }

    #[test]
    fn signature_replay_splits_same_known_and_ambiguous_scope_work() {
        let mut query = indexed_occurrence(
            &['a', 'b', 'c', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        query.span_index = Some(0);
        let mut candidates = [
            indexed_occurrence(
                &['a', 'b', 'x', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'b', 'y', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        candidates[0].span_index = Some(0);
        candidates[1].span_index = None;
        let index = SentenceEdgeSignatureIndex::new_bounded(
            &candidates,
            CandidatePostingIndexScope::Span,
            16,
        )
        .expect("bounded signature index builds");
        let mut shadow = SentenceEdgeSignatureShadow::new(
            12,
            Default::default(),
            SentenceEdgeSignatureFilterMode::PostUnion,
        )
        .expect("shadow budget computes");
        let actual = index.metrics().posting_items;
        shadow.metrics.index_posting_items_attempted = actual;
        shadow.metrics.index_posting_items_examined = actual;
        let mut plausible = vec![0, 1];
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        apply_signature_filter(
            Some(&mut shadow),
            &mut diagnostics,
            &mut None,
            Some(&index),
            &query,
            &candidates,
            &mut plausible,
            CandidatePostingBucket::Span(Some(0)),
            Some(CandidatePostingBucket::Span(None)),
            NearSearchScope::SameOrAmbiguousSpan,
            |_| true,
        );

        assert_eq!(shadow.metrics.pairs_considered, 2);
        assert_eq!(shadow.metrics.same_known_pairs, 1);
        assert_eq!(shadow.metrics.ambiguous_pairs, 1);
        assert_eq!(
            shadow.metrics.same_known_pairs + shadow.metrics.ambiguous_pairs,
            shadow.metrics.pairs_considered
        );
        assert_eq!(
            shadow.metrics.index_posting_items_attempted,
            shadow.metrics.index_posting_items_examined
        );
    }

    #[test]
    fn signature_replay_checkpoints_typed_query_limit_stop() {
        let query = indexed_occurrence(
            &['a', 'b', 'c', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let candidates = [indexed_occurrence(
            &['a', 'b', 'x', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let index =
            SentenceEdgeSignatureIndex::new(&candidates, CandidatePostingIndexScope::Global)
                .expect("signature index builds");
        let mut shadow = SentenceEdgeSignatureShadow::new(
            4,
            Default::default(),
            SentenceEdgeSignatureFilterMode::PostUnion,
        )
        .expect("shadow budget computes");
        shadow.query_limit = 0;
        let mut plausible = vec![0];
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        apply_signature_filter(
            Some(&mut shadow),
            &mut diagnostics,
            &mut None,
            Some(&index),
            &query,
            &candidates,
            &mut plausible,
            CandidatePostingBucket::Global,
            None,
            NearSearchScope::CrossSpan,
            |_| true,
        );

        let metrics = diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_shadow
            .expect("partial signature metrics remain");
        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit)
        );
        assert!(metrics.query_posting_visits_attempted > 0);
        assert_eq!(metrics.query_posting_visits_examined, 0);
    }

    #[test]
    fn direct_signature_mode_bypasses_broad_candidates_only_for_sentences() {
        let candidates = [
            indexed_occurrence(
                &['a', 'b', 'c', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'b', 'c', 'z'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
        ];
        let index = UnitCandidateIndex::new(&candidates, CandidatePostingIndexScope::Global)
            .expect("unit index builds");
        let mut budget = RecoveryBudget::new(8, 8, 16, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        let mut plausible = Vec::new();

        collect_unit_candidates_unless_direct(
            &index,
            &mut plausible,
            &candidates[0],
            &candidates,
            CandidatePostingBucket::Global,
            None,
            &mut budget,
            NearSearchScope::CrossSpan,
        )
        .expect("Sentence bypass succeeds");
        assert!(plausible.is_empty());
        assert_eq!(budget.sentence_work.edge_posting_visits_attempted, 0);

        collect_unit_candidates_unless_direct(
            &index,
            &mut plausible,
            &candidates[1],
            &candidates,
            CandidatePostingBucket::Global,
            None,
            &mut budget,
            NearSearchScope::CrossSpan,
        )
        .expect("Line query succeeds");
        assert_eq!(plausible, [1]);
        assert!(budget.line_work.edge_posting_visits_attempted > 0);
    }

    #[test]
    fn signature_allocation_failures_preserve_partial_work() {
        let candidates = [
            indexed_occurrence(
                &['a', 'b', 'c', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'b', 'd', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        let index_error = match SentenceEdgeSignatureIndex::new_with_allocation_failure_after(
            &candidates,
            CandidatePostingIndexScope::Global,
            1,
        ) {
            Ok(_) => panic!("injected index allocation failure must be reported"),
            Err(error) => error,
        };
        let mut index_shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::PostUnion,
        )
        .expect("shadow budget fits");
        record_signature_index_error(&mut index_shadow, index_error);
        assert_eq!(index_shadow.metrics.index_posting_items_examined, 1);
        assert!(
            index_shadow.metrics.index_posting_items_attempted
                > index_shadow.metrics.index_posting_items_examined
        );
        assert_eq!(
            index_shadow.metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::AllocationFailure)
        );

        let index =
            SentenceEdgeSignatureIndex::new(&candidates, CandidatePostingIndexScope::Global)
                .expect("signature index builds");
        let mut plausible = Vec::new();
        let query_error = index
            .collect_with_allocation_failure_after(
                &mut plausible,
                &candidates[0],
                CandidatePostingBucket::Global,
                None,
                1,
            )
            .expect_err("injected query allocation failure is reported");
        let mut query_shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::PostUnion,
        )
        .expect("shadow budget fits");
        record_signature_query_error(&mut query_shadow, query_error);
        assert!(query_shadow.metrics.query_posting_visits_examined > 0);
        assert!(
            query_shadow.metrics.query_posting_visits_attempted
                > query_shadow.metrics.query_posting_visits_examined
        );
        assert_eq!(
            query_shadow.metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::AllocationFailure)
        );
    }

    #[test]
    fn signature_replay_finalizer_preserves_early_return_and_error_checkpoints() {
        let checkpoint = SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                    index_posting_items_examined: 2,
                    index_posting_items_attempted: 3,
                    stop_reason: Some(SentenceEdgeSignatureShadowStopReason::AllocationFailure),
                    ..SentenceEdgeSignatureShadowMetrics::default()
                }),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        };

        for outcome in [
            Ok(SentenceRecoveryBuildOutcome::default()),
            Err(crate::Error::Unresolved(
                "injected replay failure".to_owned(),
            )),
        ] {
            let finalized = finalize_signature_replay_outcome(
                outcome,
                SentenceEdgeSignatureFilterMode::PostUnion,
                Some(checkpoint),
            )
            .expect("replay diagnostics are recovered");
            let metrics = finalized
                .diagnostics
                .expect("checkpoint diagnostics remain")
                .metrics
                .sentence_edge_signature_shadow
                .expect("signature metrics remain");
            assert_eq!(metrics.index_posting_items_examined, 2);
            assert_eq!(metrics.index_posting_items_attempted, 3);
            assert_eq!(
                metrics.stop_reason,
                Some(SentenceEdgeSignatureShadowStopReason::AllocationFailure)
            );
        }
    }

    #[test]
    fn stopped_signature_shadow_does_not_reactivate_in_the_next_stage() {
        let stop_reason = SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                    complete: false,
                    stop_reason: Some(stop_reason),
                    query_posting_visits_attempted: 7,
                    query_posting_visits_examined: 5,
                    ..SentenceEdgeSignatureShadowMetrics::default()
                }),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;
        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::PostUnion,
        );
        let mut relations =
            empty_modified_sentence_relations(&[], &[]).expect("empty relation buffers allocate");
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::PostUnion;

        enable_sentence_edge_signature_shadow(
            &mut relations,
            &budget,
            &mut diagnostics,
            &mut checkpoint,
        );

        let shadow = relations
            .edge_signature_shadow
            .expect("stopped shadow remains represented");
        assert!(!shadow.active);
        assert_eq!(shadow.metrics.stop_reason, Some(stop_reason));
        assert_eq!(shadow.metrics.query_posting_visits_attempted, 7);
        assert_eq!(shadow.metrics.query_posting_visits_examined, 5);

        let mut inconsistent_diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                    complete: true,
                    stop_reason: Some(stop_reason),
                    ..SentenceEdgeSignatureShadowMetrics::default()
                }),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut inconsistent_relations =
            empty_modified_sentence_relations(&[], &[]).expect("empty relation buffers allocate");
        enable_sentence_edge_signature_shadow(
            &mut inconsistent_relations,
            &budget,
            &mut inconsistent_diagnostics,
            &mut None,
        );
        assert!(
            !inconsistent_relations
                .edge_signature_shadow
                .expect("inconsistent stopped shadow remains represented")
                .active
        );
    }

    #[test]
    fn direct_signature_metrics_accumulate_across_stages_and_keep_first_stop() {
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                    complete: true,
                    ..SentenceEdgeSignatureShadowMetrics::default()
                }),
                sentence_edge_signature_direct_shadow: Some(
                    SentenceEdgeSignatureDirectShadowMetrics {
                        signature_index_items_examined: 3,
                        signature_index_items_attempted: 3,
                        signature_queries: 1,
                        signature_queries_attempted: 1,
                        ..SentenceEdgeSignatureDirectShadowMetrics::default()
                    },
                ),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;
        let mut budget = RecoveryBudget::new(8, 8, 16, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::Direct,
        );
        let mut relations =
            empty_modified_sentence_relations(&[], &[]).expect("relation buffers allocate");
        enable_sentence_edge_signature_shadow(
            &mut relations,
            &budget,
            &mut diagnostics,
            &mut checkpoint,
        );
        let shadow = relations
            .edge_signature_shadow
            .as_mut()
            .expect("direct shadow resumes");
        assert!(shadow.active);
        assert_eq!(shadow.direct_metrics.signature_index_items_examined, 3);
        shadow.direct_metrics.signature_index_items_examined = 5;
        shadow.direct_metrics.signature_index_items_attempted = 5;
        checkpoint_signature_shadow(&mut diagnostics, &mut checkpoint, shadow);

        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::Direct,
        );
        let mut next_relations =
            empty_modified_sentence_relations(&[], &[]).expect("relation buffers allocate");
        enable_sentence_edge_signature_shadow(
            &mut next_relations,
            &budget,
            &mut diagnostics,
            &mut checkpoint,
        );
        let next = next_relations
            .edge_signature_shadow
            .as_mut()
            .expect("next direct stage resumes");
        assert!(next.active);
        assert_eq!(next.direct_metrics.signature_index_items_examined, 5);
        next.stop_direct(SentenceEdgeSignatureDirectShadowStopReason::SignatureCandidateUnionLimit);
        checkpoint_signature_shadow(&mut diagnostics, &mut checkpoint, next);

        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::Direct,
        );
        let mut stopped_relations =
            empty_modified_sentence_relations(&[], &[]).expect("relation buffers allocate");
        enable_sentence_edge_signature_shadow(
            &mut stopped_relations,
            &budget,
            &mut diagnostics,
            &mut checkpoint,
        );
        let mut stopped = stopped_relations
            .edge_signature_shadow
            .expect("stopped direct shadow remains represented");
        assert!(!stopped.active);
        assert_eq!(stopped.direct_metrics.signature_index_items_examined, 5);
        assert_eq!(
            stopped.direct_metrics.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureCandidateUnionLimit)
        );
        let before = stopped.direct_metrics;
        let occurrences = [indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        assert!(
            build_signature_index(
                Some(&mut stopped),
                &occurrences,
                CandidatePostingIndexScope::Global,
            )
            .is_none()
        );
        assert_eq!(stopped.direct_metrics, before);

        checkpoint_direct_signature_setup_failure(&budget, &mut diagnostics, &mut checkpoint);
        let preserved = diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_direct_shadow
            .expect("direct metrics remain");
        assert_eq!(preserved.signature_index_items_examined, 5);
        assert_eq!(preserved.stop_reason, stopped.direct_metrics.stop_reason);
    }

    #[test]
    fn next_signature_stage_invalidates_a_paired_only_completion() {
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                    complete: true,
                    parity_evaluable: true,
                    verification_evaluable: true,
                    plan_parity: true,
                    ..SentenceEdgeSignatureShadowMetrics::default()
                }),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;

        begin_sentence_edge_signature_stage(
            &mut diagnostics,
            &mut checkpoint,
            SentenceEdgeSignatureFilterMode::PostUnion,
        );
        let stage_diagnostics = diagnostics;
        let finalized = finalize_signature_replay_outcome(
            Ok(SentenceRecoveryBuildOutcome {
                diagnostics: stage_diagnostics,
                ..SentenceRecoveryBuildOutcome::default()
            }),
            SentenceEdgeSignatureFilterMode::PostUnion,
            checkpoint,
        )
        .expect("stage allocation failure remains diagnostic-only");

        let metrics = finalized
            .diagnostics
            .expect("stage checkpoint is restored")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics remain");
        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete)
        );
        assert!(!metrics.parity_evaluable);
        assert!(!metrics.verification_evaluable);
        assert!(!metrics.plan_parity);
    }

    #[test]
    fn inactive_signature_shadow_still_applies_reverse_suppression() {
        let query = indexed_occurrence(
            &['a', 'b', 'c', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let candidates = [
            indexed_occurrence(
                &['a', 'b', 'c', 'z'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'b', 'c', 'z'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
        ];
        let mut shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::PostUnion,
        )
        .expect("shadow budget fits");
        shadow.stop(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit);
        let mut plausible = vec![0, 1];
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        apply_signature_filter(
            Some(&mut shadow),
            &mut diagnostics,
            &mut None,
            None,
            &query,
            &candidates,
            &mut plausible,
            CandidatePostingBucket::Global,
            None,
            NearSearchScope::SameOrAmbiguousSpan,
            |index| index == 1,
        );

        assert_eq!(plausible, [1]);
        assert_eq!(
            shadow.metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit)
        );
    }

    #[test]
    fn inactive_direct_signature_shadow_fails_closed() {
        let query = indexed_occurrence(
            &['a', 'b', 'c', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let candidates = [indexed_occurrence(
            &['a', 'b', 'c', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let mut shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::Direct,
        )
        .expect("shadow budget fits");
        shadow.stop(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit);
        let mut plausible = vec![0];
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        assert!(
            apply_signature_filter(
                Some(&mut shadow),
                &mut diagnostics,
                &mut None,
                None,
                &query,
                &candidates,
                &mut plausible,
                CandidatePostingBucket::Global,
                None,
                NearSearchScope::CrossSpan,
                |_| true,
            )
            .is_none()
        );
    }

    #[test]
    fn retained_pair_fingerprint_detects_equal_count_swaps_and_order_changes() {
        let mut baseline = SentenceEdgeRetainedFingerprint::default();
        baseline
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 2)
            .expect("fingerprint records");
        baseline
            .record(NearSearchScope::CrossSpan, OccurrenceSide::New, 3, 4)
            .expect("fingerprint records");

        let mut reordered = SentenceEdgeRetainedFingerprint::default();
        reordered
            .record(NearSearchScope::CrossSpan, OccurrenceSide::New, 3, 4)
            .expect("fingerprint records");
        reordered
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 2)
            .expect("fingerprint records");
        assert!(baseline.same_set(reordered));
        assert!(!baseline.same_order(reordered));

        let mut swapped = SentenceEdgeRetainedFingerprint::default();
        swapped
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 4)
            .expect("fingerprint records");
        swapped
            .record(NearSearchScope::CrossSpan, OccurrenceSide::New, 3, 2)
            .expect("fingerprint records");
        assert_eq!(baseline.count, swapped.count);
        assert!(!baseline.same_set(swapped));

        let mut metrics = SentenceEdgeSignatureDirectShadowMetrics::default();
        compare_direct_retained_fingerprints(&mut metrics, baseline, swapped);
        assert_eq!(metrics.retained_pair_count_mismatches, 0);
        assert_eq!(metrics.retained_pair_set_mismatches, 1);
        assert_eq!(metrics.retained_pair_order_mismatches, 1);
    }

    #[test]
    fn direct_scope_candidate_total_is_checked() {
        let metrics = SentenceEdgeSignatureDirectShadowMetrics {
            direct_candidates: 15,
            paired_interval_candidates: 1,
            paired_cross_interval_candidates: 2,
            same_known_candidates: 3,
            ambiguous_candidates: 4,
            cross_span_candidates: 5,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        assert_eq!(direct_scope_candidate_total(&metrics), Some(15));
    }

    #[test]
    fn direct_signature_shape_and_work_invariants_are_checked() {
        let metrics = SentenceEdgeSignatureDirectShadowMetrics {
            signature_index_items_examined: 7,
            signature_index_items_attempted: 7,
            signature_index_own_posting_items: 3,
            signature_index_all_posting_items: 4,
            signature_index_own_distinct_keys: 2,
            signature_index_all_distinct_keys: 3,
            signature_index_distinct_keys_examined: 5,
            signature_index_distinct_keys_attempted: 5,
            signature_index_posting_capacity_items: 8,
            signature_index_estimated_logical_bytes: 1_024,
            signature_index_estimated_logical_bytes_examined: 1_024,
            signature_index_estimated_logical_bytes_attempted: 1_024,
            signature_index_depth_1_posting_items: 2,
            signature_index_depth_2_to_3_posting_items: 3,
            signature_index_depth_4_plus_posting_items: 2,
            signature_queries: 3,
            signature_queries_attempted: 3,
            signature_depth_1_queries: 1,
            signature_depth_2_to_3_queries: 1,
            signature_depth_4_plus_queries: 1,
            direct_candidates: 6,
            signature_candidate_union_attempted: 6,
            signature_depth_1_candidate_union: 1,
            signature_depth_2_to_3_candidate_union: 2,
            signature_depth_4_plus_candidate_union: 3,
            exact_edge_rechecks: 6,
            exact_edge_rechecks_attempted: 6,
            exact_edge_retained_pairs: 4,
            exact_edge_rejected_pairs: 2,
            cross_orientation_only_candidates: 1,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        assert!(direct_signature_metrics_are_consistent(&metrics));

        let mut inconsistent = metrics;
        inconsistent.signature_depth_4_plus_candidate_union = 4;
        assert!(!direct_signature_metrics_are_consistent(&inconsistent));
    }

    #[test]
    fn direct_signature_index_shape_aggregates_with_checked_totals() {
        let source = SentenceEdgeSignatureIndexMetrics {
            posting_items: 7,
            own_distinct_keys: 2,
            all_distinct_keys: 3,
            own_key_capacity: 4,
            all_key_capacity: 5,
            own_posting_items: 3,
            all_posting_items: 4,
            posting_capacity_items: 9,
            largest_posting: 6,
            depth_1_posting_items: 2,
            depth_2_to_3_posting_items: 3,
            depth_4_plus_posting_items: 2,
            estimated_logical_bytes: 1_024,
        };
        let mut aggregate = SentenceEdgeSignatureDirectShadowMetrics::default();
        add_direct_signature_index_metrics(&mut aggregate, source).expect("first index fits");
        add_direct_signature_index_metrics(&mut aggregate, source).expect("second index fits");

        assert_eq!(aggregate.signature_index_items_examined, 14);
        assert_eq!(aggregate.signature_index_own_distinct_keys, 4);
        assert_eq!(aggregate.signature_index_all_distinct_keys, 6);
        assert_eq!(aggregate.signature_index_distinct_keys_examined, 10);
        assert_eq!(aggregate.signature_index_distinct_keys_attempted, 10);
        assert_eq!(aggregate.signature_index_posting_capacity_items, 18);
        assert_eq!(aggregate.signature_index_estimated_logical_bytes, 2_048);
        assert_eq!(
            aggregate.signature_index_estimated_logical_bytes_attempted,
            2_048
        );
        assert_eq!(aggregate.signature_index_largest_posting, 6);
    }

    #[test]
    fn direct_signature_query_metrics_split_depth_and_cross_orientation() {
        let query = indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let candidates = [
            indexed_occurrence(
                &['a', 'x', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['c', 'x', 'a'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['q', 'x', 'r'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        assert!(
            !is_cross_orientation_only(&query, &candidates[0])
                .expect("same-orientation evidence is measurable")
        );
        assert!(
            is_cross_orientation_only(&query, &candidates[1])
                .expect("cross-orientation evidence is measurable")
        );
        // A hash-collision over-inclusion has no exact oriented edge and must
        // not be attributed to the shared prefix/suffix namespace.
        assert!(
            !is_cross_orientation_only(&query, &candidates[2])
                .expect("collision over-inclusion is measurable")
        );
        let both = indexed_occurrence(
            &['a', 'x', 'a'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let symmetric_query = indexed_occurrence(
            &['a', 'b', 'a'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        assert!(
            !is_cross_orientation_only(&symmetric_query, &both)
                .expect("mixed orientation evidence is measurable")
        );
        let mut shadow = SentenceEdgeSignatureShadow::new(
            16,
            Default::default(),
            SentenceEdgeSignatureFilterMode::Direct,
        )
        .expect("shadow budget fits");

        record_direct_signature_query_metrics(
            &mut shadow,
            SentenceEdgeSignatureQueryMetrics {
                posting_visits: 5,
                candidate_union: 3,
                depth_band: Some(SentenceEdgeSignatureDepthBand::One),
            },
            3,
            &[0, 1, 2],
        )
        .expect("metrics fit");

        assert_eq!(shadow.direct_metrics.signature_query_visits_examined, 5);
        assert_eq!(shadow.direct_metrics.signature_depth_1_queries, 1);
        assert_eq!(shadow.direct_metrics.signature_depth_1_candidate_union, 3);
        let mut budget = RecoveryBudget::new(9, 3, 12, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        let edge_filter = classify_sentence_edge_filter_query(
            &query,
            &candidates,
            &[0, 1, 2],
            &mut budget,
            |_| true,
        );
        assert!(matches!(
            edge_filter,
            SentenceEdgeFilterQuery::Filtered { rejected: None, .. }
        ));
        assert_eq!(
            edge_filter.cross_orientation_only_count(&query, &candidates),
            Some(1)
        );
        assert_eq!(budget.signature_exact_rechecks, 3);
        assert_eq!(budget.sentence_edge_filter_pairs_retained, 1);
        assert_eq!(budget.sentence_edge_filter_pairs_rejected, 2);
    }

    #[test]
    fn direct_signature_build_stops_preserve_typed_resource_reasons() {
        let cases = [
            (
                SentenceEdgeSignatureIndexBuildError::PostingLimit {
                    examined: 2,
                    attempted: 3,
                },
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexPostingLimit,
            ),
            (
                SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
                    examined: 2,
                    attempted: 3,
                    progress: SentenceEdgeSignatureIndexMetrics::default(),
                },
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexDistinctKeyLimit,
            ),
            (
                SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
                    examined: 2,
                    attempted: 3,
                    progress: SentenceEdgeSignatureIndexMetrics::default(),
                },
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexEstimatedByteLimit,
            ),
            (
                SentenceEdgeSignatureIndexBuildError::AllocationFailure {
                    examined: 2,
                    attempted: 3,
                    progress: SentenceEdgeSignatureIndexMetrics::default(),
                },
                SentenceEdgeSignatureDirectShadowStopReason::AllocationFailure,
            ),
            (
                SentenceEdgeSignatureIndexBuildError::Index(
                    SentenceEdgeSignatureIndexError::InvalidScope {
                        examined: 0,
                        attempted: 0,
                    },
                ),
                SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure,
            ),
        ];
        for (case_index, (error, expected)) in cases.into_iter().enumerate() {
            let mut shadow = SentenceEdgeSignatureShadow::new(
                8,
                Default::default(),
                SentenceEdgeSignatureFilterMode::Direct,
            )
            .expect("shadow budget fits");
            record_direct_signature_build_error(&mut shadow, error);
            assert_eq!(shadow.direct_metrics.stop_reason, Some(expected));
            assert_eq!(
                shadow.direct_metrics.signature_index_items_examined,
                if case_index == 0 { 2 } else { 0 }
            );
            assert_eq!(
                shadow.direct_metrics.signature_index_items_attempted,
                if case_index == 0 { 3 } else { 0 }
            );
        }

        let occurrences = [indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        for (configure, expected) in [
            (
                0usize,
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexPostingLimit,
            ),
            (
                1usize,
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexDistinctKeyLimit,
            ),
            (
                2usize,
                SentenceEdgeSignatureDirectShadowStopReason::SignatureIndexEstimatedByteLimit,
            ),
        ] {
            let mut shadow = SentenceEdgeSignatureShadow::new(
                8,
                Default::default(),
                SentenceEdgeSignatureFilterMode::Direct,
            )
            .expect("shadow budget fits");
            match configure {
                0 => shadow.posting_limit = 0,
                1 => shadow.distinct_key_limit = 1,
                2 => {
                    shadow.estimated_byte_limit =
                        std::mem::size_of::<SentenceEdgeSignatureIndex>() + 1;
                }
                _ => unreachable!(),
            }
            assert!(
                build_signature_index(
                    Some(&mut shadow),
                    &occurrences,
                    CandidatePostingIndexScope::Global,
                )
                .is_none()
            );
            assert_eq!(shadow.direct_metrics.stop_reason, Some(expected));
            if configure == 1 {
                assert_eq!(
                    shadow.direct_metrics.signature_index_distinct_keys_examined,
                    1
                );
                assert_eq!(
                    shadow
                        .direct_metrics
                        .signature_index_distinct_keys_attempted,
                    2
                );
            } else if configure == 2 {
                assert!(
                    shadow
                        .direct_metrics
                        .signature_index_estimated_logical_bytes_examined
                        > 0
                );
                assert!(
                    shadow
                        .direct_metrics
                        .signature_index_estimated_logical_bytes_attempted
                        > shadow
                            .direct_metrics
                            .signature_index_estimated_logical_bytes_examined
                );
            }
        }
    }

    #[test]
    fn signature_index_build_limits_use_the_public_build_path() {
        let occurrences = [
            indexed_occurrence(
                &['a', 'b', 'c', 'd', 'e', 'f', 'g'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &[
                    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p',
                    'q', 'r', 's', 't', 'u',
                ],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        let unlimited = SentenceEdgeSignatureIndexBuildLimits {
            posting_items: usize::MAX,
            distinct_keys: usize::MAX,
            estimated_logical_bytes: usize::MAX,
        };
        let baseline = SentenceEdgeSignatureIndex::new_with_limits(
            &occurrences,
            CandidatePostingIndexScope::Global,
            unlimited,
        )
        .expect("baseline signature index fits");
        let metrics = baseline.metrics();
        let distinct_keys = metrics.own_distinct_keys + metrics.all_distinct_keys;
        let probe = match SentenceEdgeSignatureIndex::new_with_limits(
            &occurrences,
            CandidatePostingIndexScope::Global,
            SentenceEdgeSignatureIndexBuildLimits {
                posting_items: metrics.posting_items,
                distinct_keys,
                estimated_logical_bytes: metrics.estimated_logical_bytes,
            },
        ) {
            Ok(_) => panic!("completed bytes exclude the transition peak"),
            Err(error) => error,
        };
        let SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            attempted: transition_peak,
            ..
        } = probe
        else {
            panic!("expected transition peak limit");
        };
        let exact = SentenceEdgeSignatureIndexBuildLimits {
            posting_items: metrics.posting_items,
            distinct_keys,
            estimated_logical_bytes: transition_peak,
        };
        let exact_index = SentenceEdgeSignatureIndex::new_with_limits(
            &occurrences,
            CandidatePostingIndexScope::Global,
            exact,
        )
        .expect("exact limits admit the index");
        assert_eq!(exact_index.metrics(), metrics);

        assert!(matches!(
            SentenceEdgeSignatureIndex::new_with_limits(
                &occurrences,
                CandidatePostingIndexScope::Global,
                SentenceEdgeSignatureIndexBuildLimits {
                    posting_items: metrics.posting_items - 1,
                    ..exact
                },
            ),
            Err(SentenceEdgeSignatureIndexBuildError::PostingLimit {
                examined: 0,
                attempted,
            }) if attempted == metrics.posting_items
        ));
        assert!(matches!(
            SentenceEdgeSignatureIndex::new_with_limits(
                &occurrences,
                CandidatePostingIndexScope::Global,
                SentenceEdgeSignatureIndexBuildLimits {
                    distinct_keys: distinct_keys - 1,
                    estimated_logical_bytes: usize::MAX,
                    ..exact
                },
            ),
            Err(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
                examined,
                attempted,
                progress,
            }) if examined == distinct_keys - 1
                && attempted == distinct_keys
                && progress.own_distinct_keys + progress.all_distinct_keys == distinct_keys
        ));
        assert!(matches!(
            SentenceEdgeSignatureIndex::new_with_limits(
                &occurrences,
                CandidatePostingIndexScope::Global,
                SentenceEdgeSignatureIndexBuildLimits {
                    estimated_logical_bytes: transition_peak - 1,
                    ..exact
                },
            ),
            Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
                examined,
                attempted,
                progress,
            }) if examined == progress.estimated_logical_bytes
                && attempted == transition_peak
                && examined < attempted
        ));
    }

    #[test]
    fn direct_signature_exact_recheck_budget_fails_closed() {
        let mut budget = RecoveryBudget::new(1, 1, 2, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        budget.signature_exact_recheck_limit = 0;
        assert!(!budget.charge_sentence_edge_filter_pair());
        assert_eq!(budget.signature_exact_rechecks, 0);
        assert_eq!(budget.signature_exact_rechecks_attempted, 1);
        assert_eq!(
            budget.signature_exact_recheck_stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureExactEdgeRecheckLimit)
        );

        let mut budget = RecoveryBudget::new(1, 1, 2, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        budget.signature_exact_recheck_comparison_limit = 0;
        assert!(!budget.charge_sentence_edge_filter_comparison());
        assert_eq!(budget.signature_exact_recheck_comparisons, 0);
        assert_eq!(budget.signature_exact_recheck_comparisons_attempted, 1);
    }

    #[test]
    fn direct_signature_query_and_candidate_limits_fail_closed() {
        let query = indexed_occurrence(
            &['a', 'b', 'c'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let candidates = [indexed_occurrence(
            &['a', 'x', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let index =
            SentenceEdgeSignatureIndex::new(&candidates, CandidatePostingIndexScope::Global)
                .expect("index builds");
        let mut shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::Direct,
        )
        .expect("shadow budget fits");
        shadow.query_count_limit = 1;
        let mut plausible = Vec::new();
        assert!(
            apply_signature_filter(
                Some(&mut shadow),
                &mut None,
                &mut None,
                Some(&index),
                &query,
                &candidates,
                &mut plausible,
                CandidatePostingBucket::Global,
                None,
                NearSearchScope::CrossSpan,
                |_| true,
            )
            .is_some()
        );
        assert!(
            apply_signature_filter(
                Some(&mut shadow),
                &mut None,
                &mut None,
                Some(&index),
                &query,
                &candidates,
                &mut plausible,
                CandidatePostingBucket::Global,
                None,
                NearSearchScope::CrossSpan,
                |_| true,
            )
            .is_none()
        );
        assert_eq!(
            shadow.direct_metrics.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryCountLimit)
        );
        assert_eq!(shadow.direct_metrics.signature_queries, 1);
        assert_eq!(shadow.direct_metrics.signature_queries_attempted, 2);

        let mut shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::Direct,
        )
        .expect("shadow budget fits");
        shadow.query_limit = 0;
        assert!(
            apply_signature_filter(
                Some(&mut shadow),
                &mut None,
                &mut None,
                Some(&index),
                &query,
                &candidates,
                &mut plausible,
                CandidatePostingBucket::Global,
                None,
                NearSearchScope::CrossSpan,
                |_| true,
            )
            .is_none()
        );
        assert_eq!(
            shadow.direct_metrics.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryPostingVisitLimit)
        );
        assert!(shadow.direct_metrics.signature_query_visits_attempted > 0);
        assert_eq!(shadow.direct_metrics.signature_query_visits_examined, 0);

        let mut shadow = SentenceEdgeSignatureShadow::new(
            8,
            Default::default(),
            SentenceEdgeSignatureFilterMode::Direct,
        )
        .expect("shadow budget fits");
        shadow.candidate_union_limit = 1;
        record_direct_signature_query_metrics(
            &mut shadow,
            SentenceEdgeSignatureQueryMetrics {
                posting_visits: 1,
                candidate_union: 1,
                depth_band: Some(SentenceEdgeSignatureDepthBand::One),
            },
            1,
            &[0],
        )
        .expect("first candidate union fits");
        assert!(
            record_direct_signature_query_metrics(
                &mut shadow,
                SentenceEdgeSignatureQueryMetrics {
                    posting_visits: 1,
                    candidate_union: 1,
                    depth_band: Some(SentenceEdgeSignatureDepthBand::One),
                },
                1,
                &[0],
            )
            .is_none()
        );
        assert_eq!(
            shadow.direct_metrics.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureCandidateUnionLimit)
        );
        assert_eq!(shadow.direct_metrics.direct_candidates, 1);
        assert_eq!(shadow.direct_metrics.signature_candidate_union_attempted, 2);
    }

    #[test]
    fn direct_stop_priority_keeps_edge_filter_before_downstream_near() {
        let signature = SentenceEdgeSignatureShadowMetrics {
            stop_reason: Some(SentenceEdgeSignatureShadowStopReason::PairVisitLimit),
            ..SentenceEdgeSignatureShadowMetrics::default()
        };
        let recovery = SentenceRecoveryMetrics {
            sentence_edge_filter_stop_reason: Some(SentenceEdgeFilterStopReason::PairVisitLimit),
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            ..SentenceRecoveryMetrics::default()
        };
        assert_eq!(
            select_direct_signature_stop_reason(signature, recovery),
            Some(SentenceEdgeSignatureDirectShadowStopReason::DirectEdgePairVisitLimit)
        );

        let signature = SentenceEdgeSignatureShadowMetrics {
            stop_reason: Some(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit),
            ..signature
        };
        assert_eq!(
            select_direct_signature_stop_reason(signature, recovery),
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureQueryPostingVisitLimit)
        );
    }

    #[test]
    fn standard_and_reference_budget_limits_are_exact() {
        let standard = RecoveryBudget::new(3, 5, 8, 1).expect("standard budget fits");
        assert_eq!(standard.pair_visit_limit, 8);
        assert_eq!(standard.comparison_limit, 32);
        assert_eq!(standard.candidate_posting_visit_limit, 32);

        let limits = ReferenceBudgetLimits::derive(3, 5).expect("reference limits fit");
        assert_eq!(limits.pair_visits, 64);
        assert_eq!(limits.comparisons, 256);
        assert_eq!(limits.candidate_posting_visits, 256);
        let capped = ReferenceBudgetLimits::derive(1_000_000, 1_000_000)
            .expect("capped reference limits fit");
        assert_eq!(capped.pair_visits, REFERENCE_PAIR_WORK_CAP);
        assert_eq!(capped.comparisons, REFERENCE_COMPARISON_WORK_CAP);
        assert_eq!(
            capped.candidate_posting_visits,
            REFERENCE_COMPARISON_WORK_CAP
        );
        assert!(ReferenceBudgetLimits::derive(usize::MAX, 1).is_none());
        assert!(ReferenceBudgetLimits::derive(usize::MAX / 16, 0).is_none());
    }

    #[test]
    fn fragment_veto_test_limits_clear_when_the_scoped_operation_panics() {
        let panic = std::panic::catch_unwind(|| {
            with_next_fragment_veto_test_limits(
                FragmentVetoTestLimits {
                    pair_visits: 0,
                    comparisons: 0,
                },
                || panic!("expected test panic"),
            );
        });
        assert!(panic.is_err());

        let budget = FragmentVetoBudget::new(3, 5).expect("default budget remains available");
        assert_eq!(budget.pair_visit_limit, 8);
        assert_eq!(budget.comparison_limit, 32);
    }

    fn reference_test_outcome(
        plan: SentenceRecoveryPlan,
        fingerprint: SentenceEdgeRetainedFingerprint,
    ) -> SentenceRecoveryBuildOutcome {
        SentenceRecoveryBuildOutcome {
            plan: Some(plan),
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics {
                    near_relation_complete: true,
                    sentence_edge_filter_complete: true,
                    sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                        complete: true,
                        ..SentenceEdgeSignatureShadowMetrics::default()
                    }),
                    ..SentenceRecoveryMetrics::default()
                },
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: fingerprint,
                signature_retained_fingerprint_valid: true,
            }),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        }
    }

    #[test]
    fn reference_oracle_detects_plan_and_fingerprint_differences() {
        let empty = SentenceEdgeRetainedFingerprint::default();
        let direct = reference_test_outcome(SentenceRecoveryPlan::default(), empty);
        let same = reference_test_outcome(SentenceRecoveryPlan::default(), empty);
        let metrics = evaluate_reference_oracle(&direct, &same);
        assert!(metrics.complete);
        assert!(metrics.plan_parity_evaluable);
        assert!(metrics.plan_parity);
        assert!(metrics.fingerprint_evaluable);
        assert_eq!(metrics.retained_pair_count_mismatches, 0);
        assert_eq!(metrics.retained_pair_set_mismatches, 0);
        assert_eq!(metrics.retained_pair_order_mismatches, 0);

        let different_plan = SentenceRecoveryPlan {
            cross_span_replacement_new_spans: vec![1],
            ..SentenceRecoveryPlan::default()
        };
        let reference = reference_test_outcome(different_plan, empty);
        let metrics = evaluate_reference_oracle(&direct, &reference);
        assert!(metrics.complete);
        assert!(!metrics.plan_parity);

        let mut one = empty;
        one.record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 2)
            .expect("fingerprint records");
        let reference = reference_test_outcome(SentenceRecoveryPlan::default(), one);
        let metrics = evaluate_reference_oracle(&direct, &reference);
        assert!(metrics.complete);
        assert_eq!(metrics.retained_pair_misses, 1);
        assert_eq!(metrics.retained_pair_count_mismatches, 1);
        assert_eq!(metrics.retained_pair_set_mismatches, 1);
        assert_eq!(metrics.retained_pair_order_mismatches, 1);

        let mut other = empty;
        other
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 3, 4)
            .expect("fingerprint records");
        let direct_one = reference_test_outcome(SentenceRecoveryPlan::default(), one);
        let reference_other = reference_test_outcome(SentenceRecoveryPlan::default(), other);
        let metrics = evaluate_reference_oracle(&direct_one, &reference_other);
        assert_eq!(metrics.retained_pair_count_mismatches, 0);
        assert_eq!(metrics.retained_pair_set_mismatches, 1);
        assert_eq!(metrics.retained_pair_order_mismatches, 1);

        let mut direct_order = empty;
        direct_order
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 2)
            .and_then(|_| {
                direct_order.record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 3, 4)
            })
            .expect("fingerprints record");
        let mut reference_order = empty;
        reference_order
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 3, 4)
            .and_then(|_| {
                reference_order.record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 2)
            })
            .expect("fingerprints record");
        let direct = reference_test_outcome(SentenceRecoveryPlan::default(), direct_order);
        let reference = reference_test_outcome(SentenceRecoveryPlan::default(), reference_order);
        let metrics = evaluate_reference_oracle(&direct, &reference);
        assert_eq!(metrics.retained_pair_count_mismatches, 0);
        assert_eq!(metrics.retained_pair_set_mismatches, 0);
        assert_eq!(metrics.retained_pair_order_mismatches, 1);
    }

    #[test]
    fn reference_oracle_stops_discard_partial_parity() {
        for (signature_stop, expected) in [
            (
                SentenceEdgeSignatureShadowStopReason::IndexPostingLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::DiagnosticFailure,
            ),
            (
                SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::DiagnosticFailure,
            ),
        ] {
            let direct = reference_test_outcome(
                SentenceRecoveryPlan::default(),
                SentenceEdgeRetainedFingerprint::default(),
            );
            let mut reference = reference_test_outcome(
                SentenceRecoveryPlan::default(),
                SentenceEdgeRetainedFingerprint::default(),
            );
            let diagnostics = reference.diagnostics.as_mut().expect("diagnostics exist");
            diagnostics.metrics.sentence_edge_signature_shadow =
                Some(SentenceEdgeSignatureShadowMetrics {
                    complete: false,
                    stop_reason: Some(signature_stop),
                    ..SentenceEdgeSignatureShadowMetrics::default()
                });
            diagnostics.metrics.near_candidate_posting_visits_examined = 3;
            diagnostics.metrics.near_candidate_posting_visits_attempted = 4;
            let metrics = evaluate_reference_oracle(&direct, &reference);
            assert!(!metrics.complete);
            assert_eq!(metrics.stop_reason, Some(expected));
            assert_eq!(metrics.candidate_posting_visits_examined, 3);
            assert_eq!(metrics.candidate_posting_visits_attempted, 4);
            assert!(!metrics.plan_parity_evaluable);
            assert!(!metrics.fingerprint_evaluable);
        }

        let direct = reference_test_outcome(
            SentenceRecoveryPlan::default(),
            SentenceEdgeRetainedFingerprint::default(),
        );
        for (edge_stop, expected) in [
            (
                SentenceEdgeFilterStopReason::PairVisitLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::EdgeFilterPairVisitLimit,
            ),
            (
                SentenceEdgeFilterStopReason::SimilarityComparisonLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::EdgeFilterSimilarityComparisonLimit,
            ),
            (
                SentenceEdgeFilterStopReason::AllocationFailure,
                SentenceEdgeSignatureReferenceOracleStopReason::AllocationFailure,
            ),
            (
                SentenceEdgeFilterStopReason::CounterOverflow,
                SentenceEdgeSignatureReferenceOracleStopReason::CounterOverflow,
            ),
        ] {
            let mut reference = reference_test_outcome(
                SentenceRecoveryPlan::default(),
                SentenceEdgeRetainedFingerprint::default(),
            );
            let diagnostics = reference.diagnostics.as_mut().expect("diagnostics exist");
            diagnostics.metrics.sentence_edge_filter_complete = false;
            diagnostics.metrics.sentence_edge_filter_stop_reason = Some(edge_stop);
            diagnostics.metrics.near_relation_complete = false;
            diagnostics.metrics.near_relation_stop_reason =
                Some(NearRelationStopReason::CandidateCountLimit);
            let metrics = evaluate_reference_oracle(&direct, &reference);
            assert!(!metrics.complete);
            assert_eq!(metrics.stop_reason, Some(expected));
            assert!(!metrics.plan_parity_evaluable);
            assert!(!metrics.fingerprint_evaluable);
        }

        for (near_stop, expected) in [
            (
                NearRelationStopReason::CandidatePostingVisitLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::CandidatePostingVisitLimit,
            ),
            (
                NearRelationStopReason::PairVisitLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::PairVisitLimit,
            ),
            (
                NearRelationStopReason::SimilarityComparisonLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::SimilarityComparisonLimit,
            ),
            (
                NearRelationStopReason::CandidateCountLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::CandidateCountLimit,
            ),
        ] {
            let mut reference = reference_test_outcome(
                SentenceRecoveryPlan::default(),
                SentenceEdgeRetainedFingerprint::default(),
            );
            let diagnostics = reference.diagnostics.as_mut().expect("diagnostics exist");
            diagnostics.metrics.near_relation_complete = false;
            diagnostics.metrics.near_relation_stop_reason = Some(near_stop);
            let metrics = evaluate_reference_oracle(&direct, &reference);
            assert!(!metrics.complete);
            assert_eq!(metrics.stop_reason, Some(expected));
            assert!(!metrics.plan_parity_evaluable);
        }

        for (fragment_stop, expected) in [
            (
                FragmentVetoStopReason::PairVisitLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::FragmentVetoPairVisitLimit,
            ),
            (
                FragmentVetoStopReason::SimilarityComparisonLimit,
                SentenceEdgeSignatureReferenceOracleStopReason::FragmentVetoSimilarityComparisonLimit,
            ),
            (
                FragmentVetoStopReason::InvalidEvidence,
                SentenceEdgeSignatureReferenceOracleStopReason::FragmentVetoIncomplete,
            ),
        ] {
            let mut reference = reference_test_outcome(
                SentenceRecoveryPlan::default(),
                SentenceEdgeRetainedFingerprint::default(),
            );
            reference.fragment_veto_complete = false;
            reference.fragment_veto_stop_reason = Some(fragment_stop);
            match fragment_stop {
                FragmentVetoStopReason::PairVisitLimit => {
                    reference.fragment_veto_pair_visits_attempted = 1;
                }
                FragmentVetoStopReason::SimilarityComparisonLimit => {
                    reference.fragment_veto_comparisons_attempted = 1;
                }
                FragmentVetoStopReason::InvalidEvidence => {}
                FragmentVetoStopReason::AllocationFailure
                | FragmentVetoStopReason::CounterOverflow => unreachable!(),
            }
            let metrics = evaluate_reference_oracle(&direct, &reference);
            assert!(!metrics.complete);
            assert_eq!(metrics.stop_reason, Some(expected));
            assert!(!metrics.plan_parity_evaluable);
            assert!(!metrics.fingerprint_evaluable);
            assert_eq!(
                metrics.fragment_veto_pair_visits_attempted
                    - metrics.fragment_veto_pair_visits_examined,
                usize::from(fragment_stop == FragmentVetoStopReason::PairVisitLimit)
            );
            assert_eq!(
                metrics.fragment_veto_similarity_comparisons_attempted
                    - metrics.fragment_veto_similarity_comparisons_examined,
                usize::from(
                    fragment_stop == FragmentVetoStopReason::SimilarityComparisonLimit
                )
            );
        }
    }

    #[test]
    fn reference_oracle_rejects_incomplete_sentence_edge_observation() {
        let direct = reference_test_outcome(
            SentenceRecoveryPlan::default(),
            SentenceEdgeRetainedFingerprint::default(),
        );
        let mut incomplete = SentenceEdgeRetainedFingerprint::default();
        incomplete
            .record_reference_attempt()
            .expect("attempt counter fits");
        let reference = reference_test_outcome(SentenceRecoveryPlan::default(), incomplete);

        let metrics = evaluate_reference_oracle(&direct, &reference);

        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureReferenceOracleStopReason::ProductionTraversalIncomplete)
        );
        assert_eq!(metrics.legacy_sentence_edge_pairs_attempted, 1);
        assert_eq!(metrics.legacy_sentence_edge_pairs_examined, 0);
        assert!(!metrics.plan_parity_evaluable);
        assert!(!metrics.fingerprint_evaluable);
    }

    #[test]
    fn reference_oracle_is_absent_for_complete_accepted_outcome() {
        let mut accepted = SentenceRecoveryBuildOutcome {
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics {
                    near_relation_complete: true,
                    sentence_edge_filter_complete: true,
                    ..SentenceRecoveryMetrics::default()
                },
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            }),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        record_reference_oracle_direct_incomplete(&mut accepted);
        assert!(
            accepted
                .diagnostics
                .expect("diagnostics remain")
                .metrics
                .sentence_edge_signature_reference_oracle
                .is_none()
        );
    }

    #[test]
    fn reference_oracle_direct_stop_and_attachment_preserve_accepted_state() {
        let mut accepted = SentenceRecoveryBuildOutcome {
            plan: Some(SentenceRecoveryPlan::default()),
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics {
                    near_pair_candidates: 17,
                    ..SentenceRecoveryMetrics::default()
                },
                eligible_old_source_tokens: 3,
                eligible_new_source_tokens: 5,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            }),
            watch_diagnostics: Some(RecoveryWatchDiagnostics {
                complete: true,
                ..RecoveryWatchDiagnostics::default()
            }),
            ..SentenceRecoveryBuildOutcome::default()
        };
        record_reference_oracle_direct_incomplete(&mut accepted);
        let diagnostics = accepted.diagnostics.as_ref().expect("diagnostics remain");
        let oracle = diagnostics
            .metrics
            .sentence_edge_signature_reference_oracle
            .expect("typed direct stop is attached");
        assert_eq!(
            oracle.stop_reason,
            Some(SentenceEdgeSignatureReferenceOracleStopReason::DirectReplayIncomplete)
        );
        assert_eq!(diagnostics.metrics.near_pair_candidates, 17);
        assert_eq!(diagnostics.eligible_old_source_tokens, 3);
        assert_eq!(diagnostics.eligible_new_source_tokens, 5);
        assert!(accepted.plan == Some(SentenceRecoveryPlan::default()));
        assert!(
            accepted
                .watch_diagnostics
                .as_ref()
                .is_some_and(|watch| watch.complete)
        );

        attach_reference_oracle_metrics(
            &mut accepted,
            SentenceEdgeSignatureReferenceOracleMetrics {
                complete: true,
                direct_complete: true,
                ..SentenceEdgeSignatureReferenceOracleMetrics::default()
            },
        );
        let diagnostics = accepted.diagnostics.expect("diagnostics remain");
        assert_eq!(diagnostics.metrics.near_pair_candidates, 17);
        assert!(accepted.plan == Some(SentenceRecoveryPlan::default()));
        assert!(
            accepted
                .watch_diagnostics
                .is_some_and(|watch| watch.complete)
        );
    }

    #[test]
    fn direct_setup_failure_checkpoints_diagnostic_stop() {
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget fits");
        budget.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut checkpoint = None;

        checkpoint_direct_signature_setup_failure(&budget, &mut diagnostics, &mut checkpoint);

        let metrics = checkpoint
            .expect("failure checkpoint exists")
            .metrics
            .sentence_edge_signature_shadow
            .expect("typed signature stop exists");
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure)
        );
    }

    #[test]
    fn stopped_post_union_fingerprint_does_not_invalidate_complete_direct_replay() {
        let mut accepted = SentenceRecoveryBuildOutcome {
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics {
                    near_relation_complete: true,
                    sentence_edge_filter_complete: true,
                    sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                        complete: false,
                        stop_reason: Some(
                            SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit,
                        ),
                        ..SentenceEdgeSignatureShadowMetrics::default()
                    }),
                    ..SentenceRecoveryMetrics::default()
                },
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            }),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        let replay = SentenceRecoveryBuildOutcome {
            diagnostics: Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics {
                    near_relation_complete: true,
                    sentence_edge_filter_complete: true,
                    sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                        complete: true,
                        ..SentenceEdgeSignatureShadowMetrics::default()
                    }),
                    ..SentenceRecoveryMetrics::default()
                },
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            }),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };

        record_sentence_edge_signature_direct_replay(&mut accepted, &replay);

        let metrics = accepted
            .diagnostics
            .expect("accepted diagnostics remain")
            .metrics
            .sentence_edge_signature_direct_shadow
            .expect("direct metrics exist");
        assert!(metrics.complete);
        assert!(metrics.parity_evaluable);
        assert!(metrics.plan_parity);
        assert!(!metrics.verification_evaluable);
        assert_eq!(metrics.retained_pair_count_mismatches, 0);
        assert_eq!(metrics.retained_pair_set_mismatches, 0);
        assert_eq!(metrics.retained_pair_order_mismatches, 0);
        assert_eq!(metrics.stop_reason, None);
    }

    #[test]
    fn incomplete_fragment_veto_makes_direct_replay_unevaluable() {
        let diagnostics = SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                near_relation_complete: true,
                sentence_edge_filter_complete: true,
                sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                    complete: true,
                    ..SentenceEdgeSignatureShadowMetrics::default()
                }),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: true,
        };
        let mut accepted = SentenceRecoveryBuildOutcome {
            plan: Some(SentenceRecoveryPlan::default()),
            diagnostics: Some(diagnostics),
            ..SentenceRecoveryBuildOutcome::default()
        };
        let replay = SentenceRecoveryBuildOutcome {
            plan: Some(SentenceRecoveryPlan::default()),
            diagnostics: Some(diagnostics),
            fragment_veto_stop_reason: Some(FragmentVetoStopReason::PairVisitLimit),
            fragment_veto_pair_visits_attempted: 1,
            ..SentenceRecoveryBuildOutcome::default()
        };

        record_sentence_edge_signature_direct_replay(&mut accepted, &replay);

        let metrics = accepted
            .diagnostics
            .expect("accepted diagnostics remain")
            .metrics
            .sentence_edge_signature_direct_shadow
            .expect("direct metrics exist");
        assert!(!metrics.complete);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::FragmentVetoPairVisitLimit)
        );
        assert!(!metrics.parity_evaluable);
        assert!(!metrics.verification_evaluable);
        assert_eq!(metrics.fragment_veto_pair_visits_examined, 0);
        assert_eq!(metrics.fragment_veto_pair_visits_attempted, 1);
    }

    #[test]
    fn evaluable_direct_parity_and_fingerprint_mismatches_fail_closed() {
        let complete_metrics = SentenceRecoveryMetrics {
            near_relation_complete: true,
            sentence_edge_filter_complete: true,
            sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                complete: true,
                ..SentenceEdgeSignatureShadowMetrics::default()
            }),
            sentence_edge_signature_direct_shadow: Some(
                SentenceEdgeSignatureDirectShadowMetrics::default(),
            ),
            ..SentenceRecoveryMetrics::default()
        };
        let diagnostics = |valid, fingerprint| SentenceRecoveryDiagnostics {
            metrics: complete_metrics,
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: fingerprint,
            signature_retained_fingerprint_valid: valid,
        };

        let mut accepted = SentenceRecoveryBuildOutcome {
            plan: Some(SentenceRecoveryPlan::default()),
            diagnostics: Some(diagnostics(
                false,
                SentenceEdgeRetainedFingerprint::default(),
            )),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        let replay = SentenceRecoveryBuildOutcome {
            diagnostics: Some(diagnostics(
                false,
                SentenceEdgeRetainedFingerprint::default(),
            )),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        record_sentence_edge_signature_direct_replay(&mut accepted, &replay);
        let plan_mismatch = accepted
            .diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_direct_shadow
            .expect("direct metrics exist");
        assert!(!plan_mismatch.complete);
        assert_eq!(
            plan_mismatch.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure)
        );
        assert!(!plan_mismatch.verification_evaluable);

        let baseline = SentenceEdgeRetainedFingerprint::default();
        let mut changed = baseline;
        changed
            .record(NearSearchScope::CrossSpan, OccurrenceSide::Old, 1, 2)
            .expect("fingerprint records");
        let mut accepted = SentenceRecoveryBuildOutcome {
            diagnostics: Some(diagnostics(true, baseline)),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        let replay = SentenceRecoveryBuildOutcome {
            diagnostics: Some(diagnostics(false, changed)),
            fragment_veto_complete: true,
            ..SentenceRecoveryBuildOutcome::default()
        };
        record_sentence_edge_signature_direct_replay(&mut accepted, &replay);
        let fingerprint_mismatch = accepted
            .diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_direct_shadow
            .expect("direct metrics exist");
        assert!(!fingerprint_mismatch.complete);
        assert!(fingerprint_mismatch.verification_evaluable);
        assert_eq!(fingerprint_mismatch.retained_pair_set_mismatches, 1);
        assert_eq!(
            fingerprint_mismatch.stop_reason,
            Some(SentenceEdgeSignatureDirectShadowStopReason::DiagnosticFailure)
        );
    }

    #[test]
    fn signature_metrics_preserve_the_first_stop_reason() {
        let signature = SentenceEdgeSignatureShadowMetrics {
            stop_reason: Some(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit),
            ..SentenceEdgeSignatureShadowMetrics::default()
        };
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics {
                sentence_edge_signature_shadow: Some(signature),
                ..SentenceRecoveryMetrics::default()
            },
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget fits");
        budget.near_relation_stop_reason = Some(NearRelationStopReason::SimilarityComparisonLimit);

        record_near_search_metrics(&mut diagnostics, &budget);

        let after_near = diagnostics
            .as_ref()
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics remain");
        assert_eq!(after_near.stop_reason, signature.stop_reason);

        let relations = ModifiedSentenceRelations {
            old: Vec::new(),
            new: Vec::new(),
            complete: false,
            edge_gate_shadow: None,
            edge_signature_shadow: Some(SentenceEdgeSignatureShadow {
                metrics: after_near,
                direct_metrics: SentenceEdgeSignatureDirectShadowMetrics::default(),
                posting_limit: 1,
                query_limit: 1,
                distinct_key_limit: 1,
                estimated_byte_limit: 1,
                query_count_limit: 1,
                candidate_union_limit: 1,
                active: false,
                mode: SentenceEdgeSignatureFilterMode::PostUnion,
                retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            }),
        };
        record_sentence_edge_gate_shadow(
            &mut diagnostics,
            &relations,
            Some(NearRelationStopReason::PairVisitLimit),
        );

        let after_relations = diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics remain");
        assert_eq!(after_relations.stop_reason, signature.stop_reason);
    }

    #[test]
    fn signature_collision_overinclude_does_not_mark_replay_incomplete() {
        let relations = ModifiedSentenceRelations {
            old: Vec::new(),
            new: Vec::new(),
            complete: true,
            edge_gate_shadow: None,
            edge_signature_shadow: Some(SentenceEdgeSignatureShadow {
                metrics: SentenceEdgeSignatureShadowMetrics {
                    signature_not_in_edge_union: 1,
                    ..SentenceEdgeSignatureShadowMetrics::default()
                },
                direct_metrics: SentenceEdgeSignatureDirectShadowMetrics::default(),
                posting_limit: 1,
                query_limit: 1,
                distinct_key_limit: 1,
                estimated_byte_limit: 1,
                query_count_limit: 1,
                candidate_union_limit: 1,
                active: true,
                mode: SentenceEdgeSignatureFilterMode::PostUnion,
                retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            }),
        };
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        record_sentence_edge_gate_shadow(&mut diagnostics, &relations, None);

        let metrics = diagnostics
            .expect("diagnostics remain")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics exist");
        assert!(metrics.complete);
        assert_eq!(metrics.stop_reason, None);
        assert_eq!(metrics.signature_not_in_edge_union, 1);
    }

    #[test]
    fn signature_replay_parity_requires_complete_near_relations() {
        let mut accepted = fallback_test_outcome(true, 41, 42);
        {
            let metrics = &mut accepted
                .diagnostics
                .as_mut()
                .expect("accepted diagnostics exist")
                .metrics;
            metrics.sentence_edge_filter_complete = true;
            metrics.sentence_edge_filter_pairs_retained = 3;
        }
        let mut replay = fallback_test_outcome(true, 41, 0);
        {
            let metrics = &mut replay
                .diagnostics
                .as_mut()
                .expect("replay diagnostics exist")
                .metrics;
            metrics.sentence_edge_filter_complete = true;
            metrics.sentence_edge_signature_shadow = Some(SentenceEdgeSignatureShadowMetrics {
                complete: true,
                exact_edge_retained_pairs: 2,
                ..SentenceEdgeSignatureShadowMetrics::default()
            });
        }

        record_sentence_edge_signature_replay(&mut accepted, replay);

        let complete = accepted
            .diagnostics
            .as_ref()
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics exist");
        assert!(complete.parity_evaluable);
        assert!(complete.verification_evaluable);
        assert!(complete.plan_parity);
        assert_eq!(complete.retained_pair_misses, 1);

        let mut accepted = fallback_test_outcome(true, 41, 42);
        let mut replay = fallback_test_outcome(true, 41, 0);
        replay
            .diagnostics
            .as_mut()
            .expect("replay diagnostics exist")
            .metrics
            .sentence_edge_signature_shadow = Some(SentenceEdgeSignatureShadowMetrics {
            complete: true,
            ..SentenceEdgeSignatureShadowMetrics::default()
        });

        record_sentence_edge_signature_replay(&mut accepted, replay);

        let incomplete = accepted
            .diagnostics
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics exist");
        assert!(!incomplete.parity_evaluable);
        assert!(!incomplete.verification_evaluable);
        assert!(!incomplete.plan_parity);

        let mut accepted = fallback_test_outcome(true, 41, 42);
        accepted
            .diagnostics
            .as_mut()
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_filter_complete = true;
        let mut replay = fallback_test_outcome(true, 41, 0);
        {
            let metrics = &mut replay
                .diagnostics
                .as_mut()
                .expect("replay diagnostics exist")
                .metrics;
            metrics.sentence_edge_filter_complete = true;
            metrics.sentence_edge_signature_shadow = Some(SentenceEdgeSignatureShadowMetrics {
                complete: false,
                stop_reason: Some(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit),
                ..SentenceEdgeSignatureShadowMetrics::default()
            });
        }

        record_sentence_edge_signature_replay(&mut accepted, replay);

        let stopped = accepted
            .diagnostics
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics exist");
        assert!(!stopped.parity_evaluable);
        assert!(!stopped.plan_parity);
    }

    #[test]
    fn signature_replay_rejects_impossible_retained_pair_count() {
        let mut accepted = fallback_test_outcome(true, 51, 52);
        {
            let metrics = &mut accepted
                .diagnostics
                .as_mut()
                .expect("accepted diagnostics exist")
                .metrics;
            metrics.sentence_edge_filter_complete = true;
            metrics.sentence_edge_filter_pairs_retained = 1;
        }
        let mut replay = fallback_test_outcome(true, 51, 0);
        {
            let metrics = &mut replay
                .diagnostics
                .as_mut()
                .expect("replay diagnostics exist")
                .metrics;
            metrics.sentence_edge_filter_complete = true;
            metrics.sentence_edge_signature_shadow = Some(SentenceEdgeSignatureShadowMetrics {
                complete: true,
                exact_edge_retained_pairs: 2,
                ..SentenceEdgeSignatureShadowMetrics::default()
            });
        }

        record_sentence_edge_signature_replay(&mut accepted, replay);

        let metrics = accepted
            .diagnostics
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics exist");
        assert!(!metrics.complete);
        assert!(!metrics.verification_evaluable);
        assert_eq!(
            metrics.stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure)
        );
    }

    #[test]
    fn signature_replay_does_not_evaluate_inconsistent_completed_stop() {
        let mut accepted = fallback_test_outcome(true, 61, 62);
        accepted
            .diagnostics
            .as_mut()
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_filter_complete = true;
        let mut replay = fallback_test_outcome(true, 61, 0);
        {
            let metrics = &mut replay
                .diagnostics
                .as_mut()
                .expect("replay diagnostics exist")
                .metrics;
            metrics.sentence_edge_filter_complete = true;
            metrics.sentence_edge_signature_shadow = Some(SentenceEdgeSignatureShadowMetrics {
                complete: true,
                stop_reason: Some(SentenceEdgeSignatureShadowStopReason::AllocationFailure),
                ..SentenceEdgeSignatureShadowMetrics::default()
            });
        }

        record_sentence_edge_signature_replay(&mut accepted, replay);

        let metrics = accepted
            .diagnostics
            .expect("accepted diagnostics exist")
            .metrics
            .sentence_edge_signature_shadow
            .expect("signature metrics exist");
        assert!(!metrics.parity_evaluable);
        assert!(!metrics.verification_evaluable);
        assert!(!metrics.plan_parity);
    }

    #[test]
    fn replay_failure_preserves_accepted_plan_watch_and_production_metrics() {
        let mut accepted = fallback_test_outcome(true, 31, 32);
        let expected = fallback_test_outcome(true, 31, 32);
        let expected_watch = accepted.watch_diagnostics.clone();
        let expected_pair_visits = accepted
            .diagnostics
            .as_ref()
            .expect("accepted diagnostics exist")
            .metrics
            .near_pair_visits_examined;

        record_sentence_edge_signature_replay_failure(&mut accepted);

        assert!(sentence_recovery_plan_parity(
            accepted.plan.as_ref(),
            expected.plan.as_ref()
        ));
        assert_eq!(accepted.watch_diagnostics, expected_watch);
        let metrics = accepted.diagnostics.expect("diagnostics remain").metrics;
        assert_eq!(metrics.near_pair_visits_examined, expected_pair_visits);
        assert_eq!(
            metrics
                .sentence_edge_signature_shadow
                .expect("typed replay failure exists")
                .stop_reason,
            Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure)
        );
    }

    #[test]
    fn unit_candidate_index_unions_first_and_last_postings_in_index_order() {
        let occurrences = [
            indexed_occurrence(
                &['a', 'x'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['y', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(&['q'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
        ];
        let query = indexed_occurrence(
            &['a', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(5, 2, 7, 1).expect("budget is valid");

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &query,
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("query succeeds");

        assert_eq!(plausible, vec![0, 1]);
    }

    #[test]
    fn span_candidate_index_includes_ambiguous_and_skips_unrelated_spans() {
        let mut occurrences = [
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
        ];
        occurrences[0].span_index = Some(0);
        occurrences[1].span_index = Some(1);
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Span)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(3, 1, 4, 1).expect("budget is valid");

        let query = index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[0],
                &occurrences,
                CandidatePostingBucket::Span(Some(0)),
                Some(CandidatePostingBucket::Span(None)),
                &mut budget,
            )
            .expect("query succeeds");
        budget.record_candidate_query(query, RecoveryUnitKind::Sentence, plausible.len());

        assert_eq!(plausible, vec![0, 2]);
        assert_eq!(budget.candidate_posting_visits, 2);
        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .edge_posting_visits_examined,
            1
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .edge_posting_visits_examined,
            1
        );
        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .edge_query_union_candidates,
            1
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .edge_query_union_candidates,
            1
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn paired_candidate_index_skips_unrelated_intervals() {
        let occurrences = [
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
        ];
        let first = PairedInterval {
            pair_index: 0,
            interval_index: 0,
        };
        let intervals = [
            Some(first),
            Some(PairedInterval {
                pair_index: 0,
                interval_index: 1,
            }),
            None,
        ];
        let index =
            UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Paired(&intervals))
                .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(3, 1, 4, 1).expect("budget is valid");

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[0],
                &occurrences,
                CandidatePostingBucket::Paired(first),
                None,
                &mut budget,
            )
            .expect("query succeeds");

        assert_eq!(plausible, vec![0]);
        assert_eq!(budget.candidate_posting_visits, 1);
    }

    #[test]
    fn cross_interval_pass_ignores_unrelated_kind_and_role() {
        let first = PairedInterval {
            pair_index: 0,
            interval_index: 0,
        };
        let second = PairedInterval {
            pair_index: 0,
            interval_index: 1,
        };
        let old_occurrences = [indexed_occurrence(
            &['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let new_occurrences = [
            indexed_occurrence(
                &['0', '1', '2', '3', '4'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
                RecoveryUnitKind::Line,
                Some(BlockRole::RepeatedFooter),
            ),
        ];
        let old_candidates = paired_test_candidates(0, first);
        let new_candidates = paired_test_candidates(0, first);
        let old_candidate_by_occurrence = [Some(0)];
        let new_candidate_by_occurrence = [Some(0), None, None];
        let old_intervals = [Some(first)];
        let new_intervals = [Some(first), Some(second), Some(second)];
        let mut relations = empty_modified_sentence_relations(
            &old_candidates.recoveries,
            &new_candidates.recoveries,
        )
        .expect("relation allocation succeeds");
        let mut budget = RecoveryBudget::new(10, 25, 35, 1).expect("budget is valid");
        let mut diagnostics = None;

        record_cross_interval_disqualifying_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &old_candidate_by_occurrence,
            &new_candidate_by_occurrence,
            &old_intervals,
            &new_intervals,
            &mut relations,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("cross-interval evidence fits the budget");

        assert!(!relations.old[0].vetoed());
        assert!(!relations.new[0].vetoed());
    }

    #[test]
    fn same_interval_noise_does_not_hide_compatible_cross_interval_veto() {
        let first = PairedInterval {
            pair_index: 0,
            interval_index: 0,
        };
        let second = PairedInterval {
            pair_index: 0,
            interval_index: 1,
        };
        let old_occurrences = [indexed_occurrence(
            &['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let new_occurrences = [
            indexed_occurrence(
                &['0', '1', '2', '3', '4'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['q', '1', '1', '1', '1', '1', '1', '1', '1', 'z'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
                RecoveryUnitKind::Line,
                Some(BlockRole::Body),
            ),
        ];
        let old_candidates = paired_test_candidates(0, first);
        let new_candidates = paired_test_candidates(0, first);
        let old_candidate_by_occurrence = [Some(0)];
        let new_candidate_by_occurrence = [Some(0), None, None];
        let old_intervals = [Some(first)];
        let new_intervals = [Some(first), Some(first), Some(second)];
        let mut relations = empty_modified_sentence_relations(
            &old_candidates.recoveries,
            &new_candidates.recoveries,
        )
        .expect("relation allocation succeeds");
        let mut budget = RecoveryBudget::new(10, 25, 35, 1).expect("budget is valid");
        let mut diagnostics = None;

        record_cross_interval_disqualifying_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &old_candidate_by_occurrence,
            &new_candidate_by_occurrence,
            &old_intervals,
            &new_intervals,
            &mut relations,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("cross-interval evidence fits the budget");

        assert!(relations.old[0].vetoed());
        assert!(!relations.new[0].vetoed());
    }

    #[test]
    fn compatible_cross_interval_candidates_veto_both_sides() {
        let first = PairedInterval {
            pair_index: 0,
            interval_index: 0,
        };
        let second = PairedInterval {
            pair_index: 0,
            interval_index: 1,
        };
        let old_occurrences = [indexed_occurrence(
            &['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let new_occurrences = [indexed_occurrence(
            &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let old_candidates = paired_test_candidates(0, first);
        let new_candidates = paired_test_candidates(0, second);
        let old_candidate_by_occurrence = [Some(0)];
        let new_candidate_by_occurrence = [Some(0)];
        let old_intervals = [Some(first)];
        let new_intervals = [Some(second)];
        let mut relations = empty_modified_sentence_relations(
            &old_candidates.recoveries,
            &new_candidates.recoveries,
        )
        .expect("relation allocation succeeds");
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("budget is valid");
        let mut diagnostics = None;

        record_cross_interval_disqualifying_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &old_candidate_by_occurrence,
            &new_candidate_by_occurrence,
            &old_intervals,
            &new_intervals,
            &mut relations,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("cross-interval evidence fits the budget");

        assert!(relations.old[0].vetoed());
        assert!(relations.new[0].vetoed());
    }

    #[test]
    fn line_trigram_index_finds_near_pair_with_different_edges() {
        let old_occurrences = [indexed_occurrence(
            &['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let new_occurrences = [indexed_occurrence(
            &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("budget is valid");
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("interior trigram relation fits the budget");

        assert_eq!(relations.old[0].unique_partner(), Some(0));
        assert_eq!(relations.new[0].unique_partner(), Some(0));
    }

    #[test]
    fn line_trigram_index_excludes_cross_kind_and_cross_role_postings() {
        let shared = ['q', 'x', 'y', 'w', 'z'];
        let occurrences = [
            indexed_occurrence(&shared, RecoveryUnitKind::Line, Some(BlockRole::Body)),
            indexed_occurrence(
                &shared,
                RecoveryUnitKind::Line,
                Some(BlockRole::RepeatedFooter),
            ),
            indexed_occurrence(&shared, RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
        ];
        let query = indexed_occurrence(
            &['q', 'b', 'c', 'd', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(15, 5, 20, 1).expect("budget is valid");

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &query,
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("query succeeds");

        assert_eq!(plausible, vec![0]);
    }

    #[test]
    fn line_trigram_index_retains_repeated_window_multiplicity() {
        let occurrences = [indexed_occurrence(
            &['a', 'a', 'a', 'a', 'a', 'a'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let trigram = [SentenceEvidenceToken::Scalar('a'); LINE_NGRAM_SIZE];

        assert_eq!(
            index
                .line_trigram_postings
                .get(&(
                    CandidatePostingBucket::Global,
                    RecoveryUnitKind::Line,
                    OccurrenceRole::Body,
                    trigram,
                ))
                .map(Vec::as_slice),
            Some(
                &[LineTrigramPosting {
                    occurrence_index: 0,
                    multiplicity: 4,
                }][..]
            )
        );

        let query = indexed_occurrence(
            &['a', 'a', 'a', 'a', 'a'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(5, 6, 11, 1).expect("budget is valid");
        index
            .collect_plausible_occurrences(
                &mut plausible,
                &query,
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("multiplicity-aware query succeeds");
        assert_eq!(plausible, vec![0]);
    }

    #[test]
    fn line_trigram_candidate_threshold_is_exact_at_seven_thousand() {
        assert_eq!(
            line_trigram_candidate_meets_threshold(6_999, 10_000, 10_000),
            Some(false)
        );
        assert_eq!(
            line_trigram_candidate_meets_threshold(7_000, 10_000, 10_000),
            Some(true)
        );
    }

    #[test]
    fn one_trigram_noise_does_not_reach_pair_or_similarity_work() {
        let old_occurrences = [indexed_occurrence(
            &['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let new_occurrences = [indexed_occurrence(
            &['x', 'a', 'b', 'c', 'y', 'z', 'u', 'v', 'w', 'q'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("budget is valid");
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &[],
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("low-overlap trigram noise is filtered");

        assert!(!relations.old[0].vetoed());
        assert_eq!(budget.candidate_posting_visits, 18);
        assert_eq!(budget.pair_visits, 0);
        assert_eq!(budget.comparisons, 0);
    }

    #[test]
    fn paired_stream_line_index_skips_unrelated_streams_before_charging_work() {
        let query = indexed_occurrence(
            &['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let occurrences = (0..64)
            .map(|_| {
                indexed_occurrence(
                    &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
                    RecoveryUnitKind::Line,
                    Some(BlockRole::Body),
                )
            })
            .collect::<Vec<_>>();
        let intervals = (0..64)
            .map(|occurrence_index| {
                Some(PairedInterval {
                    pair_index: occurrence_index,
                    interval_index: 1,
                })
            })
            .collect::<Vec<_>>();
        let index = UnitCandidateIndex::new(
            &occurrences,
            CandidatePostingIndexScope::PairedStream(&intervals),
        )
        .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(10, 640, 650, 1).expect("budget is valid");
        budget.candidate_posting_visit_limit = 14;

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &query,
                &occurrences,
                CandidatePostingBucket::PairedStream(0),
                None,
                &mut budget,
            )
            .expect("same-pair interior evidence fits the exact posting budget");

        assert_eq!(plausible, vec![0]);
        assert_eq!(budget.candidate_posting_visits, 14);
        assert_eq!(budget.candidate_posting_visits_attempted, 14);
        assert_eq!(
            budget
                .same_or_ambiguous_shared_query_work
                .line_work
                .line_trigram_posting_visits_examined,
            14
        );
        assert_eq!(
            budget
                .same_known_span_work
                .line_work
                .line_trigram_posting_visits_examined,
            0
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .line_work
                .line_trigram_posting_visits_examined,
            0
        );
        assert_near_work_sums_match_aggregates(&budget);

        let mut limited = Vec::new();
        let mut limited_budget = RecoveryBudget::new(10, 640, 650, 1).expect("budget is valid");
        limited_budget.candidate_posting_visit_limit = 13;

        assert!(
            index
                .collect_plausible_occurrences(
                    &mut limited,
                    &query,
                    &occurrences,
                    CandidatePostingBucket::PairedStream(0),
                    None,
                    &mut limited_budget,
                )
                .is_none()
        );
        assert!(limited.is_empty());
        assert_eq!(limited_budget.candidate_posting_visits, 8);
        assert_eq!(limited_budget.candidate_posting_visits_attempted, 14);
        assert_eq!(
            limited_budget
                .same_or_ambiguous_shared_query_work
                .line_work
                .line_trigram_posting_visits_examined,
            8
        );
        assert_eq!(
            limited_budget
                .same_known_span_work
                .line_work
                .line_trigram_posting_visits_attempted,
            0
        );
        assert_near_work_sums_match_aggregates(&limited_budget);
        assert_eq!(
            limited_budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidatePostingVisitLimit)
        );
    }

    #[test]
    fn line_trigram_posting_work_is_topology_local_before_traversal() {
        let mut old_occurrence = indexed_occurrence(
            &['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        old_occurrence.span_index = Some(0);
        let new_occurrences = (0..64)
            .map(|span_index| {
                let mut occurrence = indexed_occurrence(
                    &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
                    RecoveryUnitKind::Line,
                    Some(BlockRole::Body),
                );
                occurrence.span_index = Some(span_index);
                occurrence
            })
            .collect::<Vec<_>>();
        let old_occurrences = [old_occurrence];
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut relations = empty_modified_sentence_relations(&old_candidates, &[])
            .expect("relation allocation succeeds");
        let mut budget = RecoveryBudget::new(10, 640, 650, 1).expect("budget is valid");
        budget.candidate_posting_visit_limit = 14;
        let mut diagnostics = None;

        extend_modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &[],
            NearRelationScope::SameOrAmbiguous,
            &mut relations,
            &mut budget,
            &mut diagnostics,
            None,
            None,
        )
        .expect("the exact posting-work limit admits the query");

        assert!(relations.old[0].vetoed());
        assert_eq!(budget.candidate_posting_visits, 14);
        assert_eq!(budget.candidate_posting_visits_attempted, 14);
        assert_eq!(budget.pair_visits, 1);
        assert_eq!(
            budget
                .same_or_ambiguous_shared_query_work
                .line_work
                .line_trigram_posting_visits_examined,
            8
        );
        assert_eq!(
            budget
                .same_known_span_work
                .line_work
                .line_trigram_posting_visits_examined,
            6
        );
        assert_eq!(
            budget
                .same_known_span_work
                .line_work
                .line_trigram_only_query_union_candidates,
            1
        );
        assert_eq!(
            budget.same_known_span_work.line_work.pair_visits_examined,
            1
        );
        assert_near_work_sums_match_aggregates(&budget);

        let mut limited_relations = empty_modified_sentence_relations(&old_candidates, &[])
            .expect("relation allocation succeeds");
        let mut limited_budget = RecoveryBudget::new(10, 640, 650, 1).expect("budget is valid");
        limited_budget.candidate_posting_visit_limit = 13;
        limited_budget.record_candidate_count_limit();
        let mut limited_diagnostics = None;

        assert!(
            extend_modified_sentence_relations(
                &old_occurrences,
                &new_occurrences,
                &old_candidates,
                &[],
                NearRelationScope::SameOrAmbiguous,
                &mut limited_relations,
                &mut limited_budget,
                &mut limited_diagnostics,
                None,
                None,
            )
            .is_none()
        );
        assert!(!limited_relations.old[0].vetoed());
        assert_eq!(limited_budget.candidate_posting_visits, 8);
        assert_eq!(limited_budget.candidate_posting_visits_attempted, 14);
        assert_eq!(limited_budget.pair_visits, 0);
        assert_eq!(limited_budget.comparisons, 0);
        assert_eq!(
            limited_budget
                .same_or_ambiguous_shared_query_work
                .line_work
                .line_trigram_posting_visits_examined,
            8
        );
        assert_eq!(
            limited_budget
                .same_known_span_work
                .line_work
                .line_trigram_posting_visits_attempted,
            6
        );
        assert_near_work_sums_match_aggregates(&limited_budget);
        assert!(limited_budget.candidate_count_truncated);
        assert_eq!(
            limited_budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidatePostingVisitLimit)
        );
    }

    #[test]
    fn line_trigram_candidate_budget_failure_aborts_relation_collection() {
        let old_occurrences = [indexed_occurrence(
            &['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let new_occurrences = [indexed_occurrence(
            &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        )];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("budget is valid");
        budget.pair_visits = budget.token_limit;
        let mut diagnostics = None;

        assert!(
            modified_sentence_relations(
                &old_occurrences,
                &new_occurrences,
                &candidates,
                &candidates,
                &[],
                &mut budget,
                &mut diagnostics,
                None,
            )
            .is_none()
        );
        assert_eq!(budget.comparisons, 0);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::PairVisitLimit)
        );
    }

    #[test]
    fn near_search_metrics_track_query_maxima_and_successful_charges() {
        let occurrences = [
            indexed_occurrence(
                &['a', 'x'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['a', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['y', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        let query = indexed_occurrence(
            &['a', 'z'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        );
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(8, 0, 8, 1).expect("budget is valid");
        let query_metrics = index
            .collect_plausible_occurrences(
                &mut plausible,
                &query,
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("query succeeds");
        budget.record_candidate_query(query_metrics, RecoveryUnitKind::Sentence, 2);
        assert!(budget.charge_pair_visits(2, RecoveryUnitKind::Sentence));
        assert!(budget.charge_comparisons(3, RecoveryUnitKind::Sentence));
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        record_near_search_metrics(&mut diagnostics, &budget);

        let metrics = diagnostics.expect("metrics remain available").metrics;
        assert_eq!(metrics.near_pair_visits_examined, 2);
        assert_eq!(metrics.near_pair_visits_attempted, 2);
        assert_eq!(metrics.near_similarity_comparisons_examined, 3);
        assert_eq!(metrics.near_similarity_comparisons_attempted, 3);
        assert_eq!(metrics.near_candidate_posting_visits_examined, 4);
        assert_eq!(metrics.near_candidate_posting_visits_attempted, 4);
        assert_eq!(metrics.near_largest_edge_posting, 2);
        assert_eq!(metrics.near_largest_edge_query_union, 3);
        assert_eq!(metrics.near_largest_filtered_candidate_set, 2);
        assert_eq!(metrics.near_relation_stop_reason, None);
        assert_eq!(metrics.near_line_work, NearSearchWorkMetrics::default());
        assert_eq!(metrics.near_sentence_work.edge_query_union_candidates, 3);
        assert_eq!(metrics.near_sentence_work.filtered_candidates, 2);
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn line_query_separates_trigram_only_candidates_from_edge_overlap() {
        let trigram_only = indexed_occurrence(
            &['x', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'y'],
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let overlap_tokens = ['q', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'z'];
        let overlap = indexed_occurrence(
            &overlap_tokens,
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let occurrences = [trigram_only, overlap];
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(40, 0, 40, 1).expect("budget is valid");

        let query_occurrence = indexed_occurrence(
            &overlap_tokens,
            RecoveryUnitKind::Line,
            Some(BlockRole::Body),
        );
        let query = index
            .collect_plausible_occurrences(
                &mut plausible,
                &query_occurrence,
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("query succeeds");
        budget.record_candidate_query(query, RecoveryUnitKind::Line, plausible.len());

        assert_eq!(query.edge_query_union, 1);
        assert_eq!(query.line_trigram_only_query_union, 1);
        assert_eq!(plausible, [0, 1]);
        assert_eq!(budget.sentence_work, NearSearchWorkMetrics::default());
        assert_eq!(budget.line_work.edge_query_union_candidates, 1);
        assert_eq!(budget.line_work.line_trigram_only_query_union_candidates, 1);
        assert!(budget.line_work.line_trigram_posting_visits_examined > 0);
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn recovery_candidate_cap_is_complete_at_limit_and_incomplete_above_it() {
        let occurrences = (0..=MAX_UNTRUSTED_LINE_NEAR_CANDIDATES)
            .map(|index| {
                let mut occurrence = positioned_occurrence(
                    &format!("unique-line-{index}"),
                    index as u64 + 1,
                    0,
                    index,
                );
                occurrence.kind = RecoveryUnitKind::Line;
                occurrence
            })
            .collect::<Vec<_>>();
        let counts = occurrence_counts(&occurrences, &[]).expect("occurrence counts fit");

        let at_limit = recovery_candidates(
            &occurrences[..MAX_UNTRUSTED_LINE_NEAR_CANDIDATES],
            &counts,
            OccurrenceSide::Old,
            &[true],
            1,
        )
        .expect("candidate collection succeeds at the limit");
        let above_limit =
            recovery_candidates(&occurrences, &counts, OccurrenceSide::Old, &[true], 1)
                .expect("candidate collection succeeds above the limit");

        assert!(at_limit.complete);
        assert_eq!(at_limit.values.len(), MAX_UNTRUSTED_LINE_NEAR_CANDIDATES);
        assert!(!above_limit.complete);
        assert_eq!(above_limit.values.len(), MAX_UNTRUSTED_LINE_NEAR_CANDIDATES);
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.record_candidate_count_limit();
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidateCountLimit)
        );
        assert!(budget.candidate_count_truncated);
        assert_eq!(budget.pair_visits, budget.pair_visits_attempted);
        assert_eq!(budget.comparisons, budget.comparisons_attempted);
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        record_near_search_metrics(&mut diagnostics, &budget);
        let metrics = diagnostics.expect("metrics remain available").metrics;
        assert!(metrics.near_candidate_count_truncated);
        assert_eq!(
            metrics.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidateCountLimit)
        );
    }

    #[test]
    fn reverse_query_records_fan_out_even_when_forward_examined_every_pair() {
        let old_occurrences = [
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
        ];
        let new_occurrences = [indexed_occurrence(
            &['a'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let old_candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 2,
                span_index: 0,
            },
        ];
        let new_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(8, 0, 8, 1).expect("budget is valid");
        let mut diagnostics = None;

        modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("relations fit within budget");

        assert_eq!(budget.pair_visits, 3);
        assert_eq!(budget.largest_filtered_candidate_set, 3);
        assert_eq!(budget.sentence_work.edge_query_union_candidates, 12);
        assert_eq!(budget.sentence_work.filtered_candidates, 3);
        assert_eq!(
            budget
                .same_or_ambiguous_span_work
                .sentence_work
                .edge_query_union_candidates,
            6
        );
        assert_eq!(
            budget
                .same_or_ambiguous_span_work
                .sentence_work
                .filtered_candidates,
            3
        );
        assert_eq!(
            budget
                .cross_span_work
                .sentence_work
                .edge_query_union_candidates,
            6
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn comparison_limit_records_first_stop_reason_without_examining_failed_work() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.record_candidate_count_limit();
        assert!(budget.charge_comparisons(4, RecoveryUnitKind::Sentence));
        assert!(!budget.charge_comparisons(2, RecoveryUnitKind::Sentence));

        assert_eq!(budget.comparisons, 4);
        assert_eq!(budget.comparisons_attempted, 6);
        assert_eq!(budget.sentence_work.similarity_comparisons_examined, 4);
        assert_eq!(budget.sentence_work.similarity_comparisons_attempted, 6);
        assert_near_work_sums_match_aggregates(&budget);
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::SimilarityComparisonLimit)
        );
    }

    #[test]
    fn candidate_posting_limit_remains_the_first_resource_stop() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.candidate_posting_visit_limit = 0;

        assert!(!budget.charge_candidate_posting_visits(
            1,
            RecoveryUnitKind::Sentence,
            CandidatePostingKind::Edge,
        ));
        assert!(!budget.charge_pair_visits(2, RecoveryUnitKind::Sentence));

        assert_eq!(budget.candidate_posting_visits, 0);
        assert_eq!(budget.candidate_posting_visits_attempted, 1);
        assert_eq!(budget.pair_visits, 0);
        assert_eq!(budget.pair_visits_attempted, 2);
        assert_eq!(budget.sentence_work.edge_posting_visits_examined, 0);
        assert_eq!(budget.sentence_work.edge_posting_visits_attempted, 1);
        assert_eq!(budget.sentence_work.filtered_candidates, 2);
        assert_near_work_sums_match_aggregates(&budget);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidatePostingVisitLimit)
        );
    }

    #[test]
    fn same_or_ambiguous_sentence_work_tracks_forward_and_reverse_candidates() {
        let occurrence = |span_index| {
            let mut occurrence = indexed_occurrence(
                &['a', 'b'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            );
            occurrence.span_index = span_index;
            occurrence
        };
        let old_occurrences = [occurrence(Some(0)), occurrence(None)];
        let new_occurrences = [occurrence(Some(0)), occurrence(None)];
        let old_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let new_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut relations = empty_modified_sentence_relations(&old_candidates, &new_candidates)
            .expect("relation allocation succeeds");
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
        let mut diagnostics = None;

        extend_modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            NearRelationScope::SameOrAmbiguous,
            &mut relations,
            &mut budget,
            &mut diagnostics,
            None,
            None,
        )
        .expect("same-span and ambiguous candidates fit the budget");

        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .pair_visits_examined,
            1
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .pair_visits_examined,
            2
        );
        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .similarity_comparisons_examined,
            2
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .similarity_comparisons_examined,
            4
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn known_span_sentence_shadow_reports_ambiguous_second_score_effects() {
        let occurrence = |tokens: &[char], span_index| {
            let mut occurrence =
                indexed_occurrence(tokens, RecoveryUnitKind::Sentence, Some(BlockRole::Body));
            occurrence.span_index = span_index;
            occurrence
        };
        let old_occurrences = [occurrence(&['a', 'b'], Some(0))];
        let new_occurrences = [
            occurrence(&['a', 'b'], Some(0)),
            occurrence(&['a', 'x'], None),
        ];
        let candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 0,
        }];
        let mut budget = RecoveryBudget::new(2, 4, 6, 1).expect("budget is valid");
        budget.enable_known_span_sentence_shadow = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("shadow relation fits within the production budget");
        let shadow = diagnostics
            .expect("diagnostics remain available")
            .metrics
            .known_span_sentence_shadow
            .expect("sentence shadow is available");

        assert!(relations.complete);
        assert!(shadow.complete);
        assert_eq!(shadow.pairs_considered, 2);
        assert_eq!(shadow.pairs_retained, 1);
        assert_eq!(shadow.pairs_rejected, 1);
        assert_eq!(shadow.cross_span_pairs_considered, 0);
        assert_eq!(shadow.same_paired_anchor_interval_pairs, 0);
        assert_eq!(shadow.same_paired_stream_other_interval_pairs, 0);
        assert_eq!(shadow.same_page_only_pairs, 0);
        assert_eq!(shadow.unclassified_pairs, 0);
        assert_eq!(shadow.best_partner_mismatches, 0);
        assert_eq!(shadow.best_score_mismatches, 0);
        assert_eq!(shadow.second_score_mismatches, 1);
        assert_eq!(shadow.veto_mismatches, 0);
        assert_eq!(shadow.unique_partner_mismatches, 0);
        assert_eq!(shadow.reciprocal_pair_mismatches, 0);
        assert!(!shadow.exact_relation_parity);
    }

    #[test]
    fn cross_span_sentence_shadow_uses_paired_anchor_locality() {
        fn shadow_for(
            old_stream: Option<(usize, usize)>,
            new_stream: Option<(usize, usize)>,
            old_page: Option<u32>,
            new_page: Option<u32>,
            pairs: &[PairedTrustedStream],
        ) -> KnownSpanSentenceShadowMetrics {
            let mut old = [positioned_occurrence("old", 1, 0, 0)];
            let mut new = [positioned_occurrence("new", 2, 1, 0)];
            old[0].span_index = Some(0);
            new[0].span_index = Some(1);
            old[0].trusted_position =
                old_stream.map(|(stream_index, ordinal)| TrustedStreamPosition {
                    stream_index,
                    ordinal,
                });
            new[0].trusted_position =
                new_stream.map(|(stream_index, ordinal)| TrustedStreamPosition {
                    stream_index,
                    ordinal,
                });
            old[0].page = old_page;
            new[0].page = new_page;
            let candidates = [RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            }];
            let mut budget = RecoveryBudget::new(5, 5, 10, 1).expect("budget is valid");
            budget.enable_known_span_sentence_shadow = true;
            let mut diagnostics = Some(SentenceRecoveryDiagnostics {
                metrics: SentenceRecoveryMetrics::default(),
                eligible_old_source_tokens: 0,
                eligible_new_source_tokens: 0,
                signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
                signature_retained_fingerprint_valid: false,
            });

            modified_sentence_relations(
                &old,
                &new,
                &candidates,
                &candidates,
                pairs,
                &mut budget,
                &mut diagnostics,
                None,
            )
            .expect("relation replay fits the budget");
            diagnostics
                .expect("diagnostics remain available")
                .metrics
                .known_span_sentence_shadow
                .expect("shadow replay succeeds")
        }

        let pairs = [PairedTrustedStream {
            old_stream: 0,
            new_stream: 1,
            anchors: vec![(5, 5)],
        }];
        let same_interval = shadow_for(Some((0, 0)), Some((1, 0)), None, None, &pairs);
        assert_eq!(same_interval.pairs_retained, 1);
        assert_eq!(same_interval.cross_span_pairs_considered, 1);
        assert_eq!(same_interval.same_paired_anchor_interval_pairs, 1);

        let other_interval = shadow_for(Some((0, 0)), Some((1, 6)), None, None, &pairs);
        assert_eq!(other_interval.pairs_rejected, 1);
        assert_eq!(other_interval.same_paired_stream_other_interval_pairs, 1);

        let same_page = shadow_for(None, None, Some(7), Some(7), &pairs);
        assert_eq!(same_page.pairs_rejected, 1);
        assert_eq!(same_page.same_page_only_pairs, 1);

        let unclassified = shadow_for(Some((0, 0)), None, Some(7), Some(8), &pairs);
        assert_eq!(unclassified.pairs_rejected, 1);
        assert_eq!(unclassified.unclassified_pairs, 1);
    }

    #[test]
    fn sentence_pair_locality_rejects_different_paired_streams() {
        let first = PairedInterval {
            pair_index: 0,
            interval_index: 1,
        };
        let second_pair = PairedInterval {
            pair_index: 1,
            interval_index: 1,
        };

        assert_eq!(
            sentence_pair_locality(Some(first), Some(second_pair), Some(3), Some(3)),
            SentencePairLocality::Unclassified
        );
        assert_eq!(
            sentence_pair_locality(Some(first), None, Some(3), Some(3)),
            SentencePairLocality::SamePageOnly
        );
        assert_eq!(
            sentence_pair_locality(None, None, None, None),
            SentencePairLocality::Unclassified
        );
    }

    #[test]
    fn cross_span_sentence_shadow_counts_reverse_disqualifying_work_once() {
        let mut old = [positioned_occurrence("old", 1, 0, 0)];
        let mut new = [positioned_occurrence("new", 2, 1, 0)];
        old[0].span_index = Some(0);
        new[0].span_index = Some(1);
        let new_candidates = [RecoveryCandidate {
            occurrence_index: 0,
            span_index: 1,
        }];
        let pairs = [PairedTrustedStream {
            old_stream: 0,
            new_stream: 1,
            anchors: Vec::new(),
        }];
        let mut budget = RecoveryBudget::new(5, 5, 10, 1).expect("budget is valid");
        budget.enable_known_span_sentence_shadow = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        modified_sentence_relations(
            &old,
            &new,
            &[],
            &new_candidates,
            &pairs,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("reverse disqualifying replay fits the budget");
        let shadow = diagnostics
            .expect("diagnostics remain available")
            .metrics
            .known_span_sentence_shadow
            .expect("shadow replay succeeds");

        assert_eq!(shadow.pairs_considered, 1);
        assert_eq!(shadow.pairs_retained, 1);
        assert_eq!(shadow.cross_span_pairs_considered, 1);
        assert_eq!(shadow.same_paired_anchor_interval_pairs, 1);
    }

    #[test]
    fn known_span_sentence_shadow_distinguishes_relation_mismatch_classes() {
        let mut production = CandidateNearRelation::default();
        production.record_eligible(1, MIN_NEAR_SCORE + MIN_NEAR_SCORE_MARGIN);
        let mut shadow = CandidateNearRelation::default();
        shadow.record_disqualifying(MIN_NEAR_SCORE + 1);
        let mut metrics = KnownSpanSentenceShadowMetrics::default();

        assert!(compare_known_span_shadow_relation(
            &production,
            &shadow,
            &mut metrics,
            true,
        ));
        assert_eq!(metrics.old_relation_mismatches, 1);
        assert_eq!(metrics.best_partner_mismatches, 1);
        assert_eq!(metrics.best_score_mismatches, 1);
        assert_eq!(metrics.second_score_mismatches, 0);
        assert_eq!(metrics.veto_mismatches, 0);
        assert_eq!(metrics.unique_partner_mismatches, 1);

        assert!(compare_known_span_shadow_relation(
            &CandidateNearRelation::default(),
            &production,
            &mut metrics,
            false,
        ));
        assert_eq!(metrics.new_relation_mismatches, 1);
        assert_eq!(metrics.veto_mismatches, 1);
    }

    #[test]
    fn known_span_sentence_shadow_passes_lines_without_counting_them() {
        let occurrence = |kind| {
            let mut occurrence = indexed_occurrence(&['a', 'b'], kind, Some(BlockRole::Body));
            occurrence.span_index = Some(0);
            occurrence
        };
        let old = [
            occurrence(RecoveryUnitKind::Sentence),
            occurrence(RecoveryUnitKind::Line),
        ];
        let new = [
            occurrence(RecoveryUnitKind::Sentence),
            occurrence(RecoveryUnitKind::Line),
        ];
        let candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 0,
            },
        ];
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
        budget.enable_known_span_sentence_shadow = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let relations = modified_sentence_relations(
            &old,
            &new,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("mixed relations fit within budget");
        let shadow = diagnostics
            .expect("diagnostics remain available")
            .metrics
            .known_span_sentence_shadow
            .expect("shadow replay succeeds");

        assert!(relations.complete);
        assert_eq!(shadow.pairs_considered, 1);
        assert_eq!(shadow.pairs_retained, 1);
        assert_eq!(shadow.pairs_rejected, 0);
        assert!(shadow.exact_relation_parity);
    }

    #[test]
    fn known_span_sentence_shadow_respects_line_work_before_sentence_budget_stop() {
        let occurrence = |kind, span_index| {
            let mut occurrence = indexed_occurrence(&['a', 'b'], kind, Some(BlockRole::Body));
            occurrence.span_index = Some(span_index);
            occurrence
        };
        let old = [
            occurrence(RecoveryUnitKind::Line, 0),
            occurrence(RecoveryUnitKind::Line, 1),
            occurrence(RecoveryUnitKind::Sentence, 0),
            occurrence(RecoveryUnitKind::Sentence, 1),
        ];
        let new = [
            occurrence(RecoveryUnitKind::Line, 0),
            occurrence(RecoveryUnitKind::Line, 1),
            occurrence(RecoveryUnitKind::Sentence, 0),
            occurrence(RecoveryUnitKind::Sentence, 1),
        ];
        let candidates = (0..4)
            .map(|occurrence_index| RecoveryCandidate {
                occurrence_index,
                span_index: occurrence_index % 2,
            })
            .collect::<Vec<_>>();
        let mut budget = RecoveryBudget::new(8, 8, 16, 1).expect("budget is valid");
        budget.token_limit = 5;
        budget.enable_known_span_sentence_shadow = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let relations = modified_sentence_relations(
            &old,
            &new,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("same-span relations survive the cross-span budget stop");
        let shadow = diagnostics
            .expect("diagnostics remain available")
            .metrics
            .known_span_sentence_shadow
            .expect("baseline replay matches production");

        assert!(!relations.complete);
        assert_eq!(budget.pair_visits, 5);
        assert_eq!(shadow.pairs_considered, 2);
        assert_eq!(shadow.pairs_retained, 2);
        assert!(shadow.exact_relation_parity);
    }

    #[test]
    fn failed_combined_charges_only_attempt_each_same_or_ambiguous_subdivision() {
        let mut budget = RecoveryBudget::new(2, 0, 2, 1).expect("budget is valid");
        let split = NearSearchWorkSplit {
            same_known: 1,
            ambiguous: 2,
            shared: 0,
        };

        assert!(!budget.charge_pair_visits_in_scope_split(
            3,
            RecoveryUnitKind::Sentence,
            NearSearchScope::SameOrAmbiguousSpan,
            split,
        ));
        budget.candidate_posting_visit_limit = 2;
        assert!(!budget.charge_candidate_posting_visits_in_scope_split(
            3,
            RecoveryUnitKind::Sentence,
            CandidatePostingKind::Edge,
            NearSearchScope::SameOrAmbiguousSpan,
            split,
        ));
        budget.comparison_limit = 2;
        assert!(!budget.charge_comparisons_in_scope_split(
            3,
            RecoveryUnitKind::Sentence,
            NearSearchScope::SameOrAmbiguousSpan,
            split,
        ));

        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .pair_visits_examined,
            0
        );
        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .pair_visits_attempted,
            1
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .pair_visits_attempted,
            2
        );
        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .edge_posting_visits_attempted,
            1
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .edge_posting_visits_attempted,
            2
        );
        assert_eq!(
            budget
                .same_known_span_work
                .sentence_work
                .similarity_comparisons_attempted,
            1
        );
        assert_eq!(
            budget
                .ambiguous_span_work
                .sentence_work
                .similarity_comparisons_attempted,
            2
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn near_search_work_is_attributed_to_each_active_scope_and_kind() {
        for scope in [
            NearSearchScope::PairedInterval,
            NearSearchScope::PairedCrossIntervalVeto,
            NearSearchScope::SameOrAmbiguousSpan,
            NearSearchScope::CrossSpan,
        ] {
            let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
            assert!(budget.charge_candidate_posting_visits_in_scope(
                1,
                RecoveryUnitKind::Sentence,
                CandidatePostingKind::Edge,
                scope,
            ));
            assert!(budget.charge_candidate_posting_visits_in_scope(
                1,
                RecoveryUnitKind::Line,
                CandidatePostingKind::LineTrigram,
                scope,
            ));
            budget.record_candidate_query_in_scope(
                UnitCandidateQueryMetrics {
                    largest_edge_posting: 0,
                    edge_query_union: 2,
                    line_trigram_only_query_union: 0,
                    ..UnitCandidateQueryMetrics::default()
                },
                RecoveryUnitKind::Sentence,
                1,
                scope,
            );
            budget.record_candidate_query_in_scope(
                UnitCandidateQueryMetrics {
                    largest_edge_posting: 0,
                    edge_query_union: 1,
                    line_trigram_only_query_union: 1,
                    ..UnitCandidateQueryMetrics::default()
                },
                RecoveryUnitKind::Line,
                1,
                scope,
            );
            assert!(budget.charge_pair_visits_in_scope(1, RecoveryUnitKind::Sentence, scope,));
            assert!(budget.charge_pair_visits_in_scope(1, RecoveryUnitKind::Line, scope,));
            assert!(budget.charge_comparisons_in_scope(1, RecoveryUnitKind::Sentence, scope,));
            assert!(budget.charge_comparisons_in_scope(1, RecoveryUnitKind::Line, scope,));

            let active = match scope {
                NearSearchScope::PairedInterval => budget.paired_interval_work,
                NearSearchScope::PairedCrossIntervalVeto => budget.paired_cross_interval_veto_work,
                NearSearchScope::SameOrAmbiguousSpan => budget.same_or_ambiguous_span_work,
                NearSearchScope::CrossSpan => budget.cross_span_work,
            };
            assert_eq!(active.sentence_work, budget.sentence_work);
            assert_eq!(active.line_work, budget.line_work);
            assert_near_work_sums_match_aggregates(&budget);
        }
    }

    #[test]
    fn failed_atomic_charge_is_attempted_only_in_the_active_scope_and_kind() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.candidate_posting_visit_limit = 0;

        assert!(!budget.charge_candidate_posting_visits_in_scope(
            1,
            RecoveryUnitKind::Line,
            CandidatePostingKind::Edge,
            NearSearchScope::CrossSpan,
        ));

        assert_eq!(budget.sentence_work, NearSearchWorkMetrics::default());
        assert_eq!(budget.line_work.edge_posting_visits_examined, 0);
        assert_eq!(budget.line_work.edge_posting_visits_attempted, 1);
        assert_eq!(
            budget
                .cross_span_work
                .line_work
                .edge_posting_visits_attempted,
            1
        );
        assert_eq!(
            budget.paired_interval_work,
            NearSearchScopeMetrics::default()
        );
        assert_eq!(
            budget.paired_cross_interval_veto_work,
            NearSearchScopeMetrics::default()
        );
        assert_eq!(
            budget.same_or_ambiguous_span_work,
            NearSearchScopeMetrics::default()
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    #[test]
    fn near_search_counter_overflow_makes_metrics_unavailable_without_a_limit_reason() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.pair_visits_attempted = usize::MAX;
        assert!(!budget.charge_pair_visits(1, RecoveryUnitKind::Sentence));
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        record_near_search_metrics(&mut diagnostics, &budget);

        assert!(diagnostics.is_none());
        assert_eq!(budget.near_relation_stop_reason, None);
    }

    #[test]
    fn speculative_near_search_commit_copies_per_kind_and_scope_work() {
        let mut budget = RecoveryBudget::new(4, 4, 8, 1).expect("budget is valid");
        assert!(budget.charge_pair_visits_in_scope(
            1,
            RecoveryUnitKind::Sentence,
            NearSearchScope::SameOrAmbiguousSpan,
        ));
        let mut speculative = budget;
        assert!(speculative.charge_pair_visits_in_scope(
            2,
            RecoveryUnitKind::Line,
            NearSearchScope::CrossSpan,
        ));
        assert!(speculative.charge_comparisons_in_scope(
            3,
            RecoveryUnitKind::Line,
            NearSearchScope::CrossSpan,
        ));
        speculative.sentence_edge_signature_filter_mode = SentenceEdgeSignatureFilterMode::Direct;
        assert!(speculative.charge_signature_exact_recheck(true));
        assert!(speculative.charge_signature_exact_recheck(false));
        speculative.signature_exact_recheck_stop_reason =
            Some(SentenceEdgeSignatureDirectShadowStopReason::SignatureExactEdgeRecheckLimit);

        budget.commit_near_search_spend_from(speculative);

        assert_eq!(budget.sentence_work, speculative.sentence_work);
        assert_eq!(budget.line_work, speculative.line_work);
        assert_eq!(
            budget.same_known_span_work,
            speculative.same_known_span_work
        );
        assert_eq!(budget.ambiguous_span_work, speculative.ambiguous_span_work);
        assert_eq!(
            budget.same_or_ambiguous_shared_query_work,
            speculative.same_or_ambiguous_shared_query_work
        );
        assert_eq!(budget.cross_span_work, speculative.cross_span_work);
        assert_eq!(
            budget.signature_exact_rechecks,
            speculative.signature_exact_rechecks
        );
        assert_eq!(
            budget.signature_exact_recheck_comparisons_attempted,
            speculative.signature_exact_recheck_comparisons_attempted
        );
        assert_eq!(
            budget.signature_exact_recheck_stop_reason,
            speculative.signature_exact_recheck_stop_reason
        );
        assert_near_work_sums_match_aggregates(&budget);
    }

    fn assert_near_work_sums_match_aggregates(budget: &RecoveryBudget) {
        assert_scope_work_sums_match_kind_work(budget);
        assert_same_or_ambiguous_subdivision_matches_parent(budget);
        assert_eq!(budget.sentence_work.line_trigram_posting_visits_examined, 0);
        assert_eq!(
            budget.sentence_work.line_trigram_posting_visits_attempted,
            0
        );
        assert_eq!(
            budget
                .sentence_work
                .edge_posting_visits_examined
                .checked_add(budget.line_work.edge_posting_visits_examined)
                .and_then(|total| {
                    total.checked_add(budget.line_work.line_trigram_posting_visits_examined)
                }),
            Some(budget.candidate_posting_visits)
        );
        assert_eq!(
            budget
                .sentence_work
                .edge_posting_visits_attempted
                .checked_add(budget.line_work.edge_posting_visits_attempted)
                .and_then(|total| {
                    total.checked_add(budget.line_work.line_trigram_posting_visits_attempted)
                }),
            Some(budget.candidate_posting_visits_attempted)
        );
        assert_eq!(
            budget
                .sentence_work
                .pair_visits_examined
                .checked_add(budget.line_work.pair_visits_examined),
            Some(budget.pair_visits)
        );
        assert_eq!(
            budget
                .sentence_work
                .pair_visits_attempted
                .checked_add(budget.line_work.pair_visits_attempted),
            Some(budget.pair_visits_attempted)
        );
        assert_eq!(
            budget
                .sentence_work
                .similarity_comparisons_examined
                .checked_add(budget.line_work.similarity_comparisons_examined),
            Some(budget.comparisons)
        );
        assert_eq!(
            budget
                .sentence_work
                .similarity_comparisons_attempted
                .checked_add(budget.line_work.similarity_comparisons_attempted),
            Some(budget.comparisons_attempted)
        );
    }

    fn assert_same_or_ambiguous_subdivision_matches_parent(budget: &RecoveryBudget) {
        for (parent, children) in [
            (
                budget.same_or_ambiguous_span_work.sentence_work,
                [
                    budget.same_known_span_work.sentence_work,
                    budget.ambiguous_span_work.sentence_work,
                    budget.same_or_ambiguous_shared_query_work.sentence_work,
                ],
            ),
            (
                budget.same_or_ambiguous_span_work.line_work,
                [
                    budget.same_known_span_work.line_work,
                    budget.ambiguous_span_work.line_work,
                    budget.same_or_ambiguous_shared_query_work.line_work,
                ],
            ),
        ] {
            macro_rules! assert_sum {
                ($field:ident) => {
                    assert_eq!(
                        children
                            .iter()
                            .try_fold(0usize, |total, work| total.checked_add(work.$field)),
                        Some(parent.$field),
                        stringify!($field)
                    );
                };
            }
            assert_sum!(edge_posting_visits_examined);
            assert_sum!(edge_posting_visits_attempted);
            assert_sum!(line_trigram_posting_visits_examined);
            assert_sum!(line_trigram_posting_visits_attempted);
            assert_sum!(edge_query_union_candidates);
            assert_sum!(line_trigram_only_query_union_candidates);
            assert_sum!(filtered_candidates);
            assert_sum!(pair_visits_examined);
            assert_sum!(pair_visits_attempted);
            assert_sum!(similarity_comparisons_examined);
            assert_sum!(similarity_comparisons_attempted);
        }
    }

    fn assert_scope_work_sums_match_kind_work(budget: &RecoveryBudget) {
        for (kind_work, scope_work) in [
            (
                budget.sentence_work,
                [
                    budget.paired_interval_work.sentence_work,
                    budget.paired_cross_interval_veto_work.sentence_work,
                    budget.same_or_ambiguous_span_work.sentence_work,
                    budget.cross_span_work.sentence_work,
                ],
            ),
            (
                budget.line_work,
                [
                    budget.paired_interval_work.line_work,
                    budget.paired_cross_interval_veto_work.line_work,
                    budget.same_or_ambiguous_span_work.line_work,
                    budget.cross_span_work.line_work,
                ],
            ),
        ] {
            macro_rules! assert_sum {
                ($field:ident) => {
                    assert_eq!(
                        scope_work
                            .iter()
                            .try_fold(0usize, |total, work| total.checked_add(work.$field)),
                        Some(kind_work.$field),
                        stringify!($field)
                    );
                };
            }
            assert_sum!(edge_posting_visits_examined);
            assert_sum!(edge_posting_visits_attempted);
            assert_sum!(line_trigram_posting_visits_examined);
            assert_sum!(line_trigram_posting_visits_attempted);
            assert_sum!(edge_query_union_candidates);
            assert_sum!(line_trigram_only_query_union_candidates);
            assert_sum!(filtered_candidates);
            assert_sum!(pair_visits_examined);
            assert_sum!(pair_visits_attempted);
            assert_sum!(similarity_comparisons_examined);
            assert_sum!(similarity_comparisons_attempted);
        }
    }

    #[test]
    fn unit_candidate_index_deduplicates_equal_edge_postings() {
        let occurrences = [indexed_occurrence(
            &['a'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(1, 1, 2, 1).expect("budget is valid");

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[0],
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("query succeeds");

        assert_eq!(plausible, vec![0]);
    }

    #[test]
    fn unit_candidate_index_constructor_preserves_aligned_edge_facts() {
        let occurrences = [
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
            indexed_occurrence(
                &['a', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['z', 'a'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['x', 'z'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
            indexed_occurrence(
                &['y', 'x'],
                RecoveryUnitKind::Sentence,
                Some(BlockRole::Body),
            ),
        ];
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let query = &occurrences[1];
        let facts = |candidate_index| {
            index.aligned_sentence_edge_facts(
                query,
                candidate_index,
                CandidatePostingBucket::Global,
                None,
            )
        };

        let one_token = facts(0).expect("one-token candidate shares the prefix");
        assert!(one_token.prefix_equal());
        assert!(!one_token.suffix_equal());
        let aligned = facts(1).expect("aligned candidate shares both edges");
        assert!(aligned.prefix_equal());
        assert!(aligned.suffix_equal());
        let crossing = facts(2).expect("cross-orientation candidate remains in the broad union");
        assert!(!crossing.prefix_equal());
        assert!(!crossing.suffix_equal());
        let suffix = facts(3).expect("suffix candidate shares the aligned suffix");
        assert!(!suffix.prefix_equal());
        assert!(suffix.suffix_equal());
        assert!(facts(4).is_none());
    }

    #[test]
    fn unit_candidate_index_reserves_map_capacity_by_distinct_edge_key() {
        let occurrences = (0..256)
            .map(|_| indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)))
            .collect::<Vec<_>>();

        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");

        assert_eq!(index.edge_postings.len(), 1);
        assert!(index.edge_postings.capacity() < occurrences.len());
        assert_eq!(
            index.edge_postings.values().next().map(Vec::len),
            Some(occurrences.len())
        );
    }

    #[test]
    fn unit_candidate_index_skips_roleless_occurrences_and_queries() {
        let occurrences = [
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, None),
            indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)),
        ];
        let index = UnitCandidateIndex::new(&occurrences, CandidatePostingIndexScope::Global)
            .expect("index construction succeeds");
        let mut plausible = Vec::new();
        let mut budget = RecoveryBudget::new(2, 1, 3, 1).expect("budget is valid");

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[1],
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("role-bearing query succeeds");
        assert_eq!(plausible, vec![1]);

        index
            .collect_plausible_occurrences(
                &mut plausible,
                &occurrences[0],
                &occurrences,
                CandidatePostingBucket::Global,
                None,
                &mut budget,
            )
            .expect("roleless query safely has no candidates");
        assert!(plausible.is_empty());
    }

    #[test]
    fn cross_span_budget_failure_keeps_same_span_relations_and_spent_work() {
        let occurrence = |key: &str, span_index: usize| SentenceOccurrence {
            key: key.to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a')],
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: None,
            span_index: Some(span_index),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        };
        let old_occurrences = [occurrence("old-a", 0), occurrence("old-b", 1)];
        let new_occurrences = [occurrence("new-a", 0), occurrence("new-b", 1)];
        let old_candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 1,
            },
        ];
        let new_candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 1,
            },
        ];
        let mut baseline_budget = RecoveryBudget::new(3, 0, 3, 1).expect("test budget is valid");
        baseline_budget.record_candidate_count_limit();
        let mut baseline_diagnostics = None;
        let baseline = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &[],
            &mut baseline_budget,
            &mut baseline_diagnostics,
            None,
        )
        .expect("baseline same-span relations survive optional cross-span exhaustion");
        let mut budget = RecoveryBudget::new(3, 0, 3, 1).expect("test budget is valid");
        budget.record_candidate_count_limit();
        budget.enable_known_span_sentence_shadow = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("same-span relations survive optional cross-span exhaustion");

        assert_eq!(relations.old, baseline.old);
        assert_eq!(relations.new, baseline.new);
        assert_eq!(relations.complete, baseline.complete);
        assert_eq!(budget.pair_visits, baseline_budget.pair_visits);
        assert_eq!(
            budget.pair_visits_attempted,
            baseline_budget.pair_visits_attempted
        );
        assert_eq!(budget.comparisons, baseline_budget.comparisons);
        assert_eq!(
            budget.comparisons_attempted,
            baseline_budget.comparisons_attempted
        );
        assert!(!relations.complete);
        assert_eq!(relations.old[0].unique_partner(), Some(0));
        assert_eq!(relations.old[1].unique_partner(), Some(1));
        assert_eq!(budget.pair_visits, 3);
        assert_eq!(budget.pair_visits_attempted, 4);
        assert_eq!(budget.comparisons, 3);
        assert_eq!(budget.comparisons_attempted, 3);
        assert_eq!(budget.candidate_posting_visits, 8);
        assert_eq!(budget.candidate_posting_visits_attempted, 8);
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::PairVisitLimit)
        );
        let shadow = diagnostics
            .expect("diagnostics remain available")
            .metrics
            .known_span_sentence_shadow
            .expect("same-phase shadow snapshot remains available");
        assert!(!shadow.complete);
        assert_eq!(shadow.pairs_considered, 2);
        assert_eq!(shadow.pairs_retained, 2);
        assert_eq!(shadow.pairs_rejected, 0);
        assert!(shadow.exact_relation_parity);
        assert_eq!(
            diagnostics.map(|diagnostics| {
                let metrics = diagnostics.metrics;
                (
                    metrics.relation_floor_pairs_considered,
                    metrics.relation_floor_word_scans,
                    metrics.relation_floor_stop_opportunities,
                    metrics.relation_floor_potential_saved_word_comparisons,
                )
            }),
            Some((1, 0, 0, 0))
        );
    }

    #[test]
    fn cross_span_comparison_failure_keeps_completed_relation_floor_probes() {
        let occurrence = |words: &[u8], token: char, span_index| {
            let mut occurrence = occurrence_from_word_ids(words, RecoveryUnitKind::Sentence);
            occurrence.tokens = (0..10)
                .map(|index| SentenceEvidenceToken::Scalar(if index < 3 { 'x' } else { token }))
                .collect();
            occurrence.span_index = Some(span_index);
            occurrence
        };
        let old_occurrences = [occurrence(&[0; 10], 'a', 0), occurrence(&[2; 10], 'c', 1)];
        let new_occurrences = [occurrence(&[1; 10], 'b', 0), occurrence(&[3; 10], 'd', 1)];
        let candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 1,
            },
        ];
        let mut successful_budget =
            RecoveryBudget::new(20, 20, 40, 1).expect("test budget is valid");
        let mut successful_diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &mut successful_budget,
            &mut successful_diagnostics,
            None,
        )
        .expect("unrestricted cross-span traversal completes");
        let same_span_comparisons = successful_budget
            .same_or_ambiguous_span_work
            .sentence_work
            .similarity_comparisons_examined;
        let cross_span_comparisons = successful_budget
            .cross_span_work
            .sentence_work
            .similarity_comparisons_examined;
        assert_eq!(cross_span_comparisons % 2, 0);

        let mut expected_relations = empty_modified_sentence_relations(&candidates, &candidates)
            .expect("relation allocation succeeds");
        let mut expected_budget = RecoveryBudget::new(20, 20, 40, 1).expect("test budget is valid");
        let mut expected_diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });
        extend_modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            NearRelationScope::SameOrAmbiguous,
            &mut expected_relations,
            &mut expected_budget,
            &mut expected_diagnostics,
            None,
            None,
        )
        .expect("same-span baseline completes");

        let mut budget = RecoveryBudget::new(20, 20, 40, 1).expect("test budget is valid");
        budget.comparison_limit = same_span_comparisons + cross_span_comparisons / 2;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("same-span relations survive cross-span comparison exhaustion");

        assert_eq!(relations.old, expected_relations.old);
        assert_eq!(relations.new, expected_relations.new);
        assert!(!relations.complete);
        assert_eq!(budget.comparisons, budget.comparison_limit);
        assert_eq!(budget.comparisons_attempted, budget.comparisons + 1);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::SimilarityComparisonLimit)
        );
        let metrics = diagnostics
            .expect("partial diagnostics remain available")
            .metrics;
        assert!(!metrics.near_relation_complete);
        assert_eq!(metrics.relation_floor_pairs_considered, 1);
        assert_eq!(metrics.relation_floor_word_scans, 1);
        assert_eq!(metrics.relation_floor_stop_opportunities, 0);
        assert_eq!(metrics.relation_floor_potential_saved_word_comparisons, 0);
    }

    #[test]
    fn cross_span_success_commits_equal_examined_and_attempted_work() {
        let occurrence = |key: &str, span_index: usize| SentenceOccurrence {
            key: key.to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a')],
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            role: Some(BlockRole::Body),
            location: None,
            span_index: Some(span_index),
            trusted_position: None,
            run_descriptor_index: None,
            page: None,
            evidence_block_index: None,
        };
        let old_occurrences = [occurrence("old-a", 0), occurrence("old-b", 1)];
        let new_occurrences = [occurrence("new-a", 0), occurrence("new-b", 1)];
        let candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 1,
            },
        ];
        let mut baseline_budget = RecoveryBudget::new(8, 0, 8, 1).expect("test budget is valid");
        baseline_budget.record_candidate_count_limit();
        let mut baseline_diagnostics = None;
        let baseline = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &mut baseline_budget,
            &mut baseline_diagnostics,
            None,
        )
        .expect("baseline cross-span relations complete within budget");
        let mut budget = RecoveryBudget::new(8, 0, 8, 1).expect("test budget is valid");
        budget.record_candidate_count_limit();
        budget.enable_known_span_sentence_shadow = true;
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
            signature_retained_fingerprint: SentenceEdgeRetainedFingerprint::default(),
            signature_retained_fingerprint_valid: false,
        });

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &[],
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("cross-span relations complete within budget");

        assert_eq!(relations.old, baseline.old);
        assert_eq!(relations.new, baseline.new);
        assert_eq!(budget.comparisons, baseline_budget.comparisons);
        assert_eq!(budget.pair_visits, baseline_budget.pair_visits);
        assert!(relations.complete);
        assert_eq!(budget.candidate_posting_visits, 12);
        assert_eq!(
            budget.candidate_posting_visits,
            budget.candidate_posting_visits_attempted
        );
        assert_eq!(budget.pair_visits, budget.pair_visits_attempted);
        assert_eq!(budget.comparisons, budget.comparisons_attempted);
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidateCountLimit)
        );
        let metrics = diagnostics.expect("diagnostics remain available").metrics;
        assert_eq!(metrics.relation_floor_pairs_considered, 2);
        assert_eq!(metrics.relation_floor_word_scans, 0);
        let shadow = metrics
            .known_span_sentence_shadow
            .expect("committed cross-span shadow is available");
        assert!(shadow.complete);
        assert_eq!(shadow.pairs_considered, 4);
        assert_eq!(shadow.pairs_retained, 2);
        assert_eq!(shadow.pairs_rejected, 2);
        assert_eq!(shadow.cross_span_pairs_considered, 2);
        assert_eq!(shadow.unclassified_pairs, 2);
        assert!(!shadow.exact_relation_parity);
    }

    #[test]
    fn recovery_range_cap_accepts_4096_and_rejects_one_more_without_fixtures() {
        let token_limit = MAX_SENTENCE_RECOVERY_RANGES + 1;
        let mut budget = RecoveryBudget::new(token_limit, 0, token_limit, 1)
            .expect("range-cap test budget is valid");
        assert!(budget.charge_outputs(MAX_SENTENCE_RECOVERY_RANGES, MAX_SENTENCE_RECOVERY_RANGES,));
        assert!(!budget.charge_output(1));
    }

    #[test]
    fn recovery_budget_rejects_arithmetic_overflow() {
        assert!(RecoveryBudget::new(usize::MAX, 1, usize::MAX, 1).is_none());
        assert!(RecoveryBudget::new(usize::MAX, 0, usize::MAX, 1).is_none());
    }
}
