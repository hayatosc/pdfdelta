//! Hash-bound evaluation of the CLI's multi-channel document reports.

use pdfdelta_core::document::{
    BackendIdentity, ChannelCoverage, ComparisonContract, CorrespondenceScope,
    DocumentViewComparison, EvidenceIssue, InterpretationStatus, MatchingAlgorithm,
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
    #[serde(default)]
    native_glyphs: Option<usize>,
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

/// Operational observations of one shared-pipeline route. These counts are not
/// annotation recall, character precision, or independent extraction coverage.
#[derive(Debug, Serialize)]
pub struct DocumentRouteSummary {
    pub schema_version: u32,
    pub old_sha256: String,
    pub new_sha256: String,
    pub contract: ComparisonContract,
    pub comparison_wall_time_ms: Option<u64>,
    pub comparison_complete: bool,
    pub coverage: Vec<ChannelCoverage>,
    pub conditional_operations: usize,
    pub inferred_operations: usize,
    /// Old/new local token positions; distinct glyph references are not scalars.
    pub conditional_text_mask_positions: [usize; 2],
    pub inferred_text_mask_positions: [usize; 2],
    pub scopes: Vec<ScopeSearchSummary>,
    pub old_issues: Vec<EvidenceIssue>,
    pub new_issues: Vec<EvidenceIssue>,
    pub relation_unresolved: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ScopeSearchSummary {
    pub scope: CorrespondenceScope,
    pub enumeration_complete: bool,
    pub conflict_search_complete: bool,
    pub optimization_complete: bool,
    pub candidate_count: usize,
    pub accepted_correspondences: usize,
    pub components_without_mandatory_edges: usize,
    pub assignment_components: usize,
    pub assignment_work: usize,
    pub subset_states: usize,
    pub unresolved: Vec<String>,
}

/// Summarizes the shared report without converting inference into acceptance.
/// Missing historical timings stay unavailable. The report's completion and
/// change-count assertions are recomputed from its underlying observations.
///
/// # Errors
/// Rejects oversized/malformed reports, unsupported versions, and inconsistent
/// channel inventories. Does not certify that the PDF was fully extracted.
pub fn summarize_document_report(bytes: &[u8]) -> Result<DocumentRouteSummary> {
    let report = parse_report(bytes)?;
    validate_coverage(&report)?;
    let mut summary = DocumentRouteSummary {
        schema_version: 1,
        old_sha256: report.old.revision,
        new_sha256: report.new.revision,
        contract: report.contract,
        comparison_wall_time_ms: report.comparison_wall_time_ms,
        comparison_complete: report.coverage.iter().all(|coverage| coverage.complete)
            && report.comparison.search_resolved(),
        coverage: report.coverage,
        conditional_operations: 0,
        inferred_operations: 0,
        conditional_text_mask_positions: [0; 2],
        inferred_text_mask_positions: [0; 2],
        scopes: Vec::new(),
        old_issues: report.old.issues,
        new_issues: report.new.issues,
        relation_unresolved: report.comparison.relation_unresolved.clone(),
    };
    for scope in &report.comparison.scopes {
        for local in &scope.result.comparisons {
            let inferred = scope.interpretation == InterpretationStatus::Inferred
                || local.interpretation == InterpretationStatus::Inferred;
            let (operations, positions) = if inferred {
                (
                    &mut summary.inferred_operations,
                    &mut summary.inferred_text_mask_positions,
                )
            } else {
                (
                    &mut summary.conditional_operations,
                    &mut summary.conditional_text_mask_positions,
                )
            };
            *operations += usize::from(local.operation.is_some());
            if let Some(mask) = &local.text_mask {
                positions[0] += mask.old.len();
                positions[1] += mask.new.len();
            }
        }
        let result = &scope.result;
        summary.scopes.push(ScopeSearchSummary {
            scope: result.matching.scope,
            enumeration_complete: result.candidates.exhaustive,
            conflict_search_complete: result.matching.conflict_search_complete,
            optimization_complete: result.matching.conflict_search_complete
                && result
                    .matching
                    .components
                    .iter()
                    .all(|component| component.exhaustive),
            candidate_count: result.candidates.proposals.len(),
            accepted_correspondences: result.accepted_correspondences.len(),
            components_without_mandatory_edges: result
                .matching
                .components
                .iter()
                .filter(|component| component.exhaustive && component.mandatory.is_empty())
                .count(),
            assignment_components: result
                .matching
                .components
                .iter()
                .filter(|component| component.algorithm == MatchingAlgorithm::BipartiteAssignment)
                .count(),
            assignment_work: checked_work_sum(
                result
                    .matching
                    .components
                    .iter()
                    .map(|component| component.assignment_work),
            )?,
            subset_states: checked_work_sum(
                result
                    .matching
                    .components
                    .iter()
                    .map(|component| component.explored_states),
            )?,
            unresolved: result.unresolved.clone(),
        });
    }
    Ok(summary)
}

fn checked_work_sum(mut counts: impl Iterator<Item = usize>) -> Result<usize> {
    counts.try_fold(0usize, |total, count| {
        total
            .checked_add(count)
            .ok_or_else(|| BenchError::InvalidInput("document report work count overflow".into()))
    })
}

fn parse_report(bytes: &[u8]) -> Result<DocumentReport> {
    if bytes.len() > MAX_DOCUMENT_REPORT_BYTES {
        return Err(BenchError::InvalidInput(
            "document report exceeds byte limit".into(),
        ));
    }
    let report: DocumentReport = serde_json::from_slice(bytes)
        .map_err(|error| BenchError::InvalidInput(format!("document report: {error}")))?;
    if report.schema_version != 2 || report.contract.version != 1 {
        return Err(BenchError::InvalidInput(
            "unsupported document report or contract version".into(),
        ));
    }
    Ok(report)
}

/// Projects a shared-route report onto separately reconstructed native source
/// blocks. Input hashes must identify exactly the source PDFs used by the report.
///
/// # Errors
/// Rejects malformed reports, hash mismatch, inconsistent coverage, and exhausted
/// source-projection limits. Invalid individual annotations remain unavailable.
pub fn evaluate_revision_document_report(
    expected: &crate::revisions::ExpectedDocument,
    sources: [&crate::revisions::revision_selectors::PreparedRevisionSource; 2],
    bytes: &[u8],
) -> Result<crate::revisions::revision_document_report::RevisionDocumentEvaluation> {
    let report = parse_report(bytes)?;
    validate_coverage(&report)?;
    let projects_native_sources = report.comparison.scopes.iter().any(|scope| {
        scope.interpretation != InterpretationStatus::Inferred
            && scope.result.comparisons.iter().any(|local| {
                local.interpretation != InterpretationStatus::Inferred
                    && local.compared
                    && local
                        .text_mask
                        .as_ref()
                        .is_some_and(|mask| !mask.old.is_empty() || !mask.new.is_empty())
            })
    });
    for (source, actual) in sources.into_iter().zip([&report.old, &report.new]) {
        let expected = &source.metadata;
        if expected.sha256.len() != 64 || !expected.sha256.eq_ignore_ascii_case(&actual.revision) {
            return Err(BenchError::InvalidInput(
                "revision report input hash mismatch".into(),
            ));
        }
        if projects_native_sources
            && (!expected.extraction_complete
                || actual.native_glyphs != Some(expected.glyph_count)
                || !actual.backends.iter().any(|backend| {
                    backend.kind == pdfdelta_core::document::BackendKind::NativeParser
                        && backend.name == "pdfdelta-native"
                        && backend.version == env!("CARGO_PKG_VERSION")
                        && backend.profile == "content-stream-v2-paint-order-worker-v1"
                }))
        {
            return Err(BenchError::InvalidInput(
                "native source identity protocol or inventory differs".into(),
            ));
        }
    }
    crate::revisions::revision_document_report::evaluate(
        expected,
        [&sources[0].blocks, &sources[1].blocks],
        &report.comparison,
    )
    .map_err(BenchError::InvalidInput)
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
    let report = parse_report(report)?;
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
    validate_coverage(&report)?;
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

fn validate_coverage(report: &DocumentReport) -> Result<()> {
    let invalid = |message: &str| BenchError::InvalidInput(message.into());
    if report.coverage.len() != report.contract.channels.len() || report.coverage.is_empty() {
        return Err(invalid("document report lacks selected channel coverage"));
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
    Ok(())
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
    fn operational_summary_preserves_unknown_coverage_and_timing() {
        let (_, report) = fixture();
        let summary =
            summarize_document_report(&serde_json::to_vec(&report).expect("serialize report"))
                .expect("summarize incomplete report");
        assert!(!summary.comparison_complete);
        assert_eq!(summary.comparison_wall_time_ms, None);
        assert_eq!(summary.coverage[0].old_uncompared_sources, 1);
        assert_eq!(summary.conditional_operations, 0);
        assert_eq!(summary.inferred_operations, 0);
        assert!(checked_work_sum([usize::MAX, 1].into_iter()).is_err());
        assert_eq!(checked_work_sum([37, 19].into_iter()).expect("sum"), 56);
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
