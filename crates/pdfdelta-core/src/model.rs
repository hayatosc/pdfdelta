use crate::pdf::ObjectRef;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct GlyphId(pub u64);

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct VectorLineId(pub u64);

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PageId(pub u32);

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct FontId(pub u32);

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct FontProgramHash(pub Vec<u8>);

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DecodedText {
    Mapped(String),
    Unmapped {
        font_hash: FontProgramHash,
        glyph_id: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextRenderMode {
    Fill,
    Stroke,
    FillAndStroke,
    Invisible,
    FillAndClip,
    StrokeAndClip,
    FillStrokeAndClip,
    Clip,
}

/// Geometric relationship between a glyph and the page `CropBox`.
///
/// This records only the page-level crop boundary. It does not claim to
/// resolve path clipping, transparency, or later paint operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlyphCropStatus {
    Inside,
    PartiallyOutside,
    Outside,
}

/// Geometric relationship between a glyph and the supported graphics clip.
///
/// `Unclipped` means neither a path clip nor a Form bounding-box clip was active.
/// The page `CropBox` is recorded independently by [`GlyphCropStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlyphPathClipStatus {
    Unclipped,
    Inside,
    PartiallyOutside,
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GlyphProvenance {
    pub content_stream: ObjectRef,
    pub operator_index: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Glyph {
    pub id: GlyphId,
    pub text: DecodedText,
    pub raw_code: Vec<u8>,
    pub page: PageId,
    pub bbox: Rect,
    pub baseline: Vec2,
    pub direction: Vec2,
    pub font_id: FontId,
    pub font_size: f64,
    pub render_order: u32,
    pub render_mode: TextRenderMode,
    pub crop_status: GlyphCropStatus,
    pub path_clip_status: GlyphPathClipStatus,
    pub provenance: GlyphProvenance,
}

/// One stroked straight path segment retained as layout and render evidence.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorLine {
    pub id: VectorLineId,
    pub page: PageId,
    pub from: Vec2,
    pub to: Vec2,
    pub width: f64,
    pub render_order: u32,
    pub provenance: GlyphProvenance,
}

/// Compact report evidence for one glyph.
///
/// `bbox` is the layout bounding box. `provenance.content_stream` uses the
/// backend-neutral [`ObjectRef`] facade. Glyph ids are expected to be unique
/// within one document side.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GlyphEvidence {
    pub id: GlyphId,
    pub page: PageId,
    pub bbox: Rect,
    pub provenance: GlyphProvenance,
}

impl From<&Glyph> for GlyphEvidence {
    fn from(glyph: &Glyph) -> Self {
        Self {
            id: glyph.id,
            page: glyph.page,
            bbox: glyph.bbox,
            provenance: glyph.provenance,
        }
    }
}

/// Raw per-glyph text-space evidence retained beside the glyphs.
///
/// The values are the exact operands and transforms the interpreter used when
/// it emitted the glyph, not values reconstructed from the rounded advance or
/// from baseline differences. A glyph without an entry has no evidence, and a
/// run boundary means the text-space displacement is not continuous across it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GlyphDisplacement {
    pub glyph: GlyphId,
    pub page: PageId,
    pub raw_code: Vec<u8>,
    pub width_1000_em: f64,
    pub font_size: f64,
    pub character_spacing: f64,
    /// The applied word spacing: the raw state value for a space glyph and
    /// zero for every other glyph.
    pub word_spacing_applied: f64,
    /// The raw `Tz` percent operand, 100 by default. The computed scale the
    /// interpreter used is `horizontal_scale`.
    pub horizontal_scale_percent: f64,
    /// The computed horizontal scale, `horizontal_scale_percent / 100`.
    pub horizontal_scale: f64,
    pub rise: f64,
    pub horizontal: bool,
    /// Continuous text-space run; a reset, a coordinate change or an
    /// unsupported path starts a new run.
    pub run: u32,
    /// Raw text matrix, CTM and page transform at emission, before the advance.
    pub text_matrix: [f64; 6],
    pub ctm: [f64; 6],
    pub page_transform: [f64; 6],
}

/// A marked-content sequence in page content or one Form `XObject` invocation.
/// The half-open range indexes the document's primary glyph items. Repeated
/// invocations remain separate records; an MCID alone is not a unique identity.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarkedContent {
    pub page: PageId,
    pub form: Option<ObjectRef>,
    pub mcid: u32,
    pub glyph_range: std::ops::Range<usize>,
    /// False for unterminated or nesting-limited sequences. Their glyphs survive.
    pub complete: bool,
}

/// An opaque or non-text painting operation with a conservative bound in native page
/// coordinates. An unknown bound remains an obstruction to local text closure;
/// neither a known bound nor its absence identifies the painted content.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NonTextPaint {
    pub page: PageId,
    /// The next native render-order index at this operation, before later glyphs.
    pub render_order: u32,
    pub bounds: Option<Rect>,
    pub content_stream: ObjectRef,
    pub operator_index: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Document<T> {
    items: Vec<T>,
    vector_lines: Vec<VectorLine>,
    #[serde(default)]
    marked_content: Vec<MarkedContent>,
    #[serde(default)]
    displacements: Vec<GlyphDisplacement>,
    #[serde(default)]
    last_non_text_paint: std::collections::BTreeMap<PageId, u32>,
    /// None denotes an older or incomplete paint-bound inventory, not no paint.
    #[serde(default)]
    non_text_paint_bounds: Option<Vec<NonTextPaint>>,
}

impl<T> Document<T> {
    #[must_use]
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            vector_lines: Vec::new(),
            marked_content: Vec::new(),
            displacements: Vec::new(),
            last_non_text_paint: std::collections::BTreeMap::new(),
            non_text_paint_bounds: None,
        }
    }

    /// Creates a document with neutral straight-path evidence retained beside
    /// its primary items.
    #[must_use]
    pub fn with_vector_lines(items: Vec<T>, vector_lines: Vec<VectorLine>) -> Self {
        Self {
            items,
            vector_lines,
            marked_content: Vec::new(),
            displacements: Vec::new(),
            last_non_text_paint: std::collections::BTreeMap::new(),
            non_text_paint_bounds: None,
        }
    }

    /// Attaches reversible source memberships without changing primary items.
    /// Consumers must validate ranges before dereferencing untrusted metadata.
    #[must_use]
    pub fn with_marked_content(mut self, marked_content: Vec<MarkedContent>) -> Self {
        self.marked_content = marked_content;
        self
    }

    #[must_use]
    pub fn marked_content(&self) -> &[MarkedContent] {
        &self.marked_content
    }

    /// Attaches the raw per-glyph displacement evidence. Documents without it
    /// carry no evidence and never satisfy an exact-displacement proof.
    #[must_use]
    pub fn with_displacements(mut self, displacements: Vec<GlyphDisplacement>) -> Self {
        self.displacements = displacements;
        self
    }

    #[must_use]
    pub fn displacements(&self) -> &[GlyphDisplacement] {
        &self.displacements
    }

    /// or paths. This boundary does not prove recognition or actual visibility.
    #[must_use]
    pub fn with_last_non_text_paint(
        mut self,
        pages: std::collections::BTreeMap<PageId, u32>,
    ) -> Self {
        self.last_non_text_paint = pages;
        self
    }

    #[must_use]
    pub fn last_non_text_paint(&self) -> &std::collections::BTreeMap<PageId, u32> {
        &self.last_non_text_paint
    }

    /// Attaches an exhaustive inventory of encountered painting operations.
    /// Acquisition issues still invalidate affected pages; each unbounded paint
    /// stays explicit. This does not make the document's text inventory complete.
    #[must_use]
    pub fn with_non_text_paint_bounds(mut self, paints: Vec<NonTextPaint>) -> Self {
        self.last_non_text_paint.clear();
        for paint in &paints {
            self.last_non_text_paint
                .entry(paint.page)
                .and_modify(|order| *order = (*order).max(paint.render_order))
                .or_insert(paint.render_order);
        }
        self.non_text_paint_bounds = Some(paints);
        self
    }

    #[must_use]
    pub fn non_text_paint_bounds(&self) -> Option<&[NonTextPaint]> {
        self.non_text_paint_bounds.as_deref()
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Transforms primary items while retaining the auxiliary source evidence.
    ///
    /// The displacement sidecar is dropped: a mapping may change glyph
    /// identity or raw codes, so the retained records could no longer describe
    /// the items. Re-attach a validated sidecar explicitly when the items are
    /// unchanged.
    pub fn map_items<U>(self, map: impl FnMut(T) -> U) -> Document<U> {
        Document {
            items: self.items.into_iter().map(map).collect(),
            vector_lines: self.vector_lines,
            marked_content: self.marked_content,
            displacements: Vec::new(),
            last_non_text_paint: self.last_non_text_paint,
            non_text_paint_bounds: self.non_text_paint_bounds,
        }
    }

    #[must_use]
    pub fn vector_lines(&self) -> &[VectorLine] {
        &self.vector_lines
    }

    /// Returns only the primary items, discarding all auxiliary evidence.
    ///
    /// Keep the document when all evidence must survive ownership transfer.
    #[must_use]
    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    /// Returns primary items and vector lines, discarding other acquisition metadata.
    #[must_use]
    pub fn into_parts(self) -> (Vec<T>, Vec<VectorLine>) {
        (self.items, self.vector_lines)
    }
}

impl Document<Glyph> {
    /// Rebuilds the displacement sidecar for a filtered glyph sequence.
    ///
    /// `keep` indexes the primary items, not the sidecar. Every kept glyph is
    /// matched to its record by [`GlyphId`] and the record must agree with the
    /// real glyph's page and raw code; a missing, duplicated or disagreeing
    /// record leaves that glyph without evidence. Runs are renumbered so two
    /// kept glyphs stay in one run only when they are adjacent items with the
    /// same original run; a dropped glyph breaks the continuity. The run
    /// address space is checked, so the renumbering never wraps or collides.
    #[must_use]
    pub fn filtered_displacements(&self, keep: &[bool]) -> Vec<GlyphDisplacement> {
        let mut by_glyph = std::collections::HashMap::new();
        let mut duplicated = std::collections::HashSet::new();
        for entry in &self.displacements {
            if by_glyph.insert(entry.glyph, entry).is_some() {
                duplicated.insert(entry.glyph);
            }
        }
        let mut result = Vec::new();
        let mut previous: Option<(usize, u32, u32)> = None;
        let mut next_run = 1u32;
        for (index, glyph) in self.items.iter().enumerate() {
            if !keep.get(index).copied().unwrap_or(false) {
                continue;
            }
            let Some(entry) = by_glyph.get(&glyph.id) else {
                continue;
            };
            if duplicated.contains(&glyph.id)
                || entry.page != glyph.page
                || entry.raw_code != glyph.raw_code
            {
                continue;
            }
            let run = match previous {
                Some((previous_index, previous_run, new_run))
                    if previous_index + 1 == index && previous_run == entry.run =>
                {
                    new_run
                }
                _ => {
                    let Some(next) = next_run.checked_add(1) else {
                        break;
                    };
                    next_run = next;
                    next_run
                }
            };
            let original_run = entry.run;
            let mut entry = (*entry).clone();
            entry.run = run;
            result.push(entry);
            previous = Some((index, original_run, run));
        }
        result
    }
}

/// Lookup structure for glyphs indexed by id.
///
/// Extraction assigns every glyph the sequential id equal to its position,
/// so the common case uses a direct-index vector with O(1) lookups and no
/// hashing. Programmatic `Document` fixtures may use sparse or out-of-range
/// ids; those fall back to an id-sorted vector with binary search. Both
/// representations contain every document glyph exactly once, so `len` is
/// the document's glyph count.
pub(crate) enum GlyphIndex<'a> {
    Direct(Vec<Option<&'a Glyph>>),
    Sorted(Vec<(GlyphId, &'a Glyph)>),
}

impl GlyphIndex<'_> {
    pub(crate) fn get(&self, id: GlyphId) -> Option<&Glyph> {
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

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Direct(glyphs) => glyphs.len(),
            Self::Sorted(entries) => entries.len(),
        }
    }
}

/// Indexes glyphs by id, rejecting documents with duplicate glyph ids so
/// downstream layout and normalization share one validated lookup.
///
/// The duplicate check preserves the sequential document-order scan's error:
/// the reported id is the one whose second occurrence has the smallest
/// document index.
pub(crate) fn index_glyphs(document: &Document<Glyph>) -> Result<GlyphIndex<'_>> {
    let items = document.items();
    let len = items.len() as u64;
    // Ids below the item count with pairwise distinct slots leave no holes,
    // but duplicates must still be rejected here — in document order, exactly
    // like the sequential scan this replaces.
    if items.iter().all(|glyph| glyph.id.0 < len) {
        let mut direct = Vec::with_capacity(items.len());
        direct.resize(items.len(), None);
        for glyph in items {
            let slot = usize::try_from(glyph.id.0).expect("validated against len above");
            if direct[slot].is_some() {
                return Err(Error::Unresolved(format!(
                    "duplicate glyph id {}",
                    glyph.id.0
                )));
            }
            direct[slot] = Some(glyph);
        }
        return Ok(GlyphIndex::Direct(direct));
    }

    let mut entries = items
        .iter()
        .enumerate()
        .map(|(document_index, glyph)| (glyph.id, document_index, glyph))
        .collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(id, document_index, _)| (*id, *document_index));
    let mut first_duplicate: Option<(usize, GlyphId)> = None;
    let mut group_start = 0;
    for index in 1..=entries.len() {
        let ends_group = index == entries.len() || entries[index].0 != entries[group_start].0;
        if !ends_group {
            continue;
        }
        if index - group_start > 1 {
            // Sorted by document index within the id group, so the second
            // element is that id's earliest duplicated occurrence.
            let duplicate = (entries[group_start + 1].1, entries[group_start].0);
            let replace = match first_duplicate {
                Some((existing_second_index, _)) => duplicate.0 < existing_second_index,
                None => true,
            };
            if replace {
                first_duplicate = Some(duplicate);
            }
        }
        group_start = index;
    }
    if let Some((_, id)) = first_duplicate {
        return Err(Error::Unresolved(format!("duplicate glyph id {}", id.0)));
    }
    Ok(GlyphIndex::Sorted(
        entries
            .into_iter()
            .map(|(id, _, glyph)| (id, glyph))
            .collect(),
    ))
}

pub(crate) fn is_cjk(scalar: char) -> bool {
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
