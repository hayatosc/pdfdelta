use std::collections::HashMap;

use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::{Change, TextSpan},
    layout::BlockId,
    normalize::BlockText,
};

use super::{
    ExpectedChange, ExpectedScope,
    revision_diagnostics::{
        DiagnosticBudget, DiagnosticLimits, QuoteLocateOutcome, QuoteLocation, ScopedQuoteRange,
        locate_scope_anchor_quote, locate_scoped_quote,
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

#[derive(Clone, Copy)]
struct ClassificationLimits {
    max_work: usize,
    max_output: usize,
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
    first_selected: Option<ScopeCoordinate>,
    last_selected: Option<ScopeCoordinate>,
    first_selected_synthetic: bool,
    last_selected_synthetic: bool,
}

impl SpanProjection {
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
        if selected {
            if let Some(coordinate) = coordinate {
                self.first_selected.get_or_insert(coordinate);
                self.last_selected = Some(coordinate);
                self.last_selected_synthetic = false;
            } else if scalar {
                if self.first_selected.is_none() {
                    self.first_selected_synthetic = true;
                }
                self.last_selected_synthetic = true;
            }
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
    ) -> Result<QuoteLocation, String> {
        match locate_scope_anchor_quote(self.blocks, quote, self.budget, self.limits) {
            Ok(QuoteLocateOutcome::Unique(location))
                if self.order.contains_key(&location.block) =>
            {
                Ok(location)
            }
            Ok(QuoteLocateOutcome::Unique(_)) | Ok(QuoteLocateOutcome::Indeterminate) | Err(_) => {
                Err(anchor_unavailable(
                    scope,
                    self.side,
                    anchor,
                    "indeterminate",
                ))
            }
            Ok(QuoteLocateOutcome::Missing) => {
                Err(anchor_unavailable(scope, self.side, anchor, "missing"))
            }
            Ok(QuoteLocateOutcome::Segmented) => {
                Err(anchor_unavailable(scope, self.side, anchor, "segmented"))
            }
            Ok(QuoteLocateOutcome::Ambiguous) => {
                Err(anchor_unavailable(scope, self.side, anchor, "ambiguous"))
            }
            Ok(QuoteLocateOutcome::Limited) => Err(SCOPE_RESOLUTION_LIMITED.to_owned()),
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
            block_order: self.order[&start.block],
            scalar: start.scalar_range.start,
        };
        let start_end = ScopeCoordinate {
            block_order: self.order[&start.block],
            scalar: start.scalar_range.end.checked_sub(1).ok_or_else(|| {
                anchor_unavailable(&scope.id, self.side, "start", "indeterminate")
            })?,
        };
        let end_begin = ScopeCoordinate {
            block_order: self.order[&end.block],
            scalar: end.scalar_range.start,
        };
        let end_end =
            ScopeCoordinate {
                block_order: self.order[&end.block],
                scalar: end.scalar_range.end.checked_sub(1).ok_or_else(|| {
                    anchor_unavailable(&scope.id, self.side, "end", "indeterminate")
                })?,
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

fn span_range_with_limits(
    span: &TextSpan,
    blocks: &[BlockText],
    order: &HashMap<BlockId, usize>,
    budget: &mut ClassificationBudget,
    limits: ClassificationLimits,
) -> Result<ResolvedScopeRange, String> {
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
    let mut previous_order = None;
    let mut combined_last_whitespace = None;
    for (position, block_id) in span.blocks.iter().enumerate() {
        budget.charge_work(1, limits)?;
        let block_order = *order
            .get(block_id)
            .ok_or_else(|| SCOPED_CHANGE_INDETERMINATE.to_owned())?;
        if let Some(previous) = previous_order
            && block_order != budget.checked_add(previous, 1)?
        {
            return Err(SCOPED_CHANGE_INDETERMINATE.to_owned());
        }
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
            && separator == BlockSeparator::Space
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
        previous_order = Some(block_order);
    }
    if span.comparable_range.end > projection.comparable_offset
        || span.canonical_range.end > projection.scalar_offset
        || projection.first_selected.is_none()
        || projection.last_selected.is_none()
        || projection.first_selected_synthetic
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

pub(super) fn validate_scoped_expected_changes(
    changes: &[ExpectedChange],
    scopes: &[ResolvedScope],
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
) -> Result<(), String> {
    let mut budget = DiagnosticBudget::default();
    let limits = DiagnosticLimits::default();
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
        for (side, quote, blocks, range) in [
            ("old", change.old_quote.as_deref(), old_blocks, scope.old),
            ("new", change.new_quote.as_deref(), new_blocks, scope.new),
        ] {
            let Some(quote) = quote else { continue };
            let outcome = locate_scoped_quote(
                blocks,
                quote,
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
            let unavailable = match outcome {
                QuoteLocateOutcome::Unique(_) | QuoteLocateOutcome::Segmented => continue,
                QuoteLocateOutcome::Missing => "missing",
                QuoteLocateOutcome::Ambiguous => "ambiguous",
                QuoteLocateOutcome::Indeterminate => "indeterminate",
                QuoteLocateOutcome::Limited => {
                    return Err(SCOPED_EXPECTED_CHANGE_LIMITED.to_owned());
                }
            };
            return Err(expected_quote_unavailable(
                change,
                scope_id,
                side,
                unavailable,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::diff::{ChangeKind, ChangeOccurrence, Confidence, TokenRange};
    use pdfdelta_core::normalize::{
        ComparableToken, MappedText, NormalizationIssue, NormalizationIssueKind, ScalarRange,
        TextSource,
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

    fn scope(
        id: &str,
        old_start: &str,
        old_end: &str,
        new_start: &str,
        new_end: &str,
    ) -> ExpectedScope {
        ExpectedScope {
            id: id.to_owned(),
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
            old_quote: Some(old.to_owned()),
            new_quote: Some(new.to_owned()),
            note: String::new(),
        }
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
        .expect_err("segmented anchor fails");
        assert_eq!(
            segmented,
            "scoped-complete scope \"s\" old start anchor is segmented"
        );

        let mut uncertain = block(5, "other text");
        uncertain.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource { atoms: Vec::new() },
        });
        let indeterminate = resolve_revision_scopes(
            &[scope("s", "known", "text", "new start", "new end")],
            &[block(6, "known"), uncertain],
            &new,
        )
        .expect_err("known anchor with uncertain peer evidence fails");
        assert_eq!(
            indeterminate,
            "scoped-complete scope \"s\" old start anchor is indeterminate"
        );
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
        validate_scoped_expected_changes(
            &[expected("body", "split quote", "split quote")],
            &scopes,
            &old,
            &new,
        )
        .expect("a unique wrapped quote is valid inside a reviewed scope");

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

        let mut uncertain_old = block(5, "target");
        uncertain_old.issues.push(NormalizationIssue {
            kind: NormalizationIssueKind::AmbiguousLineBreak,
            raw_range: ScalarRange { start: 0, end: 1 },
            source: TextSource { atoms: Vec::new() },
        });
        let uncertain_scopes = [ResolvedScope {
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
        assert_eq!(
            validate_scoped_expected_changes(
                &[expected("body", "target", "target")],
                &uncertain_scopes,
                &[uncertain_old],
                &[block(6, "target")],
            ),
            Err(
                "scoped-complete expected change \"reviewed\" old quote is indeterminate within scope \"body\""
                    .to_owned()
            )
        );
    }
}
