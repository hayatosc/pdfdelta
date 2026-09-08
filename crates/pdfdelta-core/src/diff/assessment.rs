//! Evidence assessment and exclusive source ownership for comparison results.
//!
//! Candidate ranking is deliberately separate from relation acceptance. An
//! exact edit script describes a proposed pair; it does not establish that
//! the pair belongs to the same source-backed comparison domain.

mod exact;
mod local;
mod semantic;
mod validation;
mod views;

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use super::{
    ChangeEvent, ChangeKind, Comparison, Coverage, DiffOptions, FormattingChange, GroupText,
    ProvenChangedRegion, SentenceRecoveryInput, Side, TextSpan, TokenRange, UnresolvedRegion,
    myers, sentence,
};
use crate::{
    Error, Result,
    alignment::{Alignment, AlignmentEvidence, AlignmentKind, BlockSeparator},
    layout::BlockId,
    normalize::{ComparableToken, ScalarRange},
};

/// Version of the evidence and resolution-accounting policy.
pub const ASSESSMENT_POLICY_VERSION: u32 = 1;

/// Why a proposed correspondence cannot establish a localized change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssessmentReason {
    UnknownReadingOrder,
    InferredReadingOrder,
    ExtractionGap,
    NormalizationUncertainty,
    CompetingCorrespondence,
    AmbiguousEditLocation,
    SearchIncomplete,
    DomainNotClosed,
    SourceEvidenceMissing,
    WorkLimit,
    OutputLimit,
}

/// A model assumption retained separately from exact token evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComparisonAssumption {
    /// The supplied block order is a supported reading order for this domain.
    InputReadingOrder,
    /// Comparison uses reversible canonical normalization rather than raw text.
    CanonicalNormalization,
    /// Unmapped token identity depends on the supplied font-program identity.
    UnmappedFontIdentity,
    /// A source-backed layout view supplied a synthetic inter-block separator.
    ReconstructedSpacing,
    /// Ordered correspondence does not cross declared extraction,
    /// normalization, or reading-order barriers outside this domain.
    LocalEvidenceBoundaries,
}

/// Whether the exact search required for a relation finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchCompleteness {
    /// All alternatives in the declared correspondence model were examined.
    Complete,
    /// The claim needs work that was not completed within the shared budget.
    Incomplete,
}

/// Result of assessing one proposed relation before final event emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationOutcome {
    Established,
    Tentative,
}

/// Source-backed domain and evidence for one correspondence claim.
///
/// `parent` refers to an earlier relation in the same assessment. A tentative
/// parent cannot establish a child. Scores and confidence labels are not
/// acceptance evidence. Source locations are document-local, not stable
/// identities across revisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationAssessment {
    pub old_span: Option<TextSpan>,
    pub new_span: Option<TextSpan>,
    pub parent: Option<usize>,
    pub outcome: RelationOutcome,
    pub search: SearchCompleteness,
    pub assumptions: Vec<ComparisonAssumption>,
    pub reasons: Vec<AssessmentReason>,
}

/// A proposed localized edit that remains unresolved.
///
/// `relation` indexes the owning comparison's relation assessments. Candidates
/// in the same alternative group compete; their spans may overlap. Neither
/// their confidence labels nor their exact edit ranges resolve source tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeCandidate {
    pub change: ChangeEvent,
    pub relation: usize,
    pub alternative_group: usize,
}

/// Exclusive resolution state of an extracted comparable-token interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionState {
    Equal,
    Changed,
    Unresolved,
}

/// One block-local interval in a complete, non-overlapping side partition.
///
/// Synthetic separators between blocks never contribute to this interval's
/// token count. Canonical bounds preserve projection for unmapped tokens,
/// including intervals with a nonempty token range and zero scalar width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionRange {
    pub block: BlockId,
    pub comparable_range: TokenRange,
    pub canonical_range: ScalarRange,
    pub state: ResolutionState,
}

/// Consumption of the shared assessment budget, in execution-stage order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssessmentWork {
    pub anchor_verification: usize,
    pub local_views: usize,
    pub localization: usize,
    pub emission: usize,
}

impl AssessmentWork {
    fn total(self) -> Option<usize> {
        self.anchor_verification
            .checked_add(self.local_views)?
            .checked_add(self.localization)?
            .checked_add(self.emission)
    }
}

/// Evidence and source ownership produced by the shared comparison boundary.
///
/// The old/new partitions each cover all extracted comparable tokens exactly
/// once. Candidates and unlocalized changed regions remain unresolved. These
/// partitions do not establish extraction completeness or visual equivalence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonAssessment {
    pub policy_version: u32,
    pub relations: Vec<RelationAssessment>,
    pub old_resolution: Vec<ResolutionRange>,
    pub new_resolution: Vec<ResolutionRange>,
    pub work_limit: usize,
    pub work_used: usize,
    pub work_by_stage: AssessmentWork,
    /// Additional candidate descriptions were omitted within output limits.
    pub candidates_truncated: bool,
    /// Additional programmatic evidence for emitted local changes. Standard
    /// reports summarize the corresponding relations rather than these edits.
    pub localized_edits: Vec<LocalizedEditScript>,
}

/// One optimal edit witness for a completed local comparison.
///
/// When several optimal paths exist, their emitted semantic ranges agree;
/// this witness does not identify an author's historical editing sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalizedEditScript {
    /// Established relation whose old/new spans bound this comparison.
    pub relation: usize,
    /// Contiguous indices in [`Comparison::changes`] emitted from this script.
    pub changes: Range<usize>,
    /// Coordinates start at zero within the relation's old/new spans.
    pub edits: Vec<super::AtomicEdit>,
}

impl ComparisonAssessment {
    /// Checks the structural evidence and exclusive token accounting.
    ///
    /// Caller-supplied assessments remain assertions about source evidence.
    /// This validation does not independently extract the original PDF.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] for invalid relation references,
    /// contradictory outcomes, overlapping partitions, or coverage mismatch.
    pub fn validate(&self, comparison: &Comparison) -> Result<()> {
        if self.policy_version != ASSESSMENT_POLICY_VERSION
            || self.work_used > self.work_limit
            || self.work_by_stage.total() != Some(self.work_used)
        {
            return Err(invalid(
                "invalid assessment policy version or work accounting",
            ));
        }
        for (index, relation) in self.relations.iter().enumerate() {
            if relation.old_span.is_none()
                && relation.new_span.is_none()
                && !validation::is_output_limit_sentinel(relation)
            {
                return Err(invalid("assessment relations require a source range"));
            }
            if let Some(parent) = relation.parent {
                if parent >= index {
                    return Err(invalid("assessment parents must precede their children"));
                }
                if relation.outcome == RelationOutcome::Established
                    && self.relations[parent].outcome != RelationOutcome::Established
                {
                    return Err(invalid(
                        "a tentative parent cannot establish a child relation",
                    ));
                }
            }
            match relation.outcome {
                RelationOutcome::Established
                    if !relation.reasons.is_empty()
                        || relation.search != SearchCompleteness::Complete =>
                {
                    return Err(invalid(
                        "established relations require complete, uncontradicted evidence",
                    ));
                }
                RelationOutcome::Tentative if relation.reasons.is_empty() => {
                    return Err(invalid("tentative relations require an uncertainty reason"));
                }
                _ => {}
            }
        }
        for candidate in &comparison.change_candidates {
            let relation = self
                .relations
                .get(candidate.relation)
                .ok_or_else(|| invalid("candidate refers to a missing relation"))?;
            if relation.outcome != RelationOutcome::Tentative {
                return Err(invalid("candidate relation must remain tentative"));
            }
            if candidate.alternative_group >= self.relations.len() {
                return Err(invalid("candidate alternative group is out of bounds"));
            }
        }
        let mut previous_change_end = 0;
        for trace in &self.localized_edits {
            let relation = self
                .relations
                .get(trace.relation)
                .ok_or_else(|| invalid("localized edits refer to a missing relation"))?;
            let (Some(old), Some(new)) = (&relation.old_span, &relation.new_span) else {
                return Err(invalid("localized edit contexts require both source sides"));
            };
            if relation.outcome != RelationOutcome::Established
                || trace.changes.start < previous_change_end
                || trace.changes.start >= trace.changes.end
                || trace.changes.end > comparison.changes.len()
                || trace.edits.is_empty()
            {
                return Err(invalid(
                    "localized edits require distinct emitted changes and an established context",
                ));
            }
            let lengths = [
                old.comparable_range
                    .end
                    .checked_sub(old.comparable_range.start),
                new.comparable_range
                    .end
                    .checked_sub(new.comparable_range.start),
            ];
            let [Some(old_len), Some(new_len)] = lengths else {
                return Err(invalid("localized edit context is reversed"));
            };
            let mut cursor = [0, 0];
            for edit in &trace.edits {
                if edit.old.start > edit.old.end
                    || edit.new.start > edit.new.end
                    || edit.old.end > old_len
                    || edit.new.end > new_len
                    || edit.old.start < cursor[0]
                    || edit.new.start < cursor[1]
                    || edit.old.is_empty() == edit.new.is_empty()
                    || edit.old.start - cursor[0] != edit.new.start - cursor[1]
                {
                    return Err(invalid("localized edit coordinates are inconsistent"));
                }
                cursor = [edit.old.end, edit.new.end];
            }
            if old_len - cursor[0] != new_len - cursor[1]
                || comparison.changes[trace.changes.clone()]
                    .iter()
                    .any(|change| change.kind == ChangeKind::Move)
            {
                return Err(invalid(
                    "localized edits cannot establish movement or unequal trailing ranges",
                ));
            }
            for change in &comparison.changes[trace.changes.clone()] {
                for occurrence in &change.occurrences {
                    for (span, context) in [
                        (occurrence.old_span.as_ref(), old),
                        (occurrence.new_span.as_ref(), new),
                    ] {
                        if let Some(span) = span
                            && (span.blocks != context.blocks
                                || span.separator != context.separator
                                || span.comparable_range.start < context.comparable_range.start
                                || span.comparable_range.end > context.comparable_range.end
                                || span.canonical_range.start < context.canonical_range.start
                                || span.canonical_range.end > context.canonical_range.end)
                        {
                            return Err(invalid(
                                "localized change is outside its comparison context",
                            ));
                        }
                    }
                }
            }
            previous_change_end = trace.changes.end;
        }
        validate_partition(&self.old_resolution, comparison.old_coverage)?;
        validate_partition(&self.new_resolution, comparison.new_coverage)
    }
}

fn validate_partition(ranges: &[ResolutionRange], coverage: Coverage) -> Result<()> {
    let mut previous: Option<&ResolutionRange> = None;
    let mut seen_blocks = HashSet::new();
    let mut total = 0usize;
    let mut resolved = 0usize;
    for range in ranges {
        if range.comparable_range.start >= range.comparable_range.end
            || range.canonical_range.start > range.canonical_range.end
            || range.canonical_range.end - range.canonical_range.start
                > range.comparable_range.end - range.comparable_range.start
        {
            return Err(invalid("invalid assessment resolution interval"));
        }
        match previous {
            Some(previous) if previous.block == range.block => {
                if previous.comparable_range.end != range.comparable_range.start
                    || previous.canonical_range.end != range.canonical_range.start
                {
                    return Err(invalid("assessment partition has an overlap or gap"));
                }
            }
            _ => {
                if range.comparable_range.start != 0
                    || range.canonical_range.start != 0
                    || !seen_blocks.insert(range.block)
                {
                    return Err(invalid("assessment block partition is not contiguous"));
                }
            }
        }
        let count = range.comparable_range.end - range.comparable_range.start;
        total = total
            .checked_add(count)
            .ok_or_else(|| invalid("assessment token count overflow"))?;
        if range.state != ResolutionState::Unresolved {
            resolved = resolved
                .checked_add(count)
                .ok_or_else(|| invalid("assessment resolved count overflow"))?;
        }
        previous = Some(range);
    }
    if total != coverage.total_tokens || resolved != coverage.resolved_tokens {
        return Err(invalid(
            "assessment ownership does not match reported coverage",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> Error {
    Error::InvalidConfiguration(message.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceInterval {
    block_index: usize,
    start: usize,
    end: usize,
}

/// Projects a grouped span without assigning synthetic separator tokens to a
/// source block. Both scalar and comparable coordinates must describe the same
/// interval, including zero-width scalar intervals containing unmapped tokens.
fn project(side: &Side<'_>, span: &TextSpan) -> Result<Vec<SourceInterval>> {
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(span.blocks.len())
        .map_err(|_| allocation_error("source projection"))?;
    let mut token_offset = 0usize;
    let mut scalar_offset = 0usize;
    let mut projected_start = None;
    let mut projected_end = None;
    let mut preceding_space = false;
    let mut seen = HashSet::new();
    for (position, block) in span.blocks.iter().enumerate() {
        let &block_index = side
            .index
            .get(block)
            .ok_or_else(|| invalid("assessment refers to an unknown block"))?;
        if !seen.insert(block_index) {
            return Err(invalid("assessment span repeats a source block"));
        }
        let tokens = &side.canonical[block_index];
        let separator = position > 0
            && span.separator == Some(BlockSeparator::Space)
            && !preceding_space
            && !tokens.first().is_some_and(space_token);
        if separator {
            record_boundary(
                span,
                token_offset,
                scalar_offset,
                &mut projected_start,
                &mut projected_end,
            );
            token_offset += 1;
            scalar_offset += 1;
        }
        let block_start = token_offset;
        let block_end = block_start + tokens.len();
        for boundary in [span.comparable_range.start, span.comparable_range.end] {
            if boundary >= block_start && boundary <= block_end {
                let scalar = scalar_offset
                    + block_scalar_boundary(
                        &side.blocks[block_index].canonical,
                        boundary - block_start,
                    );
                record_boundary(
                    span,
                    boundary,
                    scalar,
                    &mut projected_start,
                    &mut projected_end,
                );
            }
        }
        token_offset = block_end;
        scalar_offset += tokens.len() - side.blocks[block_index].canonical.unmapped.len();
        let start = span.comparable_range.start.max(block_start);
        let end = span.comparable_range.end.min(token_offset);
        if start < end {
            ranges.push(SourceInterval {
                block_index,
                start: start - block_start,
                end: end - block_start,
            });
        }
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, space_token);
    }
    record_boundary(
        span,
        token_offset,
        scalar_offset,
        &mut projected_start,
        &mut projected_end,
    );
    if span.comparable_range.start > span.comparable_range.end
        || projected_start != Some(span.canonical_range.start)
        || projected_end != Some(span.canonical_range.end)
        || (span.blocks.len() > 1 && span.separator.is_none())
        || (span.blocks.len() <= 1 && span.separator.is_some())
    {
        return Err(invalid("assessment source coordinates do not agree"));
    }
    Ok(ranges)
}

fn block_scalar_boundary(text: &crate::normalize::MappedText, token_boundary: usize) -> usize {
    let mut start = 0;
    let mut end = text.unmapped.len();
    while start < end {
        let middle = start + (end - start) / 2;
        // Materialization validates sorted scalar positions. Each preceding
        // unmapped token adds one comparable position but no scalar width.
        if text.unmapped[middle].scalar_index + middle < token_boundary {
            start = middle + 1;
        } else {
            end = middle;
        }
    }
    token_boundary - start
}

fn span_has_source_issues(side: &Side<'_>, span: &TextSpan, remaining: &mut usize) -> Result<bool> {
    for interval in project(side, span)? {
        let block = &side.blocks[interval.block_index];
        if block.issues.is_empty() {
            continue;
        }
        let source_items = block
            .raw
            .text
            .len()
            .saturating_add(block.canonical.text.len())
            .saturating_add(block.raw.source_map.len())
            .saturating_add(block.canonical.source_map.len())
            .saturating_add(block.normalization_events.len())
            .saturating_add(block.canonical.unmapped.len());
        if !charge_work(
            remaining,
            source_items.saturating_mul(block.issues.len().saturating_add(1)),
        ) {
            return Ok(true);
        }
        let Ok(issues) = block.checked_normalization_issue_ranges() else {
            return Ok(true);
        };
        let canonical_start = block_scalar_boundary(&block.canonical, interval.start);
        let canonical_end = block_scalar_boundary(&block.canonical, interval.end);
        if issues.iter().any(|issue| {
            let start = if issue.start == issue.end {
                issue.start.saturating_sub(1)
            } else {
                issue.start
            };
            let end = if issue.start == issue.end {
                issue.end.saturating_add(1)
            } else {
                issue.end
            };
            if canonical_start == canonical_end {
                start <= canonical_start && canonical_start <= end
            } else {
                start < canonical_end && canonical_start < end
            }
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn record_boundary(
    span: &TextSpan,
    token: usize,
    scalar: usize,
    start: &mut Option<usize>,
    end: &mut Option<usize>,
) {
    if span.comparable_range.start == token {
        *start = Some(scalar);
    }
    if span.comparable_range.end == token {
        *end = Some(scalar);
    }
}

fn space_token(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(' '))
}

fn allocation_error(resource: &'static str) -> Error {
    Error::Unresolved(format!("assessment {resource} allocation failed"))
}

/// Accepted contexts and their changed subranges are collected separately.
/// Subtracting final unresolved intervals is never used to infer equality.
struct Ownership {
    accepted: Vec<SourceInterval>,
    changed: Vec<SourceInterval>,
}

impl Ownership {
    fn new() -> Self {
        Self {
            accepted: Vec::new(),
            changed: Vec::new(),
        }
    }

    fn accept(&mut self, side: &Side<'_>, span: &TextSpan, range_limit: usize) -> Result<()> {
        let projected = project(side, span)?;
        reserve_ranges(&mut self.accepted, projected.len(), range_limit)?;
        self.accepted.extend(projected);
        Ok(())
    }

    fn change(&mut self, side: &Side<'_>, span: &TextSpan, range_limit: usize) -> Result<()> {
        let projected = project(side, span)?;
        reserve_ranges(&mut self.changed, projected.len(), range_limit)?;
        self.changed.extend(projected);
        Ok(())
    }

    fn exclude(&mut self, side: &Side<'_>, span: &TextSpan, range_limit: usize) -> Result<()> {
        for excluded in project(side, span)? {
            let mut output = Vec::new();
            for interval in &self.accepted {
                if interval.block_index != excluded.block_index
                    || interval.end <= excluded.start
                    || excluded.end <= interval.start
                {
                    reserve_ranges(&mut output, 1, range_limit)?;
                    output.push(*interval);
                } else {
                    if interval.start < excluded.start {
                        reserve_ranges(&mut output, 1, range_limit)?;
                        output.push(SourceInterval {
                            end: excluded.start,
                            ..*interval
                        });
                    }
                    if excluded.end < interval.end {
                        reserve_ranges(&mut output, 1, range_limit)?;
                        output.push(SourceInterval {
                            start: excluded.end,
                            ..*interval
                        });
                    }
                }
            }
            self.accepted = output;
        }
        Ok(())
    }

    fn finish(mut self, side: &Side<'_>, range_limit: usize) -> Result<Vec<ResolutionRange>> {
        merge_intervals(&mut self.accepted);
        merge_intervals(&mut self.changed);
        let mut accepted_by_block = HashMap::<usize, Vec<SourceInterval>>::new();
        let mut changed_by_block = HashMap::<usize, Vec<SourceInterval>>::new();
        for interval in self.accepted {
            accepted_by_block
                .entry(interval.block_index)
                .or_default()
                .push(interval);
        }
        for interval in self.changed {
            changed_by_block
                .entry(interval.block_index)
                .or_default()
                .push(interval);
        }
        let mut result = Vec::new();
        for (block_index, tokens) in side.canonical.iter().enumerate() {
            if tokens.is_empty() {
                continue;
            }
            let accepted = accepted_by_block
                .get(&block_index)
                .map_or(&[][..], Vec::as_slice);
            let changed = changed_by_block
                .get(&block_index)
                .map_or(&[][..], Vec::as_slice);
            let mut boundaries = vec![0, tokens.len()];
            for interval in accepted.iter().chain(changed) {
                boundaries.push(interval.start);
                boundaries.push(interval.end);
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            let mut scalar_start = 0usize;
            let mut accepted_cursor = 0usize;
            let mut changed_cursor = 0usize;
            for pair in boundaries.windows(2) {
                let [start, end] = [pair[0], pair[1]];
                let established = interval_contains(accepted, &mut accepted_cursor, start, end);
                let is_changed = interval_contains(changed, &mut changed_cursor, start, end);
                if is_changed && !established {
                    return Err(invalid(
                        "changed source tokens have no accepted correspondence",
                    ));
                }
                let state = if is_changed {
                    ResolutionState::Changed
                } else if established {
                    ResolutionState::Equal
                } else {
                    ResolutionState::Unresolved
                };
                let scalar_end = scalar_start
                    + tokens[start..end]
                        .iter()
                        .filter(|token| matches!(token, ComparableToken::Scalar(_)))
                        .count();
                let block = side.blocks[block_index].block;
                if let Some(previous) =
                    result.last_mut().filter(|previous: &&mut ResolutionRange| {
                        previous.block == block && previous.state == state
                    })
                {
                    previous.comparable_range.end = end;
                    previous.canonical_range.end = scalar_end;
                } else {
                    reserve_ranges(&mut result, 1, range_limit)?;
                    result.push(ResolutionRange {
                        block,
                        comparable_range: TokenRange { start, end },
                        canonical_range: ScalarRange {
                            start: scalar_start,
                            end: scalar_end,
                        },
                        state,
                    });
                }
                scalar_start = scalar_end;
            }
        }
        Ok(result)
    }
}

fn interval_contains(
    intervals: &[SourceInterval],
    cursor: &mut usize,
    start: usize,
    end: usize,
) -> bool {
    while intervals
        .get(*cursor)
        .is_some_and(|interval| interval.end <= start)
    {
        *cursor += 1;
    }
    intervals
        .get(*cursor)
        .is_some_and(|interval| interval.start <= start && interval.end >= end)
}

fn merge_intervals(intervals: &mut Vec<SourceInterval>) {
    intervals.sort_unstable_by_key(|interval| (interval.block_index, interval.start, interval.end));
    let mut retained = 0usize;
    for index in 0..intervals.len() {
        let interval = intervals[index];
        if retained > 0
            && intervals[retained - 1].block_index == interval.block_index
            && intervals[retained - 1].end >= interval.start
        {
            intervals[retained - 1].end = intervals[retained - 1].end.max(interval.end);
        } else {
            intervals[retained] = interval;
            retained += 1;
        }
    }
    intervals.truncate(retained);
}

fn reserve_ranges<T>(output: &mut Vec<T>, additional: usize, limit: usize) -> Result<()> {
    if output
        .len()
        .checked_add(additional)
        .is_none_or(|count| count > limit)
    {
        return Err(Error::LimitExceeded {
            resource: "assessment output ranges",
            limit,
        });
    }
    output
        .try_reserve_exact(additional)
        .map_err(|_| allocation_error("output ranges"))
}

/// Output of proposal discovery, before any relation becomes public evidence.
pub(super) struct ProposedComparison {
    pub changes: Vec<ChangeEvent>,
    pub proven_changed_regions: Vec<ProvenChangedRegion>,
    pub formatting_changes: Vec<FormattingChange>,
    pub unresolved_regions: Vec<UnresolvedRegion>,
}

struct ProposedRelation {
    old: Option<TextSpan>,
    new: Option<TextSpan>,
    span_indices: [Option<usize>; 2],
    exact_recovery: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProposalKey {
    old: Option<TextSpan>,
    new: Option<TextSpan>,
    span_indices: [Option<usize>; 2],
}

impl From<&ProposedRelation> for ProposalKey {
    fn from(proposal: &ProposedRelation) -> Self {
        Self {
            old: proposal.old.clone(),
            new: proposal.new.clone(),
            span_indices: proposal.span_indices,
        }
    }
}

fn full_relation_span(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
) -> Option<TextSpan> {
    (!blocks.is_empty())
        .then(|| super::try_group_full_span(side, blocks, separator))
        .flatten()
}

fn recovered_span(recovered: &sentence::RecoveredSentence) -> TextSpan {
    TextSpan {
        blocks: recovered.blocks.clone(),
        separator: recovered.separator,
        canonical_range: recovered.canonical,
        comparable_range: recovered.comparable,
    }
}

fn collect_relations(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: Option<&sentence::SentenceRecoveryPlan>,
    proposed: &ProposedComparison,
    limit: usize,
) -> Result<(Vec<ProposedRelation>, bool)> {
    let [old, new] = sides;
    let mut relations = Vec::new();
    for (index, span) in alignment.spans.iter().enumerate() {
        if span.kind == AlignmentKind::Unresolved {
            continue;
        }
        let old_span = full_relation_span(old, &span.old, span.old_separator);
        let new_span = full_relation_span(new, &span.new, span.new_separator);
        if (old_span.is_none() || new_span.is_none())
            && proposed.changes.iter().any(|change| {
                change.kind == ChangeKind::Move
                    && change.occurrences.iter().any(|occurrence| {
                        (old_span.is_some() && occurrence.old_span == old_span)
                            || (new_span.is_some() && occurrence.new_span == new_span)
                    })
            })
        {
            continue;
        }
        if proposed
            .unresolved_regions
            .iter()
            .any(|region| region.old_span == old_span && region.new_span == new_span)
        {
            continue;
        }
        if relations.len() == limit {
            return Ok((relations, true));
        }
        reserve_ranges(&mut relations, 1, limit)?;
        relations.push(ProposedRelation {
            old: old_span,
            new: new_span,
            span_indices: [
                (!span.old.is_empty()).then_some(index),
                (!span.new.is_empty()).then_some(index),
            ],
            exact_recovery: span.evidence.contains(&AlignmentEvidence::ExactCanonical),
        });
    }
    if let Some(plan) = recovery {
        for matched in &plan.matches {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(
                &mut relations,
                Some(&matched.old),
                Some(&matched.new),
                true,
                limit,
            )?;
        }
        for replacement in &plan.replacements {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(
                &mut relations,
                Some(&replacement.old),
                Some(&replacement.new),
                false,
                limit,
            )?;
        }
        for replacement in &plan.anchored_replacements {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(
                &mut relations,
                Some(&replacement.old),
                Some(&replacement.new),
                false,
                limit,
            )?;
        }
        for deletion in &plan.deletions {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(&mut relations, Some(deletion), None, false, limit)?;
        }
        for insertion in &plan.insertions {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(&mut relations, None, Some(insertion), false, limit)?;
        }
        // Cross-span recovery stores each side independently. Re-establish the
        // pair from exact source tokens instead of treating positional zip as
        // correspondence evidence.
        let mut old_keys =
            HashMap::<Vec<ComparableToken>, Vec<&sentence::RecoveredSentence>>::new();
        let mut new_keys =
            HashMap::<Vec<ComparableToken>, Vec<&sentence::RecoveredSentence>>::new();
        for (side, recovered, keys) in [
            (old, &plan.cross_span_match_old, &mut old_keys),
            (new, &plan.cross_span_match_new, &mut new_keys),
        ] {
            for part in recovered {
                let span = recovered_span(part);
                let tokens = span_tokens(side, &span)?;
                keys.entry(tokens).or_default().push(part);
            }
        }
        // Iterate source order; hash-map iteration must not affect group IDs.
        for part in &plan.cross_span_match_old {
            let key = span_tokens(old, &recovered_span(part))?;
            if let (Some(old_parts), Some(new_parts)) = (old_keys.get(&key), new_keys.get(&key))
                && old_parts.len() == 1
                && new_parts.len() == 1
            {
                if relations.len() == limit {
                    return Ok((relations, true));
                }
                push_recovered_relation(
                    &mut relations,
                    Some(part),
                    Some(new_parts[0]),
                    true,
                    limit,
                )?;
            }
        }
    }
    let mut seen = relations
        .iter()
        .map(ProposalKey::from)
        .collect::<HashSet<_>>();
    for change in &proposed.changes {
        if change.kind == ChangeKind::Move {
            continue;
        }
        for occurrence in &change.occurrences {
            let proposal = ProposedRelation {
                old: occurrence.old_span.clone(),
                new: occurrence.new_span.clone(),
                span_indices: occurrence_indices(
                    alignment,
                    [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()],
                ),
                exact_recovery: false,
            };
            if !seen.insert(ProposalKey::from(&proposal)) {
                continue;
            }
            if relations.len() == limit {
                return Ok((relations, true));
            }
            reserve_ranges(&mut relations, 1, limit)?;
            relations.push(proposal);
        }
    }
    Ok((relations, false))
}

fn push_recovered_relation(
    output: &mut Vec<ProposedRelation>,
    old: Option<&sentence::RecoveredSentence>,
    new: Option<&sentence::RecoveredSentence>,
    exact: bool,
    limit: usize,
) -> Result<()> {
    reserve_ranges(output, 1, limit)?;
    output.push(ProposedRelation {
        old: old.map(recovered_span),
        new: new.map(recovered_span),
        span_indices: [
            old.map(|part| part.span_index),
            new.map(|part| part.span_index),
        ],
        exact_recovery: exact,
    });
    Ok(())
}

fn span_tokens(side: &Side<'_>, span: &TextSpan) -> Result<Vec<ComparableToken>> {
    project(side, span)?;
    let group = side.canonical_group(&span.blocks, span.separator);
    let tokens = group
        .tokens
        .get(span.comparable_range.start..span.comparable_range.end)
        .ok_or_else(|| invalid("assessment span exceeds its source tokens"))?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(tokens.len())
        .map_err(|_| allocation_error("span tokens"))?;
    copy.extend_from_slice(tokens);
    Ok(copy)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DomainKey {
    local: Option<(TextSpan, TextSpan)>,
    old: Range<usize>,
    new: Range<usize>,
    old_separator: BlockSeparator,
    new_separator: BlockSeparator,
}

struct DomainProof {
    relation: usize,
    unique: bool,
    search: SearchCompleteness,
    edits: Vec<super::AtomicEdit>,
    lengths: [usize; 2],
    strict_unique: bool,
    stable_events: Option<Vec<ProjectedEvent>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedEvent {
    kind: ChangeKind,
    old: Option<Vec<SourceInterval>>,
    new: Option<Vec<SourceInterval>>,
}

fn projected_event(
    sides: [&Side<'_>; 2],
    kind: ChangeKind,
    old: Option<&TextSpan>,
    new: Option<&TextSpan>,
) -> Result<ProjectedEvent> {
    Ok(ProjectedEvent {
        kind,
        old: old.map(|span| project(sides[0], span)).transpose()?,
        new: new.map(|span| project(sides[1], span)).transpose()?,
    })
}

fn charge_work(remaining: &mut usize, count: usize) -> bool {
    if let Some(next) = remaining.checked_sub(count) {
        *remaining = next;
        true
    } else {
        *remaining = 0;
        false
    }
}

fn tokens_equal_with_budget(
    old: &[ComparableToken],
    new: &[ComparableToken],
    remaining: &mut usize,
) -> Option<bool> {
    if !charge_work(remaining, 1) {
        return None;
    }
    if old.len() != new.len() {
        return Some(false);
    }
    // Most source windows disagree near the beginning. Charge the inspected
    // prefix rather than exhausting the budget on an unvisited suffix.
    for (old, new) in old.iter().zip(new) {
        if !charge_work(remaining, 1) {
            return None;
        }
        if old != new {
            return Some(false);
        }
    }
    Some(true)
}

fn semantic_signature(
    sides: [&Side<'_>; 2],
    groups: [&GroupText; 2],
    edits: &[super::AtomicEdit],
    remaining: &mut usize,
    limit: usize,
) -> Result<Option<Vec<ProjectedEvent>>> {
    let [old, new] = groups;
    let token_work = old.tokens.len().saturating_add(new.tokens.len());
    if !charge_work(remaining, token_work.saturating_add(edits.len())) {
        return Ok(None);
    }
    let mut output = Vec::new();
    let mut error = None;
    let mut visit = |hunk: super::SemanticHunk, kind: ChangeKind| {
        if output.len() >= limit || !charge_work(remaining, token_work) {
            return false;
        }
        let old_span = (!hunk.old.is_empty()).then(|| old.span(hunk.old.start, hunk.old.end));
        let new_span = (!hunk.new.is_empty()).then(|| new.span(hunk.new.start, hunk.new.end));
        match projected_event(sides, kind, old_span.as_ref(), new_span.as_ref()) {
            Ok(event) => {
                output.push(event);
                true
            }
            Err(cause) => {
                error = Some(cause);
                false
            }
        }
    };
    let complete = visit_domain_hunks(old, new, edits, &mut visit);
    if let Some(error) = error {
        return Err(error);
    }
    Ok(complete.then_some(output))
}

fn visit_domain_hunks(
    old: &GroupText,
    new: &GroupText,
    edits: &[super::AtomicEdit],
    mut visit: impl FnMut(super::SemanticHunk, ChangeKind) -> bool,
) -> bool {
    match super::beneficial_line_grouped_ranges(old, new, edits) {
        Some(groups) => groups
            .into_iter()
            .all(|(old, new)| visit(super::SemanticHunk { old, new }, ChangeKind::Replacement)),
        None => super::visit_semantic_hunks(&old.tokens, &new.tokens, edits, |hunk| {
            let kind =
                super::change_kind(hunk.old.start, hunk.new.start, hunk.old.end, hunk.new.end)
                    .expect("a semantic hunk changes at least one side");
            visit(hunk, kind)
        }),
    }
}

fn block_extent(side: &Side<'_>, span: &TextSpan) -> Result<Range<usize>> {
    let first = *span
        .blocks
        .first()
        .ok_or_else(|| invalid("empty source block list"))?;
    let mut start = *side
        .index
        .get(&first)
        .ok_or_else(|| invalid("unknown source block"))?;
    let mut end = start + 1;
    for block in &span.blocks {
        let index = *side
            .index
            .get(block)
            .ok_or_else(|| invalid("unknown source block"))?;
        start = start.min(index);
        end = end.max(index + 1);
    }
    Ok(start..end)
}

fn domain_group(side: &Side<'_>, range: Range<usize>, separator: BlockSeparator) -> GroupText {
    let blocks = side.blocks[range]
        .iter()
        .map(|block| block.block)
        .collect::<Vec<_>>();
    side.canonical_group(&blocks, (blocks.len() > 1).then_some(separator))
}

fn proof_groups(sides: [&Side<'_>; 2], key: &DomainKey) -> Result<[GroupText; 2]> {
    let Some((old, new)) = &key.local else {
        return Ok([
            domain_group(sides[0], key.old.clone(), key.old_separator),
            domain_group(sides[1], key.new.clone(), key.new_separator),
        ]);
    };
    let group = |side: usize, span: &TextSpan| -> Result<GroupText> {
        GroupText::try_new(
            span.blocks.clone(),
            span.separator,
            span_tokens(sides[side], span)?,
            None,
            None,
            None,
            None,
        )
        .and_then(|group| {
            group.with_origins(span.canonical_range.start, span.comparable_range.start)
        })
        .ok_or_else(|| allocation_error("local domain source group"))
    };
    Ok([group(0, old)?, group(1, new)?])
}

fn nonempty_span(group: &GroupText) -> Option<TextSpan> {
    (!group.blocks.is_empty()).then(|| group.full_span())
}

fn assumptions(groups: [&GroupText; 2]) -> Vec<ComparisonAssumption> {
    let mut result = vec![
        ComparisonAssumption::InputReadingOrder,
        ComparisonAssumption::CanonicalNormalization,
    ];
    if groups
        .iter()
        .any(|group| group.separator == Some(BlockSeparator::Space))
    {
        result.push(ComparisonAssumption::ReconstructedSpacing);
    }
    if groups.iter().any(|group| {
        group
            .tokens
            .iter()
            .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    }) {
        result.push(ComparisonAssumption::UnmappedFontIdentity);
    }
    result
}

fn point_on_script(point: [usize; 2], edits: &[super::AtomicEdit], lengths: [usize; 2]) -> bool {
    let mut cursor = [0, 0];
    let mut index = 0;
    while index < edits.len() {
        let first = &edits[index];
        if point[0] >= cursor[0]
            && point[0] <= first.old.start
            && point[1] >= cursor[1]
            && point[1] <= first.new.start
            && point[0] - cursor[0] == point[1] - cursor[1]
        {
            return true;
        }
        let start = [first.old.start, first.new.start];
        let mut end = [first.old.end, first.new.end];
        index += 1;
        while let Some(next) = edits.get(index) {
            if [next.old.start, next.new.start] != end {
                break;
            }
            end = [next.old.end, next.new.end];
            index += 1;
        }
        // Edit-step permutations inside one hunk are not correspondence
        // boundaries. Only its two outer corners have stable coordinates.
        if point == start || point == end {
            return true;
        }
        cursor = end;
    }
    point[0] >= cursor[0]
        && point[0] <= lengths[0]
        && point[1] >= cursor[1]
        && point[1] <= lengths[1]
        && point[0] - cursor[0] == point[1] - cursor[1]
}

fn locate_in_group(
    side: &Side<'_>,
    span: Option<&TextSpan>,
    group: &GroupText,
) -> Result<Option<Range<usize>>> {
    let Some(span) = span else {
        return Ok(group.tokens.is_empty().then_some(0..0));
    };
    project(side, span)?;
    let Some(start_block) = group
        .blocks
        .iter()
        .position(|block| span.blocks.first() == Some(block))
    else {
        return Ok(None);
    };
    let end_block = start_block + span.blocks.len();
    if group.blocks.get(start_block..end_block) != Some(span.blocks.as_slice())
        || (span.blocks.len() > 1 && span.separator != group.separator)
    {
        return Ok(None);
    }
    let prefix = side.canonical_group(&group.blocks[..start_block], group.separator);
    let local = side.canonical_group(&span.blocks, span.separator);
    let separator = usize::from(
        start_block > 0
            && group.separator == Some(BlockSeparator::Space)
            && !prefix.tokens.last().is_some_and(space_token)
            && !local.tokens.first().is_some_and(space_token),
    );
    let Some(start) = (prefix.tokens.len() + separator + span.comparable_range.start)
        .checked_sub(group.comparable_origin)
    else {
        return Ok(None);
    };
    let Some(end) = (prefix.tokens.len() + separator + span.comparable_range.end)
        .checked_sub(group.comparable_origin)
    else {
        return Ok(None);
    };
    Ok((end <= group.tokens.len()).then_some(start..end))
}

fn localize_proposal(
    sides: [&Side<'_>; 2],
    spans: [Option<&TextSpan>; 2],
    groups: &[GroupText; 2],
    edits: &[super::AtomicEdit],
) -> Result<[Option<Range<usize>>; 2]> {
    let mut ranges = [
        locate_in_group(sides[0], spans[0], &groups[0])?,
        locate_in_group(sides[1], spans[1], &groups[1])?,
    ];
    for missing in 0..2 {
        let present = 1 - missing;
        if spans[missing].is_some() || ranges[missing].is_some() {
            continue;
        }
        let Some(changed) = ranges[present].as_ref() else {
            continue;
        };
        if changed.is_empty() {
            continue;
        }
        let mut boundary = None;
        // Only a pure insertion/deletion in the parent's established edit
        // script supplies the absent side. Nearby equal text is insufficient.
        super::visit_atomic_hunks(edits, |hunk| {
            let pair = [hunk.old, hunk.new];
            if pair[missing].is_empty()
                && pair[present].start <= changed.start
                && changed.end <= pair[present].end
            {
                boundary = Some(pair[missing].clone());
            }
            true
        });
        ranges[missing] = boundary;
    }
    Ok(ranges)
}

fn contains_span(
    side: &Side<'_>,
    outer: Option<&TextSpan>,
    inner: Option<&TextSpan>,
) -> Result<bool> {
    match (outer, inner) {
        (_, None) => Ok(true),
        (None, Some(_)) => Ok(false),
        (Some(outer), Some(inner)) => {
            let group = side.canonical_group(&outer.blocks, outer.separator);
            Ok(
                locate_in_group(side, Some(inner), &group)?.is_some_and(|range| {
                    range.start >= outer.comparable_range.start
                        && range.end <= outer.comparable_range.end
                }),
            )
        }
    }
}

fn occurrence_indices(alignment: &Alignment, spans: [Option<&TextSpan>; 2]) -> [Option<usize>; 2] {
    std::array::from_fn(|side| {
        spans[side].and_then(|span| {
            alignment.spans.iter().position(|aligned| {
                let blocks = if side == 0 {
                    &aligned.old
                } else {
                    &aligned.new
                };
                span.blocks
                    .first()
                    .is_some_and(|block| blocks.contains(block))
            })
        })
    })
}

fn candidate_groups(
    sides: [&Side<'_>; 2],
    candidates: &mut [ChangeCandidate],
    limit: usize,
) -> Result<bool> {
    let mut intervals = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        for occurrence in &candidate.change.occurrences {
            for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                .into_iter()
                .enumerate()
            {
                if let Some(span) = span {
                    let projected = project(sides[side], span)?;
                    if intervals.len().saturating_add(projected.len()) > limit.saturating_mul(2) {
                        return Ok(true);
                    }
                    for interval in projected {
                        intervals.push((
                            side,
                            interval.block_index,
                            interval.start,
                            interval.end,
                            index,
                        ));
                    }
                }
            }
        }
    }
    intervals.sort_unstable();
    let mut parents = (0..candidates.len()).collect::<Vec<_>>();
    let root = |parents: &mut [usize], mut index: usize| {
        while parents[index] != index {
            parents[index] = parents[parents[index]];
            index = parents[index];
        }
        index
    };
    let mut previous: Option<(usize, usize, usize, usize)> = None;
    for (side, block, start, end, index) in intervals {
        if let Some((previous_side, previous_block, previous_end, previous_index)) = previous
            && side == previous_side
            && block == previous_block
            && start < previous_end
        {
            let left = root(&mut parents, previous_index);
            let right = root(&mut parents, index);
            parents[left.max(right)] = left.min(right);
            previous = Some((side, block, previous_end.max(end), index));
        } else {
            previous = Some((side, block, end, index));
        }
    }
    let groups = (0..candidates.len())
        .map(|index| root(&mut parents, index))
        .collect::<Vec<_>>();
    for (candidate, group) in candidates.iter_mut().zip(groups) {
        candidate.alternative_group = group;
    }
    Ok(false)
}

fn unresolved_output(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    partitions: [&[ResolutionRange]; 2],
    mut original: Vec<UnresolvedRegion>,
    limit: usize,
) -> Result<Vec<UnresolvedRegion>> {
    let mut output = Vec::new();
    let mut retained = [Vec::<SourceInterval>::new(), Vec::new()];
    for aligned in &alignment.spans {
        let old_span = full_relation_span(sides[0], &aligned.old, aligned.old_separator);
        let new_span = full_relation_span(sides[1], &aligned.new, aligned.new_separator);
        if sides[0].source_token_count(&aligned.old) > 0
            || sides[1].source_token_count(&aligned.new) > 0
        {
            original.push(UnresolvedRegion {
                old_span,
                new_span,
                evidence: aligned.evidence.clone(),
            });
        }
    }
    for region in original {
        let mut projected = [Vec::new(), Vec::new()];
        let mut valid = true;
        for (side, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
            .into_iter()
            .enumerate()
        {
            if let Some(span) = span {
                projected[side] = project(sides[side], span)?;
                valid &= projected[side].iter().all(|interval| {
                    let block = sides[side].blocks[interval.block_index].block;
                    partitions[side].iter().any(|part| {
                        part.block == block
                            && part.state == ResolutionState::Unresolved
                            && part.comparable_range.start <= interval.start
                            && part.comparable_range.end >= interval.end
                    }) && !retained[side].iter().any(|previous| {
                        previous.block_index == interval.block_index
                            && previous.start < interval.end
                            && interval.start < previous.end
                    })
                });
            }
        }
        if valid
            && (projected.iter().any(|ranges| !ranges.is_empty())
                || region.evidence.contains(&AlignmentEvidence::ExtractionGap)
                || region
                    .evidence
                    .contains(&AlignmentEvidence::NormalizationIssue))
        {
            reserve_ranges(&mut output, 1, limit)?;
            output.push(region);
            for (side, projected) in projected.into_iter().enumerate() {
                retained[side].extend(projected);
            }
        }
    }
    for side in 0..2 {
        merge_intervals(&mut retained[side]);
        for range in partitions[side]
            .iter()
            .filter(|range| range.state == ResolutionState::Unresolved)
        {
            let block_index = sides[side].index[&range.block];
            let mut cursor = range.comparable_range.start;
            let group = sides[side].canonical_group(&[range.block], None);
            let mut emit = |start, end| -> Result<()> {
                if start == end {
                    return Ok(());
                }
                let span = group.span(start, end);
                let mut evidence = Vec::new();
                for aligned in &alignment.spans {
                    let blocks = if side == 0 {
                        &aligned.old
                    } else {
                        &aligned.new
                    };
                    if blocks.contains(&range.block) {
                        for reason in &aligned.evidence {
                            if !evidence.contains(reason) {
                                evidence.push(*reason);
                            }
                        }
                    }
                }
                reserve_ranges(&mut output, 1, limit)?;
                output.push(UnresolvedRegion {
                    old_span: (side == 0).then(|| span.clone()),
                    new_span: (side == 1).then_some(span),
                    evidence,
                });
                Ok(())
            };
            for previous in retained[side]
                .iter()
                .filter(|previous| previous.block_index == block_index)
            {
                if previous.end <= cursor || previous.start >= range.comparable_range.end {
                    continue;
                }
                emit(cursor, previous.start.max(cursor))?;
                cursor = cursor.max(previous.end);
            }
            emit(cursor, range.comparable_range.end)?;
        }
    }
    Ok(output)
}

/// The only transition from discovered proposals to public comparison output.
pub(super) fn finish(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: Option<SentenceRecoveryInput<'_>>,
    recovery_plan: Option<&sentence::SentenceRecoveryPlan>,
    proposed: ProposedComparison,
    options: DiffOptions,
) -> Result<Comparison> {
    let (proposals, proposals_truncated) = collect_relations(
        sides,
        alignment,
        recovery_plan,
        &proposed,
        options.max_assessment_ranges,
    )?;
    let mut assessor = Assessor::new(sides, alignment, recovery, options)?;
    let after_anchors = assessor.remaining_work;
    let mut accepted = Vec::new();
    for proposal in &proposals {
        let index = assessor.assess_cached(proposal)?;
        if assessor.records[index].outcome == RelationOutcome::Established {
            if assessor.validate_semantic_emission(index, &proposed.changes)? {
                accepted.push(index);
            } else {
                assessor.reject_semantic(proposal);
            }
        }
    }
    for change in &proposed.changes {
        if change.kind != ChangeKind::Move {
            continue;
        }
        for occurrence in &change.occurrences {
            let proposal = ProposedRelation {
                old: occurrence.old_span.clone(),
                new: occurrence.new_span.clone(),
                span_indices: occurrence_indices(
                    alignment,
                    [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()],
                ),
                exact_recovery: false,
            };
            assessor.assess_move_cached(&proposal)?;
        }
    }
    let after_initial_localization = assessor.remaining_work;
    assessor.discover_local_domains(&proposals)?;
    let after_views = assessor.remaining_work;
    if assessor.remaining_work > 0
        && (!assessor.local_domains.is_empty() || !assessor.local_anchors.is_empty())
    {
        for proposal in &proposals {
            // Preserve completed exact anchors before spending optional work
            // on edited domains. Local content is emitted from the parent's
            // verified edit script after ordinary result emission.
            if !proposal.exact_recovery {
                continue;
            }
            let Some(previous) = assessor.cached_relation(proposal) else {
                continue;
            };
            if assessor.semantic_rejected(proposal) {
                continue;
            }
            if assessor.records[previous].outcome != RelationOutcome::Tentative {
                continue;
            }
            let index = assessor.reassess(proposal)?;
            if assessor.records[index].outcome == RelationOutcome::Established {
                if assessor.validate_semantic_emission(index, &proposed.changes)? {
                    accepted.push(index);
                } else {
                    assessor.reject_semantic(proposal);
                }
            }
        }
    }
    let after_localization = assessor.remaining_work;
    let mut ownership = [Ownership::new(), Ownership::new()];
    for &index in &accepted {
        let relation = &assessor.records[index];
        for (side, span) in [relation.old_span.as_ref(), relation.new_span.as_ref()]
            .into_iter()
            .enumerate()
        {
            if let Some(span) = span {
                ownership[side].accept(sides[side], span, options.max_assessment_ranges)?;
            }
        }
    }
    let mut changes = Vec::new();
    let mut candidates = Vec::new();
    let mut candidates_truncated = proposals_truncated;
    for change in proposed.changes {
        let mut established = Vec::new();
        for occurrence in &change.occurrences {
            let proposal = ProposedRelation {
                old: occurrence.old_span.clone(),
                new: occurrence.new_span.clone(),
                span_indices: occurrence_indices(
                    alignment,
                    [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()],
                ),
                exact_recovery: false,
            };
            let cached_relation = assessor.cached_relation(&proposal);
            let cached_move = if change.kind == ChangeKind::Move {
                assessor.cached_move(&proposal)
            } else {
                None
            };
            let semantic_rejected = assessor.semantic_rejected(&proposal);
            let mut owner = if semantic_rejected {
                None
            } else if let Some(move_result) = cached_move {
                move_result
            } else {
                cached_relation.filter(|&index| {
                    assessor
                        .records
                        .get(index)
                        .is_some_and(|relation| relation.outcome == RelationOutcome::Established)
                })
            };
            if owner.is_none()
                && !semantic_rejected
                && !(change.kind == ChangeKind::Move && cached_move.is_some())
            {
                for &index in &accepted {
                    let relation = &assessor.records[index];
                    if contains_span(
                        sides[0],
                        relation.old_span.as_ref(),
                        occurrence.old_span.as_ref(),
                    )? && contains_span(
                        sides[1],
                        relation.new_span.as_ref(),
                        occurrence.new_span.as_ref(),
                    )? {
                        owner = Some(index);
                        break;
                    }
                }
            }
            if !semantic_rejected && change.kind == ChangeKind::Move && cached_move.is_none() {
                owner = assessor.assess_move_cached(&proposal)?;
                if let Some(index) = owner {
                    assessor.remember_relation(&proposal, index);
                    for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(span) = span {
                            ownership[side].accept(
                                sides[side],
                                span,
                                options.max_assessment_ranges,
                            )?;
                        }
                    }
                }
            }
            if !semantic_rejected && let Some(Some(_)) = cached_move {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        ownership[side].accept(sides[side], span, options.max_assessment_ranges)?;
                    }
                }
            }
            if owner.is_some() {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        ownership[side].change(sides[side], span, options.max_assessment_ranges)?;
                    }
                }
                established.push(occurrence.clone());
            } else {
                let mut relation = cached_relation.unwrap_or(assessor.assess_cached(&proposal)?);
                if assessor.records[relation].outcome == RelationOutcome::Established {
                    let mut record = assessor.records[relation].clone();
                    record.parent = Some(relation);
                    record.outcome = RelationOutcome::Tentative;
                    record.reasons.push(AssessmentReason::DomainNotClosed);
                    relation = assessor.record(record)?;
                }
                let alternative_group = assessor.records[relation].parent.unwrap_or(relation);
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        ownership[side].exclude(
                            sides[side],
                            span,
                            options.max_assessment_ranges,
                        )?;
                    }
                }
                if candidates.len() < options.max_assessment_ranges {
                    candidates.push(ChangeCandidate {
                        change: ChangeEvent {
                            occurrences: vec![occurrence.clone()],
                            ..change.clone()
                        },
                        relation,
                        alternative_group,
                    });
                } else {
                    candidates_truncated = true;
                }
            }
        }
        if !established.is_empty() {
            changes.push(ChangeEvent {
                occurrences: established,
                ..change
            });
        }
    }
    let mut formatting_changes = Vec::new();
    candidates_truncated |=
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
    for formatting in proposed.formatting_changes {
        for &index in &accepted {
            let relation = &assessor.records[index];
            if contains_span(
                sides[0],
                relation.old_span.as_ref(),
                Some(&formatting.old_span),
            )? && contains_span(
                sides[1],
                relation.new_span.as_ref(),
                Some(&formatting.new_span),
            )? {
                formatting_changes.push(formatting);
                break;
            }
        }
    }
    let [old_ownership, new_ownership] = ownership;
    let old_resolution = old_ownership.finish(sides[0], options.max_assessment_ranges)?;
    let new_resolution = new_ownership.finish(sides[1], options.max_assessment_ranges)?;
    let unresolved_regions = unresolved_output(
        sides,
        alignment,
        [&old_resolution, &new_resolution],
        proposed.unresolved_regions,
        options.max_assessment_ranges,
    )?;
    let mut proven_changed_regions = Vec::new();
    // Local recovery can resolve part of an earlier unlocalized proof. Its
    // original whole-domain proof cannot be reused for the remaining ranges.
    let entirely_unresolved = |spans: [Option<&TextSpan>; 2]| -> Result<bool> {
        for (side, span) in spans.into_iter().enumerate() {
            let Some(span) = span else {
                continue;
            };
            let partition = if side == 0 {
                &old_resolution
            } else {
                &new_resolution
            };
            if !project(sides[side], span)?.iter().all(|interval| {
                partition.iter().any(|part| {
                    part.block == sides[side].blocks[interval.block_index].block
                        && part.state == ResolutionState::Unresolved
                        && part.comparable_range.start <= interval.start
                        && part.comparable_range.end >= interval.end
                })
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    for region in proposed.proven_changed_regions {
        let proposal = ProposedRelation {
            old: region.old_span.clone(),
            new: region.new_span.clone(),
            span_indices: occurrence_indices(
                alignment,
                [region.old_span.as_ref(), region.new_span.as_ref()],
            ),
            exact_recovery: false,
        };
        let key = assessor.domain_key(&proposal)?;
        assessor.prove_domain(&key)?;
        let domain = &assessor.records[assessor.domains[&key].relation];
        if domain.outcome == RelationOutcome::Established
            && domain.old_span == region.old_span
            && domain.new_span == region.new_span
            && entirely_unresolved([region.old_span.as_ref(), region.new_span.as_ref()])?
        {
            proven_changed_regions.push(region);
        }
    }
    let domain_indices = assessor
        .domains
        .values()
        .map(|proof| proof.relation)
        .collect::<HashSet<_>>();
    // Source order makes emission independent of hash-map iteration order.
    for index in 0..assessor.records.len() {
        if !domain_indices.contains(&index) {
            continue;
        }
        let relation = &assessor.records[index];
        if relation.outcome != RelationOutcome::Established {
            continue;
        }
        if !entirely_unresolved([relation.old_span.as_ref(), relation.new_span.as_ref()])?
            || proven_changed_regions.iter().any(|region| {
                region.old_span == relation.old_span && region.new_span == relation.new_span
            })
        {
            continue;
        }
        let old_span = relation.old_span.clone();
        let new_span = relation.new_span.clone();
        let old_tokens = old_span
            .as_ref()
            .map(|span| span_tokens(sides[0], span))
            .transpose()?
            .unwrap_or_default();
        let new_tokens = new_span
            .as_ref()
            .map(|span| span_tokens(sides[1], span))
            .transpose()?
            .unwrap_or_default();
        if !assessor.charge(old_tokens.len().saturating_add(new_tokens.len())) {
            continue;
        }
        let mut counts = HashMap::<&ComparableToken, i64>::new();
        for token in &old_tokens {
            *counts.entry(token).or_default() += 1;
        }
        for token in &new_tokens {
            *counts.entry(token).or_default() -= 1;
        }
        if counts.values().all(|count| *count == 0) {
            continue;
        }
        proven_changed_regions.push(ProvenChangedRegion {
            old_span,
            new_span,
            confidence: super::Confidence::High,
            proof: if old_tokens.is_empty() || new_tokens.is_empty() {
                super::ChangedRegionProof::OneSidedNonEmptyRange
            } else {
                super::ChangedRegionProof::ExactTokenMultisetMismatch
            },
        });
    }
    let coverage = |partition: &[ResolutionRange], total| {
        super::coverage(
            partition
                .iter()
                .filter(|range| range.state != ResolutionState::Unresolved)
                .map(|range| range.comparable_range.end - range.comparable_range.start)
                .sum(),
            total,
        )
    };
    if candidate_groups(sides, &mut candidates, options.max_assessment_ranges)? {
        candidates.clear();
        candidates_truncated = true;
    }
    let mut comparison = Comparison {
        changes,
        change_candidates: candidates,
        proven_changed_regions,
        formatting_changes,
        unresolved_regions,
        old_coverage: coverage(&old_resolution, sides[0].total_tokens),
        new_coverage: coverage(&new_resolution, sides[1].total_tokens),
        assessment: None,
    };
    let assessment = ComparisonAssessment {
        policy_version: ASSESSMENT_POLICY_VERSION,
        relations: assessor.records,
        old_resolution,
        new_resolution,
        work_limit: options.max_assessment_work,
        work_used: options.max_assessment_work - assessor.remaining_work,
        work_by_stage: AssessmentWork {
            anchor_verification: options.max_assessment_work - after_anchors,
            local_views: after_initial_localization - after_views,
            localization: (after_anchors - after_initial_localization)
                + (after_views - after_localization),
            emission: after_localization - assessor.remaining_work,
        },
        candidates_truncated: candidates_truncated || assessor.output_stop.is_some(),
        localized_edits: assessor.localized_edits,
    };
    validation::validate(sides, &assessment, &comparison)?;
    comparison.assessment = Some(assessment);
    Ok(comparison)
}

struct Assessor<'a, 'document> {
    sides: [&'a Side<'document>; 2],
    alignment: &'a Alignment,
    recovery: Option<SentenceRecoveryInput<'a>>,
    options: DiffOptions,
    remaining_work: usize,
    anchors: Vec<(usize, usize)>,
    domains: HashMap<DomainKey, DomainProof>,
    records: Vec<RelationAssessment>,
    output_stop: Option<usize>,
    root_relation: Option<usize>,
    root_reasons: Vec<AssessmentReason>,
    semantic_acceptance: HashMap<usize, DomainKey>,
    local_domains: Vec<views::LocalDomain>,
    local_anchors: Vec<views::LocalDomain>,
    localized_edits: Vec<LocalizedEditScript>,
    localized_edit_count: usize,
    proposal_relations: HashMap<ProposalKey, usize>,
    semantic_rejections: HashSet<ProposalKey>,
    move_relations: HashMap<ProposalKey, Option<usize>>,
}

impl<'a, 'document> Assessor<'a, 'document> {
    fn validate_semantic_emission(
        &mut self,
        index: usize,
        changes: &[ChangeEvent],
    ) -> Result<bool> {
        let Some(key) = self.semantic_acceptance.get(&index) else {
            return Ok(true);
        };
        let expected = self.domains[key]
            .stable_events
            .clone()
            .expect("semantic proof has a signature");
        let old = self.records[index].old_span.clone();
        let new = self.records[index].new_span.clone();
        let context_work = old
            .as_ref()
            .map_or(0, |span| self.sides[0].source_token_count(&span.blocks))
            .saturating_add(
                new.as_ref()
                    .map_or(0, |span| self.sides[1].source_token_count(&span.blocks)),
            );
        let mut actual = Vec::new();
        for change in changes {
            if change.kind == ChangeKind::Move {
                continue;
            }
            for occurrence in &change.occurrences {
                let source_work = occurrence
                    .old_span
                    .as_ref()
                    .map_or(0, |span| self.sides[0].source_token_count(&span.blocks))
                    .saturating_add(
                        occurrence
                            .new_span
                            .as_ref()
                            .map_or(0, |span| self.sides[1].source_token_count(&span.blocks)),
                    );
                if !self.charge(context_work.saturating_add(source_work).saturating_mul(2)) {
                    self.records[index].outcome = RelationOutcome::Tentative;
                    self.records[index].search = SearchCompleteness::Incomplete;
                    self.records[index]
                        .reasons
                        .push(AssessmentReason::WorkLimit);
                    return Ok(false);
                }
                if contains_span(self.sides[0], old.as_ref(), occurrence.old_span.as_ref())?
                    && contains_span(self.sides[1], new.as_ref(), occurrence.new_span.as_ref())?
                {
                    actual.push(projected_event(
                        self.sides,
                        change.kind,
                        occurrence.old_span.as_ref(),
                        occurrence.new_span.as_ref(),
                    )?);
                }
            }
        }
        if actual == expected {
            return Ok(true);
        }
        self.records[index].outcome = RelationOutcome::Tentative;
        self.records[index]
            .reasons
            .push(AssessmentReason::AmbiguousEditLocation);
        Ok(false)
    }

    fn assess_move(&mut self, proposal: &ProposedRelation) -> Result<Option<usize>> {
        let (Some(old), Some(new)) = (&proposal.old, &proposal.new) else {
            return Ok(None);
        };
        let tokens = span_tokens(self.sides[0], old)?;
        if tokens.is_empty() || tokens != span_tokens(self.sides[1], new)? {
            return Ok(None);
        }
        let extents = self.proposal_extents(proposal)?;
        let crossed = self
            .anchors
            .iter()
            .copied()
            .filter(|&(a, b)| {
                (extents[0].end <= a && extents[1].start > b)
                    || (extents[0].start > a && extents[1].end <= b)
            })
            .collect::<Vec<_>>();
        let mut isolated = None;
        for (a, b) in crossed {
            if !self.charge(self.alignment.spans.len().saturating_add(1)) {
                return Ok(None);
            }
            let key = DomainKey {
                local: None,
                old: extents[0].start.min(a)..extents[0].end.max(a + 1),
                new: extents[1].start.min(b)..extents[1].end.max(b + 1),
                old_separator: old.separator.unwrap_or(BlockSeparator::Space),
                new_separator: new.separator.unwrap_or(BlockSeparator::Space),
            };
            let (reasons, local) = self.domain_reasons(&key)?;
            if reasons.is_empty() {
                isolated = Some(local);
                break;
            }
        }
        let Some(isolated) = isolated else {
            return Ok(None);
        };
        let mut groups = Vec::new();
        for side in 0..2 {
            let group = domain_group(
                self.sides[side],
                0..self.sides[side].blocks.len(),
                BlockSeparator::Space,
            );
            if !self.charge(group.tokens.len().saturating_mul(tokens.len())) {
                return Ok(None);
            }
            if group
                .tokens
                .windows(tokens.len())
                .filter(|window| *window == tokens.as_slice())
                .count()
                != 1
            {
                return Ok(None);
            }
            groups.push(group);
        }
        let mut move_assumptions = assumptions([&groups[0], &groups[1]]);
        if isolated {
            move_assumptions.push(ComparisonAssumption::LocalEvidenceBoundaries);
        }
        self.record(RelationAssessment {
            old_span: Some(old.clone()),
            new_span: Some(new.clone()),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: move_assumptions,
            reasons: Vec::new(),
        })
        .map(Some)
    }
    fn discover_local_domains(&mut self, proposals: &[ProposedRelation]) -> Result<()> {
        if self.source_reasons().is_empty() {
            return self.discover_ordered_domains();
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let anchors = proposals
            .iter()
            .filter(|proposal| proposal.exact_recovery)
            .filter_map(|proposal| {
                Some((
                    proposal.old.as_ref()?.clone(),
                    proposal.new.as_ref()?.clone(),
                ))
            })
            .collect::<Vec<_>>();
        let discovery = views::discover(
            self.sides,
            recovery,
            &anchors,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
        )?;
        self.local_domains = discovery.domains;
        self.local_anchors = discovery.anchors;
        if self.remaining_work == 0 {
            // The root already retains reading-order uncertainty. Record the
            // unfinished optional search without changing established local
            // relations whose checks completed before this attempt.
            let root = self.root_relation()?;
            self.records[root].search = SearchCompleteness::Incomplete;
            if !self.records[root]
                .reasons
                .contains(&AssessmentReason::WorkLimit)
            {
                self.records[root].reasons.push(AssessmentReason::WorkLimit);
            }
        }
        Ok(())
    }

    fn source_reasons(&self) -> Vec<AssessmentReason> {
        self.root_reasons.clone()
    }

    fn domain_reasons(&mut self, key: &DomainKey) -> Result<(Vec<AssessmentReason>, bool)> {
        let mut reasons = self.source_reasons();
        if key.local.is_some() {
            reasons.retain(|reason| {
                !matches!(
                    reason,
                    AssessmentReason::UnknownReadingOrder | AssessmentReason::InferredReadingOrder
                )
            });
        }
        let has_barrier = reasons.iter().any(|reason| {
            matches!(
                reason,
                AssessmentReason::ExtractionGap
                    | AssessmentReason::NormalizationUncertainty
                    | AssessmentReason::UnknownReadingOrder
                    | AssessmentReason::InferredReadingOrder
            )
        });
        if !has_barrier {
            return Ok((reasons, false));
        }
        let local_issue = if let Some((old, new)) = &key.local {
            span_has_source_issues(self.sides[0], old, &mut self.remaining_work)?
                || span_has_source_issues(self.sides[1], new, &mut self.remaining_work)?
        } else {
            true
        };
        let intersects = |blocks: &[BlockId], side: usize| {
            if let Some((old, new)) = &key.local {
                let local = if side == 0 { old } else { new };
                return blocks.iter().any(|block| local.blocks.contains(block));
            }
            let range = if side == 0 { &key.old } else { &key.new };
            blocks
                .iter()
                .any(|block| range.contains(&self.sides[side].index[block]))
        };
        let touches_barrier = self.alignment.spans.iter().any(|span| {
            span.evidence.iter().any(|reason| {
                matches!(reason, AlignmentEvidence::ExtractionGap)
                    || (local_issue && *reason == AlignmentEvidence::NormalizationIssue)
                    || (key.local.is_none()
                        && matches!(
                            reason,
                            AlignmentEvidence::ReadingOrderUnknown
                                | AlignmentEvidence::ReadingOrderInferred
                        ))
            }) && (intersects(&span.old, 0) || intersects(&span.new, 1))
        }) || self.sides.iter().enumerate().any(|(side, source)| {
            source.blocks.iter().any(|block| {
                local_issue && !block.issues.is_empty() && intersects(&[block.block], side)
            })
        });
        if has_barrier && !touches_barrier {
            reasons.retain(|reason| {
                !matches!(
                    reason,
                    AssessmentReason::ExtractionGap
                        | AssessmentReason::NormalizationUncertainty
                        | AssessmentReason::UnknownReadingOrder
                        | AssessmentReason::InferredReadingOrder
                )
            });
            return Ok((reasons, true));
        }
        Ok((reasons, false))
    }

    fn inspect_source_reasons(&self) -> Vec<AssessmentReason> {
        let mut reasons = Vec::new();
        for span in &self.alignment.spans {
            for evidence in &span.evidence {
                let reason = match evidence {
                    AlignmentEvidence::ReadingOrderUnknown => {
                        Some(AssessmentReason::UnknownReadingOrder)
                    }
                    AlignmentEvidence::ReadingOrderInferred => {
                        Some(AssessmentReason::InferredReadingOrder)
                    }
                    AlignmentEvidence::ExtractionGap => Some(AssessmentReason::ExtractionGap),
                    AlignmentEvidence::NormalizationIssue => {
                        Some(AssessmentReason::NormalizationUncertainty)
                    }
                    _ => None,
                };
                if let Some(reason) = reason
                    && !reasons.contains(&reason)
                {
                    reasons.push(reason);
                }
            }
        }
        if self
            .sides
            .iter()
            .any(|side| side.blocks.iter().any(|block| !block.issues.is_empty()))
            && !reasons.contains(&AssessmentReason::NormalizationUncertainty)
        {
            reasons.push(AssessmentReason::NormalizationUncertainty);
        }
        if let Some(recovery) = self.recovery {
            let intervals = [
                recovery.old_trusted_run_intervals,
                recovery.new_trusted_run_intervals,
            ];
            let complete_run = |side: usize| {
                if self.sides[side].blocks.is_empty() {
                    return true;
                }
                let Some(first) = intervals[side].first().copied().flatten() else {
                    return false;
                };
                if first.start != 0 {
                    return false;
                }
                let evidence = if side == 0 {
                    recovery.old_trusted_run_evidence
                } else {
                    recovery.new_trusted_run_evidence
                };
                if let Some(evidence) = evidence {
                    let Some(descriptor) = evidence
                        .descriptors
                        .iter()
                        .find(|descriptor| descriptor.id == first.run_id)
                    else {
                        return false;
                    };
                    if !descriptor.block_indices.is_empty()
                        || !descriptor.trusted_block_indices.is_empty()
                    {
                        let complete = |indices: &[usize]| {
                            (0..self.sides[side].blocks.len()).eq(indices.iter().copied())
                        };
                        if descriptor.role.is_none()
                            || !complete(&descriptor.block_indices)
                            || !complete(&descriptor.trusted_block_indices)
                        {
                            return false;
                        }
                    }
                }
                let mut end = first.start;
                intervals[side].iter().all(|interval| {
                    interval.is_some_and(|interval| {
                        let contiguous = interval.run_id == first.run_id && interval.start == end;
                        end = interval.end;
                        contiguous
                    })
                })
            };
            // A run covering every extracted block supplies internal order
            // even when layout could not establish order between regions.
            if complete_run(0) && complete_run(1) {
                reasons.retain(|reason| {
                    !matches!(
                        reason,
                        AssessmentReason::UnknownReadingOrder
                            | AssessmentReason::InferredReadingOrder
                    )
                });
            }
        }
        reasons
    }

    fn proposal_extents(&self, proposal: &ProposedRelation) -> Result<[Range<usize>; 2]> {
        let mut extents = [0..0, 0..0];
        for (side_index, span) in [&proposal.old, &proposal.new].into_iter().enumerate() {
            if let Some(span) = span {
                extents[side_index] = block_extent(self.sides[side_index], span)?;
            } else {
                let index = proposal.span_indices[1 - side_index].unwrap_or(0);
                let position = self.alignment.spans[..index]
                    .iter()
                    .map(|span| {
                        if side_index == 0 {
                            span.old.len()
                        } else {
                            span.new.len()
                        }
                    })
                    .sum();
                extents[side_index] = position..position;
            }
        }
        Ok(extents)
    }

    fn domain_key(&self, proposal: &ProposedRelation) -> Result<DomainKey> {
        if proposal.old.is_some() || proposal.new.is_some() {
            let anchors = if proposal.exact_recovery {
                self.local_anchors.as_slice()
            } else {
                &[]
            };
            let local_key = |local: &views::LocalDomain| -> Result<DomainKey> {
                Ok(DomainKey {
                    local: Some((local.old_span.clone(), local.new_span.clone())),
                    old: block_extent(self.sides[0], &local.old_span)?,
                    new: block_extent(self.sides[1], &local.new_span)?,
                    old_separator: local.old_span.separator.unwrap_or(BlockSeparator::Space),
                    new_separator: local.new_span.separator.unwrap_or(BlockSeparator::Space),
                })
            };
            // Local emission already supplies a discovered domain verbatim.
            // Avoid rebuilding every other view's tokens to locate that key.
            if let Some(local) = anchors.iter().chain(&self.local_domains).find(|local| {
                proposal.old.as_ref() == Some(&local.old_span)
                    && proposal.new.as_ref() == Some(&local.new_span)
            }) {
                return local_key(local);
            }
            for local in anchors.iter().chain(&self.local_domains) {
                if contains_span(self.sides[0], Some(&local.old_span), proposal.old.as_ref())?
                    && contains_span(self.sides[1], Some(&local.new_span), proposal.new.as_ref())?
                {
                    return local_key(local);
                }
            }
        }
        let [old, new] = self.proposal_extents(proposal)?;
        let mut starts = [0, 0];
        let mut ends = [self.sides[0].blocks.len(), self.sides[1].blocks.len()];
        for &(a, b) in &self.anchors {
            if old == (a..a + 1) && new == (b..b + 1) {
                starts = [a, b];
                ends = [a + 1, b + 1];
                break;
            }
            if a < old.start && b < new.start {
                starts = [a + 1, b + 1];
            }
            if a >= old.end && b >= new.end {
                ends = [a, b];
                break;
            }
        }
        Ok(DomainKey {
            local: None,
            old: starts[0]..ends[0],
            new: starts[1]..ends[1],
            old_separator: proposal
                .old
                .as_ref()
                .and_then(|span| span.separator)
                .unwrap_or(BlockSeparator::Space),
            new_separator: proposal
                .new
                .as_ref()
                .and_then(|span| span.separator)
                .unwrap_or(BlockSeparator::Space),
        })
    }

    fn prove_domain(&mut self, key: &DomainKey) -> Result<()> {
        if self.domains.contains_key(key) {
            return Ok(());
        }
        let parent = self.root_relation()?;
        let [old, new] = proof_groups(self.sides, key)?;
        let (mut reasons, isolated) = self.domain_reasons(key)?;
        let mut unique = false;
        let mut strict_unique = false;
        let mut stable_events = None;
        let mut edits = Vec::new();
        let mut search = SearchCompleteness::Complete;
        if reasons.is_empty() {
            match exact::check(&old.tokens, &new.tokens, &mut self.remaining_work) {
                Ok(exact::ExactUniqueness::Unique) => {
                    strict_unique = true;
                    // Charge the bounded reconstruction as well as the
                    // exhaustive uniqueness search before running Myers.
                    let bound = old
                        .tokens
                        .len()
                        .saturating_add(new.tokens.len())
                        .saturating_mul(
                            self.options
                                .max_edit_distance
                                .min(old.tokens.len().saturating_add(new.tokens.len()))
                                .saturating_add(1),
                        );
                    if old.tokens == new.tokens {
                        unique = true;
                    } else if self.charge(bound) {
                        match myers::diff(&old.tokens, &new.tokens, self.options.max_edit_distance)
                        {
                            Ok(Some(script)) => {
                                unique = true;
                                edits = script;
                            }
                            Ok(None) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                                search = SearchCompleteness::Incomplete;
                            }
                            Err(error) => return Err(error),
                        }
                    } else {
                        search = SearchCompleteness::Incomplete;
                    }
                }
                Ok(exact::ExactUniqueness::Ambiguous) => {
                    let sides = self.sides;
                    let limit = self.options.max_assessment_ranges;
                    match semantic::check_hunks(
                        &old.tokens,
                        &new.tokens,
                        &mut self.remaining_work,
                        |edits, remaining| {
                            semantic_signature(sides, [&old, &new], edits, remaining, limit)
                        },
                    ) {
                        Ok(semantic::Outcome::Unique {
                            signature,
                            edits: witness,
                        }) => {
                            unique = true;
                            stable_events = Some(signature);
                            edits = witness;
                        }
                        Ok(semantic::Outcome::Ambiguous) => {}
                        Ok(semantic::Outcome::BudgetExceeded)
                        | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                            search = SearchCompleteness::Incomplete;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(exact::ExactUniqueness::BudgetExceeded)
                | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                    search = SearchCompleteness::Incomplete;
                }
                Err(error) => return Err(error),
            }
        }
        // Domain correspondence and edit localization are separate claims.
        // A closed domain remains valid when its internal LCS is ambiguous.
        let closed = reasons.is_empty();
        if !closed {
            reasons.push(AssessmentReason::DomainNotClosed);
        }
        let mut domain_assumptions = assumptions([&old, &new]);
        if isolated {
            domain_assumptions.push(ComparisonAssumption::LocalEvidenceBoundaries);
        }
        let relation = self.record(RelationAssessment {
            old_span: nonempty_span(&old),
            new_span: nonempty_span(&new),
            parent: (!isolated && key.local.is_none()).then_some(parent),
            outcome: if closed {
                RelationOutcome::Established
            } else {
                RelationOutcome::Tentative
            },
            search: SearchCompleteness::Complete,
            assumptions: domain_assumptions,
            reasons,
        })?;
        if search == SearchCompleteness::Incomplete {
            // The domain boundary remains established; dependent localization
            // records carry the incomplete-search reason.
            unique = false;
        }
        self.domains.insert(
            key.clone(),
            DomainProof {
                relation,
                unique,
                strict_unique,
                stable_events,
                search,
                edits,
                lengths: [old.tokens.len(), new.tokens.len()],
            },
        );
        Ok(())
    }

    fn assess(&mut self, proposal: &ProposedRelation) -> Result<usize> {
        if let Some(index) = self.output_stop {
            return Ok(index);
        }
        let key = self.domain_key(proposal)?;
        self.prove_domain(&key)?;
        let proof = &self.domains[&key];
        let parent = proof.relation;
        let mut reasons = self.records[parent].reasons.clone();
        let mut search = SearchCompleteness::Complete;
        if reasons.is_empty() && !proof.unique {
            reasons.push(if proof.search == SearchCompleteness::Incomplete {
                AssessmentReason::WorkLimit
            } else {
                AssessmentReason::AmbiguousEditLocation
            });
            search = proof.search;
        }
        if reasons.is_empty()
            && !proof.strict_unique
            && (proposal.old != self.records[parent].old_span
                || proposal.new != self.records[parent].new_span)
        {
            reasons.push(AssessmentReason::AmbiguousEditLocation);
        }
        if reasons.is_empty() {
            let groups = proof_groups(self.sides, &key)?;
            let ranges = localize_proposal(
                self.sides,
                [proposal.old.as_ref(), proposal.new.as_ref()],
                &groups,
                &proof.edits,
            )?;
            match ranges {
                [Some(old), Some(new)]
                    if point_on_script([old.start, new.start], &proof.edits, proof.lengths)
                        && point_on_script([old.end, new.end], &proof.edits, proof.lengths) => {}
                _ => reasons.push(AssessmentReason::CompetingCorrespondence),
            }
        }
        let semantic_proof = reasons.is_empty() && proof.stable_events.is_some();
        let index = self.record(RelationAssessment {
            old_span: proposal.old.clone(),
            new_span: proposal.new.clone(),
            parent: Some(parent),
            outcome: if reasons.is_empty() {
                RelationOutcome::Established
            } else {
                RelationOutcome::Tentative
            },
            search,
            assumptions: self.records[parent].assumptions.clone(),
            reasons,
        })?;
        if semantic_proof {
            self.semantic_acceptance.insert(index, key);
        }
        Ok(index)
    }

    fn cached_relation(&self, proposal: &ProposedRelation) -> Option<usize> {
        self.proposal_relations
            .get(&ProposalKey::from(proposal))
            .copied()
    }

    fn remember_relation(&mut self, proposal: &ProposedRelation, index: usize) {
        self.proposal_relations
            .insert(ProposalKey::from(proposal), index);
    }

    fn reject_semantic(&mut self, proposal: &ProposedRelation) {
        self.semantic_rejections.insert(ProposalKey::from(proposal));
    }

    fn semantic_rejected(&self, proposal: &ProposedRelation) -> bool {
        self.semantic_rejections
            .contains(&ProposalKey::from(proposal))
    }

    fn cached_move(&self, proposal: &ProposedRelation) -> Option<Option<usize>> {
        self.move_relations
            .get(&ProposalKey::from(proposal))
            .copied()
    }

    fn assess_move_cached(&mut self, proposal: &ProposedRelation) -> Result<Option<usize>> {
        let key = ProposalKey::from(proposal);
        if let Some(&result) = self.move_relations.get(&key) {
            return Ok(result);
        }
        let result = if self.semantic_rejected(proposal) {
            None
        } else {
            self.assess_move(proposal)?
        };
        self.move_relations.insert(key, result);
        Ok(result)
    }

    fn assess_cached(&mut self, proposal: &ProposedRelation) -> Result<usize> {
        let key = ProposalKey::from(proposal);
        if let Some(&index) = self.proposal_relations.get(&key) {
            return Ok(index);
        }
        let index = self.assess(proposal)?;
        self.proposal_relations.insert(key, index);
        Ok(index)
    }

    fn reassess(&mut self, proposal: &ProposedRelation) -> Result<usize> {
        let key = ProposalKey::from(proposal);
        let index = self.assess(proposal)?;
        self.proposal_relations.insert(key, index);
        Ok(index)
    }

    fn new(
        sides: [&'a Side<'document>; 2],
        alignment: &'a Alignment,
        recovery: Option<SentenceRecoveryInput<'a>>,
        options: DiffOptions,
    ) -> Result<Self> {
        let mut assessor = Self {
            sides,
            alignment,
            recovery,
            options,
            remaining_work: options.max_assessment_work,
            anchors: Vec::new(),
            domains: HashMap::new(),
            records: Vec::new(),
            output_stop: None,
            root_relation: None,
            root_reasons: Vec::new(),
            semantic_acceptance: HashMap::new(),
            local_domains: Vec::new(),
            local_anchors: Vec::new(),
            localized_edits: Vec::new(),
            localized_edit_count: 0,
            proposal_relations: HashMap::new(),
            semantic_rejections: HashSet::new(),
            move_relations: HashMap::new(),
        };
        assessor.root_reasons = assessor.inspect_source_reasons();
        assessor.anchors = assessor.verified_anchors()?;
        Ok(assessor)
    }

    fn root_relation(&mut self) -> Result<usize> {
        if let Some(index) = self.root_relation {
            return Ok(index);
        }
        let old = domain_group(
            self.sides[0],
            0..self.sides[0].blocks.len(),
            BlockSeparator::Space,
        );
        let new = domain_group(
            self.sides[1],
            0..self.sides[1].blocks.len(),
            BlockSeparator::Space,
        );
        let reasons = self.source_reasons();
        let index = self.record(RelationAssessment {
            old_span: nonempty_span(&old),
            new_span: nonempty_span(&new),
            parent: None,
            outcome: if reasons.is_empty() {
                RelationOutcome::Established
            } else {
                RelationOutcome::Tentative
            },
            search: SearchCompleteness::Complete,
            assumptions: assumptions([&old, &new]),
            reasons,
        })?;
        self.root_relation = Some(index);
        Ok(index)
    }

    fn charge(&mut self, work: usize) -> bool {
        match self.remaining_work.checked_sub(work) {
            Some(remaining) => {
                self.remaining_work = remaining;
                true
            }
            None => {
                self.remaining_work = 0;
                false
            }
        }
    }

    fn record(&mut self, record: RelationAssessment) -> Result<usize> {
        if let Some(index) = self.output_stop {
            return Ok(index);
        }
        if self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1) {
            let old = domain_group(
                self.sides[0],
                0..self.sides[0].blocks.len(),
                BlockSeparator::Space,
            );
            let new = domain_group(
                self.sides[1],
                0..self.sides[1].blocks.len(),
                BlockSeparator::Space,
            );
            let index = self.records.len();
            reserve_ranges(&mut self.records, 1, self.options.max_assessment_ranges)?;
            self.records.push(RelationAssessment {
                old_span: nonempty_span(&old),
                new_span: nonempty_span(&new),
                parent: None,
                outcome: RelationOutcome::Tentative,
                search: SearchCompleteness::Incomplete,
                assumptions: Vec::new(),
                reasons: vec![AssessmentReason::OutputLimit],
            });
            self.output_stop = Some(index);
            return Ok(index);
        }
        reserve_ranges(&mut self.records, 1, self.options.max_assessment_ranges)?;
        let index = self.records.len();
        self.records.push(record);
        Ok(index)
    }

    fn verified_anchors(&mut self) -> Result<Vec<(usize, usize)>> {
        let [old, new] = self.sides;
        let mut counts = [
            HashMap::<&[ComparableToken], Vec<usize>>::new(),
            HashMap::new(),
        ];
        for (side, keys) in self.sides.into_iter().zip(&mut counts) {
            for (index, tokens) in side.canonical.iter().enumerate() {
                if !self.charge(tokens.len().saturating_add(1)) {
                    return Ok(Vec::new());
                }
                if !tokens.is_empty() {
                    keys.entry(tokens).or_default().push(index);
                }
            }
        }
        let mut anchors = Vec::new();
        for (old_index, tokens) in old.canonical.iter().enumerate() {
            if let (Some(old_positions), Some(new_positions)) = (
                counts[0].get(tokens.as_slice()),
                counts[1].get(tokens.as_slice()),
            ) && old_positions.len() == 1
                && new_positions.len() == 1
            {
                let new_index = new_positions[0];
                if old.blocks[old_index].issues.is_empty()
                    && new.blocks[new_index].issues.is_empty()
                    && old.blocks[old_index]
                        .role
                        .is_alignment_compatible(new.blocks[new_index].role)
                {
                    anchors.push((old_index, new_index));
                }
            }
        }
        // Whole-block identity is only a candidate boundary: an equal source
        // sequence spanning removable block boundaries also competes with it.
        for side in self.sides {
            for separator in [BlockSeparator::Space, BlockSeparator::Concatenate] {
                if !self.charge(side.total_tokens.saturating_add(side.blocks.len())) {
                    return Ok(Vec::new());
                }
                let group = domain_group(side, 0..side.blocks.len(), separator);
                let mut postings = HashMap::<&ComparableToken, Vec<usize>>::new();
                for (position, token) in group.tokens.iter().enumerate() {
                    postings.entry(token).or_default().push(position);
                }
                let mut verified = Vec::new();
                for anchor in anchors {
                    let needle = &old.canonical[anchor.0];
                    let positions = postings
                        .get(&needle[0])
                        .map(Vec::as_slice)
                        .unwrap_or_default();
                    let mut count = 0;
                    for &start in positions {
                        let candidate = group
                            .tokens
                            .get(start..start.saturating_add(needle.len()))
                            .unwrap_or_default();
                        match tokens_equal_with_budget(candidate, needle, &mut self.remaining_work)
                        {
                            None => return Ok(Vec::new()),
                            Some(false) => {}
                            Some(true) => {
                                count += 1;
                                if count == 2 {
                                    break;
                                }
                            }
                        }
                    }
                    if count == 1 {
                        verified.push(anchor);
                    }
                }
                anchors = verified;
            }
        }
        let old_order = anchors.iter().map(|&(old, _)| old).collect::<Vec<_>>();
        let mut new_order = anchors.clone();
        new_order.sort_unstable_by_key(|&(_, new)| new);
        let new_order = new_order.iter().map(|&(old, _)| old).collect::<Vec<_>>();
        if exact::check(&old_order, &new_order, &mut self.remaining_work)?
            != exact::ExactUniqueness::Unique
        {
            return Ok(Vec::new());
        }
        // The exact check establishes a unique increasing spine. Reconstruct
        // that spine without choosing an arbitrary equal-length alternative.
        let mut tails = Vec::<usize>::new();
        let mut previous = vec![None; anchors.len()];
        for index in 0..anchors.len() {
            let position = tails.partition_point(|&tail| anchors[tail].1 < anchors[index].1);
            if position > 0 {
                previous[index] = Some(tails[position - 1]);
            }
            if position == tails.len() {
                tails.push(index);
            } else {
                tails[position] = index;
            }
        }
        let mut result = Vec::new();
        let mut cursor = tails.last().copied();
        while let Some(index) = cursor {
            result.push(anchors[index]);
            cursor = previous[index];
        }
        result.reverse();
        Ok(result)
    }
}
