//! Conservative suffix translation adoption.
//!
//! This pass runs after every existing equal-fragment and raw-cut recovery pass
//! and spends only the budget they left. It reconstructs its seeds from the
//! surviving strict-closed `DomainProof` records, so the in-place compaction of
//! the earlier deferred candidate list never hides a seed, and it adopts a
//! maximal translated suffix only when every prerequisite proof completes for
//! the whole proposed range. A held, incomplete or exhausted proof adopts
//! nothing.

use std::collections::{HashMap, HashSet};

use crate::{
    Result,
    diff::{ProvenChangedRegion, Side},
};

use super::{
    Assessor, ChangeCandidate, ComparisonAssumption, Ownership, RelationOutcome,
    SearchCompleteness, SourceInterval, TextSpan, project, reserve_ranges,
};

/// One proposed suffix: blocks, selected ranges and the exact delta bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SuffixProposal {
    old_block: usize,
    old_start: usize,
    old_end: usize,
    new_block: usize,
    new_start: usize,
    new_end: usize,
    delta_x_bits: u64,
    delta_y_bits: u64,
}

/// The latest ownership and change evidence one suffix attempt must respect,
/// bundled so the attempt keeps a small fixed argument list.
struct SuffixContext<'a> {
    ownership: &'a mut [Ownership; 2],
    candidates: &'a [ChangeCandidate],
    proven: &'a [ProvenChangedRegion],
    limit: usize,
}

/// Outcome of one suffix attempt.
enum SuffixStep {
    /// Nothing was adopted; the pass may continue.
    Continue,
    /// The suffix was adopted with its premise.
    Adopted,
    /// The shared budget or a reservation was exhausted; stop the pass.
    Stop,
}

type Bounds = (f64, f64, f64, f64);

impl Assessor<'_, '_> {
    /// Adopts maximal translated suffixes after every existing recovery pass.
    ///
    /// Seeds are reconstructed from the surviving strict-closed domain proofs
    /// in deterministic source-coordinate order. A suffix is adopted only when
    /// the whole-range source certificate, an independent whole-block anchor
    /// with the same exact delta, the same-block band/obstacle scan, the
    /// reference order checks, occurrence uniqueness and the latest ownership
    /// vetoes all complete. Every adoption records
    /// [`ComparisonAssumption::RigidSuffixTranslation`].
    pub(super) fn recover_suffix_translations(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        limit: usize,
    ) -> Result<()> {
        if self.remaining_work == 0 || self.output_stop.is_some() {
            return Ok(());
        }
        if !self.charge(self.domains.len()) {
            return Ok(());
        }
        let mut eligible: Vec<usize> = Vec::new();
        for proof in self.domains.values() {
            let relation = proof.relation;
            let Some(record) = self.records.get(relation) else {
                continue;
            };
            if !proof.unique
                || !proof.strict_unique
                || proof.search != SearchCompleteness::Complete
                || !proof.edits.is_empty()
                || proof.lengths[0] == 0
                || proof.lengths[0] != proof.lengths[1]
                || record.outcome != RelationOutcome::Established
                || record.search != SearchCompleteness::Complete
                || !record.reasons.is_empty()
            {
                continue;
            }
            if eligible.try_reserve(1).is_err() {
                self.remaining_work = 0;
                return Ok(());
            }
            eligible.push(relation);
        }
        let mut seeds: Vec<(usize, TextSpan, TextSpan)> = Vec::new();
        for relation in eligible {
            let cost = self.records.get(relation).and_then(|record| {
                let old = record.old_span.as_ref()?;
                let new = record.new_span.as_ref()?;
                Some(old.blocks.len().saturating_add(new.blocks.len()))
            });
            let Some(cost) = cost else {
                continue;
            };
            if !self.charge(cost) {
                return Ok(());
            }
            let Some((old_span, new_span)) = self.copy_recorded_spans(relation)? else {
                continue;
            };
            if seeds.try_reserve(1).is_err() {
                self.remaining_work = 0;
                return Ok(());
            }
            seeds.push((relation, old_span, new_span));
        }
        let sort_work = seeds
            .len()
            .saturating_mul(seeds.len().checked_ilog2().unwrap_or(0) as usize + 1);
        if !self.charge(sort_work) {
            return Ok(());
        }
        seeds.sort_unstable_by_key(|(relation, old, new)| {
            (
                old.blocks.first().map(|block| block.0),
                old.comparable_range.start,
                old.comparable_range.end,
                new.blocks.first().map(|block| block.0),
                new.comparable_range.start,
                new.comparable_range.end,
                *relation,
            )
        });
        let mut proposed: HashSet<SuffixProposal> = HashSet::new();
        for (relation, old_span, new_span) in seeds {
            if self.remaining_work == 0 {
                return Ok(());
            }
            match self.try_suffix_translation(
                SuffixContext {
                    ownership: &mut *ownership,
                    candidates,
                    proven,
                    limit,
                },
                relation,
                &old_span,
                &new_span,
                &mut proposed,
            )? {
                SuffixStep::Continue | SuffixStep::Adopted => {}
                SuffixStep::Stop => return Ok(()),
            }
        }
        Ok(())
    }

    fn try_suffix_translation(
        &mut self,
        context: SuffixContext<'_>,
        _relation: usize,
        old_span: &TextSpan,
        new_span: &TextSpan,
        proposed: &mut HashSet<SuffixProposal>,
    ) -> Result<SuffixStep> {
        let SuffixContext {
            ownership,
            candidates,
            proven,
            limit,
        } = context;
        if !self.charge(old_span.blocks.len()) {
            return Ok(SuffixStep::Stop);
        }
        let old_intervals = project(self.sides[0], old_span)?;
        if !self.charge(new_span.blocks.len()) {
            return Ok(SuffixStep::Stop);
        }
        let new_intervals = project(self.sides[1], new_span)?;
        if old_intervals.len() != 1 || new_intervals.len() != 1 {
            return Ok(SuffixStep::Continue);
        }
        let old_interval = old_intervals[0];
        let new_interval = new_intervals[0];
        let (old_block, old_start, old_end) = (
            old_interval.block_index,
            old_interval.start,
            old_interval.end,
        );
        let (new_block, new_start, new_end) = (
            new_interval.block_index,
            new_interval.start,
            new_interval.end,
        );
        let len = old_end - old_start;
        if len == 0 || len != new_end - new_start {
            return Ok(SuffixStep::Continue);
        }
        let ownership_scan = ownership[0]
            .accepted
            .len()
            .saturating_add(ownership[0].changed.len())
            .saturating_add(ownership[1].accepted.len())
            .saturating_add(ownership[1].changed.len());
        if !self.charge(ownership_scan) {
            return Ok(SuffixStep::Stop);
        }
        if seed_ownership_vetoed(
            ownership,
            SourceInterval {
                block_index: old_block,
                start: old_start,
                end: old_end,
            },
            SourceInterval {
                block_index: new_block,
                start: new_start,
                end: new_end,
            },
        ) {
            return Ok(SuffixStep::Continue);
        }
        let Some(key) = self.constant_delta(old_block, old_start, new_block, new_start, len)?
        else {
            return Ok(SuffixStep::Continue);
        };
        if f64::from_bits(key.0) == 0.0 && f64::from_bits(key.1) == 0.0 {
            return Ok(SuffixStep::Continue);
        }
        let (Some(old_page), Some(new_page)) = (
            single_page(self.sides[0], old_block),
            single_page(self.sides[1], new_block),
        ) else {
            return Ok(SuffixStep::Continue);
        };
        if !self.block_horizontal(0, old_block)? || !self.block_horizontal(1, new_block)? {
            return Ok(SuffixStep::Continue);
        }
        let Some((old_span, new_span)) = self.extend_suffix(old_interval, new_interval, key)?
        else {
            return Ok(SuffixStep::Continue);
        };
        let (old_block, old_start, old_end) = (old_span.block_index, old_span.start, old_span.end);
        let (new_block, new_start, new_end) = (new_span.block_index, new_span.start, new_span.end);
        if old_end != self.sides[0].canonical[old_block].len()
            || new_end != self.sides[1].canonical[new_block].len()
        {
            return Ok(SuffixStep::Continue);
        }
        let proposal = SuffixProposal {
            old_block,
            old_start,
            old_end,
            new_block,
            new_start,
            new_end,
            delta_x_bits: key.0,
            delta_y_bits: key.1,
        };
        if !self.charge(1) {
            return Ok(SuffixStep::Stop);
        }
        if proposed.try_reserve(1).is_err() {
            return Ok(SuffixStep::Stop);
        }
        if !proposed.insert(proposal) {
            return Ok(SuffixStep::Continue);
        }
        // Cheap vetoes first: the expanded suffix contains the seed, so an
        // overlapping seed, ownership conflict, candidate, proven region or
        // limit cannot become a valid no-overlap suffix.
        let old_span = SourceInterval {
            block_index: old_block,
            start: old_start,
            end: old_end,
        };
        let new_span = SourceInterval {
            block_index: new_block,
            start: new_start,
            end: new_end,
        };
        match self.suffix_vetoes(ownership, candidates, proven, limit, old_span, new_span)? {
            Some(true) => {}
            Some(false) => return Ok(SuffixStep::Continue),
            None => return Ok(SuffixStep::Stop),
        }
        // At least one independent whole-block anchor with this exact delta
        // must also pass the same-block band/obstacle adjacency.
        let Some(anchors) =
            self.suffix_anchors(ownership, old_page, new_page, key, old_block, new_block)?
        else {
            return Ok(SuffixStep::Stop);
        };
        if anchors.is_empty() {
            return Ok(SuffixStep::Continue);
        }
        match self.suffix_adjacency(old_span, new_span, (old_page, new_page)) {
            Some(true) => {}
            Some(false) => return Ok(SuffixStep::Continue),
            None => return Ok(SuffixStep::Stop),
        }
        // Relative geometry against the production whole-block population.
        match self.suffix_references(ownership, (old_page, new_page), old_span, new_span)? {
            Some(true) => {}
            Some(false) => return Ok(SuffixStep::Continue),
            None => return Ok(SuffixStep::Stop),
        }
        // Full transformed-key occurrence uniqueness in both directions.
        match self.suffix_unique((old_page, new_page), old_span, new_span)? {
            Some(true) => {}
            Some(false) => return Ok(SuffixStep::Continue),
            None => return Ok(SuffixStep::Stop),
        }
        // The whole-range source certificate including document-wide sharing
        // is the most expensive proof and runs last.
        let verdict = {
            let mut remaining = self.remaining_work;
            let verdict = super::raw_source::raw_source_range_certificate(
                [self.sides[0], self.sides[1]],
                old_block,
                old_start..old_end,
                new_block,
                new_start..new_end,
                key,
                &mut remaining,
            );
            self.remaining_work = remaining;
            verdict
        };
        match verdict {
            super::raw_source::RawSourceVerdict::Isomorphic => {}
            super::raw_source::RawSourceVerdict::Exhausted => return Ok(SuffixStep::Stop),
            _ => return Ok(SuffixStep::Continue),
        }
        let accepted = [
            [SourceInterval {
                block_index: old_block,
                start: old_start,
                end: old_end,
            }],
            [SourceInterval {
                block_index: new_block,
                start: new_start,
                end: new_end,
            }],
        ];
        let max_records = self.options.max_assessment_ranges;
        if self.output_stop.is_some() || self.records.len() >= max_records.saturating_sub(1) {
            return Ok(SuffixStep::Continue);
        }
        let mut old_blocks = Vec::new();
        let mut new_blocks = Vec::new();
        let mut assumptions = Vec::new();
        if old_blocks.try_reserve_exact(1).is_err()
            || new_blocks.try_reserve_exact(1).is_err()
            || assumptions.try_reserve_exact(1).is_err()
        {
            return Ok(SuffixStep::Continue);
        }
        old_blocks.push(self.sides[0].blocks[old_block].block);
        new_blocks.push(self.sides[1].blocks[new_block].block);
        assumptions.push(ComparisonAssumption::RigidSuffixTranslation);
        let old_adopted = TextSpan {
            blocks: old_blocks,
            separator: None,
            canonical_range: crate::normalize::ScalarRange {
                start: old_start,
                end: old_end,
            },
            comparable_range: crate::diff::TokenRange {
                start: old_start,
                end: old_end,
            },
        };
        let new_adopted = TextSpan {
            blocks: new_blocks,
            separator: None,
            canonical_range: crate::normalize::ScalarRange {
                start: new_start,
                end: new_end,
            },
            comparable_range: crate::diff::TokenRange {
                start: new_start,
                end: new_end,
            },
        };
        for (owner, ranges) in ownership.iter_mut().zip(&accepted) {
            reserve_ranges(&mut owner.accepted, ranges.len(), limit)?;
        }
        let index = self.record(crate::diff::RelationAssessment {
            old_span: Some(old_adopted),
            new_span: Some(new_adopted),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions,
            reasons: Vec::new(),
        })?;
        let stored = self.records.get(index).is_some_and(|stored| {
            stored.outcome == RelationOutcome::Established
                && stored.search == SearchCompleteness::Complete
                && stored.reasons.is_empty()
                && stored
                    .assumptions
                    .contains(&ComparisonAssumption::RigidSuffixTranslation)
        });
        if !stored {
            return Ok(SuffixStep::Continue);
        }
        for (owner, ranges) in ownership.iter_mut().zip(&accepted) {
            owner.accepted.extend(ranges.iter().copied());
        }
        Ok(SuffixStep::Adopted)
    }

    /// Constant finite `new - old` delta over one token range, or `None`.
    fn constant_delta(
        &mut self,
        old_block: usize,
        old_start: usize,
        new_block: usize,
        new_start: usize,
        len: usize,
    ) -> Result<Option<(u64, u64)>> {
        let Some(old_signatures) = complete_signatures(self.sides[0], old_block) else {
            return Ok(None);
        };
        let Some(new_signatures) = complete_signatures(self.sides[1], new_block) else {
            return Ok(None);
        };
        if old_start + len > old_signatures.len() || new_start + len > new_signatures.len() {
            return Ok(None);
        }
        if !self.charge(len) {
            return Ok(None);
        }
        let mut key: Option<(u64, u64)> = None;
        for offset in 0..len {
            let old = old_signatures[old_start + offset];
            let new = new_signatures[new_start + offset];
            if !same_direction(old, new) {
                return Ok(None);
            }
            let dx = new.baseline().x - old.baseline().x;
            let dy = new.baseline().y - old.baseline().y;
            if !dx.is_finite() || !dy.is_finite() {
                return Ok(None);
            }
            let bits = (dx.to_bits(), dy.to_bits());
            match key {
                None => key = Some(bits),
                Some(previous) if previous != bits => return Ok(None),
                Some(_) => {}
            }
        }
        Ok(key)
    }

    /// Extends one seed in both directions to a maximal literal,
    /// same-direction, exact-delta suffix, returning the new edges.
    fn extend_suffix(
        &mut self,
        old: SourceInterval,
        new: SourceInterval,
        key: (u64, u64),
    ) -> Result<Option<(SourceInterval, SourceInterval)>> {
        let (old_block, mut old_start, mut old_end) = (old.block_index, old.start, old.end);
        let (new_block, mut new_start, mut new_end) = (new.block_index, new.start, new.end);
        let old_len = self.sides[0].canonical[old_block].len();
        let new_len = self.sides[1].canonical[new_block].len();
        while old_start > 0 && new_start > 0 {
            if !self.charge(1) {
                return Ok(None);
            }
            let old_token = old_start - 1;
            let new_token = new_start - 1;
            let (Some(old_signature), Some(new_signature)) = (
                position_at(self.sides[0], old_block, old_token),
                position_at(self.sides[1], new_block, new_token),
            ) else {
                break;
            };
            if !suffix_step_matches(
                (self.sides[0], old_block, old_token, old_signature),
                (self.sides[1], new_block, new_token, new_signature),
                key,
            ) {
                break;
            }
            old_start -= 1;
            new_start -= 1;
        }
        while old_end < old_len && new_end < new_len {
            if !self.charge(1) {
                return Ok(None);
            }
            let (Some(old_signature), Some(new_signature)) = (
                position_at(self.sides[0], old_block, old_end),
                position_at(self.sides[1], new_block, new_end),
            ) else {
                break;
            };
            if !suffix_step_matches(
                (self.sides[0], old_block, old_end, old_signature),
                (self.sides[1], new_block, new_end, new_signature),
                key,
            ) {
                break;
            }
            old_end += 1;
            new_end += 1;
        }
        Ok(Some((
            SourceInterval {
                block_index: old_block,
                start: old_start,
                end: old_end,
            },
            SourceInterval {
                block_index: new_block,
                start: new_start,
                end: new_end,
            },
        )))
    }

    /// Every independent whole-block owned Established anchor at the block
    /// immediately after the suffix with this exact delta, in deterministic
    /// source order. `None` means the shared budget
    /// was exhausted while collecting, so the caller must stop rather than
    /// treat the suffix as unanchored.
    fn suffix_anchors(
        &mut self,
        ownership: &[Ownership; 2],
        old_page: u32,
        new_page: u32,
        key: (u64, u64),
        own_old_block: usize,
        own_new_block: usize,
    ) -> Result<Option<Vec<(usize, usize)>>> {
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(None);
        };
        if !self.charge(established.len()) {
            return Ok(None);
        }
        let mut candidates: Vec<(usize, usize)> = Vec::new();
        for entry in established {
            let (Some(&old_block), Some(&new_block)) = (
                self.sides[0].index.get(&entry.old_block),
                self.sides[1].index.get(&entry.new_block),
            ) else {
                return Ok(None);
            };
            if old_block != own_old_block + 1 || new_block != own_new_block + 1 {
                continue;
            }
            if single_page(self.sides[0], old_block) != Some(old_page)
                || single_page(self.sides[1], new_block) != Some(new_page)
            {
                continue;
            }
            let len = self.sides[0].canonical[old_block].len();
            let Some(anchor_key) = self.constant_delta(old_block, 0, new_block, 0, len)? else {
                continue;
            };
            if anchor_key != key {
                continue;
            }
            if !self.block_horizontal(0, old_block)? || !self.block_horizontal(1, new_block)? {
                continue;
            }
            if candidates.try_reserve(1).is_err() {
                return Ok(None);
            }
            candidates.push((old_block, new_block));
        }
        let sort_work = candidates
            .len()
            .saturating_mul(candidates.len().checked_ilog2().unwrap_or(0) as usize + 1);
        if !self.charge(sort_work) {
            return Ok(None);
        }
        candidates.sort_unstable();
        Ok(Some(candidates))
    }

    /// Whether the suffix reaches both block ends and is band-adjacent to the
    /// anchor without any unselected piece, including the candidate's own
    /// block prefix, standing between them.
    ///
    /// The band is the strict positive overlap of the suffix and anchor
    /// baseline x-intervals and the gap is their orthogonal separation, as in
    /// the established whole-block neighbour rule. Every other same-page
    /// source block is compared by its whole-block baseline bounds with
    /// closed-interval band and gap intersection, so a block whose bounds
    /// cross the band while its endpoint baselines sit outside it still
    /// counts as an obstacle; missing geometry vetoes instead of being
    /// ignored. The candidate's own unselected prefix is bounded by the same
    /// closed intersection: touching the gap boundary counts, because the
    /// prefix's own translation is not proven by this pass. The anchor block
    /// is the neighbour itself and is not an obstacle. The anchor is always
    /// the block immediately after the
    /// suffix, and every anchor candidate in that position is equivalent for
    /// this scan, so the caller only has to prove that at least one
    /// independent anchor exists.
    fn suffix_adjacency(
        &mut self,
        old: SourceInterval,
        new: SourceInterval,
        pages: (u32, u32),
    ) -> Option<bool> {
        let (old_block, old_start, old_end) = (old.block_index, old.start, old.end);
        let (new_block, new_start, new_end) = (new.block_index, new.start, new.end);
        let (old_page, new_page) = pages;
        let anchor_old_block = old_block + 1;
        let anchor_new_block = new_block + 1;
        let old_len = self.sides[0].canonical[old_block].len();
        let new_len = self.sides[1].canonical[new_block].len();
        if old_end != old_len || new_end != new_len {
            return Some(false);
        }
        let Some(suffix_old) = self.interval_bounds(0, old_block, old_start, old_end) else {
            return Some(false);
        };
        let Some(suffix_new) = self.interval_bounds(1, new_block, new_start, new_end) else {
            return Some(false);
        };
        let Some(anchor_old) = self.interval_bounds(
            0,
            anchor_old_block,
            0,
            self.sides[0].canonical[anchor_old_block].len(),
        ) else {
            return Some(false);
        };
        let Some(anchor_new) = self.interval_bounds(
            1,
            anchor_new_block,
            0,
            self.sides[1].canonical[anchor_new_block].len(),
        ) else {
            return Some(false);
        };
        for (side_index, page, block, skip_start, anchor_block, suffix, anchor) in [
            (
                0usize,
                old_page,
                old_block,
                old_start,
                anchor_old_block,
                suffix_old,
                anchor_old,
            ),
            (
                1usize,
                new_page,
                new_block,
                new_start,
                anchor_new_block,
                suffix_new,
                anchor_new,
            ),
        ] {
            let side = self.sides[side_index];
            let band_min = suffix.0.max(anchor.0);
            let band_max = suffix.2.min(anchor.2);
            if band_min >= band_max {
                return Some(false);
            }
            let gap = if anchor.3 <= suffix.1 {
                (anchor.3, suffix.1)
            } else if suffix.3 <= anchor.1 {
                (suffix.3, anchor.1)
            } else {
                return Some(false);
            };
            if !self.charge(side.blocks.len()) {
                return None;
            }
            for (index, candidate_block) in side.blocks.iter().enumerate() {
                if index == anchor_block {
                    continue;
                }
                let prefix_only = index == block;
                if prefix_only && skip_start == 0 {
                    continue;
                }
                if candidate_block.pages.is_empty()
                    || (candidate_block.pages.len() > 1 && candidate_block.pages.contains(&page))
                {
                    return Some(false);
                }
                if !candidate_block.pages.contains(&page) {
                    continue;
                }
                let no_tokens = side.canonical[index].is_empty();
                let no_source = candidate_block.raw.source_map.is_empty()
                    && candidate_block.canonical.source_map.is_empty();
                if no_tokens
                    && no_source
                    && candidate_block.raw.text.is_empty()
                    && candidate_block.canonical.text.is_empty()
                {
                    continue;
                }
                let Some(signatures) = candidate_block.position_signatures.as_deref() else {
                    return Some(false);
                };
                if signatures.len() != side.canonical[index].len() {
                    return Some(false);
                }
                let scan_end = if prefix_only {
                    skip_start
                } else {
                    signatures.len()
                };
                if !self.charge(scan_end.saturating_add(1)) {
                    return None;
                }
                let mut bounds: Option<Bounds> = None;
                for signature in &signatures[..scan_end] {
                    let baseline = signature.baseline();
                    if !baseline.x.is_finite() || !baseline.y.is_finite() {
                        return Some(false);
                    }
                    bounds = Some(match bounds {
                        None => (baseline.x, baseline.y, baseline.x, baseline.y),
                        Some((min_x, min_y, max_x, max_y)) => (
                            min_x.min(baseline.x),
                            min_y.min(baseline.y),
                            max_x.max(baseline.x),
                            max_y.max(baseline.y),
                        ),
                    });
                }
                let Some(obstacle) = bounds else {
                    return Some(false);
                };
                if obstacle.0.max(band_min) > obstacle.2.min(band_max) {
                    continue;
                }
                if obstacle.1.max(gap.0) <= obstacle.3.min(gap.1) {
                    return Some(false);
                }
            }
        }
        Some(true)
    }

    /// Relative geometry against the production whole-block reference
    /// population.
    ///
    /// The population is exactly `collect_established_blocks`: whole original
    /// block members with a complete Established proof that are already owned
    /// on both sides, including stationary and nonuniform correspondences.
    /// Pending or partial matches are not independent references, and a
    /// reference with unknown geometry on the candidate page holds the pass.
    fn suffix_references(
        &mut self,
        ownership: &[Ownership; 2],
        pages: (u32, u32),
        old: SourceInterval,
        new: SourceInterval,
    ) -> Result<Option<bool>> {
        let (old_block, old_start, old_end) = (old.block_index, old.start, old.end);
        let (new_block, new_start, new_end) = (new.block_index, new.start, new.end);
        let (old_page, new_page) = pages;
        let Some(established) = self.collect_established_blocks(ownership)? else {
            return Ok(None);
        };
        if !self.charge(established.len()) {
            return Ok(None);
        }
        let Some(suffix_old) = self.interval_bounds(0, old_block, old_start, old_end) else {
            return Ok(None);
        };
        let Some(suffix_new) = self.interval_bounds(1, new_block, new_start, new_end) else {
            return Ok(None);
        };
        for entry in established {
            let (Some(&index_old), Some(&index_new)) = (
                self.sides[0].index.get(&entry.old_block),
                self.sides[1].index.get(&entry.new_block),
            ) else {
                return Ok(None);
            };
            if index_old == old_block || index_new == new_block {
                continue;
            }
            let old_len = self.sides[0].canonical[index_old].len();
            let new_len = self.sides[1].canonical[index_new].len();
            let pages = (
                single_page(self.sides[0], index_old),
                single_page(self.sides[1], index_new),
            );
            let (Some(page_old), Some(page_new)) = pages else {
                return Ok(None);
            };
            if page_old != old_page && page_new != new_page {
                continue;
            }
            if page_old != old_page || page_new != new_page {
                return Ok(None);
            }
            let Some(reference_old) = self.interval_bounds(0, index_old, 0, old_len) else {
                return Ok(None);
            };
            let Some(reference_new) = self.interval_bounds(1, index_new, 0, new_len) else {
                return Ok(None);
            };
            if relative_flags(suffix_old, reference_old)
                != relative_flags(suffix_new, reference_new)
            {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    /// Full transformed-key occurrence uniqueness in both directions.
    ///
    /// Every scan step is charged. An occurrence with unknown page metadata or
    /// incomplete geometry holds the pass; a provably different page is not a
    /// competing occurrence.
    fn suffix_unique(
        &mut self,
        pages: (u32, u32),
        old: SourceInterval,
        new: SourceInterval,
    ) -> Result<Option<bool>> {
        let (old_page, new_page) = pages;
        let (old_block, old_start, old_end) = (old.block_index, old.start, old.end);
        let (new_block, new_start, new_end) = (new.block_index, new.start, new.end);
        let old_len = old_end - old_start;
        if old_len != new_end - new_start {
            return Ok(Some(false));
        }
        let Some(old_signatures) = complete_signatures(self.sides[0], old_block) else {
            return Ok(Some(false));
        };
        let Some(new_signatures) = complete_signatures(self.sides[1], new_block) else {
            return Ok(Some(false));
        };
        let mut old_index: HashMap<char, Vec<(usize, usize)>> = HashMap::new();
        let mut new_index: HashMap<char, Vec<(usize, usize)>> = HashMap::new();
        for side_index in 0..2 {
            let side = self.sides[side_index];
            if !self.charge(side.canonical.len()) {
                return Ok(None);
            }
            let total: usize = side.canonical.iter().map(Vec::len).sum();
            if !self.charge(total) {
                return Ok(None);
            }
            let map = if side_index == 0 {
                &mut old_index
            } else {
                &mut new_index
            };
            if map.try_reserve(total).is_err() {
                return Ok(None);
            }
            for (block_index, tokens) in side.canonical.iter().enumerate() {
                for (token_index, token) in tokens.iter().enumerate() {
                    if let Some(scalar) = token.as_scalar() {
                        let map = if side_index == 0 {
                            &mut old_index
                        } else {
                            &mut new_index
                        };
                        if !self.charge(1) {
                            return Ok(None);
                        }
                        let postings = map.entry(scalar).or_default();
                        if postings.try_reserve(1).is_err() {
                            return Ok(None);
                        }
                        postings.push((block_index, token_index));
                    }
                }
            }
        }
        if !self.charge(old_len.saturating_mul(2)) {
            return Ok(None);
        }
        for offset in 0..old_len {
            let old_token = old_start + offset;
            let new_token = new_start + offset;
            let old_signature = old_signatures[old_token];
            let new_signature = new_signatures[new_token];
            let key = key_between(old_signature, new_signature);
            let Some(scalar) = self.sides[0].canonical[old_block][old_token].as_scalar() else {
                return Ok(Some(false));
            };
            if let Some(occurrences) = new_index.get(&scalar) {
                for &(block_index, token_index) in occurrences {
                    if !self.charge(1) {
                        return Ok(None);
                    }
                    if block_index == new_block && token_index == new_token {
                        continue;
                    }
                    let Some(occurrence) = position_at(self.sides[1], block_index, token_index)
                    else {
                        return Ok(Some(false));
                    };
                    match single_page(self.sides[1], block_index) {
                        Some(page) if page == new_page => {}
                        Some(_) => continue,
                        None => return Ok(Some(false)),
                    }
                    if key_between(old_signature, occurrence) == key {
                        return Ok(Some(false));
                    }
                }
            }
            let Some(scalar) = self.sides[1].canonical[new_block][new_token].as_scalar() else {
                return Ok(Some(false));
            };
            if let Some(occurrences) = old_index.get(&scalar) {
                for &(block_index, token_index) in occurrences {
                    if !self.charge(1) {
                        return Ok(None);
                    }
                    if block_index == old_block && token_index == old_token {
                        continue;
                    }
                    let Some(occurrence) = position_at(self.sides[0], block_index, token_index)
                    else {
                        return Ok(Some(false));
                    };
                    match single_page(self.sides[0], block_index) {
                        Some(page) if page == old_page => {}
                        Some(_) => continue,
                        None => return Ok(Some(false)),
                    }
                    if key_between(occurrence, new_signature) == key {
                        return Ok(Some(false));
                    }
                }
            }
        }
        Ok(Some(true))
    }

    /// Latest-ownership, candidate, proven, containment and limit vetoes for
    /// the whole proposed suffix.
    fn suffix_vetoes(
        &mut self,
        ownership: &mut [Ownership; 2],
        candidates: &[ChangeCandidate],
        proven: &[ProvenChangedRegion],
        limit: usize,
        old: SourceInterval,
        new: SourceInterval,
    ) -> Result<Option<bool>> {
        let (old_block, old_start, old_end) = (old.block_index, old.start, old.end);
        let (new_block, new_start, new_end) = (new.block_index, new.start, new.end);
        let accepted = [old_block, new_block];
        let ranges = [(old_start, old_end), (new_start, new_end)];
        for side in 0..2 {
            if !self.charge(ownership[side].changed.len()) {
                return Ok(None);
            }
            for changed in &ownership[side].changed {
                if changed.block_index == accepted[side]
                    && changed.start < ranges[side].1
                    && ranges[side].0 < changed.end
                {
                    return Ok(Some(false));
                }
            }
            if !self.charge(ownership[side].accepted.len()) {
                return Ok(None);
            }
            for existing in &ownership[side].accepted {
                if existing.block_index == accepted[side]
                    && existing.start < ranges[side].1
                    && ranges[side].0 < existing.end
                {
                    return Ok(Some(false));
                }
            }
            if ownership[side].accepted.len().saturating_add(1) > limit {
                return Ok(Some(false));
            }
        }
        if !self.charge(candidates.len()) {
            return Ok(None);
        }
        for candidate in candidates {
            if !self.charge(candidate.change.occurrences.len()) {
                return Ok(None);
            }
            for occurrence in &candidate.change.occurrences {
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
                    let Ok(source) = project(self.sides[side], span) else {
                        return Ok(None);
                    };
                    if source.iter().any(|interval| {
                        interval.block_index == accepted[side]
                            && interval.start < ranges[side].1
                            && ranges[side].0 < interval.end
                    }) {
                        return Ok(Some(false));
                    }
                }
            }
        }
        if !self.charge(proven.len().saturating_mul(2)) {
            return Ok(None);
        }
        for region in proven {
            for (side, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
                .into_iter()
                .enumerate()
            {
                let Some(span) = span else {
                    continue;
                };
                if !self.charge(span.blocks.len()) {
                    return Ok(None);
                }
                let Ok(source) = project(self.sides[side], span) else {
                    return Ok(None);
                };
                if source.iter().any(|interval| {
                    interval.block_index == accepted[side]
                        && interval.start < ranges[side].1
                        && ranges[side].0 < interval.end
                }) {
                    return Ok(Some(false));
                }
            }
        }
        Ok(Some(true))
    }

    /// Charged interval bounds over the selected signatures of one block.
    fn interval_bounds(
        &mut self,
        side_index: usize,
        block: usize,
        start: usize,
        end: usize,
    ) -> Option<Bounds> {
        if !self.charge(end.saturating_sub(start)) {
            return None;
        }
        interval_bounds_unchecked(self.sides[side_index], block, start, end)
    }

    /// Whether every selected signature of one block is complete, finite,
    /// horizontal and has a direction.
    fn block_horizontal(&mut self, side_index: usize, block: usize) -> Result<bool> {
        let Some(signatures) = complete_signatures(self.sides[side_index], block) else {
            return Ok(false);
        };
        if !self.charge(signatures.len()) {
            return Ok(false);
        }
        Ok(signatures.iter().all(super::views::horizontal_direction))
    }
}

fn complete_signatures<'a>(
    side: &'a Side<'_>,
    block: usize,
) -> Option<&'a [crate::normalize::PositionSignature]> {
    let signatures = side.blocks[block].position_signatures.as_deref()?;
    if signatures.len() != side.canonical[block].len() {
        return None;
    }
    Some(signatures)
}

fn position_at(
    side: &Side<'_>,
    block: usize,
    token: usize,
) -> Option<crate::normalize::PositionSignature> {
    complete_signatures(side, block)?.get(token).copied()
}

fn single_page(side: &Side<'_>, block: usize) -> Option<u32> {
    let [page] = side.blocks[block].pages.as_slice() else {
        return None;
    };
    Some(*page)
}

fn same_direction(
    old: crate::normalize::PositionSignature,
    new: crate::normalize::PositionSignature,
) -> bool {
    let old = old.direction();
    let new = new.direction();
    old.x.to_bits() == new.x.to_bits() && old.y.to_bits() == new.y.to_bits()
}

fn key_between(
    old: crate::normalize::PositionSignature,
    new: crate::normalize::PositionSignature,
) -> (u64, u64) {
    (
        (new.baseline().x - old.baseline().x).to_bits(),
        (new.baseline().y - old.baseline().y).to_bits(),
    )
}

type StepSample<'a> = (
    &'a Side<'a>,
    usize,
    usize,
    crate::normalize::PositionSignature,
);

fn suffix_step_matches(old: StepSample<'_>, new: StepSample<'_>, key: (u64, u64)) -> bool {
    let (old_side, old_block, old_token, old_signature) = old;
    let (new_side, new_block, new_token, new_signature) = new;
    old_side.canonical[old_block][old_token].as_scalar()
        == new_side.canonical[new_block][new_token].as_scalar()
        && same_direction(old_signature, new_signature)
        && key_between(old_signature, new_signature) == key
}

/// Cheap prefilter: the seed is contained in every later suffix proposal, so
/// an already accepted or changed interval overlapping the seed vetoes the
/// attempt before any block-wide scan or extension work runs.
fn seed_ownership_vetoed(
    ownership: &[Ownership; 2],
    old: SourceInterval,
    new: SourceInterval,
) -> bool {
    for (side, seed) in [old, new].into_iter().enumerate() {
        for interval in ownership[side]
            .accepted
            .iter()
            .chain(ownership[side].changed.iter())
        {
            if interval.block_index == seed.block_index
                && interval.start < seed.end
                && seed.start < interval.end
            {
                return true;
            }
        }
    }
    false
}

fn interval_bounds_unchecked(
    side: &Side<'_>,
    block: usize,
    start: usize,
    end: usize,
) -> Option<Bounds> {
    let signatures = complete_signatures(side, block)?;
    if end > signatures.len() || start >= end {
        return None;
    }
    let mut bounds: Option<Bounds> = None;
    for signature in &signatures[start..end] {
        let baseline = signature.baseline();
        if !baseline.x.is_finite() || !baseline.y.is_finite() {
            return None;
        }
        bounds = Some(match bounds {
            None => (baseline.x, baseline.y, baseline.x, baseline.y),
            Some((min_x, min_y, max_x, max_y)) => (
                min_x.min(baseline.x),
                min_y.min(baseline.y),
                max_x.max(baseline.x),
                max_y.max(baseline.y),
            ),
        });
    }
    bounds
}

fn relative_flags(candidate: Bounds, reference: Bounds) -> [bool; 5] {
    [
        candidate.0 <= reference.2
            && reference.0 <= candidate.2
            && candidate.1 <= reference.3
            && reference.1 <= candidate.3,
        candidate.3 <= reference.1,
        reference.3 <= candidate.1,
        candidate.2 <= reference.0,
        reference.2 <= candidate.0,
    ]
}
