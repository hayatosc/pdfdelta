use crate::{
    Error, Result,
    alignment::{
        Alignment, AlignmentOptions, InvertedIndexCandidateGenerator,
        align_ordered_with_metrics_and_gap_plan, build_block_features,
        estimate_ngram_token_elements, plan_ordered_gaps, validate_alignment_options,
        validate_ngram_size,
    },
    diff::{
        Comparison, DiffOptions, MAX_MYERS_EDIT_DISTANCE, RecoveryWatchDiagnostics,
        RecoveryWatchQuery, SentenceRecoveryInput, SentenceRecoveryMetrics,
        TrustedRunRecoveryInput, compare_aligned,
        compare_aligned_with_known_span_sentence_shadow_diagnostics,
        compare_aligned_with_recovery_watch_diagnostics,
        compare_aligned_with_sentence_recovery_metrics, enforce_diff_raw_token_budget,
        enforce_diff_token_budget, validate_diff_options,
    },
    layout::{
        BlockOptions, LayoutIssue, LineOptions, TrustedRegionEdge, TrustedRunDescriptor,
        TrustedRunInterval, reconstruct_blocks_with_issues, reconstruct_lines,
        validate_block_options, validate_line_options,
    },
    model::{Document, Glyph, GlyphCropStatus, GlyphEvidence, GlyphPathClipStatus, TextRenderMode},
    normalize::{BlockText, normalize_blocks},
    report::{DocumentSide, ExtractionIssueRecord, ExtractionStatus},
    source::{ExtractionIssue, ExtractionOutcome, ExtractionScope},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PipelineOptions {
    pub line: LineOptions,
    pub block: BlockOptions,
    pub ngram_size: usize,
    /// Maximum retained n-gram token elements across both document sides.
    pub max_ngram_token_elements: usize,
    pub alignment: AlignmentOptions,
    pub diff: DiffOptions,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            line: LineOptions::default(),
            block: BlockOptions::default(),
            ngram_size: 3,
            // Three elements per token bounds the default 3-gram representation.
            max_ngram_token_elements: 15_300_000,
            alignment: AlignmentOptions::default(),
            diff: DiffOptions::default(),
        }
    }
}

impl PipelineOptions {
    /// Returns these options with comparison resource limits scaled uniformly.
    ///
    /// Layout parameters and matching behavior are unchanged. Only n-gram
    /// token elements, alignment candidate visits, alignment DP cells, diff
    /// tokens, and diff edit distance are scaled. The edit-distance budget
    /// saturates at the bounded Myers implementation's 64 MiB trace cap.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] when `scale` is non-finite or
    /// less than one.
    pub fn scaled_limits(mut self, scale: f64) -> Result<Self> {
        let scale = validate_limit_scale(scale)?;
        let scale_limit = |value: usize| ((value as f64 * scale) as usize).max(value);
        self.max_ngram_token_elements = scale_limit(self.max_ngram_token_elements);
        self.alignment.max_candidate_visits = scale_limit(self.alignment.max_candidate_visits);
        self.alignment.max_dp_cells = scale_limit(self.alignment.max_dp_cells);
        self.diff.max_tokens = scale_limit(self.diff.max_tokens);
        self.diff.max_edit_distance =
            scale_limit(self.diff.max_edit_distance).min(MAX_MYERS_EDIT_DISTANCE);
        Ok(self)
    }

    fn validate(self) -> Result<Self> {
        validate_line_options(self.line)?;
        validate_block_options(self.block)?;
        validate_ngram_size(self.ngram_size)?;
        validate_ngram_token_element_limit(self.max_ngram_token_elements)?;
        validate_alignment_options(self.alignment)?;
        validate_diff_options(self.diff)?;
        Ok(self)
    }
}

/// Validates a multiplier for comparison resource limits.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] when `scale` is non-finite or
/// less than one, which would weaken configured limits.
pub fn validate_limit_scale(scale: f64) -> Result<f64> {
    if !scale.is_finite() || scale < 1.0 {
        return Err(Error::InvalidConfiguration(format!(
            "limit scale must be a finite value >= 1.0 so configured defaults are never weakened, found {scale}"
        )));
    }
    Ok(scale)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComparisonOutcome {
    pub comparison: Comparison,
    pub extraction: ExtractionStatus,
    /// Normalized old-side blocks backing the comparison spans, for
    /// report rendering; empty when a document-scoped extraction issue
    /// suppresses the diff.
    pub old_blocks: Vec<BlockText>,
    /// Normalized new-side blocks backing the comparison spans.
    pub new_blocks: Vec<BlockText>,
    /// Retained old-side glyph evidence used for report provenance projection.
    pub old_glyph_evidence: Vec<GlyphEvidence>,
    /// Retained new-side glyph evidence used for report provenance projection.
    pub new_glyph_evidence: Vec<GlyphEvidence>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComparisonOutcomeWithRecoveryWatch {
    pub outcome: ComparisonOutcome,
    pub alignment: Option<Alignment>,
    pub diagnostics: Option<RecoveryWatchDiagnostics>,
}

struct ValidatedComparisonOutcome {
    comparison: Comparison,
    old_blocks: Vec<BlockText>,
    new_blocks: Vec<BlockText>,
    alignment: Alignment,
    recovery_watch_diagnostics: Option<RecoveryWatchDiagnostics>,
}

#[derive(Clone, Copy)]
struct ComparisonInstrumentation<'a> {
    old_issue_boundaries: &'a [LocalizedIssueBoundary],
    new_issue_boundaries: &'a [LocalizedIssueBoundary],
    enable_sentence_recovery: bool,
    watch_queries: &'a [RecoveryWatchQuery<'a>],
    enable_known_span_sentence_shadow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelinePhase {
    ConfigurationValidation,
    CompletenessGate,
    PreLayoutBudget,
    LineReconstruction,
    BlockReconstruction,
    Normalization,
    DiffTokenBudget,
    NgramBudget,
    FeatureBuild,
    CandidateIndex,
    Alignment,
    ExactDiff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelinePhaseStatus {
    Completed,
    Incomplete,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineMetrics {
    /// Painting glyphs retained for comparison after page CropBox filtering.
    pub painting_glyphs: Option<usize>,
    pub lines: Option<usize>,
    pub blocks: Option<usize>,
    pub normalized_blocks: Option<usize>,
    pub raw_tokens: Option<usize>,
    pub ngram_token_elements: Option<usize>,
    pub features: Option<usize>,
    pub indexed_features: Option<usize>,
    pub alignment_spans: Option<usize>,
    /// Sum of `CandidateGenerator::estimated_visits` charged against
    /// `max_candidate_visits` for non-anchor old blocks outside forced
    /// uncertainty windows; on a limit failure
    /// this is the attempted cumulative charge including the exceeding block.
    pub candidate_visits: Option<usize>,
    /// Checked sum of `CandidateGenerator::estimated_visits` over every
    /// eligible old block, independent of the budget: the full candidate
    /// work the alignment would need. `Some` when the full sum completed
    /// (including on a limit failure); `None` when an estimate error or
    /// overflow made the sum unavailable, or the candidate preflight was
    /// never reached (e.g. an earlier alignment error). Identity alignment
    /// is `Some(0)`.
    pub candidate_visits_required: Option<usize>,
    /// Exact-match posting visits of the required candidate sum; `Some`
    /// only when every eligible old block reported a breakdown and every
    /// component sum completed. Identity alignment is `Some(0)`.
    pub candidate_visits_required_exact: Option<usize>,
    /// N-gram posting visits of the required candidate sum; `Some` under
    /// the same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_ngram: Option<usize>,
    /// Short-block fallback visits of the required candidate sum; `Some`
    /// under the same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_short_fallback: Option<usize>,
    /// The `AlignmentOptions::max_candidate_visits` budget the charge was
    /// compared against.
    pub max_candidate_visits: Option<usize>,
    pub changes: Option<usize>,
    pub formatting_changes: Option<usize>,
    pub unresolved_regions: Option<usize>,
    /// Sentence recovery diagnostics for the completed exact-diff phase.
    pub sentence_recovery_metrics: Option<SentenceRecoveryMetrics>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineErrorKind {
    Backend,
    Report,
    InvalidConfiguration,
    Unsupported,
    Unresolved,
    LimitExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineErrorSnapshot {
    pub kind: PipelineErrorKind,
    pub message: String,
    pub resource: Option<&'static str>,
    pub limit: Option<usize>,
}

impl From<&Error> for PipelineErrorSnapshot {
    fn from(error: &Error) -> Self {
        let (kind, resource, limit) = match error {
            Error::Backend(_) => (PipelineErrorKind::Backend, None, None),
            Error::Report(_) => (PipelineErrorKind::Report, None, None),
            Error::InvalidConfiguration(_) => (PipelineErrorKind::InvalidConfiguration, None, None),
            Error::Unsupported(_) => (PipelineErrorKind::Unsupported, None, None),
            Error::Unresolved(_) => (PipelineErrorKind::Unresolved, None, None),
            Error::LimitExceeded { resource, limit } => (
                PipelineErrorKind::LimitExceeded,
                Some(*resource),
                Some(*limit),
            ),
        };
        Self {
            kind,
            message: error.to_string(),
            resource,
            limit,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineDiagnosticRecord {
    pub phase: PipelinePhase,
    pub side: Option<DocumentSide>,
    pub status: PipelinePhaseStatus,
    pub metrics: PipelineMetrics,
    pub error: Option<PipelineErrorSnapshot>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipelineDiagnostics {
    records: Vec<PipelineDiagnosticRecord>,
}

impl PipelineDiagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> &[PipelineDiagnosticRecord] {
        &self.records
    }

    fn begin(&mut self) {
        self.records.clear();
    }

    fn completed(
        &mut self,
        phase: PipelinePhase,
        side: Option<DocumentSide>,
        metrics: PipelineMetrics,
    ) {
        self.records.push(PipelineDiagnosticRecord {
            phase,
            side,
            status: PipelinePhaseStatus::Completed,
            metrics,
            error: None,
        });
    }

    fn incomplete(&mut self, phase: PipelinePhase) {
        self.records.push(PipelineDiagnosticRecord {
            phase,
            side: None,
            status: PipelinePhaseStatus::Incomplete,
            metrics: PipelineMetrics::default(),
            error: None,
        });
    }

    fn failed(&mut self, phase: PipelinePhase, side: Option<DocumentSide>, error: &Error) {
        self.failed_with_metrics(phase, side, error, PipelineMetrics::default());
    }

    fn failed_with_metrics(
        &mut self,
        phase: PipelinePhase,
        side: Option<DocumentSide>,
        error: &Error,
        metrics: PipelineMetrics,
    ) {
        self.records.push(PipelineDiagnosticRecord {
            phase,
            side,
            status: PipelinePhaseStatus::Failed,
            metrics,
            error: Some(error.into()),
        });
    }
}

pub fn compare_extraction_outcomes(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
) -> Result<ComparisonOutcome> {
    compare_extraction_outcomes_with_diagnostics(old, new, options, &mut PipelineDiagnostics::new())
}

pub fn compare_extraction_outcomes_with_diagnostics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<ComparisonOutcome> {
    compare_extraction_outcomes_with_alignment_diagnostics(old, new, options, diagnostics)
        .map(|(outcome, _)| outcome)
}

/// Compares extracted documents while retaining the block alignment used by
/// the exact-diff phase.
///
/// The alignment is `None` when a document-scoped extraction issue suppresses
/// the diff. Callers that only need the public comparison should use
/// [`compare_extraction_outcomes_with_diagnostics`].
///
/// # Errors
///
/// Returns an error when configuration validation, layout reconstruction,
/// alignment, or exact diffing fails.
pub fn compare_extraction_outcomes_with_alignment_diagnostics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<(ComparisonOutcome, Option<Alignment>)> {
    compare_extraction_outcomes_with_recovery_watch_inner(
        old,
        new,
        options,
        diagnostics,
        &[],
        false,
    )
    .map(|outcome| (outcome.outcome, outcome.alignment))
}

/// Compares extracted documents and observes selected uncertain-region recovery evidence.
///
/// Empty queries are equivalent to
/// [`compare_extraction_outcomes_with_alignment_diagnostics`] and do not add
/// recovery scanning or similarity work.
pub fn compare_extraction_outcomes_with_recovery_watch_diagnostics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
    watch_queries: &[RecoveryWatchQuery<'_>],
) -> Result<ComparisonOutcomeWithRecoveryWatch> {
    compare_extraction_outcomes_with_recovery_watch_inner(
        old,
        new,
        options,
        diagnostics,
        watch_queries,
        false,
    )
}

/// Compares extracted documents and records a known-span sentence shadow diagnostic.
///
/// This entry point adds a bounded post-production relation replay. Ordinary
/// comparison and recovery-watch entry points do not pay that runtime cost.
///
/// # Errors
///
/// Returns an error when configuration validation, layout reconstruction,
/// alignment, exact diffing, or a resource limit fails.
pub fn compare_extraction_outcomes_with_known_span_sentence_shadow_diagnostics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
    watch_queries: &[RecoveryWatchQuery<'_>],
) -> Result<ComparisonOutcomeWithRecoveryWatch> {
    compare_extraction_outcomes_with_recovery_watch_inner(
        old,
        new,
        options,
        diagnostics,
        watch_queries,
        true,
    )
}

fn compare_extraction_outcomes_with_recovery_watch_inner(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
    watch_queries: &[RecoveryWatchQuery<'_>],
    enable_known_span_sentence_shadow: bool,
) -> Result<ComparisonOutcomeWithRecoveryWatch> {
    diagnostics.begin();
    let options = match options.validate() {
        Ok(options) => options,
        Err(error) => {
            diagnostics.failed(PipelinePhase::ConfigurationValidation, None, &error);
            return Err(error);
        }
    };
    diagnostics.completed(
        PipelinePhase::ConfigurationValidation,
        None,
        PipelineMetrics::default(),
    );
    let old_complete = old.is_complete();
    let new_complete = new.is_complete();
    let (old_document, old_issues) = old.into_parts();
    let (new_document, new_issues) = new.into_parts();
    let old_glyph_evidence = glyph_evidence(&old_document);
    let new_glyph_evidence = glyph_evidence(&new_document);
    if old_complete && new_complete {
        diagnostics.completed(
            PipelinePhase::CompletenessGate,
            None,
            PipelineMetrics::default(),
        );
        let compared = compare_validated_glyph_documents_inner(
            &old_document,
            &new_document,
            options,
            diagnostics,
            ComparisonInstrumentation {
                old_issue_boundaries: &[],
                new_issue_boundaries: &[],
                enable_sentence_recovery: true,
                watch_queries,
                enable_known_span_sentence_shadow,
            },
        )?;
        return Ok(ComparisonOutcomeWithRecoveryWatch {
            outcome: ComparisonOutcome {
                comparison: compared.comparison,
                extraction: ExtractionStatus::complete(),
                old_blocks: compared.old_blocks,
                new_blocks: compared.new_blocks,
                old_glyph_evidence,
                new_glyph_evidence,
            },
            alignment: Some(compared.alignment),
            diagnostics: compared.recovery_watch_diagnostics,
        });
    }

    diagnostics.incomplete(PipelinePhase::CompletenessGate);

    let has_document_issue = old_issues
        .iter()
        .chain(&new_issues)
        .any(|issue| issue.scope() == ExtractionScope::Document);

    if !has_document_issue {
        let old_gap_boundaries = issue_boundaries(&old_issues);
        let new_gap_boundaries = issue_boundaries(&new_issues);
        let mut compared = compare_validated_glyph_documents_inner(
            &old_document,
            &new_document,
            options,
            diagnostics,
            ComparisonInstrumentation {
                old_issue_boundaries: &old_gap_boundaries,
                new_issue_boundaries: &new_gap_boundaries,
                enable_sentence_recovery: false,
                watch_queries,
                enable_known_span_sentence_shadow,
            },
        )?;
        if !old_complete {
            compared.comparison.old_coverage.ratio = None;
        }
        if !new_complete {
            compared.comparison.new_coverage.ratio = None;
        }
        let issues = extraction_issue_records(old_issues, new_issues);
        return Ok(ComparisonOutcomeWithRecoveryWatch {
            outcome: ComparisonOutcome {
                comparison: compared.comparison,
                extraction: ExtractionStatus {
                    old_complete,
                    new_complete,
                    issues,
                },
                old_blocks: compared.old_blocks,
                new_blocks: compared.new_blocks,
                old_glyph_evidence,
                new_glyph_evidence,
            },
            alignment: Some(compared.alignment),
            diagnostics: compared.recovery_watch_diagnostics,
        });
    }

    let (old_tokens, new_tokens) =
        record_pre_layout_token_counts(&old_document, &new_document, options.diff, diagnostics)?;
    let issues = extraction_issue_records(old_issues, new_issues);

    // Incomplete extraction suppresses the diff to prevent false comparison output.
    Ok(ComparisonOutcomeWithRecoveryWatch {
        outcome: ComparisonOutcome {
            comparison: Comparison {
                changes: Vec::new(),
                formatting_changes: Vec::new(),
                unresolved_regions: Vec::new(),
                old_coverage: conservative_coverage(old_tokens, old_complete),
                new_coverage: conservative_coverage(new_tokens, new_complete),
            },
            extraction: ExtractionStatus {
                old_complete,
                new_complete,
                issues,
            },
            old_blocks: Vec::new(),
            new_blocks: Vec::new(),
            old_glyph_evidence,
            new_glyph_evidence,
        },
        alignment: None,
        diagnostics: None,
    })
}

fn glyph_evidence(document: &Document<Glyph>) -> Vec<GlyphEvidence> {
    document.items().iter().map(GlyphEvidence::from).collect()
}

pub fn compare_glyph_documents(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<Comparison> {
    let options = options.validate()?;
    compare_validated_glyph_documents(old, new, options, &mut PipelineDiagnostics::new())
        .map(|(comparison, _, _, _)| comparison)
}

fn compare_validated_glyph_documents(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<(Comparison, Vec<BlockText>, Vec<BlockText>, Alignment)> {
    compare_validated_glyph_documents_inner(
        old,
        new,
        options,
        diagnostics,
        ComparisonInstrumentation {
            old_issue_boundaries: &[],
            new_issue_boundaries: &[],
            enable_sentence_recovery: true,
            watch_queries: &[],
            enable_known_span_sentence_shadow: false,
        },
    )
    .map(|outcome| {
        (
            outcome.comparison,
            outcome.old_blocks,
            outcome.new_blocks,
            outcome.alignment,
        )
    })
}

fn compare_validated_glyph_documents_inner(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
    instrumentation: ComparisonInstrumentation<'_>,
) -> Result<ValidatedComparisonOutcome> {
    let old_document = old;
    let new_document = new;
    record_pre_layout_token_counts(old, new, options.diff, diagnostics)?;
    let old_prepared = prepare(old, options, DocumentSide::Old, diagnostics)?;
    let new_prepared = prepare(new, options, DocumentSide::New, diagnostics)?;
    let PreparedDocument {
        blocks: old,
        uncertain_block_indices: old_uncertain_block_indices,
        trusted_run_intervals: old_trusted_run_intervals,
        trusted_run_descriptors: old_trusted_run_descriptors,
        trusted_region_edges: old_trusted_region_edges,
    } = old_prepared;
    let PreparedDocument {
        blocks: new,
        uncertain_block_indices: new_uncertain_block_indices,
        trusted_run_intervals: new_trusted_run_intervals,
        trusted_run_descriptors: new_trusted_run_descriptors,
        trusted_region_edges: new_trusted_region_edges,
    } = new_prepared;
    let (old_gap_boundaries, old_extraction_uncertain_block_indices) =
        gap_boundaries(old_document, &old, instrumentation.old_issue_boundaries);
    let (new_gap_boundaries, new_extraction_uncertain_block_indices) =
        gap_boundaries(new_document, &new, instrumentation.new_issue_boundaries);
    phase_result(
        diagnostics,
        PipelinePhase::DiffTokenBudget,
        None,
        enforce_diff_token_budget(&old, &new, options.diff),
    )?;
    diagnostics.completed(
        PipelinePhase::DiffTokenBudget,
        None,
        PipelineMetrics {
            normalized_blocks: Some(old.len().saturating_add(new.len())),
            ..PipelineMetrics::default()
        },
    );
    record_ngram_token_element_budget(&old, &new, options, diagnostics)?;
    let old_features = phase_result(
        diagnostics,
        PipelinePhase::FeatureBuild,
        Some(DocumentSide::Old),
        build_block_features(&old, options.ngram_size),
    )?;
    diagnostics.completed(
        PipelinePhase::FeatureBuild,
        Some(DocumentSide::Old),
        PipelineMetrics {
            normalized_blocks: Some(old.len()),
            features: Some(old_features.len()),
            ..PipelineMetrics::default()
        },
    );
    let new_features = phase_result(
        diagnostics,
        PipelinePhase::FeatureBuild,
        Some(DocumentSide::New),
        build_block_features(&new, options.ngram_size),
    )?;
    diagnostics.completed(
        PipelinePhase::FeatureBuild,
        Some(DocumentSide::New),
        PipelineMetrics {
            normalized_blocks: Some(new.len()),
            features: Some(new_features.len()),
            ..PipelineMetrics::default()
        },
    );
    let gap_plan = phase_result(
        diagnostics,
        PipelinePhase::Alignment,
        None,
        plan_ordered_gaps(
            &old_features,
            &new_features,
            options.alignment,
            &old_gap_boundaries,
            &new_gap_boundaries,
            &old_extraction_uncertain_block_indices,
            &new_extraction_uncertain_block_indices,
            &old_uncertain_block_indices,
            &new_uncertain_block_indices,
        ),
    )?;
    let indexed_new_features = new_features
        .iter()
        .filter(|features| gap_plan.allows_new_block(features.block))
        .cloned()
        .collect::<Vec<_>>();
    let candidates = phase_result(
        diagnostics,
        PipelinePhase::CandidateIndex,
        Some(DocumentSide::New),
        InvertedIndexCandidateGenerator::new(&indexed_new_features),
    )?;
    diagnostics.completed(
        PipelinePhase::CandidateIndex,
        Some(DocumentSide::New),
        PipelineMetrics {
            indexed_features: Some(indexed_new_features.len()),
            ..PipelineMetrics::default()
        },
    );
    let attempt = align_ordered_with_metrics_and_gap_plan(
        &old_features,
        &new_features,
        &candidates,
        options.alignment,
        gap_plan,
    );
    let alignment = match attempt.result {
        Ok(alignment) => {
            diagnostics.completed(
                PipelinePhase::Alignment,
                None,
                PipelineMetrics {
                    alignment_spans: Some(alignment.spans.len()),
                    candidate_visits: Some(attempt.visit_metrics.candidate_visits),
                    candidate_visits_required: attempt.visit_metrics.candidate_visits_required,
                    candidate_visits_required_exact: attempt
                        .visit_metrics
                        .candidate_visits_required_exact,
                    candidate_visits_required_ngram: attempt
                        .visit_metrics
                        .candidate_visits_required_ngram,
                    candidate_visits_required_short_fallback: attempt
                        .visit_metrics
                        .candidate_visits_required_short_fallback,
                    max_candidate_visits: Some(attempt.visit_metrics.max_candidate_visits),
                    ..PipelineMetrics::default()
                },
            );
            alignment
        }
        Err(error) => {
            diagnostics.failed_with_metrics(
                PipelinePhase::Alignment,
                None,
                &error,
                PipelineMetrics {
                    candidate_visits: Some(attempt.visit_metrics.candidate_visits),
                    candidate_visits_required: attempt.visit_metrics.candidate_visits_required,
                    candidate_visits_required_exact: attempt
                        .visit_metrics
                        .candidate_visits_required_exact,
                    candidate_visits_required_ngram: attempt
                        .visit_metrics
                        .candidate_visits_required_ngram,
                    candidate_visits_required_short_fallback: attempt
                        .visit_metrics
                        .candidate_visits_required_short_fallback,
                    max_candidate_visits: Some(attempt.visit_metrics.max_candidate_visits),
                    ..PipelineMetrics::default()
                },
            );
            return Err(error);
        }
    };
    let recovery = SentenceRecoveryInput {
        old_trusted_run_intervals: &old_trusted_run_intervals,
        new_trusted_run_intervals: &new_trusted_run_intervals,
        old_trusted_run_evidence: Some(TrustedRunRecoveryInput {
            descriptors: &old_trusted_run_descriptors,
            raw_region_edges: &old_trusted_region_edges,
        }),
        new_trusted_run_evidence: Some(TrustedRunRecoveryInput {
            descriptors: &new_trusted_run_descriptors,
            raw_region_edges: &new_trusted_region_edges,
        }),
        min_tokens: options.alignment.anchor_min_tokens,
        enable_known_span_sentence_shadow: false,
    };
    let comparison_result = if instrumentation.enable_sentence_recovery
        && instrumentation.enable_known_span_sentence_shadow
    {
        compare_aligned_with_known_span_sentence_shadow_diagnostics(
            &old,
            &new,
            &alignment,
            options.diff,
            recovery,
            instrumentation.watch_queries,
        )
        .map(|outcome| {
            (
                outcome.comparison,
                outcome.sentence_recovery_metrics,
                outcome.recovery_watch_diagnostics,
            )
        })
    } else if instrumentation.enable_sentence_recovery && !instrumentation.watch_queries.is_empty()
    {
        compare_aligned_with_recovery_watch_diagnostics(
            &old,
            &new,
            &alignment,
            options.diff,
            recovery,
            instrumentation.watch_queries,
        )
        .map(|outcome| {
            (
                outcome.comparison,
                outcome.sentence_recovery_metrics,
                outcome.recovery_watch_diagnostics,
            )
        })
    } else if instrumentation.enable_sentence_recovery {
        compare_aligned_with_sentence_recovery_metrics(
            &old,
            &new,
            &alignment,
            options.diff,
            recovery,
        )
        .map(|outcome| (outcome.comparison, outcome.sentence_recovery_metrics, None))
    } else {
        compare_aligned(&old, &new, &alignment, options.diff)
            .map(|comparison| (comparison, None, None))
    };
    let (comparison, sentence_recovery_metrics, recovery_watch_diagnostics) = phase_result(
        diagnostics,
        PipelinePhase::ExactDiff,
        None,
        comparison_result,
    )?;
    diagnostics.completed(
        PipelinePhase::ExactDiff,
        None,
        PipelineMetrics {
            changes: Some(comparison.changes.len()),
            formatting_changes: Some(comparison.formatting_changes.len()),
            unresolved_regions: Some(comparison.unresolved_regions.len()),
            sentence_recovery_metrics,
            ..PipelineMetrics::default()
        },
    );
    Ok(ValidatedComparisonOutcome {
        comparison,
        old_blocks: old,
        new_blocks: new,
        alignment,
        recovery_watch_diagnostics,
    })
}

#[derive(Clone, Copy)]
enum LocalizedIssueBoundary {
    Page(usize),
    Glyph(usize),
}

fn issue_boundaries(issues: &[ExtractionIssue]) -> Vec<LocalizedIssueBoundary> {
    issues.iter().filter_map(localized_issue_boundary).collect()
}

fn extraction_issue_records(
    old: Vec<ExtractionIssue>,
    new: Vec<ExtractionIssue>,
) -> Vec<ExtractionIssueRecord> {
    old.into_iter()
        .map(|issue| ExtractionIssueRecord::from_issue(DocumentSide::Old, issue))
        .chain(
            new.into_iter()
                .map(|issue| ExtractionIssueRecord::from_issue(DocumentSide::New, issue)),
        )
        .collect()
}

fn localized_issue_boundary(issue: &ExtractionIssue) -> Option<LocalizedIssueBoundary> {
    match issue.scope() {
        ExtractionScope::Page(page) => Some(LocalizedIssueBoundary::Page(page.0 as usize)),
        ExtractionScope::PageGap { retained_before } => {
            Some(LocalizedIssueBoundary::Page(retained_before))
        }
        ExtractionScope::GlyphGap { retained_before } => {
            Some(LocalizedIssueBoundary::Glyph(retained_before))
        }
        ExtractionScope::Document => None,
    }
}

fn gap_boundaries(
    document: &Document<Glyph>,
    blocks: &[BlockText],
    boundaries: &[LocalizedIssueBoundary],
) -> (Vec<usize>, Vec<usize>) {
    let glyph_positions = document
        .items()
        .iter()
        .enumerate()
        .map(|(position, glyph)| (glyph.id, position))
        .collect::<std::collections::HashMap<_, _>>();
    let block_extents = blocks
        .iter()
        .map(|block| {
            block
                .raw
                .source_map
                .iter()
                .flat_map(|entry| &entry.source.atoms)
                .chain(
                    block
                        .raw
                        .unmapped
                        .iter()
                        .flat_map(|token| &token.source.atoms),
                )
                .flat_map(|atom| match atom {
                    crate::normalize::TextSourceAtom::Glyph(glyph) => [Some(glyph), None],
                    crate::normalize::TextSourceAtom::SyntheticSpace {
                        preceding,
                        following,
                    }
                    | crate::normalize::TextSourceAtom::LineBreak {
                        preceding,
                        following,
                    } => [Some(preceding), Some(following)],
                })
                .flatten()
                .filter_map(|glyph| glyph_positions.get(glyph).copied())
                .fold(None, |extent: Option<(usize, usize)>, position| {
                    Some(extent.map_or((position, position), |(min, max)| {
                        (min.min(position), max.max(position))
                    }))
                })
        })
        .collect::<Vec<Option<(usize, usize)>>>();

    let mut straddling_block_indices = Vec::new();
    let mut result = boundaries
        .iter()
        .map(|boundary| match *boundary {
            LocalizedIssueBoundary::Page(page) => blocks.partition_point(|block| {
                block
                    .pages
                    .last()
                    .is_some_and(|last_page| (*last_page as usize) < page)
            }),
            LocalizedIssueBoundary::Glyph(retained_before) => {
                for (index, extent) in block_extents.iter().enumerate() {
                    if extent
                        .is_some_and(|(min, max)| min < retained_before && retained_before <= max)
                    {
                        straddling_block_indices.push(index);
                    }
                }
                block_extents
                    .iter()
                    .position(|extent| extent.is_some_and(|(_, max)| max >= retained_before))
                    .unwrap_or(blocks.len())
            }
        })
        .collect::<Vec<_>>();
    result.sort_unstable();
    result.dedup();
    straddling_block_indices.sort_unstable();
    straddling_block_indices.dedup();
    (result, straddling_block_indices)
}

fn phase_result<T>(
    diagnostics: &mut PipelineDiagnostics,
    phase: PipelinePhase,
    side: Option<DocumentSide>,
    result: Result<T>,
) -> Result<T> {
    result.inspect_err(|error| {
        diagnostics.failed(phase, side, error);
    })
}

fn conservative_coverage(total_tokens: usize, extraction_complete: bool) -> crate::diff::Coverage {
    crate::diff::Coverage {
        resolved_tokens: 0,
        total_tokens,
        ratio: extraction_complete.then_some(if total_tokens == 0 { 1.0 } else { 0.0 }),
    }
}

fn record_ngram_token_element_budget(
    old: &[BlockText],
    new: &[BlockText],
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<()> {
    let old_elements = phase_result(
        diagnostics,
        PipelinePhase::NgramBudget,
        Some(DocumentSide::Old),
        estimate_ngram_token_elements(old, options.ngram_size, options.max_ngram_token_elements),
    )?;
    diagnostics.completed(
        PipelinePhase::NgramBudget,
        Some(DocumentSide::Old),
        PipelineMetrics {
            ngram_token_elements: Some(old_elements),
            ..PipelineMetrics::default()
        },
    );
    let new_elements = phase_result(
        diagnostics,
        PipelinePhase::NgramBudget,
        Some(DocumentSide::New),
        estimate_ngram_token_elements(new, options.ngram_size, options.max_ngram_token_elements),
    )?;
    diagnostics.completed(
        PipelinePhase::NgramBudget,
        Some(DocumentSide::New),
        PipelineMetrics {
            ngram_token_elements: Some(new_elements),
            ..PipelineMetrics::default()
        },
    );
    let aggregate = old_elements
        .checked_add(new_elements)
        .ok_or(Error::LimitExceeded {
            resource: "alignment n-gram token elements",
            limit: options.max_ngram_token_elements,
        })
        .and_then(|total| {
            if total > options.max_ngram_token_elements {
                Err(Error::LimitExceeded {
                    resource: "alignment n-gram token elements",
                    limit: options.max_ngram_token_elements,
                })
            } else {
                Ok(())
            }
        });
    phase_result(diagnostics, PipelinePhase::NgramBudget, None, aggregate)?;
    Ok(())
}

fn validate_ngram_token_element_limit(limit: usize) -> Result<()> {
    if limit == 0 {
        return Err(Error::InvalidConfiguration(
            "pipeline max_ngram_token_elements must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

fn record_pre_layout_token_counts(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: DiffOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<(usize, usize)> {
    let old_tokens = phase_result(
        diagnostics,
        PipelinePhase::PreLayoutBudget,
        Some(DocumentSide::Old),
        painting_raw_token_lower_bound(old, options.max_tokens),
    )?;
    if old_tokens > options.max_tokens {
        return phase_result(
            diagnostics,
            PipelinePhase::PreLayoutBudget,
            Some(DocumentSide::Old),
            Err(Error::LimitExceeded {
                resource: "diff raw evidence tokens",
                limit: options.max_tokens,
            }),
        );
    }
    diagnostics.completed(
        PipelinePhase::PreLayoutBudget,
        Some(DocumentSide::Old),
        PipelineMetrics {
            raw_tokens: Some(old_tokens),
            ..PipelineMetrics::default()
        },
    );
    let new_tokens = phase_result(
        diagnostics,
        PipelinePhase::PreLayoutBudget,
        Some(DocumentSide::New),
        painting_raw_token_lower_bound(new, options.max_tokens),
    )?;
    if new_tokens > options.max_tokens {
        return phase_result(
            diagnostics,
            PipelinePhase::PreLayoutBudget,
            Some(DocumentSide::New),
            Err(Error::LimitExceeded {
                resource: "diff raw evidence tokens",
                limit: options.max_tokens,
            }),
        );
    }
    diagnostics.completed(
        PipelinePhase::PreLayoutBudget,
        Some(DocumentSide::New),
        PipelineMetrics {
            raw_tokens: Some(new_tokens),
            ..PipelineMetrics::default()
        },
    );
    phase_result(
        diagnostics,
        PipelinePhase::PreLayoutBudget,
        None,
        enforce_diff_raw_token_budget(old_tokens, new_tokens, options),
    )?;
    Ok((old_tokens, new_tokens))
}

fn painting_raw_token_lower_bound(document: &Document<Glyph>, limit: usize) -> Result<usize> {
    let mut tokens = 0_usize;
    for glyph in document
        .items()
        .iter()
        .filter(|glyph| is_comparison_visible(glyph))
    {
        let glyph_tokens = match &glyph.text {
            crate::model::DecodedText::Mapped(text) => text.chars().count(),
            crate::model::DecodedText::Unmapped { .. } => 1,
        };
        tokens = tokens
            .checked_add(glyph_tokens)
            .ok_or(crate::Error::LimitExceeded {
                resource: "diff raw evidence tokens",
                limit,
            })?;
    }
    Ok(tokens)
}

fn prepare(
    document: &Document<Glyph>,
    options: PipelineOptions,
    side: DocumentSide,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<PreparedDocument> {
    let document = Document::with_vector_lines(
        document
            .items()
            .iter()
            .filter(|glyph| is_comparison_visible(glyph))
            .cloned()
            .collect(),
        document.vector_lines().to_vec(),
    );
    let painting_glyphs = document.items().len();
    let lines = phase_result(
        diagnostics,
        PipelinePhase::LineReconstruction,
        Some(side),
        reconstruct_lines(&document, options.line),
    )?;
    diagnostics.completed(
        PipelinePhase::LineReconstruction,
        Some(side),
        PipelineMetrics {
            painting_glyphs: Some(painting_glyphs),
            lines: Some(lines.len()),
            ..PipelineMetrics::default()
        },
    );
    let reconstruction = phase_result(
        diagnostics,
        PipelinePhase::BlockReconstruction,
        Some(side),
        reconstruct_blocks_with_issues(&document, &lines, options.block),
    )?;
    let blocks = reconstruction.blocks;
    let trusted_run_intervals = reconstruction.trusted_run_intervals;
    let trusted_run_descriptors = reconstruction.trusted_run_descriptors;
    let trusted_region_edges = reconstruction.trusted_region_edges;
    if let Err(error) =
        validate_trusted_run_interval_count(blocks.len(), trusted_run_intervals.len())
    {
        return phase_result(
            diagnostics,
            PipelinePhase::BlockReconstruction,
            Some(side),
            Err(error),
        );
    }
    diagnostics.completed(
        PipelinePhase::BlockReconstruction,
        Some(side),
        PipelineMetrics {
            painting_glyphs: Some(painting_glyphs),
            lines: Some(lines.len()),
            blocks: Some(blocks.len()),
            ..PipelineMetrics::default()
        },
    );
    let normalized = phase_result(
        diagnostics,
        PipelinePhase::Normalization,
        Some(side),
        normalize_blocks(&document, &lines, &blocks),
    )?;
    if !normalized
        .iter()
        .zip(&blocks)
        .all(|(normalized, block)| normalized.block == block.id)
        || normalized.len() != blocks.len()
    {
        return phase_result(
            diagnostics,
            PipelinePhase::Normalization,
            Some(side),
            Err(Error::Unresolved(
                "normalized blocks do not preserve reconstruction order".to_owned(),
            )),
        );
    }
    diagnostics.completed(
        PipelinePhase::Normalization,
        Some(side),
        PipelineMetrics {
            blocks: Some(blocks.len()),
            normalized_blocks: Some(normalized.len()),
            ..PipelineMetrics::default()
        },
    );
    let uncertain_line_ids = reconstruction
        .issues
        .into_iter()
        .flat_map(|issue| match issue {
            LayoutIssue::UnknownReadingOrder { page: _, line_ids } => line_ids,
        })
        .collect::<std::collections::HashSet<_>>();
    let uncertain_blocks = blocks
        .iter()
        .filter(|block| {
            block
                .lines
                .iter()
                .any(|line_id| uncertain_line_ids.contains(line_id))
        })
        .map(|block| block.id)
        .collect::<std::collections::HashSet<_>>();
    let uncertain_block_indices = normalized
        .iter()
        .enumerate()
        .filter_map(|(index, block)| uncertain_blocks.contains(&block.block).then_some(index))
        .collect();
    Ok(PreparedDocument {
        blocks: normalized,
        uncertain_block_indices,
        trusted_run_intervals,
        trusted_run_descriptors,
        trusted_region_edges,
    })
}

struct PreparedDocument {
    blocks: Vec<BlockText>,
    uncertain_block_indices: Vec<usize>,
    trusted_run_intervals: Vec<Option<TrustedRunInterval>>,
    trusted_run_descriptors: Vec<TrustedRunDescriptor>,
    trusted_region_edges: Vec<TrustedRegionEdge>,
}

fn validate_trusted_run_interval_count(block_count: usize, interval_count: usize) -> Result<()> {
    if block_count != interval_count {
        return Err(Error::Unresolved(
            "block reconstruction metadata length does not match blocks".to_owned(),
        ));
    }
    Ok(())
}

fn is_painting(mode: TextRenderMode) -> bool {
    matches!(
        mode,
        TextRenderMode::Fill
            | TextRenderMode::Stroke
            | TextRenderMode::FillAndStroke
            | TextRenderMode::FillAndClip
            | TextRenderMode::StrokeAndClip
            | TextRenderMode::FillStrokeAndClip
    )
}

fn is_comparison_visible(glyph: &Glyph) -> bool {
    is_painting(glyph.render_mode)
        && glyph.crop_status != GlyphCropStatus::Outside
        && glyph.path_clip_status != GlyphPathClipStatus::Outside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::{BlockRole, TrustedRunId},
        model::{DecodedText, FontId, GlyphId, GlyphProvenance, PageId, Rect, Vec2},
        pdf::ObjectRef,
    };

    #[test]
    fn prepared_document_retains_trusted_run_descriptors() {
        let document = Document::new(vec![Glyph {
            id: GlyphId(1),
            text: DecodedText::Mapped("A".to_owned()),
            raw_code: vec![b'A'],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 10.0, y: 10.0 },
                max: Vec2 { x: 20.0, y: 20.0 },
            },
            baseline: Vec2 { x: 10.0, y: 10.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: 0,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: 0,
            },
        }]);
        let mut diagnostics = PipelineDiagnostics::new();

        let prepared = prepare(
            &document,
            PipelineOptions::default(),
            DocumentSide::Old,
            &mut diagnostics,
        )
        .expect("one supported line should prepare");

        assert_eq!(prepared.trusted_run_descriptors.len(), 1);
        let descriptor = &prepared.trusted_run_descriptors[0];
        assert_eq!(descriptor.id, TrustedRunId(0));
        assert_eq!(descriptor.page, PageId(0));
        assert_eq!(descriptor.block_indices, vec![0]);
        assert_eq!(descriptor.trusted_block_indices, vec![0]);
        assert_eq!(descriptor.role, Some(BlockRole::Body));
        assert!(prepared.trusted_region_edges.is_empty());
        assert_eq!(
            prepared.trusted_run_intervals[0].map(|run| run.run_id),
            Some(descriptor.id)
        );
    }

    #[test]
    fn trusted_run_interval_metadata_must_match_block_count() {
        validate_trusted_run_interval_count(2, 2).expect("parallel metadata should be accepted");

        let error = validate_trusted_run_interval_count(2, 1)
            .expect_err("missing block metadata must be rejected");
        assert!(matches!(error, Error::Unresolved(message) if message.contains("metadata length")));
    }
}
