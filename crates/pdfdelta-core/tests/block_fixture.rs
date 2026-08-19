use pdfdelta_core::{
    Error,
    layout::{BlockOptions, BlockRole, Line, LineId, reconstruct_blocks},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
};

fn options() -> BlockOptions {
    BlockOptions {
        vertical_proximity_weight: 0.25,
        horizontal_overlap_weight: 0.25,
        indent_similarity_weight: 0.25,
        font_continuity_weight: 0.25,
        min_join_score: 0.75,
        max_vertical_gap_height_ratio: 0.8,
        max_indent_height_ratio: 0.5,
        min_horizontal_overlap_ratio: 0.8,
        min_font_similarity: 0.8,
    }
}

#[test]
fn groups_regular_lines_into_one_body_block() {
    let fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "first", 100.0),
        LineSpec::body(2, 0, "second", 88.0),
        LineSpec::body(3, 0, "third", 76.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("regular paragraph should be grouped");

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].lines, [LineId(1), LineId(2), LineId(3)]);
    assert_eq!(blocks[0].role, BlockRole::Body);
}

#[test]
fn separates_paragraphs_with_a_large_relative_gap() {
    let fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "first", 100.0),
        LineSpec::body(2, 0, "second", 88.0),
        LineSpec::body(3, 0, "new paragraph", 68.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("paragraph spacing should be resolved");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1), LineId(2)]);
    assert_eq!(blocks[1].lines, [LineId(3)]);
}

#[test]
fn separates_a_heading_from_body_text_by_font_continuity() {
    let fixture = Fixture::new(vec![
        LineSpec {
            id: 1,
            page: 0,
            text: "Heading",
            x: 0.0,
            y: 110.0,
            width: 100.0,
            height: 16.0,
            font_size: 16.0,
            font: 2,
        },
        LineSpec::body(2, 0, "first", 96.0),
        LineSpec::body(3, 0, "second", 84.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("heading boundary should be resolved");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1)]);
    assert_eq!(blocks[1].lines, [LineId(2), LineId(3)]);
}

#[test]
fn keeps_page_boundaries_split_without_cross_page_evidence() {
    let fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "page one", 10.0),
        LineSpec::body(2, 1, "page two", 100.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("page boundary should remain conservative");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1)]);
    assert_eq!(blocks[1].lines, [LineId(2)]);
}

#[test]
fn rejects_unknown_and_unassigned_glyph_evidence() {
    let mut fixture = Fixture::new(vec![LineSpec::body(1, 0, "known", 100.0)]);
    fixture.lines[0].glyphs[0] = GlyphId(99);

    let error = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect_err("unknown glyph must not be dropped");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("unknown glyph")));
}

#[test]
fn rejects_invalid_score_weights() {
    let fixture = Fixture::new(vec![LineSpec::body(1, 0, "line", 100.0)]);
    let mut invalid_options = options();
    invalid_options.font_continuity_weight = 0.5;

    let error = reconstruct_blocks(&fixture.document, &fixture.lines, invalid_options)
        .expect_err("weights must sum to one");

    assert!(matches!(error, Error::InvalidConfiguration(message) if message.contains("sum")));
}

#[test]
fn rejects_duplicate_line_ids() {
    let mut fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "first", 100.0),
        LineSpec::body(2, 0, "second", 88.0),
    ]);
    fixture.lines[1].id = LineId(1);

    let error = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect_err("line ids must be unique");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("duplicate line")));
}

#[test]
fn rejects_a_line_bbox_larger_than_its_glyph_union() {
    let mut fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "far", 100.0),
        LineSpec::body(2, 0, "near", 0.0),
    ]);
    fixture.lines[0].bbox.min.y = 0.0;

    let error = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect_err("line geometry must come from its glyphs");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("glyph union")));
}

#[test]
fn rejects_non_finite_projected_width_from_finite_coordinates() {
    let mut fixture = Fixture::new(vec![LineSpec::body(1, 0, "wide", 100.0)]);
    let mut glyphs = fixture.document.clone().into_items();
    glyphs[0].bbox.min.x = -f64::MAX;
    glyphs[0].bbox.max.x = f64::MAX;
    fixture.lines[0].bbox = glyphs[0].bbox;
    fixture.document = Document::new(glyphs);

    let error = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect_err("unrepresentable projected width must be rejected");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("projected geometry")));
}

struct Fixture {
    document: Document<Glyph>,
    lines: Vec<Line>,
}

impl Fixture {
    fn new(specs: Vec<LineSpec>) -> Self {
        let mut glyphs = Vec::with_capacity(specs.len());
        let mut lines = Vec::with_capacity(specs.len());
        for spec in specs {
            let glyph_id = GlyphId(spec.id);
            let page = PageId(spec.page);
            let bbox = Rect {
                min: Vec2 {
                    x: spec.x,
                    y: spec.y,
                },
                max: Vec2 {
                    x: spec.x + spec.width,
                    y: spec.y + spec.height,
                },
            };
            glyphs.push(Glyph {
                id: glyph_id,
                text: DecodedText::Mapped(spec.text.to_owned()),
                raw_code: spec.text.as_bytes().to_vec(),
                page,
                bbox,
                baseline: Vec2 {
                    x: spec.x,
                    y: spec.y,
                },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(spec.font),
                font_size: spec.font_size,
                render_order: spec.id as u32,
                render_mode: TextRenderMode::Fill,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: spec.page + 1,
                        generation: 0,
                    },
                    operator_index: spec.id as u32,
                },
            });
            lines.push(Line {
                id: LineId(spec.id),
                page,
                glyphs: vec![glyph_id],
                synthetic_spaces: Vec::new(),
                bbox,
                baseline: Vec2 {
                    x: spec.x,
                    y: spec.y,
                },
                direction: Vec2 { x: 1.0, y: 0.0 },
            });
        }
        Self {
            document: Document::new(glyphs),
            lines,
        }
    }
}

struct LineSpec {
    id: u64,
    page: u32,
    text: &'static str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: f64,
    font: u32,
}

impl LineSpec {
    fn body(id: u64, page: u32, text: &'static str, y: f64) -> Self {
        Self {
            id,
            page,
            text,
            x: 0.0,
            y,
            width: 100.0,
            height: 10.0,
            font_size: 10.0,
            font: 1,
        }
    }
}
