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

/// Constant-space diagnostics for sentence recovery inside uncertain spans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SentenceRecoveryMetrics {
    pub old_trusted_run_source_tokens: usize,
    pub new_trusted_run_source_tokens: usize,
    pub exact_shared_units: usize,
    pub old_exact_one_sided_units: usize,
    pub new_exact_one_sided_units: usize,
    pub near_relation_complete: bool,
    pub near_pair_candidates: usize,
    pub vetoed_near_pairs: usize,
    pub recovered_exact_match_old_tokens: usize,
    pub recovered_exact_match_new_tokens: usize,
    pub recovered_replacement_old_tokens: usize,
    pub recovered_replacement_new_tokens: usize,
    pub recovered_deletion_tokens: usize,
    pub recovered_insertion_tokens: usize,
    pub unresolved_remainder_old_source_tokens: usize,
    pub unresolved_remainder_new_source_tokens: usize,
}

pub(crate) struct ComparisonWithSentenceRecoveryMetrics {
    pub(crate) comparison: Comparison,
    pub(crate) sentence_recovery_metrics: Option<SentenceRecoveryMetrics>,
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
    /// Base changed-token ratio for rejecting implausible non-exact matches.
    /// Low-confidence matches use this value directly. Medium- and
    /// high-confidence matches use progressively relaxed multiples, with
    /// additional short-span headroom because a small replacement has a high
    /// edit-operation ratio even when it forms one coherent change.
    ///
    /// The field name is retained for API compatibility; the guard applies to
    /// every alignment confidence because confidence cannot prove a changed
    /// counterpart is plausible after exact diff evidence contradicts it.
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
    compare_aligned_inner(
        old,
        new,
        alignment,
        options,
        None,
        RecoveryOutputLimits::default(),
    )
    .map(|outcome| outcome.comparison)
}

#[cfg(test)]
pub(crate) fn compare_aligned_with_sentence_recovery(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
    recovery: SentenceRecoveryInput<'_>,
) -> Result<Comparison> {
    compare_aligned_inner(
        old,
        new,
        alignment,
        options,
        Some(recovery),
        RecoveryOutputLimits::default(),
    )
    .map(|outcome| outcome.comparison)
}

pub(crate) fn compare_aligned_with_sentence_recovery_metrics(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
    recovery: SentenceRecoveryInput<'_>,
) -> Result<ComparisonWithSentenceRecoveryMetrics> {
    compare_aligned_inner(
        old,
        new,
        alignment,
        options,
        Some(recovery),
        RecoveryOutputLimits::default(),
    )
}

fn compare_aligned_inner(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
    recovery: Option<SentenceRecoveryInput<'_>>,
    recovery_output_limits: RecoveryOutputLimits,
) -> Result<ComparisonWithSentenceRecoveryMetrics> {
    if let Some(recovery) = recovery {
        validate_sentence_recovery_input(old, new, recovery)?;
    }
    let (old, new) = inspect_sides_with_budget(old, new, options)?;
    let old = old.materialize()?;
    let new = new.materialize()?;
    validate_alignment(&old, &new, alignment)?;
    let mut sentence_recovery = match recovery {
        Some(recovery) => sentence::build_sentence_recovery_plan(
            &old,
            &new,
            alignment,
            recovery,
            options.max_tokens,
        )?,
        None => sentence::SentenceRecoveryBuildOutcome::default(),
    };
    let (moves_by_old, moves_by_new) = promotable_moves(&old, &new, alignment);

    let mut changes = Vec::new();
    let mut formatting_changes = Vec::new();
    let mut unresolved_regions = Vec::new();
    let mut resolved_old = 0;
    let mut resolved_new = 0;
    let mut sentence_recovery_output_budget = RecoveryOutputBudget {
        limits: recovery_output_limits,
        items: 0,
        bytes: 0,
    };
    let requires_atomic_recovery = sentence_recovery
        .plan
        .as_ref()
        .is_some_and(sentence::SentenceRecoveryPlan::has_cross_span_recovery);
    let mut atomic_recovery = requires_atomic_recovery
        .then(|| {
            prepare_sentence_recovery_batch(
                &old,
                &new,
                alignment,
                sentence_recovery.plan.as_ref()?,
                &mut sentence_recovery_output_budget,
            )
        })
        .flatten();
    if requires_atomic_recovery && atomic_recovery.is_none() {
        sentence_recovery.plan = None;
    }
    let mut atomic_recovery_cursor = 0usize;

    for (span_index, span) in alignment.spans.iter().enumerate() {
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
                if let Some(entry) = atomic_recovery
                    .as_mut()
                    .and_then(|batch| batch.entries.get_mut(atomic_recovery_cursor))
                    .filter(|entry| entry.span_index == span_index)
                {
                    entry.fallback_change_index = changes.len();
                    entry.fallback_unresolved_index = unresolved_regions.len();
                    atomic_recovery_cursor += 1;
                    unresolved_regions.push(UnresolvedRegion {
                        old_span: full_span(&old, &span.old, span.old_separator),
                        new_span: full_span(&new, &span.new, span.new_separator),
                        evidence: span.evidence.clone(),
                    });
                } else if let Some(recovery) = &sentence_recovery.plan
                    && recovery.has_recovery(span_index)
                {
                    let committed = apply_sentence_recovery_or_fallback(
                        &old,
                        &new,
                        span_index,
                        span,
                        recovery,
                        &mut changes,
                        &mut unresolved_regions,
                        &mut resolved_old,
                        &mut resolved_new,
                        &mut sentence_recovery_output_budget,
                    );
                    if let Some(committed) = committed {
                        sentence_recovery.record_committed(committed);
                    }
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

    if let Some(batch) = atomic_recovery
        && let Some(committed) = commit_prepared_sentence_recovery_batch(
            batch,
            &mut changes,
            &mut unresolved_regions,
            &mut resolved_old,
            &mut resolved_new,
        )
    {
        sentence_recovery.record_committed(committed);
    }

    Ok(ComparisonWithSentenceRecoveryMetrics {
        comparison: Comparison {
            changes,
            formatting_changes,
            unresolved_regions,
            old_coverage: coverage(resolved_old, old.total_tokens),
            new_coverage: coverage(resolved_new, new.total_tokens),
        },
        sentence_recovery_metrics: sentence_recovery.finish_metrics(),
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
        self.charge_many(1, estimated_bytes)
    }

    fn charge_many(&mut self, item_count: usize, estimated_bytes: usize) -> bool {
        let Some(items) = self.items.checked_add(item_count) else {
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
    committed: SentenceRecoveryCommittedTokens,
}

struct PreparedSentenceRecoveryBatch {
    entries: Vec<PreparedSentenceRecoveryEntry>,
}

struct PreparedSentenceRecoveryEntry {
    span_index: usize,
    fallback_change_index: usize,
    fallback_unresolved_index: usize,
    prepared: PreparedSentenceRecovery,
}

#[derive(Clone, Copy, Default)]
struct SentenceRecoveryCommittedTokens {
    exact_match_old: usize,
    exact_match_new: usize,
    replacement_old: usize,
    replacement_new: usize,
    deletion: usize,
    insertion: usize,
}

impl SentenceRecoveryCommittedTokens {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            exact_match_old: self.exact_match_old.checked_add(other.exact_match_old)?,
            exact_match_new: self.exact_match_new.checked_add(other.exact_match_new)?,
            replacement_old: self.replacement_old.checked_add(other.replacement_old)?,
            replacement_new: self.replacement_new.checked_add(other.replacement_new)?,
            deletion: self.deletion.checked_add(other.deletion)?,
            insertion: self.insertion.checked_add(other.insertion)?,
        })
    }
}

fn prepare_sentence_recovery_batch(
    old: &Side<'_>,
    new: &Side<'_>,
    alignment: &Alignment,
    recovery: &sentence::SentenceRecoveryPlan,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<PreparedSentenceRecoveryBatch> {
    let mut tentative_budget = *output_budget;
    let mut entries = Vec::new();
    for (span_index, span) in alignment.spans.iter().enumerate() {
        if span.kind != AlignmentKind::Unresolved || !recovery.has_recovery(span_index) {
            continue;
        }
        if entries.len() == sentence::MAX_SENTENCE_RECOVERY_RANGES {
            return None;
        }
        let prepared =
            prepare_sentence_recovery(old, new, span_index, span, recovery, &mut tentative_budget)?;
        entries.try_reserve(1).ok()?;
        entries.push(PreparedSentenceRecoveryEntry {
            span_index,
            fallback_change_index: usize::MAX,
            fallback_unresolved_index: usize::MAX,
            prepared,
        });
    }
    *output_budget = tentative_budget;
    Some(PreparedSentenceRecoveryBatch { entries })
}

fn commit_prepared_sentence_recovery_batch(
    mut batch: PreparedSentenceRecoveryBatch,
    changes: &mut Vec<Change>,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    resolved_old: &mut usize,
    resolved_new: &mut usize,
) -> Option<SentenceRecoveryCommittedTokens> {
    let mut change_count = changes.len();
    let mut unresolved_count = unresolved_regions.len().checked_sub(batch.entries.len())?;
    let mut recovered_old = 0usize;
    let mut recovered_new = 0usize;
    let mut committed = SentenceRecoveryCommittedTokens::default();
    let mut previous_change_index = 0usize;
    let mut previous_unresolved_index = None;
    for entry in &batch.entries {
        if entry.fallback_change_index < previous_change_index
            || entry.fallback_change_index > changes.len()
            || entry.fallback_unresolved_index >= unresolved_regions.len()
            || previous_unresolved_index
                .is_some_and(|previous| entry.fallback_unresolved_index <= previous)
        {
            return None;
        }
        previous_change_index = entry.fallback_change_index;
        previous_unresolved_index = Some(entry.fallback_unresolved_index);
        change_count = change_count.checked_add(entry.prepared.changes.len())?;
        unresolved_count = unresolved_count.checked_add(entry.prepared.unresolved_regions.len())?;
        recovered_old = recovered_old.checked_add(entry.prepared.resolved_old)?;
        recovered_new = recovered_new.checked_add(entry.prepared.resolved_new)?;
        committed = committed.checked_add(entry.prepared.committed)?;
    }
    let next_resolved_old = resolved_old.checked_add(recovered_old)?;
    let next_resolved_new = resolved_new.checked_add(recovered_new)?;

    let mut merged_changes = Vec::new();
    let mut merged_unresolved = Vec::new();
    merged_changes.try_reserve_exact(change_count).ok()?;
    merged_unresolved.try_reserve_exact(unresolved_count).ok()?;

    let mut fallback_changes = std::mem::take(changes).into_iter().enumerate().peekable();
    for entry in &mut batch.entries {
        while fallback_changes
            .peek()
            .is_some_and(|(index, _)| *index < entry.fallback_change_index)
        {
            merged_changes.push(
                fallback_changes
                    .next()
                    .expect("validated fallback change index remains available")
                    .1,
            );
        }
        merged_changes.append(&mut entry.prepared.changes);
    }
    merged_changes.extend(fallback_changes.map(|(_, change)| change));

    let mut fallback_unresolved = std::mem::take(unresolved_regions)
        .into_iter()
        .enumerate()
        .peekable();
    for entry in &mut batch.entries {
        while fallback_unresolved
            .peek()
            .is_some_and(|(index, _)| *index < entry.fallback_unresolved_index)
        {
            merged_unresolved.push(
                fallback_unresolved
                    .next()
                    .expect("validated fallback unresolved index remains available")
                    .1,
            );
        }
        let (index, _) = fallback_unresolved
            .next()
            .expect("validated fallback unresolved replacement remains available");
        debug_assert_eq!(index, entry.fallback_unresolved_index);
        merged_unresolved.append(&mut entry.prepared.unresolved_regions);
    }
    merged_unresolved.extend(fallback_unresolved.map(|(_, region)| region));

    *changes = merged_changes;
    *unresolved_regions = merged_unresolved;
    *resolved_old = next_resolved_old;
    *resolved_new = next_resolved_new;
    Some(committed)
}

#[allow(clippy::too_many_arguments)]
fn apply_sentence_recovery_or_fallback(
    old: &Side<'_>,
    new: &Side<'_>,
    span_index: usize,
    span: &AlignmentSpan,
    recovery: &sentence::SentenceRecoveryPlan,
    changes: &mut Vec<Change>,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    resolved_old: &mut usize,
    resolved_new: &mut usize,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<SentenceRecoveryCommittedTokens> {
    let committed = append_sentence_recovery(
        old,
        new,
        span_index,
        span,
        recovery,
        changes,
        unresolved_regions,
        resolved_old,
        resolved_new,
        output_budget,
    );
    if committed.is_none() {
        unresolved_regions.push(UnresolvedRegion {
            old_span: full_span(old, &span.old, span.old_separator),
            new_span: full_span(new, &span.new, span.new_separator),
            evidence: span.evidence.clone(),
        });
    }
    committed
}

#[allow(clippy::too_many_arguments)]
fn append_sentence_recovery(
    old: &Side<'_>,
    new: &Side<'_>,
    span_index: usize,
    span: &AlignmentSpan,
    recovery: &sentence::SentenceRecoveryPlan,
    changes: &mut Vec<Change>,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
    resolved_old: &mut usize,
    resolved_new: &mut usize,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<SentenceRecoveryCommittedTokens> {
    let mut tentative_budget = *output_budget;
    let prepared =
        prepare_sentence_recovery(old, new, span_index, span, recovery, &mut tentative_budget)?;
    let next_resolved_old = resolved_old.checked_add(prepared.resolved_old)?;
    let next_resolved_new = resolved_new.checked_add(prepared.resolved_new)?;
    if changes.try_reserve_exact(prepared.changes.len()).is_err()
        || unresolved_regions
            .try_reserve_exact(prepared.unresolved_regions.len())
            .is_err()
    {
        return None;
    }

    let committed = prepared.committed;
    changes.extend(prepared.changes);
    unresolved_regions.extend(prepared.unresolved_regions);
    *resolved_old = next_resolved_old;
    *resolved_new = next_resolved_new;
    *output_budget = tentative_budget;
    Some(committed)
}

fn prepare_sentence_recovery(
    old: &Side<'_>,
    new: &Side<'_>,
    span_index: usize,
    span: &AlignmentSpan,
    recovery: &sentence::SentenceRecoveryPlan,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<PreparedSentenceRecovery> {
    let matches = sentence::matches_for_span(&recovery.matches, span_index);
    let cross_span_match_old =
        sentence::recoveries_for_span(&recovery.cross_span_match_old, span_index);
    let cross_span_match_new =
        sentence::recoveries_for_span(&recovery.cross_span_match_new, span_index);
    let deletions = sentence::recoveries_for_span(&recovery.deletions, span_index);
    let insertions = sentence::recoveries_for_span(&recovery.insertions, span_index);
    let replacements = sentence::replacements_for_span(&recovery.replacements, span_index);
    let change_capacity = deletions
        .len()
        .checked_add(insertions.len())?
        .checked_add(replacements.len())?;
    let deletion_consumed_count = recovered_range_count(&span.old, &recovery.deletion_consumed)?;
    let insertion_consumed_count = recovered_range_count(&span.new, &recovery.insertion_consumed)?;
    let unresolved_capacity = deletion_consumed_count
        .checked_add(insertion_consumed_count)?
        .checked_mul(3)?
        .checked_add(2)?;
    let reservation_items = change_capacity.checked_add(unresolved_capacity)?;
    let reservation_bytes =
        estimated_recovery_reservation_bytes(change_capacity, unresolved_capacity)?;
    let mut reservation_budget = *output_budget;
    if !reservation_budget.charge_many(reservation_items, reservation_bytes) {
        return None;
    }
    let mut changes = Vec::new();
    let mut unresolved_regions = Vec::new();
    changes.try_reserve_exact(change_capacity).ok()?;
    unresolved_regions
        .try_reserve_exact(unresolved_capacity)
        .ok()?;

    let (same_span_match_old, same_span_match_new) = recovered_exact_match_tokens(matches)?;
    let exact_match_old =
        same_span_match_old.checked_add(recovered_sentence_tokens(cross_span_match_old)?)?;
    let exact_match_new =
        same_span_match_new.checked_add(recovered_sentence_tokens(cross_span_match_new)?)?;
    let (replacement_old, replacement_new) =
        prepare_recovered_replacements(replacements, &mut changes, output_budget)?;
    let deletion =
        prepare_recovered_changes(deletions, ChangeKind::Deletion, &mut changes, output_budget)?;
    let resolved_old = exact_match_old
        .checked_add(replacement_old)?
        .checked_add(deletion)?;
    sort_recovered_old_changes(old, &mut changes)?;
    let insertion = prepare_recovered_changes(
        insertions,
        ChangeKind::Insertion,
        &mut changes,
        output_budget,
    )?;
    let resolved_new = exact_match_new
        .checked_add(replacement_new)?
        .checked_add(insertion)?;
    prepare_unresolved_remainders(
        old,
        &span.old,
        span.old_separator,
        &recovery.deletion_consumed,
        RemainderSide::Old,
        &span.evidence,
        &mut unresolved_regions,
        output_budget,
    )?;
    prepare_unresolved_remainders(
        new,
        &span.new,
        span.new_separator,
        &recovery.insertion_consumed,
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
        committed: SentenceRecoveryCommittedTokens {
            exact_match_old,
            exact_match_new,
            replacement_old,
            replacement_new,
            deletion,
            insertion,
        },
    })
}

fn recovered_exact_match_tokens(
    matches: &[sentence::RecoveredExactMatch],
) -> Option<(usize, usize)> {
    matches
        .iter()
        .try_fold((0usize, 0usize), |(resolved_old, resolved_new), matched| {
            if matched.old.span_index != matched.new.span_index
                || !valid_recovered_sentence(&matched.old)
                || !valid_recovered_sentence(&matched.new)
            {
                return None;
            }
            Some((
                resolved_old.checked_add(matched.old.source_tokens)?,
                resolved_new.checked_add(matched.new.source_tokens)?,
            ))
        })
}

fn recovered_sentence_tokens(recovered: &[sentence::RecoveredSentence]) -> Option<usize> {
    recovered.iter().try_fold(0usize, |resolved, recovery| {
        if !valid_recovered_sentence(recovery) {
            return None;
        }
        resolved.checked_add(recovery.source_tokens)
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
    recovered: &[sentence::RecoveredSentence],
    kind: ChangeKind,
    changes: &mut Vec<Change>,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<usize> {
    let mut resolved = 0usize;
    for recovery in recovered {
        if !valid_recovered_sentence(recovery) {
            return None;
        }
        resolved = resolved.checked_add(recovery.source_tokens)?;
        if !output_budget.charge(estimated_change_bytes(recovery.blocks.len())?) {
            return None;
        }
        let span = TextSpan {
            blocks: try_copy_slice(&recovery.blocks)?,
            separator: recovery.separator,
            canonical_range: recovery.canonical,
            comparable_range: recovery.comparable,
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
    Some(resolved)
}

fn prepare_recovered_replacements(
    replacements: &[sentence::RecoveredReplacement],
    changes: &mut Vec<Change>,
    output_budget: &mut RecoveryOutputBudget,
) -> Option<(usize, usize)> {
    let mut resolved_old = 0usize;
    let mut resolved_new = 0usize;
    for replacement in replacements {
        if !valid_recovered_sentence(&replacement.old)
            || !valid_recovered_sentence(&replacement.new)
        {
            return None;
        }
        resolved_old = resolved_old.checked_add(replacement.old.source_tokens)?;
        resolved_new = resolved_new.checked_add(replacement.new.source_tokens)?;
        let block_count = replacement
            .old
            .blocks
            .len()
            .checked_add(replacement.new.blocks.len())?;
        if !output_budget.charge(estimated_change_bytes(block_count)?) {
            return None;
        }
        changes.push(Change {
            kind: ChangeKind::Replacement,
            old_span: Some(recovered_text_span(&replacement.old)?),
            new_span: Some(recovered_text_span(&replacement.new)?),
            confidence: Confidence::High,
            tags: Vec::new(),
        });
    }
    Some((resolved_old, resolved_new))
}

fn sort_recovered_old_changes(side: &Side<'_>, changes: &mut [Change]) -> Option<()> {
    if changes
        .iter()
        .any(|change| recovered_old_change_key(side, change).is_none())
    {
        return None;
    }
    changes.sort_unstable_by_key(|change| recovered_old_change_key(side, change));
    Some(())
}

fn recovered_old_change_key(
    side: &Side<'_>,
    change: &Change,
) -> Option<(usize, usize, usize, bool)> {
    if !matches!(change.kind, ChangeKind::Deletion | ChangeKind::Replacement) {
        return None;
    }
    let span = change.old_span.as_ref()?;
    let block = span.blocks.first()?;
    Some((
        *side.index.get(block)?,
        span.comparable_range.start,
        span.comparable_range.end,
        change.kind == ChangeKind::Replacement,
    ))
}

fn valid_recovered_sentence(recovery: &sentence::RecoveredSentence) -> bool {
    (match recovery.blocks.len() {
        0 => false,
        1 if recovery.separator.is_some() => false,
        1 => true,
        _ => recovery.separator == Some(BlockSeparator::Space),
    }) && recovery.canonical.start < recovery.canonical.end
        && recovery.comparable.start < recovery.comparable.end
        && recovery.source_tokens != 0
}

fn recovered_text_span(recovery: &sentence::RecoveredSentence) -> Option<TextSpan> {
    Some(TextSpan {
        blocks: try_copy_slice(&recovery.blocks)?,
        separator: recovery.separator,
        canonical_range: recovery.canonical,
        comparable_range: recovery.comparable,
    })
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

fn estimated_change_bytes(block_count: usize) -> Option<usize> {
    std::mem::size_of::<Change>()
        .checked_add(block_count.checked_mul(std::mem::size_of::<BlockId>())?)?
        .checked_mul(2)
}

fn estimated_recovery_reservation_bytes(
    change_count: usize,
    unresolved_count: usize,
) -> Option<usize> {
    change_count
        .checked_mul(std::mem::size_of::<Change>())?
        .checked_add(unresolved_count.checked_mul(std::mem::size_of::<UnresolvedRegion>())?)?
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

    let Some(edits) = myers::diff(&old.tokens, &new.tokens, options.max_edit_distance)? else {
        push_unresolved_match(
            &old,
            &new,
            span,
            AlignmentEvidence::DiffEditDistanceExceeded,
            unresolved_regions,
        );
        return Ok(false);
    };
    if is_implausible_match(&edits, &old.tokens, &new.tokens, span.confidence, options) {
        push_unresolved_match(
            &old,
            &new,
            span,
            AlignmentEvidence::DiffRejectedAsImplausible,
            unresolved_regions,
        );
        return Ok(false);
    }

    append_changes(&old, &new, &edits, span.confidence.into(), changes);
    Ok(true)
}

fn push_unresolved_match(
    old: &GroupText,
    new: &GroupText,
    span: &AlignmentSpan,
    cause: AlignmentEvidence,
    unresolved_regions: &mut Vec<UnresolvedRegion>,
) {
    let mut evidence = span.evidence.clone();
    evidence.push(cause);
    unresolved_regions.push(UnresolvedRegion {
        old_span: Some(old.full_span()),
        new_span: Some(new.full_span()),
        evidence,
    });
}

/// Maximum allowable hunk-to-token ratio before degrading a non-exact match.
const MAX_WEAK_MATCH_HUNK_RATIO: f64 = 0.2;

/// Minimum span length in tokens required to evaluate hunk density.
const MIN_HUNK_DENSITY_TOKENS: usize = 8;

/// Returns true if a non-exact match has excessive changes or hunk fragmentation.
fn is_implausible_match(
    edits: &[Edit],
    old_tokens: &[ComparableToken],
    new_tokens: &[ComparableToken],
    confidence: AlignmentConfidence,
    options: DiffOptions,
) -> bool {
    let total = old_tokens.len().max(new_tokens.len());
    if total == 0 {
        return false;
    }
    let changed = edits.iter().filter(|edit| **edit != Edit::Equal).count();
    let confidence_factor = match (confidence, total < MIN_HUNK_DENSITY_TOKENS) {
        (AlignmentConfidence::Low, _) => 1.0,
        (AlignmentConfidence::Medium, true) => 3.0,
        (AlignmentConfidence::Medium, false) => 1.5,
        (AlignmentConfidence::High, true) => 4.0,
        (AlignmentConfidence::High, false) => 2.0,
    };
    let change_ratio_limit = (options.max_weak_match_change_ratio * confidence_factor).min(2.0);
    if changed as f64 / total as f64 > change_ratio_limit {
        return true;
    }
    if total < MIN_HUNK_DENSITY_TOKENS {
        return false;
    }
    let hunks = count_grouped_hunks(edits, old_tokens, new_tokens);
    let hunk_ratio_limit = match confidence {
        AlignmentConfidence::Low => MAX_WEAK_MATCH_HUNK_RATIO,
        AlignmentConfidence::Medium => 0.25,
        AlignmentConfidence::High => 0.3,
    };
    hunks as f64 / total as f64 > hunk_ratio_limit
}

fn count_grouped_hunks(
    edits: &[Edit],
    old_tokens: &[ComparableToken],
    new_tokens: &[ComparableToken],
) -> usize {
    let mut old_index = 0;
    let mut new_index = 0;
    let mut hunks = 0;
    let mut hunk_start = None;
    let mut equal_start = None;

    for (edit_index, edit) in edits.iter().enumerate() {
        match edit {
            Edit::Equal => {
                if hunk_start.is_some() {
                    equal_start.get_or_insert((old_index, new_index));
                }
                old_index += 1;
                new_index += 1;
            }
            Edit::Delete | Edit::Insert => {
                if let Some((old_equal_start, new_equal_start)) = equal_start.take() {
                    debug_assert_eq!(
                        old_tokens[old_equal_start..old_index],
                        new_tokens[new_equal_start..new_index]
                    );
                    let left_kind = hunk_start.and_then(|(old_start, new_start)| {
                        change_kind(old_start, new_start, old_equal_start, new_equal_start)
                    });
                    let right_kind = contiguous_change_kind(&edits[edit_index..]);
                    if !bridges_replacement_hunks(
                        &old_tokens[old_equal_start..old_index],
                        left_kind,
                        right_kind,
                    ) {
                        hunks += 1;
                        hunk_start = Some((old_index, new_index));
                    }
                } else if hunk_start.is_none() {
                    hunk_start = Some((old_index, new_index));
                }
                match edit {
                    Edit::Delete => old_index += 1,
                    Edit::Insert => new_index += 1,
                    Edit::Equal => unreachable!(),
                }
            }
        }
    }
    debug_assert_eq!(old_index, old_tokens.len());
    debug_assert_eq!(new_index, new_tokens.len());
    hunks + usize::from(hunk_start.is_some())
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
    let mut equal_start = None;

    for (edit_index, edit) in edits.iter().enumerate() {
        match edit {
            Edit::Equal => {
                if hunk_start.is_some() {
                    equal_start.get_or_insert((old_index, new_index));
                }
                old_index += 1;
                new_index += 1;
            }
            Edit::Delete | Edit::Insert => {
                if let Some((old_equal_start, new_equal_start)) = equal_start.take() {
                    let left_kind = hunk_start.and_then(|(old_start, new_start)| {
                        change_kind(old_start, new_start, old_equal_start, new_equal_start)
                    });
                    let right_kind = contiguous_change_kind(&edits[edit_index..]);
                    if !bridges_replacement_hunks(
                        &old.tokens[old_equal_start..old_index],
                        left_kind,
                        right_kind,
                    ) {
                        flush_hunk(
                            old,
                            new,
                            hunk_start.take(),
                            old_equal_start,
                            new_equal_start,
                            confidence,
                            changes,
                        );
                        hunk_start = Some((old_index, new_index));
                    }
                } else if hunk_start.is_none() {
                    hunk_start = Some((old_index, new_index));
                }
                match edit {
                    Edit::Delete => old_index += 1,
                    Edit::Insert => new_index += 1,
                    Edit::Equal => unreachable!(),
                }
            }
        }
    }
    let (old_end, new_end) = equal_start.unwrap_or((old_index, new_index));
    flush_hunk(old, new, hunk_start, old_end, new_end, confidence, changes);
}

fn contiguous_change_kind(edits: &[Edit]) -> Option<ChangeKind> {
    let mut old_changed = false;
    let mut new_changed = false;
    for edit in edits.iter().take_while(|edit| **edit != Edit::Equal) {
        match edit {
            Edit::Delete => old_changed = true,
            Edit::Insert => new_changed = true,
            Edit::Equal => unreachable!(),
        }
    }
    change_kind(0, 0, usize::from(old_changed), usize::from(new_changed))
}

fn bridges_replacement_hunks(
    tokens: &[ComparableToken],
    previous_kind: Option<ChangeKind>,
    next_kind: Option<ChangeKind>,
) -> bool {
    const MAX_SEPARATOR_TOKENS: usize = 3;

    if previous_kind != Some(ChangeKind::Replacement) || next_kind != Some(ChangeKind::Replacement)
    {
        return false;
    }
    match tokens {
        [ComparableToken::Scalar(_)] => true,
        [] => false,
        tokens if tokens.len() <= MAX_SEPARATOR_TOKENS => tokens.iter().all(|token| {
            matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace() || is_common_punctuation(*scalar))
        }),
        _ => false,
    }
}

fn is_common_punctuation(scalar: char) -> bool {
    scalar.is_ascii_punctuation()
        || matches!(
            scalar,
            '。' | '、'
                | '，'
                | '．'
                | '：'
                | '；'
                | '！'
                | '？'
                | '（'
                | '）'
                | '［'
                | '］'
                | '｛'
                | '｝'
                | '「'
                | '」'
                | '『'
                | '』'
                | '【'
                | '】'
                | '〈'
                | '〉'
                | '《'
                | '》'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '…'
                | '—'
                | '–'
        )
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
    let Some(kind) = change_kind(old_start, new_start, old_end, new_end) else {
        return;
    };
    let old_changed = old_start != old_end;
    let new_changed = new_start != new_end;
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

fn change_kind(
    old_start: usize,
    new_start: usize,
    old_end: usize,
    new_end: usize,
) -> Option<ChangeKind> {
    match (old_start != old_end, new_start != new_end) {
        (true, true) => Some(ChangeKind::Replacement),
        (true, false) => Some(ChangeKind::Deletion),
        (false, true) => Some(ChangeKind::Insertion),
        (false, false) => None,
    }
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

    fn scalar_tokens(count: usize) -> Vec<ComparableToken> {
        vec![ComparableToken::Scalar('a'); count]
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
        assert!(!is_implausible_match(
            &[],
            &[],
            &[],
            AlignmentConfidence::Low,
            options(0.5)
        ));
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
        let old = scalar_tokens(4);
        let new = scalar_tokens(4);
        assert!(!is_implausible_match(
            &edits,
            &old,
            &new,
            AlignmentConfidence::Low,
            options(0.5)
        ));
        // Just above the limit the changed-token ratio still gates short spans.
        let edits = [Edit::Delete, Edit::Insert, Edit::Delete, Edit::Insert];
        assert!(is_implausible_match(
            &edits,
            &scalar_tokens(2),
            &scalar_tokens(2),
            AlignmentConfidence::Low,
            options(0.5)
        ));
    }

    #[test]
    fn short_nonexact_matches_apply_confidence_scaled_ratio_limits() {
        let full_replacement = [
            Edit::Delete,
            Edit::Delete,
            Edit::Delete,
            Edit::Delete,
            Edit::Insert,
            Edit::Insert,
            Edit::Insert,
            Edit::Insert,
        ];

        assert!(is_implausible_match(
            &full_replacement,
            &scalar_tokens(4),
            &scalar_tokens(4),
            AlignmentConfidence::Low,
            options(0.5)
        ));
        assert!(is_implausible_match(
            &full_replacement,
            &scalar_tokens(4),
            &scalar_tokens(4),
            AlignmentConfidence::Medium,
            options(0.5)
        ));
        assert!(!is_implausible_match(
            &full_replacement,
            &scalar_tokens(4),
            &scalar_tokens(4),
            AlignmentConfidence::High,
            options(0.5)
        ));
        let medium_boundary = [
            Edit::Delete,
            Edit::Delete,
            Edit::Delete,
            Edit::Insert,
            Edit::Insert,
            Edit::Insert,
            Edit::Equal,
        ];
        assert!(!is_implausible_match(
            &medium_boundary,
            &scalar_tokens(4),
            &scalar_tokens(4),
            AlignmentConfidence::Medium,
            options(0.5)
        ));

        for confidence in [
            AlignmentConfidence::Low,
            AlignmentConfidence::Medium,
            AlignmentConfidence::High,
        ] {
            assert!(is_implausible_match(
                &[Edit::Delete, Edit::Insert],
                &scalar_tokens(1),
                &scalar_tokens(1),
                confidence,
                options(0.0),
            ));
        }
    }

    #[test]
    fn a_large_enough_span_with_dense_low_ratio_hunks_is_soup() {
        // Four islands separated by three-token equal runs: changed ratio
        // 8/17 stays under the limit while the hunk density exceeds it.
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
        assert!(is_implausible_match(
            &edits,
            &scalar_tokens(17),
            &scalar_tokens(17),
            AlignmentConfidence::Low,
            options(0.5)
        ));
    }

    #[test]
    fn short_equal_islands_do_not_inflate_hunk_density() {
        let mut edits = Vec::new();
        for _ in 0..4 {
            edits.extend([Edit::Delete, Edit::Insert, Edit::Equal]);
        }
        let old = scalar_tokens(8);
        let new = scalar_tokens(8);

        assert!(!is_implausible_match(
            &edits,
            &old,
            &new,
            AlignmentConfidence::Low,
            options(1.0)
        ));
    }

    #[test]
    fn hunk_density_is_skipped_below_the_minimum_sample_size() {
        // Two isolated deletions in a five-token span: under the ceiling on
        // changed tokens, so fragmentation must not degrade it.
        let edits = [
            Edit::Equal,
            Edit::Delete,
            Edit::Equal,
            Edit::Delete,
            Edit::Equal,
        ];
        assert!(!is_implausible_match(
            &edits,
            &scalar_tokens(5),
            &scalar_tokens(3),
            AlignmentConfidence::Low,
            options(0.5)
        ));
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
            vec![UnresolvedRegion {
                old_span: Some(test_span(1, short.chars().count(), sentence_start)),
                new_span: None,
                evidence: evidence.clone(),
            }]
        );
        assert_eq!(
            deletion.old_coverage.resolved_tokens,
            recovered_tokens + short.chars().count()
        );
        assert_eq!(deletion.new_coverage.resolved_tokens, short.chars().count());

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
            vec![UnresolvedRegion {
                old_span: None,
                new_span: Some(test_span(4, short.chars().count(), sentence_start)),
                evidence,
            }]
        );
        assert_eq!(
            insertion.old_coverage.resolved_tokens,
            short.chars().count()
        );
        assert_eq!(
            insertion.new_coverage.resolved_tokens,
            recovered_tokens + short.chars().count()
        );
    }

    #[test]
    fn exact_anchor_recovers_repeated_sentences_inside_paired_trusted_runs() {
        let anchor = "A unique anchor identifies this trusted run.";
        let repeated = "The repeated obligation remains unchanged.";
        let text = concat!(
            "A unique anchor identifies this trusted run. ",
            "The repeated obligation remains unchanged. ",
            "The repeated obligation remains unchanged."
        );
        let old = vec![sentence_block(1, text)];
        let new = vec![sentence_block(2, text)];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        assert!(result.changes.is_empty());
        let recovered_tokens = anchor.chars().count() + 2 * repeated.chars().count();
        assert_eq!(result.old_coverage.resolved_tokens, recovered_tokens);
        assert_eq!(result.new_coverage.resolved_tokens, recovered_tokens);
        assert_eq!(source_tokens(&old) - recovered_tokens, 2);
        assert_eq!(source_tokens(&new) - recovered_tokens, 2);
        assert_eq!(result.unresolved_regions.len(), 4);
    }

    #[test]
    fn exact_anchor_recovers_a_short_unit_inside_paired_trusted_runs() {
        let anchor = "A sufficiently long unique sentence identifies this trusted run.";
        let short = "Scope.";
        let text = format!("{anchor} {short}");
        let old = vec![sentence_block(1, &text)];
        let new = vec![sentence_block(2, &text)];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            16,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        let recovered_tokens = anchor.chars().count() + short.chars().count();
        assert!(result.changes.is_empty());
        assert_eq!(result.old_coverage.resolved_tokens, recovered_tokens);
        assert_eq!(result.new_coverage.resolved_tokens, recovered_tokens);
        assert_eq!(source_tokens(&old) - recovered_tokens, 1);
        assert_eq!(source_tokens(&new) - recovered_tokens, 1);
    }

    #[test]
    fn exact_anchors_scope_a_repeated_near_replacement_to_its_trusted_run() {
        let anchor_a = "A unique anchor identifies the first trusted run.";
        let anchor_b = "A different unique anchor identifies the second trusted run.";
        let old_clause = "The reviewed clause keeps every value except alpha.";
        let new_clause = "The reviewed clause keeps every value except beta.";
        let old = vec![
            sentence_block(1, anchor_a),
            sentence_block(2, old_clause),
            sentence_block(3, anchor_b),
            sentence_block(4, old_clause),
        ];
        let new = vec![
            sentence_block(101, anchor_a),
            sentence_block(102, new_clause),
            sentence_block(103, anchor_b),
            sentence_block(104, old_clause),
        ];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[
                Some(TrustedRunId(1)),
                Some(TrustedRunId(1)),
                Some(TrustedRunId(2)),
                Some(TrustedRunId(2)),
            ],
            &[
                Some(TrustedRunId(11)),
                Some(TrustedRunId(11)),
                Some(TrustedRunId(12)),
                Some(TrustedRunId(12)),
            ],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
        assert_eq!(
            result.changes[0].old_span,
            Some(test_span(2, 0, old_clause.chars().count()))
        );
        assert_eq!(
            result.changes[0].new_span,
            Some(test_span(102, 0, new_clause.chars().count()))
        );
    }

    #[test]
    fn crossing_near_replacements_inside_a_trusted_run_remain_unresolved() {
        let anchor = "A unique anchor identifies this trusted run.";
        let old_alpha = "The alpha requirement preserves all values except old.";
        let new_alpha = "The alpha requirement preserves all values except new.";
        let old_beta = "The beta requirement preserves all values except old.";
        let new_beta = "The beta requirement preserves all values except new.";
        let old = vec![
            sentence_block(1, anchor),
            sentence_block(2, old_alpha),
            sentence_block(3, old_beta),
        ];
        let new = vec![
            sentence_block(101, anchor),
            sentence_block(102, new_beta),
            sentence_block(103, new_alpha),
        ];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[
                Some(TrustedRunId(1)),
                Some(TrustedRunId(1)),
                Some(TrustedRunId(1)),
            ],
            &[
                Some(TrustedRunId(2)),
                Some(TrustedRunId(2)),
                Some(TrustedRunId(2)),
            ],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        assert!(result.changes.is_empty(), "changes={:?}", result.changes);
        assert_eq!(result.old_coverage.resolved_tokens, anchor.chars().count());
        assert_eq!(result.new_coverage.resolved_tokens, anchor.chars().count());
        assert!(!result.unresolved_regions.is_empty());
    }

    #[test]
    fn near_replacement_cannot_cross_recovered_exact_units() {
        let anchor = "A unique anchor identifies this trusted run.";
        let repeated = "The repeated obligation remains unchanged.";
        let old_clause = "The reviewed clause keeps every value except old.";
        let new_clause = "The reviewed clause keeps every value except new.";
        let old = vec![
            sentence_block(1, anchor),
            sentence_block(2, repeated),
            sentence_block(3, repeated),
            sentence_block(4, old_clause),
        ];
        let new = vec![
            sentence_block(101, anchor),
            sentence_block(102, new_clause),
            sentence_block(103, repeated),
            sentence_block(104, repeated),
        ];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1)); 4],
            &[Some(TrustedRunId(2)); 4],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        let exact_tokens = anchor.chars().count() + 2 * repeated.chars().count();
        assert!(result.changes.is_empty());
        assert_eq!(result.old_coverage.resolved_tokens, exact_tokens);
        assert_eq!(result.new_coverage.resolved_tokens, exact_tokens);
    }

    #[test]
    fn recovered_exact_unit_remains_near_veto_evidence_inside_a_paired_run() {
        let anchor = "A unique anchor identifies this trusted run.";
        let exact = "The reviewed clause keeps every value except alpha.";
        let old_clause = "The reviewed clause keeps every value except beta.";
        let new_clause = "The reviewed clause keeps every value except gamma.";
        let old = vec![
            sentence_block(1, anchor),
            sentence_block(2, exact),
            sentence_block(3, old_clause),
        ];
        let new = vec![
            sentence_block(101, anchor),
            sentence_block(102, exact),
            sentence_block(103, new_clause),
        ];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1)); 3],
            &[Some(TrustedRunId(2)); 3],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        let exact_tokens = anchor.chars().count() + exact.chars().count();
        assert!(result.changes.is_empty());
        assert_eq!(result.old_coverage.resolved_tokens, exact_tokens);
        assert_eq!(result.new_coverage.resolved_tokens, exact_tokens);
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
        let first_range = sentence::LocalSentenceRange {
            block: BlockId(3),
            canonical: ScalarRange {
                start: first_recovered.start,
                end: first_recovered.end,
            },
            comparable: first_recovered,
        };
        let second_range = sentence::LocalSentenceRange {
            block: BlockId(3),
            canonical: ScalarRange {
                start: second_recovered.start,
                end: second_recovered.end,
            },
            comparable: second_recovered,
        };
        let recovery = sentence::SentenceRecoveryPlan {
            deletions: vec![
                test_recovered_sentence(first_range, 0),
                test_recovered_sentence(second_range, 0),
            ],
            deletion_consumed: vec![first_range, second_range],
            ..sentence::SentenceRecoveryPlan::default()
        };
        let mut changes = Vec::new();
        let mut unresolved_regions = Vec::new();
        let mut resolved_old = 0;
        let mut resolved_new = 0;
        let mut output_budget = RecoveryOutputBudget::default();
        assert!(
            append_sentence_recovery(
                &old_side,
                &new_side,
                0,
                &alignment.spans[0],
                &recovery,
                &mut changes,
                &mut unresolved_regions,
                &mut resolved_old,
                &mut resolved_new,
                &mut output_budget,
            )
            .is_some()
        );

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
    fn duplicate_sentences_on_both_sides_remain_unresolved() {
        let old = vec![sentence_block(1, "Repeat sentence. Repeat sentence.")];
        let new = vec![sentence_block(2, "Repeat sentence. Repeat sentence.")];
        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(result.changes.is_empty());
        assert_eq!(result.unresolved_regions.len(), 1);
        assert_eq!(result.old_coverage.resolved_tokens, 0);
        assert_eq!(result.new_coverage.resolved_tokens, 0);
    }

    #[test]
    fn unique_exact_sentence_recovers_without_a_change() {
        let old = vec![sentence_block(2, "Same sentence.")];
        let new = vec![sentence_block(3, "Same sentence.")];
        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[Some(TrustedRunId(2))],
            &[Some(TrustedRunId(3))],
            5,
        );
        let comparison = outcome.comparison;
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("exact recovery diagnostics complete");

        assert!(comparison.changes.is_empty());
        assert!(comparison.unresolved_regions.is_empty());
        assert_eq!(comparison.old_coverage.resolved_tokens, source_tokens(&old));
        assert_eq!(comparison.new_coverage.resolved_tokens, source_tokens(&new));
        assert_eq!(comparison.old_coverage.ratio, Some(1.0));
        assert_eq!(comparison.new_coverage.ratio, Some(1.0));
        assert_eq!(metrics.exact_shared_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(
            metrics.recovered_exact_match_old_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.recovered_exact_match_new_tokens,
            source_tokens(&new)
        );
        assert_eq!(metrics.unresolved_remainder_old_source_tokens, 0);
        assert_eq!(metrics.unresolved_remainder_new_source_tokens, 0);
    }

    #[test]
    fn unique_modified_sentences_recover_as_replacements_symmetrically() {
        let before = vec![sentence_block(
            10,
            "The reviewed clause keeps every value except alpha.",
        )];
        let after = vec![sentence_block(
            11,
            "The reviewed clause keeps every value except beta.",
        )];

        for (old, new) in [(&before, &after), (&after, &before)] {
            let result = compare_sentence_recovery(
                old,
                new,
                &[Some(TrustedRunId(1))],
                &[Some(TrustedRunId(2))],
                5,
                vec![AlignmentEvidence::ReadingOrderUnknown],
            );
            assert_eq!(result.changes.len(), 1);
            assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
            assert_eq!(
                result.changes[0].old_span,
                Some(test_span(
                    old[0].block.0,
                    0,
                    old[0].canonical.text.chars().count(),
                ))
            );
            assert_eq!(
                result.changes[0].new_span,
                Some(test_span(
                    new[0].block.0,
                    0,
                    new[0].canonical.text.chars().count(),
                ))
            );
            assert!(result.unresolved_regions.is_empty());
            assert_eq!(
                result.old_coverage.resolved_tokens,
                old[0].matching_tokens.len()
            );
            assert_eq!(
                result.new_coverage.resolved_tokens,
                new[0].matching_tokens.len()
            );
        }
    }

    #[test]
    fn replacement_remainders_do_not_overlap_and_coverage_counts_only_sentences() {
        let prefix = "Leading context. ";
        let old_sentence = "The reviewed clause keeps every value except alpha.";
        let new_sentence = "The reviewed clause keeps every value except beta.";
        let suffix = " Trailing context.";
        let old_text = format!("{prefix}{old_sentence}{suffix}");
        let new_text = format!("{prefix}{new_sentence}{suffix}");
        let old = vec![sentence_block(12, &old_text)];
        let new = vec![sentence_block(13, &new_text)];
        let evidence = vec![AlignmentEvidence::ReadingOrderUnknown];

        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
            evidence.clone(),
        );

        let old_start = prefix.chars().count();
        let old_end = old_start + old_sentence.chars().count();
        let new_start = prefix.chars().count();
        let new_end = new_start + new_sentence.chars().count();
        assert_eq!(
            result.changes,
            vec![Change {
                kind: ChangeKind::Replacement,
                old_span: Some(test_span(12, old_start, old_end)),
                new_span: Some(test_span(13, new_start, new_end)),
                confidence: Confidence::High,
                tags: Vec::new(),
            }]
        );
        assert_eq!(
            result.unresolved_regions,
            vec![
                UnresolvedRegion {
                    old_span: Some(test_span(12, old_start - 1, old_start)),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: Some(test_span(12, old_end, old_end + 1)),
                    new_span: None,
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: None,
                    new_span: Some(test_span(13, new_start - 1, new_start)),
                    evidence: evidence.clone(),
                },
                UnresolvedRegion {
                    old_span: None,
                    new_span: Some(test_span(13, new_end, new_end + 1)),
                    evidence,
                },
            ]
        );
        assert_eq!(
            result.old_coverage.resolved_tokens,
            old_text.chars().count() - 2
        );
        assert_eq!(
            result.new_coverage.resolved_tokens,
            new_text.chars().count() - 2
        );
    }

    #[test]
    fn mixed_deletion_and_replacement_follow_old_source_order() {
        let deleted = "An obsolete standalone sentence disappears.";
        let replacement_old = "The reviewed clause keeps every value except alpha.";
        let replacement_new = "The reviewed clause keeps every value except beta.";

        for deletion_first in [true, false] {
            let old_text = if deletion_first {
                format!("{deleted} {replacement_old}")
            } else {
                format!("{replacement_old} {deleted}")
            };
            let old = vec![sentence_block(14, &old_text)];
            let new = vec![sentence_block(15, replacement_new)];
            let result = compare_sentence_recovery(
                &old,
                &new,
                &[Some(TrustedRunId(1))],
                &[Some(TrustedRunId(2))],
                5,
                vec![AlignmentEvidence::ReadingOrderUnknown],
            );

            let expected_kinds = if deletion_first {
                [ChangeKind::Deletion, ChangeKind::Replacement]
            } else {
                [ChangeKind::Replacement, ChangeKind::Deletion]
            };
            assert_eq!(
                result
                    .changes
                    .iter()
                    .map(|change| change.kind)
                    .collect::<Vec<_>>(),
                expected_kinds
            );
            let old_starts = result
                .changes
                .iter()
                .map(|change| {
                    change
                        .old_span
                        .as_ref()
                        .expect("old-side recovery has an old span")
                        .comparable_range
                        .start
                })
                .collect::<Vec<_>>();
            assert!(old_starts.windows(2).all(|pair| pair[0] < pair[1]));
            let replacement = result
                .changes
                .iter()
                .find(|change| change.kind == ChangeKind::Replacement)
                .expect("unique near pair becomes a replacement");
            assert_eq!(
                replacement.new_span,
                Some(test_span(15, 0, replacement_new.chars().count()))
            );
            assert_eq!(
                result.old_coverage.resolved_tokens,
                deleted.chars().count() + replacement_old.chars().count()
            );
            assert_eq!(
                result.new_coverage.resolved_tokens,
                replacement_new.chars().count()
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
    fn cross_block_unique_near_sentences_recover_as_replacements_symmetrically() {
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
        let split_end = split
            .iter()
            .map(|block| block.canonical.text.chars().count())
            .sum::<usize>()
            + 1;
        let clean_span = test_span(16, 0, clean[0].canonical.text.chars().count());
        let split_span = TextSpan {
            blocks: vec![BlockId(17), BlockId(18)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange {
                start: 0,
                end: split_end,
            },
            comparable_range: TokenRange {
                start: 0,
                end: split_end,
            },
        };

        for reverse in [false, true] {
            let (old, new, old_runs, new_runs, expected_old, expected_new) = if reverse {
                (
                    split.as_slice(),
                    clean.as_slice(),
                    split_run.as_slice(),
                    clean_run.as_slice(),
                    split_span.clone(),
                    clean_span.clone(),
                )
            } else {
                (
                    clean.as_slice(),
                    split.as_slice(),
                    clean_run.as_slice(),
                    split_run.as_slice(),
                    clean_span.clone(),
                    split_span.clone(),
                )
            };
            let result = compare_sentence_recovery(
                old,
                new,
                old_runs,
                new_runs,
                5,
                vec![AlignmentEvidence::ReadingOrderUnknown],
            );

            assert_eq!(result.changes.len(), 1);
            assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
            assert_eq!(result.changes[0].old_span, Some(expected_old));
            assert_eq!(result.changes[0].new_span, Some(expected_new));
            assert!(result.unresolved_regions.is_empty());
            assert_eq!(
                result.old_coverage.resolved_tokens,
                old.iter()
                    .map(|block| block.matching_tokens.len())
                    .sum::<usize>()
            );
            assert_eq!(
                result.new_coverage.resolved_tokens,
                new.iter()
                    .map(|block| block.matching_tokens.len())
                    .sum::<usize>()
            );
        }
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

        let repeated = vec![sentence_block(
            22,
            concat!(
                "The reviewed clause keeps every value except beta. ",
                "The reviewed clause keeps every value except beta."
            ),
        )];
        for (old, new) in [(&one, &repeated), (&repeated, &one)] {
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
    fn reciprocal_modified_sentences_cross_unresolved_spans() {
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

        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
        assert!(result.unresolved_regions.is_empty());
        assert_eq!(result.old_coverage.ratio, Some(1.0));
        assert_eq!(result.new_coverage.ratio, Some(1.0));
    }

    #[test]
    fn word_similarity_recovers_cross_span_replacement_with_internal_edits() {
        let old = vec![sentence_block(
            34,
            "In general, a bank's operational risk exposure is increased when a bank engages in new activities or develops new products; enters unfamiliar markets; implements new business processes or technology systems; and engages in businesses that are geographically distant from the head office.",
        )];
        let new = vec![sentence_block(
            35,
            "In general, a bank's operational risk exposure evolves when a bank initiates change, such as engaging in new activities or developing new products or services; entering unfamiliar markets or jurisdictions; implementing new or modifying business processes or technology systems; and engaging in businesses that are geographically distant from the head office.",
        )];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(34)], Vec::new()),
                reading_order_unknown_span(Vec::new(), vec![BlockId(35)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };

        let result = compare_sentence_recovery_with_intervals(
            &old,
            &new,
            &alignment,
            &trusted_run_intervals(&[Some(TrustedRunId(34))]),
            &trusted_run_intervals(&[Some(TrustedRunId(35))]),
            5,
            DiffOptions::default(),
        );

        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
        assert!(result.unresolved_regions.is_empty());
    }

    #[test]
    fn cross_span_replacement_requires_a_clear_score_margin() {
        let old = vec![sentence_block(
            36,
            "The reviewed clause keeps every value except alpha.",
        )];
        let new = vec![
            sentence_block(37, "The reviewed clause keeps every value except beta."),
            sentence_block(38, "The reviewed clause keeps every value except gamma."),
        ];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(36)], Vec::new()),
                reading_order_unknown_span(Vec::new(), vec![BlockId(37)]),
                reading_order_unknown_span(Vec::new(), vec![BlockId(38)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };

        let result = compare_sentence_recovery_with_intervals(
            &old,
            &new,
            &alignment,
            &trusted_run_intervals(&[Some(TrustedRunId(36))]),
            &trusted_run_intervals(&[Some(TrustedRunId(37)), Some(TrustedRunId(38))]),
            5,
            DiffOptions::default(),
        );

        assert!(result.changes.is_empty());
        assert_eq!(result.unresolved_regions.len(), 3);
        assert_eq!(result.old_coverage.resolved_tokens, 0);
        assert_eq!(result.new_coverage.resolved_tokens, 0);
    }

    #[test]
    fn globally_unique_exact_sentences_in_different_spans_are_resolved() {
        let old = vec![sentence_block(32, "Same sentence belongs to the old span.")];
        let new = vec![sentence_block(33, "Same sentence belongs to the old span.")];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(32)], Vec::new()),
                reading_order_unknown_span(Vec::new(), vec![BlockId(33)]),
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

        assert!(result.changes.is_empty());
        assert!(result.unresolved_regions.is_empty());
        assert_eq!(result.old_coverage.ratio, Some(1.0));
        assert_eq!(result.new_coverage.ratio, Some(1.0));
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
    fn recovery_budget_exhaustion_preserves_committed_exact_matches() {
        let shared = "Shared exact sentence.";
        let old_text = std::iter::once(shared.to_owned())
            .chain((0..40).map(|index| format!("A{index:02}.")))
            .collect::<Vec<_>>()
            .join(" ");
        let new_text = std::iter::once(shared.to_owned())
            .chain((0..40).map(|index| format!("B{index:02}.")))
            .collect::<Vec<_>>()
            .join(" ");
        let old = vec![sentence_block(52, &old_text)];
        let new = vec![sentence_block(53, &new_text)];
        let exact_tokens = shared.chars().count();

        for (old, new, old_run, new_run) in [
            (&old, &new, TrustedRunId(1), TrustedRunId(2)),
            (&new, &old, TrustedRunId(2), TrustedRunId(1)),
        ] {
            let outcome = compare_sentence_recovery_with_metrics(
                old,
                new,
                &[Some(old_run)],
                &[Some(new_run)],
                1,
            );
            let comparison = outcome.comparison;
            let metrics = outcome
                .sentence_recovery_metrics
                .expect("exact-only recovery diagnostics complete");

            assert!(comparison.changes.is_empty());
            assert!(!comparison.unresolved_regions.is_empty());
            assert_eq!(comparison.old_coverage.resolved_tokens, exact_tokens);
            assert_eq!(comparison.new_coverage.resolved_tokens, exact_tokens);
            assert_eq!(metrics.exact_shared_units, 1);
            assert!(!metrics.near_relation_complete);
            assert_eq!(metrics.recovered_exact_match_old_tokens, exact_tokens);
            assert_eq!(metrics.recovered_exact_match_new_tokens, exact_tokens);
            assert_eq!(
                metrics.unresolved_remainder_old_source_tokens,
                source_tokens(old) - exact_tokens
            );
            assert_eq!(
                metrics.unresolved_remainder_new_source_tokens,
                source_tokens(new) - exact_tokens
            );
            assert_eq!(metrics.recovered_replacement_old_tokens, 0);
            assert_eq!(metrics.recovered_replacement_new_tokens, 0);
            assert_eq!(metrics.recovered_deletion_tokens, 0);
            assert_eq!(metrics.recovered_insertion_tokens, 0);
        }
    }

    #[test]
    fn unique_exact_sentences_cross_uncertain_span_boundaries_without_becoming_moves() {
        let old = vec![
            sentence_block(60, "Alpha remains exactly stable."),
            sentence_block(61, "Beta remains exactly stable."),
        ];
        let new = vec![
            sentence_block(70, "Beta remains exactly stable."),
            sentence_block(71, "Alpha remains exactly stable."),
        ];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(60)], vec![BlockId(70)]),
                reading_order_unknown_span(vec![BlockId(61)], vec![BlockId(71)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let old_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(60)), Some(TrustedRunId(61))]);
        let new_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(70)), Some(TrustedRunId(71))]);

        let outcome = compare_aligned_with_sentence_recovery_metrics(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &old_intervals,
                new_trusted_run_intervals: &new_intervals,
                min_tokens: 5,
            },
        )
        .expect("cross-span exact recovery succeeds");
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("cross-span recovery diagnostics are complete");

        assert!(outcome.comparison.changes.is_empty());
        assert!(outcome.comparison.unresolved_regions.is_empty());
        assert_eq!(outcome.comparison.old_coverage.ratio, Some(1.0));
        assert_eq!(outcome.comparison.new_coverage.ratio, Some(1.0));
        assert_eq!(metrics.exact_shared_units, 2);
        assert_eq!(
            metrics.recovered_exact_match_old_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.recovered_exact_match_new_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn cross_span_exact_remainders_stay_sorted_when_near_relation_budget_exhausts() {
        let alpha = "Alpha remains exactly stable.";
        let beta = "Beta remains exactly stable.";
        let old_noise = (0..40)
            .map(|index| format!("A{index:02}."))
            .collect::<Vec<_>>()
            .join(" ");
        let new_noise = (0..40)
            .map(|index| format!("B{index:02}."))
            .collect::<Vec<_>>()
            .join(" ");
        let old = vec![
            sentence_block(80, &format!("{alpha} {old_noise}")),
            sentence_block(81, beta),
        ];
        let new = vec![
            sentence_block(90, &format!("{beta} {new_noise}")),
            sentence_block(91, alpha),
        ];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(80)], vec![BlockId(90)]),
                reading_order_unknown_span(vec![BlockId(81)], vec![BlockId(91)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let old_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(80)), Some(TrustedRunId(81))]);
        let new_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(90)), Some(TrustedRunId(91))]);

        let outcome = compare_aligned_with_sentence_recovery_metrics(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &old_intervals,
                new_trusted_run_intervals: &new_intervals,
                min_tokens: 1,
            },
        )
        .expect("exact-only fallback comparison succeeds");
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("exact-only fallback diagnostics are retained");

        assert!(!metrics.near_relation_complete);
        assert_eq!(metrics.exact_shared_units, 2);
        assert_eq!(
            metrics.recovered_exact_match_old_tokens,
            alpha.chars().count() + beta.chars().count()
        );
        assert_eq!(
            metrics.recovered_exact_match_new_tokens,
            alpha.chars().count() + beta.chars().count()
        );
        assert!(outcome.comparison.unresolved_regions.iter().all(|region| {
            region
                .old_span
                .as_ref()
                .is_none_or(|span| !span.blocks.contains(&BlockId(81)))
                && region
                    .new_span
                    .as_ref()
                    .is_none_or(|span| !span.blocks.contains(&BlockId(91)))
        }));
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
                recovery.deletions.push(test_recovered_sentence(range, 0));
                recovery.deletion_consumed.push(range);
            } else {
                recovery.insertions.push(test_recovered_sentence(range, 0));
                recovery.insertion_consumed.push(range);
            }

            let mut changes = Vec::new();
            let mut unresolved_regions = Vec::new();
            let mut resolved_old = 7;
            let mut resolved_new = 11;
            let mut output_budget = RecoveryOutputBudget::with_limits(RecoveryOutputLimits {
                max_items: 1,
                max_bytes: usize::MAX,
            });
            let committed = apply_sentence_recovery_or_fallback(
                &old_side,
                &new_side,
                0,
                &alignment.spans[0],
                &recovery,
                &mut changes,
                &mut unresolved_regions,
                &mut resolved_old,
                &mut resolved_new,
                &mut output_budget,
            );
            assert!(committed.is_none());
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
    fn span_output_fallback_keeps_recovered_metrics_zero_and_remainder_full() {
        let old = vec![sentence_block(54, "Unique old sentence.")];
        let new = vec![sentence_block(55, "Unique new sentence.")];
        let alignment =
            unresolved_alignment(&old, &new, vec![AlignmentEvidence::ReadingOrderUnknown]);
        let old_intervals = trusted_run_intervals(&[Some(TrustedRunId(1))]);
        let new_intervals = trusted_run_intervals(&[Some(TrustedRunId(2))]);

        let outcome = compare_aligned_inner(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            Some(SentenceRecoveryInput {
                old_trusted_run_intervals: &old_intervals,
                new_trusted_run_intervals: &new_intervals,
                min_tokens: 1,
            }),
            RecoveryOutputLimits {
                max_items: 1,
                max_bytes: usize::MAX,
            },
        )
        .expect("fallback comparison succeeds");
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("plan diagnostics completed before output fallback");

        assert!(outcome.comparison.changes.is_empty());
        assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
        assert_eq!(metrics.recovered_replacement_old_tokens, 0);
        assert_eq!(metrics.recovered_replacement_new_tokens, 0);
        assert_eq!(metrics.recovered_deletion_tokens, 0);
        assert_eq!(metrics.recovered_insertion_tokens, 0);
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.unresolved_remainder_new_source_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn exact_match_output_fallback_is_atomic_and_does_not_commit_match_metrics() {
        let old = vec![sentence_block(58, "Same exact sentence remains stable.")];
        let new = vec![sentence_block(59, "Same exact sentence remains stable.")];
        let alignment =
            unresolved_alignment(&old, &new, vec![AlignmentEvidence::ReadingOrderUnknown]);
        let old_intervals = trusted_run_intervals(&[Some(TrustedRunId(1))]);
        let new_intervals = trusted_run_intervals(&[Some(TrustedRunId(2))]);

        let outcome = compare_aligned_inner(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            Some(SentenceRecoveryInput {
                old_trusted_run_intervals: &old_intervals,
                new_trusted_run_intervals: &new_intervals,
                min_tokens: 5,
            }),
            RecoveryOutputLimits {
                max_items: 1,
                max_bytes: usize::MAX,
            },
        )
        .expect("exact fallback comparison succeeds");
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("plan diagnostics completed before output fallback");

        assert!(outcome.comparison.changes.is_empty());
        assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
        assert_eq!(outcome.comparison.old_coverage.resolved_tokens, 0);
        assert_eq!(outcome.comparison.new_coverage.resolved_tokens, 0);
        assert_eq!(metrics.exact_shared_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(metrics.recovered_exact_match_old_tokens, 0);
        assert_eq!(metrics.recovered_exact_match_new_tokens, 0);
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.unresolved_remainder_new_source_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn cross_span_exact_match_output_fallback_is_atomic() {
        let old = vec![
            sentence_block(60, "Alpha remains exactly stable. Old."),
            sentence_block(61, "Beta remains exactly stable. Old."),
        ];
        let new = vec![
            sentence_block(70, "Beta remains exactly stable. New."),
            sentence_block(71, "Alpha remains exactly stable. New."),
        ];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(60)], vec![BlockId(70)]),
                reading_order_unknown_span(vec![BlockId(61)], vec![BlockId(71)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let old_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(60)), Some(TrustedRunId(61))]);
        let new_intervals =
            trusted_run_intervals(&[Some(TrustedRunId(70)), Some(TrustedRunId(71))]);

        let outcome = compare_aligned_inner(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            Some(SentenceRecoveryInput {
                old_trusted_run_intervals: &old_intervals,
                new_trusted_run_intervals: &new_intervals,
                min_tokens: 5,
            }),
            RecoveryOutputLimits {
                max_items: 8,
                max_bytes: usize::MAX,
            },
        )
        .expect("cross-span fallback comparison succeeds");
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("cross-span plan diagnostics remain available");

        assert!(outcome.comparison.changes.is_empty());
        assert_eq!(outcome.comparison.unresolved_regions.len(), 2);
        assert_eq!(outcome.comparison.old_coverage.resolved_tokens, 0);
        assert_eq!(outcome.comparison.new_coverage.resolved_tokens, 0);
        assert_eq!(metrics.exact_shared_units, 2);
        assert_eq!(metrics.recovered_exact_match_old_tokens, 0);
        assert_eq!(metrics.recovered_exact_match_new_tokens, 0);
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.unresolved_remainder_new_source_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn cross_span_replacement_output_fallback_is_atomic() {
        let old = vec![sentence_block(
            72,
            "The reviewed clause keeps every value except alpha.",
        )];
        let new = vec![sentence_block(
            73,
            "The reviewed clause keeps every value except beta.",
        )];
        let alignment = Alignment {
            spans: vec![
                reading_order_unknown_span(vec![BlockId(72)], Vec::new()),
                reading_order_unknown_span(Vec::new(), vec![BlockId(73)]),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let old_intervals = trusted_run_intervals(&[Some(TrustedRunId(72))]);
        let new_intervals = trusted_run_intervals(&[Some(TrustedRunId(73))]);

        let outcome = compare_aligned_inner(
            &old,
            &new,
            &alignment,
            DiffOptions::default(),
            Some(SentenceRecoveryInput {
                old_trusted_run_intervals: &old_intervals,
                new_trusted_run_intervals: &new_intervals,
                min_tokens: 5,
            }),
            RecoveryOutputLimits {
                max_items: 1,
                max_bytes: usize::MAX,
            },
        )
        .expect("cross-span replacement fallback comparison succeeds");
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("cross-span replacement diagnostics remain available");

        assert!(outcome.comparison.changes.is_empty());
        assert_eq!(outcome.comparison.unresolved_regions.len(), 2);
        assert_eq!(outcome.comparison.old_coverage.resolved_tokens, 0);
        assert_eq!(outcome.comparison.new_coverage.resolved_tokens, 0);
        assert_eq!(metrics.recovered_replacement_old_tokens, 0);
        assert_eq!(metrics.recovered_replacement_new_tokens, 0);
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.unresolved_remainder_new_source_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn many_block_recovery_preflights_small_output_limit_and_falls_back_atomically() {
        const BLOCK_COUNT: usize = 64;

        let old = (0..BLOCK_COUNT)
            .map(|index| sentence_block(index as u64 + 1, "x"))
            .collect::<Vec<_>>();
        let alignment =
            unresolved_alignment(&old, &[], vec![AlignmentEvidence::ReadingOrderUnknown]);
        let (old_side, new_side) = inspect_sides_with_budget(&old, &[], DiffOptions::default())
            .expect("fixture sides are valid");
        let old_side = old_side.materialize().expect("old side materializes");
        let new_side = new_side.materialize().expect("new side materializes");
        let ranges = old
            .iter()
            .map(|block| sentence::LocalSentenceRange {
                block: block.block,
                canonical: ScalarRange { start: 0, end: 1 },
                comparable: TokenRange { start: 0, end: 1 },
            })
            .collect::<Vec<_>>();
        let recovery = sentence::SentenceRecoveryPlan {
            deletions: vec![sentence::RecoveredSentence {
                span_index: 0,
                blocks: old.iter().map(|block| block.block).collect(),
                separator: Some(BlockSeparator::Space),
                canonical: ScalarRange {
                    start: 0,
                    end: BLOCK_COUNT * 2 - 1,
                },
                comparable: TokenRange {
                    start: 0,
                    end: BLOCK_COUNT * 2 - 1,
                },
                source_tokens: BLOCK_COUNT,
            }],
            deletion_consumed: ranges,
            ..sentence::SentenceRecoveryPlan::default()
        };
        let mut changes = Vec::new();
        let mut unresolved_regions = Vec::new();
        let mut resolved_old = 7;
        let mut resolved_new = 11;
        let mut output_budget = RecoveryOutputBudget::with_limits(RecoveryOutputLimits {
            max_items: 8,
            max_bytes: usize::MAX,
        });

        let committed = apply_sentence_recovery_or_fallback(
            &old_side,
            &new_side,
            0,
            &alignment.spans[0],
            &recovery,
            &mut changes,
            &mut unresolved_regions,
            &mut resolved_old,
            &mut resolved_new,
            &mut output_budget,
        );
        assert!(committed.is_none());

        assert!(changes.is_empty());
        assert_eq!(unresolved_regions.len(), 1);
        assert_eq!(
            unresolved_regions[0]
                .old_span
                .as_ref()
                .map(|span| span.blocks.len()),
            Some(BLOCK_COUNT)
        );
        assert_eq!((resolved_old, resolved_new), (7, 11));
        assert_eq!((output_budget.items, output_budget.bytes), (0, 0));
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
    fn terminal_block_boundaries_keep_trusted_rows_separate() {
        for (first, second) in [
            (
                "Hash(M) The result of applying a hash function.",
                "k a per-message secret value.",
            ),
            ("ハッシュ関数です。", "k メッセージごとの秘密値です。"),
        ] {
            let source = vec![sentence_block(10, first), sentence_block(11, second)];
            let result = compare_sentence_recovery(
                &source,
                &[],
                &[Some(TrustedRunId(1)), Some(TrustedRunId(1))],
                &[],
                1,
                vec![AlignmentEvidence::ReadingOrderUnknown],
            );

            assert_eq!(result.changes.len(), 2);
            assert_eq!(result.changes[0].kind, ChangeKind::Deletion);
            assert_eq!(
                result.changes[0].old_span,
                Some(test_span(10, 0, first.chars().count()))
            );
            assert_eq!(result.changes[1].kind, ChangeKind::Deletion);
            assert_eq!(
                result.changes[1].old_span,
                Some(test_span(11, 0, second.chars().count()))
            );
            assert!(result.unresolved_regions.is_empty());
            assert_eq!(
                result.old_coverage.resolved_tokens,
                first.chars().count() + second.chars().count()
            );
        }
    }

    #[test]
    fn nonterminal_block_boundary_keeps_one_cross_block_sentence() {
        let first = "This trusted cross-block sentence has";
        let second = "no terminal punctuation until here.";
        let source = vec![sentence_block(12, first), sentence_block(13, second)];
        let result = compare_sentence_recovery(
            &source,
            &[],
            &[Some(TrustedRunId(1)), Some(TrustedRunId(1))],
            &[],
            1,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        assert_eq!(result.changes.len(), 1);
        let span = result.changes[0]
            .old_span
            .as_ref()
            .expect("joined old sentence has a span");
        assert_eq!(span.blocks, [BlockId(12), BlockId(13)]);
        assert_eq!(span.separator, Some(BlockSeparator::Space));
        assert_eq!(
            span.comparable_range,
            TokenRange {
                start: 0,
                end: first.chars().count() + 1 + second.chars().count(),
            }
        );
        assert_eq!(
            result.old_coverage.resolved_tokens,
            first.chars().count() + second.chars().count()
        );
    }

    #[test]
    fn multi_block_and_single_block_exact_sentences_recover_as_one_match() {
        let first = "This exact sentence spans";
        let second = "multiple trusted blocks.";
        let joined = format!("{first} {second}");
        let old = vec![sentence_block(14, first), sentence_block(15, second)];
        let new = vec![sentence_block(16, &joined)];

        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[Some(TrustedRunId(1)), Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
        );
        let comparison = outcome.comparison;
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("cross-block exact diagnostics complete");

        assert!(comparison.changes.is_empty());
        assert!(comparison.unresolved_regions.is_empty());
        assert_eq!(comparison.old_coverage.resolved_tokens, source_tokens(&old));
        assert_eq!(comparison.new_coverage.resolved_tokens, source_tokens(&new));
        assert_eq!(metrics.exact_shared_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(
            metrics.recovered_exact_match_old_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.recovered_exact_match_new_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn fips_like_terminal_rows_recover_only_the_near_replacement() {
        let definition = "Hash(M) The result of applying a hash function.";
        let old_parameter = "k A per-message secret number used in the signing process.";
        let new_parameter = "k A per-message secret value used in the signing process.";
        let old = vec![
            sentence_block(20, definition),
            sentence_block(21, old_parameter),
        ];
        let new = vec![
            sentence_block(22, definition),
            sentence_block(23, new_parameter),
        ];
        let result = compare_sentence_recovery(
            &old,
            &new,
            &[Some(TrustedRunId(1)), Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2)), Some(TrustedRunId(2))],
            5,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );

        assert_eq!(
            result.changes,
            vec![Change {
                kind: ChangeKind::Replacement,
                old_span: Some(test_span(21, 0, old_parameter.chars().count())),
                new_span: Some(test_span(23, 0, new_parameter.chars().count())),
                confidence: Confidence::High,
                tags: Vec::new(),
            }]
        );
        assert_eq!(
            result.old_coverage.resolved_tokens,
            definition.chars().count() + old_parameter.chars().count()
        );
        assert_eq!(
            result.new_coverage.resolved_tokens,
            definition.chars().count() + new_parameter.chars().count()
        );
        assert!(result.unresolved_regions.is_empty());
    }

    #[test]
    fn sentence_recovery_metrics_count_exact_near_and_committed_replacement_evidence() {
        let definition = "Hash(M) The result of applying a hash function.";
        let old_parameter = "k A per-message secret number used in the signing process.";
        let new_parameter = "k A per-message secret value used in the signing process.";
        let old = vec![
            sentence_block(20, definition),
            sentence_block(21, old_parameter),
        ];
        let new = vec![
            sentence_block(22, definition),
            sentence_block(23, new_parameter),
        ];

        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[Some(TrustedRunId(1)), Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2)), Some(TrustedRunId(2))],
            5,
        );
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("completed diagnostics remain present");

        assert_eq!(metrics.old_trusted_run_source_tokens, source_tokens(&old));
        assert_eq!(metrics.new_trusted_run_source_tokens, source_tokens(&new));
        assert_eq!(metrics.exact_shared_units, 1);
        assert_eq!(metrics.old_exact_one_sided_units, 1);
        assert_eq!(metrics.new_exact_one_sided_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(metrics.near_pair_candidates, 1);
        assert_eq!(metrics.vetoed_near_pairs, 0);
        assert_eq!(
            metrics.recovered_exact_match_old_tokens,
            definition.chars().count()
        );
        assert_eq!(
            metrics.recovered_exact_match_new_tokens,
            definition.chars().count()
        );
        assert_eq!(
            metrics.recovered_replacement_old_tokens,
            old_parameter.chars().count()
        );
        assert_eq!(
            metrics.recovered_replacement_new_tokens,
            new_parameter.chars().count()
        );
        assert_eq!(metrics.recovered_deletion_tokens, 0);
        assert_eq!(metrics.recovered_insertion_tokens, 0);
        assert_eq!(metrics.unresolved_remainder_old_source_tokens, 0);
        assert_eq!(metrics.unresolved_remainder_new_source_tokens, 0);
    }

    #[test]
    fn completed_empty_sentence_recovery_diagnostics_are_present_as_real_zeros() {
        let outcome = compare_aligned_with_sentence_recovery_metrics(
            &[],
            &[],
            &Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            },
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &[],
                new_trusted_run_intervals: &[],
                min_tokens: 1,
            },
        )
        .expect("empty comparison succeeds");

        assert_eq!(
            outcome.sentence_recovery_metrics,
            Some(SentenceRecoveryMetrics {
                near_relation_complete: true,
                ..SentenceRecoveryMetrics::default()
            })
        );
    }

    #[test]
    fn sentence_recovery_metrics_count_each_vetoed_near_pair_once() {
        let old = vec![sentence_block(
            30,
            "The reviewed clause keeps every value except alpha.",
        )];
        let new = vec![sentence_block(
            31,
            concat!(
                "The reviewed clause keeps every value except beta. ",
                "The reviewed clause keeps every value except gamma."
            ),
        )];

        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
        );
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("completed diagnostics remain present");

        assert_eq!(metrics.old_exact_one_sided_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(metrics.new_exact_one_sided_units, 2);
        assert_eq!(metrics.near_pair_candidates, 2);
        assert_eq!(metrics.vetoed_near_pairs, 2);
        assert_eq!(metrics.recovered_replacement_old_tokens, 0);
        assert_eq!(metrics.recovered_replacement_new_tokens, 0);
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            source_tokens(&old)
        );
        assert_eq!(
            metrics.unresolved_remainder_new_source_tokens,
            source_tokens(&new)
        );
    }

    #[test]
    fn recovered_exact_occurrence_remains_near_veto_evidence() {
        let exact = "The reviewed clause keeps every value except alpha.";
        let near_old_only = "The reviewed clause keeps every value except beta.";
        let old = vec![sentence_block(34, exact), sentence_block(35, near_old_only)];
        let new = vec![sentence_block(36, exact)];

        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[Some(TrustedRunId(1)), Some(TrustedRunId(2))],
            &[Some(TrustedRunId(3))],
            5,
        );
        let comparison = outcome.comparison;
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("exact and veto diagnostics complete");

        assert!(comparison.changes.is_empty());
        assert_eq!(comparison.unresolved_regions.len(), 1);
        assert_eq!(
            comparison.old_coverage.resolved_tokens,
            exact.chars().count()
        );
        assert_eq!(comparison.new_coverage.resolved_tokens, source_tokens(&new));
        assert_eq!(metrics.exact_shared_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(metrics.near_pair_candidates, 1);
        assert_eq!(metrics.vetoed_near_pairs, 1);
        assert_eq!(metrics.recovered_deletion_tokens, 0);
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            near_old_only.chars().count()
        );
        assert_eq!(metrics.unresolved_remainder_new_source_tokens, 0);
    }

    #[test]
    fn sentence_recovery_metrics_count_committed_deletion_and_insertion_tokens() {
        let old = vec![sentence_block(
            40,
            "Completely obsolete language appears here.",
        )];
        let new = vec![sentence_block(
            41,
            "A fresh and unrelated statement replaces it.",
        )];

        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[Some(TrustedRunId(1))],
            &[Some(TrustedRunId(2))],
            5,
        );
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("completed diagnostics remain present");

        assert!(metrics.near_relation_complete);
        assert_eq!(metrics.near_pair_candidates, 0);
        assert_eq!(metrics.vetoed_near_pairs, 0);
        assert_eq!(metrics.recovered_deletion_tokens, source_tokens(&old));
        assert_eq!(metrics.recovered_insertion_tokens, source_tokens(&new));
        assert_eq!(metrics.unresolved_remainder_old_source_tokens, 0);
        assert_eq!(metrics.unresolved_remainder_new_source_tokens, 0);
    }

    #[test]
    fn exact_replacement_deletion_and_insertion_coexist_with_exact_remainders() {
        let exact = "Stable exact sentence remains unchanged.";
        let replacement_old = "The reviewed clause keeps every value except alpha.";
        let replacement_new = "The reviewed clause keeps every value except beta.";
        let deleted = "Completely obsolete language appears here.";
        let inserted = "A fresh and unrelated statement appears here.";
        let short = "Tiny.";
        let old = vec![
            sentence_block(50, exact),
            sentence_block(51, replacement_old),
            sentence_block(52, deleted),
            sentence_block(53, short),
        ];
        let new = vec![
            sentence_block(54, exact),
            sentence_block(55, replacement_new),
            sentence_block(56, inserted),
            sentence_block(57, short),
        ];

        let outcome = compare_sentence_recovery_with_metrics(
            &old,
            &new,
            &[
                Some(TrustedRunId(1)),
                Some(TrustedRunId(2)),
                Some(TrustedRunId(3)),
                Some(TrustedRunId(4)),
            ],
            &[
                Some(TrustedRunId(5)),
                Some(TrustedRunId(6)),
                Some(TrustedRunId(7)),
                Some(TrustedRunId(8)),
            ],
            10,
        );
        let comparison = outcome.comparison;
        let metrics = outcome
            .sentence_recovery_metrics
            .expect("mixed recovery diagnostics complete");

        assert_eq!(
            comparison
                .changes
                .iter()
                .map(|change| change.kind)
                .collect::<Vec<_>>(),
            [
                ChangeKind::Replacement,
                ChangeKind::Deletion,
                ChangeKind::Insertion,
            ]
        );
        assert_eq!(comparison.unresolved_regions.len(), 2);
        assert_eq!(metrics.exact_shared_units, 1);
        assert!(metrics.near_relation_complete);
        assert_eq!(
            metrics.recovered_exact_match_old_tokens,
            exact.chars().count()
        );
        assert_eq!(
            metrics.recovered_exact_match_new_tokens,
            exact.chars().count()
        );
        assert_eq!(
            metrics.recovered_replacement_old_tokens,
            replacement_old.chars().count()
        );
        assert_eq!(
            metrics.recovered_replacement_new_tokens,
            replacement_new.chars().count()
        );
        assert_eq!(metrics.recovered_deletion_tokens, deleted.chars().count());
        assert_eq!(metrics.recovered_insertion_tokens, inserted.chars().count());
        assert_eq!(
            metrics.unresolved_remainder_old_source_tokens,
            short.chars().count()
        );
        assert_eq!(
            metrics.unresolved_remainder_new_source_tokens,
            short.chars().count()
        );
        assert_eq!(
            comparison.old_coverage.resolved_tokens,
            source_tokens(&old) - short.chars().count()
        );
        assert_eq!(
            comparison.new_coverage.resolved_tokens,
            source_tokens(&new) - short.chars().count()
        );
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
    fn cross_block_recovery_requires_one_unbroken_clean_trusted_run() {
        let first = "This guarded cross-block";
        let second = "sentence must remain unresolved.";
        let base = vec![sentence_block(30, first), sentence_block(31, second)];
        let mut issue = sentence_block(31, second);
        issue.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource { atoms: Vec::new() },
        });

        let cases = vec![
            (
                "distinct run",
                base.clone(),
                vec![trusted_interval(1, 0, 1), trusted_interval(2, 0, 1)],
            ),
            (
                "ordinal gap",
                base.clone(),
                vec![trusted_interval(3, 0, 1), trusted_interval(3, 2, 3)],
            ),
            (
                "untrusted barrier",
                vec![
                    sentence_block(30, "This guarded"),
                    sentence_block(32, "cross-block"),
                    sentence_block(31, "sentence remains unresolved."),
                ],
                vec![trusted_interval(4, 0, 1), None, trusted_interval(4, 1, 2)],
            ),
            (
                "normalization issue",
                vec![sentence_block(30, first), issue],
                trusted_run_intervals(&[Some(TrustedRunId(5)), Some(TrustedRunId(5))]),
            ),
            (
                "unmapped glyph",
                vec![
                    sentence_block(30, first),
                    sentence_block_with_unmapped(31, second),
                ],
                trusted_run_intervals(&[Some(TrustedRunId(6)), Some(TrustedRunId(6))]),
            ),
            (
                "untrusted block",
                base,
                vec![trusted_interval(7, 0, 1), None],
            ),
        ];

        for (case, source, intervals) in cases {
            for reverse in [false, true] {
                let (old, new, old_intervals, new_intervals) = if reverse {
                    (&[][..], source.as_slice(), &[][..], intervals.as_slice())
                } else {
                    (source.as_slice(), &[][..], intervals.as_slice(), &[][..])
                };
                let alignment =
                    unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
                let public = compare_aligned(old, new, &alignment, DiffOptions::default())
                    .expect("comparison without recovery succeeds");
                let recovered = compare_sentence_recovery_with_intervals(
                    old,
                    new,
                    &alignment,
                    old_intervals,
                    new_intervals,
                    35,
                    DiffOptions::default(),
                );
                assert_eq!(recovered, public, "case={case}, reverse={reverse}");
            }
        }
    }

    #[test]
    fn same_run_cross_block_sentence_emits_one_change_with_source_only_coverage_symmetrically() {
        let first_text = "Hi! This unique";
        let second_sentence = "cross-block sentence is removed.";
        let second_text = format!("{second_sentence} Ok!");
        let source = vec![
            sentence_block(2, &second_text),
            sentence_block(1, first_text),
        ];
        let intervals = [trusted_interval(1, 1, 2), trusted_interval(1, 0, 1)];
        let prefix_end = "Hi! ".chars().count();
        let second_sentence_end = second_sentence.chars().count();
        let source_tokens = first_text.chars().count() - prefix_end + second_sentence_end;
        let joined_end = first_text.chars().count() + 1 + second_sentence_end;

        for reverse in [false, true] {
            let (old, new, old_intervals, new_intervals) = if reverse {
                (&[][..], source.as_slice(), &[][..], intervals.as_slice())
            } else {
                (source.as_slice(), &[][..], intervals.as_slice(), &[][..])
            };
            let alignment =
                unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
            let result = compare_sentence_recovery_with_intervals(
                old,
                new,
                &alignment,
                old_intervals,
                new_intervals,
                20,
                DiffOptions::default(),
            );

            assert_eq!(result.changes.len(), 1);
            let change = &result.changes[0];
            assert_eq!(
                change.kind,
                if reverse {
                    ChangeKind::Insertion
                } else {
                    ChangeKind::Deletion
                }
            );
            let span = if reverse {
                change.new_span.as_ref()
            } else {
                change.old_span.as_ref()
            }
            .expect("the recovered side has one multi-block span");
            assert_eq!(span.blocks, [BlockId(1), BlockId(2)]);
            assert_eq!(span.separator, Some(BlockSeparator::Space));
            assert_eq!(
                span.canonical_range,
                ScalarRange {
                    start: prefix_end,
                    end: joined_end,
                }
            );
            assert_eq!(
                span.comparable_range,
                TokenRange {
                    start: prefix_end,
                    end: joined_end,
                }
            );
            assert_eq!(
                span.comparable_range.end - span.comparable_range.start,
                source_tokens + 1,
                "the joined span includes its synthetic separator"
            );

            let (resolved_source, resolved_empty) = if reverse {
                (
                    result.new_coverage.resolved_tokens,
                    result.old_coverage.resolved_tokens,
                )
            } else {
                (
                    result.old_coverage.resolved_tokens,
                    result.new_coverage.resolved_tokens,
                )
            };
            assert_eq!(resolved_source, source_tokens);
            assert_eq!(resolved_empty, 0);

            let mut remainder_spans = result
                .unresolved_regions
                .iter()
                .map(|region| {
                    let (span, absent) = if reverse {
                        (&region.new_span, &region.old_span)
                    } else {
                        (&region.old_span, &region.new_span)
                    };
                    assert!(absent.is_none());
                    span.as_ref()
                        .expect("each remainder stays on the source side")
                })
                .collect::<Vec<_>>();
            remainder_spans.sort_unstable_by_key(|span| span.blocks[0]);
            assert_eq!(remainder_spans.len(), 2);
            assert_eq!(remainder_spans[0], &test_span(1, 0, prefix_end));
            assert_eq!(
                remainder_spans[1],
                &test_span(2, second_sentence_end, second_text.chars().count())
            );
        }
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
            assert_eq!(result.unresolved_regions.len(), 1);
            assert_eq!(result.old_coverage.resolved_tokens, 0);
            assert_eq!(result.new_coverage.resolved_tokens, 0);
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
            assert_eq!(result.unresolved_regions.len(), 1);
            assert_eq!(result.old_coverage.resolved_tokens, 0);
            assert_eq!(result.new_coverage.resolved_tokens, 0);
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

        let exact_new = [sentence_block(2, "Short sentence.")];
        let exact_short = compare_sentence_recovery(
            &old,
            &exact_new,
            &run,
            &[Some(TrustedRunId(2))],
            "Short sentence.".chars().count() + 1,
            vec![AlignmentEvidence::ReadingOrderUnknown],
        );
        assert!(exact_short.changes.is_empty());
        assert_eq!(exact_short.unresolved_regions.len(), 1);
        assert_eq!(exact_short.old_coverage.resolved_tokens, 0);
        assert_eq!(exact_short.new_coverage.resolved_tokens, 0);

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
    fn sentence_recovery_resolves_an_exact_pair_left_unresolved_by_public_comparison() {
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
        assert_eq!(public.unresolved_regions.len(), 1);
        assert_eq!(public.old_coverage.resolved_tokens, 0);
        assert_eq!(public.new_coverage.resolved_tokens, 0);
        assert!(recovered.changes.is_empty());
        assert!(recovered.unresolved_regions.is_empty());
        assert_eq!(recovered.old_coverage.resolved_tokens, source_tokens(&old));
        assert_eq!(recovered.new_coverage.resolved_tokens, source_tokens(&new));
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

    fn compare_sentence_recovery_with_metrics(
        old: &[BlockText],
        new: &[BlockText],
        old_trusted_run_ids: &[Option<TrustedRunId>],
        new_trusted_run_ids: &[Option<TrustedRunId>],
        min_tokens: usize,
    ) -> ComparisonWithSentenceRecoveryMetrics {
        let alignment =
            unresolved_alignment(old, new, vec![AlignmentEvidence::ReadingOrderUnknown]);
        let old_trusted_run_intervals = trusted_run_intervals(old_trusted_run_ids);
        let new_trusted_run_intervals = trusted_run_intervals(new_trusted_run_ids);
        compare_aligned_with_sentence_recovery_metrics(
            old,
            new,
            &alignment,
            DiffOptions::default(),
            SentenceRecoveryInput {
                old_trusted_run_intervals: &old_trusted_run_intervals,
                new_trusted_run_intervals: &new_trusted_run_intervals,
                min_tokens,
            },
        )
        .expect("sentence recovery comparison succeeds")
    }

    fn source_tokens(blocks: &[BlockText]) -> usize {
        blocks.iter().map(|block| block.matching_tokens.len()).sum()
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

    fn trusted_interval(run_id: u64, start: usize, end: usize) -> Option<TrustedRunInterval> {
        Some(TrustedRunInterval {
            run_id: TrustedRunId(run_id),
            start,
            end,
        })
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

    fn test_recovered_sentence(
        range: sentence::LocalSentenceRange,
        span_index: usize,
    ) -> sentence::RecoveredSentence {
        sentence::RecoveredSentence {
            span_index,
            blocks: vec![range.block],
            separator: None,
            canonical: range.canonical,
            comparable: range.comparable,
            source_tokens: range.comparable.end - range.comparable.start,
        }
    }
}
