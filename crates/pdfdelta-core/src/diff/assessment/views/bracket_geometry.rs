use super::{BaselineBounds, Side, View, charge, reference_bounds};
use std::ops::Range;

/// Completed geometry for one immutable block-to-view/range population.
///
/// Only the fixed block index is accepted by queries. Source sides, views and
/// exact ranges cannot be replaced while results are retained. Unknown geometry
/// remains a hold; incomplete calculations are never retained. Each query pays
/// six units for fixed-map/slot access, and setup pays for every initialized
/// slot. Refused setup keeps the original scan without refunding spent work.
/// Each side retains at most 16 MiB, including actual vector capacity and header.
pub(super) struct SideGeometry<'a> {
    side: &'a Side<'a>,
    views: &'a [View],
    view_of_block: &'a [usize],
    range_of_block: &'a [Option<Range<usize>>],
    slots: Vec<Option<Option<BaselineBounds>>>,
    attempted: bool,
}

impl<'a> SideGeometry<'a> {
    pub(super) fn new(
        side: &'a Side<'a>,
        views: &'a [View],
        view_of_block: &'a [usize],
        range_of_block: &'a [Option<Range<usize>>],
    ) -> Self {
        Self {
            side,
            views,
            view_of_block,
            range_of_block,
            slots: Vec::new(),
            attempted: false,
        }
    }

    pub(super) fn side(&self) -> &'a Side<'a> {
        self.side
    }

    pub(super) fn view_map(&self) -> &'a [usize] {
        self.view_of_block
    }

    pub(super) fn ranges(&self) -> &'a [Option<Range<usize>>] {
        self.range_of_block
    }

    fn prepare(&mut self, remaining: &mut usize) {
        if self.attempted {
            return;
        }
        self.attempted = true;
        let count = self.side.blocks.len();
        let bytes = count
            .checked_mul(std::mem::size_of::<Option<Option<BaselineBounds>>>())
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()));
        if bytes.is_none_or(|bytes| bytes > 16 * 1024 * 1024) || count > *remaining {
            return;
        }
        if !charge(remaining, count) || self.slots.try_reserve_exact(count).is_err() {
            return;
        }
        let retained = self
            .slots
            .capacity()
            .checked_mul(std::mem::size_of::<Option<Option<BaselineBounds>>>())
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()));
        if retained.is_none_or(|bytes| bytes > 16 * 1024 * 1024) {
            self.slots = Vec::new();
            return;
        }
        self.slots.resize(count, None);
    }

    pub(super) fn get(
        &mut self,
        block: usize,
        remaining: &mut usize,
    ) -> Option<Option<BaselineBounds>> {
        if !charge(remaining, 6) {
            return None;
        }
        self.prepare(remaining);
        if let Some(Some(bounds)) = self.slots.get(block) {
            return Some(*bounds);
        }
        let (Some(&view), Some(Some(range))) = (
            self.view_of_block.get(block),
            self.range_of_block.get(block),
        ) else {
            return Some(None);
        };
        let Some(view) = self.views.get(view) else {
            return Some(None);
        };
        let bounds = reference_bounds(self.side, view, range, block, remaining)?;
        if let Some(slot) = self.slots.get_mut(block) {
            *slot = Some(bounds);
        }
        Some(bounds)
    }
}
