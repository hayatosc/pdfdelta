use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::{Change, ChangeKind, ChangedRegionProof, ProvenChangedRegion, TextSpan},
    layout::{BlockId, reconstruct_blocks, reconstruct_lines},
    model::{Document, Glyph, GlyphCropStatus, GlyphPathClipStatus, TextRenderMode},
    normalize::normalize_blocks,
    pdf::{LopdfParser, ParseLimits},
    pipeline::{PipelineOptions, compare_extraction_outcomes},
    report::summarize,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};

use crate::{
    BenchError, Result,
    cases::BenchmarkCase,
    mutation::{ExpectedCanonicalSpan, ExpectedManifest, ExpectedSemanticChange, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};

/// Controlled ASCII fixtures require exact canonical-range agreement (IoU 1.0).
pub const EXPECTED_SPAN_IOU_THRESHOLD: f64 = 1.0;

#[derive(Debug)]
struct CanonicalBlockPosition {
    text: String,
    global_start: usize,
    source_gap_before: usize,
}

#[derive(Debug)]
struct CanonicalDocumentIndex {
    blocks: Vec<CanonicalBlockPosition>,
    positions: HashMap<BlockId, usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProjectedSpan {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvaluationRecord {
    pub case_name: String,
    pub renderer: String,
    pub expected: ExpectedManifest,
    pub actual_changes: usize,
    pub actual_kinds: Vec<ChangeKind>,
    pub formatting_only_changes: usize,
    pub extraction_complete: bool,
    pub comparison_complete: bool,
    pub old_coverage: Option<f64>,
    pub new_coverage: Option<f64>,
    pub precision: GeneratedPrecisionMetrics,
    pub passed: bool,
    pub detail: String,
}

/// Precision and recall for one fully reviewed generated fixture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeneratedPrecisionMetrics {
    pub reported_events: usize,
    pub expected_events: usize,
    pub matched_events: usize,
    pub event_precision: f64,
    pub event_recall: f64,
    pub event_f1: f64,
    pub token_metrics: Option<GeneratedTokenMetrics>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeneratedTokenMetrics {
    pub changed_token_true_positives: usize,
    pub changed_token_false_positives: usize,
    pub changed_token_false_negatives: usize,
    pub changed_token_precision: f64,
    pub changed_token_recall: f64,
    pub changed_token_f1: f64,
    pub unchanged_tokens: usize,
    pub false_positive_changed_tokens_per_10k_unchanged_tokens: f64,
}

impl GeneratedPrecisionMetrics {
    /// Aggregates fully reviewed renderer records without averaging ratios.
    pub fn aggregate(records: impl IntoIterator<Item = Self>) -> Option<Self> {
        let mut totals = PrecisionCounts::default();
        let mut evaluated = 0_usize;
        for metrics in records {
            evaluated = evaluated.saturating_add(1);
            totals.reported_events = totals
                .reported_events
                .saturating_add(metrics.reported_events);
            totals.expected_events = totals
                .expected_events
                .saturating_add(metrics.expected_events);
            totals.matched_events = totals.matched_events.saturating_add(metrics.matched_events);
            let token_metrics = metrics.token_metrics?;
            totals.changed_token_true_positives = totals
                .changed_token_true_positives
                .saturating_add(token_metrics.changed_token_true_positives);
            totals.changed_token_false_positives = totals
                .changed_token_false_positives
                .saturating_add(token_metrics.changed_token_false_positives);
            totals.changed_token_false_negatives = totals
                .changed_token_false_negatives
                .saturating_add(token_metrics.changed_token_false_negatives);
            totals.unchanged_tokens = totals
                .unchanged_tokens
                .saturating_add(token_metrics.unchanged_tokens);
        }
        (evaluated > 0).then(|| totals.metrics(true))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PrecisionCounts {
    reported_events: usize,
    expected_events: usize,
    matched_events: usize,
    changed_token_true_positives: usize,
    changed_token_false_positives: usize,
    changed_token_false_negatives: usize,
    unchanged_tokens: usize,
}

impl PrecisionCounts {
    fn metrics(self, token_metrics_complete: bool) -> GeneratedPrecisionMetrics {
        let event_precision = score(self.matched_events, self.reported_events);
        let event_recall = score(self.matched_events, self.expected_events);
        let changed_token_precision = score(
            self.changed_token_true_positives,
            self.changed_token_true_positives
                .saturating_add(self.changed_token_false_positives),
        );
        let changed_token_recall = score(
            self.changed_token_true_positives,
            self.changed_token_true_positives
                .saturating_add(self.changed_token_false_negatives),
        );
        GeneratedPrecisionMetrics {
            reported_events: self.reported_events,
            expected_events: self.expected_events,
            matched_events: self.matched_events,
            event_precision,
            event_recall,
            event_f1: harmonic_mean(event_precision, event_recall),
            token_metrics: token_metrics_complete.then_some(GeneratedTokenMetrics {
                changed_token_true_positives: self.changed_token_true_positives,
                changed_token_false_positives: self.changed_token_false_positives,
                changed_token_false_negatives: self.changed_token_false_negatives,
                changed_token_precision,
                changed_token_recall,
                changed_token_f1: harmonic_mean(changed_token_precision, changed_token_recall),
                unchanged_tokens: self.unchanged_tokens,
                false_positive_changed_tokens_per_10k_unchanged_tokens: if self.unchanged_tokens
                    == 0
                {
                    0.0
                } else {
                    self.changed_token_false_positives as f64 * 10_000.0
                        / self.unchanged_tokens as f64
                },
            }),
        }
    }
}

pub fn evaluate_case(case: &BenchmarkCase, renderer: RendererKind) -> Result<EvaluationRecord> {
    evaluate(
        case.name(),
        case.plan().old(),
        case.plan().new_plan(),
        case.plan().expectation(),
        renderer,
    )
}

pub fn evaluate(
    case_name: &str,
    old_plan: &RenderPlan,
    new_plan: &RenderPlan,
    expected: &ExpectedManifest,
    renderer: RendererKind,
) -> Result<EvaluationRecord> {
    validate_case_name(case_name)?;
    let render_limits = RenderLimits::default();
    let old_pdf = renderer.render(old_plan, render_limits)?;
    let new_pdf = renderer.render(new_plan, render_limits)?;
    evaluate_rendered(
        case_name,
        old_plan,
        new_plan,
        expected,
        renderer.name(),
        Arc::from(old_pdf),
        Arc::from(new_pdf),
    )
}

/// Evaluates a canonical mutation against a PDF pair produced outside the
/// project renderers.
///
/// This keeps renderer execution out of the evaluator while applying the same
/// extraction, comparison, and canonical-span expectation checks used by
/// [`evaluate`].
///
/// # Errors
///
/// Returns [`BenchError::InvalidInput`] when the case or renderer identity is
/// invalid. Returns [`BenchError::Core`] when extraction, canonical projection,
/// comparison, or summary generation fails.
pub fn evaluate_rendered(
    case_name: &str,
    old_plan: &RenderPlan,
    new_plan: &RenderPlan,
    expected: &ExpectedManifest,
    renderer: &str,
    old_pdf: Arc<[u8]>,
    new_pdf: Arc<[u8]>,
) -> Result<EvaluationRecord> {
    validate_case_name(case_name)?;
    validate_renderer_name(renderer)?;
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let old = source
        .extract_outcome(old_pdf, parse_limits(), extraction_limits())
        .map_err(|error| core_error("old extraction", error))?;
    let new = source
        .extract_outcome(new_pdf, parse_limits(), extraction_limits())
        .map_err(|error| core_error("new extraction", error))?;
    let options = pipeline_options();
    let (old_index, new_index) = if old.is_complete() && new.is_complete() {
        (
            canonical_document_index(old_plan, old.document(), options)
                .map_err(|error| core_error("old expectation context", error))?,
            canonical_document_index(new_plan, new.document(), options)
                .map_err(|error| core_error("new expectation context", error))?,
        )
    } else {
        (
            CanonicalDocumentIndex::empty(),
            CanonicalDocumentIndex::empty(),
        )
    };
    let outcome = compare_extraction_outcomes(old, new, options)
        .map_err(|error| core_error("comparison", error))?;
    let summary = summarize(&outcome.comparison, &outcome.extraction)
        .map_err(|error| core_error("summary", error))?;

    let actual_kinds = outcome
        .comparison
        .changes
        .iter()
        .map(|change| change.kind)
        .collect::<Vec<_>>();
    let extraction_complete = outcome.extraction.old_complete && outcome.extraction.new_complete;
    let full_old_coverage = outcome.comparison.old_coverage.ratio == Some(1.0)
        && outcome.comparison.old_coverage.resolved_tokens
            == outcome.comparison.old_coverage.total_tokens;
    let full_new_coverage = outcome.comparison.new_coverage.ratio == Some(1.0)
        && outcome.comparison.new_coverage.resolved_tokens
            == outcome.comparison.new_coverage.total_tokens;
    let expected_change = matches_expectation(
        expected,
        &outcome.comparison.changes,
        &old_index,
        &new_index,
    );
    let expected_presence = matches_presence_expectation(
        expected,
        &outcome.comparison.proven_changed_regions,
        &old_index,
        &new_index,
    );
    let precision = generated_precision_metrics(
        expected,
        &outcome.comparison.changes,
        &old_index,
        &new_index,
        canonical_scalar_len(old_plan),
        canonical_scalar_len(new_plan),
    );
    let exact_resolution_required = expected.proven_regions().is_empty();
    let passed = extraction_complete
        && (!exact_resolution_required
            || (summary.comparison_complete && full_old_coverage && full_new_coverage))
        && expected_change
        && expected_presence;

    let mut failures = Vec::new();
    if !extraction_complete {
        failures.push("extraction is incomplete".to_owned());
    }
    if exact_resolution_required && !summary.comparison_complete {
        failures.push("comparison is incomplete".to_owned());
    }
    if exact_resolution_required && (!full_old_coverage || !full_new_coverage) {
        failures.push(format!(
            "coverage is old={} new={}",
            coverage_label(outcome.comparison.old_coverage.ratio),
            coverage_label(outcome.comparison.new_coverage.ratio)
        ));
    }
    if !expected_change {
        failures.push(format!(
            "expected {} at {:?}, observed {} at {:?}; document-global canonical span IoU must be at least {EXPECTED_SPAN_IOU_THRESHOLD:.1}",
            expected.label(),
            expected.changes(),
            actual_label(&actual_kinds),
            outcome.comparison.changes,
        ));
    }
    if !expected_presence {
        failures.push(format!(
            "expected {} at {:?}, observed {} proven changed regions",
            expected.label(),
            expected.proven_regions(),
            outcome.comparison.proven_changed_regions.len(),
        ));
    }

    Ok(EvaluationRecord {
        case_name: case_name.to_owned(),
        renderer: renderer.to_owned(),
        expected: expected.clone(),
        actual_changes: actual_kinds.len(),
        actual_kinds,
        formatting_only_changes: summary.formatting_only_changes,
        extraction_complete,
        comparison_complete: summary.comparison_complete,
        old_coverage: outcome.comparison.old_coverage.ratio,
        new_coverage: outcome.comparison.new_coverage.ratio,
        precision,
        passed,
        detail: if failures.is_empty() {
            if exact_resolution_required {
                "matched expectation with complete extraction and coverage".to_owned()
            } else {
                "matched proven changed-region expectation with complete extraction".to_owned()
            }
        } else {
            failures.join("; ")
        },
    })
}

fn generated_precision_metrics(
    expected: &ExpectedManifest,
    actual: &[Change],
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
    old_total_tokens: usize,
    new_total_tokens: usize,
) -> GeneratedPrecisionMetrics {
    let actual_old =
        projected_actual_spans(actual, old_index, |occurrence| occurrence.old_span.as_ref());
    let actual_new =
        projected_actual_spans(actual, new_index, |occurrence| occurrence.new_span.as_ref());
    let token_metrics_complete = actual_old.is_some() && actual_new.is_some();
    let actual_old = actual_old.unwrap_or_default();
    let actual_new = actual_new.unwrap_or_default();
    let expected_old = selected_expected_spans(expected.exact_changes(), &actual_old, |change| {
        change.old_spans()
    });
    let expected_new = selected_expected_spans(expected.exact_changes(), &actual_new, |change| {
        change.new_spans()
    });
    let actual_old = merge_spans(actual_old);
    let actual_new = merge_spans(actual_new);
    let expected_old = merge_spans(expected_old);
    let expected_new = merge_spans(expected_new);
    let changed_token_true_positives = intersection_len(&actual_old, &expected_old)
        .saturating_add(intersection_len(&actual_new, &expected_new));
    let actual_changed_tokens = spans_len(&actual_old).saturating_add(spans_len(&actual_new));
    let expected_changed_tokens = spans_len(&expected_old).saturating_add(spans_len(&expected_new));
    let expected_total_tokens = old_total_tokens.saturating_add(new_total_tokens);
    PrecisionCounts {
        reported_events: actual.len(),
        expected_events: expected.exact_changes().len(),
        matched_events: matched_event_count(expected, actual, old_index, new_index),
        changed_token_true_positives,
        changed_token_false_positives: actual_changed_tokens
            .saturating_sub(changed_token_true_positives),
        changed_token_false_negatives: expected_changed_tokens
            .saturating_sub(changed_token_true_positives),
        unchanged_tokens: expected_total_tokens.saturating_sub(expected_changed_tokens),
    }
    .metrics(token_metrics_complete)
}

fn projected_actual_spans(
    actual: &[Change],
    index: &CanonicalDocumentIndex,
    side: for<'a> fn(&'a pdfdelta_core::diff::ChangeOccurrence) -> Option<&'a TextSpan>,
) -> Option<Vec<ProjectedSpan>> {
    actual
        .iter()
        .flat_map(|change| &change.occurrences)
        .filter_map(side)
        .map(|span| global_span(span, index))
        .collect()
}

fn selected_expected_spans(
    expected: &[ExpectedSemanticChange],
    actual: &[ProjectedSpan],
    side: for<'a> fn(&'a ExpectedSemanticChange) -> &'a [ExpectedCanonicalSpan],
) -> Vec<ProjectedSpan> {
    expected
        .iter()
        .filter_map(|change| select_expected_span(side(change), actual))
        .collect()
}

fn select_expected_span(
    variants: &[ExpectedCanonicalSpan],
    actual: &[ProjectedSpan],
) -> Option<ProjectedSpan> {
    let mut best = None;
    for variant in variants {
        let iou = best_span_iou(*variant, actual);
        if best.is_none_or(|(_, best_iou)| iou > best_iou) {
            best = Some((*variant, iou));
        }
    }
    best.map(|(span, _)| ProjectedSpan {
        start: span.start(),
        end: span.end(),
    })
}

fn best_span_iou(expected: ExpectedCanonicalSpan, actual: &[ProjectedSpan]) -> f64 {
    actual
        .iter()
        .map(|actual| canonical_span_iou(expected, *actual))
        .fold(0.0, f64::max)
}

fn merge_spans(mut spans: Vec<ProjectedSpan>) -> Vec<ProjectedSpan> {
    spans.sort_unstable_by_key(|span| (span.start, span.end));
    let mut merged: Vec<ProjectedSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        if let Some(last) = merged.last_mut()
            && span.start <= last.end
        {
            last.end = last.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged
}

fn spans_len(spans: &[ProjectedSpan]) -> usize {
    spans.iter().fold(0_usize, |total, span| {
        total.saturating_add(span.end.saturating_sub(span.start))
    })
}

fn intersection_len(left: &[ProjectedSpan], right: &[ProjectedSpan]) -> usize {
    let (mut left_index, mut right_index, mut total) = (0, 0, 0_usize);
    while let (Some(left), Some(right)) = (left.get(left_index), right.get(right_index)) {
        total = total.saturating_add(
            left.end
                .min(right.end)
                .saturating_sub(left.start.max(right.start)),
        );
        if left.end <= right.end {
            left_index += 1;
        } else {
            right_index += 1;
        }
    }
    total
}

fn score(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn harmonic_mean(precision: f64, recall: f64) -> f64 {
    if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

fn canonical_scalar_len(plan: &RenderPlan) -> usize {
    plan.pages()
        .iter()
        .flatten()
        .map(|line| line.chars().count())
        .sum::<usize>()
        .saturating_add(
            plan.pages()
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                .saturating_sub(1),
        )
}

fn coverage_label(ratio: Option<f64>) -> String {
    ratio.map_or_else(|| "unknown".to_owned(), |ratio| ratio.to_string())
}

fn matches_expectation(
    expected: &ExpectedManifest,
    actual: &[Change],
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
) -> bool {
    expected.exact_changes().len() == actual.len()
        && matched_event_count(expected, actual, old_index, new_index) == actual.len()
}

fn matches_presence_expectation(
    expected: &ExpectedManifest,
    actual: &[ProvenChangedRegion],
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
) -> bool {
    let expected = expected.proven_regions();
    let edges = expected
        .iter()
        .map(|expected_region| {
            actual
                .iter()
                .enumerate()
                .filter_map(|(index, actual_region)| {
                    proven_region_matches(expected_region, actual_region, old_index, new_index)
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    expected.len() == actual.len()
        && maximum_cardinality_matching(&edges, actual.len()) == actual.len()
}

fn matched_event_count(
    expected: &ExpectedManifest,
    actual: &[Change],
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
) -> usize {
    let edges = expected
        .exact_changes()
        .iter()
        .map(|expected_change| {
            actual
                .iter()
                .enumerate()
                .filter_map(|(index, actual_change)| {
                    change_matches(expected_change, actual_change, old_index, new_index)
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    maximum_cardinality_matching(&edges, actual.len())
}

fn proven_region_matches(
    expected: &ExpectedSemanticChange,
    actual: &ProvenChangedRegion,
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
) -> bool {
    let proof_matches = match expected.kind() {
        ChangeKind::Replacement => actual.proof == ChangedRegionProof::ExactTokenMultisetMismatch,
        ChangeKind::Insertion | ChangeKind::Deletion => {
            actual.proof == ChangedRegionProof::OneSidedNonEmptyRange
        }
        ChangeKind::Move => false,
    };
    proof_matches
        && presence_side_span_matches(expected.old_spans(), actual.old_span.as_ref(), old_index)
        && presence_side_span_matches(expected.new_spans(), actual.new_span.as_ref(), new_index)
}

fn maximum_cardinality_matching(edges: &[Vec<usize>], right_count: usize) -> usize {
    let mut left_match: Vec<Option<usize>> = vec![None; edges.len()];
    let mut right_match: Vec<Option<usize>> = vec![None; right_count];
    let mut matched = 0;

    for root in 0..edges.len() {
        let mut queue = VecDeque::from([root]);
        let mut visited_left = vec![false; edges.len()];
        let mut visited_right = vec![false; right_count];
        let mut right_parent = vec![None; right_count];
        visited_left[root] = true;
        let mut free_right = None;

        while let Some(left) = queue.pop_front() {
            for &right in &edges[left] {
                if right >= right_count || visited_right[right] {
                    continue;
                }
                visited_right[right] = true;
                right_parent[right] = Some(left);
                if let Some(next_left) = right_match[right] {
                    if !visited_left[next_left] {
                        visited_left[next_left] = true;
                        queue.push_back(next_left);
                    }
                } else {
                    free_right = Some(right);
                    break;
                }
            }
            if free_right.is_some() {
                break;
            }
        }

        let Some(mut right) = free_right else {
            continue;
        };
        loop {
            let left = right_parent[right].expect("augmenting paths have a left parent");
            let previous_right = left_match[left];
            left_match[left] = Some(right);
            right_match[right] = Some(left);
            let Some(previous_right) = previous_right else {
                break;
            };
            right = previous_right;
        }
        matched += 1;
    }
    matched
}

fn change_matches(
    expected: &ExpectedSemanticChange,
    actual: &Change,
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
) -> bool {
    expected.kind() == actual.kind
        && actual.occurrences.len() == 1
        && actual.occurrences.iter().all(|occurrence| {
            side_span_matches(
                expected.old_spans(),
                occurrence.old_span.as_ref(),
                old_index,
            ) && side_span_matches(
                expected.new_spans(),
                occurrence.new_span.as_ref(),
                new_index,
            )
        })
}

fn side_span_matches(
    expected: &[ExpectedCanonicalSpan],
    actual: Option<&TextSpan>,
    index: &CanonicalDocumentIndex,
) -> bool {
    match (expected.is_empty(), actual) {
        (true, None) => true,
        (false, Some(actual)) => global_span(actual, index).is_some_and(|actual| {
            expected.iter().any(|expected| {
                canonical_span_iou(*expected, actual) >= EXPECTED_SPAN_IOU_THRESHOLD
            })
        }),
        (true, Some(_)) | (false, None) => false,
    }
}

fn presence_side_span_matches(
    expected: &[ExpectedCanonicalSpan],
    actual: Option<&TextSpan>,
    index: &CanonicalDocumentIndex,
) -> bool {
    match (expected.is_empty(), actual) {
        (true, None) => true,
        (false, Some(actual)) => global_span(actual, index).is_some_and(|actual| {
            expected
                .iter()
                .any(|expected| actual.start <= expected.start() && expected.end() <= actual.end)
        }),
        (true, Some(_)) | (false, None) => false,
    }
}

fn canonical_span_iou(expected: ExpectedCanonicalSpan, actual: ProjectedSpan) -> f64 {
    let intersection_start = expected.start().max(actual.start);
    let intersection_end = expected.end().min(actual.end);
    let intersection = intersection_end.saturating_sub(intersection_start);
    let expected_len = expected.end() - expected.start();
    let actual_len = actual.end.saturating_sub(actual.start);
    let union = expected_len
        .saturating_add(actual_len)
        .saturating_sub(intersection);
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn global_span(span: &TextSpan, index: &CanonicalDocumentIndex) -> Option<ProjectedSpan> {
    let group = index.ordered_group(&span.blocks)?;
    let separator = match (span.blocks.len(), span.separator) {
        (1, None) => None,
        (count, Some(separator)) if count > 1 => Some(separator),
        _ => return None,
    };
    project_group_span(span, &group, separator)
}

fn project_group_span(
    span: &TextSpan,
    group: &[&CanonicalBlockPosition],
    separator: Option<BlockSeparator>,
) -> Option<ProjectedSpan> {
    let mut group_scalars = group.first()?.text.chars().count();
    for (boundary, blocks) in group.windows(2).enumerate() {
        let separator_scalars = usize::from(
            separator?.at(boundary) == BlockSeparator::Space
                && needs_group_space(&blocks[0].text, &blocks[1].text),
        );
        if blocks[1].source_gap_before != separator_scalars {
            return None;
        }
        group_scalars = group_scalars
            .checked_add(separator_scalars)?
            .checked_add(blocks[1].text.chars().count())?;
    }
    if span.canonical_range.start >= span.canonical_range.end
        || span.canonical_range.end > group_scalars
    {
        return None;
    }
    let global_start = group
        .first()?
        .global_start
        .checked_add(span.canonical_range.start)?;
    let global_end = group
        .first()?
        .global_start
        .checked_add(span.canonical_range.end)?;
    Some(ProjectedSpan {
        start: global_start,
        end: global_end,
    })
}

fn needs_group_space(left: &str, right: &str) -> bool {
    !left.ends_with(' ') && !right.starts_with(' ')
}

fn canonical_document_index(
    plan: &RenderPlan,
    document: &Document<Glyph>,
    options: PipelineOptions,
) -> pdfdelta_core::Result<CanonicalDocumentIndex> {
    let document = Document::with_vector_lines(
        document
            .items()
            .iter()
            .filter(|glyph| is_comparison_visible(glyph))
            .cloned()
            .collect(),
        document.vector_lines().to_vec(),
    );
    let lines = reconstruct_lines(&document, options.line)?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)?;
    let normalized = normalize_blocks(&document, &lines, &blocks)?;
    let canonical_source = plan
        .pages()
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    map_canonical_blocks(
        &canonical_source,
        normalized
            .into_iter()
            .map(|block| (block.block, block.canonical.text)),
    )
}

fn map_canonical_blocks(
    canonical_source: &str,
    blocks: impl IntoIterator<Item = (BlockId, String)>,
) -> pdfdelta_core::Result<CanonicalDocumentIndex> {
    let blocks = blocks.into_iter();
    let mut indexed = Vec::with_capacity(blocks.size_hint().0);
    let mut positions = HashMap::with_capacity(blocks.size_hint().0);
    let mut source_cursor = 0_usize;

    for (position, (block_id, text)) in blocks.enumerate() {
        if text.is_empty() {
            return Err(pdfdelta_core::Error::Unresolved(format!(
                "normalized block {} has empty canonical text",
                block_id.0
            )));
        }
        if positions.insert(block_id, position).is_some() {
            return Err(pdfdelta_core::Error::Unresolved(format!(
                "duplicate normalized block id {}",
                block_id.0
            )));
        }
        let remaining = canonical_source.get(source_cursor..).ok_or_else(|| {
            pdfdelta_core::Error::Unresolved(format!(
                "normalized block {} starts beyond the render-plan canonical source",
                block_id.0
            ))
        })?;
        let source_gap_before = if remaining.starts_with(&text) {
            0
        } else if position > 0
            && remaining
                .strip_prefix(' ')
                .is_some_and(|remaining| remaining.starts_with(&text))
        {
            1
        } else {
            return Err(pdfdelta_core::Error::Unresolved(format!(
                "normalized block {} does not map at render-plan canonical scalar {}",
                block_id.0, source_cursor
            )));
        };
        let global_start = source_cursor.checked_add(source_gap_before).ok_or(
            pdfdelta_core::Error::LimitExceeded {
                resource: "benchmark canonical document scalars",
                limit: usize::MAX,
            },
        )?;
        let text_len = text.len();
        indexed.push(CanonicalBlockPosition {
            text,
            global_start,
            source_gap_before,
        });
        source_cursor =
            global_start
                .checked_add(text_len)
                .ok_or(pdfdelta_core::Error::LimitExceeded {
                    resource: "benchmark canonical document scalars",
                    limit: usize::MAX,
                })?;
    }
    if source_cursor != canonical_source.len() {
        return Err(pdfdelta_core::Error::Unresolved(format!(
            "normalized blocks consumed {source_cursor} of {} render-plan canonical scalars",
            canonical_source.len()
        )));
    }
    Ok(CanonicalDocumentIndex {
        blocks: indexed,
        positions,
    })
}

impl CanonicalDocumentIndex {
    fn empty() -> Self {
        Self {
            blocks: Vec::new(),
            positions: HashMap::new(),
        }
    }

    fn ordered_group(&self, ids: &[BlockId]) -> Option<Vec<&CanonicalBlockPosition>> {
        let positions = ids
            .iter()
            .map(|id| self.positions.get(id).copied())
            .collect::<Option<Vec<_>>>()?;
        if positions.is_empty()
            || positions
                .windows(2)
                .any(|positions| positions[0].checked_add(1) != Some(positions[1]))
        {
            return None;
        }
        positions
            .into_iter()
            .map(|position| self.blocks.get(position))
            .collect()
    }
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

fn actual_label(actual: &[ChangeKind]) -> String {
    match actual {
        [] => "none".to_owned(),
        [kind] => change_kind_name(*kind).to_owned(),
        _ => format!("{} changes", actual.len()),
    }
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}

fn validate_case_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || !name.is_ascii() {
        return Err(BenchError::InvalidInput(
            "evaluation case names must be nonblank ASCII".to_owned(),
        ));
    }
    Ok(())
}

fn validate_renderer_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.trim() != name
        || name.len() > 128
        || !name.is_ascii()
        || name.chars().any(char::is_control)
    {
        return Err(BenchError::InvalidInput(
            "evaluation renderer names must be 1-128 printable ASCII bytes without surrounding whitespace"
                .to_owned(),
        ));
    }
    Ok(())
}

fn core_error(stage: &'static str, error: pdfdelta_core::Error) -> BenchError {
    BenchError::Core {
        stage,
        source: error,
    }
}

fn parse_limits() -> ParseLimits {
    ParseLimits {
        max_input_bytes: RenderLimits::default().max_pdf_bytes,
        max_objects: 64,
        max_recursion_depth: 16,
        max_decoded_stream_bytes: 256 * 1024,
        max_total_object_stream_bytes: 256 * 1024,
        max_pages: RenderLimits::default().max_pages,
    }
}

fn extraction_limits() -> ExtractionLimits {
    ExtractionLimits {
        max_glyphs: 16 * 1024,
        max_form_depth: 4,
        max_nesting_depth: 16,
        max_operators: 4 * 1024,
        max_stream_invocations: 64,
        max_total_decoded_bytes: 256 * 1024,
        max_operand_stack: 256,
        max_array_elements: 4 * 1024,
        max_operand_nodes: 32 * 1024,
        max_fonts: 16,
        max_cmap_entries: 4 * 1024,
        max_cid_width_entries: 4 * 1024,
        max_string_bytes: 16 * 1024,
        max_vector_lines: 16 * 1024,
    }
}

fn pipeline_options() -> PipelineOptions {
    let mut options = PipelineOptions {
        max_ngram_token_elements: 64 * 1024,
        ..PipelineOptions::default()
    };
    options.alignment.max_candidate_visits = 16 * 1024;
    options.alignment.max_dp_cells = 16 * 1024;
    options.diff.max_tokens = 32 * 1024;
    options.diff.max_edit_distance = 4_000;
    options
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        diff::{ChangeOccurrence, Confidence, TokenRange},
        normalize::ScalarRange,
    };

    use super::*;

    #[test]
    fn renderer_identity_requires_bounded_printable_ascii() {
        for invalid in ["", " typst", "typst ", "typst\n0.15.1", "ティプスト"] {
            assert!(validate_renderer_name(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_renderer_name(&"x".repeat(129)).is_err());
        assert!(validate_renderer_name("typst-0.15.1").is_ok());
    }

    #[test]
    fn concatenate_projects_across_a_zero_scalar_source_boundary() {
        let index = map_canonical_blocks(
            "abcd",
            [(BlockId(1), "ab".to_owned()), (BlockId(2), "cd".to_owned())],
        )
        .expect("blocks map to canonical source");
        let span = text_span(BlockSeparator::Concatenate, 2, 3);

        let projected = global_span(&span, &index);

        assert_eq!(projected, Some(ProjectedSpan { start: 2, end: 3 }));
        assert_ne!(projected, Some(ProjectedSpan { start: 3, end: 4 }));
    }

    #[test]
    fn projection_rejects_a_separator_that_disagrees_with_the_source_boundary() {
        let index = map_canonical_blocks(
            "ab cd",
            [(BlockId(1), "ab".to_owned()), (BlockId(2), "cd".to_owned())],
        )
        .expect("blocks map to canonical source");

        assert_eq!(
            global_span(&text_span(BlockSeparator::Concatenate, 2, 3), &index),
            None
        );
        assert_eq!(
            global_span(&text_span(BlockSeparator::Space, 3, 4), &index),
            Some(ProjectedSpan { start: 3, end: 4 })
        );
    }

    #[test]
    fn canonical_index_rejects_unmapped_and_incompletely_consumed_sources() {
        let unmapped = map_canonical_blocks(
            "abcd",
            [(BlockId(1), "ab".to_owned()), (BlockId(2), "xy".to_owned())],
        );
        let incomplete = map_canonical_blocks("abcd", [(BlockId(1), "ab".to_owned())]);

        assert!(matches!(unmapped, Err(pdfdelta_core::Error::Unresolved(_))));
        assert!(matches!(
            incomplete,
            Err(pdfdelta_core::Error::Unresolved(_))
        ));
    }

    #[test]
    fn ordinary_expectation_rejects_a_matching_occurrence_with_a_surplus_occurrence() {
        let old_index =
            map_canonical_blocks("abcd", [(BlockId(1), "abcd".to_owned())]).expect("old index");
        let new_index =
            map_canonical_blocks("wxyz", [(BlockId(2), "wxyz".to_owned())]).expect("new index");
        let expected = ExpectedSemanticChange::new(
            ChangeKind::Replacement,
            vec![ExpectedCanonicalSpan::new(1, 2).expect("old expected span")],
            vec![ExpectedCanonicalSpan::new(1, 2).expect("new expected span")],
        )
        .expect("valid expectation");
        let mut actual = Change::single_occurrence(
            ChangeKind::Replacement,
            Some(single_block_span(1, 1, 2)),
            Some(single_block_span(2, 1, 2)),
            Confidence::High,
            Vec::new(),
        );
        actual.occurrences.push(ChangeOccurrence {
            old_span: Some(single_block_span(1, 2, 3)),
            new_span: Some(single_block_span(2, 2, 3)),
        });

        assert!(!change_matches(&expected, &actual, &old_index, &new_index));
    }

    #[test]
    fn proven_region_expectation_requires_one_containing_context_envelope() {
        let old_index =
            map_canonical_blocks("abcd", [(BlockId(1), "abcd".to_owned())]).expect("old index");
        let new_index =
            map_canonical_blocks("wxyz", [(BlockId(2), "wxyz".to_owned())]).expect("new index");
        let expected = ExpectedManifest::one_proven_region(
            ExpectedSemanticChange::new(
                ChangeKind::Replacement,
                vec![ExpectedCanonicalSpan::new(1, 3).expect("old expected span")],
                vec![ExpectedCanonicalSpan::new(1, 3).expect("new expected span")],
            )
            .expect("valid expectation"),
        );
        let actual = ProvenChangedRegion {
            old_span: Some(single_block_span(1, 0, 4)),
            new_span: Some(single_block_span(2, 0, 4)),
            proof: ChangedRegionProof::ExactTokenMultisetMismatch,
            confidence: Confidence::High,
        };

        assert!(matches_presence_expectation(
            &expected,
            std::slice::from_ref(&actual),
            &old_index,
            &new_index,
        ));
        let mut too_narrow = actual.clone();
        too_narrow.old_span = Some(single_block_span(1, 2, 4));
        assert!(!matches_presence_expectation(
            &expected,
            &[too_narrow],
            &old_index,
            &new_index,
        ));

        assert!(!matches_presence_expectation(
            &expected,
            &[actual.clone(), actual],
            &old_index,
            &new_index,
        ));
    }

    #[test]
    fn exact_expectations_reject_unexpected_presence_proofs() {
        let actual = ProvenChangedRegion {
            old_span: None,
            new_span: None,
            proof: ChangedRegionProof::ExactTokenMultisetMismatch,
            confidence: Confidence::High,
        };

        assert!(!matches_presence_expectation(
            &ExpectedManifest::none(),
            &[actual],
            &CanonicalDocumentIndex::empty(),
            &CanonicalDocumentIndex::empty(),
        ));
    }

    #[test]
    fn precision_selects_one_maximum_iou_paragraph_span_variant() {
        let old_index = map_canonical_blocks("abcdefghij", [(BlockId(1), "abcdefghij".to_owned())])
            .expect("old index");
        let expected = ExpectedManifest::one(
            ExpectedSemanticChange::new(
                ChangeKind::Deletion,
                vec![
                    ExpectedCanonicalSpan::new(1, 4).expect("separator variant"),
                    ExpectedCanonicalSpan::new(2, 4).expect("exact variant"),
                ],
                Vec::new(),
            )
            .expect("valid expectation"),
        );
        let actual = Change::single_occurrence(
            ChangeKind::Deletion,
            Some(single_block_span(1, 2, 4)),
            None,
            Confidence::High,
            Vec::new(),
        );

        let metrics = generated_precision_metrics(
            &expected,
            &[actual],
            &old_index,
            &CanonicalDocumentIndex::empty(),
            10,
            10,
        );

        assert_eq!(metrics.matched_events, 1);
        let tokens = metrics.token_metrics.expect("token metrics");
        assert_eq!(tokens.changed_token_true_positives, 2);
        assert_eq!(tokens.changed_token_false_positives, 0);
        assert_eq!(tokens.changed_token_false_negatives, 0);
        assert_eq!(tokens.unchanged_tokens, 18);

        assert_eq!(
            select_expected_span(
                &[
                    ExpectedCanonicalSpan::new(2, 4).expect("exact variant"),
                    ExpectedCanonicalSpan::new(2, 5).expect("separator variant"),
                ],
                &[],
            ),
            Some(ProjectedSpan { start: 2, end: 4 })
        );
    }

    #[test]
    fn precision_counts_unmatched_actual_and_expected_events() {
        let old_index = map_canonical_blocks("abcdefghij", [(BlockId(1), "abcdefghij".to_owned())])
            .expect("old index");
        let expected = ExpectedManifest::one(
            ExpectedSemanticChange::new(
                ChangeKind::Deletion,
                vec![ExpectedCanonicalSpan::new(2, 4).expect("expected span")],
                Vec::new(),
            )
            .expect("valid expectation"),
        );
        let actual = Change::single_occurrence(
            ChangeKind::Deletion,
            Some(single_block_span(1, 6, 8)),
            None,
            Confidence::High,
            Vec::new(),
        );

        let metrics = generated_precision_metrics(
            &expected,
            &[actual],
            &old_index,
            &CanonicalDocumentIndex::empty(),
            10,
            10,
        );

        assert_eq!(metrics.matched_events, 0);
        assert_eq!(metrics.event_precision, 0.0);
        assert_eq!(metrics.event_recall, 0.0);
        let tokens = metrics.token_metrics.expect("token metrics");
        assert_eq!(tokens.changed_token_true_positives, 0);
        assert_eq!(tokens.changed_token_false_positives, 2);
        assert_eq!(tokens.changed_token_false_negatives, 2);
    }

    #[test]
    fn layout_only_precision_explicitly_requires_zero_content_changes() {
        let clean = generated_precision_metrics(
            &ExpectedManifest::none(),
            &[],
            &CanonicalDocumentIndex::empty(),
            &CanonicalDocumentIndex::empty(),
            10,
            10,
        );

        assert_eq!(clean.reported_events, 0);
        assert_eq!(clean.expected_events, 0);
        assert_eq!(clean.event_precision, 1.0);
        assert_eq!(clean.event_recall, 1.0);
        let clean_tokens = clean.token_metrics.expect("clean token metrics");
        assert_eq!(clean_tokens.changed_token_false_positives, 0);
        assert_eq!(
            clean_tokens.false_positive_changed_tokens_per_10k_unchanged_tokens,
            0.0
        );

        let old_index = map_canonical_blocks("abcdefghij", [(BlockId(1), "abcdefghij".to_owned())])
            .expect("old index");
        let false_change = Change::single_occurrence(
            ChangeKind::Deletion,
            Some(single_block_span(1, 2, 4)),
            None,
            Confidence::Low,
            Vec::new(),
        );
        let noisy = generated_precision_metrics(
            &ExpectedManifest::none(),
            &[false_change],
            &old_index,
            &CanonicalDocumentIndex::empty(),
            10,
            10,
        );
        assert_eq!(noisy.event_precision, 0.0);
        let noisy_tokens = noisy.token_metrics.expect("noisy token metrics");
        assert_eq!(noisy_tokens.changed_token_false_positives, 2);
        assert_eq!(
            noisy_tokens.false_positive_changed_tokens_per_10k_unchanged_tokens,
            1_000.0
        );
    }

    #[test]
    fn token_metrics_are_unavailable_when_any_actual_span_cannot_be_projected() {
        assert_eq!(GeneratedPrecisionMetrics::aggregate([]), None);

        let actual = Change::single_occurrence(
            ChangeKind::Deletion,
            Some(single_block_span(99, 0, 1)),
            None,
            Confidence::Low,
            Vec::new(),
        );

        let metrics = generated_precision_metrics(
            &ExpectedManifest::none(),
            &[actual],
            &CanonicalDocumentIndex::empty(),
            &CanonicalDocumentIndex::empty(),
            10,
            10,
        );

        assert_eq!(metrics.reported_events, 1);
        assert_eq!(metrics.event_precision, 0.0);
        assert_eq!(metrics.token_metrics, None);
        assert_eq!(GeneratedPrecisionMetrics::aggregate([metrics]), None);
    }

    #[test]
    fn event_matching_uses_a_maximum_cardinality_assignment() {
        // A -> {1, 2}, B -> {1}; a first-fit assignment of A to 1 loses B.
        let edges = vec![vec![0, 1], vec![0]];

        assert_eq!(maximum_cardinality_matching(&edges, 2), 2);
    }

    fn text_span(separator: BlockSeparator, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(separator),
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn single_block_span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }
}
