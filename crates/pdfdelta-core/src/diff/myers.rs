use super::MAX_MYERS_EDIT_DISTANCE;
use crate::{Error, Result};

const MAX_MYERS_TRACE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Equal,
    Delete,
    Insert,
}

pub(super) fn diff<T: Eq>(
    old: &[T],
    new: &[T],
    max_edit_distance: usize,
) -> Result<Option<Vec<Edit>>> {
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
) -> Result<Vec<Edit>> {
    let mut old_index = old.len();
    let mut new_index = new.len();
    let mut reversed = Vec::with_capacity(old.len() + new.len());

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

        while old_index > previous_old && new_index > previous_new {
            reversed.push(Edit::Equal);
            old_index -= 1;
            new_index -= 1;
        }
        if old_index == previous_old {
            reversed.push(Edit::Insert);
            new_index -= 1;
        } else {
            reversed.push(Edit::Delete);
            old_index -= 1;
        }
    }

    while old_index > 0 && new_index > 0 && old[old_index - 1] == new[new_index - 1] {
        reversed.push(Edit::Equal);
        old_index -= 1;
        new_index -= 1;
    }
    if old_index != 0 || new_index != 0 {
        return Err(Error::Unresolved(
            "Myers backtracking did not reach the origin".to_owned(),
        ));
    }

    reversed.reverse();
    Ok(reversed)
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
    use super::{Edit, MAX_MYERS_TRACE_BYTES, diff, maximum_trace_bytes};
    use crate::diff::MAX_MYERS_EDIT_DISTANCE;

    #[test]
    fn returns_only_equal_edits_for_identical_input() {
        assert_eq!(
            diff(b"same", b"same", 0)
                .expect("identical input should diff")
                .expect("identical input fits any limit"),
            vec![Edit::Equal; 4]
        );
    }

    #[test]
    fn handles_empty_and_one_sided_inputs() {
        assert_eq!(
            diff(b"", b"", 0)
                .expect("empty input should diff")
                .expect("empty input fits any limit"),
            Vec::<Edit>::new()
        );
        assert_eq!(
            diff(b"abc", b"", 3)
                .expect("deletion should diff")
                .expect("deletion should fit the limit"),
            vec![Edit::Delete; 3]
        );
        assert_eq!(
            diff(b"", b"abc", 3)
                .expect("insertion should diff")
                .expect("insertion should fit the limit"),
            vec![Edit::Insert; 3]
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
        assert_eq!(edits.iter().filter(|edit| **edit != Edit::Equal).count(), 5);
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
            vec![Edit::Equal; 3]
        );
    }

    #[test]
    fn configured_cap_fits_the_trace_allocation_budget() {
        let bytes = maximum_trace_bytes(MAX_MYERS_EDIT_DISTANCE)
            .expect("the configured edit-distance cap should have a finite trace size");

        assert!(bytes <= MAX_MYERS_TRACE_BYTES);
    }

    fn assert_script(old: &[u8], new: &[u8], edits: &[Edit]) {
        let mut old_index = 0;
        let mut new_index = 0;
        for edit in edits {
            match edit {
                Edit::Equal => {
                    assert_eq!(old[old_index], new[new_index]);
                    old_index += 1;
                    new_index += 1;
                }
                Edit::Delete => old_index += 1,
                Edit::Insert => new_index += 1,
            }
        }
        assert_eq!(old_index, old.len());
        assert_eq!(new_index, new.len());
    }
}
