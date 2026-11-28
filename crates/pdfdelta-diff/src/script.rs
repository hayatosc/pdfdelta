//! Edit-script construction from an equal-token spine.
//!
//! Every spine pair is an equal token that is neither changed nor moved. The
//! gaps between consecutive spine pairs are classified as deletion, insertion,
//! or replacement, and replacements are refined into layout-only changes and
//! moves. The resulting script is one specific correct script; benchmarks
//! measure whether the choices are good, while [`crate::verify::verify`] guarantees
//! they are correct.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
};

use pdfdelta_core::model::PageId;

use super::Token;

/// Minimum number of tokens in a moved run.
pub const MOVE_MIN_TOKENS: usize = 5;

/// Parameters of the move rewriting rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MoveRule {
    /// Promote same-page contents-page moves that sit beside numeric changes.
    pub allow_same_page: bool,
    /// Minimum information weight (weight minus one per token) of a move.
    pub min_information: u64,
}

/// Minimum information weight in a moved run.
///
/// Every token weighs at least one, so a run of dot leaders, separators, or
/// other near-zero-information tokens has a weight above zero but no
/// information. Subtracting the minimum weight from every token makes a run of
/// punctuation contribute nothing; a heading or prose run must contribute at
/// least this much before it can be reported as a move.
pub const MOVE_MIN_INFORMATION_WEIGHT: u64 = 1;

/// Length of the k-gram used to detect moved runs.
const MOVE_GRAM: usize = 5;

/// Maximum number of deletion candidates indexed for one k-gram.
const MAX_CANDIDATES_PER_GRAM: usize = 16;

/// One change of a token-stream edit script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Old tokens removed without a matching new run.
    Delete { old: Range<usize> },
    /// New tokens added without a matching old run.
    Insert { new: Range<usize> },
    /// Old tokens replaced by new tokens.
    Replace {
        old: Range<usize>,
        new: Range<usize>,
    },
    /// A contiguous run removed from one place and inserted at another.
    Move {
        old: Range<usize>,
        new: Range<usize>,
    },
    /// A run whose text is unchanged but whose reading-order position moved
    /// while its visual row and left-to-right order stayed fixed.
    ///
    /// The two runs of a same-row transposition are physically side by side in
    /// both documents. Only their extraction order differs, so the run is
    /// layout evidence: it never selects a content difference.
    ReadingOrder {
        old: Range<usize>,
        new: Range<usize>,
    },
    /// Token text is unchanged; only line or page boundaries moved.
    LayoutOnly {
        old: Range<usize>,
        new: Range<usize>,
    },
}

impl Change {
    /// Old token range covered by this change; empty for an insertion.
    #[must_use]
    pub fn old_range(&self) -> Range<usize> {
        match self {
            Self::Delete { old }
            | Self::Replace { old, .. }
            | Self::Move { old, .. }
            | Self::ReadingOrder { old, .. }
            | Self::LayoutOnly { old, .. } => old.clone(),
            Self::Insert { .. } => 0..0,
        }
    }

    /// New token range covered by this change; empty for a deletion.
    #[must_use]
    pub fn new_range(&self) -> Range<usize> {
        match self {
            Self::Insert { new }
            | Self::Replace { new, .. }
            | Self::Move { new, .. }
            | Self::ReadingOrder { new, .. }
            | Self::LayoutOnly { new, .. } => new.clone(),
            Self::Delete { .. } => 0..0,
        }
    }
}

/// Builds an edit script for `old` -> `new` given an equal-token spine.
///
/// The spine must be strictly increasing on both sides. Every non-spine token
/// is covered exactly once by the returned changes.
#[must_use]
pub fn build_script(old: &[Token], new: &[Token], spine: &[(usize, usize)]) -> Vec<Change> {
    let (old_ids, new_ids) = super::intern_tokens(old, new);
    let weights = super::align::information_weights(&old_ids, &new_ids);
    let dot_leaders = DotLeaderCache::new(old, new);
    let context = ScriptContext {
        old,
        new,
        old_ids: &old_ids,
        new_ids: &new_ids,
        weights: &weights,
        dot_leaders: &dot_leaders,
    };
    build_script_and_promotions(
        &context,
        spine,
        MoveRule {
            allow_same_page: true,
            min_information: MOVE_MIN_INFORMATION_WEIGHT,
        },
    )
    .0
}

/// The token streams and alignment inputs one script is built from.
pub(crate) struct ScriptContext<'a> {
    pub(crate) old: &'a [Token],
    pub(crate) new: &'a [Token],
    pub(crate) old_ids: &'a [u32],
    pub(crate) new_ids: &'a [u32],
    pub(crate) weights: &'a [u64],
    pub(crate) dot_leaders: &'a DotLeaderCache,
}

/// Gap classification of one spine, before any move rule rewrites it.
///
/// The base depends only on the spine and the token streams, so the three move
/// rules that share one alignment share one base script and only re-run the
/// move detector and rewriting on it.
pub(crate) struct ScriptBase {
    changes: Vec<GapChange>,
}

/// Classifies the gaps of `spine` and marks layout-only replacements.
pub(crate) fn build_script_base(
    old: &[Token],
    new: &[Token],
    spine: &[(usize, usize)],
) -> ScriptBase {
    let mut changes = base_changes(old.len(), new.len(), spine);
    refine_layout_only(&mut changes, old, new);
    ScriptBase { changes }
}

/// Builds the script and reports same-gap runs that must be promoted, plus
/// whether any move candidate exists at all.
///
/// The candidate flag lets the caller skip the remaining move rules when no
/// run can be moved: every rule then leaves the script unchanged.
pub(crate) fn build_script_and_promotions(
    context: &ScriptContext<'_>,
    spine: &[(usize, usize)],
    rule: MoveRule,
) -> (Vec<Change>, Vec<(usize, usize)>, bool) {
    let base = build_script_base(context.old, context.new, spine);
    script_from_base(context, &base, rule)
}

/// Applies one move rule to an already classified spine.
pub(crate) fn script_from_base(
    context: &ScriptContext<'_>,
    base: &ScriptBase,
    rule: MoveRule,
) -> (Vec<Change>, Vec<(usize, usize)>, bool) {
    let mut changes = base.changes.clone();
    // Both the promotion pass and the move rewriting filter the same accepted
    // candidate spans, so the detection runs once per script.
    let moves = detect_moves(context, &changes, rule);
    let candidates = !moves.is_empty();
    let promotions = in_place_promotions(context, &changes, rule, &moves);
    apply_moves(context, &mut changes, &moves);
    (
        changes.into_iter().map(|entry| entry.change).collect(),
        promotions,
        candidates,
    )
}

/// A change plus the index of the spine gap that produced it.
#[derive(Clone)]
struct GapChange {
    gap: usize,
    change: Change,
}

/// Classifies the gaps between spine pairs.
fn base_changes(old_len: usize, new_len: usize, spine: &[(usize, usize)]) -> Vec<GapChange> {
    let mut changes = Vec::new();
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    let mut gap = 0;
    for &(old_index, new_index) in spine {
        if old_index < old_cursor
            || new_index < new_cursor
            || old_index >= old_len
            || new_index >= new_len
        {
            continue;
        }
        push_gap(
            &mut changes,
            gap,
            old_cursor..old_index,
            new_cursor..new_index,
        );
        gap += 1;
        old_cursor = old_index + 1;
        new_cursor = new_index + 1;
    }
    push_gap(&mut changes, gap, old_cursor..old_len, new_cursor..new_len);
    changes
}

fn push_gap(changes: &mut Vec<GapChange>, gap: usize, old: Range<usize>, new: Range<usize>) {
    let change = match (old.is_empty(), new.is_empty()) {
        (true, true) => return,
        (true, false) => Change::Insert { new },
        (false, true) => Change::Delete { old },
        (false, false) => Change::Replace { old, new },
    };
    changes.push(GapChange { gap, change });
}

/// Rewrites replacements whose only difference is layout into `LayoutOnly`.
fn refine_layout_only(changes: &mut [GapChange], old: &[Token], new: &[Token]) {
    for entry in changes {
        let Change::Replace {
            old: old_range,
            new: new_range,
        } = &entry.change
        else {
            continue;
        };
        if layout_only_equal(&old[old_range.clone()], &new[new_range.clone()]) {
            entry.change = Change::LayoutOnly {
                old: old_range.clone(),
                new: new_range.clone(),
            };
        }
    }
}

/// Returns whether two token sequences differ only in layout.
///
/// The relation holds when the concatenated texts are equal, or when the texts
/// become equal after removing hyphen-minus, hyphen, and soft-hyphen characters
/// and at least one side contains a hyphen at a source line end. Hyphenation is
/// the only layout decision that can add punctuation to the visible stream, so
/// the second arm only applies where the line-break evidence supports it.
#[must_use]
pub fn layout_only_equal(old: &[Token], new: &[Token]) -> bool {
    if concatenated_text(old) == concatenated_text(new) {
        return true;
    }
    if !(has_line_end_hyphen(old) || has_line_end_hyphen(new)) {
        return false;
    }
    strip_hyphens(&concatenated_text(old)) == strip_hyphens(&concatenated_text(new))
}

fn concatenated_text(tokens: &[Token]) -> String {
    let mut text = String::new();
    for token in tokens {
        text.push_str(&token.text);
    }
    text
}

fn strip_hyphens(text: &str) -> String {
    text.chars()
        .filter(|character| !is_break_hyphen(*character))
        .collect()
}

fn is_break_hyphen(character: char) -> bool {
    matches!(character, '-' | '\u{2010}' | '\u{00ad}')
}

fn has_line_end_hyphen(tokens: &[Token]) -> bool {
    tokens.iter().any(|token| {
        token.line_break_after && token.text.chars().last().is_some_and(is_break_hyphen)
    })
}

/// Detects moved runs and rewrites the affected changes around them.
///
/// A candidate pairs the old side of a deletion or replacement with the new
/// side of an insertion or replacement in a different spine gap. Replacement
/// sides participate so that a paragraph moved into a region whose surrounding
/// text was also edited becomes a move instead of one large replacement. An
/// accepted move is removed from its source changes; the remainders are paired
/// in order and reclassified as deletions, insertions, replacements, or
/// layout-only changes.
fn apply_moves(context: &ScriptContext<'_>, changes: &mut Vec<GapChange>, moves: &[MoveSpan]) {
    let moves: Vec<MoveSpan> = moves
        .iter()
        .filter(|span| changes[span.old_change].gap != changes[span.new_change].gap)
        .cloned()
        .collect();
    if moves.is_empty() {
        return;
    }
    let mut moves_by_old: HashMap<usize, Vec<&MoveSpan>> = HashMap::new();
    let mut moves_by_new: HashMap<usize, Vec<&MoveSpan>> = HashMap::new();
    for move_span in &moves {
        moves_by_old
            .entry(move_span.old_change)
            .or_default()
            .push(move_span);
        moves_by_new
            .entry(move_span.new_change)
            .or_default()
            .push(move_span);
    }
    for spans in moves_by_old.values_mut() {
        spans.sort_unstable_by_key(|span| span.old.start);
    }
    for spans in moves_by_new.values_mut() {
        spans.sort_unstable_by_key(|span| span.new.start);
    }
    let mut out = Vec::with_capacity(changes.len() + moves.len() * 2);
    for (index, entry) in changes.iter().enumerate() {
        let old_moves: &[&MoveSpan] = moves_by_old.get(&index).map_or(&[], Vec::as_slice);
        let new_moves: &[&MoveSpan] = moves_by_new.get(&index).map_or(&[], Vec::as_slice);
        match &entry.change {
            Change::Delete { old: range } => {
                let mut events: Vec<(usize, Change)> = complement(range.clone(), old_moves, true)
                    .into_iter()
                    .map(|piece| (piece.start, Change::Delete { old: piece }))
                    .collect();
                events.extend(old_moves.iter().map(|span| {
                    (
                        span.old.start,
                        Change::Move {
                            old: span.old.clone(),
                            new: span.new.clone(),
                        },
                    )
                }));
                events.sort_unstable_by_key(|(start, _)| *start);
                out.extend(events.into_iter().map(|(_, change)| change));
            }
            Change::Insert { new: range } => {
                out.extend(
                    complement(range.clone(), new_moves, false)
                        .into_iter()
                        .map(|piece| Change::Insert { new: piece }),
                );
            }
            Change::Replace {
                old: old_range,
                new: new_range,
            } => emit_replacement_split(
                &mut out,
                old_range,
                new_range,
                old_moves,
                new_moves,
                context.old,
                context.new,
            ),
            other => out.push(other.clone()),
        }
    }
    fold_rotation_remainders(&mut out, context.old_ids, context.new_ids);
    changes.clear();
    changes.extend(out.into_iter().map(|change| GapChange { gap: 0, change }));
}

/// Reports a rotation remainder around a move as a move of its own.
///
/// A moved run can rotate one token (or a short run) from the front of the old
/// range to the end of the new range, or from the end to the front. The main
/// moved run is accepted above, but the rotated tokens are left as a Delete run
/// and an Insert run of identical text because they are shorter than
/// [`MOVE_MIN_TOKENS`] and have different neighbours. When such a Delete and
/// Insert touch opposite ends of the same accepted move, they are the remainder
/// of that rotation, so they are reported as one Move. Unrelated short equal
/// runs are not adjacent to opposite ends of one move and stay Delete+Insert.
fn fold_rotation_remainders(changes: &mut Vec<Change>, old_ids: &[u32], new_ids: &[u32]) {
    loop {
        let mut delete_by_end = HashMap::new();
        let mut delete_by_start = HashMap::new();
        let mut insert_by_start = HashMap::new();
        let mut insert_by_end = HashMap::new();
        for (index, change) in changes.iter().enumerate() {
            match change {
                Change::Delete { old } => {
                    delete_by_end.insert(old.end, index);
                    delete_by_start.insert(old.start, index);
                }
                Change::Insert { new } => {
                    insert_by_start.insert(new.start, index);
                    insert_by_end.insert(new.end, index);
                }
                Change::Move { .. }
                | Change::ReadingOrder { .. }
                | Change::Replace { .. }
                | Change::LayoutOnly { .. } => {}
            }
        }
        let mut folds: Vec<(usize, Range<usize>, Range<usize>)> = Vec::new();
        let mut used_delete = HashSet::new();
        let mut used_insert = HashSet::new();
        for change in changes.iter() {
            let Change::Move {
                old: moved_old,
                new: moved_new,
            } = change
            else {
                continue;
            };
            for (delete_index, insert_index) in [
                (
                    delete_by_end.get(&moved_old.start),
                    insert_by_start.get(&moved_new.end),
                ),
                (
                    delete_by_start.get(&moved_old.end),
                    insert_by_end.get(&moved_new.start),
                ),
            ] {
                let (Some(&delete_index), Some(&insert_index)) = (delete_index, insert_index)
                else {
                    continue;
                };
                if used_delete.contains(&delete_index) || used_insert.contains(&insert_index) {
                    continue;
                }
                let Change::Delete { old: deleted } = &changes[delete_index] else {
                    continue;
                };
                let Change::Insert { new: inserted } = &changes[insert_index] else {
                    continue;
                };
                if deleted.len() != inserted.len()
                    || old_ids[deleted.clone()] != new_ids[inserted.clone()]
                {
                    continue;
                }
                used_delete.insert(delete_index);
                used_insert.insert(insert_index);
                folds.push((delete_index, deleted.clone(), inserted.clone()));
            }
        }
        if folds.is_empty() {
            break;
        }
        for (delete_index, old, new) in &folds {
            changes[*delete_index] = Change::Move {
                old: old.clone(),
                new: new.clone(),
            };
        }
        // Remove the folded insertions from back to front so earlier indices
        // stay valid. Folds are collected in increasing delete order, and the
        // insert indices are unique.
        let mut insert_indices = used_insert.into_iter().collect::<Vec<_>>();
        insert_indices.sort_unstable();
        for index in insert_indices.into_iter().rev() {
            changes.remove(index);
        }
    }
}

/// Emits the remainders of a replacement after its moves are removed.
///
/// Old and new remainders are paired by position: the first old remainder with
/// the first new remainder, and so on. Unpaired old remainders become
/// deletions, unpaired new remainders become insertions, and a paired remainder
/// whose text is layout-equivalent becomes a layout-only change instead of a
/// replacement.
fn emit_replacement_split(
    out: &mut Vec<Change>,
    old_range: &Range<usize>,
    new_range: &Range<usize>,
    old_moves: &[&MoveSpan],
    new_moves: &[&MoveSpan],
    old: &[Token],
    new: &[Token],
) {
    let old_pieces = complement(old_range.clone(), old_moves, true);
    let new_pieces = complement(new_range.clone(), new_moves, false);
    let paired = old_pieces.len().min(new_pieces.len());
    let mut events: Vec<(usize, Change)> = Vec::with_capacity(old_pieces.len() + old_moves.len());
    for index in 0..paired {
        let old_piece = old_pieces[index].clone();
        let new_piece = new_pieces[index].clone();
        let change = if layout_only_equal(&old[old_piece.clone()], &new[new_piece.clone()]) {
            Change::LayoutOnly {
                old: old_piece.clone(),
                new: new_piece,
            }
        } else {
            Change::Replace {
                old: old_piece.clone(),
                new: new_piece,
            }
        };
        events.push((old_piece.start, change));
    }
    for piece in old_pieces.into_iter().skip(paired) {
        events.push((piece.start, Change::Delete { old: piece }));
    }
    for span in old_moves {
        events.push((
            span.old.start,
            Change::Move {
                old: span.old.clone(),
                new: span.new.clone(),
            },
        ));
    }
    events.sort_unstable_by_key(|(start, _)| *start);
    out.extend(events.into_iter().map(|(_, change)| change));
    // New-only remainders have no old position, so they follow the
    // old-ordered changes of the same replacement.
    out.extend(
        new_pieces
            .into_iter()
            .skip(paired)
            .map(|piece| Change::Insert { new: piece }),
    );
}

/// Returns the parts of `range` not covered by the given move spans.
///
/// `spans` must be sorted and disjoint and lie inside `range`.
fn complement(range: Range<usize>, spans: &[&MoveSpan], old_side: bool) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut cursor = range.start;
    for span in spans {
        let span_range = if old_side { &span.old } else { &span.new };
        if span_range.start > cursor {
            pieces.push(cursor..span_range.start);
        }
        cursor = span_range.end;
    }
    if cursor < range.end {
        pieces.push(cursor..range.end);
    }
    pieces
}

#[derive(Clone)]
struct MoveSpan {
    old_change: usize,
    new_change: usize,
    old: Range<usize>,
    new: Range<usize>,
}

/// Returns the token pairs of same-gap move candidates.
///
/// A run whose old and new ranges live in the same gap never left its place:
/// it is alignable as equal content. The caller promotes these pairs into the
/// spine and aligns the sub-gaps again, so the run becomes equal tokens rather
/// than a move or a delete/insert pair.
fn in_place_promotions(
    context: &ScriptContext<'_>,
    changes: &[GapChange],
    rule: MoveRule,
    moves: &[MoveSpan],
) -> Vec<(usize, usize)> {
    let ScriptContext {
        old,
        new,
        dot_leaders,
        ..
    } = *context;
    let mut promotions = Vec::new();
    for span in moves {
        let same_gap = changes[span.old_change].gap == changes[span.new_change].gap;
        let same_page = if span.old.is_empty() || span.new.is_empty() {
            false
        } else {
            let old_page = old[span.old.start].page;
            let new_page = new[span.new.start].page;
            old_page == new_page
                && old[span.old.clone()]
                    .iter()
                    .all(|token| token.page == old_page)
                && new[span.new.clone()]
                    .iter()
                    .all(|token| token.page == new_page)
        };
        if !same_gap {
            if !rule.allow_same_page {
                continue;
            }
            let (only_low, saw_low) = low_weight_changes_between(context, changes, span);
            let leaders = same_page
                && dot_leaders.old_page(old[span.old.start].page)
                && dot_leaders.new_page(new[span.new.start].page);
            if !same_page || !only_low || !saw_low || !leaders {
                continue;
            }
        }
        for offset in 0..span.old.len() {
            promotions.push((span.old.start + offset, span.new.start + offset));
        }
    }
    promotions
}

/// Per-page table-of-contents leader-dot flags for both documents.
///
/// Same-page identical-text runs beside page numbers are alignment artifacts
/// on contents pages. A page needs a meaningful density of standalone `.`
/// tokens; ordinary prose and code pages have none, so their genuine same-page
/// relocations stay moves. The density is computed once per document because
/// the promotion pass asks about every candidate run.
pub(crate) struct DotLeaderCache {
    old_pages: Vec<bool>,
    new_pages: Vec<bool>,
    old_any: bool,
    new_any: bool,
}

impl DotLeaderCache {
    /// Classifies every page of both token streams.
    #[must_use]
    pub(crate) fn new(old: &[Token], new: &[Token]) -> Self {
        let (old_pages, old_any) = page_dot_leaders(old);
        let (new_pages, new_any) = page_dot_leaders(new);
        Self {
            old_pages,
            new_pages,
            old_any,
            new_any,
        }
    }

    /// Whether both documents contain a dot-leader page.
    ///
    /// The contents promotion needs a leader-dense page on each side, so
    /// without one on both sides the richest rule cannot promote anything.
    #[must_use]
    pub(crate) fn contents_promotion_possible(&self) -> bool {
        self.old_any && self.new_any
    }

    fn old_page(&self, page: PageId) -> bool {
        self.old_pages
            .get(usize::try_from(page.0).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(false)
    }

    fn new_page(&self, page: PageId) -> bool {
        self.new_pages
            .get(usize::try_from(page.0).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(false)
    }
}

/// Returns one dot-leader flag per page and whether any flag is set.
fn page_dot_leaders(tokens: &[Token]) -> (Vec<bool>, bool) {
    let Some(page_count) = tokens
        .iter()
        .map(|token| usize::try_from(token.page.0).unwrap_or(usize::MAX))
        .max()
        .map(|max| max.saturating_add(1))
        .filter(|max| *max != usize::MAX)
    else {
        return (Vec::new(), false);
    };
    if page_count > tokens.len().saturating_add(1) {
        // Page ids are document page numbers; a sparse fixture with an
        // out-of-range id must not allocate an unbounded table.
        return (Vec::new(), false);
    }
    let mut dots = vec![0usize; page_count];
    let mut totals = vec![0usize; page_count];
    for token in tokens {
        let page = usize::try_from(token.page.0).unwrap_or(usize::MAX);
        if page >= page_count {
            continue;
        }
        totals[page] += 1;
        if token.text == "." {
            dots[page] += 1;
        }
    }
    let mut any = false;
    let pages = dots
        .into_iter()
        .zip(totals)
        .map(|(dots, total)| {
            let leader = dots >= 8 && dots * 20 >= total;
            any |= leader;
            leader
        })
        .collect();
    (pages, any)
}

/// Returns whether the changes between a move's source and target are only
/// low-information or numeric tokens, and whether any such token exists.
///
/// A table-of-contents entry whose page number changed can be aligned as a
/// replacement of the number while the heading and dot leaders stay equal.
/// When a same-page move spans only such changes, promoting the moved run back
/// into the spine turns the move into that in-place replacement. Requiring at
/// least one low/numeric token keeps an ordinary same-page reordering, which
/// has no such token around it, as a move.
fn low_weight_changes_between(
    context: &ScriptContext<'_>,
    changes: &[GapChange],
    span: &MoveSpan,
) -> (bool, bool) {
    let ScriptContext {
        old,
        new,
        old_ids,
        new_ids,
        weights,
        ..
    } = *context;
    let first = changes[span.old_change].gap;
    let second = changes[span.new_change].gap;
    let (low, high) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let mut saw_low = false;
    for (index, entry) in changes.iter().enumerate() {
        if entry.gap < low || entry.gap > high {
            continue;
        }
        let mut check = |tokens: &[Token], ids: &[u32]| {
            let (all_low, any_low) = low_weight_tokens(tokens, ids, weights);
            saw_low |= any_low;
            all_low
        };
        match &entry.change {
            Change::Delete { old: range } => {
                let pieces = if index == span.old_change {
                    complement(range.clone(), &[span], true)
                } else {
                    vec![range.clone()]
                };
                if !pieces
                    .iter()
                    .all(|piece| check(&old[piece.clone()], &old_ids[piece.clone()]))
                {
                    return (false, saw_low);
                }
            }
            Change::Insert { new: range } => {
                let pieces = if index == span.new_change {
                    complement(range.clone(), &[span], false)
                } else {
                    vec![range.clone()]
                };
                if !pieces
                    .iter()
                    .all(|piece| check(&new[piece.clone()], &new_ids[piece.clone()]))
                {
                    return (false, saw_low);
                }
            }
            Change::Replace {
                old: old_range,
                new: new_range,
            } => {
                let old_pieces = if index == span.old_change {
                    complement(old_range.clone(), &[span], true)
                } else {
                    vec![old_range.clone()]
                };
                let new_pieces = if index == span.new_change {
                    complement(new_range.clone(), &[span], false)
                } else {
                    vec![new_range.clone()]
                };
                if !old_pieces
                    .iter()
                    .all(|piece| check(&old[piece.clone()], &old_ids[piece.clone()]))
                    || !new_pieces
                        .iter()
                        .all(|piece| check(&new[piece.clone()], &new_ids[piece.clone()]))
                {
                    return (false, saw_low);
                }
            }
            Change::Move { .. } | Change::ReadingOrder { .. } | Change::LayoutOnly { .. } => {}
        }
    }
    (true, saw_low)
}

/// Whether every token has minimum information weight or is numeric/punctuation,
/// and whether at least one token is.
fn low_weight_tokens(tokens: &[Token], ids: &[u32], weights: &[u64]) -> (bool, bool) {
    let mut any = false;
    let all = tokens.iter().zip(ids).all(|(token, &id)| {
        let low = weights[id as usize] <= 1
            || token
                .text
                .chars()
                .all(|character| character.is_ascii_digit() || !character.is_alphanumeric());
        any |= low;
        low
    });
    (all, any)
}

/// A set of disjoint half-open intervals used to reject overlapping moves.
struct IntervalSet {
    by_start: BTreeMap<usize, usize>,
}

impl IntervalSet {
    fn new() -> Self {
        Self {
            by_start: BTreeMap::new(),
        }
    }

    fn overlaps(&self, start: usize, len: usize) -> bool {
        let end = start + len;
        if let Some((_, &previous_end)) = self.by_start.range(..=start).next_back()
            && previous_end > start
        {
            return true;
        }
        if let Some((&next_start, _)) = self.by_start.range(start..).next()
            && next_start < end
        {
            return true;
        }
        false
    }

    fn insert(&mut self, start: usize, len: usize) {
        self.by_start.insert(start, start + len);
    }
}

/// Returns the old-token range of a change that can start a move.
fn old_change_range(change: &Change) -> Option<&Range<usize>> {
    match change {
        Change::Delete { old } | Change::Replace { old, .. } => Some(old),
        Change::Insert { .. }
        | Change::Move { .. }
        | Change::ReadingOrder { .. }
        | Change::LayoutOnly { .. } => None,
    }
}

/// Returns the new-token range of a change that can receive a move.
fn new_change_range(change: &Change) -> Option<&Range<usize>> {
    match change {
        Change::Insert { new } | Change::Replace { new, .. } => Some(new),
        Change::Delete { .. }
        | Change::Move { .. }
        | Change::ReadingOrder { .. }
        | Change::LayoutOnly { .. } => None,
    }
}

fn detect_moves(
    context: &ScriptContext<'_>,
    changes: &[GapChange],
    rule: MoveRule,
) -> Vec<MoveSpan> {
    let ScriptContext {
        old_ids,
        new_ids,
        weights,
        ..
    } = *context;
    let mut index: HashMap<[u32; MOVE_GRAM], Vec<(usize, usize)>> = HashMap::new();
    for (change_index, entry) in changes.iter().enumerate() {
        let Some(old_range) = old_change_range(&entry.change) else {
            continue;
        };
        let ids = &old_ids[old_range.clone()];
        if ids.len() < MOVE_GRAM {
            continue;
        }
        for offset in 0..=ids.len() - MOVE_GRAM {
            let candidates = index.entry(gram::<MOVE_GRAM>(ids, offset)).or_default();
            if candidates.len() < MAX_CANDIDATES_PER_GRAM {
                candidates.push((change_index, offset));
            }
        }
    }
    if index.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<(u64, usize, usize, usize, usize, usize)> = Vec::new();
    for (new_change, entry) in changes.iter().enumerate() {
        let Some(new_range) = new_change_range(&entry.change) else {
            continue;
        };
        let new_side = &new_ids[new_range.clone()];
        if new_side.len() < MOVE_GRAM {
            continue;
        }
        for offset in 0..=new_side.len() - MOVE_GRAM {
            let Some(matches) = index.get(&gram::<MOVE_GRAM>(new_side, offset)) else {
                continue;
            };
            for &(old_change, old_offset) in matches {
                let Some(old_range) = old_change_range(&changes[old_change].change) else {
                    continue;
                };
                let old_side = &old_ids[old_range.clone()];
                let mut left = 0;
                while old_offset > left
                    && offset > left
                    && old_side[old_offset - left - 1] == new_side[offset - left - 1]
                {
                    left += 1;
                }
                let mut right = 0;
                while old_offset + MOVE_GRAM + right < old_side.len()
                    && offset + MOVE_GRAM + right < new_side.len()
                    && old_side[old_offset + MOVE_GRAM + right]
                        == new_side[offset + MOVE_GRAM + right]
                {
                    right += 1;
                }
                let length = MOVE_GRAM + left + right;
                let run_start = old_range.start + old_offset - left;
                let information = old_ids[run_start..run_start + length]
                    .iter()
                    .map(|id| weights[*id as usize].saturating_sub(1))
                    .sum::<u64>();
                candidates.push((
                    information,
                    length,
                    run_start,
                    new_range.start + offset - left,
                    old_change,
                    new_change,
                ));
            }
        }
    }
    if rule.min_information == 0 {
        // The baseline variant keeps length-first order so it stays a strict
        // fallback for information-weighted selection.
        candidates.sort_unstable_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
        });
    } else {
        candidates.sort_unstable_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
        });
    }
    let mut old_taken = IntervalSet::new();
    let mut new_taken = IntervalSet::new();
    let mut accepted = Vec::new();
    for (information, length, old_start, new_start, old_change, new_change) in candidates {
        if information < rule.min_information || length < MOVE_MIN_TOKENS {
            break;
        }
        if old_taken.overlaps(old_start, length) || new_taken.overlaps(new_start, length) {
            continue;
        }
        old_taken.insert(old_start, length);
        new_taken.insert(new_start, length);
        accepted.push(MoveSpan {
            old_change,
            new_change,
            old: old_start..old_start + length,
            new: new_start..new_start + length,
        });
    }
    accepted
}

fn gram<const N: usize>(values: &[u32], start: usize) -> [u32; N] {
    let mut key = [0; N];
    key.copy_from_slice(&values[start..start + N]);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::model::PageId;

    fn tokens(texts: &[&str]) -> Vec<Token> {
        texts
            .iter()
            .map(|&text| Token {
                text: text.to_owned(),
                page: PageId(0),
                bounds: None,
                block: 0,
                uncertain: false,
                line_break_after: false,
                font_size: None,
                raw: None,
                position: None,
                page_break_after: false,
                normalization: 0,
            })
            .collect()
    }

    #[test]
    fn dot_leader_pages_need_a_dense_dot_pattern() {
        let mut contents = tokens(&["."; 12]);
        contents.extend(tokens(&["a", "b", "c", "d", "e", "f", "g", "h"]));
        for token in &mut contents[..12] {
            token.page = PageId(0);
        }
        for token in &mut contents[12..] {
            token.page = PageId(1);
        }
        let cache = DotLeaderCache::new(&contents, &contents);
        assert!(cache.contents_promotion_possible());
        assert!(cache.old_page(PageId(0)));
        assert!(!cache.old_page(PageId(1)));
        assert!(!cache.old_page(PageId(9)));
    }

    #[test]
    fn a_script_from_a_base_matches_the_direct_builder() {
        let old = tokens(&["k0", "k1", "d0", "m0", "m1", "m2", "m3", "m4", "t0"]);
        let new = tokens(&["k0", "k1", "e0", "t0", "m0", "m1", "m2", "m3", "m4"]);
        let spine = [(0, 0), (1, 1), (8, 3)];
        let (old_ids, new_ids) = crate::intern_tokens(&old, &new);
        let weights = crate::align::information_weights(&old_ids, &new_ids);
        let dot_leaders = DotLeaderCache::new(&old, &new);
        let rule = MoveRule {
            allow_same_page: true,
            min_information: MOVE_MIN_INFORMATION_WEIGHT,
        };
        let context = ScriptContext {
            old: &old,
            new: &new,
            old_ids: &old_ids,
            new_ids: &new_ids,
            weights: &weights,
            dot_leaders: &dot_leaders,
        };
        let direct = build_script_and_promotions(&context, &spine, rule);
        let base = build_script_base(&old, &new, &spine);
        let split = script_from_base(&context, &base, rule);
        assert_eq!(direct, split);
    }

    #[test]
    fn a_token_order_change_is_never_layout_only() {
        let old = tokens(&["alpha", "beta", "gamma", "delta"]);
        let new = tokens(&["gamma", "delta", "alpha", "beta"]);
        assert!(!layout_only_equal(&old, &new));
        let diff = crate::diff(&old, &new).expect("test script verifies");
        assert!(
            !diff
                .changes
                .iter()
                .any(|change| matches!(change, Change::LayoutOnly { .. })),
            "{:?}",
            diff.changes
        );
        assert!(crate::selects_difference(&diff.stats()));
    }

    #[test]
    fn reflowed_tokens_remain_layout_only() {
        let old = tokens(&["alpha", "beta", "gamma"]);
        let new = tokens(&["alphabeta", "gamma"]);
        assert!(layout_only_equal(&old, &new));
    }

    #[test]
    fn a_rotation_remainder_around_a_move_becomes_a_move() {
        let old = tokens(&["a", "r", "m0", "m1", "m2", "m3", "m4", "b"]);
        let new = tokens(&["a", "m0", "m1", "m2", "m3", "m4", "r", "b"]);
        let (old_ids, new_ids) = crate::intern_tokens(&old, &new);
        let mut changes = vec![
            Change::Delete { old: 1..2 },
            Change::Move {
                old: 2..7,
                new: 1..6,
            },
            Change::Insert { new: 6..7 },
        ];
        fold_rotation_remainders(&mut changes, &old_ids, &new_ids);
        assert_eq!(
            changes,
            [
                Change::Move {
                    old: 1..2,
                    new: 6..7,
                },
                Change::Move {
                    old: 2..7,
                    new: 1..6,
                },
            ]
        );
    }

    #[test]
    fn unrelated_short_equal_runs_stay_delete_and_insert() {
        // The equal runs are beside the same end of the move, not opposite
        // ends, so they are not a rotation.
        let old = tokens(&["a", "m0", "m1", "m2", "m3", "m4", "r", "b"]);
        let new = tokens(&["a", "m0", "m1", "m2", "m3", "m4", "b"]);
        let (old_ids, new_ids) = crate::intern_tokens(&old, &new);
        let mut changes = vec![
            Change::Move {
                old: 1..6,
                new: 1..6,
            },
            Change::Delete { old: 6..7 },
        ];
        fold_rotation_remainders(&mut changes, &old_ids, &new_ids);
        assert_eq!(
            changes,
            [
                Change::Move {
                    old: 1..6,
                    new: 1..6,
                },
                Change::Delete { old: 6..7 },
            ]
        );
    }

    #[test]
    fn moves_extract_from_replacements_with_remainders() {
        let old = tokens(&["k0", "k1", "d0", "m0", "m1", "m2", "m3", "m4", "t0"]);
        let new = tokens(&["k0", "k1", "e0", "t0", "m0", "m1", "m2", "m3", "m4"]);
        let changes = build_script(&old, &new, &[(0, 0), (1, 1), (8, 3)]);
        assert_eq!(
            changes,
            [
                Change::Replace {
                    old: 2..3,
                    new: 2..3,
                },
                Change::Move {
                    old: 3..8,
                    new: 4..9,
                },
            ]
        );
    }

    #[test]
    fn in_place_runs_are_promoted_instead_of_moved() {
        // Both sides of a single replacement are the same location. The shared
        // run is an alignment miss, not a relocation: it is reported as a
        // promotion for the caller to force back into the spine, never emitted
        // as a move by the script builder on its own.
        let changes = vec![GapChange {
            gap: 0,
            change: Change::Replace {
                old: 0..7,
                new: 0..12,
            },
        }];
        let old = tokens(&["a0", "a1", "a2", "a3", "a4", "a5", "a6"]);
        let new = tokens(&[
            "a0", "a1", "a2", "a3", "a4", "a5", "x", "a0", "a1", "a2", "a3", "a4",
        ]);
        let old_ids = [1, 2, 3, 4, 5, 6, 7];
        let new_ids = [1, 2, 3, 4, 5, 6, 8, 2, 3, 4, 5, 6];
        let weights = [1u64, 4, 4, 4, 4, 4, 4, 1, 1];
        let rule = MoveRule {
            allow_same_page: true,
            min_information: MOVE_MIN_INFORMATION_WEIGHT,
        };
        let dot_leaders = DotLeaderCache::new(&old, &new);
        let context = ScriptContext {
            old: &old,
            new: &new,
            old_ids: &old_ids,
            new_ids: &new_ids,
            weights: &weights,
            dot_leaders: &dot_leaders,
        };
        let moves = detect_moves(&context, &changes, rule);
        let promotions = in_place_promotions(&context, &changes, rule, &moves);
        assert_eq!(promotions, [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]);
        let script = build_script(&old, &new, &[]);
        assert!(
            !script
                .iter()
                .any(|change| matches!(change, Change::Move { .. })),
            "a same-gap run must not be emitted as a move: {script:?}"
        );
    }

    #[test]
    fn moves_extract_from_replacements_leaving_insertions() {
        let old = tokens(&["k0", "k1", "m0", "m1", "m2", "m3", "m4", "t0", "t1"]);
        let new = tokens(&[
            "k0", "k1", "e0", "e1", "t0", "t1", "m0", "m1", "m2", "m3", "m4",
        ]);
        let changes = build_script(&old, &new, &[(0, 0), (1, 1), (7, 4), (8, 5)]);
        assert_eq!(
            changes,
            [
                Change::Move {
                    old: 2..7,
                    new: 6..11,
                },
                Change::Insert { new: 2..4 },
            ]
        );
    }
}
