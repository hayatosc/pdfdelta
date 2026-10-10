//! Literal suffixes inside an initial source component with a forced right cut.
//!
//! The containing exact domain supplies its left boundary. Two mandatory pairs
//! immediately after the first component supply its right boundary. This proves
//! closure only; the component's ambiguous edits retain their original status.

use super::{
    Assessor, COARSE_MEMORY_BYTES, ChangeCandidate, ComparableToken, ComparisonAssumption,
    DomainKey, DomainProof, Error, Ownership, ProofScope, ProvenChangedRegion, RelationAssessment,
    RelationOutcome, ResolutionRange, Result, ScalarRange, SearchCompleteness, Side,
    SourceInterval, TextSpan, TokenRange, borrowed_group_pair, charge_work, coarse_project,
    coarse_reviewed_domain_bytes, coarse_source_guard, equal_fragment, light_group_metadata, local,
    mandatory_equal, semantic,
};

/// Optional initial-component proof; larger domains keep every existing result.
const MAX_PARENT_TOKENS: usize = 512;

fn eligible_header(key: &DomainKey, proof: &DomainProof, record: &RelationAssessment) -> bool {
    key.local.is_none()
        && key.old.len() > 1
        && key.new.len() > 1
        && proof.scope == ProofScope::ExactKey
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

fn component_span(side: &Side<'_>, index: usize) -> Option<TextSpan> {
    let block = &side.blocks[index];
    let len = side.canonical[index].len();
    if len == 0 || !block.canonical.unmapped.is_empty() {
        return None;
    }
    Some(TextSpan {
        blocks: super::super::try_copy_slice(&[block.block])?,
        separator: None,
        canonical_range: ScalarRange { start: 0, end: len },
        comparable_range: TokenRange { start: 0, end: len },
    })
}

fn literal_suffix(span: &TextSpan, component: &TextSpan) -> bool {
    span.blocks == component.blocks
        && span.separator.is_none()
        && span.canonical_range.start == span.comparable_range.start
        && span.canonical_range.end == span.comparable_range.end
        && span.comparable_range.start < span.comparable_range.end
        && span.comparable_range.end == component.comparable_range.end
}

fn suffix_partition(parts: &[ResolutionRange], component: &TextSpan) -> Option<TextSpan> {
    let mut selected = None;
    for part in parts {
        if part.block != component.blocks[0]
            || part.state != super::ResolutionState::Unresolved
            || part.comparable_range.end != component.comparable_range.end
        {
            continue;
        }
        let span = TextSpan {
            blocks: super::super::try_copy_slice(&[part.block])?,
            separator: None,
            canonical_range: part.canonical_range,
            comparable_range: part.comparable_range,
        };
        if selected.is_some() || !literal_suffix(&span, component) {
            return None;
        }
        selected = Some(span);
    }
    selected
}

fn copy_span(span: &TextSpan) -> Option<TextSpan> {
    Some(TextSpan {
        blocks: super::super::try_copy_slice(&span.blocks)?,
        separator: span.separator,
        canonical_range: span.canonical_range,
        comparable_range: span.comparable_range,
    })
}

impl Assessor<'_, '_> {
    /// Attempts only whole final unresolved suffixes after all prior consumers.
    /// Every raw source, latest ownership, diagnostic, work and output veto is
    /// checked before either side or either new proof record is published.
    pub(super) fn recover_source_closed_initial_children(
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
        let Some(cache_bytes) =
            mandatory_equal::literal_source_capacity_bound(self.sides, &mut self.remaining_work)
        else {
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
                || !eligible_header(key, proof, &self.records[proof.relation])
            {
                continue;
            }
            let (Some(old_component), Some(new_component)) = (
                component_span(self.sides[0], key.old.start),
                component_span(self.sides[1], key.new.start),
            ) else {
                continue;
            };
            // Select each side's unique whole unresolved final suffix.
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
                suffix_partition(old_partition, &old_component),
                suffix_partition(new_partition, &new_component),
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
            // The retained proof must cover the exact whole key, including
            // original separators. Its stored length is never an equality.
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
                exact_whole &= metadata.span == *span && !metadata.has_unmapped;
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
            let cuts = [
                old_component.comparable_range.end,
                new_component.comparable_range.end,
            ];
            if proof.exact_lengths() != Some([tokens[0].len(), tokens[1].len()])
                || cuts
                    .iter()
                    .enumerate()
                    .any(|(s, cut)| cut.saturating_add(2) > tokens[s].len())
            {
                continue;
            }
            let mut source_closed = true;
            for side in 0..2 {
                let next_index = if side == 0 {
                    key.old.start + 1
                } else {
                    key.new.start + 1
                };
                let next = &self.sides[side].canonical[next_index];
                // Both boundary witnesses are real nonspace tokens of the
                // next original component, never an inserted group separator.
                if next.len() < 2
                    || !self.charge(4)
                    || next[..2].iter().any(
                        |token| !matches!(token,ComparableToken::Scalar(c) if !c.is_whitespace()),
                    )
                    || tokens[side][cuts[side]..cuts[side] + 2]
                        .iter()
                        .copied()
                        .ne(next[..2].iter())
                    || !self.charge(cuts[side])
                    || !tokens[side][..cuts[side]]
                        .iter()
                        .copied()
                        .eq(self.sides[side].canonical[if side == 0 {
                            key.old.start
                        } else {
                            key.new.start
                        }]
                        .iter())
                    || !coarse_source_guard(
                        self.sides[side],
                        [old_parent, new_parent][side],
                        tokens[side],
                        &mut self.remaining_work,
                    )?
                    || !self.charge(cuts[side])
                    || !tokens[side][..cuts[side]].iter().any(
                        |token| matches!(token, ComparableToken::Scalar(c) if !c.is_whitespace()),
                    )
                {
                    source_closed = false;
                    break;
                }
                // The exact whole parent projects its first block with the
                // same whole-component cuts. Its source guard has checked all
                // these nonspace glyphs, including their unique literal raw
                // counterparts across the entire side. The literal prefix
                // and nonspace check above therefore supply every component
                // source obligation without a second document-wide scan.
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
                    old_selected.comparable_range.start,
                    new_selected.comparable_range.start,
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
            closure_assumptions.push(ComparisonAssumption::MandatoryInitialComponentBoundary);
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

    /// Returns the paired projection only when every latest claim is disjoint.
    fn closed_child_latest_veto(
        &mut self,
        ownership: &[Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        spans: [&TextSpan; 2],
    ) -> Result<Option<[Vec<SourceInterval>; 2]>> {
        if !self.charge(8) {
            return Ok(None);
        }
        let (Some(old), Some(new)) = (
            coarse_project(self.sides[0], spans[0]),
            coarse_project(self.sides[1], spans[1]),
        ) else {
            return Ok(None);
        };
        let selected = [old, new];
        for side in 0..2 {
            if ownership[side]
                .accepted
                .len()
                .saturating_add(selected[side].len())
                > self.options.max_assessment_ranges
            {
                return Ok(None);
            }
            for owned in [&ownership[side].accepted, &ownership[side].changed] {
                if local::overlaps(&selected[side], owned, &mut self.remaining_work) != Some(false)
                {
                    return Ok(None);
                }
            }
        }
        for spans in candidates
            .iter()
            .flat_map(|c| &c.change.occurrences)
            .map(|o| [o.old_span.as_ref(), o.new_span.as_ref()])
            .chain(
                proven
                    .iter()
                    .map(|p| [p.old_span.as_ref(), p.new_span.as_ref()]),
            )
        {
            for (side, span) in spans.into_iter().enumerate() {
                let Some(span) = span else { continue };
                let n = span.blocks.len();
                if n.saturating_mul(std::mem::size_of::<SourceInterval>()) > COARSE_MEMORY_BYTES
                    || !self.charge(n.saturating_mul(usize::BITS as usize + 4))
                {
                    return Ok(None);
                }
                let Some(other) = coarse_project(self.sides[side], span) else {
                    return Ok(None);
                };
                if local::overlaps(&selected[side], &other, &mut self.remaining_work) != Some(false)
                {
                    return Ok(None);
                }
            }
        }
        Ok(Some(selected))
    }
}

#[cfg(test)]
mod tests {
    use super::super::UnresolvedRegion;
    use super::*;
    use crate::{
        alignment::{Alignment, AlignmentConfidence, AlignmentKind, AlignmentSpan, BlockSeparator},
        diff::{Change, ChangeKind, ChangedRegionProof, Confidence, DiffOptions},
        layout::{BlockId, BlockRole},
        model::{GlyphId, Vec2},
        normalize::{
            BlockText, FontSizeSignature, MappedText, PositionSignature, SourceMapEntry,
            TextSource, TextSourceAtom,
        },
    };

    fn block(id: u64, text: &str) -> BlockText {
        let mapped = MappedText {
            text: text.into(),
            source_map: text
                .chars()
                .enumerate()
                .map(|(i, _)| SourceMapEntry {
                    output_range: ScalarRange {
                        start: i,
                        end: i + 1,
                    },
                    source: TextSource {
                        atoms: vec![TextSourceAtom::Glyph(GlyphId(id * 1000 + i as u64 + 1))]
                            .into(),
                    },
                })
                .collect(),
            unmapped: Vec::new(),
        };
        let tokens = mapped.comparable_tokens().expect("valid fixture tokens");
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: mapped.clone(),
            canonical: mapped,
            matching: text.into(),
            matching_tokens: tokens.clone(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: Some(vec![
                FontSizeSignature::new(&[10.0]).expect("finite font");
                tokens.len()
            ]),
            position_signatures: Some(vec![
                PositionSignature::new(
                    Vec2 { x: 0.0, y: 0.0 },
                    Vec2 { x: 1.0, y: 0.0 }
                )
                .expect("finite position");
                tokens.len()
            ]),
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    #[test]
    fn whole_parent_source_guard_implies_its_nonspace_initial_component() -> Result<()> {
        let mut certified = 0;
        let mut held = 0;
        for text in ["A雪 B", " ", "\t", "", "Q"] {
            for mutation in 0..7 {
                let mut blocks = [block(1, text), block(2, "xy")];
                if let Some(first) = blocks[0].canonical.source_map.first().cloned() {
                    match mutation {
                        1 => blocks[0].raw.text = "mismatch".into(),
                        2 => blocks[1].raw.source_map[0].source = first.source,
                        3 => blocks[1].canonical.source_map[0].source = first.source,
                        4 => blocks[0].canonical.source_map[0].output_range.end = 2,
                        5 => {
                            blocks[0].raw.source_map[0].source.atoms = vec![
                                TextSourceAtom::Glyph(GlyphId(1001)),
                                TextSourceAtom::Glyph(GlyphId(1001)),
                            ]
                            .into();
                        }
                        // A defect in an unrelated glyph need not veto the
                        // parent's selected literal component.
                        6 => blocks[1].raw.source_map[1].output_range.end = 3,
                        _ => {}
                    }
                }
                let side = super::super::super::SidePlan::inspect("source implication", &blocks)?
                    .materialize()?;
                let group =
                    side.canonical_group(&[BlockId(1), BlockId(2)], Some(BlockSeparator::Space));
                let tokens = group.tokens.iter().collect::<Vec<_>>();
                let mut parent_work = 32_000_000;
                let parent =
                    coarse_source_guard(&side, &group.full_span(), &tokens, &mut parent_work)?;
                let Some(component) = component_span(&side, 0) else {
                    held += 1;
                    continue;
                };
                let prefix = &tokens[..side.canonical[0].len()];
                let has_nonspace = prefix
                    .iter()
                    .any(|token| matches!(token, ComparableToken::Scalar(c) if !c.is_whitespace()));
                let mut component_work = 32_000_000;
                let original = coarse_source_guard(&side, &component, prefix, &mut component_work)?;
                if parent && has_nonspace {
                    assert!(
                        original,
                        "parent must entail its component: {text:?}/{mutation}"
                    );
                    certified += 1;
                } else {
                    held += 1;
                }
                if !has_nonspace {
                    assert!(!original, "a whitespace-only component retains its veto");
                }
            }
        }
        assert!(certified > 0 && held > 0);
        Ok(())
    }

    #[derive(Clone, Copy, Debug)]
    enum Case {
        Positive,
        ForcedGrowth,
        OneSidedDiagnostics,
        Incomplete,
        BroadScope,
        Alternative,
        PartialParent,
        MissingCut,
        VirtualCut,
        SharedGlyph,
        CompoundSpace,
        RawMismatch,
        Candidate,
        Proven,
        ChangedOwner,
        AcceptedOwner,
        ExtractionGap,
        PartialDiagnostic,
    }

    fn run(case: Case, budget: usize, limit: usize) -> Result<bool> {
        run_with_work(case, budget, limit).map(|(adopted, _)| adopted)
    }

    fn run_with_work(case: Case, budget: usize, limit: usize) -> Result<(bool, usize)> {
        let mut old_blocks = [block(1, "AAA X tail "), block(2, "YZ")];
        let mut new_blocks = [block(101, "AA X tail "), block(102, "YZ")];
        match case {
            Case::MissingCut => new_blocks[1] = block(102, "YQ"),
            Case::VirtualCut => {
                old_blocks[0] = block(1, "AAA X tail");
                new_blocks[0] = block(101, "AA X tail");
            }
            Case::SharedGlyph => {
                let source = old_blocks[0].canonical.source_map[6].source.clone();
                old_blocks[1].canonical.source_map[0].source = source.clone();
                old_blocks[1].raw.source_map[0].source = source;
            }
            Case::CompoundSpace => old_blocks[0].canonical.source_map[5]
                .source
                .atoms
                .push(TextSourceAtom::Glyph(GlyphId(999_999))),
            Case::RawMismatch => old_blocks[0].raw.text.replace_range(6..7, "T"),
            _ => {}
        }
        let old =
            super::super::super::SidePlan::inspect("old fixture", &old_blocks)?.materialize()?;
        let new =
            super::super::super::SidePlan::inspect("new fixture", &new_blocks)?.materialize()?;
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
                old_separator: Some(BlockSeparator::Space),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let options = DiffOptions {
            max_assessment_ranges: limit,
            ..Default::default()
        };
        let mut a = Assessor::new([&old, &new], &alignment, None, options)?;
        let key = DomainKey {
            local: None,
            old: 0..2,
            new: 0..2,
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        };
        let mut work = 32_000_000;
        let old_parent = light_group_metadata(
            &old,
            0..2,
            BlockSeparator::Space,
            &mut work,
            COARSE_MEMORY_BYTES,
        )
        .expect("old whole group");
        let new_parent = light_group_metadata(
            &new,
            0..2,
            BlockSeparator::Space,
            &mut work,
            COARSE_MEMORY_BYTES,
        )
        .expect("new whole group");
        let lengths = [
            old_parent.span.comparable_range.end,
            new_parent.span.comparable_range.end,
        ];
        a.records.push(RelationAssessment {
            old_span: Some(old_parent.span),
            new_span: Some(new_parent.span),
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
            key,
            DomainProof {
                scope: ProofScope::ExactKey,
                relation: 0,
                unique: false,
                search: SearchCompleteness::Complete,
                edits: Vec::new(),
                lengths,
                strict_unique: false,
                stable_events: None,
            },
        )];
        match case {
            Case::Incomplete => domains[0].1.search = SearchCompleteness::Incomplete,
            Case::BroadScope => domains[0].1.scope = ProofScope::BroadRootRefusal,
            Case::Alternative => a.records[0]
                .assumptions
                .push(ComparisonAssumption::AlternativeLineBreakNormalization),
            Case::PartialParent => {
                a.records[0]
                    .old_span
                    .as_mut()
                    .expect("old span")
                    .comparable_range
                    .start = 1;
            }
            _ => {}
        }
        let component_lengths = [old.canonical[0].len(), new.canonical[0].len()];
        let spans = std::array::from_fn::<_, 2, _>(|side| TextSpan {
            blocks: vec![if side == 0 { BlockId(1) } else { BlockId(101) }],
            separator: None,
            canonical_range: ScalarRange {
                start: component_lengths[side] - 6,
                end: component_lengths[side],
            },
            comparable_range: TokenRange {
                start: component_lengths[side] - 6,
                end: component_lengths[side],
            },
        });
        let mut owners = [Ownership::new(), Ownership::new()];
        let mut parts = [Vec::new(), Vec::new()];
        for side in 0..2 {
            let block = spans[side].blocks[0];
            let start = spans[side].comparable_range.start;
            owners[side].accepted.push(SourceInterval {
                block_index: 0,
                start: start - 2,
                end: start,
            });
            for (begin, end, state) in [
                (0, start - 2, super::super::ResolutionState::Unresolved),
                (start - 2, start, super::super::ResolutionState::Equal),
                (
                    start,
                    component_lengths[side],
                    super::super::ResolutionState::Unresolved,
                ),
            ] {
                parts[side].push(ResolutionRange {
                    block,
                    canonical_range: ScalarRange { start: begin, end },
                    comparable_range: TokenRange { start: begin, end },
                    state,
                });
            }
            parts[side].push(ResolutionRange {
                block: if side == 0 { BlockId(2) } else { BlockId(102) },
                canonical_range: ScalarRange { start: 0, end: 2 },
                comparable_range: TokenRange { start: 0, end: 2 },
                state: super::super::ResolutionState::Unresolved,
            });
        }
        let mut regions = vec![
            UnresolvedRegion {
                old_span: Some(spans[0].clone()),
                new_span: Some(spans[1].clone()),
                evidence: Vec::new(),
            },
            UnresolvedRegion {
                old_span: None,
                new_span: Some(TextSpan {
                    blocks: vec![BlockId(102)],
                    separator: None,
                    canonical_range: ScalarRange { start: 0, end: 2 },
                    comparable_range: TokenRange { start: 0, end: 2 },
                }),
                evidence: Vec::new(),
            },
        ];
        if matches!(case, Case::OneSidedDiagnostics) {
            let mut new = regions[0].clone();
            new.old_span = None;
            regions[0].new_span = None;
            regions.insert(1, new);
        }
        let mut candidates = Vec::new();
        let mut proven = Vec::new();
        match case {
            Case::Candidate => candidates.push(ChangeCandidate {
                change: Change::single_occurrence(
                    ChangeKind::Replacement,
                    Some(spans[0].clone()),
                    Some(spans[1].clone()),
                    Confidence::High,
                    Vec::new(),
                ),
                relation: 0,
                alternative_group: 0,
            }),
            Case::Proven => proven.push(ProvenChangedRegion {
                old_span: Some(spans[0].clone()),
                new_span: None,
                proof: ChangedRegionProof::OneSidedNonEmptyRange,
                confidence: Confidence::High,
            }),
            Case::ChangedOwner => owners[0].changed.push(SourceInterval {
                block_index: 0,
                start: spans[0].comparable_range.start,
                end: spans[0].comparable_range.end,
            }),
            Case::AcceptedOwner => owners[1].accepted.push(SourceInterval {
                block_index: 0,
                start: spans[1].comparable_range.start,
                end: spans[1].comparable_range.end,
            }),
            Case::ExtractionGap => regions[0]
                .evidence
                .push(super::super::AlignmentEvidence::ExtractionGap),
            Case::PartialDiagnostic => {
                let mut extra = regions[0].clone();
                let span = extra.old_span.as_mut().expect("old diagnostic");
                span.comparable_range.start -= 1;
                span.canonical_range.start -= 1;
                regions.push(extra);
            }
            _ => {}
        }
        if matches!(case, Case::ForcedGrowth) {
            a.records.shrink_to_fit();
            for owner in &mut owners {
                owner.accepted.shrink_to_fit();
            }
        }
        let before_parts = parts.clone();
        let before_regions = regions.clone();
        let before_owners = [owners[0].accepted.clone(), owners[1].accepted.clone()];
        let parent = a.records[0].clone();
        a.remaining_work = budget;
        let [p0, p1] = &mut parts;
        let adopted = a.recover_source_closed_initial_children(
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
        assert!(a.remaining_work <= budget, "no work refund for {case:?}");
        assert_eq!(a.records[0], parent, "original proof preserved");
        if adopted {
            assert_eq!(a.records.len(), 3);
            assert_eq!(a.records[1].parent, Some(0));
            assert_eq!(a.records[2].parent, Some(1));
            assert!(
                a.records[1]
                    .assumptions
                    .contains(&ComparisonAssumption::MandatoryInitialComponentBoundary)
            );
            assert!(
                a.records[2]
                    .assumptions
                    .contains(&ComparisonAssumption::MandatoryMatchingEquality)
            );
            assert!(
                a.semantic_acceptance.is_empty(),
                "closure never accepts ambiguous edits"
            );
            assert_eq!(
                regions,
                vec![
                    before_regions
                        .last()
                        .expect("unrelated retained diagnostic")
                        .clone()
                ]
            );
            for side in 0..2 {
                assert_eq!(parts[side][0], before_parts[side][0]);
                assert_eq!(parts[side].last(), before_parts[side].last());
                assert_eq!(owners[side].accepted[..1], before_owners[side]);
                assert_eq!(
                    owners[side]
                        .accepted
                        .last()
                        .expect("paired publication")
                        .start,
                    spans[side].comparable_range.start
                );
            }
        } else {
            assert_eq!(a.records.len(), 1, "no partial premise for {case:?}");
            assert_eq!(parts, before_parts, "no partial partition for {case:?}");
            assert_eq!(regions, before_regions, "no diagnostic loss for {case:?}");
            assert_eq!(
                [owners[0].accepted.clone(), owners[1].accepted.clone()],
                before_owners,
                "no partial owner for {case:?}"
            );
        }
        Ok((adopted, budget - a.remaining_work))
    }

    #[test]
    fn initial_child_resolves_literal_whitespace_suffix_without_resolving_parent_edits()
    -> Result<()> {
        assert!(run(Case::Positive, 32_000_000, 1024)?);
        assert!(run(Case::OneSidedDiagnostics, 32_000_000, 1024)?);
        Ok(())
    }

    #[test]
    fn initial_child_keeps_every_source_closure_and_latest_claim_refusal_atomic() -> Result<()> {
        for case in [
            Case::Incomplete,
            Case::BroadScope,
            Case::Alternative,
            Case::PartialParent,
            Case::MissingCut,
            Case::VirtualCut,
            Case::SharedGlyph,
            Case::CompoundSpace,
            Case::RawMismatch,
            Case::Candidate,
            Case::Proven,
            Case::ChangedOwner,
            Case::AcceptedOwner,
            Case::ExtractionGap,
            Case::PartialDiagnostic,
        ] {
            assert!(!run(case, 32_000_000, 1024)?, "must remain held: {case:?}");
        }
        Ok(())
    }

    #[test]
    fn initial_child_pays_record_and_both_ownership_moves_before_publication() -> Result<()> {
        let (spare, spare_work) = run_with_work(Case::Positive, 32_000_000, 1024)?;
        let (growth, growth_work) = run_with_work(Case::ForcedGrowth, 32_000_000, 1024)?;
        assert!(spare && growth);
        assert_eq!(
            growth_work,
            spare_work + 3,
            "one existing record and one existing interval on each side move"
        );
        assert!(
            !run(Case::ForcedGrowth, growth_work - 1, 1024)?,
            "the final unpaid move cannot publish either side"
        );
        Ok(())
    }

    #[test]
    fn initial_child_preserves_output_limit_and_insufficient_work_without_partial_proofs()
    -> Result<()> {
        for budget in [0, 1, 8, 64, 512] {
            assert!(!run(Case::Positive, budget, 1024)?);
        }
        assert!(!run(Case::Positive, 32_000_000, 3)?);
        Ok(())
    }
}
