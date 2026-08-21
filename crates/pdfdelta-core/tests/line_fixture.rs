use pdfdelta_core::{
    Error,
    layout::{LineOptions, SyntheticSpace, reconstruct_lines},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
};

fn options() -> LineOptions {
    LineOptions {
        max_baseline_distance_ratio: 0.25,
        min_cross_axis_overlap_ratio: 0.25,
        min_direction_similarity: 0.98,
        max_inline_gap_font_size_ratio: 4.0,
        space_gap_font_size_ratio: 0.2,
        space_gap_advance_ratio: 0.5,
    }
}

#[test]
fn reconstructs_one_column_lines_in_reading_order() {
    let document = Document::new(vec![
        glyph(3, "C", 0, 0.0, 80.0, 10.0, 10.0, 10.0, 80.0),
        glyph(1, "A", 0, 0.0, 100.0, 10.0, 10.0, 10.0, 100.0),
        glyph(2, "B", 0, 11.0, 100.0, 10.0, 10.0, 10.0, 100.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("line reconstruction should work");

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].glyphs, [GlyphId(1), GlyphId(2)]);
    assert!(lines[0].synthetic_spaces.is_empty());
    assert_eq!(lines[1].glyphs, [GlyphId(3)]);
}

#[test]
fn keeps_mixed_size_superscript_on_the_same_line() {
    let document = Document::new(vec![
        glyph(1, "x", 0, 0.0, 0.0, 6.0, 10.0, 10.0, 0.0),
        glyph(2, "2", 0, 6.0, 5.0, 4.0, 6.0, 6.0, 5.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("superscript should be resolved");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(1), GlyphId(2)]);
    assert!(lines[0].baseline.y.abs() < f64::EPSILON);
}

#[test]
fn records_a_missing_english_space_between_glyphs() {
    let document = Document::new(vec![
        glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
        glyph(2, "B", 0, 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("space reconstruction should work");

    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].synthetic_spaces,
        [SyntheticSpace {
            preceding: GlyphId(1),
            following: GlyphId(2),
        }]
    );
}

#[test]
fn does_not_duplicate_an_explicit_space_glyph() {
    let document = Document::new(vec![
        glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
        glyph(2, " ", 0, 6.0, 0.0, 3.0, 10.0, 10.0, 0.0),
        glyph(3, "B", 0, 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("explicit space should be retained");

    assert_eq!(lines.len(), 1);
    assert!(lines[0].synthetic_spaces.is_empty());
}

#[test]
fn rejects_non_finite_glyph_geometry() {
    let document = Document::new(vec![glyph(1, "A", 0, 0.0, f64::NAN, 5.0, 10.0, 10.0, 0.0)]);

    let error = reconstruct_lines(&document, options()).expect_err("NaN must be rejected");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("non-finite")));
}

#[test]
fn reconstructs_axis_aligned_vertical_glyphs_in_reading_order() {
    let mut first = glyph(1, "A", 0, 0.0, 0.0, 10.0, 5.0, 10.0, 0.0);
    first.direction = Vec2 { x: 0.0, y: 1.0 };
    let mut second = glyph(2, "B", 0, 0.0, 6.0, 10.0, 5.0, 10.0, 6.0);
    second.direction = Vec2 { x: 0.0, y: 1.0 };
    let document = Document::new(vec![second, first]);

    let lines = reconstruct_lines(&document, options()).expect("vertical label should be resolved");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(1), GlyphId(2)]);
    assert_eq!(lines[0].direction, Vec2 { x: 0.0, y: 1.0 });
}

#[test]
fn reconstructs_tilted_glyphs_in_projected_order() {
    let direction = Vec2 {
        x: 0.999_657_376_647_797_5,
        y: 0.026_174_974_950_197_414,
    };
    let mut later = glyph(
        1,
        "B",
        0,
        10.0 * direction.x,
        10.0 * direction.y,
        5.0,
        10.0,
        10.0,
        10.0 * direction.y,
    );
    later.direction = direction;
    let mut earlier = glyph(2, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    earlier.direction = direction;
    let document = Document::new(vec![later, earlier]);

    let lines = reconstruct_lines(&document, options()).expect("tilted text should be resolved");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(2), GlyphId(1)]);
    assert_eq!(lines[0].direction, direction);
}

#[test]
fn keeps_orthogonal_directions_out_of_the_same_line() {
    let horizontal = glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    let mut vertical = glyph(2, "B", 0, 0.0, 0.0, 10.0, 5.0, 10.0, 0.0);
    vertical.direction = Vec2 { x: 0.0, y: 1.0 };
    let document = Document::new(vec![horizontal, vertical]);

    let lines = reconstruct_lines(&document, options())
        .expect("orthogonal axis-aligned glyphs are independently supported");

    assert_eq!(lines.len(), 2);
}

#[test]
fn accepts_negative_horizontal_writing_direction() {
    let mut horizontal = glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    horizontal.direction = Vec2 { x: -1.0, y: 0.0 };
    let document = Document::new(vec![horizontal]);

    let lines = reconstruct_lines(&document, options()).expect("horizontal direction is supported");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(1)]);
}

#[test]
fn rejects_zero_cross_axis_extent() {
    let document = Document::new(vec![glyph(1, "A", 0, 0.0, 0.0, 5.0, 0.0, 10.0, 0.0)]);

    let error = reconstruct_lines(&document, options()).expect_err("zero-height text is invalid");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("cross-axis")));
}

#[allow(clippy::too_many_arguments)]
fn glyph(
    id: u64,
    text: &str,
    page: u32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: f64,
    baseline_y: f64,
) -> Glyph {
    Glyph {
        id: GlyphId(id),
        text: DecodedText::Mapped(text.to_owned()),
        raw_code: text.as_bytes().to_vec(),
        page: PageId(page),
        bbox: Rect {
            min: Vec2 { x, y },
            max: Vec2 {
                x: x + width,
                y: y + height,
            },
        },
        baseline: Vec2 { x, y: baseline_y },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size,
        render_order: id as u32,
        render_mode: TextRenderMode::Fill,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: id as u32,
        },
    }
}
