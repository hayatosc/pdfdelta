//! Private worker transport. Positional glyphs share identical consecutive
//! source context while preserving every atom and the response-byte ceiling.

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
};

use super::{Failure, MAX_RESPONSE};
use crate::extraction_cache::CeilingWriter;
use flate2::{Compression, bufread::GzDecoder, write::GzEncoder};
use serde::de::{DeserializeSeed, SeqAccess, Visitor};

use pdfdelta_core::{
    document::{
        BackendIdentity, Channel, ChannelInventory, EvidenceIssue, EvidenceLimits, EvidenceStore,
        KeyInventory, NativeStructureInventory, PageEvidence, RenderedEvidence, SourceRef,
        StructuredEvidence,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, MarkedContent, NonTextPaint, PageId, Rect, TextRenderMode, Vec2,
        VectorLine,
    },
    pdf::ObjectRef,
};
use serde::{Deserialize, Serialize};

// A version change is required when reordering positional fields. This format
// is internal to one executable; public reports retain their named fields.
const VERSION: u8 = 8;
const MAX_ATOMS: usize = 4096;

type GlyphAtom = (DecodedText, Vec<u8>);

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Atom {
    Shared(u16),
    Inline(GlyphAtom),
}

#[derive(Default)]
struct Atoms(HashMap<GlyphAtom, u16>);

impl Atoms {
    fn encode(&mut self, atom: GlyphAtom) -> Atom {
        if let Some(&index) = self.0.get(&atom) {
            return Atom::Shared(index);
        }
        if self.0.len() == MAX_ATOMS {
            return Atom::Inline(atom);
        }
        let index = self.0.len() as u16;
        self.0.insert(atom, index);
        Atom::Shared(index)
    }

    fn finish(self) -> Vec<GlyphAtom> {
        let mut indexed: Vec<_> = self.0.into_iter().collect();
        indexed.sort_unstable_by_key(|(_, index)| *index);
        indexed.into_iter().map(|(atom, _)| atom).collect()
    }
}

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
    atoms: Vec<GlyphAtom>,
    native: NativeDocument,
    rendered: Vec<RenderedEvidence>,
    structured: Vec<StructuredEvidence>,
    inventories: Vec<Inventory>,
    key_inventories: Vec<KeyInventory>,
    native_structures: Vec<NativeStructureInventory>,
    issues: Vec<EvidenceIssue>,
}

#[derive(Serialize, Deserialize)]
struct NativeDocument {
    items: GlyphBlock,
    vector_lines: Vec<VectorLine>,
    marked_content: Vec<MarkedContent>,
    last_non_text_paint: BTreeMap<PageId, u32>,
    non_text_paint_bounds: Option<Vec<Paint>>,
}

// Both the transported response and each inflated glyph block retain the same
// byte ceiling. Declared lengths never authorize allocation or unchecked expansion.
#[derive(Serialize, Deserialize)]
struct GlyphBlock {
    #[serde(with = "base64_bytes")]
    compressed: Vec<u8>,
    uncompressed_bytes: usize,
    glyph_count: usize,
}

mod base64_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

impl GlyphBlock {
    fn encode(items: &[CompactGlyph]) -> Result<Self, Failure> {
        let mut raw = Vec::new();
        serde_json::to_writer(&mut CeilingWriter::new(&mut raw, MAX_RESPONSE), items).map_err(
            |_| {
                Failure::core(pdfdelta_core::Error::LimitExceeded {
                    resource: "native wire inflated glyph bytes",
                    limit: MAX_RESPONSE,
                })
            },
        )?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(&raw)
            .map_err(|error| Failure::backend(error.to_string()))?;
        let compressed = encoder
            .finish()
            .map_err(|error| Failure::backend(error.to_string()))?;
        Ok(Self {
            compressed,
            uncompressed_bytes: raw.len(),
            glyph_count: items.len(),
        })
    }

    fn decode(self) -> Result<Vec<CompactGlyph>, Failure> {
        if self.uncompressed_bytes > MAX_RESPONSE
            || self.glyph_count > EvidenceLimits::default().max_items
        {
            return Err(Failure::core(pdfdelta_core::Error::LimitExceeded {
                resource: "native wire glyph block",
                limit: MAX_RESPONSE,
            }));
        }
        let mut decoder = GzDecoder::new(self.compressed.as_slice());
        let mut raw = Vec::new();
        decoder
            .by_ref()
            .take(self.uncompressed_bytes as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|error| {
                Failure::backend(format!("invalid native glyph compression: {error}"))
            })?;
        if raw.len() != self.uncompressed_bytes || !decoder.into_inner().is_empty() {
            return Err(Failure::backend(
                "native glyph block length or trailing data mismatch",
            ));
        }
        let mut deserializer = serde_json::Deserializer::from_slice(&raw);
        let items = BoundedGlyphs(self.glyph_count)
            .deserialize(&mut deserializer)
            .map_err(|error| Failure::backend(format!("invalid native glyph block: {error}")))?;
        deserializer
            .end()
            .map_err(|error| Failure::backend(error.to_string()))?;
        if items.len() != self.glyph_count {
            return Err(Failure::backend("native glyph count mismatch"));
        }
        Ok(items)
    }
}

struct BoundedGlyphs(usize);

impl<'de> DeserializeSeed<'de> for BoundedGlyphs {
    type Value = Vec<CompactGlyph>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for BoundedGlyphs {
    type Value = Vec<CompactGlyph>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded glyph sequence")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = sequence.next_element()? {
            if items.len() == self.0 {
                return Err(serde::de::Error::custom("native glyph count exceeded"));
            }
            items.push(item);
        }
        Ok(items)
    }
}

#[derive(Serialize, Deserialize)]
struct Paint(PageId, u32, Option<[f64; 4]>, (u32, u16), u32);

impl NativeDocument {
    fn encode(value: Document<CompactGlyph>) -> Result<Self, Failure> {
        let marked_content = value.marked_content().to_vec();
        let last_non_text_paint = value.last_non_text_paint().clone();
        let non_text_paint_bounds = value.non_text_paint_bounds().map(|paints| {
            paints
                .iter()
                .map(|paint| {
                    Paint(
                        paint.page,
                        paint.render_order,
                        paint
                            .bounds
                            .map(|bounds| [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]),
                        (
                            paint.content_stream.object_number,
                            paint.content_stream.generation,
                        ),
                        paint.operator_index,
                    )
                })
                .collect()
        });
        let (items, vector_lines) = value.into_parts();
        Ok(Self {
            items: GlyphBlock::encode(&items)?,
            vector_lines,
            marked_content,
            last_non_text_paint,
            non_text_paint_bounds,
        })
    }
}

impl NativeDocument {
    fn decode(self, atoms: &[GlyphAtom]) -> Result<Document<Glyph>, Failure> {
        let items = self.items.decode()?;
        if items.first().is_some_and(|glyph| glyph.6.is_none()) {
            return Err(Failure::backend(
                "native response starts with an absent glyph context",
            ));
        }
        validate_atoms(&items, atoms, EvidenceLimits::default())?;
        let mut context = None;
        let glyphs = items
            .into_iter()
            .map(|glyph| glyph.decode(&mut context, atoms))
            .collect();
        let mut document = Document::with_vector_lines(glyphs, self.vector_lines)
            .with_marked_content(self.marked_content);
        if let Some(paints) = self.non_text_paint_bounds {
            document = document.with_non_text_paint_bounds(
                paints
                    .into_iter()
                    .map(|paint| {
                        let Paint(
                            page,
                            render_order,
                            bounds,
                            (object_number, generation),
                            operator_index,
                        ) = paint;
                        NonTextPaint {
                            page,
                            render_order,
                            bounds: bounds.map(|[x0, y0, x1, y1]| Rect {
                                min: Vec2 { x: x0, y: y0 },
                                max: Vec2 { x: x1, y: y1 },
                            }),
                            content_stream: ObjectRef {
                                object_number,
                                generation,
                            },
                            operator_index,
                        }
                    })
                    .collect(),
            );
        }
        // Preserve the supplied map even when the optional paint census is absent.
        Ok(document.with_last_non_text_paint(self.last_non_text_paint))
    }
}

#[derive(Serialize, Deserialize)]
struct CompactGlyph(
    GlyphId,
    Atom,
    [f64; 2],
    Option<f64>,
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
    fn encode(glyph: Glyph, previous: &mut Option<GlyphContext>, atoms: &mut Atoms) -> Self {
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
            atoms.encode((text, raw_code)),
            [bbox.min.x, bbox.max.x],
            (baseline.x.to_bits() != bbox.min.x.to_bits()).then_some(baseline.x),
            render_order,
            provenance.operator_index,
            context,
        )
    }

    fn decode(self, previous: &mut Option<GlyphContext>, atoms: &[GlyphAtom]) -> Glyph {
        let CompactGlyph(id, atom, bbox, baseline, render_order, operator_index, context) = self;
        let (text, raw_code) = match atom {
            Atom::Shared(index) => atoms[usize::from(index)].clone(),
            Atom::Inline(atom) => atom,
        };
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
                x: baseline.unwrap_or(bbox[0]),
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

impl Acquisition {
    pub(super) fn encode(value: super::Acquisition) -> Result<Self, Failure> {
        let EvidenceStore {
            revision,
            backends,
            pages,
            native,
            rendered,
            structured,
            inventories,
            key_inventories,
            native_structures,
            issues,
        } = value.store;
        let mut context = None;
        let mut atoms = Atoms::default();
        let native =
            native.map_items(|glyph| CompactGlyph::encode(glyph, &mut context, &mut atoms));
        Ok(Self {
            version: VERSION,
            store: Store {
                revision,
                backends,
                pages,
                atoms: atoms.finish(),
                native: NativeDocument::encode(native)?,
                rendered,
                structured,
                inventories: inventories.into_iter().map(Inventory::from).collect(),
                key_inventories,
                native_structures,
                issues,
            },
            page_refs: value.page_refs,
        })
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
            atoms,
            native,
            rendered,
            structured,
            inventories,
            key_inventories,
            native_structures,
            issues,
        } = self.store;
        validate_inventories(&inventories, EvidenceLimits::default())?;
        Ok(super::Acquisition {
            store: EvidenceStore {
                revision,
                backends,
                pages,
                native: native.decode(&atoms)?,
                rendered,
                structured,
                inventories: inventories.into_iter().map(Inventory::decode).collect(),
                key_inventories,
                native_structures,
                issues,
            },
            page_refs: self.page_refs,
        })
    }
}

/// Validate references and expanded payload bytes before cloning shared text.
/// The byte ceiling on compressed JSON is not a bound on decoded evidence.
fn validate_atoms(
    glyphs: &[CompactGlyph],
    atoms: &[GlyphAtom],
    limits: EvidenceLimits,
) -> Result<(), super::Failure> {
    let bounded = |count, limit, resource| {
        if count > limit {
            Err(super::Failure::core(pdfdelta_core::Error::LimitExceeded {
                resource,
                limit,
            }))
        } else {
            Ok(())
        }
    };
    bounded(atoms.len(), MAX_ATOMS, "native wire text atoms")?;
    bounded(glyphs.len(), limits.max_items, "native wire glyphs")?;
    let mut bytes = 0usize;
    for glyph in glyphs {
        let (text, raw_code) = match &glyph.1 {
            Atom::Shared(index) => atoms.get(usize::from(*index)).ok_or_else(|| {
                super::Failure::backend("native response references an unknown text atom")
            })?,
            Atom::Inline(atom) => atom,
        };
        let length = match text {
            DecodedText::Mapped(text) => text.len(),
            DecodedText::Unmapped { font_hash, .. } => font_hash.0.len(),
        };
        bytes = bytes.saturating_add(length).saturating_add(raw_code.len());
        bounded(
            bytes,
            limits.max_text_bytes,
            "native wire expanded text bytes",
        )?;
    }
    Ok(())
}

// Only adjacent, numerically consecutive native references share a run. Source
// order, gaps, duplicates, mixed origins, and inventory completeness are retained.
#[derive(Serialize, Deserialize)]
struct Inventory(Option<PageId>, Channel, usize, Vec<InventorySource>, bool);

#[derive(Serialize, Deserialize)]
enum InventorySource {
    Native { start: u64, count: usize },
    Single(SourceRef),
}

impl From<ChannelInventory> for Inventory {
    fn from(value: ChannelInventory) -> Self {
        let mut sources = Vec::new();
        for source in value.sources {
            if let SourceRef::Native { glyph } = source {
                if let Some(InventorySource::Native { start, count }) = sources.last_mut()
                    && start.checked_add(*count as u64) == Some(glyph.0)
                {
                    *count += 1;
                } else {
                    sources.push(InventorySource::Native {
                        start: glyph.0,
                        count: 1,
                    });
                }
            } else {
                sources.push(InventorySource::Single(source));
            }
        }
        Self(
            value.page,
            value.channel,
            value.backend,
            sources,
            value.complete,
        )
    }
}

impl Inventory {
    fn decode(self) -> ChannelInventory {
        let Self(page, channel, backend, encoded, complete) = self;
        let mut sources = Vec::new();
        for source in encoded {
            match source {
                InventorySource::Native { start, count } => {
                    sources.extend((0..count).map(|offset| SourceRef::Native {
                        glyph: GlyphId(start + offset as u64),
                    }));
                }
                InventorySource::Single(source) => sources.push(source),
            }
        }
        ChannelInventory {
            page,
            channel,
            backend,
            sources,
            complete,
        }
    }
}

fn validate_inventories(
    inventories: &[Inventory],
    limits: EvidenceLimits,
) -> Result<(), super::Failure> {
    let mut references = 0usize;
    for inventory in inventories {
        for source in &inventory.3 {
            let count = match source {
                InventorySource::Native { start, count } => {
                    if *count == 0 || start.checked_add((*count - 1) as u64).is_none() {
                        return Err(super::Failure::backend("invalid native inventory run"));
                    }
                    *count
                }
                InventorySource::Single(_) => 1,
            };
            references = references.saturating_add(count);
            if references > limits.max_items {
                return Err(super::Failure::core(pdfdelta_core::Error::LimitExceeded {
                    resource: "native wire expanded inventory references",
                    limit: limits.max_items,
                }));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(atom: Atom) -> CompactGlyph {
        CompactGlyph(GlyphId(0), atom, [0.0; 2], Some(0.0), 0, 0, None)
    }

    #[test]
    fn glyph_compression_rejects_corruption_lengths_counts_and_trailing_members() {
        let block = || GlyphBlock::encode(&[glyph(Atom::Shared(0))]).expect("encode");
        assert_eq!(block().decode().expect("valid block").len(), 1);
        for bytes in [
            0,
            block().uncompressed_bytes - 1,
            block().uncompressed_bytes + 1,
            MAX_RESPONSE + 1,
        ] {
            let mut invalid = block();
            invalid.uncompressed_bytes = bytes;
            assert!(invalid.decode().is_err());
        }
        for count in [0, 2, EvidenceLimits::default().max_items + 1] {
            let mut invalid = block();
            invalid.glyph_count = count;
            assert!(invalid.decode().is_err());
        }
        let original = block().compressed;
        for end in [0, original.len() / 2, original.len() - 1] {
            let mut invalid = block();
            invalid.compressed.truncate(end);
            assert!(invalid.decode().is_err());
        }
        let mut invalid = block();
        let crc = invalid.compressed.len() - 8;
        invalid.compressed[crc] ^= 1;
        assert!(invalid.decode().is_err());
        for suffix in [vec![0], original] {
            let mut invalid = block();
            invalid.compressed.extend(suffix);
            assert!(invalid.decode().is_err());
        }
    }

    #[test]
    fn compressed_document_still_rejects_missing_initial_context() {
        let document = Document::new(vec![glyph(Atom::Shared(0))]);
        assert!(
            NativeDocument::encode(document)
                .expect("encode")
                .decode(&[])
                .is_err()
        );
    }

    #[test]
    fn atom_table_saturation_preserves_inline_values_and_existing_references() {
        let mut atoms = Atoms::default();
        for index in 0..MAX_ATOMS {
            assert!(matches!(
                atoms.encode((DecodedText::Mapped(index.to_string()), vec![255])),
                Atom::Shared(value) if usize::from(value) == index
            ));
        }
        assert!(matches!(
            atoms.encode((DecodedText::Mapped("0".into()), vec![255])),
            Atom::Shared(0)
        ));
        let excess = (DecodedText::Mapped("additional λ".into()), vec![0, 128]);
        let Atom::Inline(retained) = atoms.encode(excess.clone()) else {
            panic!("full dictionary must retain an inline value");
        };
        assert_eq!(retained, excess);
        let table = atoms.finish();
        assert_eq!(table.len(), MAX_ATOMS);
        for (index, atom) in table.iter().enumerate() {
            assert_eq!(*atom, (DecodedText::Mapped(index.to_string()), vec![255]));
        }
    }

    #[test]
    fn expansion_checks_shared_and_inline_bytes_before_cloning() {
        let atoms = vec![(DecodedText::Mapped("abc".into()), vec![1, 2])];
        let glyphs = vec![
            glyph(Atom::Shared(0)),
            glyph(Atom::Inline(atoms[0].clone())),
        ];
        let mut limits = EvidenceLimits {
            max_text_bytes: 10,
            ..EvidenceLimits::default()
        };
        assert!(validate_atoms(&glyphs, &atoms, limits).is_ok());
        limits.max_text_bytes = 9;
        let error = validate_atoms(&glyphs, &atoms, limits).expect_err("expanded byte ceiling");
        assert_eq!(
            error.kind,
            pdfdelta_core::document::EvidenceFailure::ResourceLimit
        );
        limits.max_text_bytes = 10;
        limits.max_items = 1;
        assert!(validate_atoms(&glyphs, &atoms, limits).is_err());
    }

    #[test]
    fn missing_atoms_and_oversized_tables_are_rejected() {
        let glyphs = vec![glyph(Atom::Shared(1))];
        let atoms = vec![(DecodedText::Mapped("x".into()), vec![120])];
        assert!(validate_atoms(&glyphs, &atoms, EvidenceLimits::default()).is_err());
        assert!(
            validate_atoms(
                &[],
                &vec![atoms[0].clone(); MAX_ATOMS + 1],
                EvidenceLimits::default()
            )
            .is_err()
        );
    }

    #[test]
    fn inventory_runs_preserve_order_gaps_duplicates_and_mixed_sources() {
        let native = |id| SourceRef::Native { glyph: GlyphId(id) };
        let expected = ChannelInventory {
            page: Some(PageId(2)),
            channel: Channel::Text,
            backend: 3,
            complete: false,
            sources: vec![
                native(4),
                native(5),
                native(6),
                native(8),
                SourceRef::Structured { element: 7 },
                native(9),
                native(9),
                native(3),
            ],
        };
        let inventory = Inventory::from(expected.clone());
        assert_eq!(inventory.3.len(), 6);
        validate_inventories(std::slice::from_ref(&inventory), EvidenceLimits::default())
            .expect("valid runs");
        let encoded = serde_json::to_vec(&inventory).expect("wire inventory");
        let restored: Inventory = serde_json::from_slice(&encoded).expect("decode inventory");
        assert_eq!(restored.decode(), expected);
    }

    #[test]
    fn inventory_expansion_rejects_overflow_empty_runs_and_aggregate_limit() {
        let inventory = |start, count| {
            Inventory(
                None,
                Channel::Text,
                0,
                vec![InventorySource::Native { start, count }],
                false,
            )
        };
        for item in [inventory(0, 0), inventory(u64::MAX, 2)] {
            assert!(validate_inventories(&[item], EvidenceLimits::default()).is_err());
        }
        let limit = EvidenceLimits {
            max_items: 3,
            ..EvidenceLimits::default()
        };
        assert!(validate_inventories(&[inventory(u64::MAX, 1)], limit).is_ok());
        assert!(validate_inventories(&[inventory(0, 2), inventory(2, 2)], limit).is_err());
        assert!(validate_inventories(&[inventory(0, 2), inventory(2, 1)], limit).is_ok());
    }

    #[test]
    fn paint_transport_preserves_unknown_bounds_and_absent_census() {
        let unknown = NonTextPaint {
            page: PageId(2),
            render_order: 17,
            bounds: None,
            content_stream: ObjectRef {
                object_number: 7,
                generation: 3,
            },
            operator_index: 8,
        };
        let bounded = NonTextPaint {
            bounds: Some(Rect {
                min: Vec2 { x: -0.0, y: -5.0 },
                max: Vec2 { x: 4.25, y: 7.0 },
            }),
            ..unknown.clone()
        };
        for paints in [
            None,
            Some(Vec::new()),
            Some(vec![unknown.clone(), bounded.clone()]),
        ] {
            let mut document = Document::<CompactGlyph>::new(Vec::new());
            if let Some(paints) = paints {
                document = document.with_non_text_paint_bounds(paints);
            }
            document = document.with_last_non_text_paint([(PageId(2), 19)].into());
            let expected = serde_json::to_value(&document).expect("original paint evidence");
            let encoded =
                serde_json::to_vec(&NativeDocument::encode(document).expect("bounded glyph block"))
                    .expect("wire document");
            let restored: NativeDocument =
                serde_json::from_slice(&encoded).expect("retained paint fields");
            let restored = restored.decode(&[]).expect("valid glyph block");
            assert_eq!(
                serde_json::to_value(&restored).expect("restored paint evidence"),
                expected
            );
            if let Some(paints) = restored.non_text_paint_bounds()
                && !paints.is_empty()
            {
                assert!(paints[0].bounds.is_none());
                assert_eq!(
                    paints[1].bounds.expect("bounded paint").min.x.to_bits(),
                    (-0.0f64).to_bits()
                );
            }
        }
    }
}
