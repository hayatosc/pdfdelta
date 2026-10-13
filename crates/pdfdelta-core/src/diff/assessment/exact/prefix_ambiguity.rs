//! Two different optimal matching-pair sequences certify ambiguity.
//!
//! The immutable, already paid prefix table certifies every chosen predecessor:
//! a skip preserves rank and a literal diagonal decreases rank by one. Two
//! opposite skip priorities may expose different pairs at the same rank. Those
//! pairs cannot belong to the same ordered LCS; the unvisited prefix still has
//! exactly the rank required to complete each path. Equal paths make no claim.
//! No source verdict, input cache or retained matching-pair array is reused.

fn upper_work(old: usize, new: usize) -> Option<usize> {
    old.checked_add(new)?
        .checked_mul(32)?
        .checked_add(old.min(new).checked_mul(8)?)?
        .checked_add(128)
}

pub(super) fn admitted(old: usize, new: usize, reverse: usize, remaining: usize) -> bool {
    upper_work(old, new)
        .and_then(|work| work.checked_add(reverse))
        .is_some_and(|work| work <= remaining)
}

/// Prepaid metadata covers four cursors, ranks, pair slots and table release.
/// Each predecessor step costs 16 before reading ranks or token equality;
/// each comparison of same-rank pairs costs eight. The dimension bound admits
/// two complete paths without refunding any executed step on a negative exit.
pub(super) fn check<T: Eq>(old: &[T], new: &[T], prefix: &[usize], remaining: &mut usize) -> bool {
    let Some(work) = upper_work(old.len(), new.len()) else {
        return false;
    };
    let Some(columns) = new.len().checked_add(1) else {
        return false;
    };
    if old
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_mul(columns))
        != Some(prefix.len())
        || work > *remaining
        || !super::super::charge(remaining, 128)
    {
        return false;
    }
    let mut left = (old.len(), new.len());
    let mut right = left;
    loop {
        let a = next_pair(old, new, prefix, &mut left, true, remaining);
        let b = next_pair(old, new, prefix, &mut right, false, remaining);
        let (Some(a), Some(b)) = (a, b) else {
            return false;
        };
        if !super::super::charge(remaining, 8) {
            return false;
        }
        if a != b {
            return true;
        }
    }
}

fn next_pair<T: Eq>(
    old: &[T],
    new: &[T],
    prefix: &[usize],
    cursor: &mut (usize, usize),
    old_first: bool,
    remaining: &mut usize,
) -> Option<(usize, usize)> {
    let columns = new.len() + 1;
    while cursor.0 > 0 && cursor.1 > 0 {
        if !super::super::charge(remaining, 16) {
            return None;
        }
        let (i, j) = *cursor;
        let rank = prefix[i * columns + j];
        if (old_first && prefix[(i - 1) * columns + j] == rank)
            || (!old_first && prefix[i * columns + j - 1] == rank)
        {
            if old_first {
                cursor.0 -= 1;
            } else {
                cursor.1 -= 1;
            }
        } else if old[i - 1] == new[j - 1]
            && prefix[(i - 1) * columns + j - 1].checked_add(1) == Some(rank)
        {
            *cursor = (i - 1, j - 1);
            return Some(*cursor);
        } else if prefix[(i - 1) * columns + j] == rank {
            cursor.0 -= 1;
        } else if prefix[i * columns + j - 1] == rank {
            cursor.1 -= 1;
        } else {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::check;

    fn prefix(old: &[u8], new: &[u8]) -> Vec<usize> {
        let c = new.len() + 1;
        let mut p = vec![0; (old.len() + 1) * c];
        for i in 0..old.len() {
            for j in 0..new.len() {
                p[(i + 1) * c + j + 1] = if old[i] == new[j] {
                    p[i * c + j] + 1
                } else {
                    p[i * c + j + 1].max(p[(i + 1) * c + j])
                };
            }
        }
        p
    }

    #[test]
    fn positive_certificates_agree_with_independent_pair_sequence_enumeration() {
        use std::collections::BTreeSet;
        let words: Vec<Vec<u8>> = (0..=4)
            .flat_map(|n| {
                (0..1usize << n).map(move |bits| {
                    (0..n)
                        .map(|i| b'a' + u8::from(bits & (1 << i) != 0))
                        .collect()
                })
            })
            .collect();
        let mut claims = 0;
        for old in &words {
            for new in &words {
                let mut sequences = BTreeSet::from([Vec::<(usize, usize)>::new()]);
                for (i, a) in old.iter().enumerate() {
                    let mut additions = Vec::new();
                    for (j, b) in new.iter().enumerate() {
                        if a == b {
                            for seq in &sequences {
                                if seq.last().is_none_or(|&(x, y)| x < i && y < j) {
                                    let mut extended = seq.clone();
                                    extended.push((i, j));
                                    additions.push(extended);
                                }
                            }
                        }
                    }
                    sequences.extend(additions);
                }
                let length = sequences
                    .iter()
                    .map(Vec::len)
                    .max()
                    .expect("empty sequence");
                if check(old, new, &prefix(old, new), &mut 1_000_000) {
                    assert!(sequences.iter().filter(|seq| seq.len() == length).count() > 1);
                    claims += 1;
                }
            }
        }
        assert!(claims > 0);
    }

    #[test]
    fn skips_equal_tokens_to_expose_pairs_and_keeps_negative_charges() {
        let p = prefix(b"aa", b"a");
        let mut budget = 1_000;
        assert!(check(b"aa", b"a", &p, &mut budget));
        assert_eq!(1_000 - budget, 128 + 3 * 16 + 8);
        let mut negative = 1_000;
        assert!(!check(b"ab", b"ac", &prefix(b"ab", b"ac"), &mut negative));
        assert!(negative < 1_000);
        let mut refused = 1;
        assert!(!check(b"aa", b"a", &p, &mut refused));
        assert_eq!(refused, 1);
        assert!(!super::admitted(343, 388, 133_084, 133_084));
        assert!(super::admitted(343, 388, 133_084, 204_802));
    }
}
