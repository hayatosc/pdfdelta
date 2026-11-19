//! Detection of repeated page furniture.
//!
//! Headers, footers, and page numbers repeat on many pages at the same
//! relative vertical position and differ only in digit runs. Reporting them as
//! content changes buries real edits, so they are removed from the main token
//! stream and diffed separately by their digit-masked text.
//!
//! Positions are relative to the vertical extent of the visible content on each
//! page, not to absolute points: the neutral document facade does not expose
//! page boxes. A line near the top of that extent is still a header, and a line
//! near the bottom is still a footer.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use pdfdelta_core::model::PageId;

use crate::{FlowStream, FlowToken};

/// Fraction of a page's content extent that counts as near the top or bottom.
const EDGE_FRACTION: f64 = 0.12;

/// Maximum difference in relative position for lines to count as the same spot.
const POSITION_TOLERANCE: f64 = 0.12;

/// Fraction of pages on which a furniture line must repeat.
const PAGE_FRACTION: f64 = 0.2;

/// Minimum number of pages on which a furniture line must repeat.
const MIN_PAGES: usize = 3;

/// One repeated furniture line of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FurnitureLine {
    /// Line text with every digit run replaced by `0`.
    pub text: String,
    /// Pages on which the masked text occurs, sorted and deduplicated.
    pub pages: Vec<PageId>,
    /// Number of line occurrences, including several on one page.
    pub occurrences: usize,
}

/// Direction of one furniture change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FurnitureKind {
    /// The masked furniture text only occurs on the new side.
    Added,
    /// The masked furniture text only occurs on the old side.
    Removed,
}

/// One furniture line added or removed between revisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FurnitureChange {
    /// Direction of the change.
    pub kind: FurnitureKind,
    /// Masked furniture text.
    pub text: String,
    /// Pages on the old side, empty for additions.
    pub old_pages: Vec<PageId>,
    /// Pages on the new side, empty for removals.
    pub new_pages: Vec<PageId>,
}

/// Splits repeated furniture lines out of `tokens`.
pub(crate) fn split(tokens: Vec<FlowToken>) -> (Vec<FlowToken>, Vec<FurnitureLine>) {
    let (records, selection) = select_furniture(&tokens, &BTreeMap::new(), &BTreeSet::new());
    apply_selection(tokens, &records, &selection)
}

/// Splits furniture jointly over both documents of one comparison.
///
/// A masked line that qualifies as furniture on either side is furniture on
/// both sides, so adding or removing one page cannot turn an untouched page's
/// repeated edge line into content on one side and furniture on the other.
/// Membership is by masked line text. Alphabetic lines, and numeric lines
/// selected at any edge position on either side, are removed at every
/// line-level occurrence of the joined text. Inline text that merely contains
/// the same words is part of a longer line and is not matched.
///
/// Selecting one cluster per side and then applying the union of those clusters
/// to both sides keeps the decision independent of which side's page count
/// crossed the furniture threshold: a deleted page can change the cluster
/// statistics on one side without turning that side's edge lines into content.
pub(crate) fn split_pair(
    old: FlowStream,
    new: FlowStream,
    old_bands: &BTreeMap<u32, Vec<(f64, f64)>>,
    new_bands: &BTreeMap<u32, Vec<(f64, f64)>>,
) -> (FlowStream, FlowStream) {
    let empty = BTreeSet::new();
    let (old_records, mut old_selection) = select_furniture(&old.tokens, old_bands, &empty);
    let (new_records, mut new_selection) = select_furniture(&new.tokens, new_bands, &empty);
    let union = old_selection
        .keys()
        .chain(new_selection.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    // Qualifying a text on one side must not turn its edge occurrences on the
    // other side into content. Re-select each side with the page threshold
    // waived for the joint texts, exactly as single-side planning would for a
    // text the other side qualified.
    let (_, old_extra) = select_furniture(&old.tokens, old_bands, &union);
    let (_, new_extra) = select_furniture(&new.tokens, new_bands, &union);
    merge_selection(&mut old_selection, old_extra);
    merge_selection(&mut new_selection, new_extra);
    let joint = join_selections(old_selection, new_selection);
    let (old_tokens, old_furniture) = apply_selection(old.tokens, &old_records, &joint);
    let (new_tokens, new_furniture) = apply_selection(new.tokens, &new_records, &joint);
    (
        FlowStream {
            tokens: old_tokens,
            furniture: old_furniture,
            conservation: old.conservation,
        },
        FlowStream {
            tokens: new_tokens,
            furniture: new_furniture,
            conservation: new.conservation,
        },
    )
}

/// One masked text's selected removal positions.
struct SelectedText {
    /// Remove every line-level occurrence, regardless of relative position.
    force_all: bool,
    /// Relative-position ranges (inclusive) selected on either side.
    ranges: Vec<(f64, f64)>,
}

impl SelectedText {
    fn covers(&self, relative: f64) -> bool {
        self.force_all
            || self
                .ranges
                .iter()
                .any(|(minimum, maximum)| relative >= *minimum && relative <= *maximum)
    }
}

/// Adds the ranges of `extra` to `selection`.
fn merge_selection(
    selection: &mut BTreeMap<String, SelectedText>,
    extra: BTreeMap<String, SelectedText>,
) {
    for (text, selected) in extra {
        let entry = selection.entry(text).or_insert_with(|| SelectedText {
            force_all: false,
            ranges: Vec::new(),
        });
        entry.force_all |= selected.force_all;
        entry.ranges.extend(selected.ranges);
    }
}

/// Combines both sides' selections into one symmetric removal plan.
///
/// Alphabetic texts force every occurrence on both sides. Numeric texts keep
/// the union of the edge clusters selected on either side, so a side whose own
/// page count no longer meets the qualification threshold still removes the
/// same lines as the side that does.
fn join_selections(
    old: BTreeMap<String, SelectedText>,
    new: BTreeMap<String, SelectedText>,
) -> BTreeMap<String, SelectedText> {
    let mut joint: BTreeMap<String, SelectedText> = BTreeMap::new();
    for selection in [old, new] {
        for (text, selected) in selection {
            let entry = joint.entry(text).or_insert_with(|| SelectedText {
                force_all: false,
                ranges: Vec::new(),
            });
            entry.force_all |= selected.force_all;
            entry.ranges.extend(selected.ranges);
        }
    }
    for (text, selected) in &mut joint {
        if selected.force_all || text.chars().any(char::is_alphabetic) {
            selected.force_all = true;
            selected.ranges.clear();
            continue;
        }
        selected
            .ranges
            .sort_by(|left, right| left.0.total_cmp(&right.0));
        let mut merged: Vec<(f64, f64)> = Vec::with_capacity(selected.ranges.len());
        for (minimum, maximum) in selected.ranges.drain(..) {
            if let Some(last) = merged.last_mut()
                && minimum <= last.1 + f64::EPSILON
            {
                last.1 = last.1.max(maximum);
            } else {
                merged.push((minimum, maximum));
            }
        }
        selected.ranges = merged;
    }
    joint
}

/// Selects one edge cluster per repeated masked line of one document side.
///
/// The page-count threshold uses this side's page count. A cluster must sit
/// near the top or bottom of its page and must not span more than
/// [`POSITION_TOLERANCE`] of the page extent, which is what separates a running
/// header from a body line that happens to repeat.
fn select_furniture(
    tokens: &[FlowToken],
    bands: &BTreeMap<u32, Vec<(f64, f64)>>,
    extra_texts: &BTreeSet<String>,
) -> (Vec<LineRecord>, BTreeMap<String, SelectedText>) {
    let records = line_records(tokens, bands);
    let total_pages = tokens
        .iter()
        .map(|token| token.page)
        .collect::<BTreeSet<_>>()
        .len();
    let required = ((total_pages as f64) * PAGE_FRACTION)
        .ceil()
        .max(MIN_PAGES as f64) as usize;
    let mut by_text: BTreeMap<&str, BTreeMap<i64, Vec<usize>>> = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        let bucket = (record.relative * 20.0).round() as i64;
        by_text
            .entry(record.masked.as_str())
            .or_default()
            .entry(bucket)
            .or_default()
            .push(index);
    }
    let mut selected = BTreeMap::new();
    for (text, buckets) in by_text {
        // Several clusters can qualify equally often (for example the same
        // running title at the top on some pages and at the bottom on
        // others). Pick by a total order so the result never depends on map
        // iteration: more distinct pages, then more occurrences, then the
        // upper position, then the earliest occurrence.
        let extra = extra_texts.contains(text);
        let mut best: Option<(ClusterKey, &Vec<usize>)> = None;
        for (&bucket, found) in &buckets {
            let pages = found
                .iter()
                .map(|&index| records[index].page)
                .collect::<BTreeSet<_>>();
            if pages.len() < required && !extra {
                continue;
            }
            let mut minimum = f64::INFINITY;
            let mut maximum = f64::NEG_INFINITY;
            for &index in found {
                let relative = records[index].relative;
                minimum = minimum.min(relative);
                maximum = maximum.max(relative);
            }
            let near_edge = minimum <= EDGE_FRACTION || maximum >= 1.0 - EDGE_FRACTION;
            if !near_edge || maximum - minimum > POSITION_TOLERANCE {
                continue;
            }
            let first = found.first().copied().unwrap_or(usize::MAX);
            let key = (
                pages.len(),
                found.len(),
                std::cmp::Reverse(bucket),
                std::cmp::Reverse(first),
            );
            if best.as_ref().is_none_or(|(best_key, _)| key > *best_key) {
                best = Some((key, found));
            }
        }
        let Some((_, found)) = best else {
            continue;
        };
        let (minimum, maximum) = found.iter().fold(
            (f64::INFINITY, f64::NEG_INFINITY),
            |(minimum, maximum), &index| {
                let relative = records[index].relative;
                (minimum.min(relative), maximum.max(relative))
            },
        );
        selected.insert(
            text.to_owned(),
            SelectedText {
                force_all: false,
                ranges: vec![(minimum, maximum)],
            },
        );
    }
    (records, selected)
}

/// Builds one record per reconstructed line.
///
/// A line on a page split into stacked bands uses its band's vertical extent,
/// so the seam of a merged page is treated like the page edge it came from.
fn line_records(tokens: &[FlowToken], bands: &BTreeMap<u32, Vec<(f64, f64)>>) -> Vec<LineRecord> {
    let lines = collect_lines(tokens);
    let extents = banded_extents(tokens, bands);
    let mut records = Vec::with_capacity(lines.len());
    for line in &lines {
        let page = tokens[line.start].page;
        let band = band_index(bands.get(&page.0), &tokens[line.start]);
        let Some(&(min_y, max_y)) = extents.get(&(page, band)) else {
            continue;
        };
        let height = max_y - min_y;
        if height <= f64::EPSILON {
            continue;
        }
        let Some(center) = line_center(&tokens[line.start..=line.end]) else {
            continue;
        };
        let relative = ((center - min_y) / height).clamp(0.0, 1.0);
        let masked = mask_digits(&join_text(&tokens[line.start..=line.end]));
        records.push(LineRecord {
            start: line.start,
            end: line.end,
            page,
            masked,
            relative,
        });
    }
    records
}

/// Removes the selected records and reports the removed furniture families.
fn apply_selection(
    tokens: Vec<FlowToken>,
    records: &[LineRecord],
    selection: &BTreeMap<String, SelectedText>,
) -> (Vec<FlowToken>, Vec<FurnitureLine>) {
    let mut removed = vec![false; tokens.len()];
    let mut removed_indices = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let selected = selection
            .get(record.masked.as_str())
            .is_some_and(|selected| selected.covers(record.relative));
        if !selected {
            continue;
        }
        removed_indices.push(index);
        for slot in &mut removed[record.start..=record.end] {
            *slot = true;
        }
    }
    if removed_indices.is_empty() {
        return (tokens, Vec::new());
    }
    let mut by_text: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for index in removed_indices {
        by_text
            .entry(records[index].masked.as_str())
            .or_default()
            .push(index);
    }
    let mut furniture = Vec::new();
    for indices in by_text.into_values() {
        for (text, group) in furniture_groups(&tokens, records, &indices) {
            let pages = group
                .iter()
                .map(|&index| records[index].page)
                .collect::<BTreeSet<_>>();
            furniture.push(FurnitureLine {
                text,
                pages: pages.into_iter().collect(),
                occurrences: group.len(),
            });
        }
    }
    apply_plan(tokens, removed, furniture)
}

/// Groups furniture occurrences by digit-run lengths and builds each text.
///
/// Lines with the same full digit mask can still be different furniture
/// families: a running header may contain a constant form code and a constant
/// year, while a page number of another length shares the same mask. Grouping
/// by digit-run lengths separates those families. Within one family, a run that
/// changes between occurrences is masked to `0`; a constant run stays literal,
/// so an edit of a year or form number becomes a furniture change instead of
/// disappearing behind the mask.
fn furniture_groups(
    tokens: &[FlowToken],
    records: &[LineRecord],
    indices: &[usize],
) -> Vec<(String, Vec<usize>)> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &index in indices {
        groups
            .entry(line_signature(tokens, &records[index]))
            .or_default()
            .push(index);
    }
    groups
        .into_values()
        .map(|group| {
            let text = furniture_line_text(tokens, records, &group);
            (text, group)
        })
        .collect()
}

/// The digit-run lengths of a line, used to separate furniture families.
fn line_signature(tokens: &[FlowToken], record: &LineRecord) -> String {
    tokens[record.start..=record.end]
        .iter()
        .map(|token| {
            split_digit_runs(&token.text)
                .iter()
                .filter(|(is_digit, _)| *is_digit)
                .map(|(_, run)| run.len().to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Builds one furniture family's text, masking only varying digit runs.
fn furniture_line_text(tokens: &[FlowToken], records: &[LineRecord], indices: &[usize]) -> String {
    let Some(&first) = indices.first() else {
        return String::new();
    };
    let width = records[first].end - records[first].start + 1;
    let mut words = Vec::with_capacity(width);
    for offset in 0..width {
        let texts = indices
            .iter()
            .map(|&index| tokens[records[index].start + offset].text.as_str())
            .collect::<Vec<_>>();
        if texts.windows(2).all(|pair| pair[0] == pair[1]) {
            words.push(texts[0].to_owned());
            continue;
        }
        let runs = split_digit_runs(texts[0]);
        let mut masked = String::new();
        for (run_index, (is_digit, run)) in runs.iter().enumerate() {
            let varies = texts.iter().any(|text| {
                split_digit_runs(text)
                    .get(run_index)
                    .is_none_or(|(other_is_digit, other)| {
                        other_is_digit != is_digit || other != run
                    })
            });
            if *is_digit && varies {
                masked.push('0');
            } else {
                masked.push_str(run);
            }
        }
        words.push(masked);
    }
    words.join(" ")
}

/// Splits a token into alternating digit and non-digit runs.
fn split_digit_runs(text: &str) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    for scalar in text.chars() {
        let digit = scalar.is_ascii_digit();
        if let Some((previous_digit, run)) = runs.last_mut()
            && *previous_digit == digit
        {
            run.push(scalar);
            continue;
        }
        runs.push((digit, scalar.to_string()));
    }
    runs
}

/// Removes planned furniture tokens and keeps the trailing break evidence.
fn apply_plan(
    tokens: Vec<FlowToken>,
    removed: Vec<bool>,
    mut furniture: Vec<FurnitureLine>,
) -> (Vec<FlowToken>, Vec<FurnitureLine>) {
    if furniture.is_empty() {
        return (tokens, Vec::new());
    }
    furniture.sort_unstable_by(|left, right| left.text.cmp(&right.text));
    let mut kept = Vec::with_capacity(tokens.len());
    let mut kept_last = None;
    for (index, token) in tokens.iter().enumerate() {
        if removed[index] {
            continue;
        }
        kept.push(token.clone());
        kept_last = Some(kept.len() - 1);
    }
    if let Some(last) = kept_last {
        kept[last].line_break_after |= removed[tokens.len() - 1];
    }
    (kept, furniture)
}

struct LineSpan {
    start: usize,
    end: usize,
}

fn collect_lines(tokens: &[FlowToken]) -> Vec<LineSpan> {
    let mut lines = Vec::new();
    let mut start = 0;
    for index in 0..tokens.len() {
        let next = tokens.get(index + 1);
        let break_here = tokens[index].line_break_after
            || next.is_none()
            || next.is_some_and(|token| token.page != tokens[index].page);
        if break_here {
            lines.push(LineSpan { start, end: index });
            start = index + 1;
        }
    }
    lines
}

/// Vertical token extents keyed by page and stacked-band index.
///
/// Pages without bands use index zero, so their behaviour is unchanged.
fn banded_extents(
    tokens: &[FlowToken],
    bands: &BTreeMap<u32, Vec<(f64, f64)>>,
) -> BTreeMap<(PageId, usize), (f64, f64)> {
    let mut extents: BTreeMap<(PageId, usize), (f64, f64)> = BTreeMap::new();
    for token in tokens {
        let Some(bounds) = token.bounds else {
            continue;
        };
        let center = f64::midpoint(bounds.min.y, bounds.max.y);
        let band = band_index(bands.get(&token.page.0), token);
        let entry = extents
            .entry((token.page, band))
            .or_insert((f64::INFINITY, f64::NEG_INFINITY));
        entry.0 = entry.0.min(center);
        entry.1 = entry.1.max(center);
    }
    extents
}

/// Index of the band containing a token's vertical center.
fn band_index(bands: Option<&Vec<(f64, f64)>>, token: &FlowToken) -> usize {
    let Some(bands) = bands else {
        return 0;
    };
    let y = token.bounds.map_or(f64::INFINITY, |bounds| {
        f64::midpoint(bounds.min.y, bounds.max.y)
    });
    bands
        .iter()
        .position(|(bottom, _top)| y >= *bottom)
        .unwrap_or(bands.len().saturating_sub(1))
}

fn line_center(tokens: &[FlowToken]) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0;
    for token in tokens {
        if let Some(bounds) = token.bounds {
            sum += f64::midpoint(bounds.min.y, bounds.max.y);
            count += 1;
        }
    }
    (count > 0).then(|| sum / count as f64)
}

fn join_text(tokens: &[FlowToken]) -> String {
    tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn mask_digits(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut in_digits = false;
    for scalar in text.chars() {
        if scalar.is_ascii_digit() {
            if !in_digits {
                masked.push('0');
                in_digits = true;
            }
        } else {
            masked.push(scalar);
            in_digits = false;
        }
    }
    masked
}

/// Compares the furniture streams of two revisions.
#[must_use]
pub fn furniture_changes(old: &FlowStream, new: &FlowStream) -> Vec<FurnitureChange> {
    let old_text: HashMap<&str, &FurnitureLine> = old
        .furniture
        .iter()
        .map(|line| (line.text.as_str(), line))
        .collect();
    let new_text: HashMap<&str, &FurnitureLine> = new
        .furniture
        .iter()
        .map(|line| (line.text.as_str(), line))
        .collect();
    let mut changes = Vec::new();
    for line in &old.furniture {
        if !new_text.contains_key(line.text.as_str()) {
            changes.push(FurnitureChange {
                kind: FurnitureKind::Removed,
                text: line.text.clone(),
                old_pages: line.pages.clone(),
                new_pages: Vec::new(),
            });
        }
    }
    for line in &new.furniture {
        if !old_text.contains_key(line.text.as_str()) {
            changes.push(FurnitureChange {
                kind: FurnitureKind::Added,
                text: line.text.clone(),
                old_pages: Vec::new(),
                new_pages: line.pages.clone(),
            });
        }
    }
    changes.sort_unstable_by(|left, right| {
        left.text
            .cmp(&right.text)
            .then_with(|| left.kind.cmp(&right.kind))
    });
    changes
}

/// Total ordering for choosing one furniture cluster on ties.
type ClusterKey = (
    usize,
    usize,
    std::cmp::Reverse<i64>,
    std::cmp::Reverse<usize>,
);

struct LineRecord {
    start: usize,
    end: usize,
    page: PageId,
    masked: String,
    relative: f64,
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::model::{PageId, Rect, Vec2};

    use super::*;

    fn token(page: u32, text: &str, y: f64, line_break: bool) -> FlowToken {
        FlowToken {
            text: text.to_owned(),
            page: PageId(page),
            bounds: Some(Rect {
                min: Vec2 { x: 0.0, y },
                max: Vec2 {
                    x: 10.0,
                    y: y + 10.0,
                },
            }),
            block: 0,
            uncertain: false,
            line_break_after: line_break,
        }
    }

    fn page_tokens(page: u32, header: bool) -> Vec<FlowToken> {
        let mut tokens = Vec::new();
        if header {
            tokens.push(token(page, "Running", 0.0, false));
            tokens.push(token(page, "Header", 0.0, false));
            tokens.push(token(page, "0", 0.0, true));
        }
        // Unique body text keeps the fixture from creating unrelated
        // repeated lines. Digits are avoided because furniture masking
        // collapses every digit run to `0`.
        let name = ["alpha", "beta", "gamma", "delta", "epsilon"]
            [usize::try_from(page).expect("test page fits usize") % 5];
        tokens.push(token(page, &format!("{name}body"), 50.0, true));
        tokens.push(token(page, &format!("{name}text"), 50.0, true));
        tokens
    }

    fn stream(pages: u32, header_pages: &[u32]) -> FlowStream {
        let mut tokens = Vec::new();
        for page in 0..pages {
            tokens.extend(page_tokens(page, header_pages.contains(&page)));
        }
        FlowStream {
            tokens,
            furniture: Vec::new(),
            conservation: crate::Conservation::default(),
        }
    }

    #[test]
    fn joint_split_unions_furniture_membership() {
        // The header repeats on three of five pages on the old side and on
        // only two on the new side. Individually only the old side would
        // qualify it; jointly both sides must remove it.
        let old = stream(5, &[0, 1, 2]);
        let new = stream(5, &[0, 1]);
        assert_eq!(split(old.tokens.clone()).1.len(), 1);
        assert_eq!(split(new.tokens.clone()).1.len(), 0);
        let (old, new) = split_pair(old, new, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(old.furniture.len(), 1);
        assert_eq!(new.furniture.len(), 1);
        assert!(
            !old.tokens.iter().any(|token| token.text == "Running"),
            "the old side must remove the joint furniture"
        );
        assert!(
            !new.tokens.iter().any(|token| token.text == "Running"),
            "the new side must remove the joint furniture"
        );
    }

    fn marker_stream(top_pages: &[u32], bottom_pages: &[u32]) -> FlowStream {
        let names = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"];
        let mut tokens = Vec::new();
        for (page, name) in names.into_iter().enumerate() {
            let page = u32::try_from(page).expect("test page fits u32");
            tokens.push(token(page, &format!("{name}top"), 0.0, true));
            tokens.push(token(page, &format!("{name}body"), 50.0, true));
            if top_pages.contains(&page) {
                tokens.push(token(page, "Marker", 5.0, true));
            }
            if bottom_pages.contains(&page) {
                tokens.push(token(page, "Marker", 45.0, true));
            }
        }
        FlowStream {
            tokens,
            furniture: Vec::new(),
            conservation: crate::Conservation::default(),
        }
    }

    #[test]
    fn joint_split_unions_the_selected_position_buckets() {
        // The old side qualifies the marker only near the top; the new side
        // qualifies it more often near the bottom. Each side's chosen cluster
        // is therefore a different edge position. The joint decision must
        // remove both positions from both documents, so deleting a page cannot
        // turn one side's edge marker into content.
        let old = marker_stream(&[0, 1, 2, 3, 4], &[5, 6]);
        let new = marker_stream(&[0, 1, 2], &[3, 4, 5, 6]);
        let (old, new) = split_pair(old, new, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(old.furniture.len(), 1);
        assert_eq!(new.furniture.len(), 1);
        assert!(
            !old.tokens.iter().any(|token| token.text == "Marker"),
            "the old side must remove both joined positions"
        );
        assert!(
            !new.tokens.iter().any(|token| token.text == "Marker"),
            "the new side must remove both joined positions"
        );
    }

    #[test]
    fn joint_split_removes_the_same_line_at_a_moved_position() {
        // One side repeats the marker at the page edge; the other side repeats
        // it in the middle of each page (the page-break case after a merge).
        // The line is furniture on the first side, so the joint rule removes
        // every line-level occurrence on both sides.
        let names = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"];
        let build = |y: f64| {
            let mut tokens = Vec::new();
            for (page, name) in names.iter().enumerate() {
                let page = u32::try_from(page).expect("test page fits u32");
                tokens.push(token(page, &format!("{name}top"), 0.0, true));
                tokens.push(token(page, &format!("{name}body"), 50.0, true));
                tokens.push(token(page, "Marker", y, true));
            }
            FlowStream {
                tokens,
                furniture: Vec::new(),
                conservation: crate::Conservation::default(),
            }
        };
        let (old, new) = split_pair(build(5.0), build(25.0), &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(old.furniture.len(), 1);
        assert_eq!(new.furniture.len(), 1);
        assert!(
            !old.tokens.iter().any(|token| token.text == "Marker"),
            "the edge side must remove the joined line"
        );
        assert!(
            !new.tokens.iter().any(|token| token.text == "Marker"),
            "the moved-position side must remove the joined line"
        );
    }

    #[test]
    fn joint_split_keeps_inline_repeats() {
        // A body line repeated on two pages is not near an edge, so waiving
        // the page threshold for the other side's furniture text must not
        // remove it.
        let plain = stream(5, &[]);
        let (old, new) = split_pair(plain.clone(), plain, &BTreeMap::new(), &BTreeMap::new());
        assert!(old.furniture.is_empty());
        assert!(new.furniture.is_empty());
        assert_eq!(old.tokens.len(), new.tokens.len());
    }
}
