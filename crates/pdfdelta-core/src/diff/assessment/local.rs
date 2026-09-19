use super::{
    Assessor, ChangeCandidate, ChangeEvent, Ownership, ProposedRelation, RelationOutcome,
    SourceInterval, charge, occurrence_indices, project, proof_groups, visit_domain_hunks,
};
use crate::{
    Result,
    alignment::BlockSeparator,
    diff::{Confidence, Side},
};

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

    /// Recovers the unproposed residue of closed whole-view domains.
    ///
    /// A strictly unique, search-complete domain proof can cover source
    /// ranges the alignment marked unresolved, so no proposal ever carried
    /// them into the output. Each domain is evaluated read-only and committed
    /// atomically after every work, limit and allocation check: unclaimed
    /// hunks become changes, equal gaps are accepted per side-consistent
    /// sub-range, and ownership, change events and the localized witness
    /// commit together. An interrupted recovery records a tentative
    /// `WorkLimit` or `OutputLimit` child of the recovered relation and never
    /// demotes an established proof.
    pub(super) fn recover_closed_domains(
        &mut self,
        ownership: &mut [Ownership; 2],
        changes: &mut Vec<ChangeEvent>,
        candidates: &[ChangeCandidate],
    ) -> Result<()> {
        let limit = self.options.max_assessment_ranges;
        let mut candidate_intervals = [Vec::new(), Vec::new()];
        for candidate in candidates {
            for occurrence in &candidate.change.occurrences {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    let Some(span) = span else {
                        continue;
                    };
                    if !self.charge(span.blocks.len()) {
                        let root = self.root_relation()?;
                        self.mark_recovery_incomplete(root, super::AssessmentReason::WorkLimit)?;
                        return Ok(());
                    }
                    candidate_intervals[side].extend(project(self.sides[side], span)?);
                }
            }
        }
        // Whole-view multi-block domains only, in a complete deterministic
        // order: range extents and separator shapes distinguish every key.
        let mut keys = self
            .domains
            .keys()
            .filter(|key| key.local.is_none() && key.old.len() > 1 && key.new.len() > 1)
            .cloned()
            .collect::<Vec<_>>();
        if !self.charge(
            keys.len()
                .saturating_mul(keys.len().checked_ilog2().unwrap_or(0) as usize + 1),
        ) {
            let root = self.root_relation()?;
            self.mark_recovery_incomplete(root, super::AssessmentReason::WorkLimit)?;
            return Ok(());
        }
        keys.sort_by_key(|key| {
            (
                key.old.start,
                key.old.end,
                key.new.start,
                key.new.end,
                separator_order(key.old_separator),
                separator_order(key.new_separator),
            )
        });
        for key in keys {
            if self.remaining_work == 0 || self.output_stop.is_some() {
                break;
            }
            let (relation, strict_unique, search) = {
                let proof = &self.domains[&key];
                (proof.relation, proof.strict_unique, proof.search)
            };
            if !strict_unique
                || search != super::SearchCompleteness::Complete
                || self.records[relation].outcome != RelationOutcome::Established
            {
                continue;
            }
            let groups = proof_groups(self.sides, &key)?;
            let token_work = groups[0]
                .tokens
                .len()
                .saturating_add(groups[1].tokens.len());
            let blocks_work = groups[0]
                .blocks
                .len()
                .saturating_add(groups[1].blocks.len());
            if !self.charge(token_work.saturating_add(blocks_work)) {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            let edits_len = self.domains[&key].edits.len();
            if edits_len == 0 {
                continue;
            }
            if !self.charge(edits_len) {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            let edits = self.domains[&key].edits.clone();
            let mut budget_exhausted = false;
            let mut output_capped = false;
            let mut hunks = Vec::new();
            let complete = visit_domain_hunks(&groups[0], &groups[1], &edits, |hunk, _| {
                if hunks.len() >= limit {
                    output_capped = true;
                    return false;
                }
                if !self.charge(token_work.saturating_add(edits.len())) {
                    return false;
                }
                hunks.push(hunk);
                true
            });
            if !complete {
                let reason = if output_capped {
                    super::AssessmentReason::OutputLimit
                } else {
                    super::AssessmentReason::WorkLimit
                };
                self.mark_recovery_incomplete(relation, reason)?;
                break;
            }
            if hunks.is_empty() {
                continue;
            }
            let mut hunk_intervals = Vec::new();
            for hunk in &hunks {
                if !self.charge(token_work.saturating_add(blocks_work)) {
                    budget_exhausted = true;
                    break;
                }
                hunk_intervals.push([
                    project(self.sides[0], &groups[0].span(hunk.old.start, hunk.old.end))?,
                    project(self.sides[1], &groups[1].span(hunk.new.start, hunk.new.end))?,
                ]);
            }
            if budget_exhausted {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            // Evaluate every hunk without touching ownership; a claimed or
            // unbacked hunk is held back whole.
            let mut committable = Vec::new();
            'hunks: for (index, hunk) in hunks.iter().enumerate() {
                if !self.charge(token_work.saturating_add(blocks_work)) {
                    budget_exhausted = true;
                    break;
                }
                let mut conflict = false;
                for side in 0..2 {
                    for blocked in [
                        &ownership[side].accepted,
                        &ownership[side].changed,
                        &candidate_intervals[side],
                    ] {
                        let Some(overlap) = overlaps(
                            &hunk_intervals[index][side],
                            blocked,
                            &mut self.remaining_work,
                        ) else {
                            budget_exhausted = true;
                            break 'hunks;
                        };
                        conflict |= overlap;
                    }
                }
                if conflict {
                    continue;
                }
                let Some((events, changed)) = self.recovery_backed_events(&groups, &edits, hunk)?
                else {
                    budget_exhausted = true;
                    break;
                };
                if events.is_empty() || !self.validate_semantic_emission(relation, &events)? {
                    continue;
                }
                committable.push((index, events, changed));
            }
            if budget_exhausted {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            // Paired equal gaps between the script's hunks: both sides are
            // accepted together only when neither side is claimed, so a
            // one-sided claim holds the pair.
            let mut gaps = Vec::new();
            let mut old_cursor = 0usize;
            let mut new_cursor = 0usize;
            for hunk in &hunks {
                if hunk.old.start > old_cursor || hunk.new.start > new_cursor {
                    gaps.push((old_cursor..hunk.old.start, new_cursor..hunk.new.start));
                }
                old_cursor = hunk.old.end;
                new_cursor = hunk.new.end;
            }
            let old_end = groups[0].tokens.len();
            let new_end = groups[1].tokens.len();
            if old_cursor < old_end || new_cursor < new_end {
                gaps.push((old_cursor..old_end, new_cursor..new_end));
            }
            let mut residue = [Vec::new(), Vec::new()];
            if !self.charge(token_work.saturating_add(blocks_work)) {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            let old_block_offsets = group_block_offsets(self.sides[0], &groups[0])?;
            let new_block_offsets = group_block_offsets(self.sides[1], &groups[1])?;
            for (old_gap, new_gap) in gaps {
                if old_gap.len() != new_gap.len() || (old_gap.is_empty() && new_gap.is_empty()) {
                    continue;
                }
                // Split the equal gap at every claim boundary from either side
                // and accept only the sub-ranges no side still claims.
                let mut splits = vec![0usize, old_gap.len()];
                let mut blocked_ranges = [Vec::new(), Vec::new()];
                for side in 0..2 {
                    let (group, gap, offsets) = if side == 0 {
                        (&groups[0], &old_gap, &old_block_offsets)
                    } else {
                        (&groups[1], &new_gap, &new_block_offsets)
                    };
                    for intervals in [
                        &ownership[side].accepted,
                        &ownership[side].changed,
                        &candidate_intervals[side],
                    ] {
                        if !self.charge(
                            intervals
                                .len()
                                .saturating_mul(group.blocks.len().saturating_add(1)),
                        ) {
                            budget_exhausted = true;
                            break;
                        }
                        for interval in intervals {
                            let Some(range) =
                                source_group_range(self.sides[side], group, offsets, interval)?
                            else {
                                continue;
                            };
                            if range.start < gap.end && gap.start < range.end {
                                splits.push(range.start.max(gap.start) - gap.start);
                                splits.push(range.end.min(gap.end) - gap.start);
                                blocked_ranges[side].push(range);
                            }
                        }
                    }
                    if budget_exhausted {
                        break;
                    }
                }
                if budget_exhausted {
                    break;
                }
                let blocked_total = blocked_ranges[0]
                    .len()
                    .saturating_add(blocked_ranges[1].len());
                if !self.charge(splits.len().saturating_mul(blocked_total.saturating_add(1))) {
                    budget_exhausted = true;
                    break;
                }
                splits.sort_unstable();
                splits.dedup();
                for window in splits.windows(2) {
                    let [start, end] = [window[0], window[1]];
                    if start >= end {
                        continue;
                    }
                    let old_range = (old_gap.start + start)..(old_gap.start + end);
                    let new_range = (new_gap.start + start)..(new_gap.start + end);
                    let claimed = blocked_ranges[0]
                        .iter()
                        .any(|range| range.start < old_range.end && old_range.start < range.end)
                        || blocked_ranges[1].iter().any(|range| {
                            range.start < new_range.end && new_range.start < range.end
                        });
                    if claimed {
                        continue;
                    }
                    if !self.charge(
                        end.saturating_sub(start)
                            .saturating_add(blocks_work)
                            .saturating_add(1),
                    ) {
                        budget_exhausted = true;
                        break;
                    }
                    residue[0].extend(project(
                        self.sides[0],
                        &groups[0].span(old_range.start, old_range.end),
                    )?);
                    residue[1].extend(project(
                        self.sides[1],
                        &groups[1].span(new_range.start, new_range.end),
                    )?);
                }
                if budget_exhausted {
                    break;
                }
            }
            if budget_exhausted {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            // Every limit and the commit work are checked before any mutation.
            let accepted_addition = [
                committable
                    .iter()
                    .map(|(index, _, _)| hunk_intervals[*index][0].len())
                    .sum::<usize>()
                    .saturating_add(residue[0].len()),
                committable
                    .iter()
                    .map(|(index, _, _)| hunk_intervals[*index][1].len())
                    .sum::<usize>()
                    .saturating_add(residue[1].len()),
            ];
            let changed_addition = [
                committable
                    .iter()
                    .map(|(_, _, changed)| changed[0].len())
                    .sum::<usize>(),
                committable
                    .iter()
                    .map(|(_, _, changed)| changed[1].len())
                    .sum::<usize>(),
            ];
            let event_count = committable
                .iter()
                .map(|(_, events, _)| events.len())
                .sum::<usize>();
            let commit_work = edits
                .len()
                .saturating_add(event_count)
                .saturating_add(accepted_addition[0])
                .saturating_add(accepted_addition[1])
                .saturating_add(changed_addition[0])
                .saturating_add(changed_addition[1]);
            if !self.charge(commit_work) {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            let fits = (0..2).all(|side| {
                ownership[side]
                    .accepted
                    .len()
                    .checked_add(accepted_addition[side])
                    .is_some_and(|count| count <= limit)
                    && ownership[side]
                        .changed
                        .len()
                        .checked_add(changed_addition[side])
                        .is_some_and(|count| count <= limit)
            }) && changes
                .len()
                .checked_add(event_count)
                .is_some_and(|count| count <= limit)
                && (event_count == 0 || self.localized_edits.len() < limit)
                && self
                    .localized_edit_count
                    .checked_add(edits.len())
                    .is_some_and(|count| count <= limit);
            if !fits {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::OutputLimit)?;
                break;
            }
            // Allocation pre-check: nothing mutates before every reserve
            // succeeds, so an allocation failure leaves this domain unchanged.
            for side in 0..2 {
                reserve_capacity(&mut ownership[side].accepted, accepted_addition[side])?;
                reserve_capacity(&mut ownership[side].changed, changed_addition[side])?;
            }
            reserve_capacity(changes, event_count)?;
            if event_count > 0 {
                reserve_capacity(&mut self.localized_edits, 1)?;
            }
            // Commit the domain's ownership, change events and witness together.
            let base = changes.len();
            for (index, events, changed) in committable {
                for side in 0..2 {
                    ownership[side]
                        .accepted
                        .extend(hunk_intervals[index][side].iter().copied());
                    ownership[side]
                        .changed
                        .extend(changed[side].iter().copied());
                }
                changes.extend(events);
            }
            for side in 0..2 {
                ownership[side]
                    .accepted
                    .extend(std::mem::take(&mut residue[side]));
            }
            if event_count > 0 {
                self.localized_edit_count += edits.len();
                self.localized_edits.push(super::LocalizedEditScript {
                    relation,
                    changes: base..base + event_count,
                    edits,
                });
            }
        }
        Ok(())
    }

    /// Extracts the backed change events of one hunk without touching
    /// ownership. `None` reports an exhausted shared work budget.
    fn recovery_backed_events(
        &mut self,
        groups: &[super::super::GroupText; 2],
        edits: &[crate::diff::AtomicEdit],
        hunk: &super::super::SemanticHunk,
    ) -> Result<Option<RecoveryHunkEvents>> {
        let mut events = Vec::new();
        super::super::append_semantic_hunk(
            &groups[0],
            &groups[1],
            edits,
            hunk.clone(),
            Confidence::High,
            &mut events,
        );
        let mut backed_events = Vec::new();
        let mut changed = [Vec::new(), Vec::new()];
        for mut event in events {
            let mut occurrences = Vec::new();
            for occurrence in std::mem::take(&mut event.occurrences) {
                let mut projections = [Vec::new(), Vec::new()];
                let mut backed = true;
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    let Some(span) = span else {
                        continue;
                    };
                    if !self.charge(span.blocks.len()) {
                        return Ok(None);
                    }
                    let projected = project(self.sides[side], span)?;
                    if projected.is_empty()
                        && span.comparable_range.start != span.comparable_range.end
                    {
                        backed = false;
                    }
                    projections[side].extend(projected);
                }
                if backed {
                    for side in 0..2 {
                        changed[side].extend(projections[side].iter().copied());
                    }
                    occurrences.push(occurrence);
                }
            }
            if occurrences.is_empty() {
                continue;
            }
            event.occurrences = occurrences;
            backed_events.push(event);
        }
        Ok(Some((backed_events, changed)))
    }

    /// Records an unfinished optional recovery as a tentative child of the
    /// relation it was recovering, without demoting any established proof.
    fn mark_recovery_incomplete(
        &mut self,
        parent: usize,
        reason: super::AssessmentReason,
    ) -> Result<()> {
        let (old_span, new_span, assumptions) = {
            let record = &self.records[parent];
            (
                record.old_span.clone(),
                record.new_span.clone(),
                record.assumptions.clone(),
            )
        };
        if old_span.is_none() && new_span.is_none() {
            return Ok(());
        }
        self.record(super::RelationAssessment {
            old_span,
            new_span,
            parent: Some(parent),
            outcome: RelationOutcome::Tentative,
            search: super::SearchCompleteness::Incomplete,
            assumptions,
            reasons: vec![reason],
        })?;
        Ok(())
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

/// Backed change events and their changed source intervals for one hunk.
type RecoveryHunkEvents = (Vec<ChangeEvent>, [Vec<SourceInterval>; 2]);

/// Group token offsets of every block in a proof group, mirroring the
/// separator insertion used by source projection.
fn group_block_offsets(side: &Side<'_>, group: &super::super::GroupText) -> Result<Vec<usize>> {
    let mut offsets = Vec::new();
    let mut offset = 0usize;
    let mut preceding_space = false;
    for (position, block) in group.blocks.iter().enumerate() {
        let block_index = *side
            .index
            .get(block)
            .ok_or_else(|| super::invalid("closed domain refers to an unknown block"))?;
        let tokens = &side.canonical[block_index];
        let separator = position > 0
            && group.separator.map(|separator| separator.at(position - 1))
                == Some(BlockSeparator::Space)
            && !preceding_space
            && !tokens.first().is_some_and(super::space_token);
        if separator {
            offset += 1;
        }
        offsets.push(offset);
        offset += tokens.len();
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, super::space_token);
    }
    Ok(offsets)
}

/// Maps a source interval into the proof group's token offsets when its block
/// belongs to the group.
fn source_group_range(
    side: &Side<'_>,
    group: &super::super::GroupText,
    offsets: &[usize],
    interval: &SourceInterval,
) -> Result<Option<std::ops::Range<usize>>> {
    let Some(position) = group
        .blocks
        .iter()
        .position(|block| side.index.get(block) == Some(&interval.block_index))
    else {
        return Ok(None);
    };
    let base = *offsets
        .get(position)
        .ok_or_else(|| super::invalid("closed domain block offsets are incomplete"))?;
    Ok(Some((base + interval.start)..(base + interval.end)))
}

/// Complete ordering for separator shapes so domain processing order never
/// depends on hash-map iteration.
fn separator_order(separator: BlockSeparator) -> (u8, u8, u8) {
    match separator {
        BlockSeparator::Concatenate => (0, 0, 0),
        BlockSeparator::Space => (1, 0, 0),
        BlockSeparator::PerBoundary([left, right]) => (2, u8::from(left), u8::from(right)),
    }
}

/// Reserves capacity before a commit without changing any length.
fn reserve_capacity<T>(output: &mut Vec<T>, additional: usize) -> Result<()> {
    output
        .try_reserve_exact(additional)
        .map_err(|_| super::allocation_error("closed domain recovery ranges"))
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

    fn matched_alignment(old: &[BlockId], new: &[BlockId]) -> Alignment {
        Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Match,
                old: old.to_vec(),
                new: new.to_vec(),
                score: 1.0,
                canonical_similarity: 1.0,
                score_margin: None,
                confidence: AlignmentConfidence::High,
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: Some(BlockSeparator::Space),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
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

    fn closed_domain_alignment() -> Alignment {
        Alignment {
            spans: vec![
                AlignmentSpan {
                    kind: AlignmentKind::Match,
                    old: vec![BlockId(1)],
                    new: vec![BlockId(101)],
                    score: 1.0,
                    canonical_similarity: 1.0,
                    score_margin: None,
                    confidence: AlignmentConfidence::High,
                    evidence: vec![AlignmentEvidence::ExactCanonical],
                    old_separator: Some(BlockSeparator::Space),
                    new_separator: Some(BlockSeparator::Space),
                },
                AlignmentSpan {
                    kind: AlignmentKind::Unresolved,
                    old: vec![BlockId(2), BlockId(3)],
                    new: vec![BlockId(102), BlockId(103)],
                    score: 0.0,
                    canonical_similarity: 0.0,
                    score_margin: None,
                    confidence: AlignmentConfidence::Low,
                    evidence: vec![
                        AlignmentEvidence::TextSimilarity,
                        AlignmentEvidence::CandidateCompetition,
                    ],
                    old_separator: Some(BlockSeparator::Space),
                    new_separator: Some(BlockSeparator::Space),
                },
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    fn block_intervals(
        side: &super::super::Side<'_>,
        group: &super::super::GroupText,
        start: usize,
        end: usize,
    ) -> Result<Vec<SourceInterval>> {
        super::super::project(side, &group.span(start, end))
    }

    fn block_len(block: &BlockText) -> usize {
        block.canonical.comparable_tokens().expect("tokens").len()
    }

    /// Group offsets of one fixture block; the fixture separates blocks with a
    /// single synthetic space and no block text starts or ends with a space.
    fn fixture_block_range(blocks: &[BlockText], index: usize) -> std::ops::Range<usize> {
        let mut start = 0;
        for block in &blocks[..index] {
            start += block_len(block) + 1;
        }
        start..start + block_len(&blocks[index])
    }

    fn fixture_key(old_blocks: &[BlockText], new_blocks: &[BlockText]) -> super::super::DomainKey {
        super::super::DomainKey {
            local: None,
            old: 0..old_blocks.len(),
            new: 0..new_blocks.len(),
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        }
    }

    fn fixture_blocks() -> ([BlockText; 3], [BlockText; 3]) {
        (
            [
                sourced_block(1, "Alpha value 10"),
                sourced_block(2, "Beta amount 100"),
                sourced_block(3, "Gamma stable text"),
            ],
            [
                sourced_block(101, "Alpha value 20"),
                sourced_block(102, "Beta amount 200"),
                sourced_block(103, "Gamma stable text"),
            ],
        )
    }

    fn assert_block_change(
        changes: &[ChangeEvent],
        old: &super::super::Side<'_>,
        new: &super::super::Side<'_>,
        block_index: usize,
        old_range: std::ops::Range<usize>,
        new_range: std::ops::Range<usize>,
    ) -> Result<()> {
        assert_eq!(changes.len(), 1, "{changes:?}");
        let mut old_intervals = Vec::new();
        let mut new_intervals = Vec::new();
        for occurrence in &changes[0].occurrences {
            if let Some(span) = occurrence.old_span.as_ref() {
                old_intervals.extend(super::super::project(old, span)?);
            }
            if let Some(span) = occurrence.new_span.as_ref() {
                new_intervals.extend(super::super::project(new, span)?);
            }
        }
        assert_eq!(
            old_intervals,
            vec![SourceInterval {
                block_index,
                start: old_range.start,
                end: old_range.end,
            }],
            "{changes:?}"
        );
        assert_eq!(
            new_intervals,
            vec![SourceInterval {
                block_index,
                start: new_range.start,
                end: new_range.end,
            }],
            "{changes:?}"
        );
        Ok(())
    }

    #[test]
    fn group_block_offsets_match_projection_around_empty_blocks() -> Result<()> {
        let blocks = [
            sourced_block(1, ""),
            sourced_block(2, "Alpha "),
            sourced_block(3, ""),
            sourced_block(4, "Beta"),
            sourced_block(5, ""),
        ];
        let old = side(&blocks);
        let ids = [BlockId(1), BlockId(2), BlockId(3), BlockId(4), BlockId(5)];
        for separator in [BlockSeparator::Space, BlockSeparator::Concatenate] {
            let group = old.canonical_group(&ids, Some(separator));
            let offsets = super::group_block_offsets(&old, &group)?;
            assert_eq!(offsets.len(), blocks.len());
            for (index, block) in blocks.iter().enumerate() {
                let len = block_len(block);
                if len == 0 {
                    continue;
                }
                let range = offsets[index]..offsets[index] + len;
                let projected = super::super::project(&old, &group.span(range.start, range.end))?;
                assert_eq!(
                    projected,
                    vec![SourceInterval {
                        block_index: index,
                        start: 0,
                        end: len,
                    }],
                    "separator={separator:?} block={index}"
                );
                assert_eq!(
                    super::source_group_range(&old, &group, &offsets, &projected[0])?,
                    Some(range),
                    "separator={separator:?} block={index}"
                );
            }
        }
        // Per-boundary separators with an empty middle block.
        let three = [
            sourced_block(1, "Alpha"),
            sourced_block(2, ""),
            sourced_block(3, "Beta"),
        ];
        let three_side = side(&three);
        let ids = [BlockId(1), BlockId(2), BlockId(3)];
        let separator = BlockSeparator::PerBoundary([true, false]);
        let group = three_side.canonical_group(&ids, Some(separator));
        let offsets = super::group_block_offsets(&three_side, &group)?;
        for (index, block) in three.iter().enumerate() {
            let len = block_len(block);
            if len == 0 {
                continue;
            }
            let range = offsets[index]..offsets[index] + len;
            let projected =
                super::super::project(&three_side, &group.span(range.start, range.end))?;
            assert_eq!(
                projected,
                vec![SourceInterval {
                    block_index: index,
                    start: 0,
                    end: len,
                }],
                "per-boundary block={index}"
            );
        }
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_emits_unproposed_change_and_equal_residue() -> Result<()> {
        use super::super::SearchCompleteness;
        use crate::diff::ChangeKind;

        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        let groups = super::super::proof_groups([&old, &new], &key)?;
        assessor.prove_domain(&key)?;
        let proof = &assessor.domains[&key];
        assert!(
            proof.strict_unique && proof.unique && proof.search == SearchCompleteness::Complete,
            "strict={} unique={} search={:?} edits={}",
            proof.strict_unique,
            proof.unique,
            proof.search,
            proof.edits.len()
        );
        assert!(!proof.edits.is_empty());

        // Simulate the already established block 1 change.
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let block1_old = block_intervals(&old, &groups[0], 12, 13)?;
        let block1_new = block_intervals(&new, &groups[1], 12, 13)?;
        ownership[0].accepted.extend(block1_old.iter().copied());
        ownership[0].changed.extend(block1_old.iter().copied());
        ownership[1].accepted.extend(block1_new.iter().copied());
        ownership[1].changed.extend(block1_new.iter().copied());

        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;

        assert_eq!(changes[0].kind, ChangeKind::Replacement);
        assert_block_change(&changes, &old, &new, 1, 12..13, 12..13)?;
        // Block 1 keeps its established claim; block 2's equal parts and the
        // whole equal block 3 are accepted on both sides.
        let expected = vec![
            SourceInterval {
                block_index: 0,
                start: 12,
                end: 13,
            },
            SourceInterval {
                block_index: 1,
                start: 12,
                end: 13,
            },
            SourceInterval {
                block_index: 0,
                start: 0,
                end: 12,
            },
            SourceInterval {
                block_index: 0,
                start: 13,
                end: 14,
            },
            SourceInterval {
                block_index: 1,
                start: 0,
                end: 12,
            },
            SourceInterval {
                block_index: 1,
                start: 13,
                end: 15,
            },
            SourceInterval {
                block_index: 2,
                start: 0,
                end: 17,
            },
        ];
        assert_eq!(
            ownership[0].accepted, expected,
            "{:?}",
            ownership[0].accepted
        );
        assert_eq!(
            ownership[1].accepted, expected,
            "{:?}",
            ownership[1].accepted
        );
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_holds_a_candidate_claimed_hunk() -> Result<()> {
        use crate::diff::{ChangeEvent, ChangeKind, ChangeOccurrence};

        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        let groups = super::super::proof_groups([&old, &new], &key)?;
        assessor.prove_domain(&key)?;

        // The candidate claims exactly block 2, so its hunk and the equal gaps
        // that share its ranges are held while the block 1 hunk is recovered.
        let block2 = fixture_block_range(&old_blocks, 1);
        let candidates = vec![ChangeCandidate {
            change: ChangeEvent {
                kind: ChangeKind::Replacement,
                occurrences: vec![ChangeOccurrence {
                    old_span: Some(groups[0].span(block2.start, block2.end)),
                    new_span: Some(groups[1].span(block2.start, block2.end)),
                }],
                confidence: Confidence::High,
                tags: Vec::new(),
            },
            relation: 0,
            alternative_group: 0,
        }];
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &candidates)?;
        assert_block_change(&changes, &old, &new, 0, 12..13, 12..13)?;
        assert_eq!(
            ownership[0].accepted,
            vec![
                SourceInterval {
                    block_index: 0,
                    start: 12,
                    end: 13
                },
                SourceInterval {
                    block_index: 0,
                    start: 0,
                    end: 12
                },
                SourceInterval {
                    block_index: 0,
                    start: 13,
                    end: 14
                },
                SourceInterval {
                    block_index: 2,
                    start: 0,
                    end: 17
                },
            ],
            "{:?}",
            ownership[0].accepted
        );
        assert_eq!(
            ownership[1].accepted,
            vec![
                SourceInterval {
                    block_index: 0,
                    start: 12,
                    end: 13
                },
                SourceInterval {
                    block_index: 0,
                    start: 0,
                    end: 12
                },
                SourceInterval {
                    block_index: 0,
                    start: 13,
                    end: 14
                },
                SourceInterval {
                    block_index: 2,
                    start: 0,
                    end: 17
                },
            ],
            "{:?}",
            ownership[1].accepted
        );
        assert!(
            !ownership[0]
                .accepted
                .iter()
                .any(|interval| interval.block_index == 1),
            "the claimed block 2 must stay unaccepted: {:?}",
            ownership[0].accepted
        );
        assert!(
            !ownership[1]
                .accepted
                .iter()
                .any(|interval| interval.block_index == 1),
            "the claimed block 2 must stay unaccepted on the new side: {:?}",
            ownership[1].accepted
        );
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_holds_a_one_sided_partial_claim() -> Result<()> {
        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        assessor.prove_domain(&key)?;

        // Only the old side of the block 2 changed token is already claimed;
        // that hunk is held while the independent block 1 hunk is recovered
        // and the surrounding equal gaps stay consistent on both sides.
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        ownership[0].accepted.push(SourceInterval {
            block_index: 1,
            start: 12,
            end: 13,
        });
        ownership[0].changed.push(SourceInterval {
            block_index: 1,
            start: 12,
            end: 13,
        });
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
        assert_block_change(&changes, &old, &new, 0, 12..13, 12..13)?;
        assert!(
            !ownership[1]
                .changed
                .iter()
                .any(|interval| interval.block_index == 1),
            "the held block 2 hunk must not mark the new side changed: {:?}",
            ownership[1].changed
        );
        for (side, owner) in ownership.iter().enumerate() {
            for interval in [
                SourceInterval {
                    block_index: 1,
                    start: 0,
                    end: 12,
                },
                SourceInterval {
                    block_index: 1,
                    start: 13,
                    end: 15,
                },
                SourceInterval {
                    block_index: 2,
                    start: 0,
                    end: 17,
                },
            ] {
                assert!(
                    owner.accepted.contains(&interval),
                    "side {side} must accept {interval:?}: {:?}",
                    owner.accepted
                );
            }
        }
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_budget_stop_leaves_results_untouched() -> Result<()> {
        use super::super::{AssessmentReason, SearchCompleteness};

        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        assessor.prove_domain(&key)?;
        let relation = assessor.domains[&key].relation;
        let before = assessor.records[relation].clone();

        assessor.remaining_work = 0;
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
        assert!(changes.is_empty());
        assert!(ownership[0].accepted.is_empty() && ownership[0].changed.is_empty());
        assert!(ownership[1].accepted.is_empty() && ownership[1].changed.is_empty());
        let after = &assessor.records[relation];
        assert_eq!(after.outcome, before.outcome);
        assert_eq!(after.search, before.search);
        assert_eq!(after.reasons, before.reasons);
        let child = assessor.records.iter().find(|record| {
            record.outcome == RelationOutcome::Tentative
                && record.search == SearchCompleteness::Incomplete
                && record.reasons == [AssessmentReason::WorkLimit]
        });
        assert!(child.is_some(), "recovery must record a tentative child");
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_mid_evaluation_budget_stop_preserves_results() -> Result<()> {
        use super::super::{AssessmentReason, SearchCompleteness};
        use crate::diff::{ChangeEvent, ChangeKind, ChangeOccurrence};

        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let key = fixture_key(&old_blocks, &new_blocks);
        let groups = super::super::proof_groups([&old, &new], &key)?;

        let seeded = || {
            let sentinel = SourceInterval {
                block_index: 0,
                start: 0,
                end: 1,
            };
            let mut ownership = [
                super::super::Ownership::new(),
                super::super::Ownership::new(),
            ];
            ownership[0].accepted.push(sentinel);
            ownership[0].changed.push(sentinel);
            let changes = vec![ChangeEvent {
                kind: ChangeKind::Replacement,
                occurrences: vec![ChangeOccurrence {
                    old_span: Some(groups[0].span(0, 1)),
                    new_span: Some(groups[1].span(0, 1)),
                }],
                confidence: Confidence::High,
                tags: Vec::new(),
            }];
            (ownership, changes)
        };

        // Measure the full seeded recovery work on an equivalent assessor.
        let used = {
            let mut probe = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            let (mut ownership, mut changes) = seeded();
            probe.prove_domain(&key)?;
            let after_prove = probe.remaining_work;
            probe.recover_closed_domains(&mut ownership, &mut changes, &[])?;
            assert_eq!(changes.len(), 3);
            after_prove - probe.remaining_work
        };
        assert!(used > 1);

        // Positive budgets that stop during evaluation and just before commit.
        for budget in [used / 2, used - 1] {
            let mut assessor = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            assessor.prove_domain(&key)?;
            let relation = assessor.domains[&key].relation;
            let before = assessor.records[relation].clone();
            // Already established results must survive the interruption.
            let sentinel = SourceInterval {
                block_index: 0,
                start: 0,
                end: 1,
            };
            let (mut ownership, mut changes) = seeded();
            assessor.remaining_work = budget;
            assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
            assert_eq!(changes.len(), 1, "budget {budget}");
            assert_eq!(ownership[0].accepted, vec![sentinel], "budget {budget}");
            assert_eq!(ownership[0].changed, vec![sentinel], "budget {budget}");
            assert!(
                ownership[1].accepted.is_empty() && ownership[1].changed.is_empty(),
                "budget {budget}"
            );
            assert!(
                assessor.localized_edits.is_empty() && assessor.localized_edit_count == 0,
                "budget {budget}"
            );
            let after = &assessor.records[relation];
            assert_eq!(after.outcome, before.outcome, "budget {budget}");
            assert_eq!(after.search, before.search, "budget {budget}");
            assert_eq!(after.reasons, before.reasons, "budget {budget}");
            let child = assessor.records.iter().find(|record| {
                record.outcome == RelationOutcome::Tentative
                    && record.search == SearchCompleteness::Incomplete
                    && record.reasons == [AssessmentReason::WorkLimit]
            });
            assert!(
                child.is_some(),
                "budget {budget}: recovery must record a tentative child"
            );
        }
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_output_limit_leaves_results_untouched() -> Result<()> {
        use super::super::{AssessmentReason, SearchCompleteness};

        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        assessor.prove_domain(&key)?;
        let relation = assessor.domains[&key].relation;
        let before = assessor.records[relation].clone();

        assessor.localized_edit_count = assessor.options.max_assessment_ranges;
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
        assert!(changes.is_empty());
        assert!(ownership[0].accepted.is_empty() && ownership[0].changed.is_empty());
        assert!(ownership[1].accepted.is_empty() && ownership[1].changed.is_empty());
        let after = &assessor.records[relation];
        assert_eq!(after.outcome, before.outcome);
        assert_eq!(after.search, before.search);
        assert_eq!(after.reasons, before.reasons);
        let child = assessor.records.iter().find(|record| {
            record.outcome == RelationOutcome::Tentative
                && record.search == SearchCompleteness::Incomplete
                && record.reasons == [AssessmentReason::OutputLimit]
        });
        assert!(child.is_some(), "recovery must record a tentative child");
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_holds_separator_only_hunks() -> Result<()> {
        let old_blocks = [sourced_block(1, "Alpha"), sourced_block(2, "Beta")];
        let new_blocks = [sourced_block(101, "Alpha"), sourced_block(102, "Beta")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(1), BlockId(2)],
                new: vec![BlockId(101), BlockId(102)],
                score: 1.0,
                canonical_similarity: 1.0,
                score_margin: None,
                confidence: AlignmentConfidence::High,
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: Some(BlockSeparator::Concatenate),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = super::super::DomainKey {
            local: None,
            old: 0..2,
            new: 0..2,
            old_separator: BlockSeparator::Concatenate,
            new_separator: BlockSeparator::Space,
        };
        assessor.prove_domain(&key)?;
        assert!(
            assessor.domains[&key].strict_unique,
            "the synthetic separator insertion must be the unique script"
        );
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
        assert!(
            changes.is_empty(),
            "a separator-only hunk must never become a change: {changes:?}"
        );
        // The equal block tokens are still accepted on both sides.
        for owner in &ownership {
            assert!(
                owner
                    .accepted
                    .iter()
                    .any(|interval| interval.block_index == 0),
                "{:?}",
                owner.accepted
            );
        }
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_reaches_the_public_comparison() -> Result<()> {
        use super::super::ProposedComparison;
        use crate::diff::{ChangeEvent, ChangeKind, ChangeOccurrence};

        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let key = fixture_key(&old_blocks, &new_blocks);
        let groups = super::super::proof_groups([&old, &new], &key)?;
        let block1 = fixture_block_range(&old_blocks, 0);
        let proposed = ProposedComparison {
            changes: vec![ChangeEvent {
                kind: ChangeKind::Replacement,
                occurrences: vec![ChangeOccurrence {
                    old_span: Some(groups[0].span(block1.start, block1.end)),
                    new_span: Some(groups[1].span(block1.start, block1.end)),
                }],
                confidence: Confidence::High,
                tags: Vec::new(),
            }],
            proven_changed_regions: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions: Vec::new(),
        };
        let comparison = super::super::finish(
            [&old, &new],
            &alignment,
            None,
            None,
            proposed,
            DiffOptions::default(),
        )?;
        assert_eq!(comparison.changes.len(), 2, "{:?}", comparison.changes);
        assert!(comparison.change_candidates.is_empty());
        assert!(
            comparison.unresolved_regions.is_empty(),
            "{:?}",
            comparison.unresolved_regions
        );
        assert_eq!(comparison.old_coverage.ratio, Some(1.0));
        assert_eq!(comparison.new_coverage.ratio, Some(1.0));
        Ok(())
    }

    #[test]
    fn closed_domain_recovery_requires_unique_complete_proofs() -> Result<()> {
        use super::super::SearchCompleteness;

        // Repeated text with a shifted insertion leaves the script ambiguous.
        let old_blocks = [
            sourced_block(1, "Schedule SE 2024 "),
            sourced_block(2, "Schedule SE 2024"),
            sourced_block(3, "Gamma stable text"),
        ];
        let new_blocks = [
            sourced_block(101, "Schedule SE 2025 Created 5/7/25 "),
            sourced_block(102, "Schedule SE 2025"),
            sourced_block(103, "Gamma stable text"),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        assessor.prove_domain(&key)?;
        assert!(
            !assessor.domains[&key].strict_unique,
            "fixture must be ambiguous"
        );
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
        assert!(changes.is_empty());
        assert!(ownership[0].accepted.is_empty() && ownership[1].accepted.is_empty());

        // A completed-looking proof whose search is incomplete is skipped too.
        let (old_blocks, new_blocks) = fixture_blocks();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = closed_domain_alignment();
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let key = fixture_key(&old_blocks, &new_blocks);
        assessor.prove_domain(&key)?;
        assert!(assessor.domains[&key].strict_unique);
        assessor.domains.get_mut(&key).expect("domain proof").search =
            SearchCompleteness::Incomplete;
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        let mut changes = Vec::new();
        assessor.recover_closed_domains(&mut ownership, &mut changes, &[])?;
        assert!(changes.is_empty());
        assert!(ownership[0].accepted.is_empty() && ownership[1].accepted.is_empty());
        Ok(())
    }

    #[test]
    fn targeted_proof_exhaustion_records_a_work_limit_child_relation() -> Result<()> {
        use super::super::{
            AssessmentReason, ProposedRelation, RelationOutcome, SearchCompleteness,
        };

        let old_blocks = [
            sourced_block(1, "Schedule SE 2024"),
            sourced_block(2, "Schedule SE 2024"),
        ];
        let new_blocks = [
            sourced_block(101, "Schedule SE 2025 Created"),
            sourced_block(102, "Schedule SE 2025"),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = matched_alignment(&[BlockId(1), BlockId(2)], &[BlockId(101), BlockId(102)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let proposal = ProposedRelation {
            old: Some(span(
                2,
                old_blocks[1]
                    .canonical
                    .comparable_tokens()
                    .expect("old tokens")
                    .len(),
            )),
            new: Some(span(
                102,
                new_blocks[1]
                    .canonical
                    .comparable_tokens()
                    .expect("new tokens")
                    .len(),
            )),
            span_indices: [Some(0), Some(0)],
            exact_recovery: false,
        };
        let key = assessor.domain_key(&proposal)?;
        assessor.prove_domain(&key)?;
        assert_eq!(
            assessor.domains[&key].search,
            SearchCompleteness::Complete,
            "domain proof must complete before the targeted proof runs"
        );
        assert!(!assessor.domains[&key].unique);

        assessor.remaining_work = 0;
        let index = assessor.assess(&proposal)?;
        assert_eq!(
            assessor.records[index].reasons,
            [AssessmentReason::WorkLimit]
        );
        assert_eq!(
            assessor.records[index].search,
            SearchCompleteness::Incomplete
        );
        assert_eq!(assessor.records[index].outcome, RelationOutcome::Tentative);
        assert_eq!(
            assessor.domains[&key].search,
            SearchCompleteness::Complete,
            "the parent domain keeps its completed search"
        );
        Ok(())
    }

    #[test]
    fn targeted_proof_traversal_exhaustion_is_not_a_negative_answer() -> Result<()> {
        use super::super::{ProposalProof, ProposedRelation};

        let old_blocks = [
            sourced_block(1, "Schedule SE 2024"),
            sourced_block(2, "Schedule SE 2024"),
        ];
        let new_blocks = [
            sourced_block(101, "Schedule SE 2025 Created"),
            sourced_block(102, "Schedule SE 2025"),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = matched_alignment(&[BlockId(1), BlockId(2)], &[BlockId(101), BlockId(102)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let proposal = ProposedRelation {
            old: Some(span(
                2,
                old_blocks[1]
                    .canonical
                    .comparable_tokens()
                    .expect("old tokens")
                    .len(),
            )),
            new: Some(span(
                102,
                new_blocks[1]
                    .canonical
                    .comparable_tokens()
                    .expect("new tokens")
                    .len(),
            )),
            span_indices: [Some(0), Some(0)],
            exact_recovery: false,
        };
        let key = assessor.domain_key(&proposal)?;
        assessor.prove_domain(&key)?;
        let [old_group, new_group] = super::super::proof_groups(assessor.sides, &key)?;
        let token_work = old_group
            .tokens
            .len()
            .saturating_add(new_group.tokens.len());
        assessor.remaining_work = token_work.saturating_add(1);
        assert_eq!(
            assessor.proposal_edits_are_invariant(&proposal, &key)?,
            ProposalProof::Exhausted
        );
        Ok(())
    }
}
