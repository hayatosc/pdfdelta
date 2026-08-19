use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Equal,
    Delete,
    Insert,
}

pub(super) fn diff<T: Eq>(old: &[T], new: &[T], max_edit_distance: usize) -> Result<Vec<Edit>> {
    let max_distance = old
        .len()
        .checked_add(new.len())
        .ok_or(Error::LimitExceeded {
            resource: "Myers input tokens",
            limit: usize::MAX,
        })?;
    if max_distance == 0 {
        return Ok(Vec::new());
    }
    if max_distance > isize::MAX as usize {
        return Err(Error::LimitExceeded {
            resource: "Myers input tokens",
            limit: isize::MAX as usize,
        });
    }

    let frontier_len = max_distance
        .checked_mul(2)
        .and_then(|length| length.checked_add(3))
        .ok_or(Error::LimitExceeded {
            resource: "Myers frontier entries",
            limit: usize::MAX,
        })?;
    let offset = max_distance as isize + 1;
    let mut frontier = vec![0; frontier_len];
    frontier[index(1, offset)] = 0;
    let mut trace = Vec::new();

    for distance in 0..=max_distance.min(max_edit_distance) {
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
                return backtrack(old, new, &trace, distance as usize);
            }
        }
        trace.push(layer);
    }

    Err(Error::LimitExceeded {
        resource: "Myers edit distance",
        limit: max_edit_distance,
    })
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
    use super::{Edit, diff};
    use crate::Error;

    #[test]
    fn returns_only_equal_edits_for_identical_input() {
        assert_eq!(
            diff(b"same", b"same", 0).expect("identical input should diff"),
            vec![Edit::Equal; 4]
        );
    }

    #[test]
    fn handles_empty_and_one_sided_inputs() {
        assert_eq!(
            diff(b"", b"", 0).expect("empty input should diff"),
            Vec::<Edit>::new()
        );
        assert_eq!(
            diff(b"abc", b"", 3).expect("deletion should fit the limit"),
            vec![Edit::Delete; 3]
        );
        assert_eq!(
            diff(b"", b"abc", 3).expect("insertion should fit the limit"),
            vec![Edit::Insert; 3]
        );
    }

    #[test]
    fn produces_a_valid_shortest_script_for_repeated_tokens() {
        let old = b"ABCABBA";
        let new = b"CBABAC";
        let edits = diff(old, new, 5).expect("known edit distance should fit the limit");

        assert_script(old, new, &edits);
        assert_eq!(edits.iter().filter(|edit| **edit != Edit::Equal).count(), 5);
    }

    #[test]
    fn enforces_the_edit_distance_limit() {
        assert!(matches!(
            diff(b"before", b"after", 2),
            Err(Error::LimitExceeded {
                resource: "Myers edit distance",
                limit: 2
            })
        ));
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
