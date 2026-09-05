use std::collections::HashMap;

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

/// Indexes glyphs by id, rejecting documents with duplicate glyph ids so
/// downstream layout and normalization share one validated lookup.
pub(crate) fn index_glyphs(document: &Document<Glyph>) -> Result<HashMap<GlyphId, &Glyph>> {
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
