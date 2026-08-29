use std::{collections::BTreeMap, io::Write, path::Path};

use pdfdelta_core::{
    Error,
    pipeline::{
        PipelineDiagnosticRecord, PipelineDiagnostics, PipelineErrorKind, PipelineMetrics,
        PipelinePhase, PipelinePhaseStatus,
    },
    report::DocumentSide,
    source::{ExtractionIssue, ExtractionIssueKind},
};
use serde::Serialize;

const TRACE_SCHEMA_VERSION: u8 = 4;
const MAX_ERROR_MESSAGE_BYTES: usize = 2_048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceSide {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Completed,
    Incomplete,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CommandStatus {
    Completed,
    Incomplete,
    Failed,
}

#[derive(Debug, Serialize)]
struct TraceCommand {
    kind: &'static str,
    strict: bool,
    old_path: String,
    new_path: String,
}

#[derive(Debug, Serialize)]
struct TraceResult {
    status: CommandStatus,
    exit_code: u8,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceError {
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct TracePhase {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<TraceSide>,
    status: TraceStatus,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    metrics: BTreeMap<&'static str, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TraceError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skip_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionTrace {
    trace_schema_version: u8,
    command: TraceCommand,
    result: TraceResult,
    phases: Vec<TracePhase>,
}

impl ExecutionTrace {
    pub fn new(old_path: &Path, new_path: &Path, strict: bool) -> Self {
        Self {
            trace_schema_version: TRACE_SCHEMA_VERSION,
            command: TraceCommand {
                kind: "compare",
                strict,
                old_path: old_path.to_string_lossy().into_owned(),
                new_path: new_path.to_string_lossy().into_owned(),
            },
            result: TraceResult {
                status: CommandStatus::Failed,
                exit_code: 2,
            },
            phases: Vec::new(),
        }
    }

    pub fn complete(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        metrics: impl IntoIterator<Item = (&'static str, usize)>,
    ) {
        self.record(name, side, TraceStatus::Completed, metrics, None);
    }

    pub fn incomplete_extraction(
        &mut self,
        side: TraceSide,
        glyphs: usize,
        issues: &[ExtractionIssue],
    ) {
        let unsupported = issues
            .iter()
            .filter(|issue| issue.kind() == ExtractionIssueKind::Unsupported)
            .count();
        let unresolved = issues.len().saturating_sub(unsupported);
        let error = issues.first().map(|issue| TraceError {
            kind: match issue.kind() {
                ExtractionIssueKind::Unsupported => "unsupported",
                ExtractionIssueKind::Unresolved => "unresolved",
            },
            message: bounded_message(issue.description()),
            resource: None,
            limit: None,
        });
        self.record(
            "glyph_extraction",
            Some(side),
            TraceStatus::Incomplete,
            [
                ("glyphs", glyphs),
                ("issues", issues.len()),
                ("unsupported_issues", unsupported),
                ("unresolved_issues", unresolved),
            ],
            error,
        );
    }

    pub fn fail_message(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        kind: &'static str,
        message: &str,
    ) {
        self.record(
            name,
            side,
            TraceStatus::Failed,
            [],
            Some(TraceError {
                kind,
                message: bounded_message(message),
                resource: None,
                limit: None,
            }),
        );
    }

    pub fn fail_limit(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        message: &str,
        resource: &'static str,
        limit: usize,
    ) {
        self.record(
            name,
            side,
            TraceStatus::Failed,
            [],
            Some(TraceError {
                kind: "limit_exceeded",
                message: bounded_message(message),
                resource: Some(resource),
                limit: Some(limit),
            }),
        );
    }

    pub fn fail_core(&mut self, name: &'static str, side: Option<TraceSide>, error: &Error) {
        let (kind, resource, limit) = error_parts(error);
        self.record(
            name,
            side,
            TraceStatus::Failed,
            [],
            Some(TraceError {
                kind,
                message: bounded_message(&error.to_string()),
                resource,
                limit,
            }),
        );
    }

    pub fn incomplete_core(&mut self, name: &'static str, side: Option<TraceSide>, error: &Error) {
        let (kind, resource, limit) = error_parts(error);
        self.record(
            name,
            side,
            TraceStatus::Incomplete,
            [],
            Some(TraceError {
                kind,
                message: bounded_message(&error.to_string()),
                resource,
                limit,
            }),
        );
    }

    pub fn extend_pipeline(&mut self, diagnostics: &PipelineDiagnostics) {
        self.phases
            .extend(diagnostics.records().iter().map(pipeline_record));
    }

    pub fn finish(&mut self, result: Result<u8, ()>, incomplete: bool) {
        self.result = match result {
            Ok(exit_code) => TraceResult {
                status: if incomplete {
                    CommandStatus::Incomplete
                } else {
                    CommandStatus::Completed
                },
                exit_code,
            },
            Err(()) => TraceResult {
                status: CommandStatus::Failed,
                exit_code: 2,
            },
        };
        self.append_skipped_phases();
    }

    pub fn write_json<W: Write>(&self, writer: &mut W) -> Result<(), serde_json::Error> {
        serde_json::to_writer_pretty(&mut *writer, self)?;
        writer.write_all(b"\n").map_err(serde_json::Error::io)
    }

    fn record(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        status: TraceStatus,
        metrics: impl IntoIterator<Item = (&'static str, usize)>,
        error: Option<TraceError>,
    ) {
        self.phases.push(TracePhase {
            name,
            side,
            status,
            metrics: metrics.into_iter().collect(),
            error,
            skip_reason: None,
        });
    }

    fn append_skipped_phases(&mut self) {
        for (name, side) in expected_phases() {
            if !self
                .phases
                .iter()
                .any(|record| record.name == name && record.side == side)
            {
                self.phases.push(TracePhase {
                    name,
                    side,
                    status: TraceStatus::Skipped,
                    metrics: BTreeMap::new(),
                    error: None,
                    skip_reason: Some("prior_phase_did_not_complete"),
                });
            }
        }
    }
}

fn pipeline_record(record: &PipelineDiagnosticRecord) -> TracePhase {
    TracePhase {
        name: pipeline_phase_name(record.phase),
        side: record.side.map(trace_side),
        status: match record.status {
            PipelinePhaseStatus::Completed => TraceStatus::Completed,
            PipelinePhaseStatus::Incomplete => TraceStatus::Incomplete,
            PipelinePhaseStatus::Failed => TraceStatus::Failed,
        },
        metrics: pipeline_metrics(record.metrics, record.side),
        error: record.error.as_ref().map(|error| TraceError {
            kind: match error.kind {
                PipelineErrorKind::Backend => "backend",
                PipelineErrorKind::Report => "report",
                PipelineErrorKind::InvalidConfiguration => "invalid_configuration",
                PipelineErrorKind::Unsupported => "unsupported",
                PipelineErrorKind::Unresolved => "unresolved",
                PipelineErrorKind::LimitExceeded => "limit_exceeded",
            },
            message: bounded_message(&error.message),
            resource: error.resource,
            limit: error.limit,
        }),
        skip_reason: None,
    }
}

fn pipeline_metrics(
    metrics: PipelineMetrics,
    _side: Option<DocumentSide>,
) -> BTreeMap<&'static str, usize> {
    let mut flattened = [
        ("painting_glyphs", metrics.painting_glyphs),
        ("lines", metrics.lines),
        ("blocks", metrics.blocks),
        ("normalized_blocks", metrics.normalized_blocks),
        ("raw_tokens", metrics.raw_tokens),
        ("ngram_token_elements", metrics.ngram_token_elements),
        ("features", metrics.features),
        ("indexed_features", metrics.indexed_features),
        ("alignment_spans", metrics.alignment_spans),
        ("candidate_visits", metrics.candidate_visits),
        (
            "candidate_visits_required",
            metrics.candidate_visits_required,
        ),
        (
            "candidate_visits_required_exact",
            metrics.candidate_visits_required_exact,
        ),
        (
            "candidate_visits_required_ngram",
            metrics.candidate_visits_required_ngram,
        ),
        (
            "candidate_visits_required_short_fallback",
            metrics.candidate_visits_required_short_fallback,
        ),
        ("max_candidate_visits", metrics.max_candidate_visits),
        ("changes", metrics.changes),
        ("formatting_changes", metrics.formatting_changes),
        ("unresolved_regions", metrics.unresolved_regions),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name, value)))
    .collect::<BTreeMap<_, _>>();
    if let Some(sentence) = metrics.sentence_recovery_metrics {
        let pair_visit_limit = matches!(
            sentence.near_relation_stop_reason,
            Some(pdfdelta_core::diff::NearRelationStopReason::PairVisitLimit)
        );
        let similarity_comparison_limit = matches!(
            sentence.near_relation_stop_reason,
            Some(pdfdelta_core::diff::NearRelationStopReason::SimilarityComparisonLimit)
        );
        let candidate_count_limit = matches!(
            sentence.near_relation_stop_reason,
            Some(pdfdelta_core::diff::NearRelationStopReason::CandidateCountLimit)
        );
        flattened.extend([
            (
                "sentence_recovery_old_trusted_run_source_tokens",
                sentence.old_trusted_run_source_tokens,
            ),
            (
                "sentence_recovery_new_trusted_run_source_tokens",
                sentence.new_trusted_run_source_tokens,
            ),
            (
                "sentence_recovery_structural_pairing_available",
                usize::from(sentence.structural_pairing_available),
            ),
            (
                "sentence_recovery_old_structural_descriptors",
                sentence.old_structural_descriptors,
            ),
            (
                "sentence_recovery_new_structural_descriptors",
                sentence.new_structural_descriptors,
            ),
            (
                "sentence_recovery_old_structural_eligible_descriptors",
                sentence.old_structural_eligible_descriptors,
            ),
            (
                "sentence_recovery_new_structural_eligible_descriptors",
                sentence.new_structural_eligible_descriptors,
            ),
            (
                "sentence_recovery_old_structural_mixed_descriptors",
                sentence.old_structural_mixed_descriptors,
            ),
            (
                "sentence_recovery_new_structural_mixed_descriptors",
                sentence.new_structural_mixed_descriptors,
            ),
            (
                "sentence_recovery_old_structural_split_descriptors",
                sentence.old_structural_split_descriptors,
            ),
            (
                "sentence_recovery_new_structural_split_descriptors",
                sentence.new_structural_split_descriptors,
            ),
            (
                "sentence_recovery_structural_shared_profiles",
                sentence.structural_shared_profiles,
            ),
            (
                "sentence_recovery_structural_candidate_pairs",
                sentence.structural_candidate_pairs,
            ),
            (
                "sentence_recovery_structural_largest_posting",
                sentence.structural_largest_posting,
            ),
            (
                "sentence_recovery_structural_duplicate_pairs",
                sentence.structural_duplicate_pairs,
            ),
            (
                "sentence_recovery_structural_unique_reciprocal_pairs",
                sentence.structural_unique_reciprocal_pairs,
            ),
            (
                "sentence_recovery_structural_unique_no_anchor_pairs",
                sentence.structural_unique_no_anchor_pairs,
            ),
            (
                "sentence_recovery_structural_unique_monotone_anchor_pairs",
                sentence.structural_unique_monotone_anchor_pairs,
            ),
            (
                "sentence_recovery_structural_unique_crossing_veto_pairs",
                sentence.structural_unique_crossing_veto_pairs,
            ),
            (
                "sentence_recovery_exact_shared_units",
                sentence.exact_shared_units,
            ),
            (
                "sentence_recovery_old_exact_one_sided_units",
                sentence.old_exact_one_sided_units,
            ),
            (
                "sentence_recovery_new_exact_one_sided_units",
                sentence.new_exact_one_sided_units,
            ),
            (
                "sentence_recovery_near_relation_complete",
                usize::from(sentence.near_relation_complete),
            ),
            (
                "sentence_recovery_near_pair_candidates",
                sentence.near_pair_candidates,
            ),
            (
                "sentence_recovery_near_pair_visits_examined",
                sentence.near_pair_visits_examined,
            ),
            (
                "sentence_recovery_near_pair_visits_attempted",
                sentence.near_pair_visits_attempted,
            ),
            (
                "sentence_recovery_near_similarity_comparisons_examined",
                sentence.near_similarity_comparisons_examined,
            ),
            (
                "sentence_recovery_near_similarity_comparisons_attempted",
                sentence.near_similarity_comparisons_attempted,
            ),
            (
                "sentence_recovery_near_largest_edge_posting",
                sentence.near_largest_edge_posting,
            ),
            (
                "sentence_recovery_near_largest_edge_query_union",
                sentence.near_largest_edge_query_union,
            ),
            (
                "sentence_recovery_near_largest_filtered_candidate_set",
                sentence.near_largest_filtered_candidate_set,
            ),
            (
                "sentence_recovery_near_candidate_count_truncated",
                usize::from(sentence.near_candidate_count_truncated),
            ),
            (
                "sentence_recovery_near_relation_stop_reason_pair_visit_limit",
                usize::from(pair_visit_limit),
            ),
            (
                "sentence_recovery_near_relation_stop_reason_similarity_comparison_limit",
                usize::from(similarity_comparison_limit),
            ),
            (
                "sentence_recovery_near_relation_stop_reason_candidate_count_limit",
                usize::from(candidate_count_limit),
            ),
            (
                "sentence_recovery_vetoed_near_pairs",
                sentence.vetoed_near_pairs,
            ),
            (
                "sentence_recovery_recovered_exact_match_old_tokens",
                sentence.recovered_exact_match_old_tokens,
            ),
            (
                "sentence_recovery_recovered_exact_match_new_tokens",
                sentence.recovered_exact_match_new_tokens,
            ),
            (
                "sentence_recovery_recovered_replacement_old_tokens",
                sentence.recovered_replacement_old_tokens,
            ),
            (
                "sentence_recovery_recovered_replacement_new_tokens",
                sentence.recovered_replacement_new_tokens,
            ),
            (
                "sentence_recovery_recovered_deletion_tokens",
                sentence.recovered_deletion_tokens,
            ),
            (
                "sentence_recovery_recovered_insertion_tokens",
                sentence.recovered_insertion_tokens,
            ),
            (
                "sentence_recovery_unresolved_remainder_old_source_tokens",
                sentence.unresolved_remainder_old_source_tokens,
            ),
            (
                "sentence_recovery_unresolved_remainder_new_source_tokens",
                sentence.unresolved_remainder_new_source_tokens,
            ),
        ]);
    }
    flattened
}

fn pipeline_phase_name(phase: PipelinePhase) -> &'static str {
    match phase {
        PipelinePhase::ConfigurationValidation => "configuration_validation",
        PipelinePhase::CompletenessGate => "completeness_gate",
        PipelinePhase::PreLayoutBudget => "pre_layout_budget",
        PipelinePhase::LineReconstruction => "line_reconstruction",
        PipelinePhase::BlockReconstruction => "block_reconstruction",
        PipelinePhase::Normalization => "normalization",
        PipelinePhase::DiffTokenBudget => "diff_token_budget",
        PipelinePhase::NgramBudget => "ngram_budget",
        PipelinePhase::FeatureBuild => "feature_build",
        PipelinePhase::CandidateIndex => "candidate_index",
        PipelinePhase::Alignment => "alignment",
        PipelinePhase::ExactDiff => "exact_diff",
    }
}

fn trace_side(side: DocumentSide) -> TraceSide {
    match side {
        DocumentSide::Old => TraceSide::Old,
        DocumentSide::New => TraceSide::New,
    }
}

fn error_parts(error: &Error) -> (&'static str, Option<&'static str>, Option<usize>) {
    match error {
        Error::Backend(_) => ("backend", None, None),
        Error::Report(_) => ("report", None, None),
        Error::InvalidConfiguration(_) => ("invalid_configuration", None, None),
        Error::Unsupported(_) => ("unsupported", None, None),
        Error::Unresolved(_) => ("unresolved", None, None),
        Error::LimitExceeded { resource, limit } => {
            ("limit_exceeded", Some(*resource), Some(*limit))
        }
        _ => ("core", None, None),
    }
}

fn bounded_message(message: &str) -> String {
    if message.len() <= MAX_ERROR_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_ERROR_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

fn expected_phases() -> Vec<(&'static str, Option<TraceSide>)> {
    use TraceSide::{New, Old};

    vec![
        ("output_validation", None),
        ("input_read", Some(Old)),
        ("pdf_parse", Some(Old)),
        ("glyph_extraction", Some(Old)),
        ("input_read", Some(New)),
        ("pdf_parse", Some(New)),
        ("glyph_extraction", Some(New)),
        ("configuration_validation", None),
        ("completeness_gate", None),
        ("pre_layout_budget", Some(Old)),
        ("pre_layout_budget", Some(New)),
        ("line_reconstruction", Some(Old)),
        ("block_reconstruction", Some(Old)),
        ("normalization", Some(Old)),
        ("line_reconstruction", Some(New)),
        ("block_reconstruction", Some(New)),
        ("normalization", Some(New)),
        ("diff_token_budget", None),
        ("ngram_budget", Some(Old)),
        ("ngram_budget", Some(New)),
        ("feature_build", Some(Old)),
        ("feature_build", Some(New)),
        ("candidate_index", Some(New)),
        ("alignment", None),
        ("exact_diff", None),
        ("report", None),
    ]
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        diff::{NearRelationStopReason, SentenceRecoveryMetrics},
        pipeline::PipelineMetrics,
    };

    use super::{bounded_message, pipeline_metrics};

    #[test]
    fn bounds_error_messages_at_utf8_boundaries() {
        let message = "あ".repeat(700);
        let bounded = bounded_message(&message);

        assert!(bounded.len() <= 2_051);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn flattens_sentence_recovery_metrics_including_real_zeros() {
        let sentence = SentenceRecoveryMetrics {
            old_trusted_run_source_tokens: 41,
            structural_pairing_available: true,
            old_structural_descriptors: 3,
            structural_candidate_pairs: 2,
            structural_duplicate_pairs: 2,
            near_relation_complete: true,
            near_pair_visits_examined: 29,
            near_pair_visits_attempted: 31,
            near_similarity_comparisons_examined: 19,
            near_similarity_comparisons_attempted: 23,
            near_largest_edge_posting: 11,
            near_largest_edge_query_union: 13,
            near_largest_filtered_candidate_set: 7,
            near_candidate_count_truncated: true,
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            recovered_deletion_tokens: 17,
            unresolved_remainder_old_source_tokens: 24,
            ..SentenceRecoveryMetrics::default()
        };

        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(sentence),
                ..PipelineMetrics::default()
            },
            None,
        );

        assert_eq!(
            metrics["sentence_recovery_old_trusted_run_source_tokens"],
            41
        );
        assert_eq!(metrics["sentence_recovery_recovered_deletion_tokens"], 17);
        assert_eq!(metrics["sentence_recovery_exact_shared_units"], 0);
        assert_eq!(metrics["sentence_recovery_structural_pairing_available"], 1);
        assert_eq!(metrics["sentence_recovery_old_structural_descriptors"], 3);
        assert_eq!(metrics["sentence_recovery_structural_candidate_pairs"], 2);
        assert_eq!(
            metrics["sentence_recovery_structural_unique_no_anchor_pairs"],
            0
        );
        assert_eq!(metrics["sentence_recovery_near_relation_complete"], 1);
        assert_eq!(metrics["sentence_recovery_near_pair_visits_examined"], 29);
        assert_eq!(metrics["sentence_recovery_near_pair_visits_attempted"], 31);
        assert_eq!(
            metrics["sentence_recovery_near_similarity_comparisons_examined"],
            19
        );
        assert_eq!(
            metrics["sentence_recovery_near_similarity_comparisons_attempted"],
            23
        );
        assert_eq!(metrics["sentence_recovery_near_largest_edge_posting"], 11);
        assert_eq!(
            metrics["sentence_recovery_near_largest_edge_query_union"],
            13
        );
        assert_eq!(
            metrics["sentence_recovery_near_largest_filtered_candidate_set"],
            7
        );
        assert_eq!(
            metrics["sentence_recovery_near_candidate_count_truncated"],
            1
        );
        assert_eq!(
            metrics["sentence_recovery_near_relation_stop_reason_pair_visit_limit"],
            0
        );
        assert_eq!(
            metrics["sentence_recovery_near_relation_stop_reason_similarity_comparison_limit"],
            1
        );
        assert_eq!(
            metrics["sentence_recovery_near_relation_stop_reason_candidate_count_limit"],
            0
        );
        assert_eq!(
            metrics["sentence_recovery_unresolved_remainder_old_source_tokens"],
            24
        );
    }

    #[test]
    fn flattens_each_near_relation_stop_reason_as_one_hot() {
        let cases = [
            (
                NearRelationStopReason::PairVisitLimit,
                "sentence_recovery_near_relation_stop_reason_pair_visit_limit",
            ),
            (
                NearRelationStopReason::SimilarityComparisonLimit,
                "sentence_recovery_near_relation_stop_reason_similarity_comparison_limit",
            ),
            (
                NearRelationStopReason::CandidateCountLimit,
                "sentence_recovery_near_relation_stop_reason_candidate_count_limit",
            ),
        ];

        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        near_relation_stop_reason: Some(reason),
                        ..SentenceRecoveryMetrics::default()
                    }),
                    ..PipelineMetrics::default()
                },
                None,
            );

            for (_, key) in cases {
                assert_eq!(metrics[key], usize::from(key == expected));
            }
        }
    }

    #[test]
    fn omits_sentence_recovery_metrics_when_unavailable() {
        let metrics = pipeline_metrics(PipelineMetrics::default(), None);

        assert!(
            !metrics
                .keys()
                .any(|name| name.starts_with("sentence_recovery_"))
        );
    }
}
