//! Paid immutable singleton projections; mutable ownership is never cached.
use super::{EqualConstraints, Side, SourceInterval, charge_work, ownership_projection_clear};

struct Entry<'a> {
    span: &'a super::super::TextSpan,
    region: Option<usize>,
    literal: Option<(usize, usize, usize)>,
}

pub(in crate::diff::assessment) struct LiteralVetoIndex<'c, 's, 'b> {
    constraints: EqualConstraints<'c>,
    sides: [&'s Side<'b>; 2],
    entries: [Vec<Entry<'c>>; 2],
    pub(in crate::diff::assessment) bytes: usize,
}

impl<'c, 's, 'b> LiteralVetoIndex<'c, 's, 'b> {
    pub(in crate::diff::assessment) fn build(
        constraints: EqualConstraints<'c>,
        sides: [&'s Side<'b>; 2],
        available: usize,
        remaining: &mut usize,
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
                list.push(Entry {
                    span,
                    region,
                    literal,
                });
            }
        }
        Some(Self {
            constraints,
            sides,
            entries,
            bytes,
        })
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
