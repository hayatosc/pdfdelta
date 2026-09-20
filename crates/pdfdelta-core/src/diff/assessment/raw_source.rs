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
