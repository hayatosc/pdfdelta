//! Optional literal ownership after all earlier proof consumers have finished.
use super::{
    Assessor, COARSE_MEMORY_BYTES, DomainKey, DomainProof, EqualConstraints, EqualRun,
    LiteralVetoIndex, Ownership, ResolutionRange, ResolutionState, Result, TailResources,
    borrowed_group_pair, diagnostic_allowance, equal_fragment, light_group_metadata,
    plan_group_materialization, publication_memory,
};

#[cfg(test)]
mod tests;

/// Checks the recorded common refinement without treating its metadata as a
/// replacement for the native-source certificate used during generation.
pub(in crate::diff::assessment) fn compatible_refined_child(
    sides: [&super::Side<'_>; 2],
    relations: &[super::RelationAssessment],
    child_index: usize,
) -> bool {
    use super::{ComparableToken, ComparisonAssumption as A, RelationOutcome, SearchCompleteness};
    let child = &relations[child_index];
    let Some(parent_index) = child.parent.filter(|i| *i < child_index) else {
        return false;
    };
    let parent = &relations[parent_index];
    if parent.parent.is_some()
        || [parent, child].into_iter().any(|r| {
            r.outcome != RelationOutcome::Established
                || r.search != SearchCompleteness::Complete
                || !r.reasons.is_empty()
                || !r.assumptions.contains(&A::LocalEvidenceBoundaries)
                || r.assumptions
                    .contains(&A::AlternativeLineBreakNormalization)
                || r.assumptions
                    .contains(&A::PageShiftedMandatoryMatchingEquality)
        })
        || !child.assumptions.contains(&A::MandatoryMatchingEquality)
        || parent.assumptions.contains(&A::RefinedExactParentEquality)
    {
        return false;
    }
    let mut texts = [Vec::new(), Vec::new()];
    let mut selected = [None, None];
    for side in 0..2 {
        let (Some(outer), Some(inner)) = (if side == 0 {
            (&parent.old_span, &child.old_span)
        } else {
            (&parent.new_span, &child.new_span)
        }) else {
            return false;
        };
        if outer.blocks.len() < 2
            || outer.comparable_range.start != 0
            || outer.canonical_range.start != 0
            || outer.comparable_range.end == 0
            || outer.comparable_range.end > 512
            || outer.canonical_range.end != outer.comparable_range.end
            || inner.blocks.len() != 1
            || inner.separator.is_some()
            || inner.comparable_range.start >= inner.comparable_range.end
            || inner.canonical_range.start != inner.comparable_range.start
            || inner.canonical_range.end != inner.comparable_range.end
        {
            return false;
        }
        if texts[side]
            .try_reserve_exact(outer.comparable_range.end)
            .is_err()
        {
            return false;
        }
        let mut previous = None;
        let mut page = None;
        for (position, block) in outer.blocks.iter().enumerate() {
            let Some(&index) = sides[side].index.get(block) else {
                return false;
            };
            let source = &sides[side].blocks[index];
            let [this_page] = source.pages.as_slice() else {
                return false;
            };
            if previous.is_some_and(|p| p >= index)
                || page.is_some_and(|p| p != *this_page)
                || !source.canonical.unmapped.is_empty()
                || !source.raw.unmapped.is_empty()
            {
                return false;
            }
            previous = Some(index);
            page = Some(*this_page);
            let tokens = &sides[side].canonical[index];
            let space =
                |t: &ComparableToken| matches!(t, ComparableToken::Scalar(c) if c.is_whitespace());
            if position > 0
                && outer
                    .separator
                    .is_some_and(|s| s.at(position - 1) == crate::alignment::BlockSeparator::Space)
                && !texts[side].last().is_some_and(space)
                && !tokens.first().is_some_and(space)
            {
                if texts[side].len() == outer.comparable_range.end {
                    return false;
                }
                texts[side].push(ComparableToken::Scalar(' '));
            }
            if inner.blocks[0] == *block {
                let Some(run) =
                    tokens.get(inner.comparable_range.start..inner.comparable_range.end)
                else {
                    return false;
                };
                if run
                    .iter()
                    .any(|t| !matches!(t, ComparableToken::Scalar(c) if !c.is_whitespace()))
                {
                    return false;
                }
                selected[side] = Some((
                    texts[side].len() + inner.comparable_range.start,
                    texts[side].len() + inner.comparable_range.end,
                ));
            }
            if texts[side].len().saturating_add(tokens.len()) > outer.comparable_range.end {
                return false;
            }
            texts[side].extend_from_slice(tokens);
        }
        if selected[side].is_none() || texts[side].len() != outer.comparable_range.end {
            return false;
        }
    }
    selected[0] == selected[1] && texts[0] == texts[1]
}

impl Assessor<'_, '_> {
    /// Refines only complete exact equal roots. Every source run is certified
    /// before any new ownership; latest ownership and all competing/coarse
    /// obligations are then checked by the original atomic publication path.
    pub(in crate::diff::assessment) fn recover_refined_equal_parents(
        &mut self,
        domains: &[(DomainKey, DomainProof)],
        domain_bytes: usize,
        ownership: &mut [Ownership; 2],
        mut partitions: [&mut Vec<ResolutionRange>; 2],
        constraints: EqualConstraints<'_>,
    ) -> Result<bool> {
        if self.remaining_work == 0 || self.output_stop.is_some() {
            return Ok(false);
        }
        let mut resources = None;
        let mut adopted = false;
        for (key, proof) in domains {
            if !self.charge(32usize.saturating_add(self.records[proof.relation].assumptions.len()))
            {
                break;
            }
            if !equal_fragment::eligible_refinement(key, proof, &self.records[proof.relation]) {
                continue;
            }
            let mut unresolved = [false; 2];
            for side in 0..2 {
                let extent = if side == 0 { &key.old } else { &key.new };
                for part in &*partitions[side] {
                    if !self.charge(8) {
                        return Ok(adopted);
                    }
                    if part.state == ResolutionState::Unresolved
                        && self.sides[side]
                            .index
                            .get(&part.block)
                            .is_some_and(|i| extent.contains(i))
                    {
                        unresolved[side] = true;
                        break;
                    }
                }
            }
            if !unresolved.into_iter().all(|v| v) {
                continue;
            }
            if resources.is_none() {
                let Some(source_bytes) = self.cached_literal_source_capacity_bound() else {
                    break;
                };
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
                    break;
                };
                let Some(diagnostics) = diagnostic_allowance(
                    self,
                    [&*partitions[0], &*partitions[1]],
                    constraints.original,
                    available,
                ) else {
                    break;
                };
                let Some(available) = diagnostics.bytes().and_then(|n| available.checked_sub(n))
                else {
                    break;
                };
                let veto_index = LiteralVetoIndex::build(
                    constraints,
                    self.sides,
                    available,
                    &mut self.remaining_work,
                );
                let source_bytes =
                    source_bytes.saturating_add(veto_index.as_ref().map_or(0, |index| index.bytes));
                resources = Some(TailResources {
                    veto_index,
                    source_bytes,
                    retained_children: 0,
                    group_peak_bytes: 0,
                    diagnostics,
                });
            }
            let resources = resources.as_mut().expect("initialized tail resources");
            let Some(available) = publication_memory(
                domain_bytes,
                resources.source_bytes,
                resources.retained_children,
                &self.records,
                self.records.capacity(),
                ownership,
                [&*partitions[0], &*partitions[1]],
            )
            .and_then(|n| n.checked_add(resources.diagnostics.bytes()?))
            .and_then(|n| n.checked_add(resources.group_peak_bytes))
            .and_then(|n| COARSE_MEMORY_BYTES.checked_sub(n)) else {
                break;
            };
            let cache = self
                .equal_fragment_cache
                .get_or_insert_with(|| equal_fragment::EqualFragmentCache::new(self.sides));
            let Some(certificate) = cache.prove_refined_equal_parent(
                key,
                proof,
                &self.records[proof.relation],
                available,
                &mut self.remaining_work,
            )?
            else {
                continue;
            };
            for run in certificate.runs() {
                if self.remaining_work == 0
                    || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
                {
                    break;
                }
                let spans = [&run.spans[0], &run.spans[1]];
                let Some(retained) = domain_bytes.checked_add(certificate.bytes()) else {
                    break;
                };
                let Some(available) = publication_memory(
                    retained,
                    resources.source_bytes,
                    resources.retained_children,
                    &self.records,
                    self.records.capacity(),
                    ownership,
                    [&*partitions[0], &*partitions[1]],
                )
                .and_then(|n| n.checked_add(resources.diagnostics.bytes()?))
                .and_then(|n| n.checked_add(resources.group_peak_bytes))
                .and_then(|n| COARSE_MEMORY_BYTES.checked_sub(n)) else {
                    break;
                };
                let mut groups = [None, None];
                for (side, span) in spans.into_iter().enumerate() {
                    let index = self.sides[side].index[&span.blocks[0]];
                    let Ok(metadata) = light_group_metadata(
                        self.sides[side],
                        index..index + 1,
                        crate::alignment::BlockSeparator::Concatenate,
                        &mut self.remaining_work,
                        available,
                    ) else {
                        break;
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
                let [Some(a), Some(b)] = groups else {
                    continue;
                };
                let groups = [a, b];
                let group_bytes = groups[0].bytes.max(groups[1].bytes);
                let Some(available) = available.checked_sub(group_bytes) else {
                    continue;
                };
                let Ok(views) =
                    borrowed_group_pair(self.sides, spans, &mut self.remaining_work, available)
                else {
                    continue;
                };
                let Some(retained) = retained
                    .checked_add(group_bytes)
                    .and_then(|n| n.checked_add(views[0].retained_bytes))
                    .and_then(|n| n.checked_add(views[1].retained_bytes))
                else {
                    continue;
                };
                let length = run.parent_end - run.parent_start;
                adopted |= self.commit_mandatory_coarse_equal_run(
                    EqualRun {
                        refined_parent: Some(&certificate),
                        page_shifted: false,
                        parent: proof.relation,
                        region: usize::MAX,
                        views: &views,
                        ranges: [0..length, 0..length],
                        domain_bytes: retained,
                        groups: &groups,
                    },
                    ownership,
                    &mut partitions,
                    constraints,
                    resources,
                    None,
                )?;
            }
        }
        Ok(adopted)
    }
}
