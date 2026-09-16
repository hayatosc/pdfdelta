use crate::Result;

use super::super::{
    ChangeEvent, ChangeKind, ChangeOccurrence, Comparison, FormattingChange, ProvenChangedRegion,
    TextSpan, UnresolvedRegion, valid_change_occurrence_shape,
};
use super::{
    ComparisonAssessment, ResolutionRange, ResolutionState, allocation_error,
    block_scalar_boundary, invalid, project,
};

#[derive(Clone, Copy)]
struct PartitionInterval {
    start: usize,
    end: usize,
    canonical_end: usize,
    state: ResolutionState,
}

struct PartitionLookup {
    by_block: Vec<Vec<PartitionInterval>>,
}

#[derive(Clone, Copy)]
enum OwnershipRequirement {
    Any,
    Changed,
    Unresolved,
    Resolved,
}

/// Validates source-backed ownership for a finished comparison.
pub(super) fn validate(
    sides: [&super::super::Side<'_>; 2],
    assessment: &ComparisonAssessment,
    comparison: &Comparison,
) -> Result<()> {
    assessment.validate(comparison)?;
    let lookups = [
        build_partition_lookup(
            sides[0],
            &assessment.old_resolution,
            comparison.old_coverage,
        )?,
        build_partition_lookup(
            sides[1],
            &assessment.new_resolution,
            comparison.new_coverage,
        )?,
    ];

    for relation in &assessment.relations {
        if relation.old_span.is_none()
            && relation.new_span.is_none()
            && !is_output_limit_sentinel(relation)
        {
            return Err(invalid("assessment relations require a source range"));
        }
        validate_optional_span(
            sides[0],
            &lookups[0],
            relation.old_span.as_ref(),
            OwnershipRequirement::Any,
            "assessment relation",
        )?;
        validate_optional_span(
            sides[1],
            &lookups[1],
            relation.new_span.as_ref(),
            OwnershipRequirement::Any,
            "assessment relation",
        )?;
    }

    for unit in &assessment.review_units {
        let relation = &assessment.relations[unit.relation];
        let mut mandatory_count = 0usize;
        let mut source_count = 0usize;
        let mut optional_count = 0usize;
        for (side, ranges) in [&unit.mandatory_old, &unit.mandatory_new]
            .into_iter()
            .enumerate()
        {
            let parent = if side == 0 {
                relation.old_span.as_ref()
            } else {
                relation.new_span.as_ref()
            };
            if let Some(parent) = parent {
                for interval in project(sides[side], parent)? {
                    source_count += interval.end - interval.start;
                }
            }
            let mut seen = Vec::new();
            for span in ranges {
                if !super::contains_span(sides[side], parent, Some(span))? {
                    return Err(invalid(
                        "mandatory change range lies outside its review domain",
                    ));
                }
                for interval in project(sides[side], span)? {
                    mandatory_count += interval.end - interval.start;
                    seen.push(interval);
                }
            }
            let optional = if side == 0 {
                &unit.normalization_old
            } else {
                &unit.normalization_new
            };
            for span in optional {
                if !super::contains_span(sides[side], parent, Some(span))? {
                    return Err(invalid(
                        "normalization alternatives lie outside their domain",
                    ));
                }
                for interval in project(sides[side], span)? {
                    optional_count += interval.end - interval.start;
                    seen.push(interval);
                }
            }
            seen.sort_unstable_by_key(|interval| {
                (interval.block_index, interval.start, interval.end)
            });
            if seen.windows(2).any(|pair| {
                pair[0].block_index == pair[1].block_index && pair[0].end > pair[1].start
            }) {
                return Err(invalid("mandatory or optional source ranges overlap"));
            }
        }
        let hypotheses = u32::try_from(optional_count)
            .ok()
            .and_then(|count| 1usize.checked_shl(count));
        if hypotheses != Some(unit.normalization_hypotheses)
            || (optional_count > 0
                && !relation
                    .assumptions
                    .contains(&super::ComparisonAssumption::AlternativeLineBreakNormalization))
        {
            return Err(invalid(
                "review normalization premises do not match their hypothesis count",
            ));
        }
        if unit
            .changed_count
            .is_some_and(|bounds| mandatory_count > bounds.lower)
        {
            return Err(invalid(
                "mandatory change count exceeds the domain lower bound",
            ));
        }
        if unit
            .changed_count
            .is_some_and(|bounds| bounds.upper > source_count)
        {
            return Err(invalid("review count exceeds its source domain"));
        }
    }
    for change in &comparison.changes {
        validate_change_event(
            sides,
            &lookups,
            change,
            OwnershipRequirement::Changed,
            "accepted change",
        )?;
    }
    for candidate in &comparison.change_candidates {
        validate_change_event(
            sides,
            &lookups,
            &candidate.change,
            OwnershipRequirement::Unresolved,
            "change candidate",
        )?;
    }
    for region in &comparison.unresolved_regions {
        validate_unresolved_region(sides, &lookups, region)?;
    }
    for region in &comparison.proven_changed_regions {
        validate_proven_changed_region(sides, &lookups, region)?;
    }
    for formatting in &comparison.formatting_changes {
        validate_formatting_change(sides, &lookups, formatting)?;
    }
    Ok(())
}

fn build_partition_lookup(
    side: &super::super::Side<'_>,
    ranges: &[ResolutionRange],
    coverage: super::super::Coverage,
) -> Result<PartitionLookup> {
    let mut by_block: Vec<Vec<PartitionInterval>> = Vec::new();
    by_block
        .try_reserve_exact(side.blocks.len())
        .map_err(|_| allocation_error("partition block lookup"))?;
    by_block.resize_with(side.blocks.len(), Vec::new);

    let mut seen_blocks = Vec::new();
    seen_blocks
        .try_reserve_exact(side.blocks.len())
        .map_err(|_| allocation_error("partition block markers"))?;
    seen_blocks.resize(side.blocks.len(), false);

    let mut previous_block = None;
    let mut resolved_tokens = 0usize;
    for range in ranges {
        let Some(&block_index) = side.index.get(&range.block) else {
            return Err(invalid("assessment partition refers to an unknown block"));
        };
        let start = range.comparable_range.start;
        let end = range.comparable_range.end;
        if start >= end || end > side.canonical[block_index].len() {
            return Err(invalid("assessment partition exceeds actual block tokens"));
        }
        let canonical_start = range.canonical_range.start;
        let canonical_end = range.canonical_range.end;
        if canonical_start > canonical_end {
            return Err(invalid("assessment partition has reversed scalar bounds"));
        }
        let expected_canonical_start =
            block_scalar_boundary(&side.blocks[block_index].canonical, start);
        let expected_canonical_end =
            block_scalar_boundary(&side.blocks[block_index].canonical, end);
        if canonical_start != expected_canonical_start || canonical_end != expected_canonical_end {
            return Err(invalid(
                "assessment partition scalar bounds do not match source",
            ));
        }

        if previous_block != Some(block_index) {
            if seen_blocks[block_index] {
                return Err(invalid("assessment partition repeats a source block"));
            }
            seen_blocks[block_index] = true;
            if start != 0 || canonical_start != 0 {
                return Err(invalid("assessment partition has a block-local gap"));
            }
        } else if let Some(previous) = by_block[block_index].last()
            && (previous.end != start || previous.canonical_end != canonical_start)
        {
            return Err(invalid("assessment partition has an overlap or gap"));
        }

        let token_count = end - start;
        if range.state != ResolutionState::Unresolved {
            resolved_tokens = resolved_tokens
                .checked_add(token_count)
                .ok_or_else(|| invalid("assessment resolved token count overflow"))?;
        }
        by_block[block_index]
            .try_reserve(1)
            .map_err(|_| allocation_error("partition range lookup"))?;
        by_block[block_index].push(PartitionInterval {
            start,
            end,
            canonical_end,
            state: range.state,
        });
        previous_block = Some(block_index);
    }

    let mut actual_tokens = 0usize;
    for (block_index, tokens) in side.canonical.iter().enumerate() {
        actual_tokens = actual_tokens
            .checked_add(tokens.len())
            .ok_or_else(|| invalid("assessment source token count overflow"))?;
        if tokens.is_empty() {
            if !by_block[block_index].is_empty() {
                return Err(invalid("empty source blocks cannot own partition tokens"));
            }
            continue;
        }
        let Some(last) = by_block[block_index].last() else {
            return Err(invalid("assessment partition omits a source block"));
        };
        let expected_end = block_scalar_boundary(&side.blocks[block_index].canonical, tokens.len());
        if last.end != tokens.len() || last.canonical_end != expected_end {
            return Err(invalid(
                "assessment partition does not cover a source block",
            ));
        }
    }
    if coverage.total_tokens != actual_tokens || coverage.resolved_tokens != resolved_tokens {
        return Err(invalid(
            "assessment coverage does not match source partitions",
        ));
    }
    Ok(PartitionLookup { by_block })
}

fn validate_change_event(
    sides: [&super::super::Side<'_>; 2],
    lookups: &[PartitionLookup; 2],
    change: &ChangeEvent,
    requirement: OwnershipRequirement,
    context: &str,
) -> Result<()> {
    if change.occurrences.is_empty() {
        return Err(invalid("content changes require at least one occurrence"));
    }
    for occurrence in &change.occurrences {
        validate_change_occurrence_shape(change.kind, occurrence, context)?;
        validate_optional_span(
            sides[0],
            &lookups[0],
            occurrence.old_span.as_ref(),
            requirement,
            context,
        )?;
        validate_optional_span(
            sides[1],
            &lookups[1],
            occurrence.new_span.as_ref(),
            requirement,
            context,
        )?;
    }
    Ok(())
}

fn validate_change_occurrence_shape(
    kind: ChangeKind,
    occurrence: &ChangeOccurrence,
    context: &str,
) -> Result<()> {
    if !valid_change_occurrence_shape(
        kind,
        occurrence.old_span.as_ref(),
        occurrence.new_span.as_ref(),
    ) {
        return Err(invalid(context));
    }
    let old_empty = occurrence.old_span.as_ref().is_some_and(is_empty_span);
    let new_empty = occurrence.new_span.as_ref().is_some_and(is_empty_span);
    let empty_sides = usize::from(old_empty) + usize::from(new_empty);
    let valid_empty_sides = match kind {
        ChangeKind::Replacement => empty_sides <= 1,
        ChangeKind::Insertion | ChangeKind::Deletion | ChangeKind::Move => empty_sides == 0,
    };
    if !valid_empty_sides {
        return Err(invalid(
            "content change occurrences cannot own empty ranges",
        ));
    }
    Ok(())
}

fn validate_unresolved_region(
    sides: [&super::super::Side<'_>; 2],
    lookups: &[PartitionLookup; 2],
    region: &UnresolvedRegion,
) -> Result<()> {
    if region.old_span.is_none() && region.new_span.is_none() {
        return Err(invalid("unresolved regions require a source range"));
    }
    validate_optional_span(
        sides[0],
        &lookups[0],
        region.old_span.as_ref(),
        OwnershipRequirement::Unresolved,
        "unresolved region",
    )?;
    validate_optional_span(
        sides[1],
        &lookups[1],
        region.new_span.as_ref(),
        OwnershipRequirement::Unresolved,
        "unresolved region",
    )
}

fn validate_proven_changed_region(
    sides: [&super::super::Side<'_>; 2],
    lookups: &[PartitionLookup; 2],
    region: &ProvenChangedRegion,
) -> Result<()> {
    let old_nonempty = region
        .old_span
        .as_ref()
        .is_some_and(|span| !is_empty_span(span));
    let new_nonempty = region
        .new_span
        .as_ref()
        .is_some_and(|span| !is_empty_span(span));
    let valid_proof = region.proof.matches_span_shape(old_nonempty, new_nonempty);
    if !valid_proof {
        return Err(invalid(
            "proven changed region proof does not match its spans",
        ));
    }
    validate_optional_span(
        sides[0],
        &lookups[0],
        region.old_span.as_ref(),
        OwnershipRequirement::Unresolved,
        "proven changed region",
    )?;
    validate_optional_span(
        sides[1],
        &lookups[1],
        region.new_span.as_ref(),
        OwnershipRequirement::Unresolved,
        "proven changed region",
    )
}

fn validate_formatting_change(
    sides: [&super::super::Side<'_>; 2],
    lookups: &[PartitionLookup; 2],
    formatting: &FormattingChange,
) -> Result<()> {
    validate_span(
        sides[0],
        &lookups[0],
        &formatting.old_span,
        OwnershipRequirement::Resolved,
        "formatting change",
    )?;
    validate_span(
        sides[1],
        &lookups[1],
        &formatting.new_span,
        OwnershipRequirement::Resolved,
        "formatting change",
    )
}

fn validate_optional_span(
    side: &super::super::Side<'_>,
    lookup: &PartitionLookup,
    span: Option<&TextSpan>,
    requirement: OwnershipRequirement,
    context: &str,
) -> Result<()> {
    if let Some(span) = span {
        validate_span(side, lookup, span, requirement, context)?;
    }
    Ok(())
}

fn validate_span(
    side: &super::super::Side<'_>,
    lookup: &PartitionLookup,
    span: &TextSpan,
    requirement: OwnershipRequirement,
    context: &str,
) -> Result<()> {
    let projected = project(side, span)?;
    for interval in projected {
        let ranges = lookup
            .by_block
            .get(interval.block_index)
            .ok_or_else(|| invalid("assessment span refers to an unknown source block"))?;
        let mut range_index = ranges.partition_point(|range| range.end <= interval.start);
        let mut cursor = interval.start;
        while cursor < interval.end {
            let Some(owner) = ranges.get(range_index) else {
                return Err(invalid(context));
            };
            if owner.start > cursor
                || owner.end <= cursor
                || !state_satisfies(owner.state, requirement)
            {
                return Err(invalid(context));
            }
            cursor = owner.end.min(interval.end);
            range_index += 1;
        }
    }
    Ok(())
}

fn state_satisfies(state: ResolutionState, requirement: OwnershipRequirement) -> bool {
    match requirement {
        OwnershipRequirement::Any => true,
        OwnershipRequirement::Changed => state == ResolutionState::Changed,
        OwnershipRequirement::Unresolved => state == ResolutionState::Unresolved,
        OwnershipRequirement::Resolved => state != ResolutionState::Unresolved,
    }
}

fn is_empty_span(span: &TextSpan) -> bool {
    span.comparable_range.start == span.comparable_range.end
}

pub(super) fn is_output_limit_sentinel(relation: &super::RelationAssessment) -> bool {
    relation.parent.is_none()
        && relation.outcome == super::RelationOutcome::Tentative
        && relation.search == super::SearchCompleteness::Incomplete
        && relation.reasons.len() == 1
        && relation.reasons[0] == super::AssessmentReason::OutputLimit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{Change, Confidence, Coverage, TextSpan, TokenRange},
        layout::{BlockId, BlockRole},
        model::FontProgramHash,
        normalize::{
            BlockText, ComparableToken, MappedText, ScalarRange, TextSource, UnmappedToken,
        },
    };

    fn block(id: u64, text: &str) -> BlockText {
        let canonical = MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        };
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: text.to_owned(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    fn unmapped_block(id: u64, scalar: char) -> BlockText {
        let canonical = MappedText {
            text: scalar.to_string(),
            source_map: Vec::new(),
            unmapped: vec![UnmappedToken {
                scalar_index: 0,
                font_hash: FontProgramHash(vec![1]),
                glyph_id: 7,
                source: TextSource {
                    atoms: Default::default(),
                },
            }],
        };
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: scalar.to_string(),
            matching_tokens: vec![
                ComparableToken::Unmapped {
                    font_hash: FontProgramHash(vec![1]),
                    glyph_id: 7,
                },
                ComparableToken::Scalar(scalar),
            ],
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    fn side(blocks: &[BlockText]) -> super::super::super::Side<'_> {
        super::super::super::SidePlan::inspect("test", blocks)
            .expect("test blocks are valid")
            .materialize()
            .expect("test blocks materialize")
    }

    fn span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn range(block: u64, start: usize, end: usize, state: ResolutionState) -> ResolutionRange {
        ResolutionRange {
            block: BlockId(block),
            comparable_range: TokenRange { start, end },
            canonical_range: ScalarRange { start, end },
            state,
        }
    }

    fn assessment(
        old_resolution: Vec<ResolutionRange>,
        new_resolution: Vec<ResolutionRange>,
    ) -> ComparisonAssessment {
        ComparisonAssessment {
            localized_edits: Vec::new(),
            review_units: Vec::new(),
            policy_version: super::super::ASSESSMENT_POLICY_VERSION,
            relations: Vec::new(),
            old_resolution,
            new_resolution,
            work_limit: 10,
            work_used: 0,
            work_by_stage: Default::default(),
            candidates_truncated: false,
        }
    }

    fn comparison(total_tokens: usize, resolved_tokens: usize) -> Comparison {
        Comparison {
            change_candidates: Vec::new(),
            assessment: None,
            changes: Vec::new(),
            proven_changed_regions: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions: Vec::new(),
            old_coverage: Coverage {
                resolved_tokens,
                total_tokens,
                ratio: None,
            },
            new_coverage: Coverage {
                resolved_tokens,
                total_tokens,
                ratio: None,
            },
        }
    }

    #[test]
    fn overlapping_candidate_and_accepted_change_is_rejected() {
        let old_blocks = [block(1, "abc")];
        let new_blocks = [block(2, "axc")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let changed_old = span(1, 1, 2);
        let changed_new = span(2, 1, 2);
        let accepted = Change::single_occurrence(
            ChangeKind::Replacement,
            Some(changed_old.clone()),
            Some(changed_new.clone()),
            Confidence::High,
            Vec::new(),
        );
        let candidate = Change {
            kind: ChangeKind::Replacement,
            occurrences: vec![ChangeOccurrence {
                old_span: Some(changed_old),
                new_span: Some(changed_new),
            }],
            confidence: Confidence::Low,
            tags: Vec::new(),
        };
        let mut comparison = comparison(3, 3);
        comparison.changes.push(accepted);
        comparison
            .change_candidates
            .push(super::super::ChangeCandidate {
                change: candidate,
                relation: 0,
                alternative_group: 0,
            });
        let assessment = ComparisonAssessment {
            localized_edits: Vec::new(),
            review_units: Vec::new(),
            policy_version: super::super::ASSESSMENT_POLICY_VERSION,
            relations: vec![super::super::RelationAssessment {
                old_span: Some(span(1, 0, 3)),
                new_span: Some(span(2, 0, 3)),
                parent: None,
                outcome: super::super::RelationOutcome::Tentative,
                search: super::super::SearchCompleteness::Complete,
                assumptions: Vec::new(),
                reasons: vec![super::super::AssessmentReason::AmbiguousEditLocation],
            }],
            old_resolution: vec![
                range(1, 0, 1, ResolutionState::Equal),
                ResolutionRange {
                    block: BlockId(1),
                    comparable_range: TokenRange { start: 1, end: 2 },
                    canonical_range: ScalarRange { start: 1, end: 2 },
                    state: ResolutionState::Changed,
                },
                range(1, 2, 3, ResolutionState::Equal),
            ],
            new_resolution: vec![
                range(2, 0, 1, ResolutionState::Equal),
                ResolutionRange {
                    block: BlockId(2),
                    comparable_range: TokenRange { start: 1, end: 2 },
                    canonical_range: ScalarRange { start: 1, end: 2 },
                    state: ResolutionState::Changed,
                },
                range(2, 2, 3, ResolutionState::Equal),
            ],
            work_limit: 10,
            work_used: 0,
            work_by_stage: Default::default(),
            candidates_truncated: false,
        };
        assert!(validate([&old, &new], &assessment, &comparison).is_err());
    }

    #[test]
    fn partition_must_match_actual_block_length() {
        let old_blocks = [block(1, "abc")];
        let new_blocks = [block(2, "abc")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let assessment = assessment(
            vec![range(1, 0, 2, ResolutionState::Equal)],
            vec![range(2, 0, 3, ResolutionState::Equal)],
        );
        let comparison = comparison(2, 2);
        assert!(validate([&old, &new], &assessment, &comparison).is_err());
    }

    #[test]
    fn unmapped_zero_scalar_partition_boundary_is_valid() {
        let old_blocks = [unmapped_block(1, 'a')];
        let new_blocks = [unmapped_block(2, 'a')];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let assessment = assessment(
            vec![
                ResolutionRange {
                    block: BlockId(1),
                    comparable_range: TokenRange { start: 0, end: 1 },
                    canonical_range: ScalarRange { start: 0, end: 0 },
                    state: ResolutionState::Equal,
                },
                ResolutionRange {
                    block: BlockId(1),
                    comparable_range: TokenRange { start: 1, end: 2 },
                    canonical_range: ScalarRange { start: 0, end: 1 },
                    state: ResolutionState::Changed,
                },
            ],
            vec![
                ResolutionRange {
                    block: BlockId(2),
                    comparable_range: TokenRange { start: 0, end: 1 },
                    canonical_range: ScalarRange { start: 0, end: 0 },
                    state: ResolutionState::Equal,
                },
                ResolutionRange {
                    block: BlockId(2),
                    comparable_range: TokenRange { start: 1, end: 2 },
                    canonical_range: ScalarRange { start: 0, end: 1 },
                    state: ResolutionState::Changed,
                },
            ],
        );
        let comparison = comparison(2, 2);
        validate([&old, &new], &assessment, &comparison).expect("valid unmapped partition");
    }

    #[test]
    fn only_output_limit_may_omit_both_relation_spans() {
        let old_blocks: [BlockText; 0] = [];
        let new_blocks: [BlockText; 0] = [];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let sentinel = super::super::RelationAssessment {
            old_span: None,
            new_span: None,
            parent: None,
            outcome: super::super::RelationOutcome::Tentative,
            search: super::super::SearchCompleteness::Incomplete,
            assumptions: Vec::new(),
            reasons: vec![super::super::AssessmentReason::OutputLimit],
        };
        let assessment = ComparisonAssessment {
            localized_edits: Vec::new(),
            review_units: Vec::new(),
            policy_version: super::super::ASSESSMENT_POLICY_VERSION,
            relations: vec![sentinel.clone()],
            old_resolution: Vec::new(),
            new_resolution: Vec::new(),
            work_limit: 1,
            work_used: 0,
            work_by_stage: Default::default(),
            candidates_truncated: true,
        };
        let comparison = comparison(0, 0);
        validate([&old, &new], &assessment, &comparison)
            .expect("output-limit sentinel is source-neutral");

        let mut invalid_assessment = assessment;
        invalid_assessment.relations[0] = super::super::RelationAssessment {
            reasons: vec![super::super::AssessmentReason::WorkLimit],
            ..sentinel
        };
        assert!(validate([&old, &new], &invalid_assessment, &comparison).is_err());
    }
}
