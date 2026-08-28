mod myers;
mod sentence;

/// Keeps the retained Myers frontier and trace below the internal 64 MiB
/// allocation budget while allowing benchmark runs to exceed the default.
pub(crate) const MAX_MYERS_EDIT_DISTANCE: usize = 4_000;

use std::collections::HashMap;

use crate::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        BlockSeparator,
    },
    layout::{BlockId, TrustedRunInterval},
    model::Vec2,
    normalize::{
        BlockText, ComparableToken, FontSizeSignature, PositionSignature, ScalarRange,
        character_width_fold,
    },
    validate::validate_unit_interval,
};

use self::myers::Edit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Replacement,
    Insertion,
    Deletion,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeTag {
    CharacterWidth,
    OcrConfusion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub blocks: Vec<BlockId>,
    pub separator: Option<BlockSeparator>,
    pub canonical_range: ScalarRange,
    pub comparable_range: TokenRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub old_span: Option<TextSpan>,
    pub new_span: Option<TextSpan>,
    pub confidence: Confidence,
    pub tags: Vec<ChangeTag>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormattingReason {
    Normalization,
    BlockStructure,
    FontSize,
    Position,
    LineBreak,
    PageBreak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattingChange {
    pub old_span: TextSpan,
    pub new_span: TextSpan,
    pub confidence: Confidence,
    pub reasons: Vec<FormattingReason>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnresolvedRegion {
    pub old_span: Option<TextSpan>,
    pub new_span: Option<TextSpan>,
    pub evidence: Vec<AlignmentEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coverage {
    pub resolved_tokens: usize,
    pub total_tokens: usize,
    pub ratio: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    pub changes: Vec<Change>,
    pub formatting_changes: Vec<FormattingChange>,
    pub unresolved_regions: Vec<UnresolvedRegion>,
    pub old_coverage: Coverage,
    pub new_coverage: Coverage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffOptions {
    /// Maximum comparable or raw evidence tokens across both document sides.
    pub max_tokens: usize,
    /// Maximum Myers edit distance before a matched span becomes unresolved.
    ///
    /// Values above the implementation cap are rejected because retained
    /// backtracking trace memory grows quadratically with this value.
    pub max_edit_distance: usize,
    /// Weak (low-confidence) matched spans whose bounded Myers diff changes
    /// more than this fraction of tokens degrade to unresolved regions
    /// instead of emitting fragmented character-level changes.
    pub max_weak_match_change_ratio: f64,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            // The measured Unicode Standard corpus peaks at 5,001,224 post-layout raw tokens.
            max_tokens: 5_100_000,
            max_edit_distance: 2_048,
            max_weak_match_change_ratio: 0.5,
        }
    }
}

pub(crate) fn enforce_diff_token_budget(
    old: &[BlockText],
    new: &[BlockText],
    options: DiffOptions,
) -> Result<()> {
    inspect_sides_with_budget(old, new, options).map(|_| ())
}

pub(crate) fn validate_diff_options(options: DiffOptions) -> Result<()> {
    if options.max_tokens == 0 {
        return Err(Error::InvalidConfiguration(
            "diff max_tokens must be greater than zero".to_owned(),
        ));
    }
    if options.max_edit_distance > MAX_MYERS_EDIT_DISTANCE {
        return Err(Error::InvalidConfiguration(format!(
            "diff max_edit_distance must not exceed {MAX_MYERS_EDIT_DISTANCE} because the bounded Myers trace has a 64 MiB allocation budget"
        )));
    }
    validate_unit_interval(
        "max_weak_match_change_ratio",
        options.max_weak_match_change_ratio,
    )?;
    Ok(())
}

pub(crate) fn enforce_diff_raw_token_budget(
    old_tokens: usize,
    new_tokens: usize,
    options: DiffOptions,
) -> Result<()> {
    validate_diff_options(options)?;
    enforce_combined_token_budget(
        "diff raw evidence tokens",
        old_tokens,
        new_tokens,
        options.max_tokens,
    )
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SentenceRecoveryInput<'a> {
    pub(crate) old_trusted_run_intervals: &'a [Option<TrustedRunInterval>],
    pub(crate) new_trusted_run_intervals: &'a [Option<TrustedRunInterval>],
    pub(crate) min_tokens: usize,
}

pub fn compare_aligned(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
) -> Result<Comparison> {
    compare_aligned_inner(old, new, alignment, options, None)
}

pub(crate) fn compare_aligned_with_sentence_recovery(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
    recovery: SentenceRecoveryInput<'_>,
) -> Result<Comparison> {
    compare_aligned_inner(old, new, alignment, options, Some(recovery))
}

fn compare_aligned_inner(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
    recovery: Option<SentenceRecoveryInput<'_>>,
) -> Result<Comparison> {
    if let Some(recovery) = recovery {
        validate_sentence_recovery_input(old, new, recovery)?;
    }
    let (old, new) = inspect_sides_with_budget(old, new, options)?;
    let old = old.materialize()?;
    let new = new.materialize()?;
    validate_alignment(&old, &new, alignment)?;
    let sentence_recovery = match recovery {
        Some(recovery) => sentence::build_sentence_recovery_plan(
            &old,
            &new,
            alignment,
            recovery,
            options.max_tokens,
        )?,
        None => None,
    };
    let (moves_by_old, moves_by_new) = promotable_moves(&old, &new, alignment);

    let mut changes = Vec::new();
    let mut formatting_changes = Vec::new();
    let mut unresolved_regions = Vec::new();
    let mut resolved_old = 0;
    let mut resolved_new = 0;
    let mut sentence_recovery_output_budget = RecoveryOutputBudget::default();

    for span in &alignment.spans {
        match span.kind {
            AlignmentKind::Match => {
                // A matched span only counts toward resolved coverage when
                // compare_match actually resolved it; a span degraded to an
                // unresolved region must not inflate the metric.
                if compare_match(
                    &old,
                    &new,
                    span,
                    options,
                    &mut changes,
                    &mut formatting_changes,
                    &mut unresolved_regions,
                )? {
                    resolved_old += old.source_token_count(&span.old);
                    resolved_new += new.source_token_count(&span.new);
                }
            }
            AlignmentKind::Deletion => {
                if let Some(promoted) = moves_by_old.get(&span.old[0]) {
                    let old_tokens = old.source_token_count(&span.old);
                    let new_blocks = [promoted.new];
                    let new_tokens = new.source_token_count(&new_blocks);
                    resolved_old += old_tokens;
                    resolved_new += new_tokens;
                    changes.push(Change {
                        kind: ChangeKind::Move,
                        old_span: Some(old.canonical_group(&span.old, None).full_span()),
                        new_span: Some(new.canonical_group(&new_blocks, None).full_span()),
                        confidence: promoted.confidence,
                        tags: Vec::new(),
                    });
                    let old_raw = old.raw_group(&span.old, None)?;
                    let new_raw = new.raw_group(&new_blocks, None)?;
                    if old_raw.tokens != new_raw.tokens {
                        formatting_changes.push(FormattingChange {
                            old_span: old.canonical_group(&span.old, None).full_span(),
                            new_span: new.canonical_group(&new_blocks, None).full_span(),
                            confidence: promoted.confidence,
                            reasons: vec![FormattingReason::Normalization],
                        });
                    }
                    continue;
                }
                let source_tokens = old.source_token_count(&span.old);
                resolved_old += source_tokens;
                if source_tokens > 0 {
                    let group = old.canonical_group(&span.old, span.old_separator);
                    changes.push(Change {
                        kind: ChangeKind::Deletion,
                        old_span: Some(group.full_span()),
                        new_span: None,
                        confidence: span.confidence.into(),
                        tags: Vec::new(),
                    });
                }
            }
            AlignmentKind::Insertion => {
                if moves_by_new.contains_key(&span.new[0]) {
                    continue;
                }
                let source_tokens = new.source_token_count(&span.new);
                resolved_new += source_tokens;
                if source_tokens > 0 {
                    let group = new.canonical_group(&span.new, span.new_separator);
                    changes.push(Change {
                        kind: ChangeKind::Insertion,
                        old_span: None,
                        new_span: Some(group.full_span()),
                        confidence: span.confidence.into(),
                        tags: Vec::new(),
                    });
                }
            }
            AlignmentKind::Unresolved => {
                if let Some(recovery) = &sentence_recovery
                    && recovery.has_recovery(&span.old, &span.new)
                {
                    apply_sentence_recovery_or_fallback(
                        &old,
                        &new,
                        span,
                        recovery,
                        &mut changes,
                        &mut unresolved_regions,
                        &mut resolved_old,
                        &mut resolved_new,
                        &mut sentence_recovery_output_budget,
                    );
                } else {
                    unresolved_regions.push(UnresolvedRegion {
                        old_span: full_span(&old, &span.old, span.old_separator),
                        new_span: full_span(&new, &span.new, span.new_separator),
                        evidence: span.evidence.clone(),
                    });
                }
            }
        }
    }

    Ok(Comparison {
        changes,
        formatting_changes,
        unresolved_regions,
        old_coverage: coverage(resolved_old, old.total_tokens),
        new_coverage: coverage(resolved_new, new.total_tokens),
    })
}

fn validate_sentence_recovery_input(
    old: &[BlockText],
    new: &[BlockText],
    input: SentenceRecoveryInput<'_>,
) -> Result<()> {
    if input.old_trusted_run_intervals.len() != old.len() {
        return Err(Error::InvalidConfiguration(
            "old trusted-run interval metadata must contain one entry per normalized block"
                .to_owned(),
        ));
    }
    if input.new_trusted_run_intervals.len() != new.len() {
        return Err(Error::InvalidConfiguration(
            "new trusted-run interval metadata must contain one entry per normalized block"
                .to_owned(),
        ));
    }
    if input.min_tokens == 0 {
        return Err(Error::InvalidConfiguration(
            "sentence recovery min_tokens must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

const MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS: usize = sentence::MAX_SENTENCE_RECOVERY_RANGES * 8 + 2;
const MAX_SENTENCE_RECOVERY_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct RecoveryOutputLimits {
    max_items: usize,
    max_bytes: usize,
}

impl Default for RecoveryOutputLimits {
    fn default() -> Self {
        Self {
            max_items: MAX_SENTENCE_RECOVERY_OUTPUT_ITEMS,
            max_bytes: MAX_SENTENCE_RECOVERY_OUTPUT_BYTES,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct RecoveryOutputBudget {
    limits: RecoveryOutputLimits,
    items: usize,
    bytes: usize,
}

impl RecoveryOutputBudget {
    #[cfg(test)]
    fn with_limits(limits: RecoveryOutputLimits) -> Self {
        Self {
            limits,
            items: 0,
            bytes: 0,
        }
    }

    fn charge(&mut self, estimated_bytes: usize) -> bool {
        let Some(items) = self.items.checked_add(1) else {
            return false;
        };
        let Some(bytes) = self.bytes.checked_add(estimated_bytes) else {
            return false;
        };
        if items > self.limits.max_items || bytes > self.limits.max_bytes {
            return false;
        }
        self.items = items;
        self.bytes = bytes;
        true
    }
}

struct PreparedSentenceRecovery {
    changes: Vec<Change>,
    unresolved_regions: Vec<UnresolvedRegion>,
    resolved_old: usize,
    resolved_new: usize,
}

#[allow(clippy::too_many_arguments)]
fn apply_sentence_recovery_or_fallback(
    old: &Side<'_>,
    new: &Side<'_>,
    span: &AlignmentSpan,
    recovery: &sentence::SentenceRecoveryPlan,
    changes: &mut Vec<Change>,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    resolved_old: &mut usize,
    resolved_new: &mut usize,
    output_budget: &mut RecoveryOutputBudget,
) {
    if !append_sentence_recovery(
        old,
        new,
        span,
        recovery,
        changes,
        unresolved_regions,
        resolved_old,
        resolved_new,
        output_budget,
    ) {
        unresolved_regions.push(UnresolvedRegion {
            old_span: full_span(old, &span.old, span.old_separator),
            new_span: full_span(new, &span.new, span.new_separator),
            evidence: span.evidence.clone(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn append_sentence_recovery(
    old: &Side<'_>,
    new: &Side<'_>,
    span: &AlignmentSpan,
    recovery: &sentence::SentenceRecoveryPlan,
    changes: &mut Vec<Change>,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    resolved_old: &mut usize,
    resolved_new: &mut usize,
    output_budget: &mut RecoveryOutputBudget,
) -> bool {
    let mut tentative_budget = *output_budget;
    let Some(prepared) = prepare_sentence_recovery(old, new, span, recovery, &mut tentative_budget)
    else {
        return false;
    };
    let Some(next_resolved_old) = resolved_old.checked_add(prepared.resolved_old) else {
        return false;
    };
    let Some(next_resolved_new) = resolved_new.checked_add(prepared.resolved_new) else {
        return false;
    };
    if changes.try_reserve_exact(prepared.changes.len()).is_err()
        || unresolved_regions
            .try_reserve_exact(prepared.unresolved_regions.len())
            .is_err()
    {
        return false;
    }

    changes.extend(prepared.changes);
    unresolved_regions.extend(prepared.unresolved_regions);
    *resolved_old = next_resolved_old;
    *resolved_new = next_resolved_new;
    *output_budget = tentative_budget;
    true
}

fn prepare_sentence_recovery(
    old: &Side<'_>,
    new: &Side<'_>,
    span: &AlignmentSpan,
    recovery: &sentence::SentenceRecoveryPlan,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<PreparedSentenceRecovery> {
    let deletion_count = recovered_range_count(&span.old, &recovery.deletions)?;
    let insertion_count = recovered_range_count(&span.new, &recovery.insertions)?;
    let change_capacity = deletion_count.checked_add(insertion_count)?;
    let unresolved_capacity = change_capacity.checked_mul(3)?.checked_add(2)?;
    let mut changes = Vec::new();
    let mut unresolved_regions = Vec::new();
    changes.try_reserve_exact(change_capacity).ok()?;
    unresolved_regions
        .try_reserve_exact(unresolved_capacity)
        .ok()?;

    let resolved_old = prepare_recovered_changes(
        &span.old,
        &recovery.deletions,
        ChangeKind::Deletion,
        &mut changes,
        output_budget,
    )?;
    let resolved_new = prepare_recovered_changes(
        &span.new,
        &recovery.insertions,
        ChangeKind::Insertion,
        &mut changes,
        output_budget,
    )?;
    prepare_unresolved_remainders(
        old,
        &span.old,
        span.old_separator,
        &recovery.deletions,
        RemainderSide::Old,
        &span.evidence,
        &mut unresolved_regions,
        output_budget,
    )?;
    prepare_unresolved_remainders(
        new,
        &span.new,
        span.new_separator,
        &recovery.insertions,
        RemainderSide::New,
        &span.evidence,
        &mut unresolved_regions,
        output_budget,
    )?;

    Some(PreparedSentenceRecovery {
        changes,
        unresolved_regions,
        resolved_old,
        resolved_new,
    })
}

fn recovered_range_count(
    blocks: &[BlockId],
    recovered: &[sentence::LocalSentenceRange],
) -> Option<usize> {
    blocks.iter().try_fold(0usize, |count, block| {
        count.checked_add(sentence::ranges_for_block(recovered, *block).len())
    })
}

fn prepare_recovered_changes(
    blocks: &[BlockId],
    recovered: &[sentence::LocalSentenceRange],
    kind: ChangeKind,
    changes: &mut Vec<Change>,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<usize> {
    let mut resolved = 0usize;
    for block in blocks {
        for range in sentence::ranges_for_block(recovered, *block) {
            let token_count = range.comparable.end.checked_sub(range.comparable.start)?;
            resolved = resolved.checked_add(token_count)?;
            if !output_budget.charge(estimated_change_bytes()?) {
                return None;
            }
            let span = TextSpan {
                blocks: try_single_block(range.block)?,
                separator: None,
                canonical_range: range.canonical,
                comparable_range: range.comparable,
            };
            let (old_span, new_span) = match kind {
                ChangeKind::Deletion => (Some(span), None),
                ChangeKind::Insertion => (None, Some(span)),
                ChangeKind::Replacement | ChangeKind::Move => return None,
            };
            changes.push(Change {
                kind,
                old_span,
                new_span,
                confidence: Confidence::High,
                tags: Vec::new(),
            });
        }
    }
    Some(resolved)
}

#[allow(clippy::too_many_arguments)]
fn prepare_unresolved_remainders(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
    recovered: &[sentence::LocalSentenceRange],
    remainder_side: RemainderSide,
    evidence: &[AlignmentEvidence],
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<()> {
    let mut full_run_start = 0usize;
    for (block_position, block) in blocks.iter().copied().enumerate() {
        let ranges = sentence::ranges_for_block(recovered, block);
        if ranges.is_empty() {
            continue;
        }
        prepare_unresolved_block_run(
            side,
            &blocks[full_run_start..block_position],
            separator,
            remainder_side,
            evidence,
            unresolved_regions,
            output_budget,
        )?;
        let block_index = *side.index.get(&block)?;
        let token_end = side.canonical.get(block_index)?.len();
        let scalar_end = side.blocks.get(block_index)?.canonical.text.chars().count();
        let mut token_cursor = 0usize;
        let mut scalar_cursor = 0usize;
        for range in ranges {
            prepare_unresolved_remainder(
                block,
                token_cursor,
                range.comparable.start,
                scalar_cursor,
                range.canonical.start,
                remainder_side,
                evidence,
                unresolved_regions,
                output_budget,
            )?;
            token_cursor = range.comparable.end;
            scalar_cursor = range.canonical.end;
        }
        prepare_unresolved_remainder(
            block,
            token_cursor,
            token_end,
            scalar_cursor,
            scalar_end,
            remainder_side,
            evidence,
            unresolved_regions,
            output_budget,
        )?;
        full_run_start = block_position.checked_add(1)?;
    }
    prepare_unresolved_block_run(
        side,
        &blocks[full_run_start..],
        separator,
        remainder_side,
        evidence,
        unresolved_regions,
        output_budget,
    )?;
    Some(())
}

#[derive(Clone, Copy)]
enum RemainderSide {
    Old,
    New,
}

fn prepare_unresolved_block_run(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
    remainder_side: RemainderSide,
    evidence: &[AlignmentEvidence],
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<()> {
    if blocks.is_empty() || checked_source_token_count(side, blocks)? == 0 {
        return Some(());
    }
    if !output_budget.charge(estimated_unresolved_bytes(blocks.len(), evidence.len())?) {
        return None;
    }
    let span = try_group_full_span(side, blocks, separator)?;
    let (old_span, new_span) = match remainder_side {
        RemainderSide::Old => (Some(span), None),
        RemainderSide::New => (None, Some(span)),
    };
    unresolved_regions.push(UnresolvedRegion {
        old_span,
        new_span,
        evidence: try_copy_slice(evidence)?,
    });
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_unresolved_remainder(
    block: BlockId,
    token_start: usize,
    token_end: usize,
    scalar_start: usize,
    scalar_end: usize,
    remainder_side: RemainderSide,
    evidence: &[AlignmentEvidence],
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<()> {
    if token_start == token_end {
        return Some(());
    }
    if token_start > token_end || scalar_start > scalar_end {
        return None;
    }
    if !output_budget.charge(estimated_unresolved_bytes(1, evidence.len())?) {
        return None;
    }
    let span = TextSpan {
        blocks: try_single_block(block)?,
        separator: None,
        canonical_range: ScalarRange {
            start: scalar_start,
            end: scalar_end,
        },
        comparable_range: TokenRange {
            start: token_start,
            end: token_end,
        },
    };
    let (old_span, new_span) = match remainder_side {
        RemainderSide::Old => (Some(span), None),
        RemainderSide::New => (None, Some(span)),
    };
    unresolved_regions.push(UnresolvedRegion {
        old_span,
        new_span,
        evidence: try_copy_slice(evidence)?,
    });
    Some(())
}

fn checked_source_token_count(side: &Side<'_>, blocks: &[BlockId]) -> Option<usize> {
    blocks.iter().try_fold(0usize, |count, block| {
        let block_index = *side.index.get(block)?;
        count.checked_add(side.canonical.get(block_index)?.len())
    })
}

fn try_group_full_span(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
) -> Option<TextSpan> {
    let separator = effective_group_separator(blocks.len(), separator);
    let mut token_count = 0usize;
    let mut scalar_count = 0usize;
    let mut last_is_space = None;
    for (position, block) in blocks.iter().enumerate() {
        let block_index = *side.index.get(block)?;
        let next = side.canonical.get(block_index)?;
        if position > 0
            && separator == Some(BlockSeparator::Space)
            && last_is_space != Some(true)
            && !next.first().is_some_and(is_space_token)
        {
            token_count = token_count.checked_add(1)?;
            scalar_count = scalar_count.checked_add(1)?;
            last_is_space = Some(true);
        }
        token_count = token_count.checked_add(next.len())?;
        scalar_count = scalar_count.checked_add(
            next.iter()
                .filter(|token| matches!(token, ComparableToken::Scalar(_)))
                .count(),
        )?;
        if let Some(last) = next.last() {
            last_is_space = Some(is_space_token(last));
        }
    }

    Some(TextSpan {
        blocks: try_copy_slice(blocks)?,
        separator,
        canonical_range: ScalarRange {
            start: 0,
            end: scalar_count,
        },
        comparable_range: TokenRange {
            start: 0,
            end: token_count,
        },
    })
}

fn is_space_token(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn try_single_block(block: BlockId) -> Option<Vec<BlockId>> {
    let mut blocks = Vec::new();
    blocks.try_reserve_exact(1).ok()?;
    blocks.push(block);
    Some(blocks)
}

fn try_copy_slice<T: Copy>(source: &[T]) -> Option<Vec<T>> {
    let mut copied = Vec::new();
    copied.try_reserve_exact(source.len()).ok()?;
    copied.extend_from_slice(source);
    Some(copied)
}

fn estimated_change_bytes() -> Option<usize> {
    std::mem::size_of::<Change>()
        .checked_add(std::mem::size_of::<BlockId>())?
        .checked_mul(2)
}

fn estimated_unresolved_bytes(block_count: usize, evidence_count: usize) -> Option<usize> {
    std::mem::size_of::<UnresolvedRegion>()
        .checked_add(block_count.checked_mul(std::mem::size_of::<BlockId>())?)?
        .checked_add(evidence_count.checked_mul(std::mem::size_of::<AlignmentEvidence>())?)?
        .checked_mul(2)
}

#[derive(Clone, Copy)]
struct PromotedMove {
    new: BlockId,
    confidence: Confidence,
}

fn promotable_moves(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
) -> (HashMap<BlockId, PromotedMove>, HashMap<BlockId, BlockId>) {
    let deletions = alignment
        .spans
        .iter()
        .filter(|span| {
            span.kind == AlignmentKind::Deletion
                && span.evidence.contains(&AlignmentEvidence::MoveCandidate)
        })
        .map(|span| (span.old[0], span))
        .collect::<HashMap<_, _>>();
    let insertions = alignment
        .spans
        .iter()
        .filter(|span| {
            span.kind == AlignmentKind::Insertion
                && span.evidence.contains(&AlignmentEvidence::MoveCandidate)
        })
        .map(|span| (span.new[0], span))
        .collect::<HashMap<_, _>>();

    let mut old_candidate_counts = HashMap::new();
    let mut new_candidate_counts = HashMap::new();
    for anchor in &alignment.move_candidates {
        *old_candidate_counts.entry(anchor.old).or_insert(0_usize) += 1;
        *new_candidate_counts.entry(anchor.new).or_insert(0_usize) += 1;
    }

    let mut old_token_counts = HashMap::new();
    for tokens in &old.canonical {
        if !tokens.is_empty() {
            *old_token_counts.entry(tokens).or_insert(0_usize) += 1;
        }
    }
    let mut new_token_counts = HashMap::new();
    for tokens in &new.canonical {
        if !tokens.is_empty() {
            *new_token_counts.entry(tokens).or_insert(0_usize) += 1;
        }
    }

    let mut by_old = HashMap::new();
    let mut by_new = HashMap::new();

    for anchor in &alignment.move_candidates {
        if old_candidate_counts.get(&anchor.old) != Some(&1)
            || new_candidate_counts.get(&anchor.new) != Some(&1)
        {
            continue;
        }
        let (Some(deletion), Some(insertion)) =
            (deletions.get(&anchor.old), insertions.get(&anchor.new))
        else {
            continue;
        };
        let (Some(old_index), Some(new_index)) =
            (old.index.get(&anchor.old), new.index.get(&anchor.new))
        else {
            continue;
        };
        let old_tokens = &old.canonical[*old_index];
        let new_tokens = &new.canonical[*new_index];
        // 1:1 uniqueness in move_candidates ensures anchor.old and anchor.new
        // are visited at most once in this loop.
        if old_tokens.is_empty()
            || old_tokens != new_tokens
            || old_token_counts.get(old_tokens) != Some(&1)
            || new_token_counts.get(new_tokens) != Some(&1)
        {
            continue;
        }
        let confidence = weaker_confidence(deletion.confidence, insertion.confidence).into();
        by_old.insert(
            anchor.old,
            PromotedMove {
                new: anchor.new,
                confidence,
            },
        );
        by_new.insert(anchor.new, anchor.old);
    }
    (by_old, by_new)
}

fn weaker_confidence(left: AlignmentConfidence, right: AlignmentConfidence) -> AlignmentConfidence {
    match (left, right) {
        (AlignmentConfidence::Low, _) | (_, AlignmentConfidence::Low) => AlignmentConfidence::Low,
        (AlignmentConfidence::Medium, _) | (_, AlignmentConfidence::Medium) => {
            AlignmentConfidence::Medium
        }
        (AlignmentConfidence::High, AlignmentConfidence::High) => AlignmentConfidence::High,
    }
}

fn inspect_sides_with_budget<'a>(
    old: &'a [BlockText],
    new: &'a [BlockText],
    options: DiffOptions,
) -> Result<(SidePlan<'a>, SidePlan<'a>)> {
    validate_diff_options(options)?;

    let old = SidePlan::inspect("old", old)?;
    let new = SidePlan::inspect("new", new)?;
    enforce_combined_token_budget(
        "diff comparable tokens",
        old.total_tokens,
        new.total_tokens,
        options.max_tokens,
    )?;
    enforce_combined_token_budget(
        "diff raw evidence tokens",
        old.raw_tokens,
        new.raw_tokens,
        options.max_tokens,
    )?;
    Ok((old, new))
}

fn compare_match(
    old_side: &Side<'_>,
    new_side: &Side<'_>,
    span: &AlignmentSpan,
    options: DiffOptions,
    changes: &mut Vec<Change>,
    formatting_changes: &mut Vec<FormattingChange>,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
) -> Result<bool> {
    let old = old_side.canonical_group(&span.old, span.old_separator);
    let new = new_side.canonical_group(&span.new, span.new_separator);
    if old.tokens == new.tokens {
        let mut reasons = Vec::new();
        let old_raw = old_side.raw_group(&span.old, span.old_separator)?;
        let new_raw = new_side.raw_group(&span.new, span.new_separator)?;
        if old_raw.tokens != new_raw.tokens {
            reasons.push(FormattingReason::Normalization);
        }
        if span.old.len() != span.new.len() || span.old_separator != span.new_separator {
            reasons.push(FormattingReason::BlockStructure);
        }
        if let (Some(old_sizes), Some(new_sizes)) =
            (&old.font_size_signatures, &new.font_size_signatures)
            && old_sizes != new_sizes
        {
            reasons.push(FormattingReason::FontSize);
        }
        if has_position_change(&old, &new) {
            reasons.push(FormattingReason::Position);
        }
        if let (Some(old_breaks), Some(new_breaks)) = (&old.line_breaks, &new.line_breaks)
            && old_breaks != new_breaks
        {
            reasons.push(FormattingReason::LineBreak);
        }
        if let (Some(old_breaks), Some(new_breaks)) = (&old.page_breaks, &new.page_breaks)
            && old_breaks != new_breaks
        {
            reasons.push(FormattingReason::PageBreak);
        }
        if !reasons.is_empty() {
            formatting_changes.push(FormattingChange {
                old_span: old.full_span(),
                new_span: new.full_span(),
                confidence: span.confidence.into(),
                reasons,
            });
        }
        return Ok(true);
    }

    let edits = match myers::diff(&old.tokens, &new.tokens, options.max_edit_distance)? {
        Some(edits)
            if span.confidence != AlignmentConfidence::Low
                || !is_implausible_match(&edits, old.tokens.len(), new.tokens.len(), options) =>
        {
            edits
        }
        _ => {
            // Either the edit distance budget was exceeded, or a weak alignment
            // produced an implausible match / change soup. Keep the comparison alive
            // and report the matched group as an unresolved region instead of failing
            // the whole document.
            unresolved_regions.push(UnresolvedRegion {
                old_span: Some(old.full_span()),
                new_span: Some(new.full_span()),
                evidence: span.evidence.clone(),
            });
            return Ok(false);
        }
    };

    append_changes(&old, &new, &edits, span.confidence.into(), changes);
    Ok(true)
}

/// Maximum allowable hunk-to-token ratio for weak matches before degrading to unresolved.
const MAX_WEAK_MATCH_HUNK_RATIO: f64 = 0.2;

/// Minimum span length in tokens required to evaluate hunk density.
const MIN_HUNK_DENSITY_TOKENS: usize = 8;

/// Returns true if a weak match has excessive changes or hunk fragmentation.
fn is_implausible_match(
    edits: &[Edit],
    old_tokens: usize,
    new_tokens: usize,
    options: DiffOptions,
) -> bool {
    let total = old_tokens.max(new_tokens);
    if total == 0 {
        return false;
    }
    let changed = edits.iter().filter(|edit| **edit != Edit::Equal).count();
    if changed as f64 / total as f64 > options.max_weak_match_change_ratio {
        return true;
    }
    if total < MIN_HUNK_DENSITY_TOKENS {
        return false;
    }
    let hunks = edits
        .iter()
        .fold((0usize, false), |(hunks, in_hunk), edit| {
            match *edit != Edit::Equal {
                true if !in_hunk => (hunks + 1, true),
                true => (hunks, true),
                false => (hunks, false),
            }
        })
        .0;
    hunks as f64 / total as f64 > MAX_WEAK_MATCH_HUNK_RATIO
}

fn append_changes(
    old: &GroupText,
    new: &GroupText,
    edits: &[Edit],
    confidence: Confidence,
    changes: &mut Vec<Change>,
) {
    let mut old_index = 0;
    let mut new_index = 0;
    let mut hunk_start = None;

    for edit in edits {
        match edit {
            Edit::Equal => {
                flush_hunk(
                    old,
                    new,
                    hunk_start.take(),
                    old_index,
                    new_index,
                    confidence,
                    changes,
                );
                old_index += 1;
                new_index += 1;
            }
            Edit::Delete => {
                hunk_start.get_or_insert((old_index, new_index));
                old_index += 1;
            }
            Edit::Insert => {
                hunk_start.get_or_insert((old_index, new_index));
                new_index += 1;
            }
        }
    }
    flush_hunk(
        old, new, hunk_start, old_index, new_index, confidence, changes,
    );
}

#[allow(clippy::too_many_arguments)]
fn flush_hunk(
    old: &GroupText,
    new: &GroupText,
    start: Option<(usize, usize)>,
    old_end: usize,
    new_end: usize,
    confidence: Confidence,
    changes: &mut Vec<Change>,
) {
    let Some((old_start, new_start)) = start else {
        return;
    };
    let old_changed = old_start != old_end;
    let new_changed = new_start != new_end;
    let kind = match (old_changed, new_changed) {
        (true, true) => ChangeKind::Replacement,
        (true, false) => ChangeKind::Deletion,
        (false, true) => ChangeKind::Insertion,
        (false, false) => return,
    };
    let tags = (kind == ChangeKind::Replacement
        && is_character_width_replacement(
            &old.tokens[old_start..old_end],
            &new.tokens[new_start..new_end],
        ))
    .then_some(ChangeTag::CharacterWidth)
    .into_iter()
    .collect();
    changes.push(Change {
        kind,
        old_span: old_changed.then(|| old.span(old_start, old_end)),
        new_span: new_changed.then(|| new.span(new_start, new_end)),
        confidence,
        tags,
    });
}

fn is_character_width_replacement(old: &[ComparableToken], new: &[ComparableToken]) -> bool {
    let Some((old_folded, old_changed)) = character_width_fold(old) else {
        return false;
    };
    let Some((new_folded, new_changed)) = character_width_fold(new) else {
        return false;
    };

    (old_changed || new_changed) && old_folded == new_folded
}

fn validate_alignment(old: &Side<'_>, new: &Side<'_>, alignment: &Alignment) -> Result<()> {
    let mut old_cursor = 0;
    let mut new_cursor = 0;

    for span in &alignment.spans {
        validate_span_shape(span)?;
        validate_separator("old", &span.old, span.old_separator, span.kind)?;
        validate_separator("new", &span.new, span.new_separator, span.kind)?;
        consume_blocks("old", &span.old, old, &mut old_cursor)?;
        consume_blocks("new", &span.new, new, &mut new_cursor)?;
    }
    if old_cursor != old.blocks.len() {
        return Err(Error::Unresolved(format!(
            "alignment assigned {} of {} old blocks",
            old_cursor,
            old.blocks.len()
        )));
    }
    if new_cursor != new.blocks.len() {
        return Err(Error::Unresolved(format!(
            "alignment assigned {} of {} new blocks",
            new_cursor,
            new.blocks.len()
        )));
    }
    Ok(())
}

fn validate_span_shape(span: &AlignmentSpan) -> Result<()> {
    let valid = match span.kind {
        AlignmentKind::Match => !span.old.is_empty() && !span.new.is_empty(),
        AlignmentKind::Deletion => span.old.len() == 1 && span.new.is_empty(),
        AlignmentKind::Insertion => span.old.is_empty() && span.new.len() == 1,
        AlignmentKind::Unresolved => !span.old.is_empty() || !span.new.is_empty(),
    };
    if valid {
        Ok(())
    } else {
        Err(Error::Unresolved(format!(
            "invalid {:?} alignment span shape",
            span.kind
        )))
    }
}

fn validate_separator(
    side: &str,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
    kind: AlignmentKind,
) -> Result<()> {
    if kind != AlignmentKind::Match && separator.is_some() {
        return Err(Error::Unresolved(format!(
            "{side} alignment separators are only valid for matched block groups"
        )));
    }
    if blocks.len() <= 1 && separator.is_some() {
        return Err(Error::Unresolved(format!(
            "{side} alignment separator requires multiple blocks"
        )));
    }
    if kind == AlignmentKind::Match && blocks.len() > 1 && separator.is_none() {
        return Err(Error::Unresolved(format!(
            "matched {side} block group is missing its separator"
        )));
    }
    Ok(())
}

fn consume_blocks(
    side: &str,
    blocks: &[BlockId],
    source: &Side<'_>,
    cursor: &mut usize,
) -> Result<()> {
    for block in blocks {
        let Some(position) = source.index.get(block).copied() else {
            return Err(Error::Unresolved(format!(
                "alignment references unknown {side} block {}",
                block.0
            )));
        };
        if position != *cursor {
            return Err(Error::Unresolved(format!(
                "alignment consumes {side} block {} at source index {position}, expected index {}",
                block.0, *cursor
            )));
        }
        *cursor += 1;
    }
    Ok(())
}

fn full_span(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
) -> Option<TextSpan> {
    (!blocks.is_empty()).then(|| side.canonical_group(blocks, separator).full_span())
}

fn enforce_combined_token_budget(
    resource: &'static str,
    old_tokens: usize,
    new_tokens: usize,
    limit: usize,
) -> Result<()> {
    let total = old_tokens
        .checked_add(new_tokens)
        .ok_or(Error::LimitExceeded { resource, limit })?;
    if total > limit {
        return Err(Error::LimitExceeded { resource, limit });
    }
    Ok(())
}

fn coverage(resolved_tokens: usize, total_tokens: usize) -> Coverage {
    Coverage {
        resolved_tokens,
        total_tokens,
        ratio: Some(if total_tokens == 0 {
            1.0
        } else {
            resolved_tokens as f64 / total_tokens as f64
        }),
    }
}

impl From<AlignmentConfidence> for Confidence {
    fn from(value: AlignmentConfidence) -> Self {
        match value {
            AlignmentConfidence::High => Self::High,
            AlignmentConfidence::Medium => Self::Medium,
            AlignmentConfidence::Low => Self::Low,
        }
    }
}

struct SidePlan<'a> {
    blocks: &'a [BlockText],
    index: HashMap<BlockId, usize>,
    total_tokens: usize,
    raw_tokens: usize,
}

impl<'a> SidePlan<'a> {
    fn inspect(name: &str, blocks: &'a [BlockText]) -> Result<Self> {
        let mut index = HashMap::with_capacity(blocks.len());
        let mut total_tokens = 0usize;
        let mut raw_tokens = 0usize;

        for (position, block) in blocks.iter().enumerate() {
            if index.insert(block.block, position).is_some() {
                return Err(Error::Unresolved(format!(
                    "duplicate {name} block id {}",
                    block.block.0
                )));
            }
            let canonical_count = block.canonical.comparable_token_count()?;
            validate_font_size_signatures(name, block, canonical_count)?;
            validate_position_signatures(name, block, canonical_count)?;
            validate_line_breaks(name, block, canonical_count)?;
            validate_page_breaks(name, block, canonical_count)?;
            total_tokens =
                total_tokens
                    .checked_add(canonical_count)
                    .ok_or(Error::LimitExceeded {
                        resource: "diff comparable tokens",
                        limit: usize::MAX,
                    })?;
            raw_tokens = raw_tokens
                .checked_add(block.raw.comparable_token_count()?)
                .ok_or(Error::LimitExceeded {
                    resource: "diff raw evidence tokens",
                    limit: usize::MAX,
                })?;
        }

        Ok(Self {
            blocks,
            index,
            total_tokens,
            raw_tokens,
        })
    }

    fn materialize(self) -> Result<Side<'a>> {
        let canonical = self
            .blocks
            .iter()
            .map(|block| block.canonical.comparable_tokens())
            .collect::<Result<Vec<_>>>()?;
        Ok(Side {
            blocks: self.blocks,
            index: self.index,
            canonical,
            total_tokens: self.total_tokens,
        })
    }
}

struct Side<'a> {
    blocks: &'a [BlockText],
    index: HashMap<BlockId, usize>,
    canonical: Vec<Vec<ComparableToken>>,
    total_tokens: usize,
}

impl Side<'_> {
    fn source_token_count(&self, blocks: &[BlockId]) -> usize {
        blocks
            .iter()
            .map(|block| self.canonical[self.index[block]].len())
            .sum()
    }

    fn canonical_group(&self, blocks: &[BlockId], separator: Option<BlockSeparator>) -> GroupText {
        let separator = effective_group_separator(blocks.len(), separator);
        let mut tokens = Vec::new();
        let mut font_size_signatures = Some(Vec::new());
        let mut position_signatures = Some(Vec::new());
        let mut line_breaks = Some(Vec::new());
        let mut page_breaks = Some(Vec::new());
        let mut previous_page = None;
        for (position, block) in blocks.iter().enumerate() {
            let block_index = self.index[block];
            let block = &self.blocks[block_index];
            let next = &self.canonical[block_index];
            let preceding_token_count = tokens.len();
            if position == 0 {
                tokens.extend_from_slice(next);
            } else {
                separator
                    .unwrap_or(BlockSeparator::Concatenate)
                    .append(&mut tokens, next);
            }
            let block_start = tokens.len() - next.len();
            let inserted_separator_tokens = block_start - preceding_token_count;

            font_size_signatures = font_size_signatures.take().and_then(|combined| {
                block.font_size_signatures.as_ref().and_then(|next_sizes| {
                    append_font_size_signatures(combined, next_sizes, inserted_separator_tokens)
                })
            });
            position_signatures = position_signatures.take().and_then(|combined| {
                block
                    .position_signatures
                    .as_ref()
                    .and_then(|next_positions| {
                        append_position_signatures(
                            combined,
                            next_positions,
                            inserted_separator_tokens,
                        )
                    })
            });

            if position > 0 {
                if let Some(breaks) = &mut line_breaks {
                    breaks.push(block_start);
                }
                match (previous_page, block.pages.first().copied()) {
                    (Some(previous), Some(current)) if previous != current => {
                        if let Some(breaks) = &mut page_breaks {
                            breaks.push(block_start);
                        }
                    }
                    (Some(_), Some(_)) => {}
                    _ => page_breaks = None,
                }
            }
            match (&mut line_breaks, &block.line_breaks) {
                (Some(group_breaks), Some(block_breaks)) => {
                    group_breaks.extend(block_breaks.iter().map(|offset| block_start + offset))
                }
                _ => line_breaks = None,
            }
            match (&mut page_breaks, &block.page_breaks) {
                (Some(group_breaks), Some(block_breaks)) => {
                    group_breaks.extend(block_breaks.iter().map(|offset| block_start + offset))
                }
                _ => page_breaks = None,
            }
            previous_page = block.pages.last().copied();
        }
        GroupText::new(
            blocks.to_vec(),
            separator,
            tokens,
            font_size_signatures,
            position_signatures,
            line_breaks,
            page_breaks,
        )
    }

    fn raw_group(
        &self,
        blocks: &[BlockId],
        separator: Option<BlockSeparator>,
    ) -> Result<GroupText> {
        let separator = effective_group_separator(blocks.len(), separator);
        let mut tokens = Vec::new();
        for (position, block) in blocks.iter().enumerate() {
            let next = self.blocks[self.index[block]].raw.comparable_tokens()?;
            if position == 0 {
                tokens = next;
            } else {
                separator
                    .unwrap_or(BlockSeparator::Concatenate)
                    .append(&mut tokens, &next);
            }
        }
        Ok(GroupText::new(
            blocks.to_vec(),
            separator,
            tokens,
            None,
            None,
            None,
            None,
        ))
    }
}

fn append_font_size_signatures(
    mut combined: Vec<FontSizeSignature>,
    next: &[FontSizeSignature],
    inserted_separator_tokens: usize,
) -> Option<Vec<FontSizeSignature>> {
    match inserted_separator_tokens {
        0 => {}
        1 => combined.push(combined.last()?.union(next.first()?)),
        _ => return None,
    }
    combined.extend_from_slice(next);
    Some(combined)
}

fn append_position_signatures(
    mut combined: Vec<Option<PositionSignature>>,
    next: &[PositionSignature],
    inserted_separator_tokens: usize,
) -> Option<Vec<Option<PositionSignature>>> {
    match inserted_separator_tokens {
        0 => {}
        1 => combined.push(None),
        _ => return None,
    }
    combined.extend(next.iter().copied().map(Some));
    Some(combined)
}

fn validate_font_size_signatures(name: &str, block: &BlockText, token_count: usize) -> Result<()> {
    let Some(signatures) = &block.font_size_signatures else {
        return Ok(());
    };
    if signatures.len() != token_count {
        return Err(Error::Unresolved(format!(
            "{name} block {} font-size signature count does not match its canonical token count",
            block.block.0
        )));
    }
    Ok(())
}

fn validate_position_signatures(name: &str, block: &BlockText, token_count: usize) -> Result<()> {
    let Some(signatures) = &block.position_signatures else {
        return Ok(());
    };
    if signatures.len() != token_count {
        return Err(Error::Unresolved(format!(
            "{name} block {} position signature count does not match its canonical token count",
            block.block.0
        )));
    }
    Ok(())
}

fn has_position_change(old: &GroupText, new: &GroupText) -> bool {
    let (
        Some(old_positions),
        Some(new_positions),
        Some(old_line_breaks),
        Some(new_line_breaks),
        Some(old_page_breaks),
        Some(new_page_breaks),
    ) = (
        &old.position_signatures,
        &new.position_signatures,
        &old.line_breaks,
        &new.line_breaks,
        &old.page_breaks,
        &new.page_breaks,
    )
    else {
        return false;
    };
    if old_positions.len() != new_positions.len()
        || old_line_breaks != new_line_breaks
        || old_page_breaks != new_page_breaks
    {
        return false;
    }

    let mut start = 0;
    let mut changed = false;
    for end in old_line_breaks
        .iter()
        .copied()
        .chain(std::iter::once(old_positions.len()))
    {
        let Some(line_changed) =
            translated_line_change(&old_positions[start..end], &new_positions[start..end])
        else {
            return false;
        };
        changed |= line_changed;
        start = end;
    }
    changed
}

fn translated_line_change(
    old: &[Option<PositionSignature>],
    new: &[Option<PositionSignature>],
) -> Option<bool> {
    let mut anchors: Option<(Vec2, Vec2)> = None;
    let mut changed = false;

    for (old, new) in old.iter().zip(new) {
        let (old, new) = match (old, new) {
            (Some(old), Some(new)) => (*old, *new),
            (None, None) => continue,
            _ => return None,
        };
        if old.direction() != new.direction() {
            return None;
        }
        let old_baseline = old.baseline();
        let new_baseline = new.baseline();
        if let Some((old_anchor, new_anchor)) = anchors {
            if old_baseline.x - old_anchor.x != new_baseline.x - new_anchor.x
                || old_baseline.y - old_anchor.y != new_baseline.y - new_anchor.y
            {
                return None;
            }
        } else {
            changed = old_baseline != new_baseline;
            anchors = Some((old_baseline, new_baseline));
        }
    }

    anchors.map(|_| changed)
}

fn validate_line_breaks(name: &str, block: &BlockText, token_count: usize) -> Result<()> {
    let Some(line_breaks) = &block.line_breaks else {
        return Ok(());
    };
    validate_break_offsets(name, block.block, "line-break", line_breaks, token_count)
}

fn validate_page_breaks(name: &str, block: &BlockText, token_count: usize) -> Result<()> {
    let Some(page_breaks) = &block.page_breaks else {
        return Ok(());
    };
    if !block.pages.is_empty() && page_breaks.len() + 1 != block.pages.len() {
        return Err(Error::Unresolved(format!(
            "{name} block {} page-break count does not match its page coverage",
            block.block.0
        )));
    }

    validate_break_offsets(name, block.block, "page-break", page_breaks, token_count)
}

fn validate_break_offsets(
    name: &str,
    block: BlockId,
    kind: &str,
    offsets: &[usize],
    token_count: usize,
) -> Result<()> {
    let mut previous = None;
    for offset in offsets {
        if *offset == 0
            || *offset >= token_count
            || previous.is_some_and(|previous| previous >= *offset)
        {
            return Err(Error::Unresolved(format!(
                "{name} block {} {kind} offsets must be strictly increasing token boundaries",
                block.0
            )));
        }
        previous = Some(*offset);
    }
    Ok(())
}

fn effective_group_separator(
    block_count: usize,
    separator: Option<BlockSeparator>,
) -> Option<BlockSeparator> {
    (block_count > 1).then(|| separator.unwrap_or(BlockSeparator::Concatenate))
}

struct GroupText {
    blocks: Vec<BlockId>,
    separator: Option<BlockSeparator>,
    tokens: Vec<ComparableToken>,
    font_size_signatures: Option<Vec<FontSizeSignature>>,
    position_signatures: Option<Vec<Option<PositionSignature>>>,
    line_breaks: Option<Vec<usize>>,
    page_breaks: Option<Vec<usize>>,
    scalar_boundaries: Vec<usize>,
}

impl GroupText {
    fn new(
        blocks: Vec<BlockId>,
        separator: Option<BlockSeparator>,
        tokens: Vec<ComparableToken>,
        font_size_signatures: Option<Vec<FontSizeSignature>>,
        position_signatures: Option<Vec<Option<PositionSignature>>>,
        line_breaks: Option<Vec<usize>>,
        page_breaks: Option<Vec<usize>>,
    ) -> Self {
        let mut scalar_boundaries = Vec::with_capacity(tokens.len() + 1);
        let mut scalar_count = 0;
        scalar_boundaries.push(0);
        for token in &tokens {
            if matches!(token, ComparableToken::Scalar(_)) {
                scalar_count += 1;
            }
            scalar_boundaries.push(scalar_count);
        }
        Self {
            blocks,
            separator,
            tokens,
            font_size_signatures,
            position_signatures,
            line_breaks,
            page_breaks,
            scalar_boundaries,
        }
    }

    fn full_span(&self) -> TextSpan {
        self.span(0, self.tokens.len())
    }

    fn span(&self, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: self.blocks.clone(),
            separator: self.separator,
            canonical_range: ScalarRange {
                start: self.scalar_boundaries[start],
                end: self.scalar_boundaries[end],
            },
            comparable_range: TokenRange { start, end },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        alignment::Alignment,
        layout::TrustedRunId,
        model::FontProgramHash,
        normalize::{
            MappedText, NormalizationIssue, NormalizationIssueKind, TextSource, UnmappedToken,
        },
    };

    fn options(ratio: f64) -> DiffOptions {
        DiffOptions {
            max_weak_match_change_ratio: ratio,
            ..DiffOptions::default()
        }
    }

    #[test]
    fn rejects_edit_distance_above_the_trace_memory_cap() {
        let at_cap = DiffOptions {
            max_edit_distance: MAX_MYERS_EDIT_DISTANCE,
            ..DiffOptions::default()
        };
        assert_eq!(validate_diff_options(at_cap), Ok(()));

        let error = validate_diff_options(DiffOptions {
            max_edit_distance: MAX_MYERS_EDIT_DISTANCE + 1,
            ..DiffOptions::default()
        })
        .expect_err("an edit distance above the trace cap must be rejected");
        assert!(matches!(
            error,
            Error::InvalidConfiguration(message)
                if message.contains("max_edit_distance") && message.contains("64 MiB")
        ));
    }

    #[test]
    fn empty_edits_are_never_implausible() {
        assert!(!is_implausible_match(&[], 0, 0, options(0.5)));
    }

    #[test]
    fn a_short_clean_replacement_stays_plausible_at_the_exact_ratio_limit() {
        // "Xaaa" -> "Yaaa": one hunk, changed ratio exactly at the limit.
        let edits = [
            Edit::Delete,
            Edit::Insert,
            Edit::Equal,
            Edit::Equal,
            Edit::Equal,
        ];
        assert!(!is_implausible_match(&edits, 4, 4, options(0.5)));
        // Just above the limit the changed-token ratio still gates short spans.
        let edits = [Edit::Delete, Edit::Insert, Edit::Delete, Edit::Insert];
        assert!(is_implausible_match(&edits, 4, 4, options(0.5)));
    }

    #[test]
    fn a_large_enough_span_with_dense_low_ratio_hunks_is_soup() {
        // Four islands separated by three-token equal runs: changed ratio
        // 8/19 stays under the limit while the hunk density exceeds it.
        let mut edits = Vec::new();
        for _ in 0..4 {
            edits.extend([
                Edit::Equal,
                Edit::Equal,
                Edit::Equal,
                Edit::Delete,
                Edit::Insert,
            ]);
        }
        edits.push(Edit::Equal);
        assert!(is_implausible_match(&edits, 19, 19, options(0.5)));
    }

    #[test]
    fn hunk_density_is_skipped_below_the_minimum_sample_size() {
        // Two isolated deletions in a five-token span: under the ceiling on
        // changed tokens, so fragmentation must not degrade it.
        let edits = [Edit::Equal, Edit::Delete, Edit::Equal, Edit::Delete];
        assert!(!is_implausible_match(&edits, 5, 3, options(0.5)));
    }

    #[test]
    fn weaker_confidence_satisfies_lattice_lower_bound_laws() {
        use crate::alignment::AlignmentConfidence::*;

        // Idempotency: weaker(a, a) == a
        assert_eq!(weaker_confidence(High, High), High);
        assert_eq!(weaker_confidence(Medium, Medium), Medium);
        assert_eq!(weaker_confidence(Low, Low), Low);

        // Commutativity: weaker(a, b) == weaker(b, a)
        assert_eq!(weaker_confidence(High, Medium), Medium);
        assert_eq!(weaker_confidence(Medium, High), Medium);
        assert_eq!(weaker_confidence(High, Low), Low);
        assert_eq!(weaker_confidence(Low, High), Low);
        assert_eq!(weaker_confidence(Medium, Low), Low);
        assert_eq!(weaker_confidence(Low, Medium), Low);

        // Associativity: weaker(weaker(a, b), c) == weaker(a, weaker(b, c))
        let confidences = [High, Medium, Low];
        for a in confidences {
            for b in confidences {
                for c in confidences {
                    assert_eq!(
                        weaker_confidence(weaker_confidence(a, b), c),
                        weaker_confidence(a, weaker_confidence(b, c))
                    );
                }
            }
        }
    }

    #[test]
    fn recovers_exact_deletion_and_insertion_with_partitioned_remainders() {
        let short = "Retain.";
        let long = "Retain. Removed sentence.";
        let sentence_start = "Retain. ".chars().count();
        let sentence_end = long.chars().count();
        let recovered_tokens = sentence_end - sentence_start;
        let evidence = vec![AlignmentEvidence::ReadingOrderUnknown];

        let old = vec![sentence_block(1, long)];
        let new = vec![sentence_block(2, short)];
        let deletion = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
            evidence.clone(),
        );
        assert_eq!(
            deletion.changes,
            vec![Change {
                kind: ChangeKind::Deletion,
                old_span: Some(test_span(1, sentence_start, sentence_end)),
                new_span: None,
                confidence: Confidence::High,
                tags: Vec::new(),
            }]
        );
        assert_eq!(
            deletion.unresolved_regions,
            vec![
                UnresolvedRegion {
                    old_span: Some(test_span(1, 0, sentence_start)),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: None,
                    new_span: Some(test_span(2, 0, short.chars().count())),
                    evidence: evidence.clone(),
                },
            ]
        );
        assert_eq!(deletion.old_coverage.resolved_tokens, recovered_tokens);
        assert_eq!(deletion.new_coverage.resolved_tokens, 0);

        let old = vec![sentence_block(3, short)];
        let new = vec![sentence_block(4, long)];
        let insertion = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(3))],
            &[Some(TrustedRunId(4))],
            5,
            evidence.clone(),
        );
        assert_eq!(
            insertion.changes,
            vec![Change {
                kind: ChangeKind::Insertion,
                old_span: None,
                new_span: Some(test_span(4, sentence_start, sentence_end)),
                confidence: Confidence::High,
                tags: Vec::new(),
            }]
        );
        assert_eq!(
            insertion.unresolved_regions,
            vec![
                UnresolvedRegion {
                    old_span: Some(test_span(3, 0, short.chars().count())),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: None,
                    new_span: Some(test_span(4, 0, sentence_start)),
                    evidence,
                },
            ]
        );
        assert_eq!(insertion.old_coverage.resolved_tokens, 0);
        assert_eq!(insertion.new_coverage.resolved_tokens, recovered_tokens);
    }

    #[test]
    fn coalesces_thousands_of_full_block_remainders_into_maximal_runs() {
        const RUN_LENGTH: usize = 1_000;

        let recovered_text = "Recovered sentence.";
        let recovered_block = BlockId(RUN_LENGTH as u64 + 1);
        let mut old = Vec::with_capacity(RUN_LENGTH * 2 + 1);
        for index in 0..RUN_LENGTH {
            old.push(sentence_block(index as u64 + 1, "x"));
        }
        old.push(sentence_block(recovered_block.0, recovered_text));
        for index in 0..RUN_LENGTH {
            old.push(sentence_block(recovered_block.0 + index as u64 + 1, "x"));
        }
        let mut trusted_run_ids = vec![None; old.len()];
        trusted_run_ids[RUN_LENGTH] = Some(TrustedRunId(1));
        let evidence = vec![AlignmentEvidence::ReadingOrderUnknown];

        let result =
            compare_sentence_recovery(&old, &[], &trusted_run_ids, &[], 1, evidence.clone());

        let before_region_count = RUN_LENGTH * 2;
        let after_region_count = result.unresolved_regions.len();
        assert_eq!((before_region_count, after_region_count), (2_000, 2));
        let first = result.unresolved_regions[0]
            .old_span
            .as_ref()
            .expect("first run is an old-side remainder");
        let second = result.unresolved_regions[1]
            .old_span
            .as_ref()
            .expect("second run is an old-side remainder");
        assert_eq!(first.blocks.len(), RUN_LENGTH);
        assert_eq!(first.blocks.first(), Some(&BlockId(1)));
        assert_eq!(first.blocks.last(), Some(&BlockId(RUN_LENGTH as u64)));
        assert_eq!(second.blocks.len(), RUN_LENGTH);
        assert_eq!(second.blocks.first(), Some(&BlockId(recovered_block.0 + 1)));
        assert_eq!(
            second.blocks.last(),
            Some(&BlockId(recovered_block.0 + RUN_LENGTH as u64))
        );
        assert_eq!(first.separator, Some(BlockSeparator::Concatenate));
        assert_eq!(second.separator, Some(BlockSeparator::Concatenate));
        assert!(
            result
                .unresolved_regions
                .iter()
                .all(|region| region.new_span.is_none() && region.evidence == evidence)
        );
        assert_eq!(
            result.changes,
            vec![Change {
                kind: ChangeKind::Deletion,
                old_span: Some(test_span(
                    recovered_block.0,
                    0,
                    recovered_text.chars().count(),
                )),
                new_span: None,
                confidence: Confidence::High,
                tags: Vec::new(),
            }]
        );
        assert_eq!(
            result.old_coverage.resolved_tokens,
            recovered_text.chars().count()
        );
        assert_eq!(result.new_coverage.resolved_tokens, 0);
    }

    #[test]
    fn full_block_runs_stop_at_partial_recovery_and_keep_exact_local_gaps_and_separator() {
        let partial = "aa111bb222cc";
        let first_recovered = TokenRange { start: 2, end: 5 };
        let second_recovered = TokenRange { start: 7, end: 10 };
        let evidence = vec![AlignmentEvidence::ReadingOrderUnknown];
        let old = vec![
            sentence_block(1, "before-one"),
            sentence_block(2, "before-two"),
            sentence_block(3, partial),
            sentence_block(4, "after-one"),
            sentence_block(5, "after-two"),
        ];
        let new = vec![sentence_block(6, "Keep. Tail.")];
        let mut alignment = unresolved_alignment(&old, &new, evidence.clone());
        alignment.spans[0].old_separator = Some(BlockSeparator::Space);
        let (old_side, new_side) = inspect_sides_with_budget(&old, &new, DiffOptions::default())
            .expect("fixture sides are valid");
        let old_side = old_side.materialize().expect("old side materializes");
        let new_side = new_side.materialize().expect("new side materializes");
        let recovery = sentence::SentenceRecoveryPlan {
            deletions: vec![
                sentence::LocalSentenceRange {
                    block: BlockId(3),
                    canonical: ScalarRange {
                        start: first_recovered.start,
                        end: first_recovered.end,
                    },
                    comparable: first_recovered,
                },
                sentence::LocalSentenceRange {
                    block: BlockId(3),
                    canonical: ScalarRange {
                        start: second_recovered.start,
                        end: second_recovered.end,
                    },
                    comparable: second_recovered,
                },
            ],
            ..sentence::SentenceRecoveryPlan::default()
        };
        let mut changes = Vec::new();
        let mut unresolved_regions = Vec::new();
        let mut resolved_old = 0;
        let mut resolved_new = 0;
        let mut output_budget = RecoveryOutputBudget::default();
        assert!(append_sentence_recovery(
            &old_side,
            &new_side,
            &alignment.spans[0],
            &recovery,
            &mut changes,
            &mut unresolved_regions,
            &mut resolved_old,
            &mut resolved_new,
            &mut output_budget,
        ));

        assert_eq!(
            changes,
            vec![
                Change {
                    kind: ChangeKind::Deletion,
                    old_span: Some(test_span(3, first_recovered.start, first_recovered.end,)),
                    new_span: None,
                    confidence: Confidence::High,
                    tags: Vec::new(),
                },
                Change {
                    kind: ChangeKind::Deletion,
                    old_span: Some(test_span(3, second_recovered.start, second_recovered.end,)),
                    new_span: None,
                    confidence: Confidence::High,
                    tags: Vec::new(),
                },
            ]
        );
        assert_eq!(
            coverage(resolved_old, old_side.total_tokens).resolved_tokens,
            first_recovered.end - first_recovered.start + second_recovered.end
                - second_recovered.start
        );
        assert_eq!(
            coverage(resolved_new, new_side.total_tokens).resolved_tokens,
            0
        );
        assert_eq!(
            unresolved_regions,
            vec![
                UnresolvedRegion {
                    old_span: Some(TextSpan {
                        blocks: vec![BlockId(1), BlockId(2)],
                        separator: Some(BlockSeparator::Space),
                        canonical_range: ScalarRange { start: 0, end: 21 },
                        comparable_range: TokenRange { start: 0, end: 21 },
                    }),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: Some(test_span(3, 0, first_recovered.start)),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: Some(test_span(3, first_recovered.end, second_recovered.start)),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: Some(test_span(3, second_recovered.end, partial.chars().count(),)),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: Some(TextSpan {
                        blocks: vec![BlockId(4), BlockId(5)],
                        separator: Some(BlockSeparator::Space),
                        canonical_range: ScalarRange { start: 0, end: 19 },
                        comparable_range: TokenRange { start: 0, end: 19 },
                    }),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: None,
                    new_span: Some(test_span(6, 0, "Keep. Tail.".chars().count())),
                    evidence,
                },
            ]
        );
    }

    #[test]
    fn repeated_and_one_to_one_sentences_are_not_recovered() {
        let repeated = vec![sentence_block(1, "Repeat sentence. Repeat sentence.")];
        let repeated_result = compare_sentence_recovery(
            &repeated,
            &[],
            &[Some(TrustedRunId(1))],
            &[],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(repeated_result.changes.is_empty());

        let old = vec![sentence_block(2, "Same sentence.")];
        let new = vec![sentence_block(3, "Same sentence.")];
        let one_to_one = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(2))],
            &[Some(TrustedRunId(3))],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(one_to_one.changes.is_empty());
    }

    #[test]
    fn modified_sentences_stay_unresolved_symmetrically() {
        let before = vec![sentence_block(
            10,
            "The reviewed clause keeps every value except alpha.",
        )];
        let after = vec![sentence_block(
            11,
            "The reviewed clause keeps every value except beta.",
        )];

        for (old, new) in [(&before, &after), (&after, &before)] {
            let alignment =
                unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
            assert_recovery_keeps_original_unresolved(
                old,
                new,
                &alignment,
                5,
                DiffOptions::default(),
            );
        }
    }

    #[test]
    fn veto_only_near_occurrences_cover_untrusted_issue_and_unmapped_blocks() {
        let clean_text = "Alpha value remains stable throughout this reviewed clause.";
        let near_text = "Beta value remains stable throughout this reviewed clause.";
        let clean = vec![sentence_block(12, clean_text)];
        let mut issue = sentence_block(13, near_text);
        issue.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource { atoms: Vec::new() },
        });

        for (opposite, opposite_run) in [
            (vec![sentence_block(14, near_text)], vec![None]),
            (vec![issue], vec![Some(TrustedRunId(2))]),
            (
                vec![sentence_block_with_unmapped(15, near_text)],
                vec![Some(TrustedRunId(3))],
            ),
        ] {
            assert_recovery_with_run_ids_keeps_original_unresolved(
                &clean,
                &opposite,
                &[Some(TrustedRunId(1))],
                &opposite_run,
                5,
            );
            assert_recovery_with_run_ids_keeps_original_unresolved(
                &opposite,
                &clean,
                &opposite_run,
                &[Some(TrustedRunId(1))],
                5,
            );
        }
    }

    #[test]
    fn cross_block_veto_only_occurrence_suppresses_near_change_symmetrically() {
        let clean = vec![sentence_block(
            16,
            "Alpha value remains stable throughout this reviewed clause.",
        )];
        let split = vec![
            sentence_block(17, "Beta value remains stable"),
            sentence_block(18, "throughout this reviewed clause."),
        ];
        let clean_run = [Some(TrustedRunId(1))];
        let split_run = [Some(TrustedRunId(2)), Some(TrustedRunId(2))];

        assert_recovery_with_run_ids_keeps_original_unresolved(
            &clean, &split, &clean_run, &split_run, 5,
        );
        assert_recovery_with_run_ids_keeps_original_unresolved(
            &split, &clean, &split_run, &clean_run, 5,
        );
    }

    #[test]
    fn occurrence_crossing_alignment_spans_fails_closed_symmetrically() {
        let clean = vec![sentence_block(
            19,
            "Alpha value remains stable throughout this reviewed clause.",
        )];
        let split = vec![
            sentence_block(20, "Beta value remains stable"),
            sentence_block(21, "throughout this reviewed clause."),
        ];
        let forward = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(19)], vec![BlockId(20)]),
                reading_order_unknown_span(Vec::new(), vec![BlockId(21)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        assert_recovery_keeps_original_unresolved(
            &clean,
            &split,
            &forward,
            5,
            DiffOptions::default(),
        );

        let reverse = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(20)], vec![BlockId(19)]),
                reading_order_unknown_span(vec![BlockId(21)], Vec::new()),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        assert_recovery_keeps_original_unresolved(
            &split,
            &clean,
            &reverse,
            5,
            DiffOptions::default(),
        );
    }

    #[test]
    fn one_to_many_modified_sentences_veto_every_incident_candidate() {
        let one = vec![sentence_block(
            20,
            "The reviewed clause keeps every value except alpha.",
        )];
        let many = vec![sentence_block(
            21,
            concat!(
                "The reviewed clause keeps every value except beta. ",
                "The reviewed clause keeps every value except gamma."
            ),
        )];

        for (old, new) in [(&one, &many), (&many, &one)] {
            let alignment =
                unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
            assert_recovery_keeps_original_unresolved(
                old,
                new,
                &alignment,
                5,
                DiffOptions::default(),
            );
        }
    }

    #[test]
    fn modified_sentence_veto_does_not_cross_unresolved_spans() {
        let old = vec![sentence_block(
            30,
            "The reviewed clause keeps every value except alpha.",
        )];
        let new = vec![sentence_block(
            31,
            "The reviewed clause keeps every value except beta.",
        )];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(30)], Vec::new()),
                reading_order_unknown_span(Vec::new(), vec![BlockId(31)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let result = compare_sentence_recovery_with_intervals(
            &old,
            &new,
            &alignment,
            &trusted_run_intervals(&[Some(TrustedRunId(1))]),
            &trusted_run_intervals(&[Some(TrustedRunId(2))]),
            5,
            DiffOptions::default(),
        );

        assert_eq!(result.changes.len(), 2);
        assert!(
            result
                .changes
                .iter()
                .any(|change| change.kind == ChangeKind::Deletion)
        );
        assert!(
            result
                .changes
                .iter()
                .any(|change| change.kind == ChangeKind::Insertion)
        );
        assert!(
            result
                .changes
                .iter()
                .all(|change| change.kind != ChangeKind::Replacement)
        );
    }

    #[test]
    fn cross_span_near_occurrence_vetoes_only_incident_candidate_symmetrically() {
        for reverse in [false, true] {
            let result = compare_cross_span_occurrence(
                "The reviewed clause keeps every value",
                "except beta. Trailing",
                reverse,
            );

            assert_recovered_clean_blocks(&result, reverse, &[BlockId(101)]);
        }
    }

    #[test]
    fn cross_span_dissimilar_occurrence_keeps_unrelated_recovery_symmetrically() {
        for reverse in [false, true] {
            let result = compare_cross_span_occurrence(
                "A wholly different statement describes",
                "unrelated archival material. Trailing",
                reverse,
            );

            assert_recovered_clean_blocks(&result, reverse, &[BlockId(100), BlockId(101)]);
        }
    }

    #[test]
    fn cross_span_exact_occurrence_participates_in_global_counts_symmetrically() {
        for reverse in [false, true] {
            let result = compare_cross_span_occurrence(
                "The reviewed clause keeps every value",
                "except alpha. Trailing",
                reverse,
            );

            assert_recovered_clean_blocks(&result, reverse, &[BlockId(101)]);
        }
    }

    #[test]
    fn unrelated_unique_sentences_remain_exact_deletion_and_insertion() {
        let old = vec![sentence_block(
            40,
            "Completely obsolete language appears here.",
        )];
        let new = vec![sentence_block(
            41,
            "A fresh and unrelated statement replaces it.",
        )];
        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[0].kind, ChangeKind::Deletion);
        assert_eq!(result.changes[1].kind, ChangeKind::Insertion);
        assert!(
            result
                .changes
                .iter()
                .all(|change| change.kind != ChangeKind::Replacement)
        );
    }

    #[test]
    fn recovery_budget_exhaustion_returns_no_partial_plan_in_both_directions() {
        let before_text = (0..40)
            .map(|index| format!("A{index:02}."))
            .collect::<Vec<_>>()
            .join(" ");
        let after_text = (0..40)
            .map(|index| format!("B{index:02}."))
            .collect::<Vec<_>>()
            .join(" ");
        let before = vec![sentence_block(50, &before_text)];
        let after = vec![sentence_block(51, &after_text)];

        for (old, new) in [(&before, &after), (&after, &before)] {
            let alignment =
                unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
            assert_recovery_keeps_original_unresolved(
                old,
                new,
                &alignment,
                1,
                DiffOptions::default(),
            );
        }
    }

    #[test]
    fn span_output_failure_emits_only_original_unresolved_in_both_directions() {
        let old = vec![sentence_block(52, "Unique old sentence.")];
        let new = vec![sentence_block(53, "Unique new sentence.")];
        let alignment =
            unresolved_alignment(&old, &new, vec![AlignmentEvidence::ReadingOrderUnknown]);
        let (old_side, new_side) = inspect_sides_with_budget(&old, &new, DiffOptions::default())
            .expect("fixture sides are valid");
        let old_side = old_side.materialize().expect("old side materializes");
        let new_side = new_side.materialize().expect("new side materializes");

        for recover_old in [true, false] {
            let mut recovery = sentence::SentenceRecoveryPlan::default();
            let (block, end) = if recover_old {
                (BlockId(52), "Unique old sentence.".chars().count())
            } else {
                (BlockId(53), "Unique new sentence.".chars().count())
            };
            let range = sentence::LocalSentenceRange {
                block,
                canonical: ScalarRange { start: 0, end },
                comparable: TokenRange { start: 0, end },
            };
            if recover_old {
                recovery.deletions.push(range);
            } else {
                recovery.insertions.push(range);
            }

            let mut changes = Vec::new();
            let mut unresolved_regions = Vec::new();
            let mut resolved_old = 7;
            let mut resolved_new = 11;
            let mut output_budget = RecoveryOutputBudget::with_limits(RecoveryOutputLimits {
                max_items: 1,
                max_bytes: usize::MAX,
            });
            apply_sentence_recovery_or_fallback(
                &old_side,
                &new_side,
                &alignment.spans[0],
                &recovery,
                &mut changes,
                &mut unresolved_regions,
                &mut resolved_old,
                &mut resolved_new,
                &mut output_budget,
            );
            assert!(changes.is_empty());
            assert_eq!(
                unresolved_regions,
                vec![UnresolvedRegion {
                    old_span: Some(test_span(52, 0, "Unique old sentence.".chars().count())),
                    new_span: Some(test_span(53, 0, "Unique new sentence.".chars().count())),
                    evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                }]
            );
            assert_eq!((resolved_old, resolved_new), (7, 11));
            assert_eq!((output_budget.items, output_budget.bytes), (0, 0));
        }
    }

    #[test]
    fn opposite_sentence_split_across_one_trusted_run_vetoes_recovery() {
        let old = vec![sentence_block(1, "Shared sentence.")];
        let new = vec![sentence_block(2, "Shared"), sentence_block(3, "sentence.")];
        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2)), Some(TrustedRunId(2))],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(result.changes.is_empty());
    }

    #[test]
    fn different_run_fragments_remain_veto_only_near_evidence() {
        let old = vec![sentence_block(1, "Shared sentence.")];
        let new = vec![sentence_block(2, "Shared"), sentence_block(3, "sentence.")];
        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2)), Some(TrustedRunId(3))],
            10,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(result.changes.is_empty());
    }

    #[test]
    fn sentence_crossing_source_blocks_is_presence_only() {
        let old = vec![sentence_block(1, "Unique"), sentence_block(2, "sentence.")];
        let result = compare_sentence_recovery(
            &old,
            &[],
            &[Some(TrustedRunId(1)), Some(TrustedRunId(1))],
            &[],
            1,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(result.changes.is_empty());
        assert_eq!(result.unresolved_regions.len(), 1);
        assert_eq!(
            result.unresolved_regions[0]
                .old_span
                .as_ref()
                .map(|span| &span.blocks),
            Some(&vec![BlockId(1), BlockId(2)])
        );
    }

    #[test]
    fn issue_unmapped_and_untrusted_blocks_veto_but_never_emit() {
        let clean = vec![sentence_block(1, "Guarded sentence.")];
        let mut issue = sentence_block(2, "Guarded sentence.");
        issue.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource { atoms: Vec::new() },
        });
        let unmapped = sentence_block_with_unmapped(3, "Guarded sentence.");

        for (opposite, run) in [
            (vec![issue.clone()], vec![Some(TrustedRunId(2))]),
            (vec![unmapped.clone()], vec![Some(TrustedRunId(3))]),
            (vec![sentence_block(4, "Guarded sentence.")], vec![None]),
        ] {
            let result = compare_sentence_recovery(
                &clean,
                &opposite,
                &[Some(TrustedRunId(1))],
                &run,
                5,
                vec![AlignmentEvidence::ReadingOrderUnknown],
            );
            assert!(result.changes.is_empty());
        }

        for (source, run) in [
            (vec![issue], vec![Some(TrustedRunId(2))]),
            (vec![unmapped], vec![Some(TrustedRunId(3))]),
            (vec![sentence_block(4, "Guarded sentence.")], vec![None]),
        ] {
            let result = compare_sentence_recovery(
                &source,
                &[],
                &run,
                &[],
                5,
                vec![AlignmentEvidence::ReadingOrderUnknown],
            );
            assert!(result.changes.is_empty());
        }
    }

    #[test]
    fn minimum_token_and_exact_evidence_gates_are_fail_closed() {
        let old = vec![sentence_block(1, "Short sentence.")];
        let run = [Some(TrustedRunId(1))];
        let too_short = compare_sentence_recovery(
            &old,
            &[],
            &run,
            &[],
            "Short sentence.".chars().count() + 1,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(too_short.changes.is_empty());

        let extra_evidence = compare_sentence_recovery(
            &old,
            &[],
            &run,
            &[],
            1,
            vec![
                AlignmentEvidence::ReadingOrderUnknown,
                AlignmentEvidence::NormalizationIssue,
            ],
        );
        assert!(extra_evidence.changes.is_empty());
    }

    #[test]
    fn invalid_sentence_recovery_metadata_is_rejected() {
        let old = vec![sentence_block(1, "Sentence.")];
        let alignment =
            unresolved_alignment(&old, &[], vec![AlignmentEvidence::ReadingOrderUnknown]);
        let error = compare_aligned_with_sentence_recovery(
            &old,
            &[],
            &alignment,
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &[],
                new_trusted_run_intervals: &[],
                min_tokens: 1,
            },
        )
        .expect_err("metadata length mismatch must be rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)));

        let error = compare_aligned_with_sentence_recovery(
            &old,
            &[],
            &alignment,
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &[Some(TrustedRunInterval {
                    run_id: TrustedRunId(1),
                    start: 0,
                    end: 1,
                })],
                new_trusted_run_intervals: &[],
                min_tokens: 0,
            },
        )
        .expect_err("zero sentence threshold must be rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)));
    }

    #[test]
    fn zero_recovery_is_structurally_identical_to_public_comparison() {
        let old = vec![sentence_block(1, "Same sentence.")];
        let new = vec![sentence_block(2, "Same sentence.")];
        let alignment =
            unresolved_alignment(&old, &new, vec![AlignmentEvidence::ReadingOrderUnknown]);
        let public = compare_aligned(&old, &new, &alignment, DiffOptions::default())
            .expect("public comparison succeeds");
        let recovered = compare_aligned_with_sentence_recovery(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &[Some(TrustedRunInterval {
                    run_id: TrustedRunId(1),
                    start: 0,
                    end: 1,
                })],
                new_trusted_run_intervals: &[Some(TrustedRunInterval {
                    run_id: TrustedRunId(2),
                    start: 0,
                    end: 1,
                })],
                min_tokens: 1,
            },
        )
        .expect("recovery comparison succeeds");
        assert_eq!(recovered, public);
    }

    fn compare_sentence_recovery(
        old: &[BlockText],
        new: &[BlockText],
        old_trusted_run_ids: &[Option<TrustedRunId>],
        new_trusted_run_ids: &[Option<TrustedRunId>],
        min_tokens: usize,
        evidence: Vec<AlignmentEvidence>,
    ) -> Comparison {
        let alignment = unresolved_alignment(old, new, evidence);
        let old_trusted_run_intervals = trusted_run_intervals(old_trusted_run_ids);
        let new_trusted_run_intervals = trusted_run_intervals(new_trusted_run_ids);
        compare_sentence_recovery_with_intervals(
            old,
            new,
            &alignment,
            &old_trusted_run_intervals,
            &new_trusted_run_intervals,
            min_tokens,
            DiffOptions::default(),
        )
    }

    fn compare_sentence_recovery_with_intervals(
        old: &[BlockText],
        new: &[BlockText],
        alignment: &Alignment,
        old_trusted_run_intervals: &[Option<TrustedRunInterval>],
        new_trusted_run_intervals: &[Option<TrustedRunInterval>],
        min_tokens: usize,
        options: DiffOptions,
    ) -> Comparison {
        compare_aligned_with_sentence_recovery(
            old,
            new,
            alignment,
            options,
            SentenceRecoveryInput {
                old_trusted_run_intervals,
                new_trusted_run_intervals,
                min_tokens,
            },
        )
        .expect("sentence recovery comparison succeeds")
    }

    fn compare_cross_span_occurrence(
        cross_span_start: &str,
        cross_span_end: &str,
        reverse: bool,
    ) -> Comparison {
        let clean = vec![
            sentence_block(100, "The reviewed clause keeps every value except alpha."),
            sentence_block(
                101,
                "A separate obsolete provision belongs only to this span.",
            ),
        ];
        let cross_span = vec![
            sentence_block(102, cross_span_start),
            sentence_block(103, cross_span_end),
        ];
        let alignment = Alignment {
            spans: if reverse {
                vec![
                    reading_order_unknown_span(vec![BlockId(102)], vec![BlockId(100)]),
                    reading_order_unknown_span(vec![BlockId(103)], vec![BlockId(101)]),
                ]
            } else {
                vec![
                    reading_order_unknown_span(vec![BlockId(100)], vec![BlockId(102)]),
                    reading_order_unknown_span(vec![BlockId(101)], vec![BlockId(103)]),
                ]
            },
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let clean_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(100)), Some(TrustedRunId(101))]);
        let cross_span_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(102)), Some(TrustedRunId(102))]);
        let (old, new, old_intervals, new_intervals) = if reverse {
            (
                cross_span.as_slice(),
                clean.as_slice(),
                cross_span_intervals.as_slice(),
                clean_intervals.as_slice(),
            )
        } else {
            (
                clean.as_slice(),
                cross_span.as_slice(),
                clean_intervals.as_slice(),
                cross_span_intervals.as_slice(),
            )
        };

        compare_sentence_recovery_with_intervals(
            old,
            new,
            &alignment,
            old_intervals,
            new_intervals,
            5,
            DiffOptions::default(),
        )
    }

    fn assert_recovered_clean_blocks(
        result: &Comparison,
        reverse: bool,
        expected_blocks: &[BlockId],
    ) {
        let expected_kind = if reverse {
            ChangeKind::Insertion
        } else {
            ChangeKind::Deletion
        };
        let recovered_blocks = result
            .changes
            .iter()
            .map(|change| {
                assert_eq!(change.kind, expected_kind);
                let (recovered, absent) = if reverse {
                    (&change.new_span, &change.old_span)
                } else {
                    (&change.old_span, &change.new_span)
                };
                assert!(absent.is_none());
                let recovered = recovered
                    .as_ref()
                    .expect("sentence recovery emits the expected side");
                assert_eq!(recovered.blocks.len(), 1);
                recovered.blocks[0]
            })
            .collect::<Vec<_>>();
        assert_eq!(recovered_blocks, expected_blocks);
    }

    fn assert_recovery_keeps_original_unresolved(
        old: &[BlockText],
        new: &[BlockText],
        alignment: &Alignment,
        min_tokens: usize,
        options: DiffOptions,
    ) {
        let public = compare_aligned(old, new, alignment, options)
            .expect("comparison without recovery succeeds");
        let old_run_ids = vec![Some(TrustedRunId(101)); old.len()];
        let new_run_ids = vec![Some(TrustedRunId(102)); new.len()];
        let recovered = compare_sentence_recovery_with_intervals(
            old,
            new,
            alignment,
            &trusted_run_intervals(&old_run_ids),
            &trusted_run_intervals(&new_run_ids),
            min_tokens,
            options,
        );
        assert_eq!(recovered, public);
    }

    fn assert_recovery_with_run_ids_keeps_original_unresolved(
        old: &[BlockText],
        new: &[BlockText],
        old_trusted_run_ids: &[Option<TrustedRunId>],
        new_trusted_run_ids: &[Option<TrustedRunId>],
        min_tokens: usize,
    ) {
        let alignment =
            unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
        let public = compare_aligned(old, new, &alignment, DiffOptions::default())
            .expect("comparison without recovery succeeds");
        let recovered = compare_sentence_recovery(
            old,
            new,
            old_trusted_run_ids,
            new_trusted_run_ids,
            min_tokens,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert_eq!(recovered, public);
    }

    fn trusted_run_intervals(run_ids: &[Option<TrustedRunId>]) -> Vec<Option<TrustedRunInterval>> {
        let mut next_by_run = std::collections::HashMap::<TrustedRunId, usize>::new();
        run_ids
            .iter()
            .map(|run_id| {
                run_id.map(|run_id| {
                    let start = *next_by_run.entry(run_id).or_default();
                    let end = start.checked_add(1).expect("test interval fits usize");
                    next_by_run.insert(run_id, end);
                    TrustedRunInterval { run_id, start, end }
                })
            })
            .collect()
    }

    fn unresolved_alignment(
        old: &[BlockText],
        new: &[BlockText],
        evidence: Vec<AlignmentEvidence>,
    ) -> Alignment {
        Alignment {
            spans: vec![AlignmentSpan {
                evidence,
                ..reading_order_unknown_span(
                    old.iter().map(|block| block.block).collect(),
                    new.iter().map(|block| block.block).collect(),
                )
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    fn reading_order_unknown_span(old: Vec<BlockId>, new: Vec<BlockId>) -> AlignmentSpan {
        AlignmentSpan {
            kind: AlignmentKind::Unresolved,
            old,
            new,
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
            old_separator: None,
            new_separator: None,
        }
    }

    fn sentence_block(id: u64, text: &str) -> BlockText {
        BlockText {
            block: BlockId(id),
            raw: test_mapped(text),
            canonical: test_mapped(text),
            matching: text.to_owned(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    fn sentence_block_with_unmapped(id: u64, text: &str) -> BlockText {
        let mut block = sentence_block(id, text);
        block.canonical.unmapped.push(UnmappedToken {
            scalar_index: 0,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 1,
            source: TextSource { atoms: Vec::new() },
        });
        block
    }

    fn test_mapped(text: &str) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        }
    }

    fn test_span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }
}
