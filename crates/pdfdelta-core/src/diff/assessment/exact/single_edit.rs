//! Exact uniqueness of one insertion or deletion, without a matching graph.
//!
//! A maximal equal prefix fixes the latest possible gap. If the extra token
//! equals the preceding prefix token, a complete embedding would have multiple
//! gaps; an incomplete embedding has none. Both cases can safely return no
//! claim without inspecting the tail. Otherwise only that gap is possible,
//! and the complete shifted suffix proves the unique matching-pair sequence.
//! Physical source, context and ownership remain caller responsibilities.

use super::MAX_EXACT_MEMORY_BYTES;
use crate::{
    Result,
    diff::{AtomicEdit, myers},
};

/// Preflights complete checker work, then pays each executed phase beforehand.
/// Three units per pair cover literal inspection, coordinate bookkeeping and
/// cursor advancement; every prefix, boundary and shifted-tail probe is paid.
/// A complete attempt needs at most `short_len + 2` probes. Original Myers
/// admission/live-allocation metadata costs 128; output construction and
/// retention costs 16 and is paid before allocation only for a complete unique
/// witness. Early negative decisions never refund an executed charge and make
/// no positive or physical claim. Downstream proof fees remain unchanged.
pub(in super::super) fn check<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    if old.len().abs_diff(new.len()) != 1 || old.is_empty() || new.is_empty() {
        return Ok(None);
    }
    let short_len = old.len().min(new.len());
    let Some(work) = short_len
        .checked_add(2)
        .and_then(|x| x.checked_mul(3))
        .and_then(|x| x.checked_add(128 + 16))
    else {
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
    let (short, long) = if old.len() < new.len() {
        (old, new)
    } else {
        (new, old)
    };
    let mut prefix = 0;
    while prefix < short_len {
        if !super::super::charge(remaining, 3) {
            return Ok(None);
        }
        if short[prefix] != long[prefix] {
            break;
        }
        prefix += 1;
    }
    if prefix > 0 {
        if !super::super::charge(remaining, 3) {
            return Ok(None);
        }
        if long[prefix] == short[prefix - 1] {
            return Ok(None);
        }
    }
    for i in prefix..short_len {
        if !super::super::charge(remaining, 3) {
            return Ok(None);
        }
        if short[i] != long[i + 1] {
            return Ok(None);
        }
    }
    if !super::super::charge(remaining, 16) {
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
    #[test]
    fn negative_boundary_stops_before_tail_and_charges_each_real_probe() {
        use std::cell::Cell;
        struct Token<'a> {
            value: char,
            probes: &'a Cell<usize>,
        }
        impl PartialEq for Token<'_> {
            fn eq(&self, other: &Self) -> bool {
                self.probes.set(self.probes.get() + 1);
                self.value == other.value
            }
        }
        impl Eq for Token<'_> {}
        let probes = Cell::new(0usize);
        let mut raw = vec!['a'; 303];
        raw[138] = 'x';
        raw[139] = 'y';
        let mut longer = raw.clone();
        longer.insert(139, 'x');
        let old = raw
            .iter()
            .map(|&value| Token {
                value,
                probes: &probes,
            })
            .collect::<Vec<_>>();
        let new = longer
            .iter()
            .map(|&value| Token {
                value,
                probes: &probes,
            })
            .collect::<Vec<_>>();
        let mut remaining = 13928;
        assert!(
            check(&old, &new, 100, &mut remaining)
                .expect("negative ambiguity witness")
                .is_none()
        );
        assert_eq!(
            probes.get(),
            141,
            "tail is not read after the negative boundary"
        );
        assert_eq!(
            13928 - remaining,
            128 + 3 * 141,
            "all executed probes are paid; none refunded"
        );
    }
}
