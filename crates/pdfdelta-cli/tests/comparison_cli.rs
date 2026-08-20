use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use lopdf::{Document, Object, Stream, dictionary};
use serde_json::Value;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-cli-comparison-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory should be created");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn identical_documents_exit_zero() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &[]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("Content changes:          0"));
}

#[test]
fn inspect_without_flags_prints_backend_summary() {
    let directory = TestDirectory::new();
    let document = directory.join("document.pdf");
    write_pdf(&document, &["A generic paragraph remains stable"]);

    let output = inspect(&document, &[]);
    let report = stdout(&output);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(report.contains("backend: lopdf"), "{report}");
    assert!(report.contains("pdf-version: 1.7"), "{report}");
    assert!(report.contains("pages: 1"), "{report}");
    assert!(!report.contains("glyphs:"), "{report}");
}

#[test]
fn replacement_exits_one() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("replacement.json");
    write_pdf(
        &old,
        &[
            "Opening paragraph establishes context",
            "Release 10 remains available",
            "Closing paragraph confirms context",
        ],
    );
    write_pdf(
        &new,
        &[
            "Opening paragraph establishes context",
            "Release 20 remains available",
            "Closing paragraph confirms context",
        ],
    );

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_complete_json_report(&report, 1, Some("replacement"));
}

#[test]
fn line_wrap_only_exits_zero() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("line-wrap.json");
    write_positioned_pdf_pages(&old, &[&["A simple release note remains stable"]], 12);
    write_positioned_pdf_pages(&new, &[&["A simple release note", "remains stable"]], 12);

    let glyph_output = inspect(&old, &["--glyphs"]);
    let glyph_report = stdout(&glyph_output);
    assert_eq!(
        glyph_output.status.code(),
        Some(0),
        "{}",
        stderr(&glyph_output)
    );
    assert!(glyph_report.contains("text=\"A\""), "{glyph_report}");
    assert!(!glyph_report.contains("text=\" \""), "{glyph_report}");

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_complete_json_report(&report, 0, None);
}

#[test]
fn page_break_only_exits_zero() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("page-break.json");
    let first = "First line keeps a steady cadence";
    let second = "Second line keeps a steady cadence";
    let third = "Third line keeps a steady cadence";
    let fourth = "Fourth line keeps a steady cadence";
    write_pdf_pages(&old, &[&[first, second, third, fourth]], 12);
    write_pdf_pages(&new, &[&[first, second], &[third, fourth]], 12);

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_complete_json_report(&report, 0, None);
}

#[test]
fn paragraph_insertion_exits_one_with_one_change() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("insertion.json");
    write_pdf(
        &old,
        &[
            "Opening paragraph remains stable",
            "Closing paragraph remains stable",
        ],
    );
    write_pdf(
        &new,
        &[
            "Opening paragraph remains stable",
            "Inserted paragraph contains generic text",
            "Closing paragraph remains stable",
        ],
    );

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_complete_json_report(&report, 1, Some("insertion"));
}

#[test]
fn paragraph_deletion_exits_one_with_one_change() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("deletion.json");
    write_pdf(
        &old,
        &[
            "Opening paragraph remains stable",
            "Removed paragraph contains generic text",
            "Closing paragraph remains stable",
        ],
    );
    write_pdf(
        &new,
        &[
            "Opening paragraph remains stable",
            "Closing paragraph remains stable",
        ],
    );

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_complete_json_report(&report, 1, Some("deletion"));
}

#[test]
fn writes_json_report_atomically() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("comparison.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &["--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let json = fs::read_to_string(report).expect("JSON report should be readable");
    assert!(json.contains("\"schema_version\": 4"));
    assert!(json.contains("\"content_changes\": 0"));
    assert_no_temporary_reports(&directory);
}

#[test]
fn preserves_existing_json_report_on_publish_collision() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("comparison.json");
    let existing_report = b"{\"preserved\":true}\n";
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    fs::write(&report, existing_report).expect("existing report should be written");

    let output = compare(&old, &new, &["--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("refusing to overwrite existing JSON report"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read(report).expect("existing report should remain readable"),
        existing_report
    );
    assert_no_temporary_reports(&directory);
}

#[test]
fn malformed_old_document_exits_two_with_context() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    fs::write(&old, b"not a PDF").expect("malformed fixture should be written");
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &[]);
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("old PDF"), "{error}");
    assert!(error.contains(path_text(&old)), "{error}");
}

#[test]
fn malformed_type0_extraction_reports_without_false_changes() {
    let directory = TestDirectory::new();
    let malformed = directory.join("malformed-type0.pdf");
    let complete = directory.join("complete.pdf");
    let report_path = directory.join("unresolved.json");
    write_type0_pdf(&malformed, &["Malformed page text is present"]);
    write_pdf(&complete, &["Complete page text remains visible"]);

    let default_output = compare(&malformed, &complete, &[]);
    let default_stderr = stderr(&default_output);
    let default_report = stdout(&default_output);

    assert_eq!(default_output.status.code(), Some(0), "{default_stderr}");
    assert!(default_stderr.contains("extraction issue for old PDF"));
    assert!(default_stderr.contains("kind=unresolved"));
    assert!(default_stderr.contains("Type0 font has no Encoding"));
    assert!(default_report.contains("Content changes:          0"));
    assert!(default_report.contains("Unresolved extraction:    1"));
    assert!(default_report.contains("Alignment coverage:       old=unknown, new=0.0%"));
    assert!(default_report.contains("Comparison coverage:      unknown"));
    assert!(
        default_report.contains("Extraction issue (side=old, kind=unresolved, scope=page, page=0)")
    );

    let strict_output = compare(
        &complete,
        &malformed,
        &["--strict", "--json", path_text(&report_path)],
    );
    let strict_stderr = stderr(&strict_output);

    assert_eq!(strict_output.status.code(), Some(3), "{strict_stderr}");
    assert!(strict_stderr.contains("extraction issue for new PDF"));
    let report: Value = serde_json::from_slice(
        &fs::read(report_path).expect("incomplete JSON report should be readable"),
    )
    .expect("incomplete JSON report should be valid");
    assert_eq!(report["schema_version"], 4);
    assert_eq!(report["summary"]["content_changes"], 0);
    assert_eq!(report["summary"]["comparison_complete"], false);
    assert_eq!(report["summary"]["unresolved_extraction_issues"], 1);
    assert_eq!(report["summary"]["old_alignment_coverage"]["ratio"], 0.0);
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["ratio"],
        Value::Null
    );
    assert_eq!(report["summary"]["comparison_coverage_ratio"], Value::Null);
    assert_eq!(
        report["changes"]
            .as_array()
            .expect("changes should be an array")
            .len(),
        0
    );
    assert_eq!(report["extraction"]["new_complete"], false);
    assert_eq!(report["extraction"]["issues"][0]["side"], "new");
    assert_eq!(report["extraction"]["issues"][0]["kind"], "unresolved");
    assert_eq!(report["extraction"]["issues"][0]["scope"], "page");
    assert_eq!(report["extraction"]["issues"][0]["page"], 0);
}

#[test]
fn strict_unresolved_comparison_exits_three() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["The archive contains 10 files"]);
    write_pdf(&new, &["The archive contains 20 files"]);

    let output = compare(&old, &new, &["--strict"]);

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stdout(&output).contains("Unresolved regions:       1"));
}

#[test]
fn rejects_json_output_that_aliases_an_input() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let original = fs::read(&old).expect("old fixture should be readable");

    let output = compare(&old, &new, &["--json", path_text(&old)]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("refusing JSON output"));
    assert_eq!(
        fs::read(&old).expect("old fixture should remain readable"),
        original
    );
    assert_no_temporary_reports(&directory);
}

fn compare(old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(old)
        .arg(new)
        .args(extra_arguments)
        .output()
        .expect("pdfdelta should run")
}

fn inspect(document: &Path, extra_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg("inspect")
        .arg(document)
        .args(extra_arguments)
        .output()
        .expect("pdfdelta inspect should run")
}

fn write_pdf(path: &Path, lines: &[&str]) {
    write_pdf_pages(path, &[lines], 30);
}

fn write_type0_pdf(path: &Path, lines: &[&str]) {
    write_pdf_pages_with_font(path, &[lines], 30, "Type0", TextEncoding::Literal);
}

fn write_pdf_pages(path: &Path, pages_content: &[&[&str]], line_gap: i64) {
    write_pdf_pages_with_font(
        path,
        pages_content,
        line_gap,
        "Type1",
        TextEncoding::Literal,
    );
}

fn write_positioned_pdf_pages(path: &Path, pages_content: &[&[&str]], line_gap: i64) {
    write_pdf_pages_with_font(
        path,
        pages_content,
        line_gap,
        "Type1",
        TextEncoding::PositionedWords,
    );
}

#[derive(Clone, Copy)]
enum TextEncoding {
    Literal,
    PositionedWords,
}

fn write_pdf_pages_with_font(
    path: &Path,
    pages_content: &[&[&str]],
    line_gap: i64,
    font_subtype: &str,
    text_encoding: TextEncoding,
) {
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let widths = vec![Object::Integer(500); 256];
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => Object::Name(font_subtype.as_bytes().to_vec()),
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => widths,
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Helvetica",
            "Ascent" => 800,
            "Descent" => -200,
            "MissingWidth" => 500,
        },
    });
    let resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font },
    });
    let page_ids = pages_content
        .iter()
        .map(|lines| {
            let content = lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let y = 250_i64
                        - i64::try_from(index).expect("line index should fit in i64") * line_gap;
                    let text = match text_encoding {
                        TextEncoding::Literal => format!("({}) Tj", escape_pdf_literal(line)),
                        TextEncoding::PositionedWords => positioned_words(line),
                    };
                    format!("BT /F1 10 Tf 1 0 0 1 30 {y} Tm {text} ET\n")
                })
                .collect::<String>();
            let contents = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages,
                "Contents" => contents,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
            })
        })
        .collect::<Vec<_>>();
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.into_iter().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => i64::try_from(pages_content.len()).expect("page count should fit in i64"),
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
    });
    document.trailer.set("Root", catalog);
    document.save(path).expect("fixture PDF should serialize");
}

fn positioned_words(text: &str) -> String {
    let words = text
        .split_ascii_whitespace()
        .map(|word| format!("({})", escape_pdf_literal(word)))
        .collect::<Vec<_>>()
        .join(" -500 ");
    format!("[{words}] TJ")
}

fn assert_complete_json_report(
    report_path: &Path,
    expected_changes: usize,
    expected_kind: Option<&str>,
) {
    let json = fs::read_to_string(report_path).expect("JSON report should be readable");
    let report: Value = serde_json::from_str(&json).expect("JSON report should be valid");
    assert_eq!(report["schema_version"], 4, "{report:#}");
    let summary = &report["summary"];
    assert_eq!(
        summary["content_changes"].as_u64(),
        Some(u64::try_from(expected_changes).expect("expected change count should fit in u64")),
        "{report:#}"
    );
    assert_eq!(
        summary["unresolved_regions"].as_u64(),
        Some(0),
        "{report:#}"
    );
    assert_eq!(
        summary["comparison_complete"].as_bool(),
        Some(true),
        "{report:#}"
    );
    for coverage_name in ["old_alignment_coverage", "new_alignment_coverage"] {
        let coverage = &summary[coverage_name];
        assert_eq!(coverage["ratio"].as_f64(), Some(1.0), "{report:#}");
        let resolved_tokens = coverage["resolved_tokens"]
            .as_u64()
            .expect("resolved token count should be an integer");
        let total_tokens = coverage["total_tokens"]
            .as_u64()
            .expect("total token count should be an integer");
        assert_eq!(resolved_tokens, total_tokens, "{report:#}");
    }

    let unresolved = report["unresolved_regions"]
        .as_array()
        .expect("unresolved regions should be an array");
    assert!(unresolved.is_empty(), "{report:#}");
    let changes = report["changes"]
        .as_array()
        .expect("changes should be an array");
    assert_eq!(changes.len(), expected_changes, "{report:#}");
    if let Some(expected_kind) = expected_kind {
        assert_eq!(
            changes[0]["kind"].as_str(),
            Some(expected_kind),
            "{report:#}"
        );
    }
}

fn assert_no_temporary_reports(directory: &TestDirectory) {
    let temporary_reports = fs::read_dir(&directory.0)
        .expect("test directory should be readable")
        .map(|entry| {
            entry
                .expect("test directory entry should be readable")
                .file_name()
        })
        .filter(|name| {
            let name = name.to_string_lossy();
            name.contains(".pdfdelta-") && name.ends_with(".tmp")
        })
        .collect::<Vec<_>>();
    assert!(temporary_reports.is_empty(), "{temporary_reports:?}");
}

fn escape_pdf_literal(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("temporary test path should be UTF-8")
}
