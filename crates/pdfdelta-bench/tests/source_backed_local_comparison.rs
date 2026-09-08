#[path = "../examples/support/glyph_fixture.rs"]
mod glyph_fixture;

use std::{collections::BTreeSet, path::Path};

use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::{ChangeKind, TextSpan},
    model::{Document, Glyph, GlyphId},
    normalize::{BlockText, ComparableToken, TextSource, TextSourceAtom},
    pipeline::{PipelineOptions, compare_extraction_outcomes},
    source::ExtractionOutcome,
};

fn read(side: &str) -> Document<Glyph> {
    glyph_fixture::read(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../fixtures/issue12/fips-{side}.glyphs.json")),
    )
    .expect("checked-in source glyph fixture")
}

fn source_ids(blocks: &[BlockText], span: Option<&TextSpan>) -> BTreeSet<GlyphId> {
    let Some(span) = span else {
        return BTreeSet::new();
    };
    let mut tokens: Vec<(ComparableToken, TextSource)> = Vec::new();
    for block in &span.blocks {
        let block = blocks
            .iter()
            .find(|candidate| candidate.block == *block)
            .expect("reported source block");
        let next = block
            .canonical
            .comparable_tokens_with_sources()
            .expect("mapped source tokens");
        if !tokens.is_empty()
            && span.separator == Some(BlockSeparator::Space)
            && !matches!(tokens.last(), Some((ComparableToken::Scalar(' '), _)))
            && !matches!(next.first(), Some((ComparableToken::Scalar(' '), _)))
        {
            tokens.push((
                ComparableToken::Scalar(' '),
                TextSource { atoms: Vec::new() },
            ));
        }
        tokens.extend(next);
    }
    tokens[span.comparable_range.start..span.comparable_range.end]
        .iter()
        .flat_map(|(_, source)| &source.atoms)
        .filter_map(|atom| match atom {
            TextSourceAtom::Glyph(id) => Some(*id),
            _ => None,
        })
        .collect()
}

#[test]
fn fips_introduction_edits_keep_their_original_source_glyphs() {
    for (reverse, permuted) in [(false, false), (true, false), (false, true)] {
        let mut documents = [read("old"), read("new")];
        if permuted {
            documents = documents.map(|document| {
                let (mut glyphs, lines) = document.into_parts();
                glyphs.reverse();
                Document::with_vector_lines(glyphs, lines)
            });
        }
        if reverse {
            documents.swap(0, 1);
        }
        let [old, new] = documents;
        let outcome = compare_extraction_outcomes(
            ExtractionOutcome::complete(old),
            ExtractionOutcome::complete(new),
            PipelineOptions::default(),
        )
        .expect("source-backed comparison succeeds");
        let mut actual = outcome
            .comparison
            .changes
            .iter()
            .flat_map(|change| {
                change.occurrences.iter().map(|occurrence| {
                    (
                        change.kind,
                        source_ids(&outcome.old_blocks, occurrence.old_span.as_ref()),
                        source_ids(&outcome.new_blocks, occurrence.new_span.as_ref()),
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut expected = if reverse {
            vec![
                (
                    ChangeKind::Replacement,
                    BTreeSet::from([GlyphId(22058)]),
                    BTreeSet::from([GlyphId(25995)]),
                ),
                (
                    ChangeKind::Insertion,
                    BTreeSet::new(),
                    BTreeSet::from([GlyphId(26131)]),
                ),
            ]
        } else {
            vec![
                (
                    ChangeKind::Replacement,
                    BTreeSet::from([GlyphId(25995)]),
                    BTreeSet::from([GlyphId(22058)]),
                ),
                (
                    ChangeKind::Deletion,
                    BTreeSet::from([GlyphId(26131)]),
                    BTreeSet::new(),
                ),
            ]
        };
        // Source review confirms the RSA citation insertion, revision number,
        // and algorithm-list number in addition to the introduction edits.
        expected.extend(if reverse {
            vec![
                (
                    ChangeKind::Replacement,
                    BTreeSet::from([GlyphId(22664)]),
                    BTreeSet::from([GlyphId(26765)]),
                ),
                (
                    ChangeKind::Deletion,
                    BTreeSet::from([
                        GlyphId(22502),
                        GlyphId(22503),
                        GlyphId(22504),
                        GlyphId(22505),
                    ]),
                    BTreeSet::new(),
                ),
                (
                    ChangeKind::Replacement,
                    BTreeSet::from([GlyphId(22517)]),
                    BTreeSet::from([GlyphId(26650)]),
                ),
            ]
        } else {
            vec![
                (
                    ChangeKind::Replacement,
                    BTreeSet::from([GlyphId(26765)]),
                    BTreeSet::from([GlyphId(22664)]),
                ),
                (
                    ChangeKind::Insertion,
                    BTreeSet::new(),
                    BTreeSet::from([
                        GlyphId(22502),
                        GlyphId(22503),
                        GlyphId(22504),
                        GlyphId(22505),
                    ]),
                ),
                (
                    ChangeKind::Replacement,
                    BTreeSet::from([GlyphId(26650)]),
                    BTreeSet::from([GlyphId(22517)]),
                ),
            ]
        });
        let source_order =
            |a: &(ChangeKind, BTreeSet<GlyphId>, BTreeSet<GlyphId>),
             b: &(ChangeKind, BTreeSet<GlyphId>, BTreeSet<GlyphId>)| {
                a.1.cmp(&b.1).then(a.2.cmp(&b.2))
            };
        actual.sort_by(source_order);
        expected.sort_by(source_order);
        assert_eq!(actual, expected, "reverse={reverse}, permuted={permuted}");
        assert!(
            outcome
                .comparison
                .old_coverage
                .ratio
                .is_some_and(|coverage| coverage < 1.0)
        );
        assert!(
            outcome
                .comparison
                .new_coverage
                .ratio
                .is_some_and(|coverage| coverage < 1.0)
        );
    }
}
