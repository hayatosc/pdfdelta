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

    // The warm run must actually serve both sides from the cache.
    let trace = read_json(&warm_trace);
    for side in ["old", "new"] {
        let parse = phase(&trace, "pdf_parse", Some(side));
        assert_eq!(parse["status"], "skipped");
        assert_eq!(parse["skip_reason"], "extraction_cache_hit");
    }
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

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_complete_json_report(&report, 1, Some("replacement"));
    let report_json = read_json(&report);
    let old_text = report_json["changes"][0]["occurrences"][0]["old_span"]["text"]
        .as_str()
        .expect("replacement old span should carry resolved text");
    let new_text = report_json["changes"][0]["occurrences"][0]["new_span"]["text"]
        .as_str()
        .expect("replacement new span should carry resolved text");
    // Exact-diff semantics keep the change span minimal: only the differing
    // digit is reported as changed content.
    assert_eq!(old_text, "1");
    assert_eq!(new_text, "2");
    assert_eq!(
        report_json["changes"][0]["occurrences"][0]["old_span"]["pages"][0],
        0
    );
    assert_eq!(
        report_json["changes"][0]["occurrences"][0]["new_span"]["pages"][0],
        0
    );
    let source = &report_json["changes"][0]["occurrences"][0]["old_span"]["sources"][0];
    assert_eq!(source["kind"], "glyph");
    assert!(source["glyph_id"].is_u64());
    assert!(source["bbox"]["min"]["x"].is_number());
    assert!(source["bbox"]["min"]["y"].is_number());
    assert!(source["bbox"]["max"]["x"].is_number());
    assert!(source["bbox"]["max"]["y"].is_number());
    assert!(source["content_stream"]["object_number"].is_u64());
    assert!(source["content_stream"]["generation"].is_u64());
    assert!(source["operator_index"].is_u64());
}

#[test]
fn replacement_report_renders_a_unified_diff_hunk() {
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

    // Piped output defaults to `auto`, so no ANSI escapes may appear even
    // though the report carries -/+ markers and context.
    let output = compare(&old, &new, &[]);
    let report = stdout(&output);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(report.contains("content changes: 1 "), "{report}");
    assert!(
        report.contains(&format!("--- {}", path_text(&old))),
        "{report}"
    );
    assert!(
        report.contains(&format!("+++ {}", path_text(&new))),
        "{report}"
    );
    // Human-facing page numbers are one-based.
    assert!(
        report.contains("@@ page 1 · old block 1 -> new block 1 · confidence: medium @@"),
        "{report}"
    );
    assert!(
        report.contains("- Release 10 remains available"),
        "{report}"
    );
    assert!(
        report.contains("+ Release 20 remains available"),
        "{report}"
    );
    assert!(!report.contains("\u{1b}["), "{report}");
}

#[test]
fn insertion_and_deletion_render_single_sided_hunks() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
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
            "Inserted paragraph contains generic text",
            "Closing paragraph remains stable",
        ],
    );

    let output = compare(&old, &new, &[]);
    let report = stdout(&output);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    // Both scalar-level exact changes share one block pair, so they coalesce
    // into one presentation hunk with adjacent single-sided markers and
    // bounded context.
    assert!(
        report.contains("- Removed paragraph contains generic te ..."),
        "{report}"
    );
    assert!(
        report.contains("+ Inserted paragraph contains generic te ..."),
        "{report}"
    );
}

#[test]
fn color_always_colorizes_piped_output_and_never_disables_it() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    // Context paragraphs give the aligner exact anchors.
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

    let always = compare(&old, &new, &["--color", "always"]);
    let always_report = stdout(&always);
    assert_eq!(always.status.code(), Some(1), "{}", stderr(&always));
    assert!(
        always_report.contains("\u{1b}[1;36m@@ page 1"),
        "{always_report}"
    );
    assert!(
        always_report.contains("\u{1b}[31m- Release"),
        "{always_report}"
    );
    assert!(
        always_report.contains("\u{1b}[32m+ Release"),
        "{always_report}"
    );

    let never = compare(&old, &new, &["--color", "never"]);
    assert_eq!(never.status.code(), Some(1), "{}", stderr(&never));
    assert!(!stdout(&never).contains("\u{1b}["), "{:#}", stdout(&never));
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
    assert_complete_json_report(&report, 1, Some("replacement"));
    let trace_json = fs::read_to_string(&trace).expect("trace report should be readable");
    assert!(
        !trace_json.contains(SECRET),
        "the trace leaked the password"
    );
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

    // Default mode reports the unsupported side and returns the incomplete status.
    let default_output = compare(&old, &new, &arguments);
    assert_eq!(
        default_output.status.code(),
        Some(3),
        "{}",
        stderr(&default_output)
    );
    let message = stderr(&default_output);
    assert!(message.contains("password"), "{message}");
    assert!(!message.contains(SECRET), "{message}");
    assert!(!message.contains(WRONG), "{message}");

    // Strict mode refuses the incomplete comparison with exit code 3.
    let mut strict_arguments = arguments.to_vec();
    strict_arguments.push("--strict");
    let strict_output = compare(&old, &new, &strict_arguments);
    assert_eq!(strict_output.status.code(), Some(3));
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
    let report_json = read_json(&report);
    assert_eq!(
        report_json["changes"][0]["occurrences"][0]["new_span"]["text"],
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

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_complete_json_report(&report, 1, Some("deletion"));
    let report_json = read_json(&report);
    assert_eq!(
        report_json["changes"][0]["occurrences"][0]["old_span"]["text"],
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

    let output = compare(&old, &new, &["--json", path_text(&report)]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let json = fs::read_to_string(report).expect("JSON report should be readable");
    assert!(json.contains("\"schema_version\": 11"));
    assert!(json.contains("\"content_changes\": 0"));
    assert_no_temporary_reports(&directory);
}

#[test]
fn writes_complete_phase_trace_separately_from_the_report() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report = directory.join("comparison.json");
    let trace = directory.join("trace.json");
    write_pdf(&old, &["A generic paragraph remains stable"]);
    write_pdf(&new, &["A generic paragraph remains stable"]);

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

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let trace = read_json(&trace);
    assert_eq!(trace["trace_schema_version"], 26);
    assert_eq!(trace["command"]["kind"], "compare");
    assert_eq!(trace["result"]["status"], "completed");
    assert_eq!(trace["result"]["exit_code"], 0);
    assert!(
        phase(&trace, "input_read", Some("old"))["metrics"]["input_bytes"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        phase(&trace, "line_reconstruction", Some("old"))["status"],
        "completed"
    );
    assert_eq!(phase(&trace, "alignment", None)["status"], "completed");
    let exact_diff = phase(&trace, "exact_diff", None);
    assert_eq!(exact_diff["status"], "completed");
    assert!(
        exact_diff["metrics"]["sentence_recovery_near_pair_visits_examined"]
            .as_u64()
            .is_some()
    );
    assert!(
        exact_diff["metrics"]["sentence_recovery_near_similarity_comparisons_attempted"]
            .as_u64()
            .is_some()
    );
    assert!(
        exact_diff["metrics"]["sentence_recovery_near_candidate_posting_visits_examined"]
            .as_u64()
            .is_some()
    );
    assert!(
        exact_diff["metrics"]["sentence_recovery_near_candidate_posting_visits_attempted"]
            .as_u64()
            .is_some()
    );
    for key in [
        "sentence_recovery_sentence_edge_filter_complete",
        "sentence_recovery_sentence_edge_filter_pairs_examined",
        "sentence_recovery_sentence_edge_filter_pairs_attempted",
        "sentence_recovery_sentence_edge_filter_similarity_comparisons_examined",
        "sentence_recovery_sentence_edge_filter_similarity_comparisons_attempted",
        "sentence_recovery_sentence_edge_filter_pairs_retained",
        "sentence_recovery_sentence_edge_filter_pairs_rejected",
        "sentence_recovery_sentence_edge_filter_stop_reason_pair_visit_limit",
        "sentence_recovery_sentence_edge_filter_stop_reason_similarity_comparison_limit",
        "sentence_recovery_sentence_edge_filter_stop_reason_allocation_failure",
        "sentence_recovery_sentence_edge_filter_stop_reason_counter_overflow",
        "sentence_recovery_sentence_edge_filter_full_build_fallback_used",
        "sentence_recovery_sentence_edge_filter_discarded_near_pair_visits_examined",
        "sentence_recovery_sentence_edge_filter_discarded_near_pair_visits_attempted",
        "sentence_recovery_sentence_edge_filter_discarded_near_similarity_comparisons_examined",
        "sentence_recovery_sentence_edge_filter_discarded_near_similarity_comparisons_attempted",
        "sentence_recovery_sentence_edge_filter_discarded_near_candidate_posting_visits_examined",
        "sentence_recovery_sentence_edge_filter_discarded_near_candidate_posting_visits_attempted",
        "sentence_recovery_sentence_edge_gate_shadow_complete",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_candidate_posting_visit_limit",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_pair_visit_limit",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_similarity_comparison_limit",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_candidate_count_limit",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_allocation_failure",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_counter_overflow",
        "sentence_recovery_sentence_edge_gate_shadow_stop_reason_diagnostic_failure",
        "sentence_recovery_sentence_edge_gate_shadow_pairs_considered",
        "sentence_recovery_sentence_edge_gate_shadow_pairs_rejected",
    ] {
        assert!(
            exact_diff["metrics"][key].as_u64().is_some(),
            "missing edge-gate trace metric {key}"
        );
    }
    for kind in ["sentence", "line"] {
        for field in [
            "edge_posting_visits_examined",
            "edge_posting_visits_attempted",
            "line_trigram_posting_visits_examined",
            "line_trigram_posting_visits_attempted",
            "edge_query_union_candidates",
            "line_trigram_only_query_union_candidates",
            "filtered_candidates",
            "pair_visits_examined",
            "pair_visits_attempted",
            "similarity_comparisons_examined",
            "similarity_comparisons_attempted",
        ] {
            let key = format!("sentence_recovery_near_{kind}_work_{field}");
            assert!(exact_diff["metrics"][&key].as_u64().is_some(), "{key}");
        }
    }
    let scopes = [
        "near_paired_interval_work",
        "near_paired_cross_interval_veto_work",
        "near_same_or_ambiguous_span_work",
        "near_same_known_span_work",
        "near_ambiguous_span_work",
        "near_same_or_ambiguous_shared_query_work",
        "near_cross_span_work",
    ];
    let fields = [
        "edge_posting_visits_examined",
        "edge_posting_visits_attempted",
        "line_trigram_posting_visits_examined",
        "line_trigram_posting_visits_attempted",
        "edge_query_union_candidates",
        "line_trigram_only_query_union_candidates",
        "filtered_candidates",
        "pair_visits_examined",
        "pair_visits_attempted",
        "similarity_comparisons_examined",
        "similarity_comparisons_attempted",
    ];
    let mut expected_scope_keys = scopes
        .iter()
        .flat_map(|scope| {
            fields.iter().flat_map(move |field| {
                ["sentence", "line"]
                    .map(|kind| format!("sentence_recovery_{scope}_{kind}_work_{field}"))
            })
        })
        .collect::<Vec<_>>();
    expected_scope_keys.sort();
    let mut actual_scope_keys = exact_diff["metrics"]
        .as_object()
        .expect("exact-diff metrics object")
        .keys()
        .filter(|key| {
            scopes
                .iter()
                .any(|scope| key.starts_with(&format!("sentence_recovery_{scope}_")))
        })
        .cloned()
        .collect::<Vec<_>>();
    actual_scope_keys.sort();
    assert_eq!(actual_scope_keys, expected_scope_keys);
    assert_eq!(
        exact_diff["metrics"]["sentence_recovery_near_candidate_count_truncated"],
        0
    );
    assert_eq!(
        exact_diff["metrics"]["sentence_recovery_near_relation_stop_reason_candidate_posting_visit_limit"],
        0
    );
    assert_eq!(
        exact_diff["metrics"]["sentence_recovery_near_relation_stop_reason_pair_visit_limit"],
        0
    );
    assert_eq!(
        exact_diff["metrics"]["sentence_recovery_near_relation_stop_reason_similarity_comparison_limit"],
        0
    );
    assert_eq!(
        exact_diff["metrics"]["sentence_recovery_near_relation_stop_reason_candidate_count_limit"],
        0
    );
    assert_eq!(phase(&trace, "report", None)["status"], "completed");
    assert_no_temporary_reports(&directory);
}

#[test]
fn trace_records_candidate_visit_metrics_on_the_alignment_phase() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let trace = directory.join("trace.json");
    write_pdf(&old, &["Stable old paragraph remains visible"]);
    write_pdf(&new, &["Stable new paragraph remains visible"]);

    let output = compare(&old, &new, &["--trace-json", path_text(&trace)]);

    // Different paragraphs produce a content change, so the command exits 1
    // while the trace still records the completed alignment phase.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let trace = read_json(&trace);
    let alignment = phase(&trace, "alignment", None);
    assert_eq!(alignment["status"], "completed");
    let visits = alignment["metrics"]["candidate_visits"]
        .as_u64()
        .expect("candidate visits should be recorded");
    assert!(visits > 0, "non-anchor old blocks must be charged");
    assert_eq!(
        alignment["metrics"]["candidate_visits_required"], visits,
        "attempted charge must equal the required sum on success"
    );
    let exact = alignment["metrics"]["candidate_visits_required_exact"]
        .as_u64()
        .expect("inverted index reports an exact component");
    let ngram = alignment["metrics"]["candidate_visits_required_ngram"]
        .as_u64()
        .expect("inverted index reports an ngram component");
    let short_fallback = alignment["metrics"]["candidate_visits_required_short_fallback"]
        .as_u64()
        .expect("inverted index reports a short fallback component");
    assert_eq!(
        exact + ngram + short_fallback,
        visits,
        "required components must sum to the required total"
    );
    assert_eq!(alignment["metrics"]["max_candidate_visits"], 1_000_000);
}

#[test]
fn concurrent_extraction_matches_sequential_phases_when_the_old_side_fails() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let trace_path = directory.join("trace.json");
    fs::write(&old, b"not a PDF").expect("malformed fixture should be written");
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &["--trace-json", path_text(&trace_path)]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let trace = read_json(&trace_path);
    // The sides extract concurrently, but a failed old side must discard the
    // new side's phase records so the trace matches the sequential run where
    // the new side never ran: new-side entries may only be the
    // `prior_phase_did_not_complete` skip placeholders, and the old side's
    // error still wins.
    let phases = trace["phases"].as_array().expect("trace phases");
    assert!(
        phases
            .iter()
            .all(|phase| phase["side"] != Value::String("new".to_owned())
                || phase["status"] == "skipped"),
        "{phases:#?}"
    );
    assert_eq!(phase(&trace, "pdf_parse", Some("old"))["status"], "failed");
}

#[test]
fn writes_failure_trace_when_pdf_parsing_stops_the_pipeline() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let trace = directory.join("trace.json");
    fs::write(&old, b"not a PDF").expect("malformed fixture should be written");
    write_pdf(&new, &["A generic paragraph remains stable"]);

    let output = compare(&old, &new, &["--trace-json", path_text(&trace)]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let trace = read_json(&trace);
    assert_eq!(trace["result"]["status"], "failed");
    assert_eq!(trace["result"]["exit_code"], 2);
    let parse = phase(&trace, "pdf_parse", Some("old"));
    assert_eq!(parse["status"], "failed");
    assert_eq!(parse["error"]["kind"], "backend");
    assert_eq!(
        phase(&trace, "glyph_extraction", Some("old"))["status"],
        "skipped"
    );
    assert_eq!(phase(&trace, "alignment", None)["status"], "skipped");
    assert_no_temporary_reports(&directory);
}

#[test]
fn traces_incomplete_extraction_without_calling_it_a_fatal_failure() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let malformed = directory.join("malformed-type0.pdf");
    let trace = directory.join("trace.json");
    write_pdf(&old, &["Complete page text remains visible"]);
    write_type0_pdf(&malformed, &["Malformed page text is present"]);

    let output = compare(
        &old,
        &malformed,
        &["--strict", "--trace-json", path_text(&trace)],
    );

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let trace = read_json(&trace);
    assert_eq!(trace["result"]["status"], "incomplete");
    assert_eq!(trace["result"]["exit_code"], 3);
    assert_eq!(
        phase(&trace, "glyph_extraction", Some("new"))["status"],
        "incomplete"
    );
    assert_eq!(
        phase(&trace, "completeness_gate", None)["status"],
        "incomplete"
    );
    let alignment = phase(&trace, "alignment", None);
    assert_eq!(alignment["status"], "completed");
    assert_eq!(alignment["metrics"]["candidate_visits"], 0);
    let exact_diff = phase(&trace, "exact_diff", None);
    assert_eq!(exact_diff["status"], "completed");
    assert_eq!(exact_diff["metrics"]["changes"], 0);
    assert_eq!(exact_diff["metrics"]["unresolved_regions"], 1);
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
    assert!(error.contains("refusing to overwrite existing trace report"));
    assert!(!error.contains("cannot inspect input path"));
    assert!(report.exists());
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
    assert!(error.contains("old PDF"), "{error}");
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
    assert!(error.contains("old PDF"), "{error}");
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

    assert_eq!(default_output.status.code(), Some(3), "{default_stderr}");
    assert!(default_stderr.contains("extraction issue for old PDF"));
    assert!(default_stderr.contains("kind=unresolved"));
    assert!(default_stderr.contains("Type0 font has no Encoding"));
    assert!(
        default_report.contains("content changes: 0 "),
        "{default_report}"
    );
    assert!(
        default_report.contains("extraction incomplete: old=no, new=yes"),
        "{default_report}"
    );
    assert!(
        default_report.contains(
            "! extraction issue (side=old, kind=unresolved, scope=page, page=1): Type0 font has no Encoding"
        ),
        "{default_report}"
    );
    assert!(
        default_report.contains("coverage unknown"),
        "{default_report}"
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
    assert_eq!(report["schema_version"], 11);
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

    let default_output = compare(&old, &new, &[]);
    assert_eq!(
        default_output.status.code(),
        Some(3),
        "{}",
        stderr(&default_output)
    );
    let text = stdout(&default_output);
    assert!(text.contains("content changes: 1"), "{text}");
    assert!(
        text.contains("scope=page-gap, retained-pages-before=1"),
        "{text}"
    );

    let strict_output = compare(&old, &new, &["--strict", "--json", path_text(&report_path)]);
    assert_eq!(
        strict_output.status.code(),
        Some(3),
        "{}",
        stderr(&strict_output)
    );
    let report: Value = serde_json::from_slice(
        &fs::read(report_path).expect("page-gap JSON report should be readable"),
    )
    .expect("page-gap JSON report should be valid");
    assert_eq!(report["schema_version"], 11);
    assert_eq!(report["summary"]["content_changes"], 1);
    assert_eq!(report["summary"]["unresolved_regions"], 1);
    assert_eq!(report["extraction"]["issues"][0]["scope"], "page_gap");
    assert_eq!(
        report["extraction"]["issues"][0]["retained_pages_before"],
        1
    );
    assert_eq!(
        report["unresolved_regions"][0]["evidence"][0],
        "extraction_gap"
    );
}

#[test]
fn strict_unresolved_comparison_exits_three() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let trace_path = directory.join("trace.json");
    write_pdf(&old, &["The archive contains echo echo files"]);
    write_pdf(&new, &["The archive contains echo echo echo files"]);

    let output = compare(
        &old,
        &new,
        &["--strict", "--trace-json", path_text(&trace_path)],
    );

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("unresolved regions: 1"), "{report}");
    assert!(report.contains("@@ page 1 · UNRESOLVED @@"), "{report}");
    assert!(
        report.contains("? could not safely align this region"),
        "{report}"
    );
    let trace = read_json(&trace_path);
    assert_eq!(trace["result"]["status"], "incomplete");
    assert_eq!(trace["result"]["exit_code"], 3);
    assert_eq!(phase(&trace, "exact_diff", None)["status"], "completed");
}

#[test]
fn strict_unique_numeric_replacement_exits_one() {
    let directory = TestDirectory::new();
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    let report_path = directory.join("report.json");
    write_pdf(&old, &["The archive contains 10 files"]);
    write_pdf(&new, &["The archive contains 20 files"]);

    let output = compare(&old, &new, &["--strict", "--json", path_text(&report_path)]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = read_json(&report_path);
    assert_eq!(report["summary"]["content_changes"], 1);
    assert_eq!(report["summary"]["unresolved_regions"], 0);
    assert_eq!(report["changes"][0]["kind"], "replacement");
    let occurrence = &report["changes"][0]["occurrences"][0];
    assert_eq!(occurrence["old_span"]["text"], "1");
    assert_eq!(occurrence["new_span"]["text"], "2");
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
    let trace = read_json(&trace_path);
    assert_eq!(trace["result"]["status"], "failed");
    assert_eq!(phase(&trace, "output_validation", None)["status"], "failed");
    assert_eq!(
        phase(&trace, "input_read", Some("old"))["status"],
        "skipped"
    );
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
        report_content.contains("- Release 10 remains available"),
        "{report_content}"
    );
    assert!(
        report_content.contains("+ Release 20 remains available"),
        "{report_content}"
    );
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
        old_pdf.exists(),
        "vendored old.pdf must exist at {}",
        old_pdf.display()
    );
    assert!(
        new_pdf.exists(),
        "vendored new.pdf must exist at {}",
        new_pdf.display()
    );

    let directory = TestDirectory::new();
    let json_path = directory.join("report.json");

    // 1. Text report to stdout
    let text_output = compare(&old_pdf, &new_pdf, &[]);
    assert_eq!(
        text_output.status.code(),
        Some(1),
        "{}",
        stderr(&text_output)
    );
    let stdout_text = stdout(&text_output);
    assert!(stdout_text.contains("content changes: 1"), "{stdout_text}");
    assert!(stdout_text.contains("formatting-only: 0"), "{stdout_text}");
    assert!(stdout_text.contains("uncertain: 0"), "{stdout_text}");
    assert!(
        stdout_text.contains("unresolved regions: 0"),
        "{stdout_text}"
    );
    assert!(stdout_text.contains("coverage 100.0%"), "{stdout_text}");
    assert!(stdout_text.contains("- Release 10"), "{stdout_text}");
    assert!(stdout_text.contains("+ Release 20"), "{stdout_text}");

    // 2. Structured JSON report
    let json_output = compare(&old_pdf, &new_pdf, &["-j", path_text(&json_path)]);
    assert_eq!(
        json_output.status.code(),
        Some(1),
        "{}",
        stderr(&json_output)
    );

    let json_text = fs::read_to_string(&json_path).expect("JSON report should be readable");
    let report: serde_json::Value =
        serde_json::from_str(&json_text).expect("JSON report should parse");

    assert_eq!(report["schema_version"], 11);
    assert_eq!(report["summary"]["content_changes"], 1);
    assert_eq!(report["summary"]["formatting_only_changes"], 0);
    assert_eq!(report["summary"]["uncertain_changes"], 0);
    assert_eq!(report["summary"]["unresolved_regions"], 0);
    assert_eq!(report["summary"]["unsupported_extraction_issues"], 0);
    assert_eq!(report["summary"]["unresolved_extraction_issues"], 0);
    assert_eq!(report["summary"]["comparison_complete"], true);
    assert_eq!(report["summary"]["old_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(report["summary"]["new_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(report["summary"]["comparison_coverage_ratio"], 1.0);

    let changes = report["changes"]
        .as_array()
        .expect("changes should be array");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["kind"], "replacement");
    assert_eq!(changes[0]["occurrences"][0]["old_span"]["text"], "1");
    assert_eq!(changes[0]["occurrences"][0]["new_span"]["text"], "2");

    assert_eq!(report["extraction"]["old_complete"], true);
    assert_eq!(report["extraction"]["new_complete"], true);
    assert_eq!(
        report["extraction"]["issues"].as_array().map(Vec::len),
        Some(0)
    );

    // Strict mode must succeed with exit code 1 rather than exit code 3 (incomplete).
    let strict_output = compare(&old_pdf, &new_pdf, &["--strict"]);
    assert_eq!(
        strict_output.status.code(),
        Some(1),
        "strict mode should accept complete comparison: {}",
        stderr(&strict_output)
    );
}

#[test]
fn externally_rendered_typst_japanese_revision_pair_reports_exact_replacement() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");

    assert!(
        old_pdf.exists(),
        "vendored old.pdf must exist at {}",
        old_pdf.display()
    );
    assert!(
        new_pdf.exists(),
        "vendored new.pdf must exist at {}",
        new_pdf.display()
    );

    let directory = TestDirectory::new();
    let json_path = directory.join("report.json");

    // 1. Text report to stdout
    let text_output = compare(&old_pdf, &new_pdf, &[]);
    assert_eq!(
        text_output.status.code(),
        Some(1),
        "{}",
        stderr(&text_output)
    );
    let stdout_text = stdout(&text_output);
    assert!(stdout_text.contains("content changes: 1"), "{stdout_text}");
    assert!(stdout_text.contains("formatting-only: 0"), "{stdout_text}");
    assert!(stdout_text.contains("uncertain: 0"), "{stdout_text}");
    assert!(
        stdout_text.contains("unresolved regions: 0"),
        "{stdout_text}"
    );
    assert!(stdout_text.contains("coverage 100.0%"), "{stdout_text}");
    assert!(stdout_text.contains("@@ page 1 ·"), "{stdout_text}");
    assert!(
        stdout_text.contains("- 第10版の運用手順書を引き続き適用します。"),
        "{stdout_text}"
    );
    assert!(
        stdout_text.contains("+ 第20版の運用手順書を引き続き適用します。"),
        "{stdout_text}"
    );

    // 2. Glyph inspection asserting 87 mapped horizontal glyphs per side
    let inspect_old = inspect(&old_pdf, &["--glyphs"]);
    assert_eq!(inspect_old.status.code(), Some(0));
    let inspect_old_text = stdout(&inspect_old);
    assert!(
        inspect_old_text.contains("glyphs: 87"),
        "{inspect_old_text}"
    );
    assert!(
        inspect_old_text.contains("text=\"定\""),
        "{inspect_old_text}"
    );
    assert!(
        inspect_old_text.contains("text=\"1\""),
        "{inspect_old_text}"
    );
    assert!(
        inspect_old_text.contains("direction=(1,0)"),
        "{inspect_old_text}"
    );
    assert!(
        !inspect_old_text.contains("text=unmapped"),
        "{inspect_old_text}"
    );

    let inspect_new = inspect(&new_pdf, &["--glyphs"]);
    assert_eq!(inspect_new.status.code(), Some(0));
    let inspect_new_text = stdout(&inspect_new);
    assert!(
        inspect_new_text.contains("glyphs: 87"),
        "{inspect_new_text}"
    );
    assert!(
        inspect_new_text.contains("text=\"定\""),
        "{inspect_new_text}"
    );
    assert!(
        inspect_new_text.contains("text=\"2\""),
        "{inspect_new_text}"
    );
    assert!(
        inspect_new_text.contains("direction=(1,0)"),
        "{inspect_new_text}"
    );
    assert!(
        !inspect_new_text.contains("text=unmapped"),
        "{inspect_new_text}"
    );

    // 3. Structured JSON report
    let json_output = compare(&old_pdf, &new_pdf, &["-j", path_text(&json_path)]);
    assert_eq!(
        json_output.status.code(),
        Some(1),
        "{}",
        stderr(&json_output)
    );

    let json_text = fs::read_to_string(&json_path).expect("JSON report should be readable");
    let report: serde_json::Value =
        serde_json::from_str(&json_text).expect("JSON report should parse");

    assert_eq!(report["schema_version"], 11);
    assert_eq!(report["summary"]["content_changes"], 1);
    assert_eq!(report["summary"]["formatting_only_changes"], 0);
    assert_eq!(report["summary"]["uncertain_changes"], 0);
    assert_eq!(report["summary"]["unresolved_regions"], 0);
    assert_eq!(report["summary"]["unsupported_extraction_issues"], 0);
    assert_eq!(report["summary"]["unresolved_extraction_issues"], 0);
    assert_eq!(report["summary"]["comparison_complete"], true);
    assert_eq!(
        report["summary"]["old_alignment_coverage"]["total_tokens"],
        87
    );
    assert_eq!(
        report["summary"]["old_alignment_coverage"]["resolved_tokens"],
        87
    );
    assert_eq!(report["summary"]["old_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["total_tokens"],
        87
    );
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["resolved_tokens"],
        87
    );
    assert_eq!(report["summary"]["new_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(report["summary"]["comparison_coverage_ratio"], 1.0);

    let changes = report["changes"]
        .as_array()
        .expect("changes should be array");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["kind"], "replacement");
    assert_eq!(changes[0]["occurrences"][0]["old_span"]["text"], "1");
    assert_eq!(changes[0]["occurrences"][0]["new_span"]["text"], "2");
    assert_eq!(
        changes[0]["occurrences"][0]["old_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        changes[0]["occurrences"][0]["new_span"]["pages"],
        serde_json::json!([0])
    );

    assert_eq!(report["extraction"]["old_complete"], true);
    assert_eq!(report["extraction"]["new_complete"], true);
    assert_eq!(
        report["extraction"]["issues"].as_array().map(Vec::len),
        Some(0)
    );

    // Strict mode must succeed with exit code 1 rather than exit code 3 (incomplete).
    let strict_output = compare(&old_pdf, &new_pdf, &["--strict"]);
    assert_eq!(
        strict_output.status.code(),
        Some(1),
        "strict mode should accept complete comparison: {}",
        stderr(&strict_output)
    );
}

#[test]
fn externally_rendered_typst_japanese_case1_wrap_revision_pair_reports_zero_content_changes() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case1-japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");

    assert!(
        old_pdf.exists(),
        "vendored old.pdf must exist at {}",
        old_pdf.display()
    );
    assert!(
        new_pdf.exists(),
        "vendored new.pdf must exist at {}",
        new_pdf.display()
    );

    let directory = TestDirectory::new();
    let json_path = directory.join("report.json");

    // 1. Text report to stdout
    let text_output = compare(&old_pdf, &new_pdf, &[]);
    assert_eq!(
        text_output.status.code(),
        Some(0),
        "{}",
        stderr(&text_output)
    );
    let stdout_text = stdout(&text_output);
    assert!(stdout_text.contains("content changes: 0"), "{stdout_text}");
    assert!(stdout_text.contains("formatting-only: 2"), "{stdout_text}");
    assert!(stdout_text.contains("uncertain: 0"), "{stdout_text}");
    assert!(
        stdout_text.contains("unresolved regions: 0"),
        "{stdout_text}"
    );
    assert!(stdout_text.contains("coverage 100.0%"), "{stdout_text}");

    // 2. Glyph inspection asserting 158 mapped horizontal glyphs on both sides
    let inspect_old = inspect(&old_pdf, &["--glyphs"]);
    assert_eq!(inspect_old.status.code(), Some(0));
    let inspect_old_text = stdout(&inspect_old);
    assert!(
        inspect_old_text.contains("glyphs: 158"),
        "{inspect_old_text}"
    );
    assert!(
        inspect_old_text.contains("text=\"定\""),
        "{inspect_old_text}"
    );
    assert!(
        inspect_old_text.contains("direction=(1,0)"),
        "{inspect_old_text}"
    );
    assert!(
        !inspect_old_text.contains("text=unmapped"),
        "{inspect_old_text}"
    );

    let inspect_new = inspect(&new_pdf, &["--glyphs"]);
    assert_eq!(inspect_new.status.code(), Some(0));
    let inspect_new_text = stdout(&inspect_new);
    assert!(
        inspect_new_text.contains("glyphs: 158"),
        "{inspect_new_text}"
    );
    assert!(
        inspect_new_text.contains("text=\"定\""),
        "{inspect_new_text}"
    );
    assert!(
        inspect_new_text.contains("direction=(1,0)"),
        "{inspect_new_text}"
    );
    assert!(
        !inspect_new_text.contains("text=unmapped"),
        "{inspect_new_text}"
    );

    // 3. Structured JSON report
    let json_output = compare(&old_pdf, &new_pdf, &["-j", path_text(&json_path)]);
    assert_eq!(
        json_output.status.code(),
        Some(0),
        "{}",
        stderr(&json_output)
    );

    let json_text = fs::read_to_string(&json_path).expect("JSON report should be readable");
    let report: serde_json::Value =
        serde_json::from_str(&json_text).expect("JSON report should parse");

    assert_eq!(report["schema_version"], 11);
    assert_eq!(report["summary"]["content_changes"], 0);
    assert_eq!(report["summary"]["formatting_only_changes"], 2);
    assert_eq!(report["summary"]["uncertain_changes"], 0);
    assert_eq!(report["summary"]["unresolved_regions"], 0);
    assert_eq!(report["summary"]["unsupported_extraction_issues"], 0);
    assert_eq!(report["summary"]["unresolved_extraction_issues"], 0);
    assert_eq!(report["summary"]["comparison_complete"], true);
    assert_eq!(
        report["summary"]["old_alignment_coverage"]["total_tokens"],
        158
    );
    assert_eq!(
        report["summary"]["old_alignment_coverage"]["resolved_tokens"],
        158
    );
    assert_eq!(report["summary"]["old_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["total_tokens"],
        158
    );
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["resolved_tokens"],
        158
    );
    assert_eq!(report["summary"]["new_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(report["summary"]["comparison_coverage_ratio"], 1.0);

    let changes = report["changes"]
        .as_array()
        .expect("changes should be array");
    assert_eq!(changes.len(), 0);

    let formatting_changes = report["formatting_only_changes"]
        .as_array()
        .expect("formatting_only_changes should be array");
    assert_eq!(formatting_changes.len(), 2);
    assert_eq!(
        formatting_changes[0]["reasons"],
        serde_json::json!(["normalization", "line_break"])
    );
    assert_eq!(
        formatting_changes[0]["old_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        formatting_changes[0]["new_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        formatting_changes[0]["old_span"]["text"],
        "今後の保守計画およびサービス稼働状況に関する概要です。クラウド基盤およびオンプレミス環境の定期点検を完了し、全システムの稼働率は計画値を上回る高い安定性を維持しています。"
    );
    assert_eq!(
        formatting_changes[0]["new_span"]["text"],
        "今後の保守計画およびサービス稼働状況に関する概要です。クラウド基盤およびオンプレミス環境の定期点検を完了し、全システムの稼働率は計画値を上回る高い安定性を維持しています。"
    );

    assert_eq!(
        formatting_changes[1]["reasons"],
        serde_json::json!(["normalization", "line_break"])
    );
    assert_eq!(
        formatting_changes[1]["old_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        formatting_changes[1]["new_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        formatting_changes[1]["old_span"]["text"],
        "運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"
    );
    assert_eq!(
        formatting_changes[1]["new_span"]["text"],
        "運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"
    );

    assert_eq!(report["extraction"]["old_complete"], true);
    assert_eq!(report["extraction"]["new_complete"], true);
    assert_eq!(
        report["extraction"]["issues"].as_array().map(Vec::len),
        Some(0)
    );

    // Strict mode must succeed with exit code 0.
    let strict_output = compare(&old_pdf, &new_pdf, &["--strict"]);
    assert_eq!(
        strict_output.status.code(),
        Some(0),
        "strict mode should accept unchanged comparison: {}",
        stderr(&strict_output)
    );
}

#[test]
fn externally_rendered_typst_japanese_case2_pagebreak_revision_pair_reports_zero_content_changes() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external/case2-japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");

    assert!(
        old_pdf.exists(),
        "vendored old.pdf must exist at {}",
        old_pdf.display()
    );
    assert!(
        new_pdf.exists(),
        "vendored new.pdf must exist at {}",
        new_pdf.display()
    );

    let directory = TestDirectory::new();
    let json_path = directory.join("report.json");

    // 1. Text report to stdout
    let text_output = compare(&old_pdf, &new_pdf, &[]);
    assert_eq!(
        text_output.status.code(),
        Some(0),
        "{}",
        stderr(&text_output)
    );
    let stdout_text = stdout(&text_output);
    assert!(stdout_text.contains("content changes: 0"), "{stdout_text}");
    assert!(stdout_text.contains("formatting-only: 1"), "{stdout_text}");
    assert!(stdout_text.contains("uncertain: 0"), "{stdout_text}");
    assert!(
        stdout_text.contains("unresolved regions: 0"),
        "{stdout_text}"
    );
    assert!(stdout_text.contains("coverage 100.0%"), "{stdout_text}");

    // 2. Glyph inspection asserting 100 mapped horizontal glyphs on both sides
    let inspect_old = inspect(&old_pdf, &["--glyphs"]);
    assert_eq!(inspect_old.status.code(), Some(0));
    let inspect_old_text = stdout(&inspect_old);
    let inspect_old_lines: Vec<&str> = inspect_old_text
        .lines()
        .filter(|l| l.starts_with("glyph id="))
        .collect();
    assert_eq!(inspect_old_lines.len(), 100);
    assert!(
        inspect_old_lines.iter().all(|l| {
            l.contains("text=")
                && l.contains("direction=(1,0)")
                && !l.contains("unmapped-font-hash=")
                && !l.contains("unmapped-glyph-id=")
        }),
        "{inspect_old_text}"
    );

    let inspect_new = inspect(&new_pdf, &["--glyphs"]);
    assert_eq!(inspect_new.status.code(), Some(0));
    let inspect_new_text = stdout(&inspect_new);
    let inspect_new_lines: Vec<&str> = inspect_new_text
        .lines()
        .filter(|l| l.starts_with("glyph id="))
        .collect();
    assert_eq!(inspect_new_lines.len(), 100);
    assert!(
        inspect_new_lines.iter().all(|l| {
            l.contains("text=")
                && l.contains("direction=(1,0)")
                && !l.contains("unmapped-font-hash=")
                && !l.contains("unmapped-glyph-id=")
        }),
        "{inspect_new_text}"
    );

    // 3. Structured JSON report
    let json_output = compare(&old_pdf, &new_pdf, &["-j", path_text(&json_path)]);
    assert_eq!(
        json_output.status.code(),
        Some(0),
        "{}",
        stderr(&json_output)
    );

    let json_text = fs::read_to_string(&json_path).expect("JSON report should be readable");
    let report: serde_json::Value =
        serde_json::from_str(&json_text).expect("JSON report should parse");

    assert_eq!(report["schema_version"], 11);
    assert_eq!(report["summary"]["content_changes"], 0);
    assert_eq!(report["summary"]["formatting_only_changes"], 1);
    assert_eq!(report["summary"]["uncertain_changes"], 0);
    assert_eq!(report["summary"]["unresolved_regions"], 0);
    assert_eq!(report["summary"]["unsupported_extraction_issues"], 0);
    assert_eq!(report["summary"]["unresolved_extraction_issues"], 0);
    assert_eq!(report["summary"]["comparison_complete"], true);
    assert_eq!(
        report["summary"]["old_alignment_coverage"]["total_tokens"],
        100
    );
    assert_eq!(
        report["summary"]["old_alignment_coverage"]["resolved_tokens"],
        100
    );
    assert_eq!(report["summary"]["old_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["total_tokens"],
        100
    );
    assert_eq!(
        report["summary"]["new_alignment_coverage"]["resolved_tokens"],
        100
    );
    assert_eq!(report["summary"]["new_alignment_coverage"]["ratio"], 1.0);
    assert_eq!(report["summary"]["comparison_coverage_ratio"], 1.0);

    let changes = report["changes"]
        .as_array()
        .expect("changes should be array");
    assert_eq!(changes.len(), 0);

    let formatting_changes = report["formatting_only_changes"]
        .as_array()
        .expect("formatting_only_changes should be array");
    assert_eq!(formatting_changes.len(), 1);
    assert_eq!(
        formatting_changes[0]["reasons"],
        serde_json::json!(["position"])
    );
    assert_eq!(
        formatting_changes[0]["old_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        formatting_changes[0]["new_span"]["pages"],
        serde_json::json!([1])
    );

    assert_eq!(report["extraction"]["old_complete"], true);
    assert_eq!(report["extraction"]["new_complete"], true);
    assert_eq!(
        report["extraction"]["issues"].as_array().map(Vec::len),
        Some(0)
    );

    // Strict mode must succeed with exit code 0.
    let strict_output = compare(&old_pdf, &new_pdf, &["--strict"]);
    assert_eq!(
        strict_output.status.code(),
        Some(0),
        "strict mode should accept unchanged comparison: {}",
        stderr(&strict_output)
    );
}

#[test]
fn externally_rendered_typst_japanese_case4_case5_revision_pair_reports_exact_insertion_and_deletion()
 {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/external/case4-case5-japanese-typst");
    let old_pdf = fixture_dir.join("old.pdf");
    let new_pdf = fixture_dir.join("new.pdf");

    assert!(
        old_pdf.exists(),
        "vendored old.pdf must exist at {}",
        old_pdf.display()
    );
    assert!(
        new_pdf.exists(),
        "vendored new.pdf must exist at {}",
        new_pdf.display()
    );

    let directory = TestDirectory::new();
    let forward_json_path = directory.join("forward_report.json");
    let reverse_json_path = directory.join("reverse_report.json");

    // =========================================================================
    // 1. FORWARD COMPARISON (old -> new: Case 4 Paragraph Insertion)
    // =========================================================================
    let text_output = compare(&old_pdf, &new_pdf, &[]);
    assert_eq!(
        text_output.status.code(),
        Some(1),
        "{}",
        stderr(&text_output)
    );
    let stdout_text = stdout(&text_output);
    assert!(stdout_text.contains("content changes: 1"), "{stdout_text}");
    assert!(stdout_text.contains("formatting-only: 1"), "{stdout_text}");
    assert!(stdout_text.contains("uncertain: 0"), "{stdout_text}");
    assert!(
        stdout_text.contains("unresolved regions: 0"),
        "{stdout_text}"
    );
    assert!(stdout_text.contains("coverage 100.0%"), "{stdout_text}");
    assert!(
        stdout_text
            .contains("+ 運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"),
        "{stdout_text}"
    );

    // Glyph inspection: old has 66 mapped horizontal glyphs, new has 100
    let inspect_old = inspect(&old_pdf, &["--glyphs"]);
    assert_eq!(inspect_old.status.code(), Some(0));
    let inspect_old_text = stdout(&inspect_old);
    let inspect_old_lines: Vec<&str> = inspect_old_text
        .lines()
        .filter(|l| l.starts_with("glyph id="))
        .collect();
    assert_eq!(inspect_old_lines.len(), 66);
    assert!(
        inspect_old_lines.iter().all(|l| {
            l.contains("text=")
                && l.contains("direction=(1,0)")
                && !l.contains("unmapped-font-hash=")
                && !l.contains("unmapped-glyph-id=")
        }),
        "{inspect_old_text}"
    );

    let inspect_new = inspect(&new_pdf, &["--glyphs"]);
    assert_eq!(inspect_new.status.code(), Some(0));
    let inspect_new_text = stdout(&inspect_new);
    let inspect_new_lines: Vec<&str> = inspect_new_text
        .lines()
        .filter(|l| l.starts_with("glyph id="))
        .collect();
    assert_eq!(inspect_new_lines.len(), 100);
    assert!(
        inspect_new_lines.iter().all(|l| {
            l.contains("text=")
                && l.contains("direction=(1,0)")
                && !l.contains("unmapped-font-hash=")
                && !l.contains("unmapped-glyph-id=")
        }),
        "{inspect_new_text}"
    );

    // Forward JSON report
    let forward_json_output = compare(&old_pdf, &new_pdf, &["-j", path_text(&forward_json_path)]);
    assert_eq!(
        forward_json_output.status.code(),
        Some(1),
        "{}",
        stderr(&forward_json_output)
    );

    let forward_json_text =
        fs::read_to_string(&forward_json_path).expect("forward JSON report should be readable");
    let forward_report: serde_json::Value =
        serde_json::from_str(&forward_json_text).expect("forward JSON report should parse");

    assert_eq!(forward_report["schema_version"], 11);
    assert_eq!(forward_report["summary"]["content_changes"], 1);
    assert_eq!(forward_report["summary"]["formatting_only_changes"], 1);
    assert_eq!(forward_report["summary"]["uncertain_changes"], 0);
    assert_eq!(forward_report["summary"]["unresolved_regions"], 0);
    assert_eq!(
        forward_report["summary"]["unsupported_extraction_issues"],
        0
    );
    assert_eq!(forward_report["summary"]["unresolved_extraction_issues"], 0);
    assert_eq!(forward_report["summary"]["comparison_complete"], true);
    assert_eq!(
        forward_report["summary"]["old_alignment_coverage"]["total_tokens"],
        66
    );
    assert_eq!(
        forward_report["summary"]["old_alignment_coverage"]["resolved_tokens"],
        66
    );
    assert_eq!(
        forward_report["summary"]["old_alignment_coverage"]["ratio"],
        1.0
    );
    assert_eq!(
        forward_report["summary"]["new_alignment_coverage"]["total_tokens"],
        100
    );
    assert_eq!(
        forward_report["summary"]["new_alignment_coverage"]["resolved_tokens"],
        100
    );
    assert_eq!(
        forward_report["summary"]["new_alignment_coverage"]["ratio"],
        1.0
    );
    assert_eq!(forward_report["summary"]["comparison_coverage_ratio"], 1.0);

    let forward_changes = forward_report["changes"]
        .as_array()
        .expect("changes should be array");
    assert_eq!(forward_changes.len(), 1);
    assert_eq!(forward_changes[0]["kind"], "insertion");
    assert!(forward_changes[0]["occurrences"][0]["old_span"].is_null());
    assert_eq!(
        forward_changes[0]["occurrences"][0]["new_span"]["blocks"],
        serde_json::json!([2])
    );
    assert_eq!(
        forward_changes[0]["occurrences"][0]["new_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        forward_changes[0]["occurrences"][0]["new_span"]["text"],
        "運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"
    );

    let forward_formatting = forward_report["formatting_only_changes"]
        .as_array()
        .expect("formatting_only_changes should be array");
    assert_eq!(forward_formatting.len(), 1);
    assert_eq!(
        forward_formatting[0]["reasons"],
        serde_json::json!(["position"])
    );

    assert_eq!(forward_report["extraction"]["old_complete"], true);
    assert_eq!(forward_report["extraction"]["new_complete"], true);
    assert_eq!(
        forward_report["extraction"]["issues"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    // Forward Strict mode
    let forward_strict_output = compare(&old_pdf, &new_pdf, &["--strict"]);
    assert_eq!(
        forward_strict_output.status.code(),
        Some(1),
        "strict mode must still exit 1 (Changed) when content changes exist: {}",
        stderr(&forward_strict_output)
    );

    // =========================================================================
    // 2. REVERSE COMPARISON (new -> old: Case 5 Paragraph Deletion)
    // =========================================================================
    let reverse_text_output = compare(&new_pdf, &old_pdf, &[]);
    assert_eq!(
        reverse_text_output.status.code(),
        Some(1),
        "{}",
        stderr(&reverse_text_output)
    );
    let reverse_stdout_text = stdout(&reverse_text_output);
    assert!(
        reverse_stdout_text.contains("content changes: 1"),
        "{reverse_stdout_text}"
    );
    assert!(
        reverse_stdout_text.contains("formatting-only: 1"),
        "{reverse_stdout_text}"
    );
    assert!(
        reverse_stdout_text.contains("uncertain: 0"),
        "{reverse_stdout_text}"
    );
    assert!(
        reverse_stdout_text.contains("unresolved regions: 0"),
        "{reverse_stdout_text}"
    );
    assert!(
        reverse_stdout_text.contains("coverage 100.0%"),
        "{reverse_stdout_text}"
    );
    assert!(
        reverse_stdout_text
            .contains("- 運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"),
        "{reverse_stdout_text}"
    );

    // Reverse JSON report
    let reverse_json_output = compare(&new_pdf, &old_pdf, &["-j", path_text(&reverse_json_path)]);
    assert_eq!(
        reverse_json_output.status.code(),
        Some(1),
        "{}",
        stderr(&reverse_json_output)
    );

    let reverse_json_text =
        fs::read_to_string(&reverse_json_path).expect("reverse JSON report should be readable");
    let reverse_report: serde_json::Value =
        serde_json::from_str(&reverse_json_text).expect("reverse JSON report should parse");

    assert_eq!(reverse_report["schema_version"], 11);
    assert_eq!(reverse_report["summary"]["content_changes"], 1);
    assert_eq!(reverse_report["summary"]["formatting_only_changes"], 1);
    assert_eq!(reverse_report["summary"]["uncertain_changes"], 0);
    assert_eq!(reverse_report["summary"]["unresolved_regions"], 0);
    assert_eq!(
        reverse_report["summary"]["unsupported_extraction_issues"],
        0
    );
    assert_eq!(reverse_report["summary"]["unresolved_extraction_issues"], 0);
    assert_eq!(reverse_report["summary"]["comparison_complete"], true);
    assert_eq!(
        reverse_report["summary"]["old_alignment_coverage"]["total_tokens"],
        100
    );
    assert_eq!(
        reverse_report["summary"]["old_alignment_coverage"]["resolved_tokens"],
        100
    );
    assert_eq!(
        reverse_report["summary"]["old_alignment_coverage"]["ratio"],
        1.0
    );
    assert_eq!(
        reverse_report["summary"]["new_alignment_coverage"]["total_tokens"],
        66
    );
    assert_eq!(
        reverse_report["summary"]["new_alignment_coverage"]["resolved_tokens"],
        66
    );
    assert_eq!(
        reverse_report["summary"]["new_alignment_coverage"]["ratio"],
        1.0
    );
    assert_eq!(reverse_report["summary"]["comparison_coverage_ratio"], 1.0);

    let reverse_changes = reverse_report["changes"]
        .as_array()
        .expect("changes should be array");
    assert_eq!(reverse_changes.len(), 1);
    assert_eq!(reverse_changes[0]["kind"], "deletion");
    assert_eq!(
        reverse_changes[0]["occurrences"][0]["old_span"]["blocks"],
        serde_json::json!([2])
    );
    assert_eq!(
        reverse_changes[0]["occurrences"][0]["old_span"]["pages"],
        serde_json::json!([0])
    );
    assert_eq!(
        reverse_changes[0]["occurrences"][0]["old_span"]["text"],
        "運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。"
    );
    assert!(reverse_changes[0]["occurrences"][0]["new_span"].is_null());

    let reverse_formatting = reverse_report["formatting_only_changes"]
        .as_array()
        .expect("formatting_only_changes should be array");
    assert_eq!(reverse_formatting.len(), 1);
    assert_eq!(
        reverse_formatting[0]["reasons"],
        serde_json::json!(["position"])
    );

    assert_eq!(reverse_report["extraction"]["old_complete"], true);
    assert_eq!(reverse_report["extraction"]["new_complete"], true);
    assert_eq!(
        reverse_report["extraction"]["issues"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    // Reverse Strict mode
    let reverse_strict_output = compare(&new_pdf, &old_pdf, &["--strict"]);
    assert_eq!(
        reverse_strict_output.status.code(),
        Some(1),
        "strict mode must still exit 1 (Changed) when content changes exist: {}",
        stderr(&reverse_strict_output)
    );
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

fn compare(old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
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
        assert_eq!(selected["schema_version"], 2);
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
    assert_eq!(report["schema_version"], 2);
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
            .output()
            .expect("visual comparison");
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let report: Value =
            serde_json::from_slice(&fs::read(changed_report).expect("visual report"))
                .expect("JSON report");
        assert_eq!(report["old"]["native_glyphs"], 0);
        assert_eq!(report["inferred_changes"], 1, "{report}");
        assert_eq!(report["typed_changes"], 0);
        let pair = &report["comparison"]["scopes"][0]["result"]["comparisons"][0];
        assert_eq!(pair["operation"]["kind"], "page_rendering_changed");
        assert_eq!(pair["pixel_mask"]["changed_pixels"], 72 * 72);
        assert!(pair["text_mask"].is_null());

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

#[test]
fn default_report_displays_inferred_text_changes_and_exact_mask_counts() {
    let directory = TestDirectory::new();
    let old = directory.join("old-body.pdf");
    let new = directory.join("new-body.pdf");
    write_pdf(&old, &["The fee is 100 dollars."]);
    write_pdf(&new, &["The fee is 200 dollars."]);
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old)
        .arg(&new)
        .output()
        .expect("document comparison");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Inferred change: Paragraph (page 1)"),
        "{text}"
    );
    assert!(text.contains("  - \"The fee is 100 dollars.\""), "{text}");
    assert!(text.contains("  + \"The fee is 200 dollars.\""), "{text}");
    assert!(
        text.contains("Mandatory changed positions: old 1, new 1"),
        "{text}"
    );
    assert!(text.contains("Unresolved old Visual"), "{text}");
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
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(json.exists() && text.exists() && trace.exists());

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
fn native_structure_ids_preserve_cell_identity_when_values_are_swapped() {
    fn write_table(path: &Path, values: [&str; 2], reversed_drawing: bool, cover: bool) {
        let mut pdf = Document::with_version("1.7");
        let pages = pdf.new_object_id();
        let page = pdf.new_object_id();
        let root = pdf.new_object_id();
        let table = pdf.new_object_id();
        let font = pdf.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
        );
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        let mut identifiers = vec![("totals".to_owned(), table)];
        for (index, name) in ["sales", "profit"].into_iter().enumerate() {
            let row = pdf.new_object_id();
            let cell = pdf.add_object(dictionary! {
                "Type" => "StructElem", "S" => "TD", "P" => row, "Pg" => page, "K" => index as i64,
                "ID" => Object::string_literal(format!("{name}-amount")),
            });
            pdf.objects.insert(
                row,
                Object::Dictionary(dictionary! {
                    "Type" => "StructElem", "S" => "TR", "P" => table, "K" => cell,
                    "ID" => Object::string_literal(name),
                }),
            );
            rows.push(Object::Reference(row));
            cells.push(Object::Reference(cell));
            identifiers.push((name.to_owned(), row));
            identifiers.push((format!("{name}-amount"), cell));
        }
        pdf.objects.insert(
            table,
            Object::Dictionary(dictionary! {
                "Type" => "StructElem", "S" => "Table", "P" => root, "K" => rows,
                "ID" => Object::string_literal("totals"),
            }),
        );
        identifiers.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<_> = identifiers
            .into_iter()
            .flat_map(|(id, object)| [Object::string_literal(id), Object::Reference(object)])
            .collect();
        pdf.objects.insert(root, Object::Dictionary(dictionary! {
            "Type" => "StructTreeRoot", "K" => table,
            "IDTree" => dictionary! { "Names" => names },
            "ParentTree" => dictionary! { "Nums" => vec![Object::Integer(0), Object::Array(cells)] },
            "ParentTreeNextKey" => 1,
        }));
        let order = if reversed_drawing { [1, 0] } else { [0, 1] };
        let mut content = String::from("BT /F1 10 Tf ");
        for index in order {
            content.push_str(&format!(
                "/Span << /MCID {index} >> BDC 1 0 0 1 20 {} Tm ({}) Tj EMC ",
                [80, 50][index],
                values[index]
            ));
        }
        content.push_str("ET");
        let contents = pdf.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        pdf.objects.insert(page, Object::Dictionary(dictionary! {
            "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "Contents" => contents,
            "StructParents" => 0,
        }));
        let mut kids = Vec::new();
        if cover {
            let blank = pdf.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            });
            kids.push(Object::Reference(blank));
        }
        kids.push(Object::Reference(page));
        pdf.objects.insert(
            pages,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids },
            ),
        );
        let catalog = pdf.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => pages, "StructTreeRoot" => root, "MarkInfo" => dictionary! { "Marked" => true } },
        );
        pdf.trailer.set("Root", catalog);
        pdf.save(path).expect("tagged table fixture");
    }
    let directory = TestDirectory::new();
    let old = directory.join("old-table.pdf");
    write_table(&old, ["100", "20"], false, false);
    for (index, (values, reversed, cover, changes)) in [
        (["20", "100"], false, false, 2),
        (["100", "20"], true, false, 0),
        (["100", "20"], false, true, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let new = directory.join(&format!("new-table-{index}.pdf"));
        let report_path = directory.join(&format!("table-{index}.json"));
        write_table(&new, values, reversed, cover);
        let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
            .args(["--channels", "text,relations"])
            .arg(&old)
            .arg(&new)
            .arg("--json")
            .arg(&report_path)
            .output()
            .expect("tagged table comparison");
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let report: Value =
            serde_json::from_slice(&fs::read(report_path).expect("report")).expect("JSON");
        assert_eq!(report["old"]["structured_elements"], 5);
        assert_eq!(report["new"]["structured_elements"], 5);
        assert_eq!(report["typed_changes"], changes, "{report}");
        assert_eq!(report["inferred_changes"], 0, "{report}");
        assert_eq!(report["comparison_complete"], false);
    }
}

#[test]
fn form_values_follow_field_names_when_values_and_field_order_are_swapped() {
    let directory = TestDirectory::new();
    let old = directory.join("old-forms.pdf");
    let new = directory.join("new-forms.pdf");
    let report = directory.join("forms.json");
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
        4
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
}

#[test]
#[cfg(target_os = "linux")]
fn native_worker_limit_preserves_independent_stored_field_changes() {
    let directory = TestDirectory::new();
    let old = directory.join("limited-old.pdf");
    let new = directory.join("limited-new.pdf");
    for (path, value) in [(&old, "100"), (&new, "200")] {
        write_pdf(path, &["native extraction will exceed its array budget"]);
        let mut pdf = Document::load(path).expect("native fixture");
        let mut content = vec![b'['];
        content.extend_from_slice(
            &b"0 "
                .repeat(pdfdelta_core::source::ExtractionLimits::default().max_array_elements + 1),
        );
        content.extend_from_slice(b"] TJ");
        let contents = pdf.add_object(Stream::new(dictionary! {}, content));
        let page = pdf.get_pages()[&1];
        pdf.get_object_mut(page)
            .and_then(Object::as_dict_mut)
            .expect("page")
            .set("Contents", contents);
        let field = pdf.add_object(dictionary! { "FT" => "Tx", "T" => Object::string_literal("fee"), "V" => Object::string_literal(value) });
        let root = pdf
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .expect("root");
        pdf.get_object_mut(root)
            .and_then(Object::as_dict_mut)
            .expect("catalog")
            .set(
                "AcroForm",
                dictionary! { "Fields" => vec![Object::Reference(field)] },
            );
        pdf.save(path).expect("limited extraction fixture");
    }
    let report_path = directory.join("limited.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "text,forms"])
        .arg(&old)
        .arg(&new)
        .arg("--json")
        .arg(&report_path)
        .output()
        .expect("worker comparison");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report: Value =
        serde_json::from_slice(&fs::read(report_path).expect("report")).expect("JSON");
    assert_eq!(report["typed_changes"], 1, "{report}");
    assert_eq!(report["comparison_complete"], false);
    for side in ["old", "new"] {
        assert_eq!(report[side]["pages"], 1);
        assert_eq!(
            report[side]["form_fields"]
                .as_array()
                .expect("fields")
                .len(),
            1
        );
        assert!(
            report[side]["issues"]
                .as_array()
                .expect("issues")
                .iter()
                .any(|issue| issue["channel"] == "text" && issue["kind"] == "resource_limit"),
            "{report}"
        );
    }
    let fields = report["coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .find(|coverage| coverage["channel"] == "forms")
        .expect("forms coverage");
    assert_eq!(fields["complete"], true);
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
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let json = fs::read_to_string(report_path).expect("encrypted report");
        assert!(!json.contains(password));
        let report: Value = serde_json::from_str(&json).expect("JSON");
        assert_eq!(report["old"]["native_glyphs"], 39);
        assert_eq!(report["inferred_changes"], 1);
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
        (vec![b'x'; 256 * 1024 + 1], 3),
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
            let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
                .args(["--channels", channels])
                .arg(baseline)
                .arg(input)
                .arg("--json")
                .arg(&report_path)
                .output()
                .expect("widget comparison");
            assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
            let report: Value =
                serde_json::from_slice(&fs::read(report_path).expect("widget report"))
                    .expect("JSON report");
            assert_eq!(report["comparison_complete"], false);
            assert_eq!(
                report["typed_changes"],
                usize::from(index == 2 || index == 3)
                    + usize::from(index == 1 || index == 3)
                    + 2 * usize::from(index == 9),
                "{report}"
            );
            assert_eq!(report["inferred_changes"], 0, "{report}");
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
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let report = read_json(&destination);
        assert_eq!(report["comparison_complete"], false);
        assert_eq!(report["old"]["native_glyphs"], 14);
        assert_eq!(report["new"]["native_glyphs"], glyphs);
        let changes: Vec<_> = report["comparison"]["scopes"]
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
        assert_eq!(changes.len(), usize::from(changed));
        if changed {
            assert_eq!(changes[0]["interpretation"], "inferred");
            assert_eq!(changes[0]["operation"]["old"], "出荷重量百キログラム");
            assert_eq!(changes[0]["operation"]["new"], "出荷重量二百キログラム");
            assert_eq!(changes[0]["text_mask"]["old"], serde_json::json!([]));
            assert_eq!(
                changes[0]["text_mask"]["new"],
                serde_json::json!([
                    { "position": 4, "sources": [{ "origin": "native", "glyph": 8 }] }
                ])
            );
        } else {
            let coverage = report["coverage"]
                .as_array()
                .expect("channels")
                .iter()
                .find(|coverage| coverage["channel"] == "text")
                .expect("text coverage");
            assert_eq!(coverage["old_compared_sources"], 14);
            assert_eq!(coverage["new_compared_sources"], 14);
        }
    }
}

/// Run a comparison with the given directory as the working directory so
/// relative output-path spellings can be exercised.
fn compare_in(directory: &Path, old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
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

fn write_pdf(path: &Path, lines: &[&str]) {
    write_pdf_pages(path, &[lines], 30);
}

#[test]
fn selected_text_does_not_prove_absence_of_outlined_text() {
    let directory = TestDirectory::new();
    let input = directory.join("outlined.pdf");
    write_pdf(&input, &["temporary native content"]);
    let mut pdf = Document::load(&input).expect("load fixture");
    let page = pdf.get_pages()[&1];
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"20 20 m 30 25 l 30 70 l 20 70 l 40 70 l S".to_vec(),
    ));
    pdf.get_object_mut(page)
        .expect("page")
        .as_dict_mut()
        .expect("page dictionary")
        .set("Contents", content);
    pdf.save(&input).expect("save outlined fixture");
    let report_path = directory.join("outlined.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "text"])
        .arg(&input)
        .arg(&input)
        .arg("--json")
        .arg(&report_path)
        .output()
        .expect("selected-text comparison");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report: Value =
        serde_json::from_slice(&fs::read(report_path).expect("report")).expect("JSON");
    assert_eq!(report["old"]["native_glyphs"], 0);
    assert_eq!(report["comparison_complete"], false);
    assert_eq!(report["coverage"][0]["old_inventory_complete"], false);

    let old = directory.join("mixed-old.pdf");
    let new = directory.join("mixed-new.pdf");
    for (path, text) in [
        (&old, "The shipment quantity is 100 kilograms."),
        (&new, "The shipment quantity is 200 kilograms."),
    ] {
        write_pdf(path, &[text]);
        let mut pdf = Document::load(path).expect("load native fixture");
        let page = pdf.get_pages()[&1];
        let painted = pdf.add_object(Stream::new(
            dictionary! {},
            b"20 20 m 30 25 l 30 70 l S".to_vec(),
        ));
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
        pdf.save(path).expect("save mixed fixture");
    }
    let report_path = directory.join("mixed.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args(["--channels", "text"])
        .arg(&old)
        .arg(&new)
        .arg("--json")
        .arg(&report_path)
        .output()
        .expect("mixed comparison");
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let report: Value =
        serde_json::from_slice(&fs::read(report_path).expect("report")).expect("JSON");
    assert_eq!(report["old"]["native_glyphs"], 39);
    assert_eq!(report["new"]["native_glyphs"], 39);
    assert_eq!(
        report["typed_changes"].as_u64().expect("typed count")
            + report["inferred_changes"].as_u64().expect("inferred count"),
        1
    );
    assert_eq!(report["coverage"][0]["old_inventory_complete"], false);
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
    assert_eq!(report["schema_version"], 11, "{report:#}");
    let summary = &report["summary"];
    assert_eq!(
        summary["content_changes"].as_u64(),
        Some(u64::try_from(expected_changes).expect("expected change count should fit in u64")),
        "{report:#}"
    );
    assert_eq!(
        summary["proven_changed_regions"].as_u64(),
        Some(0),
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
    let proven = report["proven_changed_regions"]
        .as_array()
        .expect("proven changed regions should be an array");
    assert!(proven.is_empty(), "{report:#}");
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

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("JSON file should be readable"))
        .expect("JSON file should be valid")
}

fn phase<'a>(trace: &'a Value, name: &str, side: Option<&str>) -> &'a Value {
    trace["phases"]
        .as_array()
        .expect("trace phases should be an array")
        .iter()
        .find(|phase| {
            phase["name"].as_str() == Some(name)
                && match side {
                    Some(side) => phase["side"].as_str() == Some(side),
                    None => phase.get("side").is_none(),
                }
        })
        .expect("trace phase should exist")
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
