use crate::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentOptions,
        AlignmentSpan, BlockFeatures, InvertedIndexCandidateGenerator,
        align_ordered_with_metrics_and_gap_plan, build_block_features,
        estimate_ngram_token_elements, plan_ordered_gaps, validate_alignment_options,
        validate_ngram_size,
    },
    diff::{
        Comparison, DiffOptions, ExactDisplacementInput, MAX_MYERS_EDIT_DISTANCE,
        MatchedAtomicDiff, RecoveredAtomicDiff, RecoveryOwnershipPartitionAnalysis,
        RecoveryWatchDiagnostics, RecoveryWatchQuery, SentenceRecoveryInput,
        SentenceRecoveryMetrics, TrustedRunRecoveryInput, compare_aligned,
        compare_aligned_with_atomic_edits,
        compare_aligned_with_known_span_sentence_shadow_diagnostics,
        compare_aligned_with_recovery_watch_diagnostics,
        compare_aligned_with_sentence_recovery_metrics_and_atomic_edits,
        compare_aligned_with_sentence_recovery_metrics_and_evidence, enforce_diff_raw_token_budget,
        enforce_diff_token_budget, validate_diff_options,
    },
    layout::{
        BlockOptions, LayoutIssue, LineOptions, TrustedRegionEdge, TrustedRunDescriptor,
        TrustedRunInterval, UncertainLineReason, reconstruct_blocks_with_issues, reconstruct_lines,
        validate_block_options, validate_line_options,
    },
    model::{
        Document, Glyph, GlyphCropStatus, GlyphDisplacement, GlyphEvidence, GlyphId,
        GlyphPathClipStatus, TextRenderMode,
    },
    normalize::{BlockText, MappedText, TextSourceAtom, normalize_blocks},
    report::{DocumentSide, ExtractionIssueRecord, ExtractionStatus},
    source::{ExtractionIssue, ExtractionOutcome, ExtractionScope},
};
use std::time::{Duration, Instant};

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
    /// tokens, diff edit distance, and assessment work/range limits are
    /// scaled. The edit-distance budget saturates at the bounded Myers
    /// implementation's 64 MiB trace cap.
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
        self.diff.max_assessment_work = scale_limit(self.diff.max_assessment_work);
        self.diff.max_assessment_ranges = scale_limit(self.diff.max_assessment_ranges);
        Ok(self)
    }

    fn validate(self) -> Result<Self> {
        validate_line_options(self.line)?;
        validate_block_options(self.block)?;
        validate_ngram_size(self.ngram_size)?;
        if self.max_ngram_token_elements == 0 {
            return Err(Error::InvalidConfiguration(
                "pipeline max_ngram_token_elements must be greater than zero".to_owned(),
            ));
        }
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
    /// Normalized old-side blocks backing the comparison spans, including
    /// retained evidence left unresolved by document-scoped extraction issues.
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
    pub matched_atomic_diffs: Vec<MatchedAtomicDiff>,
    pub recovered_atomic_diffs: Vec<RecoveredAtomicDiff>,
    pub recovery_ownership_partition: Option<RecoveryOwnershipPartitionAnalysis>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComparisonOutcomeWithAtomicEdits {
    pub outcome: ComparisonOutcome,
    pub alignment: Option<Alignment>,
    pub matched_atomic_diffs: Vec<MatchedAtomicDiff>,
    pub recovered_atomic_diffs: Vec<RecoveredAtomicDiff>,
    pub recovery_ownership_partition: Option<RecoveryOwnershipPartitionAnalysis>,
}

struct InstrumentedComparisonOutcome {
    outcome: ComparisonOutcome,
    alignment: Option<Alignment>,
    recovery_watch_diagnostics: Option<RecoveryWatchDiagnostics>,
    matched_atomic_diffs: Vec<MatchedAtomicDiff>,
    recovered_atomic_diffs: Vec<RecoveredAtomicDiff>,
    recovery_ownership_partition: Option<RecoveryOwnershipPartitionAnalysis>,
}

impl InstrumentedComparisonOutcome {
    fn into_recovery_watch(self) -> ComparisonOutcomeWithRecoveryWatch {
        ComparisonOutcomeWithRecoveryWatch {
            outcome: self.outcome,
            alignment: self.alignment,
            diagnostics: self.recovery_watch_diagnostics,
            matched_atomic_diffs: self.matched_atomic_diffs,
            recovered_atomic_diffs: self.recovered_atomic_diffs,
            recovery_ownership_partition: self.recovery_ownership_partition,
        }
    }
}

struct ValidatedComparisonOutcome {
    comparison: Comparison,
    old_blocks: Vec<BlockText>,
    new_blocks: Vec<BlockText>,
    alignment: Alignment,
    recovery_watch_diagnostics: Option<RecoveryWatchDiagnostics>,
    matched_atomic_diffs: Vec<MatchedAtomicDiff>,
    recovered_atomic_diffs: Vec<RecoveredAtomicDiff>,
    recovery_ownership_partition: Option<RecoveryOwnershipPartitionAnalysis>,
}

#[derive(Clone, Copy)]
struct ExtractionComparisonInstrumentation<'a> {
    watch_queries: &'a [RecoveryWatchQuery<'a>],
    enable_known_span_sentence_shadow: bool,
    enable_sentence_edge_gate_shadow: bool,
    retain_atomic_edits: bool,
}

#[derive(Clone, Copy)]
struct ComparisonInstrumentation<'a> {
    old_issue_boundaries: &'a [LocalizedIssueBoundary],
    new_issue_boundaries: &'a [LocalizedIssueBoundary],
    enable_sentence_recovery: bool,
    watch_queries: &'a [RecoveryWatchQuery<'a>],
    enable_known_span_sentence_shadow: bool,
    enable_sentence_edge_gate_shadow: bool,
    retain_atomic_edits: bool,
    /// Optional native structure-order proofs per side. A proof marks blocks
    /// whose content order is established by the parser structure tree; it
    /// carries order only and never asserts equality or layout provenance.
    old_native_order_proof: Option<&'a crate::document::NativeOrderProof<'a>>,
    new_native_order_proof: Option<&'a crate::document::NativeOrderProof<'a>>,
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
    /// Painting glyphs retained for comparison after page `CropBox` filtering.
    pub painting_glyphs: Option<usize>,
    pub lines: Option<usize>,
    pub blocks: Option<usize>,
    pub normalized_blocks: Option<usize>,
    /// Uncertain lines whose partition has several regions with an unproven
    /// inter-region order.
    pub uncertain_lines_unproven_inter_region_order: Option<usize>,
    /// Uncertain lines outside the proven monotone runs of a single leaf.
    pub uncertain_lines_render_disorder: Option<usize>,
    /// Uncertain lines outside the trusted runs of a known region order.
    pub uncertain_lines_untrusted_in_known_order: Option<usize>,
    /// Lines ordered by a geometrically inferred region order rather than a
    /// proven one; not uncertain, but changes derived from them are
    /// reported at low confidence.
    pub inferred_reading_order_lines: Option<usize>,
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
    pub proven_changed_regions: Option<usize>,
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
    /// Wall time from the previous diagnostic record (or the start of the
    /// comparison) to this record, so per-phase cost can be attributed without
    /// threading a clock through every phase call site.
    pub duration: Duration,
}

#[derive(Debug)]
pub struct PipelineDiagnostics {
    records: Vec<PipelineDiagnosticRecord>,
    /// Start of the interval currently being measured; reset by every record.
    phase_started: Instant,
}

impl Default for PipelineDiagnostics {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            phase_started: Instant::now(),
        }
    }
}

impl PipelineDiagnostics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn records(&self) -> &[PipelineDiagnosticRecord] {
        &self.records
    }

    fn begin(&mut self) {
        self.records.clear();
        self.phase_started = Instant::now();
    }

    /// Appends another diagnostics' records verbatim, keeping their recorded
    /// durations. Used to merge per-side records from parallel phases back
    /// into deterministic (old-side first) order. Merged records keep their
    /// own durations; the parent measurement interval restarts so the next
    /// parent-side record does not include time already accounted for inside
    /// the merged per-side records.
    fn append(&mut self, other: PipelineDiagnostics) {
        self.records.extend(other.records);
        self.phase_started = Instant::now();
    }

    fn push(&mut self, mut record: PipelineDiagnosticRecord) {
        record.duration = self.phase_started.elapsed();
        self.phase_started = Instant::now();
        self.records.push(record);
    }

    fn completed(
        &mut self,
        phase: PipelinePhase,
        side: Option<DocumentSide>,
        metrics: PipelineMetrics,
    ) {
        self.push(PipelineDiagnosticRecord {
            phase,
            side,
            status: PipelinePhaseStatus::Completed,
            metrics,
            error: None,
            duration: Duration::ZERO,
        });
    }

    fn incomplete(&mut self, phase: PipelinePhase) {
        self.push(PipelineDiagnosticRecord {
            phase,
            side: None,
            status: PipelinePhaseStatus::Incomplete,
            metrics: PipelineMetrics::default(),
            error: None,
            duration: Duration::ZERO,
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
        self.push(PipelineDiagnosticRecord {
            phase,
            side,
            status: PipelinePhaseStatus::Failed,
            metrics,
            error: Some(error.into()),
            duration: Duration::ZERO,
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
        ExtractionComparisonInstrumentation {
            watch_queries: &[],
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
            retain_atomic_edits: false,
        },
    )
    .map(|outcome| (outcome.outcome, outcome.alignment))
}

/// Compares extracted documents while retaining exact edit traces from both
/// accepted alignment matches and uncertain-region replacements.
///
/// The alignment is `None` and both trace lists are empty when a
/// document-scoped extraction issue suppresses the diff.
///
/// # Errors
///
/// Returns an error when configuration validation, layout reconstruction,
/// alignment, exact diffing, trace retention, or a resource limit fails.
pub fn compare_extraction_outcomes_with_atomic_edits(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<ComparisonOutcomeWithAtomicEdits> {
    compare_extraction_outcomes_with_recovery_watch_inner(
        old,
        new,
        options,
        diagnostics,
        ExtractionComparisonInstrumentation {
            watch_queries: &[],
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
            retain_atomic_edits: true,
        },
    )
    .map(|outcome| ComparisonOutcomeWithAtomicEdits {
        outcome: outcome.outcome,
        alignment: outcome.alignment,
        matched_atomic_diffs: outcome.matched_atomic_diffs,
        recovered_atomic_diffs: outcome.recovered_atomic_diffs,
        recovery_ownership_partition: outcome.recovery_ownership_partition,
    })
}

/// Compares extracted documents and records the Sentence edge-gate shadow.
///
/// This entry point is intended for execution traces. It reuses production
/// scores and does not enable the separate known-span replay.
///
/// # Errors
///
/// Returns an error when configuration validation, layout reconstruction,
/// alignment, exact diffing, or a resource limit fails.
pub fn compare_extraction_outcomes_with_sentence_edge_gate_shadow_diagnostics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<ComparisonOutcome> {
    compare_extraction_outcomes_with_recovery_watch_inner(
        old,
        new,
        options,
        diagnostics,
        ExtractionComparisonInstrumentation {
            watch_queries: &[],
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: true,
            retain_atomic_edits: false,
        },
    )
    .map(|outcome| outcome.outcome)
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
        ExtractionComparisonInstrumentation {
            watch_queries,
            enable_known_span_sentence_shadow: true,
            enable_sentence_edge_gate_shadow: true,
            retain_atomic_edits: true,
        },
    )
    .map(InstrumentedComparisonOutcome::into_recovery_watch)
}

fn compare_extraction_outcomes_with_recovery_watch_inner(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
    instrumentation: ExtractionComparisonInstrumentation<'_>,
) -> Result<InstrumentedComparisonOutcome> {
    compare_extraction_outcomes_with_structures_inner(
        old,
        new,
        options,
        diagnostics,
        instrumentation,
        (None, None),
        None,
    )
}

/// Work spent acquiring optional native order proofs, per side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeOrderWork {
    pub old_spent: usize,
    pub new_spent: usize,
}

/// Compares extracted documents with optional native order proofs.
///
/// Precondition: each [`ExtractionOutcome`] must originate from the PDF passed
/// beside it. The runtime `NativeOrderProof::binds` check prevents a proof from
/// being applied to a different document after acquisition, but it cannot
/// itself prove that an arbitrary supplied PDF and a caller-constructed
/// outcome share an origin; the CLI supplies the same parsed source pair it
/// extracted from.
///
/// The optional proofs are acquired from the supplied PDFs with the unchanged
/// shared default assessment budget; absence or failure leaves the comparison
/// exactly as before. Returns the comparison together with the per-side work
/// spent on proof acquisition so callers can record it separately.
///
/// # Errors
///
/// Returns an error when configuration validation, layout reconstruction,
/// alignment, exact diffing, or a resource limit fails.
pub fn compare_extraction_outcomes_with_native_order_proofs(
    old_pdf: Option<Box<dyn crate::pdf::ParsedPdf>>,
    old: ExtractionOutcome,
    new_pdf: Option<Box<dyn crate::pdf::ParsedPdf>>,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<(ComparisonOutcome, NativeOrderWork)> {
    let mut work = NativeOrderWork::default();
    let outcome = compare_extraction_outcomes_with_structures_inner(
        old,
        new,
        options,
        diagnostics,
        ExtractionComparisonInstrumentation {
            watch_queries: &[],
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: true,
            retain_atomic_edits: false,
        },
        (old_pdf, new_pdf),
        Some(&mut work),
    )?;
    Ok((outcome.outcome, work))
}

/// One optional parsed source per document side.
type NativePdfPair = (
    Option<Box<dyn crate::pdf::ParsedPdf>>,
    Option<Box<dyn crate::pdf::ParsedPdf>>,
);

fn compare_extraction_outcomes_with_structures_inner(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
    instrumentation: ExtractionComparisonInstrumentation<'_>,
    native_pdfs: NativePdfPair,
    work_out: Option<&mut NativeOrderWork>,
) -> Result<InstrumentedComparisonOutcome> {
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
    // One shared default assessment budget covers both optional acquisitions;
    // per-side deltas are recorded. Each owned document is moved through the
    // temporary validated store and handed back as a stable reference with an
    // optional proof; the parsed sources are dropped inside the helper.
    let (old_pdf, new_pdf) = native_pdfs;
    let mut proof_budget = options.diff.max_assessment_work;
    let old_before = proof_budget;
    crate::document::with_native_order_proof(
        old_pdf,
        old_document,
        &mut proof_budget,
        |old_document, old_proof, proof_budget| {
            let old_spent = old_before.saturating_sub(*proof_budget);
            let new_before = *proof_budget;
            crate::document::with_native_order_proof(
                new_pdf,
                new_document,
                proof_budget,
                |new_document, new_proof, proof_budget| {
                    let new_spent = new_before.saturating_sub(*proof_budget);
                    if let Some(work_out) = work_out {
                        *work_out = NativeOrderWork {
                            old_spent,
                            new_spent,
                        };
                    }
                    let old_glyph_evidence = glyph_evidence(old_document);
                    let new_glyph_evidence = glyph_evidence(new_document);
                    if old_complete && new_complete {
                        diagnostics.completed(
                            PipelinePhase::CompletenessGate,
                            None,
                            PipelineMetrics::default(),
                        );
                        let compared = compare_validated_glyph_documents_inner(
                            old_document,
                            new_document,
                            options,
                            diagnostics,
                            ComparisonInstrumentation {
                                old_native_order_proof: old_proof,
                                new_native_order_proof: new_proof,
                                old_issue_boundaries: &[],
                                new_issue_boundaries: &[],
                                enable_sentence_recovery: true,
                                watch_queries: instrumentation.watch_queries,
                                enable_known_span_sentence_shadow: instrumentation
                                    .enable_known_span_sentence_shadow,
                                enable_sentence_edge_gate_shadow: instrumentation
                                    .enable_sentence_edge_gate_shadow,
                                retain_atomic_edits: instrumentation.retain_atomic_edits,
                            },
                        )?;
                        return Ok(InstrumentedComparisonOutcome {
                            outcome: ComparisonOutcome {
                                comparison: compared.comparison,
                                extraction: ExtractionStatus::complete(),
                                old_blocks: compared.old_blocks,
                                new_blocks: compared.new_blocks,
                                old_glyph_evidence,
                                new_glyph_evidence,
                            },
                            alignment: Some(compared.alignment),
                            recovery_watch_diagnostics: compared.recovery_watch_diagnostics,
                            matched_atomic_diffs: compared.matched_atomic_diffs,
                            recovered_atomic_diffs: compared.recovered_atomic_diffs,
                            recovery_ownership_partition: compared.recovery_ownership_partition,
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
                            old_document,
                            new_document,
                            options,
                            diagnostics,
                            ComparisonInstrumentation {
                                old_native_order_proof: old_proof,
                                new_native_order_proof: new_proof,
                                old_issue_boundaries: &old_gap_boundaries,
                                new_issue_boundaries: &new_gap_boundaries,
                                enable_sentence_recovery: false,
                                watch_queries: instrumentation.watch_queries,
                                enable_known_span_sentence_shadow: instrumentation
                                    .enable_known_span_sentence_shadow,
                                enable_sentence_edge_gate_shadow: instrumentation
                                    .enable_sentence_edge_gate_shadow,
                                retain_atomic_edits: instrumentation.retain_atomic_edits,
                            },
                        )?;
                        if !old_complete {
                            compared.comparison.old_coverage.ratio = None;
                        }
                        if !new_complete {
                            compared.comparison.new_coverage.ratio = None;
                        }
                        let issues = extraction_issue_records(old_issues, new_issues);
                        return Ok(InstrumentedComparisonOutcome {
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
                            recovery_watch_diagnostics: compared.recovery_watch_diagnostics,
                            matched_atomic_diffs: compared.matched_atomic_diffs,
                            recovered_atomic_diffs: compared.recovered_atomic_diffs,
                            recovery_ownership_partition: compared.recovery_ownership_partition,
                        });
                    }

                    record_pre_layout_token_counts(
                        old_document,
                        new_document,
                        options.diff,
                        diagnostics,
                    )?;
                    let issues = extraction_issue_records(old_issues, new_issues);
                    // A document-scoped issue prevents correspondence claims, but extracted
                    // evidence on either side still belongs to the unresolved partition.
                    let old_blocks =
                        prepare(old_document, options, DocumentSide::Old, diagnostics, None)?
                            .blocks;
                    let new_blocks =
                        prepare(new_document, options, DocumentSide::New, diagnostics, None)?
                            .blocks;
                    let spans = if old_blocks.is_empty() && new_blocks.is_empty() {
                        Vec::new()
                    } else {
                        vec![AlignmentSpan {
                            kind: AlignmentKind::Unresolved,
                            old: old_blocks.iter().map(|block| block.block).collect(),
                            new: new_blocks.iter().map(|block| block.block).collect(),
                            score: 0.0,
                            canonical_similarity: 0.0,
                            score_margin: None,
                            confidence: AlignmentConfidence::Low,
                            evidence: vec![AlignmentEvidence::ExtractionGap],
                            old_separator: None,
                            new_separator: None,
                        }]
                    };
                    let alignment = Alignment {
                        spans,
                        main_anchors: Vec::new(),
                        move_candidates: Vec::new(),
                    };
                    let mut comparison =
                        compare_aligned(&old_blocks, &new_blocks, &alignment, options.diff)?;
                    if !old_complete {
                        comparison.old_coverage.ratio = None;
                    }
                    if !new_complete {
                        comparison.new_coverage.ratio = None;
                    }
                    Ok(InstrumentedComparisonOutcome {
                        outcome: ComparisonOutcome {
                            comparison,
                            extraction: ExtractionStatus {
                                old_complete,
                                new_complete,
                                issues,
                            },
                            old_blocks,
                            new_blocks,
                            old_glyph_evidence,
                            new_glyph_evidence,
                        },
                        alignment: None,
                        recovery_watch_diagnostics: None,
                        matched_atomic_diffs: Vec::new(),
                        recovered_atomic_diffs: Vec::new(),
                        recovery_ownership_partition: None,
                    })
                },
            )
        },
    )
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
            old_native_order_proof: None,
            new_native_order_proof: None,
            old_issue_boundaries: &[],
            new_issue_boundaries: &[],
            enable_sentence_recovery: true,
            watch_queries: &[],
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
            retain_atomic_edits: false,
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
    // Line/block reconstruction, normalization, and feature builds are pure
    // per-side functions, so both sides run in parallel. Each side records
    // into its own diagnostics, merged afterwards in the same old-then-new
    // order the sequential pipeline produced, and a failed old side hides the
    // new side's records exactly as a sequential early return did.
    let ((old_prepared, old_prepare_diagnostics), (new_prepared, new_prepare_diagnostics)) =
        rayon::join(
            || {
                prepare_with_diagnostics(
                    old,
                    options,
                    DocumentSide::Old,
                    instrumentation
                        .old_native_order_proof
                        .filter(|proof| proof.binds(old)),
                )
            },
            || {
                prepare_with_diagnostics(
                    new,
                    options,
                    DocumentSide::New,
                    instrumentation
                        .new_native_order_proof
                        .filter(|proof| proof.binds(new)),
                )
            },
        );
    diagnostics.append(old_prepare_diagnostics);
    if old_prepared.is_ok() {
        diagnostics.append(new_prepare_diagnostics);
    }
    let old_prepared = old_prepared?;
    let new_prepared = new_prepared?;
    let PreparedDocument {
        blocks: old,
        uncertain_block_indices: old_uncertain_block_indices,
        inferred_order_block_indices: old_inferred_order_block_indices,
        native_order_blocks: old_native_order_blocks,
        trusted_run_intervals: old_trusted_run_intervals,
        trusted_run_descriptors: old_trusted_run_descriptors,
        trusted_region_edges: old_trusted_region_edges,
        displacements: old_displacements,
    } = old_prepared;
    let PreparedDocument {
        blocks: new,
        uncertain_block_indices: new_uncertain_block_indices,
        inferred_order_block_indices: new_inferred_order_block_indices,
        native_order_blocks: new_native_order_blocks,
        trusted_run_intervals: new_trusted_run_intervals,
        trusted_run_descriptors: new_trusted_run_descriptors,
        trusted_region_edges: new_trusted_region_edges,
        displacements: new_displacements,
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
    // Feature builds are pure per-side work; run both sides in parallel and
    // merge records in the same old-then-new order the sequential pipeline
    // produced. A failed old side hides the new side's records exactly as a
    // sequential early return did.
    let ((old_features, old_feature_diagnostics), (new_features, new_feature_diagnostics)) =
        rayon::join(
            || build_features_with_diagnostics(&old, options.ngram_size, DocumentSide::Old),
            || build_features_with_diagnostics(&new, options.ngram_size, DocumentSide::New),
        );
    diagnostics.append(old_feature_diagnostics);
    let old_features = old_features?;
    diagnostics.append(new_feature_diagnostics);
    let new_features = new_features?;
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
            &old_inferred_order_block_indices,
            &new_inferred_order_block_indices,
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
    let visit_metrics = PipelineMetrics {
        candidate_visits: Some(attempt.visit_metrics.candidate_visits),
        candidate_visits_required: attempt.visit_metrics.candidate_visits_required,
        candidate_visits_required_exact: attempt.visit_metrics.candidate_visits_required_exact,
        candidate_visits_required_ngram: attempt.visit_metrics.candidate_visits_required_ngram,
        candidate_visits_required_short_fallback: attempt
            .visit_metrics
            .candidate_visits_required_short_fallback,
        max_candidate_visits: Some(attempt.visit_metrics.max_candidate_visits),
        ..PipelineMetrics::default()
    };
    let alignment = match attempt.result {
        Ok(alignment) => {
            diagnostics.completed(
                PipelinePhase::Alignment,
                None,
                PipelineMetrics {
                    alignment_spans: Some(alignment.spans.len()),
                    ..visit_metrics
                },
            );
            if alignment
                .spans
                .iter()
                .any(|span| span.evidence.contains(&AlignmentEvidence::SearchIncomplete))
            {
                diagnostics
                    .records
                    .last_mut()
                    .expect("alignment diagnostic was recorded")
                    .status = PipelinePhaseStatus::Incomplete;
            }
            alignment
        }
        Err(error) => {
            diagnostics.failed_with_metrics(PipelinePhase::Alignment, None, &error, visit_metrics);
            return Err(error);
        }
    };
    // A document whose other side carries no native text at all is one-sided:
    // the whole present side is an insertion or deletion, independent of the
    // window reading-order uncertainty that the layout could not resolve. The
    // existing one-sided span contract already emits that change, so the
    // alignment is rewritten to one evidence-free insertion or deletion span
    // per present-side block and no reading-order veto survives. A clean empty
    // side is required: any extraction-gap evidence keeps the conservative
    // unresolved windows.
    let alignment = if old.is_empty() != new.is_empty()
        && !alignment
            .spans
            .iter()
            .any(|span| span.evidence.contains(&AlignmentEvidence::ExtractionGap))
    {
        one_sided_alignment(&old, &new)
    } else {
        alignment
    };
    let recovery = SentenceRecoveryInput {
        old_trusted_run_intervals: &old_trusted_run_intervals,
        new_trusted_run_intervals: &new_trusted_run_intervals,
        old_native_order_blocks: &old_native_order_blocks,
        new_native_order_blocks: &new_native_order_blocks,
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
        enable_sentence_edge_gate_shadow: instrumentation.enable_sentence_edge_gate_shadow,
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
        .and_then(|outcome| {
            let matched_atomic_diffs = outcome.matched_atomic_diffs.ok_or_else(|| {
                Error::Unresolved(
                    "atomic diff retention did not initialize matched output".to_owned(),
                )
            })?;
            let recovered_atomic_diffs = outcome.recovered_atomic_diffs.ok_or_else(|| {
                Error::Unresolved(
                    "atomic diff retention did not initialize recovery output".to_owned(),
                )
            })?;
            Ok((
                outcome.comparison,
                outcome.sentence_recovery_metrics,
                outcome.recovery_watch_diagnostics,
                matched_atomic_diffs,
                recovered_atomic_diffs,
                outcome.recovery_ownership_partition,
            ))
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
                Vec::new(),
                Vec::new(),
                outcome.recovery_ownership_partition,
            )
        })
    } else if instrumentation.enable_sentence_recovery && instrumentation.retain_atomic_edits {
        compare_aligned_with_sentence_recovery_metrics_and_atomic_edits(
            &old,
            &new,
            &alignment,
            options.diff,
            recovery,
        )
        .and_then(|outcome| {
            let matched_atomic_diffs = outcome.matched_atomic_diffs.ok_or_else(|| {
                Error::Unresolved(
                    "atomic diff retention did not initialize matched output".to_owned(),
                )
            })?;
            let recovered_atomic_diffs = outcome.recovered_atomic_diffs.ok_or_else(|| {
                Error::Unresolved(
                    "atomic diff retention did not initialize recovery output".to_owned(),
                )
            })?;
            Ok((
                outcome.comparison,
                outcome.sentence_recovery_metrics,
                None,
                matched_atomic_diffs,
                recovered_atomic_diffs,
                outcome.recovery_ownership_partition,
            ))
        })
    } else if instrumentation.enable_sentence_recovery {
        compare_aligned_with_sentence_recovery_metrics_and_evidence(
            &old,
            &new,
            &alignment,
            options.diff,
            recovery,
            ExactDisplacementInput {
                old: &old_displacements,
                new: &new_displacements,
            },
        )
        .map(|outcome| {
            (
                outcome.comparison,
                outcome.sentence_recovery_metrics,
                None,
                Vec::new(),
                Vec::new(),
                outcome.recovery_ownership_partition,
            )
        })
    } else if instrumentation.retain_atomic_edits {
        compare_aligned_with_atomic_edits(&old, &new, &alignment, options.diff).map(|outcome| {
            (
                outcome.comparison,
                None,
                None,
                outcome.matched_atomic_diffs,
                Vec::new(),
                None,
            )
        })
    } else {
        compare_aligned(&old, &new, &alignment, options.diff)
            .map(|comparison| (comparison, None, None, Vec::new(), Vec::new(), None))
    };
    let (
        comparison,
        sentence_recovery_metrics,
        recovery_watch_diagnostics,
        matched_atomic_diffs,
        recovered_atomic_diffs,
        recovery_ownership_partition,
    ) = phase_result(
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
            proven_changed_regions: Some(comparison.proven_changed_regions.len()),
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
        matched_atomic_diffs,
        recovered_atomic_diffs,
        recovery_ownership_partition,
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
        ExtractionScope::GlyphGap { retained_before }
        | ExtractionScope::PageGlyphGap {
            retained_before, ..
        } => Some(LocalizedIssueBoundary::Glyph(retained_before)),
        ExtractionScope::Document => None,
    }
}

/// Builds the per-block one-sided spans of a comparison whose other side
/// carries no native text.
///
/// The present side has no counterpart to correspond to, so the reading-order
/// uncertainty of its windows cannot change the insertion or deletion claim.
/// Each block uses the ordinary evidence-free one-sided span shape, so the
/// existing insertion/deletion emission path owns the whole side and no
/// window veto survives.
fn one_sided_alignment(old: &[BlockText], new: &[BlockText]) -> Alignment {
    let insertion = old.is_empty();
    let blocks = if insertion { new } else { old };
    let spans = blocks
        .iter()
        .map(|block| AlignmentSpan {
            kind: if insertion {
                AlignmentKind::Insertion
            } else {
                AlignmentKind::Deletion
            },
            old: if insertion {
                Vec::new()
            } else {
                vec![block.block]
            },
            new: if insertion {
                vec![block.block]
            } else {
                Vec::new()
            },
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Medium,
            evidence: Vec::new(),
            old_separator: None,
            new_separator: None,
        })
        .collect();
    Alignment {
        spans,
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
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

fn record_ngram_token_element_budget(
    old: &[BlockText],
    new: &[BlockText],
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<()> {
    let record_side_elements =
        |blocks: &[BlockText], side: DocumentSide, diagnostics: &mut PipelineDiagnostics| {
            let elements = phase_result(
                diagnostics,
                PipelinePhase::NgramBudget,
                Some(side),
                estimate_ngram_token_elements(
                    blocks,
                    options.ngram_size,
                    options.max_ngram_token_elements,
                ),
            )?;
            diagnostics.completed(
                PipelinePhase::NgramBudget,
                Some(side),
                PipelineMetrics {
                    ngram_token_elements: Some(elements),
                    ..PipelineMetrics::default()
                },
            );
            Ok(elements)
        };
    let old_elements = record_side_elements(old, DocumentSide::Old, diagnostics)?;
    let new_elements = record_side_elements(new, DocumentSide::New, diagnostics)?;
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

fn record_pre_layout_token_counts(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: DiffOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<(usize, usize)> {
    let record_side_tokens =
        |document: &Document<Glyph>, side: DocumentSide, diagnostics: &mut PipelineDiagnostics| {
            let tokens = phase_result(
                diagnostics,
                PipelinePhase::PreLayoutBudget,
                Some(side),
                painting_raw_token_lower_bound(document, options.max_tokens),
            )?;
            if tokens > options.max_tokens {
                return phase_result(
                    diagnostics,
                    PipelinePhase::PreLayoutBudget,
                    Some(side),
                    Err(Error::LimitExceeded {
                        resource: "diff raw evidence tokens",
                        limit: options.max_tokens,
                    }),
                );
            }
            diagnostics.completed(
                PipelinePhase::PreLayoutBudget,
                Some(side),
                PipelineMetrics {
                    raw_tokens: Some(tokens),
                    ..PipelineMetrics::default()
                },
            );
            Ok(tokens)
        };
    let old_tokens = record_side_tokens(old, DocumentSide::Old, diagnostics)?;
    let new_tokens = record_side_tokens(new, DocumentSide::New, diagnostics)?;
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
    native_order: Option<&crate::document::NativeOrderProof<'_>>,
) -> Result<PreparedDocument> {
    let kept = document
        .items()
        .iter()
        .map(is_comparison_visible)
        .collect::<Vec<_>>();
    let displacements = document.filtered_displacements(&kept);
    let document = Document::with_vector_lines(
        document
            .items()
            .iter()
            .filter(|glyph| is_comparison_visible(glyph))
            .cloned()
            .collect(),
        document.vector_lines().to_vec(),
    )
    .with_displacements(displacements);
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
    let inferred_order_line_ids = reconstruction.inferred_order_line_ids;
    let mut uncertain_inter_region = 0usize;
    let mut uncertain_render_disorder = 0usize;
    let mut uncertain_untrusted_known = 0usize;
    for issue in &reconstruction.issues {
        let LayoutIssue::UnknownReadingOrder {
            page: _,
            line_ids,
            reason,
        } = issue;
        let count = line_ids.len();
        match reason {
            UncertainLineReason::UnprovenInterRegionOrder => {
                uncertain_inter_region += count;
            }
            UncertainLineReason::RenderDisorderOutsideTrustedRuns => {
                uncertain_render_disorder += count;
            }
            UncertainLineReason::UntrustedLinesInKnownOrder => {
                uncertain_untrusted_known += count;
            }
        }
    }
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
            uncertain_lines_unproven_inter_region_order: Some(uncertain_inter_region),
            uncertain_lines_render_disorder: Some(uncertain_render_disorder),
            uncertain_lines_untrusted_in_known_order: Some(uncertain_untrusted_known),
            inferred_reading_order_lines: Some(inferred_order_line_ids.len()),
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
            LayoutIssue::UnknownReadingOrder {
                page: _,
                line_ids,
                reason: _,
            } => line_ids,
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
    let inferred_order_blocks = blocks
        .iter()
        .filter(|block| {
            block
                .lines
                .iter()
                .any(|line_id| inferred_order_line_ids.contains(line_id))
        })
        .map(|block| block.id)
        .collect::<std::collections::HashSet<_>>();
    let inferred_order_block_indices = normalized
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            inferred_order_blocks
                .contains(&block.block)
                .then_some(index)
        })
        .collect();
    let native_order_blocks =
        native_order_blocks_for(&normalized, &trusted_run_intervals, native_order);
    Ok(PreparedDocument {
        blocks: normalized,
        uncertain_block_indices,
        inferred_order_block_indices,
        native_order_blocks,
        trusted_run_intervals,
        trusted_run_descriptors,
        trusted_region_edges,
        displacements: document.displacements().to_vec(),
    })
}

/// Prepares one document side with side-local diagnostics so the old and new
/// sides can run concurrently; the caller merges the records deterministically.
/// Fallible all-false flags; `None` is the explicit no-proof fallback.
fn false_flags(len: usize) -> Option<Vec<bool>> {
    let mut flags = Vec::new();
    flags.try_reserve_exact(len).ok()?;
    flags.resize(len, false);
    Some(flags)
}

/// Raw primary glyph claims of one block plus whether the block itself has
/// complete, unique, single-scalar ownership.
///
/// Claims are always preserved, including duplicates and claims from otherwise
/// invalid blocks, so global ownership stays unambiguous. `None` means only an
/// allocation or count failure; the caller then falls back to the empty
/// no-proof form for the whole join.
fn raw_glyph_claims(raw: &MappedText) -> Option<(Vec<GlyphId>, bool)> {
    let scalar_count = raw.text.chars().count();
    let glyph_count = raw.source_map.iter().try_fold(0usize, |total, entry| {
        total.checked_add(entry.source.atoms.len())
    })?;
    let mut glyphs = Vec::new();
    glyphs.try_reserve_exact(glyph_count).ok()?;
    let mut unique = std::collections::HashSet::new();
    unique.try_reserve(glyph_count).ok()?;
    let mut unique_ownership = true;
    let mut structurally_valid = true;
    let mut next_start = 0usize;
    for entry in &raw.source_map {
        if entry.output_range.start != next_start
            || entry
                .output_range
                .end
                .saturating_sub(entry.output_range.start)
                != 1
            || entry.output_range.end > scalar_count
            || entry.source.atoms.is_empty()
        {
            structurally_valid = false;
        }
        for atom in &entry.source.atoms {
            if let TextSourceAtom::Glyph(glyph) = atom {
                if !unique.insert(*glyph) {
                    unique_ownership = false;
                }
                glyphs.push(*glyph);
            }
        }
        next_start = entry.output_range.end;
    }
    if next_start != scalar_count {
        structurally_valid = false;
    }
    Some((glyphs, structurally_valid && unique_ownership))
}

/// Joins an optional native structure-order proof to normalized blocks.
///
/// Fails closed to all-false flags when the proof is absent, when any
/// allocation reservation fails, or when a block's glyphs cannot be bound
/// exactly. The join is linear in blocks and glyphs, which are already bounded
/// by the extraction and reconstruction limits.
fn native_order_blocks_for(
    blocks: &[BlockText],
    intervals: &[Option<TrustedRunInterval>],
    proof: Option<&crate::document::NativeOrderProof<'_>>,
) -> Vec<bool> {
    let Some(proof) = proof else {
        return Vec::new();
    };
    let mut block_glyphs = Vec::new();
    if block_glyphs.try_reserve_exact(blocks.len()).is_err() {
        return Vec::new();
    }
    let mut eligible = Vec::new();
    if eligible.try_reserve_exact(blocks.len()).is_err() {
        return Vec::new();
    }
    for (index, block) in blocks.iter().enumerate() {
        let Some((glyphs, unique_ownership)) = raw_glyph_claims(&block.raw) else {
            return Vec::new();
        };
        let normalization_ok = block
            .normalization_events
            .iter()
            .all(|event| event.kind == crate::normalize::NormalizationKind::WhitespaceCollapse);
        eligible.push(
            block.issues.is_empty()
                && normalization_ok
                && unique_ownership
                && block.canonical.unmapped.is_empty()
                && block.line_breaks.as_ref().is_some_and(Vec::is_empty)
                && block.page_breaks.as_ref().is_some_and(Vec::is_empty)
                && block.pages.len() == 1
                && intervals.get(index).copied().flatten().is_none()
                && !glyphs.is_empty(),
        );
        block_glyphs.push(glyphs);
    }
    certified_native_order_blocks(&block_glyphs, &eligible, proof.runs())
}

/// Marks blocks whose own glyphs are a complete consecutive source subsequence.
///
/// Positive: every glyph of the block maps to one validated run, at strictly
/// increasing consecutive positions in the block's own order, with unique
/// ownership. Negative: a candidate glyph that is missing from the runs,
/// duplicated inside or across runs, owned by another block, or separated by a
/// gap in run positions stays uncertified. Unrelated run members do not veto
/// the candidate, because the returned flag asserts only the block's own order.
/// Every collection is reserved fallibly; an allocation failure returns the
/// empty no-proof form and leaves all existing uncertainty untouched.
fn certified_native_order_blocks(
    block_glyphs: &[Vec<GlyphId>],
    eligible: &[bool],
    runs: &[Vec<GlyphId>],
) -> Vec<bool> {
    let Some(mut certified) = false_flags(block_glyphs.len()) else {
        return Vec::new();
    };
    let Some(block_glyph_count) = block_glyphs
        .iter()
        .try_fold(0usize, |total, glyphs| total.checked_add(glyphs.len()))
    else {
        return certified;
    };
    let mut owner = std::collections::HashMap::new();
    if owner.try_reserve(block_glyph_count).is_err() {
        return certified;
    }
    let mut ambiguous = std::collections::HashSet::new();
    if ambiguous.try_reserve(block_glyph_count).is_err() {
        return certified;
    }
    for (index, glyphs) in block_glyphs.iter().enumerate() {
        for glyph in glyphs {
            if owner.insert(*glyph, index).is_some() {
                ambiguous.insert(*glyph);
            }
        }
    }
    let Some(run_glyph_count) = runs
        .iter()
        .try_fold(0usize, |total, run| total.checked_add(run.len()))
    else {
        return certified;
    };
    let mut location = std::collections::HashMap::new();
    if location.try_reserve(run_glyph_count).is_err() {
        return certified;
    }
    let mut duplicated = std::collections::HashSet::new();
    if duplicated.try_reserve(run_glyph_count).is_err() {
        return certified;
    }
    for (run_index, run) in runs.iter().enumerate() {
        for (position, glyph) in run.iter().enumerate() {
            if location.insert(*glyph, (run_index, position)).is_some() {
                duplicated.insert(*glyph);
            }
        }
    }
    for (index, glyphs) in block_glyphs.iter().enumerate() {
        if !eligible.get(index).copied().unwrap_or(false) || glyphs.is_empty() {
            continue;
        }
        let mut run_index = None;
        let mut previous_position = None;
        let mut ok = true;
        for glyph in glyphs {
            if ambiguous.contains(glyph) || duplicated.contains(glyph) {
                ok = false;
                break;
            }
            let Some(&(candidate_run, position)) = location.get(glyph) else {
                ok = false;
                break;
            };
            match run_index {
                None => run_index = Some(candidate_run),
                Some(current) if current != candidate_run => {
                    ok = false;
                    break;
                }
                Some(_) => {}
            }
            if previous_position.is_some_and(|previous| position != previous + 1) {
                ok = false;
                break;
            }
            previous_position = Some(position);
        }
        if ok {
            certified[index] = true;
        }
    }
    certified
}

fn prepare_with_diagnostics(
    document: &Document<Glyph>,
    options: PipelineOptions,
    side: DocumentSide,
    native_order: Option<&crate::document::NativeOrderProof<'_>>,
) -> (Result<PreparedDocument>, PipelineDiagnostics) {
    let mut diagnostics = PipelineDiagnostics::new();
    let prepared = prepare(document, options, side, &mut diagnostics, native_order);
    (prepared, diagnostics)
}

/// Builds one side's block features with side-local diagnostics; mirrors the
/// sequential phase recording including the completed metrics record.
fn build_features_with_diagnostics(
    blocks: &[BlockText],
    ngram_size: usize,
    side: DocumentSide,
) -> (Result<Vec<BlockFeatures>>, PipelineDiagnostics) {
    let mut diagnostics = PipelineDiagnostics::new();
    let features: Result<Vec<BlockFeatures>> = phase_result(
        &mut diagnostics,
        PipelinePhase::FeatureBuild,
        Some(side),
        build_block_features(blocks, ngram_size),
    );
    if features.is_ok() {
        diagnostics.completed(
            PipelinePhase::FeatureBuild,
            Some(side),
            PipelineMetrics {
                normalized_blocks: Some(blocks.len()),
                features: Some(features.as_ref().map_or(0, Vec::len)),
                ..PipelineMetrics::default()
            },
        );
    }
    (features, diagnostics)
}

struct PreparedDocument {
    blocks: Vec<BlockText>,
    uncertain_block_indices: Vec<usize>,
    /// Blocks placed by a geometrically inferred region order rather than a
    /// proven one; not excluded from anchoring, but every change whose span
    /// intersects one of them must be reported at low confidence.
    inferred_order_block_indices: Vec<usize>,
    /// Blocks whose content order is proven by the native structure-order
    /// certificate. The proof covers order only; equality still needs the
    /// exact comparison.
    native_order_blocks: Vec<bool>,
    trusted_run_intervals: Vec<Option<TrustedRunInterval>>,
    trusted_run_descriptors: Vec<TrustedRunDescriptor>,
    trusted_region_edges: Vec<TrustedRegionEdge>,
    /// Raw per-glyph displacement evidence for the comparison-visible glyphs,
    /// renumbered so dropped glyphs break run continuity.
    displacements: Vec<GlyphDisplacement>,
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

    pub(super) fn single_glyph_document() -> Document<Glyph> {
        Document::new(vec![Glyph {
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
        }])
    }

    #[test]
    fn prepared_document_retains_trusted_run_descriptors() {
        let document = single_glyph_document();
        let mut diagnostics = PipelineDiagnostics::new();

        let prepared = prepare(
            &document,
            PipelineOptions::default(),
            DocumentSide::Old,
            &mut diagnostics,
            None,
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
    fn scaled_limits_include_assessment_budgets() {
        let options = PipelineOptions {
            diff: DiffOptions {
                max_assessment_work: 10,
                max_assessment_ranges: 20,
                ..DiffOptions::default()
            },
            ..PipelineOptions::default()
        }
        .scaled_limits(2.5)
        .expect("valid limit scale");

        assert_eq!(options.diff.max_assessment_work, 25);
        assert_eq!(options.diff.max_assessment_ranges, 50);
    }

    #[test]
    fn trusted_run_interval_metadata_must_match_block_count() {
        validate_trusted_run_interval_count(2, 2).expect("parallel metadata should be accepted");

        let error = validate_trusted_run_interval_count(2, 1)
            .expect_err("missing block metadata must be rejected");
        assert!(matches!(error, Error::Unresolved(message) if message.contains("metadata length")));
    }
}

#[cfg(test)]
mod h27_order_certificate {
    use super::*;
    use crate::model::GlyphId;
    use crate::pdf::{LopdfParser, ParseLimits, PdfParser};
    use crate::source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor};

    fn glyphs(ids: &[u64]) -> Vec<GlyphId> {
        ids.iter().copied().map(GlyphId).collect()
    }

    fn eligible(len: usize) -> Vec<bool> {
        vec![true; len]
    }

    #[test]
    fn optional_acquisition_preserves_document_and_releases_source() {
        use crate::pdf::{
            DecodedStream, PageRef, ParsedPdf, PdfDict, PdfObject, PdfVersion, RawStream,
        };
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        struct TrackedPdf(Arc<AtomicBool>);
        impl Drop for TrackedPdf {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        impl ParsedPdf for TrackedPdf {
            fn version(&self) -> PdfVersion {
                PdfVersion { major: 1, minor: 7 }
            }
            fn trailer(&self) -> crate::Result<PdfDict> {
                Ok(PdfDict::new())
            }
            fn resolve(&self, _reference: crate::pdf::ObjectRef) -> crate::Result<PdfObject> {
                Ok(PdfObject::Null)
            }
            fn pages(&self) -> crate::Result<Vec<PageRef>> {
                Ok(Vec::new())
            }
            fn page_dict(&self, _page: PageRef) -> crate::Result<PdfDict> {
                Ok(PdfDict::new())
            }
            fn raw_stream(&self, _reference: crate::pdf::ObjectRef) -> crate::Result<RawStream> {
                Ok(RawStream {
                    dictionary: PdfDict::new(),
                    bytes: Vec::new(),
                })
            }
            fn decoded_stream(
                &self,
                _reference: crate::pdf::ObjectRef,
            ) -> crate::Result<DecodedStream> {
                Ok(DecodedStream {
                    dictionary: PdfDict::new(),
                    bytes: Vec::new(),
                })
            }
        }
        let document = super::tests::single_glyph_document();
        let expected = document.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let mut remaining = 0usize;
        let (preserved, absent) = crate::document::with_native_order_proof(
            Some(Box::new(TrackedPdf(alive.clone()))),
            document,
            &mut remaining,
            |document, proof, _budget| {
                assert!(
                    !alive.load(Ordering::SeqCst),
                    "the parsed owner must drop before the callback"
                );
                (document.clone(), proof.is_none())
            },
        );
        assert!(absent, "an exhausted budget must leave the proof absent");
        assert_eq!(
            preserved, expected,
            "the original document contents must be preserved"
        );
    }

    #[test]
    fn refuses_impossible_flag_length() {
        assert!(false_flags(usize::MAX).is_none());
    }

    fn block_fixture(block: u64, raw: MappedText, pages: Vec<u32>) -> BlockText {
        BlockText {
            block: crate::layout::BlockId(block),
            role: crate::layout::BlockRole::Body,
            canonical: raw.clone(),
            matching: raw.text.clone(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages,
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
            raw,
        }
    }

    #[test]
    fn wrapper_vetoes_shared_glyph_from_ineligible_block() {
        let origin: Document<Glyph> = Document::new(Vec::new());
        let proof =
            crate::document::NativeOrderProof::proof_for_test(&origin, vec![vec![GlyphId(7)]])
                .expect("fixture proof");
        let invalid = block_fixture(1, raw_source("ab", &[(0, 1, &[7]), (1, 2, &[7])]), vec![0]);
        let otherwise_eligible = block_fixture(2, raw_source("c", &[(0, 1, &[7])]), vec![0]);
        assert_eq!(
            native_order_blocks_for(&[invalid, otherwise_eligible], &[None, None], Some(&proof)),
            vec![false, false],
            "a glyph claimed twice by an invalid block must veto the sharing block"
        );
    }

    #[test]
    fn wrapper_certifies_eligible_block_next_to_unrelated_invalid() {
        let origin: Document<Glyph> = Document::new(Vec::new());
        let proof =
            crate::document::NativeOrderProof::proof_for_test(&origin, vec![vec![GlyphId(7)]])
                .expect("fixture proof");
        let unrelated_invalid = block_fixture(1, raw_source("d", &[(0, 1, &[9])]), vec![0, 1]);
        let eligible = block_fixture(2, raw_source("c", &[(0, 1, &[7])]), vec![0]);
        assert_eq!(
            native_order_blocks_for(&[unrelated_invalid, eligible], &[None, None], Some(&proof)),
            vec![false, true],
            "an unrelated invalid block must not hide a sound certificate"
        );
    }

    #[test]
    fn refuses_empty_run_list() {
        let blocks = vec![glyphs(&[1, 2, 3])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(1), &[]),
            vec![false]
        );
    }

    #[test]
    fn certifies_single_fully_covered_block() {
        let blocks = vec![glyphs(&[1, 2, 3])];
        let runs = vec![glyphs(&[1, 2, 3])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(1), &runs),
            vec![true]
        );
    }

    #[test]
    fn certifies_ordered_multi_block_run() {
        let blocks = vec![glyphs(&[1, 2]), glyphs(&[3, 4])];
        let runs = vec![glyphs(&[1, 2, 3, 4])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(2), &runs),
            vec![true, true]
        );
    }

    #[test]
    fn certifies_covered_block_next_to_ineligible_member() {
        let blocks = vec![glyphs(&[1, 2]), glyphs(&[3, 4])];
        let runs = vec![glyphs(&[1, 2, 3, 4])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &[true, false], &runs),
            vec![true, false]
        );
    }

    #[test]
    fn rejects_partial_coverage() {
        let blocks = vec![glyphs(&[1, 2, 3])];
        let runs = vec![glyphs(&[1, 2])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(1), &runs),
            vec![false]
        );
    }

    #[test]
    fn rejects_duplicate_inside_run() {
        let blocks = vec![glyphs(&[1, 2])];
        let runs = vec![glyphs(&[1, 1, 2])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(1), &runs),
            vec![false]
        );
    }

    #[test]
    fn rejects_cross_run_duplicate() {
        let blocks = vec![glyphs(&[1, 2]), glyphs(&[2, 3])];
        let runs = vec![glyphs(&[1, 2]), glyphs(&[2, 3])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(2), &runs),
            vec![false, false]
        );
    }

    #[test]
    fn rejects_reordered_run() {
        let blocks = vec![glyphs(&[1, 2])];
        let runs = vec![glyphs(&[2, 1])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(1), &runs),
            vec![false]
        );
    }

    #[test]
    fn certifies_consecutive_subsequence_and_rejects_crossed_gap() {
        let blocks = vec![glyphs(&[1, 3]), glyphs(&[2])];
        let runs = vec![glyphs(&[1, 2, 3])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(2), &runs),
            vec![false, true]
        );
    }

    #[test]
    fn ignores_unrelated_run_glyphs() {
        let blocks = vec![glyphs(&[1, 2])];
        let runs = vec![glyphs(&[1, 2, 99])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(1), &runs),
            vec![true]
        );
    }

    #[test]
    fn rejects_ineligible_and_trusted_blocks() {
        let blocks = vec![glyphs(&[1, 2]), glyphs(&[3, 4])];
        let runs = vec![glyphs(&[1, 2, 3, 4])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &[false, false], &runs),
            vec![false, false]
        );
    }

    fn raw_source(text: &str, entries: &[(usize, usize, &[u64])]) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: entries
                .iter()
                .map(|(start, end, glyphs)| crate::normalize::SourceMapEntry {
                    output_range: crate::normalize::ScalarRange {
                        start: *start,
                        end: *end,
                    },
                    source: crate::normalize::TextSource {
                        atoms: glyphs
                            .iter()
                            .map(|glyph| TextSourceAtom::Glyph(GlyphId(*glyph)))
                            .collect::<Vec<_>>()
                            .into(),
                    },
                })
                .collect(),
            unmapped: Vec::new(),
        }
    }

    #[test]
    fn refuses_duplicate_raw_glyph_ownership() {
        let raw = raw_source("ab", &[(0, 1, &[7]), (1, 2, &[7])]);
        assert!(!raw_glyph_claims(&raw).expect("h27 fixture").1);
    }

    #[test]
    fn accepts_unique_raw_glyph_ownership() {
        let raw = raw_source("ab", &[(0, 1, &[7]), (1, 2, &[8])]);
        assert!(raw_glyph_claims(&raw).expect("h27 fixture").1);
    }

    #[test]
    fn refuses_multi_scalar_source_entry() {
        let raw = raw_source("ab", &[(0, 2, &[7])]);
        assert!(!raw_glyph_claims(&raw).expect("h27 fixture").1);
    }

    #[test]
    fn allows_synthetic_boundary_atoms() {
        let raw = MappedText {
            text: "a b".to_owned(),
            source_map: vec![
                crate::normalize::SourceMapEntry {
                    output_range: crate::normalize::ScalarRange { start: 0, end: 1 },
                    source: crate::normalize::TextSource {
                        atoms: vec![TextSourceAtom::Glyph(GlyphId(7))].into(),
                    },
                },
                crate::normalize::SourceMapEntry {
                    output_range: crate::normalize::ScalarRange { start: 1, end: 2 },
                    source: crate::normalize::TextSource {
                        atoms: vec![TextSourceAtom::SyntheticSpace {
                            preceding: GlyphId(7),
                            following: GlyphId(8),
                        }]
                        .into(),
                    },
                },
                crate::normalize::SourceMapEntry {
                    output_range: crate::normalize::ScalarRange { start: 2, end: 3 },
                    source: crate::normalize::TextSource {
                        atoms: vec![TextSourceAtom::Glyph(GlyphId(8))].into(),
                    },
                },
            ],
            unmapped: Vec::new(),
        };
        assert!(raw_glyph_claims(&raw).expect("h27 fixture").1);
    }

    #[test]
    fn rejects_ambiguous_shared_glyph() {
        let blocks = vec![glyphs(&[1, 2]), glyphs(&[2, 3])];
        let runs = vec![glyphs(&[1, 2])];
        assert_eq!(
            certified_native_order_blocks(&blocks, &eligible(2), &runs),
            vec![false, false]
        );
    }

    fn extract(path: &str) -> (Box<dyn crate::pdf::ParsedPdf>, ExtractionOutcome) {
        let bytes = std::fs::read(path).expect("h27 fixture");
        let pdf = LopdfParser
            .parse(bytes.into(), ParseLimits::default())
            .expect("h27 fixture");
        let outcome = ContentStreamGlyphExtractor
            .extract_outcome(&*pdf, ExtractionLimits::default())
            .expect("h27 fixture");
        (pdf, outcome)
    }

    #[test]
    fn h27_causal_replay() {
        let Ok(paths_file) = std::env::var("H26_PDFS") else {
            return;
        };
        let paths: Vec<String> = std::fs::read_to_string(paths_file)
            .expect("h27 fixture")
            .lines()
            .map(String::from)
            .collect();
        let (old_pdf, old_outcome) = extract(&paths[0]);
        let (new_pdf, new_outcome) = extract(&paths[1]);
        let (_second_old_pdf, second_old_outcome) = extract(&paths[0]);
        let default_budget = PipelineOptions::default().diff.max_assessment_work;
        let (flag_pdf, _) = extract(&paths[0]);
        let mut flag_budget = default_budget;
        let old_flags = crate::document::with_native_order_proof(
            Some(flag_pdf),
            old_outcome.document().clone(),
            &mut flag_budget,
            |document, proof, _budget| {
                assert!(
                    proof.is_some(),
                    "the NIST old side must certify its native order"
                );
                assert!(
                    proof.is_some_and(|proof| proof.binds(document)),
                    "the proof must bind its originating document"
                );
                assert!(
                    proof.is_some_and(|proof| !proof.binds(second_old_outcome.document())),
                    "same-id content from a second parse must never share the proof"
                );
                let prepared = prepare(
                    document,
                    PipelineOptions::default(),
                    DocumentSide::Old,
                    &mut PipelineDiagnostics::new(),
                    proof,
                )
                .expect("h27 fixture");
                let index = prepared
                    .blocks
                    .iter()
                    .position(|block| block.block.0 == 547)
                    .expect("target block exists");
                (
                    prepared.native_order_blocks[index],
                    prepared.trusted_run_intervals[index].map(|interval| interval.run_id.0),
                    prepared.uncertain_block_indices.contains(&index),
                    prepared
                        .native_order_blocks
                        .iter()
                        .filter(|flag| **flag)
                        .count(),
                )
            },
        );
        let (flag_new_pdf, _) = extract(&paths[1]);
        let mut flag_new_budget = default_budget;
        let new_flags = crate::document::with_native_order_proof(
            Some(flag_new_pdf),
            new_outcome.document().clone(),
            &mut flag_new_budget,
            |document, proof, _budget| {
                assert!(
                    proof.is_some(),
                    "the NIST new side must certify its native order"
                );
                let prepared = prepare(
                    document,
                    PipelineOptions::default(),
                    DocumentSide::New,
                    &mut PipelineDiagnostics::new(),
                    proof,
                )
                .expect("h27 fixture");
                let index = prepared
                    .blocks
                    .iter()
                    .position(|block| block.block.0 == 592)
                    .expect("target block exists");
                (
                    prepared.native_order_blocks[index],
                    prepared.trusted_run_intervals[index].map(|interval| interval.run_id.0),
                    prepared.uncertain_block_indices.contains(&index),
                    prepared
                        .native_order_blocks
                        .iter()
                        .filter(|flag| **flag)
                        .count(),
                )
            },
        );
        assert!(old_flags.0, "old block 547 must be certified");
        assert!(new_flags.0, "new block 592 must be certified");
        assert_eq!(
            old_flags.1, None,
            "certification must not synthesize a layout trusted run"
        );
        assert_eq!(
            new_flags.1, None,
            "certification must not synthesize a layout trusted run"
        );
        assert!(old_flags.2, "original uncertainty must be preserved");
        assert!(new_flags.2, "original uncertainty must be preserved");
        println!(
            "H27 certified old547={} new592={} old_certified_blocks={} new_certified_blocks={}",
            old_flags.0, new_flags.0, old_flags.3, new_flags.3
        );
        // Original-document preservation when the optional acquisition fails.
        let mut zero_budget = 0usize;
        let preserved = crate::document::with_native_order_proof(
            Some(extract(&paths[0]).0),
            old_outcome.document().clone(),
            &mut zero_budget,
            |document, proof, _budget| (document.items().len(), proof.is_none()),
        );
        assert_eq!(preserved.0, old_outcome.document().items().len());
        assert!(
            preserved.1,
            "an exhausted budget must leave the proof absent"
        );
        let (compared, work) = compare_extraction_outcomes_with_native_order_proofs(
            Some(old_pdf),
            old_outcome,
            Some(new_pdf),
            new_outcome,
            PipelineOptions::default(),
            &mut PipelineDiagnostics::new(),
        )
        .expect("h27 fixture");
        assert!(
            work.old_spent > 0 || work.new_spent > 0,
            "the production entry must acquire native order proofs"
        );
        println!(
            "H27 production work old={} new={}",
            work.old_spent, work.new_spent
        );
        println!(
            "H27 unresolved={} baseline=5598 changes={} baseline_changes=0 formatting={}",
            compared.comparison.unresolved_regions.len(),
            compared.comparison.changes.len(),
            compared.comparison.formatting_changes.len(),
        );
        assert!(
            compared.comparison.unresolved_regions.len() < 5598,
            "the certificate-backed consumer must reduce unresolved regions"
        );
        assert_eq!(
            compared.comparison.changes.len(),
            0,
            "changed remainders must not be proven without the exact diff"
        );
    }
}
