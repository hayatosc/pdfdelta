//! Bounded, behavior-neutral structural-container diagnostics.

use std::collections::HashMap;

use crate::{
    layout::BlockRole,
    normalize::{BlockText, ComparableToken},
};

use super::super::Side;
use super::ownership::{
    RecoveryOwnership, RecoveryOwnershipLedger, RecoveryOwnershipLedgerRange, RecoveryOwnershipRole,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StructuralContainerKind {
    Document,
    Section,
    Paragraph,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Typed reason why complete structural-container diagnostics are unavailable.
pub enum StructuralContainerStopReason {
    BlockLimit,
    RangeLimit,
    TokenLimit,
    FontEvidenceLimit,
    ContainerLimit,
    AllocationFailure,
    CounterOverflow,
    InvalidLedger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct StructuralContainerLimits {
    pub max_blocks: usize,
    pub max_ranges: usize,
    pub max_tokens: usize,
    pub max_font_evidence: usize,
    pub max_containers: usize,
}

impl StructuralContainerLimits {
    pub(in crate::diff) fn from_max_tokens(max_tokens: usize) -> Self {
        Self {
            max_blocks: max_tokens,
            max_ranges: max_tokens,
            max_tokens,
            max_font_evidence: max_tokens.saturating_mul(4),
            max_containers: max_tokens.saturating_add(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Complete per-side structural-container counters.
pub struct StructuralContainerSideMetrics {
    pub documents: usize,
    pub sections: usize,
    pub paragraphs: usize,
    pub heading_candidates: usize,
    pub accepted_headings: usize,
    pub rejected_unsafe: usize,
    pub rejected_not_single_line: usize,
    pub rejected_without_numbering: usize,
    pub rejected_not_font_prominent: usize,

    pub block_visits: usize,
    pub range_visits: usize,
    pub token_visits: usize,
    pub font_evidence_visits: usize,
    pub max_section_depth: usize,
}

/// Heap-free structural-container diagnostics published by the diff pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StructuralContainerMetrics {
    pub complete: bool,
    pub stop_reason: Option<StructuralContainerStopReason>,
    pub old: StructuralContainerSideMetrics,
    pub new: StructuralContainerSideMetrics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StructuralContainer {
    pub id: usize,
    pub kind: StructuralContainerKind,
    pub parent: Option<usize>,
    pub block_id: Option<u64>,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub trusted_run_id: Option<u64>,
    pub ordinal_start: Option<usize>,
    pub ordinal_end: Option<usize>,
    pub numbering_level: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StructuralContainerSideAnalysis {
    pub metrics: StructuralContainerSideMetrics,
    pub containers: Vec<StructuralContainer>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StructuralContainerAnalysis {
    pub sides: [StructuralContainerSideAnalysis; 2],
}

/// Builds diagnostic-only containers from verified ownership ledgers.
///
/// Failure returns no partial metrics or container records.
fn analyze_structural_containers(
    sides: [&Side<'_>; 2],
    ledgers: &[RecoveryOwnershipLedger; 2],
    limits: StructuralContainerLimits,
) -> Result<StructuralContainerAnalysis, StructuralContainerStopReason> {
    Ok(StructuralContainerAnalysis {
        sides: [
            analyze_side(sides[0], &ledgers[0], limits)?,
            analyze_side(sides[1], &ledgers[1], limits)?,
        ],
    })
}

pub(in crate::diff) fn analyze_structural_container_metrics(
    sides: [&Side<'_>; 2],
    ledgers: &[RecoveryOwnershipLedger; 2],
    limits: StructuralContainerLimits,
) -> StructuralContainerMetrics {
    match analyze_structural_containers(sides, ledgers, limits) {
        Ok(analysis) => StructuralContainerMetrics {
            complete: true,
            stop_reason: None,
            old: analysis.sides[0].metrics,
            new: analysis.sides[1].metrics,
        },
        Err(reason) => StructuralContainerMetrics {
            complete: false,
            stop_reason: Some(reason),
            ..StructuralContainerMetrics::default()
        },
    }
}

#[derive(Clone)]
struct SafeBlock {
    side_index: usize,
    block_id: u64,
    page: u32,
    run_id: u64,
    ordinal_start: usize,
    ordinal_end: usize,
    font_median: f64,
    numbering_level: Option<u8>,
    single_line: bool,
}

fn analyze_side(
    side: &Side<'_>,
    ledger: &RecoveryOwnershipLedger,
    limits: StructuralContainerLimits,
) -> Result<StructuralContainerSideAnalysis, StructuralContainerStopReason> {
    enforce(
        ledger.blocks.len(),
        limits.max_blocks,
        StructuralContainerStopReason::BlockLimit,
    )?;
    enforce(
        ledger.ranges.len(),
        limits.max_ranges,
        StructuralContainerStopReason::RangeLimit,
    )?;

    let mut ranges_by_block = Vec::new();
    ranges_by_block
        .try_reserve_exact(ledger.blocks.len())
        .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
    ranges_by_block.resize_with(ledger.blocks.len(), Vec::new);
    for range in &ledger.ranges {
        let ranges = ranges_by_block
            .get_mut(range.block_index)
            .ok_or(StructuralContainerStopReason::InvalidLedger)?;
        ranges
            .try_reserve(1)
            .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
        ranges.push(*range);
    }

    let mut metrics = StructuralContainerSideMetrics {
        documents: 1,
        ..StructuralContainerSideMetrics::default()
    };
    let mut token_visits = 0usize;
    let mut font_visits = 0usize;
    let mut safe = Vec::new();
    safe.try_reserve(ledger.blocks.len())
        .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;

    for (metadata, ranges) in ledger.blocks.iter().zip(&ranges_by_block) {
        metrics.block_visits = checked_inc(metrics.block_visits)?;
        metrics.range_visits = checked_add(metrics.range_visits, ranges.len())?;
        let side_index = *side
            .index
            .get(&crate::layout::BlockId(metadata.block_id))
            .ok_or(StructuralContainerStopReason::InvalidLedger)?;
        let block = side
            .blocks
            .get(side_index)
            .ok_or(StructuralContainerStopReason::InvalidLedger)?;
        let tokens = side
            .canonical
            .get(side_index)
            .ok_or(StructuralContainerStopReason::InvalidLedger)?;
        token_visits = checked_add(token_visits, tokens.len())?;
        enforce(
            token_visits,
            limits.max_tokens,
            StructuralContainerStopReason::TokenLimit,
        )?;

        let Some(candidate) = safe_block(
            side_index,
            metadata.block_id,
            metadata.role,
            metadata.trusted,
            &metadata.context,
            block,
            tokens,
            ranges,
            &mut font_visits,
            limits.max_font_evidence,
        )?
        else {
            metrics.rejected_unsafe = checked_inc(metrics.rejected_unsafe)?;
            continue;
        };
        safe.push(candidate);
    }
    metrics.token_visits = token_visits;
    metrics.font_evidence_visits = font_visits;

    let page_medians = page_body_font_medians(&safe)?;
    let mut containers = Vec::new();
    let capacity = checked_inc(safe.len())?;
    enforce(
        capacity,
        limits.max_containers,
        StructuralContainerStopReason::ContainerLimit,
    )?;
    containers
        .try_reserve(capacity)
        .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
    containers.push(StructuralContainer {
        id: 0,
        kind: StructuralContainerKind::Document,
        parent: None,
        block_id: None,
        comparable_start: 0,
        comparable_end: 0,
        trusted_run_id: None,
        ordinal_start: None,
        ordinal_end: None,
        numbering_level: None,
    });

    let mut stack: Vec<(u64, u8, usize, usize)> = Vec::new();
    stack
        .try_reserve(safe.len())
        .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
    let mut previous: Option<(u64, usize)> = None;
    for block in safe {
        let continuous = previous == Some((block.run_id, block.ordinal_start));
        if !continuous {
            stack.clear();
        }
        previous = Some((block.run_id, block.ordinal_end));

        let prominent = page_medians
            .get(&block.page)
            .is_some_and(|values| block.font_median > values[values.len() / 2]);
        let heading_candidate = block.numbering_level.is_some() || (block.single_line && prominent);
        if heading_candidate {
            metrics.heading_candidates = checked_inc(metrics.heading_candidates)?;
        }
        if heading_candidate && block.numbering_level.is_none() {
            metrics.rejected_without_numbering = checked_inc(metrics.rejected_without_numbering)?;
        }
        if heading_candidate && !block.single_line {
            metrics.rejected_not_single_line = checked_inc(metrics.rejected_not_single_line)?;
        }
        if heading_candidate && !prominent {
            metrics.rejected_not_font_prominent = checked_inc(metrics.rejected_not_font_prominent)?;
        }

        if let Some(level) = block
            .numbering_level
            .filter(|_| block.single_line && prominent)
        {
            while stack
                .last()
                .is_some_and(|(_, parent_level, _, _)| *parent_level >= level)
            {
                stack.pop();
            }
            let parent = stack.last().map_or(0, |(_, _, id, _)| *id);
            let id = containers.len();
            push_container(
                &mut containers,
                StructuralContainer {
                    id,
                    kind: StructuralContainerKind::Section,
                    parent: Some(parent),
                    block_id: Some(block.block_id),
                    comparable_start: 0,
                    comparable_end: side.canonical[block.side_index].len(),
                    trusted_run_id: Some(block.run_id),
                    ordinal_start: Some(block.ordinal_start),
                    ordinal_end: Some(block.ordinal_end),
                    numbering_level: Some(level),
                },
                limits.max_containers,
            )?;
            stack.push((block.run_id, level, id, block.ordinal_end));
            metrics.sections = checked_inc(metrics.sections)?;
            metrics.accepted_headings = checked_inc(metrics.accepted_headings)?;
            metrics.max_section_depth = metrics.max_section_depth.max(stack.len());
            continue;
        }

        let parent = stack.last().map_or(0, |(_, _, id, _)| *id);
        let id = containers.len();
        push_container(
            &mut containers,
            StructuralContainer {
                id,
                kind: StructuralContainerKind::Paragraph,
                parent: Some(parent),
                block_id: Some(block.block_id),
                comparable_start: 0,
                comparable_end: side.canonical[block.side_index].len(),
                trusted_run_id: Some(block.run_id),
                ordinal_start: Some(block.ordinal_start),
                ordinal_end: Some(block.ordinal_end),
                numbering_level: None,
            },
            limits.max_containers,
        )?;
        metrics.paragraphs = checked_inc(metrics.paragraphs)?;
    }

    Ok(StructuralContainerSideAnalysis {
        metrics,
        containers,
    })
}

#[allow(clippy::too_many_arguments)]
fn safe_block(
    side_index: usize,
    block_id: u64,
    role: RecoveryOwnershipRole,
    trusted: bool,
    context: &super::ownership::RecoveryOwnershipContext,
    block: &BlockText,
    tokens: &[ComparableToken],
    ranges: &[RecoveryOwnershipLedgerRange],
    font_visits: &mut usize,
    max_font_evidence: usize,
) -> Result<Option<SafeBlock>, StructuralContainerStopReason> {
    if role != RecoveryOwnershipRole::Body
        || block.role != BlockRole::Body
        || !trusted
        || !block.issues.is_empty()
        || block.pages.len() != 1
        || tokens.is_empty()
        || tokens
            .iter()
            .any(|token| matches!(token, ComparableToken::Unmapped { .. }))
        || ranges.is_empty()
        || ranges
            .iter()
            .any(|range| matches!(range.ownership, RecoveryOwnership::Gap(_)))
    {
        return Ok(None);
    }
    let (Some(run_id), Some(ordinal_start), Some(ordinal_end), Some(page)) = (
        context.trusted_run_id,
        context.ordinal_start,
        context.ordinal_end,
        context.page,
    ) else {
        return Ok(None);
    };
    if ordinal_start >= ordinal_end || page != block.pages[0] || !ranges_cover(tokens.len(), ranges)
    {
        return Ok(None);
    }
    let signatures = match &block.font_size_signatures {
        Some(signatures) if signatures.len() == tokens.len() => signatures,
        _ => return Ok(None),
    };
    let mut sizes = Vec::new();
    for signature in signatures {
        for size in signature.values() {
            *font_visits = checked_inc(*font_visits)?;
            enforce(
                *font_visits,
                max_font_evidence,
                StructuralContainerStopReason::FontEvidenceLimit,
            )?;
            sizes
                .try_reserve(1)
                .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
            sizes.push(size);
        }
    }
    if sizes.is_empty() {
        return Ok(None);
    }
    sizes.sort_by(f64::total_cmp);
    let font_median = sizes[sizes.len() / 2];
    let single_line = block.line_breaks.as_ref().is_some_and(Vec::is_empty);
    Ok(Some(SafeBlock {
        side_index,
        block_id,
        page,
        run_id,
        ordinal_start,
        ordinal_end,
        font_median,
        numbering_level: numbering_level(&block.canonical.text),
        single_line,
    }))
}

fn ranges_cover(token_count: usize, ranges: &[RecoveryOwnershipLedgerRange]) -> bool {
    let mut cursor = 0usize;
    ranges.iter().all(|range| {
        let valid = range.comparable_start == cursor
            && range.comparable_start < range.comparable_end
            && range.comparable_end <= token_count;
        cursor = range.comparable_end;
        valid
    }) && cursor == token_count
}

fn page_body_font_medians(
    blocks: &[SafeBlock],
) -> Result<HashMap<u32, Vec<f64>>, StructuralContainerStopReason> {
    let mut values = HashMap::<u32, Vec<f64>>::new();
    values
        .try_reserve(blocks.len())
        .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
    for block in blocks
        .iter()
        .filter(|block| block.numbering_level.is_none())
    {
        values
            .entry(block.page)
            .or_default()
            .try_reserve(1)
            .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
        values
            .get_mut(&block.page)
            .ok_or(StructuralContainerStopReason::AllocationFailure)?
            .push(block.font_median);
    }
    for page_values in values.values_mut() {
        page_values.sort_by(f64::total_cmp);
    }
    Ok(values)
}

fn numbering_level(text: &str) -> Option<u8> {
    let text = text.trim_start();
    if text
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("appendix"))
        && text[8..].chars().next().is_none_or(char::is_whitespace)
    {
        return Some(1);
    }
    let mut chars = text.char_indices().peekable();
    let first = chars.peek()?.1;
    if first.is_ascii_alphabetic() {
        chars.next();
        return matches!(chars.next(), Some((_, '.')))
            .then(|| chars.next().is_none_or(|(_, value)| value.is_whitespace()))
            .filter(|value| *value)
            .map(|_| 1);
    }
    if !first.is_ascii_digit() {
        return None;
    }
    let mut level = 1u8;
    while chars.next_if(|(_, value)| value.is_ascii_digit()).is_some() {}
    while let Some((_, '.')) = chars.peek().copied() {
        chars.next();
        if !chars
            .peek()
            .is_some_and(|(_, value)| value.is_ascii_digit())
        {
            break;
        }
        level = level.checked_add(1)?;
        while chars.next_if(|(_, value)| value.is_ascii_digit()).is_some() {}
    }
    chars
        .next()
        .is_none_or(|(_, value)| value.is_whitespace() || value == ':' || value == '-')
        .then_some(level)
}

fn push_container(
    containers: &mut Vec<StructuralContainer>,
    container: StructuralContainer,
    limit: usize,
) -> Result<(), StructuralContainerStopReason> {
    let attempted = checked_inc(containers.len())?;
    enforce(
        attempted,
        limit,
        StructuralContainerStopReason::ContainerLimit,
    )?;
    containers
        .try_reserve(1)
        .map_err(|_| StructuralContainerStopReason::AllocationFailure)?;
    containers.push(container);
    Ok(())
}

fn checked_inc(value: usize) -> Result<usize, StructuralContainerStopReason> {
    value
        .checked_add(1)
        .ok_or(StructuralContainerStopReason::CounterOverflow)
}

fn checked_add(left: usize, right: usize) -> Result<usize, StructuralContainerStopReason> {
    left.checked_add(right)
        .ok_or(StructuralContainerStopReason::CounterOverflow)
}

fn enforce(
    actual: usize,
    limit: usize,
    reason: StructuralContainerStopReason,
) -> Result<(), StructuralContainerStopReason> {
    if actual > limit { Err(reason) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use crate::{
        diff::recovery::ownership::{
            RecoveryLeafKind, RecoveryOwnershipContext, RecoveryOwnershipLedgerBlock,
            RecoveryOwnershipRect,
        },
        layout::{BlockId, BlockRole},
        normalize::{FontSizeSignature, MappedText},
    };

    use super::*;

    fn block(id: u64, text: &str, size: f64, multiline: bool) -> BlockText {
        let mapped = || MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        };
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: mapped(),
            canonical: mapped(),
            matching: text.to_owned(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![1],
            font_size_signatures: Some(
                text.chars()
                    .map(|_| FontSizeSignature::new(&[size]).expect("valid size"))
                    .collect(),
            ),
            position_signatures: None,
            line_breaks: Some(if multiline { vec![1] } else { Vec::new() }),
            page_breaks: Some(Vec::new()),
        }
    }

    fn side(blocks: &[BlockText]) -> Side<'_> {
        let canonical = blocks
            .iter()
            .map(|block| block.canonical.comparable_tokens().expect("valid text"))
            .collect::<Vec<_>>();
        let total_tokens = canonical.iter().map(Vec::len).sum();
        Side {
            blocks,
            index: blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.block, index))
                .collect(),
            canonical,
            total_tokens,
        }
    }

    fn fixture_ledger(
        source: &[BlockText],
        runs: &[(u64, usize, usize)],
        ownerships: &[RecoveryOwnership],
    ) -> RecoveryOwnershipLedger {
        let blocks = source
            .iter()
            .zip(runs)
            .map(|(block, (run, start, end))| RecoveryOwnershipLedgerBlock {
                block_id: block.block.0,
                trusted: true,
                role: RecoveryOwnershipRole::Body,
                context: RecoveryOwnershipContext {
                    trusted_run_id: Some(*run),
                    ordinal_start: Some(*start),
                    ordinal_end: Some(*end),
                    region_id: Some(*run),
                    page: Some(1),
                    bbox: Some(RecoveryOwnershipRect {
                        min_x_bits: 0,
                        min_y_bits: 0,
                        max_x_bits: 1.0f64.to_bits(),
                        max_y_bits: 1.0f64.to_bits(),
                    }),
                },
            })
            .collect();
        let ranges = source
            .iter()
            .zip(ownerships)
            .enumerate()
            .map(|(block_index, (block, ownership))| {
                let token_len = block.canonical.text.chars().count();
                RecoveryOwnershipLedgerRange {
                    block_index,
                    canonical_start: 0,
                    canonical_end: token_len,
                    comparable_start: 0,
                    comparable_end: token_len,
                    ownership: *ownership,
                }
            })
            .collect();
        RecoveryOwnershipLedger { blocks, ranges }
    }

    fn complete(
        blocks: &[BlockText],
        runs: &[(u64, usize, usize)],
        ownerships: &[RecoveryOwnership],
        limits: StructuralContainerLimits,
    ) -> StructuralContainerAnalysis {
        let ledger = fixture_ledger(blocks, runs, ownerships);
        let side = side(blocks);
        match analyze_structural_containers([&side, &side], &[ledger.clone(), ledger], limits) {
            Ok(analysis) => analysis,
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    fn leaf() -> RecoveryOwnership {
        RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody)
    }

    #[test]
    fn clean_block_and_mixed_accepted_leaf_ranges_form_one_paragraph() {
        let blocks = vec![block(1, "Body", 10.0, false)];
        let mut ledger = fixture_ledger(&blocks, &[(1, 0, 1)], &[leaf()]);
        ledger.ranges = vec![
            RecoveryOwnershipLedgerRange {
                comparable_end: 2,
                canonical_end: 2,
                ..ledger.ranges[0]
            },
            RecoveryOwnershipLedgerRange {
                comparable_start: 2,
                canonical_start: 2,
                ownership: RecoveryOwnership::Accepted,
                ..ledger.ranges[0]
            },
        ];
        let side = side(&blocks);
        let outcome = analyze_structural_containers(
            [&side, &side],
            &[ledger.clone(), ledger],
            StructuralContainerLimits::from_max_tokens(64),
        );
        let Ok(analysis) = outcome else {
            panic!("complete analysis");
        };
        assert_eq!(analysis.sides[0].metrics.paragraphs, 1);
    }

    #[test]
    fn accepts_only_numbered_single_line_prominent_headings() {
        let blocks = vec![
            block(1, "1 Scope", 20.0, false),
            block(2, "Body", 10.0, false),
            block(3, "2 Other", 10.0, false),
            block(4, "3 Multi", 20.0, true),
        ];
        let analysis = complete(
            &blocks,
            &[(1, 0, 1), (1, 1, 2), (1, 2, 3), (1, 3, 4)],
            &[leaf(); 4],
            StructuralContainerLimits::from_max_tokens(128),
        );
        let metrics = analysis.sides[0].metrics;
        assert_eq!(metrics.heading_candidates, 3);
        assert_eq!(metrics.accepted_headings, 1);
        assert_eq!(metrics.rejected_not_font_prominent, 1);
        assert_eq!(metrics.rejected_not_single_line, 1);
        assert_eq!(metrics.rejected_without_numbering, 0);
    }
    #[test]
    fn prominent_unnumbered_lines_remain_rejected_candidates() {
        let blocks = vec![
            block(1, "Introduction", 20.0, false),
            block(2, "First body", 10.0, false),
            block(3, "Second body", 10.0, false),
        ];
        let analysis = complete(
            &blocks,
            &[(1, 0, 1), (1, 1, 2), (1, 2, 3)],
            &[leaf(); 3],
            StructuralContainerLimits::from_max_tokens(128),
        );
        let metrics = analysis.sides[0].metrics;
        assert_eq!(metrics.heading_candidates, 1);
        assert_eq!(metrics.accepted_headings, 0);
        assert_eq!(metrics.rejected_without_numbering, 1);
        assert_eq!(metrics.sections, 0);
        assert_eq!(metrics.paragraphs, 3);
    }

    #[test]
    fn nests_numbered_sections_within_one_contiguous_run() {
        let blocks = vec![
            block(1, "1 Scope", 20.0, false),
            block(2, "1.1 Detail", 18.0, false),
            block(3, "Body", 10.0, false),
        ];
        let analysis = complete(
            &blocks,
            &[(1, 0, 1), (1, 1, 2), (1, 2, 3)],
            &[leaf(); 3],
            StructuralContainerLimits::from_max_tokens(128),
        );
        let sections = analysis.sides[0]
            .containers
            .iter()
            .filter(|container| container.kind == StructuralContainerKind::Section)
            .collect::<Vec<_>>();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1].parent, Some(sections[0].id));
        assert_eq!(analysis.sides[0].metrics.max_section_depth, 2);
    }

    #[test]
    fn run_and_ordinal_boundaries_reset_section_parentage() {
        let blocks = vec![
            block(1, "1 Scope", 20.0, false),
            block(2, "1.1 Other", 18.0, false),
            block(3, "Body", 10.0, false),
        ];
        let analysis = complete(
            &blocks,
            &[(1, 0, 1), (2, 4, 5), (2, 5, 6)],
            &[leaf(); 3],
            StructuralContainerLimits::from_max_tokens(128),
        );
        let sections = analysis.sides[0]
            .containers
            .iter()
            .filter(|container| container.kind == StructuralContainerKind::Section)
            .collect::<Vec<_>>();
        assert_eq!(sections[1].parent, Some(0));
    }

    #[test]
    fn excludes_gap_owned_and_unsafe_blocks() {
        let mut unsafe_block = block(2, "Body", 10.0, false);
        unsafe_block.font_size_signatures = None;
        let blocks = vec![block(1, "Body", 10.0, false), unsafe_block];
        let analysis = complete(
            &blocks,
            &[(1, 0, 1), (1, 1, 2)],
            &[
                RecoveryOwnership::Gap(
                    super::super::ownership::RecoveryGapReason::NormalizationIssue,
                ),
                leaf(),
            ],
            StructuralContainerLimits::from_max_tokens(64),
        );
        assert_eq!(analysis.sides[0].metrics.paragraphs, 0);
        assert_eq!(analysis.sides[0].metrics.rejected_unsafe, 2);
    }
    #[test]
    fn empty_block_is_rejected_without_stopping_complete_analysis() {
        let blocks = vec![block(1, "", 10.0, false)];
        let mut ledger = fixture_ledger(&blocks, &[(1, 0, 1)], &[leaf()]);
        ledger.ranges.clear();
        let side = side(&blocks);
        let outcome = analyze_structural_containers(
            [&side, &side],
            &[ledger.clone(), ledger],
            StructuralContainerLimits::from_max_tokens(64),
        );
        let Ok(analysis) = outcome else {
            panic!("complete analysis");
        };
        assert_eq!(analysis.sides[0].metrics.block_visits, 1);
        assert_eq!(analysis.sides[0].metrics.range_visits, 0);
        assert_eq!(analysis.sides[0].metrics.rejected_unsafe, 1);
    }

    #[test]
    fn resource_stops_hide_all_partial_metrics() {
        let blocks = vec![block(1, "Body", 10.0, false)];
        let ledger = fixture_ledger(&blocks, &[(1, 0, 1)], &[leaf()]);
        let side = side(&blocks);
        let base = StructuralContainerLimits::from_max_tokens(64);
        let cases = [
            (
                StructuralContainerLimits {
                    max_blocks: 0,
                    ..base
                },
                StructuralContainerStopReason::BlockLimit,
            ),
            (
                StructuralContainerLimits {
                    max_ranges: 0,
                    ..base
                },
                StructuralContainerStopReason::RangeLimit,
            ),
            (
                StructuralContainerLimits {
                    max_tokens: 3,
                    ..base
                },
                StructuralContainerStopReason::TokenLimit,
            ),
            (
                StructuralContainerLimits {
                    max_font_evidence: 3,
                    ..base
                },
                StructuralContainerStopReason::FontEvidenceLimit,
            ),
            (
                StructuralContainerLimits {
                    max_containers: 1,
                    ..base
                },
                StructuralContainerStopReason::ContainerLimit,
            ),
        ];
        for (limits, reason) in cases {
            let metrics = analyze_structural_container_metrics(
                [&side, &side],
                &[ledger.clone(), ledger.clone()],
                limits,
            );
            assert_eq!(
                metrics,
                StructuralContainerMetrics {
                    complete: false,
                    stop_reason: Some(reason),
                    ..StructuralContainerMetrics::default()
                }
            );
        }
    }
}
