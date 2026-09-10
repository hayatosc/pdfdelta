//! Observational failure attribution; never an input to comparison or scoring.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{BenchError, Result, evaluation::sha256_hex};

pub const MAX_DIAGNOSTIC_REPORT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Acquisition,
    Normalization,
    Scope,
    Retrieval,
    Optimization,
    CounterpartDecision,
    Localization,
    ReportingEvaluation,
}

#[derive(Debug, Serialize)]
pub struct StageFinding {
    /// None preserves a reason whose stage is not established by this contract.
    pub stage: Option<FailureStage>,
    pub reason: String,
    /// Number of diagnostic observations, not events or distinct failures.
    pub observations: usize,
    /// Up to four JSON pointers into the hash-bound original report.
    pub example_locations: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct StageDiagnostics {
    pub schema_version: u32,
    pub report_sha256: Option<String>,
    pub report_schema_version: Option<u32>,
    pub findings: Vec<StageFinding>,
}

impl StageDiagnostics {
    /// A failed read or decode is evidence about reporting, not an empty diff.
    pub fn reporting_failure(reason: String) -> Self {
        Self {
            schema_version: 1,
            report_sha256: None,
            report_schema_version: None,
            findings: vec![StageFinding {
                stage: Some(FailureStage::ReportingEvaluation),
                reason,
                observations: 1,
                example_locations: Vec::new(),
            }],
        }
    }
}

#[derive(Default)]
struct Findings(BTreeMap<(Option<FailureStage>, String), StageFinding>);

impl Findings {
    fn add(&mut self, stage: Option<FailureStage>, reason: &str, location: String) {
        let finding = self
            .0
            .entry((stage, reason.to_owned()))
            .or_insert_with(|| StageFinding {
                stage,
                reason: reason.into(),
                observations: 0,
                example_locations: Vec::new(),
            });
        finding.observations += 1;
        if finding.example_locations.len() < 4 {
            finding.example_locations.push(location);
        }
    }
}

#[derive(Deserialize)]
struct Header {
    schema_version: u32,
}

/// Reads only diagnostics; irrelevant native text/source payloads are skipped
/// during deserialization. Missing findings do not certify any stage complete.
///
/// # Errors
/// Rejects unsupported schemas, malformed reports, inconsistent shared coverage
/// and oversized input. Shared reports keep their existing 64 MiB limit.
pub fn diagnose_report(bytes: &[u8]) -> Result<StageDiagnostics> {
    if bytes.len() > MAX_DIAGNOSTIC_REPORT_BYTES {
        return Err(BenchError::InvalidInput(
            "diagnostic report byte limit".into(),
        ));
    }
    let header: Header = serde_json::from_slice(bytes)
        .map_err(|error| BenchError::InvalidInput(format!("report header: {error}")))?;
    let mut findings = Findings::default();
    match header.schema_version {
        2 => shared(bytes, &mut findings)?,
        11 => native(bytes, &mut findings)?,
        _ => {
            return Err(BenchError::InvalidInput(
                "unsupported diagnostic report schema".into(),
            ));
        }
    }
    Ok(StageDiagnostics {
        schema_version: 1,
        report_sha256: Some(sha256_hex(bytes)),
        report_schema_version: Some(header.schema_version),
        findings: findings.0.into_values().collect(),
    })
}

fn shared(bytes: &[u8], findings: &mut Findings) -> Result<()> {
    use FailureStage::*;
    let report = super::parse_report(bytes)?;
    super::validate_coverage(&report)?;
    for (side, evidence) in [("old", &report.old), ("new", &report.new)] {
        for (index, issue) in evidence.issues.iter().enumerate() {
            findings.add(
                Some(Acquisition),
                &issue.reason,
                format!("/{side}/issues/{index}/reason"),
            );
        }
    }
    for (index, reason) in report.comparison.relation_unresolved.iter().enumerate() {
        findings.add(
            Some(Scope),
            reason,
            format!("/comparison/relation_unresolved/{index}"),
        );
    }
    for (scope_index, scope) in report.comparison.scopes.iter().enumerate() {
        let root = format!("/comparison/scopes/{scope_index}/result");
        let result = &scope.result;
        for (name, complete) in [
            ("candidates", result.candidates.exhaustive),
            ("text_search", result.text_search.exhaustive),
            ("visual_search", result.visual_search.exhaustive),
        ] {
            if !complete {
                findings.add(
                    Some(Retrieval),
                    &format!("{name} exhaustive=false"),
                    format!("{root}/{name}/exhaustive"),
                );
            }
        }
        for (index, reason) in result.unresolved.iter().enumerate() {
            let stage = match reason.as_str() {
                "scope candidate enumeration is incomplete"
                | "source candidate enumeration is incomplete in retained dependency regions"
                | "visual candidate enumeration is incomplete"
                | "text candidate enumeration is incomplete"
                | "structural candidate enumeration is incomplete"
                | "a correspondence component depends on omitted source candidates" => {
                    Some(Retrieval)
                }
                "a correspondence conflict component exceeded its search budget" => {
                    Some(Optimization)
                }
                "a correspondence conflict component has competing optima" => {
                    Some(CounterpartDecision)
                }
                "hierarchy traversal budget left child scopes unexamined" => Some(Scope),
                _ if reason.starts_with("local correspondence ") => Some(Localization),
                _ if reason.starts_with("child correspondence ") => Some(Scope),
                _ if reason.ends_with(" depends on unexamined correspondence rivals") => {
                    Some(CounterpartDecision)
                }
                _ => None,
            };
            findings.add(stage, reason, format!("{root}/unresolved/{index}"));
        }
        if !result.matching.conflict_search_complete {
            findings.add(
                Some(Optimization),
                "conflict_search_complete=false",
                format!("{root}/matching/conflict_search_complete"),
            );
        }
        for (index, component) in result.matching.components.iter().enumerate() {
            if !component.exhaustive {
                findings.add(
                    Some(Optimization),
                    "component exhaustive=false",
                    format!("{root}/matching/components/{index}/exhaustive"),
                );
            }
        }
        for (index, decision) in result.counterpart_decisions.unresolved.iter().enumerate() {
            for (missing_index, missing) in decision.missing.iter().enumerate() {
                let reason = serde_json::to_value(missing)
                    .map_err(|error| BenchError::InvalidInput(error.to_string()))?;
                let reason = reason.as_str().ok_or_else(|| {
                    BenchError::InvalidInput("counterpart reason is not a string".into())
                })?;
                findings.add(
                    Some(CounterpartDecision),
                    reason,
                    format!(
                        "{root}/counterpart_decisions/unresolved/{index}/missing/{missing_index}"
                    ),
                );
            }
        }
        for (index, local) in result.comparisons.iter().enumerate() {
            if !local.compared {
                findings.add(
                    Some(Localization),
                    "local compared=false",
                    format!("{root}/comparisons/{index}/compared"),
                );
            }
            for (reason_index, reason) in local.unresolved.iter().enumerate() {
                let stage = if reason
                    == "local text normalization has no source-validated interpretation family"
                {
                    Normalization
                } else {
                    Localization
                };
                findings.add(
                    Some(stage),
                    reason,
                    format!("{root}/comparisons/{index}/unresolved/{reason_index}"),
                );
            }
        }
        for (index, dependency) in result.extraction_dependencies.iter().enumerate() {
            if dependency.work_limited
                || !dependency.old_issues.is_empty()
                || !dependency.new_issues.is_empty()
            {
                findings.add(
                    Some(Acquisition),
                    "local comparison retains extraction dependencies",
                    format!("{root}/extraction_dependencies/{index}"),
                );
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct NativeReport {
    extraction: NativeExtraction,
    unresolved_regions: Vec<NativeRegion>,
    assessment: NativeAssessment,
}
#[derive(Deserialize)]
struct NativeExtraction {
    issues: Vec<NativeIssue>,
    old_complete: bool,
    new_complete: bool,
}
#[derive(Deserialize)]
struct NativeIssue {
    description: String,
}
#[derive(Deserialize)]
struct NativeRegion {
    evidence: Vec<String>,
}
#[derive(Deserialize)]
struct NativeAssessment {
    relations: Vec<NativeRelation>,
}
#[derive(Deserialize)]
struct NativeRelation {
    reasons: Vec<String>,
}

fn native(bytes: &[u8], findings: &mut Findings) -> Result<()> {
    use FailureStage::*;
    let report: NativeReport = serde_json::from_slice(bytes)
        .map_err(|error| BenchError::InvalidInput(format!("native diagnostics: {error}")))?;
    for (side, complete) in [
        ("old", report.extraction.old_complete),
        ("new", report.extraction.new_complete),
    ] {
        if !complete {
            findings.add(
                Some(Acquisition),
                "native extraction incomplete",
                format!("/extraction/{side}_complete"),
            );
        }
    }
    for (index, issue) in report.extraction.issues.iter().enumerate() {
        findings.add(
            Some(Acquisition),
            &issue.description,
            format!("/extraction/issues/{index}/description"),
        );
    }
    let regions = report
        .unresolved_regions
        .iter()
        .enumerate()
        .map(|(index, region)| {
            (
                format!("/unresolved_regions/{index}/evidence"),
                &region.evidence,
            )
        });
    let relations = report
        .assessment
        .relations
        .iter()
        .enumerate()
        .map(|(index, relation)| {
            (
                format!("/assessment/relations/{index}/reasons"),
                &relation.reasons,
            )
        });
    for (root, reasons) in regions.chain(relations) {
        for (index, reason) in reasons.iter().enumerate() {
            if matches!(
                reason.as_str(),
                "anchor"
                    | "anchor_interval"
                    | "neighbor_consistency"
                    | "numeric_mask"
                    | "split_merge"
                    | "move_candidate"
            ) || reason.starts_with("candidate_source:")
            {
                continue;
            }
            let stage = match reason.as_str() {
                "extraction_gap" => Some(Acquisition),
                "normalization_uncertainty" | "normalization_issue" => Some(Normalization),
                "unknown_reading_order"
                | "inferred_reading_order"
                | "reading_order_unknown"
                | "reading_order_inferred"
                | "domain_not_closed" => Some(Scope),
                "candidate_set_empty" | "candidate_scoring_rejected" => Some(Retrieval),
                "competing_correspondence" | "candidate_competition" => Some(CounterpartDecision),
                "ambiguous_edit_location"
                | "source_evidence_missing"
                | "diff_edit_distance_exceeded"
                | "diff_rejected_as_implausible" => Some(Localization),
                "output_limit" => Some(ReportingEvaluation),
                // These native reasons do not identify which search exhausted its budget.
                _ => None,
            };
            findings.add(stage, reason, format!("{root}/{index}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_stage_reasons_preserve_unknowns_and_ignore_positive_evidence() {
        let report = json!({
            "schema_version":11,
            "extraction":{"old_complete":false,"new_complete":true,
                "issues":[{"description":"unmapped source"}]},
            "unresolved_regions":[{"evidence":["anchor", "candidate_source:paragraph",
                "normalization_issue", "reading_order_unknown", "candidate_set_empty",
                "candidate_competition", "diff_edit_distance_exceeded"]}],
            "assessment":{"relations":[{"reasons":["output_limit", "search_incomplete", "future_reason"]}]}
        });
        let bytes = serde_json::to_vec(&report).expect("JSON");
        let result = diagnose_report(&bytes).expect("diagnostics");
        for finding in &result.findings {
            for location in &finding.example_locations {
                assert!(report.pointer(location).is_some(), "{location}");
            }
        }
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.reason == "anchor")
        );
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.reason.starts_with("candidate_source:"))
        );
        assert_eq!(
            result
                .findings
                .iter()
                .filter(|finding| finding.stage.is_none())
                .count(),
            2
        );
        for stage in [
            FailureStage::Acquisition,
            FailureStage::Normalization,
            FailureStage::Scope,
            FailureStage::Retrieval,
            FailureStage::CounterpartDecision,
            FailureStage::Localization,
            FailureStage::ReportingEvaluation,
        ] {
            assert!(
                result
                    .findings
                    .iter()
                    .any(|finding| finding.stage == Some(stage)),
                "{stage:?}"
            );
        }
    }

    #[test]
    fn grouping_bounds_examples_but_keeps_observation_counts() {
        let mut findings = Findings::default();
        for index in 0..12 {
            findings.add(
                Some(FailureStage::Optimization),
                "search incomplete",
                format!("/{index}"),
            );
        }
        let finding = findings.0.into_values().next().expect("finding");
        assert_eq!(finding.observations, 12);
        assert_eq!(finding.example_locations, ["/0", "/1", "/2", "/3"]);
    }

    #[test]
    fn shared_diagnostics_leave_existing_operational_summary_unchanged() {
        let (_, mut report) = super::super::tests::fixture();
        report["old"]["issues"] = json!([{
            "page":null,"channel":"visual","sources":[],"kind":"unsupported","reason":"unexamined paint"
        }]);
        report["comparison"]["relation_unresolved"] = json!(["hierarchy gap"]);
        let bytes = serde_json::to_vec(&report).expect("JSON");
        let before =
            serde_json::to_value(super::super::summarize_document_report(&bytes).expect("summary"))
                .expect("JSON");
        let diagnostics = diagnose_report(&bytes).expect("diagnostics");
        let after =
            serde_json::to_value(super::super::summarize_document_report(&bytes).expect("summary"))
                .expect("JSON");
        assert_eq!(before, after);
        assert_eq!(diagnostics.report_schema_version, Some(2));
        assert!(
            diagnostics
                .findings
                .iter()
                .any(|finding| finding.stage == Some(FailureStage::Acquisition))
        );
        assert!(
            diagnostics
                .findings
                .iter()
                .any(|finding| finding.stage == Some(FailureStage::Scope))
        );
        for finding in &diagnostics.findings {
            for location in &finding.example_locations {
                assert!(report.pointer(location).is_some());
            }
        }
        assert_eq!(diagnostics.report_sha256, Some(sha256_hex(&bytes)));
    }

    #[test]
    fn malformed_and_unknown_reports_do_not_become_empty_success() {
        assert!(diagnose_report(b"{}").is_err());
        assert!(diagnose_report(br#"{"schema_version":999}"#).is_err());
        assert!(diagnose_report(br#"{"schema_version":11}"#).is_err());
        let failure = StageDiagnostics::reporting_failure("report byte limit".into());
        assert_eq!(
            failure.findings[0].stage,
            Some(FailureStage::ReportingEvaluation)
        );
        assert!(failure.report_sha256.is_none());
    }
}
