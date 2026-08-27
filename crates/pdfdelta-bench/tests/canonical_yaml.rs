use std::sync::Arc;

use pdfdelta_bench::{
    canonical::{CanonicalRenderDocument, MAX_CANONICAL_YAML_BYTES},
    mutation::RenderPlan,
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
