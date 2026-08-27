use std::collections::{BTreeSet, HashMap, HashSet};

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Error, Result,
    layout::{Block, BlockId, Line, LineId},
    model::{DecodedText, Document, FontProgramHash, Glyph, GlyphId, index_glyphs},
};

pub const DEFAULT_MAX_NUMERIC_MASK_RATIO: f64 = 0.3;
const NUMBER_MASK: &str = "<NUM>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalarRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextSourceAtom {
    Glyph(GlyphId),
    SyntheticSpace {
        preceding: GlyphId,
        following: GlyphId,
    },
    LineBreak {
        preceding: GlyphId,
        following: GlyphId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSource {
    pub atoms: Vec<TextSourceAtom>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapEntry {
    pub output_range: ScalarRange,
    pub source: TextSource,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComparableToken {
    Scalar(char),
    Unmapped {
        font_hash: FontProgramHash,
        glyph_id: u16,
    },
}

impl ComparableToken {
    pub fn as_scalar(&self) -> Option<char> {
        match self {
            Self::Scalar(scalar) => Some(*scalar),
            Self::Unmapped { .. } => None,
        }
    }

    pub fn is_scalar(&self) -> bool {
        matches!(self, Self::Scalar(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmappedToken {
    pub scalar_index: usize,
    pub font_hash: FontProgramHash,
    pub glyph_id: u16,
    pub source: TextSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedText {
    pub text: String,
    pub source_map: Vec<SourceMapEntry>,
    pub unmapped: Vec<UnmappedToken>,
}

impl MappedText {
    pub fn comparable_tokens(&self) -> Result<Vec<ComparableToken>> {
        let (scalar_count, token_count) = self.validated_token_counts()?;
        let mut tokens = Vec::with_capacity(token_count);
        let mut unmapped_index = 0;

        for (index, scalar) in self.text.chars().enumerate() {
            while self
                .unmapped
                .get(unmapped_index)
                .is_some_and(|token| token.scalar_index == index)
            {
                let token = &self.unmapped[unmapped_index];
                tokens.push(ComparableToken::Unmapped {
                    font_hash: token.font_hash.clone(),
                    glyph_id: token.glyph_id,
                });
                unmapped_index += 1;
            }
            tokens.push(ComparableToken::Scalar(scalar));
        }

        for token in &self.unmapped[unmapped_index..] {
            tokens.push(ComparableToken::Unmapped {
                font_hash: token.font_hash.clone(),
                glyph_id: token.glyph_id,
            });
        }

        debug_assert_eq!(tokens.len(), scalar_count + self.unmapped.len());
        Ok(tokens)
    }

    /// Returns comparable tokens paired with their source evidence in exactly
    /// the same order as [`Self::comparable_tokens`].
    ///
    /// # Errors
    ///
    /// Returns an error for invalid unmapped-token offsets or token-count
    /// overflow.
    pub fn comparable_tokens_with_sources(&self) -> Result<Vec<(ComparableToken, TextSource)>> {
        let (scalar_count, token_count) = self.validated_token_counts()?;
        self.validate_source_map(scalar_count)?;
        let mut tokens = Vec::with_capacity(token_count);
        let mut unmapped_index = 0;
        let mut source_index = 0;

        for (index, scalar) in self.text.chars().enumerate() {
            while self
                .unmapped
                .get(unmapped_index)
                .is_some_and(|token| token.scalar_index == index)
            {
                let token = &self.unmapped[unmapped_index];
                tokens.push((
                    ComparableToken::Unmapped {
                        font_hash: token.font_hash.clone(),
                        glyph_id: token.glyph_id,
                    },
                    token.source.clone(),
                ));
                unmapped_index += 1;
            }
            while self
                .source_map
                .get(source_index)
                .is_some_and(|entry| entry.output_range.end <= index)
            {
                source_index += 1;
            }
            let source = self
                .source_map
                .get(source_index)
                .filter(|entry| entry.output_range.start <= index && index < entry.output_range.end)
                .map_or_else(
                    || TextSource { atoms: Vec::new() },
                    |entry| entry.source.clone(),
                );
            tokens.push((ComparableToken::Scalar(scalar), source));
        }

        for token in &self.unmapped[unmapped_index..] {
            tokens.push((
                ComparableToken::Unmapped {
                    font_hash: token.font_hash.clone(),
                    glyph_id: token.glyph_id,
                },
                token.source.clone(),
            ));
        }

        debug_assert_eq!(tokens.len(), scalar_count + self.unmapped.len());
        Ok(tokens)
    }

    fn validate_source_map(&self, scalar_count: usize) -> Result<()> {
        let mut previous_end = 0;
        for entry in &self.source_map {
            if entry.output_range.start > entry.output_range.end
                || entry.output_range.end > scalar_count
                || entry.output_range.start < previous_end
            {
                return Err(Error::Unresolved(
                    "source map ranges must be ordered, non-overlapping scalar offsets within the text"
                        .to_owned(),
                ));
            }
            previous_end = entry.output_range.end;
        }
        Ok(())
    }

    pub(crate) fn comparable_token_count(&self) -> Result<usize> {
        self.validated_token_counts().map(|(_, count)| count)
    }

    fn validated_token_counts(&self) -> Result<(usize, usize)> {
        let scalar_count = self.text.chars().count();
        let mut previous_index = 0;
        for token in &self.unmapped {
            if token.scalar_index > scalar_count || token.scalar_index < previous_index {
                return Err(Error::Unresolved(
                    "unmapped token indices must be ordered scalar offsets within the text"
                        .to_owned(),
                ));
            }
            previous_index = token.scalar_index;
        }
        let token_count =
            scalar_count
                .checked_add(self.unmapped.len())
                .ok_or(Error::LimitExceeded {
                    resource: "mapped text comparable tokens",
                    limit: usize::MAX,
                })?;
        Ok((scalar_count, token_count))
    }

    /// Returns the `TextSource` covering the given output `ScalarRange`.
    pub fn project_source(&self, range: ScalarRange) -> TextSource {
        let mut atoms = Vec::new();
        let mut seen = HashSet::new();

        if range.start == range.end {
            for entry in &self.source_map {
                if entry.output_range.start <= range.start && range.start <= entry.output_range.end
                {
                    for atom in &entry.source.atoms {
                        if seen.insert(atom.clone()) {
                            atoms.push(atom.clone());
                        }
                    }
                }
            }
            for unmapped in &self.unmapped {
                if unmapped.scalar_index == range.start {
                    for atom in &unmapped.source.atoms {
                        if seen.insert(atom.clone()) {
                            atoms.push(atom.clone());
                        }
                    }
                }
            }
            return TextSource { atoms };
        }

        for entry in &self.source_map {
            if entry.output_range.start < range.end && entry.output_range.end > range.start {
                for atom in &entry.source.atoms {
                    if seen.insert(atom.clone()) {
                        atoms.push(atom.clone());
                    }
                }
            }
        }

        for unmapped in &self.unmapped {
            if unmapped.scalar_index >= range.start && unmapped.scalar_index < range.end {
                for atom in &unmapped.source.atoms {
                    if seen.insert(atom.clone()) {
                        atoms.push(atom.clone());
                    }
                }
            }
        }

        TextSource { atoms }
    }

    /// Returns all unique `GlyphId`s associated with the given output `ScalarRange`.
    pub fn project_glyph_ids(&self, range: ScalarRange) -> Vec<GlyphId> {
        let source = self.project_source(range);
        let mut glyph_ids = Vec::new();
        let mut seen = HashSet::new();

        for atom in source.atoms {
            match atom {
                TextSourceAtom::Glyph(glyph_id) => {
                    if seen.insert(glyph_id) {
                        glyph_ids.push(glyph_id);
                    }
                }
                TextSourceAtom::SyntheticSpace {
                    preceding,
                    following,
                }
                | TextSourceAtom::LineBreak {
                    preceding,
                    following,
                } => {
                    if seen.insert(preceding) {
                        glyph_ids.push(preceding);
                    }
                    if seen.insert(following) {
                        glyph_ids.push(following);
                    }
                }
            }
        }
        glyph_ids
    }

    /// Returns the ordered list of `TextSourceAtom`s covering the given output `ScalarRange`.
    pub fn project_source_atoms(&self, range: ScalarRange) -> Vec<TextSourceAtom> {
        self.project_source(range).atoms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NormalizationKind {
    Nfc,
    SoftLineBreak,
    WhitespaceCollapse,
    HyphenationJoin,
    LigatureExpansion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizationEvent {
    pub kind: NormalizationKind,
    pub raw_range: ScalarRange,
    pub canonical_range: ScalarRange,
    pub source: TextSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizationIssueKind {
    AmbiguousLineBreak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizationIssue {
    pub kind: NormalizationIssueKind,
    pub raw_range: ScalarRange,
    pub source: TextSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockText {
    pub block: BlockId,
    pub raw: MappedText,
    pub canonical: MappedText,
    pub matching: String,
    pub matching_tokens: Vec<ComparableToken>,
    pub numeric_mask_applied: bool,
    pub normalization_events: Vec<NormalizationEvent>,
    pub issues: Vec<NormalizationIssue>,
    /// Sorted unique page numbers covered by the block's lines.
    pub pages: Vec<u32>,
    /// Comparable-token offsets immediately before text that starts on a new line.
    ///
    /// `None` means the source evidence could not locate every line boundary. When present, the
    /// offsets are strictly increasing.
    pub line_breaks: Option<Vec<usize>>,
    /// Comparable-token offsets immediately before text that starts on a new page.
    ///
    /// `None` means the source evidence could not locate every page boundary. When present, the
    /// offsets are strictly increasing and contain one entry for each transition in [`Self::pages`].
    pub page_breaks: Option<Vec<usize>>,
}

impl BlockText {
    /// Projects a canonical `ScalarRange` to the corresponding raw `ScalarRange` in `self.raw`.
    pub fn canonical_to_raw_range(&self, canonical_range: ScalarRange) -> ScalarRange {
        if canonical_range.start == canonical_range.end {
            return self.canonical_point_to_raw_offset(canonical_range.start);
        }

        let mut min_raw = usize::MAX;
        let mut max_raw = 0;
        let mut found = false;

        let source = self.canonical.project_source(canonical_range);
        let source_atom_set: HashSet<_> = source.atoms.into_iter().collect();

        for entry in &self.raw.source_map {
            if entry
                .source
                .atoms
                .iter()
                .any(|atom| source_atom_set.contains(atom))
            {
                min_raw = min_raw.min(entry.output_range.start);
                max_raw = max_raw.max(entry.output_range.end);
                found = true;
            }
        }

        for token in &self.raw.unmapped {
            if token
                .source
                .atoms
                .iter()
                .any(|atom| source_atom_set.contains(atom))
            {
                min_raw = min_raw.min(token.scalar_index);
                max_raw = max_raw.max(token.scalar_index);
                found = true;
            }
        }

        for event in &self.normalization_events {
            if event.canonical_range.start < canonical_range.end
                && event.canonical_range.end > canonical_range.start
            {
                min_raw = min_raw.min(event.raw_range.start);
                max_raw = max_raw.max(event.raw_range.end);
                found = true;
            }
            if event.canonical_range.start == event.canonical_range.end
                && canonical_range.start < event.canonical_range.start
                && event.canonical_range.start < canonical_range.end
            {
                min_raw = min_raw.min(event.raw_range.start);
                max_raw = max_raw.max(event.raw_range.end);
                found = true;
            }
        }

        if found {
            ScalarRange {
                start: min_raw,
                end: max_raw,
            }
        } else {
            self.canonical_point_to_raw_offset(canonical_range.start)
        }
    }

    /// Projects a raw `ScalarRange` to the corresponding canonical `ScalarRange` in `self.canonical`.
    pub fn raw_to_canonical_range(&self, raw_range: ScalarRange) -> ScalarRange {
        if raw_range.start == raw_range.end {
            return self.raw_point_to_canonical_offset(raw_range.start);
        }

        let mut min_canonical = usize::MAX;
        let mut max_canonical = 0;
        let mut found = false;

        let source = self.raw.project_source(raw_range);
        let source_atom_set: HashSet<_> = source.atoms.into_iter().collect();

        for entry in &self.canonical.source_map {
            if entry
                .source
                .atoms
                .iter()
                .any(|atom| source_atom_set.contains(atom))
            {
                min_canonical = min_canonical.min(entry.output_range.start);
                max_canonical = max_canonical.max(entry.output_range.end);
                found = true;
            }
        }

        for token in &self.canonical.unmapped {
            if token
                .source
                .atoms
                .iter()
                .any(|atom| source_atom_set.contains(atom))
            {
                min_canonical = min_canonical.min(token.scalar_index);
                max_canonical = max_canonical.max(token.scalar_index);
                found = true;
            }
        }

        for event in &self.normalization_events {
            if event.raw_range.start < raw_range.end && event.raw_range.end > raw_range.start {
                min_canonical = min_canonical.min(event.canonical_range.start);
                max_canonical = max_canonical.max(event.canonical_range.end);
                found = true;
            }
            if event.raw_range.start == event.raw_range.end
                && raw_range.start < event.raw_range.start
                && event.raw_range.start < raw_range.end
            {
                min_canonical = min_canonical.min(event.canonical_range.start);
                max_canonical = max_canonical.max(event.canonical_range.end);
                found = true;
            }
        }

        if found {
            ScalarRange {
                start: min_canonical,
                end: max_canonical,
            }
        } else {
            self.raw_point_to_canonical_offset(raw_range.start)
        }
    }

    /// Projects a canonical `ScalarRange` directly to its source `TextSource`.
    pub fn project_canonical_source(&self, canonical_range: ScalarRange) -> TextSource {
        self.canonical.project_source(canonical_range)
    }

    /// Projects a canonical `ScalarRange` directly to its source `GlyphId`s.
    pub fn project_canonical_glyph_ids(&self, canonical_range: ScalarRange) -> Vec<GlyphId> {
        self.canonical.project_glyph_ids(canonical_range)
    }

    fn canonical_point_to_raw_offset(&self, canonical_offset: usize) -> ScalarRange {
        for entry in &self.canonical.source_map {
            if entry.output_range.start <= canonical_offset
                && canonical_offset <= entry.output_range.end
            {
                let source = &entry.source;
                for raw_entry in &self.raw.source_map {
                    if raw_entry
                        .source
                        .atoms
                        .iter()
                        .any(|atom| source.atoms.contains(atom))
                    {
                        let offset = if canonical_offset == entry.output_range.start {
                            raw_entry.output_range.start
                        } else {
                            raw_entry.output_range.end
                        };
                        return ScalarRange {
                            start: offset,
                            end: offset,
                        };
                    }
                }
            }
        }
        let raw_len = self.raw.text.chars().count();
        let clamped = canonical_offset.min(raw_len);
        ScalarRange {
            start: clamped,
            end: clamped,
        }
    }

    fn raw_point_to_canonical_offset(&self, raw_offset: usize) -> ScalarRange {
        for entry in &self.raw.source_map {
            if entry.output_range.start <= raw_offset && raw_offset <= entry.output_range.end {
                let source = &entry.source;
                for can_entry in &self.canonical.source_map {
                    if can_entry
                        .source
                        .atoms
                        .iter()
                        .any(|atom| source.atoms.contains(atom))
                    {
                        let offset = if raw_offset == entry.output_range.start {
                            can_entry.output_range.start
                        } else {
                            can_entry.output_range.end
                        };
                        return ScalarRange {
                            start: offset,
                            end: offset,
                        };
                    }
                }
            }
        }
        let can_len = self.canonical.text.chars().count();
        let clamped = raw_offset.min(can_len);
        ScalarRange {
            start: clamped,
            end: clamped,
        }
    }
}

pub fn normalize_blocks(
    document: &Document<Glyph>,
    lines: &[Line],
    blocks: &[Block],
) -> Result<Vec<BlockText>> {
    let glyphs = index_glyphs(document)?;
    let lines = index_lines(lines)?;
    let mut block_ids = HashSet::with_capacity(blocks.len());
    let mut assigned_lines = HashSet::new();
    let mut assigned_glyphs = HashSet::new();
    let mut normalized = Vec::with_capacity(blocks.len());

    for block in blocks {
        if !block_ids.insert(block.id) {
            return Err(Error::Unresolved(format!(
                "duplicate block id {}",
                block.id.0
            )));
        }
        if block.lines.is_empty() {
            return Err(Error::Unresolved(format!(
                "block {} contains no lines",
                block.id.0
            )));
        }

        let raw = build_raw_block(
            block,
            &lines,
            &glyphs,
            &mut assigned_lines,
            &mut assigned_glyphs,
        )?;
        let pages = block_pages(block, &lines);
        normalized.push(normalize_block(block.id, raw, pages)?);
    }

    if assigned_lines.len() != lines.len() {
        return Err(Error::Unresolved(format!(
            "normalization assigned {} of {} lines",
            assigned_lines.len(),
            lines.len()
        )));
    }
    if assigned_glyphs.len() != glyphs.len() {
        return Err(Error::Unresolved(format!(
            "normalization assigned {} of {} glyphs",
            assigned_glyphs.len(),
            glyphs.len()
        )));
    }

    Ok(normalized)
}

#[derive(Clone, Debug)]
enum AtomValue {
    Scalar(char),
    Unmapped {
        font_hash: FontProgramHash,
        glyph_id: u16,
    },
    Deleted,
}

#[derive(Clone, Debug)]
struct Atom {
    value: AtomValue,
    raw_range: ScalarRange,
    source: TextSource,
    kinds: BTreeSet<NormalizationKind>,
}

#[derive(Clone, Debug)]
struct RawBlock {
    mapped: MappedText,
    atoms: Vec<Atom>,
    line_break_following_glyphs: Vec<GlyphId>,
    page_break_following_glyphs: Vec<GlyphId>,
}

fn index_lines(lines: &[Line]) -> Result<HashMap<LineId, &Line>> {
    let mut indexed = HashMap::with_capacity(lines.len());
    for line in lines {
        if indexed.insert(line.id, line).is_some() {
            return Err(Error::Unresolved(format!(
                "duplicate line id {}",
                line.id.0
            )));
        }
    }
    Ok(indexed)
}

fn build_raw_block(
    block: &Block,
    lines: &HashMap<LineId, &Line>,
    glyphs: &HashMap<GlyphId, &Glyph>,
    assigned_lines: &mut HashSet<LineId>,
    assigned_glyphs: &mut HashSet<GlyphId>,
) -> Result<RawBlock> {
    let mut builder = RawBuilder::default();
    let mut preceding_line = None;

    for line_id in block.lines.iter().copied() {
        if !assigned_lines.insert(line_id) {
            return Err(Error::Unresolved(format!(
                "line {} is assigned to more than one block",
                line_id.0
            )));
        }
        let line = lines.get(&line_id).ok_or_else(|| {
            Error::Unresolved(format!(
                "block {} references unknown line {}",
                block.id.0, line_id.0
            ))
        })?;
        let (line_start, line_end) = validate_line(line, glyphs)?;

        if let Some((preceding, preceding_page)) = preceding_line {
            builder.push_scalar(
                '\n',
                TextSource::single(TextSourceAtom::LineBreak {
                    preceding,
                    following: line_start,
                }),
            );
            builder.line_break_following_glyphs.push(line_start);
            if preceding_page != line.page {
                builder.page_break_following_glyphs.push(line_start);
            }
        }

        let synthetic_spaces = line
            .synthetic_spaces
            .iter()
            .map(|space| ((space.preceding, space.following), space))
            .collect::<HashMap<_, _>>();

        for (glyph_index, glyph_id) in line.glyphs.iter().copied().enumerate() {
            if !assigned_glyphs.insert(glyph_id) {
                return Err(Error::Unresolved(format!(
                    "glyph {} is assigned to more than one line",
                    glyph_id.0
                )));
            }
            let glyph = glyphs.get(&glyph_id).ok_or_else(|| {
                Error::Unresolved(format!(
                    "line {} references unknown glyph {}",
                    line.id.0, glyph_id.0
                ))
            })?;
            builder.push_glyph(glyph)?;

            let Some(following) = line.glyphs.get(glyph_index + 1).copied() else {
                continue;
            };
            if synthetic_spaces.contains_key(&(glyph_id, following)) {
                builder.push_scalar(
                    ' ',
                    TextSource::single(TextSourceAtom::SyntheticSpace {
                        preceding: glyph_id,
                        following,
                    }),
                );
            }
        }
        preceding_line = Some((line_end, line.page));
    }

    Ok(builder.finish())
}

fn validate_line(line: &Line, glyphs: &HashMap<GlyphId, &Glyph>) -> Result<(GlyphId, GlyphId)> {
    let Some(first_glyph) = line.glyphs.first().copied() else {
        return Err(Error::Unresolved(format!(
            "line {} contains no glyphs",
            line.id.0
        )));
    };
    let last_glyph = line.glyphs.last().copied().unwrap_or(first_glyph);

    let adjacent = line
        .glyphs
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect::<HashSet<_>>();
    let mut spaces = HashSet::with_capacity(line.synthetic_spaces.len());
    for space in &line.synthetic_spaces {
        let pair = (space.preceding, space.following);
        if !adjacent.contains(&pair) {
            return Err(Error::Unresolved(format!(
                "line {} has a synthetic space outside an adjacent glyph pair",
                line.id.0
            )));
        }
        if !spaces.insert(pair) {
            return Err(Error::Unresolved(format!(
                "line {} has a duplicate synthetic space",
                line.id.0
            )));
        }
    }

    for glyph_id in &line.glyphs {
        let glyph = glyphs.get(glyph_id).ok_or_else(|| {
            Error::Unresolved(format!(
                "line {} references unknown glyph {}",
                line.id.0, glyph_id.0
            ))
        })?;
        if glyph.page != line.page {
            return Err(Error::Unresolved(format!(
                "line {} and glyph {} are on different pages",
                line.id.0, glyph.id.0
            )));
        }
    }

    Ok((first_glyph, last_glyph))
}

#[derive(Default)]
struct RawBuilder {
    text: String,
    source_map: Vec<SourceMapEntry>,
    unmapped: Vec<UnmappedToken>,
    atoms: Vec<Atom>,
    scalar_index: usize,
    line_break_following_glyphs: Vec<GlyphId>,
    page_break_following_glyphs: Vec<GlyphId>,
}

impl RawBuilder {
    fn push_glyph(&mut self, glyph: &Glyph) -> Result<()> {
        let source = TextSource::single(TextSourceAtom::Glyph(glyph.id));
        match &glyph.text {
            DecodedText::Mapped(text) => {
                if text.is_empty() {
                    return Err(Error::Unresolved(format!(
                        "glyph {} has an empty Unicode mapping",
                        glyph.id.0
                    )));
                }
                for scalar in text.chars() {
                    self.push_scalar(scalar, source.clone());
                }
            }
            DecodedText::Unmapped {
                font_hash,
                glyph_id,
            } => {
                self.unmapped.push(UnmappedToken {
                    scalar_index: self.scalar_index,
                    font_hash: font_hash.clone(),
                    glyph_id: *glyph_id,
                    source: source.clone(),
                });
                self.atoms.push(Atom {
                    value: AtomValue::Unmapped {
                        font_hash: font_hash.clone(),
                        glyph_id: *glyph_id,
                    },
                    raw_range: ScalarRange {
                        start: self.scalar_index,
                        end: self.scalar_index,
                    },
                    source,
                    kinds: BTreeSet::new(),
                });
            }
        }
        Ok(())
    }

    fn push_scalar(&mut self, scalar: char, source: TextSource) {
        let start = self.scalar_index;
        self.scalar_index += 1;
        self.text.push(scalar);
        self.source_map.push(SourceMapEntry {
            output_range: ScalarRange {
                start,
                end: self.scalar_index,
            },
            source: source.clone(),
        });
        self.atoms.push(Atom {
            value: AtomValue::Scalar(scalar),
            raw_range: ScalarRange {
                start,
                end: self.scalar_index,
            },
            source,
            kinds: BTreeSet::new(),
        });
    }

    fn finish(self) -> RawBlock {
        RawBlock {
            mapped: MappedText {
                text: self.text,
                source_map: merge_source_map(self.source_map),
                unmapped: self.unmapped,
            },
            atoms: self.atoms,
            line_break_following_glyphs: self.line_break_following_glyphs,
            page_break_following_glyphs: self.page_break_following_glyphs,
        }
    }
}

impl TextSource {
    fn single(atom: TextSourceAtom) -> Self {
        Self { atoms: vec![atom] }
    }

    fn combine<'a>(sources: impl IntoIterator<Item = &'a Self>) -> Self {
        let mut seen = HashSet::new();
        let mut atoms = Vec::new();
        for source in sources {
            for atom in &source.atoms {
                if seen.insert(atom.clone()) {
                    atoms.push(atom.clone());
                }
            }
        }
        Self { atoms }
    }
}

fn block_pages(block: &Block, lines: &HashMap<LineId, &Line>) -> Vec<u32> {
    let mut pages = block
        .lines
        .iter()
        .map(|line_id| lines[line_id].page.0)
        .collect::<Vec<_>>();
    pages.sort_unstable();
    pages.dedup();
    pages
}

fn normalize_block(block: BlockId, raw: RawBlock, pages: Vec<u32>) -> Result<BlockText> {
    let mut issues = Vec::new();
    let line_break_following_glyphs = raw.line_break_following_glyphs;
    let page_break_following_glyphs = raw.page_break_following_glyphs;
    let atoms = expand_ligatures(raw.atoms);
    let atoms = resolve_line_breaks(atoms, &mut issues);
    let atoms = collapse_whitespace(atoms);
    let pieces = normalize_nfc(atoms);
    let (canonical, events) = assemble_canonical(pieces);
    let matching = build_matching(&canonical, DEFAULT_MAX_NUMERIC_MASK_RATIO)?;
    let line_breaks = canonical_breaks(&canonical, &line_break_following_glyphs)?;
    let page_breaks = canonical_breaks(&canonical, &page_break_following_glyphs)?;

    Ok(BlockText {
        block,
        raw: raw.mapped,
        canonical,
        matching: matching.text,
        matching_tokens: matching.tokens,
        numeric_mask_applied: matching.numeric_mask_applied,
        normalization_events: events,
        issues,
        pages,
        line_breaks,
        page_breaks,
    })
}

fn canonical_breaks(
    canonical: &MappedText,
    following_glyphs: &[GlyphId],
) -> Result<Option<Vec<usize>>> {
    let tokens = canonical.comparable_tokens_with_sources()?;
    let mut glyph_offsets = HashMap::new();
    for (offset, (_, source)) in tokens.iter().enumerate() {
        for atom in &source.atoms {
            if let TextSourceAtom::Glyph(glyph) = atom {
                glyph_offsets.entry(*glyph).or_insert(offset);
            }
        }
    }
    let mut offsets = Vec::with_capacity(following_glyphs.len());

    for following in following_glyphs {
        let Some(offset) = glyph_offsets.get(following).copied() else {
            return Ok(None);
        };
        if offset == 0 || offsets.last().is_some_and(|previous| *previous >= offset) {
            return Ok(None);
        }
        offsets.push(offset);
    }

    Ok(Some(offsets))
}

struct MatchingText {
    text: String,
    tokens: Vec<ComparableToken>,
    numeric_mask_applied: bool,
}

fn build_matching(canonical: &MappedText, max_numeric_mask_ratio: f64) -> Result<MatchingText> {
    let compatible = compatibility_fold(canonical.comparable_tokens()?);
    let (masked, masked_scalar_count, contains_number) = mask_numbers(&compatible);
    let output_scalar_count = masked.iter().filter(|token| token.is_scalar()).count();
    let numeric_mask_ratio = if output_scalar_count == 0 {
        0.0
    } else {
        masked_scalar_count as f64 / output_scalar_count as f64
    };
    let numeric_mask_applied = contains_number && numeric_mask_ratio <= max_numeric_mask_ratio;
    let tokens = if numeric_mask_applied {
        masked
    } else {
        compatible
    };
    let text = tokens
        .iter()
        .filter_map(ComparableToken::as_scalar)
        .collect();

    Ok(MatchingText {
        text,
        tokens,
        numeric_mask_applied,
    })
}

fn compatibility_fold(tokens: Vec<ComparableToken>) -> Vec<ComparableToken> {
    let mut folded = Vec::with_capacity(tokens.len());
    let mut scalar_run = String::new();

    for token in tokens {
        match token {
            ComparableToken::Scalar(scalar) => scalar_run.push(scalar),
            unmapped @ ComparableToken::Unmapped { .. } => {
                push_compatibility_folded(&mut folded, &mut scalar_run);
                folded.push(unmapped);
            }
        }
    }
    push_compatibility_folded(&mut folded, &mut scalar_run);
    folded
}

fn push_compatibility_folded(output: &mut Vec<ComparableToken>, input: &mut String) {
    output.extend(input.nfkc().map(ComparableToken::Scalar));
    input.clear();
}

fn mask_numbers(tokens: &[ComparableToken]) -> (Vec<ComparableToken>, usize, bool) {
    let mut masked = Vec::with_capacity(tokens.len());
    let mut masked_scalar_count = 0;
    let mut contains_number = false;
    let mut index = 0;

    while index < tokens.len() {
        let Some(ComparableToken::Scalar(first)) = tokens.get(index) else {
            masked.push(tokens[index].clone());
            index += 1;
            continue;
        };
        if !first.is_numeric() {
            masked.push(tokens[index].clone());
            index += 1;
            continue;
        }

        contains_number = true;
        index += 1;
        while index < tokens.len() {
            match tokens.get(index) {
                Some(ComparableToken::Scalar(scalar)) if scalar.is_numeric() => index += 1,
                Some(ComparableToken::Scalar('.' | ','))
                    if tokens.get(index + 1).is_some_and(
                        |token| matches!(token, ComparableToken::Scalar(scalar) if scalar.is_numeric()),
                    ) =>
                {
                    index += 1;
                }
                _ => break,
            }
        }
        masked.extend(NUMBER_MASK.chars().map(ComparableToken::Scalar));
        masked_scalar_count += NUMBER_MASK.chars().count();
    }

    (masked, masked_scalar_count, contains_number)
}

fn expand_ligatures(atoms: Vec<Atom>) -> Vec<Atom> {
    let mut expanded = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let AtomValue::Scalar(scalar) = atom.value else {
            expanded.push(atom);
            continue;
        };
        let Some(replacement) = ligature_expansion(scalar) else {
            expanded.push(Atom {
                value: AtomValue::Scalar(scalar),
                ..atom
            });
            continue;
        };

        for scalar in replacement.chars() {
            let mut kinds = atom.kinds.clone();
            kinds.insert(NormalizationKind::LigatureExpansion);
            expanded.push(Atom {
                value: AtomValue::Scalar(scalar),
                raw_range: atom.raw_range,
                source: atom.source.clone(),
                kinds,
            });
        }
    }
    expanded
}

fn ligature_expansion(scalar: char) -> Option<&'static str> {
    match scalar {
        '\u{fb00}' => Some("ff"),
        '\u{fb01}' => Some("fi"),
        '\u{fb02}' => Some("fl"),
        '\u{fb03}' => Some("ffi"),
        '\u{fb04}' => Some("ffl"),
        '\u{fb05}' | '\u{fb06}' => Some("st"),
        _ => None,
    }
}

fn resolve_line_breaks(atoms: Vec<Atom>, issues: &mut Vec<NormalizationIssue>) -> Vec<Atom> {
    let mut resolved = Vec::with_capacity(atoms.len());
    let mut index = 0;

    while index < atoms.len() {
        if !matches!(atoms[index].value, AtomValue::Scalar('\n')) {
            resolved.push(atoms[index].clone());
            index += 1;
            continue;
        }

        let previous = resolved.last();
        let following = atoms.get(index + 1);
        let previous_scalar = previous.and_then(Atom::scalar);
        let following_scalar = following.and_then(Atom::scalar);

        let latin_prefix_len = latin_prefix_len_before_hyphen(&resolved);
        let latin_suffix_len = latin_suffix_len_after_break(&atoms, index);

        let is_hyphenation = is_hyphen(previous_scalar)
            && latin_prefix_len >= 2
            && latin_suffix_len >= 2
            && following_scalar.is_some_and(is_latin_lowercase);

        let is_lexical_hyphen = is_hyphen(previous_scalar)
            && resolved
                .get(resolved.len().saturating_sub(2))
                .and_then(Atom::scalar)
                .is_some_and(|s| is_latin_letter_or_digit(s) || is_cjk(s))
            && following_scalar.is_some_and(|s| is_latin_letter_or_digit(s) || is_cjk(s));

        if is_hyphenation && let Some(hyphen) = resolved.pop() {
            resolved.push(deleted_atom(
                [&hyphen, &atoms[index]],
                NormalizationKind::HyphenationJoin,
            ));
        } else if is_lexical_hyphen {
            // A lexical hyphen before a line break (e.g. "Franco-\nPrussian", "pre-\n1990",
            // "COVID-\n19", "X-\nray", "1990-\n2000") is retained in place; the trailing
            // line break is deleted as a soft line break so no spurious space is inserted.
            resolved.push(changed_atom(
                &atoms[index],
                AtomValue::Deleted,
                NormalizationKind::SoftLineBreak,
            ));
        } else if previous_scalar.is_some_and(is_decimal_digit)
            && following_scalar.is_some_and(is_decimal_digit)
        {
            // Breaks between digits insert a space to avoid merging numeric values.
            resolved.push(changed_atom(
                &atoms[index],
                AtomValue::Scalar(' '),
                NormalizationKind::SoftLineBreak,
            ));
        } else if previous_scalar.is_some_and(is_horizontal_whitespace)
            || following_scalar.is_some_and(is_horizontal_whitespace)
            // CJK-CJK and CJK-Latin/digit boundaries join without inserted spaces.
            || previous_scalar.is_some_and(is_cjk) && following_scalar.is_some_and(is_cjk)
            || previous_scalar.is_some_and(is_cjk)
                && following_scalar.is_some_and(is_latin_letter_or_digit)
            || previous_scalar.is_some_and(is_latin_letter_or_digit)
                && following_scalar.is_some_and(is_cjk)
        {
            resolved.push(changed_atom(
                &atoms[index],
                AtomValue::Deleted,
                NormalizationKind::SoftLineBreak,
            ));
        } else if previous_scalar.is_some_and(is_latin_letter_or_digit)
            && following_scalar.is_some_and(is_latin_letter_or_digit)
        {
            resolved.push(changed_atom(
                &atoms[index],
                AtomValue::Scalar(' '),
                NormalizationKind::SoftLineBreak,
            ));
        } else {
            issues.push(NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: atoms[index].raw_range,
                source: atoms[index].source.clone(),
            });
            resolved.push(atoms[index].clone());
        }
        index += 1;
    }

    resolved
}

fn collapse_whitespace(atoms: Vec<Atom>) -> Vec<Atom> {
    let mut collapsed = Vec::with_capacity(atoms.len());
    let mut index = 0;

    while index < atoms.len() {
        let Some(first) = atoms.get(index) else {
            break;
        };
        if !first.scalar().is_some_and(is_horizontal_whitespace) {
            collapsed.push(first.clone());
            index += 1;
            continue;
        }

        let start = index;
        while atoms
            .get(index)
            .and_then(Atom::scalar)
            .is_some_and(is_horizontal_whitespace)
        {
            index += 1;
        }
        let run = &atoms[start..index];
        if run.len() == 1 && run[0].scalar() == Some(' ') {
            collapsed.push(run[0].clone());
        } else {
            collapsed.push(Atom {
                value: AtomValue::Scalar(' '),
                raw_range: union_range(run.iter().map(|atom| atom.raw_range)),
                source: TextSource::combine(run.iter().map(|atom| &atom.source)),
                kinds: merged_kinds(run, NormalizationKind::WhitespaceCollapse),
            });
        }
    }

    collapsed
}

#[derive(Clone, Debug)]
enum FinalValue {
    Text(String),
    Unmapped {
        font_hash: FontProgramHash,
        glyph_id: u16,
    },
    Deleted,
}

#[derive(Clone, Debug)]
struct FinalPiece {
    value: FinalValue,
    raw_range: ScalarRange,
    source: TextSource,
    kinds: BTreeSet<NormalizationKind>,
}

fn normalize_nfc(atoms: Vec<Atom>) -> Vec<FinalPiece> {
    let mut pieces = Vec::with_capacity(atoms.len());
    let mut mapped_run = Vec::new();

    for atom in atoms {
        match atom.value {
            AtomValue::Scalar(_) => mapped_run.push(atom),
            AtomValue::Unmapped {
                font_hash,
                glyph_id,
            } => {
                flush_nfc_run(&mut mapped_run, &mut pieces);
                pieces.push(FinalPiece {
                    value: FinalValue::Unmapped {
                        font_hash,
                        glyph_id,
                    },
                    raw_range: atom.raw_range,
                    source: atom.source,
                    kinds: atom.kinds,
                });
            }
            AtomValue::Deleted => {
                flush_nfc_run(&mut mapped_run, &mut pieces);
                pieces.push(FinalPiece {
                    value: FinalValue::Deleted,
                    raw_range: atom.raw_range,
                    source: atom.source,
                    kinds: atom.kinds,
                });
            }
        }
    }
    flush_nfc_run(&mut mapped_run, &mut pieces);
    pieces
}

fn flush_nfc_run(run: &mut Vec<Atom>, pieces: &mut Vec<FinalPiece>) {
    if run.is_empty() {
        return;
    }

    let text = run.iter().filter_map(Atom::scalar).collect::<String>();
    let mut scalar_offset = 0;
    for grapheme in text.graphemes(true) {
        let scalar_count = grapheme.chars().count();
        let atoms = &run[scalar_offset..scalar_offset + scalar_count];
        let normalized = grapheme.nfc().collect::<String>();
        let mut kinds = atoms
            .iter()
            .flat_map(|atom| atom.kinds.iter().copied())
            .collect::<BTreeSet<_>>();
        if normalized != grapheme {
            kinds.insert(NormalizationKind::Nfc);
        }
        pieces.push(FinalPiece {
            value: FinalValue::Text(normalized),
            raw_range: union_range(atoms.iter().map(|atom| atom.raw_range)),
            source: TextSource::combine(atoms.iter().map(|atom| &atom.source)),
            kinds,
        });
        scalar_offset += scalar_count;
    }
    run.clear();
}

fn assemble_canonical(pieces: Vec<FinalPiece>) -> (MappedText, Vec<NormalizationEvent>) {
    let mut text = String::new();
    let mut source_map = Vec::new();
    let mut unmapped = Vec::new();
    let mut events = Vec::new();
    let mut scalar_index = 0;

    for piece in pieces {
        let start = scalar_index;
        match piece.value {
            FinalValue::Text(value) => {
                scalar_index += value.chars().count();
                text.push_str(&value);
                source_map.push(SourceMapEntry {
                    output_range: ScalarRange {
                        start,
                        end: scalar_index,
                    },
                    source: piece.source.clone(),
                });
            }
            FinalValue::Unmapped {
                font_hash,
                glyph_id,
            } => unmapped.push(UnmappedToken {
                scalar_index,
                font_hash,
                glyph_id,
                source: piece.source.clone(),
            }),
            FinalValue::Deleted => {}
        }

        for kind in piece.kinds {
            events.push(NormalizationEvent {
                kind,
                raw_range: piece.raw_range,
                canonical_range: ScalarRange {
                    start,
                    end: scalar_index,
                },
                source: piece.source.clone(),
            });
        }
    }

    (
        MappedText {
            text,
            source_map: merge_source_map(source_map),
            unmapped,
        },
        merge_events(events),
    )
}

impl Atom {
    fn scalar(&self) -> Option<char> {
        match self.value {
            AtomValue::Scalar(scalar) => Some(scalar),
            AtomValue::Unmapped { .. } | AtomValue::Deleted => None,
        }
    }
}

fn changed_atom(atom: &Atom, value: AtomValue, kind: NormalizationKind) -> Atom {
    let mut kinds = atom.kinds.clone();
    kinds.insert(kind);
    Atom {
        value,
        raw_range: atom.raw_range,
        source: atom.source.clone(),
        kinds,
    }
}

fn deleted_atom<const N: usize>(atoms: [&Atom; N], kind: NormalizationKind) -> Atom {
    Atom {
        value: AtomValue::Deleted,
        raw_range: union_range(atoms.iter().map(|atom| atom.raw_range)),
        source: TextSource::combine(atoms.iter().map(|atom| &atom.source)),
        kinds: merged_kinds(atoms, kind),
    }
}

fn merged_kinds<'a>(
    atoms: impl IntoIterator<Item = &'a Atom>,
    kind: NormalizationKind,
) -> BTreeSet<NormalizationKind> {
    let mut kinds = atoms
        .into_iter()
        .flat_map(|atom| atom.kinds.iter().copied())
        .collect::<BTreeSet<_>>();
    kinds.insert(kind);
    kinds
}

fn union_range(ranges: impl IntoIterator<Item = ScalarRange>) -> ScalarRange {
    let mut ranges = ranges.into_iter();
    let Some(first) = ranges.next() else {
        return ScalarRange { start: 0, end: 0 };
    };
    ranges.fold(first, |range, current| ScalarRange {
        start: range.start.min(current.start),
        end: range.end.max(current.end),
    })
}

fn merge_source_map(entries: Vec<SourceMapEntry>) -> Vec<SourceMapEntry> {
    let mut merged = Vec::<SourceMapEntry>::with_capacity(entries.len());
    for entry in entries {
        if let Some(previous) = merged.last_mut()
            && previous.output_range.end == entry.output_range.start
            && previous.source == entry.source
        {
            previous.output_range.end = entry.output_range.end;
        } else {
            merged.push(entry);
        }
    }
    merged
}

fn merge_events(mut events: Vec<NormalizationEvent>) -> Vec<NormalizationEvent> {
    events.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then(left.raw_range.start.cmp(&right.raw_range.start))
            .then(left.canonical_range.start.cmp(&right.canonical_range.start))
    });

    let mut merged = Vec::<NormalizationEvent>::with_capacity(events.len());
    for event in events {
        if let Some(previous) = merged.last_mut()
            && previous.kind == event.kind
            && event.raw_range.start <= previous.raw_range.end
            && event.canonical_range.start <= previous.canonical_range.end
        {
            previous.raw_range.end = previous.raw_range.end.max(event.raw_range.end);
            previous.canonical_range.end =
                previous.canonical_range.end.max(event.canonical_range.end);
            previous.source = TextSource::combine([&previous.source, &event.source]);
        } else {
            merged.push(event);
        }
    }
    merged.sort_by(|left, right| {
        left.canonical_range
            .start
            .cmp(&right.canonical_range.start)
            .then(left.raw_range.start.cmp(&right.raw_range.start))
            .then(left.kind.cmp(&right.kind))
    });
    merged
}

fn is_hyphen(scalar: Option<char>) -> bool {
    matches!(scalar, Some('-' | '\u{2010}'))
}

fn is_horizontal_whitespace(scalar: char) -> bool {
    scalar.is_whitespace() && !matches!(scalar, '\n' | '\r')
}

fn is_latin_letter_or_digit(scalar: char) -> bool {
    scalar.is_numeric()
        || scalar.is_alphabetic()
            && matches!(
                scalar,
                'A'..='Z'
                    | 'a'..='z'
                    | '\u{00c0}'..='\u{02af}'
                    | '\u{1d00}'..='\u{1eff}'
                    | '\u{ab30}'..='\u{ab6f}'
            )
}

/// Decimal digits in ASCII and fullwidth forms. Han numerals such as 五 are
/// excluded on purpose: they behave as CJK words and keep the empty
/// soft-line-break join.
fn is_decimal_digit(scalar: char) -> bool {
    matches!(scalar, '0'..='9' | '\u{ff10}'..='\u{ff19}')
}

fn is_cjk(scalar: char) -> bool {
    matches!(
        scalar,
        '\u{3000}'..='\u{303f}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{31f0}'..='\u{31ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{ac00}'..='\u{d7af}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ff00}'..='\u{ffef}'
            | '\u{20000}'..='\u{2ffff}'
            | '\u{30000}'..='\u{3134f}'
    )
}

fn is_latin_letter(scalar: char) -> bool {
    scalar.is_alphabetic()
        && matches!(
            scalar,
            'A'..='Z'
                | 'a'..='z'
                | '\u{00c0}'..='\u{02af}'
                | '\u{1d00}'..='\u{1eff}'
                | '\u{ab30}'..='\u{ab6f}'
        )
}

fn is_latin_lowercase(scalar: char) -> bool {
    scalar.is_lowercase()
        && matches!(
            scalar,
            'a'..='z'
                | '\u{00df}'..='\u{02af}'
                | '\u{1d00}'..='\u{1eff}'
                | '\u{ab30}'..='\u{ab6f}'
        )
}

fn latin_prefix_len_before_hyphen(resolved: &[Atom]) -> usize {
    if resolved.len() < 2 {
        return 0;
    }
    let mut count = 0;
    for atom in resolved.iter().rev().skip(1) {
        if let Some(scalar) = atom.scalar() {
            if is_latin_letter(scalar) {
                count += 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    count
}

fn latin_suffix_len_after_break(atoms: &[Atom], break_index: usize) -> usize {
    let mut count = 0;
    for atom in &atoms[break_index + 1..] {
        if let Some(scalar) = atom.scalar() {
            if is_latin_letter(scalar) {
                count += 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    count
}
