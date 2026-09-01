use std::{cmp::Ordering, collections::HashMap, ops::Range};

use pdfdelta_core::{
    Error,
    alignment::BlockSeparator,
    diff::{
        AtomicEdit, ChangeOrigin, RecoveredAtomicDiff, RecoveredAtomicOccurrence,
        RecoveryWatchNearScope, TextSpan,
    },
    model::{GlyphEvidence, Rect},
    normalize::{BlockText, ComparableToken},
    report::{SpanSourceEvidence, SpanSourceProjectionLimits, SpanSourceProjector},
};
use serde::Serialize;

use super::{RecoveryWatchNearScopeReport, RecoveryWatchRectReport};

const SAMPLE_LIMIT: usize = 256;
const SOURCE_SAMPLE_LIMIT: usize = 64;
const CONTEXT_TEXT_LIMIT: usize = 4_096;
const MAX_TRACES: usize = 16_384;
const MAX_OCCURRENCES: usize = 65_536;
const MAX_WORK_ITEMS: usize = 20_000_000;
const MAX_OUTPUT_ITEMS: usize = 1_000_000;
const MAX_SOURCE_EVIDENCE: usize = 2_000_000;
const MAX_TEXT_SCALARS: usize = 2_000_000;
const MAX_CONTEXT_TOKENS: usize = 5_100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalFragmentReviewStopReason {
    TraceLimit,
    OccurrenceLimit,
    WorkLimit,
    OutputLimit,
    SourceEvidenceLimit,
    TextLimit,
    AllocationFailure,
    InvalidTrace,
    SourceProjectionLimit,
    SourceProjectionFailed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LocalFragmentReviewBundleReport {
    Complete {
        total_traces: usize,
        total_occurrences: usize,
        sample_limit: usize,
        truncated: bool,
        samples: Vec<LocalFragmentReviewTraceReport>,
    },
    Unavailable {
        reason: LocalFragmentReviewStopReason,
    },
}

impl Default for LocalFragmentReviewBundleReport {
    fn default() -> Self {
        Self::Complete {
            total_traces: 0,
            total_occurrences: 0,
            sample_limit: SAMPLE_LIMIT,
            truncated: false,
            samples: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalFragmentReviewOriginReport {
    LocalFragment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalFragmentBlockSeparatorReport {
    Concatenate,
    Space,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalFragmentTextSpanReport {
    pub blocks: Vec<u64>,
    pub separator: Option<LocalFragmentBlockSeparatorReport>,
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalFragmentContextTextReport {
    pub text: String,
    pub total_scalars: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LocalFragmentAtomicEditReport {
    pub old_start: usize,
    pub old_end: usize,
    pub new_start: usize,
    pub new_end: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LocalFragmentChangedOccurrenceReport {
    pub old_span: Option<LocalFragmentTextSpanReport>,
    pub new_span: Option<LocalFragmentTextSpanReport>,
    pub old_text: Option<LocalFragmentContextTextReport>,
    pub new_text: Option<LocalFragmentContextTextReport>,
    pub old_source: Option<LocalFragmentSourceSideReport>,
    pub new_source: Option<LocalFragmentSourceSideReport>,
    pub edit_start: usize,
    pub edit_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LocalFragmentRelationSideReport {
    pub best_score: u16,
    pub second_score: u16,
    pub margin: u16,
    pub scope: Option<RecoveryWatchNearScopeReport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LocalFragmentObjectRefReport {
    pub object_number: u32,
    pub generation: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocalFragmentSourceEvidenceReport {
    Glyph {
        glyph_id: u64,
        page: u32,
        bbox: RecoveryWatchRectReport,
        content_stream: LocalFragmentObjectRefReport,
        operator_index: u32,
    },
    SyntheticSpace {
        preceding_glyph_id: u64,
        following_glyph_id: u64,
    },
    LineBreak {
        preceding_glyph_id: u64,
        following_glyph_id: u64,
    },
    BlockSeparatorSpace,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LocalFragmentSourceSideReport {
    pub total: usize,
    pub truncated: bool,
    pub samples: Vec<LocalFragmentSourceEvidenceReport>,
    pub pages: Vec<LocalFragmentSourcePageReport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LocalFragmentSourcePageReport {
    pub page: u32,
    pub bbox: RecoveryWatchRectReport,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LocalFragmentReviewTraceReport {
    pub origin: LocalFragmentReviewOriginReport,
    pub old_alignment_span: usize,
    pub new_alignment_span: usize,
    pub old_relation: LocalFragmentRelationSideReport,
    pub new_relation: LocalFragmentRelationSideReport,
    pub old_context: LocalFragmentTextSpanReport,
    pub new_context: LocalFragmentTextSpanReport,
    pub old_context_text: LocalFragmentContextTextReport,
    pub new_context_text: LocalFragmentContextTextReport,
    pub old_source: LocalFragmentSourceSideReport,
    pub new_source: LocalFragmentSourceSideReport,
    pub edits: Vec<LocalFragmentAtomicEditReport>,
    pub changed_occurrences: Vec<LocalFragmentChangedOccurrenceReport>,
}

pub(super) fn build_local_fragment_review_bundle(
    traces: &[RecoveredAtomicDiff],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
) -> LocalFragmentReviewBundleReport {
    build_local_fragment_review_bundle_with_limit(
        traces,
        old_blocks,
        new_blocks,
        old_glyph_evidence,
        new_glyph_evidence,
        SAMPLE_LIMIT,
    )
    .unwrap_or_else(|reason| LocalFragmentReviewBundleReport::Unavailable { reason })
}

pub(super) fn validate_local_fragment_review_bundle_contract(
    bundle: &LocalFragmentReviewBundleReport,
    proposal_complete: Option<bool>,
    proposals_committed: usize,
    traces: &[RecoveredAtomicDiff],
) -> Result<(), String> {
    let observed_traces = traces
        .iter()
        .filter(|trace| trace.origin == ChangeOrigin::LocalFragment)
        .count();
    if observed_traces != proposals_committed {
        return Err("local-fragment trace count disagrees with committed proposals".to_owned());
    }
    if proposal_complete != Some(true) && observed_traces != 0 {
        return Err("stopped local-fragment proposals expose review traces".to_owned());
    }
    let LocalFragmentReviewBundleReport::Complete {
        total_traces,
        total_occurrences,
        sample_limit,
        truncated,
        samples,
    } = bundle
    else {
        return Ok(());
    };
    if *sample_limit != SAMPLE_LIMIT
        || samples.len() != (*total_traces).min(*sample_limit)
        || *truncated != (*total_traces > *sample_limit)
    {
        return Err("local-fragment review sample accounting is inconsistent".to_owned());
    }
    let retained_occurrences = samples.iter().try_fold(0usize, |total, sample| {
        total.checked_add(sample.changed_occurrences.len())
    });
    if retained_occurrences.is_none_or(|count| {
        count > *total_occurrences || !*truncated && count != *total_occurrences
    }) {
        return Err("local-fragment review occurrence accounting is inconsistent".to_owned());
    }
    if *total_traces != observed_traces {
        return Err("local-fragment review traces disagree with committed proposals".to_owned());
    }
    if proposal_complete != Some(true)
        && (*total_traces != 0 || *total_occurrences != 0 || !samples.is_empty())
    {
        return Err("stopped local-fragment proposals expose review traces".to_owned());
    }
    Ok(())
}

fn build_local_fragment_review_bundle_with_limit(
    traces: &[RecoveredAtomicDiff],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    sample_limit: usize,
) -> Result<LocalFragmentReviewBundleReport, LocalFragmentReviewStopReason> {
    if traces.len() > MAX_WORK_ITEMS {
        return Err(LocalFragmentReviewStopReason::WorkLimit);
    }
    let mut local = Vec::new();
    local
        .try_reserve(traces.len().min(MAX_TRACES))
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    for trace in traces {
        if trace.origin == ChangeOrigin::LocalFragment {
            if local.len() == MAX_TRACES {
                return Err(LocalFragmentReviewStopReason::TraceLimit);
            }
            local.push(trace);
        }
    }
    local.sort_unstable_by(|left, right| compare_traces(left, right));

    let total_occurrences = local.iter().try_fold(0usize, |total, trace| {
        total
            .checked_add(trace.changed_occurrences.len())
            .filter(|count| *count <= MAX_OCCURRENCES)
            .ok_or(LocalFragmentReviewStopReason::OccurrenceLimit)
    })?;
    if local.is_empty() {
        return Ok(LocalFragmentReviewBundleReport::Complete {
            total_traces: 0,
            total_occurrences: 0,
            sample_limit,
            truncated: false,
            samples: Vec::new(),
        });
    }

    let mut budget = ReviewBudget::default();
    budget.charge_work(traces.len())?;
    let document_work = old_blocks
        .len()
        .checked_add(new_blocks.len())
        .and_then(|total| total.checked_add(old_glyph_evidence.len()))
        .and_then(|total| total.checked_add(new_glyph_evidence.len()))
        .ok_or(LocalFragmentReviewStopReason::WorkLimit)?;
    budget.charge_work(document_work)?;
    let projection_limits = SpanSourceProjectionLimits {
        max_comparable_tokens: MAX_CONTEXT_TOKENS,
        max_evidence_items: MAX_SOURCE_EVIDENCE,
    };
    let old_projector = SpanSourceProjector::new(old_blocks, old_glyph_evidence, projection_limits)
        .map_err(map_projection_error)?;
    let new_projector = SpanSourceProjector::new(new_blocks, new_glyph_evidence, projection_limits)
        .map_err(map_projection_error)?;
    let old_block_map = build_block_map(old_blocks)?;
    let new_block_map = build_block_map(new_blocks)?;

    let retained = local.len().min(sample_limit);
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(retained)
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    for (index, trace) in local.iter().enumerate() {
        budget.charge_work(1)?;
        if index < sample_limit {
            let report = build_trace_report(
                trace,
                [&old_block_map, &new_block_map],
                [&old_projector, &new_projector],
                &mut budget,
            )?;
            budget.charge_output(1)?;
            samples.push(report);
        } else {
            validate_trace(
                trace,
                [&old_block_map, &new_block_map],
                [&old_projector, &new_projector],
                &mut budget,
            )?;
        }
    }
    Ok(LocalFragmentReviewBundleReport::Complete {
        total_traces: local.len(),
        total_occurrences,
        sample_limit,
        truncated: local.len() > sample_limit,
        samples,
    })
}

fn validate_trace(
    trace: &RecoveredAtomicDiff,
    block_maps: [&HashMap<u64, &BlockText>; 2],
    projectors: [&SpanSourceProjector<'_>; 2],
    budget: &mut ReviewBudget,
) -> Result<(), LocalFragmentReviewStopReason> {
    let old_len = validate_context(&trace.old_context, block_maps[0], budget)?;
    let new_len = validate_context(&trace.new_context, block_maps[1], budget)?;
    validate_edits(&trace.edits, old_len, new_len)?;
    validate_occurrences(trace, old_len, new_len, block_maps, budget)?;
    validate_relation(trace.old_best_score, trace.old_second_score)?;
    validate_relation(trace.new_best_score, trace.new_second_score)?;
    for (span, projector) in [
        (&trace.old_context, projectors[0]),
        (&trace.new_context, projectors[1]),
    ] {
        let evidence = projector.project(span).map_err(map_projection_error)?;
        budget.charge_source(evidence.len())?;
        budget.charge_work(evidence.len())?;
    }
    validate_occurrence_projections(trace, projectors, budget)
}

fn build_trace_report(
    trace: &RecoveredAtomicDiff,
    block_maps: [&HashMap<u64, &BlockText>; 2],
    projectors: [&SpanSourceProjector<'_>; 2],
    budget: &mut ReviewBudget,
) -> Result<LocalFragmentReviewTraceReport, LocalFragmentReviewStopReason> {
    let (old_len, old_context_text) =
        materialize_context(&trace.old_context, block_maps[0], budget)?;
    let (new_len, new_context_text) =
        materialize_context(&trace.new_context, block_maps[1], budget)?;
    validate_edits(&trace.edits, old_len, new_len)?;
    validate_occurrences(trace, old_len, new_len, block_maps, budget)?;
    validate_relation(trace.old_best_score, trace.old_second_score)?;
    validate_relation(trace.new_best_score, trace.new_second_score)?;
    let old_evidence = projectors[0]
        .project(&trace.old_context)
        .map_err(map_projection_error)?;
    let new_evidence = projectors[1]
        .project(&trace.new_context)
        .map_err(map_projection_error)?;
    let old_source = source_report(old_evidence, budget)?;
    let new_source = source_report(new_evidence, budget)?;

    let mut edits = Vec::new();
    edits
        .try_reserve_exact(trace.edits.len())
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    budget.charge_output(trace.edits.len())?;
    edits.extend(trace.edits.iter().map(edit_report));

    let mut changed_occurrences = Vec::new();
    changed_occurrences
        .try_reserve_exact(trace.changed_occurrences.len())
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    budget.charge_output(trace.changed_occurrences.len())?;
    for occurrence in &trace.changed_occurrences {
        changed_occurrences.push(occurrence_report(
            occurrence, block_maps, projectors, budget,
        )?);
    }

    Ok(LocalFragmentReviewTraceReport {
        origin: LocalFragmentReviewOriginReport::LocalFragment,
        old_alignment_span: trace.old_alignment_span_index,
        new_alignment_span: trace.new_alignment_span_index,
        old_relation: relation_report(
            trace.old_best_score,
            trace.old_second_score,
            trace.old_best_scope,
        )?,
        new_relation: relation_report(
            trace.new_best_score,
            trace.new_second_score,
            trace.new_best_scope,
        )?,
        old_context: span_report(&trace.old_context, budget)?,
        new_context: span_report(&trace.new_context, budget)?,
        old_context_text,
        new_context_text,
        old_source,
        new_source,
        edits,
        changed_occurrences,
    })
}

fn build_block_map(
    blocks: &[BlockText],
) -> Result<HashMap<u64, &BlockText>, LocalFragmentReviewStopReason> {
    let mut map = HashMap::new();
    map.try_reserve(blocks.len())
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    for block in blocks {
        if map.insert(block.block.0, block).is_some() {
            return Err(LocalFragmentReviewStopReason::InvalidTrace);
        }
    }
    Ok(map)
}

fn validate_context(
    span: &TextSpan,
    blocks: &HashMap<u64, &BlockText>,
    budget: &mut ReviewBudget,
) -> Result<usize, LocalFragmentReviewStopReason> {
    let tokens = collect_context_tokens(span, blocks, budget)?;
    validate_context_ranges(span, &tokens)
}

fn materialize_context(
    span: &TextSpan,
    blocks: &HashMap<u64, &BlockText>,
    budget: &mut ReviewBudget,
) -> Result<(usize, LocalFragmentContextTextReport), LocalFragmentReviewStopReason> {
    let tokens = collect_context_tokens(span, blocks, budget)?;
    let comparable_len = validate_context_ranges(span, &tokens)?;
    let total_scalars = span
        .canonical_range
        .end
        .checked_sub(span.canonical_range.start)
        .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    budget.charge_text(total_scalars)?;
    let retained = total_scalars.min(CONTEXT_TEXT_LIMIT);
    let bytes = retained
        .checked_mul(4)
        .ok_or(LocalFragmentReviewStopReason::OutputLimit)?;
    let mut text = String::new();
    text.try_reserve(bytes)
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    text.extend(
        tokens
            .iter()
            .filter_map(ComparableToken::as_scalar)
            .skip(span.canonical_range.start)
            .take(retained),
    );
    Ok((
        comparable_len,
        LocalFragmentContextTextReport {
            text,
            total_scalars,
            truncated: total_scalars > retained,
        },
    ))
}

fn collect_context_tokens(
    span: &TextSpan,
    blocks: &HashMap<u64, &BlockText>,
    budget: &mut ReviewBudget,
) -> Result<Vec<ComparableToken>, LocalFragmentReviewStopReason> {
    if span.blocks.is_empty()
        || span.canonical_range.start > span.canonical_range.end
        || span.comparable_range.start > span.comparable_range.end
        || (span.blocks.len() > 1) != span.separator.is_some()
    {
        return Err(LocalFragmentReviewStopReason::InvalidTrace);
    }
    let mut tokens = Vec::new();
    for (index, block_id) in span.blocks.iter().enumerate() {
        let block = blocks
            .get(&block_id.0)
            .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
        let next = block
            .canonical
            .comparable_tokens()
            .map_err(|_| LocalFragmentReviewStopReason::InvalidTrace)?;
        budget.charge_work(next.len())?;
        let separator = usize::from(index > 0);
        let capacity = next
            .len()
            .checked_add(separator)
            .ok_or(LocalFragmentReviewStopReason::WorkLimit)?;
        tokens
            .try_reserve(capacity)
            .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
        if index == 0 {
            tokens.extend(next);
        } else {
            append_with_separator(
                &mut tokens,
                span.separator == Some(BlockSeparator::Space),
                &next,
            );
        }
        if tokens.len() > MAX_CONTEXT_TOKENS {
            return Err(LocalFragmentReviewStopReason::WorkLimit);
        }
    }
    Ok(tokens)
}

fn validate_context_ranges(
    span: &TextSpan,
    tokens: &[ComparableToken],
) -> Result<usize, LocalFragmentReviewStopReason> {
    let scalar_len = tokens.iter().filter(|token| token.is_scalar()).count();
    if span.comparable_range.end > tokens.len() || span.canonical_range.end > scalar_len {
        return Err(LocalFragmentReviewStopReason::InvalidTrace);
    }
    Ok(span.comparable_range.end - span.comparable_range.start)
}

fn validate_edits(
    edits: &[AtomicEdit],
    old_len: usize,
    new_len: usize,
) -> Result<(), LocalFragmentReviewStopReason> {
    let mut previous_old = 0;
    let mut previous_new = 0;
    for edit in edits {
        if !valid_range(&edit.old, old_len)
            || !valid_range(&edit.new, new_len)
            || edit.old.is_empty() == edit.new.is_empty()
            || edit.old.start < previous_old
            || edit.new.start < previous_new
        {
            return Err(LocalFragmentReviewStopReason::InvalidTrace);
        }
        previous_old = edit.old.end;
        previous_new = edit.new.end;
    }
    Ok(())
}

fn validate_occurrences(
    trace: &RecoveredAtomicDiff,
    old_len: usize,
    new_len: usize,
    block_maps: [&HashMap<u64, &BlockText>; 2],
    budget: &mut ReviewBudget,
) -> Result<(), LocalFragmentReviewStopReason> {
    let mut previous_edit_end = 0;
    for changed in &trace.changed_occurrences {
        if changed.edit_range.start >= changed.edit_range.end
            || changed.edit_range.end > trace.edits.len()
            || changed.edit_range.start < previous_edit_end
        {
            return Err(LocalFragmentReviewStopReason::InvalidTrace);
        }
        let old_span = changed
            .occurrence
            .old_span
            .as_ref()
            .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
        let new_span = changed
            .occurrence
            .new_span
            .as_ref()
            .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
        let old_relative = project_child_span_into_context(
            old_span,
            &trace.old_context,
            old_len,
            block_maps[0],
            budget,
        )?;
        let new_relative = project_child_span_into_context(
            new_span,
            &trace.new_context,
            new_len,
            block_maps[1],
            budget,
        )?;
        if trace.edits[changed.edit_range.clone()].iter().any(|edit| {
            !range_is_covered(&edit.old, &old_relative)
                || !range_is_covered(&edit.new, &new_relative)
        }) {
            return Err(LocalFragmentReviewStopReason::InvalidTrace);
        }
        previous_edit_end = changed.edit_range.end;
    }
    if previous_edit_end != trace.edits.len() {
        return Err(LocalFragmentReviewStopReason::InvalidTrace);
    }
    Ok(())
}

fn validate_occurrence_projections(
    trace: &RecoveredAtomicDiff,
    projectors: [&SpanSourceProjector<'_>; 2],
    budget: &mut ReviewBudget,
) -> Result<(), LocalFragmentReviewStopReason> {
    for changed in &trace.changed_occurrences {
        for (span, projector) in [
            (changed.occurrence.old_span.as_ref(), projectors[0]),
            (changed.occurrence.new_span.as_ref(), projectors[1]),
        ] {
            let span = span.ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
            let evidence = projector.project(span).map_err(map_projection_error)?;
            budget.charge_source(evidence.len())?;
            budget.charge_work(evidence.len())?;
        }
    }
    Ok(())
}

fn project_child_span_into_context(
    child: &TextSpan,
    context: &TextSpan,
    context_len: usize,
    blocks: &HashMap<u64, &BlockText>,
    budget: &mut ReviewBudget,
) -> Result<Range<usize>, LocalFragmentReviewStopReason> {
    if child.blocks.is_empty()
        || child.blocks.len() > context.blocks.len()
        || (child.blocks.len() > 1) != child.separator.is_some()
        || (child.blocks.len() > 1 && child.separator != context.separator)
    {
        return Err(LocalFragmentReviewStopReason::InvalidTrace);
    }
    let candidate_windows = context
        .blocks
        .len()
        .checked_sub(child.blocks.len())
        .and_then(|count| count.checked_add(1))
        .ok_or(LocalFragmentReviewStopReason::WorkLimit)?;
    let comparison_work = candidate_windows
        .checked_mul(child.blocks.len())
        .ok_or(LocalFragmentReviewStopReason::WorkLimit)?;
    budget.charge_work(comparison_work)?;
    let mut child_block_start = None;
    for start in 0..=context.blocks.len() - child.blocks.len() {
        if context.blocks[start..start + child.blocks.len()] == child.blocks
            && child_block_start.replace(start).is_some()
        {
            return Err(LocalFragmentReviewStopReason::InvalidTrace);
        }
    }
    let child_block_start = child_block_start.ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    let child_tokens = collect_context_tokens(child, blocks, budget)?;
    validate_context_ranges(child, &child_tokens)?;
    let child_group_offset = context_block_offset(context, child_block_start, blocks, budget)?;
    let absolute_start = child_group_offset
        .checked_add(child.comparable_range.start)
        .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    let absolute_end = child_group_offset
        .checked_add(child.comparable_range.end)
        .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    let relative = absolute_start
        .checked_sub(context.comparable_range.start)
        .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?
        ..absolute_end
            .checked_sub(context.comparable_range.start)
            .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    if !valid_range(&relative, context_len) {
        return Err(LocalFragmentReviewStopReason::InvalidTrace);
    }
    Ok(relative)
}

fn context_block_offset(
    context: &TextSpan,
    target: usize,
    blocks: &HashMap<u64, &BlockText>,
    budget: &mut ReviewBudget,
) -> Result<usize, LocalFragmentReviewStopReason> {
    let mut offset = 0usize;
    let mut previous_is_space = None;
    for (index, block_id) in context.blocks.iter().enumerate().take(target + 1) {
        let block = blocks
            .get(&block_id.0)
            .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
        let tokens = block
            .canonical
            .comparable_tokens()
            .map_err(|_| LocalFragmentReviewStopReason::InvalidTrace)?;
        budget.charge_work(tokens.len())?;
        if index > 0
            && context.separator == Some(BlockSeparator::Space)
            && previous_is_space != Some(true)
            && !tokens.first().is_some_and(is_space_token)
        {
            offset = offset
                .checked_add(1)
                .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
        }
        if index == target {
            return Ok(offset);
        }
        offset = offset
            .checked_add(tokens.len())
            .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
        if let Some(last) = tokens.last() {
            previous_is_space = Some(is_space_token(last));
        }
    }
    Err(LocalFragmentReviewStopReason::InvalidTrace)
}

fn range_is_covered(range: &Range<usize>, hunk: &Range<usize>) -> bool {
    if range.is_empty() {
        hunk.start <= range.start && range.start <= hunk.end
    } else {
        hunk.start <= range.start && range.end <= hunk.end
    }
}

fn valid_range(range: &Range<usize>, limit: usize) -> bool {
    range.start <= range.end && range.end <= limit
}

fn source_report(
    evidence: Vec<SpanSourceEvidence>,
    budget: &mut ReviewBudget,
) -> Result<LocalFragmentSourceSideReport, LocalFragmentReviewStopReason> {
    budget.charge_source(evidence.len())?;
    budget.charge_work(evidence.len())?;
    let retained = evidence.len().min(SOURCE_SAMPLE_LIMIT);
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(retained)
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    budget.charge_output(retained)?;
    samples.extend(evidence.iter().take(retained).copied().map(source_item));

    let mut page_bboxes = HashMap::<u32, Rect>::new();
    for source in &evidence {
        if let SpanSourceEvidence::Glyph {
            page,
            bbox: glyph_bbox,
            ..
        } = source
        {
            if !valid_rect(*glyph_bbox) {
                return Err(LocalFragmentReviewStopReason::InvalidTrace);
            }
            if let Some(page_bbox) = page_bboxes.get_mut(&page.0) {
                *page_bbox = union_rect(Some(*page_bbox), *glyph_bbox);
            } else {
                budget.charge_output(1)?;
                page_bboxes
                    .try_reserve(1)
                    .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
                page_bboxes.insert(page.0, *glyph_bbox);
            }
        }
    }
    let mut pages = Vec::new();
    pages
        .try_reserve_exact(page_bboxes.len())
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    pages.extend(
        page_bboxes
            .into_iter()
            .map(|(page, bbox)| LocalFragmentSourcePageReport {
                page,
                bbox: bbox.into(),
            }),
    );
    pages.sort_unstable_by_key(|page| page.page);
    Ok(LocalFragmentSourceSideReport {
        total: evidence.len(),
        truncated: evidence.len() > retained,
        samples,
        pages,
    })
}

fn source_item(source: SpanSourceEvidence) -> LocalFragmentSourceEvidenceReport {
    match source {
        SpanSourceEvidence::Glyph {
            glyph_id,
            page,
            bbox,
            content_stream,
            operator_index,
        } => LocalFragmentSourceEvidenceReport::Glyph {
            glyph_id: glyph_id.0,
            page: page.0,
            bbox: bbox.into(),
            content_stream: LocalFragmentObjectRefReport {
                object_number: content_stream.object_number,
                generation: content_stream.generation,
            },
            operator_index,
        },
        SpanSourceEvidence::SyntheticSpace {
            preceding_glyph_id,
            following_glyph_id,
        } => LocalFragmentSourceEvidenceReport::SyntheticSpace {
            preceding_glyph_id: preceding_glyph_id.0,
            following_glyph_id: following_glyph_id.0,
        },
        SpanSourceEvidence::LineBreak {
            preceding_glyph_id,
            following_glyph_id,
        } => LocalFragmentSourceEvidenceReport::LineBreak {
            preceding_glyph_id: preceding_glyph_id.0,
            following_glyph_id: following_glyph_id.0,
        },
        SpanSourceEvidence::BlockSeparatorSpace => {
            LocalFragmentSourceEvidenceReport::BlockSeparatorSpace
        }
    }
}

fn span_report(
    span: &TextSpan,
    budget: &mut ReviewBudget,
) -> Result<LocalFragmentTextSpanReport, LocalFragmentReviewStopReason> {
    budget.charge_output(span.blocks.len())?;
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(span.blocks.len())
        .map_err(|_| LocalFragmentReviewStopReason::AllocationFailure)?;
    blocks.extend(span.blocks.iter().map(|block| block.0));
    Ok(LocalFragmentTextSpanReport {
        blocks,
        separator: span.separator.map(|separator| match separator {
            BlockSeparator::Concatenate => LocalFragmentBlockSeparatorReport::Concatenate,
            BlockSeparator::Space => LocalFragmentBlockSeparatorReport::Space,
        }),
        canonical_start: span.canonical_range.start,
        canonical_end: span.canonical_range.end,
        comparable_start: span.comparable_range.start,
        comparable_end: span.comparable_range.end,
    })
}

fn occurrence_report(
    changed: &RecoveredAtomicOccurrence,
    block_maps: [&HashMap<u64, &BlockText>; 2],
    projectors: [&SpanSourceProjector<'_>; 2],
    budget: &mut ReviewBudget,
) -> Result<LocalFragmentChangedOccurrenceReport, LocalFragmentReviewStopReason> {
    let old_span = changed
        .occurrence
        .old_span
        .as_ref()
        .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    let new_span = changed
        .occurrence
        .new_span
        .as_ref()
        .ok_or(LocalFragmentReviewStopReason::InvalidTrace)?;
    let (_, old_text) = materialize_context(old_span, block_maps[0], budget)?;
    let (_, new_text) = materialize_context(new_span, block_maps[1], budget)?;
    let old_source = source_report(
        projectors[0]
            .project(old_span)
            .map_err(map_projection_error)?,
        budget,
    )?;
    let new_source = source_report(
        projectors[1]
            .project(new_span)
            .map_err(map_projection_error)?,
        budget,
    )?;
    Ok(LocalFragmentChangedOccurrenceReport {
        old_span: Some(span_report(old_span, budget)?),
        new_span: Some(span_report(new_span, budget)?),
        old_text: Some(old_text),
        new_text: Some(new_text),
        old_source: Some(old_source),
        new_source: Some(new_source),
        edit_start: changed.edit_range.start,
        edit_end: changed.edit_range.end,
    })
}

fn edit_report(edit: &AtomicEdit) -> LocalFragmentAtomicEditReport {
    LocalFragmentAtomicEditReport {
        old_start: edit.old.start,
        old_end: edit.old.end,
        new_start: edit.new.start,
        new_end: edit.new.end,
    }
}

fn relation_report(
    best_score: u16,
    second_score: u16,
    scope: Option<RecoveryWatchNearScope>,
) -> Result<LocalFragmentRelationSideReport, LocalFragmentReviewStopReason> {
    validate_relation(best_score, second_score)?;
    let margin = best_score - second_score;
    Ok(LocalFragmentRelationSideReport {
        best_score,
        second_score,
        margin,
        scope: scope.map(Into::into),
    })
}

fn validate_relation(
    best_score: u16,
    second_score: u16,
) -> Result<(), LocalFragmentReviewStopReason> {
    if best_score < second_score {
        return Err(LocalFragmentReviewStopReason::InvalidTrace);
    }
    Ok(())
}

fn map_projection_error(error: Error) -> LocalFragmentReviewStopReason {
    match error {
        Error::LimitExceeded { .. } => LocalFragmentReviewStopReason::SourceProjectionLimit,
        _ => LocalFragmentReviewStopReason::SourceProjectionFailed,
    }
}

fn compare_traces(left: &RecoveredAtomicDiff, right: &RecoveredAtomicDiff) -> Ordering {
    left.old_alignment_span_index
        .cmp(&right.old_alignment_span_index)
        .then_with(|| compare_spans(&left.old_context, &right.old_context))
        .then_with(|| {
            left.new_alignment_span_index
                .cmp(&right.new_alignment_span_index)
        })
        .then_with(|| compare_spans(&left.new_context, &right.new_context))
        .then_with(|| compare_edits(&left.edits, &right.edits))
        .then_with(|| left.old_best_score.cmp(&right.old_best_score))
        .then_with(|| left.old_second_score.cmp(&right.old_second_score))
        .then_with(|| scope_rank(left.old_best_scope).cmp(&scope_rank(right.old_best_scope)))
        .then_with(|| left.new_best_score.cmp(&right.new_best_score))
        .then_with(|| left.new_second_score.cmp(&right.new_second_score))
        .then_with(|| scope_rank(left.new_best_scope).cmp(&scope_rank(right.new_best_scope)))
        .then_with(|| compare_occurrences(&left.changed_occurrences, &right.changed_occurrences))
}

fn compare_spans(left: &TextSpan, right: &TextSpan) -> Ordering {
    left.blocks
        .iter()
        .map(|block| block.0)
        .cmp(right.blocks.iter().map(|block| block.0))
        .then_with(|| separator_rank(left.separator).cmp(&separator_rank(right.separator)))
        .then_with(|| left.canonical_range.start.cmp(&right.canonical_range.start))
        .then_with(|| left.canonical_range.end.cmp(&right.canonical_range.end))
        .then_with(|| {
            left.comparable_range
                .start
                .cmp(&right.comparable_range.start)
        })
        .then_with(|| left.comparable_range.end.cmp(&right.comparable_range.end))
}

fn compare_edits(left: &[AtomicEdit], right: &[AtomicEdit]) -> Ordering {
    left.iter()
        .map(edit_sort_key)
        .cmp(right.iter().map(edit_sort_key))
}

fn edit_sort_key(edit: &AtomicEdit) -> (usize, usize, usize, usize) {
    (edit.old.start, edit.old.end, edit.new.start, edit.new.end)
}

fn separator_rank(separator: Option<BlockSeparator>) -> u8 {
    match separator {
        None => 0,
        Some(BlockSeparator::Concatenate) => 1,
        Some(BlockSeparator::Space) => 2,
    }
}

fn scope_rank(scope: Option<RecoveryWatchNearScope>) -> u8 {
    match scope {
        None => 0,
        Some(RecoveryWatchNearScope::SameSpan) => 1,
        Some(RecoveryWatchNearScope::AmbiguousSpan) => 2,
        Some(RecoveryWatchNearScope::CrossSpan) => 3,
        Some(RecoveryWatchNearScope::PairedStream) => 4,
    }
}

fn compare_occurrences(
    left: &[RecoveredAtomicOccurrence],
    right: &[RecoveredAtomicOccurrence],
) -> Ordering {
    for (left, right) in left.iter().zip(right) {
        let ordering = compare_optional_spans(
            left.occurrence.old_span.as_ref(),
            right.occurrence.old_span.as_ref(),
        )
        .then_with(|| {
            compare_optional_spans(
                left.occurrence.new_span.as_ref(),
                right.occurrence.new_span.as_ref(),
            )
        })
        .then_with(|| left.edit_range.start.cmp(&right.edit_range.start))
        .then_with(|| left.edit_range.end.cmp(&right.edit_range.end));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn compare_optional_spans(left: Option<&TextSpan>, right: Option<&TextSpan>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => compare_spans(left, right),
    }
}

fn append_with_separator(
    tokens: &mut Vec<ComparableToken>,
    space_separator: bool,
    next: &[ComparableToken],
) {
    if space_separator
        && !tokens.last().is_some_and(is_space_token)
        && !next.first().is_some_and(is_space_token)
    {
        tokens.push(ComparableToken::Scalar(' '));
    }
    tokens.extend_from_slice(next);
}

fn is_space_token(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn valid_rect(rect: Rect) -> bool {
    rect.min.x.is_finite()
        && rect.min.y.is_finite()
        && rect.max.x.is_finite()
        && rect.max.y.is_finite()
        && rect.min.x <= rect.max.x
        && rect.min.y <= rect.max.y
}

fn union_rect(current: Option<Rect>, next: Rect) -> Rect {
    current.map_or(next, |current| Rect {
        min: pdfdelta_core::model::Vec2 {
            x: current.min.x.min(next.min.x),
            y: current.min.y.min(next.min.y),
        },
        max: pdfdelta_core::model::Vec2 {
            x: current.max.x.max(next.max.x),
            y: current.max.y.max(next.max.y),
        },
    })
}

#[derive(Default)]
struct ReviewBudget {
    work_items: usize,
    output_items: usize,
    source_evidence: usize,
    text_scalars: usize,
}

impl ReviewBudget {
    fn charge_work(&mut self, amount: usize) -> Result<(), LocalFragmentReviewStopReason> {
        charge(
            &mut self.work_items,
            amount,
            MAX_WORK_ITEMS,
            LocalFragmentReviewStopReason::WorkLimit,
        )
    }

    fn charge_output(&mut self, amount: usize) -> Result<(), LocalFragmentReviewStopReason> {
        charge(
            &mut self.output_items,
            amount,
            MAX_OUTPUT_ITEMS,
            LocalFragmentReviewStopReason::OutputLimit,
        )
    }

    fn charge_source(&mut self, amount: usize) -> Result<(), LocalFragmentReviewStopReason> {
        charge(
            &mut self.source_evidence,
            amount,
            MAX_SOURCE_EVIDENCE,
            LocalFragmentReviewStopReason::SourceEvidenceLimit,
        )
    }

    fn charge_text(&mut self, amount: usize) -> Result<(), LocalFragmentReviewStopReason> {
        charge(
            &mut self.text_scalars,
            amount,
            MAX_TEXT_SCALARS,
            LocalFragmentReviewStopReason::TextLimit,
        )
    }
}

fn charge(
    current: &mut usize,
    amount: usize,
    limit: usize,
    reason: LocalFragmentReviewStopReason,
) -> Result<(), LocalFragmentReviewStopReason> {
    let next = current.checked_add(amount).ok_or(reason)?;
    if next > limit {
        return Err(reason);
    }
    *current = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        diff::{ChangeOccurrence, TokenRange},
        layout::{BlockId, BlockRole},
        model::{GlyphId, GlyphProvenance, PageId, Vec2},
        normalize::{MappedText, ScalarRange, SourceMapEntry, TextSource, TextSourceAtom},
        pdf::ObjectRef,
    };

    use super::*;

    #[test]
    fn bundle_is_deterministic_and_contains_exact_source_evidence() {
        let (old_blocks, old_glyphs) = evidence_side(1, "old");
        let (new_blocks, new_glyphs) = evidence_side(2, "new");
        let first = trace(4, 8, 1, 2);
        let second = trace(2, 6, 1, 2);
        let forward = build_local_fragment_review_bundle(
            &[first.clone(), second.clone()],
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        );
        let reverse = build_local_fragment_review_bundle(
            &[second, first],
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        );
        assert_eq!(forward, reverse);
        let json = serde_json::to_value(&forward).expect("review bundle serializes");
        assert_eq!(
            object_keys(&json["samples"][0]),
            [
                "changed_occurrences",
                "edits",
                "new_alignment_span",
                "new_context",
                "new_context_text",
                "new_relation",
                "new_source",
                "old_alignment_span",
                "old_context",
                "old_context_text",
                "old_relation",
                "old_source",
                "origin",
            ]
        );
        assert_eq!(
            object_keys(&json["samples"][0]["old_context"]),
            [
                "blocks",
                "canonical_end",
                "canonical_start",
                "comparable_end",
                "comparable_start",
                "separator",
            ]
        );
        assert_eq!(
            object_keys(&json["samples"][0]["old_relation"]),
            ["best_score", "margin", "scope", "second_score"]
        );
        assert_eq!(
            object_keys(&json["samples"][0]["old_source"]),
            ["pages", "samples", "total", "truncated"]
        );
        assert_eq!(
            object_keys(&json["samples"][0]["old_source"]["pages"][0]),
            ["bbox", "page"]
        );
        assert_eq!(
            object_keys(&json["samples"][0]["edits"][0]),
            ["new_end", "new_start", "old_end", "old_start"]
        );
        assert_eq!(
            object_keys(&json["samples"][0]["changed_occurrences"][0]),
            [
                "edit_end",
                "edit_start",
                "new_source",
                "new_span",
                "new_text",
                "old_source",
                "old_span",
                "old_text",
            ]
        );
        assert!(
            validate_local_fragment_review_bundle_contract(
                &forward,
                Some(true),
                2,
                &[trace(4, 8, 1, 2), trace(2, 6, 1, 2)],
            )
            .is_ok()
        );
        let LocalFragmentReviewBundleReport::Complete { samples, .. } = &forward else {
            panic!("valid source evidence produces a complete bundle");
        };
        assert_eq!(samples[0].old_alignment_span, 2);
        assert_eq!(samples[0].old_context_text.text, "old");
        assert_eq!(samples[0].old_source.total, 3);
        assert_eq!(samples[0].old_source.pages.len(), 1);
        assert_eq!(samples[0].old_source.pages[0].page, 1);
        assert_eq!(samples[0].old_relation.margin, 1_000);
        assert_eq!(samples[0].edits[0].old_start, 1);
        assert_eq!(samples[0].changed_occurrences[0].edit_end, 2);
        assert_eq!(
            samples[0].changed_occurrences[0]
                .old_text
                .as_ref()
                .map(|text| text.text.as_str()),
            Some("l")
        );
        assert_eq!(
            samples[0].changed_occurrences[0]
                .old_source
                .as_ref()
                .map(|source| source.total),
            Some(1)
        );
    }

    #[test]
    fn bundle_reports_deterministic_truncation() {
        let (old_blocks, old_glyphs) = evidence_side(1, "old");
        let (new_blocks, new_glyphs) = evidence_side(2, "new");
        let traces = (0..3)
            .map(|index| trace(index, index + 10, 1, 2))
            .collect::<Vec<_>>();
        let bundle = build_local_fragment_review_bundle_with_limit(
            &traces,
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
            2,
        )
        .expect("bounded build succeeds");
        let LocalFragmentReviewBundleReport::Complete {
            total_traces,
            total_occurrences,
            sample_limit,
            truncated,
            samples,
        } = bundle
        else {
            panic!("valid traces produce a complete bundle");
        };
        assert_eq!(total_traces, 3);
        assert_eq!(total_occurrences, 3);
        assert_eq!(sample_limit, 2);
        assert!(truncated);
        assert_eq!(samples.len(), 2);
    }

    #[test]
    fn bundle_projects_subblock_occurrence_into_multiblock_context() {
        let (mut old_blocks, mut old_glyphs) = evidence_side(1, "aaa");
        let (mut old_tail_blocks, mut old_tail_glyphs) = evidence_side(2, "bbb");
        old_blocks.append(&mut old_tail_blocks);
        old_glyphs.append(&mut old_tail_glyphs);
        let (mut new_blocks, mut new_glyphs) = evidence_side(3, "aaa");
        let (mut new_tail_blocks, mut new_tail_glyphs) = evidence_side(4, "bXbb");
        new_blocks.append(&mut new_tail_blocks);
        new_glyphs.append(&mut new_tail_glyphs);
        let old_context = TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 0, end: 7 },
            comparable_range: TokenRange { start: 0, end: 7 },
        };
        let new_context = TextSpan {
            blocks: vec![BlockId(3), BlockId(4)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 0, end: 8 },
            comparable_range: TokenRange { start: 0, end: 8 },
        };
        let trace = RecoveredAtomicDiff {
            origin: ChangeOrigin::LocalFragment,
            old_alignment_span_index: 1,
            new_alignment_span_index: 2,
            old_context,
            new_context,
            changed_occurrences: vec![RecoveredAtomicOccurrence {
                occurrence: ChangeOccurrence {
                    old_span: Some(TextSpan {
                        blocks: vec![BlockId(2)],
                        separator: None,
                        canonical_range: ScalarRange { start: 1, end: 1 },
                        comparable_range: TokenRange { start: 1, end: 1 },
                    }),
                    new_span: Some(TextSpan {
                        blocks: vec![BlockId(4)],
                        separator: None,
                        canonical_range: ScalarRange { start: 1, end: 2 },
                        comparable_range: TokenRange { start: 1, end: 2 },
                    }),
                },
                edit_range: 0..1,
            }],
            edits: vec![AtomicEdit {
                old: 5..5,
                new: 5..6,
            }],
            old_best_score: 8_000,
            old_second_score: 7_000,
            old_best_scope: Some(RecoveryWatchNearScope::SameSpan),
            new_best_score: 8_100,
            new_second_score: 7_000,
            new_best_scope: Some(RecoveryWatchNearScope::SameSpan),
        };

        let bundle = build_local_fragment_review_bundle(
            std::slice::from_ref(&trace),
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        );

        let LocalFragmentReviewBundleReport::Complete {
            total_traces,
            samples,
            ..
        } = bundle
        else {
            panic!("a contiguous child block projects into its recovery context");
        };
        assert_eq!(total_traces, 1);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].changed_occurrences[0].edit_start, 0);
        assert_eq!(samples[0].changed_occurrences[0].edit_end, 1);
    }

    #[test]
    fn invalid_unsampled_trace_fails_closed() {
        let (old_blocks, old_glyphs) = evidence_side(1, "old");
        let (new_blocks, new_glyphs) = evidence_side(2, "new");
        let valid = trace(1, 2, 1, 2);
        let mut invalid = trace(3, 4, 1, 2);
        invalid.edits[0].old.end = 99;
        let bundle = build_local_fragment_review_bundle_with_limit(
            &[valid, invalid],
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
            1,
        );
        assert_eq!(bundle, Err(LocalFragmentReviewStopReason::InvalidTrace));
    }

    #[test]
    fn source_bboxes_are_partitioned_by_page() {
        let evidence = vec![
            glyph_source(1, 1, 0.0, 1.0),
            glyph_source(2, 1, 2.0, 3.0),
            glyph_source(3, 2, 10.0, 11.0),
        ];
        let report = source_report(evidence, &mut ReviewBudget::default())
            .expect("page-specific source report succeeds");
        assert_eq!(report.pages.len(), 2);
        assert_eq!(report.pages[0].page, 1);
        assert_eq!(report.pages[0].bbox.min.x, 0.0);
        assert_eq!(report.pages[0].bbox.max.x, 3.0);
        assert_eq!(report.pages[1].page, 2);
        assert_eq!(report.pages[1].bbox.min.x, 10.0);
        assert_eq!(report.pages[1].bbox.max.x, 11.0);
    }

    #[test]
    fn bundle_contract_rejects_proposal_count_and_stopped_trace_mismatches() {
        let empty = LocalFragmentReviewBundleReport::default();
        assert!(validate_local_fragment_review_bundle_contract(&empty, None, 0, &[]).is_ok());
        let local = trace(1, 2, 1, 2);
        assert!(
            validate_local_fragment_review_bundle_contract(
                &empty,
                Some(false),
                0,
                std::slice::from_ref(&local),
            )
            .is_err()
        );
        assert!(
            validate_local_fragment_review_bundle_contract(
                &empty,
                Some(true),
                1,
                std::slice::from_ref(&local),
            )
            .is_err()
        );
    }

    #[test]
    fn sampled_and_unsampled_paths_charge_each_projection_once() {
        let (old_blocks, old_glyphs) = evidence_side(1, "old");
        let (new_blocks, new_glyphs) = evidence_side(2, "new");
        let old_map = build_block_map(&old_blocks).expect("old block map builds");
        let new_map = build_block_map(&new_blocks).expect("new block map builds");
        let limits = SpanSourceProjectionLimits {
            max_comparable_tokens: MAX_CONTEXT_TOKENS,
            max_evidence_items: MAX_SOURCE_EVIDENCE,
        };
        let old_projector = SpanSourceProjector::new(&old_blocks, &old_glyphs, limits)
            .expect("old projector builds");
        let new_projector = SpanSourceProjector::new(&new_blocks, &new_glyphs, limits)
            .expect("new projector builds");
        let trace = trace(1, 2, 1, 2);

        let mut sampled_budget = ReviewBudget::default();
        build_trace_report(
            &trace,
            [&old_map, &new_map],
            [&old_projector, &new_projector],
            &mut sampled_budget,
        )
        .expect("sampled trace materializes");
        assert_eq!(sampled_budget.source_evidence, 8);
        assert!(sampled_budget.output_items > 0);
        assert!(sampled_budget.text_scalars > 0);

        let mut unsampled_budget = ReviewBudget::default();
        validate_trace(
            &trace,
            [&old_map, &new_map],
            [&old_projector, &new_projector],
            &mut unsampled_budget,
        )
        .expect("unsampled trace validates");
        assert_eq!(unsampled_budget.source_evidence, 8);
        assert_eq!(unsampled_budget.output_items, 0);
        assert_eq!(unsampled_budget.text_scalars, 0);
    }

    #[test]
    fn empty_bundle_has_the_exact_nested_schema_key() {
        let value = serde_json::to_value(super::super::SentenceRecoveryMetricsReport::default())
            .expect("report serializes");
        let bundle = value
            .as_object()
            .and_then(|object| object.get("local_fragment_review_bundle"))
            .expect("nested bundle key exists");
        assert_eq!(bundle["status"], "complete");
        assert_eq!(bundle["total_traces"], 0);
        assert_eq!(bundle["sample_limit"], SAMPLE_LIMIT);
        assert_eq!(bundle["samples"], serde_json::json!([]));
    }

    fn trace(
        old_alignment_span_index: usize,
        new_alignment_span_index: usize,
        old_block: u64,
        new_block: u64,
    ) -> RecoveredAtomicDiff {
        let old_context = span(old_block, 3);
        let new_context = span(new_block, 3);
        RecoveredAtomicDiff {
            origin: ChangeOrigin::LocalFragment,
            old_alignment_span_index,
            new_alignment_span_index,
            old_context: old_context.clone(),
            new_context: new_context.clone(),
            changed_occurrences: vec![RecoveredAtomicOccurrence {
                occurrence: ChangeOccurrence {
                    old_span: Some(TextSpan {
                        canonical_range: ScalarRange { start: 1, end: 2 },
                        comparable_range: TokenRange { start: 1, end: 2 },
                        ..old_context
                    }),
                    new_span: Some(TextSpan {
                        canonical_range: ScalarRange { start: 1, end: 2 },
                        comparable_range: TokenRange { start: 1, end: 2 },
                        ..new_context
                    }),
                },
                edit_range: 0..2,
            }],
            edits: vec![
                AtomicEdit {
                    old: 1..2,
                    new: 1..1,
                },
                AtomicEdit {
                    old: 2..2,
                    new: 1..2,
                },
            ],
            old_best_score: 8_000,
            old_second_score: 7_000,
            old_best_scope: Some(RecoveryWatchNearScope::SameSpan),
            new_best_score: 8_100,
            new_second_score: 7_000,
            new_best_scope: Some(RecoveryWatchNearScope::SameSpan),
        }
    }

    fn span(block: u64, len: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: len },
            comparable_range: TokenRange { start: 0, end: len },
        }
    }

    fn evidence_side(block: u64, text: &str) -> (Vec<BlockText>, Vec<GlyphEvidence>) {
        let mut source_map = Vec::new();
        let mut glyphs = Vec::new();
        for (index, _) in text.chars().enumerate() {
            let glyph_id = GlyphId(block * 100 + index as u64);
            source_map.push(SourceMapEntry {
                output_range: ScalarRange {
                    start: index,
                    end: index + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(glyph_id)],
                },
            });
            glyphs.push(GlyphEvidence {
                id: glyph_id,
                page: PageId(1),
                bbox: Rect {
                    min: Vec2 {
                        x: index as f64,
                        y: 2.0,
                    },
                    max: Vec2 {
                        x: index as f64 + 1.0,
                        y: 3.0,
                    },
                },
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: 5,
                        generation: 0,
                    },
                    operator_index: index as u32,
                },
            });
        }
        let mapped = || MappedText {
            text: text.to_owned(),
            source_map: source_map.clone(),
            unmapped: Vec::new(),
        };
        (
            vec![BlockText {
                block: BlockId(block),
                role: BlockRole::Body,
                raw: mapped(),
                canonical: mapped(),
                matching: text.to_owned(),
                matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
                numeric_mask_applied: false,
                normalization_events: Vec::new(),
                issues: Vec::new(),
                pages: vec![1],
                font_size_signatures: None,
                position_signatures: None,
                line_breaks: None,
                page_breaks: None,
            }],
            glyphs,
        )
    }

    fn glyph_source(glyph_id: u64, page: u32, min_x: f64, max_x: f64) -> SpanSourceEvidence {
        SpanSourceEvidence::Glyph {
            glyph_id: GlyphId(glyph_id),
            page: PageId(page),
            bbox: Rect {
                min: Vec2 { x: min_x, y: 2.0 },
                max: Vec2 { x: max_x, y: 3.0 },
            },
            content_stream: ObjectRef {
                object_number: 5,
                generation: 0,
            },
            operator_index: 0,
        }
    }

    fn object_keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys = value
            .as_object()
            .expect("value is an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys
    }
}
