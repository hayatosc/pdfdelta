use std::{collections::BTreeMap, sync::Arc};

use pdfdelta_core::{
    Error, Result,
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::{
        DecodedStream, ObjectRef, PageRef, ParseLimits, ParsedPdf, PdfDict, PdfObject, PdfParser,
        PdfVersion, RawStream,
    },
    source::{
        ExtractionIssue, ExtractionIssueKind, ExtractionLimits, ExtractionOutcome, ExtractionScope,
        GlyphExtractor, ParserBackedGlyphSource,
    },
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

struct IssueParser(Error);

impl PdfParser for IssueParser {
    fn parse(&self, _pdf: Arc<[u8]>, _limits: ParseLimits) -> Result<Box<dyn ParsedPdf>> {
        Err(self.0.clone())
    }
}

struct FixtureExtractor;

impl GlyphExtractor for FixtureExtractor {
    fn extract(&self, pdf: &dyn ParsedPdf, _limits: ExtractionLimits) -> Result<Document<Glyph>> {
        assert_eq!(pdf.version().major, 1);
        Ok(Document::new(vec![fixture_glyph()]))
    }
}

struct LegacyIssueExtractor(Error);

impl GlyphExtractor for LegacyIssueExtractor {
    fn extract(&self, _pdf: &dyn ParsedPdf, _limits: ExtractionLimits) -> Result<Document<Glyph>> {
        Err(self.0.clone())
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
            fixture_parse_limits(),
            fixture_extraction_limits(),
        )
        .expect("fixture extraction should succeed");

    assert_eq!(document.items(), [fixture_glyph()]);
}

#[test]
fn legacy_style_extractor_gets_a_complete_default_outcome() {
    let outcome = FixtureExtractor
        .extract_outcome(&FixturePdf, fixture_extraction_limits())
        .expect("legacy-style extractor should use the default outcome adapter");

    assert!(outcome.is_complete());
    assert_eq!(outcome.document().items(), [fixture_glyph()]);
}

#[test]
fn legacy_style_extractor_default_outcome_classifies_only_recoverable_errors() {
    for (error, expected_kind) in [
        (
            Error::Unsupported("extractor feature is not supported".to_owned()),
            ExtractionIssueKind::Unsupported,
        ),
        (
            Error::Unresolved("extractor structure is ambiguous".to_owned()),
            ExtractionIssueKind::Unresolved,
        ),
    ] {
        let outcome = LegacyIssueExtractor(error)
            .extract_outcome(&FixturePdf, fixture_extraction_limits())
            .expect("recoverable extractor error should become an outcome");

        assert!(outcome.document().items().is_empty());
        assert_eq!(outcome.issues().len(), 1);
        assert_eq!(outcome.issues()[0].kind(), expected_kind);
        assert_eq!(outcome.issues()[0].scope(), ExtractionScope::Document);
    }

    assert!(matches!(
        LegacyIssueExtractor(Error::Backend("extractor failed".to_owned()))
            .extract_outcome(&FixturePdf, fixture_extraction_limits()),
        Err(Error::Backend(message)) if message == "extractor failed"
    ));
}

#[test]
fn legacy_style_extractor_recovers_blank_messages_without_losing_error_class() {
    for (error, expected_kind, expected_description) in [
        (
            Error::Unsupported(String::new()),
            ExtractionIssueKind::Unsupported,
            "unsupported extraction feature",
        ),
        (
            Error::Unresolved("  \t".to_owned()),
            ExtractionIssueKind::Unresolved,
            "unresolved extraction content",
        ),
    ] {
        let source =
            ParserBackedGlyphSource::new(FixtureParser, LegacyIssueExtractor(error.clone()));
        let outcome = source
            .extract_outcome(
                Arc::from(&b"%PDF-fixture"[..]),
                fixture_parse_limits(),
                fixture_extraction_limits(),
            )
            .expect("blank recoverable extractor error should use a fallback description");

        assert_eq!(outcome.issues()[0].kind(), expected_kind);
        assert_eq!(outcome.issues()[0].description(), expected_description);
        assert_error_kind(
            source
                .extract(
                    Arc::from(&b"%PDF-fixture"[..]),
                    fixture_parse_limits(),
                    fixture_extraction_limits(),
                )
                .expect_err("complete extraction should restore the extractor error class"),
            expected_kind,
        );
    }
}

#[test]
fn parser_backed_source_reports_document_scoped_parser_issues() {
    for (error, expected_kind) in [
        (
            Error::Unsupported("document feature is not supported".to_owned()),
            ExtractionIssueKind::Unsupported,
        ),
        (
            Error::Unresolved("document structure is ambiguous".to_owned()),
            ExtractionIssueKind::Unresolved,
        ),
    ] {
        let source = ParserBackedGlyphSource::new(IssueParser(error.clone()), FixtureExtractor);
        let outcome = source
            .extract_outcome(
                Arc::from(&b"%PDF-fixture"[..]),
                fixture_parse_limits(),
                fixture_extraction_limits(),
            )
            .expect("parser issue should become an extraction outcome");

        assert!(outcome.document().items().is_empty());
        assert_eq!(outcome.issues().len(), 1);
        assert_eq!(outcome.issues()[0].kind(), expected_kind);
        assert_eq!(outcome.issues()[0].scope(), ExtractionScope::Document);
        assert!(!outcome.issues()[0].description().is_empty());
        assert_eq!(
            source
                .extract(
                    Arc::from(&b"%PDF-fixture"[..]),
                    fixture_parse_limits(),
                    fixture_extraction_limits(),
                )
                .expect_err("complete extraction should restore the parser error class"),
            error
        );
    }
}

#[test]
fn parser_recovers_blank_messages_without_losing_error_class() {
    for (error, expected_kind, expected_description) in [
        (
            Error::Unsupported(" \n".to_owned()),
            ExtractionIssueKind::Unsupported,
            "unsupported extraction feature",
        ),
        (
            Error::Unresolved(String::new()),
            ExtractionIssueKind::Unresolved,
            "unresolved extraction content",
        ),
    ] {
        let source = ParserBackedGlyphSource::new(IssueParser(error), FixtureExtractor);
        let outcome = source
            .extract_outcome(
                Arc::from(&b"%PDF-fixture"[..]),
                fixture_parse_limits(),
                fixture_extraction_limits(),
            )
            .expect("blank recoverable parser error should use a fallback description");

        assert_eq!(outcome.issues()[0].kind(), expected_kind);
        assert_eq!(outcome.issues()[0].description(), expected_description);
        assert_error_kind(
            source
                .extract(
                    Arc::from(&b"%PDF-fixture"[..]),
                    fixture_parse_limits(),
                    fixture_extraction_limits(),
                )
                .expect_err("complete extraction should restore the parser error class"),
            expected_kind,
        );
    }
}

#[test]
fn parser_backed_source_keeps_fatal_parser_errors() {
    let source = ParserBackedGlyphSource::new(
        IssueParser(Error::Backend("fixture parser failed".to_owned())),
        FixtureExtractor,
    );

    assert!(matches!(
        source.extract_outcome(
            Arc::from(&b"%PDF-fixture"[..]),
            fixture_parse_limits(),
            fixture_extraction_limits(),
        ),
        Err(Error::Backend(message)) if message == "fixture parser failed"
    ));
}

#[test]
fn extraction_issue_requires_a_description() {
    assert!(matches!(
        pdfdelta_core::source::ExtractionIssue::new(
            ExtractionIssueKind::Unsupported,
            ExtractionScope::Document,
            "  ",
        ),
        Err(Error::InvalidConfiguration(message)) if message.contains("require a description")
    ));
}

#[test]
fn document_scoped_outcome_requires_empty_evidence_and_one_issue() -> Result<()> {
    let issue = ExtractionIssue::new(
        ExtractionIssueKind::Unsupported,
        ExtractionScope::Document,
        "document feature is not supported",
    )?;
    assert!(matches!(
        ExtractionOutcome::new(Document::new(vec![fixture_glyph()]), vec![issue.clone()]),
        Err(Error::InvalidConfiguration(message)) if message.contains("requires an empty document")
    ));
    assert!(matches!(
        ExtractionOutcome::new(Document::new(Vec::new()), vec![issue.clone(), issue]),
        Err(Error::InvalidConfiguration(message)) if message.contains("sole issue")
    ));
    Ok(())
}

#[test]
fn page_scoped_outcome_rejects_duplicate_pages() -> Result<()> {
    let issue = ExtractionIssue::new(
        ExtractionIssueKind::Unresolved,
        ExtractionScope::Page(PageId(3)),
        "page structure is ambiguous",
    )?;

    assert!(matches!(
        ExtractionOutcome::new(Document::new(Vec::new()), vec![issue.clone(), issue]),
        Err(Error::InvalidConfiguration(message)) if message.contains("duplicate")
    ));
    Ok(())
}

#[test]
fn page_scoped_outcome_rejects_glyphs_from_the_affected_page() -> Result<()> {
    let same_page_issue = ExtractionIssue::new(
        ExtractionIssueKind::Unresolved,
        ExtractionScope::Page(PageId(0)),
        "page structure is ambiguous",
    )?;
    assert!(matches!(
        ExtractionOutcome::new(
            Document::new(vec![fixture_glyph()]),
            vec![same_page_issue]
        ),
        Err(Error::InvalidConfiguration(message)) if message.contains("cannot retain glyph evidence")
    ));

    let other_page_issue = ExtractionIssue::new(
        ExtractionIssueKind::Unresolved,
        ExtractionScope::Page(PageId(1)),
        "another page is ambiguous",
    )?;
    let outcome =
        ExtractionOutcome::new(Document::new(vec![fixture_glyph()]), vec![other_page_issue])?;
    assert_eq!(outcome.document().items(), [fixture_glyph()]);
    assert_eq!(
        outcome.issues()[0].scope(),
        ExtractionScope::Page(PageId(1))
    );
    Ok(())
}

fn assert_error_kind(error: Error, expected: ExtractionIssueKind) {
    assert!(
        matches!(
            (expected, &error),
            (ExtractionIssueKind::Unsupported, Error::Unsupported(_))
                | (ExtractionIssueKind::Unresolved, Error::Unresolved(_))
        ),
        "expected {expected:?}, got {error}"
    );
}

fn fixture_parse_limits() -> ParseLimits {
    ParseLimits {
        max_input_bytes: 1024,
        max_objects: 10,
        max_recursion_depth: 4,
        max_decoded_stream_bytes: 1024,
        max_total_object_stream_bytes: 4096,
        max_pages: 1,
    }
}

fn fixture_extraction_limits() -> ExtractionLimits {
    ExtractionLimits {
        max_glyphs: 10,
        max_form_depth: 2,
        max_nesting_depth: 4,
        max_operators: 100,
        max_stream_invocations: 10,
        max_total_decoded_bytes: 4096,
        max_operand_stack: 16,
        max_operand_nodes: 64,
        max_fonts: 4,
        max_cmap_entries: 16,
        max_cid_width_entries: 16,
        max_string_bytes: 1024,
    }
}
