use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

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

use super::{
    ExactSegmentRelation, MAX_SENTENCE_RECOVERY_OUTPUT_BYTES, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS,
    NearRelationStopReason, RecoveryWatchDiagnostics, RecoveryWatchNearScope,
    RecoveryWatchOccurrence, RecoveryWatchOccurrenceEvidence, RecoveryWatchOccurrences,
    RecoveryWatchPairEvidence, RecoveryWatchQuery, RecoveryWatchRecord, RecoveryWatchRelation,
    RecoveryWatchSegmentPairEvidence, RecoveryWatchUnitKind, RunSignatureStopReason,
    SegmentStopReason, SentenceRecoveryCommittedTokens, SentenceRecoveryInput,
    SentenceRecoveryMetrics, Side, TokenRange, TrustedRunRecoveryInput,
};

pub(super) const MAX_SENTENCE_RECOVERY_RANGES: usize = 8_192;
const MIN_NEAR_SCORE: u16 = 7_000;
const MIN_NEAR_SCORE_MARGIN: u16 = 500;
const MIN_WORD_SCORE_EDGE_EVIDENCE: u16 = 3_000;
const MIN_PAIRED_STREAM_EXACT_TOKENS: usize = 4;
const MIN_PAIRED_STREAM_NEAR_TOKENS: usize = 4;
const LINE_NGRAM_SIZE: usize = 3;
const MAX_LINE_NEAR_LENGTH_RATIO: usize = 3;
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

#[derive(Default)]
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
}

#[derive(Clone, Copy)]
struct SentenceRecoveryDiagnostics {
    metrics: SentenceRecoveryMetrics,
    eligible_old_source_tokens: usize,
    eligible_new_source_tokens: usize,
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
enum SentenceEvidenceToken {
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

struct SentenceOccurrence {
    key: String,
    tokens: Vec<SentenceEvidenceToken>,
    word_ranges: Vec<Range<usize>>,
    kind: RecoveryUnitKind,
    role: Option<BlockRole>,
    location: Option<SentenceLocation>,
    span_index: Option<usize>,
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
enum RecoveryUnitKind {
    Sentence,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OccurrenceRole {
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

struct UnitCandidateIndex {
    edge_postings: HashMap<(RecoveryUnitKind, OccurrenceRole, SentenceEvidenceToken), Vec<usize>>,
}

#[derive(Clone, Copy, Default)]
struct UnitCandidateQueryMetrics {
    largest_edge_posting: usize,
    edge_query_union: usize,
}

impl UnitCandidateIndex {
    fn new(occurrences: &[SentenceOccurrence]) -> Option<Self> {
        let mut index = Self {
            edge_postings: HashMap::new(),
        };
        for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
            let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
                continue;
            };
            let first = *occurrence.tokens.first()?;
            let last = *occurrence.tokens.last()?;
            index.push_edge_posting((occurrence.kind, role, first), occurrence_index)?;
            if last != first {
                index.push_edge_posting((occurrence.kind, role, last), occurrence_index)?;
            }
        }
        Some(index)
    }

    fn push_edge_posting(
        &mut self,
        key: (RecoveryUnitKind, OccurrenceRole, SentenceEvidenceToken),
        occurrence_index: usize,
    ) -> Option<()> {
        if !self.edge_postings.contains_key(&key) {
            self.edge_postings.try_reserve(1).ok()?;
            self.edge_postings.insert(key, Vec::new());
        }
        let postings = self.edge_postings.get_mut(&key)?;
        postings.try_reserve(1).ok()?;
        postings.push(occurrence_index);
        Some(())
    }

    fn collect_plausible_occurrences(
        &self,
        plausible: &mut Vec<usize>,
        occurrence: &SentenceOccurrence,
    ) -> Option<UnitCandidateQueryMetrics> {
        plausible.clear();
        let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
            return Some(UnitCandidateQueryMetrics::default());
        };
        let first = *occurrence.tokens.first()?;
        let last = *occurrence.tokens.last()?;
        // A pair satisfying the prefix/suffix threshold must share at least one
        // edge token, so this index prunes work without reducing candidate recall.
        let first_occurrences = self
            .edge_postings
            .get(&(occurrence.kind, role, first))
            .map_or(&[][..], Vec::as_slice);
        let last_occurrences = self
            .edge_postings
            .get(&(occurrence.kind, role, last))
            .map_or(&[][..], Vec::as_slice);
        plausible
            .try_reserve(
                first_occurrences
                    .len()
                    .checked_add(last_occurrences.len())?,
            )
            .ok()?;
        plausible.extend_from_slice(first_occurrences);
        if first != last {
            plausible.extend_from_slice(last_occurrences);
        }
        plausible.sort_unstable();
        plausible.dedup();
        Some(UnitCandidateQueryMetrics {
            largest_edge_posting: first_occurrences.len().max(last_occurrences.len()),
            edge_query_union: plausible.len(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PairedInterval {
    pair_index: usize,
    interval_index: usize,
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

#[derive(Clone, Copy, Default)]
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
    records: Vec<RecoveryWatchStateRecord>,
    pair_by_occurrences: HashMap<(usize, usize), Vec<usize>>,
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
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
            scan_work: 0,
            scan_limit: context.max_tokens.checked_mul(16)?,
            retained_one_sided_occurrences: 0,
        };
        state.records.try_reserve_exact(processed).ok()?;
        state.pair_by_occurrences.try_reserve(processed).ok()?;
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
struct RecoveryBudget {
    token_limit: usize,
    // Joined evidence may own synthetic separators. The caller's token budget
    // bounds them without counting them as source or recovered output tokens.
    evidence_token_limit: usize,
    key_byte_limit: usize,
    comparison_limit: usize,
    output_range_limit: usize,
    occurrences: usize,
    key_bytes: usize,
    pair_visits: usize,
    comparisons: usize,
    pair_visits_attempted: usize,
    comparisons_attempted: usize,
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
            evidence_token_limit: max_tokens,
            key_byte_limit: scaled_limit,
            comparison_limit: scaled_limit,
            output_range_limit: (token_limit / min_tokens).min(MAX_SENTENCE_RECOVERY_RANGES),
            occurrences: 0,
            key_bytes: 0,
            pair_visits: 0,
            comparisons: 0,
            pair_visits_attempted: 0,
            comparisons_attempted: 0,
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
        })
    }

    fn charge_occurrences(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.occurrences, amount, self.token_limit)
    }

    fn charge_key_bytes(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.key_bytes, amount, self.key_byte_limit)
    }

    fn charge_pair_visits(&mut self, amount: usize) -> bool {
        Self::charge_near_search(
            &mut self.pair_visits,
            &mut self.pair_visits_attempted,
            amount,
            self.token_limit,
            NearRelationStopReason::PairVisitLimit,
            &mut self.near_relation_stop_reason,
            &mut self.near_metrics_available,
        )
    }

    fn charge_comparisons(&mut self, amount: usize) -> bool {
        Self::charge_near_search(
            &mut self.comparisons,
            &mut self.comparisons_attempted,
            amount,
            self.comparison_limit,
            NearRelationStopReason::SimilarityComparisonLimit,
            &mut self.near_relation_stop_reason,
            &mut self.near_metrics_available,
        )
    }

    fn record_candidate_query(
        &mut self,
        query: UnitCandidateQueryMetrics,
        filtered_candidate_count: usize,
    ) {
        self.largest_edge_posting = self.largest_edge_posting.max(query.largest_edge_posting);
        self.largest_edge_query_union = self.largest_edge_query_union.max(query.edge_query_union);
        self.largest_filtered_candidate_set = self
            .largest_filtered_candidate_set
            .max(filtered_candidate_count);
    }

    fn record_candidate_count_limit(&mut self) {
        self.candidate_count_truncated = true;
        self.near_relation_stop_reason
            .get_or_insert(NearRelationStopReason::CandidateCountLimit);
    }

    fn commit_near_search_spend_from(&mut self, other: Self) {
        self.pair_visits = other.pair_visits;
        self.comparisons = other.comparisons;
        self.pair_visits_attempted = other.pair_visits_attempted;
        self.comparisons_attempted = other.comparisons_attempted;
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

#[derive(Clone, Copy)]
struct FragmentVetoBudget {
    pair_visit_limit: usize,
    comparison_limit: usize,
    pair_visits: usize,
    comparisons: usize,
}

impl FragmentVetoBudget {
    fn new(old_tokens: usize, new_tokens: usize) -> Option<Self> {
        let token_limit = old_tokens.checked_add(new_tokens)?;
        let comparison_limit = token_limit.checked_mul(4)?;
        // Fragment veto analysis is isolated so it cannot consume recovery's
        // remaining pair/comparison budget. Each limit is no larger than the
        // corresponding RecoveryBudget limit, so running both analyses can at
        // most double their combined pair/comparison work.
        Some(Self {
            pair_visit_limit: token_limit,
            comparison_limit,
            pair_visits: 0,
            comparisons: 0,
        })
    }

    fn charge_pair_visits(&mut self, amount: usize) -> bool {
        RecoveryBudget::charge(&mut self.pair_visits, amount, self.pair_visit_limit)
    }

    fn charge_comparisons(&mut self, amount: usize) -> bool {
        RecoveryBudget::charge(&mut self.comparisons, amount, self.comparison_limit)
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
        return Ok(SentenceRecoveryBuildOutcome {
            plan: None,
            diagnostics,
            watch_diagnostics: None,
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
    if extend_paired_stream_exact_matches(
        &old_occurrences,
        &new_occurrences,
        &mut exact_match_candidates,
        &paired_streams,
        &membership.recovery_spans,
        input.min_tokens.min(MIN_PAIRED_STREAM_EXACT_TOKENS),
        budget.output_range_limit / 2,
    )
    .is_none()
    {
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
    if watch.as_mut().is_some_and(|watch| {
        watch
            .record_exact_candidates(&exact_match_candidates, &old_occurrences, &new_occurrences)
            .is_none()
    }) {
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
    if append_paired_stream_replacements(
        &mut plan,
        &mut old_occurrences,
        &mut new_occurrences,
        &paired_streams,
        &membership.recovery_spans,
        input.min_tokens.min(MIN_PAIRED_STREAM_NEAR_TOKENS),
        &mut budget,
        &mut diagnostics,
        &mut paired_vetoes,
        watch.as_mut(),
    )
    .is_none()
    {
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
            });
        }
        if watch.is_some() {
            return Ok(SentenceRecoveryBuildOutcome {
                plan: None,
                diagnostics: None,
                watch_diagnostics: watch.map(|watch| watch.finish(None)),
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
    let Some(mut relations) = modified_sentence_relations(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &mut budget,
        &mut diagnostics,
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
            });
        }
        if watch.is_some() {
            return Ok(SentenceRecoveryBuildOutcome {
                plan: None,
                diagnostics: None,
                watch_diagnostics: watch.map(|watch| watch.finish(None)),
            });
        }
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    veto_fragment_completed_replacements(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &old_fragments,
        &new_fragments,
        &mut relations,
        &mut fragment_veto_budget,
    );
    record_vetoed_near_pairs(&mut diagnostics, &relations, near_pair_start);
    if watch.as_mut().is_some_and(|watch| {
        watch
            .record_relations(&old_candidates, &new_candidates, &relations)
            .is_none()
    }) {
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
            });
        }
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    let watch_diagnostics = watch.map(|watch| watch.finish(Some(&plan)));
    Ok(SentenceRecoveryBuildOutcome {
        plan: Some(plan),
        diagnostics,
        watch_diagnostics,
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
    diagnostics.metrics.near_pair_visits_examined = budget.pair_visits;
    diagnostics.metrics.near_pair_visits_attempted = budget.pair_visits_attempted;
    diagnostics.metrics.near_similarity_comparisons_examined = budget.comparisons;
    diagnostics.metrics.near_similarity_comparisons_attempted = budget.comparisons_attempted;
    diagnostics.metrics.near_largest_edge_posting = budget.largest_edge_posting;
    diagnostics.metrics.near_largest_edge_query_union = budget.largest_edge_query_union;
    diagnostics.metrics.near_largest_filtered_candidate_set = budget.largest_filtered_candidate_set;
    diagnostics.metrics.near_candidate_count_truncated = budget.candidate_count_truncated;
    diagnostics.metrics.near_relation_stop_reason = budget.near_relation_stop_reason;
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
            ..SentenceRecoveryMetrics::default()
        },
        eligible_old_source_tokens: eligible_source_tokens(old, alignment, recovery_spans, true)?,
        eligible_new_source_tokens: eligible_source_tokens(new, alignment, recovery_spans, false)?,
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
                (Some(span_index), Some(_)) if recovery_spans.get(span_index).copied()? => {
                    sentence_location(
                        side,
                        &stream,
                        boundary,
                        touched_blocks,
                        span_index,
                        kind,
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
    kind: RecoveryUnitKind,
    budget: &mut RecoveryBudget,
) -> Option<Option<SentenceLocation>> {
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

fn extend_paired_stream_exact_matches<'a>(
    old_occurrences: &'a [SentenceOccurrence],
    new_occurrences: &'a [SentenceOccurrence],
    candidates: &mut Vec<ExactMatchCandidate>,
    pairs: &[PairedTrustedStream],
    recovery_spans: &[bool],
    min_tokens: usize,
    max_candidates: usize,
) -> Option<()> {
    if pairs.is_empty() {
        return Some(());
    }
    candidates
        .try_reserve(max_candidates.checked_sub(candidates.len())?)
        .ok()?;

    let mut old_pair_by_stream = HashMap::new();
    let mut new_pair_by_stream = HashMap::new();
    old_pair_by_stream.try_reserve(pairs.len()).ok()?;
    new_pair_by_stream.try_reserve(pairs.len()).ok()?;
    for (pair_index, pair) in pairs.iter().enumerate() {
        old_pair_by_stream.insert(pair.old_stream, pair_index);
        new_pair_by_stream.insert(pair.new_stream, pair_index);
    }

    let group_limit = max_candidates.checked_mul(2)?;
    let mut groups =
        HashMap::<(usize, usize, &'a str, OccurrenceRole), PairedStreamOccurrences>::new();
    groups.try_reserve(group_limit).ok()?;
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
    selected_old.try_reserve(max_candidates).ok()?;
    selected_new.try_reserve(max_candidates).ok()?;
    for candidate in candidates.iter() {
        selected_old.insert(candidate.old_occurrence_index);
        selected_new.insert(candidate.new_occurrence_index);
    }

    let remaining_candidates = max_candidates.checked_sub(candidates.len())?;
    let mut proposals = Vec::new();
    proposals.try_reserve_exact(remaining_candidates).ok()?;
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
                return None;
            }
            proposals.push(PairedExactProposal {
                pair_index: *pair_index,
                interval_index: *interval_index,
                old_ordinal: old_occurrences.get(old_index)?.trusted_position?.ordinal,
                new_ordinal: new_occurrences.get(new_index)?.trusted_position?.ordinal,
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
        let first = proposals.get(proposal_start)?;
        let proposal_end = proposal_start
            + proposals[proposal_start..].partition_point(|proposal| {
                proposal.pair_index == first.pair_index
                    && proposal.interval_index == first.interval_index
            });
        let interval = proposals.get(proposal_start..proposal_end)?;
        if interval.windows(2).all(|proposals| {
            proposals[0].old_ordinal < proposals[1].old_ordinal
                && proposals[0].new_ordinal < proposals[1].new_ordinal
        }) {
            for proposal in interval {
                candidates.push(ExactMatchCandidate {
                    old_span_index: old_occurrences
                        .get(proposal.old_occurrence_index)?
                        .span_index?,
                    new_span_index: new_occurrences
                        .get(proposal.new_occurrence_index)?
                        .span_index?,
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
    Some(())
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
) -> Option<()> {
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
            || !scope.recovery_spans.get(span_index).copied()?
        {
            continue;
        }
        appended = appended.checked_add(1)?;
        if appended > scope.max_occurrences {
            return None;
        }
        let pair = scope.pairs.get(pair_index)?;
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
        indices.try_reserve(1).ok()?;
        indices.push(occurrence_index);
    }
    Some(())
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
    vetoes: &mut PairedNearVetoes,
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<()> {
    if pairs.is_empty() {
        return Some(());
    }
    let (old_pair_by_stream, new_pair_by_stream) = paired_stream_indices(pairs)?;
    let group_limit = budget.output_range_limit.checked_mul(2)?;
    let mut groups =
        HashMap::<(usize, usize, &'a str, OccurrenceRole), PairedStreamOccurrences>::new();
    groups.try_reserve(group_limit).ok()?;
    append_paired_stream_occurrences(
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
    )?;
    append_paired_stream_occurrences(
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
    )?;

    let old_candidates = paired_near_candidates(
        &groups,
        old_occurrences,
        OccurrenceSide::Old,
        budget.output_range_limit,
    )?;
    let new_candidates = paired_near_candidates(
        &groups,
        new_occurrences,
        OccurrenceSide::New,
        budget.output_range_limit,
    )?;
    if old_candidates.recoveries.is_empty() || new_candidates.recoveries.is_empty() {
        return Some(());
    }

    let near_pair_start = diagnostics
        .as_ref()
        .map_or(0, |diagnostics| diagnostics.metrics.near_pair_candidates);
    let mut relations = paired_modified_sentence_relations(
        old_occurrences,
        new_occurrences,
        &old_candidates,
        &new_candidates,
        pairs,
        &old_pair_by_stream,
        &new_pair_by_stream,
        budget,
        diagnostics,
        watch.as_deref_mut(),
    )?;
    reject_crossing_paired_replacements(&old_candidates, &new_candidates, &mut relations)?;
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
    collect_paired_near_vetoes(&old_candidates, &new_candidates, &relations, vetoes)?;
    append_replacements(
        plan,
        old_occurrences,
        new_occurrences,
        &old_candidates.recoveries,
        &new_candidates.recoveries,
        &relations,
        budget,
    )
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
) {
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
        veto_all_mutual_replacements(relations);
        return;
    };
    if apply_fragment_vetoes(relations, &vetoes).is_none() {
        veto_all_mutual_replacements(relations);
        return;
    }
    *budget = tentative_budget;
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
    proposals.try_reserve_exact(relations.old.len()).ok()?;
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
    vetoes.try_reserve_exact(proposals.len()).ok()?;
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
                uncertain_spans.try_reserve(1).ok()?;
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
            clean_by_span_and_len.try_reserve(1).ok()?;
            clean_by_span_and_len.insert(key, Vec::new());
        }
        let bucket = clean_by_span_and_len.get_mut(&key)?;
        bucket.try_reserve(1).ok()?;
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
) -> Option<()> {
    vetoes
        .old_occurrences
        .try_reserve_exact(relations.old.len())
        .ok()?;
    vetoes
        .new_occurrences
        .try_reserve_exact(relations.new.len())
        .ok()?;
    for (index, relation) in relations.old.iter().enumerate() {
        if relation.vetoed() {
            vetoes
                .old_occurrences
                .push(old_candidates.recoveries.get(index)?.occurrence_index);
        }
    }
    for (index, relation) in relations.new.iter().enumerate() {
        if relation.vetoed() {
            vetoes
                .new_occurrences
                .push(new_candidates.recoveries.get(index)?.occurrence_index);
        }
    }
    vetoes.old_occurrences.sort_unstable();
    vetoes.old_occurrences.dedup();
    vetoes.new_occurrences.sort_unstable();
    vetoes.new_occurrences.dedup();
    Some(())
}

fn paired_stream_indices(
    pairs: &[PairedTrustedStream],
) -> Option<(HashMap<usize, usize>, HashMap<usize, usize>)> {
    let mut old = HashMap::new();
    let mut new = HashMap::new();
    old.try_reserve(pairs.len()).ok()?;
    new.try_reserve(pairs.len()).ok()?;
    for (pair_index, pair) in pairs.iter().enumerate() {
        if old.insert(pair.old_stream, pair_index).is_some()
            || new.insert(pair.new_stream, pair_index).is_some()
        {
            return None;
        }
    }
    Some((old, new))
}

fn paired_near_candidates(
    groups: &HashMap<(usize, usize, &str, OccurrenceRole), PairedStreamOccurrences>,
    occurrences: &[SentenceOccurrence],
    side: OccurrenceSide,
    max_candidates: usize,
) -> Option<PairedNearCandidates> {
    let mut candidates = PairedNearCandidates::default();
    candidates
        .recoveries
        .try_reserve_exact(max_candidates)
        .ok()?;
    candidates
        .intervals
        .try_reserve_exact(max_candidates)
        .ok()?;
    candidates.ordinals.try_reserve_exact(max_candidates).ok()?;
    for ((pair_index, interval_index, _, _), group) in groups {
        let (own, other) = match side {
            OccurrenceSide::Old => (&group.old, &group.new),
            OccurrenceSide::New => (&group.new, &group.old),
        };
        if own.len() != 1 || !other.is_empty() {
            continue;
        }
        if candidates.recoveries.len() == max_candidates {
            return None;
        }
        let occurrence_index = own[0];
        let occurrence = occurrences.get(occurrence_index)?;
        candidates.recoveries.push(RecoveryCandidate {
            occurrence_index,
            span_index: occurrence.span_index?,
        });
        candidates.intervals.push(PairedInterval {
            pair_index: *pair_index,
            interval_index: *interval_index,
        });
        candidates
            .ordinals
            .push(occurrence.trusted_position?.ordinal);
    }
    let mut order = Vec::new();
    order.try_reserve_exact(candidates.recoveries.len()).ok()?;
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
) -> Option<PairedNearCandidates> {
    let mut sorted = PairedNearCandidates::default();
    sorted.recoveries.try_reserve_exact(order.len()).ok()?;
    sorted.intervals.try_reserve_exact(order.len()).ok()?;
    sorted.ordinals.try_reserve_exact(order.len()).ok()?;
    for index in order {
        sorted.recoveries.push(RecoveryCandidate {
            occurrence_index: candidates.recoveries.get(*index)?.occurrence_index,
            span_index: candidates.recoveries.get(*index)?.span_index,
        });
        sorted.intervals.push(*candidates.intervals.get(*index)?);
        sorted.ordinals.push(*candidates.ordinals.get(*index)?);
    }
    Some(sorted)
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
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<ModifiedSentenceRelations> {
    let mut relations =
        empty_modified_sentence_relations(&old_candidates.recoveries, &new_candidates.recoveries)?;
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
    let old_index = UnitCandidateIndex::new(old_occurrences)?;
    let new_index = UnitCandidateIndex::new(new_occurrences)?;
    let mut plausible = Vec::new();

    for (old_candidate_index, old_candidate) in old_candidates.recoveries.iter().enumerate() {
        let interval = *old_candidates.intervals.get(old_candidate_index)?;
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        let query = new_index.collect_plausible_occurrences(&mut plausible, old_occurrence)?;
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
                    .is_some_and(|candidate| candidate.pair_index == interval.pair_index)
        });
        budget.record_candidate_query(query, plausible.len());
        if !budget.charge_pair_visits(plausible.len()) {
            return None;
        }
        for &new_occurrence_index in &plausible {
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
            if let Some(watch) = watch.as_mut() {
                watch.record_near(
                    old_candidate.occurrence_index,
                    new_occurrence_index,
                    score,
                    RecoveryWatchNearScope::PairedStream,
                );
            }
            if let Some(new_candidate_index) = new_candidate_by_occurrence[new_occurrence_index]
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
        let query = old_index.collect_plausible_occurrences(&mut plausible, new_occurrence)?;
        plausible.retain(|occurrence_index| {
            old_occurrences[*occurrence_index].kind == new_occurrence.kind
                && occurrence_roles_are_compatible(
                    new_occurrence,
                    &old_occurrences[*occurrence_index],
                )
                && old_intervals
                    .get(*occurrence_index)
                    .copied()
                    .flatten()
                    .is_some_and(|candidate| candidate.pair_index == interval.pair_index)
        });
        let noncandidate_visits =
            plausible
                .iter()
                .try_fold(0usize, |count, occurrence_index| {
                    if old_candidate_by_occurrence
                        .get(*occurrence_index)?
                        .is_none()
                    {
                        count.checked_add(1)
                    } else {
                        Some(count)
                    }
                })?;
        budget.record_candidate_query(query, plausible.len());
        if !budget.charge_pair_visits(noncandidate_visits) {
            return None;
        }
        for &old_occurrence_index in &plausible {
            if old_candidate_by_occurrence[old_occurrence_index].is_some() {
                continue;
            }
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
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
            let interval_index = pair.anchors.partition_point(|(old, new)| {
                let anchor_ordinal = match side {
                    OccurrenceSide::Old => *old,
                    OccurrenceSide::New => *new,
                };
                anchor_ordinal < position.ordinal
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

fn reject_crossing_paired_replacements(
    old_candidates: &PairedNearCandidates,
    new_candidates: &PairedNearCandidates,
    relations: &mut ModifiedSentenceRelations,
) -> Option<()> {
    let mut proposals = Vec::new();
    proposals.try_reserve_exact(relations.old.len()).ok()?;
    for (old_index, relation) in relations.old.iter().copied().enumerate() {
        let Some(new_index) = mutual_replacement_partner(old_index, relation, &relations.new)
        else {
            continue;
        };
        let interval = *old_candidates.intervals.get(old_index)?;
        if new_candidates.intervals.get(new_index).copied()? != interval {
            return None;
        }
        proposals.push((
            interval,
            *old_candidates.ordinals.get(old_index)?,
            *new_candidates.ordinals.get(new_index)?,
            old_index,
            new_index,
        ));
    }
    proposals.sort_unstable();
    let mut start = 0usize;
    while start < proposals.len() {
        let interval = proposals[start].0;
        let end = start + proposals[start..].partition_point(|proposal| proposal.0 == interval);
        if !proposals[start..end]
            .windows(2)
            .all(|pair| pair[0].1 < pair[1].1 && pair[0].2 < pair[1].2)
        {
            for proposal in &proposals[start..end] {
                let old_score = relations.old[proposal.3].best_score;
                let new_score = relations.new[proposal.4].best_score;
                relations.old[proposal.3].record_disqualifying(old_score);
                relations.new[proposal.4].record_disqualifying(new_score);
            }
        }
        start = end;
    }
    Some(())
}

fn modified_sentence_relations(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<ModifiedSentenceRelations> {
    let mut relations = empty_modified_sentence_relations(old_candidates, new_candidates)?;
    extend_modified_sentence_relations(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        NearRelationScope::SameOrAmbiguous,
        &mut relations,
        budget,
        diagnostics,
        watch.as_deref_mut(),
    )?;

    let Some(mut cross_relations) = try_clone_modified_sentence_relations(&relations) else {
        return Some(relations);
    };
    let mut cross_budget = *budget;
    let mut cross_diagnostics = *diagnostics;
    if extend_modified_sentence_relations(
        old_occurrences,
        new_occurrences,
        old_candidates,
        new_candidates,
        NearRelationScope::CrossSpan,
        &mut cross_relations,
        &mut cross_budget,
        &mut cross_diagnostics,
        watch,
    )
    .is_some()
    {
        cross_relations.complete = true;
        *budget = cross_budget;
        *diagnostics = cross_diagnostics;
        return Some(cross_relations);
    }

    budget.commit_near_search_spend_from(cross_budget);
    Some(relations)
}

#[derive(Clone, Copy)]
enum NearRelationScope {
    SameOrAmbiguous,
    CrossSpan,
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
    })
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
    Some(ModifiedSentenceRelations {
        old,
        new,
        complete: relations.complete,
    })
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
    mut watch: Option<&mut RecoveryWatchState>,
) -> Option<()> {
    if relations.old.len() != old_candidates.len() || relations.new.len() != new_candidates.len() {
        return None;
    }
    let old_candidate_by_occurrence =
        candidate_index_by_occurrence(old_occurrences.len(), old_candidates)?;
    let new_candidate_by_occurrence =
        candidate_index_by_occurrence(new_occurrences.len(), new_candidates)?;
    let old_index = UnitCandidateIndex::new(old_occurrences)?;
    let new_index = UnitCandidateIndex::new(new_occurrences)?;
    let mut plausible = Vec::new();

    for (old_candidate_index, old_candidate) in old_candidates.iter().enumerate() {
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        let query = new_index.collect_plausible_occurrences(&mut plausible, old_occurrence)?;
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
        budget.record_candidate_query(query, plausible.len());
        if !budget.charge_pair_visits(plausible.len()) {
            return None;
        }
        for &new_occurrence_index in &plausible {
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
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
            if let Some(new_candidate_index) = new_candidate_by_occurrence[new_occurrence_index] {
                relations.old[old_candidate_index].record_eligible(new_candidate_index, score);
                relations.new[new_candidate_index].record_eligible(old_candidate_index, score);
            } else {
                relations.old[old_candidate_index].record_disqualifying(score);
            }
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }

    for (new_candidate_index, new_candidate) in new_candidates.iter().enumerate() {
        let new_occurrence = new_occurrences.get(new_candidate.occurrence_index)?;
        let query = old_index.collect_plausible_occurrences(&mut plausible, new_occurrence)?;
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
        let noncandidate_visits =
            plausible
                .iter()
                .try_fold(0usize, |count, occurrence_index| {
                    if old_candidate_by_occurrence
                        .get(*occurrence_index)?
                        .is_none()
                    {
                        count.checked_add(1)
                    } else {
                        Some(count)
                    }
                })?;
        budget.record_candidate_query(query, plausible.len());
        if !budget.charge_pair_visits(noncandidate_visits) {
            return None;
        }
        for &old_occurrence_index in &plausible {
            if old_candidate_by_occurrence[old_occurrence_index].is_some() {
                continue;
            }
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
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
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }
    Some(())
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

fn sentence_similarity(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    budget: &mut RecoveryBudget,
) -> Option<u16> {
    let shorter = old.tokens.len().min(new.tokens.len());
    if shorter == 0 {
        return Some(0);
    }
    if old.kind == RecoveryUnitKind::Line
        && new.kind == RecoveryUnitKind::Line
        && old.tokens.len().max(new.tokens.len())
            > shorter.checked_mul(MAX_LINE_NEAR_LENGTH_RATIO)?
    {
        return Some(0);
    }

    let mut prefix = 0usize;
    while prefix < shorter {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if old.tokens[prefix] != new.tokens[prefix] {
            break;
        }
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < shorter - prefix {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if old.tokens[old.tokens.len() - suffix - 1] != new.tokens[new.tokens.len() - suffix - 1] {
            break;
        }
        suffix += 1;
    }

    let shared = prefix.checked_add(suffix)?;
    let edge_score = basis_points(shared, shorter)?;
    let mut exact_score = edge_score;
    if old.kind == RecoveryUnitKind::Line && new.kind == RecoveryUnitKind::Line {
        let old_ngrams = ngram_count(old.tokens.len(), LINE_NGRAM_SIZE)?;
        let new_ngrams = ngram_count(new.tokens.len(), LINE_NGRAM_SIZE)?;
        if multiset_dice_upper_bound(old_ngrams, new_ngrams)? > exact_score {
            let line_score =
                token_ngram_multiset_dice(&old.tokens, &new.tokens, LINE_NGRAM_SIZE, budget)?;
            exact_score = exact_score.max(line_score);
        }
    }
    if edge_score < MIN_WORD_SCORE_EDGE_EVIDENCE {
        return Some(exact_score);
    }
    if multiset_dice_upper_bound(old.word_ranges.len(), new.word_ranges.len())? > exact_score {
        let word_score = word_multiset_dice(old, new, budget)?;
        exact_score = exact_score.max(word_score);
    }
    Some(exact_score)
}

fn multiset_dice_upper_bound(old_count: usize, new_count: usize) -> Option<u16> {
    let total = old_count.checked_add(new_count)?;
    basis_points(old_count.min(new_count).checked_mul(2)?, total)
}

fn word_multiset_dice(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    budget: &mut RecoveryBudget,
) -> Option<u16> {
    let total = old.word_ranges.len().checked_add(new.word_ranges.len())?;
    if total == 0 {
        return Some(0);
    }
    let mut old_index = 0usize;
    let mut new_index = 0usize;
    let mut shared = 0usize;
    while old_index < old.word_ranges.len() && new_index < new.word_ranges.len() {
        if !budget.charge_comparisons(1) {
            return None;
        }
        let old_word = old.key.get(old.word_ranges[old_index].clone())?;
        let new_word = new.key.get(new.word_ranges[new_index].clone())?;
        match old_word.cmp(new_word) {
            std::cmp::Ordering::Less => old_index += 1,
            std::cmp::Ordering::Greater => new_index += 1,
            std::cmp::Ordering::Equal => {
                shared = shared.checked_add(1)?;
                old_index += 1;
                new_index += 1;
            }
        }
    }
    basis_points(shared.checked_mul(2)?, total)
}

fn token_ngram_multiset_dice(
    old: &[SentenceEvidenceToken],
    new: &[SentenceEvidenceToken],
    size: usize,
    budget: &mut RecoveryBudget,
) -> Option<u16> {
    let old_windows = ngram_count(old.len(), size)?;
    let new_windows = ngram_count(new.len(), size)?;
    if old_windows == 0 || new_windows == 0 {
        return Some(0);
    }
    let total = old_windows.checked_add(new_windows)?;
    if !budget.charge_comparisons(total) {
        return None;
    }

    let mut old_counts = HashMap::<&[SentenceEvidenceToken], usize>::new();
    let mut new_counts = HashMap::<&[SentenceEvidenceToken], usize>::new();
    old_counts.try_reserve(old_windows).ok()?;
    new_counts.try_reserve(new_windows).ok()?;
    for ngram in old.windows(size) {
        let count = old_counts.entry(ngram).or_default();
        *count = count.checked_add(1)?;
    }
    for ngram in new.windows(size) {
        let count = new_counts.entry(ngram).or_default();
        *count = count.checked_add(1)?;
    }
    let (shorter, longer) = if old_counts.len() <= new_counts.len() {
        (&old_counts, &new_counts)
    } else {
        (&new_counts, &old_counts)
    };
    if !budget.charge_comparisons(shorter.len()) {
        return None;
    }
    let shared = shorter.iter().try_fold(0usize, |shared, (ngram, count)| {
        shared.checked_add((*count).min(longer.get(ngram).copied().unwrap_or(0)))
    })?;
    basis_points(shared.checked_mul(2)?, total)
}

fn ngram_count(token_count: usize, size: usize) -> Option<usize> {
    if size == 0 || token_count < size {
        return Some(0);
    }
    token_count.checked_sub(size)?.checked_add(1)
}

fn basis_points(numerator: usize, denominator: usize) -> Option<u16> {
    if denominator == 0 {
        return Some(0);
    }
    let score = numerator.checked_mul(10_000)?.checked_div(denominator)?;
    u16::try_from(score).ok()
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
    if old_candidates.len() != relations.old.len() || new_candidates.len() != relations.new.len() {
        return None;
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
        let old_candidate = old_candidates.get(old_candidate_index)?;
        let new_candidate = new_candidates.get(new_candidate_index)?;
        let old_location = old_occurrences
            .get(old_candidate.occurrence_index)?
            .location
            .as_ref()?;
        let new_location = new_occurrences
            .get(new_candidate.occurrence_index)?
            .location
            .as_ref()?;
        replacement_count = replacement_count.checked_add(1)?;
        source_tokens = source_tokens
            .checked_add(old_location.recovery.source_tokens)?
            .checked_add(new_location.recovery.source_tokens)?;
        old_consumed_count = old_consumed_count.checked_add(old_location.consumed.len())?;
        new_consumed_count = new_consumed_count.checked_add(new_location.consumed.len())?;
    }

    let output_ranges = replacement_count.checked_mul(2)?;
    if !budget.charge_outputs(output_ranges, source_tokens) {
        return None;
    }
    plan.replacements
        .try_reserve_exact(replacement_count)
        .ok()?;
    plan.cross_span_replacement_new_spans
        .try_reserve_exact(replacement_count)
        .ok()?;
    plan.deletion_consumed
        .try_reserve_exact(old_consumed_count)
        .ok()?;
    plan.insertion_consumed
        .try_reserve_exact(new_consumed_count)
        .ok()?;

    for (old_candidate_index, old_relation) in relations.old.iter().copied().enumerate() {
        let Some(new_candidate_index) =
            mutual_replacement_partner(old_candidate_index, old_relation, &relations.new)
        else {
            continue;
        };
        let old_occurrence_index = old_candidates.get(old_candidate_index)?.occurrence_index;
        let new_occurrence_index = new_candidates.get(new_candidate_index)?.occurrence_index;
        let old_location = old_occurrences
            .get_mut(old_occurrence_index)?
            .location
            .take()?;
        let new_location = new_occurrences
            .get_mut(new_occurrence_index)?
            .location
            .take()?;
        plan.replacements.push(RecoveredReplacement {
            old: old_location.recovery,
            new: new_location.recovery,
        });
        let replacement = plan.replacements.last()?;
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
    Some(())
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
        if occurrence.kind == RecoveryUnitKind::Line {
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
        if occurrence.kind == RecoveryUnitKind::Line {
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
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
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
                },
                old_occurrence: Some(0),
                new_occurrence: Some(1),
                old_segment: None,
                new_segment: None,
            }],
            pair_by_occurrences: HashMap::new(),
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
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
            scan_work: 0,
            scan_limit: 100_000,
            retained_one_sided_occurrences: 0,
        }
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
            records: Vec::new(),
            pair_by_occurrences: HashMap::new(),
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

    fn extend_test_paired_exact_matches(
        old: &[SentenceOccurrence],
        new: &[SentenceOccurrence],
        candidates: &mut Vec<ExactMatchCandidate>,
        max_candidates: usize,
    ) -> Option<()> {
        let pairs = paired_trusted_streams(old, new, candidates)?;
        extend_paired_stream_exact_matches(old, new, candidates, &pairs, &[true], 5, max_candidates)
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

        assert_eq!(sentence_similarity(&old, &new, &mut budget), Some(7_323));

        let tiny = occurrence("a");
        let mut budget = RecoveryBudget::new(
            old.tokens.len(),
            tiny.tokens.len(),
            old.tokens.len() + tiny.tokens.len(),
            1,
        )
        .expect("length guard budget is valid");
        assert_eq!(sentence_similarity(&old, &tiny, &mut budget), Some(0));
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
            sentence_similarity(&old, &same, &mut equality_budget),
            Some(10_000)
        );
        assert_eq!(equality_budget.comparisons, 4);

        let longer = occurrence("abXcd");
        let mut unequal_budget = RecoveryBudget::new(4, 5, 9, 1).expect("valid budget");
        assert_eq!(
            sentence_similarity(&old, &longer, &mut unequal_budget),
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

        assert_eq!(sentence_similarity(&old, &new, &mut budget), Some(9_999));
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

        assert_eq!(sentence_similarity(&old, &new, &mut budget), Some(5_000));
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

        assert_eq!(sentence_similarity(&old, &new, &mut budget), Some(10_000));
        assert_eq!(budget.comparisons, 10_002);
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
        assert!(budget.charge_pair_visits(5));
        assert!(!budget.charge_pair_visits(1));
        assert!(budget.charge_comparisons(20));
        assert!(!budget.charge_comparisons(1));
        assert_eq!(budget.pair_visits, 5);
        assert_eq!(budget.pair_visits_attempted, 6);
        assert_eq!(budget.comparisons, 20);
        assert_eq!(budget.comparisons_attempted, 21);
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
        assert!(fragment.charge_comparisons(20));
        assert!(!fragment.charge_comparisons(1));
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
        assert_eq!(budget.pair_visits, before.pair_visits);
        assert_eq!(budget.comparisons, before.comparisons);

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
        assert_eq!(budget.pair_visits, before.pair_visits);
        assert_eq!(budget.comparisons, before.comparisons);
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
        assert_eq!(budget.pair_visits, before.pair_visits);
        assert_eq!(budget.comparisons, before.comparisons);
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
                RecoveryUnitKind::Sentence,
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
                RecoveryUnitKind::Sentence,
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
        let index = UnitCandidateIndex::new(&occurrences).expect("index construction succeeds");
        let mut plausible = Vec::new();

        index
            .collect_plausible_occurrences(&mut plausible, &occurrences[0])
            .expect("query succeeds");

        assert_eq!(plausible, vec![0]);
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
        let index = UnitCandidateIndex::new(&occurrences).expect("index construction succeeds");
        let mut plausible = Vec::new();

        index
            .collect_plausible_occurrences(&mut plausible, &query)
            .expect("query succeeds");

        assert_eq!(plausible, vec![0, 1]);
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
        let index = UnitCandidateIndex::new(&occurrences).expect("index construction succeeds");
        let mut plausible = Vec::new();
        let query_metrics = index
            .collect_plausible_occurrences(&mut plausible, &query)
            .expect("query succeeds");
        let mut budget = RecoveryBudget::new(8, 0, 8, 1).expect("budget is valid");
        budget.record_candidate_query(query_metrics, 2);
        assert!(budget.charge_pair_visits(2));
        assert!(budget.charge_comparisons(3));
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
        });

        record_near_search_metrics(&mut diagnostics, &budget);

        let metrics = diagnostics.expect("metrics remain available").metrics;
        assert_eq!(metrics.near_pair_visits_examined, 2);
        assert_eq!(metrics.near_pair_visits_attempted, 2);
        assert_eq!(metrics.near_similarity_comparisons_examined, 3);
        assert_eq!(metrics.near_similarity_comparisons_attempted, 3);
        assert_eq!(metrics.near_largest_edge_posting, 2);
        assert_eq!(metrics.near_largest_edge_query_union, 3);
        assert_eq!(metrics.near_largest_filtered_candidate_set, 2);
        assert_eq!(metrics.near_relation_stop_reason, None);
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
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("relations fit within budget");

        assert_eq!(budget.pair_visits, 3);
        assert_eq!(budget.largest_filtered_candidate_set, 3);
    }

    #[test]
    fn comparison_limit_records_first_stop_reason_without_examining_failed_work() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.record_candidate_count_limit();
        assert!(budget.charge_comparisons(4));
        assert!(!budget.charge_comparisons(2));

        assert_eq!(budget.comparisons, 4);
        assert_eq!(budget.comparisons_attempted, 6);
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::SimilarityComparisonLimit)
        );
    }

    #[test]
    fn near_search_counter_overflow_makes_metrics_unavailable_without_a_limit_reason() {
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("budget is valid");
        budget.pair_visits_attempted = usize::MAX;
        assert!(!budget.charge_pair_visits(1));
        let mut diagnostics = Some(SentenceRecoveryDiagnostics {
            metrics: SentenceRecoveryMetrics::default(),
            eligible_old_source_tokens: 0,
            eligible_new_source_tokens: 0,
        });

        record_near_search_metrics(&mut diagnostics, &budget);

        assert!(diagnostics.is_none());
        assert_eq!(budget.near_relation_stop_reason, None);
    }

    #[test]
    fn unit_candidate_index_deduplicates_equal_edge_postings() {
        let occurrences = [indexed_occurrence(
            &['a'],
            RecoveryUnitKind::Sentence,
            Some(BlockRole::Body),
        )];
        let index = UnitCandidateIndex::new(&occurrences).expect("index construction succeeds");
        let mut plausible = Vec::new();

        index
            .collect_plausible_occurrences(&mut plausible, &occurrences[0])
            .expect("query succeeds");

        assert_eq!(plausible, vec![0]);
    }

    #[test]
    fn unit_candidate_index_reserves_map_capacity_by_distinct_edge_key() {
        let occurrences = (0..256)
            .map(|_| indexed_occurrence(&['a'], RecoveryUnitKind::Sentence, Some(BlockRole::Body)))
            .collect::<Vec<_>>();

        let index = UnitCandidateIndex::new(&occurrences).expect("index construction succeeds");

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
        let index = UnitCandidateIndex::new(&occurrences).expect("index construction succeeds");
        let mut plausible = Vec::new();

        index
            .collect_plausible_occurrences(&mut plausible, &occurrences[1])
            .expect("role-bearing query succeeds");
        assert_eq!(plausible, vec![1]);

        index
            .collect_plausible_occurrences(&mut plausible, &occurrences[0])
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
        let mut budget = RecoveryBudget::new(3, 0, 3, 1).expect("test budget is valid");
        budget.record_candidate_count_limit();
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("same-span relations survive optional cross-span exhaustion");

        assert!(!relations.complete);
        assert_eq!(relations.old[0].unique_partner(), Some(0));
        assert_eq!(relations.old[1].unique_partner(), Some(1));
        assert_eq!(budget.pair_visits, 3);
        assert_eq!(budget.pair_visits_attempted, 4);
        assert_eq!(budget.comparisons, 3);
        assert_eq!(budget.comparisons_attempted, 3);
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::PairVisitLimit)
        );
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
        let mut budget = RecoveryBudget::new(8, 0, 8, 1).expect("test budget is valid");
        budget.record_candidate_count_limit();
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &candidates,
            &candidates,
            &mut budget,
            &mut diagnostics,
            None,
        )
        .expect("cross-span relations complete within budget");

        assert!(relations.complete);
        assert_eq!(budget.pair_visits, budget.pair_visits_attempted);
        assert_eq!(budget.comparisons, budget.comparisons_attempted);
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidateCountLimit)
        );
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
