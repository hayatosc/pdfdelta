//! Complete mandatory LCS pairs from the graph of all literal matching edges.
//!
//! Forward ranks are exact longest increasing chains of matching coordinates.
//! A backward strict-dominance walk retains exactly the edges belonging to a
//! maximum chain. One eligible edge at a rank is mandatory in every maximum
//! matching, the same condition as the full prefix/suffix matrix.
//!
//! Work includes bucket initialization, token and candidate visits, every
//! binary rank comparison, backward edge visits and exact retained-pair moves.
//! The complete remaining-work bound is checked before the graph is built;
//! only actual visits are spent. All simultaneously live requested buffers,
//! bucket tables and fixed bookkeeping remain under the caller's memory cap.

use super::{
    MAX_SEMANTIC_MEMORY_BYTES, MandatoryMatchAnalysis, cells_limit_error, charge,
    mandatory_match_analysis_with_memory_limit, memory_limit_error,
};
use crate::{Error, Result, normalize::ComparableToken};

const NONE: usize = usize::MAX;
const BUCKETS: usize = 256;

struct Edge {
    old: usize,
    new: usize,
    next: usize,
}

/// Keeps the ordinary matrix when its original work allowance is available.
/// Otherwise attempts every scalar matching edge within the same shared work
/// and memory limits. Opaque, partial, unpayable or incomplete analyses never
/// return a certificate. This establishes no physical source or ownership.
///
/// # Errors
/// Returns the original resource errors on overflow or live-capacity excess,
/// and an unresolved error when a fallible allocation cannot be fulfilled.
pub(in crate::diff::assessment) fn mandatory_literal_analysis_with_memory_limit(
    old: &[&ComparableToken],
    new: &[&ComparableToken],
    remaining: &mut usize,
    memory_limit: usize,
) -> Result<Option<MandatoryMatchAnalysis>> {
    let dense_work = old
        .len()
        .checked_mul(new.len())
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| n.checked_add(old.len().checked_mul(2)?))
        .and_then(|n| n.checked_add(new.len().checked_mul(2)?))
        .and_then(|n| n.checked_add(1))
        .ok_or_else(cells_limit_error)?;
    if dense_work <= *remaining {
        return mandatory_match_analysis_with_memory_limit(old, new, remaining, memory_limit);
    }
    sparse(old, new, remaining, memory_limit)
}

fn reserve<T>(n: usize) -> Result<Vec<T>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(n)
        .map_err(|_| Error::Unresolved("sparse mandatory-match allocation failed".to_owned()))?;
    if result.capacity() != n {
        return Err(memory_limit_error());
    }
    Ok(result)
}

fn sparse(
    old: &[&ComparableToken],
    new: &[&ComparableToken],
    remaining: &mut usize,
    memory_limit: usize,
) -> Result<Option<MandatoryMatchAnalysis>> {
    let rank_limit = old.len().min(new.len());
    let limit = memory_limit.min(MAX_SEMANTIC_MEMORY_BYTES);
    let fixed = (2 * BUCKETS)
        .checked_mul(std::mem::size_of::<usize>())
        .and_then(|n| n.checked_add(6 * std::mem::size_of::<Vec<usize>>()))
        .and_then(|n| n.checked_add(32 * std::mem::size_of::<usize>()))
        .ok_or_else(memory_limit_error)?;
    let base_bytes = new
        .len()
        .checked_mul(std::mem::size_of::<usize>())
        .and_then(|n| n.checked_add(rank_limit.checked_mul(2 * std::mem::size_of::<usize>())?))
        .and_then(|n| n.checked_add(rank_limit.checked_mul(std::mem::size_of::<(usize, usize)>())?))
        .and_then(|n| n.checked_add(fixed))
        .ok_or_else(memory_limit_error)?;
    if base_bytes > limit {
        return Err(memory_limit_error());
    }
    let prepass = (2 * BUCKETS)
        .checked_add(old.len())
        .and_then(|n| n.checked_add(new.len()))
        .and_then(|n| n.checked_add(32))
        .ok_or_else(cells_limit_error)?;
    if !charge(remaining, prepass) {
        return Ok(None);
    }
    let mut heads = [NONE; BUCKETS];
    let mut counts = [0usize; BUCKETS];
    let mut next = reserve(new.len())?;
    for (j, token) in new.iter().enumerate() {
        let Some(scalar) = token.as_scalar() else {
            return Ok(None);
        };
        let bucket = scalar as usize % BUCKETS;
        next.push(heads[bucket]);
        heads[bucket] = j;
        counts[bucket] += 1;
    }
    let mut candidates = 0usize;
    for token in old {
        let Some(scalar) = token.as_scalar() else {
            return Ok(None);
        };
        candidates = candidates
            .checked_add(counts[scalar as usize % BUCKETS])
            .ok_or_else(cells_limit_error)?;
    }
    let bytes = candidates
        .checked_mul(std::mem::size_of::<Edge>())
        .and_then(|n| n.checked_add(base_bytes))
        .ok_or_else(memory_limit_error)?;
    if bytes > limit {
        return Err(memory_limit_error());
    }
    let comparisons = rank_limit.checked_ilog2().map_or(0, |n| n as usize + 1);
    let upper_work = candidates
        .checked_mul(comparisons.checked_add(3).ok_or_else(cells_limit_error)?)
        .and_then(|n| n.checked_add(old.len()))
        .and_then(|n| n.checked_add(rank_limit.checked_mul(3)?))
        .ok_or_else(cells_limit_error)?;
    if upper_work > *remaining {
        return Ok(None);
    }
    let mut edges: Vec<Edge> = reserve(candidates)?;
    let mut tails = reserve(rank_limit)?;
    let mut ranks = reserve(rank_limit)?;
    let mut output = reserve(rank_limit)?;
    // Bucket collisions retain extra comparisons, never a false match. New
    // positions are visited descending, so a row cannot match itself twice.
    for (i, token) in old.iter().enumerate() {
        if !charge(remaining, 1) {
            return Ok(None);
        }
        let scalar = token
            .as_scalar()
            .expect("the immutable input was validated");
        let mut j = heads[scalar as usize % BUCKETS];
        while j != NONE {
            if !charge(remaining, 1) {
                return Ok(None);
            }
            if *token == new[j] {
                let (mut lo, mut hi) = (0, tails.len());
                while lo < hi {
                    if !charge(remaining, 1) {
                        return Ok(None);
                    }
                    let mid = lo + (hi - lo) / 2;
                    if tails[mid] < j {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                if lo == tails.len() {
                    tails.push(j);
                    ranks.push(NONE);
                } else {
                    tails[lo] = j;
                }
                edges.push(Edge {
                    old: i + 1,
                    new: j + 1,
                    next: ranks[lo],
                });
                ranks[lo] = edges.len() - 1;
            }
            j = next[j];
        }
    }
    // Rank lists are descending in old position. Each eligible next-rank
    // edge is visited at most once; strict dominance reconstructs all maximum
    // paths without choosing an edit order or dropping competing matchings.
    for rank in (0..ranks.len()).rev() {
        if !charge(remaining, 1) {
            return Ok(None);
        }
        let mut current = ranks[rank];
        let mut successor = ranks.get(rank + 1).copied().unwrap_or(NONE);
        let (mut maximum_new, mut kept, mut tail) = (0, NONE, NONE);
        let (mut count, mut unique) = (0usize, (0, 0));
        while current != NONE {
            if !charge(remaining, 1) {
                return Ok(None);
            }
            let following = edges[current].next;
            while successor != NONE && edges[successor].old > edges[current].old {
                if !charge(remaining, 1) {
                    return Ok(None);
                }
                maximum_new = maximum_new.max(edges[successor].new);
                successor = edges[successor].next;
            }
            if rank + 1 == ranks.len() || maximum_new > edges[current].new {
                count += 1;
                unique = (edges[current].old, edges[current].new);
                if kept == NONE {
                    kept = current;
                } else {
                    edges[tail].next = current;
                }
                tail = current;
                edges[current].next = NONE;
            }
            current = following;
        }
        ranks[rank] = kept;
        if count == 1 {
            output.push(unique);
        }
    }
    // Release scratch before moving pairs to an exact-size retained array.
    // The base allowance already covers two rank-sized pair arrays at once.
    drop(next);
    drop(edges);
    drop(tails);
    drop(ranks);
    let mut exact = reserve(output.len())?;
    for pair in output.into_iter().rev() {
        if !charge(remaining, 2) {
            return Ok(None);
        }
        exact.push(pair);
    }
    Ok(Some(MandatoryMatchAnalysis {
        unique_pairs: exact,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(word: &[u8], alphabet: [char; 2]) -> Vec<ComparableToken> {
        word.iter()
            .map(|b| ComparableToken::Scalar(alphabet[usize::from(*b)]))
            .collect()
    }

    #[test]
    fn sparse_complete_paths_match_the_full_matrix_including_bucket_collisions() {
        let words = super::super::super::all_words(4);
        for alphabet in [['a', 'b'], ['a', '\u{161}'], ['日', '😀']] {
            for a in &words {
                for b in &words {
                    let a = tokens(a, alphabet);
                    let b = tokens(b, alphabet);
                    let a = a.iter().collect::<Vec<_>>();
                    let b = b.iter().collect::<Vec<_>>();
                    let expected = mandatory_match_analysis_with_memory_limit(
                        &a,
                        &b,
                        &mut 1_000_000,
                        MAX_SEMANTIC_MEMORY_BYTES,
                    )
                    .expect("matrix")
                    .expect("complete");
                    let actual = sparse(&a, &b, &mut 1_000_000, MAX_SEMANTIC_MEMORY_BYTES)
                        .expect("sparse")
                        .expect("complete");
                    assert_eq!(actual.pairs(), expected.pairs(), "a={a:?}, b={b:?}");
                    assert_eq!(
                        actual.retained_heap_bytes(),
                        Some(std::mem::size_of_val(actual.pairs()))
                    );
                }
            }
        }
    }

    #[test]
    fn sparse_literal_route_preserves_dense_admission_and_refuses_partial_results() {
        let a = (0..70)
            .map(|i| ComparableToken::Scalar(char::from_u32(0x400 + i).expect("scalar")))
            .collect::<Vec<_>>();
        let mut b = a.clone();
        b[35] = ComparableToken::Scalar('X');
        let a = a.iter().collect::<Vec<_>>();
        let b = b.iter().collect::<Vec<_>>();
        let mut dense = 1_000_000;
        let expected = mandatory_match_analysis_with_memory_limit(
            &a,
            &b,
            &mut dense,
            MAX_SEMANTIC_MEMORY_BYTES,
        )
        .expect("matrix")
        .expect("complete");
        let required = 1_000_000 - dense;
        let mut exact = required;
        let original = mandatory_literal_analysis_with_memory_limit(
            &a,
            &b,
            &mut exact,
            MAX_SEMANTIC_MEMORY_BYTES,
        )
        .expect("original route")
        .expect("complete");
        assert_eq!(exact, 0);
        assert_eq!(original.pairs(), expected.pairs());
        let mut small = 4_000;
        let result = mandatory_literal_analysis_with_memory_limit(
            &a,
            &b,
            &mut small,
            MAX_SEMANTIC_MEMORY_BYTES,
        )
        .expect("sparse route")
        .expect("complete");
        assert_eq!(result.pairs(), expected.pairs());
        let used = 4_000 - small;
        assert!(used < required / 4, "actual sparse work is paid");
        for budget in [0, 511, used - 1] {
            assert!(
                sparse(&a, &b, &mut { budget }, MAX_SEMANTIC_MEMORY_BYTES)
                    .expect("resource refusal")
                    .is_none()
            );
        }
        assert!(sparse(&a, &b, &mut 1_000_000, 1024).is_err());
    }
}
