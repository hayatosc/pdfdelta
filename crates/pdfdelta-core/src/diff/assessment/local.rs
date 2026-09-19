use super::{
    Assessor, ChangeCandidate, ChangeEvent, Ownership, ProposedRelation, RelationOutcome,
    SourceInterval, charge, occurrence_indices, project, proof_groups, visit_domain_hunks,
};
use crate::{Result, diff::Confidence};

impl Assessor<'_, '_> {
    /// Known input order also closes gaps between verified global anchors.
    /// These gaps can cross soft block or page boundaries even when the
    /// upstream alignment did not propose a content edit for them.
    pub(super) fn discover_ordered_domains(&mut self) -> Result<()> {
        // A complete local run can remove an order warning from localization,
        // but it does not establish correspondence at a document boundary.
        if self.alignment.spans.iter().any(|span| {
            span.evidence.iter().any(|evidence| {
                matches!(
                    evidence,
                    crate::alignment::AlignmentEvidence::ReadingOrderUnknown
                        | crate::alignment::AlignmentEvidence::ReadingOrderInferred
                        | crate::alignment::AlignmentEvidence::SearchIncomplete
                )
            })
        }) {
            return Ok(());
        }
        let mut domains = Vec::new();
        for (index, span) in self.alignment.spans.iter().enumerate() {
            if span.kind != crate::alignment::AlignmentKind::Unresolved
                || span.old.is_empty()
                || span.new.is_empty()
            {
                continue;
            }
            let cost = self.sides[0]
                .source_token_count(&span.old)
                .saturating_add(self.sides[1].source_token_count(&span.new));
            if !self.charge(cost) {
                break;
            }
            let proposal = ProposedRelation {
                old: Some(
                    self.sides[0]
                        .canonical_group(
                            &span.old,
                            (span.old.len() > 1).then_some(crate::alignment::BlockSeparator::Space),
                        )
                        .full_span(),
                ),
                new: Some(
                    self.sides[1]
                        .canonical_group(
                            &span.new,
                            (span.new.len() > 1).then_some(crate::alignment::BlockSeparator::Space),
                        )
                        .full_span(),
                ),
                span_indices: [Some(index), Some(index)],
                exact_recovery: false,
            };
            let key = self.domain_key(&proposal)?;
            if key.old.is_empty() || key.new.is_empty() {
                continue;
            }
            if !self.charge(
                self.sides[0].blocks[key.old.clone()]
                    .iter()
                    .map(|block| block.canonical.text.len())
                    .sum::<usize>()
                    .saturating_add(
                        self.sides[1].blocks[key.new.clone()]
                            .iter()
                            .map(|block| block.canonical.text.len())
                            .sum::<usize>(),
                    ),
            ) {
                break;
            }
            let [old, new] = proof_groups(self.sides, &key)?;
            let domain = super::views::LocalDomain {
                old_span: old.full_span(),
                new_span: new.full_span(),
                source_bounded: false,
            };
            if !domains.contains(&domain) {
                super::reserve_ranges(&mut domains, 1, self.options.max_assessment_ranges)?;
                domains.push(domain);
            }
        }
        self.local_domains = domains;
        Ok(())
    }

    fn mark_local_work_limit(&mut self, relation: usize) {
        let record = &mut self.records[relation];
        record.outcome = RelationOutcome::Tentative;
        record.search = super::SearchCompleteness::Incomplete;
        record.reasons.push(super::AssessmentReason::WorkLimit);
    }

    /// Commits a completed local comparison after ordinary result emission.
    /// A tentative candidate may be superseded; an established source result
    /// is never removed to fund or make room for optional recovery.
    pub(super) fn recover_local(
        &mut self,
        ownership: &mut [Ownership; 2],
        changes: &mut Vec<ChangeEvent>,
        candidates: &mut Vec<ChangeCandidate>,
    ) -> Result<bool> {
        let mut order = (0..self.local_domains.len()).collect::<Vec<_>>();
        if !self.charge(
            order
                .len()
                .saturating_mul(order.len().checked_ilog2().unwrap_or(0) as usize + 1),
        ) {
            return Ok(false);
        }
        order.sort_unstable_by_key(|&index| {
            let domain = &self.local_domains[index];
            (
                domain
                    .old_span
                    .comparable_range
                    .end
                    .saturating_sub(domain.old_span.comparable_range.start)
                    .saturating_add(
                        domain
                            .new_span
                            .comparable_range
                            .end
                            .saturating_sub(domain.new_span.comparable_range.start),
                    ),
                index,
            )
        });
        for index in order {
            if self.remaining_work == 0 || self.output_stop.is_some() {
                break;
            }
            let domain = self.local_domains[index].clone();
            let spans = [&domain.old_span, &domain.new_span];
            if !self.charge(spans.iter().map(|span| span.blocks.len()).sum()) {
                break;
            }
            let accepted = [
                project(self.sides[0], spans[0])?,
                project(self.sides[1], spans[1])?,
            ];
            let mut conflict = false;
            for side in 0..2 {
                let Some(overlap) = overlaps(
                    &accepted[side],
                    &ownership[side].changed,
                    &mut self.remaining_work,
                ) else {
                    return Ok(false);
                };
                conflict |= overlap;
            }
            if conflict {
                continue;
            }
            let span_indices = occurrence_indices(self.alignment, [Some(spans[0]), Some(spans[1])]);
            let proposal = ProposedRelation {
                old: Some(domain.old_span),
                new: Some(domain.new_span),
                span_indices,
                exact_recovery: false,
            };
            let relation = self.assess(&proposal)?;
            if self.records[relation].outcome != RelationOutcome::Established {
                continue;
            }
            let key = self.domain_key(&proposal)?;
            let groups = proof_groups(self.sides, &key)?;
            let proof = &self.domains[&key];
            if proof.edits.is_empty() {
                // Only complete source-bounded singleton domains may publish
                // an equal range without an edit script. Trusted-run fragments,
                // ordered and footer domains keep their existing obligations.
                if !domain.source_bounded
                    || self.records[relation].search != super::SearchCompleteness::Complete
                {
                    continue;
                }
                // Protect tentative candidates: an accepted equality must not
                // swallow source ranges a candidate still claims.
                let mut candidate_conflict = false;
                for candidate in candidates.iter() {
                    for occurrence in &candidate.change.occurrences {
                        for (side, span) in
                            [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                                .into_iter()
                                .enumerate()
                        {
                            let Some(span) = span else {
                                continue;
                            };
                            if !self.charge(span.blocks.len()) {
                                self.mark_local_work_limit(relation);
                                return Ok(false);
                            }
                            let source = project(self.sides[side], span)?;
                            let Some(overlap) =
                                overlaps(&source, &accepted[side], &mut self.remaining_work)
                            else {
                                self.mark_local_work_limit(relation);
                                return Ok(false);
                            };
                            candidate_conflict |= overlap;
                        }
                    }
                }
                if candidate_conflict {
                    continue;
                }
                let limit = self.options.max_assessment_ranges;
                let fits = (0..2).all(|side| {
                    ownership[side]
                        .accepted
                        .len()
                        .saturating_add(accepted[side].len())
                        <= limit
                });
                if !fits {
                    continue;
                }
                for (owner, accepted_side) in ownership.iter_mut().zip(&accepted) {
                    super::reserve_ranges(&mut owner.accepted, accepted_side.len(), limit)?;
                    owner.accepted.extend(accepted_side.iter().copied());
                }
                continue;
            }
            let mut local_changes = Vec::new();
            let cost = groups
                .iter()
                .map(|group| group.tokens.len().saturating_add(group.blocks.len()))
                .sum();
            let complete = visit_domain_hunks(&groups[0], &groups[1], &proof.edits, |hunk, _| {
                if local_changes.len() >= self.options.max_assessment_ranges
                    || !charge(&mut self.remaining_work, cost)
                {
                    return false;
                }
                super::super::append_semantic_hunk(
                    &groups[0],
                    &groups[1],
                    &proof.edits,
                    hunk,
                    Confidence::High,
                    &mut local_changes,
                );
                true
            });
            if !complete {
                let output_limit = self.remaining_work > 0;
                self.records[relation].outcome = RelationOutcome::Tentative;
                self.records[relation].search = super::SearchCompleteness::Incomplete;
                self.records[relation].reasons.push(if output_limit {
                    super::AssessmentReason::OutputLimit
                } else {
                    super::AssessmentReason::WorkLimit
                });
                return Ok(output_limit);
            }
            if !self.validate_semantic_emission(relation, &local_changes)? {
                continue;
            }
            // A content operation must name at least one source-backed range on
            // every side it claims. Synthetic inter-block separators project to
            // no source block, so a separator-only edit is a structural
            // difference and must stay unresolved instead of becoming an
            // established content change.
            let mut backed_changes = Vec::new();
            let mut changed = [Vec::new(), Vec::new()];
            for mut change in local_changes {
                let mut backed_occurrences = Vec::new();
                for occurrence in std::mem::take(&mut change.occurrences) {
                    let mut projections = [Vec::new(), Vec::new()];
                    let mut backed = true;
                    for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(span) = span {
                            if !self.charge(span.blocks.len()) {
                                self.mark_local_work_limit(relation);
                                return Ok(false);
                            }
                            let projected = project(self.sides[side], span)?;
                            // A zero-width span is a valid empty side of a
                            // replacement; only a span that claims source
                            // tokens yet projects to no block is unbacked.
                            if projected.is_empty()
                                && span.comparable_range.start != span.comparable_range.end
                            {
                                backed = false;
                            }
                            projections[side].extend(projected);
                        }
                    }
                    if backed {
                        for side in 0..2 {
                            changed[side].extend(projections[side].iter().copied());
                        }
                        backed_occurrences.push(occurrence);
                    }
                }
                if backed_occurrences.is_empty() {
                    continue;
                }
                change.occurrences = backed_occurrences;
                backed_changes.push(change);
            }
            if backed_changes.is_empty() {
                continue;
            }
            let local_changes = backed_changes;
            for side in 0..2 {
                let Some(overlap) = overlaps(
                    &changed[side],
                    &ownership[side].accepted,
                    &mut self.remaining_work,
                ) else {
                    self.mark_local_work_limit(relation);
                    return Ok(false);
                };
                conflict |= overlap;
            }
            if conflict {
                continue;
            }
            let mut superseded = Vec::new();
            for (index, candidate) in candidates.iter().enumerate() {
                let mut overlaps_domain = false;
                for occurrence in &candidate.change.occurrences {
                    for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(span) = span {
                            if !self.charge(span.blocks.len()) {
                                self.mark_local_work_limit(relation);
                                return Ok(false);
                            }
                            let source = project(self.sides[side], span)?;
                            let Some(overlap) =
                                overlaps(&source, &accepted[side], &mut self.remaining_work)
                            else {
                                self.mark_local_work_limit(relation);
                                return Ok(false);
                            };
                            overlaps_domain |= overlap;
                        }
                    }
                }
                if overlaps_domain {
                    superseded.push(index);
                }
            }
            let limit = self.options.max_assessment_ranges;
            let edit_count = self.domains[&key].edits.len();
            let fits = changes.len().saturating_add(local_changes.len()) <= limit
                && self.localized_edit_count.saturating_add(edit_count) <= limit
                && self.localized_edits.len() < limit
                && (0..2).all(|side| {
                    ownership[side]
                        .accepted
                        .len()
                        .saturating_add(accepted[side].len())
                        <= limit
                        && ownership[side]
                            .changed
                            .len()
                            .saturating_add(changed[side].len())
                            <= limit
                });
            if !fits {
                self.records[relation].outcome = RelationOutcome::Tentative;
                self.records[relation].search = super::SearchCompleteness::Incomplete;
                self.records[relation]
                    .reasons
                    .push(super::AssessmentReason::OutputLimit);
                return Ok(true);
            }
            if !self.charge(edit_count) {
                self.mark_local_work_limit(relation);
                return Ok(false);
            }
            let mut edits = Vec::new();
            edits
                .try_reserve_exact(edit_count)
                .map_err(|_| super::allocation_error("localized edit witness"))?;
            edits.extend_from_slice(&self.domains[&key].edits);
            super::reserve_ranges(&mut self.localized_edits, 1, limit)?;
            let changed_events = changes.len()..changes.len() + local_changes.len();
            for side in 0..2 {
                super::reserve_ranges(&mut ownership[side].accepted, accepted[side].len(), limit)?;
                super::reserve_ranges(&mut ownership[side].changed, changed[side].len(), limit)?;
            }
            super::reserve_ranges(changes, local_changes.len(), limit)?;
            for ((owner, accepted), changed) in ownership.iter_mut().zip(accepted).zip(changed) {
                owner.accepted.extend(accepted);
                owner.changed.extend(changed);
            }
            changes.extend(local_changes);
            self.localized_edit_count += edit_count;
            self.localized_edits.push(super::LocalizedEditScript {
                relation,
                changes: changed_events,
                edits,
            });
            let mut next = superseded.into_iter().peekable();
            let mut index = 0;
            candidates.retain(|_| {
                let retain = next.peek() != Some(&index);
                if !retain {
                    next.next();
                }
                index += 1;
                retain
            });
        }
        Ok(false)
    }
}

fn overlaps(old: &[SourceInterval], new: &[SourceInterval], remaining: &mut usize) -> Option<bool> {
    if !charge(
        remaining,
        old.len().saturating_mul(new.len()).saturating_add(1),
    ) {
        return None;
    }
    Some(old.iter().any(|old| {
        new.iter().any(|new| {
            old.block_index == new.block_index && old.start < new.end && new.start < old.end
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        alignment::{
            Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
            BlockSeparator,
        },
        diff::DiffOptions,
        diff::{TextSpan, assessment::views::LocalDomain},
        layout::{BlockId, BlockRole},
        model::{GlyphId, Vec2},
        normalize::{
            BlockText, FontSizeSignature, MappedText, PositionSignature, ScalarRange,
            SourceMapEntry, TextSource, TextSourceAtom,
        },
    };

    fn sourced_block(id: u64, text: &str) -> BlockText {
        let source_map = text
            .chars()
            .enumerate()
            .map(|(index, _)| SourceMapEntry {
                output_range: ScalarRange {
                    start: index,
                    end: index + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(id * 1000 + index as u64 + 1))]
                        .into(),
                },
            })
            .collect::<Vec<_>>();
        let canonical = MappedText {
            text: text.to_owned(),
            source_map,
            unmapped: Vec::new(),
        };
        let tokens = canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens");
        let font_size = FontSizeSignature::new(&[10.0]).expect("valid font size");
        let position = PositionSignature::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position");
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: text.to_owned(),
            matching_tokens: tokens.clone(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: Some(vec![font_size; tokens.len()]),
            position_signatures: Some(vec![position; tokens.len()]),
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    fn side(blocks: &[BlockText]) -> super::super::Side<'_> {
        super::super::super::SidePlan::inspect("test", blocks)
            .expect("test blocks are valid")
            .materialize()
            .expect("test blocks materialize")
    }

    fn span(block: u64, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end },
            comparable_range: super::super::super::TokenRange { start: 0, end },
        }
    }

    fn unresolved_alignment(old: &[BlockId], new: &[BlockId]) -> Alignment {
        Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: old.to_vec(),
                new: new.to_vec(),
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: Some(BlockSeparator::Space),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    #[test]
    fn established_source_bounded_equal_domain_accepts_its_ranges() -> Result<()> {
        let old_blocks = [sourced_block(1, "shared unique text")];
        let new_blocks = [sourced_block(101, "shared unique text")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let tokens = old_blocks[0]
            .canonical
            .comparable_tokens()
            .expect("old tokens")
            .len();
        let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assessor.local_domains = vec![LocalDomain {
            old_span: span(1, tokens),
            new_span: span(101, tokens),
            source_bounded: true,
        }];
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        let mut candidates = Vec::new();

        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;

        let [old_ownership, _new_ownership] = ownership;
        let resolution = old_ownership.finish(&old, assessor.options.max_assessment_ranges)?;
        assert!(
            resolution.iter().any(|range| {
                range.block == BlockId(1)
                    && range.state == super::super::ResolutionState::Equal
                    && range.comparable_range.start == 0
                    && range.comparable_range.end == tokens
            }),
            "{resolution:?}"
        );
        Ok(())
    }
}
