//! Pure source/position proof for one literal-equal local fragment.
//!
//! The proof answers one narrow question: is this `TextSpan` pair a fragment
//! whose literal text is equal and whose source correspondence is complete on
//! both sides? A [`FragmentVerdict::Proven`] result requires, for every
//! selected token, exactly one canonical glyph and exactly one single-scalar
//! raw counterpart with the same literal, one contiguous raw run, no
//! normalization evidence across the fragment, and bit-exact equal finite
//! horizontal position signatures on one shared page. Any other evidence
//! holds the fragment.
//!
//! The proof is deliberately local and never claims more: it does not
//! establish that the surrounding domain is closed, that the correspondence is
//! globally unique, that the token partition is complete, or that no trailing
//! obligation remains. Those stay caller prerequisites, and the existing
//! candidate, changed-ownership and reading-order protections are unchanged.
//!
//! # Evidence rules
//!
//! - Both spans must project to exactly one nonempty source interval whose
//!   comparable length equals the span's own token count, so a synthetic
//!   inter-block separator or a multi-block span never proves. A span may name
//!   earlier blocks and a separator as long as the real projection lands
//!   inside one later block; only the projected interval is used.
//! - Every selected token must be a literal Unicode scalar; the two sides must
//!   carry the same scalars in the same order.
//! - The selected block must carry no unmapped comparable tokens, so its
//!   block-local comparable index is also its canonical scalar index.
//! - Every selected canonical scalar must be covered by exactly one
//!   single-scalar canonical source-map entry whose single atom is a real
//!   glyph. Unmapped, multi-atom, non-glyph and missing sources hold.
//! - Every selected glyph must have exactly one single-scalar raw source-map
//!   entry with the same literal, and the raw entries must form one increasing
//!   contiguous run. Missing, duplicated, multi-scalar and non-contiguous
//!   counterparts hold.
//! - A selected glyph must occur exactly once in the side's raw sources and
//!   exactly once in its canonical sources, counting every block. Sharing with
//!   any other entry or block holds; one raw and one canonical occurrence in
//!   the same block is the normal case and is not a duplicate.
//! - A normalization event is allowed only when its structure and both of its
//!   side evidence sets are valid, its ranges stay outside the selected run
//!   including the cut edges, and no source atom names or touches a selected
//!   glyph. A zero-length canonical range is a deletion and must additionally
//!   prove that its source has disappeared from the canonical map.
//!   Normalization issues are validated through the existing checked
//!   projection, so an empty, inconsistent or zero-length issue holds even
//!   when it sits outside the selected range, and an issue whose raw or
//!   projected canonical range overlaps the selected range holds as well.
//! - Position signatures must be complete for the whole block, carry finite
//!   baseline and direction geometry, be horizontal and bit-exactly equal on
//!   both sides; both blocks must carry exactly one shared page.
//! - Malformed source maps hold; they are never treated as empty or as a known
//!   content mismatch.
//! - Every scan, comparison and lookup is charged to the shared work counter
//!   before it runs, and every allocation is fallible. Budget exhaustion
//!   returns [`FragmentVerdict::Exhausted`], allocation failure returns the
//!   existing assessment error, and no partial proof is ever reported.
//!
//! # Reuse
//!
//! [`EqualFragmentCache`] borrows the two immutable sides for one assessment
//! and stores only selection-independent results: the document-wide glyph
//! sharing index of each side, the structural validity of every normalization
//! event and the checked issue projection of every block. Selection-dependent
//! checks always run per call, nothing is stored before its charge is paid and
//! its build completed, and an entry can never be used for another side, block
//! or comparison. The proof is connected from a deferred tail pass: the local
//! equal/no-edit gate only records a strict-closed candidate, and after every
//! existing recovery, proof and emission pass the tail pass re-checks the
//! latest ownership and spends only the remaining budget. A held, exhausted
//! or failed proof adopts nothing and leaves the candidate pending.
//!
//! A separately named fallback proof keeps [`EqualFragmentCache::prove`]
//! untouched and adds one narrow allowance: a raw gap between two consecutive
//! selected real glyphs may be certified when it is exactly one raw newline
//! scalar with a singleton `LineBreak` atom naming those glyphs and exactly one
//! validated `SoftLineBreak` event deleting that raw range to a zero-length
//! canonical range at the following selected scalar. The certified break
//! offsets relative to the selected interval must agree on both sides, and
//! every other touching event, issue, sharing, position, page, projection and
//! ownership veto stays unchanged. The fallback reuses the same cache and
//! immutable sides and spends only the caller's remaining budget.

use std::collections::HashSet;

use crate::{
    Error, Result,
    model::GlyphId,
    normalize::{
        BlockText, MappedText, NormalizationEvent, NormalizationKind, PositionSignature,
        ScalarRange, TextSourceAtom, has_duplicate_source_atoms, scalar_range_contains_or_touches,
    },
};

use super::super::{Side, TextSpan};
use super::views::horizontal_direction;
use super::{SourceInterval, allocation_error, charge, invalid, project};

/// Outcome of one equal-fragment source/position proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FragmentVerdict {
    /// The fragment is literal-equal with complete source and position evidence.
    Proven,
    /// The evidence is incomplete, unsupported or contradictory.
    Held(FragmentHold),
    /// The shared work budget ended before the proof completed.
    Exhausted,
}

/// Reason an equal-fragment proof makes no equality claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FragmentHold {
    /// The span did not project to exactly one nonempty source interval with
    /// one comparable token per span token.
    Projection,
    /// A selected token is not a literal Unicode scalar.
    NonScalarToken,
    /// The selected block carries unmapped comparable tokens, so comparable
    /// and scalar coordinates are not one-to-one.
    UnmappedBlock,
    /// The two sides' selected literal scalars differ.
    LiteralMismatch,
    /// A canonical source map is malformed or a selected scalar lacks exactly
    /// one single-glyph source.
    CanonicalSource,
    /// A selected glyph lacks exactly one single-scalar raw counterpart, or the
    /// raw literal differs.
    RawSource,
    /// The selected raw counterparts are not one increasing contiguous run.
    RawCut,
    /// A selected glyph occurs more than once in the side's raw or canonical
    /// sources, including another block.
    SharedGlyph,
    /// The block carries an unsupported normalization event, or a
    /// normalization issue is malformed or overlaps the selected range.
    NormalizationBoundary,
    /// The block does not carry exactly one page.
    MultiplePages,
    /// The two sides' single pages differ.
    PageMismatch,
    /// Complete per-token position signatures are missing.
    MissingPositions,
    /// A selected position is not finite, not horizontal, or differs from the
    /// other side in any bit.
    PositionMismatch,
}

/// Internal stop reason that keeps error propagation separate from verdicts.
enum Stop {
    Held(FragmentHold),
    Exhausted,
    Error(Error),
}

impl From<Error> for Stop {
    fn from(error: Error) -> Self {
        Self::Error(error)
    }
}

/// Internal result of one proof step.
type Check<T> = std::result::Result<T, Stop>;

/// Per-assessment equal-fragment proof cache.
///
/// The cache borrows the two immutable sides for one assessment. Its tables
/// are reserved on first use, each build is charged before it runs and only a
/// completed build is stored, so an exhausted or partial build never becomes a
/// cached result. The entries are indexed by this cache's own side and block,
/// so they can never be reused for another side, block or comparison.
pub(super) struct EqualFragmentCache<'a> {
    sides: [&'a Side<'a>; 2],
    sharing: [Option<SharingIndex>; 2],
    blocks: [Vec<BlockCache>; 2],
    tables_ready: [bool; 2],
}

/// Document-wide glyph occurrence index of one side.
///
/// A glyph recorded in a set occurs more than once in that side's map kind, so
/// it can never be the unique source of a selected token. Counting every block
/// keeps a glyph shared with another block visible; one raw and one canonical
/// occurrence in the same block stays the normal case.
struct SharingIndex {
    shared_raw: HashSet<GlyphId>,
    shared_canonical: HashSet<GlyphId>,
}

impl SharingIndex {
    fn is_shared(&self, glyph: GlyphId) -> bool {
        self.shared_raw.contains(&glyph) || self.shared_canonical.contains(&glyph)
    }
}

/// Selection-independent cached evidence of one block.
#[derive(Default)]
struct BlockCache {
    /// Structural validity per normalization event, `None` before validation.
    events: Option<Vec<Option<bool>>>,
    /// The checked issue projection of the block.
    issues: Option<CachedIssueRanges>,
}

/// The exact result of one checked issue-range validation.
enum CachedIssueRanges {
    /// The complete validated issue ranges of the block.
    Ranges(Vec<ScalarRange>),
    /// The validation failed; the block keeps its issue veto.
    Invalid,
}

impl<'a> EqualFragmentCache<'a> {
    /// Creates an empty cache that borrows both sides for one assessment.
    pub(super) fn new(sides: [&'a Side<'a>; 2]) -> Self {
        Self {
            sides,
            sharing: [None, None],
            blocks: [Vec::new(), Vec::new()],
            tables_ready: [false, false],
        }
    }

    /// Proves or holds one literal-equal source/position-complete fragment.
    ///
    /// `spans` are the old and new span of one proposed correspondence; the
    /// caller has already established the surrounding domain closure and
    /// uniqueness. `remaining` is the shared assessment work counter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] when a span's source coordinates
    /// do not agree with its blocks, [`Error::LimitExceeded`] for count
    /// overflow and [`Error::Unresolved`] when a bounded allocation fails.
    pub(super) fn prove(
        &mut self,
        spans: [&TextSpan; 2],
        remaining: &mut usize,
    ) -> Result<FragmentVerdict> {
        Self::verdict(self.check_fragment(spans, false, remaining))
    }

    /// Test-only convenience wrapper around
    /// [`Self::prove_internal_deleted_soft_line_break_with_certificate`] that
    /// discards the certificate.
    #[cfg(test)]
    pub(super) fn prove_internal_deleted_soft_line_break(
        &mut self,
        spans: [&TextSpan; 2],
        remaining: &mut usize,
    ) -> Result<FragmentVerdict> {
        Self::verdict(self.check_fragment(spans, true, remaining))
    }

    /// Proves or holds one literal-equal fragment that may contain internal
    /// deleted soft line breaks, reporting whether the allowance was used.
    ///
    /// The evidence rules are exactly [`Self::prove`]'s, with one narrow
    /// allowance: a raw gap between two consecutive selected real glyphs is
    /// accepted only when it is exactly one raw newline scalar whose single raw
    /// source atom is a `LineBreak` naming those two glyphs and exactly one
    /// `SoftLineBreak` event deletes that raw range to a zero-length canonical
    /// range at the following selected scalar. The event must pass the full
    /// structural validation, both endpoints must stay strictly inside the
    /// selection, and the certified break offsets relative to the selected
    /// interval must be equal on both sides. Glyph identifiers are never
    /// compared across sides. Malformed, duplicated, conflicting or otherwise
    /// touching events, non-newline deleted raw content, unsupported atoms,
    /// cut-edge breaks, issues and every existing sharing, position, page,
    /// projection and ownership veto hold the fragment.
    ///
    /// The returned certificate is `true` only when at least one internal
    /// deleted soft line break was actually certified. A `Proven` verdict with
    /// `certified == false` is an ordinary contiguous equality, so the caller
    /// must not label it with the internal deleted soft line break premise.
    ///
    /// The caller runs this only after every existing recovery pass has
    /// finished and spends only the remaining shared budget; the cache and its
    /// immutable sides are shared with [`Self::prove`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] when a span's source coordinates
    /// do not agree with its blocks, [`Error::LimitExceeded`] for count
    /// overflow and [`Error::Unresolved`] when a bounded allocation fails.
    pub(super) fn prove_internal_deleted_soft_line_break_with_certificate(
        &mut self,
        spans: [&TextSpan; 2],
        remaining: &mut usize,
    ) -> Result<(FragmentVerdict, bool)> {
        match self.check_fragment(spans, true, remaining) {
            Ok(certified) => Ok((FragmentVerdict::Proven, certified)),
            Err(Stop::Held(hold)) => Ok((FragmentVerdict::Held(hold), false)),
            Err(Stop::Exhausted) => Ok((FragmentVerdict::Exhausted, false)),
            Err(Stop::Error(error)) => Err(error),
        }
    }

    fn verdict(result: Check<bool>) -> Result<FragmentVerdict> {
        match result {
            Ok(_) => Ok(FragmentVerdict::Proven),
            Err(Stop::Held(hold)) => Ok(FragmentVerdict::Held(hold)),
            Err(Stop::Exhausted) => Ok(FragmentVerdict::Exhausted),
            Err(Stop::Error(error)) => Err(error),
        }
    }

    fn check_fragment(
        &mut self,
        spans: [&TextSpan; 2],
        allow_internal_deleted_break: bool,
        remaining: &mut usize,
    ) -> Check<bool> {
        let old = self.analyze_side(0, spans[0], allow_internal_deleted_break, remaining)?;
        let new = self.analyze_side(1, spans[1], allow_internal_deleted_break, remaining)?;
        if !charge(remaining, old.chars.len().saturating_mul(2)) {
            return Err(Stop::Exhausted);
        }
        if old.chars.len() != new.chars.len() || old.chars != new.chars {
            return Err(Stop::Held(FragmentHold::LiteralMismatch));
        }
        if old.page != new.page {
            return Err(Stop::Held(FragmentHold::PageMismatch));
        }
        if old.positions.len() != new.positions.len() {
            return Err(Stop::Held(FragmentHold::PositionMismatch));
        }
        if !charge(remaining, old.positions.len().saturating_mul(2)) {
            return Err(Stop::Exhausted);
        }
        for (old_position, new_position) in old.positions.iter().zip(&new.positions) {
            if old_position != new_position {
                return Err(Stop::Held(FragmentHold::PositionMismatch));
            }
        }
        self.check_side_sharing(0, &old.glyphs, remaining)?;
        self.check_side_sharing(1, &new.glyphs, remaining)?;
        if allow_internal_deleted_break {
            if !charge(remaining, old.breaks.len()) {
                return Err(Stop::Exhausted);
            }
            if old.breaks != new.breaks {
                return Err(Stop::Held(FragmentHold::NormalizationBoundary));
            }
        }
        Ok(allow_internal_deleted_break && !old.breaks.is_empty())
    }

    /// Returns the per-block cache, reserving the side table on first use.
    ///
    /// The reservation is charged and fallible; a failed reservation stores
    /// nothing, so the next call retries it.
    fn block_cache(
        &mut self,
        side_index: usize,
        block_index: usize,
        remaining: &mut usize,
    ) -> Check<&mut BlockCache> {
        let side = self.sides[side_index];
        if !self.tables_ready[side_index] {
            if !charge(remaining, side.blocks.len()) {
                return Err(Stop::Exhausted);
            }
            let mut table = Vec::new();
            table
                .try_reserve_exact(side.blocks.len())
                .map_err(|_| allocation_error("equal fragment block table"))?;
            table.resize_with(side.blocks.len(), BlockCache::default);
            self.blocks[side_index] = table;
            self.tables_ready[side_index] = true;
        }
        Ok(&mut self.blocks[side_index][block_index])
    }

    /// Returns the structural validity of one normalization event.
    ///
    /// The first query pays the full structural validation of that event and
    /// stores the completed result; later queries pay a bounded lookup. A
    /// failed validation is a completed result and stays cached.
    fn event_structure(
        &mut self,
        side_index: usize,
        block_index: usize,
        event_index: usize,
        raw_count: usize,
        scalar_count: usize,
        remaining: &mut usize,
    ) -> Check<bool> {
        let side = self.sides[side_index];
        let block = &side.blocks[block_index];
        let event = &block.normalization_events[event_index];
        let cached = {
            let cache = self.block_cache(side_index, block_index, remaining)?;
            if cache.events.is_none() {
                if !charge(remaining, block.normalization_events.len()) {
                    return Err(Stop::Exhausted);
                }
                let mut table = Vec::new();
                table
                    .try_reserve_exact(block.normalization_events.len())
                    .map_err(|_| allocation_error("equal fragment event table"))?;
                table.resize(block.normalization_events.len(), None);
                cache.events = Some(table);
            }
            cache
                .events
                .as_ref()
                .expect("the event table was just reserved")[event_index]
        };
        if let Some(valid) = cached {
            if !charge(remaining, 1 + event.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            return Ok(valid);
        }
        let valid = event_structure_holds(block, event, raw_count, scalar_count, remaining)?;
        let cache = self.block_cache(side_index, block_index, remaining)?;
        cache
            .events
            .as_mut()
            .expect("the event table was just reserved")[event_index] = Some(valid);
        Ok(valid)
    }

    /// Returns the checked issue projection of one block.
    ///
    /// The first query pays the projection cost and stores the completed
    /// ranges or the completed failure; later queries pay a bounded lookup.
    fn checked_issue_ranges(
        &mut self,
        side_index: usize,
        block_index: usize,
        projection_cost: usize,
        remaining: &mut usize,
    ) -> Check<&CachedIssueRanges> {
        let side = self.sides[side_index];
        let block = &side.blocks[block_index];
        let cache = self.block_cache(side_index, block_index, remaining)?;
        if cache.issues.is_none() {
            if !charge(remaining, projection_cost) {
                return Err(Stop::Exhausted);
            }
            cache.issues = Some(match block.checked_normalization_issue_ranges() {
                Ok(ranges) => CachedIssueRanges::Ranges(ranges),
                Err(error @ Error::LimitExceeded { .. }) => return Err(Stop::Error(error)),
                Err(_) => CachedIssueRanges::Invalid,
            });
        }
        let cache = self.block_cache(side_index, block_index, remaining)?;
        Ok(cache
            .issues
            .as_ref()
            .expect("the issue projection was just stored"))
    }

    /// Requires exactly one raw and one canonical occurrence of every glyph.
    ///
    /// The document-wide index is built once per side, charged before it runs
    /// and stored only when complete; each query pays a bounded lookup.
    fn check_side_sharing(
        &mut self,
        side_index: usize,
        glyphs: &[GlyphId],
        remaining: &mut usize,
    ) -> Check<()> {
        if self.sharing[side_index].is_none() {
            let index = build_sharing_index(self.sides[side_index], remaining)?;
            self.sharing[side_index] = Some(index);
        }
        if !charge(remaining, glyphs.len()) {
            return Err(Stop::Exhausted);
        }
        let index = self.sharing[side_index]
            .as_ref()
            .expect("the sharing index was just stored");
        for glyph in glyphs {
            if index.is_shared(*glyph) {
                return Err(Stop::Held(FragmentHold::SharedGlyph));
            }
        }
        Ok(())
    }

    /// Validates one side's span and collects its selected fragment evidence.
    fn analyze_side(
        &mut self,
        side_index: usize,
        span: &TextSpan,
        allow_internal_deleted_break: bool,
        remaining: &mut usize,
    ) -> Check<SideFragment> {
        let side = self.sides[side_index];
        // The projection walks every token of every referenced block, so the
        // conservative container scan is charged before it runs: one unit per
        // block lookup and one unit per canonical token of that block.
        for block in &span.blocks {
            if !charge(remaining, 1) {
                return Err(Stop::Exhausted);
            }
            let Some(&block_index) = side.index.get(block) else {
                return Err(Stop::Error(invalid(
                    "equal fragment span refers to an unknown block",
                )));
            };
            if !charge(remaining, side.canonical[block_index].len()) {
                return Err(Stop::Exhausted);
            }
        }
        let intervals = project(side, span)?;
        let [interval] = intervals.as_slice() else {
            return Err(Stop::Held(FragmentHold::Projection));
        };
        let interval = *interval;
        let span_tokens = span
            .comparable_range
            .end
            .saturating_sub(span.comparable_range.start);
        if interval.end <= interval.start || interval.end - interval.start != span_tokens {
            return Err(Stop::Held(FragmentHold::Projection));
        }
        let block = &side.blocks[interval.block_index];
        if !block.raw.unmapped.is_empty() || !block.canonical.unmapped.is_empty() {
            return Err(Stop::Held(FragmentHold::UnmappedBlock));
        }
        let block_tokens = &side.canonical[interval.block_index];
        let tokens = &block_tokens[interval.start..interval.end];
        if !charge(remaining, tokens.len().saturating_mul(2)) {
            return Err(Stop::Exhausted);
        }
        let mut chars = Vec::new();
        chars
            .try_reserve(tokens.len())
            .map_err(|_| allocation_error("equal fragment literals"))?;
        for token in tokens {
            let Some(scalar) = token.as_scalar() else {
                return Err(Stop::Held(FragmentHold::NonScalarToken));
            };
            chars.push(scalar);
        }
        let Some(signatures) = block.position_signatures.as_deref() else {
            return Err(Stop::Held(FragmentHold::MissingPositions));
        };
        if signatures.len() != block_tokens.len() {
            return Err(Stop::Held(FragmentHold::MissingPositions));
        }
        let selected_positions = &signatures[interval.start..interval.end];
        if !charge(remaining, selected_positions.len().saturating_mul(2)) {
            return Err(Stop::Exhausted);
        }
        for position in selected_positions {
            if !finite_signature(position) || !horizontal_direction(position) {
                return Err(Stop::Held(FragmentHold::PositionMismatch));
            }
        }
        let mut positions = Vec::new();
        positions
            .try_reserve(selected_positions.len())
            .map_err(|_| allocation_error("equal fragment positions"))?;
        positions.extend_from_slice(selected_positions);
        let [page] = block.pages.as_slice() else {
            return Err(Stop::Held(FragmentHold::MultiplePages));
        };
        let glyphs = canonical_glyphs(block, &interval, remaining)?;
        let raw_scalars = raw_counterparts(block, &glyphs, remaining)?;
        let run = RawRun {
            interval: &interval,
            glyphs: &glyphs,
            chars: &chars,
            raw_scalars: &raw_scalars,
        };
        let (breaks, admitted_events) = self.check_raw_run(
            side_index,
            interval.block_index,
            &run,
            allow_internal_deleted_break,
            remaining,
        )?;
        let Some(&raw_first) = raw_scalars.first() else {
            return Err(Stop::Held(FragmentHold::RawCut));
        };
        let Some(&raw_last) = raw_scalars.last() else {
            return Err(Stop::Held(FragmentHold::RawCut));
        };
        let selection = BoundarySelection {
            glyphs: &glyphs,
            bounds: FragmentBounds {
                raw_first,
                raw_last,
                scalar_first: interval.start,
                scalar_last: interval.end - 1,
            },
            admitted_events: &admitted_events,
        };
        self.check_normalization_boundary(side_index, interval.block_index, &selection, remaining)?;
        Ok(SideFragment {
            glyphs,
            chars,
            positions,
            page: *page,
            breaks,
        })
    }

    /// Holds when normalization evidence touches the selected fragment.
    ///
    /// The selection-dependent checks run first, so a touching event holds
    /// before any structural validation is paid. The selection-independent
    /// event structure and the checked issue projection are cached per block;
    /// their first query pays the full validation and later queries pay a
    /// bounded lookup. The issue ranges are still tested against the selected
    /// raw and canonical runs on every call.
    fn check_normalization_boundary(
        &mut self,
        side_index: usize,
        block_index: usize,
        selection: &BoundarySelection<'_>,
        remaining: &mut usize,
    ) -> Check<()> {
        let side = self.sides[side_index];
        let block = &side.blocks[block_index];
        // The text scan and the estimation scan are charged before they run.
        if !charge(
            remaining,
            block
                .raw
                .text
                .len()
                .saturating_add(block.canonical.text.len())
                .saturating_add(1),
        ) {
            return Err(Stop::Exhausted);
        }
        let raw_count = block.raw.text.chars().count();
        let scalar_count = block.canonical.text.chars().count();
        let mut raw_entries = 0usize;
        let mut raw_atoms = 0usize;
        let mut canonical_entries = 0usize;
        let mut canonical_atoms = 0usize;
        for entry in &block.raw.source_map {
            if !charge(remaining, 1 + entry.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            raw_entries = raw_entries.saturating_add(1);
            raw_atoms = raw_atoms.saturating_add(entry.source.atoms.len());
        }
        for token in &block.raw.unmapped {
            if !charge(remaining, 1 + token.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            raw_entries = raw_entries.saturating_add(1);
            raw_atoms = raw_atoms.saturating_add(token.source.atoms.len());
        }
        for entry in &block.canonical.source_map {
            if !charge(remaining, 1 + entry.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            canonical_entries = canonical_entries.saturating_add(1);
            canonical_atoms = canonical_atoms.saturating_add(entry.source.atoms.len());
        }
        for token in &block.canonical.unmapped {
            if !charge(remaining, 1 + token.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            canonical_entries = canonical_entries.saturating_add(1);
            canonical_atoms = canonical_atoms.saturating_add(token.source.atoms.len());
        }
        let mut issue_atoms = 0usize;
        for issue in &block.issues {
            if !charge(remaining, 1 + issue.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            issue_atoms = issue_atoms.saturating_add(issue.source.atoms.len());
        }
        let mut event_atoms = 0usize;
        for event in &block.normalization_events {
            if !charge(remaining, 1 + event.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            event_atoms = event_atoms.saturating_add(event.source.atoms.len());
        }
        // `checked_normalization_issue_ranges` validates both texts and maps
        // once, then for every issue walks the raw map
        // (`issue_raw_source_is_exact`), the canonical map, the canonical
        // unmapped tokens and the event list. Every visited pair tests its
        // source atoms, every intersecting entry appends its matching atoms
        // with a duplicate scan over the already matched set, and the final
        // check tests every issue atom against that set. The matched set only
        // ever holds atoms of the issue itself, so the append scans and the
        // final check are bounded by the issue's own atom count; the repeated
        // text and map validation is charged here as well. The expression
        // stays a structured product of the block's counts and never charges
        // a cubic map-atom term.
        let issue_atom_scan = issue_atoms.saturating_mul(issue_atoms);
        let validation_cost = block
            .raw
            .text
            .len()
            .saturating_add(block.canonical.text.len())
            .saturating_add(block.raw.unmapped.len())
            .saturating_add(block.canonical.unmapped.len())
            .saturating_add(raw_entries)
            .saturating_add(canonical_entries)
            .saturating_add(1);
        let per_issue = raw_entries
            .saturating_add(raw_atoms)
            .saturating_add(canonical_entries)
            .saturating_add(canonical_atoms)
            .saturating_add(block.normalization_events.len())
            .saturating_add(event_atoms)
            .saturating_add(1)
            .saturating_mul(issue_atoms.saturating_add(1))
            .saturating_add(
                canonical_atoms
                    .saturating_add(event_atoms)
                    .saturating_add(1)
                    .saturating_mul(issue_atom_scan),
            );
        let projection_cost = validation_cost
            .saturating_add(per_issue.saturating_mul(block.issues.len().saturating_add(1)));
        let mut selected = HashSet::new();
        if !charge(remaining, selection.glyphs.len()) {
            return Err(Stop::Exhausted);
        }
        selected
            .try_reserve(selection.glyphs.len())
            .map_err(|_| allocation_error("equal fragment event selection"))?;
        for glyph in selection.glyphs {
            selected.insert(*glyph);
        }
        let bounds = selection.bounds;
        for (event_index, event) in block.normalization_events.iter().enumerate() {
            if !charge(remaining, 1 + event.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            if !selection.admitted_events.is_empty() {
                if !charge(remaining, selection.admitted_events.len()) {
                    return Err(Stop::Exhausted);
                }
                if selection.admitted_events.contains(&event_index) {
                    continue;
                }
            }
            if event_touches_selection(event, &selected, bounds) {
                return Err(Stop::Held(FragmentHold::NormalizationBoundary));
            }
        }
        for event_index in 0..block.normalization_events.len() {
            if !self.event_structure(
                side_index,
                block_index,
                event_index,
                raw_count,
                scalar_count,
                remaining,
            )? {
                return Err(Stop::Held(FragmentHold::NormalizationBoundary));
            }
        }
        if !charge(remaining, 1 + block.issues.len()) {
            return Err(Stop::Exhausted);
        }
        match self.checked_issue_ranges(side_index, block_index, projection_cost, remaining)? {
            CachedIssueRanges::Invalid => {
                return Err(Stop::Held(FragmentHold::NormalizationBoundary));
            }
            CachedIssueRanges::Ranges(ranges) => {
                for (issue, range) in block.issues.iter().zip(ranges) {
                    if !charge(remaining, 1 + issue.source.atoms.len()) {
                        return Err(Stop::Exhausted);
                    }
                    if range_overlaps(issue.raw_range, bounds.raw_first, bounds.raw_last)
                        || range_overlaps(*range, bounds.scalar_first, bounds.scalar_last)
                    {
                        return Err(Stop::Held(FragmentHold::NormalizationBoundary));
                    }
                }
            }
        }
        Ok(())
    }

    /// Checks the selected raw run: contiguous and literal-equal.
    ///
    /// Without `allow_internal_deleted_break` the behavior, charge order and
    /// result are exactly the contiguous-run contract. With the flag, a gap of
    /// exactly one raw scalar between two consecutive selected glyphs may be
    /// certified as an internal deleted soft line break; the certified break
    /// pair indices and the event indices admitted by the boundary veto are
    /// returned.
    fn check_raw_run(
        &mut self,
        side_index: usize,
        block_index: usize,
        run: &RawRun<'_>,
        allow_internal_deleted_break: bool,
        remaining: &mut usize,
    ) -> Check<(Vec<usize>, Vec<usize>)> {
        let side = self.sides[side_index];
        let block = &side.blocks[block_index];
        let raw_scalars = run.raw_scalars;
        let chars = run.chars;
        if !charge(remaining, raw_scalars.len()) {
            return Err(Stop::Exhausted);
        }
        let mut breaks = Vec::new();
        let mut admitted_events = Vec::new();
        for (pair_index, pair) in raw_scalars.windows(2).enumerate() {
            if pair[1] == pair[0] + 1 {
                continue;
            }
            if !allow_internal_deleted_break || pair[1] != pair[0] + 2 {
                return Err(Stop::Held(FragmentHold::RawCut));
            }
            let event_index = self.admit_internal_deleted_break(
                side_index,
                block_index,
                run,
                pair_index,
                pair[0] + 1,
                remaining,
            )?;
            if !charge(remaining, 1) {
                return Err(Stop::Exhausted);
            }
            breaks
                .try_reserve(1)
                .map_err(|_| allocation_error("equal fragment internal breaks"))?;
            breaks.push(pair_index);
            admitted_events
                .try_reserve(1)
                .map_err(|_| allocation_error("equal fragment admitted events"))?;
            admitted_events.push(event_index);
        }
        let mut next = 0usize;
        for (index, scalar) in block.raw.text.chars().enumerate() {
            if !charge(remaining, 1) {
                return Err(Stop::Exhausted);
            }
            if next >= raw_scalars.len() {
                break;
            }
            if raw_scalars[next] == index {
                if chars[next] != scalar {
                    return Err(Stop::Held(FragmentHold::RawSource));
                }
                next += 1;
            }
        }
        if next != raw_scalars.len() {
            return Err(Stop::Held(FragmentHold::RawSource));
        }
        Ok((breaks, admitted_events))
    }

    /// Certifies one internal deleted soft line break gap.
    ///
    /// The gap must be exactly one raw newline scalar whose single raw source
    /// atom is a `LineBreak` naming the two adjacent selected glyphs, and
    /// exactly one `SoftLineBreak` event must delete that raw range to the
    /// zero-length canonical range at the following selected scalar. The event
    /// must pass the full structural validation. Returns the event index that
    /// the boundary veto admits.
    fn admit_internal_deleted_break(
        &mut self,
        side_index: usize,
        block_index: usize,
        run: &RawRun<'_>,
        pair_index: usize,
        gap: usize,
        remaining: &mut usize,
    ) -> Check<usize> {
        let side = self.sides[side_index];
        let block = &side.blocks[block_index];
        if pair_index + 1 >= run.glyphs.len() {
            return Err(Stop::Held(FragmentHold::RawCut));
        }
        if !charge(remaining, gap.saturating_add(1)) {
            return Err(Stop::Exhausted);
        }
        if block.raw.text.chars().nth(gap) != Some('\n') {
            return Err(Stop::Held(FragmentHold::RawCut));
        }
        let expected = TextSourceAtom::LineBreak {
            preceding: run.glyphs[pair_index],
            following: run.glyphs[pair_index + 1],
        };
        let mut raw_atom_found = false;
        for entry in &block.raw.source_map {
            if !charge(remaining, 1 + entry.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            if entry.output_range.start != gap {
                continue;
            }
            if raw_atom_found
                || entry.output_range.end != gap + 1
                || entry.source.atoms.len() != 1
                || entry.source.atoms[0] != expected
            {
                return Err(Stop::Held(FragmentHold::RawCut));
            }
            raw_atom_found = true;
        }
        if !raw_atom_found {
            return Err(Stop::Held(FragmentHold::RawCut));
        }
        let mut candidates = 0usize;
        let mut event_index = None;
        for (index, event) in block.normalization_events.iter().enumerate() {
            if !charge(remaining, 1 + event.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            if event.raw_range
                == (ScalarRange {
                    start: gap,
                    end: gap + 1,
                })
            {
                candidates += 1;
                if candidates > 1 {
                    return Err(Stop::Held(FragmentHold::NormalizationBoundary));
                }
                event_index = Some(index);
            }
        }
        let Some(event_index) = event_index else {
            return Err(Stop::Held(FragmentHold::NormalizationBoundary));
        };
        let boundary = run.interval.start + pair_index + 1;
        let event = &block.normalization_events[event_index];
        if event.kind != NormalizationKind::SoftLineBreak
            || event.canonical_range
                != (ScalarRange {
                    start: boundary,
                    end: boundary,
                })
            || event.source.atoms.len() != 1
            || event.source.atoms[0] != expected
        {
            return Err(Stop::Held(FragmentHold::NormalizationBoundary));
        }
        if !charge(
            remaining,
            block
                .raw
                .text
                .len()
                .saturating_add(block.canonical.text.len())
                .saturating_add(1),
        ) {
            return Err(Stop::Exhausted);
        }
        let raw_count = block.raw.text.chars().count();
        let scalar_count = block.canonical.text.chars().count();
        if !self.event_structure(
            side_index,
            block_index,
            event_index,
            raw_count,
            scalar_count,
            remaining,
        )? {
            return Err(Stop::Held(FragmentHold::NormalizationBoundary));
        }
        Ok(event_index)
    }
}

/// One side's validated selected fragment.
struct SideFragment {
    /// Selected canonical glyphs, in comparable order.
    glyphs: Vec<GlyphId>,
    /// Selected literal scalars, parallel to `glyphs`.
    chars: Vec<char>,
    /// Selected position signatures, parallel to `glyphs`.
    positions: Vec<PositionSignature>,
    /// Single page of the selected block.
    page: u32,
    /// Selected-index positions of certified internal deleted soft line breaks,
    /// in ascending order. Always empty without the fallback allowance.
    breaks: Vec<usize>,
}

/// Selection-dependent raw evidence of one side's fragment.
struct RawRun<'a> {
    /// The projected selected interval of the side's block.
    interval: &'a SourceInterval,
    /// Selected canonical glyphs, in comparable order.
    glyphs: &'a [GlyphId],
    /// Selected literal scalars, parallel to `glyphs`.
    chars: &'a [char],
    /// Raw scalar positions of the selected glyphs, parallel to `glyphs`.
    raw_scalars: &'a [usize],
}

/// Selection-dependent evidence passed to the normalization boundary veto.
struct BoundarySelection<'a> {
    /// Selected canonical glyphs, in comparable order.
    glyphs: &'a [GlyphId],
    /// Inclusive raw and scalar bounds of the selection.
    bounds: FragmentBounds,
    /// Event indices admitted by the internal deleted soft line break proof.
    admitted_events: &'a [usize],
}

/// Whether a signature carries finite baseline and direction geometry.
///
/// `PositionSignature::new` rejects non-finite geometry, but the evidence
/// checks do not rely on that constructor invariant: a non-finite signature
/// that happens to match bit for bit must still hold.
fn finite_signature(signature: &PositionSignature) -> bool {
    let baseline = signature.baseline();
    let direction = signature.direction();
    baseline.x.is_finite()
        && baseline.y.is_finite()
        && direction.x.is_finite()
        && direction.y.is_finite()
}

/// Extracts one single-glyph canonical source per selected scalar.
///
/// The caller has already held blocks with unmapped tokens, so a block-local
/// comparable index is also its canonical scalar index. The whole canonical
/// source map is validated while the covering entries are collected; a
/// malformed range, a gap, a multi-scalar entry or a non-glyph atom holds the
/// fragment.
fn canonical_glyphs(
    block: &BlockText,
    interval: &SourceInterval,
    remaining: &mut usize,
) -> Check<Vec<GlyphId>> {
    if !charge(remaining, block.canonical.text.len().saturating_add(1)) {
        return Err(Stop::Exhausted);
    }
    let scalar_count = block.canonical.text.chars().count();
    let mut glyphs = Vec::new();
    let selected = interval.end - interval.start;
    if !charge(remaining, selected) {
        return Err(Stop::Exhausted);
    }
    glyphs
        .try_reserve(selected)
        .map_err(|_| allocation_error("equal fragment canonical glyphs"))?;
    let mut previous_end = 0usize;
    let mut next = interval.start;
    for entry in &block.canonical.source_map {
        if !charge(remaining, 1 + entry.source.atoms.len()) {
            return Err(Stop::Exhausted);
        }
        if entry.output_range.start > entry.output_range.end
            || entry.output_range.end > scalar_count
            || entry.output_range.start < previous_end
        {
            return Err(Stop::Held(FragmentHold::CanonicalSource));
        }
        previous_end = entry.output_range.end;
        if next >= interval.end {
            continue;
        }
        if next < entry.output_range.start {
            return Err(Stop::Held(FragmentHold::CanonicalSource));
        }
        if next >= entry.output_range.end {
            continue;
        }
        if entry.output_range.start != next || entry.output_range.end != next + 1 {
            return Err(Stop::Held(FragmentHold::CanonicalSource));
        }
        let [atom] = entry.source.atoms.as_slice() else {
            return Err(Stop::Held(FragmentHold::CanonicalSource));
        };
        let TextSourceAtom::Glyph(glyph) = atom else {
            return Err(Stop::Held(FragmentHold::CanonicalSource));
        };
        glyphs.push(*glyph);
        next += 1;
    }
    if next != interval.end {
        return Err(Stop::Held(FragmentHold::CanonicalSource));
    }
    Ok(glyphs)
}

/// Resolves the unique single-scalar raw counterpart of every selected glyph.
///
/// The whole raw source map is validated; a malformed range, a multi-scalar
/// entry, a multi-atom entry or a repeated selected glyph holds the fragment.
fn raw_counterparts(
    block: &BlockText,
    glyphs: &[GlyphId],
    remaining: &mut usize,
) -> Check<Vec<usize>> {
    if !charge(remaining, block.raw.text.len().saturating_add(1)) {
        return Err(Stop::Exhausted);
    }
    let raw_scalar_count = block.raw.text.chars().count();
    let mut selected = HashSet::new();
    if !charge(remaining, glyphs.len()) {
        return Err(Stop::Exhausted);
    }
    selected
        .try_reserve(glyphs.len())
        .map_err(|_| allocation_error("equal fragment raw selection"))?;
    for glyph in glyphs {
        selected.insert(*glyph);
    }
    let mut positions = std::collections::HashMap::new();
    if !charge(remaining, glyphs.len()) {
        return Err(Stop::Exhausted);
    }
    positions
        .try_reserve(glyphs.len())
        .map_err(|_| allocation_error("equal fragment raw positions"))?;
    let mut previous_end = 0usize;
    for entry in &block.raw.source_map {
        if !charge(remaining, 1 + entry.source.atoms.len()) {
            return Err(Stop::Exhausted);
        }
        if entry.output_range.start > entry.output_range.end
            || entry.output_range.end > raw_scalar_count
            || entry.output_range.start < previous_end
        {
            return Err(Stop::Held(FragmentHold::RawSource));
        }
        previous_end = entry.output_range.end;
        for atom in &entry.source.atoms {
            let TextSourceAtom::Glyph(glyph) = atom else {
                continue;
            };
            if !selected.contains(glyph) {
                continue;
            }
            if entry.output_range.end != entry.output_range.start + 1
                || entry.source.atoms.len() != 1
            {
                return Err(Stop::Held(FragmentHold::RawSource));
            }
            if positions.insert(*glyph, entry.output_range.start).is_some() {
                return Err(Stop::Held(FragmentHold::RawSource));
            }
        }
    }
    let mut raw_scalars = Vec::new();
    if !charge(remaining, glyphs.len()) {
        return Err(Stop::Exhausted);
    }
    raw_scalars
        .try_reserve(glyphs.len())
        .map_err(|_| allocation_error("equal fragment raw counterparts"))?;
    for glyph in glyphs {
        if !charge(remaining, 1) {
            return Err(Stop::Exhausted);
        }
        let Some(&position) = positions.get(glyph) else {
            return Err(Stop::Held(FragmentHold::RawSource));
        };
        raw_scalars.push(position);
    }
    Ok(raw_scalars)
}

/// Whether a scalar range touches the inclusive selected run.
///
/// A zero-length range is widened by one position, so an insertion or deletion
/// exactly at a fragment edge or cut still counts as touching and an empty
/// range is never silently ignored. This matches the existing source issue
/// veto.
fn range_overlaps(range: ScalarRange, first: usize, last: usize) -> bool {
    let (start, end) = if range.start == range.end {
        (range.start.saturating_sub(1), range.end.saturating_add(1))
    } else {
        (range.start, range.end)
    };
    start <= last && end > first
}

/// Selected fragment bounds and block scalar counts for one event check.
#[derive(Clone, Copy)]
struct FragmentBounds {
    raw_first: usize,
    raw_last: usize,
    scalar_first: usize,
    scalar_last: usize,
}

/// Whether one event touches the selected fragment or its cut edges.
///
/// A range overlap or a source atom that names a selected glyph holds; a
/// line-break or synthetic-space endpoint that is a selected glyph holds as
/// well. This check only reads the selected run, so it stays per call.
fn event_touches_selection(
    event: &NormalizationEvent,
    selected: &HashSet<GlyphId>,
    bounds: FragmentBounds,
) -> bool {
    if range_overlaps(event.raw_range, bounds.raw_first, bounds.raw_last)
        || range_overlaps(
            event.canonical_range,
            bounds.scalar_first,
            bounds.scalar_last,
        )
    {
        return true;
    }
    event.source.atoms.iter().any(|atom| match atom {
        TextSourceAtom::Glyph(glyph) => selected.contains(glyph),
        TextSourceAtom::SyntheticSpace {
            preceding,
            following,
        }
        | TextSourceAtom::LineBreak {
            preceding,
            following,
        } => selected.contains(preceding) || selected.contains(following),
    })
}

/// Whether one event's structure and side evidence are valid.
///
/// The event must have ordered in-bounds ranges and a nonempty source without
/// duplicate atoms, its raw source must be exactly the raw evidence at its raw
/// range in both directions, and its canonical range must either match the
/// canonical evidence at that range or, for a zero-length deletion output,
/// prove that the source has disappeared from the canonical map. The result
/// depends only on the block, so the cache stores it.
fn event_structure_holds(
    block: &BlockText,
    event: &NormalizationEvent,
    raw_count: usize,
    scalar_count: usize,
    remaining: &mut usize,
) -> Check<bool> {
    let atoms = event.source.atoms.len();
    if !charge(remaining, 1 + atoms + atoms.saturating_mul(atoms)) {
        return Err(Stop::Exhausted);
    }
    if event.raw_range.start > event.raw_range.end
        || event.raw_range.end > raw_count
        || event.canonical_range.start > event.canonical_range.end
        || event.canonical_range.end > scalar_count
        || event.source.atoms.is_empty()
        || has_duplicate_source_atoms(&event.source)
    {
        return Ok(false);
    }
    if !side_event_evidence(&block.raw, event, event.raw_range, remaining)? {
        return Ok(false);
    }
    if event.canonical_range.start < event.canonical_range.end {
        if !side_event_evidence(&block.canonical, event, event.canonical_range, remaining)? {
            return Ok(false);
        }
    } else if !canonical_source_absent(&block.canonical, event, remaining)? {
        return Ok(false);
    }
    Ok(true)
}

/// Whether one side's source map is exactly the event source at `range`.
///
/// Every entry or unmapped token whose range is contained in or touches
/// `range` must carry a nonempty source inside the event source, every event
/// source atom must appear in one of them, and a nonempty range must be
/// covered by the entries without a gap. Each scan is charged before it runs.
fn side_event_evidence(
    mapped: &MappedText,
    event: &NormalizationEvent,
    range: ScalarRange,
    remaining: &mut usize,
) -> Check<bool> {
    let event_atoms = event.source.atoms.len();
    let mut mapped_atoms = 0usize;
    for entry in &mapped.source_map {
        if !charge(remaining, 1 + entry.source.atoms.len()) {
            return Err(Stop::Exhausted);
        }
        mapped_atoms = mapped_atoms.saturating_add(entry.source.atoms.len());
    }
    for token in &mapped.unmapped {
        if !charge(remaining, 1 + token.source.atoms.len()) {
            return Err(Stop::Exhausted);
        }
        mapped_atoms = mapped_atoms.saturating_add(token.source.atoms.len());
    }
    let mut next = range.start;
    for entry in &mapped.source_map {
        if !charge(
            remaining,
            (1 + entry.source.atoms.len()).saturating_mul(event_atoms.saturating_add(1)),
        ) {
            return Err(Stop::Exhausted);
        }
        if !scalar_range_contains_or_touches(range, entry.output_range) {
            continue;
        }
        if entry.source.atoms.is_empty()
            || entry
                .source
                .atoms
                .iter()
                .any(|atom| !event.source.atoms.contains(atom))
        {
            return Ok(false);
        }
        if range.start < range.end && entry.output_range.end > next {
            if entry.output_range.start > next {
                return Ok(false);
            }
            next = entry.output_range.end;
        }
    }
    if range.start < range.end && next < range.end {
        return Ok(false);
    }
    for token in &mapped.unmapped {
        if !charge(
            remaining,
            (1 + token.source.atoms.len()).saturating_mul(event_atoms.saturating_add(1)),
        ) {
            return Err(Stop::Exhausted);
        }
        let point = ScalarRange {
            start: token.scalar_index,
            end: token.scalar_index,
        };
        if !scalar_range_contains_or_touches(range, point) {
            continue;
        }
        if token.source.atoms.is_empty()
            || token
                .source
                .atoms
                .iter()
                .any(|atom| !event.source.atoms.contains(atom))
        {
            return Ok(false);
        }
    }
    let entries = mapped
        .source_map
        .len()
        .saturating_add(mapped.unmapped.len());
    if !charge(
        remaining,
        entries
            .saturating_add(mapped_atoms)
            .saturating_add(1)
            .saturating_mul(event_atoms.max(1)),
    ) {
        return Err(Stop::Exhausted);
    }
    for atom in &event.source.atoms {
        let covered = mapped.source_map.iter().any(|entry| {
            scalar_range_contains_or_touches(range, entry.output_range)
                && entry.source.atoms.contains(atom)
        }) || mapped.unmapped.iter().any(|token| {
            scalar_range_contains_or_touches(
                range,
                ScalarRange {
                    start: token.scalar_index,
                    end: token.scalar_index,
                },
            ) && token.source.atoms.contains(atom)
        });
        if !covered {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether a deleted event source has really disappeared from one side.
///
/// A zero-length output range is a deletion, so no entry or unmapped token of
/// that side may still carry an event source atom: the deletion must be backed
/// by the side's own evidence instead of an arbitrary empty range.
fn canonical_source_absent(
    mapped: &MappedText,
    event: &NormalizationEvent,
    remaining: &mut usize,
) -> Check<bool> {
    for entry in &mapped.source_map {
        if !charge(
            remaining,
            (1 + entry.source.atoms.len())
                .saturating_mul(event.source.atoms.len().saturating_add(1)),
        ) {
            return Err(Stop::Exhausted);
        }
        if entry
            .source
            .atoms
            .iter()
            .any(|atom| event.source.atoms.contains(atom))
        {
            return Ok(false);
        }
    }
    for token in &mapped.unmapped {
        if !charge(
            remaining,
            (1 + token.source.atoms.len())
                .saturating_mul(event.source.atoms.len().saturating_add(1)),
        ) {
            return Err(Stop::Exhausted);
        }
        if token
            .source
            .atoms
            .iter()
            .any(|atom| event.source.atoms.contains(atom))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Builds the document-wide glyph sharing index of one side.
///
/// Every block, entry and atom is charged before it is visited, every table
/// insert is fallible and the index is returned only when complete, so a
/// partial build is never stored. A glyph that occurs more than once in one
/// map kind is recorded as shared regardless of the block it appears in.
fn build_sharing_index(side: &Side<'_>, remaining: &mut usize) -> Check<SharingIndex> {
    let mut seen_raw = HashSet::new();
    let mut seen_canonical = HashSet::new();
    let mut shared_raw = HashSet::new();
    let mut shared_canonical = HashSet::new();
    for block in side.blocks {
        if !charge(remaining, 1) {
            return Err(Stop::Exhausted);
        }
        for entry in &block.raw.source_map {
            if !charge(remaining, 1 + entry.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            note_shared(
                &entry.source.atoms,
                &mut seen_raw,
                &mut shared_raw,
                remaining,
            )?;
        }
        for token in &block.raw.unmapped {
            if !charge(remaining, 1 + token.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            note_shared(
                &token.source.atoms,
                &mut seen_raw,
                &mut shared_raw,
                remaining,
            )?;
        }
        for entry in &block.canonical.source_map {
            if !charge(remaining, 1 + entry.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            note_shared(
                &entry.source.atoms,
                &mut seen_canonical,
                &mut shared_canonical,
                remaining,
            )?;
        }
        for token in &block.canonical.unmapped {
            if !charge(remaining, 1 + token.source.atoms.len()) {
                return Err(Stop::Exhausted);
            }
            note_shared(
                &token.source.atoms,
                &mut seen_canonical,
                &mut shared_canonical,
                remaining,
            )?;
        }
    }
    Ok(SharingIndex {
        shared_raw,
        shared_canonical,
    })
}

/// Records the glyphs of one source atom list, keeping the repeats.
fn note_shared(
    atoms: &[TextSourceAtom],
    seen: &mut HashSet<GlyphId>,
    shared: &mut HashSet<GlyphId>,
    remaining: &mut usize,
) -> Check<()> {
    for atom in atoms {
        let TextSourceAtom::Glyph(glyph) = atom else {
            continue;
        };
        if !charge(remaining, 1) {
            return Err(Stop::Exhausted);
        }
        if seen.try_reserve(1).is_err() {
            return Err(Stop::Exhausted);
        }
        if !seen.insert(*glyph) {
            if !charge(remaining, 1) {
                return Err(Stop::Exhausted);
            }
            if shared.try_reserve(1).is_err() {
                return Err(Stop::Exhausted);
            }
            shared.insert(*glyph);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        alignment::BlockSeparator,
        diff::TokenRange,
        layout::{BlockId, BlockRole},
        model::{FontProgramHash, Vec2},
        normalize::{
            FontSizeSignature, MappedText, NormalizationEvent, NormalizationIssue,
            NormalizationIssueKind, NormalizationKind, SourceMapEntry, TextSource, UnmappedToken,
        },
    };

    fn glyph(block: u64, index: usize) -> GlyphId {
        GlyphId(block * 1000 + index as u64 + 1)
    }

    fn glyph_entry(start: usize, glyph: GlyphId) -> SourceMapEntry {
        SourceMapEntry {
            output_range: ScalarRange {
                start,
                end: start + 1,
            },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(glyph)].into(),
            },
        }
    }

    fn span_entry(start: usize, end: usize, glyph: GlyphId) -> SourceMapEntry {
        SourceMapEntry {
            output_range: ScalarRange { start, end },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(glyph)].into(),
            },
        }
    }

    fn atoms_entry(start: usize, atoms: Vec<TextSourceAtom>) -> SourceMapEntry {
        SourceMapEntry {
            output_range: ScalarRange {
                start,
                end: start + 1,
            },
            source: TextSource {
                atoms: atoms.into(),
            },
        }
    }

    fn mapped(block: u64, text: &str) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: text
                .chars()
                .enumerate()
                .map(|(index, _)| glyph_entry(index, glyph(block, index)))
                .collect(),
            unmapped: Vec::new(),
        }
    }

    fn positioned_block(block: u64, text: &str, x: f64, y: f64, page: u32) -> BlockText {
        let raw = mapped(block, text);
        let canonical = mapped(block, text);
        let tokens = canonical.comparable_tokens().expect("fixture tokens");
        let font_size = FontSizeSignature::new(&[10.0]).expect("valid font size");
        let positions = (0..tokens.len())
            .map(|index| {
                PositionSignature::new(
                    Vec2 {
                        x: x + index as f64 * 10.0,
                        y,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("valid fixture position")
            })
            .collect::<Vec<_>>();
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw,
            canonical,
            matching: text.to_owned(),
            matching_tokens: tokens.clone(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![page],
            font_size_signatures: Some(vec![font_size; tokens.len()]),
            position_signatures: Some(positions),
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    /// Raw `AB  CD` collapsed to canonical `AB CD`, with the event and the
    /// multi-atom canonical space source exactly as the normalization emits.
    fn collapsed_block(block: u64, x: f64, y: f64) -> BlockText {
        let mut fixture = positioned_block(block, "AB CD", x, y, 0);
        fixture.raw = MappedText {
            text: "AB  CD".to_owned(),
            source_map: (0..6)
                .map(|index| glyph_entry(index, glyph(block, index)))
                .collect(),
            unmapped: Vec::new(),
        };
        fixture.canonical.source_map = vec![
            glyph_entry(0, glyph(block, 0)),
            glyph_entry(1, glyph(block, 1)),
            atoms_entry(
                2,
                vec![
                    TextSourceAtom::Glyph(glyph(block, 2)),
                    TextSourceAtom::Glyph(glyph(block, 3)),
                ],
            ),
            glyph_entry(3, glyph(block, 4)),
            glyph_entry(4, glyph(block, 5)),
        ];
        fixture.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::WhitespaceCollapse,
            raw_range: ScalarRange { start: 2, end: 4 },
            canonical_range: ScalarRange { start: 2, end: 3 },
            source: TextSource {
                atoms: vec![
                    TextSourceAtom::Glyph(glyph(block, 2)),
                    TextSourceAtom::Glyph(glyph(block, 3)),
                ]
                .into(),
            },
        }];
        fixture
    }

    /// Raw `AB\nCD` whose line break is deleted to canonical `ABCD`.
    fn deleted_break_block(block: u64, x: f64, y: f64) -> BlockText {
        let mut fixture = positioned_block(block, "ABCD", x, y, 0);
        fixture.raw = MappedText {
            text: "AB\nCD".to_owned(),
            source_map: vec![
                glyph_entry(0, glyph(block, 0)),
                glyph_entry(1, glyph(block, 1)),
                atoms_entry(
                    2,
                    vec![TextSourceAtom::LineBreak {
                        preceding: glyph(block, 1),
                        following: glyph(block, 2),
                    }],
                ),
                glyph_entry(3, glyph(block, 2)),
                glyph_entry(4, glyph(block, 3)),
            ],
            unmapped: Vec::new(),
        };
        fixture.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange { start: 2, end: 3 },
            canonical_range: ScalarRange { start: 2, end: 2 },
            source: TextSource {
                atoms: vec![TextSourceAtom::LineBreak {
                    preceding: glyph(block, 1),
                    following: glyph(block, 2),
                }]
                .into(),
            },
        }];
        fixture
    }

    /// Raw `A\nB` whose line break is kept as the canonical space.
    fn kept_break_block(block: u64, x: f64, y: f64) -> BlockText {
        let mut fixture = positioned_block(block, "A B", x, y, 0);
        let line_break = TextSourceAtom::LineBreak {
            preceding: glyph(block, 0),
            following: glyph(block, 1),
        };
        fixture.raw = MappedText {
            text: "A\nB".to_owned(),
            source_map: vec![
                glyph_entry(0, glyph(block, 0)),
                atoms_entry(1, vec![line_break.clone()]),
                glyph_entry(2, glyph(block, 1)),
            ],
            unmapped: Vec::new(),
        };
        fixture.canonical.source_map = vec![
            glyph_entry(0, glyph(block, 0)),
            atoms_entry(1, vec![line_break.clone()]),
            glyph_entry(2, glyph(block, 1)),
        ];
        fixture.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange { start: 1, end: 2 },
            canonical_range: ScalarRange { start: 1, end: 2 },
            source: TextSource {
                atoms: vec![line_break].into(),
            },
        }];
        fixture
    }

    /// Raw `A...\n...D` whose line break is deleted to canonical `ABCD`.
    fn deleted_break_at(block: u64, break_index: usize) -> BlockText {
        let mut fixture = positioned_block(block, "ABCD", 300.0, 700.0, 0);
        let mut raw_text = String::from("ABCD");
        raw_text.insert(break_index, '\n');
        let mut source_map = Vec::new();
        for index in 0..4 {
            let raw_index = if index < break_index {
                index
            } else {
                index + 1
            };
            source_map.push(glyph_entry(raw_index, glyph(block, index)));
        }
        let line_break = TextSourceAtom::LineBreak {
            preceding: glyph(block, break_index - 1),
            following: glyph(block, break_index),
        };
        source_map.insert(
            break_index,
            atoms_entry(break_index, vec![line_break.clone()]),
        );
        fixture.raw = MappedText {
            text: raw_text,
            source_map,
            unmapped: Vec::new(),
        };
        fixture.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange {
                start: break_index,
                end: break_index + 1,
            },
            canonical_range: ScalarRange {
                start: break_index,
                end: break_index,
            },
            source: TextSource {
                atoms: vec![line_break].into(),
            },
        }];
        fixture
    }

    /// Raw `A\nB\nCD` with two deleted line breaks to canonical `ABCD`.
    fn double_deleted_break_block(block: u64) -> BlockText {
        let mut fixture = positioned_block(block, "ABCD", 300.0, 700.0, 0);
        let first = TextSourceAtom::LineBreak {
            preceding: glyph(block, 0),
            following: glyph(block, 1),
        };
        let second = TextSourceAtom::LineBreak {
            preceding: glyph(block, 1),
            following: glyph(block, 2),
        };
        fixture.raw = MappedText {
            text: "A\nB\nCD".to_owned(),
            source_map: vec![
                glyph_entry(0, glyph(block, 0)),
                atoms_entry(1, vec![first.clone()]),
                glyph_entry(2, glyph(block, 1)),
                atoms_entry(3, vec![second.clone()]),
                glyph_entry(4, glyph(block, 2)),
                glyph_entry(5, glyph(block, 3)),
            ],
            unmapped: Vec::new(),
        };
        fixture.normalization_events = vec![
            NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: vec![first].into(),
                },
            },
            NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 3, end: 4 },
                canonical_range: ScalarRange { start: 2, end: 2 },
                source: TextSource {
                    atoms: vec![second].into(),
                },
            },
        ];
        fixture
    }

    fn side(blocks: &[BlockText]) -> Side<'_> {
        super::super::super::SidePlan::inspect("equal fragment test", blocks)
            .expect("test blocks are valid")
            .materialize()
            .expect("test blocks materialize")
    }

    fn fragment(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn prove_pair(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        old_span: &TextSpan,
        new_span: &TextSpan,
        budget: usize,
    ) -> (FragmentVerdict, usize) {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let mut cache = EqualFragmentCache::new([&old, &new]);
        let mut remaining = budget;
        let verdict = cache
            .prove([old_span, new_span], &mut remaining)
            .expect("valid fixture evidence");
        (verdict, budget - remaining)
    }

    fn prove_internal_pair(
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        old_span: &TextSpan,
        new_span: &TextSpan,
        budget: usize,
    ) -> (FragmentVerdict, usize) {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let mut cache = EqualFragmentCache::new([&old, &new]);
        let mut remaining = budget;
        let verdict = cache
            .prove_internal_deleted_soft_line_break([old_span, new_span], &mut remaining)
            .expect("valid fixture evidence");
        (verdict, budget - remaining)
    }

    #[test]
    fn mid_block_fragment_proves() {
        let old_blocks = vec![positioned_block(1, "abcdefgh", 300.0, 700.0, 0)];
        let new_blocks = vec![positioned_block(101, "abcdefgh", 300.0, 700.0, 0)];
        let (verdict, used) = prove_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 2, 6),
            &fragment(101, 2, 6),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
        assert!(used > 0);
    }

    #[test]
    fn container_origin_span_uses_the_real_projection() {
        // The span names a preceding block and its separator, but the
        // comparable range starts inside the second block: only that block's
        // middle tokens and their own source and position evidence may count.
        let old_blocks = vec![
            positioned_block(1, "head", 100.0, 700.0, 0),
            positioned_block(2, "abcdefgh", 300.0, 680.0, 0),
        ];
        let new_blocks = vec![
            positioned_block(101, "head", 100.0, 700.0, 0),
            positioned_block(102, "abcdefgh", 300.0, 680.0, 0),
        ];
        let span = |first: u64, second: u64| TextSpan {
            blocks: vec![BlockId(first), BlockId(second)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 7, end: 11 },
            comparable_range: TokenRange { start: 7, end: 11 },
        };
        let (verdict, _) = prove_pair(
            &old_blocks,
            &new_blocks,
            &span(1, 2),
            &span(101, 102),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
    }

    #[test]
    fn raw_contiguity_gap_holds() {
        let mut old = positioned_block(1, "ab", 300.0, 700.0, 0);
        old.raw = MappedText {
            text: "a b".to_owned(),
            source_map: vec![
                glyph_entry(0, glyph(1, 0)),
                glyph_entry(1, GlyphId(1099)),
                glyph_entry(2, glyph(1, 1)),
            ],
            unmapped: Vec::new(),
        };
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let (verdict, _) = prove_pair(
            &[old],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawCut));
    }

    #[test]
    fn raw_literal_mismatch_holds() {
        let mut old = positioned_block(1, "ab", 300.0, 700.0, 0);
        old.raw = MappedText {
            text: "ax".to_owned(),
            source_map: vec![glyph_entry(0, glyph(1, 0)), glyph_entry(1, glyph(1, 1))],
            unmapped: Vec::new(),
        };
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let (verdict, _) = prove_pair(
            &[old],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawSource));
    }

    #[test]
    fn multi_scalar_and_multi_source_entries_hold() {
        let new = positioned_block(101, "abc", 300.0, 700.0, 0);
        let mut canonical_multi = positioned_block(1, "abc", 300.0, 700.0, 0);
        canonical_multi.canonical.source_map =
            vec![glyph_entry(0, glyph(1, 0)), span_entry(1, 3, glyph(1, 1))];
        let (verdict, _) = prove_pair(
            &[canonical_multi],
            &[new],
            &fragment(1, 0, 3),
            &fragment(101, 0, 3),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::CanonicalSource)
        );

        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut raw_multi = positioned_block(1, "ab", 300.0, 700.0, 0);
        raw_multi.raw.source_map = vec![span_entry(0, 2, glyph(1, 0))];
        let (verdict, _) = prove_pair(
            &[raw_multi],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawSource));

        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut multi_source = positioned_block(1, "ab", 300.0, 700.0, 0);
        multi_source.canonical.source_map[0] = atoms_entry(
            0,
            vec![glyph(1, 0), GlyphId(1099)]
                .into_iter()
                .map(TextSourceAtom::Glyph)
                .collect(),
        );
        let (verdict, _) = prove_pair(
            &[multi_source],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::CanonicalSource)
        );
    }

    #[test]
    fn missing_and_malformed_maps_hold() {
        let new = positioned_block(101, "abc", 300.0, 700.0, 0);
        let mut missing = positioned_block(1, "abc", 300.0, 700.0, 0);
        missing.canonical.source_map =
            vec![glyph_entry(0, glyph(1, 0)), glyph_entry(2, glyph(1, 2))];
        let (verdict, _) = prove_pair(
            &[missing],
            &[new],
            &fragment(1, 0, 3),
            &fragment(101, 0, 3),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::CanonicalSource)
        );

        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut overlapping = positioned_block(1, "ab", 300.0, 700.0, 0);
        overlapping.canonical.source_map =
            vec![glyph_entry(0, glyph(1, 0)), glyph_entry(0, glyph(1, 1))];
        let (verdict, _) = prove_pair(
            &[overlapping],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::CanonicalSource)
        );

        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut beyond = positioned_block(1, "ab", 300.0, 700.0, 0);
        beyond.raw.source_map = vec![glyph_entry(0, glyph(1, 0)), span_entry(1, 5, glyph(1, 1))];
        let (verdict, _) = prove_pair(
            &[beyond],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawSource));
    }

    #[test]
    fn glyph_sharing_holds() {
        // A raw glyph shared with another block.
        let mut second = positioned_block(2, "xy", 300.0, 600.0, 0);
        second.raw.source_map[0] = glyph_entry(0, glyph(1, 0));
        let old_blocks = vec![positioned_block(1, "ab", 300.0, 700.0, 0), second];
        let new_blocks = vec![positioned_block(101, "ab", 300.0, 700.0, 0)];
        let (verdict, _) = prove_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::SharedGlyph));

        // A canonical glyph shared with another block.
        let mut second = positioned_block(2, "xy", 300.0, 600.0, 0);
        second.canonical.source_map[0] = glyph_entry(0, glyph(1, 0));
        let old_blocks = vec![positioned_block(1, "ab", 300.0, 700.0, 0), second];
        let new_blocks = vec![positioned_block(101, "ab", 300.0, 700.0, 0)];
        let (verdict, _) = prove_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::SharedGlyph));

        // A raw glyph repeated outside the selection in the same block.
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut repeated = positioned_block(1, "ab", 300.0, 700.0, 0);
        repeated.raw.source_map = vec![glyph_entry(0, glyph(1, 0)), glyph_entry(1, glyph(1, 0))];
        let (verdict, _) = prove_pair(
            &[repeated],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawSource));

        // A canonical glyph repeated outside the selection in the same block;
        // the repeated glyph leaves the raw run non-contiguous as well.
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut repeated = positioned_block(1, "ab", 300.0, 700.0, 0);
        repeated.canonical.source_map =
            vec![glyph_entry(0, glyph(1, 0)), glyph_entry(1, glyph(1, 0))];
        let (verdict, _) = prove_pair(
            &[repeated],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawCut));
    }

    #[test]
    fn normalization_boundary_holds() {
        let event = NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange { start: 1, end: 2 },
            canonical_range: ScalarRange { start: 1, end: 1 },
            source: TextSource {
                atoms: vec![TextSourceAtom::LineBreak {
                    preceding: glyph(1, 0),
                    following: glyph(1, 1),
                }]
                .into(),
            },
        };
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut with_event = positioned_block(1, "ab", 300.0, 700.0, 0);
        with_event.normalization_events = vec![event];
        let (verdict, _) = prove_pair(
            &[with_event],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        let issue = |raw_range: ScalarRange, atoms: Vec<TextSourceAtom>| NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range,
            source: TextSource {
                atoms: atoms.into(),
            },
        };

        // An issue overlapping the selected glyph holds.
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut overlapping = positioned_block(1, "ab", 300.0, 700.0, 0);
        overlapping.issues = vec![issue(
            ScalarRange { start: 0, end: 1 },
            vec![TextSourceAtom::Glyph(glyph(1, 0))],
        )];
        let (verdict, _) = prove_pair(
            &[overlapping],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // A malformed issue holds even when it sits outside the selection.
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut malformed = positioned_block(1, "ab", 300.0, 700.0, 0);
        malformed.issues = vec![issue(ScalarRange { start: 1, end: 2 }, Vec::new())];
        let (verdict, _) = prove_pair(
            &[malformed],
            &[new],
            &fragment(1, 0, 1),
            &fragment(101, 0, 1),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // A zero-length issue is never silently ignored.
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut empty = positioned_block(1, "ab", 300.0, 700.0, 0);
        empty.issues = vec![issue(
            ScalarRange { start: 1, end: 1 },
            vec![TextSourceAtom::Glyph(glyph(1, 1))],
        )];
        let (verdict, _) = prove_pair(
            &[empty],
            &[new],
            &fragment(1, 0, 1),
            &fragment(101, 0, 1),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // A valid issue outside the selection does not hold the fragment.
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut outside = positioned_block(1, "ab", 300.0, 700.0, 0);
        outside.issues = vec![issue(
            ScalarRange { start: 1, end: 2 },
            vec![TextSourceAtom::Glyph(glyph(1, 1))],
        )];
        let (verdict, _) = prove_pair(
            &[outside],
            &[new],
            &fragment(1, 0, 1),
            &fragment(101, 0, 1),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
    }

    #[test]
    fn outside_normalization_events_are_isolated_from_the_fragment() {
        let new = positioned_block(101, "AB CD", 300.0, 700.0, 0);
        let collapsed = collapsed_block(1, 300.0, 700.0);
        // `CD` sits away from the collapse event and keeps a contiguous
        // single-glyph raw run, so the fragment proves with the event present.
        let (verdict, used) = prove_pair(
            std::slice::from_ref(&collapsed),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
        assert!(used > 0);
        let (verdict, consumed) = prove_pair(
            std::slice::from_ref(&collapsed),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            used - 1,
        );
        assert_eq!(verdict, FragmentVerdict::Exhausted);
        assert_eq!(consumed, used - 1);
        let (verdict, consumed) = prove_pair(
            std::slice::from_ref(&collapsed),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            used,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
        assert_eq!(consumed, used);

        // The event's own canonical token is not a single real glyph source,
        // so selecting it holds before any event range is considered.
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&collapsed),
            std::slice::from_ref(&new),
            &fragment(1, 2, 3),
            &fragment(101, 2, 3),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::CanonicalSource)
        );

        // A deleted line break at the fragment's end cut holds.
        let deleted = deleted_break_block(1, 300.0, 700.0);
        let new_abcd = positioned_block(101, "ABCD", 300.0, 700.0, 0);
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&deleted),
            std::slice::from_ref(&new_abcd),
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // The same event at the fragment's start cut holds.
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&deleted),
            std::slice::from_ref(&new_abcd),
            &fragment(1, 2, 4),
            &fragment(101, 2, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // An outside event whose line-break endpoint is a selected glyph holds.
        let kept = kept_break_block(1, 300.0, 700.0);
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&kept),
            &[positioned_block(101, "A B", 300.0, 700.0, 0)],
            &fragment(1, 0, 1),
            &fragment(101, 0, 1),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // A malformed outside event holds instead of being ignored.
        let mut empty_source = collapsed_block(1, 300.0, 700.0);
        empty_source.normalization_events[0].source = TextSource::default();
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&empty_source),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        let mut reversed = collapsed_block(1, 300.0, 700.0);
        reversed.normalization_events[0].raw_range = ScalarRange { start: 4, end: 2 };
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&reversed),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // An outside event whose source is not the raw evidence at its range
        // holds instead of being ignored.
        let mut foreign = collapsed_block(1, 300.0, 700.0);
        foreign.normalization_events[0].source = TextSource {
            atoms: vec![TextSourceAtom::Glyph(glyph(1, 1))].into(),
        };
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&foreign),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // An outside event whose canonical range points at the wrong canonical
        // source holds instead of being ignored.
        let mut misplaced = collapsed_block(1, 300.0, 700.0);
        misplaced.normalization_events[0].canonical_range = ScalarRange { start: 0, end: 1 };
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&misplaced),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // An outside event whose canonical range leaves a gap holds as well.
        let mut gap = collapsed_block(1, 300.0, 700.0);
        gap.normalization_events[0].canonical_range = ScalarRange { start: 2, end: 4 };
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&gap),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // An empty canonical range is only a valid deletion when the event
        // source has really disappeared from the canonical map; an arbitrary
        // empty position that still carries the source holds.
        let mut emptied = collapsed_block(1, 300.0, 700.0);
        emptied.normalization_events[0].canonical_range = ScalarRange { start: 1, end: 1 };
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&emptied),
            std::slice::from_ref(&new),
            &fragment(1, 3, 5),
            &fragment(101, 3, 5),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // A legitimate deleted line break strictly outside the selection still
        // proves: its source is absent from the canonical map.
        let mut deleted = positioned_block(1, "ABC", 300.0, 700.0, 0);
        deleted.raw = MappedText {
            text: "A\nBC".to_owned(),
            source_map: vec![
                glyph_entry(0, glyph(1, 0)),
                atoms_entry(
                    1,
                    vec![TextSourceAtom::LineBreak {
                        preceding: glyph(1, 0),
                        following: glyph(1, 1),
                    }],
                ),
                glyph_entry(2, glyph(1, 1)),
                glyph_entry(3, glyph(1, 2)),
            ],
            unmapped: Vec::new(),
        };
        deleted.normalization_events = vec![NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange { start: 1, end: 2 },
            canonical_range: ScalarRange { start: 1, end: 1 },
            source: TextSource {
                atoms: vec![TextSourceAtom::LineBreak {
                    preceding: glyph(1, 0),
                    following: glyph(1, 1),
                }]
                .into(),
            },
        }];
        let (verdict, _) = prove_pair(
            std::slice::from_ref(&deleted),
            &[positioned_block(101, "ABC", 300.0, 700.0, 0)],
            &fragment(1, 2, 3),
            &fragment(101, 2, 3),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
    }

    #[test]
    fn normalization_validation_stays_linear_without_issues_or_events() {
        let old_small = positioned_block(1, "abcdefgh", 300.0, 700.0, 0);
        let new_small = positioned_block(101, "abcdefgh", 300.0, 700.0, 0);
        let old_large = positioned_block(1, "abcdefghijklmnop", 300.0, 700.0, 0);
        let new_large = positioned_block(101, "abcdefghijklmnop", 300.0, 700.0, 0);
        let (verdict, small) = prove_pair(
            std::slice::from_ref(&old_small),
            std::slice::from_ref(&new_small),
            &fragment(1, 0, 8),
            &fragment(101, 0, 8),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
        let (verdict, large) = prove_pair(
            std::slice::from_ref(&old_large),
            std::slice::from_ref(&new_large),
            &fragment(1, 0, 16),
            &fragment(101, 0, 16),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
        assert!(
            large < small * 3,
            "doubling the input must stay sub-quadratic: {large} vs {small}"
        );
    }

    #[test]
    fn multiple_issues_outside_the_selection_are_charged_and_prove() {
        let issue = |start: usize, glyph: GlyphId| NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange {
                start,
                end: start + 1,
            },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(glyph)].into(),
            },
        };
        let plain = positioned_block(1, "abcd", 300.0, 700.0, 0);
        let new = positioned_block(101, "abcd", 300.0, 700.0, 0);
        let mut with_issues = positioned_block(1, "abcd", 300.0, 700.0, 0);
        with_issues.issues = vec![
            issue(1, glyph(1, 1)),
            issue(2, glyph(1, 2)),
            issue(3, glyph(1, 3)),
        ];
        let event = |start: usize, glyph: GlyphId| NormalizationEvent {
            kind: NormalizationKind::SoftLineBreak,
            raw_range: ScalarRange {
                start,
                end: start + 1,
            },
            canonical_range: ScalarRange {
                start,
                end: start + 1,
            },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(glyph)].into(),
            },
        };
        with_issues.normalization_events = vec![event(1, glyph(1, 1)), event(3, glyph(1, 3))];
        let old_span = fragment(1, 0, 1);
        let new_span = fragment(101, 0, 1);
        let (plain_verdict, plain_used) = prove_pair(
            std::slice::from_ref(&plain),
            std::slice::from_ref(&new),
            &old_span,
            &new_span,
            usize::MAX,
        );
        assert_eq!(plain_verdict, FragmentVerdict::Proven);
        let (verdict, used) = prove_pair(
            std::slice::from_ref(&with_issues),
            std::slice::from_ref(&new),
            &old_span,
            &new_span,
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);
        assert!(
            used > plain_used,
            "the issue projection must be charged: {used} vs {plain_used}"
        );
        let (verdict, consumed) = prove_pair(
            std::slice::from_ref(&with_issues),
            std::slice::from_ref(&new),
            &old_span,
            &new_span,
            used - 1,
        );
        assert_eq!(verdict, FragmentVerdict::Exhausted);
        assert_eq!(consumed, used - 1);
    }

    #[test]
    fn unmapped_blocks_hold() {
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut canonical_unmapped = positioned_block(1, "ab", 300.0, 700.0, 0);
        canonical_unmapped.canonical.unmapped = vec![UnmappedToken {
            scalar_index: 0,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 1,
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(1099))].into(),
            },
        }];
        let signature = canonical_unmapped
            .position_signatures
            .as_ref()
            .expect("fixture positions")[0];
        canonical_unmapped.position_signatures = Some(vec![signature; 3]);
        let font_size = canonical_unmapped
            .font_size_signatures
            .as_ref()
            .expect("fixture font sizes")[0]
            .clone();
        canonical_unmapped.font_size_signatures = Some(vec![font_size; 3]);
        let old_span = TextSpan {
            blocks: vec![BlockId(1)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: TokenRange { start: 1, end: 3 },
        };
        let (verdict, _) = prove_pair(
            &[canonical_unmapped],
            &[new],
            &old_span,
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::UnmappedBlock));

        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut raw_unmapped = positioned_block(1, "ab", 300.0, 700.0, 0);
        raw_unmapped.raw.unmapped = vec![UnmappedToken {
            scalar_index: 0,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 1,
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(1099))].into(),
            },
        }];
        let (verdict, _) = prove_pair(
            &[raw_unmapped],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::UnmappedBlock));
    }

    #[test]
    fn missing_positions_hold() {
        let mut old = positioned_block(1, "ab", 300.0, 700.0, 0);
        old.position_signatures = None;
        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let (verdict, _) = prove_pair(
            &[old],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::MissingPositions)
        );
    }

    #[test]
    fn non_finite_positions_hold() {
        for (baseline, direction) in [
            (
                Vec2 {
                    x: f64::NAN,
                    y: 0.0,
                },
                Vec2 { x: 1.0, y: 0.0 },
            ),
            (
                Vec2 {
                    x: f64::INFINITY,
                    y: 0.0,
                },
                Vec2 { x: 1.0, y: 0.0 },
            ),
            (
                Vec2 { x: 0.0, y: 0.0 },
                Vec2 {
                    x: f64::NAN,
                    y: 0.0,
                },
            ),
        ] {
            let mut old = positioned_block(1, "ab", 300.0, 700.0, 0);
            let mut new = positioned_block(101, "ab", 300.0, 700.0, 0);
            let signature = PositionSignature::from_raw_bits(baseline, direction);
            old.position_signatures = Some(vec![signature; 2]);
            new.position_signatures = Some(vec![signature; 2]);
            let (verdict, _) = prove_pair(
                &[old],
                &[new],
                &fragment(1, 0, 2),
                &fragment(101, 0, 2),
                usize::MAX,
            );
            assert_eq!(
                verdict,
                FragmentVerdict::Held(FragmentHold::PositionMismatch)
            );
        }
    }

    #[test]
    fn lowest_bit_position_difference_holds() {
        let old = positioned_block(1, "ab", 1.0, 700.0, 0);
        let new = positioned_block(101, "ab", 1.0 + f64::EPSILON, 700.0, 0);
        let (verdict, _) = prove_pair(
            &[old],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::PositionMismatch)
        );
    }

    #[test]
    fn page_differences_hold() {
        let old = positioned_block(1, "ab", 300.0, 700.0, 0);
        let new = positioned_block(101, "ab", 300.0, 700.0, 1);
        let (verdict, _) = prove_pair(
            &[old],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::PageMismatch));

        let new = positioned_block(101, "ab", 300.0, 700.0, 0);
        let mut multiple = positioned_block(1, "ab", 300.0, 700.0, 0);
        multiple.pages = vec![0, 1];
        multiple.page_breaks = Some(vec![1]);
        let (verdict, _) = prove_pair(
            &[multiple],
            &[new],
            &fragment(1, 0, 2),
            &fragment(101, 0, 2),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::MultiplePages));
    }

    #[test]
    fn every_partial_budget_exhausts_without_a_partial_proof() {
        // The proof must stop at every cut point, including inside the raw
        // validation and before or after the document-wide sharing scan, with
        // `Exhausted` and never with a partial `Proven`.
        let old_blocks = vec![positioned_block(1, "abcdefgh", 300.0, 700.0, 0)];
        let new_blocks = vec![positioned_block(101, "abcdefgh", 300.0, 700.0, 0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_span = fragment(1, 2, 6);
        let new_span = fragment(101, 2, 6);
        // Each budget runs on a fresh cache so the measured cut points are the
        // uncached proof's own charges.
        let mut fresh = EqualFragmentCache::new([&old, &new]);
        let mut unlimited = usize::MAX;
        assert_eq!(
            fresh
                .prove([&old_span, &new_span], &mut unlimited)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        let total = usize::MAX - unlimited;
        assert!(total > 0);
        for budget in 0..total {
            let mut cache = EqualFragmentCache::new([&old, &new]);
            let mut remaining = budget;
            let verdict = cache
                .prove([&old_span, &new_span], &mut remaining)
                .expect("valid fixture evidence");
            assert_eq!(
                verdict,
                FragmentVerdict::Exhausted,
                "budget {budget} of {total}"
            );
            assert_eq!(remaining, 0, "budget {budget} of {total}");
        }
        let mut cache = EqualFragmentCache::new([&old, &new]);
        let mut exact = total;
        assert_eq!(
            cache
                .prove([&old_span, &new_span], &mut exact)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        assert_eq!(exact, 0);
    }

    #[test]
    fn cached_reuse_keeps_the_proof_and_never_leaks_a_partial_one() {
        let old_blocks = vec![positioned_block(1, "abcdefgh", 300.0, 700.0, 0)];
        let new_blocks = vec![positioned_block(101, "abcdefgh", 300.0, 700.0, 0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_span = fragment(1, 2, 6);
        let new_span = fragment(101, 2, 6);
        let mut cache = EqualFragmentCache::new([&old, &new]);
        let mut first = usize::MAX;
        assert_eq!(
            cache
                .prove([&old_span, &new_span], &mut first)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        let build_cost = usize::MAX - first;
        let mut second = usize::MAX;
        assert_eq!(
            cache
                .prove([&old_span, &new_span], &mut second)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        let cached_cost = usize::MAX - second;
        assert!(
            cached_cost < build_cost,
            "the shared indexes must be reused: {cached_cost} vs {build_cost}"
        );
        // A populated cache must never turn an exhausted budget into a proof.
        let mut low = cached_cost - 1;
        assert_eq!(
            cache
                .prove([&old_span, &new_span], &mut low)
                .expect("valid fixture evidence"),
            FragmentVerdict::Exhausted
        );
        assert_eq!(low, 0);
        // The cache stays usable after the exhausted attempt.
        let mut enough = cached_cost;
        assert_eq!(
            cache
                .prove([&old_span, &new_span], &mut enough)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        assert_eq!(enough, 0);
    }

    #[test]
    fn internal_deleted_soft_line_break_proves_only_in_the_fallback() {
        let old_blocks = vec![deleted_break_block(1, 300.0, 700.0)];
        let new_blocks = vec![deleted_break_block(101, 300.0, 700.0)];
        let old_span = fragment(1, 0, 4);
        let new_span = fragment(101, 0, 4);
        // The original proof keeps its contiguous-run contract untouched.
        let (verdict, _) = prove_pair(&old_blocks, &new_blocks, &old_span, &new_span, usize::MAX);
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawCut));
        let (verdict, used) =
            prove_internal_pair(&old_blocks, &new_blocks, &old_span, &new_span, usize::MAX);
        assert_eq!(verdict, FragmentVerdict::Proven);
        assert!(used > 0);
    }

    #[test]
    fn internal_break_wrong_endpoints_hold() {
        // The raw atom names a foreign following glyph.
        let mut old = deleted_break_block(1, 300.0, 700.0);
        old.raw.source_map[2] = atoms_entry(
            2,
            vec![TextSourceAtom::LineBreak {
                preceding: glyph(1, 1),
                following: GlyphId(9999),
            }],
        );
        let new = deleted_break_block(101, 300.0, 700.0);
        let (verdict, _) = prove_internal_pair(
            &[old],
            std::slice::from_ref(&new),
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawCut));

        // The raw atom is right but the event source names a foreign endpoint.
        let mut old = deleted_break_block(1, 300.0, 700.0);
        old.normalization_events[0].source = TextSource {
            atoms: vec![TextSourceAtom::LineBreak {
                preceding: glyph(1, 0),
                following: GlyphId(9999),
            }]
            .into(),
        };
        let (verdict, _) = prove_internal_pair(
            &[old],
            std::slice::from_ref(&new),
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );
    }

    #[test]
    fn internal_break_wrong_event_output_boundary_holds() {
        let mut old = deleted_break_block(1, 300.0, 700.0);
        old.normalization_events[0].canonical_range = ScalarRange { start: 1, end: 1 };
        let new = deleted_break_block(101, 300.0, 700.0);
        let (verdict, _) = prove_internal_pair(
            &[old],
            &[new],
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );
    }

    #[test]
    fn internal_break_real_character_deletion_holds() {
        let mut old = deleted_break_block(1, 300.0, 700.0);
        old.raw.text = "ABxCD".to_owned();
        let new = deleted_break_block(101, 300.0, 700.0);
        let (verdict, _) = prove_internal_pair(
            &[old],
            &[new],
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawCut));
    }

    #[test]
    fn mismatched_side_break_offsets_hold() {
        let old_blocks = vec![deleted_break_at(1, 1)];
        let new_blocks = vec![deleted_break_at(101, 2)];
        let (verdict, _) = prove_internal_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );
    }

    #[test]
    fn duplicate_internal_break_event_holds() {
        let mut old = deleted_break_block(1, 300.0, 700.0);
        let duplicate = old.normalization_events[0].clone();
        old.normalization_events.push(duplicate);
        let new = deleted_break_block(101, 300.0, 700.0);
        let (verdict, _) = prove_internal_pair(
            &[old],
            &[new],
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );
    }

    #[test]
    fn internal_break_at_a_selection_cut_holds() {
        let old_blocks = vec![deleted_break_block(1, 300.0, 700.0)];
        let new_blocks = vec![deleted_break_block(101, 300.0, 700.0)];
        for (start, end) in [(0usize, 2usize), (2, 4)] {
            let (verdict, _) = prove_internal_pair(
                &old_blocks,
                &new_blocks,
                &fragment(1, start, end),
                &fragment(101, start, end),
                usize::MAX,
            );
            assert_eq!(
                verdict,
                FragmentVerdict::Held(FragmentHold::NormalizationBoundary),
                "fragment {start}..{end}"
            );
        }
    }

    #[test]
    fn internal_break_shared_glyph_holds() {
        let mut second = positioned_block(2, "xy", 300.0, 600.0, 0);
        second.raw.source_map[0] = glyph_entry(0, glyph(1, 0));
        let old_blocks = vec![deleted_break_block(1, 300.0, 700.0), second];
        let new_blocks = vec![deleted_break_block(101, 300.0, 700.0)];
        let (verdict, _) = prove_internal_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::SharedGlyph));
    }

    #[test]
    fn internal_break_fallback_exhausts_without_a_partial_proof() {
        let old_blocks = vec![deleted_break_block(1, 300.0, 700.0)];
        let new_blocks = vec![deleted_break_block(101, 300.0, 700.0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_span = fragment(1, 0, 4);
        let new_span = fragment(101, 0, 4);
        let mut fresh = EqualFragmentCache::new([&old, &new]);
        let mut unlimited = usize::MAX;
        assert_eq!(
            fresh
                .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut unlimited)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        let total = usize::MAX - unlimited;
        assert!(total > 0);
        for budget in 0..total {
            let mut cache = EqualFragmentCache::new([&old, &new]);
            let mut remaining = budget;
            assert_eq!(
                cache
                    .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut remaining)
                    .expect("valid fixture evidence"),
                FragmentVerdict::Exhausted,
                "budget {budget} of {total}"
            );
            assert_eq!(remaining, 0, "budget {budget} of {total}");
        }
        let mut cache = EqualFragmentCache::new([&old, &new]);
        let mut exact = total;
        assert_eq!(
            cache
                .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut exact)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        assert_eq!(exact, 0);
    }

    #[test]
    fn multiple_internal_breaks_prove_and_a_conflicting_touching_event_holds() {
        let old_blocks = vec![double_deleted_break_block(1)];
        let new_blocks = vec![double_deleted_break_block(101)];
        let (verdict, _) = prove_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Held(FragmentHold::RawCut));
        let (verdict, _) = prove_internal_pair(
            &old_blocks,
            &new_blocks,
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(verdict, FragmentVerdict::Proven);

        // A second touching event with the wrong canonical boundary is never
        // skipped by the admission of the first break.
        let mut conflicting = double_deleted_break_block(1);
        conflicting.normalization_events[1].canonical_range = ScalarRange { start: 1, end: 1 };
        let (verdict, _) = prove_internal_pair(
            &[conflicting],
            &[double_deleted_break_block(101)],
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );

        // A second touching event of an unsupported kind holds as well.
        let mut unsupported = double_deleted_break_block(1);
        unsupported.normalization_events[1].kind = NormalizationKind::WhitespaceCollapse;
        let (verdict, _) = prove_internal_pair(
            &[unsupported],
            &[double_deleted_break_block(101)],
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );
    }

    #[test]
    fn internal_break_with_an_overlapping_issue_holds() {
        let mut old = deleted_break_block(1, 300.0, 700.0);
        old.issues = vec![NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(glyph(1, 0))].into(),
            },
        }];
        let new = deleted_break_block(101, 300.0, 700.0);
        let (verdict, _) = prove_internal_pair(
            &[old],
            &[new],
            &fragment(1, 0, 4),
            &fragment(101, 0, 4),
            usize::MAX,
        );
        assert_eq!(
            verdict,
            FragmentVerdict::Held(FragmentHold::NormalizationBoundary)
        );
    }

    #[test]
    fn cached_internal_break_fallback_reuses_and_never_leaks_a_partial_proof() {
        let old_blocks = vec![deleted_break_block(1, 300.0, 700.0)];
        let new_blocks = vec![deleted_break_block(101, 300.0, 700.0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_span = fragment(1, 0, 4);
        let new_span = fragment(101, 0, 4);
        let mut cache = EqualFragmentCache::new([&old, &new]);
        let mut first = usize::MAX;
        assert_eq!(
            cache
                .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut first)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        let build_cost = usize::MAX - first;
        let mut second = usize::MAX;
        assert_eq!(
            cache
                .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut second)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        let cached_cost = usize::MAX - second;
        assert!(
            cached_cost < build_cost,
            "the shared indexes must be reused: {cached_cost} vs {build_cost}"
        );
        let mut low = cached_cost - 1;
        assert_eq!(
            cache
                .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut low)
                .expect("valid fixture evidence"),
            FragmentVerdict::Exhausted
        );
        assert_eq!(low, 0);
        let mut enough = cached_cost;
        assert_eq!(
            cache
                .prove_internal_deleted_soft_line_break([&old_span, &new_span], &mut enough)
                .expect("valid fixture evidence"),
            FragmentVerdict::Proven
        );
        assert_eq!(enough, 0);
    }
}
