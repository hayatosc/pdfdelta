use std::collections::HashMap;

use super::*;

const MAX_CROSS_GRANULARITY_SUBSTITUTIONS: usize = 2;
const MAX_CROSS_GRANULARITY_EDIT_WINDOW: usize = 4;

type CrossGranularityResult<T> = std::result::Result<T, CrossGranularityStopReason>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CrossGranularityStopReason {
    CandidateCountLimit,
    OutputLimit,
    Work(NearRelationStopReason),
    AllocationFailure,
    CounterOverflow,
    EvidenceUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CrossGranularityOutcome {
    Complete(usize),
    Stopped(CrossGranularityStopReason),
}

impl CrossGranularityOutcome {
    pub(super) fn is_complete(self) -> bool {
        matches!(self, Self::Complete(_))
    }
}

fn checked<T>(value: Option<T>) -> CrossGranularityResult<T> {
    value.ok_or(CrossGranularityStopReason::CounterOverflow)
}

fn evidence<T>(value: Option<T>) -> CrossGranularityResult<T> {
    value.ok_or(CrossGranularityStopReason::EvidenceUnavailable)
}

fn reserve(
    result: std::result::Result<(), std::collections::TryReserveError>,
) -> CrossGranularityResult<()> {
    result.map_err(|_| CrossGranularityStopReason::AllocationFailure)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CrossGranularitySegmentId {
    side: OccurrenceSide,
    stream_index: usize,
    start_ordinal: usize,
    end_ordinal: usize,
}

#[derive(Clone, Copy)]
struct CrossGranularitySegment {
    id: CrossGranularitySegmentId,
    block: BlockId,
    canonical: ScalarRange,
    comparable: TokenRange,
    span_index: usize,
    role: BlockRole,
}

#[derive(Clone, Copy)]
struct CrossGranularityPair {
    segment: CrossGranularitySegment,
    singleton_side: OccurrenceSide,
    singleton_index: usize,
    singleton_canonical: ScalarRange,
    singleton_comparable: TokenRange,
    score: u16,
}

#[derive(Clone, Copy, Default)]
struct CrossGranularityWork {
    posting_visits: usize,
    pair_attempts: usize,
    comparisons: usize,
}

#[derive(Clone, Copy)]
struct CrossGranularitySourceRange {
    side: OccurrenceSide,
    block: BlockId,
    start: usize,
    end: usize,
    replacement: usize,
}

fn single_block_occurrence_source<'a>(
    side: &'a Side<'_>,
    occurrence: &'a SentenceOccurrence,
) -> Option<(BlockId, &'a [ComparableToken], &'a SentenceLocation)> {
    let location = occurrence.location.as_ref()?;
    let [block] = location.recovery.blocks.as_slice() else {
        return None;
    };
    let [consumed] = location.consumed.as_slice() else {
        return None;
    };
    if location.recovery.separator.is_some()
        || consumed.block != *block
        || consumed.canonical != location.recovery.canonical
        || consumed.comparable != location.recovery.comparable
    {
        return None;
    }
    let tokens = side
        .canonical
        .get(*side.index.get(block)?)?
        .get(consumed.comparable.start..consumed.comparable.end)?;
    if tokens.len()
        != consumed
            .canonical
            .end
            .checked_sub(consumed.canonical.start)?
        || tokens
            .iter()
            .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    {
        return None;
    }
    Some((*block, tokens, location))
}

#[derive(Clone, Copy)]
enum OccurrenceBoundary {
    Start,
    End,
}

fn occurrence_boundary_source<'a>(
    side: &'a Side<'_>,
    occurrence: &'a SentenceOccurrence,
    boundary: OccurrenceBoundary,
) -> Option<(BlockId, &'a [ComparableToken], LocalSentenceRange)> {
    let location = occurrence.location.as_ref()?;
    if location.recovery.blocks.len() != location.consumed.len() {
        return None;
    }
    let (block, consumed) = match boundary {
        OccurrenceBoundary::Start => (
            location.recovery.blocks.first()?,
            location.consumed.first()?,
        ),
        OccurrenceBoundary::End => (location.recovery.blocks.last()?, location.consumed.last()?),
    };
    if consumed.block != *block {
        return None;
    }
    let tokens = side
        .canonical
        .get(*side.index.get(block)?)?
        .get(consumed.comparable.start..consumed.comparable.end)?;
    if tokens.len()
        != consumed
            .canonical
            .end
            .checked_sub(consumed.canonical.start)?
        || tokens
            .iter()
            .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    {
        return None;
    }
    Some((*block, tokens, *consumed))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossGranularityWindowRelation {
    Exact,
    Near(u16),
}

fn cross_granularity_window_relation(
    segment: &[ComparableToken],
    candidate: &[ComparableToken],
    min_context: usize,
    comparisons: &mut usize,
    comparison_limit: usize,
) -> CrossGranularityResult<Option<CrossGranularityWindowRelation>> {
    if segment.len() != candidate.len() || segment.len() < checked(min_context.checked_mul(2))? {
        return Ok(None);
    }
    let attempted = checked(comparisons.checked_add(segment.len()))?;
    if attempted > comparison_limit {
        return Err(CrossGranularityStopReason::Work(
            NearRelationStopReason::SimilarityComparisonLimit,
        ));
    }
    *comparisons = attempted;

    let mut first_mismatch = None;
    let mut last_mismatch = 0usize;
    let mut mismatch_count = 0usize;
    for (index, (old, new)) in segment.iter().zip(candidate).enumerate() {
        if old == new {
            continue;
        }
        let (ComparableToken::Scalar(old), ComparableToken::Scalar(new)) = (old, new) else {
            return Ok(None);
        };
        let safe_substitution = (old.is_ascii_punctuation() && new.is_ascii_punctuation())
            || old.eq_ignore_ascii_case(new);
        if !safe_substitution {
            return Ok(None);
        }
        mismatch_count = checked(mismatch_count.checked_add(1))?;
        if mismatch_count > MAX_CROSS_GRANULARITY_SUBSTITUTIONS {
            return Ok(None);
        }
        first_mismatch.get_or_insert(index);
        last_mismatch = index;
    }
    let Some(first_mismatch) = first_mismatch else {
        return Ok(Some(CrossGranularityWindowRelation::Exact));
    };
    let edit_window = checked(checked(last_mismatch.checked_sub(first_mismatch))?.checked_add(1))?;
    let accepted = first_mismatch >= min_context
        && checked(
            segment
                .len()
                .checked_sub(checked(last_mismatch.checked_add(1))?),
        )? >= min_context
        && edit_window <= MAX_CROSS_GRANULARITY_EDIT_WINDOW;
    if !accepted {
        return Ok(None);
    }
    let matching = checked(segment.len().checked_sub(mismatch_count))?;
    let score = checked(matching.checked_mul(10_000))?
        .checked_div(segment.len())
        .ok_or(CrossGranularityStopReason::CounterOverflow)?
        .try_into()
        .map_err(|_| CrossGranularityStopReason::CounterOverflow)?;
    Ok(Some(CrossGranularityWindowRelation::Near(score)))
}

fn cross_granularity_segment_location(
    segment: CrossGranularitySegment,
) -> Option<SentenceLocation> {
    let source_tokens = segment
        .comparable
        .end
        .checked_sub(segment.comparable.start)?;
    let consumed = LocalSentenceRange {
        block: segment.block,
        canonical: segment.canonical,
        comparable: segment.comparable,
    };
    Some(SentenceLocation {
        recovery: RecoveredSentence {
            origin: ChangeOrigin::CrossGranularity,
            span_index: segment.span_index,
            kind: RecoveryUnitKind::Sentence,
            role: segment.role.into(),
            blocks: vec![segment.block],
            separator: None,
            canonical: segment.canonical,
            comparable: segment.comparable,
            source_tokens,
        },
        consumed: vec![consumed],
    })
}

fn cross_granularity_singleton_location(
    occurrence: &SentenceOccurrence,
    canonical: ScalarRange,
    comparable: TokenRange,
) -> Option<SentenceLocation> {
    let location = occurrence.location.as_ref()?;
    let [block] = location.recovery.blocks.as_slice() else {
        return None;
    };
    if canonical.start < location.recovery.canonical.start
        || canonical.end > location.recovery.canonical.end
        || comparable.start < location.recovery.comparable.start
        || comparable.end > location.recovery.comparable.end
    {
        return None;
    }
    let source_tokens = comparable.end.checked_sub(comparable.start)?;
    let consumed = LocalSentenceRange {
        block: *block,
        canonical,
        comparable,
    };
    Some(SentenceLocation {
        recovery: RecoveredSentence {
            origin: ChangeOrigin::CrossGranularity,
            span_index: location.recovery.span_index,
            kind: occurrence.kind,
            role: location.recovery.role,
            blocks: vec![*block],
            separator: None,
            canonical,
            comparable,
            source_tokens,
        },
        consumed: vec![consumed],
    })
}

fn reject_cross_granularity_range_conflicts(
    pending: &[RecoveredReplacement],
) -> CrossGranularityResult<Vec<bool>> {
    let mut ranges = Vec::new();
    reserve(ranges.try_reserve_exact(checked(pending.len().checked_mul(2))?))?;
    for (replacement, candidate) in pending.iter().enumerate() {
        let [old] = candidate.old_consumed.as_slice() else {
            return Err(CrossGranularityStopReason::EvidenceUnavailable);
        };
        let [new] = candidate.new_consumed.as_slice() else {
            return Err(CrossGranularityStopReason::EvidenceUnavailable);
        };
        for (side, range) in [(OccurrenceSide::Old, old), (OccurrenceSide::New, new)] {
            if range.comparable.start >= range.comparable.end {
                return Err(CrossGranularityStopReason::EvidenceUnavailable);
            }
            ranges.push(CrossGranularitySourceRange {
                side,
                block: range.block,
                start: range.comparable.start,
                end: range.comparable.end,
                replacement,
            });
        }
    }
    cross_granularity_range_conflicts(&mut ranges, pending.len())
}

fn cross_granularity_range_conflicts(
    ranges: &mut [CrossGranularitySourceRange],
    replacement_count: usize,
) -> CrossGranularityResult<Vec<bool>> {
    ranges.sort_unstable_by_key(|range| {
        (
            range.side,
            range.block,
            range.start,
            range.end,
            range.replacement,
        )
    });

    let mut rejected = Vec::new();
    reserve(rejected.try_reserve_exact(replacement_count))?;
    rejected.resize(replacement_count, false);
    let mut group_start = 0usize;
    while group_start < ranges.len() {
        let side = ranges[group_start].side;
        let block = ranges[group_start].block;
        let group_end = group_start
            + ranges[group_start..]
                .partition_point(|range| range.side == side && range.block == block);
        let mut component_start = group_start;
        while component_start < group_end {
            let mut component_end = component_start + 1;
            let mut max_end = ranges[component_start].end;
            while component_end < group_end && ranges[component_end].start < max_end {
                max_end = max_end.max(ranges[component_end].end);
                component_end += 1;
            }
            if component_end - component_start > 1 {
                for range in &ranges[component_start..component_end] {
                    *evidence(rejected.get_mut(range.replacement))? = true;
                }
            }
            component_start = component_end;
        }
        group_start = group_end;
    }
    Ok(rejected)
}

fn collect_cross_granularity_segments(
    side: &Side<'_>,
    occurrences: &[SentenceOccurrence],
    side_kind: OccurrenceSide,
    min_context: usize,
    candidate_limit: usize,
) -> CrossGranularityResult<Vec<CrossGranularitySegment>> {
    let mut by_position = Vec::<(TrustedStreamPosition, Option<usize>)>::new();
    let position_limit = checked(candidate_limit.checked_mul(2))?;
    for (index, occurrence) in occurrences.iter().enumerate() {
        if let Some(position) = occurrence.trusted_position {
            if by_position.len() == position_limit {
                return Err(CrossGranularityStopReason::CandidateCountLimit);
            }
            reserve(by_position.try_reserve(1))?;
            by_position.push((position, Some(index)));
        }
    }
    by_position.sort_unstable_by_key(|entry| entry.0);
    let mut read = 0usize;
    let mut write = 0usize;
    while read < by_position.len() {
        let position = by_position[read].0;
        let end = read + by_position[read..].partition_point(|entry| entry.0 == position);
        by_position[write] = (
            position,
            (end - read == 1).then_some(by_position[read].1).flatten(),
        );
        write += 1;
        read = end;
    }
    by_position.truncate(write);

    let mut segments = Vec::new();
    for (first_index, first) in occurrences.iter().enumerate() {
        let (Some(position), Some(span_index), Some(role)) =
            (first.trusted_position, first.span_index, first.role)
        else {
            continue;
        };
        if first.kind != RecoveryUnitKind::Sentence
            || by_position
                .binary_search_by_key(&position, |entry| entry.0)
                .ok()
                .and_then(|index| by_position.get(index)?.1)
                != Some(first_index)
        {
            continue;
        }
        let next_position = TrustedStreamPosition {
            stream_index: position.stream_index,
            ordinal: checked(position.ordinal.checked_add(1))?,
        };
        let Some(next_index) = by_position
            .binary_search_by_key(&next_position, |entry| entry.0)
            .ok()
            .and_then(|index| by_position.get(index)?.1)
        else {
            continue;
        };
        let next = evidence(occurrences.get(next_index))?;
        if next.kind != RecoveryUnitKind::Sentence
            || next.span_index != Some(span_index)
            || next.role != Some(role)
        {
            continue;
        }
        let Some((first_block, _, first_boundary)) =
            occurrence_boundary_source(side, first, OccurrenceBoundary::End)
        else {
            continue;
        };
        let Some((next_block, _, next_boundary)) =
            occurrence_boundary_source(side, next, OccurrenceBoundary::Start)
        else {
            continue;
        };
        if first_block != next_block
            || first_boundary.comparable.end > next_boundary.comparable.start
            || first_boundary.canonical.end > next_boundary.canonical.start
        {
            continue;
        }
        let comparable_gap = next_boundary
            .comparable
            .start
            .checked_sub(first_boundary.comparable.end)
            .ok_or(CrossGranularityStopReason::EvidenceUnavailable)?;
        let canonical_gap = next_boundary
            .canonical
            .start
            .checked_sub(first_boundary.canonical.end)
            .ok_or(CrossGranularityStopReason::EvidenceUnavailable)?;
        if comparable_gap != canonical_gap || comparable_gap > MAX_CROSS_GRANULARITY_EDIT_WINDOW {
            continue;
        }
        let context_with_headroom =
            checked(min_context.checked_add(MAX_CROSS_GRANULARITY_EDIT_WINDOW))?;
        let Some(comparable_start) = first_boundary
            .comparable
            .end
            .checked_sub(context_with_headroom)
        else {
            continue;
        };
        let Some(comparable_end) = next_boundary
            .comparable
            .start
            .checked_add(context_with_headroom)
        else {
            continue;
        };
        let Some(canonical_start) = first_boundary
            .canonical
            .end
            .checked_sub(context_with_headroom)
        else {
            continue;
        };
        let Some(canonical_end) = next_boundary
            .canonical
            .start
            .checked_add(context_with_headroom)
        else {
            continue;
        };
        let segment = CrossGranularitySegment {
            id: CrossGranularitySegmentId {
                side: side_kind,
                stream_index: position.stream_index,
                start_ordinal: position.ordinal,
                end_ordinal: checked(next_position.ordinal.checked_add(1))?,
            },
            block: first_block,
            canonical: ScalarRange {
                start: canonical_start,
                end: canonical_end,
            },
            comparable: TokenRange {
                start: comparable_start,
                end: comparable_end,
            },
            span_index,
            role,
        };
        if cross_granularity_segment_tokens(side, &segment).is_none() {
            continue;
        }
        if segments.len() == candidate_limit {
            return Err(CrossGranularityStopReason::CandidateCountLimit);
        }
        reserve(segments.try_reserve(1))?;
        segments.push(segment);
    }
    Ok(segments)
}

fn cross_granularity_segment_tokens<'a>(
    side: &'a Side<'_>,
    segment: &CrossGranularitySegment,
) -> Option<&'a [ComparableToken]> {
    let tokens = side
        .canonical
        .get(*side.index.get(&segment.block)?)?
        .get(segment.comparable.start..segment.comparable.end)?;
    if tokens.len() != segment.canonical.end.checked_sub(segment.canonical.start)?
        || tokens
            .iter()
            .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    {
        return None;
    }
    Some(tokens)
}

fn increment_cross_granularity_count<K: Eq + std::hash::Hash>(
    counts: &mut HashMap<K, usize>,
    key: K,
    limit: usize,
) -> CrossGranularityResult<()> {
    if let Some(count) = counts.get_mut(&key) {
        *count = checked(count.checked_add(1))?;
        return Ok(());
    }
    if counts.len() == limit {
        return Err(CrossGranularityStopReason::CandidateCountLimit);
    }
    reserve(counts.try_reserve(1))?;
    counts.insert(key, 1);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_cross_granularity_pairs(
    segment_side: &Side<'_>,
    segments: &[CrossGranularitySegment],
    singleton_side: &Side<'_>,
    singleton_occurrences: &[SentenceOccurrence],
    singleton_side_kind: OccurrenceSide,
    min_context: usize,
    posting_limit: usize,
    pair_limit: usize,
    comparison_limit: usize,
    candidate_limit: usize,
    posting_visits: &mut usize,
    pair_attempts: &mut usize,
    comparisons: &mut usize,
    pairs: &mut Vec<CrossGranularityPair>,
    segment_counts: &mut HashMap<CrossGranularitySegmentId, usize>,
    singleton_counts: &mut HashMap<(OccurrenceSide, usize, usize, usize), usize>,
) -> CrossGranularityResult<()> {
    let mut singleton_by_span = HashMap::<(usize, BlockRole), Vec<usize>>::new();
    let mut posting_items = 0usize;
    for (index, occurrence) in singleton_occurrences.iter().enumerate() {
        let (Some(span), Some(role)) = (occurrence.span_index, occurrence.role) else {
            continue;
        };
        if occurrence.kind != RecoveryUnitKind::Line
            || single_block_occurrence_source(singleton_side, occurrence).is_none()
        {
            continue;
        }
        posting_items = checked(posting_items.checked_add(1))?;
        if posting_items > candidate_limit {
            return Err(CrossGranularityStopReason::CandidateCountLimit);
        }
        if !singleton_by_span.contains_key(&(span, role)) {
            reserve(singleton_by_span.try_reserve(1))?;
        }
        let posting = singleton_by_span.entry((span, role)).or_default();
        reserve(posting.try_reserve(1))?;
        posting.push(index);
    }

    for segment in segments {
        let segment_tokens = evidence(cross_granularity_segment_tokens(segment_side, segment))?;
        let Some(singletons) = singleton_by_span.get(&(segment.span_index, segment.role)) else {
            continue;
        };
        for singleton_index in singletons {
            let singleton = evidence(singleton_occurrences.get(*singleton_index))?;
            let (_, singleton_tokens, singleton_location) =
                evidence(single_block_occurrence_source(singleton_side, singleton))?;
            if singleton_tokens.len() < segment_tokens.len() {
                continue;
            }
            for offset in 0..=singleton_tokens.len() - segment_tokens.len() {
                *posting_visits = checked(posting_visits.checked_add(1))?;
                if *posting_visits > posting_limit {
                    return Err(CrossGranularityStopReason::Work(
                        NearRelationStopReason::CandidatePostingVisitLimit,
                    ));
                }
                let candidate = evidence(
                    singleton_tokens
                        .get(offset..checked(offset.checked_add(segment_tokens.len()))?),
                )?;
                if candidate.first() != segment_tokens.first()
                    || candidate.last() != segment_tokens.last()
                {
                    continue;
                }
                *pair_attempts = checked(pair_attempts.checked_add(1))?;
                if *pair_attempts > pair_limit {
                    return Err(CrossGranularityStopReason::Work(
                        NearRelationStopReason::PairVisitLimit,
                    ));
                }
                let Some(relation) = cross_granularity_window_relation(
                    segment_tokens,
                    candidate,
                    min_context,
                    comparisons,
                    comparison_limit,
                )?
                else {
                    continue;
                };
                let comparable_start = singleton_location
                    .recovery
                    .comparable
                    .start
                    .checked_add(offset)
                    .ok_or(CrossGranularityStopReason::CounterOverflow)?;
                let comparable_end = checked(comparable_start.checked_add(segment_tokens.len()))?;
                let canonical_start = singleton_location
                    .recovery
                    .canonical
                    .start
                    .checked_add(offset)
                    .ok_or(CrossGranularityStopReason::CounterOverflow)?;
                let canonical_end = checked(canonical_start.checked_add(segment_tokens.len()))?;
                let singleton_key = (
                    singleton_side_kind,
                    *singleton_index,
                    comparable_start,
                    comparable_end,
                );
                increment_cross_granularity_count(segment_counts, segment.id, candidate_limit)?;
                increment_cross_granularity_count(
                    singleton_counts,
                    singleton_key,
                    candidate_limit,
                )?;
                let CrossGranularityWindowRelation::Near(score) = relation else {
                    continue;
                };
                if pairs.len() == candidate_limit {
                    return Err(CrossGranularityStopReason::CandidateCountLimit);
                }
                reserve(pairs.try_reserve(1))?;
                pairs.push(CrossGranularityPair {
                    segment: *segment,
                    singleton_side: singleton_side_kind,
                    singleton_index: *singleton_index,
                    singleton_canonical: ScalarRange {
                        start: canonical_start,
                        end: canonical_end,
                    },
                    singleton_comparable: TokenRange {
                        start: comparable_start,
                        end: comparable_end,
                    },
                    score,
                });
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cross_granularity_pair_is_globally_unique(
    pair: &CrossGranularityPair,
    segment_side: &Side<'_>,
    segments: &[CrossGranularitySegment],
    singleton_side: &Side<'_>,
    singleton_occurrences: &[SentenceOccurrence],
    min_context: usize,
    posting_limit: usize,
    pair_limit: usize,
    comparison_limit: usize,
    posting_visits: &mut usize,
    pair_attempts: &mut usize,
    comparisons: &mut usize,
) -> CrossGranularityResult<bool> {
    let segment_tokens = evidence(cross_granularity_segment_tokens(
        segment_side,
        &pair.segment,
    ))?;
    for (singleton_index, occurrence) in singleton_occurrences.iter().enumerate() {
        if occurrence.kind != RecoveryUnitKind::Line || occurrence.role != Some(pair.segment.role) {
            continue;
        }
        let Some((_, singleton_tokens, singleton_location)) =
            single_block_occurrence_source(singleton_side, occurrence)
        else {
            continue;
        };
        if singleton_tokens.len() < segment_tokens.len() {
            continue;
        }
        for offset in 0..=singleton_tokens.len() - segment_tokens.len() {
            *posting_visits = checked(posting_visits.checked_add(1))?;
            if *posting_visits > posting_limit {
                return Err(CrossGranularityStopReason::Work(
                    NearRelationStopReason::CandidatePostingVisitLimit,
                ));
            }
            let candidate = evidence(
                singleton_tokens.get(offset..checked(offset.checked_add(segment_tokens.len()))?),
            )?;
            if candidate.first() != segment_tokens.first()
                || candidate.last() != segment_tokens.last()
            {
                continue;
            }
            *pair_attempts = checked(pair_attempts.checked_add(1))?;
            if *pair_attempts > pair_limit {
                return Err(CrossGranularityStopReason::Work(
                    NearRelationStopReason::PairVisitLimit,
                ));
            }
            if cross_granularity_window_relation(
                segment_tokens,
                candidate,
                min_context,
                comparisons,
                comparison_limit,
            )?
            .is_none()
            {
                continue;
            }
            let comparable_start = singleton_location
                .recovery
                .comparable
                .start
                .checked_add(offset)
                .ok_or(CrossGranularityStopReason::CounterOverflow)?;
            let comparable_end = checked(comparable_start.checked_add(segment_tokens.len()))?;
            if singleton_index != pair.singleton_index
                || comparable_start != pair.singleton_comparable.start
                || comparable_end != pair.singleton_comparable.end
            {
                return Ok(false);
            }
        }
    }

    let singleton = evidence(singleton_occurrences.get(pair.singleton_index))?;
    let (_, singleton_tokens, singleton_location) =
        evidence(single_block_occurrence_source(singleton_side, singleton))?;
    let singleton_offset = pair
        .singleton_comparable
        .start
        .checked_sub(singleton_location.recovery.comparable.start)
        .ok_or(CrossGranularityStopReason::EvidenceUnavailable)?;
    let singleton_end = checked(singleton_offset.checked_add(segment_tokens.len()))?;
    let candidate = evidence(singleton_tokens.get(singleton_offset..singleton_end))?;
    for segment in segments {
        if segment.id == pair.segment.id || segment.role != pair.segment.role {
            continue;
        }
        let competitor = evidence(cross_granularity_segment_tokens(segment_side, segment))?;
        if competitor.len() != candidate.len() {
            continue;
        }
        *posting_visits = checked(posting_visits.checked_add(1))?;
        if *posting_visits > posting_limit {
            return Err(CrossGranularityStopReason::Work(
                NearRelationStopReason::CandidatePostingVisitLimit,
            ));
        }
        if competitor.first() != candidate.first() || competitor.last() != candidate.last() {
            continue;
        }
        *pair_attempts = checked(pair_attempts.checked_add(1))?;
        if *pair_attempts > pair_limit {
            return Err(CrossGranularityStopReason::Work(
                NearRelationStopReason::PairVisitLimit,
            ));
        }
        if cross_granularity_window_relation(
            competitor,
            candidate,
            min_context,
            comparisons,
            comparison_limit,
        )?
        .is_some()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn record_cross_granularity_work(
    budget: &mut RecoveryBudget,
    same_span: CrossGranularityWork,
    cross_span: CrossGranularityWork,
) {
    for (scope, work) in [
        (NearSearchScope::SameOrAmbiguousSpan, same_span),
        (NearSearchScope::CrossSpan, cross_span),
    ] {
        let _ = budget.charge_candidate_posting_visits_in_scope_split(
            work.posting_visits,
            RecoveryUnitKind::Line,
            CandidatePostingKind::Edge,
            scope,
            NearSearchWorkSplit::shared(work.posting_visits),
        );
        let _ = budget.charge_pair_visits_in_scope_split(
            work.pair_attempts,
            RecoveryUnitKind::Sentence,
            scope,
            NearSearchWorkSplit::shared(work.pair_attempts),
        );
        let _ = budget.charge_comparisons_in_scope_split(
            work.comparisons,
            RecoveryUnitKind::Sentence,
            scope,
            NearSearchWorkSplit::shared(work.comparisons),
        );
    }
}

fn stop_cross_granularity_recovery(
    budget: &mut RecoveryBudget,
    same_span_work: CrossGranularityWork,
    cross_span_work: CrossGranularityWork,
    reason: CrossGranularityStopReason,
) -> CrossGranularityOutcome {
    record_cross_granularity_work(budget, same_span_work, cross_span_work);
    match reason {
        CrossGranularityStopReason::CandidateCountLimit => budget.record_candidate_count_limit(),
        CrossGranularityStopReason::OutputLimit => {}
        CrossGranularityStopReason::Work(reason) => {
            budget.near_relation_stop_reason.get_or_insert(reason);
        }
        CrossGranularityStopReason::AllocationFailure
        | CrossGranularityStopReason::CounterOverflow
        | CrossGranularityStopReason::EvidenceUnavailable => {
            budget.near_metrics_available = false;
        }
    }
    CrossGranularityOutcome::Stopped(reason)
}

pub(super) fn append_cross_granularity_replacements(
    plan: &mut SentenceRecoveryPlan,
    old_side: &Side<'_>,
    new_side: &Side<'_>,
    old_occurrences: &[SentenceOccurrence],
    new_occurrences: &[SentenceOccurrence],
    min_context: usize,
    budget: &mut RecoveryBudget,
) -> CrossGranularityOutcome {
    let mut same_span_work = CrossGranularityWork::default();
    let mut cross_span_work = CrossGranularityWork::default();
    let prepared: CrossGranularityResult<_> = (|| {
        let posting_limit = checked(
            budget
                .candidate_posting_visit_limit
                .checked_sub(budget.candidate_posting_visits),
        )?;
        let pair_limit = checked(budget.pair_visit_limit.checked_sub(budget.pair_visits))?;
        let comparison_limit = checked(budget.comparison_limit.checked_sub(budget.comparisons))?;
        let candidate_limit = budget.output_range_limit;
        let old_segments = collect_cross_granularity_segments(
            old_side,
            old_occurrences,
            OccurrenceSide::Old,
            min_context,
            candidate_limit,
        )?;
        let new_segments = collect_cross_granularity_segments(
            new_side,
            new_occurrences,
            OccurrenceSide::New,
            min_context,
            candidate_limit,
        )?;
        let mut pairs = Vec::new();
        let mut segment_counts = HashMap::<CrossGranularitySegmentId, usize>::new();
        let mut singleton_counts = HashMap::<(OccurrenceSide, usize, usize, usize), usize>::new();
        collect_cross_granularity_pairs(
            old_side,
            &old_segments,
            new_side,
            new_occurrences,
            OccurrenceSide::New,
            min_context,
            posting_limit,
            pair_limit,
            comparison_limit,
            candidate_limit,
            &mut same_span_work.posting_visits,
            &mut same_span_work.pair_attempts,
            &mut same_span_work.comparisons,
            &mut pairs,
            &mut segment_counts,
            &mut singleton_counts,
        )?;
        collect_cross_granularity_pairs(
            new_side,
            &new_segments,
            old_side,
            old_occurrences,
            OccurrenceSide::Old,
            min_context,
            posting_limit,
            pair_limit,
            comparison_limit,
            candidate_limit,
            &mut same_span_work.posting_visits,
            &mut same_span_work.pair_attempts,
            &mut same_span_work.comparisons,
            &mut pairs,
            &mut segment_counts,
            &mut singleton_counts,
        )?;

        pairs.retain(|pair| {
            segment_counts.get(&pair.segment.id) == Some(&1)
                && singleton_counts.get(&(
                    pair.singleton_side,
                    pair.singleton_index,
                    pair.singleton_comparable.start,
                    pair.singleton_comparable.end,
                )) == Some(&1)
        });

        let mut globally_unique = Vec::new();
        reserve(globally_unique.try_reserve_exact(pairs.len()))?;
        for pair in pairs {
            let (segment_side, segments, singleton_side, singleton_occurrences) =
                match pair.segment.id.side {
                    OccurrenceSide::Old => {
                        (old_side, old_segments.as_slice(), new_side, new_occurrences)
                    }
                    OccurrenceSide::New => {
                        (new_side, new_segments.as_slice(), old_side, old_occurrences)
                    }
                };
            if cross_granularity_pair_is_globally_unique(
                &pair,
                segment_side,
                segments,
                singleton_side,
                singleton_occurrences,
                min_context,
                posting_limit,
                pair_limit,
                comparison_limit,
                &mut cross_span_work.posting_visits,
                &mut cross_span_work.pair_attempts,
                &mut cross_span_work.comparisons,
            )? {
                globally_unique.push(pair);
            }
        }

        let mut pending = Vec::new();
        reserve(pending.try_reserve_exact(globally_unique.len()))?;
        for pair in globally_unique {
            let segment_location = evidence(cross_granularity_segment_location(pair.segment))?;
            let singleton_occurrence = match pair.singleton_side {
                OccurrenceSide::Old => evidence(old_occurrences.get(pair.singleton_index))?,
                OccurrenceSide::New => evidence(new_occurrences.get(pair.singleton_index))?,
            };
            let singleton_location = evidence(cross_granularity_singleton_location(
                singleton_occurrence,
                pair.singleton_canonical,
                pair.singleton_comparable,
            ))?;
            let (old, new) = match pair.segment.id.side {
                OccurrenceSide::Old => (segment_location, singleton_location),
                OccurrenceSide::New => (singleton_location, segment_location),
            };
            if old
                .consumed
                .iter()
                .any(|range| local_sentence_ranges_overlap(range, &plan.deletion_consumed))
                || new
                    .consumed
                    .iter()
                    .any(|range| local_sentence_ranges_overlap(range, &plan.insertion_consumed))
            {
                continue;
            }
            pending.push(RecoveredReplacement {
                origin: ChangeOrigin::CrossGranularity,
                old: old.recovery,
                new: new.recovery,
                old_consumed: old.consumed,
                new_consumed: new.consumed,
                relation: RecoveryRelationEvidence {
                    old_best_score: pair.score,
                    old_second_score: 0,
                    old_best_scope: None,
                    new_best_score: pair.score,
                    new_second_score: 0,
                    new_best_scope: None,
                },
                hunk_policy: RecoveryHunkPolicy::Atomic,
                repeated_group: None,
                edits: None,
            });
        }

        let rejected = reject_cross_granularity_range_conflicts(&pending)?;
        let mut retained_index = 0usize;
        pending.retain(|_| {
            let retained = !rejected.get(retained_index).copied().unwrap_or(true);
            retained_index += 1;
            retained
        });

        let source_tokens = pending.iter().try_fold(0usize, |total, replacement| {
            checked(total.checked_add(replacement.old.source_tokens))
                .and_then(|total| checked(total.checked_add(replacement.new.source_tokens)))
        })?;
        let old_consumed = pending.iter().try_fold(0usize, |total, replacement| {
            checked(total.checked_add(replacement.old_consumed.len()))
        })?;
        let new_consumed = pending.iter().try_fold(0usize, |total, replacement| {
            checked(total.checked_add(replacement.new_consumed.len()))
        })?;
        let mut trial_budget = *budget;
        record_cross_granularity_work(&mut trial_budget, same_span_work, cross_span_work);
        if let Some(reason) = trial_budget.near_relation_stop_reason {
            return Err(CrossGranularityStopReason::Work(reason));
        }
        let output_ranges = checked(pending.len().checked_mul(2))?;
        if !trial_budget.charge_outputs(output_ranges, source_tokens) {
            return Err(CrossGranularityStopReason::OutputLimit);
        }
        Ok((pending, old_consumed, new_consumed, trial_budget))
    })();
    let (pending, old_consumed, new_consumed, trial_budget) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => {
            return stop_cross_granularity_recovery(
                budget,
                same_span_work,
                cross_span_work,
                reason,
            );
        }
    };
    if reserve(plan.replacements.try_reserve_exact(pending.len())).is_err()
        || reserve(plan.deletion_consumed.try_reserve_exact(old_consumed)).is_err()
        || reserve(plan.insertion_consumed.try_reserve_exact(new_consumed)).is_err()
    {
        return stop_cross_granularity_recovery(
            budget,
            same_span_work,
            cross_span_work,
            CrossGranularityStopReason::AllocationFailure,
        );
    }
    let committed = pending.len();
    for replacement in pending {
        plan.deletion_consumed
            .extend(replacement.old_consumed.iter().copied());
        plan.insertion_consumed
            .extend(replacement.new_consumed.iter().copied());
        plan.replacements.push(replacement);
    }
    sort_replacement_recoveries(plan);
    *budget = trial_budget;
    CrossGranularityOutcome::Complete(committed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_location(range: LocalSentenceRange, span_index: usize) -> SentenceLocation {
        SentenceLocation {
            recovery: RecoveredSentence {
                origin: ChangeOrigin::CrossGranularity,
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
    fn cross_granularity_test_block(block: u64, text: &str) -> crate::normalize::BlockText {
        use crate::normalize::{BlockText, MappedText};

        let mapped = MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        };
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped.clone(),
            canonical: mapped,
            matching: text.to_owned(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    fn cross_granularity_test_occurrence(
        kind: RecoveryUnitKind,
        span_index: usize,
        stream_index: usize,
        ordinal: usize,
        location: SentenceLocation,
    ) -> SentenceOccurrence {
        let mut occurrence = positioned_occurrence("cross", 0, stream_index, ordinal);
        occurrence.kind = kind;
        occurrence.span_index = Some(span_index);
        occurrence.role = Some(BlockRole::Body);
        occurrence.location = Some(location);
        occurrence
    }

    fn cross_granularity_test_side<'a>(blocks: &'a [crate::normalize::BlockText]) -> Side<'a> {
        let index = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.block, index))
            .collect();
        let canonical = blocks
            .iter()
            .map(|block| {
                block
                    .canonical
                    .text
                    .chars()
                    .map(ComparableToken::Scalar)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let total_tokens = canonical.iter().map(Vec::len).sum();
        Side {
            blocks,
            index,
            canonical,
            total_tokens,
        }
    }

    #[test]
    fn cross_granularity_window_accepts_punctuation_and_case_at_sentence_boundary() {
        let old = "abcdefghijklmnopqrst. For example, uvwxyz"
            .chars()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let new = "abcdefghijklmnopqrst; for example, uvwxyz"
            .chars()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let mut comparisons = 0;

        assert_eq!(
            cross_granularity_window_relation(&old, &new, 16, &mut comparisons, 1_000),
            Ok(Some(CrossGranularityWindowRelation::Near(9_512)))
        );
        assert_eq!(comparisons, old.len());
    }

    #[test]
    fn cross_granularity_window_rejects_unsafe_or_weak_changes() {
        let tokens = |text: &str| {
            text.chars()
                .map(ComparableToken::Scalar)
                .collect::<Vec<_>>()
        };
        let old = tokens("abcdefghijklmnopqrst. For example, uvwxyz");
        let unsafe_letter = tokens("abcdefghijklmnopqrst. Xor example, uvwxyz");
        let three_substitutions = tokens("abcdefghijklmnopqrst; for-example, uvwxyz");
        let weak_left_context = tokens("abcdefghijklmno; For example, uvwxyz0123456789");
        let weak_left_baseline = tokens("abcdefghijklmno. For example, uvwxyz0123456789");

        let mut comparisons = 0;
        assert_eq!(
            cross_granularity_window_relation(&old, &unsafe_letter, 16, &mut comparisons, 10_000,),
            Ok(None)
        );
        assert_eq!(
            cross_granularity_window_relation(
                &old,
                &three_substitutions,
                16,
                &mut comparisons,
                10_000,
            ),
            Ok(None)
        );
        assert_eq!(
            cross_granularity_window_relation(
                &weak_left_baseline,
                &weak_left_context,
                16,
                &mut comparisons,
                10_000,
            ),
            Ok(None)
        );
    }

    #[test]
    fn cross_granularity_range_sweep_allows_adjacency_and_rejects_overlap() {
        let source_range = |start, end, replacement| CrossGranularitySourceRange {
            side: OccurrenceSide::Old,
            block: BlockId(1),
            start,
            end,
            replacement,
        };
        let mut ranges = [
            source_range(0, 4, 0),
            source_range(4, 8, 1),
            source_range(7, 10, 2),
            source_range(12, 16, 3),
        ];

        let rejected =
            cross_granularity_range_conflicts(&mut ranges, 4).expect("the bounded sweep completes");

        assert_eq!(rejected, [false, true, true, false]);
    }

    #[test]
    fn cross_granularity_exact_counterpart_vetoes_near_replacement() {
        let old_text = "abcdefghijklmnop. Forqrstuvwxyz";
        let old_blocks = [cross_granularity_test_block(1, old_text)];
        let old_side = cross_granularity_test_side(&old_blocks);
        let old_occurrences = [
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Sentence,
                0,
                1,
                0,
                test_location(
                    LocalSentenceRange {
                        block: BlockId(1),
                        canonical: ScalarRange { start: 0, end: 17 },
                        comparable: TokenRange { start: 0, end: 17 },
                    },
                    0,
                ),
            ),
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Sentence,
                0,
                1,
                1,
                test_location(
                    LocalSentenceRange {
                        block: BlockId(1),
                        canonical: ScalarRange { start: 18, end: 31 },
                        comparable: TokenRange { start: 18, end: 31 },
                    },
                    0,
                ),
            ),
        ];
        let segments = collect_cross_granularity_segments(
            &old_side,
            &old_occurrences,
            OccurrenceSide::Old,
            4,
            100,
        )
        .expect("the adjacent segment is available");
        let exact = cross_granularity_segment_tokens(&old_side, &segments[0])
            .expect("the segment has scalar source")
            .iter()
            .filter_map(|token| match token {
                ComparableToken::Scalar(scalar) => Some(*scalar),
                _ => None,
            })
            .collect::<String>();
        let near = exact.replacen('.', ";", 1).replacen('F', "f", 1);
        let new_blocks = [
            cross_granularity_test_block(2, &near),
            cross_granularity_test_block(3, &exact),
        ];
        let new_side = cross_granularity_test_side(&new_blocks);
        let line = |block, text: &str| {
            let end = text.chars().count();
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Line,
                0,
                block as usize,
                0,
                test_location(
                    LocalSentenceRange {
                        block: BlockId(block),
                        canonical: ScalarRange { start: 0, end },
                        comparable: TokenRange { start: 0, end },
                    },
                    0,
                ),
            )
        };
        let new_occurrences = [line(2, &near), line(3, &exact)];
        let mut pairs = Vec::new();
        let mut segment_counts = HashMap::new();
        let mut singleton_counts = HashMap::new();
        let mut posting_visits = 0;
        let mut pair_attempts = 0;
        let mut comparisons = 0;

        collect_cross_granularity_pairs(
            &old_side,
            &segments,
            &new_side,
            &new_occurrences,
            OccurrenceSide::New,
            4,
            1_000,
            1_000,
            10_000,
            100,
            &mut posting_visits,
            &mut pair_attempts,
            &mut comparisons,
            &mut pairs,
            &mut segment_counts,
            &mut singleton_counts,
        )
        .expect("the bounded candidate scan completes");

        assert_eq!(pairs.len(), 1);
        assert_eq!(segment_counts.get(&segments[0].id), Some(&2));
    }

    #[test]
    fn cross_granularity_cross_span_exact_counterpart_vetoes_local_pair() {
        let exact = "jklmnop. Forqrstu";
        let near = "jklmnop; forqrstu";
        let old_blocks = [cross_granularity_test_block(10, exact)];
        let old_side = cross_granularity_test_side(&old_blocks);
        let segment = CrossGranularitySegment {
            id: CrossGranularitySegmentId {
                side: OccurrenceSide::Old,
                stream_index: 1,
                start_ordinal: 0,
                end_ordinal: 2,
            },
            block: BlockId(10),
            canonical: ScalarRange {
                start: 0,
                end: exact.chars().count(),
            },
            comparable: TokenRange {
                start: 0,
                end: exact.chars().count(),
            },
            span_index: 0,
            role: BlockRole::Body,
        };
        let new_blocks = [
            cross_granularity_test_block(11, near),
            cross_granularity_test_block(12, exact),
        ];
        let new_side = cross_granularity_test_side(&new_blocks);
        let line = |block, text: &str, span_index| {
            let end = text.chars().count();
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Line,
                span_index,
                block as usize,
                0,
                test_location(
                    LocalSentenceRange {
                        block: BlockId(block),
                        canonical: ScalarRange { start: 0, end },
                        comparable: TokenRange { start: 0, end },
                    },
                    span_index,
                ),
            )
        };
        let new_occurrences = [line(11, near, 0), line(12, exact, 1)];
        let pair = CrossGranularityPair {
            segment,
            singleton_side: OccurrenceSide::New,
            singleton_index: 0,
            singleton_canonical: ScalarRange {
                start: 0,
                end: near.chars().count(),
            },
            singleton_comparable: TokenRange {
                start: 0,
                end: near.chars().count(),
            },
            score: 8_888,
        };
        let mut pair_attempts = 0;
        let mut comparisons = 0;
        let mut posting_visits = 0;

        assert_eq!(
            cross_granularity_pair_is_globally_unique(
                &pair,
                &old_side,
                std::slice::from_ref(&segment),
                &new_side,
                &new_occurrences,
                4,
                1_000,
                1_000,
                10_000,
                &mut posting_visits,
                &mut pair_attempts,
                &mut comparisons,
            ),
            Ok(false)
        );
    }

    #[test]
    fn cross_granularity_segment_uses_shared_boundary_block_of_multiblock_sentence() {
        let term = "Association";
        let definition = "abcdefghijklmnop. Forqrstuvwxyz";
        let blocks = [
            cross_granularity_test_block(20, term),
            cross_granularity_test_block(21, definition),
        ];
        let side = cross_granularity_test_side(&blocks);
        let term_end = term.chars().count();
        let first_definition = LocalSentenceRange {
            block: BlockId(21),
            canonical: ScalarRange { start: 0, end: 17 },
            comparable: TokenRange { start: 0, end: 17 },
        };
        let first_location = SentenceLocation {
            recovery: RecoveredSentence {
                origin: ChangeOrigin::CrossGranularity,
                span_index: 0,
                kind: RecoveryUnitKind::Sentence,
                role: OccurrenceRole::Body,
                blocks: vec![BlockId(20), BlockId(21)],
                separator: Some(BlockSeparator::Space),
                canonical: ScalarRange {
                    start: 0,
                    end: term_end + 1 + 17,
                },
                comparable: TokenRange {
                    start: 0,
                    end: term_end + 1 + 17,
                },
                source_tokens: term_end + 17,
            },
            consumed: vec![
                LocalSentenceRange {
                    block: BlockId(20),
                    canonical: ScalarRange {
                        start: 0,
                        end: term_end,
                    },
                    comparable: TokenRange {
                        start: 0,
                        end: term_end,
                    },
                },
                first_definition,
            ],
        };
        let next_range = LocalSentenceRange {
            block: BlockId(21),
            canonical: ScalarRange { start: 18, end: 31 },
            comparable: TokenRange { start: 18, end: 31 },
        };
        let occurrences = [
            cross_granularity_test_occurrence(RecoveryUnitKind::Sentence, 0, 1, 0, first_location),
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Sentence,
                0,
                1,
                1,
                test_location(next_range, 0),
            ),
        ];

        let segments =
            collect_cross_granularity_segments(&side, &occurrences, OccurrenceSide::Old, 4, 100)
                .expect("the multiblock sentence exposes its shared boundary block");

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].block, BlockId(21));
        assert_eq!(segments[0].comparable, TokenRange { start: 9, end: 26 });
    }

    fn cross_granularity_appender_fixture() -> (
        Vec<crate::normalize::BlockText>,
        Vec<SentenceOccurrence>,
        Vec<crate::normalize::BlockText>,
        Vec<SentenceOccurrence>,
    ) {
        let old_text = "abcdefghijklmnop. Forqrstuvwxyz";
        let new_text = "abcdefghijklmnop; forqrstuvwxyz";
        let old_blocks = vec![cross_granularity_test_block(30, old_text)];
        let new_blocks = vec![cross_granularity_test_block(31, new_text)];
        let old_occurrences = vec![
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Sentence,
                0,
                1,
                0,
                test_location(
                    LocalSentenceRange {
                        block: BlockId(30),
                        canonical: ScalarRange { start: 0, end: 17 },
                        comparable: TokenRange { start: 0, end: 17 },
                    },
                    0,
                ),
            ),
            cross_granularity_test_occurrence(
                RecoveryUnitKind::Sentence,
                0,
                1,
                1,
                test_location(
                    LocalSentenceRange {
                        block: BlockId(30),
                        canonical: ScalarRange { start: 18, end: 31 },
                        comparable: TokenRange { start: 18, end: 31 },
                    },
                    0,
                ),
            ),
        ];
        let new_occurrences = vec![cross_granularity_test_occurrence(
            RecoveryUnitKind::Line,
            0,
            2,
            0,
            test_location(
                LocalSentenceRange {
                    block: BlockId(31),
                    canonical: ScalarRange { start: 0, end: 31 },
                    comparable: TokenRange { start: 0, end: 31 },
                },
                0,
            ),
        )];
        (old_blocks, old_occurrences, new_blocks, new_occurrences)
    }

    #[test]
    fn cross_granularity_appender_commits_one_atomic_replacement() {
        let (old_blocks, old_occurrences, new_blocks, new_occurrences) =
            cross_granularity_appender_fixture();
        let old_side = cross_granularity_test_side(&old_blocks);
        let new_side = cross_granularity_test_side(&new_blocks);
        let mut plan = SentenceRecoveryPlan::default();
        let mut budget =
            RecoveryBudget::new(old_side.total_tokens, new_side.total_tokens, 10_000, 4)
                .expect("the fixture fits the recovery budget");

        let committed = append_cross_granularity_replacements(
            &mut plan,
            &old_side,
            &new_side,
            &old_occurrences,
            &new_occurrences,
            4,
            &mut budget,
        );

        assert_eq!(committed, CrossGranularityOutcome::Complete(1));
        assert_eq!(plan.replacements.len(), 1);
        assert_eq!(plan.replacements[0].hunk_policy, RecoveryHunkPolicy::Atomic);
        assert_eq!(plan.deletion_consumed.len(), 1);
        assert_eq!(plan.insertion_consumed.len(), 1);
        assert!(budget.candidate_posting_visits > 0);
        assert!(budget.comparisons > 0);
        assert_eq!(budget.near_relation_stop_reason, None);
    }

    #[test]
    fn cross_granularity_appender_records_stopped_work_without_committing() {
        let (old_blocks, old_occurrences, new_blocks, new_occurrences) =
            cross_granularity_appender_fixture();
        let old_side = cross_granularity_test_side(&old_blocks);
        let new_side = cross_granularity_test_side(&new_blocks);
        let preserved = LocalSentenceRange {
            block: BlockId(999),
            canonical: ScalarRange { start: 0, end: 1 },
            comparable: TokenRange { start: 0, end: 1 },
        };
        let mut plan = SentenceRecoveryPlan {
            deletion_consumed: vec![preserved],
            ..SentenceRecoveryPlan::default()
        };
        let mut budget =
            RecoveryBudget::new(old_side.total_tokens, new_side.total_tokens, 10_000, 4)
                .expect("the fixture fits the recovery budget");
        budget.candidate_posting_visit_limit = 0;

        let committed = append_cross_granularity_replacements(
            &mut plan,
            &old_side,
            &new_side,
            &old_occurrences,
            &new_occurrences,
            4,
            &mut budget,
        );

        assert_eq!(
            committed,
            CrossGranularityOutcome::Stopped(CrossGranularityStopReason::Work(
                NearRelationStopReason::CandidatePostingVisitLimit,
            ))
        );
        assert!(plan.replacements.is_empty());
        assert_eq!(plan.deletion_consumed, [preserved]);
        assert!(plan.insertion_consumed.is_empty());
        assert_eq!(budget.candidate_posting_visits, 0);
        assert_eq!(budget.candidate_posting_visits_attempted, 1);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidatePostingVisitLimit)
        );
        assert_eq!(budget.output_ranges, 0);
        assert_eq!(budget.output_tokens, 0);
    }

    #[test]
    fn cross_granularity_candidate_cap_has_typed_stop_and_no_partial_commit() {
        let (old_blocks, old_occurrences, new_blocks, new_occurrences) =
            cross_granularity_appender_fixture();
        let old_side = cross_granularity_test_side(&old_blocks);
        let new_side = cross_granularity_test_side(&new_blocks);
        let mut plan = SentenceRecoveryPlan::default();
        let mut budget =
            RecoveryBudget::new(old_side.total_tokens, new_side.total_tokens, 10_000, 4)
                .expect("the fixture fits the recovery budget");
        budget.output_range_limit = 0;

        let outcome = append_cross_granularity_replacements(
            &mut plan,
            &old_side,
            &new_side,
            &old_occurrences,
            &new_occurrences,
            4,
            &mut budget,
        );

        assert_eq!(
            outcome,
            CrossGranularityOutcome::Stopped(CrossGranularityStopReason::CandidateCountLimit)
        );
        assert!(plan.replacements.is_empty());
        assert!(plan.deletion_consumed.is_empty());
        assert!(plan.insertion_consumed.is_empty());
        assert!(budget.candidate_count_truncated);
        assert_eq!(
            budget.near_relation_stop_reason,
            Some(NearRelationStopReason::CandidateCountLimit)
        );
    }

    #[test]
    fn cross_granularity_output_limit_has_typed_stop_and_no_partial_commit() {
        let (old_blocks, old_occurrences, new_blocks, new_occurrences) =
            cross_granularity_appender_fixture();
        let old_side = cross_granularity_test_side(&old_blocks);
        let new_side = cross_granularity_test_side(&new_blocks);
        let mut plan = SentenceRecoveryPlan::default();
        let mut budget =
            RecoveryBudget::new(old_side.total_tokens, new_side.total_tokens, 10_000, 4)
                .expect("the fixture fits the recovery budget");
        budget.output_range_limit = 1;

        let outcome = append_cross_granularity_replacements(
            &mut plan,
            &old_side,
            &new_side,
            &old_occurrences,
            &new_occurrences,
            4,
            &mut budget,
        );

        assert_eq!(
            outcome,
            CrossGranularityOutcome::Stopped(CrossGranularityStopReason::OutputLimit)
        );
        assert!(plan.replacements.is_empty());
        assert!(plan.deletion_consumed.is_empty());
        assert!(plan.insertion_consumed.is_empty());
        assert!(!budget.candidate_count_truncated);
        assert_eq!(budget.near_relation_stop_reason, None);
        assert!(budget.near_metrics_available);
        assert_eq!(budget.output_ranges, 0);
        assert_eq!(budget.output_tokens, 0);
    }

    #[test]
    fn cross_granularity_counter_failure_invalidates_metrics_without_public_stop() {
        let mut budget = RecoveryBudget::new(1, 1, 2, 1).expect("the budget is valid");

        let outcome = stop_cross_granularity_recovery(
            &mut budget,
            CrossGranularityWork::default(),
            CrossGranularityWork::default(),
            CrossGranularityStopReason::CounterOverflow,
        );

        assert_eq!(
            outcome,
            CrossGranularityOutcome::Stopped(CrossGranularityStopReason::CounterOverflow)
        );
        assert!(!budget.near_metrics_available);
        assert_eq!(budget.near_relation_stop_reason, None);
    }
}
