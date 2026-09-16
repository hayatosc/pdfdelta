//! Non-owning claims over established comparison domains.

use super::{
    AlignmentPolicy, AssessmentReason, Assessor, ComparisonAssumption, EditCountBounds, GroupText,
    ProposedRelation, RelationOutcome, ResolutionRange, ResolutionState, ReviewUnit,
    SearchCompleteness, Side, TextSpan, allocation_error, claims, hypotheses, normalization,
    proof_groups, space_token,
};
use crate::{Error, Result, alignment::BlockSeparator};

pub(super) fn collect(
    assessor: &mut Assessor<'_, '_>,
    partitions: [&[ResolutionRange]; 2],
) -> Result<Vec<ReviewUnit>> {
    discover_hypothesis_domains(assessor)?;
    // Localization has finished. Move its cached domains rather than cloning
    // every source span and edit witness for the final non-owning claim pass.
    let mut domains = Vec::new();
    domains
        .try_reserve_exact(assessor.domains.len())
        .map_err(|_| allocation_error("review domains"))?;
    domains.extend(std::mem::take(&mut assessor.domains));
    domains.sort_by_key(|(_, proof)| proof.relation);
    let mut units = Vec::new();
    let mut output_ranges = 0usize;
    for (key, proof) in domains {
        let relation = proof.relation;
        if assessor.remaining_work == 0 || output_ranges >= assessor.options.max_assessment_ranges {
            break;
        }
        let reasons = &assessor.records[relation].reasons;
        let alternative_normalization = reasons
            .contains(&AssessmentReason::NormalizationUncertainty)
            && reasons.iter().all(|reason| {
                matches!(
                    reason,
                    AssessmentReason::NormalizationUncertainty | AssessmentReason::DomainNotClosed
                )
            });
        if assessor.records[relation].outcome != RelationOutcome::Established
            && !alternative_normalization
        {
            continue;
        }
        if !assessor.charge(assessor.alignment.spans.len()) {
            break;
        }
        let mut search_complete = true;
        for index in 0..assessor.alignment.spans.len() {
            let span = &assessor.alignment.spans[index];
            if !span
                .evidence
                .contains(&crate::alignment::AlignmentEvidence::SearchIncomplete)
            {
                continue;
            }
            let work = span
                .old
                .len()
                .saturating_add(span.new.len())
                .saturating_add(1);
            if !assessor.charge(work) {
                search_complete = false;
                break;
            }
            let span = &assessor.alignment.spans[index];
            if span
                .old
                .iter()
                .any(|block| key.old.contains(&assessor.sides[0].index[block]))
                || span
                    .new
                    .iter()
                    .any(|block| key.new.contains(&assessor.sides[1].index[block]))
            {
                search_complete = false;
                break;
            }
        }
        if !search_complete {
            continue;
        }
        let groups = proof_groups(assessor.sides, &key)?;
        let mapping_work = groups
            .iter()
            .enumerate()
            .try_fold(0usize, |sum, (side, group)| {
                sum.checked_add(group.tokens.len())?
                    .checked_add(group.blocks.len().checked_mul(partitions[side].len())?)
            });
        if !assessor.charge(mapping_work.unwrap_or(usize::MAX)) {
            break;
        }
        let masks = [
            masks(assessor.sides[0], &groups[0], partitions[0])?,
            masks(assessor.sides[1], &groups[1], partitions[1])?,
        ];
        // Fully resolved units already have exact events and need no extra DP.
        if !masks
            .iter()
            .any(|mask| mask.unresolved.iter().any(|value| *value))
        {
            continue;
        }
        let mut unit = ReviewUnit {
            relation,
            policy: AlignmentPolicy::LiteralMinimal,
            search: SearchCompleteness::Incomplete,
            normalization_hypotheses: 1,
            normalization_old: Vec::new(),
            normalization_new: Vec::new(),
            changed_count: None,
            unresolved_changed_count: None,
            mandatory_old: Vec::new(),
            mandatory_new: Vec::new(),
        };
        output_ranges += 1;
        if alternative_normalization {
            if let Some(unit) =
                normalized_unit(assessor, &groups, &masks, unit, &mut output_ranges)?
            {
                units.push(unit);
            }
            continue;
        }
        let literal = match claims::literal_claims(
            &groups[0].tokens,
            &groups[1].tokens,
            &masks[0].source,
            &masks[1].source,
            &masks[0].unresolved,
            &masks[1].unresolved,
            &mut assessor.remaining_work,
        ) {
            Ok(Some(literal)) => Some(literal),
            Ok(None) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => None,
            Err(error) => return Err(error),
        };
        let Some(literal) = literal else {
            units.push(unit);
            continue;
        };
        unit.changed_count = Some(EditCountBounds {
            lower: literal.source.lower,
            upper: literal.source.upper,
        });
        let residual = if masks.iter().all(|mask| mask.source == mask.unresolved) {
            literal.source
        } else {
            literal.residual
        };
        unit.unresolved_changed_count = Some(EditCountBounds {
            lower: residual.lower,
            upper: residual.upper,
        });
        let mut limit = assessor.options.max_assessment_ranges - output_ranges;
        if let Some(old) = spans(
            &groups[0],
            &literal.mandatory.old,
            &masks[0].source,
            &mut limit,
            &mut assessor.remaining_work,
        )? && let Some(new) = spans(
            &groups[1],
            &literal.mandatory.new,
            &masks[1].source,
            &mut limit,
            &mut assessor.remaining_work,
        )? {
            output_ranges += old.len() + new.len();
            unit.mandatory_old = old;
            unit.mandatory_new = new;
            unit.search = SearchCompleteness::Complete;
        }
        units.push(unit);
    }
    Ok(units)
}

fn discover_hypothesis_domains(assessor: &mut Assessor<'_, '_>) -> Result<()> {
    for index in 0..assessor.alignment.spans.len() {
        let span = &assessor.alignment.spans[index];
        if span.old.is_empty() || span.new.is_empty() {
            continue;
        }
        let work = span.old.len().saturating_add(span.new.len());
        if !assessor.charge(work) {
            break;
        }
        let span = &assessor.alignment.spans[index];
        let normalization_issue =
            [&span.old, &span.new]
                .into_iter()
                .enumerate()
                .any(|(side, blocks)| {
                    blocks.iter().any(|block| {
                        !assessor.sides[side].blocks[assessor.sides[side].index[block]]
                            .issues
                            .is_empty()
                    })
                });
        if !normalization_issue {
            continue;
        }
        let proposal = ProposedRelation {
            old: super::full_relation_span(assessor.sides[0], &span.old, span.old_separator),
            new: super::full_relation_span(assessor.sides[1], &span.new, span.new_separator),
            span_indices: [Some(index), Some(index)],
            exact_recovery: false,
        };
        let key = assessor.domain_key(&proposal)?;
        if assessor.domains.contains_key(&key) {
            continue;
        }
        let groups = proof_groups(assessor.sides, &key)?;
        let Some(old_optional) = normalization::optional_tokens(
            assessor.sides[0],
            &groups[0],
            &mut assessor.remaining_work,
        )?
        else {
            continue;
        };
        let Some(new_optional) = normalization::optional_tokens(
            assessor.sides[1],
            &groups[1],
            &mut assessor.remaining_work,
        )?
        else {
            continue;
        };
        if !old_optional
            .iter()
            .chain(&new_optional)
            .any(|optional| *optional)
        {
            continue;
        }
        assessor.prove_domain(&key)?;
        if assessor.output_stop.is_some() {
            break;
        }
    }
    Ok(())
}

fn normalized_unit(
    assessor: &mut Assessor<'_, '_>,
    groups: &[GroupText; 2],
    masks: &[Masks; 2],
    mut unit: ReviewUnit,
    output_ranges: &mut usize,
) -> Result<Option<ReviewUnit>> {
    let Some(old_optional) = normalization::optional_tokens(
        assessor.sides[0],
        &groups[0],
        &mut assessor.remaining_work,
    )?
    else {
        return Ok(None);
    };
    let Some(new_optional) = normalization::optional_tokens(
        assessor.sides[1],
        &groups[1],
        &mut assessor.remaining_work,
    )?
    else {
        return Ok(None);
    };
    if !old_optional
        .iter()
        .chain(&new_optional)
        .any(|optional| *optional)
    {
        return Ok(None);
    }
    let proof = match hypotheses::universal_claims(
        hypotheses::HypothesisSide {
            tokens: &groups[0].tokens,
            optional: &old_optional,
            source: &masks[0].source,
            residual: &masks[0].unresolved,
        },
        hypotheses::HypothesisSide {
            tokens: &groups[1].tokens,
            optional: &new_optional,
            source: &masks[1].source,
            residual: &masks[1].unresolved,
        },
        &mut assessor.remaining_work,
    ) {
        Ok(Some(proof)) => proof,
        Ok(None) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut limit = assessor.options.max_assessment_ranges - *output_ranges;
    let mut projected = Vec::new();
    for (side, selected) in [
        (0, &old_optional),
        (1, &new_optional),
        (0, &proof.mandatory_old),
        (1, &proof.mandatory_new),
    ] {
        let Some(ranges) = spans(
            &groups[side],
            selected,
            &masks[side].source,
            &mut limit,
            &mut assessor.remaining_work,
        )?
        else {
            return Ok(None);
        };
        projected.push(ranges);
    }
    let [
        normalization_old,
        normalization_new,
        mandatory_old,
        mandatory_new,
    ]: [Vec<TextSpan>; 4] = projected
        .try_into()
        .expect("four source masks are projected");
    let mut relation = assessor.records[unit.relation].clone();
    // This separate relation quantifies over all supported interpretations.
    // The original canonical-only relation and its candidates remain tentative.
    relation.parent = None;
    relation.reasons.clear();
    relation.outcome = RelationOutcome::Established;
    relation.search = SearchCompleteness::Complete;
    relation
        .assumptions
        .push(ComparisonAssumption::AlternativeLineBreakNormalization);
    let relation_index = assessor.record(relation)?;
    if assessor.records[relation_index].outcome != RelationOutcome::Established {
        return Ok(None);
    }
    unit.relation = relation_index;
    unit.normalization_hypotheses = proof.completed_hypothesis_pairs;
    unit.normalization_old = normalization_old;
    unit.normalization_new = normalization_new;
    unit.mandatory_old = mandatory_old;
    unit.mandatory_new = mandatory_new;
    unit.changed_count = Some(EditCountBounds {
        lower: proof.source.lower,
        upper: proof.source.upper,
    });
    unit.unresolved_changed_count = Some(EditCountBounds {
        lower: proof.residual.lower,
        upper: proof.residual.upper,
    });
    unit.search = SearchCompleteness::Complete;
    *output_ranges = assessor.options.max_assessment_ranges - limit;
    Ok(Some(unit))
}

struct Masks {
    source: Vec<bool>,
    unresolved: Vec<bool>,
}

fn masks(side: &Side<'_>, group: &GroupText, partition: &[ResolutionRange]) -> Result<Masks> {
    let mut source = Vec::new();
    source
        .try_reserve_exact(group.tokens.len())
        .map_err(|_| allocation_error("claim source mask"))?;
    source.resize(group.tokens.len(), false);
    let mut unresolved = source.clone();
    let mut offset = 0usize;
    let mut preceding_space = false;
    for (position, block) in group.blocks.iter().enumerate() {
        let block_index = side.index[block];
        let tokens = &side.canonical[block_index];
        let separator = position > 0
            && group.separator.map(|separator| separator.at(position - 1))
                == Some(BlockSeparator::Space)
            && !preceding_space
            && !tokens.first().is_some_and(space_token);
        offset += usize::from(separator);
        let group_end = group.comparable_origin + group.tokens.len();
        let start = offset.max(group.comparable_origin);
        let end = (offset + tokens.len()).min(group_end);
        if start < end {
            source[start - group.comparable_origin..end - group.comparable_origin].fill(true);
        }
        for part in partition
            .iter()
            .filter(|part| part.block == *block && part.state == ResolutionState::Unresolved)
        {
            let start = (offset + part.comparable_range.start).max(group.comparable_origin);
            let end = (offset + part.comparable_range.end).min(group_end);
            if start < end {
                unresolved[start - group.comparable_origin..end - group.comparable_origin]
                    .fill(true);
            }
        }
        offset += tokens.len();
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, space_token);
    }
    Ok(Masks { source, unresolved })
}

fn spans(
    group: &GroupText,
    mandatory: &[bool],
    source: &[bool],
    limit: &mut usize,
    remaining: &mut usize,
) -> Result<Option<Vec<TextSpan>>> {
    if !super::charge(remaining, mandatory.len()) {
        return Ok(None);
    }
    let mut output = Vec::new();
    let mut index = 0;
    while index < mandatory.len() {
        if !mandatory[index] || !source[index] {
            index += 1;
            continue;
        }
        let start = index;
        while index < mandatory.len() && mandatory[index] && source[index] {
            index += 1;
        }
        if *limit == 0 || !super::charge(remaining, group.blocks.len()) {
            return Ok(None);
        }
        *limit -= 1;
        output
            .try_reserve(1)
            .map_err(|_| allocation_error("mandatory source ranges"))?;
        output.push(
            group
                .try_span(start, index)
                .ok_or_else(|| allocation_error("mandatory source span"))?,
        );
    }
    Ok(Some(output))
}
