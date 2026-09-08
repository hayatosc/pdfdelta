//! Universal claims over independently optional normalization positions.
//!
//! A side is represented by one original token slice and a mask naming tokens
//! that a checked normalization premise may remove. The traversal keeps one
//! active-index path per side and evaluates every old/new hypothesis pair; it
//! never selects a convenient normalization or stores all expanded variants.

use std::{hash::Hash, mem::size_of};

use super::claims::{self, CountBounds};
use crate::{Error, Result};

const MAX_HYPOTHESIS_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const HYPOTHESIS_MEMORY_RESOURCE: &str = "normalization hypothesis memory";
const HYPOTHESIS_COMBINATIONS_RESOURCE: &str = "normalization hypothesis combinations";

/// Token and mask inputs for one independently normalized side.
pub(super) struct HypothesisSide<'a, T> {
    pub(super) tokens: &'a [T],
    pub(super) optional: &'a [bool],
    pub(super) source: &'a [bool],
    pub(super) residual: &'a [bool],
}

/// Universal literal-minimal claims across every completed hypothesis pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UniversalClaims {
    /// Bounds for the selected source masks across all hypothesis pairs.
    pub(super) source: CountBounds,
    /// Bounds for the selected unresolved masks across all hypothesis pairs.
    pub(super) residual: CountBounds,
    /// Original old-side positions unmatched in every permitted alignment and
    /// every completed normalization hypothesis pair.
    pub(super) mandatory_old: Vec<bool>,
    /// Original new-side positions unmatched in every permitted alignment and
    /// every completed normalization hypothesis pair.
    pub(super) mandatory_new: Vec<bool>,
    /// Number of old/new hypothesis pairs fully quantified by this result.
    pub(super) completed_hypothesis_pairs: usize,
}

/// Evaluates source and residual claims over all independently optional token
/// hypotheses on both sides.
///
/// The optional masks select positions that may be absent. All combinations of
/// those choices are enumerated. Input masks use original token coordinates; a
/// mask entry for an absent optional token is omitted from that hypothesis
/// rather than counted as a change. Returned mandatory masks retain original
/// coordinates and never mark an optional token, because it is absent in at
/// least one permitted hypothesis.
///
/// The traversal shares active-index paths and uses the bounded literal DP for
/// each pair. If any pair or its proof exceeds the remaining work budget, the
/// function returns Ok(None) and publishes no aggregate claim. A result is
/// returned only after every pair has completed.
///
/// # Errors
///
/// Returns Error::InvalidConfiguration for masks whose lengths do not match
/// their sides, Error::LimitExceeded for overflowing hypothesis counts or the
/// configured memory ceiling, and Error::Unresolved when a bounded temporary
/// allocation fails.
pub(super) fn universal_claims<T: Eq + Hash>(
    old: HypothesisSide<'_, T>,
    new: HypothesisSide<'_, T>,
    remaining_work: &mut usize,
) -> Result<Option<UniversalClaims>> {
    validate_lengths(&old, &new)?;
    let setup_work = old
        .tokens
        .len()
        .checked_add(new.tokens.len())
        .and_then(|length| length.checked_mul(2))
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    if !charge(remaining_work, setup_work) {
        return Ok(None);
    }
    let old_hypotheses = hypothesis_count(old.optional)?;
    let new_hypotheses = hypothesis_count(new.optional)?;
    old_hypotheses
        .checked_mul(new_hypotheses)
        .ok_or(limit_error(HYPOTHESIS_COMBINATIONS_RESOURCE))?;
    ensure_memory(required_memory_bytes(old.tokens.len(), new.tokens.len())?)?;

    let mut old_indices = Vec::new();
    old_indices
        .try_reserve_exact(old.tokens.len())
        .map_err(|_| allocation_error("old hypothesis path"))?;
    let mut new_indices = Vec::new();
    new_indices
        .try_reserve_exact(new.tokens.len())
        .map_err(|_| allocation_error("new hypothesis path"))?;

    let mut aggregate = Aggregate::new(old.optional, new.optional);
    for old_choice in 0..old_hypotheses {
        for new_choice in 0..new_hypotheses {
            let pair_work = old
                .tokens
                .len()
                .checked_add(new.tokens.len())
                .and_then(|length| length.checked_add(1))
                .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
            if !charge(remaining_work, pair_work) {
                return Ok(None);
            }
            fill_indices(old.optional, old_choice, &mut old_indices)?;
            fill_indices(new.optional, new_choice, &mut new_indices)?;
            if !evaluate_pair(
                &old,
                &new,
                &old_indices,
                &new_indices,
                remaining_work,
                &mut aggregate,
            )? {
                return Ok(None);
            }
        }
    }

    Ok(Some(aggregate.finish()?))
}

fn fill_indices(optional: &[bool], choice: usize, indices: &mut Vec<usize>) -> Result<()> {
    indices.clear();
    let mut optional_bit = 0u32;
    for (position, is_optional) in optional.iter().copied().enumerate() {
        let present = if is_optional {
            let bit = 1usize
                .checked_shl(optional_bit)
                .ok_or(limit_error(HYPOTHESIS_COMBINATIONS_RESOURCE))?;
            optional_bit = optional_bit
                .checked_add(1)
                .ok_or(limit_error(HYPOTHESIS_COMBINATIONS_RESOURCE))?;
            choice & bit != 0
        } else {
            true
        };
        if present {
            indices.push(position);
        }
    }
    Ok(())
}

fn evaluate_pair<T: Eq + Hash>(
    old: &HypothesisSide<'_, T>,
    new: &HypothesisSide<'_, T>,
    old_indices: &[usize],
    new_indices: &[usize],
    remaining_work: &mut usize,
    aggregate: &mut Aggregate,
) -> Result<bool> {
    let view_work = old_indices
        .len()
        .checked_add(new_indices.len())
        .and_then(|length| length.checked_mul(6))
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    if !charge(remaining_work, view_work) {
        return Ok(false);
    }
    let old_values = values(old.tokens, old_indices)?;
    let new_values = values(new.tokens, new_indices)?;
    let old_source = project(old.source, old_indices)?;
    let new_source = project(new.source, new_indices)?;
    let old_residual = project(old.residual, old_indices)?;
    let new_residual = project(new.residual, new_indices)?;

    let literal = match claims::literal_claims(
        &old_values,
        &new_values,
        &old_source,
        &new_source,
        &old_residual,
        &new_residual,
        remaining_work,
    )? {
        Some(claims) => claims,
        None => return Ok(false),
    };
    let update_work = old_indices
        .len()
        .checked_add(new_indices.len())
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    if !charge(remaining_work, update_work) {
        return Ok(false);
    }
    aggregate.record(
        literal.source,
        literal.residual,
        old_indices,
        new_indices,
        &literal.mandatory,
    )?;
    Ok(true)
}

struct Aggregate {
    source: Option<CountBounds>,
    residual: Option<CountBounds>,
    mandatory_old: Vec<bool>,
    mandatory_new: Vec<bool>,
    completed_hypothesis_pairs: usize,
}

impl Aggregate {
    fn new(old_optional: &[bool], new_optional: &[bool]) -> Self {
        Self {
            source: None,
            residual: None,
            mandatory_old: old_optional.iter().map(|optional| !optional).collect(),
            mandatory_new: new_optional.iter().map(|optional| !optional).collect(),
            completed_hypothesis_pairs: 0,
        }
    }

    fn record(
        &mut self,
        source: CountBounds,
        residual: CountBounds,
        old_indices: &[usize],
        new_indices: &[usize],
        mandatory: &claims::MandatoryChanges,
    ) -> Result<()> {
        update_bounds(&mut self.source, source);
        update_bounds(&mut self.residual, residual);
        for (&original, &changed) in old_indices.iter().zip(&mandatory.old) {
            self.mandatory_old[original] &= changed;
        }
        for (&original, &changed) in new_indices.iter().zip(&mandatory.new) {
            self.mandatory_new[original] &= changed;
        }
        self.completed_hypothesis_pairs = self
            .completed_hypothesis_pairs
            .checked_add(1)
            .ok_or(limit_error(HYPOTHESIS_COMBINATIONS_RESOURCE))?;
        Ok(())
    }

    fn finish(self) -> Result<UniversalClaims> {
        Ok(UniversalClaims {
            source: self
                .source
                .ok_or_else(|| invalid("hypothesis traversal produced no pairs"))?,
            residual: self
                .residual
                .ok_or_else(|| invalid("hypothesis traversal produced no pairs"))?,
            mandatory_old: self.mandatory_old,
            mandatory_new: self.mandatory_new,
            completed_hypothesis_pairs: self.completed_hypothesis_pairs,
        })
    }
}

fn update_bounds(target: &mut Option<CountBounds>, current: CountBounds) {
    if let Some(target) = target {
        target.lower = target.lower.min(current.lower);
        target.upper = target.upper.max(current.upper);
    } else {
        *target = Some(current);
    }
}

fn validate_lengths<T>(old: &HypothesisSide<'_, T>, new: &HypothesisSide<'_, T>) -> Result<()> {
    if old.tokens.len() != old.optional.len()
        || new.tokens.len() != new.optional.len()
        || old.tokens.len() != old.source.len()
        || new.tokens.len() != new.source.len()
        || old.tokens.len() != old.residual.len()
        || new.tokens.len() != new.residual.len()
    {
        return Err(Error::InvalidConfiguration(
            "normalization hypothesis masks must match their input lengths".to_owned(),
        ));
    }
    Ok(())
}

fn hypothesis_count(optional: &[bool]) -> Result<usize> {
    optional.iter().try_fold(1usize, |count, is_optional| {
        if *is_optional {
            count
                .checked_mul(2)
                .ok_or(limit_error(HYPOTHESIS_COMBINATIONS_RESOURCE))
        } else {
            Ok(count)
        }
    })
}

fn required_memory_bytes(old_len: usize, new_len: usize) -> Result<usize> {
    let total = old_len
        .checked_add(new_len)
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    let indices = total
        .checked_mul(size_of::<usize>())
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    let values = total
        .checked_mul(size_of::<&()>())
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    let masks = total
        .checked_mul(5 * size_of::<bool>())
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))?;
    indices
        .checked_add(values)
        .and_then(|bytes| bytes.checked_add(masks))
        .ok_or(limit_error(HYPOTHESIS_MEMORY_RESOURCE))
}

fn ensure_memory(bytes: usize) -> Result<()> {
    if bytes > MAX_HYPOTHESIS_MEMORY_BYTES {
        return Err(Error::LimitExceeded {
            resource: HYPOTHESIS_MEMORY_RESOURCE,
            limit: MAX_HYPOTHESIS_MEMORY_BYTES,
        });
    }
    Ok(())
}

fn values<'a, T>(input: &'a [T], indices: &[usize]) -> Result<Vec<&'a T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(indices.len())
        .map_err(|_| allocation_error("hypothesis token view"))?;
    for &index in indices {
        values.push(
            input
                .get(index)
                .ok_or_else(|| invalid("hypothesis path index is out of bounds"))?,
        );
    }
    Ok(values)
}

fn project(input: &[bool], indices: &[usize]) -> Result<Vec<bool>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(indices.len())
        .map_err(|_| allocation_error("hypothesis mask view"))?;
    for &index in indices {
        output.push(
            *input
                .get(index)
                .ok_or_else(|| invalid("hypothesis mask index is out of bounds"))?,
        );
    }
    Ok(output)
}

fn charge(remaining_work: &mut usize, amount: usize) -> bool {
    match remaining_work.checked_sub(amount) {
        Some(remaining) => {
            *remaining_work = remaining;
            true
        }
        None => {
            *remaining_work = 0;
            false
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::InvalidConfiguration(message.to_owned())
}

fn limit_error(resource: &'static str) -> Error {
    Error::LimitExceeded {
        resource,
        limit: usize::MAX,
    }
}

fn allocation_error(resource: &'static str) -> Error {
    Error::Unresolved(format!("{resource} allocation failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all<T>(tokens: &[T]) -> Vec<bool> {
        vec![true; tokens.len()]
    }

    fn none<T>(tokens: &[T]) -> Vec<bool> {
        vec![false; tokens.len()]
    }

    fn side<'a, T>(
        tokens: &'a [T],
        optional: &'a [bool],
        source: &'a [bool],
        residual: &'a [bool],
    ) -> HypothesisSide<'a, T> {
        HypothesisSide {
            tokens,
            optional,
            source,
            residual,
        }
    }

    #[test]
    fn optional_line_break_hyphen_keeps_the_numeric_change_and_leaves_hyphen_unresolved() {
        let old = b"inter-national tax is 20.";
        let new = b"international tax is 30.";
        let mut old_optional = none(old);
        old_optional[5] = true;
        let new_optional = none(new);
        let old_source = all(old);
        let new_source = all(new);
        let old_residual = old
            .iter()
            .enumerate()
            .map(|(index, _)| index == 5)
            .collect::<Vec<_>>();
        let new_residual = none(new);
        let mut work = usize::MAX;
        let claims = universal_claims(
            side(old, &old_optional, &old_source, &old_residual),
            side(new, &new_optional, &new_source, &new_residual),
            &mut work,
        )
        .expect("optional line-break hypotheses should be valid")
        .expect("all two hypotheses should fit the budget");

        assert_eq!(claims.completed_hypothesis_pairs, 2);
        assert_eq!(claims.source, CountBounds { lower: 2, upper: 3 });
        assert_eq!(claims.residual, CountBounds { lower: 0, upper: 1 });
        assert!(!claims.mandatory_old[5]);
        assert_eq!(
            claims
                .mandatory_old
                .iter()
                .filter(|changed| **changed)
                .count(),
            1
        );
        assert_eq!(
            claims
                .mandatory_new
                .iter()
                .filter(|changed| **changed)
                .count(),
            1
        );
    }

    #[test]
    fn optional_recover_hyphen_has_no_universal_mandatory_position() {
        let old = b"re-cover";
        let new = b"recover";
        let mut old_optional = none(old);
        old_optional[2] = true;
        let new_optional = none(new);
        let old_source = all(old);
        let new_source = all(new);
        let old_residual = none(old);
        let new_residual = none(new);
        let mut work = usize::MAX;
        let claims = universal_claims(
            side(old, &old_optional, &old_source, &old_residual),
            side(new, &new_optional, &new_source, &new_residual),
            &mut work,
        )
        .expect("optional recover hypotheses should be valid")
        .expect("all two hypotheses should fit the budget");

        assert_eq!(claims.completed_hypothesis_pairs, 2);
        assert!(claims.mandatory_old.iter().all(|changed| !changed));
        assert!(claims.mandatory_new.iter().all(|changed| !changed));
        assert_eq!(claims.source, CountBounds { lower: 0, upper: 1 });
    }

    #[test]
    fn both_sides_are_enumerated_without_cloning_the_token_values() {
        let old = b"ab";
        let new = b"ac";
        let old_optional = [false, true];
        let new_optional = [false, true];
        let old_source = all(old);
        let new_source = all(new);
        let old_residual = none(old);
        let new_residual = none(new);
        let mut work = usize::MAX;
        let claims = universal_claims(
            side(old, &old_optional, &old_source, &old_residual),
            side(new, &new_optional, &new_source, &new_residual),
            &mut work,
        )
        .expect("both-side hypotheses should be valid")
        .expect("all four hypotheses should fit the budget");

        assert_eq!(claims.completed_hypothesis_pairs, 4);
        assert_eq!(claims.source, CountBounds { lower: 0, upper: 2 });
        assert_eq!(claims.mandatory_old, vec![false, false]);
        assert_eq!(claims.mandatory_new, vec![false, false]);
    }

    #[test]
    fn incomplete_hypothesis_enumeration_publishes_no_partial_claim() {
        let old = b"abcd";
        let new = b"wxyz";
        let old_optional = [true, true, true, true];
        let new_optional = [true, true, true, true];
        let old_source = all(old);
        let new_source = all(new);
        let old_residual = none(old);
        let new_residual = none(new);
        let mut work = 20;
        let claims = universal_claims(
            side(old, &old_optional, &old_source, &old_residual),
            side(new, &new_optional, &new_source, &new_residual),
            &mut work,
        )
        .expect("budget exhaustion is an unresolved result");
        assert_eq!(claims, None);
        assert_eq!(work, 0);
    }

    #[test]
    fn inconsistent_hypothesis_masks_are_rejected_before_traversal() {
        let old = b"a";
        let new = b"b";
        let old_optional = [];
        let new_optional = [false];
        let old_source = [true];
        let new_source = [true];
        let old_residual = [false];
        let new_residual = [false];
        let mut work = 10;
        let result = universal_claims(
            side(old, &old_optional, &old_source, &old_residual),
            side(new, &new_optional, &new_source, &new_residual),
            &mut work,
        );
        assert_eq!(
            result,
            Err(Error::InvalidConfiguration(
                "normalization hypothesis masks must match their input lengths".to_owned()
            ))
        );
        assert_eq!(work, 10);
    }

    #[test]
    fn mandatory_runs_do_not_use_call_stack() {
        let old = vec![b'a'; 16 * 1024];
        let new = Vec::<u8>::new();
        let old_optional = none(&old);
        let new_optional = none(&new);
        let old_source = all(&old);
        let new_source = all(&new);
        let old_residual = none(&old);
        let new_residual = none(&new);
        let mut work = usize::MAX;
        let claims = universal_claims(
            side(&old, &old_optional, &old_source, &old_residual),
            side(&new, &new_optional, &new_source, &new_residual),
            &mut work,
        )
        .expect("long mandatory runs should be valid")
        .expect("long mandatory runs should fit the budget");

        assert_eq!(
            claims.source,
            CountBounds {
                lower: old.len(),
                upper: old.len()
            }
        );
        assert!(claims.mandatory_old.iter().all(|changed| *changed));
    }

    #[test]
    fn all_hyphen_hypotheses_keep_insertion_ambiguity() {
        let old = b"a-b";
        let new = b"ab-b";
        let old_optional = [false, true, false];
        let new_optional = none(new);
        let old_source = all(old);
        let new_source = all(new);
        let old_residual = none(old);
        let new_residual = none(new);
        let mut work = usize::MAX;
        let claims = universal_claims(
            side(old, &old_optional, &old_source, &old_residual),
            side(new, &new_optional, &new_source, &new_residual),
            &mut work,
        )
        .expect("the hyphen hypotheses should be valid")
        .expect("all hyphen hypotheses should fit the budget");

        assert_eq!(claims.completed_hypothesis_pairs, 2);
        assert_eq!(claims.source, CountBounds { lower: 1, upper: 2 });
        assert_eq!(claims.mandatory_old, vec![false, false, false]);
        assert_eq!(claims.mandatory_new, vec![false, false, false, false]);
    }
}
