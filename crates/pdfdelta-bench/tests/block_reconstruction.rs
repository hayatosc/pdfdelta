use pdfdelta_core::{
    layout::{BlockOptions, BlockRole, Line, LineId, LineTextDirection, reconstruct_blocks},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
};

struct BlockCase {
    name: &'static str,
    specs: Vec<LineSpec>,
    expected_blocks: Vec<Vec<LineId>>,
}

#[test]
fn block_reconstruction_matrix_covers_supported_prose_cases() {
    for case in cases() {
        let fixture = Fixture::new(case.specs);
        let input_lines = sorted_line_ids(fixture.lines.iter().map(|line| line.id));
        let blocks = reconstruct_blocks(&fixture.document, &fixture.lines, BlockOptions::default())
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));

        assert_eq!(
            blocks
                .iter()
                .map(|block| block.lines.clone())
                .collect::<Vec<_>>(),
            case.expected_blocks,
            "{}",
            case.name
        );
        assert!(
            blocks.iter().all(|block| block.role == BlockRole::Body),
            "{}",
            case.name
        );
        assert_eq!(
            sorted_line_ids(blocks.iter().flat_map(|block| block.lines.iter().copied())),
            input_lines,
            "{} must retain every input line exactly once",
            case.name
        );
    }
}

fn cases() -> Vec<BlockCase> {
    vec![
        BlockCase {
            name: "regular paragraph",
            specs: vec![
                LineSpec::body(1, 0, "first", 100.0),
                LineSpec::body(2, 0, "second", 88.0),
                LineSpec::body(3, 0, "third", 76.0),
            ],
            expected_blocks: vec![vec![LineId(1), LineId(2), LineId(3)]],
        },
        BlockCase {
            name: "paragraph spacing boundary",
            specs: vec![
                LineSpec::body(1, 0, "first", 100.0),
                LineSpec::body(2, 0, "second", 88.0),
                LineSpec::body(3, 0, "new paragraph", 68.0),
            ],
            expected_blocks: vec![vec![LineId(1), LineId(2)], vec![LineId(3)]],
        },
        BlockCase {
            name: "heading boundary",
            specs: vec![
                LineSpec::heading(1, "Heading", 110.0),
                LineSpec::body(2, 0, "first", 96.0),
                LineSpec::body(3, 0, "second", 84.0),
            ],
            expected_blocks: vec![vec![LineId(1)], vec![LineId(2), LineId(3)]],
        },
        BlockCase {
            name: "page boundary without cadence evidence",
            specs: vec![
                LineSpec::body(1, 0, "page zero", 10.0),
                LineSpec::body(2, 1, "page one", 100.0),
            ],
            expected_blocks: vec![vec![LineId(1)], vec![LineId(2)]],
        },
        BlockCase {
            name: "page boundary with matching cadence",
            specs: two_page_body([100.0, 88.0, 76.0], [100.0, 88.0, 76.0]),
            expected_blocks: vec![vec![
                LineId(1),
                LineId(2),
                LineId(3),
                LineId(4),
                LineId(5),
                LineId(6),
            ]],
        },
        BlockCase {
            name: "page boundary with mismatched cadence",
            specs: two_page_body([100.0, 88.0, 76.0], [100.0, 83.0, 66.0]),
            expected_blocks: vec![
                vec![LineId(1), LineId(2), LineId(3)],
                vec![LineId(4), LineId(5), LineId(6)],
            ],
        },
    ]
}

fn two_page_body(old_y: [f64; 3], new_y: [f64; 3]) -> Vec<LineSpec> {
    old_y
        .into_iter()
        .enumerate()
        .map(|(index, y)| LineSpec::body(index as u64 + 1, 0, "old page", y))
        .chain(
            new_y
                .into_iter()
                .enumerate()
                .map(|(index, y)| LineSpec::body(index as u64 + 4, 1, "new page", y)),
        )
        .collect()
}

fn sorted_line_ids(ids: impl Iterator<Item = LineId>) -> Vec<LineId> {
    let mut ids = ids.collect::<Vec<_>>();
    ids.sort_by_key(|id| id.0);
    ids
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
            let render_order = u32::try_from(spec.id).expect("fixture id should fit in u32");
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
                render_order,
                render_mode: TextRenderMode::Fill,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: spec.page + 1,
                        generation: 0,
                    },
                    operator_index: render_order,
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
                text_direction: LineTextDirection::LeftToRight,
                render_order: render_order..=render_order,
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

    fn heading(id: u64, text: &'static str, y: f64) -> Self {
        Self {
            id,
            page: 0,
            text,
            x: 0.0,
            y,
            width: 100.0,
            height: 16.0,
            font_size: 16.0,
            font: 2,
        }
    }
}
