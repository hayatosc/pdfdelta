//! Bounded exact uniqueness checks for token alignments.

mod single_edit;
pub(super) use single_edit::check as check_unique_single_edit_script;
mod banded;
mod negative_pair;

use crate::{Error, Result};

const MAX_EXACT_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const DP_CELLS_RESOURCE: &str = "exact uniqueness DP cells";
const DP_MEMORY_RESOURCE: &str = "exact uniqueness DP memory";

/// Classification of the equal-token pairs that can occur in a shortest
/// insertion/deletion alignment.
///
/// `Unique` means every shortest alignment contains the same ordered sequence
/// of equal token index pairs. Insertions and deletions may still be ordered
/// differently inside one changed hunk; that ordering does not affect this
/// classification. Equality is exactly the `Eq` implementation of the input
/// token type, so this result makes no semantic-identity promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExactUniqueness {
    /// Every shortest alignment has the same equal-token pair sequence.
    Unique,
    /// Shortest alignments contain more than one equal-token pair sequence.
    Ambiguous,
    /// The supplied work budget was insufficient to complete the check.
    BudgetExceeded,
}

/// Checks whether all shortest insertion/deletion alignments have the same
/// equal-token pair sequence.
///
/// The forward LCS table is retained so that a reverse rolling computation can
/// enumerate the union of all matching diagonal edges on an LCS path. The
/// caller's budget is charged for every forward and reverse interior DP cell,
/// and for every comparison made by the equal-input fast path. `BudgetExceeded`
/// therefore carries no uniqueness claim and lets callers share one bound
/// across multiple checks.
/// Two eligible diagonals in one old-token row already certify ambiguity:
/// both occur on optimal paths but cannot occur together in one pair sequence.
/// This predicate is part of the charged reverse-cell uniqueness recurrence,
/// with no extra traversal, allocation, retained certificate or work refund.
///
/// # Errors
///
/// Returns [`Error::LimitExceeded`] when dimensions, cell counts, or the
/// retained DP memory would exceed representable or configured limits. Returns
/// [`Error::Unresolved`] when a bounded DP allocation cannot be reserved.
#[cfg(test)]
pub(super) fn check<T: Eq>(
    old: &[T],
    new: &[T],
    remaining_cells: &mut usize,
) -> Result<ExactUniqueness> {
    check_recording(old, new, remaining_cells, &mut 0)
}

/// Records the exact size of a refused request; successful checks leave zero.
/// DP preflight refusal does not consume the unspent budget.
#[cfg(test)]
pub(super) fn check_recording<T: Eq>(
    old: &[T],
    new: &[T],
    remaining_cells: &mut usize,
    refused_request: &mut usize,
) -> Result<ExactUniqueness> {
    *refused_request = 0;
    if old.is_empty() || new.is_empty() {
        return Ok(ExactUniqueness::Unique);
    }

    if old.len() == new.len() {
        let mut identical = true;
        for (old_token, new_token) in old.iter().zip(new) {
            if !charge_cell(remaining_cells) {
                *refused_request = 1;
                return Ok(ExactUniqueness::BudgetExceeded);
            }
            if old_token != new_token {
                identical = false;
                break;
            }
        }

        if identical {
            return Ok(ExactUniqueness::Unique);
        }
    }

    let forward_cells = old
        .len()
        .checked_mul(new.len())
        .ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;
    let dp_cells = forward_cells.checked_mul(2).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    if dp_cells > *remaining_cells {
        *refused_request = dp_cells;
        return Ok(ExactUniqueness::BudgetExceeded);
    }

    let columns = new.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let rows = old.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let prefix_cells = rows.checked_mul(columns).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let cell_bytes = std::mem::size_of::<usize>();
    let prefix_bytes = prefix_cells
        .checked_mul(cell_bytes)
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    let row_bytes = columns
        .checked_mul(cell_bytes)
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    let rolling_bytes = row_bytes.checked_mul(2).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_EXACT_MEMORY_BYTES,
    })?;
    let total_bytes = prefix_bytes
        .checked_add(rolling_bytes)
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    if total_bytes > MAX_EXACT_MEMORY_BYTES {
        return Err(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        });
    }

    let mut prefix = Vec::<usize>::new();
    prefix.try_reserve_exact(prefix_cells).map_err(|_| {
        Error::Unresolved("exact uniqueness prefix table allocation failed".to_owned())
    })?;
    prefix.resize(prefix_cells, 0);

    for (old_index, old_token) in old.iter().enumerate() {
        let previous_row = old_index * columns;
        let current_row = (old_index + 1) * columns;
        for new_index in 0..new.len() {
            if !charge_cell(remaining_cells) {
                *refused_request = 1;
                return Ok(ExactUniqueness::BudgetExceeded);
            }
            prefix[current_row + new_index + 1] = if *old_token == new[new_index] {
                prefix[previous_row + new_index]
                    .checked_add(1)
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?
            } else {
                prefix[previous_row + new_index + 1].max(prefix[current_row + new_index])
            };
        }
    }

    let mut suffix_next = allocate_row(columns, "exact uniqueness suffix row")?;
    let mut suffix_current = allocate_row(columns, "exact uniqueness suffix row")?;
    let lcs_length = prefix[prefix_cells - 1];
    let mut matching_edges = 0usize;

    for old_index in (0..old.len()).rev() {
        suffix_current.fill(0);
        let prefix_row = old_index * columns;
        let mut row_has_match = false;
        for new_index in (0..new.len()).rev() {
            if !charge_cell(remaining_cells) {
                *refused_request = 1;
                return Ok(ExactUniqueness::BudgetExceeded);
            }
            let suffix_after_match = suffix_next[new_index + 1];
            suffix_current[new_index] = if old[old_index] == new[new_index] {
                let through_match = prefix[prefix_row + new_index]
                    .checked_add(1)
                    .and_then(|length| length.checked_add(suffix_after_match))
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?;
                if through_match == lcs_length {
                    matching_edges = matching_edges.checked_add(1).ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?;
                    if row_has_match || matching_edges > lcs_length {
                        return Ok(ExactUniqueness::Ambiguous);
                    }
                    row_has_match = true;
                }
                suffix_after_match
                    .checked_add(1)
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?
            } else {
                suffix_current[new_index + 1].max(suffix_next[new_index])
            };
        }
        std::mem::swap(&mut suffix_current, &mut suffix_next);
    }

    debug_assert_eq!(suffix_next[0], lcs_length);
    Ok(if matching_edges == lcs_length {
        ExactUniqueness::Unique
    } else {
        ExactUniqueness::Ambiguous
    })
}

/// Exact classification plus an optional, private same-input suffix continuation.
pub(super) struct RetainedCheck<'a, T> {
    pub(super) outcome: ExactUniqueness,
    pub(super) suffix: Option<RetainedSuffix<'a, T>>,
}

impl<T> RetainedCheck<'_, T> {
    fn plain(outcome: ExactUniqueness) -> Self {
        Self {
            outcome,
            suffix: None,
        }
    }
}

/// A private suffix with exact input bindings; it carries no source verdict.
///
/// A complete form is returned only after exact uniqueness is proved and its
/// retention metadata is paid. The incomplete form belongs to an Ambiguous
/// result and cannot reconstruct a unique script.
/// Its matrix is the original forward buffer. The lower/right completed
/// contour contains suffix cells; the remaining interior still contains
/// unusable prefix cells. The exact check has already returned Ambiguous and
/// released every need for those prefix values. The triggering cell was not
/// completed before that return and must be charged again when resumed.
pub(super) struct RetainedSuffix<'a, T> {
    old: &'a [T],
    new: &'a [T],
    table: Vec<usize>,
    next_old: usize,
    next_new: usize,
    completed: usize,
    row_ambiguity: Option<RowAmbiguity>,
}

#[derive(Clone, Copy)]
struct RowAmbiguity {
    first_new: usize,
    first_prefix_rank: usize,
    lcs_length: usize,
}

pub(super) enum ReconstructedScript {
    Unavailable,
    DistanceExceeded,
    Ready(Vec<crate::diff::AtomicEdit>),
}

impl<T: Eq> RetainedSuffix<'_, T> {
    /// Identity of the immutable pair that paid for this retained matrix.
    /// This is neither a semantic verdict nor a transferable content match.
    pub(super) fn is_bound_to(&self, old: &[T], new: &[T]) -> bool {
        std::ptr::eq(old, self.old) && std::ptr::eq(new, self.new)
    }

    /// A paid complete LCS rank establishes distance one; this hint supplies
    /// no semantic or physical verdict and is valid only for these slices.
    pub(super) fn has_one_edit_rank(&self, old: &[T], new: &[T]) -> bool {
        old.len().abs_diff(new.len()) == 1
            && std::ptr::eq(old, self.old)
            && std::ptr::eq(new, self.new)
            && self
                .row_ambiguity
                .is_some_and(|w| w.lcs_length == old.len().min(new.len()))
    }

    /// Prepayment for the continuation lookup and both immutable slice bindings.
    const BINDING_WORK: usize = 3;

    pub(super) fn pending_work(&self) -> Result<usize> {
        self.old
            .len()
            .checked_mul(self.new.len())
            .and_then(|cells| cells.checked_sub(self.completed))
            .and_then(|cells| cells.checked_add(Self::BINDING_WORK))
            .ok_or(Error::LimitExceeded {
                resource: DP_CELLS_RESOURCE,
                limit: usize::MAX,
            })
    }

    /// Uses only a complete unique table with the original immutable inputs.
    /// All traversal, output capacity and table-plus-output live bytes are
    /// admitted before reconstruction; no physical source verdict is reused.
    pub(super) fn reconstruct_unique(
        self,
        old: &[T],
        new: &[T],
        max_distance: usize,
        remaining: &mut usize,
    ) -> Result<ReconstructedScript> {
        use crate::diff::{AtomicEdit, myers};
        // Prepay immutable binding, original Myers admission, live-capacity
        // arithmetic, fallback metadata, and table release on every exit.
        if *remaining < 128 {
            return Ok(ReconstructedScript::Unavailable);
        }
        if !super::charge(remaining, 128) {
            return Ok(ReconstructedScript::Unavailable);
        }
        if !std::ptr::eq(old, self.old)
            || !std::ptr::eq(new, self.new)
            || self.completed
                != old
                    .len()
                    .checked_mul(new.len())
                    .ok_or(Error::Unresolved("unique table dimensions".to_owned()))?
        {
            return Ok(ReconstructedScript::Unavailable);
        }
        let (bound, _) = myers::reconstruction_admission(old.len(), new.len(), max_distance)?;
        let length = old
            .len()
            .checked_add(new.len())
            .ok_or(Error::Unresolved("unique script dimensions".to_owned()))?;
        let distance = length
            .checked_sub(
                self.table[0]
                    .checked_mul(2)
                    .ok_or(Error::Unresolved("unique script rank".to_owned()))?,
            )
            .ok_or(Error::Unresolved("unique script distance".to_owned()))?;
        if distance > bound {
            return Ok(ReconstructedScript::DistanceExceeded);
        }
        let bytes = self
            .table
            .capacity()
            .checked_mul(std::mem::size_of::<usize>())
            .and_then(|x| x.checked_add(distance.checked_mul(std::mem::size_of::<AtomicEdit>())?))
            .and_then(|x| {
                x.checked_add(
                    std::mem::size_of::<Self>()
                        + std::mem::size_of::<Vec<AtomicEdit>>()
                        + 16 * std::mem::size_of::<usize>(),
                )
            });
        if bytes.is_none_or(|x| x > MAX_EXACT_MEMORY_BYTES) {
            return Ok(ReconstructedScript::Unavailable);
        }
        let work = length
            .checked_mul(8)
            .and_then(|x| x.checked_add(distance.checked_mul(4)?));
        let Some(work) = work else {
            return Ok(ReconstructedScript::Unavailable);
        };
        if work > *remaining {
            return Ok(ReconstructedScript::Unavailable);
        }
        if !super::charge(remaining, work) {
            return Ok(ReconstructedScript::Unavailable);
        }
        let mut edits = Vec::new();
        if edits.try_reserve_exact(distance).is_err() || edits.capacity() != distance {
            return Ok(ReconstructedScript::Unavailable);
        }
        let columns = new.len() + 1;
        let (mut i, mut j, mut start_i, mut start_j) = (0, 0, 0, 0);
        while i < old.len() && j < new.len() {
            if old[i] == new[j] {
                if start_i < i {
                    edits.push(AtomicEdit {
                        old: start_i..i,
                        new: start_j..start_j,
                    });
                }
                if start_j < j {
                    edits.push(AtomicEdit {
                        old: i..i,
                        new: start_j..j,
                    });
                }
                i += 1;
                j += 1;
                start_i = i;
                start_j = j;
            } else if self.table[i * columns + j + 1] >= self.table[(i + 1) * columns + j] {
                j += 1;
            } else {
                i += 1;
            }
        }
        if start_i < old.len() {
            edits.push(AtomicEdit {
                old: start_i..old.len(),
                new: start_j..start_j,
            });
        }
        if start_j < new.len() {
            edits.push(AtomicEdit {
                old: old.len()..old.len(),
                new: start_j..new.len(),
            });
        }
        Ok(ReconstructedScript::Ready(edits))
    }

    /// Completes every unpaid cell before returning a usable suffix table.
    ///
    /// Only the exact same immutable slices may consume the continuation.
    /// Dimensions alone, equal token values or reversed sides cannot bind it.
    /// The caller must first apply the ordinary semantic memory preflight.
    ///
    /// # Errors
    /// Returns [`Error::Unresolved`] for a mismatched input binding and
    /// [`Error::LimitExceeded`] for
    /// unrepresentable cell arithmetic. A refused work preflight returns None
    /// with the ordinary exhausted semantic budget behavior.
    pub(super) fn complete_for(
        mut self,
        old: &[T],
        new: &[T],
        remaining: &mut usize,
    ) -> Result<Option<Vec<usize>>> {
        let pending = self.pending_work()?;
        if pending > *remaining {
            *remaining = 0;
            return Ok(None);
        }
        if !super::charge(remaining, Self::BINDING_WORK) {
            return Ok(None);
        }
        if !std::ptr::eq(old, self.old) || !std::ptr::eq(new, self.new) {
            return Err(Error::Unresolved(
                "suffix continuation input identity mismatch".to_owned(),
            ));
        }
        if self.completed
            == old
                .len()
                .checked_mul(new.len())
                .ok_or(Error::LimitExceeded {
                    resource: DP_CELLS_RESOURCE,
                    limit: usize::MAX,
                })?
        {
            return Ok(Some(self.table));
        }
        let columns = new.len() + 1;
        for old_index in (0..=self.next_old).rev() {
            let last_new = if old_index == self.next_old {
                self.next_new
            } else {
                new.len() - 1
            };
            for new_index in (0..=last_new).rev() {
                if !charge_cell(remaining) {
                    return Ok(None);
                }
                let current = old_index * columns + new_index;
                self.table[current] = if old[old_index] == new[new_index] {
                    self.table[(old_index + 1) * columns + new_index + 1]
                        .checked_add(1)
                        .ok_or(Error::LimitExceeded {
                            resource: DP_CELLS_RESOURCE,
                            limit: usize::MAX,
                        })?
                } else {
                    self.table[(old_index + 1) * columns + new_index].max(self.table[current + 1])
                };
            }
        }
        Ok(Some(self.table))
    }
}

/// Uses the original forward allocation for reverse cells after their prefix
/// values have been consumed. Classification, early exits, equal-input charges,
/// 2NM work preflight and the original forward-plus-two-rows memory limit stay
/// unchanged. No second full matrix is allocated or copied. Border clearing
/// replaces the larger rolling-row initialization, within the same charged
/// reverse-cell computation. A retained Ambiguous result is not a proof and
/// may be dropped without additional search work.
///
/// # Errors
/// Returns the original dimensional and configured memory refusals, and prefix
/// allocation failure. The two rolling-row allocations are no longer needed.
#[cfg(test)]
pub(super) fn check_retaining<'a, T: Eq>(
    old: &'a [T],
    new: &'a [T],
    remaining_cells: &mut usize,
) -> Result<RetainedCheck<'a, T>> {
    check_retaining_internal(old, new, remaining_cells, false, false)
}

/// Also preserves a paid complete unique table for immediate same-input use.
pub(super) fn check_retaining_for_reconstruction<'a, T: Eq>(
    old: &'a [T],
    new: &'a [T],
    remaining_cells: &mut usize,
) -> Result<RetainedCheck<'a, T>> {
    check_retaining_internal(old, new, remaining_cells, true, false)
}

/// Immutable same-call inputs and the first mismatch found by the paid
/// identical-input scout. The slices prevent rebinding its prefix metadata
/// to another scope or to a copied allocation.
struct PaidMismatch<'a, T> {
    old: &'a [T],
    new: &'a [T],
    index: usize,
}

/// Preserves the ordinary equal-input scout and full dense admission before
/// an optional lower-cost literal certificate. A failed attempt keeps its
/// charges and enough original allowance for dense proof and reconstruction.
/// Tight allowances may use the complete affordable Hamming band. Ample
/// allowances attempt only a single positional substitution, stopping at the
/// second paid mismatch. Both paths preserve full dense admission and its
/// reconstruction reserve; every failed probe keeps its actual charges.
/// No diagnostic input, physical verdict or cross-scope cache is reused.
pub(super) fn check_for_localization<'a, T: Eq>(
    old: &'a [T],
    new: &'a [T],
    max_distance: usize,
    remaining: &mut usize,
) -> Result<(RetainedCheck<'a, T>, Option<Vec<crate::diff::AtomicEdit>>)> {
    if old.is_empty() || old.len() != new.len() {
        return check_retaining_for_reconstruction(old, new, remaining).map(|r| (r, None));
    }
    let mut identical = true;
    let mut equal_prefix = 0;
    for (a, b) in old.iter().zip(new) {
        if !charge_cell(remaining) {
            return Ok((RetainedCheck::plain(ExactUniqueness::BudgetExceeded), None));
        }
        if a != b {
            identical = false;
            break;
        }
        equal_prefix += 1;
    }
    if identical {
        return Ok((
            RetainedCheck::plain(ExactUniqueness::Unique),
            Some(Vec::new()),
        ));
    }
    let forward = old.len().checked_mul(new.len());
    let dense = forward.and_then(|n| n.checked_mul(2));
    let bytes = old
        .len()
        .checked_add(1)
        .and_then(|rows| {
            new.len()
                .checked_add(1)
                .and_then(|columns| rows.checked_mul(columns))
        })
        .and_then(|cells| cells.checked_add(new.len().checked_add(1)?.checked_mul(2)?))
        .and_then(|cells| cells.checked_mul(size_of::<usize>()));
    let reserve = dense.and_then(|n| {
        old.len()
            .checked_add(new.len())
            .and_then(|length| length.checked_mul(12))
            .and_then(|work| n.checked_add(work))
            .and_then(|work| work.checked_add(128 + 16))
    });
    let uncached_reconstruction = old
        .len()
        .checked_add(new.len())
        .map(|length| length.saturating_mul(max_distance.min(length).saturating_add(1)));
    if let (Some(forward), Some(dense), Some(reserve), Some(bytes)) =
        (forward, dense, reserve, bytes)
        && dense <= *remaining
        && bytes <= MAX_EXACT_MEMORY_BYTES
        && let Some(script) =
            if uncached_reconstruction.is_some_and(|work| work > remaining.saturating_sub(dense)) {
                banded::check_reserved(
                    old,
                    new,
                    max_distance,
                    remaining,
                    reserve,
                    forward.saturating_sub(1),
                )?
            } else {
                banded::check_single_substitution_after_prefix_reserved(
                    PaidMismatch {
                        old,
                        new,
                        index: equal_prefix,
                    },
                    max_distance,
                    remaining,
                    reserve,
                    forward.saturating_sub(1),
                )?
            }
    {
        return Ok((RetainedCheck::plain(ExactUniqueness::Unique), Some(script)));
    }
    check_retaining_internal(old, new, remaining, true, true).map(|r| (r, None))
}

fn check_retaining_internal<'a, T: Eq>(
    old: &'a [T],
    new: &'a [T],
    remaining_cells: &mut usize,
    retain_unique: bool,
    identical_already_checked: bool,
) -> Result<RetainedCheck<'a, T>> {
    if old.is_empty() || new.is_empty() {
        return Ok(RetainedCheck::plain(ExactUniqueness::Unique));
    }

    if old.len() == new.len() && !identical_already_checked {
        let mut identical = true;
        for (old_token, new_token) in old.iter().zip(new) {
            if !charge_cell(remaining_cells) {
                return Ok(RetainedCheck::plain(ExactUniqueness::BudgetExceeded));
            }
            if old_token != new_token {
                identical = false;
                break;
            }
        }

        if identical {
            return Ok(RetainedCheck::plain(ExactUniqueness::Unique));
        }
    }

    let forward_cells = old
        .len()
        .checked_mul(new.len())
        .ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;
    let dp_cells = forward_cells.checked_mul(2).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    if dp_cells > *remaining_cells {
        return Ok(RetainedCheck::plain(ExactUniqueness::BudgetExceeded));
    }

    let columns = new.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let rows = old.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let prefix_cells = rows.checked_mul(columns).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let cell_bytes = std::mem::size_of::<usize>();
    let prefix_bytes = prefix_cells
        .checked_mul(cell_bytes)
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    let row_bytes = columns
        .checked_mul(cell_bytes)
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    let rolling_bytes = row_bytes.checked_mul(2).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_EXACT_MEMORY_BYTES,
    })?;
    let total_bytes = prefix_bytes
        .checked_add(rolling_bytes)
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    if total_bytes > MAX_EXACT_MEMORY_BYTES {
        return Err(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        });
    }

    let mut prefix = Vec::<usize>::new();
    prefix.try_reserve_exact(prefix_cells).map_err(|_| {
        Error::Unresolved("exact uniqueness prefix table allocation failed".to_owned())
    })?;
    prefix.resize(prefix_cells, 0);

    for (old_index, old_token) in old.iter().enumerate() {
        let previous_row = old_index * columns;
        let current_row = (old_index + 1) * columns;
        for new_index in 0..new.len() {
            if !charge_cell(remaining_cells) {
                return Ok(RetainedCheck::plain(ExactUniqueness::BudgetExceeded));
            }
            prefix[current_row + new_index + 1] = if *old_token == new[new_index] {
                prefix[previous_row + new_index]
                    .checked_add(1)
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?
            } else {
                prefix[previous_row + new_index + 1].max(prefix[current_row + new_index])
            };
        }
    }

    let lcs_length = prefix[prefix_cells - 1];
    // Only the current prefix cell is needed for its uniqueness decision.
    // Later reverse cells never read the processed row/column again. The
    // completed lower/right region can therefore store suffix values in place.
    prefix[old.len() * columns..].fill(0);
    for old_index in 0..old.len() {
        prefix[old_index * columns + new.len()] = 0;
    }
    let mut matching_edges = 0usize;

    for old_index in (0..old.len()).rev() {
        let prefix_row = old_index * columns;
        let mut row_has_match = false;
        let mut first_match = None;
        for new_index in (0..new.len()).rev() {
            if !charge_cell(remaining_cells) {
                return Ok(RetainedCheck::plain(ExactUniqueness::BudgetExceeded));
            }
            let suffix_after_match = prefix[(old_index + 1) * columns + new_index + 1];
            prefix[prefix_row + new_index] = if old[old_index] == new[new_index] {
                let through_match = prefix[prefix_row + new_index]
                    .checked_add(1)
                    .and_then(|length| length.checked_add(suffix_after_match))
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?;
                if through_match == lcs_length {
                    matching_edges = matching_edges.checked_add(1).ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?;
                    if row_has_match || matching_edges > lcs_length {
                        let completed = forward_cells - (old_index * new.len() + new_index + 1);
                        let suffix = (completed
                            >= RetainedSuffix::<T>::BINDING_WORK
                                + super::SuffixReuseWork::ACCOUNTING_WORK
                            && prefix.capacity() == prefix_cells)
                            .then_some(RetainedSuffix {
                                old,
                                new,
                                table: prefix,
                                next_old: old_index,
                                next_new: new_index,
                                completed,
                                row_ambiguity: if row_has_match {
                                    first_match.map(|(first_new, first_prefix_rank)| RowAmbiguity {
                                        first_new,
                                        first_prefix_rank,
                                        lcs_length,
                                    })
                                } else {
                                    None
                                },
                            });
                        return Ok(RetainedCheck {
                            outcome: ExactUniqueness::Ambiguous,
                            suffix,
                        });
                    }
                    first_match.get_or_insert((new_index, prefix[prefix_row + new_index]));
                    row_has_match = true;
                }
                suffix_after_match
                    .checked_add(1)
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?
            } else {
                prefix[prefix_row + new_index + 1]
                    .max(prefix[(old_index + 1) * columns + new_index])
            };
        }
    }

    debug_assert_eq!(prefix[0], lcs_length);
    // Ten shallow words hold the suffix input references, vector and cursor
    // fields; six more cover classification, retention and release metadata.
    if retain_unique && matching_edges == lcs_length && *remaining_cells >= 16 {
        if !super::charge(remaining_cells, 16) {
            return Ok(RetainedCheck::plain(ExactUniqueness::Unique));
        }
        return Ok(RetainedCheck {
            outcome: ExactUniqueness::Unique,
            suffix: Some(RetainedSuffix {
                old,
                new,
                table: prefix,
                next_old: 0,
                next_new: 0,
                completed: forward_cells,
                row_ambiguity: None,
            }),
        });
    }
    Ok(RetainedCheck::plain(if matching_edges == lcs_length {
        ExactUniqueness::Unique
    } else {
        ExactUniqueness::Ambiguous
    }))
}

/// Checks uniqueness of a maximum increasing subsequence of distinct positions.
///
/// Each position identifies a different whole-block anchor. Consequently an
/// equal-token LCS between the two anchor orders is exactly an increasing
/// subsequence here. Counts saturate at two: more than one maximum path is enough
/// to withhold the proof, without selecting an arbitrary tie. Repeated positions
/// are rejected because this specialized contract does not apply to them.
pub(super) fn increasing_spine(
    positions: &[usize],
    remaining_work: &mut usize,
    refused_request: &mut usize,
) -> Result<ExactUniqueness> {
    *refused_request = 0;
    if positions.is_empty() {
        return Ok(ExactUniqueness::Unique);
    }
    let cells = positions.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let memory = positions
        .len()
        .checked_mul(std::mem::size_of::<usize>())
        .and_then(|bytes| {
            cells
                .checked_mul(std::mem::size_of::<SpinePaths>())
                .and_then(|tree| bytes.checked_add(tree))
        })
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    if memory > MAX_EXACT_MEMORY_BYTES {
        return Err(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        });
    }
    let levels = (usize::BITS - positions.len().leading_zeros()) as usize;
    let preparation = positions.len().saturating_mul(levels.saturating_add(1));
    if !spend(remaining_work, preparation, refused_request) {
        return Ok(ExactUniqueness::BudgetExceeded);
    }
    let mut sorted = Vec::new();
    sorted
        .try_reserve_exact(positions.len())
        .map_err(|_| Error::Unresolved("anchor order allocation failed".to_owned()))?;
    sorted.extend_from_slice(positions);
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::InvalidConfiguration(
            "anchor positions must be distinct".to_owned(),
        ));
    }
    let mut tree = Vec::<SpinePaths>::new();
    tree.try_reserve_exact(cells)
        .map_err(|_| Error::Unresolved("anchor order tree allocation failed".to_owned()))?;
    tree.resize(cells, SpinePaths::default());
    let mut maximum = SpinePaths::default();
    for position in positions {
        if !spend(remaining_work, levels, refused_request) {
            return Ok(ExactUniqueness::BudgetExceeded);
        }
        let rank = sorted
            .binary_search(position)
            .map_err(|_| Error::InvalidConfiguration("anchor rank missing".to_owned()))?;
        let mut index = rank;
        let mut prefix = SpinePaths::default();
        while index > 0 {
            if !spend(remaining_work, 1, refused_request) {
                return Ok(ExactUniqueness::BudgetExceeded);
            }
            prefix = prefix.merge(tree[index]);
            index &= index - 1;
        }
        let next = SpinePaths {
            length: prefix.length + 1,
            count: if prefix.length == 0 { 1 } else { prefix.count },
        };
        maximum = maximum.merge(next);
        index = rank + 1;
        while index < cells {
            if !spend(remaining_work, 1, refused_request) {
                return Ok(ExactUniqueness::BudgetExceeded);
            }
            tree[index] = tree[index].merge(next);
            index = index.saturating_add(index.isolate_lowest_one());
        }
    }
    Ok(if maximum.count == 1 {
        ExactUniqueness::Unique
    } else {
        ExactUniqueness::Ambiguous
    })
}

#[derive(Clone, Copy, Default)]
struct SpinePaths {
    length: usize,
    count: u8,
}

/// Common boundaries and every competing vertex on maximum increasing paths.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct MandatorySpine {
    pub(super) mandatory: Vec<usize>,
    pub(super) competing: Vec<usize>,
}

/// Returns precisely the common and competing maximum-path vertices.
///
/// Positions must be distinct. A vertex is eligible when its forward and
/// reverse lengths sum to the maximum length plus one. Every maximum path has
/// exactly one eligible vertex at each forward level, so a singleton level is
/// mandatory. Other eligible vertices remain alternatives; none is selected.
/// `None` means the bounded computation did not finish and carries no proof.
/// The retained vectors, including the returned indices, share a 64 MiB cap.
pub(super) fn mandatory_increasing_spine(
    positions: &[usize],
    remaining_work: &mut usize,
    refused_request: &mut usize,
) -> Result<Option<MandatorySpine>> {
    *refused_request = 0;
    if positions.is_empty() {
        return Ok(Some(MandatorySpine {
            mandatory: Vec::new(),
            competing: Vec::new(),
        }));
    }
    let cells = positions.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let memory = cells
        .checked_mul(6)
        .and_then(|cells| cells.checked_mul(std::mem::size_of::<usize>()))
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        })?;
    if memory > MAX_EXACT_MEMORY_BYTES {
        return Err(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_EXACT_MEMORY_BYTES,
        });
    }
    let levels = (usize::BITS - positions.len().leading_zeros()) as usize;
    let preparation = positions.len().saturating_mul(levels.saturating_add(1));
    if !spend(remaining_work, preparation, refused_request) {
        return Ok(None);
    }
    let mut sorted = allocate_row(positions.len(), "mandatory anchor positions")?;
    sorted.copy_from_slice(positions);
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::InvalidConfiguration(
            "anchor positions must be distinct".to_owned(),
        ));
    }
    let mut tree = allocate_row(cells, "mandatory anchor tree")?;
    let mut forward = allocate_row(positions.len(), "mandatory anchor forward lengths")?;
    let mut reverse = allocate_row(positions.len(), "mandatory anchor reverse lengths")?;
    for (backwards, lengths) in [(false, &mut forward), (true, &mut reverse)] {
        tree.fill(0);
        for step in 0..positions.len() {
            let vertex = if backwards {
                positions.len() - 1 - step
            } else {
                step
            };
            if !spend(remaining_work, levels, refused_request) {
                return Ok(None);
            }
            let rank = sorted
                .binary_search(&positions[vertex])
                .map_err(|_| Error::InvalidConfiguration("anchor rank missing".to_owned()))?;
            let rank = if backwards {
                positions.len() - 1 - rank
            } else {
                rank
            };
            let mut index = rank;
            let mut length = 0;
            while index > 0 {
                if !spend(remaining_work, 1, refused_request) {
                    return Ok(None);
                }
                length = length.max(tree[index]);
                index &= index - 1;
            }
            lengths[vertex] = length + 1;
            index = rank + 1;
            while index < cells {
                if !spend(remaining_work, 1, refused_request) {
                    return Ok(None);
                }
                tree[index] = tree[index].max(length + 1);
                index = index.saturating_add(index.isolate_lowest_one());
            }
        }
    }
    if !spend(
        remaining_work,
        positions.len().saturating_mul(4).saturating_add(1),
        refused_request,
    ) {
        return Ok(None);
    }
    let maximum = forward.iter().copied().max().unwrap_or(0);
    let mut eligible_counts = allocate_row(cells, "mandatory anchor level counts")?;
    for (&prefix, &suffix) in forward.iter().zip(&reverse) {
        if prefix + suffix - 1 == maximum {
            eligible_counts[prefix] += 1;
        }
    }
    let (mandatory_count, eligible_count) = eligible_counts
        .iter()
        .fold((0, 0), |(mandatory, eligible), &count| {
            (mandatory + usize::from(count == 1), eligible + count)
        });
    let mut mandatory = Vec::new();
    mandatory
        .try_reserve_exact(mandatory_count)
        .map_err(|_| Error::Unresolved("mandatory anchor result allocation failed".to_owned()))?;
    let mut competing = Vec::new();
    competing
        .try_reserve_exact(eligible_count - mandatory_count)
        .map_err(|_| Error::Unresolved("competing anchor result allocation failed".to_owned()))?;
    for (vertex, (&prefix, &suffix)) in forward.iter().zip(&reverse).enumerate() {
        if prefix + suffix - 1 == maximum {
            if eligible_counts[prefix] == 1 {
                mandatory.push(vertex);
            } else {
                competing.push(vertex);
            }
        }
    }
    Ok(Some(MandatorySpine {
        mandatory,
        competing,
    }))
}

impl SpinePaths {
    fn merge(self, other: Self) -> Self {
        match self.length.cmp(&other.length) {
            std::cmp::Ordering::Less => other,
            std::cmp::Ordering::Greater => self,
            std::cmp::Ordering::Equal => Self {
                length: self.length,
                count: self.count.saturating_add(other.count).min(2),
            },
        }
    }
}

fn spend(remaining: &mut usize, amount: usize, refused: &mut usize) -> bool {
    if let Some(next) = remaining.checked_sub(amount) {
        *remaining = next;
        true
    } else {
        *refused = amount;
        false
    }
}

fn allocate_row(columns: usize, description: &'static str) -> Result<Vec<usize>> {
    let mut row = Vec::new();
    row.try_reserve_exact(columns)
        .map_err(|_| Error::Unresolved(format!("{description} allocation failed")))?;
    row.resize(columns, 0);
    Ok(row)
}

fn charge_cell(remaining_cells: &mut usize) -> bool {
    if *remaining_cells == 0 {
        return false;
    }
    *remaining_cells -= 1;
    true
}

#[cfg(test)]
mod tests {
    use super::super::all_words;
    use super::{ExactUniqueness, check};

    #[test]
    fn unique_match_gap_deletion_order_matches_myers_minimal_reproduction() {
        use crate::diff::{AtomicEdit, myers};
        for old in &all_words(4) {
            for new in &all_words(4) {
                if super::check(old, new, &mut 1_000_000).expect("exact oracle")
                    != super::ExactUniqueness::Unique
                {
                    continue;
                }
                let columns = new.len() + 1;
                let mut suffix = vec![0; (old.len() + 1) * columns];
                for i in (0..old.len()).rev() {
                    for j in (0..new.len()).rev() {
                        suffix[i * columns + j] = if old[i] == new[j] {
                            suffix[(i + 1) * columns + j + 1] + 1
                        } else {
                            suffix[(i + 1) * columns + j].max(suffix[i * columns + j + 1])
                        };
                    }
                }
                let mut pairs = Vec::new();
                let (mut i, mut j) = (0, 0);
                while i < old.len() && j < new.len() {
                    if old[i] == new[j] {
                        pairs.push((i, j));
                        i += 1;
                        j += 1;
                    } else if suffix[i * columns + j + 1] >= suffix[(i + 1) * columns + j] {
                        j += 1;
                    } else {
                        i += 1;
                    }
                }
                let mut edits = Vec::new();
                let (mut i, mut j) = (0, 0);
                for (x, y) in pairs.into_iter().chain([(old.len(), new.len())]) {
                    if i < x {
                        edits.push(AtomicEdit {
                            old: i..x,
                            new: j..j,
                        });
                    }
                    if j < y {
                        edits.push(AtomicEdit {
                            old: x..x,
                            new: j..y,
                        });
                    }
                    i = x + 1;
                    j = y + 1;
                }
                assert_eq!(
                    edits,
                    myers::diff(old, new, 100)
                        .expect("Myers")
                        .expect("within bound"),
                    "old={old:?} new={new:?}"
                );
            }
        }
    }

    #[test]
    fn complete_unique_reconstruction_keeps_myers_order_identity_budget_and_distance() {
        use super::{ExactUniqueness, ReconstructedScript};
        use crate::diff::myers;
        for a in &all_words(4) {
            for b in &all_words(4) {
                let old = a
                    .iter()
                    .map(|x| if *x == 0 { 'a' } else { 'š' })
                    .collect::<Vec<_>>();
                let new = b
                    .iter()
                    .map(|x| if *x == 0 { 'a' } else { 'š' })
                    .collect::<Vec<_>>();
                let checked = super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000)
                    .expect("check");
                if checked.outcome != ExactUniqueness::Unique {
                    continue;
                }
                let Some(table) = checked.suffix else {
                    continue;
                };
                let mut work = 1_000_000;
                let ReconstructedScript::Ready(script) = table
                    .reconstruct_unique(&old, &new, 100, &mut work)
                    .expect("reconstruction")
                else {
                    panic!("complete unique");
                };
                assert_eq!(Some(script), myers::diff(&old, &new, 100).expect("Myers"));
                let used = 1_000_000 - work;
                let checked = super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000)
                    .expect("check");
                assert!(matches!(
                    checked
                        .suffix
                        .expect("complete")
                        .reconstruct_unique(&old, &new, 100, &mut (used - 1))
                        .expect("bounded"),
                    ReconstructedScript::Unavailable
                ));
                let checked = super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000)
                    .expect("check");
                assert!(matches!(
                    checked
                        .suffix
                        .expect("complete")
                        .reconstruct_unique(&old, &new, 0, &mut 1_000_000)
                        .expect("distance"),
                    ReconstructedScript::DistanceExceeded
                ));
                let checked = super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000)
                    .expect("check");
                assert!(matches!(
                    checked
                        .suffix
                        .expect("complete")
                        .reconstruct_unique(&old.clone(), &new, 100, &mut 1_000_000)
                        .expect("identity"),
                    ReconstructedScript::Unavailable
                ));
            }
        }
        let old = [0, 0];
        let new = [0];
        let check = super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000)
            .expect("ambiguous");
        assert_eq!(check.outcome, ExactUniqueness::Ambiguous);
        if let Some(table) = check.suffix {
            assert!(matches!(
                table
                    .reconstruct_unique(&old, &new, 100, &mut 1_000_000)
                    .expect("incomplete"),
                ReconstructedScript::Unavailable
            ));
        }
    }

    #[test]
    fn unique_reconstruction_rejects_reversed_bindings_and_excess_live_capacity() {
        use super::ReconstructedScript;
        let old = ['a', 'b'];
        let new = ['a', 'c'];
        let checked =
            super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000).expect("unique");
        assert!(matches!(
            checked
                .suffix
                .expect("complete")
                .reconstruct_unique(&new, &old, 100, &mut 1_000_000)
                .expect("reversed"),
            ReconstructedScript::Unavailable
        ));
        let checked =
            super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000).expect("unique");
        let mut table = checked.suffix.expect("complete");
        table
            .table
            .try_reserve_exact(super::MAX_EXACT_MEMORY_BYTES / std::mem::size_of::<usize>() + 1)
            .expect("test capacity");
        assert!(matches!(
            table
                .reconstruct_unique(&old, &new, 100, &mut 1_000_000)
                .expect("memory bound"),
            ReconstructedScript::Unavailable
        ));
        let checked =
            super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000).expect("unique");
        let mut work = 3;
        assert!(
            checked
                .suffix
                .expect("complete")
                .complete_for(&old, &new, &mut work)
                .expect("already complete")
                .is_some()
        );
        assert_eq!(work, 0);
    }

    #[test]
    fn retained_suffix_exact_outcomes_and_charges_match_rolling_oracle() {
        let words = all_words(4);
        for old in &words {
            for new in &words {
                for budget in 0..=2 * old.len() * new.len() + old.len() + 2 {
                    let mut original_budget = budget;
                    let mut actual_budget = budget;
                    let original =
                        super::check(old, new, &mut original_budget).expect("rolling oracle");
                    let actual = super::check_retaining(old, new, &mut actual_budget)
                        .expect("in-place check");
                    assert_eq!(
                        original, actual.outcome,
                        "old={old:?} new={new:?} budget={budget}"
                    );
                    assert_eq!(original_budget, actual_budget);
                }
            }
        }
    }

    fn independent_suffix(old: &[u8], new: &[u8]) -> Vec<usize> {
        let columns = new.len() + 1;
        let mut table = vec![0; (old.len() + 1) * columns];
        for i in (0..old.len()).rev() {
            for j in (0..new.len()).rev() {
                table[i * columns + j] = if old[i] == new[j] {
                    table[(i + 1) * columns + j + 1] + 1
                } else {
                    table[(i + 1) * columns + j].max(table[i * columns + j + 1])
                };
            }
        }
        table
    }

    #[test]
    fn retained_suffix_completes_every_cell_in_the_original_allocation() {
        let words = all_words(6);
        let mut continuations = 0;
        for old in &words {
            for new in &words {
                let checked = super::check_retaining(old, new, &mut 1_000_000).expect("check");
                if let Some(suffix) = checked.suffix {
                    assert_eq!(checked.outcome, ExactUniqueness::Ambiguous);
                    let pointer = suffix.table.as_ptr();
                    let capacity = suffix.table.capacity();
                    let mut remaining = suffix.pending_work().expect("pending");
                    let completed = suffix
                        .complete_for(old, new, &mut remaining)
                        .expect("binding")
                        .expect("complete");
                    assert_eq!(remaining, 0);
                    assert_eq!(completed, independent_suffix(old, new));
                    assert_eq!(pointer, completed.as_ptr());
                    assert_eq!(capacity, completed.capacity());
                    continuations += 1;
                }
            }
        }
        assert!(continuations > 0);
        eprintln!(
            "RETAINED_SUFFIX_MINIMAL same_buffer_full_tables={continuations} all_cell_values_match_independent_suffix=true"
        );
    }

    #[test]
    fn retained_suffix_pays_triggering_cell_and_binding() {
        let old = b"AB".repeat(64);
        let new = b"AB".repeat(63);
        let mut remaining = 1_000_000;
        let checked = super::check_retaining(&old, &new, &mut remaining).expect("check");
        assert_eq!(1_000_000 - remaining, 16383);
        let suffix = checked.suffix.expect("retained cells");
        assert_eq!(suffix.completed, 254);
        assert_eq!(suffix.pending_work().expect("pending"), 15877);
        let mut too_small = 15876;
        assert!(
            suffix
                .complete_for(&old, &new, &mut too_small)
                .expect("refusal")
                .is_none()
        );
        assert_eq!(too_small, 0);
    }

    #[test]
    fn retained_suffix_rejects_other_domains_and_reversed_sides() {
        let old = b"AB".repeat(8);
        let new = b"AB".repeat(7);
        let other_old = old.clone();
        let other_new = new.clone();
        for (bound_old, bound_new) in [(&other_old, &other_new), (&new, &old)] {
            let suffix = super::check_retaining(&old, &new, &mut 100_000)
                .expect("check")
                .suffix
                .expect("retained");
            let mut remaining = 100_000;
            assert!(matches!(
                suffix.complete_for(bound_old, bound_new, &mut remaining),
                Err(crate::Error::Unresolved(_))
            ));
            assert_eq!(remaining, 99_997);
        }
    }

    #[test]
    fn retained_suffix_keeps_small_fallback_and_unused_costs() {
        let mut original_budget = 100;
        let mut actual_budget = 100;
        let original = super::check(b"aa", b"a", &mut original_budget).expect("original");
        let actual = super::check_retaining(b"aa", b"a", &mut actual_budget).expect("retaining");
        assert_eq!(original, actual.outcome);
        assert_eq!(original_budget, actual_budget);
        assert!(actual.suffix.is_none());
        let old = b"AB".repeat(8);
        let new = b"AB".repeat(7);
        actual_budget = 100_000;
        let result =
            super::check_retaining(&old, &new, &mut actual_budget).expect("retained proof");
        assert!(result.suffix.is_some());
        let saved = actual_budget;
        drop(result);
        assert_eq!(actual_budget, saved);
    }

    #[test]
    fn retained_suffix_keeps_original_memory_admission_without_rolling_rows() {
        let new = vec![b'b'; 2_200_000];
        let mut original_budget = usize::MAX;
        let original = super::check(b"a", &new, &mut original_budget);
        let mut actual_budget = usize::MAX;
        let actual = super::check_retaining(b"a", &new, &mut actual_budget);
        assert!(matches!(
            original,
            Err(crate::Error::LimitExceeded {
                resource: super::DP_MEMORY_RESOURCE,
                limit: super::MAX_EXACT_MEMORY_BYTES
            })
        ));
        assert!(matches!(
            actual,
            Err(crate::Error::LimitExceeded {
                resource: super::DP_MEMORY_RESOURCE,
                limit: super::MAX_EXACT_MEMORY_BYTES
            })
        ));
    }

    fn classify(old: &[u8], new: &[u8]) -> ExactUniqueness {
        let mut budget = usize::MAX;
        check(old, new, &mut budget).expect("small exact check should fit its budget")
    }

    #[test]
    fn refused_request_distinguishes_fast_path_from_dp_preflight() {
        let mut remaining = 2;
        let mut refused = usize::MAX;
        assert_eq!(
            super::check_recording(b"abc", b"abc", &mut remaining, &mut refused)
                .expect("bounded check"),
            ExactUniqueness::BudgetExceeded
        );
        assert_eq!((remaining, refused), (0, 1));
        remaining = 2;
        assert_eq!(
            super::check_recording(b"ab", b"xyz", &mut remaining, &mut refused)
                .expect("bounded check"),
            ExactUniqueness::BudgetExceeded
        );
        assert_eq!((remaining, refused), (2, 12));
        remaining = 100;
        assert_eq!(
            super::check_recording(b"ab", b"ab", &mut remaining, &mut refused)
                .expect("bounded check"),
            ExactUniqueness::Unique
        );
        assert_eq!((remaining, refused), (98, 0));
    }

    #[test]
    fn increasing_spine_matches_exact_oracle_for_all_small_permutations() {
        fn permutations(values: &mut [usize], start: usize) {
            if start == values.len() {
                let mut order = (0..values.len()).collect::<Vec<_>>();
                order.sort_unstable_by_key(|&index| values[index]);
                let expected = check(
                    &(0..values.len()).collect::<Vec<_>>(),
                    &order,
                    &mut 1_000_000,
                )
                .expect("small oracle");
                let actual =
                    super::increasing_spine(values, &mut 1_000_000, &mut 0).expect("small spine");
                assert_eq!(actual, expected, "positions={values:?}");
                return;
            }
            for next in start..values.len() {
                values.swap(start, next);
                permutations(values, start + 1);
                values.swap(start, next);
            }
        }
        for length in 0..=8 {
            permutations(&mut (0..length).collect::<Vec<_>>(), 0);
        }
    }

    #[test]
    fn mandatory_spine_matches_all_maximum_path_intersections() {
        fn oracle(values: &[usize]) -> super::MandatorySpine {
            let mut maximum = 0;
            let mut common = (0..values.len()).collect::<Vec<_>>();
            let mut eligible = std::collections::BTreeSet::new();
            for mask in 0usize..1 << values.len() {
                let path = (0..values.len())
                    .filter(|&index| mask & (1 << index) != 0)
                    .collect::<Vec<_>>();
                if !path
                    .windows(2)
                    .all(|pair| values[pair[0]] < values[pair[1]])
                {
                    continue;
                }
                match path.len().cmp(&maximum) {
                    std::cmp::Ordering::Greater => {
                        maximum = path.len();
                        eligible = path.iter().copied().collect();
                        common = path;
                    }
                    std::cmp::Ordering::Equal => {
                        eligible.extend(path.iter().copied());
                        common.retain(|vertex| path.contains(vertex));
                    }
                    std::cmp::Ordering::Less => {}
                }
            }
            let competing = eligible
                .into_iter()
                .filter(|vertex| !common.contains(vertex))
                .collect();
            super::MandatorySpine {
                mandatory: common,
                competing,
            }
        }
        fn permutations(values: &mut [usize], start: usize) {
            if start == values.len() {
                let actual = super::mandatory_increasing_spine(values, &mut 1_000_000, &mut 0)
                    .expect("small bounded proof")
                    .expect("finished proof");
                assert_eq!(actual, oracle(values), "positions={values:?}");
                return;
            }
            for next in start..values.len() {
                values.swap(start, next);
                permutations(values, start + 1);
                values.swap(start, next);
            }
        }
        for length in 0..=8 {
            permutations(&mut (0..length).collect::<Vec<_>>(), 0);
        }
    }

    #[test]
    fn mandatory_spine_refuses_every_unfinished_budget() {
        let positions = [0, 2, 1, 3];
        let mut remaining = 1_000;
        assert_eq!(
            super::mandatory_increasing_spine(&positions, &mut remaining, &mut 0)
                .expect("finished proof"),
            Some(super::MandatorySpine {
                mandatory: vec![0, 3],
                competing: vec![1, 2]
            })
        );
        let required = 1_000 - remaining;
        for budget in 0..required {
            assert_eq!(
                super::mandatory_increasing_spine(&positions, &mut { budget }, &mut 0)
                    .expect("bounded proof"),
                None,
                "budget={budget} required={required}"
            );
        }
        assert!(super::mandatory_increasing_spine(&[2, 2], &mut 100, &mut 0).is_err());
        assert_eq!(
            super::mandatory_increasing_spine(&[20, 10], &mut 100, &mut 0)
                .expect("no common vertex"),
            Some(super::MandatorySpine {
                mandatory: vec![],
                competing: vec![0, 1]
            })
        );
    }

    #[test]
    fn mandatory_spine_checks_aggregate_allocation_before_spending_work() {
        let count = super::MAX_EXACT_MEMORY_BYTES / (6 * std::mem::size_of::<usize>()) + 1;
        let positions = vec![0; count];
        let mut remaining = usize::MAX;
        assert!(matches!(
            super::mandatory_increasing_spine(&positions, &mut remaining, &mut 0),
            Err(crate::Error::LimitExceeded {
                resource: super::DP_MEMORY_RESOURCE,
                ..
            })
        ));
        assert_eq!(remaining, usize::MAX);
    }

    #[test]
    fn increasing_spine_withholds_proof_on_budget_and_repeated_positions() {
        let mut remaining = 1;
        let mut refused = 0;
        assert_eq!(
            super::increasing_spine(&[5, 1, 4], &mut remaining, &mut refused)
                .expect("bounded spine"),
            ExactUniqueness::BudgetExceeded
        );
        assert_eq!((remaining, refused), (1, 9));
        assert!(super::increasing_spine(&[5, 5], &mut 100, &mut 0).is_err());
        assert_eq!(
            super::increasing_spine(&[50, 10, 40], &mut 100, &mut 0).expect("sparse ranks"),
            ExactUniqueness::Unique
        );
    }

    #[test]
    fn increasing_spine_never_certifies_an_unfinished_tree() {
        let positions = [5, 1, 4];
        let mut remaining = 1_000;
        assert_eq!(
            super::increasing_spine(&positions, &mut remaining, &mut 0).expect("completed spine"),
            ExactUniqueness::Unique
        );
        let required = 1_000 - remaining;
        for budget in 0..required {
            assert_eq!(
                super::increasing_spine(&positions, &mut { budget }, &mut 0)
                    .expect("bounded spine"),
                ExactUniqueness::BudgetExceeded,
                "budget={budget} required={required}"
            );
        }
    }

    #[test]
    fn repeated_insertion_is_ambiguous() {
        assert_eq!(classify(b"a", b"aa"), ExactUniqueness::Ambiguous);
    }

    #[test]
    fn disjoint_replacements_have_unique_equal_pairs() {
        assert_eq!(classify(b"abcde", b"aXcYe"), ExactUniqueness::Unique);
    }

    #[test]
    fn equal_duplicate_sequences_are_unique() {
        assert_eq!(classify(&[7, 7, 7], &[7, 7, 7]), ExactUniqueness::Unique);
    }

    #[test]
    fn crossing_lcs_choices_are_ambiguous() {
        assert_eq!(classify(b"ab", b"ba"), ExactUniqueness::Ambiguous);
    }

    #[test]
    fn empty_side_is_unique_without_work() {
        let mut budget = 0;
        assert_eq!(
            check::<u8>(&[], &[1], &mut budget).expect("empty side needs no DP"),
            ExactUniqueness::Unique
        );
        assert_eq!(budget, 0);
    }

    #[test]
    fn exhausted_budget_is_shared_across_checks() {
        let mut budget = 10;
        assert_eq!(
            check(b"ab", b"ac", &mut budget).expect("ten cells cover this exact check"),
            ExactUniqueness::Unique
        );
        assert_eq!(budget, 0);
        assert_eq!(
            check(b"ab", b"ac", &mut budget).expect("budget exhaustion is a classification"),
            ExactUniqueness::BudgetExceeded
        );
    }

    #[test]
    fn inverse_inputs_have_the_same_classification() {
        for (old, new) in [
            (b"ab".as_slice(), b"ba".as_slice()),
            (b"a".as_slice(), b"aa".as_slice()),
            (b"abcde".as_slice(), b"aXcYe".as_slice()),
        ] {
            assert_eq!(classify(old, new), classify(new, old));
        }
    }

    #[test]
    fn exhaustive_small_alphabet_matches_pair_sequence_oracle() {
        let words = all_words(3);
        for old in &words {
            for new in &words {
                let expected = if matching_pair_sequences(old, new).len() <= 1 {
                    ExactUniqueness::Unique
                } else {
                    ExactUniqueness::Ambiguous
                };
                assert_eq!(classify(old, new), expected, "old={old:?}, new={new:?}");
            }
        }
    }

    #[test]
    fn paid_prefix_entrypoint_matches_dense_with_repeated_literal_choices() {
        for prefix in [0u16, 64] {
            for n in 1..=4 {
                let words: Vec<Vec<u16>> = (0..1usize << n)
                    .map(|bits| {
                        let mut word: Vec<u16> = (100..100 + prefix).collect();
                        word.extend((0..n).map(|i| u16::from(bits & (1 << i) != 0)));
                        word.extend(300..364);
                        word
                    })
                    .collect();
                for old in &words {
                    for new in &words {
                        let (actual, script) =
                            super::check_for_localization(old, new, 400, &mut 1_000_000)
                                .expect("same-input prefix receipt");
                        let oracle =
                            super::check_retaining_for_reconstruction(old, new, &mut 1_000_000)
                                .expect("complete dense oracle");
                        assert_eq!(actual.outcome, oracle.outcome, "{old:?} -> {new:?}");
                        if let Some(script) = script {
                            assert_eq!(oracle.outcome, ExactUniqueness::Unique);
                            let expected = if old == new {
                                Vec::new()
                            } else {
                                let super::ReconstructedScript::Ready(expected) = oracle
                                    .suffix
                                    .expect("complete unique table")
                                    .reconstruct_unique(old, new, 400, &mut 1_000_000)
                                    .expect("original admission")
                                else {
                                    panic!("dense funded");
                                };
                                expected
                            };
                            assert_eq!(script, expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ample_single_substitution_preserves_dense_proof_and_admission() {
        let old: Vec<u32> = (0..200).collect();
        let mut new = old.clone();
        new[33] = 10_000;
        let mut remaining = 1_000_000;
        let (result, script) = super::check_for_localization(&old, &new, 400, &mut remaining)
            .expect("ample original admission");
        assert_eq!(result.outcome, ExactUniqueness::Unique);
        let script = script.expect("complete radius-one certificate");
        let oracle = super::check_retaining_for_reconstruction(&old, &new, &mut 1_000_000)
            .expect("dense oracle");
        let super::ReconstructedScript::Ready(expected) = oracle
            .suffix
            .expect("dense table")
            .reconstruct_unique(&old, &new, 400, &mut 1_000_000)
            .expect("dense reconstruction")
        else {
            panic!("dense admitted");
        };
        assert_eq!(script, expected);
        assert_eq!(1_000_000 - remaining, 6976 - 3 * 34);
        let mut refused = 80_033;
        let (result, script) = super::check_for_localization(&old, &new, 400, &mut refused)
            .expect("original matrix refusal");
        assert_eq!(result.outcome, ExactUniqueness::BudgetExceeded);
        assert!(script.is_none());
        assert_eq!(refused, 79_999);
    }

    #[test]
    fn admitted_band_preserves_dense_script_with_full_fallback_reserved() {
        let old: Vec<u32> = (0..200).collect();
        let mut new = old.clone();
        new[33] = 10_000;
        let mut work = 100_000;
        let (result, script) = super::check_for_localization(&old, &new, 400, &mut work)
            .expect("admitted literal certificate");
        assert_eq!(result.outcome, ExactUniqueness::Unique);
        assert!(result.suffix.is_none());
        let script = script.expect("complete certified band");
        let oracle = super::check_retaining_for_reconstruction(&old, &new, &mut 100_000)
            .expect("dense oracle");
        let super::ReconstructedScript::Ready(expected) = oracle
            .suffix
            .expect("complete table")
            .reconstruct_unique(&old, &new, 400, &mut 100_000)
            .expect("original admission")
        else {
            panic!("funded dense script");
        };
        assert_eq!(script, expected);
        assert_eq!(100_000 - work, 6976);
        let mut refused = 80_033;
        let (result, script) = super::check_for_localization(&old, &new, 400, &mut refused)
            .expect("ordinary full matrix preflight");
        assert_eq!(result.outcome, ExactUniqueness::BudgetExceeded);
        assert!(script.is_none());
        assert_eq!(refused, 79_999, "only the original scout ran");
    }

    #[test]
    fn negative_band_keeps_charges_and_original_dense_classification() {
        let old = b"ab".repeat(100);
        let new = b"ba".repeat(100);
        let mut original = 100_000;
        let expected = super::check_retaining_for_reconstruction(&old, &new, &mut original)
            .expect("dense oracle");
        let mut actual = 100_000;
        let (result, script) =
            super::check_for_localization(&old, &new, 400, &mut actual).expect("paid fallback");
        assert_eq!(result.outcome, expected.outcome);
        assert_eq!(
            actual + 146,
            original,
            "metadata and six real Hamming probes are not refunded"
        );
        assert!(script.is_none());
        let mut equal = 100_000;
        let (result, script) = super::check_for_localization(&old, &old, 0, &mut equal)
            .expect("ordinary identical fast path");
        assert_eq!(result.outcome, ExactUniqueness::Unique);
        assert_eq!(script, Some(Vec::new()));
        assert_eq!(equal, 99_800);
    }

    #[test]
    fn ample_multiple_substitutions_keep_dense_table_and_pay_failed_probe() {
        let old: Vec<u32> = (0..200).collect();
        let mut new = old.clone();
        new[33] = 10_000;
        new[99] = 20_000;
        let mut expected_work = 1_000_000;
        let expected = super::check_retaining_for_reconstruction(&old, &new, &mut expected_work)
            .expect("ordinary full checker");
        let mut actual_work = 1_000_000;
        let (actual, script) = super::check_for_localization(&old, &new, 400, &mut actual_work)
            .expect("ample allowance");
        assert_eq!(actual.outcome, expected.outcome);
        assert!(script.is_none());
        assert_eq!(actual_work, expected_work - 128 - 3 * (100 - 34));
        let actual = actual.suffix.expect("original complete table");
        let expected = expected.suffix.expect("ordinary complete table");
        assert_eq!(actual.table, expected.table);
        assert_eq!(actual.completed, expected.completed);
    }

    #[test]
    fn same_row_ambiguity_keeps_full_admission_and_pays_each_inspected_cell() {
        use std::cell::Cell;
        struct Token<'a>(u8, &'a Cell<usize>);
        impl PartialEq for Token<'_> {
            fn eq(&self, other: &Self) -> bool {
                self.1.set(self.1.get() + 1);
                self.0 == other.0
            }
        }
        impl Eq for Token<'_> {}
        let probes = Cell::new(0);
        let old: Vec<_> = (0..40).map(|x| Token(x, &probes)).collect();
        let new: Vec<_> = (0..40).chain([39]).map(|x| Token(x, &probes)).collect();
        let forward = old.len() * new.len();
        let mut refused = forward * 2 - 1;
        assert_eq!(
            check(&old, &new, &mut refused).expect("original preflight"),
            ExactUniqueness::BudgetExceeded
        );
        assert_eq!((refused, probes.get()), (forward * 2 - 1, 0));
        let mut work = forward * 2;
        assert_eq!(
            check(&old, &new, &mut work).expect("two optimal last-row edges"),
            ExactUniqueness::Ambiguous
        );
        assert_eq!(work, forward - 2);
        assert_eq!(probes.get(), forward + 2);
        // Only two eligible edges were inspected, fewer than LCS length 40.
        // They cannot both match old index39 in one ordered pair sequence.
        assert_eq!(
            matching_pair_sequences(
                &(0..40).collect::<Vec<_>>(),
                &(0..40).chain([39]).collect::<Vec<_>>()
            )
            .len(),
            2
        );
    }

    #[test]
    fn early_row_certificate_keeps_paid_partial_suffix_cursor_and_allocation() {
        let old: Vec<u8> = (0..40).collect();
        let new: Vec<u8> = old.iter().copied().chain([200; 30]).chain([39]).collect();
        let forward = old.len() * new.len();
        let mut work = forward * 2;
        let checked = super::check_retaining(&old, &new, &mut work).expect("row certificate");
        assert_eq!(checked.outcome, ExactUniqueness::Ambiguous);
        assert_eq!(work, forward - 32);
        let suffix = checked
            .suffix
            .expect("31 completed cells admit same-input continuation");
        let pointer = suffix.table.as_ptr();
        let capacity = suffix.table.capacity();
        assert_eq!(suffix.completed, 31);
        let mut remaining = suffix.pending_work().expect("ordinary pending work");
        assert_eq!(remaining, forward - 31 + 3);
        let table = suffix
            .complete_for(&old, &new, &mut remaining)
            .expect("same immutable inputs")
            .expect("funded");
        assert_eq!(remaining, 0);
        assert_eq!((table.as_ptr(), table.capacity()), (pointer, capacity));
        assert_eq!(table, independent_suffix(&old, &new));
    }

    fn matching_pair_sequences(old: &[u8], new: &[u8]) -> Vec<Vec<(usize, usize)>> {
        let columns = new.len() + 1;
        let mut suffix = vec![0; (old.len() + 1) * columns];
        for old_index in (0..old.len()).rev() {
            for new_index in (0..new.len()).rev() {
                suffix[old_index * columns + new_index] = if old[old_index] == new[new_index] {
                    suffix[(old_index + 1) * columns + new_index + 1] + 1
                } else {
                    suffix[(old_index + 1) * columns + new_index]
                        .max(suffix[old_index * columns + new_index + 1])
                };
            }
        }
        let mut sequences = enumerate_sequences(old, new, &suffix, columns, 0, 0);
        sequences.sort();
        sequences.dedup();
        sequences
    }

    fn enumerate_sequences(
        old: &[u8],
        new: &[u8],
        suffix: &[usize],
        columns: usize,
        old_index: usize,
        new_index: usize,
    ) -> Vec<Vec<(usize, usize)>> {
        if old_index == old.len() || new_index == new.len() {
            return vec![Vec::new()];
        }

        let target = suffix[old_index * columns + new_index];
        let mut sequences = Vec::new();
        if old[old_index] == new[new_index]
            && suffix[(old_index + 1) * columns + new_index + 1] + 1 == target
        {
            for mut sequence in
                enumerate_sequences(old, new, suffix, columns, old_index + 1, new_index + 1)
            {
                sequence.insert(0, (old_index, new_index));
                sequences.push(sequence);
            }
        }
        if suffix[(old_index + 1) * columns + new_index] == target {
            sequences.extend(enumerate_sequences(
                old,
                new,
                suffix,
                columns,
                old_index + 1,
                new_index,
            ));
        }
        if suffix[old_index * columns + new_index + 1] == target {
            sequences.extend(enumerate_sequences(
                old,
                new,
                suffix,
                columns,
                old_index,
                new_index + 1,
            ));
        }
        sequences
    }
}
