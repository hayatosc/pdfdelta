use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};

#[path = "revisions/assessment_diagnostics.rs"]
mod assessment_diagnostics;
pub use assessment_diagnostics::FinalAssessmentDiagnostics;

use pdfdelta_core::{
    alignment::{
        Alignment, AlignmentEvidence, AlignmentKind, BlockFeatures, BlockSeparator,
        CandidateGenerator, InvertedIndexCandidateGenerator, build_block_features,
    },
    diff::{ChangeKind, Comparison, TextSpan},
    layout::BlockId,
    normalize::{BlockText, ComparableToken, ScalarRange},
    pipeline::PipelineOptions,
};

use super::{
    ActualChange, ActualChangeOccurrence, ActualRelationTraceStatus, CandidateRecallMetrics,
    ChangeOriginReport, ExpectedChange, ExpectedChangeDiagnostics, ExpectedChangeFailure,
    ExpectedChangeFailureReason, ExpectedChangeMatchEvaluation, ExpectedChangedRange, ExpectedKind,
    MAX_EXPECTED_CHANGE_DIAGNOSTICS, MatchOutcome, MissSide, WrongChangeKindDiagnostic,
    WrongChangeKindDiagnosticStopReason, WrongChangeKindSemanticHunkReport,
    WrongChangeKindTraceReport, build_block_map, change_kind_name, collapse_whitespace,
    is_space_token, normalized_expected_quotes, occurrence_matches_expected_kind, ratio,
};

#[derive(Clone, Copy)]
pub(super) struct DiagnosticLimits {
    max_quote_chars: usize,
    max_scan_work: usize,
    max_occurrences: usize,
    max_cross_states: usize,
    max_candidate_visits: usize,
    max_regions: usize,
    max_hunks: usize,
    max_expected_changes: usize,
    max_output_records: usize,
}

impl Default for DiagnosticLimits {
    fn default() -> Self {
        Self {
            max_quote_chars: 4_096,
            max_scan_work: 64_000_000,
            max_occurrences: 256,
            max_cross_states: 256,
            max_candidate_visits: PipelineOptions::default().alignment.max_candidate_visits,
            max_regions: 65_536,
            max_hunks: 65_536,
            max_expected_changes: MAX_EXPECTED_CHANGE_DIAGNOSTICS,
            max_output_records: 4_096,
        }
    }
}

#[cfg(test)]
impl DiagnosticLimits {
    pub(super) fn with_max_scan_work(mut self, max_scan_work: usize) -> Self {
        self.max_scan_work = max_scan_work;
        self
    }
}

#[derive(Default)]
pub(super) struct DiagnosticBudget {
    scan_work: usize,
    occurrences: usize,
    candidate_visits: usize,
    candidate_visits_limited: bool,
    regions: usize,
    hunks: usize,
    output_records: usize,
    limited: bool,
}

#[derive(Debug)]
enum DiagnosticScanError {
    Limited,
    Invalid(String),
}

type DiagnosticScanResult<T> = std::result::Result<T, DiagnosticScanError>;

impl DiagnosticBudget {
    fn charge_scan(&mut self, amount: usize, limits: DiagnosticLimits) -> DiagnosticScanResult<()> {
        Self::charge(
            &mut self.scan_work,
            amount,
            limits.max_scan_work,
            &mut self.limited,
        )
    }

    fn charge_occurrence(&mut self, limits: DiagnosticLimits) -> DiagnosticScanResult<()> {
        Self::charge(
            &mut self.occurrences,
            1,
            limits.max_occurrences,
            &mut self.limited,
        )
    }

    fn charge_candidate_visits(&mut self, amount: usize, limit: usize) -> DiagnosticScanResult<()> {
        if self.candidate_visits_limited {
            return Err(DiagnosticScanError::Limited);
        }
        match self.candidate_visits.checked_add(amount) {
            Some(next) if next <= limit => {
                self.candidate_visits = next;
                Ok(())
            }
            _ => {
                self.candidate_visits_limited = true;
                self.limited = true;
                Err(DiagnosticScanError::Limited)
            }
        }
    }

    fn charge_region(&mut self, limits: DiagnosticLimits) -> DiagnosticScanResult<()> {
        Self::charge(&mut self.regions, 1, limits.max_regions, &mut self.limited)
    }

    pub(super) fn charge_scope_scan(&mut self, limits: DiagnosticLimits) -> bool {
        self.charge_scan(1, limits).is_ok()
    }

    pub(super) fn charge_scope(&mut self, limits: DiagnosticLimits) -> bool {
        self.charge_region(limits).is_ok()
    }

    fn charge_hunk(&mut self, limits: DiagnosticLimits) -> DiagnosticScanResult<()> {
        Self::charge(&mut self.hunks, 1, limits.max_hunks, &mut self.limited)
    }

    fn charge_output(&mut self, limits: DiagnosticLimits) -> DiagnosticScanResult<()> {
        Self::charge(
            &mut self.output_records,
            1,
            limits.max_output_records,
            &mut self.limited,
        )
    }

    fn checked_add(&mut self, left: usize, right: usize) -> DiagnosticScanResult<usize> {
        left.checked_add(right).ok_or_else(|| {
            self.limited = true;
            DiagnosticScanError::Limited
        })
    }

    fn charge(
        counter: &mut usize,
        amount: usize,
        limit: usize,
        limited: &mut bool,
    ) -> DiagnosticScanResult<()> {
        match counter.checked_add(amount) {
            Some(next) if next <= limit => {
                *counter = next;
                Ok(())
            }
            _ => {
                *limited = true;
                Err(DiagnosticScanError::Limited)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct QuoteLocation {
    pub(super) block: BlockId,
    pub(super) scalar_range: ScalarRange,
}

pub(super) struct ScopedQuoteRange {
    pub(super) start_block: usize,
    pub(super) start_scalar: usize,
    pub(super) end_block: usize,
    pub(super) end_scalar: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScopedQuoteLocation {
    pub(super) start_block: usize,
    pub(super) start_scalar: usize,
    pub(super) end_block: usize,
    pub(super) end_scalar: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ScopedQuoteLocateOutcome {
    Unique(ScopedQuoteLocation),
    Missing,
    Ambiguous,
    Indeterminate,
    Limited,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum QuoteLocateOutcome {
    Unique(QuoteLocation),
    Missing,
    Segmented,
    Ambiguous,
    Indeterminate,
    Limited,
}

#[derive(Clone)]
struct ExpectedQuoteLocations {
    old: Option<QuoteLocateOutcome>,
    new: Option<QuoteLocateOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewedCandidateResult {
    NotApplicable,
    Unavailable,
    Limited,
    Recalled,
    Missed,
}

enum CandidateQuery {
    Available(Vec<BlockId>),
    Limited,
}

pub(super) struct ReviewedDiagnostics {
    pub(super) candidate_recall: Option<CandidateRecallMetrics>,
    pub(super) expected_change_diagnostics: ExpectedChangeDiagnostics,
}

#[derive(Clone)]
enum LocatedItem {
    Scalar { value: char, range: ScalarRange },
    Barrier,
}

fn normalize_quote(
    quote: &str,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Vec<char>> {
    let mut normalized = Vec::new();
    let mut pending_space = false;
    for scalar in quote.chars() {
        budget.charge_scan(1, limits)?;
        if scalar.is_whitespace() {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            normalized.push(' ');
        }
        normalized.push(scalar);
        pending_space = false;
        if normalized.len() > limits.max_quote_chars {
            budget.limited = true;
            return Err(DiagnosticScanError::Limited);
        }
    }
    Ok(normalized)
}

fn normalize_block_items(
    block: &BlockText,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Vec<LocatedItem>> {
    normalize_block_items_in_range(block, None, budget, limits)
}

fn normalize_block_items_in_range(
    block: &BlockText,
    allowed: Option<ScalarRange>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Vec<LocatedItem>> {
    let tokens = block
        .canonical
        .comparable_tokens()
        .map_err(|error| DiagnosticScanError::Invalid(error.to_string()))?;
    let mut items = Vec::new();
    let mut scalar_index = 0_usize;
    let mut segment_has_text = false;
    let mut pending_space = None::<ScalarRange>;
    for token in tokens {
        budget.charge_scan(1, limits)?;
        match token {
            ComparableToken::Scalar(value) => {
                let end = scalar_index.checked_add(1).ok_or_else(|| {
                    budget.limited = true;
                    DiagnosticScanError::Limited
                })?;
                let range = ScalarRange {
                    start: scalar_index,
                    end,
                };
                scalar_index = end;
                if allowed
                    .is_some_and(|allowed| range.start < allowed.start || range.end > allowed.end)
                {
                    continue;
                }
                if value.is_whitespace() {
                    if segment_has_text {
                        pending_space = Some(match pending_space {
                            Some(pending) => ScalarRange {
                                start: pending.start,
                                end,
                            },
                            None => range,
                        });
                    }
                    continue;
                }
                if let Some(range) = pending_space.take() {
                    items.push(LocatedItem::Scalar { value: ' ', range });
                }
                items.push(LocatedItem::Scalar { value, range });
                segment_has_text = true;
            }
            ComparableToken::Unmapped { .. } => {
                if allowed.is_some_and(|allowed| {
                    scalar_index < allowed.start || scalar_index > allowed.end
                }) {
                    continue;
                }
                pending_space = None;
                segment_has_text = false;
                if !matches!(items.last(), Some(LocatedItem::Barrier)) {
                    items.push(LocatedItem::Barrier);
                }
            }
        }
    }
    if allowed.is_some_and(|allowed| allowed.start > allowed.end || allowed.end > scalar_index) {
        return Err(DiagnosticScanError::Invalid(
            "scoped quote range exceeds canonical block text".to_owned(),
        ));
    }
    Ok(items)
}

fn kmp_prefix(
    needle: &[char],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Vec<usize>> {
    let mut prefix = vec![0; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        while matched > 0 {
            budget.charge_scan(1, limits)?;
            if needle[index] == needle[matched] {
                break;
            }
            matched = prefix[matched - 1];
        }
        if matched == 0 {
            budget.charge_scan(1, limits)?;
            if needle[index] != needle[0] {
                continue;
            }
        }
        matched += 1;
        prefix[index] = matched;
    }
    Ok(prefix)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ScalarPosition {
    block: usize,
    item: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MatchOrigin {
    Scalar(ScalarPosition),
    VirtualSpace,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct MatcherState {
    matched: usize,
    origins: VecDeque<MatchOrigin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RawOccurrence {
    start: ScalarPosition,
    end: ScalarPosition,
    cross_block: bool,
}

fn retain_origin_suffix(
    state: &mut MatcherState,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<()> {
    let discard = state
        .origins
        .len()
        .checked_sub(state.matched)
        .ok_or_else(|| {
            budget.limited = true;
            DiagnosticScanError::Limited
        })?;
    budget.charge_scan(discard, limits)?;
    for _ in 0..discard {
        state.origins.pop_front();
    }
    Ok(())
}

fn advance_matcher(
    state: &mut MatcherState,
    value: char,
    origin: MatchOrigin,
    needle: &[char],
    prefix: &[usize],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<RawOccurrence>> {
    let mut matches = false;
    while state.matched > 0 {
        budget.charge_scan(1, limits)?;
        if needle[state.matched] == value {
            matches = true;
            break;
        }
        state.matched = prefix[state.matched - 1];
        retain_origin_suffix(state, budget, limits)?;
    }
    if state.matched == 0 {
        budget.charge_scan(1, limits)?;
        matches = needle[0] == value;
    }
    if !matches {
        return Ok(None);
    }

    state.matched += 1;
    state.origins.push_back(origin);
    if state.matched != needle.len() {
        return Ok(None);
    }

    budget.charge_scan(state.origins.len(), limits)?;
    let start = state.origins.iter().find_map(|origin| match origin {
        MatchOrigin::Scalar(position) => Some(*position),
        MatchOrigin::VirtualSpace => None,
    });
    let end = state.origins.iter().rev().find_map(|origin| match origin {
        MatchOrigin::Scalar(position) => Some(*position),
        MatchOrigin::VirtualSpace => None,
    });
    let (Some(start), Some(end)) = (start, end) else {
        return Err(DiagnosticScanError::Invalid(
            "quote match contains no extracted scalar".to_owned(),
        ));
    };
    let occurrence = RawOccurrence {
        start,
        end,
        cross_block: start.block != end.block || state.origins.contains(&MatchOrigin::VirtualSpace),
    };
    state.matched = prefix[needle.len() - 1];
    retain_origin_suffix(state, budget, limits)?;
    Ok(Some(occurrence))
}

fn deduplicate_states(
    states: Vec<MatcherState>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Vec<MatcherState>> {
    if states.len() <= 1 {
        return Ok(states);
    }
    let mut seen = HashSet::with_capacity(states.len());
    let mut unique = Vec::with_capacity(states.len());
    for state in states {
        let hash_work = budget.checked_add(state.origins.len(), 1)?;
        budget.charge_scan(hash_work, limits)?;
        budget.charge_scan(state.origins.len(), limits)?;
        if seen.insert(state.clone()) {
            unique.push(state);
        }
    }
    if unique.len() > limits.max_cross_states {
        budget.limited = true;
        return Err(DiagnosticScanError::Limited);
    }
    Ok(unique)
}

fn occurrence_outcome(
    occurrence: RawOccurrence,
    blocks: &[BlockText],
    normalized_blocks: &[Vec<LocatedItem>],
) -> DiagnosticScanResult<QuoteLocateOutcome> {
    if occurrence.cross_block {
        return Ok(QuoteLocateOutcome::Segmented);
    }
    let range = |position: ScalarPosition| match normalized_blocks
        .get(position.block)
        .and_then(|items| items.get(position.item))
    {
        Some(LocatedItem::Scalar { range, .. }) => Ok(*range),
        _ => Err(DiagnosticScanError::Invalid(
            "quote match references a non-scalar position".to_owned(),
        )),
    };
    let start = range(occurrence.start)?;
    let end = range(occurrence.end)?;
    Ok(QuoteLocateOutcome::Unique(QuoteLocation {
        block: blocks[occurrence.start.block].block,
        scalar_range: ScalarRange {
            start: start.start,
            end: end.end,
        },
    }))
}

#[derive(Clone, Copy)]
enum RawQuoteOutcome {
    Unique(RawOccurrence),
    Ambiguous,
}

fn record_raw_occurrence(
    occurrence: RawOccurrence,
    first: &mut Option<RawOccurrence>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<RawQuoteOutcome>> {
    if first.as_ref().is_some_and(|seen| *seen == occurrence) {
        return Ok(None);
    }
    budget.charge_occurrence(limits)?;
    if first.is_some() {
        return Ok(Some(RawQuoteOutcome::Ambiguous));
    }
    *first = Some(occurrence);
    Ok(None)
}

fn scan_quote_raw(
    normalized_blocks: &[Vec<LocatedItem>],
    needle: &[char],
    prefix: &[usize],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<RawQuoteOutcome>> {
    let mut states = vec![MatcherState::default()];
    let mut first = None;
    for (block_index, items) in normalized_blocks.iter().enumerate() {
        if block_index > 0 {
            let temporary_limit = limits.max_cross_states.checked_mul(2).ok_or_else(|| {
                budget.limited = true;
                DiagnosticScanError::Limited
            })?;
            let branched_capacity = states.len().checked_mul(2).ok_or_else(|| {
                budget.limited = true;
                DiagnosticScanError::Limited
            })?;
            if branched_capacity > temporary_limit {
                budget.limited = true;
                return Err(DiagnosticScanError::Limited);
            }
            let mut branched = Vec::with_capacity(branched_capacity);
            for state in states {
                let clone_work = budget.checked_add(state.origins.len(), 1)?;
                budget.charge_scan(clone_work, limits)?;
                let mut spaced = state.clone();
                branched.push(state);
                if let Some(occurrence) = advance_matcher(
                    &mut spaced,
                    ' ',
                    MatchOrigin::VirtualSpace,
                    needle,
                    prefix,
                    budget,
                    limits,
                )? && let Some(outcome) =
                    record_raw_occurrence(occurrence, &mut first, budget, limits)?
                {
                    return Ok(Some(outcome));
                }
                branched.push(spaced);
            }
            states = deduplicate_states(branched, budget, limits)?;
        }

        for (item_index, item) in items.iter().enumerate() {
            let LocatedItem::Scalar { value, .. } = item else {
                states.clear();
                states.push(MatcherState::default());
                continue;
            };
            let mut advanced = Vec::with_capacity(states.len());
            for mut state in states {
                if let Some(occurrence) = advance_matcher(
                    &mut state,
                    *value,
                    MatchOrigin::Scalar(ScalarPosition {
                        block: block_index,
                        item: item_index,
                    }),
                    needle,
                    prefix,
                    budget,
                    limits,
                )? && let Some(outcome) =
                    record_raw_occurrence(occurrence, &mut first, budget, limits)?
                {
                    return Ok(Some(outcome));
                }
                advanced.push(state);
            }
            states = advanced;
        }
    }
    Ok(first.map(RawQuoteOutcome::Unique))
}

fn scan_quote_matches(
    blocks: &[BlockText],
    normalized_blocks: &[Vec<LocatedItem>],
    needle: &[char],
    prefix: &[usize],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<QuoteLocateOutcome>> {
    match scan_quote_raw(normalized_blocks, needle, prefix, budget, limits)? {
        Some(RawQuoteOutcome::Unique(occurrence)) => {
            occurrence_outcome(occurrence, blocks, normalized_blocks).map(Some)
        }
        Some(RawQuoteOutcome::Ambiguous) => Ok(Some(QuoteLocateOutcome::Ambiguous)),
        None => Ok(None),
    }
}

fn scoped_occurrence_location(
    occurrence: RawOccurrence,
    normalized_blocks: &[Vec<LocatedItem>],
    block_offset: usize,
) -> DiagnosticScanResult<ScopedQuoteLocation> {
    let range = |position: ScalarPosition| match normalized_blocks
        .get(position.block)
        .and_then(|items| items.get(position.item))
    {
        Some(LocatedItem::Scalar { range, .. }) => Ok(*range),
        _ => Err(DiagnosticScanError::Invalid(
            "scoped quote match references a non-scalar position".to_owned(),
        )),
    };
    let start = range(occurrence.start)?;
    let end = range(occurrence.end)?;
    Ok(ScopedQuoteLocation {
        start_block: block_offset
            .checked_add(occurrence.start.block)
            .ok_or(DiagnosticScanError::Limited)?,
        start_scalar: start.start,
        end_block: block_offset
            .checked_add(occurrence.end.block)
            .ok_or(DiagnosticScanError::Limited)?,
        end_scalar: end.end,
    })
}

pub(super) fn locate_quote(
    blocks: &[BlockText],
    quote: &str,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> std::result::Result<QuoteLocateOutcome, String> {
    let needle = match normalize_quote(quote, budget, limits) {
        Ok(needle) => needle,
        Err(DiagnosticScanError::Limited) => return Ok(QuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    if needle.is_empty() {
        return Ok(QuoteLocateOutcome::Missing);
    }
    let prefix = match kmp_prefix(&needle, budget, limits) {
        Ok(prefix) => prefix,
        Err(DiagnosticScanError::Limited) => return Ok(QuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let normalized_blocks = match blocks
        .iter()
        .map(|block| normalize_block_items(block, budget, limits))
        .collect::<DiagnosticScanResult<Vec<_>>>()
    {
        Ok(blocks) => blocks,
        Err(DiagnosticScanError::Limited) => return Ok(QuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let occurrence =
        match scan_quote_matches(blocks, &normalized_blocks, &needle, &prefix, budget, limits) {
            Ok(occurrence) => occurrence,
            Err(DiagnosticScanError::Limited) => return Ok(QuoteLocateOutcome::Limited),
            Err(DiagnosticScanError::Invalid(error)) => return Err(error),
        };
    match occurrence {
        Some(outcome) => Ok(outcome),
        None if blocks.iter().any(|block| {
            !block.issues.is_empty()
                || !block.raw.unmapped.is_empty()
                || !block.canonical.unmapped.is_empty()
        }) =>
        {
            Ok(QuoteLocateOutcome::Indeterminate)
        }
        None => Ok(QuoteLocateOutcome::Missing),
    }
}

pub(super) fn locate_scope_anchor_quote(
    blocks: &[BlockText],
    quote: &str,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> std::result::Result<ScopedQuoteLocateOutcome, String> {
    let needle = match normalize_quote(quote, budget, limits) {
        Ok(needle) => needle,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    if needle.is_empty() {
        return Ok(ScopedQuoteLocateOutcome::Missing);
    }
    let prefix = match kmp_prefix(&needle, budget, limits) {
        Ok(prefix) => prefix,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let normalized_blocks = match blocks
        .iter()
        .map(|block| normalize_block_items(block, budget, limits))
        .collect::<DiagnosticScanResult<Vec<_>>>()
    {
        Ok(blocks) => blocks,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let occurrence = match scan_quote_raw(&normalized_blocks, &needle, &prefix, budget, limits) {
        Ok(Some(RawQuoteOutcome::Unique(occurrence))) => occurrence,
        Ok(Some(RawQuoteOutcome::Ambiguous)) => return Ok(ScopedQuoteLocateOutcome::Ambiguous),
        Ok(None)
            if blocks.iter().any(|block| {
                !block.issues.is_empty()
                    || !block.raw.unmapped.is_empty()
                    || !block.canonical.unmapped.is_empty()
            }) =>
        {
            return Ok(ScopedQuoteLocateOutcome::Indeterminate);
        }
        Ok(None) => return Ok(ScopedQuoteLocateOutcome::Missing),
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let location = scoped_occurrence_location(occurrence, &normalized_blocks, 0).map_err(
        |error| match error {
            DiagnosticScanError::Limited => "scope anchor coordinates overflow".to_owned(),
            DiagnosticScanError::Invalid(error) => error,
        },
    )?;
    let Some(matched_blocks) = blocks.get(location.start_block..=location.end_block) else {
        return Ok(ScopedQuoteLocateOutcome::Indeterminate);
    };
    for block in matched_blocks {
        if budget.charge_scan(1, limits).is_err() {
            return Ok(ScopedQuoteLocateOutcome::Limited);
        }
        if !block.issues.is_empty()
            || !block.raw.unmapped.is_empty()
            || !block.canonical.unmapped.is_empty()
        {
            return Ok(ScopedQuoteLocateOutcome::Indeterminate);
        }
    }
    Ok(ScopedQuoteLocateOutcome::Unique(location))
}

pub(super) fn locate_scoped_quote(
    blocks: &[BlockText],
    quote: &str,
    range: ScopedQuoteRange,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> std::result::Result<ScopedQuoteLocateOutcome, String> {
    let ScopedQuoteRange {
        start_block,
        start_scalar,
        end_block,
        end_scalar,
    } = range;
    if start_block > end_block || end_block >= blocks.len() {
        return Ok(ScopedQuoteLocateOutcome::Indeterminate);
    }
    let needle = match normalize_quote(quote, budget, limits) {
        Ok(needle) => needle,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    if needle.is_empty() {
        return Ok(ScopedQuoteLocateOutcome::Missing);
    }
    let prefix = match kmp_prefix(&needle, budget, limits) {
        Ok(prefix) => prefix,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let scope_blocks = &blocks[start_block..=end_block];
    let normalized_blocks = match scope_blocks
        .iter()
        .enumerate()
        .map(|(offset, block)| {
            let block_index = start_block.checked_add(offset).ok_or_else(|| {
                DiagnosticScanError::Invalid("scoped quote block coordinate overflow".to_owned())
            })?;
            let start = if block_index == start_block {
                start_scalar
            } else {
                0
            };
            let end = if block_index == end_block {
                end_scalar.checked_add(1).ok_or_else(|| {
                    DiagnosticScanError::Invalid("scoped quote end coordinate overflow".to_owned())
                })?
            } else {
                let scalar_count = block.canonical.text.chars().count();
                budget.charge_scan(scalar_count, limits)?;
                scalar_count
            };
            normalize_block_items_in_range(block, Some(ScalarRange { start, end }), budget, limits)
        })
        .collect::<DiagnosticScanResult<Vec<_>>>()
    {
        Ok(blocks) => blocks,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    let outcome = match scan_quote_raw(&normalized_blocks, &needle, &prefix, budget, limits) {
        Ok(Some(RawQuoteOutcome::Unique(occurrence))) => ScopedQuoteLocateOutcome::Unique(
            scoped_occurrence_location(occurrence, &normalized_blocks, start_block).map_err(
                |error| match error {
                    DiagnosticScanError::Limited => "scoped quote coordinates overflow".to_owned(),
                    DiagnosticScanError::Invalid(error) => error,
                },
            )?,
        ),
        Ok(Some(RawQuoteOutcome::Ambiguous)) => ScopedQuoteLocateOutcome::Ambiguous,
        Ok(None) => ScopedQuoteLocateOutcome::Missing,
        Err(DiagnosticScanError::Limited) => return Ok(ScopedQuoteLocateOutcome::Limited),
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    };
    for block in scope_blocks {
        if budget.charge_scan(1, limits).is_err() {
            return Ok(ScopedQuoteLocateOutcome::Limited);
        }
        if !block.issues.is_empty()
            || !block.raw.unmapped.is_empty()
            || !block.canonical.unmapped.is_empty()
        {
            return Ok(ScopedQuoteLocateOutcome::Indeterminate);
        }
    }
    Ok(outcome)
}

fn checked_increment(counter: &mut usize, name: &str) -> std::result::Result<(), String> {
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| format!("{name} overflow"))?;
    Ok(())
}

fn location(outcome: &Option<QuoteLocateOutcome>) -> Option<&QuoteLocation> {
    match outcome {
        Some(QuoteLocateOutcome::Unique(location)) => Some(location),
        _ => None,
    }
}

fn side_for_status(
    locations: &ExpectedQuoteLocations,
    predicate: impl Fn(&QuoteLocateOutcome) -> bool,
) -> Option<MissSide> {
    match (
        locations.old.as_ref().is_some_and(&predicate),
        locations.new.as_ref().is_some_and(predicate),
    ) {
        (true, true) => Some(MissSide::Both),
        (true, false) => Some(MissSide::Old),
        (false, true) => Some(MissSide::New),
        (false, false) => None,
    }
}

fn span_contains_location(
    span: &TextSpan,
    location: &QuoteLocation,
    blocks_by_id: &HashMap<u64, &BlockText>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<bool> {
    Ok(
        span_location_range(span, location, blocks_by_id, budget, limits)?.is_some_and(|range| {
            span.canonical_range.start <= range.start && range.end <= span.canonical_range.end
        }),
    )
}

fn span_location_range(
    span: &TextSpan,
    location: &QuoteLocation,
    blocks_by_id: &HashMap<u64, &BlockText>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<ScalarRange>> {
    let mut scalar_offset = 0_usize;
    let mut previous_last = None::<ComparableToken>;
    for (position, block_id) in span.blocks.iter().enumerate() {
        budget.charge_scan(1, limits)?;
        let block = blocks_by_id.get(&block_id.0).ok_or_else(|| {
            DiagnosticScanError::Invalid(format!(
                "diagnostic span references missing block {}",
                block_id.0
            ))
        })?;
        let tokens = block
            .canonical
            .comparable_tokens()
            .map_err(|error| DiagnosticScanError::Invalid(error.to_string()))?;
        budget.charge_scan(tokens.len(), limits)?;
        if position > 0
            && span
                .separator
                .is_some_and(|separator| separator.at(position - 1) == BlockSeparator::Space)
            && !previous_last.as_ref().is_some_and(is_space_token)
            && !tokens.first().is_some_and(is_space_token)
        {
            scalar_offset = budget.checked_add(scalar_offset, 1)?;
        }
        if *block_id == location.block {
            let start = budget.checked_add(scalar_offset, location.scalar_range.start)?;
            let end = budget.checked_add(scalar_offset, location.scalar_range.end)?;
            return Ok(Some(ScalarRange { start, end }));
        }
        let scalar_count = tokens.iter().filter(|token| token.is_scalar()).count();
        scalar_offset = budget.checked_add(scalar_offset, scalar_count)?;
        previous_last = tokens.last().cloned();
    }
    Ok(None)
}

const OLD_SIDE_MASK: u8 = 1;
const NEW_SIDE_MASK: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum EvidenceCause {
    CandidateNotGenerated,
    CandidateScoringRejected,
    AlignmentAmbiguous,
    ReadingOrderUnresolved,
    DiffEditDistanceExceeded,
    DiffRejectedAsImplausible,
}

fn required_location_mask(locations: &ExpectedQuoteLocations) -> Option<u8> {
    let mut mask = 0;
    for (outcome, side) in [
        (&locations.old, OLD_SIDE_MASK),
        (&locations.new, NEW_SIDE_MASK),
    ] {
        match outcome {
            Some(QuoteLocateOutcome::Unique(_)) => mask |= side,
            Some(_) => return None,
            None => {}
        }
    }
    (mask != 0).then_some(mask)
}

fn region_location_mask(
    region: &pdfdelta_core::diff::UnresolvedRegion,
    locations: &ExpectedQuoteLocations,
    blocks_by_side: [&HashMap<u64, &BlockText>; 2],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<u8> {
    let mut mask = 0;
    for (span, quote, blocks, side) in [
        (
            region.old_span.as_ref(),
            location(&locations.old),
            blocks_by_side[0],
            OLD_SIDE_MASK,
        ),
        (
            region.new_span.as_ref(),
            location(&locations.new),
            blocks_by_side[1],
            NEW_SIDE_MASK,
        ),
    ] {
        if let (Some(span), Some(quote)) = (span, quote)
            && span_contains_location(span, quote, blocks, budget, limits)?
        {
            mask |= side;
        }
    }
    Ok(mask)
}

fn unique_cause(
    items: impl Iterator<Item = EvidenceCause>,
) -> std::result::Result<Option<EvidenceCause>, ()> {
    let mut causes = HashSet::new();
    for cause in items {
        if !causes.insert(cause) {
            return Err(());
        }
    }
    match causes.len() {
        0 => Ok(None),
        1 => Ok(causes.into_iter().next()),
        _ => Err(()),
    }
}

fn evidence_cause(
    evidence: &[AlignmentEvidence],
) -> std::result::Result<Option<EvidenceCause>, ()> {
    let diff = unique_cause(evidence.iter().filter_map(|item| match item {
        AlignmentEvidence::DiffEditDistanceExceeded => {
            Some(EvidenceCause::DiffEditDistanceExceeded)
        }
        AlignmentEvidence::DiffRejectedAsImplausible => {
            Some(EvidenceCause::DiffRejectedAsImplausible)
        }
        _ => None,
    }))?;
    if diff.is_some() {
        return Ok(diff);
    }
    match evidence
        .iter()
        .filter(|item| **item == AlignmentEvidence::ReadingOrderUnknown)
        .count()
    {
        0 => {}
        1 => return Ok(Some(EvidenceCause::ReadingOrderUnresolved)),
        _ => return Err(()),
    }
    unique_cause(evidence.iter().filter_map(|item| match item {
        AlignmentEvidence::CandidateSetEmpty => Some(EvidenceCause::CandidateNotGenerated),
        AlignmentEvidence::CandidateScoringRejected => {
            Some(EvidenceCause::CandidateScoringRejected)
        }
        AlignmentEvidence::CandidateCompetition => Some(EvidenceCause::AlignmentAmbiguous),
        _ => None,
    }))
}

fn side_from_mask(mask: u8) -> MissSide {
    match mask {
        OLD_SIDE_MASK => MissSide::Old,
        NEW_SIDE_MASK => MissSide::New,
        _ => MissSide::Both,
    }
}

fn quote_failure_reason(locations: &ExpectedQuoteLocations) -> ExpectedChangeFailureReason {
    let mut missing = 0;
    let mut segmented = 0;
    let mut uncertain = false;
    for (outcome, side) in [
        (&locations.old, OLD_SIDE_MASK),
        (&locations.new, NEW_SIDE_MASK),
    ] {
        match outcome {
            Some(QuoteLocateOutcome::Missing) => missing |= side,
            Some(QuoteLocateOutcome::Segmented) => segmented |= side,
            Some(QuoteLocateOutcome::Ambiguous | QuoteLocateOutcome::Indeterminate) => {
                uncertain = true;
            }
            _ => {}
        }
    }
    if uncertain || missing != 0 && segmented != 0 {
        return ExpectedChangeFailureReason::AlignmentOrCandidate {
            diagnostic_limited: false,
        };
    }
    if segmented != 0 {
        return ExpectedChangeFailureReason::UnitSegmentationFailure {
            side: side_from_mask(segmented),
        };
    }
    if missing != 0 {
        return ExpectedChangeFailureReason::QuoteNotExtracted {
            side: side_from_mask(missing),
        };
    }
    ExpectedChangeFailureReason::AlignmentOrCandidate {
        diagnostic_limited: false,
    }
}

fn cause_failure_reason(cause: EvidenceCause, side_mask: u8) -> ExpectedChangeFailureReason {
    match cause {
        EvidenceCause::CandidateNotGenerated => ExpectedChangeFailureReason::CandidateNotGenerated,
        EvidenceCause::CandidateScoringRejected => {
            ExpectedChangeFailureReason::CandidateScoringRejected
        }
        EvidenceCause::AlignmentAmbiguous => ExpectedChangeFailureReason::AlignmentAmbiguous,
        EvidenceCause::ReadingOrderUnresolved => {
            ExpectedChangeFailureReason::ReadingOrderUnresolved {
                side: side_from_mask(side_mask),
            }
        }
        EvidenceCause::DiffEditDistanceExceeded => {
            ExpectedChangeFailureReason::DiffEditDistanceExceeded
        }
        EvidenceCause::DiffRejectedAsImplausible => {
            ExpectedChangeFailureReason::DiffRejectedAsImplausible
        }
    }
}

fn unresolved_failure_reason(
    comparison: &Comparison,
    locations: &ExpectedQuoteLocations,
    blocks_by_side: [&HashMap<u64, &BlockText>; 2],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<ExpectedChangeFailureReason>> {
    let Some(required_mask) = required_location_mask(locations) else {
        return Ok(None);
    };
    let mut specific_cause = None;
    let mut covered_sides = 0;
    let mut conflicting = false;
    for region in &comparison.unresolved_regions {
        budget.charge_region(limits)?;
        let region_mask = region_location_mask(region, locations, blocks_by_side, budget, limits)?
            & required_mask;
        if region_mask == 0 {
            continue;
        }
        match evidence_cause(&region.evidence) {
            Ok(Some(cause)) => {
                if covered_sides & region_mask != 0 {
                    conflicting = true;
                }
                covered_sides |= region_mask;
                if region_mask == required_mask && specific_cause.replace(cause).is_some() {
                    conflicting = true;
                }
            }
            Ok(None) => {}
            Err(()) => conflicting = true,
        }
    }
    if conflicting {
        return Ok(Some(ExpectedChangeFailureReason::AlignmentOrCandidate {
            diagnostic_limited: false,
        }));
    }
    Ok(specific_cause.map(|cause| cause_failure_reason(cause, required_mask)))
}

fn hunk_indices_inside_quote(
    comparison: &Comparison,
    location: &QuoteLocation,
    old_side: bool,
    claimed_actuals: &HashSet<usize>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<Vec<usize>>> {
    let mut pieces = Vec::new();
    for (index, change) in comparison.changes.iter().enumerate() {
        budget.charge_hunk(limits)?;
        for occurrence in &change.occurrences {
            let span = if old_side {
                occurrence.old_span.as_ref()
            } else {
                occurrence.new_span.as_ref()
            };
            let Some(span) = span else {
                continue;
            };
            if span.blocks != [location.block] {
                if span.blocks.contains(&location.block) {
                    return Ok(None);
                }
                continue;
            }
            let overlaps = span.canonical_range.start < location.scalar_range.end
                && location.scalar_range.start < span.canonical_range.end;
            if !overlaps {
                continue;
            }
            if claimed_actuals.contains(&index)
                || span.canonical_range.start < location.scalar_range.start
                || span.canonical_range.end > location.scalar_range.end
                || span.canonical_range.start >= span.canonical_range.end
            {
                return Ok(None);
            }
            pieces.push((index, span.canonical_range));
        }
    }
    pieces.sort_unstable_by_key(|(index, range)| (range.start, range.end, *index));
    if pieces.len() < 2
        || pieces
            .first()
            .is_none_or(|(_, range)| range.start != location.scalar_range.start)
        || pieces
            .last()
            .is_none_or(|(_, range)| range.end != location.scalar_range.end)
        || pieces
            .windows(2)
            .any(|pair| pair[0].1.end != pair[1].1.start)
    {
        return Ok(None);
    }
    Ok(Some(pieces.into_iter().map(|(index, _)| index).collect()))
}

fn fragmentation_reason(
    change: &ExpectedChange,
    comparison: &Comparison,
    locations: &ExpectedQuoteLocations,
    claimed_actuals: &HashSet<usize>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<ExpectedChangeFailureReason>> {
    let hunk_set = |mut indices: Vec<usize>| {
        indices.sort_unstable();
        indices.dedup();
        indices
    };
    let old_indices = match location(&locations.old) {
        Some(location) => {
            hunk_indices_inside_quote(comparison, location, true, claimed_actuals, budget, limits)?
                .map(hunk_set)
        }
        None => None,
    };
    let new_indices = match location(&locations.new) {
        Some(location) => {
            hunk_indices_inside_quote(comparison, location, false, claimed_actuals, budget, limits)?
                .map(hunk_set)
        }
        None => None,
    };
    let counts = match change.kind {
        ExpectedKind::Replacement | ExpectedKind::Move
            if old_indices.is_some() && old_indices == new_indices =>
        {
            (
                old_indices.as_ref().map_or(0, Vec::len),
                new_indices.as_ref().map_or(0, Vec::len),
            )
        }
        ExpectedKind::Deletion if old_indices.is_some() => {
            (old_indices.as_ref().map_or(0, Vec::len), 0)
        }
        ExpectedKind::Insertion if new_indices.is_some() => {
            (0, new_indices.as_ref().map_or(0, Vec::len))
        }
        _ => return Ok(None),
    };
    Ok(Some(ExpectedChangeFailureReason::FragmentedAcrossHunks {
        old_hunks: counts.0,
        new_hunks: counts.1,
    }))
}

struct FailureContext<'a> {
    alignment_index: Option<&'a AlignmentSpanIndex>,
    alignment_index_limited: bool,
    comparison: &'a Comparison,
    blocks_by_side: [&'a HashMap<u64, &'a BlockText>; 2],
    claimed_actuals: &'a HashSet<usize>,
    budget: &'a mut DiagnosticBudget,
    limits: DiagnosticLimits,
}

fn optional_quote_presence(text: Option<&str>, quote: Option<&str>) -> Option<bool> {
    text.zip(quote).map(|(text, quote)| text.contains(quote))
}

fn wrong_kind_scan_work(occurrence: &ActualChangeOccurrence) -> Option<usize> {
    let mut work = 1usize;
    for text in [
        occurrence.old_text.as_deref(),
        occurrence.new_text.as_deref(),
        occurrence.old_relation_context.as_deref(),
        occurrence.new_relation_context.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        work = work.checked_add(text.len())?;
    }
    for hunk in occurrence.semantic_hunks.as_deref().unwrap_or_default() {
        for text in [hunk.old_text.as_deref(), hunk.new_text.as_deref()]
            .into_iter()
            .flatten()
        {
            work = work.checked_add(text.len())?;
        }
    }
    Some(work)
}

fn wrong_kind_diagnostic(
    change: &ExpectedChange,
    actual_index: usize,
    actual: &ActualChange,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<WrongChangeKindDiagnostic> {
    let needles = normalized_expected_quotes(change);
    let mut quote_matching_occurrences = 0usize;
    let mut matching_occurrence_index = None;
    for (index, occurrence) in actual.occurrences.iter().enumerate() {
        let Some(scan_work) = wrong_kind_scan_work(occurrence) else {
            budget.limited = true;
            return Ok(WrongChangeKindDiagnostic::Limited {
                actual_index,
                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
            });
        };
        if budget.charge_scan(scan_work, limits).is_err() {
            return Ok(WrongChangeKindDiagnostic::Limited {
                actual_index,
                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
            });
        }
        if occurrence_matches_expected_kind(occurrence, &needles, change.kind, actual.kind) {
            let Some(next) = quote_matching_occurrences.checked_add(1) else {
                budget.limited = true;
                return Ok(WrongChangeKindDiagnostic::Limited {
                    actual_index,
                    stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
                });
            };
            quote_matching_occurrences = next;
            matching_occurrence_index.get_or_insert(index);
        }
    }

    let trace = if quote_matching_occurrences == 1 {
        let occurrence_index = matching_occurrence_index.ok_or_else(|| {
            DiagnosticScanError::Invalid(
                "wrong-kind quote-match count lacks an occurrence index".to_owned(),
            )
        })?;
        let occurrence = &actual.occurrences[occurrence_index];
        match &occurrence.relation_trace {
            ActualRelationTraceStatus::Available(trace) => {
                let semantic_hunks = match occurrence.semantic_hunks.as_deref() {
                    Some(hunks) => match wrong_kind_semantic_hunks(occurrence, hunks, &needles) {
                        Ok(report) => report,
                        Err(DiagnosticScanError::Limited) => {
                            budget.limited = true;
                            return Ok(WrongChangeKindDiagnostic::Limited {
                                actual_index,
                                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
                            });
                        }
                        Err(error @ DiagnosticScanError::Invalid(_)) => return Err(error),
                    },
                    None => WrongChangeKindSemanticHunkReport::Unavailable,
                };
                WrongChangeKindTraceReport::Available {
                    occurrence_index,
                    origin: ChangeOriginReport::from(trace.origin),
                    old_alignment_span_index: trace.old_alignment_span_index,
                    new_alignment_span_index: trace.new_alignment_span_index,
                    old_relation_context_tokens: occurrence.old_relation_context_len,
                    new_relation_context_tokens: occurrence.new_relation_context_len,
                    expected_old_quote_in_relation_context: optional_quote_presence(
                        occurrence.old_relation_context.as_deref(),
                        needles.old_context.as_deref(),
                    ),
                    expected_new_quote_in_relation_context: optional_quote_presence(
                        occurrence.new_relation_context.as_deref(),
                        needles.new_context.as_deref(),
                    ),
                    semantic_hunks,
                    old_best_score: trace.old_best_score,
                    old_second_score: trace.old_second_score,
                    old_best_scope: trace.old_best_scope,
                    new_best_score: trace.new_best_score,
                    new_second_score: trace.new_second_score,
                    new_best_scope: trace.new_best_scope,
                }
            }
            ActualRelationTraceStatus::Assessed { relation } => {
                let semantic_hunks = match occurrence.semantic_hunks.as_deref() {
                    Some(hunks) => match wrong_kind_semantic_hunks(occurrence, hunks, &needles) {
                        Ok(report) => report,
                        Err(DiagnosticScanError::Limited) => {
                            budget.limited = true;
                            return Ok(WrongChangeKindDiagnostic::Limited {
                                actual_index,
                                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
                            });
                        }
                        Err(error @ DiagnosticScanError::Invalid(_)) => return Err(error),
                    },
                    None => WrongChangeKindSemanticHunkReport::Unavailable,
                };
                WrongChangeKindTraceReport::Assessed {
                    occurrence_index,
                    relation: *relation,
                    semantic_hunks,
                }
            }
            ActualRelationTraceStatus::Ambiguous => WrongChangeKindTraceReport::Ambiguous,
            ActualRelationTraceStatus::Untraced => WrongChangeKindTraceReport::Untraced,
        }
    } else {
        WrongChangeKindTraceReport::Ambiguous
    };
    Ok(WrongChangeKindDiagnostic::Complete {
        actual_index,
        quote_matching_occurrences,
        trace,
    })
}

fn wrong_kind_semantic_hunks(
    occurrence: &ActualChangeOccurrence,
    hunks: &[super::ActualSemanticHunk],
    needles: &super::NormalizedExpectedQuotes,
) -> DiagnosticScanResult<WrongChangeKindSemanticHunkReport> {
    if hunks.iter().any(|hunk| {
        (hunk.old_atomic_changed_tokens != 0 && hunk.old_text.is_none())
            || (hunk.new_atomic_changed_tokens != 0 && hunk.new_text.is_none())
    }) {
        return Ok(WrongChangeKindSemanticHunkReport::Unavailable);
    }
    let mut insertion_only_hunks = 0usize;
    let mut deletion_only_hunks = 0usize;
    let mut replacement_hunks = 0usize;
    let mut old_atomic_changed_tokens = 0usize;
    let mut new_atomic_changed_tokens = 0usize;
    for hunk in hunks {
        old_atomic_changed_tokens = old_atomic_changed_tokens
            .checked_add(hunk.old_atomic_changed_tokens)
            .ok_or(DiagnosticScanError::Limited)?;
        new_atomic_changed_tokens = new_atomic_changed_tokens
            .checked_add(hunk.new_atomic_changed_tokens)
            .ok_or(DiagnosticScanError::Limited)?;
        let count = match (
            hunk.old_atomic_changed_tokens != 0,
            hunk.new_atomic_changed_tokens != 0,
        ) {
            (false, true) => &mut insertion_only_hunks,
            (true, false) => &mut deletion_only_hunks,
            (true, true) => &mut replacement_hunks,
            (false, false) => continue,
        };
        *count = count.checked_add(1).ok_or(DiagnosticScanError::Limited)?;
    }
    Ok(WrongChangeKindSemanticHunkReport::Available {
        old_reported_semantic_tokens: occurrence.old_semantic_changed_tokens,
        new_reported_semantic_tokens: occurrence.new_semantic_changed_tokens,
        old_atomic_changed_tokens,
        new_atomic_changed_tokens,
        insertion_only_hunks,
        deletion_only_hunks,
        replacement_hunks,
        expected_old_quote_in_semantic_hunk: needles.old_context.as_deref().map(|quote| {
            hunks.iter().any(|hunk| {
                hunk.old_text
                    .as_deref()
                    .is_some_and(|text| text.contains(quote))
            })
        }),
        expected_new_quote_in_semantic_hunk: needles.new_context.as_deref().map(|quote| {
            hunks.iter().any(|hunk| {
                hunk.new_text
                    .as_deref()
                    .is_some_and(|text| text.contains(quote))
            })
        }),
        expected_new_quote_in_insertion_only_hunk: needles.new_context.as_deref().map(|quote| {
            hunks.iter().any(|hunk| {
                hunk.old_atomic_changed_tokens == 0
                    && hunk.new_atomic_changed_tokens != 0
                    && hunk
                        .new_text
                        .as_deref()
                        .is_some_and(|text| text.contains(quote))
            })
        }),
    })
}

#[derive(Clone, Copy)]
pub(super) struct ComparisonDiagnosticInput<'a> {
    pub(super) alignment: Option<&'a Alignment>,
    pub(super) comparison: &'a Comparison,
    pub(super) actual_scopes: Option<&'a [Option<String>]>,
}

struct AlignmentSpanIndex {
    old: HashMap<BlockId, usize>,
    new: HashMap<BlockId, usize>,
    reading_order_unknown: HashSet<usize>,
}

fn build_alignment_span_index(
    alignment: &Alignment,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<AlignmentSpanIndex> {
    let mut old = HashMap::new();
    let mut new = HashMap::new();
    let mut reading_order_unknown = HashSet::new();
    for (index, span) in alignment.spans.iter().enumerate() {
        budget.charge_region(limits)?;
        if span.kind == AlignmentKind::Unresolved
            && span.evidence == [AlignmentEvidence::ReadingOrderUnknown]
        {
            reading_order_unknown.insert(index);
        }
        for block in &span.old {
            budget.charge_scan(1, limits)?;
            if old.insert(*block, index).is_some() {
                return Err(DiagnosticScanError::Invalid(format!(
                    "old block {} occurs in multiple alignment spans",
                    block.0
                )));
            }
        }
        for block in &span.new {
            budget.charge_scan(1, limits)?;
            if new.insert(*block, index).is_some() {
                return Err(DiagnosticScanError::Invalid(format!(
                    "new block {} occurs in multiple alignment spans",
                    block.0
                )));
            }
        }
    }
    Ok(AlignmentSpanIndex {
        old,
        new,
        reading_order_unknown,
    })
}

fn alignment_failure_reason(
    index: Option<&AlignmentSpanIndex>,
    locations: &ExpectedQuoteLocations,
) -> Option<ExpectedChangeFailureReason> {
    let (Some(index), Some(old), Some(new)) =
        (index, location(&locations.old), location(&locations.new))
    else {
        return None;
    };
    match (index.old.get(&old.block), index.new.get(&new.block)) {
        (Some(old_span), Some(new_span)) if old_span != new_span => {
            let unknown_mask = u8::from(index.reading_order_unknown.contains(old_span))
                | (u8::from(index.reading_order_unknown.contains(new_span)) << 1);
            if unknown_mask == 0 {
                Some(ExpectedChangeFailureReason::AlignmentSpanMismatch)
            } else {
                Some(ExpectedChangeFailureReason::ReadingOrderUnresolved {
                    side: side_from_mask(unknown_mask),
                })
            }
        }
        (Some(old_span), Some(new_span))
            if old_span == new_span && index.reading_order_unknown.contains(old_span) =>
        {
            Some(ExpectedChangeFailureReason::ReadingOrderUnresolved {
                side: MissSide::Both,
            })
        }
        _ => None,
    }
}

fn final_one_sided_ownership(
    change: &ExpectedChange,
    comparison: &Comparison,
    locations: &ExpectedQuoteLocations,
    blocks_by_side: [&HashMap<u64, &BlockText>; 2],
    claimed_actuals: &HashSet<usize>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<bool> {
    if change.kind != ExpectedKind::Replacement {
        return Ok(false);
    }
    let (Some(old), Some(new)) = (location(&locations.old), location(&locations.new)) else {
        return Ok(false);
    };
    let mut old_owners = 0usize;
    let mut new_owners = 0usize;
    for (change_index, event) in comparison.changes.iter().enumerate() {
        if claimed_actuals.contains(&change_index) {
            continue;
        }
        budget.charge_hunk(limits)?;
        match event.kind {
            ChangeKind::Deletion => {
                for occurrence in &event.occurrences {
                    let Some(span) = occurrence.old_span.as_ref() else {
                        continue;
                    };
                    if span_contains_location(span, old, blocks_by_side[0], budget, limits)? {
                        old_owners = old_owners.checked_add(1).ok_or_else(|| {
                            budget.limited = true;
                            DiagnosticScanError::Limited
                        })?;
                        if old_owners > 1 {
                            return Ok(false);
                        }
                    }
                }
            }
            ChangeKind::Insertion => {
                for occurrence in &event.occurrences {
                    let Some(span) = occurrence.new_span.as_ref() else {
                        continue;
                    };
                    if span_contains_location(span, new, blocks_by_side[1], budget, limits)? {
                        new_owners = new_owners.checked_add(1).ok_or_else(|| {
                            budget.limited = true;
                            DiagnosticScanError::Limited
                        })?;
                        if new_owners > 1 {
                            return Ok(false);
                        }
                    }
                }
            }
            ChangeKind::Replacement | ChangeKind::Move => {}
        }
    }
    Ok(old_owners == 1 && new_owners == 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExpectedMaskObjectiveCompatibility {
    expected_mask_is_valid_edit_witness: bool,
    expected_mask_cost: usize,
    optimal_cost_under_declared_policy: usize,
    expected_mask_in_policy_solution_set: bool,
}

fn changed_range_cost(
    ranges: &[ExpectedChangedRange],
    context_len: usize,
) -> Option<(Vec<bool>, usize)> {
    let mut mask = vec![false; context_len];
    let mut cost = 0usize;
    let mut previous_end = 0usize;
    for range in ranges {
        if range.start >= range.end || range.end > context_len || range.start < previous_end {
            return None;
        }
        for selected in &mut mask[range.start..range.end] {
            *selected = true;
        }
        cost = cost.checked_add(range.end.checked_sub(range.start)?)?;
        previous_end = range.end;
    }
    Some((mask, cost))
}

fn literal_minimal_cost(
    old: &[char],
    new: &[char],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<usize> {
    let mut previous = vec![
        0usize;
        new.len().checked_add(1).ok_or_else(|| {
            budget.limited = true;
            DiagnosticScanError::Limited
        })?
    ];
    let mut current = vec![0usize; previous.len()];
    for old_value in old {
        current[0] = 0;
        for (new_index, new_value) in new.iter().enumerate() {
            budget.charge_scan(1, limits)?;
            current[new_index + 1] = if old_value == new_value {
                previous[new_index] + 1
            } else {
                current[new_index].max(previous[new_index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let lcs = previous[new.len()];
    old.len()
        .checked_add(new.len())
        .and_then(|length| {
            lcs.checked_mul(2)
                .and_then(|matched| length.checked_sub(matched))
        })
        .ok_or_else(|| {
            budget.limited = true;
            DiagnosticScanError::Limited
        })
}

fn expected_mask_objective_compatibility(
    change: &ExpectedChange,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<ExpectedMaskObjectiveCompatibility>> {
    let (Some(old_quote), Some(new_quote), Some(old_ranges), Some(new_ranges)) = (
        change.old_quote.as_deref(),
        change.new_quote.as_deref(),
        change.old_changed_ranges.as_deref(),
        change.new_changed_ranges.as_deref(),
    ) else {
        return Ok(None);
    };
    let old_text = collapse_whitespace(old_quote);
    let new_text = collapse_whitespace(new_quote);
    let old_chars = old_text.chars().collect::<Vec<_>>();
    let new_chars = new_text.chars().collect::<Vec<_>>();
    let quote_work = old_chars
        .len()
        .checked_add(new_chars.len())
        .ok_or_else(|| {
            budget.limited = true;
            DiagnosticScanError::Limited
        })?;
    budget.charge_scan(quote_work, limits)?;
    let Some((old_mask, old_cost)) = changed_range_cost(old_ranges, old_chars.len()) else {
        return Ok(None);
    };
    let Some((new_mask, new_cost)) = changed_range_cost(new_ranges, new_chars.len()) else {
        return Ok(None);
    };
    let old_kept = old_chars
        .iter()
        .zip(&old_mask)
        .filter_map(|(value, changed)| (!changed).then_some(*value))
        .collect::<String>();
    let new_kept = new_chars
        .iter()
        .zip(&new_mask)
        .filter_map(|(value, changed)| (!changed).then_some(*value))
        .collect::<String>();
    let valid = old_kept == new_kept;
    let expected_cost = old_cost.checked_add(new_cost).ok_or_else(|| {
        budget.limited = true;
        DiagnosticScanError::Limited
    })?;
    let optimal_cost = literal_minimal_cost(&old_chars, &new_chars, budget, limits)?;
    Ok(Some(ExpectedMaskObjectiveCompatibility {
        expected_mask_is_valid_edit_witness: valid,
        expected_mask_cost: expected_cost,
        optimal_cost_under_declared_policy: optimal_cost,
        expected_mask_in_policy_solution_set: valid && expected_cost == optimal_cost,
    }))
}

fn expected_mask_objective_failure_reason(
    change: &ExpectedChange,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<ExpectedChangeFailureReason>> {
    let Some(compatibility) = expected_mask_objective_compatibility(change, budget, limits)? else {
        return Ok(None);
    };
    if compatibility.expected_mask_is_valid_edit_witness
        && !compatibility.expected_mask_in_policy_solution_set
    {
        return Ok(Some(
            ExpectedChangeFailureReason::ExpectationOutsideAlignmentObjective {
                expected_mask_is_valid_edit_witness: compatibility
                    .expected_mask_is_valid_edit_witness,
                expected_mask_cost: compatibility.expected_mask_cost,
                optimal_cost_under_declared_policy: compatibility
                    .optimal_cost_under_declared_policy,
                expected_mask_in_policy_solution_set: compatibility
                    .expected_mask_in_policy_solution_set,
            },
        ));
    }
    Ok(None)
}

fn classify_expected_failure(
    change: &ExpectedChange,
    candidate: ReviewedCandidateResult,
    locations: &ExpectedQuoteLocations,
    context: &mut FailureContext<'_>,
) -> std::result::Result<ExpectedChangeFailureReason, String> {
    if side_for_status(locations, |outcome| {
        matches!(outcome, QuoteLocateOutcome::Limited)
    })
    .is_some()
    {
        return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
            diagnostic_limited: true,
        });
    }
    match expected_mask_objective_failure_reason(change, context.budget, context.limits) {
        Ok(Some(reason)) => return Ok(reason),
        Ok(None) => {}
        Err(DiagnosticScanError::Limited) => {
            return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            });
        }
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    }
    if candidate == ReviewedCandidateResult::Missed {
        return Ok(ExpectedChangeFailureReason::CandidateNotGenerated);
    }
    if candidate == ReviewedCandidateResult::Recalled && context.alignment_index_limited {
        return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
            diagnostic_limited: true,
        });
    }
    match unresolved_failure_reason(
        context.comparison,
        locations,
        context.blocks_by_side,
        context.budget,
        context.limits,
    ) {
        Ok(Some(reason)) => return Ok(reason),
        Ok(None) => {}
        Err(DiagnosticScanError::Limited) => {
            return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            });
        }
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    }
    if candidate == ReviewedCandidateResult::Recalled {
        match final_one_sided_ownership(
            change,
            context.comparison,
            locations,
            context.blocks_by_side,
            context.claimed_actuals,
            context.budget,
            context.limits,
        ) {
            Ok(true) => {
                return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
                    diagnostic_limited: false,
                });
            }
            Ok(false) => {}
            Err(DiagnosticScanError::Limited) => {
                return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
                    diagnostic_limited: true,
                });
            }
            Err(DiagnosticScanError::Invalid(error)) => return Err(error),
        }
        if let Some(reason) = alignment_failure_reason(context.alignment_index, locations) {
            return Ok(reason);
        }
    }
    if candidate == ReviewedCandidateResult::Limited {
        return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
            diagnostic_limited: true,
        });
    }
    match fragmentation_reason(
        change,
        context.comparison,
        locations,
        context.claimed_actuals,
        context.budget,
        context.limits,
    ) {
        Ok(Some(reason)) => return Ok(reason),
        Ok(None) => {}
        Err(DiagnosticScanError::Limited) => {
            return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            });
        }
        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
    }
    Ok(quote_failure_reason(locations))
}

fn candidate_query<'a>(
    block: BlockId,
    features: &BlockFeatures,
    generator: &InvertedIndexCandidateGenerator,
    top_k: usize,
    visit_limit: usize,
    budget: &mut DiagnosticBudget,
    cache: &'a mut HashMap<BlockId, CandidateQuery>,
) -> std::result::Result<&'a CandidateQuery, String> {
    let entry = match cache.entry(block) {
        Entry::Occupied(entry) => return Ok(entry.into_mut()),
        Entry::Vacant(entry) => entry,
    };
    let estimated_visits = generator
        .estimated_visits(features, top_k)
        .map_err(|error| format!("reviewed candidate estimate failed: {error}"))?;
    let query = if budget
        .charge_candidate_visits(estimated_visits, visit_limit)
        .is_err()
    {
        CandidateQuery::Limited
    } else {
        let candidates = generator
            .candidates(features, top_k)
            .map_err(|error| format!("reviewed candidate query failed: {error}"))?
            .into_iter()
            .map(|candidate| candidate.block)
            .collect();
        CandidateQuery::Available(candidates)
    };
    Ok(entry.insert(query))
}

fn evaluate_reviewed_diagnostics_with_limits(
    expected: &[ExpectedChange],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    input: ComparisonDiagnosticInput<'_>,
    actuals: &[ActualChange],
    outcome: &MatchOutcome,
    limits: DiagnosticLimits,
) -> std::result::Result<ReviewedDiagnostics, String> {
    let ComparisonDiagnosticInput {
        alignment,
        comparison,
        actual_scopes,
    } = input;
    debug_assert!(actual_scopes.is_none_or(|scopes| scopes.len() == actuals.len()));
    let processed_expected = &expected[..expected.len().min(limits.max_expected_changes)];
    let first_unprocessed = expected.get(processed_expected.len());
    let production = PipelineOptions::default();
    let top_k = production.alignment.candidate_limit;
    let candidate_visit_limit = production
        .alignment
        .max_candidate_visits
        .min(limits.max_candidate_visits);
    let has_reviewed_counterpart = |change: &ExpectedChange| {
        matches!(change.kind, ExpectedKind::Replacement | ExpectedKind::Move)
            && change.old_quote.is_some()
            && change.new_quote.is_some()
    };
    // Candidate metrics describe only annotations processed within the diagnostic cap.
    let annotated_counterparts = processed_expected
        .iter()
        .try_fold(0_usize, |count, change| {
            if has_reviewed_counterpart(change) {
                count
                    .checked_add(1)
                    .ok_or_else(|| "annotated candidate counterparts overflow".to_owned())
            } else {
                Ok(count)
            }
        })?;
    let old_features = build_block_features(old_blocks, production.ngram_size)
        .map_err(|error| format!("reviewed candidate old features failed: {error}"))?;
    let new_features = build_block_features(new_blocks, production.ngram_size)
        .map_err(|error| format!("reviewed candidate new features failed: {error}"))?;
    let generator = InvertedIndexCandidateGenerator::new(&new_features)
        .map_err(|error| format!("reviewed candidate index failed: {error}"))?;
    let old_features_by_id = old_features
        .iter()
        .map(|features| (features.block, features))
        .collect::<HashMap<_, _>>();
    let mut budget = DiagnosticBudget {
        limited: first_unprocessed.is_some(),
        ..DiagnosticBudget::default()
    };
    let mut locations = Vec::with_capacity(processed_expected.len());
    let mut candidate_results = Vec::with_capacity(processed_expected.len());
    let mut candidate_cache = HashMap::new();
    let mut evaluable_counterparts = 0_usize;
    let mut recalled_counterparts = 0_usize;

    for change in processed_expected {
        let old = match change.old_quote.as_deref() {
            Some(quote) => Some(locate_quote(old_blocks, quote, &mut budget, limits)?),
            None => None,
        };
        let new = match change.new_quote.as_deref() {
            Some(quote) => Some(locate_quote(new_blocks, quote, &mut budget, limits)?),
            None => None,
        };
        let change_locations = ExpectedQuoteLocations { old, new };
        let candidate = if has_reviewed_counterpart(change) {
            match (
                location(&change_locations.old),
                location(&change_locations.new),
            ) {
                (Some(old), Some(new)) => {
                    let features = old_features_by_id.get(&old.block).ok_or_else(|| {
                        format!(
                            "reviewed candidate quote resolved to missing old block {}",
                            old.block.0
                        )
                    })?;
                    match candidate_query(
                        old.block,
                        features,
                        &generator,
                        top_k,
                        candidate_visit_limit,
                        &mut budget,
                        &mut candidate_cache,
                    )? {
                        CandidateQuery::Available(candidates) => {
                            checked_increment(
                                &mut evaluable_counterparts,
                                "evaluable candidate counterparts",
                            )?;
                            if candidates.contains(&new.block) {
                                checked_increment(
                                    &mut recalled_counterparts,
                                    "recalled candidate counterparts",
                                )?;
                                ReviewedCandidateResult::Recalled
                            } else {
                                ReviewedCandidateResult::Missed
                            }
                        }
                        CandidateQuery::Limited => ReviewedCandidateResult::Limited,
                    }
                }
                _ => ReviewedCandidateResult::Unavailable,
            }
        } else {
            ReviewedCandidateResult::NotApplicable
        };
        locations.push(change_locations);
        candidate_results.push(candidate);
    }

    let unavailable_counterparts = annotated_counterparts
        .checked_sub(evaluable_counterparts)
        .ok_or_else(|| "candidate availability counters are inconsistent".to_owned())?;
    let candidate_recall = first_unprocessed
        .is_none()
        .then_some(CandidateRecallMetrics {
            top_k,
            annotated_counterparts,
            evaluable_counterparts,
            recalled_counterparts,
            unavailable_counterparts,
            recall_at_k: ratio(recalled_counterparts, evaluable_counterparts),
        });

    let old_map = build_block_map(old_blocks);
    let new_map = build_block_map(new_blocks);
    let needs_alignment_index = candidate_results
        .iter()
        .zip(&outcome.claimed_actual_by_expected)
        .any(|(result, actual)| *result == ReviewedCandidateResult::Recalled && actual.is_none());
    let (alignment_index, alignment_index_limited) = if needs_alignment_index {
        match alignment.map(|alignment| build_alignment_span_index(alignment, &mut budget, limits))
        {
            Some(Ok(index)) => (Some(index), false),
            Some(Err(DiagnosticScanError::Limited)) => (None, true),
            Some(Err(DiagnosticScanError::Invalid(error))) => return Err(error),
            None => (None, false),
        }
    } else {
        (None, false)
    };
    let mut failures = Vec::new();
    let mut context = FailureContext {
        alignment_index: alignment_index.as_ref(),
        alignment_index_limited,
        comparison,
        blocks_by_side: [&old_map, &new_map],
        claimed_actuals: &outcome.claimed_actuals,
        budget: &mut budget,
        limits,
    };
    for (index, change) in processed_expected.iter().enumerate() {
        let reason = match outcome.claimed_actual_by_expected[index] {
            Some(actual_index) if change.kind.agrees_with(actuals[actual_index].kind) => continue,
            Some(actual_index) => ExpectedChangeFailureReason::WrongChangeKind {
                expected: change.kind.name().to_owned(),
                actual: change_kind_name(actuals[actual_index].kind).to_owned(),
                diagnostic: Box::new(
                    match wrong_kind_diagnostic(
                        change,
                        actual_index,
                        &actuals[actual_index],
                        context.budget,
                        limits,
                    ) {
                        Ok(diagnostic) => diagnostic,
                        Err(DiagnosticScanError::Limited) => {
                            context.budget.limited = true;
                            WrongChangeKindDiagnostic::Limited {
                                actual_index,
                                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
                            }
                        }
                        Err(DiagnosticScanError::Invalid(error)) => return Err(error),
                    },
                ),
            },
            None => match outcome.occurrence_count_mismatch_by_expected[index] {
                Some((expected, actual)) => {
                    ExpectedChangeFailureReason::OccurrenceCountMismatch { expected, actual }
                }
                None => classify_expected_failure(
                    change,
                    candidate_results[index],
                    &locations[index],
                    &mut context,
                )?,
            },
        };
        if context.budget.charge_output(limits).is_err() {
            if limits.max_output_records > 0 {
                failures.pop();
                failures.push(ExpectedChangeFailure {
                    expected_id: change.id.clone(),
                    reason: ExpectedChangeFailureReason::AlignmentOrCandidate {
                        diagnostic_limited: true,
                    },
                });
            }
            break;
        }
        failures.push(ExpectedChangeFailure {
            expected_id: change.id.clone(),
            reason,
        });
    }
    if let Some(change) = first_unprocessed {
        let fallback = ExpectedChangeFailure {
            expected_id: change.id.clone(),
            reason: ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            },
        };
        if context.budget.charge_output(limits).is_ok() {
            failures.push(fallback);
        } else if limits.max_output_records > 0 {
            failures.pop();
            failures.push(fallback);
        }
    }
    let final_assessment = assessment_diagnostics::evaluate(
        processed_expected,
        &locations,
        comparison,
        [&old_map, &new_map],
        limits,
        first_unprocessed.is_none(),
    )?;
    Ok(ReviewedDiagnostics {
        candidate_recall,
        expected_change_diagnostics: ExpectedChangeDiagnostics {
            complete: !context.budget.limited,
            failures,
            recovery_watch: None,
            final_assessment,
            expected_matches: expected_match_evaluations(processed_expected, actuals, outcome),
        },
    })
}

pub(super) fn expected_match_evaluations(
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
    outcome: &MatchOutcome,
) -> Vec<ExpectedChangeMatchEvaluation> {
    expected
        .iter()
        .enumerate()
        .map(|(expected_index, change)| {
            let actual_index = outcome
                .claimed_actual_by_expected
                .get(expected_index)
                .copied()
                .flatten();
            let actual_kind = actual_index
                .and_then(|index| actuals.get(index))
                .map(|actual| change_kind_name(actual.kind).to_owned());
            let kind_agrees = actual_index
                .and_then(|index| actuals.get(index))
                .map(|actual| change.kind.agrees_with(actual.kind));
            ExpectedChangeMatchEvaluation {
                expected_id: change.id.clone(),
                matched: actual_index.is_some(),
                actual_index,
                actual_kind,
                kind_agrees,
            }
        })
        .collect()
}

pub(super) fn evaluate_reviewed_diagnostics(
    expected: &[ExpectedChange],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    input: ComparisonDiagnosticInput<'_>,
    actuals: &[ActualChange],
    outcome: &MatchOutcome,
) -> std::result::Result<ReviewedDiagnostics, String> {
    evaluate_reviewed_diagnostics_with_limits(
        expected,
        old_blocks,
        new_blocks,
        input,
        actuals,
        outcome,
        DiagnosticLimits::default(),
    )
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        alignment::{AlignmentConfidence, AlignmentKind, AlignmentSpan},
        diff::{Change, ChangeKind, Confidence, Coverage, TokenRange, UnresolvedRegion},
        layout::BlockRole,
        model::FontProgramHash,
        normalize::{
            MappedText, NormalizationIssue, NormalizationIssueKind, TextSource, UnmappedToken,
        },
    };

    use super::super::{
        Annotation, RecoveryWatchNearScopeReport, compute_quality, match_changes,
        match_changes_with_scopes, quality_from_match_outcome,
    };
    use super::*;

    fn expected_change(
        id: &str,
        kind: ExpectedKind,
        old: Option<&str>,
        new: Option<&str>,
    ) -> ExpectedChange {
        ExpectedChange {
            id: id.to_owned(),
            kind,
            scope: None,
            occurrence_count: None,
            old_quote: old.map(str::to_owned),
            new_quote: new.map(str::to_owned),
            old_changed_quote: None,
            new_changed_quote: None,
            old_changed_ranges: None,
            new_changed_ranges: None,
            note: String::new(),
        }
    }

    fn actual_change(
        kind: ChangeKind,
        old_text: Option<&str>,
        new_text: Option<&str>,
        old_len: Option<usize>,
        new_len: Option<usize>,
    ) -> ActualChange {
        ActualChange {
            kind,
            reported_hunk_count: 1,
            occurrences: vec![crate::revisions::ActualChangeOccurrence {
                old_text: old_text.map(str::to_owned),
                new_text: new_text.map(str::to_owned),
                old_relation_context: None,
                new_relation_context: None,
                old_relation_context_len: None,
                new_relation_context_len: None,
                old_comparable_len: old_len,
                new_comparable_len: new_len,
                old_atomic_changed_tokens: None,
                new_atomic_changed_tokens: None,
                old_semantic_changed_tokens: old_len,
                new_semantic_changed_tokens: new_len,
                semantic_hunks: None,
                relation_trace: ActualRelationTraceStatus::Untraced,
                resolvable: true,
            }],
        }
    }

    fn diagnostic_mapped(text: &str) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        }
    }

    fn diagnostic_block(id: u64, text: &str) -> BlockText {
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: diagnostic_mapped(text),
            canonical: diagnostic_mapped(text),
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

    fn diagnostic_span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn diagnostic_change(
        kind: ChangeKind,
        old_span: Option<TextSpan>,
        new_span: Option<TextSpan>,
    ) -> Change {
        Change::single_occurrence(kind, old_span, new_span, Confidence::High, Vec::new())
    }

    fn diagnostic_comparison(
        changes: Vec<Change>,
        unresolved_regions: Vec<UnresolvedRegion>,
    ) -> Comparison {
        let coverage = Coverage {
            resolved_tokens: 0,
            total_tokens: 0,
            ratio: None,
        };
        Comparison {
            changes,
            change_candidates: Vec::new(),
            assessment: None,
            proven_changed_regions: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions,
            old_coverage: coverage,
            new_coverage: coverage,
        }
    }

    fn reviewed_diagnostics(
        expected: &[ExpectedChange],
        old_blocks: &[BlockText],
        new_blocks: &[BlockText],
        comparison: &Comparison,
        actuals: &[ActualChange],
    ) -> ReviewedDiagnostics {
        let outcome = match_changes(expected, actuals);
        evaluate_reviewed_diagnostics(
            expected,
            old_blocks,
            new_blocks,
            ComparisonDiagnosticInput {
                alignment: None,
                comparison,
                actual_scopes: None,
            },
            actuals,
            &outcome,
        )
        .expect("diagnostics succeed")
    }

    fn comparison_with_final_candidate() -> Comparison {
        use pdfdelta_core::diff::{
            AssessmentReason, AssessmentWork, ChangeCandidate, ComparisonAssessment,
            RelationAssessment, RelationOutcome, SearchCompleteness,
        };

        let mut comparison = diagnostic_comparison(Vec::new(), Vec::new());
        comparison.assessment = Some(ComparisonAssessment {
            localized_edits: Vec::new(),
            policy_version: 1,
            relations: vec![
                RelationAssessment {
                    old_span: Some(diagnostic_span(1, 0, 16)),
                    new_span: Some(diagnostic_span(2, 0, 17)),
                    parent: None,
                    outcome: RelationOutcome::Tentative,
                    search: SearchCompleteness::Complete,
                    assumptions: Vec::new(),
                    reasons: vec![AssessmentReason::UnknownReadingOrder],
                },
                RelationAssessment {
                    old_span: Some(diagnostic_span(1, 7, 10)),
                    new_span: Some(diagnostic_span(2, 7, 11)),
                    parent: Some(0),
                    outcome: RelationOutcome::Tentative,
                    search: SearchCompleteness::Complete,
                    assumptions: Vec::new(),
                    reasons: vec![AssessmentReason::UnknownReadingOrder],
                },
            ],
            old_resolution: Vec::new(),
            new_resolution: Vec::new(),
            work_limit: 100,
            work_used: 0,
            work_by_stage: AssessmentWork::default(),
            candidates_truncated: false,
            review_units: Vec::new(),
        });
        comparison.change_candidates.push(ChangeCandidate {
            change: diagnostic_change(
                ChangeKind::Replacement,
                Some(diagnostic_span(1, 7, 10)),
                Some(diagnostic_span(2, 7, 11)),
            ),
            relation: 1,
            alternative_group: 1,
        });
        comparison
    }

    #[test]
    fn final_assessment_trace_keeps_candidate_and_parent_rejection_separate_from_generator() {
        let expected = [expected_change(
            "color",
            ExpectedKind::Replacement,
            Some("before red after"),
            Some("before blue after"),
        )];
        let comparison = comparison_with_final_candidate();
        let diagnostic = reviewed_diagnostics(
            &expected,
            &[diagnostic_block(1, "before red after")],
            &[diagnostic_block(2, "before blue after")],
            &comparison,
            &[],
        );
        let final_assessment = diagnostic
            .expected_change_diagnostics
            .final_assessment
            .expect("final assessment is available");
        assert!(final_assessment.complete);
        let trace = &final_assessment.records[0];
        assert!(trace.scan_complete);
        assert_eq!(trace.candidates.len(), 1);
        assert_eq!(trace.candidates[0].relation, Some(1));
        assert!(trace.accepted_changes.is_empty());
        assert_eq!(trace.relations.len(), 2);
        assert_eq!(trace.relations[1].parent, Some(0));
        assert_eq!(trace.relations[0].reasons, ["UnknownReadingOrder"]);
        assert_eq!(trace.old.canonical_range, Some([0, 16]));
        assert_eq!(
            trace.relations[1]
                .old
                .as_ref()
                .expect("replacement retains its old range")
                .canonical_range,
            [7, 10]
        );
    }

    #[test]
    fn final_assessment_trace_preserves_diagnostic_limits() {
        let expected = [expected_change(
            "color",
            ExpectedKind::Replacement,
            Some("red"),
            Some("blue"),
        )];
        let comparison = comparison_with_final_candidate();
        for limits in [
            DiagnosticLimits {
                max_output_records: 0,
                ..DiagnosticLimits::default()
            },
            DiagnosticLimits {
                max_output_records: 1,
                ..DiagnosticLimits::default()
            },
            DiagnosticLimits {
                max_scan_work: 0,
                ..DiagnosticLimits::default()
            },
        ] {
            let diagnostic = evaluate_reviewed_diagnostics_with_limits(
                &expected,
                &[diagnostic_block(1, "before red after")],
                &[diagnostic_block(2, "before blue after")],
                ComparisonDiagnosticInput {
                    alignment: None,
                    comparison: &comparison,
                    actual_scopes: None,
                },
                &[],
                &match_changes(&expected, &[]),
                limits,
            )
            .expect("limited diagnostics remain available");
            let final_assessment = diagnostic
                .expected_change_diagnostics
                .final_assessment
                .expect("comparison supplies a final assessment");
            assert!(!final_assessment.complete);
            assert!(final_assessment.records.len() <= limits.max_output_records);
            assert!(
                final_assessment
                    .records
                    .iter()
                    .all(|trace| !trace.scan_complete)
            );
        }
    }

    fn one_sided_alignment_span(kind: AlignmentKind, block: BlockId) -> AlignmentSpan {
        let (old, new) = match kind {
            AlignmentKind::Deletion => (vec![block], Vec::new()),
            AlignmentKind::Insertion => (Vec::new(), vec![block]),
            _ => panic!("test helper requires a one-sided alignment kind"),
        };
        AlignmentSpan {
            kind,
            old,
            new,
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: Vec::new(),
            old_separator: None,
            new_separator: None,
        }
    }

    fn candidate_recall(diagnostics: &ReviewedDiagnostics) -> CandidateRecallMetrics {
        diagnostics
            .candidate_recall
            .expect("candidate diagnostics are complete")
    }

    #[test]
    fn fixed_annotations_are_valid_but_outside_literal_minimal_objective() {
        let cases = [
            (
                include_str!("../../../benchmark/realworld/expected/nist-csf-v1-1-to-v2-0.json"),
                "core-expanded-from-five-to-six-functions",
                178,
                158,
            ),
            (
                include_str!("../../../benchmark/realworld/expected/arxiv-attention-v6-to-v7.json"),
                "arxiv-version-date-stamp",
                11,
                7,
            ),
        ];
        for (contents, id, expected_cost, optimal_cost) in cases {
            let document: super::super::ExpectedDocument =
                serde_json::from_str(contents).expect("fixed annotation parses");
            let change = document
                .changes
                .iter()
                .find(|change| change.id == id)
                .expect("fixed annotation change exists");
            let mut budget = DiagnosticBudget::default();
            let compatibility = expected_mask_objective_compatibility(
                change,
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("objective audit completes")
            .expect("fixed annotation has changed ranges");
            assert_eq!(
                compatibility,
                ExpectedMaskObjectiveCompatibility {
                    expected_mask_is_valid_edit_witness: true,
                    expected_mask_cost: expected_cost,
                    optimal_cost_under_declared_policy: optimal_cost,
                    expected_mask_in_policy_solution_set: false,
                }
            );
            assert_eq!(
                expected_mask_objective_failure_reason(
                    change,
                    &mut budget,
                    DiagnosticLimits::default(),
                )
                .expect("objective failure audit completes"),
                Some(
                    ExpectedChangeFailureReason::ExpectationOutsideAlignmentObjective {
                        expected_mask_is_valid_edit_witness: true,
                        expected_mask_cost: expected_cost,
                        optimal_cost_under_declared_policy: optimal_cost,
                        expected_mask_in_policy_solution_set: false,
                    }
                )
            );
        }
    }

    #[test]
    fn an_outside_objective_mask_is_reported_without_affecting_quality_matching() {
        let mut expected = expected_change(
            "nonminimal",
            ExpectedKind::Replacement,
            Some("ab"),
            Some("ba"),
        );
        expected.old_changed_ranges = Some(vec![ExpectedChangedRange { start: 0, end: 2 }]);
        expected.new_changed_ranges = Some(vec![ExpectedChangedRange { start: 0, end: 2 }]);
        let expected = [expected];
        let actuals = Vec::new();
        let quality = compute_quality(Annotation::Partial, &expected, &actuals);
        let diagnostics = reviewed_diagnostics(
            &expected,
            &[diagnostic_block(1, "ab")],
            &[diagnostic_block(2, "ba")],
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &actuals,
        );

        assert_eq!(quality.recall, Some(0.0));
        assert_eq!(quality.expected_changes, 1);
        assert_eq!(quality.reported_changes, 0);
        assert_eq!(diagnostics.expected_change_diagnostics.failures.len(), 1);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::ExpectationOutsideAlignmentObjective {
                expected_mask_is_valid_edit_witness: true,
                expected_mask_cost: 4,
                optimal_cost_under_declared_policy: 2,
                expected_mask_in_policy_solution_set: false,
            }
        );
    }

    #[test]
    fn recalled_counterparts_in_separate_alignment_spans_report_a_span_mismatch() {
        let expected = [expected_change(
            "fee-replacement",
            ExpectedKind::Replacement,
            Some("annual fee of fifty dollars"),
            Some("annual fee of sixty dollars"),
        )];
        let old = [diagnostic_block(1, "annual fee of fifty dollars")];
        let new = [diagnostic_block(2, "annual fee of sixty dollars")];
        let alignment = Alignment {
            spans: vec![
                one_sided_alignment_span(AlignmentKind::Deletion, BlockId(1)),
                one_sided_alignment_span(AlignmentKind::Insertion, BlockId(2)),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let comparison = diagnostic_comparison(Vec::new(), Vec::new());
        let outcome = match_changes(&expected, &[]);

        let diagnostics = evaluate_reviewed_diagnostics(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: Some(&alignment),
                comparison: &comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
        )
        .expect("diagnostics succeed");

        assert_eq!(candidate_recall(&diagnostics).recalled_counterparts, 1);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentSpanMismatch
        );
    }

    #[test]
    fn mismatched_unknown_order_spans_report_the_affected_sides() {
        let locations = ExpectedQuoteLocations {
            old: Some(QuoteLocateOutcome::Unique(QuoteLocation {
                block: BlockId(1),
                scalar_range: ScalarRange { start: 0, end: 3 },
            })),
            new: Some(QuoteLocateOutcome::Unique(QuoteLocation {
                block: BlockId(2),
                scalar_range: ScalarRange { start: 0, end: 3 },
            })),
        };
        for (unknown, side) in [
            (HashSet::from([0]), MissSide::Old),
            (HashSet::from([1]), MissSide::New),
            (HashSet::from([0, 1]), MissSide::Both),
        ] {
            let index = AlignmentSpanIndex {
                old: HashMap::from([(BlockId(1), 0)]),
                new: HashMap::from([(BlockId(2), 1)]),
                reading_order_unknown: unknown,
            };
            assert_eq!(
                alignment_failure_reason(Some(&index), &locations),
                Some(ExpectedChangeFailureReason::ReadingOrderUnresolved { side })
            );
        }

        let index = AlignmentSpanIndex {
            old: HashMap::from([(BlockId(1), 0)]),
            new: HashMap::from([(BlockId(2), 1)]),
            reading_order_unknown: HashSet::new(),
        };
        assert_eq!(
            alignment_failure_reason(Some(&index), &locations),
            Some(ExpectedChangeFailureReason::AlignmentSpanMismatch)
        );
    }

    #[test]
    fn recalled_counterparts_in_one_unknown_order_span_report_reading_order() {
        let expected = [expected_change(
            "fee-replacement",
            ExpectedKind::Replacement,
            Some("annual fee of fifty dollars"),
            Some("annual fee of sixty dollars"),
        )];
        let old = [diagnostic_block(1, "annual fee of fifty dollars")];
        let new = [diagnostic_block(2, "annual fee of sixty dollars")];
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: vec![BlockId(1)],
                new: vec![BlockId(2)],
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: None,
                new_separator: None,
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let comparison = diagnostic_comparison(
            Vec::new(),
            vec![UnresolvedRegion {
                old_span: Some(diagnostic_span(1, 0, "annual fee of fifty dollars".len())),
                new_span: Some(diagnostic_span(2, 0, "annual fee of sixty dollars".len())),
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
            }],
        );
        let outcome = match_changes(&expected, &[]);

        let diagnostics = evaluate_reviewed_diagnostics(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: Some(&alignment),
                comparison: &comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
        )
        .expect("diagnostics succeed");

        assert_eq!(candidate_recall(&diagnostics).recalled_counterparts, 1);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::ReadingOrderUnresolved {
                side: MissSide::Both,
            }
        );
    }

    #[test]
    fn recalled_replacement_with_final_one_sided_owners_reports_alignment_or_candidate() {
        let expected = [expected_change(
            "fee-replacement",
            ExpectedKind::Replacement,
            Some("annual fee of fifty dollars"),
            Some("annual fee of sixty dollars"),
        )];
        let old = [diagnostic_block(1, "annual fee of fifty dollars")];
        let new = [diagnostic_block(2, "annual fee of sixty dollars")];
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: vec![BlockId(1)],
                new: vec![BlockId(2)],
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: None,
                new_separator: None,
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let comparison = diagnostic_comparison(
            vec![
                diagnostic_change(
                    ChangeKind::Deletion,
                    Some(diagnostic_span(1, 0, "annual fee of fifty dollars".len())),
                    None,
                ),
                diagnostic_change(
                    ChangeKind::Insertion,
                    None,
                    Some(diagnostic_span(2, 0, "annual fee of sixty dollars".len())),
                ),
            ],
            Vec::new(),
        );
        let outcome = match_changes(&expected, &[]);

        let diagnostics = evaluate_reviewed_diagnostics(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: Some(&alignment),
                comparison: &comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
        )
        .expect("diagnostics succeed");

        assert_eq!(candidate_recall(&diagnostics).recalled_counterparts, 1);
        assert_eq!(diagnostics.expected_change_diagnostics.failures.len(), 1);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: false,
            }
        );
    }

    #[test]
    fn alignment_span_index_charges_each_block_and_rejects_duplicate_membership() {
        let alignment = Alignment {
            spans: vec![
                one_sided_alignment_span(AlignmentKind::Deletion, BlockId(1)),
                one_sided_alignment_span(AlignmentKind::Insertion, BlockId(2)),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let mut budget = DiagnosticBudget::default();
        assert!(matches!(
            build_alignment_span_index(
                &alignment,
                &mut budget,
                DiagnosticLimits {
                    max_scan_work: 1,
                    ..DiagnosticLimits::default()
                }
            ),
            Err(DiagnosticScanError::Limited)
        ));
        assert!(budget.limited);

        let duplicate = Alignment {
            spans: vec![
                one_sided_alignment_span(AlignmentKind::Deletion, BlockId(1)),
                one_sided_alignment_span(AlignmentKind::Deletion, BlockId(1)),
            ],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let mut budget = DiagnosticBudget::default();
        assert!(matches!(
            build_alignment_span_index(&duplicate, &mut budget, DiagnosticLimits::default()),
            Err(DiagnosticScanError::Invalid(_))
        ));
    }

    #[test]
    fn matcher_state_deduplication_preserves_first_seen_order() {
        let matched = MatcherState {
            matched: 1,
            origins: VecDeque::from([MatchOrigin::Scalar(ScalarPosition { block: 1, item: 2 })]),
        };
        let empty = MatcherState::default();
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            deduplicate_states(
                vec![matched.clone(), empty.clone(), matched.clone()],
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("bounded state deduplication"),
            vec![matched, empty]
        );
    }

    #[test]
    fn quote_locator_collapses_unicode_whitespace_and_detects_cross_block_segmentation() {
        let blocks = vec![
            diagnostic_block(1, "annual"),
            diagnostic_block(2, "fee\u{2003}of"),
            diagnostic_block(3, "fifty dollars"),
        ];
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_quote(
                &blocks,
                "annualfee\t of fifty dollars",
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("quote scan"),
            QuoteLocateOutcome::Segmented
        );

        let blocks = [diagnostic_block(4, "pay annual\u{2003} fee now")];
        let mut budget = DiagnosticBudget::default();
        let QuoteLocateOutcome::Unique(location) = locate_quote(
            &blocks,
            "annual\nfee",
            &mut budget,
            DiagnosticLimits::default(),
        )
        .expect("quote scan") else {
            panic!("quote should be uniquely located");
        };
        assert_eq!(location.block, BlockId(4));
        assert_eq!(location.scalar_range, ScalarRange { start: 4, end: 15 });

        let blocks = vec![
            diagnostic_block(5, "split"),
            diagnostic_block(6, "quote"),
            diagnostic_block(7, "between"),
            diagnostic_block(8, "split"),
            diagnostic_block(9, "quote"),
        ];
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_quote(
                &blocks,
                "split quote",
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("quote scan"),
            QuoteLocateOutcome::Ambiguous
        );

        let long_quote = "x".repeat(300);
        let mut budget = DiagnosticBudget::default();
        assert!(matches!(
            locate_quote(
                &[diagnostic_block(10, &long_quote)],
                &long_quote,
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("long quote scan"),
            QuoteLocateOutcome::Unique(_)
        ));
    }

    #[test]
    fn quote_locator_matches_every_short_binary_substring_exactly() {
        for length in 1..=8usize {
            for mask in 0..(1usize << length) {
                let text = (0..length)
                    .map(|index| if mask & (1 << index) == 0 { 'a' } else { 'b' })
                    .collect::<String>();
                for start in 0..length {
                    for end in (start + 1)..=length {
                        let quote = &text[start..end];
                        let occurrences = (0..=text.len() - quote.len())
                            .filter(|offset| text[*offset..].starts_with(quote))
                            .count();
                        let mut budget = DiagnosticBudget::default();
                        let outcome = locate_quote(
                            &[diagnostic_block(1, &text)],
                            quote,
                            &mut budget,
                            DiagnosticLimits::default(),
                        )
                        .expect("plain quote scan");
                        match outcome {
                            QuoteLocateOutcome::Unique(location) => {
                                assert_eq!(
                                    occurrences, 1,
                                    "text={text:?} quote={quote:?} matched a repeated quote"
                                );
                                assert_eq!(location.block, BlockId(1));
                                assert_eq!(
                                    location.scalar_range,
                                    ScalarRange { start, end },
                                    "text={text:?} quote={quote:?}"
                                );
                            }
                            QuoteLocateOutcome::Ambiguous => assert!(
                                occurrences >= 2,
                                "text={text:?} quote={quote:?} rejected a unique quote"
                            ),
                            other => panic!("text={text:?} quote={quote:?} produced {other:?}"),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn quote_locator_treats_unmapped_tokens_as_hard_barriers_and_issues_as_indeterminate() {
        let mut unmapped = diagnostic_block(1, "abcdef");
        unmapped.canonical.unmapped.push(UnmappedToken {
            scalar_index: 3,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 7,
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_quote(
                &[unmapped],
                "abcdef",
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("quote scan"),
            QuoteLocateOutcome::Indeterminate
        );

        let mut issue = diagnostic_block(2, "other text");
        issue.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_quote(
                &[issue],
                "missing",
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("quote scan"),
            QuoteLocateOutcome::Indeterminate
        );
    }

    #[test]
    fn scope_anchor_ignores_unrelated_uncertain_evidence_but_requires_a_trusted_match() {
        let mut unrelated = diagnostic_block(2, "unrelated");
        unrelated.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        let unmapped = UnmappedToken {
            scalar_index: 0,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 7,
            source: TextSource {
                atoms: Vec::new().into(),
            },
        };
        unrelated.raw.unmapped.push(unmapped.clone());
        unrelated.canonical.unmapped.push(unmapped);

        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_scope_anchor_quote(
                &[
                    diagnostic_block(1, "unique"),
                    diagnostic_block(4, "anchor"),
                    unrelated,
                ],
                "unique anchor",
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("scope anchor scan"),
            ScopedQuoteLocateOutcome::Unique(ScopedQuoteLocation {
                start_block: 0,
                start_scalar: 0,
                end_block: 1,
                end_scalar: 6,
            })
        );

        let mut uncertain_anchor = diagnostic_block(3, "unique anchor");
        uncertain_anchor.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_scope_anchor_quote(
                &[uncertain_anchor],
                "unique anchor",
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("scope anchor scan"),
            ScopedQuoteLocateOutcome::Indeterminate
        );
    }

    #[test]
    fn scope_anchor_keeps_exact_duplicates_ambiguous_despite_uncertainty() {
        for mut duplicate in [
            diagnostic_block(2, "exact anchor"),
            diagnostic_block(3, "exact anchor"),
        ] {
            if duplicate.block == BlockId(3) {
                duplicate.issues.push(NormalizationIssue {
                    kind: NormalizationIssueKind::AmbiguousLineBreak,
                    raw_range: ScalarRange { start: 0, end: 1 },
                    source: TextSource {
                        atoms: Vec::new().into(),
                    },
                });
            }
            let mut budget = DiagnosticBudget::default();
            assert_eq!(
                locate_scope_anchor_quote(
                    &[diagnostic_block(1, "exact anchor"), duplicate],
                    "exact anchor",
                    &mut budget,
                    DiagnosticLimits::default(),
                )
                .expect("scope anchor scan"),
                ScopedQuoteLocateOutcome::Ambiguous
            );
        }
    }

    #[test]
    fn quote_locator_stays_complete_for_adversarial_repeated_prefixes() {
        let prefix = "a".repeat(512);
        let quote = format!("{prefix}b");
        let limits = DiagnosticLimits {
            max_scan_work: 100_000,
            ..DiagnosticLimits::default()
        };

        let mut budget = DiagnosticBudget::default();
        let text = format!("{}b", "a".repeat(20_000));
        assert!(matches!(
            locate_quote(&[diagnostic_block(1, &text)], &quote, &mut budget, limits,)
                .expect("linear repeated-prefix scan"),
            QuoteLocateOutcome::Unique(_)
        ));
        assert!(!budget.limited);
        assert!(budget.scan_work <= limits.max_scan_work);

        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_quote(
                &[diagnostic_block(2, &"a".repeat(20_000))],
                &quote,
                &mut budget,
                limits,
            )
            .expect("linear missing-prefix scan"),
            QuoteLocateOutcome::Missing
        );
        assert!(!budget.limited);

        let mut blocks = (0..20)
            .map(|index| diagnostic_block(index + 10, &"a".repeat(100)))
            .collect::<Vec<_>>();
        blocks.push(diagnostic_block(30, "b"));
        let cross_limits = DiagnosticLimits {
            max_scan_work: 300_000,
            ..DiagnosticLimits::default()
        };
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            locate_quote(&blocks, &quote, &mut budget, cross_limits)
                .expect("bounded cross-block repeated-prefix scan"),
            QuoteLocateOutcome::Segmented
        );
        assert!(!budget.limited);
        assert!(budget.scan_work <= cross_limits.max_scan_work);
    }

    #[test]
    fn candidate_recall_counts_hit_miss_and_unavailable_counterparts() {
        let empty = reviewed_diagnostics(
            &[],
            &[diagnostic_block(1, "old")],
            &[diagnostic_block(2, "new")],
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &[],
        );
        assert_eq!(candidate_recall(&empty).annotated_counterparts, 0);
        assert_eq!(candidate_recall(&empty).recall_at_k, None);
        assert!(empty.expected_change_diagnostics.complete);
        assert!(empty.expected_change_diagnostics.failures.is_empty());

        let expected = vec![
            expected_change(
                "hit",
                ExpectedKind::Replacement,
                Some("annual fee of fifty dollars"),
                Some("annual fee of sixty dollars"),
            ),
            expected_change(
                "miss",
                ExpectedKind::Move,
                Some("abcdefghijklmno"),
                Some("zyxwvutsrqponml"),
            ),
        ];
        let old = vec![
            diagnostic_block(1, "annual fee of fifty dollars"),
            diagnostic_block(2, "abcdefghijklmno"),
        ];
        let new = vec![
            diagnostic_block(11, "annual fee of sixty dollars"),
            diagnostic_block(12, "zyxwvutsrqponml"),
        ];
        let diagnostics = reviewed_diagnostics(
            &expected,
            &old,
            &new,
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &[],
        );
        assert_eq!(
            diagnostics.candidate_recall,
            Some(CandidateRecallMetrics {
                top_k: PipelineOptions::default().alignment.candidate_limit,
                annotated_counterparts: 2,
                evaluable_counterparts: 2,
                recalled_counterparts: 1,
                unavailable_counterparts: 0,
                recall_at_k: Some(0.5),
            })
        );
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[1].reason,
            ExpectedChangeFailureReason::CandidateNotGenerated
        );

        let duplicate_old = vec![
            diagnostic_block(1, "duplicate quote"),
            diagnostic_block(2, "duplicate quote"),
        ];
        let unavailable = reviewed_diagnostics(
            &[expected_change(
                "duplicate",
                ExpectedKind::Replacement,
                Some("duplicate quote"),
                Some("unique replacement"),
            )],
            &duplicate_old,
            &[diagnostic_block(3, "unique replacement")],
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &[],
        );
        assert_eq!(candidate_recall(&unavailable).evaluable_counterparts, 0);
        assert_eq!(candidate_recall(&unavailable).unavailable_counterparts, 1);
        assert_eq!(candidate_recall(&unavailable).recall_at_k, None);
    }

    #[test]
    fn candidate_queries_share_one_production_visit_charge_and_fail_closed_at_the_cap() {
        let old = [diagnostic_block(
            1,
            "annual fee fifty dollars clause alpha applies",
        )];
        let new = [diagnostic_block(
            2,
            "annual fee sixty dollars clause beta applies",
        )];
        let expected = [
            expected_change(
                "fee",
                ExpectedKind::Replacement,
                Some("annual fee fifty dollars"),
                Some("annual fee sixty dollars"),
            ),
            expected_change(
                "clause",
                ExpectedKind::Replacement,
                Some("clause alpha applies"),
                Some("clause beta applies"),
            ),
        ];
        let production = PipelineOptions::default();
        let old_features = build_block_features(&old, production.ngram_size).expect("old features");
        let new_features = build_block_features(&new, production.ngram_size).expect("new features");
        let generator = InvertedIndexCandidateGenerator::new(&new_features).expect("index");
        let visits = generator
            .estimated_visits(&old_features[0], production.alignment.candidate_limit)
            .expect("visit estimate");
        assert!(visits > 0);

        let comparison = diagnostic_comparison(Vec::new(), Vec::new());
        let outcome = match_changes(&expected, &[]);
        let cached = evaluate_reviewed_diagnostics_with_limits(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
            DiagnosticLimits {
                max_candidate_visits: visits,
                ..DiagnosticLimits::default()
            },
        )
        .expect("one cached query fits its exact visit estimate");
        assert!(cached.expected_change_diagnostics.complete);
        assert_eq!(candidate_recall(&cached).evaluable_counterparts, 2);
        assert_eq!(candidate_recall(&cached).recalled_counterparts, 2);
        assert_eq!(candidate_recall(&cached).unavailable_counterparts, 0);

        let one_expected = &expected[..1];
        let outcome = match_changes(one_expected, &[]);
        let limited = evaluate_reviewed_diagnostics_with_limits(
            one_expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
            DiagnosticLimits {
                max_candidate_visits: visits - 1,
                ..DiagnosticLimits::default()
            },
        )
        .expect("candidate visit cap is a safe unavailable result");
        assert!(!limited.expected_change_diagnostics.complete);
        assert_eq!(candidate_recall(&limited).evaluable_counterparts, 0);
        assert_eq!(candidate_recall(&limited).unavailable_counterparts, 1);
        assert_eq!(candidate_recall(&limited).recall_at_k, None);
        assert_eq!(
            limited.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            }
        );

        let mut overflow = DiagnosticBudget {
            candidate_visits: usize::MAX,
            ..DiagnosticBudget::default()
        };
        assert!(matches!(
            overflow.charge_candidate_visits(1, usize::MAX),
            Err(DiagnosticScanError::Limited)
        ));
        assert!(overflow.limited);
        assert!(overflow.candidate_visits_limited);
    }

    #[test]
    fn unresolved_evidence_maps_to_specific_failure_reasons() {
        let expected = [expected_change(
            "missing",
            ExpectedKind::Deletion,
            Some("reviewed text"),
            None,
        )];
        let old = [diagnostic_block(1, "reviewed text")];
        let new = [diagnostic_block(2, "remaining text")];
        let cases = [
            (
                AlignmentEvidence::CandidateSetEmpty,
                ExpectedChangeFailureReason::CandidateNotGenerated,
            ),
            (
                AlignmentEvidence::CandidateScoringRejected,
                ExpectedChangeFailureReason::CandidateScoringRejected,
            ),
            (
                AlignmentEvidence::CandidateCompetition,
                ExpectedChangeFailureReason::AlignmentAmbiguous,
            ),
            (
                AlignmentEvidence::ReadingOrderUnknown,
                ExpectedChangeFailureReason::ReadingOrderUnresolved {
                    side: MissSide::Old,
                },
            ),
            (
                AlignmentEvidence::DiffEditDistanceExceeded,
                ExpectedChangeFailureReason::DiffEditDistanceExceeded,
            ),
            (
                AlignmentEvidence::DiffRejectedAsImplausible,
                ExpectedChangeFailureReason::DiffRejectedAsImplausible,
            ),
        ];
        for (evidence, reason) in cases {
            let comparison = diagnostic_comparison(
                Vec::new(),
                vec![UnresolvedRegion {
                    old_span: Some(diagnostic_span(1, 0, 13)),
                    new_span: None,
                    evidence: vec![evidence],
                }],
            );
            let diagnostics = reviewed_diagnostics(&expected, &old, &new, &comparison, &[]);
            assert_eq!(
                diagnostics.expected_change_diagnostics.failures[0].reason,
                reason
            );
        }

        let duplicate = diagnostic_comparison(
            Vec::new(),
            vec![
                UnresolvedRegion {
                    old_span: Some(diagnostic_span(1, 0, 13)),
                    new_span: None,
                    evidence: vec![AlignmentEvidence::CandidateSetEmpty],
                },
                UnresolvedRegion {
                    old_span: Some(diagnostic_span(1, 0, 13)),
                    new_span: None,
                    evidence: vec![AlignmentEvidence::CandidateSetEmpty],
                },
            ],
        );
        let diagnostics = reviewed_diagnostics(&expected, &old, &new, &duplicate, &[]);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: false,
            }
        );
    }

    #[test]
    fn two_sided_unresolved_cause_requires_one_region_covering_both_quotes() {
        let expected = [expected_change(
            "replace",
            ExpectedKind::Replacement,
            Some("annual fee fifty"),
            Some("annual fee sixty"),
        )];
        let old = [diagnostic_block(1, "annual fee fifty")];
        let new = [diagnostic_block(2, "annual fee sixty")];
        let same_region = diagnostic_comparison(
            Vec::new(),
            vec![UnresolvedRegion {
                old_span: Some(diagnostic_span(1, 0, 17)),
                new_span: Some(diagnostic_span(2, 0, 16)),
                evidence: vec![AlignmentEvidence::DiffEditDistanceExceeded],
            }],
        );
        let diagnostics = reviewed_diagnostics(&expected, &old, &new, &same_region, &[]);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::DiffEditDistanceExceeded
        );

        let separate_regions = diagnostic_comparison(
            Vec::new(),
            vec![
                UnresolvedRegion {
                    old_span: Some(diagnostic_span(1, 0, 17)),
                    new_span: None,
                    evidence: vec![AlignmentEvidence::DiffEditDistanceExceeded],
                },
                UnresolvedRegion {
                    old_span: None,
                    new_span: Some(diagnostic_span(2, 0, 17)),
                    evidence: vec![AlignmentEvidence::DiffEditDistanceExceeded],
                },
            ],
        );
        let diagnostics = reviewed_diagnostics(&expected, &old, &new, &separate_regions, &[]);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: false,
            }
        );

        let missed_expected = [expected_change(
            "miss",
            ExpectedKind::Replacement,
            Some("abcdefghijklmno"),
            Some("zyxwvutsrqponml"),
        )];
        let missed_old = [diagnostic_block(3, "abcdefghijklmno")];
        let missed_new = [diagnostic_block(4, "zyxwvutsrqponml")];
        let unresolved = diagnostic_comparison(
            Vec::new(),
            vec![UnresolvedRegion {
                old_span: Some(diagnostic_span(3, 0, 15)),
                new_span: Some(diagnostic_span(4, 0, 15)),
                evidence: vec![AlignmentEvidence::DiffEditDistanceExceeded],
            }],
        );
        let diagnostics =
            reviewed_diagnostics(&missed_expected, &missed_old, &missed_new, &unresolved, &[]);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::CandidateNotGenerated
        );
    }

    #[test]
    fn expected_change_cap_bounds_diagnostics_without_touching_quality_inputs() {
        let expected = (0..10_000)
            .map(|index| {
                expected_change(
                    &format!("expected-{index}"),
                    ExpectedKind::Replacement,
                    Some("annual fee fifty dollars"),
                    Some("annual fee sixty dollars"),
                )
            })
            .collect::<Vec<_>>();
        let old = [diagnostic_block(1, "annual fee fifty dollars")];
        let new = [diagnostic_block(2, "annual fee sixty dollars")];
        let comparison = diagnostic_comparison(Vec::new(), Vec::new());
        let outcome = match_changes(&expected, &[]);
        let quality_before =
            quality_from_match_outcome(Annotation::Complete, &expected, &[], &outcome);

        let diagnostics = evaluate_reviewed_diagnostics_with_limits(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
            DiagnosticLimits {
                max_expected_changes: 2,
                max_output_records: 2,
                ..DiagnosticLimits::default()
            },
        )
        .expect("expected cap returns a bounded fallback");

        assert!(!diagnostics.expected_change_diagnostics.complete);
        assert_eq!(diagnostics.candidate_recall, None);
        assert_eq!(diagnostics.expected_change_diagnostics.failures.len(), 2);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[1],
            ExpectedChangeFailure {
                expected_id: "expected-2".to_owned(),
                reason: ExpectedChangeFailureReason::AlignmentOrCandidate {
                    diagnostic_limited: true,
                },
            }
        );
        assert_eq!(
            quality_from_match_outcome(Annotation::Complete, &expected, &[], &outcome),
            quality_before
        );
    }

    #[test]
    fn wrong_kind_diagnostic_does_not_change_quality_matching() {
        let expected = [expected_change(
            "replace",
            ExpectedKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
        )];
        let mut actual = actual_change(
            ChangeKind::Move,
            Some("old reviewed text"),
            Some("new reviewed text"),
            Some(17),
            Some(17),
        );
        actual.occurrences[0].semantic_hunks = Some(vec![crate::revisions::ActualSemanticHunk {
            old_text: Some("old reviewed text".to_owned()),
            new_text: Some("new reviewed text".to_owned()),
            old_range: None,
            new_range: None,
            old_atomic_changed_tokens: 17,
            new_atomic_changed_tokens: 17,
            atomic_fragments: Some(vec![
                crate::revisions::ActualAtomicFragment {
                    old_text: "old reviewed text".to_owned(),
                    new_text: String::new(),
                    old_changed_tokens: 17,
                    new_changed_tokens: 0,
                },
                crate::revisions::ActualAtomicFragment {
                    old_text: String::new(),
                    new_text: "new reviewed text".to_owned(),
                    old_changed_tokens: 0,
                    new_changed_tokens: 17,
                },
            ]),
        }]);
        let actuals = [actual];
        let before = compute_quality(Annotation::Complete, &expected, &actuals);
        let outcome = match_changes(&expected, &actuals);
        let after = quality_from_match_outcome(Annotation::Complete, &expected, &actuals, &outcome);
        assert_eq!(after, before);
        let diagnostics = evaluate_reviewed_diagnostics(
            &expected,
            &[diagnostic_block(1, "old reviewed text")],
            &[diagnostic_block(2, "new reviewed text")],
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &diagnostic_comparison(Vec::new(), Vec::new()),
                actual_scopes: None,
            },
            &actuals,
            &outcome,
        )
        .expect("diagnostics succeed");
        assert_eq!(candidate_recall(&diagnostics).annotated_counterparts, 1);
        assert_eq!(candidate_recall(&diagnostics).recalled_counterparts, 1);
        assert_eq!(
            diagnostics.expected_change_diagnostics.expected_matches,
            vec![ExpectedChangeMatchEvaluation {
                expected_id: "replace".to_owned(),
                matched: true,
                actual_index: Some(0),
                actual_kind: Some("move".to_owned()),
                kind_agrees: Some(false),
            }]
        );
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::WrongChangeKind {
                expected: "replacement".to_owned(),
                actual: "move".to_owned(),
                diagnostic: Box::new(WrongChangeKindDiagnostic::Complete {
                    actual_index: 0,
                    quote_matching_occurrences: 1,
                    trace: WrongChangeKindTraceReport::Untraced,
                }),
            }
        );
    }

    #[test]
    fn wrong_kind_diagnostic_reports_recovery_context_without_serializing_text() {
        let expected = expected_change(
            "insert",
            ExpectedKind::Insertion,
            None,
            Some("copyright notice"),
        );
        let mut actual = actual_change(
            ChangeKind::Replacement,
            Some("old relation context"),
            Some("prefix copyright notice suffix"),
            Some(20),
            Some(29),
        );
        let occurrence = &mut actual.occurrences[0];
        occurrence.old_relation_context = Some("old relation context".to_owned());
        occurrence.new_relation_context = Some("prefix copyright notice suffix".to_owned());
        occurrence.old_relation_context_len = Some(20);
        occurrence.new_relation_context_len = Some(29);
        occurrence.old_atomic_changed_tokens = Some(1);
        occurrence.new_atomic_changed_tokens = Some(1);
        occurrence.old_semantic_changed_tokens = Some(0);
        occurrence.new_semantic_changed_tokens = Some(16);
        occurrence.semantic_hunks = Some(vec![crate::revisions::ActualSemanticHunk {
            old_text: Some(String::new()),
            new_text: Some("copyright notice".to_owned()),
            old_range: None,
            new_range: None,
            old_atomic_changed_tokens: 0,
            new_atomic_changed_tokens: 16,
            atomic_fragments: Some(vec![crate::revisions::ActualAtomicFragment {
                old_text: String::new(),
                new_text: "copyright notice".to_owned(),
                old_changed_tokens: 0,
                new_changed_tokens: 16,
            }]),
        }]);
        occurrence.relation_trace =
            ActualRelationTraceStatus::Available(crate::revisions::ActualRelationTrace {
                origin: pdfdelta_core::diff::ChangeOrigin::SentenceNear,
                old_alignment_span_index: 12,
                new_alignment_span_index: 19,
                old_best_score: Some(9_814),
                old_second_score: Some(7_100),
                old_best_scope: Some(RecoveryWatchNearScopeReport::PairedStream),
                new_best_score: Some(9_700),
                new_second_score: Some(7_000),
                new_best_scope: Some(RecoveryWatchNearScopeReport::CrossSpan),
            });

        let diagnostic = wrong_kind_diagnostic(
            &expected,
            3,
            &actual,
            &mut DiagnosticBudget::default(),
            DiagnosticLimits::default(),
        )
        .expect("diagnostic succeeds");

        let WrongChangeKindDiagnostic::Complete {
            actual_index,
            quote_matching_occurrences,
            trace,
        } = &diagnostic
        else {
            panic!("expected a complete diagnostic")
        };
        assert_eq!(*actual_index, 3);
        assert_eq!(*quote_matching_occurrences, 1);
        let WrongChangeKindTraceReport::Available {
            origin,
            old_alignment_span_index,
            new_alignment_span_index,
            old_relation_context_tokens,
            new_relation_context_tokens,
            expected_new_quote_in_relation_context,
            semantic_hunks,
            old_best_score,
            old_second_score,
            old_best_scope,
            new_best_scope,
            ..
        } = trace
        else {
            panic!("expected one available recovery trace")
        };
        assert_eq!(*origin, ChangeOriginReport::SentenceNear);
        assert_eq!(
            (*old_alignment_span_index, *new_alignment_span_index),
            (12, 19)
        );
        assert_eq!(
            (*old_relation_context_tokens, *new_relation_context_tokens),
            (Some(20), Some(29))
        );
        let WrongChangeKindSemanticHunkReport::Available {
            old_reported_semantic_tokens,
            new_reported_semantic_tokens,
            replacement_hunks,
            insertion_only_hunks,
            expected_new_quote_in_semantic_hunk,
            expected_new_quote_in_insertion_only_hunk,
            ..
        } = semantic_hunks
        else {
            panic!("expected available semantic hunk evidence")
        };
        assert_eq!(
            (*old_reported_semantic_tokens, *new_reported_semantic_tokens),
            (Some(0), Some(16))
        );
        assert_eq!((*replacement_hunks, *insertion_only_hunks), (0, 1));
        assert_eq!(*expected_new_quote_in_relation_context, Some(true));
        assert_eq!(*expected_new_quote_in_semantic_hunk, Some(true));
        assert_eq!(*expected_new_quote_in_insertion_only_hunk, Some(true));
        assert_eq!(
            (*old_best_score, *old_second_score),
            (Some(9_814), Some(7_100))
        );
        assert_eq!(
            *old_best_scope,
            Some(RecoveryWatchNearScopeReport::PairedStream)
        );
        assert_eq!(
            *new_best_scope,
            Some(RecoveryWatchNearScopeReport::CrossSpan)
        );
        assert!(
            !serde_json::to_string(&diagnostic)
                .expect("diagnostic serializes")
                .contains("copyright notice")
        );
    }

    #[test]
    fn wrong_kind_diagnostic_selects_the_same_occurrence_as_matching() {
        let expected = expected_change("insert", ExpectedKind::Insertion, None, Some("added"));
        let mut actual = actual_change(
            ChangeKind::Replacement,
            Some("context"),
            Some("added"),
            Some(7),
            Some(5),
        );
        let occurrence = &mut actual.occurrences[0];
        occurrence.old_relation_context = Some("context".to_owned());
        occurrence.new_relation_context = Some("added".to_owned());
        occurrence.old_relation_context_len = Some(7);
        occurrence.new_relation_context_len = Some(5);
        occurrence.semantic_hunks = Some(vec![crate::revisions::ActualSemanticHunk {
            old_text: Some(String::new()),
            new_text: Some("added".to_owned()),
            old_range: None,
            new_range: None,
            old_atomic_changed_tokens: 0,
            new_atomic_changed_tokens: 5,
            atomic_fragments: Some(vec![crate::revisions::ActualAtomicFragment {
                old_text: String::new(),
                new_text: "added".to_owned(),
                old_changed_tokens: 0,
                new_changed_tokens: 5,
            }]),
        }]);
        occurrence.relation_trace =
            ActualRelationTraceStatus::Available(crate::revisions::ActualRelationTrace {
                origin: pdfdelta_core::diff::ChangeOrigin::OrderedAlignment,
                old_alignment_span_index: 4,
                new_alignment_span_index: 4,
                old_best_score: None,
                old_second_score: None,
                old_best_scope: None,
                new_best_score: None,
                new_second_score: None,
                new_best_scope: None,
            });

        let diagnostic = wrong_kind_diagnostic(
            &expected,
            0,
            &actual,
            &mut DiagnosticBudget::default(),
            DiagnosticLimits::default(),
        )
        .expect("diagnostic succeeds");
        assert!(matches!(
            diagnostic,
            WrongChangeKindDiagnostic::Complete {
                trace: WrongChangeKindTraceReport::Available {
                    origin: ChangeOriginReport::OrderedAlignment,
                    semantic_hunks: WrongChangeKindSemanticHunkReport::Available {
                        insertion_only_hunks: 1,
                        expected_new_quote_in_insertion_only_hunk: Some(true),
                        ..
                    },
                    old_best_score: None,
                    ..
                },
                ..
            }
        ));

        let matching_occurrence = actual.occurrences[0].clone();
        actual.occurrences[0].semantic_hunks = Some(vec![crate::revisions::ActualSemanticHunk {
            old_text: Some("old".to_owned()),
            new_text: Some("other".to_owned()),
            old_range: None,
            new_range: None,
            old_atomic_changed_tokens: 3,
            new_atomic_changed_tokens: 5,
            atomic_fragments: Some(vec![crate::revisions::ActualAtomicFragment {
                old_text: "old".to_owned(),
                new_text: "other".to_owned(),
                old_changed_tokens: 3,
                new_changed_tokens: 5,
            }]),
        }]);
        actual.occurrences.push(matching_occurrence);
        let outcome = match_changes(
            std::slice::from_ref(&expected),
            std::slice::from_ref(&actual),
        );
        assert_eq!(outcome.claimed_actual_by_expected, vec![Some(0)]);
        let selected = wrong_kind_diagnostic(
            &expected,
            0,
            &actual,
            &mut DiagnosticBudget::default(),
            DiagnosticLimits::default(),
        )
        .expect("diagnostic succeeds");
        assert!(matches!(
            selected,
            WrongChangeKindDiagnostic::Complete {
                quote_matching_occurrences: 1,
                trace: WrongChangeKindTraceReport::Available {
                    occurrence_index: 1,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn wrong_kind_scan_limit_is_typed_and_marks_outer_diagnostics_incomplete() {
        let expected = [expected_change(
            "insert",
            ExpectedKind::Insertion,
            None,
            Some("target"),
        )];
        let long_text = format!("target {}", "x".repeat(512));
        let mut actual = actual_change(
            ChangeKind::Replacement,
            Some("old"),
            Some(&long_text),
            Some(3),
            Some(long_text.chars().count()),
        );
        actual.occurrences[0].semantic_hunks = Some(vec![crate::revisions::ActualSemanticHunk {
            old_text: Some(String::new()),
            new_text: Some("target".to_owned()),
            old_range: None,
            new_range: None,
            old_atomic_changed_tokens: 0,
            new_atomic_changed_tokens: 6,
            atomic_fragments: Some(vec![crate::revisions::ActualAtomicFragment {
                old_text: String::new(),
                new_text: "target".to_owned(),
                old_changed_tokens: 0,
                new_changed_tokens: 6,
            }]),
        }]);
        let actuals = [actual];
        let outcome = match_changes(&expected, &actuals);
        let diagnostics = evaluate_reviewed_diagnostics_with_limits(
            &expected,
            &[diagnostic_block(1, "old")],
            &[diagnostic_block(2, "target")],
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &diagnostic_comparison(Vec::new(), Vec::new()),
                actual_scopes: None,
            },
            &actuals,
            &outcome,
            DiagnosticLimits::default().with_max_scan_work(128),
        )
        .expect("diagnostic limit is not a pair failure");

        assert!(!diagnostics.expected_change_diagnostics.complete);
        let ExpectedChangeFailureReason::WrongChangeKind { diagnostic, .. } =
            &diagnostics.expected_change_diagnostics.failures[0].reason
        else {
            panic!("expected a wrong-kind failure")
        };
        assert!(matches!(
            diagnostic.as_ref(),
            WrongChangeKindDiagnostic::Limited {
                actual_index: 0,
                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
            }
        ));
    }

    #[test]
    fn wrong_kind_unavailable_projection_is_not_reported_as_zero_or_false() {
        let expected = expected_change("insert", ExpectedKind::Insertion, None, Some("added"));
        let mut actual = actual_change(
            ChangeKind::Insertion,
            Some("old"),
            Some("added"),
            Some(3),
            Some(5),
        );
        let occurrence = &mut actual.occurrences[0];
        occurrence.relation_trace =
            ActualRelationTraceStatus::Available(crate::revisions::ActualRelationTrace {
                origin: pdfdelta_core::diff::ChangeOrigin::OrderedAlignment,
                old_alignment_span_index: 1,
                new_alignment_span_index: 1,
                old_best_score: None,
                old_second_score: None,
                old_best_scope: None,
                new_best_score: None,
                new_second_score: None,
                new_best_scope: None,
            });

        let diagnostic = wrong_kind_diagnostic(
            &expected,
            0,
            &actual,
            &mut DiagnosticBudget::default(),
            DiagnosticLimits::default(),
        )
        .expect("diagnostic succeeds");
        assert!(matches!(
            diagnostic,
            WrongChangeKindDiagnostic::Complete {
                trace: WrongChangeKindTraceReport::Available {
                    old_relation_context_tokens: None,
                    new_relation_context_tokens: None,
                    expected_new_quote_in_relation_context: None,
                    semantic_hunks: WrongChangeKindSemanticHunkReport::Unavailable,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn wrong_kind_multi_occurrence_limit_publishes_no_partial_count() {
        let expected = expected_change("insert", ExpectedKind::Insertion, None, Some("target"));
        let mut actual = actual_change(
            ChangeKind::Replacement,
            Some("old"),
            Some("target"),
            Some(3),
            Some(6),
        );
        let mut second = actual.occurrences[0].clone();
        second.new_text = Some(format!("target {}", "x".repeat(512)));
        actual.occurrences.push(second);
        let mut budget = DiagnosticBudget::default();
        let diagnostic = wrong_kind_diagnostic(
            &expected,
            4,
            &actual,
            &mut budget,
            DiagnosticLimits::default().with_max_scan_work(128),
        )
        .expect("limit returns a typed diagnostic");

        assert!(budget.limited);
        assert_eq!(
            diagnostic,
            WrongChangeKindDiagnostic::Limited {
                actual_index: 4,
                stop_reason: WrongChangeKindDiagnosticStopReason::ScanLimit,
            }
        );
    }

    #[test]
    fn occurrence_count_mismatch_precedes_generic_failure_diagnostics() {
        let mut expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
        );
        expected.occurrence_count = Some(2);
        let actuals = [actual_change(
            ChangeKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
            Some(17),
            Some(17),
        )];
        let diagnostics = reviewed_diagnostics(
            &[expected],
            &[diagnostic_block(1, "old reviewed text")],
            &[diagnostic_block(2, "new reviewed text")],
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &actuals,
        );

        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::OccurrenceCountMismatch {
                expected: 2,
                actual: 1,
            }
        );
    }

    #[test]
    fn occurrence_count_mismatch_requires_a_matching_scope() {
        let mut expected = expected_change(
            "repeated",
            ExpectedKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
        );
        expected.scope = Some("reviewed".to_owned());
        expected.occurrence_count = Some(2);
        let expected = [expected];
        let actuals = [actual_change(
            ChangeKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
            Some(17),
            Some(17),
        )];
        let old = [diagnostic_block(1, "old reviewed text")];
        let new = [diagnostic_block(2, "new reviewed text")];
        let comparison = diagnostic_comparison(Vec::new(), Vec::new());
        let outside_scopes = [Some("outside".to_owned())];
        let outcome = match_changes_with_scopes(&expected, &actuals, Some(&outside_scopes));
        let outside = evaluate_reviewed_diagnostics(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &comparison,
                actual_scopes: Some(&outside_scopes),
            },
            &actuals,
            &outcome,
        )
        .expect("diagnostics succeed");
        assert_ne!(
            outside.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::OccurrenceCountMismatch {
                expected: 2,
                actual: 1,
            }
        );

        let reviewed_scopes = [Some("reviewed".to_owned())];
        let outcome = match_changes_with_scopes(&expected, &actuals, Some(&reviewed_scopes));
        let reviewed = evaluate_reviewed_diagnostics(
            &expected,
            &old,
            &new,
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &comparison,
                actual_scopes: Some(&reviewed_scopes),
            },
            &actuals,
            &outcome,
        )
        .expect("diagnostics succeed");
        assert_eq!(
            reviewed.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::OccurrenceCountMismatch {
                expected: 2,
                actual: 1,
            }
        );
    }

    #[test]
    fn occurrence_count_mismatch_ignores_actuals_claimed_by_other_expectations() {
        let exact = expected_change(
            "exact",
            ExpectedKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
        );
        let mut repeated = exact.clone();
        repeated.id = "repeated".to_owned();
        repeated.occurrence_count = Some(2);
        let expected = [exact, repeated];
        let actuals = [actual_change(
            ChangeKind::Replacement,
            Some("old reviewed text"),
            Some("new reviewed text"),
            Some(17),
            Some(17),
        )];
        let diagnostics = reviewed_diagnostics(
            &expected,
            &[diagnostic_block(1, "old reviewed text")],
            &[diagnostic_block(2, "new reviewed text")],
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &actuals,
        );

        assert_eq!(diagnostics.expected_change_diagnostics.failures.len(), 1);
        assert_ne!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::OccurrenceCountMismatch {
                expected: 2,
                actual: 1,
            }
        );
    }

    #[test]
    fn fragmentation_requires_consecutive_hunks_covering_the_entire_quote() {
        let expected = [expected_change(
            "delete",
            ExpectedKind::Deletion,
            Some("abcdef"),
            None,
        )];
        let old = [diagnostic_block(1, "abcdef")];
        let new = [diagnostic_block(2, "remaining")];
        let complete = diagnostic_comparison(
            vec![
                diagnostic_change(ChangeKind::Deletion, Some(diagnostic_span(1, 0, 3)), None),
                diagnostic_change(ChangeKind::Deletion, Some(diagnostic_span(1, 3, 6)), None),
            ],
            Vec::new(),
        );
        let diagnostics = reviewed_diagnostics(&expected, &old, &new, &complete, &[]);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::FragmentedAcrossHunks {
                old_hunks: 2,
                new_hunks: 0,
            }
        );

        let gap = diagnostic_comparison(
            vec![
                diagnostic_change(ChangeKind::Deletion, Some(diagnostic_span(1, 0, 2)), None),
                diagnostic_change(ChangeKind::Deletion, Some(diagnostic_span(1, 3, 6)), None),
            ],
            Vec::new(),
        );
        let diagnostics = reviewed_diagnostics(&expected, &old, &new, &gap, &[]);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: false,
            }
        );
    }

    #[test]
    fn move_fragmentation_compares_hunk_sets_when_side_order_is_reversed() {
        let expected = expected_change("move", ExpectedKind::Move, Some("abcdef"), Some("uvwxyz"));
        let comparison = diagnostic_comparison(
            vec![
                diagnostic_change(
                    ChangeKind::Move,
                    Some(diagnostic_span(1, 0, 3)),
                    Some(diagnostic_span(2, 3, 6)),
                ),
                diagnostic_change(
                    ChangeKind::Move,
                    Some(diagnostic_span(1, 3, 6)),
                    Some(diagnostic_span(2, 0, 3)),
                ),
            ],
            Vec::new(),
        );
        let locations = ExpectedQuoteLocations {
            old: Some(QuoteLocateOutcome::Unique(QuoteLocation {
                block: BlockId(1),
                scalar_range: ScalarRange { start: 0, end: 6 },
            })),
            new: Some(QuoteLocateOutcome::Unique(QuoteLocation {
                block: BlockId(2),
                scalar_range: ScalarRange { start: 0, end: 6 },
            })),
        };
        let mut budget = DiagnosticBudget::default();
        assert_eq!(
            fragmentation_reason(
                &expected,
                &comparison,
                &locations,
                &HashSet::new(),
                &mut budget,
                DiagnosticLimits::default(),
            )
            .expect("fragmentation scan"),
            Some(ExpectedChangeFailureReason::FragmentedAcrossHunks {
                old_hunks: 2,
                new_hunks: 2,
            })
        );
    }

    #[test]
    fn absence_segmentation_ambiguity_and_caps_fail_closed() {
        let empty_comparison = diagnostic_comparison(Vec::new(), Vec::new());
        for (kind, old_quote, new_quote, side) in [
            (ExpectedKind::Deletion, Some("missing"), None, MissSide::Old),
            (
                ExpectedKind::Insertion,
                None,
                Some("missing"),
                MissSide::New,
            ),
            (
                ExpectedKind::Replacement,
                Some("missing old"),
                Some("missing new"),
                MissSide::Both,
            ),
        ] {
            let diagnostics = reviewed_diagnostics(
                &[expected_change("missing", kind, old_quote, new_quote)],
                &[diagnostic_block(1, "old other")],
                &[diagnostic_block(2, "new other")],
                &empty_comparison,
                &[],
            );
            assert_eq!(
                diagnostics.expected_change_diagnostics.failures[0].reason,
                ExpectedChangeFailureReason::QuoteNotExtracted { side }
            );
        }

        let segmented = reviewed_diagnostics(
            &[expected_change(
                "segmented",
                ExpectedKind::Deletion,
                Some("split quote"),
                None,
            )],
            &[diagnostic_block(1, "split"), diagnostic_block(2, "quote")],
            &[diagnostic_block(3, "other")],
            &empty_comparison,
            &[],
        );
        assert_eq!(
            segmented.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::UnitSegmentationFailure {
                side: MissSide::Old,
            }
        );

        let ambiguous = reviewed_diagnostics(
            &[expected_change(
                "ambiguous",
                ExpectedKind::Deletion,
                Some("duplicate"),
                None,
            )],
            &[
                diagnostic_block(1, "duplicate"),
                diagnostic_block(2, "duplicate"),
            ],
            &[diagnostic_block(3, "other")],
            &empty_comparison,
            &[],
        );
        assert_eq!(
            ambiguous.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: false,
            }
        );

        let mixed = reviewed_diagnostics(
            &[expected_change(
                "mixed",
                ExpectedKind::Replacement,
                Some("missing old"),
                Some("split new"),
            )],
            &[diagnostic_block(1, "other old")],
            &[diagnostic_block(2, "split"), diagnostic_block(3, "new")],
            &empty_comparison,
            &[],
        );
        assert_eq!(
            mixed.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: false,
            }
        );

        let expected = [expected_change(
            "limited",
            ExpectedKind::Deletion,
            Some("too long"),
            None,
        )];
        let outcome = match_changes(&expected, &[]);
        let limited = evaluate_reviewed_diagnostics_with_limits(
            &expected,
            &[diagnostic_block(1, "too long")],
            &[diagnostic_block(2, "other")],
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &empty_comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
            DiagnosticLimits {
                max_quote_chars: 3,
                ..DiagnosticLimits::default()
            },
        )
        .expect("bounded diagnostics return a fallback");
        assert!(!limited.expected_change_diagnostics.complete);
        assert_eq!(
            limited.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            }
        );

        let expected = [
            expected_change("first", ExpectedKind::Deletion, Some("first missing"), None),
            expected_change(
                "second",
                ExpectedKind::Deletion,
                Some("second missing"),
                None,
            ),
        ];
        let outcome = match_changes(&expected, &[]);
        let output_limited = evaluate_reviewed_diagnostics_with_limits(
            &expected,
            &[diagnostic_block(1, "other")],
            &[diagnostic_block(2, "remaining")],
            ComparisonDiagnosticInput {
                alignment: None,
                comparison: &empty_comparison,
                actual_scopes: None,
            },
            &[],
            &outcome,
            DiagnosticLimits {
                max_output_records: 1,
                ..DiagnosticLimits::default()
            },
        )
        .expect("output cap returns a fallback");
        assert!(!output_limited.expected_change_diagnostics.complete);
        assert_eq!(output_limited.expected_change_diagnostics.failures.len(), 1);
        assert!(matches!(
            output_limited.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true
            }
        ));
    }
}
