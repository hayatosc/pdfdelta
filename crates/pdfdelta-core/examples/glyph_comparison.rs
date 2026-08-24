//! Compare two programmatically constructed glyph documents.
//!
//! This example exercises the backend-independent Track B API. A PDF parser can
//! be connected later by producing the same `Document<Glyph>` representation.

use pdfdelta_core::{
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
    pipeline::{PipelineOptions, compare_extraction_outcomes},
    report::render_text,
    source::ExtractionOutcome,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let old = document(&[
        "Opening context remains stable.",
        "Release 10 remains available.",
        "Closing context remains stable.",
    ]);
    let new = document(&[
        "Opening context remains stable.",
        "Release 20 remains available.",
        "Closing context remains stable.",
    ]);

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::new(old, Vec::new())?,
        ExtractionOutcome::new(new, Vec::new())?,
        PipelineOptions::default(),
    )?;
    print!(
        "{}",
        render_text(
            &outcome.old_blocks,
            &outcome.new_blocks,
            &outcome.comparison,
            &outcome.extraction,
        )?
    );
    Ok(())
}

fn document(lines: &[&str]) -> Document<Glyph> {
    let mut glyphs = Vec::new();
    let mut next_id = 0_u64;

    for (line_index, text) in lines.iter().enumerate() {
        let mut x = 72.0;
        let y = 720.0 - line_index as f64 * 30.0;
        for character in text.chars() {
            let width = if character.is_whitespace() { 4.0 } else { 6.0 };
            let id = GlyphId(next_id);
            let glyph_text = character.to_string();
            glyphs.push(Glyph {
                id,
                text: DecodedText::Mapped(glyph_text.clone()),
                raw_code: glyph_text.into_bytes(),
                page: PageId(0),
                bbox: Rect {
                    min: Vec2 { x, y },
                    max: Vec2 {
                        x: x + width,
                        y: y + 10.0,
                    },
                },
                baseline: Vec2 { x, y },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(1),
                font_size: 10.0,
                render_order: next_id as u32,
                render_mode: TextRenderMode::Fill,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: 1,
                        generation: 0,
                    },
                    operator_index: next_id as u32,
                },
            });
            next_id += 1;
            x += width;
        }
    }

    Document::new(glyphs)
}
