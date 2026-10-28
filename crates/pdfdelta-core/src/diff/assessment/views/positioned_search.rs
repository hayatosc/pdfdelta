//! Paid occurrence indexing for one immutable positioned view population.
//!
//! The full population is retained, including unknown and deny-only metadata.
//! Raw-source augmentation must finish before borrowing the views for a search.

use super::{
    PositionedOccurrences, TokenPostings, View, charge, positioned_occurrences,
    positioned_occurrences_indexed,
};
use crate::Result;

/// A lazily indexed population. Completed scans supply admission receipts;
/// budget stops supply neither negative answers nor reusable receipts.
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

    /// Returns the original count, unknown veto and first match. An empty
    /// needle nominates no correspondence. Other ranges must index the needle.
    pub(super) fn find(
        &mut self,
        needle_view: &View,
        needle_range: &std::ops::Range<usize>,
        self_view: usize,
        remaining: &mut usize,
    ) -> Result<Option<PositionedOccurrences>> {
        if needle_range.is_empty() {
            return Ok(Some(PositionedOccurrences {
                same: 0,
                unknown: false,
                matched: None,
            }));
        }
        if let Some(index) = &self.postings {
            return positioned_occurrences_indexed(
                self.views,
                index,
                needle_view,
                needle_range,
                self_view,
                remaining,
            );
        }
        if self.disabled || self.views.len() < 4 || needle_range.len() < 4 {
            return positioned_occurrences(
                self.views,
                needle_view,
                needle_range,
                self_view,
                remaining,
            );
        }
        // Pay for admission receipts even when this query keeps the scanner.
        if !charge(remaining, 4) {
            return Ok(None);
        }
        if self.calls >= 4 && self.planned.is_none() {
            self.plan(remaining);
        }
        if let Some(bound) = self.planned
            && self.scan_work >= bound
            && bound <= *remaining / 8
        {
            // A refused build is not retried; already spent work stays spent.
            self.disabled = true;
            self.postings = TokenPostings::build(self.views, remaining);
            if let Some(index) = &self.postings {
                return positioned_occurrences_indexed(
                    self.views,
                    index,
                    needle_view,
                    needle_range,
                    self_view,
                    remaining,
                );
            }
        }
        let before = *remaining;
        let result =
            positioned_occurrences(self.views, needle_view, needle_range, self_view, remaining);
        if matches!(result, Ok(Some(_))) {
            let work = before - *remaining;
            self.calls = self.calls.saturating_add(1);
            self.scan_work = self.scan_work.saturating_add(work);
            self.last = Some((needle_range.len(), work));
        }
        result
    }

    /// The completed scanner's charge gives its exact first-token hit count.
    /// A paid header walk computes starts and a conservative build bound.
    fn plan(&mut self, remaining: &mut usize) {
        let headers = self.views.len().saturating_mul(4).saturating_add(16);
        if headers > *remaining / 64 || !charge(remaining, headers) {
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
        for view in self.views {
            let n = view.group.tokens.len();
            total = total.saturating_add(n);
            if n >= length {
                starts = starts.saturating_add(n - length + 1);
                setup = setup.saturating_add(length);
            }
        }
        let hits = work.saturating_sub(setup).saturating_sub(starts) / length.saturating_add(1);
        let bound = total.saturating_mul(6).saturating_add(2);
        if starts == 0 || hits.saturating_mul(4) >= starts || bound > *remaining / 8 {
            self.disabled = true;
        } else {
            self.planned = Some(bound);
        }
    }
}

#[cfg(test)]
mod tests;
