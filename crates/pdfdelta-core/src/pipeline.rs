use crate::{
    Error, Result,
    alignment::{
        AlignmentOptions, InvertedIndexCandidateGenerator, align_ordered_with_metrics,
        build_block_features, estimate_ngram_token_elements, validate_alignment_options,
        validate_ngram_size,
    },
    diff::{
        Comparison, DiffOptions, compare_aligned, enforce_diff_raw_token_budget,
        enforce_diff_token_budget, validate_diff_options,
    },
    layout::{
        BlockOptions, LineOptions, reconstruct_blocks, reconstruct_lines, validate_block_options,
        validate_line_options,
    },
    model::{Document, Glyph, TextRenderMode},
    normalize::{BlockText, normalize_blocks},
    report::{DocumentSide, ExtractionIssueRecord, ExtractionStatus},
    source::ExtractionOutcome,
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

#[derive(Clone, Debug, PartialEq)]
pub struct ComparisonOutcome {
    pub comparison: Comparison,
    pub extraction: ExtractionStatus,
    /// Normalized old-side blocks backing the comparison spans, for
    /// report rendering; empty when extraction gaps suppressed the diff.
    pub old_blocks: Vec<BlockText>,
    /// Normalized new-side blocks backing the comparison spans.
    pub new_blocks: Vec<BlockText>,
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
    /// `max_candidate_visits` for non-anchor old blocks; on a limit failure
    /// this is the attempted cumulative charge including the exceeding block.
    pub candidate_visits: Option<usize>,
    /// Checked sum of `CandidateGenerator::estimated_visits` over every
    /// non-anchor old block, independent of the budget: the full candidate
    /// work the alignment would need. `Some` when the full sum completed
    /// (including on a limit failure); `None` when an estimate error or
    /// overflow made the sum unavailable, or the candidate preflight was
    /// never reached (e.g. an earlier alignment error). Identity alignment
    /// is `Some(0)`.
    pub candidate_visits_required: Option<usize>,
    /// Exact-match posting visits of the required candidate sum; `Some`
    /// only when every non-anchor old block reported a breakdown and every
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
    if old_complete && new_complete {
        diagnostics.completed(
            PipelinePhase::CompletenessGate,
            None,
            PipelineMetrics::default(),
        );
        let (comparison, old_blocks, new_blocks) =
            compare_validated_glyph_documents(&old_document, &new_document, options, diagnostics)?;
        return Ok(ComparisonOutcome {
            comparison,
            extraction: ExtractionStatus::complete(),
            old_blocks,
            new_blocks,
        });
    }

    diagnostics.incomplete(PipelinePhase::CompletenessGate);

    let (old_tokens, new_tokens) =
        record_pre_layout_token_counts(&old_document, &new_document, options.diff, diagnostics)?;
    let issues = old_issues
        .into_iter()
        .map(|issue| ExtractionIssueRecord::from_issue(DocumentSide::Old, issue))
        .chain(
            new_issues
                .into_iter()
                .map(|issue| ExtractionIssueRecord::from_issue(DocumentSide::New, issue)),
        )
        .collect();

    // Incomplete extraction suppresses the diff to prevent false comparison output.
    Ok(ComparisonOutcome {
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
    })
}

pub fn compare_glyph_documents(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<Comparison> {
    let options = options.validate()?;
    compare_validated_glyph_documents(old, new, options, &mut PipelineDiagnostics::new())
        .map(|(comparison, _, _)| comparison)
}

fn compare_validated_glyph_documents(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
    diagnostics: &mut PipelineDiagnostics,
) -> Result<(Comparison, Vec<BlockText>, Vec<BlockText>)> {
    record_pre_layout_token_counts(old, new, options.diff, diagnostics)?;
    let old = prepare(old, options, DocumentSide::Old, diagnostics)?;
    let new = prepare(new, options, DocumentSide::New, diagnostics)?;
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
    let candidates = phase_result(
        diagnostics,
        PipelinePhase::CandidateIndex,
        Some(DocumentSide::New),
        InvertedIndexCandidateGenerator::new(&new_features),
    )?;
    diagnostics.completed(
        PipelinePhase::CandidateIndex,
        Some(DocumentSide::New),
        PipelineMetrics {
            indexed_features: Some(new_features.len()),
            ..PipelineMetrics::default()
        },
    );
    let attempt =
        align_ordered_with_metrics(&old_features, &new_features, &candidates, options.alignment);
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
    let comparison = phase_result(
        diagnostics,
        PipelinePhase::ExactDiff,
        None,
        compare_aligned(&old, &new, &alignment, options.diff),
    )?;
    diagnostics.completed(
        PipelinePhase::ExactDiff,
        None,
        PipelineMetrics {
            changes: Some(comparison.changes.len()),
            formatting_changes: Some(comparison.formatting_changes.len()),
            unresolved_regions: Some(comparison.unresolved_regions.len()),
            ..PipelineMetrics::default()
        },
    );
    Ok((comparison, old, new))
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
        .filter(|glyph| is_painting(glyph.render_mode))
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
) -> Result<Vec<BlockText>> {
    let document = Document::new(
        document
            .items()
            .iter()
            .filter(|glyph| is_painting(glyph.render_mode))
            .cloned()
            .collect(),
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
    let blocks = phase_result(
        diagnostics,
        PipelinePhase::BlockReconstruction,
        Some(side),
        reconstruct_blocks(&document, &lines, options.block),
    )?;
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
    diagnostics.completed(
        PipelinePhase::Normalization,
        Some(side),
        PipelineMetrics {
            blocks: Some(blocks.len()),
            normalized_blocks: Some(normalized.len()),
            ..PipelineMetrics::default()
        },
    );
    Ok(normalized)
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
