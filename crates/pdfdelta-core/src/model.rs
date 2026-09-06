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

/// Geometric relationship between a glyph and the page CropBox.
///
/// This records only the page-level crop boundary. It does not claim to
/// resolve path clipping, transparency, or later paint operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlyphCropStatus {
    Inside,
    PartiallyOutside,
    Outside,
}

/// Geometric relationship between a glyph and a supported explicit path clip.
///
/// `Unclipped` means no explicit path clip was active. The page CropBox is
/// recorded independently by [`GlyphCropStatus`].
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Document<T> {
    items: Vec<T>,
    vector_lines: Vec<VectorLine>,
}

impl<T> Document<T> {
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            vector_lines: Vec::new(),
        }
    }

    /// Creates a document with neutral straight-path evidence retained beside
    /// its primary items.
    pub fn with_vector_lines(items: Vec<T>, vector_lines: Vec<VectorLine>) -> Self {
        Self {
            items,
            vector_lines,
        }
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn vector_lines(&self) -> &[VectorLine] {
        &self.vector_lines
    }

    /// Returns only the primary items, discarding vector-line evidence.
    ///
    /// Use [`Document::into_parts`] when the evidence must survive ownership
    /// transfer.
    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    pub fn into_parts(self) -> (Vec<T>, Vec<VectorLine>) {
        (self.items, self.vector_lines)
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
