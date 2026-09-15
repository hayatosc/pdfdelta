use std::sync::Arc;

use lopdf::{Document as LopdfDocument, Object, ObjectId, Stream, dictionary};
use pdfdelta_core::{
    Error, Result,
    diff::{ChangeKind, Confidence, TextSpan, TokenRange},
    extraction_conformance::{
        GeometryTolerance, PrimitiveExtractionSnapshot, SnapshotGlyph, compare_snapshots,
    },
    layout::BlockId,
    model::{
        DecodedText, Document, Glyph, GlyphCropStatus, GlyphPathClipStatus, PageId, Rect, Vec2,
    },
    normalize::ScalarRange,
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::{PipelineOptions, compare_glyph_documents},
    source::{
        ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionIssueKind, ExtractionLimits,
        ExtractionOutcome, ExtractionScope, GlyphExtractor,
    },
};

fn base_font(document: &mut LopdfDocument) -> lopdf::ObjectId {
    let widths = vec![Object::Integer(500); 256];
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => widths,
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Helvetica",
            "Ascent" => 800,
            "Descent" => -200,
            "MissingWidth" => 500,
        },
    })
}

fn symbol_set_font(document: &mut LopdfDocument, to_unicode: Option<ObjectId>) -> lopdf::ObjectId {
    let font = base_font(document);
    let dictionary = document
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    dictionary.set("Encoding", "SymbolSetEncoding");
    if let Some(to_unicode) = to_unicode {
        dictionary.set("ToUnicode", to_unicode);
    }
    font
}

fn embedded_simple_font(
    document: &mut LopdfDocument,
    program_bytes: &[u8],
    compressed: bool,
    to_unicode: Option<ObjectId>,
) -> ObjectId {
    let mut program = Stream::new(dictionary! {}, program_bytes.to_vec());
    if compressed {
        program.compress().expect("fixture font should compress");
    }
    let program = document.add_object(program);
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "FixtureEmbedded",
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
        "Flags" => 32,
        "FontFile2" => program,
    });
    let mut font = dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "FixtureEmbedded",
        "FirstChar" => 65,
        "LastChar" => 66,
        "Widths" => vec![Object::Integer(600), Object::Integer(700)],
        "FontDescriptor" => descriptor,
    };
    if let Some(to_unicode) = to_unicode {
        font.set("ToUnicode", to_unicode);
    }
    document.add_object(font)
}

fn embedded_font_program(document: &LopdfDocument, font: ObjectId) -> ObjectId {
    let descriptor = embedded_font_descriptor(document, font);
    document.objects[&descriptor]
        .as_dict()
        .expect("fixture descriptor should be a dictionary")
        .get(b"FontFile2")
        .expect("fixture descriptor should have a font program")
        .as_reference()
        .expect("fixture font program should be indirect")
}

fn embedded_font_descriptor(document: &LopdfDocument, font: ObjectId) -> ObjectId {
    document.objects[&font]
        .as_dict()
        .expect("fixture font should be a dictionary")
        .get(b"FontDescriptor")
        .expect("fixture font should have a descriptor")
        .as_reference()
        .expect("fixture descriptor should be indirect")
}

fn identity_h_font(
    document: &mut LopdfDocument,
    to_unicode: ObjectId,
    widths: Vec<Object>,
) -> ObjectId {
    let descendant = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "FixtureSans",
        "DW" => 900,
        "W" => widths,
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "FixtureSans",
            "Ascent" => 800,
            "Descent" => -200,
        },
    });
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "FixtureSans",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(descendant)],
        "ToUnicode" => to_unicode,
    })
}

fn identity_v_font(document: &mut LopdfDocument, to_unicode: ObjectId) -> ObjectId {
    let descendant = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType0",
        "BaseFont" => "FixtureVertical",
        "DW" => 1000,
        "DW2" => vec![Object::Integer(880), Object::Integer(-1000)],
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "FixtureVertical",
            "Ascent" => 1179,
            "Descent" => -179,
        },
    });
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "FixtureVertical",
        "Encoding" => "Identity-V",
        "DescendantFonts" => vec![Object::Reference(descendant)],
        "ToUnicode" => to_unicode,
    })
}

fn embedded_identity_h_font(
    document: &mut LopdfDocument,
    to_unicode: ObjectId,
    cid_to_gid_map: Option<Object>,
) -> ObjectId {
    let program = document.add_object(Stream::new(dictionary! {}, b"embedded CID font".to_vec()));
    let mut descendant = dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "FixtureCidEmbedded",
        "DW" => 900,
        "W" => vec![
            Object::Integer(1),
            Object::Array(vec![Object::Integer(500), Object::Integer(700)]),
        ],
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "FixtureCidEmbedded",
            "Ascent" => 800,
            "Descent" => -200,
            "FontFile2" => program,
        },
    };
    if let Some(cid_to_gid_map) = cid_to_gid_map {
        descendant.set("CIDToGIDMap", cid_to_gid_map);
    }
    let descendant = document.add_object(descendant);
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "FixtureCidEmbedded",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(descendant)],
        "ToUnicode" => to_unicode,
    })
}

fn install_page(
    document: &mut LopdfDocument,
    contents: Object,
    resources: Object,
    crop_box: Option<[i64; 4]>,
    rotation: Option<i64>,
) {
    let pages = document.new_object_id();
    let mut page = dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "Contents" => contents,
        "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
    };
    if let Some([x0, y0, x1, y1]) = crop_box {
        page.set("CropBox", vec![x0.into(), y0.into(), x1.into(), y1.into()]);
    }
    if let Some(rotation) = rotation {
        page.set("Rotate", rotation);
    }
    let page = document.add_object(page);
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page)],
            "Count" => 1,
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
    });
    document.trailer.set("Root", catalog);
}

fn install_plain_pages(document: &mut LopdfDocument, contents: &[Object], resources: Object) {
    let pages = document.new_object_id();
    let page_ids = contents
        .iter()
        .map(|contents| {
            document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages,
                "Contents" => contents.clone(),
                "Resources" => resources.clone(),
                "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
            })
        })
        .collect::<Vec<_>>();
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.into_iter().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => i64::try_from(contents.len()).expect("page count should fit in i64"),
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
    });
    document.trailer.set("Root", catalog);
}

fn extract(mut document: LopdfDocument, limits: ExtractionLimits) -> Result<Document<Glyph>> {
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor.extract(pdf.as_ref(), limits)
}

fn ruled_two_column_document(changed_value: &str) -> LopdfDocument {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = format!(
        "1 w 40 160 220 55 re S 150 160 m 150 215 l S 40 195 m 260 195 l S 40 180 m 260 180 l S BT /F1 10 Tf 1 0 0 1 50 200 Tm (Alpha) Tj 1 0 0 1 180 200 Tm (One) Tj 1 0 0 1 50 185 Tm (Beta) Tj 1 0 0 1 180 185 Tm ({changed_value}) Tj 1 0 0 1 50 170 Tm (Gamma) Tj 1 0 0 1 180 170 Tm (Three) Tj ET"
    );
    let content = pdf.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    pdf
}

fn extract_outcome(
    mut document: LopdfDocument,
    limits: ExtractionLimits,
) -> Result<ExtractionOutcome> {
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor.extract_outcome(pdf.as_ref(), limits)
}

fn extract_with_external_font_identities(
    mut document: LopdfDocument,
    limits: ExtractionLimits,
    identities: &ExternalFontIdentities,
) -> Result<Document<Glyph>> {
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor
        .extract_outcome_with_external_font_identities(pdf.as_ref(), limits, identities)?
        .into_complete()
}

fn mapped_text(glyphs: &[Glyph]) -> String {
    glyphs
        .iter()
        .map(|glyph| match &glyph.text {
            DecodedText::Mapped(text) => text.as_str(),
            DecodedText::Unmapped { .. } => "<unmapped>",
        })
        .collect()
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

fn document_with_unsupported_middle_page() -> LopdfDocument {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let contents = [
        b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (A) Tj ET".as_slice(),
        b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (B) Tj ET ZZ".as_slice(),
        b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (C) Tj ET".as_slice(),
    ]
    .into_iter()
    .map(|bytes| Object::Reference(pdf.add_object(Stream::new(dictionary! {}, bytes.to_vec()))))
    .collect::<Vec<_>>();
    let resources = Object::Dictionary(dictionary! {
        "Font" => dictionary! { "F1" => font },
    });
    install_plain_pages(&mut pdf, &contents, resources);
    pdf
}

fn document_with_unsupported_then_fatal_page() -> LopdfDocument {
    let mut pdf = LopdfDocument::with_version("1.7");
    let contents = [b"ZZ".as_slice(), b"q Q".as_slice()]
        .into_iter()
        .map(|bytes| Object::Reference(pdf.add_object(Stream::new(dictionary! {}, bytes.to_vec()))))
        .collect::<Vec<_>>();
    install_plain_pages(&mut pdf, &contents, Object::Dictionary(dictionary! {}));
    pdf
}

#[test]
fn rolls_back_unsupported_page_and_continues_later_pages() -> Result<()> {
    let outcome = extract_outcome(
        document_with_unsupported_middle_page(),
        ExtractionLimits::default(),
    )?;

    assert_eq!(mapped_text(outcome.document().items()), "AC");
    assert_eq!(
        outcome
            .document()
            .items()
            .iter()
            .map(|glyph| glyph.page.0)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert_eq!(outcome.issues().len(), 1);
    assert_eq!(outcome.issues()[0].kind(), ExtractionIssueKind::Unsupported);
    assert_eq!(
        outcome.issues()[0].scope(),
        ExtractionScope::Page(pdfdelta_core::model::PageId(1))
    );
    assert!(outcome.issues()[0].description().contains("ZZ"));

    assert!(matches!(
        extract(
            document_with_unsupported_middle_page(),
            ExtractionLimits::default()
        ),
        Err(Error::Unsupported(message)) if message.contains("ZZ")
    ));
    Ok(())
}

#[test]
fn later_fatal_page_overrides_an_earlier_unsupported_page() {
    let limits = ExtractionLimits {
        max_operators: 1,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract_outcome(document_with_unsupported_then_fatal_page(), limits),
        Err(Error::LimitExceeded {
            resource: "content operators",
            limit: 1,
        })
    ));
}

#[test]
fn extracts_rotated_simple_font_glyphs_with_provenance() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (AB) Tj ET".to_vec(),
    ));
    let resources = dictionary! {
        "Font" => dictionary! { "F1" => font },
    };
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(resources),
        Some([10, 20, 210, 120]),
        Some(90),
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "AB");
    assert_eq!(glyphs[0].raw_code, b"A");
    assert_eq!(glyphs[0].crop_status, GlyphCropStatus::Inside);
    assert_eq!(glyphs[0].provenance.content_stream.object_number, content.0);
    assert_eq!(glyphs[0].provenance.operator_index, 3);
    let expected = PrimitiveExtractionSnapshot::new(vec![
        SnapshotGlyph::mapped(
            "A",
            PageId(0),
            0,
            Rect {
                min: Vec2 { x: 18.0, y: 175.0 },
                max: Vec2 { x: 28.0, y: 180.0 },
            },
            Vec2 { x: 20.0, y: 180.0 },
            Vec2 { x: 0.0, y: -1.0 },
        ),
        SnapshotGlyph::mapped(
            "B",
            PageId(0),
            1,
            Rect {
                min: Vec2 { x: 18.0, y: 170.0 },
                max: Vec2 { x: 28.0, y: 175.0 },
            },
            Vec2 { x: 20.0, y: 175.0 },
            Vec2 { x: 0.0, y: -1.0 },
        ),
    ]);
    compare_snapshots(
        &expected,
        &PrimitiveExtractionSnapshot::from(&document),
        GeometryTolerance::new(1e-9).expect("valid tolerance"),
    )
    .expect("rotated extraction should match the geometry oracle");
    Ok(())
}

#[test]
fn normalizes_reversed_page_box_coordinates_before_rotation() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        Some([210, 120, 10, 20]),
        Some(90),
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyph = &document.items()[0];
    assert_close(glyph.baseline.x, 20.0);
    assert_close(glyph.baseline.y, 180.0);
    assert_close(glyph.direction.x, 0.0);
    assert_close(glyph.direction.y, -1.0);
    Ok(())
}

#[test]
fn records_crop_box_visibility_without_discarding_glyph_evidence() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 60 100 Tm (I) Tj 1 0 0 1 48 100 Tm (P) Tj 1 0 0 1 30 100 Tm (O) Tj ET"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        Some([50, 50, 250, 250]),
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "IPO");
    assert_eq!(
        document
            .items()
            .iter()
            .map(|glyph| glyph.crop_status)
            .collect::<Vec<_>>(),
        [
            GlyphCropStatus::Inside,
            GlyphCropStatus::PartiallyOutside,
            GlyphCropStatus::Outside,
        ]
    );
    Ok(())
}

#[test]
fn retains_rectangular_clip_status_and_straight_path_evidence() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 40 40 100 80 re W n BT /F1 10 Tf 1 0 0 1 60 80 Tm (I) Tj 1 0 0 1 138 80 Tm (P) Tj 1 0 0 1 150 80 Tm (O) Tj ET Q 1 w 40 40 m 140 40 l 140 120 l 40 120 l h S 90 40 m 90 120 l S 40 80 m 140 80 l S"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "IPO");
    assert_eq!(
        document
            .items()
            .iter()
            .map(|glyph| glyph.path_clip_status)
            .collect::<Vec<_>>(),
        [
            GlyphPathClipStatus::Inside,
            GlyphPathClipStatus::PartiallyOutside,
            GlyphPathClipStatus::Outside,
        ]
    );

    let lines = document.vector_lines();
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[0].from, Vec2 { x: 40.0, y: 40.0 });
    assert_eq!(lines[0].to, Vec2 { x: 140.0, y: 40.0 });
    assert_eq!(lines[4].from, Vec2 { x: 90.0, y: 40.0 });
    assert_eq!(lines[4].to, Vec2 { x: 90.0, y: 120.0 });
    assert_eq!(lines[5].from, Vec2 { x: 40.0, y: 80.0 });
    assert_eq!(lines[5].to, Vec2 { x: 140.0, y: 80.0 });
    assert!(lines.iter().all(|line| line.width == 1.0));
    assert_eq!(lines[0].render_order, 3);
    assert_eq!(lines[5].render_order, 8);
    assert!(
        lines
            .iter()
            .all(|line| line.provenance.content_stream.object_number == content.0)
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.provenance.operator_index)
            .collect::<Vec<_>>(),
        [20, 20, 20, 20, 23, 26]
    );
    Ok(())
}

#[test]
fn recognizes_single_line_built_rectangular_clips() -> Result<()> {
    for path in [
        "40 40 m 140 40 l 140 120 l 40 120 l h",
        "40 40 m 140 40 l 140 120 l 40 120 l",
        "40 40 m 140 40 l 140 120 l 40 120 l 5 5 m",
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let content_bytes = format!(
            "q {path} W n \
             BT /F1 10 Tf 1 0 0 1 60 80 Tm (I) Tj \
             1 0 0 1 138 80 Tm (P) Tj 1 0 0 1 150 80 Tm (O) Tj ET Q"
        );
        let content = pdf.add_object(Stream::new(dictionary! {}, content_bytes.into_bytes()));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font },
            }),
            None,
            None,
        );

        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(outcome.is_complete(), "path: {path}");
        assert_eq!(mapped_text(outcome.document().items()), "IPO");
        assert_eq!(
            outcome
                .document()
                .items()
                .iter()
                .map(|glyph| glyph.path_clip_status)
                .collect::<Vec<_>>(),
            [
                GlyphPathClipStatus::Inside,
                GlyphPathClipStatus::PartiallyOutside,
                GlyphPathClipStatus::Outside,
            ]
        );
    }
    Ok(())
}

#[test]
fn convex_quadrilateral_clips_classify_glyphs_without_box_substitution() -> Result<()> {
    for (path, rule) in [
        ("40 80 m 100 20 l 160 80 l 100 140 l h", "W"),
        ("100 140 m 160 80 l 100 20 l 40 80 l", "W*"),
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let content = pdf.add_object(Stream::new(dictionary! {}, format!(
            "q {path} {rule} n BT /F1 10 Tf 1 0 0 1 95 80 Tm (I) Tj 1 0 0 1 153 80 Tm (P) Tj 1 0 0 1 45 25 Tm (O) Tj ET Q BT /F1 10 Tf 1 0 0 1 45 25 Tm (C) Tj ET"
        ).into_bytes()));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font },
            }),
            None,
            None,
        );
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(outcome.is_complete());
        let glyphs = outcome.document().items();
        assert_eq!(mapped_text(glyphs), "IPOC");
        assert_eq!(
            glyphs
                .iter()
                .map(|g| g.path_clip_status)
                .collect::<Vec<_>>(),
            [
                GlyphPathClipStatus::Inside,
                GlyphPathClipStatus::PartiallyOutside,
                GlyphPathClipStatus::Outside,
                GlyphPathClipStatus::Unclipped,
            ]
        );
        assert_eq!(glyphs[2].raw_code, b"O");
        assert_close(glyphs[2].baseline.x, 45.0);
        assert_eq!(glyphs[2].provenance.content_stream.object_number, content.0);
    }
    Ok(())
}

#[test]
fn convex_clip_classification_has_a_shared_extraction_work_limit() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"40 80 m 100 20 l 160 80 l 100 140 l h W n BT /F1 10 Tf 1 0 0 1 80 80 Tm (Repeated) Tj ET"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    assert!(matches!(
        extract(
            pdf,
            ExtractionLimits {
                max_operators: 50,
                ..ExtractionLimits::default()
            }
        ),
        Err(Error::LimitExceeded {
            resource: "convex clipping work",
            limit: 50
        })
    ));
}

#[test]
fn sheared_form_clip_intersects_caller_and_restores_after_invocation() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 40.into(), 40.into()],
            "Matrix" => vec![1.into(), 1.into(), 0.into(), 1.into(), 0.into(), 0.into()],
        },
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (I) Tj 1 0 0 1 30 20 Tm (O) Tj ET".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 0 0 20 100 re W n /X Do Q BT /F1 10 Tf 1 0 0 1 50 20 Tm (C) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "X" => form },
        }),
        None,
        None,
    );
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(outcome.is_complete());
    assert_eq!(mapped_text(outcome.document().items()), "IOC");
    assert_eq!(
        outcome
            .document()
            .items()
            .iter()
            .map(|g| g.path_clip_status)
            .collect::<Vec<_>>(),
        [
            GlyphPathClipStatus::Inside,
            GlyphPathClipStatus::Outside,
            GlyphPathClipStatus::Unclipped,
        ]
    );
    Ok(())
}

#[test]
fn opaque_form_paints_retain_the_outward_form_bound_without_leaking_it() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![10.into(), 20.into(), 30.into(), 40.into()],
            "Matrix" => vec![1.into(), 1.into(), 0.into(), 1.into(), 0.into(), 0.into()],
        },
        b"-100 -100 m 0 200 200 0 100 100 c f".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 1 0 0 1 50 60 cm /X Do Q -100 -100 m 0 200 200 0 100 100 c f".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X" => form },
        }),
        None,
        None,
    );
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(outcome.is_complete());
    let paints = outcome
        .document()
        .non_text_paint_bounds()
        .expect("paint records");
    assert_eq!(paints.len(), 2);
    let bounds = paints[0].bounds.expect("Form clips its opaque curve");
    for (actual, expected, lower) in [
        (bounds.min.x, 60.0, true),
        (bounds.min.y, 90.0, true),
        (bounds.max.x, 80.0, false),
        (bounds.max.y, 130.0, false),
    ] {
        assert!((actual - expected).abs() < 1e-9);
        assert!(if lower {
            actual <= expected
        } else {
            actual >= expected
        });
    }
    assert_eq!(paints[0].content_stream.object_number, form.0);
    assert!(paints[1].bounds.is_none());
    Ok(())
}

#[test]
fn rejects_non_rectangular_and_multiple_line_built_clips() -> Result<()> {
    for path in [
        "40 40 m 140 40 l 90 120 l h",
        "40 40 m 140 40 l 140 120 l 40 120 l h 50 50 m 60 50 l 60 60 l 50 60 l h",
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let content = pdf.add_object(Stream::new(
            dictionary! {},
            format!("{path} W n").into_bytes(),
        ));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! {}),
            None,
            None,
        );

        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(!outcome.is_complete(), "path: {path}");
        assert_eq!(outcome.issues().len(), 1);
        assert_eq!(outcome.issues()[0].kind(), ExtractionIssueKind::Unsupported);
        assert!(
            outcome.issues()[0]
                .description()
                .contains("non-rectangular clipping path")
        );
    }
    Ok(())
}

#[test]
fn ruled_two_column_pdf_reports_one_exact_cell_replacement() -> Result<()> {
    let old = extract(
        ruled_two_column_document("VersionTwoStable"),
        ExtractionLimits::default(),
    )?;
    let new = extract(
        ruled_two_column_document("VersionTenStable"),
        ExtractionLimits::default(),
    )?;
    assert_eq!(old.vector_lines().len(), 7);
    assert_eq!(new.vector_lines().len(), 7);

    let old_without_rules = Document::new(old.items().to_vec());
    let new_without_rules = Document::new(new.items().to_vec());
    let without_rules = compare_glyph_documents(
        &old_without_rules,
        &new_without_rules,
        PipelineOptions::default(),
    )?;
    assert!(without_rules.changes.is_empty(), "{without_rules:#?}");
    assert_eq!(
        without_rules.change_candidates.len(),
        1,
        "{without_rules:#?}"
    );
    let candidate = &without_rules.change_candidates[0];
    assert_eq!(candidate.change.kind, ChangeKind::Replacement);
    assert_eq!(candidate.change.confidence, Confidence::Medium);
    assert_eq!(
        candidate.change.occurrences[0].old_span,
        Some(TextSpan {
            blocks: vec![BlockId(4)],
            separator: None,
            canonical_range: ScalarRange { start: 8, end: 10 },
            comparable_range: TokenRange { start: 8, end: 10 },
        })
    );
    assert_eq!(
        candidate.change.occurrences[0].new_span,
        Some(TextSpan {
            blocks: vec![BlockId(4)],
            separator: None,
            canonical_range: ScalarRange { start: 8, end: 10 },
            comparable_range: TokenRange { start: 8, end: 10 },
        })
    );
    assert!(
        without_rules
            .assessment
            .as_ref()
            .expect("candidate assessment")
            .relations[candidate.relation]
            .reasons
            .contains(&pdfdelta_core::diff::AssessmentReason::UnknownReadingOrder),
        "{without_rules:#?}"
    );
    assert_eq!(
        without_rules.unresolved_regions.len(),
        6,
        "{without_rules:#?}"
    );
    assert!(without_rules.unresolved_regions.iter().all(|region| {
        region.evidence == [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    }));
    assert_eq!(without_rules.old_coverage.resolved_tokens, 14);
    assert_eq!(without_rules.old_coverage.total_tokens, 38);
    assert_eq!(without_rules.new_coverage.resolved_tokens, 14);
    assert_eq!(without_rules.new_coverage.total_tokens, 38);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert_eq!(comparison.changes[0].kind, ChangeKind::Replacement);
    assert!(comparison.unresolved_regions.is_empty());
    assert_eq!(comparison.old_coverage.ratio, Some(1.0));
    assert_eq!(comparison.new_coverage.ratio, Some(1.0));
    Ok(())
}

#[test]
fn vector_line_and_path_segment_limits_are_enforced() -> Result<()> {
    // Vector lines share an explicit extraction limit (`max_vector_lines`).
    // A stroked path that would emit more lines than the limit must be
    // reported as `LimitExceeded` rather than growing the `vector_lines`
    // buffer without bound.
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 w 0 0 m 10 0 l S 20 0 m 30 0 l S 40 0 m 50 0 l S".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_vector_lines: 2,
        ..ExtractionLimits::default()
    };
    let error = extract_outcome(pdf, limits).expect_err("vector line limit must be enforced");
    assert!(
        matches!(
            error,
            pdfdelta_core::Error::LimitExceeded { resource, .. } if resource == "vector line count"
        ),
        "{error:?}"
    );

    // Path segment count is bounded even before stroking. A single path with
    // many `l` operations must hit the same limit without emitting any
    // vector line.
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    // Build a single path with 3 segments (m + l + l + l) exceeding limit 2.
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"0 0 m 10 0 l 20 0 l 30 0 l n".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_vector_lines: 2,
        ..ExtractionLimits::default()
    };
    let error = extract_outcome(pdf, limits).expect_err("path segment limit must be enforced");
    assert!(
        matches!(
            error,
            pdfdelta_core::Error::LimitExceeded { resource, .. } if resource == "path segment count"
        ),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn rejects_zero_page_box_dimensions_after_normalization() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let content = pdf.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {}),
        Some([10, 120, 10, 20]),
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Unresolved(message))
            if message.contains("page box must have finite positive dimensions")
    ));
}

#[test]
fn preserves_text_state_across_ordered_content_streams() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let first = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 1 0 0 1 10 50 Tm (A) Tj".to_vec(),
    ));
    let second = pdf.add_object(Stream::new(dictionary! {}, b"(B) Tj ET".to_vec()));
    install_page(
        &mut pdf,
        Object::Array(vec![first.into(), second.into()]),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "AB");
    assert_eq!(glyphs[0].provenance.content_stream.object_number, first.0);
    assert_eq!(glyphs[1].provenance.content_stream.object_number, second.0);
    assert_close(glyphs[0].baseline.x, 10.0);
    assert_close(glyphs[1].baseline.x, 16.0);
    Ok(())
}

#[test]
fn carries_complete_operands_across_content_stream_boundaries() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let operands = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 1 0 0 1 10 50 Tm (A)".to_vec(),
    ));
    let operator = pdf.add_object(Stream::new(dictionary! {}, b"Tj ET".to_vec()));
    install_page(
        &mut pdf,
        Object::Array(vec![operands.into(), operator.into()]),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "A");
    assert_eq!(
        document.items()[0].provenance.content_stream.object_number,
        operator.0
    );
    assert_eq!(document.items()[0].provenance.operator_index, 0);
    Ok(())
}

#[test]
fn parses_tagged_content_dictionary_operands() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"/P << /MCID 0 >> BDC BT /F1 10 Tf 1 0 0 1 20 30 Tm (Tagged note) Tj ET EMC".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    use pdfdelta_core::document::{
        BackendIdentity, BackendKind, Channel, DocumentGraph, EvidenceLimits, EvidenceStore,
        GraphLimits, NodeContent, NodeKind, PageEvidence, SourceRef, StructureLimits,
        StructuredValue, extract_structure_evidence,
    };
    let page = pdf.get_pages()[&1];
    let root = pdf.new_object_id();
    let table = pdf.new_object_id();
    let row = pdf.new_object_id();
    let cell = pdf.add_object(
        dictionary! { "Type" => "StructElem", "S" => "TD", "P" => row, "Pg" => page, "K" => 0 },
    );
    pdf.objects.insert(
        row,
        Object::Dictionary(
            dictionary! { "Type" => "StructElem", "S" => "TR", "P" => table, "K" => cell },
        ),
    );
    pdf.objects.insert(
        table,
        Object::Dictionary(
            dictionary! { "Type" => "StructElem", "S" => "Table", "P" => root, "K" => row },
        ),
    );
    pdf.objects.insert(
        root,
        Object::Dictionary(dictionary! { "Type" => "StructTreeRoot", "K" => table }),
    );
    let catalog = pdf
        .trailer
        .get(b"Root")
        .expect("catalog")
        .as_reference()
        .expect("catalog reference");
    pdf.get_object_mut(catalog)
        .expect("catalog")
        .as_dict_mut()
        .expect("catalog dictionary")
        .set("StructTreeRoot", root);

    let document = extract(pdf.clone(), ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "Tagged note");
    let sequence = &document.marked_content()[0];
    assert_eq!(sequence.mcid, 0);
    assert_eq!(sequence.form, None);
    assert_eq!(sequence.glyph_range, 0..11);
    assert!(sequence.complete);
    for mcid in [0, 99] {
        pdf.get_object_mut(cell)
            .expect("cell")
            .as_dict_mut()
            .expect("cell dictionary")
            .set("K", mcid);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).expect("tagged PDF");
        let parsed = LopdfParser.parse(bytes.into(), ParseLimits::default())?;
        let outcome = ContentStreamGlyphExtractor
            .extract_outcome(parsed.as_ref(), ExtractionLimits::default())?;
        let mut store = EvidenceStore::from_native(
            "tagged-fixture".into(),
            BackendIdentity {
                kind: BackendKind::NativeParser,
                name: "fixture".into(),
                version: "1".into(),
                profile: "tags".into(),
                model: None,
            },
            vec![PageEvidence {
                page: pdfdelta_core::model::PageId(0),
                bounds: None,
            }],
            outcome,
            EvidenceLimits::default(),
        )?;
        let tags = extract_structure_evidence(
            parsed.as_ref(),
            &store.native,
            0,
            0,
            StructureLimits::default(),
        )?;
        assert_eq!(tags.elements.len(), 3);
        assert!(tags.key_inventory.complete);
        if mcid == 0 {
            let limited = extract_structure_evidence(
                parsed.as_ref(),
                &store.native,
                0,
                0,
                StructureLimits {
                    max_depth: 0,
                    ..StructureLimits::default()
                },
            )?;
            assert_eq!(limited.elements.len(), 1);
            assert!(!limited.key_inventory.complete);
            assert!(limited.issues.iter().any(|issue| issue.kind
                == pdfdelta_core::document::EvidenceFailure::ResourceLimit
                && issue.reason.contains("structure nesting depth")));
        }
        let StructuredValue::StructureElement { glyphs, .. } = &tags.elements[2].value else {
            panic!("tagged cell");
        };
        assert_eq!(glyphs.len(), if mcid == 0 { 11 } else { 0 });
        store.structured = tags.elements;
        store.issues.extend(tags.issues);
        store.inventories.push(tags.inventory);
        store.key_inventories.push(tags.key_inventory);
        let graph = DocumentGraph::from_evidence(
            &store,
            PipelineOptions::default(),
            EvidenceLimits::default(),
            GraphLimits::default(),
        )?;
        let cell = graph
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Cell)
            .expect("cell view");
        if mcid == 0 {
            let NodeContent::Text { view } = &cell.content else {
                panic!("native cell text");
            };
            assert_eq!(view.tokens.len(), 11);
            assert!(view.origins.iter().all(|origins| {
                origins
                    .iter()
                    .any(|source| matches!(source, SourceRef::Native { .. }))
                    && origins
                        .iter()
                        .any(|source| matches!(source, SourceRef::Structured { .. }))
            }));
            assert_eq!(
                cell.sources
                    .iter()
                    .filter(|source| matches!(source, SourceRef::Native { .. }))
                    .count(),
                11
            );
            // A tag spanning pages has no single page in the evidence store.
            // Its glyph membership still establishes all contributing pages.
            let mut glyphs = store.native.items().to_vec();
            let mut extra = glyphs[0].clone();
            extra.id = pdfdelta_core::model::GlyphId(1000);
            extra.page = pdfdelta_core::model::PageId(1);
            let extra_id = extra.id;
            glyphs.push(extra);
            store.native = pdfdelta_core::model::Document::new(glyphs);
            store.pages.push(PageEvidence {
                page: pdfdelta_core::model::PageId(1),
                bounds: None,
            });
            let StructuredValue::StructureElement {
                glyphs, content, ..
            } = &mut store.structured[2].value
            else {
                unreachable!()
            };
            // This constructed replacement has no acquired marked-content inventory.
            *content = None;
            glyphs.push(extra_id);
            store.structured[2].page = None;
            let graph = DocumentGraph::from_evidence(
                &store,
                PipelineOptions::default(),
                EvidenceLimits::default(),
                GraphLimits::default(),
            )?;
            let cell = graph
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Cell)
                .expect("tagged cell view");
            assert_eq!(
                cell.pages,
                vec![
                    pdfdelta_core::model::PageId(0),
                    pdfdelta_core::model::PageId(1)
                ]
            );
        } else {
            assert!(
                store
                    .issues
                    .iter()
                    .any(|issue| issue.reason.contains("marked content was not extracted"))
            );
        }
        assert_eq!(
            mapped_text(store.native.items()),
            if mcid == 0 {
                "Tagged noteT"
            } else {
                "Tagged note"
            }
        );
        assert!(!store.inventory_complete(None, Channel::Relations));
    }
    Ok(())
}

#[test]
fn mixed_structure_content_preserves_order_duplicates_and_failed_slots() -> Result<()> {
    use pdfdelta_core::document::{
        BackendIdentity, BackendKind, Channel, EvidenceLimits, EvidenceStore, NativeStructureKid,
        PageEvidence, StructureLimits, StructuredValue, extract_structure_evidence,
    };
    for fault in [
        "none",
        "duplicate",
        "object",
        "parent",
        "mcid",
        "depth",
        "malformed",
        "annotation",
        "annotation-page",
        "parents-duplicate",
        "parents-limits",
        "parents-cycle",
        "parents-budget",
        "parents-container",
        "parents-owner",
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let first = pdf.add_object(Stream::new(dictionary! {},
            b"BT /F1 10 Tf 1 0 0 1 20 30 Tm /P << /MCID 0 >> BDC (A) Tj EMC /Span << /MCID 1 >> BDC (B) Tj EMC ET".to_vec()));
        let second = pdf.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 10 Tf 1 0 0 1 20 30 Tm /P << /MCID 0 >> BDC (C) Tj EMC ET".to_vec(),
        ));
        install_plain_pages(
            &mut pdf,
            &[first.into(), second.into()],
            Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }),
        );
        let pages = pdf.get_pages();
        let root = pdf.new_object_id();
        let parent = pdf.new_object_id();
        let child = pdf.add_object(dictionary! {
            "S" => "Span", "P" => if fault == "parent" { root } else { parent },
            "K" => if fault == "mcid" { 99 } else { 1 },
        });
        let cross_page = Object::Dictionary(dictionary! {
            "Type" => "MCR", "Pg" => pages[&2], "MCID" => 0,
        });
        let mut kids = vec![Object::Integer(0), child.into(), cross_page.clone()];
        if fault == "duplicate" {
            kids.push(cross_page);
        }
        if fault == "malformed" {
            kids.insert(2, Object::Dictionary(dictionary! { "Type" => "MCR" }));
        }
        if fault == "object" {
            kids.push(Object::Dictionary(dictionary! { "Type" => "OBJR" }));
        }
        let annotation = pdf.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Link", "P" => pages[&1],
        });
        if matches!(fault, "annotation" | "annotation-page") {
            kids.push(Object::Dictionary(dictionary! {
                "Type" => "OBJR", "Obj" => annotation,
                "Pg" => if fault == "annotation-page" { pages[&2] } else { pages[&1] },
            }));
        }
        pdf.objects.insert(
            parent,
            Object::Dictionary(dictionary! {
                "S" => "P", "P" => root, "Pg" => pages[&1], "K" => kids,
            }),
        );
        pdf.objects.insert(
            root,
            Object::Dictionary(dictionary! { "Type" => "StructTreeRoot", "K" => parent }),
        );
        for (number, page) in &pages {
            pdf.get_object_mut(*page)
                .expect("fixture page")
                .as_dict_mut()
                .expect("page dictionary")
                .set(
                    "StructParents",
                    if fault == "parents-container" {
                        0
                    } else {
                        i64::from(*number) - 1
                    },
                );
        }
        let first_parents = pdf.add_object(dictionary! {
            "Limits" => vec![Object::Integer(0), Object::Integer(if fault == "parents-limits" { 1 } else { 0 })],
            "Nums" => vec![Object::Integer(0), Object::Array(vec![
                if fault == "parents-owner" { root.into() } else { parent.into() }, child.into(),
            ])],
        });
        let second_key = if fault == "parents-duplicate" { 0 } else { 1 };
        let second_parents = pdf.add_object(dictionary! {
            "Limits" => vec![Object::Integer(second_key), Object::Integer(second_key)],
            "Nums" => vec![Object::Integer(second_key), Object::Array(vec![parent.into()])],
        });
        let parent_tree = pdf.new_object_id();
        pdf.objects.insert(parent_tree, Object::Dictionary(dictionary! {
            "Kids" => if fault == "parents-cycle" { vec![Object::Reference(parent_tree)] }
                else { vec![Object::Reference(first_parents), Object::Reference(second_parents)] },
        }));
        pdf.get_object_mut(root)
            .expect("structure root")
            .as_dict_mut()
            .expect("root dictionary")
            .set("ParentTree", parent_tree);
        let catalog = pdf
            .trailer
            .get(b"Root")
            .expect("fixture catalog")
            .as_reference()
            .expect("catalog reference");
        pdf.get_object_mut(catalog)
            .expect("fixture catalog object")
            .as_dict_mut()
            .expect("fixture catalog dictionary")
            .set("StructTreeRoot", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).expect("mixed fixture bytes");
        let parsed = LopdfParser.parse(bytes.into(), ParseLimits::default())?;
        let outcome = ContentStreamGlyphExtractor
            .extract_outcome(parsed.as_ref(), ExtractionLimits::default())?;
        let mut store = EvidenceStore::from_native(
            "mixed-tags".into(),
            BackendIdentity {
                kind: BackendKind::NativeParser,
                name: "fixture".into(),
                version: "1".into(),
                profile: "mixed-tags".into(),
                model: None,
            },
            (0..2)
                .map(|page| PageEvidence {
                    page: PageId(page),
                    bounds: None,
                })
                .collect(),
            outcome,
            EvidenceLimits::default(),
        )?;
        let tags = extract_structure_evidence(
            parsed.as_ref(),
            &store.native,
            0,
            37,
            StructureLimits {
                max_depth: if fault == "depth" { 0 } else { 64 },
                max_nodes: if fault == "parents-budget" {
                    10
                } else {
                    100_000
                },
                ..StructureLimits::default()
            },
        )?;
        let StructuredValue::StructureElement {
            glyphs,
            content: Some(content),
            ..
        } = &tags.elements[0].value
        else {
            panic!("retained mixed content")
        };
        // The legacy flat view remains unproved; ordered raw membership survives.
        assert!(glyphs.is_empty());
        assert_eq!(
            content[0],
            NativeStructureKid::MarkedContent { sequence: 0 }
        );
        assert_eq!(
            content[if fault == "malformed" { 3 } else { 2 }],
            NativeStructureKid::MarkedContent { sequence: 2 }
        );
        if fault == "malformed" {
            assert_eq!(content[2], NativeStructureKid::Unresolved);
        }
        assert_eq!(
            content[1],
            if matches!(fault, "parent" | "depth") {
                NativeStructureKid::Unresolved
            } else {
                NativeStructureKid::Element { element: 38 }
            }
        );
        if fault == "duplicate" {
            assert_eq!(content[3], content[2]);
        }
        if fault == "object" {
            assert_eq!(content[3], NativeStructureKid::Unresolved);
        }
        if fault == "annotation-page" {
            assert_eq!(content[3], NativeStructureKid::Unresolved);
        }
        if fault == "annotation" {
            assert_eq!(
                content[3],
                NativeStructureKid::Annotation {
                    object: pdfdelta_core::pdf::ObjectRef {
                        object_number: annotation.0,
                        generation: annotation.1
                    },
                    page: PageId(0),
                }
            );
        }
        if fault == "mcid" {
            let StructuredValue::StructureElement {
                content: Some(content),
                ..
            } = &tags.elements[1].value
            else {
                panic!("failed binding")
            };
            assert_eq!(content, &[NativeStructureKid::Unresolved]);
        }
        assert_eq!(
            tags.native_inventory.complete,
            !matches!(
                fault,
                "object" | "parent" | "mcid" | "depth" | "malformed" | "annotation-page"
            ),
            "{fault}"
        );
        if matches!(
            fault,
            "parents-duplicate"
                | "parents-limits"
                | "parents-cycle"
                | "parents-budget"
                | "parents-container"
                | "depth"
        ) {
            assert!(tags.native_inventory.parents.is_none(), "{fault}");
        } else {
            let bindings = tags
                .native_inventory
                .parents
                .as_ref()
                .expect("closed parent lookup");
            assert_eq!(bindings.len(), 3);
            assert_eq!(bindings[0].sequence, 0);
            assert_eq!(
                bindings[0].owner.object_number,
                if fault == "parents-owner" {
                    root.0
                } else {
                    parent.0
                }
            );
            assert_eq!(bindings[1].owner.object_number, child.0);
            assert_eq!(bindings[2].owner.object_number, parent.0);
        }
        store.native_structures.push(tags.native_inventory);
        store.structured = tags.elements;
        store.issues.extend(tags.issues);
        store.inventories.push(tags.inventory);
        store.key_inventories.push(tags.key_inventory);
        store.validate(EvidenceLimits::default())?;
        assert!(!store.inventory_complete(None, Channel::Relations));
        let restored: EvidenceStore =
            serde_json::from_slice(&serde_json::to_vec(&store).expect("serialize mixed evidence"))
                .expect("deserialize mixed evidence");
        assert_eq!(store, restored);
        if fault == "none" {
            for mutation in [
                "missing-root",
                "repeated-root",
                "repeated-parent",
                "parent-sequence",
            ] {
                let mut bad = store.clone();
                let inventory = &mut bad.native_structures[0];
                match mutation {
                    "missing-root" => inventory.roots.clear(),
                    "repeated-root" => inventory.roots.push(inventory.roots[0]),
                    "repeated-parent" => {
                        let parents = inventory.parents.as_mut().expect("parent bindings");
                        parents.push(parents[0].clone());
                    }
                    "parent-sequence" => {
                        inventory.parents.as_mut().expect("parent bindings")[0].sequence =
                            usize::MAX;
                    }
                    _ => unreachable!(),
                }
                assert!(
                    bad.validate(EvidenceLimits::default()).is_err(),
                    "{mutation}"
                );
            }
            for mutation in ["sequence", "omission", "order", "child", "flat"] {
                let mut bad = store.clone();
                let StructuredValue::StructureElement {
                    content: Some(content),
                    glyphs,
                    ..
                } = &mut bad.structured[0].value
                else {
                    unreachable!()
                };
                match mutation {
                    "sequence" => {
                        content[0] = NativeStructureKid::MarkedContent {
                            sequence: usize::MAX,
                        }
                    }
                    "omission" => content[1] = NativeStructureKid::Unresolved,
                    "order" => content.swap(0, 1),
                    "child" => content[1] = NativeStructureKid::Element { element: u64::MAX },
                    "flat" => glyphs.push(bad.native.items()[0].id),
                    _ => unreachable!(),
                }
                assert!(
                    bad.validate(EvidenceLimits::default()).is_err(),
                    "{mutation}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn incomplete_marked_content_preserves_native_glyphs() -> Result<()> {
    for suffix in ["", "1 EMC"] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let content = pdf.add_object(Stream::new(
            dictionary! {},
            format!("/P << /MCID 7 >> BDC BT /F1 10 Tf (Keep) Tj ET {suffix}").into_bytes(),
        ));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }),
            None,
            None,
        );
        let document = extract(pdf, ExtractionLimits::default())?;
        assert_eq!(mapped_text(document.items()), "Keep");
        assert_eq!(document.marked_content()[0].glyph_range, 0..4);
        assert!(!document.marked_content()[0].complete);
    }
    Ok(())
}

#[test]
fn form_marked_content_uses_its_own_structural_parent_key() -> Result<()> {
    use pdfdelta_core::document::{
        NativeStructureKid, StructuredValue, extract_structure_evidence,
    };
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "StructParents" => 7,
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        },
        b"/Span << /MCID 0 >> BDC BT /F1 10 Tf (A) Tj ET EMC".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X Do".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X" => form },
        }),
        None,
        None,
    );
    let page = pdf.get_pages()[&1];
    let root = pdf.new_object_id();
    let owner = pdf.add_object(dictionary! {
        "S" => "Span", "P" => root, "Pg" => page,
        "K" => dictionary! { "Type" => "MCR", "Stm" => form, "MCID" => 0 },
    });
    pdf.objects.insert(
        root,
        Object::Dictionary(dictionary! {
            "Type" => "StructTreeRoot", "K" => owner,
            "ParentTree" => dictionary! { "Nums" => vec![
                Object::Integer(7), Object::Array(vec![owner.into()]),
            ] },
        }),
    );
    let catalog = pdf
        .trailer
        .get(b"Root")
        .expect("catalog")
        .as_reference()
        .expect("reference");
    pdf.get_object_mut(catalog)
        .expect("catalog object")
        .as_dict_mut()
        .expect("dictionary")
        .set("StructTreeRoot", root);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).expect("form fixture bytes");
    let parsed = LopdfParser.parse(bytes.into(), ParseLimits::default())?;
    let native =
        ContentStreamGlyphExtractor.extract(parsed.as_ref(), ExtractionLimits::default())?;
    assert_eq!(mapped_text(native.items()), "A");
    let tags = extract_structure_evidence(parsed.as_ref(), &native, 0, 0, Default::default())?;
    assert!(tags.native_inventory.complete);
    let parents = tags.native_inventory.parents.expect("form parent lookup");
    assert_eq!(parents.len(), 1);
    assert_eq!(parents[0].sequence, 0);
    assert_eq!(parents[0].owner.object_number, owner.0);
    let StructuredValue::StructureElement {
        content: Some(content),
        ..
    } = &tags.elements[0].value
    else {
        panic!("form membership")
    };
    assert_eq!(
        content,
        &[NativeStructureKid::MarkedContent { sequence: 0 }]
    );
    Ok(())
}

#[test]
fn repeated_form_marked_content_retains_each_invocation() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(dictionary! {
        "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
    }, b"/Span << /MCID 0 >> BDC BT /F1 10 Tf (A) Tj ET EMC".to_vec()));
    let contents = pdf.add_object(Stream::new(
        dictionary! {},
        b"/P << /MCID 0 >> BDC /X Do /X Do EMC".to_vec(),
    ));
    install_page(
        &mut pdf,
        contents.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X" => form },
        }),
        None,
        None,
    );
    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "AA");
    let sequences = document.marked_content();
    assert_eq!(sequences.len(), 3);
    assert_eq!(sequences[0].form, None);
    assert_eq!(sequences[0].glyph_range, 0..2);
    assert_eq!(
        sequences[1].form,
        Some(pdfdelta_core::pdf::ObjectRef {
            object_number: form.0,
            generation: form.1
        })
    );
    assert_eq!(sequences[1].glyph_range, 0..1);
    assert_eq!(sequences[2].form, sequences[1].form);
    assert_eq!(sequences[2].glyph_range, 1..2);
    assert!(sequences.iter().all(|sequence| sequence.complete));
    Ok(())
}

#[test]
fn form_box_intersects_caller_clip_and_preserves_raw_glyphs() -> Result<()> {
    for (clip, expected) in [
        (
            "",
            [
                GlyphPathClipStatus::Inside,
                GlyphPathClipStatus::PartiallyOutside,
                GlyphPathClipStatus::Outside,
            ],
        ),
        (
            "0 0 30 100 re W n",
            [
                GlyphPathClipStatus::Inside,
                GlyphPathClipStatus::Outside,
                GlyphPathClipStatus::Outside,
            ],
        ),
        ("0 0 0 100 re W n", [GlyphPathClipStatus::Outside; 3]),
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let form = pdf.add_object(Stream::new(dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![40.into(), 40.into(), 0.into(), 0.into()],
            "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 0.into(), 0.into()],
        }, b"q BT /F1 10 Tf 1 0 0 1 10 20 Tm (I) Tj 1 0 0 1 38 20 Tm (P) Tj 1 0 0 1 50 20 Tm (O) Tj ET Q".to_vec()));
        let contents = pdf.add_object(Stream::new(
            dictionary! {},
            format!("q 1 0 0 1 10 20 cm {clip} /X Do Q BT /F1 10 Tf 1 0 0 1 150 20 Tm (C) Tj ET")
                .into_bytes(),
        ));
        install_page(
            &mut pdf,
            contents.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "X" => form },
            }),
            None,
            None,
        );
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(outcome.is_complete());
        let glyphs = outcome.document().items();
        assert_eq!(mapped_text(glyphs), "IPOC");
        assert_eq!(
            glyphs[..3]
                .iter()
                .map(|g| g.path_clip_status)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(glyphs[3].path_clip_status, GlyphPathClipStatus::Unclipped);
        assert_close(glyphs[0].baseline.x, 30.0);
        assert_close(glyphs[0].baseline.y, 60.0);
        assert_eq!(glyphs[0].raw_code, b"I");
        assert_eq!(glyphs[0].provenance.content_stream.object_number, form.0);
    }
    Ok(())
}

#[test]
fn nested_form_box_cannot_expand_the_enclosing_form_clip() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let inner = pdf.add_object(Stream::new(dictionary! {
        "Type" => "XObject", "Subtype" => "Form",
        "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
    }, b"q 0 0 200 200 re W n BT /F1 10 Tf 1 0 0 1 10 20 Tm (I) Tj 1 0 0 1 58 20 Tm (P) Tj 1 0 0 1 80 20 Tm (O) Tj ET Q".to_vec()));
    let outer = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 60.into(), 80.into()],
        },
        b"/Inner Do".to_vec(),
    ));
    let contents = pdf.add_object(Stream::new(dictionary! {}, b"/Outer Do /Inner Do".to_vec()));
    install_page(
        &mut pdf,
        contents.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "Inner" => inner, "Outer" => outer },
        }),
        None,
        None,
    );
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(outcome.is_complete());
    assert_eq!(mapped_text(outcome.document().items()), "IPOIPO");
    assert_eq!(
        outcome
            .document()
            .items()
            .iter()
            .map(|g| g.path_clip_status)
            .collect::<Vec<_>>(),
        [
            GlyphPathClipStatus::Inside,
            GlyphPathClipStatus::PartiallyOutside,
            GlyphPathClipStatus::Outside,
            GlyphPathClipStatus::Inside,
            GlyphPathClipStatus::Inside,
            GlyphPathClipStatus::Inside,
        ]
    );
    Ok(())
}

#[test]
fn unsupported_form_boxes_keep_an_extraction_gap() -> Result<()> {
    for (bbox, matrix) in [
        (None, [1, 0, 0, 1, 0, 0]),
        (Some([0, 0, 40, 40]), [1, 1, 1, 1, 0, 0]),
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let mut dict = dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "Matrix" => matrix.into_iter().map(Object::Integer).collect::<Vec<_>>(),
        };
        if let Some(bbox) = bbox {
            dict.set(
                "BBox",
                bbox.into_iter().map(Object::Integer).collect::<Vec<_>>(),
            );
        }
        let form = pdf.add_object(Stream::new(
            dict,
            b"BT /F1 10 Tf 10 20 Td (F) Tj ET".to_vec(),
        ));
        let contents = pdf.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 10 Tf 10 20 Td (A) Tj ET /X Do BT /F1 10 Tf 20 20 Td (B) Tj ET".to_vec(),
        ));
        install_page(
            &mut pdf,
            contents.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "X" => form },
            }),
            None,
            None,
        );
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(!outcome.is_complete());
        assert_eq!(mapped_text(outcome.document().items()), "AB");
        assert_eq!(outcome.issues().len(), 1);
        assert!(matches!(
            outcome.issues()[0].scope(),
            ExtractionScope::PageGlyphGap {
                retained_before: 1,
                ..
            }
        ));
    }
    Ok(())
}

#[test]
fn applies_form_matrix_and_form_resources() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 3.into(), 4.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
            },
        },
        b"BT /F1 10 Tf 1 0 0 1 5 6 Tm (Form) Tj ET".to_vec(),
    ));
    let page_content = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 1 0 0 1 10 20 cm /X1 Do Q".to_vec(),
    ));
    install_page(
        &mut pdf,
        page_content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "Form");
    assert_eq!(glyphs[0].provenance.content_stream.object_number, form.0);
    assert_close(glyphs[0].baseline.x, 23.0);
    assert_close(glyphs[0].baseline.y, 36.0);
    assert_close(glyphs[0].font_size, 20.0);
    Ok(())
}

#[test]
fn discards_residual_form_graphics_saves_without_changing_caller_state() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        },
        b"q 2 0 0 2 50 0 cm BT /F1 10 Tf 1 0 0 1 0 20 Tm (A) Tj ET".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"/X1 Do BT /F1 10 Tf 1 0 0 1 10 20 Tm (B) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "AB");
    assert_close(glyphs[0].baseline.x, 50.0);
    assert_close(glyphs[0].baseline.y, 40.0);
    assert_close(glyphs[0].font_size, 20.0);
    assert_close(glyphs[1].baseline.x, 10.0);
    assert_close(glyphs[1].baseline.y, 20.0);
    assert_close(glyphs[1].font_size, 10.0);
    Ok(())
}

#[test]
fn keeps_form_graphics_stack_underflow_unresolved() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"Q".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Unresolved(message))
            if message.contains("content operator Q")
                && message.contains("graphics-state stack underflow")
    ));
}

#[test]
fn failed_form_bounds_use_the_declared_box_and_full_transform() -> Result<()> {
    for bbox in [Some([10, 20, 30, 40]), None, Some([30, 20, 10, 40])] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let mut dictionary = dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "Matrix" => vec![2.into(), 1.into(), 1.into(), 3.into(), 4.into(), 5.into()],
        };
        if let Some(bbox) = bbox {
            dictionary.set(
                "BBox",
                bbox.into_iter().map(Object::Integer).collect::<Vec<_>>(),
            );
        }
        let form = pdf.add_object(Stream::new(
            dictionary,
            b"0 0 1 1 re f UnsupportedOperator".to_vec(),
        ));
        let content = pdf.add_object(Stream::new(
            dictionary! {},
            b"0 0 2 2 re f BT /F1 10 Tf 10 20 Td (A) Tj ET \
              q 0 1 -1 0 100 50 cm /X Do Q BT /F1 10 Tf 20 20 Td (B) Tj ET"
                .to_vec(),
        ));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font },
                "XObject" => dictionary! { "X" => form },
            }),
            None,
            None,
        );
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert_eq!(mapped_text(outcome.document().items()), "AB");
        let bounded = bbox == Some([10, 20, 30, 40]);
        assert_eq!(
            outcome.issues()[0].scope(),
            ExtractionScope::PageGlyphGap {
                page: PageId(0),
                retained_before: 1,
                paint_index: bounded.then_some(1),
            }
        );
        let paints = outcome
            .document()
            .non_text_paint_bounds()
            .expect("paint inventory");
        assert_eq!(
            paints.len(),
            2,
            "partial Form paints are replaced by the opaque invocation"
        );
        assert_eq!(paints[1].content_stream.object_number, content.0);
        assert_eq!(paints[1].render_order, 1);
        if bounded {
            let bounds = paints[1].bounds.expect("finite transformed Form box");
            for (actual, expected, lower) in [
                (bounds.min.x, -55.0, true),
                (bounds.min.y, 94.0, true),
                (bounds.max.x, 25.0, false),
                (bounds.max.y, 154.0, false),
            ] {
                assert!((actual - expected).abs() < 1e-9);
                assert!(if lower {
                    actual <= expected
                } else {
                    actual >= expected
                });
            }
        } else {
            assert!(paints[1].bounds.is_none());
        }
    }
    Ok(())
}

#[test]
fn localizes_a_recoverable_form_failure_between_retained_page_text() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"Q".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (A) Tj ET /X1 Do \
          BT /F1 10 Tf 1 0 0 1 20 20 Tm (B) Tj ET"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(outcome.document().items()), "AB");
    assert_eq!(outcome.issues().len(), 1);
    assert_eq!(
        outcome.issues()[0].scope(),
        ExtractionScope::PageGlyphGap {
            page: PageId(0),
            retained_before: 1,
            paint_index: Some(0),
        }
    );
    assert_eq!(outcome.issues()[0].kind(), ExtractionIssueKind::Unresolved);
    let before = outcome.document().items()[0].id;
    let after = outcome.document().items()[1].id;
    let store = pdfdelta_core::document::EvidenceStore::from_native(
        "failed-form-gap".into(),
        pdfdelta_core::document::BackendIdentity {
            kind: pdfdelta_core::document::BackendKind::NativeParser,
            name: "native".into(),
            version: "fixture".into(),
            profile: "raw".into(),
            model: None,
        },
        vec![pdfdelta_core::document::PageEvidence {
            page: PageId(0),
            bounds: None,
        }],
        outcome,
        Default::default(),
    )?;
    assert_eq!(
        store.issues[0].boundary,
        Some(pdfdelta_core::document::EvidenceBoundary::PageGlyphGap {
            page: PageId(0),
            retained_before: 1,
            before: Some(before),
            after: Some(after),
            paint_index: Some(0),
        })
    );
    Ok(())
}

#[test]
fn failed_form_at_a_page_edge_does_not_poison_other_page_inventories() -> Result<()> {
    use pdfdelta_core::document::{
        BackendIdentity, BackendKind, Channel, EvidenceBoundary, EvidenceStore, PageEvidence,
    };

    for prefix in ["", "BT /F1 10 Tf 10 20 Td (A) Tj ET "] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let form = pdf.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            },
            b"UnsupportedOperator".to_vec(),
        ));
        let failed = pdf.add_object(Stream::new(
            dictionary! {},
            format!("{prefix}/Bad Do").into_bytes(),
        ));
        let unaffected = pdf.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 10 Tf 10 20 Td (B) Tj ET".to_vec(),
        ));
        install_plain_pages(
            &mut pdf,
            &[failed.into(), unaffected.into()],
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "Bad" => form },
            }),
        );
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        let retained_before = usize::from(!prefix.is_empty());
        assert_eq!(
            outcome.issues()[0].scope(),
            ExtractionScope::PageGlyphGap {
                page: PageId(0),
                retained_before,
                paint_index: Some(0),
            }
        );
        let before = retained_before
            .checked_sub(1)
            .map(|i| outcome.document().items()[i].id);
        let after = Some(outcome.document().items()[retained_before].id);
        let mut store = EvidenceStore::from_native(
            "page-edge-form".into(),
            BackendIdentity {
                kind: BackendKind::NativeParser,
                name: "fixture".into(),
                version: "1".into(),
                profile: "raw".into(),
                model: None,
            },
            vec![
                PageEvidence {
                    page: PageId(0),
                    bounds: None,
                },
                PageEvidence {
                    page: PageId(1),
                    bounds: None,
                },
            ],
            outcome,
            Default::default(),
        )?;
        assert_eq!(
            store.issues[0].boundary,
            Some(EvidenceBoundary::PageGlyphGap {
                page: PageId(0),
                retained_before,
                before,
                after,
                paint_index: Some(0),
            })
        );
        assert!(!store.inventory_complete(None, Channel::Text));
        assert!(!store.inventory_complete(Some(PageId(0)), Channel::Text));
        assert!(store.inventory_complete(Some(PageId(1)), Channel::Text));
        let roundtrip: EvidenceStore =
            serde_json::from_slice(&serde_json::to_vec(&store).expect("serialize"))
                .expect("deserialize");
        roundtrip.validate(Default::default())?;
        store.issues[0].page = Some(PageId(1));
        assert!(store.validate(Default::default()).is_err());
    }
    Ok(())
}

#[test]
fn rolls_back_partially_emitted_form_glyphs_and_render_order() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (X) Tj ET UnsupportedOperator".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (A) Tj ET /X1 Do \
          BT /F1 10 Tf 1 0 0 1 20 20 Tm (B) Tj ET"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(outcome.document().items()), "AB");
    assert_eq!(outcome.document().items()[1].id.0, 1);
    assert_eq!(outcome.document().items()[1].render_order, 1);
    assert_eq!(outcome.issues().len(), 1);
    assert_eq!(outcome.issues()[0].kind(), ExtractionIssueKind::Unsupported);
    Ok(())
}

#[test]
fn discards_localized_form_issues_when_the_enclosing_page_fails() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"Q".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do Q".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(outcome.document().items().is_empty());
    assert_eq!(outcome.issues().len(), 1);
    assert_eq!(
        outcome.issues()[0].scope(),
        ExtractionScope::Page(PageId(0))
    );
    Ok(())
}

#[test]
fn enclosing_form_failure_discards_nested_gap_and_preserves_page_suffix() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let inner = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"Q".to_vec(),
    ));
    let outer = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (X) Tj ET /Inner Do UnsupportedOperator".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (A) Tj ET /Outer Do \
          BT /F1 10 Tf 1 0 0 1 20 20 Tm (B) Tj ET"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "Inner" => inner, "Outer" => outer },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(outcome.document().items()), "AB");
    assert_eq!(outcome.issues().len(), 1);
    assert_eq!(
        outcome.issues()[0].scope(),
        ExtractionScope::PageGlyphGap {
            page: PageId(0),
            retained_before: 1,
            paint_index: Some(0),
        }
    );
    assert_eq!(outcome.issues()[0].kind(), ExtractionIssueKind::Unsupported);
    Ok(())
}

#[test]
fn repeated_failing_forms_do_not_refund_the_glyph_budget() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (X) Tj ET UnsupportedOperator".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do /X1 Do".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract_outcome(
            pdf,
            ExtractionLimits {
                max_glyphs: 1,
                ..ExtractionLimits::default()
            }
        ),
        Err(Error::LimitExceeded { .. })
    ));
}

#[test]
fn keeps_page_graphics_stack_imbalance_unresolved() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let content = pdf.add_object(Stream::new(dictionary! {}, b"q".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {}),
        None,
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Unresolved(message))
            if message.contains("page")
                && message.contains("unbalanced graphics-state stack")
    ));
}

#[test]
fn keeps_inherited_font_bound_to_its_selection_scope_inside_a_form() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let page_font = base_font(&mut pdf);
    let shadowing_form_font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => shadowing_form_font },
            },
        },
        b"BT 1 0 0 1 20 30 Tm (B) Tj ET".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"/F1 10 Tf BT 1 0 0 1 10 30 Tm (A) Tj ET /X1 Do".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "AB");
    assert_eq!(document.items()[0].font_id, document.items()[1].font_id);
    Ok(())
}

#[test]
fn shares_a_direct_font_decoder_across_category_map_aliases() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font_reference = base_font(&mut pdf);
    let direct_font = pdf
        .objects
        .remove(&font_reference)
        .expect("fixture font should exist");
    let font_map = pdf.add_object(dictionary! { "F1" => direct_font });
    let first_font_alias = pdf.add_object(Object::Reference(font_map));
    let second_font_alias = pdf.add_object(Object::Reference(font_map));
    let first_form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! { "Font" => first_font_alias },
        },
        b"BT /F1 10 Tf 1 0 0 1 10 30 Tm (A) Tj ET".to_vec(),
    ));
    let second_form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! { "Font" => second_font_alias },
        },
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (B) Tj ET".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do /X2 Do".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! {
                "X1" => first_form,
                "X2" => second_form,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "AB");
    assert_eq!(document.items()[0].font_id, document.items()[1].font_id);
    Ok(())
}

#[test]
fn shares_an_indirect_font_across_font_entry_aliases() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let first_font_alias = pdf.add_object(Object::Reference(font));
    let second_font_alias = pdf.add_object(Object::Reference(font));
    let first_form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => first_font_alias },
            },
        },
        b"BT /F1 10 Tf 1 0 0 1 10 30 Tm (A) Tj ET".to_vec(),
    ));
    let second_form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => second_font_alias },
            },
        },
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (B) Tj ET".to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do /X2 Do".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! {
                "X1" => first_form,
                "X2" => second_form,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "AB");
    assert_eq!(document.items()[0].font_id, document.items()[1].font_id);
    Ok(())
}

#[test]
fn applies_text_spacing_rise_render_mode_and_tj_adjustments() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"/F1 10 Tf 2 Tc 4 Tw 50 Tz 1 Ts 3 Tr \
          BT 1 0 0 1 10 50 Tm [(A) 200 ( ) -100 (B)] TJ ET"
            .to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "A B");
    let expected = PrimitiveExtractionSnapshot::new(vec![
        SnapshotGlyph::mapped(
            "A",
            PageId(0),
            0,
            Rect {
                min: Vec2 { x: 10.0, y: 49.0 },
                max: Vec2 { x: 12.5, y: 59.0 },
            },
            Vec2 { x: 10.0, y: 51.0 },
            Vec2 { x: 1.0, y: 0.0 },
        ),
        SnapshotGlyph::mapped(
            " ",
            PageId(0),
            1,
            Rect {
                min: Vec2 { x: 12.5, y: 49.0 },
                max: Vec2 { x: 15.0, y: 59.0 },
            },
            Vec2 { x: 12.5, y: 51.0 },
            Vec2 { x: 1.0, y: 0.0 },
        ),
        SnapshotGlyph::mapped(
            "B",
            PageId(0),
            2,
            Rect {
                min: Vec2 { x: 18.5, y: 49.0 },
                max: Vec2 { x: 21.0, y: 59.0 },
            },
            Vec2 { x: 18.5, y: 51.0 },
            Vec2 { x: 1.0, y: 0.0 },
        ),
    ]);
    compare_snapshots(
        &expected,
        &PrimitiveExtractionSnapshot::from(&document),
        GeometryTolerance::new(1e-9).expect("valid tolerance"),
    )
    .expect("text-state extraction should match the geometry oracle");
    assert!(
        glyphs
            .iter()
            .all(|glyph| glyph.render_mode == pdfdelta_core::model::TextRenderMode::Invisible)
    );
    Ok(())
}

#[test]
fn applies_font_selection_from_an_extgstate() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"/GS1 gs BT 1 0 0 1 10 20 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "ExtGState" => dictionary! {
                "GS1" => dictionary! {
                    "Type" => "ExtGState",
                    "Font" => vec![font.into(), 14.into()],
                },
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "A");
    assert_close(document.items()[0].font_size, 14.0);
    Ok(())
}

#[test]
fn decodes_japanese_only_where_tounicode_behavior_is_required() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <00> <FF> endcodespacerange\n\
          1 beginbfchar <01> <65E5> endbfchar"
            .to_vec(),
    ));
    let font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("ToUnicode", cmap);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <01> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "日");
    assert_eq!(document.items()[0].raw_code, [1]);
    Ok(())
}

#[test]
fn emits_stable_unmapped_tokens_from_decoded_embedded_font_programs() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let first_font = embedded_simple_font(&mut pdf, b"same decoded font", false, None);
    let second_font = embedded_simple_font(&mut pdf, b"same decoded font", true, None);
    let third_font = embedded_simple_font(&mut pdf, b"different decoded font", false, None);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj /F2 10 Tf 1 0 0 1 30 30 Tm (A) Tj /F3 10 Tf 1 0 0 1 40 30 Tm (B) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => first_font,
                "F2" => second_font,
                "F3" => third_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    let tokens = glyphs
        .iter()
        .map(|glyph| match &glyph.text {
            DecodedText::Unmapped {
                font_hash,
                glyph_id,
            } => Ok((font_hash, *glyph_id)),
            DecodedText::Mapped(_) => Err(Error::Unresolved(
                "fixture glyph should remain unmapped".into(),
            )),
        })
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(tokens[0].0.0.len(), 32);
    assert_eq!(tokens[0].0, tokens[1].0);
    assert_ne!(tokens[0].0, tokens[2].0);
    assert_eq!(
        tokens.iter().map(|token| token.1).collect::<Vec<_>>(),
        [65, 65, 66]
    );
    assert_eq!(glyphs[0].raw_code, b"A");
    assert_eq!(glyphs[2].raw_code, b"B");
    assert_close(glyphs[0].baseline.x, 20.0);
    assert_close(glyphs[0].bbox.min.y, 28.0);
    assert_close(glyphs[0].bbox.max.y, 38.0);
    assert_close(glyphs[0].bbox.max.x, 26.0);
    assert_eq!(glyphs[0].provenance.content_stream.object_number, content.0);
    assert_eq!(glyphs[0].provenance.content_stream.generation, content.1);
    assert_eq!(glyphs[0].render_order, 0);
    Ok(())
}

#[test]
fn preserves_partial_tounicode_gaps_as_embedded_font_tokens() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <00> <FF> endcodespacerange \
          1 beginbfchar <41> <005A> endbfchar"
            .to_vec(),
    ));
    let font = embedded_simple_font(&mut pdf, b"partial cmap font", false, Some(cmap));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AB) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs[0].text, DecodedText::Mapped("Z".into()));
    assert!(matches!(
        &glyphs[1].text,
        DecodedText::Unmapped {
            font_hash,
            glyph_id: 66,
        } if font_hash.0.len() == 32
    ));
    assert_eq!(glyphs[1].raw_code, b"B");
    assert_close(glyphs[1].baseline.x, 26.0);
    Ok(())
}

#[test]
fn keeps_unknown_explicit_encoding_differences_contextually_unresolved() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"differences font", false, None);
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"Aacute".to_vec()),
                    Object::Name(b"UnknownGlyph".to_vec()),
                ],
            },
        );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AB) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Unresolved(message))
            if message.contains("content operator Tj")
                && message.contains("no Unicode mapping or stable font identity")
    ));
}

#[test]
fn falls_back_to_standard_encoding_for_partial_to_unicode() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <00> <FF> endcodespacerange \
          1 beginbfchar <41> <0041> endbfchar"
            .to_vec(),
    ));
    let font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("ToUnicode", cmap);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (B) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "B");
    Ok(())
}

#[test]
fn symbol_set_encoding_extracts_fully_mapped_to_unicode_text() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <00> <FF> endcodespacerange \
          2 beginbfchar <41> <0041> <42> <20AC> endbfchar"
            .to_vec(),
    ));
    let font = symbol_set_font(&mut pdf, Some(cmap));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AB) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;

    assert_eq!(mapped_text(document.items()), "A€");
    Ok(())
}

#[test]
fn symbol_set_encoding_gaps_remain_page_scoped_unresolved() -> Result<()> {
    for partial in [true, false] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let cmap = partial.then(|| {
            pdf.add_object(Stream::new(
                dictionary! {},
                b"1 begincodespacerange <00> <FF> endcodespacerange \
                  1 beginbfchar <41> <0041> endbfchar"
                    .to_vec(),
            ))
        });
        let font = symbol_set_font(&mut pdf, cmap);
        let content = pdf.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AB) Tj ET".to_vec(),
        ));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font },
            }),
            None,
            None,
        );

        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;

        assert!(outcome.document().items().is_empty());
        assert_eq!(outcome.issues().len(), 1);
        assert_eq!(outcome.issues()[0].kind(), ExtractionIssueKind::Unresolved);
        assert_eq!(
            outcome.issues()[0].scope(),
            ExtractionScope::Page(PageId(0))
        );
        assert!(
            outcome.issues()[0]
                .description()
                .contains("no Unicode mapping or stable font identity")
        );
    }
    Ok(())
}

#[test]
fn keeps_mismatched_embedded_program_keys_contextually_unresolved() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"mismatched TrueType program", false, None);
    let descriptor = embedded_font_descriptor(&pdf, font);
    let program = embedded_font_program(&pdf, font);
    let descriptor = pdf
        .objects
        .get_mut(&descriptor)
        .expect("fixture descriptor should exist")
        .as_dict_mut()
        .expect("fixture descriptor should be a dictionary");
    descriptor.remove(b"FontFile2");
    descriptor.set("FontFile", program);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Unresolved(message))
            if message.contains("content operator Tj")
                && message.contains("no Unicode mapping or stable font identity")
    ));
}

#[test]
fn reports_unsupported_embedded_font_filters_without_fabricating_identity() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"encoded font bytes", false, None);
    let program = embedded_font_program(&pdf, font);
    pdf.objects
        .get_mut(&program)
        .expect("fixture font program should exist")
        .as_stream_mut()
        .expect("fixture font program should be a stream")
        .dict
        .set("Filter", "UnsupportedFontFilter");
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let result = extract(pdf, ExtractionLimits::default());
    assert!(
        matches!(
            &result,
            Err(Error::Unsupported(message)) if message.contains("decoding stream")
        ),
        "unexpected extraction result: {result:?}"
    );
}

#[test]
fn fully_mapped_tounicode_never_decodes_an_unreadable_font_program() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <00> <FF> endcodespacerange \
          1 beginbfchar <41> <0041> endbfchar"
            .to_vec(),
    ));
    let font = embedded_simple_font(&mut pdf, b"encoded font bytes", false, Some(cmap));
    let program = embedded_font_program(&pdf, font);
    pdf.objects
        .get_mut(&program)
        .expect("fixture font program should exist")
        .as_stream_mut()
        .expect("fixture font program should be a stream")
        .dict
        .set("Filter", "UnsupportedFontFilter");
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;

    assert_eq!(mapped_text(document.items()), "A");
    Ok(())
}

#[test]
fn fully_mapped_tounicode_ignores_font_program_bytes_outside_the_remaining_budget() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap_bytes = b"1 begincodespacerange <00> <FF> endcodespacerange \
        1 beginbfchar <41> <0041> endbfchar"
        .to_vec();
    let cmap = pdf.add_object(Stream::new(dictionary! {}, cmap_bytes.clone()));
    let font = embedded_simple_font(&mut pdf, &vec![0x5a; 8192], false, Some(cmap));
    let content_bytes = b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec();
    let content = pdf.add_object(Stream::new(dictionary! {}, content_bytes.clone()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(
        pdf,
        ExtractionLimits {
            max_total_decoded_bytes: content_bytes.len() + cmap_bytes.len() + 1,
            ..ExtractionLimits::default()
        },
    )?;

    assert_eq!(mapped_text(document.items()), "A");
    Ok(())
}

#[test]
fn accounts_an_embedded_font_program_once_per_cached_font() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let program_bytes = b"cached embedded font";
    let font = embedded_simple_font(&mut pdf, program_bytes, false, None);
    let content_bytes = b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj /F2 10 Tf (B) Tj ET".to_vec();
    let content = pdf.add_object(Stream::new(dictionary! {}, content_bytes.clone()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font, "F2" => font },
        }),
        None,
        None,
    );

    let document = extract(
        pdf,
        ExtractionLimits {
            max_total_decoded_bytes: content_bytes.len() + program_bytes.len(),
            ..ExtractionLimits::default()
        },
    )?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].font_id, glyphs[1].font_id);
    let (
        DecodedText::Unmapped {
            font_hash: first_hash,
            ..
        },
        DecodedText::Unmapped {
            font_hash: second_hash,
            ..
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        return Err(Error::Unresolved(
            "fixture glyphs should remain unmapped".into(),
        ));
    };
    assert_eq!(first_hash, second_hash);
    Ok(())
}

#[test]
fn uses_tounicode_when_the_fallback_encoding_has_differences() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <00> <FF> endcodespacerange\n\
          1 beginbfchar <41> <005A> endbfchar"
            .to_vec(),
    ));
    let font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"CustomA".to_vec()),
                ],
            },
        );
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("ToUnicode", cmap);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "Z");
    Ok(())
}

#[test]
fn extracts_encoding_differences_without_changing_codes_or_widths() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let mut widths = vec![Object::Integer(500); 256];
    widths[65] = Object::Integer(600);
    widths[66] = Object::Integer(700);
    widths[67] = Object::Integer(800);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set("Widths", widths);
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "BaseEncoding" => "WinAnsiEncoding",
            "Differences" => vec![
                Object::Integer(65),
                Object::Name(b"fi".to_vec()),
                Object::Name(b"bullet".to_vec()),
                Object::Name(b"fl".to_vec()),
            ],
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (ABC) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "ﬁ•ﬂ");
    assert_eq!(glyphs[0].raw_code, b"A");
    assert_eq!(glyphs[1].raw_code, b"B");
    assert_eq!(glyphs[2].raw_code, b"C");
    assert_close(glyphs[0].baseline.x, 20.0);
    assert_close(glyphs[1].baseline.x, 26.0);
    assert_close(glyphs[2].baseline.x, 33.0);
    Ok(())
}

#[test]
fn extracts_identity_h_type0_glyphs_with_cid_geometry_and_shared_cache() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap_bytes = b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
        3 beginbfchar <0001> <0041> <0002> <0042> <0003> <0043> endbfchar"
        .to_vec();
    let cmap = pdf.add_object(Stream::new(dictionary! {}, cmap_bytes));
    let font = identity_h_font(
        &mut pdf,
        cmap,
        vec![
            Object::Integer(1),
            Object::Array(vec![Object::Integer(500), Object::Integer(700)]),
        ],
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <00010002> Tj /F2 10 Tf <0003> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font, "F2" => font },
        }),
        None,
        None,
    );

    let document = extract(
        pdf,
        ExtractionLimits {
            max_fonts: 1,
            max_cmap_entries: 4,
            max_cid_width_entries: 2,
            ..ExtractionLimits::default()
        },
    )?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "ABC");
    assert_eq!(glyphs[0].raw_code, [0, 1]);
    assert_eq!(glyphs[1].raw_code, [0, 2]);
    assert_eq!(glyphs[2].raw_code, [0, 3]);
    assert_eq!(glyphs[0].font_id, glyphs[2].font_id);
    assert_close(glyphs[0].baseline.x, 20.0);
    assert_close(glyphs[1].baseline.x, 25.0);
    assert_close(glyphs[2].baseline.x, 32.0);
    assert_eq!(glyphs[0].provenance.content_stream.object_number, content.0);
    assert_eq!(glyphs[0].provenance.operator_index, 3);
    assert_eq!(glyphs[2].provenance.operator_index, 5);
    Ok(())
}

#[test]
fn extracts_identity_v_glyphs_with_vertical_geometry_and_tj_adjustments() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
          3 beginbfchar <0001> <0041> <0002> <0042> <0003> <0043> endbfchar"
            .to_vec(),
    ));
    let font = identity_v_font(&mut pdf, cmap);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 100 Tm [<00010002> -250 <0003>] TJ ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();

    assert_eq!(mapped_text(glyphs), "ABC");
    assert_close(glyphs[0].baseline.x, 20.0);
    assert_close(glyphs[0].baseline.y, 100.0);
    assert_close(glyphs[1].baseline.y, 90.0);
    assert_close(glyphs[2].baseline.y, 82.5);
    assert_close(glyphs[0].direction.x, 0.0);
    assert_close(glyphs[0].direction.y, -1.0);
    assert_close(glyphs[0].bbox.min.x, 15.0);
    assert_close(glyphs[0].bbox.max.x, 25.0);
    assert_close(glyphs[0].bbox.min.y, 89.41);
    assert_close(glyphs[0].bbox.max.y, 102.99);
    Ok(())
}

#[test]
fn preserves_partial_identity_h_tounicode_gaps_with_descendant_font_identity() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
          1 beginbfchar <0001> <0041> endbfchar"
            .to_vec(),
    ));
    let font = embedded_identity_h_font(&mut pdf, cmap, None);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <00010002> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs[0].text, DecodedText::Mapped("A".into()));
    assert!(matches!(
        &glyphs[1].text,
        DecodedText::Unmapped {
            font_hash,
            glyph_id: 2,
        } if font_hash.0.len() == 32
    ));
    assert_eq!(glyphs[1].raw_code, [0, 2]);
    assert_close(glyphs[1].baseline.x, 25.0);
    assert_close(glyphs[1].bbox.max.x, 32.0);
    Ok(())
}

#[test]
fn keeps_custom_cid_to_gid_map_gaps_unresolved_with_an_external_identity() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
          1 beginbfchar <0001> <0041> endbfchar"
            .to_vec(),
    ));
    let cid_to_gid_map = pdf.add_object(Stream::new(dictionary! {}, vec![0, 0, 0, 9]));
    let font = embedded_identity_h_font(&mut pdf, cmap, Some(Object::Reference(cid_to_gid_map)));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <0002> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let mut identities = ExternalFontIdentities::default();
    identities
        .insert(b"FixtureCidEmbedded", b"fixture-font-program-v1")
        .expect("external identity should be valid");
    assert!(matches!(
        extract_with_external_font_identities(pdf, ExtractionLimits::default(), &identities),
        Err(Error::Unresolved(message))
            if message.contains("content operator Tj")
                && message.contains("no Unicode mapping or stable font identity")
    ));
}

#[test]
fn uses_an_explicit_external_identity_for_unembedded_cid_glyphs() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
          1 beginbfchar <0001> <0041> endbfchar"
            .to_vec(),
    ));
    let font = identity_h_font(
        &mut pdf,
        cmap,
        vec![
            Object::Integer(1),
            Object::Array(vec![Object::Integer(500), Object::Integer(700)]),
        ],
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <0002> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    let mut identities = ExternalFontIdentities::default();
    identities.insert(b"FixtureSans", b"fixture-font-program-v1")?;

    let document =
        extract_with_external_font_identities(pdf, ExtractionLimits::default(), &identities)?;
    assert!(matches!(
        &document.items()[0].text,
        DecodedText::Unmapped {
            font_hash,
            glyph_id: 2,
        } if font_hash.0.len() == 32
    ));
    Ok(())
}

#[test]
fn accepts_more_than_256_cid_width_entries() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
          1 beginbfchar <0001> <0041> endbfchar"
            .to_vec(),
    ));
    let font = identity_h_font(
        &mut pdf,
        cmap,
        vec![
            Object::Integer(0),
            Object::Integer(256),
            Object::Integer(500),
        ],
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <0001> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }),
        None,
        None,
    );

    let document = extract(
        pdf,
        ExtractionLimits {
            max_cid_width_entries: 257,
            ..ExtractionLimits::default()
        },
    )?;
    assert_eq!(mapped_text(document.items()), "A");
    Ok(())
}

#[test]
fn bounds_aggregate_cid_width_entries_across_distinct_fonts() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
          1 beginbfchar <0001> <0041> endbfchar"
            .to_vec(),
    ));
    let widths = vec![Object::Integer(1), Object::Integer(2), Object::Integer(500)];
    let first_font = identity_h_font(&mut pdf, cmap, widths.clone());
    let second_font = identity_h_font(&mut pdf, cmap, widths);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm <0001> Tj /F2 10 Tf <0001> Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => first_font, "F2" => second_font },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract(
            pdf,
            ExtractionLimits {
                max_cid_width_entries: 3,
                ..ExtractionLimits::default()
            }
        ),
        Err(Error::LimitExceeded {
            resource: "CID width entries",
            limit: 3,
        })
    ));
}

#[test]
fn uses_difference_glyph_metrics_without_explicit_widths() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => dictionary! {
            "Differences" => vec![
                Object::Integer(65),
                Object::Name(b"i".to_vec()),
            ],
        },
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AB) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(mapped_text(glyphs), "iB");
    assert_eq!(glyphs[0].raw_code, b"A");
    assert_close(glyphs[0].baseline.x, 20.0);
    assert_close(glyphs[1].baseline.x, 22.22);
    Ok(())
}

#[test]
fn bounds_repeated_tounicode_output_before_cloning_each_mapping() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cmap_bytes = b"1 begincodespacerange <00> <FF> endcodespacerange\n\
                       1 beginbfchar <41> <00610062006300640065006600670068> endbfchar"
        .to_vec();
    let cmap = pdf.add_object(Stream::new(dictionary! {}, cmap_bytes.clone()));
    let font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("ToUnicode", cmap);
    let content_bytes = b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AAAA) Tj ET".to_vec();
    let content = pdf.add_object(Stream::new(dictionary! {}, content_bytes.clone()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_total_decoded_bytes: cmap_bytes.len() + content_bytes.len() + 31,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "decoded Unicode text bytes",
            limit: 31,
        })
    ));
}

#[test]
fn enforces_the_aggregate_glyph_limit() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (AB) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_glyphs: 1,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "decoded simple-font glyphs",
            limit: 1,
        })
    ));
}

#[test]
fn counts_skipped_inline_images_toward_the_aggregate_operator_limit() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let first = pdf.add_object(Stream::new(
        dictionary! {},
        b"BI /W 1 /H 1 ID x EI".to_vec(),
    ));
    let second = pdf.add_object(Stream::new(
        dictionary! {},
        b"BI /W 1 /H 1 ID y EI".to_vec(),
    ));
    install_page(
        &mut pdf,
        Object::Array(vec![first.into(), second.into()]),
        Object::Dictionary(dictionary! {}),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_operators: 1,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "content operators",
            limit: 1,
        })
    ));
}

#[test]
fn counts_form_operators_before_parsing_the_next_content_stream() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"q Q".to_vec(),
    ));
    let first = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do".to_vec()));
    let second = pdf.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
    install_page(
        &mut pdf,
        Object::Array(vec![first.into(), second.into()]),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_operators: 4,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "content operators",
            limit: 4,
        })
    ));
}

#[test]
fn shares_the_operand_node_budget_across_page_and_nested_form_parsers() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let inner = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"1 Tc".to_vec(),
    ));
    let outer = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        b"/Inner Do".to_vec(),
    ));
    let first = pdf.add_object(Stream::new(dictionary! {}, b"/Outer Do".to_vec()));
    let second = pdf.add_object(Stream::new(dictionary! {}, b"2 Tc".to_vec()));
    install_page(
        &mut pdf,
        Object::Array(vec![first.into(), second.into()]),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! {
                "Outer" => outer,
                "Inner" => inner,
            },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_operand_nodes: 3,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "content operand nodes",
            limit: 3,
        })
    ));
}

#[test]
fn defaults_to_the_corpus_measured_operand_node_budget() {
    assert_eq!(ExtractionLimits::default().max_operand_nodes, 10_000_000);
}

#[test]
fn charges_cached_form_bytes_once_per_unique_stream() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form_bytes = b"% a comment-only form whose decoded bytes are charged once";
    let page_bytes = b"/X1 Do /X1 Do /X1 Do";
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        form_bytes.to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, page_bytes.to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );
    // Exactly enough budget for one charge of each unique stream: repeated
    // executions of the cached form must not re-charge its decoded bytes.
    let limits = ExtractionLimits {
        max_total_decoded_bytes: page_bytes.len() + form_bytes.len(),
        ..ExtractionLimits::default()
    };
    extract(pdf, limits).expect("cached form bytes should be charged once");

    let mut pdf = LopdfDocument::with_version("1.7");
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        form_bytes.to_vec(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, page_bytes.to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_total_decoded_bytes: page_bytes.len() + form_bytes.len() - 1,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "decoded extraction bytes",
            limit,
        }) if limit == page_bytes.len() + form_bytes.len() - 1
    ));
}

#[test]
fn limits_cached_empty_form_stream_invocations() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        Vec::new(),
    ));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"/X1 Do /X1 Do".to_vec()));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { "X1" => form },
        }),
        None,
        None,
    );
    let limits = ExtractionLimits {
        max_stream_invocations: 2,
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "content stream invocations",
            limit: 2,
        })
    ));
}

#[test]
fn embedded_simple_font_with_explicit_encoding_and_unmapped_code_fails_closed_without_unmapped_identity()
-> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"embedded font program", false, None);
    // Add an explicit /Encoding dictionary with an unknown /Differences glyph name that lacks a Unicode mapping
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
            "Differences" => vec![
                Object::Integer(65),
                Object::Name(b"CustomUnmappedGlyph".to_vec()),
            ],
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    // 1. extract_outcome must report a contextual Unresolved issue with operator provenance
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );

    // 2. into_complete() must return Err(Error::Unresolved(...))
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn embedded_simple_font_without_explicit_encoding_preserves_stable_unmapped_identity() -> Result<()>
{
    // Positive control: when simple font has NO explicit /Encoding, embedded font program hash is trusted as identity
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"verified embedded font program", false, None);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 1);
    assert_eq!(glyphs[0].raw_code, b"A");
    match &glyphs[0].text {
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => {
            assert_eq!(font_hash.0.len(), 32);
            assert_eq!(*glyph_id, 65);
        }
        DecodedText::Mapped(text) => panic!("expected unmapped text, got {text}"),
    }
    Ok(())
}

#[test]
fn embedded_simple_font_with_named_encoding_and_unmapped_code_fails_closed() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"embedded font program", false, None);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    // Code 0x81 (129) is unmapped in WinAnsiEncoding and has no ToUnicode
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    // 1. extract_outcome must report a contextual Unresolved issue with operator provenance and zero emitted glyphs
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );

    // 2. into_complete() must return Err(Error::Unresolved(...))
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn cross_revision_divergent_encodings_do_not_fabricate_matching_unmapped_tokens() -> Result<()> {
    let make_pdf = |custom_glyph_name: &[u8]| -> Result<ExtractionOutcome> {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = embedded_simple_font(&mut pdf, b"shared embedded font program", false, None);
        let font_dictionary = pdf
            .objects
            .get_mut(&font)
            .expect("fixture font should exist")
            .as_dict_mut()
            .expect("fixture font should be a dictionary");
        font_dictionary.set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(custom_glyph_name.to_vec()),
                ],
            },
        );
        let content = pdf.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
        ));
        install_page(
            &mut pdf,
            content.into(),
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => font },
            }),
            None,
            None,
        );
        extract_outcome(pdf, ExtractionLimits::default())
    };

    let old_outcome = make_pdf(b"CustomGlyphAlpha")?;
    let new_outcome = make_pdf(b"CustomGlyphBeta")?;

    // Both revisions must report unresolved extraction issues rather than claiming complete extraction
    // with matching unmapped tokens (which would cause a false-match on code 65)
    assert!(!old_outcome.is_complete());
    assert!(!new_outcome.is_complete());
    assert_eq!(old_outcome.issues().len(), 1);
    assert_eq!(new_outcome.issues().len(), 1);
    Ok(())
}

#[test]
fn standard14_font_with_base_encoding_only_dictionary_matches_named_encoding_canonical_identity()
-> Result<()> {
    // Standard 14 Helvetica with named /WinAnsiEncoding vs dictionary << /Type /Encoding /BaseEncoding /WinAnsiEncoding >>
    // Code 0x81 (129) is undefined in WinAnsiEncoding and has no ToUnicode map.
    let mut pdf = LopdfDocument::with_version("1.7");
    let named_font = base_font(&mut pdf);
    let dict_font = base_font(&mut pdf);

    pdf.objects
        .get_mut(&named_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));

    pdf.objects
        .get_mut(&dict_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
            },
        );

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => named_font,
                "F2" => dict_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        panic!("both glyphs should be unmapped with canonical Standard 14 identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(
        hash1, hash2,
        "named and BaseEncoding-only dictionary must yield identical canonical identity"
    );
    assert_eq!(hash1.0.len(), 32);
    Ok(())
}

#[test]
fn standard14_font_with_differences_in_encoding_dictionary_suppresses_canonical_identity()
-> Result<()> {
    // Standard 14 Helvetica with /Encoding dictionary containing /Differences
    // Code 0x81 (129) mapped to an unmapped glyph name without ToUnicode
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
            "Differences" => vec![
                Object::Integer(129),
                Object::Name(b"CustomUnmappedGlyph".to_vec()),
            ],
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn embedded_simple_font_with_base_encoding_only_dictionary_remains_fail_closed() -> Result<()> {
    // Embedded non-Standard14 TrueType font with dictionary << /Type /Encoding /BaseEncoding /WinAnsiEncoding >>
    // Code 0x81 (129) without ToUnicode must fail-close (no stable identity fabricated)
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"embedded font program", false, None);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn standard14_font_with_unknown_base_encoding_reports_unsupported() -> Result<()> {
    // Standard 14 font with dictionary << /Type /Encoding /BaseEncoding /NonExistentEncoding >>
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "NonExistentEncoding",
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (A) Tj ET".to_vec(),
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unsupported);
    assert!(
        issues[0]
            .description()
            .contains("/NonExistentEncoding is not supported")
    );
    Ok(())
}

fn embedded_standard14_named_font(
    document: &mut LopdfDocument,
    base_font_name: &str,
    program_bytes: &[u8],
) -> ObjectId {
    let program = document.add_object(Stream::new(dictionary! {}, program_bytes.to_vec()));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => base_font_name,
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
        "Flags" => 32,
        "FontFile2" => program,
    });
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => base_font_name,
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => descriptor,
    })
}

#[test]
fn embedded_program_with_standard14_name_under_base_encoding_dictionary_fails_closed() -> Result<()>
{
    // Forged Standard 14 Helvetica with embedded FontFile2 program + BaseEncoding-only dictionary
    let mut pdf = LopdfDocument::with_version("1.7");
    let font =
        embedded_standard14_named_font(&mut pdf, "Helvetica", b"custom embedded font program");
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn embedded_program_with_standard14_name_under_named_encoding_fails_closed() -> Result<()> {
    // Forged Standard 14 Helvetica with embedded FontFile2 program + named /WinAnsiEncoding
    let mut pdf = LopdfDocument::with_version("1.7");
    let font =
        embedded_standard14_named_font(&mut pdf, "Helvetica", b"custom embedded font program");
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn embedded_program_with_standard14_name_under_no_encoding_uses_embedded_hash_not_standard14_identity()
-> Result<()> {
    // Forged Standard 14 Helvetica with embedded FontFile2 program and no /Encoding key
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_standard14_named_font(
        &mut pdf,
        "Helvetica",
        b"custom embedded font program bytes",
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 1);
    assert_eq!(glyphs[0].raw_code, vec![0x81]);
    let DecodedText::Unmapped {
        font_hash,
        glyph_id,
    } = &glyphs[0].text
    else {
        panic!("expected unmapped glyph with embedded font identity");
    };
    assert_eq!(*glyph_id, 129);

    // Also extract genuine Standard 14 Helvetica with no /Encoding to compare font hashes
    let mut genuine_pdf = LopdfDocument::with_version("1.7");
    let genuine_font = base_font(&mut genuine_pdf);
    let genuine_content = genuine_pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut genuine_pdf,
        genuine_content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => genuine_font },
        }),
        None,
        None,
    );
    let genuine_doc = extract(genuine_pdf, ExtractionLimits::default())?;
    let DecodedText::Unmapped {
        font_hash: genuine_hash,
        ..
    } = &genuine_doc.items()[0].text
    else {
        panic!("expected genuine Standard 14 unmapped text");
    };

    assert_ne!(
        font_hash, genuine_hash,
        "embedded font program must NOT receive genuine Standard 14 canonical identity"
    );
    Ok(())
}

#[test]
fn standard14_font_with_empty_encoding_dictionary_matches_built_in_encoding_canonical_identity()
-> Result<()> {
    // Genuine Standard 14 Helvetica with no /Encoding vs empty /Encoding << /Type /Encoding >>
    let mut pdf = LopdfDocument::with_version("1.7");
    let no_enc_font = base_font(&mut pdf);
    let empty_dict_font = base_font(&mut pdf);

    pdf.objects
        .get_mut(&empty_dict_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("Encoding", dictionary! { "Type" => "Encoding" });

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => no_enc_font,
                "F2" => empty_dict_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        panic!("both glyphs should be unmapped with canonical Standard 14 built-in identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(
        hash1, hash2,
        "omitted /Encoding and empty /Encoding dictionary must yield identical canonical identity"
    );
    assert_eq!(hash1.0.len(), 32);
    Ok(())
}

#[test]
fn non_standard14_font_with_empty_encoding_dictionary_remains_fail_closed() -> Result<()> {
    // Non-Standard14 embedded TrueType font with empty /Encoding << /Type /Encoding >>
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = embedded_simple_font(&mut pdf, b"embedded font program", false, None);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set("Encoding", dictionary! { "Type" => "Encoding" });

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn standard14_font_with_empty_differences_array_suppresses_canonical_identity() -> Result<()> {
    // Standard 14 Helvetica with /Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [] >>
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let font_dictionary = pdf
        .objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "BaseEncoding" => "WinAnsiEncoding",
            "Differences" => Vec::<Object>::new(),
        },
    );
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn embedded_type1_program_with_standard14_name_under_named_encoding_fails_closed() -> Result<()> {
    // Forged Standard 14 Helvetica with /Subtype /Type1 and embedded /FontFile program + /WinAnsiEncoding
    let mut pdf = LopdfDocument::with_version("1.7");
    let program = pdf.add_object(Stream::new(
        dictionary! {},
        b"custom Type1 font program bytes".to_vec(),
    ));
    let descriptor = pdf.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Helvetica",
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
        "FontFile" => program,
    });
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => descriptor,
        "Encoding" => Object::Name(b"WinAnsiEncoding".to_vec()),
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn standard14_font_with_null_encoding_matches_omitted_encoding_canonical_identity() -> Result<()> {
    // Genuine Standard 14 Helvetica with omitted /Encoding vs explicit /Encoding null
    let mut pdf = LopdfDocument::with_version("1.7");
    let no_enc_font = base_font(&mut pdf);
    let null_enc_font = base_font(&mut pdf);

    pdf.objects
        .get_mut(&null_enc_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("Encoding", Object::Null);

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => no_enc_font,
                "F2" => null_enc_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        panic!("both glyphs should be unmapped with canonical Standard 14 built-in identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(
        hash1, hash2,
        "omitted /Encoding and /Encoding null must yield identical canonical identity"
    );
    assert_eq!(hash1.0.len(), 32);
    Ok(())
}

#[test]
fn standard14_font_with_null_descriptor_keeps_canonical_identity() -> Result<()> {
    // Genuine Standard 14 Helvetica with /FontDescriptor null
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => Object::Null,
        "Encoding" => Object::Name(b"WinAnsiEncoding".to_vec()),
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 1);
    let DecodedText::Unmapped {
        font_hash,
        glyph_id,
    } = &glyphs[0].text
    else {
        panic!("expected unmapped glyph with canonical identity");
    };
    assert_eq!(*glyph_id, 129);
    assert_eq!(font_hash.0.len(), 32);
    Ok(())
}

#[test]
fn non_standard14_font_with_null_entries_fails_closed_without_identity() -> Result<()> {
    // Non-Standard14 font with /Encoding null and /FontDescriptor with null font file
    let mut pdf = LopdfDocument::with_version("1.7");
    let descriptor = pdf.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "NonStandardCustomFont",
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
        "FontFile2" => Object::Null,
    });
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "NonStandardCustomFont",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "Encoding" => Object::Null,
        "FontDescriptor" => descriptor,
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].scope(), ExtractionScope::Page(PageId(0)));
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(matches!(
        outcome.into_complete(),
        Err(Error::Unresolved(desc)) if desc.contains("font code has no Unicode mapping or stable font identity")
    ));
    Ok(())
}

#[test]
fn font_with_null_base_font_is_treated_as_absent_name() -> Result<()> {
    // Font with /BaseFont null: treated as absent rather than "BaseFont is not a name" malformed error
    let mut pdf = LopdfDocument::with_version("1.7");
    let descriptor = pdf.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "UnnamedFont",
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
    });
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => Object::Null,
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => descriptor,
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(!issues[0].description().contains("BaseFont is not a name"));
    Ok(())
}

#[test]
fn non_embedded_truetype_standard14_matches_type1_canonical_identity() -> Result<()> {
    // Non-embedded /Subtype /TrueType with BaseFont /Helvetica, named /WinAnsiEncoding, no FontFile*
    // vs non-embedded /Subtype /Type1 Helvetica with /WinAnsiEncoding
    let mut pdf = LopdfDocument::with_version("1.7");
    let type1_font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&type1_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));

    let descriptor = pdf.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Helvetica",
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
        "Flags" => 32,
    });
    let truetype_font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => descriptor,
        "Encoding" => Object::Name(b"WinAnsiEncoding".to_vec()),
    });

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => type1_font,
                "F2" => truetype_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);

    let (
        DecodedText::Unmapped {
            font_hash: type1_hash,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: truetype_hash,
            glyph_id: id2,
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        panic!("both glyphs should be unmapped with canonical Standard 14 identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(
        type1_hash, truetype_hash,
        "non-embedded TrueType Standard14 must receive identical canonical identity to Type1 Standard14"
    );
    assert_eq!(type1_hash.0.len(), 32);
    Ok(())
}

#[test]
fn standard14_font_with_direct_and_indirect_null_differences_retains_canonical_identity()
-> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let indirect_null = pdf.add_object(Object::Null);

    let no_diff_font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&no_diff_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
            },
        );

    let direct_null_diff_font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&direct_null_diff_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => Object::Null,
            },
        );

    let indirect_null_diff_font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&indirect_null_diff_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => indirect_null,
            },
        );

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'/', b'F', b'3', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0',
            b' ', b'0', b' ', b'1', b' ', b'6', b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ',
            b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => no_diff_font,
                "F2" => direct_null_diff_font,
                "F3" => indirect_null_diff_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 3);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
        DecodedText::Unmapped {
            font_hash: hash3,
            glyph_id: id3,
        },
    ) = (&glyphs[0].text, &glyphs[1].text, &glyphs[2].text)
    else {
        panic!("all glyphs should be unmapped with canonical Standard 14 identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(*id3, 129);
    assert_eq!(hash1, hash2);
    assert_eq!(hash1, hash3);
    assert_eq!(hash1.0.len(), 32);
    Ok(())
}

#[test]
fn standard14_font_with_indirect_null_encoding_matches_omitted_encoding() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let indirect_null = pdf.add_object(Object::Null);
    let no_enc_font = base_font(&mut pdf);
    let null_enc_font = base_font(&mut pdf);

    pdf.objects
        .get_mut(&null_enc_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("Encoding", indirect_null);

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => no_enc_font,
                "F2" => null_enc_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        panic!("both glyphs should be unmapped with canonical Standard 14 identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(hash1, hash2);
    Ok(())
}

#[test]
fn font_with_cyclic_encoding_reference_reports_error() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cyclic_ref = pdf.add_object(Object::Null);
    pdf.objects
        .insert(cyclic_ref, Object::Reference(cyclic_ref));

    let font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set("Encoding", Object::Reference(cyclic_ref));

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Backend(message)) if message.contains("reference cycle")
    ));
    Ok(())
}

#[test]
fn font_with_indirect_base_font_name_matches_direct_name() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let name_obj = pdf.add_object(Object::Name(b"Helvetica".to_vec()));

    let direct_font = base_font(&mut pdf);
    let indirect_font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => name_obj,
    });

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => direct_font,
                "F2" => indirect_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 2);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
    ) = (&glyphs[0].text, &glyphs[1].text)
    else {
        panic!("both glyphs should be unmapped with canonical Standard 14 identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(hash1, hash2);
    Ok(())
}

#[test]
fn font_with_indirect_null_base_font_is_treated_as_absent_name() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let null_obj = pdf.add_object(Object::Null);
    let descriptor = pdf.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "UnnamedFont",
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
    });
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => null_obj,
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => descriptor,
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert!(outcome.document().items().is_empty());
    let issues = outcome.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind(), ExtractionIssueKind::Unresolved);
    assert!(
        issues[0]
            .description()
            .contains("font code has no Unicode mapping or stable font identity")
    );
    assert!(!issues[0].description().contains("BaseFont is not a name"));
    Ok(())
}

#[test]
fn font_with_cyclic_base_font_reports_error() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let cyclic_ref = pdf.add_object(Object::Null);
    pdf.objects
        .insert(cyclic_ref, Object::Reference(cyclic_ref));

    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => cyclic_ref,
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => font },
        }),
        None,
        None,
    );

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Backend(message)) if message.contains("reference cycle")
    ));
    Ok(())
}

#[test]
fn standard14_font_with_direct_and_indirect_null_base_encoding_selects_builtin_identity()
-> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let indirect_null = pdf.add_object(Object::Null);
    let no_enc_font = base_font(&mut pdf);

    let direct_null_base_enc_font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&direct_null_base_enc_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => Object::Null,
            },
        );

    let indirect_null_base_enc_font = base_font(&mut pdf);
    pdf.objects
        .get_mut(&indirect_null_base_enc_font)
        .expect("fixture font should exist")
        .as_dict_mut()
        .expect("fixture font should be a dictionary")
        .set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => indirect_null,
            },
        );

    let content = pdf.add_object(Stream::new(
        dictionary! {},
        vec![
            b'B', b'T', b' ', b'/', b'F', b'1', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1',
            b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'2', b'0', b' ', b'3', b'0', b' ', b'T',
            b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'/', b'F', b'2', b' ', b'1',
            b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0', b' ', b'0', b' ', b'1', b' ', b'4',
            b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ', b'(', 0x81, b')', b' ', b'T', b'j',
            b' ', b'/', b'F', b'3', b' ', b'1', b'0', b' ', b'T', b'f', b' ', b'1', b' ', b'0',
            b' ', b'0', b' ', b'1', b' ', b'6', b'0', b' ', b'3', b'0', b' ', b'T', b'm', b' ',
            b'(', 0x81, b')', b' ', b'T', b'j', b' ', b'E', b'T',
        ],
    ));
    install_page(
        &mut pdf,
        content.into(),
        Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => no_enc_font,
                "F2" => direct_null_base_enc_font,
                "F3" => indirect_null_base_enc_font,
            },
        }),
        None,
        None,
    );

    let document = extract(pdf, ExtractionLimits::default())?;
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 3);

    let (
        DecodedText::Unmapped {
            font_hash: hash1,
            glyph_id: id1,
        },
        DecodedText::Unmapped {
            font_hash: hash2,
            glyph_id: id2,
        },
        DecodedText::Unmapped {
            font_hash: hash3,
            glyph_id: id3,
        },
    ) = (&glyphs[0].text, &glyphs[1].text, &glyphs[2].text)
    else {
        panic!("all glyphs should be unmapped with canonical Standard 14 identity");
    };

    assert_eq!(*id1, 129);
    assert_eq!(*id2, 129);
    assert_eq!(*id3, 129);
    assert_eq!(hash1, hash2);
    assert_eq!(hash1, hash3);
    assert_eq!(hash1.0.len(), 32);
    Ok(())
}

#[test]
fn externally_rendered_japanese_typst_fixture_extracts_complete_horizontal_glyphs() -> Result<()> {
    let old_bytes = include_bytes!("../../../fixtures/external/japanese-typst/old.pdf");
    let new_bytes = include_bytes!("../../../fixtures/external/japanese-typst/new.pdf");

    let old_pdf = LopdfParser.parse(Arc::from(old_bytes.as_slice()), ParseLimits::default())?;
    let new_pdf = LopdfParser.parse(Arc::from(new_bytes.as_slice()), ParseLimits::default())?;

    let old_outcome = ContentStreamGlyphExtractor
        .extract_outcome(old_pdf.as_ref(), ExtractionLimits::default())?;
    let new_outcome = ContentStreamGlyphExtractor
        .extract_outcome(new_pdf.as_ref(), ExtractionLimits::default())?;

    assert!(old_outcome.is_complete());
    assert!(new_outcome.is_complete());
    assert!(old_outcome.issues().is_empty());
    assert!(new_outcome.issues().is_empty());

    let old_glyphs = old_outcome.document().items();
    let new_glyphs = new_outcome.document().items();

    assert_eq!(old_glyphs.len(), 87);
    assert_eq!(new_glyphs.len(), 87);

    for glyph in old_glyphs.iter().chain(new_glyphs.iter()) {
        assert_eq!(
            glyph.direction,
            pdfdelta_core::model::Vec2 { x: 1.0, y: 0.0 }
        );
        assert!(matches!(glyph.text, DecodedText::Mapped(_)));
    }

    let old_text = old_glyphs
        .iter()
        .map(|g| match &g.text {
            DecodedText::Mapped(s) => s.as_str(),
            DecodedText::Unmapped { .. } => panic!("all glyphs must be mapped"),
        })
        .collect::<String>();
    let new_text = new_glyphs
        .iter()
        .map(|g| match &g.text {
            DecodedText::Mapped(s) => s.as_str(),
            DecodedText::Unmapped { .. } => panic!("all glyphs must be mapped"),
        })
        .collect::<String>();

    assert_eq!(
        old_text,
        "定期システム運用報告書今後の保守計画およびサービス稼働状況に関する概要です。第10版の運用手順書を引き続き適用します。すべての基幹業務システムは各地域で正常に稼働しています。"
    );
    assert_eq!(
        new_text,
        "定期システム運用報告書今後の保守計画およびサービス稼働状況に関する概要です。第20版の運用手順書を引き続き適用します。すべての基幹業務システムは各地域で正常に稼働しています。"
    );

    Ok(())
}
