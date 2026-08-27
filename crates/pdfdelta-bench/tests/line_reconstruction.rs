use pdfdelta_core::{
    layout::{LineOptions, SyntheticSpace, reconstruct_lines},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphProvenance, PageId,
        Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
};

struct LineCase {
    name: &'static str,
    glyphs: Vec<Glyph>,
    expected_glyphs: Vec<Vec<GlyphId>>,
    expected_spaces: Vec<Vec<SyntheticSpace>>,
}

#[test]
fn line_reconstruction_matrix_covers_supported_prose_cases() {
    for case in cases() {
        let input_ids = sorted_ids(case.glyphs.iter().map(|glyph| glyph.id));
        let lines = reconstruct_lines(&Document::new(case.glyphs), LineOptions::default())
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));

        assert_eq!(
            lines
                .iter()
                .map(|line| line.glyphs.clone())
                .collect::<Vec<_>>(),
            case.expected_glyphs,
            "{}",
            case.name
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| line.synthetic_spaces.clone())
                .collect::<Vec<_>>(),
            case.expected_spaces,
            "{}",
            case.name
        );
        assert_eq!(
            sorted_ids(lines.iter().flat_map(|line| line.glyphs.iter().copied())),
            input_ids,
            "{} must retain every input glyph exactly once",
            case.name
        );
    }
}

fn cases() -> Vec<LineCase> {
    vec![
        LineCase {
            name: "single-column English",
            glyphs: vec![
                glyph(3, "C", 0.0, 80.0, 5.0, 10.0, 10.0, 80.0),
                glyph(1, "A", 0.0, 100.0, 5.0, 10.0, 10.0, 100.0),
                glyph(2, "B", 6.0, 100.0, 5.0, 10.0, 10.0, 100.0),
            ],
            expected_glyphs: vec![vec![GlyphId(1), GlyphId(2)], vec![GlyphId(3)]],
            expected_spaces: vec![Vec::new(), Vec::new()],
        },
        mixed_font_size_case(),
        LineCase {
            name: "superscript and following body line",
            glyphs: vec![
                glyph(1, "x", 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
                glyph(2, "2", 6.0, 5.0, 4.0, 6.0, 6.0, 5.0),
                glyph(3, "n", 0.0, -20.0, 5.0, 10.0, 10.0, -20.0),
            ],
            expected_glyphs: vec![vec![GlyphId(1), GlyphId(2)], vec![GlyphId(3)]],
            expected_spaces: vec![Vec::new(), Vec::new()],
        },
        LineCase {
            name: "missing English space",
            glyphs: vec![
                glyph(1, "A", 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
                glyph(2, "B", 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
            ],
            expected_glyphs: vec![vec![GlyphId(1), GlyphId(2)]],
            expected_spaces: vec![vec![SyntheticSpace {
                preceding: GlyphId(1),
                following: GlyphId(2),
            }]],
        },
        LineCase {
            name: "horizontal Japanese",
            glyphs: vec![
                glyph(1, "日", 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
                glyph(2, "本", 5.0, 0.0, 5.0, 10.0, 10.0, 0.0),
                glyph(3, "語", 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
            ],
            expected_glyphs: vec![vec![GlyphId(1), GlyphId(2), GlyphId(3)]],
            expected_spaces: vec![Vec::new()],
        },
        LineCase {
            name: "mixed Japanese and Latin",
            glyphs: vec![
                glyph(1, "A", 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
                glyph(2, "日", 5.0, 0.0, 5.0, 10.0, 10.0, 0.0),
                glyph(3, "B", 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
            ],
            expected_glyphs: vec![vec![GlyphId(1), GlyphId(2), GlyphId(3)]],
            expected_spaces: vec![Vec::new()],
        },
    ]
}

fn mixed_font_size_case() -> LineCase {
    let first = glyph(1, "A", 0.0, 0.0, 6.0, 14.0, 14.0, 0.0);
    let mut second = glyph(2, "b", 8.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    second.font_id = FontId(2);

    LineCase {
        name: "multiple font sizes",
        glyphs: vec![first, second],
        expected_glyphs: vec![vec![GlyphId(1), GlyphId(2)]],
        expected_spaces: vec![Vec::new()],
    }
}

fn sorted_ids(ids: impl Iterator<Item = GlyphId>) -> Vec<GlyphId> {
    let mut ids = ids.collect::<Vec<_>>();
    ids.sort_by_key(|id| id.0);
    ids
}

#[allow(clippy::too_many_arguments)]
fn glyph(
    id: u64,
    text: &str,
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
        page: PageId(0),
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
        render_order: u32::try_from(id).expect("fixture glyph id should fit in u32"),
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: u32::try_from(id).expect("fixture glyph id should fit in u32"),
        },
    }
}
