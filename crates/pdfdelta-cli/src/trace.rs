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

const TRACE_SCHEMA_VERSION: u8 = 20;
const MAX_ERROR_MESSAGE_BYTES: usize = 2_048;

macro_rules! extend_near_scope_metrics {
    ($metrics:expr, $scope:literal, $work:expr) => {{
        let work = $work;
        $metrics.extend([
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_edge_posting_visits_examined"
                ),
                work.sentence_work.edge_posting_visits_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_edge_posting_visits_attempted"
                ),
                work.sentence_work.edge_posting_visits_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_line_trigram_posting_visits_examined"
                ),
                work.sentence_work.line_trigram_posting_visits_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_line_trigram_posting_visits_attempted"
                ),
                work.sentence_work.line_trigram_posting_visits_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_edge_query_union_candidates"
                ),
                work.sentence_work.edge_query_union_candidates,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_line_trigram_only_query_union_candidates"
                ),
                work.sentence_work.line_trigram_only_query_union_candidates,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_filtered_candidates"
                ),
                work.sentence_work.filtered_candidates,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_pair_visits_examined"
                ),
                work.sentence_work.pair_visits_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_pair_visits_attempted"
                ),
                work.sentence_work.pair_visits_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_similarity_comparisons_examined"
                ),
                work.sentence_work.similarity_comparisons_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_sentence_work_similarity_comparisons_attempted"
                ),
                work.sentence_work.similarity_comparisons_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_edge_posting_visits_examined"
                ),
                work.line_work.edge_posting_visits_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_edge_posting_visits_attempted"
                ),
                work.line_work.edge_posting_visits_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_line_trigram_posting_visits_examined"
                ),
                work.line_work.line_trigram_posting_visits_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_line_trigram_posting_visits_attempted"
                ),
                work.line_work.line_trigram_posting_visits_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_edge_query_union_candidates"
                ),
                work.line_work.edge_query_union_candidates,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_line_trigram_only_query_union_candidates"
                ),
                work.line_work.line_trigram_only_query_union_candidates,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_filtered_candidates"
                ),
                work.line_work.filtered_candidates,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_pair_visits_examined"
                ),
                work.line_work.pair_visits_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_pair_visits_attempted"
                ),
                work.line_work.pair_visits_attempted,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_similarity_comparisons_examined"
                ),
                work.line_work.similarity_comparisons_examined,
            ),
            (
                concat!(
                    "sentence_recovery_",
                    $scope,
                    "_line_work_similarity_comparisons_attempted"
                ),
                work.line_work.similarity_comparisons_attempted,
            ),
        ]);
    }};
}

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
        use pdfdelta_core::diff::SentenceEdgeFilterStopReason;

        let candidate_posting_visit_limit = matches!(
            sentence.near_relation_stop_reason,
            Some(pdfdelta_core::diff::NearRelationStopReason::CandidatePostingVisitLimit)
        );
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
        let run_signature_posting_limit = matches!(
            sentence.run_signature_stop_reason,
            Some(pdfdelta_core::diff::RunSignatureStopReason::PostingVisitLimit)
        );
        let run_signature_verification_limit = matches!(
            sentence.run_signature_stop_reason,
            Some(pdfdelta_core::diff::RunSignatureStopReason::TokenVerificationLimit)
        );
        let run_signature_candidate_limit = matches!(
            sentence.run_signature_stop_reason,
            Some(pdfdelta_core::diff::RunSignatureStopReason::CandidatePairLimit)
        );
        let sentence_edge_filter_stop_reasons = match sentence.sentence_edge_filter_stop_reason {
            None => [false; 4],
            Some(SentenceEdgeFilterStopReason::PairVisitLimit) => [true, false, false, false],
            Some(SentenceEdgeFilterStopReason::SimilarityComparisonLimit) => {
                [false, true, false, false]
            }
            Some(SentenceEdgeFilterStopReason::AllocationFailure) => [false, false, true, false],
            Some(SentenceEdgeFilterStopReason::CounterOverflow) => [false, false, false, true],
        };
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
                "sentence_recovery_run_signature_available",
                usize::from(sentence.run_signature_available),
            ),
            (
                "sentence_recovery_run_signature_complete",
                usize::from(sentence.run_signature_complete),
            ),
            (
                "sentence_recovery_old_run_signature_unique_units",
                sentence.old_run_signature_unique_units,
            ),
            (
                "sentence_recovery_new_run_signature_unique_units",
                sentence.new_run_signature_unique_units,
            ),
            (
                "sentence_recovery_old_run_signature_duplicate_units",
                sentence.old_run_signature_duplicate_units,
            ),
            (
                "sentence_recovery_new_run_signature_duplicate_units",
                sentence.new_run_signature_duplicate_units,
            ),
            (
                "sentence_recovery_run_signature_shared_unit_keys",
                sentence.run_signature_shared_unit_keys,
            ),
            (
                "sentence_recovery_run_signature_largest_posting",
                sentence.run_signature_largest_posting,
            ),
            (
                "sentence_recovery_run_signature_posting_visits_attempted",
                sentence.run_signature_posting_visits_attempted,
            ),
            (
                "sentence_recovery_run_signature_posting_visits_examined",
                sentence.run_signature_posting_visits_examined,
            ),
            (
                "sentence_recovery_run_signature_token_verifications_attempted",
                sentence.run_signature_token_verifications_attempted,
            ),
            (
                "sentence_recovery_run_signature_token_verifications_examined",
                sentence.run_signature_token_verifications_examined,
            ),
            (
                "sentence_recovery_run_signature_candidate_pairs",
                sentence.run_signature_candidate_pairs,
            ),
            (
                "sentence_recovery_run_signature_globally_anchored_runs_skipped",
                sentence.run_signature_globally_anchored_runs_skipped,
            ),
            (
                "sentence_recovery_run_signature_reciprocal_unique_pairs",
                sentence.run_signature_reciprocal_unique_pairs,
            ),
            (
                "sentence_recovery_run_signature_margin_qualified_pairs",
                sentence.run_signature_margin_qualified_pairs,
            ),
            (
                "sentence_recovery_run_signature_margin_veto_pairs",
                sentence.run_signature_margin_veto_pairs,
            ),
            (
                "sentence_recovery_run_signature_monotone_pairs",
                sentence.run_signature_monotone_pairs,
            ),
            (
                "sentence_recovery_run_signature_crossing_veto_pairs",
                sentence.run_signature_crossing_veto_pairs,
            ),
            (
                "sentence_recovery_run_signature_max_shared_units",
                sentence.run_signature_max_shared_units,
            ),
            (
                "sentence_recovery_run_signature_stop_reason_posting_visit_limit",
                usize::from(run_signature_posting_limit),
            ),
            (
                "sentence_recovery_run_signature_stop_reason_token_verification_limit",
                usize::from(run_signature_verification_limit),
            ),
            (
                "sentence_recovery_run_signature_stop_reason_candidate_pair_limit",
                usize::from(run_signature_candidate_limit),
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
                "sentence_recovery_near_sentence_work_edge_posting_visits_examined",
                sentence.near_sentence_work.edge_posting_visits_examined,
            ),
            (
                "sentence_recovery_near_sentence_work_edge_posting_visits_attempted",
                sentence.near_sentence_work.edge_posting_visits_attempted,
            ),
            (
                "sentence_recovery_near_sentence_work_line_trigram_posting_visits_examined",
                sentence
                    .near_sentence_work
                    .line_trigram_posting_visits_examined,
            ),
            (
                "sentence_recovery_near_sentence_work_line_trigram_posting_visits_attempted",
                sentence
                    .near_sentence_work
                    .line_trigram_posting_visits_attempted,
            ),
            (
                "sentence_recovery_near_sentence_work_edge_query_union_candidates",
                sentence.near_sentence_work.edge_query_union_candidates,
            ),
            (
                "sentence_recovery_near_sentence_work_line_trigram_only_query_union_candidates",
                sentence
                    .near_sentence_work
                    .line_trigram_only_query_union_candidates,
            ),
            (
                "sentence_recovery_near_sentence_work_filtered_candidates",
                sentence.near_sentence_work.filtered_candidates,
            ),
            (
                "sentence_recovery_near_sentence_work_pair_visits_examined",
                sentence.near_sentence_work.pair_visits_examined,
            ),
            (
                "sentence_recovery_near_sentence_work_pair_visits_attempted",
                sentence.near_sentence_work.pair_visits_attempted,
            ),
            (
                "sentence_recovery_near_sentence_work_similarity_comparisons_examined",
                sentence.near_sentence_work.similarity_comparisons_examined,
            ),
            (
                "sentence_recovery_near_sentence_work_similarity_comparisons_attempted",
                sentence.near_sentence_work.similarity_comparisons_attempted,
            ),
            (
                "sentence_recovery_near_line_work_edge_posting_visits_examined",
                sentence.near_line_work.edge_posting_visits_examined,
            ),
            (
                "sentence_recovery_near_line_work_edge_posting_visits_attempted",
                sentence.near_line_work.edge_posting_visits_attempted,
            ),
            (
                "sentence_recovery_near_line_work_line_trigram_posting_visits_examined",
                sentence.near_line_work.line_trigram_posting_visits_examined,
            ),
            (
                "sentence_recovery_near_line_work_line_trigram_posting_visits_attempted",
                sentence
                    .near_line_work
                    .line_trigram_posting_visits_attempted,
            ),
            (
                "sentence_recovery_near_line_work_edge_query_union_candidates",
                sentence.near_line_work.edge_query_union_candidates,
            ),
            (
                "sentence_recovery_near_line_work_line_trigram_only_query_union_candidates",
                sentence
                    .near_line_work
                    .line_trigram_only_query_union_candidates,
            ),
            (
                "sentence_recovery_near_line_work_filtered_candidates",
                sentence.near_line_work.filtered_candidates,
            ),
            (
                "sentence_recovery_near_line_work_pair_visits_examined",
                sentence.near_line_work.pair_visits_examined,
            ),
            (
                "sentence_recovery_near_line_work_pair_visits_attempted",
                sentence.near_line_work.pair_visits_attempted,
            ),
            (
                "sentence_recovery_near_line_work_similarity_comparisons_examined",
                sentence.near_line_work.similarity_comparisons_examined,
            ),
            (
                "sentence_recovery_near_line_work_similarity_comparisons_attempted",
                sentence.near_line_work.similarity_comparisons_attempted,
            ),
            (
                "sentence_recovery_near_pair_candidates",
                sentence.near_pair_candidates,
            ),
            (
                "sentence_recovery_relation_floor_pairs_considered",
                sentence.relation_floor_pairs_considered,
            ),
            (
                "sentence_recovery_relation_floor_word_scans",
                sentence.relation_floor_word_scans,
            ),
            (
                "sentence_recovery_relation_floor_stop_opportunities",
                sentence.relation_floor_stop_opportunities,
            ),
            (
                "sentence_recovery_relation_floor_potential_saved_word_comparisons",
                sentence.relation_floor_potential_saved_word_comparisons,
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
                "sentence_recovery_near_candidate_posting_visits_examined",
                sentence.near_candidate_posting_visits_examined,
            ),
            (
                "sentence_recovery_near_candidate_posting_visits_attempted",
                sentence.near_candidate_posting_visits_attempted,
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
                "sentence_recovery_near_relation_stop_reason_candidate_posting_visit_limit",
                usize::from(candidate_posting_visit_limit),
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
                "sentence_recovery_sentence_edge_filter_complete",
                usize::from(sentence.sentence_edge_filter_complete),
            ),
            (
                "sentence_recovery_sentence_edge_filter_pairs_examined",
                sentence.sentence_edge_filter_pairs_examined,
            ),
            (
                "sentence_recovery_sentence_edge_filter_pairs_attempted",
                sentence.sentence_edge_filter_pairs_attempted,
            ),
            (
                "sentence_recovery_sentence_edge_filter_similarity_comparisons_examined",
                sentence.sentence_edge_filter_similarity_comparisons_examined,
            ),
            (
                "sentence_recovery_sentence_edge_filter_similarity_comparisons_attempted",
                sentence.sentence_edge_filter_similarity_comparisons_attempted,
            ),
            (
                "sentence_recovery_sentence_edge_filter_pairs_retained",
                sentence.sentence_edge_filter_pairs_retained,
            ),
            (
                "sentence_recovery_sentence_edge_filter_pairs_rejected",
                sentence.sentence_edge_filter_pairs_rejected,
            ),
            (
                "sentence_recovery_sentence_edge_filter_stop_reason_pair_visit_limit",
                usize::from(sentence_edge_filter_stop_reasons[0]),
            ),
            (
                "sentence_recovery_sentence_edge_filter_stop_reason_similarity_comparison_limit",
                usize::from(sentence_edge_filter_stop_reasons[1]),
            ),
            (
                "sentence_recovery_sentence_edge_filter_stop_reason_allocation_failure",
                usize::from(sentence_edge_filter_stop_reasons[2]),
            ),
            (
                "sentence_recovery_sentence_edge_filter_stop_reason_counter_overflow",
                usize::from(sentence_edge_filter_stop_reasons[3]),
            ),
            (
                "sentence_recovery_sentence_edge_filter_full_build_fallback_used",
                usize::from(sentence.sentence_edge_filter_full_build_fallback_used),
            ),
            (
                "sentence_recovery_sentence_edge_filter_discarded_near_pair_visits_examined",
                sentence.sentence_edge_filter_discarded_near_pair_visits_examined,
            ),
            (
                "sentence_recovery_sentence_edge_filter_discarded_near_pair_visits_attempted",
                sentence.sentence_edge_filter_discarded_near_pair_visits_attempted,
            ),
            (
                "sentence_recovery_sentence_edge_filter_discarded_near_similarity_comparisons_examined",
                sentence.sentence_edge_filter_discarded_near_similarity_comparisons_examined,
            ),
            (
                "sentence_recovery_sentence_edge_filter_discarded_near_similarity_comparisons_attempted",
                sentence.sentence_edge_filter_discarded_near_similarity_comparisons_attempted,
            ),
            (
                "sentence_recovery_sentence_edge_filter_discarded_near_candidate_posting_visits_examined",
                sentence.sentence_edge_filter_discarded_near_candidate_posting_visits_examined,
            ),
            (
                "sentence_recovery_sentence_edge_filter_discarded_near_candidate_posting_visits_attempted",
                sentence.sentence_edge_filter_discarded_near_candidate_posting_visits_attempted,
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
        extend_near_scope_metrics!(
            flattened,
            "near_paired_interval_work",
            sentence.near_paired_interval_work
        );
        extend_near_scope_metrics!(
            flattened,
            "near_paired_cross_interval_veto_work",
            sentence.near_paired_cross_interval_veto_work
        );
        extend_near_scope_metrics!(
            flattened,
            "near_same_or_ambiguous_span_work",
            sentence.near_same_or_ambiguous_span_work
        );
        extend_near_scope_metrics!(
            flattened,
            "near_same_known_span_work",
            sentence.near_same_known_span_work
        );
        extend_near_scope_metrics!(
            flattened,
            "near_ambiguous_span_work",
            sentence.near_ambiguous_span_work
        );
        extend_near_scope_metrics!(
            flattened,
            "near_same_or_ambiguous_shared_query_work",
            sentence.near_same_or_ambiguous_shared_query_work
        );
        extend_near_scope_metrics!(
            flattened,
            "near_cross_span_work",
            sentence.near_cross_span_work
        );
        if let Some(shadow) = sentence.known_span_sentence_shadow {
            flattened.extend([
                (
                    "sentence_recovery_known_span_sentence_shadow_complete",
                    usize::from(shadow.complete),
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_pairs_considered",
                    shadow.pairs_considered,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_pairs_retained",
                    shadow.pairs_retained,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_pairs_rejected",
                    shadow.pairs_rejected,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_cross_span_pairs_considered",
                    shadow.cross_span_pairs_considered,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_same_paired_anchor_interval_pairs",
                    shadow.same_paired_anchor_interval_pairs,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_same_paired_stream_other_interval_pairs",
                    shadow.same_paired_stream_other_interval_pairs,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_same_page_only_pairs",
                    shadow.same_page_only_pairs,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_unclassified_pairs",
                    shadow.unclassified_pairs,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_old_relation_mismatches",
                    shadow.old_relation_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_new_relation_mismatches",
                    shadow.new_relation_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_best_partner_mismatches",
                    shadow.best_partner_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_best_score_mismatches",
                    shadow.best_score_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_second_score_mismatches",
                    shadow.second_score_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_veto_mismatches",
                    shadow.veto_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_unique_partner_mismatches",
                    shadow.unique_partner_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_reciprocal_pair_mismatches",
                    shadow.reciprocal_pair_mismatches,
                ),
                (
                    "sentence_recovery_known_span_sentence_shadow_exact_relation_parity",
                    usize::from(shadow.exact_relation_parity),
                ),
            ]);
        }
        if let Some(shadow) = sentence.sentence_edge_gate_shadow {
            use pdfdelta_core::diff::SentenceEdgeGateShadowStopReason;

            flattened.extend([
                (
                    "sentence_recovery_sentence_edge_gate_shadow_complete",
                    usize::from(shadow.complete),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_candidate_posting_visit_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::CandidatePostingVisitLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_pair_visit_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::PairVisitLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_similarity_comparison_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::SimilarityComparisonLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_candidate_count_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::CandidateCountLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_allocation_failure",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::AllocationFailure)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_counter_overflow",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::CounterOverflow)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_stop_reason_diagnostic_failure",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeGateShadowStopReason::DiagnosticFailure)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_pairs_considered",
                    shadow.pairs_considered,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_pairs_retained",
                    shadow.pairs_retained,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_pairs_rejected",
                    shadow.pairs_rejected,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_same_known_rejected",
                    shadow.same_known_rejected,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_ambiguous_rejected",
                    shadow.ambiguous_rejected,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_cross_span_rejected",
                    shadow.cross_span_rejected,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_unclassified_rejected",
                    shadow.unclassified_rejected,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_projected_pair_visits",
                    shadow.projected_pair_visits,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_projected_similarity_comparisons",
                    shadow.projected_similarity_comparisons,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_rejected_max_production_score",
                    usize::from(shadow.rejected_max_production_score),
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_threshold_violations",
                    shadow.threshold_violations,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_veto_mismatches",
                    shadow.veto_mismatches,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_unique_partner_mismatches",
                    shadow.unique_partner_mismatches,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_reciprocal_pair_mismatches",
                    shadow.reciprocal_pair_mismatches,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_adopted_replacement_mismatches",
                    shadow.adopted_replacement_mismatches,
                ),
                (
                    "sentence_recovery_sentence_edge_gate_shadow_insertion_deletion_veto_mismatches",
                    shadow.insertion_deletion_veto_mismatches,
                ),
            ]);
        }
        if let Some(shadow) = sentence.sentence_edge_signature_shadow {
            use pdfdelta_core::diff::SentenceEdgeSignatureShadowStopReason;

            flattened.extend([
                (
                    "sentence_recovery_sentence_edge_signature_shadow_complete",
                    usize::from(shadow.complete),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_index_posting_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::IndexPostingLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_query_posting_visit_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_allocation_failure",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::AllocationFailure)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_counter_overflow",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::CounterOverflow)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_production_traversal_incomplete",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(
                            SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete
                        )
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_candidate_posting_visit_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::CandidatePostingVisitLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_pair_visit_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::PairVisitLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_similarity_comparison_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_candidate_count_limit",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::CandidateCountLimit)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_stop_reason_diagnostic_failure",
                    usize::from(matches!(
                        shadow.stop_reason,
                        Some(SentenceEdgeSignatureShadowStopReason::DiagnosticFailure)
                    )),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_index_posting_items_examined",
                    shadow.index_posting_items_examined,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_index_posting_items_attempted",
                    shadow.index_posting_items_attempted,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_query_posting_visits_examined",
                    shadow.query_posting_visits_examined,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_query_posting_visits_attempted",
                    shadow.query_posting_visits_attempted,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_pairs_considered",
                    shadow.pairs_considered,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_signature_candidates",
                    shadow.signature_candidates,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_projected_pairs_pruned",
                    shadow.projected_pairs_pruned,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_exact_edge_retained_pairs",
                    shadow.exact_edge_retained_pairs,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_verification_evaluable",
                    usize::from(shadow.verification_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_retained_pair_misses",
                    shadow.retained_pair_misses,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_signature_not_in_edge_union",
                    shadow.signature_not_in_edge_union,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_largest_signature_candidate_set",
                    shadow.largest_signature_candidate_set,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_paired_interval_pairs",
                    shadow.paired_interval_pairs,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_paired_cross_interval_pairs",
                    shadow.paired_cross_interval_pairs,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_same_known_pairs",
                    shadow.same_known_pairs,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_ambiguous_pairs",
                    shadow.ambiguous_pairs,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_cross_span_shared_pairs",
                    shadow.cross_span_shared_pairs,
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_parity_evaluable",
                    usize::from(shadow.parity_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_shadow_plan_parity",
                    usize::from(shadow.plan_parity),
                ),
            ]);
        }
        if let Some(shadow) = sentence.sentence_edge_signature_direct_shadow {
            use pdfdelta_core::diff::SentenceEdgeSignatureDirectShadowStopReason as Stop;

            macro_rules! direct_stop_reasons {
                ($(($name:literal, $reason:ident)),+ $(,)?) => {
                    $(flattened.insert(
                        concat!(
                            "sentence_recovery_sentence_edge_signature_direct_shadow_stop_reason_",
                            $name
                        ),
                        usize::from(shadow.stop_reason == Some(Stop::$reason)),
                    );)+
                };
            }
            direct_stop_reasons!(
                ("signature_index_posting_limit", SignatureIndexPostingLimit),
                (
                    "signature_index_distinct_key_limit",
                    SignatureIndexDistinctKeyLimit
                ),
                (
                    "signature_index_estimated_byte_limit",
                    SignatureIndexEstimatedByteLimit
                ),
                ("signature_query_count_limit", SignatureQueryCountLimit),
                (
                    "signature_query_posting_visit_limit",
                    SignatureQueryPostingVisitLimit
                ),
                (
                    "signature_candidate_union_limit",
                    SignatureCandidateUnionLimit
                ),
                (
                    "signature_exact_edge_recheck_limit",
                    SignatureExactEdgeRecheckLimit
                ),
                ("direct_edge_pair_visit_limit", DirectEdgePairVisitLimit),
                (
                    "direct_edge_similarity_comparison_limit",
                    DirectEdgeSimilarityComparisonLimit
                ),
                ("candidate_posting_visit_limit", CandidatePostingVisitLimit),
                ("pair_visit_limit", PairVisitLimit),
                ("similarity_comparison_limit", SimilarityComparisonLimit),
                ("candidate_count_limit", CandidateCountLimit),
                ("fragment_veto_pair_visit_limit", FragmentVetoPairVisitLimit),
                (
                    "fragment_veto_similarity_comparison_limit",
                    FragmentVetoSimilarityComparisonLimit
                ),
                ("fragment_veto_incomplete", FragmentVetoIncomplete),
                ("watch_probe_pair_limit", WatchProbePairLimit),
                (
                    "watch_probe_similarity_comparison_limit",
                    WatchProbeSimilarityComparisonLimit
                ),
                (
                    "watch_probe_invariant_violation",
                    WatchProbeInvariantViolation
                ),
                ("watch_diagnostics_mismatch", WatchDiagnosticsMismatch),
                ("allocation_failure", AllocationFailure),
                ("counter_overflow", CounterOverflow),
                (
                    "production_traversal_incomplete",
                    ProductionTraversalIncomplete
                ),
                ("diagnostic_failure", DiagnosticFailure),
            );
            macro_rules! direct_metrics {
                ($($field:ident),+ $(,)?) => {
                    $(flattened.insert(
                        concat!(
                            "sentence_recovery_sentence_edge_signature_direct_shadow_",
                            stringify!($field)
                    ),
                        shadow.$field,
                    );)+
                };
            }
            flattened.insert(
                "sentence_recovery_sentence_edge_signature_direct_shadow_complete",
                usize::from(shadow.complete),
            );
            direct_metrics!(
                signature_index_items_examined,
                signature_index_items_attempted,
                signature_query_visits_examined,
                signature_query_visits_attempted,
                signature_index_own_distinct_keys,
                signature_index_all_distinct_keys,
                signature_index_distinct_keys_examined,
                signature_index_distinct_keys_attempted,
                signature_index_own_key_capacity,
                signature_index_all_key_capacity,
                signature_index_own_posting_items,
                signature_index_all_posting_items,
                signature_index_posting_capacity_items,
                signature_index_largest_posting,
                signature_index_estimated_logical_bytes,
                signature_index_estimated_logical_bytes_examined,
                signature_index_estimated_logical_bytes_attempted,
                signature_index_depth_1_posting_items,
                signature_index_depth_2_to_3_posting_items,
                signature_index_depth_4_plus_posting_items,
                signature_queries,
                signature_queries_attempted,
                signature_depth_1_queries,
                signature_depth_2_to_3_queries,
                signature_depth_4_plus_queries,
                signature_depth_1_candidate_union,
                signature_depth_2_to_3_candidate_union,
                signature_depth_4_plus_candidate_union,
                direct_candidates,
                signature_candidate_union_attempted,
                paired_interval_candidates,
                paired_cross_interval_candidates,
                same_known_candidates,
                ambiguous_candidates,
                cross_span_candidates,
                edge_filter_pairs_examined,
                edge_filter_pairs_attempted,
                edge_filter_comparisons_examined,
                edge_filter_comparisons_attempted,
                exact_edge_retained_pairs,
                exact_edge_rechecks,
                exact_edge_rechecks_attempted,
                exact_edge_recheck_comparisons_examined,
                exact_edge_recheck_comparisons_attempted,
                exact_edge_rejected_pairs,
                cross_orientation_only_candidates,
                sentence_broad_edge_postings_examined,
                sentence_broad_edge_postings_attempted,
                downstream_candidate_postings_examined,
                downstream_candidate_postings_attempted,
                downstream_pair_visits_examined,
                downstream_pair_visits_attempted,
                downstream_similarity_comparisons_examined,
                downstream_similarity_comparisons_attempted,
                fragment_veto_pair_visits_examined,
                fragment_veto_pair_visits_attempted,
                fragment_veto_similarity_comparisons_examined,
                fragment_veto_similarity_comparisons_attempted,
                watch_probe_pairs_examined,
                watch_probe_pairs_attempted,
                watch_probe_similarity_comparisons_examined,
                watch_probe_similarity_comparisons_attempted,
                watch_probe_missing_signature_candidates,
                watch_probe_invariant_violations,
                watch_preservation_mismatches,
                retained_pair_misses,
                retained_pair_count_mismatches,
                retained_pair_set_mismatches,
                retained_pair_order_mismatches,
            );
            flattened.extend([
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_candidate_count_truncated",
                    usize::from(shadow.candidate_count_truncated),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_parity_evaluable",
                    usize::from(shadow.parity_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_plan_parity",
                    usize::from(shadow.plan_parity),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_verification_evaluable",
                    usize::from(shadow.verification_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_watch_preservation_evaluable",
                    usize::from(shadow.watch_preservation_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_watch_evidence_preserved",
                    usize::from(shadow.watch_evidence_preserved),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_watch_exact_parity_evaluable",
                    usize::from(shadow.watch_exact_parity_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_direct_shadow_watch_exact_parity",
                    usize::from(shadow.watch_exact_parity),
                ),
            ]);
        }
        if let Some(oracle) = sentence.sentence_edge_signature_reference_oracle {
            use pdfdelta_core::diff::SentenceEdgeSignatureReferenceOracleStopReason as Stop;

            macro_rules! reference_stop_reasons {
                ($(($name:literal, $reason:ident)),+ $(,)?) => {
                    $(flattened.insert(
                        concat!(
                            "sentence_recovery_sentence_edge_signature_reference_oracle_stop_reason_",
                            $name
                        ),
                        usize::from(oracle.stop_reason == Some(Stop::$reason)),
                    );)+
                };
            }
            reference_stop_reasons!(
                ("direct_replay_incomplete", DirectReplayIncomplete),
                ("candidate_posting_visit_limit", CandidatePostingVisitLimit),
                ("pair_visit_limit", PairVisitLimit),
                ("similarity_comparison_limit", SimilarityComparisonLimit),
                ("candidate_count_limit", CandidateCountLimit),
                ("fragment_veto_pair_visit_limit", FragmentVetoPairVisitLimit),
                (
                    "fragment_veto_similarity_comparison_limit",
                    FragmentVetoSimilarityComparisonLimit
                ),
                ("fragment_veto_incomplete", FragmentVetoIncomplete),
                ("allocation_failure", AllocationFailure),
                ("counter_overflow", CounterOverflow),
                (
                    "production_traversal_incomplete",
                    ProductionTraversalIncomplete
                ),
                ("diagnostic_failure", DiagnosticFailure),
            );
            macro_rules! reference_metrics {
                ($($field:ident),+ $(,)?) => {
                    $(flattened.insert(
                        concat!(
                            "sentence_recovery_sentence_edge_signature_reference_oracle_",
                            stringify!($field)
                        ),
                        oracle.$field,
                    );)+
                };
            }
            flattened.extend([
                (
                    "sentence_recovery_sentence_edge_signature_reference_oracle_complete",
                    usize::from(oracle.complete),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_reference_oracle_direct_complete",
                    usize::from(oracle.direct_complete),
                ),
            ]);
            reference_metrics!(
                candidate_posting_visits_examined,
                candidate_posting_visits_attempted,
                pair_visits_examined,
                pair_visits_attempted,
                similarity_comparisons_examined,
                similarity_comparisons_attempted,
                legacy_sentence_edge_pairs_examined,
                legacy_sentence_edge_pairs_attempted,
                legacy_sentence_edge_pairs_retained,
                legacy_sentence_edge_pairs_rejected,
                fragment_veto_pair_visits_examined,
                fragment_veto_pair_visits_attempted,
                fragment_veto_similarity_comparisons_examined,
                fragment_veto_similarity_comparisons_attempted,
                retained_pair_misses,
                retained_pair_count_mismatches,
                retained_pair_set_mismatches,
                retained_pair_order_mismatches,
            );
            flattened.extend([
                (
                    "sentence_recovery_sentence_edge_signature_reference_oracle_candidate_count_truncated",
                    usize::from(oracle.candidate_count_truncated),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_reference_oracle_plan_parity_evaluable",
                    usize::from(oracle.plan_parity_evaluable),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_reference_oracle_plan_parity",
                    usize::from(oracle.plan_parity),
                ),
                (
                    "sentence_recovery_sentence_edge_signature_reference_oracle_fingerprint_evaluable",
                    usize::from(oracle.fingerprint_evaluable),
                ),
            ]);
        }
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
        diff::{
            KnownSpanSentenceShadowMetrics, NearRelationStopReason, RunSignatureStopReason,
            SentenceEdgeFilterStopReason, SentenceEdgeGateShadowMetrics,
            SentenceEdgeGateShadowStopReason, SentenceEdgeSignatureDirectShadowMetrics,
            SentenceEdgeSignatureDirectShadowStopReason,
            SentenceEdgeSignatureReferenceOracleMetrics,
            SentenceEdgeSignatureReferenceOracleStopReason, SentenceEdgeSignatureShadowMetrics,
            SentenceEdgeSignatureShadowStopReason, SentenceRecoveryMetrics,
        },
        pipeline::PipelineMetrics,
    };

    use super::{TRACE_SCHEMA_VERSION, bounded_message, pipeline_metrics};

    #[test]
    fn trace_schema_version_covers_sentence_edge_signature_watch_probe_metrics() {
        assert_eq!(TRACE_SCHEMA_VERSION, 20);
    }

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
            run_signature_available: true,
            run_signature_complete: false,
            old_run_signature_unique_units: 7,
            run_signature_shared_unit_keys: 3,
            run_signature_largest_posting: 4,
            run_signature_posting_visits_attempted: 9,
            run_signature_posting_visits_examined: 8,
            run_signature_candidate_pairs: 5,
            run_signature_stop_reason: Some(RunSignatureStopReason::PostingVisitLimit),
            near_relation_complete: true,
            relation_floor_pairs_considered: 17,
            relation_floor_word_scans: 13,
            relation_floor_stop_opportunities: 5,
            relation_floor_potential_saved_word_comparisons: 23,
            near_pair_visits_examined: 29,
            near_pair_visits_attempted: 31,
            near_similarity_comparisons_examined: 19,
            near_similarity_comparisons_attempted: 23,
            near_candidate_posting_visits_examined: 31,
            near_candidate_posting_visits_attempted: 37,
            near_largest_edge_posting: 11,
            near_largest_edge_query_union: 13,
            near_largest_filtered_candidate_set: 7,
            near_candidate_count_truncated: true,
            near_relation_stop_reason: Some(NearRelationStopReason::SimilarityComparisonLimit),
            known_span_sentence_shadow: Some(KnownSpanSentenceShadowMetrics {
                complete: true,
                pairs_considered: 11,
                pairs_retained: 7,
                pairs_rejected: 4,
                cross_span_pairs_considered: 11,
                same_paired_anchor_interval_pairs: 7,
                same_paired_stream_other_interval_pairs: 0,
                same_page_only_pairs: 0,
                unclassified_pairs: 4,
                old_relation_mismatches: 3,
                new_relation_mismatches: 2,
                best_partner_mismatches: 2,
                best_score_mismatches: 1,
                second_score_mismatches: 4,
                veto_mismatches: 1,
                unique_partner_mismatches: 2,
                reciprocal_pair_mismatches: 1,
                exact_relation_parity: false,
            }),
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
        assert_eq!(metrics["sentence_recovery_run_signature_available"], 1);
        assert_eq!(metrics["sentence_recovery_run_signature_complete"], 0);
        assert_eq!(
            metrics["sentence_recovery_old_run_signature_unique_units"],
            7
        );
        assert_eq!(
            metrics["sentence_recovery_run_signature_candidate_pairs"],
            5
        );
        assert_eq!(
            metrics["sentence_recovery_run_signature_stop_reason_posting_visit_limit"],
            1
        );
        assert_eq!(
            metrics["sentence_recovery_run_signature_stop_reason_token_verification_limit"],
            0
        );
        assert_eq!(metrics["sentence_recovery_near_relation_complete"], 1);
        assert_eq!(
            metrics["sentence_recovery_relation_floor_pairs_considered"],
            17
        );
        assert_eq!(metrics["sentence_recovery_relation_floor_word_scans"], 13);
        assert_eq!(
            metrics["sentence_recovery_relation_floor_stop_opportunities"],
            5
        );
        assert_eq!(
            metrics["sentence_recovery_relation_floor_potential_saved_word_comparisons"],
            23
        );
        assert_eq!(
            metrics["sentence_recovery_known_span_sentence_shadow_pairs_considered"],
            11
        );
        assert_eq!(
            metrics["sentence_recovery_known_span_sentence_shadow_pairs_rejected"],
            4
        );
        assert_eq!(
            metrics["sentence_recovery_known_span_sentence_shadow_cross_span_pairs_considered"],
            11
        );
        assert_eq!(
            metrics["sentence_recovery_known_span_sentence_shadow_same_paired_anchor_interval_pairs"],
            7
        );
        assert_eq!(
            metrics["sentence_recovery_known_span_sentence_shadow_unclassified_pairs"],
            4
        );
        assert_eq!(
            metrics["sentence_recovery_known_span_sentence_shadow_exact_relation_parity"],
            0
        );
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
        assert_eq!(
            metrics["sentence_recovery_near_candidate_posting_visits_examined"],
            31
        );
        assert_eq!(
            metrics["sentence_recovery_near_candidate_posting_visits_attempted"],
            37
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
            metrics["sentence_recovery_near_relation_stop_reason_candidate_posting_visit_limit"],
            0
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
                NearRelationStopReason::CandidatePostingVisitLimit,
                "sentence_recovery_near_relation_stop_reason_candidate_posting_visit_limit",
            ),
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
    fn flattens_each_run_signature_stop_reason_as_one_hot() {
        let cases = [
            (
                RunSignatureStopReason::PostingVisitLimit,
                "sentence_recovery_run_signature_stop_reason_posting_visit_limit",
            ),
            (
                RunSignatureStopReason::TokenVerificationLimit,
                "sentence_recovery_run_signature_stop_reason_token_verification_limit",
            ),
            (
                RunSignatureStopReason::CandidatePairLimit,
                "sentence_recovery_run_signature_stop_reason_candidate_pair_limit",
            ),
        ];

        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        run_signature_stop_reason: Some(reason),
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
    fn flattens_every_sentence_edge_filter_metric() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_filter_complete: true,
                    sentence_edge_filter_pairs_examined: 1,
                    sentence_edge_filter_pairs_attempted: 2,
                    sentence_edge_filter_similarity_comparisons_examined: 3,
                    sentence_edge_filter_similarity_comparisons_attempted: 4,
                    sentence_edge_filter_pairs_retained: 5,
                    sentence_edge_filter_pairs_rejected: 6,
                    sentence_edge_filter_full_build_fallback_used: true,
                    sentence_edge_filter_discarded_near_pair_visits_examined: 7,
                    sentence_edge_filter_discarded_near_pair_visits_attempted: 8,
                    sentence_edge_filter_discarded_near_similarity_comparisons_examined: 9,
                    sentence_edge_filter_discarded_near_similarity_comparisons_attempted: 10,
                    sentence_edge_filter_discarded_near_candidate_posting_visits_examined: 11,
                    sentence_edge_filter_discarded_near_candidate_posting_visits_attempted: 12,
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );

        let expected = [
            ("complete", 1),
            ("pairs_examined", 1),
            ("pairs_attempted", 2),
            ("similarity_comparisons_examined", 3),
            ("similarity_comparisons_attempted", 4),
            ("pairs_retained", 5),
            ("pairs_rejected", 6),
            ("full_build_fallback_used", 1),
            ("discarded_near_pair_visits_examined", 7),
            ("discarded_near_pair_visits_attempted", 8),
            ("discarded_near_similarity_comparisons_examined", 9),
            ("discarded_near_similarity_comparisons_attempted", 10),
            ("discarded_near_candidate_posting_visits_examined", 11),
            ("discarded_near_candidate_posting_visits_attempted", 12),
        ];
        for (field, value) in expected {
            let key = format!("sentence_recovery_sentence_edge_filter_{field}");
            assert_eq!(metrics[key.as_str()], value, "unexpected value for {key}");
        }
    }

    #[test]
    fn preserves_sentence_edge_filter_zeros_and_false() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics::default()),
                ..PipelineMetrics::default()
            },
            None,
        );
        let edge_filter_metrics = metrics
            .iter()
            .filter(|(name, _)| name.starts_with("sentence_recovery_sentence_edge_filter_"))
            .collect::<Vec<_>>();

        assert_eq!(edge_filter_metrics.len(), 18);
        assert!(edge_filter_metrics.iter().all(|(_, value)| **value == 0));
    }

    #[test]
    fn flattens_each_sentence_edge_filter_stop_reason_as_one_hot() {
        let cases = [
            (
                SentenceEdgeFilterStopReason::PairVisitLimit,
                "sentence_recovery_sentence_edge_filter_stop_reason_pair_visit_limit",
            ),
            (
                SentenceEdgeFilterStopReason::SimilarityComparisonLimit,
                "sentence_recovery_sentence_edge_filter_stop_reason_similarity_comparison_limit",
            ),
            (
                SentenceEdgeFilterStopReason::AllocationFailure,
                "sentence_recovery_sentence_edge_filter_stop_reason_allocation_failure",
            ),
            (
                SentenceEdgeFilterStopReason::CounterOverflow,
                "sentence_recovery_sentence_edge_filter_stop_reason_counter_overflow",
            ),
        ];

        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        sentence_edge_filter_stop_reason: Some(reason),
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
    fn flattens_every_sentence_edge_gate_shadow_metric() {
        let shadow = SentenceEdgeGateShadowMetrics {
            complete: true,
            stop_reason: None,
            pairs_considered: 1,
            pairs_retained: 2,
            pairs_rejected: 3,
            same_known_rejected: 4,
            ambiguous_rejected: 5,
            cross_span_rejected: 6,
            unclassified_rejected: 7,
            projected_pair_visits: 8,
            projected_similarity_comparisons: 9,
            rejected_max_production_score: 10,
            threshold_violations: 11,
            veto_mismatches: 12,
            unique_partner_mismatches: 13,
            reciprocal_pair_mismatches: 14,
            adopted_replacement_mismatches: 15,
            insertion_deletion_veto_mismatches: 16,
        };
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_gate_shadow: Some(shadow),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );

        let expected = [
            ("complete", 1),
            ("pairs_considered", 1),
            ("pairs_retained", 2),
            ("pairs_rejected", 3),
            ("same_known_rejected", 4),
            ("ambiguous_rejected", 5),
            ("cross_span_rejected", 6),
            ("unclassified_rejected", 7),
            ("projected_pair_visits", 8),
            ("projected_similarity_comparisons", 9),
            ("rejected_max_production_score", 10),
            ("threshold_violations", 11),
            ("veto_mismatches", 12),
            ("unique_partner_mismatches", 13),
            ("reciprocal_pair_mismatches", 14),
            ("adopted_replacement_mismatches", 15),
            ("insertion_deletion_veto_mismatches", 16),
        ];
        for (field, value) in expected {
            let key = format!("sentence_recovery_sentence_edge_gate_shadow_{field}");
            assert_eq!(metrics[key.as_str()], value, "unexpected value for {key}");
        }
    }

    #[test]
    fn preserves_sentence_edge_gate_shadow_zeros_and_false() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetrics::default()),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let shadow_metrics = metrics
            .iter()
            .filter(|(name, _)| name.starts_with("sentence_recovery_sentence_edge_gate_shadow_"))
            .collect::<Vec<_>>();

        assert_eq!(shadow_metrics.len(), 24);
        assert!(shadow_metrics.iter().all(|(_, value)| **value == 0));
    }

    #[test]
    fn flattens_each_sentence_edge_gate_shadow_stop_reason_as_one_hot() {
        let cases = [
            (
                SentenceEdgeGateShadowStopReason::CandidatePostingVisitLimit,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_candidate_posting_visit_limit",
            ),
            (
                SentenceEdgeGateShadowStopReason::PairVisitLimit,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_pair_visit_limit",
            ),
            (
                SentenceEdgeGateShadowStopReason::SimilarityComparisonLimit,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_similarity_comparison_limit",
            ),
            (
                SentenceEdgeGateShadowStopReason::CandidateCountLimit,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_candidate_count_limit",
            ),
            (
                SentenceEdgeGateShadowStopReason::AllocationFailure,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_allocation_failure",
            ),
            (
                SentenceEdgeGateShadowStopReason::CounterOverflow,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_counter_overflow",
            ),
            (
                SentenceEdgeGateShadowStopReason::DiagnosticFailure,
                "sentence_recovery_sentence_edge_gate_shadow_stop_reason_diagnostic_failure",
            ),
        ];

        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        sentence_edge_gate_shadow: Some(SentenceEdgeGateShadowMetrics {
                            stop_reason: Some(reason),
                            ..SentenceEdgeGateShadowMetrics::default()
                        }),
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
    fn omits_sentence_edge_gate_shadow_metrics_when_unavailable() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics::default()),
                ..PipelineMetrics::default()
            },
            None,
        );

        assert!(
            !metrics
                .keys()
                .any(|name| { name.starts_with("sentence_recovery_sentence_edge_gate_shadow_") })
        );
    }

    #[test]
    fn flattens_every_sentence_edge_signature_shadow_metric() {
        let shadow = SentenceEdgeSignatureShadowMetrics {
            complete: true,
            index_posting_items_examined: 2,
            index_posting_items_attempted: 3,
            query_posting_visits_examined: 4,
            query_posting_visits_attempted: 5,
            pairs_considered: 6,
            signature_candidates: 7,
            projected_pairs_pruned: 8,
            exact_edge_retained_pairs: 9,
            verification_evaluable: true,
            retained_pair_misses: 10,
            signature_not_in_edge_union: 11,
            largest_signature_candidate_set: 12,
            paired_interval_pairs: 13,
            paired_cross_interval_pairs: 14,
            same_known_pairs: 15,
            ambiguous_pairs: 16,
            cross_span_shared_pairs: 17,
            parity_evaluable: true,
            plan_parity: true,
            ..SentenceEdgeSignatureShadowMetrics::default()
        };
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_shadow: Some(shadow),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let expected = [
            ("complete", 1),
            ("index_posting_items_examined", 2),
            ("index_posting_items_attempted", 3),
            ("query_posting_visits_examined", 4),
            ("query_posting_visits_attempted", 5),
            ("pairs_considered", 6),
            ("signature_candidates", 7),
            ("projected_pairs_pruned", 8),
            ("exact_edge_retained_pairs", 9),
            ("verification_evaluable", 1),
            ("retained_pair_misses", 10),
            ("signature_not_in_edge_union", 11),
            ("largest_signature_candidate_set", 12),
            ("paired_interval_pairs", 13),
            ("paired_cross_interval_pairs", 14),
            ("same_known_pairs", 15),
            ("ambiguous_pairs", 16),
            ("cross_span_shared_pairs", 17),
            ("parity_evaluable", 1),
            ("plan_parity", 1),
        ];
        for (field, value) in expected {
            let key = format!("sentence_recovery_sentence_edge_signature_shadow_{field}");
            assert_eq!(metrics[key.as_str()], value, "unexpected value for {key}");
        }
        assert_eq!(
            metrics
                .keys()
                .filter(|name| {
                    name.starts_with("sentence_recovery_sentence_edge_signature_shadow_")
                })
                .count(),
            30
        );
    }

    #[test]
    fn preserves_sentence_edge_signature_shadow_zeros_and_false() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_shadow: Some(
                        SentenceEdgeSignatureShadowMetrics::default(),
                    ),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let shadow_metrics = metrics
            .iter()
            .filter(|(name, _)| {
                name.starts_with("sentence_recovery_sentence_edge_signature_shadow_")
            })
            .collect::<Vec<_>>();

        assert_eq!(shadow_metrics.len(), 30);
        assert!(shadow_metrics.iter().all(|(_, value)| **value == 0));
    }

    #[test]
    fn flattens_each_sentence_edge_signature_shadow_stop_reason_as_one_hot() {
        let cases = [
            (
                SentenceEdgeSignatureShadowStopReason::IndexPostingLimit,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_index_posting_limit",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::QueryPostingVisitLimit,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_query_posting_visit_limit",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::AllocationFailure,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_allocation_failure",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::CounterOverflow,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_counter_overflow",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::ProductionTraversalIncomplete,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_production_traversal_incomplete",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::CandidatePostingVisitLimit,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_candidate_posting_visit_limit",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::PairVisitLimit,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_pair_visit_limit",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::SimilarityComparisonLimit,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_similarity_comparison_limit",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::CandidateCountLimit,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_candidate_count_limit",
            ),
            (
                SentenceEdgeSignatureShadowStopReason::DiagnosticFailure,
                "sentence_recovery_sentence_edge_signature_shadow_stop_reason_diagnostic_failure",
            ),
        ];

        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        sentence_edge_signature_shadow: Some(SentenceEdgeSignatureShadowMetrics {
                            stop_reason: Some(reason),
                            ..SentenceEdgeSignatureShadowMetrics::default()
                        }),
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
    fn omits_sentence_edge_signature_shadow_metrics_when_unavailable() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics::default()),
                ..PipelineMetrics::default()
            },
            None,
        );

        assert!(
            !metrics.keys().any(|name| {
                name.starts_with("sentence_recovery_sentence_edge_signature_shadow_")
            })
        );
    }

    #[test]
    fn flattens_every_sentence_edge_signature_direct_shadow_metric() {
        let mut shadow = SentenceEdgeSignatureDirectShadowMetrics {
            complete: true,
            candidate_count_truncated: true,
            parity_evaluable: true,
            plan_parity: true,
            verification_evaluable: true,
            watch_preservation_evaluable: true,
            watch_evidence_preserved: true,
            watch_exact_parity_evaluable: true,
            watch_exact_parity: true,
            ..SentenceEdgeSignatureDirectShadowMetrics::default()
        };
        let mut value = 0usize;
        macro_rules! assign_direct_metrics {
            ($($field:ident),+ $(,)?) => {
                $(value += 1; shadow.$field = value;)+
            };
        }
        assign_direct_metrics!(
            signature_index_items_examined,
            signature_index_items_attempted,
            signature_query_visits_examined,
            signature_query_visits_attempted,
            signature_index_own_distinct_keys,
            signature_index_all_distinct_keys,
            signature_index_distinct_keys_examined,
            signature_index_distinct_keys_attempted,
            signature_index_own_key_capacity,
            signature_index_all_key_capacity,
            signature_index_own_posting_items,
            signature_index_all_posting_items,
            signature_index_posting_capacity_items,
            signature_index_largest_posting,
            signature_index_estimated_logical_bytes,
            signature_index_estimated_logical_bytes_examined,
            signature_index_estimated_logical_bytes_attempted,
            signature_index_depth_1_posting_items,
            signature_index_depth_2_to_3_posting_items,
            signature_index_depth_4_plus_posting_items,
            signature_queries,
            signature_queries_attempted,
            signature_depth_1_queries,
            signature_depth_2_to_3_queries,
            signature_depth_4_plus_queries,
            signature_depth_1_candidate_union,
            signature_depth_2_to_3_candidate_union,
            signature_depth_4_plus_candidate_union,
            direct_candidates,
            signature_candidate_union_attempted,
            paired_interval_candidates,
            paired_cross_interval_candidates,
            same_known_candidates,
            ambiguous_candidates,
            cross_span_candidates,
            edge_filter_pairs_examined,
            edge_filter_pairs_attempted,
            edge_filter_comparisons_examined,
            edge_filter_comparisons_attempted,
            exact_edge_retained_pairs,
            exact_edge_rechecks,
            exact_edge_rechecks_attempted,
            exact_edge_recheck_comparisons_examined,
            exact_edge_recheck_comparisons_attempted,
            exact_edge_rejected_pairs,
            cross_orientation_only_candidates,
            sentence_broad_edge_postings_examined,
            sentence_broad_edge_postings_attempted,
            downstream_candidate_postings_examined,
            downstream_candidate_postings_attempted,
            downstream_pair_visits_examined,
            downstream_pair_visits_attempted,
            downstream_similarity_comparisons_examined,
            downstream_similarity_comparisons_attempted,
            fragment_veto_pair_visits_examined,
            fragment_veto_pair_visits_attempted,
            fragment_veto_similarity_comparisons_examined,
            fragment_veto_similarity_comparisons_attempted,
            watch_probe_pairs_examined,
            watch_probe_pairs_attempted,
            watch_probe_similarity_comparisons_examined,
            watch_probe_similarity_comparisons_attempted,
            watch_probe_missing_signature_candidates,
            watch_probe_invariant_violations,
            watch_preservation_mismatches,
            retained_pair_misses,
            retained_pair_count_mismatches,
            retained_pair_set_mismatches,
            retained_pair_order_mismatches,
        );
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_direct_shadow: Some(shadow),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let mut expected = 0usize;
        macro_rules! assert_direct_metrics {
            ($($field:ident),+ $(,)?) => {
                $(expected += 1; assert_eq!(
                    metrics[concat!(
                        "sentence_recovery_sentence_edge_signature_direct_shadow_",
                        stringify!($field)
                    )],
                    expected,
                );)+
            };
        }
        assert_direct_metrics!(
            signature_index_items_examined,
            signature_index_items_attempted,
            signature_query_visits_examined,
            signature_query_visits_attempted,
            signature_index_own_distinct_keys,
            signature_index_all_distinct_keys,
            signature_index_distinct_keys_examined,
            signature_index_distinct_keys_attempted,
            signature_index_own_key_capacity,
            signature_index_all_key_capacity,
            signature_index_own_posting_items,
            signature_index_all_posting_items,
            signature_index_posting_capacity_items,
            signature_index_largest_posting,
            signature_index_estimated_logical_bytes,
            signature_index_estimated_logical_bytes_examined,
            signature_index_estimated_logical_bytes_attempted,
            signature_index_depth_1_posting_items,
            signature_index_depth_2_to_3_posting_items,
            signature_index_depth_4_plus_posting_items,
            signature_queries,
            signature_queries_attempted,
            signature_depth_1_queries,
            signature_depth_2_to_3_queries,
            signature_depth_4_plus_queries,
            signature_depth_1_candidate_union,
            signature_depth_2_to_3_candidate_union,
            signature_depth_4_plus_candidate_union,
            direct_candidates,
            signature_candidate_union_attempted,
            paired_interval_candidates,
            paired_cross_interval_candidates,
            same_known_candidates,
            ambiguous_candidates,
            cross_span_candidates,
            edge_filter_pairs_examined,
            edge_filter_pairs_attempted,
            edge_filter_comparisons_examined,
            edge_filter_comparisons_attempted,
            exact_edge_retained_pairs,
            exact_edge_rechecks,
            exact_edge_rechecks_attempted,
            exact_edge_recheck_comparisons_examined,
            exact_edge_recheck_comparisons_attempted,
            exact_edge_rejected_pairs,
            cross_orientation_only_candidates,
            sentence_broad_edge_postings_examined,
            sentence_broad_edge_postings_attempted,
            downstream_candidate_postings_examined,
            downstream_candidate_postings_attempted,
            downstream_pair_visits_examined,
            downstream_pair_visits_attempted,
            downstream_similarity_comparisons_examined,
            downstream_similarity_comparisons_attempted,
            fragment_veto_pair_visits_examined,
            fragment_veto_pair_visits_attempted,
            fragment_veto_similarity_comparisons_examined,
            fragment_veto_similarity_comparisons_attempted,
            watch_probe_pairs_examined,
            watch_probe_pairs_attempted,
            watch_probe_similarity_comparisons_examined,
            watch_probe_similarity_comparisons_attempted,
            watch_probe_missing_signature_candidates,
            watch_probe_invariant_violations,
            watch_preservation_mismatches,
            retained_pair_misses,
            retained_pair_count_mismatches,
            retained_pair_set_mismatches,
            retained_pair_order_mismatches,
        );
        for field in [
            "complete",
            "candidate_count_truncated",
            "parity_evaluable",
            "plan_parity",
            "verification_evaluable",
            "watch_preservation_evaluable",
            "watch_evidence_preserved",
            "watch_exact_parity_evaluable",
            "watch_exact_parity",
        ] {
            let key = format!("sentence_recovery_sentence_edge_signature_direct_shadow_{field}");
            assert_eq!(metrics[key.as_str()], 1);
        }
        assert_eq!(
            metrics
                .keys()
                .filter(|name| name
                    .starts_with("sentence_recovery_sentence_edge_signature_direct_shadow_"))
                .count(),
            value + 9 + 24
        );
    }

    #[test]
    fn flattens_each_sentence_edge_signature_direct_shadow_stop_reason_as_one_hot() {
        use SentenceEdgeSignatureDirectShadowStopReason as Stop;
        let cases = [
            (
                Stop::SignatureIndexPostingLimit,
                "signature_index_posting_limit",
            ),
            (
                Stop::SignatureIndexDistinctKeyLimit,
                "signature_index_distinct_key_limit",
            ),
            (
                Stop::SignatureIndexEstimatedByteLimit,
                "signature_index_estimated_byte_limit",
            ),
            (
                Stop::SignatureQueryCountLimit,
                "signature_query_count_limit",
            ),
            (
                Stop::SignatureQueryPostingVisitLimit,
                "signature_query_posting_visit_limit",
            ),
            (
                Stop::SignatureCandidateUnionLimit,
                "signature_candidate_union_limit",
            ),
            (
                Stop::SignatureExactEdgeRecheckLimit,
                "signature_exact_edge_recheck_limit",
            ),
            (
                Stop::DirectEdgePairVisitLimit,
                "direct_edge_pair_visit_limit",
            ),
            (
                Stop::DirectEdgeSimilarityComparisonLimit,
                "direct_edge_similarity_comparison_limit",
            ),
            (
                Stop::CandidatePostingVisitLimit,
                "candidate_posting_visit_limit",
            ),
            (Stop::PairVisitLimit, "pair_visit_limit"),
            (
                Stop::SimilarityComparisonLimit,
                "similarity_comparison_limit",
            ),
            (Stop::CandidateCountLimit, "candidate_count_limit"),
            (
                Stop::FragmentVetoPairVisitLimit,
                "fragment_veto_pair_visit_limit",
            ),
            (
                Stop::FragmentVetoSimilarityComparisonLimit,
                "fragment_veto_similarity_comparison_limit",
            ),
            (Stop::FragmentVetoIncomplete, "fragment_veto_incomplete"),
            (Stop::WatchProbePairLimit, "watch_probe_pair_limit"),
            (
                Stop::WatchProbeSimilarityComparisonLimit,
                "watch_probe_similarity_comparison_limit",
            ),
            (
                Stop::WatchProbeInvariantViolation,
                "watch_probe_invariant_violation",
            ),
            (Stop::WatchDiagnosticsMismatch, "watch_diagnostics_mismatch"),
            (Stop::AllocationFailure, "allocation_failure"),
            (Stop::CounterOverflow, "counter_overflow"),
            (
                Stop::ProductionTraversalIncomplete,
                "production_traversal_incomplete",
            ),
            (Stop::DiagnosticFailure, "diagnostic_failure"),
        ];
        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        sentence_edge_signature_direct_shadow: Some(
                            SentenceEdgeSignatureDirectShadowMetrics {
                                stop_reason: Some(reason),
                                ..SentenceEdgeSignatureDirectShadowMetrics::default()
                            },
                        ),
                        ..SentenceRecoveryMetrics::default()
                    }),
                    ..PipelineMetrics::default()
                },
                None,
            );
            for (_, name) in cases {
                let key = format!(
                    "sentence_recovery_sentence_edge_signature_direct_shadow_stop_reason_{name}"
                );
                assert_eq!(metrics[key.as_str()], usize::from(name == expected));
            }
        }
    }

    #[test]
    fn flattens_complete_sentence_edge_signature_reference_oracle_metrics() {
        let mut oracle = SentenceEdgeSignatureReferenceOracleMetrics {
            complete: true,
            direct_complete: true,
            candidate_count_truncated: true,
            plan_parity_evaluable: true,
            plan_parity: true,
            fingerprint_evaluable: true,
            ..SentenceEdgeSignatureReferenceOracleMetrics::default()
        };
        let mut value = 0usize;
        macro_rules! assign_reference_metrics {
            ($($field:ident),+ $(,)?) => {
                $(value += 1; oracle.$field = value;)+
            };
        }
        assign_reference_metrics!(
            candidate_posting_visits_examined,
            candidate_posting_visits_attempted,
            pair_visits_examined,
            pair_visits_attempted,
            similarity_comparisons_examined,
            similarity_comparisons_attempted,
            legacy_sentence_edge_pairs_examined,
            legacy_sentence_edge_pairs_attempted,
            legacy_sentence_edge_pairs_retained,
            legacy_sentence_edge_pairs_rejected,
            fragment_veto_pair_visits_examined,
            fragment_veto_pair_visits_attempted,
            fragment_veto_similarity_comparisons_examined,
            fragment_veto_similarity_comparisons_attempted,
            retained_pair_misses,
            retained_pair_count_mismatches,
            retained_pair_set_mismatches,
            retained_pair_order_mismatches,
        );
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_reference_oracle: Some(oracle),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let mut expected = 0usize;
        macro_rules! assert_reference_metrics {
            ($($field:ident),+ $(,)?) => {
                $(expected += 1; assert_eq!(
                    metrics[concat!(
                        "sentence_recovery_sentence_edge_signature_reference_oracle_",
                        stringify!($field)
                    )],
                    expected,
                );)+
            };
        }
        assert_reference_metrics!(
            candidate_posting_visits_examined,
            candidate_posting_visits_attempted,
            pair_visits_examined,
            pair_visits_attempted,
            similarity_comparisons_examined,
            similarity_comparisons_attempted,
            legacy_sentence_edge_pairs_examined,
            legacy_sentence_edge_pairs_attempted,
            legacy_sentence_edge_pairs_retained,
            legacy_sentence_edge_pairs_rejected,
            fragment_veto_pair_visits_examined,
            fragment_veto_pair_visits_attempted,
            fragment_veto_similarity_comparisons_examined,
            fragment_veto_similarity_comparisons_attempted,
            retained_pair_misses,
            retained_pair_count_mismatches,
            retained_pair_set_mismatches,
            retained_pair_order_mismatches,
        );
        for field in [
            "complete",
            "direct_complete",
            "candidate_count_truncated",
            "plan_parity_evaluable",
            "plan_parity",
            "fingerprint_evaluable",
        ] {
            let key = format!("sentence_recovery_sentence_edge_signature_reference_oracle_{field}");
            assert_eq!(metrics[key.as_str()], 1);
        }
        assert_eq!(
            metrics
                .keys()
                .filter(|name| name
                    .starts_with("sentence_recovery_sentence_edge_signature_reference_oracle_"))
                .count(),
            value + 6 + 12
        );
    }

    #[test]
    fn flattens_each_sentence_edge_signature_reference_oracle_stop_reason_as_one_hot() {
        use SentenceEdgeSignatureReferenceOracleStopReason as Stop;
        let cases = [
            (Stop::DirectReplayIncomplete, "direct_replay_incomplete"),
            (
                Stop::CandidatePostingVisitLimit,
                "candidate_posting_visit_limit",
            ),
            (Stop::PairVisitLimit, "pair_visit_limit"),
            (
                Stop::SimilarityComparisonLimit,
                "similarity_comparison_limit",
            ),
            (Stop::CandidateCountLimit, "candidate_count_limit"),
            (
                Stop::FragmentVetoPairVisitLimit,
                "fragment_veto_pair_visit_limit",
            ),
            (
                Stop::FragmentVetoSimilarityComparisonLimit,
                "fragment_veto_similarity_comparison_limit",
            ),
            (Stop::FragmentVetoIncomplete, "fragment_veto_incomplete"),
            (Stop::AllocationFailure, "allocation_failure"),
            (Stop::CounterOverflow, "counter_overflow"),
            (
                Stop::ProductionTraversalIncomplete,
                "production_traversal_incomplete",
            ),
            (Stop::DiagnosticFailure, "diagnostic_failure"),
        ];
        for (reason, expected) in cases {
            let metrics = pipeline_metrics(
                PipelineMetrics {
                    sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                        sentence_edge_signature_reference_oracle: Some(
                            SentenceEdgeSignatureReferenceOracleMetrics {
                                stop_reason: Some(reason),
                                legacy_sentence_edge_pairs_examined: 41,
                                legacy_sentence_edge_pairs_attempted: 43,
                                legacy_sentence_edge_pairs_retained: 17,
                                legacy_sentence_edge_pairs_rejected: 24,
                                ..SentenceEdgeSignatureReferenceOracleMetrics::default()
                            },
                        ),
                        ..SentenceRecoveryMetrics::default()
                    }),
                    ..PipelineMetrics::default()
                },
                None,
            );
            for (_, name) in cases {
                let key = format!(
                    "sentence_recovery_sentence_edge_signature_reference_oracle_stop_reason_{name}"
                );
                assert_eq!(metrics[key.as_str()], usize::from(name == expected));
            }
            for (field, value) in [
                ("legacy_sentence_edge_pairs_examined", 41),
                ("legacy_sentence_edge_pairs_attempted", 43),
                ("legacy_sentence_edge_pairs_retained", 17),
                ("legacy_sentence_edge_pairs_rejected", 24),
            ] {
                let key =
                    format!("sentence_recovery_sentence_edge_signature_reference_oracle_{field}");
                assert_eq!(metrics[key.as_str()], value);
            }
        }
    }

    #[test]
    fn preserves_signature_fragment_veto_counter_deficits_and_omits_absent_oracle() {
        let present = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_direct_shadow: Some(
                        SentenceEdgeSignatureDirectShadowMetrics {
                            fragment_veto_pair_visits_examined: 3,
                            fragment_veto_pair_visits_attempted: 4,
                            fragment_veto_similarity_comparisons_examined: 5,
                            fragment_veto_similarity_comparisons_attempted: 6,
                            ..SentenceEdgeSignatureDirectShadowMetrics::default()
                        },
                    ),
                    sentence_edge_signature_reference_oracle: Some(
                        SentenceEdgeSignatureReferenceOracleMetrics {
                            fragment_veto_pair_visits_examined: 7,
                            fragment_veto_pair_visits_attempted: 8,
                            fragment_veto_similarity_comparisons_examined: 9,
                            fragment_veto_similarity_comparisons_attempted: 10,
                            ..SentenceEdgeSignatureReferenceOracleMetrics::default()
                        },
                    ),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        for (prefix, expected) in [
            (
                "sentence_recovery_sentence_edge_signature_direct_shadow_",
                [3, 4, 5, 6],
            ),
            (
                "sentence_recovery_sentence_edge_signature_reference_oracle_",
                [7, 8, 9, 10],
            ),
        ] {
            for (field, value) in [
                "fragment_veto_pair_visits_examined",
                "fragment_veto_pair_visits_attempted",
                "fragment_veto_similarity_comparisons_examined",
                "fragment_veto_similarity_comparisons_attempted",
            ]
            .into_iter()
            .zip(expected)
            {
                assert_eq!(present[format!("{prefix}{field}").as_str()], value);
            }
        }

        let absent = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics::default()),
                ..PipelineMetrics::default()
            },
            None,
        );
        assert!(!absent.keys().any(|name| {
            name.starts_with("sentence_recovery_sentence_edge_signature_reference_oracle_")
        }));
    }

    #[test]
    fn preserves_direct_watch_probe_counter_deficits() {
        let metrics = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_direct_shadow: Some(
                        SentenceEdgeSignatureDirectShadowMetrics {
                            watch_probe_pairs_examined: 3,
                            watch_probe_pairs_attempted: 5,
                            watch_probe_similarity_comparisons_examined: 7,
                            watch_probe_similarity_comparisons_attempted: 11,
                            watch_probe_missing_signature_candidates: 13,
                            watch_probe_invariant_violations: 17,
                            watch_preservation_mismatches: 19,
                            ..SentenceEdgeSignatureDirectShadowMetrics::default()
                        },
                    ),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let prefix = "sentence_recovery_sentence_edge_signature_direct_shadow_";
        for (field, expected) in [
            ("watch_probe_pairs_examined", 3),
            ("watch_probe_pairs_attempted", 5),
            ("watch_probe_similarity_comparisons_examined", 7),
            ("watch_probe_similarity_comparisons_attempted", 11),
            ("watch_probe_missing_signature_candidates", 13),
            ("watch_probe_invariant_violations", 17),
            ("watch_preservation_mismatches", 19),
        ] {
            assert_eq!(metrics[format!("{prefix}{field}").as_str()], expected);
        }
    }

    #[test]
    fn preserves_direct_shadow_zeros_and_omits_unavailable_metrics() {
        let present = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics {
                    sentence_edge_signature_direct_shadow: Some(
                        SentenceEdgeSignatureDirectShadowMetrics::default(),
                    ),
                    ..SentenceRecoveryMetrics::default()
                }),
                ..PipelineMetrics::default()
            },
            None,
        );
        let prefix = "sentence_recovery_sentence_edge_signature_direct_shadow_";
        let direct = present
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .collect::<Vec<_>>();
        assert!(!direct.is_empty());
        assert!(direct.iter().all(|(_, value)| **value == 0));

        let absent = pipeline_metrics(
            PipelineMetrics {
                sentence_recovery_metrics: Some(SentenceRecoveryMetrics::default()),
                ..PipelineMetrics::default()
            },
            None,
        );
        assert!(!absent.keys().any(|name| name.starts_with(prefix)));
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
