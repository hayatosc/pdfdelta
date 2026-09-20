//! The packet audit is the regression gate for the review contract, so it has
//! to fail when an invariant is violated and stay quiet when none is.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use pdfdelta_bench::agent_review::{FindingKind, audit_bundle};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Bundle(PathBuf);

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Writes a minimal bundle by hand, so the audit is tested against the
/// published shape rather than against the writer that produced it.
fn bundle(case: Value, extra: &[(&str, Vec<u8>)]) -> Bundle {
    let directory = std::env::temp_dir().join(format!(
        "pdfbench-audit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(directory.join("cases")).expect("bundle directory");
    let case_id = case["case_id"].as_str().expect("case id").to_owned();
    let mut record = json!({
        "case": case_id,
        "question": case["question"],
        "engine_class": case["engine_class"],
        "finding": case["finding"],
        "reasons": [],
        "completeness": case["completeness"],
        "alternatives_total": case["alternatives_total"],
        "next": "text",
    });
    // A published listing leaves out an enumeration that returned nothing, so
    // the audit has to read a record without the field rather than reject it.
    if case["alternatives_returned"].as_u64() != Some(0) {
        record["alternatives_returned"] = case["alternatives_returned"].clone();
    }
    let index =
        serde_json::to_vec(&json!({ "bundle_id": "b0", "records": [record] })).expect("index");
    let packet = serde_json::to_vec(&case).expect("case");

    let mut files = vec![
        ("cases/index.json".to_owned(), index),
        (format!("cases/{case_id}.json"), packet),
    ];
    for (name, bytes) in extra {
        files.push(((*name).to_owned(), bytes.clone()));
    }
    let mut records = Vec::new();
    for (name, bytes) in &files {
        let path = directory.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("artifact directory");
        }
        fs::write(&path, bytes).expect("artifact");
        records.push(json!({
            "name": name,
            "bytes": bytes.len(),
            "sha256": hex(&Sha256::digest(bytes)),
        }));
    }
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "bundle_id": "b0",
            "pipeline": "shared_evidence",
            "engine": { "comparison_complete": false },
            "export": { "export_complete": true },
            "unlocalized_gaps": [],
            "files": records,
        }))
        .expect("manifest"),
    )
    .expect("manifest");
    Bundle(directory)
}

fn sound_case() -> Value {
    json!({
        "case_id": "R0123456789ab",
        "content_digest": "00",
        "question": "compare_content",
        "pipeline": "shared_evidence",
        "engine_class": "strict",
        "finding": "difference_established",
        "channels": ["text"],
        "completeness": {
            "evidence": "unknown",
            "candidate_enumeration": "complete",
            "solver_search": "complete",
            "response": "complete",
        },
        "reasons": [{ "reason": "competing_correspondence" }],
        "alternatives_returned": 0,
        "alternatives_total": 0,
        "evidence": ["old:g1"],
    })
}

#[test]
fn a_sound_bundle_reports_no_findings() {
    let bundle = bundle(sound_case(), &[]);
    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(audit.sound(), "{:?}", audit.findings);
    assert_eq!(audit.cases, 1);
    assert_eq!(audit.cost.measured_in, "bytes");
    assert!(
        audit.cost.tokenizer.is_none(),
        "tokens are not measured, so no tokenizer is named"
    );
    assert!(audit.cost.index_bytes > 0);
    assert_eq!(audit.cost.case_bytes_median, audit.cost.case_bytes_max);
}

#[test]
fn an_edited_packet_is_a_finding() {
    let bundle = bundle(sound_case(), &[]);
    let packet = bundle.0.join("cases/R0123456789ab.json");
    let mut stored: Value =
        serde_json::from_slice(&fs::read(&packet).expect("packet")).expect("JSON");
    stored["engine_class"] = Value::String("inferred".into());
    fs::write(&packet, serde_json::to_vec(&stored).expect("edited")).expect("write");

    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(
        audit
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::ArtifactDigestMismatch),
        "{:?}",
        audit.findings
    );
    assert!(!audit.sound());
}

#[test]
fn a_case_without_a_reason_is_a_finding() {
    let mut case = sound_case();
    case["reasons"] = Value::Array(Vec::new());
    let bundle = bundle(case, &[]);
    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(
        audit
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::MissingReason),
        "{:?}",
        audit.findings
    );
}

#[test]
fn a_total_for_an_unfinished_enumeration_is_a_finding() {
    let mut case = sound_case();
    case["completeness"]["candidate_enumeration"] = Value::String("incomplete".into());
    case["alternatives_total"] = json!(3);
    let bundle = bundle(case, &[]);
    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(
        audit
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::CompletenessAccounting),
        "{:?}",
        audit.findings
    );
}

#[test]
fn a_picture_offered_for_an_unpublished_page_is_a_finding() {
    let mut case = sound_case();
    case["regions"] = json!([{
        "side": "old",
        "page_number": 1,
        "page_index": 0,
    }]);
    case["available_actions"] = json!([{ "action": "render", "case": "R0123456789ab" }]);
    let bundle = bundle(case, &[]);
    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(
        audit
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::UnpublishedRegionPage),
        "{:?}",
        audit.findings
    );
}

#[test]
fn a_file_the_manifest_does_not_name_is_a_finding() {
    let bundle = bundle(sound_case(), &[]);
    fs::write(bundle.0.join("cases/stray.json"), b"{}").expect("stray file");
    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(
        audit
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::UnlistedArtifact),
        "{:?}",
        audit.findings
    );
}

#[test]
fn a_host_usage_record_is_carried_through_without_being_merged() {
    let bundle = bundle(sound_case(), &[]);
    let usage = bundle.0.join("usage.json");
    fs::write(
        &usage,
        serde_json::to_vec(&json!({
            "tokenizer": "example-tokenizer/1",
            "input_tokens": 1234,
        }))
        .expect("usage"),
    )
    .expect("write usage");

    let audit = audit_bundle(&bundle.0, None, Some(&usage)).expect("audit");
    let reported = &audit.host_usage.expect("host usage").reported;
    assert_eq!(reported["input_tokens"], 1234);
    // The host's tokenizer describes the host's own figures, not the byte
    // measurements beside them.
    assert_eq!(reported["tokenizer"], "example-tokenizer/1");
    let audit = audit_bundle(&bundle.0, None, None).expect("audit");
    assert!(audit.cost.tokenizer.is_none());
}
