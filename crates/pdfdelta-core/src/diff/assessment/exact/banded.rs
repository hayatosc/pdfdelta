//! Exact shortest-path matching-edge union inside a certified diagonal band.
//!
//! Equal-length inputs with `k` literal positional mismatches have an explicit
//! insertion/deletion path of length at most `2k`. Every optimal path therefore
//! has at most `k` deletions and `k` insertions and lies in `|i-j| <= k`.
//! Complete forward and reverse distances in that band enumerate every equal
//! diagonal on any optimal path. The union has exactly LCS-length edges iff
//! every optimal alignment has the same ordered matching-pair sequence.
//! No source, ownership, context or publication verdict is made here.

use super::MAX_EXACT_MEMORY_BYTES;
use crate::{
    Result,
    diff::{AtomicEdit, myers},
};

struct Band {
    radius: usize,
    width: usize,
    length: usize,
    infinity: usize,
    values: Vec<usize>,
}

impl Band {
    fn index(&self, i: usize, j: usize) -> Option<usize> {
        if i > self.length || j > self.length || i.abs_diff(j) > self.radius {
            return None;
        }
        Some(i * self.width + (j + self.radius - i))
    }

    fn get(&self, i: usize, j: usize) -> usize {
        self.index(i, j)
            .map_or(self.infinity, |index| self.values[index])
    }

    fn set(&mut self, i: usize, j: usize, value: usize) {
        let index = self.index(i, j).expect("admitted band coordinate");
        self.values[index] = value;
    }

    fn range(&self, i: usize) -> std::ops::RangeInclusive<usize> {
        i.saturating_sub(self.radius)..=self.length.min(i + self.radius)
    }
}

/// Dense and band DP use the same one-unit-per-cell contract, including their
/// rank/distance recurrence and literal comparison. The band additionally pays
/// three per Hamming probe, 128 for admission/allocation/release metadata, one
/// per allocated word for each of two initialized tables, and one per matching
/// edge eligibility check. Script traversal and output words cost the existing
/// `8*(old_len+new_len)+4*distance` reconstruction fee. All actual allocation
/// capacities and simultaneous table-plus-output bytes obey the original cap.
/// A dimension-derived affordable radius admits the entire attempt before any
/// scan; exceeding it returns no claim with all executed work still paid.
#[cfg(test)]
fn check<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    check_reserved(old, new, max_distance, remaining, 0, usize::MAX)
}

/// All executed charges go directly to the original shared allowance.
/// `reserve` is an admission constraint, not a separate allowance or refund;
/// it protects the ordinary dense fallback and its reconstruction. The probe
/// ceiling also requires a complete affordable band to cost less than the
/// original forward pass. Refused probes never start a token scan.
pub(super) fn check_reserved<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
    reserve: usize,
    ceiling: usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    check_reserved_radius(
        old,
        new,
        max_distance,
        remaining,
        [reserve, ceiling, usize::MAX],
        None,
    )
}

/// Limits an ample-budget probe to a single positional substitution. A second
/// mismatch stops the paid scout before any DP allocation. This is still a
/// complete optimal-edge-union proof, not a chosen diagonal alignment.
#[cfg(test)]
fn check_single_substitution_reserved<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
    reserve: usize,
    ceiling: usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    check_reserved_radius(
        old,
        new,
        max_distance,
        remaining,
        [reserve, ceiling, 1],
        None,
    )
}

/// The original scout paid every literal through the first mismatch. The
/// receipt carries both immutable slices and cannot transfer to another scope.
/// Only its uninspected tail is probed; complete band and physical proofs,
/// original admissions, and all earlier charges remain necessary.
pub(super) fn check_single_substitution_after_prefix_reserved<T: Eq>(
    paid: super::PaidMismatch<'_, T>,
    max_distance: usize,
    remaining: &mut usize,
    reserve: usize,
    ceiling: usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    check_reserved_radius(
        paid.old,
        paid.new,
        max_distance,
        remaining,
        [reserve, ceiling, 1],
        Some(paid.index),
    )
}

fn check_reserved_radius<T: Eq>(
    old: &[T],
    new: &[T],
    max_distance: usize,
    remaining: &mut usize,
    limits: [usize; 3],
    paid_mismatch: Option<usize>,
) -> Result<Option<Vec<AtomicEdit>>> {
    let [reserve, ceiling, radius_ceiling] = limits;
    let n = old.len();
    if n == 0 || n != new.len() {
        return Ok(None);
    }
    let Some(length) = n.checked_mul(2) else {
        return Ok(None);
    };
    let Some(rows) = n.checked_add(1) else {
        return Ok(None);
    };
    // 5 table-width units: two initializations, forward, reverse, edge union.
    let Some(base) = n
        .checked_mul(19)
        .and_then(|x| x.checked_add(rows.checked_mul(5)?))
        .and_then(|x| x.checked_add(128))
    else {
        return Ok(None);
    };
    let Some(per_radius) = rows.checked_mul(10).and_then(|x| x.checked_add(8)) else {
        return Ok(None);
    };
    let available = remaining.saturating_sub(reserve).min(ceiling);
    let affordable = available.saturating_sub(base) / per_radius;
    if affordable == 0 || !super::super::charge(remaining, 128) {
        return Ok(None);
    }
    let (bound, _) = myers::reconstruction_admission(n, n, max_distance)?;
    let radius_limit = affordable.min(n).min(bound / 2).min(radius_ceiling);
    if radius_limit == 0 {
        return Ok(None);
    }
    let (mut radius, start) = paid_mismatch.map_or((0, 0), |index| (1, index + 1));
    for (a, b) in old[start..].iter().zip(&new[start..]) {
        if !super::super::charge(remaining, 3) {
            return Ok(None);
        }
        radius += usize::from(a != b);
        if radius > radius_limit {
            return Ok(None);
        }
    }
    if radius == 0 {
        return Ok(None);
    }
    let Some(width) = radius.checked_mul(2).and_then(|x| x.checked_add(1)) else {
        return Ok(None);
    };
    let Some(cells) = rows.checked_mul(width) else {
        return Ok(None);
    };
    let bytes = cells
        .checked_mul(2 * size_of::<usize>())
        .and_then(|x| x.checked_add(radius.checked_mul(2 * size_of::<AtomicEdit>())?))
        .and_then(|x| x.checked_add(2 * size_of::<Band>() + size_of::<Vec<AtomicEdit>>() + 256));
    if bytes.is_none_or(|bytes| bytes > MAX_EXACT_MEMORY_BYTES)
        || !super::super::charge(remaining, cells * 2)
    {
        return Ok(None);
    }
    let Some(infinity) = length.checked_add(1) else {
        return Ok(None);
    };
    let allocate = || {
        let mut values = Vec::new();
        if values.try_reserve_exact(cells).is_err() || values.capacity() != cells {
            return None;
        }
        values.resize(cells, infinity);
        Some(Band {
            radius,
            width,
            length: n,
            infinity,
            values,
        })
    };
    let (Some(mut forward), Some(mut reverse)) = (allocate(), allocate()) else {
        return Ok(None);
    };
    for i in 0..=n {
        for j in forward.range(i) {
            if !super::super::charge(remaining, 1) {
                return Ok(None);
            }
            let mut value = if i == 0 && j == 0 { 0 } else { infinity };
            if i > 0 {
                value = value.min(forward.get(i - 1, j).saturating_add(1));
            }
            if j > 0 {
                value = value.min(forward.get(i, j - 1).saturating_add(1));
            }
            if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
                value = value.min(forward.get(i - 1, j - 1));
            }
            forward.set(i, j, value);
        }
    }
    let distance = forward.get(n, n);
    if distance > bound || distance > radius * 2 {
        return Ok(None);
    }
    for i in (0..=n).rev() {
        for j in reverse.range(i).rev() {
            if !super::super::charge(remaining, 1) {
                return Ok(None);
            }
            let mut value = if i == n && j == n { 0 } else { infinity };
            if i < n {
                value = value.min(reverse.get(i + 1, j).saturating_add(1));
            }
            if j < n {
                value = value.min(reverse.get(i, j + 1).saturating_add(1));
            }
            if i < n && j < n && old[i] == new[j] {
                value = value.min(reverse.get(i + 1, j + 1));
            }
            reverse.set(i, j, value);
        }
    }
    let lcs = (length - distance) / 2;
    let mut matching_edges = 0;
    for (i, old_token) in old.iter().enumerate() {
        for j in forward.range(i) {
            if j == n {
                continue;
            }
            if !super::super::charge(remaining, 1) {
                return Ok(None);
            }
            if *old_token == new[j] && forward.get(i, j) + reverse.get(i + 1, j + 1) == distance {
                matching_edges += 1;
                if matching_edges > lcs {
                    return Ok(None);
                }
            }
        }
    }
    if matching_edges != lcs || !super::super::charge(remaining, 8 * length + 4 * distance) {
        return Ok(None);
    }
    let mut edits = Vec::new();
    if edits.try_reserve_exact(distance).is_err() || edits.capacity() != distance {
        return Ok(None);
    }
    let (mut i, mut j, mut start_i, mut start_j) = (0, 0, 0, 0);
    while i < n && j < n {
        let value = reverse.get(i, j);
        if old[i] == new[j] && reverse.get(i + 1, j + 1) == value {
            if start_i < i {
                edits.push(AtomicEdit {
                    old: start_i..i,
                    new: start_j..start_j,
                });
            }
            if start_j < j {
                edits.push(AtomicEdit {
                    old: i..i,
                    new: start_j..j,
                });
            }
            i += 1;
            j += 1;
            start_i = i;
            start_j = j;
        } else if reverse.get(i, j + 1).saturating_add(1) == value {
            j += 1;
        } else if reverse.get(i + 1, j).saturating_add(1) == value {
            i += 1;
        } else {
            return Ok(None);
        }
    }
    if start_i < n {
        edits.push(AtomicEdit {
            old: start_i..n,
            new: start_j..start_j,
        });
    }
    if start_j < n {
        edits.push(AtomicEdit {
            old: n..n,
            new: start_j..n,
        });
    }
    Ok(Some(edits))
}

#[cfg(test)]
mod tests {
    use super::super::{ExactUniqueness, ReconstructedScript, check_retaining_for_reconstruction};
    use super::check;

    #[test]
    fn single_substitution_probe_matches_complete_dense_oracle() {
        for n in 1..=4 {
            let words: Vec<Vec<u16>> = (0..1usize << n)
                .map(|bits| {
                    let mut word = (0..n)
                        .map(|i| u16::from(bits & (1 << i) != 0))
                        .collect::<Vec<_>>();
                    word.extend(100..164);
                    word
                })
                .collect();
            for old in &words {
                for new in &words {
                    let mismatch = old.iter().zip(new).filter(|(a, b)| a != b).count();
                    let actual = super::check_single_substitution_reserved(
                        old,
                        new,
                        200,
                        &mut 1_000_000,
                        0,
                        usize::MAX,
                    )
                    .expect("bounded complete probe");
                    let oracle = check_retaining_for_reconstruction(old, new, &mut 1_000_000)
                        .expect("complete dense oracle");
                    if mismatch == 1 && oracle.outcome == ExactUniqueness::Unique {
                        let ReconstructedScript::Ready(expected) = oracle
                            .suffix
                            .expect("dense unique table")
                            .reconstruct_unique(old, new, 200, &mut 1_000_000)
                            .expect("dense script")
                        else {
                            panic!("dense admitted");
                        };
                        assert_eq!(actual, Some(expected), "{old:?} -> {new:?}");
                    } else {
                        assert!(actual.is_none(), "{old:?} -> {new:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn band_union_and_canonical_script_match_dense_oracle() {
        for n in 1..=5 {
            let words: Vec<Vec<char>> = (0..1usize << n)
                .map(|bits| {
                    (0..n)
                        .map(|i| {
                            if bits & (1 << i) == 0 {
                                '日'
                            } else {
                                '\u{10ffff}'
                            }
                        })
                        .collect()
                })
                .collect();
            for old in &words {
                for new in &words {
                    let result = check(old, new, 100, &mut 1_000_000).expect("band");
                    let dense = check_retaining_for_reconstruction(old, new, &mut 1_000_000)
                        .expect("dense");
                    if old != new && dense.outcome == ExactUniqueness::Unique {
                        let ReconstructedScript::Ready(expected) = dense
                            .suffix
                            .expect("unique")
                            .reconstruct_unique(old, new, 100, &mut 1_000_000)
                            .expect("script")
                        else {
                            panic!("dense admission");
                        };
                        assert_eq!(result, Some(expected), "{old:?} -> {new:?}");
                    } else {
                        assert!(result.is_none(), "{old:?} -> {new:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn budget_distance_and_negative_attempts_keep_actual_charges() {
        let old = ['a', 'b', 'c'];
        let new = ['a', 'x', 'c'];
        let mut work = 10_000;
        assert!(check(&old, &new, 100, &mut work).expect("unique").is_some());
        assert!(work < 10_000);
        let mut refused = 1;
        assert!(
            check(&old, &new, 100, &mut refused)
                .expect("refused")
                .is_none()
        );
        assert_eq!(refused, 1);
        let mut distance = 10_000;
        assert!(
            check(&old, &new, 1, &mut distance)
                .expect("distance")
                .is_none()
        );
        assert_eq!(distance, 10_000 - 128);
        let mut negative = 10_000;
        assert!(
            check(&['a', 'b'], &['b', 'a'], 100, &mut negative)
                .expect("ambiguous")
                .is_none()
        );
        assert!(negative < 10_000);
        let mut unequal = 10_000;
        assert!(
            check(&old, &new[..2], 100, &mut unequal)
                .expect("outside band")
                .is_none()
        );
        assert_eq!(unequal, 10_000);
    }

    #[test]
    fn literal_probes_and_initialized_capacity_are_paid_before_use() {
        use std::cell::Cell;
        struct Token<'a>(char, &'a Cell<usize>);
        impl PartialEq for Token<'_> {
            fn eq(&self, other: &Self) -> bool {
                self.1.set(self.1.get() + 1);
                self.0 == other.0
            }
        }
        impl Eq for Token<'_> {}
        let probes = Cell::new(0);
        let old = ['a', 'b', 'c'].map(|c| Token(c, &probes));
        let new = ['a', 'x', 'c'].map(|c| Token(c, &probes));
        let mut refused = 252;
        assert!(
            check(&old, &new, 100, &mut refused)
                .expect("preflight")
                .is_none()
        );
        assert_eq!((refused, probes.get()), (252, 0));
        let mut admitted = 253;
        assert!(
            check(&old, &new, 100, &mut admitted)
                .expect("paid")
                .is_some()
        );
        // Admission, Hamming, both complete padded initializations, all real
        // boundary/interior cells, matching-edge union and retained script.
        let paid = 128 + 9 + 24 + 10 + 10 + 7 + 56;
        assert_eq!(admitted, 253 - paid);
        assert_eq!(probes.get(), 28);
    }
}
