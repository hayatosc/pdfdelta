//! Universal maximal-hunk boundaries without enumerating optimal matchings.
//!
//! An interior script boundary touches a selected equal pair immediately
//! before or after it. Forbidding both adjacent pairs therefore tests whether
//! an optimal matching can avoid the boundary. This proves only the cuts;
//! correspondence, source evidence and internal edit locations remain separate.

use std::ops::Range;

use super::{charge, invalid};
use crate::{Error, Result};

const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Proven,
    Rejected,
    BudgetExceeded,
}

/// Checks the boundary-only predicate for two fixed, source-localized ranges.
///
/// Every optimal matching must pass both cuts and contain a changed hunk
/// between them. The contained token sequences must differ; equal sequences
/// cannot have an optimal changed hunk between universally fixed cuts.
/// This does not establish the stronger invariant-hunk signature.
///
/// # Errors
/// Rejects invalid ranges, unrepresentable work, and bounded allocation failure.
pub(super) fn certify<T: Eq>(
    old: &[T],
    new: &[T],
    target: &[Range<usize>; 2],
    remaining: &mut usize,
) -> Result<Outcome> {
    certify_with_memory_limit(old, new, target, remaining, MAX_MEMORY_BYTES)
}

fn certify_with_memory_limit<T: Eq>(
    old: &[T],
    new: &[T],
    target: &[Range<usize>; 2],
    remaining: &mut usize,
    memory_limit: usize,
) -> Result<Outcome> {
    let Some(a) = old.get(target[0].clone()) else {
        return Err(invalid("boundary cut exceeds the old token range"));
    };
    let Some(b) = new.get(target[1].clone()) else {
        return Err(invalid("boundary cut exceeds the new token range"));
    };
    match paid_equal(a, b, remaining) {
        None => return Ok(Outcome::BudgetExceeded),
        Some(true) => return Ok(Outcome::Rejected),
        Some(false) => {}
    }
    match paid_equal(old, new, remaining) {
        None => return Ok(Outcome::BudgetExceeded),
        Some(true) => return Ok(Outcome::Rejected),
        Some(false) => {}
    }
    let cuts = [
        [target[0].start, target[1].start],
        [target[0].end, target[1].end],
    ];
    let interior = |point: &&[usize; 2]| **point != [0, 0] && **point != [old.len(), new.len()];
    let count = cuts.iter().filter(interior).count();
    if count == 0 {
        return Ok(Outcome::Proven);
    }
    let columns = new.len().checked_add(1).ok_or_else(work_limit)?;
    let slots = columns.checked_mul(2).ok_or_else(work_limit)?;
    let bytes = slots
        .checked_mul(std::mem::size_of::<usize>())
        .ok_or_else(work_limit)?;
    if bytes > memory_limit {
        return Err(Error::LimitExceeded {
            resource: "universal boundary DP memory",
            limit: memory_limit,
        });
    }
    let passes = count + 1;
    // One fee per DP cell, buffer initialization, pass row reset and row start.
    // The entire allowance is reserved before allocating or inspecting cells.
    let work = old
        .len()
        .checked_mul(new.len())
        .and_then(|cells| cells.checked_mul(passes))
        .and_then(|cells| cells.checked_add(columns.checked_mul(passes + 2)?))
        .and_then(|cells| cells.checked_add(old.len().checked_mul(passes)?))
        .ok_or_else(work_limit)?;
    if !charge(remaining, work) {
        return Ok(Outcome::BudgetExceeded);
    }
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(slots)
        .map_err(|_| Error::Unresolved("universal boundary DP allocation".to_owned()))?;
    buffer.resize(slots, 0);
    let optimum = score(old, new, None, &mut buffer);
    for &cut in cuts.iter().filter(interior) {
        if score(old, new, Some(cut), &mut buffer) == optimum {
            return Ok(Outcome::Rejected);
        }
    }
    Ok(Outcome::Proven)
}

fn paid_equal<T: Eq>(old: &[T], new: &[T], remaining: &mut usize) -> Option<bool> {
    if old.len() != new.len() {
        return Some(false);
    }
    for (a, b) in old.iter().zip(new) {
        if !charge(remaining, 1) {
            return None;
        }
        if a != b {
            return Some(false);
        }
    }
    Some(true)
}

fn score<T: Eq>(old: &[T], new: &[T], avoid: Option<[usize; 2]>, buffer: &mut [usize]) -> usize {
    let (mut previous, mut next) = buffer.split_at_mut(new.len() + 1);
    previous.fill(0);
    for (i, a) in old.iter().enumerate() {
        next[0] = 0;
        for (j, b) in new.iter().enumerate() {
            let mut best = previous[j + 1].max(next[j]);
            if a == b && avoid.is_none_or(|cut| cut != [i, j] && cut != [i + 1, j + 1]) {
                best = best.max(previous[j] + 1);
            }
            next[j + 1] = best;
        }
        std::mem::swap(&mut previous, &mut next);
    }
    previous[new.len()]
}

fn work_limit() -> Error {
    Error::LimitExceeded {
        resource: "universal boundary DP work",
        limit: usize::MAX,
    }
}

#[cfg(test)]
mod tests;
