//! Hash-bound evaluation of the CLI's multi-channel document reports.

use pdfdelta_core::document::{
    BackendIdentity, ChannelCoverage, ComparisonContract, DocumentViewComparison, EvidenceIssue,
};
use serde::{Deserialize, Serialize};

use crate::{
    BenchError, Result,
    evaluation::BenchmarkProvenance,
    generalization::{GeneralizationAnnotation, GeneralizationScore, evaluate_observations},
};

pub const MAX_DOCUMENT_ANNOTATION_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DOCUMENT_REPORT_BYTES: usize = 64 * 1024 * 1024;

/// Provenance describes the pair; producer changes should name both producers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentAnnotation {
    pub old_sha256: String,
    pub new_sha256: String,
    pub provenance: BenchmarkProvenance,
    pub content_mutation: String,
    pub representation_mutation: String,
    pub expectations: GeneralizationAnnotation,
}

#[derive(Debug, Serialize)]
pub struct DocumentEvaluation {
    pub schema_version: u32,
    pub old_sha256: String,
    pub new_sha256: String,
    pub provenance: BenchmarkProvenance,
    pub content_mutation: String,
    pub representation_mutation: String,
    /// Missing timing in an older report stays unknown, not zero.
    pub comparison_wall_time_ms: Option<u64>,
    pub old_backends: Vec<BackendIdentity>,
    pub new_backends: Vec<BackendIdentity>,
    pub score: GeneralizationScore,
}

#[derive(Deserialize)]
struct ReportEvidence {
    revision: String,
    issues: Vec<EvidenceIssue>,
    backends: Vec<BackendIdentity>,
}

#[derive(Deserialize)]
struct DocumentReport {
    schema_version: u32,
    #[serde(default)]
    comparison_wall_time_ms: Option<u64>,
    contract: ComparisonContract,
    coverage: Vec<ChannelCoverage>,
    old: ReportEvidence,
    new: ReportEvidence,
    comparison: DocumentViewComparison,
}

/// Evaluate a report without treating its declared completion flag as truth.
///
/// Coverage is checked for internal consistency, not independently re-extracted
/// from the PDF. Input hashes bind this evaluation to the report's revisions.
///
/// # Errors
/// Rejects oversized or malformed JSON, unknown versions, hash/channel mismatch,
/// invalid provenance, inconsistent coverage, and excessive evaluation work.
pub fn evaluate_document_report(annotation: &[u8], report: &[u8]) -> Result<DocumentEvaluation> {
    let invalid = |message: &str| BenchError::InvalidInput(message.into());
    if annotation.len() > MAX_DOCUMENT_ANNOTATION_BYTES || report.len() > MAX_DOCUMENT_REPORT_BYTES
    {
        return Err(invalid("document evaluation input exceeds byte limit"));
    }
    let annotation: DocumentAnnotation = serde_json::from_slice(annotation)
        .map_err(|error| BenchError::InvalidInput(format!("document annotation: {error}")))?;
    let report: DocumentReport = serde_json::from_slice(report)
        .map_err(|error| BenchError::InvalidInput(format!("document report: {error}")))?;
    if report.schema_version != 2 || report.contract.version != 1 {
        return Err(invalid("unsupported document report or contract version"));
    }
    for (expected, actual) in [
        (&annotation.old_sha256, &report.old.revision),
        (&annotation.new_sha256, &report.new.revision),
    ] {
        if expected.len() != 64
            || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !expected.eq_ignore_ascii_case(actual)
        {
            return Err(invalid("document annotation/report input hash mismatch"));
        }
    }
    if annotation.content_mutation.trim().is_empty()
        || annotation.representation_mutation.trim().is_empty()
    {
        return Err(invalid(
            "content and representation mutation axes must be named",
        ));
    }
    let provenance = &annotation.provenance;
    BenchmarkProvenance::parse(
        &[
            &provenance.document_series_id,
            &provenance.derivation_group,
            &provenance.producer_family,
            &provenance.producer_version,
            &provenance.annotation_scope,
            &provenance.first_evaluated,
            provenance.tuning_use.label(),
        ],
        1,
    )?;
    if report.contract.channels != annotation.expectations.channels
        || report.coverage.len() != report.contract.channels.len()
    {
        return Err(invalid("document evaluation channels differ"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for coverage in &report.coverage {
        if !report.contract.channels.contains(&coverage.channel) || !seen.insert(coverage.channel) {
            return Err(invalid("unexpected or duplicate coverage channel"));
        }
        for (discovered, compared, uncompared) in [
            (
                coverage.old_discovered_sources,
                coverage.old_compared_sources,
                coverage.old_uncompared_sources,
            ),
            (
                coverage.new_discovered_sources,
                coverage.new_compared_sources,
                coverage.new_uncompared_sources,
            ),
        ] {
            if compared.checked_add(uncompared) != Some(discovered) {
                return Err(invalid("inconsistent source coverage counts"));
            }
        }
        let complete = coverage.old_inventory_complete
            && coverage.new_inventory_complete
            && coverage.old_uncompared_sources == 0
            && coverage.new_uncompared_sources == 0;
        if coverage.complete != complete {
            return Err(invalid("inconsistent channel completion"));
        }
        for (inventory_complete, evidence) in [
            (coverage.old_inventory_complete, &report.old),
            (coverage.new_inventory_complete, &report.new),
        ] {
            if inventory_complete
                && evidence
                    .issues
                    .iter()
                    .any(|issue| issue.channel == coverage.channel)
            {
                return Err(invalid(
                    "complete inventory contradicts reported evidence issues",
                ));
            }
        }
    }
    let issue_count = |evidence: &ReportEvidence| {
        evidence
            .issues
            .iter()
            .filter(|issue| annotation.expectations.channels.contains(&issue.channel))
            .count()
    };
    let old_issues = issue_count(&report.old);
    let new_issues = issue_count(&report.new);
    let score = evaluate_observations(
        &annotation.expectations,
        &report.comparison,
        report.coverage,
        old_issues,
        new_issues,
    )?;
    Ok(DocumentEvaluation {
        schema_version: 1,
        old_sha256: report.old.revision,
        new_sha256: report.new.revision,
        provenance: annotation.provenance,
        content_mutation: annotation.content_mutation,
        representation_mutation: annotation.representation_mutation,
        comparison_wall_time_ms: report.comparison_wall_time_ms,
        old_backends: report.old.backends,
        new_backends: report.new.backends,
        score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> (Value, Value) {
        let hash = "a".repeat(64);
        let annotation = json!({
            "old_sha256": hash, "new_sha256": hash,
            "provenance": {
                "document_series_id": "test", "derivation_group": "test",
                "producer_family": "fixture", "producer_version": "1",
                "annotation_scope": "complete", "first_evaluated": "2026-09-09",
                "tuning_use": "used_for_fix"
            },
            "content_mutation": "none", "representation_mutation": "none",
            "expectations": {
                "schema_version": 1, "channels": ["visual"],
                "dimensions": [{"dimension": "change_unit", "alternatives": [[]]}]
            }
        });
        let report = json!({
            "schema_version": 2, "comparison_complete": true,
            "contract": {"version": 1, "channels": ["visual"]},
            "old": {"revision": hash, "issues": [], "backends": []},
            "new": {"revision": hash, "issues": [], "backends": []},
            "comparison": {"scopes": [], "relations": [], "relation_unresolved": []},
            "coverage": [{
                "channel": "visual", "old_inventory_complete": false, "new_inventory_complete": false,
                "old_discovered_sources": 1, "new_discovered_sources": 1,
                "old_compared_sources": 0, "new_compared_sources": 0,
                "old_uncompared_sources": 1, "new_uncompared_sources": 1, "complete": false
            }]
        });
        (annotation, report)
    }

    fn run(annotation: &Value, report: &Value) -> Result<DocumentEvaluation> {
        evaluate_document_report(
            &serde_json::to_vec(annotation).expect("serialize test annotation"),
            &serde_json::to_vec(report).expect("serialize test report"),
        )
    }

    #[test]
    fn unknown_timing_and_incomplete_discovery_survive_a_claimed_complete_report() {
        let (annotation, mut report) = fixture();
        let result = run(&annotation, &report).expect("score unknown visual discovery");
        assert!(!result.score.comparison_complete);
        assert_eq!(result.comparison_wall_time_ms, None);
        assert_eq!(result.score.coverage[0].old_uncompared_sources, 1);
        report["comparison_wall_time_ms"] = json!(37);
        assert_eq!(
            run(&annotation, &report)
                .expect("score measured report")
                .comparison_wall_time_ms,
            Some(37)
        );
    }

    #[test]
    fn incompatible_inputs_and_inconsistent_coverage_are_rejected() {
        let (annotation, report) = fixture();
        for (path, value) in [
            ("/old/revision", json!("b".repeat(64))),
            ("/schema_version", json!(1)),
            ("/contract/channels", json!(["forms"])),
            ("/coverage/0/old_compared_sources", json!(1)),
            ("/coverage/0/complete", json!(true)),
        ] {
            let mut malformed = report.clone();
            *malformed.pointer_mut(path).expect("fixture property") = value;
            assert!(run(&annotation, &malformed).is_err(), "{path}");
        }
    }
}
