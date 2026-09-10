//! Private worker transport. Positional glyphs avoid repeating field names for
//! every source atom while preserving the same response-byte ceiling.

use pdfdelta_core::{
    document::{
        BackendIdentity, ChannelInventory, EvidenceIssue, EvidenceStore, KeyInventory,
        PageEvidence, RenderedEvidence, StructuredEvidence,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
};
use serde::{Deserialize, Serialize};

// A version change is required when reordering positional fields. This format
// is internal to one executable; public reports retain their named fields.
const VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
pub(super) struct Acquisition {
    version: u8,
    store: Store,
    page_refs: Vec<ObjectRef>,
}

#[derive(Serialize, Deserialize)]
struct Store {
    revision: String,
    backends: Vec<BackendIdentity>,
    pages: Vec<PageEvidence>,
    native: Document<CompactGlyph>,
    rendered: Vec<RenderedEvidence>,
    structured: Vec<StructuredEvidence>,
    inventories: Vec<ChannelInventory>,
    key_inventories: Vec<KeyInventory>,
    issues: Vec<EvidenceIssue>,
}

#[derive(Serialize, Deserialize)]
struct CompactGlyph(
    GlyphId,
    DecodedText,
    Vec<u8>,
    PageId,
    [f64; 4],
    [f64; 2],
    [f64; 2],
    FontId,
    f64,
    u32,
    TextRenderMode,
    GlyphCropStatus,
    GlyphPathClipStatus,
    (u32, u16, u32),
);

impl From<Glyph> for CompactGlyph {
    fn from(glyph: Glyph) -> Self {
        let Glyph {
            id,
            text,
            raw_code,
            page,
            bbox,
            baseline,
            direction,
            font_id,
            font_size,
            render_order,
            render_mode,
            crop_status,
            path_clip_status,
            provenance,
        } = glyph;
        Self(
            id,
            text,
            raw_code,
            page,
            [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y],
            [baseline.x, baseline.y],
            [direction.x, direction.y],
            font_id,
            font_size,
            render_order,
            render_mode,
            crop_status,
            path_clip_status,
            (
                provenance.content_stream.object_number,
                provenance.content_stream.generation,
                provenance.operator_index,
            ),
        )
    }
}

impl From<CompactGlyph> for Glyph {
    fn from(glyph: CompactGlyph) -> Self {
        let CompactGlyph(
            id,
            text,
            raw_code,
            page,
            bbox,
            baseline,
            direction,
            font_id,
            font_size,
            render_order,
            render_mode,
            crop_status,
            path_clip_status,
            (object_number, generation, operator_index),
        ) = glyph;
        Self {
            id,
            text,
            raw_code,
            page,
            bbox: Rect {
                min: Vec2 {
                    x: bbox[0],
                    y: bbox[1],
                },
                max: Vec2 {
                    x: bbox[2],
                    y: bbox[3],
                },
            },
            baseline: Vec2 {
                x: baseline[0],
                y: baseline[1],
            },
            direction: Vec2 {
                x: direction[0],
                y: direction[1],
            },
            font_id,
            font_size,
            render_order,
            render_mode,
            crop_status,
            path_clip_status,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number,
                    generation,
                },
                operator_index,
            },
        }
    }
}

impl From<super::Acquisition> for Acquisition {
    fn from(value: super::Acquisition) -> Self {
        let EvidenceStore {
            revision,
            backends,
            pages,
            native,
            rendered,
            structured,
            inventories,
            key_inventories,
            issues,
        } = value.store;
        Self {
            version: VERSION,
            store: Store {
                revision,
                backends,
                pages,
                native: native.map_items(CompactGlyph::from),
                rendered,
                structured,
                inventories,
                key_inventories,
                issues,
            },
            page_refs: value.page_refs,
        }
    }
}

impl Acquisition {
    pub(super) fn decode(self) -> Result<super::Acquisition, super::Failure> {
        if self.version != VERSION {
            return Err(super::Failure::backend(
                "unsupported native response version",
            ));
        }
        let Store {
            revision,
            backends,
            pages,
            native,
            rendered,
            structured,
            inventories,
            key_inventories,
            issues,
        } = self.store;
        Ok(super::Acquisition {
            store: EvidenceStore {
                revision,
                backends,
                pages,
                native: native.map_items(Glyph::from),
                rendered,
                structured,
                inventories,
                key_inventories,
                issues,
            },
            page_refs: self.page_refs,
        })
    }
}
