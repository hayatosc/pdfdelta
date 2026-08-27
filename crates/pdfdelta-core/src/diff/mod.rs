mod myers;

const MAX_MYERS_EDIT_DISTANCE: usize = 32_768;

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

use crate::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        BlockSeparator,
    },
    layout::BlockId,
    model::Vec2,
    normalize::{BlockText, ComparableToken, FontSizeSignature, PositionSignature, ScalarRange},
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
            "diff max_edit_distance must not exceed {MAX_MYERS_EDIT_DISTANCE} because Myers trace memory grows quadratically"
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

pub fn compare_aligned(
    old: &[BlockText],
    new: &[BlockText],
    alignment: &Alignment,
    options: DiffOptions,
) -> Result<Comparison> {
    let (old, new) = inspect_sides_with_budget(old, new, options)?;
    let old = old.materialize()?;
    let new = new.materialize()?;
    validate_alignment(&old, &new, alignment)?;
    let (moves_by_old, moves_by_new) = promotable_moves(&old, &new, alignment);

    let mut changes = Vec::new();
    let mut formatting_changes = Vec::new();
    let mut unresolved_regions = Vec::new();
    let mut resolved_old = 0;
    let mut resolved_new = 0;

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
            AlignmentKind::Unresolved => unresolved_regions.push(UnresolvedRegion {
                old_span: full_span(&old, &span.old, span.old_separator),
                new_span: full_span(&new, &span.new, span.new_separator),
                evidence: span.evidence.clone(),
            }),
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
    let Some((old_folded, old_changed)) = restricted_width_fold(old) else {
        return false;
    };
    let Some((new_folded, new_changed)) = restricted_width_fold(new) else {
        return false;
    };

    (old_changed || new_changed) && old_folded == new_folded
}

fn restricted_width_fold(tokens: &[ComparableToken]) -> Option<(String, bool)> {
    let mut folded = String::new();
    let mut changed = false;

    for token in tokens {
        let ComparableToken::Scalar(scalar) = token else {
            return None;
        };
        if *scalar == '\u{3000}' || ('\u{ff00}'..='\u{ffef}').contains(scalar) {
            let original = scalar.to_string();
            let normalized = original.as_str().nfkc().collect::<String>();
            if normalized != original {
                folded.push_str(&normalized);
                changed = true;
                continue;
            }
        }
        folded.push(*scalar);
    }

    Some((folded.nfc().collect(), changed))
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
                if message.contains("max_edit_distance") && message.contains("quadratically")
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
}
