use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};

use pdfdelta_core::{
    alignment::{
        Alignment, AlignmentEvidence, AlignmentKind, BlockFeatures, BlockSeparator,
        CandidateGenerator, InvertedIndexCandidateGenerator, build_block_features,
    },
    diff::{Comparison, TextSpan},
    layout::BlockId,
    normalize::{BlockText, ComparableToken, ScalarRange},
    pipeline::PipelineOptions,
};

use super::{
    ActualChange, CandidateRecallMetrics, ExpectedChange, ExpectedChangeDiagnostics,
    ExpectedChangeFailure, ExpectedChangeFailureReason, ExpectedKind, MatchOutcome, MissSide,
    build_block_map, change_kind_name, is_space_token, ratio,
};

#[derive(Clone, Copy)]
struct DiagnosticLimits {
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
            max_expected_changes: 4_096,
            max_output_records: 4_096,
        }
    }
}

#[derive(Default)]
struct DiagnosticBudget {
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
struct QuoteLocation {
    block: BlockId,
    scalar_range: ScalarRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum QuoteLocateOutcome {
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
                pending_space = None;
                segment_has_text = false;
                if !matches!(items.last(), Some(LocatedItem::Barrier)) {
                    items.push(LocatedItem::Barrier);
                }
            }
        }
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

fn record_occurrence(
    occurrence: RawOccurrence,
    blocks: &[BlockText],
    normalized_blocks: &[Vec<LocatedItem>],
    first: &mut Option<(RawOccurrence, QuoteLocateOutcome)>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<QuoteLocateOutcome>> {
    if first.as_ref().is_some_and(|(seen, _)| *seen == occurrence) {
        return Ok(None);
    }
    budget.charge_occurrence(limits)?;
    let outcome = occurrence_outcome(occurrence, blocks, normalized_blocks)?;
    if first.is_some() {
        return Ok(Some(QuoteLocateOutcome::Ambiguous));
    }
    *first = Some((occurrence, outcome));
    Ok(None)
}

fn scan_quote_matches(
    blocks: &[BlockText],
    normalized_blocks: &[Vec<LocatedItem>],
    needle: &[char],
    prefix: &[usize],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<Option<QuoteLocateOutcome>> {
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
                )? && let Some(outcome) = record_occurrence(
                    occurrence,
                    blocks,
                    normalized_blocks,
                    &mut first,
                    budget,
                    limits,
                )? {
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
                )? && let Some(outcome) = record_occurrence(
                    occurrence,
                    blocks,
                    normalized_blocks,
                    &mut first,
                    budget,
                    limits,
                )? {
                    return Ok(Some(outcome));
                }
                advanced.push(state);
            }
            states = advanced;
        }
    }
    Ok(first.map(|(_, outcome)| outcome))
}

fn locate_quote(
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
            && span.separator == Some(BlockSeparator::Space)
            && !previous_last.as_ref().is_some_and(is_space_token)
            && !tokens.first().is_some_and(is_space_token)
        {
            scalar_offset = budget.checked_add(scalar_offset, 1)?;
        }
        if *block_id == location.block {
            let start = budget.checked_add(scalar_offset, location.scalar_range.start)?;
            let end = budget.checked_add(scalar_offset, location.scalar_range.end)?;
            return Ok(span.canonical_range.start <= start && end <= span.canonical_range.end);
        }
        let scalar_count = tokens.iter().filter(|token| token.is_scalar()).count();
        scalar_offset = budget.checked_add(scalar_offset, scalar_count)?;
        previous_last = tokens.last().cloned();
    }
    Ok(false)
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

#[derive(Clone, Copy)]
struct ComparisonDiagnosticInput<'a> {
    alignment: Option<&'a Alignment>,
    comparison: &'a Comparison,
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
    if candidate == ReviewedCandidateResult::Missed {
        return Ok(ExpectedChangeFailureReason::CandidateNotGenerated);
    }
    if candidate == ReviewedCandidateResult::Recalled {
        if context.alignment_index_limited {
            return Ok(ExpectedChangeFailureReason::AlignmentOrCandidate {
                diagnostic_limited: true,
            });
        }
        if let Some(reason) = alignment_failure_reason(context.alignment_index, locations) {
            return Ok(reason);
        }
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
    } = input;
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
            },
            None => classify_expected_failure(
                change,
                candidate_results[index],
                &locations[index],
                &mut context,
            )?,
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
    Ok(ReviewedDiagnostics {
        candidate_recall,
        expected_change_diagnostics: ExpectedChangeDiagnostics {
            complete: !context.budget.limited,
            failures,
        },
    })
}

pub(super) fn evaluate_reviewed_diagnostics(
    expected: &[ExpectedChange],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    alignment: Option<&Alignment>,
    comparison: &Comparison,
    actuals: &[ActualChange],
    outcome: &MatchOutcome,
) -> std::result::Result<ReviewedDiagnostics, String> {
    evaluate_reviewed_diagnostics_with_limits(
        expected,
        old_blocks,
        new_blocks,
        ComparisonDiagnosticInput {
            alignment,
            comparison,
        },
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
        model::FontProgramHash,
        normalize::{
            MappedText, NormalizationIssue, NormalizationIssueKind, TextSource, UnmappedToken,
        },
    };

    use super::super::{Annotation, compute_quality, match_changes, quality_from_match_outcome};
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
            old_quote: old.map(str::to_owned),
            new_quote: new.map(str::to_owned),
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
            occurrences: vec![crate::revisions::ActualChangeOccurrence {
                old_text: old_text.map(str::to_owned),
                new_text: new_text.map(str::to_owned),
                old_comparable_len: old_len,
                new_comparable_len: new_len,
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
            expected, old_blocks, new_blocks, None, comparison, actuals, &outcome,
        )
        .expect("diagnostics succeed")
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
            Some(&alignment),
            &comparison,
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
        let comparison = diagnostic_comparison(Vec::new(), Vec::new());
        let outcome = match_changes(&expected, &[]);

        let diagnostics = evaluate_reviewed_diagnostics(
            &expected,
            &old,
            &new,
            Some(&alignment),
            &comparison,
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
    fn quote_locator_treats_unmapped_tokens_as_hard_barriers_and_issues_as_indeterminate() {
        let mut unmapped = diagnostic_block(1, "abcdef");
        unmapped.canonical.unmapped.push(UnmappedToken {
            scalar_index: 3,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 7,
            source: TextSource { atoms: Vec::new() },
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
            source: TextSource { atoms: Vec::new() },
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
        let actuals = [actual_change(
            ChangeKind::Move,
            Some("old reviewed text"),
            Some("new reviewed text"),
            Some(17),
            Some(17),
        )];
        let before = compute_quality(Annotation::Complete, &expected, &actuals);
        let outcome = match_changes(&expected, &actuals);
        let after = quality_from_match_outcome(Annotation::Complete, &expected, &actuals, &outcome);
        assert_eq!(after, before);
        let diagnostics = evaluate_reviewed_diagnostics(
            &expected,
            &[diagnostic_block(1, "old reviewed text")],
            &[diagnostic_block(2, "new reviewed text")],
            None,
            &diagnostic_comparison(Vec::new(), Vec::new()),
            &actuals,
            &outcome,
        )
        .expect("diagnostics succeed");
        assert_eq!(candidate_recall(&diagnostics).annotated_counterparts, 1);
        assert_eq!(candidate_recall(&diagnostics).recalled_counterparts, 1);
        assert_eq!(
            diagnostics.expected_change_diagnostics.failures[0].reason,
            ExpectedChangeFailureReason::WrongChangeKind {
                expected: "replacement".to_owned(),
                actual: "move".to_owned(),
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
