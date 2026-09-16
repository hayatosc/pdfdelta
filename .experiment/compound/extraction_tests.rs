
#[test]
fn compound_path_bounds_preserve_subpath_gaps_for_every_fill_rule() -> Result<()> {
    for program in [
        "10 10 30 2 re 10 100 30 2 re f",
        "10 10 30 2 re 10 100 30 2 re f*",
        "10 10 m 40 10 l 40 12 l 10 100 m 40 100 l 40 102 l f",
        "10 10 m 20 12 30 12 40 10 c 10 100 m 20 102 30 102 40 100 c f",
        "10 10 m 30 12 40 10 v 10 100 m 30 102 40 100 y f",
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let content = pdf.add_object(Stream::new(dictionary! {},
            format!("{program} BT /F1 10 Tf 20 160 Td (after) Tj ET").into_bytes()));
        install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }), None, None);
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(outcome.is_complete(), "{program}");
        assert_eq!(mapped_text(outcome.document().items()), "after");
        let paints = outcome.document().non_text_paint_bounds().expect("complete paint acquisition");
        assert_eq!(paints.len(), 2, "{program}");
        for (paint, y) in paints.iter().zip([10.0, 100.0]) {
            assert_eq!(paint.content_stream.object_number, content.0);
            assert_eq!(paint.operator_index, paints[0].operator_index);
            assert_eq!(paint.render_order, paints[0].render_order);
            let bounds = paint.bounds.expect("finite subpath envelope");
            assert!(bounds.min.x <= 10.0 && bounds.min.x > 9.0);
            assert!(bounds.max.x >= 40.0 && bounds.max.x < 41.0);
            assert!(bounds.min.y <= y && bounds.min.y > y - 1.0);
            assert!(bounds.max.y >= y + 2.0 && bounds.max.y < y + 3.0);
            assert!(!(bounds.min.y <= 50.0 && bounds.max.y >= 50.0));
        }
    }
    Ok(())
}

#[test]
fn compound_path_component_exhaustion_retains_the_entire_paint_envelope() -> Result<()> {
    for (count, expected_records) in [(2, 2), (64, 64), (65, 1), (100, 1)] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let mut program = String::new();
        for row in 0..count {
            program.push_str(&format!("10 {} 20 1 re ", row * 2 + 10));
        }
        program.push_str("f BT /F1 10 Tf 20 250 Td (after) Tj ET");
        let content = pdf.add_object(Stream::new(dictionary! {}, program.into_bytes()));
        install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }), None, None);
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(outcome.is_complete());
        assert_eq!(mapped_text(outcome.document().items()), "after");
        let paints = outcome.document().non_text_paint_bounds().expect("complete paint acquisition");
        assert_eq!(paints.len(), expected_records, "component count {count}");
        for row in 0..count {
            let y = (row * 2 + 10) as f64;
            assert!(paints.iter().any(|paint| paint.bounds.is_some_and(|bounds| {
                bounds.min.x <= 10.0 && bounds.max.x >= 30.0
                    && bounds.min.y <= y && bounds.max.y >= y + 1.0
            })), "all components remain enclosed: row {row} of {count}");
        }
    }
    Ok(())
}

#[test]
fn compound_path_hairlines_and_dangling_moves_are_not_empty_paint() -> Result<()> {
    for (program, expected_records, bounded) in [
        ("0 w 10 10 m 40 10 l 10 100 m 40 100 l S", 1, false),
        ("10 10 30 2 re 10 100 30 2 re 100 150 m f", 2, true),
        ("10 10 30 2 re 60 40 l 10 100 30 2 re f", 2, true),
    ] {
        let mut pdf = LopdfDocument::with_version("1.7");
        let font = base_font(&mut pdf);
        let content = pdf.add_object(Stream::new(dictionary! {},
            format!("{program} BT /F1 10 Tf 20 160 Td (after) Tj ET").into_bytes()));
        install_page(&mut pdf, content.into(), Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => font } }), None, None);
        let outcome = extract_outcome(pdf, ExtractionLimits::default())?;
        assert!(outcome.is_complete());
        let paints = outcome.document().non_text_paint_bounds().expect("opaque effects retained");
        assert_eq!(paints.len(), expected_records, "{program}");
        assert!(paints.iter().all(|paint| paint.bounds.is_some() == bounded));
        if program.contains("60 40 l") {
            let first = paints[0].bounds.expect("the connected extension is still in its subpath");
            assert!(first.max.x >= 60.0 && first.max.y >= 40.0);
        }
    }
    Ok(())
}
