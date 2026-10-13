//! Exact uniqueness of one insertion or deletion, without a matching graph.
//!
//! With a length difference of one, every shortest alignment matching the
//! entire shorter input has a single gap. Its possible gap positions range
//! from `short_len - common_suffix` through `common_prefix`, inclusive. Equal
//! endpoints certify the sole matching-pair sequence; overlap means ambiguity.
//! This is only a literal proof; source, context and ownership stay with callers.

use super::MAX_EXACT_MEMORY_BYTES;
use crate::{
    Result,
    diff::{AtomicEdit, myers},
};

/// Uses only the structurally eligible one-edit domain after dense refusal.
/// Other dimensions construct no proof and inspect no token. Both scans are
/// prepaid at six units per combined token, including comparison/cursors and
/// output-position binding. Metadata covers original Myers admission and live
/// allocation checks; sixteen units cover the single edit's construction and
/// retention. Refused or ambiguous executions never refund prepaid work.
pub(in super::super) fn check<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    if old.len().abs_diff(new.len()) != 1 || old.is_empty() || new.is_empty() {
        return Ok(None);
    }
    let Some(length) = old.len().checked_add(new.len()) else {
        return Ok(None);
    };
    let Some(work) = length.checked_mul(6).and_then(|x| x.checked_add(128 + 16)) else {
        return Ok(None);
    };
    let bytes = size_of::<AtomicEdit>() + size_of::<Vec<AtomicEdit>>() + 256;
    if work > *remaining || bytes > MAX_EXACT_MEMORY_BYTES {
        return Ok(None);
    }
    if !super::super::charge(remaining, 128) {
        return Ok(None);
    }
    let (bound, _) = myers::reconstruction_admission(old.len(), new.len(), max_distance)?;
    if bound == 0 {
        return Ok(None);
    }
    if !super::super::charge(remaining, work - 128) {
        return Ok(None);
    }
    let short_len = old.len().min(new.len());
    let mut prefix = 0;
    while prefix < short_len && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < short_len && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    if prefix + suffix != short_len {
        return Ok(None);
    }
    let mut edits = Vec::new();
    if edits.try_reserve_exact(1).is_err() || edits.capacity() != 1 {
        return Ok(None);
    }
    edits.push(if old.len() < new.len() {
        AtomicEdit {
            old: prefix..prefix,
            new: prefix..prefix + 1,
        }
    } else {
        AtomicEdit {
            old: prefix..prefix + 1,
            new: prefix..prefix,
        }
    });
    Ok(Some(edits))
}

#[cfg(test)]
mod tests {
    use super::super::{ExactUniqueness, ReconstructedScript, check_retaining_for_reconstruction};
    use super::check;

    #[test]
    fn single_edit_matches_dense_oracle_and_script_both_directions() {
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
                let is_single_edit = {
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
                    i == short.len() && old.len().abs_diff(new.len()) == 1
                };
                if is_single_edit && dense.outcome == ExactUniqueness::Unique {
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
    fn single_edit_budget_distance_and_repeated_identity_guards() {
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
