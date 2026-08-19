use std::{collections::BTreeMap, sync::Arc};

use pdfdelta_core::{
    Result,
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::{
        DecodedStream, ObjectRef, PageRef, ParseLimits, ParsedPdf, PdfDict, PdfObject, PdfParser,
        PdfVersion, RawStream,
    },
    source::{ExtractionLimits, GlyphExtractor, ParserBackedGlyphSource},
};

struct FixturePdf;

impl ParsedPdf for FixturePdf {
    fn version(&self) -> PdfVersion {
        PdfVersion { major: 1, minor: 7 }
    }

    fn trailer(&self) -> Result<PdfDict> {
        Ok(BTreeMap::new())
    }

    fn resolve(&self, _reference: ObjectRef) -> Result<PdfObject> {
        Ok(PdfObject::Null)
    }

    fn pages(&self) -> Result<Vec<PageRef>> {
        Ok(vec![PageRef(ObjectRef {
            object_number: 3,
            generation: 0,
        })])
    }

    fn page_dict(&self, _page: PageRef) -> Result<PdfDict> {
        Ok(BTreeMap::new())
    }

    fn raw_stream(&self, _reference: ObjectRef) -> Result<RawStream> {
        Ok(RawStream {
            dictionary: BTreeMap::new(),
            bytes: b"BT (A) Tj ET".to_vec(),
        })
    }

    fn decoded_stream(&self, _reference: ObjectRef) -> Result<DecodedStream> {
        Ok(DecodedStream {
            dictionary: BTreeMap::new(),
            bytes: b"BT (A) Tj ET".to_vec(),
        })
    }
}

struct FixtureParser;

impl PdfParser for FixtureParser {
    fn parse(&self, pdf: Arc<[u8]>, _limits: ParseLimits) -> Result<Box<dyn ParsedPdf>> {
        assert_eq!(pdf.as_ref(), b"%PDF-fixture");
        Ok(Box::new(FixturePdf))
    }
}

struct FixtureExtractor;

impl GlyphExtractor for FixtureExtractor {
    fn extract(&self, pdf: &dyn ParsedPdf, _limits: ExtractionLimits) -> Result<Document<Glyph>> {
        assert_eq!(pdf.version().major, 1);
        Ok(Document::new(vec![fixture_glyph()]))
    }
}

fn fixture_glyph() -> Glyph {
    Glyph {
        id: GlyphId(1),
        text: DecodedText::Mapped("A".to_owned()),
        raw_code: vec![0x41],
        page: PageId(0),
        bbox: Rect {
            min: Vec2 { x: 10.0, y: 20.0 },
            max: Vec2 { x: 16.0, y: 30.0 },
        },
        baseline: Vec2 { x: 1.0, y: 0.0 },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(7),
        font_size: 10.0,
        render_order: 0,
        render_mode: TextRenderMode::Fill,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 12,
                generation: 0,
            },
            operator_index: 3,
        },
    }
}

#[test]
fn parser_backed_source_preserves_glyph_evidence() {
    let source = ParserBackedGlyphSource::new(FixtureParser, FixtureExtractor);
    let document = source
        .extract(
            Arc::from(&b"%PDF-fixture"[..]),
            ParseLimits {
                max_input_bytes: 1024,
                max_objects: 10,
                max_recursion_depth: 4,
                max_decoded_stream_bytes: 1024,
                max_total_object_stream_bytes: 4096,
                max_pages: 1,
            },
            ExtractionLimits {
                max_glyphs: 10,
                max_form_depth: 2,
                max_nesting_depth: 4,
            },
        )
        .expect("fixture extraction should succeed");

    assert_eq!(document.items(), [fixture_glyph()]);
}
