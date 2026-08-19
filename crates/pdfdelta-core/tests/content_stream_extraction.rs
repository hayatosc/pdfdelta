use std::sync::Arc;

use lopdf::{Document as LopdfDocument, Object, Stream, dictionary};
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
