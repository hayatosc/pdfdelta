mod myers;

use std::collections::HashMap;

use crate::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        BlockSeparator,
    },
    layout::BlockId,
    normalize::{BlockText, ComparableToken, ScalarRange},
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
    pub ratio: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    pub changes: Vec<Change>,
    pub formatting_changes: Vec<FormattingChange>,
    pub unresolved_regions: Vec<UnresolvedRegion>,
    pub old_coverage: Coverage,
    pub new_coverage: Coverage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    pub max_tokens: usize,
    pub max_edit_distance: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            max_tokens: 1_000_000,
            max_edit_distance: 2_048,
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

    let mut changes = Vec::new();
    let mut formatting_changes = Vec::new();
    let mut unresolved_regions = Vec::new();
    let mut resolved_old = 0;
    let mut resolved_new = 0;

    for span in &alignment.spans {
        match span.kind {
            AlignmentKind::Match => {
                resolved_old += old.source_token_count(&span.old);
                resolved_new += new.source_token_count(&span.new);
                compare_match(
                    &old,
                    &new,
                    span,
                    options.max_edit_distance,
                    &mut changes,
                    &mut formatting_changes,
                )?;
            }
            AlignmentKind::Deletion => {
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
    max_edit_distance: usize,
    changes: &mut Vec<Change>,
    formatting_changes: &mut Vec<FormattingChange>,
) -> Result<()> {
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
        if !reasons.is_empty() {
            formatting_changes.push(FormattingChange {
                old_span: old.full_span(),
                new_span: new.full_span(),
                confidence: span.confidence.into(),
                reasons,
            });
        }
        return Ok(());
    }

    let edits = myers::diff(&old.tokens, &new.tokens, max_edit_distance)?;
    append_changes(&old, &new, &edits, span.confidence.into(), changes);
    Ok(())
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
    changes.push(Change {
        kind,
        old_span: old_changed.then(|| old.span(old_start, old_end)),
        new_span: new_changed.then(|| new.span(new_start, new_end)),
        confidence,
        tags: Vec::new(),
    });
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
        ratio: if total_tokens == 0 {
            1.0
        } else {
            resolved_tokens as f64 / total_tokens as f64
        },
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
        let mut tokens = Vec::new();
        for (position, block) in blocks.iter().enumerate() {
            let next = &self.canonical[self.index[block]];
            if position == 0 {
                tokens.extend_from_slice(next);
            } else {
                separator
                    .unwrap_or(BlockSeparator::Concatenate)
                    .append(&mut tokens, next);
            }
        }
        GroupText::new(blocks.to_vec(), tokens)
    }

    fn raw_group(
        &self,
        blocks: &[BlockId],
        separator: Option<BlockSeparator>,
    ) -> Result<GroupText> {
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
        Ok(GroupText::new(blocks.to_vec(), tokens))
    }
}

struct GroupText {
    blocks: Vec<BlockId>,
    tokens: Vec<ComparableToken>,
    scalar_boundaries: Vec<usize>,
}

impl GroupText {
    fn new(blocks: Vec<BlockId>, tokens: Vec<ComparableToken>) -> Self {
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
            tokens,
            scalar_boundaries,
        }
    }

    fn full_span(&self) -> TextSpan {
        self.span(0, self.tokens.len())
    }

    fn span(&self, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: self.blocks.clone(),
            canonical_range: ScalarRange {
                start: self.scalar_boundaries[start],
                end: self.scalar_boundaries[end],
            },
            comparable_range: TokenRange { start, end },
        }
    }
}
