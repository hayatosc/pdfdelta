//! Raw-source isomorphism between two whole original blocks.
//!
//! The comparison proves that both blocks carry the same native characters,
//! sources, pages and positions in the same order without interpreting how the
//! reading order and line breaks are normalized. It never turns unknown
//! metadata into equality: unsupported atoms, events, issues, projections or
//! unknown break evidence hold the pair instead of accepting it. A
//! [`RawSourceVerdict::Different`] verdict states that the isomorphism
//! conditions do not hold; it is not by itself a confirmed content change and
//! must not be reported as one.
//!
//! Document-local glyph identifiers are normalized to their occurrence ordinal
//! in the raw scalar sequence, so numeric identifier agreement is never
//! evidence. Every side must independently satisfy its own topology checks
//! before the two sides are compared, so two identically malformed blocks hold
//! instead of matching.

use std::collections::HashMap;

use crate::{
    diff::Side,
    model::GlyphId,
    normalize::{
        BlockText, MappedText, NormalizationIssueKind, NormalizationKind, PositionSignature,
        TextSourceAtom,
    },
};

/// Outcome of the raw-source isomorphism proof.
///
/// A partial run never reports [`Self::Isomorphic`]: budget exhaustion, held
/// evidence and a failed isomorphism check stay distinguishable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawSourceVerdict {
    /// Both blocks are raw-source isomorphic.
    Isomorphic,
    /// The evidence shows that the isomorphism conditions do not hold.
    ///
    /// The mismatch may equally come from a content edit or from malformed or
    /// contradictory source evidence, so this verdict alone never establishes
    /// a change.
    Different,
    /// The evidence is incomplete or unsupported, so no claim is made.
    Held(RawSourceHold),
    /// The work budget or an allocation was exhausted.
    Exhausted,
}

/// Reason a raw-source comparison makes no equality claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawSourceHold {
    /// At least one scalar has no scalar projection.
    UnmappedProjection,
    /// A synthetic or otherwise unsupported source atom is present.
    UnsupportedAtom,
    /// A normalization event kind outside the supported set is present.
    UnsupportedEvent,
    /// A normalization issue kind outside the supported set is present.
    UnsupportedIssue,
    /// A source map does not cover every scalar exactly once with one atom.
    InvalidSourceMap,
    /// Position signatures are missing or misaligned with the canonical scalars.
    MissingPositions,
    /// A block spans zero or multiple pages.
    MultiplePages,
    /// Line-break evidence is unknown or inconsistent on at least one side.
    UnknownLineBreaks,
    /// Page-break evidence is unknown or inconsistent with a single page.
    UnknownPageBreaks,
    /// A line break's endpoints are not its adjacent source glyphs.
    LineBreakTopology,
    /// Glyph presence, uniqueness or the raw-to-canonical bijection is broken.
    GlyphEvidence,
    /// A line break is not explained by a soft line break event or issue.
    UnexplainedLineBreak,
    /// A real glyph of the selected range occurs more than once in its own
    /// raw or canonical document-wide universe.
    SharedRealGlyph,
    /// An event or issue range does not match its source evidence.
    InvalidEventRange,
}

/// How one raw line break was consumed by a soft line break event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Consumed {
    /// The raw line break is deleted and claimed to sit at this canonical
    /// scalar boundary.
    Deleted { canonical_start: usize },
    /// The raw line break is substituted by the canonical line break at this
    /// scalar.
    Kept(usize),
}

/// One side's validated raw and canonical source projection.
struct SideAnalysis<'a> {
    raw: Vec<TextSourceAtom>,
    canonical: Vec<TextSourceAtom>,
    /// Raw occurrence ordinal of every real glyph.
    raw_index_of: HashMap<GlyphId, usize>,
    /// Line break consumption per raw scalar.
    consumed: Vec<Option<Consumed>>,
    /// Canonical scalar image of every raw scalar, or `None` for a deleted
    /// line break. Filled by the already charged projection walk.
    canonical_of_raw: Vec<Option<usize>>,
    positions: &'a [PositionSignature],
}

/// Whether two whole original blocks are raw-source isomorphic.
///
/// The proof requires equal literal raw and canonical text, one page with
/// empty page breaks, known and equal line-break offsets, ordered one-scalar
/// source maps without holes, and a one-to-one ordered projection from raw to
/// canonical scalars in which every real glyph keeps the same literal scalar.
/// Each side must additionally prove that every line break's endpoints are its
/// adjacent raw glyphs and that the canonical line break keeps the same raw
/// endpoints. Deleted or space-substituted line breaks are accepted only when
/// a soft line break event explains exactly one raw line break with an exactly
/// matching source and, for deletions, the canonical boundary position. Every
/// retained line break and ambiguous line break issue must be isomorphic
/// across the pair, and every real glyph's position signature must match
/// bit-exactly. Unsupported or contradictory evidence holds the pair, and two
/// identically malformed blocks hold rather than match.
///
/// `remaining` bounds all scanning and allocation work. Text length, map,
/// event, issue and break counts are charged before any scan, and per-element
/// charges cover every later walk. On exhaustion the verdict is
/// [`RawSourceVerdict::Exhausted`] and no partial proof is reported.
pub(crate) fn raw_source_isomorphic(
    old: &BlockText,
    new: &BlockText,
    remaining: &mut usize,
) -> RawSourceVerdict {
    if !charge(remaining, 64) {
        return RawSourceVerdict::Exhausted;
    }
    let text_cost = text_bytes(old).saturating_add(text_bytes(new));
    let map_cost = map_entries(old).saturating_add(map_entries(new));
    let break_cost = break_entries(old).saturating_add(break_entries(new));
    let event_count = old
        .normalization_events
        .len()
        .saturating_add(new.normalization_events.len());
    let issue_count = old.issues.len().saturating_add(new.issues.len());
    let counts = map_cost
        .saturating_add(break_cost)
        .saturating_add(event_count)
        .saturating_add(issue_count);
    if !charge(remaining, text_cost) || !charge(remaining, counts) {
        return RawSourceVerdict::Exhausted;
    }
    if old.raw.text != new.raw.text || old.canonical.text != new.canonical.text {
        return RawSourceVerdict::Different;
    }
    if !old.raw.unmapped.is_empty()
        || !old.canonical.unmapped.is_empty()
        || !new.raw.unmapped.is_empty()
        || !new.canonical.unmapped.is_empty()
    {
        return RawSourceVerdict::Held(RawSourceHold::UnmappedProjection);
    }
    let raw_scalars = old.raw.text.chars().count();
    let canonical_scalars = old.canonical.text.chars().count();
    if new.raw.text.chars().count() != raw_scalars
        || new.canonical.text.chars().count() != canonical_scalars
    {
        return RawSourceVerdict::Different;
    }
    if old.pages.len() != 1 || new.pages.len() != 1 {
        return RawSourceVerdict::Held(RawSourceHold::MultiplePages);
    }
    if old.pages != new.pages {
        return RawSourceVerdict::Different;
    }
    let Some(old_line_breaks) = old.line_breaks.as_deref() else {
        return RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks);
    };
    let Some(new_line_breaks) = new.line_breaks.as_deref() else {
        return RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks);
    };
    if !valid_offsets(old_line_breaks, canonical_scalars)
        || !valid_offsets(new_line_breaks, canonical_scalars)
    {
        return RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks);
    }
    if old.line_breaks != new.line_breaks {
        return RawSourceVerdict::Different;
    }
    if !is_empty_breaks(old.page_breaks.as_deref()) || !is_empty_breaks(new.page_breaks.as_deref())
    {
        return RawSourceVerdict::Held(RawSourceHold::UnknownPageBreaks);
    }
    let old_side = match analyze_side(old, raw_scalars, canonical_scalars, remaining) {
        Ok(side) => side,
        Err(verdict) => return verdict,
    };
    let new_side = match analyze_side(new, raw_scalars, canonical_scalars, remaining) {
        Ok(side) => side,
        Err(verdict) => return verdict,
    };
    compare_sides(old, new, &old_side, &new_side, remaining)
}

fn charge(remaining: &mut usize, amount: usize) -> bool {
    if *remaining < amount {
        *remaining = 0;
        false
    } else {
        *remaining -= amount;
        true
    }
}

fn text_bytes(block: &BlockText) -> usize {
    block
        .raw
        .text
        .len()
        .saturating_add(block.canonical.text.len())
}

fn map_entries(block: &BlockText) -> usize {
    block
        .raw
        .source_map
        .len()
        .saturating_add(block.canonical.source_map.len())
}

fn break_entries(block: &BlockText) -> usize {
    block
        .line_breaks
        .as_ref()
        .map_or(0, Vec::len)
        .saturating_add(block.page_breaks.as_ref().map_or(0, Vec::len))
}

/// Whether sorted offsets stay strictly below `scalars`.
fn valid_offsets(offsets: &[usize], scalars: usize) -> bool {
    offsets
        .iter()
        .try_fold(0usize, |previous, offset| {
            if *offset < previous || *offset >= scalars {
                None
            } else {
                Some(*offset + 1)
            }
        })
        .is_some()
}

fn is_empty_breaks(breaks: Option<&[usize]>) -> bool {
    matches!(breaks, Some(offsets) if offsets.is_empty())
}

/// Validates one side and builds its raw-to-canonical projection.
fn analyze_side<'a>(
    block: &'a BlockText,
    raw_scalars: usize,
    canonical_scalars: usize,
    remaining: &mut usize,
) -> Result<SideAnalysis<'a>, RawSourceVerdict> {
    let Some(positions) = block.position_signatures.as_deref() else {
        return Err(RawSourceVerdict::Held(RawSourceHold::MissingPositions));
    };
    if positions.len() != canonical_scalars {
        return Err(RawSourceVerdict::Held(RawSourceHold::MissingPositions));
    }
    let raw = map_atoms(&block.raw, raw_scalars, remaining)?;
    let canonical = map_atoms(&block.canonical, canonical_scalars, remaining)?;
    let raw_chars = text_chars(&block.raw.text, raw_scalars, remaining)?;
    let canonical_chars = text_chars(&block.canonical.text, canonical_scalars, remaining)?;
    if raw
        .iter()
        .chain(canonical.iter())
        .any(|atom| matches!(atom, TextSourceAtom::SyntheticSpace { .. }))
    {
        return Err(RawSourceVerdict::Held(RawSourceHold::UnsupportedAtom));
    }
    let mut last_glyph = None;
    for (index, atom) in canonical.iter().enumerate() {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if matches!(atom, TextSourceAtom::Glyph(_)) {
            last_glyph = Some(index);
        }
        if matches!(atom, TextSourceAtom::LineBreak { .. }) {
            let Some(preceding) = last_glyph else {
                return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
            };
            if positions[preceding] != positions[index] {
                return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
            }
        }
    }
    let mut raw_index_of = HashMap::new();
    if raw_index_of.try_reserve(raw.len()).is_err() {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    for (index, atom) in raw.iter().enumerate() {
        if let TextSourceAtom::Glyph(glyph) = atom
            && raw_index_of.insert(*glyph, index).is_some()
        {
            return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
        }
    }
    let mut nearest_before = Vec::new();
    let mut nearest_after = Vec::new();
    if nearest_before.try_reserve(raw.len()).is_err()
        || nearest_after.try_reserve(raw.len()).is_err()
    {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    let mut last = None;
    for atom in &raw {
        nearest_before.push(last);
        if let TextSourceAtom::Glyph(glyph) = atom {
            last = Some(*glyph);
        }
    }
    nearest_after.resize(raw.len(), None);
    let mut next = None;
    for (index, atom) in raw.iter().enumerate().rev() {
        nearest_after[index] = next;
        if let TextSourceAtom::Glyph(glyph) = atom {
            next = Some(*glyph);
        }
    }
    for (index, atom) in raw.iter().enumerate() {
        if let TextSourceAtom::LineBreak {
            preceding,
            following,
        } = atom
        {
            if raw_chars[index] != '\n' {
                return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
            }
            let (Some(before), Some(after)) = (nearest_before[index], nearest_after[index]) else {
                return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
            };
            if *preceding != before || *following != after {
                return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
            }
        }
    }
    let mut consumed = Vec::new();
    if consumed.try_reserve(raw.len()).is_err() {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    consumed.resize(raw.len(), None);
    for event in &block.normalization_events {
        if !charge(remaining, 1 + event.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if event.kind != NormalizationKind::SoftLineBreak {
            return Err(RawSourceVerdict::Held(RawSourceHold::UnsupportedEvent));
        }
        let canonical_start = event.canonical_range.start;
        let canonical_end = event.canonical_range.end;
        if canonical_start > canonical_end || canonical_end > canonical.len() {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        let range = event.raw_range;
        if range.start >= range.end || range.end > raw.len() || range.end - range.start != 1 {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        let index = range.start;
        if consumed[index].is_some()
            || !matches!(raw[index], TextSourceAtom::LineBreak { .. })
            || raw_chars[index] != '\n'
        {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        if event.source.atoms.len() != 1 || event.source.atoms[0] != raw[index] {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        match canonical_end - canonical_start {
            0 => consumed[index] = Some(Consumed::Deleted { canonical_start }),
            1 => {
                let target = canonical_start;
                if canonical_chars[target] != ' '
                    || !matches!(canonical[target], TextSourceAtom::LineBreak { .. })
                {
                    return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
                }
                if let TextSourceAtom::LineBreak {
                    preceding,
                    following,
                } = &canonical[target]
                {
                    let TextSourceAtom::LineBreak {
                        preceding: raw_preceding,
                        following: raw_following,
                    } = &raw[index]
                    else {
                        return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
                    };
                    if preceding != raw_preceding || following != raw_following {
                        return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
                    }
                }
                consumed[index] = Some(Consumed::Kept(target));
            }
            _ => return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)),
        }
    }
    let mut canonical_of_raw = Vec::new();
    if canonical_of_raw.try_reserve(raw.len()).is_err() {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    canonical_of_raw.resize(raw.len(), None);
    let mut canonical_index = 0usize;
    for (raw_index, raw_atom) in raw.iter().enumerate() {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        match consumed[raw_index] {
            Some(Consumed::Deleted { canonical_start }) => {
                if canonical_start != canonical_index {
                    return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
                }
                continue;
            }
            Some(Consumed::Kept(target)) => {
                if target != canonical_index
                    || !matches!(canonical[canonical_index], TextSourceAtom::LineBreak { .. })
                {
                    return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
                }
                canonical_of_raw[raw_index] = Some(canonical_index);
                canonical_index += 1;
                continue;
            }
            None => {}
        }
        let Some(canonical_atom) = canonical.get(canonical_index) else {
            return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
        };
        match (raw_atom, canonical_atom) {
            (TextSourceAtom::Glyph(raw_glyph), TextSourceAtom::Glyph(canonical_glyph)) => {
                if raw_glyph != canonical_glyph
                    || raw_chars[raw_index] != canonical_chars[canonical_index]
                {
                    return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
                }
            }
            (
                TextSourceAtom::LineBreak {
                    preceding: raw_preceding,
                    following: raw_following,
                },
                TextSourceAtom::LineBreak {
                    preceding: canonical_preceding,
                    following: canonical_following,
                },
            ) => {
                if raw_preceding != canonical_preceding
                    || raw_following != canonical_following
                    || canonical_chars[canonical_index] != '\n'
                {
                    return Err(RawSourceVerdict::Held(RawSourceHold::LineBreakTopology));
                }
            }
            _ => return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence)),
        }
        canonical_of_raw[raw_index] = Some(canonical_index);
        canonical_index += 1;
    }
    if canonical_index != canonical.len() {
        return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
    }
    let glyphs = canonical
        .iter()
        .filter(|atom| matches!(atom, TextSourceAtom::Glyph(_)))
        .count();
    if glyphs != raw_index_of.len() {
        return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
    }
    let mut issue_covered = Vec::new();
    if issue_covered.try_reserve(raw.len()).is_err() {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    issue_covered.resize(raw.len(), false);
    for issue in &block.issues {
        if !charge(remaining, 1 + issue.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if issue.kind != NormalizationIssueKind::AmbiguousLineBreak {
            return Err(RawSourceVerdict::Held(RawSourceHold::UnsupportedIssue));
        }
        let range = issue.raw_range;
        if range.start >= range.end || range.end > raw.len() || range.end - range.start != 1 {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        let index = range.start;
        if consumed[index].is_some() {
            return Err(RawSourceVerdict::Held(RawSourceHold::UnexplainedLineBreak));
        }
        if !matches!(raw[index], TextSourceAtom::LineBreak { .. }) {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        if issue.source.atoms.len() != 1 || issue.source.atoms[0] != raw[index] {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        if issue_covered[index] {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        issue_covered[index] = true;
        let Some(target) = canonical_of_raw[index] else {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        };
        if !matches!(canonical[target], TextSourceAtom::LineBreak { .. }) {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
    }
    Ok(SideAnalysis {
        raw,
        canonical,
        raw_index_of,
        consumed,
        canonical_of_raw,
        positions,
    })
}

fn map_atoms(
    mapped: &MappedText,
    scalars: usize,
    remaining: &mut usize,
) -> Result<Vec<TextSourceAtom>, RawSourceVerdict> {
    if mapped.source_map.len() != scalars {
        return Err(RawSourceVerdict::Held(RawSourceHold::InvalidSourceMap));
    }
    let mut atoms = Vec::new();
    if atoms.try_reserve(scalars).is_err() {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    for (index, entry) in mapped.source_map.iter().enumerate() {
        if !charge(remaining, 1 + entry.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if entry.output_range.start != index
            || entry.output_range.end != index + 1
            || entry.source.atoms.len() != 1
        {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidSourceMap));
        }
        atoms.push(entry.source.atoms[0].clone());
    }
    Ok(atoms)
}

fn text_chars(
    text: &str,
    scalars: usize,
    remaining: &mut usize,
) -> Result<Vec<char>, RawSourceVerdict> {
    let mut chars = Vec::new();
    if chars.try_reserve(scalars).is_err() {
        *remaining = 0;
        return Err(RawSourceVerdict::Exhausted);
    }
    for scalar in text.chars() {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        chars.push(scalar);
    }
    if chars.len() != scalars {
        return Err(RawSourceVerdict::Held(RawSourceHold::InvalidSourceMap));
    }
    Ok(chars)
}

fn endpoint_ordinal(
    raw_index_of: &HashMap<GlyphId, usize>,
    glyph: GlyphId,
) -> Result<usize, RawSourceVerdict> {
    raw_index_of
        .get(&glyph)
        .copied()
        .ok_or(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence))
}

fn compare_sides(
    old: &BlockText,
    new: &BlockText,
    old_side: &SideAnalysis<'_>,
    new_side: &SideAnalysis<'_>,
    remaining: &mut usize,
) -> RawSourceVerdict {
    if old_side.consumed != new_side.consumed {
        return RawSourceVerdict::Different;
    }
    for (old_atom, new_atom) in old_side.raw.iter().zip(&new_side.raw) {
        if !charge(remaining, 1) {
            return RawSourceVerdict::Exhausted;
        }
        match atom_verdict(old_atom, new_atom, old_side, new_side) {
            RawSourceVerdict::Isomorphic => {}
            verdict => return verdict,
        }
    }
    if old_side.canonical.len() != new_side.canonical.len() {
        return RawSourceVerdict::Different;
    }
    for (index, (old_atom, new_atom)) in old_side
        .canonical
        .iter()
        .zip(&new_side.canonical)
        .enumerate()
    {
        if !charge(remaining, 1) {
            return RawSourceVerdict::Exhausted;
        }
        match atom_verdict(old_atom, new_atom, old_side, new_side) {
            RawSourceVerdict::Isomorphic => {}
            verdict => return verdict,
        }
        if matches!(old_atom, TextSourceAtom::Glyph(_))
            && old_side.positions[index] != new_side.positions[index]
        {
            return RawSourceVerdict::Different;
        }
    }
    if old.normalization_events.len() != new.normalization_events.len()
        || old.issues.len() != new.issues.len()
    {
        return RawSourceVerdict::Different;
    }
    for (old_event, new_event) in old
        .normalization_events
        .iter()
        .zip(&new.normalization_events)
    {
        if !charge(remaining, 1 + old_event.source.atoms.len()) {
            return RawSourceVerdict::Exhausted;
        }
        if old_event.kind != new_event.kind
            || old_event.raw_range != new_event.raw_range
            || old_event.canonical_range != new_event.canonical_range
        {
            return RawSourceVerdict::Different;
        }
        if old_event.source.atoms.len() != new_event.source.atoms.len() {
            return RawSourceVerdict::Different;
        }
        for (old_atom, new_atom) in old_event.source.atoms.iter().zip(&new_event.source.atoms) {
            match atom_verdict(old_atom, new_atom, old_side, new_side) {
                RawSourceVerdict::Isomorphic => {}
                verdict => return verdict,
            }
        }
    }
    for (old_issue, new_issue) in old.issues.iter().zip(&new.issues) {
        if !charge(remaining, 1 + old_issue.source.atoms.len()) {
            return RawSourceVerdict::Exhausted;
        }
        if old_issue.kind != new_issue.kind || old_issue.raw_range != new_issue.raw_range {
            return RawSourceVerdict::Different;
        }
        if old_issue.source.atoms.len() != new_issue.source.atoms.len() {
            return RawSourceVerdict::Different;
        }
        for (old_atom, new_atom) in old_issue.source.atoms.iter().zip(&new_issue.source.atoms) {
            match atom_verdict(old_atom, new_atom, old_side, new_side) {
                RawSourceVerdict::Isomorphic => {}
                verdict => return verdict,
            }
        }
    }
    RawSourceVerdict::Isomorphic
}

fn atom_verdict(
    old_atom: &TextSourceAtom,
    new_atom: &TextSourceAtom,
    old_side: &SideAnalysis<'_>,
    new_side: &SideAnalysis<'_>,
) -> RawSourceVerdict {
    match (old_atom, new_atom) {
        (TextSourceAtom::Glyph(old_glyph), TextSourceAtom::Glyph(new_glyph)) => {
            let old_ordinal = match endpoint_ordinal(&old_side.raw_index_of, *old_glyph) {
                Ok(ordinal) => ordinal,
                Err(verdict) => return verdict,
            };
            let new_ordinal = match endpoint_ordinal(&new_side.raw_index_of, *new_glyph) {
                Ok(ordinal) => ordinal,
                Err(verdict) => return verdict,
            };
            if old_ordinal == new_ordinal {
                RawSourceVerdict::Isomorphic
            } else {
                RawSourceVerdict::Different
            }
        }
        (
            TextSourceAtom::LineBreak {
                preceding: old_preceding,
                following: old_following,
            },
            TextSourceAtom::LineBreak {
                preceding: new_preceding,
                following: new_following,
            },
        ) => {
            for (old_glyph, new_glyph) in [
                (old_preceding, new_preceding),
                (old_following, new_following),
            ] {
                let old_ordinal = match endpoint_ordinal(&old_side.raw_index_of, *old_glyph) {
                    Ok(ordinal) => ordinal,
                    Err(verdict) => return verdict,
                };
                let new_ordinal = match endpoint_ordinal(&new_side.raw_index_of, *new_glyph) {
                    Ok(ordinal) => ordinal,
                    Err(verdict) => return verdict,
                };
                if old_ordinal != new_ordinal {
                    return RawSourceVerdict::Different;
                }
            }
            RawSourceVerdict::Isomorphic
        }
        _ => RawSourceVerdict::Held(RawSourceHold::UnsupportedAtom),
    }
}

#[cfg(test)]
mod tests {
    use smallvec::smallvec;

    use super::*;
    use crate::{
        layout::{BlockId, BlockRole},
        model::Vec2,
        normalize::{NormalizationEvent, NormalizationIssue, ScalarRange, TextSource},
    };

    fn glyph(id: u64) -> TextSourceAtom {
        TextSourceAtom::Glyph(GlyphId(id))
    }

    fn line_break(preceding: u64, following: u64) -> TextSourceAtom {
        TextSourceAtom::LineBreak {
            preceding: GlyphId(preceding),
            following: GlyphId(following),
        }
    }

    fn entry(index: usize, atom: TextSourceAtom) -> crate::normalize::SourceMapEntry {
        crate::normalize::SourceMapEntry {
            output_range: ScalarRange {
                start: index,
                end: index + 1,
            },
            source: TextSource {
                atoms: smallvec![atom],
            },
        }
    }

    fn mapped(text: &str, atoms: Vec<TextSourceAtom>) -> crate::normalize::MappedText {
        crate::normalize::MappedText {
            text: text.to_owned(),
            source_map: atoms
                .into_iter()
                .enumerate()
                .map(|(index, atom)| entry(index, atom))
                .collect(),
            unmapped: Vec::new(),
        }
    }

    fn position(x: f64) -> PositionSignature {
        PositionSignature::new(Vec2 { x, y: 100.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position")
    }

    /// A block with one deleted line break and one retained ambiguous one.
    ///
    /// Raw `A\nB\nC` becomes canonical `AB\nC`: the first line break is
    /// deleted by a soft line break event and the second is retained with an
    /// ambiguous line break issue.
    /// Builds a block whose canonical line-break signature mirrors its
    /// preceding glyph, as the source projection requires.
    fn block(block: u64, glyphs: [u64; 3], positions: [f64; 4]) -> BlockText {
        let [first, second, third] = glyphs;
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped(
                "A\nB\nC",
                vec![
                    glyph(first),
                    line_break(first, second),
                    glyph(second),
                    line_break(second, third),
                    glyph(third),
                ],
            ),
            canonical: mapped(
                "AB\nC",
                vec![
                    glyph(first),
                    glyph(second),
                    line_break(second, third),
                    glyph(third),
                ],
            ),
            matching: "AB\nC".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: smallvec![line_break(first, second)],
                },
            }],
            issues: vec![NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: ScalarRange { start: 3, end: 4 },
                source: TextSource {
                    atoms: smallvec![line_break(second, third)],
                },
            }],
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some(positions.into_iter().map(position).collect()),
            line_breaks: Some(vec![2]),
            page_breaks: Some(Vec::new()),
        }
    }

    fn isomorphic(old: &BlockText, new: &BlockText) -> RawSourceVerdict {
        let mut budget = usize::MAX;
        raw_source_isomorphic(old, new, &mut budget)
    }

    fn upfront_charge(block: &BlockText) -> usize {
        text_bytes(block)
            .saturating_add(map_entries(block))
            .saturating_add(break_entries(block))
            .saturating_add(block.normalization_events.len())
            .saturating_add(block.issues.len())
    }

    #[test]
    fn raw_source_isomorphism_accepts_identical_evidence() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        assert_eq!(isomorphic(&old, &new), RawSourceVerdict::Isomorphic);
    }

    #[test]
    fn raw_source_isomorphism_accepts_document_local_glyph_ids() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let new = block(9, [11, 12, 13], [10.0, 20.0, 20.0, 40.0]);
        assert_eq!(isomorphic(&old, &new), RawSourceVerdict::Isomorphic);
    }

    #[test]
    fn raw_source_isomorphism_accepts_space_substituted_line_break() {
        let substitute = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            block.canonical = mapped(
                "A B\nC",
                vec![
                    glyph(1),
                    line_break(1, 2),
                    glyph(2),
                    line_break(2, 3),
                    glyph(3),
                ],
            );
            block.normalization_events = vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 2 },
                source: TextSource {
                    atoms: smallvec![line_break(1, 2)],
                },
            }];
            block.position_signatures = Some(vec![
                position(10.0),
                position(10.0),
                position(20.0),
                position(20.0),
                position(40.0),
            ]);
            block
        };
        let old = substitute(1);
        let new = substitute(2);
        assert_eq!(isomorphic(&old, &new), RawSourceVerdict::Isomorphic);
    }

    #[test]
    fn raw_source_isomorphism_rejects_changed_raw_text() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.raw.text = "A\nB\nX".to_owned();
        assert_eq!(isomorphic(&old, &new), RawSourceVerdict::Different);
    }

    #[test]
    fn raw_source_isomorphism_holds_on_shared_glyph() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.canonical = mapped(
            "AB\nC",
            vec![glyph(1), glyph(1), line_break(2, 3), glyph(3)],
        );
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::GlyphEvidence)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_missing_glyph() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.raw = mapped(
            "A\nB\nC",
            vec![glyph(1), line_break(1, 2), glyph(2), glyph(2), glyph(3)],
        );
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::GlyphEvidence)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_wrong_endpoint_on_both_sides() {
        let invalid = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 30.0, 40.0]);
            block.raw = mapped(
                "A\nB\nC",
                vec![
                    glyph(1),
                    line_break(1, 3),
                    glyph(2),
                    line_break(2, 3),
                    glyph(3),
                ],
            );
            block.normalization_events[0].source = TextSource {
                atoms: smallvec![line_break(1, 3)],
            };
            block
        };
        let old = invalid(1);
        let new = invalid(2);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::LineBreakTopology)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_canonical_endpoint_mismatch() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.canonical = mapped(
            "AB\nC",
            vec![glyph(1), glyph(2), line_break(1, 3), glyph(3)],
        );
        new.issues = vec![NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 3, end: 4 },
            source: TextSource {
                atoms: smallvec![line_break(2, 3)],
            },
        }];
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::LineBreakTopology)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_invalid_event_range() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.normalization_events[0].raw_range = ScalarRange { start: 3, end: 5 };
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_deleted_event_position() {
        let invalid = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            block.normalization_events[0].canonical_range = ScalarRange {
                start: 999,
                end: 999,
            };
            block
        };
        let old = invalid(1);
        let new = invalid(2);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
        let reversed = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            block.normalization_events[0].canonical_range = ScalarRange { start: 2, end: 1 };
            block
        };
        let old = reversed(3);
        let new = reversed(4);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_event_source_mismatch() {
        let invalid = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            block.normalization_events[0].source = TextSource {
                atoms: smallvec![line_break(1, 3)],
            };
            block
        };
        let old = invalid(1);
        let new = invalid(2);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_issue_source_mismatch() {
        let invalid = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            block.issues[0].source = TextSource {
                atoms: smallvec![glyph(1)],
            };
            block
        };
        let old = invalid(1);
        let new = invalid(2);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_duplicate_issue() {
        let invalid = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            let duplicate = block.issues[0].clone();
            block.issues.push(duplicate);
            block
        };
        let old = invalid(1);
        let new = invalid(2);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn raw_source_isomorphism_rejects_moved_position() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let new = block(2, [1, 2, 3], [10.5, 20.0, 20.0, 40.0]);
        assert_eq!(isomorphic(&old, &new), RawSourceVerdict::Different);
    }

    #[test]
    fn raw_source_isomorphism_holds_on_unknown_positions() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut missing = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        missing.position_signatures = None;
        assert_eq!(
            isomorphic(&old, &missing),
            RawSourceVerdict::Held(RawSourceHold::MissingPositions)
        );
        let mut short = block(3, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        short.position_signatures = Some(vec![position(10.0)]);
        assert_eq!(
            isomorphic(&old, &short),
            RawSourceVerdict::Held(RawSourceHold::MissingPositions)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_unknown_breaks() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut line = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        line.line_breaks = None;
        assert_eq!(
            isomorphic(&old, &line),
            RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks)
        );
        let mut page = block(3, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        page.page_breaks = None;
        assert_eq!(
            isomorphic(&old, &page),
            RawSourceVerdict::Held(RawSourceHold::UnknownPageBreaks)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_wrong_line_break_signature() {
        let invalid = |block_id: u64| {
            let mut block = block(block_id, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
            let mut signatures = block.position_signatures.take().expect("positions");
            signatures[2] = position(99.0);
            block.position_signatures = Some(signatures);
            block
        };
        let old = invalid(1);
        let new = invalid(2);
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::LineBreakTopology)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_unsupported_event() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.normalization_events[0].kind = NormalizationKind::Nfc;
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::UnsupportedEvent)
        );
    }

    #[test]
    fn raw_source_isomorphism_holds_on_unsupported_atom() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let mut new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        new.canonical = mapped(
            "AB\nC",
            vec![
                glyph(1),
                TextSourceAtom::SyntheticSpace {
                    preceding: GlyphId(1),
                    following: GlyphId(2),
                },
                line_break(2, 3),
                glyph(3),
            ],
        );
        assert_eq!(
            isomorphic(&old, &new),
            RawSourceVerdict::Held(RawSourceHold::UnsupportedAtom)
        );
    }

    #[test]
    fn raw_source_isomorphism_exhausts_budget() {
        let old = block(1, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        let new = block(2, [1, 2, 3], [10.0, 20.0, 20.0, 40.0]);
        for budget in [0usize, 1, 63, 64] {
            let mut remaining = budget;
            assert_eq!(
                raw_source_isomorphic(&old, &new, &mut remaining),
                RawSourceVerdict::Exhausted,
                "budget {budget}"
            );
        }
        let text = upfront_charge(&old).saturating_add(upfront_charge(&new));
        for budget in [64, 64 + text / 2, 64 + text - 1, 64 + text] {
            let mut remaining = budget;
            assert_eq!(
                raw_source_isomorphic(&old, &new, &mut remaining),
                RawSourceVerdict::Exhausted,
                "budget {budget}"
            );
        }
        assert_eq!(isomorphic(&old, &new), RawSourceVerdict::Isomorphic);
    }
}

/// Non-bypassable range certificate: the validated source proof first, then
/// document-wide real-glyph sharing on the proven projections.
///
/// A caller can only observe [`RawSourceVerdict::Isomorphic`] when every range
/// check and the sharing condition hold. The certificate claims only the
/// selected source correspondence: it never asserts equality, order or
/// document-wide uniqueness of glyphs outside the selected raw ranges.
/// `Exhausted` never carries a proof.
pub(crate) fn raw_source_range_certificate(
    sides: [&Side<'_>; 2],
    old_block: usize,
    old_range: std::ops::Range<usize>,
    new_block: usize,
    new_range: std::ops::Range<usize>,
    delta_bits: (u64, u64),
    remaining: &mut usize,
) -> RawSourceVerdict {
    let (delta_x_bits, delta_y_bits) = delta_bits;
    let (Some(old), Some(new)) = (
        sides[0].blocks.get(old_block),
        sides[1].blocks.get(new_block),
    ) else {
        return RawSourceVerdict::Held(RawSourceHold::GlyphEvidence);
    };
    let (verdict, raw_ranges) = range_proof_core(
        old,
        old_range.clone(),
        new,
        new_range.clone(),
        (delta_x_bits, delta_y_bits),
        remaining,
    );
    if verdict != RawSourceVerdict::Isomorphic {
        return verdict;
    }
    let Some((old_raw, new_raw)) = raw_ranges else {
        return RawSourceVerdict::Held(RawSourceHold::GlyphEvidence);
    };
    let Some(old_sharing) = super::equal_fragment::sharing_index(sides[0], remaining) else {
        return RawSourceVerdict::Exhausted;
    };
    let Some(new_sharing) = super::equal_fragment::sharing_index(sides[1], remaining) else {
        return RawSourceVerdict::Exhausted;
    };
    match selection_shared(old, &old_range, &old_raw, &old_sharing, remaining) {
        Ok(false) => {}
        Ok(true) => return RawSourceVerdict::Held(RawSourceHold::SharedRealGlyph),
        Err(verdict) => return verdict,
    }
    match selection_shared(new, &new_range, &new_raw, &new_sharing, remaining) {
        Ok(false) => {}
        Ok(true) => return RawSourceVerdict::Held(RawSourceHold::SharedRealGlyph),
        Err(verdict) => return verdict,
    }
    RawSourceVerdict::Isomorphic
}

/// Whether every real glyph of the proven selection is unique in its document.
///
/// Only real `Glyph` atoms count; line-break and synthetic-space endpoints are
/// references, never additional occurrences. Map entries that are skipped are
/// charged with the ones that are read.
fn selection_shared(
    block: &BlockText,
    canonical: &std::ops::Range<usize>,
    raw: &std::ops::Range<usize>,
    sharing: &super::equal_fragment::SharingIndex,
    remaining: &mut usize,
) -> Result<bool, RawSourceVerdict> {
    if canonical.start > canonical.end || raw.start > raw.end {
        return Ok(true);
    }
    if !charge(remaining, 1 + block.canonical.source_map.len()) {
        return Err(RawSourceVerdict::Exhausted);
    }
    for entry in &block.canonical.source_map {
        if entry.output_range.start >= canonical.end || canonical.start >= entry.output_range.end {
            continue;
        }
        if !charge(remaining, 1 + entry.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        for atom in &entry.source.atoms {
            if let TextSourceAtom::Glyph(glyph) = atom
                && sharing.is_shared(*glyph)
            {
                return Ok(true);
            }
        }
    }
    if !charge(remaining, 1 + block.raw.source_map.len()) {
        return Err(RawSourceVerdict::Exhausted);
    }
    for entry in &block.raw.source_map {
        if entry.output_range.start >= raw.end || raw.start >= entry.output_range.end {
            continue;
        }
        if !charge(remaining, 1 + entry.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        for atom in &entry.source.atoms {
            if let TextSourceAtom::Glyph(glyph) = atom
                && sharing.is_shared(*glyph)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Proven raw projections of a successful range proof.
type RangeProjections = (std::ops::Range<usize>, std::ops::Range<usize>);

/// Range proof wrapper that keeps the proven raw projections.
///
/// The projections are only returned together with
/// [`RawSourceVerdict::Isomorphic`], so no caller can pair a failed proof with
/// guessed boundaries.
fn range_proof_core(
    old: &BlockText,
    old_range: std::ops::Range<usize>,
    new: &BlockText,
    new_range: std::ops::Range<usize>,
    delta_bits: (u64, u64),
    remaining: &mut usize,
) -> (RawSourceVerdict, Option<RangeProjections>) {
    if !charge(remaining, 64) {
        return (RawSourceVerdict::Exhausted, None);
    }
    let text_cost = text_bytes(old).saturating_add(text_bytes(new));
    let map_cost = map_entries(old).saturating_add(map_entries(new));
    let break_cost = break_entries(old).saturating_add(break_entries(new));
    let event_count = old
        .normalization_events
        .len()
        .saturating_add(new.normalization_events.len());
    let issue_count = old.issues.len().saturating_add(new.issues.len());
    if !charge(remaining, text_cost)
        || !charge(
            remaining,
            map_cost
                .saturating_add(break_cost)
                .saturating_add(event_count)
                .saturating_add(issue_count),
        )
    {
        return (RawSourceVerdict::Exhausted, None);
    }
    let old_scalars = old.canonical.text.chars().count();
    let new_scalars = new.canonical.text.chars().count();
    if old_range.is_empty()
        || new_range.is_empty()
        || old_range.end > old_scalars
        || new_range.end > new_scalars
        || old_range.len() != new_range.len()
    {
        return (
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange),
            None,
        );
    }
    if !old.raw.unmapped.is_empty()
        || !old.canonical.unmapped.is_empty()
        || !new.raw.unmapped.is_empty()
        || !new.canonical.unmapped.is_empty()
    {
        return (
            RawSourceVerdict::Held(RawSourceHold::UnmappedProjection),
            None,
        );
    }
    if old.pages.len() != 1 || new.pages.len() != 1 {
        return (RawSourceVerdict::Held(RawSourceHold::MultiplePages), None);
    }
    if old.pages != new.pages {
        return (RawSourceVerdict::Different, None);
    }
    let (Some(old_breaks), Some(new_breaks)) =
        (old.line_breaks.as_deref(), new.line_breaks.as_deref())
    else {
        return (
            RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks),
            None,
        );
    };
    let old_raw_scalars = old.raw.text.chars().count();
    let new_raw_scalars = new.raw.text.chars().count();
    if !valid_offsets(old_breaks, old_scalars) || !valid_offsets(new_breaks, new_scalars) {
        return (
            RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks),
            None,
        );
    }
    if !is_empty_breaks(old.page_breaks.as_deref()) || !is_empty_breaks(new.page_breaks.as_deref())
    {
        return (
            RawSourceVerdict::Held(RawSourceHold::UnknownPageBreaks),
            None,
        );
    }
    let old_selected_breaks = match selected_breaks(old_breaks, &old_range, remaining) {
        Ok(breaks) => breaks,
        Err(verdict) => return (verdict, None),
    };
    let new_selected_breaks = match selected_breaks(new_breaks, &new_range, remaining) {
        Ok(breaks) => breaks,
        Err(verdict) => return (verdict, None),
    };
    if old_selected_breaks != new_selected_breaks {
        return (RawSourceVerdict::Different, None);
    }
    let old_side = match analyze_side(old, old_raw_scalars, old_scalars, remaining) {
        Ok(side) => side,
        Err(verdict) => return (verdict, None),
    };
    let new_side = match analyze_side(new, new_raw_scalars, new_scalars, remaining) {
        Ok(side) => side,
        Err(verdict) => return (verdict, None),
    };
    let old_raw_range = match range_projection(&old_side, &old_range, remaining) {
        Ok(range) => range,
        Err(verdict) => return (verdict, None),
    };
    let new_raw_range = match range_projection(&new_side, &new_range, remaining) {
        Ok(range) => range,
        Err(verdict) => return (verdict, None),
    };
    match cut_event_verdict(
        &old.normalization_events,
        &old_range,
        &old_raw_range,
        remaining,
    ) {
        Ok(None) => {}
        Ok(Some(verdict)) => return (verdict, None),
        Err(verdict) => return (verdict, None),
    }
    match cut_event_verdict(
        &new.normalization_events,
        &new_range,
        &new_raw_range,
        remaining,
    ) {
        Ok(None) => {}
        Ok(Some(verdict)) => return (verdict, None),
        Err(verdict) => return (verdict, None),
    }
    match touching_issue_verdict(
        IssueCut {
            issues: &old.issues,
            raw: &old_raw_range,
            side: &old_side,
            canonical_start: old_range.start,
        },
        IssueCut {
            issues: &new.issues,
            raw: &new_raw_range,
            side: &new_side,
            canonical_start: new_range.start,
        },
        remaining,
    ) {
        Ok(None) => {}
        Ok(Some(verdict)) => return (verdict, None),
        Err(verdict) => return (verdict, None),
    }
    match boundary_line_break(&old_side, &old_range, &old_raw_range, remaining) {
        Ok(true) => {
            return (
                RawSourceVerdict::Held(RawSourceHold::LineBreakTopology),
                None,
            );
        }
        Ok(false) => {}
        Err(verdict) => return (verdict, None),
    }
    match boundary_line_break(&new_side, &new_range, &new_raw_range, remaining) {
        Ok(true) => {
            return (
                RawSourceVerdict::Held(RawSourceHold::LineBreakTopology),
                None,
            );
        }
        Ok(false) => {}
        Err(verdict) => return (verdict, None),
    }
    match text_range_equal(
        &old.canonical.text,
        &old_range,
        &new.canonical.text,
        &new_range,
        remaining,
    ) {
        Ok(true) => {}
        Ok(false) => return (RawSourceVerdict::Different, None),
        Err(verdict) => return (verdict, None),
    }
    match text_range_equal(
        &old.raw.text,
        &old_raw_range,
        &new.raw.text,
        &new_raw_range,
        remaining,
    ) {
        Ok(true) => {}
        Ok(false) => return (RawSourceVerdict::Different, None),
        Err(verdict) => return (verdict, None),
    }
    let old_raw_atoms = &old_side.raw[old_raw_range.clone()];
    let new_raw_atoms = &new_side.raw[new_raw_range.clone()];
    if old_raw_atoms.len() != new_raw_atoms.len() {
        return (RawSourceVerdict::Different, None);
    }
    for (old_atom, new_atom) in old_raw_atoms.iter().zip(new_raw_atoms) {
        if !charge(remaining, 1) {
            return (RawSourceVerdict::Exhausted, None);
        }
        if !line_break_inside(&old_side, &old_raw_range, old_atom)
            || !line_break_inside(&new_side, &new_raw_range, new_atom)
        {
            return (
                RawSourceVerdict::Held(RawSourceHold::LineBreakTopology),
                None,
            );
        }
        match range_atom_verdict(
            old_atom,
            new_atom,
            &old_side,
            old_raw_range.start,
            &new_side,
            new_raw_range.start,
        ) {
            RawSourceVerdict::Isomorphic => {}
            verdict => return (verdict, None),
        }
    }
    let old_canonical_atoms = &old_side.canonical[old_range.clone()];
    let new_canonical_atoms = &new_side.canonical[new_range.clone()];
    if old_canonical_atoms.len() != new_canonical_atoms.len() {
        return (RawSourceVerdict::Different, None);
    }
    for (index, (old_atom, new_atom)) in old_canonical_atoms
        .iter()
        .zip(new_canonical_atoms)
        .enumerate()
    {
        if !charge(remaining, 1) {
            return (RawSourceVerdict::Exhausted, None);
        }
        if !line_break_inside(&old_side, &old_raw_range, old_atom)
            || !line_break_inside(&new_side, &new_raw_range, new_atom)
        {
            return (
                RawSourceVerdict::Held(RawSourceHold::LineBreakTopology),
                None,
            );
        }
        match range_atom_verdict(
            old_atom,
            new_atom,
            &old_side,
            old_raw_range.start,
            &new_side,
            new_raw_range.start,
        ) {
            RawSourceVerdict::Isomorphic => {}
            verdict => return (verdict, None),
        }
        if matches!(old_atom, TextSourceAtom::Glyph(_)) {
            let old_position = old_side.positions[old_range.start + index];
            let new_position = new_side.positions[new_range.start + index];
            if !range_positions_match(old_position, new_position, delta_bits) {
                return (RawSourceVerdict::Different, None);
            }
        }
    }
    let old_events = match range_events(&old.normalization_events, &old_range, remaining) {
        Ok(events) => events,
        Err(verdict) => return (verdict, None),
    };
    let new_events = match range_events(&new.normalization_events, &new_range, remaining) {
        Ok(events) => events,
        Err(verdict) => return (verdict, None),
    };
    if old_events.len() != new_events.len() {
        return (RawSourceVerdict::Different, None);
    }
    for (old_event, new_event) in old_events.iter().zip(&new_events) {
        if !charge(remaining, 1 + old_event.source.atoms.len()) {
            return (RawSourceVerdict::Exhausted, None);
        }
        if old_event.kind != new_event.kind
            || old_event.canonical_range.start - old_range.start
                != new_event.canonical_range.start - new_range.start
            || old_event.canonical_range.end - old_range.start
                != new_event.canonical_range.end - new_range.start
            || old_event.raw_range.start - old_raw_range.start
                != new_event.raw_range.start - new_raw_range.start
            || old_event.raw_range.end - old_raw_range.start
                != new_event.raw_range.end - new_raw_range.start
            || old_event.source.atoms.len() != new_event.source.atoms.len()
        {
            return (RawSourceVerdict::Different, None);
        }
        for (old_atom, new_atom) in old_event.source.atoms.iter().zip(&new_event.source.atoms) {
            if !charge(remaining, 1) {
                return (RawSourceVerdict::Exhausted, None);
            }
            match range_atom_verdict(
                old_atom,
                new_atom,
                &old_side,
                old_raw_range.start,
                &new_side,
                new_raw_range.start,
            ) {
                RawSourceVerdict::Isomorphic => {}
                verdict => return (verdict, None),
            }
        }
    }
    let old_issues = match range_issues(&old.issues, &old_raw_range, remaining) {
        Ok(issues) => issues,
        Err(verdict) => return (verdict, None),
    };
    let new_issues = match range_issues(&new.issues, &new_raw_range, remaining) {
        Ok(issues) => issues,
        Err(verdict) => return (verdict, None),
    };
    if old_issues.len() != new_issues.len() {
        return (RawSourceVerdict::Different, None);
    }
    for (old_issue, new_issue) in old_issues.iter().zip(&new_issues) {
        if !charge(remaining, 1 + old_issue.source.atoms.len()) {
            return (RawSourceVerdict::Exhausted, None);
        }
        if old_issue.kind != new_issue.kind
            || old_issue.raw_range.start - old_raw_range.start
                != new_issue.raw_range.start - new_raw_range.start
            || old_issue.raw_range.end - old_raw_range.start
                != new_issue.raw_range.end - new_raw_range.start
            || old_issue.source.atoms.len() != new_issue.source.atoms.len()
        {
            return (RawSourceVerdict::Different, None);
        }
        for (old_atom, new_atom) in old_issue.source.atoms.iter().zip(&new_issue.source.atoms) {
            if !charge(remaining, 1) {
                return (RawSourceVerdict::Exhausted, None);
            }
            match range_atom_verdict(
                old_atom,
                new_atom,
                &old_side,
                old_raw_range.start,
                &new_side,
                new_raw_range.start,
            ) {
                RawSourceVerdict::Isomorphic => {}
                verdict => return (verdict, None),
            }
        }
    }
    (
        RawSourceVerdict::Isomorphic,
        Some((old_raw_range, new_raw_range)),
    )
}

/// Exact raw projection of one canonical range: every real scalar whose
/// canonical image lies inside the range plus every deleted soft line break
/// whose canonical boundary lies strictly inside it.
fn range_projection(
    side: &SideAnalysis<'_>,
    canonical: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<std::ops::Range<usize>, RawSourceVerdict> {
    let included = |raw_index: usize| -> bool {
        match side.canonical_of_raw[raw_index] {
            Some(target) => canonical.start <= target && target < canonical.end,
            None => match side.consumed[raw_index] {
                Some(Consumed::Deleted { canonical_start }) => {
                    canonical.start < canonical_start && canonical_start < canonical.end
                }
                _ => false,
            },
        }
    };
    let mut first = None;
    let mut last = None;
    for raw_index in 0..side.raw.len() {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if included(raw_index) {
            if first.is_none() {
                first = Some(raw_index);
            }
            last = Some(raw_index);
        }
    }
    let (Some(first), Some(last)) = (first, last) else {
        return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
    };
    for raw_index in first..=last {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if !included(raw_index) {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
    }
    Ok(first..last + 1)
}

/// Selected line-break offsets relative to the range start.
fn selected_breaks(
    breaks: &[usize],
    canonical: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<Vec<usize>, RawSourceVerdict> {
    let mut selected = Vec::new();
    for &value in breaks {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if canonical.start <= value && value < canonical.end {
            if selected.try_reserve(1).is_err() {
                *remaining = 0;
                return Err(RawSourceVerdict::Exhausted);
            }
            selected.push(value - canonical.start);
        }
    }
    Ok(selected)
}

/// Held verdict for events touching or crossing a cut edge.
///
/// Touching events include a deleted break whose canonical boundary equals a
/// cut; the range proof holds rather than widening the cut silently.
fn cut_event_verdict(
    events: &[crate::normalize::NormalizationEvent],
    canonical: &std::ops::Range<usize>,
    raw: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<Option<RawSourceVerdict>, RawSourceVerdict> {
    for event in events {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        let touching = event.canonical_range.end == canonical.start
            || event.canonical_range.start == canonical.end
            || event.raw_range.end == raw.start
            || event.raw_range.start == raw.end;
        let crossing = (event.canonical_range.start < canonical.start
            && canonical.start < event.canonical_range.end)
            || (event.canonical_range.start < canonical.end
                && canonical.end < event.canonical_range.end)
            || (event.raw_range.start < raw.start && raw.start < event.raw_range.end)
            || (event.raw_range.start < raw.end && raw.end < event.raw_range.end);
        if touching || crossing {
            return Ok(Some(RawSourceVerdict::Held(
                RawSourceHold::InvalidEventRange,
            )));
        }
    }
    Ok(None)
}

/// Relative structure of one retained ambiguous issue touching a cut edge.
///
/// `analyze_side` already proved per side that the issue is one raw line break
/// with an adjacent real-glyph endpoint topology, a matching source atom and a
/// canonical line-break image. The cross-side comparison therefore only needs
/// the cut-relative range, the canonical target offset and the endpoint that
/// lies inside the selection. The outside endpoint is deliberately not
/// recorded: the certificate makes no claim about glyphs outside the range.
struct TouchingIssue {
    category: &'static str,
    relative_range: (i64, i64),
    canonical_target: i64,
    inside_endpoint: i64,
}

/// One side of the cut-adjacent ambiguous-issue validation.
struct IssueCut<'a, 'b> {
    issues: &'a [crate::normalize::NormalizationIssue],
    raw: &'a std::ops::Range<usize>,
    side: &'a SideAnalysis<'b>,
    canonical_start: usize,
}

/// Validates retained ambiguous issues touching a cut edge across sides.
///
/// Strict-interior crossings hold. Touching issues stay exterior by half-open
/// semantics but are compared pairwise, so asymmetric or malformed retained
/// evidence is a difference instead of a silent drop.
fn touching_issue_verdict(
    old: IssueCut<'_, '_>,
    new: IssueCut<'_, '_>,
    remaining: &mut usize,
) -> Result<Option<RawSourceVerdict>, RawSourceVerdict> {
    let old_touching = issue_touching_entries(old, remaining)?;
    let new_touching = issue_touching_entries(new, remaining)?;
    if old_touching.len() != new_touching.len() {
        return Ok(Some(RawSourceVerdict::Different));
    }
    for (old_entry, new_entry) in old_touching.iter().zip(&new_touching) {
        if !charge(remaining, 1) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if old_entry.category != new_entry.category
            || old_entry.relative_range != new_entry.relative_range
            || old_entry.canonical_target != new_entry.canonical_target
            || old_entry.inside_endpoint != new_entry.inside_endpoint
        {
            return Ok(Some(RawSourceVerdict::Different));
        }
    }
    Ok(None)
}

/// Borrowed touching-issue entries of one side, charged and fallible.
fn issue_touching_entries(
    cut: IssueCut<'_, '_>,
    remaining: &mut usize,
) -> Result<Vec<TouchingIssue>, RawSourceVerdict> {
    let (issues, raw, side, canonical_start) = (cut.issues, cut.raw, cut.side, cut.canonical_start);
    let mut touching = Vec::new();
    for issue in issues {
        if !charge(remaining, 1 + issue.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        let crossing = (issue.raw_range.start < raw.start && raw.start < issue.raw_range.end)
            || (issue.raw_range.start < raw.end && raw.end < issue.raw_range.end);
        if crossing {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        }
        let category = if issue.raw_range.end == raw.start {
            "raw_left"
        } else if issue.raw_range.start == raw.end {
            "raw_right"
        } else {
            continue;
        };
        let index = issue.raw_range.start;
        let Some(TextSourceAtom::LineBreak {
            preceding,
            following,
        }) = side.raw.get(index)
        else {
            return Err(RawSourceVerdict::Held(RawSourceHold::InvalidEventRange));
        };
        let inside_glyph = if category == "raw_left" {
            *following
        } else {
            *preceding
        };
        let Some(inside_index) = side.raw_index_of.get(&inside_glyph) else {
            return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
        };
        let Some(target) = side.canonical_of_raw.get(index).copied().flatten() else {
            return Err(RawSourceVerdict::Held(RawSourceHold::GlyphEvidence));
        };
        if touching.try_reserve(1).is_err() {
            *remaining = 0;
            return Err(RawSourceVerdict::Exhausted);
        }
        touching.push(TouchingIssue {
            category,
            relative_range: (
                issue.raw_range.start as i64 - raw.start as i64,
                issue.raw_range.end as i64 - raw.start as i64,
            ),
            canonical_target: target as i64 - canonical_start as i64,
            inside_endpoint: *inside_index as i64 - raw.start as i64,
        });
    }
    Ok(touching)
}

/// Whether a cut edge lands on a line-break atom.
fn boundary_line_break(
    side: &SideAnalysis<'_>,
    canonical: &std::ops::Range<usize>,
    raw: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<bool, RawSourceVerdict> {
    if !charge(remaining, 1) {
        return Err(RawSourceVerdict::Exhausted);
    }
    Ok(matches!(
        side.canonical[canonical.start],
        TextSourceAtom::LineBreak { .. }
    ) || matches!(
        side.canonical[canonical.end - 1],
        TextSourceAtom::LineBreak { .. }
    ) || matches!(side.raw[raw.start], TextSourceAtom::LineBreak { .. })
        || matches!(side.raw[raw.end - 1], TextSourceAtom::LineBreak { .. }))
}

/// Whether a line-break atom's real-glyph endpoints lie inside the selection.
fn line_break_inside(
    side: &SideAnalysis<'_>,
    raw: &std::ops::Range<usize>,
    atom: &TextSourceAtom,
) -> bool {
    let TextSourceAtom::LineBreak {
        preceding,
        following,
    } = atom
    else {
        return true;
    };
    let inside = |glyph: GlyphId| {
        side.raw_index_of
            .get(&glyph)
            .is_some_and(|index| raw.contains(index))
    };
    inside(*preceding) && inside(*following)
}

/// Compares two source atoms by selection-relative raw scalar positions.
///
/// `raw_index_of` stores the raw scalar index of every real glyph, so the
/// selection origin is the validated raw range start; glyph counts play no
/// role.
fn range_atom_verdict(
    old_atom: &TextSourceAtom,
    new_atom: &TextSourceAtom,
    old_side: &SideAnalysis<'_>,
    old_raw_start: usize,
    new_side: &SideAnalysis<'_>,
    new_raw_start: usize,
) -> RawSourceVerdict {
    let relative = |side: &SideAnalysis<'_>, glyph: GlyphId, origin: usize| {
        side.raw_index_of
            .get(&glyph)
            .and_then(|index| index.checked_sub(origin))
    };
    match (old_atom, new_atom) {
        (TextSourceAtom::Glyph(old_glyph), TextSourceAtom::Glyph(new_glyph)) => {
            match (
                relative(old_side, *old_glyph, old_raw_start),
                relative(new_side, *new_glyph, new_raw_start),
            ) {
                (Some(old), Some(new)) if old == new => RawSourceVerdict::Isomorphic,
                (Some(_), Some(_)) => RawSourceVerdict::Different,
                _ => RawSourceVerdict::Held(RawSourceHold::GlyphEvidence),
            }
        }
        (
            TextSourceAtom::LineBreak {
                preceding: old_preceding,
                following: old_following,
            },
            TextSourceAtom::LineBreak {
                preceding: new_preceding,
                following: new_following,
            },
        ) => {
            for (old_glyph, new_glyph) in [
                (old_preceding, new_preceding),
                (old_following, new_following),
            ] {
                match (
                    relative(old_side, *old_glyph, old_raw_start),
                    relative(new_side, *new_glyph, new_raw_start),
                ) {
                    (Some(old), Some(new)) if old == new => {}
                    (Some(_), Some(_)) => return RawSourceVerdict::Different,
                    _ => return RawSourceVerdict::Held(RawSourceHold::GlyphEvidence),
                }
            }
            RawSourceVerdict::Isomorphic
        }
        _ => RawSourceVerdict::Held(RawSourceHold::UnsupportedAtom),
    }
}

/// Events whose canonical range intersects the selection, charged and
/// fallible.
fn range_events<'a>(
    events: &'a [crate::normalize::NormalizationEvent],
    canonical: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<Vec<&'a crate::normalize::NormalizationEvent>, RawSourceVerdict> {
    let mut selected = Vec::new();
    for event in events {
        if !charge(remaining, 1 + event.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if event.canonical_range.start < canonical.end
            && canonical.start < event.canonical_range.end
        {
            if selected.try_reserve(1).is_err() {
                *remaining = 0;
                return Err(RawSourceVerdict::Exhausted);
            }
            selected.push(event);
        }
    }
    Ok(selected)
}

/// Issues whose raw range intersects the selection, charged and fallible.
fn range_issues<'a>(
    issues: &'a [crate::normalize::NormalizationIssue],
    raw: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<Vec<&'a crate::normalize::NormalizationIssue>, RawSourceVerdict> {
    let mut selected = Vec::new();
    for issue in issues {
        if !charge(remaining, 1 + issue.source.atoms.len()) {
            return Err(RawSourceVerdict::Exhausted);
        }
        if issue.raw_range.start < raw.end && raw.start < issue.raw_range.end {
            if selected.try_reserve(1).is_err() {
                *remaining = 0;
                return Err(RawSourceVerdict::Exhausted);
            }
            selected.push(issue);
        }
    }
    Ok(selected)
}

/// Literal equality of two scalar ranges, charged for the skipped prefixes and
/// every compared scalar, allocation free.
fn text_range_equal(
    old: &str,
    old_range: &std::ops::Range<usize>,
    new: &str,
    new_range: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Result<bool, RawSourceVerdict> {
    if !charge(remaining, old_range.start)
        || !charge(remaining, new_range.start)
        || !charge(remaining, 1)
    {
        return Err(RawSourceVerdict::Exhausted);
    }
    if old_range.len() != new_range.len() {
        return Ok(false);
    }
    let mut old_chars = old.chars().skip(old_range.start).take(old_range.len());
    let mut new_chars = new.chars().skip(new_range.start).take(new_range.len());
    loop {
        match (old_chars.next(), new_chars.next()) {
            (Some(left), Some(right)) => {
                if !charge(remaining, 1) {
                    return Err(RawSourceVerdict::Exhausted);
                }
                if left != right {
                    return Ok(false);
                }
            }
            (None, None) => return Ok(true),
            _ => return Ok(false),
        }
    }
}

/// Whether two selected positions keep one exact declared translation.
///
/// Directions must be bit-identical and the exact `new - old` difference must
/// match the independently established delta bits; positions are never
/// rewritten to make a check pass.
fn range_positions_match(
    old: PositionSignature,
    new: PositionSignature,
    delta_bits: (u64, u64),
) -> bool {
    let old_direction = old.direction();
    let new_direction = new.direction();
    if old_direction.x.to_bits() != new_direction.x.to_bits()
        || old_direction.y.to_bits() != new_direction.y.to_bits()
    {
        return false;
    }
    let old_baseline = old.baseline();
    let new_baseline = new.baseline();
    (new_baseline.x - old_baseline.x).to_bits() == delta_bits.0
        && (new_baseline.y - old_baseline.y).to_bits() == delta_bits.1
}

#[cfg(test)]
mod range_tests {
    use smallvec::smallvec;

    use super::*;
    use crate::{
        layout::{BlockId, BlockRole},
        model::Vec2,
        normalize::{
            MappedText, NormalizationEvent, NormalizationIssue, NormalizationIssueKind,
            NormalizationKind, ScalarRange, SourceMapEntry, TextSource,
        },
    };

    fn glyph(id: u64) -> TextSourceAtom {
        TextSourceAtom::Glyph(GlyphId(id))
    }

    fn line_break(preceding: u64, following: u64) -> TextSourceAtom {
        TextSourceAtom::LineBreak {
            preceding: GlyphId(preceding),
            following: GlyphId(following),
        }
    }

    fn entry(index: usize, atom: TextSourceAtom) -> SourceMapEntry {
        SourceMapEntry {
            output_range: ScalarRange {
                start: index,
                end: index + 1,
            },
            source: TextSource {
                atoms: smallvec![atom],
            },
        }
    }

    fn mapped(text: &str, atoms: Vec<TextSourceAtom>) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: atoms
                .into_iter()
                .enumerate()
                .map(|(index, atom)| entry(index, atom))
                .collect(),
            unmapped: Vec::new(),
        }
    }

    fn position(x: f64) -> PositionSignature {
        PositionSignature::new(Vec2 { x, y: 100.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position")
    }

    fn plain(block: u64, glyphs: [u64; 3], xs: [f64; 3]) -> BlockText {
        let [first, second, third] = glyphs;
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped("ABC", vec![glyph(first), glyph(second), glyph(third)]),
            canonical: mapped("ABC", vec![glyph(first), glyph(second), glyph(third)]),
            matching: "ABC".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some(xs.into_iter().map(position).collect()),
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    /// Raw `A\nB` with a deleted soft line break and no issues.
    fn deleted_only_block(block: u64, ids: [u64; 2]) -> BlockText {
        let [first, second] = ids;
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped(
                "A\nB",
                vec![glyph(first), line_break(first, second), glyph(second)],
            ),
            canonical: mapped("AB", vec![glyph(first), glyph(second)]),
            matching: "AB".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: smallvec![line_break(first, second)],
                },
            }],
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some([10.0, 30.0].into_iter().map(position).collect()),
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    /// Raw `A\nB\nC` with a deleted break at boundary 1 and an ambiguous
    /// retained break issuing at raw 3.
    fn deletion_block(block: u64) -> BlockText {
        let (first, second, third) = (1, 2, 3);
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped(
                "A\nB\nC",
                vec![
                    glyph(first),
                    line_break(first, second),
                    glyph(second),
                    line_break(second, third),
                    glyph(third),
                ],
            ),
            canonical: mapped(
                "AB\nC",
                vec![
                    glyph(first),
                    glyph(second),
                    line_break(second, third),
                    glyph(third),
                ],
            ),
            matching: "AB\nC".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: smallvec![line_break(first, second)],
                },
            }],
            issues: vec![NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: ScalarRange { start: 3, end: 4 },
                source: TextSource {
                    atoms: smallvec![line_break(second, third)],
                },
            }],
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some([10.0, 20.0, 20.0, 40.0].into_iter().map(position).collect()),
            line_breaks: Some(vec![2]),
            page_breaks: Some(Vec::new()),
        }
    }

    /// Raw `P\nABC` with an ambiguous retained break at raw 1, immediately
    /// before the selection `2..5`; the preceding glyph is outside the range.
    fn adjacent_issue_block(
        block: u64,
        preamble: u64,
        preamble_x: f64,
        ids: [u64; 3],
    ) -> BlockText {
        let [first, second, third] = ids;
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped(
                "P\nABC",
                vec![
                    glyph(preamble),
                    line_break(preamble, first),
                    glyph(first),
                    glyph(second),
                    glyph(third),
                ],
            ),
            canonical: mapped(
                "P\nABC",
                vec![
                    glyph(preamble),
                    line_break(preamble, first),
                    glyph(first),
                    glyph(second),
                    glyph(third),
                ],
            ),
            matching: "P\nABC".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: vec![NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                source: TextSource {
                    atoms: smallvec![line_break(preamble, first)],
                },
            }],
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some(
                [preamble_x, preamble_x, 10.0, 20.0, 30.0]
                    .into_iter()
                    .map(position)
                    .collect(),
            ),
            line_breaks: Some(vec![1]),
            page_breaks: Some(Vec::new()),
        }
    }

    /// Raw `A\nB` with a kept line break at canonical 1.
    fn kept_break_block(block: u64, xs: [f64; 3]) -> BlockText {
        let (first, second) = (1, 2);
        BlockText {
            block: BlockId(block),
            role: BlockRole::Body,
            raw: mapped(
                "A\nB",
                vec![glyph(first), line_break(first, second), glyph(second)],
            ),
            canonical: mapped(
                "A B",
                vec![glyph(first), line_break(first, second), glyph(second)],
            ),
            matching: "A B".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 2 },
                source: TextSource {
                    atoms: smallvec![line_break(first, second)],
                },
            }],
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some(xs.into_iter().map(position).collect()),
            line_breaks: Some(vec![1]),
            page_breaks: Some(Vec::new()),
        }
    }

    fn isomorphic_range(
        old: &BlockText,
        old_range: std::ops::Range<usize>,
        new: &BlockText,
        new_range: std::ops::Range<usize>,
    ) -> RawSourceVerdict {
        let mut budget = usize::MAX;
        range_proof_core(old, old_range, new, new_range, (0, 0), &mut budget).0
    }

    fn translated_range(
        old: &BlockText,
        old_range: std::ops::Range<usize>,
        new: &BlockText,
        new_range: std::ops::Range<usize>,
        delta_bits: (u64, u64),
    ) -> RawSourceVerdict {
        let mut budget = usize::MAX;
        range_proof_core(old, old_range, new, new_range, delta_bits, &mut budget).0
    }

    #[test]
    fn range_isomorphism_accepts_identical_subrange() {
        let old = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        let new = plain(9, [11, 12, 13], [10.0, 20.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 1..3, &new, 1..3),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    fn range_isomorphism_normalizes_shifted_ordinals() {
        // The old prefix carries a retained line break and two glyphs, the new
        // prefix one glyph, so the raw scalar origin difference is not a glyph
        // count difference.
        let old = BlockText {
            block: BlockId(1),
            role: BlockRole::Body,
            raw: mapped(
                "X\nYABC",
                vec![
                    glyph(1),
                    line_break(1, 2),
                    glyph(2),
                    glyph(3),
                    glyph(4),
                    glyph(5),
                ],
            ),
            canonical: mapped(
                "X\nYABC",
                vec![
                    glyph(1),
                    line_break(1, 2),
                    glyph(2),
                    glyph(3),
                    glyph(4),
                    glyph(5),
                ],
            ),
            matching: "X\nYABC".to_owned(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: Some(
                [1.0, 1.0, 2.0, 10.0, 20.0, 30.0]
                    .into_iter()
                    .map(position)
                    .collect(),
            ),
            line_breaks: Some(vec![1]),
            page_breaks: Some(Vec::new()),
        };
        let new = plain(2, [11, 12, 13], [10.0, 20.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 3..6, &new, 0..3),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    fn range_isomorphism_requires_the_declared_translation_rule() {
        let delta = 7.097_999_999_999_956_f64;
        let old = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        let new = plain(2, [4, 5, 6], [10.0 + delta, 20.0 + delta, 30.0 + delta]);
        assert_eq!(
            isomorphic_range(&old, 0..3, &new, 0..3),
            RawSourceVerdict::Different
        );
        assert_eq!(
            translated_range(&old, 0..3, &new, 0..3, (delta.to_bits(), 0.0_f64.to_bits()),),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    fn range_isomorphism_holds_on_identically_malformed_blocks() {
        let old = plain(1, [1, 1, 3], [10.0, 20.0, 30.0]);
        let new = plain(2, [1, 1, 3], [10.0, 20.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 0..3, &new, 0..3),
            RawSourceVerdict::Held(RawSourceHold::GlyphEvidence)
        );
    }

    #[test]
    fn range_isomorphism_accepts_deleted_break_inside() {
        let old = deleted_only_block(1, [1, 2]);
        let new = deleted_only_block(2, [11, 12]);
        assert_eq!(
            isomorphic_range(&old, 0..2, &new, 0..2),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    fn range_isomorphism_holds_on_deletion_touching_cut() {
        let old = deletion_block(1);
        let new = deletion_block(2);
        assert_eq!(
            isomorphic_range(&old, 1..2, &new, 1..2),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn range_isomorphism_accepts_issue_touching_cut() {
        let old = deletion_block(1);
        let new = deletion_block(2);
        assert_eq!(
            isomorphic_range(&old, 0..2, &new, 0..2),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    fn range_isomorphism_does_not_claim_outside_preceding_glyph() {
        // The touching issue's outside preceding glyph differs in identity and
        // position; the certificate must not claim it and must still hold on
        // the inside structure alone.
        let old = adjacent_issue_block(1, 1, 5.0, [2, 3, 4]);
        let new = adjacent_issue_block(2, 9, 999.0, [12, 13, 14]);
        assert_eq!(
            isomorphic_range(&old, 2..5, &new, 2..5),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    fn range_isomorphism_rejects_asymmetric_touching_issue() {
        let old = deletion_block(1);
        let mut new = deletion_block(2);
        new.issues.clear();
        assert_eq!(
            isomorphic_range(&old, 0..2, &new, 0..2),
            RawSourceVerdict::Different
        );
    }

    #[test]
    fn range_isomorphism_holds_on_boundary_line_break() {
        let old = kept_break_block(1, [10.0, 10.0, 30.0]);
        let new = kept_break_block(2, [10.0, 10.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 1..3, &new, 1..3),
            RawSourceVerdict::Held(RawSourceHold::LineBreakTopology)
        );
    }

    #[test]
    fn range_isomorphism_rejects_selected_line_break_mismatch() {
        let mut old = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        old.line_breaks = Some(vec![1]);
        let new = plain(2, [1, 2, 3], [10.0, 20.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 0..3, &new, 0..3),
            RawSourceVerdict::Different
        );
    }

    #[test]
    fn range_isomorphism_holds_on_unknown_line_breaks() {
        let mut old = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        old.line_breaks = None;
        let new = plain(2, [1, 2, 3], [10.0, 20.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 0..3, &new, 0..3),
            RawSourceVerdict::Held(RawSourceHold::UnknownLineBreaks)
        );
    }

    #[test]
    fn range_isomorphism_rejects_different_range_lengths() {
        let old = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        let new = plain(2, [1, 2, 3], [10.0, 20.0, 30.0]);
        assert_eq!(
            isomorphic_range(&old, 0..2, &new, 0..3),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn range_isomorphism_exhausts_budget() {
        let old = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        let new = plain(2, [1, 2, 3], [10.0, 20.0, 30.0]);
        let mut budget = 1usize;
        assert_eq!(
            range_proof_core(&old, 0..3, &new, 0..3, (0, 0), &mut budget).0,
            RawSourceVerdict::Exhausted
        );
    }

    #[test]
    fn range_certificate_accepts_clean_sides() {
        let old_blocks = vec![plain(1, [1, 2, 3], [10.0, 20.0, 30.0])];
        let new_blocks = vec![plain(2, [11, 12, 13], [10.0, 20.0, 30.0])];
        let old_side = super::super::super::SidePlan::inspect("range certificate old", &old_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let new_side = super::super::super::SidePlan::inspect("range certificate new", &new_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let mut budget = usize::MAX;
        assert_eq!(
            raw_source_range_certificate(
                [&old_side, &new_side],
                0,
                0..3,
                0,
                0..3,
                (0, 0),
                &mut budget,
            ),
            RawSourceVerdict::Isomorphic
        );
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)] // deliberate malformed range
    fn range_certificate_holds_on_malformed_range() {
        let old_blocks = vec![plain(1, [1, 2, 3], [10.0, 20.0, 30.0])];
        let new_blocks = vec![plain(2, [11, 12, 13], [10.0, 20.0, 30.0])];
        let old_side = super::super::super::SidePlan::inspect("range malformed old", &old_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let new_side = super::super::super::SidePlan::inspect("range malformed new", &new_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let mut budget = usize::MAX;
        assert_eq!(
            raw_source_range_certificate(
                [&old_side, &new_side],
                0,
                3..1,
                0,
                0..3,
                (0, 0),
                &mut budget,
            ),
            RawSourceVerdict::Held(RawSourceHold::InvalidEventRange)
        );
    }

    #[test]
    fn range_certificate_exhausts_zero_budget() {
        let old_blocks = vec![plain(1, [1, 2, 3], [10.0, 20.0, 30.0])];
        let new_blocks = vec![plain(2, [11, 12, 13], [10.0, 20.0, 30.0])];
        let old_side = super::super::super::SidePlan::inspect("range zero old", &old_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let new_side = super::super::super::SidePlan::inspect("range zero new", &new_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let mut budget = 0usize;
        assert_eq!(
            raw_source_range_certificate(
                [&old_side, &new_side],
                0,
                0..3,
                0,
                0..3,
                (0, 0),
                &mut budget,
            ),
            RawSourceVerdict::Exhausted
        );
    }

    #[test]
    fn range_certificate_rejects_shared_glyph() {
        let selected = plain(1, [1, 2, 3], [10.0, 20.0, 30.0]);
        let other = plain(2, [3, 4, 5], [10.0, 20.0, 30.0]);
        let old_blocks = vec![selected, other];
        let new_blocks = vec![plain(3, [11, 12, 13], [10.0, 20.0, 30.0])];
        let old_side = super::super::super::SidePlan::inspect("range shared old", &old_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let new_side = super::super::super::SidePlan::inspect("range shared new", &new_blocks)
            .expect("valid")
            .materialize()
            .expect("materialize");
        let mut budget = usize::MAX;
        assert_eq!(
            raw_source_range_certificate(
                [&old_side, &new_side],
                0,
                0..3,
                0,
                0..3,
                (0, 0),
                &mut budget,
            ),
            RawSourceVerdict::Held(RawSourceHold::SharedRealGlyph)
        );
    }
}
