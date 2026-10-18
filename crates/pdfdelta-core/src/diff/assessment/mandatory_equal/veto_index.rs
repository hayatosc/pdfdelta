//! Paid immutable singleton projections; mutable ownership is never cached.
use super::{EqualConstraints, Side, SourceInterval, charge_work, ownership_projection_clear};

struct Entry<'a> {
    span: &'a super::super::TextSpan,
    region: Option<usize>,
    literal: Option<(usize, usize, usize)>,
}

/// Literal headers are grouped by source block; every deferred header remains
/// global, including an invalid header on an otherwise unrelated block.
struct Buckets {
    heads: [Vec<Option<usize>>; 2],
    next: [Vec<Option<usize>>; 2],
    deferred: [Option<usize>; 2],
    heap_bytes: usize,
}

impl Buckets {
    fn build(
        entries: &[Vec<Entry<'_>>; 2],
        block_counts: [usize; 2],
        available: usize,
        work_allowance: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        let n = entries[0].len().checked_add(entries[1].len())?;
        let blocks = block_counts[0].checked_add(block_counts[1])?;
        let work = n
            .checked_mul(16)?
            .checked_add(blocks.checked_mul(4)?)?
            .checked_add(64)?;
        let bytes = n
            .checked_add(blocks)?
            .checked_mul(std::mem::size_of::<Option<usize>>())?;
        if bytes > available || work > work_allowance || !charge_work(remaining, work) {
            return None;
        }
        let mut heads = [Vec::new(), Vec::new()];
        let mut next = [Vec::new(), Vec::new()];
        for side in 0..2 {
            heads[side].try_reserve_exact(block_counts[side]).ok()?;
            next[side].try_reserve_exact(entries[side].len()).ok()?;
        }
        let heap_bytes = heads.iter().chain(&next).try_fold(0usize, |n, v| {
            n.checked_add(
                v.capacity()
                    .checked_mul(std::mem::size_of::<Option<usize>>())?,
            )
        })?;
        if heap_bytes > available {
            return None;
        }
        let mut deferred = [None; 2];
        for side in 0..2 {
            heads[side].resize(block_counts[side], None);
            next[side].resize(entries[side].len(), None);
            for (i, entry) in entries[side].iter().enumerate() {
                let head = if let Some((block, _, _)) = entry.literal {
                    heads[side].get_mut(block)?
                } else {
                    &mut deferred[side]
                };
                next[side][i] = *head;
                *head = Some(i);
            }
        }
        Some(Self {
            heads,
            next,
            deferred,
            heap_bytes,
        })
    }
}

pub(in crate::diff::assessment) struct LiteralVetoIndex<'c, 's, 'b> {
    constraints: EqualConstraints<'c>,
    sides: [&'s Side<'b>; 2],
    entries: [Vec<Entry<'c>>; 2],
    buckets: Option<Buckets>,
    pub(in crate::diff::assessment) bytes: usize,
}

impl<'c, 's, 'b> LiteralVetoIndex<'c, 's, 'b> {
    pub(in crate::diff::assessment) fn build(
        constraints: EqualConstraints<'c>,
        sides: [&'s Side<'b>; 2],
        available: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        Self::build_mode(constraints, sides, available, remaining, true)
    }

    #[cfg(test)]
    pub(in crate::diff::assessment) fn build_linear(
        constraints: EqualConstraints<'c>,
        sides: [&'s Side<'b>; 2],
        available: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        Self::build_mode(constraints, sides, available, remaining, false)
    }

    fn build_mode(
        constraints: EqualConstraints<'c>,
        sides: [&'s Side<'b>; 2],
        available: usize,
        remaining: &mut usize,
        prepare_buckets: bool,
    ) -> Option<Self> {
        if constraints
            .candidates
            .len()
            .checked_add(constraints.regions.len())?
            < 32
        {
            return None;
        }
        let initial = *remaining;
        let count_work = constraints.candidates.len().checked_add(16)?;
        if count_work > initial / 64 || !charge_work(remaining, count_work) {
            return None;
        }
        let upper = constraints
            .candidates
            .iter()
            .try_fold(constraints.regions.len(), |n, c| {
                n.checked_add(c.change.occurrences.len())
            })?;
        let work = upper.checked_mul(64)?.checked_add(64)?;
        let bytes = upper
            .checked_mul(2)?
            .checked_mul(std::mem::size_of::<Entry<'_>>())?
            .checked_add(std::mem::size_of::<Self>())?;
        let limit = available.min(64 * 1024);
        if bytes > limit
            || count_work.checked_add(work)? > initial / 64
            || !charge_work(remaining, work)
        {
            return None;
        }
        let mut entries = [Vec::new(), Vec::new()];
        for list in &mut entries {
            list.try_reserve_exact(upper).ok()?;
        }
        let bytes = entries
            .iter()
            .try_fold(std::mem::size_of::<Self>(), |n, list| {
                n.checked_add(
                    list.capacity()
                        .checked_mul(std::mem::size_of::<Entry<'_>>())?,
                )
            })?;
        if bytes > limit {
            return None;
        }
        let mut literals = 0usize;
        for (side, list) in entries.iter_mut().enumerate() {
            let spans = constraints
                .candidates
                .iter()
                .flat_map(|c| &c.change.occurrences)
                .filter_map(|o| {
                    if side == 0 {
                        o.old_span.as_ref()
                    } else {
                        o.new_span.as_ref()
                    }
                })
                .map(|span| (None, span))
                .chain(constraints.regions.iter().enumerate().filter_map(|(i, r)| {
                    (if side == 0 {
                        r.old_span.as_ref()
                    } else {
                        r.new_span.as_ref()
                    })
                    .map(|s| (Some(i), s))
                }));
            for (region, span) in spans {
                // This is precisely the literal branch's validated header.
                // Invalid and complex headers keep the original paid predicate.
                let literal = if let [block] = span.blocks.as_slice()
                    && span.separator.is_none()
                    && let Some(&index) = sides[side].index.get(block)
                    && sides[side].blocks[index].canonical.unmapped.is_empty()
                    && span.comparable_range.start <= span.comparable_range.end
                    && span.comparable_range.end <= sides[side].canonical[index].len()
                    && span.canonical_range.start == span.comparable_range.start
                    && span.canonical_range.end == span.comparable_range.end
                {
                    Some((
                        index,
                        span.comparable_range.start,
                        span.comparable_range.end,
                    ))
                } else {
                    None
                };
                literals += usize::from(literal.is_some());
                list.push(Entry {
                    span,
                    region,
                    literal,
                });
            }
        }
        let mut result = Self {
            constraints,
            sides,
            entries,
            buckets: None,
            bytes,
        };
        if prepare_buckets {
            // Refuse the optional layout when most headers need fresh projection.
            // The count came from the already paid header validation pass.
            let total = result.entries[0]
                .len()
                .checked_add(result.entries[1].len())?;
            let allowance = (initial / 64).saturating_sub(initial - *remaining);
            if literals >= total.saturating_sub(literals)
                && let Some(buckets) = Buckets::build(
                    &result.entries,
                    sides.map(|side| side.blocks.len()),
                    limit.checked_sub(result.bytes)?,
                    allowance,
                    remaining,
                )
            {
                result.bytes = result.bytes.checked_add(buckets.heap_bytes)?;
                result.buckets = Some(buckets);
            }
        }
        Some(result)
    }

    pub(in crate::diff::assessment) fn clear(
        &self,
        sides: [&Side<'_>; 2],
        constraints: EqualConstraints<'_>,
        excluded: usize,
        selected: &[SourceInterval; 2],
        remaining: &mut usize,
    ) -> Option<bool> {
        if !charge_work(remaining, 16)
            || !std::ptr::eq(self.constraints.candidates, constraints.candidates)
            || !std::ptr::eq(self.constraints.regions, constraints.regions)
            || !std::ptr::eq(self.sides[0], sides[0])
            || !std::ptr::eq(self.sides[1], sides[1])
        {
            return None;
        }
        if let Some(buckets) = &self.buckets {
            for (side, interval) in selected.iter().enumerate() {
                if !charge_work(remaining, 16) {
                    return None;
                }
                let local = buckets.heads[side]
                    .get(interval.block_index)
                    .copied()
                    .flatten();
                for first in [buckets.deferred[side], local] {
                    let mut cursor = first;
                    while let Some(i) = cursor {
                        if !charge_work(remaining, 16) {
                            return None;
                        }
                        let entry = self.entries[side].get(i)?;
                        cursor = *buckets.next[side].get(i)?;
                        if entry.region == Some(excluded) {
                            continue;
                        }
                        let clear = if let Some((_, start, end)) = entry.literal {
                            start == end || interval.end <= start || end <= interval.start
                        } else {
                            ownership_projection_clear(
                                sides[side],
                                entry.span,
                                interval,
                                remaining,
                            )?
                        };
                        if !clear {
                            return Some(false);
                        }
                    }
                }
            }
            return Some(true);
        }
        for (side, interval) in selected.iter().enumerate() {
            for entry in &self.entries[side] {
                if !charge_work(remaining, 8) {
                    return None;
                }
                if entry.region == Some(excluded) {
                    continue;
                }
                let clear = if let Some((block, start, end)) = entry.literal {
                    start == end
                        || block != interval.block_index
                        || interval.end <= start
                        || end <= interval.start
                } else {
                    ownership_projection_clear(sides[side], entry.span, interval, remaining)?
                };
                if !clear {
                    return Some(false);
                }
            }
        }
        Some(true)
    }
}
