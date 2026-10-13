//! Exact uniqueness for a complete embedding of the shorter token sequence.
//!
//! Earliest and latest greedy embeddings bound every complete embedding
//! componentwise. If both coincide, every shortest alignment matches those
//! same pairs: its LCS contains the entire shorter input. Other cases carry
//! no claim. Physical source, context and ownership remain caller obligations.

use super::MAX_EXACT_MEMORY_BYTES;
use crate::{
    Result,
    diff::{AtomicEdit, myers},
};

/// Builds only a unique full-subsequence script after dense budget refusal.
/// Equal-length and empty inputs keep the existing paths without constructing
/// an index. No hash, cached input or partially inspected sequence is evidence.
///
/// Preflight covers both full greedy scans (six units per combined token),
/// both retained position arrays (two words per shorter token), and the same
/// eight-unit token traversal/four-unit edit capacity reconstruction allowance
/// as the dense retained-table path. Metadata includes original Myers admission
/// and all live capacities. Every executed phase is paid; unused prepaid work
/// is retained on refusal. A failed dimensional preflight performs no scan.
pub(in super::super) fn check<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    if old.len() == new.len() || old.is_empty() || new.is_empty() {
        return Ok(None);
    }
    let Some(length) = old.len().checked_add(new.len()) else {
        return Ok(None);
    };
    let short_len = old.len().min(new.len());
    let distance = old.len().abs_diff(new.len());
    let work = length
        .checked_mul(14)
        .and_then(|x| x.checked_add(short_len.checked_mul(2)?))
        .and_then(|x| x.checked_add(distance.checked_mul(4)?))
        .and_then(|x| x.checked_add(128));
    let bytes = short_len
        .checked_mul(2 * size_of::<usize>())
        .and_then(|x| x.checked_add(distance.checked_mul(size_of::<AtomicEdit>())?))
        .and_then(|x| x.checked_add(256));
    let Some(work) = work else {
        return Ok(None);
    };
    if work > *remaining || bytes.is_none_or(|x| x > MAX_EXACT_MEMORY_BYTES) {
        return Ok(None);
    }
    if !super::super::charge(remaining, 128) {
        return Ok(None);
    }
    let (bound, _) = myers::reconstruction_admission(old.len(), new.len(), max_distance)?;
    if distance > bound {
        return Ok(None);
    }
    if !super::super::charge(remaining, work - 128) {
        return Ok(None);
    }
    let (short, long) = if old.len() < new.len() {
        (old, new)
    } else {
        (new, old)
    };
    let mut earliest = Vec::new();
    let mut latest = Vec::new();
    if earliest.try_reserve_exact(short_len).is_err()
        || latest.try_reserve_exact(short_len).is_err()
        || earliest.capacity() != short_len
        || latest.capacity() != short_len
    {
        return Ok(None);
    }
    let mut i = 0;
    for (j, token) in long.iter().enumerate() {
        if short[i] == *token {
            earliest.push(j);
            i += 1;
        }
        if i == short_len {
            break;
        }
    }
    if i != short_len {
        return Ok(None);
    }
    latest.resize(short_len, 0usize);
    for (j, token) in long.iter().enumerate().rev() {
        if short[i - 1] == *token {
            i -= 1;
            latest[i] = j;
        }
        if i == 0 {
            break;
        }
    }
    if i != 0 || earliest != latest {
        return Ok(None);
    }
    let mut edits = Vec::new();
    if edits.try_reserve_exact(distance).is_err() || edits.capacity() != distance {
        return Ok(None);
    }
    let mut cursor = 0;
    for (i, &j) in earliest.iter().enumerate() {
        if cursor < j {
            edits.push(if old.len() < new.len() {
                AtomicEdit {
                    old: i..i,
                    new: cursor..j,
                }
            } else {
                AtomicEdit {
                    old: cursor..j,
                    new: i..i,
                }
            });
        }
        cursor = j + 1;
    }
    if cursor < long.len() {
        edits.push(if old.len() < new.len() {
            AtomicEdit {
                old: short_len..short_len,
                new: cursor..long.len(),
            }
        } else {
            AtomicEdit {
                old: cursor..long.len(),
                new: short_len..short_len,
            }
        });
    }
    Ok(Some(edits))
}

#[cfg(test)]
mod tests {
    use super::super::{ExactUniqueness, ReconstructedScript, check_retaining_for_reconstruction};
    use super::check;

    #[test]
    fn full_subsequence_matches_dense_oracle_and_script_both_directions() {
        let mut words = Vec::new();
        for length in 1..=5 {
            for bits in 0..(1usize << length) {
                words.push(
                    (0..length)
                        .map(|i| {
                            if bits & (1 << i) == 0 {
                                '日'
                            } else {
                                '\u{10ffff}'
                            }
                        })
                        .collect::<Vec<_>>(),
                );
            }
        }
        for old in &words {
            for new in &words {
                let result = check(old, new, 100, &mut 1_000_000).expect("linear check");
                let dense = check_retaining_for_reconstruction(old, new, &mut 1_000_000)
                    .expect("dense oracle");
                let is_full_subsequence = {
                    let (short, long) = if old.len() < new.len() {
                        (old, new)
                    } else {
                        (new, old)
                    };
                    let mut i = 0;
                    for token in long {
                        if i < short.len() && short[i] == *token {
                            i += 1;
                        }
                    }
                    i == short.len() && old.len() != new.len()
                };
                if is_full_subsequence && dense.outcome == ExactUniqueness::Unique {
                    let ReconstructedScript::Ready(expected) = dense
                        .suffix
                        .expect("unique retained table")
                        .reconstruct_unique(old, new, 100, &mut 1_000_000)
                        .expect("dense script")
                    else {
                        panic!("dense admission")
                    };
                    assert_eq!(result, Some(expected), "{old:?} -> {new:?}");
                } else {
                    assert!(result.is_none(), "{old:?} -> {new:?}");
                }
            }
        }
    }

    #[test]
    fn full_subsequence_budget_distance_and_repeated_identity_guards() {
        let old = ['a', '日', 'b'];
        let new = ['a', '日', '9', 'b'];
        let mut allowance = 1_000_000;
        let script = check(&old, &new, 100, &mut allowance)
            .expect("unique")
            .expect("script");
        let paid = 1_000_000 - allowance;
        let mut exact = paid;
        assert_eq!(
            check(&old, &new, 100, &mut exact).expect("exact allowance"),
            Some(script)
        );
        assert_eq!(exact, 0);
        let mut refused = paid - 1;
        assert!(
            check(&old, &new, 100, &mut refused)
                .expect("preflight refusal")
                .is_none()
        );
        assert_eq!(refused, paid - 1, "no token scan began");
        let mut distance = paid;
        assert!(
            check(&old, &new, 0, &mut distance)
                .expect("distance refusal")
                .is_none()
        );
        assert_eq!(distance, paid - 128, "admission is paid without scanning");
        assert!(
            check(&['a'], &['a', 'a'], 100, &mut 1_000_000)
                .expect("distinct embeddings")
                .is_none()
        );
        assert!(
            check(&['a', 'a'], &['a', 'a', 'a'], 100, &mut 1_000_000)
                .expect("duplicate endpoints")
                .is_none()
        );
        let mut equal = 100;
        assert!(
            check(&['a'], &['b'], 100, &mut equal)
                .expect("equal length outside domain")
                .is_none()
        );
        assert_eq!(equal, 100);
    }
}
