use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::diff) enum BoundarySide {
    Prefix,
    Suffix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct ExactBoundaryJoinFragment {
    pub(in crate::diff) group: u8,
    pub(in crate::diff) parent: usize,
    pub(in crate::diff) token_count: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::diff) struct ExactBoundaryJoinLimits {
    pub(in crate::diff) postings: usize,
    pub(in crate::diff) intersections: usize,
    pub(in crate::diff) admissions: usize,
    pub(in crate::diff) sort_items: usize,
    pub(in crate::diff) estimated_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::diff) struct ExactBoundaryJoinWork {
    pub(in crate::diff) postings_examined: usize,
    pub(in crate::diff) postings_attempted: usize,
    pub(in crate::diff) intersections_examined: usize,
    pub(in crate::diff) intersections_attempted: usize,
    pub(in crate::diff) admissions_examined: usize,
    pub(in crate::diff) admissions_attempted: usize,
    pub(in crate::diff) sort_items_examined: usize,
    pub(in crate::diff) sort_items_attempted: usize,
    pub(in crate::diff) estimated_bytes_examined: usize,
    pub(in crate::diff) estimated_bytes_attempted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum ExactBoundaryJoinStopReason {
    PostingLimit,
    IntersectionLimit,
    AdmissionLimit,
    SortLimit,
    EstimatedByteLimit,
    AllocationFailure,
    CounterOverflow,
    InvalidInput,
}

pub(in crate::diff) struct ExactBoundaryJoinBudget {
    limits: ExactBoundaryJoinLimits,
    work: ExactBoundaryJoinWork,
}

impl ExactBoundaryJoinBudget {
    pub(in crate::diff) fn new(limits: ExactBoundaryJoinLimits) -> Self {
        Self {
            limits,
            work: ExactBoundaryJoinWork::default(),
        }
    }

    pub(in crate::diff) fn work(&self) -> ExactBoundaryJoinWork {
        self.work
    }

    fn charge(
        &mut self,
        kind: ExactBoundaryJoinWorkKind,
        amount: usize,
    ) -> Result<(), ExactBoundaryJoinStopReason> {
        let (examined, attempted, limit, reason) = match kind {
            ExactBoundaryJoinWorkKind::Postings => (
                &mut self.work.postings_examined,
                &mut self.work.postings_attempted,
                self.limits.postings,
                ExactBoundaryJoinStopReason::PostingLimit,
            ),
            ExactBoundaryJoinWorkKind::Intersections => (
                &mut self.work.intersections_examined,
                &mut self.work.intersections_attempted,
                self.limits.intersections,
                ExactBoundaryJoinStopReason::IntersectionLimit,
            ),
            ExactBoundaryJoinWorkKind::Admissions => (
                &mut self.work.admissions_examined,
                &mut self.work.admissions_attempted,
                self.limits.admissions,
                ExactBoundaryJoinStopReason::AdmissionLimit,
            ),
            ExactBoundaryJoinWorkKind::SortItems => (
                &mut self.work.sort_items_examined,
                &mut self.work.sort_items_attempted,
                self.limits.sort_items,
                ExactBoundaryJoinStopReason::SortLimit,
            ),
            ExactBoundaryJoinWorkKind::EstimatedBytes => (
                &mut self.work.estimated_bytes_examined,
                &mut self.work.estimated_bytes_attempted,
                self.limits.estimated_bytes,
                ExactBoundaryJoinStopReason::EstimatedByteLimit,
            ),
        };
        let Some(next) = examined.checked_add(amount) else {
            *attempted = usize::MAX;
            return Err(ExactBoundaryJoinStopReason::CounterOverflow);
        };
        *attempted = next;
        if next > limit {
            return Err(reason);
        }
        *examined = next;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ExactBoundaryJoinWorkKind {
    Postings,
    Intersections,
    Admissions,
    SortItems,
    EstimatedBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ExactBoundaryPostingKey {
    group: u8,
    side: BoundarySide,
    depth: usize,
    class_id: u32,
}

type ExactBoundaryPostingIndex = HashMap<ExactBoundaryPostingKey, Vec<usize>>;
type MatchingBoundaryPostings<'a> = (Option<&'a [usize]>, Option<&'a [usize]>);

fn checked_item_bytes<T>(items: usize) -> Result<usize, ExactBoundaryJoinStopReason> {
    items
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)
}

fn posting_key_estimated_bytes() -> Result<usize, ExactBoundaryJoinStopReason> {
    std::mem::size_of::<ExactBoundaryPostingKey>()
        .checked_add(std::mem::size_of::<Vec<usize>>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<usize>()))
        .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)
}

fn boundary_class(
    combined_index: usize,
    side: BoundarySide,
    depth: usize,
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
) -> Result<u32, ExactBoundaryJoinStopReason> {
    optional_boundary_class(
        combined_index,
        side,
        depth,
        offsets,
        prefix_classes,
        suffix_classes,
    )?
    .ok_or(ExactBoundaryJoinStopReason::InvalidInput)
}

fn optional_boundary_class(
    combined_index: usize,
    side: BoundarySide,
    depth: usize,
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
) -> Result<Option<u32>, ExactBoundaryJoinStopReason> {
    let start = usize::try_from(
        *offsets
            .get(combined_index)
            .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?,
    )
    .map_err(|_| ExactBoundaryJoinStopReason::InvalidInput)?;
    let end = usize::try_from(
        *offsets
            .get(
                combined_index
                    .checked_add(1)
                    .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?,
            )
            .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?,
    )
    .map_err(|_| ExactBoundaryJoinStopReason::InvalidInput)?;
    let available = end
        .checked_sub(start)
        .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?;
    if depth == 0 {
        return Err(ExactBoundaryJoinStopReason::InvalidInput);
    }
    if depth > available {
        return Ok(None);
    }
    let slot = start
        .checked_add(
            depth
                .checked_sub(1)
                .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?,
        )
        .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?;
    let classes = match side {
        BoundarySide::Prefix => prefix_classes,
        BoundarySide::Suffix => suffix_classes,
    };
    let class_id = *classes
        .get(slot)
        .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?;
    (class_id != 0)
        .then_some(Some(class_id))
        .ok_or(ExactBoundaryJoinStopReason::InvalidInput)
}

#[allow(clippy::too_many_arguments)]
fn build_exact_boundary_postings(
    fragments: &[ExactBoundaryJoinFragment],
    combined_start: usize,
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
    required_matches: impl Fn(usize) -> Option<usize>,
    budget: &mut ExactBoundaryJoinBudget,
) -> Result<ExactBoundaryPostingIndex, ExactBoundaryJoinStopReason> {
    let mut index = ExactBoundaryPostingIndex::new();
    for (local_index, fragment) in fragments.iter().enumerate() {
        let combined_index = combined_start
            .checked_add(local_index)
            .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?;
        let depth = required_matches(fragment.token_count)
            .filter(|depth| *depth > 0)
            .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?;
        for side in [BoundarySide::Prefix, BoundarySide::Suffix] {
            for current in 1..=depth {
                budget.charge(ExactBoundaryJoinWorkKind::Postings, 1)?;
                let key = ExactBoundaryPostingKey {
                    group: fragment.group,
                    side,
                    depth: current,
                    class_id: boundary_class(
                        combined_index,
                        side,
                        current,
                        offsets,
                        prefix_classes,
                        suffix_classes,
                    )?,
                };
                if !index.contains_key(&key) {
                    budget.charge(
                        ExactBoundaryJoinWorkKind::EstimatedBytes,
                        posting_key_estimated_bytes()?,
                    )?;
                    index
                        .try_reserve(1)
                        .map_err(|_| ExactBoundaryJoinStopReason::AllocationFailure)?;
                    index.insert(key, Vec::new());
                }
                let posting = index
                    .get_mut(&key)
                    .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?;
                budget.charge(
                    ExactBoundaryJoinWorkKind::EstimatedBytes,
                    checked_item_bytes::<usize>(1)?,
                )?;
                posting
                    .try_reserve(1)
                    .map_err(|_| ExactBoundaryJoinStopReason::AllocationFailure)?;
                posting.push(local_index);
            }
        }
    }
    Ok(index)
}

#[allow(clippy::too_many_arguments)]
fn matching_postings<'a>(
    query: &ExactBoundaryJoinFragment,
    query_combined_index: usize,
    split: usize,
    required: usize,
    postings: &'a ExactBoundaryPostingIndex,
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
) -> Result<MatchingBoundaryPostings<'a>, ExactBoundaryJoinStopReason> {
    let posting = |side, depth| -> Result<Option<&'a [usize]>, ExactBoundaryJoinStopReason> {
        if depth == 0 {
            return Ok(None);
        }
        let key = ExactBoundaryPostingKey {
            group: query.group,
            side,
            depth,
            class_id: boundary_class(
                query_combined_index,
                side,
                depth,
                offsets,
                prefix_classes,
                suffix_classes,
            )?,
        };
        Ok(postings.get(&key).map(Vec::as_slice))
    };
    Ok((
        posting(BoundarySide::Prefix, split)?,
        posting(BoundarySide::Suffix, required - split)?,
    ))
}

#[allow(clippy::too_many_arguments)]
fn filter_posting_by_boundary_class(
    posting: &[usize],
    candidate_combined_start: usize,
    side: BoundarySide,
    depth: usize,
    expected_class: u32,
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
    budget: &mut ExactBoundaryJoinBudget,
    output: &mut Vec<usize>,
) -> Result<(), ExactBoundaryJoinStopReason> {
    for &candidate_index in posting {
        budget.charge(ExactBoundaryJoinWorkKind::Intersections, 1)?;
        let combined_index = candidate_combined_start
            .checked_add(candidate_index)
            .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?;
        if optional_boundary_class(
            combined_index,
            side,
            depth,
            offsets,
            prefix_classes,
            suffix_classes,
        )? == Some(expected_class)
        {
            budget.charge(
                ExactBoundaryJoinWorkKind::EstimatedBytes,
                checked_item_bytes::<usize>(1)?,
            )?;
            output
                .try_reserve(1)
                .map_err(|_| ExactBoundaryJoinStopReason::AllocationFailure)?;
            output.push(candidate_index);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_query_pairs(
    query_local_index: usize,
    query: &ExactBoundaryJoinFragment,
    query_combined_index: usize,
    candidates: &[ExactBoundaryJoinFragment],
    candidate_combined_start: usize,
    postings: &ExactBoundaryPostingIndex,
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
    admitted_new_parents: &[Vec<usize>],
    query_is_old: bool,
    required_matches: impl Fn(usize) -> Option<usize>,
    budget: &mut ExactBoundaryJoinBudget,
    pairs: &mut Vec<(usize, usize)>,
) -> Result<(), ExactBoundaryJoinStopReason> {
    let required = required_matches(query.token_count)
        .filter(|required| *required > 0)
        .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?;
    let mut matches = Vec::new();
    for split in 0..=required {
        let (prefix, suffix) = matching_postings(
            query,
            query_combined_index,
            split,
            required,
            postings,
            offsets,
            prefix_classes,
            suffix_classes,
        )?;
        if (split > 0 && prefix.is_none()) || (required > split && suffix.is_none()) {
            continue;
        }
        matches.clear();
        match (prefix, suffix) {
            (Some(prefix), Some(suffix)) => {
                let (posting, verify_side, verify_depth) = if prefix.len() <= suffix.len() {
                    (prefix, BoundarySide::Suffix, required - split)
                } else {
                    (suffix, BoundarySide::Prefix, split)
                };
                let expected_class = boundary_class(
                    query_combined_index,
                    verify_side,
                    verify_depth,
                    offsets,
                    prefix_classes,
                    suffix_classes,
                )?;
                filter_posting_by_boundary_class(
                    posting,
                    candidate_combined_start,
                    verify_side,
                    verify_depth,
                    expected_class,
                    offsets,
                    prefix_classes,
                    suffix_classes,
                    budget,
                    &mut matches,
                )?;
            }
            (Some(posting), None) | (None, Some(posting)) => {
                budget.charge(ExactBoundaryJoinWorkKind::Intersections, posting.len())?;
                budget.charge(
                    ExactBoundaryJoinWorkKind::EstimatedBytes,
                    checked_item_bytes::<usize>(posting.len())?,
                )?;
                matches
                    .try_reserve(posting.len())
                    .map_err(|_| ExactBoundaryJoinStopReason::AllocationFailure)?;
                matches.extend_from_slice(posting);
            }
            (None, None) => return Err(ExactBoundaryJoinStopReason::InvalidInput),
        }
        for &candidate_index in &matches {
            let candidate = candidates
                .get(candidate_index)
                .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?;
            let length_eligible = if query_is_old {
                candidate.token_count >= query.token_count
            } else {
                candidate.token_count > query.token_count
            };
            if !length_eligible {
                continue;
            }
            budget.charge(ExactBoundaryJoinWorkKind::Admissions, 1)?;
            let (old_index, new_index, old_parent, new_parent) = if query_is_old {
                (
                    query_local_index,
                    candidate_index,
                    query.parent,
                    candidate.parent,
                )
            } else {
                (
                    candidate_index,
                    query_local_index,
                    candidate.parent,
                    query.parent,
                )
            };
            let admitted = admitted_new_parents
                .get(old_parent)
                .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?
                .binary_search(&new_parent)
                .is_ok();
            if admitted {
                budget.charge(
                    ExactBoundaryJoinWorkKind::EstimatedBytes,
                    checked_item_bytes::<(usize, usize)>(1)?,
                )?;
                pairs
                    .try_reserve(1)
                    .map_err(|_| ExactBoundaryJoinStopReason::AllocationFailure)?;
                pairs.push((old_index, new_index));
            }
        }
    }
    Ok(())
}

/// Enumerates every old/new pair whose exact prefix and suffix evidence reaches
/// the edge threshold. Equal-length pairs are queried from the old side; only
/// strictly shorter new fragments query the reverse index, so every pair has a
/// single directional owner before split duplicates are removed.
#[allow(clippy::too_many_arguments)]
pub(in crate::diff) fn exact_boundary_join(
    old: &[ExactBoundaryJoinFragment],
    new: &[ExactBoundaryJoinFragment],
    offsets: &[u32],
    prefix_classes: &[u32],
    suffix_classes: &[u32],
    admitted_new_parents: &[Vec<usize>],
    required_matches: impl Fn(usize) -> Option<usize> + Copy,
    budget: &mut ExactBoundaryJoinBudget,
) -> Result<Vec<(usize, usize)>, ExactBoundaryJoinStopReason> {
    let combined_count = old
        .len()
        .checked_add(new.len())
        .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?;
    let expected_offset_count = combined_count
        .checked_add(1)
        .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?;
    if offsets.len() != expected_offset_count
        || usize::try_from(
            *offsets
                .last()
                .ok_or(ExactBoundaryJoinStopReason::InvalidInput)?,
        )
        .ok()
        .is_none_or(|slots| prefix_classes.len() != slots || suffix_classes.len() != slots)
    {
        return Err(ExactBoundaryJoinStopReason::InvalidInput);
    }
    let old_postings = build_exact_boundary_postings(
        old,
        0,
        offsets,
        prefix_classes,
        suffix_classes,
        required_matches,
        budget,
    )?;
    let new_postings = build_exact_boundary_postings(
        new,
        old.len(),
        offsets,
        prefix_classes,
        suffix_classes,
        required_matches,
        budget,
    )?;
    let mut pairs = Vec::new();
    for (old_index, query) in old.iter().enumerate() {
        collect_query_pairs(
            old_index,
            query,
            old_index,
            new,
            old.len(),
            &new_postings,
            offsets,
            prefix_classes,
            suffix_classes,
            admitted_new_parents,
            true,
            required_matches,
            budget,
            &mut pairs,
        )?;
    }
    for (new_index, query) in new.iter().enumerate() {
        collect_query_pairs(
            new_index,
            query,
            old.len()
                .checked_add(new_index)
                .ok_or(ExactBoundaryJoinStopReason::CounterOverflow)?,
            old,
            0,
            &old_postings,
            offsets,
            prefix_classes,
            suffix_classes,
            admitted_new_parents,
            false,
            required_matches,
            budget,
            &mut pairs,
        )?;
    }
    budget.charge(ExactBoundaryJoinWorkKind::SortItems, pairs.len())?;
    pairs.sort_unstable();
    pairs.dedup();
    Ok(pairs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct CappedExactLength {
    pub(in crate::diff) tokens: usize,
    pub(in crate::diff) rank_comparisons: usize,
}

/// Exact prefix and suffix ranks for a collection of token sequences.
///
/// At each depth, equal nonzero ranks must mean that the complete boundary token
/// sequence through that depth is equal. This invariant makes the adaptive
/// search collision-free: equality is decided by exact rank construction,
/// never by a probabilistic hash.
pub(in crate::diff) struct ExactBoundaryRanks<'a> {
    offsets: &'a [u32],
    prefix: &'a [u32],
    suffix: &'a [u32],
}

impl<'a> ExactBoundaryRanks<'a> {
    pub(in crate::diff) fn new(
        offsets: &'a [u32],
        prefix: &'a [u32],
        suffix: &'a [u32],
    ) -> Option<Self> {
        let storage_len = *offsets.last()? as usize;
        (prefix.len() == storage_len && suffix.len() == storage_len).then_some(Self {
            offsets,
            prefix,
            suffix,
        })
    }

    #[cfg(test)]
    fn capped_common_boundary(
        &self,
        first: usize,
        second: usize,
        side: BoundarySide,
        cap: usize,
        known_equal: usize,
    ) -> Option<CappedExactLength> {
        match self.capped_common_boundary_with(first, second, side, cap, known_equal, || {
            Ok::<(), std::convert::Infallible>(())
        }) {
            Ok(result) => result,
            Err(never) => match never {},
        }
    }

    pub(in crate::diff) fn capped_common_boundary_with<E>(
        &self,
        first: usize,
        second: usize,
        side: BoundarySide,
        cap: usize,
        known_equal: usize,
        mut before_comparison: impl FnMut() -> Result<(), E>,
    ) -> Result<Option<CappedExactLength>, E> {
        let Some(first_start) = self.offsets.get(first).map(|offset| *offset as usize) else {
            return Ok(None);
        };
        let Some(first_end) = first
            .checked_add(1)
            .and_then(|index| self.offsets.get(index))
            .map(|offset| *offset as usize)
        else {
            return Ok(None);
        };
        let Some(second_start) = self.offsets.get(second).map(|offset| *offset as usize) else {
            return Ok(None);
        };
        let Some(second_end) = second
            .checked_add(1)
            .and_then(|index| self.offsets.get(index))
            .map(|offset| *offset as usize)
        else {
            return Ok(None);
        };
        let Some(first_available) = first_end.checked_sub(first_start) else {
            return Ok(None);
        };
        let Some(second_available) = second_end.checked_sub(second_start) else {
            return Ok(None);
        };
        let available = first_available.min(second_available);
        let cap = cap.min(available);
        let mut low = known_equal.min(cap);
        let ranks = match side {
            BoundarySide::Prefix => self.prefix,
            BoundarySide::Suffix => self.suffix,
        };
        let mut rank_comparisons = 0usize;

        let mut equal_at = |depth: usize| -> Result<Option<bool>, E> {
            before_comparison()?;
            let Some(index) = depth.checked_sub(1) else {
                return Ok(None);
            };
            let Some(first_slot) = first_start.checked_add(index) else {
                return Ok(None);
            };
            let Some(second_slot) = second_start.checked_add(index) else {
                return Ok(None);
            };
            let Some(&first_rank) = ranks.get(first_slot) else {
                return Ok(None);
            };
            let Some(&second_rank) = ranks.get(second_slot) else {
                return Ok(None);
            };
            Ok(Some(first_rank != 0 && first_rank == second_rank))
        };

        let origin = low;
        let mut step = 1usize;
        let mut high = cap;
        while low < cap {
            let probe = origin.saturating_add(step).min(cap);
            let Some(equal) = equal_at(probe)? else {
                return Ok(None);
            };
            let Some(next_comparisons) = rank_comparisons.checked_add(1) else {
                return Ok(None);
            };
            rank_comparisons = next_comparisons;
            if equal {
                low = probe;
                step = step.saturating_mul(2);
            } else {
                let Some(next_high) = probe.checked_sub(1) else {
                    return Ok(None);
                };
                high = next_high;
                break;
            }
        }

        while low < high {
            let Some(distance) = high.checked_sub(low) else {
                return Ok(None);
            };
            let Some(middle) = low
                .checked_add(distance / 2)
                .and_then(|middle| middle.checked_add(1))
            else {
                return Ok(None);
            };
            let Some(equal) = equal_at(middle)? else {
                return Ok(None);
            };
            let Some(next_comparisons) = rank_comparisons.checked_add(1) else {
                return Ok(None);
            };
            rank_comparisons = next_comparisons;
            if equal {
                low = middle;
            } else {
                high = middle - 1;
            }
        }

        Ok(Some(CappedExactLength {
            tokens: low,
            rank_comparisons,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        BoundarySide, ExactBoundaryJoinBudget, ExactBoundaryJoinFragment, ExactBoundaryJoinLimits,
        ExactBoundaryJoinStopReason, ExactBoundaryRanks, boundary_class, checked_item_bytes,
        exact_boundary_join, filter_posting_by_boundary_class, posting_key_estimated_bytes,
    };

    fn ranks(sequences: &[Vec<u16>]) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut offsets = vec![0u32];
        for sequence in sequences {
            offsets.push(
                offsets
                    .last()
                    .expect("offsets always contains the zero origin")
                    + sequence.len() as u32,
            );
        }
        let mut prefix = vec![
            0;
            *offsets
                .last()
                .expect("offsets always contains the zero origin")
                as usize
        ];
        let mut suffix = prefix.clone();
        for (side, output) in [
            (BoundarySide::Prefix, &mut prefix),
            (BoundarySide::Suffix, &mut suffix),
        ] {
            let max_depth = sequences.iter().map(Vec::len).max().unwrap_or(0);
            let mut parents = vec![0u32; sequences.len()];
            for depth in 0..max_depth {
                let mut keys = sequences
                    .iter()
                    .enumerate()
                    .filter(|(_, sequence)| sequence.len() > depth)
                    .map(|(index, sequence)| {
                        let token = match side {
                            BoundarySide::Prefix => sequence[depth],
                            BoundarySide::Suffix => sequence[sequence.len() - depth - 1],
                        };
                        (index, (parents[index], token))
                    })
                    .collect::<Vec<_>>();
                let distinct = keys.iter().map(|(_, key)| *key).collect::<BTreeSet<_>>();
                let ids = distinct
                    .iter()
                    .enumerate()
                    .map(|(index, key)| (*key, index as u32 + 1))
                    .collect::<BTreeMap<_, _>>();
                for (index, key) in keys.drain(..) {
                    let rank = ids[&key];
                    parents[index] = rank;
                    output[offsets[index] as usize + depth] = rank;
                }
            }
        }
        (offsets, prefix, suffix)
    }

    #[test]
    fn finds_capped_prefix_and_suffix_without_overlap() {
        let sequences = vec![b"abcd1234wxyz".to_vec(), b"abcd5678wxyz".to_vec()]
            .into_iter()
            .map(|bytes| bytes.into_iter().map(u16::from).collect())
            .collect::<Vec<_>>();
        let (offsets, prefix, suffix) = ranks(&sequences);
        let index = ExactBoundaryRanks::new(&offsets, &prefix, &suffix)
            .expect("test ranks have consistent storage");
        assert_eq!(
            index
                .capped_common_boundary(0, 1, BoundarySide::Prefix, 12, 0)
                .expect("prefix query is in range")
                .tokens,
            4
        );
        assert_eq!(
            index
                .capped_common_boundary(0, 1, BoundarySide::Suffix, 8, 0)
                .expect("suffix query is in range")
                .tokens,
            4
        );
        assert_eq!(
            index
                .capped_common_boundary(0, 1, BoundarySide::Suffix, 3, 0)
                .expect("capped suffix query is in range")
                .tokens,
            3
        );
        let certified = index
            .capped_common_boundary(0, 1, BoundarySide::Prefix, 12, 3)
            .expect("certified prefix query is in range");
        assert_eq!(certified.tokens, 4);
        assert_eq!(certified.rank_comparisons, 2);
    }

    #[test]
    fn handles_unequal_and_repeated_sequences() {
        let sequences = vec![vec![7, 7, 7, 8], vec![7, 7], vec![7, 7, 7, 9]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let index = ExactBoundaryRanks::new(&offsets, &prefix, &suffix)
            .expect("test ranks have consistent storage");
        assert_eq!(
            index
                .capped_common_boundary(0, 1, BoundarySide::Prefix, usize::MAX, 0)
                .expect("unequal prefix query is in range")
                .tokens,
            2
        );
        assert_eq!(
            index
                .capped_common_boundary(0, 2, BoundarySide::Prefix, usize::MAX, 0)
                .expect("repeated prefix query is in range")
                .tokens,
            3
        );
        assert_eq!(
            index
                .capped_common_boundary(0, 2, BoundarySide::Suffix, usize::MAX, 0)
                .expect("repeated suffix query is in range")
                .tokens,
            0
        );
    }

    #[test]
    fn rejects_inconsistent_storage() {
        assert!(ExactBoundaryRanks::new(&[0, 1], &[], &[]).is_none());
        assert!(ExactBoundaryRanks::new(&[], &[], &[]).is_none());
    }

    #[test]
    fn comparison_callback_stops_before_rank_access() {
        let index = ExactBoundaryRanks::new(&[0, 1, 2], &[0, 0], &[0, 0])
            .expect("storage lengths are consistent");
        let mut attempts = 0usize;

        let result = index.capped_common_boundary_with(0, 1, BoundarySide::Prefix, 1, 0, || {
            attempts += 1;
            Err("comparison limit")
        });

        assert_eq!(result, Err("comparison limit"));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn comparison_callback_count_matches_completed_query_work() {
        let sequences = vec![vec![1, 2, 3, 4], vec![1, 2, 9, 4]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let index = ExactBoundaryRanks::new(&offsets, &prefix, &suffix)
            .expect("test ranks have consistent storage");
        let mut examined = 0usize;

        let result = index
            .capped_common_boundary_with(0, 1, BoundarySide::Prefix, 4, 0, || {
                examined += 1;
                Ok::<(), ()>(())
            })
            .expect("budget permits every comparison")
            .expect("query is valid");

        assert_eq!(examined, result.rank_comparisons);
    }

    fn join_limits() -> ExactBoundaryJoinLimits {
        ExactBoundaryJoinLimits {
            postings: 1_000_000,
            intersections: 1_000_000,
            admissions: 1_000_000,
            sort_items: 1_000_000,
            estimated_bytes: 1_000_000,
        }
    }

    fn required_matches(length: usize) -> Option<usize> {
        length.checked_mul(3)?.checked_add(9)?.checked_div(10)
    }

    fn exact_edge_retained(old: &[u16], new: &[u16]) -> bool {
        let shorter = old.len().min(new.len());
        let prefix = old
            .iter()
            .zip(new)
            .take_while(|(old, new)| old == new)
            .count();
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take_while(|(old, new)| old == new)
            .count()
            .min(shorter.saturating_sub(prefix));
        prefix + suffix >= required_matches(shorter).expect("small threshold fits")
    }

    #[test]
    fn exact_boundary_join_matches_small_full_scan_exhaustively() {
        let sequence = |length: usize, bits: usize| {
            (0..length)
                .map(|offset| u16::from(bits & (1 << offset) != 0))
                .collect::<Vec<_>>()
        };
        for old_len in 1..=5 {
            for new_len in 1..=5 {
                for old_bits in 0..(1 << old_len) {
                    for new_bits in 0..(1 << new_len) {
                        let old_tokens = sequence(old_len, old_bits);
                        let new_tokens = sequence(new_len, new_bits);
                        let (offsets, prefix, suffix) =
                            ranks(&[old_tokens.clone(), new_tokens.clone()]);
                        let old = [ExactBoundaryJoinFragment {
                            group: 0,
                            parent: 0,
                            token_count: old_len,
                        }];
                        let new = [ExactBoundaryJoinFragment {
                            group: 0,
                            parent: 0,
                            token_count: new_len,
                        }];
                        let mut budget = ExactBoundaryJoinBudget::new(join_limits());
                        let actual = exact_boundary_join(
                            &old,
                            &new,
                            &offsets,
                            &prefix,
                            &suffix,
                            &[vec![0]],
                            required_matches,
                            &mut budget,
                        )
                        .expect("small join completes");
                        let expected_postings = 2
                            * (required_matches(old_len).expect("small threshold fits")
                                + required_matches(new_len).expect("small threshold fits"));
                        assert_eq!(budget.work().postings_examined, expected_postings);
                        assert_eq!(budget.work().postings_attempted, expected_postings);
                        let expected = if exact_edge_retained(&old_tokens, &new_tokens) {
                            vec![(0, 0)]
                        } else {
                            Vec::new()
                        };
                        assert_eq!(actual, expected, "old={old_tokens:?} new={new_tokens:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn boundary_class_filter_scans_only_the_supplied_smaller_posting() {
        let mut sequences = vec![vec![1, 2, 3, 4]];
        sequences.extend((0..128).map(|_| vec![9, 9, 9, 9]));
        sequences[74] = vec![8, 8, 8, 4];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let expected_class = boundary_class(0, BoundarySide::Suffix, 1, &offsets, &prefix, &suffix)
            .expect("query suffix class exists");
        let mut budget = ExactBoundaryJoinBudget::new(join_limits());
        let mut matches = Vec::new();

        filter_posting_by_boundary_class(
            &[73],
            1,
            BoundarySide::Suffix,
            1,
            expected_class,
            &offsets,
            &prefix,
            &suffix,
            &mut budget,
            &mut matches,
        )
        .expect("class filter completes");

        assert_eq!(matches, vec![73]);
        assert_eq!(budget.work().intersections_examined, 1);
        assert_eq!(budget.work().intersections_attempted, 1);
        assert_eq!(budget.work().postings_examined, 0);
        assert_eq!(budget.work().postings_attempted, 0);
    }

    #[test]
    fn exact_join_skips_a_short_candidate_before_reading_the_next_fragment_slot() {
        let sequences = vec![vec![1, 2, 3, 4, 5, 6, 7], vec![9, 9, 6, 7], vec![1]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let old = [ExactBoundaryJoinFragment {
            group: 0,
            parent: 0,
            token_count: 7,
        }];
        let new = [
            ExactBoundaryJoinFragment {
                group: 0,
                parent: 0,
                token_count: 4,
            },
            ExactBoundaryJoinFragment {
                group: 0,
                parent: 1,
                token_count: 1,
            },
        ];
        let mut budget = ExactBoundaryJoinBudget::new(join_limits());

        let pairs = exact_boundary_join(
            &old,
            &new,
            &offsets,
            &prefix,
            &suffix,
            &[vec![0, 1]],
            required_matches,
            &mut budget,
        )
        .expect("an unavailable candidate depth is a nonmatch");

        assert_eq!(pairs, vec![(0, 0), (0, 1)]);
        assert!(budget.work().intersections_examined > 0);
    }

    #[test]
    fn exact_boundary_join_preserves_group_admission_and_order() {
        let sequences = vec![
            vec![1, 1, 1, 1],
            vec![2, 2, 2, 2],
            vec![1, 1, 1, 1],
            vec![2, 2, 2, 2],
            vec![1, 1, 1],
            vec![1, 1, 1, 1],
        ];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let old = [
            ExactBoundaryJoinFragment {
                group: 0,
                parent: 0,
                token_count: 4,
            },
            ExactBoundaryJoinFragment {
                group: 1,
                parent: 1,
                token_count: 4,
            },
        ];
        let new = [
            ExactBoundaryJoinFragment {
                group: 0,
                parent: 0,
                token_count: 4,
            },
            ExactBoundaryJoinFragment {
                group: 1,
                parent: 1,
                token_count: 4,
            },
            ExactBoundaryJoinFragment {
                group: 0,
                parent: 2,
                token_count: 3,
            },
            ExactBoundaryJoinFragment {
                group: 1,
                parent: 3,
                token_count: 4,
            },
        ];
        let mut budget = ExactBoundaryJoinBudget::new(join_limits());
        let pairs = exact_boundary_join(
            &old,
            &new,
            &offsets,
            &prefix,
            &suffix,
            &[vec![0, 2, 3], vec![1]],
            required_matches,
            &mut budget,
        )
        .expect("grouped join completes");
        assert_eq!(pairs, vec![(0, 0), (0, 2), (1, 1)]);
        assert!(budget.work().sort_items_examined > pairs.len());

        let mut restricted = ExactBoundaryJoinBudget::new(join_limits());
        let pairs = exact_boundary_join(
            &old,
            &new,
            &offsets,
            &prefix,
            &suffix,
            &[vec![2], vec![]],
            required_matches,
            &mut restricted,
        )
        .expect("restricted join completes");
        assert_eq!(pairs, vec![(0, 2)]);
    }

    #[test]
    fn exact_boundary_join_stops_each_work_stage_atomically() {
        let sequences = vec![vec![1, 1, 1, 1], vec![1, 1, 1, 1]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let old = [ExactBoundaryJoinFragment {
            group: 0,
            parent: 0,
            token_count: 4,
        }];
        let new = [ExactBoundaryJoinFragment {
            group: 0,
            parent: 0,
            token_count: 4,
        }];
        for (expected, mutate) in [
            (
                ExactBoundaryJoinStopReason::PostingLimit,
                (|limits: &mut ExactBoundaryJoinLimits| limits.postings = 0)
                    as fn(&mut ExactBoundaryJoinLimits),
            ),
            (
                ExactBoundaryJoinStopReason::IntersectionLimit,
                |limits: &mut ExactBoundaryJoinLimits| limits.intersections = 0,
            ),
            (
                ExactBoundaryJoinStopReason::AdmissionLimit,
                |limits: &mut ExactBoundaryJoinLimits| limits.admissions = 0,
            ),
            (
                ExactBoundaryJoinStopReason::SortLimit,
                |limits: &mut ExactBoundaryJoinLimits| limits.sort_items = 0,
            ),
            (
                ExactBoundaryJoinStopReason::EstimatedByteLimit,
                |limits: &mut ExactBoundaryJoinLimits| limits.estimated_bytes = 0,
            ),
        ] {
            let mut limits = join_limits();
            mutate(&mut limits);
            let mut budget = ExactBoundaryJoinBudget::new(limits);
            let result = exact_boundary_join(
                &old,
                &new,
                &offsets,
                &prefix,
                &suffix,
                &[vec![0]],
                required_matches,
                &mut budget,
            );
            assert_eq!(result, Err(expected));
            let work = budget.work();
            assert!(
                work.postings_examined < work.postings_attempted
                    || work.intersections_examined < work.intersections_attempted
                    || work.admissions_examined < work.admissions_attempted
                    || work.sort_items_examined < work.sort_items_attempted
                    || work.estimated_bytes_examined < work.estimated_bytes_attempted
            );
        }
    }

    #[test]
    fn exact_join_estimated_byte_counter_overflow_is_atomic() {
        assert_eq!(
            checked_item_bytes::<usize>(usize::MAX),
            Err(ExactBoundaryJoinStopReason::CounterOverflow)
        );
        let sequences = vec![vec![1], vec![1]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let fragment = ExactBoundaryJoinFragment {
            group: 0,
            parent: 0,
            token_count: 1,
        };
        let mut budget = ExactBoundaryJoinBudget::new(join_limits());
        budget.work.estimated_bytes_examined = usize::MAX;

        let result = exact_boundary_join(
            &[fragment],
            &[fragment],
            &offsets,
            &prefix,
            &suffix,
            &[vec![0]],
            required_matches,
            &mut budget,
        );

        assert_eq!(result, Err(ExactBoundaryJoinStopReason::CounterOverflow));
        assert_eq!(budget.work().estimated_bytes_examined, usize::MAX);
        assert_eq!(budget.work().estimated_bytes_attempted, usize::MAX);
    }

    #[test]
    fn exact_join_stops_before_each_memory_allocation_stage() {
        let sequences = vec![vec![1], vec![1]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let fragment = ExactBoundaryJoinFragment {
            group: 0,
            parent: 0,
            token_count: 1,
        };
        let key_bytes = posting_key_estimated_bytes().expect("key size fits");
        let item_bytes = checked_item_bytes::<usize>(1).expect("item size fits");
        let build_bytes = 4 * (key_bytes + item_bytes);
        for expected_examined in [0, key_bytes, build_bytes, build_bytes + item_bytes] {
            let mut limits = join_limits();
            limits.estimated_bytes = expected_examined;
            let mut budget = ExactBoundaryJoinBudget::new(limits);

            let result = exact_boundary_join(
                &[fragment],
                &[fragment],
                &offsets,
                &prefix,
                &suffix,
                &[vec![0]],
                required_matches,
                &mut budget,
            );

            assert_eq!(result, Err(ExactBoundaryJoinStopReason::EstimatedByteLimit));
            assert_eq!(budget.work().estimated_bytes_examined, expected_examined);
            assert!(
                budget.work().estimated_bytes_attempted > budget.work().estimated_bytes_examined
            );
        }
    }

    #[test]
    fn endpoint_candidate_traversal_uses_intersection_budget_atomically() {
        let sequences = vec![vec![1, 1, 1, 1], vec![1, 1, 1, 1]];
        let (offsets, prefix, suffix) = ranks(&sequences);
        let old = [ExactBoundaryJoinFragment {
            group: 0,
            parent: 0,
            token_count: 4,
        }];
        let new = old;
        let mut limits = join_limits();
        limits.intersections = 0;
        let mut budget = ExactBoundaryJoinBudget::new(limits);

        let result = exact_boundary_join(
            &old,
            &new,
            &offsets,
            &prefix,
            &suffix,
            &[vec![0]],
            required_matches,
            &mut budget,
        );

        assert_eq!(result, Err(ExactBoundaryJoinStopReason::IntersectionLimit));
        let work = budget.work();
        assert_eq!(work.postings_examined, 8);
        assert_eq!(work.postings_attempted, 8);
        assert_eq!(work.intersections_examined, 0);
        assert_eq!(work.intersections_attempted, 1);
        assert_eq!(work.admissions_examined, 0);
        assert_eq!(work.sort_items_examined, 0);
    }
}
