use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Result,
    alignment::{Alignment, AlignmentEvidence, AlignmentKind, BlockSeparator},
    layout::{BlockId, TrustedRunId, TrustedRunInterval},
    normalize::{ComparableToken, ScalarRange},
};

use super::{
    MAX_SENTENCE_RECOVERY_OUTPUT_BYTES, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS,
    SentenceRecoveryCommittedTokens, SentenceRecoveryInput, SentenceRecoveryMetrics, Side,
    TokenRange,
};

pub(super) const MAX_SENTENCE_RECOVERY_RANGES: usize = 4_096;
const MIN_NEAR_SCORE: u16 = 7_000;
const MIN_NEAR_SCORE_MARGIN: u16 = 500;
const MIN_WORD_SCORE_EDGE_EVIDENCE: u16 = 3_000;
const MIN_PAIRED_STREAM_EXACT_TOKENS: usize = 4;
const MIN_PAIRED_STREAM_NEAR_TOKENS: usize = 4;
const LINE_NGRAM_SIZE: usize = 3;
const MAX_LINE_NEAR_LENGTH_RATIO: usize = 3;
const MAX_UNTRUSTED_LINE_NEAR_CANDIDATES: usize = 256;
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

    pub(super) fn finish_metrics(self) -> Option<SentenceRecoveryMetrics> {
        let diagnostics = self.diagnostics?;
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
    location: Option<SentenceLocation>,
    span_index: Option<usize>,
    trusted_position: Option<TrustedStreamPosition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RecoveryUnitKind {
    Sentence,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

type OccurrenceKey<'a> = (&'a str, RecoveryUnitKind);

struct RecoveryCandidate {
    occurrence_index: usize,
    span_index: usize,
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

#[derive(Clone, Copy, Default)]
struct CandidateNearRelation {
    best_score: u16,
    second_score: u16,
    best_partner: Option<usize>,
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
}

struct ModifiedSentenceRelations {
    old: Vec<CandidateNearRelation>,
    new: Vec<CandidateNearRelation>,
    complete: bool,
}

struct StreamPlan {
    block_indices: Vec<usize>,
    trusted: bool,
}

#[derive(Clone, Copy)]
struct IntervalBlock {
    block_index: usize,
    start: usize,
    end: usize,
}

enum StreamPlanGroup {
    Trusted(Vec<IntervalBlock>),
    Untrusted(usize),
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
        Self::charge(&mut self.pair_visits, amount, self.token_limit)
    }

    fn charge_comparisons(&mut self, amount: usize) -> bool {
        Self::charge(&mut self.comparisons, amount, self.comparison_limit)
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
}

pub(super) fn build_sentence_recovery_plan(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
    input: SentenceRecoveryInput<'_>,
    max_tokens: usize,
) -> Result<SentenceRecoveryBuildOutcome> {
    let Some(mut budget) = RecoveryBudget::new(
        old.total_tokens,
        new.total_tokens,
        max_tokens,
        input.min_tokens,
    ) else {
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
    if !membership.recovery_spans.iter().any(|eligible| *eligible) {
        return Ok(SentenceRecoveryBuildOutcome {
            plan: None,
            diagnostics,
        });
    }

    let Some(mut old_occurrences) = collect_occurrences(
        old,
        input.old_trusted_run_intervals,
        &membership.old,
        &membership.recovery_spans,
        &mut budget,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(mut new_occurrences) = collect_occurrences(
        new,
        input.new_trusted_run_intervals,
        &membership.new,
        &membership.recovery_spans,
        &mut budget,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
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
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    let Some(paired_streams) =
        paired_trusted_streams(&old_occurrences, &new_occurrences, &exact_match_candidates)
    else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(mut old_candidates) = recovery_candidates(
        &old_occurrences,
        &counts,
        OccurrenceSide::Old,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(mut new_candidates) = recovery_candidates(
        &new_occurrences,
        &counts,
        OccurrenceSide::New,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
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
    )
    .is_none()
    {
        if plan.has_exact_matches()
            && normalize_ranges(&mut plan.deletion_consumed)
            && normalize_ranges(&mut plan.insertion_consumed)
        {
            return Ok(SentenceRecoveryBuildOutcome {
                plan: Some(plan),
                diagnostics,
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
    let Some(relations) = modified_sentence_relations(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &mut budget,
        &mut diagnostics,
    ) else {
        if plan.has_exact_matches()
            && normalize_ranges(&mut plan.deletion_consumed)
            && normalize_ranges(&mut plan.insertion_consumed)
        {
            return Ok(SentenceRecoveryBuildOutcome {
                plan: Some(plan),
                diagnostics,
            });
        }
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    record_vetoed_near_pairs(&mut diagnostics, &relations, near_pair_start);
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.metrics.near_relation_complete = relations.complete;
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
        return Ok(SentenceRecoveryBuildOutcome::default());
    }
    Ok(SentenceRecoveryBuildOutcome {
        plan: Some(plan),
        diagnostics,
    })
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
    span_by_block: &HashMap<BlockId, usize>,
    recovery_spans: &[bool],
    budget: &mut RecoveryBudget,
) -> Option<Vec<SentenceOccurrence>> {
    let plans = stream_plans(trusted_run_intervals)?;
    let mut occurrences = Vec::new();
    for (stream_index, plan) in plans.into_iter().enumerate() {
        let stream = build_stream(side, &plan)?;
        let sentence_boundaries =
            sentence_boundaries(&stream.text, &stream.forced_sentence_boundaries, budget)?;
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
            let tokens = sentence_tokens(&stream, boundary, budget)?;
            let span_index =
                sentence_span_index(side, &stream, touched_blocks.clone(), span_by_block)?;
            let location = match span_index {
                Some(span_index) if recovery_spans.get(span_index).copied()? => sentence_location(
                    side,
                    &stream,
                    boundary,
                    touched_blocks,
                    span_index,
                    kind,
                    budget,
                )?,
                Some(_) | None => None,
            };
            occurrences.try_reserve(1).ok()?;
            occurrences.push(SentenceOccurrence {
                key: owned_key,
                tokens,
                word_ranges,
                kind,
                location,
                span_index,
                trusted_position: stream.trusted.then_some(TrustedStreamPosition {
                    stream_index,
                    ordinal,
                }),
            });
        }
    }
    occurrences.sort_unstable_by_key(|occurrence| occurrence.span_index);
    Some(occurrences)
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
                    let StreamPlanGroup::Trusted(blocks) = groups.get_mut(position)? else {
                        return None;
                    };
                    blocks.try_reserve(1).ok()?;
                    blocks.push(block);
                } else {
                    let mut blocks = Vec::new();
                    blocks.try_reserve(1).ok()?;
                    blocks.push(block);
                    trusted_positions.insert(interval.run_id, groups.len());
                    groups.push(StreamPlanGroup::Trusted(blocks));
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
                });
            }
            StreamPlanGroup::Trusted(mut blocks) => {
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
    let mut forced_sentence_boundaries = Vec::new();
    text.try_reserve_exact(text_capacity).ok()?;
    tokens.try_reserve_exact(token_capacity).ok()?;
    blocks.try_reserve_exact(plan.block_indices.len()).ok()?;
    let mut scalar_count = 0usize;
    let mut previous_ended_terminal = false;

    for (position, side_index) in plan.block_indices.iter().copied().enumerate() {
        let block = side.blocks.get(side_index)?;
        let next = side.canonical.get(side_index)?;
        if plan.trusted && previous_ended_terminal {
            forced_sentence_boundaries.try_reserve(1).ok()?;
            forced_sentence_boundaries.push(ForcedSentenceBoundary {
                byte_offset: text.len(),
                scalar_offset: scalar_count,
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
        let count = counts
            .entry((occurrence.key.as_str(), occurrence.kind))
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
            let count = counts.get(&(old.key.as_str(), old.kind))?;
            if count.old != 1 || count.new != 1 || count.old_index != Some(old_occurrence_index) {
                continue;
            }
            let new_occurrence_index = count.new_index?;
            let new = new_occurrences.get(new_occurrence_index)?;
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
    let mut groups = HashMap::<(usize, usize, &'a str), PairedStreamOccurrences>::new();
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
    for ((pair_index, interval_index, _), group) in groups.iter_mut() {
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
    groups: &mut HashMap<(usize, usize, &'a str), PairedStreamOccurrences>,
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
            .entry((pair_index, interval_index, occurrence.key.as_str()))
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
) -> Option<()> {
    if pairs.is_empty() {
        return Some(());
    }
    let (old_pair_by_stream, new_pair_by_stream) = paired_stream_indices(pairs)?;
    let group_limit = budget.output_range_limit.checked_mul(2)?;
    let mut groups = HashMap::<(usize, usize, &'a str), PairedStreamOccurrences>::new();
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
    )?;
    reject_crossing_paired_replacements(&old_candidates, &new_candidates, &mut relations)?;
    record_vetoed_near_pairs(diagnostics, &relations, near_pair_start);
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
    groups: &HashMap<(usize, usize, &str), PairedStreamOccurrences>,
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
    for ((pair_index, interval_index, _), group) in groups {
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
) -> Option<Vec<RecoveryCandidate>> {
    let mut candidates = Vec::new();
    candidates.try_reserve(occurrences.len()).ok()?;
    let mut line_candidates = 0usize;
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
        let count = counts.get(&(occurrence.key.as_str(), occurrence.kind))?;
        let unique = match side {
            OccurrenceSide::Old => count.old == 1 && count.new == 0,
            OccurrenceSide::New => count.new == 1 && count.old == 0,
        };
        if unique {
            if occurrence.kind == RecoveryUnitKind::Line {
                if line_candidates == MAX_UNTRUSTED_LINE_NEAR_CANDIDATES {
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
    Some(candidates)
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
    let old_edges = occurrence_edge_index(old_occurrences)?;
    let new_edges = occurrence_edge_index(new_occurrences)?;
    let mut plausible = Vec::new();

    for (old_candidate_index, old_candidate) in old_candidates.recoveries.iter().enumerate() {
        let interval = *old_candidates.intervals.get(old_candidate_index)?;
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        collect_plausible_occurrences(&mut plausible, &new_edges, &old_occurrence.tokens)?;
        plausible.retain(|occurrence_index| {
            new_occurrences[*occurrence_index].kind == old_occurrence.kind
                && new_intervals
                    .get(*occurrence_index)
                    .copied()
                    .flatten()
                    .is_some_and(|candidate| candidate.pair_index == interval.pair_index)
        });
        if !budget.charge_pair_visits(plausible.len()) {
            return None;
        }
        for &new_occurrence_index in &plausible {
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
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
        collect_plausible_occurrences(&mut plausible, &old_edges, &new_occurrence.tokens)?;
        plausible.retain(|occurrence_index| {
            old_occurrences[*occurrence_index].kind == new_occurrence.kind
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
        if !budget.charge_pair_visits(noncandidate_visits) {
            return None;
        }
        for &old_occurrence_index in &plausible {
            if old_candidate_by_occurrence[old_occurrence_index].is_some() {
                continue;
            }
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
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
    )
    .is_some()
    {
        cross_relations.complete = true;
        *budget = cross_budget;
        *diagnostics = cross_diagnostics;
        return Some(cross_relations);
    }

    budget.pair_visits = cross_budget.pair_visits;
    budget.comparisons = cross_budget.comparisons;
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
) -> Option<()> {
    if relations.old.len() != old_candidates.len() || relations.new.len() != new_candidates.len() {
        return None;
    }
    let old_candidate_by_occurrence =
        candidate_index_by_occurrence(old_occurrences.len(), old_candidates)?;
    let new_candidate_by_occurrence =
        candidate_index_by_occurrence(new_occurrences.len(), new_candidates)?;
    let old_edges = occurrence_edge_index(old_occurrences)?;
    let new_edges = occurrence_edge_index(new_occurrences)?;
    let mut plausible = Vec::new();

    for (old_candidate_index, old_candidate) in old_candidates.iter().enumerate() {
        let old_occurrence = old_occurrences.get(old_candidate.occurrence_index)?;
        collect_plausible_occurrences(&mut plausible, &new_edges, &old_occurrence.tokens)?;
        plausible.retain(|occurrence_index| {
            new_occurrences[*occurrence_index].kind == old_occurrence.kind
                && scope.includes(
                    old_occurrence.span_index,
                    new_occurrences[*occurrence_index].span_index,
                )
        });
        if !budget.charge_pair_visits(plausible.len()) {
            return None;
        }
        for &new_occurrence_index in &plausible {
            let new_occurrence = new_occurrences.get(new_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
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
        collect_plausible_occurrences(&mut plausible, &old_edges, &new_occurrence.tokens)?;
        plausible.retain(|occurrence_index| {
            old_occurrences[*occurrence_index].kind == new_occurrence.kind
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
        if !budget.charge_pair_visits(noncandidate_visits) {
            return None;
        }
        for &old_occurrence_index in &plausible {
            if old_candidate_by_occurrence[old_occurrence_index].is_some() {
                continue;
            }
            let old_occurrence = old_occurrences.get(old_occurrence_index)?;
            let score = sentence_similarity(old_occurrence, new_occurrence, budget)?;
            relations.new[new_candidate_index].record_disqualifying(score);
            if score >= MIN_NEAR_SCORE {
                record_near_pair(diagnostics);
            }
        }
    }
    Some(())
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

fn occurrence_edge_index(
    occurrences: &[SentenceOccurrence],
) -> Option<HashMap<SentenceEvidenceToken, Vec<usize>>> {
    let mut index = HashMap::<SentenceEvidenceToken, Vec<usize>>::new();
    index.try_reserve(occurrences.len()).ok()?;
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        let first = *occurrence.tokens.first()?;
        let last = *occurrence.tokens.last()?;
        let first_occurrences = index.entry(first).or_default();
        first_occurrences.try_reserve(1).ok()?;
        first_occurrences.push(occurrence_index);
        if last != first {
            let last_occurrences = index.entry(last).or_default();
            last_occurrences.try_reserve(1).ok()?;
            last_occurrences.push(occurrence_index);
        }
    }
    Some(index)
}

fn collect_plausible_occurrences(
    plausible: &mut Vec<usize>,
    index: &HashMap<SentenceEvidenceToken, Vec<usize>>,
    tokens: &[SentenceEvidenceToken],
) -> Option<()> {
    plausible.clear();
    let first = tokens.first()?;
    let last = tokens.last()?;
    // A pair satisfying the prefix/suffix threshold must share at least one
    // edge token, so this index prunes work without reducing candidate recall.
    let first_occurrences = index.get(first).map_or(&[][..], Vec::as_slice);
    let last_occurrences = index.get(last).map_or(&[][..], Vec::as_slice);
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
    Some(())
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
    let line_score = if old.kind == RecoveryUnitKind::Line && new.kind == RecoveryUnitKind::Line {
        token_ngram_multiset_dice(&old.tokens, &new.tokens, LINE_NGRAM_SIZE, budget)?
    } else {
        0
    };
    if edge_score < MIN_WORD_SCORE_EDGE_EVIDENCE {
        return Some(edge_score.max(line_score));
    }
    let word_score = word_multiset_dice(old, new, budget)?;
    Some(edge_score.max(word_score).max(line_score))
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
    if size == 0 || old.len() < size || new.len() < size {
        return Some(0);
    }
    let old_windows = old.len().checked_sub(size)?.checked_add(1)?;
    let new_windows = new.len().checked_sub(size)?.checked_add(1)?;
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
    use crate::model::FontProgramHash;

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
            location: Some(test_location(range, 0)),
            span_index: Some(0),
            trusted_position: Some(TrustedStreamPosition {
                stream_index,
                ordinal,
            }),
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
            location: Some(test_location(location, 0)),
            span_index: Some(0),
            trusted_position: None,
        }];
        let new_occurrences = [SentenceOccurrence {
            key: "counterpart".to_owned(),
            tokens: counterpart_tokens,
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            location: None,
            span_index: Some(0),
            trusted_position: None,
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
            location: None,
            span_index: Some(0),
            trusted_position: None,
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
        assert!(budget.charge_evidence_tokens(5));
        assert!(!budget.charge_evidence_tokens(1));

        let mut output = RecoveryBudget::new(3, 2, 5, 1).expect("budget is valid");
        for _ in 0..5 {
            assert!(output.charge_output(1));
        }
        assert!(!output.charge_output(1));
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
            location: Some(test_location(old_range, 0)),
            span_index: Some(0),
            trusted_position: None,
        }];
        let mut new_occurrences = [SentenceOccurrence {
            key: "same".to_owned(),
            tokens: Vec::new(),
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            location: Some(test_location(new_range, 0)),
            span_index: Some(0),
            trusted_position: None,
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
                location: Some(test_location(range, span_index)),
                span_index: Some(span_index),
                trusted_position: None,
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
                location: None,
                span_index: Some(0),
                trusted_position: None,
            },
            SentenceOccurrence {
                key: "old-b".to_owned(),
                tokens: vec![SentenceEvidenceToken::Scalar('b')],
                word_ranges: Vec::new(),
                kind: RecoveryUnitKind::Sentence,
                location: None,
                span_index: Some(0),
                trusted_position: None,
            },
        ];
        let new_occurrences = [SentenceOccurrence {
            key: "new".to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a')],
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            location: None,
            span_index: None,
            trusted_position: None,
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
        )
        .expect("only the edge-compatible pair is visited");

        assert_eq!(budget.pair_visits, 1);
        assert_eq!(budget.comparisons, 1);
        assert!(relations.old[0].vetoed());
        assert!(!relations.old[1].vetoed());
    }

    #[test]
    fn cross_span_budget_failure_keeps_same_span_relations_and_spent_work() {
        let occurrence = |key: &str, span_index: usize| SentenceOccurrence {
            key: key.to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a')],
            word_ranges: Vec::new(),
            kind: RecoveryUnitKind::Sentence,
            location: None,
            span_index: Some(span_index),
            trusted_position: None,
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
        let mut diagnostics = None;

        let relations = modified_sentence_relations(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &new_candidates,
            &mut budget,
            &mut diagnostics,
        )
        .expect("same-span relations survive optional cross-span exhaustion");

        assert!(!relations.complete);
        assert_eq!(relations.old[0].unique_partner(), Some(0));
        assert_eq!(relations.old[1].unique_partner(), Some(1));
        assert_eq!(budget.pair_visits, 3);
        assert_eq!(budget.comparisons, 3);
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
