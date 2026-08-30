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
    // Context paragraphs give the aligner exact anchors; without them a lone
    // numeric-masked match cannot be confirmed and stays unresolved.
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
    // Context paragraphs give the aligner exact anchors; without them a lone
    // numeric-masked match cannot be confirmed and stays unresolved by design.
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

    // Default mode reports the unsupported side and keeps exit code 0.
    let default_output = compare(&old, &new, &arguments);
    assert_eq!(
        default_output.status.code(),
        Some(0),
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
    assert!(json.contains("\"schema_version\": 8"));
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
    assert_eq!(trace["trace_schema_version"], 18);
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
    assert_eq!(report["schema_version"], 8);
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
        Some(1),
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
    assert_eq!(report["schema_version"], 8);
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
    write_pdf(&old, &["The archive contains 10 files"]);
    write_pdf(&new, &["The archive contains 20 files"]);

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

    assert_eq!(report["schema_version"], 8);
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

    assert_eq!(report["schema_version"], 8);
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

    assert_eq!(report["schema_version"], 8);
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

    assert_eq!(report["schema_version"], 8);
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

    assert_eq!(forward_report["schema_version"], 8);
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

    assert_eq!(reverse_report["schema_version"], 8);
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

fn compare(old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(old)
        .arg(new)
        .args(extra_arguments)
        .output()
        .expect("pdfdelta should run")
}

/// Run a comparison with the given directory as the working directory so
/// relative output-path spellings can be exercised.
fn compare_in(directory: &Path, old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
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
    assert_eq!(report["schema_version"], 8, "{report:#}");
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
