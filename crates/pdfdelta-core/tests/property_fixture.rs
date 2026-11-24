//! Property tests over programmatically constructed `Document<Glyph>` fixtures.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;

use pdfdelta_core::{
    layout::{
        Block, BlockId, BlockOptions, BlockRole, Line, LineId, LineOptions, reconstruct_blocks,
        reconstruct_lines,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::{BlockText, normalize_blocks},
    pdf::ObjectRef,
};

/// Word alphabet mixing repeated words, numeric runs for masking, line-break
/// punctuation and hyphen boundaries, decomposed Unicode, and CJK (ASCII escapes).
const WORDS: [&str; 14] = [
    "alpha",
    "beta",
    "gamma",
    "delta",
    "10",
    "20",
    "status:",
    "ready,",
    "adminis-",
    "tration",
    "e\u{301}clair",
    "caf\u{e9}",
    "\u{8a2d}\u{5b9a}",
    "\u{4fdd}\u{5b58}",
];

struct GlyphFixture {
    document: Document<Glyph>,
    lines: Vec<Line>,
    blocks: Vec<Block>,
}

proptest! {
    #[test]
    fn normalize_is_idempotent(block_words in arb_block_words()) {
        let fixture = fixture_from(1, block_words);
        let first = normalize_blocks(&fixture.document, &fixture.lines, &fixture.blocks)
            .expect("normalization should succeed");

        for block_text in &first {
            let second = normalize_canonical_text(&block_text.canonical.text)?;
            prop_assert_eq!(&second.canonical.text, &block_text.canonical.text);
            prop_assert_eq!(&second.matching, &block_text.matching);
            prop_assert_eq!(&second.matching_tokens, &block_text.matching_tokens);
        }
    }

    #[test]
    fn line_reconstruction_partitions_every_glyph(block_words in arb_block_words()) {
        let fixture = fixture_from(1, block_words);
        let render_orders = fixture
            .document
            .items()
            .iter()
            .map(|glyph| (glyph.id, glyph.render_order))
            .collect::<HashMap<_, _>>();
        let lines = reconstruct_lines(&fixture.document, LineOptions::default())
            .expect("valid synthetic geometry should reconstruct");

        let mut line_ids = HashSet::new();
        let mut assigned = HashSet::new();
        for line in &lines {
            prop_assert!(line_ids.insert(line.id), "duplicate line id {}", line.id.0);
            prop_assert!(!line.glyphs.is_empty(), "line {} has no glyphs", line.id.0);
            for glyph_id in &line.glyphs {
                prop_assert!(
                    assigned.insert(*glyph_id),
                    "glyph {:?} is assigned to more than one line",
                    glyph_id
                );
                let render_order = render_orders[glyph_id];
                prop_assert!(
                    line.render_order.contains(&render_order),
                    "line {} range {:?} excludes glyph {} render order {}",
                    line.id.0,
                    line.render_order,
                    glyph_id.0,
                    render_order
                );
            }
        }
        prop_assert_eq!(assigned.len(), fixture.document.items().len());
    }

    #[test]
    fn block_reconstruction_covers_every_line_and_keeps_normalization_order(
        block_words in arb_block_words(),
    ) {
        let fixture = fixture_from(1, block_words);
        let lines = reconstruct_lines(&fixture.document, LineOptions::default())
            .expect("valid synthetic geometry should reconstruct");
        let blocks = reconstruct_blocks(&fixture.document, &lines, BlockOptions::default())
            .expect("every glyph is assigned to a line");

        let expected = lines.iter().map(|line| line.id).collect::<HashSet<_>>();
        let actual = blocks
            .iter()
            .flat_map(|block| block.lines.iter().copied())
            .collect::<HashSet<_>>();
        prop_assert_eq!(actual, expected, "block reconstruction changed line coverage");
        prop_assert_eq!(
            blocks
                .iter()
                .flat_map(|block| block.lines.iter())
                .count(),
            lines.len(),
            "a line must belong to exactly one block"
        );
        let mut block_ids = HashSet::new();
        for block in &blocks {
            prop_assert!(block_ids.insert(block.id), "duplicate block id {}", block.id.0);
        }

        let normalized = normalize_blocks(&fixture.document, &lines, &blocks)
            .expect("reconstructed blocks should normalize");
        prop_assert_eq!(normalized.len(), blocks.len());
        for (text, block) in normalized.iter().zip(&blocks) {
            prop_assert_eq!(text.block, block.id);
        }
    }

}

fn arb_block_words() -> impl Strategy<Value = Vec<Vec<Vec<String>>>> {
    proptest::collection::vec(
        proptest::collection::vec(proptest::collection::vec(any_word(), 1..=3), 1..=3),
        1..=3,
    )
}

fn any_word() -> impl Strategy<Value = String> {
    prop::sample::select(&WORDS).prop_map(str::to_owned)
}

fn normalize_canonical_text(text: &str) -> Result<BlockText, TestCaseError> {
    let line_words = text
        .split('\n')
        .map(|segment| vec![segment.to_owned()])
        .collect();
    let fixture = fixture_from(1, vec![line_words]);
    let blocks = normalize_blocks(&fixture.document, &fixture.lines, &fixture.blocks)
        .expect("normalization should succeed");
    if blocks.len() != 1 {
        return Err(TestCaseError::fail(format!(
            "canonical re-normalization should yield one block, got {}",
            blocks.len()
        )));
    }
    Ok(blocks.into_iter().next().expect("one block"))
}

fn fixture_from(id_base: u64, block_words: Vec<Vec<Vec<String>>>) -> GlyphFixture {
    let mut glyphs = Vec::new();
    let mut lines = Vec::new();
    let mut blocks = Vec::new();
    let mut next_glyph_id = id_base;
    let mut next_line_id = id_base;
    let mut baseline_y = 0.0;

    for (block_index, line_words) in block_words.into_iter().enumerate() {
        let mut block_lines = Vec::new();
        for words in line_words {
            let mut line_glyphs = Vec::new();
            let mut x = 0.0;
            for word in words {
                glyphs.push(positioned_glyph(next_glyph_id, &word, x, baseline_y));
                line_glyphs.push(GlyphId(next_glyph_id));
                next_glyph_id += 1;
                x += FIXTURE_ADVANCE;
            }
            let width = if line_glyphs.is_empty() {
                0.0
            } else {
                x - FIXTURE_ADVANCE + FIXTURE_GLYPH_WIDTH
            };
            block_lines.push(LineId(next_line_id));
            lines.push(positioned_line(
                next_line_id,
                line_glyphs,
                width,
                baseline_y,
            ));
            next_line_id += 1;
            baseline_y -= FIXTURE_LINE_STEP;
        }
        baseline_y -= FIXTURE_BLOCK_GAP;
        blocks.push(Block {
            id: BlockId(id_base + block_index as u64),
            lines: block_lines,
            role: BlockRole::Body,
        });
    }

    GlyphFixture {
        document: Document::new(glyphs),
        lines,
        blocks,
    }
}

const FIXTURE_ADVANCE: f64 = 10.0;
const FIXTURE_GLYPH_WIDTH: f64 = 8.0;
const FIXTURE_GLYPH_HEIGHT: f64 = 10.0;
const FIXTURE_LINE_STEP: f64 = 12.0;
const FIXTURE_BLOCK_GAP: f64 = 18.0;
fn positioned_line(id: u64, glyphs: Vec<GlyphId>, width: f64, baseline_y: f64) -> Line {
    Line {
        id: LineId(id),
        page: PageId(0),
        glyphs,
        synthetic_spaces: Vec::new(),
        bbox: Rect {
            min: Vec2 {
                x: 0.0,
                y: baseline_y,
            },
            max: Vec2 {
                x: width,
                y: baseline_y + FIXTURE_GLYPH_HEIGHT,
            },
        },
        baseline: Vec2 {
            x: 0.0,
            y: baseline_y,
        },
        direction: Vec2 { x: 1.0, y: 0.0 },
        text_direction: pdfdelta_core::layout::LineTextDirection::LeftToRight,
        render_order: id as u32..=id as u32,
    }
}

fn positioned_glyph(id: u64, text: &str, x: f64, baseline_y: f64) -> Glyph {
    Glyph {
        id: GlyphId(id),
        raw_code: text.as_bytes().to_vec(),
        text: DecodedText::Mapped(text.to_owned()),
        page: PageId(0),
        bbox: Rect {
            min: Vec2 { x, y: baseline_y },
            max: Vec2 {
                x: x + FIXTURE_GLYPH_WIDTH,
                y: baseline_y + FIXTURE_GLYPH_HEIGHT,
            },
        },
        baseline: Vec2 { x, y: baseline_y },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size: 10.0,
        render_order: id as u32,
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        path_clip_status: GlyphPathClipStatus::Unclipped,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: id as u32,
        },
    }
}
