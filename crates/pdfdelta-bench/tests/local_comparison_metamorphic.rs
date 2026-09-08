use std::sync::Arc;

use lopdf::{Object, Stream, dictionary};
use pdfdelta_bench::{
    mutation::{RenderLine, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};
use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::ChangeKind,
    model::Glyph,
    pdf::{LopdfParser, ParseLimits},
    pipeline::{ComparisonOutcome, PipelineOptions, compare_extraction_outcomes},
    source::{
        ContentStreamGlyphExtractor, ExtractionLimits, ExtractionOutcome, ParserBackedGlyphSource,
    },
};

const OPENING: &str = "A unique opening explains the signal:";
const CLOSING: &str = "and a stable ending closes the passage.";

fn plan(marker: &str) -> RenderPlan {
    RenderPlan::positioned(
        vec![vec![
            RenderLine::new(format!("{OPENING} {marker} {CLOSING}"), 0, 0).expect("target line"),
            RenderLine::new("An independent passage describes copper wiring.", 3, 0).expect("line"),
            RenderLine::new("Another independent passage describes glass optics.", 6, 0)
                .expect("line"),
            RenderLine::new("A final independent passage describes ceramic parts.", 9, 0)
                .expect("line"),
        ]],
        12,
    )
    .expect("nonoverlapping positioned lines")
}

/// Every generated line uses absolute text positioning and its own saved
/// graphics state. Reordering whole streams changes execution order while
/// preserving object/operator provenance and all glyph rendering evidence.
fn pdf(plan: &RenderPlan, interleaved: bool) -> Vec<u8> {
    let bytes = RendererKind::LopdfTj
        .render(plan, RenderLimits::default())
        .expect("rendered PDF");
    let mut document = lopdf::Document::load_mem(&bytes).expect("renderer output parses");
    for page in document.get_pages().into_values() {
        let content = document.get_page_contents(page);
        assert_eq!(content.len(), 1);
        let stream = document
            .get_object(content[0])
            .expect("content object")
            .as_stream()
            .expect("content stream");
        let lines = String::from_utf8(stream.content.clone()).expect("ASCII renderer content");
        let mut streams = lines
            .lines()
            .map(|line| {
                assert!(line.starts_with("BT ") && line.ends_with(" Tj ET"));
                document.add_object(Stream::new(
                    dictionary! {},
                    format!("q\n{line}\nQ\n").into_bytes(),
                ))
            })
            .collect::<Vec<_>>();
        if interleaved && streams.len() >= 4 {
            streams.swap(1, 2);
        }
        document
            .get_object_mut(page)
            .expect("page object")
            .as_dict_mut()
            .expect("page dictionary")
            .set(
                "Contents",
                streams
                    .into_iter()
                    .map(Object::Reference)
                    .collect::<Vec<_>>(),
            );
    }
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("fixture serialization");
    bytes
}

fn extract(bytes: Vec<u8>) -> ExtractionOutcome {
    ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor)
        .extract_outcome(
            Arc::from(bytes),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .expect("fixture extracts completely")
}

#[test]
fn independent_stream_interleaving_preserves_evidence_and_content_edits() {
    let old = extract(pdf(&plan("X"), false));
    let baseline = extract(pdf(&plan("Y"), false));
    let interleaved = extract(pdf(&plan("Y"), true));
    assert!(baseline.issues().is_empty());
    assert!(interleaved.issues().is_empty());
    let mut original = baseline.document().items().to_vec();
    let mut reordered = interleaved.document().items().to_vec();
    let key = |glyph: &Glyph| {
        (
            glyph.provenance.content_stream.object_number,
            glyph.provenance.content_stream.generation,
            glyph.provenance.operator_index,
            glyph.baseline.x.to_bits(),
        )
    };
    original.sort_by_key(key);
    reordered.sort_by_key(key);
    assert!(
        original
            .iter()
            .zip(&reordered)
            .any(|(a, b)| a.render_order != b.render_order)
    );
    for (a, b) in original.iter().zip(&mut reordered) {
        // IDs and render order describe execution, so they are the only
        // fields expected to differ after interleaving complete streams.
        b.id = a.id;
        b.render_order = a.render_order;
    }
    assert_eq!(original, reordered);
    for new in [baseline, interleaved] {
        let outcome = compare_extraction_outcomes(old.clone(), new, PipelineOptions::default())
            .expect("comparison succeeds");
        assert_marker_change(&outcome);
    }
}

fn assert_marker_change(outcome: &ComparisonOutcome) {
    assert_eq!(
        outcome.comparison.changes.len(),
        1,
        "{:#?}",
        outcome.comparison.changes
    );
    let change = &outcome.comparison.changes[0];
    assert_eq!(change.kind, ChangeKind::Replacement);
    assert_eq!(change.occurrences.len(), 1);
    for (blocks, span, expected) in [
        (
            &outcome.old_blocks,
            change.occurrences[0].old_span.as_ref(),
            "X",
        ),
        (
            &outcome.new_blocks,
            change.occurrences[0].new_span.as_ref(),
            "Y",
        ),
    ] {
        let span = span.expect("replacement side");
        let group = span
            .blocks
            .iter()
            .map(|id| {
                blocks
                    .iter()
                    .find(|block| block.block == *id)
                    .expect("source block")
                    .canonical
                    .text
                    .as_str()
            })
            .collect::<Vec<_>>()
            .join(if span.separator == Some(BlockSeparator::Space) {
                " "
            } else {
                ""
            });
        let text = group
            .chars()
            .skip(span.canonical_range.start)
            .take(span.canonical_range.end - span.canonical_range.start)
            .collect::<String>();
        assert_eq!(text, expected);
    }
}

#[test]
fn content_edit_survives_wrap_soft_block_split_and_page_break() {
    let old = extract(pdf(&plan("X"), false));
    for variant in 0..3 {
        let mut pages = vec![vec![
            RenderLine::new(format!("{OPENING} Y"), 0, 0).expect("opening"),
        ]];
        let row = if variant == 1 { 3 } else { 1 };
        if variant == 2 {
            pages.push(Vec::new());
        }
        let last = pages.last_mut().expect("page");
        last.push(RenderLine::new(CLOSING, row, 0).expect("closing"));
        for (offset, text) in [
            "An independent passage describes copper wiring.",
            "Another independent passage describes glass optics.",
            "A final independent passage describes ceramic parts.",
        ]
        .into_iter()
        .enumerate()
        {
            last.push(
                RenderLine::new(text, row + (offset + 1) * 3, 0).expect("independent passage"),
            );
        }
        let new = RenderPlan::positioned(pages, 12).expect("layout variation");
        let outcome = compare_extraction_outcomes(
            old.clone(),
            extract(pdf(&new, false)),
            PipelineOptions::default(),
        )
        .expect("layout comparison");
        assert_eq!(
            outcome.comparison.changes.len(),
            1,
            "layout variant={variant}"
        );
        assert_marker_change(&outcome);
        if variant == 1 {
            assert!(
                outcome.new_blocks.len() > outcome.old_blocks.len(),
                "soft block split is exercised"
            );
        }
    }
}

#[test]
fn ambiguous_neighbor_does_not_invalidate_an_independent_edit() {
    let plan = |marker: &str, repeated: &str| {
        RenderPlan::positioned(
            vec![vec![
                RenderLine::new(format!("{OPENING} {marker} {CLOSING}"), 0, 0).expect("target"),
                RenderLine::new("An independent passage describes copper wiring.", 3, 0)
                    .expect("boundary"),
                RenderLine::new(repeated, 6, 0).expect("ambiguous neighbor"),
                RenderLine::new("A final independent passage describes ceramic parts.", 9, 0)
                    .expect("boundary"),
            ]],
            12,
        )
        .expect("independent passages")
    };
    let old = extract(pdf(
        &plan("X", "Repeated neighboring words: echo echo."),
        false,
    ));
    let new = extract(pdf(
        &plan("Y", "Repeated neighboring words: echo echo echo."),
        true,
    ));
    let outcome =
        compare_extraction_outcomes(old, new, PipelineOptions::default()).expect("comparison");
    assert_marker_change(&outcome);
    assert!(!outcome.comparison.change_candidates.is_empty());
}
