use std::collections::{HashMap, HashSet};

use rayon::prelude::*;
use smallvec::SmallVec;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Error, Result,
    layout::{Block, BlockId, BlockRole, Line, LineId},
    model::{
        DecodedText, Document, FontProgramHash, Glyph, GlyphId, GlyphIndex, Vec2, index_glyphs,
        is_cjk,
    },
};

pub const DEFAULT_MAX_NUMERIC_MASK_RATIO: f64 = 0.3;
const NUMBER_MASK: &str = "<NUM>";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScalarRange {
    pub start: usize,
    pub end: usize,
}

/// A canonical insertion boundary projected through all contributing raw sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawBoundary {
    Exact(usize),
    /// The boundary splits a source shared by adjacent canonical scalars.
    WithinSource(ScalarRange),
    /// Deleted or inconsistent evidence leaves multiple possible raw positions.
    Ambiguous(ScalarRange),
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

/// Source atoms of one canonical token.
///
/// The overwhelming majority of sources hold exactly one atom, so the vector
/// is an inline-capacity-1 [`SmallVec`]: single-atom sources (one per
/// character in the hot normalization path) clone and move without a heap
/// allocation. Iteration order and equality semantics match the previous
/// `Vec` representation exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSource {
    pub atoms: SmallVec<[TextSourceAtom; 1]>,
}

/// Exact effective font sizes observed for one canonical comparable token.
///
/// Values are stored as IEEE-754 bit patterns so formatting evidence remains
/// equality-comparable without introducing a layout threshold. A signature is
/// sorted, deduplicated, and never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSizeSignature {
    bits: Vec<u64>,
}

impl FontSizeSignature {
    /// Builds a signature from positive, finite effective font sizes.
    ///
    /// Returns `None` when `sizes` is empty or contains an invalid value.
    pub fn new(sizes: &[f64]) -> Option<Self> {
        if sizes.is_empty() || sizes.iter().any(|size| !size.is_finite() || *size <= 0.0) {
            return None;
        }
        let mut bits = sizes.iter().map(|size| size.to_bits()).collect::<Vec<_>>();
        bits.sort_unstable();
        bits.dedup();
        Some(Self { bits })
    }

    /// Returns the represented effective font sizes in ascending order.
    pub fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.bits.iter().copied().map(f64::from_bits)
    }

    pub(crate) fn union(&self, other: &Self) -> Self {
        let mut bits = Vec::with_capacity(self.bits.len() + other.bits.len());
        bits.extend_from_slice(&self.bits);
        bits.extend_from_slice(&other.bits);
        bits.sort_unstable();
        bits.dedup();
        Self { bits }
    }
}

/// Exact baseline and text direction of a canonical token's first source glyph.
///
/// Coordinates use the normalized page space retained by [`Glyph`]. IEEE-754
/// bit patterns keep the evidence equality-comparable without a pixel threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionSignature {
    baseline_x: u64,
    baseline_y: u64,
    direction_x: u64,
    direction_y: u64,
}

impl PositionSignature {
    /// Builds a signature from finite geometry and a non-zero text direction.
    ///
    /// Returns `None` when any component is non-finite or `direction` is zero.
    pub fn new(baseline: Vec2, direction: Vec2) -> Option<Self> {
        if !baseline.x.is_finite()
            || !baseline.y.is_finite()
            || !direction.x.is_finite()
            || !direction.y.is_finite()
            || direction.x == 0.0 && direction.y == 0.0
        {
            return None;
        }
        Some(Self {
            baseline_x: canonical_f64_bits(baseline.x),
            baseline_y: canonical_f64_bits(baseline.y),
            direction_x: canonical_f64_bits(direction.x),
            direction_y: canonical_f64_bits(direction.y),
        })
    }

    /// Returns the normalized-page baseline represented by this signature.
    pub fn baseline(self) -> Vec2 {
        Vec2 {
            x: f64::from_bits(self.baseline_x),
            y: f64::from_bits(self.baseline_y),
        }
    }

    /// Returns the text direction represented by this signature.
    pub fn direction(self) -> Vec2 {
        Vec2 {
            x: f64::from_bits(self.direction_x),
            y: f64::from_bits(self.direction_y),
        }
    }
}

fn canonical_f64_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0_f64.to_bits()
    } else {
        value.to_bits()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapEntry {
    pub output_range: ScalarRange,
    pub source: TextSource,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
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
                .map_or_else(TextSource::default, |entry| entry.source.clone());
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
            return TextSource {
                atoms: atoms.into(),
            };
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

        TextSource {
            atoms: atoms.into(),
        }
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
        self.project_source(range).atoms.into_vec()
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
    pub role: BlockRole,
    pub raw: MappedText,
    pub canonical: MappedText,
    pub matching: String,
    pub matching_tokens: Vec<ComparableToken>,
    pub numeric_mask_applied: bool,
    pub normalization_events: Vec<NormalizationEvent>,
    pub issues: Vec<NormalizationIssue>,
    /// Sorted unique page numbers covered by the block's lines.
    pub pages: Vec<u32>,
    /// Exact source-glyph font-size signatures aligned with canonical comparable tokens.
    ///
    /// `None` means at least one token lacks complete source evidence. When present, this has
    /// exactly one entry per canonical comparable token.
    pub font_size_signatures: Option<Vec<FontSizeSignature>>,
    /// First-source-glyph positions aligned with canonical comparable tokens.
    ///
    /// `None` means at least one token lacks complete source geometry. When present, this has
    /// exactly one entry per canonical comparable token.
    pub position_signatures: Option<Vec<PositionSignature>>,
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
    /// Projects every normalization issue to a verified canonical range.
    ///
    /// Unlike [`Self::raw_to_canonical_range`], this method has no positional
    /// fallback. Every issue atom must be backed by evidence inside its raw
    /// range and by canonical source-map, unmapped-token, or normalization-event
    /// evidence. Any malformed or ambiguous correspondence fails the complete
    /// projection.
    ///
    /// # Errors
    ///
    /// Returns an error when mapped-text ranges are invalid, issue evidence is
    /// empty or inconsistent, projected evidence is missing, or a projected
    /// range falls outside the canonical text.
    pub(crate) fn checked_normalization_issue_ranges(&self) -> Result<Vec<ScalarRange>> {
        let (raw_scalars, _) = self.raw.validated_token_counts()?;
        let (canonical_scalars, _) = self.canonical.validated_token_counts()?;
        self.raw.validate_source_map(raw_scalars)?;
        self.canonical.validate_source_map(canonical_scalars)?;

        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(self.issues.len())
            .map_err(|_| Error::LimitExceeded {
                resource: "normalization issue projections",
                limit: self.issues.len(),
            })?;
        for issue in &self.issues {
            if issue.raw_range.start >= issue.raw_range.end
                || issue.raw_range.end > raw_scalars
                || issue.source.atoms.is_empty()
                || has_duplicate_source_atoms(&issue.source)
            {
                return Err(invalid_issue_projection());
            }
            if !issue_raw_source_is_exact(self, issue) {
                return Err(invalid_issue_projection());
            }

            let mut start = usize::MAX;
            let mut end = 0usize;
            let mut found = false;
            let mut matched_atoms = Vec::new();
            matched_atoms
                .try_reserve_exact(issue.source.atoms.len())
                .map_err(|_| Error::LimitExceeded {
                    resource: "normalization issue source atoms",
                    limit: issue.source.atoms.len(),
                })?;
            for entry in &self.canonical.source_map {
                if sources_intersect(&entry.source, &issue.source) {
                    include_projection_range(
                        entry.output_range,
                        canonical_scalars,
                        &mut start,
                        &mut end,
                        &mut found,
                    )?;
                    append_matching_atoms(&mut matched_atoms, &entry.source, &issue.source)?;
                }
            }
            for token in &self.canonical.unmapped {
                if sources_intersect(&token.source, &issue.source) {
                    include_projection_range(
                        ScalarRange {
                            start: token.scalar_index,
                            end: token.scalar_index,
                        },
                        canonical_scalars,
                        &mut start,
                        &mut end,
                        &mut found,
                    )?;
                    append_matching_atoms(&mut matched_atoms, &token.source, &issue.source)?;
                }
            }
            for event in &self.normalization_events {
                if !sources_intersect(&event.source, &issue.source) {
                    continue;
                }
                if event.source.atoms.is_empty()
                    || event.raw_range.start > event.raw_range.end
                    || event.raw_range.end > raw_scalars
                    || !scalar_range_contains_or_touches(issue.raw_range, event.raw_range)
                {
                    return Err(invalid_issue_projection());
                }
                include_projection_range(
                    event.canonical_range,
                    canonical_scalars,
                    &mut start,
                    &mut end,
                    &mut found,
                )?;
                append_matching_atoms(&mut matched_atoms, &event.source, &issue.source)?;
            }
            if !found
                || issue
                    .source
                    .atoms
                    .iter()
                    .any(|atom| !matched_atoms.contains(atom))
            {
                return Err(invalid_issue_projection());
            }
            ranges.push(ScalarRange { start, end });
        }
        Ok(ranges)
    }

    /// Projects a canonical scalar range to its complete raw source extent.
    ///
    /// Empty ranges return a point only for an exact boundary; otherwise they
    /// return the containing raw extent. Use [`Self::canonical_to_raw_boundary`]
    /// to distinguish a shared source from an ambiguous boundary.
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
            ScalarRange {
                start: 0,
                end: self.raw.text.chars().count(),
            }
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

    /// Projects an insertion boundary without placing it inside a composed source.
    ///
    /// Returns `None` for an out-of-range offset or missing scalar evidence.
    pub fn canonical_to_raw_boundary(&self, offset: usize) -> Option<RawBoundary> {
        let canonical_len = self.canonical.text.chars().count();
        let raw_len = self.raw.text.chars().count();
        if offset > canonical_len {
            return None;
        }
        let extent = |index| {
            let range = ScalarRange {
                start: index,
                end: index + 1,
            };
            let source = self.canonical.project_source(range);
            if source.atoms.is_empty() {
                return None;
            }
            let mut missing = source.atoms.iter().collect::<HashSet<_>>();
            for entry in &self.raw.source_map {
                for atom in &entry.source.atoms {
                    missing.remove(atom);
                }
            }
            missing
                .is_empty()
                .then(|| self.canonical_to_raw_range(range))
        };
        let left = if offset == 0 {
            0
        } else {
            extent(offset - 1)?.end
        };
        let right = if offset == canonical_len {
            raw_len
        } else {
            extent(offset)?.start
        };
        Some(if left == right {
            RawBoundary::Exact(left)
        } else if left > right {
            RawBoundary::WithinSource(ScalarRange {
                start: right,
                end: left,
            })
        } else {
            RawBoundary::Ambiguous(ScalarRange {
                start: left,
                end: right,
            })
        })
    }

    fn canonical_point_to_raw_offset(&self, canonical_offset: usize) -> ScalarRange {
        match self.canonical_to_raw_boundary(canonical_offset) {
            Some(RawBoundary::Exact(offset)) => ScalarRange {
                start: offset,
                end: offset,
            },
            Some(RawBoundary::WithinSource(range) | RawBoundary::Ambiguous(range)) => range,
            None => ScalarRange {
                start: 0,
                end: self.raw.text.chars().count(),
            },
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

fn invalid_issue_projection() -> Error {
    Error::Unresolved("normalization issue source projection is incomplete".to_owned())
}

fn has_duplicate_source_atoms(source: &TextSource) -> bool {
    source
        .atoms
        .iter()
        .enumerate()
        .any(|(index, atom)| source.atoms[..index].contains(atom))
}

fn scalar_range_contains_or_touches(container: ScalarRange, candidate: ScalarRange) -> bool {
    if container.start == container.end {
        candidate.start <= container.start && container.start <= candidate.end
    } else if candidate.start == candidate.end {
        container.start <= candidate.start && candidate.start < container.end
    } else {
        container.start < candidate.end && candidate.start < container.end
    }
}

fn issue_raw_source_is_exact(block: &BlockText, issue: &NormalizationIssue) -> bool {
    let all_atoms_belong_to_issue = |source: &TextSource| {
        !source.atoms.is_empty()
            && source
                .atoms
                .iter()
                .all(|atom| issue.source.atoms.contains(atom))
    };
    for entry in &block.raw.source_map {
        if scalar_range_contains_or_touches(issue.raw_range, entry.output_range)
            && !all_atoms_belong_to_issue(&entry.source)
        {
            return false;
        }
    }
    for token in &block.raw.unmapped {
        let point = ScalarRange {
            start: token.scalar_index,
            end: token.scalar_index,
        };
        if scalar_range_contains_or_touches(issue.raw_range, point)
            && !all_atoms_belong_to_issue(&token.source)
        {
            return false;
        }
    }
    issue.source.atoms.iter().all(|atom| {
        block.raw.source_map.iter().any(|entry| {
            scalar_range_contains_or_touches(issue.raw_range, entry.output_range)
                && entry.source.atoms.contains(atom)
        }) || block.raw.unmapped.iter().any(|token| {
            scalar_range_contains_or_touches(
                issue.raw_range,
                ScalarRange {
                    start: token.scalar_index,
                    end: token.scalar_index,
                },
            ) && token.source.atoms.contains(atom)
        })
    })
}

fn sources_intersect(left: &TextSource, right: &TextSource) -> bool {
    left.atoms.iter().any(|atom| right.atoms.contains(atom))
}

fn append_matching_atoms(
    output: &mut Vec<TextSourceAtom>,
    evidence: &TextSource,
    issue: &TextSource,
) -> Result<()> {
    for atom in &evidence.atoms {
        if issue.atoms.contains(atom) && !output.contains(atom) {
            output.try_reserve(1).map_err(|_| Error::LimitExceeded {
                resource: "normalization issue source atoms",
                limit: issue.atoms.len(),
            })?;
            output.push(atom.clone());
        }
    }
    Ok(())
}

fn include_projection_range(
    range: ScalarRange,
    canonical_scalars: usize,
    start: &mut usize,
    end: &mut usize,
    found: &mut bool,
) -> Result<()> {
    if range.start > range.end || range.end > canonical_scalars {
        return Err(invalid_issue_projection());
    }
    *start = (*start).min(range.start);
    *end = (*end).max(range.end);
    *found = true;
    Ok(())
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

    // Raw-block building mutates shared duplicate-detection sets, so it stays
    // sequential in block order and stops at the first build error, exactly
    // like the original single-threaded loop. `normalize_block` is pure given
    // its raw block, so the collected raws are normalized in parallel and
    // consumed in block order: the first normalization error (lowest block
    // index) is reported, or the deferred build error when every
    // normalization succeeded — the same error the sequential loop would
    // have reported.
    let mut raws = Vec::with_capacity(blocks.len());
    let mut build_error = None;
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

        match build_raw_block(
            block,
            &lines,
            &glyphs,
            &mut assigned_lines,
            &mut assigned_glyphs,
        ) {
            Ok(raw) => {
                let pages = block_pages(block, &lines);
                raws.push((block.id, block.role, raw, pages));
            }
            Err(error) => {
                build_error = Some(error);
                break;
            }
        }
    }

    let normalized = raws
        .into_par_iter()
        .map(|(block, role, raw, pages)| normalize_block(block, role, raw, pages, &glyphs))
        .collect::<Vec<Result<_>>>();
    let mut normalized_blocks = Vec::with_capacity(normalized.len());
    for result in normalized {
        normalized_blocks.push(result?);
    }
    if let Some(error) = build_error {
        return Err(error);
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

    Ok(normalized_blocks)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NormalizationKinds(u8);

impl NormalizationKinds {
    /// Bit index of a kind equals its `NormalizationKind` discriminant, so
    /// ascending bit iteration reproduces the previous `BTreeSet` order and
    /// `NormalizationEvent` ordering is unchanged.
    fn bit(kind: NormalizationKind) -> u8 {
        1 << (kind as u8)
    }

    fn insert(&mut self, kind: NormalizationKind) {
        self.0 |= Self::bit(kind);
    }

    fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterates the present kinds in ascending discriminant order.
    fn iter(self) -> impl Iterator<Item = NormalizationKind> {
        (0u8..5).filter_map(move |bit| {
            if self.0 & (1 << bit) != 0 {
                Some(match bit {
                    0 => NormalizationKind::Nfc,
                    1 => NormalizationKind::SoftLineBreak,
                    2 => NormalizationKind::WhitespaceCollapse,
                    3 => NormalizationKind::HyphenationJoin,
                    _ => NormalizationKind::LigatureExpansion,
                })
            } else {
                None
            }
        })
    }
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
    kinds: NormalizationKinds,
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
    glyphs: &GlyphIndex<'_>,
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
            let glyph = glyphs.get(glyph_id).ok_or_else(|| {
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

fn validate_line(line: &Line, glyphs: &GlyphIndex<'_>) -> Result<(GlyphId, GlyphId)> {
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
        let glyph = glyphs.get(*glyph_id).ok_or_else(|| {
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
                    kinds: NormalizationKinds::default(),
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
            kinds: NormalizationKinds::default(),
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

impl Default for TextSource {
    fn default() -> Self {
        Self {
            atoms: SmallVec::new(),
        }
    }
}

impl TextSource {
    fn single(atom: TextSourceAtom) -> Self {
        Self {
            atoms: SmallVec::from_buf([atom]),
        }
    }

    fn combine<'a>(sources: impl IntoIterator<Item = &'a Self>) -> Self {
        // Keep tiny merges allocation-free; whitespace runs can contain arbitrarily
        // many distinct atoms, so bound linear scans before switching to a set.
        const LINEAR_LIMIT: usize = 16;
        let mut atoms = SmallVec::new();
        let mut seen = None::<HashSet<TextSourceAtom>>;
        for source in sources {
            for atom in &source.atoms {
                if atoms.len() == LINEAR_LIMIT && seen.is_none() {
                    seen = Some(atoms.iter().cloned().collect());
                }
                let duplicate = match &mut seen {
                    Some(seen) => !seen.insert(atom.clone()),
                    None => atoms.contains(atom),
                };
                if !duplicate {
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

fn normalize_block(
    block: BlockId,
    role: BlockRole,
    raw: RawBlock,
    pages: Vec<u32>,
    glyphs: &GlyphIndex<'_>,
) -> Result<BlockText> {
    let mut issues = Vec::new();
    let line_break_following_glyphs = raw.line_break_following_glyphs;
    let page_break_following_glyphs = raw.page_break_following_glyphs;
    // Only ligature expansion can grow the atom vector, so it owns the one
    // allocation of the pipeline; the line-break and whitespace passes
    // rewrite the buffer in place (their output never exceeds the input),
    // and NFC consumes it.
    let mut atoms = expand_ligatures(raw.atoms);
    resolve_line_breaks(&mut atoms, &mut issues);
    collapse_whitespace(&mut atoms);
    let pieces = normalize_nfc(atoms);
    let (canonical, events) = assemble_canonical(pieces);
    let matching = build_matching(&canonical, DEFAULT_MAX_NUMERIC_MASK_RATIO)?;
    let canonical_tokens = canonical.comparable_tokens_with_sources()?;
    let font_size_signatures = canonical_font_size_signatures(&canonical_tokens, glyphs)?;
    let position_signatures = canonical_position_signatures(&canonical_tokens, glyphs)?;
    let line_breaks = canonical_breaks(&canonical_tokens, &line_break_following_glyphs);
    let page_breaks = canonical_breaks(&canonical_tokens, &page_break_following_glyphs);

    Ok(BlockText {
        block,
        role,
        raw: raw.mapped,
        canonical,
        matching: matching.text,
        matching_tokens: matching.tokens,
        numeric_mask_applied: matching.numeric_mask_applied,
        normalization_events: events,
        issues,
        pages,
        font_size_signatures,
        position_signatures,
        line_breaks,
        page_breaks,
    })
}

fn canonical_font_size_signatures(
    tokens: &[(ComparableToken, TextSource)],
    glyphs: &GlyphIndex<'_>,
) -> Result<Option<Vec<FontSizeSignature>>> {
    let mut signatures = Vec::with_capacity(tokens.len());

    for (_, source) in tokens {
        let mut sizes = Vec::new();
        for atom in &source.atoms {
            match atom {
                TextSourceAtom::Glyph(glyph) => push_font_size(&mut sizes, *glyph, glyphs)?,
                TextSourceAtom::SyntheticSpace {
                    preceding,
                    following,
                }
                | TextSourceAtom::LineBreak {
                    preceding,
                    following,
                } => {
                    push_font_size(&mut sizes, *preceding, glyphs)?;
                    push_font_size(&mut sizes, *following, glyphs)?;
                }
            }
        }
        let Some(signature) = FontSizeSignature::new(&sizes) else {
            return Ok(None);
        };
        signatures.push(signature);
    }

    Ok(Some(signatures))
}

fn canonical_position_signatures(
    tokens: &[(ComparableToken, TextSource)],
    glyphs: &GlyphIndex<'_>,
) -> Result<Option<Vec<PositionSignature>>> {
    let mut signatures = Vec::with_capacity(tokens.len());

    for (_, source) in tokens {
        let Some(glyph_id) = source.atoms.first().map(|atom| match atom {
            TextSourceAtom::Glyph(glyph) => *glyph,
            TextSourceAtom::SyntheticSpace { preceding, .. }
            | TextSourceAtom::LineBreak { preceding, .. } => *preceding,
        }) else {
            return Ok(None);
        };
        let glyph = glyphs.get(glyph_id).ok_or_else(|| {
            Error::Unresolved(format!(
                "canonical text references unknown glyph {}",
                glyph_id.0
            ))
        })?;
        let Some(signature) = PositionSignature::new(glyph.baseline, glyph.direction) else {
            return Err(Error::Unresolved(format!(
                "glyph {} has invalid position evidence",
                glyph_id.0
            )));
        };
        signatures.push(signature);
    }

    Ok(Some(signatures))
}

fn push_font_size(sizes: &mut Vec<f64>, glyph_id: GlyphId, glyphs: &GlyphIndex<'_>) -> Result<()> {
    let glyph = glyphs.get(glyph_id).ok_or_else(|| {
        Error::Unresolved(format!(
            "canonical text references unknown glyph {}",
            glyph_id.0
        ))
    })?;
    if !glyph.font_size.is_finite() || glyph.font_size <= 0.0 {
        return Err(Error::Unresolved(format!(
            "glyph {} has an invalid effective font size",
            glyph_id.0
        )));
    }
    sizes.push(glyph.font_size);
    Ok(())
}

fn canonical_breaks(
    tokens: &[(ComparableToken, TextSource)],
    following_glyphs: &[GlyphId],
) -> Option<Vec<usize>> {
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
        let offset = glyph_offsets.get(following).copied()?;
        if offset == 0 || offsets.last().is_some_and(|previous| *previous >= offset) {
            return None;
        }
        offsets.push(offset);
    }

    Some(offsets)
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

pub(crate) fn character_width_fold(tokens: &[ComparableToken]) -> Option<(String, bool)> {
    let mut folded = String::new();
    let mut changed = false;

    for token in tokens {
        let ComparableToken::Scalar(scalar) = token else {
            return None;
        };
        if *scalar == '\u{3000}' || ('\u{ff00}'..='\u{ffef}').contains(scalar) {
            let original = scalar.to_string();
            let normalized = original.as_str().nfkc().collect::<String>();
            if normalized != original {
                folded.push_str(&normalized);
                changed = true;
                continue;
            }
        }
        folded.push(*scalar);
    }

    Some((folded.nfc().collect(), changed))
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
            let mut kinds = atom.kinds;
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

/// Resolves line-break atoms in place.
///
/// Every input atom yields at most one output atom, so the write cursor never
/// overtakes the read cursor and the output is written back into the same
/// buffer, avoiding a full `Vec<Atom>` rebuild. Slots below the write cursor
/// hold finished output and are never read again; slots at or above the read
/// cursor still hold the untouched input the lookahead consults.
fn resolve_line_breaks(atoms: &mut Vec<Atom>, issues: &mut Vec<NormalizationIssue>) {
    let mut write = 0;
    let mut index = 0;

    while index < atoms.len() {
        if !matches!(atoms[index].value, AtomValue::Scalar('\n')) {
            atoms.swap(write, index);
            write += 1;
            index += 1;
            continue;
        }

        let previous_scalar = (write > 0).then(|| atoms[write - 1].scalar()).flatten();
        let following_scalar = atoms.get(index + 1).and_then(Atom::scalar);

        let latin_prefix_len = latin_prefix_len_before_hyphen(&atoms[..write]);
        let latin_suffix_len = latin_suffix_len_after_break(atoms, index);

        let ambiguous_hyphenation = is_hyphen(previous_scalar)
            && latin_prefix_len >= 2
            && latin_suffix_len >= 2
            && following_scalar.is_some_and(is_latin_lowercase);

        let is_lexical_hyphen = is_hyphen(previous_scalar)
            && (write > 1)
                .then(|| atoms[write - 2].scalar())
                .flatten()
                .is_some_and(|s| is_latin_letter_or_digit(s) || is_cjk(s))
            && following_scalar.is_some_and(|s| is_latin_letter_or_digit(s) || is_cjk(s));

        if previous_scalar == Some('\u{ad}') && following_scalar.is_some() {
            let replacement = deleted_atom(
                [&atoms[write - 1], &atoms[index]],
                NormalizationKind::HyphenationJoin,
            );
            atoms[write - 1] = replacement;
        } else if is_lexical_hyphen {
            if ambiguous_hyphenation {
                issues.push(NormalizationIssue {
                    kind: NormalizationIssueKind::AmbiguousLineBreak,
                    raw_range: atoms[write - 1].raw_range,
                    source: atoms[write - 1].source.clone(),
                });
            }
            // A lexical hyphen before a line break (e.g. "Franco-\nPrussian", "pre-\n1990",
            // "COVID-\n19", "X-\nray", "1990-\n2000") is retained in place; the trailing
            // line break is deleted as a soft line break so no spurious space is inserted.
            let replacement = changed_atom(
                &atoms[index],
                AtomValue::Deleted,
                NormalizationKind::SoftLineBreak,
            );
            atoms[write] = replacement;
            write += 1;
        } else if previous_scalar.is_some_and(is_decimal_digit)
            && following_scalar.is_some_and(is_decimal_digit)
        {
            // Breaks between digits insert a space to avoid merging numeric values.
            let replacement = changed_atom(
                &atoms[index],
                AtomValue::Scalar(' '),
                NormalizationKind::SoftLineBreak,
            );
            atoms[write] = replacement;
            write += 1;
        } else if previous_scalar.is_some_and(is_horizontal_whitespace)
            || following_scalar.is_some_and(is_horizontal_whitespace)
            // CJK-CJK and CJK-Latin/digit boundaries join without inserted spaces.
            || previous_scalar.is_some_and(is_cjk) && following_scalar.is_some_and(is_cjk)
            || previous_scalar.is_some_and(is_cjk)
                && following_scalar.is_some_and(is_latin_letter_or_digit)
            || previous_scalar.is_some_and(is_latin_letter_or_digit)
                && following_scalar.is_some_and(is_cjk)
        {
            let replacement = changed_atom(
                &atoms[index],
                AtomValue::Deleted,
                NormalizationKind::SoftLineBreak,
            );
            atoms[write] = replacement;
            write += 1;
        } else if previous_scalar.is_some_and(is_latin_letter_or_digit)
            && following_scalar.is_some_and(is_latin_letter_or_digit)
        {
            let replacement = changed_atom(
                &atoms[index],
                AtomValue::Scalar(' '),
                NormalizationKind::SoftLineBreak,
            );
            atoms[write] = replacement;
            write += 1;
        } else {
            issues.push(NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: atoms[index].raw_range,
                source: atoms[index].source.clone(),
            });
            atoms.swap(write, index);
            write += 1;
        }
        index += 1;
    }

    atoms.truncate(write);
}

/// Collapses horizontal whitespace runs in place.
///
/// Runs shrink to a single atom, so the write cursor never overtakes the read
/// cursor and the output reuses the input buffer. Read ranges always lie at
/// or above the write cursor, so finished output slots are never re-read.
fn collapse_whitespace(atoms: &mut Vec<Atom>) {
    let mut write = 0;
    let mut index = 0;

    while index < atoms.len() {
        let first_is_ws = atoms[index].scalar().is_some_and(is_horizontal_whitespace);
        if !first_is_ws {
            atoms.swap(write, index);
            write += 1;
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
        let run_len = index - start;
        let single_space = run_len == 1 && atoms[start].scalar() == Some(' ');
        if single_space {
            // Preserve the original atom (including its source and kinds).
            atoms.swap(write, start);
        } else {
            let merged = Atom {
                value: AtomValue::Scalar(' '),
                raw_range: union_range(atoms[start..index].iter().map(|atom| atom.raw_range)),
                source: TextSource::combine(atoms[start..index].iter().map(|atom| &atom.source)),
                kinds: merged_kinds(&atoms[start..index], NormalizationKind::WhitespaceCollapse),
            };
            atoms[write] = merged;
        }
        write += 1;
    }

    atoms.truncate(write);
}

#[derive(Clone, Debug)]
enum FinalValue {
    Text(TextPiece),
    Unmapped {
        font_hash: FontProgramHash,
        glyph_id: u16,
    },
    Deleted,
}

/// Canonical text of one NFC piece.
///
/// Unchanged single-scalar graphemes — the overwhelming majority — carry the
/// scalar inline so the per-character hot path allocates no `String`.
#[derive(Clone, Debug)]
enum TextPiece {
    Char(char),
    Str(String),
}

impl TextPiece {
    fn push_into(self, text: &mut String) -> usize {
        match self {
            Self::Char(scalar) => {
                text.push(scalar);
                1
            }
            Self::Str(value) => {
                let count = value.chars().count();
                text.push_str(&value);
                count
            }
        }
    }
}

#[derive(Clone, Debug)]
struct FinalPiece {
    value: FinalValue,
    raw_range: ScalarRange,
    source: TextSource,
    kinds: NormalizationKinds,
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
        // The overwhelming majority of graphemes are already NFC, often a
        // single scalar: skip the normalization iterator and its per-character
        // String allocation for them. `IsNormalized::Maybe` only means the
        // quick check is inconclusive, so those graphemes still have to be
        // normalized and compared — reporting `Nfc` for them unconditionally
        // would record a normalization that never happened.
        let (value, normalized) = match is_nfc_quick(grapheme.chars()) {
            IsNormalized::Yes => {
                let mut chars = grapheme.chars();
                let value = match (chars.next(), chars.next()) {
                    (Some(single), None) => TextPiece::Char(single),
                    _ => TextPiece::Str(grapheme.to_owned()),
                };
                (value, false)
            }
            _ => {
                let canonical = grapheme.nfc().collect::<String>();
                let normalized = canonical != grapheme;
                (TextPiece::Str(canonical), normalized)
            }
        };
        let mut kinds = atoms
            .iter()
            .fold(NormalizationKinds::default(), |acc, atom| {
                acc.union(atom.kinds)
            });
        if normalized {
            kinds.insert(NormalizationKind::Nfc);
        }
        pieces.push(FinalPiece {
            value: FinalValue::Text(value),
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
        // The piece owns its source, so the consumer can move it instead of
        // cloning; only the rare event-per-piece path needs a copy.
        let mut source = piece.source;
        match piece.value {
            FinalValue::Text(value) => {
                scalar_index += value.push_into(&mut text);
                let entry_source = if piece.kinds.is_empty() {
                    std::mem::take(&mut source)
                } else {
                    source.clone()
                };
                source_map.push(SourceMapEntry {
                    output_range: ScalarRange {
                        start,
                        end: scalar_index,
                    },
                    source: entry_source,
                });
                emit_piece_events(
                    &mut events,
                    piece.kinds,
                    piece.raw_range,
                    start,
                    scalar_index,
                    &mut source,
                );
            }
            FinalValue::Unmapped {
                font_hash,
                glyph_id,
            } => {
                let token_source = if piece.kinds.is_empty() {
                    std::mem::take(&mut source)
                } else {
                    source.clone()
                };
                unmapped.push(UnmappedToken {
                    scalar_index,
                    font_hash,
                    glyph_id,
                    source: token_source,
                });
                emit_piece_events(
                    &mut events,
                    piece.kinds,
                    piece.raw_range,
                    start,
                    scalar_index,
                    &mut source,
                );
            }
            FinalValue::Deleted => {
                emit_piece_events(
                    &mut events,
                    piece.kinds,
                    piece.raw_range,
                    start,
                    scalar_index,
                    &mut source,
                );
            }
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

/// Emits one event per kind, moving the owned source into the last event and
/// cloning it only for the earlier ones.
fn emit_piece_events(
    events: &mut Vec<NormalizationEvent>,
    kinds: NormalizationKinds,
    raw_range: ScalarRange,
    start: usize,
    end: usize,
    source: &mut TextSource,
) {
    let count = kinds.iter().count();
    for (index, kind) in kinds.iter().enumerate() {
        let event_source = if index + 1 == count {
            std::mem::take(source)
        } else {
            source.clone()
        };
        events.push(NormalizationEvent {
            kind,
            raw_range,
            canonical_range: ScalarRange { start, end },
            source: event_source,
        });
    }
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
    let mut kinds = atom.kinds;
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
) -> NormalizationKinds {
    let mut kinds = atoms
        .into_iter()
        .fold(NormalizationKinds::default(), |acc, atom| {
            acc.union(atom.kinds)
        });
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
