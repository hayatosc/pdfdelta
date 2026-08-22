use std::collections::{BTreeSet, HashMap, HashSet};

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Error, Result,
    layout::{Block, BlockId, Line, LineId},
    model::{DecodedText, Document, FontProgramHash, Glyph, GlyphId},
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
        normalized.push(normalize_block(block.id, raw)?);
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
}

fn index_glyphs(document: &Document<Glyph>) -> Result<HashMap<GlyphId, &Glyph>> {
    let mut glyphs = HashMap::with_capacity(document.items().len());
    for glyph in document.items() {
        if glyphs.insert(glyph.id, glyph).is_some() {
            return Err(Error::Unresolved(format!(
                "duplicate glyph id {}",
                glyph.id.0
            )));
        }
    }
    Ok(glyphs)
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
    let mut preceding_line_end = None;

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

        if let Some(preceding) = preceding_line_end {
            builder.push_scalar(
                '\n',
                TextSource::single(TextSourceAtom::LineBreak {
                    preceding,
                    following: line_start,
                }),
            );
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
        preceding_line_end = Some(line_end);
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

fn normalize_block(block: BlockId, raw: RawBlock) -> Result<BlockText> {
    let mut issues = Vec::new();
    let atoms = expand_ligatures(raw.atoms);
    let atoms = resolve_line_breaks(atoms, &mut issues);
    let atoms = collapse_whitespace(atoms);
    let pieces = normalize_nfc(atoms);
    let (canonical, events) = assemble_canonical(pieces);
    let matching = build_matching(&canonical, DEFAULT_MAX_NUMERIC_MASK_RATIO)?;

    Ok(BlockText {
        block,
        raw: raw.mapped,
        canonical,
        matching: matching.text,
        matching_tokens: matching.tokens,
        numeric_mask_applied: matching.numeric_mask_applied,
        normalization_events: events,
        issues,
    })
}

struct MatchingText {
    text: String,
    tokens: Vec<ComparableToken>,
    numeric_mask_applied: bool,
}

fn build_matching(canonical: &MappedText, max_numeric_mask_ratio: f64) -> Result<MatchingText> {
    let compatible = compatibility_fold(canonical.comparable_tokens()?);
    let (masked, masked_scalar_count, contains_number) = mask_numbers(&compatible);
    let output_scalar_count = masked
        .iter()
        .filter(|token| matches!(token, ComparableToken::Scalar(_)))
        .count();
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
        .filter_map(|token| match token {
            ComparableToken::Scalar(scalar) => Some(*scalar),
            ComparableToken::Unmapped { .. } => None,
        })
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

        let is_hyphenation = is_hyphen(previous_scalar)
            && resolved
                .get(resolved.len().saturating_sub(2))
                .and_then(Atom::scalar)
                .is_some_and(is_latin_letter_or_digit)
            && following_scalar.is_some_and(is_latin_letter_or_digit);
        if is_hyphenation && let Some(hyphen) = resolved.pop() {
            resolved.push(deleted_atom(
                [&hyphen, &atoms[index]],
                NormalizationKind::HyphenationJoin,
            ));
        } else if previous_scalar.is_some_and(is_decimal_digit)
            && following_scalar.is_some_and(is_decimal_digit)
        {
            // A break inside a number sequence must never silently merge the
            // numerals into one value; mirror the ASCII policy and insert a
            // canonical space instead (recorded as an auditable event).
            resolved.push(changed_atom(
                &atoms[index],
                AtomValue::Scalar(' '),
                NormalizationKind::SoftLineBreak,
            ));
        } else if previous_scalar.is_some_and(is_horizontal_whitespace)
            || following_scalar.is_some_and(is_horizontal_whitespace)
            // deliberate: CJK classification keeps precedence over numeric
            // classification whenever both sides are not decimal digits, so
            // kanji-to-fullwidth-digit boundaries still join without a space.
            || previous_scalar.is_some_and(is_cjk) && following_scalar.is_some_and(is_cjk)
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
