//! Source certificates for the common refinement of an exact equal parent.
use super::{Check, EqualFragmentCache, FragmentHold, Stop, charge};
use crate::diff::assessment::{
    BlockId, ComparableToken, ComparisonAssumption, DomainKey, DomainProof, ProofScope,
    RelationAssessment, RelationOutcome, ScalarRange, SearchCompleteness, Side, TextSpan,
    TokenRange, borrowed_group_pair, light_group_metadata,
};
use crate::{Result, normalize::BlockText};

#[cfg(test)]
mod tests;

pub(in crate::diff::assessment) const MAX_REFINED_PARENT_TOKENS: usize = 512;

/// One restriction of the full parent's unique N/N diagonal, never a local LCS.
pub(in crate::diff::assessment) struct RefinedLiteralRun {
    pub spans: [TextSpan; 2],
    pub parent_start: usize,
    pub parent_end: usize,
}

/// Constructed only after every nonspace token of the exact whole parent has
/// passed literal singleton, contiguous raw, normalization and global sharing
/// checks. It grants no ownership and no exception to another source claim.
pub(in crate::diff::assessment) struct RefinedParentCertificate<'a> {
    parent: usize,
    blocks: [&'a [BlockText]; 2],
    canonical: [&'a [Vec<ComparableToken>]; 2],
    runs: Vec<RefinedLiteralRun>,
    bytes: usize,
}

impl RefinedParentCertificate<'_> {
    pub(in crate::diff::assessment) fn runs(&self) -> &[RefinedLiteralRun] {
        &self.runs
    }

    pub(in crate::diff::assessment) fn bytes(&self) -> usize {
        self.bytes
    }

    /// A completed source verdict cannot be reused for another side or range.
    pub(in crate::diff::assessment) fn accepts(
        &self,
        sides: [&Side<'_>; 2],
        parent: usize,
        spans: [&TextSpan; 2],
        remaining: &mut usize,
    ) -> bool {
        if !charge(
            remaining,
            self.runs.len().saturating_mul(24).saturating_add(8),
        ) || self.parent != parent
            || !(0..2).all(|s| {
                std::ptr::eq(self.blocks[s], sides[s].blocks)
                    && std::ptr::eq(self.canonical[s], sides[s].canonical.as_slice())
            })
        {
            return false;
        }
        self.runs
            .iter()
            .any(|run| run.spans[0] == *spans[0] && run.spans[1] == *spans[1])
    }
}

pub(in crate::diff::assessment) fn eligible_refinement(
    key: &DomainKey,
    proof: &DomainProof,
    parent: &RelationAssessment,
) -> bool {
    key.local.is_none()
        && key.old.len() > 1
        && key.new.len() > 1
        && proof.scope == ProofScope::ExactKey
        && proof.search == SearchCompleteness::Complete
        && proof.unique
        && proof.strict_unique
        && proof.edits.is_empty()
        && proof.lengths[0] > 0
        && proof.lengths[0] <= MAX_REFINED_PARENT_TOKENS
        && proof.lengths[0] == proof.lengths[1]
        && parent.parent.is_none()
        && parent.outcome == RelationOutcome::Established
        && parent.search == SearchCompleteness::Complete
        && parent.reasons.is_empty()
        && parent
            .assumptions
            .contains(&ComparisonAssumption::LocalEvidenceBoundaries)
        && !parent
            .assumptions
            .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
        && !parent
            .assumptions
            .contains(&ComparisonAssumption::RefinedExactParentEquality)
        && !parent
            .assumptions
            .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality)
}

#[derive(Clone, Copy)]
struct Origin {
    block: BlockId,
    offset: usize,
}

fn origins(
    side: &Side<'_>,
    span: &TextSpan,
    length: usize,
    remaining: &mut usize,
) -> Check<Vec<Option<Origin>>> {
    if !charge(
        remaining,
        length
            .saturating_mul(8)
            .saturating_add(span.blocks.len().saturating_mul(8)),
    ) {
        return Err(Stop::Exhausted);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Stop::Exhausted)?;
    let mut previous_space = None;
    let mut previous_index = None;
    let mut page = None;
    for (position, id) in span.blocks.iter().enumerate() {
        let Some(&index) = side.index.get(id) else {
            return Err(Stop::Held(FragmentHold::Projection));
        };
        let block = &side.blocks[index];
        let [this_page] = block.pages.as_slice() else {
            return Err(Stop::Held(FragmentHold::MultiplePages));
        };
        if previous_index.is_some_and(|i| i >= index)
            || page.is_some_and(|p| p != *this_page)
            || !block.canonical.unmapped.is_empty()
            || !block.raw.unmapped.is_empty()
        {
            return Err(Stop::Held(FragmentHold::Projection));
        }
        previous_index = Some(index);
        page = Some(*this_page);
        let tokens = &side.canonical[index];
        let scalar_space =
            |t: &ComparableToken| matches!(t, ComparableToken::Scalar(c) if c.is_whitespace());
        if position > 0
            && span
                .separator
                .is_some_and(|s| s.at(position - 1) == crate::alignment::BlockSeparator::Space)
            && previous_space != Some(true)
            && !tokens.first().is_some_and(scalar_space)
        {
            if output.len() == length {
                return Err(Stop::Held(FragmentHold::Projection));
            }
            output.push(None);
            previous_space = Some(true);
        }
        for (offset, token) in tokens.iter().enumerate() {
            if !matches!(token, ComparableToken::Scalar(_)) || output.len() == length {
                return Err(Stop::Held(FragmentHold::Projection));
            }
            output.push(Some(Origin { block: *id, offset }));
            previous_space = Some(scalar_space(token));
        }
    }
    if output.len() != length {
        return Err(Stop::Held(FragmentHold::Projection));
    }
    Ok(output)
}

impl<'a> EqualFragmentCache<'a> {
    /// Certifies source-preserving subdivisions of an independently closed
    /// exact equal parent. Page ordinals may differ only under this whole
    /// parent's retained correspondence; positions supply no correspondence.
    /// Synthetic separators and all whitespace remain outside the certificate.
    pub(in crate::diff::assessment) fn prove_refined_equal_parent(
        &mut self,
        key: &DomainKey,
        proof: &DomainProof,
        parent: &RelationAssessment,
        available: usize,
        remaining: &mut usize,
    ) -> Result<Option<RefinedParentCertificate<'a>>> {
        match self.refined_equal_parent(key, proof, parent, available, remaining) {
            Ok(proof) => Ok(Some(proof)),
            Err(Stop::Held(_) | Stop::Exhausted) => Ok(None),
            Err(Stop::Error(error)) => Err(error),
        }
    }

    fn refined_equal_parent(
        &mut self,
        key: &DomainKey,
        proof: &DomainProof,
        parent: &RelationAssessment,
        available: usize,
        remaining: &mut usize,
    ) -> Check<RefinedParentCertificate<'a>> {
        let held = || Stop::Held(FragmentHold::ParentCorrespondence);
        if !charge(remaining, parent.assumptions.len().saturating_add(32)) {
            return Err(Stop::Exhausted);
        }
        if !eligible_refinement(key, proof, parent) {
            return Err(held());
        }
        let (Some(old), Some(new)) = (&parent.old_span, &parent.new_span) else {
            return Err(held());
        };
        let spans = [old, new];
        for (side, (range, separator)) in [
            (key.old.clone(), key.old_separator),
            (key.new.clone(), key.new_separator),
        ]
        .into_iter()
        .enumerate()
        {
            let metadata =
                light_group_metadata(self.sides[side], range, separator, remaining, available)
                    .map_err(|_| held())?;
            if metadata.span != *spans[side] || metadata.has_unmapped {
                return Err(held());
            }
        }
        let n = proof.lengths[0];
        let planned = n
            .checked_mul(
                2 * std::mem::size_of::<Option<Origin>>()
                    + std::mem::size_of::<RefinedLiteralRun>()
                    + 2 * std::mem::size_of::<BlockId>(),
            )
            .and_then(|b| b.checked_add(4096))
            .ok_or_else(held)?;
        let view_limit = available.checked_sub(planned).ok_or_else(held)?;
        let views =
            borrowed_group_pair(self.sides, spans, remaining, view_limit).map_err(|_| held())?;
        let tokens = [views[0].selected_tokens(), views[1].selected_tokens()];
        if !charge(remaining, n.saturating_mul(4)) {
            return Err(Stop::Exhausted);
        }
        if tokens[0].len() != n || tokens[1].len() != n || tokens[0] != tokens[1] {
            return Err(held());
        }
        let source = [
            origins(self.sides[0], old, n, remaining)?,
            origins(self.sides[1], new, n, remaining)?,
        ];
        let mut runs = Vec::<RefinedLiteralRun>::new();
        if !charge(remaining, n.saturating_mul(32)) {
            return Err(Stop::Exhausted);
        }
        runs.try_reserve_exact(n).map_err(|_| Stop::Exhausted)?;
        let mut pending = None;
        for i in 0..n {
            if !matches!(tokens[0][i], ComparableToken::Scalar(c) if !c.is_whitespace()) {
                pending = None;
                continue;
            }
            let (Some(a), Some(b)) = (source[0][i], source[1][i]) else {
                return Err(held());
            };
            let pair = [a, b];
            let extends = pending.is_some_and(|index: usize| {
                (0..2).all(|s| {
                    runs[index].spans[s].blocks.as_slice() == [pair[s].block]
                        && runs[index].spans[s].comparable_range.end == pair[s].offset
                })
            });
            if extends {
                let run = &mut runs[pending.expect("tested current run")];
                for (span, origin) in run.spans.iter_mut().zip(pair) {
                    span.canonical_range.end = origin.offset + 1;
                    span.comparable_range.end = origin.offset + 1;
                }
                run.parent_end = i + 1;
            } else {
                let span = |origin: Origin| -> Check<TextSpan> {
                    let mut blocks = Vec::new();
                    blocks.try_reserve_exact(1).map_err(|_| Stop::Exhausted)?;
                    blocks.push(origin.block);
                    Ok(TextSpan {
                        blocks,
                        separator: None,
                        canonical_range: ScalarRange {
                            start: origin.offset,
                            end: origin.offset + 1,
                        },
                        comparable_range: TokenRange {
                            start: origin.offset,
                            end: origin.offset + 1,
                        },
                    })
                };
                runs.push(RefinedLiteralRun {
                    spans: [span(a)?, span(b)?],
                    parent_start: i,
                    parent_end: i + 1,
                });
                pending = Some(runs.len() - 1);
            }
        }
        if runs.is_empty() {
            return Err(held());
        }
        let bytes = runs
            .capacity()
            .checked_mul(std::mem::size_of::<RefinedLiteralRun>())
            .and_then(|b| {
                runs.iter().try_fold(b, |b, r| {
                    r.spans.iter().try_fold(b, |b, s| {
                        b.checked_add(
                            s.blocks
                                .capacity()
                                .checked_mul(std::mem::size_of::<BlockId>())?,
                        )
                    })
                })
            })
            .ok_or_else(held)?;
        let transient = source
            .iter()
            .try_fold(bytes, |b, s| {
                b.checked_add(
                    s.capacity()
                        .checked_mul(std::mem::size_of::<Option<Origin>>())?,
                )
            })
            .and_then(|b| b.checked_add(views[0].retained_bytes))
            .and_then(|b| b.checked_add(views[1].retained_bytes))
            .and_then(|b| b.checked_add(4096))
            .ok_or_else(held)?;
        if transient > available {
            return Err(held());
        }
        // Every run, including previously owned tokens, passes before the
        // certificate escapes. A failure cannot certify a partial parent.
        for run in &runs {
            self.check_page_shifted_literal_sources([&run.spans[0], &run.spans[1]], remaining)?;
        }
        Ok(RefinedParentCertificate {
            parent: proof.relation,
            blocks: self.sides.map(|s| s.blocks),
            canonical: self.sides.map(|s| s.canonical.as_slice()),
            runs,
            bytes,
        })
    }
}
