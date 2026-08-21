use std::sync::Arc;

use lopdf::{Document as LopdfDocument, Object, ObjectId, Stream, dictionary};
use pdfdelta_core::{
    Error, Result,
    model::{DecodedText, Document, Glyph},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    source::{
        ContentStreamGlyphExtractor, ExtractionIssueKind, ExtractionLimits, ExtractionOutcome,
        ExtractionScope, GlyphExtractor,
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
    assert_eq!(glyphs[0].provenance.content_stream.object_number, content.0);
    assert_eq!(glyphs[0].provenance.operator_index, 3);
    assert_eq!(glyphs[0].render_order, 0);
    assert_eq!(glyphs[1].render_order, 1);
    assert_close(glyphs[0].baseline.x, 20.0);
    assert_close(glyphs[0].baseline.y, 180.0);
    assert_close(glyphs[1].baseline.x, 20.0);
    assert_close(glyphs[1].baseline.y, 175.0);
    assert_close(glyphs[0].direction.x, 0.0);
    assert_close(glyphs[0].direction.y, -1.0);
    assert_close(glyphs[0].bbox.min.x, 18.0);
    assert_close(glyphs[0].bbox.max.x, 28.0);
    assert_close(glyphs[0].bbox.min.y, 175.0);
    assert_close(glyphs[0].bbox.max.y, 180.0);
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

    let document = extract(pdf, ExtractionLimits::default())?;
    assert_eq!(mapped_text(document.items()), "Tagged note");
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
    assert_close(glyphs[0].baseline.x, 10.0);
    assert_close(glyphs[1].baseline.x, 12.5);
    assert_close(glyphs[2].baseline.x, 18.5);
    assert_close(glyphs[0].baseline.y, 51.0);
    assert_close(glyphs[0].bbox.min.y, 49.0);
    assert_close(glyphs[0].bbox.max.y, 59.0);
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
fn keeps_custom_cid_to_gid_map_gaps_contextually_unresolved() {
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

    assert!(matches!(
        extract(pdf, ExtractionLimits::default()),
        Err(Error::Unresolved(message))
            if message.contains("content operator Tj")
                && message.contains("no Unicode mapping or stable font identity")
    ));
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
fn accounts_cached_form_bytes_on_every_execution() {
    let mut pdf = LopdfDocument::with_version("1.7");
    let form_bytes = b"% a comment-only form whose decoded bytes remain execution-bounded";
    let page_bytes = b"/X1 Do /X1 Do";
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
        max_total_decoded_bytes: page_bytes.len() + form_bytes.len(),
        ..ExtractionLimits::default()
    };

    assert!(matches!(
        extract(pdf, limits),
        Err(Error::LimitExceeded {
            resource: "decoded extraction bytes",
            limit,
        }) if limit == page_bytes.len() + form_bytes.len()
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
