//! Prefer a complete sparse analysis only behind a paid conservative bound.
//!
//! The original dense allowance remains reserved even if the sparse trial
//! fails. Selection never changes the set of mandatory pairs, correspondence
//! premises, source guards, or ownership. Unfinished trials grant no evidence.

use super::{
    BUCKETS, ComparableToken, Edge, MAX_SEMANTIC_MEMORY_BYTES, MandatoryMatchAnalysis, charge,
    mandatory_match_analysis_with_memory_limit, sparse,
};
use crate::{Error, Result};

fn dense_work(n: usize, m: usize) -> Option<usize> {
    n.checked_mul(m)?
        .checked_mul(4)?
        .checked_add(n.checked_mul(2)?)?
        .checked_add(m.checked_mul(2)?)?
        .checked_add(1)
}

fn probe_work(n: usize, m: usize) -> Option<usize> {
    n.checked_add(m)?.checked_mul(2)?.checked_add(544)
}

/// Count a superset of matching edges without constructing the edge graph.
///
/// The caller prepays the complete scan. Bucket collisions only increase the
/// bound. Opaque tokens, overflow, or insufficient live memory select dense.
fn sparse_trial_bound(
    old: &[&ComparableToken],
    new: &[&ComparableToken],
    memory_limit: usize,
) -> Option<usize> {
    let mut counts = [0usize; BUCKETS];
    for token in new {
        let bucket = token.as_scalar()? as usize % BUCKETS;
        counts[bucket] = counts[bucket].checked_add(1)?;
    }
    let mut candidates = 0usize;
    for token in old {
        candidates = candidates.checked_add(counts[token.as_scalar()? as usize % BUCKETS])?;
    }
    let total = old.len().checked_add(new.len())?;
    let rank = old.len().min(new.len());
    let comparisons = rank.checked_ilog2().map_or(0, |value| value as usize + 1);
    // The existing sparse prepass, row visits, rank visits and pair moves
    // cost at most 544 + 2n + m + 3min(n,m), plus r(log-rank + 3).
    let work = candidates
        .checked_mul(comparisons.checked_add(4)?)?
        .checked_add(total.checked_mul(4)?)?
        .checked_add(1024)?;
    // Cover the existing bucket arrays, vector headers and bookkeeping, plus
    // this scout's stack array even if the compiler inlines the two calls.
    let fixed = (3 * BUCKETS + 64)
        .checked_mul(std::mem::size_of::<usize>())?
        .checked_add(6 * std::mem::size_of::<Vec<usize>>())?
        .max(8192);
    let bytes = candidates
        .checked_mul(std::mem::size_of::<Edge>())?
        .checked_add(total.checked_mul(32)?)?
        .checked_add(fixed)?;
    (bytes <= memory_limit.min(MAX_SEMANTIC_MEMORY_BYTES)).then_some(work)
}

/// Return the same complete mandatory pairs as the dense analysis.
///
/// A scout runs only when its full charge, a trial of at most half the dense
/// work, and the original dense fallback all fit. Every consumed scout/trial
/// unit is paid from the caller's shared budget. A failed trial retains the
/// original dense allowance; a successful trial retains all unmatched and
/// ambiguous alternatives through the existing sparse certificate kernel.
///
/// # Errors
/// Returns the ordinary dense resource/allocation errors or an unexpected
/// sparse error. No incomplete analysis is returned as a certificate.
pub(in crate::diff::assessment) fn mandatory_literal_analysis_with_sparse_preference(
    old: &[&ComparableToken],
    new: &[&ComparableToken],
    remaining: &mut usize,
    memory_limit: usize,
) -> Result<Option<MandatoryMatchAnalysis>> {
    let plan = dense_work(old.len(), new.len()).and_then(|dense| {
        let probe = probe_work(old.len(), new.len())?;
        let allowance = dense.checked_add(probe)?.checked_add(dense / 2)?;
        let scout_bytes = BUCKETS.checked_mul(std::mem::size_of::<usize>())?;
        (probe.checked_mul(8)? <= dense
            && allowance <= *remaining
            && scout_bytes <= memory_limit.min(MAX_SEMANTIC_MEMORY_BYTES))
        .then_some((dense, probe))
    });
    let Some((dense, probe)) = plan else {
        return mandatory_match_analysis_with_memory_limit(old, new, remaining, memory_limit);
    };
    if !charge(remaining, probe) {
        return Ok(None);
    }
    let Some(bound) =
        sparse_trial_bound(old, new, memory_limit).filter(|bound| *bound <= dense / 2)
    else {
        return mandatory_match_analysis_with_memory_limit(old, new, remaining, memory_limit);
    };
    let mut trial_remaining = bound;
    let result = sparse(old, new, &mut trial_remaining, memory_limit);
    let spent = bound
        .checked_sub(trial_remaining)
        .ok_or_else(super::cells_limit_error)?;
    *remaining = remaining
        .checked_sub(spent)
        .ok_or_else(super::cells_limit_error)?;
    match result {
        Ok(Some(analysis)) => Ok(Some(analysis)),
        Ok(None) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
            mandatory_match_analysis_with_memory_limit(old, new, remaining, memory_limit)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
