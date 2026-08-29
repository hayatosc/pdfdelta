use std::ops::Range;

use super::MAX_MYERS_EDIT_DISTANCE;
use crate::{Error, Result};

const MAX_MYERS_TRACE_BYTES: usize = 64 * 1024 * 1024;

/// One coalesced insertion or deletion in a Myers edit script.
///
/// Exactly one range is non-empty. Equal runs are the same-length gaps between
/// adjacent edits, so the representation is bounded by the edit distance. The
/// range coordinates are relative to the corresponding context in
/// `MatchedAtomicDiff`, not document-global token offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicEdit {
    /// Changed old-token range, or an empty range for an insertion.
    pub old: Range<usize>,
    /// Changed new-token range, or an empty range for a deletion.
    pub new: Range<usize>,
}

impl AtomicEdit {
    pub(super) fn is_deletion(&self) -> bool {
        !self.old.is_empty()
    }

    pub(super) fn changed_token_count(&self) -> usize {
        self.old.len() + self.new.len()
    }
}

pub(super) fn diff<T: Eq>(
    old: &[T],
    new: &[T],
    max_edit_distance: usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    let max_distance = old
        .len()
        .checked_add(new.len())
        .ok_or(Error::LimitExceeded {
            resource: "Myers input tokens",
            limit: usize::MAX,
        })?;
    if max_distance == 0 {
        return Ok(Some(Vec::new()));
    }

    // Only diagonals within the capped distance are ever visited, so the
    // frontier is sized on the effective bound instead of the full input
    // length.
    let distance_bound = max_distance
        .min(max_edit_distance)
        .min(MAX_MYERS_EDIT_DISTANCE);
    // The offset arithmetic below casts the effective internal bound to
    // isize. Keep the representation check local even though the current
    // trace-allocation cap is much smaller.
    if distance_bound > isize::MAX as usize {
        return Err(Error::LimitExceeded {
            resource: "Myers diagonal offset",
            limit: isize::MAX as usize,
        });
    }
    let frontier_len = distance_bound
        .checked_mul(2)
        .and_then(|length| length.checked_add(3))
        .ok_or(Error::LimitExceeded {
            resource: "Myers frontier entries",
            limit: usize::MAX,
        })?;
    let trace_bytes = maximum_trace_bytes(distance_bound).ok_or(Error::LimitExceeded {
        resource: "Myers trace bytes",
        limit: MAX_MYERS_TRACE_BYTES,
    })?;
    if trace_bytes > MAX_MYERS_TRACE_BYTES {
        return Err(Error::LimitExceeded {
            resource: "Myers trace bytes",
            limit: MAX_MYERS_TRACE_BYTES,
        });
    }
    let offset = distance_bound as isize + 1;
    let mut frontier = vec![0; frontier_len];
    frontier[index(1, offset)] = 0;
    let mut trace = Vec::new();

    for distance in 0..=distance_bound {
        let distance = distance as isize;
        let mut layer = Vec::with_capacity(distance as usize + 1);
        for diagonal in (-distance..=distance).step_by(2) {
            let mut old_index = if diagonal == -distance
                || (diagonal != distance
                    && frontier[index(diagonal - 1, offset)]
                        < frontier[index(diagonal + 1, offset)])
            {
                frontier[index(diagonal + 1, offset)]
            } else {
                frontier[index(diagonal - 1, offset)] + 1
            };
            let mut new_index = (old_index as isize - diagonal) as usize;

            while old_index < old.len() && new_index < new.len() && old[old_index] == new[new_index]
            {
                old_index += 1;
                new_index += 1;
            }

            frontier[index(diagonal, offset)] = old_index;
            layer.push(old_index);
            if old_index == old.len() && new_index == new.len() {
                return backtrack(old, new, &trace, distance as usize).map(Some);
            }
        }
        trace.push(layer);
    }

    // The edit script exceeds the configured distance budget. Callers degrade
    // this matched span instead of failing the whole comparison.
    Ok(None)
}

fn maximum_trace_bytes(distance_bound: usize) -> Option<usize> {
    let layer_count = distance_bound.checked_add(1)?;
    let layer_entries = layer_count
        .checked_mul(distance_bound.checked_add(2)?)?
        .checked_div(2)?;
    let frontier_entries = distance_bound.checked_mul(2)?.checked_add(3)?;
    layer_entries
        .checked_add(frontier_entries)?
        .checked_mul(std::mem::size_of::<usize>())?
        .checked_add(layer_count.checked_mul(std::mem::size_of::<Vec<usize>>())?)
}

fn backtrack<T: Eq>(
    old: &[T],
    new: &[T],
    trace: &[Vec<usize>],
    edit_distance: usize,
) -> Result<Vec<AtomicEdit>> {
    let mut old_index = old.len();
    let mut new_index = new.len();
    let mut reversed = Vec::with_capacity(edit_distance);

    for distance in (1..=edit_distance).rev() {
        let diagonal = old_index as isize - new_index as isize;
        let distance_diagonal = distance as isize;
        let previous = &trace[distance - 1];
        let previous_diagonal = if diagonal == -distance_diagonal
            || (diagonal != distance_diagonal
                && layer_value(previous, distance - 1, diagonal - 1)
                    < layer_value(previous, distance - 1, diagonal + 1))
        {
            diagonal + 1
        } else {
            diagonal - 1
        };
        let previous_old = layer_value(previous, distance - 1, previous_diagonal);
        let previous_new = (previous_old as isize - previous_diagonal) as usize;
        let insertion = previous_diagonal == diagonal + 1;
        let Some(snake_old) = previous_old.checked_add(usize::from(!insertion)) else {
            return Err(backtrack_error());
        };
        let Some(snake_new) = previous_new.checked_add(usize::from(insertion)) else {
            return Err(backtrack_error());
        };
        if snake_old > old_index
            || snake_new > new_index
            || old_index - snake_old != new_index - snake_new
            || old[snake_old..old_index] != new[snake_new..new_index]
        {
            return Err(backtrack_error());
        }
        old_index = snake_old;
        new_index = snake_new;
        if insertion {
            let Some(start) = new_index.checked_sub(1) else {
                return Err(backtrack_error());
            };
            reversed.push(AtomicEdit {
                old: old_index..old_index,
                new: start..new_index,
            });
            new_index = start;
        } else {
            let Some(start) = old_index.checked_sub(1) else {
                return Err(backtrack_error());
            };
            reversed.push(AtomicEdit {
                old: start..old_index,
                new: new_index..new_index,
            });
            old_index = start;
        }
    }

    if old_index != new_index || old[..old_index] != new[..new_index] {
        return Err(backtrack_error());
    }

    Ok(coalesce_reversed(reversed))
}

fn coalesce_reversed(reversed: Vec<AtomicEdit>) -> Vec<AtomicEdit> {
    let mut edits: Vec<AtomicEdit> = Vec::with_capacity(reversed.len());
    for edit in reversed.into_iter().rev() {
        if let Some(previous) = edits.last_mut() {
            if previous.is_deletion()
                && edit.is_deletion()
                && previous.old.end == edit.old.start
                && previous.new == edit.new
            {
                previous.old.end = edit.old.end;
                continue;
            }
            if !previous.is_deletion()
                && !edit.is_deletion()
                && previous.old == edit.old
                && previous.new.end == edit.new.start
            {
                previous.new.end = edit.new.end;
                continue;
            }
        }
        edits.push(edit);
    }
    edits
}

fn backtrack_error() -> Error {
    Error::Unresolved("Myers backtracking did not reach the origin".to_owned())
}

fn index(diagonal: isize, offset: isize) -> usize {
    (diagonal + offset) as usize
}

fn layer_value(layer: &[usize], distance: usize, diagonal: isize) -> usize {
    let position = (diagonal + distance as isize) / 2;
    layer[position as usize]
}

#[cfg(test)]
mod tests {
    use super::{AtomicEdit, MAX_MYERS_TRACE_BYTES, diff, maximum_trace_bytes};
    use crate::diff::MAX_MYERS_EDIT_DISTANCE;

    #[test]
    fn identical_input_needs_no_atomic_edits() {
        assert_eq!(
            diff(b"same", b"same", 0)
                .expect("identical input should diff")
                .expect("identical input fits any limit"),
            Vec::<AtomicEdit>::new()
        );
    }

    #[test]
    fn handles_empty_and_one_sided_inputs() {
        assert_eq!(
            diff(b"", b"", 0)
                .expect("empty input should diff")
                .expect("empty input fits any limit"),
            Vec::<AtomicEdit>::new()
        );
        assert_eq!(
            diff(b"abc", b"", 3)
                .expect("deletion should diff")
                .expect("deletion should fit the limit"),
            vec![AtomicEdit {
                old: 0..3,
                new: 0..0,
            }]
        );
        assert_eq!(
            diff(b"", b"abc", 3)
                .expect("insertion should diff")
                .expect("insertion should fit the limit"),
            vec![AtomicEdit {
                old: 0..0,
                new: 0..3,
            }]
        );
    }

    #[test]
    fn produces_a_valid_shortest_script_for_repeated_tokens() {
        let old = b"ABCABBA";
        let new = b"CBABAC";
        let edits = diff(old, new, 5)
            .expect("known edit distance should diff")
            .expect("known edit distance should fit the limit");

        assert_script(old, new, &edits);
        assert_eq!(
            edits
                .iter()
                .map(AtomicEdit::changed_token_count)
                .sum::<usize>(),
            5
        );
    }

    #[test]
    fn reports_none_when_the_edit_distance_exceeds_the_limit() {
        assert_eq!(diff(b"before", b"after", 2), Ok(None));
    }

    #[test]
    fn accepts_usize_max_for_inputs_within_the_internal_cap() {
        assert_eq!(
            diff(b"abc", b"abc", usize::MAX)
                .expect("identical input should diff")
                .expect("identical input fits an uncapped budget"),
            Vec::<AtomicEdit>::new()
        );
    }

    #[test]
    fn configured_cap_fits_the_trace_allocation_budget() {
        let bytes = maximum_trace_bytes(MAX_MYERS_EDIT_DISTANCE)
            .expect("the configured edit-distance cap should have a finite trace size");

        assert!(bytes <= MAX_MYERS_TRACE_BYTES);
    }

    #[test]
    fn coalesces_adjacent_operations_and_infers_equal_islands() {
        let old = b"prefix OLD middle tail";
        let new = b"prefix NEW middle tails";
        let edits = diff(old, new, 8)
            .expect("bounded diff should run")
            .expect("known edit distance should fit the limit");

        assert_script(old, new, &edits);
        assert!(edits.len() <= 8);
        assert!(
            edits
                .iter()
                .all(|edit| edit.old.is_empty() ^ edit.new.is_empty())
        );
    }

    #[test]
    fn reconstructs_all_short_binary_sequences() {
        let sequences = binary_sequences(4);
        for old in &sequences {
            for new in &sequences {
                let edits = diff(old, new, old.len() + new.len())
                    .expect("short input should diff")
                    .expect("the full edit-distance budget should fit");
                assert_script(old, new, &edits);
                assert!(edits.len() <= old.len() + new.len());
            }
        }
    }

    fn assert_script(old: &[u8], new: &[u8], edits: &[AtomicEdit]) {
        let mut old_index = 0;
        let mut new_index = 0;
        let mut reconstructed = Vec::with_capacity(new.len());
        for edit in edits {
            assert!(edit.old.is_empty() ^ edit.new.is_empty());
            assert!(edit.old.start >= old_index);
            assert!(edit.new.start >= new_index);
            assert_eq!(
                &old[old_index..edit.old.start],
                &new[new_index..edit.new.start]
            );
            reconstructed.extend_from_slice(&old[old_index..edit.old.start]);
            reconstructed.extend_from_slice(&new[edit.new.clone()]);
            old_index = edit.old.end;
            new_index = edit.new.end;
        }
        assert_eq!(&old[old_index..], &new[new_index..]);
        reconstructed.extend_from_slice(&old[old_index..]);
        assert_eq!(reconstructed, new);
        assert!(edits.windows(2).all(|pair| {
            !(pair[0].is_deletion() == pair[1].is_deletion()
                && ((pair[0].is_deletion()
                    && pair[0].old.end == pair[1].old.start
                    && pair[0].new == pair[1].new)
                    || (!pair[0].is_deletion()
                        && pair[0].old == pair[1].old
                        && pair[0].new.end == pair[1].new.start)))
        }));
    }

    fn binary_sequences(max_len: usize) -> Vec<Vec<u8>> {
        let mut sequences = vec![Vec::new()];
        for len in 1..=max_len {
            for bits in 0..1usize << len {
                sequences.push(
                    (0..len)
                        .map(|index| b'a' + ((bits >> index) & 1) as u8)
                        .collect(),
                );
            }
        }
        sequences
    }
}
