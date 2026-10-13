//! Two complete optimal matching paths from one paid row-ambiguity contour.

use super::{MAX_EXACT_MEMORY_BYTES, RetainedSuffix, RowAmbiguity};
use crate::Result;

type OptimalPairs = Vec<(usize, usize)>;

impl<T: Eq> RetainedSuffix<'_, T> {
    /// Builds only a negative-proof candidate, never a unique edit witness.
    /// Original full semantic admission remains protected while the optional
    /// immutable-binding, row-repair and path construction work is prepaid.
    pub(in crate::diff::assessment) fn optimal_pair_certificate(
        &self,
        old: &[T],
        new: &[T],
        remaining: &mut usize,
        fallback: usize,
    ) -> Result<Option<[OptimalPairs; 2]>> {
        let Some(witness) = self.row_ambiguity else {
            return Ok(None);
        };
        let n = old.len();
        let m = new.len();
        let Some(length) = n.checked_add(m) else {
            return Ok(None);
        };
        let i = self.next_old;
        let j = self.next_new;
        let RowAmbiguity {
            first_new,
            first_prefix_rank,
            lcs_length,
        } = witness;
        if i == 0 || i >= n || j >= m || first_new <= j || first_new >= m {
            return Ok(None);
        }
        let width = first_new - j + 1;
        let Some(work) = length
            .checked_mul(24)
            .and_then(|v| width.checked_mul(3).and_then(|w| v.checked_add(w)))
            .and_then(|v| v.checked_add(128))
        else {
            return Ok(None);
        };
        let Some(required) = fallback.checked_add(work) else {
            return Ok(None);
        };
        let Some(words) = n
            .min(m)
            .checked_mul(4)
            .and_then(|v| width.checked_add(v))
            .and_then(|v| v.checked_add(self.table.capacity()))
        else {
            return Ok(None);
        };
        if *remaining < required
            || words
                .checked_mul(std::mem::size_of::<usize>())
                .is_none_or(|b| b > MAX_EXACT_MEMORY_BYTES)
        {
            return Ok(None);
        }
        // This covers two paths of at most n+m decisions (four rank/token
        // inspections each), their pair validation/reversal, both backing
        // arrays, and the repaired row's initialization and cell recurrence.
        if !super::super::charge(remaining, work) {
            return Ok(None);
        }
        if !std::ptr::eq(old, self.old) || !std::ptr::eq(new, self.new) {
            return Ok(None);
        }
        let columns = m + 1;
        if self.table.len() != (n + 1).checked_mul(columns).unwrap_or(0) {
            return Ok(None);
        }
        let mut row = Vec::new();
        row.try_reserve_exact(width)
            .map_err(|_| crate::Error::Unresolved("ambiguity prefix row allocation".to_owned()))?;
        if row.capacity() != width {
            return Ok(None);
        }
        row.push(self.table[i * columns + j]);
        for y in j + 1..=first_new {
            let value = if old[i - 1] == new[y - 1] {
                self.table[(i - 1) * columns + y - 1] + 1
            } else {
                self.table[(i - 1) * columns + y].max(*row.last().expect("row boundary"))
            };
            row.push(value);
        }
        if row[width - 1] != first_prefix_rank
            || row[0] + 1 + self.table[(i + 1) * columns + j + 1] != lcs_length
            || first_prefix_rank + 1 + self.table[(i + 1) * columns + first_new + 1] != lcs_length
            || old[i] != new[j]
            || old[i] != new[first_new]
        {
            return Ok(None);
        }
        let prefix = |x: usize, y: usize| {
            if x == i && y >= j && y <= first_new {
                row[y - j]
            } else {
                self.table[x * columns + y]
            }
        };
        let mut paths = [Vec::new(), Vec::new()];
        for (pairs, anchor_new) in paths.iter_mut().zip([first_new, j]) {
            pairs.try_reserve_exact(n.min(m)).map_err(|_| {
                crate::Error::Unresolved("ambiguity optimal pairs allocation".to_owned())
            })?;
            if pairs.capacity() != n.min(m) {
                return Ok(None);
            }
            let (mut x, mut y) = (i, anchor_new);
            while x > 0 && y > 0 {
                let rank = prefix(x, y);
                if old[x - 1] == new[y - 1] && prefix(x - 1, y - 1) + 1 == rank {
                    pairs.push((x - 1, y - 1));
                    x -= 1;
                    y -= 1;
                } else if prefix(x - 1, y) == rank {
                    x -= 1;
                } else if prefix(x, y - 1) == rank {
                    y -= 1;
                } else {
                    return Ok(None);
                }
            }
            pairs.reverse();
            pairs.push((i, anchor_new));
            (x, y) = (i + 1, anchor_new + 1);
            while x < n && y < m {
                let rank = self.table[x * columns + y];
                if old[x] == new[y] && self.table[(x + 1) * columns + y + 1] + 1 == rank {
                    pairs.push((x, y));
                    x += 1;
                    y += 1;
                } else if self.table[(x + 1) * columns + y] == rank {
                    x += 1;
                } else if self.table[x * columns + y + 1] == rank {
                    y += 1;
                } else {
                    return Ok(None);
                }
            }
            if pairs.len() != lcs_length
                || !pairs.iter().all(|&(x, y)| old[x] == new[y])
                || !pairs.windows(2).all(|p| p[0].0 < p[1].0 && p[0].1 < p[1].1)
            {
                return Ok(None);
            }
        }
        Ok(Some(paths))
    }
}
