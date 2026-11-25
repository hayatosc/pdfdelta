use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use lopdf::{
    Document, Object, Stream, dictionary,
    encryption::{EncryptionState, EncryptionVersion, Permissions},
};
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
    assert!(
        stdout(&output).contains("content changes: 0"),
        "{:#}",
        stdout(&output)
    );
    assert!(
        !stdout(&output).contains("\u{1b}["),
        "{:#}",
        stdout(&output)
    );
}

#[test]
fn comparison_limit_scale_is_available_and_never_lowers_defaults() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let scaled = compare(&old, &new, &["--limit-scale", "2"]);
    assert_eq!(scaled.status.code(), Some(0), "{}", stderr(&scaled));

    let lowered = compare(&old, &new, &["--limit-scale", "0.5"]);
    assert_eq!(lowered.status.code(), Some(2), "{}", stderr(&lowered));
    assert!(stderr(&lowered).contains("finite value >= 1.0"));

    let help = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg("--help")
        .output()
        .expect("pdfdelta help should run");
    assert_eq!(help.status.code(), Some(0), "{}", stderr(&help));
    assert!(stdout(&help).contains("--limit-scale <FACTOR>"));
}

#[test]
fn extraction_cache_produces_identical_reports_cold_and_warm() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
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
    let cache_dir = directory.join("cache");
    let baseline = directory.join("baseline.json");
    let cold = directory.join("cold.json");
    let warm = directory.join("warm.json");
    let warm_trace = directory.join("warm-trace.json");

    let baseline_output = compare(&old, &new, &["--json", path_text(&baseline)]);
    assert_eq!(
        baseline_output.status.code(),
        Some(1),
        "{}",
        stderr(&baseline_output)
    );
    let cold_output = compare(
        &old,
        &new,
        &[
            "--json",
            path_text(&cold),
            "--extraction-cache-dir",
            path_text(&cache_dir),
        ],
    );
    assert_eq!(
        cold_output.status.code(),
        Some(1),
        "{}",
        stderr(&cold_output)
    );
    let warm_output = compare(
        &old,
        &new,
        &[
            "--json",
            path_text(&warm),
            "--trace-json",
            path_text(&warm_trace),
            "--extraction-cache-dir",
            path_text(&cache_dir),
        ],
    );
    assert_eq!(
        warm_output.status.code(),
        Some(1),
        "{}",
        stderr(&warm_output)
    );

    // Comparison results must be identical with and without the cache.
    let baseline_report = read_json(&baseline);
    assert_eq!(read_json(&cold), baseline_report);
    assert_eq!(read_json(&warm), baseline_report);

    // The warm run records a flow trace beside the unchanged report.
    let trace = read_json(&warm_trace);
    assert_eq!(trace["trace_schema"], "pdfdelta-flow-trace/1");
    assert_eq!(trace["result"]["exit_code"], 1);
}

#[test]
fn extraction_cache_directory_cannot_alias_a_report_destination() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let report = directory.join("report.json");

    let output = compare(
        &old,
        &new,
        &[
            "--extraction-cache-dir",
            path_text(&report),
            "--json",
            path_text(&report),
        ],
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("extraction cache directory"), "{message}");
    assert!(
        !report.exists(),
        "the report destination must remain unused"
    );
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
fn inspect_with_svg_flag_renders_valid_svg_file() {
    let directory = TestDirectory::new();
    let document = directory.join("document.pdf");
    let svg_path = directory.join("overlay.svg");
    write_pdf(&document, &["Testing SVG glyph overlay rendering"]);

    let output = inspect(&document, &["--svg", path_text(&svg_path)]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(svg_path.exists(), "SVG overlay file should be created");
    let svg_content = fs::read_to_string(&svg_path).expect("svg content should be readable");
    assert!(svg_content.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg_content.contains("class=\"glyph-bbox\""));
    assert!(svg_content.contains("class=\"glyph-baseline\""));
    assert!(svg_content.contains("class=\"glyph-text\""));
    assert!(svg_content.contains("data-glyph-id="));
    assert!(svg_content.contains("data-cs-num="));
}

#[test]
fn inspect_svg_refuses_to_alias_or_overwrite_outputs() {
    let directory = TestDirectory::new();
    let document = directory.join("document.pdf");
    write_pdf(&document, &["Testing SVG glyph overlay rendering"]);
    let original = fs::read(&document).expect("input PDF should be readable");

    // The SVG destination must not refer to the inspected PDF.
    let aliased = inspect(&document, &["--svg", path_text(&document)]);
    assert_eq!(aliased.status.code(), Some(2), "{}", stderr(&aliased));
    assert!(
        stderr(&aliased).contains("refers to the inspected PDF"),
        "{}",
        stderr(&aliased)
    );
    assert_eq!(
        fs::read(&document).expect("input PDF should remain readable"),
        original,
        "refused SVG output must not modify the inspected PDF"
    );

    // Existing output files are never replaced.
    let svg_path = directory.join("overlay.svg");
    let first = inspect(&document, &["--svg", path_text(&svg_path)]);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let published = fs::read(&svg_path).expect("published SVG should be readable");
    let second = inspect(&document, &["--svg", path_text(&svg_path)]);
    assert_eq!(second.status.code(), Some(2), "{}", stderr(&second));
    assert!(
        stderr(&second).contains("already exists"),
        "{}",
        stderr(&second)
    );
    assert_eq!(
        fs::read(&svg_path).expect("published SVG should remain readable"),
        published,
        "a refused overwrite must leave the published SVG unchanged"
    );
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

    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "--strict",
            "-j",
            path_text(&report),
            "-q",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_json(&report);
    let change = single_flow_change(&json, "replacement");
    assert_eq!(change["old_span"]["text"], "10");
    assert_eq!(change["new_span"]["text"], "20");
    assert!(json["report"].get("assessment").is_none());
    assert!(json["report"].get("not_produced_by_engine").is_none());
}

#[test]
fn replacement_report_renders_a_flow_change_entry() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
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

    // Piped output defaults to `auto`, so no ANSI escapes may appear.
    let output = run_default_engine(&old, &new, &["--native-text-only"]);
    let report = stdout(&output);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(report.contains("content changes: 1"), "{report}");
    assert!(report.contains("1. replace old 1 -> new 1"), "{report}");
    assert!(report.contains("old: "), "{report}");
    assert!(report.contains("new: "), "{report}");
    assert!(!report.contains("\u{1b}["), "{report}");
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

    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "--strict",
            "-j",
            path_text(&report),
            "-q",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = read_json(&report);
    assert_eq!(json["report"]["engine"], "flow");
    assert_eq!(json["report"]["summary"]["content_changes"], 0);
    assert_eq!(json["report"]["changes"], Value::Array(Vec::new()));
}

#[test]
fn encrypted_comparison_accepts_password_files_without_leaking_secrets() {
    const SECRET: &str = "correct horse battery";
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    // Context paragraphs give the aligner exact anchors.
    write_encrypted_pdf(
        &old,
        &[
            "Opening paragraph establishes context",
            "Release 10 remains available",
            "Closing paragraph confirms context",
        ],
        SECRET,
    );
    write_encrypted_pdf(
        &new,
        &[
            "Opening paragraph establishes context",
            "Release 20 remains available",
            "Closing paragraph confirms context",
        ],
        SECRET,
    );

    // Both password files carry one trailing newline; the reader must strip
    // exactly that newline before handing the password to the backend.
    let old_secret = directory.join("old.secret");
    let new_secret = directory.join("new.secret");
    fs::write(&old_secret, format!("{SECRET}\n")).expect("old password file should be written");
    fs::write(&new_secret, format!("{SECRET}\n")).expect("new password file should be written");
    let report = directory.join("encrypted.json");
    let trace = directory.join("encrypted-trace.json");

    let output = compare(
        &old,
        &new,
        &[
            "--old-password-file",
            path_text(&old_secret),
            "--new-password-file",
            path_text(&new_secret),
            "--json",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(!stdout(&output).contains(SECRET));
    assert!(!stderr(&output).contains(SECRET));
    let json = read_json(&report);
    assert_eq!(json["report"]["engine"], "flow");
    assert_eq!(json["report"]["summary"]["content_changes"], 1);
    let trace_json = fs::read_to_string(&trace).expect("trace report should be readable");
    assert!(
        !trace_json.contains(SECRET),
        "the trace leaked the password"
    );
    assert_eq!(read_json(&trace)["result"]["exit_code"], 1);
}

#[test]
fn encrypted_comparison_reports_a_wrong_password_as_unsupported_without_leaking_it() {
    const SECRET: &str = "side-specific secret";
    const WRONG: &str = "a different password";
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_encrypted_pdf(
        &old,
        &[
            "Opening paragraph establishes context",
            "Release 10 remains available",
        ],
        SECRET,
    );
    write_encrypted_pdf(
        &new,
        &[
            "Opening paragraph establishes context",
            "Release 20 remains available",
        ],
        SECRET,
    );
    let old_secret = directory.join("old.secret");
    let new_secret = directory.join("new.secret");
    fs::write(&old_secret, format!("{SECRET}\n")).expect("old password file should be written");
    fs::write(&new_secret, format!("{WRONG}\n")).expect("new password file should be written");

    let arguments = [
        "--old-password-file",
        path_text(&old_secret),
        "--new-password-file",
        path_text(&new_secret),
    ];

    // A wrong password is a fatal parse error on the flow path. The trace
    // carries the typed unsupported class, not a wording-derived backend one.
    let trace = directory.join("wrong-password-trace.json");
    let mut traced_arguments = arguments.to_vec();
    traced_arguments.extend(["--trace-json", path_text(&trace)]);
    let default_output = compare(&old, &new, &traced_arguments);
    assert_eq!(
        default_output.status.code(),
        Some(2),
        "{}",
        stderr(&default_output)
    );
    let message = stderr(&default_output);
    assert!(message.contains("password"), "{message}");
    assert!(!message.contains(SECRET), "{message}");
    assert!(!message.contains(WRONG), "{message}");
    let trace = read_json(&trace);
    let failed = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["status"] == "failed")
        .expect("failed phase");
    assert_eq!(failed["error"]["kind"], "unsupported", "{trace}");

    let mut strict_arguments = arguments.to_vec();
    strict_arguments.push("--strict");
    let strict_output = compare(&old, &new, &strict_arguments);
    assert_eq!(strict_output.status.code(), Some(2));
}

#[test]
fn rejects_oversized_password_files() {
    const SECRET: &str = "small secret";
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_encrypted_pdf(&old, &["Release 10 remains available"], SECRET);
    write_encrypted_pdf(&new, &["Release 10 remains available"], SECRET);
    let oversized = directory.join("oversized.secret");
    fs::write(&oversized, "x".repeat(4_097)).expect("oversized file should be written");

    let output = compare(
        &old,
        &new,
        &[
            "--old-password-file",
            path_text(&oversized),
            "--new-password-file",
            path_text(&oversized),
        ],
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("password file"), "{message}");
}

#[test]
fn rejects_standard_input_password_files() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &["--old-password-file", "-"]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("standard input is reserved"), "{message}");
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

    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "--strict",
            "-j",
            path_text(&report),
            "-q",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = read_json(&report);
    assert_eq!(json["report"]["engine"], "flow");
    assert_eq!(json["report"]["summary"]["content_changes"], 0);
    assert_eq!(json["report"]["changes"], Value::Array(Vec::new()));
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

    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "--strict",
            "-j",
            path_text(&report),
            "-q",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_json(&report);
    let change = single_flow_change(&json, "insertion");
    assert_eq!(
        change["new_span"]["text"],
        "Inserted paragraph contains generic text"
    );
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

    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "--strict",
            "-j",
            path_text(&report),
            "-q",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_json(&report);
    let change = single_flow_change(&json, "deletion");
    assert_eq!(
        change["old_span"]["text"],
        "Removed paragraph contains generic text"
    );
}

#[test]
fn writes_json_report_atomically() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("comparison.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &["--json", path_text(&report), "-q"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let json = read_json(&report);
    assert_eq!(json["report"]["schema_version"], 12);
    assert_eq!(json["report"]["summary"]["content_changes"], 0);
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
        stderr(&output).contains("refusing to overwrite existing full flow JSON report"),
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
fn preserves_existing_text_report_on_publish_collision() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("comparison.txt");
    let existing_report = b"existing text report\n";
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    fs::write(&report, existing_report).expect("existing report should be written");

    let output = compare(&old, &new, &["--output", path_text(&report)]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("refusing to overwrite existing text report"),
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
fn rejects_trace_and_report_output_aliases_before_processing() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let output_path = directory.join("output.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(
        &old,
        &new,
        &[
            "--json",
            path_text(&output_path),
            "--trace-json",
            path_text(&output_path),
        ],
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("refusing trace output"));
    assert!(!output_path.exists());
    assert_no_temporary_reports(&directory);
}

#[test]
fn handles_a_missing_report_path_when_the_trace_already_exists() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("report.json");
    let trace = directory.join("trace.json");
    let existing_trace = b"{\"preserved\":true}\n";
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    fs::write(&trace, existing_trace).expect("existing trace should be written");

    let output = compare(
        &old,
        &new,
        &[
            "--json",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("refusing to overwrite existing flow trace report"));
    assert!(!error.contains("cannot inspect input path"));
    assert!(!report.exists());
    assert_eq!(
        fs::read(trace).expect("existing trace should remain readable"),
        existing_trace
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
    assert!(error.contains("failed to parse"), "{error}");
    assert!(error.contains(path_text(&old)), "{error}");
}

#[test]
fn rejects_an_empty_input_document_as_malformed() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    fs::write(&old, b"").expect("empty fixture should be written");
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &[]);
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("failed to parse"), "{error}");
    assert!(error.contains(path_text(&old)), "{error}");
}

#[test]
fn malformed_type0_extraction_reports_without_false_changes() {
    let directory = TestDirectory::new();
    let malformed = directory.join("malformed-type0.pdf");
    let complete = directory.join("complete.pdf");
    write_type0_pdf(&malformed, &["Malformed page text is present"]);
    write_pdf(&complete, &["Complete page text remains visible"]);

    let default_output = compare(&malformed, &complete, &["-q"]);
    assert_eq!(
        default_output.status.code(),
        Some(3),
        "{}",
        stderr(&default_output)
    );

    let report_path = directory.join("unresolved.json");
    let strict_output = compare(
        &complete,
        &malformed,
        &["--strict", "-q", "-j", path_text(&report_path)],
    );
    assert_eq!(
        strict_output.status.code(),
        Some(3),
        "{}",
        stderr(&strict_output)
    );
    let report = read_json(&report_path);
    let report = &report["report"];
    assert_eq!(report["schema_version"], 12);
    assert_eq!(report["summary"]["content_changes"], 1);
    assert_eq!(report["summary"]["comparison_complete"], false);
    assert_eq!(report["summary"]["unresolved_extraction_issues"], 1);
    assert_eq!(report["extraction"]["new_complete"], false);
    let issue = &report["extraction"]["issues"][0];
    assert_eq!(issue["side"], "new");
    assert_eq!(issue["kind"], "unresolved");
    assert!(
        issue["message"]
            .as_str()
            .is_some_and(|description| description.contains("Type0 font has no Encoding")),
        "{issue:#}"
    );
}

#[test]
fn localized_page_tree_gap_preserves_known_change_and_reports_boundary() {
    let directory = TestDirectory::new();
    let old = directory.join("old-gap.pdf");
    let new = directory.join("new-complete.pdf");
    let report_path = directory.join("page-gap.json");
    let old_pages: &[&[&str]] = &[
        &["Opening anchor remains exactly stable"],
        &["Old missing branch content"],
        &["Boundary anchor remains exactly stable"],
        &["Release 10 remains available"],
        &["Closing anchor remains exactly stable"],
    ];
    let new_pages: &[&[&str]] = &[
        &["Opening anchor remains exactly stable"],
        &["New counterpart branch content"],
        &["Boundary anchor remains exactly stable"],
        &["Release 20 remains available"],
        &["Closing anchor remains exactly stable"],
    ];
    write_pdf_with_missing_page_tree_child(&old, old_pages, 1);
    write_pdf_pages(&new, new_pages, 30);

    let default_output = compare(&old, &new, &["-q"]);
    assert_eq!(
        default_output.status.code(),
        Some(3),
        "{}",
        stderr(&default_output)
    );

    let strict_output = compare(
        &old,
        &new,
        &["--strict", "-q", "-j", path_text(&report_path)],
    );
    assert_eq!(
        strict_output.status.code(),
        Some(3),
        "{}",
        stderr(&strict_output)
    );
    let report = read_json(&report_path);
    let report = &report["report"];
    assert_eq!(report["schema_version"], 12);
    assert_eq!(report["summary"]["content_changes"], 2);
    assert_eq!(report["summary"]["comparison_complete"], false);
    assert_eq!(report["extraction"]["old_complete"], false);
    assert!(
        report["extraction"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| issue["side"] == "old" && issue["kind"] == "unresolved"),
        "{report:#}"
    );
    assert!(
        report["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .any(|change| change["kind"] == "insertion"),
        "{report:#}"
    );
}

#[test]
fn rejects_json_output_that_aliases_an_input() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let trace_path = directory.join("trace.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let original = fs::read(&old).expect("old fixture should be readable");

    let output = compare(
        &old,
        &new,
        &[
            "--json",
            path_text(&old),
            "--trace-json",
            path_text(&trace_path),
        ],
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("refusing JSON output"));
    assert_eq!(
        fs::read(&old).expect("old fixture should remain readable"),
        original
    );
    assert!(!trace_path.exists(), "validation failure writes no trace");
    assert_no_temporary_reports(&directory);
}

#[test]
fn rejects_lexical_aliases_of_a_missing_report_leaf() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    // `result.json` and `./result.json` name the same future file even though
    // the leaf does not exist yet, so validation must reject both spellings.
    let output = compare_in(
        directory.0.as_path(),
        &old,
        &new,
        &["--json", "result.json", "--trace-json", "./result.json"],
    );
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("refusing trace output"), "{error}");
    assert!(!directory.join("result.json").exists());
    assert_no_temporary_reports(&directory);
}

#[test]
fn rejects_lexical_aliases_of_a_missing_leaf_in_an_existing_subdirectory() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    fs::create_dir(directory.join("out")).expect("subdirectory should be created");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare_in(
        directory.0.as_path(),
        &old,
        &new,
        &[
            "--json",
            "./out/result.json",
            "--trace-json",
            "out/result.json",
        ],
    );
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("refusing trace output"), "{error}");
    assert!(!directory.join("out").join("result.json").exists());
    assert_no_temporary_reports(&directory);
}

#[test]
fn rejects_absolute_and_relative_aliases_of_a_missing_report() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let absolute = directory.join("result.json");

    let output = compare_in(
        directory.0.as_path(),
        &old,
        &new,
        &[
            "--json",
            path_text(&absolute),
            "--trace-json",
            "result.json",
        ],
    );
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("refusing trace output"), "{error}");
    assert!(!absolute.exists());
    assert_no_temporary_reports(&directory);
}

#[test]
fn accepts_genuinely_distinct_missing_output_paths() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare_in(
        directory.0.as_path(),
        &old,
        &new,
        &["--json", "result.json", "--trace-json", "trace.json"],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(directory.join("result.json").exists());
    assert!(directory.join("trace.json").exists());
    assert_no_temporary_reports(&directory);
}

#[cfg(unix)]
#[test]
fn still_rejects_existing_symlink_output_aliases() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("report.json");
    let trace = directory.join("trace.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    fs::write(&report, b"{\"existing\":true}\n").expect("existing report should be written");
    symlink(&report, &trace).expect("trace symlink should be created");

    let output = compare(
        &old,
        &new,
        &[
            "--json",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    let error = stderr(&output);

    assert_eq!(output.status.code(), Some(2), "{error}");
    assert!(error.contains("refusing trace output"), "{error}");
    assert_eq!(
        fs::read(&report).expect("existing report should remain readable"),
        b"{\"existing\":true}\n"
    );
    assert_no_temporary_reports(&directory);
}

#[test]
fn completions_subcommand_generates_shell_scripts() {
    let output_bash = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["completions", "bash"])
        .output()
        .expect("pdfdelta completions bash should run");
    assert_eq!(output_bash.status.code(), Some(0));
    let bash_script = stdout(&output_bash);
    assert!(bash_script.contains("pdfdelta"), "{bash_script}");

    let output_zsh = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["completions", "zsh"])
        .output()
        .expect("pdfdelta completions zsh should run");
    assert_eq!(output_zsh.status.code(), Some(0));
    let zsh_script = stdout(&output_zsh);
    assert!(zsh_script.contains("pdfdelta"), "{zsh_script}");

    let output_fish = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["completions", "fish"])
        .output()
        .expect("pdfdelta completions fish should run");
    assert_eq!(output_fish.status.code(), Some(0));
    let fish_script = stdout(&output_fish);
    assert!(fish_script.contains("pdfdelta"), "{fish_script}");
}

#[test]
fn quiet_flag_suppresses_stdout_report() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
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

    let output = compare(&old, &new, &["--quiet"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).is_empty(),
        "stdout should be suppressed by --quiet"
    );

    let output_short = compare(&old, &new, &["-q"]);
    assert_eq!(output_short.status.code(), Some(1));
    assert!(
        stdout(&output_short).is_empty(),
        "stdout should be suppressed by -q"
    );
}

#[test]
fn output_flag_writes_text_report_to_file() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report_file = directory.join("report.txt");
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

    let output = compare(&old, &new, &["--output", path_text(&report_file)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).is_empty(),
        "stdout should be empty when --output is provided"
    );

    let report_content = fs::read_to_string(&report_file).expect("report file should exist");
    assert!(
        report_content.contains("content changes: 1"),
        "{report_content}"
    );
    assert!(
        report_content.contains("1. replace old 1 -> new 1"),
        "{report_content}"
    );
    assert!(report_content.contains("old: "), "{report_content}");
    assert!(report_content.contains("new: "), "{report_content}");
}

#[test]
fn inspect_objects_prints_structural_summary() {
    let directory = TestDirectory::new();
    let document = directory.join("document.pdf");
    write_pdf(&document, &["Testing object inspection"]);

    let output = inspect(&document, &["--objects"]);
    assert_eq!(output.status.code(), Some(0));
    let report = stdout(&output);
    assert!(report.contains("trailer: <<"), "{report}");
    assert!(report.contains("pages: 1"), "{report}");
    assert!(report.contains("page 1: object"), "{report}");
    assert!(report.contains("/Type /Page"), "{report}");
}

#[test]
fn stdin_collision_fails_with_informative_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["-", "-"])
        .output()
        .expect("pdfdelta should run");
    assert_eq!(output.status.code(), Some(2));
    let error = stderr(&output);
    assert!(
        error.contains("cannot read both OLD_PDF and NEW_PDF from standard input"),
        "{error}"
    );
}

#[test]
fn proof_engine_selection_exits_two_with_replacements() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["old.pdf", "new.pdf", "--engine", "proof"])
        .output()
        .expect("pdfdelta should run");
    assert_eq!(output.status.code(), Some(2));
    let error = stderr(&output);
    assert!(error.contains("proof engine was removed"), "{error}");
    for replacement in [
        "--trace-json",
        "--compact-json",
        "--full-json",
        "--review",
        "--agent-review",
        "pdfdelta flow",
    ] {
        assert!(
            error.contains(replacement),
            "missing {replacement}: {error}"
        );
    }
}

#[test]
fn stdin_supports_reading_old_pdf() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let old_bytes = fs::read(&old).expect("old PDF bytes should be readable");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg("--native-text-only")
        .arg("-")
        .arg(&new)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("pdfdelta should spawn");

    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .expect("stdin should be open")
        .write_all(&old_bytes)
        .expect("stdin write should succeed");

    let output = child.wait_with_output().expect("pdfdelta should finish");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("content changes: 0"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn output_aliasing_with_input_is_rejected() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &["--output", path_text(&old)]);
    assert_eq!(output.status.code(), Some(2));
    let error = stderr(&output);
    assert!(error.contains("refusing text report output"), "{error}");
}

#[test]
fn externally_rendered_typst_case3_revision_pair_reports_exact_replacement() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case3-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");
    assert!(
        old_pdf.exists() && new_pdf.exists(),
        "vendored fixture must exist"
    );

    let directory = TestDirectory::new();
    let report = directory.join("report.json");
    let output = run_default_engine(
        &old_pdf,
        &new_pdf,
        &["--native-text-only", "-j", path_text(&report), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_json(&report);
    let change = single_flow_change(&json, "replacement");
    assert_eq!(change["old_span"]["text"], "10");
    assert_eq!(change["new_span"]["text"], "20");

    let text = run_default_engine(&old_pdf, &new_pdf, &["--native-text-only"]);
    assert_eq!(text.status.code(), Some(1), "{}", stderr(&text));
    assert!(
        stdout(&text).contains("content changes: 1"),
        "{}",
        stdout(&text)
    );
}

#[test]
fn externally_rendered_typst_japanese_revision_pair_reports_exact_replacement() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");
    assert!(
        old_pdf.exists() && new_pdf.exists(),
        "vendored fixture must exist"
    );

    let directory = TestDirectory::new();
    let report = directory.join("report.json");
    let output = run_default_engine(
        &old_pdf,
        &new_pdf,
        &["--native-text-only", "-j", path_text(&report), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_json(&report);
    let change = single_flow_change(&json, "replacement");
    assert_eq!(change["old_span"]["text"], "10");
    assert_eq!(change["new_span"]["text"], "20");
}

#[test]
fn externally_rendered_typst_japanese_case1_wrap_revision_pair_reports_zero_content_changes() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case1-japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");
    assert!(
        old_pdf.exists() && new_pdf.exists(),
        "vendored fixture must exist"
    );

    let directory = TestDirectory::new();
    let report = directory.join("report.json");
    let output = run_default_engine(
        &old_pdf,
        &new_pdf,
        &["--native-text-only", "-j", path_text(&report), "-q"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = read_json(&report);
    assert_eq!(json["report"]["summary"]["content_changes"], 0);
    assert_eq!(json["report"]["changes"], Value::Array(Vec::new()));
}

#[test]
fn externally_rendered_typst_japanese_case2_pagebreak_revision_pair_reports_zero_content_changes() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case2-japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");
    assert!(
        old_pdf.exists() && new_pdf.exists(),
        "vendored fixture must exist"
    );

    let directory = TestDirectory::new();
    let report = directory.join("report.json");
    let output = run_default_engine(
        &old_pdf,
        &new_pdf,
        &["--native-text-only", "-j", path_text(&report), "-q"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = read_json(&report);
    assert_eq!(json["report"]["summary"]["content_changes"], 0);
    assert_eq!(json["report"]["changes"], Value::Array(Vec::new()));
}

#[test]
fn externally_rendered_typst_japanese_case4_case5_revision_pair_reports_exact_insertion_and_deletion()
 {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/external/case4-case5-japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");
    assert!(
        old_pdf.exists() && new_pdf.exists(),
        "vendored fixture must exist"
    );
    let inserted = "運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。";

    let directory = TestDirectory::new();
    let forward = directory.join("forward.json");
    let output = run_default_engine(
        &old_pdf,
        &new_pdf,
        &["--native-text-only", "-j", path_text(&forward), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let forward = read_json(&forward);
    let change = single_flow_change(&forward, "insertion");
    assert!(change["old_span"].is_null());
    assert_eq!(
        change["new_span"]["text"]
            .as_str()
            .expect("inserted text")
            .replace(' ', ""),
        inserted
    );

    let reverse = directory.join("reverse.json");
    let output = run_default_engine(
        &new_pdf,
        &old_pdf,
        &["--native-text-only", "-j", path_text(&reverse), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let reverse = read_json(&reverse);
    let change = single_flow_change(&reverse, "deletion");
    assert_eq!(
        change["old_span"]["text"]
            .as_str()
            .expect("deleted text")
            .replace(' ', ""),
        inserted
    );
    assert!(change["new_span"].is_null());
}

#[test]
fn presentation_channel_reports_unsupported_coverage_without_changes() {
    let directory = TestDirectory::new();
    let old = directory.join("old-presentation.pdf");
    let new = directory.join("new-presentation.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let report_path = directory.join("presentation.json");

    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "presentation"])
        .arg(&old)
        .arg(&new)
        .arg("--json")
        .arg(&report_path)
        .output()
        .expect("presentation comparison");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report_path);
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["typed_changes"], 0);
    let coverage = report["coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .find(|coverage| coverage["channel"] == "presentation")
        .expect("presentation coverage");
    assert_eq!(coverage["complete"], false);
    assert!(
        report["old"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| issue["channel"] == "presentation" && issue["kind"] == "unsupported"),
        "{report}"
    );
}

#[test]
fn external_font_identity_flags_are_validated_end_to_end() {
    let directory = TestDirectory::new();
    let old = directory.join("old-identity.pdf");
    let new = directory.join("new-identity.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let accepted = compare(
        &old,
        &new,
        &[
            "--old-font-identity",
            "Helvetica=windows-v1",
            "--new-font-identity",
            "Helvetica=windows-v1",
        ],
    );
    assert_eq!(accepted.status.code(), Some(0), "{}", stderr(&accepted));

    let malformed = compare(&old, &new, &["--old-font-identity", "Helvetica"]);
    assert_eq!(malformed.status.code(), Some(2), "{}", stderr(&malformed));
    assert!(
        stderr(&malformed).contains("BASE_FONT=IDENTITY"),
        "{}",
        stderr(&malformed)
    );
}

#[test]
fn default_evidence_path_accepts_a_large_font_identity_table() {
    let directory = TestDirectory::new();
    let old = directory.join("old-identity-table.pdf");
    let new = directory.join("new-identity-table.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let report_path = directory.join("identity-table.json");
    let identity = "a".repeat(4_096);
    let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
    command.args(["--channels", "text"]).arg(&old).arg(&new);
    for index in 0..64 {
        let value = format!("Font{index}={identity}");
        command.arg("--old-font-identity").arg(&value);
        command.arg("--new-font-identity").arg(&value);
    }
    command.arg("--json").arg(&report_path);

    let output = command.output().expect("large identity table comparison");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = read_json(&report_path);
    assert_eq!(report["old"]["native_glyphs"], 34, "{report}");
    assert_eq!(report["new"]["native_glyphs"], 34, "{report}");
}

fn compare(old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
    if extra_arguments
        .iter()
        .any(|arg| matches!(*arg, "--json" | "-j"))
    {
        command.arg("--full-json");
    }
    command
        .arg("--native-text-only")
        .arg(old)
        .arg(new)
        .args(extra_arguments)
        .output()
        .expect("pdfdelta should run")
}

#[test]
fn default_contract_retains_image_evidence_without_claiming_complete_coverage() {
    let directory = TestDirectory::new();
    let input = directory.join("image.pdf");
    let report = directory.join("document.json");
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let image = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB",
        },
        vec![255, 0, 0],
    ));
    let contents = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 72 0 0 72 0 0 cm /I Do Q".to_vec(),
    ));
    let page = pdf.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(72), Object::from(72)],
        "Resources" => dictionary! { "XObject" => dictionary! { "I" => image } }, "Contents" => contents,
    });
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
        }),
    );
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    pdf.save(&input).expect("image-only fixture");
    for channels in ["text", "text,relations"] {
        let report = directory.join(&format!("{channels}.json"));
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .args(["--channels", channels])
            .arg(&input)
            .arg(&input)
            .arg("--json")
            .arg(&report)
            .output()
            .expect("selected-text comparison CLI");
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let selected: Value =
            serde_json::from_slice(&fs::read(&report).expect("selected-text report"))
                .expect("JSON report");
        assert_eq!(selected["schema_version"], 3);
        assert_eq!(selected["comparison_complete"], false);
        let text = selected["coverage"]
            .as_array()
            .expect("coverage")
            .iter()
            .find(|coverage| coverage["channel"] == "text")
            .expect("text coverage");
        assert_eq!(text["old_inventory_complete"], false);
        assert_eq!(text["new_inventory_complete"], false);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&input)
        .arg(&input)
        .arg("--json")
        .arg(&report)
        .output()
        .expect("document comparison CLI");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report: Value =
        serde_json::from_slice(&fs::read(report).expect("document report")).expect("JSON report");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["old"]["native_glyphs"], 0);
    assert_eq!(report["coverage"].as_array().expect("channels").len(), 4);
    assert_eq!(report["coverage"][1]["old_inventory_complete"], false);
    assert!(
        !report["old"]["issues"]
            .as_array()
            .expect("issues")
            .is_empty()
    );
    #[cfg(target_os = "linux")]
    {
        assert_eq!(report["old"]["rendered_regions"], 1);
        assert_eq!(report["old"]["issues"][0]["kind"], "unresolved");
        assert_eq!(report["inferred_changes"], 0);

        let changed = directory.join("changed-image.pdf");
        let changed_report = directory.join("changed-image.json");
        let review = directory.join("image-review");
        pdf.get_object_mut(image)
            .expect("image object")
            .as_stream_mut()
            .expect("image stream")
            .content = vec![0, 0, 255];
        pdf.save(&changed).expect("changed image fixture");
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .args(["--channels", "visual"])
            .arg(&input)
            .arg(&changed)
            .arg("--json")
            .arg(&changed_report)
            .arg("--review")
            .arg(&review)
            .output()
            .expect("visual comparison");
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let report: Value =
            serde_json::from_slice(&fs::read(changed_report).expect("visual report"))
                .expect("JSON report");
        assert_eq!(report["old"]["native_glyphs"], 0);
        assert_eq!(report["inferred_changes"], 1, "{report}");
        assert_eq!(report["typed_changes"], 0);
        let pair = &report["image_diff"]["comparison"]["changes"][0];
        assert_eq!(pair["kind"], "changed");
        assert_eq!(report["image_diff"]["old"]["images"][0]["width"], 1);
        assert_ne!(
            report["image_diff"]["old"]["images"][0]["sha256"],
            report["image_diff"]["new"]["images"][0]["sha256"]
        );
        assert!(pair.get("pixel_mask").is_none());
        let html = fs::read_to_string(review.join("index.html")).expect("visual review");
        assert!(html.contains("category category-c"));
        assert!(!html.contains("category category-a"));
        assert_review_pointers(&html, &report);
        for (side, color) in [("old", [255, 0, 0]), ("new", [0, 0, 255])] {
            let png = fs::read(review.join(format!("{side}-region-0.png"))).expect("page preview");
            let mut decoder = png::Decoder::new(std::io::Cursor::new(png))
                .read_info()
                .expect("PNG header");
            let mut pixels = vec![
                0;
                decoder
                    .output_buffer_size()
                    .expect("bounded PNG dimensions")
            ];
            let info = decoder.next_frame(&mut pixels).expect("PNG samples");
            assert_eq!((info.width, info.height), (72, 72));
            assert_eq!(info.color_type, png::ColorType::Rgb);
            assert!(
                pixels[..info.buffer_size()]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .all(|pixel| *pixel == color)
            );
        }

        // Equal dimensions and page counts cannot substitute for source identity.
        let mut child = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .args(["render-page", "0", "1", "72", "72", "4294967295", "0"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("render process");
        std::io::Write::write_all(
            &mut child.stdin.take().expect("piped input"),
            &fs::read(&input).expect("image fixture"),
        )
        .expect("render input");
        let output = child.wait_with_output().expect("render result");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    #[cfg(not(target_os = "linux"))]
    assert_eq!(report["old"]["rendered_regions"], 0);
}

#[test]
#[cfg(target_os = "linux")]
fn render_process_rejects_oversized_output_before_reading_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["render-page", "0", "1", "65535", "65535", "1", "0"])
        .output()
        .expect("render process");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn non_utf8_cache_directories_do_not_disable_native_evidence() {
    use std::os::unix::ffi::OsStringExt;

    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let cache_dir = directory
        .0
        .join(std::ffi::OsString::from_vec(b"cache-\xff".to_vec()));
    fs::create_dir_all(&cache_dir).expect("non-UTF-8 cache directory");
    let report_path = directory.join("non-utf8-cache.json");

    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "text"])
        .arg(&old)
        .arg(&new)
        .arg("--extraction-cache-dir")
        .arg(&cache_dir)
        .arg("--json")
        .arg(&report_path)
        .output()
        .expect("non-UTF-8 cache comparison");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = read_json(&report_path);
    assert_eq!(report["old"]["native_glyphs"], 34, "{report}");
    assert_eq!(report["new"]["native_glyphs"], 34, "{report}");
}

#[test]
fn default_evidence_path_publishes_json_text_and_trace_reports() {
    let directory = TestDirectory::new();
    let old = directory.join("old-body.pdf");
    let new = directory.join("new-body.pdf");
    write_pdf(&old, &["The fee is 100 dollars."]);
    write_pdf(&new, &["The fee is 200 dollars."]);
    let json = directory.join("report.json");
    let text = directory.join("report.txt");
    let trace = directory.join("trace.json");
    let arguments = |command: &mut Command| {
        command
            .arg(&old)
            .arg(&new)
            .arg("--json")
            .arg(&json)
            .arg("--output")
            .arg(&text)
            .arg("--trace-json")
            .arg(&trace);
    };

    let output = {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
        arguments(&mut command);
        command.output().expect("default evidence comparison")
    };
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(json.exists() && text.exists() && trace.exists());
    let trace_json = read_json(&trace);
    assert_eq!(trace_json["trace_schema"], "pdfdelta-flow-trace/1");
    assert_eq!(trace_json["engine"], "flow");

    // Report publication refuses to replace an existing destination.
    let repeated = {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
        arguments(&mut command);
        command
            .output()
            .expect("repeated default evidence comparison")
    };
    assert_eq!(repeated.status.code(), Some(2), "{}", stderr(&repeated));
    assert!(
        stderr(&repeated).contains("refusing to overwrite existing"),
        "{}",
        stderr(&repeated)
    );
}

#[test]
fn form_values_follow_field_names_when_values_and_field_order_are_swapped() {
    let directory = TestDirectory::new();
    let old = directory.join("old-forms.pdf");
    let new = directory.join("new-forms.pdf");
    let report = directory.join("forms.json");
    let review = directory.join("forms-review");
    fn fields(path: &Path, values: &[(&str, &str)]) {
        write_pdf(
            path,
            &["Retained background text is outside the selected form channel"],
        );
        let mut pdf = Document::load(path).expect("background text fixture");
        let fields: Vec<_> = values.iter().map(|(name, value)| Object::Reference(pdf.add_object(dictionary! {
            "FT" => "Tx", "T" => Object::string_literal(*name), "V" => Object::string_literal(*value),
        }))).collect();
        let root = pdf
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .expect("catalog reference");
        pdf.get_object_mut(root)
            .and_then(Object::as_dict_mut)
            .expect("catalog")
            .set("AcroForm", dictionary! { "Fields" => fields });
        pdf.save(path).expect("stored fields without widgets");
    }
    fields(&old, &[("revenue", "100"), ("profit", "20")]);
    fields(&new, &[("profit", "100"), ("revenue", "20")]);
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old)
        .arg(&new)
        .args(["--channels", "forms", "--json"])
        .arg(&report)
        .arg("--review")
        .arg(&review)
        .output()
        .expect("typed form comparison");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Field \"revenue\""), "{text}");
    assert!(text.contains("Field \"profit\""), "{text}");
    assert!(text.contains("  - \"100\"\n  + \"20\""), "{text}");
    assert!(text.contains("  - \"20\"\n  + \"100\""), "{text}");
    assert!(text.contains("Mandatory changed positions:"), "{text}");
    let report: Value =
        serde_json::from_slice(&fs::read(report).expect("report")).expect("typed report");
    assert_eq!(report["comparison_complete"], true);
    assert_eq!(report["typed_changes"], 2);
    assert!(
        report["old"]["native_glyphs"]
            .as_u64()
            .expect("retained glyph count")
            > 0
    );
    assert_eq!(
        report["comparison"]["scopes"][0]["result"]["candidates"]["examined_pairs"],
        2
    );
    assert_eq!(
        report["comparison"]["scopes"][0]["result"]["matching"]["channels"]["text"],
        false
    );
    let pairs = report["comparison"]["scopes"][0]["result"]["comparisons"]
        .as_array()
        .expect("paired fields");
    assert_eq!(pairs[0]["operation"]["old"]["value"], "100");
    assert_eq!(pairs[0]["operation"]["new"]["value"], "20");
    assert_eq!(pairs[1]["operation"]["old"]["value"], "20");
    assert_eq!(pairs[1]["operation"]["new"]["value"], "100");
    let html = fs::read_to_string(review.join("index.html")).expect("field review");
    assert_eq!(html.matches("category category-a").count(), 2);
    assert!(html.contains("no retained page locator"));
    assert_review_pointers(&html, &report);
}

/// Writes a one-page PDF with caller-supplied text and stored form fields.
fn stored_fields_fixture_with_text(path: &Path, lines: &[&str], values: &[(&str, &str)]) {
    write_pdf(path, lines);
    let mut pdf = Document::load(path).expect("background text fixture");
    let fields: Vec<_> = values
        .iter()
        .map(|(name, value)| {
            Object::Reference(pdf.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal(*name), "V" => Object::string_literal(*value),
    }))
        })
        .collect();
    let root = pdf
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog reference");
    pdf.get_object_mut(root)
        .and_then(Object::as_dict_mut)
        .expect("catalog")
        .set("AcroForm", dictionary! { "Fields" => fields });
    pdf.save(path).expect("stored fields without widgets");
}

fn stored_fields_fixture(path: &Path, values: &[(&str, &str)]) {
    stored_fields_fixture_with_text(
        path,
        &["Retained background text is outside the selected form channel"],
        values,
    );
}

/// Adds an XFA component so the forms channel reports its explicit
/// unexamined-form failure without changing the stored field inventory.
fn add_xfa_component(path: &Path) {
    let mut pdf = Document::load(path).expect("load XFA fixture");
    let root = pdf
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog reference");
    let catalog = pdf
        .get_object_mut(root)
        .and_then(Object::as_dict_mut)
        .expect("catalog");
    catalog
        .get_mut(b"AcroForm")
        .and_then(Object::as_dict_mut)
        .expect("form dictionary")
        .set("XFA", dictionary! {});
    pdf.save(path).expect("save XFA fixture");
}

fn append_broken_text_stream(path: &Path) {
    append_content_stream(
        path,
        Stream::new(
            dictionary! { "Filter" => "FlateDecode" },
            b"not a zlib stream".to_vec(),
        ),
    );
}

#[test]
fn default_channels_text_detection_survives_broken_forms() {
    let directory = TestDirectory::new();
    let old = directory.join("text-forms-old.pdf");
    let new = directory.join("text-forms-new.pdf");
    stored_fields_fixture_with_text(
        &old,
        &["Release 10 remains available"],
        &[("revenue", "100")],
    );
    stored_fields_fixture_with_text(
        &new,
        &["Release 20 remains available"],
        &[("revenue", "100")],
    );
    add_xfa_component(&old);
    add_xfa_component(&new);
    let report = directory.join("text-forms.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    // The text channel detected the replacement with intact flow evidence.
    // The forms channel's XFA failure cannot make that detection false.
    let report = read_json(&report);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(report["difference_status"], "detected");
    assert_eq!(report["exit_basis"], "detected_with_unexamined_evidence");
    let verdicts = report["channel_verdicts"].as_array().expect("verdicts");
    let text = verdicts
        .iter()
        .find(|verdict| verdict["channel"] == "text")
        .expect("text verdict");
    assert_eq!(text["detected"], true);
    assert_eq!(text["broken"], false);
    let forms = verdicts
        .iter()
        .find(|verdict| verdict["channel"] == "forms")
        .expect("forms verdict");
    assert_eq!(forms["broken"], true);
}

#[test]
fn default_channels_text_detection_does_not_survive_broken_text() {
    let directory = TestDirectory::new();
    let old = directory.join("broken-text-old.pdf");
    let new = directory.join("broken-text-new.pdf");
    write_pdf(&old, &["Release 10 remains available"]);
    write_pdf(&new, &["Release 20 remains available"]);
    append_broken_text_stream(&new);
    let report = directory.join("broken-text.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["exit_basis"], "broken_evidence");
    let text = report["channel_verdicts"]
        .as_array()
        .expect("verdicts")
        .iter()
        .find(|verdict| verdict["channel"] == "text")
        .expect("text verdict");
    assert_eq!(text["detected"], true);
    assert_eq!(text["broken"], true);
}

#[test]
fn default_channels_forms_detection_survives_broken_text() {
    let directory = TestDirectory::new();
    let old = directory.join("forms-text-old.pdf");
    let new = directory.join("forms-text-new.pdf");
    stored_fields_fixture(&old, &[("revenue", "100")]);
    stored_fields_fixture(&new, &[("revenue", "120")]);
    append_broken_text_stream(&new);
    let report = directory.join("forms-text.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    // The field value is a typed forms difference; neither text nor forms is
    // the detecting channel with broken evidence.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["exit_basis"], "detected_with_unexamined_evidence");
    let verdicts = report["channel_verdicts"].as_array().expect("verdicts");
    let forms = verdicts
        .iter()
        .find(|verdict| verdict["channel"] == "forms")
        .expect("forms verdict");
    assert_eq!(forms["detected"], true);
    assert_eq!(forms["broken"], false);
    let text = verdicts
        .iter()
        .find(|verdict| verdict["channel"] == "text")
        .expect("text verdict");
    assert_eq!(text["broken"], true);
    let output = run_default_engine(&old, &new, &[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stdout(&output).contains(
            "difference detected by forms, but some evidence was not examined (non-text paint or visual evidence)"
        ),
        "{}",
        stdout(&output)
    );
}

#[test]
fn default_channels_no_detection_with_broken_forms_is_exit_three() {
    let directory = TestDirectory::new();
    let old = directory.join("broken-forms-old.pdf");
    let new = directory.join("broken-forms-new.pdf");
    stored_fields_fixture(&old, &[("revenue", "100")]);
    stored_fields_fixture(&new, &[("revenue", "100")]);
    add_xfa_component(&old);
    add_xfa_component(&new);
    let report = directory.join("broken-forms.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["exit_basis"], "broken_evidence");
    assert_eq!(report["difference_status"], "indeterminate");
}

#[test]
fn default_channels_forms_only_change_still_exits_one() {
    let directory = TestDirectory::new();
    let old = directory.join("forms-only-old.pdf");
    let new = directory.join("forms-only-new.pdf");
    stored_fields_fixture(&old, &[("revenue", "100"), ("profit", "20")]);
    stored_fields_fixture(&new, &[("revenue", "120"), ("profit", "20")]);
    let report = directory.join("forms-only.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    // Identical text and no broken evidence: the forms value change is a
    // detected difference even though visual evidence stays unexamined.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["engine"], "flow");
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["difference_status"], "detected");
    assert_eq!(report["exit_basis"], "detected_with_unexamined_evidence");
    assert!(
        report["typed_changes"]
            .as_u64()
            .expect("typed change count")
            >= 1
    );
    let coverage = report["coverage"].as_array().expect("coverage");
    let forms = coverage
        .iter()
        .find(|entry| entry["channel"] == "forms")
        .expect("forms coverage");
    assert_eq!(forms["complete"], true);
    let text = coverage
        .iter()
        .find(|entry| entry["channel"] == "text")
        .expect("text coverage");
    assert_eq!(text["complete"], true);
    assert_eq!(text["old_compared_sources"], text["old_discovered_sources"]);
    assert_eq!(text["new_compared_sources"], text["new_discovered_sources"]);
    assert!(
        text["old_compared_sources"]
            .as_u64()
            .expect("flow token count")
            > 0
    );
    assert_eq!(report["flow_owned_text"]["comparisons"], 1);
    assert_eq!(report["flow_owned_text"]["components"], 1);
}

#[test]
fn flow_engine_non_text_channels_do_not_use_proof_text_as_exit_evidence() {
    let directory = TestDirectory::new();
    let old = directory.join("forms-channel-old.pdf");
    let new = directory.join("forms-channel-new.pdf");
    stored_fields_fixture(&old, &[("revenue", "100")]);
    stored_fields_fixture(&new, &[("revenue", "120")]);
    let report = directory.join("forms-channel.json");
    let output = run_flow_engine(
        &old,
        &new,
        &[
            "--channels",
            "forms",
            "--json",
            path_text(&report),
            "--quiet",
        ],
    );
    // The identical text must not be compared by proof as a hidden text
    // channel. The forms value change is the selected detection and exits 1.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["engine"], "flow");
    assert_eq!(report["flow_text"], Value::Null);
    let verdicts = report["channel_verdicts"].as_array().expect("verdicts");
    let forms = verdicts
        .iter()
        .find(|verdict| verdict["channel"] == "forms")
        .expect("forms verdict");
    assert_eq!(forms["detected"], true);
    assert_eq!(forms["broken"], false);
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict["channel"] != "text" || verdict["detected"] == false),
        "an unselected text channel must not justify the exit: {report:#}"
    );
}

#[test]
fn flow_engine_without_text_does_not_report_a_proof_text_detection() {
    let directory = TestDirectory::new();
    let old = directory.join("text-context-old.pdf");
    let new = directory.join("text-context-new.pdf");
    write_pdf(&old, &["Release 10 remains available"]);
    write_pdf(&new, &["Release 20 remains available"]);
    let report = directory.join("text-context.json");
    let output = run_flow_engine(
        &old,
        &new,
        &[
            "--channels",
            "forms,relations",
            "--json",
            path_text(&report),
            "--quiet",
        ],
    );
    let report = read_json(&report);
    assert_eq!(report["engine"], "flow");
    let verdicts = report["channel_verdicts"].as_array().expect("verdicts");
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict["channel"] != "text" || verdict["detected"] == false),
        "unselected proof text must remain context, not a detection: {report:#}"
    );
    // The text-only replacement cannot justify exit 1 when text was not
    // selected. Another selected channel may still detect, so only the text
    // justification is asserted absent.
    assert_ne!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn flow_color_always_styles_native_and_multi_channel_reports() {
    let directory = TestDirectory::new();
    let old = directory.join("color-old.pdf");
    let new = directory.join("color-new.pdf");
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
    for native in [true, false] {
        let extra = if native {
            vec!["--native-text-only"]
        } else {
            vec![]
        };
        let always = run_default_engine(
            &old,
            &new,
            &[extra.as_slice(), &["--color", "always"]].concat(),
        );
        assert_eq!(always.status.code(), Some(1), "{}", stderr(&always));
        assert!(
            stdout(&always).contains("\u{1b}["),
            "native={native}: {:#}",
            stdout(&always)
        );

        let never = run_default_engine(
            &old,
            &new,
            &[extra.as_slice(), &["--color", "never"]].concat(),
        );
        assert_eq!(never.status.code(), Some(1), "{}", stderr(&never));
        assert!(
            !stdout(&never).contains("\u{1b}["),
            "native={native}: {:#}",
            stdout(&never)
        );

        // Piped `auto` stays plain.
        let auto = run_default_engine(&old, &new, &extra);
        assert_eq!(auto.status.code(), Some(1), "{}", stderr(&auto));
        assert!(
            !stdout(&auto).contains("\u{1b}["),
            "native={native}: {:#}",
            stdout(&auto)
        );
    }

    // A text report written to a file follows the proof engine's rule: only
    // `--color always` styles it, because there is no terminal to inspect.
    let styled_path = directory.join("styled.txt");
    let styled = run_default_engine(
        &old,
        &new,
        &["--color", "always", "--output", path_text(&styled_path)],
    );
    assert_eq!(styled.status.code(), Some(1), "{}", stderr(&styled));
    assert!(
        fs::read_to_string(&styled_path)
            .expect("styled report")
            .contains("\u{1b}[")
    );
    let plain_path = directory.join("plain.txt");
    let plain = run_default_engine(
        &old,
        &new,
        &["--color", "never", "--output", path_text(&plain_path)],
    );
    assert_eq!(plain.status.code(), Some(1), "{}", stderr(&plain));
    assert!(
        !fs::read_to_string(&plain_path)
            .expect("plain report")
            .contains("\u{1b}[")
    );
}

#[test]
fn native_field_presence_reaches_cli_counts_and_coverage_without_character_masks() {
    let directory = TestDirectory::new();
    let old = directory.join("empty-fields.pdf");
    let new = directory.join("added-field.pdf");
    for (path, added) in [(&old, false), (&new, true)] {
        write_pdf(path, &["Retained background"]);
        let mut pdf = Document::load(path).expect("background");
        let fields = if added {
            vec![Object::Reference(pdf.add_object(dictionary! { "FT" => "Tx", "T" => Object::string_literal("added"), "V" => Object::string_literal("Stored value") }))]
        } else {
            Vec::new()
        };
        let root = pdf
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .expect("root");
        pdf.get_object_mut(root)
            .and_then(Object::as_dict_mut)
            .expect("catalog")
            .set("AcroForm", dictionary! { "Fields" => fields });
        pdf.save(path).expect("form PDF");
    }
    for (old, new, kind, presence) in [
        (&old, &new, "inserted", "new_presence_sources"),
        (&new, &old, "removed", "old_presence_sources"),
    ] {
        let report_path = directory.join(&format!("presence-{kind}.json"));
        let review = directory.join(&format!("presence-{kind}-review"));
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .arg(old)
            .arg(new)
            .args(["--channels", "forms", "--json"])
            .arg(&report_path)
            .arg("--review")
            .arg(&review)
            .output()
            .expect("field presence CLI");
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Keyed Field"), "{text}");
        assert!(text.contains("no character mask"), "{text}");
        assert!(!text.contains("Mandatory changed positions:"), "{text}");
        let report: Value =
            serde_json::from_slice(&fs::read(&report_path).expect("report")).expect("JSON");
        assert_eq!(report["comparison_complete"], true);
        assert_eq!(report["typed_changes"], 1);
        assert_eq!(report["coverage"][0][presence], 1);
        let operations = report["comparison"]["key_presence"]["scoped"]["operations"]
            .as_array()
            .expect("operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0]["kind"], kind);
        let html = fs::read_to_string(review.join("index.html")).expect("membership review");
        assert!(html.contains("Only identity evidence is owned"));
        assert_review_pointers(&html, &report);
        assert!(
            report["comparison"]["scopes"][0]["result"]["comparisons"]
                .as_array()
                .expect("comparisons")
                .is_empty()
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn native_worker_preserves_encrypted_cache_results_without_exposing_passwords() {
    let directory = TestDirectory::new();
    let old = directory.join("encrypted-old.pdf");
    let new = directory.join("encrypted-new.pdf");
    let password_path = directory.join("password.txt");
    let password = "fixture-acquisition-password";
    fs::write(&password_path, password).expect("fixture password");
    write_encrypted_pdf(&old, &["The shipment quantity is 100 kilograms."], password);
    write_encrypted_pdf(&new, &["The shipment quantity is 200 kilograms."], password);
    let cache = directory.join("cache");
    let mut previous: Option<Value> = None;
    for index in 0..3 {
        let report_path = directory.join(&format!("encrypted-{index}.json"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
        command
            .args(["--channels", "text"])
            .arg(&old)
            .arg(&new)
            .arg("--old-password-file")
            .arg(&password_path)
            .arg("--new-password-file")
            .arg(&password_path)
            .arg("--json")
            .arg(&report_path);
        if index != 0 {
            command.arg("--extraction-cache-dir").arg(&cache);
        }
        let output = command.output().expect("encrypted evidence comparison");
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        let json = fs::read_to_string(report_path).expect("encrypted report");
        assert!(!json.contains(password));
        let report: Value = serde_json::from_str(&json).expect("JSON");
        assert_eq!(report["old"]["native_glyphs"], 39);
        assert_eq!(report["typed_changes"], 1);
        if let Some(previous) = &previous {
            assert_eq!(report["comparison"], previous["comparison"]);
            assert_eq!(report["coverage"], previous["coverage"]);
        }
        previous = Some(report);
    }
    fs::write(&password_path, "incorrect-fixture-password").expect("wrong fixture password");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "text"])
        .arg(&old)
        .arg(&new)
        .arg("--old-password-file")
        .arg(&password_path)
        .arg("--new-password-file")
        .arg(&password_path)
        .arg("--extraction-cache-dir")
        .arg(&cache)
        .output()
        .expect("wrong-password comparison");
    assert_eq!(output.status.code(), Some(2));
    assert!(!stderr(&output).contains("incorrect-fixture-password"));
}

#[test]
#[cfg(target_os = "linux")]
fn native_worker_rejects_malformed_and_oversized_request_frames() {
    use std::io::Write as _;
    for (input, expected) in [
        (b"{}".to_vec(), 2),
        (b"{}\n".to_vec(), 2),
        (vec![b'x'; 1024 * 1024 + 1], 3),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .arg("acquire-native")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("native worker");
        child
            .stdin
            .take()
            .expect("worker input")
            .write_all(&input)
            .expect("send worker frame");
        let output = child.wait_with_output().expect("worker completion");
        assert_eq!(output.status.code(), Some(expected));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn button_appearance_state_disagreement_keeps_independent_field_changes() {
    let directory = TestDirectory::new();
    let old = directory.join("old-button.pdf");
    let new = directory.join("new-button.pdf");
    for (path, amount) in [(&old, "100"), (&new, "200")] {
        let mut pdf = build_pdf_pages_with_font(&[&[]], 12, "Type1", TextEncoding::Literal);
        let page = pdf.get_pages()[&1];
        let appearance = pdf.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()] }, b"0 0 10 10 re S".to_vec()));
        let button = pdf.add_object(dictionary! { "FT" => "Btn", "T" => Object::string_literal("choice"),
            "Subtype" => "Widget", "P" => page, "Rect" => vec![10.into(), 10.into(), 20.into(), 20.into()],
            "V" => "Yes", "AS" => "Off", "AP" => dictionary! { "N" => dictionary! { "Off" => appearance, "Yes" => appearance } } });
        let value = pdf.add_object(dictionary! { "FT" => "Tx", "T" => Object::string_literal("fee"), "V" => Object::string_literal(amount) });
        pdf.get_object_mut(page)
            .expect("page")
            .as_dict_mut()
            .expect("page dictionary")
            .set("Annots", vec![Object::Reference(button)]);
        let root = pdf
            .trailer
            .get(b"Root")
            .expect("catalog")
            .as_reference()
            .expect("catalog reference");
        pdf.get_object_mut(root).expect("catalog object").as_dict_mut().expect("catalog dictionary")
            .set("AcroForm", dictionary! { "Fields" => vec![Object::Reference(button), Object::Reference(value)] });
        pdf.save(path).expect("button fixture");
    }
    let report_path = directory.join("button.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "forms"])
        .arg(old)
        .arg(new)
        .arg("--json")
        .arg(&report_path)
        .output()
        .expect("forms comparison");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report: Value =
        serde_json::from_slice(&fs::read(report_path).expect("form report")).expect("JSON report");
    assert_eq!(report["typed_changes"], 1, "{report}");
    assert_eq!(report["comparison_complete"], false);
    for side in ["old", "new"] {
        let field = &report[side]["form_fields"][0];
        assert_eq!(
            field["value"]["button_states"][0]["name"],
            serde_json::json!([79, 102, 102])
        );
        assert!(!field["value"]["button_states"][0]["widget"].is_null());
        let issue = report[side]["issues"]
            .as_array()
            .expect("form issues")
            .iter()
            .find(|issue| {
                issue["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("states disagree"))
            })
            .expect("appearance disagreement");
        assert_eq!(issue["sources"][0]["origin"], "structured");
        assert_eq!(issue["sources"][0]["element"], 0);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn form_widget_pixels_and_saved_values_compare_independently() {
    let directory = TestDirectory::new();
    let old = directory.join("old-widget.pdf");
    let mut inputs = Vec::new();
    for (index, (value, color, moved, cover, second_x)) in [
        ("100", "1 0 0", false, false, None),
        ("100", "0 0 1", false, false, None),
        ("200", "1 0 0", false, false, None),
        ("200", "0 0 1", false, false, None),
        ("100", "1 0 0", true, false, None),
        ("100", "1 0 0", false, true, None),
        ("100", "1 0 0", false, false, Some(10)),
        ("100", "0 0 1", false, false, Some(10)),
        ("100", "1 0 0", false, false, Some(160)),
        ("100", "0 0 1", false, false, Some(160)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut pdf = build_pdf_pages_with_font(
            if cover { &[&[], &[]] } else { &[&[]] },
            12,
            "Type1",
            TextEncoding::Literal,
        );
        let page = pdf.get_pages()[&if cover { 2 } else { 1 }];
        let normal = pdf.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 100.into(), 30.into()], "Resources" => dictionary!{} }, format!("{color} rg 0 0 100 30 re f").into_bytes()));
        let x = if moved { 60 } else { 10 };
        let field = pdf.add_object(dictionary! { "FT" => "Tx", "Subtype" => "Widget", "T" => Object::string_literal("fee"), "V" => Object::string_literal(value),
            "Rect" => vec![x.into(), 10.into(), (x + 100).into(), 40.into()], "AP" => dictionary! { "N" => normal } });
        let mut fields = vec![Object::Reference(field)];
        if let Some(x) = second_x {
            let mut other = pdf
                .get_object(field)
                .expect("field")
                .as_dict()
                .expect("field dictionary")
                .clone();
            other.set("T", Object::string_literal("other"));
            other.set(
                "Rect",
                vec![Object::from(x), 10.into(), (x + 100).into(), 40.into()],
            );
            fields.push(Object::Reference(pdf.add_object(other)));
        }
        // Page membership comes from Annots; the optional P entry is absent.
        pdf.get_object_mut(page)
            .expect("page")
            .as_dict_mut()
            .expect("page dictionary")
            .set("Annots", fields.clone());
        let root = pdf
            .trailer
            .get(b"Root")
            .expect("catalog")
            .as_reference()
            .expect("catalog reference");
        pdf.get_object_mut(root)
            .expect("catalog object")
            .as_dict_mut()
            .expect("catalog dictionary")
            .set("AcroForm", dictionary! { "Fields" => fields });
        let path = if index == 0 {
            old.clone()
        } else {
            directory.join(&format!("widget-{index}.pdf"))
        };
        pdf.save(&path).expect("widget fixture");
        inputs.push(path);
    }
    for (index, input) in inputs.iter().enumerate() {
        let baseline = if index >= 8 {
            &inputs[8]
        } else if index >= 6 {
            &inputs[6]
        } else {
            &old
        };
        for channels in ["forms", "forms,visual"] {
            let report_path = directory.join(&format!("widget-{index}-{channels}.json"));
            let review = directory.join(&format!("widget-review-{index}-{channels}"));
            let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
                .args(["--channels", channels])
                .arg(baseline)
                .arg(input)
                .arg("--json")
                .arg(&report_path)
                .arg("--review")
                .arg(&review)
                .output()
                .expect("widget comparison");
            let report: Value =
                serde_json::from_slice(&fs::read(report_path).expect("widget report"))
                    .expect("JSON report");
            // Flow's multi-channel policy: only a channel that detected a
            // typed difference with its own intact evidence selects exit 1.
            // Broken forms evidence still keeps the comparison at exit 3.
            let intact_detection = report["channel_verdicts"]
                .as_array()
                .is_some_and(|verdicts| {
                    verdicts
                        .iter()
                        .any(|verdict| verdict["detected"] == true && verdict["broken"] == false)
                });
            assert_eq!(
                output.status.code(),
                Some(if intact_detection { 1 } else { 3 }),
                "index {index} channels {channels}: {}",
                stderr(&output)
            );
            assert_eq!(report["comparison_complete"], false);
            assert_eq!(
                report["typed_changes"],
                usize::from(index == 2 || index == 3)
                    + usize::from(index == 1 || index == 3)
                    + 2 * usize::from(index == 9),
                "{report}"
            );
            assert_eq!(report["inferred_changes"], 0, "{report}");
            if matches!(index, 1 | 2 | 3 | 9) {
                let html = fs::read_to_string(review.join("index.html")).expect("widget review");
                assert!(html.contains("old.pdf#page=1") && html.contains("new.pdf#page=1"));
                assert_review_pointers(&html, &report);
            }
            assert!(
                !report["old"]["form_fields"][0]["value"]["widgets"][0]["crop"].is_null(),
                "{report}"
            );
            assert_eq!(
                report["old"]["rendered_regions"],
                if index >= 6 { 3 } else { 2 }
            );
            if index == 7 {
                assert!(
                    report["comparison"]["scopes"]
                        .as_array()
                        .expect("scopes")
                        .iter()
                        .any(|scope| scope["result"]["unresolved"]
                            .as_array()
                            .expect("scope issues")
                            .iter()
                            .any(|reason| reason
                                .as_str()
                                .is_some_and(|reason| reason.contains("competing optima")))),
                    "{report}"
                );
            }
        }
    }
}

#[test]
fn external_vertical_text_preserves_evidence_across_page_moves_and_edits() {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/vertical-tectonic");
    let old = fixtures.join("vertical-old.pdf");
    let extracted = inspect(&old, &["--glyphs"]);
    assert!(extracted.status.success(), "{}", stderr(&extracted));
    assert_eq!(stdout(&extracted).matches("direction=(0,-1)").count(), 10);
    for (name, glyphs, changed) in [
        ("vertical-moved", 14, false),
        ("vertical-changed", 15, true),
    ] {
        let directory = TestDirectory::new();
        let destination = directory.join("report.json");
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .arg(&old)
            .arg(fixtures.join(format!("{name}.pdf")))
            .arg("--json")
            .arg(&destination)
            .arg("--quiet")
            .output()
            .expect("default evidence comparison");
        let expected_exit = if changed { Some(1) } else { Some(3) };
        assert_eq!(output.status.code(), expected_exit, "{}", stderr(&output));
        let report = read_json(&destination);
        assert_eq!(report["comparison_complete"], false);
        if changed {
            assert_eq!(report["exit_basis"], "detected_with_unexamined_evidence");
        }
        assert_eq!(report["old"]["native_glyphs"], 14);
        assert_eq!(report["new"]["native_glyphs"], glyphs);
        // The text channel is flow's; the proof comparison no longer retains
        // a second text change or its local mask.
        let proof_text_changes: Vec<_> = report["comparison"]["scopes"]
            .as_array()
            .expect("scopes")
            .iter()
            .flat_map(|scope| {
                scope["result"]["comparisons"]
                    .as_array()
                    .expect("local comparisons")
            })
            .filter(|pair| pair["operation"]["kind"] == "text_changed")
            .collect();
        assert!(proof_text_changes.is_empty(), "{report}");
        assert!(
            report["flow_owned_text"]["comparisons"]
                .as_u64()
                .expect("superseded text comparisons")
                >= 1
        );
        let coverage = report["coverage"]
            .as_array()
            .expect("channels")
            .iter()
            .find(|coverage| coverage["channel"] == "text")
            .expect("text coverage");
        assert_eq!(coverage["old_compared_sources"], 14);
        assert_eq!(coverage["new_compared_sources"], glyphs);
        if changed {
            assert_eq!(report["typed_changes"], 1);
            let flow_changes = report["flow_text"]["changes"]
                .as_array()
                .expect("flow changes");
            assert_eq!(flow_changes.len(), 1);
            assert_eq!(flow_changes[0]["kind"], "insertion");
            assert_eq!(flow_changes[0]["new_span"]["text"], "二");
        } else {
            assert!(
                report["flow_text"]["changes"]
                    .as_array()
                    .expect("flow changes")
                    .is_empty()
            );
        }
    }
}

/// Run a comparison with the given directory as the working directory so
/// relative output-path spellings can be exercised.
fn compare_in(directory: &Path, old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
    if extra_arguments
        .iter()
        .any(|arg| matches!(*arg, "--json" | "-j"))
    {
        command.arg("--full-json");
    }
    command
        .arg("--native-text-only")
        .arg(old)
        .arg(new)
        .args(extra_arguments)
        .current_dir(directory)
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

fn assert_review_pointers(html: &str, report: &Value) {
    let mut count = 0;
    for suffix in html.split("comparison.json pointer: <code>").skip(1) {
        let pointer = suffix.split("</code>").next().expect("evidence pointer");
        assert!(
            report.pointer(pointer).is_some(),
            "missing report pointer {pointer}"
        );
        count += 1;
    }
    assert!(count > 0, "review should contain evidence-backed units");
}

#[test]
fn static_review_refuses_existing_destinations_and_nested_report_collisions() {
    let directory = TestDirectory::new();
    let input = directory.join("source.pdf");
    write_pdf(&input, &["Retained source"]);
    let original = fs::read(&input).expect("source");
    let review = directory.join("review");
    let nested = review.join("comparison.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&input)
        .arg(&input)
        .arg("--review")
        .arg(&review)
        .arg("--json")
        .arg(&nested)
        .output()
        .expect("collision check");
    assert_eq!(output.status.code(), Some(2));
    assert!(!review.exists());
    for destination in [&input, &directory.0] {
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .arg(&input)
            .arg(&input)
            .arg("--review")
            .arg(destination)
            .output()
            .expect("existing destination check");
        assert_eq!(output.status.code(), Some(2));
        assert!(stderr(&output).contains("refusing existing review directory"));
    }
    assert_eq!(fs::read(&input).expect("preserved input"), original);
}

fn write_huge_image_pdf(path: &Path) {
    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let image = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 19_848_i64,
            "Height" => 28_063_i64,
            "BitsPerComponent" => 1,
            "ColorSpace" => "DeviceGray",
            "ImageMask" => true,
        },
        vec![0_u8; 8],
    ));
    let contents = document.add_object(Stream::new(
        dictionary! {},
        b"q 100 0 0 100 0 0 cm /Im0 Do Q".to_vec(),
    ));
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(100), Object::from(100)],
        "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
        "Contents" => contents,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page)],
            "Count" => 1,
        }),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document
        .save(path)
        .expect("huge-image fixture should serialize");
}

fn write_inline_image_pdf(path: &Path) {
    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let contents = document.add_object(Stream::new(
        dictionary! {},
        b"q 100 0 0 100 0 0 cm BI /W 19848 /H 28063 /BPC 1 /CS /G /IM true ID \x00 EI Q".to_vec(),
    ));
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(100), Object::from(100)],
        "Contents" => contents,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page)],
            "Count" => 1,
        }),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document
        .save(path)
        .expect("inline-image fixture should serialize");
}

fn write_many_page_pdf(path: &Path, page_count: usize) {
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let widths = vec![Object::Integer(500); 256];
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
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
    let mut kids = Vec::with_capacity(page_count);
    for _ in 0..page_count {
        let content = document.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 8 Tf 10 40 Td (P) Tj ET".to_vec(),
        ));
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "Contents" => content,
            "Resources" => resources,
            "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(100), Object::from(100)],
        });
        kids.push(Object::Reference(page));
    }
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => i64::try_from(page_count).expect("page count should fit in i64"),
        }),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document
        .save(path)
        .expect("many-page fixture should serialize");
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

fn write_pdf_with_missing_page_tree_child(
    path: &Path,
    pages_content: &[&[&str]],
    broken_index: usize,
) {
    let mut document = build_pdf_pages_with_font(pages_content, 30, "Type1", TextEncoding::Literal);
    let catalog = document
        .trailer
        .get(b"Root")
        .expect("fixture trailer should contain Root")
        .as_reference()
        .expect("fixture Root should be a reference");
    let pages = document
        .objects
        .get(&catalog)
        .expect("fixture catalog should exist")
        .as_dict()
        .expect("fixture catalog should be a dictionary")
        .get(b"Pages")
        .expect("fixture catalog should contain Pages")
        .as_reference()
        .expect("fixture Pages should be a reference");
    let broken_page = document
        .objects
        .get(&pages)
        .expect("fixture Pages root should exist")
        .as_dict()
        .expect("fixture Pages root should be a dictionary")
        .get(b"Kids")
        .expect("fixture Pages root should contain Kids")
        .as_array()
        .expect("fixture Kids should be an array")[broken_index]
        .as_reference()
        .expect("fixture child should be a reference");
    document.objects.remove(&broken_page);
    document.save(path).expect("fixture PDF should serialize");
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
    let mut document =
        build_pdf_pages_with_font(pages_content, line_gap, font_subtype, text_encoding);
    document.save(path).expect("fixture PDF should serialize");
}

fn write_encrypted_pdf(path: &Path, lines: &[&str], user_password: &str) {
    let mut document = build_pdf_pages_with_font(&[lines], 30, "Type1", TextEncoding::Literal);
    // Standard-security encryption derives keys from the trailer /ID, and a
    // non-empty owner password keeps the loader from accepting the empty
    // user password on this fixture.
    document.trailer.set(
        "ID",
        Object::Array(vec![
            Object::string_literal(vec![1_u8; 16]),
            Object::string_literal(vec![2_u8; 16]),
        ]),
    );
    let version = EncryptionVersion::V1 {
        document: &document,
        owner_password: "fixture-owner-password",
        user_password,
        permissions: Permissions::all(),
    };
    let state = EncryptionState::try_from(version).expect("encryption state should build");
    document
        .encrypt(&state)
        .expect("fixture PDF should encrypt");
    document.save(path).expect("fixture PDF should serialize");
}

fn build_pdf_pages_with_font(
    pages_content: &[&[&str]],
    line_gap: i64,
    font_subtype: &str,
    text_encoding: TextEncoding,
) -> Document {
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
    document
}

fn write_pdf_with_structure(path: &Path, lines: &[&str], structure_root: Option<Object>) {
    let mut document = build_pdf_pages_with_font(&[lines], 30, "Type1", TextEncoding::Literal);
    if let Some(structure_root) = structure_root {
        let catalog = document
            .trailer
            .get(b"Root")
            .expect("fixture trailer should contain Root")
            .as_reference()
            .expect("fixture Root should be a reference");
        document
            .objects
            .get_mut(&catalog)
            .expect("fixture catalog should exist")
            .as_dict_mut()
            .expect("fixture catalog should be a dictionary")
            .set("StructTreeRoot", structure_root);
    }
    document.save(path).expect("fixture PDF should serialize");
}

fn run_default_report(old: &Path, new: &Path, report: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(old)
        .arg(new)
        .arg("--json")
        .arg(report)
        .output()
        .expect("default comparison CLI")
}

fn positioned_words(text: &str) -> String {
    let words = text
        .split_ascii_whitespace()
        .map(|word| format!("({})", escape_pdf_literal(word)))
        .collect::<Vec<_>>()
        .join(" -500 ");
    format!("[{words}] TJ")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("JSON file should be readable"))
        .expect("JSON file should be valid")
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

fn append_content_stream(path: &Path, stream: Stream) {
    let mut pdf = Document::load(path).expect("load fixture");
    let page = pdf.get_pages()[&1];
    let painted = pdf.add_object(stream);
    let dictionary = pdf
        .get_object_mut(page)
        .expect("page")
        .as_dict_mut()
        .expect("page dictionary");
    let native = dictionary
        .get(b"Contents")
        .expect("native contents")
        .clone();
    dictionary.set("Contents", vec![native, Object::Reference(painted)]);
    pdf.save(path).expect("save fixture with appended content");
}

fn image_only_fixture(path: &Path, rgb: [u8; 3]) {
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let image = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB",
        },
        rgb.to_vec(),
    ));
    let contents = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 72 0 0 72 0 0 cm /I Do Q".to_vec(),
    ));
    let page = pdf.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(72), Object::from(72)],
        "Resources" => dictionary! { "XObject" => dictionary! { "I" => image } }, "Contents" => contents,
    });
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
        }),
    );
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    pdf.save(path).expect("image-only fixture");
}

fn run_flow_engine(old: &Path, new: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg("--engine")
        .arg("flow")
        .arg(old)
        .arg(new)
        .args(extra)
        .output()
        .expect("pdfdelta flow engine should run")
}

/// Runs the main command without `--engine` so the default selection is under test.
fn run_default_engine(old: &Path, new: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(old)
        .arg(new)
        .args(extra)
        .output()
        .expect("pdfdelta default engine should run")
}

/// Replaces the page content with one vector paint program and no native text.
fn outlined_fixture(path: &Path, program: &[u8]) {
    write_pdf(path, &["temporary native content"]);
    let mut pdf = Document::load(path).expect("load outlined fixture");
    let page = pdf.get_pages()[&1];
    let content = pdf.add_object(Stream::new(dictionary! {}, program.to_vec()));
    pdf.get_object_mut(page)
        .expect("page")
        .as_dict_mut()
        .expect("page dictionary")
        .set("Contents", content);
    pdf.save(path).expect("save outlined fixture");
}

fn read_gzip_json(path: &Path) -> Value {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(path).expect("gzip report"))
        .read_to_string(&mut text)
        .expect("gzip report should be readable");
    serde_json::from_str(&text).expect("gzip report should be JSON")
}

fn single_flow_change<'a>(report: &'a Value, kind: &str) -> &'a Value {
    let inner = &report["report"];
    assert_eq!(inner["engine"], "flow");
    assert_eq!(inner["summary"]["content_changes"], 1, "{inner:#}");
    assert_eq!(inner["summary"]["difference_status"], "detected");
    let changes = inner["changes"].as_array().expect("changes");
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(changes[0]["kind"], kind, "{changes:#?}");
    &changes[0]
}

#[test]
fn flow_paint_inventory_guards_outlined_text_and_images() {
    let directory = TestDirectory::new();
    let same = directory.join("outlined-same.pdf");
    let changed = directory.join("outlined-changed.pdf");
    let program = b"20 20 m 30 25 l 30 70 l 20 70 l h f";
    let other = b"20 20 m 45 25 l 45 70 l 20 70 l h f";
    outlined_fixture(&same, program);
    outlined_fixture(&changed, other);

    // Identical outlined content is complete: the paint inventory itself
    // certifies that the visible shapes did not change.
    let same_report = directory.join("outlined-same.json");
    let output = run_default_engine(
        &same,
        &same,
        &["--native-text-only", "-j", path_text(&same_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = read_json(&same_report);
    assert_eq!(report["report"]["engine"], "flow");
    assert_eq!(report["report"]["summary"]["comparison_complete"], true);
    assert_eq!(
        report["report"]["extraction"]["issues"],
        Value::Array(Vec::new())
    );

    // Different outlines are unexamined text, not a silent "no change".
    let changed_report = directory.join("outlined-changed.json");
    let output = run_default_engine(
        &same,
        &changed,
        &["--native-text-only", "-j", path_text(&changed_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&changed_report);
    assert_eq!(report["report"]["engine"], "flow");
    assert_eq!(report["report"]["summary"]["comparison_complete"], false);
    assert_eq!(
        report["report"]["summary"]["difference_status"],
        "indeterminate"
    );
    assert_eq!(report["report"]["exit_basis"], "unexamined_evidence");
    assert!(
        report["report"]["extraction"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| issue["message"]
                .as_str()
                .is_some_and(|description| description.contains("non-text paint differs")))
    );
    // Paint issues are unexamined evidence, not broken extraction: they have
    // their own counter and do not inflate the extraction counters.
    assert_eq!(report["report"]["summary"]["unexamined_paint_issues"], 2);
    assert_eq!(
        report["report"]["summary"]["unresolved_extraction_issues"],
        0
    );

    // Native text with identical paint keeps the text change complete.
    let mixed_old = directory.join("mixed-outlined-old.pdf");
    let mixed_new = directory.join("mixed-outlined-new.pdf");
    write_pdf(&mixed_old, &["Release 10 remains available"]);
    write_pdf(&mixed_new, &["Release 20 remains available"]);
    for path in [&mixed_old, &mixed_new] {
        append_content_stream(path, Stream::new(dictionary! {}, program.to_vec()));
    }
    let mixed_report = directory.join("mixed-outlined.json");
    let output = run_default_engine(
        &mixed_old,
        &mixed_new,
        &["--native-text-only", "-j", path_text(&mixed_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&mixed_report);
    assert_eq!(report["report"]["summary"]["comparison_complete"], true);
    assert_eq!(report["report"]["summary"]["content_changes"], 1);
    assert_eq!(report["report"]["changes"][0]["kind"], "replacement");

    // A detected native change plus changed outline paint is exit 1, but the
    // report stays incomplete and lists the unexamined paint.
    let mixed_changed = directory.join("mixed-outlined-changed.pdf");
    write_pdf(&mixed_changed, &["Release 20 remains available"]);
    append_content_stream(&mixed_changed, Stream::new(dictionary! {}, other.to_vec()));
    let mixed_changed_report = directory.join("mixed-outlined-changed.json");
    let output = run_default_engine(
        &mixed_old,
        &mixed_changed,
        &[
            "--native-text-only",
            "-j",
            path_text(&mixed_changed_report),
            "-q",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&mixed_changed_report);
    assert_eq!(report["report"]["summary"]["comparison_complete"], false);
    assert_eq!(report["report"]["summary"]["difference_status"], "detected");
    assert_eq!(
        report["report"]["exit_basis"],
        "detected_with_unexamined_evidence"
    );
    assert_eq!(report["report"]["summary"]["content_changes"], 1);
    assert!(
        report["report"]["extraction"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| issue["message"]
                .as_str()
                .is_some_and(|description| description.contains("non-text paint differs")))
    );
    let text_output = run_default_engine(&mixed_old, &mixed_changed, &["--native-text-only"]);
    assert_eq!(
        text_output.status.code(),
        Some(1),
        "{}",
        stderr(&text_output)
    );
    assert!(
        stdout(&text_output).contains(
            "difference detected, but some evidence was not examined (non-text paint or visual evidence)"
        ),
        "{}",
        stdout(&text_output)
    );

    // Broken extraction evidence keeps exit 3 even when a text change is
    // visible, because the detected change may be an artefact of missing
    // glyphs.
    let broken_old = directory.join("mixed-broken-old.pdf");
    let broken_new = directory.join("mixed-broken-new.pdf");
    write_pdf(&broken_old, &["Release 10 remains available"]);
    write_pdf(&broken_new, &["Release 20 remains available"]);
    append_content_stream(
        &broken_old,
        Stream::new(
            dictionary! { "Filter" => "FlateDecode" },
            b"not a zlib stream".to_vec(),
        ),
    );
    let broken_report = directory.join("mixed-broken.json");
    let output = run_default_engine(
        &broken_old,
        &broken_new,
        &["--native-text-only", "-j", path_text(&broken_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&broken_report);
    assert_eq!(report["report"]["summary"]["comparison_complete"], false);
    assert_eq!(report["report"]["exit_basis"], "broken_evidence");
    assert_eq!(report["report"]["summary"]["unexamined_paint_issues"], 0);
    assert!(
        report["report"]["summary"]["unresolved_extraction_issues"]
            .as_u64()
            .expect("broken extraction counter")
            >= 1
    );
    assert!(
        report["report"]["extraction"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| !issue["message"]
                .as_str()
                .is_some_and(|description| description.contains("non-text paint differs")))
    );

    // Artifact-marked paint follows the same rule; the proof test treats it as
    // unexamined, so the flow inventory must not ignore it.
    let artifact_same = directory.join("artifact-same.pdf");
    let artifact_changed = directory.join("artifact-changed.pdf");
    let artifact_program = [b"/Artifact BMC ".as_slice(), program, b" EMC"].concat();
    let artifact_other = [b"/Artifact BMC ".as_slice(), other, b" EMC"].concat();
    outlined_fixture(&artifact_same, &artifact_program);
    outlined_fixture(&artifact_changed, &artifact_other);
    let artifact_report = directory.join("artifact-same.json");
    let output = run_default_engine(
        &artifact_same,
        &artifact_same,
        &[
            "--native-text-only",
            "-j",
            path_text(&artifact_report),
            "-q",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let output = run_default_engine(
        &artifact_same,
        &artifact_changed,
        &["--native-text-only", "-q"],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));

    // The multi-channel text route and the standalone flow command see the
    // same paint incompleteness and reason.
    let channels_report = directory.join("outlined-channels.json");
    let output = run_default_engine(
        &same,
        &changed,
        &[
            "--channels",
            "text",
            "-j",
            path_text(&channels_report),
            "-q",
        ],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&channels_report);
    assert_eq!(report["engine"], "flow");
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["flow_text"]["complete"], false);
    assert_eq!(report["flow_text"]["summary"]["unexamined_paint_issues"], 2);
    assert_eq!(
        report["flow_text"]["summary"]["unresolved_extraction_issues"],
        0
    );
    assert!(
        report["flow_text"]["changes"]
            .as_array()
            .expect("flow changes")
            .is_empty(),
        "{report:#}"
    );

    let flow_report = directory.join("outlined-flow.json");
    let flow_output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg("flow")
        .arg(&same)
        .arg(&changed)
        .arg("--json")
        .arg(&flow_report)
        .output()
        .expect("standalone flow command");
    assert_eq!(
        flow_output.status.code(),
        Some(3),
        "{}",
        stderr(&flow_output)
    );
    let flow_json = read_json(&flow_report);
    assert_eq!(flow_json["exit_basis"], "unexamined_evidence");
    assert_eq!(flow_json["difference_status"], "indeterminate");
    assert!(
        flow_json["old"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .chain(flow_json["new"]["issues"].as_array().expect("issues"))
            .any(|issue| issue["description"]
                .as_str()
                .is_some_and(|description| description.contains(
                    "non-text paint differs; text drawn as paths or images was not examined"
                )))
    );

    // Image-only pages use the same rule. Equal images are complete; changed
    // image data is incomplete before any image-token change can be trusted.
    let image_same = directory.join("image-same.pdf");
    let image_changed = directory.join("image-changed.pdf");
    image_only_fixture(&image_same, [255, 0, 0]);
    image_only_fixture(&image_changed, [0, 0, 255]);
    let image_report = directory.join("image-same.json");
    let output = run_default_engine(
        &image_same,
        &image_same,
        &["--native-text-only", "-j", path_text(&image_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = read_json(&image_report);
    assert_eq!(report["report"]["summary"]["comparison_complete"], true);

    let image_changed_report = directory.join("image-changed.json");
    let output = run_default_engine(
        &image_same,
        &image_changed,
        &[
            "--native-text-only",
            "-j",
            path_text(&image_changed_report),
            "-q",
        ],
    );
    // A changed image is a detected flow difference (the image token changed)
    // plus unexamined paint, so M4c selects exit 1 while keeping the report
    // incomplete.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&image_changed_report);
    assert_eq!(report["report"]["summary"]["comparison_complete"], false);
    assert_eq!(
        report["report"]["exit_basis"],
        "detected_with_unexamined_evidence"
    );
    assert_eq!(report["report"]["summary"]["unexamined_paint_issues"], 2);
    assert_eq!(
        report["report"]["summary"]["unresolved_extraction_issues"],
        0
    );
    assert!(
        report["report"]["extraction"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| issue["message"]
                .as_str()
                .is_some_and(|description| description.contains("non-text paint differs")))
    );
}

#[test]
fn default_channels_report_unexamined_visual_evidence_without_detected_change() {
    let directory = TestDirectory::new();
    let old = directory.join("identical-old.pdf");
    let new = directory.join("identical-new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let report = directory.join("identical-default.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    // The native text channel is complete; the visual channel retains page
    // rasters without certifying renderer coverage, so the default channels
    // are unexamined rather than broken.
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["engine"], "flow");
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["difference_status"], "indeterminate");
    assert_eq!(report["exit_basis"], "unexamined_evidence");
    assert!(
        report["coverage"]
            .as_array()
            .expect("coverage")
            .iter()
            .any(|entry| entry["channel"] == "visual" && entry["complete"] == false)
    );
}

#[test]
fn untagged_pair_reports_a_complete_empty_relation_inventory() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("report.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = run_default_report(&old, &new, &report);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["comparison"]["relation_inventory"], "complete_empty");
    assert_eq!(report["comparison"]["relations"], serde_json::json!([]));
    assert_eq!(
        report["comparison"]["relation_unresolved"],
        serde_json::json!([])
    );
    let relations = report["coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .find(|coverage| coverage["channel"] == "relations")
        .expect("relations coverage");
    assert_eq!(relations["old_inventory_complete"], true);
    assert_eq!(relations["new_inventory_complete"], true);
    assert_eq!(relations["complete"], true);
    let relations = report["channel_verdicts"]
        .as_array()
        .expect("channel verdicts")
        .iter()
        .find(|verdict| verdict["channel"] == "relations");
    assert!(
        relations
            .is_none_or(|verdict| { verdict["broken"] == false && verdict["unexamined"] == false })
    );
}

#[test]
fn a_single_tagged_source_keeps_relations_unexamined() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("report.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf_with_structure(
        &new,
        &["A generic paragraph remains stable"],
        Some(Object::Dictionary(dictionary! {
            "Type" => "StructTreeRoot",
            "K" => Vec::<Object>::new(),
        })),
    );

    let output = run_default_report(&old, &new, &report);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert!(report["comparison"].get("relation_inventory").is_none());
    let relations = report["coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .find(|coverage| coverage["channel"] == "relations")
        .expect("relations coverage");
    assert_eq!(relations["old_inventory_complete"], false);
    assert_eq!(relations["complete"], false);
    let relations = report["channel_verdicts"]
        .as_array()
        .expect("channel verdicts")
        .iter()
        .find(|verdict| verdict["channel"] == "relations")
        .expect("relations verdict");
    assert_eq!(relations["unexamined"], true);
}

#[test]
fn an_unreadable_structure_root_remains_broken_relation_evidence() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("report.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf_with_structure(
        &new,
        &["A generic paragraph remains stable"],
        Some(Object::Dictionary(dictionary! {
            "Type" => "StructTreeRoot",
            "K" => Object::Integer(7),
        })),
    );

    let output = run_default_report(&old, &new, &report);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert!(report["comparison"].get("relation_inventory").is_none());
    let relations = report["channel_verdicts"]
        .as_array()
        .expect("channel verdicts")
        .iter()
        .find(|verdict| verdict["channel"] == "relations")
        .expect("relations verdict");
    assert_eq!(relations["broken"], true);
}

#[test]
fn tagged_structure_key_obligations_are_unexamined_not_broken() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case1-japanese-typst");
    let old = fixture_dir.join("old.pdf");
    let new = fixture_dir.join("new.pdf");
    assert!(old.exists() && new.exists(), "vendored fixture must exist");
    let directory = TestDirectory::new();
    let report = directory.join("tagged-default.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    // The Typst structure tree has no element IDs. The resulting key
    // obligation records missing membership, not examined-and-untrustworthy
    // evidence, so the line-wrap-only pair exits 3 as unexamined.
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["exit_basis"], "unexamined_evidence");
    let relations = report["channel_verdicts"]
        .as_array()
        .expect("verdicts")
        .iter()
        .find(|verdict| verdict["channel"] == "relations")
        .expect("relations verdict");
    assert_eq!(relations["broken"], false);
    assert_eq!(relations["unexamined"], true);
    let obligations = report["comparison"]["key_presence"]["obligations"]
        .as_array()
        .expect("key obligations");
    assert!(!obligations.is_empty());
    assert!(
        obligations
            .iter()
            .all(|obligation| obligation["work"] == "unavailable")
    );
}

#[test]
fn tagged_structure_key_obligations_do_not_block_a_text_detection() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case3-typst");
    let old = fixture_dir.join("old.pdf");
    let new = fixture_dir.join("new.pdf");
    assert!(old.exists() && new.exists(), "vendored fixture must exist");
    let directory = TestDirectory::new();
    let report = directory.join("tagged-replacement.json");
    let output = run_default_engine(&old, &new, &["-j", path_text(&report), "-q"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["exit_basis"], "detected_with_unexamined_evidence");
    assert_eq!(report["difference_status"], "detected");
    let relations = report["channel_verdicts"]
        .as_array()
        .expect("verdicts")
        .iter()
        .find(|verdict| verdict["channel"] == "relations")
        .expect("relations verdict");
    assert_eq!(relations["broken"], false);
}

#[test]
fn flow_engine_acceptance_cases() {
    let directory = TestDirectory::new();
    // Line-wrap invariance.
    let old = directory.join("wrap-old.pdf");
    let new = directory.join("wrap-new.pdf");
    let wrap_report = directory.join("wrap.json.gz");
    write_pdf(
        &old,
        &["A detailed release note remains stable across lines"],
    );
    write_pdf(
        &new,
        &["A detailed release note", "remains stable across lines"],
    );
    let output = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&wrap_report), "-q"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}\n{}",
        stderr(&output),
        String::from_utf8_lossy(&output.stdout)
    );
    let wrap = read_gzip_json(&wrap_report);
    assert_eq!(wrap["report"]["engine"], "flow");
    assert_eq!(wrap["report"]["summary"]["content_changes"], 0);
    assert_eq!(
        wrap["report"]["summary"]["difference_status"],
        "no_content_change"
    );
    assert_eq!(wrap["report"]["changes"], Value::Array(Vec::new()));

    // Page-break invariance.
    let old = directory.join("page-old.pdf");
    let new = directory.join("page-new.pdf");
    let page_report = directory.join("page.json.gz");
    write_pdf_pages(
        &old,
        &[&[
            "First line keeps a steady cadence",
            "Second line keeps a steady cadence",
            "Third line keeps a steady cadence",
            "Fourth line keeps a steady cadence",
        ]],
        12,
    );
    write_pdf_pages(
        &new,
        &[
            &[
                "First line keeps a steady cadence",
                "Second line keeps a steady cadence",
            ],
            &[
                "Third line keeps a steady cadence",
                "Fourth line keeps a steady cadence",
            ],
        ],
        12,
    );
    let output = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&page_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let page = read_gzip_json(&page_report);
    assert_eq!(page["report"]["engine"], "flow");
    assert_eq!(page["report"]["summary"]["content_changes"], 0);
    assert_eq!(page["report"]["changes"], Value::Array(Vec::new()));

    // Replacement: one exact change.
    let old = directory.join("replace-old.pdf");
    let new = directory.join("replace-new.pdf");
    let report = directory.join("replace.json.gz");
    write_pdf(
        &old,
        &[
            "Opening context",
            "Release 10 remains available",
            "Closing context",
        ],
    );
    write_pdf(
        &new,
        &[
            "Opening context",
            "Release 20 remains available",
            "Closing context",
        ],
    );
    let output = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&report), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_gzip_json(&report);
    let change = single_flow_change(&json, "replacement");
    assert_eq!(change["old_span"]["text"], "10");
    assert_eq!(change["new_span"]["text"], "20");
    assert!(json["report"].get("assessment").is_none());
    assert!(json["report"].get("not_produced_by_engine").is_none());

    // Paragraph insertion: one exact change.
    let old = directory.join("insert-old.pdf");
    let new = directory.join("insert-new.pdf");
    let insert_report = directory.join("insert.json.gz");
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
    let output = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&insert_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let insert = read_gzip_json(&insert_report);
    let change = single_flow_change(&insert, "insertion");
    assert!(change["old_span"].is_null());
    assert_eq!(
        change["new_span"]["text"],
        "Inserted paragraph contains generic text"
    );

    // Paragraph deletion: one exact change.
    let old = directory.join("delete-old.pdf");
    let new = directory.join("delete-new.pdf");
    let delete_report = directory.join("delete.json.gz");
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
    let output = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&delete_report), "-q"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let delete = read_gzip_json(&delete_report);
    let change = single_flow_change(&delete, "deletion");
    assert_eq!(
        change["old_span"]["text"],
        "Removed paragraph contains generic text"
    );
    assert!(change["new_span"].is_null());
}

#[test]
fn full_json_uses_flow_when_engine_is_omitted() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
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
    let report = directory.join("full.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg("--native-text-only")
        .arg("--full-json")
        .arg("-j")
        .arg(&report)
        .arg("-q")
        .arg(&old)
        .arg(&new)
        .output()
        .expect("implicit flow full comparison");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        !stderr(&output).contains("select the proof engine"),
        "{}",
        stderr(&output)
    );
    let json = read_json(&report);
    assert_eq!(json["artifact_format"], "pdfdelta-native-full");
    assert_eq!(json["report"]["engine"], "flow");
    assert!(
        json["report"]["token_sources"]["old"]
            .as_array()
            .is_some_and(|tokens| !tokens.is_empty())
    );
    assert!(
        json["report"]["glyph_accounting"]["old"]["token_referenced"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
}

#[test]
fn flow_full_and_trace_on_line_wrap_acceptance_fixture() {
    let directory = TestDirectory::new();
    let old = directory.join("acceptance-full-old.pdf");
    let new = directory.join("acceptance-full-new.pdf");
    write_pdf(
        &old,
        &["A detailed release note remains stable across lines"],
    );
    write_pdf(
        &new,
        &["A detailed release note", "remains stable across lines"],
    );
    let report = directory.join("acceptance-full.json.gz");
    let trace = directory.join("acceptance-full-trace.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--native-text-only", "--full-json", "-j"])
        .arg(&report)
        .arg("-q")
        .arg("--trace-json")
        .arg(&trace)
        .arg(&old)
        .arg(&new)
        .output()
        .expect("full acceptance comparison");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = read_gzip_json(&report);
    assert_eq!(json["report"]["summary"]["content_changes"], 0);
    assert_eq!(
        json["report"]["summary"]["difference_status"],
        "no_content_change"
    );
    assert!(
        json["report"]["token_sources"]["old"]
            .as_array()
            .is_some_and(|tokens| !tokens.is_empty())
    );
    let trace = read_json(&trace);
    assert_eq!(trace["result"]["exit_code"], 0);
    assert_eq!(trace["result"]["status"], "completed");
}

#[test]
fn compact_json_is_a_flow_native_alias() {
    let directory = TestDirectory::new();
    let old = directory.join("compact-old.pdf");
    let new = directory.join("compact-new.pdf");
    write_pdf(&old, &["A stable compact paragraph"]);
    write_pdf(&new, &["A stable compact paragraph"]);
    let plain = directory.join("plain.json");
    let explicit = directory.join("explicit.json");
    let first = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&plain), "-q"],
    );
    let second = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "--compact-json",
            "-j",
            path_text(&explicit),
            "-q",
        ],
    );
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(
        fs::read(&plain).expect("plain report"),
        fs::read(&explicit).expect("compact report")
    );
}

#[test]
fn flow_trace_reports_phases_without_changing_report_or_exit() {
    let directory = TestDirectory::new();
    let old = directory.join("trace-old.pdf");
    let new = directory.join("trace-new.pdf");
    write_pdf(&old, &["The fee is 100 dollars."]);
    write_pdf(&new, &["The fee is 200 dollars."]);
    let plain = directory.join("plain.json");
    let traced = directory.join("traced.json");
    let trace = directory.join("trace.json");
    let first = run_default_engine(
        &old,
        &new,
        &["--native-text-only", "-j", path_text(&plain), "-q"],
    );
    let second = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "-j",
            path_text(&traced),
            "-q",
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(first.status.code(), Some(1));
    assert_eq!(
        fs::read(&plain).expect("plain report"),
        fs::read(&traced).expect("traced report")
    );
    let value = read_json(&trace);
    assert_eq!(value["trace_schema"], "pdfdelta-flow-trace/1");
    assert_eq!(value["trace_schema_version"], 1);
    assert_eq!(value["engine"], "flow");
    assert_eq!(value["result"]["exit_code"], 1);
    assert_eq!(
        value["nondeterministic_fields"],
        serde_json::json!(["duration_us"])
    );
    let names = value["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .map(|phase| phase["name"].as_str().expect("phase name"))
        .collect::<Vec<_>>();
    for expected in [
        "input_reading",
        "parse",
        "glyph_extraction",
        "tokenization_normalization",
        "furniture_order",
        "flow_diff",
        "assessment_formatting",
        "report",
    ] {
        assert!(names.contains(&expected), "{expected}: {names:?}");
    }
    assert!(
        value["phases"]
            .as_array()
            .expect("phases")
            .iter()
            .filter(|phase| phase["status"] == "completed")
            .all(|phase| phase.get("duration_us").is_some())
    );
    // A text-only run has no channel phase to run, but it still records the
    // phase as explicitly skipped rather than omitting it.
    let channel = value["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["name"] == "channel_comparison")
        .expect("text-only trace should list channel_comparison");
    assert_eq!(channel["status"], "skipped");
    assert_eq!(channel["skip_reason"], "text_only");

    // An existing trace destination is refused without replacing it.
    let repeated = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "-j",
            path_text(&traced),
            "-q",
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(repeated.status.code(), Some(2));
    assert!(
        stderr(&repeated).contains("refusing to overwrite existing flow trace report"),
        "{}",
        stderr(&repeated)
    );
}

#[test]
fn flow_trace_records_failure_and_skipped_phases() {
    let directory = TestDirectory::new();
    let old = directory.join("trace-failure-old.pdf");
    let new = directory.join("trace-failure-new.pdf");
    write_pdf(&old, &["A valid paragraph"]);
    fs::write(&new, b"this is not a PDF").expect("invalid PDF");
    let report = directory.join("trace-failure.json.gz");
    let trace = directory.join("trace-failure-trace.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--native-text-only", "-q", "-j"])
        .arg(&report)
        .arg("--trace-json")
        .arg(&trace)
        .arg(&old)
        .arg(&new)
        .output()
        .expect("failed comparison trace");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let trace = read_json(&trace);
    assert_eq!(trace["result"]["status"], "failed");
    assert_eq!(trace["result"]["exit_code"], 2);
    let phases = trace["phases"].as_array().expect("phases");
    let parse = phases
        .iter()
        .find(|phase| phase["status"] == "failed" && phase["name"] == "parse")
        .expect("parse failure phase");
    assert_eq!(parse["error"]["kind"], "backend", "{trace}");
    assert!(
        phases.iter().any(|phase| phase["status"] == "skipped"
            && phase["skip_reason"] == "prior_phase_did_not_complete"),
        "{trace}"
    );
}

#[test]
fn flow_trace_reports_an_unsupported_input_from_the_error_class() {
    const SECRET: &str = "trace secret";
    let directory = TestDirectory::new();
    let old = directory.join("trace-unsupported-old.pdf");
    let new = directory.join("trace-unsupported-new.pdf");
    write_encrypted_pdf(&old, &["A paragraph with a secret"], SECRET);
    write_pdf(&new, &["A paragraph without a secret"]);
    let report = directory.join("trace-unsupported.json");
    let trace = directory.join("trace-unsupported-trace.json");
    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "-q",
            "-j",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!stderr(&output).contains(SECRET), "{}", stderr(&output));
    let trace = read_json(&trace);
    let failed = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["status"] == "failed")
        .expect("failed phase");
    assert_eq!(failed["name"], "parse", "{trace}");
    assert_eq!(failed["error"]["kind"], "unsupported", "{trace}");
}

#[test]
fn flow_trace_reports_unresolved_extraction_from_the_issue_kind() {
    let directory = TestDirectory::new();
    let old = directory.join("trace-image-old.pdf");
    let new = directory.join("trace-image-new.pdf");
    image_only_fixture(&old, [255, 0, 0]);
    image_only_fixture(&new, [255, 0, 0]);
    let report = directory.join("trace-image.json");
    let trace = directory.join("trace-image-trace.json");
    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "-q",
            "-j",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let trace = read_json(&trace);
    let incomplete = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .filter(|phase| phase["name"] == "glyph_extraction" && phase["status"] == "incomplete")
        .collect::<Vec<_>>();
    assert_eq!(incomplete.len(), 2, "{trace}");
    for phase in incomplete {
        // The issue is typed unresolved even though its description says the
        // content was "not examined"; no wording classifier is involved.
        assert_eq!(phase["error"]["kind"], "unresolved", "{trace}");
    }
}

#[test]
fn flow_trace_reports_a_limit_exceeded_for_oversized_input() {
    let directory = TestDirectory::new();
    let old = directory.join("trace-oversized-old.pdf");
    let new = directory.join("trace-oversized-new.pdf");
    let oversized = fs::File::create(&old).expect("oversized fixture");
    oversized
        .set_len(256_u64 * 1024 * 1024 + 1)
        .expect("oversized fixture is sparse");
    write_pdf(&new, &["A paragraph within the input limit"]);
    let report = directory.join("trace-oversized.json");
    let trace = directory.join("trace-oversized-trace.json");
    let output = run_default_engine(
        &old,
        &new,
        &[
            "--native-text-only",
            "-q",
            "-j",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let trace = read_json(&trace);
    let failed = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["status"] == "failed")
        .expect("failed phase");
    assert_eq!(failed["name"], "input_reading", "{trace}");
    assert_eq!(failed["side"], "old", "{trace}");
    assert_eq!(failed["error"]["kind"], "limit_exceeded", "{trace}");
    assert_eq!(failed["error"]["resource"], "PDF input bytes", "{trace}");
    assert_eq!(failed["error"]["limit"], 256_u64 * 1024 * 1024, "{trace}");
}

#[test]
fn shared_evidence_trace_reports_unsupported_for_encrypted_input() {
    const SECRET: &str = "shared trace secret";
    let directory = TestDirectory::new();
    let old = directory.join("shared-unsupported-old.pdf");
    let new = directory.join("shared-unsupported-new.pdf");
    write_encrypted_pdf(&old, &["A paragraph with a secret"], SECRET);
    write_pdf(&new, &["A paragraph without a secret"]);
    let report = directory.join("shared-unsupported.json");
    let trace = directory.join("shared-unsupported-trace.json");
    // Selecting only a non-text channel skips the flow text phase, so the
    // failure is produced by compare_documents_with_evidence itself.
    let output = run_default_engine(
        &old,
        &new,
        &[
            "--channels",
            "visual",
            "-q",
            "-j",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!stderr(&output).contains(SECRET), "{}", stderr(&output));
    let trace = read_json(&trace);
    let failed = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["status"] == "failed")
        .expect("failed phase");
    assert_eq!(failed["name"], "channel_comparison", "{trace}");
    assert_eq!(failed["error"]["kind"], "unsupported", "{trace}");
}

#[test]
fn shared_evidence_trace_reports_a_limit_for_oversized_input() {
    let directory = TestDirectory::new();
    let old = directory.join("shared-oversized-old.pdf");
    let new = directory.join("shared-oversized-new.pdf");
    let oversized = fs::File::create(&old).expect("oversized fixture");
    oversized
        .set_len(256_u64 * 1024 * 1024 + 1)
        .expect("oversized fixture is sparse");
    write_pdf(&new, &["A paragraph within the input limit"]);
    let report = directory.join("shared-oversized.json");
    let trace = directory.join("shared-oversized-trace.json");
    let output = run_default_engine(
        &old,
        &new,
        &[
            "--channels",
            "visual",
            "-q",
            "-j",
            path_text(&report),
            "--trace-json",
            path_text(&trace),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let trace = read_json(&trace);
    let failed = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["status"] == "failed")
        .expect("failed phase");
    assert_eq!(failed["name"], "channel_comparison", "{trace}");
    assert_eq!(failed["error"]["kind"], "limit_exceeded", "{trace}");
    assert_eq!(failed["error"]["resource"], "PDF input bytes", "{trace}");
    assert_eq!(failed["error"]["limit"], 256_u64 * 1024 * 1024, "{trace}");
}

#[test]
fn default_channel_trace_reports_the_review_preview_limit() {
    let directory = TestDirectory::new();
    let input = directory.join("many-pages.pdf");
    write_many_page_pdf(&input, 3);
    let review = directory.join("many-pages-review");
    let trace = directory.join("many-pages-trace.json");
    // The default channel set runs flow text first and then the shared
    // evidence review. The debug-only seam lowers the preview bound so the
    // CLI trace path does not need a 1,025-page fixture.
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .env("PDFDELTA_REVIEW_PREVIEW_LIMIT", "2")
        .arg(&input)
        .arg(&input)
        .args([
            "--review",
            path_text(&review),
            "-q",
            "--trace-json",
            path_text(&trace),
        ])
        .output()
        .expect("default review-limit comparison");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let trace = read_json(&trace);
    let failed = trace["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .find(|phase| phase["status"] == "failed")
        .expect("failed phase");
    assert_eq!(failed["name"], "channel_comparison", "{trace}");
    assert_eq!(failed["error"]["kind"], "limit_exceeded", "{trace}");
    assert_eq!(
        failed["error"]["resource"], "static review page previews",
        "{trace}"
    );
    assert_eq!(failed["error"]["limit"], 2, "{trace}");
}

#[test]
fn huge_inline_image_page_is_a_resource_limit_not_a_renderer_abort() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("report.json");
    write_inline_image_pdf(&old);
    write_inline_image_pdf(&new);

    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old)
        .arg(&new)
        .arg("--json")
        .arg(&report)
        .output()
        .expect("inline-image comparison CLI");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = read_json(&report);
    let issues = report["old"]["issues"].as_array().expect("old issues");
    assert!(issues.iter().any(|issue| {
        issue["channel"] == "visual" && issue["kind"] == "resource_limit" && issue["page"] == 0
    }));
    assert!(
        !issues
            .iter()
            .any(|issue| { issue["channel"] == "visual" && issue["kind"] == "backend_failure" })
    );
}

#[test]
fn huge_image_page_is_a_resource_limit_not_a_renderer_abort() {
    let directory = TestDirectory::new();
    let input = directory.join("huge-image.pdf");
    write_huge_image_pdf(&input);
    let report = directory.join("huge-image.json");
    let output = run_default_engine(&input, &input, &["-q", "-j", path_text(&report)]);
    // The renderer would request a multi-gigabyte buffer for this 1-bit mask.
    // The page must be a typed resource limit, not a SIGABRT/backend failure.
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(!stderr(&output).contains("SIGABRT"), "{}", stderr(&output));
    let report = read_json(&report);
    let issues = report["new"]["issues"].as_array().expect("new-side issues");
    let page_issue = issues
        .iter()
        .find(|issue| issue["page"] == 0 && issue["channel"] == "visual")
        .expect("page 1 visual issue");
    assert_eq!(page_issue["kind"], "resource_limit", "{page_issue:#}");
    assert!(
        !issues
            .iter()
            .any(|issue| issue["kind"] == "backend_failure"),
        "{issues:#?}"
    );
}

#[test]
fn flow_trace_with_channels_records_channel_comparison() {
    let directory = TestDirectory::new();
    let old = directory.join("channel-trace-old.pdf");
    let new = directory.join("channel-trace-new.pdf");
    write_pdf(&old, &["A stable channel paragraph"]);
    write_pdf(&new, &["A stable channel paragraph"]);
    let report = directory.join("channels.json");
    let trace = directory.join("channels-trace.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "text", "-q", "-j"])
        .arg(&report)
        .arg("--trace-json")
        .arg(&trace)
        .arg(&old)
        .arg(&new)
        .output()
        .expect("channel trace comparison");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = read_json(&report);
    assert_eq!(report["engine"], "flow");
    let trace = read_json(&trace);
    assert_eq!(trace["trace_schema"], "pdfdelta-flow-trace/1");
    assert!(
        trace["phases"]
            .as_array()
            .expect("phases")
            .iter()
            .any(|phase| phase["name"] == "channel_comparison" && phase["status"] == "completed"),
        "{trace}"
    );
}

#[test]
fn full_flow_report_accounts_for_every_visible_glyph() {
    let directory = TestDirectory::new();
    let old = directory.join("full-old.pdf");
    let new = directory.join("full-new.pdf");
    write_pdf(&old, &["A full report paragraph"]);
    write_pdf(&new, &["A full report paragraph"]);
    let report = directory.join("full.json.gz");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--native-text-only", "--full-json", "-j"])
        .arg(&report)
        .arg("-q")
        .arg(&old)
        .arg(&new)
        .output()
        .expect("full flow comparison");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = read_gzip_json(&report);
    assert_eq!(json["artifact_format"], "pdfdelta-native-full");
    let report = &json["report"];
    for side in ["old", "new"] {
        let accounting = &report["glyph_accounting"][side];
        let excluded = accounting["excluded"].as_array().expect("excluded");
        let counts = &accounting["excluded_counts"];
        assert_eq!(
            accounting["visible"].as_u64().expect("visible"),
            accounting["token_referenced"].as_u64().expect("referenced")
                + counts["furniture"].as_u64().expect("furniture")
                + counts["whitespace"].as_u64().expect("whitespace")
                + counts["shared"].as_u64().expect("shared")
        );
        assert_eq!(
            excluded.len(),
            counts["furniture"].as_u64().expect("furniture") as usize
                + counts["whitespace"].as_u64().expect("whitespace") as usize
                + counts["invisible"].as_u64().expect("invisible") as usize
                + counts["shared"].as_u64().expect("shared") as usize
        );
        assert!(excluded.iter().all(|glyph| {
            matches!(
                glyph["reason"].as_str(),
                Some("furniture" | "whitespace" | "invisible" | "shared_source")
            )
        }));
        let tokens = report["token_sources"][side]
            .as_array()
            .expect("token sources");
        assert!(tokens.iter().all(|token| {
            token["sources"]
                .as_array()
                .is_some_and(|sources| !sources.is_empty())
        }));
        let mut seen = std::collections::BTreeSet::new();
        for token in tokens {
            for source in token["sources"].as_array().expect("source") {
                assert!(
                    seen.insert(source["glyph_id"].as_u64().expect("glyph id")),
                    "a glyph source is referenced by more than one token: {side}"
                );
            }
        }
        assert_eq!(
            seen.len(),
            accounting["token_referenced"].as_u64().expect("referenced") as usize
                + counts["shared"].as_u64().expect("shared") as usize
        );
    }
}

#[test]
fn flow_engine_supports_report_destinations_and_quiet() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(
        &old,
        &[
            "Opening context",
            "Release 10 remains available",
            "Closing context",
        ],
    );
    write_pdf(
        &new,
        &[
            "Opening context",
            "Release 20 remains available",
            "Closing context",
        ],
    );
    for (label, extra, flow) in [
        ("flow", vec!["--engine", "flow"], true),
        ("default", Vec::new(), true),
    ] {
        let text = directory.join(&format!("{label}.txt"));
        let report = directory.join(&format!("{label}.json.gz"));
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .arg("--native-text-only")
            .args(extra)
            .arg(&old)
            .arg(&new)
            .args(["-q", "-o", path_text(&text), "-j", path_text(&report)])
            .output()
            .expect("native comparison");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label}: {}",
            stderr(&output)
        );
        assert!(output.stdout.is_empty(), "{label} quiet stdout");
        assert_eq!(
            fs::read_to_string(&text)
                .expect("text report")
                .contains("flow summary"),
            flow,
            "{label} text report"
        );
        assert!(report.is_file(), "{label} JSON report");
        let json = read_gzip_json(&report);
        assert_eq!(
            json["report"].get("engine").and_then(Value::as_str),
            flow.then_some("flow")
        );
    }
}

#[test]
fn flow_engine_channels_text_uses_flow_and_keeps_channel_report() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(
        &old,
        &[
            "Opening context",
            "Release 10 remains available",
            "Closing context",
        ],
    );
    write_pdf(
        &new,
        &[
            "Opening context",
            "Release 20 remains available",
            "Closing context",
        ],
    );
    for (label, explicit) in [("flow", true), ("default", false)] {
        let report = directory.join(&format!("channels-{label}.json"));
        let arguments = ["--channels", "text", "-j", path_text(&report), "-q"];
        let output = if explicit {
            run_flow_engine(&old, &new, &arguments)
        } else {
            run_default_engine(&old, &new, &arguments)
        };
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label}: {}",
            stderr(&output)
        );
        let json = read_json(&report);
        assert_eq!(json["engine"], "flow", "{label}");
        assert_eq!(json["comparison_complete"], true, "{label}");
        assert_eq!(json["flow_text"]["complete"], true, "{label}");
        assert_eq!(
            json["flow_text"]["summary"]["content_changes"], 1,
            "{label}"
        );
        assert!(
            json["coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .any(|coverage| coverage["channel"] == "text" && coverage["complete"] == true),
            "{label}"
        );
    }
}

/// Replaces text inside a fixture's single content stream.
fn rewrite_content(path: &Path, from: &str, to: &str) {
    let mut pdf = Document::load(path).expect("load fixture for rewrite");
    let page = *pdf.get_pages().values().next().expect("fixture has a page");
    let contents = pdf
        .get_object(page)
        .expect("page")
        .as_dict()
        .expect("page dictionary")
        .get(b"Contents")
        .expect("page contents")
        .as_reference()
        .expect("contents reference");
    let stream = pdf
        .get_object_mut(contents)
        .expect("contents stream")
        .as_stream_mut()
        .expect("content stream");
    let content = String::from_utf8(stream.content.clone())
        .expect("fixture content is UTF-8")
        .replace(from, to);
    stream.set_content(content.into_bytes());
    pdf.save(path).expect("rewritten fixture saves");
}

#[test]
fn flow_review_links_changes_formatting_and_verdicts_to_its_json() {
    let directory = TestDirectory::new();
    let old = directory.join("flow-review-old.pdf");
    let new = directory.join("flow-review-new.pdf");
    write_pdf(
        &old,
        &["A stable heading.", "The reported total is 10 units."],
    );
    write_pdf(
        &new,
        &["A stable heading.", "The reported total is 20 units."],
    );
    // The heading keeps its text and changes size only: a formatting entry,
    // not a content change.
    rewrite_content(&new, "10 Tf", "14 Tf");
    let review = directory.join("review");
    let report = directory.join("report.json");
    let plain = directory.join("plain.json");
    let plain_output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old)
        .arg(&new)
        .args(["--channels", "text", "--json"])
        .arg(&plain)
        .arg("-q")
        .output()
        .expect("plain flow comparison");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old)
        .arg(&new)
        .args(["--channels", "text", "--review"])
        .arg(&review)
        .args(["--json"])
        .arg(&report)
        .arg("-q")
        .output()
        .expect("flow review");
    assert_eq!(output.status.code(), plain_output.status.code());
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let mut plain_json: Value =
        serde_json::from_slice(&fs::read(&plain).expect("plain report")).expect("JSON");
    let mut reviewed_json: Value =
        serde_json::from_slice(&fs::read(&report).expect("report")).expect("JSON");
    plain_json["comparison_wall_time_ms"] = Value::Null;
    reviewed_json["comparison_wall_time_ms"] = Value::Null;
    assert_eq!(
        plain_json, reviewed_json,
        "export must not alter the report"
    );
    let report: Value = serde_json::from_slice(&fs::read(&report).expect("report")).expect("JSON");
    let html = fs::read_to_string(review.join("index.html")).expect("HTML review");
    assert!(html.contains("id=\"flow-text\""));
    assert!(
        html.contains("/flow_text/changes/0"),
        "the content change links its JSON entry: {html}"
    );
    assert_review_pointers(&html, &report);
    assert!(html.contains("old.pdf#page=1") && html.contains("new.pdf#page=1"));
    assert!(
        report["flow_text"]["formatting_only_changes"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()),
        "the font-size change is a formatting entry: {report}"
    );
    assert!(html.contains("flow-formatting-0"));
    assert!(html.contains("/flow_text/formatting_only_changes/0"));
    assert!(html.contains("/channel_verdicts/0"));
    // A flow review stores the much larger source index compressed, and the
    // HTML links that artifact rather than an absent sources.json.
    assert!(!review.join("sources.json").exists());
    let sources = fs::read(review.join("sources.json.gz")).expect("compressed sources");
    assert_eq!(&sources[..2], &[0x1f, 0x8b]);
    // The current writer keeps the inner gzip member for size; the
    // compatibility reader accepts both the nested and single-stream shapes.
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(sources.as_slice()),
        &mut decoded,
    )
    .expect("outer gzip layer");
    if decoded.starts_with(&[0x1f, 0x8b]) {
        let inner = std::mem::take(&mut decoded);
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(inner.as_slice()),
            &mut decoded,
        )
        .expect("inner gzip layer");
    }
    let index: Value = serde_json::from_slice(&decoded).expect("source index JSON");
    assert_eq!(index["version"], 1);
    assert!(html.contains("sources.json.gz"));
    assert!(
        !html.contains("<script") && !html.contains("http://") && !html.contains("https://"),
        "the static bundle neither runs script nor reaches the network"
    );
}

#[test]
fn flow_engine_publishes_review_bundles_without_changing_the_exit() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);
    let agent = directory.join("agent-review");
    let html = directory.join("html-review");
    let agent_output = run_flow_engine(
        &old,
        &new,
        &[
            "--channels",
            "text",
            "--agent-review",
            path_text(&agent),
            "-q",
        ],
    );
    let html_output = run_flow_engine(
        &old,
        &new,
        &["--channels", "text", "--review", path_text(&html), "-q"],
    );
    // An equal comparison exits 0 with or without a bundle.
    assert_eq!(
        agent_output.status.code(),
        Some(0),
        "{}",
        stderr(&agent_output)
    );
    assert_eq!(
        html_output.status.code(),
        Some(0),
        "{}",
        stderr(&html_output)
    );
    let manifest = read_json(&agent.join("manifest.json"));
    assert_eq!(manifest["pipeline"], "shared_evidence");
    assert!(
        manifest["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability["detail"] == "text" && capability["available"] == true)
    );
    let index = fs::read_to_string(html.join("index.html")).expect("flow HTML review");
    assert!(
        index.contains("id=\"flow-text\"") && index.contains("/flow_text/summary"),
        "flow review should link its own JSON section"
    );
    assert!(index.contains("/channel_verdicts/0"));
}
