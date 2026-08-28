use std::{
    fs,
    path::{Path, PathBuf},
};

use pdfdelta_bench::{
    cases::built_in_cases,
    renderers::{RenderLimits, RendererKind},
    revisions::{
        Annotation, MANIFEST_HEADER, PairRunStatus, PairSet, normalize_output_destination,
        run_revision_benchmark, write_reports_json, write_summary_json,
    },
};
use sha2::{Digest, Sha256};

struct TempCorpus {
    root: PathBuf,
}

impl Drop for TempCorpus {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn temp_corpus(name: &str) -> TempCorpus {
    let mut root = std::env::temp_dir();
    root.push(format!(
        "pdfbench-revisions-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    fs::create_dir_all(root.join("expected")).expect("temp corpus directory");
    TempCorpus { root }
}

fn replacement_case_pdf_bytes() -> (Vec<u8>, Vec<u8>) {
    let case = built_in_cases()
        .expect("built-in cases")
        .into_iter()
        .find(|case| case.name() == "text-replacement")
        .expect("text-replacement case exists");
    let limits = RenderLimits::default();
    let old = RendererKind::LopdfTj
        .render(case.plan().old(), limits)
        .expect("old side renders");
    let new = RendererKind::LopdfTj
        .render(case.plan().new_plan(), limits)
        .expect("new side renders");
    (old, new)
}

fn provenance(bytes: &[u8]) -> (u64, String) {
    let digest = Sha256::digest(bytes);
    let sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    (bytes.len() as u64, sha256)
}

fn store(cache_dir: &Path, pair_id: &str, side: &str, bytes: &[u8]) -> String {
    fs::write(cache_dir.join(format!("{pair_id}-{side}.pdf")), bytes).expect("side bytes written");
    provenance(bytes).1
}

fn manifest_row(
    pair_id: &str,
    expected_file: &str,
    old_count: u64,
    old_sha: &str,
    new_count: u64,
    new_sha: &str,
) -> String {
    format!(
        "{pair_id}\tdev\tstandard\tintegration-fixture\tsingle-column\ttrue\t-\t2026-08-24\tcomplete\t1.0\t{expected_file}\t\
         file:///{pair_id}-old.pdf\t{old_count}\t{old_sha}\tfile:///{pair_id}-new.pdf\t{new_count}\t{new_sha}"
    )
}

#[test]
fn reviewed_synthetic_pair_achieves_perfect_recall_precision_and_fragmentation() {
    let corpus = temp_corpus("clean");
    let pair_id = "synthetic-replacement";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair_id, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair_id, "new", &new_bytes);

    // Phase one reviews without annotations: the recorded preview supplies
    // the quotes a human reviewer would curate.
    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                "-",
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        true,
    )
    .expect("benchmark runs");
    let preview = &reports[0].reported_changes_preview;
    assert_eq!(preview.len(), 1, "one replacement hunk expected");
    assert_eq!(preview[0].kind, "replacement");
    let old_quote = preview[0].old_text.as_deref().expect("old-side span text");
    let new_quote = preview[0].new_text.as_deref().expect("new-side span text");
    assert_eq!(
        reports[0].quality_skipped_reason.as_deref(),
        Some("no expected annotations are recorded for this pair")
    );

    // Phase two records the reviewed expectation and re-runs.
    fs::write(
        corpus.root.join("expected").join(format!("{pair_id}.json")),
        format!(
            r#"{{"version":1,"pair":"{pair_id}","reviewed_on":"2026-08-24","annotation":"complete","changes":[{{"id":"reviewed","kind":"replacement","old_quote":{old_quote:?},"new_quote":{new_quote:?}}}]}}"#
        ),
    )
    .expect("expected file written");
    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                &format!("expected/{pair_id}.json"),
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest rewritten");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        true,
    )
    .expect("benchmark runs");

    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.healthy(), "unexpected failure: {:?}", report.failure);
    assert!(report.provenance_verified);
    assert_eq!(report.status, PairRunStatus::Ok);
    assert_eq!(report.extraction_complete, Some(true));
    assert_eq!(report.comparison_complete, Some(true));
    assert!(report.quality_skipped_reason.is_none());
    let quality = report.quality.expect("quality metrics present");
    assert_eq!(quality.annotation, Annotation::Complete);
    assert_eq!(quality.expected_changes, 1);
    assert_eq!(quality.reported_changes, 1);
    assert_eq!(quality.recall, Some(1.0));
    assert_eq!(quality.precision, Some(1.0));
    assert_eq!(quality.kind_accuracy, Some(1.0));
    assert_eq!(quality.reported_hunks_per_matched_change, Some(1.0));
    assert_eq!(quality.review_hunks_per_expected_change, Some(1.0));
    assert_eq!(quality.unmatched_tiny_changes, 0);
    assert_eq!(report.reported_changes_preview.len(), 1);
}

#[test]
fn checksum_mismatch_fails_one_pair_without_aborting_the_rest() {
    let corpus = temp_corpus("mismatch");
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let corrupted: Vec<u8> = old_bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| if index == 200 { byte ^ 0xff } else { *byte })
        .collect();

    let corrupt_old_sha = store(&corpus.root, "alpha", "old", &corrupted);
    let (alpha_new_count, alpha_new_sha) = provenance(&new_bytes);
    store(&corpus.root, "alpha", "new", &new_bytes);
    let beta_old_sha = store(&corpus.root, "beta", "old", &old_bytes);
    let (beta_new_count, beta_new_sha) = provenance(&new_bytes);
    store(&corpus.root, "beta", "new", &new_bytes);

    // alpha's row claims the pristine checksum while the stored download is corrupted.
    let (_, honest_old_sha) = provenance(&old_bytes);
    assert_ne!(corrupt_old_sha, honest_old_sha);

    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                "alpha",
                "-",
                corrupted.len() as u64,
                &honest_old_sha,
                alpha_new_count,
                &alpha_new_sha
            ),
            manifest_row(
                "beta",
                "-",
                old_bytes.len() as u64,
                &beta_old_sha,
                beta_new_count,
                &beta_new_sha
            ),
        ),
    )
    .expect("manifest written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        None,
        None,
        true,
    )
    .expect("benchmark runs");

    assert_eq!(reports.len(), 2);
    assert!(!reports[0].healthy(), "checksum mismatch must fail alpha");
    assert!(
        reports[0]
            .failure
            .as_deref()
            .unwrap_or_default()
            .contains("sha256")
    );
    assert_eq!(reports[1].pair_id, "beta");
    assert!(
        reports[1].healthy(),
        "beta must still run: {:?}",
        reports[1].failure
    );
}

#[test]
fn set_filters_apply_and_header_only_manifests_are_rejected() {
    let corpus = temp_corpus("filters");
    fs::write(
        corpus.root.join("manifest.tsv"),
        format!("{}\n", MANIFEST_HEADER.join("\t")),
    )
    .expect("header-only manifest placeholder");

    let rejected = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        Some(PairSet::Dev),
        None,
        None,
        false,
    );
    assert!(rejected.is_err(), "a manifest without pairs is invalid");
}

#[test]
fn mismatched_expected_pair_identity_fails_instead_of_reporting_foreign_metrics() {
    let corpus = temp_corpus("foreign");
    let pair_id = "synthetic-replacement";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair_id, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair_id, "new", &new_bytes);
    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                "expected/other-pair.json",
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");
    // Valid JSON for a different pair id; quotes would even match this
    // document, but the identity check must reject it outright.
    fs::write(
        corpus.root.join("expected").join("other-pair.json"),
        r#"{"version":1,"pair":"some-other-document","reviewed_on":"2026-08-24","annotation":"complete","changes":[{"id":"x","kind":"deletion","old_quote":"never present"}]}"#,
    )
    .expect("expected file written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        true,
    )
    .expect("benchmark runs");

    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report.status, PairRunStatus::Failed);
    assert!(!report.healthy());
    let failure = report.failure.as_deref().unwrap_or_default();
    assert!(failure.contains("describe pair"), "failure was {failure:?}");
    assert!(failure.contains("some-other-document"));
}

#[test]
fn checksums_only_mode_verifies_provenance_without_comparing() {
    let corpus = temp_corpus("verify-only");
    let pair_id = "synthetic-replacement";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair_id, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair_id, "new", &new_bytes);
    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                "-",
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        false,
    )
    .expect("benchmark runs");

    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report.status, PairRunStatus::Ok);
    assert!(report.healthy());
    assert!(report.provenance_verified);
    assert!(!report.compared);
    assert_eq!(report.extraction_complete, None);
    assert!(report.quality.is_none());
}

#[test]
fn summary_json_output_writes_compact_schema_and_preserves_metrics() {
    let corpus = temp_corpus("summary-output");
    let pair_id = "synthetic-replacement";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair_id, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair_id, "new", &new_bytes);

    // Discover exact quote text from unannotated run
    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                "-",
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");

    let initial_reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        true,
    )
    .expect("initial run");
    let preview = &initial_reports[0].reported_changes_preview[0];
    let old_quote = preview.old_text.as_deref().expect("old text");
    let new_quote = preview.new_text.as_deref().expect("new text");

    fs::write(
        corpus.root.join("expected").join(format!("{pair_id}.json")),
        format!(
            r#"{{"version":1,"pair":"{pair_id}","reviewed_on":"2026-08-24","annotation":"complete","changes":[{{"id":"rep-1","kind":"replacement","old_quote":{old_quote:?},"new_quote":{new_quote:?}}}]}}"#
        ),
    )
    .expect("expected file written");

    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                &format!("expected/{pair_id}.json"),
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        true,
    )
    .expect("benchmark runs");

    let summary_path = corpus.root.join("summary.json");
    write_summary_json(&summary_path, &reports).expect("write summary json");

    let content = fs::read_to_string(&summary_path).expect("read summary json");
    assert!(content.ends_with('\n'), "must have trailing newline");

    let val: serde_json::Value = serde_json::from_str(&content).expect("parse summary json");
    assert_eq!(val["schema_version"], 5);
    let records = val["records"].as_array().expect("records array");
    assert_eq!(records.len(), 1);

    let rec = &records[0];
    assert_eq!(rec["pair_id"], pair_id);
    assert_eq!(rec["status"], "ok");
    assert_eq!(rec["extraction_complete"], true);
    assert_eq!(rec["comparison_complete"], true);
    assert_eq!(rec["coverage_comparison"], 1.0);
    assert_eq!(rec["unresolved_regions"], 0);
    assert_eq!(rec["reported_content_changes"], 1);
    assert_eq!(rec["reported_formatting_changes"], 0);
    assert_eq!(rec["reported_uncertain_changes"], 0);
    assert_eq!(
        rec["sentence_recovery_metrics"]["recovered_exact_match_old_tokens"],
        0
    );
    assert_eq!(
        rec["sentence_recovery_metrics"]["near_relation_complete"],
        true
    );

    let q = &rec["quality"];
    assert_eq!(q["annotation"], "complete");
    assert_eq!(q["expected_changes"], 1);
    assert_eq!(q["reported_changes"], 1);
    assert_eq!(q["recall"], 1.0);
    assert_eq!(q["precision"], 1.0);
    assert_eq!(q["kind_accuracy"], 1.0);
    assert_eq!(q["review_hunks_per_expected_change"], 1.0);
    assert_eq!(q["unmatched_tiny_changes"], 0);
    assert_eq!(rec["candidate_recall"]["top_k"], 32);
    assert_eq!(rec["candidate_recall"]["annotated_counterparts"], 1);
    assert_eq!(rec["candidate_recall"]["evaluable_counterparts"], 1);
    assert_eq!(rec["candidate_recall"]["recalled_counterparts"], 1);
    assert_eq!(rec["candidate_recall"]["recall_at_k"], 1.0);
    assert_eq!(rec["expected_change_diagnostics"]["complete"], true);
    assert_eq!(
        rec["expected_change_diagnostics"]["failures"],
        serde_json::json!([])
    );

    // Verify raw content contains no forbidden fields
    assert!(!content.contains("runtime_ms"));
    assert!(!content.contains("actual_changes"));
    assert!(!content.contains("reported_changes_preview"));
    assert!(!content.contains("candidate_visits"));
    assert!(!content.contains("candidate_visit_pressure"));
}

#[test]
fn full_and_summary_outputs_work_together_and_reject_identical_paths() {
    let corpus = temp_corpus("full-and-summary");
    let pair_id = "synthetic-replacement";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair_id, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair_id, "new", &new_bytes);

    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                "-",
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        true,
    )
    .expect("benchmark runs");

    let full_path = corpus.root.join("full.json");
    let summary_path = corpus.root.join("summary.json");

    write_reports_json(&full_path, &reports).expect("write full json");
    write_summary_json(&summary_path, &reports).expect("write summary json");

    let full_content = fs::read_to_string(&full_path).expect("read full");
    let summary_content = fs::read_to_string(&summary_path).expect("read summary");

    assert!(full_content.contains("reported_changes_preview"));
    assert!(!summary_content.contains("reported_changes_preview"));

    // Attempting to overwrite existing full_path with summary fails cleanly
    let error = write_summary_json(&full_path, &reports).expect_err("must reject existing path");
    assert!(
        error
            .to_string()
            .contains("destination path already exists")
    );
}

#[test]
fn checksums_only_summary_records_uncompared_null_metrics() {
    let corpus = temp_corpus("checksums-only-summary");
    let pair_id = "synthetic-replacement";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair_id, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair_id, "new", &new_bytes);

    fs::write(
        corpus.root.join("manifest.tsv"),
        format!(
            "{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row(
                pair_id,
                "-",
                old_bytes.len() as u64,
                &old_sha,
                new_count,
                &new_sha
            ),
        ),
    )
    .expect("manifest written");

    let reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        Some(pair_id),
        None,
        false, // checksums-only
    )
    .expect("benchmark runs");

    let summary_path = corpus.root.join("summary.json");
    write_summary_json(&summary_path, &reports).expect("write summary json");

    let content = fs::read_to_string(&summary_path).expect("read summary");
    let val: serde_json::Value = serde_json::from_str(&content).expect("parse json");
    let rec = &val["records"][0];

    assert_eq!(rec["pair_id"], pair_id);
    assert_eq!(rec["compared"], false);
    assert_eq!(rec["status"], "ok");
    assert_eq!(rec["extraction_complete"], serde_json::Value::Null);
    assert_eq!(rec["coverage_old"], serde_json::Value::Null);
    assert_eq!(rec["quality"], serde_json::Value::Null);
    assert_eq!(rec["reported_uncertain_changes"], serde_json::Value::Null);
}

#[test]
fn set_and_pair_filtering_preserves_manifest_order_in_summary() {
    let corpus = temp_corpus("filter-order");
    let pair1 = "synthetic-dev";
    let pair2 = "synthetic-holdout";
    let (old_bytes, new_bytes) = replacement_case_pdf_bytes();
    let old_sha = store(&corpus.root, pair1, "old", &old_bytes);
    let (new_count, new_sha) = provenance(&new_bytes);
    store(&corpus.root, pair1, "new", &new_bytes);
    store(&corpus.root, pair2, "old", &old_bytes);
    store(&corpus.root, pair2, "new", &new_bytes);

    let row1 = format!(
        "{pair1}\tdev\tstandard\tintegration-fixture\tsingle-column\ttrue\t-\t2026-08-24\tcomplete\t1.0\t-\t\
         file:///{pair1}-old.pdf\t{}\t{old_sha}\tfile:///{pair1}-new.pdf\t{new_count}\t{new_sha}",
        old_bytes.len()
    );
    let row2 = format!(
        "{pair2}\tholdout\tstandard\tintegration-fixture\tsingle-column\ttrue\t-\t2026-08-24\tcomplete\t1.0\t-\t\
         file:///{pair2}-old.pdf\t{}\t{old_sha}\tfile:///{pair2}-new.pdf\t{new_count}\t{new_sha}",
        old_bytes.len()
    );

    fs::write(
        corpus.root.join("manifest.tsv"),
        format!("{}\n{row1}\n{row2}\n", MANIFEST_HEADER.join("\t")),
    )
    .expect("manifest written");

    // Filter dev only
    let dev_reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        Some(PairSet::Dev),
        None,
        None,
        false,
    )
    .expect("dev benchmark");
    let summary_dev = corpus.root.join("summary-dev.json");
    write_summary_json(&summary_dev, &dev_reports).expect("write dev summary");
    let dev_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&summary_dev).expect("read dev")).expect("parse");
    assert_eq!(dev_val["records"].as_array().expect("records").len(), 1);
    assert_eq!(dev_val["records"][0]["pair_id"], pair1);

    // All pairs in manifest order
    let all_reports = run_revision_benchmark(
        &corpus.root.join("manifest.tsv"),
        &corpus.root,
        None,
        None,
        None,
        false,
    )
    .expect("all benchmark");
    let summary_all = corpus.root.join("summary-all.json");
    write_summary_json(&summary_all, &all_reports).expect("write all summary");
    let all_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&summary_all).expect("read all")).expect("parse");
    let all_records = all_val["records"].as_array().expect("records");
    assert_eq!(all_records.len(), 2);
    assert_eq!(all_records[0]["pair_id"], pair1);
    assert_eq!(all_records[1]["pair_id"], pair2);
}

#[test]
fn destination_normalization_detects_aliases_and_symlinks() {
    let corpus = temp_corpus("norm-alias-check");
    fs::create_dir_all(corpus.root.join("target_dir")).expect("create target dir");

    let direct = corpus.root.join("target_dir").join("summary.json");
    let relative = corpus
        .root
        .join("target_dir")
        .join(".")
        .join("summary.json");

    let norm_direct = normalize_output_destination(&direct).expect("normalize direct");
    let norm_relative = normalize_output_destination(&relative).expect("normalize relative");
    assert_eq!(norm_direct, norm_relative);

    #[cfg(unix)]
    {
        let symlink_dir = corpus.root.join("symlink_dir");
        if std::os::unix::fs::symlink(corpus.root.join("target_dir"), &symlink_dir).is_ok() {
            let symlinked = symlink_dir.join("summary.json");
            let norm_symlinked =
                normalize_output_destination(&symlinked).expect("normalize symlink");
            assert_eq!(norm_direct, norm_symlinked);
        }
    }
}
