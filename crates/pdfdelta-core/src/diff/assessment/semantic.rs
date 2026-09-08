//! Bounded semantic uniqueness checks for optimal insertion/deletion paths.

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
        .ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;

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

    let suffix = match build_suffix(old, new, remaining_work)? {
        Some(suffix) => suffix,
        None => return Ok(Outcome::BudgetExceeded),
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
        .ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;
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
    let rows = old.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let columns = new.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let cells = rows.checked_mul(columns).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
    let interior_cells = old
        .len()
        .checked_mul(new.len())
        .ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;
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
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?
            } else {
                suffix[(old_index + 1) * columns + new_index]
                    .max(suffix[old_index * columns + new_index + 1])
            };
        }
    }
    Ok(Some(suffix))
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
    let columns = new.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_CELLS_RESOURCE,
        limit: usize::MAX,
    })?;
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
            let callback_work =
                token_count
                    .checked_add(edits.len())
                    .ok_or(Error::LimitExceeded {
                        resource: DP_CELLS_RESOURCE,
                        limit: usize::MAX,
                    })?;
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
        let old_end = step.old_index.checked_add(1).ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;
        let new_end = step.new_index.checked_add(1).ok_or(Error::LimitExceeded {
            resource: DP_CELLS_RESOURCE,
            limit: usize::MAX,
        })?;
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
        return Err(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_SEMANTIC_MEMORY_BYTES,
        });
    }
    Ok(())
}

fn required_memory_bytes(old_len: usize, new_len: usize) -> Result<usize> {
    let rows = old_len.checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_SEMANTIC_MEMORY_BYTES,
    })?;
    let columns = new_len.checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_SEMANTIC_MEMORY_BYTES,
    })?;
    let token_count = old_len.checked_add(new_len).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_SEMANTIC_MEMORY_BYTES,
    })?;
    let cells = rows.checked_mul(columns).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_SEMANTIC_MEMORY_BYTES,
    })?;
    let suffix_bytes =
        cells
            .checked_mul(std::mem::size_of::<usize>())
            .ok_or(Error::LimitExceeded {
                resource: DP_MEMORY_RESOURCE,
                limit: MAX_SEMANTIC_MEMORY_BYTES,
            })?;
    let frame_count = token_count.checked_add(1).ok_or(Error::LimitExceeded {
        resource: DP_MEMORY_RESOURCE,
        limit: MAX_SEMANTIC_MEMORY_BYTES,
    })?;
    let stack_bytes = frame_count
        .checked_mul(std::mem::size_of::<Frame>())
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_SEMANTIC_MEMORY_BYTES,
        })?;
    let path_bytes = token_count
        .checked_mul(std::mem::size_of::<PathEdit>())
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_SEMANTIC_MEMORY_BYTES,
        })?;
    let witness_bytes = token_count
        .checked_mul(std::mem::size_of::<AtomicEdit>())
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_SEMANTIC_MEMORY_BYTES,
        })?;
    suffix_bytes
        .checked_add(stack_bytes)
        .and_then(|bytes| bytes.checked_add(path_bytes))
        .and_then(|bytes| bytes.checked_add(witness_bytes))
        .ok_or(Error::LimitExceeded {
            resource: DP_MEMORY_RESOURCE,
            limit: MAX_SEMANTIC_MEMORY_BYTES,
        })
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

fn chargeable(remaining_work: usize, work: usize) -> bool {
    remaining_work >= work
}

fn charge(remaining_work: &mut usize, work: usize) -> bool {
    if !chargeable(*remaining_work, work) {
        *remaining_work = 0;
        return false;
    }
    *remaining_work -= work;
    true
}

fn traversal_error() -> Error {
    Error::Unresolved("semantic uniqueness traversal did not produce a path".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicEdit, DP_MEMORY_RESOURCE, MAX_SEMANTIC_MEMORY_BYTES, Outcome, check,
        preflight_memory, required_memory_bytes,
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

    fn script_signature(edits: &[AtomicEdit]) -> ScriptSignature {
        edits
            .iter()
            .map(|edit| (edit.old.start, edit.old.end, edit.new.start, edit.new.end))
            .collect()
    }

    fn all_words(max_length: usize) -> Vec<Vec<u8>> {
        let mut words = Vec::new();
        for length in 0..=max_length {
            append_words(&mut words, &mut Vec::new(), length);
        }
        words
    }

    fn append_words(words: &mut Vec<Vec<u8>>, current: &mut Vec<u8>, remaining: usize) {
        if remaining == 0 {
            words.push(current.clone());
            return;
        }
        for token in 0..=1 {
            current.push(token);
            append_words(words, current, remaining - 1);
            current.pop();
        }
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
