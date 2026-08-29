use std::collections::HashMap;

use pdfdelta_core::{layout::BlockId, normalize::BlockText};

use super::{
    ExpectedScope,
    revision_diagnostics::{
        DiagnosticBudget, DiagnosticLimits, QuoteLocateOutcome, QuoteLocation,
        locate_scope_anchor_quote,
    },
};

const SCOPE_RESOLUTION_LIMITED: &str =
    "scoped-complete scope anchor resolution reached its resource limit";
const SCOPE_RESOLUTION_INDETERMINATE: &str =
    "scoped-complete scope anchor resolution is indeterminate";

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

#[cfg(test)]
mod tests {
    use pdfdelta_core::normalize::{
        ComparableToken, MappedText, NormalizationIssue, NormalizationIssueKind, ScalarRange,
        TextSource,
    };

    use super::super::QuoteScope;
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
}
