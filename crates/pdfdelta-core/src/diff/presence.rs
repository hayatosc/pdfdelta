use std::ops::Range;

use crate::{
    alignment::{Alignment, AlignmentEvidence, BlockSeparator},
    layout::BlockId,
    normalize::ComparableToken,
};

use super::{
    ChangeEvent, ChangedRegionProof, Confidence, ProvenChangedRegion, Side, TextSpan,
    UnresolvedRegion,
    recovery::ownership::{RecoveryGapReason, RecoveryOwnership, RecoveryOwnershipLedger},
    try_group_full_span,
};

#[derive(Clone, Debug)]
struct AnchorWindow {
    old: Range<usize>,
    new: Range<usize>,
}

#[derive(Default)]
struct WindowInput {
    blocked: bool,
    has_unresolved: bool,
    range_indices: [Vec<usize>; 2],
    separators: [Option<BlockSeparator>; 2],
    separators_valid: [bool; 2],
}

struct PreparedWindowInputs {
    values: Vec<WindowInput>,
    work: usize,
}

struct WindowMaps {
    by_position: [Vec<Option<usize>>; 2],
}

#[derive(Clone, Copy)]
struct SpanWindow {
    first: Option<usize>,
    invalid: bool,
}

struct SideEvidence {
    tokens: Vec<ComparableToken>,
    span: Option<TextSpan>,
}

enum WindowEvidence {
    Ready(SideEvidence),
    Skip,
    Abort,
}

#[derive(Clone, Copy, Default)]
enum BoundarySeparator {
    #[default]
    Unknown,
    Concatenate,
    Space,
    Conflict,
}
/// Proves content presence changes inside windows bounded by consecutive main anchors.
///
/// Returned spans are context envelopes for the anchor-bounded windows. They do not
/// claim exact localization, and callers must keep the corresponding unresolved regions.
/// None means the complete batch could not be proven within its resource or projection
/// bounds; no partial batch may be published.
pub(super) fn plan_anchor_bounded_changed_regions(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    changes: &[ChangeEvent],
    unresolved_regions: &[UnresolvedRegion],
    ledgers: &[RecoveryOwnershipLedger; 2],
    max_tokens: usize,
) -> Option<Vec<ProvenChangedRegion>> {
    let [old, new] = sides;
    if alignment
        .spans
        .iter()
        .any(|span| span.evidence.contains(&AlignmentEvidence::ExtractionGap))
        || unresolved_regions
            .iter()
            .any(|region| region.evidence.contains(&AlignmentEvidence::ExtractionGap))
    {
        return Some(Vec::new());
    }
    let windows = anchor_windows(old, new, alignment)?;
    let inputs = prepare_window_inputs(
        [old, new],
        &windows,
        alignment,
        changes,
        unresolved_regions,
        ledgers,
        max_tokens,
    )?;
    debug_assert!(inputs.work <= max_tokens);
    let mut planned = Vec::new();
    planned.try_reserve_exact(windows.len()).ok()?;
    let mut examined_tokens = 0usize;

    for (input, window) in inputs.values.into_iter().zip(&windows) {
        if input.blocked || !input.has_unresolved {
            continue;
        }

        if !input.separators_valid[0] || !input.separators_valid[1] {
            continue;
        }
        let old_evidence = match collect_side_evidence(
            old,
            &ledgers[0],
            &input.range_indices[0],
            &window.old,
            input.separators[0],
            &mut examined_tokens,
            max_tokens,
        ) {
            WindowEvidence::Ready(evidence) => evidence,
            WindowEvidence::Skip => continue,
            WindowEvidence::Abort => return None,
        };
        let new_evidence = match collect_side_evidence(
            new,
            &ledgers[1],
            &input.range_indices[1],
            &window.new,
            input.separators[1],
            &mut examined_tokens,
            max_tokens,
        ) {
            WindowEvidence::Ready(evidence) => evidence,
            WindowEvidence::Skip => continue,
            WindowEvidence::Abort => return None,
        };

        let proof = match (
            old_evidence.tokens.is_empty(),
            new_evidence.tokens.is_empty(),
        ) {
            (true, true) => continue,
            (true, false) | (false, true) => ChangedRegionProof::OneSidedNonEmptyRange,
            (false, false) if old_evidence.tokens == new_evidence.tokens => continue,
            (false, false) => ChangedRegionProof::ExactTokenMultisetMismatch,
        };
        planned.push(ProvenChangedRegion {
            old_span: old_evidence.span,
            new_span: new_evidence.span,
            proof,
            confidence: Confidence::High,
        });
    }

    Some(planned)
}

fn anchor_windows(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
) -> Option<Vec<AnchorWindow>> {
    let window_count = alignment.main_anchors.len().saturating_sub(1);
    let mut windows = Vec::new();
    windows.try_reserve_exact(window_count).ok()?;
    for anchors in alignment.main_anchors.windows(2) {
        let old_left = *old.index.get(&anchors[0].old)?;
        let old_right = *old.index.get(&anchors[1].old)?;
        let new_left = *new.index.get(&anchors[0].new)?;
        let new_right = *new.index.get(&anchors[1].new)?;
        let old_start = old_left.checked_add(1)?;
        let new_start = new_left.checked_add(1)?;
        if old_start > old_right || new_start > new_right {
            return None;
        }
        windows.push(AnchorWindow {
            old: old_start..old_right,
            new: new_start..new_right,
        });
    }
    Some(windows)
}

fn prepare_window_inputs(
    sides: [&Side<'_>; 2],
    windows: &[AnchorWindow],
    alignment: &Alignment,
    changes: &[ChangeEvent],
    unresolved_regions: &[UnresolvedRegion],
    ledgers: &[RecoveryOwnershipLedger; 2],
    max_work: usize,
) -> Option<PreparedWindowInputs> {
    let mut work = 0usize;
    let maps = WindowMaps {
        by_position: [
            build_window_map(
                sides[0].index.len(),
                windows.iter().map(|window| &window.old),
                &mut work,
                max_work,
            )?,
            build_window_map(
                sides[1].index.len(),
                windows.iter().map(|window| &window.new),
                &mut work,
                max_work,
            )?,
        ],
    };
    let mut inputs = Vec::new();
    inputs.try_reserve_exact(windows.len()).ok()?;
    inputs.resize_with(windows.len(), WindowInput::default);
    let boundaries =
        build_boundary_separators(sides, alignment, unresolved_regions, &mut work, max_work)?;
    assign_window_separators(&mut inputs, windows, &boundaries, &mut work, max_work)?;

    for side_index in 0..2 {
        for (range_index, range) in ledgers[side_index].ranges.iter().enumerate() {
            charge_work(&mut work, 1, max_work)?;
            let block = ledgers[side_index].blocks.get(range.block_index)?;
            let position = *sides[side_index].index.get(&BlockId(block.block_id))?;
            let Some(window_index) = *maps.by_position[side_index].get(position)? else {
                continue;
            };
            let ranges = &mut inputs.get_mut(window_index)?.range_indices[side_index];
            ranges.try_reserve(1).ok()?;
            ranges.push(range_index);
        }
    }

    for change in changes {
        for occurrence in &change.occurrences {
            mark_span_windows(
                occurrence.old_span.as_ref(),
                sides[0],
                &maps.by_position[0],
                &mut inputs,
                &mut work,
                max_work,
            )?;
            mark_span_windows(
                occurrence.new_span.as_ref(),
                sides[1],
                &maps.by_position[1],
                &mut inputs,
                &mut work,
                max_work,
            )?;
        }
    }

    for region in unresolved_regions {
        charge_work(&mut work, region.evidence.len(), max_work)?;
        let old_span = classify_span_window(
            region.old_span.as_ref(),
            sides[0],
            &maps.by_position[0],
            &mut work,
            max_work,
        )?;
        let new_span = classify_span_window(
            region.new_span.as_ref(),
            sides[1],
            &maps.by_position[1],
            &mut work,
            max_work,
        )?;
        let same_window = match (old_span.first, new_span.first) {
            (None, None) => continue,
            (Some(old), Some(new)) => old == new,
            _ => true,
        };
        let safe_evidence = !region.evidence.is_empty()
            && !region.evidence.contains(&AlignmentEvidence::ExtractionGap)
            && !region
                .evidence
                .contains(&AlignmentEvidence::NormalizationIssue);
        if old_span.invalid || new_span.invalid || !same_window || !safe_evidence {
            mark_span_windows(
                region.old_span.as_ref(),
                sides[0],
                &maps.by_position[0],
                &mut inputs,
                &mut work,
                max_work,
            )?;
            mark_span_windows(
                region.new_span.as_ref(),
                sides[1],
                &maps.by_position[1],
                &mut inputs,
                &mut work,
                max_work,
            )?;
        } else if let Some(window) = old_span.first.or(new_span.first) {
            inputs.get_mut(window)?.has_unresolved = true;
        }
    }
    Some(PreparedWindowInputs {
        values: inputs,
        work,
    })
}

fn build_window_map<'a>(
    positions: usize,
    ranges: impl Iterator<Item = &'a Range<usize>>,
    work: &mut usize,
    max_work: usize,
) -> Option<Vec<Option<usize>>> {
    let mut by_position = Vec::new();
    by_position.try_reserve_exact(positions).ok()?;
    by_position.resize(positions, None);
    for (window_index, range) in ranges.enumerate() {
        for position in range.clone() {
            charge_work(work, 1, max_work)?;
            let entry = by_position.get_mut(position)?;
            if entry.replace(window_index).is_some() {
                return None;
            }
        }
    }
    Some(by_position)
}

fn classify_span_window(
    span: Option<&TextSpan>,
    side: &Side<'_>,
    by_position: &[Option<usize>],
    work: &mut usize,
    max_work: usize,
) -> Option<SpanWindow> {
    let Some(span) = span else {
        return Some(SpanWindow {
            first: None,
            invalid: false,
        });
    };
    if span.blocks.is_empty() {
        return None;
    }
    let mut first = None;
    let mut outside = false;
    let mut multiple = false;
    for block in &span.blocks {
        charge_work(work, 1, max_work)?;
        let position = *side.index.get(block)?;
        match *by_position.get(position)? {
            Some(window) => {
                if let Some(first_window) = first {
                    multiple |= first_window != window;
                } else {
                    first = Some(window);
                }
            }
            None => outside = true,
        }
    }
    Some(SpanWindow {
        first,
        invalid: first.is_some() && (outside || multiple),
    })
}

fn mark_span_windows(
    span: Option<&TextSpan>,
    side: &Side<'_>,
    by_position: &[Option<usize>],
    inputs: &mut [WindowInput],
    work: &mut usize,
    max_work: usize,
) -> Option<()> {
    let Some(span) = span else {
        return Some(());
    };
    for block in &span.blocks {
        charge_work(work, 1, max_work)?;
        let position = *side.index.get(block)?;
        if let Some(window) = *by_position.get(position)? {
            inputs.get_mut(window)?.blocked = true;
        }
    }
    Some(())
}

fn charge_work(work: &mut usize, amount: usize, limit: usize) -> Option<()> {
    let next = work.checked_add(amount)?;
    if next > limit {
        return None;
    }
    *work = next;
    Some(())
}

fn build_boundary_separators(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    unresolved_regions: &[UnresolvedRegion],
    work: &mut usize,
    max_work: usize,
) -> Option<[Vec<BoundarySeparator>; 2]> {
    let mut boundaries = [
        allocate_boundary_map(sides[0].index.len())?,
        allocate_boundary_map(sides[1].index.len())?,
    ];
    for span in &alignment.spans {
        record_boundary_separators(
            sides[0],
            &span.old,
            span.old_separator,
            &mut boundaries[0],
            work,
            max_work,
        )?;
        record_boundary_separators(
            sides[1],
            &span.new,
            span.new_separator,
            &mut boundaries[1],
            work,
            max_work,
        )?;
    }
    for region in unresolved_regions {
        if let Some(span) = &region.old_span {
            record_boundary_separators(
                sides[0],
                &span.blocks,
                span.separator,
                &mut boundaries[0],
                work,
                max_work,
            )?;
        }
        if let Some(span) = &region.new_span {
            record_boundary_separators(
                sides[1],
                &span.blocks,
                span.separator,
                &mut boundaries[1],
                work,
                max_work,
            )?;
        }
    }
    Some(boundaries)
}

fn allocate_boundary_map(positions: usize) -> Option<Vec<BoundarySeparator>> {
    let length = positions.saturating_sub(1);
    let mut boundaries = Vec::new();
    boundaries.try_reserve_exact(length).ok()?;
    boundaries.resize(length, BoundarySeparator::Unknown);
    Some(boundaries)
}

fn record_boundary_separators(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
    boundaries: &mut [BoundarySeparator],
    work: &mut usize,
    max_work: usize,
) -> Option<()> {
    let evidence = match separator.unwrap_or(BlockSeparator::Concatenate) {
        BlockSeparator::Concatenate => BoundarySeparator::Concatenate,
        BlockSeparator::Space => BoundarySeparator::Space,
    };
    for pair in blocks.windows(2) {
        charge_work(work, 1, max_work)?;
        let left = *side.index.get(&pair[0])?;
        let right = *side.index.get(&pair[1])?;
        if right != left.checked_add(1)? {
            continue;
        }
        let boundary = boundaries.get_mut(left)?;
        *boundary = match (*boundary, evidence) {
            (BoundarySeparator::Unknown, value) => value,
            (BoundarySeparator::Concatenate, BoundarySeparator::Concatenate)
            | (BoundarySeparator::Space, BoundarySeparator::Space) => evidence,
            _ => BoundarySeparator::Conflict,
        };
    }
    Some(())
}

fn assign_window_separators(
    inputs: &mut [WindowInput],
    windows: &[AnchorWindow],
    boundaries: &[Vec<BoundarySeparator>; 2],
    work: &mut usize,
    max_work: usize,
) -> Option<()> {
    for (input, window) in inputs.iter_mut().zip(windows) {
        for (side_index, range) in [window.old.clone(), window.new.clone()]
            .into_iter()
            .enumerate()
        {
            let (valid, separator) =
                uniform_window_separator(range, &boundaries[side_index], work, max_work)?;
            input.separators_valid[side_index] = valid;
            input.separators[side_index] = separator;
        }
    }
    Some(())
}

fn uniform_window_separator(
    window: Range<usize>,
    boundaries: &[BoundarySeparator],
    work: &mut usize,
    max_work: usize,
) -> Option<(bool, Option<BlockSeparator>)> {
    if window.len() <= 1 {
        return Some((true, None));
    }
    let boundary_end = window.end.checked_sub(1)?;
    let mut selected = None;
    for boundary in boundaries.get(window.start..boundary_end)? {
        charge_work(work, 1, max_work)?;
        let separator = match boundary {
            BoundarySeparator::Unknown => BlockSeparator::Space,
            BoundarySeparator::Concatenate => BlockSeparator::Concatenate,
            BoundarySeparator::Space => BlockSeparator::Space,
            BoundarySeparator::Conflict => return Some((false, None)),
        };
        if selected.is_some_and(|current| current != separator) {
            return Some((false, None));
        }
        selected = Some(separator);
    }
    Some((true, selected))
}

fn collect_side_evidence(
    side: &Side<'_>,
    ledger: &RecoveryOwnershipLedger,
    range_indices: &[usize],
    window: &Range<usize>,
    separator: Option<BlockSeparator>,
    examined_tokens: &mut usize,
    max_tokens: usize,
) -> WindowEvidence {
    let mut token_count = 0usize;
    let Some(source_blocks) = side.blocks.get(window.clone()) else {
        return WindowEvidence::Abort;
    };
    let mut window_blocks = Vec::new();
    if window_blocks
        .try_reserve_exact(source_blocks.len())
        .is_err()
    {
        return WindowEvidence::Abort;
    }
    for (offset, block) in source_blocks.iter().enumerate() {
        if side.index.get(&block.block) != window.start.checked_add(offset).as_ref() {
            return WindowEvidence::Abort;
        }
        window_blocks.push(block.block);
    }
    for range_index in range_indices {
        let Some(range) = ledger.ranges.get(*range_index) else {
            return WindowEvidence::Abort;
        };
        let Some(block) = ledger.blocks.get(range.block_index) else {
            return WindowEvidence::Abort;
        };
        let Some(position) = side.index.get(&BlockId(block.block_id)) else {
            return WindowEvidence::Abort;
        };
        if !window.contains(position) {
            return WindowEvidence::Abort;
        }
        if range.ownership == RecoveryOwnership::Accepted {
            continue;
        }
        if is_unsafe_gap(range.ownership) {
            return WindowEvidence::Skip;
        }
        let Some(length) = range.comparable_end.checked_sub(range.comparable_start) else {
            return WindowEvidence::Abort;
        };
        token_count = match token_count.checked_add(length) {
            Some(count) => count,
            None => return WindowEvidence::Abort,
        };
        if side.canonical.get(*position).is_none() {
            return WindowEvidence::Abort;
        }
    }

    let next_examined = match examined_tokens.checked_add(token_count) {
        Some(count) if count <= max_tokens => count,
        _ => return WindowEvidence::Abort,
    };
    *examined_tokens = next_examined;

    let mut tokens = Vec::new();
    if tokens.try_reserve_exact(token_count).is_err() {
        return WindowEvidence::Abort;
    }
    for range_index in range_indices {
        let Some(range) = ledger.ranges.get(*range_index) else {
            return WindowEvidence::Abort;
        };
        let Some(block) = ledger.blocks.get(range.block_index) else {
            return WindowEvidence::Abort;
        };
        let Some(position) = side.index.get(&BlockId(block.block_id)) else {
            return WindowEvidence::Abort;
        };
        if range.ownership == RecoveryOwnership::Accepted {
            continue;
        }
        let Some(block_tokens) = side.canonical.get(*position) else {
            return WindowEvidence::Abort;
        };
        let Some(range_tokens) = block_tokens.get(range.comparable_start..range.comparable_end)
        else {
            return WindowEvidence::Abort;
        };
        tokens.extend(
            range_tokens
                .iter()
                .filter(|token| !is_whitespace(token))
                .cloned(),
        );
    }
    tokens.sort_unstable();

    let span = if tokens.is_empty() {
        None
    } else {
        match try_group_full_span(side, &window_blocks, separator) {
            Some(span) => Some(span),
            None => return WindowEvidence::Abort,
        }
    };
    WindowEvidence::Ready(SideEvidence { tokens, span })
}

fn is_whitespace(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn is_unsafe_gap(ownership: RecoveryOwnership) -> bool {
    matches!(
        ownership,
        RecoveryOwnership::Gap(
            RecoveryGapReason::LocationProjectionFailed
                | RecoveryGapReason::NormalizationIssue
                | RecoveryGapReason::UnmappedChangedEvidence
        )
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{
        alignment::{AlignmentConfidence, AlignmentKind, AlignmentSpan, ExactAnchor},
        diff::{
            ChangeKind, ChangeOccurrence, TokenRange,
            recovery::ownership::{
                RecoveryLeafKind, RecoveryOwnershipContext, RecoveryOwnershipLedgerBlock,
                RecoveryOwnershipLedgerRange, RecoveryOwnershipRole,
            },
        },
        layout::BlockRole,
        normalize::{BlockText, MappedText, ScalarRange},
    };

    use super::*;

    fn side(entries: &[(u64, &str)]) -> Side<'static> {
        let blocks = entries
            .iter()
            .map(|(id, text)| BlockText {
                block: BlockId(*id),
                role: BlockRole::Body,
                raw: mapped(text),
                canonical: mapped(text),
                matching: (*text).to_owned(),
                matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
                numeric_mask_applied: false,
                normalization_events: Vec::new(),
                issues: Vec::new(),
                pages: Vec::new(),
                font_size_signatures: None,
                position_signatures: None,
                line_breaks: None,
                page_breaks: None,
            })
            .collect::<Vec<_>>();
        let blocks = Box::leak(blocks.into_boxed_slice());
        let index = blocks
            .iter()
            .enumerate()
            .map(|(position, block)| (block.block, position))
            .collect::<HashMap<_, _>>();
        let canonical = blocks
            .iter()
            .map(|block| {
                block
                    .canonical
                    .comparable_tokens()
                    .expect("test canonical text is valid")
            })
            .collect::<Vec<_>>();
        let total_tokens = canonical.iter().map(Vec::len).sum();
        Side {
            blocks,
            index,
            canonical,
            total_tokens,
        }
    }

    fn mapped(text: &str) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        }
    }

    fn ledger(side: &Side<'_>, content: &[(u64, RecoveryOwnership)]) -> RecoveryOwnershipLedger {
        let blocks = content
            .iter()
            .map(|(id, _)| RecoveryOwnershipLedgerBlock {
                block_id: *id,
                trusted: true,
                role: RecoveryOwnershipRole::Body,
                context: RecoveryOwnershipContext::default(),
            })
            .collect();
        let ranges = content
            .iter()
            .enumerate()
            .map(|(block_index, (id, ownership))| {
                let length = side.canonical[side.index[&BlockId(*id)]].len();
                RecoveryOwnershipLedgerRange {
                    block_index,
                    canonical_start: 0,
                    canonical_end: length,
                    comparable_start: 0,
                    comparable_end: length,
                    ownership: *ownership,
                }
            })
            .collect();
        RecoveryOwnershipLedger { blocks, ranges }
    }

    fn alignment(old_content: &[BlockId], new_content: &[BlockId]) -> Alignment {
        Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: old_content.to_vec(),
                new: new_content.to_vec(),
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: None,
                new_separator: None,
            }],
            main_anchors: vec![
                ExactAnchor {
                    old: BlockId(1),
                    new: BlockId(11),
                },
                ExactAnchor {
                    old: BlockId(3),
                    new: BlockId(13),
                },
            ],
            move_candidates: Vec::new(),
        }
    }

    fn span(blocks: Vec<BlockId>, token_count: usize) -> TextSpan {
        TextSpan {
            blocks,
            separator: None,
            canonical_range: ScalarRange {
                start: 0,
                end: token_count,
            },
            comparable_range: TokenRange {
                start: 0,
                end: token_count,
            },
        }
    }

    fn unresolved(old_blocks: Vec<BlockId>, new_blocks: Vec<BlockId>) -> UnresolvedRegion {
        let old_len = old_blocks.len();
        let new_len = new_blocks.len();
        UnresolvedRegion {
            old_span: (!old_blocks.is_empty()).then(|| span(old_blocks, old_len)),
            new_span: (!new_blocks.is_empty()).then(|| span(new_blocks, new_len)),
            evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
        }
    }

    fn leaf() -> RecoveryOwnership {
        RecoveryOwnership::Leaf(RecoveryLeafKind::TrustedRunResidual)
    }

    #[test]
    fn proves_mismatched_multisets_inside_bounded_window() {
        let old = side(&[(1, "left"), (2, "alpha"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "omega"), (13, "right")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let ledgers = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];

        let planned = plan_anchor_bounded_changed_regions(
            [&old, &new],
            &alignment,
            &[],
            &regions,
            &ledgers,
            100,
        )
        .expect("presence proof planning should complete");

        assert_eq!(planned.len(), 1);
        assert_eq!(
            planned[0].proof,
            ChangedRegionProof::ExactTokenMultisetMismatch
        );
        assert_eq!(
            planned[0]
                .old_span
                .as_ref()
                .expect("mismatch proof should retain old context")
                .blocks,
            [BlockId(2)]
        );
        assert_eq!(
            planned[0]
                .new_span
                .as_ref()
                .expect("mismatch proof should retain new context")
                .blocks,
            [BlockId(12)]
        );
    }

    #[test]
    fn ignores_equal_multisets_even_when_order_differs() {
        let old = side(&[(1, "left"), (2, "ab"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "ba"), (13, "right")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let ledgers = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];

        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &regions,
                &ledgers,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );
    }

    #[test]
    fn ignores_role_reclassification_when_tokens_are_identical() {
        let old = side(&[(1, "left"), (2, "stable"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "stable"), (13, "right")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];

        for role in [
            RecoveryOwnershipRole::RepeatedHeader,
            RecoveryOwnershipRole::RepeatedFooter,
        ] {
            let old_ledger = ledger(&old, &[(2, leaf())]);
            let mut new_ledger = ledger(&new, &[(12, leaf())]);
            new_ledger.blocks[0].role = role;

            assert!(
                plan_anchor_bounded_changed_regions(
                    [&old, &new],
                    &alignment,
                    &[],
                    &regions,
                    &[old_ledger, new_ledger],
                    100,
                )
                .expect("presence proof planning should complete")
                .is_empty()
            );
        }
    }

    #[test]
    fn ignores_whitespace_inserted_by_block_splitting() {
        let old = side(&[(1, "left"), (2, "alpha beta"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "alpha"), (13, "beta"), (14, "right")]);
        let mut alignment = alignment(&[BlockId(2)], &[BlockId(12), BlockId(13)]);
        alignment.main_anchors[1].new = BlockId(14);
        let ledgers = [
            ledger(&old, &[(2, leaf())]),
            ledger(&new, &[(12, leaf()), (13, leaf())]),
        ];
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12), BlockId(13)])];

        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &regions,
                &ledgers,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );
    }

    #[test]
    fn spans_the_complete_window_with_readable_block_separation() {
        let old = side(&[
            (1, "left"),
            (2, "alpha"),
            (3, " "),
            (4, "beta"),
            (5, "right"),
        ]);
        let new = side(&[
            (11, "left"),
            (12, "alpha"),
            (13, " "),
            (14, "omega"),
            (15, "right"),
        ]);
        let mut alignment = alignment(
            &[BlockId(2), BlockId(3), BlockId(4)],
            &[BlockId(12), BlockId(13), BlockId(14)],
        );
        alignment.main_anchors[1].old = BlockId(5);
        alignment.main_anchors[1].new = BlockId(15);
        alignment.spans[0].old_separator = Some(BlockSeparator::Space);
        alignment.spans[0].new_separator = Some(BlockSeparator::Space);
        let ledgers = [
            ledger(
                &old,
                &[(2, leaf()), (3, RecoveryOwnership::Accepted), (4, leaf())],
            ),
            ledger(
                &new,
                &[
                    (12, leaf()),
                    (13, RecoveryOwnership::Accepted),
                    (14, leaf()),
                ],
            ),
        ];
        let mut regions = [unresolved(
            vec![BlockId(2), BlockId(3), BlockId(4)],
            vec![BlockId(12), BlockId(13), BlockId(14)],
        )];
        regions[0]
            .old_span
            .as_mut()
            .expect("old context exists")
            .separator = Some(BlockSeparator::Space);
        regions[0]
            .new_span
            .as_mut()
            .expect("new context exists")
            .separator = Some(BlockSeparator::Space);

        let planned = plan_anchor_bounded_changed_regions(
            [&old, &new],
            &alignment,
            &[],
            &regions,
            &ledgers,
            500,
        )
        .expect("presence proof planning should complete");
        let old_span = planned[0]
            .old_span
            .as_ref()
            .expect("old context should exist");
        assert_eq!(old_span.blocks, [BlockId(2), BlockId(3), BlockId(4)]);
        assert_eq!(old_span.separator, Some(BlockSeparator::Space));
        assert_eq!(old_span.comparable_range.end, 10);
        let new_span = planned[0]
            .new_span
            .as_ref()
            .expect("new context should exist");
        assert_eq!(new_span.blocks, [BlockId(12), BlockId(13), BlockId(14)]);
        assert_eq!(new_span.separator, Some(BlockSeparator::Space));
        assert_eq!(new_span.comparable_range.end, 11);
    }

    #[test]
    fn preserves_concatenate_for_multi_block_proof_context() {
        let old = side(&[(1, "left"), (2, "ab"), (3, "cd"), (4, "right")]);
        let new = side(&[(11, "left"), (12, "ab"), (13, "XY"), (14, "right")]);
        let mut alignment = alignment(&[BlockId(2), BlockId(3)], &[BlockId(12), BlockId(13)]);
        alignment.main_anchors[1].old = BlockId(4);
        alignment.main_anchors[1].new = BlockId(14);
        alignment.spans[0].old_separator = Some(BlockSeparator::Concatenate);
        alignment.spans[0].new_separator = Some(BlockSeparator::Concatenate);
        let ledgers = [
            ledger(&old, &[(2, leaf()), (3, leaf())]),
            ledger(&new, &[(12, leaf()), (13, leaf())]),
        ];
        let mut regions = [unresolved(
            vec![BlockId(2), BlockId(3)],
            vec![BlockId(12), BlockId(13)],
        )];
        regions[0]
            .old_span
            .as_mut()
            .expect("old context exists")
            .separator = Some(BlockSeparator::Concatenate);
        regions[0]
            .new_span
            .as_mut()
            .expect("new context exists")
            .separator = Some(BlockSeparator::Concatenate);

        let planned = plan_anchor_bounded_changed_regions(
            [&old, &new],
            &alignment,
            &[],
            &regions,
            &ledgers,
            200,
        )
        .expect("presence proof planning should complete");

        assert_eq!(planned.len(), 1);
        let old_span = planned[0]
            .old_span
            .as_ref()
            .expect("old context should exist");
        assert_eq!(old_span.blocks, [BlockId(2), BlockId(3)]);
        assert_eq!(old_span.separator, Some(BlockSeparator::Concatenate));
        assert_eq!(old_span.comparable_range.end, 4);
    }

    #[test]
    fn includes_resolved_non_ledger_block_in_contiguous_envelope() {
        let old = side(&[
            (1, "left"),
            (2, "alpha"),
            (3, "stable"),
            (4, "beta"),
            (5, "right"),
        ]);
        let new = side(&[
            (11, "left"),
            (12, "alpha"),
            (13, "stable"),
            (14, "omega"),
            (15, "right"),
        ]);
        let mut alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        alignment.main_anchors[1].old = BlockId(5);
        alignment.main_anchors[1].new = BlockId(15);
        let single_span = |kind, old, new, evidence| AlignmentSpan {
            kind,
            old: vec![old],
            new: vec![new],
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence,
            old_separator: None,
            new_separator: None,
        };
        alignment.spans = vec![
            single_span(
                AlignmentKind::Unresolved,
                BlockId(2),
                BlockId(12),
                vec![AlignmentEvidence::ReadingOrderUnknown],
            ),
            single_span(AlignmentKind::Match, BlockId(3), BlockId(13), Vec::new()),
            single_span(
                AlignmentKind::Unresolved,
                BlockId(4),
                BlockId(14),
                vec![AlignmentEvidence::ReadingOrderUnknown],
            ),
        ];
        let ledgers = [
            ledger(&old, &[(2, leaf()), (4, leaf())]),
            ledger(&new, &[(12, leaf()), (14, leaf())]),
        ];
        let regions = [
            unresolved(vec![BlockId(2)], vec![BlockId(12)]),
            unresolved(vec![BlockId(4)], vec![BlockId(14)]),
        ];

        let planned = plan_anchor_bounded_changed_regions(
            [&old, &new],
            &alignment,
            &[],
            &regions,
            &ledgers,
            300,
        )
        .expect("presence proof planning should complete");

        assert_eq!(planned.len(), 1);
        assert_eq!(
            planned[0]
                .old_span
                .as_ref()
                .expect("old context should exist")
                .blocks,
            [BlockId(2), BlockId(3), BlockId(4)]
        );
        assert_eq!(
            planned[0]
                .new_span
                .as_ref()
                .expect("new context should exist")
                .blocks,
            [BlockId(12), BlockId(13), BlockId(14)]
        );
        assert_eq!(
            planned[0]
                .old_span
                .as_ref()
                .expect("old context should exist")
                .separator,
            Some(BlockSeparator::Space)
        );
    }
    #[test]
    fn preprocessing_work_is_linear_and_bounded() {
        let old = side(&[(1, "left"), (2, "alpha"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "omega"), (13, "right")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let windows = anchor_windows(&old, &new, &alignment)
            .expect("bounded anchors should define one window");
        let ledgers = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];

        let prepared = prepare_window_inputs(
            [&old, &new],
            &windows,
            &alignment,
            &[],
            &regions,
            &ledgers,
            7,
        )
        .expect("each position, range, evidence item, and span block should be visited once");
        assert_eq!(prepared.work, 7);
        assert!(
            prepare_window_inputs(
                [&old, &new],
                &windows,
                &alignment,
                &[],
                &regions,
                &ledgers,
                6
            )
            .is_none()
        );
    }

    #[test]
    fn proves_one_sided_non_empty_window() {
        let old = side(&[(1, "left"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "added"), (13, "right")]);
        let alignment = alignment(&[], &[BlockId(12)]);
        let ledgers = [
            RecoveryOwnershipLedger::default(),
            ledger(&new, &[(12, leaf())]),
        ];
        let regions = [unresolved(Vec::new(), vec![BlockId(12)])];

        let planned = plan_anchor_bounded_changed_regions(
            [&old, &new],
            &alignment,
            &[],
            &regions,
            &ledgers,
            100,
        )
        .expect("presence proof planning should complete");

        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].proof, ChangedRegionProof::OneSidedNonEmptyRange);
        assert!(planned[0].old_span.is_none());
        assert!(planned[0].new_span.is_some());
    }

    #[test]
    fn skips_windows_with_existing_changes_or_unsafe_evidence() {
        let old = side(&[(1, "left"), (2, "alpha"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "omega"), (13, "right")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];
        let change = ChangeEvent {
            kind: ChangeKind::Replacement,
            occurrences: vec![ChangeOccurrence {
                old_span: Some(span(vec![BlockId(2)], 5)),
                new_span: Some(span(vec![BlockId(12)], 5)),
            }],
            confidence: Confidence::High,
            tags: Vec::new(),
        };
        let safe = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[change],
                &regions,
                &safe,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );

        let unsafe_ledgers = [
            ledger(
                &old,
                &[(
                    2,
                    RecoveryOwnership::Gap(RecoveryGapReason::NormalizationIssue),
                )],
            ),
            ledger(&new, &[(12, leaf())]),
        ];
        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &regions,
                &unsafe_ledgers,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );
    }

    #[test]
    fn excludes_accepted_ranges_and_rejects_external_unresolved_spans() {
        let old = side(&[(1, "left"), (2, "alpha"), (3, "right"), (4, "outside")]);
        let new = side(&[(11, "left"), (12, "omega"), (13, "right"), (14, "outside")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let accepted = [
            ledger(&old, &[(2, RecoveryOwnership::Accepted)]),
            ledger(&new, &[(12, RecoveryOwnership::Accepted)]),
        ];
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];
        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &regions,
                &accepted,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );

        let ledgers = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        let crossing = [unresolved(
            vec![BlockId(2), BlockId(4)],
            vec![BlockId(12), BlockId(14)],
        )];
        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &crossing,
                &ledgers,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );
    }

    #[test]
    fn aborts_the_whole_batch_when_budget_is_exhausted() {
        let old = side(&[(1, "left"), (2, "alpha"), (3, "right")]);
        let new = side(&[(11, "left"), (12, "omega"), (13, "right")]);
        let alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        let ledgers = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        let regions = [unresolved(vec![BlockId(2)], vec![BlockId(12)])];

        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &regions,
                &ledgers,
                9,
            )
            .is_none()
        );
    }

    #[test]
    fn ignores_unbounded_regions_and_document_wide_extraction_gaps() {
        let old = side(&[(1, "alpha")]);
        let new = side(&[(11, "omega")]);
        let unbounded_alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let ledgers = [ledger(&old, &[(1, leaf())]), ledger(&new, &[(11, leaf())])];
        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &unbounded_alignment,
                &[],
                &[],
                &ledgers,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );

        let old = side(&[(1, "left"), (2, "alpha"), (3, "right"), (4, "gap")]);
        let new = side(&[(11, "left"), (12, "omega"), (13, "right"), (14, "gap")]);
        let mut alignment = alignment(&[BlockId(2)], &[BlockId(12)]);
        alignment.spans.push(AlignmentSpan {
            kind: AlignmentKind::Unresolved,
            old: vec![BlockId(4)],
            new: vec![BlockId(14)],
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: vec![AlignmentEvidence::ExtractionGap],
            old_separator: None,
            new_separator: None,
        });
        let ledgers = [ledger(&old, &[(2, leaf())]), ledger(&new, &[(12, leaf())])];
        let safe_window = unresolved(vec![BlockId(2)], vec![BlockId(12)]);
        assert!(
            plan_anchor_bounded_changed_regions(
                [&old, &new],
                &alignment,
                &[],
                &[safe_window],
                &ledgers,
                100,
            )
            .expect("presence proof planning should complete")
            .is_empty()
        );
    }
}
