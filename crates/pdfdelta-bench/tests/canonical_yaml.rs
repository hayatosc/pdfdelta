use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use pdfdelta_bench::{
    canonical::{CanonicalRenderDocument, MAX_CANONICAL_YAML_BYTES},
    evaluator::evaluate,
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
