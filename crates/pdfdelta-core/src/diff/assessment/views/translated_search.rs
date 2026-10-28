//! Exact translated occurrence search over the complete first-token population.
//!
//! Missing position metadata remains a veto even after an earlier page mismatch.
//! The page-specific index is deliberately unused. Full token and translation
//! checks retain the scanner's order and conditions on every surviving start.

use super::{
    PositionedOccurrences, TokenPostings, View, charge, positioned_block, same_direction,
    translated_occurrences,
};
use crate::{Result, model::Vec2};

/// One immutable view population's paid, lazily built occurrence index.
/// Short or sparse queries keep the scanner. Setup is attempted only after
/// prior scans spent its conservative work bound; no past work is refunded.
pub(super) struct Search<'a> {
    views: &'a [View],
    postings: Option<TokenPostings<'a>>,
    calls: usize,
    scan_work: usize,
    last: Option<(usize, usize)>,
    planned: Option<usize>,
    disabled: bool,
}

impl<'a> Search<'a> {
    pub(super) fn new(views: &'a [View]) -> Self {
        Self {
            views,
            postings: None,
            calls: 0,
            scan_work: 0,
            last: None,
            planned: None,
            disabled: false,
        }
    }

    pub(super) fn find(
        &mut self,
        needle_view: &View,
        needle_range: &std::ops::Range<usize>,
        self_view: usize,
        translation: Vec2,
        remaining: &mut usize,
    ) -> Result<Option<PositionedOccurrences>> {
        let views = self.views;
        if let Some(index) = &self.postings {
            return indexed(
                views,
                index,
                needle_view,
                needle_range,
                self_view,
                translation,
                remaining,
            );
        }
        if self.disabled || views.len() < 4 || needle_range.len() < 4 {
            return translated_occurrences(
                views,
                needle_view,
                needle_range,
                self_view,
                translation,
                remaining,
            );
        }
        // Four units cover attempt-count, old-work and last-query receipts.
        if !charge(remaining, 4) {
            return Ok(None);
        }
        if self.calls >= 4 && self.planned.is_none() {
            self.plan(views, remaining);
        }
        if let Some(bound) = self.planned
            && self.scan_work >= bound
            && bound <= *remaining / 8
        {
            self.disabled = true;
            self.postings = TokenPostings::build(views, remaining);
            if let Some(index) = &self.postings {
                return indexed(
                    views,
                    index,
                    needle_view,
                    needle_range,
                    self_view,
                    translation,
                    remaining,
                );
            }
        }
        let before = *remaining;
        let result = translated_occurrences(
            views,
            needle_view,
            needle_range,
            self_view,
            translation,
            remaining,
        );
        let work = before - *remaining;
        self.calls = self.calls.saturating_add(1);
        self.scan_work = self.scan_work.saturating_add(work);
        self.last = Some((needle_range.len(), work));
        result
    }

    /// Six units per token bound the existing base-plus-page builder. The
    /// paid header walk also derives the previous scanner's exact first-token
    /// hit count; dense hits cannot amortize the index and disable this route.
    fn plan(&mut self, views: &[View], remaining: &mut usize) {
        let headers = views.len().saturating_mul(4).saturating_add(16);
        if headers > *remaining / 64 {
            self.disabled = true;
            return;
        }
        if !charge(remaining, headers) {
            self.disabled = true;
            return;
        }
        let Some((length, work)) = self.last else {
            self.disabled = true;
            return;
        };
        let mut total = 0usize;
        let mut starts = 0usize;
        let mut setup = 0usize;
        for view in views {
            let n = view.group.tokens.len();
            total = total.saturating_add(n);
            if n >= length {
                starts = starts.saturating_add(n - length + 1);
                setup = setup.saturating_add(length);
            }
        }
        let hits = work.saturating_sub(setup).saturating_sub(starts) / (length.saturating_add(1));
        let bound = total.saturating_mul(6).saturating_add(2);
        if starts == 0 || hits.saturating_mul(4) >= starts || bound > *remaining / 8 {
            self.disabled = true;
        } else {
            self.planned = Some(bound);
        }
    }
}

pub(super) fn indexed(
    views: &[View],
    postings: &TokenPostings<'_>,
    needle_view: &View,
    needle_range: &std::ops::Range<usize>,
    self_view: usize,
    translation: crate::model::Vec2,
    remaining: &mut usize,
) -> Result<Option<PositionedOccurrences>> {
    let needle = &needle_view.group.tokens[needle_range.clone()];
    let mut result = PositionedOccurrences {
        same: 0,
        unknown: false,
        matched: None,
    };
    let Some(first) = needle.first() else {
        return Ok(Some(result));
    };
    let Some(starts) = postings.map.get(first) else {
        return Ok(Some(result));
    };
    if !charge(remaining, starts.len().saturating_add(1)) {
        return Ok(None);
    }
    let mut last_view = None;
    for &(view_index, start) in starts {
        let view_index = view_index as usize;
        let start = start as usize;
        let view = &views[view_index];
        let tokens = &view.group.tokens;
        if needle.len() > tokens.len() || start > tokens.len() - needle.len() {
            continue;
        }
        if last_view != Some(view_index) {
            last_view = Some(view_index);
            if !charge(remaining, needle.len()) {
                return Ok(None);
            }
        }
        if !charge(remaining, needle.len().saturating_add(1)) {
            return Ok(None);
        }
        if &tokens[start..start + needle.len()] != needle {
            continue;
        }
        if view_index == self_view && start == needle_range.start {
            continue;
        }
        let mut same = true;
        let mut complete = true;
        for offset in 0..needle.len() {
            if let (Some(position), Some(page), Some(needle_position), Some(needle_page)) = (
                view.token_positions[start + offset],
                view.token_pages[start + offset],
                needle_view.token_positions[needle_range.start + offset],
                needle_view.token_pages[needle_range.start + offset],
            ) {
                if page != needle_page || !same_direction(position, needle_position) {
                    same = false;
                    continue;
                }
                let position = position.baseline();
                let needle_position = needle_position.baseline();
                let dx = position.x - needle_position.x;
                let dy = position.y - needle_position.y;
                if !dx.is_finite()
                    || !dy.is_finite()
                    || dx.to_bits() != translation.x.to_bits()
                    || dy.to_bits() != translation.y.to_bits()
                {
                    same = false;
                }
            } else {
                complete = false;
                break;
            }
        }
        if !complete {
            result.unknown = true;
        } else if same {
            result.same = result.same.saturating_add(1).min(2);
            if result.matched.is_none() {
                let end = start + needle.len();
                let whole = view
                    .block_ranges
                    .iter()
                    .position(|range| range.start == start && range.end == end)
                    .is_some_and(|block| positioned_block(view, block));
                result.matched = Some((view_index, start..end, whole));
            }
        }
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests;
