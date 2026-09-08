//! Bounded exact uniqueness checks for token alignments.

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
///
/// # Errors
///
/// Returns [`Error::LimitExceeded`] when dimensions, cell counts, or the
/// retained DP memory would exceed representable or configured limits. Returns
/// [`Error::Unresolved`] when a bounded DP allocation cannot be reserved.
pub(super) fn check<T: Eq>(
    old: &[T],
    new: &[T],
    remaining_cells: &mut usize,
) -> Result<ExactUniqueness> {
    if old.is_empty() || new.is_empty() {
        return Ok(ExactUniqueness::Unique);
    }

    if old.len() == new.len() {
        let mut identical = true;
        for (old_token, new_token) in old.iter().zip(new) {
            if !charge_cell(remaining_cells) {
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
        for new_index in (0..new.len()).rev() {
            if !charge_cell(remaining_cells) {
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
                    if matching_edges > lcs_length {
                        return Ok(ExactUniqueness::Ambiguous);
                    }
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
    use super::{ExactUniqueness, check};

    fn classify(old: &[u8], new: &[u8]) -> ExactUniqueness {
        let mut budget = usize::MAX;
        check(old, new, &mut budget).expect("small exact check should fit its budget")
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
