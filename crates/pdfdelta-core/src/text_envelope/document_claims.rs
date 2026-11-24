use serde::{Deserialize, Serialize};

use crate::{Result, normalize::ComparableToken};

use super::{
    claims,
    envelope::{
        EnvelopeUnavailable, LocalTextEnvelope, LocalTextEnvelopeLimits, LocalTextEnvelopeResult,
        MAX_ENVELOPE_CELLS,
    },
    hypotheses::{self, HypothesisSide},
};

/// Inputs retain original token positions. Optional removal requires an
/// independently established normalization premise, not a lower edit cost.
pub struct LocalTextSide<'a> {
    pub tokens: &'a [ComparableToken],
    pub source: &'a [bool],
    pub optional: &'a [bool],
}

/// Retains all optimal literal alignments as a bounded DAG and reports the
/// exact positions whose operation or counterpart can vary. This is an opt-in
/// explanation; it never establishes document correspondence or changes an
/// existing comparison's status. No paths are enumerated.
///
/// Uses the same banded prefix grid and backward scan as [`local_text_claims`].
/// With `C` retained cells and `n + m` tokens, additional time and space are
/// `O(C + n + m)`; the existing bit-parallel LCS prepass is unchanged. Optional
/// normalization families are refused rather than choosing a convenient one.
/// A refusal contains no partial graph. Completed setup work remains spent.
///
/// # Errors
/// Rejects inconsistent masks or a cell cap above [`MAX_ENVELOPE_CELLS`], and
/// returns allocation/dimension errors from the existing bounded proof kernel.
pub fn local_text_envelope(
    old: LocalTextSide<'_>,
    new: LocalTextSide<'_>,
    remaining_work: &mut usize,
    limits: LocalTextEnvelopeLimits,
) -> Result<LocalTextEnvelopeResult> {
    if old.optional.len() != old.tokens.len()
        || new.optional.len() != new.tokens.len()
        || old.source.len() != old.tokens.len()
        || new.source.len() != new.tokens.len()
    {
        return Err(crate::Error::InvalidConfiguration(
            "local envelope mask length differs from tokens".into(),
        ));
    }
    if limits.max_cells > MAX_ENVELOPE_CELLS {
        return Err(crate::Error::InvalidConfiguration(
            "local envelope cell cap exceeds the internal maximum".into(),
        ));
    }
    if old
        .optional
        .iter()
        .chain(new.optional)
        .any(|optional| *optional)
    {
        return Ok(LocalTextEnvelopeResult::Unavailable {
            reason: EnvelopeUnavailable::NormalizationAlternatives,
        });
    }
    let mut capture = claims::DagCapture::new(limits.max_cells);
    let Some(claim) = claims::literal_claims_with_dag(
        old.tokens,
        new.tokens,
        old.source,
        new.source,
        remaining_work,
        &mut capture,
    )?
    else {
        return Ok(LocalTextEnvelopeResult::Unavailable {
            reason: capture.refusal,
        });
    };
    let mut claims = LocalTextClaims {
        changed_source_lower: claim.source.lower,
        changed_source_upper: claim.source.upper,
        mandatory_old: claim.mandatory.old,
        mandatory_new: claim.mandatory.new,
        normalization_pairs: 1,
    };
    for (mandatory, source) in claims.mandatory_old.iter_mut().zip(old.source) {
        *mandatory &= *source;
    }
    for (mandatory, source) in claims.mandatory_new.iter_mut().zip(new.source) {
        *mandatory &= *source;
    }
    let dag = capture
        .dag
        .ok_or_else(|| claims::invalid("complete envelope has no DAG"))?;
    Ok(LocalTextEnvelopeResult::Complete {
        envelope: Box::new(LocalTextEnvelope::from_dag(claims, dag)?),
    })
}

/// Exact claims under literal-minimal token alignment and the supplied optional
/// normalization premises. These do not establish document correspondence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalTextClaims {
    pub changed_source_lower: usize,
    pub changed_source_upper: usize,
    pub mandatory_old: Vec<bool>,
    pub mandatory_new: Vec<bool>,
    pub normalization_pairs: usize,
}

/// Runs the existing bounded proof kernel for an already proposed local pair.
/// Returned masks contain only source-backed positions; synthetic layout
/// separators participate in alignment but never become changed source tokens.
/// `None` means a proof did not finish, not that the pair is equal.
/// An exact grid declined before evaluation leaves its unspent work available;
/// completed setup and LCS work remain charged.
///
/// # Errors
/// Rejects inconsistent mask dimensions and reports resource/allocation limits
/// from the shared exact and normalization kernels.
pub fn local_text_claims(
    old: LocalTextSide<'_>,
    new: LocalTextSide<'_>,
    remaining_work: &mut usize,
) -> Result<Option<LocalTextClaims>> {
    if old.optional.len() != old.tokens.len() || new.optional.len() != new.tokens.len() {
        return Err(crate::Error::InvalidConfiguration(
            "local normalization mask length differs from tokens".into(),
        ));
    }
    let result = if old
        .optional
        .iter()
        .chain(new.optional)
        .any(|optional| *optional)
    {
        let Some(claim) = hypotheses::universal_claims(
            HypothesisSide {
                tokens: old.tokens,
                optional: old.optional,
                source: old.source,
                residual: old.source,
            },
            HypothesisSide {
                tokens: new.tokens,
                optional: new.optional,
                source: new.source,
                residual: new.source,
            },
            remaining_work,
        )?
        else {
            return Ok(None);
        };
        LocalTextClaims {
            changed_source_lower: claim.source.lower,
            changed_source_upper: claim.source.upper,
            mandatory_old: claim.mandatory_old,
            mandatory_new: claim.mandatory_new,
            normalization_pairs: claim.completed_hypothesis_pairs,
        }
    } else {
        let Some(claim) = claims::literal_claims_retaining_unspent_grid_work(
            old.tokens,
            new.tokens,
            old.source,
            new.source,
            old.source,
            new.source,
            remaining_work,
        )?
        else {
            return Ok(None);
        };
        LocalTextClaims {
            changed_source_lower: claim.source.lower,
            changed_source_upper: claim.source.upper,
            mandatory_old: claim.mandatory.old,
            mandatory_new: claim.mandatory.new,
            normalization_pairs: 1,
        }
    };
    let mut result = result;
    for (mandatory, source) in result.mandatory_old.iter_mut().zip(old.source) {
        *mandatory &= *source;
    }
    for (mandatory, source) in result.mandatory_new.iter_mut().zip(new.source) {
        *mandatory &= *source;
    }
    Ok(Some(result))
}
