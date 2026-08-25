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
        max_cross_page_indent_height_ratio: 0.25,
        min_cross_page_horizontal_overlap_ratio: 0.9,
        min_cross_page_font_similarity: 0.9,
        max_cross_page_cadence_difference: 0.1,
        repeated_edge_line_limit: 1,
        repeated_min_pages: 3,
        min_repeated_margin_font_similarity: 0.95,
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
fn keeps_horizontal_body_and_vertical_label_in_separate_blocks() {
    let mut fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "body", 100.0),
        LineSpec::body(2, 0, "Print or type.", 95.0),
    ]);
    let mut glyphs = fixture.document.clone().into_items();
    glyphs[1].direction = Vec2 { x: 0.0, y: 1.0 };
    fixture.document = Document::new(glyphs);
    fixture.lines[1].direction = Vec2 { x: 0.0, y: 1.0 };

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("axis-aligned orientations should be preserved");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1)]);
    assert_eq!(blocks[1].lines, [LineId(2)]);
}

#[test]
fn keeps_tilted_lines_in_separate_blocks() {
    let mut fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "first tilted line", 100.0),
        LineSpec::body(2, 0, "second tilted line", 95.0),
    ]);
    let direction = Vec2 {
        x: 0.999_657_376_647_797_5,
        y: 0.026_174_974_950_197_414,
    };
    let mut glyphs = fixture.document.into_items();
    for (glyph, line) in glyphs.iter_mut().zip(&mut fixture.lines) {
        glyph.direction = direction;
        line.direction = direction;
    }
    fixture.document = Document::new(glyphs);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("tilted lines should be preserved");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1)]);
    assert_eq!(blocks[1].lines, [LineId(2)]);
}

#[test]
fn keeps_cross_axis_separated_vertical_labels_in_singleton_blocks() {
    let first = LineSpec::body(1, 0, "left label", 100.0);
    let mut second = LineSpec::body(2, 0, "right label", 100.0);
    second.x = 200.0;
    let mut fixture = Fixture::new(vec![first, second]);
    let mut glyphs = fixture.document.into_items();
    for (glyph, line) in glyphs.iter_mut().zip(&mut fixture.lines) {
        glyph.direction = Vec2 { x: 0.0, y: 1.0 };
        line.direction = Vec2 { x: 0.0, y: 1.0 };
    }
    fixture.document = Document::new(glyphs);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("separate rotated labels should remain valid");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1)]);
    assert_eq!(blocks[1].lines, [LineId(2)]);
    assert!(blocks.iter().all(|block| block.role == BlockRole::Body));
}

#[test]
fn excludes_vertical_labels_from_repeated_margin_roles() {
    let mut fixture = Fixture::new(vec![
        LineSpec::margin(1, 0, "Repeated label", 120.0),
        LineSpec::body(2, 0, "page zero first", 100.0),
        LineSpec::body(3, 0, "page zero second", 88.0),
        LineSpec::margin(4, 0, "footer zero", 0.0),
        LineSpec::margin(5, 1, "Repeated label", 120.0),
        LineSpec::body(6, 1, "page one first", 100.0),
        LineSpec::body(7, 1, "page one second", 88.0),
        LineSpec::margin(8, 1, "footer one", 0.0),
        LineSpec::margin(9, 2, "Repeated label", 120.0),
        LineSpec::body(10, 2, "page two first", 100.0),
        LineSpec::body(11, 2, "page two second", 88.0),
        LineSpec::margin(12, 2, "footer two", 0.0),
    ]);
    let mut glyphs = fixture.document.into_items();
    for index in [0, 4, 8] {
        glyphs[index].direction = Vec2 { x: 0.0, y: 1.0 };
        fixture.lines[index].direction = Vec2 { x: 0.0, y: 1.0 };
    }
    fixture.document = Document::new(glyphs);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("vertical labels should not affect horizontal repeated margins");

    for line in [LineId(1), LineId(5), LineId(9)] {
        let block = blocks
            .iter()
            .find(|block| block.lines.contains(&line))
            .expect("rotated label should be preserved");
        assert_eq!(block.lines, [line]);
        assert_eq!(block.role, BlockRole::Body);
    }
}

#[test]
fn rejects_opposite_direction_inside_vertical_line() {
    let mut fixture = Fixture::new(vec![LineSpec::body(1, 0, "label", 100.0)]);
    let mut glyphs = fixture.document.into_items();
    glyphs[0].direction = Vec2 { x: 0.0, y: -1.0 };
    fixture.document = Document::new(glyphs);
    fixture.lines[0].direction = Vec2 { x: 0.0, y: 1.0 };

    let error = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect_err("opposite glyph direction must be rejected");

    assert!(
        matches!(error, Error::Unresolved(message) if message.contains("inconsistent direction"))
    );
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
fn joins_body_across_pages_when_both_line_cadences_match() {
    let fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "page zero first", 100.0),
        LineSpec::body(2, 0, "page zero second", 88.0),
        LineSpec::body(3, 0, "page zero third", 76.0),
        LineSpec::body(4, 1, "page one first", 100.0),
        LineSpec::body(5, 1, "page one second", 88.0),
        LineSpec::body(6, 1, "page one third", 76.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("matching page cadence should join");

    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].lines,
        [
            LineId(1),
            LineId(2),
            LineId(3),
            LineId(4),
            LineId(5),
            LineId(6),
        ]
    );
}

#[test]
fn does_not_join_across_a_missing_page() {
    let fixture = Fixture::new(vec![
        LineSpec::body(1, 0, "page zero first", 100.0),
        LineSpec::body(2, 0, "page zero second", 88.0),
        LineSpec::body(3, 0, "page zero third", 76.0),
        LineSpec::body(4, 2, "page two first", 100.0),
        LineSpec::body(5, 2, "page two second", 88.0),
        LineSpec::body(6, 2, "page two third", 76.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("missing page evidence should remain conservative");

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].lines, [LineId(1), LineId(2), LineId(3)]);
    assert_eq!(blocks[1].lines, [LineId(4), LineId(5), LineId(6)]);
}

#[test]
fn preserves_repeated_headers_and_footers_as_separate_roles() {
    let fixture = Fixture::new(vec![
        LineSpec::margin(1, 0, "Repeated header", 120.0),
        LineSpec::body(2, 0, "page zero first", 100.0),
        LineSpec::body(3, 0, "page zero second", 88.0),
        LineSpec::margin(4, 0, "Repeated footer", 0.0),
        LineSpec::margin(5, 1, "Repeated header", 120.0),
        LineSpec::body(6, 1, "page one first", 100.0),
        LineSpec::body(7, 1, "page one second", 88.0),
        LineSpec::margin(8, 1, "Repeated footer", 0.0),
        LineSpec::margin(9, 2, "Repeated header", 120.0),
        LineSpec::body(10, 2, "page two first", 100.0),
        LineSpec::body(11, 2, "page two second", 88.0),
        LineSpec::margin(12, 2, "Repeated footer", 0.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("running matter should be classified without being dropped");

    assert_eq!(
        blocks
            .iter()
            .filter(|block| block.role == BlockRole::RepeatedHeader)
            .count(),
        3
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|block| block.role == BlockRole::RepeatedFooter)
            .count(),
        3
    );
    let body = blocks
        .iter()
        .find(|block| block.role == BlockRole::Body)
        .expect("body block should remain");
    assert_eq!(
        body.lines,
        [
            LineId(2),
            LineId(3),
            LineId(6),
            LineId(7),
            LineId(10),
            LineId(11),
        ]
    );

    let mut preserved_lines: Vec<_> = blocks
        .iter()
        .flat_map(|block| block.lines.iter().copied())
        .collect();
    preserved_lines.sort_by_key(|line| line.0);
    assert_eq!(preserved_lines, (1..=12).map(LineId).collect::<Vec<_>>());
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

    fn margin(id: u64, page: u32, text: &'static str, y: f64) -> Self {
        Self {
            id,
            page,
            text,
            x: 0.0,
            y,
            width: 100.0,
            height: 8.0,
            font_size: 8.0,
            font: 2,
        }
    }

    fn column(id: u64, page: u32, text: &'static str, x: f64, y: f64, width: f64) -> Self {
        Self {
            id,
            page,
            text,
            x,
            y,
            width,
            height: 10.0,
            font_size: 10.0,
            font: 1,
        }
    }
}

#[test]
fn two_column_layout_groups_columns_without_interleaving() {
    // Interleaved in Y-order: L1 (y=700), R1 (y=700), L2 (y=685), R2 (y=685)
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "left column line 1", 50.0, 700.0, 200.0),
        LineSpec::column(2, 0, "right column line 1", 350.0, 700.0, 200.0),
        LineSpec::column(3, 0, "left column line 2", 50.0, 685.0, 200.0),
        LineSpec::column(4, 0, "right column line 2", 350.0, 685.0, 200.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("two column blocks should reconstruct");

    assert_eq!(blocks.len(), 2, "Should produce two column blocks");
    assert_eq!(
        blocks[0].lines,
        [LineId(1), LineId(3)],
        "Left column block lines"
    );
    assert_eq!(
        blocks[1].lines,
        [LineId(2), LineId(4)],
        "Right column block lines"
    );
}

#[test]
fn table_cells_remain_separate_blocks_without_merging_into_columns() {
    // 3-row, 2-column table:
    // Row 1: "Item" (L1), "Price" (L2)
    // Row 2: "Apple" (L3), "$1.50" (L4)
    // Row 3: "Banana" (L5), "$0.75" (L6)
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "Item", 50.0, 700.0, 60.0),
        LineSpec::column(2, 0, "Price", 250.0, 700.0, 60.0),
        LineSpec::column(3, 0, "Apple", 50.0, 685.0, 60.0),
        LineSpec::column(4, 0, "$1.50", 250.0, 685.0, 60.0),
        LineSpec::column(5, 0, "Banana", 50.0, 670.0, 60.0),
        LineSpec::column(6, 0, "$0.75", 250.0, 670.0, 60.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("table blocks should reconstruct");

    assert_eq!(
        blocks.len(),
        6,
        "Table cells must remain separate blocks instead of merging into column paragraphs"
    );
    for (i, block) in blocks.iter().enumerate() {
        assert_eq!(
            block.lines.len(),
            1,
            "Block {} should contain exactly 1 cell line, but got {:?}",
            i,
            block.lines
        );
    }
}

#[test]
fn form_key_value_fields_remain_separate_blocks() {
    // 3-row key-value form fields:
    // Row 1: "First Name:" (L1), "Alice" (L2)
    // Row 2: "Last Name:" (L3), "Smith" (L4)
    // Row 3: "Role:" (L5), "Engineer" (L6)
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "First Name:", 50.0, 500.0, 70.0),
        LineSpec::column(2, 0, "Alice", 160.0, 500.0, 50.0),
        LineSpec::column(3, 0, "Last Name:", 50.0, 480.0, 70.0),
        LineSpec::column(4, 0, "Smith", 160.0, 480.0, 50.0),
        LineSpec::column(5, 0, "Role:", 50.0, 460.0, 70.0),
        LineSpec::column(6, 0, "Engineer", 160.0, 460.0, 60.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("form blocks should reconstruct");

    assert_eq!(
        blocks.len(),
        6,
        "Form field labels and values must not merge into paragraph blocks"
    );
}

#[test]
fn multi_column_three_by_three_grid_preserves_cell_boundaries() {
    // 3 columns x 3 rows grid
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "A1", 50.0, 700.0, 40.0),
        LineSpec::column(2, 0, "B1", 150.0, 700.0, 40.0),
        LineSpec::column(3, 0, "C1", 250.0, 700.0, 40.0),
        LineSpec::column(4, 0, "A2", 50.0, 685.0, 40.0),
        LineSpec::column(5, 0, "B2", 150.0, 685.0, 40.0),
        LineSpec::column(6, 0, "C2", 250.0, 685.0, 40.0),
        LineSpec::column(7, 0, "A3", 50.0, 670.0, 40.0),
        LineSpec::column(8, 0, "B3", 150.0, 670.0, 40.0),
        LineSpec::column(9, 0, "C3", 250.0, 670.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("3x3 grid blocks should reconstruct");

    assert_eq!(
        blocks.len(),
        9,
        "All 9 grid cells must remain separate blocks"
    );
}

#[test]
fn table_with_multiline_cell_groups_within_cell_but_separates_rows() {
    // Row 1 Col 1 has 2 lines: L1 (y=700), L2 (y=688)
    // Row 1 Col 2 has 1 line spanning the row: L3 (y=690..708)
    // Row 2 Col 1 has 1 line: L4 (y=665)
    // Row 2 Col 2 has 1 line: L5 (y=665)
    let mut l3 = LineSpec::column(3, 0, "Price Category", 250.0, 690.0, 80.0);
    l3.height = 18.0;
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "Product", 50.0, 700.0, 60.0),
        LineSpec::column(2, 0, "Description", 50.0, 688.0, 60.0),
        l3,
        LineSpec::column(4, 0, "Widget", 50.0, 665.0, 60.0),
        LineSpec::column(5, 0, "$10.00", 250.0, 665.0, 60.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("multiline cell table should reconstruct");

    assert_eq!(blocks.len(), 4, "Should produce 4 distinct cell blocks");
    assert_eq!(
        blocks[0].lines,
        [LineId(1), LineId(2)],
        "Multiline cell lines 1 and 2 should group into one cell block"
    );
    assert_eq!(blocks[1].lines, [LineId(4)], "Row 2 Col 1 cell block");
    assert_eq!(blocks[2].lines, [LineId(3)], "Row 1 Col 2 cell block");
    assert_eq!(blocks[3].lines, [LineId(5)], "Row 2 Col 2 cell block");
}

#[test]
fn prose_columns_with_short_terminating_line_remains_single_block() {
    // 2-column prose article where the final line in each column is a short paragraph ending.
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "left column full width line one", 50.0, 700.0, 200.0),
        LineSpec::column(
            2,
            0,
            "right column full width line one",
            350.0,
            700.0,
            200.0,
        ),
        LineSpec::column(3, 0, "left column full width line two", 50.0, 685.0, 200.0),
        LineSpec::column(
            4,
            0,
            "right column full width line two",
            350.0,
            685.0,
            200.0,
        ),
        LineSpec::column(5, 0, "left short end.", 50.0, 670.0, 70.0),
        LineSpec::column(6, 0, "right short end.", 350.0, 670.0, 70.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("prose columns should reconstruct");

    assert_eq!(
        blocks.len(),
        2,
        "Prose columns with short terminating lines must remain continuous paragraph blocks"
    );
    assert_eq!(
        blocks[0].lines,
        [LineId(1), LineId(3), LineId(5)],
        "Left column paragraph block"
    );
    assert_eq!(
        blocks[1].lines,
        [LineId(2), LineId(4), LineId(6)],
        "Right column paragraph block"
    );
}

#[test]
fn three_column_prose_with_short_terminating_lines_remains_one_block_per_column() {
    // 3-column prose article (width 150 each) with 3 lines per column, including short terminal lines.
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "col 1 full line 1", 50.0, 700.0, 150.0),
        LineSpec::column(2, 0, "col 2 full line 1", 230.0, 700.0, 150.0),
        LineSpec::column(3, 0, "col 3 full line 1", 410.0, 700.0, 150.0),
        LineSpec::column(4, 0, "col 1 full line 2", 50.0, 685.0, 150.0),
        LineSpec::column(5, 0, "col 2 full line 2", 230.0, 685.0, 150.0),
        LineSpec::column(6, 0, "col 3 full line 2", 410.0, 685.0, 150.0),
        LineSpec::column(7, 0, "col 1 end.", 50.0, 670.0, 60.0),
        LineSpec::column(8, 0, "col 2 end.", 230.0, 670.0, 60.0),
        LineSpec::column(9, 0, "col 3 end.", 410.0, 670.0, 60.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("3-column prose should reconstruct");

    assert_eq!(
        blocks.len(),
        3,
        "3-column prose must produce exactly 3 column blocks, not 9 line blocks"
    );
    assert_eq!(
        blocks[0].lines,
        [LineId(1), LineId(4), LineId(7)],
        "Column 1 paragraph block"
    );
    assert_eq!(
        blocks[1].lines,
        [LineId(2), LineId(5), LineId(8)],
        "Column 2 paragraph block"
    );
    assert_eq!(
        blocks[2].lines,
        [LineId(3), LineId(6), LineId(9)],
        "Column 3 paragraph block"
    );
}

#[test]
fn wide_body_column_with_narrow_sidebar_does_not_fragment_body_paragraph() {
    // Wide body column (width 350) accompanied by a narrow sidebar (width 50).
    let fixture = Fixture::new(vec![
        LineSpec::column(
            1,
            0,
            "Wide body paragraph continuous text line one",
            50.0,
            700.0,
            350.0,
        ),
        LineSpec::column(2, 0, "Tag1", 430.0, 700.0, 50.0),
        LineSpec::column(
            3,
            0,
            "Wide body paragraph continuous text line two",
            50.0,
            685.0,
            350.0,
        ),
        LineSpec::column(4, 0, "Tag2", 430.0, 685.0, 50.0),
        LineSpec::column(5, 0, "Body short ending.", 50.0, 670.0, 100.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("wide body with sidebar should reconstruct");

    // Body lines 1, 3, 5 must form a single continuous body paragraph block
    let body_block = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(1)))
        .expect("body block should exist");
    assert_eq!(
        body_block.lines,
        [LineId(1), LineId(3), LineId(5)],
        "Wide body column must remain a single paragraph block and not fragment into lines"
    );
}

#[test]
fn table_with_wide_description_column_preserves_row_boundaries() {
    // 3-column table: Col 1 (ID, width 30), Col 2 (Description, width 200), Col 3 (Price, width 40)
    // Row 1: ID 1 (L1), Desc "Alpha widget\nHigh quality" (L2, L3), Price "$10" (L4)
    // Row 2: ID 2 (L5), Desc "Beta widget" (L6), Price "$20" (L7)
    let mut l4 = LineSpec::column(4, 0, "$10", 320.0, 690.0, 40.0);
    l4.height = 18.0; // spans row 1
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "1", 50.0, 700.0, 30.0),
        LineSpec::column(2, 0, "Alpha widget", 100.0, 700.0, 200.0),
        LineSpec::column(3, 0, "High quality", 100.0, 688.0, 200.0),
        l4,
        LineSpec::column(5, 0, "2", 50.0, 665.0, 30.0),
        LineSpec::column(6, 0, "Beta widget", 100.0, 665.0, 200.0),
        LineSpec::column(7, 0, "$20", 320.0, 665.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("table with wide description should reconstruct");

    // Row 1 Description (L2, L3) should be joined as a multi-line cell
    let row1_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(2)))
        .expect("row 1 desc block should exist");
    assert_eq!(
        row1_desc.lines,
        [LineId(2), LineId(3)],
        "Row 1 description lines should join within the cell"
    );

    // Row 2 Description (L6) must NOT join with Row 1 Description
    let row2_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(6)))
        .expect("row 2 desc block should exist");
    assert_eq!(
        row2_desc.lines,
        [LineId(6)],
        "Row 2 description must be separate from Row 1"
    );
}

#[test]
fn full_width_heading_above_dense_three_by_three_table_preserves_grid_cells() {
    // Full width heading (width 500) above a dense 3x3 table (cells width 40).
    // The heading must NOT inflate column widths of the table cells below it.
    let fixture = Fixture::new(vec![
        LineSpec::column(
            1,
            0,
            "Full Width Document Title Banner Heading",
            50.0,
            750.0,
            500.0,
        ),
        LineSpec::column(2, 0, "A1", 50.0, 700.0, 40.0),
        LineSpec::column(3, 0, "B1", 150.0, 700.0, 40.0),
        LineSpec::column(4, 0, "C1", 250.0, 700.0, 40.0),
        LineSpec::column(5, 0, "A2", 50.0, 685.0, 40.0),
        LineSpec::column(6, 0, "B2", 150.0, 685.0, 40.0),
        LineSpec::column(7, 0, "C2", 250.0, 685.0, 40.0),
        LineSpec::column(8, 0, "A3", 50.0, 670.0, 40.0),
        LineSpec::column(9, 0, "B3", 150.0, 670.0, 40.0),
        LineSpec::column(10, 0, "C3", 250.0, 670.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("heading and 3x3 table should reconstruct");

    assert_eq!(
        blocks.len(),
        10,
        "Full width heading (1) plus all 9 table cells must remain 10 separate blocks"
    );
    assert_eq!(blocks[0].lines, [LineId(1)], "Heading block");
}

#[test]
fn table_with_asymmetric_multiline_cells_and_realistic_leading_preserves_row_boundaries() {
    // 3-column table: Col 1 (ID, width 30), Col 2 (Description, width 200), Col 3 (Price, width 40)
    // Row 1: ID 1 (L1, y=700), Desc Line 1 (L2, y=700), Desc Line 2 (L3, y=688), Price $10 (L4, y=694, h=18)
    // Row 2: ID 2 (L5, y=676), Desc Line 1 (L6, y=676), Price $20 (L7, y=676)
    // Vertical gap between L3 (y=688) and L6 (y=676) is 2.0 (normal line leading),
    // and row 1 has an asymmetric single-peer line at L3's baseline.
    let mut l4 = LineSpec::column(4, 0, "$10", 320.0, 694.0, 40.0);
    l4.height = 18.0;
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "1", 50.0, 700.0, 30.0),
        LineSpec::column(2, 0, "Alpha first description line", 100.0, 700.0, 200.0),
        LineSpec::column(3, 0, "Alpha second description line", 100.0, 688.0, 200.0),
        l4,
        LineSpec::column(5, 0, "2", 50.0, 676.0, 30.0),
        LineSpec::column(6, 0, "Beta first description line", 100.0, 676.0, 200.0),
        LineSpec::column(7, 0, "$20", 320.0, 676.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("asymmetric table should reconstruct");

    let row1_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(2)))
        .expect("row 1 desc block should exist");
    assert_eq!(
        row1_desc.lines,
        [LineId(2), LineId(3)],
        "Row 1 description lines must join within the cell"
    );

    let row2_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(6)))
        .expect("row 2 desc block should exist");
    assert_eq!(
        row2_desc.lines,
        [LineId(6)],
        "Row 2 description must not merge with Row 1 even with tight leading and asymmetric peers"
    );
}

#[test]
fn vertical_rotated_label_does_not_affect_horizontal_prose_or_table_width() {
    // Horizontal body column and a vertical label on the same page.
    let mut fixture = Fixture::new(vec![
        LineSpec::column(
            1,
            0,
            "Horizontal body paragraph line one",
            50.0,
            700.0,
            350.0,
        ),
        LineSpec::column(
            2,
            0,
            "Horizontal body paragraph line two",
            50.0,
            685.0,
            350.0,
        ),
        LineSpec::column(3, 0, "SIDEBAR", 450.0, 680.0, 10.0),
    ]);
    let mut glyphs = fixture.document.clone().into_items();
    glyphs[2].direction = Vec2 { x: 0.0, y: 1.0 };
    fixture.document = Document::new(glyphs);
    fixture.lines[2].direction = Vec2 { x: 0.0, y: 1.0 };

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("horizontal body with vertical label should reconstruct");

    let body_block = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(1)))
        .expect("body block should exist");
    assert_eq!(
        body_block.lines,
        [LineId(1), LineId(2)],
        "Horizontal body lines must join into a single paragraph block"
    );
}

#[test]
fn two_column_table_with_wrapped_multiline_description_and_price_preserves_rows() {
    // 2-column table: Col 1 (wide Description, width 200), Col 2 (narrow Price, width 40).
    // Row 1: Description line 1 (L1, y=700), Description wrapped line 2 (L2, y=688), Price $10 (L3, y=700).
    // Row 2: Description line 1 (L4, y=676), Price $20 (L5, y=676).
    // Leading between wrapped L2 (y=688) and next row L4 (y=676) is tight (gap = 2.0).
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "Premium Alpha Item Description", 50.0, 700.0, 200.0),
        LineSpec::column(
            2,
            0,
            "Includes extended warranty coverage",
            50.0,
            688.0,
            200.0,
        ),
        LineSpec::column(3, 0, "$10.00", 270.0, 700.0, 40.0),
        LineSpec::column(4, 0, "Standard Beta Item Description", 50.0, 676.0, 200.0),
        LineSpec::column(5, 0, "$20.00", 270.0, 676.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("2-column table should reconstruct");

    assert_eq!(
        blocks.len(),
        4,
        "2-column table must produce 4 blocks: Row 1 Desc, Row 2 Desc, Row 1 Price, Row 2 Price"
    );

    let row1_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(1)))
        .expect("row 1 desc block");
    assert_eq!(
        row1_desc.lines,
        [LineId(1), LineId(2)],
        "Row 1 description lines must join within the cell"
    );

    let row2_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(4)))
        .expect("row 2 desc block");
    assert_eq!(
        row2_desc.lines,
        [LineId(4)],
        "Row 2 description must remain separate from Row 1 description"
    );

    let row1_price = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(3)))
        .expect("row 1 price block");
    assert_eq!(row1_price.lines, [LineId(3)], "Row 1 price cell block");

    let row2_price = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(5)))
        .expect("row 2 price block");
    assert_eq!(row2_price.lines, [LineId(5)], "Row 2 price cell block");
}

#[test]
fn compact_gutter_table_preserves_cell_boundaries() {
    // 3-column table with a compact gutter of 2.5pt (0.25em with 10pt font).
    // Col 1: 50.0..90.0 (w=40)
    // Col 2: 92.5..132.5 (w=40, gutter=2.5)
    // Col 3: 135.0..175.0 (w=40, gutter=2.5)
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "A1", 50.0, 700.0, 40.0),
        LineSpec::column(2, 0, "B1", 92.5, 700.0, 40.0),
        LineSpec::column(3, 0, "C1", 135.0, 700.0, 40.0),
        LineSpec::column(4, 0, "A2", 50.0, 685.0, 40.0),
        LineSpec::column(5, 0, "B2", 92.5, 685.0, 40.0),
        LineSpec::column(6, 0, "C2", 135.0, 685.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("compact gutter table should reconstruct");

    assert_eq!(
        blocks.len(),
        6,
        "Compact gutter table cells must remain 6 separate blocks"
    );
}

#[test]
fn two_column_table_with_bottom_aligned_price_preserves_rows() {
    // 2-column table with bottom-aligned price in Row 1:
    // Row 1: Desc line 1 (L1, y=700), Desc line 2 (L2, y=688), Price $10 (L3, y=688, bottom-aligned!)
    // Row 2: Desc line 1 (L4, y=676), Price $20 (L5, y=676)
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "Premium Alpha Item Description", 50.0, 700.0, 200.0),
        LineSpec::column(
            2,
            0,
            "Includes extended warranty coverage",
            50.0,
            688.0,
            200.0,
        ),
        LineSpec::column(3, 0, "$10.00", 270.0, 688.0, 40.0),
        LineSpec::column(4, 0, "Standard Beta Item Description", 50.0, 676.0, 200.0),
        LineSpec::column(5, 0, "$20.00", 270.0, 676.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("2-column bottom-aligned table should reconstruct");

    assert_eq!(
        blocks.len(),
        4,
        "Bottom-aligned 2-column table must produce 4 blocks"
    );

    let row1_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(1)))
        .expect("row 1 desc block");
    assert_eq!(
        row1_desc.lines,
        [LineId(1), LineId(2)],
        "Row 1 description lines must join within the cell"
    );

    let row2_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(4)))
        .expect("row 2 desc block");
    assert_eq!(
        row2_desc.lines,
        [LineId(4)],
        "Row 2 description must remain separate from Row 1"
    );
}

#[test]
fn two_column_table_with_tall_row_spanning_price_preserves_rows() {
    // 2-column table with a tall row-spanning price cell in Row 1:
    // Row 1: Desc line 1 (L1, y=700), Desc line 2 (L2, y=688), Price $10 (L3, y=694, h=22, spans row 1)
    // Row 2: Desc line 1 (L4, y=676), Price $20 (L5, y=676)
    let mut l3 = LineSpec::column(3, 0, "$10.00", 270.0, 694.0, 40.0);
    l3.height = 22.0;
    let fixture = Fixture::new(vec![
        LineSpec::column(1, 0, "Premium Alpha Item Description", 50.0, 700.0, 200.0),
        LineSpec::column(
            2,
            0,
            "Includes extended warranty coverage",
            50.0,
            688.0,
            200.0,
        ),
        l3,
        LineSpec::column(4, 0, "Standard Beta Item Description", 50.0, 676.0, 200.0),
        LineSpec::column(5, 0, "$20.00", 270.0, 676.0, 40.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("2-column tall price table should reconstruct");

    let row1_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(1)))
        .expect("row 1 desc block");
    assert_eq!(
        row1_desc.lines,
        [LineId(1), LineId(2)],
        "Row 1 description lines must join within the cell"
    );

    let row2_desc = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(4)))
        .expect("row 2 desc block");
    assert_eq!(
        row2_desc.lines,
        [LineId(4)],
        "Row 2 description must remain separate from Row 1"
    );
}

#[test]
fn continuing_prose_with_late_starting_narrow_callout_does_not_fragment_paragraph() {
    // Continuous wide body paragraph (width 350) with a late-starting narrow callout at line 3.
    let fixture = Fixture::new(vec![
        LineSpec::column(
            1,
            0,
            "First line of long continuous body paragraph",
            50.0,
            700.0,
            350.0,
        ),
        LineSpec::column(2, 0, "Second short ending.", 50.0, 685.0, 120.0),
        LineSpec::column(
            3,
            0,
            "Third line of continuing body paragraph text",
            50.0,
            670.0,
            350.0,
        ),
        LineSpec::column(4, 0, "NOTE", 430.0, 670.0, 50.0),
    ]);

    let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, options())
        .expect("continuing prose with late callout should reconstruct");

    let body_block = blocks
        .iter()
        .find(|b| b.lines.contains(&LineId(1)))
        .expect("body block should exist");
    assert_eq!(
        body_block.lines,
        [LineId(1), LineId(2), LineId(3)],
        "Continuing body paragraph lines must remain a single block despite late-starting callout"
    );
}
