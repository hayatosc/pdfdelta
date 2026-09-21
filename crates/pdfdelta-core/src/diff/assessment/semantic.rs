//! Bounded semantic uniqueness checks for optimal insertion/deletion paths.

use super::charge;
use crate::{Error, Result, diff::AtomicEdit};

const MAX_SEMANTIC_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const DP_CELLS_RESOURCE: &str = "semantic uniqueness DP cells";
const DP_MEMORY_RESOURCE: &str = "semantic uniqueness DP memory";

/// Result of comparing the callback output across all optimal edit paths.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome<S> {
    /// Every optimal path produced the same callback signature.
    Unique {
        /// Signature produced by the first optimal path.
        signature: S,
        /// Canonical edits from the first optimal path.
        edits: Vec<AtomicEdit>,
    },
    /// At least two optimal paths produced different signatures.
    Ambiguous,
    /// The shared work budget ended before every optimal path was checked.
    BudgetExceeded,
}

/// Checks semantic signatures once per distinct sequence of equal source pairs.
///
/// The callback must depend only on maximal contiguous changed rectangles and
/// their surrounding equal ranges. Insertion/deletion permutations inside one
/// rectangle have the same boundaries, so one deletion-before-insertion path
/// represents them all. Every distinct equal-token pairing is still examined;
/// this does not choose among ambiguous whitespace or repeated-text locations.
///
/// # Errors
///
/// Returns resource-limit and allocation failures from the bounded traversal,
/// and propagates callback errors.
pub(super) fn check_hunks<T, S, F>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
    signature: F,
) -> Result<Outcome<S>>
where
    T: Eq,
    S: Eq,
    F: FnMut(&[AtomicEdit], &mut usize) -> Result<Option<S>>,
{
    check_with_traversal(
        old,
        new,
        remaining_work,
        signature,
        Traversal::DistinctEqualPairs,
    )
}

#[derive(Clone, Copy)]
enum Traversal {
    #[cfg(test)]
    AllPaths,
    DistinctEqualPairs,
}

/// Checks whether every shortest insertion/deletion alignment has the same
/// caller-defined semantic signature.
///
/// The suffix LCS table identifies every edge that can belong to an optimal
/// path. An iterative depth-first traversal then visits all such paths,
/// retaining one canonical edit script for each callback invocation. A path
/// is accepted only after the traversal is complete and all callback results
/// compare equal.
///
/// `remaining_work` is shared with the caller. One unit is charged for each
/// interior LCS cell and each path edge. Before each callback, the complete
/// raw edit count is charged before canonicalization allocates or traverses
/// the path, then the complete input token count plus the canonical edit count
/// is charged. The callback receives the same counter so it can charge
/// projection and output work before allocating; returning `Ok(None)` reports
/// [`Outcome::BudgetExceeded`].
///
/// # Errors
///
/// Returns [`Error::LimitExceeded`] when table dimensions or retained memory
/// exceed representable or configured limits. Returns [`Error::Unresolved`]
/// when a bounded allocation cannot be reserved. Callback errors are returned
/// unchanged.
#[cfg(test)]
pub(super) fn check<T, S, F>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
    signature: F,
) -> Result<Outcome<S>>
where
    T: Eq,
    S: Eq,
    F: FnMut(&[AtomicEdit], &mut usize) -> Result<Option<S>>,
{
    check_with_traversal(old, new, remaining_work, signature, Traversal::AllPaths)
}

fn check_with_traversal<T, S, F>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
    mut signature: F,
    traversal: Traversal,
) -> Result<Outcome<S>>
where
    T: Eq,
    S: Eq,
    F: FnMut(&[AtomicEdit], &mut usize) -> Result<Option<S>>,
{
    let token_count = old
        .len()
        .checked_add(new.len())
        .ok_or_else(cells_limit_error)?;

    if old.is_empty() || new.is_empty() {
        let edits = one_sided_edits(old.len(), new.len())?;
        if !charge(remaining_work, token_count) {
            return Ok(Outcome::BudgetExceeded);
        }
        return finish_single(edits, token_count, remaining_work, &mut signature);
    }

    if old.len() == new.len() {
        let mut equal = true;
        for (old_token, new_token) in old.iter().zip(new) {
            if !charge(remaining_work, 1) {
                return Ok(Outcome::BudgetExceeded);
            }
            if old_token != new_token {
                equal = false;
                break;
            }
        }
        if equal {
            return finish_single(Vec::new(), token_count, remaining_work, &mut signature);
        }
    }

    let Some(suffix) = build_suffix(old, new, remaining_work)? else {
        return Ok(Outcome::BudgetExceeded);
    };
    enumerate_paths(
        old,
        new,
        &suffix,
        remaining_work,
        token_count,
        &mut signature,
        traversal,
    )
}

fn finish_single<S, F>(
    edits: Vec<AtomicEdit>,
    token_count: usize,
    remaining_work: &mut usize,
    signature: &mut F,
) -> Result<Outcome<S>>
where
    S: Eq,
    F: FnMut(&[AtomicEdit], &mut usize) -> Result<Option<S>>,
{
    let callback_work = token_count
        .checked_add(edits.len())
        .ok_or_else(cells_limit_error)?;
    if !charge(remaining_work, callback_work) {
        return Ok(Outcome::BudgetExceeded);
    }
    let Some(signature) = signature(&edits, remaining_work)? else {
        return Ok(Outcome::BudgetExceeded);
    };
    Ok(Outcome::Unique { signature, edits })
}

fn one_sided_edits(old_len: usize, new_len: usize) -> Result<Vec<AtomicEdit>> {
    if old_len == 0 && new_len == 0 {
        return Ok(Vec::new());
    }

    let mut edits = Vec::new();
    edits
        .try_reserve_exact(1)
        .map_err(|_| Error::Unresolved("semantic uniqueness edit allocation failed".to_owned()))?;
    if old_len == 0 {
        edits.push(AtomicEdit {
            old: 0..0,
            new: 0..new_len,
        });
    } else {
        edits.push(AtomicEdit {
            old: 0..old_len,
            new: 0..0,
        });
    }
    Ok(edits)
}

fn build_suffix<T: Eq>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
) -> Result<Option<Vec<usize>>> {
    let rows = old.len().checked_add(1).ok_or_else(cells_limit_error)?;
    let columns = new.len().checked_add(1).ok_or_else(cells_limit_error)?;
    let cells = rows.checked_mul(columns).ok_or_else(cells_limit_error)?;
    let interior_cells = old
        .len()
        .checked_mul(new.len())
        .ok_or_else(cells_limit_error)?;
    if !chargeable(*remaining_work, interior_cells) {
        *remaining_work = 0;
        return Ok(None);
    }

    preflight_memory(old.len(), new.len())?;

    let mut suffix = Vec::<usize>::new();
    suffix.try_reserve_exact(cells).map_err(|_| {
        Error::Unresolved("semantic uniqueness suffix allocation failed".to_owned())
    })?;
    suffix.resize(cells, 0);

    for old_index in (0..old.len()).rev() {
        for new_index in (0..new.len()).rev() {
            // The preflight above keeps this charge from failing midway, but
            // charging at the cell that is actually computed preserves the
            // shared counter's accounting contract.
            if !charge(remaining_work, 1) {
                return Ok(None);
            }
            let current = old_index * columns + new_index;
            suffix[current] = if old[old_index] == new[new_index] {
                suffix[(old_index + 1) * columns + new_index + 1]
                    .checked_add(1)
                    .ok_or_else(cells_limit_error)?
            } else {
                suffix[(old_index + 1) * columns + new_index]
                    .max(suffix[old_index * columns + new_index + 1])
            };
        }
    }
    Ok(Some(suffix))
}

/// Unique eligible equal edge per rank of every maximum LCS matching.
///
/// `unique_pairs` holds the sorted `(old_index, new_index)` matched pairs
/// (1-based token counts) whose rank has exactly one eligible edge, so every
/// maximum matching contains them.
pub(super) struct MandatoryMatchAnalysis {
    unique_pairs: Vec<(usize, usize)>,
}

impl MandatoryMatchAnalysis {
    /// True when the matched pair ending at `(old_index, new_index)`
    /// (1-based) occurs in every maximum matching.
    pub(super) fn pair_is_mandatory(&self, old_index: usize, new_index: usize) -> bool {
        if old_index == 0 || new_index == 0 {
            return false;
        }
        self.unique_pairs
            .binary_search(&(old_index, new_index))
            .is_ok()
    }

    /// Number of mandatory diagonal pairs along a slice of `length` tokens
    /// starting at the given zero-based offsets.
    pub(super) fn mandatory_diagonal_count(
        &self,
        old_start: usize,
        new_start: usize,
        length: usize,
    ) -> usize {
        (0..length)
            .filter(|offset| self.pair_is_mandatory(old_start + offset + 1, new_start + offset + 1))
            .count()
    }
}

/// Computes the mandatory matched pairs of every maximum LCS matching.
///
/// Suffix values live in one table; prefix values stream through two rolling
/// rows. The full work is charged before any allocation; an unaffordable
/// analysis returns `None` without erasing the shared remainder.
pub(super) fn mandatory_match_analysis<T: Eq>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
) -> Result<Option<MandatoryMatchAnalysis>> {
    let rows = old.len().checked_add(1).ok_or_else(cells_limit_error)?;
    let columns = new.len().checked_add(1).ok_or_else(cells_limit_error)?;
    let cells = rows.checked_mul(columns).ok_or_else(cells_limit_error)?;
    let interior = old
        .len()
        .checked_mul(new.len())
        .ok_or_else(cells_limit_error)?;
    let rank_capacity = old.len().min(new.len());
    let analysis_work = interior
        .checked_mul(3)
        .and_then(|work| work.checked_add(old.len()))
        .and_then(|work| work.checked_add(new.len()))
        .and_then(|work| work.checked_add(cells))
        .ok_or_else(cells_limit_error)?;
    let rows_bytes = 2usize
        .checked_mul(columns)
        .and_then(|count| count.checked_mul(std::mem::size_of::<usize>()))
        .ok_or_else(memory_limit_error)?;
    let rank_bytes = rank_capacity
        .checked_mul(std::mem::size_of::<Option<(usize, usize)>>())
        .and_then(|bytes| bytes.checked_add(rank_capacity))
        .ok_or_else(memory_limit_error)?;
    // Peak live bytes: suffix table, rolling rows, the transient rank slots
    // and flags, and the collected output while the rank slots are still
    // alive, so rank storage is counted twice.
    let bytes = cells
        .checked_mul(std::mem::size_of::<usize>())
        .and_then(|bytes| bytes.checked_add(rows_bytes))
        .and_then(|bytes| bytes.checked_add(rank_bytes))
        .and_then(|bytes| bytes.checked_add(rank_bytes))
        .ok_or_else(memory_limit_error)?;
    if bytes > MAX_SEMANTIC_MEMORY_BYTES {
        return Err(memory_limit_error());
    }
    if !chargeable(*remaining_work, analysis_work) {
        return Ok(None);
    }
    if !charge(remaining_work, analysis_work) {
        return Ok(None);
    }
    let mut suffix = Vec::new();
    suffix.try_reserve_exact(cells).map_err(|_| {
        Error::Unresolved("semantic mandatory-match suffix allocation failed".to_owned())
    })?;
    suffix.resize(cells, 0);
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
    let lcs_length = suffix[0];
    let mut unique_per_rank = Vec::new();
    unique_per_rank.try_reserve_exact(lcs_length).map_err(|_| {
        Error::Unresolved("semantic mandatory-match rank allocation failed".to_owned())
    })?;
    unique_per_rank.resize(lcs_length, None);
    let mut multiple = Vec::new();
    multiple.try_reserve_exact(lcs_length).map_err(|_| {
        Error::Unresolved("semantic mandatory-match flag allocation failed".to_owned())
    })?;
    multiple.resize(lcs_length, false);
    let mut previous = Vec::new();
    previous.try_reserve_exact(columns).map_err(|_| {
        Error::Unresolved("semantic mandatory-match prefix allocation failed".to_owned())
    })?;
    previous.resize(columns, 0);
    let mut current = Vec::new();
    current.try_reserve_exact(columns).map_err(|_| {
        Error::Unresolved("semantic mandatory-match prefix allocation failed".to_owned())
    })?;
    current.resize(columns, 0);
    for old_index in 1..=old.len() {
        current[0] = 0;
        for new_index in 1..=new.len() {
            current[new_index] = if old[old_index - 1] == new[new_index - 1] {
                previous[new_index - 1] + 1
            } else {
                previous[new_index].max(current[new_index - 1])
            };
            if old[old_index - 1] == new[new_index - 1]
                && previous[new_index - 1] + 1 + suffix[old_index * columns + new_index]
                    == lcs_length
            {
                let rank = previous[new_index - 1] + 1;
                if rank <= lcs_length && !multiple[rank - 1] {
                    let edge = (old_index, new_index);
                    match unique_per_rank[rank - 1] {
                        None => unique_per_rank[rank - 1] = Some(edge),
                        Some(existing) if existing == edge => {}
                        Some(_) => {
                            unique_per_rank[rank - 1] = None;
                            multiple[rank - 1] = true;
                        }
                    }
                }
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    drop(suffix);
    drop(previous);
    drop(current);
    drop(multiple);
    let output_len = unique_per_rank.iter().filter(|slot| slot.is_some()).count();
    let mut unique_pairs = Vec::new();
    unique_pairs.try_reserve_exact(output_len).map_err(|_| {
        Error::Unresolved("semantic mandatory-match output allocation failed".to_owned())
    })?;
    for edge in unique_per_rank.into_iter().flatten() {
        unique_pairs.push(edge);
    }
    Ok(Some(MandatoryMatchAnalysis { unique_pairs }))
}

fn enumerate_paths<T, S, F>(
    old: &[T],
    new: &[T],
    suffix: &[usize],
    remaining_work: &mut usize,
    token_count: usize,
    signature: &mut F,
    traversal: Traversal,
) -> Result<Outcome<S>>
where
    T: Eq,
    S: Eq,
    F: FnMut(&[AtomicEdit], &mut usize) -> Result<Option<S>>,
{
    let columns = new.len().checked_add(1).ok_or_else(cells_limit_error)?;
    let mut stack = Vec::new();
    stack.try_reserve_exact(1).map_err(|_| {
        Error::Unresolved("semantic uniqueness traversal allocation failed".to_owned())
    })?;
    stack.push(Frame {
        old_index: 0,
        new_index: 0,
        next_edge: 0,
        path_len_before: 0,
        insertion_started: false,
    });
    let mut path = Vec::new();
    let mut first_signature = None;
    let mut first_edits = None;

    while let Some(last_index) = stack.len().checked_sub(1) {
        let frame = stack[last_index];
        if frame.old_index == old.len() && frame.new_index == new.len() {
            if !charge(remaining_work, path.len()) {
                return Ok(Outcome::BudgetExceeded);
            }
            let edits = canonicalize(&path)?;
            let callback_work = token_count
                .checked_add(edits.len())
                .ok_or_else(cells_limit_error)?;
            if !charge(remaining_work, callback_work) {
                return Ok(Outcome::BudgetExceeded);
            }
            let Some(current_signature) = signature(&edits, remaining_work)? else {
                return Ok(Outcome::BudgetExceeded);
            };
            if let Some(previous_signature) = first_signature.as_ref() {
                if previous_signature != &current_signature {
                    return Ok(Outcome::Ambiguous);
                }
            } else {
                first_signature = Some(current_signature);
                first_edits = Some(edits);
            }
            let frame = stack.pop().ok_or_else(traversal_error)?;
            path.truncate(frame.path_len_before);
            continue;
        }

        let edge = next_edge(old, new, suffix, columns, &mut stack[last_index], traversal);
        let Some((old_index, new_index, edit)) = edge else {
            let frame = stack.pop().ok_or_else(traversal_error)?;
            path.truncate(frame.path_len_before);
            continue;
        };
        if !charge(remaining_work, 1) {
            return Ok(Outcome::BudgetExceeded);
        }
        let path_len_before = path.len();
        if let Some(edit) = edit {
            path.try_reserve_exact(1).map_err(|_| {
                Error::Unresolved("semantic uniqueness path allocation failed".to_owned())
            })?;
            path.push(edit);
        }
        stack.try_reserve_exact(1).map_err(|_| {
            Error::Unresolved("semantic uniqueness traversal allocation failed".to_owned())
        })?;
        let insertion_started = match edit {
            Some(edit) => !edit.deletion || frame.insertion_started,
            None => false,
        };
        stack.push(Frame {
            old_index,
            new_index,
            next_edge: 0,
            path_len_before,
            insertion_started,
        });
    }

    match (first_signature, first_edits) {
        (Some(signature), Some(edits)) => Ok(Outcome::Unique { signature, edits }),
        _ => Err(traversal_error()),
    }
}

fn next_edge<T: Eq>(
    old: &[T],
    new: &[T],
    suffix: &[usize],
    columns: usize,
    frame: &mut Frame,
    traversal: Traversal,
) -> Option<(usize, usize, Option<PathEdit>)> {
    let target = suffix[frame.old_index * columns + frame.new_index];
    while frame.next_edge < 3 {
        let edge = frame.next_edge;
        frame.next_edge += 1;
        match edge {
            0 if frame.old_index < old.len()
                && frame.new_index < new.len()
                && old[frame.old_index] == new[frame.new_index]
                && suffix[(frame.old_index + 1) * columns + frame.new_index + 1].checked_add(1)
                    == Some(target) =>
            {
                return Some((frame.old_index + 1, frame.new_index + 1, None));
            }
            1 if frame.old_index < old.len()
                && !(matches!(traversal, Traversal::DistinctEqualPairs)
                    && frame.insertion_started)
                && suffix[(frame.old_index + 1) * columns + frame.new_index] == target =>
            {
                return Some((
                    frame.old_index + 1,
                    frame.new_index,
                    Some(PathEdit {
                        old_index: frame.old_index,
                        new_index: frame.new_index,
                        deletion: true,
                    }),
                ));
            }
            2 if frame.new_index < new.len()
                && suffix[frame.old_index * columns + frame.new_index + 1] == target =>
            {
                return Some((
                    frame.old_index,
                    frame.new_index + 1,
                    Some(PathEdit {
                        old_index: frame.old_index,
                        new_index: frame.new_index,
                        deletion: false,
                    }),
                ));
            }
            _ => {}
        }
    }
    None
}

fn canonicalize(path: &[PathEdit]) -> Result<Vec<AtomicEdit>> {
    let mut edits = Vec::<AtomicEdit>::new();
    edits.try_reserve_exact(path.len()).map_err(|_| {
        Error::Unresolved("semantic uniqueness canonical edit allocation failed".to_owned())
    })?;
    for step in path {
        let old_end = step
            .old_index
            .checked_add(1)
            .ok_or_else(cells_limit_error)?;
        let new_end = step
            .new_index
            .checked_add(1)
            .ok_or_else(cells_limit_error)?;
        let edit = if step.deletion {
            AtomicEdit {
                old: step.old_index..old_end,
                new: step.new_index..step.new_index,
            }
        } else {
            AtomicEdit {
                old: step.old_index..step.old_index,
                new: step.new_index..new_end,
            }
        };
        if let Some(previous) = edits.last_mut() {
            if previous.old.start != previous.old.end
                && edit.old.start != edit.old.end
                && previous.old.end == edit.old.start
                && previous.new == edit.new
            {
                previous.old.end = edit.old.end;
                continue;
            }
            if previous.old.start == previous.old.end
                && edit.old.start == edit.old.end
                && previous.old == edit.old
                && previous.new.end == edit.new.start
            {
                previous.new.end = edit.new.end;
                continue;
            }
        }
        edits.push(edit);
    }
    Ok(edits)
}

fn preflight_memory(old_len: usize, new_len: usize) -> Result<()> {
    let bytes = required_memory_bytes(old_len, new_len)?;
    if bytes > MAX_SEMANTIC_MEMORY_BYTES {
        return Err(memory_limit_error());
    }
    Ok(())
}

fn required_memory_bytes(old_len: usize, new_len: usize) -> Result<usize> {
    let rows = old_len.checked_add(1).ok_or_else(memory_limit_error)?;
    let columns = new_len.checked_add(1).ok_or_else(memory_limit_error)?;
    let token_count = old_len
        .checked_add(new_len)
        .ok_or_else(memory_limit_error)?;
    let cells = rows.checked_mul(columns).ok_or_else(memory_limit_error)?;
    let suffix_bytes = cells
        .checked_mul(std::mem::size_of::<usize>())
        .ok_or_else(memory_limit_error)?;
    let frame_count = token_count.checked_add(1).ok_or_else(memory_limit_error)?;
    let stack_bytes = frame_count
        .checked_mul(std::mem::size_of::<Frame>())
        .ok_or_else(memory_limit_error)?;
    let path_bytes = token_count
        .checked_mul(std::mem::size_of::<PathEdit>())
        .ok_or_else(memory_limit_error)?;
    let witness_bytes = token_count
        .checked_mul(std::mem::size_of::<AtomicEdit>())
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or_else(memory_limit_error)?;
    suffix_bytes
        .checked_add(stack_bytes)
        .and_then(|bytes| bytes.checked_add(path_bytes))
        .and_then(|bytes| bytes.checked_add(witness_bytes))
        .ok_or_else(memory_limit_error)
}

#[derive(Clone, Copy)]
struct Frame {
    old_index: usize,
    new_index: usize,
    next_edge: u8,
    path_len_before: usize,
    insertion_started: bool,
}

#[derive(Clone, Copy)]
struct PathEdit {
    old_index: usize,
    new_index: usize,
    deletion: bool,
}

fn cells_limit_error() -> Error {
    Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    }
}

fn memory_limit_error() -> Error {
    Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_SEMANTIC_MEMORY_BYTES,
    }
}

fn chargeable(remaining_work: usize, work: usize) -> bool {
    remaining_work >= work
}

fn traversal_error() -> Error {
    Error::Unresolved("semantic uniqueness traversal did not produce a path".to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::all_words;
    use super::{
        AtomicEdit, DP_MEMORY_RESOURCE, MAX_SEMANTIC_MEMORY_BYTES, Outcome, check,
        mandatory_match_analysis, preflight_memory, required_memory_bytes,
    };

    type ScriptSignature = Vec<(usize, usize, usize, usize)>;

    fn semantic_signature(
        old: &[crate::normalize::ComparableToken],
        new: &[crate::normalize::ComparableToken],
        edits: &[AtomicEdit],
    ) -> ScriptSignature {
        let mut result = Vec::new();
        assert!(super::super::super::visit_semantic_hunks(
            old,
            new,
            edits,
            |hunk| {
                result.push((hunk.old.start, hunk.old.end, hunk.new.start, hunk.new.end));
                true
            }
        ));
        result
    }

    #[test]
    fn distinct_equal_pairs_match_all_paths_for_actual_semantic_hunks() {
        use crate::normalize::ComparableToken::Scalar;
        let words = all_words(4);
        for alphabet in [['a', 'b'], ['a', ' '], ['a', ',']] {
            for old in &words {
                for new in &words {
                    let old = old
                        .iter()
                        .map(|&token| Scalar(alphabet[usize::from(token)]))
                        .collect::<Vec<_>>();
                    let new = new
                        .iter()
                        .map(|&token| Scalar(alphabet[usize::from(token)]))
                        .collect::<Vec<_>>();
                    let signature = |edits: &[AtomicEdit], _: &mut usize| {
                        Ok(Some(semantic_signature(&old, &new, edits)))
                    };
                    let mut all_budget = usize::MAX;
                    let mut distinct_budget = usize::MAX;
                    let all =
                        check(&old, &new, &mut all_budget, signature).expect("exhaustive oracle");
                    let distinct = super::check_hunks(&old, &new, &mut distinct_budget, signature)
                        .expect("distinct pair traversal");
                    match (all, distinct) {
                        (
                            Outcome::Unique { signature: a, .. },
                            Outcome::Unique { signature: b, .. },
                        ) => assert_eq!(a, b),
                        (Outcome::Ambiguous, Outcome::Ambiguous) => {}
                        pair => panic!("old={old:?}, new={new:?}, outcomes={pair:?}"),
                    }
                }
            }
        }
    }

    fn exhaustive_mandatory_pairs(
        old: &[u8],
        new: &[u8],
    ) -> std::collections::BTreeSet<(usize, usize)> {
        let columns = new.len() + 1;
        let mut suffix = vec![0usize; (old.len() + 1) * columns];
        for i in (0..old.len()).rev() {
            for j in (0..new.len()).rev() {
                suffix[i * columns + j] = if old[i] == new[j] {
                    suffix[(i + 1) * columns + j + 1] + 1
                } else {
                    suffix[(i + 1) * columns + j].max(suffix[i * columns + j + 1])
                };
            }
        }
        struct Oracle<'a> {
            old: &'a [u8],
            new: &'a [u8],
            suffix: &'a [usize],
            columns: usize,
            paths: Vec<Vec<(usize, usize)>>,
        }
        impl Oracle<'_> {
            fn walk(&mut self, i: usize, j: usize, path: &mut Vec<(usize, usize)>) {
                if i == self.old.len() && j == self.new.len() {
                    self.paths.push(path.clone());
                    return;
                }
                let target = self.suffix[i * self.columns + j];
                if i < self.old.len()
                    && j < self.new.len()
                    && self.old[i] == self.new[j]
                    && self.suffix[(i + 1) * self.columns + j + 1] + 1 == target
                {
                    path.push((i + 1, j + 1));
                    self.walk(i + 1, j + 1, path);
                    path.pop();
                }
                if i < self.old.len() && self.suffix[(i + 1) * self.columns + j] == target {
                    self.walk(i + 1, j, path);
                }
                if j < self.new.len() && self.suffix[i * self.columns + j + 1] == target {
                    self.walk(i, j + 1, path);
                }
            }
        }
        let mut oracle = Oracle {
            old,
            new,
            suffix: &suffix,
            columns,
            paths: Vec::new(),
        };
        oracle.walk(0, 0, &mut Vec::new());
        let paths = oracle.paths;
        let mut mandatory = std::collections::BTreeSet::new();
        for i in 1..=old.len() {
            for j in 1..=new.len() {
                if old[i - 1] == new[j - 1] && paths.iter().all(|path| path.contains(&(i, j))) {
                    mandatory.insert((i, j));
                }
            }
        }
        mandatory
    }

    #[test]
    fn mandatory_analysis_matches_the_exhaustive_path_oracle() {
        for alphabet in [b"ab".as_slice(), b"a ".as_slice()] {
            for old_word in all_words(3) {
                for new_word in all_words(3) {
                    let old: Vec<u8> = old_word
                        .iter()
                        .map(|&token| alphabet[usize::from(token)])
                        .collect();
                    let new: Vec<u8> = new_word
                        .iter()
                        .map(|&token| alphabet[usize::from(token)])
                        .collect();
                    let mut budget = 10_000_000;
                    let analysis = mandatory_match_analysis(&old, &new, &mut budget)
                        .expect("analysis completes")
                        .expect("analysis is affordable");
                    let expected = exhaustive_mandatory_pairs(&old, &new);
                    let mut actual = std::collections::BTreeSet::new();
                    for i in 1..=old.len() {
                        for j in 1..=new.len() {
                            if analysis.pair_is_mandatory(i, j) {
                                actual.insert((i, j));
                            }
                        }
                    }
                    assert_eq!(actual, expected, "old {old:?} new {new:?}");
                }
            }
        }
    }

    #[test]
    fn repeated_equal_text_without_mandatory_positions_is_not_forced() {
        // "aaa" against "aa" has several maximum matchings; no full diagonal
        // can be mandatory across all of them.
        let mut budget = 10_000_000;
        let analysis = mandatory_match_analysis(b"aaa", b"aa", &mut budget)
            .expect("analysis completes")
            .expect("analysis is affordable");
        assert!(
            analysis.mandatory_diagonal_count(0, 0, 2) < 2,
            "repeated equal text without fixed positions must not be forced"
        );
    }

    #[test]
    fn forced_diagonal_requires_every_token_on_a_mandatory_pair() {
        let mut budget = 10_000_000;
        let analysis = mandatory_match_analysis(b"abcXdef", b"abcYdef", &mut budget)
            .expect("analysis completes")
            .expect("analysis is affordable");
        assert_eq!(analysis.mandatory_diagonal_count(0, 0, 3), 3);
        assert_eq!(analysis.mandatory_diagonal_count(4, 4, 3), 3);
        assert_eq!(analysis.mandatory_diagonal_count(0, 0, 7), 6);
    }

    #[test]
    fn unaffordable_analysis_keeps_the_shared_remainder() {
        let old = b"abcdefghijklmnop";
        let new = b"abcdefghijklmnop";
        let mut budget = 3;
        assert!(
            mandatory_match_analysis(old, new, &mut budget)
                .expect("analysis completes")
                .is_none()
        );
        assert_eq!(budget, 3, "the shared remainder is not erased");
    }

    #[test]
    fn commuting_edit_orders_do_not_exhaust_a_shared_hunk_budget() {
        use crate::normalize::ComparableToken::Scalar;
        let old = "XabqqqqqqqqqqY".chars().map(Scalar).collect::<Vec<_>>();
        let new = "ZbappppppppppW".chars().map(Scalar).collect::<Vec<_>>();
        let signature =
            |edits: &[AtomicEdit], _: &mut usize| Ok(Some(semantic_signature(&old, &new, edits)));
        assert!(matches!(
            check(&old, &new, &mut 100_000, signature).expect("bounded exhaustive traversal"),
            Outcome::BudgetExceeded
        ));
        let mut remaining = 100_000;
        let result = super::check_hunks(&old, &new, &mut remaining, signature)
            .expect("bounded distinct pairing traversal");
        assert!(matches!(result, Outcome::Unique { .. }), "{result:?}");
        assert!(remaining > 90_000);
    }

    #[test]
    fn exhaustive_small_alphabet_matches_brute_path_oracle() {
        let words = all_words(3);
        for old in &words {
            for new in &words {
                let expected = brute_signatures(old, new);
                let mut budget = usize::MAX;
                let actual = check(old, new, &mut budget, |edits, _| {
                    Ok(Some(script_signature(edits)))
                })
                .expect("small semantic check should fit its budget");
                match (expected.len(), actual) {
                    (1, Outcome::Unique { signature, .. }) => {
                        assert_eq!(signature, expected[0], "old={old:?}, new={new:?}");
                    }
                    (count, Outcome::Ambiguous) if count > 1 => {}
                    (count, actual) => panic!(
                        "unexpected outcome for old={old:?}, new={new:?}: {count} signatures, {actual:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn equal_semantic_signatures_accept_competing_replacement_orders() {
        let mut budget = usize::MAX;
        let result = check(b"ab", b"ac", &mut budget, |edits, _| {
            let old_start = edits.iter().map(|edit| edit.old.start).min().unwrap_or(0);
            let old_end = edits.iter().map(|edit| edit.old.end).max().unwrap_or(0);
            let new_start = edits.iter().map(|edit| edit.new.start).min().unwrap_or(0);
            let new_end = edits.iter().map(|edit| edit.new.end).max().unwrap_or(0);
            Ok(Some((old_start, old_end, new_start, new_end)))
        })
        .expect("semantic signature callback should succeed");
        assert_eq!(
            result,
            Outcome::Unique {
                signature: (1, 2, 1, 2),
                edits: vec![
                    AtomicEdit {
                        old: 1..2,
                        new: 1..1,
                    },
                    AtomicEdit {
                        old: 2..2,
                        new: 1..2,
                    },
                ],
            }
        );
    }

    #[test]
    fn distinct_semantic_signatures_reject_competing_paths() {
        let mut budget = usize::MAX;
        let result = check(b"ab", b"ac", &mut budget, |edits, _| {
            Ok(Some(edits.first().is_some_and(|edit| !edit.old.is_empty())))
        })
        .expect("semantic signature callback should succeed");
        assert_eq!(result, Outcome::Ambiguous);
    }

    #[test]
    fn budget_exhaustion_never_calls_the_signature_callback() {
        let mut calls = 0;
        let mut budget = 0;
        let result = check(b"ab", b"ac", &mut budget, |_, _| {
            calls += 1;
            Ok(Some(()))
        })
        .expect("budget exhaustion is a classification");
        assert_eq!(result, Outcome::BudgetExceeded);
        assert_eq!(calls, 0);
    }

    #[test]
    fn callback_budget_exhaustion_returns_budget_exceeded() {
        let mut budget = usize::MAX;
        let result = check(b"a", b"b", &mut budget, |_, remaining| {
            *remaining = 0;
            Ok(None::<()>)
        })
        .expect("callback budget exhaustion is a classification");
        assert_eq!(result, Outcome::BudgetExceeded);
        assert_eq!(budget, 0);
    }

    #[test]
    fn skinny_input_memory_preflight_includes_traversal_state() {
        let required =
            required_memory_bytes(1, 1_000_000).expect("test dimensions fit checked arithmetic");
        assert!(required > MAX_SEMANTIC_MEMORY_BYTES);
        assert!(matches!(
            preflight_memory(1, 1_000_000),
            Err(crate::Error::LimitExceeded {
                resource: DP_MEMORY_RESOURCE,
                limit: MAX_SEMANTIC_MEMORY_BYTES,
            })
        ));
    }

    #[test]
    fn empty_and_equal_fast_paths_still_call_the_signature_callback() {
        let mut empty_calls = 0;
        let mut empty_budget = 0;
        let empty = check::<u8, (), _>(&[], &[], &mut empty_budget, |_, _| {
            empty_calls += 1;
            Ok(Some(()))
        })
        .expect("empty inputs need no work");
        assert_eq!(
            empty,
            Outcome::Unique {
                signature: (),
                edits: Vec::new()
            }
        );
        assert_eq!(empty_calls, 1);

        let mut equal_calls = 0;
        let mut equal_budget = 3;
        let equal = check(b"a", b"a", &mut equal_budget, |_, _| {
            equal_calls += 1;
            Ok(Some(()))
        })
        .expect("equal inputs use their fast path");
        assert_eq!(
            equal,
            Outcome::Unique {
                signature: (),
                edits: Vec::new()
            }
        );
        assert_eq!(equal_calls, 1);
    }

    #[test]
    fn target_scoped_hunk_callback_matches_independent_matching_oracle() {
        let words = all_words(3);
        for old in &words {
            for new in &words {
                let lengths = [old.len(), new.len()];
                for old_start in 0..=old.len() {
                    for old_end in old_start..=old.len() {
                        for new_start in 0..=new.len() {
                            for new_end in new_start..=new.len() {
                                let target_old = old_start..old_end;
                                let target_new = new_start..new_end;
                                let expected =
                                    expected_target_signatures(old, new, &target_old, &target_new);
                                let mut budget = usize::MAX;
                                let actual =
                                    super::check_hunks(old, new, &mut budget, |edits, _| {
                                        if !super::super::point_on_script(
                                            [target_old.start, target_new.start],
                                            edits,
                                            lengths,
                                        ) || !super::super::point_on_script(
                                            [target_old.end, target_new.end],
                                            edits,
                                            lengths,
                                        ) {
                                            return Ok(Some(None));
                                        }
                                        Ok(Some(super::super::target_hunk_signature(
                                            &target_old,
                                            &target_new,
                                            edits,
                                        )))
                                    })
                                    .expect("target-scoped traversal fits its budget");
                                match (&expected[..], actual) {
                                    ([only], Outcome::Unique { signature, .. }) => {
                                        assert_eq!(
                                            canonical_signature(&signature),
                                            *only,
                                            "old={old:?}, new={new:?}, targets={target_old:?}/{target_new:?}"
                                        );
                                    }
                                    (many, Outcome::Ambiguous) if many.len() > 1 => {}
                                    (many, actual) => panic!(
                                        "unexpected outcome for old={old:?}, new={new:?}, \
                                         targets={target_old:?}/{target_new:?}: \
                                         {many:?} signatures, {actual:?}"
                                    ),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn independent_matching_oracle_reports_indel_model_distances() {
        // The production model matches equal pairs and turns every unmatched
        // token into a deletion or insertion. `ab` to `cd` has no equal pair,
        // so its one optimal script is the single hunk 0..2 to 0..2.
        let expected = expected_target_signatures(b"ab", b"cd", &(0..2), &(0..2));
        assert_eq!(expected.len(), 1, "{expected:?}");
        assert_eq!(
            expected[0],
            Some((0, 2, 0, 2, vec![(0, 2, 0, 2)])),
            "{expected:?}"
        );
        // `ab` to `ba` has two maximum matchings and therefore two different
        // whole-target signatures.
        let swapped = expected_target_signatures(b"ab", b"ba", &(0..2), &(0..2));
        assert_eq!(swapped.len(), 2, "{swapped:?}");
        // A target no matching hunk touches stays absent on every path.
        let absent = expected_target_signatures(b"ab", b"ab", &(1..2), &(1..2));
        assert_eq!(absent, vec![None], "{absent:?}");
    }

    #[test]
    fn repeated_positions_make_a_swap_target_ambiguous() {
        let mut budget = usize::MAX;
        let target_old = 0..1;
        let target_new = 0..1;
        let actual = super::check_hunks(b"ab", b"ba", &mut budget, |edits, _| {
            Ok(Some(super::super::target_hunk_signature(
                &target_old,
                &target_new,
                edits,
            )))
        })
        .expect("swap traversal fits its budget");
        assert_eq!(actual, Outcome::Ambiguous);
    }

    type CanonicalSignature = Option<(usize, usize, usize, usize, ScriptSignature)>;

    fn canonical_signature(
        signature: &Option<super::super::ProposalHunkSignature>,
    ) -> CanonicalSignature {
        signature.as_ref().map(|(old, new, hunks)| {
            (
                old.start,
                old.end,
                new.start,
                new.end,
                hunks
                    .iter()
                    .map(|(old, new)| (old.start, old.end, new.start, new.end))
                    .collect(),
            )
        })
    }

    /// Independent expected set: enumerate every maximum equal-pair matching,
    /// derive its changed hunks from the unmatched gaps, and apply the target
    /// contract (boundary points plus contained strict hunks) without calling
    /// any production function.
    fn expected_target_signatures(
        old: &[u8],
        new: &[u8],
        target_old: &std::ops::Range<usize>,
        target_new: &std::ops::Range<usize>,
    ) -> Vec<CanonicalSignature> {
        let mut matchings = Vec::new();
        enumerate_matchings(old, new, 0, 0, &mut Vec::new(), &mut matchings);
        let best = matchings
            .iter()
            .map(Vec::len)
            .max()
            .expect("the empty matching always exists");
        let lengths = (old.len(), new.len());
        let mut signatures = matchings
            .into_iter()
            .filter(|matching| matching.len() == best)
            .map(|matching| {
                let hunks = matching_hunks(old, new, &matching);
                if !independent_on_script((target_old.start, target_new.start), &hunks, lengths)
                    || !independent_on_script((target_old.end, target_new.end), &hunks, lengths)
                {
                    return None;
                }
                independent_signature(&hunks, target_old, target_new)
            })
            .collect::<Vec<_>>();
        signatures.sort();
        signatures.dedup();
        signatures
    }

    fn enumerate_matchings(
        old: &[u8],
        new: &[u8],
        old_index: usize,
        new_index: usize,
        path: &mut Vec<(usize, usize)>,
        output: &mut Vec<Vec<(usize, usize)>>,
    ) {
        output.push(path.clone());
        for i in old_index..old.len() {
            for j in new_index..new.len() {
                if old[i] == new[j] {
                    path.push((i, j));
                    enumerate_matchings(old, new, i + 1, j + 1, path, output);
                    path.pop();
                }
            }
        }
    }

    fn matching_hunks(
        old: &[u8],
        new: &[u8],
        matching: &[(usize, usize)],
    ) -> Vec<(usize, usize, usize, usize)> {
        let mut hunks = Vec::new();
        let mut previous = (0usize, 0usize);
        for &(old_match, new_match) in matching {
            if previous.0 < old_match || previous.1 < new_match {
                hunks.push((previous.0, old_match, previous.1, new_match));
            }
            previous = (old_match + 1, new_match + 1);
        }
        if previous.0 < old.len() || previous.1 < new.len() {
            hunks.push((previous.0, old.len(), previous.1, new.len()));
        }
        hunks
    }

    fn independent_on_script(
        point: (usize, usize),
        hunks: &[(usize, usize, usize, usize)],
        lengths: (usize, usize),
    ) -> bool {
        let mut cursor = (0usize, 0usize);
        for &(old_start, old_end, new_start, new_end) in hunks {
            if point.0 >= cursor.0
                && point.0 <= old_start
                && point.1 >= cursor.1
                && point.1 <= new_start
                && point.0 - cursor.0 == point.1 - cursor.1
            {
                return true;
            }
            if point == (old_start, new_start) || point == (old_end, new_end) {
                return true;
            }
            cursor = (old_end, new_end);
        }
        point.0 >= cursor.0
            && point.0 <= lengths.0
            && point.1 >= cursor.1
            && point.1 <= lengths.1
            && point.0 - cursor.0 == point.1 - cursor.1
    }

    fn independent_signature(
        hunks: &[(usize, usize, usize, usize)],
        target_old: &std::ops::Range<usize>,
        target_new: &std::ops::Range<usize>,
    ) -> CanonicalSignature {
        let mut contained = Vec::new();
        for &(old_start, old_end, new_start, new_end) in hunks {
            let old_overlap = old_start < target_old.end && target_old.start < old_end;
            let new_overlap = new_start < target_new.end && target_new.start < new_end;
            let old_inside = target_old.start <= old_start && old_end <= target_old.end;
            let new_inside = target_new.start <= new_start && new_end <= target_new.end;
            if (old_overlap && !old_inside) || (new_overlap && !new_inside) {
                return None;
            }
            if old_inside && new_inside {
                contained.push((old_start, old_end, new_start, new_end));
            }
        }
        if contained.is_empty() {
            return None;
        }
        Some((
            target_old.start,
            target_old.end,
            target_new.start,
            target_new.end,
            contained,
        ))
    }

    fn script_signature(edits: &[AtomicEdit]) -> ScriptSignature {
        edits
            .iter()
            .map(|edit| (edit.old.start, edit.old.end, edit.new.start, edit.new.end))
            .collect()
    }

    fn brute_signatures(old: &[u8], new: &[u8]) -> Vec<ScriptSignature> {
        let mut paths = Vec::new();
        enumerate_brute(old, new, 0, 0, 0, &mut Vec::new(), &mut paths);
        let best_matches = paths
            .iter()
            .map(|path| path.0)
            .max()
            .expect("every finite pair has an edit path");
        let mut signatures = paths
            .into_iter()
            .filter(|(matches, _)| *matches == best_matches)
            .map(|(_, path)| script_signature(&canonicalize_brute(&path)))
            .collect::<Vec<_>>();
        signatures.sort();
        signatures.dedup();
        signatures
    }

    fn enumerate_brute(
        old: &[u8],
        new: &[u8],
        old_index: usize,
        new_index: usize,
        matches: usize,
        path: &mut Vec<AtomicEdit>,
        paths: &mut Vec<(usize, Vec<AtomicEdit>)>,
    ) {
        if old_index == old.len() && new_index == new.len() {
            paths.push((matches, path.clone()));
            return;
        }
        if old_index < old.len() {
            path.push(AtomicEdit {
                old: old_index..old_index + 1,
                new: new_index..new_index,
            });
            enumerate_brute(old, new, old_index + 1, new_index, matches, path, paths);
            path.pop();
        }
        if new_index < new.len() {
            path.push(AtomicEdit {
                old: old_index..old_index,
                new: new_index..new_index + 1,
            });
            enumerate_brute(old, new, old_index, new_index + 1, matches, path, paths);
            path.pop();
        }
        if old_index < old.len() && new_index < new.len() && old[old_index] == new[new_index] {
            enumerate_brute(
                old,
                new,
                old_index + 1,
                new_index + 1,
                matches + 1,
                path,
                paths,
            );
        }
    }

    fn canonicalize_brute(path: &[AtomicEdit]) -> Vec<AtomicEdit> {
        let mut edits = Vec::new();
        for edit in path {
            let Some(previous) = edits.last_mut() else {
                edits.push(edit.clone());
                continue;
            };
            if !previous.old.is_empty()
                && !edit.old.is_empty()
                && previous.old.end == edit.old.start
                && previous.new == edit.new
            {
                previous.old.end = edit.old.end;
            } else if previous.old.is_empty()
                && edit.old.is_empty()
                && previous.old == edit.old
                && previous.new.end == edit.new.start
            {
                previous.new.end = edit.new.end;
            } else {
                edits.push(edit.clone());
            }
        }
        edits
    }
}
