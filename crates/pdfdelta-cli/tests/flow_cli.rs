//! End-to-end tests for the `pdfdelta flow` subcommand.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use flate2::read::GzDecoder;
use serde_json::Value;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-cli-flow-{}-{sequence}",
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

fn fixture(pair: &str, side: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/external")
        .join(pair)
        .join(format!("{side}.pdf"))
}

fn run_flow(old: &Path, new: &Path, extra_arguments: &[&str]) -> Output {
    run_flow_with_env(old, new, extra_arguments, &[])
}

fn run_flow_with_env(
    old: &Path,
    new: &Path,
    extra_arguments: &[&str],
    environment: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pdfdelta"));
    command.arg("flow").arg(old).arg(new).args(extra_arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("pdfdelta flow should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn read_gzip_json(path: &Path) -> Value {
    let mut json = String::new();
    GzDecoder::new(fs::File::open(path).expect("report should be readable"))
        .read_to_string(&mut json)
        .expect("report should be gzip-encoded JSON");
    serde_json::from_str(&json).expect("report should be JSON")
}

#[test]
fn identical_documents_exit_zero_and_report_complete_extraction() {
    let old = fixture("case3-typst", "old");
    let output = run_flow(&old, &old, &[]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("content changes: 0"), "{report}");
    assert!(report.contains("old extraction: complete"), "{report}");
    assert!(report.contains("new extraction: complete"), "{report}");
    assert!(!report.contains("1. "), "{report}");
}

#[test]
fn flow_limit_scale_scales_extraction_and_rejects_lower_values() {
    let old = fixture("case3-typst", "old");
    let output = run_flow(&old, &old, &["--limit-scale", "2"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let lowered = run_flow(&old, &old, &["--limit-scale", "0.5"]);
    assert_eq!(lowered.status.code(), Some(2), "{}", stderr(&lowered));
    assert!(
        stderr(&lowered).contains("limit scale"),
        "{}",
        stderr(&lowered)
    );
}

#[test]
fn cached_and_in_process_candidates_produce_identical_output() {
    let directory = TestDirectory::new();
    let old = fixture("case3-typst", "old");
    let new = fixture("case3-typst", "new");
    let plain = directory.join("plain.json.gz");
    let cached = directory.join("cached.json.gz");
    let plain_run = run_flow(
        &old,
        &new,
        &["--json", plain.to_str().expect("UTF-8 path"), "--tokens"],
    );
    let cached_run = run_flow(
        &old,
        &new,
        &[
            "--limit-scale",
            "1.01",
            "--json",
            cached.to_str().expect("UTF-8 path"),
            "--tokens",
        ],
    );
    assert_eq!(
        plain_run.status.code(),
        cached_run.status.code(),
        "{}",
        stderr(&cached_run)
    );
    assert_eq!(stdout(&plain_run), stdout(&cached_run));
    assert_eq!(
        fs::read(&plain).expect("in-process report should exist"),
        fs::read(&cached).expect("cached report should exist"),
        "byte-identical JSON report"
    );
    for side in ["old", "new"] {
        let plain_path = PathBuf::from(format!(
            "{}.{side}-tokens.jsonl.gz",
            plain.with_extension("").display()
        ));
        let cached_path = PathBuf::from(format!(
            "{}.{side}-tokens.jsonl.gz",
            cached.with_extension("").display()
        ));
        assert_eq!(
            fs::read(&plain_path).expect("in-process sidecar should exist"),
            fs::read(&cached_path).expect("cached sidecar should exist"),
            "byte-identical {side} token sidecar"
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn failed_candidate_builder_exits_two_without_a_report() {
    let directory = TestDirectory::new();
    let report = directory.join("report.json.gz");
    let output = run_flow_with_env(
        &fixture("case3-typst", "old"),
        &fixture("case3-typst", "new"),
        &[
            "--limit-scale",
            "1.01",
            "--json",
            report.to_str().expect("UTF-8 path"),
        ],
        &[("PDFDELTA_FLOW_BUILD_FAIL", "1")],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("candidate builder"),
        "{}",
        stderr(&output)
    );
    assert!(!report.exists(), "a failed child must not publish a report");
}

#[test]
fn changed_documents_exit_one_and_write_the_gzip_json_report() {
    let directory = TestDirectory::new();
    let report_path = directory.join("report.json.gz");
    let output = run_flow(
        &fixture("case3-typst", "old"),
        &fixture("case3-typst", "new"),
        &["--json", report_path.to_str().expect("UTF-8 path")],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("content changes: 1"), "{report}");
    assert!(report.contains("1. replace old 1 -> new 1"), "{report}");
    assert!(report.contains("old: "), "{report}");
    assert!(report.contains("new: "), "{report}");

    let json = read_gzip_json(&report_path);
    assert_eq!(json["schema"], "pdfdelta-flow/1");
    assert_eq!(json["old"]["extraction_complete"], true);
    assert_eq!(json["new"]["extraction_complete"], true);
    assert_eq!(json["old"]["issues"], serde_json::json!([]));
    let old_tokens = json["old"]["tokens"].as_u64().expect("old token count");
    assert!(old_tokens > 0);
    assert_eq!(json["stats"]["replace"]["changes"], 1);
    assert_eq!(json["stats"]["replace"]["old_tokens"], 1);
    assert_eq!(json["stats"]["replace"]["new_tokens"], 1);
    let changes = json["changes"].as_array().expect("changes array");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["kind"], "replace");
    assert_eq!(changes[0]["old_pages"], serde_json::json!([1]));
    assert_eq!(changes[0]["new_pages"], serde_json::json!([1]));
    assert_eq!(changes[0]["old_text"], "10");
    assert_eq!(changes[0]["new_text"], "20");
    assert_eq!(changes[0]["touches_incomplete_page"], false);
    assert_eq!(json["old"]["incomplete_pages"], serde_json::json!([]));
    assert_eq!(
        json["new"]["incomplete_document_kinds"],
        serde_json::json!([])
    );
    assert_eq!(
        changes[0]["old"].as_array().expect("old range").len(),
        2,
        "ranges are [start, end]"
    );
    assert!(
        json.get("old_tokens").is_none(),
        "token texts live in sidecars, not in the report"
    );
}

fn read_token_sidecar(path: &Path) -> Vec<Value> {
    let mut text = String::new();
    GzDecoder::new(fs::File::open(path).expect("token sidecar should be readable"))
        .read_to_string(&mut text)
        .expect("token sidecar should be gzip-encoded");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("one JSON string per line"))
        .collect()
}

#[test]
fn tokens_flag_includes_every_token_text() {
    let directory = TestDirectory::new();
    let report_path = directory.join("tokens.json.gz");
    let output = run_flow(
        &fixture("case3-typst", "old"),
        &fixture("case3-typst", "new"),
        &[
            "--json",
            report_path.to_str().expect("UTF-8 path"),
            "--tokens",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_gzip_json(&report_path);
    assert!(
        json.get("old_tokens").is_none(),
        "the report object stays small"
    );
    let old_tokens = read_token_sidecar(&directory.join("tokens.json.old-tokens.jsonl.gz"));
    assert_eq!(
        old_tokens.len() as u64,
        json["old"]["tokens"].as_u64().expect("old token count")
    );
    assert!(old_tokens.iter().all(Value::is_string));
    let new_tokens = read_token_sidecar(&directory.join("tokens.json.new-tokens.jsonl.gz"));
    assert_eq!(
        new_tokens.len() as u64,
        json["new"]["tokens"].as_u64().expect("new token count")
    );
}

#[test]
fn cjk_changes_are_isolated_to_one_scalar() {
    let directory = TestDirectory::new();
    let report_path = directory.join("cjk.json.gz");
    let output = run_flow(
        &fixture("japanese-typst", "old"),
        &fixture("japanese-typst", "new"),
        &["--json", report_path.to_str().expect("UTF-8 path")],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json = read_gzip_json(&report_path);
    let changes = json["changes"].as_array().expect("changes array");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["kind"], "replace");
    assert_eq!(changes[0]["old_text"], "10");
    assert_eq!(changes[0]["new_text"], "20");
    assert_eq!(changes[0]["touches_incomplete_page"], false);
    assert_eq!(json["old"]["incomplete_pages"], serde_json::json!([]));
    assert_eq!(
        json["new"]["incomplete_document_kinds"],
        serde_json::json!([])
    );
    assert_eq!(json["stats"]["replace"]["old_tokens"], 1);
    assert_eq!(json["stats"]["replace"]["new_tokens"], 1);
}

#[test]
fn missing_input_exits_two() {
    let directory = TestDirectory::new();
    let output = run_flow(
        &directory.join("missing.pdf"),
        &fixture("case3-typst", "new"),
        &[],
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("missing.pdf"),
        "{}",
        stderr(&output)
    );
}
