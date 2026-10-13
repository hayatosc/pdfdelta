use super::{
    Assessor, ChangeCandidate, ChangeEvent, ComparisonAssumption, Ownership, ProposedRelation,
    RelationOutcome, SearchCompleteness, SourceInterval, TextSpan, charge, occurrence_indices,
    project, visit_domain_hunks,
};
use crate::{
    Result,
    alignment::BlockSeparator,
    diff::{Confidence, ProvenChangedRegion, Side},
};

/// Outcome of one deferred equal-fragment retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EqualFragmentRetry {
    /// The candidate was adopted with its projected intervals and premise.
    Adopted,
    /// The candidate stayed held.
    Held,
    /// The candidate stayed held specifically on a raw cut, so the contiguous
    /// proof would reject it but the internal deleted soft line break
    /// allowance may still repair it. This is the only hold the fallback pass
    /// retries.
    HeldRawCut,
    /// The shared budget or output limit ended the pass; nothing is rewritten.
    Stop,
}

/// A negative-only check of the full coupled source projection premise.
/// Synthetic separator gaps, reordered parts or unequal component lengths
/// cannot pass the later physical proof. Success here grants no evidence.
fn coupled_projection_covers_parent(
    projected: [&[SourceInterval]; 2],
    spans: [&TextSpan; 2],
    remaining: &mut usize,
) -> Option<bool> {
    if !charge(
        remaining,
        projected[0]
            .len()
            .saturating_add(projected[1].len())
            .saturating_mul(6)
            .saturating_add(8),
    ) {
        return None;
    }
    let n = spans[0]
        .comparable_range
        .end
        .saturating_sub(spans[0].comparable_range.start);
    if n == 0
        || spans[1]
            .comparable_range
            .end
            .saturating_sub(spans[1].comparable_range.start)
            != n
        || projected[0].len() < 2
        || projected[0].len() != projected[1].len()
    {
        return Some(false);
    }
    for parts in projected {
        let mut total = 0usize;
        let mut previous = None;
        for part in parts {
            if part.start >= part.end || previous.is_some_and(|index| index >= part.block_index) {
                return Some(false);
            }
            total = total.saturating_add(part.end - part.start);
            previous = Some(part.block_index);
        }
        if total != n {
            return Some(false);
        }
    }
    if !charge(remaining, projected[0].len().saturating_mul(3)) {
        return None;
    }
    Some(
        projected[0]
            .iter()
            .zip(projected[1])
            .all(|(old, new)| old.end - old.start == new.end - new.start),
    )
}

/// Final partitions, emitted diagnostics and the original reviewed proofs.
/// The coupled pass never regenerates or reorders a surviving diagnostic.
pub(super) struct CoupledPublication<'a> {
    pub(super) domains: &'a [(super::DomainKey, super::DomainProof)],
    pub(super) partitions: [&'a mut Vec<super::ResolutionRange>; 2],
    pub(super) unresolved: &'a mut Vec<super::UnresolvedRegion>,
}

struct CoupledContextPublication<'a> {
    domains: &'a [(super::DomainKey, super::DomainProof)],
    partitions: [&'a mut Vec<super::ResolutionRange>; 2],
    unresolved: &'a mut dyn super::context_projection::CoupledContext,
}

/// The proof used after the common whole-parent ownership vetoes.
enum EqualFragmentProof<'a> {
    Contiguous,
    InternalDeletedBreak,
    CoupledParent(CoupledContextPublication<'a>),
}

/// Outcome of processing one local domain during recovery.
enum LocalRecoveryStep {
    /// The domain was processed; the caller may continue.
    Continue,
    /// The caller must stop; `truncated` reports whether output limits cut
    /// the recovery short.
    Stop { truncated: bool },
}

/// One call's complete immutable ownership population, grouped by source block.
/// Every interval is retained in its original order within a block, including
/// duplicates and empty ranges. Grouping never merges intervals: containment
/// still requires one original owner, and overlap keeps its strict endpoints.
struct OwnershipBuckets<'a> {
    offsets: Vec<usize>,
    intervals: Vec<&'a SourceInterval>,
}

impl<'a> OwnershipBuckets<'a> {
    fn build(
        intervals: &'a [SourceInterval],
        block_count: usize,
        expected_queries: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        let count = intervals.len();
        let work = block_count
            .checked_mul(3)?
            .checked_add(count.checked_mul(3)?)?
            .checked_add(1)?;
        // Attempt the optional index only when its setup costs less than half
        // the existing full-population query bound. Refusal leaves that rule
        // available; work already paid by a failed build is never refunded.
        if count == 0 || work >= expected_queries.saturating_mul(count) / 2 || work > *remaining {
            return None;
        }
        let bytes = block_count
            .checked_mul(2)?
            .checked_add(1)?
            .checked_mul(std::mem::size_of::<usize>())?
            .checked_add(count.checked_mul(std::mem::size_of::<&SourceInterval>())?)?
            .checked_add(std::mem::size_of::<Self>())?
            .checked_add(std::mem::size_of::<Vec<usize>>())?;
        if bytes > 32 * 1024 * 1024 || !charge(remaining, work) {
            return None;
        }
        let mut offsets = Vec::new();
        offsets
            .try_reserve_exact(block_count.checked_add(1)?)
            .ok()?;
        offsets.resize(block_count + 1, 0_usize);
        for interval in intervals {
            if interval.block_index >= block_count {
                // An out-of-population owner cannot be silently omitted.
                return None;
            }
            offsets[interval.block_index + 1] += 1;
        }
        for index in 0..block_count {
            offsets[index + 1] += offsets[index];
        }
        let mut cursors = Vec::new();
        cursors.try_reserve_exact(block_count).ok()?;
        cursors.extend_from_slice(&offsets[..block_count]);
        let mut ordered = Vec::new();
        ordered.try_reserve_exact(count).ok()?;
        ordered.resize(count, intervals.first()?);
        for interval in intervals {
            let cursor = &mut cursors[interval.block_index];
            ordered[*cursor] = interval;
            *cursor += 1;
        }
        Some(Self {
            offsets,
            intervals: ordered,
        })
    }

    /// Prepays the two offset lookups and every original interval inspected.
    fn range(&self, block_index: usize, remaining: &mut usize) -> Option<std::ops::Range<usize>> {
        if !charge(remaining, 2) {
            return None;
        }
        let start = *self.offsets.get(block_index)?;
        let end = *self.offsets.get(block_index.checked_add(1)?)?;
        charge(remaining, end - start).then_some(start..end)
    }
}

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
            match self.global_anchor_competition(&key) {
                Some(true) => continue,
                None => break,
                Some(false) => {}
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
            let [old, new] = match self.scoped_proof_groups(&key)? {
                Ok(groups) => groups,
                Err(refusal) => {
                    let reason = if refusal == super::MaterializationRefusal::Work {
                        super::AssessmentReason::WorkLimit
                    } else {
                        super::AssessmentReason::SearchIncomplete
                    };
                    self.record_optional_search_stop(reason)?;
                    break;
                }
            };
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
                if proof.scope != super::ProofScope::ExactKey {
                    continue;
                }
                (proof.relation, proof.strict_unique, proof.search)
            };
            if !strict_unique
                || search != super::SearchCompleteness::Complete
                || self.records[relation].outcome != RelationOutcome::Established
            {
                continue;
            }
            let groups = match self.scoped_proof_groups(&key)? {
                Ok(groups) => groups,
                Err(refusal) => {
                    let reason = if refusal == super::MaterializationRefusal::Work {
                        super::AssessmentReason::WorkLimit
                    } else {
                        super::AssessmentReason::SearchIncomplete
                    };
                    self.mark_recovery_incomplete(relation, reason)?;
                    continue;
                }
            };
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
            let anchor_bound = edits
                .iter()
                .map(|edit| {
                    if edit.old.is_empty() {
                        groups[0].blocks.len()
                    } else {
                        groups[1].blocks.len()
                    }
                })
                .sum::<usize>();
            if event_count > 0
                && (anchor_bound > limit
                    || !self.charge_evidence_link(0, 128)
                    || !self.charge_evidence_link(1, anchor_bound.saturating_mul(64))
                    || !self.charge_evidence_link(
                        3,
                        anchor_bound
                            .saturating_mul(32)
                            .saturating_add(edits.len().saturating_mul(32)),
                    ))
            {
                self.mark_recovery_incomplete(relation, super::AssessmentReason::WorkLimit)?;
                break;
            }
            let source_anchors = if event_count > 0 {
                self.witness_source_anchors(relation, &edits)?
            } else {
                Vec::new()
            };
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
                    strict_unique,
                    origin_relations: Vec::new(),
                    source_anchors,
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

    /// Discovers local domains for closed single-block lines whose whole
    /// content is equal and whose exact rigid translation is carried by an
    /// independently established neighbour correspondence.
    ///
    /// The neighbour evidence is the downstream established state: the local
    /// relation must be established with a complete search and no reasons, its
    /// whole single-block source intervals must already be accepted by the
    /// comparison ownership, and the candidate must not overlap changed
    /// ownership. A domain discovered here is proven by the ordinary local
    /// path, and the rigid translation is recorded as an explicit assumption.
    fn discover_anchored_translations(&mut self, ownership: &[Ownership; 2]) -> Result<()> {
        if self.remaining_work == 0
            || self.local_domains.len() >= self.options.max_assessment_ranges
        {
            return Ok(());
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(());
        };
        if established.is_empty() {
            return Ok(());
        }
        let domains = super::views::discover_translations_with_cache(
            self.sides,
            recovery,
            &established,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
            Some(&mut self.deny_token_cache),
        )?;
        if domains.is_empty() {
            return Ok(());
        }
        // Charge the duplicate check and the append for every discovered
        // domain first: a mid-budget cut commits none of them, so the pass
        // never leaves a partial translation proof behind.
        if !self.charge(
            domains
                .len()
                .saturating_mul(self.local_domains.len().saturating_add(1)),
        ) {
            return Ok(());
        }
        for domain in domains {
            if self.local_domains.len() >= self.options.max_assessment_ranges {
                break;
            }
            self.anchored_translations
                .push((domain.old_span.clone(), domain.new_span.clone()));
            if !self.local_domains.contains(&domain) {
                self.local_domains.push(domain);
            }
        }
        Ok(())
    }

    /// Discovers whole single-block members of trusted runs that sit still
    /// and are carried by an independently established stationary neighbour.
    ///
    /// The pass runs only after the ordinary local and bracketed recovery has
    /// reached its fixpoint, so it never spends work the earlier passes still
    /// need. Candidates whose projected source intervals intersect the
    /// accepted or changed ownership at all are held; a fully owned candidate
    /// is skipped. Every candidate is checked into a buffer first and the
    /// append is charged once, so a mid-budget cut or output limit never
    /// leaves a partial domain or assumption behind.
    pub(super) fn discover_stationary_members(&mut self, ownership: &[Ownership; 2]) -> Result<()> {
        if self.remaining_work == 0
            || self.output_stop.is_some()
            || self.local_domains.len() >= self.options.max_assessment_ranges
            || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
        {
            return Ok(());
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(());
        };
        if established.is_empty() {
            return Ok(());
        }
        let Some(mask) = self.stationary_candidate_mask(ownership)? else {
            return Ok(());
        };
        if !mask[0].iter().any(|&eligible| eligible) || !mask[1].iter().any(|&eligible| eligible) {
            return Ok(());
        }
        let domains = super::views::discover_stationary_members_masked_with_cache(
            self.sides,
            recovery,
            &established,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
            &mask,
            &mut self.deny_token_cache,
        )?;
        if domains.is_empty() {
            return Ok(());
        }
        let mut accepted = Vec::new();
        for domain in domains {
            if self.local_domains.len() + accepted.len() >= self.options.max_assessment_ranges {
                break;
            }
            let duplicate_work = self.local_domains.len().saturating_add(accepted.len());
            if !self.charge(duplicate_work) {
                return Ok(());
            }
            if self.local_domains.contains(&domain) || accepted.contains(&domain) {
                continue;
            }
            match self.stationary_candidate_is_clear(ownership, &domain)? {
                None => return Ok(()),
                Some(false) => {}
                Some(true) => accepted.push(domain),
            }
        }
        if accepted.is_empty() {
            return Ok(());
        }
        if !self.charge(
            accepted
                .len()
                .saturating_mul(self.local_domains.len().saturating_add(accepted.len())),
        ) {
            return Ok(());
        }
        self.local_domains
            .try_reserve(accepted.len())
            .map_err(|_| super::allocation_error("stationary local domains"))?;
        self.stationary_members
            .try_reserve(accepted.len())
            .map_err(|_| super::allocation_error("stationary member assumptions"))?;
        for domain in accepted {
            self.stationary_members
                .push((domain.old_span.clone(), domain.new_span.clone()));
            self.local_domains.push(domain);
        }
        Ok(())
    }

    /// Proves only the stationary members queued by this pass through the
    /// ordinary local recovery, leaving the already processed queue and the
    /// translation discovery untouched. Returns whether the recovery was
    /// truncated by a stop, work limit or output limit.
    pub(super) fn recover_stationary_members(
        &mut self,
        ownership: &mut [Ownership; 2],
        changes: &mut Vec<ChangeEvent>,
        candidates: &mut Vec<ChangeCandidate>,
    ) -> Result<bool> {
        if self.remaining_work == 0 || self.output_stop.is_some() {
            return Ok(false);
        }
        let start = self.local_domains.len();
        self.discover_stationary_members(ownership)?;
        if self.local_domains.len() <= start {
            return Ok(false);
        }
        let mut truncated = false;
        for index in start..self.local_domains.len() {
            if self.remaining_work == 0 || self.output_stop.is_some() {
                truncated = true;
                break;
            }
            if let LocalRecoveryStep::Stop { truncated: stopped } =
                self.recover_local_domain(index, ownership, changes, candidates)?
            {
                truncated = stopped;
                break;
            }
        }
        Ok(truncated)
    }

    /// Discovers whole original blocks whose paired raw source projection is
    /// completely isomorphic and that are carried by an independently
    /// established stationary neighbour.
    ///
    /// The pass runs only after the ordinary local and bracketed recovery has
    /// reached its fixpoint, so it never spends work the earlier passes still
    /// need. Candidates whose projected source intervals intersect the
    /// accepted or changed ownership at all are held; a fully owned candidate
    /// is skipped. Every candidate is checked into a buffer first and the
    /// append is charged once, so a mid-budget cut or output limit never
    /// leaves a partial domain or assumption behind.
    pub(super) fn discover_raw_source_equalities(
        &mut self,
        ownership: &[Ownership; 2],
    ) -> Result<()> {
        if self.remaining_work == 0
            || self.output_stop.is_some()
            || self.local_domains.len() >= self.options.max_assessment_ranges
            || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
        {
            return Ok(());
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(());
        };
        if established.is_empty() {
            return Ok(());
        }
        let Some(mask) = self.stationary_candidate_mask(ownership)? else {
            return Ok(());
        };
        if !mask[0].iter().any(|&eligible| eligible) || !mask[1].iter().any(|&eligible| eligible) {
            return Ok(());
        }
        let domains = super::views::discover_raw_source_equalities_masked_with_cache(
            self.sides,
            recovery,
            &established,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
            &mask,
            &mut self.deny_token_cache,
        )?;
        if domains.is_empty() {
            return Ok(());
        }
        let mut accepted = Vec::new();
        for domain in domains {
            if self.local_domains.len() + accepted.len() >= self.options.max_assessment_ranges {
                break;
            }
            let duplicate_work = self.local_domains.len().saturating_add(accepted.len());
            if !self.charge(duplicate_work) {
                return Ok(());
            }
            if self.local_domains.contains(&domain) || accepted.contains(&domain) {
                continue;
            }
            match self.stationary_candidate_is_clear(ownership, &domain)? {
                None => return Ok(()),
                Some(false) => {}
                Some(true) => accepted.push(domain),
            }
        }
        if accepted.is_empty() {
            return Ok(());
        }
        if !self.charge(
            accepted
                .len()
                .saturating_mul(self.local_domains.len().saturating_add(accepted.len())),
        ) {
            return Ok(());
        }
        self.local_domains
            .try_reserve(accepted.len())
            .map_err(|_| super::allocation_error("raw-source local domains"))?;
        self.raw_source_equalities
            .try_reserve(accepted.len())
            .map_err(|_| super::allocation_error("raw-source-equality member assumptions"))?;
        for domain in accepted {
            self.raw_source_equalities
                .push((domain.old_span.clone(), domain.new_span.clone()));
            self.local_domains.push(domain);
        }
        Ok(())
    }

    /// Proves only the raw-source-equality members queued by this pass through the
    /// ordinary local recovery, leaving the already processed queue and the
    /// translation discovery untouched. Returns whether the recovery was
    /// truncated by a stop, work limit or output limit.
    pub(super) fn recover_raw_source_equalities(
        &mut self,
        ownership: &mut [Ownership; 2],
        changes: &mut Vec<ChangeEvent>,
        candidates: &mut Vec<ChangeCandidate>,
    ) -> Result<bool> {
        if self.remaining_work == 0 || self.output_stop.is_some() {
            return Ok(false);
        }
        let start = self.local_domains.len();
        self.discover_raw_source_equalities(ownership)?;
        if self.local_domains.len() <= start {
            return Ok(false);
        }
        let mut truncated = false;
        for index in start..self.local_domains.len() {
            if self.remaining_work == 0 || self.output_stop.is_some() {
                truncated = true;
                break;
            }
            let domain = self.local_domains[index].clone();
            if self
                .raw_source_equalities
                .iter()
                .any(|(old, new)| old == &domain.old_span && new == &domain.new_span)
            {
                let proposal = super::ProposedRelation {
                    old: Some(domain.old_span.clone()),
                    new: Some(domain.new_span.clone()),
                    span_indices: [None, None],
                    exact_recovery: true,
                };
                let key = self.domain_key(&proposal)?;
                if let Some(proof) = self.domains.get(&key)
                    && self.records[proof.relation].outcome != super::RelationOutcome::Established
                    && !self.records[proof.relation]
                        .assumptions
                        .contains(&crate::diff::ComparisonAssumption::RawSourceEquality)
                {
                    // The exact raw-proven pair was cached as tentative before
                    // this proof existed. Dropping only the cache entry makes
                    // the proof recompute: the earlier relation record stays
                    // as history, and no other key or established record is
                    // touched.
                    self.domains.remove(&key);
                }
            }
            if let LocalRecoveryStep::Stop { truncated: stopped } =
                self.recover_local_domain(index, ownership, changes, candidates)?
            {
                truncated = stopped;
                break;
            }
        }
        Ok(truncated)
    }

    /// Discovers whole source-bounded original members whose complete page and
    /// per-token position columns match one-to-one while their token text
    /// differs, carried by an independently established stationary neighbour.
    ///
    /// The pass runs only after the ordinary local and bracketed recovery has
    /// reached its fixpoint, so it never spends work the earlier passes still
    /// need. Candidates whose projected source intervals intersect the
    /// accepted or changed ownership at all are held; a fully owned candidate
    /// is skipped. Every candidate is checked into a buffer first and the
    /// append is charged once, so a mid-budget cut or output limit never
    /// leaves a partial domain or assumption behind.
    pub(super) fn discover_positioned_replacements(
        &mut self,
        ownership: &[Ownership; 2],
    ) -> Result<()> {
        if self.remaining_work == 0
            || self.output_stop.is_some()
            || self.local_domains.len() >= self.options.max_assessment_ranges
            || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
        {
            return Ok(());
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(());
        };
        if established.is_empty() {
            return Ok(());
        }
        let Some(mask) = self.stationary_candidate_mask(ownership)? else {
            return Ok(());
        };
        if !mask[0].iter().any(|&eligible| eligible) || !mask[1].iter().any(|&eligible| eligible) {
            return Ok(());
        }
        let domains = super::views::discover_positioned_replacements_masked_with_cache(
            self.sides,
            recovery,
            &established,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
            &mask,
            &mut self.deny_token_cache,
        )?;
        if domains.is_empty() {
            return Ok(());
        }
        let mut accepted = Vec::new();
        for domain in domains {
            if self.local_domains.len() + accepted.len() >= self.options.max_assessment_ranges {
                break;
            }
            let duplicate_work = self.local_domains.len().saturating_add(accepted.len());
            if !self.charge(duplicate_work) {
                return Ok(());
            }
            if self.local_domains.contains(&domain) || accepted.contains(&domain) {
                continue;
            }
            match self.stationary_candidate_is_clear(ownership, &domain)? {
                None => return Ok(()),
                Some(false) => {}
                Some(true) => accepted.push(domain),
            }
        }
        if accepted.is_empty() {
            return Ok(());
        }
        if !self.charge(
            accepted
                .len()
                .saturating_mul(self.local_domains.len().saturating_add(accepted.len())),
        ) {
            return Ok(());
        }
        self.local_domains
            .try_reserve(accepted.len())
            .map_err(|_| super::allocation_error("positioned-replacement local domains"))?;
        self.positioned_replacements
            .try_reserve(accepted.len())
            .map_err(|_| super::allocation_error("positioned-replacement member assumptions"))?;
        for domain in accepted {
            self.positioned_replacements
                .push((domain.old_span.clone(), domain.new_span.clone()));
            self.local_domains.push(domain);
        }
        Ok(())
    }

    /// Proves only the positioned-replacement members queued by this pass
    /// through the ordinary local recovery, leaving the already processed
    /// queue and the translation discovery untouched. Returns whether the
    /// recovery was truncated by a stop, work limit or output limit.
    pub(super) fn recover_positioned_replacements(
        &mut self,
        ownership: &mut [Ownership; 2],
        changes: &mut Vec<ChangeEvent>,
        candidates: &mut Vec<ChangeCandidate>,
    ) -> Result<bool> {
        if self.remaining_work == 0 || self.output_stop.is_some() {
            return Ok(false);
        }
        let start = self.local_domains.len();
        self.discover_positioned_replacements(ownership)?;
        if self.local_domains.len() <= start {
            return Ok(false);
        }
        let mut truncated = false;
        for index in start..self.local_domains.len() {
            if self.remaining_work == 0 || self.output_stop.is_some() {
                truncated = true;
                break;
            }
            let domain = self.local_domains[index].clone();
            if self
                .positioned_replacements
                .iter()
                .any(|(old, new)| old == &domain.old_span && new == &domain.new_span)
            {
                let proposal = super::ProposedRelation {
                    old: Some(domain.old_span.clone()),
                    new: Some(domain.new_span.clone()),
                    span_indices: [None, None],
                    exact_recovery: true,
                };
                let key = self.domain_key(&proposal)?;
                if let Some(proof) = self.domains.get(&key)
                    && self.records[proof.relation].outcome != super::RelationOutcome::Established
                    && !self.records[proof.relation]
                        .assumptions
                        .contains(&crate::diff::ComparisonAssumption::PositionedReplacement)
                {
                    // The exact position-column pair was cached as tentative
                    // before this proof existed. Dropping only the cache entry
                    // makes the proof recompute: the earlier relation record
                    // stays as history, and no other key or established record
                    // is touched.
                    self.domains.remove(&key);
                }
            }
            if let LocalRecoveryStep::Stop { truncated: stopped } =
                self.recover_local_domain(index, ownership, changes, candidates)?
            {
                truncated = stopped;
                break;
            }
        }
        Ok(truncated)
    }

    /// Reject-only eligibility mask over whole original blocks per side.
    ///
    /// A block is eligible when its whole source projection is non-empty and
    /// intersects neither the accepted nor the changed ownership. The mask is
    /// computed once per pass from the current ownership and only removes
    /// candidates; view populations and reference sets are never reduced.
    /// `None` means the shared budget ran out while projecting or scanning.
    fn stationary_candidate_mask(
        &mut self,
        ownership: &[Ownership; 2],
    ) -> Result<Option<[Vec<bool>; 2]>> {
        let mut mask: [Vec<bool>; 2] = [Vec::new(), Vec::new()];
        for side in 0..2 {
            let blocks = self.sides[side].blocks;
            if !self.charge(blocks.len()) {
                return Ok(None);
            }
            let mut side_mask = Vec::new();
            side_mask
                .try_reserve_exact(blocks.len())
                .map_err(|_| super::allocation_error("stationary candidate mask"))?;
            let accepted = OwnershipBuckets::build(
                &ownership[side].accepted,
                blocks.len(),
                blocks.len(),
                &mut self.remaining_work,
            );
            let changed = OwnershipBuckets::build(
                &ownership[side].changed,
                blocks.len(),
                blocks.len(),
                &mut self.remaining_work,
            );
            for block in blocks {
                let block_index = self.sides[side].index[&block.block];
                let comparable_end = self.sides[side].canonical[block_index].len();
                let scalar_end = comparable_end.saturating_sub(block.canonical.unmapped.len());
                let span = super::TextSpan {
                    blocks: vec![block.block],
                    separator: None,
                    canonical_range: crate::normalize::ScalarRange {
                        start: 0,
                        end: scalar_end,
                    },
                    comparable_range: crate::diff::TokenRange {
                        start: 0,
                        end: comparable_end,
                    },
                };
                if !self.charge(span.blocks.len()) {
                    return Ok(None);
                }
                let intervals = super::project(self.sides[side], &span)?;
                if intervals.is_empty() {
                    side_mask.push(false);
                    continue;
                }
                let Some(intersects) = self.ownership_intersects_buckets(
                    &ownership[side],
                    &intervals,
                    accepted.as_ref(),
                    changed.as_ref(),
                ) else {
                    return Ok(None);
                };
                side_mask.push(!intersects);
            }
            mask[side] = side_mask;
        }
        Ok(Some(mask))
    }

    /// Whether a stationary candidate may be added: `Some(true)` is clear,
    /// `Some(false)` means fully owned (skip) or an accepted/changed
    /// intersection (hold), and `None` means the shared budget ran out while
    /// projecting or scanning ownership.
    fn stationary_candidate_is_clear(
        &mut self,
        ownership: &[Ownership; 2],
        domain: &super::views::LocalDomain,
    ) -> Result<Option<bool>> {
        let mut clear = true;
        for (side, span) in [&domain.old_span, &domain.new_span].into_iter().enumerate() {
            if !self.charge(span.blocks.len()) {
                return Ok(None);
            }
            let intervals = super::project(self.sides[side], span)?;
            if intervals.is_empty() {
                return Ok(Some(false));
            }
            if !self.charge(
                intervals.len().saturating_mul(
                    ownership[side]
                        .accepted
                        .len()
                        .saturating_add(ownership[side].changed.len()),
                ),
            ) {
                return Ok(None);
            }
            for interval in intervals {
                if ownership[side].changed.iter().any(|changed| {
                    changed.block_index == interval.block_index
                        && changed.start < interval.end
                        && interval.start < changed.end
                }) {
                    return Ok(Some(false));
                }
                if ownership[side].accepted.iter().any(|accepted| {
                    accepted.block_index == interval.block_index
                        && accepted.start < interval.end
                        && interval.start < accepted.end
                }) {
                    clear = false;
                }
            }
        }
        Ok(Some(clear))
    }

    /// Collects every whole single-block established correspondence from the
    /// downstream domain proofs, not only the local domains: a block inside a
    /// multi-block domain can still carry its own established relation, and
    /// the proof must be established with a complete search, carry no reasons
    /// and already own its accepted source intervals. The set is sorted by a
    /// complete source-side key with the sort charged, duplicates are removed
    /// from the sorted order, and the result is `None` when the shared budget
    /// is exhausted.
    pub(super) fn collect_established_blocks(
        &mut self,
        ownership: &[Ownership; 2],
    ) -> Result<Option<Vec<super::views::EstablishedBlock>>> {
        if !self.charge(self.domains.len()) {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        for proof in self.domains.values() {
            if proof.scope != super::ProofScope::ExactKey {
                continue;
            }
            let record = &self.records[proof.relation];
            if record.outcome != RelationOutcome::Established
                || record.search != SearchCompleteness::Complete
                || !record.reasons.is_empty()
            {
                continue;
            }
            let (Some(old_span), Some(new_span)) = (&record.old_span, &record.new_span) else {
                continue;
            };
            if old_span.blocks.len() != 1 || new_span.blocks.len() != 1 {
                continue;
            }
            candidates.push((old_span.clone(), new_span.clone()));
        }
        if !self.charge(
            candidates
                .len()
                .saturating_mul(candidates.len().checked_ilog2().unwrap_or(0) as usize + 1),
        ) {
            return Ok(None);
        }
        candidates.sort_unstable_by_key(|(old_span, new_span)| {
            (
                old_span.blocks.first().map(|block| block.0),
                new_span.blocks.first().map(|block| block.0),
                old_span.comparable_range.start,
                old_span.comparable_range.end,
                new_span.comparable_range.start,
                new_span.comparable_range.end,
            )
        });
        let accepted = [
            OwnershipBuckets::build(
                &ownership[0].accepted,
                self.sides[0].blocks.len(),
                candidates.len(),
                &mut self.remaining_work,
            ),
            OwnershipBuckets::build(
                &ownership[1].accepted,
                self.sides[1].blocks.len(),
                candidates.len(),
                &mut self.remaining_work,
            ),
        ];
        let mut established: Vec<super::views::EstablishedBlock> = Vec::new();
        for (old_span, new_span) in candidates {
            let (Some(old_block), Some(new_block)) =
                (old_span.blocks.first(), new_span.blocks.first())
            else {
                continue;
            };
            // The sort key starts with the block pair, so duplicates are
            // adjacent and the dedupe stays linear.
            if established
                .last()
                .is_some_and(|entry| entry.old_block == *old_block && entry.new_block == *new_block)
            {
                continue;
            }
            let Some(&old_index) = self.sides[0].index.get(old_block) else {
                continue;
            };
            let Some(&new_index) = self.sides[1].index.get(new_block) else {
                continue;
            };
            if old_span.comparable_range.start != 0
                || old_span.comparable_range.end != self.sides[0].canonical[old_index].len()
                || new_span.comparable_range.start != 0
                || new_span.comparable_range.end != self.sides[1].canonical[new_index].len()
            {
                continue;
            }
            let Some(old_owned) =
                self.ownership_contains_bucket(ownership, 0, &old_span, accepted[0].as_ref())?
            else {
                return Ok(None);
            };
            if !old_owned {
                continue;
            }
            let Some(new_owned) =
                self.ownership_contains_bucket(ownership, 1, &new_span, accepted[1].as_ref())?
            else {
                return Ok(None);
            };
            if !new_owned {
                continue;
            }
            established.push(super::views::EstablishedBlock {
                old_block: *old_block,
                new_block: *new_block,
            });
        }
        Ok(Some(established))
    }

    /// Discovers local domains for whole source-bounded lines that are the
    /// unique source block between two independently established boundaries
    /// in the same column band. The domains are proven by the ordinary local
    /// assessment with the strict minimal-edit uniqueness; the bracketed
    /// geometry is recorded as an explicit assumption.
    fn discover_bracketed_domains(&mut self, ownership: &[Ownership; 2]) -> Result<()> {
        if self.remaining_work == 0
            || self.local_domains.len() >= self.options.max_assessment_ranges
        {
            return Ok(());
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(());
        };
        if established.is_empty() {
            return Ok(());
        }
        let domains = super::views::discover_bracketed_domains_with_cache(
            self.sides,
            recovery,
            &established,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
            Some(&mut self.deny_token_cache),
        )?;
        if domains.is_empty() {
            return Ok(());
        }
        if !self.charge(
            domains
                .len()
                .saturating_mul(self.local_domains.len().saturating_add(1)),
        ) {
            return Ok(());
        }
        for domain in domains {
            if self.local_domains.len() >= self.options.max_assessment_ranges {
                break;
            }
            if self.local_domains.contains(&domain) {
                // The round loop re-discovers domains that are already
                // queued; the assumption list must not grow per round.
                continue;
            }
            self.bracketed_domains
                .push((domain.old_span.clone(), domain.new_span.clone()));
            self.local_domains.push(domain);
        }
        Ok(())
    }

    /// Whether every projected interval of a span is already accepted by the
    /// comparison ownership on one side. `None` reports an exhausted shared
    /// work budget; the caller then drops the whole pass without committing a
    /// partial proof.
    fn ownership_contains_bucket(
        &mut self,
        ownership: &[Ownership; 2],
        side: usize,
        span: &TextSpan,
        buckets: Option<&OwnershipBuckets<'_>>,
    ) -> Result<Option<bool>> {
        let Some(buckets) = buckets else {
            return self.ownership_contains(ownership, side, span);
        };
        let projected = project(self.sides[side], span)?;
        if !self.charge(projected.len()) {
            return Ok(None);
        }
        for interval in &projected {
            let Some(range) = buckets.range(interval.block_index, &mut self.remaining_work) else {
                return Ok(None);
            };
            if !buckets.intervals[range].iter().any(|accepted| {
                accepted.block_index == interval.block_index
                    && accepted.start <= interval.start
                    && interval.end <= accepted.end
            }) {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    fn ownership_intersects_buckets(
        &mut self,
        ownership: &Ownership,
        intervals: &[SourceInterval],
        accepted: Option<&OwnershipBuckets<'_>>,
        changed: Option<&OwnershipBuckets<'_>>,
    ) -> Option<bool> {
        let overlaps = |owner: &SourceInterval, query: &SourceInterval| {
            owner.block_index == query.block_index
                && owner.start < query.end
                && query.start < owner.end
        };
        if accepted.is_none() && changed.is_none() {
            if !self.charge(
                intervals.len().saturating_mul(
                    ownership
                        .accepted
                        .len()
                        .saturating_add(ownership.changed.len()),
                ),
            ) {
                return None;
            }
            return Some(intervals.iter().any(|interval| {
                ownership
                    .changed
                    .iter()
                    .any(|owner| overlaps(owner, interval))
                    || ownership
                        .accepted
                        .iter()
                        .any(|owner| overlaps(owner, interval))
            }));
        }
        for interval in intervals {
            // Pay both complete groups before either predicate can short-circuit,
            // just as the original accepted-plus-changed population scan did.
            let accepted_range = if let Some(buckets) = accepted {
                buckets.range(interval.block_index, &mut self.remaining_work)?
            } else {
                if !self.charge(ownership.accepted.len()) {
                    return None;
                }
                0..ownership.accepted.len()
            };
            let changed_range = if let Some(buckets) = changed {
                buckets.range(interval.block_index, &mut self.remaining_work)?
            } else {
                if !self.charge(ownership.changed.len()) {
                    return None;
                }
                0..ownership.changed.len()
            };
            let changed_overlap = if let Some(buckets) = changed {
                buckets.intervals[changed_range]
                    .iter()
                    .any(|owner| overlaps(owner, interval))
            } else {
                ownership.changed[changed_range]
                    .iter()
                    .any(|owner| overlaps(owner, interval))
            };
            let accepted_overlap = if let Some(buckets) = accepted {
                buckets.intervals[accepted_range]
                    .iter()
                    .any(|owner| overlaps(owner, interval))
            } else {
                ownership.accepted[accepted_range]
                    .iter()
                    .any(|owner| overlaps(owner, interval))
            };
            if changed_overlap || accepted_overlap {
                return Some(true);
            }
        }
        Some(false)
    }

    fn ownership_contains(
        &mut self,
        ownership: &[Ownership; 2],
        side: usize,
        span: &TextSpan,
    ) -> Result<Option<bool>> {
        let projected = project(self.sides[side], span)?;
        // Charge the projection scan and the interval-by-interval ownership
        // comparison the check performs.
        if !self.charge(
            projected
                .len()
                .saturating_mul(ownership[side].accepted.len())
                .saturating_add(projected.len()),
        ) {
            return Ok(None);
        }
        Ok(Some(projected.iter().all(|interval| {
            ownership[side].accepted.iter().any(|accepted| {
                accepted.block_index == interval.block_index
                    && accepted.start <= interval.start
                    && interval.end <= accepted.end
            })
        })))
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
        self.discover_anchored_translations(ownership)?;
        let mut processed = 0usize;
        loop {
            let mut order = (processed..self.local_domains.len()).collect::<Vec<_>>();
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
                    return Ok(false);
                }
                match self.recover_local_domain(index, ownership, changes, candidates)? {
                    LocalRecoveryStep::Continue => {}
                    // A stop is final: the original contract returns from the
                    // whole recovery at once, so no further domain is proven
                    // and no ownership is updated after it.
                    LocalRecoveryStep::Stop { truncated } => {
                        return Ok(truncated);
                    }
                }
            }
            processed = self.local_domains.len();
            if self.remaining_work == 0 || self.output_stop.is_some() {
                return Ok(false);
            }
            let before = self.local_domains.len();
            self.discover_bracketed_domains(ownership)?;
            if self.local_domains.len() == before {
                break;
            }
        }
        Ok(false)
    }

    /// Adopts the strict-closed equal fragments that the local recovery
    /// recorded for the deferred source/position proof.
    ///
    /// The pass runs after every existing recovery, proof and emission pass,
    /// so it can only spend the budget those passes left over; an exhausted
    /// budget leaves the remaining fragments pending instead of demoting or
    /// rewriting any record. Every fragment is re-checked against the latest
    /// changed ownership, the tentative candidates and the proven changed
    /// regions before the proof runs, and only a complete proof commits its
    /// projected intervals and premise. The order follows the recording
    /// order, so the outcome is deterministic.
    pub(super) fn recover_equal_fragments(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
    ) -> Result<()> {
        if self.remaining_work == 0 || self.equal_fragment_candidates.is_empty() {
            return Ok(());
        }
        let limit = self.options.max_assessment_ranges;
        // The original contiguous pass runs first in recording order over the
        // existing candidate list. Only candidates that held on a raw cut are
        // compacted, in that same order, into the prefix of the list; the
        // fallback pass then revisits exactly those slots with the internal
        // deleted soft line break allowance. A candidate held on any other
        // check is never retried, so it can neither spend fallback budget nor
        // be reordered, and the fallback can never take budget from an earlier
        // correct proof. Compaction writes each kept slot to a position at or
        // before the slot just read, so it allocates nothing and never
        // overwrites an entry the first pass has not yet visited.
        let mut retry_len = 0;
        for read in 0..self.equal_fragment_candidates.len() {
            match self.retry_equal_fragment(ownership, candidates, proven, limit, read, false)? {
                EqualFragmentRetry::Adopted | EqualFragmentRetry::Held => {}
                EqualFragmentRetry::HeldRawCut => {
                    self.equal_fragment_candidates[retry_len] =
                        self.equal_fragment_candidates[read];
                    retry_len += 1;
                }
                EqualFragmentRetry::Stop => return Ok(()),
            }
        }
        for slot in 0..retry_len {
            match self.retry_equal_fragment(ownership, candidates, proven, limit, slot, true)? {
                EqualFragmentRetry::Adopted
                | EqualFragmentRetry::Held
                | EqualFragmentRetry::HeldRawCut => {}
                EqualFragmentRetry::Stop => return Ok(()),
            }
        }
        Ok(())
    }

    /// Reconstructs eligible parent claims from immutable records after the
    /// original candidate list has been compacted for its raw-cut fallback.
    /// A recorded flag enables this scan, but supplies no correspondence.
    /// Every lookup revalidates the exact cached parent and the current
    /// ownership, partitions, candidates and whole unresolved regions.
    pub(super) fn recover_coupled_equal_parents(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        publication: CoupledPublication<'_>,
        limit: usize,
    ) -> Result<bool> {
        if !self.has_coupled_equal_candidate
            || self.remaining_work == 0
            || self.output_stop.is_some()
        {
            return Ok(false);
        }
        let context_peak = publication.partitions[0]
            .capacity()
            .saturating_add(publication.partitions[1].capacity())
            .saturating_add(publication.partitions[0].len())
            .saturating_add(publication.partitions[1].len())
            .saturating_mul(std::mem::size_of::<super::ResolutionRange>())
            .saturating_add(publication.unresolved.len());
        let Some(mut scoped_context) = super::context_projection::ContextProjections::new(
            publication.unresolved,
            self.sides,
            (64usize * 1024 * 1024).saturating_sub(context_peak),
            &mut self.remaining_work,
        )?
        else {
            return Ok(false);
        };
        let publication = CoupledContextPublication {
            domains: publication.domains,
            partitions: publication.partitions,
            unresolved: &mut scoped_context,
        };
        let count = self.records.len();
        let setup = publication.partitions[0]
            .len()
            .saturating_add(publication.partitions[1].len())
            .saturating_add(publication.unresolved.len());
        if !self.charge(count.saturating_add(setup.saturating_mul(4))) {
            return Ok(false);
        }
        if publication.unresolved.len() > limit {
            return Ok(false);
        }
        let domains = publication.domains;
        let [old_partition, new_partition] = publication.partitions;
        let regions = publication.unresolved;
        let mut adopted = false;
        let mut nominated = false;
        for relation in 0..count {
            let record = &self.records[relation];
            if record.outcome != RelationOutcome::Established
                || record.search != SearchCompleteness::Complete
                || !record.reasons.is_empty()
                || record.parent.is_none_or(|parent| parent >= relation)
                || record
                    .old_span
                    .as_ref()
                    .is_none_or(|span| span.blocks.len() < 2)
                || record
                    .new_span
                    .as_ref()
                    .is_none_or(|span| span.blocks.len() < 2)
            {
                continue;
            }
            match self.retry_recorded_equal_fragment(
                ownership,
                candidates,
                proven,
                limit,
                relation,
                EqualFragmentProof::CoupledParent(CoupledContextPublication {
                    domains,
                    partitions: [&mut *old_partition, &mut *new_partition],
                    unresolved: &mut *regions,
                }),
            )? {
                EqualFragmentRetry::Adopted => {
                    adopted = true;
                    if !nominated {
                        nominated = true;
                        if let Some(prefixes) = &mut self.global_prefixes
                            && let Some(nomination) = prefixes.nominate(&mut self.remaining_work)?
                        {
                            if !self.charge(count.saturating_mul(8).saturating_add(64)) {
                                return Ok(adopted);
                            }
                            let relation = self.records[..count].iter().position(|record| {
                                record.outcome == RelationOutcome::Established
                                    && record.search == SearchCompleteness::Complete
                                    && record.reasons.is_empty()
                                    && record.parent.is_some()
                                    && record
                                        .old_span
                                        .as_ref()
                                        .is_some_and(|span| span == &nomination.old_span)
                                    && record
                                        .new_span
                                        .as_ref()
                                        .is_some_and(|span| span == &nomination.new_span)
                            });
                            if let Some(relation) = relation {
                                match self.retry_global_extension(
                                    ownership,
                                    candidates,
                                    proven,
                                    CoupledContextPublication {
                                        domains,
                                        partitions: [&mut *old_partition, &mut *new_partition],
                                        unresolved: &mut *regions,
                                    },
                                    limit,
                                    relation,
                                )? {
                                    EqualFragmentRetry::Adopted => adopted = true,
                                    EqualFragmentRetry::Stop => return Ok(adopted),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                EqualFragmentRetry::Stop => break,
                _ => {}
            }
        }
        Ok(adopted)
    }

    /// A held original prefix may nominate a new exact domain using its actual
    /// completed seed. All current vetoes and unsplit publication still apply.
    fn retry_global_extension(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        publication: CoupledContextPublication<'_>,
        limit: usize,
        relation: usize,
    ) -> Result<EqualFragmentRetry> {
        let Some(recovery) = self.recovery else {
            return Ok(EqualFragmentRetry::Held);
        };
        let Some(generation) = &self.global_generation else {
            return Ok(EqualFragmentRetry::Held);
        };
        let Some(prefixes) = &mut self.global_prefixes else {
            return Ok(EqualFragmentRetry::Held);
        };
        let (Some(old), Some(new)) = (
            &self.records[relation].old_span,
            &self.records[relation].new_span,
        ) else {
            return Ok(EqualFragmentRetry::Held);
        };
        let Some(extension) = prefixes.extension(
            generation,
            self.sides,
            recovery,
            [old, new],
            self.options.max_assessment_ranges,
            &mut self.remaining_work,
        )?
        else {
            return Ok(if self.remaining_work == 0 {
                EqualFragmentRetry::Stop
            } else {
                EqualFragmentRetry::Held
            });
        };
        if self.output_stop.is_some()
            || self.records.len().saturating_add(2) >= self.options.max_assessment_ranges
        {
            return Ok(EqualFragmentRetry::Held);
        }
        let spans = [&extension.domain.old_span, &extension.domain.new_span];
        let identifiers = spans.iter().map(|span| span.blocks.len()).sum::<usize>();
        let setup = self
            .alignment
            .spans
            .len()
            .saturating_mul(8)
            .saturating_add(
                self.sides
                    .iter()
                    .map(|side| side.blocks.len())
                    .sum::<usize>()
                    .saturating_mul(4),
            )
            .saturating_add(self.records.len().saturating_mul(2))
            .saturating_add(identifiers.saturating_mul(64))
            .saturating_add(128);
        if !self.charge(setup) {
            return Ok(EqualFragmentRetry::Stop);
        }
        // Reconstruct the original member sequence with exact retained cuts;
        // the longer sequence must still be equal before it gains any parent.
        match super::paid_extension_groups(self.sides, spans, &mut self.remaining_work) {
            Ok(_) => {}
            Err(super::MaterializationRefusal::Work) => return Ok(EqualFragmentRetry::Stop),
            Err(_) => return Ok(EqualFragmentRetry::Held),
        }
        let span_indices = occurrence_indices(self.alignment, [Some(spans[0]), Some(spans[1])]);
        let proposal = ProposedRelation {
            old: Some(copy_span(spans[0])?),
            new: Some(copy_span(spans[1])?),
            span_indices,
            exact_recovery: false,
        };
        self.active_global_extension = Some(extension);
        let result = self.assess(&proposal);
        self.active_global_extension = None;
        let extended_relation = result?;
        // This is a fresh child of a newly proved exact key. An old parent's
        // strict uniqueness and physical receipt cannot authorize it.
        self.retry_recorded_equal_fragment(
            ownership,
            candidates,
            proven,
            limit,
            extended_relation,
            EqualFragmentProof::CoupledParent(publication),
        )
    }

    /// Retries one strict-closed recorded candidate with the contiguous proof
    /// or, after the whole original pass, with the internal deleted soft line
    /// break proof.
    ///
    /// Every retry repeats the full latest-ownership, candidate, proven-region,
    /// containment and range-limit veto sequence, so an adoption never rewrites
    /// an earlier record or contradicts a later change. An exhausted retry
    /// stops the pass without adding ownership or a premise, and a held retry
    /// leaves the candidate pending.
    fn retry_equal_fragment(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        limit: usize,
        index: usize,
        allow_internal_deleted_break: bool,
    ) -> Result<EqualFragmentRetry> {
        let proof = if allow_internal_deleted_break {
            EqualFragmentProof::InternalDeletedBreak
        } else {
            EqualFragmentProof::Contiguous
        };
        self.retry_recorded_equal_fragment(
            ownership,
            candidates,
            proven,
            limit,
            self.equal_fragment_candidates[index],
            proof,
        )
    }

    /// Runs identical, whole-parent ownership vetoes before any selected proof.
    /// A successful coupled proof commits all intervals in one adoption; a hold
    /// or work stop never grants ownership to a component on its own.
    fn retry_recorded_equal_fragment(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        limit: usize,
        relation: usize,
        mut proof: EqualFragmentProof<'_>,
    ) -> Result<EqualFragmentRetry> {
        if self.remaining_work == 0 || self.output_stop.is_some() {
            return Ok(EqualFragmentRetry::Stop);
        }
        if self.records[relation].outcome != RelationOutcome::Established {
            return Ok(EqualFragmentRetry::Held);
        }
        // The relation already stores the span pair. Pay for the transient
        // copy before it allocates, so a wide container domain can never
        // amplify the deferred list itself.
        let coupled = matches!(proof, EqualFragmentProof::CoupledParent(_));
        let copy_work = self.records[relation]
            .old_span
            .as_ref()
            .map_or(0, |span| span.blocks.len())
            .saturating_add(
                self.records[relation]
                    .new_span
                    .as_ref()
                    .map_or(0, |span| span.blocks.len()),
            );
        if !self.charge(copy_work) {
            return Ok(EqualFragmentRetry::Stop);
        }
        let Some((old_span, new_span)) = self.copy_recorded_spans(relation)? else {
            return Ok(EqualFragmentRetry::Held);
        };
        let spans = [&old_span, &new_span];
        let accepted = [
            project(self.sides[0], spans[0])?,
            project(self.sides[1], spans[1])?,
        ];
        if let EqualFragmentProof::CoupledParent(context) = &proof {
            match coupled_projection_covers_parent(
                [&accepted[0], &accepted[1]],
                spans,
                &mut self.remaining_work,
            ) {
                Some(true) => {}
                Some(false) => return Ok(EqualFragmentRetry::Held),
                None => return Ok(EqualFragmentRetry::Stop),
            }
            match self.coupled_parent_has_strict_premise(relation, context.domains) {
                Some(true) => {}
                Some(false) => return Ok(EqualFragmentRetry::Held),
                None => return Ok(EqualFragmentRetry::Stop),
            }
        }
        // The latest changed ownership still wins over a new equality.
        let mut conflict = false;
        for side in 0..2 {
            let Some(overlap) = overlaps(
                &accepted[side],
                &ownership[side].changed,
                &mut self.remaining_work,
            ) else {
                return Ok(EqualFragmentRetry::Stop);
            };
            conflict |= overlap;
        }
        if conflict {
            return Ok(EqualFragmentRetry::Held);
        }
        // Tentative candidates still own every source range they claim.
        let mut candidate_conflict = false;
        'candidates: for candidate in candidates {
            for occurrence in &candidate.change.occurrences {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    let Some(span) = span else {
                        continue;
                    };
                    if !self.charge(span.blocks.len()) {
                        return Ok(EqualFragmentRetry::Stop);
                    }
                    let source = project(self.sides[side], span)?;
                    let Some(overlap) =
                        overlaps(&source, &accepted[side], &mut self.remaining_work)
                    else {
                        return Ok(EqualFragmentRetry::Stop);
                    };
                    if overlap {
                        candidate_conflict = true;
                        break 'candidates;
                    }
                }
            }
        }
        if candidate_conflict {
            return Ok(EqualFragmentRetry::Held);
        }
        // A proven changed region keeps its whole span unresolved, so a
        // fragment that would resolve part of it is never adopted.
        let mut proven_conflict = false;
        'proven: for region in proven {
            for (side, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
                .into_iter()
                .enumerate()
            {
                let Some(span) = span else {
                    continue;
                };
                if !self.charge(span.blocks.len()) {
                    return Ok(EqualFragmentRetry::Stop);
                }
                let source = project(self.sides[side], span)?;
                let Some(overlap) = overlaps(&source, &accepted[side], &mut self.remaining_work)
                else {
                    return Ok(EqualFragmentRetry::Stop);
                };
                if overlap {
                    proven_conflict = true;
                    break 'proven;
                }
            }
        }
        if proven_conflict {
            return Ok(EqualFragmentRetry::Held);
        }
        // A span whose projected intervals are already contained in the
        // accepted ownership needs no repeated proof or premise.
        let mut owned = true;
        for (side, span) in spans.into_iter().enumerate() {
            let Some(contains) = self.ownership_contains(ownership, side, span)? else {
                return Ok(EqualFragmentRetry::Stop);
            };
            owned &= contains;
        }
        if owned {
            return Ok(EqualFragmentRetry::Held);
        }
        let fits = (0..2).all(|side| {
            ownership[side]
                .accepted
                .len()
                .saturating_add(accepted[side].len())
                <= limit
        });
        if !fits {
            return Ok(EqualFragmentRetry::Held);
        }
        let certified = if coupled {
            let EqualFragmentProof::CoupledParent(context) = &proof else {
                return Ok(EqualFragmentRetry::Held);
            };
            match self.prove_coupled_equal_parent(relation, spans, context.domains)? {
                super::equal_fragment::FragmentVerdict::Proven => false,
                super::equal_fragment::FragmentVerdict::Held(_) => {
                    return Ok(EqualFragmentRetry::Held);
                }
                super::equal_fragment::FragmentVerdict::Exhausted => {
                    return Ok(EqualFragmentRetry::Stop);
                }
            }
        } else {
            let cache = self
                .equal_fragment_cache
                .get_or_insert_with(|| super::equal_fragment::EqualFragmentCache::new(self.sides));
            if matches!(proof, EqualFragmentProof::InternalDeletedBreak) {
                match cache.prove_internal_deleted_soft_line_break_with_certificate(
                    spans,
                    &mut self.remaining_work,
                )? {
                    (super::equal_fragment::FragmentVerdict::Proven, certified) => certified,
                    (super::equal_fragment::FragmentVerdict::Held(_), _) => {
                        return Ok(EqualFragmentRetry::Held);
                    }
                    (super::equal_fragment::FragmentVerdict::Exhausted, _) => {
                        return Ok(EqualFragmentRetry::Stop);
                    }
                }
            } else {
                match cache.prove(spans, &mut self.remaining_work)? {
                    super::equal_fragment::FragmentVerdict::Proven => false,
                    super::equal_fragment::FragmentVerdict::Held(hold) => {
                        return Ok(match hold {
                            super::equal_fragment::FragmentHold::RawCut => {
                                EqualFragmentRetry::HeldRawCut
                            }
                            _ => EqualFragmentRetry::Held,
                        });
                    }
                    super::equal_fragment::FragmentVerdict::Exhausted => {
                        return Ok(EqualFragmentRetry::Stop);
                    }
                }
            }
        };
        let updated = if coupled {
            let EqualFragmentProof::CoupledParent(context) = &mut proof else {
                return Ok(EqualFragmentRetry::Held);
            };
            let parts = &context.partitions;
            let publication_work = parts[0]
                .len()
                .saturating_mul(accepted[0].len().saturating_add(8))
                .saturating_add(
                    parts[1]
                        .len()
                        .saturating_mul(accepted[1].len().saturating_add(8)),
                )
                .saturating_add(ownership[0].accepted.len())
                .saturating_add(ownership[1].accepted.len())
                .saturating_add(accepted[0].len())
                .saturating_add(accepted[1].len())
                .saturating_add(self.records[relation].assumptions.len())
                .saturating_add(8);
            if !self.charge(publication_work) {
                return Ok(EqualFragmentRetry::Stop);
            }
            let slots = parts[0]
                .capacity()
                .saturating_add(parts[1].capacity())
                .saturating_add(parts[0].len())
                .saturating_add(parts[1].len());
            let bound = 64 * 1024 * 1024;
            if slots
                .saturating_mul(std::mem::size_of::<super::ResolutionRange>())
                .saturating_add(context.unresolved.len())
                .saturating_add(context.unresolved.retained_bytes())
                > bound
            {
                return Ok(EqualFragmentRetry::Held);
            }
            let context_peak = slots
                .saturating_mul(std::mem::size_of::<super::ResolutionRange>())
                .saturating_add(context.unresolved.len());
            if !context
                .unresolved
                .limit_retained_bytes(bound.saturating_sub(context_peak))
            {
                return Ok(EqualFragmentRetry::Held);
            }
            let Some(old) = coupled_partition(parts[0], self.sides[0], &accepted[0], limit)? else {
                return Ok(EqualFragmentRetry::Held);
            };
            let Some(new) = coupled_partition(parts[1], self.sides[1], &accepted[1], limit)? else {
                return Ok(EqualFragmentRetry::Held);
            };
            let Some(removed) = context.unresolved.removals(
                self.sides,
                &accepted,
                limit,
                &mut self.remaining_work,
            )?
            else {
                return Ok(if self.remaining_work == 0 {
                    EqualFragmentRetry::Stop
                } else {
                    EqualFragmentRetry::Held
                });
            };
            let actual = parts[0]
                .capacity()
                .saturating_add(parts[1].capacity())
                .saturating_add(old.capacity())
                .saturating_add(new.capacity());
            if actual
                .saturating_mul(std::mem::size_of::<super::ResolutionRange>())
                .saturating_add(removed.capacity())
                .saturating_add(context.unresolved.retained_bytes())
                > bound
            {
                return Ok(EqualFragmentRetry::Held);
            }
            if !context.unresolved.prepay_retain(&mut self.remaining_work) {
                return Ok(EqualFragmentRetry::Stop);
            }
            self.records[relation]
                .assumptions
                .try_reserve(1)
                .map_err(|_| super::allocation_error("coupled parent premise"))?;
            Some(([old, new], removed))
        } else {
            None
        };
        // Reserve both sides before publishing either ownership population.
        for (owner, accepted_side) in ownership.iter_mut().zip(&accepted) {
            super::reserve_ranges(&mut owner.accepted, accepted_side.len(), limit)?;
        }
        for (owner, accepted_side) in ownership.iter_mut().zip(&accepted) {
            owner.accepted.extend(accepted_side.iter().copied());
        }
        if let (EqualFragmentProof::CoupledParent(context), Some(([old, new], removed))) =
            (proof, updated)
        {
            *context.partitions[0] = old;
            *context.partitions[1] = new;
            context.unresolved.retain_removed(&removed);
        }
        self.records[relation].assumptions.push(if coupled {
            ComparisonAssumption::EqualFragmentCoupledSourcePositions
        } else if certified {
            ComparisonAssumption::EqualFragmentInternalDeletedBreak
        } else {
            ComparisonAssumption::EqualFragmentSourcePositions
        });
        Ok(EqualFragmentRetry::Adopted)
    }

    /// Removes only complete final diagnostics fully covered on both sides.
    /// Partial regions, extraction gaps and normalization issues keep the whole
    /// parent held. Every projection, comparison, mark and later retain move is
    /// paid before it occurs; surviving diagnostics are never regenerated.
    pub(super) fn coupled_region_removals(
        &mut self,
        regions: &[super::UnresolvedRegion],
        accepted: &[Vec<SourceInterval>; 2],
        limit: usize,
    ) -> Result<Option<Vec<bool>>> {
        if regions.len() > limit || !self.charge(regions.len().saturating_mul(3)) {
            return Ok(None);
        }
        let mut removed = Vec::new();
        removed
            .try_reserve_exact(regions.len())
            .map_err(|_| super::allocation_error("coupled region marks"))?;
        for region in regions {
            if !self.charge(region.evidence.len()) {
                return Ok(None);
            }
            let mut overlapping = false;
            let mut covered = true;
            for (side, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
                .into_iter()
                .enumerate()
            {
                let Some(span) = span else {
                    continue;
                };
                if !self.charge(span.blocks.len().saturating_mul(2)) {
                    return Ok(None);
                }
                let work = self.sides[side]
                    .source_token_count(&span.blocks)
                    .saturating_add(
                        accepted[side]
                            .len()
                            .saturating_mul(span.blocks.len())
                            .saturating_mul(2),
                    );
                if !self.charge(work) {
                    return Ok(None);
                }
                let projected = project(self.sides[side], span)?;
                covered &= !projected.is_empty();
                for part in &projected {
                    overlapping |= accepted[side].iter().any(|range| {
                        range.block_index == part.block_index
                            && range.start < part.end
                            && part.start < range.end
                    });
                    covered &= accepted[side].iter().any(|range| {
                        range.block_index == part.block_index
                            && range.start <= part.start
                            && part.end <= range.end
                    });
                }
            }
            if overlapping
                && (!covered
                    || region.evidence.iter().any(|evidence| {
                        matches!(
                            evidence,
                            crate::alignment::AlignmentEvidence::ExtractionGap
                                | crate::alignment::AlignmentEvidence::NormalizationIssue
                        )
                    }))
            {
                return Ok(None);
            }
            removed.push(overlapping);
        }
        Ok(Some(removed))
    }

    /// Requires an existing exact, complete, strict-unique equal parent whose
    /// recorded spans are this entire child. An equal N/N parent has only the
    /// diagonal full-cardinality matching, so every token pair is mandatory.
    /// The coupled source proof must then cover all N tokens in that order.
    /// Scores, component guards, the enable flag and a merely established
    /// boundary cannot supply this parent proof.
    /// Rejects impossible coupled proofs before projecting unrelated vetoes.
    /// This paid header check grants no correspondence or ownership: the full
    /// exact-key, source, context and current ownership proof still follows.
    fn coupled_parent_has_strict_premise(
        &mut self,
        relation: usize,
        domains: &[(super::DomainKey, super::DomainProof)],
    ) -> Option<bool> {
        if !self.charge(8) {
            return None;
        }
        let Some(parent) = self.records[relation]
            .parent
            .filter(|parent| *parent < relation)
        else {
            return Some(false);
        };
        let parent_record = &self.records[parent];
        if parent_record.outcome != RelationOutcome::Established
            || parent_record.search != SearchCompleteness::Complete
            || !parent_record.reasons.is_empty()
        {
            return Some(false);
        }
        if !self.charge(domains.len()) {
            return None;
        }
        let reviewed = domains.iter().find(|(_, proof)| proof.relation == parent);
        let retained = if reviewed.is_some() {
            reviewed.map(|(_, proof)| proof)
        } else {
            if !self.charge(self.domains.len()) {
                return None;
            }
            self.domains.values().find(|proof| proof.relation == parent)
        };
        if !charge(&mut self.remaining_work, 8) {
            return None;
        }
        Some(retained.is_some_and(|proof| {
            proof.scope == super::ProofScope::ExactKey
                && proof.strict_unique
                && proof.unique
                && proof.edits.is_empty()
                && proof.search == SearchCompleteness::Complete
        }))
    }

    fn prove_coupled_equal_parent(
        &mut self,
        relation: usize,
        spans: [&TextSpan; 2],
        domains: &[(super::DomainKey, super::DomainProof)],
    ) -> Result<super::equal_fragment::FragmentVerdict> {
        use super::equal_fragment::{FragmentHold, FragmentVerdict};
        let held = FragmentVerdict::Held(FragmentHold::ParentCorrespondence);
        let record = &self.records[relation];
        let Some(parent) = record.parent.filter(|parent| *parent < relation) else {
            return Ok(held);
        };
        if record.outcome != RelationOutcome::Established
            || record.search != SearchCompleteness::Complete
            || !record.reasons.is_empty()
        {
            return Ok(held);
        }
        // Prepay span copying, extents, key hashing/equality and parent-span
        // comparisons. The exact key neither searches another domain nor
        // rebuilds the container's token or position vectors.
        let key_work = spans
            .iter()
            .map(|span| span.blocks.len())
            .sum::<usize>()
            .saturating_mul(5)
            .saturating_add(32)
            .saturating_add(domains.len());
        if !self.charge(key_work) {
            return Ok(FragmentVerdict::Exhausted);
        }
        let key = super::DomainKey {
            local: Some((copy_span(spans[0])?, copy_span(spans[1])?)),
            old: super::block_extent(self.sides[0], spans[0])?,
            new: super::block_extent(self.sides[1], spans[1])?,
            old_separator: spans[0].separator.unwrap_or(BlockSeparator::Space),
            new_separator: spans[1].separator.unwrap_or(BlockSeparator::Space),
        };
        // Review moves the original cache. Borrow its retained proofs after
        // all earlier consumers finish; never reconstruct or relax a proof.
        // The prepaid scan inspects relation identities, then checks the full
        // exact key only on the matching parent.
        let reviewed = domains
            .iter()
            .find(|(_, proof)| proof.relation == parent)
            .map(|(key, proof)| (key, proof));
        let retained = if reviewed.is_some() {
            reviewed
        } else {
            // This second cache belongs to the current assessor. Only a new
            // parent missing from the reviewed cache performs and pays this scan.
            if !self.charge(self.domains.len().saturating_add(8)) {
                return Ok(FragmentVerdict::Exhausted);
            }
            self.domains
                .iter()
                .find(|(_, proof)| proof.relation == parent)
        };
        let Some((parent_key, proof)) = retained else {
            return Ok(held);
        };
        if *parent_key != key {
            return Ok(held);
        }
        let cache = self
            .equal_fragment_cache
            .get_or_insert_with(|| super::equal_fragment::EqualFragmentCache::new(self.sides));
        cache.prove_coupled_positioned_parent(
            spans,
            &self.records[parent],
            proof,
            &mut self.remaining_work,
        )
    }

    /// Copies one recorded relation's span pair with bounded allocation.
    ///
    /// The relation keeps the only long-lived copy of the spans; this helper
    /// returns the transient copy the tail pass needs while it projects and
    /// re-checks ownership. Every allocation is fallible, and the caller has
    /// already charged the shared budget for the copy work.
    pub(super) fn copy_recorded_spans(
        &self,
        relation: usize,
    ) -> Result<Option<(TextSpan, TextSpan)>> {
        let (Some(old), Some(new)) = (
            self.records[relation].old_span.as_ref(),
            self.records[relation].new_span.as_ref(),
        ) else {
            return Ok(None);
        };
        Ok(Some((copy_span(old)?, copy_span(new)?)))
    }

    /// Processes one discovered local domain: proves it, protects tentative
    /// candidates and commits either an equal range or a localized edit
    /// script. `Stop` tells the caller not to continue; `truncated` reports
    /// whether output limits cut the recovery short.
    fn recover_local_domain(
        &mut self,
        index: usize,
        ownership: &mut [Ownership; 2],
        changes: &mut Vec<ChangeEvent>,
        candidates: &mut Vec<ChangeCandidate>,
    ) -> Result<LocalRecoveryStep> {
        let domain = self.local_domains[index].clone();
        let spans = [&domain.old_span, &domain.new_span];
        if !self.charge(spans.iter().map(|span| span.blocks.len()).sum()) {
            return Ok(LocalRecoveryStep::Stop { truncated: false });
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
                return Ok(LocalRecoveryStep::Stop { truncated: false });
            };
            conflict |= overlap;
        }
        if conflict {
            return Ok(LocalRecoveryStep::Continue);
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
            return Ok(LocalRecoveryStep::Continue);
        }
        let key = self.domain_key(&proposal)?;
        if self
            .domains
            .get(&key)
            .is_some_and(|proof| proof.scope != super::ProofScope::ExactKey)
        {
            return Ok(LocalRecoveryStep::Continue);
        }
        let groups = match self.scoped_proof_groups(&key)? {
            Ok(groups) => groups,
            Err(refusal) => {
                let reason = if refusal == super::MaterializationRefusal::Work {
                    super::AssessmentReason::WorkLimit
                } else {
                    super::AssessmentReason::SearchIncomplete
                };
                self.mark_recovery_incomplete(relation, reason)?;
                return Ok(LocalRecoveryStep::Continue);
            }
        };
        let proof = &self.domains[&key];
        if proof.edits.is_empty() {
            // Only complete source-bounded singleton domains may publish
            // an equal range without an edit script. Trusted-run fragments,
            // ordered and footer domains keep their existing obligations.
            let complete = self.records[relation].search == super::SearchCompleteness::Complete;
            if !domain.source_bounded || !complete {
                // A strict-closed domain is only recorded here. Its
                // source/position-complete proof runs in the deferred tail
                // pass, which re-checks the latest ownership after every
                // existing recovery and emission pass and spends only the
                // budget those passes left over. This early pass adopts
                // nothing, charges nothing extra and promises nothing.
                let strict_closed = !domain.source_bounded
                    && complete
                    && proof.strict_unique
                    && proof.unique
                    && proof.search == super::SearchCompleteness::Complete;
                if strict_closed && proposal.old.is_some() && proposal.new.is_some() {
                    // The established relation already owns the span pair, so
                    // the deferred list keeps only its index and never stores
                    // another variable-length copy of the source spans.
                    self.equal_fragment_candidates
                        .try_reserve(1)
                        .map_err(|_| super::allocation_error("equal fragment candidates"))?;
                    self.equal_fragment_candidates.push(relation);
                    self.has_coupled_equal_candidate |= proposal
                        .old
                        .as_ref()
                        .is_some_and(|span| span.blocks.len() > 1)
                        && proposal
                            .new
                            .as_ref()
                            .is_some_and(|span| span.blocks.len() > 1);
                }
                return Ok(LocalRecoveryStep::Continue);
            }
            // Protect tentative candidates: an accepted equality must not
            // swallow source ranges a candidate still claims.
            let mut candidate_conflict = false;
            for candidate in candidates.iter() {
                for occurrence in &candidate.change.occurrences {
                    for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                        .into_iter()
                        .enumerate()
                    {
                        let Some(span) = span else {
                            continue;
                        };
                        if !self.charge(span.blocks.len()) {
                            self.mark_local_work_limit(relation);
                            return Ok(LocalRecoveryStep::Stop { truncated: false });
                        }
                        let source = project(self.sides[side], span)?;
                        let Some(overlap) =
                            overlaps(&source, &accepted[side], &mut self.remaining_work)
                        else {
                            self.mark_local_work_limit(relation);
                            return Ok(LocalRecoveryStep::Stop { truncated: false });
                        };
                        candidate_conflict |= overlap;
                    }
                }
            }
            if candidate_conflict {
                return Ok(LocalRecoveryStep::Continue);
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
                return Ok(LocalRecoveryStep::Continue);
            }
            for (owner, accepted_side) in ownership.iter_mut().zip(&accepted) {
                super::reserve_ranges(&mut owner.accepted, accepted_side.len(), limit)?;
                owner.accepted.extend(accepted_side.iter().copied());
            }
            return Ok(LocalRecoveryStep::Continue);
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
            return Ok(LocalRecoveryStep::Stop {
                truncated: output_limit,
            });
        }
        if !self.validate_semantic_emission(relation, &local_changes)? {
            return Ok(LocalRecoveryStep::Continue);
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
                            return Ok(LocalRecoveryStep::Stop { truncated: false });
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
            return Ok(LocalRecoveryStep::Continue);
        }
        let local_changes = backed_changes;
        for side in 0..2 {
            let Some(overlap) = overlaps(
                &changed[side],
                &ownership[side].accepted,
                &mut self.remaining_work,
            ) else {
                self.mark_local_work_limit(relation);
                return Ok(LocalRecoveryStep::Stop { truncated: false });
            };
            conflict |= overlap;
        }
        if conflict {
            return Ok(LocalRecoveryStep::Continue);
        }
        let mut superseded = Vec::new();
        let mut residue_updates = Vec::new();
        let mut residue_live_bytes = 0usize;
        for (index, candidate) in candidates.iter().enumerate() {
            // Cover the additional containment bookkeeping even when a
            // candidate projects to no nonempty source interval.
            if !self.charge(8) {
                self.mark_local_work_limit(relation);
                return Ok(LocalRecoveryStep::Stop { truncated: false });
            }
            let mut overlaps_domain = false;
            let mut wholly_covered = true;
            for occurrence in &candidate.change.occurrences {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        if !self.charge(span.blocks.len()) {
                            self.mark_local_work_limit(relation);
                            return Ok(LocalRecoveryStep::Stop { truncated: false });
                        }
                        let source = project(self.sides[side], span)?;
                        let Some(overlap) =
                            overlaps(&source, &accepted[side], &mut self.remaining_work)
                        else {
                            self.mark_local_work_limit(relation);
                            return Ok(LocalRecoveryStep::Stop { truncated: false });
                        };
                        overlaps_domain |= overlap;
                        // An overlap does not discharge the candidate's
                        // remaining source obligations. Pay the complete
                        // containment scan before deciding to replace it.
                        let coverage_cost = source
                            .len()
                            .saturating_mul(accepted[side].len().saturating_add(1))
                            .saturating_mul(8);
                        if !self.charge(coverage_cost) {
                            self.mark_local_work_limit(relation);
                            return Ok(LocalRecoveryStep::Stop { truncated: false });
                        }
                        wholly_covered &= source.iter().all(|interval| {
                            accepted[side].iter().any(|range| {
                                range.block_index == interval.block_index
                                    && range.start <= interval.start
                                    && interval.end <= range.end
                            })
                        });
                    }
                }
            }
            if overlaps_domain && !wholly_covered {
                if !self.charge(256usize.saturating_add(candidates.len().saturating_mul(32))) {
                    self.mark_local_work_limit(relation);
                    return Ok(LocalRecoveryStep::Stop { truncated: false });
                }
                let mut occurrences = Vec::new();
                let mut occurrence_body_bytes = 0usize;
                for occurrence in &candidate.change.occurrences {
                    if !super::super::valid_change_occurrence_shape(
                        candidate.change.kind,
                        occurrence.old_span.as_ref(),
                        occurrence.new_span.as_ref(),
                    ) {
                        return Err(super::invalid(
                            "candidate residue has an invalid original occurrence shape",
                        ));
                    }
                    let strict = self.domains[&key].strict_unique
                        && self.domains[&key].search == SearchCompleteness::Complete;
                    let contexts = std::array::from_fn(|side| CandidateResidueSource {
                        accepted: &accepted[side],
                        exact: strict.then(|| {
                            (
                                if side == 0 {
                                    proposal.old.as_ref()
                                } else {
                                    proposal.new.as_ref()
                                }
                                .expect("local recovery has both source spans"),
                                self.domains[&key].edits.as_slice(),
                                side,
                            )
                        }),
                    });
                    let memory_limit = (64usize * 1024 * 1024)
                        .saturating_sub(residue_live_bytes)
                        .saturating_sub(occurrence_body_bytes)
                        .saturating_sub(occurrences.capacity().saturating_mul(
                            2 * std::mem::size_of::<super::super::ChangeOccurrence>(),
                        ));
                    let Some(residues) = candidate_occurrence_residue(
                        self.sides,
                        occurrence,
                        contexts,
                        self.options.max_assessment_ranges,
                        memory_limit,
                        &mut self.remaining_work,
                    )?
                    else {
                        return Ok(LocalRecoveryStep::Continue);
                    };
                    if occurrences.len().saturating_add(residues.len())
                        > self.options.max_assessment_ranges
                    {
                        return Ok(LocalRecoveryStep::Continue);
                    }
                    occurrences
                        .try_reserve_exact(residues.len())
                        .map_err(|_| super::allocation_error("candidate residue occurrences"))?;
                    for occurrence in residues {
                        if !super::super::valid_change_occurrence_shape(
                            candidate.change.kind,
                            occurrence.old_span.as_ref(),
                            occurrence.new_span.as_ref(),
                        ) {
                            return Err(super::invalid(
                                "candidate residue changed its occurrence shape",
                            ));
                        }
                        for span in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                            .into_iter()
                            .flatten()
                        {
                            occurrence_body_bytes = occurrence_body_bytes.saturating_add(
                                span.blocks
                                    .capacity()
                                    .saturating_mul(std::mem::size_of::<crate::layout::BlockId>()),
                            );
                        }
                        occurrences.push(occurrence);
                    }
                }
                let previous_capacity = residue_updates.capacity();
                super::reserve_ranges(&mut residue_updates, 1, self.options.max_assessment_ranges)?;
                residue_live_bytes = residue_live_bytes
                    .saturating_add(occurrence_body_bytes)
                    .saturating_add(
                        occurrences
                            .capacity()
                            .saturating_mul(std::mem::size_of::<super::super::ChangeOccurrence>()),
                    )
                    .saturating_add(
                        residue_updates
                            .capacity()
                            .saturating_sub(previous_capacity)
                            .saturating_mul(std::mem::size_of::<(
                                usize,
                                Vec<super::super::ChangeOccurrence>,
                            )>()),
                    );
                if residue_live_bytes > 64 * 1024 * 1024 {
                    return Ok(LocalRecoveryStep::Continue);
                }
                if occurrences.is_empty() {
                    superseded.push(index);
                } else {
                    residue_updates.push((index, occurrences));
                }
                continue;
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
            return Ok(LocalRecoveryStep::Stop { truncated: true });
        }
        if !self.charge(edit_count) {
            self.mark_local_work_limit(relation);
            return Ok(LocalRecoveryStep::Stop { truncated: false });
        }
        let anchor_bound = self.domains[&key]
            .edits
            .iter()
            .map(|edit| {
                if edit.old.is_empty() {
                    proposal.old.as_ref().map_or(0, |s| s.blocks.len())
                } else {
                    proposal.new.as_ref().map_or(0, |s| s.blocks.len())
                }
            })
            .sum::<usize>();
        if anchor_bound > limit
            || !self.charge_evidence_link(0, 128)
            || !self.charge_evidence_link(1, anchor_bound.saturating_mul(64))
            || !self.charge_evidence_link(2, superseded.len().saturating_mul(32))
            || !self.charge_evidence_link(
                3,
                edit_count
                    .saturating_mul(32)
                    .saturating_add(anchor_bound.saturating_mul(32))
                    .saturating_add(superseded.len().saturating_mul(16)),
            )
        {
            self.mark_local_work_limit(relation);
            return Ok(LocalRecoveryStep::Stop { truncated: false });
        }
        let mut origins = Vec::new();
        origins
            .try_reserve_exact(superseded.len())
            .map_err(|_| super::allocation_error("localized witness origins"))?;
        if origins.capacity() > superseded.len() {
            return Ok(LocalRecoveryStep::Continue);
        }
        origins.extend(superseded.iter().map(|&i| candidates[i].relation));
        let strict_unique = self.domains[&key].strict_unique
            && self.domains[&key].search == SearchCompleteness::Complete;
        let mut edits = Vec::new();
        edits
            .try_reserve_exact(edit_count)
            .map_err(|_| super::allocation_error("localized edit witness"))?;
        edits.extend_from_slice(&self.domains[&key].edits);
        let source_anchors = self.witness_source_anchors(relation, &edits)?;
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
            strict_unique,
            origin_relations: origins,
            source_anchors,
            edits,
        });
        for (index, occurrences) in residue_updates {
            candidates[index].change.occurrences = occurrences;
        }
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
        Ok(LocalRecoveryStep::Continue)
    }
}

#[derive(Clone, Copy)]
struct CandidateResidueSource<'a> {
    accepted: &'a [SourceInterval],
    exact: Option<(&'a TextSpan, &'a [super::super::myers::AtomicEdit], usize)>,
}
#[cfg(test)]
fn physical_candidate_residue(accepted: &[SourceInterval]) -> CandidateResidueSource<'_> {
    CandidateResidueSource {
        accepted,
        exact: None,
    }
}

/// Keeps the original association only when no fragment correspondence must
/// be selected. An original empty anchor remains at its original source frame.
fn candidate_occurrence_residue(
    sides: [&Side<'_>; 2],
    occurrence: &super::super::ChangeOccurrence,
    contexts: [CandidateResidueSource<'_>; 2],
    limit: usize,
    memory_limit: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<super::super::ChangeOccurrence>>> {
    if !charge(remaining, 256) {
        return Ok(None);
    }
    let originals = [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()];
    let mut pieces = [Vec::new(), Vec::new()];
    let mut live_bytes = 0usize;
    for which in 0..2 {
        let Some(span) = originals[which] else {
            continue;
        };
        if span.comparable_range.start == span.comparable_range.end {
            if !charge(
                remaining,
                128usize.saturating_add(span.blocks.len().saturating_mul(64)),
            ) {
                return Ok(None);
            }
            let expected = std::mem::size_of::<TextSpan>().saturating_add(
                span.blocks
                    .len()
                    .saturating_mul(std::mem::size_of::<crate::layout::BlockId>()),
            );
            if live_bytes.saturating_add(expected) > memory_limit {
                return Ok(None);
            }
            project(sides[which], span)?;
            pieces[which]
                .try_reserve_exact(1)
                .map_err(|_| super::allocation_error("candidate empty anchor"))?;
            pieces[which].push(copy_span(span)?);
        } else {
            let Some(result) = candidate_side_residue(
                sides[which],
                span,
                contexts[which],
                limit,
                memory_limit.saturating_sub(live_bytes),
                remaining,
            )?
            else {
                return Ok(None);
            };
            pieces[which] = result;
        }
        live_bytes = live_bytes
            .saturating_add(
                pieces[which]
                    .capacity()
                    .saturating_mul(std::mem::size_of::<TextSpan>()),
            )
            .saturating_add(
                pieces[which]
                    .iter()
                    .map(|s| {
                        s.blocks
                            .capacity()
                            .saturating_mul(std::mem::size_of::<crate::layout::BlockId>())
                    })
                    .sum::<usize>(),
            );
        if live_bytes > memory_limit {
            return Ok(None);
        }
    }
    let empty = |which: usize| {
        pieces[which].len() == 1
            && pieces[which][0].comparable_range.start == pieces[which][0].comparable_range.end
    };
    let count = if originals[0].is_none() {
        pieces[1].len()
    } else if originals[1].is_none() {
        pieces[0].len()
    } else if pieces.iter().all(Vec::is_empty) {
        0
    } else if pieces.iter().any(Vec::is_empty) {
        return Ok(None);
    } else if empty(0) {
        pieces[1].len()
    } else if empty(1) {
        pieces[0].len()
    } else if pieces[0].len() == 1 && pieces[1].len() == 1 {
        1
    } else {
        return Ok(None);
    };
    if count > limit {
        return Ok(None);
    }
    let repeated_blocks = if empty(0) {
        pieces[0][0].blocks.len().saturating_mul(count)
    } else if empty(1) {
        pieces[1][0].blocks.len().saturating_mul(count)
    } else {
        0
    };
    let output_bytes = count
        .saturating_mul(std::mem::size_of::<super::super::ChangeOccurrence>())
        .saturating_add(
            repeated_blocks.saturating_mul(std::mem::size_of::<crate::layout::BlockId>()),
        );
    if live_bytes.saturating_add(output_bytes) > memory_limit
        || !charge(
            remaining,
            count
                .saturating_mul(64)
                .saturating_add(repeated_blocks.saturating_mul(32))
                .saturating_add(128),
        )
    {
        return Ok(None);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| super::allocation_error("paired candidate residues"))?;
    if output.capacity() > count {
        return Ok(None);
    }
    let [mut old, mut new] = pieces;
    if originals[0].is_none() {
        output.extend(new.into_iter().map(|span| super::super::ChangeOccurrence {
            old_span: None,
            new_span: Some(span),
        }));
    } else if originals[1].is_none() {
        output.extend(old.into_iter().map(|span| super::super::ChangeOccurrence {
            old_span: Some(span),
            new_span: None,
        }));
    } else if count > 0
        && old.len() == 1
        && old[0].comparable_range.start == old[0].comparable_range.end
    {
        let anchor = old.pop().expect("one original anchor");
        for span in new {
            output.push(super::super::ChangeOccurrence {
                old_span: Some(copy_span(&anchor)?),
                new_span: Some(span),
            });
        }
    } else if count > 0
        && new.len() == 1
        && new[0].comparable_range.start == new[0].comparable_range.end
    {
        let anchor = new.pop().expect("one original anchor");
        for span in old {
            output.push(super::super::ChangeOccurrence {
                old_span: Some(span),
                new_span: Some(copy_span(&anchor)?),
            });
        }
    } else if count == 1 {
        output.push(super::super::ChangeOccurrence {
            old_span: old.pop(),
            new_span: new.pop(),
        });
    }
    Ok(Some(output))
}

/// Subtracts established source intervals without inventing a correspondence
/// between disconnected old/new remainders. Scalar coordinates and block
/// separators remain in the original candidate's coordinate frame.
/// Source coordinates validated once for one immutable side and original span.
/// The cache is private to a single residue transaction: it cannot license a
/// different block population, separator shape, source, or original range.
struct CandidateCoordinateFrame<'side, 'document> {
    side: &'side Side<'document>,
    original: &'side TextSpan,
    blocks: Vec<CandidateBlockCoordinates>,
}

struct CandidateBlockCoordinates {
    block_index: usize,
    token_start: usize,
    token_end: usize,
    scalar_start: usize,
}

impl<'side, 'document> CandidateCoordinateFrame<'side, 'document> {
    /// All construction, validation and retained storage are prepaid by the
    /// caller. Capacity is checked against the original block population.
    fn new(side: &'side Side<'document>, original: &'side TextSpan) -> Result<Option<Self>> {
        project(side, original)?;
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(original.blocks.len())
            .map_err(|_| super::allocation_error("candidate coordinate frame"))?;
        if blocks.capacity() > original.blocks.len() {
            return Ok(None);
        }
        let mut token_offset = 0usize;
        let mut scalar_offset = 0usize;
        let mut preceding_space = false;
        for (position, id) in original.blocks.iter().enumerate() {
            let block_index = side.index[id];
            let tokens = &side.canonical[block_index];
            let separator = position > 0
                && original.separator.map(|s| s.at(position - 1)) == Some(BlockSeparator::Space)
                && !preceding_space
                && !tokens.first().is_some_and(super::space_token);
            token_offset += usize::from(separator);
            scalar_offset += usize::from(separator);
            blocks.push(CandidateBlockCoordinates {
                block_index,
                token_start: token_offset,
                token_end: token_offset + tokens.len(),
                scalar_start: scalar_offset,
            });
            token_offset += tokens.len();
            scalar_offset += tokens.len() - side.blocks[block_index].canonical.unmapped.len();
            preceding_space = tokens
                .last()
                .map_or(separator || preceding_space, super::space_token);
        }
        Ok(Some(Self {
            side,
            original,
            blocks,
        }))
    }

    /// Search the validated frame, preserving zero-width unmapped glyphs and
    /// the inserted separator's scalar boundary. No new source is materialized.
    fn scalar(&self, boundary: usize) -> Result<usize> {
        if boundary < self.original.comparable_range.start
            || boundary > self.original.comparable_range.end
        {
            return Err(super::invalid(
                "candidate residue boundary leaves its original span",
            ));
        }
        let index = self.blocks.partition_point(|b| b.token_end < boundary);
        let block = self
            .blocks
            .get(index)
            .ok_or_else(|| super::invalid("candidate residue boundary leaves its source frame"))?;
        if boundary < block.token_start {
            if boundary + 1 == block.token_start {
                return Ok(block.scalar_start - 1);
            }
            return Err(super::invalid(
                "candidate residue boundary is outside its source frame",
            ));
        }
        Ok(block.scalar_start
            + super::block_scalar_boundary(
                &self.side.blocks[block.block_index].canonical,
                boundary - block.token_start,
            ))
    }

    /// Recheck physical non-emptiness and every accepted-owner intersection
    /// using the original side's validated block offsets, with unchanged strict
    /// overlap endpoints. The caller prepays every frame/owner comparison.
    fn physical_status(
        &self,
        start: usize,
        end: usize,
        accepted: &[SourceInterval],
    ) -> (bool, bool) {
        let mut present = false;
        let mut overlap = false;
        for block in &self.blocks {
            let first = start.max(block.token_start);
            let last = end.min(block.token_end);
            if first < last {
                present = true;
                overlap |= accepted.iter().any(|a| {
                    a.block_index == block.block_index
                        && first - block.token_start < a.end
                        && a.start < last - block.token_start
                });
            }
        }
        (present, overlap)
    }
}

fn candidate_side_residue(
    side: &Side<'_>,
    span: &TextSpan,
    context: CandidateResidueSource<'_>,
    limit: usize,
    memory_limit: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<TextSpan>>> {
    let accepted = context.accepted;
    let structural_slots = if context.exact.is_some() {
        span.blocks.len().saturating_sub(1)
    } else {
        0
    };
    let slots = span
        .blocks
        .len()
        .saturating_mul(accepted.len())
        .saturating_add(structural_slots);
    let fragments = slots.saturating_add(1);
    let words = span.blocks.len().saturating_mul(fragments);
    let bytes = fragments
        .saturating_mul(std::mem::size_of::<(usize, usize)>())
        .saturating_add(words.saturating_mul(256))
        .saturating_add(slots.saturating_mul(std::mem::size_of::<(usize, usize)>()))
        .saturating_add(fragments.saturating_mul(std::mem::size_of::<TextSpan>()))
        .saturating_add(words.saturating_mul(std::mem::size_of::<crate::layout::BlockId>()));
    if !charge(
        remaining,
        128usize.saturating_add(span.blocks.len().saturating_mul(32)),
    ) || fragments > limit
        || bytes > memory_limit
    {
        return Ok(None);
    }
    let token_bound = span
        .blocks
        .iter()
        .map(|id| {
            side.index
                .get(id)
                .map_or(0, |&index| side.canonical[index].len())
        })
        .sum::<usize>();
    let structural_cost = context.exact.map_or(0, |(proof, edits, _)| {
        span.blocks
            .len()
            .saturating_mul(proof.blocks.len())
            .saturating_mul(64)
            .saturating_add(
                span.blocks
                    .len()
                    .saturating_mul(edits.len())
                    .saturating_mul(16),
            )
            .saturating_add(proof.blocks.len().saturating_mul(32))
            .saturating_add(256)
    });
    let cost = structural_cost
        .saturating_add(slots.saturating_mul(16))
        .saturating_add(words.saturating_mul(32))
        // Build and validate the immutable source frame once. Fragment
        // boundaries now search its metadata instead of rescanning the source.
        .saturating_add(token_bound.saturating_mul(4))
        .saturating_add(fragments.saturating_mul(32).saturating_mul(
            span.blocks.len().checked_ilog2().unwrap_or(0) as usize
                + token_bound.checked_ilog2().unwrap_or(0) as usize
                + 2,
        ))
        // Each residue still checks physical ownership against every accepted
        // interval. Sharing coordinates never discharges that guard.
        .saturating_add(slots.saturating_mul(fragments).saturating_mul(16))
        .saturating_add(span.blocks.len().saturating_mul(32));
    if !charge(remaining, cost) {
        return Ok(None);
    }
    let Some(frame) = CandidateCoordinateFrame::new(side, span)? else {
        return Ok(None);
    };
    if let Some((proof, _, _)) = context.exact {
        project(side, proof)?;
    }
    let mut cuts = Vec::new();
    cuts.try_reserve_exact(slots)
        .map_err(|_| super::allocation_error("candidate residue cuts"))?;
    if cuts.capacity() > slots {
        return Ok(None);
    }
    let mut token_offset = 0;
    let mut preceding_space = false;
    for (position, id) in span.blocks.iter().enumerate() {
        let index = side.index[id];
        let tokens = &side.canonical[index];
        let separator = position > 0
            && span.separator.map(|s| s.at(position - 1)) == Some(BlockSeparator::Space)
            && !preceding_space
            && !tokens.first().is_some_and(super::space_token);
        if separator && let Some((proof, edits, which)) = context.exact {
            let previous = span.blocks[position - 1];
            if let Some(boundary) = proof
                .blocks
                .windows(2)
                .position(|ids| ids == [previous, *id])
            {
                let mut proof_offset = 0usize;
                let mut proof_preceding_space = false;
                for (proof_position, proof_id) in proof.blocks.iter().enumerate() {
                    let proof_tokens = &side.canonical[side.index[proof_id]];
                    let proof_separator = proof_position > 0
                        && proof.separator.map(|s| s.at(proof_position - 1))
                            == Some(BlockSeparator::Space)
                        && !proof_preceding_space
                        && !proof_tokens.first().is_some_and(super::space_token);
                    if proof_position == boundary + 1 {
                        if proof_separator
                            && proof_offset >= proof.comparable_range.start
                            && proof_offset < proof.comparable_range.end
                        {
                            let local = proof_offset - proof.comparable_range.start;
                            let changed = edits.iter().any(|edit| {
                                if which == 0 {
                                    edit.old.contains(&local)
                                } else {
                                    edit.new.contains(&local)
                                }
                            });
                            if !changed {
                                let start = token_offset.max(span.comparable_range.start);
                                let end = token_offset
                                    .saturating_add(1)
                                    .min(span.comparable_range.end);
                                if start < end {
                                    cuts.push((start, end));
                                }
                            }
                        }
                        break;
                    }
                    proof_offset += usize::from(proof_separator) + proof_tokens.len();
                    proof_preceding_space = proof_tokens
                        .last()
                        .map_or(proof_separator || proof_preceding_space, super::space_token);
                }
            }
        }
        token_offset += usize::from(separator);
        for interval in accepted.iter().filter(|r| r.block_index == index) {
            let start = (token_offset + interval.start).max(span.comparable_range.start);
            let end = (token_offset + interval.end).min(span.comparable_range.end);
            if start < end {
                cuts.push((start, end));
            }
        }
        token_offset += tokens.len();
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, super::space_token);
    }
    if !charge(
        remaining,
        cuts.len()
            .saturating_mul(cuts.len().checked_ilog2().unwrap_or(0) as usize + 1),
    ) {
        return Ok(None);
    }
    cuts.sort_unstable();
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(fragments)
        .map_err(|_| super::allocation_error("candidate residue ranges"))?;
    if ranges.capacity() > fragments {
        return Ok(None);
    }
    let mut cursor = span.comparable_range.start;
    for (start, end) in cuts {
        if cursor < start {
            ranges.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < span.comparable_range.end {
        ranges.push((cursor, span.comparable_range.end));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(ranges.len())
        .map_err(|_| super::allocation_error("candidate residue spans"))?;
    for (start, end) in ranges {
        let mut residue = copy_span(span)?;
        residue.comparable_range = super::super::TokenRange { start, end };
        residue.canonical_range = crate::normalize::ScalarRange {
            start: frame.scalar(start)?,
            end: frame.scalar(end)?,
        };
        let (physical, overlap) = frame.physical_status(start, end, accepted);
        if !physical || overlap {
            return Ok(None);
        }
        output.push(residue);
    }
    let actual_bytes = frame
        .blocks
        .capacity()
        .saturating_mul(std::mem::size_of::<CandidateBlockCoordinates>())
        .saturating_add(
            output
                .capacity()
                .saturating_mul(std::mem::size_of::<TextSpan>())
                .saturating_add(
                    output
                        .iter()
                        .map(|s| {
                            s.blocks
                                .capacity()
                                .saturating_mul(std::mem::size_of::<crate::layout::BlockId>())
                        })
                        .sum::<usize>(),
                ),
        );
    if actual_bytes > bytes {
        return Ok(None);
    }
    Ok(Some(output))
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

/// Builds an atomic final partition without splitting an unresolved range.
/// Existing states and scalar bounds are preserved. A selected unresolved
/// range must be wholly covered by one proven component, so synthetic cuts,
/// changed states and partial coverage refuse publication. Only adjacent equal
/// ranges touched by this adoption may merge; other ranges remain byte-for-byte
/// data equivalent. The caller prepays every scan, copy and capacity slot.
pub(super) fn coupled_partition(
    source: &[super::ResolutionRange],
    side: &Side<'_>,
    accepted: &[SourceInterval],
    limit: usize,
) -> Result<Option<Vec<super::ResolutionRange>>> {
    if source.len() > limit {
        return Ok(None);
    }
    let mut output: Vec<super::ResolutionRange> = Vec::new();
    output
        .try_reserve_exact(source.len())
        .map_err(|_| super::allocation_error("coupled partition"))?;
    let mut previous_touched = false;
    for part in source {
        let index = side.index[&part.block];
        let overlap = accepted.iter().find(|range| {
            range.block_index == index
                && range.start < part.comparable_range.end
                && part.comparable_range.start < range.end
        });
        let mut next = part.clone();
        let touched = if let Some(range) = overlap {
            if part.state == super::ResolutionState::Changed {
                return Ok(None);
            }
            if part.state == super::ResolutionState::Unresolved {
                if range.start > part.comparable_range.start
                    || range.end < part.comparable_range.end
                    || part.canonical_range.start != part.comparable_range.start
                    || part.canonical_range.end != part.comparable_range.end
                {
                    return Ok(None);
                }
                next.state = super::ResolutionState::Equal;
                true
            } else {
                false
            }
        } else {
            false
        };
        if let Some(previous) = output.last_mut().filter(|previous| {
            (touched || previous_touched)
                && previous.block == next.block
                && previous.state == super::ResolutionState::Equal
                && next.state == previous.state
                && previous.comparable_range.end == next.comparable_range.start
                && previous.canonical_range.end == next.canonical_range.start
        }) {
            previous.comparable_range.end = next.comparable_range.end;
            previous.canonical_range.end = next.canonical_range.end;
            previous_touched |= touched;
        } else {
            output.push(next);
            previous_touched = touched;
        }
    }
    Ok(Some(output))
}

/// Copies one text span with a fallible block-list allocation.
fn copy_span(span: &TextSpan) -> Result<TextSpan> {
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(span.blocks.len())
        .map_err(|_| super::allocation_error("equal fragment span blocks"))?;
    blocks.extend_from_slice(&span.blocks);
    Ok(TextSpan {
        blocks,
        separator: span.separator,
        canonical_range: span.canonical_range,
        comparable_range: span.comparable_range,
    })
}

/// Reserves capacity before a commit without changing any length.
fn reserve_capacity<T>(output: &mut Vec<T>, additional: usize) -> Result<()> {
    output
        .try_reserve_exact(additional)
        .map_err(|_| super::allocation_error("closed domain recovery ranges"))
}

pub(super) fn overlaps(
    old: &[SourceInterval],
    new: &[SourceInterval],
    remaining: &mut usize,
) -> Option<bool> {
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

    #[test]
    fn overlap_protection_detects_partial_crossings_and_respects_budget() {
        let interval = |block: usize, start: usize, end: usize| SourceInterval {
            block_index: block,
            start,
            end,
        };
        let accepted = [interval(0, 10, 20)];
        assert_eq!(
            overlaps(&[interval(0, 15, 25)], &accepted, &mut 100),
            Some(true)
        );
        assert_eq!(
            overlaps(&[interval(0, 5, 15)], &accepted, &mut 100),
            Some(true)
        );
        assert_eq!(
            overlaps(&[interval(0, 20, 30)], &accepted, &mut 100),
            Some(false)
        );
        assert_eq!(
            overlaps(&[interval(1, 10, 20)], &accepted, &mut 100),
            Some(false)
        );
        assert_eq!(overlaps(&[interval(0, 15, 25)], &accepted, &mut 0), None);
    }

    use crate::{
        alignment::{
            Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
            BlockSeparator,
        },
        diff::{Change, ChangeKind, ChangedRegionProof, Confidence, DiffOptions},
        diff::{
            TextSpan,
            assessment::{DomainKey, DomainProof, views::LocalDomain},
        },
        layout::{BlockId, BlockRole},
        model::{GlyphId, Vec2},
        normalize::{
            BlockText, FontSizeSignature, MappedText, NormalizationEvent, NormalizationKind,
            PositionSignature, ScalarRange, SourceMapEntry, TextSource, TextSourceAtom,
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

    #[test]
    fn partial_local_recovery_keeps_proof_and_candidate_residue() -> Result<()> {
        let old_blocks = [sourced_block(1, "ABCD")];
        let new_blocks = [sourced_block(101, "ABXD")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = matched_alignment(&[BlockId(1)], &[BlockId(101)]);
        for (candidate_end, candidate_side) in [(3, 0), (4, 0), (3, 1), (4, 1)] {
            let mut assessor = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            assessor.local_domains.push(LocalDomain {
                old_span: span(1, 3),
                new_span: span(101, 3),
                source_bounded: true,
            });
            let mut owners = [Ownership::new(), Ownership::new()];
            let mut changes = Vec::new();
            let mut candidates = vec![super::super::ChangeCandidate {
                change: Change::single_occurrence(
                    if candidate_side == 0 {
                        ChangeKind::Deletion
                    } else {
                        ChangeKind::Insertion
                    },
                    (candidate_side == 0).then(|| span(1, candidate_end)),
                    (candidate_side == 1).then(|| span(101, candidate_end)),
                    Confidence::Low,
                    Vec::new(),
                ),
                relation: 0,
                alternative_group: 0,
            }];
            let origin = assessor.record(super::super::RelationAssessment {
                old_span: candidates[0].change.occurrences[0].old_span.clone(),
                new_span: candidates[0].change.occurrences[0].new_span.clone(),
                parent: None,
                outcome: RelationOutcome::Tentative,
                search: SearchCompleteness::Incomplete,
                assumptions: Vec::new(),
                reasons: vec![super::super::AssessmentReason::DomainNotClosed],
            })?;
            candidates[0].relation = origin;
            candidates[0].alternative_group = origin;
            let before = candidates.clone();
            assessor.recover_local_domain(0, &mut owners, &mut changes, &mut candidates)?;
            assert!(
                !changes.is_empty(),
                "candidate residue must not veto the local proof"
            );
            if candidate_end == 3 {
                assert!(
                    !changes.is_empty(),
                    "complete candidate control must recover"
                );
                assert!(candidates.is_empty());
            } else {
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].relation, before[0].relation);
                assert_eq!(
                    assessor.records[origin].old_span,
                    before[0].change.occurrences[0].old_span
                );
                assert_eq!(
                    assessor.records[origin].new_span,
                    before[0].change.occurrences[0].new_span
                );
                assert!(
                    assessor
                        .localized_edits
                        .iter()
                        .all(|proof| proof.relation != origin)
                );
                assert_eq!(candidates[0].alternative_group, before[0].alternative_group);
                let occurrence = &candidates[0].change.occurrences[0];
                let residue = if candidate_side == 0 {
                    occurrence.old_span.as_ref()
                } else {
                    occurrence.new_span.as_ref()
                }
                .expect("unresolved D retains its source");
                assert_eq!(
                    residue.comparable_range,
                    super::super::super::TokenRange { start: 3, end: 4 }
                );
                assert_eq!(residue.canonical_range, ScalarRange { start: 3, end: 4 });
                for owner in &owners {
                    assert!(owner.accepted.iter().any(|r| r.start == 0 && r.end == 3));
                    assert!(owner.changed.iter().any(|r| r.start == 2 && r.end == 3));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn scoped_evidence_capacity_reuse_preserves_budget_and_every_source_link() -> Result<()> {
        let old_blocks = [sourced_block(1, "ABC")];
        let new_blocks = [sourced_block(101, "ABX")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = matched_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut a =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        a.remaining_work = 250_000;
        let groups = [
            super::super::domain_group(&old, 0..1, BlockSeparator::Concatenate),
            super::super::domain_group(&new, 0..1, BlockSeparator::Concatenate),
        ];
        for _ in 0..315 {
            let relation = a.record(super::super::RelationAssessment {
                old_span: Some(span(1, 3)),
                new_span: Some(span(101, 3)),
                parent: None,
                outcome: RelationOutcome::Established,
                search: SearchCompleteness::Complete,
                assumptions: Vec::new(),
                reasons: Vec::new(),
            })?;
            a.capture_scoped_change_evidence(relation, [&groups[0], &groups[1]])?;
        }
        assert_eq!(
            a.scoped_change_evidence.len(),
            315,
            "every paid source link must survive"
        );
        assert!(
            a.evidence_link_work.retention <= 40_000,
            "remove actual repeated buffer relocation"
        );
        assert!(a.scoped_change_evidence.capacity() <= a.options.max_assessment_ranges);
        assert!(
            a.scoped_change_evidence.capacity()
                * std::mem::size_of::<super::super::ScopedChangeEvidence>()
                <= super::super::COARSE_MEMORY_BYTES
        );
        for (index, link) in a.scoped_change_evidence.iter().enumerate() {
            assert_eq!(link.relation, index);
            assert_eq!(a.records[index].old_span, Some(span(1, 3)));
            assert_eq!(a.records[index].new_span, Some(span(101, 3)));
            assert_eq!(link.proof, ChangedRegionProof::ExactTokenMultisetMismatch);
        }
        let old_tokens = (0..200).collect::<Vec<_>>();
        let mut new_tokens = old_tokens.clone();
        new_tokens[100] = 201;
        assert_eq!(
            super::super::exact::check(&old_tokens, &new_tokens, &mut a.remaining_work)?,
            super::super::exact::ExactUniqueness::Unique,
            "remaining allowance must fund a real subsequent proof"
        );
        Ok(())
    }

    #[test]
    fn explicit_scoped_links_are_paid_non_owning_and_reject_unclosed_scopes() -> Result<()> {
        let old_blocks = [sourced_block(1, "ABC")];
        let new_blocks = [sourced_block(101, "ABX")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = matched_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut a =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let relation = a.record(super::super::RelationAssessment {
            old_span: Some(span(1, 3)),
            new_span: Some(span(101, 3)),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: Vec::new(),
            reasons: Vec::new(),
        })?;
        let groups = [
            super::super::domain_group(&old, 0..1, BlockSeparator::Concatenate),
            super::super::domain_group(&new, 0..1, BlockSeparator::Concatenate),
        ];
        let before = a.remaining_work;
        a.capture_scoped_change_evidence(relation, [&groups[0], &groups[1]])?;
        assert_eq!(a.scoped_change_evidence.len(), 1);
        assert_eq!(a.scoped_change_evidence[0].relation, relation);
        assert_eq!(
            a.scoped_change_evidence[0].proof,
            ChangedRegionProof::ExactTokenMultisetMismatch
        );
        assert_eq!(
            before - a.remaining_work,
            a.evidence_link_work.total().expect("bounded fee")
        );
        assert!(
            a.localized_edits.is_empty(),
            "a difference link localizes no edit"
        );
        a.records[relation].outcome = RelationOutcome::Tentative;
        a.capture_scoped_change_evidence(relation, [&groups[0], &groups[1]])?;
        assert_eq!(
            a.scoped_change_evidence.len(),
            1,
            "unclosed source claims get no positive link"
        );
        Ok(())
    }

    #[test]
    fn candidate_coordinate_frame_preserves_all_source_boundaries() -> Result<()> {
        let mut first = sourced_block(1, "AšB");
        first
            .canonical
            .unmapped
            .push(crate::normalize::UnmappedToken {
                scalar_index: 1,
                font_hash: crate::model::FontProgramHash(vec![37]),
                glyph_id: 9,
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(9001))].into(),
                },
            });
        first.raw = first.canonical.clone();
        first.font_size_signatures = None;
        first.position_signatures = None;
        let blocks = [first, sourced_block(2, ""), sourced_block(3, "中D")];
        let source = side(&blocks);
        let owners = [
            SourceInterval {
                block_index: 0,
                start: 1,
                end: 2,
            },
            SourceInterval {
                block_index: 2,
                start: 1,
                end: 2,
            },
        ];
        for (separator, scalars) in [
            (BlockSeparator::Space, vec![0, 1, 1, 2, 3, 4, 5, 6]),
            (BlockSeparator::Concatenate, vec![0, 1, 1, 2, 3, 4, 5]),
        ] {
            let original = TextSpan {
                blocks: vec![BlockId(1), BlockId(2), BlockId(3)],
                separator: Some(separator),
                canonical_range: ScalarRange {
                    start: 0,
                    end: *scalars.last().expect("boundaries"),
                },
                comparable_range: super::super::super::TokenRange {
                    start: 0,
                    end: scalars.len() - 1,
                },
            };
            let frame =
                CandidateCoordinateFrame::new(&source, &original)?.expect("validated frame");
            for (start, scalar_start) in scalars.iter().enumerate() {
                assert_eq!(frame.scalar(start)?, *scalar_start);
                for (end, scalar_end) in scalars.iter().enumerate().skip(start) {
                    let mut query = original.clone();
                    query.comparable_range = super::super::super::TokenRange { start, end };
                    query.canonical_range = ScalarRange {
                        start: *scalar_start,
                        end: *scalar_end,
                    };
                    let projected = project(&source, &query)?;
                    let overlap = projected.iter().any(|p| {
                        owners.iter().any(|a| {
                            p.block_index == a.block_index && p.start < a.end && a.start < p.end
                        })
                    });
                    assert_eq!(
                        frame.physical_status(start, end, &owners),
                        (!projected.is_empty(), overlap)
                    );
                }
            }
            assert!(frame.scalar(scalars.len()).is_err());
            let mut invalid = original.clone();
            invalid.canonical_range.end += 1;
            assert!(CandidateCoordinateFrame::new(&source, &invalid).is_err());
            invalid = original.clone();
            invalid.blocks[2] = BlockId(1);
            assert!(CandidateCoordinateFrame::new(&source, &invalid).is_err());
            let mut scoped = original.clone();
            scoped.comparable_range.start = 2;
            scoped.canonical_range.start = 1;
            let frame = CandidateCoordinateFrame::new(&source, &scoped)?.expect("scoped frame");
            assert!(frame.scalar(1).is_err());
            assert_eq!(frame.scalar(2)?, 1);
        }
        Ok(())
    }

    #[test]
    fn candidate_residue_shared_frame_keeps_paid_work_for_later_proofs() -> Result<()> {
        // Twenty source blocks and three interior claims reproduce the large
        // repeated frame, without depending on parser or external input files.
        let blocks = (1..=20)
            .map(|id| sourced_block(id, &"A".repeat(70)))
            .collect::<Vec<_>>();
        let source = side(&blocks);
        let original = TextSpan {
            blocks: blocks.iter().map(|b| b.block).collect(),
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange {
                start: 0,
                end: 1419,
            },
            comparable_range: super::super::super::TokenRange {
                start: 0,
                end: 1419,
            },
        };
        let owners = [0, 10, 19].map(|block_index| SourceInterval {
            block_index,
            start: 20,
            end: 30,
        });
        let before = original.clone();
        let mut work = 250_000;
        let residues = candidate_side_residue(
            &source,
            &original,
            physical_candidate_residue(&owners),
            100,
            64 * 1024 * 1024,
            &mut work,
        )?
        .expect("shared frame retains later proof budget");
        let used = 250_000 - work;
        assert!(
            work >= 100_000,
            "frame sharing must remove repeated source walks"
        );
        assert_eq!(original, before);
        assert_eq!(residues.len(), 4);
        let mut retained = Vec::new();
        for residue in &residues {
            assert_eq!(residue.blocks, original.blocks);
            assert_eq!(residue.separator, original.separator);
            assert_eq!(
                residue.comparable_range.start,
                residue.canonical_range.start
            );
            assert_eq!(residue.comparable_range.end, residue.canonical_range.end);
            for interval in project(&source, residue)? {
                for token in interval.start..interval.end {
                    retained.push((interval.block_index, token));
                }
            }
        }
        let expected = (0..20)
            .flat_map(|block| {
                (0..70).filter_map(move |token| {
                    (!([0, 10, 19].contains(&block) && (20..30).contains(&token)))
                        .then_some((block, token))
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(retained, expected);
        assert!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&owners),
                100,
                64 * 1024 * 1024,
                &mut (used - 1)
            )?
            .is_none()
        );
        let mut exact = used;
        assert_eq!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&owners),
                100,
                64 * 1024 * 1024,
                &mut exact
            )?,
            Some(residues)
        );
        assert_eq!(exact, 0);
        let old_tokens = (0..200).collect::<Vec<_>>();
        let mut new_tokens = old_tokens.clone();
        new_tokens[100] = 201;
        assert_eq!(
            super::super::exact::check(&old_tokens, &new_tokens, &mut work)?,
            super::super::exact::ExactUniqueness::Unique,
            "saved shared work must fund an actual complete later proof"
        );
        Ok(())
    }

    #[test]
    fn candidate_residue_tracks_unicode_components_and_paid_refusals() -> Result<()> {
        let blocks = [sourced_block(1, "AšB"), sourced_block(2, "C中D")];
        let source = side(&blocks);
        let original = TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 0, end: 7 },
            comparable_range: super::super::super::TokenRange { start: 0, end: 7 },
        };
        let accepted = [
            SourceInterval {
                block_index: 0,
                start: 1,
                end: 2,
            },
            SourceInterval {
                block_index: 1,
                start: 1,
                end: 2,
            },
        ];
        let mut remaining = 10_000;
        let result = candidate_side_residue(
            &source,
            &original,
            physical_candidate_residue(&accepted),
            100,
            64 * 1024 * 1024,
            &mut remaining,
        )?
        .expect("paid physical residues");
        assert_eq!(
            result
                .iter()
                .map(|s| (s.comparable_range.start, s.comparable_range.end))
                .collect::<Vec<_>>(),
            vec![(0, 1), (2, 5), (6, 7)]
        );
        assert!(
            result
                .iter()
                .all(|s| s.canonical_range.start == s.comparable_range.start
                    && s.canonical_range.end == s.comparable_range.end)
        );
        let physical = result
            .iter()
            .map(|s| project(&source, s))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            physical,
            vec![
                SourceInterval {
                    block_index: 0,
                    start: 0,
                    end: 1
                },
                SourceInterval {
                    block_index: 0,
                    start: 2,
                    end: 3
                },
                SourceInterval {
                    block_index: 1,
                    start: 0,
                    end: 1
                },
                SourceInterval {
                    block_index: 1,
                    start: 2,
                    end: 3
                },
            ]
        );
        let used = 10_000 - remaining;
        for budget in 0..used {
            let mut remaining = budget;
            assert!(
                candidate_side_residue(
                    &source,
                    &original,
                    physical_candidate_residue(&accepted),
                    100,
                    64 * 1024 * 1024,
                    &mut remaining
                )?
                .is_none()
            );
        }
        let mut exact = used;
        assert_eq!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&accepted),
                100,
                64 * 1024 * 1024,
                &mut exact
            )?,
            Some(result)
        );
        assert_eq!(exact, 0);
        assert!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&accepted),
                4,
                64 * 1024 * 1024,
                &mut 10_000
            )?
            .is_none()
        );
        assert!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&accepted),
                100,
                0,
                &mut 10_000
            )?
            .is_none()
        );
        let full = [
            SourceInterval {
                block_index: 0,
                start: 0,
                end: 3,
            },
            SourceInterval {
                block_index: 1,
                start: 0,
                end: 3,
            },
        ];
        assert!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&full),
                100,
                64 * 1024 * 1024,
                &mut 10_000
            )?
            .is_none(),
            "a synthetic separator alone is not a physical content residue"
        );
        Ok(())
    }

    #[test]
    fn candidate_structural_residue_requires_exact_unchanged_gap() -> Result<()> {
        let blocks = [sourced_block(1, "AB"), sourced_block(2, "CD")];
        let source = side(&blocks);
        let original = TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 0, end: 5 },
            comparable_range: super::super::super::TokenRange { start: 0, end: 5 },
        };
        let accepted = [
            SourceInterval {
                block_index: 0,
                start: 0,
                end: 2,
            },
            SourceInterval {
                block_index: 1,
                start: 0,
                end: 2,
            },
        ];
        assert!(
            candidate_side_residue(
                &source,
                &original,
                physical_candidate_residue(&accepted),
                100,
                64 * 1024 * 1024,
                &mut 100_000
            )?
            .is_none()
        );
        let context = CandidateResidueSource {
            accepted: &accepted,
            exact: Some((&original, &[], 0)),
        };
        assert_eq!(
            candidate_side_residue(
                &source,
                &original,
                context,
                100,
                64 * 1024 * 1024,
                &mut 100_000
            )?,
            Some(Vec::new())
        );
        let changed = [super::super::super::myers::AtomicEdit {
            old: 2..3,
            new: 2..2,
        }];
        let context = CandidateResidueSource {
            accepted: &accepted,
            exact: Some((&original, &changed, 0)),
        };
        assert!(
            candidate_side_residue(
                &source,
                &original,
                context,
                100,
                64 * 1024 * 1024,
                &mut 100_000
            )?
            .is_none()
        );
        let mut other_model = original.clone();
        other_model.separator = Some(BlockSeparator::Concatenate);
        other_model.comparable_range.end = 4;
        other_model.canonical_range.end = 4;
        let context = CandidateResidueSource {
            accepted: &accepted,
            exact: Some((&other_model, &[], 0)),
        };
        assert!(
            candidate_side_residue(
                &source,
                &original,
                context,
                100,
                64 * 1024 * 1024,
                &mut 100_000
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn mixed_candidate_residues_keep_local_proof_and_original_empty_anchor() -> Result<()> {
        let old_blocks = [sourced_block(1, "ABCD"), sourced_block(2, "Q")];
        let new_blocks = [sourced_block(101, "ABXD")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = matched_alignment(&[BlockId(1)], &[BlockId(101)]);
        for reverse in [false, true] {
            let mut assessor = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            assessor.local_domains.push(LocalDomain {
                old_span: span(1, 3),
                new_span: span(101, 3),
                source_bounded: true,
            });
            let mut candidates = vec![
                ChangeCandidate {
                    change: Change::single_occurrence(
                        ChangeKind::Deletion,
                        Some(span(1, 4)),
                        None,
                        Confidence::Low,
                        Vec::new(),
                    ),
                    relation: 0,
                    alternative_group: 0,
                },
                ChangeCandidate {
                    change: Change::single_occurrence(
                        ChangeKind::Replacement,
                        Some(span(2, 0)),
                        Some(span(101, 4)),
                        Confidence::Low,
                        Vec::new(),
                    ),
                    relation: 0,
                    alternative_group: 0,
                },
            ];
            for candidate in &mut candidates {
                let occurrence = &candidate.change.occurrences[0];
                let origin = assessor.record(super::super::RelationAssessment {
                    old_span: occurrence.old_span.clone(),
                    new_span: occurrence.new_span.clone(),
                    parent: None,
                    outcome: RelationOutcome::Tentative,
                    search: SearchCompleteness::Incomplete,
                    assumptions: Vec::new(),
                    reasons: vec![super::super::AssessmentReason::DomainNotClosed],
                })?;
                candidate.relation = origin;
                candidate.alternative_group = origin;
            }
            if reverse {
                candidates.reverse();
            }
            let originals = candidates.clone();
            let mut ownership = [Ownership::new(), Ownership::new()];
            let mut changes = Vec::new();
            assessor.recover_local_domain(0, &mut ownership, &mut changes, &mut candidates)?;
            assert!(
                !changes.is_empty(),
                "a mixed candidate must not veto the established C/X proof"
            );
            assert_eq!(candidates.len(), 2);
            for (candidate, original) in candidates.iter().zip(&originals) {
                assert_eq!(candidate.relation, original.relation);
                assert_eq!(candidate.alternative_group, original.alternative_group);
                let origin = &assessor.records[candidate.relation];
                assert_eq!(origin.old_span, original.change.occurrences[0].old_span);
                assert_eq!(origin.new_span, original.change.occurrences[0].new_span);
                let occurrence = &candidate.change.occurrences[0];
                if candidate.change.kind == ChangeKind::Deletion {
                    assert_eq!(
                        occurrence
                            .old_span
                            .as_ref()
                            .expect("deletion residue keeps its source")
                            .comparable_range,
                        super::super::super::TokenRange { start: 3, end: 4 }
                    );
                } else {
                    assert_eq!(
                        occurrence.old_span,
                        Some(span(2, 0)),
                        "keep the original external anchor, never move it into the proof"
                    );
                    assert_eq!(
                        occurrence
                            .new_span
                            .as_ref()
                            .expect("replacement residue keeps its new source")
                            .comparable_range,
                        super::super::super::TokenRange { start: 3, end: 4 }
                    );
                }
            }
            assert!(ownership.iter().all(|side| {
                side.accepted.iter().any(|r| r.start == 0 && r.end == 3)
                    && side.changed.iter().any(|r| r.start == 2 && r.end == 3)
            }));
        }
        Ok(())
    }

    #[test]
    fn paired_candidate_residue_never_selects_disconnected_correspondences() -> Result<()> {
        let old_blocks = [sourced_block(1, "ABCD")];
        let new_blocks = [sourced_block(101, "ABXD")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let occurrence = super::super::super::ChangeOccurrence {
            old_span: Some(span(1, 4)),
            new_span: Some(span(101, 4)),
        };
        let accepted = [SourceInterval {
            block_index: 0,
            start: 0,
            end: 3,
        }];
        let contexts = [
            physical_candidate_residue(&accepted),
            physical_candidate_residue(&accepted),
        ];
        let mut remaining = 100_000;
        let residues = candidate_occurrence_residue(
            [&old, &new],
            &occurrence,
            contexts,
            100,
            64 * 1024 * 1024,
            &mut remaining,
        )?
        .expect("one original residual association");
        assert_eq!(residues.len(), 1);
        for side in [residues[0].old_span.as_ref(), residues[0].new_span.as_ref()] {
            assert_eq!(
                side.expect("paired residue keeps both source scopes")
                    .comparable_range,
                super::super::super::TokenRange { start: 3, end: 4 }
            );
        }
        let used = 100_000 - remaining;
        let mut short = used - 1;
        assert!(
            candidate_occurrence_residue(
                [&old, &new],
                &occurrence,
                contexts,
                100,
                64 * 1024 * 1024,
                &mut short
            )?
            .is_none()
        );
        let mut exact = used;
        assert_eq!(
            candidate_occurrence_residue(
                [&old, &new],
                &occurrence,
                contexts,
                100,
                64 * 1024 * 1024,
                &mut exact
            )?,
            Some(residues)
        );
        assert_eq!(exact, 0);
        let middle = [SourceInterval {
            block_index: 0,
            start: 1,
            end: 2,
        }];
        let contexts = [
            physical_candidate_residue(&middle),
            physical_candidate_residue(&middle),
        ];
        assert!(
            candidate_occurrence_residue(
                [&old, &new],
                &occurrence,
                contexts,
                100,
                64 * 1024 * 1024,
                &mut 100_000
            )?
            .is_none(),
            "matching counts do not prove a fragment pairing"
        );
        assert!(
            candidate_occurrence_residue(
                [&old, &new],
                &occurrence,
                contexts,
                100,
                0,
                &mut 100_000
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn candidate_residue_preserves_unmapped_comparable_width() -> Result<()> {
        let mut block = sourced_block(1, "AB");
        block
            .canonical
            .unmapped
            .push(crate::normalize::UnmappedToken {
                scalar_index: 1,
                font_hash: crate::model::FontProgramHash(vec![37]),
                glyph_id: 9,
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(9001))].into(),
                },
            });
        block.raw = block.canonical.clone();
        block.font_size_signatures = None;
        block.position_signatures = None;
        let blocks = [block];
        let source = side(&blocks);
        let original = TextSpan {
            blocks: vec![BlockId(1)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: super::super::super::TokenRange { start: 0, end: 3 },
        };
        let accepted = [SourceInterval {
            block_index: 0,
            start: 0,
            end: 1,
        }];
        let residues = candidate_side_residue(
            &source,
            &original,
            physical_candidate_residue(&accepted),
            100,
            64 * 1024 * 1024,
            &mut 10_000,
        )?
        .expect("unmapped residual is retained");
        assert_eq!(residues.len(), 1);
        assert_eq!(
            residues[0].comparable_range,
            super::super::super::TokenRange { start: 1, end: 3 }
        );
        assert_eq!(
            residues[0].canonical_range,
            ScalarRange { start: 1, end: 2 }
        );
        assert_eq!(
            project(&source, &residues[0])?,
            vec![SourceInterval {
                block_index: 0,
                start: 1,
                end: 3
            }]
        );
        Ok(())
    }

    #[test]
    fn established_collection_leaves_budget_after_unrelated_owner_queries() -> Result<()> {
        let old_blocks = (1_u64..=128)
            .map(|id| sourced_block(id, &format!("A{id}")))
            .collect::<Vec<_>>();
        let new_blocks = (1_u64..=128)
            .map(|id| sourced_block(id + 1_000, &format!("A{id}")))
            .collect::<Vec<_>>();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_ids = old_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let new_ids = new_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let alignment = unresolved_alignment(&old_ids, &new_ids);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let mut ownership = [Ownership::new(), Ownership::new()];
        for index in 0..128 {
            let count = old.canonical[index].len();
            let old_span = span(old_blocks[index].block.0, count);
            let new_span = span(new_blocks[index].block.0, count);
            ownership[0].accept(&old, &old_span, 4_096)?;
            ownership[1].accept(&new, &new_span, 4_096)?;
            if index < 16 {
                assessor.local_anchors.push(LocalDomain {
                    old_span: old_span.clone(),
                    new_span: new_span.clone(),
                    source_bounded: true,
                });
                let proposal = ProposedRelation {
                    old: Some(old_span),
                    new: Some(new_span),
                    span_indices: [None, None],
                    exact_recovery: true,
                };
                let key = assessor.domain_key(&proposal)?;
                assessor.prove_domain(&key)?;
            }
        }
        assessor.remaining_work = 100_000;
        let expected = assessor
            .collect_established_blocks(&ownership)?
            .expect("complete control collection");
        assert_eq!(expected.len(), 16);
        let expected = expected
            .iter()
            .map(|entry| (entry.old_block, entry.new_block))
            .collect::<Vec<_>>();
        assessor.remaining_work = 3_400;
        let bounded = assessor.collect_established_blocks(&ownership)?;
        let bounded = bounded.map(|entries| {
            entries
                .iter()
                .map(|entry| (entry.old_block, entry.new_block))
                .collect::<Vec<_>>()
        });
        assert_eq!(bounded, Some(expected));
        assert!(
            assessor.remaining_work >= 300,
            "later source proofs lost their budget"
        );
        Ok(())
    }

    #[test]
    fn ownership_buckets_preserve_original_owners_and_strict_endpoints() -> Result<()> {
        let blocks = (1_u64..=64)
            .map(|id| sourced_block(id, "abcd"))
            .collect::<Vec<_>>();
        let source = side(&blocks);
        let ids = blocks.iter().map(|block| block.block).collect::<Vec<_>>();
        let alignment = unresolved_alignment(&ids, &ids);
        let mut assessor = super::super::Assessor::new(
            [&source, &source],
            &alignment,
            None,
            DiffOptions::default(),
        )?;
        let interval = |block_index, start, end| SourceInterval {
            block_index,
            start,
            end,
        };
        let mut ownership = [Ownership::new(), Ownership::new()];
        ownership[0].accepted = vec![interval(0, 0, 2), interval(0, 2, 4), interval(0, 0, 2)];
        ownership[0]
            .accepted
            .extend((1..64).map(|index| interval(index, 0, 4)));
        let mut budget = 100_000;
        let buckets = OwnershipBuckets::build(&ownership[0].accepted, 64, 128, &mut budget)
            .expect("repeated ownership queries justify grouping");
        let range = buckets
            .range(0, &mut budget)
            .expect("complete source group");
        assert_eq!(range.len(), 3);
        for (original, grouped) in ownership[0].accepted[..3]
            .iter()
            .zip(&buckets.intervals[range])
        {
            assert!(std::ptr::eq(original, *grouped));
        }
        for start in 0..=4 {
            for end in start..=4 {
                let mut query = span(1, end);
                query.comparable_range.start = start;
                query.canonical_range.start = start;
                assessor.remaining_work = 100_000;
                let expected = assessor.ownership_contains(&ownership, 0, &query)?;
                assessor.remaining_work = 100_000;
                assert_eq!(
                    assessor.ownership_contains_bucket(&ownership, 0, &query, Some(&buckets))?,
                    expected
                );
            }
        }
        assert_eq!(
            assessor.ownership_contains_bucket(&ownership, 0, &span(1, 4), Some(&buckets))?,
            Some(false)
        );
        for query in [
            interval(0, 4, 5),
            interval(0, 1, 3),
            interval(0, 2, 2),
            interval(63, 4, 5),
        ] {
            assessor.remaining_work = 100_000;
            let expected =
                assessor.ownership_intersects_buckets(&ownership[0], &[query], None, None);
            assessor.remaining_work = 100_000;
            assert_eq!(
                assessor.ownership_intersects_buckets(
                    &ownership[0],
                    &[query],
                    Some(&buckets),
                    None
                ),
                expected
            );
        }
        assessor.remaining_work = 2;
        assert_eq!(
            assessor.ownership_contains_bucket(&ownership, 0, &span(1, 2), Some(&buckets))?,
            None
        );
        Ok(())
    }

    #[test]
    fn ownership_buckets_refuse_unfunded_and_out_of_population_inputs() {
        let valid = vec![
            SourceInterval {
                block_index: 0,
                start: 0,
                end: 1
            };
            64
        ];
        let mut budget = 0;
        assert!(OwnershipBuckets::build(&valid, 64, 128, &mut budget).is_none());
        assert_eq!(budget, 0);
        budget = 100_000;
        assert!(OwnershipBuckets::build(&valid, 64, 1, &mut budget).is_none());
        assert_eq!(budget, 100_000);
        assert!(OwnershipBuckets::build(&valid, usize::MAX, usize::MAX, &mut budget).is_none());
        assert_eq!(budget, 100_000);
        let mut invalid = valid;
        invalid[63].block_index = 64;
        assert!(OwnershipBuckets::build(&invalid, 64, 128, &mut budget).is_none());
        assert_eq!(budget, 100_000 - 385, "failed setup work is retained");
    }

    #[test]
    fn stationary_ownership_mask_preserves_every_source_block_on_both_sides() -> Result<()> {
        let mut blocks = (1_u64..=64)
            .map(|id| sourced_block(id, "abcd"))
            .collect::<Vec<_>>();
        blocks.push(sourced_block(65, ""));
        let source = side(&blocks);
        let ids = blocks.iter().map(|block| block.block).collect::<Vec<_>>();
        let alignment = unresolved_alignment(&ids, &ids);
        let mut assessor = super::super::Assessor::new(
            [&source, &source],
            &alignment,
            None,
            DiffOptions::default(),
        )?;
        let mut ownership = [Ownership::new(), Ownership::new()];
        for (side, owner) in ownership.iter_mut().enumerate() {
            for index in 0..64 {
                let interval = SourceInterval {
                    block_index: index,
                    start: if index % 4 == side { 2 } else { 4 },
                    end: if index % 4 == side { 3 } else { 4 },
                };
                if index % 2 == side {
                    owner.accepted.push(interval);
                    owner.accepted.push(interval);
                } else {
                    owner.changed.push(interval);
                }
            }
        }
        let expected: [Vec<bool>; 2] = std::array::from_fn(|side| {
            (0..65)
                .map(|index| {
                    index < 64
                        && !ownership[side]
                            .accepted
                            .iter()
                            .chain(&ownership[side].changed)
                            .any(|owner| {
                                owner.block_index == index && owner.start < 4 && 0 < owner.end
                            })
                })
                .collect()
        });
        assessor.remaining_work = 100_000;
        assert_eq!(
            assessor.stationary_candidate_mask(&ownership)?,
            Some(expected)
        );
        Ok(())
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

    /// An alignment with one span per block pair where the last pair carries a
    /// normalization issue, so a proof's scope is observable.
    fn scoped_alignment(old: &[BlockId], new: &[BlockId]) -> Alignment {
        let span = |old_block: BlockId, new_block: BlockId, issue: bool| AlignmentSpan {
            kind: AlignmentKind::Unresolved,
            old: vec![old_block],
            new: vec![new_block],
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: if issue {
                vec![
                    AlignmentEvidence::ReadingOrderUnknown,
                    AlignmentEvidence::NormalizationIssue,
                ]
            } else {
                vec![AlignmentEvidence::ReadingOrderUnknown]
            },
            old_separator: Some(BlockSeparator::Space),
            new_separator: Some(BlockSeparator::Space),
        };
        let spans = old
            .iter()
            .zip(new)
            .enumerate()
            .map(|(index, (old_block, new_block))| {
                span(*old_block, *new_block, index == 1 || index + 1 == old.len())
            })
            .collect();
        Alignment {
            spans,
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    /// An alignment with one span per block pair and an extraction gap on the
    /// middle pair only, so the outer anchors can still be proven.
    fn gapped_alignment(old: &[BlockId], new: &[BlockId]) -> Alignment {
        let span = |old_block: BlockId, new_block: BlockId, gap: bool| AlignmentSpan {
            kind: AlignmentKind::Unresolved,
            old: vec![old_block],
            new: vec![new_block],
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: if gap {
                vec![
                    AlignmentEvidence::ReadingOrderUnknown,
                    AlignmentEvidence::ExtractionGap,
                ]
            } else {
                vec![AlignmentEvidence::ReadingOrderUnknown]
            },
            old_separator: Some(BlockSeparator::Space),
            new_separator: Some(BlockSeparator::Space),
        };
        Alignment {
            spans: vec![
                span(old[0], new[0], false),
                span(old[1], new[1], true),
                span(old[2], new[2], false),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    fn positioned_block(id: u64, text: &str, x: f64, y: f64) -> BlockText {
        let mut block = sourced_block(id, text);
        let tokens = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        let position =
            PositionSignature::new(Vec2 { x, y }, Vec2 { x: 1.0, y: 0.0 }).expect("valid position");
        block.position_signatures = Some(vec![position; tokens]);
        block
    }

    fn anchored_fixture(paragraph_y: f64) -> (Vec<BlockText>, Vec<BlockText>) {
        let old_blocks = vec![
            positioned_block(1, "Support anchor line", 300.0, 300.0),
            positioned_block(2, "Moved target line", 300.0, 290.0),
            {
                let mut paragraph =
                    positioned_block(3, "Paragraph reference line", 300.0, paragraph_y);
                paragraph.line_breaks = Some(vec![1]);
                paragraph
            },
        ];
        let new_blocks = vec![
            positioned_block(101, "Support anchor line", 300.0, 100.0),
            positioned_block(102, "Moved target line", 300.0, 90.0),
            positioned_block(103, "Paragraph reference line", 300.0, paragraph_y),
        ];
        (old_blocks, new_blocks)
    }

    fn run_anchored_pass(old_blocks: &[BlockText], new_blocks: &[BlockText]) -> Result<usize> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let alignment = unresolved_alignment(
            &old_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            &new_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
        );
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let recovery = crate::diff::SentenceRecoveryInput {
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_intervals: &old_intervals,
            new_trusted_run_intervals: &new_intervals,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let mut assessor = super::super::Assessor::new(
            [&old, &new],
            &alignment,
            Some(recovery),
            DiffOptions::default(),
        )?;
        let support_tokens = old_blocks[0]
            .canonical
            .comparable_tokens()
            .expect("support tokens")
            .len();
        let reference_tokens = old_blocks[2]
            .canonical
            .comparable_tokens()
            .expect("reference tokens")
            .len();
        assessor.local_domains = vec![
            LocalDomain {
                old_span: span(1, support_tokens),
                new_span: span(101, support_tokens),
                source_bounded: true,
            },
            LocalDomain {
                old_span: span(3, reference_tokens),
                new_span: span(103, reference_tokens),
                source_bounded: true,
            },
        ];
        for (old_block, new_block, tokens) in
            [(1_u64, 101_u64, support_tokens), (3, 103, reference_tokens)]
        {
            let proposal = super::super::ProposedRelation {
                old: Some(span(old_block, tokens)),
                new: Some(span(new_block, tokens)),
                span_indices: [None, None],
                exact_recovery: false,
            };
            let key = assessor.domain_key(&proposal)?;
            assessor.prove_domain(&key)?;
        }
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        ownership[0].accept(&old, &span(1, support_tokens), 64)?;
        ownership[1].accept(&new, &span(101, support_tokens), 64)?;
        ownership[0].accept(&old, &span(3, reference_tokens), 64)?;
        ownership[1].accept(&new, &span(103, reference_tokens), 64)?;
        assessor.discover_anchored_translations(&ownership)?;
        Ok(assessor.local_domains.len())
    }

    #[test]
    fn anchored_pass_keeps_a_soft_line_break_reference() -> Result<()> {
        // The paragraph reference is not a complete single-line view (soft
        // line break), so the view metadata carries no positions. The caller
        // must still pass it as a reference and the pass must hold the
        // crossing candidate instead of dropping the reference.
        let (old_blocks, new_blocks) = anchored_fixture(200.0);
        let domains = run_anchored_pass(&old_blocks, &new_blocks)?;
        assert_eq!(domains, 2, "the crossing reference must hold the candidate");
        Ok(())
    }

    #[test]
    fn anchored_pass_closes_when_the_reference_is_not_crossed() -> Result<()> {
        let (old_blocks, new_blocks) = anchored_fixture(50.0);
        let domains = run_anchored_pass(&old_blocks, &new_blocks)?;
        assert_eq!(
            domains, 3,
            "an uncrossed reference must not hold the candidate"
        );
        Ok(())
    }

    /// A source-backed block whose tokens advance along x, so its baseline
    /// geometry is an interval and a positive-width column band exists.
    fn spread_block(id: u64, text: &str, x: f64, y: f64, advance: f64) -> BlockText {
        let mut block = sourced_block(id, text);
        let tokens = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        let signatures = (0..tokens)
            .map(|index| {
                PositionSignature::new(
                    Vec2 {
                        x: x + index as f64 * advance,
                        y,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("valid position")
            })
            .collect::<Vec<_>>();
        block.position_signatures = Some(signatures);
        block
    }

    /// Observable state of one bracketed recovery run through the real
    /// `recover_local` caller.
    struct BracketedRecovery {
        truncated: bool,
        domains: usize,
        assumptions: usize,
        accepted: [usize; 2],
        changes: usize,
    }

    /// Runs `recover_local` over a bracketed fixture: two established
    /// boundaries (blocks 1/101 above and 3/103 below) and, optionally, one
    /// pre-existing local domain.
    fn run_bracketed_recovery(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        preexisting: &[LocalDomain],
        options: DiffOptions,
        extra_accepts: usize,
    ) -> Result<BracketedRecovery> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let alignment = unresolved_alignment(
            &old_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            &new_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
        );
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let recovery = crate::diff::SentenceRecoveryInput {
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_intervals: &old_intervals,
            new_trusted_run_intervals: &new_intervals,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, Some(recovery), options)?;
        let upper_tokens = old_blocks[0]
            .canonical
            .comparable_tokens()
            .expect("upper tokens")
            .len();
        let lower_tokens = old_blocks[2]
            .canonical
            .comparable_tokens()
            .expect("lower tokens")
            .len();
        // The two boundaries are discovered local anchors: their relation keys
        // carry the local domain, so the reading-order barrier is removed and
        // the proofs are established exactly as in the real caller.
        assessor.local_anchors = vec![
            LocalDomain {
                old_span: span(1, upper_tokens),
                new_span: span(101, upper_tokens),
                source_bounded: true,
            },
            LocalDomain {
                old_span: span(3, lower_tokens),
                new_span: span(103, lower_tokens),
                source_bounded: true,
            },
        ];
        for (old_block, new_block, tokens) in [
            (1_u64, 101_u64, upper_tokens),
            (3_u64, 103_u64, lower_tokens),
        ] {
            let proposal = super::super::ProposedRelation {
                old: Some(span(old_block, tokens)),
                new: Some(span(new_block, tokens)),
                span_indices: [None, None],
                exact_recovery: true,
            };
            let key = assessor.domain_key(&proposal)?;
            assessor.prove_domain(&key)?;
        }
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        ownership[0].accept(&old, &span(1, upper_tokens), 64)?;
        ownership[1].accept(&new, &span(101, upper_tokens), 64)?;
        ownership[0].accept(&old, &span(3, lower_tokens), 64)?;
        ownership[1].accept(&new, &span(103, lower_tokens), 64)?;
        for _ in 0..extra_accepts {
            ownership[0].accept(&old, &span(1, upper_tokens), 64)?;
            ownership[1].accept(&new, &span(101, upper_tokens), 64)?;
        }
        assessor.local_domains = preexisting.to_vec();
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        let truncated = assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        Ok(BracketedRecovery {
            truncated,
            domains: assessor.local_domains.len(),
            assumptions: assessor.bracketed_domains.len(),
            accepted: [ownership[0].accepted.len(), ownership[1].accepted.len()],
            changes: changes.len(),
        })
    }

    struct StationaryRecovery {
        truncated: bool,
        work_used: usize,
        stationary: usize,
        accepted: [usize; 2],
        records_stationary: usize,
        empty_projection: Option<bool>,
    }

    fn stationary_recovery_fixture() -> (Vec<BlockText>, Vec<BlockText>) {
        let old_blocks = vec![
            spread_block(1, "Upper boundary line", 300.0, 700.0, 5.0),
            spread_block(2, "Filing year statement", 300.0, 680.0, 5.0),
            spread_block(3, "Lower boundary line", 300.0, 660.0, 5.0),
        ];
        let new_blocks = vec![
            spread_block(101, "Upper boundary line", 300.0, 700.0, 5.0),
            spread_block(102, "Filing year statement", 300.0, 680.0, 5.0),
            spread_block(103, "Lower boundary line", 300.0, 660.0, 5.0),
        ];
        (old_blocks, new_blocks)
    }
    #[derive(Default, Clone, Copy)]
    struct StationaryControls {
        partial_accept: Option<(usize, usize)>,
        full_accept: bool,
        changed: Option<(usize, usize, usize)>,
        probe_empty: bool,
        budget: Option<usize>,
        repeat: bool,
    }

    fn run_stationary_recovery(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        controls: StationaryControls,
        options: DiffOptions,
    ) -> Result<StationaryRecovery> {
        use crate::layout::{TrustedRunId, TrustedRunInterval};
        let old = side(old_blocks);
        let new = side(new_blocks);
        let alignment = unresolved_alignment(
            &old_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            &new_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
        );
        let interval = |run: u64, start: usize, end: usize| {
            Some(TrustedRunInterval {
                run_id: TrustedRunId(run),
                start,
                end,
            })
        };
        let old_intervals = vec![interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = vec![interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let recovery = crate::diff::SentenceRecoveryInput {
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_intervals: &old_intervals,
            new_trusted_run_intervals: &new_intervals,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, Some(recovery), options)?;
        if let Some(budget) = controls.budget {
            assessor.remaining_work = budget;
        }
        let tokens = |blocks: &[BlockText], index: usize| {
            blocks[index]
                .canonical
                .comparable_tokens()
                .expect("fixture tokens")
                .len()
        };
        let upper = tokens(old_blocks, 0);
        let lower = tokens(old_blocks, 2);
        assessor.local_anchors = vec![
            LocalDomain {
                old_span: span(1, upper),
                new_span: span(101, upper),
                source_bounded: true,
            },
            LocalDomain {
                old_span: span(3, lower),
                new_span: span(103, lower),
                source_bounded: true,
            },
        ];
        for (old_block, new_block, count) in [(1_u64, 101_u64, upper), (3_u64, 103_u64, lower)] {
            let proposal = super::super::ProposedRelation {
                old: Some(span(old_block, count)),
                new: Some(span(new_block, count)),
                span_indices: [None, None],
                exact_recovery: true,
            };
            let key = assessor.domain_key(&proposal)?;
            assessor.prove_domain(&key)?;
        }
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        ownership[0].accept(&old, &span(1, upper), 64)?;
        ownership[1].accept(&new, &span(101, upper), 64)?;
        ownership[0].accept(&old, &span(3, lower), 64)?;
        ownership[1].accept(&new, &span(103, lower), 64)?;
        if controls.full_accept {
            ownership[0].accept(&old, &span(2, tokens(old_blocks, 1)), 64)?;
            ownership[1].accept(&new, &span(102, tokens(new_blocks, 1)), 64)?;
        }
        if let Some((side, count)) = controls.partial_accept {
            let block = if side == 0 { BlockId(2) } else { BlockId(102) };
            let source = if side == 0 { &old } else { &new };
            ownership[side].accept(source, &span(block.0, count), 64)?;
        }
        if let Some((side, block_index, count)) = controls.changed {
            ownership[side].changed.push(super::SourceInterval {
                block_index,
                start: 0,
                end: count,
            });
        }
        let empty_projection = if controls.probe_empty {
            let mut empty = span(1, 0);
            empty.blocks.clear();
            assessor.stationary_candidate_is_clear(
                &ownership,
                &LocalDomain {
                    old_span: empty.clone(),
                    new_span: empty,
                    source_bounded: true,
                },
            )?
        } else {
            None
        };
        let work_before = assessor.remaining_work;
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        let mut truncated =
            assessor.recover_stationary_members(&mut ownership, &mut changes, &mut candidates)?;
        if controls.repeat {
            truncated |= assessor.recover_stationary_members(
                &mut ownership,
                &mut changes,
                &mut candidates,
            )?;
        }
        let records_stationary = assessor
            .records
            .iter()
            .filter(|record| {
                record
                    .assumptions
                    .contains(&crate::diff::ComparisonAssumption::StationaryNeighbour)
                    && record.outcome == super::RelationOutcome::Established
                    && record.search == super::SearchCompleteness::Complete
            })
            .count();
        Ok(StationaryRecovery {
            truncated,
            work_used: work_before.saturating_sub(assessor.remaining_work),
            stationary: assessor.stationary_members.len(),
            accepted: [ownership[0].accepted.len(), ownership[1].accepted.len()],
            records_stationary,
            empty_projection,
        })
    }

    /// A whole block whose raw projection is self-isomorphic but whose
    /// normalization carries one ambiguous line break issue.
    fn raw_issue_block(id: u64, x: f64, y: f64) -> BlockText {
        use crate::normalize::{
            MappedText, NormalizationEvent, NormalizationIssue, NormalizationIssueKind,
            NormalizationKind, PositionSignature, ScalarRange, SourceMapEntry, TextSource,
            TextSourceAtom,
        };
        let first = GlyphId(id * 1000 + 1);
        let second = GlyphId(id * 1000 + 2);
        let third = GlyphId(id * 1000 + 3);
        let glyph = |glyph: GlyphId| TextSourceAtom::Glyph(glyph);
        let line_break = |preceding: GlyphId, following: GlyphId| TextSourceAtom::LineBreak {
            preceding,
            following,
        };
        let entry = |index: usize, atom: TextSourceAtom| SourceMapEntry {
            output_range: ScalarRange {
                start: index,
                end: index + 1,
            },
            source: TextSource {
                atoms: vec![atom].into(),
            },
        };
        let raw = MappedText {
            text: "A\nB\nC".to_owned(),
            source_map: vec![
                entry(0, glyph(first)),
                entry(1, line_break(first, second)),
                entry(2, glyph(second)),
                entry(3, line_break(second, third)),
                entry(4, glyph(third)),
            ],
            unmapped: Vec::new(),
        };
        let canonical = MappedText {
            text: "AB\nC".to_owned(),
            source_map: vec![
                entry(0, glyph(first)),
                entry(1, glyph(second)),
                entry(2, line_break(second, third)),
                entry(3, glyph(third)),
            ],
            unmapped: Vec::new(),
        };
        let tokens = canonical.comparable_tokens().expect("raw issue tokens");
        let position = |index: usize| {
            PositionSignature::new(
                crate::model::Vec2 {
                    x: x + index as f64 * 10.0,
                    y,
                },
                crate::model::Vec2 { x: 1.0, y: 0.0 },
            )
            .expect("valid position")
        };
        let font_size = crate::normalize::FontSizeSignature::new(&[10.0]).expect("valid font size");
        BlockText {
            block: crate::layout::BlockId(id),
            role: crate::layout::BlockRole::Body,
            raw,
            canonical,
            matching: "AB\nC".to_owned(),
            matching_tokens: tokens.clone(),
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: vec![line_break(first, second)].into(),
                },
            }],
            issues: vec![NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: ScalarRange { start: 3, end: 4 },
                source: TextSource {
                    atoms: vec![line_break(second, third)].into(),
                },
            }],
            pages: vec![0],
            font_size_signatures: Some(vec![font_size; tokens.len()]),
            position_signatures: Some(vec![position(0), position(1), position(1), position(2)]),
            line_breaks: Some(vec![2]),
            page_breaks: Some(Vec::new()),
        }
    }

    fn positioned_replacement_fixture() -> (Vec<BlockText>, Vec<BlockText>) {
        (
            vec![
                positioned_block(1, "Anchor line", 300.0, 300.0),
                positioned_block(2, "ABCDE", 300.0, 290.0),
            ],
            vec![
                positioned_block(101, "Anchor line", 300.0, 300.0),
                positioned_block(102, "ABXDE", 300.0, 290.0),
            ],
        )
    }

    struct PositionedRecovery {
        replacements: Vec<(TextSpan, TextSpan)>,
        changes: Vec<ChangeEvent>,
        candidates: Vec<ChangeCandidate>,
        changed: [Vec<SourceInterval>; 2],
        edits: Vec<crate::diff::AtomicEdit>,
        accepted: [usize; 2],
        truncated: bool,
        assumed: bool,
        established: bool,
    }

    fn run_positioned_replacement(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        establish_anchor: bool,
        partial_ownership: Option<(usize, usize)>,
        budget: Option<usize>,
        repeat: bool,
    ) -> Result<PositionedRecovery> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let old_ids = old_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let new_ids = new_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let alignment = unresolved_alignment(&old_ids, &new_ids);
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let recovery = crate::diff::SentenceRecoveryInput {
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_intervals: &old_intervals,
            new_trusted_run_intervals: &new_intervals,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let mut assessor = super::super::Assessor::new(
            [&old, &new],
            &alignment,
            Some(recovery),
            DiffOptions::default(),
        )?;
        let anchor_len = old_blocks[0]
            .canonical
            .comparable_tokens()
            .expect("anchor tokens")
            .len();
        if establish_anchor {
            assessor.local_anchors = vec![LocalDomain {
                old_span: span(1, anchor_len),
                new_span: span(101, anchor_len),
                source_bounded: true,
            }];
            let proposal = super::super::ProposedRelation {
                old: Some(span(1, anchor_len)),
                new: Some(span(101, anchor_len)),
                span_indices: [None, None],
                exact_recovery: true,
            };
            let key = assessor.domain_key(&proposal)?;
            assessor.prove_domain(&key)?;
        }
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        if establish_anchor {
            ownership[0].accept(&old, &span(1, anchor_len), 64)?;
            ownership[1].accept(&new, &span(101, anchor_len), 64)?;
        }
        if let Some((side_index, count)) = partial_ownership {
            let block = if side_index == 0 {
                BlockId(2)
            } else {
                BlockId(102)
            };
            let source = if side_index == 0 { &old } else { &new };
            ownership[side_index].accept(source, &span(block.0, count), 64)?;
        }
        if let Some(budget) = budget {
            assessor.remaining_work = budget;
        }
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        let mut truncated = assessor.recover_positioned_replacements(
            &mut ownership,
            &mut changes,
            &mut candidates,
        )?;
        if repeat {
            // The second pass must see the queued domain as already recovered
            // and add neither a duplicate change nor a duplicate assumption.
            truncated |= assessor.recover_positioned_replacements(
                &mut ownership,
                &mut changes,
                &mut candidates,
            )?;
        }
        let edits = assessor
            .localized_edits
            .iter()
            .flat_map(|script| script.edits.iter().cloned())
            .collect::<Vec<_>>();
        let assumed = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&crate::diff::ComparisonAssumption::PositionedReplacement)
        });
        let established = assessor.records.iter().any(|record| {
            record.outcome == super::RelationOutcome::Established
                && record
                    .assumptions
                    .contains(&crate::diff::ComparisonAssumption::PositionedReplacement)
        });
        Ok(PositionedRecovery {
            replacements: assessor.positioned_replacements.clone(),
            changes,
            candidates,
            changed: [ownership[0].changed.clone(), ownership[1].changed.clone()],
            edits,
            accepted: [ownership[0].accepted.len(), ownership[1].accepted.len()],
            truncated,
            assumed,
            established,
        })
    }

    fn token_text(tokens: &[crate::normalize::ComparableToken]) -> String {
        tokens
            .iter()
            .map(|token| match token {
                crate::normalize::ComparableToken::Scalar(character) => *character,
                crate::normalize::ComparableToken::Unmapped { .. } => '?',
            })
            .collect()
    }

    #[test]
    fn positioned_replacement_recovery_publishes_one_exact_replacement() -> Result<()> {
        let (old_blocks, new_blocks) = positioned_replacement_fixture();
        let recovery =
            run_positioned_replacement(&old_blocks, &new_blocks, true, None, None, true)?;
        assert_eq!(
            recovery.replacements,
            vec![(span(2, 5), span(102, 5))],
            "the whole changed member must close exactly once"
        );
        assert!(
            recovery.established,
            "the replacement domain must establish a relation"
        );
        assert!(
            recovery.assumed,
            "the relation must carry the positioned-replacement assumption"
        );
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "both sides must own the anchor and the replacement"
        );
        assert!(!recovery.truncated);

        // The emitted output, not the fixture arrays, must carry exactly one
        // replacement change with no competing candidate left behind.
        assert_eq!(
            recovery.changes.len(),
            1,
            "exactly one change event: {:?}",
            recovery.changes
        );
        assert_eq!(
            recovery.changes[0].kind,
            crate::diff::ChangeKind::Replacement
        );
        assert!(
            recovery.candidates.is_empty(),
            "the recovery must not leave a competing candidate: {:?}",
            recovery.candidates
        );

        // The emitted occurrence ranges project to the changed source
        // intervals on both sides, and those intervals are the changed
        // ownership this recovery records.
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let mut old_intervals = Vec::new();
        let mut new_intervals = Vec::new();
        for occurrence in &recovery.changes[0].occurrences {
            if let Some(span) = occurrence.old_span.as_ref() {
                old_intervals.extend(super::super::project(&old, span)?);
            }
            if let Some(span) = occurrence.new_span.as_ref() {
                new_intervals.extend(super::super::project(&new, span)?);
            }
        }
        assert_eq!(
            old_intervals,
            vec![SourceInterval {
                block_index: old.index[&BlockId(2)],
                start: 2,
                end: 3,
            }],
            "the emitted old range is the single changed token"
        );
        assert_eq!(
            new_intervals,
            vec![SourceInterval {
                block_index: new.index[&BlockId(102)],
                start: 2,
                end: 3,
            }],
            "the emitted new range is the single changed token"
        );
        assert_eq!(
            recovery.changed[0], old_intervals,
            "the old changed ownership must match the emitted range"
        );
        assert_eq!(
            recovery.changed[1], new_intervals,
            "the new changed ownership must match the emitted range"
        );

        // The actual content of the emitted ranges is C on the old side and X
        // on the new side.
        let old_tokens = &old.canonical[old_intervals[0].block_index]
            [old_intervals[0].start..old_intervals[0].end];
        let new_tokens = &new.canonical[new_intervals[0].block_index]
            [new_intervals[0].start..new_intervals[0].end];
        assert_eq!(token_text(old_tokens), "C");
        assert_eq!(token_text(new_tokens), "X");
        assert_ne!(token_text(old_tokens), token_text(new_tokens));

        // The localized edit witness is the internal one-token change, not the
        // whole-word domain range.
        assert_eq!(
            recovery.edits,
            vec![
                crate::diff::AtomicEdit {
                    old: 2..3,
                    new: 2..2
                },
                crate::diff::AtomicEdit {
                    old: 3..3,
                    new: 2..3
                },
            ],
            "one internal replacement witness: delete C and insert X"
        );

        // The repeated recovery must not append a second change or assumption.
        assert_eq!(recovery.changes.len(), 1);
        assert_eq!(recovery.replacements.len(), 1);
        Ok(())
    }

    #[test]
    fn positioned_replacement_recovery_holds_without_an_anchor() -> Result<()> {
        let (old_blocks, new_blocks) = positioned_replacement_fixture();
        let recovery =
            run_positioned_replacement(&old_blocks, &new_blocks, false, None, None, false)?;
        assert!(
            recovery.replacements.is_empty(),
            "no established neighbour may support the replacement"
        );
        assert!(!recovery.assumed);
        Ok(())
    }

    #[test]
    fn positioned_replacement_recovery_holds_on_partial_ownership() -> Result<()> {
        let (old_blocks, new_blocks) = positioned_replacement_fixture();
        let recovery =
            run_positioned_replacement(&old_blocks, &new_blocks, true, Some((0, 2)), None, false)?;
        assert!(
            recovery.replacements.is_empty(),
            "an intersecting ownership must hold the candidate"
        );
        assert!(!recovery.assumed);
        Ok(())
    }

    #[test]
    fn positioned_replacement_recovery_holds_on_budget_exhaustion() -> Result<()> {
        let (old_blocks, new_blocks) = positioned_replacement_fixture();
        let recovery =
            run_positioned_replacement(&old_blocks, &new_blocks, true, None, Some(0), false)?;
        assert!(
            recovery.replacements.is_empty(),
            "an exhausted budget must not leave a partial proof"
        );
        assert!(!recovery.truncated);
        assert!(!recovery.assumed);
        Ok(())
    }

    fn raw_recovery_fixture() -> (Vec<BlockText>, Vec<BlockText>) {
        let old_blocks = vec![
            spread_block(1, "Upper boundary line", 300.0, 700.0, 5.0),
            raw_issue_block(2, 300.0, 680.0),
            spread_block(3, "Lower boundary line", 300.0, 660.0, 5.0),
        ];
        let new_blocks = vec![
            spread_block(101, "Upper boundary line", 300.0, 700.0, 5.0),
            raw_issue_block(102, 300.0, 680.0),
            spread_block(103, "Lower boundary line", 300.0, 660.0, 5.0),
        ];
        (old_blocks, new_blocks)
    }

    struct RawRecovery {
        truncated: bool,
        work_used: usize,
        raw: usize,
        accepted: [usize; 2],
        records_raw: usize,
        relations: Vec<RawRelation>,
        pre_cached_tentative: bool,
        same_key: bool,
        candidate_kept: bool,
        root_has_normalization: bool,
        modified_key_differs: bool,
        modified_in_registry: bool,
        modified_has_normalization: bool,
        modified_reasons: Vec<crate::diff::AssessmentReason>,
        fourth_has_normalization: bool,
        fourth_key_has_raw: bool,
    }

    struct RawRelation {
        established: bool,
        has_gap: bool,
        has_normalization: bool,
        has_raw: bool,
    }

    fn raw_key_matches(
        assessor: &mut super::Assessor<'_, '_>,
        pre_cached_key: &Option<crate::diff::assessment::DomainKey>,
    ) -> Result<bool> {
        let Some(key) = pre_cached_key else {
            return Ok(false);
        };
        let Some((old_span, new_span)) = assessor.raw_source_equalities.first().cloned() else {
            return Ok(false);
        };
        let proposal = super::super::ProposedRelation {
            old: Some(old_span),
            new: Some(new_span),
            span_indices: [None, None],
            exact_recovery: true,
        };
        Ok(assessor.domain_key(&proposal)? == *key)
    }

    fn raw_relations(assessor: &super::Assessor<'_, '_>) -> Vec<RawRelation> {
        assessor
            .records
            .iter()
            .map(|record| RawRelation {
                established: record.outcome == super::RelationOutcome::Established,
                has_gap: record
                    .reasons
                    .contains(&crate::diff::AssessmentReason::ExtractionGap),
                has_normalization: record
                    .reasons
                    .contains(&crate::diff::AssessmentReason::NormalizationUncertainty),
                has_raw: record
                    .assumptions
                    .contains(&crate::diff::ComparisonAssumption::RawSourceEquality),
            })
            .collect()
    }

    #[derive(Default, Clone, Copy)]
    struct RawControls {
        partial_accept: Option<(usize, usize)>,
        full_accept: bool,
        changed: Option<(usize, usize, usize)>,
        budget: Option<usize>,
        repeat: bool,
        skip_anchors: bool,
        extraction_gap: bool,
        conflicting_candidate: bool,
        scoped_alignment: bool,
        pre_cache: bool,
        pre_cache_manual_span: bool,
    }

    fn run_raw_recovery(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        controls: RawControls,
        options: DiffOptions,
    ) -> Result<RawRecovery> {
        let mut pre_cached_tentative = false;
        let mut pre_cached_key = None;
        run_raw_recovery_inner(
            old_blocks,
            new_blocks,
            controls,
            options,
            &mut pre_cached_tentative,
            &mut pre_cached_key,
        )
    }

    fn run_raw_recovery_inner(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        controls: RawControls,
        options: DiffOptions,
        pre_cached_tentative: &mut bool,
        pre_cached_key: &mut Option<crate::diff::assessment::DomainKey>,
    ) -> Result<RawRecovery> {
        use crate::layout::{TrustedRunId, TrustedRunInterval};
        let old = side(old_blocks);
        let new = side(new_blocks);
        let old_ids = old_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let new_ids = new_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let alignment = if controls.extraction_gap {
            gapped_alignment(&old_ids, &new_ids)
        } else if controls.scoped_alignment {
            scoped_alignment(&old_ids, &new_ids)
        } else {
            unresolved_alignment(&old_ids, &new_ids)
        };
        let interval = |run: u64, start: usize, end: usize| {
            Some(TrustedRunInterval {
                run_id: TrustedRunId(run),
                start,
                end,
            })
        };
        let old_intervals = (0..old_blocks.len())
            .map(|index| interval(1, index, index + 1))
            .collect::<Vec<_>>();
        let new_intervals = (0..new_blocks.len())
            .map(|index| interval(2, index, index + 1))
            .collect::<Vec<_>>();
        let recovery = crate::diff::SentenceRecoveryInput {
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_intervals: &old_intervals,
            new_trusted_run_intervals: &new_intervals,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, Some(recovery), options)?;
        if controls.skip_anchors {
            let mut ownership = [
                super::super::Ownership::new(),
                super::super::Ownership::new(),
            ];
            let mut changes = Vec::new();
            let mut candidates = Vec::new();
            let truncated = assessor.recover_raw_source_equalities(
                &mut ownership,
                &mut changes,
                &mut candidates,
            )?;
            let records_raw = assessor
                .records
                .iter()
                .filter(|record| {
                    record
                        .assumptions
                        .contains(&crate::diff::ComparisonAssumption::RawSourceEquality)
                        && record.outcome == super::RelationOutcome::Established
                        && record.search == super::SearchCompleteness::Complete
                })
                .count();
            return Ok(RawRecovery {
                truncated,
                work_used: 0,
                raw: assessor.raw_source_equalities.len(),
                accepted: [ownership[0].accepted.len(), ownership[1].accepted.len()],
                records_raw,
                relations: raw_relations(&assessor),
                pre_cached_tentative: *pre_cached_tentative,
                same_key: false,
                candidate_kept: false,
                root_has_normalization: false,
                modified_key_differs: false,
                modified_in_registry: false,
                modified_has_normalization: false,
                modified_reasons: Vec::new(),
                fourth_has_normalization: false,
                fourth_key_has_raw: false,
            });
        }
        let tokens = |blocks: &[BlockText], index: usize| {
            blocks[index]
                .canonical
                .comparable_tokens()
                .expect("fixture tokens")
                .len()
        };
        let upper = tokens(old_blocks, 0);
        let lower = tokens(old_blocks, 2);
        assessor.local_anchors = vec![
            LocalDomain {
                old_span: span(1, upper),
                new_span: span(101, upper),
                source_bounded: true,
            },
            LocalDomain {
                old_span: span(3, lower),
                new_span: span(103, lower),
                source_bounded: true,
            },
        ];
        for (old_block, new_block, count) in [(1_u64, 101_u64, upper), (3_u64, 103_u64, lower)] {
            let proposal = super::super::ProposedRelation {
                old: Some(span(old_block, count)),
                new: Some(span(new_block, count)),
                span_indices: [None, None],
                exact_recovery: true,
            };
            let key = assessor.domain_key(&proposal)?;
            assessor.prove_domain(&key)?;
        }
        let mut ownership = [
            super::super::Ownership::new(),
            super::super::Ownership::new(),
        ];
        ownership[0].accept(&old, &span(1, upper), 64)?;
        ownership[1].accept(&new, &span(101, upper), 64)?;
        ownership[0].accept(&old, &span(3, lower), 64)?;
        ownership[1].accept(&new, &span(103, lower), 64)?;
        if controls.full_accept {
            ownership[0].accept(&old, &span(2, tokens(old_blocks, 1)), 64)?;
            ownership[1].accept(&new, &span(102, tokens(new_blocks, 1)), 64)?;
        }
        if let Some((side, count)) = controls.partial_accept {
            let block = if side == 0 { BlockId(2) } else { BlockId(102) };
            let source = if side == 0 { &old } else { &new };
            ownership[side].accept(source, &span(block.0, count), 64)?;
        }
        if let Some((side, block_index, count)) = controls.changed {
            ownership[side].changed.push(super::SourceInterval {
                block_index,
                start: 0,
                end: count,
            });
        }
        if let Some(budget) = controls.budget {
            assessor.remaining_work = budget;
        }
        let work_before = assessor.remaining_work;
        if controls.pre_cache {
            let established = assessor
                .collect_established_blocks(&ownership)?
                .unwrap_or_default();
            let Some(mask) = assessor.stationary_candidate_mask(&ownership)? else {
                return Err(super::super::invalid("pre-cache mask budget"));
            };
            let mut probe_budget = 10_000_000usize;
            let discovered = super::super::views::discover_raw_source_equalities_masked(
                assessor.sides,
                recovery,
                &established,
                &mut probe_budget,
                assessor.options.max_assessment_ranges,
                &mask,
            )?;
            let manual = controls.pre_cache_manual_span.then(|| {
                let middle = super::TextSpan {
                    blocks: vec![old_blocks[1].block],
                    separator: None,
                    canonical_range: crate::normalize::ScalarRange {
                        start: 0,
                        end: tokens(old_blocks, 1),
                    },
                    comparable_range: super::super::TokenRange {
                        start: 0,
                        end: tokens(old_blocks, 1),
                    },
                };
                let middle_new = super::TextSpan {
                    blocks: vec![new_blocks[1].block],
                    separator: None,
                    canonical_range: crate::normalize::ScalarRange {
                        start: 0,
                        end: tokens(new_blocks, 1),
                    },
                    comparable_range: super::super::TokenRange {
                        start: 0,
                        end: tokens(new_blocks, 1),
                    },
                };
                LocalDomain {
                    old_span: middle,
                    new_span: middle_new,
                    source_bounded: false,
                }
            });
            if let Some(domain) = manual.as_ref().or(discovered.first()) {
                let old_span = domain.old_span.clone();
                let new_span = domain.new_span.clone();
                assessor.local_domains.push(LocalDomain {
                    old_span: old_span.clone(),
                    new_span: new_span.clone(),
                    source_bounded: false,
                });
                let proposal = super::super::ProposedRelation {
                    old: Some(old_span),
                    new: Some(new_span),
                    span_indices: [None, None],
                    exact_recovery: true,
                };
                let key = assessor.domain_key(&proposal)?;
                assessor.prove_domain(&key)?;
                *pre_cached_tentative = assessor.domains.get(&key).is_some_and(|proof| {
                    assessor.records[proof.relation].outcome != super::RelationOutcome::Established
                });
                *pre_cached_key = Some(key);
            }
        }
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        if controls.conflicting_candidate {
            let span_of = |blocks: &[BlockText], index: usize| super::TextSpan {
                blocks: vec![blocks[index].block],
                separator: None,
                canonical_range: crate::normalize::ScalarRange {
                    start: 0,
                    end: tokens(blocks, index),
                },
                comparable_range: super::super::TokenRange {
                    start: 0,
                    end: tokens(blocks, index),
                },
            };
            candidates.push(ChangeCandidate {
                change: ChangeEvent {
                    kind: crate::diff::ChangeKind::Replacement,
                    occurrences: vec![crate::diff::ChangeOccurrence {
                        old_span: Some(span_of(old_blocks, 1)),
                        new_span: Some(span_of(new_blocks, 1)),
                    }],
                    confidence: crate::diff::Confidence::High,
                    tags: Vec::new(),
                },
                relation: 0,
                alternative_group: 0,
            });
        }
        let mut truncated = assessor.recover_raw_source_equalities(
            &mut ownership,
            &mut changes,
            &mut candidates,
        )?;
        if controls.repeat {
            truncated |= assessor.recover_raw_source_equalities(
                &mut ownership,
                &mut changes,
                &mut candidates,
            )?;
        }
        let records_raw = assessor
            .records
            .iter()
            .filter(|record| {
                record
                    .assumptions
                    .contains(&crate::diff::ComparisonAssumption::RawSourceEquality)
                    && record.outcome == super::RelationOutcome::Established
                    && record.search == super::SearchCompleteness::Complete
            })
            .count();
        let root_has_normalization = assessor
            .source_reasons()
            .contains(&crate::diff::AssessmentReason::NormalizationUncertainty);
        let mut modified_key_differs = false;
        let mut modified_in_registry = false;
        let mut modified_has_normalization = false;
        let mut modified_reasons = Vec::new();
        let registry_key = assessor
            .raw_source_equalities
            .first()
            .map(|(old, new)| super::super::ProposedRelation {
                old: Some(old.clone()),
                new: Some(new.clone()),
                span_indices: [None, None],
                exact_recovery: true,
            })
            .map(|proposal| assessor.domain_key(&proposal))
            .transpose()?;
        if let Some((old_span, mut new_span)) = assessor.raw_source_equalities.first().cloned() {
            // Expand past the exact domain so the proposal is no longer
            // contained and cannot map onto the proven parent key.
            new_span.canonical_range.end += 1;
            new_span.comparable_range.end += 1;
            modified_in_registry = assessor
                .raw_source_equalities
                .iter()
                .any(|(old, new)| old == &old_span && new == &new_span);
            let proposal = super::super::ProposedRelation {
                old: Some(old_span),
                new: Some(new_span),
                span_indices: [None, None],
                exact_recovery: true,
            };
            let key = assessor.domain_key(&proposal)?;
            modified_key_differs = registry_key.as_ref() != Some(&key);
            let (reasons, _) = assessor.domain_reasons(&key)?;
            modified_has_normalization =
                reasons.contains(&crate::diff::AssessmentReason::NormalizationUncertainty);
            modified_reasons = reasons;
        }
        let mut fourth_has_normalization = false;
        let mut fourth_key_has_raw = false;
        if let (Some(old_span), Some(new_span)) = (
            assessor.sides[0]
                .blocks
                .get(3)
                .map(|block| super::TextSpan {
                    blocks: vec![block.block],
                    separator: None,
                    canonical_range: crate::normalize::ScalarRange {
                        start: 0,
                        end: block.canonical.text.chars().count(),
                    },
                    comparable_range: super::super::TokenRange {
                        start: 0,
                        end: block.canonical.text.chars().count(),
                    },
                }),
            assessor.sides[1]
                .blocks
                .get(3)
                .map(|block| super::TextSpan {
                    blocks: vec![block.block],
                    separator: None,
                    canonical_range: crate::normalize::ScalarRange {
                        start: 0,
                        end: block.canonical.text.chars().count(),
                    },
                    comparable_range: super::super::TokenRange {
                        start: 0,
                        end: block.canonical.text.chars().count(),
                    },
                }),
        ) {
            let proposal = super::super::ProposedRelation {
                old: Some(old_span),
                new: Some(new_span),
                span_indices: [None, None],
                exact_recovery: true,
            };
            let key = assessor.domain_key(&proposal)?;
            let (reasons, _) = assessor.domain_reasons(&key)?;
            fourth_has_normalization =
                reasons.contains(&crate::diff::AssessmentReason::NormalizationUncertainty);
            fourth_key_has_raw = assessor.domains.get(&key).is_some_and(|proof| {
                assessor.records[proof.relation]
                    .assumptions
                    .contains(&crate::diff::ComparisonAssumption::RawSourceEquality)
            });
        }
        Ok(RawRecovery {
            truncated,
            work_used: work_before.saturating_sub(assessor.remaining_work),
            raw: assessor.raw_source_equalities.len(),
            accepted: [ownership[0].accepted.len(), ownership[1].accepted.len()],
            records_raw,
            relations: raw_relations(&assessor),
            pre_cached_tentative: *pre_cached_tentative,
            same_key: raw_key_matches(&mut assessor, pre_cached_key)?,
            candidate_kept: candidates.len() == usize::from(controls.conflicting_candidate),
            root_has_normalization,
            modified_key_differs,
            modified_in_registry,
            modified_has_normalization,
            modified_reasons,
            fourth_has_normalization,
            fourth_key_has_raw,
        })
    }

    #[test]
    fn raw_source_equality_pass_scopes_its_proof_to_the_exact_pair() -> Result<()> {
        let (mut old_blocks, mut new_blocks) = raw_recovery_fixture();
        old_blocks.push(raw_issue_block(4, 300.0, 640.0));
        new_blocks.push(raw_issue_block(104, 300.0, 640.0));
        new_blocks[3].raw.text = "X\nY\nZ".to_owned();
        new_blocks[3].canonical.text = "XY\nZ".to_owned();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                scoped_alignment: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(
            recovery.raw, 1,
            "only the central exact pair may enter the registry"
        );
        assert_eq!(
            recovery.accepted,
            [3, 3],
            "only the central pair may be owned beyond the two anchors"
        );
        assert!(
            recovery
                .relations
                .iter()
                .any(|relation| relation.has_raw && relation.established),
            "the central raw relation must be Established"
        );
        assert!(
            !recovery.modified_in_registry,
            "a changed range of the same pair must not carry RawSourceEquality"
        );
        assert!(
            !recovery
                .relations
                .iter()
                .any(|relation| relation.has_raw && !relation.established),
            "RawSourceEquality must never attach to an unproven relation"
        );
        assert!(
            recovery.root_has_normalization,
            "the root reasons must carry NormalizationUncertainty"
        );
        assert!(
            recovery.modified_key_differs,
            "the expanded span must form a different key"
        );
        assert!(
            !recovery.modified_in_registry,
            "a changed range of the same pair must not carry RawSourceEquality"
        );
        assert!(
            recovery.modified_has_normalization,
            "the changed key must keep NormalizationUncertainty: {:?}",
            recovery.modified_reasons
        );
        assert!(
            recovery.fourth_has_normalization,
            "the fourth block's own key must keep NormalizationUncertainty"
        );
        assert!(
            !recovery.fourth_key_has_raw,
            "the fourth block must not carry RawSourceEquality"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_keeps_a_cached_pair_without_a_new_proof() -> Result<()> {
        let (old_blocks, mut new_blocks) = raw_recovery_fixture();
        new_blocks[1].raw.text = "A\nB\nX".to_owned();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                pre_cache: true,
                pre_cache_manual_span: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert!(
            recovery.pre_cached_tentative,
            "the fixture must first cache a tentative relation"
        );
        assert_eq!(
            recovery.raw, 0,
            "a raw difference must leave the registry empty"
        );
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "a cached key without its own new proof must not be promoted"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_re_evaluates_a_tentative_cache() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                pre_cache: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert!(
            recovery.pre_cached_tentative,
            "the fixture must first cache a tentative relation for the exact pair"
        );
        assert!(
            recovery.same_key,
            "the raw registry must contain the exact pre-cached domain key"
        );
        assert_eq!(
            recovery.accepted,
            [3, 3],
            "the raw proof must own the cached pair once it arrives"
        );
        assert_eq!(recovery.raw, 1, "the registry must not duplicate the pair");
        assert!(
            recovery
                .relations
                .iter()
                .any(|relation| relation.has_raw && relation.established),
            "a new Established raw relation must exist"
        );
        assert!(
            recovery
                .relations
                .iter()
                .any(|relation| !relation.established && relation.has_normalization),
            "the earlier tentative record must stay as history"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_keeps_an_extraction_gap_on_the_pair() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                extraction_gap: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "the gapped pair must stay unowned while the outer anchors stay owned"
        );
        assert_eq!(
            recovery.records_raw, 0,
            "no raw proof may be Established across an extraction gap"
        );
        assert_eq!(
            recovery.raw, 1,
            "the raw registry must still discover the exact pair"
        );
        assert!(
            recovery
                .relations
                .iter()
                .any(|relation| relation.has_raw && !relation.established && relation.has_gap),
            "the exact relation must keep the extraction gap: {:?}",
            recovery
                .relations
                .iter()
                .map(|relation| (relation.established, relation.has_gap, relation.has_raw))
                .collect::<Vec<_>>()
        );
        assert!(
            recovery
                .relations
                .iter()
                .filter(|relation| relation.established)
                .count()
                >= 2,
            "the outer anchors must be Established by their own proof"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_closes_an_issue_member_through_the_real_caller() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls::default(),
            DiffOptions::default(),
        )?;
        assert!(!recovery.truncated);
        assert_eq!(
            recovery.raw, 1,
            "the raw-equality member must be recorded once"
        );
        assert!(
            recovery.records_raw >= 1,
            "the paired raw proof must be Established: {}",
            recovery.records_raw
        );
        assert_eq!(
            recovery.accepted,
            [3, 3],
            "the whole four-token member must be owned on both sides"
        );

        // The ordinary stationary pass alone must keep holding the issue
        // member, so only the paired raw proof may own it.
        let stationary = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls::default(),
            DiffOptions::default(),
        )?;
        assert_eq!(
            stationary.stationary, 0,
            "the stationary mode must still hold"
        );
        assert_eq!(
            stationary.accepted,
            [2, 2],
            "the issue member stays unowned"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_holds_on_a_raw_difference() -> Result<()> {
        let (old_blocks, mut new_blocks) = raw_recovery_fixture();
        new_blocks[1].raw.text = "A\nB\nX".to_owned();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls::default(),
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.raw, 0, "a raw difference must hold");
        assert_eq!(recovery.records_raw, 0);
        assert_eq!(recovery.accepted, [2, 2]);
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_holds_without_an_independent_anchor() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                skip_anchors: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.raw, 0, "no anchor means no proof");
        assert_eq!(recovery.accepted, [0, 0]);
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_keeps_a_changed_ownership_claim() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                changed: Some((0, 1, 4)),
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.raw, 0, "a changed claim must hold the candidate");
        assert_eq!(recovery.accepted, [2, 2]);
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_never_reports_a_partial_result_on_a_budget_cut() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let full = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls::default(),
            DiffOptions::default(),
        )?;
        assert_eq!(full.accepted, [3, 3]);
        assert!(full.work_used > 0, "the full run must consume work");
        // The budget is set after the anchors are proven and owned, so these
        // cuts happen inside the raw pass itself.
        for budget in [0usize, 1, 64] {
            let recovery = run_raw_recovery(
                &old_blocks,
                &new_blocks,
                RawControls {
                    budget: Some(budget),
                    ..RawControls::default()
                },
                DiffOptions::default(),
            )?;
            assert_eq!(recovery.raw, 0, "budget {budget} must not register a pair");
            assert_eq!(
                recovery.accepted,
                [2, 2],
                "budget {budget} must not own the pair"
            );
            assert_eq!(
                recovery.records_raw, 0,
                "budget {budget} must not establish a relation"
            );
        }
        // A cut after discovery started may keep the queued registry entry,
        // but it must never establish a relation or extend ownership.
        let mid = full.work_used / 2;
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                budget: Some(mid),
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "the mid cut must not own the pair"
        );
        assert_eq!(
            recovery.records_raw, 0,
            "the mid cut must not establish a relation"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_keeps_a_conflicting_candidate() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                conflicting_candidate: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert!(
            recovery.candidate_kept,
            "the conflicting candidate must stay in the queue"
        );
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "a conflicting candidate must hold the central ownership"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_respects_the_range_limit() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        // Two anchors already fill the limit, so the raw addition must be
        // held without touching the established anchors.
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls::default(),
            DiffOptions {
                max_assessment_ranges: 2,
                ..DiffOptions::default()
            },
        )?;
        assert_eq!(recovery.raw, 0, "a full range limit must not register");
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "a full range limit must not own the pair"
        );
        Ok(())
    }

    #[test]
    fn raw_source_equality_pass_does_not_repeat_work() -> Result<()> {
        let (old_blocks, new_blocks) = raw_recovery_fixture();
        let recovery = run_raw_recovery(
            &old_blocks,
            &new_blocks,
            RawControls {
                repeat: true,
                ..RawControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.raw, 1, "the registry must not duplicate");
        assert_eq!(recovery.accepted, [3, 3], "ownership must not duplicate");
        Ok(())
    }

    #[test]
    fn stationary_pass_closes_a_stationary_member_through_the_real_caller() -> Result<()> {
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: None,
                full_accept: false,
                changed: None,
                probe_empty: true,
                budget: None,
                repeat: false,
            },
            DiffOptions::default(),
        )?;
        assert!(!recovery.truncated);
        assert_eq!(
            recovery.stationary, 1,
            "the stationary member must be recorded once"
        );
        assert!(
            recovery.records_stationary >= 1,
            "an established complete relation must carry the stationary assumption"
        );
        assert_eq!(
            recovery.accepted,
            [3, 3],
            "the candidate range joins the two boundary ranges per side"
        );
        assert_eq!(
            recovery.empty_projection,
            Some(false),
            "an empty projection must hold instead of adopting the candidate"
        );
        Ok(())
    }

    #[test]
    fn stationary_pass_holds_a_partially_accepted_candidate() -> Result<()> {
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: Some((0, 5)),
                full_accept: false,
                changed: None,
                probe_empty: false,
                budget: None,
                repeat: false,
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.stationary, 0);
        assert_eq!(recovery.records_stationary, 0);
        assert_eq!(recovery.accepted, [3, 2]);
        Ok(())
    }

    #[test]
    fn stationary_pass_holds_a_changed_overlap() -> Result<()> {
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let new_side = side(&new_blocks);
        let new_index = new_side.index[&BlockId(102)];
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: None,
                full_accept: false,
                changed: Some((1, new_index, 5)),
                probe_empty: false,
                budget: None,
                repeat: false,
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.stationary, 0);
        assert_eq!(recovery.records_stationary, 0);
        assert_eq!(recovery.accepted, [2, 2]);
        Ok(())
    }

    #[test]
    fn stationary_pass_skips_heavy_search_when_every_candidate_is_owned() -> Result<()> {
        // Every whole candidate block is already accepted, so the reject-only
        // mask leaves no eligible candidate and the pass returns before any
        // view or reference exploration.
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                full_accept: true,
                ..StationaryControls::default()
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.stationary, 0);
        assert_eq!(recovery.records_stationary, 0);
        assert_eq!(recovery.accepted, [3, 3]);
        assert!(
            recovery.work_used < 10_000,
            "an all-owned pass must not explore views: {} work used",
            recovery.work_used
        );
        Ok(())
    }

    #[test]
    fn stationary_pass_holds_a_partially_accepted_candidate_on_the_new_side() -> Result<()> {
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: Some((1, 5)),
                full_accept: false,
                changed: None,
                probe_empty: false,
                budget: None,
                repeat: false,
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.stationary, 0);
        assert_eq!(recovery.records_stationary, 0);
        assert_eq!(recovery.accepted, [2, 3]);
        Ok(())
    }

    #[test]
    fn stationary_pass_does_not_repeat_a_rediscovered_domain() -> Result<()> {
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: None,
                full_accept: false,
                changed: None,
                probe_empty: false,
                budget: None,
                repeat: true,
            },
            DiffOptions::default(),
        )?;
        assert_eq!(
            recovery.stationary, 1,
            "the same domain must not be queued or assumed twice"
        );
        assert_eq!(recovery.accepted, [3, 3]);
        Ok(())
    }

    #[test]
    fn stationary_pass_does_not_queue_at_the_output_sentinel() -> Result<()> {
        // Two boundary relations already fill max-1 records, so the sentinel
        // slot is reserved and no new queue or assumption may be added.
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let options = DiffOptions {
            max_assessment_ranges: 3,
            ..DiffOptions::default()
        };
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: None,
                full_accept: false,
                changed: None,
                probe_empty: false,
                budget: None,
                repeat: false,
            },
            options,
        )?;
        assert_eq!(recovery.stationary, 0);
        assert_eq!(recovery.records_stationary, 0);
        assert_eq!(recovery.accepted, [2, 2]);
        Ok(())
    }

    #[test]
    fn stationary_pass_yields_nothing_when_discovery_is_cut() -> Result<()> {
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let recovery = run_stationary_recovery(
            &old_blocks,
            &new_blocks,
            StationaryControls {
                partial_accept: None,
                full_accept: false,
                changed: None,
                probe_empty: false,
                budget: Some(1),
                repeat: false,
            },
            DiffOptions::default(),
        )?;
        assert_eq!(recovery.stationary, 0);
        assert_eq!(recovery.records_stationary, 0);
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "a discovery cut must not queue, assume or own anything"
        );
        Ok(())
    }

    #[test]
    fn stationary_pass_keeps_confirmed_members_and_holds_unconfirmed_ones() -> Result<()> {
        // Find the smallest budget that queues the stationary member, then
        // look for a budget just above it where the queued member is not yet
        // proven. The confirmed part must be kept; the unconfirmed member must
        // stay unowned and without an established relation.
        let (old_blocks, new_blocks) = stationary_recovery_fixture();
        let mut low = 0;
        let mut high = 100_000;
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            let recovery = run_stationary_recovery(
                &old_blocks,
                &new_blocks,
                StationaryControls {
                    budget: Some(mid),
                    ..StationaryControls::default()
                },
                DiffOptions::default(),
            )?;
            if recovery.stationary == 0 {
                low = mid;
            } else {
                high = mid;
            }
        }
        let queued = high;
        let mut unproven = None;
        for budget in queued..queued.saturating_add(64) {
            let recovery = run_stationary_recovery(
                &old_blocks,
                &new_blocks,
                StationaryControls {
                    budget: Some(budget),
                    ..StationaryControls::default()
                },
                DiffOptions::default(),
            )?;
            if recovery.stationary == 1 && recovery.records_stationary == 0 {
                unproven = Some((budget, recovery));
                break;
            }
        }
        let (budget, recovery) = unproven.expect("a processing cut window must exist");
        assert!(recovery.truncated, "budget {budget} must report a cut");
        assert_eq!(
            recovery.accepted,
            [2, 2],
            "an unconfirmed member must not be owned"
        );
        Ok(())
    }

    fn bracketed_recovery_fixture() -> (Vec<BlockText>, Vec<BlockText>) {
        let old_blocks = vec![
            spread_block(1, "Upper boundary line", 300.0, 700.0, 5.0),
            spread_block(2, "Filing year 2024 statement", 300.0, 680.0, 5.0),
            spread_block(3, "Lower boundary line", 300.0, 660.0, 5.0),
        ];
        let new_blocks = vec![
            spread_block(101, "Upper boundary line", 300.0, 700.0, 5.0),
            spread_block(102, "Filing year 2025 statement", 300.0, 680.0, 5.0),
            spread_block(103, "Lower boundary line", 300.0, 660.0, 5.0),
        ];
        (old_blocks, new_blocks)
    }

    #[test]
    fn bracketed_pass_closes_a_bracketed_replacement_once() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_recovery_fixture();
        let recovery =
            run_bracketed_recovery(&old_blocks, &new_blocks, &[], DiffOptions::default(), 0)?;
        assert!(!recovery.truncated);
        assert_eq!(
            recovery.domains, 1,
            "only the bracketed candidate may be discovered"
        );
        assert_eq!(
            recovery.assumptions, 1,
            "the assumption list must not repeat a queued domain across rounds"
        );
        assert_eq!(
            recovery.accepted,
            [3, 3],
            "the candidate range joins the two boundary ranges per side"
        );
        assert_eq!(recovery.changes, 1);
        Ok(())
    }

    #[test]
    fn bracketed_pass_stops_without_further_proof_after_an_output_limit() -> Result<()> {
        // The pre-existing edit domain hits the range output limit, so the
        // recovery stops at once: the bracketed candidate is never queued,
        // proven or accepted.
        let (old_blocks, new_blocks) = bracketed_recovery_fixture();
        let candidate_tokens = old_blocks[1]
            .canonical
            .comparable_tokens()
            .expect("candidate tokens")
            .len();
        let preexisting = [LocalDomain {
            old_span: span(2, candidate_tokens),
            new_span: span(102, candidate_tokens),
            source_bounded: true,
        }];
        // The range limit leaves room for the relation records but the
        // already accepted ownership makes the edit domain exceed it, so the
        // recovery stops with an output limit while the work budget remains.
        let options = DiffOptions {
            max_assessment_ranges: 6,
            ..DiffOptions::default()
        };
        let recovery = run_bracketed_recovery(&old_blocks, &new_blocks, &preexisting, options, 5)?;
        assert!(
            recovery.truncated,
            "the output limit must be reported: domains={} assumptions={} accepted={:?} changes={}",
            recovery.domains, recovery.assumptions, recovery.accepted, recovery.changes
        );
        assert_eq!(
            recovery.domains, 1,
            "no bracketed domain may be added after the stop"
        );
        assert_eq!(
            recovery.assumptions, 0,
            "no bracketed assumption may be recorded after the stop"
        );
        assert_eq!(
            recovery.accepted,
            [7, 7],
            "no ownership may be accepted after the stop"
        );
        assert_eq!(
            recovery.changes, 0,
            "no change may be committed after the stop"
        );
        Ok(())
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

    fn strict_closed_fragment_span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: super::super::super::TokenRange { start, end },
        }
    }

    /// Runs the local recovery and the deferred equal-fragment tail pass over
    /// one strict-closed mid-block fragment and returns the old-side
    /// resolution and whether the new position premise was recorded.
    fn run_strict_closed_fragment(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        start: usize,
        end: usize,
        prepare: impl FnOnce(&mut [Ownership; 2], &mut Vec<ChangeCandidate>),
    ) -> Result<(Vec<super::super::ResolutionRange>, bool)> {
        run_strict_closed_fragment_with(
            old_blocks,
            new_blocks,
            start,
            end,
            prepare,
            &[],
            DiffOptions::default(),
        )
    }

    /// The same fixture with a caller-chosen proven-region list and options.
    fn run_strict_closed_fragment_with(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        start: usize,
        end: usize,
        prepare: impl FnOnce(&mut [Ownership; 2], &mut Vec<ChangeCandidate>),
        proven: &[ProvenChangedRegion],
        options: DiffOptions,
    ) -> Result<(Vec<super::super::ResolutionRange>, bool)> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let alignment = unresolved_alignment(
            &old_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            &new_blocks
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
        );
        let mut assessor = super::super::Assessor::new([&old, &new], &alignment, None, options)?;
        assessor.local_domains = vec![LocalDomain {
            old_span: strict_closed_fragment_span(old_blocks[0].block.0, start, end),
            new_span: strict_closed_fragment_span(new_blocks[0].block.0, start, end),
            source_bounded: false,
        }];
        let mut ownership = [Ownership::new(), Ownership::new()];
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        prepare(&mut ownership, &mut candidates);
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        assessor.recover_equal_fragments(&mut ownership, &candidates, proven)?;
        let adopted = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
        });
        let [old_ownership, _new_ownership] = ownership;
        let resolution = old_ownership.finish(&old, assessor.options.max_assessment_ranges)?;
        Ok((resolution, adopted))
    }

    fn fragment_state(
        resolution: &[super::super::ResolutionRange],
        index: usize,
    ) -> Option<super::super::ResolutionState> {
        resolution
            .iter()
            .find(|range| {
                range.block == BlockId(1)
                    && range.comparable_range.start <= index
                    && index < range.comparable_range.end
            })
            .map(|range| range.state)
    }

    #[test]
    fn strict_closed_interior_fragment_gains_equal_coverage() -> Result<()> {
        let text = "outer unknown shared fragment unknown tail";
        let start = "outer unknown ".chars().count();
        let end = start + "shared fragment".chars().count();
        let old_blocks = [sourced_block(1, text)];
        let new_blocks = [sourced_block(101, text)];
        let (resolution, adopted) =
            run_strict_closed_fragment(&old_blocks, &new_blocks, start, end, |_, _| {})?;
        assert!(
            adopted,
            "the adopted fragment must record its position premise"
        );
        assert!(
            resolution.iter().any(|range| {
                range.block == BlockId(1)
                    && range.state == super::super::ResolutionState::Equal
                    && range.comparable_range.start == start
                    && range.comparable_range.end == end
            }),
            "{resolution:?}"
        );
        assert_eq!(
            fragment_state(&resolution, 0),
            Some(super::super::ResolutionState::Unresolved)
        );
        assert_eq!(
            fragment_state(&resolution, end),
            Some(super::super::ResolutionState::Unresolved)
        );
        Ok(())
    }

    #[test]
    fn conflicting_or_unproven_fragments_gain_no_ownership() -> Result<()> {
        let text = "outer unknown shared fragment unknown tail";
        let start = "outer unknown ".chars().count();
        let end = start + "shared fragment".chars().count();
        let old_blocks = [sourced_block(1, text)];
        let new_blocks = [sourced_block(101, text)];
        let no_equal = |resolution: &[super::super::ResolutionRange]| {
            resolution
                .iter()
                .all(|range| range.state != super::super::ResolutionState::Equal)
        };

        // A changed-ownership conflict adopts nothing.
        let (resolution, adopted) =
            run_strict_closed_fragment(&old_blocks, &new_blocks, start, end, |ownership, _| {
                let interval = SourceInterval {
                    block_index: 0,
                    start,
                    end,
                };
                ownership[0].accepted.push(interval);
                ownership[0].changed.push(interval);
            })?;
        assert!(!adopted);
        assert!(no_equal(&resolution), "{resolution:?}");

        // A candidate conflict adopts nothing.
        let (resolution, adopted) =
            run_strict_closed_fragment(&old_blocks, &new_blocks, start, end, |_, candidates| {
                candidates.push(ChangeCandidate {
                    change: Change::single_occurrence(
                        ChangeKind::Replacement,
                        Some(strict_closed_fragment_span(1, start, end)),
                        Some(strict_closed_fragment_span(101, start, end)),
                        Confidence::High,
                        Vec::new(),
                    ),
                    relation: 0,
                    alternative_group: 0,
                });
            })?;
        assert!(!adopted);
        assert!(no_equal(&resolution), "{resolution:?}");

        // A missing position signature adopts nothing.
        let mut old_missing = sourced_block(1, text);
        old_missing.position_signatures = None;
        let (resolution, adopted) =
            run_strict_closed_fragment(&[old_missing], &new_blocks, start, end, |_, _| {})?;
        assert!(!adopted);
        assert!(no_equal(&resolution), "{resolution:?}");

        // A raw source that is not the selected glyph adopts nothing.
        let mut old_bad_raw = sourced_block(1, text);
        old_bad_raw.raw.source_map[start].source = TextSource {
            atoms: vec![TextSourceAtom::Glyph(GlyphId(9999))].into(),
        };
        let (resolution, adopted) =
            run_strict_closed_fragment(&[old_bad_raw], &new_blocks, start, end, |_, _| {})?;
        assert!(!adopted);
        assert!(no_equal(&resolution), "{resolution:?}");

        // A malformed event source adopts nothing.
        let mut old_bad_event = sourced_block(1, text);
        old_bad_event.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::WhitespaceCollapse,
            raw_range: ScalarRange {
                start,
                end: start + 1,
            },
            canonical_range: ScalarRange {
                start,
                end: start + 1,
            },
            source: TextSource::default(),
        }];
        let (resolution, adopted) =
            run_strict_closed_fragment(&[old_bad_event], &new_blocks, start, end, |_, _| {})?;
        assert!(!adopted);
        assert!(no_equal(&resolution), "{resolution:?}");

        // A fragment already contained in the accepted ownership needs no
        // repeated proof or premise, and the existing range stays equal.
        let (resolution, adopted) =
            run_strict_closed_fragment(&old_blocks, &new_blocks, start, end, |ownership, _| {
                for owner in ownership.iter_mut() {
                    owner.accepted.push(SourceInterval {
                        block_index: 0,
                        start: 0,
                        end: text.chars().count(),
                    });
                }
            })?;
        assert!(!adopted);
        assert!(
            resolution.iter().any(|range| {
                range.block == BlockId(1)
                    && range.state == super::super::ResolutionState::Equal
                    && range.comparable_range.start == 0
                    && range.comparable_range.end == text.chars().count()
            }),
            "{resolution:?}"
        );

        Ok(())
    }

    #[test]
    fn proven_changed_region_blocks_deferred_adoption() -> Result<()> {
        let text = "outer unknown shared fragment unknown tail";
        let start = "outer unknown ".chars().count();
        let end = start + "shared fragment".chars().count();
        let old_blocks = [sourced_block(1, text)];
        let new_blocks = [sourced_block(101, text)];
        // The whole fragment is already claimed by a proven changed region,
        // so the tail pass must keep it unresolved and adopt nothing.
        let region = ProvenChangedRegion {
            old_span: Some(strict_closed_fragment_span(1, start, end)),
            new_span: Some(strict_closed_fragment_span(101, start, end)),
            proof: ChangedRegionProof::ExactTokenMultisetMismatch,
            confidence: Confidence::High,
        };
        let (resolution, adopted) = run_strict_closed_fragment_with(
            &old_blocks,
            &new_blocks,
            start,
            end,
            |_, _| {},
            &[region],
            DiffOptions::default(),
        )?;
        assert!(!adopted);
        assert!(
            resolution
                .iter()
                .all(|range| range.state != super::super::ResolutionState::Equal),
            "{resolution:?}"
        );
        Ok(())
    }

    #[test]
    fn deferred_proof_exhaustion_keeps_existing_records() -> Result<()> {
        let text = "outer unknown shared fragment unknown tail";
        let start = "outer unknown ".chars().count();
        let end = start + "shared fragment".chars().count();
        let old_blocks = [sourced_block(1, text)];
        let new_blocks = [sourced_block(101, text)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assessor.local_domains = vec![LocalDomain {
            old_span: strict_closed_fragment_span(1, start, end),
            new_span: strict_closed_fragment_span(101, start, end),
            source_bounded: false,
        }];
        let mut ownership = [Ownership::new(), Ownership::new()];
        ownership[0].accepted.push(SourceInterval {
            block_index: 0,
            start: 0,
            end: 2,
        });
        ownership[0].changed.push(SourceInterval {
            block_index: 0,
            start: 0,
            end: 2,
        });
        let accepted = ownership[0].accepted.clone();
        let changed = ownership[0].changed.clone();
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        assert!(
            !assessor.equal_fragment_candidates.is_empty(),
            "the recovery must record the strict-closed fragment"
        );
        let outcomes = assessor
            .records
            .iter()
            .map(|record| (record.outcome, record.search))
            .collect::<Vec<_>>();
        // An exhausted tail pass is a no-op: it commits no interval, records
        // no premise, demotes no record and leaves the candidates pending.
        assessor.remaining_work = 0;
        assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
        assert_eq!(ownership[0].accepted, accepted);
        assert_eq!(ownership[0].changed, changed);
        assert_eq!(
            assessor
                .records
                .iter()
                .map(|record| (record.outcome, record.search))
                .collect::<Vec<_>>(),
            outcomes
        );
        assert!(assessor.records.iter().all(|record| {
            !record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
        }));
        assert!(changes.is_empty());
        assert!(candidates.is_empty());
        Ok(())
    }

    #[test]
    fn deferred_proof_charges_before_copying_a_wide_container_domain() -> Result<()> {
        let old_blocks = [sourced_block(1, "container domain text")];
        let new_blocks = [sourced_block(101, "container domain text")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        // Simulate one wide recorded container domain: the deferred list keeps
        // only the relation index, and the tail pass must pay for the
        // transient span copy before it allocates.
        let wide = TextSpan {
            blocks: (1..=64).map(BlockId).collect(),
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 0, end: 64 },
            comparable_range: super::super::super::TokenRange { start: 0, end: 64 },
        };
        let relation = assessor.record(crate::diff::RelationAssessment {
            old_span: Some(wide.clone()),
            new_span: Some(wide),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: Vec::new(),
            reasons: Vec::new(),
        })?;
        assessor.equal_fragment_candidates.push(relation);
        assert_eq!(
            std::mem::size_of_val(&assessor.equal_fragment_candidates[0]),
            std::mem::size_of::<usize>(),
            "the deferred list must not retain a source span copy"
        );
        let mut ownership = [Ownership::new(), Ownership::new()];
        assessor.remaining_work = 1;
        let candidates = Vec::new();
        assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
        assert!(ownership[0].accepted.is_empty() && ownership[1].accepted.is_empty());
        assert!(ownership[0].changed.is_empty() && ownership[1].changed.is_empty());
        assert!(assessor.records.iter().all(|record| {
            !record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
        }));
        Ok(())
    }

    /// A raw `AB\nCD` block whose deleted line break is the only raw gap.
    fn deleted_break_block(id: u64) -> BlockText {
        let mut block = positioned_block(id, "ABCD", 300.0, 700.0);
        let glyph = |index: u64| GlyphId(id * 1000 + index);
        let line_break = TextSourceAtom::LineBreak {
            preceding: glyph(2),
            following: glyph(3),
        };
        let entry = |start: usize, atom: TextSourceAtom| SourceMapEntry {
            output_range: ScalarRange {
                start,
                end: start + 1,
            },
            source: TextSource {
                atoms: vec![atom].into(),
            },
        };
        block.raw = MappedText {
            text: "AB\nCD".to_owned(),
            source_map: vec![
                entry(0, TextSourceAtom::Glyph(glyph(1))),
                entry(1, TextSourceAtom::Glyph(glyph(2))),
                entry(2, line_break.clone()),
                entry(3, TextSourceAtom::Glyph(glyph(3))),
                entry(4, TextSourceAtom::Glyph(glyph(4))),
            ],
            unmapped: Vec::new(),
        };
        block.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange { start: 2, end: 3 },
            canonical_range: ScalarRange { start: 2, end: 2 },
            source: TextSource {
                atoms: vec![line_break].into(),
            },
        }];
        block
    }

    /// Old-side resolution, both adoption premises, the accepted ownership and
    /// the remaining work after one deleted-break tail pass.
    type DeletedBreakRun = (
        Vec<super::super::ResolutionRange>,
        bool,
        bool,
        Vec<SourceInterval>,
        usize,
    );

    /// Runs the tail pass over one deleted-break fragment and reports both
    /// adoption premises, the old-side accepted ownership and the remaining
    /// work after the pass.
    fn run_deleted_break_fragment(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        prepare: impl FnOnce(&mut [Ownership; 2], &mut Vec<ChangeCandidate>),
        proven: &[ProvenChangedRegion],
        budget: usize,
    ) -> Result<DeletedBreakRun> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assessor.local_domains = vec![LocalDomain {
            old_span: strict_closed_fragment_span(1, 0, 4),
            new_span: strict_closed_fragment_span(101, 0, 4),
            source_bounded: false,
        }];
        let mut ownership = [Ownership::new(), Ownership::new()];
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        prepare(&mut ownership, &mut candidates);
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        assessor.remaining_work = budget;
        assessor.recover_equal_fragments(&mut ownership, &candidates, proven)?;
        let source_positions = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
        });
        let internal_break = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
        });
        let accepted = ownership[0].accepted.clone();
        let remaining = assessor.remaining_work;
        let [old_ownership, _new_ownership] = ownership;
        let resolution = old_ownership.finish(&old, assessor.options.max_assessment_ranges)?;
        Ok((
            resolution,
            source_positions,
            internal_break,
            accepted,
            remaining,
        ))
    }

    #[test]
    fn internal_deleted_break_fallback_gains_a_previously_unowned_range() -> Result<()> {
        let old_blocks = [deleted_break_block(1)];
        let new_blocks = [deleted_break_block(101)];
        let (resolution, source_positions, internal_break, accepted, _remaining) =
            run_deleted_break_fragment(&old_blocks, &new_blocks, |_, _| {}, &[], usize::MAX)?;
        assert!(internal_break, "the fallback premise must be recorded");
        assert!(
            !source_positions,
            "the contiguous premise must not be claimed"
        );
        assert!(!accepted.is_empty(), "the fallback must commit ownership");
        assert_eq!(
            fragment_state(&resolution, 0),
            Some(super::super::ResolutionState::Equal)
        );
        assert_eq!(
            fragment_state(&resolution, 3),
            Some(super::super::ResolutionState::Equal)
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_retains_original_adoptions() -> Result<()> {
        let old_blocks = [deleted_break_block(1)];
        let new_blocks = [deleted_break_block(101)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assessor.local_domains = vec![
            LocalDomain {
                old_span: strict_closed_fragment_span(1, 0, 1),
                new_span: strict_closed_fragment_span(101, 0, 1),
                source_bounded: false,
            },
            LocalDomain {
                old_span: strict_closed_fragment_span(1, 1, 4),
                new_span: strict_closed_fragment_span(101, 1, 4),
                source_bounded: false,
            },
        ];
        let mut ownership = [Ownership::new(), Ownership::new()];
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
        let source_positions = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
        });
        let internal_break = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
        });
        assert!(
            source_positions,
            "the plain fragment must keep its original premise"
        );
        assert!(
            internal_break,
            "the deleted-break fragment must gain the fallback premise"
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_never_retries_non_raw_cut_holds() -> Result<()> {
        let old_blocks = [deleted_break_block(1)];
        let new_blocks = [deleted_break_block(101)];
        // The AB fragment ends exactly at the canonical deleted-break
        // boundary. Its selected raw run is contiguous, so the contiguous
        // proof passes the raw run check and holds on the normalization
        // boundary instead of a raw cut. The fallback must not retry it, so
        // it must spend no budget at all.
        let run = |budget: usize, tail: bool| -> Result<(bool, Vec<SourceInterval>, usize)> {
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
            let mut assessor = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            assessor.local_domains = vec![LocalDomain {
                old_span: strict_closed_fragment_span(1, 0, 2),
                new_span: strict_closed_fragment_span(101, 0, 2),
                source_bounded: false,
            }];
            let mut ownership = [Ownership::new(), Ownership::new()];
            let mut changes = Vec::new();
            let mut candidates = Vec::new();
            assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
            assert_eq!(assessor.equal_fragment_candidates.len(), 1);
            assessor.remaining_work = budget;
            if tail {
                assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
            } else {
                let limit = assessor.options.max_assessment_ranges;
                assert_eq!(
                    assessor.retry_equal_fragment(
                        &mut ownership,
                        &candidates,
                        &[],
                        limit,
                        0,
                        false
                    )?,
                    EqualFragmentRetry::Held,
                    "the fragment must hold on the normalization boundary, not a raw cut"
                );
            }
            let mut cache =
                crate::diff::assessment::equal_fragment::EqualFragmentCache::new([&old, &new]);
            let mut proof_budget = usize::MAX;
            assert_eq!(
                cache.prove(
                    [
                        &strict_closed_fragment_span(1, 0, 2),
                        &strict_closed_fragment_span(101, 0, 2),
                    ],
                    &mut proof_budget,
                )?,
                crate::diff::assessment::equal_fragment::FragmentVerdict::Held(
                    crate::diff::assessment::equal_fragment::FragmentHold::NormalizationBoundary
                ),
                "the AB fragment must hold on the normalization boundary"
            );
            let internal_break = assessor.records.iter().any(|record| {
                record
                    .assumptions
                    .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
            });
            Ok((
                internal_break,
                ownership[0].accepted.clone(),
                assessor.remaining_work,
            ))
        };
        let (_, _, first_pass_only) = run(usize::MAX, false)?;
        let (internal_break, accepted, full_tail) = run(usize::MAX, true)?;
        assert!(
            !internal_break,
            "the fallback must not adopt a non-raw-cut hold"
        );
        assert!(
            accepted.is_empty(),
            "the fallback must commit no ownership for a non-raw-cut hold"
        );
        assert_eq!(
            full_tail, first_pass_only,
            "the fallback must spend no budget on a non-raw-cut hold"
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_compacts_mixed_candidates_in_order() -> Result<()> {
        // Blocks 1 and 3 hold on a raw cut; block 2 holds on the
        // normalization boundary. The fallback must adopt only 1 and 3, in
        // recording order, and leave 2 pending.
        let old_blocks = [
            deleted_break_block(1),
            deleted_break_block(2),
            deleted_break_block(3),
        ];
        let new_blocks = [
            deleted_break_block(101),
            deleted_break_block(102),
            deleted_break_block(103),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = unresolved_alignment(
            &[BlockId(1), BlockId(2), BlockId(3)],
            &[BlockId(101), BlockId(102), BlockId(103)],
        );
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assessor.local_domains = vec![
            LocalDomain {
                old_span: strict_closed_fragment_span(1, 0, 4),
                new_span: strict_closed_fragment_span(101, 0, 4),
                source_bounded: false,
            },
            LocalDomain {
                old_span: strict_closed_fragment_span(2, 0, 2),
                new_span: strict_closed_fragment_span(102, 0, 2),
                source_bounded: false,
            },
            LocalDomain {
                old_span: strict_closed_fragment_span(3, 0, 4),
                new_span: strict_closed_fragment_span(103, 0, 4),
                source_bounded: false,
            },
        ];
        let mut ownership = [Ownership::new(), Ownership::new()];
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        assert_eq!(assessor.equal_fragment_candidates.len(), 3);
        let original = assessor.equal_fragment_candidates.clone();
        assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
        let has_internal_break = |relation: usize| {
            assessor.records[relation]
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
        };
        let relation_for_block = |block: u64| {
            original
                .iter()
                .copied()
                .find(|relation| {
                    assessor.records[*relation]
                        .old_span
                        .as_ref()
                        .is_some_and(|span| span.blocks == vec![BlockId(block)])
                })
                .expect("one candidate per block")
        };
        assert!(
            has_internal_break(relation_for_block(1)) && has_internal_break(relation_for_block(3)),
            "both raw-cut candidates must be adopted by the fallback"
        );
        assert!(
            !has_internal_break(relation_for_block(2)),
            "the normalization-boundary candidate must stay held"
        );
        assert_eq!(
            assessor.equal_fragment_candidates[..2],
            [relation_for_block(1), relation_for_block(3)],
            "compaction must keep the raw-cut candidates in recording order"
        );
        assert_eq!(
            ownership[0].accepted,
            vec![
                SourceInterval {
                    block_index: 0,
                    start: 0,
                    end: 4,
                },
                SourceInterval {
                    block_index: 2,
                    start: 0,
                    end: 4,
                },
            ],
            "the fallback must adopt the raw-cut candidates in recording order"
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_exhaustion_keeps_mixed_earlier_adoptions() -> Result<()> {
        // The plain AB fragment is adopted by the first pass; the raw-cut
        // fragment is only eligible for the fallback. An exhausted budget must
        // keep the first-pass adoption, retry nothing else and add no premise.
        let old_blocks = [
            positioned_block(1, "AB", 300.0, 700.0),
            deleted_break_block(2),
        ];
        let new_blocks = [
            positioned_block(101, "AB", 300.0, 700.0),
            deleted_break_block(102),
        ];
        let run = |budget: usize| -> Result<(bool, bool, Vec<SourceInterval>, usize)> {
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let alignment =
                unresolved_alignment(&[BlockId(1), BlockId(2)], &[BlockId(101), BlockId(102)]);
            let mut assessor = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            assessor.local_domains = vec![
                LocalDomain {
                    old_span: strict_closed_fragment_span(1, 0, 2),
                    new_span: strict_closed_fragment_span(101, 0, 2),
                    source_bounded: false,
                },
                LocalDomain {
                    old_span: strict_closed_fragment_span(2, 0, 4),
                    new_span: strict_closed_fragment_span(102, 0, 4),
                    source_bounded: false,
                },
            ];
            let mut ownership = [Ownership::new(), Ownership::new()];
            let mut changes = Vec::new();
            let mut candidates = Vec::new();
            assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
            assessor.remaining_work = budget;
            assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
            let source_positions = assessor.records.iter().any(|record| {
                record
                    .assumptions
                    .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
            });
            let internal_break = assessor.records.iter().any(|record| {
                record
                    .assumptions
                    .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
            });
            Ok((
                source_positions,
                internal_break,
                ownership[0].accepted.clone(),
                assessor.remaining_work,
            ))
        };
        let (source_positions, internal_break, accepted, remaining) = run(usize::MAX)?;
        assert!(source_positions && internal_break);
        assert_eq!(accepted.len(), 2);
        let used = usize::MAX - remaining;
        assert!(used > 1);
        let (source_positions, internal_break, accepted, _) = run(used - 1)?;
        assert!(
            source_positions,
            "the earlier first-pass adoption must remain"
        );
        assert!(
            !internal_break,
            "the exhausted fallback must add no premise"
        );
        assert_eq!(
            accepted,
            vec![SourceInterval {
                block_index: 0,
                start: 0,
                end: 2,
            }],
            "only the earlier accepted range may remain"
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_respects_latest_vetoes() -> Result<()> {
        let old_blocks = [deleted_break_block(1)];
        let new_blocks = [deleted_break_block(101)];
        // A changed-ownership conflict adopts nothing; the seeded accepted
        // range is retained exactly once.
        let seeded = SourceInterval {
            block_index: 0,
            start: 0,
            end: 4,
        };
        let (_, source_positions, internal_break, accepted, _) = run_deleted_break_fragment(
            &old_blocks,
            &new_blocks,
            |ownership, _| {
                ownership[0].accepted.push(seeded);
                ownership[0].changed.push(SourceInterval {
                    block_index: 0,
                    start: 1,
                    end: 2,
                });
            },
            &[],
            usize::MAX,
        )?;
        assert!(!source_positions && !internal_break);
        assert_eq!(accepted, vec![seeded]);
        // A candidate conflict adopts nothing.
        let (_, source_positions, internal_break, accepted, _) = run_deleted_break_fragment(
            &old_blocks,
            &new_blocks,
            |_, candidates| {
                candidates.push(ChangeCandidate {
                    change: Change::single_occurrence(
                        ChangeKind::Replacement,
                        Some(strict_closed_fragment_span(1, 0, 4)),
                        Some(strict_closed_fragment_span(101, 0, 4)),
                        Confidence::High,
                        Vec::new(),
                    ),
                    relation: 0,
                    alternative_group: 0,
                });
            },
            &[],
            usize::MAX,
        )?;
        assert!(!source_positions && !internal_break && accepted.is_empty());
        // A proven changed region adopts nothing.
        let region = ProvenChangedRegion {
            old_span: Some(strict_closed_fragment_span(1, 0, 4)),
            new_span: Some(strict_closed_fragment_span(101, 0, 4)),
            proof: ChangedRegionProof::ExactTokenMultisetMismatch,
            confidence: Confidence::High,
        };
        let (_, source_positions, internal_break, accepted, _) =
            run_deleted_break_fragment(&old_blocks, &new_blocks, |_, _| {}, &[region], usize::MAX)?;
        assert!(!source_positions && !internal_break && accepted.is_empty());
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_exhaustion_adds_nothing() -> Result<()> {
        let old_blocks = [deleted_break_block(1)];
        let new_blocks = [deleted_break_block(101)];
        let (_, _, internal_break, accepted, remaining) =
            run_deleted_break_fragment(&old_blocks, &new_blocks, |_, _| {}, &[], usize::MAX)?;
        assert!(internal_break && !accepted.is_empty());
        let used = usize::MAX - remaining;
        assert!(used > 1, "the fallback must cost more than one unit");
        let (_, source_positions, internal_break, accepted, _) =
            run_deleted_break_fragment(&old_blocks, &new_blocks, |_, _| {}, &[], used - 1)?;
        assert!(
            !source_positions,
            "the original premise must not be added on exhaustion"
        );
        assert!(
            !internal_break,
            "the fallback premise must not be added on exhaustion"
        );
        assert!(
            accepted.is_empty(),
            "no ownership may be committed on exhaustion"
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_exhaustion_keeps_earlier_adoptions() -> Result<()> {
        let old_blocks = [deleted_break_block(1)];
        let new_blocks = [deleted_break_block(101)];
        let run = |budget: usize| -> Result<(bool, bool, Vec<SourceInterval>, usize)> {
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let alignment = unresolved_alignment(&[BlockId(1)], &[BlockId(101)]);
            let mut assessor = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            assessor.local_domains = vec![
                LocalDomain {
                    old_span: strict_closed_fragment_span(1, 0, 1),
                    new_span: strict_closed_fragment_span(101, 0, 1),
                    source_bounded: false,
                },
                LocalDomain {
                    old_span: strict_closed_fragment_span(1, 1, 4),
                    new_span: strict_closed_fragment_span(101, 1, 4),
                    source_bounded: false,
                },
            ];
            let mut ownership = [Ownership::new(), Ownership::new()];
            let mut changes = Vec::new();
            let mut candidates = Vec::new();
            assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
            assessor.remaining_work = budget;
            assessor.recover_equal_fragments(&mut ownership, &candidates, &[])?;
            let source_positions = assessor.records.iter().any(|record| {
                record
                    .assumptions
                    .contains(&ComparisonAssumption::EqualFragmentSourcePositions)
            });
            let internal_break = assessor.records.iter().any(|record| {
                record
                    .assumptions
                    .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
            });
            Ok((
                source_positions,
                internal_break,
                ownership[0].accepted.clone(),
                assessor.remaining_work,
            ))
        };
        let (source_positions, internal_break, accepted, remaining) = run(usize::MAX)?;
        assert!(source_positions && internal_break);
        assert_eq!(accepted.len(), 2, "{accepted:?}");
        let used = usize::MAX - remaining;
        assert!(used > 1);
        let (source_positions, internal_break, accepted, _) = run(used - 1)?;
        assert!(
            source_positions,
            "the earlier original adoption must remain"
        );
        assert!(
            !internal_break,
            "the exhausted fallback must add no premise"
        );
        assert_eq!(
            accepted,
            vec![SourceInterval {
                block_index: 0,
                start: 0,
                end: 1,
            }],
            "the earlier accepted range must remain and the fallback must add none"
        );
        Ok(())
    }

    #[test]
    fn internal_deleted_break_fallback_malformed_control_adopts_nothing() -> Result<()> {
        let mut old = deleted_break_block(1);
        old.normalization_events[0].canonical_range = ScalarRange { start: 1, end: 1 };
        let new_blocks = [deleted_break_block(101)];
        let (_, source_positions, internal_break, accepted, _) =
            run_deleted_break_fragment(&[old], &new_blocks, |_, _| {}, &[], usize::MAX)?;
        assert!(!source_positions && !internal_break && accepted.is_empty());
        Ok(())
    }
    fn suffix_test_block(id: u64, text: &str, x: f64, y: f64) -> BlockText {
        let mut block = sourced_block(id, text);
        let count = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        let signatures = (0..count)
            .map(|index| {
                PositionSignature::new(
                    Vec2 {
                        x: x + index as f64 * 10.0,
                        y,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("valid position")
            })
            .collect();
        block.position_signatures = Some(signatures);
        block
    }

    struct SuffixOutcome {
        old_accepted: Vec<SourceInterval>,
        new_accepted: Vec<SourceInterval>,
        records: usize,
        premise: bool,
        seed_untouched: bool,
        remaining_work: usize,
    }

    fn run_suffix_case(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        options: DiffOptions,
        prepare: impl FnOnce(&mut super::super::Assessor<'_, '_>, &mut [Ownership; 2]),
    ) -> Result<SuffixOutcome> {
        run_suffix_case_full(old_blocks, new_blocks, options, 0, &[], &[], prepare)
    }

    fn run_suffix_case_full(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        options: DiffOptions,
        seed_start: usize,
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        prepare: impl FnOnce(&mut super::super::Assessor<'_, '_>, &mut [Ownership; 2]),
    ) -> Result<SuffixOutcome> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let old_ids = old_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let new_ids = new_blocks
            .iter()
            .map(|block| block.block)
            .collect::<Vec<_>>();
        let alignment = unresolved_alignment(&old_ids, &new_ids);
        let mut assessor = super::super::Assessor::new([&old, &new], &alignment, None, options)?;
        let seed_old = old_blocks[0]
            .canonical
            .comparable_tokens()
            .expect("seed old tokens")
            .len();
        let seed_new = new_blocks[0]
            .canonical
            .comparable_tokens()
            .expect("seed new tokens")
            .len();
        let anchor_old = old_blocks[1]
            .canonical
            .comparable_tokens()
            .expect("anchor old tokens")
            .len();
        let anchor_new = new_blocks[1]
            .canonical
            .comparable_tokens()
            .expect("anchor new tokens")
            .len();
        assessor.records = vec![
            crate::diff::RelationAssessment {
                old_span: Some(strict_closed_fragment_span(1, seed_start, seed_old)),
                new_span: Some(strict_closed_fragment_span(101, seed_start, seed_new)),
                parent: None,
                outcome: RelationOutcome::Established,
                search: SearchCompleteness::Complete,
                assumptions: Vec::new(),
                reasons: Vec::new(),
            },
            crate::diff::RelationAssessment {
                old_span: Some(strict_closed_fragment_span(2, 0, anchor_old)),
                new_span: Some(strict_closed_fragment_span(102, 0, anchor_new)),
                parent: None,
                outcome: RelationOutcome::Established,
                search: SearchCompleteness::Complete,
                assumptions: Vec::new(),
                reasons: Vec::new(),
            },
        ];
        let proof = |relation: usize, old_len: usize, new_len: usize| DomainProof {
            scope: super::super::ProofScope::ExactKey,
            relation,
            unique: true,
            search: SearchCompleteness::Complete,
            edits: Vec::new(),
            lengths: [old_len, new_len],
            strict_unique: true,
            stable_events: None,
        };
        assessor.domains.insert(
            DomainKey {
                local: Some((
                    strict_closed_fragment_span(1, seed_start, seed_old),
                    strict_closed_fragment_span(101, seed_start, seed_new),
                )),
                old: 0..1,
                new: 0..1,
                old_separator: BlockSeparator::Space,
                new_separator: BlockSeparator::Space,
            },
            proof(0, seed_old - seed_start, seed_new - seed_start),
        );
        assessor.domains.insert(
            DomainKey {
                local: Some((
                    strict_closed_fragment_span(2, 0, anchor_old),
                    strict_closed_fragment_span(102, 0, anchor_new),
                )),
                old: 1..2,
                new: 1..2,
                old_separator: BlockSeparator::Space,
                new_separator: BlockSeparator::Space,
            },
            proof(1, anchor_old, anchor_new),
        );
        let mut ownership = [Ownership::new(), Ownership::new()];
        ownership[0].accepted.push(SourceInterval {
            block_index: 1,
            start: 0,
            end: anchor_old,
        });
        ownership[1].accepted.push(SourceInterval {
            block_index: 1,
            start: 0,
            end: anchor_new,
        });
        prepare(&mut assessor, &mut ownership);
        let limit = assessor.options.max_assessment_ranges;
        assessor.recover_suffix_translations(&mut ownership, candidates, proven, limit)?;
        let premise = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::RigidSuffixTranslation)
        });
        let seed_untouched = assessor.records[0].assumptions.is_empty();
        Ok(SuffixOutcome {
            old_accepted: ownership[0].accepted.clone(),
            new_accepted: ownership[1].accepted.clone(),
            records: assessor.records.len(),
            premise,
            seed_untouched,
            remaining_work: assessor.remaining_work,
        })
    }

    #[test]
    fn suffix_translation_adopts_with_independent_anchor() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
        ];
        let outcome = run_suffix_case(&old_blocks, &new_blocks, DiffOptions::default(), |_, _| {})?;
        assert!(outcome.premise);
        assert!(outcome.seed_untouched);
        assert_eq!(outcome.records, 3, "one whole-suffix relation was recorded");
        assert!(
            outcome
                .old_accepted
                .iter()
                .any(|interval| interval.block_index == 0
                    && interval.start == 0
                    && interval.end == 6)
        );
        assert!(
            outcome
                .new_accepted
                .iter()
                .any(|interval| interval.block_index == 0
                    && interval.start == 0
                    && interval.end == 6)
        );
        Ok(())
    }

    #[test]
    fn suffix_translation_requires_anchor_with_same_delta() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 60.0),
        ];
        let outcome = run_suffix_case(&old_blocks, &new_blocks, DiffOptions::default(), |_, _| {})?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.old_accepted.len(), 1);
        assert_eq!(outcome.new_accepted.len(), 1);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_tentative_anchor() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
        ];
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, _| {
                assessor.records[1].outcome = RelationOutcome::Tentative;
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_owned_seed_overlap() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
        ];
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |_, ownership| {
                ownership[0].accepted.push(SourceInterval {
                    block_index: 0,
                    start: 2,
                    end: 4,
                });
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.old_accepted.len(), 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_stationary_negative_zero_delta() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 0.0),
            suffix_test_block(2, "GHIJKL", 100.0, -10.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, -0.0),
            suffix_test_block(102, "GHIJKL", 100.0, -10.0),
        ];
        let outcome = run_suffix_case(&old_blocks, &new_blocks, DiffOptions::default(), |_, _| {})?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_competing_occurrence() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
            suffix_test_block(103, "ABCDEF", 100.0, 80.0),
        ];
        let outcome = run_suffix_case(&old_blocks, &new_blocks, DiffOptions::default(), |_, _| {})?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_adopts_nothing_at_output_record_limit() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
        ];
        let options = DiffOptions {
            max_assessment_ranges: 3,
            ..DiffOptions::default()
        };
        let outcome = run_suffix_case(&old_blocks, &new_blocks, options, |_, _| {})?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2, "no output-limit placeholder was added");
        assert_eq!(outcome.old_accepted.len(), 1);
        assert_eq!(outcome.new_accepted.len(), 1);
        Ok(())
    }

    #[test]
    fn suffix_translation_adopts_nothing_without_budget() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 100.0),
            suffix_test_block(2, "GHIJKL", 100.0, 90.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
        ];
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, _| {
                assessor.remaining_work = 0;
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.old_accepted.len(), 1);
        Ok(())
    }
    fn add_owned_reference(
        assessor: &mut super::super::Assessor<'_, '_>,
        ownership: &mut [Ownership; 2],
        block_id: u64,
        index: usize,
        len: usize,
    ) {
        let relation = assessor.records.len();
        assessor.records.push(crate::diff::RelationAssessment {
            old_span: Some(strict_closed_fragment_span(block_id, 0, len)),
            new_span: Some(strict_closed_fragment_span(block_id + 100, 0, len)),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: Vec::new(),
            reasons: Vec::new(),
        });
        assessor.domains.insert(
            DomainKey {
                local: Some((
                    strict_closed_fragment_span(block_id, 0, len),
                    strict_closed_fragment_span(block_id + 100, 0, len),
                )),
                old: index..index + 1,
                new: index..index + 1,
                old_separator: BlockSeparator::Space,
                new_separator: BlockSeparator::Space,
            },
            DomainProof {
                scope: super::super::ProofScope::ExactKey,
                relation,
                unique: true,
                search: SearchCompleteness::Complete,
                edits: Vec::new(),
                lengths: [len, len],
                strict_unique: true,
                stable_events: None,
            },
        );
        ownership[0].accepted.push(SourceInterval {
            block_index: index,
            start: 0,
            end: len,
        });
        ownership[1].accepted.push(SourceInterval {
            block_index: index,
            start: 0,
            end: len,
        });
    }

    fn fragment_fixture() -> ([BlockText; 2], [BlockText; 2]) {
        (
            [
                suffix_test_block(1, "ABCDEF", 100.0, 90.0),
                suffix_test_block(2, "GHIJKL", 100.0, 80.0),
            ],
            [
                suffix_test_block(101, "ABCDEF", 100.0, 80.0),
                suffix_test_block(102, "GHIJKL", 100.0, 70.0),
            ],
        )
    }

    #[test]
    fn suffix_translation_rejects_same_block_prefix_obstacle() -> Result<()> {
        let (mut old_blocks, new_blocks) = fragment_fixture();
        old_blocks[0] = suffix_test_block(1, "ZBCDEF", 100.0, 90.0);
        old_blocks[0]
            .position_signatures
            .as_mut()
            .expect("geometry")[0] =
            PositionSignature::new(Vec2 { x: 130.0, y: 85.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let mut new_seed = suffix_test_block(101, "QBCDEF", 100.0, 80.0);
        new_seed.position_signatures.as_mut().expect("geometry")[0] =
            PositionSignature::new(Vec2 { x: 130.0, y: 80.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let new_blocks = [new_seed, new_blocks[1].clone()];
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            1,
            &[],
            &[],
            |_, _| {},
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_adopts_fragment_seed_with_clear_prefix() -> Result<()> {
        let (mut old_blocks, new_blocks) = fragment_fixture();
        old_blocks[0] = suffix_test_block(1, "ZBCDEF", 100.0, 90.0);
        old_blocks[0]
            .position_signatures
            .as_mut()
            .expect("geometry")[0] =
            PositionSignature::new(Vec2 { x: 130.0, y: 70.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let mut new_seed = suffix_test_block(101, "QBCDEF", 100.0, 80.0);
        new_seed.position_signatures.as_mut().expect("geometry")[0] =
            PositionSignature::new(Vec2 { x: 130.0, y: 60.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let new_blocks = [new_seed, new_blocks[1].clone()];
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            1,
            &[],
            &[],
            |_, _| {},
        )?;
        assert!(outcome.premise);
        assert!(
            outcome
                .old_accepted
                .iter()
                .any(|interval| interval.block_index == 0
                    && interval.start == 1
                    && interval.end == 6)
        );
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_accepted_overlap_only_in_expanded_suffix() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            1,
            &[],
            &[],
            |_, ownership| {
                ownership[0].accepted.push(SourceInterval {
                    block_index: 0,
                    start: 0,
                    end: 1,
                });
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.old_accepted.len(), 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_adopts_when_expanded_suffix_is_clear() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            1,
            &[],
            &[],
            |_, _| {},
        )?;
        assert!(outcome.premise);
        assert!(
            outcome
                .old_accepted
                .iter()
                .any(|interval| interval.block_index == 0
                    && interval.start == 0
                    && interval.end == 6)
        );
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_crossing_reference() -> Result<()> {
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 90.0),
            suffix_test_block(2, "GHIJKL", 100.0, 80.0),
            suffix_test_block(3, "MNOPQR", 500.0, 95.0),
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
            suffix_test_block(103, "MNOPQR", 500.0, 60.0),
        ];
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, ownership| {
                add_owned_reference(assessor, ownership, 3, 2, 6);
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 3);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_reference_without_geometry() -> Result<()> {
        let mut reference = suffix_test_block(3, "MNOPQR", 500.0, 95.0);
        reference.position_signatures = None;
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 90.0),
            suffix_test_block(2, "GHIJKL", 100.0, 80.0),
            reference,
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
            suffix_test_block(103, "MNOPQR", 500.0, 60.0),
        ];
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, ownership| {
                add_owned_reference(assessor, ownership, 3, 2, 6);
            },
        )?;
        assert!(!outcome.premise);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_two_token_obstacle_straddling_band() -> Result<()> {
        let mut obstacle = suffix_test_block(3, "MN", 80.0, 85.0);
        obstacle.position_signatures.as_mut().expect("geometry")[1] =
            PositionSignature::new(Vec2 { x: 170.0, y: 85.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let old_blocks = [
            suffix_test_block(1, "ABCDEF", 100.0, 90.0),
            suffix_test_block(2, "GHIJKL", 100.0, 80.0),
            obstacle,
        ];
        let new_blocks = [
            suffix_test_block(101, "ABCDEF", 100.0, 80.0),
            suffix_test_block(102, "GHIJKL", 100.0, 70.0),
            suffix_test_block(103, "MN", 80.0, 60.0),
        ];
        let outcome = run_suffix_case(&old_blocks, &new_blocks, DiffOptions::default(), |_, _| {})?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_changed_overlap() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |_, ownership| {
                ownership[0].changed.push(SourceInterval {
                    block_index: 0,
                    start: 2,
                    end: 4,
                });
            },
        )?;
        assert!(!outcome.premise);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_candidate_overlap() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let candidate = ChangeCandidate {
            change: crate::diff::Change::single_occurrence(
                crate::diff::ChangeKind::Replacement,
                Some(strict_closed_fragment_span(1, 2, 4)),
                Some(strict_closed_fragment_span(101, 2, 4)),
                Confidence::High,
                Vec::new(),
            ),
            relation: 0,
            alternative_group: 0,
        };
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            0,
            &[candidate],
            &[],
            |_, _| {},
        )?;
        assert!(!outcome.premise);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_proven_overlap() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let proven = [ProvenChangedRegion {
            old_span: Some(strict_closed_fragment_span(1, 2, 4)),
            new_span: None,
            proof: ChangedRegionProof::OneSidedNonEmptyRange,
            confidence: Confidence::High,
        }];
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            0,
            &[],
            &proven,
            |_, _| {},
        )?;
        assert!(!outcome.premise);
        Ok(())
    }

    #[test]
    fn suffix_translation_stops_on_seed_copy_exhaustion() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, _| {
                assessor.remaining_work = 3;
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.remaining_work, 0);
        Ok(())
    }

    #[test]
    fn suffix_translation_stops_in_anchor_or_veto_scan_exhaustion() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, _| {
                assessor.remaining_work = 25;
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.old_accepted.len(), 1);
        assert_eq!(outcome.remaining_work, 0);
        Ok(())
    }

    #[test]
    fn suffix_translation_stops_mid_proof_on_exhaustion() -> Result<()> {
        let (old_blocks, new_blocks) = fragment_fixture();
        let outcome = run_suffix_case(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            |assessor, _| {
                assessor.remaining_work = 60;
            },
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.old_accepted.len(), 1);
        assert_eq!(
            outcome.remaining_work, 0,
            "the mid-proof run consumed the budget"
        );
        Ok(())
    }
    #[test]
    fn suffix_translation_rejects_prefix_touching_gap_boundary() -> Result<()> {
        let (mut old_blocks, new_blocks) = fragment_fixture();
        old_blocks[0] = suffix_test_block(1, "ZBCDEF", 100.0, 90.0);
        old_blocks[0]
            .position_signatures
            .as_mut()
            .expect("geometry")[0] =
            PositionSignature::new(Vec2 { x: 130.0, y: 90.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let mut new_seed = suffix_test_block(101, "QBCDEF", 100.0, 80.0);
        new_seed.position_signatures.as_mut().expect("geometry")[0] =
            PositionSignature::new(Vec2 { x: 130.0, y: 80.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("valid position");
        let new_blocks = [new_seed, new_blocks[1].clone()];
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            1,
            &[],
            &[],
            |_, _| {},
        )?;
        assert!(
            !outcome.premise,
            "boundary contact must veto conservatively"
        );
        assert_eq!(outcome.records, 2);
        Ok(())
    }

    #[test]
    fn suffix_translation_rejects_prefix_straddling_gap() -> Result<()> {
        let mut old_seed = suffix_test_block(1, "ZZBCDEF", 100.0, 90.0);
        let signatures = old_seed.position_signatures.as_mut().expect("geometry");
        signatures[0] = PositionSignature::new(Vec2 { x: 130.0, y: 85.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position");
        signatures[1] = PositionSignature::new(Vec2 { x: 140.0, y: 95.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position");
        let old_blocks = [old_seed, suffix_test_block(2, "GHIJKL", 100.0, 80.0)];
        let mut new_seed = suffix_test_block(101, "QQBCDEF", 100.0, 80.0);
        let signatures = new_seed.position_signatures.as_mut().expect("geometry");
        signatures[0] = PositionSignature::new(Vec2 { x: 130.0, y: 60.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position");
        signatures[1] = PositionSignature::new(Vec2 { x: 140.0, y: 60.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position");
        let new_blocks = [new_seed, suffix_test_block(102, "GHIJKL", 100.0, 70.0)];
        let outcome = run_suffix_case_full(
            &old_blocks,
            &new_blocks,
            DiffOptions::default(),
            2,
            &[],
            &[],
            |_, _| {},
        )?;
        assert!(!outcome.premise);
        assert_eq!(outcome.records, 2);
        Ok(())
    }
    /// Four source components under one independently proved 32-token parent.
    /// The head and final suffix remain outside the claim.
    fn coupled_blocks(offset: u64) -> Vec<BlockText> {
        [
            "outer ",
            "Part",
            "III",
            "Cost of",
            "Goods Sold........ tail",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, text)| sourced_block(offset + index as u64 + 1, text))
        .collect()
    }

    fn coupled_span(blocks: &[BlockText], separator: BlockSeparator) -> TextSpan {
        TextSpan {
            blocks: blocks.iter().take(5).map(|block| block.block).collect(),
            separator: Some(separator),
            canonical_range: ScalarRange { start: 6, end: 38 },
            comparable_range: super::super::super::TokenRange { start: 6, end: 38 },
        }
    }

    struct CoupledRun {
        accepted: [Vec<SourceInterval>; 2],
        adopted: bool,
        remaining: usize,
        used: usize,
        regions: Vec<super::super::UnresolvedRegion>,
    }

    fn run_coupled_parent(
        blocks: [&[BlockText]; 2],
        budget: usize,
        prepare: impl FnOnce(
            &mut super::super::Assessor<'_, '_>,
            &mut [Ownership; 2],
            &mut Vec<ChangeCandidate>,
            &mut Vec<ProvenChangedRegion>,
            &mut Vec<super::super::UnresolvedRegion>,
        ),
    ) -> Result<CoupledRun> {
        let old = side(blocks[0]);
        let new = side(blocks[1]);
        let alignment = unresolved_alignment(
            &blocks[0]
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            &blocks[1]
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
        );
        let mut assessor =
            super::super::Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assessor.local_domains.push(LocalDomain {
            old_span: coupled_span(blocks[0], BlockSeparator::Concatenate),
            new_span: coupled_span(blocks[1], BlockSeparator::Concatenate),
            source_bounded: false,
        });
        let mut ownership = [Ownership::new(), Ownership::new()];
        let mut changes = Vec::new();
        let mut candidates = Vec::new();
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
        assert!(assessor.has_coupled_equal_candidate);
        for (owner, side) in ownership.iter_mut().zip([&old, &new]) {
            owner.accepted.push(SourceInterval {
                block_index: 4,
                start: 18,
                end: side.canonical[4].len(),
            });
        }
        let mut old_partition =
            ownership[0].resolution(&old, assessor.options.max_assessment_ranges)?;
        let mut new_partition =
            ownership[1].resolution(&new, assessor.options.max_assessment_ranges)?;
        let initial = [ownership[0].accepted.len(), ownership[1].accepted.len()];
        let mut regions = Vec::new();
        for (side_index, parts) in [&old_partition, &new_partition].into_iter().enumerate() {
            for part in parts
                .iter()
                .filter(|part| part.state == super::super::ResolutionState::Unresolved)
            {
                let span = TextSpan {
                    blocks: vec![part.block],
                    separator: None,
                    canonical_range: part.canonical_range,
                    comparable_range: part.comparable_range,
                };
                regions.push(super::super::UnresolvedRegion {
                    old_span: (side_index == 0).then(|| span.clone()),
                    new_span: (side_index == 1).then(|| span.clone()),
                    evidence: Vec::new(),
                });
            }
        }
        let mut proven = Vec::new();
        prepare(
            &mut assessor,
            &mut ownership,
            &mut candidates,
            &mut proven,
            &mut regions,
        );
        let original_regions = regions.clone();
        let original_partitions = [old_partition.clone(), new_partition.clone()];
        assessor.remaining_work = budget;
        assessor.recover_equal_fragments(&mut ownership, &candidates, &proven)?;
        let domains = std::mem::take(&mut assessor.domains)
            .into_iter()
            .collect::<Vec<_>>();
        assert!(assessor.domains.is_empty(), "review moved the proof cache");
        assessor.recover_coupled_equal_parents(
            &mut ownership,
            &candidates,
            &proven,
            CoupledPublication {
                domains: &domains,
                partitions: [&mut old_partition, &mut new_partition],
                unresolved: &mut regions,
            },
            assessor.options.max_assessment_ranges,
        )?;
        let adopted = assessor.records.iter().any(|record| {
            record
                .assumptions
                .contains(&ComparisonAssumption::EqualFragmentCoupledSourcePositions)
        });
        if !adopted {
            assert_eq!(regions, original_regions);
            assert_eq!([old_partition, new_partition], original_partitions);
        }
        let remaining = assessor.remaining_work;
        let [old_owner, new_owner] = ownership;
        Ok(CoupledRun {
            accepted: [
                old_owner.accepted[initial[0]..].to_vec(),
                new_owner.accepted[initial[1]..].to_vec(),
            ],
            adopted,
            remaining,
            used: budget - remaining,
            regions,
        })
    }

    #[test]
    fn context_projection_warm_receipts_keep_current_coverage_and_source_binding() -> Result<()> {
        use super::super::context_projection::{ContextProjections, CoupledContext};
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        run_coupled_parent(
            [&blocks[0], &blocks[1]],
            1_000_000,
            |a, _, _, _, regions| {
                let mut complete = [Vec::new(), Vec::new()];
                let region = &regions[0];
                for (index, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        complete[index] =
                            project(a.sides[index], span).expect("original full projection");
                    }
                }
                let mut partial = complete.clone();
                let first = partial
                    .iter_mut()
                    .flat_map(|side| side.iter_mut())
                    .next()
                    .expect("real source interval");
                first.start = first.start.saturating_add(1).min(first.end);
                let mut counter = 1_000_000;
                let reference_complete = a
                    .coupled_region_removals(regions, &complete, 1000)
                    .expect("original current coverage");
                let reference_partial = a
                    .coupled_region_removals(regions, &partial, 1000)
                    .expect("original partial coverage");
                let mut cache =
                    ContextProjections::new(regions, a.sides, 64 * 1024 * 1024, &mut counter)
                        .expect("cache allocation")
                        .expect("cache budget");
                assert!(
                    cache
                        .removals(a.sides, &[Vec::new(), Vec::new()], 1000, &mut counter)
                        .expect("warm full context")
                        .is_some()
                );
                assert_eq!(
                    cache
                        .removals(a.sides, &complete, 1000, &mut counter)
                        .expect("current complete coverage"),
                    reference_complete
                );
                assert_eq!(
                    cache
                        .removals(a.sides, &partial, 1000, &mut counter)
                        .expect("current partial veto"),
                    reference_partial
                );
                let impostor = side(&blocks[0]);
                let bytes = cache.retained_bytes();
                assert!(
                    cache
                        .removals([&impostor, a.sides[1]], &complete, 1000, &mut counter)
                        .expect("different immutable owner")
                        .is_none()
                );
                assert_eq!(cache.retained_bytes(), bytes);
            },
        )?;
        Ok(())
    }

    #[test]
    fn context_projection_removal_moves_receipts_with_their_original_spans() -> Result<()> {
        use super::super::context_projection::{ContextProjections, CoupledContext};
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        run_coupled_parent(
            [&blocks[0], &blocks[1]],
            1_000_000,
            |a, _, _, _, regions| {
                let expected = regions[1..].to_vec();
                let mut counter = 1_000_000;
                let mut cache =
                    ContextProjections::new(regions, a.sides, 64 * 1024 * 1024, &mut counter)
                        .expect("cache allocation")
                        .expect("cache budget");
                assert!(
                    cache
                        .removals(a.sides, &[Vec::new(), Vec::new()], 1000, &mut counter)
                        .expect("original receipt population")
                        .is_some()
                );
                let mut removed = vec![false; cache.len()];
                removed[0] = true;
                assert!(cache.prepay_retain(&mut counter));
                cache.retain_removed(&removed);
                assert_eq!(cache.regions(), expected);
                let mut accepted = [Vec::new(), Vec::new()];
                for (index, span) in [
                    cache.regions()[0].old_span.as_ref(),
                    cache.regions()[0].new_span.as_ref(),
                ]
                .into_iter()
                .enumerate()
                {
                    if let Some(span) = span {
                        accepted[index] = project(a.sides[index], span)
                            .expect("current projection after removal");
                    }
                }
                let original = a
                    .coupled_region_removals(cache.regions(), &accepted, 1000)
                    .expect("original predicate after removal");
                assert_eq!(
                    cache
                        .removals(a.sides, &accepted, 1000, &mut counter)
                        .expect("same current span associations"),
                    original
                );
            },
        )?;
        Ok(())
    }

    #[test]
    fn context_projection_every_insufficient_warm_budget_keeps_diagnostics_unchanged() -> Result<()>
    {
        use super::super::context_projection::{ContextProjections, CoupledContext};
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        run_coupled_parent(
            [&blocks[0], &blocks[1]],
            1_000_000,
            |a, _, _, _, regions| {
                let original = regions.clone();
                let mut counter = 1_000_000;
                let mut cache =
                    ContextProjections::new(regions, a.sides, 64 * 1024 * 1024, &mut counter)
                        .expect("cache allocation")
                        .expect("cache budget");
                let empty = [Vec::new(), Vec::new()];
                assert!(
                    cache
                        .removals(a.sides, &empty, 1000, &mut counter)
                        .expect("populate completed receipts")
                        .is_some()
                );
                let mut measured = 1_000_000;
                assert!(
                    cache
                        .removals(a.sides, &empty, 1000, &mut measured)
                        .expect("complete warm query")
                        .is_some()
                );
                let work = 1_000_000 - measured;
                assert!(work > 0);
                for budget in 0..work {
                    assert!(
                        cache
                            .removals(a.sides, &empty, 1000, &mut { budget })
                            .expect("bounded warm query")
                            .is_none()
                    );
                    assert_eq!(cache.regions(), original);
                }
                assert!(
                    cache
                        .removals(a.sides, &empty, 1000, &mut { work })
                        .expect("exact budget")
                        .is_some()
                );
            },
        )?;
        Ok(())
    }

    #[test]
    fn context_projection_capacity_and_evidence_vetoes_survive_retention() -> Result<()> {
        use super::super::context_projection::{ContextProjections, CoupledContext};
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        run_coupled_parent(
            [&blocks[0], &blocks[1]],
            1_000_000,
            |a, _, _, _, regions| {
                let mut counter = 1_000_000;
                assert!(
                    ContextProjections::new(regions, a.sides, 0, &mut counter)
                        .expect("zero capacity refusal")
                        .is_none()
                );
                assert_eq!(counter, 1_000_000);
                let mut accepted = [Vec::new(), Vec::new()];
                for (index, span) in [regions[0].old_span.as_ref(), regions[0].new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        accepted[index] =
                            project(a.sides[index], span).expect("original covered interval");
                    }
                }
                regions[0]
                    .evidence
                    .push(crate::alignment::AlignmentEvidence::ExtractionGap);
                let expected = a
                    .coupled_region_removals(regions, &accepted, 1000)
                    .expect("original evidence veto");
                assert!(expected.is_none());
                let mut cache =
                    ContextProjections::new(regions, a.sides, 64 * 1024 * 1024, &mut counter)
                        .expect("cache allocation")
                        .expect("cache budget");
                assert!(
                    cache
                        .removals(a.sides, &[Vec::new(), Vec::new()], 1000, &mut counter)
                        .expect("warm non-overlapping context")
                        .is_some()
                );
                assert_eq!(
                    cache
                        .removals(a.sides, &accepted, 1000, &mut counter)
                        .expect("current extraction gap veto"),
                    expected
                );
                assert!(!cache.limit_retained_bytes(0));
            },
        )?;
        Ok(())
    }

    #[test]
    fn coupled_parent_adopts_exactly_its_complete_source_projection() -> Result<()> {
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        let run = run_coupled_parent([&blocks[0], &blocks[1]], 1_000_000, |_, _, _, _, _| {})?;
        assert!(run.adopted);
        let expected = [
            SourceInterval {
                block_index: 1,
                start: 0,
                end: 4,
            },
            SourceInterval {
                block_index: 2,
                start: 0,
                end: 3,
            },
            SourceInterval {
                block_index: 3,
                start: 0,
                end: 7,
            },
            SourceInterval {
                block_index: 4,
                start: 0,
                end: 18,
            },
        ];
        assert_eq!(run.accepted, [expected.to_vec(), expected.to_vec()]);
        assert!(run.used > 0 && run.remaining > 0);
        assert_eq!(run.regions.len(), 2);
        assert_eq!(
            run.regions[0]
                .old_span
                .as_ref()
                .expect("outer old region")
                .blocks,
            vec![BlockId(1)]
        );
        assert_eq!(
            run.regions[1]
                .new_span
                .as_ref()
                .expect("outer new region")
                .blocks,
            vec![BlockId(101)]
        );
        Ok(())
    }

    #[test]
    fn coupled_parent_never_partially_adopts_on_any_insufficient_budget() -> Result<()> {
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        let full = run_coupled_parent([&blocks[0], &blocks[1]], 1_000_000, |_, _, _, _, _| {})?;
        assert!(full.adopted);
        for budget in 0..full.used {
            let run = run_coupled_parent([&blocks[0], &blocks[1]], budget, |_, _, _, _, _| {})?;
            assert!(!run.adopted, "budget {budget} unexpectedly adopted");
            assert!(
                run.accepted.iter().all(Vec::is_empty),
                "budget {budget} partially adopted"
            );
            assert_eq!(run.remaining, 0, "budget {budget} was not fully charged");
        }
        let exact = run_coupled_parent([&blocks[0], &blocks[1]], full.used, |_, _, _, _, _| {})?;
        assert!(exact.adopted);
        assert_eq!(exact.remaining, 0);
        assert_eq!(exact.accepted, full.accepted);
        Ok(())
    }

    #[test]
    fn coupled_projection_filter_matches_the_full_source_guard() -> Result<()> {
        for control in 0..4 {
            let mut blocks = [coupled_blocks(0), coupled_blocks(100)];
            if control == 2 {
                blocks[1][1] = sourced_block(102, "Par");
                blocks[1][2] = sourced_block(103, "tIII");
            }
            let sides = [side(&blocks[0]), side(&blocks[1])];
            let alignment = unresolved_alignment(
                &blocks[0].iter().map(|b| b.block).collect::<Vec<_>>(),
                &blocks[1].iter().map(|b| b.block).collect::<Vec<_>>(),
            );
            let mut a = super::super::Assessor::new(
                [&sides[0], &sides[1]],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            let separator = if control == 0 {
                BlockSeparator::Space
            } else {
                BlockSeparator::Concatenate
            };
            let mut spans = [
                coupled_span(&blocks[0], separator),
                coupled_span(&blocks[1], separator),
            ];
            if control == 1 {
                for span in &mut spans {
                    span.blocks.swap(1, 2);
                }
            }
            a.local_domains.push(LocalDomain {
                old_span: spans[0].clone(),
                new_span: spans[1].clone(),
                source_bounded: false,
            });
            a.recover_local(
                &mut [Ownership::new(), Ownership::new()],
                &mut Vec::new(),
                &mut Vec::new(),
            )?;
            let child = a.equal_fragment_candidates[0];
            let parent = a.records[child].parent.expect("complete parent");
            let proof = a
                .domains
                .values()
                .find(|proof| proof.relation == parent)
                .expect("retained proof");
            let (old, new) = a.copy_recorded_spans(child)?.expect("source spans");
            let spans = [&old, &new];
            let projected = [
                project(a.sides[0], spans[0])?,
                project(a.sides[1], spans[1])?,
            ];
            let parts = [&projected[0][..], &projected[1][..]];
            let mut budget = 1_000_000;
            let possible = coupled_projection_covers_parent(parts, spans, &mut budget);
            assert_eq!(possible, Some(control == 3));
            let used = 1_000_000 - budget;
            for mut cut in [0, used / 2, used - 1] {
                assert_eq!(
                    coupled_projection_covers_parent(parts, spans, &mut cut),
                    None
                );
            }
            let mut exact = used;
            assert_eq!(
                coupled_projection_covers_parent(parts, spans, &mut exact),
                possible
            );
            assert_eq!(exact, 0);
            let mut full_budget = 1_000_000;
            let full = super::super::equal_fragment::EqualFragmentCache::new(a.sides)
                .prove_coupled_positioned_parent(
                    spans,
                    &a.records[parent],
                    proof,
                    &mut full_budget,
                )?;
            assert_eq!(
                full,
                if control == 3 {
                    super::super::equal_fragment::FragmentVerdict::Proven
                } else {
                    super::super::equal_fragment::FragmentVerdict::Held(
                        super::super::equal_fragment::FragmentHold::Projection,
                    )
                }
            );
            assert!(used < 1_000_000 - full_budget);
        }
        Ok(())
    }

    #[test]
    fn coupled_parent_rejects_missing_strict_premise_before_candidate_projection() -> Result<()> {
        let mut blocks = [coupled_blocks(0), coupled_blocks(100)];
        for (side_index, side_blocks) in blocks.iter_mut().enumerate() {
            for index in 0..256 {
                side_blocks.push(sourced_block(1000 + side_index as u64 * 256 + index, "x"));
            }
        }
        let run = |with_candidates| {
            run_coupled_parent(
                [&blocks[0], &blocks[1]],
                1_000_000,
                |a, _, candidates, _, _| {
                    // Isolate this retry from the earlier contiguous proof,
                    // which uses a different correspondence premise.
                    a.equal_fragment_candidates.clear();
                    a.domains
                        .values_mut()
                        .for_each(|proof| proof.strict_unique = false);
                    if with_candidates {
                        for block in &blocks[0][5..] {
                            candidates.push(ChangeCandidate {
                                change: Change::single_occurrence(
                                    ChangeKind::Deletion,
                                    Some(strict_closed_fragment_span(block.block.0, 0, 1)),
                                    None,
                                    Confidence::High,
                                    Vec::new(),
                                ),
                                relation: 0,
                                alternative_group: 0,
                            });
                        }
                    }
                },
            )
        };
        let bare = run(false)?;
        let unrelated = run(true)?;
        assert!(!bare.adopted && !unrelated.adopted);
        assert!(bare.accepted.iter().all(Vec::is_empty));
        assert!(unrelated.accepted.iter().all(Vec::is_empty));
        assert_eq!(bare.regions, unrelated.regions);
        assert_eq!(bare.used, unrelated.used, "irrelevant candidate work");
        Ok(())
    }

    #[test]
    fn coupled_parent_requires_the_actual_complete_strict_equal_parent() -> Result<()> {
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        for control in 0..13 {
            let run = run_coupled_parent([&blocks[0], &blocks[1]], 1_000_000, |a, _, _, _, _| {
                let child = a.equal_fragment_candidates[0];
                let parent = a.records[child].parent.expect("recorded closed parent");
                match control {
                    0 => a.records[child].parent = None,
                    1 => a.records[parent].outcome = RelationOutcome::Tentative,
                    2 => a.records[parent].search = SearchCompleteness::Incomplete,
                    3 => {
                        a.records[parent]
                            .old_span
                            .as_mut()
                            .expect("old parent span")
                            .comparable_range
                            .end -= 1;
                    }
                    4 => a
                        .domains
                        .values_mut()
                        .for_each(|proof| proof.strict_unique = false),
                    5 => a
                        .domains
                        .values_mut()
                        .for_each(|proof| proof.unique = false),
                    6 => a
                        .domains
                        .values_mut()
                        .for_each(|proof| proof.search = SearchCompleteness::Incomplete),
                    7 => a
                        .domains
                        .values_mut()
                        .for_each(|proof| proof.scope = super::super::ProofScope::BroadRootRefusal),
                    8 => a
                        .domains
                        .values_mut()
                        .for_each(|proof| proof.lengths[1] -= 1),
                    9 => {
                        a.domains.clear();
                    }
                    10 => a.domains.values_mut().for_each(|proof| {
                        proof.edits.push(crate::diff::AtomicEdit {
                            old: 0..1,
                            new: 0..0,
                        });
                    }),
                    11 => a.records[child].search = SearchCompleteness::Incomplete,
                    12 => a.records[parent]
                        .reasons
                        .push(super::super::AssessmentReason::DomainNotClosed),
                    _ => unreachable!(),
                }
            })?;
            assert!(!run.adopted, "parent control {control}");
            assert!(
                run.accepted.iter().all(Vec::is_empty),
                "parent control {control}"
            );
        }
        Ok(())
    }

    #[test]
    fn coupled_parent_preserves_all_latest_ownership_vetoes() -> Result<()> {
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        for side_index in 0..2 {
            for veto in 0..4 {
                let run = run_coupled_parent(
                    [&blocks[0], &blocks[1]],
                    1_000_000,
                    |a, owners, candidates, proven, _| {
                        let overlap =
                            strict_closed_fragment_span(blocks[side_index][4].block.0, 17, 19);
                        match veto {
                            0 => owners[side_index].changed.push(SourceInterval {
                                block_index: 4,
                                start: 17,
                                end: 19,
                            }),
                            1 => candidates.push(ChangeCandidate {
                                change: Change::single_occurrence(
                                    if side_index == 0 {
                                        ChangeKind::Deletion
                                    } else {
                                        ChangeKind::Insertion
                                    },
                                    (side_index == 0).then(|| overlap.clone()),
                                    (side_index == 1).then(|| overlap.clone()),
                                    Confidence::High,
                                    Vec::new(),
                                ),
                                relation: 0,
                                alternative_group: 0,
                            }),
                            2 => proven.push(ProvenChangedRegion {
                                old_span: (side_index == 0).then(|| overlap.clone()),
                                new_span: (side_index == 1).then(|| overlap.clone()),
                                confidence: Confidence::High,
                                proof: ChangedRegionProof::OneSidedNonEmptyRange,
                            }),
                            3 => a.options.max_assessment_ranges = 3,
                            _ => unreachable!(),
                        }
                    },
                )?;
                assert!(!run.adopted, "side {side_index}, veto {veto}");
                assert!(run.accepted.iter().all(Vec::is_empty));
            }
        }
        Ok(())
    }

    #[test]
    fn coupled_parent_holds_document_wide_shared_glyphs_and_late_component_failure() -> Result<()> {
        for side_index in 0..2 {
            for duplicate_index in 1..5 {
                let mut blocks = [coupled_blocks(0), coupled_blocks(100)];
                let mut duplicate = blocks[side_index][duplicate_index].clone();
                duplicate.block = BlockId(900);
                blocks[side_index].push(duplicate);
                let run =
                    run_coupled_parent([&blocks[0], &blocks[1]], 1_000_000, |_, _, _, _, _| {})?;
                assert!(
                    !run.adopted,
                    "shared side {side_index}, component {duplicate_index}"
                );
                assert!(run.accepted.iter().all(Vec::is_empty));
            }
        }
        let mut blocks = [coupled_blocks(0), coupled_blocks(100)];
        blocks[1][4].position_signatures = None;
        let run = run_coupled_parent([&blocks[0], &blocks[1]], 1_000_000, |_, _, _, _, _| {})?;
        assert!(!run.adopted);
        assert!(run.accepted.iter().all(Vec::is_empty));
        Ok(())
    }
    #[test]
    fn coupled_parent_rejects_synthetic_gaps_reordered_sources_and_unequal_partitions() -> Result<()>
    {
        for control in 0..3 {
            let mut blocks = [coupled_blocks(0), coupled_blocks(100)];
            if control == 2 {
                blocks[1][1] = sourced_block(102, "Par");
                blocks[1][2] = sourced_block(103, "tIII");
            }
            let old = side(&blocks[0]);
            let new = side(&blocks[1]);
            let alignment = unresolved_alignment(
                &blocks[0].iter().map(|b| b.block).collect::<Vec<_>>(),
                &blocks[1].iter().map(|b| b.block).collect::<Vec<_>>(),
            );
            let mut a = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            let separator = if control == 0 {
                BlockSeparator::Space
            } else {
                BlockSeparator::Concatenate
            };
            let mut spans = [
                coupled_span(&blocks[0], separator),
                coupled_span(&blocks[1], separator),
            ];
            if control == 1 {
                for span in &mut spans {
                    span.blocks.swap(1, 2);
                }
            }
            a.local_domains.push(LocalDomain {
                old_span: spans[0].clone(),
                new_span: spans[1].clone(),
                source_bounded: false,
            });
            let mut owners = [Ownership::new(), Ownership::new()];
            a.recover_local(&mut owners, &mut Vec::new(), &mut Vec::new())?;
            assert!(
                a.has_coupled_equal_candidate,
                "control {control} must reach the independent source veto"
            );
            a.recover_equal_fragments(&mut owners, &[], &[])?;
            assert!(
                owners.iter().all(|owner| owner.accepted.is_empty()),
                "control {control}"
            );
            assert!(a.records.iter().all(|r| {
                !r.assumptions
                    .contains(&ComparisonAssumption::EqualFragmentCoupledSourcePositions)
            }));
        }
        Ok(())
    }

    #[test]
    fn coupled_parent_runs_after_and_preserves_the_original_raw_cut_fallback() -> Result<()> {
        let mut blocks = [coupled_blocks(0), coupled_blocks(100)];
        blocks[0].push(deleted_break_block(6));
        blocks[1].push(deleted_break_block(106));
        let old = side(&blocks[0]);
        let new = side(&blocks[1]);
        let alignment = unresolved_alignment(
            &blocks[0].iter().map(|b| b.block).collect::<Vec<_>>(),
            &blocks[1].iter().map(|b| b.block).collect::<Vec<_>>(),
        );
        let run = |coupled: bool, budget: usize| -> Result<(Vec<SourceInterval>, bool, usize)> {
            let mut a = super::super::Assessor::new(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
            )?;
            a.local_domains.extend([
                LocalDomain {
                    old_span: coupled_span(&blocks[0], BlockSeparator::Concatenate),
                    new_span: coupled_span(&blocks[1], BlockSeparator::Concatenate),
                    source_bounded: false,
                },
                LocalDomain {
                    old_span: strict_closed_fragment_span(6, 0, 4),
                    new_span: strict_closed_fragment_span(106, 0, 4),
                    source_bounded: false,
                },
            ]);
            let mut owners = [Ownership::new(), Ownership::new()];
            a.recover_local(&mut owners, &mut Vec::new(), &mut Vec::new())?;
            assert_eq!(a.equal_fragment_candidates.len(), 2);
            // Put the coupled claim before the raw-cut candidate to exercise
            // loss of its original queue slot during the unchanged compaction.
            let records = &a.records;
            a.equal_fragment_candidates.sort_by_key(|&relation| {
                std::cmp::Reverse(
                    records[relation]
                        .old_span
                        .as_ref()
                        .expect("candidate span")
                        .blocks
                        .len(),
                )
            });
            let recorded_fallback = a.equal_fragment_candidates[1];
            for (owner, side) in owners.iter_mut().zip([&old, &new]) {
                owner.accepted.push(SourceInterval {
                    block_index: 4,
                    start: 18,
                    end: side.canonical[4].len(),
                });
            }
            a.has_coupled_equal_candidate = coupled;
            a.remaining_work = budget;
            a.recover_equal_fragments(&mut owners, &[], &[])?;
            let mut parts = [
                owners[0].resolution(&old, a.options.max_assessment_ranges)?,
                owners[1].resolution(&new, a.options.max_assessment_ranges)?,
            ];
            let [old_parts, new_parts] = &mut parts;
            let domains = std::mem::take(&mut a.domains)
                .into_iter()
                .collect::<Vec<_>>();
            a.recover_coupled_equal_parents(
                &mut owners,
                &[],
                &[],
                CoupledPublication {
                    domains: &domains,
                    partitions: [old_parts, new_parts],
                    unresolved: &mut Vec::new(),
                },
                a.options.max_assessment_ranges,
            )?;
            assert!(a.records.iter().any(|r| {
                r.assumptions
                    .contains(&ComparisonAssumption::EqualFragmentInternalDeletedBreak)
            }));
            assert_eq!(
                a.equal_fragment_candidates[0], recorded_fallback,
                "the original fallback compaction remains unchanged"
            );
            let adopted = a.records.iter().any(|r| {
                r.assumptions
                    .contains(&ComparisonAssumption::EqualFragmentCoupledSourcePositions)
            });
            Ok((
                owners[0].accepted[1..].to_vec(),
                adopted,
                budget - a.remaining_work,
            ))
        };
        let original = run(false, 1_000_000)?;
        assert!(!original.1);
        assert_eq!(
            original.0,
            vec![SourceInterval {
                block_index: 5,
                start: 0,
                end: 4
            }]
        );
        let exhausted = run(true, original.2)?;
        assert_eq!(
            exhausted, original,
            "new work cannot erase an existing fallback proof"
        );
        let coupled = run(true, 1_000_000)?;
        assert!(coupled.1);
        assert_eq!(coupled.0[0], original.0[0]);
        assert_eq!(coupled.0.len(), 5);
        assert!(coupled.2 > original.2);
        Ok(())
    }
    #[test]
    fn coupled_parent_keeps_partial_final_regions_and_extraction_or_normalization_gaps()
    -> Result<()> {
        let blocks = [coupled_blocks(0), coupled_blocks(100)];
        for control in 0..3 {
            let run = run_coupled_parent(
                [&blocks[0], &blocks[1]],
                1_000_000,
                |_, _, _, _, regions| {
                    regions.push(super::super::UnresolvedRegion {
                        old_span: Some(strict_closed_fragment_span(2, 0, 4)),
                        new_span: (control == 0).then(|| strict_closed_fragment_span(101, 0, 6)),
                        evidence: match control {
                            1 => vec![AlignmentEvidence::ExtractionGap],
                            2 => vec![AlignmentEvidence::NormalizationIssue],
                            _ => Vec::new(),
                        },
                    });
                },
            )?;
            assert!(!run.adopted, "final diagnostic control {control}");
            assert!(run.accepted.iter().all(Vec::is_empty));
        }
        Ok(())
    }

    #[test]
    fn coupled_parent_publication_never_splits_a_current_unresolved_range() -> Result<()> {
        let blocks = coupled_blocks(0);
        let source = side(&blocks);
        let mut owner = Ownership::new();
        owner.accepted.push(SourceInterval {
            block_index: 4,
            start: 18,
            end: 23,
        });
        let partition = owner.resolution(&source, 64)?;
        let split = [SourceInterval {
            block_index: 4,
            start: 0,
            end: 17,
        }];
        assert!(super::coupled_partition(&partition, &source, &split, 64)?.is_none());
        assert!(
            super::coupled_partition(&partition, &source, &split, partition.len() - 1)?.is_none()
        );
        let complete = [SourceInterval {
            block_index: 4,
            start: 0,
            end: 18,
        }];
        let next = super::coupled_partition(&partition, &source, &complete, partition.len())?
            .expect("complete unresolved range can be replaced");
        assert!(next.len() <= partition.len());
        assert_eq!(&next[..4], &partition[..4]);
        assert_eq!(next[4].state, super::super::ResolutionState::Equal);
        assert_eq!(
            next[4].comparable_range,
            crate::diff::TokenRange { start: 0, end: 23 }
        );
        Ok(())
    }
}
