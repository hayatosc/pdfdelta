//! Cheap confidence and formatting evidence for a verified flow script.
//!
//! The edit script is always a correct replay. This module adds two
//! orthogonal, cheap annotations:
//!
//! * [`assess_changes`] explains where the chosen script may be ambiguous:
//!   coarse spans, repeated runs, order-uncertain pages, short moves, numeric
//!   runs, and numeric table cells whose neighbouring equal tokens repeat.
//! * [`formatting_changes`] reports matched token runs whose raw text,
//!   effective font size, position, line/page breaks, or block boundaries
//!   differ. Formatting evidence never changes the edit script or the exit
//!   code.
//!
//! Everything here is computed from the token streams and the verified spine;
//! no additional alignment work is performed.

use std::{
    collections::{BTreeSet, HashMap},
    ops::Range,
};

use pdfdelta_core::model::PageId;

use crate::{FlowChange, FlowDiff, FlowStats, FlowToken, intern_tokens};

/// Relative font-size difference that counts as a presentation change.
///
/// The comparison is scale-free: a 10 pt to 10.2 pt change is below 1%, while
/// a 10 pt to 14 pt change is reported. A fixed point threshold would mistake a
/// large heading's small relative change for a body-text change.
pub const FONT_SIZE_RELATIVE_TOLERANCE: f64 = 0.01;

/// Confidence that the chosen script describes the document edit in the way a
/// reader would expect, given the evidence this module can compute cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// No cheap ambiguity signal fired.
    High,
    /// At least one ambiguity signal fired.
    Medium,
    /// A coarse span, a numeric table, or a repeated numeric run fired.
    Low,
}

impl Confidence {
    /// Stable label used by text and JSON reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

/// One cheap reason why a change may be read differently than intended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AmbiguityReason {
    /// A replace covers more than the coarse-span token threshold on one side.
    CoarseSpan,
    /// A boundary run of the changed span also occurs elsewhere on its side.
    RepeatedRun,
    /// The change touches a page whose reading order was uncertain.
    OrderUncertain,
    /// A move has at most two tokens.
    ShortMove,
    /// Every changed token is numeric or numeric punctuation.
    NumericOnly,
    /// A numeric change sits beside a repeated numeric equal token, the shape
    /// produced when a table cell pair is shifted by one cell.
    NumericTable,
}

impl AmbiguityReason {
    /// Stable label used by text and JSON reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CoarseSpan => "coarse_span",
            Self::RepeatedRun => "repeated_run",
            Self::OrderUncertain => "order_uncertain",
            Self::ShortMove => "short_move",
            Self::NumericOnly => "numeric_only",
            Self::NumericTable => "numeric_table",
        }
    }
}

/// Threshold above which a replace is treated as a coarse span.
pub const COARSE_SPAN_TOKENS: usize = 40;

/// Confidence and reasons for one change of [`FlowDiff::changes`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeAssessment {
    /// Overall confidence.
    pub confidence: Confidence,
    /// Reasons in [`AmbiguityReason`] declaration order.
    pub ambiguity: Vec<AmbiguityReason>,
}

/// One formatting-only reason on matched tokens.
///
/// The vocabulary and order are fixed by the formatting contract:
/// normalization, block structure, font size, position, line break, and page
/// break. A flow entry may carry several reasons when the same matched run
/// differs in several dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FormattingReason {
    Normalization,
    BlockStructure,
    FontSize,
    Position,
    LineBreak,
    PageBreak,
}

impl FormattingReason {
    /// Stable label used by text and JSON reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normalization => "normalization",
            Self::BlockStructure => "block_structure",
            Self::FontSize => "font_size",
            Self::Position => "position",
            Self::LineBreak => "line_break",
            Self::PageBreak => "page_break",
        }
    }
}

/// One formatting-only change on matched tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct FormattingChange {
    /// Matched old token range.
    pub old: Range<usize>,
    /// Matched new token range.
    pub new: Range<usize>,
    /// Reasons in [`FormattingReason`] declaration order.
    pub reasons: Vec<FormattingReason>,
    /// Effective old font size when the entry includes a font-size change.
    pub old_size: Option<f64>,
    /// Effective new font size when the entry includes a font-size change.
    pub new_size: Option<f64>,
}

/// Returns whether a verified script selects a detected difference.
///
/// Deletions, insertions, replacements, and moves change the document or its
/// order; a layout-only change does not. This is the single exit-selection
/// predicate used by the CLI, so the policy and the summary cannot diverge.
#[must_use]
pub fn selects_difference(stats: &FlowStats) -> bool {
    stats.delete.changes + stats.insert.changes + stats.replace.changes + stats.moves.changes > 0
}

/// Assesses every change of `diff` once, in change order.
///
/// `old_order_uncertain` and `new_order_uncertain` contain the pages whose
/// reading order the caller marked uncertain. The function makes one pass over
/// each token stream to count only the boundary runs it watches; memory is
/// proportional to the number of changes, not to the number of tokens.
#[must_use]
pub fn assess_changes(
    old: &[FlowToken],
    new: &[FlowToken],
    diff: &FlowDiff,
    old_order_uncertain: &BTreeSet<PageId>,
    new_order_uncertain: &BTreeSet<PageId>,
) -> Vec<ChangeAssessment> {
    let (old_ids, new_ids) = intern_tokens(old, new);
    let numeric_table = numeric_table_candidates(&old_ids, old, &new_ids, new, diff);
    let old_watched = watched_sequences(&old_ids, diff, false, &numeric_table);
    let new_watched = watched_sequences(&new_ids, diff, true, &numeric_table);
    let old_positions = positions_of(&old_ids, &old_watched);
    let new_positions = positions_of(&new_ids, &new_watched);

    diff.changes
        .iter()
        .enumerate()
        .map(|(index, change)| {
            let mut ambiguity = Vec::new();
            let old_range = change.old_range();
            let new_range = change.new_range();
            let old_len = old_range.len();
            let new_len = new_range.len();
            let numeric_only = range_is_numeric(old, old_range.clone())
                && range_is_numeric(new, new_range.clone());
            if matches!(change, FlowChange::Replace { .. })
                && old_len.max(new_len) > COARSE_SPAN_TOKENS
            {
                ambiguity.push(AmbiguityReason::CoarseSpan);
            }
            if matches!(change, FlowChange::Move { .. }) && old_len.min(new_len) <= 2 {
                ambiguity.push(AmbiguityReason::ShortMove);
            }
            if numeric_only {
                ambiguity.push(AmbiguityReason::NumericOnly);
            }
            if touched_uncertain_page(old, old_range.clone(), old_order_uncertain)
                || touched_uncertain_page(new, new_range.clone(), new_order_uncertain)
            {
                ambiguity.push(AmbiguityReason::OrderUncertain);
            }
            if repeated_boundary(
                change,
                old,
                new,
                &old_ids,
                &new_ids,
                &old_positions,
                &new_positions,
            ) {
                ambiguity.push(AmbiguityReason::RepeatedRun);
            }
            let numeric_table_here = numeric_table[index].is_some_and(|(side, id)| {
                let (tokens, positions) = if side {
                    (new, &new_positions)
                } else {
                    (old, &old_positions)
                };
                near_duplicate(tokens, positions, vec![id], change_start(change, side))
            });
            if numeric_table_here {
                ambiguity.push(AmbiguityReason::NumericTable);
            }
            ambiguity.sort_unstable();
            ambiguity.dedup();
            let confidence = if ambiguity.contains(&AmbiguityReason::CoarseSpan)
                || ambiguity.contains(&AmbiguityReason::NumericTable)
                || (ambiguity.contains(&AmbiguityReason::RepeatedRun) && numeric_only)
            {
                Confidence::Low
            } else if ambiguity.is_empty() {
                Confidence::High
            } else {
                Confidence::Medium
            };
            ChangeAssessment {
                confidence,
                ambiguity,
            }
        })
        .collect()
}

/// Token-distance window in which a repeated boundary run makes the chosen
/// pairing ambiguous. Wider contexts are ordinary vocabulary repetition, not a
/// competing local alignment.
const REPEATED_RUN_WINDOW: usize = 16;

/// Watches the boundary sequences of every change on one side.
fn watched_sequences(
    ids: &[u32],
    diff: &FlowDiff,
    new_side: bool,
    numeric_table: &[NumericTableNeighbour],
) -> Vec<Vec<u32>> {
    let mut watched = Vec::new();
    for change in &diff.changes {
        let range = if new_side {
            change.new_range()
        } else {
            change.old_range()
        };
        watched.extend(boundary_sequences(ids, range));
    }
    for candidate in numeric_table.iter().flatten() {
        let (side, id) = *candidate;
        if side == new_side {
            watched.push(vec![id]);
        }
    }
    watched
}

/// Collects a bounded list of positions for every watched sequence.
fn positions_of(ids: &[u32], watched: &[Vec<u32>]) -> HashMap<Vec<u32>, Vec<usize>> {
    let watched: std::collections::HashSet<&[u32]> = watched.iter().map(Vec::as_slice).collect();
    let mut positions: HashMap<Vec<u32>, Vec<usize>> = HashMap::new();
    for position in 0..ids.len() {
        for length in 1..=3 {
            let Some(slice) = ids.get(position..position + length) else {
                break;
            };
            if !watched.contains(slice) {
                continue;
            }
            let entry = positions.entry(slice.to_vec()).or_default();
            if entry.len() < 32 {
                entry.push(position);
            }
        }
    }
    positions
}

fn change_start(change: &FlowChange, new_side: bool) -> usize {
    if new_side {
        change.new_range().start
    } else {
        change.old_range().start
    }
}

/// Returns whether a watched sequence occurs again near `own` in the same block.
fn near_duplicate(
    tokens: &[FlowToken],
    positions: &HashMap<Vec<u32>, Vec<usize>>,
    sequence: Vec<u32>,
    own: usize,
) -> bool {
    let Some(found) = positions.get(&sequence) else {
        return false;
    };
    let Some(own_token) = tokens.get(own) else {
        return false;
    };
    found.iter().any(|position| {
        *position != own
            && position.abs_diff(own) <= REPEATED_RUN_WINDOW
            && tokens
                .get(*position)
                .is_some_and(|token| token.block == own_token.block)
    })
}

/// One candidate numeric-table adjacency: `(side, token_id)` for the repeated
/// equal token beside a numeric change. `None` means no such neighbour.
type NumericTableNeighbour = Option<(bool, u32)>;

fn numeric_table_candidates(
    old_ids: &[u32],
    old: &[FlowToken],
    new_ids: &[u32],
    new: &[FlowToken],
    diff: &FlowDiff,
) -> Vec<NumericTableNeighbour> {
    diff.changes
        .iter()
        .map(|change| {
            let old_range = change.old_range();
            let new_range = change.new_range();
            if !range_is_numeric(old, old_range.clone())
                && !range_is_numeric(new, new_range.clone())
            {
                return None;
            }
            let old_start = change.old_range().start;
            let previous = diff
                .spine
                .partition_point(|(old_index, _)| *old_index < old_start)
                .checked_sub(1);
            let next = diff
                .spine
                .partition_point(|(old_index, _)| *old_index < change.old_range().end);
            for candidate in [previous, Some(next)] {
                let Some(pair) = candidate.and_then(|index| diff.spine.get(index)) else {
                    continue;
                };
                if is_numeric_token(&old[pair.0].text) {
                    return Some((false, old_ids[pair.0]));
                }
                if is_numeric_token(&new[pair.1].text) {
                    return Some((true, new_ids[pair.1]));
                }
            }
            None
        })
        .collect()
}

fn range_is_numeric(tokens: &[FlowToken], range: Range<usize>) -> bool {
    if range.is_empty() {
        return false;
    }
    tokens
        .get(range)
        .is_some_and(|slice| slice.iter().all(|token| is_numeric_token(&token.text)))
}

fn is_numeric_token(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_digit()
                || matches!(
                    character,
                    ',' | '.' | '%' | '-' | '+' | '/' | '(' | ')' | ':'
                )
        })
}

fn touched_uncertain_page(
    tokens: &[FlowToken],
    range: Range<usize>,
    uncertain: &BTreeSet<PageId>,
) -> bool {
    tokens
        .get(range)
        .is_some_and(|slice| slice.iter().any(|token| uncertain.contains(&token.page)))
}

/// Returns the watched boundary sequences of one change side.
///
/// Runs of three or more tokens watch their first and last three-token
/// sequences; shorter runs watch the whole run. Equal sequences are stored
/// once per side by the caller.
fn boundary_sequences(ids: &[u32], range: Range<usize>) -> Vec<Vec<u32>> {
    let Some(slice) = ids.get(range) else {
        return Vec::new();
    };
    if slice.is_empty() {
        return Vec::new();
    }
    if slice.len() <= 3 {
        return vec![slice.to_vec()];
    }
    vec![slice[..3].to_vec(), slice[slice.len() - 3..].to_vec()]
}

fn repeated_boundary(
    change: &FlowChange,
    old: &[FlowToken],
    new: &[FlowToken],
    old_ids: &[u32],
    new_ids: &[u32],
    old_positions: &HashMap<Vec<u32>, Vec<usize>>,
    new_positions: &HashMap<Vec<u32>, Vec<usize>>,
) -> bool {
    for (tokens, ids, positions, range) in [
        (old, old_ids, old_positions, change.old_range()),
        (new, new_ids, new_positions, change.new_range()),
    ] {
        let sequences = boundary_sequences(ids, range.clone());
        for sequence in sequences {
            let own = range.start;
            if near_duplicate(tokens, positions, sequence, own) {
                return true;
            }
        }
        // A longer run also has a last-boundary occurrence at range.end - 3.
        if range.len() > 3 {
            let sequence = ids[range.end - 3..range.end].to_vec();
            if near_duplicate(tokens, positions, sequence, range.end - 3) {
                return true;
            }
        }
    }
    false
}

/// Reports formatting-only evidence on matched token runs.
///
/// The spine is first split at gaps, so an entry never spans a content change.
/// A run is split by the old-side block so a large unchanged document yields
/// bounded entries; adjacent entries with the same reasons, sizes, and pages
/// are coalesced again for presentation. A formatting entry must span at least
/// one matched adjacency: a one-token match is a point left by LCS inside
/// rewritten text, not a formatting span, and is dropped after coalescing.
/// Reasons have these semantics:
///
/// * `normalization` means matched canonical text has different raw text, a
///   different normalization-event mask, or an in-block line-wrap rewrite that
///   raw projection cannot see. A page break between blocks moves no raw text
///   and does not by itself imply normalization;
/// * `font_size` means a shared effective size differs beyond
///   [`FONT_SIZE_RELATIVE_TOLERANCE`]. Each distinct changed size pair is
///   reported once over the whole matched run rather than one entry per
///   changed token;
/// * `position` means the whole run's internal line and page break offsets
///   agree, every token has complete source geometry, directions agree, and at
///   least one line is a nonzero pure translation. A run that moved to another
///   page with unchanged internal offsets is still a pure translation; only a
///   break that moved inside the run suppresses position;
/// * `line_break`/`page_break` mean the corresponding internal break flags
///   differ. A break after the last token of an old block is a group boundary
///   and is not internal evidence;
/// * `block_structure` means the sequence of block boundaries differs after
///   numbering each side's blocks from zero.
///
/// Formatting never changes the edit script or the exit code.
#[must_use]
pub fn formatting_changes(
    old: &[FlowToken],
    new: &[FlowToken],
    spine: &[(usize, usize)],
) -> Vec<FormattingChange> {
    let mut changes = Vec::new();
    let mut start = 0;
    while start < spine.len() {
        let mut end = start + 1;
        while end < spine.len()
            && spine[end].0 == spine[end - 1].0 + 1
            && spine[end].1 == spine[end - 1].1 + 1
        {
            end += 1;
        }
        push_run(&mut changes, old, new, &spine[start..end]);
        start = end;
    }
    coalesce(&mut changes, old, new);
    // A formatting entry must span a matched run, not a single token. A
    // one-token match is a point: LCS can match an isolated common word inside
    // heavily rewritten text, but it provides no line or block span that the
    // formatting contract can describe. The check runs after
    // coalescing so adjacent point matches with the same evidence become one
    // genuine span; only isolated points are dropped. Two endpoints are the
    // structural minimum for an adjacency, not a corpus-tuned pixel or token
    // threshold.
    changes.retain(|change| change.old.end - change.old.start >= 2);
    changes
}

/// Merges adjacent entries with the same evidence into one bounded entry.
///
/// Entries are split at old blocks so that an unchanged document cannot produce
/// one document-sized entry. Those splits are presentation choices, not
/// evidence: two adjacent segments with the same reasons, sizes, and pages
/// describe the same formatting change and are reported once. A page boundary
/// is never merged across, and adjacent entries from different matched runs
/// cannot merge because a content change separates their token ranges.
fn coalesce(changes: &mut Vec<FormattingChange>, old: &[FlowToken], new: &[FlowToken]) {
    let mut merged: Vec<FormattingChange> = Vec::with_capacity(changes.len());
    for change in changes.drain(..) {
        if let Some(last) = merged.last_mut()
            && last.old.end == change.old.start
            && last.new.end == change.new.start
            && mergeable_reasons(
                &last.reasons,
                &change.reasons,
                last.old.end - last.old.start,
                change.old.end - change.old.start,
            )
            && last.old_size == change.old_size
            && last.new_size == change.new_size
            && old[last.old.end - 1].page == old[change.old.start].page
            && new[last.new.end - 1].page == new[change.new.start].page
        {
            last.reasons = merged_reasons(&last.reasons, &change.reasons);
            last.old.end = change.old.end;
            last.new.end = change.new.end;
            continue;
        }
        merged.push(change);
    }
    *changes = merged;
}

fn without_position(reasons: &[FormattingReason]) -> Vec<FormattingReason> {
    reasons
        .iter()
        .copied()
        .filter(|reason| *reason != FormattingReason::Position)
        .collect()
}

fn mergeable_reasons(
    left: &[FormattingReason],
    right: &[FormattingReason],
    left_len: usize,
    right_len: usize,
) -> bool {
    let left_base = without_position(left);
    let right_base = without_position(right);
    if left_base != right_base {
        return false;
    }
    let left_position = left.contains(&FormattingReason::Position);
    let right_position = right.contains(&FormattingReason::Position);
    // A one-token segment is a point. If it only disagrees by carrying
    // position, merge it with its neighbour and drop the point's position
    // rather than letting it split a normalization/font-size span. Two
    // multi-token segments keep separate position evidence.
    left_position == right_position || left_len == 1 || right_len == 1
}

fn merged_reasons(left: &[FormattingReason], right: &[FormattingReason]) -> Vec<FormattingReason> {
    let mut reasons = without_position(left);
    if left.contains(&FormattingReason::Position) && right.contains(&FormattingReason::Position) {
        reasons.push(FormattingReason::Position);
    }
    reasons.sort_unstable();
    reasons.dedup();
    reasons
}

/// Returns the line and page break differences strictly inside one old block.
///
/// Break offsets count only strictly inside one old block; a break after a
/// token that ends an old block is a group boundary. The old block is the
/// grouping reference, so a break at an old block end is not an internal
/// reflow. This keeps the page-break acceptance fixture reporting only the
/// moved lines' position.
fn break_differences(
    old: &[FlowToken],
    new: &[FlowToken],
    pairs: &[(usize, usize)],
) -> (bool, bool) {
    let mut line_break = false;
    let mut page_break = false;
    for (offset, &(old_index, new_index)) in pairs.iter().enumerate() {
        let Some(&(next_old, _)) = pairs.get(offset + 1) else {
            break;
        };
        if old[next_old].block != old[old_index].block {
            continue;
        }
        line_break |= old[old_index].line_break_after != new[new_index].line_break_after;
        page_break |= old[old_index].page_break_after != new[new_index].page_break_after;
    }
    (line_break, page_break)
}

fn push_run(
    changes: &mut Vec<FormattingChange>,
    old: &[FlowToken],
    new: &[FlowToken],
    pairs: &[(usize, usize)],
) {
    if pairs.is_empty() {
        return;
    }
    let block_structure = block_pattern_differs(old, new, pairs);
    // A segment's own internal breaks decide whether it can be position
    // evidence. A break difference in one segment must not suppress position
    // on another segment of the same matched run.
    // Font-size evidence is group-level: the run reports that its effective
    // size signature differs, not which individual token changed. Each distinct
    // changed size pair is therefore recorded once, spanning the complete
    // matched run, instead of emitting one-token entries for the token that
    // changed. A one-token run is dropped by the point rule below.
    let run_old = pairs[0].0..pairs[pairs.len() - 1].0 + 1;
    let run_new = pairs[0].1..pairs[pairs.len() - 1].1 + 1;
    let mut size_pairs: Vec<(u64, u64)> = Vec::new();
    for &(old_index, new_index) in pairs {
        let Some((old_size, new_size)) = font_size_pair(old, new, (old_index, new_index)) else {
            continue;
        };
        if !font_size_differs(old_size, new_size) {
            continue;
        }
        let key = (old_size.to_bits(), new_size.to_bits());
        if !size_pairs.contains(&key) {
            size_pairs.push(key);
            changes.push(FormattingChange {
                old: run_old.clone(),
                new: run_new.clone(),
                reasons: vec![FormattingReason::FontSize],
                old_size: Some(old_size),
                new_size: Some(new_size),
            });
        }
    }
    let mut segment_start = 0;
    while segment_start < pairs.len() {
        let old_block = old[pairs[segment_start].0].block;
        let mut segment_end = segment_start + 1;
        while segment_end < pairs.len() && old[pairs[segment_end].0].block == old_block {
            segment_end += 1;
        }
        let segment = &pairs[segment_start..segment_end];
        let mut reasons = Vec::new();
        let (line_break, page_break) = break_differences(old, new, segment);
        // Normalization is reported when the raw token sequence differs. An
        // in-block line wrap replaces a raw gap with a line boundary that the
        // flow projection cannot see, so a line-break difference carries the
        // same normalization evidence. A page break between blocks moves no
        // raw text, so it carries only the page-break reason.
        if raw_differs_in_segment(old, new, segment)
            || normalization_differs_in_segment(old, new, segment)
            || line_break
        {
            reasons.push(FormattingReason::Normalization);
        }
        if !line_break && !page_break && position_changed(old, new, segment) {
            reasons.push(FormattingReason::Position);
        }
        if block_structure {
            // Block-structure evidence always coexists with raw separator and
            // line-break evidence on this corpus. Flow has no separate
            // separator token, so the same structural change carries
            // normalization and line-break evidence.
            reasons.push(FormattingReason::Normalization);
            reasons.push(FormattingReason::BlockStructure);
            reasons.push(FormattingReason::LineBreak);
        }
        if line_break && !block_structure {
            reasons.push(FormattingReason::LineBreak);
        }
        if page_break {
            reasons.push(FormattingReason::PageBreak);
        }
        if !reasons.is_empty() {
            reasons.sort_unstable();
            reasons.dedup();
            changes.push(FormattingChange {
                old: segment[0].0..segment[segment.len() - 1].0 + 1,
                new: segment[0].1..segment[segment.len() - 1].1 + 1,
                reasons,
                old_size: None,
                new_size: None,
            });
        }
        segment_start = segment_end;
    }
}

fn font_size_pair(
    old: &[FlowToken],
    new: &[FlowToken],
    (old_index, new_index): (usize, usize),
) -> Option<(f64, f64)> {
    let old_token = old.get(old_index)?;
    let new_token = new.get(new_index)?;
    if old_token.uncertain || new_token.uncertain {
        return None;
    }
    Some((old_token.font_size?, new_token.font_size?))
}

fn raw_differs_in_segment(old: &[FlowToken], new: &[FlowToken], pairs: &[(usize, usize)]) -> bool {
    pairs.iter().any(
        |&(old_index, new_index)| match (&old[old_index].raw, &new[new_index].raw) {
            (Some(old), Some(new)) => old != new,
            _ => false,
        },
    )
}

fn normalization_differs_in_segment(
    old: &[FlowToken],
    new: &[FlowToken],
    pairs: &[(usize, usize)],
) -> bool {
    pairs
        .iter()
        .any(|&(old_index, new_index)| old[old_index].normalization != new[new_index].normalization)
}

fn position_changed(old: &[FlowToken], new: &[FlowToken], pairs: &[(usize, usize)]) -> bool {
    let mut start = 0;
    let mut changed = false;
    for (offset, &(old_index, _new_index)) in pairs.iter().enumerate() {
        let line_ends = old[old_index].line_break_after
            || old[old_index].page_break_after
            || offset + 1 == pairs.len();
        if !line_ends {
            continue;
        }
        let Some(line_changed) = translated_line_change(old, new, &pairs[start..=offset]) else {
            return false;
        };
        changed |= line_changed;
        start = offset + 1;
    }
    changed
}

/// Returns `Some(changed)` when one line has complete geometry and every token
/// shares one translation; `None` means the evidence is incomplete or the line
/// is not a pure translation.
fn translated_line_change(
    old: &[FlowToken],
    new: &[FlowToken],
    pairs: &[(usize, usize)],
) -> Option<bool> {
    let mut anchor: Option<(pdfdelta_core::model::Vec2, pdfdelta_core::model::Vec2)> = None;
    let mut changed = false;
    for &(old_index, new_index) in pairs {
        let old_position = old[old_index].position?;
        let new_position = new[new_index].position?;
        if old_position.direction() != new_position.direction() {
            return None;
        }
        let old_baseline = old_position.baseline();
        let new_baseline = new_position.baseline();
        if let Some((old_anchor, new_anchor)) = anchor {
            if old_baseline.x - old_anchor.x != new_baseline.x - new_anchor.x
                || old_baseline.y - old_anchor.y != new_baseline.y - new_anchor.y
            {
                return None;
            }
        } else {
            changed = old_baseline != new_baseline;
            anchor = Some((old_baseline, new_baseline));
        }
    }
    anchor.map(|_| changed)
}

/// Returns whether the block-boundary pattern differs after numbering each
/// side's first block zero. Block ids themselves are side-local.
fn block_pattern_differs(old: &[FlowToken], new: &[FlowToken], pairs: &[(usize, usize)]) -> bool {
    fn pattern(tokens: &[FlowToken], pairs: &[(usize, usize)], side: usize) -> Vec<usize> {
        let mut pattern = Vec::with_capacity(pairs.len());
        let mut previous = None;
        let mut ordinal = 0;
        for &pair in pairs {
            let block = if side == 0 {
                tokens[pair.0].block
            } else {
                tokens[pair.1].block
            };
            if previous.is_some_and(|previous| previous != block) {
                ordinal += 1;
            }
            previous = Some(block);
            pattern.push(ordinal);
        }
        pattern
    }
    pattern(old, pairs, 0) != pattern(new, pairs, 1)
}

/// Returns whether two effective sizes differ by more than the relative
/// tolerance. Exact bit equality is the first test so identical sizes never
/// divide.
#[must_use]
pub fn font_size_differs(old_size: f64, new_size: f64) -> bool {
    if old_size.to_bits() == new_size.to_bits() {
        return false;
    }
    let scale = old_size.abs().max(new_size.abs());
    if !scale.is_finite() || scale <= 0.0 {
        return false;
    }
    (old_size - new_size).abs() / scale > FONT_SIZE_RELATIVE_TOLERANCE
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{model::PageId, normalize::PositionSignature};

    use super::*;
    use crate::flow_diff;

    fn token(text: &str, page: u32, size: f64) -> FlowToken {
        FlowToken {
            text: text.to_owned(),
            page: PageId(page),
            bounds: None,
            block: 0,
            uncertain: false,
            line_break_after: false,
            font_size: Some(size),
            raw: None,
            position: None,
            page_break_after: false,
            normalization: 0,
        }
    }

    fn diff(old: &[FlowToken], new: &[FlowToken]) -> FlowDiff {
        flow_diff(old, new).expect("test script verifies")
    }

    #[test]
    fn a_unique_run_is_high_confidence() {
        let old = vec![
            token("alpha", 0, 10.0),
            token("beta", 0, 10.0),
            token("gamma", 0, 10.0),
        ];
        let new = vec![
            token("alpha", 0, 10.0),
            token("delta", 0, 10.0),
            token("gamma", 0, 10.0),
        ];
        let diff = diff(&old, &new);
        let assessments = assess_changes(&old, &new, &diff, &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(assessments.len(), diff.changes.len());
        assert_eq!(assessments[0].confidence, Confidence::High);
        assert!(assessments[0].ambiguity.is_empty());
    }

    #[test]
    fn a_repeated_run_is_flagged() {
        let old = vec![
            token("9", 0, 10.0),
            token("820", 0, 10.0),
            token("9", 0, 10.0),
            token("820", 0, 10.0),
        ];
        let new = vec![
            token("9", 0, 10.0),
            token("620", 0, 10.0),
            token("9", 0, 10.0),
            token("820", 0, 10.0),
        ];
        let diff = diff(&old, &new);
        let assessments = assess_changes(&old, &new, &diff, &BTreeSet::new(), &BTreeSet::new());
        let repeated = assessments
            .iter()
            .any(|assessment| assessment.ambiguity.contains(&AmbiguityReason::RepeatedRun));
        assert!(repeated, "{assessments:?}");
    }

    #[test]
    fn a_numeric_change_beside_a_repeated_numeric_token_is_a_numeric_table() {
        let old = vec![
            token("8", 0, 10.0),
            token(",", 0, 10.0),
            token("740", 0, 10.0),
            token("9", 0, 10.0),
            token(",", 0, 10.0),
            token("820", 0, 10.0),
        ];
        let new = vec![
            token("8", 0, 10.0),
            token(",", 0, 10.0),
            token("820", 0, 10.0),
            token("9", 0, 10.0),
            token(",", 0, 10.0),
            token("820", 0, 10.0),
        ];
        let diff = diff(&old, &new);
        let assessments = assess_changes(&old, &new, &diff, &BTreeSet::new(), &BTreeSet::new());
        assert!(
            assessments.iter().any(|assessment| assessment
                .ambiguity
                .contains(&AmbiguityReason::NumericTable)),
            "{assessments:?}"
        );
    }

    #[test]
    fn an_order_uncertain_page_is_flagged() {
        let old = vec![token("alpha", 2, 10.0), token("beta", 2, 10.0)];
        let new = vec![token("alpha", 2, 10.0), token("gamma", 2, 10.0)];
        let diff = diff(&old, &new);
        let uncertain = BTreeSet::from([PageId(2)]);
        let assessments = assess_changes(&old, &new, &diff, &uncertain, &BTreeSet::new());
        assert!(assessments.iter().any(|assessment| {
            assessment
                .ambiguity
                .contains(&AmbiguityReason::OrderUncertain)
        }));
    }

    #[test]
    fn font_size_change_is_reported_and_coalesced() {
        let old = vec![token("default", 0, 10.0), token("policy", 0, 10.0)];
        let new = vec![token("default", 0, 14.0), token("policy", 0, 14.0)];
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].old, 0..2);
        assert_eq!(formatting[0].new, 0..2);
        assert_eq!(formatting[0].old_size, Some(10.0));
        assert_eq!(formatting[0].new_size, Some(14.0));
        assert_eq!(formatting[0].reasons, vec![FormattingReason::FontSize]);
        assert!(!font_size_differs(10.0, 10.05));
        assert!(font_size_differs(10.0, 14.0));
    }

    #[test]
    fn a_single_token_size_change_is_reported_on_its_whole_run() {
        let old = vec![token("default", 0, 10.0), token("policy", 0, 10.0)];
        let new = vec![token("default", 0, 14.0), token("policy", 0, 10.0)];
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].old, 0..2);
        assert_eq!(formatting[0].new, 0..2);
        assert_eq!(formatting[0].reasons, vec![FormattingReason::FontSize]);
        assert_eq!(formatting[0].old_size, Some(10.0));
        assert_eq!(formatting[0].new_size, Some(14.0));
    }

    #[test]
    fn a_translated_line_is_position_evidence() {
        use pdfdelta_core::model::Vec2;

        let mut old = vec![token("alpha", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        for (index, token) in old.iter_mut().enumerate() {
            token.position = Some(
                PositionSignature::new(
                    Vec2 {
                        x: 10.0,
                        y: 100.0 - index as f64,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("old position"),
            );
            token.line_break_after = index == 1;
        }
        for (index, token) in new.iter_mut().enumerate() {
            token.position = Some(
                PositionSignature::new(
                    Vec2 {
                        x: 10.0,
                        y: 120.0 - index as f64,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("new position"),
            );
            token.line_break_after = index == 1;
        }
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].reasons, vec![FormattingReason::Position]);
    }

    #[test]
    fn a_line_moved_to_another_page_is_position_evidence() {
        use pdfdelta_core::model::Vec2;

        let mut old = vec![token("alpha", 62, 10.0), token("beta", 62, 10.0)];
        let mut new = vec![token("alpha", 42, 10.0), token("beta", 42, 10.0)];
        for (index, token) in old.iter_mut().enumerate() {
            token.position = Some(
                PositionSignature::new(
                    Vec2 {
                        x: 10.0,
                        y: 100.0 - index as f64,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("old position"),
            );
            token.line_break_after = index == 1;
        }
        for (index, token) in new.iter_mut().enumerate() {
            token.position = Some(
                PositionSignature::new(
                    Vec2 {
                        x: 10.0,
                        y: 120.0 - index as f64,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("new position"),
            );
            token.line_break_after = index == 1;
        }
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].reasons, vec![FormattingReason::Position]);
    }

    #[test]
    fn a_moved_internal_page_break_suppresses_position() {
        use pdfdelta_core::model::Vec2;

        let mut old = vec![token("alpha", 0, 10.0), token("beta", 1, 10.0)];
        let mut new = vec![token("alpha", 0, 10.0), token("beta", 0, 10.0)];
        for token in old.iter_mut().chain(new.iter_mut()) {
            token.position = Some(
                PositionSignature::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 })
                    .expect("position"),
            );
        }
        old[0].page_break_after = true;
        old[1].line_break_after = true;
        new[1].line_break_after = true;
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert!(
            formatting[0].reasons.contains(&FormattingReason::PageBreak),
            "{formatting:?}"
        );
        assert!(
            !formatting[0].reasons.contains(&FormattingReason::Position),
            "{formatting:?}"
        );
    }

    #[test]
    fn a_document_position_survives_an_unrelated_line_break_difference() {
        use crate::tests::document;

        let old = document(&[
            ("alpha beta", 0, 300.0),
            ("gamma delta", 0, 280.0),
            ("epsilon zeta", 1, 300.0),
        ]);
        let new = document(&[
            ("alpha beta gamma", 0, 300.0),
            ("delta", 0, 280.0),
            ("epsilon zeta", 1, 320.0),
        ]);
        let pair = crate::flow_tokens_pair(&old, &new).expect("document pair");
        let diff = flow_diff(&pair.old.tokens, &pair.new.tokens).expect("verified diff");
        let formatting = formatting_changes(&pair.old.tokens, &pair.new.tokens, &diff.spine);
        let page_one = pair
            .old
            .tokens
            .iter()
            .position(|token| token.page == PageId(1))
            .expect("page one token");
        assert!(
            formatting.iter().any(|change| {
                change.old.start >= page_one && change.reasons.contains(&FormattingReason::Position)
            }),
            "{formatting:?}"
        );
    }

    #[test]
    fn a_point_position_does_not_split_a_normalization_span() {
        assert!(mergeable_reasons(
            &[FormattingReason::Normalization, FormattingReason::Position],
            &[FormattingReason::Normalization],
            1,
            3,
        ));
        assert_eq!(
            merged_reasons(
                &[FormattingReason::Normalization, FormattingReason::Position],
                &[FormattingReason::Normalization],
            ),
            vec![FormattingReason::Normalization]
        );
        assert!(!mergeable_reasons(
            &[FormattingReason::Normalization, FormattingReason::Position],
            &[FormattingReason::Normalization],
            2,
            3,
        ));
    }

    #[test]
    fn a_break_difference_does_not_suppress_position_on_another_segment() {
        use pdfdelta_core::model::Vec2;

        let mut old = vec![
            token("alpha", 0, 10.0),
            token("beta", 0, 10.0),
            token("gamma", 0, 10.0),
            token("delta", 1, 10.0),
        ];
        let mut new = old.clone();
        old[0].block = 0;
        old[1].block = 0;
        old[2].block = 1;
        old[3].block = 1;
        new[0].block = 0;
        new[1].block = 0;
        new[2].block = 1;
        new[3].block = 1;
        for index in 0..4 {
            old[index].position = Some(
                PositionSignature::new(
                    Vec2 {
                        x: 10.0,
                        y: 100.0 - index as f64,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("old position"),
            );
            new[index].position = Some(
                PositionSignature::new(
                    Vec2 {
                        x: 10.0,
                        y: 120.0 - index as f64,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("new position"),
            );
        }
        old[0].line_break_after = true;
        old[1].line_break_after = true;
        old[2].page_break_after = true;
        old[3].line_break_after = true;
        new[0].line_break_after = true;
        new[1].line_break_after = true;
        new[2].line_break_after = true;
        new[3].line_break_after = true;
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert!(
            formatting.iter().any(|change| {
                change.old == (0..2)
                    && change.new == (0..2)
                    && change.reasons == vec![FormattingReason::Position]
            }),
            "{formatting:?}"
        );
        assert!(
            formatting.iter().any(|change| {
                change.old == (2..4)
                    && change.new == (2..4)
                    && change.reasons.contains(&FormattingReason::PageBreak)
                    && !change.reasons.contains(&FormattingReason::Position)
            }),
            "{formatting:?}"
        );
    }

    #[test]
    fn a_nonuniform_line_is_not_position_evidence() {
        use pdfdelta_core::model::Vec2;

        let mut old = vec![token("alpha", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        for token in &mut old {
            token.position = Some(
                PositionSignature::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 })
                    .expect("old"),
            );
        }
        new[0].position = Some(
            PositionSignature::new(Vec2 { x: 0.0, y: 10.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("new0"),
        );
        new[1].position = Some(
            PositionSignature::new(Vec2 { x: 0.0, y: 20.0 }, Vec2 { x: 1.0, y: 0.0 })
                .expect("new1"),
        );
        let diff = diff(&old, &new);
        assert!(formatting_changes(&old, &new, &diff.spine).is_empty());
    }

    #[test]
    fn a_one_token_match_inside_a_rewritten_block_is_not_formatting_evidence() {
        let mut old = vec![
            token("alpha", 0, 10.0),
            token("beta", 0, 10.0),
            token("gamma", 0, 10.0),
        ];
        let mut new = vec![
            token("alpha", 0, 14.0),
            token("BETA", 0, 10.0),
            token("gamma", 0, 10.0),
        ];
        for index in [0, 2] {
            old[index].position = Some(
                PositionSignature::new(
                    pdfdelta_core::model::Vec2 {
                        x: 0.0,
                        y: 100.0 - index as f64,
                    },
                    pdfdelta_core::model::Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("old position"),
            );
            new[index].position = Some(
                PositionSignature::new(
                    pdfdelta_core::model::Vec2 {
                        x: 0.0,
                        y: 120.0 - index as f64,
                    },
                    pdfdelta_core::model::Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("new position"),
            );
        }
        let diff = diff(&old, &new);
        assert!(
            formatting_changes(&old, &new, &diff.spine).is_empty(),
            "a partial block is not a formatting unit"
        );
    }

    #[test]
    fn a_break_at_an_old_block_boundary_is_not_formatting_evidence() {
        let mut old = vec![
            token("alpha", 0, 10.0),
            token("beta", 0, 10.0),
            token("gamma", 0, 10.0),
            token("delta", 0, 10.0),
        ];
        let mut new = old.clone();
        for index in 0..2 {
            old[index].block = 0;
            new[index].block = 0;
        }
        for index in 2..4 {
            old[index].block = 1;
            new[index].block = 1;
        }
        old[1].page_break_after = true;
        old[3].line_break_after = true;
        new[3].line_break_after = true;
        let diff = diff(&old, &new);
        assert!(formatting_changes(&old, &new, &diff.spine).is_empty());
    }

    #[test]
    fn adjacent_segments_with_the_same_evidence_are_coalesced() {
        let mut old = vec![
            token("alpha", 0, 10.0),
            token("beta", 0, 10.0),
            token("gamma", 0, 10.0),
            token("delta", 0, 10.0),
        ];
        let mut new = old.clone();
        for index in 0..2 {
            old[index].block = 0;
            new[index].block = 0;
        }
        for index in 2..4 {
            old[index].block = 1;
            new[index].block = 1;
        }
        for index in 0..4 {
            old[index].raw = Some(old[index].text.clone().into());
            new[index].raw = Some(new[index].text.to_uppercase().into());
        }
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].old, 0..4);
        assert_eq!(formatting[0].new, 0..4);
        assert_eq!(formatting[0].reasons, vec![FormattingReason::Normalization]);
    }

    #[test]
    fn raw_text_difference_is_normalization_evidence() {
        let mut old = vec![token("A", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        old[0].raw = Some("A".into());
        new[0].raw = Some("Ａ".into());
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].reasons, vec![FormattingReason::Normalization]);
    }

    #[test]
    fn normalization_event_mask_difference_is_normalization_evidence() {
        let mut old = vec![token("A", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        old[0].raw = Some("A".into());
        new[0].raw = Some("A".into());
        old[0].normalization = 4;
        new[0].normalization = 0;
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(formatting[0].reasons, vec![FormattingReason::Normalization]);
    }

    #[test]
    fn line_and_page_break_differences_are_distinct_reasons() {
        let mut old = vec![token("alpha", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        old[0].line_break_after = false;
        new[0].line_break_after = true;
        let first_diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &first_diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert!(formatting[0].reasons.contains(&FormattingReason::LineBreak));

        let mut old = vec![token("alpha", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        old[0].page_break_after = false;
        new[0].page_break_after = true;
        let second_diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &second_diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert_eq!(
            formatting[0].reasons,
            vec![FormattingReason::PageBreak],
            "{formatting:?}"
        );
    }

    #[test]
    fn a_changed_block_boundary_is_block_structure_evidence() {
        let mut old = vec![token("alpha", 0, 10.0), token("beta", 0, 10.0)];
        let mut new = old.clone();
        old[0].block = 0;
        old[1].block = 0;
        new[0].block = 0;
        new[1].block = 1;
        old[0].line_break_after = true;
        new[0].line_break_after = true;
        let diff = diff(&old, &new);
        let formatting = formatting_changes(&old, &new, &diff.spine);
        assert_eq!(formatting.len(), 1, "{formatting:?}");
        assert!(
            formatting[0]
                .reasons
                .contains(&FormattingReason::BlockStructure),
            "{formatting:?}"
        );
    }

    #[test]
    fn layout_only_and_moves_select_differently() {
        let (old, new) = (
            vec![token("a", 0, 10.0), token("b", 0, 10.0)],
            vec![token("a", 0, 10.0), token("b", 0, 10.0)],
        );
        let unchanged = diff(&old, &new);
        assert!(!selects_difference(&unchanged.stats()));
        let (old, new) = (
            vec![
                token("a", 0, 10.0),
                token("b", 0, 10.0),
                token("c", 0, 10.0),
                token("d", 0, 10.0),
                token("e", 0, 10.0),
            ],
            vec![
                token("a", 0, 10.0),
                token("c", 0, 10.0),
                token("d", 0, 10.0),
                token("e", 0, 10.0),
                token("b", 0, 10.0),
            ],
        );
        let reordered = diff(&old, &new);
        assert!(selects_difference(&reordered.stats()));
    }
}
