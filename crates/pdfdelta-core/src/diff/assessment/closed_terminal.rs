//! Literal prefixes inside a terminal source component with a forced left cut.
//!
//! Two mandatory matching pairs at the first real source tokens fix the left
//! point on every optimal path. The exact source-closed parent's end must
//! coincide with the original terminal component's end. Ambiguous edits remain
//! unresolved; only a wholly protected, source-backed mandatory prefix may gain
//! equality ownership.

use super::closed_child::{MAX_PARENT_TOKENS, component_span, copy_span, eligible_header};
use super::{
    Assessor, COARSE_MEMORY_BYTES, ChangeCandidate, ComparableToken, ComparisonAssumption, Error,
    Ownership, ProvenChangedRegion, RelationAssessment, RelationOutcome, ResolutionRange, Result,
    SearchCompleteness, TextSpan, borrowed_group_pair, charge_work, coarse_reviewed_domain_bytes,
    coarse_source_guard, equal_fragment, light_group_metadata, local, semantic,
};

fn eligible_terminal_header(
    key: &super::DomainKey,
    proof: &super::DomainProof,
    record: &RelationAssessment,
) -> bool {
    if key.local.is_none() {
        return eligible_header(key, proof, record);
    }
    key.old.len() > 1
        && key.new.len() > 1
        && proof.scope == super::ProofScope::ExactKey
        && proof.search == SearchCompleteness::Complete
        && proof.lengths.iter().all(|n| *n <= MAX_PARENT_TOKENS)
        && record.parent.is_none()
        && record.outcome == RelationOutcome::Established
        && record.search == SearchCompleteness::Complete
        && record.reasons.is_empty()
        && !record
            .assumptions
            .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
}

fn literal_prefix(span: &TextSpan, component: &TextSpan) -> bool {
    span.blocks == component.blocks
        && span.separator.is_none()
        && span.canonical_range.start == span.comparable_range.start
        && span.canonical_range.end == span.comparable_range.end
        && span.comparable_range.start == 0
        && span.comparable_range.end > 0
        && span.comparable_range.end <= component.comparable_range.end
}
fn prefix_partition(parts: &[ResolutionRange], component: &TextSpan) -> Option<TextSpan> {
    let mut selected = None;
    for part in parts {
        if part.block != component.blocks[0]
            || part.state != super::ResolutionState::Unresolved
            || part.comparable_range.start != 0
        {
            continue;
        }
        let span = TextSpan {
            blocks: super::super::try_copy_slice(&[part.block])?,
            separator: None,
            canonical_range: part.canonical_range,
            comparable_range: part.comparable_range,
        };
        if selected.is_some() || !literal_prefix(&span, component) {
            return None;
        }
        selected = Some(span);
    }
    selected
}

impl Assessor<'_, '_> {
    /// Attempts only whole final unresolved prefixes after all prior consumers.
    /// Every raw source, latest ownership, diagnostic, work and output veto is
    /// checked before either side or either new proof record is published.
    pub(super) fn recover_source_closed_terminal_children(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        publication: local::CoupledPublication<'_>,
        domain_capacity: usize,
        limit: usize,
    ) -> Result<bool> {
        if self.remaining_work == 0
            || self.output_stop.is_some()
            || publication.unresolved.is_empty()
        {
            return Ok(false);
        }
        let Some(domain_bytes) = coarse_reviewed_domain_bytes(
            publication.domains,
            domain_capacity,
            &mut self.remaining_work,
        ) else {
            return Ok(false);
        };
        let Some(cache_bytes) = self.cached_literal_source_capacity_bound() else {
            return Ok(false);
        };
        let Some(view_limit) = COARSE_MEMORY_BYTES
            .checked_sub(domain_bytes)
            .and_then(|n| n.checked_sub(cache_bytes))
        else {
            return Ok(false);
        };
        let [old_partition, new_partition] = publication.partitions;
        let regions = publication.unresolved;
        let mut adopted = false;
        for (key, proof) in publication.domains {
            if !self.charge(16) {
                break;
            }
            if self.records.len().saturating_add(2) >= limit
                || regions.len() > limit
                || !eligible_terminal_header(key, proof, &self.records[proof.relation])
            {
                continue;
            }
            if let Some((old_scope, new_scope)) = &key.local {
                let record = &self.records[proof.relation];
                let scope_work = old_scope
                    .blocks
                    .len()
                    .saturating_add(new_scope.blocks.len())
                    .saturating_add(record.old_span.as_ref().map_or(0, |s| s.blocks.len()))
                    .saturating_add(record.new_span.as_ref().map_or(0, |s| s.blocks.len()))
                    .saturating_add(record.assumptions.len())
                    .saturating_mul(8)
                    .saturating_add(64);
                if !self.charge(scope_work) {
                    break;
                }
                let record = &self.records[proof.relation];
                if !record
                    .assumptions
                    .contains(&ComparisonAssumption::LocalEvidenceBoundaries)
                    || record.old_span.as_ref() != Some(old_scope)
                    || record.new_span.as_ref() != Some(new_scope)
                {
                    continue;
                }
            }
            let (Some(old_component), Some(new_component)) = (
                component_span(self.sides[0], key.old.end - 1),
                component_span(self.sides[1], key.new.end - 1),
            ) else {
                continue;
            };
            // Select each side's unique whole unresolved initial prefix.
            // Diagnostics may be one-sided; correspondence is supplied only
            // later by mandatory pairs of the original exact parent.
            if !self.charge(
                old_partition
                    .len()
                    .saturating_add(new_partition.len())
                    .saturating_mul(8),
            ) {
                break;
            }
            let (Some(old_selected), Some(new_selected)) = (
                prefix_partition(old_partition, &old_component),
                prefix_partition(new_partition, &new_component),
            ) else {
                continue;
            };
            if old_selected.comparable_range.end - old_selected.comparable_range.start
                != new_selected.comparable_range.end - new_selected.comparable_range.start
            {
                continue;
            }
            let spans = [&old_selected, &new_selected];
            let parent = &self.records[proof.relation];
            let [Some(old_parent), Some(new_parent)] = [&parent.old_span, &parent.new_span] else {
                continue;
            };
            if !charge_work(
                &mut self.remaining_work,
                old_parent
                    .blocks
                    .len()
                    .saturating_add(new_parent.blocks.len()),
            ) {
                break;
            }
            let (Some(old_parent), Some(new_parent)) =
                (copy_span(old_parent), copy_span(new_parent))
            else {
                continue;
            };
            let [old_parent, new_parent] = [&old_parent, &new_parent];
            // Bind the exact immutable key and original separators. Local
            // parents must close at the real terminal component end; their
            // selected tokens must still contain the entire component. A
            // retained length alone never establishes either fact.
            let mut exact_whole = true;
            for (side, (range, separator, span)) in [
                (key.old.clone(), key.old_separator, old_parent),
                (key.new.clone(), key.new_separator, new_parent),
            ]
            .into_iter()
            .enumerate()
            {
                let Ok(metadata) = light_group_metadata(
                    self.sides[side],
                    range,
                    separator,
                    &mut self.remaining_work,
                    view_limit,
                ) else {
                    exact_whole = false;
                    break;
                };
                let bound = if key.local.is_some() {
                    // A local proof may begin within an earlier component,
                    // but cannot borrow an unproven or interior right cut.
                    metadata.span.blocks == span.blocks
                        && metadata.span.separator == span.separator
                        && metadata.span.canonical_range.end == span.canonical_range.end
                        && metadata.span.comparable_range.end == span.comparable_range.end
                        && metadata.span.canonical_range.start <= span.canonical_range.start
                        && metadata.span.comparable_range.start <= span.comparable_range.start
                } else {
                    metadata.span == *span
                };
                exact_whole &= bound && !metadata.has_unmapped;
            }
            if !exact_whole {
                continue;
            }
            let Ok(views) = borrowed_group_pair(
                self.sides,
                [old_parent, new_parent],
                &mut self.remaining_work,
                view_limit,
            ) else {
                continue;
            };
            let tokens = [views[0].selected_tokens(), views[1].selected_tokens()];
            let lengths = [
                old_component.comparable_range.end,
                new_component.comparable_range.end,
            ];
            let cuts = [
                tokens[0].len().saturating_sub(lengths[0]),
                tokens[1].len().saturating_sub(lengths[1]),
            ];
            if proof.exact_lengths() != Some([tokens[0].len(), tokens[1].len()])
                || lengths
                    .iter()
                    .enumerate()
                    .any(|(side, n)| *n < 2 || *n > tokens[side].len())
                || old_selected.comparable_range.end < 2
            {
                continue;
            }
            let mut source_closed = true;
            for side in 0..2 {
                let index = if side == 0 {
                    key.old.end - 1
                } else {
                    key.new.end - 1
                };
                let component = &self.sides[side].canonical[index];
                // The whole terminal component must be the parent's exact
                // suffix. Its first two witnesses are real nonspace source
                // tokens, never a synthetic inter-block separator.
                if !self.charge(4)
                    || component[..2].iter().any(
                        |token| !matches!(token,ComparableToken::Scalar(c) if !c.is_whitespace()),
                    )
                    || tokens[side][cuts[side]..cuts[side] + 2]
                        .iter()
                        .copied()
                        .ne(component[..2].iter())
                    || !self.charge(lengths[side])
                    || !tokens[side][cuts[side]..]
                        .iter()
                        .copied()
                        .eq(component.iter())
                    || !coarse_source_guard(
                        self.sides[side],
                        [old_parent, new_parent][side],
                        tokens[side],
                        &mut self.remaining_work,
                    )?
                {
                    source_closed = false;
                    break;
                }
            }
            if !source_closed {
                continue;
            }
            let Some(analysis_limit) = view_limit
                .checked_sub(views[0].retained_bytes)
                .and_then(|n| n.checked_sub(views[1].retained_bytes))
                .and_then(|n| n.checked_sub(4096))
            else {
                continue;
            };
            let analysis = match semantic::mandatory_match_analysis_with_memory_limit(
                tokens[0],
                tokens[1],
                &mut self.remaining_work,
                analysis_limit,
            ) {
                Ok(Some(analysis)) => analysis,
                Ok(None) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => continue,
                Err(error) => return Err(error),
            };
            let len = old_selected.comparable_range.end - old_selected.comparable_range.start;
            if !self.charge(len.saturating_add(2).saturating_mul(usize::BITS as usize))
                || analysis.mandatory_diagonal_count(cuts[0], cuts[1], 2) != 2
                || analysis.mandatory_diagonal_count(
                    cuts[0] + old_selected.comparable_range.start,
                    cuts[1] + new_selected.comparable_range.start,
                    len,
                ) != len
            {
                continue;
            }
            let Some(accepted) =
                self.closed_child_latest_veto(ownership, candidates, proven, spans)?
            else {
                continue;
            };
            let cache = self
                .equal_fragment_cache
                .get_or_insert_with(|| equal_fragment::EqualFragmentCache::new(self.sides));
            match cache.prove_sources_for_mandatory_pairs(spans, &mut self.remaining_work) {
                Ok(equal_fragment::FragmentVerdict::Proven) => {}
                Ok(_) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => continue,
                Err(error) => return Err(error),
            }
            let parts = [&*old_partition, &*new_partition];
            // Reserve can move every existing record or ownership interval.
            // Pay the worst-case shallow moves before either capacity grows.
            let growth_work = if self.records.capacity() < self.records.len().saturating_add(2) {
                self.records.len()
            } else {
                0
            }
            .saturating_add(
                ownership
                    .iter()
                    .map(|owner| {
                        if owner.accepted.capacity() < owner.accepted.len().saturating_add(1) {
                            owner.accepted.len()
                        } else {
                            0
                        }
                    })
                    .sum::<usize>(),
            );
            let publication_work = parts
                .iter()
                .map(|p| p.len().saturating_mul(10))
                .sum::<usize>()
                .saturating_add(regions.len().saturating_mul(4))
                .saturating_add(
                    self.records[proof.relation]
                        .assumptions
                        .len()
                        .saturating_mul(2),
                )
                .saturating_add(16)
                .saturating_add(growth_work);
            if !self.charge(publication_work) {
                break;
            }
            let temporary_bytes = parts
                .iter()
                .map(|p| p.capacity().saturating_add(p.len()))
                .sum::<usize>()
                .saturating_mul(std::mem::size_of::<ResolutionRange>())
                .saturating_add(regions.len())
                .saturating_add(4096);
            if temporary_bytes > analysis_limit {
                continue;
            }
            let Some(old) = local::coupled_partition(parts[0], self.sides[0], &accepted[0], limit)?
            else {
                continue;
            };
            let Some(new) = local::coupled_partition(parts[1], self.sides[1], &accepted[1], limit)?
            else {
                continue;
            };
            let Some(removed) = self.coupled_region_removals(regions, &accepted, limit)? else {
                continue;
            };
            if old
                .capacity()
                .saturating_add(new.capacity())
                .saturating_mul(std::mem::size_of::<ResolutionRange>())
                .saturating_add(removed.capacity())
                .saturating_add(4096)
                > temporary_bytes
            {
                continue;
            }
            let inherited = &self.records[proof.relation].assumptions;
            let (Some(mut closure_assumptions), Some(mut equality_assumptions)) = (
                super::super::try_copy_slice(inherited),
                super::super::try_copy_slice(inherited),
            ) else {
                continue;
            };
            if closure_assumptions.try_reserve_exact(1).is_err()
                || equality_assumptions.try_reserve_exact(1).is_err()
                || self.records.try_reserve_exact(2).is_err()
            {
                continue;
            }
            for (owner, selected) in ownership.iter_mut().zip(&accepted) {
                super::reserve_ranges(&mut owner.accepted, selected.len(), limit)?;
            }
            closure_assumptions.push(ComparisonAssumption::MandatoryTerminalComponentBoundary);
            equality_assumptions.push(ComparisonAssumption::MandatoryMatchingEquality);
            let child = self.records.len();
            self.records.push(RelationAssessment {
                old_span: Some(old_component),
                new_span: Some(new_component),
                parent: Some(proof.relation),
                outcome: RelationOutcome::Established,
                search: SearchCompleteness::Complete,
                assumptions: closure_assumptions,
                reasons: Vec::new(),
            });
            self.records.push(RelationAssessment {
                old_span: Some(old_selected),
                new_span: Some(new_selected),
                parent: Some(child),
                outcome: RelationOutcome::Established,
                search: SearchCompleteness::Complete,
                assumptions: equality_assumptions,
                reasons: Vec::new(),
            });
            for (owner, selected) in ownership.iter_mut().zip(accepted) {
                owner.accepted.extend(selected);
            }
            *old_partition = old;
            *new_partition = new;
            let mut i = 0;
            regions.retain(|_| {
                let keep = !removed[i];
                i += 1;
                keep
            });
            adopted = true;
        }
        Ok(adopted)
    }
}

#[cfg(test)]
mod tests {
    use super::super::closed_child::tests::block;
    use super::super::{
        DomainKey, DomainProof, ProofScope, ResolutionState, ScalarRange, SourceInterval,
        UnresolvedRegion,
    };
    use super::*;
    use crate::{
        alignment::{Alignment, AlignmentConfidence, AlignmentKind, AlignmentSpan, BlockSeparator},
        diff::{Change, ChangeKind, ChangedRegionProof, Confidence, DiffOptions, TokenRange},
        layout::BlockId,
        model::GlyphId,
        normalize::TextSourceAtom,
    };

    fn run(mutation: usize, work: usize, limit: usize) -> Result<bool> {
        let mut old_blocks = [block(1, "YZ "), block(2, "tailX1AAA")];
        let mut new_blocks = [block(101, "YZ "), block(102, "tailX2AA")];
        if mutation == 5 {
            old_blocks[1].raw.text.replace_range(0..1, "q");
        }
        if mutation == 6 {
            let glyph = old_blocks[1].canonical.source_map[0].source.clone();
            old_blocks[0].canonical.source_map[0].source = glyph.clone();
            old_blocks[0].raw.source_map[0].source = glyph;
        }
        if mutation == 7 {
            old_blocks[1].canonical.source_map[0]
                .source
                .atoms
                .push(TextSourceAtom::Glyph(GlyphId(99999)));
        }
        if mutation == 8 {
            old_blocks[1].pages = vec![0, 1];
            old_blocks[1].page_breaks = Some(vec![1]);
        }
        if mutation == 9 {
            old_blocks[1].pages = vec![1];
        }
        if mutation == 17 {
            old_blocks[0] = block(1, "ta");
            new_blocks[0] = block(101, "tata");
        }
        let separator = if mutation == 17 {
            BlockSeparator::Concatenate
        } else {
            BlockSeparator::Space
        };
        let old = crate::diff::SidePlan::inspect("old terminal", &old_blocks)?.materialize()?;
        let new = crate::diff::SidePlan::inspect("new terminal", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: vec![BlockId(1), BlockId(2)],
                new: vec![BlockId(101), BlockId(102)],
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: Vec::new(),
                old_separator: Some(separator),
                new_separator: Some(separator),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let groups = [
            old.canonical_group(&[BlockId(1), BlockId(2)], Some(separator)),
            new.canonical_group(&[BlockId(101), BlockId(102)], Some(separator)),
        ];
        let mut a = Assessor::new(
            [&old, &new],
            &alignment,
            None,
            DiffOptions {
                max_assessment_ranges: limit,
                ..Default::default()
            },
        )?;
        a.records.push(RelationAssessment {
            old_span: Some(groups[0].full_span()),
            new_span: Some(groups[1].full_span()),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: vec![
                ComparisonAssumption::InputReadingOrder,
                ComparisonAssumption::CanonicalNormalization,
                ComparisonAssumption::LocalEvidenceBoundaries,
            ],
            reasons: Vec::new(),
        });
        let mut domains = vec![(
            DomainKey {
                local: None,
                old: 0..2,
                new: 0..2,
                old_separator: separator,
                new_separator: separator,
            },
            DomainProof {
                scope: ProofScope::ExactKey,
                relation: 0,
                unique: false,
                search: SearchCompleteness::Complete,
                edits: Vec::new(),
                lengths: [groups[0].tokens.len(), groups[1].tokens.len()],
                strict_unique: false,
                stable_events: None,
            },
        )];
        if mutation == 1 {
            domains[0].1.search = SearchCompleteness::Incomplete;
        }
        if mutation == 2 {
            domains[0].1.scope = ProofScope::BroadRootRefusal;
        }
        if mutation == 3 {
            a.records[0]
                .assumptions
                .push(ComparisonAssumption::AlternativeLineBreakNormalization);
        }
        if mutation == 4 {
            a.records[0]
                .old_span
                .as_mut()
                .expect("parent")
                .comparable_range
                .start = 1;
        }
        if (19..=23).contains(&mutation) {
            let start = if mutation == 20 { 4 } else { 1 };
            let trim = usize::from(mutation == 21);
            let old_scope = groups[0].span(start, groups[0].tokens.len() - trim);
            let new_scope = groups[1].span(start, groups[1].tokens.len() - trim);
            domains[0].0.local = Some((old_scope.clone(), new_scope.clone()));
            domains[0].1.lengths = [
                old_scope.comparable_range.end - start,
                new_scope.comparable_range.end - start,
            ];
            a.records[0].old_span = Some(old_scope);
            a.records[0].new_span = Some(new_scope);
            if mutation == 22 {
                a.records[0]
                    .assumptions
                    .retain(|a| *a != ComparisonAssumption::LocalEvidenceBoundaries);
            }
            if mutation == 23 {
                domains[0]
                    .0
                    .local
                    .as_mut()
                    .expect("local")
                    .0
                    .canonical_range
                    .start += 1;
            }
        }
        let span = |side: usize, start, end| TextSpan {
            blocks: vec![if side == 0 { BlockId(2) } else { BlockId(102) }],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        };
        let selected = [span(0, 0, 5), span(1, 0, 5)];
        let mut partitions = std::array::from_fn::<_, 2, _>(|side| {
            vec![
                ResolutionRange {
                    block: if side == 0 { BlockId(1) } else { BlockId(101) },
                    canonical_range: ScalarRange {
                        start: 0,
                        end: if side == 0 {
                            old.canonical[0].len()
                        } else {
                            new.canonical[0].len()
                        },
                    },
                    comparable_range: TokenRange {
                        start: 0,
                        end: if side == 0 {
                            old.canonical[0].len()
                        } else {
                            new.canonical[0].len()
                        },
                    },
                    state: ResolutionState::Equal,
                },
                ResolutionRange {
                    block: selected[side].blocks[0],
                    canonical_range: ScalarRange { start: 0, end: 5 },
                    comparable_range: TokenRange { start: 0, end: 5 },
                    state: ResolutionState::Unresolved,
                },
                ResolutionRange {
                    block: selected[side].blocks[0],
                    canonical_range: ScalarRange { start: 5, end: 6 },
                    comparable_range: TokenRange { start: 5, end: 6 },
                    state: ResolutionState::Changed,
                },
                ResolutionRange {
                    block: selected[side].blocks[0],
                    canonical_range: ScalarRange {
                        start: 6,
                        end: if side == 0 { 9 } else { 8 },
                    },
                    comparable_range: TokenRange {
                        start: 6,
                        end: if side == 0 { 9 } else { 8 },
                    },
                    state: ResolutionState::Unresolved,
                },
            ]
        });
        let mut regions = vec![
            UnresolvedRegion {
                old_span: Some(selected[0].clone()),
                new_span: Some(selected[1].clone()),
                evidence: vec![super::super::AlignmentEvidence::ReadingOrderUnknown],
            },
            UnresolvedRegion {
                old_span: Some(span(0, 6, 9)),
                new_span: Some(span(1, 6, 8)),
                evidence: vec![super::super::AlignmentEvidence::TextSimilarity],
            },
        ];
        if mutation == 18 {
            partitions[0][1].canonical_range.end = 4;
        }
        let mut owners = [Ownership::new(), Ownership::new()];
        for owner in &mut owners {
            owner.accepted.push(SourceInterval {
                block_index: 1,
                start: 5,
                end: 6,
            });
            owner.changed.push(SourceInterval {
                block_index: 1,
                start: 5,
                end: 6,
            });
        }
        let mut candidates = Vec::new();
        let mut proven = Vec::new();
        if mutation == 10 {
            candidates.push(ChangeCandidate {
                change: Change::single_occurrence(
                    ChangeKind::Replacement,
                    Some(selected[0].clone()),
                    Some(selected[1].clone()),
                    Confidence::High,
                    Vec::new(),
                ),
                relation: 0,
                alternative_group: 0,
            });
        }
        if mutation == 11 {
            proven.push(ProvenChangedRegion {
                old_span: Some(selected[0].clone()),
                new_span: Some(selected[1].clone()),
                proof: ChangedRegionProof::ExactTokenMultisetMismatch,
                confidence: Confidence::High,
            });
        }
        if mutation == 12 {
            owners[0].accepted.push(SourceInterval {
                block_index: 1,
                start: 0,
                end: 1,
            });
        }
        if mutation == 13 {
            owners[0].changed.push(SourceInterval {
                block_index: 1,
                start: 0,
                end: 1,
            });
        }
        if mutation == 14 {
            regions[0]
                .evidence
                .push(super::super::AlignmentEvidence::ExtractionGap);
        }
        if mutation == 15 {
            regions[0]
                .old_span
                .as_mut()
                .expect("diagnostic")
                .comparable_range
                .end = 6;
            regions[0]
                .old_span
                .as_mut()
                .expect("diagnostic")
                .canonical_range
                .end = 6;
        }
        if mutation == 16 {
            a.output_stop = Some(0);
        }
        let records = a.records.clone();
        let before_parts = partitions.clone();
        let before_regions = regions.clone();
        let owned = [owners[0].accepted.clone(), owners[1].accepted.clone()];
        a.remaining_work = work;
        let [p0, p1] = &mut partitions;
        let adopted = a.recover_source_closed_terminal_children(
            &mut owners,
            &candidates,
            &proven,
            local::CoupledPublication {
                domains: &domains,
                partitions: [p0, p1],
                unresolved: &mut regions,
            },
            domains.capacity(),
            limit,
        )?;
        assert!(a.remaining_work <= work);
        assert_eq!(a.records[0], records[0]);
        if adopted {
            assert_eq!(a.records.len(), 3);
            assert_eq!(a.records[1].parent, Some(0));
            assert_eq!(a.records[2].parent, Some(1));
            assert!(
                a.records[1]
                    .assumptions
                    .contains(&ComparisonAssumption::MandatoryTerminalComponentBoundary)
            );
            assert_eq!(regions, vec![before_regions[1].clone()]);
            for side in 0..2 {
                assert_eq!(partitions[side][0], before_parts[side][0]);
                assert_eq!(partitions[side][2..], before_parts[side][2..]);
                assert_eq!(partitions[side][1].state, ResolutionState::Equal);
                assert_eq!(owners[side].accepted[..owned[side].len()], owned[side]);
            }
            assert!(
                a.semantic_acceptance.is_empty(),
                "no ambiguous edit is localized"
            );
        } else {
            assert_eq!(a.records, records);
            assert_eq!(partitions, before_parts);
            assert_eq!(regions, before_regions);
            assert_eq!(
                [owners[0].accepted.clone(), owners[1].accepted.clone()],
                owned
            );
        }
        Ok(adopted)
    }
    #[test]
    fn terminal_component_prefix_keeps_ambiguous_edits_unresolved() -> Result<()> {
        assert!(run(0, 32_000_000, 1024)?);
        assert!(run(19, 32_000_000, 1024)?);
        Ok(())
    }
    #[test]
    fn terminal_component_keeps_every_context_source_and_ownership_veto() -> Result<()> {
        for mutation in (1..=18).chain(20..=23) {
            assert!(!run(mutation, 32_000_000, 1024)?, "mutation{mutation}");
        }
        Ok(())
    }
    #[test]
    fn terminal_component_refuses_unfunded_or_output_limited_publication() -> Result<()> {
        for budget in [0, 1, 15, 100, 1000] {
            assert!(!run(0, budget, 1024)?);
        }
        assert!(!run(0, 32_000_000, 3)?);
        Ok(())
    }
}
