//! Scores generated fixtures with the flow engine.
//!
//! The evaluator renders the same canonical mutation matrix through the same
//! project renderers, extracts glyphs, flattens both documents with the flow
//! engine, and compares the verified edit script with the mutation's canonical
//! truth. Every check is exact: an invariance case must report no content
//! change, and a content change must cover exactly the mutated text.
//!
//! Token precision is estimated from the changed token text and the canonical
//! mutation spans. It is a token-level estimate, not a source-mask comparison.

use std::{collections::VecDeque, sync::Arc};

use pdfdelta_core::span::ChangeKind;
use pdfdelta_flow::{flow_diff, flow_tokens_pair};

use crate::{
    BenchError, Result,
    cases::BenchmarkCase,
    flow_verify::{
        ActualChanges, check_truth, extract_pair, span_text, summarize_changes,
        truth_for_expectation,
    },
    mutation::{ExpectedManifest, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};

/// Retained for callers that gate exact canonical-span agreement. The flow
/// evaluator checks exact changed text instead of span `IoU`, so this is not used
/// for acceptance.
pub const EXPECTED_SPAN_IOU_THRESHOLD: f64 = 1.0;

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

/// Precision and recall for one generated fixture.
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
    /// Aggregates fully reviewed records without averaging ratios.
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

/// Evaluates an externally rendered pair with the same flow checks.
///
/// # Errors
///
/// Returns [`BenchError::InvalidInput`] when the case or renderer identity is
/// invalid, extraction is incomplete, or the flow engine rejects an input.
/// Returns [`BenchError::Core`] for extraction failures.
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
    let (old_document, new_document) = extract_pair(&old_pdf, &new_pdf)?;
    let pair = flow_tokens_pair(&old_document, &new_document)
        .map_err(|error| BenchError::core("flow tokenization", error))?;
    let diff = flow_diff(&pair.old.tokens, &pair.new.tokens)
        .map_err(|error| BenchError::InvalidInput(format!("flow comparison failed: {error}")))?;
    let actual = summarize_changes(&diff.changes, &pair.old.tokens, &pair.new.tokens);
    let truth = truth_for_expectation(expected.changes(), old_plan, new_plan)?;
    let (truth_passed, truth_detail) = check_truth(&truth, &actual);

    let conservation_complete =
        pair.old.conservation.is_complete() && pair.new.conservation.is_complete();
    let actual_kinds = change_kinds(&actual);
    let expected_events = expected.changes().len();
    let reported_events = actual.content.len().saturating_add(actual.moves.len());
    let matched_events = if truth_passed { expected_events } else { 0 };
    let precision = precision_metrics(
        expected,
        old_plan,
        new_plan,
        &actual,
        reported_events,
        expected_events,
        matched_events,
        pair.old.tokens.len().saturating_add(pair.new.tokens.len()),
    );
    let passed = truth_passed && conservation_complete;
    let mut failures = Vec::new();
    if !truth_detail.is_empty() {
        failures.push(truth_detail);
    }
    if !conservation_complete {
        failures.push("flow token conservation is incomplete".to_owned());
    }
    Ok(EvaluationRecord {
        case_name: case_name.to_owned(),
        renderer: renderer.to_owned(),
        expected: expected.clone(),
        actual_changes: actual.content.len().saturating_add(actual.moves.len()),
        actual_kinds,
        formatting_only_changes: actual.layout_only,
        extraction_complete: true,
        comparison_complete: conservation_complete,
        old_coverage: conservation_complete.then_some(1.0),
        new_coverage: conservation_complete.then_some(1.0),
        precision,
        passed,
        detail: if failures.is_empty() {
            "flow script matched exact canonical truth with conserving token streams".to_owned()
        } else {
            failures.join("; ")
        },
    })
}

fn change_kinds(actual: &ActualChanges) -> Vec<ChangeKind> {
    let mut kinds = Vec::new();
    for (kind, _, _) in &actual.content {
        let kind = match kind.as_str() {
            "replace" => ChangeKind::Replacement,
            "insert" => ChangeKind::Insertion,
            "delete" => ChangeKind::Deletion,
            _ => continue,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if !actual.moves.is_empty() {
        kinds.push(ChangeKind::Move);
    }
    kinds
}

#[allow(clippy::too_many_arguments)]
fn precision_metrics(
    expected: &ExpectedManifest,
    old_plan: &RenderPlan,
    new_plan: &RenderPlan,
    actual: &ActualChanges,
    reported_events: usize,
    expected_events: usize,
    matched_events: usize,
    total_tokens: usize,
) -> GeneratedPrecisionMetrics {
    let expected_old = expected
        .changes()
        .iter()
        .flat_map(crate::mutation::ExpectedSemanticChange::old_spans)
        .map(|span| span_text(old_plan, span).chars().count())
        .sum::<usize>();
    let expected_new = expected
        .changes()
        .iter()
        .flat_map(crate::mutation::ExpectedSemanticChange::new_spans)
        .map(|span| span_text(new_plan, span).chars().count())
        .sum::<usize>();
    let actual_old = crate::flow_verify::normalize_text(&actual.old_text())
        .chars()
        .count();
    let actual_new = crate::flow_verify::normalize_text(&actual.new_text())
        .chars()
        .count();
    let true_positives = if matched_events > 0 {
        expected_old.saturating_add(expected_new)
    } else {
        0
    };
    let false_positives = actual_old
        .saturating_add(actual_new)
        .saturating_sub(true_positives);
    let false_negatives = expected_old
        .saturating_add(expected_new)
        .saturating_sub(true_positives);
    PrecisionCounts {
        reported_events,
        expected_events,
        matched_events,
        changed_token_true_positives: true_positives,
        changed_token_false_positives: false_positives,
        changed_token_false_negatives: false_negatives,
        unchanged_tokens: total_tokens.saturating_sub(actual_old.saturating_add(actual_new)),
    }
    .metrics(true)
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

fn validate_case_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['\t', '\r', '\n']) {
        return Err(BenchError::InvalidInput(
            "benchmark case name must be non-empty and single-line".into(),
        ));
    }
    Ok(())
}

fn validate_renderer_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['\t', '\r', '\n']) {
        return Err(BenchError::InvalidInput(
            "renderer name must be non-empty and single-line".into(),
        ));
    }
    Ok(())
}

pub(crate) fn maximum_cardinality_matching(edges: &[Vec<usize>], right_count: usize) -> usize {
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
