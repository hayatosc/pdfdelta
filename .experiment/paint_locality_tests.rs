
#[test]
fn disconnected_pdf_rules_do_not_acquire_joins_between_subpaths() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"1 w 10 M 20 100 m 80 100 l 20 110 m 80 110 l S BT /F1 10 Tf 20 160 Td (after) Tj ET".to_vec(),
    ));
    install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }), None, None);
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(outcome.is_complete());
    assert_eq!(mapped_text(outcome.document().items()), "after");
    assert_eq!(outcome.document().vector_lines().len(), 2);
    let paints = outcome.document().non_text_paint_bounds().expect("retained rules");
    assert_eq!(paints.len(), 1);
    assert_eq!(paints[0].content_stream.object_number, content.0);
    let bounds = paints[0].bounds.expect("bounded positive-width rules");
    assert!(bounds.min.x > 18.0 && bounds.min.x <= 19.0);
    assert!(bounds.max.x >= 81.0 && bounds.max.x < 82.0);
    assert!(bounds.min.y > 98.0 && bounds.max.y < 112.0);
    Ok(())
}

#[test]
fn small_curve_in_large_form_retains_local_bounds_and_native_text() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let form = pdf.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 200.into(), 200.into()] }, b"20 20 m 20 40 40 40 40 20 c f".to_vec()));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"q /X Do Q BT /F1 10 Tf 20 160 Td (after) Tj ET".to_vec()));
    install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "X" => form } }), None, None);
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(outcome.is_complete());
    assert_eq!(mapped_text(outcome.document().items()), "after");
    let paints = outcome.document().non_text_paint_bounds().expect("retained curve");
    assert_eq!(paints.len(), 1);
    assert_eq!(paints[0].content_stream.object_number, form.0);
    let bounds = paints[0].bounds.expect("finite curve control hull");
    assert!(bounds.min.x > 19.0 && bounds.min.x <= 20.0);
    assert!(bounds.min.y > 19.0 && bounds.min.y <= 20.0);
    assert!(bounds.max.x >= 40.0 && bounds.max.x < 41.0);
    assert!(bounds.max.y >= 40.0 && bounds.max.y < 41.0);
    assert!(outcome.document().last_non_text_paint().contains_key(&PageId(0)));
    Ok(())
}

#[test]
fn failed_nested_pdf_form_keeps_outer_bounds_and_the_extraction_gap() -> Result<()> {
    let mut pdf = LopdfDocument::with_version("1.7");
    let font = base_font(&mut pdf);
    let inner = pdf.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 200.into(), 200.into()] }, b"0 0 1 1 re f UnsupportedOperator".to_vec()));
    let outer = pdf.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![10.into(), 10.into(), 20.into(), 20.into()], "Resources" => dictionary! { "XObject" => dictionary! { "Inner" => inner } } }, b"/Inner Do".to_vec()));
    let content = pdf.add_object(Stream::new(dictionary! {}, b"BT /F1 10 Tf 20 160 Td (A) Tj ET /Outer Do BT /F1 10 Tf 40 160 Td (B) Tj ET".to_vec()));
    install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "Outer" => outer } }), None, None);
    let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
    assert!(!outcome.is_complete());
    assert_eq!(mapped_text(outcome.document().items()), "AB");
    assert_eq!(outcome.issues().len(), 1);
    assert_eq!(outcome.issues()[0].scope(), ExtractionScope::PageGlyphGap { page: PageId(0), retained_before: 1, paint_index: Some(0) });
    let paints = outcome.document().non_text_paint_bounds().expect("retained opaque effect");
    assert_eq!(paints.len(), 1, "partial inner effects must be rolled back");
    assert_eq!(paints[0].content_stream.object_number, outer.0);
    assert_eq!(paints[0].operator_index, 0);
    let bounds = paints[0].bounds.expect("outer Form still bounds the unknown effect");
    for (actual, expected, lower) in [(bounds.min.x, 10.0, true), (bounds.min.y, 10.0, true), (bounds.max.x, 20.0, false), (bounds.max.y, 20.0, false)] {
        assert!((actual - expected).abs() < 1e-9);
        assert!(if lower { actual <= expected } else { actual >= expected });
    }
    Ok(())
}

#[test]
fn joined_pdf_rules_and_device_dependent_widths_remain_conservative() -> Result<()> {
    for (settings, path, bounded) in [
        ("1 w 10 M", "20 100 m 80 100 l 20 101 l 20 110 m 80 110 l S", true),
        ("0 w 10 M", "20 100 m 80 100 l S", false),
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let content = pdf.add_object(Stream::new(dictionary! {}, format!("{settings} {path} BT /F1 10 Tf 20 160 Td (after) Tj ET").into_bytes()));
        install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }), None, None);
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert_eq!(mapped_text(outcome.document().items()), "after");
        let paints = outcome.document().non_text_paint_bounds().expect("retained stroke");
        assert_eq!(paints.len(), 1);
        assert_eq!(paints[0].bounds.is_some(), bounded);
        if let Some(bounds) = paints[0].bounds { assert!(bounds.max.x >= 90.0); }
    }
    Ok(())
}
