use std::{collections::HashMap, sync::Arc};

use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::{Change, ChangeKind, TextSpan},
    layout::{BlockId, reconstruct_blocks, reconstruct_lines},
    model::{Document, Glyph, TextRenderMode},
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
    pub passed: bool,
    pub detail: String,
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
    let passed = extraction_complete
        && summary.comparison_complete
        && full_old_coverage
        && full_new_coverage
        && expected_change;

    let mut failures = Vec::new();
    if !extraction_complete {
        failures.push("extraction is incomplete".to_owned());
    }
    if !summary.comparison_complete {
        failures.push("comparison is incomplete".to_owned());
    }
    if !full_old_coverage || !full_new_coverage {
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
        passed,
        detail: if failures.is_empty() {
            "matched expectation with complete extraction and coverage".to_owned()
        } else {
            failures.join("; ")
        },
    })
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
    if expected.changes().len() != actual.len() {
        return false;
    }

    let mut matched = vec![false; actual.len()];
    expected.changes().iter().all(|expected_change| {
        let Some((index, _)) = actual.iter().enumerate().find(|(index, actual_change)| {
            !matched[*index] && change_matches(expected_change, actual_change, old_index, new_index)
        }) else {
            return false;
        };
        matched[index] = true;
        true
    })
}

fn change_matches(
    expected: &ExpectedSemanticChange,
    actual: &Change,
    old_index: &CanonicalDocumentIndex,
    new_index: &CanonicalDocumentIndex,
) -> bool {
    expected.kind() == actual.kind
        && side_span_matches(expected.old_spans(), actual.old_span.as_ref(), old_index)
        && side_span_matches(expected.new_spans(), actual.new_span.as_ref(), new_index)
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
    for blocks in group.windows(2) {
        let separator_scalars = match separator? {
            BlockSeparator::Concatenate => 0,
            BlockSeparator::Space => {
                usize::from(needs_group_space(&blocks[0].text, &blocks[1].text))
            }
        };
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
    let document = Document::new(
        document
            .items()
            .iter()
            .filter(|glyph| is_painting(glyph.render_mode))
            .cloned()
            .collect(),
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
    options.diff.max_edit_distance = 4 * 1024;
    options
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{diff::TokenRange, normalize::ScalarRange};

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

    fn text_span(separator: BlockSeparator, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(separator),
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }
}
