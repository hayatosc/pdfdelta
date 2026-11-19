//! Verifiable token-stream diff engine.
//!
//! The engine flattens a document into one token sequence in reconstructed
//! reading order and then computes an edit script between two such sequences.
//! Layout contributes only the order in which tokens are visited; block, line,
//! and page boundaries are retained as metadata and never constrain the diff.
//!
//! # Correctness contract
//!
//! Any script that transforms the old token sequence into the new token
//! sequence is a correct diff. [`flow_diff`] therefore always replays its own
//! script through [`crate::verify::verify`] before returning: the script must cover every old
//! and every new token exactly once, unmoved content must be equal, move
//! endpoints must be equal, and layout-only changes must satisfy the
//! layout-only relation. Which correct script is chosen is a quality decision
//! measured by benchmarks, not a claim that needs a proof.
//!
//! # Token identity
//!
//! Mapped text is split into maximal alphanumeric runs and single punctuation
//! characters; whitespace is dropped. Unmapped glyphs never disappear: each is
//! kept as one token whose text encodes the font hash and glyph id, so two
//! occurrences match only when they are the same unmapped glyph, and the token
//! is marked [`FlowToken::uncertain`].

pub mod align;
pub mod order;
pub mod script;
pub mod verify;

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

mod furniture;

pub use furniture::{FurnitureChange, FurnitureKind, FurnitureLine, furniture_changes};

use pdfdelta_core::{
    Result,
    layout::{Block, BlockOptions, Line, LineOptions, reconstruct_blocks, reconstruct_lines},
    model::{DecodedText, Document, FontProgramHash, Glyph, GlyphId, PageId, Rect, Vec2},
    normalize::{BlockText, ComparableToken, TextSource, normalize_blocks_for_flow},
};

pub use order::{
    FlowCandidates, FlowOrderPair, ORDER_AGREEMENT_MIN, ORDER_COHERENCE_MIN, OrderKind,
    PageOrderEvidence, flow_candidates, flow_candidates_owned, flow_order_from_candidates,
    flow_tokens_pair,
};
pub use script::{FlowChange, build_script};
pub use verify::{VerifyError, verify};

/// One word or punctuation token in document reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowToken {
    /// Canonical token text.
    ///
    /// For mapped text this is the exact canonical scalar sequence. For an
    /// unmapped glyph it is an encoded identity (see [`flow_tokens`]) so that
    /// identity equality is text equality.
    pub text: String,
    /// Page the token's first source glyph was painted on.
    pub page: PageId,
    /// Union of the source glyph bounding boxes, when any source glyph is known.
    pub bounds: Option<Rect>,
    /// Index of the reconstructed block that produced the token.
    pub block: usize,
    /// The token's text is not decoded, so its comparison identity is the
    /// unmapped-glyph key rather than visible text.
    pub uncertain: bool,
    /// A source line break immediately follows this token.
    pub line_break_after: bool,
}

/// Flattened token sequence of one document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowStream {
    /// Tokens in reconstructed reading order, without page furniture.
    pub tokens: Vec<FlowToken>,
    /// Repeated headers, footers, and page numbers, tracked separately.
    pub furniture: Vec<FurnitureLine>,
    /// Glyph accounting between extraction and the token stream.
    pub conservation: Conservation,
}

/// Per-page glyph accounting for one flow stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageConservation {
    /// Page the counts refer to.
    pub page: PageId,
    /// Comparison-visible glyphs extracted on this page.
    pub visible: usize,
    /// Visible glyphs referenced by a block source or the fallback.
    pub used: usize,
}

/// Glyph accounting for one flow stream.
///
/// A side is conserving only when every comparison-visible glyph is referenced
/// by the sources of a block token (including whitespace-only sources) or was
/// appended by the fallback in content-stream order. Unmapped glyphs are
/// tokens, so they count as used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conservation {
    /// Total visible glyphs.
    pub visible: usize,
    /// Visible glyphs accounted for by a block or the fallback.
    pub used: usize,
    /// Per-page detail, sorted by page.
    pub pages: Vec<PageConservation>,
}

impl Default for PageConservation {
    fn default() -> Self {
        Self {
            page: PageId(0),
            visible: 0,
            used: 0,
        }
    }
}

impl Conservation {
    /// Returns whether every visible glyph is accounted for.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.used == self.visible
    }
}

/// Verified diff of two token sequences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowDiff {
    /// Equal pairs `(old_index, new_index)`, strictly increasing on both sides.
    pub spine: Vec<(usize, usize)>,
    /// Changes covering every non-spine token exactly once.
    pub changes: Vec<FlowChange>,
    stats: FlowStats,
}

impl FlowDiff {
    /// Token and per-kind change counts of this diff.
    #[must_use]
    pub fn stats(&self) -> FlowStats {
        self.stats
    }
}

/// Number of changes and covered tokens of one change kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeStats {
    /// Number of changes of this kind.
    pub changes: usize,
    /// Old tokens covered by changes of this kind.
    pub old_tokens: usize,
    /// New tokens covered by changes of this kind.
    pub new_tokens: usize,
}

/// Token and change counts of a verified flow diff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlowStats {
    /// Number of old tokens.
    pub old_tokens: usize,
    /// Number of new tokens.
    pub new_tokens: usize,
    /// Old tokens whose text survives into the new stream.
    ///
    /// This counts spine tokens, uncovered equal tokens, moved tokens, and
    /// layout-only tokens, and excludes tokens covered by deletions and
    /// replacements.
    pub equal_tokens: usize,
    /// Deletions.
    pub delete: ChangeStats,
    /// Insertions.
    pub insert: ChangeStats,
    /// Replacements.
    pub replace: ChangeStats,
    /// Moves.
    pub moves: ChangeStats,
    /// Layout-only changes.
    pub layout_only: ChangeStats,
}

impl FlowStats {
    fn new(old_tokens: usize, new_tokens: usize, changes: &[FlowChange]) -> Self {
        let mut stats = Self {
            old_tokens,
            new_tokens,
            ..Self::default()
        };
        for change in changes {
            let bucket = match change {
                FlowChange::Delete { .. } => &mut stats.delete,
                FlowChange::Insert { .. } => &mut stats.insert,
                FlowChange::Replace { .. } => &mut stats.replace,
                FlowChange::Move { .. } => &mut stats.moves,
                FlowChange::LayoutOnly { .. } => &mut stats.layout_only,
            };
            bucket.changes += 1;
            bucket.old_tokens += change.old_range().len();
            bucket.new_tokens += change.new_range().len();
        }
        stats.equal_tokens = old_tokens - stats.delete.old_tokens - stats.replace.old_tokens;
        stats
    }
}

/// Flattens `document` into a token stream using default layout options.
///
/// # Errors
///
/// Returns an error when line or block reconstruction fails, when
/// normalization rejects the reconstructed structure, or when normalization
/// resource limits are exceeded.
pub fn flow_tokens(document: &Document<Glyph>) -> Result<FlowStream> {
    flow_tokens_with_options(document, LineOptions::default(), BlockOptions::default())
}

/// Flattens `document` into a token stream with explicit layout options.
///
/// Glyphs that are not comparison visible are dropped before reconstruction,
/// exactly like the comparison pipeline. Runs of letters and digits form one
/// token; each CJK scalar is its own token. Unmapped glyph tokens are marked
/// [`FlowToken::uncertain`] because their text is not decoded.
///
/// # Errors
///
/// Returns an error when line or block reconstruction fails, when
/// normalization rejects the reconstructed structure, or when normalization
/// resource limits are exceeded.
pub fn flow_tokens_with_options(
    document: &Document<Glyph>,
    line_options: LineOptions,
    block_options: BlockOptions,
) -> Result<FlowStream> {
    let visible = visible_document(document);
    let glyphs = GlyphLookup::new(&visible);
    let conservation = conservation_of(&visible);
    let lines = reconstruct_lines(&visible, line_options)?;
    let blocks_layout = reconstruct_blocks(&visible, &lines, block_options)?;
    stream_from_layout(
        &visible,
        &glyphs,
        &conservation,
        &lines,
        &blocks_layout,
        true,
    )
}

/// Counts comparison-visible glyphs per page for token-stream conservation.
///
/// Every visible glyph is either referenced by a block source or emitted by
/// the token-stream fallback, so the per-page used count equals the visible
/// count and the accounts are complete by construction. The result depends
/// only on the visible glyphs, so every reading-order hypothesis reuses it.
fn conservation_of(visible: &Document<Glyph>) -> Conservation {
    let items = visible.items();
    let mut pages = page_glyph_counts(items)
        .into_iter()
        .map(|(page, visible)| PageConservation {
            page,
            visible,
            used: visible,
        })
        .collect::<Vec<_>>();
    pages.sort_unstable_by_key(|entry| entry.page);
    Conservation {
        visible: items.len(),
        used: items.len(),
        pages,
    }
}

/// Counts glyphs per page, using a direct table for dense page ids.
///
/// Extracted documents number their pages sequentially. The table is capped
/// at twice the glyph count so a sparse or hostile page id cannot force a
/// large allocation; such documents fall back to a hash map.
fn page_glyph_counts(glyphs: &[Glyph]) -> HashMap<PageId, usize> {
    let Some(max) = glyphs.iter().map(|glyph| glyph.page.0).max() else {
        return HashMap::new();
    };
    let width = usize::try_from(max).ok().and_then(|max| max.checked_add(1));
    if let Some(width) = width.filter(|width| *width <= glyphs.len().saturating_mul(2).max(1024)) {
        let mut counts = vec![0usize; width];
        for glyph in glyphs {
            let index = usize::try_from(glyph.page.0).expect("bounded by the table width");
            counts[index] += 1;
        }
        return counts
            .into_iter()
            .enumerate()
            .filter(|(_, count)| *count > 0)
            .map(|(index, count)| {
                (
                    PageId(u32::try_from(index).expect("table index comes from a u32 page id")),
                    count,
                )
            })
            .collect();
    }
    let mut counts = HashMap::new();
    for glyph in glyphs {
        *counts.entry(glyph.page).or_default() += 1;
    }
    counts
}

/// Limits a document to glyphs that participate in comparison.
pub(crate) fn visible_document(document: &Document<Glyph>) -> Document<Glyph> {
    let kept = document
        .items()
        .iter()
        .map(Glyph::is_comparison_visible)
        .collect::<Vec<_>>();
    Document::with_vector_lines(
        document
            .items()
            .iter()
            .filter(|glyph| glyph.is_comparison_visible())
            .cloned()
            .collect(),
        document.vector_lines().to_vec(),
    )
    .with_displacements(document.filtered_displacements(&kept))
}

/// Limits a document to comparison glyphs without cloning its glyphs.
///
/// The flow engine reads only glyphs and vector lines from the document:
/// reading-order reconstruction, normalization, and tokenization never read
/// displacement evidence. The filtered document therefore keeps the glyph
/// vector in place (`retain` shifts the kept items without reallocating) and
/// drops the unused sidecars.
pub(crate) fn visible_document_owned(document: Document<Glyph>) -> Document<Glyph> {
    let (mut items, vector_lines) = document.into_parts();
    items.retain(Glyph::is_comparison_visible);
    Document::with_vector_lines(items, vector_lines)
}

/// Builds a flow stream from explicit lines and blocks.
///
/// `strip_furniture` removes repeated headers, footers, and page numbers. The
/// order hypotheses keep furniture in place until a page order is chosen so
/// both candidates are compared on the same tokens. `glyphs` is the shared
/// lookup for `visible`, and `conservation` the shared per-page glyph
/// accounting; the caller reuses both across order hypotheses because neither
/// depends on the line or block layout.
pub(crate) fn stream_from_layout(
    visible: &Document<Glyph>,
    glyphs: &GlyphLookup<'_>,
    conservation: &Conservation,
    lines: &[Line],
    blocks_layout: &[Block],
    strip_furniture: bool,
) -> Result<FlowStream> {
    let blocks = normalize_blocks_for_flow(visible, lines, blocks_layout)?;
    let block_count = blocks.len();
    let mut tokens = Vec::new();
    let mut referenced = ReferencedGlyphs::new(glyphs, visible.items().len());
    for (index, block) in blocks.into_iter().enumerate() {
        let mut block_tokens = block_tokens(index, &block, glyphs)?;
        if let Some(last) = block_tokens.last_mut() {
            // A block boundary is always at least a line boundary, so the last
            // token keeps the line-break evidence without a within-block offset.
            last.line_break_after = true;
        }
        // Canonical text is what tokenisation sees; white-space scalars are
        // included so their source glyphs are accounted for even though they
        // never become tokens themselves.
        block
            .canonical
            .for_each_referenced_glyph(|glyph| referenced.insert(glyph));
        tokens.extend(block_tokens);
    }

    // Every visible glyph is either referenced by a block source or emitted by
    // the fallback below, so the fallback must visit exactly the unreferenced
    // glyphs; conservation already accounts for all of them.
    let mut fallback = HashMap::<PageId, Vec<FlowToken>>::new();
    for glyph in visible.items() {
        if referenced.contains(glyph.id) {
            continue;
        }
        emit_fallback_tokens(glyph, block_count, fallback.entry(glyph.page).or_default());
    }
    let tokens = insert_fallback_tokens(tokens, fallback);
    let (tokens, furniture) = if strip_furniture {
        furniture::split(tokens)
    } else {
        (tokens, Vec::new())
    };
    Ok(FlowStream {
        tokens,
        furniture,
        conservation: conservation.clone(),
    })
}

/// Appends fallback tokens to the end of their page's token run.
fn insert_fallback_tokens(
    tokens: Vec<FlowToken>,
    mut fallback: HashMap<PageId, Vec<FlowToken>>,
) -> Vec<FlowToken> {
    if fallback.is_empty() {
        return tokens;
    }
    let mut out = Vec::with_capacity(tokens.len());
    let mut current_page = None;
    for token in tokens {
        if current_page.is_some_and(|page| page != token.page)
            && let Some(page) = current_page
            && let Some(mut extra) = fallback.remove(&page)
        {
            out.append(&mut extra);
        }
        current_page = Some(token.page);
        out.push(token);
    }
    if let Some(page) = current_page
        && let Some(mut extra) = fallback.remove(&page)
    {
        out.append(&mut extra);
    }
    let mut remaining = fallback.into_iter().collect::<Vec<_>>();
    remaining.sort_unstable_by_key(|(page, _)| *page);
    for (_, mut extra) in remaining {
        out.append(&mut extra);
    }
    out
}

/// Tokenises one visible glyph that no block referenced.
///
/// This is the conservation fallback: the glyph was extracted but the layout or
/// normalization dropped it, so it is appended in content-stream order and
/// marked uncertain instead of disappearing.
fn emit_fallback_tokens(glyph: &Glyph, block: usize, out: &mut Vec<FlowToken>) {
    let mut push = |text: String, uncertain: bool| {
        out.push(FlowToken {
            text,
            page: glyph.page,
            bounds: Some(glyph.bbox),
            block,
            uncertain,
            line_break_after: false,
        });
    };
    match &glyph.text {
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => push(unmapped_token_text(font_hash, *glyph_id), true),
        DecodedText::Mapped(text) => {
            let scalars = text.chars().collect::<Vec<_>>();
            let mut index = 0;
            while index < scalars.len() {
                let scalar = scalars[index];
                if scalar.is_whitespace() {
                    index += 1;
                } else if scalar.is_alphanumeric() && !is_cjk_scalar(scalar) {
                    let mut word = String::new();
                    while index < scalars.len()
                        && scalars[index].is_alphanumeric()
                        && !is_cjk_scalar(scalars[index])
                    {
                        word.push(scalars[index]);
                        index += 1;
                    }
                    push(word, true);
                } else {
                    push(scalar.to_string(), true);
                    index += 1;
                }
            }
        }
    }
}

/// Interns the token texts of both streams into one shared vocabulary.
///
/// Equal texts receive equal ids on both sides, and ids are assigned in
/// first-seen order over `old` then `new`. Ids are only stable within one call:
/// they are an alignment aid, not a persistent identity.
///
/// # Panics
///
/// Panics if the combined vocabulary exceeds `u32::MAX` distinct texts; token
/// streams are bounded by document size, so this is unreachable in practice.
#[must_use]
pub fn intern_tokens(old: &[FlowToken], new: &[FlowToken]) -> (Vec<u32>, Vec<u32>) {
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut intern = |text: &str| -> u32 {
        if let Some(&id) = ids.get(text) {
            return id;
        }
        let id = u32::try_from(ids.len()).expect("token vocabulary exceeds u32");
        ids.insert(text.to_owned(), id);
        id
    };
    let old_ids = old.iter().map(|token| intern(&token.text)).collect();
    let new_ids = new.iter().map(|token| intern(&token.text)).collect();
    (old_ids, new_ids)
}

/// Computes and verifies an edit script transforming `old` into `new`.
///
/// # Errors
///
/// Returns [`VerifyError`] when the produced script fails replay, which would
/// indicate a bug in the alignment or script builder. Callers never receive an
/// unverified script.
/// Equal pairs of one alignment, strictly increasing on both sides.
type Spine = Rc<Vec<(usize, usize)>>;

/// One promoted-pair set used as an alignment key.
type PromotedSet = Vec<(usize, usize)>;

/// A spine together with the changes between its pairs.
type CandidateScript = (Spine, Vec<FlowChange>);

/// One evaluated alignment/script variant during diff selection.
struct ScriptCandidate {
    count: usize,
    rule_rank: u8,
    strategy: AlignmentStrategy,
    spine: Spine,
    changes: Vec<FlowChange>,
}

/// The permissive move rule: any detected run may be a move.
const BASELINE_RULE: script::MoveRule = script::MoveRule {
    allow_same_page: false,
    min_information: 0,
};

/// The information-weighted move rule: a run of minimum-weight tokens is not
/// a move.
const INFORMATION_RULE: script::MoveRule = script::MoveRule {
    allow_same_page: false,
    min_information: script::MOVE_MIN_INFORMATION_WEIGHT,
};

/// The contents-page rule: also promote same-page dot-leader runs that sit
/// beside numeric or low-information changes.
const CONTENTS_RULE: script::MoveRule = script::MoveRule {
    allow_same_page: true,
    min_information: script::MOVE_MIN_INFORMATION_WEIGHT,
};

/// Memoizes document-level alignments by strategy and forced spine pairs.
///
/// A document pair re-aligns the same promoted pair set up to six times: the
/// three move rules start from the same empty set, and a promotion sequence
/// that a later rule repeats hits the same keys again. Alignment is a pure
/// function of the strategy, the token ids and weights, the boundaries, and the
/// promoted pairs, so one result serves every later request with that key.
struct AlignmentCache {
    entries: HashMap<(AlignmentStrategy, PromotedSet), Spine>,
    refined: HashMap<(AlignmentStrategy, PromotedSet), Rc<RefinedScript>>,
    segments: align::SegmentCache,
}

/// A residual-gap refinement and the gap classification it produced.
struct RefinedScript {
    spine: Spine,
    base: script::ScriptBase,
}

impl AlignmentCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            refined: HashMap::new(),
            segments: align::SegmentCache::new(),
        }
    }

    /// Returns the alignment for `promoted`, computing it only on a miss.
    fn spine(
        &mut self,
        streams: &DiffStreams<'_>,
        strategy: AlignmentStrategy,
        old_blocks: &[bool],
        new_blocks: &[bool],
        promoted: &[(usize, usize)],
    ) -> Spine {
        let key = (strategy, promoted.to_vec());
        if let Some(cached) = self.entries.get(&key) {
            return Rc::clone(cached);
        }
        let DiffStreams {
            old_ids,
            new_ids,
            weights,
            ..
        } = *streams;
        let aligned = match strategy {
            AlignmentStrategy::Information => align::align_with_fixed_cached(
                old_ids,
                new_ids,
                weights,
                promoted,
                &mut self.segments,
            ),
            AlignmentStrategy::Runs => align::align_flow_cached(
                old_ids,
                new_ids,
                weights,
                promoted,
                old_blocks,
                new_blocks,
                &mut self.segments,
            ),
        };
        let aligned = Rc::new(aligned);
        self.entries.insert(key, Rc::clone(&aligned));
        aligned
    }

    /// Returns the refined spine and its classified base script for `promoted`.
    ///
    /// Both are pure functions of the alignment and the token streams, so the
    /// three rules that share one alignment also share them.
    fn refine(
        &mut self,
        strategy: AlignmentStrategy,
        promoted: &[(usize, usize)],
        spine: &Spine,
        streams: &DiffStreams<'_>,
    ) -> Rc<RefinedScript> {
        let DiffStreams {
            old,
            new,
            old_ids,
            new_ids,
            ..
        } = *streams;
        let key = (strategy, promoted.to_vec());
        if let Some(cached) = self.refined.get(&key) {
            return Rc::clone(cached);
        }
        let spine = Rc::new(refine_residual_gaps(spine, old_ids, new_ids));
        let base = script::build_script_base(old, new, &spine);
        let refined = Rc::new(RefinedScript { spine, base });
        self.refined.insert(key, Rc::clone(&refined));
        refined
    }

    /// Drops the cached alignments of one strategy.
    fn release(&mut self, strategy: AlignmentStrategy) {
        self.entries.retain(|(cached, _), _| *cached != strategy);
        self.refined.retain(|(cached, _), _| *cached != strategy);
    }
}

/// The best variant so far plus the last script recorded for deduplication.
#[derive(Default)]
struct Selection {
    best: Option<ScriptCandidate>,
    considered: Option<CandidateScript>,
}

impl Selection {
    /// Records a variant unless the same spine and script were already recorded.
    ///
    /// The three rules record in descending rank order, so an identical script
    /// recorded earlier carries a higher rank and dominates this one.
    fn record(
        &mut self,
        old: &[FlowToken],
        new: &[FlowToken],
        strategy: AlignmentStrategy,
        rule_rank: u8,
        spine: Spine,
        changes: Vec<FlowChange>,
    ) {
        if let Some((previous_spine, previous_changes)) = &self.considered
            && Rc::ptr_eq(previous_spine, &spine)
            && *previous_changes == changes
        {
            return;
        }
        self.considered = Some((Rc::clone(&spine), changes.clone()));
        self.consider(old, new, strategy, rule_rank, spine, changes);
    }

    /// Applies the boundary bridge and keeps the best variant seen so far.
    fn consider(
        &mut self,
        old: &[FlowToken],
        new: &[FlowToken],
        strategy: AlignmentStrategy,
        rule_rank: u8,
        spine: Spine,
        changes: Vec<FlowChange>,
    ) {
        let (spine, changes) = bridge_equal_boundaries(old, new, &spine, changes);
        let count = changed_token_count(&changes);
        let better = match &self.best {
            None => true,
            Some(candidate) => {
                count < candidate.count
                    || (count == candidate.count && rule_rank > candidate.rule_rank)
                    || (count == candidate.count
                        && rule_rank == candidate.rule_rank
                        && strategy_rank(strategy) < strategy_rank(candidate.strategy))
            }
        };
        if better {
            self.best = Some(ScriptCandidate {
                count,
                rule_rank,
                strategy,
                spine: Rc::new(spine),
                changes,
            });
        }
    }
}

pub fn flow_diff(
    old: &[FlowToken],
    new: &[FlowToken],
) -> std::result::Result<FlowDiff, VerifyError> {
    let (old_ids, new_ids) = intern_tokens(old, new);
    let weights = align::information_weights(&old_ids, &new_ids);
    let old_blocks = block_starts(old);
    let new_blocks = block_starts(new);
    // Information-weighted LCS keeps the most informative tokens matched in
    // small gaps. Anchor-first alignment keeps longer runs and block
    // boundaries intact, which can turn a relocated paragraph into a Move
    // instead of a set of word replacements. Run both and keep the script that
    // changes fewer tokens; on a tie prefer the block-aware script, whose
    // changes start and end on reconstructed block boundaries when repeated
    // boundary tokens allow both.
    let streams = DiffStreams {
        old,
        new,
        old_ids: &old_ids,
        new_ids: &new_ids,
        weights: &weights,
    };
    // The information-weighted move rule and the same-page table-of-contents
    // promotion can each change which valid script is best. Evaluate the old
    // rule and both new variants, and keep the script with the fewest changed
    // tokens (moves counted by their old-side length). On a tie prefer the
    // richer rule, then the block-aware strategy.
    //
    // The rules differ only in how they rewrite the gaps of one alignment, so
    // two variants provably coincide without being evaluated again:
    //
    // * No candidate exists at all, so every rule leaves the script unchanged.
    // * Neither document has a dot-leader page, so the contents-page promotion
    //   of the richest rule cannot fire and it matches the information-weighted
    //   rule exactly.
    //
    // When several rules produce the identical script, the selection keeps the
    // highest rule rank, because that is the variant the full evaluation would
    // have preferred at equal changed-token counts.
    let dot_leaders = script::DotLeaderCache::new(old, new);
    let mut cache = AlignmentCache::new();
    let mut selection = Selection::default();
    for strategy in [AlignmentStrategy::Information, AlignmentStrategy::Runs] {
        let (baseline_spine, baseline_changes, baseline_candidates) = diff_once(
            &streams,
            strategy,
            &old_blocks,
            &new_blocks,
            BASELINE_RULE,
            &mut cache,
            &dot_leaders,
        );
        if !baseline_candidates {
            selection.record(old, new, strategy, 2, baseline_spine, baseline_changes);
            cache.release(strategy);
            continue;
        }
        let (information_spine, information_changes, _) = diff_once(
            &streams,
            strategy,
            &old_blocks,
            &new_blocks,
            INFORMATION_RULE,
            &mut cache,
            &dot_leaders,
        );
        let (richer_spine, richer_changes) = if dot_leaders.contents_promotion_possible() {
            let (spine, changes, _) = diff_once(
                &streams,
                strategy,
                &old_blocks,
                &new_blocks,
                CONTENTS_RULE,
                &mut cache,
                &dot_leaders,
            );
            (spine, changes)
        } else {
            (Rc::clone(&information_spine), information_changes.clone())
        };
        // Record every rule with its own rank. When two rules produce the same
        // script the higher rank already dominates the lower one, so the extra
        // entry changes neither the selected script nor the tie-breaking.
        selection.record(old, new, strategy, 2, richer_spine, richer_changes);
        selection.record(
            old,
            new,
            strategy,
            1,
            information_spine,
            information_changes,
        );
        selection.record(old, new, strategy, 0, baseline_spine, baseline_changes);
        cache.release(strategy);
    }
    let best = selection
        .best
        .expect("at least one diff variant was evaluated");
    let spine = Rc::try_unwrap(best.spine).unwrap_or_else(|shared| (*shared).clone());
    let changes = best.changes;
    verify(old, new, &changes)?;
    let stats = FlowStats::new(old.len(), new.len(), &changes);
    Ok(FlowDiff {
        spine,
        changes,
        stats,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AlignmentStrategy {
    Information,
    Runs,
}

struct DiffStreams<'a> {
    old: &'a [FlowToken],
    new: &'a [FlowToken],
    old_ids: &'a [u32],
    new_ids: &'a [u32],
    weights: &'a [u64],
}

/// Adds plain LCS matches inside every gap left by the weighted spine. Existing
/// pairs are retained, so the refinement can only reduce changed tokens.
fn refine_residual_gaps(
    spine: &[(usize, usize)],
    old_ids: &[u32],
    new_ids: &[u32],
) -> Vec<(usize, usize)> {
    let mut budget = align::residual_gap_total_cells();
    if spine.is_empty() {
        let matches = align::residual_lcs(old_ids, new_ids, &mut budget);
        return matches;
    }
    let mut refined = Vec::with_capacity(spine.len());
    let (mut old_start, mut new_start) = (0, 0);
    for &(old_index, new_index) in spine {
        // Aligners return a strictly increasing spine, but a caller may pass a
        // forced or bridged spine. Ignore a pair that would make either slice
        // range run backwards; residual LCS can still match it in the gap.
        if old_index < old_start || new_index < new_start {
            continue;
        }
        refined.extend(
            align::residual_lcs(
                &old_ids[old_start..old_index],
                &new_ids[new_start..new_index],
                &mut budget,
            )
            .into_iter()
            .map(|(old, new)| (old_start + old, new_start + new)),
        );
        refined.push((old_index, new_index));
        old_start = old_index + 1;
        new_start = new_index + 1;
    }
    refined.extend(
        align::residual_lcs(&old_ids[old_start..], &new_ids[new_start..], &mut budget)
            .into_iter()
            .map(|(old, new)| (old_start + old, new_start + new)),
    );
    refined.sort_unstable();
    refined.dedup();
    refined
}

/// Ordering used to break equal changed-token counts between strategies.
fn strategy_rank(strategy: AlignmentStrategy) -> u8 {
    match strategy {
        AlignmentStrategy::Runs => 0,
        AlignmentStrategy::Information => 1,
    }
}

fn diff_once(
    streams: &DiffStreams<'_>,
    strategy: AlignmentStrategy,
    old_blocks: &[bool],
    new_blocks: &[bool],
    rule: script::MoveRule,
    cache: &mut AlignmentCache,
    dot_leaders: &script::DotLeaderCache,
) -> (Spine, Vec<FlowChange>, bool) {
    let DiffStreams {
        old,
        new,
        old_ids,
        new_ids,
        weights,
    } = *streams;
    let context = script::ScriptContext {
        old,
        new,
        old_ids,
        new_ids,
        weights,
        dot_leaders,
    };
    let mut promoted: Vec<(usize, usize)> = Vec::new();
    let mut first_round_candidates = None;
    loop {
        let spine = cache.spine(streams, strategy, old_blocks, new_blocks, &promoted);
        let (round_changes, promotions, candidates) =
            script::build_script_and_promotions(&context, &spine, rule);
        if first_round_candidates.is_none() {
            first_round_candidates = Some(candidates);
        }
        // Promotions are kept sorted and deduplicated, so membership is a
        // binary search even for tens of thousands of forced pairs.
        let mut fresh = promotions
            .into_iter()
            .filter(|pair| promoted.binary_search(pair).is_err())
            .collect::<Vec<_>>();
        fresh.sort_unstable();
        fresh.dedup();
        if fresh.is_empty() {
            let refined = cache.refine(strategy, &promoted, &spine, streams);
            // The residual-gap refinement normally adds equal pairs, but when
            // every gap is already fully matched the spine is unchanged and the
            // script just built is the answer.
            let changes = if *refined.spine == **spine {
                round_changes
            } else {
                script::script_from_base(&context, &refined.base, rule).0
            };
            return (
                Rc::clone(&refined.spine),
                changes,
                first_round_candidates.unwrap_or(false),
            );
        }
        promoted.extend(fresh);
        promoted.sort_unstable();
        promoted.dedup();
        if promoted.len() > old.len().saturating_add(new.len()) {
            let refined = cache.refine(strategy, &promoted, &spine, streams);
            let changes = script::script_from_base(&context, &refined.base, rule).0;
            return (
                Rc::clone(&refined.spine),
                changes,
                first_round_candidates.unwrap_or(false),
            );
        }
    }
}

/// Maximum number of changes for which the rotation rewrite is attempted.
///
/// The rotation search pairs equal delete/insert changes. Its index keeps the
/// per-change work constant, but documents with tens of thousands of changes
/// still gain very little from rotations whose moved runs are shorter than the
/// move-detection gram. The move-remainder rewrite has no such limit and is
/// applied to every document.
const ROTATION_MAX_CHANGES: usize = 2_048;

/// Maximum number of accepted rewrites in one bridge pass.
const MAX_BRIDGE_REWRITES: usize = 256;

/// Maximum number of accepted rotation rewrites in one bridge pass.
const MAX_ROTATION_REWRITES: usize = 32;

/// Maximum equal-text insert candidates checked for one delete.
const MAX_ROTATION_CANDIDATES: usize = 16;

/// A rewritten equal-pair spine and edit script.
type BridgedScript = (Vec<(usize, usize)>, Vec<FlowChange>);

/// Reconciles equal tokens that the spine left on opposite sides of a run.
///
/// Two constructions are tried, each accepted only when replay verification
/// passes and each strictly reducing the changed-token count:
///
/// * A move can leave a short equal remainder outside its run: the remainder
///   follows the move on the old side while its equal counterpart precedes the
///   move on the new side. Removing the delete/insert pair makes the tokens
///   implicit equal tokens.
/// * An equal delete/insert pair can straddle a contiguous spine run that is
///   shifted by the pair's length. Moving that run and pairing the two equal
///   runs as implicit equal tokens replaces two content changes with one move,
///   which is the correct representation of a rotation.
///
/// Both rewrites rebuild the spine so the reported equal pairs stay complete.
/// A candidate that does not verify, or whose spine stops being increasing on
/// both sides, is discarded and the original script is returned unchanged.
fn bridge_equal_boundaries(
    old: &[FlowToken],
    new: &[FlowToken],
    spine: &[(usize, usize)],
    mut changes: Vec<FlowChange>,
) -> BridgedScript {
    let mut rewritten: Option<Vec<(usize, usize)>> = None;
    let mut rotations = 0usize;
    for _ in 0..MAX_BRIDGE_REWRITES {
        let current = rewritten.as_deref().unwrap_or(spine);
        if let Some((next_spine, next_changes)) =
            try_drop_move_remainder(old, new, current, &changes)
        {
            rewritten = Some(next_spine);
            changes = next_changes;
            continue;
        }
        if rotations >= MAX_ROTATION_REWRITES {
            break;
        }
        if let Some((next_spine, next_changes)) = try_rotate_spine_run(old, new, current, &changes)
        {
            rewritten = Some(next_spine);
            changes = next_changes;
            rotations += 1;
        } else {
            break;
        }
    }
    (rewritten.unwrap_or_else(|| spine.to_vec()), changes)
}

/// Drops an equal delete/insert pair that sits on opposite sides of a move.
fn try_drop_move_remainder(
    old: &[FlowToken],
    new: &[FlowToken],
    spine: &[(usize, usize)],
    changes: &[FlowChange],
) -> Option<BridgedScript> {
    let mut delete_by_start = HashMap::new();
    let mut delete_by_end = HashMap::new();
    let mut insert_by_start = HashMap::new();
    let mut insert_by_end = HashMap::new();
    for (index, change) in changes.iter().enumerate() {
        match change {
            FlowChange::Delete { old: range } => {
                delete_by_start.insert(range.start, index);
                delete_by_end.insert(range.end, index);
            }
            FlowChange::Insert { new: range } => {
                insert_by_start.insert(range.start, index);
                insert_by_end.insert(range.end, index);
            }
            _ => {}
        }
    }
    for change in changes {
        let FlowChange::Move {
            old: move_old,
            new: move_new,
        } = change
        else {
            continue;
        };
        // The equal remainder is after the move on the old side and before it
        // on the new side, or the mirror.
        let pairs = [
            (
                delete_by_start.get(&move_old.end),
                insert_by_end.get(&move_new.start),
            ),
            (
                delete_by_end.get(&move_old.start),
                insert_by_start.get(&move_new.end),
            ),
        ];
        for (delete_index, insert_index) in pairs {
            let (Some(&delete_index), Some(&insert_index)) = (delete_index, insert_index) else {
                continue;
            };
            let (FlowChange::Delete { old: deleted }, FlowChange::Insert { new: inserted }) =
                (&changes[delete_index], &changes[insert_index])
            else {
                continue;
            };
            let deleted = deleted.clone();
            let inserted = inserted.clone();
            if deleted.len() != inserted.len()
                || !old[deleted.clone()]
                    .iter()
                    .zip(&new[inserted.clone()])
                    .all(|(left, right)| left.text == right.text)
            {
                continue;
            }
            let mut candidate_spine = spine.to_vec();
            candidate_spine.extend(
                (0..deleted.len()).map(|offset| (deleted.start + offset, inserted.start + offset)),
            );
            candidate_spine.sort_unstable();
            if !strictly_increasing(&candidate_spine) {
                continue;
            }
            let mut candidate = changes.to_vec();
            remove_two(&mut candidate, delete_index, insert_index);
            if verify(old, new, &candidate).is_ok() {
                return Some((candidate_spine, candidate));
            }
        }
    }
    None
}

/// Moves a contiguous spine run so an equal delete/insert pair becomes equal.
fn try_rotate_spine_run(
    old: &[FlowToken],
    new: &[FlowToken],
    spine: &[(usize, usize)],
    changes: &[FlowChange],
) -> Option<BridgedScript> {
    if changes.len() > ROTATION_MAX_CHANGES || spine.is_empty() {
        return None;
    }
    // The spine is strictly increasing on both sides, so both coordinate
    // lookups are binary searches and no id-to-id maps are built.
    let run_from = run_lengths(spine);
    let old_to_new = |old_index: usize| {
        spine
            .binary_search_by_key(&old_index, |pair| pair.0)
            .ok()
            .map(|position| spine[position].1)
    };
    let new_to_old = |new_index: usize| {
        spine
            .binary_search_by_key(&new_index, |pair| pair.1)
            .ok()
            .map(|position| spine[position].0)
    };
    let run_at = |old_index: usize| {
        spine
            .binary_search_by_key(&old_index, |pair| pair.0)
            .ok()
            .map(|position| run_from[position])
    };
    let key_text = |tokens: &[FlowToken]| {
        tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<Vec<_>>()
            .join("\u{1f}")
    };
    let mut inserts_by_text: HashMap<(usize, String), Vec<usize>> = HashMap::new();
    for (index, change) in changes.iter().enumerate() {
        if let FlowChange::Insert { new: range } = change {
            inserts_by_text
                .entry((range.len(), key_text(&new[range.clone()])))
                .or_default()
                .push(index);
        }
    }
    for (delete_index, change) in changes.iter().enumerate() {
        let FlowChange::Delete { old: deleted } = change else {
            continue;
        };
        if deleted.is_empty() {
            continue;
        }
        let key = (deleted.len(), key_text(&old[deleted.clone()]));
        let Some(candidates) = inserts_by_text.get(&key) else {
            continue;
        };
        for &insert_index in candidates.iter().take(MAX_ROTATION_CANDIDATES) {
            let FlowChange::Insert { new: inserted } = &changes[insert_index] else {
                continue;
            };
            let mut rotations = Vec::with_capacity(2);
            // Run starts right after the insert and ends before the delete.
            if let Some(start) = new_to_old(inserted.end) {
                let length = deleted.start.saturating_sub(start);
                if length > 0 && run_at(start).is_some_and(|run| run >= length) {
                    rotations.push((start..deleted.start, inserted.end..inserted.end + length));
                }
            }
            // Run starts right after the delete and ends before the insert.
            if let Some(start_new) = old_to_new(deleted.end) {
                let length = inserted.start.saturating_sub(start_new);
                if length > 0 && run_at(deleted.end).is_some_and(|run| run >= length) {
                    rotations.push((deleted.end..deleted.end + length, start_new..inserted.start));
                }
            }
            for (move_old, move_new) in rotations {
                if move_old.end > old.len() || move_new.end > new.len() || move_old.is_empty() {
                    continue;
                }
                let mut candidate_spine = spine
                    .iter()
                    .copied()
                    .filter(|(old_index, _)| !move_old.contains(old_index))
                    .collect::<Vec<_>>();
                candidate_spine.extend(
                    (0..deleted.len())
                        .map(|offset| (deleted.start + offset, inserted.start + offset)),
                );
                candidate_spine.sort_unstable();
                if !strictly_increasing(&candidate_spine) {
                    continue;
                }
                let mut candidate = changes.to_vec();
                remove_two(&mut candidate, delete_index, insert_index);
                let move_change = FlowChange::Move {
                    old: move_old.clone(),
                    new: move_new,
                };
                let position = candidate
                    .iter()
                    .position(|candidate| {
                        let range = candidate.old_range();
                        !range.is_empty() && range.start > move_old.start
                    })
                    .unwrap_or(candidate.len());
                candidate.insert(position, move_change);
                if verify(old, new, &candidate).is_ok() {
                    return Some((candidate_spine, candidate));
                }
            }
        }
    }
    None
}

/// Consecutive increasing spine run length starting at each spine position.
fn run_lengths(spine: &[(usize, usize)]) -> Vec<usize> {
    let mut runs = vec![1usize; spine.len()];
    for position in (0..spine.len().saturating_sub(1)).rev() {
        let next = spine[position + 1];
        let current = spine[position];
        if next.0 == current.0 + 1 && next.1 == current.1 + 1 {
            runs[position] = runs[position + 1] + 1;
        }
    }
    runs
}

fn remove_two(changes: &mut Vec<FlowChange>, first: usize, second: usize) {
    let (high, low) = if first > second {
        (first, second)
    } else {
        (second, first)
    };
    changes.remove(high);
    changes.remove(low);
}

fn strictly_increasing(spine: &[(usize, usize)]) -> bool {
    spine
        .windows(2)
        .all(|pair| pair[0].0 < pair[1].0 && pair[0].1 < pair[1].1)
}

fn changed_token_count(changes: &[FlowChange]) -> usize {
    changes
        .iter()
        .map(|change| match change {
            FlowChange::Delete { old } => old.len(),
            FlowChange::Insert { new } => new.len(),
            FlowChange::Replace { old, new } => old.len().saturating_add(new.len()),
            FlowChange::Move { .. } | FlowChange::LayoutOnly { .. } => 0,
        })
        .fold(0usize, usize::saturating_add)
}

/// Marks the first token of every reconstructed block.
fn block_starts(tokens: &[FlowToken]) -> Vec<bool> {
    tokens
        .iter()
        .enumerate()
        .map(|(index, token)| index == 0 || tokens[index - 1].block != token.block)
        .collect()
}

/// Id lookup for comparison-visible glyphs.
///
/// Extracted glyph ids are dense and sequential, so the common case uses a
/// direct slot vector with O(1) lookups and no hashing. Programmatic
/// fixtures may use sparse or out-of-range ids; those fall back to an
/// id-sorted vector with binary search.
pub(crate) enum GlyphLookup<'a> {
    Direct(Vec<Option<&'a Glyph>>),
    Sorted(Vec<(GlyphId, &'a Glyph)>),
}

impl<'a> GlyphLookup<'a> {
    /// Indexes every glyph of `document`.
    ///
    /// Duplicate ids force the id-sorted representation; normalization rejects
    /// such documents before any token lookup happens.
    pub(crate) fn new(document: &'a Document<Glyph>) -> Self {
        let items = document.items();
        let len = items.len() as u64;
        if items.iter().all(|glyph| glyph.id.0 < len) {
            let mut direct = vec![None; items.len()];
            let mut unique = true;
            for glyph in items {
                let slot = usize::try_from(glyph.id.0).expect("validated against len above");
                if direct[slot].replace(glyph).is_some() {
                    unique = false;
                    break;
                }
            }
            if unique {
                return Self::Direct(direct);
            }
        }
        let mut entries = items
            .iter()
            .map(|glyph| (glyph.id, glyph))
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|(id, _)| *id);
        Self::Sorted(entries)
    }

    pub(crate) fn get(&self, id: GlyphId) -> Option<&'a Glyph> {
        match self {
            Self::Direct(glyphs) => usize::try_from(id.0)
                .ok()
                .and_then(|index| glyphs.get(index))
                .copied()
                .flatten(),
            Self::Sorted(entries) => entries
                .binary_search_by(|(candidate, _)| candidate.cmp(&id))
                .ok()
                .map(|index| entries[index].1),
        }
    }

    /// Returns the slot count when ids are dense and directly indexable.
    fn dense_len(&self) -> Option<usize> {
        match self {
            Self::Direct(glyphs) => Some(glyphs.len()),
            Self::Sorted(_) => None,
        }
    }
}

/// Membership set for glyph ids referenced by block sources.
///
/// Dense sequential ids use one byte per slot, so the fallback check costs no
/// hashing; sparse or out-of-range ids fall back to a hash set.
enum ReferencedGlyphs {
    Dense(Vec<bool>),
    Hashed(HashSet<GlyphId>),
}

impl ReferencedGlyphs {
    fn new(glyphs: &GlyphLookup<'_>, capacity: usize) -> Self {
        glyphs.dense_len().map_or_else(
            || Self::Hashed(HashSet::with_capacity(capacity)),
            |len| Self::Dense(vec![false; len]),
        )
    }

    fn insert(&mut self, id: GlyphId) {
        match self {
            Self::Dense(seen) => {
                let index = usize::try_from(id.0).expect("dense glyph ids fit usize");
                if let Some(slot) = seen.get_mut(index) {
                    *slot = true;
                }
            }
            Self::Hashed(seen) => {
                seen.insert(id);
            }
        }
    }

    fn contains(&self, id: GlyphId) -> bool {
        match self {
            Self::Dense(seen) => usize::try_from(id.0)
                .ok()
                .and_then(|index| seen.get(index))
                .copied()
                .unwrap_or(false),
            Self::Hashed(seen) => seen.contains(&id),
        }
    }
}

/// Builds the flow tokens of one normalized block.
pub(crate) fn block_tokens(
    block_index: usize,
    block: &BlockText,
    glyphs: &GlyphLookup<'_>,
) -> Result<Vec<FlowToken>> {
    let tokens = block.canonical.comparable_tokens_with_sources()?;
    let break_after = line_break_flags(&tokens, block.line_breaks.as_deref());
    let fallback_page = block.pages.first().copied().map_or(PageId(0), PageId);
    // Tokens never outnumber comparable tokens; whitespace scalars are dropped.
    let mut out = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        match &tokens[index].0 {
            ComparableToken::Unmapped {
                font_hash,
                glyph_id,
            } => {
                let source = &tokens[index].1;
                out.push(FlowToken {
                    text: unmapped_token_text(font_hash, *glyph_id),
                    page: source_page(source, glyphs).unwrap_or(fallback_page),
                    bounds: source_bounds(source, glyphs),
                    block: block_index,
                    uncertain: true,
                    line_break_after: break_after[index],
                });
                index += 1;
            }
            ComparableToken::Scalar(scalar) if scalar.is_whitespace() => {
                index += 1;
            }
            ComparableToken::Scalar(scalar) => {
                let is_word = scalar.is_alphanumeric() && !is_cjk_scalar(*scalar);
                let mut text = String::new();
                text.push(*scalar);
                let mut page = source_page(&tokens[index].1, glyphs);
                let mut bounds = source_bounds(&tokens[index].1, glyphs);
                let mut line_break = break_after[index];
                index += 1;
                if is_word {
                    while let Some((ComparableToken::Scalar(next), source)) = tokens.get(index) {
                        if !next.is_alphanumeric() || is_cjk_scalar(*next) {
                            break;
                        }
                        text.push(*next);
                        if let Some(found) = source_page(source, glyphs) {
                            page.get_or_insert(found);
                        }
                        merge_bounds(&mut bounds, source_bounds(source, glyphs));
                        line_break |= break_after[index];
                        index += 1;
                    }
                }
                out.push(FlowToken {
                    text,
                    page: page.unwrap_or(fallback_page),
                    bounds,
                    block: block_index,
                    uncertain: false,
                    line_break_after: line_break,
                });
            }
        }
    }
    Ok(out)
}

/// Marks the last non-whitespace canonical token before every line break.
fn line_break_flags(
    tokens: &[(ComparableToken, TextSource)],
    offsets: Option<&[usize]>,
) -> Vec<bool> {
    let mut flags = vec![false; tokens.len()];
    let Some(offsets) = offsets else {
        return flags;
    };
    for &offset in offsets {
        let mut index = offset.min(tokens.len());
        while index > 0 {
            index -= 1;
            if !matches!(tokens[index].0, ComparableToken::Scalar(c) if c.is_whitespace()) {
                flags[index] = true;
                break;
            }
        }
    }
    flags
}

/// Returns whether a scalar is written without spaces between words.
///
/// Han, kana, Hangul, and CJK punctuation are tokenized one scalar at a time so
/// that an edit inside a sentence changes only the affected characters. The
/// ranges intentionally include compatibility and fullwidth forms; they avoid a
/// Unicode data dependency, so unassigned code points in a block are treated
/// like their assigned neighbors.
fn is_cjk_scalar(scalar: char) -> bool {
    matches!(
        u32::from(scalar),
        0x1100..=0x11FF
            | 0x2E80..=0x2EFF
            | 0x2F00..=0x2FDF
            | 0x3000..=0x303F
            | 0x3040..=0x309F
            | 0x30A0..=0x30FF
            | 0x3130..=0x318F
            | 0x31F0..=0x31FF
            | 0x3200..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7AF
            | 0xD7B0..=0xD7FF
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE1F
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x1B000..=0x1B16F
            | 0x20000..=0x2FA1F
            | 0x30000..=0x323AF
    )
}

/// Encodes the stable identity of one unmapped glyph as token text.
///
/// The leading object-replacement character marks the text as synthetic. Two
/// occurrences are equal exactly when the identity hash and glyph id are
/// equal.
///
/// The text is the object-replacement character, the identity source
/// (`name`, `outline`, or `program`) as a label, the identity hash as
/// lowercase hex, a colon, and the decimal glyph id; it never collides with
/// mapped text because mapped canonical text cannot contain U+FFFC. The label
/// makes the evidence behind a placeholder visible in change text and token
/// sidecars. `write!` formats the integer without an intermediate allocation.
fn unmapped_token_text(font_hash: &FontProgramHash, glyph_id: u16) -> String {
    use std::fmt::Write as _;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let kind = font_hash.identity_source().name();
    let mut text = String::with_capacity(3 + kind.len() + 2 * font_hash.0.len() + 1 + 5);
    text.push('\u{fffc}');
    text.push_str(kind);
    text.push(':');
    for byte in &font_hash.0 {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text.push(':');
    let _ = write!(text, "{glyph_id}");
    text
}

fn source_page(source: &TextSource, glyphs: &GlyphLookup<'_>) -> Option<PageId> {
    source.atoms.iter().find_map(|atom| match atom {
        pdfdelta_core::normalize::TextSourceAtom::Glyph(id) => {
            glyphs.get(*id).map(|glyph| glyph.page)
        }
        _ => None,
    })
}

fn source_bounds(source: &TextSource, glyphs: &GlyphLookup<'_>) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for atom in &source.atoms {
        if let pdfdelta_core::normalize::TextSourceAtom::Glyph(id) = atom
            && let Some(glyph) = glyphs.get(*id)
        {
            merge_bounds(&mut bounds, Some(glyph.bbox));
        }
    }
    bounds
}

fn merge_bounds(target: &mut Option<Rect>, addition: Option<Rect>) {
    let Some(addition) = addition else {
        return;
    };
    match target {
        Some(current) => {
            current.min = Vec2 {
                x: current.min.x.min(addition.min.x),
                y: current.min.y.min(addition.min.y),
            };
            current.max = Vec2 {
                x: current.max.x.max(addition.max.x),
                y: current.max.y.max(addition.max.y),
            };
        }
        None => *target = Some(addition),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use pdfdelta_core::model::{
        DecodedText, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus, GlyphProvenance,
        PageId, Rect, TextRenderMode, Vec2,
    };
    use pdfdelta_core::pdf::ObjectRef;

    fn glyph(
        id: u64,
        text: DecodedText,
        page: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Glyph {
        Glyph {
            id: GlyphId(id),
            raw_code: match &text {
                DecodedText::Mapped(mapped) => mapped.as_bytes().to_vec(),
                DecodedText::Unmapped { glyph_id, .. } => glyph_id.to_be_bytes().to_vec(),
            },
            text,
            page: PageId(page),
            bbox: Rect {
                min: Vec2 { x, y },
                max: Vec2 {
                    x: x + width,
                    y: y + height,
                },
            },
            baseline: Vec2 { x, y },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: u32::try_from(id).expect("test glyph ids fit u32"),
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: u32::try_from(id).expect("test glyph ids fit u32"),
            },
        }
    }

    /// Builds a document from `(line text, page, baseline y)` triples.
    ///
    /// Each non-space character becomes one glyph with a 6pt advance and a 5pt
    /// bounding box; spaces are gaps wide enough for synthetic-space recovery.
    fn document(lines: &[(&str, u32, f64)]) -> Document<Glyph> {
        let mut glyphs = Vec::new();
        let mut id = 1u64;
        for &(text, page, y) in lines {
            let mut x = 0.0;
            for character in text.chars() {
                let advance = if character == ' ' { 5.0 } else { 6.0 };
                if character != ' ' {
                    glyphs.push(glyph(
                        id,
                        DecodedText::Mapped(character.to_string()),
                        page,
                        x,
                        y,
                        advance - 1.0,
                        10.0,
                    ));
                    id += 1;
                }
                x += advance;
            }
        }
        Document::new(glyphs)
    }

    fn token_texts(tokens: &[FlowToken]) -> Vec<&str> {
        tokens.iter().map(|token| token.text.as_str()).collect()
    }

    fn replacement(
        old_lines: &[(&str, u32, f64)],
        new_lines: &[(&str, u32, f64)],
    ) -> Vec<FlowChange> {
        let old = flow_tokens(&document(old_lines)).expect("old tokens");
        let new = flow_tokens(&document(new_lines)).expect("new tokens");
        flow_diff(&old.tokens, &new.tokens)
            .expect("verified diff")
            .changes
    }

    #[test]
    fn cjk_text_is_tokenised_per_scalar() {
        let stream =
            flow_tokens(&document(&[("日本語abc漢字、かな。", 0, 300.0)])).expect("CJK tokens");
        assert_eq!(
            token_texts(&stream.tokens),
            ["日", "本", "語", "abc", "漢", "字", "、", "か", "な", "。"]
        );
    }

    #[test]
    fn cjk_replacement_changes_one_scalar() {
        let changes = replacement(
            &[("今日は良い天気です", 0, 300.0)],
            &[("今日は悪い天気です", 0, 300.0)],
        );
        assert_eq!(changes.len(), 1, "{changes:?}");
        let FlowChange::Replace { old, new } = &changes[0] else {
            panic!("expected a replacement, got {:?}", changes[0]);
        };
        assert_eq!((old.len(), new.len()), (1, 1));
    }

    #[test]
    fn stats_count_every_token_once() {
        let old =
            flow_tokens(&document(&[("alpha beta gamma delta", 0, 300.0)])).expect("old tokens");
        let new =
            flow_tokens(&document(&[("alpha beta epsilon delta", 0, 300.0)])).expect("new tokens");
        let diff = flow_diff(&old.tokens, &new.tokens).expect("verified diff");
        let stats = diff.stats();
        assert_eq!(stats.old_tokens, 4);
        assert_eq!(stats.new_tokens, 4);
        assert_eq!(stats.equal_tokens, 3);
        assert_eq!(stats.replace.changes, 1);
        assert_eq!(stats.replace.old_tokens, 1);
        assert_eq!(stats.replace.new_tokens, 1);
        assert_eq!(stats.delete.changes, 0);
        assert_eq!(stats.insert.changes, 0);
        assert_eq!(stats.moves.changes, 0);
        assert_eq!(stats.layout_only.changes, 0);
    }

    #[test]
    fn line_wrap_changes_nothing() {
        let changes = replacement(
            &[("alpha beta gamma", 0, 300.0), ("delta epsilon", 0, 270.0)],
            &[
                ("alpha beta", 0, 300.0),
                ("gamma delta", 0, 270.0),
                ("epsilon", 0, 240.0),
            ],
        );
        assert!(
            changes.is_empty(),
            "wrapping must not change content: {changes:?}"
        );
    }

    #[test]
    fn page_break_changes_nothing() {
        let changes = replacement(
            &[("alpha beta", 0, 300.0), ("gamma delta", 0, 270.0)],
            &[("alpha beta", 0, 300.0), ("gamma delta", 1, 300.0)],
        );
        assert!(
            changes.is_empty(),
            "moving a line to another page must not change content: {changes:?}"
        );
    }

    #[test]
    fn equal_remainder_beside_a_move_is_not_a_content_change() {
        // The moved run is rotated around an equal period: the old period
        // follows the run while the new period precedes it, so neither the
        // gap LCS nor move detection can pair them. They must not be reported
        // as a delete/insert pair.
        let words = |texts: &[&str]| {
            texts
                .iter()
                .map(|text| (*text).to_owned())
                .collect::<Vec<_>>()
        };
        let old = token_stream(&words(&[
            "x0", "x1", "m0", "m1", "m2", "m3", "m4", ".", "y0", "y1",
        ]));
        let new = token_stream(&words(&[
            "x0", "x1", ".", "m0", "m1", "m2", "m3", "m4", "y0", "y1",
        ]));
        let diff = flow_diff(&old, &new).expect("rotation script must verify");
        assert_eq!(diff.stats().delete.changes, 0, "{:?}", diff.changes);
        assert_eq!(diff.stats().insert.changes, 0, "{:?}", diff.changes);
        assert_eq!(diff.stats().replace.changes, 0, "{:?}", diff.changes);
        assert!(
            diff.changes
                .iter()
                .any(|change| matches!(change, FlowChange::Move { .. })),
            "{:?}",
            diff.changes
        );
        assert!(
            diff.spine.contains(&(7, 2)),
            "the bridged period must join the spine: {:?}",
            diff.spine
        );
    }

    #[test]
    fn short_page_swap_is_a_move_not_content_changes() {
        // Both runs are shorter than the move-detection gram, so alignment
        // matches one run in place and leaves the other as a delete/insert
        // pair. The rotation rewrite must still represent the swap with a
        // move alone.
        let words = |texts: &[&str]| {
            texts
                .iter()
                .map(|text| (*text).to_owned())
                .collect::<Vec<_>>()
        };
        let old = token_stream(&words(&["a0", "a1", "a2", "b0", "b1", "b2", "b3"]));
        let new = token_stream(&words(&["b0", "b1", "b2", "b3", "a0", "a1", "a2"]));
        let diff = flow_diff(&old, &new).expect("short swap script must verify");
        assert_eq!(diff.stats().delete.changes, 0, "{:?}", diff.changes);
        assert_eq!(diff.stats().insert.changes, 0, "{:?}", diff.changes);
        assert_eq!(diff.stats().replace.changes, 0, "{:?}", diff.changes);
        assert!(
            diff.changes
                .iter()
                .any(|change| matches!(change, FlowChange::Move { .. })),
            "{:?}",
            diff.changes
        );
    }

    #[test]
    fn text_replacement_is_one_change() {
        let changes = replacement(
            &[("the quick brown fox", 0, 300.0)],
            &[("the quick red fox", 0, 300.0)],
        );
        assert_eq!(changes.len(), 1, "{changes:?}");
        match &changes[0] {
            FlowChange::Replace { old, new } => {
                assert_eq!(old.len(), 1);
                assert_eq!(new.len(), 1);
            }
            other => panic!("expected one replacement, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_insertion_is_one_change() {
        let changes = replacement(
            &[("alpha beta gamma", 0, 300.0), ("delta epsilon", 0, 270.0)],
            &[
                ("alpha beta gamma", 0, 300.0),
                ("inserted paragraph here", 0, 270.0),
                ("delta epsilon", 0, 240.0),
            ],
        );
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(&changes[0], FlowChange::Insert { .. }));
    }

    #[test]
    fn paragraph_deletion_is_one_change() {
        let changes = replacement(
            &[
                ("alpha beta gamma", 0, 300.0),
                ("deleted paragraph here", 0, 270.0),
                ("delta epsilon", 0, 240.0),
            ],
            &[("alpha beta gamma", 0, 300.0), ("delta epsilon", 0, 270.0)],
        );
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(&changes[0], FlowChange::Delete { .. }));
    }

    #[test]
    fn rehyphenation_is_a_layout_only_change() {
        let changes = replacement(
            &[("inter-", 0, 300.0), ("national", 0, 270.0)],
            &[("international", 0, 300.0)],
        );
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(&changes[0], FlowChange::LayoutOnly { .. }));
    }

    #[test]
    fn unmapped_token_text_names_the_identity_source() {
        for (tag, label) in [(0x01, "program"), (0x02, "name"), (0x03, "outline")] {
            let mut bytes = vec![0x11; 32];
            bytes[0] = tag;
            let hash = pdfdelta_core::model::FontProgramHash(bytes);
            let text = unmapped_token_text(&hash, 7);
            assert!(
                text.starts_with(&format!("\u{fffc}{label}:")),
                "unexpected placeholder text {text:?}"
            );
            assert!(text.ends_with(":7"));
        }
    }

    #[test]
    fn unmapped_glyphs_stay_visible_and_uncertain() {
        let unmapped = DecodedText::Unmapped {
            font_hash: pdfdelta_core::model::FontProgramHash(vec![1, 2, 3]),
            glyph_id: 7,
        };
        let old = Document::new(vec![glyph(1, unmapped.clone(), 0, 0.0, 100.0, 5.0, 10.0)]);
        let new = Document::new(vec![glyph(1, unmapped, 0, 0.0, 100.0, 5.0, 10.0)]);
        let old = flow_tokens(&old).expect("old tokens");
        let new = flow_tokens(&new).expect("new tokens");
        assert_eq!(old.tokens.len(), 1);
        assert!(old.tokens[0].uncertain);
        let diff = flow_diff(&old.tokens, &new.tokens).expect("verified diff");
        assert!(diff.changes.is_empty());
    }

    #[test]
    fn toc_page_number_shifts_are_replacements_not_moves() {
        let old = token_stream(
            &[
                "aload",
                "413",
                "aload",
                "_",
                "<",
                "n",
                ">",
                "414",
                "anewarray",
                "415",
                "areturn",
                "416",
            ]
            .map(str::to_owned),
        );
        let new = token_stream(
            &[
                "aload",
                "415",
                "aload",
                "_",
                "<",
                "n",
                ">",
                "416",
                "anewarray",
                "417",
                "areturn",
                "418",
            ]
            .map(str::to_owned),
        );
        let diff = flow_diff(&old, &new).expect("verified diff");
        assert!(
            diff.changes
                .iter()
                .all(|change| matches!(change, FlowChange::Replace { .. })),
            "{:?}",
            diff.changes
        );
        assert_eq!(diff.changes.len(), 4);
        for change in &diff.changes {
            for index in change.old_range() {
                assert!(
                    old[index]
                        .text
                        .chars()
                        .all(|scalar| scalar.is_ascii_digit())
                );
            }
            for index in change.new_range() {
                assert!(
                    new[index]
                        .text
                        .chars()
                        .all(|scalar| scalar.is_ascii_digit())
                );
            }
        }
    }

    fn line_token(text: &str, page: u32, y: f64) -> FlowToken {
        FlowToken {
            text: text.to_owned(),
            page: PageId(page),
            bounds: Some(Rect {
                min: Vec2 { x: 0.0, y },
                max: Vec2 {
                    x: 40.0,
                    y: y + 8.0,
                },
            }),
            block: 0,
            uncertain: false,
            line_break_after: true,
        }
    }

    #[test]
    fn repeated_footers_are_tracked_separately() {
        let bodies = ["alpha", "beta", "gamma", "delta"];
        let mut tokens = Vec::new();
        for (page, body) in bodies.iter().enumerate() {
            let page = page as u32;
            tokens.push(line_token(body, page, 300.0));
            tokens.push(line_token(&format!("page {} of 9", page + 1), page, 20.0));
        }
        let (kept, furniture) = furniture::split(tokens);
        assert_eq!(token_texts(&kept), bodies);
        assert_eq!(furniture.len(), 1);
        // The page number varies across pages and is masked; the constant
        // total page count is kept literal so a change in it is reportable.
        assert_eq!(furniture[0].text, "page 0 of 9");
        assert_eq!(
            furniture[0].pages,
            [PageId(0), PageId(1), PageId(2), PageId(3)]
        );
        assert_eq!(furniture[0].occurrences, 4);
    }

    #[test]
    fn repeated_text_at_one_edge_is_not_hidden_when_also_inline() {
        let bodies = ["alpha", "beta", "gamma", "delta"];
        let mut tokens = Vec::new();
        for (page, body) in bodies.iter().enumerate() {
            let page = page as u32;
            tokens.push(line_token("running header", page, 500.0));
            tokens.push(line_token(body, page, 300.0));
            if page == 0 {
                tokens.push(line_token("running header", page, 250.0));
            }
        }
        let (kept, furniture) = furniture::split(tokens);
        assert_eq!(furniture.len(), 1);
        assert_eq!(furniture[0].text, "running header");
        assert_eq!(furniture[0].pages.len(), 4);
        let kept_texts = token_texts(&kept);
        assert_eq!(
            kept_texts
                .iter()
                .filter(|text| **text == "running header")
                .count(),
            1,
            "the inline copy stays in the content stream"
        );
    }

    #[test]
    fn digit_shifted_furniture_is_not_a_furniture_change() {
        let bodies = ["alpha", "beta", "gamma", "delta"];
        let build = |offset: u32| {
            let mut tokens = Vec::new();
            for (page, body) in bodies.iter().enumerate() {
                let page = page as u32;
                tokens.push(line_token(body, page, 300.0));
                tokens.push(line_token(
                    &format!("page {} of 9", page + 1 + offset),
                    page,
                    20.0,
                ));
            }
            furniture::split(tokens)
        };
        let (old_tokens, old_furniture) = build(0);
        let (new_tokens, new_furniture) = build(1);
        let old = FlowStream {
            tokens: old_tokens,
            furniture: old_furniture,
            conservation: Conservation::default(),
        };
        let new = FlowStream {
            tokens: new_tokens,
            furniture: new_furniture,
            conservation: Conservation::default(),
        };
        assert!(furniture_changes(&old, &new).is_empty());
    }

    #[test]
    fn constant_furniture_digits_are_compared_literally() {
        let bodies = ["alpha", "beta", "gamma", "delta"];
        let build = |year: u32| {
            let mut tokens = Vec::new();
            for (page, body) in bodies.iter().enumerate() {
                let page = page as u32;
                tokens.push(line_token(body, page, 300.0));
                tokens.push(line_token(
                    &format!("form {year} page {}", page + 1),
                    page,
                    20.0,
                ));
            }
            furniture::split(tokens)
        };
        let (old_tokens, old_furniture) = build(2023);
        let (new_tokens, new_furniture) = build(2024);
        assert_eq!(old_furniture[0].text, "form 2023 page 0");
        assert_eq!(new_furniture[0].text, "form 2024 page 0");
        let old = FlowStream {
            tokens: old_tokens,
            furniture: old_furniture,
            conservation: Conservation::default(),
        };
        let new = FlowStream {
            tokens: new_tokens,
            furniture: new_furniture,
            conservation: Conservation::default(),
        };
        let changes = furniture_changes(&old, &new);
        assert!(
            changes
                .iter()
                .any(|change| change.kind == FurnitureKind::Removed
                    && change.text == "form 2023 page 0"),
            "the constant year must be reported as removed"
        );
        assert!(
            changes
                .iter()
                .any(|change| change.kind == FurnitureKind::Added
                    && change.text == "form 2024 page 0"),
            "the constant year must be reported as added"
        );
    }

    #[test]
    fn furniture_cluster_ties_are_deterministic() {
        let bodies = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
        ];
        let mut old_lines = Vec::new();
        for (page, body) in bodies.iter().enumerate() {
            let page = u32::try_from(page).expect("test page fits u32");
            old_lines.push((*body, page, 300.0));
            // The same running title sits at the top on four pages and at the
            // bottom on four others, producing two equally frequent, equally
            // edge-valid furniture clusters.
            let y = if page < 4 { 800.0 } else { 20.0 };
            old_lines.push(("running header", page, y));
        }
        let mut new_lines = old_lines.clone();
        new_lines[2] = ("omega", 1, 300.0);
        let old_document = document(&old_lines);
        let new_document = document(&new_lines);
        let baseline_stream = flow_tokens(&old_document).expect("baseline tokens");
        assert_eq!(baseline_stream.furniture.len(), 1);
        assert_eq!(baseline_stream.furniture[0].occurrences, 4);
        let new_stream = flow_tokens(&new_document).expect("new tokens");
        let baseline_diff = flow_diff(&baseline_stream.tokens, &new_stream.tokens)
            .expect("baseline diff")
            .changes;
        for attempt in 0..20 {
            let stream = flow_tokens(&old_document).expect("repeated tokens");
            let diff = flow_diff(&stream.tokens, &new_stream.tokens)
                .expect("repeated diff")
                .changes;
            assert_eq!(stream.tokens, baseline_stream.tokens, "attempt {attempt}");
            assert_eq!(
                stream.furniture, baseline_stream.furniture,
                "attempt {attempt}"
            );
            assert_eq!(diff, baseline_diff, "attempt {attempt}");
        }
    }

    fn token_stream(words: &[String]) -> Vec<FlowToken> {
        words
            .iter()
            .enumerate()
            .map(|(index, word)| FlowToken {
                text: word.clone(),
                page: PageId(0),
                bounds: None,
                block: index / 8,
                uncertain: false,
                line_break_after: false,
            })
            .collect()
    }

    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1))
        }

        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() >> 33) as usize % bound
        }

        fn word(&mut self) -> String {
            format!("w{}", self.below(40))
        }
    }

    fn random_words(lcg: &mut Lcg, len: usize) -> Vec<String> {
        (0..len).map(|_| lcg.word()).collect()
    }

    fn random_range(lcg: &mut Lcg, len: usize) -> std::ops::Range<usize> {
        let start = lcg.below(len);
        let end = (start + 1 + lcg.below(6)).min(len);
        start..end
    }

    fn mutate(lcg: &mut Lcg, words: &mut Vec<String>) {
        match lcg.below(5) {
            0 => {}
            1 => {
                if !words.is_empty() {
                    let range = random_range(lcg, words.len());
                    words.drain(range);
                }
            }
            2 => {
                let at = lcg.below(words.len() + 1);
                for _ in 0..=lcg.below(5) {
                    let word = lcg.word();
                    words.insert(at, word);
                }
            }
            3 => {
                if !words.is_empty() {
                    let range = random_range(lcg, words.len());
                    let replacements = (0..=lcg.below(4)).map(|_| lcg.word()).collect::<Vec<_>>();
                    words.splice(range, replacements);
                }
            }
            _ => {
                if words.len() > 2 {
                    let range = random_range(lcg, words.len());
                    let moved = words.drain(range).collect::<Vec<_>>();
                    let at = lcg.below(words.len() + 1);
                    words.splice(at..at, moved);
                }
            }
        }
    }

    #[test]
    fn randomized_diffs_always_verify() {
        for seed in 0..200 {
            let mut lcg = Lcg::new(seed);
            let length = 10 + lcg.below(50);
            let mut old = random_words(&mut lcg, length);
            let mut new = old.clone();
            for _ in 0..lcg.below(5) {
                mutate(&mut lcg, &mut new);
            }
            // shuffle old so nothing depends on a shared starting point
            let mut shuffled = Vec::with_capacity(old.len());
            while !old.is_empty() {
                let index = lcg.below(old.len());
                shuffled.push(old.remove(index));
            }
            let old_tokens = token_stream(&shuffled);
            let new_tokens = token_stream(&new);
            let diff = flow_diff(&old_tokens, &new_tokens)
                .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
            assert_eq!(
                token_texts(&old_tokens),
                shuffled.iter().map(String::as_str).collect::<Vec<_>>()
            );
            assert!(verify(&old_tokens, &new_tokens, &diff.changes).is_ok());
        }
    }

    #[test]
    fn moves_a_whole_block_with_one_move() {
        let old = (0..26).map(|index| format!("t{index}")).collect::<Vec<_>>();
        let mut new = Vec::new();
        new.extend_from_slice(&old[..10]);
        new.extend_from_slice(&old[16..]);
        new.extend_from_slice(&old[10..16]);
        let old_tokens = token_stream(&old);
        let new_tokens = token_stream(&new);
        let diff = flow_diff(&old_tokens, &new_tokens).expect("verified diff");
        assert!(matches!(&diff.changes[0], FlowChange::Move { .. }));
    }

    #[test]
    fn flow_tokens_account_for_every_visible_glyph() {
        let document = document(&[("alpha beta", 0, 300.0), ("gamma", 1, 300.0)]);
        let stream = flow_tokens(&document).expect("flow tokens");
        let visible = document.items().len();
        assert_eq!(stream.conservation.visible, visible);
        assert_eq!(stream.conservation.used, visible);
        assert!(stream.conservation.is_complete());
    }

    #[test]
    fn fallback_tokens_are_appended_to_their_page_in_content_order() {
        let token = |text: &str, page: u32| FlowToken {
            text: text.to_owned(),
            page: PageId(page),
            bounds: None,
            block: 0,
            uncertain: false,
            line_break_after: false,
        };
        let tokens = vec![token("a", 0), token("b", 1)];
        let mut fallback = HashMap::new();
        fallback.insert(PageId(0), vec![token("x", 0)]);
        fallback.insert(PageId(1), vec![token("y", 1)]);
        let merged = insert_fallback_tokens(tokens, fallback);
        let texts = merged
            .iter()
            .map(|token| token.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, ["a", "x", "b", "y"]);
    }

    #[test]
    fn identical_streams_produce_no_changes() {
        let words = (0..50).map(|index| format!("w{index}")).collect::<Vec<_>>();
        let tokens = token_stream(&words);
        let diff = flow_diff(&tokens, &tokens).expect("verified diff");
        assert!(diff.changes.is_empty());
    }

    #[test]
    fn scaled_tokens_diff_in_reasonable_time() {
        let count = 300_000usize;
        let words = (0..count)
            .map(|index| format!("t{index}"))
            .collect::<Vec<_>>();
        let old = token_stream(&words);
        let mut new = old.clone();
        for index in (0..count).step_by(97) {
            new[index].text = format!("edit{index}");
        }
        let started = Instant::now();
        let diff = flow_diff(&old, &new).expect("verified diff");
        let elapsed = started.elapsed();
        assert!(!diff.changes.is_empty());
        eprintln!("300k token diff took {elapsed:?}");
    }
}
