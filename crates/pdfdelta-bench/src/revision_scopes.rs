use std::collections::{BTreeMap, HashMap};

#[cfg(test)]
use pdfdelta_core::diff::ChangeOrigin;
use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::{Change, ProvenChangedRegion, RecoveredAtomicDiff, TextSpan, TokenRange},
    layout::BlockId,
    normalize::{BlockText, ComparableToken, ScalarRange},
};

use super::{
    ExpectedChange, ExpectedChangedRange, ExpectedScope, ScopedTokenMetrics, collapse_whitespace,
    revision_diagnostics::{
        DiagnosticBudget, DiagnosticLimits, ScopedQuoteLocateOutcome, ScopedQuoteLocation,
        ScopedQuoteRange, locate_scope_anchor_quote, locate_scoped_quote,
    },
};

const SCOPE_RESOLUTION_LIMITED: &str =
    "scoped-complete scope anchor resolution reached its resource limit";
const SCOPE_RESOLUTION_INDETERMINATE: &str =
    "scoped-complete scope anchor resolution is indeterminate";
pub(super) const SCOPED_CHANGE_INDETERMINATE: &str =
    "scoped-complete reported change coordinates are indeterminate";
pub(super) const SCOPED_CHANGE_LIMITED: &str =
    "scoped-complete reported change classification reached its resource limit";
const SCOPED_EXPECTED_CHANGE_LIMITED: &str =
    "scoped-complete expected change quote resolution reached its resource limit";
const SCOPED_TOKEN_METRICS_INDETERMINATE: &str = "scoped-complete token metrics are indeterminate";
const SCOPED_TOKEN_METRICS_LIMITED: &str =
    "scoped-complete token metrics reached its resource limit";

fn token_metrics_error(error: String) -> String {
    if error == SCOPED_CHANGE_LIMITED {
        SCOPED_TOKEN_METRICS_LIMITED.to_owned()
    } else {
        error
    }
}

#[derive(Clone, Copy)]
struct ClassificationLimits {
    max_work: usize,
    max_output: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TokenInterval {
    pub(super) block_order: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CollapsedScalarMapping {
    value: char,
    interval_start: usize,
    interval_end: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct CollapsedContextMapping {
    scalars: Vec<CollapsedScalarMapping>,
    intervals: Vec<TokenInterval>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ScopedExpectedTokenEvidence {
    pub(super) old: Vec<TokenInterval>,
    pub(super) new: Vec<TokenInterval>,
}

impl Default for ClassificationLimits {
    fn default() -> Self {
        Self {
            max_work: 64_000_000,
            max_output: 65_536,
        }
    }
}

#[derive(Default)]
struct ClassificationBudget {
    work: usize,
    output: usize,
}

impl ClassificationBudget {
    fn charge_work(&mut self, amount: usize, limits: ClassificationLimits) -> Result<(), String> {
        self.work = self
            .work
            .checked_add(amount)
            .filter(|work| *work <= limits.max_work)
            .ok_or_else(|| SCOPED_CHANGE_LIMITED.to_owned())?;
        Ok(())
    }

    fn charge_output(&mut self, limits: ClassificationLimits) -> Result<(), String> {
        self.output = self
            .output
            .checked_add(1)
            .filter(|output| *output <= limits.max_output)
            .ok_or_else(|| SCOPED_CHANGE_LIMITED.to_owned())?;
        Ok(())
    }

    fn checked_add(&mut self, left: usize, right: usize) -> Result<usize, String> {
        left.checked_add(right)
            .ok_or_else(|| SCOPED_CHANGE_LIMITED.to_owned())
    }
}

#[derive(Default)]
struct SpanProjection {
    comparable_offset: usize,
    scalar_offset: usize,
    previous_coordinate: Option<ScopeCoordinate>,
    last_source_coordinate: Option<ScopeCoordinate>,
    first_selected: Option<ScopeCoordinate>,
    last_selected: Option<ScopeCoordinate>,
    last_selected_coordinate: Option<ScopeCoordinate>,
    last_selected_synthetic: bool,
}

impl SpanProjection {
    fn record_selected_coordinate(&mut self, coordinate: ScopeCoordinate) -> Result<(), String> {
        if let Some(previous) = self.last_selected_coordinate
            && !selected_coordinate_follows(previous, coordinate, self.last_source_coordinate)
        {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        self.last_selected_coordinate = Some(coordinate);
        Ok(())
    }

    fn project(
        &mut self,
        coordinate: Option<ScopeCoordinate>,
        scalar: bool,
        span: &TextSpan,
        budget: &mut ClassificationBudget,
        limits: ClassificationLimits,
    ) -> Result<(), String> {
        if self.comparable_offset == span.comparable_range.start
            && self.scalar_offset != span.canonical_range.start
        {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        let selected = span.comparable_range.start <= self.comparable_offset
            && self.comparable_offset < span.comparable_range.end;
        self.comparable_offset = budget.checked_add(self.comparable_offset, 1)?;
        let closes_synthetic = self.last_selected_synthetic;
        if closes_synthetic {
            self.last_selected =
                Some(coordinate.ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?);
            self.record_selected_coordinate(
                coordinate.ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?,
            )?;
            self.last_selected_synthetic = false;
        }
        if selected {
            if let Some(coordinate) = coordinate {
                self.first_selected.get_or_insert(coordinate);
                self.last_selected = Some(coordinate);
                self.last_selected_synthetic = false;
                if !closes_synthetic {
                    self.record_selected_coordinate(coordinate)?;
                }
            } else if scalar {
                // A virtual separator has no source scalar. Both immediate
                // neighbors must bound its scope; neither becomes a changed token.
                let previous = self
                    .previous_coordinate
                    .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?;
                self.first_selected.get_or_insert(previous);
                if self
                    .last_selected_coordinate
                    .is_some_and(|last| last != previous)
                {
                    return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
                }
                self.last_selected_coordinate = Some(previous);
                self.last_selected = Some(previous);
                self.last_selected_synthetic = true;
            }
        }
        self.previous_coordinate = coordinate;
        if coordinate.is_some() {
            self.last_source_coordinate = coordinate;
        }
        if scalar {
            self.scalar_offset = budget.checked_add(self.scalar_offset, 1)?;
        }
        if self.comparable_offset == span.comparable_range.end
            && self.scalar_offset != span.canonical_range.end
        {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        budget.charge_work(1, limits)
    }
}

fn selected_coordinate_follows(
    previous: ScopeCoordinate,
    current: ScopeCoordinate,
    last_source: Option<ScopeCoordinate>,
) -> bool {
    (current.block_order == previous.block_order && current.scalar > previous.scalar)
        || (previous.block_order < current.block_order
            && current.scalar == 0
            && last_source == Some(previous))
}

fn selected_token_follows(
    previous_block: usize,
    previous_token: usize,
    current_block: usize,
    current_token: usize,
    last_source: Option<(usize, usize)>,
) -> bool {
    (current_block == previous_block && previous_token.checked_add(1) == Some(current_token))
        || (previous_block < current_block
            && current_token == 0
            && last_source == Some((previous_block, previous_token)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ScopeCoordinate {
    pub(super) block_order: usize,
    pub(super) scalar: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ResolvedScopeRange {
    pub(super) start: ScopeCoordinate,
    pub(super) end: ScopeCoordinate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedScope {
    pub(super) id: String,
    pub(super) old: ResolvedScopeRange,
    pub(super) new: ResolvedScopeRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScopedChange {
    pub(super) scope_id: String,
    pub(super) change_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScopedProvenChangedRegion {
    pub(super) scope_id: String,
    pub(super) region_index: usize,
}

fn block_order(
    blocks: &[BlockText],
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> Result<HashMap<BlockId, usize>, String> {
    let mut order = HashMap::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        if !budget.charge_scope_scan(limits) {
            return Err(SCOPE_RESOLUTION_LIMITED.to_owned());
        }
        if order.insert(block.block, index).is_some() {
            return Err(SCOPE_RESOLUTION_INDETERMINATE.to_owned());
        }
    }
    Ok(order)
}

fn anchor_unavailable(scope: &str, side: &str, anchor: &str, outcome: &str) -> String {
    format!("scoped-complete scope {scope:?} {side} {anchor} anchor is {outcome}")
}

struct SideResolver<'a> {
    side: &'static str,
    blocks: &'a [BlockText],
    order: &'a HashMap<BlockId, usize>,
    budget: &'a mut DiagnosticBudget,
    limits: DiagnosticLimits,
}

impl SideResolver<'_> {
    fn resolve_anchor(
        &mut self,
        scope: &str,
        anchor: &str,
        quote: &str,
    ) -> Result<ScopedQuoteLocation, String> {
        match locate_scope_anchor_quote(self.blocks, quote, self.budget, self.limits) {
            Ok(ScopedQuoteLocateOutcome::Unique(location))
                if [location.start_block, location.end_block]
                    .into_iter()
                    .all(|index| {
                        self.blocks.get(index).is_some_and(|block| {
                            self.order.get(&block.block).copied() == Some(index)
                        })
                    }) =>
            {
                Ok(location)
            }
            Ok(ScopedQuoteLocateOutcome::Unique(_))
            | Ok(ScopedQuoteLocateOutcome::Indeterminate)
            | Err(_) => Err(anchor_unavailable(
                scope,
                self.side,
                anchor,
                "indeterminate",
            )),
            Ok(ScopedQuoteLocateOutcome::Missing) => {
                Err(anchor_unavailable(scope, self.side, anchor, "missing"))
            }
            Ok(ScopedQuoteLocateOutcome::Ambiguous) => {
                Err(anchor_unavailable(scope, self.side, anchor, "ambiguous"))
            }
            Ok(ScopedQuoteLocateOutcome::Limited) => Err(SCOPE_RESOLUTION_LIMITED.to_owned()),
        }
    }

    fn resolve_range(&mut self, scope: &ExpectedScope) -> Result<ResolvedScopeRange, String> {
        let anchors = if self.side == "old" {
            &scope.old
        } else {
            &scope.new
        };
        let start = self.resolve_anchor(&scope.id, "start", &anchors.start_quote)?;
        let end = self.resolve_anchor(&scope.id, "end", &anchors.end_quote)?;
        let start_begin = ScopeCoordinate {
            block_order: start.start_block,
            scalar: start.start_scalar,
        };
        let start_end = ScopeCoordinate {
            block_order: start.end_block,
            scalar: start.end_scalar.checked_sub(1).ok_or_else(|| {
                anchor_unavailable(&scope.id, self.side, "start", "indeterminate")
            })?,
        };
        let end_begin = ScopeCoordinate {
            block_order: end.start_block,
            scalar: end.start_scalar,
        };
        let end_end = ScopeCoordinate {
            block_order: end.end_block,
            scalar: end
                .end_scalar
                .checked_sub(1)
                .ok_or_else(|| anchor_unavailable(&scope.id, self.side, "end", "indeterminate"))?,
        };
        if start_begin > end_begin || start_end > end_end {
            return Err(format!(
                "scoped-complete scope {:?} has reversed {} anchors",
                scope.id, self.side
            ));
        }
        let range = ResolvedScopeRange {
            start: start_begin,
            end: end_end,
        };
        // Metrics claim complete review coverage, so every included block must be
        // trusted even when no expected quote selects evidence from that block.
        for block_order in range.start.block_order..=range.end.block_order {
            if !self.budget.charge_scope_scan(self.limits) {
                return Err(SCOPE_RESOLUTION_LIMITED.to_owned());
            }
            let block = self
                .blocks
                .get(block_order)
                .ok_or_else(|| SCOPE_RESOLUTION_INDETERMINATE.to_owned())?;
            if !block.issues.is_empty()
                || !block.raw.unmapped.is_empty()
                || !block.canonical.unmapped.is_empty()
            {
                return Err(SCOPE_RESOLUTION_INDETERMINATE.to_owned());
            }
        }
        Ok(range)
    }
}

fn resolve_revision_scopes_with_limits(
    scopes: &[ExpectedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    limits: DiagnosticLimits,
) -> Result<Vec<ResolvedScope>, String> {
    let mut budget = DiagnosticBudget::default();
    let old_order = block_order(old_blocks, &mut budget, limits)?;
    let new_order = block_order(new_blocks, &mut budget, limits)?;
    let mut resolved = Vec::<ResolvedScope>::new();
    for scope in scopes {
        if !budget.charge_scope(limits) {
            return Err(SCOPE_RESOLUTION_LIMITED.to_owned());
        }
        let old = SideResolver {
            side: "old",
            blocks: old_blocks,
            order: &old_order,
            budget: &mut budget,
            limits,
        }
        .resolve_range(scope)?;
        let new = SideResolver {
            side: "new",
            blocks: new_blocks,
            order: &new_order,
            budget: &mut budget,
            limits,
        }
        .resolve_range(scope)?;
        if let Some(previous) = resolved.last() {
            if previous.old.end >= old.start {
                return Err(
                    "scoped-complete scopes are not strictly ordered and disjoint on old side"
                        .to_owned(),
                );
            }
            if previous.new.end >= new.start {
                return Err(
                    "scoped-complete scopes are not strictly ordered and disjoint on new side"
                        .to_owned(),
                );
            }
        }
        resolved.push(ResolvedScope {
            id: scope.id.clone(),
            old,
            new,
        });
    }
    Ok(resolved)
}

pub(super) fn resolve_revision_scopes(
    scopes: &[ExpectedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
) -> Result<Vec<ResolvedScope>, String> {
    resolve_revision_scopes_with_limits(scopes, old_blocks, new_blocks, DiagnosticLimits::default())
}

/// Returns the bounded source view used by scoped evaluation.
///
/// A PDF content stream can paint one horizontal footer from right to left even
/// when the source text reads left to right. Only a terminal, single-page band
/// with complete horizontal glyph geometry is eligible for local reordering.
/// The comparison's blocks remain untouched; every returned block retains its
/// original identity and source evidence.
pub(super) fn evaluation_block_views(
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
) -> [Vec<BlockText>; 2] {
    [
        reorder_terminal_bands(old_blocks),
        reorder_terminal_bands(new_blocks),
    ]
}

fn reorder_terminal_bands(blocks: &[BlockText]) -> Vec<BlockText> {
    let mut by_page = BTreeMap::<u32, Vec<usize>>::new();
    for (index, block) in blocks.iter().enumerate() {
        for page in &block.pages {
            by_page.entry(*page).or_default().push(index);
        }
    }
    let mut reordered = blocks.to_vec();
    for indices in by_page.into_values() {
        let Some(mut members) = certified_terminal_band(blocks, &indices) else {
            continue;
        };
        if members.len() < 2 {
            continue;
        }
        let mut target_indices = members.clone();
        target_indices.sort_unstable();
        members.sort_unstable_by(|&left, &right| {
            let left_x = first_position_x(&blocks[left]);
            let right_x = first_position_x(&blocks[right]);
            left_x.total_cmp(&right_x).then_with(|| left.cmp(&right))
        });
        for (target, source) in target_indices.into_iter().zip(members) {
            reordered[target] = blocks[source].clone();
        }
    }
    reordered
}

fn certified_terminal_band(blocks: &[BlockText], indices: &[usize]) -> Option<Vec<usize>> {
    if indices.iter().any(|&index| {
        blocks[index].pages.len() != 1
            || blocks[index].position_signatures.is_none()
            || blocks[index].font_size_signatures.is_none()
    }) {
        return None;
    }
    let lowest = indices
        .iter()
        .flat_map(|&index| {
            blocks[index]
                .position_signatures
                .as_ref()
                .expect("page geometry was checked above")
                .iter()
                .map(|position| position.baseline().y)
        })
        .min_by(f64::total_cmp)?;
    let smallest_font = indices
        .iter()
        .flat_map(|&index| {
            blocks[index]
                .font_size_signatures
                .as_ref()
                .expect("page geometry was checked above")
                .iter()
                .flat_map(|signature| signature.values())
        })
        .min_by(f64::total_cmp)?;
    let band = smallest_font * 0.5;
    if !band.is_finite() || band <= 0.0 {
        return None;
    }
    let mut members = Vec::new();
    for &index in indices {
        let block = &blocks[index];
        let positions = block
            .position_signatures
            .as_ref()
            .expect("page geometry was checked above");
        if !positions
            .iter()
            .any(|position| position.baseline().y - lowest <= band)
        {
            continue;
        }
        if block
            .line_breaks
            .as_ref()
            .is_none_or(|breaks| !breaks.is_empty())
            || positions.is_empty()
            || positions.iter().any(|position| {
                let direction = position.direction();
                direction.x <= 0.0 || direction.y != 0.0 || position.baseline().y - lowest > band
            })
            || positions
                .windows(2)
                .any(|pair| pair[0].baseline().x > pair[1].baseline().x)
        {
            return None;
        }
        members.push(index);
    }
    members.sort_unstable_by(|&left, &right| {
        first_position_x(&blocks[left])
            .total_cmp(&first_position_x(&blocks[right]))
            .then_with(|| left.cmp(&right))
    });
    if members.windows(2).any(|pair| {
        let left = &blocks[pair[0]];
        let right = &blocks[pair[1]];
        !left.role.is_alignment_compatible(right.role)
            || last_position_x(left) > first_position_x(right)
    }) {
        return None;
    }
    Some(members)
}

fn first_position_x(block: &BlockText) -> f64 {
    block
        .position_signatures
        .as_ref()
        .and_then(|positions| positions.first())
        .map_or(f64::NAN, |position| position.baseline().x)
}

fn last_position_x(block: &BlockText) -> f64 {
    block
        .position_signatures
        .as_ref()
        .and_then(|positions| positions.last())
        .map_or(f64::NAN, |position| position.baseline().x)
}

fn span_range_with_limits(
    span: &TextSpan,
    blocks: &[BlockText],
    order: &HashMap<BlockId, usize>,
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<ResolvedScopeRange, String> {
    if span.comparable_range.start == span.comparable_range.end
        && span.canonical_range.start == span.canonical_range.end
    {
        // A zero-width side retains a source boundary, not a changed token.
        // Prefer its preceding scalar, as for a single-block span; only use
        // the following scalar when the preceding one has no exact coordinate.
        let previous = span
            .comparable_range
            .start
            .checked_sub(1)
            .zip(span.canonical_range.start.checked_sub(1));
        let next = Some((span.comparable_range.start, span.canonical_range.start));
        for (comparable_start, canonical_start) in [previous, next].into_iter().flatten() {
            budget.charge_work(span.blocks.len(), limits)?;
            let adjacent = TextSpan {
                blocks: span.blocks.clone(),
                separator: span.separator,
                comparable_range: TokenRange {
                    start: comparable_start,
                    end: comparable_start
                        .checked_add(1)
                        .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?,
                },
                canonical_range: ScalarRange {
                    start: canonical_start,
                    end: canonical_start
                        .checked_add(1)
                        .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?,
                },
            };
            match span_range_with_limits(&adjacent, blocks, order, budget, limits) {
                Err(reason) if reason == SCOPED_CHANGE_INDETERMINATE => {}
                result => return result,
            }
        }
        return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
    }
    let (_, rest) = span
        .blocks
        .split_first()
        .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?;
    let separator = if rest.is_empty() {
        if span.separator.is_some() {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        BlockSeparator::Concatenate
    } else {
        span.separator.unwrap_or(BlockSeparator::Concatenate)
    };
    if span.comparable_range.start > span.comparable_range.end
        || span.canonical_range.start >= span.canonical_range.end
    {
        return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
    }
    let mut projection = SpanProjection::default();
    let mut combined_last_whitespace = None;
    for (position, block_id) in span.blocks.iter().enumerate() {
        budget.charge_work(1, limits)?;
        let block_order = *order
            .get(block_id)
            .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?;
        let mapped = &blocks
            .get(block_order)
            .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?
            .canonical;
        let next_first_whitespace = !mapped
            .unmapped
            .first()
            .is_some_and(|token| token.scalar_index == 0)
            && mapped.text.chars().next().is_some_and(char::is_whitespace);
        let insert_space = position > 0
            && separator.at(position - 1) == BlockSeparator::Space
            && combined_last_whitespace != Some(true)
            && !next_first_whitespace;
        if insert_space {
            projection.project(None, true, span, budget, limits)?;
            combined_last_whitespace = Some(true);
        }
        let mut block_scalar = 0_usize;
        let mut unmapped_index = 0_usize;
        let mut last_whitespace = None;
        for scalar in mapped.text.chars() {
            if mapped
                .unmapped
                .get(unmapped_index)
                .is_some_and(|token| token.scalar_index < block_scalar)
            {
                return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
            }
            while mapped
                .unmapped
                .get(unmapped_index)
                .is_some_and(|token| token.scalar_index == block_scalar)
            {
                budget.charge_work(1, limits)?;
                projection.project(None, false, span, budget, limits)?;
                unmapped_index = budget.checked_add(unmapped_index, 1)?;
            }
            budget.charge_work(1, limits)?;
            let coordinate = ScopeCoordinate {
                block_order,
                scalar: block_scalar,
            };
            projection.project(Some(coordinate), true, span, budget, limits)?;
            block_scalar = budget.checked_add(block_scalar, 1)?;
            last_whitespace = Some(scalar.is_whitespace());
        }
        for token in &mapped.unmapped[unmapped_index..] {
            if token.scalar_index != block_scalar {
                return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
            }
            budget.charge_work(1, limits)?;
            projection.project(None, false, span, budget, limits)?;
            last_whitespace = Some(false);
        }
        if last_whitespace.is_some() {
            combined_last_whitespace = last_whitespace;
        }
    }
    if span.comparable_range.end > projection.comparable_offset
        || span.canonical_range.end > projection.scalar_offset
        || projection.first_selected.is_none()
        || projection.last_selected.is_none()
        || projection.last_selected_synthetic
    {
        return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
    }
    Ok(ResolvedScopeRange {
        start: projection.first_selected.expect("validated above"),
        end: projection.last_selected.expect("validated above"),
    })
}

fn containing_scope<'a>(
    range: ResolvedScopeRange,
    scopes: &'a [ResolvedScope],
    side: &str,
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<Option<&'a str>, String> {
    let mut contained = None;
    for scope in scopes {
        budget.charge_work(1, limits)?;
        let scope_range = if side == "old" { scope.old } else { scope.new };
        let overlaps = range.start <= scope_range.end && range.end >= scope_range.start;
        let fully_contained = range.start >= scope_range.start && range.end <= scope_range.end;
        if overlaps && !fully_contained {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        if fully_contained && contained.replace(scope.id.as_str()).is_some() {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
    }
    Ok(contained)
}

/// Classifies complete semantic changes by reviewed scope. A single uncertain
/// occurrence invalidates the entire scoped measurement so precision is never
/// published from a partial classification.
pub(super) fn classify_scoped_changes(
    changes: &[Change],
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
) -> Result<Vec<ScopedChange>, String> {
    classify_scoped_changes_with_limits(
        changes,
        scopes,
        old_blocks,
        new_blocks,
        ClassificationLimits::default(),
    )
}

/// Classifies proven changed regions only when every present side is wholly
/// contained by the same reviewed scope.
pub(super) fn classify_scoped_proven_changed_regions(
    regions: &[ProvenChangedRegion],
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
) -> Result<Vec<ScopedProvenChangedRegion>, String> {
    let limits = ClassificationLimits::default();
    let mut budget = ClassificationBudget::default();
    let old_order = classification_block_order(old_blocks, &mut budget, limits)?;
    let new_order = classification_block_order(new_blocks, &mut budget, limits)?;
    let mut classified = Vec::new();
    for (region_index, region) in regions.iter().enumerate() {
        budget.charge_work(1, limits)?;
        let mut region_scope = None;
        let mut outside = false;
        for (side, span, blocks, order) in [
            ("old", region.old_span.as_ref(), old_blocks, &old_order),
            ("new", region.new_span.as_ref(), new_blocks, &new_order),
        ] {
            let Some(span) = span else { continue };
            let range = span_range_with_limits(span, blocks, order, &mut budget, limits)?;
            let scope = containing_scope(range, scopes, side, &mut budget, limits)?;
            match (region_scope, scope, outside) {
                (None, Some(scope), false) => region_scope = Some(scope),
                (Some(current), Some(scope), _) if current == scope => {}
                (None, None, _) => outside = true,
                _ => return Err(SCOPED_CHANGE_INDETERMINATE.to_owned()),
            }
        }
        if let Some(scope_id) = region_scope {
            budget.charge_output(limits)?;
            classified.push(ScopedProvenChangedRegion {
                scope_id: scope_id.to_owned(),
                region_index,
            });
        }
    }
    Ok(classified)
}

fn classify_scoped_changes_with_limits(
    changes: &[Change],
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    limits: ClassificationLimits,
) -> Result<Vec<ScopedChange>, String> {
    let mut budget = ClassificationBudget::default();
    let old_order = classification_block_order(old_blocks, &mut budget, limits)?;
    let new_order = classification_block_order(new_blocks, &mut budget, limits)?;
    let mut classified = Vec::new();
    for (change_index, change) in changes.iter().enumerate() {
        budget.charge_work(1, limits)?;
        budget.charge_output(limits)?;
        let mut change_scope = None;
        let mut outside = false;
        if change.occurrences.is_empty() {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        for occurrence in &change.occurrences {
            budget.charge_work(1, limits)?;
            budget.charge_output(limits)?;
            let mut occurrence_scope = None;
            let mut side_outside = false;
            let mut present = false;
            for (side, span, blocks, order) in [
                ("old", occurrence.old_span.as_ref(), old_blocks, &old_order),
                ("new", occurrence.new_span.as_ref(), new_blocks, &new_order),
            ] {
                let Some(span) = span else { continue };
                present = true;
                let range = span_range_with_limits(span, blocks, order, &mut budget, limits)?;
                let scope = containing_scope(range, scopes, side, &mut budget, limits)?;
                match (occurrence_scope, scope) {
                    (None, Some(scope)) if !side_outside => occurrence_scope = Some(scope),
                    (Some(current), Some(scope)) if current == scope => {}
                    (None, None) => side_outside = true,
                    _ => return Err(SCOPED_CHANGE_INDETERMINATE.to_owned()),
                }
            }
            if !present {
                return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
            }
            match occurrence_scope {
                Some(_) if outside => {
                    return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
                }
                Some(scope) => match change_scope {
                    None => change_scope = Some(scope),
                    Some(current) if current == scope => {}
                    Some(_) => return Err(SCOPED_CHANGE_INDETERMINATE.to_owned()),
                },
                None if change_scope.is_some() => {
                    return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
                }
                None => outside = true,
            }
        }
        if let Some(scope_id) = change_scope {
            budget.charge_output(limits)?;
            classified.push(ScopedChange {
                scope_id: scope_id.to_owned(),
                change_index,
            });
        }
    }
    Ok(classified)
}

fn classification_block_order(
    blocks: &[BlockText],
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<HashMap<BlockId, usize>, String> {
    let mut order = HashMap::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        budget.charge_work(1, limits)?;
        if order.insert(block.block, index).is_some() {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
    }
    Ok(order)
}

fn push_token_interval(
    intervals: &mut Vec<TokenInterval>,
    interval: TokenInterval,
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<(), String> {
    if interval.start >= interval.end {
        return Ok(());
    }
    if let Some(last) = intervals.last_mut()
        && last.block_order == interval.block_order
        && interval.start <= last.end
    {
        last.end = last.end.max(interval.end);
        return Ok(());
    }
    budget.charge_output(limits)?;
    intervals.push(interval);
    Ok(())
}

fn normalize_intervals(
    intervals: &mut Vec<TokenInterval>,
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<(), String> {
    budget.charge_work(intervals.len(), limits)?;
    intervals.sort_unstable();
    let mut merged = Vec::with_capacity(intervals.len());
    for interval in intervals.drain(..) {
        budget.charge_work(1, limits)?;
        push_token_interval(&mut merged, interval, budget, limits)?;
    }
    *intervals = merged;
    Ok(())
}

fn scalar_location_intervals(
    location: ScopedQuoteLocation,
    blocks: &[BlockText],
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<Vec<TokenInterval>, String> {
    if location.start_block > location.end_block
        || location.end_block >= blocks.len()
        || (location.start_block == location.end_block
            && location.start_scalar >= location.end_scalar)
    {
        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
    }
    let mut intervals = Vec::new();
    for (block_offset, block) in blocks[location.start_block..=location.end_block]
        .iter()
        .enumerate()
    {
        budget.charge_work(1, limits)?;
        let block_order = budget.checked_add(location.start_block, block_offset)?;
        let tokens = block
            .canonical
            .comparable_tokens()
            .map_err(|_| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
        let scalar_count = block.canonical.text.chars().count();
        let start_scalar = if block_order == location.start_block {
            location.start_scalar
        } else {
            0
        };
        let end_scalar = if block_order == location.end_block {
            location.end_scalar
        } else {
            scalar_count
        };
        if start_scalar > end_scalar || end_scalar > scalar_count {
            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
        }
        let mut scalar = 0_usize;
        let mut interval_start = None;
        let mut interval_end = 0_usize;
        for (token_index, token) in tokens.iter().enumerate() {
            budget.charge_work(1, limits)?;
            match token {
                ComparableToken::Scalar(_) => {
                    if start_scalar <= scalar && scalar < end_scalar {
                        interval_start.get_or_insert(token_index);
                        interval_end = budget.checked_add(token_index, 1)?;
                    }
                    scalar = budget.checked_add(scalar, 1)?;
                }
                ComparableToken::Unmapped { .. }
                    if start_scalar <= scalar && scalar < end_scalar =>
                {
                    return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
                }
                ComparableToken::Unmapped { .. } => {}
            }
        }
        if scalar != scalar_count {
            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
        }
        if let Some(start) = interval_start {
            push_token_interval(
                &mut intervals,
                TokenInterval {
                    block_order,
                    start,
                    end: interval_end,
                },
                budget,
                limits,
            )?;
        }
    }
    Ok(intervals)
}

fn scope_location(range: ResolvedScopeRange) -> Result<ScopedQuoteLocation, String> {
    Ok(ScopedQuoteLocation {
        start_block: range.start.block_order,
        start_scalar: range.start.scalar,
        end_block: range.end.block_order,
        end_scalar: range
            .end
            .scalar
            .checked_add(1)
            .ok_or_else(|| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?,
    })
}

fn is_comparable_whitespace(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn expected_quote_unavailable(
    change: &ExpectedChange,
    scope: &str,
    side: &str,
    outcome: &str,
) -> String {
    format!(
        "scoped-complete expected change {:?} {side} quote is {outcome} within scope {scope:?}",
        change.id
    )
}

fn collapsed_scalar_mapping(
    location: ScopedQuoteLocation,
    collapsed_context: &str,
    blocks: &[BlockText],
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<CollapsedContextMapping, String> {
    if location.start_block > location.end_block || location.end_block >= blocks.len() {
        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
    }
    let expected_len = collapsed_context.chars().count();
    let mut expected = Vec::new();
    expected
        .try_reserve_exact(expected_len)
        .map_err(|_| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?;
    expected.extend(collapsed_context.chars());
    let mut scalars = Vec::new();
    scalars
        .try_reserve_exact(expected_len)
        .map_err(|_| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?;
    let mut intervals = Vec::new();
    intervals
        .try_reserve_exact(expected_len)
        .map_err(|_| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?;
    let mut pending_whitespace = Vec::<TokenInterval>::new();
    let mut has_text = false;
    let mut crossed_block = false;
    let mut pending_crosses_block = false;

    for (block_offset, block) in blocks[location.start_block..=location.end_block]
        .iter()
        .enumerate()
    {
        budget.charge_work(1, limits)?;
        let block_order = budget.checked_add(location.start_block, block_offset)?;
        let tokens = block
            .canonical
            .comparable_tokens()
            .map_err(|_| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
        let scalar_count = block.canonical.text.chars().count();
        let start_scalar = if block_order == location.start_block {
            location.start_scalar
        } else {
            0
        };
        let end_scalar = if block_order == location.end_block {
            location.end_scalar
        } else {
            scalar_count
        };
        if start_scalar > end_scalar || end_scalar > scalar_count {
            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
        }

        if has_text && block_offset > 0 {
            crossed_block = true;
            pending_crosses_block = !pending_whitespace.is_empty();
        }
        let mut scalar = 0_usize;
        for (token_index, token) in tokens.iter().enumerate() {
            budget.charge_work(1, limits)?;
            match token {
                ComparableToken::Scalar(value) => {
                    let in_range = start_scalar <= scalar && scalar < end_scalar;
                    scalar = budget.checked_add(scalar, 1)?;
                    if !in_range {
                        continue;
                    }
                    let interval = TokenInterval {
                        block_order,
                        start: token_index,
                        end: budget.checked_add(token_index, 1)?,
                    };
                    if value.is_whitespace() {
                        if has_text {
                            if let Some(pending) = pending_whitespace.last_mut()
                                && pending.block_order == interval.block_order
                                && pending.end == interval.start
                            {
                                pending.end = interval.end;
                            } else {
                                pending_whitespace
                                    .try_reserve(1)
                                    .map_err(|_| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?;
                                pending_whitespace.push(interval);
                            }
                        }
                        continue;
                    }
                    if has_text && expected.get(scalars.len()) == Some(&' ') {
                        if pending_whitespace.is_empty() && !crossed_block {
                            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
                        }
                        let interval_start = intervals.len();
                        intervals
                            .try_reserve(pending_whitespace.len())
                            .map_err(|_| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?;
                        intervals.append(&mut pending_whitespace);
                        scalars.push(CollapsedScalarMapping {
                            value: ' ',
                            interval_start,
                            interval_end: intervals.len(),
                        });
                    } else if !pending_whitespace.is_empty() && !pending_crosses_block {
                        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
                    } else {
                        pending_whitespace.clear();
                    }
                    let interval_start = intervals.len();
                    intervals
                        .try_reserve(1)
                        .map_err(|_| SCOPED_TOKEN_METRICS_LIMITED.to_owned())?;
                    intervals.push(interval);
                    scalars.push(CollapsedScalarMapping {
                        value: *value,
                        interval_start,
                        interval_end: intervals.len(),
                    });
                    has_text = true;
                    crossed_block = false;
                    pending_crosses_block = false;
                }
                ComparableToken::Unmapped { .. } => {
                    return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
                }
            }
        }
        if scalar != scalar_count {
            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
        }
    }

    if scalars.len() != expected.len()
        || scalars
            .iter()
            .zip(expected)
            .any(|(mapped, expected)| mapped.value != expected)
    {
        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
    }
    Ok(CollapsedContextMapping { scalars, intervals })
}

fn relative_changed_intervals(
    context: &CollapsedContextMapping,
    ranges: &[ExpectedChangedRange],
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<Vec<TokenInterval>, String> {
    let mut selected = Vec::new();
    for range in ranges {
        let Some(changed) = context.scalars.get(range.start..range.end) else {
            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
        };
        for mapped in changed {
            budget.charge_work(1, limits)?;
            let Some(intervals) = context
                .intervals
                .get(mapped.interval_start..mapped.interval_end)
            else {
                return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
            };
            for interval in intervals {
                push_token_interval(&mut selected, *interval, budget, limits)?;
            }
        }
    }
    Ok(selected)
}

pub(super) fn validate_scoped_expected_changes(
    changes: &[ExpectedChange],
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
) -> Result<ScopedExpectedTokenEvidence, String> {
    let mut budget = DiagnosticBudget::default();
    let limits = DiagnosticLimits::default();
    let mut token_budget = ClassificationBudget::default();
    let token_limits = ClassificationLimits::default();
    let mut evidence = ScopedExpectedTokenEvidence::default();
    for change in changes {
        if !budget.charge_scope(limits) {
            return Err(SCOPED_EXPECTED_CHANGE_LIMITED.to_owned());
        }
        let scope_id = change
            .scope
            .as_deref()
            .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?;
        let mut resolved_scope = None;
        for scope in scopes {
            if !budget.charge_scope_scan(limits) {
                return Err(SCOPED_EXPECTED_CHANGE_LIMITED.to_owned());
            }
            if scope.id == scope_id && resolved_scope.replace(scope).is_some() {
                return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
            }
        }
        let scope = resolved_scope.ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?;
        for (side, context, changed, changed_ranges, blocks, range) in [
            (
                "old",
                change.old_quote.as_deref(),
                change.old_changed_quote.as_deref(),
                change.old_changed_ranges.as_deref(),
                old_blocks,
                scope.old,
            ),
            (
                "new",
                change.new_quote.as_deref(),
                change.new_changed_quote.as_deref(),
                change.new_changed_ranges.as_deref(),
                new_blocks,
                scope.new,
            ),
        ] {
            let Some(context) = context else { continue };
            let context_outcome = locate_scoped_quote(
                blocks,
                context,
                ScopedQuoteRange {
                    start_block: range.start.block_order,
                    start_scalar: range.start.scalar,
                    end_block: range.end.block_order,
                    end_scalar: range.end.scalar,
                },
                &mut budget,
                limits,
            )
            .map_err(|_| expected_quote_unavailable(change, scope_id, side, "indeterminate"))?;
            let context_location = match context_outcome {
                ScopedQuoteLocateOutcome::Unique(location) => location,
                ScopedQuoteLocateOutcome::Missing => {
                    return Err(expected_quote_unavailable(
                        change, scope_id, side, "missing",
                    ));
                }
                ScopedQuoteLocateOutcome::Ambiguous => {
                    return Err(expected_quote_unavailable(
                        change,
                        scope_id,
                        side,
                        "ambiguous",
                    ));
                }
                ScopedQuoteLocateOutcome::Indeterminate => {
                    return Err(expected_quote_unavailable(
                        change,
                        scope_id,
                        side,
                        "indeterminate",
                    ));
                }
                ScopedQuoteLocateOutcome::Limited => {
                    return Err(SCOPED_EXPECTED_CHANGE_LIMITED.to_owned());
                }
            };
            if let Some(changed_ranges) = changed_ranges {
                let collapsed_context = collapse_whitespace(context);
                let context_mapping = collapsed_scalar_mapping(
                    context_location,
                    &collapsed_context,
                    blocks,
                    &mut token_budget,
                    token_limits,
                )
                .map_err(token_metrics_error)?;
                let intervals = relative_changed_intervals(
                    &context_mapping,
                    changed_ranges,
                    &mut token_budget,
                    token_limits,
                )
                .map_err(token_metrics_error)?;
                if side == "old" {
                    evidence.old.extend(intervals);
                } else {
                    evidence.new.extend(intervals);
                }
                continue;
            }
            let location = if let Some(changed) = changed {
                if changed.is_empty() {
                    continue;
                }
                let changed_outcome = locate_scoped_quote(
                    blocks,
                    changed,
                    ScopedQuoteRange {
                        start_block: context_location.start_block,
                        start_scalar: context_location.start_scalar,
                        end_block: context_location.end_block,
                        end_scalar: context_location
                            .end_scalar
                            .checked_sub(1)
                            .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?,
                    },
                    &mut budget,
                    limits,
                )
                .map_err(|_| expected_quote_unavailable(change, scope_id, side, "indeterminate"))?;
                match changed_outcome {
                    ScopedQuoteLocateOutcome::Unique(location) => location,
                    ScopedQuoteLocateOutcome::Missing => {
                        return Err(expected_quote_unavailable(
                            change, scope_id, side, "missing",
                        ));
                    }
                    ScopedQuoteLocateOutcome::Ambiguous => {
                        return Err(expected_quote_unavailable(
                            change,
                            scope_id,
                            side,
                            "ambiguous",
                        ));
                    }
                    ScopedQuoteLocateOutcome::Indeterminate => {
                        return Err(expected_quote_unavailable(
                            change,
                            scope_id,
                            side,
                            "indeterminate",
                        ));
                    }
                    ScopedQuoteLocateOutcome::Limited => {
                        return Err(SCOPED_EXPECTED_CHANGE_LIMITED.to_owned());
                    }
                }
            } else {
                context_location
            };
            let intervals =
                scalar_location_intervals(location, blocks, &mut token_budget, token_limits)
                    .map_err(token_metrics_error)?;
            if side == "old" {
                evidence.old.extend(intervals);
            } else {
                evidence.new.extend(intervals);
            }
        }
    }
    normalize_intervals(&mut evidence.old, &mut token_budget, token_limits)
        .map_err(token_metrics_error)?;
    normalize_intervals(&mut evidence.new, &mut token_budget, token_limits)
        .map_err(token_metrics_error)?;
    Ok(evidence)
}

fn project_span_intervals(
    span: &TextSpan,
    blocks: &[BlockText],
    order: &HashMap<BlockId, usize>,
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<Vec<TokenInterval>, String> {
    let (_, rest) = span
        .blocks
        .split_first()
        .ok_or_else(|| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
    let separator = if rest.is_empty() {
        if span.separator.is_some() {
            return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
        }
        BlockSeparator::Concatenate
    } else {
        span.separator.unwrap_or(BlockSeparator::Concatenate)
    };
    if span.comparable_range.start > span.comparable_range.end
        || span.canonical_range.start > span.canonical_range.end
        || (span.comparable_range.start == span.comparable_range.end)
            != (span.canonical_range.start == span.canonical_range.end)
    {
        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
    }

    let mut intervals = Vec::new();
    let mut comparable_offset = 0_usize;
    let mut scalar_offset = 0_usize;
    let mut last_selected_token = None;
    let mut last_source_token = None;
    let mut combined_last = None::<ComparableToken>;
    for (position, block_id) in span.blocks.iter().enumerate() {
        let block_order = *order
            .get(block_id)
            .ok_or_else(|| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
        let tokens = blocks[block_order]
            .canonical
            .comparable_tokens()
            .map_err(|_| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
        let synthetic_space = position > 0
            && separator.at(position - 1) == BlockSeparator::Space
            && !combined_last.as_ref().is_some_and(is_comparable_whitespace)
            && !tokens.first().is_some_and(is_comparable_whitespace);
        if synthetic_space {
            if comparable_offset == span.comparable_range.start
                && scalar_offset != span.canonical_range.start
            {
                return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
            }
            comparable_offset = budget.checked_add(comparable_offset, 1)?;
            scalar_offset = budget.checked_add(scalar_offset, 1)?;
            combined_last = Some(ComparableToken::Scalar(' '));
            budget.charge_work(1, limits)?;
            if comparable_offset == span.comparable_range.end
                && scalar_offset != span.canonical_range.end
            {
                return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
            }
        }
        for (token_index, token) in tokens.iter().enumerate() {
            budget.charge_work(1, limits)?;
            if comparable_offset == span.comparable_range.start
                && scalar_offset != span.canonical_range.start
            {
                return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
            }
            let selected = span.comparable_range.start <= comparable_offset
                && comparable_offset < span.comparable_range.end;
            if selected {
                if !token.is_scalar() {
                    return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
                }
                if let Some((previous_block, previous_token)) = last_selected_token
                    && !selected_token_follows(
                        previous_block,
                        previous_token,
                        block_order,
                        token_index,
                        last_source_token,
                    )
                {
                    return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
                }
                last_selected_token = Some((block_order, token_index));
                push_token_interval(
                    &mut intervals,
                    TokenInterval {
                        block_order,
                        start: token_index,
                        end: budget.checked_add(token_index, 1)?,
                    },
                    budget,
                    limits,
                )?;
            }
            comparable_offset = budget.checked_add(comparable_offset, 1)?;
            if token.is_scalar() {
                scalar_offset = budget.checked_add(scalar_offset, 1)?;
            }
            if comparable_offset == span.comparable_range.end
                && scalar_offset != span.canonical_range.end
            {
                return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
            }
            last_source_token = Some((block_order, token_index));
        }
        if let Some(last) = tokens.last() {
            combined_last = Some(last.clone());
        }
    }
    if span.comparable_range.end > comparable_offset || span.canonical_range.end > scalar_offset {
        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
    }
    Ok(intervals)
}

fn interval_count(
    intervals: &[TokenInterval],
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<usize, String> {
    let mut count = 0_usize;
    for interval in intervals {
        budget.charge_work(1, limits)?;
        count = budget.checked_add(count, interval.end - interval.start)?;
    }
    Ok(count)
}

fn intersection_count(
    left: &[TokenInterval],
    right: &[TokenInterval],
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<usize, String> {
    let (mut left_index, mut right_index, mut count) = (0, 0, 0_usize);
    while left_index < left.len() && right_index < right.len() {
        budget.charge_work(1, limits)?;
        let a = left[left_index];
        let b = right[right_index];
        if a.block_order < b.block_order || (a.block_order == b.block_order && a.end <= b.start) {
            left_index += 1;
            continue;
        }
        if b.block_order < a.block_order || (a.block_order == b.block_order && b.end <= a.start) {
            right_index += 1;
            continue;
        }
        count = budget.checked_add(count, a.end.min(b.end) - a.start.max(b.start))?;
        if a.end <= b.end {
            left_index += 1;
        }
        if b.end <= a.end {
            right_index += 1;
        }
    }
    Ok(count)
}

fn rate(numerator: usize, denominator: usize, corresponding_empty: bool) -> f64 {
    if denominator == 0 {
        if corresponding_empty { 1.0 } else { 0.0 }
    } else {
        numerator as f64 / denominator as f64
    }
}

pub(super) fn evaluate_scoped_token_metrics(
    changes: &[Change],
    expected: ScopedExpectedTokenEvidence,
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    _recovered_atomic_diffs: &[RecoveredAtomicDiff],
) -> Result<ScopedTokenMetrics, String> {
    evaluate_scoped_token_metrics_with_limits(
        changes,
        expected,
        scopes,
        old_blocks,
        new_blocks,
        _recovered_atomic_diffs,
        ClassificationLimits::default(),
    )
    .map_err(token_metrics_error)
}

fn evaluate_scoped_token_metrics_with_limits(
    changes: &[Change],
    expected: ScopedExpectedTokenEvidence,
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    _recovered_atomic_diffs: &[RecoveredAtomicDiff],
    limits: ClassificationLimits,
) -> Result<ScopedTokenMetrics, String> {
    let mut budget = ClassificationBudget::default();
    let old_order = classification_block_order(old_blocks, &mut budget, limits)?;
    let new_order = classification_block_order(new_blocks, &mut budget, limits)?;
    let mut reported_old = Vec::new();
    let mut reported_new = Vec::new();
    for change in changes {
        budget.charge_work(1, limits)?;
        if change.occurrences.is_empty() {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
        for occurrence in &change.occurrences {
            budget.charge_work(1, limits)?;
            if occurrence.old_span.is_none() && occurrence.new_span.is_none() {
                return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
            }
            for (span, blocks, order, output) in [
                (
                    occurrence.old_span.as_ref(),
                    old_blocks,
                    &old_order,
                    &mut reported_old,
                ),
                (
                    occurrence.new_span.as_ref(),
                    new_blocks,
                    &new_order,
                    &mut reported_new,
                ),
            ] {
                if let Some(span) = span {
                    output.extend(project_span_intervals(
                        span,
                        blocks,
                        order,
                        &mut budget,
                        limits,
                    )?);
                }
            }
        }
    }
    score_projected_tokens(
        [reported_old, reported_new],
        expected,
        scopes,
        [old_blocks, new_blocks],
        budget,
        limits,
    )
}

/// Scores already source-projected masks through the same interval kernel as
/// native change spans. This does not infer event identity from token coverage.
pub(super) fn evaluate_projected_tokens(
    reported: [Vec<TokenInterval>; 2],
    expected: ScopedExpectedTokenEvidence,
    scopes: &[ResolvedScope],
    blocks: [&[BlockText]; 2],
) -> Result<ScopedTokenMetrics, String> {
    score_projected_tokens(
        reported,
        expected,
        scopes,
        blocks,
        ClassificationBudget::default(),
        ClassificationLimits::default(),
    )
}

fn score_projected_tokens(
    reported: [Vec<TokenInterval>; 2],
    expected: ScopedExpectedTokenEvidence,
    scopes: &[ResolvedScope],
    blocks: [&[BlockText]; 2],
    mut budget: ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<ScopedTokenMetrics, String> {
    let [old_blocks, new_blocks] = blocks;
    let [mut reported_old, mut reported_new] = reported;
    normalize_intervals(&mut reported_old, &mut budget, limits)?;
    normalize_intervals(&mut reported_new, &mut budget, limits)?;

    let mut scope_old = Vec::new();
    let mut scope_new = Vec::new();
    for scope in scopes {
        scope_old.extend(scalar_location_intervals(
            scope_location(scope.old)?,
            old_blocks,
            &mut budget,
            limits,
        )?);
        scope_new.extend(scalar_location_intervals(
            scope_location(scope.new)?,
            new_blocks,
            &mut budget,
            limits,
        )?);
    }
    normalize_intervals(&mut scope_old, &mut budget, limits)?;
    normalize_intervals(&mut scope_new, &mut budget, limits)?;

    let expected_old_count = interval_count(&expected.old, &mut budget, limits)?;
    let expected_new_count = interval_count(&expected.new, &mut budget, limits)?;
    let expected_count = budget.checked_add(expected_old_count, expected_new_count)?;
    let scope_old_count = interval_count(&scope_old, &mut budget, limits)?;
    let scope_new_count = interval_count(&scope_new, &mut budget, limits)?;
    let scope_count = budget.checked_add(scope_old_count, scope_new_count)?;
    let expected_in_scope_old = intersection_count(&expected.old, &scope_old, &mut budget, limits)?;
    let expected_in_scope_new = intersection_count(&expected.new, &scope_new, &mut budget, limits)?;
    let expected_in_scope = budget.checked_add(expected_in_scope_old, expected_in_scope_new)?;
    let reported_in_scope_old = intersection_count(&reported_old, &scope_old, &mut budget, limits)?;
    let reported_in_scope_new = intersection_count(&reported_new, &scope_new, &mut budget, limits)?;
    let reported_in_scope = budget.checked_add(reported_in_scope_old, reported_in_scope_new)?;
    if expected_in_scope != expected_count || expected_count > scope_count {
        return Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned());
    }
    let reported_count = reported_in_scope;
    let true_positive_old = intersection_count(&expected.old, &reported_old, &mut budget, limits)?;
    let true_positive_new = intersection_count(&expected.new, &reported_new, &mut budget, limits)?;
    let true_positive = budget.checked_add(true_positive_old, true_positive_new)?;
    let union = budget
        .checked_add(expected_count, reported_count)?
        .checked_sub(true_positive)
        .ok_or_else(|| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
    let unchanged = scope_count
        .checked_sub(expected_count)
        .ok_or_else(|| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
    let false_positive = reported_count
        .checked_sub(true_positive)
        .ok_or_else(|| SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned())?;
    let precision = rate(true_positive, reported_count, expected_count == 0);
    let recall = rate(true_positive, expected_count, reported_count == 0);
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Ok(ScopedTokenMetrics {
        expected_changed_tokens: expected_count,
        reported_changed_tokens: reported_count,
        true_positive_tokens: true_positive,
        precision,
        recall,
        f1,
        span_iou: rate(true_positive, union, true),
        false_positive_tokens_per_10k_unchanged: (unchanged != 0)
            .then(|| false_positive as f64 * 10_000.0 / unchanged as f64),
    })
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::diff::{
        AtomicEdit, ChangeKind, ChangeOccurrence, Confidence, RecoveredAtomicOccurrence, TokenRange,
    };
    use pdfdelta_core::layout::BlockRole;
    use pdfdelta_core::model::Vec2;
    use pdfdelta_core::normalize::{
        ComparableToken, FontSizeSignature, MappedText, NormalizationIssue, NormalizationIssueKind,
        PositionSignature, ScalarRange, TextSource,
    };

    use super::super::{ExpectedKind, QuoteScope};
    use super::*;

    fn mapped(text: &str) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        }
    }

    fn block(id: u64, text: &str) -> BlockText {
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: mapped(text),
            canonical: mapped(text),
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

    fn positioned_block(id: u64, text: &str, x: f64, y: f64) -> BlockText {
        let mut block = block(id, text);
        let count = text.chars().count();
        block.pages = vec![0];
        block.font_size_signatures = Some(
            (0..count)
                .map(|_| FontSizeSignature::new(&[10.0]).expect("valid font size"))
                .collect(),
        );
        block.position_signatures = Some(
            (0..count)
                .map(|offset| {
                    PositionSignature::new(
                        Vec2 {
                            x: x + offset as f64 * 4.0,
                            y,
                        },
                        Vec2 { x: 1.0, y: 0.0 },
                    )
                    .expect("valid position")
                })
                .collect(),
        );
        block.line_breaks = Some(Vec::new());
        block
    }

    #[test]
    fn evaluation_views_reorder_only_certified_terminal_bands() {
        let old = [
            positioned_block(1, "right", 20.0, 10.0),
            positioned_block(2, "left", 0.0, 10.0),
            positioned_block(3, "body", 0.0, 100.0),
        ];
        let [reordered, _] = evaluation_block_views(&old, &[]);
        assert_eq!(
            reordered
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            [BlockId(2), BlockId(1), BlockId(3)]
        );

        let overlapping = [
            positioned_block(4, "left", 0.0, 10.0),
            positioned_block(5, "right", 1.0, 10.0),
        ];
        let [unchanged, _] = evaluation_block_views(&overlapping, &[]);
        assert_eq!(
            unchanged
                .iter()
                .map(|block| block.block)
                .collect::<Vec<_>>(),
            [BlockId(4), BlockId(5)]
        );
    }

    fn scope(
        id: &str,
        old_start: &str,
        old_end: &str,
        new_start: &str,
        new_end: &str,
    ) -> ExpectedScope {
        ExpectedScope {
            id: id.to_owned(),
            completeness: None,
            old: QuoteScope {
                start_quote: old_start.to_owned(),
                end_quote: old_end.to_owned(),
            },
            new: QuoteScope {
                start_quote: new_start.to_owned(),
                end_quote: new_end.to_owned(),
            },
        }
    }

    fn span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn group_span(
        blocks: Vec<BlockId>,
        separator: BlockSeparator,
        start: usize,
        end: usize,
    ) -> TextSpan {
        TextSpan {
            blocks,
            separator: Some(separator),
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn change(old: Option<TextSpan>, new: Option<TextSpan>) -> Change {
        Change::single_occurrence(
            match (old.is_some(), new.is_some()) {
                (true, true) => ChangeKind::Replacement,
                (true, false) => ChangeKind::Deletion,
                (false, true) => ChangeKind::Insertion,
                (false, false) => unreachable!(),
            },
            old,
            new,
            Confidence::High,
            Vec::new(),
        )
    }

    fn expected(scope: &str, old: &str, new: &str) -> ExpectedChange {
        ExpectedChange {
            id: "reviewed".to_owned(),
            kind: ExpectedKind::Replacement,
            scope: Some(scope.to_owned()),
            occurrence_count: None,
            old_quote: Some(old.to_owned()),
            new_quote: Some(new.to_owned()),
            old_changed_quote: None,
            new_changed_quote: None,
            old_changed_ranges: None,
            new_changed_ranges: None,
            note: String::new(),
        }
    }

    fn scoped_metrics(
        old: &[BlockText],
        new: &[BlockText],
        expected_changes: &[ExpectedChange],
        actual_changes: &[Change],
    ) -> ScopedTokenMetrics {
        scoped_metrics_with_recovery(old, new, expected_changes, actual_changes, &[])
    }

    fn scoped_metrics_with_recovery(
        old: &[BlockText],
        new: &[BlockText],
        expected_changes: &[ExpectedChange],
        actual_changes: &[Change],
        recovered_atomic_diffs: &[RecoveredAtomicDiff],
    ) -> ScopedTokenMetrics {
        let old_end = old
            .last()
            .expect("old scope has a block")
            .canonical
            .text
            .chars()
            .count()
            - 1;
        let new_end = new
            .last()
            .expect("new scope has a block")
            .canonical
            .text
            .chars()
            .count()
            - 1;
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: old.len() - 1,
                    scalar: old_end,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: new.len() - 1,
                    scalar: new_end,
                },
            },
        }];
        let evidence = validate_scoped_expected_changes(expected_changes, &scopes, old, new)
            .expect("expected quotes resolve");
        evaluate_scoped_token_metrics(
            actual_changes,
            evidence,
            &scopes,
            old,
            new,
            recovered_atomic_diffs,
        )
        .expect("token metrics evaluate")
    }

    #[test]
    fn scoped_token_metrics_cover_exact_replacement_insertion_and_deletion() {
        let replacement = scoped_metrics(
            &[block(1, "aaOLDzz")],
            &[block(2, "aaNEWzz")],
            &[expected("body", "OLD", "NEW")],
            &[change(Some(span(1, 2, 5)), Some(span(2, 2, 5)))],
        );
        assert_eq!(replacement.expected_changed_tokens, 6);
        assert_eq!(replacement.reported_changed_tokens, 6);
        assert_eq!(replacement.true_positive_tokens, 6);
        assert_eq!(
            (replacement.precision, replacement.recall, replacement.f1),
            (1.0, 1.0, 1.0)
        );
        assert_eq!(replacement.span_iou, 1.0);

        let insertion_expected = ExpectedChange {
            id: "insert".to_owned(),
            kind: ExpectedKind::Insertion,
            scope: Some("body".to_owned()),
            occurrence_count: None,
            old_quote: None,
            new_quote: Some("ADD".to_owned()),
            old_changed_quote: None,
            new_changed_quote: None,
            old_changed_ranges: None,
            new_changed_ranges: None,
            note: String::new(),
        };
        let insertion = scoped_metrics(
            &[block(3, "anchor")],
            &[block(4, "aaADDzz")],
            &[insertion_expected],
            &[change(None, Some(span(4, 2, 5)))],
        );
        assert_eq!(
            (
                insertion.expected_changed_tokens,
                insertion.true_positive_tokens
            ),
            (3, 3)
        );

        let deletion_expected = ExpectedChange {
            id: "delete".to_owned(),
            kind: ExpectedKind::Deletion,
            scope: Some("body".to_owned()),
            occurrence_count: None,
            old_quote: Some("OLD".to_owned()),
            new_quote: None,
            old_changed_quote: None,
            new_changed_quote: None,
            old_changed_ranges: None,
            new_changed_ranges: None,
            note: String::new(),
        };
        let deletion = scoped_metrics(
            &[block(5, "aaOLDzz")],
            &[block(6, "anchor")],
            &[deletion_expected],
            &[change(Some(span(5, 2, 5)), None)],
        );
        assert_eq!(
            (
                deletion.expected_changed_tokens,
                deletion.true_positive_tokens
            ),
            (3, 3)
        );
    }

    #[test]
    fn scoped_token_metrics_project_crossing_events_to_reviewed_tokens() {
        let old = [block(1, "xxOLDyy")];
        let new = [block(2, "xxNEWyy")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 2,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 4,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 2,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 4,
                },
            },
        }];
        let evidence = validate_scoped_expected_changes(
            &[expected("body", "OLD", "NEW")],
            &scopes,
            &old,
            &new,
        )
        .expect("expected quotes resolve");
        let metrics = evaluate_scoped_token_metrics(
            &[change(Some(span(1, 1, 6)), Some(span(2, 1, 6)))],
            evidence,
            &scopes,
            &old,
            &new,
            &[],
        )
        .expect("crossing event projects to the reviewed scope");

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_exclude_out_of_scope_reported_changes() {
        let old = [block(1, "OLD outside")];
        let new = [block(2, "NEW outside")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 2,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 2,
                },
            },
        }];
        let evidence = validate_scoped_expected_changes(
            &[expected("body", "OLD", "NEW")],
            &scopes,
            &old,
            &new,
        )
        .expect("expected quotes resolve");
        let changes = [
            change(Some(span(1, 0, 3)), Some(span(2, 0, 3))),
            change(Some(span(1, 4, 11)), Some(span(2, 4, 11))),
        ];
        let metrics = evaluate_scoped_token_metrics(&changes, evidence, &scopes, &old, &new, &[])
            .expect("out-of-scope changes do not affect scoped metrics");

        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_reject_malformed_reported_projection() {
        let old = [block(1, "OLD")];
        let new = [block(2, "NEW")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 2,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 2,
                },
            },
        }];
        let mut comparable_empty = span(1, 0, 3);
        comparable_empty.comparable_range.end = 0;
        let mut canonical_empty = span(1, 0, 3);
        canonical_empty.canonical_range.end = 0;
        for old_span in [span(99, 0, 1), comparable_empty, canonical_empty] {
            let result = evaluate_scoped_token_metrics(
                &[change(Some(old_span), Some(span(2, 0, 1)))],
                ScopedExpectedTokenEvidence::default(),
                &scopes,
                &old,
                &new,
                &[],
            );
            assert_eq!(result, Err(SCOPED_TOKEN_METRICS_INDETERMINATE.to_owned()));
        }
    }

    #[test]
    fn scoped_token_metrics_reject_empty_and_spanless_changes() {
        let mut empty = change(Some(span(1, 0, 1)), Some(span(2, 0, 1)));
        empty.occurrences.clear();
        assert_eq!(
            evaluate_scoped_token_metrics(
                &[empty],
                ScopedExpectedTokenEvidence::default(),
                &[],
                &[],
                &[],
                &[]
            ),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );

        let mut spanless = change(Some(span(1, 0, 1)), Some(span(2, 0, 1)));
        spanless.occurrences[0] = ChangeOccurrence {
            old_span: None,
            new_span: None,
        };
        assert_eq!(
            evaluate_scoped_token_metrics(
                &[spanless],
                ScopedExpectedTokenEvidence::default(),
                &[],
                &[],
                &[],
                &[],
            ),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );
    }

    #[test]
    fn scoped_token_metrics_charge_each_occurrence_before_projection() {
        let result = evaluate_scoped_token_metrics_with_limits(
            &[change(Some(span(1, 0, 1)), Some(span(2, 0, 1)))],
            ScopedExpectedTokenEvidence::default(),
            &[],
            &[],
            &[],
            &[],
            ClassificationLimits {
                max_work: 1,
                max_output: 1,
            },
        )
        .map_err(token_metrics_error);

        assert_eq!(result, Err(SCOPED_TOKEN_METRICS_LIMITED.to_owned()));
    }

    #[test]
    fn recovered_token_metrics_use_the_emitted_semantic_hunk() {
        let old = [block(1, "aaOLDzz")];
        let new = [block(2, "aaNEWzz")];
        let old_context = span(1, 0, 7);
        let new_context = span(2, 0, 7);
        let old_event_span = span(1, 2, 5);
        let new_event_span = span(2, 2, 5);
        let changes = [change(
            Some(old_event_span.clone()),
            Some(new_event_span.clone()),
        )];
        let traces = [RecoveredAtomicDiff {
            origin: ChangeOrigin::SentenceNear,
            old_alignment_span_index: 0,
            new_alignment_span_index: 0,
            old_context,
            new_context,
            changed_occurrences: vec![RecoveredAtomicOccurrence {
                occurrence: ChangeOccurrence {
                    old_span: Some(old_event_span),
                    new_span: Some(new_event_span),
                },
                edit_range: 0..2,
            }],
            edits: vec![
                AtomicEdit {
                    old: 2..5,
                    new: 2..2,
                },
                AtomicEdit {
                    old: 5..5,
                    new: 2..5,
                },
            ],
            relation: pdfdelta_core::diff::RecoveredRelationEvidence::Near {
                old_best_score: 0,
                old_second_score: 0,
                old_best_scope: None,
                new_best_score: 0,
                new_second_score: 0,
                new_best_scope: None,
            },
        }];

        let metrics = scoped_metrics_with_recovery(
            &old,
            &new,
            &[expected("body", "OLD", "NEW")],
            &changes,
            &traces,
        );

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!(
            (
                metrics.precision,
                metrics.recall,
                metrics.f1,
                metrics.span_iou
            ),
            (1.0, 1.0, 1.0, 1.0)
        );
    }

    #[test]
    fn scoped_token_metrics_use_changed_quotes_instead_of_context() {
        let mut reviewed = expected("body", "aaOLDzz", "aaNEWzz");
        reviewed.old_changed_quote = Some("OLD".to_owned());
        reviewed.new_changed_quote = Some("NEW".to_owned());

        let metrics = scoped_metrics(
            &[block(1, "OLD aaOLDzz OLD")],
            &[block(2, "NEW aaNEWzz NEW")],
            &[reviewed],
            &[change(Some(span(1, 6, 9)), Some(span(2, 6, 9)))],
        );

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_use_disjoint_context_relative_ranges() {
        let mut reviewed = expected("body", "aaOLDxxTAILzz", "aaNEWxxHEADzz");
        reviewed.old_changed_ranges = Some(vec![
            ExpectedChangedRange { start: 2, end: 5 },
            ExpectedChangedRange { start: 7, end: 11 },
        ]);
        reviewed.new_changed_ranges = Some(vec![
            ExpectedChangedRange { start: 2, end: 5 },
            ExpectedChangedRange { start: 7, end: 11 },
        ]);
        let mut reported = change(Some(span(1, 2, 5)), Some(span(2, 2, 5)));
        reported.occurrences.push(ChangeOccurrence {
            old_span: Some(span(1, 7, 11)),
            new_span: Some(span(2, 7, 11)),
        });

        let metrics = scoped_metrics(
            &[block(1, "aaOLDxxTAILzz")],
            &[block(2, "aaNEWxxHEADzz")],
            &[reviewed],
            &[reported],
        );

        assert_eq!(metrics.expected_changed_tokens, 14);
        assert_eq!(metrics.reported_changed_tokens, 14);
        assert_eq!(metrics.true_positive_tokens, 14);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_project_collapsed_whitespace_ranges_to_raw_tokens() {
        let mut reviewed = expected("body", "aa OLDzz", "aa NEWzz");
        reviewed.old_changed_ranges = Some(vec![ExpectedChangedRange { start: 2, end: 3 }]);
        reviewed.new_changed_ranges = Some(vec![ExpectedChangedRange { start: 2, end: 3 }]);

        let metrics = scoped_metrics(
            &[block(1, "aa \n\tOLDzz")],
            &[block(2, "aa  \nNEWzz")],
            &[reviewed],
            &[change(Some(span(1, 2, 5)), Some(span(2, 2, 5)))],
        );

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_project_ranges_after_collapsed_whitespace() {
        let mut reviewed = expected("body", "aa OLDzz", "aa NEWzz");
        reviewed.old_changed_ranges = Some(vec![ExpectedChangedRange { start: 3, end: 6 }]);
        reviewed.new_changed_ranges = Some(vec![ExpectedChangedRange { start: 3, end: 6 }]);

        let metrics = scoped_metrics(
            &[block(1, "aa \n\tOLDzz")],
            &[block(2, "aa  \nNEWzz")],
            &[reviewed],
            &[change(Some(span(1, 5, 8)), Some(span(2, 5, 8)))],
        );

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_project_ranges_across_virtual_block_whitespace() {
        let mut reviewed = expected("body", "aa OLDzz", "aa NEWzz");
        reviewed.old_changed_ranges = Some(vec![ExpectedChangedRange { start: 2, end: 6 }]);
        reviewed.new_changed_ranges = Some(vec![ExpectedChangedRange { start: 2, end: 6 }]);

        let metrics = scoped_metrics(
            &[block(1, "aa"), block(2, "OLDzz")],
            &[block(3, "aa"), block(4, "NEWzz")],
            &[reviewed],
            &[change(Some(span(2, 0, 3)), Some(span(4, 0, 3)))],
        );

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_preserve_whitespace_spanning_block_boundaries() {
        let mut reviewed = expected("body", "aa OLDzz", "aa NEWzz");
        reviewed.old_changed_ranges = Some(vec![ExpectedChangedRange { start: 2, end: 3 }]);
        reviewed.new_changed_ranges = Some(vec![ExpectedChangedRange { start: 2, end: 3 }]);

        let metrics = scoped_metrics(
            &[block(1, "aa \n"), block(2, "\tOLDzz")],
            &[block(3, "aa\t"), block(4, " \nNEWzz")],
            &[reviewed],
            &[change(
                Some(group_span(
                    vec![BlockId(1), BlockId(2)],
                    BlockSeparator::Concatenate,
                    2,
                    5,
                )),
                Some(group_span(
                    vec![BlockId(3), BlockId(4)],
                    BlockSeparator::Concatenate,
                    2,
                    5,
                )),
            )],
        );

        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_allow_a_zero_token_replacement_side() {
        let mut reviewed = expected("body", "aa,zz", "aazz");
        reviewed.old_changed_quote = Some(",".to_owned());
        reviewed.new_changed_quote = Some(String::new());

        let metrics = scoped_metrics(
            &[block(1, "aa,zz")],
            &[block(2, "aazz")],
            &[reviewed],
            &[change(Some(span(1, 2, 3)), Some(span(2, 2, 2)))],
        );

        assert_eq!(metrics.expected_changed_tokens, 1);
        assert_eq!(metrics.reported_changed_tokens, 1);
        assert_eq!(metrics.true_positive_tokens, 1);
        assert_eq!((metrics.precision, metrics.recall), (1.0, 1.0));
    }

    #[test]
    fn scoped_token_metrics_penalize_overwide_and_underwide_spans() {
        let old = [block(1, "aaOLDzz")];
        let new = [block(2, "aaNEWzz")];
        let expected = [expected("body", "OLD", "NEW")];
        let overwide = scoped_metrics(
            &old,
            &new,
            &expected,
            &[change(Some(span(1, 1, 6)), Some(span(2, 1, 6)))],
        );
        assert_eq!(
            (
                overwide.expected_changed_tokens,
                overwide.reported_changed_tokens,
                overwide.true_positive_tokens
            ),
            (6, 10, 6)
        );
        assert_eq!(overwide.precision, 0.6);
        assert_eq!(overwide.recall, 1.0);
        assert_eq!(overwide.span_iou, 0.6);

        let underwide = scoped_metrics(
            &old,
            &new,
            &expected,
            &[change(Some(span(1, 3, 4)), Some(span(2, 3, 4)))],
        );
        assert_eq!(
            (
                underwide.expected_changed_tokens,
                underwide.reported_changed_tokens,
                underwide.true_positive_tokens
            ),
            (6, 2, 2)
        );
        assert_eq!(underwide.precision, 1.0);
        assert_eq!(underwide.recall, 1.0 / 3.0);
        assert_eq!(underwide.span_iou, 1.0 / 3.0);
    }

    #[test]
    fn scoped_token_metrics_deduplicate_overlaps_and_keep_sides_distinct() {
        let metrics = scoped_metrics(
            &[block(1, "aBCDe")],
            &[block(2, "aBCDe")],
            &[expected("body", "BCD", "BCD")],
            &[
                change(Some(span(1, 1, 3)), Some(span(2, 1, 3))),
                change(Some(span(1, 2, 4)), Some(span(2, 2, 4))),
            ],
        );
        assert_eq!(metrics.expected_changed_tokens, 6);
        assert_eq!(metrics.reported_changed_tokens, 6);
        assert_eq!(metrics.true_positive_tokens, 6);
    }

    #[test]
    fn scoped_token_metrics_exclude_synthetic_group_separators() {
        let old = [block(1, "ab"), block(2, "cd")];
        let new = [block(3, "ab"), block(4, "cd")];
        let metrics = scoped_metrics(
            &old,
            &new,
            &[expected("body", "ab cd", "ab cd")],
            &[change(
                Some(group_span(
                    vec![BlockId(1), BlockId(2)],
                    BlockSeparator::Space,
                    0,
                    5,
                )),
                Some(group_span(
                    vec![BlockId(3), BlockId(4)],
                    BlockSeparator::Space,
                    0,
                    5,
                )),
            )],
        );
        assert_eq!(
            (
                metrics.expected_changed_tokens,
                metrics.reported_changed_tokens
            ),
            (8, 8)
        );
        assert_eq!(metrics.span_iou, 1.0);
    }

    #[test]
    fn scoped_token_metrics_use_unicode_whitespace_separator_parity() {
        let old = [block(10, "ab\n"), block(11, "cd")];
        let new = [block(20, "ab"), block(21, "\tcd")];
        let metrics = scoped_metrics(
            &old,
            &new,
            &[expected("body", "ab cd", "ab cd")],
            &[change(
                Some(group_span(
                    vec![BlockId(10), BlockId(11)],
                    BlockSeparator::Space,
                    0,
                    5,
                )),
                Some(group_span(
                    vec![BlockId(20), BlockId(21)],
                    BlockSeparator::Space,
                    0,
                    5,
                )),
            )],
        );

        assert_eq!(metrics.expected_changed_tokens, 10);
        assert_eq!(metrics.reported_changed_tokens, 10);
        assert_eq!(metrics.true_positive_tokens, 10);
        assert_eq!(
            (metrics.precision, metrics.recall, metrics.span_iou),
            (1.0, 1.0, 1.0)
        );
    }

    #[test]
    fn scoped_token_metrics_preserve_combined_tail_across_empty_blocks() {
        let space_old = [block(30, "ab"), block(31, ""), block(32, "cd")];
        let space_new = [block(40, "ab"), block(41, ""), block(42, "cd")];
        let space = scoped_metrics(
            &space_old,
            &space_new,
            &[expected("body", "ab cd", "ab cd")],
            &[change(
                Some(group_span(
                    vec![BlockId(30), BlockId(31), BlockId(32)],
                    BlockSeparator::Space,
                    0,
                    5,
                )),
                Some(group_span(
                    vec![BlockId(40), BlockId(41), BlockId(42)],
                    BlockSeparator::Space,
                    0,
                    5,
                )),
            )],
        );
        assert_eq!(space.expected_changed_tokens, 8);
        assert_eq!(space.reported_changed_tokens, 8);
        assert_eq!(space.true_positive_tokens, 8);
        assert_eq!(
            (space.precision, space.recall, space.span_iou),
            (1.0, 1.0, 1.0)
        );

        let concatenate = scoped_metrics(
            &space_old,
            &space_new,
            &[expected("body", "abcd", "abcd")],
            &[change(
                Some(group_span(
                    vec![BlockId(30), BlockId(31), BlockId(32)],
                    BlockSeparator::Concatenate,
                    0,
                    4,
                )),
                Some(group_span(
                    vec![BlockId(40), BlockId(41), BlockId(42)],
                    BlockSeparator::Concatenate,
                    0,
                    4,
                )),
            )],
        );
        assert_eq!(concatenate.expected_changed_tokens, 8);
        assert_eq!(concatenate.reported_changed_tokens, 8);
        assert_eq!(concatenate.true_positive_tokens, 8);
        assert_eq!(
            (
                concatenate.precision,
                concatenate.recall,
                concatenate.span_iou,
            ),
            (1.0, 1.0, 1.0)
        );
    }

    #[test]
    fn scoped_token_metrics_define_empty_and_false_positive_denominators() {
        let empty = evaluate_scoped_token_metrics(
            &[],
            ScopedExpectedTokenEvidence::default(),
            &[],
            &[],
            &[],
            &[],
        )
        .expect("empty reviewed set evaluates");
        assert_eq!(
            (empty.precision, empty.recall, empty.f1, empty.span_iou),
            (1.0, 1.0, 1.0, 1.0)
        );
        assert_eq!(empty.false_positive_tokens_per_10k_unchanged, None);

        let no_unchanged = scoped_metrics(
            &[block(1, "OLD")],
            &[block(2, "NEW")],
            &[expected("body", "OLD", "NEW")],
            &[change(Some(span(1, 0, 3)), Some(span(2, 0, 3)))],
        );
        assert_eq!(no_unchanged.false_positive_tokens_per_10k_unchanged, None);

        let false_positive = scoped_metrics(
            &[block(3, "aaOLDzz")],
            &[block(4, "aaNEWzz")],
            &[expected("body", "OLD", "NEW")],
            &[change(Some(span(3, 1, 6)), Some(span(4, 1, 6)))],
        );
        assert_eq!(
            false_positive.false_positive_tokens_per_10k_unchanged,
            Some(5_000.0)
        );
    }

    #[test]
    fn scoped_token_metric_budget_exhaustion_fails_the_whole_measurement() {
        let blocks = [block(1, "a")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
            },
        }];
        let result = evaluate_scoped_token_metrics_with_limits(
            &[],
            ScopedExpectedTokenEvidence::default(),
            &scopes,
            &blocks,
            &blocks,
            &[],
            ClassificationLimits {
                max_work: 0,
                max_output: 1,
            },
        )
        .map_err(token_metrics_error);

        assert_eq!(result, Err(SCOPED_TOKEN_METRICS_LIMITED.to_owned()));
    }

    #[test]
    fn resolves_unique_scope_anchors_to_document_coordinates() {
        let scopes = [scope(
            "body",
            "old start",
            "old end",
            "new start",
            "new end",
        )];
        let resolved = resolve_revision_scopes(
            &scopes,
            &[block(10, "old start"), block(11, "old end")],
            &[block(20, "new start"), block(21, "new end")],
        )
        .expect("scope resolves");

        assert_eq!(resolved[0].id, "body");
        assert_eq!(
            resolved[0].old,
            ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 6,
                },
            }
        );
    }

    #[test]
    fn rejects_unavailable_scope_anchors_fail_closed() {
        let old = [
            block(1, "alpha beta alpha beta"),
            block(2, "split"),
            block(3, "anchor"),
        ];
        let new = [block(4, "new start and new end")];

        let missing = resolve_revision_scopes(
            &[scope("s", "missing", "beta", "new start", "new end")],
            &old,
            &new,
        )
        .expect_err("missing anchor fails");
        assert_eq!(
            missing,
            "scoped-complete scope \"s\" old start anchor is missing"
        );

        let ambiguous = resolve_revision_scopes(
            &[scope("s", "alpha", "beta", "new start", "new end")],
            &old,
            &new,
        )
        .expect_err("duplicate anchor fails");
        assert_eq!(
            ambiguous,
            "scoped-complete scope \"s\" old start anchor is ambiguous"
        );

        let segmented = resolve_revision_scopes(
            &[scope("s", "split anchor", "anchor", "new start", "new end")],
            &old,
            &new,
        )
        .expect("unique segmented anchor resolves");
        assert_eq!(
            segmented[0].old,
            ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 2,
                    scalar: 5,
                },
            }
        );

        let ambiguous_segmented = resolve_revision_scopes(
            &[scope("s", "split anchor", "anchor", "new start", "new end")],
            &[
                block(10, "split"),
                block(11, "anchor"),
                block(12, "split"),
                block(13, "anchor"),
            ],
            &new,
        )
        .expect_err("duplicate segmented anchors fail");
        assert_eq!(
            ambiguous_segmented,
            "scoped-complete scope \"s\" old start anchor is ambiguous"
        );

        let mut uncertain = block(5, "opaque evidence");
        uncertain.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        resolve_revision_scopes(
            &[scope("s", "known", "text", "new start", "new end")],
            &[block(6, "known"), block(7, "text"), uncertain],
            &new,
        )
        .expect("unrelated uncertain evidence does not invalidate exact anchors");

        let mut uncertain_anchor = block(8, "known");
        uncertain_anchor.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        assert_eq!(
            resolve_revision_scopes(
                &[scope("s", "known text", "text", "new start", "new end")],
                &[uncertain_anchor, block(9, "text")],
                &new,
            )
            .expect_err("uncertainty in any matched anchor block fails"),
            "scoped-complete scope \"s\" old start anchor is indeterminate"
        );
    }

    #[test]
    fn rejects_uncertain_scope_evidence_before_empty_change_metrics() {
        let mut uncertain = block(2, "uncertain evidence");
        uncertain.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        let old = [block(1, "old start"), uncertain, block(3, "old end")];
        let new = [block(4, "new start"), block(5, "new end")];

        let result = resolve_revision_scopes(
            &[scope(
                "body",
                "old start",
                "old end",
                "new start",
                "new end",
            )],
            &old,
            &new,
        )
        .and_then(|scopes| {
            let evidence = validate_scoped_expected_changes(&[], &scopes, &old, &new)?;
            evaluate_scoped_token_metrics(&[], evidence, &scopes, &old, &new, &[])
        });

        assert_eq!(
            result.expect_err("uncertain evidence prevents vacuous perfect metrics"),
            SCOPE_RESOLUTION_INDETERMINATE
        );
    }

    #[test]
    fn rejects_uncertain_scope_evidence_on_absent_quote_sides() {
        let scope_definition = scope("body", "old start", "old end", "new start", "new end");
        for (kind, uncertain_old, old_quote, new_quote) in [
            (ExpectedKind::Insertion, true, None, Some("added")),
            (ExpectedKind::Deletion, false, Some("removed"), None),
        ] {
            let mut old = vec![
                block(1, "old start"),
                block(2, "old evidence"),
                block(3, "old end"),
            ];
            let mut new = vec![
                block(4, "new start"),
                block(5, "new evidence"),
                block(6, "new end"),
            ];
            let uncertain = if uncertain_old {
                &mut old[1]
            } else {
                &mut new[1]
            };
            uncertain.issues.push(NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: ScalarRange { start: 0, end: 1 },
                source: TextSource {
                    atoms: Vec::new().into(),
                },
            });
            let expected = [ExpectedChange {
                id: "reviewed".to_owned(),
                kind,
                scope: Some("body".to_owned()),
                occurrence_count: None,
                old_quote: old_quote.map(str::to_owned),
                new_quote: new_quote.map(str::to_owned),
                old_changed_quote: None,
                new_changed_quote: None,
                old_changed_ranges: None,
                new_changed_ranges: None,
                note: String::new(),
            }];

            let result =
                resolve_revision_scopes(std::slice::from_ref(&scope_definition), &old, &new)
                    .and_then(|scopes| {
                        validate_scoped_expected_changes(&expected, &scopes, &old, &new)
                    });
            assert_eq!(
                result,
                Err(SCOPE_RESOLUTION_INDETERMINATE.to_owned()),
                "{kind:?} absent quote must not bypass scope evidence validation"
            );
        }
    }

    #[test]
    fn rejects_reversed_overlapping_and_crossed_scope_order() {
        let old = [block(1, "a b c d")];
        let new = [block(2, "w x y z")];

        assert_eq!(
            resolve_revision_scopes(&[scope("s", "d", "a", "w", "z")], &old, &new)
                .expect_err("reversed range fails"),
            "scoped-complete scope \"s\" has reversed old anchors"
        );
        assert_eq!(
            resolve_revision_scopes(
                &[scope("s", "bc", "ab", "w", "z")],
                &[block(3, "abcd")],
                &[block(4, "wxyz")],
            )
            .expect_err("crossed overlapping anchors fail"),
            "scoped-complete scope \"s\" has reversed old anchors"
        );

        let overlapping = [
            scope("first", "a", "c", "w", "x"),
            scope("second", "b", "d", "y", "z"),
        ];
        assert_eq!(
            resolve_revision_scopes(&overlapping, &old, &new).expect_err("shared boundary fails"),
            "scoped-complete scopes are not strictly ordered and disjoint on old side"
        );

        let shared_boundary = [
            scope("first", "a", "c", "w", "x"),
            scope("second", "c", "d", "y", "z"),
        ];
        assert_eq!(
            resolve_revision_scopes(&shared_boundary, &old, &new)
                .expect_err("shared boundary fails"),
            "scoped-complete scopes are not strictly ordered and disjoint on old side"
        );

        let crossed = [
            scope("first", "a", "b", "y", "z"),
            scope("second", "c", "d", "w", "x"),
        ];
        assert_eq!(
            resolve_revision_scopes(&crossed, &old, &new).expect_err("crossed order fails"),
            "scoped-complete scopes are not strictly ordered and disjoint on new side"
        );
    }

    #[test]
    fn scope_resolution_obeys_the_shared_diagnostic_budget() {
        let limits = DiagnosticLimits::default().with_max_scan_work(0);
        let error = resolve_revision_scopes_with_limits(
            &[scope("s", "a", "b", "c", "d")],
            &[block(1, "a b")],
            &[block(2, "c d")],
            limits,
        )
        .expect_err("zero budget fails");

        assert_eq!(error, SCOPE_RESOLUTION_LIMITED);
    }

    #[test]
    fn classifies_only_changes_fully_contained_by_one_scope() {
        let old = [block(1, "outside reviewed outside")];
        let new = [block(2, "outside reviewed outside")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 8,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 15,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 8,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 15,
                },
            },
        }];
        let changes = [
            change(Some(span(1, 0, 7)), Some(span(2, 0, 7))),
            change(Some(span(1, 8, 16)), Some(span(2, 8, 16))),
            change(None, Some(span(2, 9, 10))),
            change(Some(span(1, 10, 11)), None),
        ];

        let classified = classify_scoped_changes(&changes, &scopes, &old, &new)
            .expect("valid coordinates classify");
        assert_eq!(
            classified,
            [
                ScopedChange {
                    scope_id: "body".to_owned(),
                    change_index: 1,
                },
                ScopedChange {
                    scope_id: "body".to_owned(),
                    change_index: 2,
                },
                ScopedChange {
                    scope_id: "body".to_owned(),
                    change_index: 3,
                },
            ]
        );
    }

    #[test]
    fn projects_reordered_context_only_when_selected_coordinates_are_contiguous() {
        let old = [block(1, "a"), block(2, "b"), block(3, "c")];
        let new = [block(4, "A"), block(5, "B"), block(6, "C")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
            },
        }];
        let selected_old = group_span(
            vec![BlockId(1), BlockId(3), BlockId(2)],
            BlockSeparator::Concatenate,
            2,
            3,
        );
        let selected_new = group_span(
            vec![BlockId(4), BlockId(6), BlockId(5)],
            BlockSeparator::Concatenate,
            2,
            3,
        );
        let actual = change(Some(selected_old.clone()), Some(selected_new.clone()));
        assert_eq!(
            classify_scoped_changes(std::slice::from_ref(&actual), &scopes, &old, &new,)
                .expect("selected block projects despite unused reordered context"),
            [ScopedChange {
                scope_id: "body".to_owned(),
                change_index: 0,
            }]
        );

        let expected = [expected("body", "b", "B")];
        let evidence = validate_scoped_expected_changes(&expected, &scopes, &old, &new)
            .expect("expected quote resolves in the selected block");
        let metrics = evaluate_scoped_token_metrics(
            std::slice::from_ref(&actual),
            evidence,
            &scopes,
            &old,
            &new,
            &[],
        )
        .expect("token projection ignores unused reordered context");
        assert_eq!(
            (
                metrics.expected_changed_tokens,
                metrics.reported_changed_tokens,
                metrics.true_positive_tokens,
            ),
            (2, 2, 2)
        );

        let zero_width_old = group_span(
            vec![BlockId(1), BlockId(3), BlockId(2)],
            BlockSeparator::Concatenate,
            3,
            3,
        );
        let zero_width_new = group_span(
            vec![BlockId(4), BlockId(6), BlockId(5)],
            BlockSeparator::Concatenate,
            3,
            3,
        );
        assert_eq!(
            classify_scoped_changes(
                &[change(Some(zero_width_old), Some(zero_width_new))],
                &scopes,
                &old,
                &new,
            )
            .expect("zero-width boundary projects to the selected block"),
            [ScopedChange {
                scope_id: "body".to_owned(),
                change_index: 0,
            }]
        );

        let noncontiguous_old = group_span(
            vec![BlockId(1), BlockId(3), BlockId(2)],
            BlockSeparator::Concatenate,
            1,
            3,
        );
        let noncontiguous_new = group_span(
            vec![BlockId(4), BlockId(6), BlockId(5)],
            BlockSeparator::Concatenate,
            1,
            3,
        );
        assert_eq!(
            classify_scoped_changes(
                &[change(Some(noncontiguous_old), Some(noncontiguous_new))],
                &scopes,
                &old,
                &new,
            ),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );
    }

    #[test]
    fn classifies_proven_regions_only_when_all_sides_share_one_scope() {
        let old = [block(1, "outside reviewed outside")];
        let new = [block(2, "outside reviewed outside")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 8,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 15,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 8,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 15,
                },
            },
        }];
        let region = |old_span: Option<TextSpan>, new_span: Option<TextSpan>| ProvenChangedRegion {
            proof: if old_span.is_some() && new_span.is_some() {
                pdfdelta_core::diff::ChangedRegionProof::ExactTokenMultisetMismatch
            } else {
                pdfdelta_core::diff::ChangedRegionProof::OneSidedNonEmptyRange
            },
            old_span,
            new_span,
            confidence: pdfdelta_core::diff::Confidence::High,
        };
        let regions = [
            region(Some(span(1, 8, 16)), Some(span(2, 8, 16))),
            region(None, Some(span(2, 9, 10))),
            region(Some(span(1, 0, 7)), Some(span(2, 0, 7))),
        ];
        let classified = classify_scoped_proven_changed_regions(&regions, &scopes, &old, &new)
            .expect("valid coordinates classify");
        assert_eq!(
            classified,
            [
                ScopedProvenChangedRegion {
                    scope_id: "body".to_owned(),
                    region_index: 0,
                },
                ScopedProvenChangedRegion {
                    scope_id: "body".to_owned(),
                    region_index: 1,
                },
            ]
        );

        let mismatched = [region(Some(span(1, 0, 7)), Some(span(2, 8, 16)))];
        assert_eq!(
            classify_scoped_proven_changed_regions(&mismatched, &scopes, &old, &new),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );
        let crossing = [region(Some(span(1, 7, 9)), None)];
        assert_eq!(
            classify_scoped_proven_changed_regions(&crossing, &scopes, &old, &new),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );
    }

    #[test]
    fn rejects_crossing_different_scope_and_mixed_occurrence_changes() {
        let old = [block(1, "aaaabbbbcccc")];
        let new = [block(2, "aaaabbbbcccc")];
        let scopes = [
            ResolvedScope {
                id: "first".to_owned(),
                old: ResolvedScopeRange {
                    start: ScopeCoordinate {
                        block_order: 0,
                        scalar: 4,
                    },
                    end: ScopeCoordinate {
                        block_order: 0,
                        scalar: 7,
                    },
                },
                new: ResolvedScopeRange {
                    start: ScopeCoordinate {
                        block_order: 0,
                        scalar: 4,
                    },
                    end: ScopeCoordinate {
                        block_order: 0,
                        scalar: 7,
                    },
                },
            },
            ResolvedScope {
                id: "second".to_owned(),
                old: ResolvedScopeRange {
                    start: ScopeCoordinate {
                        block_order: 0,
                        scalar: 8,
                    },
                    end: ScopeCoordinate {
                        block_order: 0,
                        scalar: 11,
                    },
                },
                new: ResolvedScopeRange {
                    start: ScopeCoordinate {
                        block_order: 0,
                        scalar: 8,
                    },
                    end: ScopeCoordinate {
                        block_order: 0,
                        scalar: 11,
                    },
                },
            },
        ];
        for invalid in [
            change(Some(span(1, 3, 5)), Some(span(2, 4, 5))),
            change(Some(span(1, 4, 5)), Some(span(2, 8, 9))),
            Change {
                kind: ChangeKind::Replacement,
                occurrences: vec![
                    ChangeOccurrence {
                        old_span: Some(span(1, 4, 5)),
                        new_span: Some(span(2, 4, 5)),
                    },
                    ChangeOccurrence {
                        old_span: Some(span(1, 0, 1)),
                        new_span: Some(span(2, 0, 1)),
                    },
                ],
                confidence: Confidence::High,
                tags: Vec::new(),
            },
        ] {
            assert_eq!(
                classify_scoped_changes(&[invalid], &scopes, &old, &new),
                Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
            );
        }
    }

    #[test]
    fn virtual_separator_scope_requires_both_source_neighbors() {
        let blocks = [block(1, "ab"), block(2, "cd")];
        let scopes =
            resolve_revision_scopes(&[scope("body", "ab", "cd", "ab", "cd")], &blocks, &blocks)
                .expect("scope anchors resolve");
        for (start, end) in [(2, 3), (1, 3), (2, 4)] {
            let selected = group_span(
                vec![BlockId(1), BlockId(2)],
                BlockSeparator::Space,
                start,
                end,
            );
            for change in [
                change(Some(selected.clone()), None),
                change(None, Some(selected)),
            ] {
                assert_eq!(
                    classify_scoped_changes(
                        std::slice::from_ref(&change),
                        &scopes,
                        &blocks,
                        &blocks
                    )
                    .expect("both separator neighbors belong to the same scope"),
                    [ScopedChange {
                        scope_id: "body".to_owned(),
                        change_index: 0
                    }]
                );
                for scope_block in [0, 1] {
                    let range = ResolvedScopeRange {
                        start: ScopeCoordinate {
                            block_order: scope_block,
                            scalar: 0,
                        },
                        end: ScopeCoordinate {
                            block_order: scope_block,
                            scalar: 1,
                        },
                    };
                    let partial = [ResolvedScope {
                        id: "partial".to_owned(),
                        old: range,
                        new: range,
                    }];
                    assert_eq!(
                        classify_scoped_changes(
                            std::slice::from_ref(&change),
                            &partial,
                            &blocks,
                            &blocks
                        ),
                        Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
                    );
                }
            }
        }
        for (texts, start) in [(["", "ab"], 0), (["ab", ""], 2)] {
            let blocks = [block(1, texts[0]), block(2, texts[1])];
            let selected = group_span(
                vec![BlockId(1), BlockId(2)],
                BlockSeparator::Space,
                start,
                start + 1,
            );
            assert_eq!(
                classify_scoped_changes(&[change(Some(selected), None)], &[], &blocks, &blocks),
                Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
            );
        }
    }

    #[test]
    fn classifies_zero_width_sides_within_grouped_blocks() {
        let old = [block(1, "prefix"), block(2, "abc")];
        let new = [block(3, "prefix"), block(4, "aXbc")];
        let scopes = resolve_revision_scopes(
            &[scope("body", "prefix", "abc", "prefix", "aXbc")],
            &old,
            &new,
        )
        .expect("scope anchors resolve");
        for separator in [BlockSeparator::Concatenate, BlockSeparator::Space] {
            let start = 7 + usize::from(separator == BlockSeparator::Space);
            let changes = [change(
                Some(group_span(
                    vec![BlockId(1), BlockId(2)],
                    separator,
                    start,
                    start,
                )),
                Some(group_span(
                    vec![BlockId(3), BlockId(4)],
                    separator,
                    start,
                    start + 1,
                )),
            )];
            assert_eq!(
                classify_scoped_changes(&changes, &scopes, &old, &new)
                    .expect("an interior zero-width side has source coordinates"),
                [ScopedChange {
                    scope_id: "body".to_owned(),
                    change_index: 0
                }]
            );
        }
    }

    #[test]
    fn reconstructs_group_separator_coordinates_and_rejects_invalid_block_order() {
        let old = [block(1, "ab"), block(2, "cd")];
        let new = [block(3, "ab"), block(4, "cd")];
        let scopes = [ResolvedScope {
            id: "joined".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 1,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 1,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
            },
        }];
        let valid = change(
            Some(group_span(
                vec![BlockId(1), BlockId(2)],
                BlockSeparator::Space,
                1,
                4,
            )),
            Some(group_span(
                vec![BlockId(3), BlockId(4)],
                BlockSeparator::Space,
                1,
                4,
            )),
        );
        assert_eq!(
            classify_scoped_changes(&[valid], &scopes, &old, &new)
                .expect("space-separated group resolves"),
            [ScopedChange {
                scope_id: "joined".to_owned(),
                change_index: 0,
            }]
        );

        let reversed = change(
            Some(group_span(
                vec![BlockId(2), BlockId(1)],
                BlockSeparator::Concatenate,
                0,
                1,
            )),
            Some(span(3, 0, 1)),
        );
        assert_eq!(
            classify_scoped_changes(&[reversed], &scopes, &old, &new),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );
    }

    #[test]
    fn whitespace_boundaries_do_not_insert_a_second_separator() {
        let newline_blocks = [block(1, "a\n"), block(2, "bc")];
        let newline_order = HashMap::from([(BlockId(1), 0), (BlockId(2), 1)]);
        let mut budget = ClassificationBudget::default();
        let endpoint = span_range_with_limits(
            &group_span(vec![BlockId(1), BlockId(2)], BlockSeparator::Space, 2, 3),
            &newline_blocks,
            &newline_order,
            &mut budget,
            ClassificationLimits::default(),
        )
        .expect("newline suppresses the virtual separator");
        assert_eq!(
            endpoint,
            ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
            }
        );

        let tab_blocks = [block(3, "a"), block(4, "\tbc")];
        let tab_order = HashMap::from([(BlockId(3), 0), (BlockId(4), 1)]);
        let mut budget = ClassificationBudget::default();
        let endpoint = span_range_with_limits(
            &group_span(vec![BlockId(3), BlockId(4)], BlockSeparator::Space, 2, 3),
            &tab_blocks,
            &tab_order,
            &mut budget,
            ClassificationLimits::default(),
        )
        .expect("tab suppresses the virtual separator");
        assert_eq!(
            endpoint.start,
            ScopeCoordinate {
                block_order: 1,
                scalar: 1,
            }
        );

        let crossing = span_range_with_limits(
            &group_span(vec![BlockId(1), BlockId(2)], BlockSeparator::Space, 1, 3),
            &newline_blocks,
            &newline_order,
            &mut ClassificationBudget::default(),
            ClassificationLimits::default(),
        )
        .expect("cross-block span resolves");
        let scope = [ResolvedScope {
            id: "second".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 1,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 1,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 1,
                },
            },
        }];
        assert_eq!(
            containing_scope(
                crossing,
                &scope,
                "old",
                &mut ClassificationBudget::default(),
                ClassificationLimits::default(),
            ),
            Err(SCOPED_CHANGE_INDETERMINATE.to_owned())
        );
    }

    #[test]
    fn empty_middle_blocks_preserve_the_combined_separator_tail() {
        let blocks = [block(10, "a"), block(11, ""), block(12, "b")];
        let order = HashMap::from([(BlockId(10), 0), (BlockId(11), 1), (BlockId(12), 2)]);
        let ids = vec![BlockId(10), BlockId(11), BlockId(12)];
        let spaced = span_range_with_limits(
            &group_span(ids.clone(), BlockSeparator::Space, 2, 3),
            &blocks,
            &order,
            &mut ClassificationBudget::default(),
            ClassificationLimits::default(),
        )
        .expect("the empty middle block does not add a second virtual space");
        assert_eq!(
            spaced,
            ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 2,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 2,
                    scalar: 0,
                },
            }
        );

        let concatenated = span_range_with_limits(
            &group_span(ids, BlockSeparator::Concatenate, 1, 2),
            &blocks,
            &order,
            &mut ClassificationBudget::default(),
            ClassificationLimits::default(),
        )
        .expect("concatenation remains unchanged across an empty block");
        assert_eq!(concatenated, spaced);
    }

    #[test]
    fn classification_budget_exhaustion_discards_all_partial_output() {
        let old = [block(1, "inside")];
        let new = [block(2, "inside")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 5,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 5,
                },
            },
        }];
        let changes = [
            change(Some(span(1, 0, 1)), Some(span(2, 0, 1))),
            change(Some(span(1, 1, 2)), Some(span(2, 1, 2))),
        ];

        assert_eq!(
            classify_scoped_changes_with_limits(
                &changes,
                &scopes,
                &old,
                &new,
                ClassificationLimits {
                    max_work: 32,
                    max_output: 65_536,
                },
            ),
            Err(SCOPED_CHANGE_LIMITED.to_owned())
        );
    }

    #[test]
    fn expected_quotes_are_resolved_only_inside_their_referenced_scope() {
        let old = [block(1, "target gap target")];
        let new = [block(2, "target gap target")];
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 11,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 16,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 11,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 16,
                },
            },
        }];
        validate_scoped_expected_changes(
            &[expected("body", "target", "target")],
            &scopes,
            &old,
            &new,
        )
        .expect("the duplicate outside the scope does not create ambiguity");

        assert_eq!(
            validate_scoped_expected_changes(
                &[expected("body", "gap", "target")],
                &scopes,
                &old,
                &new,
            ),
            Err(
                "scoped-complete expected change \"reviewed\" old quote is missing within scope \"body\""
                    .to_owned()
            )
        );
    }

    #[test]
    fn unique_wrapped_expected_quote_is_allowed_but_ambiguous_and_uncertain_quotes_fail_closed() {
        let scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 4,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 4,
                },
            },
        }];
        let old = [block(1, "split"), block(2, "quote")];
        let new = [block(3, "split"), block(4, "quote")];
        let wrapped = validate_scoped_expected_changes(
            &[expected("body", "split quote", "split quote")],
            &scopes,
            &old,
            &new,
        )
        .expect("a unique wrapped quote is valid inside a reviewed scope");
        assert_eq!(
            wrapped.old,
            [
                TokenInterval {
                    block_order: 0,
                    start: 0,
                    end: 5,
                },
                TokenInterval {
                    block_order: 1,
                    start: 0,
                    end: 5,
                },
            ]
        );

        let ambiguous_scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 3,
                    scalar: 4,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 3,
                    scalar: 4,
                },
            },
        }];
        let ambiguous_old = [
            block(10, "split"),
            block(11, "quote"),
            block(12, "split"),
            block(13, "quote"),
        ];
        let ambiguous_new = [
            block(20, "split"),
            block(21, "quote"),
            block(22, "split"),
            block(23, "quote"),
        ];
        assert_eq!(
            validate_scoped_expected_changes(
                &[expected("body", "split quote", "split quote")],
                &ambiguous_scopes,
                &ambiguous_old,
                &ambiguous_new,
            ),
            Err(
                "scoped-complete expected change \"reviewed\" old quote is ambiguous within scope \"body\""
                    .to_owned()
            )
        );

        let mut uncertain_old = block(6, "uncertain peer");
        uncertain_old.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        let uncertain_scopes = [ResolvedScope {
            id: "body".to_owned(),
            old: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 1,
                    scalar: 13,
                },
            },
            new: ResolvedScopeRange {
                start: ScopeCoordinate {
                    block_order: 0,
                    scalar: 0,
                },
                end: ScopeCoordinate {
                    block_order: 0,
                    scalar: 5,
                },
            },
        }];
        assert_eq!(
            validate_scoped_expected_changes(
                &[expected("body", "target", "target")],
                &uncertain_scopes,
                &[block(5, "target"), uncertain_old],
                &[block(7, "target"), block(8, "trusted peer")],
            ),
            Err(
                "scoped-complete expected change \"reviewed\" old quote is indeterminate within scope \"body\""
                    .to_owned()
            )
        );
    }
}
