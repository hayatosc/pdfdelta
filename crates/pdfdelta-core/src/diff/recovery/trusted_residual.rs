//! Exact, globally unique pairing for source-backed trusted residual ranges.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    ops::Range,
};

use crate::layout::BlockRole;

/// Side-local identity and comparable-token extent of one residual candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::diff) struct TrustedResidualRange {
    pub candidate_id: usize,
    pub block_id: u64,
    pub comparable: Range<usize>,
}

/// Clean trusted residual evidence available for exact pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::diff) struct TrustedResidualCandidate<'a, T> {
    pub range: TrustedResidualRange,
    pub role: BlockRole,
    pub alignment_span: usize,
    /// Candidate-generation hash only. Token equality remains final evidence.
    pub exact_hash: u64,
    pub source_tokens: usize,
    pub comparable_tokens: &'a [T],
}

/// One exact, globally unique old/new residual relation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::diff) struct TrustedResidualPair {
    pub old: TrustedResidualRange,
    pub new: TrustedResidualRange,
    pub old_source_tokens: usize,
    pub new_source_tokens: usize,
}

/// Hard bounds for one selector invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct TrustedResidualSelectorLimits {
    pub max_candidates_per_side: usize,
    pub max_comparable_tokens: usize,
    pub max_pairs: usize,
}

/// Resource exhausted before a complete selection could be returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum TrustedResidualSelectorResource {
    Candidates,
    ComparableTokens,
    Pairs,
    Allocation,
}

/// Invalid candidate evidence that cannot safely participate in selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum TrustedResidualCandidateError {
    DuplicateId,
    EmptyRange,
    EmptyTokens,
    RangeLengthMismatch,
    EmptySource,
}

/// Side containing invalid candidate evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum TrustedResidualSide {
    Old,
    New,
}

/// Selector failure. No partial selection is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum TrustedResidualSelectorError {
    Limit(TrustedResidualSelectorResource),
    InvalidCandidate {
        side: TrustedResidualSide,
        candidate_id: usize,
        reason: TrustedResidualCandidateError,
    },
}

#[derive(Clone, Copy)]
struct Occurrence {
    index: usize,
    count: usize,
}

/// Selects exact pairs that occur once on each side and do not overlap another pair.
///
/// Exact-token multiplicity is global within each side. Alignment span, role, and
/// the caller-provided hash restrict pairing only after global uniqueness is known.
pub(in crate::diff) fn select_exact_trusted_residual_pairs<T: Eq + Hash>(
    old: &[TrustedResidualCandidate<'_, T>],
    new: &[TrustedResidualCandidate<'_, T>],
    limits: TrustedResidualSelectorLimits,
) -> Result<Vec<TrustedResidualPair>, TrustedResidualSelectorError> {
    validate_candidates(old, TrustedResidualSide::Old, limits)?;
    validate_candidates(new, TrustedResidualSide::New, limits)?;

    let old_occurrences = exact_occurrences(old)?;
    let new_occurrences = exact_occurrences(new)?;
    let pair_capacity = old.len().min(new.len()).min(limits.max_pairs);
    let mut candidates = Vec::new();
    candidates.try_reserve(pair_capacity).map_err(|_| {
        TrustedResidualSelectorError::Limit(TrustedResidualSelectorResource::Allocation)
    })?;

    for old_occurrence in old_occurrences.values().filter(|entry| entry.count == 1) {
        let old_candidate = &old[old_occurrence.index];
        let Some(new_occurrence) = new_occurrences.get(old_candidate.comparable_tokens) else {
            continue;
        };
        if new_occurrence.count != 1 {
            continue;
        }
        let new_candidate = &new[new_occurrence.index];
        if old_candidate.exact_hash != new_candidate.exact_hash
            || old_candidate.alignment_span != new_candidate.alignment_span
            || !old_candidate
                .role
                .is_alignment_compatible(new_candidate.role)
            || old_candidate.comparable_tokens != new_candidate.comparable_tokens
        {
            continue;
        }
        if candidates.len() == limits.max_pairs {
            return Err(TrustedResidualSelectorError::Limit(
                TrustedResidualSelectorResource::Pairs,
            ));
        }
        candidates.push(TrustedResidualPair {
            old: old_candidate.range.clone(),
            new: new_candidate.range.clone(),
            old_source_tokens: old_candidate.source_tokens,
            new_source_tokens: new_candidate.source_tokens,
        });
    }

    candidates.sort_by_key(pair_sort_key);
    let mut conflicted = Vec::new();
    conflicted
        .try_reserve_exact(candidates.len())
        .map_err(|_| {
            TrustedResidualSelectorError::Limit(TrustedResidualSelectorResource::Allocation)
        })?;
    conflicted.resize(candidates.len(), false);
    mark_overlap_conflicts(&candidates, &mut conflicted, PairSide::Old)?;
    mark_overlap_conflicts(&candidates, &mut conflicted, PairSide::New)?;
    let mut index = 0usize;
    candidates.retain(|_| {
        let retain = !conflicted[index];
        index += 1;
        retain
    });
    Ok(candidates)
}

fn validate_candidates<T>(
    candidates: &[TrustedResidualCandidate<'_, T>],
    side: TrustedResidualSide,
    limits: TrustedResidualSelectorLimits,
) -> Result<(), TrustedResidualSelectorError> {
    if candidates.len() > limits.max_candidates_per_side {
        return Err(TrustedResidualSelectorError::Limit(
            TrustedResidualSelectorResource::Candidates,
        ));
    }
    let comparable_tokens = candidates.iter().try_fold(0usize, |total, candidate| {
        total.checked_add(candidate.comparable_tokens.len())
    });
    if comparable_tokens.is_none_or(|total| total > limits.max_comparable_tokens) {
        return Err(TrustedResidualSelectorError::Limit(
            TrustedResidualSelectorResource::ComparableTokens,
        ));
    }

    let mut ids = HashSet::new();
    ids.try_reserve(candidates.len()).map_err(|_| {
        TrustedResidualSelectorError::Limit(TrustedResidualSelectorResource::Allocation)
    })?;
    for candidate in candidates {
        let reason = if !ids.insert(candidate.range.candidate_id) {
            Some(TrustedResidualCandidateError::DuplicateId)
        } else if candidate.range.comparable.is_empty() {
            Some(TrustedResidualCandidateError::EmptyRange)
        } else if candidate.comparable_tokens.is_empty() {
            Some(TrustedResidualCandidateError::EmptyTokens)
        } else if candidate.range.comparable.len() != candidate.comparable_tokens.len() {
            Some(TrustedResidualCandidateError::RangeLengthMismatch)
        } else if candidate.source_tokens == 0 {
            Some(TrustedResidualCandidateError::EmptySource)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(TrustedResidualSelectorError::InvalidCandidate {
                side,
                candidate_id: candidate.range.candidate_id,
                reason,
            });
        }
    }
    Ok(())
}

fn exact_occurrences<'a, T: Eq + Hash>(
    candidates: &'a [TrustedResidualCandidate<'a, T>],
) -> Result<HashMap<&'a [T], Occurrence>, TrustedResidualSelectorError> {
    let mut occurrences = HashMap::new();
    occurrences.try_reserve(candidates.len()).map_err(|_| {
        TrustedResidualSelectorError::Limit(TrustedResidualSelectorResource::Allocation)
    })?;
    for (index, candidate) in candidates.iter().enumerate() {
        occurrences
            .entry(candidate.comparable_tokens)
            .and_modify(|occurrence: &mut Occurrence| occurrence.count += 1)
            .or_insert(Occurrence { index, count: 1 });
    }
    Ok(occurrences)
}

#[derive(Clone, Copy)]
enum PairSide {
    Old,
    New,
}

fn mark_overlap_conflicts(
    pairs: &[TrustedResidualPair],
    conflicted: &mut [bool],
    side: PairSide,
) -> Result<(), TrustedResidualSelectorError> {
    let mut order = Vec::new();
    order.try_reserve_exact(pairs.len()).map_err(|_| {
        TrustedResidualSelectorError::Limit(TrustedResidualSelectorResource::Allocation)
    })?;
    order.extend(0..pairs.len());
    order.sort_unstable_by_key(|index| range_sort_key(pair_range(&pairs[*index], side)));

    let mut active: Option<(u64, usize, usize)> = None;
    for index in order {
        let range = pair_range(&pairs[index], side);
        match active {
            Some((block_id, max_end, max_end_index))
                if block_id == range.block_id && range.comparable.start < max_end =>
            {
                conflicted[index] = true;
                conflicted[max_end_index] = true;
                if range.comparable.end > max_end {
                    active = Some((range.block_id, range.comparable.end, index));
                }
            }
            _ => active = Some((range.block_id, range.comparable.end, index)),
        }
    }
    Ok(())
}

fn pair_range(pair: &TrustedResidualPair, side: PairSide) -> &TrustedResidualRange {
    match side {
        PairSide::Old => &pair.old,
        PairSide::New => &pair.new,
    }
}

fn range_sort_key(range: &TrustedResidualRange) -> (u64, usize, usize, usize) {
    (
        range.block_id,
        range.comparable.start,
        range.comparable.end,
        range.candidate_id,
    )
}

fn pair_sort_key(
    pair: &TrustedResidualPair,
) -> (u64, usize, usize, u64, usize, usize, usize, usize) {
    (
        pair.old.block_id,
        pair.old.comparable.start,
        pair.old.comparable.end,
        pair.new.block_id,
        pair.new.comparable.start,
        pair.new.comparable.end,
        pair.old.candidate_id,
        pair.new.candidate_id,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        TrustedResidualCandidate, TrustedResidualPair, TrustedResidualRange,
        TrustedResidualSelectorError, TrustedResidualSelectorLimits,
        TrustedResidualSelectorResource, select_exact_trusted_residual_pairs,
    };
    use crate::layout::BlockRole;

    const LIMITS: TrustedResidualSelectorLimits = TrustedResidualSelectorLimits {
        max_candidates_per_side: 32,
        max_comparable_tokens: 1_024,
        max_pairs: 32,
    };

    fn candidate(
        id: usize,
        block_id: u64,
        range: std::ops::Range<usize>,
        role: BlockRole,
        span: usize,
        hash: u64,
        tokens: &[u8],
    ) -> TrustedResidualCandidate<'_, u8> {
        TrustedResidualCandidate {
            range: TrustedResidualRange {
                candidate_id: id,
                block_id,
                comparable: range,
            },
            role,
            alignment_span: span,
            exact_hash: hash,
            source_tokens: tokens.len(),
            comparable_tokens: tokens,
        }
    }

    fn select(
        old: &[TrustedResidualCandidate<'_, u8>],
        new: &[TrustedResidualCandidate<'_, u8>],
    ) -> Vec<TrustedResidualPair> {
        select_exact_trusted_residual_pairs(old, new, LIMITS).expect("selection succeeds")
    }

    #[test]
    fn selects_unique_exact_pair() {
        let old = [candidate(1, 10, 2..5, BlockRole::Body, 4, 11, b"abc")];
        let new = [candidate(2, 20, 7..10, BlockRole::Body, 4, 11, b"abc")];

        let pairs = select(&old, &new);

        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].old.candidate_id, 1);
        assert_eq!(pairs[0].new.candidate_id, 2);
    }

    #[test]
    fn rejects_duplicate_on_either_side() {
        let old_duplicate = [
            candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(2, 11, 0..3, BlockRole::Body, 4, 11, b"abc"),
        ];
        let new_unique = [candidate(3, 20, 0..3, BlockRole::Body, 4, 11, b"abc")];
        assert!(select(&old_duplicate, &new_unique).is_empty());

        let old_unique = [candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc")];
        let new_duplicate = [
            candidate(3, 20, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(4, 21, 0..3, BlockRole::Body, 4, 11, b"abc"),
        ];
        assert!(select(&old_unique, &new_duplicate).is_empty());
    }

    #[test]
    fn hash_collision_does_not_create_pair() {
        let old = [candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc")];
        let new = [candidate(2, 20, 0..3, BlockRole::Body, 4, 11, b"xyz")];

        assert!(select(&old, &new).is_empty());
    }

    #[test]
    fn rejects_role_or_span_mismatch() {
        let old = [candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc")];
        let wrong_role = [candidate(
            2,
            20,
            0..3,
            BlockRole::RepeatedFooter,
            4,
            11,
            b"abc",
        )];
        let wrong_span = [candidate(3, 20, 0..3, BlockRole::Body, 5, 11, b"abc")];

        assert!(select(&old, &wrong_role).is_empty());
        assert!(select(&old, &wrong_span).is_empty());
    }

    #[test]
    fn rejects_every_pair_in_an_overlap_conflict() {
        let old = [
            candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(2, 10, 2..5, BlockRole::Body, 4, 12, b"def"),
        ];
        let new = [
            candidate(3, 20, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(4, 21, 0..3, BlockRole::Body, 4, 12, b"def"),
        ];

        assert!(select(&old, &new).is_empty());
    }

    #[test]
    fn rejects_every_pair_in_a_chained_overlap() {
        let old = [
            candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(2, 10, 2..5, BlockRole::Body, 4, 12, b"def"),
            candidate(3, 10, 4..7, BlockRole::Body, 4, 13, b"ghi"),
        ];
        let new = [
            candidate(4, 20, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(5, 21, 0..3, BlockRole::Body, 4, 12, b"def"),
            candidate(6, 22, 0..3, BlockRole::Body, 4, 13, b"ghi"),
        ];

        assert!(select(&old, &new).is_empty());
    }

    #[test]
    fn accepts_adjacent_ranges_in_the_same_block() {
        let old = [
            candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(2, 10, 3..6, BlockRole::Body, 4, 12, b"def"),
        ];
        let new = [
            candidate(3, 20, 0..3, BlockRole::Body, 4, 11, b"abc"),
            candidate(4, 20, 3..6, BlockRole::Body, 4, 12, b"def"),
        ];

        let pairs = select(&old, &new);

        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn returns_pairs_in_range_order_independent_of_input_order() {
        let old = [
            candidate(2, 20, 4..7, BlockRole::Body, 4, 12, b"def"),
            candidate(1, 10, 1..4, BlockRole::Body, 4, 11, b"abc"),
        ];
        let new = [
            candidate(4, 40, 5..8, BlockRole::Body, 4, 12, b"def"),
            candidate(3, 30, 2..5, BlockRole::Body, 4, 11, b"abc"),
        ];

        let pairs = select(&old, &new);

        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].old.candidate_id, 1);
        assert_eq!(pairs[1].old.candidate_id, 2);
    }

    #[test]
    fn limit_failure_returns_no_partial_selection() {
        let old = [candidate(1, 10, 0..3, BlockRole::Body, 4, 11, b"abc")];
        let new = [candidate(2, 20, 0..3, BlockRole::Body, 4, 11, b"abc")];
        let limits = TrustedResidualSelectorLimits {
            max_candidates_per_side: 32,
            max_comparable_tokens: 1_024,
            max_pairs: 0,
        };

        assert_eq!(
            select_exact_trusted_residual_pairs(&old, &new, limits),
            Err(TrustedResidualSelectorError::Limit(
                TrustedResidualSelectorResource::Pairs
            ))
        );
    }
}
