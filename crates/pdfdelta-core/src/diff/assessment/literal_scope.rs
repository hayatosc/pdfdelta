//! A changed local interval with one optimal positional matching.
//!
//! This alone proves no cross-document correspondence. A caller must also
//! establish both cuts on every optimal matching of its closed parent domain
//! and bind these exact tokens to that interval before accepting a proposal.

use super::{charge, exact};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalOutcome {
    UniqueChanged,
    Ambiguous,
    Unavailable,
    BudgetExceeded,
}

/// Checks local position uniqueness without enumerating optimal matchings.
///
/// Empty or identical scripts cannot establish a changed hunk. The existing
/// exact checker counts the union of optimal matching edges; its rolling
/// reverse scan avoids enumerating unrelated repetitions. Table initialization
/// is prepaid here in addition to the checker's charged interior cells.
pub(super) fn unique_changed<T: Eq>(
    old: &[T],
    new: &[T],
    remaining: &mut usize,
) -> Result<LocalOutcome> {
    if old.len() == new.len() {
        let mut identical = true;
        for (a, b) in old.iter().zip(new) {
            if !charge(remaining, 1) {
                return Ok(LocalOutcome::BudgetExceeded);
            }
            if a != b {
                identical = false;
                break;
            }
        }
        if identical {
            return Ok(LocalOutcome::Unavailable);
        }
    }
    if !old.is_empty() && !new.is_empty() {
        let columns = new.len().checked_add(1).ok_or_else(work_limit)?;
        let slots = old
            .len()
            .checked_add(3)
            .and_then(|rows| rows.checked_mul(columns))
            .ok_or_else(work_limit)?;
        let bytes = slots
            .checked_mul(std::mem::size_of::<usize>())
            .ok_or_else(work_limit)?;
        if bytes > 64 * 1024 * 1024 {
            return Err(Error::LimitExceeded {
                resource: "local literal uniqueness memory",
                limit: 64 * 1024 * 1024,
            });
        }
        if !charge(remaining, slots) {
            return Ok(LocalOutcome::BudgetExceeded);
        }
    }
    Ok(match exact::check_retaining(old, new, remaining)?.outcome {
        exact::ExactUniqueness::Unique => LocalOutcome::UniqueChanged,
        exact::ExactUniqueness::Ambiguous => LocalOutcome::Ambiguous,
        exact::ExactUniqueness::BudgetExceeded => LocalOutcome::BudgetExceeded,
    })
}

fn work_limit() -> Error {
    Error::LimitExceeded {
        resource: "local literal uniqueness work",
        limit: usize::MAX,
    }
}

#[cfg(test)]
mod tests;
