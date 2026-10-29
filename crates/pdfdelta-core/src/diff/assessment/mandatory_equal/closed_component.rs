//! Composition of a forced initial component with its unchanged coarse parent.
use super::super::TextSpan;
use super::{
    Assessor, BlockId, COARSE_MEMORY_BYTES, ComparableToken, ComparisonAssumption,
    EqualConstraints, EqualRun, LiteralVetoIndex, Ownership, ProvenChangedRegion,
    RelationAssessment, RelationOutcome, ResolutionRange, Result, SearchCompleteness, Side,
    TailResources, borrowed_group_pair, diagnostic_allowance, light_group_metadata,
    plan_group_materialization, publication_memory, semantic, shallow_words,
};

/// Constructed only after the original whole parent's exact scope, literal
/// source guard, prefix identity and two mandatory right-boundary pairs pass.
/// The analysis is the full parent's completed analysis, never a selected path.
pub(in crate::diff::assessment) struct ClosedComponentProof<'a> {
    pub parent: usize,
    pub spans: [&'a TextSpan; 2],
    pub analysis: &'a semantic::MandatoryMatchAnalysis,
    /// All still-live reviewed domains, parent views and matching arrays.
    pub retained_bytes: usize,
}

struct ComponentAttempt<'a, 'b> {
    proof: &'a ClosedComponentProof<'b>,
    child: usize,
    region: usize,
    domain_bytes: usize,
    source_bytes: usize,
}

/// The only extra coarse exception is the exact ancestor of a certified
/// initial component. Containment alone, another coarse scope, page shifts,
/// partial components and dirty ancestors do not satisfy this predicate.
pub(super) fn matches_ancestor(
    sides: [&Side<'_>; 2],
    component: &RelationAssessment,
    ancestor: &RelationAssessment,
    region: &ProvenChangedRegion,
) -> bool {
    let clean = |record: &RelationAssessment| {
        record.outcome == RelationOutcome::Established
            && record.search == SearchCompleteness::Complete
            && record.reasons.is_empty()
            && !record
                .assumptions
                .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
    };
    if !clean(component)
        || !clean(ancestor)
        || ancestor.parent.is_some()
        || !component
            .assumptions
            .contains(&ComparisonAssumption::MandatoryInitialComponentBoundary)
        || region.proof != super::super::super::ChangedRegionProof::ExactTokenMultisetMismatch
        || region.old_span != ancestor.old_span
        || region.new_span != ancestor.new_span
    {
        return false;
    }
    let mut pages = [None, None];
    for (side, (inner, outer)) in [
        (component.old_span.as_ref(), ancestor.old_span.as_ref()),
        (component.new_span.as_ref(), ancestor.new_span.as_ref()),
    ]
    .into_iter()
    .enumerate()
    {
        let (Some(inner), Some(outer)) = (inner, outer) else {
            return false;
        };
        let [block] = inner.blocks.as_slice() else {
            return false;
        };
        let Some(&index) = sides[side].index.get(block) else {
            return false;
        };
        let source = &sides[side].blocks[index];
        let [page] = source.pages.as_slice() else {
            return false;
        };
        if outer.blocks.len() < 2
            || outer.blocks.first() != Some(block)
            || outer.comparable_range.start != 0
            || outer.canonical_range.start != 0
            || inner.separator.is_some()
            || inner.comparable_range.start != 0
            || inner.comparable_range.end != sides[side].canonical[index].len()
            || inner.canonical_range.start != 0
            || inner.canonical_range.end != inner.comparable_range.end
            || inner.comparable_range.end == 0
            || outer.comparable_range.end < inner.comparable_range.end.saturating_add(2)
            || !source.raw.unmapped.is_empty()
            || !source.canonical.unmapped.is_empty()
        {
            return false;
        }
        pages[side] = Some(*page);
    }
    pages[0] == pages[1]
}

impl Assessor<'_, '_> {
    /// Keeps the parent's coarse fact and every other veto. A component header
    /// is published only together with at least one strict literal equality.
    pub(in crate::diff::assessment) fn recover_closed_component_equalities(
        &mut self,
        proof: ClosedComponentProof<'_>,
        ownership: &mut [Ownership; 2],
        mut partitions: [&mut Vec<ResolutionRange>; 2],
        constraints: EqualConstraints<'_>,
    ) -> Result<bool> {
        let cuts = proof.spans.map(|span| span.comparable_range.end);
        if self.output_stop.is_some()
            || self.records.len().saturating_add(2) >= self.options.max_assessment_ranges
            || !self.charge(128)
            || proof.analysis.mandatory_diagonal_count(cuts[0], cuts[1], 2) != 2
        {
            return Ok(false);
        }
        let parent = &self.records[proof.parent];
        let count = parent.assumptions.len().saturating_add(1);
        let ids = parent
            .old_span
            .as_ref()
            .map_or(0, |s| s.blocks.len())
            .saturating_add(parent.new_span.as_ref().map_or(0, |s| s.blocks.len()));
        let growth = if self.records.len() == self.records.capacity() {
            self.records
                .len()
                .saturating_mul(shallow_words::<RelationAssessment>())
        } else {
            0
        };
        if !self.charge(
            count
                .saturating_add(growth)
                .saturating_add(32)
                .saturating_add(
                    constraints.regions.len().saturating_mul(
                        ids.saturating_add(count.saturating_mul(2))
                            .saturating_add(4 * usize::BITS as usize),
                    ),
                ),
        ) {
            return Ok(false);
        }
        let Some(mut assumptions) =
            super::super::super::try_copy_slice(&self.records[proof.parent].assumptions)
        else {
            return Ok(false);
        };
        if assumptions.try_reserve_exact(1).is_err() {
            return Ok(false);
        }
        assumptions.push(ComparisonAssumption::MandatoryInitialComponentBoundary);
        let mut copies = [None, None];
        for (side, span) in proof.spans.into_iter().enumerate() {
            let Some(blocks) = super::super::super::try_copy_slice(&span.blocks) else {
                return Ok(false);
            };
            copies[side] = Some(TextSpan {
                blocks,
                separator: span.separator,
                canonical_range: span.canonical_range,
                comparable_range: span.comparable_range,
            });
        }
        let [old_span, new_span] = copies;
        let component = RelationAssessment {
            old_span,
            new_span,
            parent: Some(proof.parent),
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions,
            reasons: Vec::new(),
        };
        let mut region_index = None;
        for (index, region) in constraints.regions.iter().enumerate() {
            if matches_ancestor(self.sides, &component, &self.records[proof.parent], region)
                && region_index.replace(index).is_some()
            {
                return Ok(false);
            }
        }
        let Some(region_index) = region_index else {
            return Ok(false);
        };
        // The validator deliberately refuses duplicate first-component keys.
        let ids = proof.spans.map(|span| span.blocks[0]);
        if constraints
            .regions
            .iter()
            .filter(|region| {
                region
                    .old_span
                    .as_ref()
                    .is_some_and(|s| s.blocks.first() == Some(&ids[0]))
                    && region
                        .new_span
                        .as_ref()
                        .is_some_and(|s| s.blocks.first() == Some(&ids[1]))
            })
            .count()
            != 1
        {
            return Ok(false);
        }
        let Some(domain_bytes) = proof.retained_bytes.checked_add(
            component
                .assumptions
                .capacity()
                .saturating_mul(std::mem::size_of::<ComparisonAssumption>())
                .saturating_add(2 * std::mem::size_of::<BlockId>()),
        ) else {
            return Ok(false);
        };
        let Some(source_bytes) = self.cached_literal_source_capacity_bound() else {
            return Ok(false);
        };
        if publication_memory(
            domain_bytes,
            source_bytes,
            0,
            &self.records,
            self.records.capacity(),
            ownership,
            [&*partitions[0], &*partitions[1]],
        )
        .is_none_or(|bytes| {
            bytes.saturating_add(std::mem::size_of::<RelationAssessment>()) > COARSE_MEMORY_BYTES
        }) || self.records.try_reserve_exact(1).is_err()
        {
            return Ok(false);
        }
        let child = self.records.len();
        self.records.push(component);
        let result = self.try_closed_component_runs(
            ComponentAttempt {
                proof: &proof,
                child,
                region: region_index,
                domain_bytes,
                source_bytes,
            },
            ownership,
            &mut partitions,
            constraints,
        );
        let adopted = self.records.len() > child + 1;
        if !adopted {
            self.records.pop();
        }
        result.map(|()| adopted)
    }

    fn try_closed_component_runs(
        &mut self,
        attempt: ComponentAttempt<'_, '_>,
        ownership: &mut [Ownership; 2],
        partitions: &mut [&mut Vec<ResolutionRange>; 2],
        constraints: EqualConstraints<'_>,
    ) -> Result<()> {
        let ComponentAttempt {
            proof,
            child,
            region,
            domain_bytes,
            source_bytes,
        } = attempt;
        let Some(available) = publication_memory(
            domain_bytes,
            source_bytes,
            0,
            &self.records,
            self.records.capacity(),
            ownership,
            [&*partitions[0], &*partitions[1]],
        )
        .and_then(|n| COARSE_MEMORY_BYTES.checked_sub(n)) else {
            return Ok(());
        };
        let Some(diagnostics) = diagnostic_allowance(
            self,
            [&*partitions[0], &*partitions[1]],
            constraints.original,
            available,
        ) else {
            return Ok(());
        };
        let Some(available) = diagnostics.bytes().and_then(|n| available.checked_sub(n)) else {
            return Ok(());
        };
        let mut resources = TailResources {
            veto_index: None,
            source_bytes,
            retained_children: 0,
            group_peak_bytes: 0,
            diagnostics,
        };
        if let Some(index) =
            LiteralVetoIndex::build(constraints, self.sides, available, &mut self.remaining_work)
        {
            resources.source_bytes = resources.source_bytes.saturating_add(index.bytes);
            resources.veto_index = Some(index);
        }
        let Some(available) = available.checked_sub(resources.source_bytes - source_bytes) else {
            return Ok(());
        };
        let mut groups = [None, None];
        for (side, span) in proof.spans.into_iter().enumerate() {
            let index = self.sides[side].index[&span.blocks[0]];
            let Ok(metadata) = light_group_metadata(
                self.sides[side],
                index..index + 1,
                super::super::BlockSeparator::Concatenate,
                &mut self.remaining_work,
                available,
            ) else {
                return Ok(());
            };
            groups[side] = plan_group_materialization(
                self.sides[side],
                index..index + 1,
                &metadata,
                &mut self.remaining_work,
                available,
            )
            .ok();
        }
        let [Some(old), Some(new)] = groups else {
            return Ok(());
        };
        let groups = [old, new];
        let group_bytes = groups[0].bytes.max(groups[1].bytes);
        let Some(available) = available.checked_sub(group_bytes) else {
            return Ok(());
        };
        let Ok(views) =
            borrowed_group_pair(self.sides, proof.spans, &mut self.remaining_work, available)
        else {
            return Ok(());
        };
        let Some(domain_bytes) = domain_bytes
            .checked_add(group_bytes)
            .and_then(|n| n.checked_add(views[0].retained_bytes))
            .and_then(|n| n.checked_add(views[1].retained_bytes))
        else {
            return Ok(());
        };
        let tokens = [views[0].selected_tokens(), views[1].selected_tokens()];
        for fallback in [false, true] {
            if !self.charge(proof.analysis.pairs().len().saturating_mul(4)) {
                break;
            }
            let pairs = proof.analysis.pairs();
            let mut start = 0;
            while start < pairs.len() {
                if self.remaining_work == 0
                    || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
                {
                    break;
                }
                let first = pairs[start];
                if first.0 > tokens[0].len() || first.1 > tokens[1].len() {
                    break;
                }
                if !matches!(tokens[0][first.0 - 1], ComparableToken::Scalar(c) if !c.is_whitespace())
                {
                    start += 1;
                    continue;
                }
                let mut end = start + 1;
                while end < pairs.len()
                    && pairs[end].0 <= tokens[0].len()
                    && pairs[end].1 <= tokens[1].len()
                    && pairs[end] == (pairs[end - 1].0 + 1, pairs[end - 1].1 + 1)
                    && matches!(tokens[0][pairs[end].0 - 1], ComparableToken::Scalar(c) if !c.is_whitespace())
                {
                    end += 1;
                }
                let last = pairs[end - 1];
                let first_index = start;
                start = end;
                if fallback && end - first_index == 1 {
                    continue;
                }
                let mut source_held = false;
                self.commit_mandatory_coarse_equal_run(
                    EqualRun {
                        refined_parent: None,
                        page_shifted: false,
                        parent: child,
                        region,
                        views: &views,
                        ranges: [first.0 - 1..last.0, first.1 - 1..last.1],
                        domain_bytes,
                        groups: &groups,
                    },
                    ownership,
                    partitions,
                    constraints,
                    &mut resources,
                    fallback.then_some(&mut source_held),
                )?;
                if fallback && source_held {
                    for &(a, b) in &pairs[first_index..end] {
                        if self.remaining_work == 0
                            || self.records.len()
                                >= self.options.max_assessment_ranges.saturating_sub(1)
                        {
                            break;
                        }
                        self.commit_mandatory_coarse_equal_run(
                            EqualRun {
                                refined_parent: None,
                                page_shifted: false,
                                parent: child,
                                region,
                                views: &views,
                                ranges: [a - 1..a, b - 1..b],
                                domain_bytes,
                                groups: &groups,
                            },
                            ownership,
                            partitions,
                            constraints,
                            &mut resources,
                            None,
                        )?;
                    }
                }
                if self.remaining_work == 0 {
                    break;
                }
            }
        }
        Ok(())
    }
}
