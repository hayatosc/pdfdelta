use std::{collections::HashMap, ops::Range};

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Result,
    alignment::{Alignment, AlignmentEvidence, AlignmentKind, BlockSeparator},
    layout::{BlockId, TrustedRunId, TrustedRunInterval},
    normalize::{ComparableToken, ScalarRange},
};

use super::{SentenceRecoveryInput, Side, TokenRange};

pub(super) const MAX_SENTENCE_RECOVERY_RANGES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LocalSentenceRange {
    pub block: BlockId,
    pub canonical: ScalarRange,
    pub comparable: TokenRange,
}

#[derive(Default)]
pub(super) struct SentenceRecoveryPlan {
    pub deletions: Vec<LocalSentenceRange>,
    pub insertions: Vec<LocalSentenceRange>,
}

impl SentenceRecoveryPlan {
    pub fn has_recovery(&self, old: &[BlockId], new: &[BlockId]) -> bool {
        old.iter()
            .any(|block| !ranges_for_block(&self.deletions, *block).is_empty())
            || new
                .iter()
                .any(|block| !ranges_for_block(&self.insertions, *block).is_empty())
    }
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
    location: Option<LocalSentenceRange>,
    span_index: Option<usize>,
}

#[derive(Default)]
struct OccurrenceCount {
    old: usize,
    new: usize,
}

struct RecoveryCandidate {
    occurrence_index: usize,
    span_index: usize,
    location: LocalSentenceRange,
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
    trusted: bool,
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
        Self::charge(&mut self.evidence_tokens, amount, self.token_limit)
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
) -> Result<Option<SentenceRecoveryPlan>> {
    let Some(mut budget) = RecoveryBudget::new(
        old.total_tokens,
        new.total_tokens,
        max_tokens,
        input.min_tokens,
    ) else {
        return Ok(None);
    };
    let Some(membership) = span_membership(alignment) else {
        return Ok(None);
    };
    if !membership.recovery_spans.iter().any(|eligible| *eligible) {
        return Ok(None);
    }

    let Some(old_occurrences) = collect_occurrences(
        old,
        input.old_trusted_run_intervals,
        &membership.old,
        &mut budget,
    ) else {
        return Ok(None);
    };
    let Some(new_occurrences) = collect_occurrences(
        new,
        input.new_trusted_run_intervals,
        &membership.new,
        &mut budget,
    ) else {
        return Ok(None);
    };
    let Some(counts) = occurrence_counts(&old_occurrences, &new_occurrences) else {
        return Ok(None);
    };
    let Some(old_candidates) = recovery_candidates(
        &old_occurrences,
        &counts,
        OccurrenceSide::Old,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(None);
    };
    let Some(new_candidates) = recovery_candidates(
        &new_occurrences,
        &counts,
        OccurrenceSide::New,
        &membership.recovery_spans,
        input.min_tokens,
    ) else {
        return Ok(None);
    };
    let Some((old_vetoes, new_vetoes)) = modified_sentence_vetoes(
        &old_occurrences,
        &new_occurrences,
        &old_candidates,
        &new_candidates,
        &mut budget,
    ) else {
        return Ok(None);
    };

    let mut plan = SentenceRecoveryPlan::default();
    if append_candidate_ranges(
        &mut plan.deletions,
        &old_candidates,
        &old_vetoes,
        &mut budget,
    )
    .is_none()
        || append_candidate_ranges(
            &mut plan.insertions,
            &new_candidates,
            &new_vetoes,
            &mut budget,
        )
        .is_none()
        || !normalize_ranges(&mut plan.deletions)
        || !normalize_ranges(&mut plan.insertions)
    {
        return Ok(None);
    }
    Ok(Some(plan))
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
    budget: &mut RecoveryBudget,
) -> Option<Vec<SentenceOccurrence>> {
    let plans = stream_plans(trusted_run_intervals)?;
    let mut occurrences = Vec::new();
    for plan in plans {
        let stream = build_stream(side, &plan)?;
        for boundary in sentence_boundaries(&stream.text, budget)? {
            let key = stream.text.get(boundary.byte_start..boundary.byte_end)?;
            if !budget.charge_key_bytes(key.len()) {
                return None;
            }
            let mut owned_key = String::new();
            owned_key.try_reserve_exact(key.len()).ok()?;
            owned_key.push_str(key);

            let location = sentence_location(side, &stream, boundary);
            let tokens = sentence_tokens(&stream, boundary, budget)?;
            let span_index = sentence_span_index(side, &stream, boundary, span_by_block)?;
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
    text.try_reserve_exact(text_capacity).ok()?;
    tokens.try_reserve_exact(token_capacity).ok()?;
    blocks.try_reserve_exact(plan.block_indices.len()).ok()?;
    let mut scalar_count = 0usize;

    for (position, side_index) in plan.block_indices.iter().copied().enumerate() {
        let block = side.blocks.get(side_index)?;
        let next = side.canonical.get(side_index)?;
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
    }

    let scalar_to_token = scalar_to_token_boundaries(&tokens)?;
    Some(Stream {
        text,
        tokens,
        scalar_to_token,
        blocks,
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

fn sentence_location(
    side: &Side<'_>,
    stream: &Stream,
    boundary: SentenceBoundary,
) -> Option<LocalSentenceRange> {
    if !stream.trusted {
        return None;
    }
    let stream_block = stream.blocks.iter().find(|block| {
        block.scalar_range.start <= boundary.scalar_start
            && boundary.scalar_end <= block.scalar_range.end
    })?;
    let block = side.blocks.get(stream_block.side_index)?;
    let tokens = side.canonical.get(stream_block.side_index)?;
    if !block.issues.is_empty() || !block.canonical.unmapped.is_empty() {
        return None;
    }

    let local_scalar_start = boundary
        .scalar_start
        .checked_sub(stream_block.scalar_range.start)?;
    let local_scalar_end = boundary
        .scalar_end
        .checked_sub(stream_block.scalar_range.start)?;
    let token_start = *stream_block.scalar_to_token.get(local_scalar_start)?;
    let token_end = *stream_block.scalar_to_token.get(local_scalar_end)?;
    if token_start >= token_end || token_end > tokens.len() {
        return None;
    }

    Some(LocalSentenceRange {
        block: block.block,
        canonical: ScalarRange {
            start: local_scalar_start,
            end: local_scalar_end,
        },
        comparable: TokenRange {
            start: token_start,
            end: token_end,
        },
    })
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
    boundary: SentenceBoundary,
    span_by_block: &HashMap<BlockId, usize>,
) -> Option<Option<usize>> {
    let mut span_index = None;
    let mut ambiguous = false;
    for block in &stream.blocks {
        let overlap_start = boundary.scalar_start.max(block.scalar_range.start);
        let overlap_end = boundary.scalar_end.min(block.scalar_range.end);
        if overlap_start >= overlap_end {
            continue;
        }
        let block_id = side.blocks.get(block.side_index)?.block;
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
    for occurrence in occurrences {
        let count = counts.entry(occurrence.key.as_str()).or_default();
        let target = match side {
            OccurrenceSide::Old => &mut count.old,
            OccurrenceSide::New => &mut count.new,
        };
        *target = target.checked_add(1)?;
    }
    Some(())
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
        let Some(location) = occurrence.location else {
            continue;
        };
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
                location,
            });
        }
    }
    candidates.sort_unstable_by_key(|candidate| (candidate.span_index, candidate.occurrence_index));
    Some(candidates)
}

fn modified_sentence_vetoes(
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    old_candidates: &[RecoveryCandidate],
    new_candidates: &[RecoveryCandidate],
    budget: &mut RecoveryBudget,
) -> Option<(Vec<bool>, Vec<bool>)> {
    let mut old_vetoes = Vec::new();
    let mut new_vetoes = Vec::new();
    old_vetoes.try_reserve_exact(old_candidates.len()).ok()?;
    new_vetoes.try_reserve_exact(new_candidates.len()).ok()?;
    old_vetoes.resize(old_candidates.len(), false);
    new_vetoes.resize(new_candidates.len(), false);

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
                    old_vetoes[old_candidate_index] = true;
                    if let Ok(offset) = new_candidates[new_candidate_start..new_candidate_end]
                        .binary_search_by_key(&new_occurrence_index, |candidate| {
                            candidate.occurrence_index
                        })
                    {
                        new_vetoes[new_candidate_start + offset] = true;
                    }
                }
            }
            for new_occurrence in &new_occurrences[..ambiguous_new_end] {
                if sentences_are_near(
                    &old_occurrences[old_candidates[old_candidate_index].occurrence_index].tokens,
                    &new_occurrence.tokens,
                    budget,
                )? {
                    old_vetoes[old_candidate_index] = true;
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
                    new_vetoes[new_candidate_index] = true;
                }
            }
            for old_occurrence in &old_occurrences[..ambiguous_old_end] {
                if sentences_are_near(
                    &old_occurrence.tokens,
                    &new_occurrences[new_candidates[new_candidate_index].occurrence_index].tokens,
                    budget,
                )? {
                    new_vetoes[new_candidate_index] = true;
                }
            }
        }
        new_candidate_start = new_candidate_end;
    }
    Some((old_vetoes, new_vetoes))
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

fn append_candidate_ranges(
    ranges: &mut Vec<LocalSentenceRange>,
    candidates: &[RecoveryCandidate],
    vetoes: &[bool],
    budget: &mut RecoveryBudget,
) -> Option<()> {
    if candidates.len() != vetoes.len() {
        return None;
    }
    let mut retained_count = 0usize;
    let mut retained_tokens = 0usize;
    for (candidate, vetoed) in candidates.iter().zip(vetoes) {
        if *vetoed {
            continue;
        }
        retained_count = retained_count.checked_add(1)?;
        retained_tokens = retained_tokens.checked_add(
            candidate
                .location
                .comparable
                .end
                .checked_sub(candidate.location.comparable.start)?,
        )?;
    }
    if !budget.charge_outputs(retained_count, retained_tokens) {
        return None;
    }
    ranges.try_reserve_exact(retained_count).ok()?;
    for (candidate, vetoed) in candidates.iter().zip(vetoes) {
        if *vetoed {
            continue;
        }
        ranges.push(candidate.location);
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

fn sentence_boundaries(text: &str, budget: &mut RecoveryBudget) -> Option<Vec<SentenceBoundary>> {
    let mut boundaries = Vec::new();
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
                byte_start: start.checked_add(leading_bytes)?,
                byte_end: byte_end.checked_sub(trailing_bytes)?,
                scalar_start: pending_scalar_start
                    .checked_add(raw.get(..leading_bytes)?.chars().count())?,
                scalar_end: scalar_end.checked_sub(raw.get(trailing_start..)?.chars().count())?,
            });
            pending_byte_start = None;
        }
        scalar_cursor = scalar_end;
    }

    Some(boundaries)
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
        sentence_boundaries(text, &mut budget)
            .expect("sentence scan stays within budget")
            .iter()
            .map(|range| &text[range.byte_start..range.byte_end])
            .collect()
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
            trusted: true,
        };
        let boundary = SentenceBoundary {
            byte_start: 0,
            byte_end: 0,
            scalar_start: 0,
            scalar_end: 1,
        };

        assert!(sentence_span_index(&side, &stream, boundary, &HashMap::new()).is_none());
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
            location: Some(location),
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
            location,
        }];
        let mut budget = RecoveryBudget::new(10, 10, 20, 1).expect("test budget is valid");

        let (old_vetoes, new_vetoes) = modified_sentence_vetoes(
            &old_occurrences,
            &new_occurrences,
            &old_candidates,
            &[],
            &mut budget,
        )
        .expect("near-match veto stays within budget");
        assert_eq!(old_vetoes, [true]);
        assert!(new_vetoes.is_empty());
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

        let mut output = RecoveryBudget::new(3, 2, 5, 1).expect("budget is valid");
        for _ in 0..5 {
            assert!(output.charge_output(1));
        }
        assert!(!output.charge_output(1));
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
        let location = LocalSentenceRange {
            block: BlockId(1),
            canonical: ScalarRange { start: 0, end: 1 },
            comparable: TokenRange { start: 0, end: 1 },
        };
        let old_candidates = [
            RecoveryCandidate {
                occurrence_index: 0,
                span_index: 0,
                location,
            },
            RecoveryCandidate {
                occurrence_index: 1,
                span_index: 0,
                location,
            },
        ];
        let mut budget = RecoveryBudget::new(1, 0, 1, 1).expect("test budget is valid");

        assert!(
            modified_sentence_vetoes(
                &old_occurrences,
                &new_occurrences,
                &old_candidates,
                &[],
                &mut budget,
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
