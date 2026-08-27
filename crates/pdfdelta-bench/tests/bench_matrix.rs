use std::{process::Command, sync::Arc};

use pdfdelta_bench::{
    BenchError,
    canonical::{CanonicalDocument, Paragraph},
    cases::{BenchmarkCase, built_in_cases},
    evaluator::{evaluate, evaluate_case},
    mutation::{
        COLUMN_GUTTER_FONT_SIZE_RATIO, DEFAULT_MARGIN, DEFAULT_PAGE_HEIGHT, DEFAULT_PAGE_WIDTH,
        ExpectedCanonicalSpan, ExpectedManifest, ExpectedSemanticChange, MAX_LINE_GAP, MAX_MARGIN,
        MIN_LINE_GAP, Mutation, RenderLine, RenderPlan,
    },
    renderers::{RenderLimits, RendererKind},
};
use pdfdelta_core::{
    diff::{ChangeKind, FormattingReason},
    layout::{LineOptions, reconstruct_blocks, reconstruct_lines},
    model::PageId,
    normalize::normalize_blocks,
    pdf::{LopdfParser, ParseLimits, PdfObject, PdfParser},
    pipeline::{PipelineOptions, compare_extraction_outcomes},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};

fn sample_document() -> CanonicalDocument {
    CanonicalDocument::new(vec![
        Paragraph::new("context", "Context paragraph stays unchanged.").expect("valid paragraph"),
        Paragraph::new("target", "Target paragraph contains several words.")
            .expect("valid paragraph"),
    ])
    .expect("valid document")
}

fn column_document(left_text: impl Into<String>) -> CanonicalDocument {
    CanonicalDocument::new(vec![
        Paragraph::new("left-a", left_text).expect("valid left paragraph"),
        Paragraph::new("left-b", "Left remains stable").expect("valid left paragraph"),
        Paragraph::new("right-a", "Right remains stable").expect("valid right paragraph"),
        Paragraph::new("right-b", "Right also remains stable").expect("valid right paragraph"),
    ])
    .expect("valid column document")
}

fn assert_invalid<T>(result: Result<T, BenchError>) {
    assert!(matches!(result, Err(BenchError::InvalidInput(_))));
}

fn case_named(name: &str) -> BenchmarkCase {
    built_in_cases()
        .expect("built-in cases are valid")
        .into_iter()
        .find(|case| case.name() == name)
        .expect("named built-in case exists")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn normalized_canonical_blocks(plan: &RenderPlan) -> Vec<String> {
    normalized_canonical_blocks_with_renderer(plan, RendererKind::LopdfTj)
}

fn normalized_canonical_blocks_with_renderer(
    plan: &RenderPlan,
    renderer: RendererKind,
) -> Vec<String> {
    let pdf = renderer
        .render(plan, RenderLimits::default())
        .expect("renderer succeeds");
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract_outcome(
            Arc::from(pdf),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .expect("generated PDF extracts")
        .into_complete()
        .expect("extraction is complete");
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line).expect("lines reconstruct");
    let blocks = reconstruct_blocks(&document, &lines, options.block).expect("blocks reconstruct");
    normalize_blocks(&document, &lines, &blocks)
        .expect("blocks normalize")
        .into_iter()
        .map(|block| block.canonical.text)
        .collect()
}

fn compare_rendered_pdfs(
    old_pdf: Vec<u8>,
    new_pdf: Vec<u8>,
) -> pdfdelta_core::pipeline::ComparisonOutcome {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let extract = |pdf| {
        source
            .extract_outcome(
                Arc::from(pdf),
                ParseLimits::default(),
                ExtractionLimits::default(),
            )
            .expect("generated PDF extracts")
    };
    compare_extraction_outcomes(
        extract(old_pdf),
        extract(new_pdf),
        PipelineOptions::default(),
    )
    .expect("comparison completes")
}

fn media_box_size(bytes: Vec<u8>) -> (i64, i64) {
    let pdf = LopdfParser
        .parse(Arc::from(bytes), ParseLimits::default())
        .expect("generated PDF parses");
    let page = pdf.pages().expect("pages resolve")[0];
    let dictionary = pdf.page_dict(page).expect("page dictionary resolves");
    match dictionary.get(b"MediaBox".as_slice()) {
        Some(PdfObject::Array(values)) => match values.as_slice() {
            [
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(width),
                PdfObject::Integer(height),
            ] => (*width, *height),
            values => panic!("unexpected MediaBox values: {values:?}"),
        },
        value => panic!("unexpected MediaBox: {value:?}"),
    }
}

#[test]
fn canonical_documents_reject_invalid_input() {
    assert_invalid(Paragraph::new("", "Valid text."));
    assert_invalid(Paragraph::new("bad id", "Valid text."));
    assert_invalid(Paragraph::new("valid", ""));
    assert_invalid(Paragraph::new("valid", " leading space"));
    assert_invalid(Paragraph::new("valid", "Non-ASCII: café"));

    let duplicate = Paragraph::new("duplicate", "First paragraph.").expect("valid paragraph");
    assert_invalid(CanonicalDocument::new(vec![
        duplicate.clone(),
        Paragraph::new("duplicate", "Second paragraph.").expect("valid paragraph"),
    ]));
}

#[test]
fn mutations_reject_invalid_input_without_panicking() {
    let document = sample_document();

    assert_invalid(Mutation::ColumnChange.apply(&document, 12));

    assert_invalid(
        Mutation::LineWrap {
            paragraph_id: "missing".to_owned(),
            after_word: 1,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::LineWrapTwice {
            paragraph_id: "target".to_owned(),
            after_words: [2, 2],
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::LineWrapTwice {
            paragraph_id: "target".to_owned(),
            after_words: [0, 2],
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::LineWrapTwice {
            paragraph_id: "target".to_owned(),
            after_words: [2, 99],
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::LineWrap {
            paragraph_id: "target".to_owned(),
            after_word: 0,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::LineWrap {
            paragraph_id: "target".to_owned(),
            after_word: 99,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::PageBreak {
            before_paragraph: 0,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::PageBreak {
            before_paragraph: document.paragraphs().len(),
        }
        .apply(&document, 12),
    );
    assert_invalid(Mutation::LineHeightChange { new_line_gap: 12 }.apply(&document, 12));
    assert_invalid(
        Mutation::LineHeightChange {
            new_line_gap: MIN_LINE_GAP - 1,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::LineHeightChange {
            new_line_gap: MAX_LINE_GAP + 1,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::MarginChange {
            new_margin: DEFAULT_MARGIN,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::MarginChange {
            new_margin: MAX_MARGIN + 1,
        }
        .apply(&document, 12),
    );
    assert_invalid(Mutation::FontSizeChange { new_font_size: 10 }.apply(&document, 12));
    assert_invalid(Mutation::FontSizeChange { new_font_size: 0 }.apply(&document, 12));
    assert_invalid(
        Mutation::PageSizeChange {
            new_page_width: DEFAULT_PAGE_WIDTH,
            new_page_height: DEFAULT_PAGE_HEIGHT,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::PageSizeChange {
            new_page_width: 0,
            new_page_height: DEFAULT_PAGE_HEIGHT,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::PageSizeChange {
            new_page_width: DEFAULT_PAGE_WIDTH,
            new_page_height: 0,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::PageSizeChange {
            new_page_width: 20,
            new_page_height: 842,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::PageSizeChange {
            new_page_width: 595,
            new_page_height: 400,
        }
        .apply(&document, 12),
    );
    let lines = vec![vec!["Alpha beta gamma".to_owned()]];
    assert_invalid(RenderPlan::with_page_size(lines.clone(), 12, 0, 792));
    assert_invalid(RenderPlan::with_page_size(lines.clone(), 12, 612, 0));
    assert_invalid(RenderPlan::with_page_size(lines.clone(), 12, 36, 792));
    assert_invalid(RenderPlan::with_page_size(lines, 12, 612, 740));
    RenderPlan::new(vec![vec!["x".repeat(108)]], 12).expect("right-margin boundary fits");
    assert_invalid(RenderPlan::new(vec![vec!["x".repeat(109)]], 12));
    assert_invalid(RenderPlan::new(vec![vec!["x".repeat(512)]], 12));
    assert_invalid(RenderPlan::positioned(
        vec![vec![
            RenderLine::new("outside", 0, DEFAULT_PAGE_WIDTH).expect("valid text"),
        ]],
        12,
    ));
    let vertically_outside = RenderPlan::positioned(
        vec![vec![
            RenderLine::new("outside", 100, 0).expect("valid text"),
        ]],
        12,
    )
    .expect("position is validated at render time");
    for renderer in RendererKind::all() {
        assert_invalid(renderer.render(&vertically_outside, RenderLimits::default()));
    }
    Mutation::ColumnChange
        .apply(&column_document("x".repeat(44)), 12)
        .expect("left column leaves the required gutter");
    assert_invalid(Mutation::ColumnChange.apply(&column_document("x".repeat(45)), 12));
    assert_invalid(
        Mutation::TextReplace {
            paragraph_id: "missing".to_owned(),
            new_text: "Replacement text.".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextReplace {
            paragraph_id: "target".to_owned(),
            new_text: "".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextReplace {
            paragraph_id: "target".to_owned(),
            new_text: "Target paragraph contains several words.".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::ParagraphInsert {
            index: document.paragraphs().len() + 1,
            paragraph: Paragraph::new("inserted", "Inserted paragraph.").expect("valid paragraph"),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::ParagraphInsert {
            index: 1,
            paragraph: Paragraph::new("target", "Duplicate identifier.").expect("valid paragraph"),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::ParagraphDelete {
            paragraph_id: "missing".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::ParagraphMove {
            paragraph_id: "missing".to_owned(),
            to_index: 0,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::ParagraphMove {
            paragraph_id: "target".to_owned(),
            to_index: document.paragraphs().len(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::ParagraphMove {
            paragraph_id: "target".to_owned(),
            to_index: 1,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextInsert {
            paragraph_id: "missing".to_owned(),
            at: 0,
            text: "x".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextInsert {
            paragraph_id: "target".to_owned(),
            at: "Target paragraph contains several words.".chars().count() + 1,
            text: "x".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextInsert {
            paragraph_id: "target".to_owned(),
            at: 0,
            text: "".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextDelete {
            paragraph_id: "target".to_owned(),
            start: 3,
            end: 3,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::TextDelete {
            paragraph_id: "target".to_owned(),
            start: 2,
            end: "Target paragraph contains several words.".chars().count() + 1,
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::NumberReplace {
            paragraph_id: "context".to_owned(),
            new_number: "7".to_owned(),
        }
        .apply(&document, 12),
    );
    assert_invalid(
        Mutation::NumberReplace {
            paragraph_id: "missing".to_owned(),
            new_number: "7".to_owned(),
        }
        .apply(&document, 12),
    );

    let one_paragraph = CanonicalDocument::new(vec![
        Paragraph::new("only", "Only paragraph remains.").expect("valid paragraph"),
    ])
    .expect("valid document");
    assert_invalid(
        Mutation::ParagraphDelete {
            paragraph_id: "only".to_owned(),
        }
        .apply(&one_paragraph, 12),
    );
}

#[test]
fn both_renderers_produce_parseable_extractable_multipage_pdfs() {
    let case = case_named("page-break-only");

    for renderer in RendererKind::all() {
        let pdf = renderer
            .render(case.plan().new_plan(), RenderLimits::default())
            .expect("renderer succeeds");
        assert!(
            find_subslice(&pdf, b"/FontBBox").is_some(),
            "{} omitted FontBBox",
            renderer.name()
        );
        let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
        let outcome = source
            .extract_outcome(
                Arc::from(pdf),
                ParseLimits::default(),
                ExtractionLimits::default(),
            )
            .expect("generated PDF parses and extracts");

        assert!(outcome.is_complete());
        assert!(outcome.issues().is_empty());
        assert!(!outcome.document().items().is_empty());
        assert!(
            outcome
                .document()
                .items()
                .iter()
                .any(|glyph| glyph.page == PageId(0))
        );
        assert!(
            outcome
                .document()
                .items()
                .iter()
                .any(|glyph| glyph.page == PageId(1))
        );
    }
}

#[test]
fn double_line_wrap_exercises_one_to_three_block_alignment() {
    let case = case_named("double-line-wrap-only");

    assert_eq!(normalized_canonical_blocks(case.plan().old()).len(), 3);
    assert_eq!(normalized_canonical_blocks(case.plan().new_plan()).len(), 5);
}

#[test]
fn line_wrap_exercises_one_to_two_block_alignment() {
    let case = case_named("line-wrap-only");

    assert_eq!(normalized_canonical_blocks(case.plan().old()).len(), 3);
    assert_eq!(normalized_canonical_blocks(case.plan().new_plan()).len(), 4);
}

#[test]
fn column_change_is_complete_and_preserves_column_major_order() {
    let case = case_named("column-change-only");
    let expected = [
        "Left first paragraph remains stable",
        "Left second paragraph remains stable",
        "Right first paragraph remains stable",
        "Right second paragraph remains stable",
    ];

    for renderer in RendererKind::all() {
        assert_eq!(
            normalized_canonical_blocks_with_renderer(case.plan().new_plan(), renderer),
            expected
        );
        let record = evaluate_case(&case, renderer).expect("column evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert!(record.extraction_complete);
        assert!(record.comparison_complete);
        assert_eq!(record.actual_changes, 0);
        assert_eq!(record.old_coverage, Some(1.0));
        assert_eq!(record.new_coverage, Some(1.0));
    }
}

#[test]
fn column_change_boundary_preserves_complete_zero_diff_comparison() {
    assert!(
        f64::from(COLUMN_GUTTER_FONT_SIZE_RATIO)
            > LineOptions::default().max_inline_gap_font_size_ratio
    );
    let boundary_text = "Maximum width left line remains unchanged 12";
    assert_eq!(boundary_text.len(), 44);
    let document = column_document(boundary_text);
    let plan = Mutation::ColumnChange
        .apply(&document, 30)
        .expect("maximum left line leaves the required gutter");
    let expected = [
        boundary_text,
        "Left remains stable",
        "Right remains stable",
        "Right also remains stable",
    ];

    for renderer in RendererKind::all() {
        let old_pdf = renderer
            .render(plan.old(), RenderLimits::default())
            .expect("old PDF renders");
        let new_pdf = renderer
            .render(plan.new_plan(), RenderLimits::default())
            .expect("new PDF renders");
        let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
        let old = source
            .extract_outcome(
                Arc::from(old_pdf),
                ParseLimits::default(),
                ExtractionLimits::default(),
            )
            .expect("old PDF extracts");
        let new = source
            .extract_outcome(
                Arc::from(new_pdf),
                ParseLimits::default(),
                ExtractionLimits::default(),
            )
            .expect("new PDF extracts");
        assert!(old.is_complete());
        assert!(new.is_complete());

        let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())
            .expect("comparison completes");
        assert!(outcome.extraction.old_complete);
        assert!(outcome.extraction.new_complete);
        assert!(outcome.extraction.issues.is_empty());
        assert!(outcome.comparison.changes.is_empty(), "{outcome:#?}");
        assert!(outcome.comparison.unresolved_regions.is_empty());
        assert_eq!(outcome.comparison.old_coverage.ratio, Some(1.0));
        assert_eq!(outcome.comparison.new_coverage.ratio, Some(1.0));
        assert_eq!(
            outcome
                .new_blocks
                .iter()
                .map(|block| block.canonical.text.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn right_column_replacement_stays_out_of_the_left_column() {
    let x = (DEFAULT_PAGE_WIDTH - DEFAULT_MARGIN * 2) / 2;
    let positioned = |release: &str| {
        RenderPlan::positioned(
            vec![vec![
                RenderLine::new("Left alpha remains stable", 0, 0).expect("valid line"),
                RenderLine::new("Left beta remains stable", 1, 0).expect("valid line"),
                RenderLine::new(release, 0, x).expect("valid line"),
                RenderLine::new("Right closing remains stable", 1, x).expect("valid line"),
            ]],
            30,
        )
        .expect("valid positioned plan")
    };
    let old_plan = positioned("Release 10 remains available");
    let new_plan = positioned("Release 20 remains available");

    for renderer in RendererKind::all() {
        let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
        let old = source
            .extract_outcome(
                Arc::from(
                    renderer
                        .render(&old_plan, RenderLimits::default())
                        .expect("old PDF renders"),
                ),
                ParseLimits::default(),
                ExtractionLimits::default(),
            )
            .expect("old PDF extracts");
        let new = source
            .extract_outcome(
                Arc::from(
                    renderer
                        .render(&new_plan, RenderLimits::default())
                        .expect("new PDF renders"),
                ),
                ParseLimits::default(),
                ExtractionLimits::default(),
            )
            .expect("new PDF extracts");
        assert!(old.is_complete());
        assert!(new.is_complete());

        let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())
            .expect("comparison completes");
        assert_eq!(outcome.comparison.changes.len(), 1, "{outcome:#?}");
        assert_eq!(outcome.comparison.changes[0].kind, ChangeKind::Replacement);
        assert!(outcome.comparison.unresolved_regions.is_empty());
        assert_eq!(outcome.comparison.old_coverage.ratio, Some(1.0));
        assert_eq!(outcome.comparison.new_coverage.ratio, Some(1.0));
        let change = &outcome.comparison.changes[0];
        let old_span = change.old_span.as_ref().expect("replacement has old span");
        let new_span = change.new_span.as_ref().expect("replacement has new span");
        let selected_text =
            |span: &pdfdelta_core::diff::TextSpan,
             blocks: &[pdfdelta_core::normalize::BlockText]| {
                assert_eq!(span.blocks.len(), 1, "change must stay in one column block");
                let block = blocks
                    .iter()
                    .find(|block| block.block == span.blocks[0])
                    .expect("span block exists");
                assert!(block.canonical.text.contains("Release"));
                assert!(!block.canonical.text.contains("Left"));
                block
                    .canonical
                    .text
                    .chars()
                    .skip(span.canonical_range.start)
                    .take(span.canonical_range.end - span.canonical_range.start)
                    .collect::<String>()
            };
        assert_eq!(selected_text(old_span, &outcome.old_blocks), "1");
        assert_eq!(selected_text(new_span, &outcome.new_blocks), "2");
    }
}

#[test]
fn line_height_change_alters_layout_without_content_diff() {
    let case = case_named("line-height-change-only");
    assert_ne!(
        case.plan().old().line_gap(),
        case.plan().new_plan().line_gap()
    );
    assert_eq!(
        normalized_canonical_blocks(case.plan().old()),
        normalized_canonical_blocks(case.plan().new_plan()),
    );

    for renderer in RendererKind::all() {
        let old_pdf = renderer
            .render(case.plan().old(), RenderLimits::default())
            .expect("renderer succeeds");
        let new_pdf = renderer
            .render(case.plan().new_plan(), RenderLimits::default())
            .expect("renderer succeeds");
        assert_ne!(
            old_pdf,
            new_pdf,
            "{} must change rendered bytes",
            renderer.name()
        );
        let outcome = compare_rendered_pdfs(old_pdf, new_pdf);
        assert!(outcome.comparison.changes.is_empty(), "{outcome:#?}");
        assert!(
            outcome
                .comparison
                .formatting_changes
                .iter()
                .any(|change| change.reasons.contains(&FormattingReason::Position)),
            "{outcome:#?}"
        );
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert!(record.extraction_complete);
        assert!(record.comparison_complete);
        assert!(record.actual_kinds.is_empty());
    }
}

#[test]
fn margin_change_alters_layout_without_content_diff() {
    let case = case_named("margin-change-only");
    assert_ne!(case.plan().old().margin(), case.plan().new_plan().margin());
    assert_eq!(
        normalized_canonical_blocks(case.plan().old()),
        normalized_canonical_blocks(case.plan().new_plan()),
    );

    for renderer in RendererKind::all() {
        let old_pdf = renderer
            .render(case.plan().old(), RenderLimits::default())
            .expect("renderer succeeds");
        let new_pdf = renderer
            .render(case.plan().new_plan(), RenderLimits::default())
            .expect("renderer succeeds");
        assert_ne!(
            old_pdf,
            new_pdf,
            "{} must change rendered bytes",
            renderer.name()
        );
        let outcome = compare_rendered_pdfs(old_pdf, new_pdf);
        assert!(outcome.comparison.changes.is_empty(), "{outcome:#?}");
        assert!(
            outcome
                .comparison
                .formatting_changes
                .iter()
                .any(|change| change.reasons.contains(&FormattingReason::Position)),
            "{outcome:#?}"
        );
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert!(record.extraction_complete);
        assert!(record.comparison_complete);
        assert!(record.actual_kinds.is_empty());
    }
}

#[test]
fn font_size_change_alters_layout_without_content_diff() {
    let case = case_named("font-size-change-only");
    assert_ne!(
        case.plan().old().font_size(),
        case.plan().new_plan().font_size()
    );
    assert_eq!(
        normalized_canonical_blocks(case.plan().old()),
        normalized_canonical_blocks(case.plan().new_plan()),
    );

    for renderer in RendererKind::all() {
        let old_pdf = renderer
            .render(case.plan().old(), RenderLimits::default())
            .expect("renderer succeeds");
        let new_pdf = renderer
            .render(case.plan().new_plan(), RenderLimits::default())
            .expect("renderer succeeds");
        assert_ne!(
            old_pdf,
            new_pdf,
            "{} must change rendered bytes",
            renderer.name()
        );
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert!(record.extraction_complete);
        assert!(record.comparison_complete);
        assert!(record.actual_kinds.is_empty());
    }
}

#[test]
fn page_size_change_alters_layout_without_content_diff() {
    let case = case_named("page-size-change-only");
    assert_ne!(
        case.plan().old().page_width(),
        case.plan().new_plan().page_width()
    );
    assert_ne!(
        case.plan().old().page_height(),
        case.plan().new_plan().page_height()
    );
    assert_eq!(
        normalized_canonical_blocks(case.plan().old()),
        normalized_canonical_blocks(case.plan().new_plan()),
    );

    for renderer in RendererKind::all() {
        let old_pdf = renderer
            .render(case.plan().old(), RenderLimits::default())
            .expect("renderer succeeds");
        let new_pdf = renderer
            .render(case.plan().new_plan(), RenderLimits::default())
            .expect("renderer succeeds");
        assert_ne!(
            old_pdf,
            new_pdf,
            "{} must change rendered bytes",
            renderer.name()
        );
        assert_eq!(
            media_box_size(old_pdf.clone()),
            (
                i64::from(DEFAULT_PAGE_WIDTH),
                i64::from(DEFAULT_PAGE_HEIGHT)
            )
        );
        assert_eq!(media_box_size(new_pdf.clone()), (595, 842));
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert!(record.extraction_complete);
        assert!(record.comparison_complete);
        assert!(record.actual_kinds.is_empty());
    }
}

#[test]
fn classic_xref_renderer_uses_positioned_words_without_space_glyphs() {
    let plan =
        RenderPlan::new(vec![vec!["Alpha beta gamma".to_owned()]], 12).expect("valid render plan");
    let pdf = RendererKind::ClassicXrefTj
        .render(&plan, RenderLimits::default())
        .expect("renderer succeeds");
    let stream_start = find_subslice(&pdf, b"stream\n").expect("content stream exists") + 7;
    let stream_end = stream_start
        + find_subslice(&pdf[stream_start..], b"endstream").expect("content stream ends");
    let stream = &pdf[stream_start..stream_end];

    assert!(
        stream
            .windows(b"] TJ".len())
            .any(|window| window == b"] TJ")
    );
    assert!(!stream.windows(b" Tj".len()).any(|window| window == b" Tj"));

    let mut in_literal = false;
    let mut escaped = false;
    let mut saw_literal = false;
    for byte in stream {
        if in_literal {
            if escaped {
                escaped = false;
            } else {
                match byte {
                    b'\\' => escaped = true,
                    b')' => in_literal = false,
                    b' ' => panic!("space byte found inside a PDF literal string"),
                    _ => {}
                }
            }
        } else if *byte == b'(' {
            in_literal = true;
            saw_literal = true;
        }
    }

    assert!(saw_literal);
    assert!(!in_literal);

    let marker = b"startxref\n";
    let offset_start = find_subslice(&pdf, marker).expect("startxref exists") + marker.len();
    let offset_end = offset_start
        + pdf[offset_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("startxref offset ends");
    let offset = std::str::from_utf8(&pdf[offset_start..offset_end])
        .expect("ASCII offset")
        .parse::<usize>()
        .expect("numeric offset");
    assert!(pdf[offset..].starts_with(b"xref\n"));
}

#[test]
fn semantic_mutations_record_independent_expected_canonical_spans() {
    let replacement = case_named("text-replacement");
    let replacement = &replacement.plan().expectation().changes()[0];
    let release_start = "Opening paragraph establishes context".chars().count() + 1;
    let replacement_old =
        ExpectedCanonicalSpan::new(release_start + 8, release_start + 9).expect("valid span");
    let replacement_new = replacement_old;
    assert_eq!(replacement.kind(), ChangeKind::Replacement);
    assert_eq!(replacement.old_spans(), [replacement_old]);
    assert_eq!(replacement.new_spans(), [replacement_new]);

    let insertion = case_named("paragraph-insertion");
    let insertion = &insertion.plan().expectation().changes()[0];
    let inserted_start = "Opening paragraph remains stable".chars().count() + 1;
    let inserted =
        ExpectedCanonicalSpan::new(inserted_start, inserted_start + 40).expect("valid span");
    assert_eq!(insertion.kind(), ChangeKind::Insertion);
    assert!(insertion.old_spans().is_empty());
    assert_eq!(
        insertion.new_spans(),
        [
            inserted,
            ExpectedCanonicalSpan::new(inserted_start - 1, inserted_start + 40)
                .expect("valid leading-separator span"),
            ExpectedCanonicalSpan::new(inserted_start, inserted_start + 41)
                .expect("valid trailing-separator span"),
        ]
    );

    let deletion = case_named("paragraph-deletion");
    let deletion = &deletion.plan().expectation().changes()[0];
    let removed_start = "Opening paragraph remains stable".chars().count() + 1;
    let removed =
        ExpectedCanonicalSpan::new(removed_start, removed_start + 39).expect("valid span");
    assert_eq!(deletion.kind(), ChangeKind::Deletion);
    assert_eq!(
        deletion.old_spans(),
        [
            removed,
            ExpectedCanonicalSpan::new(removed_start - 1, removed_start + 39)
                .expect("valid leading-separator span"),
            ExpectedCanonicalSpan::new(removed_start, removed_start + 40)
                .expect("valid trailing-separator span"),
        ]
    );
    assert!(deletion.new_spans().is_empty());
}

#[test]
fn evaluator_rejects_same_kind_change_at_the_wrong_span() {
    let case = case_named("text-replacement");
    let expected = ExpectedManifest::one(
        ExpectedSemanticChange::new(
            ChangeKind::Replacement,
            vec![ExpectedCanonicalSpan::new(0, 2).expect("valid span")],
            vec![ExpectedCanonicalSpan::new(0, 2).expect("valid span")],
        )
        .expect("valid expected change"),
    );
    let record = evaluate(
        case.name(),
        case.plan().old(),
        case.plan().new_plan(),
        &expected,
        RendererKind::LopdfTj,
    )
    .expect("evaluation completes");

    assert!(!record.passed);
    assert_eq!(record.actual_changes, 1);
    assert_eq!(record.actual_kinds, [ChangeKind::Replacement]);
    assert!(record.detail.contains("span IoU"));
}

#[test]
fn evaluator_distinguishes_duplicate_paragraph_occurrences() {
    let document = CanonicalDocument::new(vec![
        Paragraph::new("opening", "Opening anchor remains unique").expect("valid paragraph"),
        Paragraph::new("first", "Release 10 remains available").expect("valid paragraph"),
        Paragraph::new("middle", "Middle anchor separates repeated targets")
            .expect("valid paragraph"),
        Paragraph::new("second", "Release 10 remains available").expect("valid paragraph"),
        Paragraph::new("closing", "Closing anchor remains unique").expect("valid paragraph"),
    ])
    .expect("valid document");
    let first = BenchmarkCase::new(
        "first-occurrence-expectation",
        document.clone(),
        Mutation::TextReplace {
            paragraph_id: "first".to_owned(),
            new_text: "Release 20 remains available".to_owned(),
        },
        30,
    )
    .expect("valid benchmark case");
    let second = BenchmarkCase::new(
        "second-occurrence-replacement",
        document,
        Mutation::TextReplace {
            paragraph_id: "second".to_owned(),
            new_text: "Release 20 remains available".to_owned(),
        },
        30,
    )
    .expect("valid benchmark case");

    let first_span = first.plan().expectation().changes()[0].old_spans()[0];
    let second_span = second.plan().expectation().changes()[0].old_spans()[0];
    assert_eq!(
        first_span.end() - first_span.start(),
        second_span.end() - second_span.start()
    );
    assert_ne!(first_span.start(), second_span.start());

    let correct = evaluate_case(&second, RendererKind::LopdfTj).expect("evaluation completes");
    assert!(correct.passed, "{}", correct.detail);
    assert!(correct.extraction_complete);
    assert!(correct.comparison_complete);
    assert_eq!(correct.actual_kinds, [ChangeKind::Replacement]);

    let wrong_occurrence = evaluate(
        second.name(),
        second.plan().old(),
        second.plan().new_plan(),
        first.plan().expectation(),
        RendererKind::LopdfTj,
    )
    .expect("evaluation completes");

    assert_eq!(wrong_occurrence.actual_kinds, [ChangeKind::Replacement]);
    assert!(!wrong_occurrence.passed);
}

#[test]
fn global_expectation_survives_merged_normalized_blocks() {
    let case = BenchmarkCase::new(
        "merged-block-replacement",
        CanonicalDocument::new(vec![
            Paragraph::new("opening", "Opening context remains stable").expect("valid paragraph"),
            Paragraph::new("target", "Release cat remains available").expect("valid paragraph"),
            Paragraph::new("closing", "Closing context remains stable").expect("valid paragraph"),
        ])
        .expect("valid document"),
        Mutation::TextReplace {
            paragraph_id: "target".to_owned(),
            new_text: "Release cut remains available".to_owned(),
        },
        12,
    )
    .expect("valid benchmark case");

    assert_eq!(
        normalized_canonical_blocks(case.plan().old()),
        [
            "Opening context remains stable Release cat remains available Closing context remains stable"
        ]
    );

    for renderer in RendererKind::all() {
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
    }
}

#[test]
fn merged_block_paragraph_insertion_matches_a_separator_variant() {
    let case = BenchmarkCase::new(
        "merged-block-insertion",
        CanonicalDocument::new(vec![
            Paragraph::new(
                "opening",
                "Opening context remains stable and uniquely anchored",
            )
            .expect("valid paragraph"),
            Paragraph::new(
                "closing",
                "Closing context remains stable and uniquely anchored",
            )
            .expect("valid paragraph"),
        ])
        .expect("valid document"),
        Mutation::ParagraphInsert {
            index: 1,
            paragraph: Paragraph::new("inserted", "Inserted paragraph adds focused context")
                .expect("valid paragraph"),
        },
        12,
    )
    .expect("valid benchmark case");

    assert_eq!(normalized_canonical_blocks(case.plan().old()).len(), 1);
    assert_eq!(normalized_canonical_blocks(case.plan().new_plan()).len(), 1);
    for renderer in RendererKind::all() {
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert_eq!(record.actual_kinds, [ChangeKind::Insertion]);
    }
}

#[test]
fn merged_block_paragraph_deletion_matches_a_separator_variant() {
    let case = BenchmarkCase::new(
        "merged-block-deletion",
        CanonicalDocument::new(vec![
            Paragraph::new(
                "opening",
                "Opening context remains stable and uniquely anchored",
            )
            .expect("valid paragraph"),
            Paragraph::new("removed", "Removed paragraph adds focused context")
                .expect("valid paragraph"),
            Paragraph::new(
                "closing",
                "Closing context remains stable and uniquely anchored",
            )
            .expect("valid paragraph"),
        ])
        .expect("valid document"),
        Mutation::ParagraphDelete {
            paragraph_id: "removed".to_owned(),
        },
        12,
    )
    .expect("valid benchmark case");

    assert_eq!(normalized_canonical_blocks(case.plan().old()).len(), 1);
    assert_eq!(normalized_canonical_blocks(case.plan().new_plan()).len(), 1);
    for renderer in RendererKind::all() {
        let record = evaluate_case(&case, renderer).expect("evaluation completes");
        assert!(record.passed, "{}", record.detail);
        assert_eq!(record.actual_kinds, [ChangeKind::Deletion]);
    }
}

#[test]
fn bench_errors_preserve_core_error_taxonomy() {
    let error = BenchError::Core {
        stage: "comparison",
        source: pdfdelta_core::Error::LimitExceeded {
            resource: "test tokens",
            limit: 7,
        },
    };

    assert!(matches!(
        &error,
        BenchError::Core {
            source: pdfdelta_core::Error::LimitExceeded {
                resource: "test tokens",
                limit: 7
            },
            ..
        }
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn built_in_matrix_passes_all_thirty_cells() {
    let cases = built_in_cases().expect("built-in cases are valid");
    assert_eq!(cases.len(), 15);

    let mut count = 0;
    for case in &cases {
        for renderer in RendererKind::all() {
            let record = evaluate_case(case, renderer).expect("evaluation completes");
            assert!(record.passed, "{}", record.detail);
            assert!(record.extraction_complete);
            assert!(record.comparison_complete);
            assert_eq!(record.old_coverage, Some(1.0));
            assert_eq!(record.new_coverage, Some(1.0));
            count += 1;
        }
    }

    assert_eq!(count, 30);
}

#[test]
fn verify_command_prints_a_passing_thirty_cell_matrix() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("verify")
        .output()
        .expect("pdfbench runs");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("PASS "))
            .count(),
        30
    );
    assert_eq!(stdout.lines().last(), Some("30/30 passed"));
}

#[test]
fn help_lists_benchmark_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("--help")
        .output()
        .expect("pdfbench runs");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("verify"));
    assert!(stdout.contains("render"));
    assert!(stdout.contains("candidates"));
    assert!(stdout.contains("candidate-profile"));
    assert!(stdout.contains("extraction-conformance"));
}

#[test]
fn candidates_command_prints_records_and_summary() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("candidates")
        .arg("--top-k")
        .arg("5,10")
        .output()
        .expect("pdfbench runs");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("OK case="))
            .count(),
        30
    );
    assert_eq!(
        stdout.lines().last(),
        Some("30/30 candidate evaluations OK")
    );
}

#[test]
fn candidates_command_rejects_invalid_top_k() {
    for top_k in ["0", "", "5,,10", "five", "5,5"] {
        let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
            .arg("candidates")
            .arg("--top-k")
            .arg(top_k)
            .output()
            .expect("pdfbench runs");
        assert_eq!(
            output.status.code(),
            Some(2),
            "--top-k {top_k:?} must exit 2"
        );
    }
}

#[test]
fn candidates_command_writes_a_create_new_json_artifact() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pdfbench-candidates-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("candidates")
        .arg("--top-k")
        .arg("5,10")
        .arg("--json-output")
        .arg(&path)
        .output()
        .expect("pdfbench runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = std::fs::read_to_string(&path).expect("json artifact exists");
    let records: serde_json::Value = serde_json::from_str(&json).expect("json artifact parses");
    assert_eq!(records.as_array().expect("artifact is an array").len(), 30);
    assert_eq!(records[0]["renderer"], "lopdf-tj");
    assert_eq!(records[0]["top_k"], serde_json::json!([5, 10]));

    let second = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("candidates")
        .arg("--json-output")
        .arg(&path)
        .output()
        .expect("pdfbench runs");
    assert_eq!(
        second.status.code(),
        Some(2),
        "create_new must refuse overwrite"
    );
    std::fs::remove_file(&path).expect("temp json artifact removed");
}

#[cfg(target_os = "linux")]
#[test]
fn candidate_profile_runs_generators_in_isolated_workers_and_writes_json() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pdfbench-candidate-profile-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("candidate-profile")
        .arg("--blocks")
        .arg("64")
        .arg("--top-k")
        .arg("1,5")
        .arg("--json-output")
        .arg(&path)
        .output()
        .expect("pdfbench runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("PROFILE generator="))
            .count(),
        3
    );
    assert_eq!(
        stdout.lines().last(),
        Some("3/3 candidate profiles OK; default=inverted-index (diagnostic observations only)")
    );

    let json = std::fs::read_to_string(&path).expect("json artifact exists");
    let records: serde_json::Value = serde_json::from_str(&json).expect("json artifact parses");
    let records = records.as_array().expect("artifact is an array");
    assert_eq!(records.len(), 3);
    for record in records {
        assert_eq!(record["blocks"], 64);
        assert_eq!(record["top_k"], serde_json::json!([1, 5]));
        assert_eq!(record["recall_at_k"], serde_json::json!([1.0, 1.0]));
        assert_eq!(record["memory_source"], "linux-proc-status");
        assert!(record["rss_before_build_bytes"].as_u64().unwrap_or(0) > 0);
        assert!(record["peak_rss_bytes"].as_u64().unwrap_or(0) > 0);
    }

    let second = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("candidate-profile")
        .arg("--blocks")
        .arg("64")
        .arg("--json-output")
        .arg(&path)
        .output()
        .expect("pdfbench runs");
    assert_eq!(
        second.status.code(),
        Some(2),
        "create_new must refuse overwrite"
    );
    std::fs::remove_file(&path).expect("temp json artifact removed");
}
