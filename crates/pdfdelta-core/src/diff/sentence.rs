use std::{collections::HashMap, ops::Range};

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

#[derive(Default)]
pub(super) struct SentenceRecoveryPlan {
    pub deletions: Vec<RecoveredSentence>,
    pub insertions: Vec<RecoveredSentence>,
    pub replacements: Vec<RecoveredReplacement>,
    pub deletion_consumed: Vec<LocalSentenceRange>,
    pub insertion_consumed: Vec<LocalSentenceRange>,
}

impl SentenceRecoveryPlan {
    pub fn has_recovery(&self, span_index: usize) -> bool {
        !recoveries_for_span(&self.deletions, span_index).is_empty()
            || !recoveries_for_span(&self.insertions, span_index).is_empty()
            || !replacements_for_span(&self.replacements, span_index).is_empty()
    }
}

#[derive(Default)]
pub(super) struct SentenceRecoveryBuildOutcome {
    pub plan: Option<SentenceRecoveryPlan>,
    diagnostics: Option<SentenceRecoveryDiagnostics>,
}

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
        let metrics = &mut diagnostics.metrics;
        let updated = metrics
            .recovered_replacement_old_tokens
            .checked_add(committed.replacement_old)
            .and_then(|replacement_old| {
                metrics
                    .recovered_replacement_new_tokens
                    .checked_add(committed.replacement_new)
                    .map(|replacement_new| (replacement_old, replacement_new))
            })
            .and_then(|(replacement_old, replacement_new)| {
                metrics
                    .recovered_deletion_tokens
                    .checked_add(committed.deletion)
                    .map(|deletion| (replacement_old, replacement_new, deletion))
            })
            .and_then(|(replacement_old, replacement_new, deletion)| {
                metrics
                    .recovered_insertion_tokens
                    .checked_add(committed.insertion)
                    .map(|insertion| (replacement_old, replacement_new, deletion, insertion))
            });
        let Some((replacement_old, replacement_new, deletion, insertion)) = updated else {
            self.diagnostics = None;
            return;
        };
        metrics.recovered_replacement_old_tokens = replacement_old;
        metrics.recovered_replacement_new_tokens = replacement_new;
        metrics.recovered_deletion_tokens = deletion;
        metrics.recovered_insertion_tokens = insertion;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    location: Option<SentenceLocation>,
    span_index: Option<usize>,
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

struct RecoveryCandidate {
    occurrence_index: usize,
    span_index: usize,
}

#[derive(Clone, Copy)]
struct CandidateNearRelation {
    eligible_degree: u8,
    eligible_partner: usize,
    disqualified: bool,
}

impl Default for CandidateNearRelation {
    fn default() -> Self {
        Self {
            eligible_degree: 0,
            eligible_partner: usize::MAX,
            disqualified: false,
        }
    }
}

impl CandidateNearRelation {
    fn record_eligible(&mut self, partner: usize) {
        if self.eligible_degree == 0 {
            self.eligible_partner = partner;
        }
        self.eligible_degree = self.eligible_degree.saturating_add(1).min(2);
    }

    fn record_disqualifying(&mut self) {
        self.disqualified = true;
    }

    fn vetoed(self) -> bool {
        self.eligible_degree != 0 || self.disqualified
    }

    fn unique_partner(self) -> Option<usize> {
        (self.eligible_degree == 1 && !self.disqualified).then_some(self.eligible_partner)
    }
}

struct ModifiedSentenceRelations {
    old: Vec<CandidateNearRelation>,
    new: Vec<CandidateNearRelation>,
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
    let Some(old_candidates) = recovery_candidates(
        &old_occurrences,
        &counts,
        OccurrenceSide::Old,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    let Some(new_candidates) = recovery_candidates(
        &new_occurrences,
        &counts,
        OccurrenceSide::New,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    record_exact_candidate_metrics(
        &mut diagnostics,
        &old_occurrences,
        &new_occurrences,
        &counts,
        &old_candidates,
        &new_candidates,
        &membership.recovery_spans,
        input.min_tokens,
    );
    let Some(relations) = modified_sentence_relations(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &mut budget,
        &mut diagnostics,
    ) else {
        return Ok(SentenceRecoveryBuildOutcome::default());
    };
    record_vetoed_near_pairs(&mut diagnostics, &relations);

    let mut plan = SentenceRecoveryPlan::default();
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
    for plan in plans {
        let stream = build_stream(side, &plan)?;
        for boundary in
            sentence_boundaries(&stream.text, &stream.forced_sentence_boundaries, budget)?
        {
            let key = stream.text.get(boundary.byte_start..boundary.byte_end)?;
            if !budget.charge_key_bytes(key.len()) {
                return None;
            }
            let mut owned_key = String::new();
            owned_key.try_reserve_exact(key.len()).ok()?;
            owned_key.push_str(key);

            let touched_blocks = sentence_stream_block_range(&stream, boundary)?;
            let tokens = sentence_tokens(&stream, boundary, budget)?;
            let span_index =
                sentence_span_index(side, &stream, touched_blocks.clone(), span_by_block)?;
            let location = match span_index {
                Some(span_index) if recovery_spans.get(span_index).copied()? => {
                    sentence_location(side, &stream, boundary, touched_blocks, span_index, budget)?
                }
                Some(_) | None => None,
            };
            occurrences.try_reserve(1).ok()?;
            occurrences.push(SentenceOccurrence {
                key: owned_key,
                tokens,
                location,
                span_index,
            });
        }
    }
    occurrences.sort_unstable_by_key(|occurrence| occurrence.span_index);
    Some(occurrences)
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
    Some(Stream {
        text,
        tokens,
        scalar_to_token,
        blocks,
        forced_sentence_boundaries,
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
    budget: &mut RecoveryBudget,
) -> Option<Option<SentenceLocation>> {
    if !stream.trusted {
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
) -> Option<HashMap<&'a str, OccurrenceCount>> {
    let capacity = old.len().checked_add(new.len())?;
    let mut counts = HashMap::new();
    counts.try_reserve(capacity).ok()?;
    count_occurrences(&mut counts, old, OccurrenceSide::Old)?;
    count_occurrences(&mut counts, new, OccurrenceSide::New)?;
    Some(counts)
}

fn count_occurrences<'a>(
    counts: &mut HashMap<&'a str, OccurrenceCount>,
    occurrences: &'a [SentenceOccurrence],
    side: OccurrenceSide,
) -> Option<()> {
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        let count = counts.entry(occurrence.key.as_str()).or_default();
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

#[allow(clippy::too_many_arguments)]
fn record_exact_candidate_metrics(
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    counts: &HashMap<&str, OccurrenceCount>,
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    recovery_spans: &[bool],
    min_tokens: usize,
) {
    let Some(exact_shared_units) = exact_shared_units(
        old_occurrences,
        new_occurrences,
        counts,
        recovery_spans,
        min_tokens,
    ) else {
        *diagnostics = None;
        return;
    };
    let Some(diagnostics) = diagnostics.as_mut() else {
        return;
    };
    diagnostics.metrics.exact_shared_units = exact_shared_units;
    diagnostics.metrics.old_exact_one_sided_units = old_candidates.len();
    diagnostics.metrics.new_exact_one_sided_units = new_candidates.len();
}

fn exact_shared_units(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    counts: &HashMap<&str, OccurrenceCount>,
    recovery_spans: &[bool],
    min_tokens: usize,
) -> Option<usize> {
    counts.values().try_fold(0usize, |units, count| {
        if count.old != 1 || count.new != 1 {
            return Some(units);
        }
        let old = old_occurrences.get(count.old_index?)?;
        let new = new_occurrences.get(count.new_index?)?;
        let shared_span = old.span_index == new.span_index
            && old
                .span_index
                .and_then(|span_index| recovery_spans.get(span_index))
                .copied()
                == Some(true);
        let eligible = shared_span
            && old.location.is_some()
            && new.location.is_some()
            && old.tokens.len() >= min_tokens
            && new.tokens.len() >= min_tokens;
        if eligible {
            units.checked_add(1)
        } else {
            Some(units)
        }
    })
}

fn recovery_candidates(
    occurrences: &[SentenceOccurrence],
    counts: &HashMap<&str, OccurrenceCount>,
    side: OccurrenceSide,
    recovery_spans: &[bool],
    min_tokens: usize,
) -> Option<Vec<RecoveryCandidate>> {
    let mut candidates = Vec::new();
    candidates.try_reserve(occurrences.len()).ok()?;
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
        let count = counts.get(occurrence.key.as_str())?;
        let unique = match side {
            OccurrenceSide::Old => count.old == 1 && count.new == 0,
            OccurrenceSide::New => count.new == 1 && count.old == 0,
        };
        if unique {
            candidates.push(RecoveryCandidate {
                occurrence_index,
                span_index,
            });
        }
    }
    candidates.sort_unstable_by_key(|candidate| (candidate.span_index, candidate.occurrence_index));
    Some(candidates)
}

fn modified_sentence_relations(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    budget: &mut RecoveryBudget,
    diagnostics: &mut Option<SentenceRecoveryDiagnostics>,
) -> Option<ModifiedSentenceRelations> {
    let mut old_relations = Vec::new();
    let mut new_relations = Vec::new();
    old_relations.try_reserve_exact(old_candidates.len()).ok()?;
    new_relations.try_reserve_exact(new_candidates.len()).ok()?;
    old_relations.resize(old_candidates.len(), CandidateNearRelation::default());
    new_relations.resize(new_candidates.len(), CandidateNearRelation::default());

    let ambiguous_old_end = ambiguous_occurrence_end(old_occurrences);
    let ambiguous_new_end = ambiguous_occurrence_end(new_occurrences);
    let ambiguous_visits = old_candidates
        .len()
        .checked_mul(ambiguous_new_end)?
        .checked_add(new_candidates.len().checked_mul(ambiguous_old_end)?)?;
    if !budget.charge_pair_visits(ambiguous_visits) {
        return None;
    }

    let mut old_candidate_start = 0usize;
    while old_candidate_start < old_candidates.len() {
        let old_candidate_end = candidate_group_end(old_candidates, old_candidate_start);
        let span_index = old_candidates[old_candidate_start].span_index;
        let (new_occurrence_start, new_occurrence_end) =
            occurrence_span_range(new_occurrences, span_index);
        let visits = old_candidate_end
            .checked_sub(old_candidate_start)?
            .checked_mul(new_occurrence_end.checked_sub(new_occurrence_start)?)?;
        if !budget.charge_pair_visits(visits) {
            return None;
        }
        let (new_candidate_start, new_candidate_end) =
            candidate_span_range(new_candidates, span_index);
        for old_candidate_index in old_candidate_start..old_candidate_end {
            for (new_occurrence_offset, new_occurrence) in new_occurrences
                [new_occurrence_start..new_occurrence_end]
                .iter()
                .enumerate()
            {
                let new_occurrence_index = new_occurrence_start + new_occurrence_offset;
                if sentences_are_near(
                    &old_occurrences[old_candidates[old_candidate_index].occurrence_index].tokens,
                    &new_occurrence.tokens,
                    budget,
                )? {
                    record_near_pair(diagnostics);
                    if let Ok(offset) = new_candidates[new_candidate_start..new_candidate_end]
                        .binary_search_by_key(&new_occurrence_index, |candidate| {
                            candidate.occurrence_index
                        })
                    {
                        let new_candidate_index = new_candidate_start + offset;
                        old_relations[old_candidate_index].record_eligible(new_candidate_index);
                        new_relations[new_candidate_index].record_eligible(old_candidate_index);
                    } else {
                        old_relations[old_candidate_index].record_disqualifying();
                    }
                }
            }
            for new_occurrence in &new_occurrences[..ambiguous_new_end] {
                if sentences_are_near(
                    &old_occurrences[old_candidates[old_candidate_index].occurrence_index].tokens,
                    &new_occurrence.tokens,
                    budget,
                )? {
                    record_near_pair(diagnostics);
                    old_relations[old_candidate_index].record_disqualifying();
                }
            }
        }
        old_candidate_start = old_candidate_end;
    }

    let mut new_candidate_start = 0usize;
    while new_candidate_start < new_candidates.len() {
        let new_candidate_end = candidate_group_end(new_candidates, new_candidate_start);
        let span_index = new_candidates[new_candidate_start].span_index;
        let (old_occurrence_start, old_occurrence_end) =
            occurrence_span_range(old_occurrences, span_index);
        let (old_candidate_start, old_candidate_end) =
            candidate_span_range(old_candidates, span_index);
        let noncandidate_count = old_occurrence_end
            .checked_sub(old_occurrence_start)?
            .checked_sub(old_candidate_end.checked_sub(old_candidate_start)?)?;
        let visits = new_candidate_end
            .checked_sub(new_candidate_start)?
            .checked_mul(noncandidate_count)?;
        if !budget.charge_pair_visits(visits) {
            return None;
        }
        for new_candidate_index in new_candidate_start..new_candidate_end {
            for (old_occurrence_offset, old_occurrence) in old_occurrences
                [old_occurrence_start..old_occurrence_end]
                .iter()
                .enumerate()
            {
                let old_occurrence_index = old_occurrence_start + old_occurrence_offset;
                if old_candidates[old_candidate_start..old_candidate_end]
                    .binary_search_by_key(&old_occurrence_index, |candidate| {
                        candidate.occurrence_index
                    })
                    .is_ok()
                {
                    continue;
                }
                if sentences_are_near(
                    &old_occurrence.tokens,
                    &new_occurrences[new_candidates[new_candidate_index].occurrence_index].tokens,
                    budget,
                )? {
                    record_near_pair(diagnostics);
                    new_relations[new_candidate_index].record_disqualifying();
                }
            }
            for old_occurrence in &old_occurrences[..ambiguous_old_end] {
                if sentences_are_near(
                    &old_occurrence.tokens,
                    &new_occurrences[new_candidates[new_candidate_index].occurrence_index].tokens,
                    budget,
                )? {
                    record_near_pair(diagnostics);
                    new_relations[new_candidate_index].record_disqualifying();
                }
            }
        }
        new_candidate_start = new_candidate_end;
    }
    Some(ModifiedSentenceRelations {
        old: old_relations,
        new: new_relations,
    })
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
    let Some(vetoed) = adopted.and_then(|adopted| near_pair_candidates.checked_sub(adopted)) else {
        *diagnostics = None;
        return;
    };
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.metrics.vetoed_near_pairs = vetoed;
    }
}

fn candidate_group_end(candidates: &[RecoveryCandidate], start: usize) -> usize {
    let span_index = candidates[start].span_index;
    candidates[start..]
        .iter()
        .position(|candidate| candidate.span_index != span_index)
        .map_or(candidates.len(), |offset| start + offset)
}

fn candidate_span_range(candidates: &[RecoveryCandidate], span_index: usize) -> (usize, usize) {
    let start = candidates.partition_point(|candidate| candidate.span_index < span_index);
    let end =
        candidates[start..].partition_point(|candidate| candidate.span_index == span_index) + start;
    (start, end)
}

fn occurrence_span_range(occurrences: &[SentenceOccurrence], span_index: usize) -> (usize, usize) {
    let start = occurrences.partition_point(|occurrence| occurrence.span_index < Some(span_index));
    let end = occurrences[start..]
        .partition_point(|occurrence| occurrence.span_index == Some(span_index))
        + start;
    (start, end)
}

fn ambiguous_occurrence_end(occurrences: &[SentenceOccurrence]) -> usize {
    occurrences.partition_point(|occurrence| occurrence.span_index.is_none())
}

fn sentences_are_near(
    old: &[SentenceEvidenceToken],
    new: &[SentenceEvidenceToken],
    budget: &mut RecoveryBudget,
) -> Option<bool> {
    let shorter = old.len().min(new.len());
    if shorter == 0 {
        return Some(false);
    }

    let mut prefix = 0usize;
    while prefix < shorter {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if old[prefix] != new[prefix] {
            break;
        }
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < shorter - prefix {
        if !budget.charge_comparisons(1) {
            return None;
        }
        if old[old.len() - suffix - 1] != new[new.len() - suffix - 1] {
            break;
        }
        suffix += 1;
    }

    let shared = prefix.checked_add(suffix)?;
    Some(shared >= shorter - shorter / 5)
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
        if old_candidate.span_index != new_candidate.span_index {
            return None;
        }
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
        plan.deletion_consumed.extend(old_location.consumed);
        plan.insertion_consumed.extend(new_location.consumed);
    }
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
        let location = occurrences
            .get(candidate.occurrence_index)?
            .location
            .as_ref()?;
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
        let location = occurrences
            .get_mut(candidate.occurrence_index)?
            .location
            .take()?;
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
            location: Some(test_location(location, 0)),
            span_index: Some(0),
        }];
        let new_occurrences = [SentenceOccurrence {
            key: "counterpart".to_owned(),
            tokens: counterpart_tokens,
            location: None,
            span_index: Some(0),
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
            sentence_location(&side, &stream, boundary, 0..1, 0, &mut budget)
                .is_some_and(|location| location.is_some())
        );
        assert_eq!(budget.location_items, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS);
        let bytes = budget.location_bytes;

        assert!(sentence_location(&side, &stream, boundary, 0..1, 0, &mut budget).is_none());
        assert_eq!(budget.location_items, MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS);
        assert_eq!(budget.location_bytes, bytes);
    }

    #[test]
    fn ambiguous_pair_visits_are_charged_before_comparisons() {
        let old_occurrences = [
            SentenceOccurrence {
                key: "old-a".to_owned(),
                tokens: vec![SentenceEvidenceToken::Scalar('a')],
                location: None,
                span_index: Some(0),
            },
            SentenceOccurrence {
                key: "old-b".to_owned(),
                tokens: vec![SentenceEvidenceToken::Scalar('b')],
                location: None,
                span_index: Some(0),
            },
        ];
        let new_occurrences = [SentenceOccurrence {
            key: "new".to_owned(),
            tokens: vec![SentenceEvidenceToken::Scalar('a')],
            location: None,
            span_index: None,
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

        assert!(
            modified_sentence_relations(
                &old_occurrences,
                &new_occurrences,
                &old_candidates,
                &[],
                &mut budget,
                &mut diagnostics,
            )
            .is_none()
        );
        assert_eq!(budget.pair_visits, 0);
        assert_eq!(budget.comparisons, 0);
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
