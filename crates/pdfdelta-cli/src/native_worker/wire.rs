//! Private worker transport. Positional glyphs share identical consecutive
//! source context while preserving every atom and the response-byte ceiling.

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
const VERSION: u8 = 2;

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
    [f64; 2],
    f64,
    u32,
    u32,
    Option<GlyphContext>,
);

// Bit patterns distinguish signed zero and preserve coordinates without
// quantization. Context is reused only when every field is identical.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct GlyphContext(
    PageId,
    [u64; 6],
    FontId,
    TextRenderMode,
    GlyphCropStatus,
    GlyphPathClipStatus,
    (u32, u16),
);

impl CompactGlyph {
    fn encode(glyph: Glyph, previous: &mut Option<GlyphContext>) -> Self {
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
        let context = GlyphContext(
            page,
            [
                bbox.min.y,
                bbox.max.y,
                baseline.y,
                direction.x,
                direction.y,
                font_size,
            ]
            .map(f64::to_bits),
            font_id,
            render_mode,
            crop_status,
            path_clip_status,
            (
                provenance.content_stream.object_number,
                provenance.content_stream.generation,
            ),
        );
        let context = if previous.as_ref() == Some(&context) {
            None
        } else {
            *previous = Some(context);
            Some(context)
        };
        Self(
            id,
            text,
            raw_code,
            [bbox.min.x, bbox.max.x],
            baseline.x,
            render_order,
            provenance.operator_index,
            context,
        )
    }

    fn decode(self, previous: &mut Option<GlyphContext>) -> Glyph {
        let CompactGlyph(id, text, raw_code, bbox, baseline, render_order, operator_index, context) =
            self;
        if let Some(context) = context {
            *previous = Some(context);
        }
        let GlyphContext(
            page,
            coordinates,
            font_id,
            render_mode,
            crop_status,
            path_clip_status,
            (object_number, generation),
        ) = previous.expect("the first glyph context was validated before decoding");
        let [
            min_y,
            max_y,
            baseline_y,
            direction_x,
            direction_y,
            font_size,
        ] = coordinates.map(f64::from_bits);
        Glyph {
            id,
            text,
            raw_code,
            page,
            bbox: Rect {
                min: Vec2 {
                    x: bbox[0],
                    y: min_y,
                },
                max: Vec2 {
                    x: bbox[1],
                    y: max_y,
                },
            },
            baseline: Vec2 {
                x: baseline,
                y: baseline_y,
            },
            direction: Vec2 {
                x: direction_x,
                y: direction_y,
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
        let mut context = None;
        Self {
            version: VERSION,
            store: Store {
                revision,
                backends,
                pages,
                native: native.map_items(|glyph| CompactGlyph::encode(glyph, &mut context)),
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
        if native
            .items()
            .first()
            .is_some_and(|glyph| glyph.7.is_none())
        {
            return Err(super::Failure::backend(
                "native response starts with an absent glyph context",
            ));
        }
        let mut context = None;
        Ok(super::Acquisition {
            store: EvidenceStore {
                revision,
                backends,
                pages,
                native: native.map_items(|glyph| glyph.decode(&mut context)),
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
