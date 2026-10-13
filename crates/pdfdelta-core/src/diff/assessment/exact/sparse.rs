//! Exact scalar matching graph with one retained parent word per equal pair.
//!
//! This is a literal alignment proof only. Physical source, correspondence,
//! ownership, context and output validation remain the caller's responsibility.

use super::MAX_EXACT_MEMORY_BYTES;
use crate::{
    Result,
    diff::{AtomicEdit, myers},
    normalize::ComparableToken,
};

// Zero denotes the one empty chain; MANY denotes at least two distinct
// maximum matching-pair sequences. Other IDs name an individual ending edge.
// A Fenwick prefix partitions endpoint positions into disjoint sets, so equal
// IDs are repeated references to one chain, not independent alternatives.
const MANY: usize = usize::MAX;
type State = (usize, usize);

fn merge(a: State, b: State) -> State {
    if a.0 > b.0 {
        a
    } else if b.0 > a.0 {
        b
    } else if a.0 == 0 {
        (0, 0)
    } else if a.1 == b.1 {
        a
    } else {
        (a.0, MANY)
    }
}

fn reserved<T>(length: usize) -> Option<Vec<T>> {
    let mut result = Vec::new();
    result.try_reserve_exact(length).ok()?;
    (result.capacity() == length).then_some(result)
}

/// Completes uniqueness and canonical reconstruction within one shared budget.
/// The dense caller has already refused its full matrix. No failed planning,
/// allocation or search charge is refunded, and no partial graph is evidence.
/// Only scalars are supported: unmapped glyph identity keeps the original path.
///
/// Work units follow the exact checker's cell/state convention. Planning pays
/// input inspection, all radix initialization/transfers, retained index words,
/// binary search probes, row metadata and traversal-count construction. Search
/// prepays two state words per Fenwick visit, five words/operations per edge
/// (literal comparison, rank, proposal, parent write and parent retention), all
/// initialized/retained tree words, and metadata. Reconstruction prepays every
/// row lookup, retained match, source-pair check, output capacity and traversal.
/// All vector capacities must match the prepaid size; live bytes include every
/// index, row, tree, parent, match and output allocation simultaneously.
pub(in super::super) fn check(
    old: &[ComparableToken],
    new: &[ComparableToken],
    max_distance: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<AtomicEdit>>> {
    let (n, m) = (old.len(), new.len());
    if n == 0 || m == 0 {
        return Ok(None);
    }
    let Some(length) = n.checked_add(m) else {
        return Ok(None);
    };
    let lookup = (m + 1).checked_ilog2().unwrap_or(0) as usize + 2;
    let row_lookup = (n + 1).checked_ilog2().unwrap_or(0) as usize + 2;
    // Three stable byte radix passes. Unicode scalar values fit in 21 bits.
    // Each pass counts a token, transfers two words and advances a cursor.
    // Eight extra words/token initialize and retain both exact-capacity buffers.
    let plan = m
        .checked_mul(20)
        .and_then(|x| x.checked_add(9 * 256 + 128 + length))
        .and_then(|x| x.checked_add(n.checked_mul(2 * lookup + 6)?))
        .and_then(|x| x.checked_add(m.checked_mul(lookup + 4)?));
    let Some(plan) = plan else {
        return Ok(None);
    };
    if plan > *remaining || !super::super::charge(remaining, plan) {
        return Ok(None);
    }
    let (distance_bound, _) = myers::reconstruction_admission(n, m, max_distance)?;
    if old.iter().chain(new).any(|token| !token.is_scalar()) {
        return Ok(None);
    }
    let Some(index_bytes) = m
        .checked_mul(2 * size_of::<(u32, usize)>())
        .and_then(|x| x.checked_add(n.checked_mul(size_of::<(usize, usize, usize)>())?))
        .and_then(|x| {
            x.checked_add((m + 1) * size_of::<usize>() + 3 * 256 * size_of::<usize>() + 512)
        })
    else {
        return Ok(None);
    };
    if index_bytes > MAX_EXACT_MEMORY_BYTES {
        return Ok(None);
    }
    let Some(mut positions) = reserved::<(u32, usize)>(m) else {
        return Ok(None);
    };
    let Some(mut scratch) = reserved::<(u32, usize)>(m) else {
        return Ok(None);
    };
    for (j, token) in new.iter().enumerate() {
        let ComparableToken::Scalar(value) = token else {
            return Ok(None);
        };
        positions.push((*value as u32, j));
    }
    scratch.resize(m, (0, 0));
    for shift in [0, 8, 16] {
        let mut counts = [0usize; 256];
        for &(value, _) in &positions {
            counts[((value >> shift) & 255) as usize] += 1;
        }
        let mut cursor = 0;
        for count in &mut counts {
            let length = *count;
            *count = cursor;
            cursor += length;
        }
        for &entry in &positions {
            let bucket = ((entry.0 >> shift) & 255) as usize;
            scratch[counts[bucket]] = entry;
            counts[bucket] += 1;
        }
        std::mem::swap(&mut positions, &mut scratch);
    }
    let Some(mut prefix_visits) = reserved::<usize>(m + 1) else {
        return Ok(None);
    };
    prefix_visits.push(0usize);
    for &(_, j) in &positions {
        let mut count = j.count_ones() as usize;
        let mut x = j + 1;
        while x <= m {
            count += 1;
            x += x.isolate_lowest_one();
        }
        let Some(total) = prefix_visits.last().and_then(|v| v.checked_add(count)) else {
            return Ok(None);
        };
        prefix_visits.push(total);
    }
    let Some(mut rows) = reserved::<(usize, usize, usize)>(n) else {
        return Ok(None);
    };
    let (mut edges, mut visits) = (0usize, m.count_ones() as usize);
    for token in old {
        let ComparableToken::Scalar(value) = token else {
            return Ok(None);
        };
        let value = *value as u32;
        let start = positions.partition_point(|entry| entry.0 < value);
        let end = positions.partition_point(|entry| entry.0 <= value);
        rows.push((start, end, edges));
        let Some(next) = edges.checked_add(end - start) else {
            return Ok(None);
        };
        edges = next;
        let Some(next) = visits.checked_add(prefix_visits[end] - prefix_visits[start]) else {
            return Ok(None);
        };
        visits = next;
    }
    // Complete traversal counts are known before the state search. Prefix
    // sums avoid a second enumeration of the potentially huge matching graph.
    let reconstruction = 128usize
        .checked_add(n.min(m).saturating_mul(row_lookup + 8))
        .and_then(|x| x.checked_add(length.checked_mul(4)?));
    let states = visits
        .checked_mul(2)
        .and_then(|x| x.checked_add(edges.checked_mul(5)?))
        .and_then(|x| x.checked_add((m + 1).checked_mul(4)?))
        .and_then(|x| x.checked_add(16));
    let Some(total) = states.and_then(|x| x.checked_add(reconstruction?)) else {
        return Ok(None);
    };
    let bytes = index_bytes
        .checked_add(edges.saturating_mul(size_of::<usize>()))
        .and_then(|x| x.checked_add((m + 1).checked_mul(size_of::<State>())?))
        .and_then(|x| x.checked_add(n.min(m).checked_mul(size_of::<(usize, usize)>())?))
        .and_then(|x| x.checked_add(length.checked_mul(size_of::<AtomicEdit>())?));
    if bytes.is_none_or(|x| x > MAX_EXACT_MEMORY_BYTES) || edges == MANY {
        return Ok(None);
    }
    if total > *remaining || !super::super::charge(remaining, total) {
        return Ok(None);
    }
    let Some(mut tree) = reserved::<State>(m + 1) else {
        return Ok(None);
    };
    tree.resize(m + 1, (0, 0));
    let Some(mut parents) = reserved::<usize>(edges) else {
        return Ok(None);
    };
    // Descending new positions prevent an old token from extending itself.
    // Every eligible predecessor is strictly before the new endpoint. Merge
    // preserves both maximum length and whether its exact chain is unique.
    for (row, &(start, end, _)) in rows.iter().enumerate() {
        for &(_, j) in positions[start..end].iter().rev() {
            if old[row] != new[j] {
                return Ok(None);
            }
            let mut best = (0, 0);
            let mut x = j;
            while x > 0 {
                best = merge(best, tree[x]);
                x &= x - 1;
            }
            parents.push(best.1);
            let id = if best.1 == MANY { MANY } else { parents.len() };
            let proposal = (best.0 + 1, id);
            let mut x = j + 1;
            while x <= m {
                tree[x] = merge(tree[x], proposal);
                x += x.isolate_lowest_one();
            }
        }
    }
    let mut best = (0, 0);
    let mut x = m;
    while x > 0 {
        best = merge(best, tree[x]);
        x &= x - 1;
    }
    if best.1 == MANY || length - 2 * best.0 > distance_bound {
        return Ok(None);
    }
    let Some(mut matches) = reserved::<(usize, usize)>(n.min(m)) else {
        return Ok(None);
    };
    let mut id = best.1;
    while id > 0 {
        let ordinal = id - 1;
        let row = rows.partition_point(|entry| entry.2 <= ordinal) - 1;
        let (start, end, offset) = rows[row];
        let j = positions[end - 1 - (ordinal - offset)].1;
        if ordinal - offset >= end - start || old[row] != new[j] {
            return Ok(None);
        }
        matches.push((row, j));
        id = parents[ordinal];
    }
    if matches.len() != best.0 {
        return Ok(None);
    }
    let Some(mut edits) = reserved::<AtomicEdit>(length) else {
        return Ok(None);
    };
    let (mut i, mut j) = (0, 0);
    for &(a, b) in matches.iter().rev() {
        if i < a {
            edits.push(AtomicEdit {
                old: i..a,
                new: j..j,
            });
        }
        if j < b {
            edits.push(AtomicEdit {
                old: a..a,
                new: j..b,
            });
        }
        i = a + 1;
        j = b + 1;
    }
    if i < n {
        edits.push(AtomicEdit {
            old: i..n,
            new: j..j,
        });
    }
    if j < m {
        edits.push(AtomicEdit {
            old: n..n,
            new: j..m,
        });
    }
    Ok(Some(edits))
}

#[cfg(test)]
mod tests {
    use super::super::{ExactUniqueness, ReconstructedScript, check_retaining_for_reconstruction};
    use super::check;
    use crate::normalize::ComparableToken;

    fn tokens(text: &str) -> Vec<ComparableToken> {
        text.chars().map(ComparableToken::Scalar).collect()
    }

    #[test]
    fn compact_graph_matches_dense_unique_scripts_exhaustively() {
        let mut words = Vec::new();
        for length in 1..=5 {
            for bits in 0..(1usize << length) {
                words.push(
                    (0..length)
                        .map(|i| {
                            ComparableToken::Scalar(if bits & (1 << i) == 0 { 'a' } else { 'b' })
                        })
                        .collect::<Vec<_>>(),
                );
            }
        }
        words.push(tokens("\0é日\u{10ffff}é"));
        words.push(tokens("\0é9日\u{10ffff}é"));
        for old in &words {
            for new in &words {
                let dense =
                    check_retaining_for_reconstruction(old, new, &mut 1_000_000).expect("dense");
                let sparse = check(old, new, 100, &mut 1_000_000).expect("compact");
                match dense.outcome {
                    ExactUniqueness::Unique => {
                        let expected = if let Some(suffix) = dense.suffix {
                            let ReconstructedScript::Ready(script) = suffix
                                .reconstruct_unique(old, new, 100, &mut 1_000_000)
                                .expect("dense script")
                            else {
                                panic!("dense reconstruction")
                            };
                            script
                        } else {
                            Vec::new()
                        };
                        assert_eq!(sparse.as_ref(), Some(&expected), "{old:?} -> {new:?}");
                    }
                    ExactUniqueness::Ambiguous => assert!(sparse.is_none(), "{old:?} -> {new:?}"),
                    ExactUniqueness::BudgetExceeded => panic!("independent dense allowance"),
                }
            }
        }
    }

    #[test]
    fn compact_graph_prepaid_boundary_and_distance_guards() {
        let old = tokens("a日b日c");
        let new = tokens("a日9b日c");
        let mut remaining = 1_000_000;
        let expected = check(&old, &new, 100, &mut remaining)
            .expect("complete")
            .expect("unique");
        let cost = 1_000_000 - remaining;
        let mut exact = cost;
        assert_eq!(
            check(&old, &new, 100, &mut exact).expect("exact allowance"),
            Some(expected)
        );
        assert_eq!(exact, 0);
        let mut refused = cost - 1;
        assert!(
            check(&old, &new, 100, &mut refused)
                .expect("refused")
                .is_none()
        );
        assert!(refused < cost - 1, "paid planning is not refunded");
        assert!(refused > 0, "unexecuted search is refused before payment");
        assert!(
            check(&old, &new, 0, &mut 1_000_000)
                .expect("distance refusal")
                .is_none()
        );
        assert!(
            check(&tokens("aa"), &tokens("a"), 100, &mut 1_000_000)
                .expect("two distinct maximum paths")
                .is_none()
        );
        assert!(
            check(&tokens("ab"), &tokens("ba"), 100, &mut 1_000_000)
                .expect("same rank distinct endpoints")
                .is_none()
        );
    }
}
