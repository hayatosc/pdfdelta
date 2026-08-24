use std::{
    fs,
    path::{Path, PathBuf},
};

use pdfdelta_bench::{
    cases::built_in_cases,
    renderers::{RenderLimits, RendererKind},
    revisions::{Annotation, MANIFEST_HEADER, PairRunStatus, PairSet, run_revision_benchmark},
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
