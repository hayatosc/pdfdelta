//! Glyph overlay SVG rendering and XML escaping fixtures.

use pdfdelta_core::{
    Error, Result,
    model::{
        DecodedText, Document, FontId, FontProgramHash, Glyph, GlyphCropStatus, GlyphId,
        GlyphPathClipStatus, GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    overlay_svg::{render_glyph_overlay_svg, write_glyph_overlay_svg},
    pdf::ObjectRef,
};

#[test]
fn svg_render_creates_valid_overlay_with_provenance_and_geometry() -> Result<()> {
    let glyphs = vec![
        Glyph {
            id: GlyphId(1),
            text: DecodedText::Mapped("Hello".to_owned()),
            raw_code: vec![0x48, 0x65, 0x6c, 0x6c, 0x6f],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 72.0, y: 700.0 },
                max: Vec2 { x: 120.0, y: 712.0 },
            },
            baseline: Vec2 { x: 72.0, y: 702.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(5),
            font_size: 12.0,
            render_order: 1,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::PartiallyOutside,
            path_clip_status: GlyphPathClipStatus::PartiallyOutside,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 10,
                    generation: 0,
                },
                operator_index: 3,
            },
        },
        Glyph {
            id: GlyphId(2),
            text: DecodedText::Unmapped {
                font_hash: FontProgramHash(vec![0xab, 0xcd, 0xef, 0x01]),
                glyph_id: 42,
            },
            raw_code: vec![0x00, 0x2a],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 125.0, y: 700.0 },
                max: Vec2 { x: 135.0, y: 712.0 },
            },
            baseline: Vec2 { x: 125.0, y: 702.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(6),
            font_size: 12.0,
            render_order: 2,
            render_mode: TextRenderMode::Invisible,
            crop_status: GlyphCropStatus::Outside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 10,
                    generation: 0,
                },
                operator_index: 4,
            },
        },
    ];

    let document = Document::new(glyphs);
    let svg = render_glyph_overlay_svg(&document)?;

    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.ends_with("</svg>\n"));
    assert!(svg.contains("id=\"page-1\""));
    assert!(svg.contains("Hello"));
    assert!(svg.contains("data-glyph-id=\"1\""));
    assert!(svg.contains("data-cs-num=\"10\""));
    assert!(svg.contains("data-op-idx=\"3\""));
    assert!(svg.contains("glyph-unmapped"));
    assert!(svg.contains("glyph-invisible"));
    assert!(svg.contains("glyph-crop-partial"));
    assert!(svg.contains("glyph-crop-outside"));
    assert!(svg.contains("glyph-path-clip-partial"));
    assert!(svg.contains("data-crop-status=\"PartiallyOutside\""));
    assert!(svg.contains("data-path-clip-status=\"PartiallyOutside\""));
    assert!(svg.contains("U+002A:program:abcdef01"));
    Ok(())
}

#[test]
fn svg_render_handles_empty_document_and_multipage() -> Result<()> {
    let empty_doc = Document::new(Vec::new());
    let empty_svg = render_glyph_overlay_svg(&empty_doc)?;
    assert!(empty_svg.contains("id=\"page-1\""));
    assert!(empty_svg.contains("Page 1 (glyphs: 0)"));

    let multipage_glyphs = vec![
        Glyph {
            id: GlyphId(1),
            text: DecodedText::Mapped("Page 1 text".to_owned()),
            raw_code: vec![1],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 50.0, y: 500.0 },
                max: Vec2 { x: 100.0, y: 510.0 },
            },
            baseline: Vec2 { x: 50.0, y: 500.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: 1,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: 0,
            },
        },
        Glyph {
            id: GlyphId(2),
            text: DecodedText::Mapped("Page 2 rotated".to_owned()),
            raw_code: vec![2],
            page: PageId(1),
            bbox: Rect {
                min: Vec2 { x: 50.0, y: 500.0 },
                max: Vec2 { x: 60.0, y: 550.0 },
            },
            baseline: Vec2 { x: 50.0, y: 500.0 },
            direction: Vec2 { x: 0.0, y: 1.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: 1,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 2,
                    generation: 0,
                },
                operator_index: 0,
            },
        },
    ];

    let multi_doc = Document::new(multipage_glyphs);
    let multi_svg = render_glyph_overlay_svg(&multi_doc)?;
    assert!(multi_svg.contains("id=\"page-1\""));
    assert!(multi_svg.contains("id=\"page-2\""));
    assert!(multi_svg.contains("rotate(-90.00"));
    Ok(())
}

fn sample_svg_glyph(id: u64, min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Glyph {
    Glyph {
        id: GlyphId(id),
        text: DecodedText::Mapped("A".to_owned()),
        raw_code: vec![65],
        page: PageId(0),
        bbox: Rect {
            min: Vec2 { x: min_x, y: min_y },
            max: Vec2 { x: max_x, y: max_y },
        },
        baseline: Vec2 { x: min_x, y: min_y },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size: 10.0,
        render_order: 1,
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        path_clip_status: GlyphPathClipStatus::Unclipped,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: 0,
        },
    }
}

#[test]
fn svg_render_and_write_reject_non_finite_geometry() {
    let test_cases = vec![
        ("nan in bbox.min.x", {
            let mut g = sample_svg_glyph(1, 50.0, 500.0, 100.0, 510.0);
            g.bbox.min.x = f64::NAN;
            g
        }),
        ("infinity in bbox.max.y", {
            let mut g = sample_svg_glyph(2, 50.0, 500.0, 100.0, 510.0);
            g.bbox.max.y = f64::INFINITY;
            g
        }),
        ("neg_infinity in bbox.min.y", {
            let mut g = sample_svg_glyph(3, 50.0, 500.0, 100.0, 510.0);
            g.bbox.min.y = f64::NEG_INFINITY;
            g
        }),
        ("nan in baseline.x", {
            let mut g = sample_svg_glyph(4, 50.0, 500.0, 100.0, 510.0);
            g.baseline.x = f64::NAN;
            g
        }),
        ("infinity in direction.y", {
            let mut g = sample_svg_glyph(5, 50.0, 500.0, 100.0, 510.0);
            g.direction.y = f64::INFINITY;
            g
        }),
        ("nan in font_size", {
            let mut g = sample_svg_glyph(6, 50.0, 500.0, 100.0, 510.0);
            g.font_size = f64::NAN;
            g
        }),
    ];

    for (desc, glyph) in test_cases {
        let expected_glyph_id = format!("glyph {}", glyph.id.0);
        let doc = Document::new(vec![glyph]);

        let render_res = render_glyph_overlay_svg(&doc);
        match render_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
                assert!(
                    msg.contains(&expected_glyph_id),
                    "[{desc}] error message '{msg}' should contain '{expected_glyph_id}'"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }

        let mut buffer = Vec::new();
        let write_res = write_glyph_overlay_svg(&doc, &mut buffer);
        match write_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
                assert!(
                    msg.contains(&expected_glyph_id),
                    "[{desc}] error message '{msg}' should contain '{expected_glyph_id}'"
                );
                assert!(
                    buffer.is_empty(),
                    "[{desc}] writer buffer must remain empty on validation failure"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }
    }
}

#[test]
fn svg_render_and_write_reject_negative_font_size() {
    let mut glyph = sample_svg_glyph(42, 50.0, 500.0, 100.0, 510.0);
    glyph.font_size = -12.0;
    let doc = Document::new(vec![glyph]);

    let render_res = render_glyph_overlay_svg(&doc);
    match render_res {
        Err(Error::Report(msg)) => {
            assert!(
                msg.contains("negative font size"),
                "error '{msg}' should mention negative font size"
            );
            assert!(
                msg.contains("glyph 42"),
                "error '{msg}' should contain glyph 42"
            );
        }
        other => panic!("expected Error::Report for negative font size, got {other:?}"),
    }

    let mut buffer = Vec::new();
    let write_res = write_glyph_overlay_svg(&doc, &mut buffer);
    match write_res {
        Err(Error::Report(msg)) => {
            assert!(
                msg.contains("negative font size"),
                "error '{msg}' should mention negative font size"
            );
            assert!(
                msg.contains("glyph 42"),
                "error '{msg}' should contain glyph 42"
            );
            assert!(
                buffer.is_empty(),
                "writer buffer must remain empty on negative font size failure"
            );
        }
        other => panic!("expected Error::Report for negative font size, got {other:?}"),
    }
}

#[test]
fn svg_render_and_write_reject_huge_finite_derived_overflow() {
    let test_cases = vec![
        (
            "huge bbox width overflow",
            sample_svg_glyph(10, -1e308, 0.0, 1e308, 10.0),
        ),
        ("huge font_size baseline endpoint overflow", {
            let mut g = sample_svg_glyph(11, 0.0, 0.0, 10.0, 10.0);
            g.baseline.x = 1e308;
            g.font_size = 1e308;
            g
        }),
    ];

    for (desc, glyph) in test_cases {
        let doc = Document::new(vec![glyph]);

        let render_res = render_glyph_overlay_svg(&doc);
        match render_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }

        let mut buffer = Vec::new();
        let write_res = write_glyph_overlay_svg(&doc, &mut buffer);
        match write_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
                assert!(
                    buffer.is_empty(),
                    "[{desc}] writer buffer must remain empty on derived overflow"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }
    }
}

#[test]
fn svg_render_accepts_valid_edge_cases_like_zero_width_and_height() -> Result<()> {
    let zero_dim_glyph = sample_svg_glyph(1, 50.0, 500.0, 50.0, 500.0);
    let doc = Document::new(vec![zero_dim_glyph]);

    let svg = render_glyph_overlay_svg(&doc)?;
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
        "SVG header must be present"
    );
    assert!(
        svg.contains("width=\"0.50\" height=\"0.50\""),
        "Zero-dimension glyph must be clamped to 0.50 display size"
    );
    assert!(svg.ends_with("</svg>\n"));
    Ok(())
}

#[test]
fn svg_render_and_write_handle_max_page_id_without_overflow() -> Result<()> {
    let mut glyph = sample_svg_glyph(1, 50.0, 500.0, 100.0, 510.0);
    glyph.page = PageId(u32::MAX);
    let doc = Document::new(vec![glyph]);

    let svg = render_glyph_overlay_svg(&doc)?;
    assert!(
        svg.contains("id=\"page-4294967296\""),
        "SVG must render PageId(u32::MAX) as 4294967296 without wrapping or panic"
    );
    assert!(
        svg.contains("Page 4294967296 (glyphs: 1)"),
        "SVG label must display 4294967296"
    );
    assert!(
        svg.contains("data-page=\"4294967296\""),
        "data-page must serialize 4294967296"
    );
    assert!(
        svg.contains("(Page 4294967296)"),
        "title text must display (Page 4294967296)"
    );

    let mut buffer = Vec::new();
    write_glyph_overlay_svg(&doc, &mut buffer)?;
    let write_svg = String::from_utf8(buffer).expect("valid utf-8");
    assert!(write_svg.contains("id=\"page-4294967296\""));
    Ok(())
}

#[test]
fn svg_render_escapes_xml_10_forbidden_controls_and_preserves_whitespace() -> Result<()> {
    let forbidden_and_special_text = "Hello\0World\x01\x08\x0B\x0C\x0E\x1F\u{FFFE}\u{FFFF} & <tag> \"quotes\" 'apos' \t \r \n \u{D7FF} \u{E000} \u{FFFD} \u{1F600} \u{1FFFE} \x7F \u{0080} \u{009F} <U+0000> End";
    let glyph = Glyph {
        id: GlyphId(1),
        page: PageId(0),
        text: DecodedText::Mapped(forbidden_and_special_text.to_owned()),
        raw_code: vec![0x41],
        font_id: FontId(1),
        font_size: 12.0,
        bbox: Rect {
            min: Vec2 { x: 50.0, y: 100.0 },
            max: Vec2 { x: 200.0, y: 120.0 },
        },
        baseline: Vec2 { x: 50.0, y: 100.0 },
        direction: Vec2 { x: 1.0, y: 0.0 },
        render_order: 1,
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        path_clip_status: GlyphPathClipStatus::Unclipped,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: 0,
        },
    };

    let doc = Document::new(vec![glyph]);
    let svg = render_glyph_overlay_svg(&doc)?;

    assert!(
        svg.contains("&lt;U+0000&gt;"),
        "NULL must be escaped to &lt;U+0000&gt;"
    );
    assert!(
        svg.contains("&lt;U+0001&gt;"),
        "SOH must be escaped to &lt;U+0001&gt;"
    );
    assert!(
        svg.contains("&lt;U+0008&gt;"),
        "BS must be escaped to &lt;U+0008&gt;"
    );
    assert!(
        svg.contains("&lt;U+000B&gt;"),
        "VT must be escaped to &lt;U+000B&gt;"
    );
    assert!(
        svg.contains("&lt;U+000C&gt;"),
        "FF must be escaped to &lt;U+000C&gt;"
    );
    assert!(
        svg.contains("&lt;U+000E&gt;"),
        "SO must be escaped to &lt;U+000E&gt;"
    );
    assert!(
        svg.contains("&lt;U+001F&gt;"),
        "US must be escaped to &lt;U+001F&gt;"
    );
    assert!(
        svg.contains("&lt;U+FFFE&gt;"),
        "U+FFFE non-character must be escaped to &lt;U+FFFE&gt;"
    );
    assert!(
        svg.contains("&lt;U+FFFF&gt;"),
        "U+FFFF non-character must be escaped to &lt;U+FFFF&gt;"
    );

    assert!(svg.contains("&amp;"), "ampersand must be escaped to &amp;");
    assert!(
        svg.contains("&lt;tag&gt;"),
        "tag brackets must be escaped to &lt;tag&gt;"
    );
    assert!(
        svg.contains("&quot;quotes&quot;"),
        "double quotes must be escaped to &quot;"
    );
    assert!(
        svg.contains("&apos;apos&apos;"),
        "single quotes must be escaped to &apos;"
    );

    assert!(
        !svg.contains("&lt;U+0009&gt;"),
        "TAB must not be replaced by a marker"
    );
    assert!(
        !svg.contains("&lt;U+000A&gt;"),
        "LF must not be replaced by a marker"
    );
    assert!(
        !svg.contains("&lt;U+000D&gt;"),
        "CR must not be replaced by a marker"
    );
    assert!(
        svg.contains('\u{D7FF}'),
        "U+D7FF boundary must be preserved without conversion to marker"
    );
    assert!(
        svg.contains('\u{E000}'),
        "U+E000 boundary must be preserved without conversion to marker"
    );
    assert!(
        svg.contains('\u{FFFD}'),
        "U+FFFD replacement char must be preserved without conversion to marker"
    );
    assert!(
        svg.contains('\u{1F600}'),
        "Supplementary plane emoji U+1F600 must be preserved"
    );
    assert!(
        svg.contains('\u{1FFFE}'),
        "Supplementary plane U+1FFFE must be preserved"
    );
    assert!(
        svg.contains('\x7F'),
        "DEL 0x7F must be preserved according to XML 1.0 Char definition"
    );
    assert!(
        svg.contains('\u{0080}'),
        "C1 control U+0080 must be preserved according to XML 1.0 Char definition"
    );
    assert!(
        svg.contains('\u{009F}'),
        "C1 control U+009F must be preserved according to XML 1.0 Char definition"
    );

    let svg2 = render_glyph_overlay_svg(&doc)?;
    assert_eq!(svg, svg2, "SVG serialization must be 100% deterministic");
    Ok(())
}
