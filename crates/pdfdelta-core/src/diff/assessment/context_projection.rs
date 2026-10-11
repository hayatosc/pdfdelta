//! Source projections bound to one actual coupled-context invocation.
//!
//! The adapter exclusively borrows the original mutable diagnostics and the
//! immutable sides. Its private API exposes diagnostics for reading and only
//! coordinated removal; a source span cannot be changed while its projection
//! is retained. Removing a diagnostic moves its receipt with it. No index,
//! copied key, cached correspondence, verdict, evidence or ownership is kept.
use super::{Side, SourceInterval, UnresolvedRegion, allocation_error, charge, project};
use crate::{Result, alignment::AlignmentEvidence};

/// Closed interface for the current whole context. Only this module supplies
/// implementations; no API exposes mutable spans or independently edited rows.
pub(super) trait CoupledContext {
    fn regions(&self) -> &[UnresolvedRegion];
    fn len(&self) -> usize {
        self.regions().len()
    }
    fn limit_retained_bytes(&mut self, _limit: usize) -> bool {
        true
    }
    fn retained_bytes(&self) -> usize {
        0
    }
    fn prepay_retain(&self, _remaining: &mut usize) -> bool {
        true
    }
    fn retain_removed(&mut self, removed: &[bool]);
    fn removals(
        &mut self,
        sides: [&Side<'_>; 2],
        accepted: &[Vec<SourceInterval>; 2],
        limit: usize,
        remaining: &mut usize,
    ) -> Result<Option<Vec<bool>>>;
}

type Projections = [Option<Vec<SourceInterval>>; 2];

/// An exclusive real-context borrow, with immutable input ownership.
/// A cache slot is the completed original projection of its still-live span.
/// Whole-context visits and all current coverage/evidence checks always run.
pub(super) struct ContextProjections<'context, 'a, 'document> {
    inner: &'context mut dyn CoupledContext,
    sides: [&'a Side<'document>; 2],
    projections: Vec<Projections>,
    bytes: usize,
    limit: usize,
}

impl<'context, 'a, 'document> ContextProjections<'context, 'a, 'document> {
    pub(super) fn new(
        inner: &'context mut dyn CoupledContext,
        sides: [&'a Side<'document>; 2],
        allowance: usize,
        remaining: &mut usize,
    ) -> Result<Option<Self>> {
        let count = inner.len();
        let Some(bytes) = count
            .checked_mul(std::mem::size_of::<Projections>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>()))
        else {
            return Ok(None);
        };
        let limit = allowance.min(8 * 1024 * 1024);
        if bytes > limit || !charge(remaining, count.saturating_mul(6).saturating_add(24)) {
            return Ok(None);
        }
        let mut projections = Vec::new();
        projections
            .try_reserve_exact(count)
            .map_err(|_| allocation_error("coupled context projections"))?;
        if projections.capacity() > count {
            return Ok(None);
        }
        projections.resize_with(count, || [None, None]);
        Ok(Some(Self {
            inner,
            sides,
            projections,
            bytes,
            limit,
        }))
    }
}

impl CoupledContext for Vec<UnresolvedRegion> {
    fn regions(&self) -> &[UnresolvedRegion] {
        self
    }
    fn retain_removed(&mut self, removed: &[bool]) {
        let mut index = 0;
        self.retain(|_| {
            let keep = !removed[index];
            index += 1;
            keep
        });
    }
    fn removals(
        &mut self,
        sides: [&Side<'_>; 2],
        accepted: &[Vec<SourceInterval>; 2],
        limit: usize,
        remaining: &mut usize,
    ) -> Result<Option<Vec<bool>>> {
        checked_removals(self, sides, accepted, limit, remaining, None)
    }
}

impl CoupledContext for ContextProjections<'_, '_, '_> {
    fn regions(&self) -> &[UnresolvedRegion] {
        self.inner.regions()
    }
    fn limit_retained_bytes(&mut self, limit: usize) -> bool {
        self.limit = self.limit.min(limit);
        self.bytes <= self.limit
    }
    fn retained_bytes(&self) -> usize {
        self.bytes
    }
    fn prepay_retain(&self, remaining: &mut usize) -> bool {
        charge(
            remaining,
            self.projections.len().saturating_mul(6).saturating_add(8),
        )
    }
    fn retain_removed(&mut self, removed: &[bool]) {
        self.inner.retain_removed(removed);
        let mut index = 0;
        self.projections.retain(|_| {
            let keep = !removed[index];
            index += 1;
            keep
        });
        // Dropped receipt payload remains conservatively charged in `bytes`.
    }
    fn removals(
        &mut self,
        sides: [&Side<'_>; 2],
        accepted: &[Vec<SourceInterval>; 2],
        limit: usize,
        remaining: &mut usize,
    ) -> Result<Option<Vec<bool>>> {
        if !charge(remaining, 4)
            || !std::ptr::eq(sides[0], self.sides[0])
            || !std::ptr::eq(sides[1], self.sides[1])
            || self.inner.len() != self.projections.len()
        {
            return Ok(None);
        }
        checked_removals(
            self.inner.regions(),
            self.sides,
            accepted,
            limit,
            remaining,
            Some((&mut self.projections, &mut self.bytes, self.limit)),
        )
    }
}

/// The original whole-context predicate. On a miss, the original source-token
/// allowance and `project` run unchanged. A hit skips just that projection;
/// all current accepted-range comparisons, evidence vetoes and output limits
/// still execute. The borrowed source vector is handed off without cloning.
fn checked_removals(
    regions: &[UnresolvedRegion],
    sides: [&Side<'_>; 2],
    accepted: &[Vec<SourceInterval>; 2],
    limit: usize,
    remaining: &mut usize,
    mut cache: Option<(&mut [Projections], &mut usize, usize)>,
) -> Result<Option<Vec<bool>>> {
    if regions.len() > limit || !charge(remaining, regions.len().saturating_mul(3)) {
        return Ok(None);
    }
    let mut removed = Vec::new();
    removed
        .try_reserve_exact(regions.len())
        .map_err(|_| allocation_error("coupled region marks"))?;
    for (index, region) in regions.iter().enumerate() {
        if !charge(remaining, region.evidence.len()) {
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
            if !charge(remaining, span.blocks.len().saturating_mul(2)) {
                return Ok(None);
            }
            let selection_work = accepted[side]
                .len()
                .saturating_mul(span.blocks.len())
                .saturating_mul(2);
            if !charge(remaining, selection_work) {
                return Ok(None);
            }
            let temporary;
            let projected = if let Some((slots, bytes, limit)) = cache.as_mut() {
                if !charge(remaining, 3) {
                    return Ok(None);
                }
                if slots[index][side].is_none() {
                    let Some(peak) = span
                        .blocks
                        .len()
                        .checked_mul(std::mem::size_of::<SourceInterval>())
                        .and_then(|n| (**bytes).checked_add(n))
                    else {
                        return Ok(None);
                    };
                    if peak > *limit
                        || !charge(remaining, sides[side].source_token_count(&span.blocks))
                    {
                        return Ok(None);
                    }
                    let fresh = project(sides[side], span)?;
                    let Some(actual) = fresh
                        .capacity()
                        .checked_mul(std::mem::size_of::<SourceInterval>())
                        .and_then(|n| (**bytes).checked_add(n))
                    else {
                        return Ok(None);
                    };
                    if actual > *limit || !charge(remaining, 3) {
                        return Ok(None);
                    }
                    slots[index][side] = Some(fresh);
                    **bytes = actual;
                }
                slots[index][side]
                    .as_deref()
                    .expect("completed current-span projection")
            } else {
                if !charge(remaining, sides[side].source_token_count(&span.blocks)) {
                    return Ok(None);
                }
                temporary = project(sides[side], span)?;
                &temporary
            };
            covered &= !projected.is_empty();
            for part in projected {
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
                || region.evidence.iter().any(|e| {
                    matches!(
                        e,
                        AlignmentEvidence::ExtractionGap | AlignmentEvidence::NormalizationIssue
                    )
                }))
        {
            return Ok(None);
        }
        removed.push(overlapping);
    }
    Ok(Some(removed))
}
