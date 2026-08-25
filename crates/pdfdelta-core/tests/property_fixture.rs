//! SPEC 12.5 property tests over programmatically constructed `Document<Glyph>` fixtures.

use std::collections::HashSet;

use proptest::prelude::*;

use pdfdelta_core::{
    alignment::{
        AlignmentKind, AlignmentOptions, CandidateGenerator, CandidateSource,
        InvertedIndexCandidateGenerator, MinHashLshCandidateGenerator, align_ordered,
        build_block_features, exact_anchors, partition_anchor_windows,
        select_monotone_anchor_chain,
    },
    layout::{Block, BlockId, BlockRole, Line, LineId},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    normalize::{BlockText, normalize_blocks},
    pdf::ObjectRef,
    pipeline::{PipelineOptions, compare_glyph_documents},
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
    fn diff_of_identical_documents_is_empty(block_words in arb_block_words()) {
        let fixture = fixture_from(1, block_words);

        let comparison = compare_glyph_documents(
            &fixture.document,
            &fixture.document,
            PipelineOptions::default(),
        )
        .expect("identical documents should compare");

        prop_assert!(comparison.changes.is_empty());
        prop_assert!(comparison.formatting_changes.is_empty());
        prop_assert!(comparison.unresolved_regions.is_empty());
    }

    #[test]
    fn alignment_of_equivalent_documents_is_identity(block_words in arb_unique_block_words()) {
        // Distinct id ranges force the normal alignment path instead of the
        // `old == new` identity shortcut inside align_ordered.
        let old_features = features_from(1, block_words.clone());
        let new_features = features_from(10_000, block_words);
        let generator = InvertedIndexCandidateGenerator::new(&new_features)
            .expect("candidate index should build");

        let alignment =
            align_ordered(&old_features, &new_features, &generator, AlignmentOptions::default())
                .expect("equivalent documents should align");

        prop_assert_eq!(alignment.spans.len(), old_features.len());
        for ((span, old_feature), new_feature) in alignment
            .spans
            .iter()
            .zip(&old_features)
            .zip(&new_features)
        {
            prop_assert_eq!(span.kind, AlignmentKind::Match);
            prop_assert_eq!(&span.old, &[old_feature.block]);
            prop_assert_eq!(&span.new, &[new_feature.block]);
            prop_assert_eq!(span.score, 1.0);
        }
    }

    #[test]
    fn candidate_generator_contains_identity_match(block_words in arb_block_words()) {
        let features = features_from(1, block_words);
        let generator =
            InvertedIndexCandidateGenerator::new(&features).expect("candidate index should build");

        for feature in &features {
            let candidates = generator
                .candidates(feature, features.len())
                .expect("candidate query should succeed");
            prop_assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.block == feature.block),
                "identity candidate for block {} is missing",
                feature.block.0
            );
        }
    }

    #[test]
    fn minhash_lsh_candidate_generator_contains_identity_match(block_words in arb_block_words()) {
        let features = features_from(1, block_words);
        let generator =
            MinHashLshCandidateGenerator::new(&features).expect("minhash candidate index should build");

        for feature in &features {
            let candidates = generator
                .candidates(feature, features.len())
                .expect("candidate query should succeed");
            prop_assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.block == feature.block),
                "identity candidate for block {} is missing in minhash lsh",
                feature.block.0
            );
        }
    }

    #[test]
    fn monotone_anchor_chain_is_strictly_increasing(
        old_words in arb_block_words(),
        new_words in arb_block_words(),
    ) {
        let old_features = features_from(1, old_words);
        let new_features = features_from(10_000, new_words);
        let old_indices = old_features
            .iter()
            .enumerate()
            .map(|(index, features)| (features.block, index))
            .collect::<std::collections::HashMap<_, _>>();
        let new_indices = new_features
            .iter()
            .enumerate()
            .map(|(index, features)| (features.block, index))
            .collect::<std::collections::HashMap<_, _>>();

        let anchors = exact_anchors(&old_features, &new_features, 1)
            .expect("exact anchors should extract");
        let chain = select_monotone_anchor_chain(&anchors, &old_features, &new_features)
            .expect("monotone chain should select");

        // Verify strictly increasing indices in main_chain
        for window in chain.main_chain.windows(2) {
            let prev = window[0];
            let next = window[1];
            prop_assert!(
                old_indices[&prev.old] < old_indices[&next.old],
                "old indices must be strictly increasing: {} < {}",
                old_indices[&prev.old],
                old_indices[&next.old]
            );
            prop_assert!(
                new_indices[&prev.new] < new_indices[&next.new],
                "new indices must be strictly increasing: {} < {}",
                new_indices[&prev.new],
                new_indices[&next.new]
            );
        }

        // Verify partition windows are contiguous and cover entire range
        let windows = partition_anchor_windows(&chain.main_chain, &old_features, &new_features)
            .expect("windows should partition");
        prop_assert_eq!(windows.len(), chain.main_chain.len() + 1);

        let mut expected_old = 0;
        let mut expected_new = 0;
        for (i, window) in windows.iter().enumerate() {
            prop_assert_eq!(window.old_range.0, expected_old);
            prop_assert_eq!(window.new_range.0, expected_new);
            prop_assert!(window.old_range.1 >= window.old_range.0);
            prop_assert!(window.new_range.1 >= window.new_range.0);

            if i < chain.main_chain.len() {
                let anchor = chain.main_chain[i];
                prop_assert_eq!(window.old_range.1, old_indices[&anchor.old]);
                prop_assert_eq!(window.new_range.1, new_indices[&anchor.new]);
                expected_old = old_indices[&anchor.old] + 1;
                expected_new = new_indices[&anchor.new] + 1;
            } else {
                prop_assert_eq!(window.old_range.1, old_features.len());
                prop_assert_eq!(window.new_range.1, new_features.len());
            }
        }
    }

    #[test]
    fn short_fallback_visits_only_short_new_blocks(block_words in arb_block_words()) {
        let old_features = features_from(1, block_words.clone());
        let new_features = features_from(10_000, block_words);
        let generator = InvertedIndexCandidateGenerator::new(&new_features)
            .expect("candidate index should build");
        let short_new = new_features
            .iter()
            .filter(|features| features.matching_tokens.len() <= features.ngram_size)
            .map(|features| features.block)
            .collect::<HashSet<_>>();

        for old in &old_features {
            let query_short = old.matching_tokens.len() <= old.ngram_size;
            let estimate = generator
                .estimate_visits(old, usize::MAX)
                .expect("visit estimate should succeed");
            let breakdown = estimate
                .breakdown
                .expect("inverted index reports a breakdown");
            prop_assert_eq!(
                breakdown.short_fallback,
                if query_short { short_new.len() } else { 0 },
                "short fallback must charge exactly the short new blocks"
            );
            prop_assert_eq!(
                breakdown.exact + breakdown.ngram + breakdown.short_fallback,
                estimate.total
            );

            let candidates = generator
                .candidates(old, usize::MAX)
                .expect("candidate query should succeed");
            for candidate in &candidates {
                if short_new.contains(&candidate.block) {
                    // Short new blocks are always fallback candidates for a
                    // short query.
                    if query_short {
                        prop_assert!(
                            candidate
                                .sources
                                .contains(&CandidateSource::ShortBlockFallback),
                            "short block {} lacks the fallback source",
                            candidate.block.0
                        );
                    }
                } else {
                    // Long new blocks can only enter via exact or n-gram
                    // evidence, never through the short fallback.
                    prop_assert!(
                        candidate.sources.iter().any(|source| matches!(
                            source,
                            CandidateSource::Exact | CandidateSource::NGramInvertedIndex
                        )),
                        "long block {} must not enter via the short fallback",
                        candidate.block.0
                    );
                }
            }
        }
    }
}

fn arb_block_words() -> impl Strategy<Value = Vec<Vec<Vec<String>>>> {
    proptest::collection::vec(
        proptest::collection::vec(proptest::collection::vec(any_word(), 1..=3), 1..=3),
        1..=3,
    )
}

/// Contract: ordered alignment treats identical block texts as interchangeable
/// and deliberately reports tied duplicates as unresolved (see
/// `alignment_fixture::leaves_duplicate_tie_regions_unresolved`), so identity
/// alignment is only defined for pairwise-distinct block texts. The per-block
/// index suffix guarantees distinctness without removing repeated words.
fn arb_unique_block_words() -> impl Strategy<Value = Vec<Vec<Vec<String>>>> {
    arb_block_words().prop_map(|mut block_words| {
        for (index, block) in block_words.iter_mut().enumerate() {
            block[0][0].push_str(&index.to_string());
        }
        block_words
    })
}

fn any_word() -> impl Strategy<Value = String> {
    prop::sample::select(&WORDS).prop_map(str::to_owned)
}

fn features_from(
    id_base: u64,
    block_words: Vec<Vec<Vec<String>>>,
) -> Vec<pdfdelta_core::alignment::BlockFeatures> {
    let fixture = fixture_from(id_base, block_words);
    let blocks = normalize_blocks(&fixture.document, &fixture.lines, &fixture.blocks)
        .expect("normalization should succeed");
    build_block_features(&blocks, 3).expect("features should build")
}

/// Re-normalizes canonical text by turning each retained line break back into a line.
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

    for (block_index, line_words) in block_words.into_iter().enumerate() {
        let mut block_lines = Vec::new();
        for words in line_words {
            let mut line_glyphs = Vec::new();
            for word in words {
                glyphs.push(glyph(next_glyph_id, &word));
                line_glyphs.push(GlyphId(next_glyph_id));
                next_glyph_id += 1;
            }
            block_lines.push(LineId(next_line_id));
            lines.push(line(next_line_id, line_glyphs));
            next_line_id += 1;
        }
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

fn line(id: u64, glyphs: Vec<GlyphId>) -> Line {
    Line {
        id: LineId(id),
        page: PageId(0),
        glyphs,
        synthetic_spaces: Vec::new(),
        bbox: Rect {
            min: Vec2 { x: 0.0, y: 0.0 },
            max: Vec2 { x: 100.0, y: 10.0 },
        },
        baseline: Vec2 { x: 0.0, y: 0.0 },
        direction: Vec2 { x: 1.0, y: 0.0 },
    }
}

fn glyph(id: u64, text: &str) -> Glyph {
    Glyph {
        id: GlyphId(id),
        raw_code: text.as_bytes().to_vec(),
        text: DecodedText::Mapped(text.to_owned()),
        page: PageId(0),
        bbox: Rect {
            min: Vec2 {
                x: id as f64 * 10.0,
                y: 0.0,
            },
            max: Vec2 {
                x: id as f64 * 10.0 + 8.0,
                y: 10.0,
            },
        },
        baseline: Vec2 {
            x: id as f64 * 10.0,
            y: 0.0,
        },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size: 10.0,
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
