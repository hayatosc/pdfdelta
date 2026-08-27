use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use pdfdelta_bench::{
    canonical::{CanonicalRenderDocument, MAX_CANONICAL_YAML_BYTES},
    evaluator::{evaluate, evaluate_rendered},
    mutation::{Mutation, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};
use pdfdelta_core::{
    layout::{reconstruct_blocks, reconstruct_lines},
    normalize::normalize_blocks,
    pdf::{LopdfParser, ParseLimits},
    pipeline::PipelineOptions,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};

const EXAMPLE_YAML: &str = r#"
document:
  title: Quarterly Service Report
  sections:
    - id: availability
      heading: Service availability
      paragraphs:
        - id: availability-p1
          text: Release 10 remains available during the transition.
    - id: support
      heading: Customer support
      paragraphs:
        - id: support-p1
          text: Support hours remain unchanged.
"#;

const TYPST_FIXTURE_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/external/case3-typst/document.yaml"
));
const TYPST_OLD_PDF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/external/case3-typst/old.pdf"
));
const TYPST_NEW_PDF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/external/case3-typst/new.pdf"
));

#[test]
fn canonical_yaml_preserves_document_order() {
    let document = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML).expect("valid canonical YAML");

    assert_eq!(document.title(), "Quarterly Service Report");
    assert_eq!(document.sections().len(), 2);
    assert_eq!(document.sections()[0].id(), "availability");
    assert_eq!(document.sections()[0].heading(), "Service availability");
    assert_eq!(
        document.sections()[0].paragraphs()[0].id(),
        "availability-p1"
    );
    assert_eq!(
        document.render_lines(),
        [
            "Quarterly Service Report",
            "Service availability",
            "Release 10 remains available during the transition.",
            "Customer support",
            "Support hours remain unchanged.",
        ]
    );
}

#[test]
fn canonical_yaml_renders_through_each_project_renderer() {
    let document = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML).expect("valid canonical YAML");
    let expected = document.render_lines().join(" ");
    let plan = RenderPlan::new(vec![document.render_lines()], 30).expect("valid render plan");

    for renderer in RendererKind::all() {
        assert_eq!(
            normalized_text(&plan, renderer),
            expected,
            "{}",
            renderer.name()
        );
    }
}

#[test]
fn canonical_mutation_evaluates_vendored_typst_output() {
    let document =
        CanonicalRenderDocument::from_yaml(TYPST_FIXTURE_YAML).expect("valid fixture YAML");
    let plan = Mutation::NumberReplace {
        paragraph_id: "release".to_owned(),
        new_number: "20".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect("fixture mutation applies");

    let record = evaluate_rendered(
        "yaml-number-replace",
        plan.old(),
        plan.new_plan(),
        plan.expectation(),
        "typst-0.15.1",
        Arc::<[u8]>::from(TYPST_OLD_PDF),
        Arc::<[u8]>::from(TYPST_NEW_PDF),
    )
    .expect("vendored Typst pair evaluates");

    assert!(record.passed, "{}", record.detail);
    assert_eq!(record.renderer, "typst-0.15.1");
    assert_eq!(
        record.actual_kinds,
        [pdfdelta_core::diff::ChangeKind::Replacement]
    );
    assert_eq!(record.old_coverage, Some(1.0));
    assert_eq!(record.new_coverage, Some(1.0));
}

#[test]
fn paragraph_text_mutations_feed_the_evaluator_without_dropping_metadata() {
    let yaml = EXAMPLE_YAML.replace(
        "Release 10 remains available during the transition.",
        "A very simple Release 10 note remains stable.",
    );
    let document = CanonicalRenderDocument::from_yaml(&yaml).expect("valid canonical YAML");
    let original_lines = document.render_lines();
    let cases = [
        (
            "yaml-text-replace",
            Mutation::TextReplace {
                paragraph_id: "availability-p1".to_owned(),
                new_text: "A very simple Release 20 note remains stable.".to_owned(),
            },
            "A very simple Release 20 note remains stable.",
            "replacement",
        ),
        (
            "yaml-text-insert",
            Mutation::TextInsert {
                paragraph_id: "availability-p1".to_owned(),
                at: "A very simple Release 10 note ".chars().count(),
                text: "2026 ".to_owned(),
            },
            "A very simple Release 10 note 2026 remains stable.",
            "insertion",
        ),
        (
            "yaml-text-delete",
            Mutation::TextDelete {
                paragraph_id: "availability-p1".to_owned(),
                start: "A ".chars().count(),
                end: "A very ".chars().count(),
            },
            "A simple Release 10 note remains stable.",
            "deletion",
        ),
        (
            "yaml-number-replace",
            Mutation::NumberReplace {
                paragraph_id: "availability-p1".to_owned(),
                new_number: "20".to_owned(),
            },
            "A very simple Release 20 note remains stable.",
            "replacement",
        ),
    ];

    for (name, mutation, expected_text, expected_label) in cases {
        let plan = mutation
            .apply_to_render_document(&document, 30)
            .expect("paragraph-local mutation applies");
        assert_eq!(plan.old().pages(), std::slice::from_ref(&original_lines));
        let mut expected_lines = original_lines.clone();
        expected_lines[2] = expected_text.to_owned();
        assert_eq!(
            plan.new_plan().pages(),
            std::slice::from_ref(&expected_lines)
        );
        assert_eq!(plan.expectation().label(), expected_label);

        for renderer in RendererKind::all() {
            let record = evaluate(
                name,
                plan.old(),
                plan.new_plan(),
                plan.expectation(),
                renderer,
            )
            .expect("structured mutation evaluates");
            assert!(
                record.passed,
                "{name}/{}: {}",
                renderer.name(),
                record.detail
            );
        }
    }
}

#[test]
fn paragraph_line_wrap_preserves_metadata_and_feeds_the_evaluator() {
    let document = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML).expect("valid canonical YAML");
    let plan = Mutation::LineWrap {
        paragraph_id: "availability-p1".to_owned(),
        after_word: 4,
    }
    .apply_to_render_document(&document, 30)
    .expect("paragraph line wrap applies");

    assert_eq!(plan.old().pages()[0], document.render_lines());
    assert_eq!(
        plan.new_plan().pages()[0],
        [
            "Quarterly Service Report",
            "Service availability",
            "Release 10 remains available",
            "during the transition.",
            "Customer support",
            "Support hours remain unchanged.",
        ]
    );
    assert_eq!(plan.expectation().label(), "none");

    for renderer in RendererKind::all() {
        let record = evaluate(
            "yaml-line-wrap",
            plan.old(),
            plan.new_plan(),
            plan.expectation(),
            renderer,
        )
        .expect("structured paragraph line wrap evaluates");
        assert_eq!(record.actual_changes, 0);
        assert!(record.passed, "{}: {}", renderer.name(), record.detail);
    }
}

#[test]
fn paragraph_page_break_preserves_metadata_order_and_feeds_the_evaluator() {
    let yaml = EXAMPLE_YAML.replace(
        "        - id: availability-p1\n          text: Release 10 remains available during the transition.\n",
        concat!(
            "        - id: availability-p1\n",
            "          text: Opening availability context remains stable.\n",
            "        - id: availability-p2\n",
            "          text: Continued availability context remains stable.\n",
        ),
    );
    let document = CanonicalRenderDocument::from_yaml(&yaml).expect("valid canonical YAML");
    let cases = [
        (
            "availability-p2",
            vec![
                "Quarterly Service Report".to_owned(),
                "Service availability".to_owned(),
                "Opening availability context remains stable.".to_owned(),
            ],
            vec![
                "Continued availability context remains stable.".to_owned(),
                "Customer support".to_owned(),
                "Support hours remain unchanged.".to_owned(),
            ],
        ),
        (
            "support-p1",
            vec![
                "Quarterly Service Report".to_owned(),
                "Service availability".to_owned(),
                "Opening availability context remains stable.".to_owned(),
                "Continued availability context remains stable.".to_owned(),
                "Customer support".to_owned(),
            ],
            vec!["Support hours remain unchanged.".to_owned()],
        ),
    ];

    for (paragraph_id, first_page, second_page) in cases {
        let plan = Mutation::PageBreakBefore {
            paragraph_id: paragraph_id.to_owned(),
        }
        .apply_to_render_document(&document, 30)
        .expect("paragraph-targeted page break applies");

        assert_eq!(plan.old().pages()[0], document.render_lines());
        assert_eq!(plan.new_plan().pages(), &[first_page, second_page]);
        assert_eq!(
            plan.new_plan()
                .pages()
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>(),
            document.render_lines()
        );
        assert_eq!(plan.expectation().label(), "none");

        for renderer in RendererKind::all() {
            let record = evaluate(
                "yaml-page-break",
                plan.old(),
                plan.new_plan(),
                plan.expectation(),
                renderer,
            )
            .expect("structured paragraph page break evaluates");
            assert_eq!(record.actual_changes, 0);
            assert!(record.passed, "{}: {}", renderer.name(), record.detail);
        }
    }
}

#[test]
fn global_rendering_mutations_preserve_content_and_feed_the_evaluator() {
    let document = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML).expect("valid canonical YAML");
    let cases = [
        (
            "yaml-line-height-change",
            Mutation::LineHeightChange { new_line_gap: 34 },
        ),
        (
            "yaml-margin-change",
            Mutation::MarginChange { new_margin: 48 },
        ),
        (
            "yaml-font-size-change",
            Mutation::FontSizeChange { new_font_size: 12 },
        ),
        (
            "yaml-page-size-change",
            Mutation::PageSizeChange {
                new_page_width: 640,
                new_page_height: 800,
            },
        ),
    ];

    for (name, mutation) in cases {
        let plan = mutation
            .apply_to_render_document(&document, 30)
            .expect("global rendering mutation applies");
        assert_eq!(plan.old().pages(), plan.new_plan().pages());
        assert_eq!(plan.old().pages()[0], document.render_lines());
        assert_eq!(plan.expectation().label(), "none");
        match mutation {
            Mutation::LineHeightChange { new_line_gap } => {
                assert_ne!(plan.old().line_gap(), plan.new_plan().line_gap());
                assert_eq!(plan.new_plan().line_gap(), new_line_gap);
            }
            Mutation::MarginChange { new_margin } => {
                assert_ne!(plan.old().margin(), plan.new_plan().margin());
                assert_eq!(plan.new_plan().margin(), new_margin);
            }
            Mutation::FontSizeChange { new_font_size } => {
                assert_ne!(plan.old().font_size(), plan.new_plan().font_size());
                assert_eq!(plan.new_plan().font_size(), new_font_size);
            }
            Mutation::PageSizeChange {
                new_page_width,
                new_page_height,
            } => {
                assert_ne!(plan.old().page_width(), plan.new_plan().page_width());
                assert_ne!(plan.old().page_height(), plan.new_plan().page_height());
                assert_eq!(plan.new_plan().page_width(), new_page_width);
                assert_eq!(plan.new_plan().page_height(), new_page_height);
            }
            _ => panic!("test cases contain only global rendering mutations"),
        }

        for renderer in RendererKind::all() {
            let record = evaluate(
                name,
                plan.old(),
                plan.new_plan(),
                plan.expectation(),
                renderer,
            )
            .expect("global rendering mutation evaluates");
            assert_eq!(record.actual_changes, 0);
            assert!(
                record.passed,
                "{name}/{}: {}",
                renderer.name(),
                record.detail
            );
        }
    }
}

#[test]
fn single_section_column_change_preserves_metadata_and_feeds_the_evaluator() {
    let yaml = r#"
document:
  title: Column report
  sections:
    - id: body
      heading: Report body
      paragraphs:
        - id: body-p1
          text: Left first paragraph remains stable.
        - id: body-p2
          text: Left second paragraph remains stable.
        - id: body-p3
          text: Right first paragraph remains stable.
        - id: body-p4
          text: Right second paragraph remains stable.
"#;
    let document = CanonicalRenderDocument::from_yaml(yaml).expect("valid canonical YAML");
    let plan = Mutation::ColumnChangeInSection {
        section_id: "body".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect("single-section column change applies");

    assert_eq!(plan.old().pages()[0], document.render_lines());
    assert_eq!(plan.new_plan().pages()[0], document.render_lines());
    assert_eq!(plan.expectation().label(), "none");

    for renderer in RendererKind::all() {
        let record = evaluate(
            "yaml-column-change",
            plan.old(),
            plan.new_plan(),
            plan.expectation(),
            renderer,
        )
        .expect("structured column change evaluates");
        assert_eq!(record.actual_changes, 0);
        assert!(record.passed, "{}: {}", renderer.name(), record.detail);
    }
}

#[test]
fn multi_section_column_change_preserves_surrounding_sections_and_feeds_the_evaluator() {
    let yaml = r#"
document:
  title: Column report
  sections:
    - id: introduction
      heading: Introduction
      paragraphs:
        - id: intro-p1
          text: Introductory context remains full width.
    - id: body
      heading: Report body
      paragraphs:
        - id: body-p1
          text: Left first paragraph remains stable.
        - id: body-p2
          text: Left second paragraph remains stable.
        - id: body-p3
          text: Right first paragraph remains stable.
        - id: body-p4
          text: Right second paragraph remains stable.
    - id: conclusion
      heading: Conclusion
      paragraphs:
        - id: conclusion-p1
          text: Closing context remains full width.
"#;
    let document = CanonicalRenderDocument::from_yaml(yaml).expect("valid canonical YAML");
    let plan = Mutation::ColumnChangeInSection {
        section_id: "body".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect("multi-section column change applies");

    assert_eq!(plan.old().pages()[0], document.render_lines());
    assert_eq!(plan.new_plan().pages()[0], document.render_lines());
    assert_eq!(plan.expectation().label(), "none");

    for renderer in RendererKind::all() {
        let record = evaluate(
            "yaml-column-change",
            plan.old(),
            plan.new_plan(),
            plan.expectation(),
            renderer,
        )
        .expect("multi-section column change evaluates");
        assert_eq!(record.actual_changes, 0);
        assert!(record.passed, "{}: {}", renderer.name(), record.detail);
    }
}

#[test]
fn paragraph_deletion_preserves_its_section_and_feeds_the_evaluator() {
    let yaml = EXAMPLE_YAML.replace(
        "        - id: availability-p1\n          text: Release 10 remains available during the transition.\n",
        concat!(
            "        - id: availability-p1\n",
            "          text: Opening availability context remains stable.\n",
            "        - id: availability-p2\n",
            "          text: Obsolete availability guidance is removed.\n",
            "        - id: availability-p3\n",
            "          text: Closing availability context remains stable.\n",
        ),
    );
    let document = CanonicalRenderDocument::from_yaml(&yaml).expect("valid canonical YAML");
    let plan = Mutation::ParagraphDelete {
        paragraph_id: "availability-p2".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect("paragraph deletion retains a nonempty section");

    assert_eq!(plan.old().pages()[0], document.render_lines());
    assert_eq!(
        plan.new_plan().pages()[0],
        [
            "Quarterly Service Report",
            "Service availability",
            "Opening availability context remains stable.",
            "Closing availability context remains stable.",
            "Customer support",
            "Support hours remain unchanged.",
        ]
    );
    assert_eq!(plan.expectation().label(), "deletion");

    for renderer in RendererKind::all() {
        let record = evaluate(
            "yaml-paragraph-delete",
            plan.old(),
            plan.new_plan(),
            plan.expectation(),
            renderer,
        )
        .expect("structured paragraph deletion evaluates");
        assert!(record.passed, "{}: {}", renderer.name(), record.detail);
    }
}

#[test]
fn section_local_paragraph_insertion_preserves_structure_and_feeds_the_evaluator() {
    let yaml = EXAMPLE_YAML.replace(
        "        - id: availability-p1\n          text: Release 10 remains available during the transition.\n",
        concat!(
            "        - id: availability-p1\n",
            "          text: Opening availability context remains stable.\n",
            "        - id: availability-p2\n",
            "          text: Closing availability context remains stable.\n",
        ),
    );
    let document = CanonicalRenderDocument::from_yaml(&yaml).expect("valid canonical YAML");
    let cases = [
        (
            0,
            "availability-insert-start",
            "Inserted opening detail.",
            2,
        ),
        (
            1,
            "availability-insert-middle",
            "Inserted middle detail.",
            3,
        ),
        (2, "availability-insert-end", "Inserted closing detail.", 4),
    ];

    for (index, paragraph_id, text, flattened_index) in cases {
        let plan = Mutation::ParagraphInsertInSection {
            section_id: "availability".to_owned(),
            index,
            paragraph_id: paragraph_id.to_owned(),
            text: text.to_owned(),
        }
        .apply_to_render_document(&document, 30)
        .expect("section-local paragraph insertion applies");

        assert_eq!(plan.old().pages()[0], document.render_lines());
        let mut expected_lines = document.render_lines();
        expected_lines.insert(flattened_index, text.to_owned());
        assert_eq!(plan.new_plan().pages()[0], expected_lines);
        assert_eq!(plan.expectation().label(), "insertion");

        for renderer in RendererKind::all() {
            let record = evaluate(
                "yaml-paragraph-insert",
                plan.old(),
                plan.new_plan(),
                plan.expectation(),
                renderer,
            )
            .expect("structured paragraph insertion evaluates");
            assert_eq!(record.actual_changes, 1);
            assert!(record.passed, "{}: {}", renderer.name(), record.detail);
        }
    }
}

#[test]
fn section_local_paragraph_insertion_reserves_generated_metadata_ids() {
    let document = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML).expect("valid canonical YAML");
    let plan = Mutation::ParagraphInsertInSection {
        section_id: "support".to_owned(),
        index: 1,
        paragraph_id: "pdfdelta-title-0".to_owned(),
        text: "Inserted support detail.".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect("source-style metadata id remains usable");

    assert_eq!(plan.old().pages()[0][0], document.title());
    assert_eq!(plan.new_plan().pages()[0][0], document.title());
    assert_eq!(
        plan.new_plan().pages()[0].last().map(String::as_str),
        Some("Inserted support detail.")
    );
}

#[test]
fn same_section_paragraph_move_preserves_structure_and_feeds_the_evaluator() {
    let yaml = EXAMPLE_YAML.replace(
        "        - id: availability-p1\n          text: Release 10 remains available during the transition.\n",
        concat!(
            "        - id: availability-p1\n",
            "          text: Opening availability paragraph remains stable and identifies the start.\n",
            "        - id: availability-p2\n",
            "          text: Middle availability paragraph remains stable and identifies the center.\n",
            "        - id: availability-p3\n",
            "          text: Closing availability paragraph remains stable and identifies the end.\n",
        ),
    );
    let document = CanonicalRenderDocument::from_yaml(&yaml).expect("valid canonical YAML");
    let cases = [
        (
            "availability-p1",
            2,
            [
                "Middle availability paragraph remains stable and identifies the center.",
                "Closing availability paragraph remains stable and identifies the end.",
                "Opening availability paragraph remains stable and identifies the start.",
            ],
        ),
        (
            "availability-p3",
            0,
            [
                "Closing availability paragraph remains stable and identifies the end.",
                "Opening availability paragraph remains stable and identifies the start.",
                "Middle availability paragraph remains stable and identifies the center.",
            ],
        ),
    ];

    for (paragraph_id, to_index, expected_paragraphs) in cases {
        let plan = Mutation::ParagraphMoveInSection {
            paragraph_id: paragraph_id.to_owned(),
            to_index,
        }
        .apply_to_render_document(&document, 30)
        .expect("same-section paragraph move applies");

        assert_eq!(plan.old().pages()[0], document.render_lines());
        assert_eq!(
            &plan.new_plan().pages()[0][..2],
            ["Quarterly Service Report", "Service availability"]
        );
        assert_eq!(&plan.new_plan().pages()[0][2..5], expected_paragraphs);
        assert_eq!(
            &plan.new_plan().pages()[0][5..],
            ["Customer support", "Support hours remain unchanged."]
        );
        assert_eq!(plan.expectation().label(), "move");

        for renderer in RendererKind::all() {
            let record = evaluate(
                "yaml-paragraph-move",
                plan.old(),
                plan.new_plan(),
                plan.expectation(),
                renderer,
            )
            .expect("structured paragraph move evaluates");
            assert_eq!(record.actual_changes, 1);
            assert!(record.passed, "{}: {}", renderer.name(), record.detail);
        }
    }
}

#[test]
fn cross_section_paragraph_move_preserves_structure_and_feeds_the_evaluator() {
    let yaml = r#"
document:
  title: Transfer report
  sections:
    - id: source
      heading: Source section
      paragraphs:
        - id: source-p1
          text: This movable source paragraph has unique transfer details.
        - id: source-p2
          text: This source paragraph remains under the source heading.
    - id: destination
      heading: Destination section
      paragraphs:
        - id: destination-p1
          text: This destination paragraph remains first.
        - id: destination-p2
          text: This destination paragraph remains last.
"#;
    let document = CanonicalRenderDocument::from_yaml(yaml).expect("valid canonical YAML");
    let cases = [
        (
            "source-p1",
            "destination",
            1,
            [
                "Transfer report",
                "Source section",
                "This source paragraph remains under the source heading.",
                "Destination section",
                "This destination paragraph remains first.",
                "This movable source paragraph has unique transfer details.",
                "This destination paragraph remains last.",
            ],
        ),
        (
            "destination-p2",
            "source",
            0,
            [
                "Transfer report",
                "Source section",
                "This destination paragraph remains last.",
                "This movable source paragraph has unique transfer details.",
                "This source paragraph remains under the source heading.",
                "Destination section",
                "This destination paragraph remains first.",
            ],
        ),
    ];

    for (paragraph_id, to_section_id, to_index, expected_lines) in cases {
        let plan = Mutation::ParagraphMoveToSection {
            paragraph_id: paragraph_id.to_owned(),
            to_section_id: to_section_id.to_owned(),
            to_index,
        }
        .apply_to_render_document(&document, 30)
        .expect("cross-section paragraph move applies");

        assert_eq!(plan.old().pages()[0], document.render_lines());
        assert_eq!(plan.new_plan().pages()[0], expected_lines);
        assert_eq!(plan.expectation().label(), "move");

        for renderer in RendererKind::all() {
            let record = evaluate(
                "yaml-paragraph-move",
                plan.old(),
                plan.new_plan(),
                plan.expectation(),
                renderer,
            )
            .expect("cross-section paragraph move evaluates");
            assert_eq!(record.actual_changes, 1);
            assert!(record.passed, "{}: {}", renderer.name(), record.detail);
        }
    }
}

#[test]
fn structured_mutations_reject_metadata_targets_empty_sections_and_ambiguous_changes() {
    let document = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML).expect("valid canonical YAML");
    let metadata_error = Mutation::TextReplace {
        paragraph_id: "pdfdelta-title-0".to_owned(),
        new_text: "Changed title".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect_err("metadata must not be a mutation target")
    .to_string();
    assert!(
        metadata_error.contains("unknown structured paragraph id"),
        "{metadata_error}"
    );

    let deletion_error = Mutation::ParagraphDelete {
        paragraph_id: "availability-p1".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect_err("deletion must not empty a section")
    .to_string();
    assert!(
        deletion_error.contains("cannot leave section"),
        "{deletion_error}"
    );

    let move_error = Mutation::ParagraphMove {
        paragraph_id: "availability-p1".to_owned(),
        to_index: 1,
    }
    .apply_to_render_document(&document, 30)
    .expect_err("section-ambiguous mutation must be rejected")
    .to_string();
    assert!(
        move_error.contains("currently support only"),
        "{move_error}"
    );

    let page_break_error = Mutation::PageBreakBefore {
        paragraph_id: "pdfdelta-title-0".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect_err("metadata must not be a page-break target")
    .to_string();
    assert!(
        page_break_error.contains("unknown structured paragraph id"),
        "{page_break_error}"
    );

    for (mutation, expected) in [
        (
            Mutation::ParagraphInsertInSection {
                section_id: "missing".to_owned(),
                index: 0,
                paragraph_id: "inserted".to_owned(),
                text: "Inserted paragraph.".to_owned(),
            },
            "unknown structured section id",
        ),
        (
            Mutation::ParagraphInsertInSection {
                section_id: "availability".to_owned(),
                index: 2,
                paragraph_id: "inserted".to_owned(),
                text: "Inserted paragraph.".to_owned(),
            },
            "exceeds section",
        ),
        (
            Mutation::ParagraphInsertInSection {
                section_id: "availability".to_owned(),
                index: 0,
                paragraph_id: "support-p1".to_owned(),
                text: "Duplicate source id.".to_owned(),
            },
            "duplicate paragraph id",
        ),
    ] {
        let error = mutation
            .apply_to_render_document(&document, 30)
            .expect_err("invalid section-local insertion is rejected")
            .to_string();
        assert!(error.contains(expected), "{error}");
    }

    for (mutation, expected) in [
        (
            Mutation::ParagraphMoveInSection {
                paragraph_id: "availability-p1".to_owned(),
                to_index: 0,
            },
            "must change its section-local index",
        ),
        (
            Mutation::ParagraphMoveInSection {
                paragraph_id: "availability-p1".to_owned(),
                to_index: 1,
            },
            "exceeds section",
        ),
        (
            Mutation::ParagraphMoveInSection {
                paragraph_id: "pdfdelta-title-0".to_owned(),
                to_index: 0,
            },
            "unknown structured paragraph id",
        ),
    ] {
        let error = mutation
            .apply_to_render_document(&document, 30)
            .expect_err("invalid same-section move is rejected")
            .to_string();
        assert!(error.contains(expected), "{error}");
    }

    let cross_section_document = CanonicalRenderDocument::from_yaml(
        r#"
document:
  title: Transfer report
  sections:
    - id: source
      heading: Source section
      paragraphs:
        - id: source-p1
          text: Movable source paragraph.
        - id: source-p2
          text: Retained source paragraph.
    - id: destination
      heading: Destination section
      paragraphs:
        - id: destination-p1
          text: Existing destination paragraph.
"#,
    )
    .expect("cross-section YAML is valid");
    for (mutation, expected) in [
        (
            Mutation::ParagraphMoveToSection {
                paragraph_id: "source-p1".to_owned(),
                to_section_id: "source".to_owned(),
                to_index: 0,
            },
            "requires a different destination section",
        ),
        (
            Mutation::ParagraphMoveToSection {
                paragraph_id: "source-p1".to_owned(),
                to_section_id: "missing".to_owned(),
                to_index: 0,
            },
            "unknown structured section id",
        ),
        (
            Mutation::ParagraphMoveToSection {
                paragraph_id: "missing".to_owned(),
                to_section_id: "destination".to_owned(),
                to_index: 0,
            },
            "unknown structured paragraph id",
        ),
        (
            Mutation::ParagraphMoveToSection {
                paragraph_id: "source-p1".to_owned(),
                to_section_id: "destination".to_owned(),
                to_index: 2,
            },
            "exceeds destination section",
        ),
        (
            Mutation::ParagraphMoveToSection {
                paragraph_id: "destination-p1".to_owned(),
                to_section_id: "source".to_owned(),
                to_index: 0,
            },
            "cannot leave source section",
        ),
    ] {
        let error = mutation
            .apply_to_render_document(&cross_section_document, 30)
            .expect_err("invalid cross-section move is rejected")
            .to_string();
        assert!(error.contains(expected), "{error}");
    }

    let single_section = CanonicalRenderDocument::from_yaml(
        r#"
document:
  title: Short report
  sections:
    - id: body
      heading: Report body
      paragraphs:
        - id: body-p1
          text: Only paragraph.
"#,
    )
    .expect("single-section YAML is valid");
    for (section_id, expected) in [
        ("missing", "unknown structured section id"),
        ("body", "requires at least four paragraphs"),
    ] {
        let error = Mutation::ColumnChangeInSection {
            section_id: section_id.to_owned(),
        }
        .apply_to_render_document(&single_section, 30)
        .expect_err("invalid structured column change is rejected")
        .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn generated_metadata_ids_do_not_shadow_source_paragraph_ids() {
    let yaml = EXAMPLE_YAML.replace("availability-p1", "pdfdelta-title-0");
    let document = CanonicalRenderDocument::from_yaml(&yaml).expect("valid canonical YAML");
    let plan = Mutation::NumberReplace {
        paragraph_id: "pdfdelta-title-0".to_owned(),
        new_number: "20".to_owned(),
    }
    .apply_to_render_document(&document, 30)
    .expect("source paragraph remains addressable");

    assert_eq!(plan.old().pages()[0], document.render_lines());
    assert_eq!(
        plan.new_plan().pages()[0][2],
        "Release 20 remains available during the transition."
    );
    assert_eq!(plan.new_plan().pages()[0][0], document.title());
}

#[test]
fn render_command_publishes_a_new_pdf_without_overwriting() {
    let (input, output) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");

    let first = run_render_command(&input, &output);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stderr.is_empty());
    let stdout = String::from_utf8(first.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("classic-xref-tj"));
    let first_pdf = fs::read(&output).expect("rendered PDF is published");
    let expected = CanonicalRenderDocument::from_yaml(EXAMPLE_YAML)
        .expect("valid canonical YAML")
        .render_lines()
        .join(" ");
    assert_eq!(normalized_pdf_text(first_pdf.clone()), expected);

    let second = run_render_command(&input, &output);
    assert_eq!(second.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&second.stderr).contains("already exists"));
    assert_eq!(
        fs::read(&output).expect("first PDF remains published"),
        first_pdf
    );

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
    fs::remove_file(&output).expect("temporary PDF is removed");
}

#[test]
fn evaluate_yaml_command_reports_a_passing_record_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output = run_evaluate_command(
            &input,
            renderer,
            &[
                "number-replace",
                "--paragraph-id",
                "availability-p1",
                "--new-number",
                "20",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-number-replace renderer={renderer} expected=replacement \
                 actual=replacement coverage=1.000/1.000"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_rendered_yaml_command_reports_vendored_typst_result() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case3-typst");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("evaluate-rendered-yaml")
        .arg(fixture.join("document.yaml"))
        .arg("--old-pdf")
        .arg(fixture.join("old.pdf"))
        .arg("--new-pdf")
        .arg(fixture.join("new.pdf"))
        .arg("--renderer")
        .arg("typst-0.15.1")
        .args([
            "number-replace",
            "--paragraph-id",
            "release",
            "--new-number",
            "20",
        ])
        .output()
        .expect("pdfbench runs");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        stdout.contains(
            "PASS case=yaml-number-replace renderer=typst-0.15.1 expected=replacement \
             actual=replacement coverage=1.000/1.000"
        ),
        "{stdout}"
    );
}

#[test]
fn evaluate_yaml_command_runs_each_global_rendering_mutation() {
    let (input, _) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");
    let cases: [(&str, &[&str]); 4] = [
        (
            "yaml-line-height-change",
            &["line-height-change", "--new-line-gap", "34"],
        ),
        (
            "yaml-margin-change",
            &["margin-change", "--new-margin", "48"],
        ),
        (
            "yaml-font-size-change",
            &["font-size-change", "--new-font-size", "12"],
        ),
        (
            "yaml-page-size-change",
            &[
                "page-size-change",
                "--new-page-width",
                "640",
                "--new-page-height",
                "800",
            ],
        ),
    ];

    for (case_name, arguments) in cases {
        let output = run_evaluate_command(&input, "lopdf-tj", arguments);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case={case_name} renderer=lopdf-tj expected=none actual=none"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_runs_multi_section_column_change_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    let yaml = r#"
document:
  title: Column report
  sections:
    - id: introduction
      heading: Introduction
      paragraphs:
        - id: intro-p1
          text: Introductory context remains full width.
    - id: body
      heading: Report body
      paragraphs:
        - id: body-p1
          text: Left first paragraph remains stable.
        - id: body-p2
          text: Left second paragraph remains stable.
        - id: body-p3
          text: Right first paragraph remains stable.
        - id: body-p4
          text: Right second paragraph remains stable.
    - id: conclusion
      heading: Conclusion
      paragraphs:
        - id: conclusion-p1
          text: Closing context remains full width.
"#;
    fs::write(&input, yaml).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output =
            run_evaluate_command(&input, renderer, &["column-change", "--section-id", "body"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-column-change renderer={renderer} expected=none actual=none"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_runs_paragraph_line_wrap_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output = run_evaluate_command(
            &input,
            renderer,
            &[
                "line-wrap",
                "--paragraph-id",
                "availability-p1",
                "--after-word",
                "4",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-line-wrap renderer={renderer} expected=none actual=none"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_runs_paragraph_page_break_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output = run_evaluate_command(
            &input,
            renderer,
            &["page-break", "--before-paragraph-id", "support-p1"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-page-break renderer={renderer} expected=none actual=none"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_runs_section_local_paragraph_insertion_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output = run_evaluate_command(
            &input,
            renderer,
            &[
                "paragraph-insert",
                "--section-id",
                "availability",
                "--index",
                "1",
                "--paragraph-id",
                "availability-p2",
                "--text",
                "Inserted availability detail.",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-paragraph-insert renderer={renderer} \
                 expected=insertion actual=insertion coverage=1.000/1.000"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_runs_same_section_paragraph_move_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    let yaml = EXAMPLE_YAML.replace(
        "        - id: availability-p1\n          text: Release 10 remains available during the transition.\n",
        concat!(
            "        - id: availability-p1\n",
            "          text: Opening availability paragraph remains stable and identifies the start.\n",
            "        - id: availability-p2\n",
            "          text: Middle availability paragraph remains stable and identifies the center.\n",
            "        - id: availability-p3\n",
            "          text: Closing availability paragraph remains stable and identifies the end.\n",
        ),
    );
    fs::write(&input, yaml).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output = run_evaluate_command(
            &input,
            renderer,
            &[
                "paragraph-move",
                "--paragraph-id",
                "availability-p3",
                "--to-index",
                "0",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-paragraph-move renderer={renderer} expected=move actual=move \
                 coverage=1.000/1.000"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_runs_cross_section_paragraph_move_for_each_renderer() {
    let (input, _) = temp_fixture_paths();
    let yaml = EXAMPLE_YAML.replace(
        "        - id: availability-p1\n          text: Release 10 remains available during the transition.\n",
        concat!(
            "        - id: availability-p1\n",
            "          text: Opening availability paragraph remains stable and identifies the start.\n",
            "        - id: availability-p2\n",
            "          text: Middle availability paragraph remains stable and identifies the center.\n",
            "        - id: availability-p3\n",
            "          text: Closing availability paragraph moves to customer support.\n",
        ),
    );
    fs::write(&input, yaml).expect("temporary canonical YAML is written");

    for renderer in ["lopdf-tj", "classic-xref-tj"] {
        let output = run_evaluate_command(
            &input,
            renderer,
            &[
                "paragraph-move",
                "--paragraph-id",
                "availability-p3",
                "--to-section-id",
                "support",
                "--to-index",
                "1",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains(&format!(
                "PASS case=yaml-paragraph-move renderer={renderer} expected=move actual=move \
                 coverage=1.000/1.000"
            )),
            "{stdout}"
        );
    }

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn evaluate_yaml_command_rejects_invalid_mutation_inputs() {
    let (input, _) = temp_fixture_paths();
    fs::write(&input, EXAMPLE_YAML).expect("temporary canonical YAML is written");

    for (arguments, expected) in [
        (
            [
                "number-replace",
                "--paragraph-id",
                "missing",
                "--new-number",
                "20",
            ],
            "unknown structured paragraph id",
        ),
        (
            [
                "number-replace",
                "--paragraph-id",
                "availability-p1",
                "--new-number",
                "twenty",
            ],
            "number replacement must use nonempty ASCII digits",
        ),
        (
            [
                "line-wrap",
                "--paragraph-id",
                "availability-p1",
                "--after-word",
                "0",
            ],
            "line wrap after_word must split paragraph",
        ),
    ] {
        let output = run_evaluate_command(&input, "lopdf-tj", &arguments);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        assert!(stderr.contains(expected), "{stderr}");
    }

    let unchanged_margin =
        run_evaluate_command(&input, "lopdf-tj", &["margin-change", "--new-margin", "36"]);
    assert_eq!(unchanged_margin.status.code(), Some(2));
    assert!(unchanged_margin.stdout.is_empty());
    let stderr = String::from_utf8(unchanged_margin.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("margin change must alter"), "{stderr}");

    let metadata_page_break = run_evaluate_command(
        &input,
        "lopdf-tj",
        &["page-break", "--before-paragraph-id", "pdfdelta-title-0"],
    );
    assert_eq!(metadata_page_break.status.code(), Some(2));
    assert!(metadata_page_break.stdout.is_empty());
    let stderr = String::from_utf8(metadata_page_break.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("unknown structured paragraph id"),
        "{stderr}"
    );

    fs::remove_file(&input).expect("temporary canonical YAML is removed");
}

#[test]
fn canonical_yaml_rejects_invalid_structure_and_fields() {
    for (name, yaml, expected) in [
        (
            "unknown field",
            EXAMPLE_YAML.replace(
                "  title: Quarterly Service Report",
                "  title: Quarterly Service Report\n  subtitle: Unknown",
            ),
            "unknown field",
        ),
        (
            "blank title",
            EXAMPLE_YAML.replace("Quarterly Service Report", "''"),
            "document title must not be blank",
        ),
        (
            "no sections",
            "document:\n  title: Report\n  sections: []\n".to_owned(),
            "requires at least one section",
        ),
        (
            "empty section",
            "document:\n  title: Report\n  sections:\n    - id: section\n      heading: Heading\n      paragraphs: []\n"
                .to_owned(),
            "requires at least one paragraph",
        ),
        (
            "duplicate section",
            EXAMPLE_YAML.replace("    - id: support", "    - id: availability"),
            "duplicate section id",
        ),
        (
            "duplicate paragraph",
            EXAMPLE_YAML.replace("        - id: support-p1", "        - id: availability-p1"),
            "duplicate paragraph id",
        ),
    ] {
        assert_invalid(name, &yaml, expected);
    }
}

#[test]
fn canonical_yaml_rejects_input_and_render_line_limit_overruns() {
    let oversized = "x".repeat(MAX_CANONICAL_YAML_BYTES + 1);
    assert_invalid("input bytes", &oversized, "must not exceed");

    let mut too_many_lines =
        "document:\n  title: Report\n  sections:\n    - id: section\n      heading: Heading\n      paragraphs:\n"
            .to_owned();
    for index in 0..31 {
        too_many_lines.push_str(&format!(
            "        - id: paragraph-{index}\n          text: Paragraph {index}\n"
        ));
    }
    assert_invalid("render lines", &too_many_lines, "rendered lines");
}

fn assert_invalid(name: &str, yaml: &str, expected: &str) {
    let error = CanonicalRenderDocument::from_yaml(yaml)
        .expect_err("canonical YAML must be rejected")
        .to_string();
    assert!(
        error.contains(expected),
        "{name}: expected {expected:?} in {error:?}"
    );
}

fn normalized_text(plan: &RenderPlan, renderer: RendererKind) -> String {
    let pdf = renderer
        .render(plan, RenderLimits::default())
        .expect("canonical fixture renders");
    normalized_pdf_text(pdf)
}

fn normalized_pdf_text(pdf: Vec<u8>) -> String {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract_outcome(
            Arc::from(pdf),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .expect("canonical fixture extracts")
        .into_complete()
        .expect("canonical fixture extraction is complete");
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line).expect("fixture lines reconstruct");
    let blocks =
        reconstruct_blocks(&document, &lines, options.block).expect("fixture blocks reconstruct");
    normalize_blocks(&document, &lines, &blocks)
        .expect("fixture blocks normalize")
        .into_iter()
        .map(|block| block.canonical.text)
        .collect::<Vec<_>>()
        .join(" ")
}

fn temp_fixture_paths() -> (PathBuf, PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let stem = format!("pdfbench-canonical-{}-{nonce}", std::process::id());
    let directory = std::env::temp_dir();
    (
        directory.join(format!("{stem}.yaml")),
        directory.join(format!("{stem}.pdf")),
    )
}

fn run_render_command(input: &Path, output: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("render")
        .arg(input)
        .arg("--renderer")
        .arg("classic-xref-tj")
        .arg("--output")
        .arg(output)
        .output()
        .expect("pdfbench runs")
}

fn run_evaluate_command(
    input: &Path,
    renderer: &str,
    mutation_arguments: &[&str],
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pdfbench"))
        .arg("evaluate-yaml")
        .arg(input)
        .arg("--renderer")
        .arg(renderer)
        .args(mutation_arguments)
        .output()
        .expect("pdfbench runs")
}
