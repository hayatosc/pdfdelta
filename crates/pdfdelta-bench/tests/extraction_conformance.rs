use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use pdfdelta_bench::{
    evaluation::hex_digest,
    extraction_conformance::evaluate_extraction_conformance,
    mutation::RenderPlan,
    renderers::{RenderLimits, RendererKind},
};
use pdfdelta_core::{
    model::DecodedText,
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn curated_pdf_oxide_snapshot_matches_typst_fixture() {
    const PDF: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/external/case1-japanese-typst/old.pdf"
    ));
    const ORACLE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/extraction-conformance/pdf-oxide-0.3.77/case1-japanese-typst-old.oracle.json"
    ));

    let record = evaluate_extraction_conformance(Arc::<[u8]>::from(PDF), ORACLE, 0.25)
        .expect("curated external oracle is valid");

    assert!(record.passed(), "{:?}", record.mismatch);
    assert_eq!(record.producer.name, "pdf_oxide");
    assert_eq!(record.producer.version, "0.3.77");
    assert_eq!(record.producer.parser_family, "pdf_oxide custom parser");
    assert_eq!(record.expected_glyphs, 158);
    assert_eq!(record.actual_glyphs, 158);
}

#[test]
fn command_accepts_a_matching_versioned_external_snapshot() {
    let (pdf, oracle) = fixture_oracle();
    let inputs = TempInputs::new("pass", &pdf, &oracle);

    let output = run_command(&inputs, 0.25, None);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        stdout.starts_with("PASS extraction-conformance "),
        "{stdout}"
    );
    assert!(stdout.contains("producer=\"fixture-position-extractor\""));
    assert!(stdout.contains("parser_family=\"fixture-parser\""));
}

#[test]
fn command_returns_one_for_a_geometry_mismatch() {
    let (pdf, mut oracle) = fixture_oracle();
    let x = oracle["glyphs"][0]["bbox"]["min"]["x"]
        .as_f64()
        .expect("fixture x is numeric");
    oracle["glyphs"][0]["bbox"]["min"]["x"] = json!(x + 1.0);
    let inputs = TempInputs::new("mismatch", &pdf, &oracle);

    let output = run_command(&inputs, 0.25, None);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        stdout.starts_with("FAIL extraction-conformance "),
        "{stdout}"
    );
    assert!(stdout.contains("glyph 0 bbox.min.x mismatch"), "{stdout}");
}

#[test]
fn command_writes_a_mismatch_svg_without_changing_the_failure_contract() {
    let (pdf, mut oracle) = fixture_oracle();
    oracle["glyphs"][0]["text"]["value"] = json!("Different");
    let inputs = TempInputs::new("mismatch-svg", &pdf, &oracle);
    let mismatch_svg = inputs.mismatch_svg_path();

    let output = run_command(&inputs, 0.25, Some(&mismatch_svg));

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        stdout.starts_with("FAIL extraction-conformance "),
        "{stdout}"
    );
    assert!(stdout.contains("glyph 0 text mismatch"), "{stdout}");
    let svg = fs::read_to_string(&mismatch_svg).expect("mismatch SVG is published");
    assert!(svg.contains(r#"role="img""#));
    assert!(svg.contains(r#"class="snapshot-glyph expected mismatch""#));
    assert!(svg.contains(r#"class="snapshot-glyph actual mismatch""#));
}

#[test]
fn command_does_not_create_a_mismatch_svg_for_a_match() {
    let (pdf, oracle) = fixture_oracle();
    let inputs = TempInputs::new("match-svg", &pdf, &oracle);
    let mismatch_svg = inputs.mismatch_svg_path();

    let output = run_command(&inputs, 0.25, Some(&mismatch_svg));

    assert!(output.status.success());
    assert!(!mismatch_svg.exists());
}

#[test]
fn command_refuses_to_overwrite_an_existing_mismatch_svg() {
    let (pdf, mut oracle) = fixture_oracle();
    oracle["glyphs"][0]["text"]["value"] = json!("Different");
    let inputs = TempInputs::new("existing-svg", &pdf, &oracle);
    let mismatch_svg = inputs.mismatch_svg_path();
    fs::write(&mismatch_svg, b"existing evidence\n").expect("existing artifact writes");

    let output = run_command(&inputs, 0.25, Some(&mismatch_svg));

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("destination path already exists"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(&mismatch_svg).expect("existing artifact remains readable"),
        b"existing evidence\n"
    );
}

#[test]
fn command_returns_two_for_missing_producer_identity() {
    let (pdf, mut oracle) = fixture_oracle();
    oracle["producer"]["name"] = json!("");
    let inputs = TempInputs::new("invalid-producer", &pdf, &oracle);

    let output = run_command(&inputs, 0.25, None);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("producer name must be non-empty"),
        "{stderr}"
    );
}

fn fixture_oracle() -> (Vec<u8>, Value) {
    let plan = RenderPlan::new(vec![vec!["Oracle fixture".to_owned()]], 30)
        .expect("fixture render plan is valid");
    let pdf = RendererKind::LopdfTj
        .render(&plan, RenderLimits::default())
        .expect("fixture PDF renders");
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract(
            Arc::from(pdf.clone()),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .expect("fixture PDF extracts");
    let glyphs = document
        .items()
        .iter()
        .map(|glyph| {
            let text = match &glyph.text {
                DecodedText::Mapped(value) => json!({
                    "kind": "mapped",
                    "value": value,
                }),
                DecodedText::Unmapped {
                    font_hash,
                    glyph_id,
                } => json!({
                    "kind": "unmapped",
                    "font_identity_sha256": hex_digest(&font_hash.0),
                    "glyph_id": glyph_id,
                }),
            };
            json!({
                "text": text,
                "page": glyph.page.0,
                "render_order": glyph.render_order,
                "bbox": {
                    "min": { "x": glyph.bbox.min.x, "y": glyph.bbox.min.y },
                    "max": { "x": glyph.bbox.max.x, "y": glyph.bbox.max.y },
                },
                "baseline": { "x": glyph.baseline.x, "y": glyph.baseline.y },
                "direction": { "x": glyph.direction.x, "y": glyph.direction.y },
            })
        })
        .collect::<Vec<_>>();
    let oracle = json!({
        "schema_version": 1,
        "producer": {
            "name": "fixture-position-extractor",
            "version": "1.0.0",
            "parser_family": "fixture-parser",
        },
        "input_sha256": hex_digest(&Sha256::digest(&pdf)),
        "glyphs": glyphs,
    });
    (pdf, oracle)
}

fn run_command(
    inputs: &TempInputs,
    tolerance: f64,
    mismatch_svg: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pdfbench"));
    command
        .arg("extraction-conformance")
        .arg(&inputs.pdf)
        .arg("--oracle")
        .arg(&inputs.oracle)
        .arg("--geometry-tolerance")
        .arg(tolerance.to_string());
    if let Some(path) = mismatch_svg {
        command.arg("--mismatch-svg").arg(path);
    }
    command.output().expect("pdfbench runs")
}

struct TempInputs {
    pdf: PathBuf,
    oracle: PathBuf,
}

impl TempInputs {
    fn new(name: &str, pdf: &[u8], oracle: &Value) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after Unix epoch")
            .as_nanos();
        let stem = format!("pdfbench-extraction-{name}-{}-{nonce}", std::process::id());
        let pdf_path = std::env::temp_dir().join(format!("{stem}.pdf"));
        let oracle_path = std::env::temp_dir().join(format!("{stem}.json"));
        fs::write(&pdf_path, pdf).expect("fixture PDF writes");
        fs::write(
            &oracle_path,
            serde_json::to_vec(oracle).expect("fixture oracle serializes"),
        )
        .expect("fixture oracle writes");
        Self {
            pdf: pdf_path,
            oracle: oracle_path,
        }
    }

    fn mismatch_svg_path(&self) -> PathBuf {
        self.pdf.with_extension("mismatch.svg")
    }
}

impl Drop for TempInputs {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.mismatch_svg_path());
        let _ = fs::remove_file(&self.pdf);
        let _ = fs::remove_file(&self.oracle);
    }
}
