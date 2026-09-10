#[path = "../examples/support/glyph_fixture.rs"]
mod glyph_fixture;

use std::{collections::BTreeSet, path::Path};

use pdfdelta_core::{
    diff::TextSpan,
    model::{Document, GlyphId},
    normalize::{BlockText, TextSourceAtom},
    pipeline::{PipelineOptions, compare_extraction_outcomes},
    source::ExtractionOutcome,
};

fn changed_sources(blocks: &[BlockText], span: Option<&TextSpan>) -> Vec<GlyphId> {
    let Some(span) = span else {
        return Vec::new();
    };
    assert_eq!(span.blocks.len(), 1, "the captured stamp is a single line");
    let block = blocks
        .iter()
        .find(|block| block.block == span.blocks[0])
        .expect("source block");
    let tokens = block
        .canonical
        .comparable_tokens_with_sources()
        .expect("source tokens");
    tokens[span.comparable_range.start..span.comparable_range.end]
        .iter()
        .flat_map(|(_, source)| source.atoms.iter())
        .map(|atom| match atom {
            TextSourceAtom::Glyph(id) => *id,
            _ => panic!("unchanged layout separators must not be marked changed"),
        })
        .collect()
}

#[test]
fn captured_rotated_stamp_preserves_exact_changed_glyphs() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/issue20");
    for (reverse, permuted) in [(false, false), (true, false), (false, true)] {
        let mut documents = ["old", "new"].map(|side| {
            glyph_fixture::read(&fixture_root.join(format!("attention-{side}.glyphs.json")))
                .expect("source-backed stamp fixture")
        });
        let mut expected = [
            BTreeSet::from([GlyphId(2442), GlyphId(2455), GlyphId(2457), GlyphId(2459)]),
            BTreeSet::from([GlyphId(2442), GlyphId(2456), GlyphId(2458)]),
        ];
        if reverse {
            documents.swap(0, 1);
            expected.swap(0, 1);
        }
        if permuted {
            documents = documents.map(|document| {
                let (mut glyphs, lines) = document.into_parts();
                glyphs.reverse();
                Document::with_vector_lines(glyphs, lines)
            });
        }
        let [old, new] = documents;
        let outcome = compare_extraction_outcomes(
            ExtractionOutcome::complete(old),
            ExtractionOutcome::complete(new),
            PipelineOptions::default(),
        )
        .expect("production source comparison");
        for (side, expected) in expected.iter().enumerate() {
            let reported = outcome
                .comparison
                .changes
                .iter()
                .flat_map(|change| &change.occurrences)
                .flat_map(|occurrence| {
                    if side == 0 {
                        changed_sources(&outcome.old_blocks, occurrence.old_span.as_ref())
                    } else {
                        changed_sources(&outcome.new_blocks, occurrence.new_span.as_ref())
                    }
                })
                .collect::<Vec<_>>();
            let unique = reported.iter().copied().collect::<BTreeSet<_>>();
            assert_eq!(reported.len(), unique.len(), "a source is owned once");
            assert_eq!(
                &unique, expected,
                "unchanged spaces and month letters remain equal"
            );
        }
    }
}
