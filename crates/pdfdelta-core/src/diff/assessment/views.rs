use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{
    Error, Result,
    alignment::BlockSeparator,
    layout::{BlockId, BlockRole, TrustedRunDescriptor, TrustedRunId, TrustedRunInterval},
    normalize::{BlockText, ComparableToken, NormalizationKind},
};

use super::super::{GroupText, SentenceRecoveryInput, Side, TextSpan};
use super::{SourceInterval, charge};

mod anchors;

/// A source-backed correspondence domain with independently established local order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LocalDomain {
    pub(super) old_span: TextSpan,
    pub(super) new_span: TextSpan,
    /// Both closing views were complete source-bounded single views. Only
    /// these domains may publish an equal range without an edit script.
    pub(super) source_bounded: bool,
}

/// Source-ordered local domain keyed by view order and span starts.
type SortedDomain = ((usize, usize, usize, usize), LocalDomain);

#[derive(Default)]
pub(super) struct Discovery {
    pub(super) domains: Vec<LocalDomain>,
    pub(super) anchors: Vec<LocalDomain>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewKind {
    Trusted(TrustedRunId),
    Untrusted(usize),
}

struct View {
    kind: ViewKind,
    source_order: usize,
    block_indices: Vec<usize>,
    group: GroupText,
    source_bounded: bool,
    /// Every source position signature describes horizontal left-to-right text.
    /// A source-bounded whole-view anchor may only close its own domain for
    /// such text; vertical or tilted blocks stay unresolved.
    horizontal_text: bool,
    /// Token range of every member block inside this view's group, in member
    /// order. A positioned equality is adopted per complete original block,
    /// never per arbitrary substring.
    block_ranges: Vec<std::ops::Range<usize>>,
    /// A block may close a positioned equality as one complete original
    /// block: complete source evidence, horizontal left-to-right text, one
    /// page and one exact position per token. A trusted run member keeps the
    /// run-level veto everywhere else but is not a fragment of the view when
    /// its own whole source block is proven.
    block_candidates: Vec<bool>,
    /// Exact per-token source positions of a source-bounded view. A whole-view
    /// anchor only closes its own domain when both sides carry the same
    /// positions on the same page, so a moved singleton keeps its move or
    /// order obligation. Each entry is the first source glyph of one canonical
    /// token, not the full glyph geometry of the block.
    position_signatures: Vec<crate::normalize::PositionSignature>,
    /// Single source page of a source-bounded view. A whole-view anchor only
    /// closes its own domain between views on the same page; page
    /// correspondence beyond that stays unresolved.
    page: Option<u32>,
    /// Exact per-token source metadata of the whole view, including trusted
    /// runs and views that cannot close a domain themselves: the first source
    /// glyph's position signature and single page of each canonical token, or
    /// `None` where the token has no source, no complete signature or spans
    /// multiple pages. The positioned-equality veto compares occurrences in
    /// every view through this metadata instead of excluding views that are
    /// not candidates themselves.
    token_positions: Vec<Option<crate::normalize::PositionSignature>>,
    token_pages: Vec<Option<u32>>,
}

#[derive(Clone, Copy)]
struct RunMember {
    block_index: usize,
    interval: TrustedRunInterval,
}

#[derive(Clone, Copy)]
struct Occurrence {
    view_index: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct AnchorHit {
    input_index: usize,
    old_view: usize,
    new_view: usize,
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

#[derive(Default)]
struct OccurrenceSummary {
    // A second occurrence conclusively refutes uniqueness.
    count: usize,
    first: Option<Occurrence>,
}

enum SearchResult {
    Complete(OccurrenceSummary),
    BudgetExceeded,
}

/// Discovers complete local correspondence domains in trusted reading-order runs.
///
/// A domain is emitted only after every supplied anchor has been checked for exact
/// token equality and globally unique occurrence across the available views. A
/// shared-work budget exhaustion therefore returns no newly inferred domain; callers
/// may safely retain domains discovered by an earlier completed invocation.
pub(super) fn discover(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    exact_anchors: &[(TextSpan, TextSpan)],
    remaining_work: &mut usize,
    max_ranges: usize,
) -> Result<Discovery> {
    if max_ranges == 0 {
        return Err(super::invalid(
            "local-domain range limit must be greater than zero",
        ));
    }
    if recovery.old_trusted_run_intervals.len() != sides[0].blocks.len()
        || recovery.new_trusted_run_intervals.len() != sides[1].blocks.len()
    {
        return Err(super::invalid(
            "trusted run interval metadata must match normalized blocks",
        ));
    }
    if *remaining_work == 0 || sides.iter().any(|side| side.blocks.is_empty()) {
        return Ok(Discovery::default());
    }

    let old_descriptors = recovery
        .old_trusted_run_evidence
        .map(|evidence| evidence.descriptors);
    let new_descriptors = recovery
        .new_trusted_run_evidence
        .map(|evidence| evidence.descriptors);
    let Some(old_views) = build_views(
        sides[0],
        recovery.old_trusted_run_intervals,
        old_descriptors,
        remaining_work,
    )?
    else {
        return Ok(Discovery::default());
    };
    let Some(new_views) = build_views(
        sides[1],
        recovery.new_trusted_run_intervals,
        new_descriptors,
        remaining_work,
    )?
    else {
        return Ok(Discovery::default());
    };
    if old_views.is_empty() || new_views.is_empty() {
        return Ok(Discovery::default());
    }
    let mut pair_anchors = BTreeMap::<(usize, usize), Vec<AnchorHit>>::new();
    let Some(seeds) = anchors::discover(
        [&old_views, &new_views],
        recovery.min_tokens,
        remaining_work,
        max_ranges,
    )?
    else {
        return Ok(Discovery::default());
    };
    for anchor in seeds {
        if !view_is_anchorable(&old_views[anchor.old_view])
            || !view_is_anchorable(&new_views[anchor.new_view])
        {
            continue;
        }
        let old_span = old_views[anchor.old_view]
            .group
            .span(anchor.old_start, anchor.old_end);
        let new_span = new_views[anchor.new_view]
            .group
            .span(anchor.new_start, anchor.new_end);
        if !compatible_roles(sides, [&old_span, &new_span], remaining_work)?
            || super::span_has_source_issues(sides[0], &old_span, remaining_work)?
            || super::span_has_source_issues(sides[1], &new_span, remaining_work)?
        {
            continue;
        }
        pair_anchors
            .entry((anchor.old_view, anchor.new_view))
            .or_default()
            .push(anchor);
    }
    for (input_index, (old_anchor, new_anchor)) in exact_anchors.iter().enumerate() {
        let Some(old_tokens) = anchor_tokens(sides[0], old_anchor, remaining_work)? else {
            continue;
        };
        let Some(new_tokens) = anchor_tokens(sides[1], new_anchor, remaining_work)? else {
            continue;
        };
        if old_tokens.is_empty() || new_tokens.is_empty() {
            continue;
        }
        let Some(equal_cost) = old_tokens.len().checked_add(1) else {
            *remaining_work = 0;
            return Ok(Discovery::default());
        };
        if !charge(remaining_work, equal_cost) {
            return Ok(Discovery::default());
        }
        if old_tokens != new_tokens {
            continue;
        }

        let old_occurrences = match search_views(&old_views, &old_tokens, remaining_work)? {
            SearchResult::Complete(summary) => summary,
            SearchResult::BudgetExceeded => return Ok(Discovery::default()),
        };
        let new_occurrences = match search_views(&new_views, &new_tokens, remaining_work)? {
            SearchResult::Complete(summary) => summary,
            SearchResult::BudgetExceeded => return Ok(Discovery::default()),
        };
        let (Some(old_occurrence), Some(new_occurrence)) = (
            unique_occurrence(old_occurrences),
            unique_occurrence(new_occurrences),
        ) else {
            continue;
        };
        if !view_is_anchorable(&old_views[old_occurrence.view_index])
            || !view_is_anchorable(&new_views[new_occurrence.view_index])
        {
            continue;
        }
        let old_span = old_views[old_occurrence.view_index]
            .group
            .span(old_occurrence.start, old_occurrence.end);
        let new_span = new_views[new_occurrence.view_index]
            .group
            .span(new_occurrence.start, new_occurrence.end);
        if !compatible_roles(sides, [&old_span, &new_span], remaining_work)? {
            continue;
        }
        pair_anchors
            .entry((old_occurrence.view_index, new_occurrence.view_index))
            .or_default()
            .push(AnchorHit {
                input_index,
                old_view: old_occurrence.view_index,
                new_view: new_occurrence.view_index,
                old_start: old_occurrence.start,
                old_end: old_occurrence.end,
                new_start: new_occurrence.start,
                new_end: new_occurrence.end,
            });
    }

    if !add_source_end_anchors([&old_views, &new_views], &mut pair_anchors, remaining_work)? {
        return Ok(Discovery::default());
    }

    let mut exact_domains = Vec::new();
    for anchor in pair_anchors
        .values()
        .flatten()
        .filter(|anchor| anchor.input_index != usize::MAX)
    {
        let old = &old_views[anchor.old_view];
        let new = &new_views[anchor.new_view];
        if old.source_bounded || new.source_bounded {
            continue;
        }
        if !charge(
            remaining_work,
            old.group
                .blocks
                .len()
                .saturating_add(new.group.blocks.len()),
        ) {
            return Ok(Discovery::default());
        }
        exact_domains.push(LocalDomain {
            old_span: old.group.span(anchor.old_start, anchor.old_end),
            new_span: new.group.span(anchor.new_start, anchor.new_end),
            source_bounded: false,
        });
    }
    let Some(chains) = ordered_chains(pair_anchors, remaining_work, max_ranges) else {
        return Ok(Discovery::default());
    };
    let mut domains = Vec::new();
    let mut domain_anchors = Vec::new();
    for mut anchors in chains {
        let pair = (anchors[0].old_view, anchors[0].new_view);
        let source_references = old_views[pair.0]
            .group
            .blocks
            .len()
            .saturating_add(new_views[pair.1].group.blocks.len());
        if !charge(remaining_work, source_references) {
            return Ok(Discovery::default());
        }
        let Some(domain) = close_domain(&old_views, &new_views, &mut anchors, remaining_work)
        else {
            continue;
        };
        let separated =
            !compatible_roles(sides, [&domain.old_span, &domain.new_span], remaining_work)?
                || super::span_has_source_issues(sides[0], &domain.old_span, remaining_work)?
                || super::span_has_source_issues(sides[1], &domain.new_span, remaining_work)?;
        let parts = if separated {
            let Some(parts) = split_at_barriers(
                sides,
                [&old_views[pair.0], &new_views[pair.1]],
                &anchors,
                remaining_work,
            )?
            else {
                return Ok(Discovery::default());
            };
            parts
        } else {
            vec![domain]
        };
        for domain in parts {
            if !compatible_roles(sides, [&domain.old_span, &domain.new_span], remaining_work)?
                || super::span_has_source_issues(sides[0], &domain.old_span, remaining_work)?
                || super::span_has_source_issues(sides[1], &domain.new_span, remaining_work)?
            {
                continue;
            }
            if !charge(remaining_work, anchors.len()) {
                return Ok(Discovery::default());
            }
            domain_anchors.push(
                anchors
                    .iter()
                    .copied()
                    .filter(|anchor| {
                        domain.old_span.comparable_range.start <= anchor.old_start
                            && anchor.old_end <= domain.old_span.comparable_range.end
                            && domain.new_span.comparable_range.start <= anchor.new_start
                            && anchor.new_end <= domain.new_span.comparable_range.end
                    })
                    .collect::<Vec<_>>(),
            );
            domains.push((
                (
                    old_views[pair.0].source_order,
                    new_views[pair.1].source_order,
                    domain.old_span.comparable_range.start,
                    domain.new_span.comparable_range.start,
                ),
                domain,
            ));
        }
    }
    let Some(competing) = competing_domains(sides, &domains, remaining_work, max_ranges)? else {
        return Ok(Discovery::default());
    };
    // Competition is checked on complete chains before adjacent comparisons
    // share their already verified equal anchors. A difficult gap must not
    // invalidate a separate gap whose boundaries and localization are exact.
    let mut localized = Vec::new();
    for (index, ((key, domain), anchors)) in domains.into_iter().zip(domain_anchors).enumerate() {
        if competing.contains(&index) {
            continue;
        }
        if anchors.len() <= 1 {
            localized.push((key, domain));
            continue;
        }
        for pair in anchors.windows(2) {
            if localized.len() == max_ranges
                || !charge(
                    remaining_work,
                    domain
                        .old_span
                        .blocks
                        .len()
                        .saturating_add(domain.new_span.blocks.len()),
                )
            {
                *remaining_work = 0;
                return Ok(Discovery::default());
            }
            let old_span = old_views[pair[0].old_view]
                .group
                .span(pair[0].old_start, pair[1].old_end);
            let new_span = new_views[pair[0].new_view]
                .group
                .span(pair[0].new_start, pair[1].new_end);
            localized.push((
                (key.0, key.1, pair[0].old_start, pair[0].new_start),
                LocalDomain {
                    old_span,
                    new_span,
                    source_bounded: old_views[pair[0].old_view].source_bounded
                        && new_views[pair[0].new_view].source_bounded,
                },
            ));
        }
    }
    // The positioned pass is optional and runs after the anchor flow. An
    // exhausted budget drops only its own unproven additions; the completed
    // anchor domains and exact anchors are kept, and the caller records the
    // work limit from the shared budget.
    let _positioned_complete = positioned_equalities(
        sides,
        [&old_views, &new_views],
        remaining_work,
        &mut localized,
        max_ranges,
    )?;
    localized.sort_unstable_by_key(|(key, _)| *key);
    localized.truncate(max_ranges);
    Ok(Discovery {
        domains: localized.into_iter().map(|(_, domain)| domain).collect(),
        anchors: exact_domains,
    })
}

/// True when a member block of a view may close a positioned equality: it is
/// a complete source-bounded original block with horizontal left-to-right
/// text, one page and one exact position per token.
fn positioned_block(view: &View, block: usize) -> bool {
    view.block_candidates.get(block).copied().unwrap_or(false)
}

/// Occurrences of a positioned candidate block's token sequence across every
/// view.
struct PositionedOccurrences {
    /// Other occurrences whose complete metadata equals the candidate's key.
    same: usize,
    /// An occurrence whose metadata is incomplete and which no offset proves
    /// different; it vetoes the proof. An occurrence with a definitive
    /// mismatch at any offset is not unknown and does not compete.
    unknown: bool,
    /// First same-position occurrence, its range and whether it is itself a
    /// complete eligible block. A partial occurrence competes but can never
    /// be adopted.
    matched: Option<(usize, std::ops::Range<usize>, bool)>,
}

/// Searches every view, including trusted runs and views that cannot close a
/// domain themselves, for occurrences of the candidate block's tokens. An
/// occurrence only competes when its page and every token position are
/// available and equal; a different complete position is a different physical
/// line, and an occurrence without metadata vetoes the proof instead of being
/// ignored.
fn positioned_occurrences(
    views: &[View],
    needle_view: &View,
    needle_range: &std::ops::Range<usize>,
    self_view: usize,
    remaining: &mut usize,
) -> Result<Option<PositionedOccurrences>> {
    let needle = &needle_view.group.tokens[needle_range.clone()];
    let mut result = PositionedOccurrences {
        same: 0,
        unknown: false,
        matched: None,
    };
    for (view_index, view) in views.iter().enumerate() {
        let tokens = &view.group.tokens;
        if needle.len() > tokens.len() {
            continue;
        }
        if !charge(remaining, needle.len()) {
            return Ok(None);
        }
        for start in 0..=tokens.len() - needle.len() {
            // A cheap first-token prefilter keeps the optional positioned
            // search from consuming the shared budget on non-matching starts.
            if !charge(remaining, 1) {
                return Ok(None);
            }
            if tokens[start] != needle[0] {
                continue;
            }
            if !charge(remaining, needle.len().saturating_add(1)) {
                return Ok(None);
            }
            if &tokens[start..start + needle.len()] != needle {
                continue;
            }
            if view_index == self_view && start == needle_range.start {
                continue;
            }
            // One offset with complete metadata that differs already proves
            // this occurrence cannot carry the candidate's key, so later
            // missing metadata cannot make it unknown. Missing metadata alone
            // stays unverifiable and vetoes the proof; it is never treated as
            // equal. The scan continues past a missing offset so a later
            // definitive mismatch is still found.
            let mut same = true;
            let mut complete = true;
            let mut different = false;
            for offset in 0..needle.len() {
                if let (Some(position), Some(page), Some(needle_position), Some(needle_page)) = (
                    view.token_positions[start + offset],
                    view.token_pages[start + offset],
                    needle_view.token_positions[needle_range.start + offset],
                    needle_view.token_pages[needle_range.start + offset],
                ) {
                    if position != needle_position || page != needle_page {
                        same = false;
                        different = true;
                        break;
                    }
                } else {
                    complete = false;
                }
            }
            if different {
                continue;
            }
            if !complete {
                result.unknown = true;
            } else if same {
                result.same = result.same.saturating_add(1).min(2);
                if result.matched.is_none() {
                    let end = start + needle.len();
                    let whole = view
                        .block_ranges
                        .iter()
                        .position(|range| range.start == start && range.end == end)
                        .is_some_and(|block| positioned_block(view, block));
                    result.matched = Some((view_index, start..end, whole));
                }
            }
        }
    }
    Ok(Some(result))
}

/// Projects a span to source intervals with a conservative work check.
fn project_span(
    side: &Side<'_>,
    span: &TextSpan,
    remaining: &mut usize,
) -> Result<Option<Vec<SourceInterval>>> {
    if !charge(remaining, span.blocks.len().saturating_add(1)) {
        return Ok(None);
    }
    Ok(Some(super::project(side, span)?))
}

/// Source intervals of two ranges overlap on the same block. Different block
/// vector shapes that point at the same source range are still compared.
fn projected_overlap(old: &[SourceInterval], new: &[SourceInterval]) -> bool {
    old.iter().any(|old| {
        new.iter().any(|new| {
            old.block_index == new.block_index && old.start < new.end && new.start < old.end
        })
    })
}

/// One completed domain with the view and group range of its first block.
struct PositionedDomain {
    old_view: usize,
    new_view: usize,
    old_range: std::ops::Range<usize>,
    new_range: std::ops::Range<usize>,
    old_projection: Vec<SourceInterval>,
    new_projection: Vec<SourceInterval>,
}

/// Closes complete source-bounded original blocks whose canonical tokens,
/// page and exact per-token source positions form a one-to-one key.
///
/// Adoption is per whole original block: the matching occurrence must cover
/// exactly one complete eligible block on the other side, and the full token,
/// page and position vectors are re-verified immediately before the domain is
/// added, so a substring is never expanded. A block is adopted only when it is
/// a whole single-block view on **both** sides, or when the same equal domain
/// contains the whole old and new blocks at the same offset. An equal domain
/// here is an already closed local domain from the anchor flow whose old and
/// new group tokens are exactly equal over its own ranges; the assessment
/// stage later confirms it as established, and this pass only relies on the
/// token equality of the mapping, not on that later confirmation.
///
/// Uniqueness is checked across every view with per-token metadata; an
/// occurrence without comparable metadata vetoes the proof. An addition that
/// overlaps an existing domain is held unless it is the same block pair or the
/// equal mapping that contains the block at the same offset on both sides, so
/// different source shapes cannot evade the conflict and an existing proved
/// boundary is never broken. A trusted run keeps its veto: only a complete
/// original block with its own source evidence participates, never an
/// arbitrary fragment of a longer view.
fn positioned_equalities(
    sides: [&Side<'_>; 2],
    views: [&[View]; 2],
    remaining: &mut usize,
    domains: &mut Vec<SortedDomain>,
    limit: usize,
) -> Result<bool> {
    // Map every source block to the view that contains it.
    let mut view_of_block = [
        vec![usize::MAX; sides[0].blocks.len()],
        vec![usize::MAX; sides[1].blocks.len()],
    ];
    for (side, side_views) in views.iter().enumerate() {
        for (view_index, view) in side_views.iter().enumerate() {
            for &block_index in &view.block_indices {
                if let Some(slot) = view_of_block[side].get_mut(block_index) {
                    *slot = view_index;
                }
            }
        }
    }
    let mut existing = Vec::new();
    for (_, domain) in domains.iter() {
        let old_view = domain
            .old_span
            .blocks
            .first()
            .and_then(|block| sides[0].index.get(block))
            .and_then(|block| view_of_block[0].get(*block))
            .copied()
            .unwrap_or(usize::MAX);
        let new_view = domain
            .new_span
            .blocks
            .first()
            .and_then(|block| sides[1].index.get(block))
            .and_then(|block| view_of_block[1].get(*block))
            .copied()
            .unwrap_or(usize::MAX);
        let Some(old_projection) = project_span(sides[0], &domain.old_span, remaining)? else {
            return Ok(false);
        };
        let Some(new_projection) = project_span(sides[1], &domain.new_span, remaining)? else {
            return Ok(false);
        };
        existing.push(PositionedDomain {
            old_view,
            new_view,
            old_range: domain.old_span.comparable_range.start..domain.old_span.comparable_range.end,
            new_range: domain.new_span.comparable_range.start..domain.new_span.comparable_range.end,
            old_projection,
            new_projection,
        });
    }
    // Complete blocks inside a proven equal domain are its unpublished
    // residue: the domain's own mapping is equal at the same offsets, so a
    // member block may close its own positioned equality. Blocks outside any
    // equal domain still need to be whole single-block views.
    let mut equal_domains = Vec::new();
    for (_, domain) in domains.iter() {
        let old_view = domain
            .old_span
            .blocks
            .first()
            .and_then(|block| sides[0].index.get(block))
            .and_then(|block| view_of_block[0].get(*block))
            .copied()
            .unwrap_or(usize::MAX);
        let new_view = domain
            .new_span
            .blocks
            .first()
            .and_then(|block| sides[1].index.get(block))
            .and_then(|block| view_of_block[1].get(*block))
            .copied()
            .unwrap_or(usize::MAX);
        if old_view == usize::MAX || new_view == usize::MAX {
            continue;
        }
        let old_range =
            domain.old_span.comparable_range.start..domain.old_span.comparable_range.end;
        let new_range =
            domain.new_span.comparable_range.start..domain.new_span.comparable_range.end;
        if old_range.is_empty() || new_range.is_empty() {
            continue;
        }
        if !charge(
            remaining,
            old_range
                .len()
                .saturating_add(new_range.len())
                .saturating_add(1),
        ) {
            return Ok(false);
        }
        if views[0][old_view].group.tokens[old_range.clone()]
            == views[1][new_view].group.tokens[new_range.clone()]
        {
            equal_domains.push(PositionedDomain {
                old_view,
                new_view,
                old_range,
                new_range,
                old_projection: Vec::new(),
                new_projection: Vec::new(),
            });
        }
    }
    let mut additions: Vec<(PositionedDomain, LocalDomain)> = Vec::new();
    'views: for (old_view_index, old_view) in views[0].iter().enumerate() {
        for block in 0..old_view.block_ranges.len() {
            if !positioned_block(old_view, block) {
                continue;
            }
            let old_range = old_view.block_ranges[block].clone();
            let old_whole = old_view.block_indices.len() == 1
                && old_range.start == 0
                && old_range.end == old_view.group.tokens.len();
            if !old_whole && equal_domains.is_empty() {
                continue;
            }
            if old_view.token_positions[old_range.clone()]
                .iter()
                .any(Option::is_none)
                || old_view.token_pages[old_range.clone()]
                    .iter()
                    .any(Option::is_none)
            {
                continue;
            }
            if domains.len().saturating_add(additions.len()) >= limit {
                break 'views;
            }
            if !charge(remaining, old_range.len().saturating_add(1)) {
                return Ok(false);
            }
            // The old side must not contain another occurrence at the same
            // position, and no occurrence with unverifiable metadata.
            let Some(old_occurrences) =
                positioned_occurrences(views[0], old_view, &old_range, old_view_index, remaining)?
            else {
                return Ok(false);
            };
            if old_occurrences.same != 0 || old_occurrences.unknown {
                continue;
            }
            // The new side must contain exactly one same-position occurrence,
            // it must cover exactly one complete eligible block and that block
            // must itself be adoptable. A substring occurrence competes but is
            // never adopted.
            let Some(new_occurrences) =
                positioned_occurrences(views[1], old_view, &old_range, usize::MAX, remaining)?
            else {
                return Ok(false);
            };
            if new_occurrences.same != 1 || new_occurrences.unknown {
                continue;
            }
            let Some((new_view_index, new_range, true)) = new_occurrences.matched else {
                continue;
            };
            let new_view = &views[1][new_view_index];
            // Re-verify the whole-block equality invariant immediately before
            // the domain is added: full token sequence, page and every
            // position must match, and no substring may be expanded.
            let whole_block_equal = old_view.group.tokens[old_range.clone()]
                == new_view.group.tokens[new_range.clone()]
                && (0..old_range.len()).all(|offset| {
                    old_view.token_positions[old_range.start + offset]
                        == new_view.token_positions[new_range.start + offset]
                        && old_view.token_pages[old_range.start + offset]
                            == new_view.token_pages[new_range.start + offset]
                });
            if !whole_block_equal {
                continue;
            }
            let new_whole = new_view.block_indices.len() == 1
                && new_range.start == 0
                && new_range.end == new_view.group.tokens.len();
            // A residue block must be contained in the same equal domain on
            // both sides at the same offset; containment on one side or a
            // shifted correspondence is not the same mapping. The scan is
            // charged per examined domain and skipped entirely when both sides
            // are already whole views.
            let residue = if old_whole && new_whole {
                false
            } else {
                if !charge(remaining, equal_domains.len().saturating_add(1)) {
                    return Ok(false);
                }
                equal_domains.iter().any(|domain| {
                    domain.old_view == old_view_index
                        && domain.new_view == new_view_index
                        && domain.old_range.start <= old_range.start
                        && old_range.end <= domain.old_range.end
                        && domain.new_range.start <= new_range.start
                        && new_range.end <= domain.new_range.end
                        && (old_range.start - domain.old_range.start)
                            == (new_range.start - domain.new_range.start)
                })
            };
            // Adoption requires a whole view on both sides or the same equal
            // mapping; a member of a longer view on one side alone is never
            // expanded into a whole-view equality.
            if !((old_whole && new_whole) || residue) {
                continue;
            }
            let old_span = old_view.group.span(old_range.start, old_range.end);
            let new_span = new_view.group.span(new_range.start, new_range.end);
            let Some(old_projection) = project_span(sides[0], &old_span, remaining)? else {
                return Ok(false);
            };
            let Some(new_projection) = project_span(sides[1], &new_span, remaining)? else {
                return Ok(false);
            };
            // Keep existing domains; hold any addition whose source ranges
            // overlap one of them or a pending addition unless the overlap is
            // the same block pair or a proven equal mapping containing it at
            // the same offset on both sides.
            let mut conflict = false;
            for other in existing
                .iter()
                .chain(additions.iter().map(|(existing, _)| existing))
            {
                // Charge the interval combination the overlap check performs.
                if !charge(
                    remaining,
                    other
                        .old_projection
                        .len()
                        .saturating_mul(old_projection.len())
                        .saturating_add(
                            other
                                .new_projection
                                .len()
                                .saturating_mul(new_projection.len()),
                        )
                        .saturating_add(1),
                ) {
                    return Ok(false);
                }
                if !(projected_overlap(&old_projection, &other.old_projection)
                    || projected_overlap(&new_projection, &other.new_projection))
                {
                    continue;
                }
                let same_pair = other.old_view == old_view_index
                    && other.old_range == old_range
                    && other.new_view == new_view_index
                    && other.new_range == new_range;
                let redundant = !same_pair
                    && other.old_view == old_view_index
                    && other.new_view == new_view_index
                    && other.old_range.start <= old_range.start
                    && old_range.end <= other.old_range.end
                    && other.new_range.start <= new_range.start
                    && new_range.end <= other.new_range.end
                    && (old_range.start - other.old_range.start)
                        == (new_range.start - other.new_range.start)
                    && {
                        if !charge(
                            remaining,
                            other
                                .old_range
                                .len()
                                .saturating_add(other.new_range.len())
                                .saturating_add(1),
                        ) {
                            return Ok(false);
                        }
                        old_view.group.tokens[other.old_range.clone()]
                            == new_view.group.tokens[other.new_range.clone()]
                    };
                if same_pair {
                    // The exact block pair is already present.
                    conflict = true;
                    break;
                }
                if redundant {
                    // A proven equal mapping contains this block at the same
                    // offset on both sides: the overlap is the same
                    // correspondence, so the addition is compatible.
                    continue;
                }
                conflict = true;
                break;
            }
            if conflict {
                continue;
            }
            if !compatible_roles(sides, [&old_span, &new_span], remaining)?
                || super::span_has_source_issues(sides[0], &old_span, remaining)?
                || super::span_has_source_issues(sides[1], &new_span, remaining)?
            {
                continue;
            }
            additions.push((
                PositionedDomain {
                    old_view: old_view_index,
                    new_view: new_view_index,
                    old_range,
                    new_range,
                    old_projection,
                    new_projection,
                },
                LocalDomain {
                    old_span,
                    new_span,
                    source_bounded: true,
                },
            ));
        }
    }
    for (_, domain) in additions {
        let old_index = domain
            .old_span
            .blocks
            .first()
            .and_then(|block| sides[0].index.get(block))
            .copied()
            .unwrap_or(usize::MAX);
        let new_index = domain
            .new_span
            .blocks
            .first()
            .and_then(|block| sides[1].index.get(block))
            .copied()
            .unwrap_or(usize::MAX);
        domains.push((
            (
                old_index,
                new_index,
                domain.old_span.comparable_range.start,
                domain.new_span.comparable_range.start,
            ),
            domain,
        ));
    }
    Ok(true)
}

/// An independently established whole single-block correspondence.
///
/// The assessment builds this list from relations that are established with a
/// complete search, carry no reasons, and whose source intervals are already
/// accepted by the comparison ownership. A rigid translation may only lean on
/// evidence of this kind, never on the mere presence of a view domain.
#[derive(Clone, Copy)]
pub(super) struct EstablishedBlock {
    pub(super) old_block: BlockId,
    pub(super) new_block: BlockId,
}

/// The one raw baseline translation shared by every token of two ranges.
///
/// Returns `None` unless both ranges have the same non-empty length, every
/// token has complete metadata, the text directions are bit-identical, and
/// every token's raw baseline difference is the same finite vector. Exact bit
/// equality is required; no tolerance is applied. A zero vector is a valid
/// stationary correspondence and is returned as such.
fn finite_translation(
    old_view: &View,
    old_range: &std::ops::Range<usize>,
    new_view: &View,
    new_range: &std::ops::Range<usize>,
) -> Option<crate::model::Vec2> {
    if old_range.is_empty() || old_range.len() != new_range.len() {
        return None;
    }
    let mut translation: Option<(u64, u64)> = None;
    for offset in 0..old_range.len() {
        let old = old_view.token_positions[old_range.start + offset]?;
        let new = new_view.token_positions[new_range.start + offset]?;
        if !same_direction(old, new) {
            return None;
        }
        let old_baseline = old.baseline();
        let new_baseline = new.baseline();
        let dx = new_baseline.x - old_baseline.x;
        let dy = new_baseline.y - old_baseline.y;
        if !dx.is_finite() || !dy.is_finite() {
            // Finite baselines may still overflow their difference; an
            // overflowing delta is never a valid translation.
            return None;
        }
        let difference = (dx.to_bits(), dy.to_bits());
        match translation {
            None => translation = Some(difference),
            Some(previous) if previous != difference => return None,
            Some(_) => {}
        }
    }
    let (dx, dy) = translation?;
    Some(crate::model::Vec2 {
        x: f64::from_bits(dx),
        y: f64::from_bits(dy),
    })
}

/// The one raw finite non-zero baseline translation shared by every token of
/// two ranges. A stationary correspondence is not a move and returns `None`.
fn exact_translation(
    old_view: &View,
    old_range: &std::ops::Range<usize>,
    new_view: &View,
    new_range: &std::ops::Range<usize>,
) -> Option<crate::model::Vec2> {
    let translation = finite_translation(old_view, old_range, new_view, new_range)?;
    if translation.x == 0.0 && translation.y == 0.0 {
        return None;
    }
    Some(translation)
}

/// Bit-identical raw translation vectors.
fn same_translation(left: crate::model::Vec2, right: crate::model::Vec2) -> bool {
    left.x.to_bits() == right.x.to_bits() && left.y.to_bits() == right.y.to_bits()
}

/// Bit-identical text directions of two source positions.
fn same_direction(
    left: crate::normalize::PositionSignature,
    right: crate::normalize::PositionSignature,
) -> bool {
    let left = left.direction();
    let right = right.direction();
    left.x.to_bits() == right.x.to_bits() && left.y.to_bits() == right.y.to_bits()
}

/// The single page of a complete range, or `None` when any token lacks one.
fn single_page(view: &View, range: &std::ops::Range<usize>) -> Option<u32> {
    let mut page = None;
    for offset in range.clone() {
        let next = view.token_pages[offset]?;
        match page {
            None => page = Some(next),
            Some(previous) if previous != next => return None,
            Some(_) => {}
        }
    }
    page
}

/// Occurrences of a positioned candidate block's token sequence under one
/// exact translation key.
///
/// Mirrors [`positioned_occurrences`] with the raw baseline difference as the
/// key instead of full position equality: an occurrence only competes when its
/// page, direction and every token's raw baseline difference equal the
/// translation. Exact bit equality is required; no tolerance is applied.
fn translated_occurrences(
    views: &[View],
    needle_view: &View,
    needle_range: &std::ops::Range<usize>,
    self_view: usize,
    translation: crate::model::Vec2,
    remaining: &mut usize,
) -> Result<Option<PositionedOccurrences>> {
    let needle = &needle_view.group.tokens[needle_range.clone()];
    let mut result = PositionedOccurrences {
        same: 0,
        unknown: false,
        matched: None,
    };
    for (view_index, view) in views.iter().enumerate() {
        let tokens = &view.group.tokens;
        if needle.len() > tokens.len() {
            continue;
        }
        if !charge(remaining, needle.len()) {
            return Ok(None);
        }
        for start in 0..=tokens.len() - needle.len() {
            if !charge(remaining, 1) {
                return Ok(None);
            }
            if tokens[start] != needle[0] {
                continue;
            }
            if !charge(remaining, needle.len().saturating_add(1)) {
                return Ok(None);
            }
            if &tokens[start..start + needle.len()] != needle {
                continue;
            }
            if view_index == self_view && start == needle_range.start {
                continue;
            }
            let mut same = true;
            let mut complete = true;
            for offset in 0..needle.len() {
                if let (Some(position), Some(page), Some(needle_position), Some(needle_page)) = (
                    view.token_positions[start + offset],
                    view.token_pages[start + offset],
                    needle_view.token_positions[needle_range.start + offset],
                    needle_view.token_pages[needle_range.start + offset],
                ) {
                    if page != needle_page || !same_direction(position, needle_position) {
                        same = false;
                        continue;
                    }
                    let position = position.baseline();
                    let needle_position = needle_position.baseline();
                    let dx = position.x - needle_position.x;
                    let dy = position.y - needle_position.y;
                    if !dx.is_finite()
                        || !dy.is_finite()
                        || dx.to_bits() != translation.x.to_bits()
                        || dy.to_bits() != translation.y.to_bits()
                    {
                        same = false;
                    }
                } else {
                    complete = false;
                    break;
                }
            }
            if !complete {
                result.unknown = true;
            } else if same {
                result.same = result.same.saturating_add(1).min(2);
                if result.matched.is_none() {
                    let end = start + needle.len();
                    let whole = view
                        .block_ranges
                        .iter()
                        .position(|range| range.start == start && range.end == end)
                        .is_some_and(|block| positioned_block(view, block));
                    result.matched = Some((view_index, start..end, whole));
                }
            }
        }
    }
    Ok(Some(result))
}

/// One established neighbour correspondence resolved to its views and ranges.
struct EstablishedNeighbour {
    old_index: usize,
    new_index: usize,
    /// Member ordinal of the reference inside its own old view.
    old_member: usize,
    /// Member ordinal of the reference inside its own new view.
    new_member: usize,
    old_view: usize,
    new_view: usize,
    old_range: std::ops::Range<usize>,
    new_range: std::ops::Range<usize>,
    /// Uniform finite translation when the whole block moved as one. `None`
    /// means the transform is not uniform (or a position is missing); such an
    /// entry can never support a move but stays an order reference.
    translation: Option<crate::model::Vec2>,
    /// Pages covered by the old block's lines, taken from the source side
    /// independently of the view metadata.
    old_pages: Vec<u32>,
    /// Pages covered by the new block's lines, taken from the source side
    /// independently of the view metadata.
    new_pages: Vec<u32>,
}

/// How a reference correspondence relates to the candidate's page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferencePage {
    /// Both sides are single-page and equal to the candidate's page, so raw
    /// coordinates are comparable.
    Same,
    /// Both sides provably do not cover the candidate's page; raw coordinates
    /// across pages are not order evidence and the reference is excluded.
    Other,
    /// The page relation cannot be proven either way; the candidate is held
    /// rather than comparing unrelated coordinates.
    Unknown,
}

/// Classifies a reference correspondence against the candidate's page using
/// the source-side page lists only, so missing view metadata never hides a
/// same-page reference.
fn reference_page(old_pages: &[u32], new_pages: &[u32], candidate_page: u32) -> ReferencePage {
    if old_pages.len() == 1
        && old_pages[0] == candidate_page
        && new_pages.len() == 1
        && new_pages[0] == candidate_page
    {
        return ReferencePage::Same;
    }
    if !old_pages.is_empty()
        && !new_pages.is_empty()
        && !old_pages.contains(&candidate_page)
        && !new_pages.contains(&candidate_page)
    {
        return ReferencePage::Other;
    }
    ReferencePage::Unknown
}

/// Raw baseline bounding box of a view range, or `None` when any token lacks
/// a position.
fn baseline_bounds(
    view: &View,
    range: &std::ops::Range<usize>,
    remaining: &mut usize,
) -> Option<(f64, f64, f64, f64)> {
    if !charge(remaining, range.len().saturating_add(1)) {
        return None;
    }
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for offset in range.clone() {
        let position = view.token_positions[offset]?.baseline();
        bounds = Some(match bounds {
            None => (position.x, position.y, position.x, position.y),
            Some((min_x, min_y, max_x, max_y)) => (
                min_x.min(position.x),
                min_y.min(position.y),
                max_x.max(position.x),
                max_y.max(position.y),
            ),
        });
    }
    bounds
}

/// Whether the candidate and the neighbour keep the same raw baseline order
/// and intersection relation on both sides. The check compares exact
/// coordinates only and covers the whole block, not just its first token.
fn same_relative_geometry(
    candidate_old: (f64, f64, f64, f64),
    neighbour_old: (f64, f64, f64, f64),
    candidate_new: (f64, f64, f64, f64),
    neighbour_new: (f64, f64, f64, f64),
) -> bool {
    let intersects = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
        a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
    };
    let above = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| a.3 <= b.1;
    let below = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| b.3 <= a.1;
    let left = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| a.2 <= b.0;
    let right = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| b.2 <= a.0;
    intersects(candidate_old, neighbour_old) == intersects(candidate_new, neighbour_new)
        && above(candidate_old, neighbour_old) == above(candidate_new, neighbour_new)
        && below(candidate_old, neighbour_old) == below(candidate_new, neighbour_new)
        && left(candidate_old, neighbour_old) == left(candidate_new, neighbour_new)
        && right(candidate_old, neighbour_old) == right(candidate_new, neighbour_new)
}

/// Raw baseline bounds of an established reference block.
///
/// The view metadata is preferred; when it is missing (for example a
/// paragraph whose normalization keeps a soft line break, so the block is not
/// a complete single-line view) the block's own source position signatures
/// are used. `Some(None)` reports that no geometry is available at all and
/// the caller must hold; `None` reports an exhausted shared work budget.
fn reference_bounds(
    side: &Side<'_>,
    view: &View,
    range: &std::ops::Range<usize>,
    block_index: usize,
    remaining: &mut usize,
) -> Option<Option<(f64, f64, f64, f64)>> {
    if !charge(remaining, range.len().saturating_add(1)) {
        return None;
    }
    if range
        .clone()
        .all(|offset| view.token_positions[offset].is_some())
    {
        return baseline_bounds(view, range, remaining).map(Some);
    }
    let block = &side.blocks[block_index];
    let Some(signatures) = block.position_signatures.as_deref() else {
        return Some(None);
    };
    if signatures.len() != side.canonical[block_index].len() {
        return Some(None);
    }
    if !charge(remaining, signatures.len().saturating_add(1)) {
        return None;
    }
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for signature in signatures {
        let baseline = signature.baseline();
        bounds = Some(match bounds {
            None => (baseline.x, baseline.y, baseline.x, baseline.y),
            Some((min_x, min_y, max_x, max_y)) => (
                min_x.min(baseline.x),
                min_y.min(baseline.y),
                max_x.max(baseline.x),
                max_y.max(baseline.y),
            ),
        });
    }
    Some(bounds)
}

/// Whether every token of a reference block has a horizontal text direction,
/// using the view metadata or the block's own source signatures.
fn reference_horizontal(
    side: &Side<'_>,
    view: &View,
    range: &std::ops::Range<usize>,
    block_index: usize,
    remaining: &mut usize,
) -> Option<bool> {
    if !charge(remaining, range.len().saturating_add(1)) {
        return None;
    }
    if range
        .clone()
        .all(|offset| view.token_positions[offset].is_some())
    {
        return Some(range.clone().all(|offset| {
            view.token_positions[offset].is_some_and(|position| horizontal_direction(&position))
        }));
    }
    let Some(signatures) = side.blocks[block_index].position_signatures.as_deref() else {
        return Some(false);
    };
    if signatures.len() != side.canonical[block_index].len() {
        return Some(false);
    }
    if !charge(remaining, signatures.len().saturating_add(1)) {
        return None;
    }
    Some(signatures.iter().all(horizontal_direction))
}

/// One side of a band adjacency check: the source side, the whole-block view,
/// its token range and its block index.
#[derive(Clone, Copy)]
struct BandSide<'a> {
    side: &'a Side<'a>,
    view: &'a View,
    range: &'a std::ops::Range<usize>,
    block: usize,
    page: u32,
}

/// Whether the neighbour is the nearest source boundary of the candidate in
/// the same column band on one side.
///
/// The band is the strict positive overlap of the two baseline x-intervals;
/// the neighbour must be strictly below or above the candidate on the
/// orthogonal axis, and no other source block of that side whose baseline
/// x-interval overlaps the band may intersect the gap between them. Every
/// source block is inspected, including blocks with normalization issues, and
/// a block whose geometry is missing vetoes the proof instead of being
/// ignored. `None` reports an exhausted shared work budget.
fn band_nearest(
    candidate: BandSide<'_>,
    candidate_bounds: (f64, f64, f64, f64),
    neighbour: BandSide<'_>,
    neighbour_bounds: (f64, f64, f64, f64),
    remaining: &mut usize,
) -> Option<bool> {
    if !reference_horizontal(
        candidate.side,
        candidate.view,
        candidate.range,
        candidate.block,
        remaining,
    )? || !reference_horizontal(
        neighbour.side,
        neighbour.view,
        neighbour.range,
        neighbour.block,
        remaining,
    )? {
        return Some(false);
    }
    let band_min = candidate_bounds.0.max(neighbour_bounds.0);
    let band_max = candidate_bounds.2.min(neighbour_bounds.2);
    if band_min >= band_max {
        return Some(false);
    }
    let gap = if neighbour_bounds.3 < candidate_bounds.1 {
        (neighbour_bounds.3, candidate_bounds.1)
    } else if candidate_bounds.3 < neighbour_bounds.1 {
        (candidate_bounds.3, neighbour_bounds.1)
    } else {
        return Some(false);
    };
    if !charge(remaining, candidate.side.blocks.len()) {
        return None;
    }
    for (index, block) in candidate.side.blocks.iter().enumerate() {
        if index == candidate.block || index == neighbour.block {
            continue;
        }
        // Only a block provably on the candidate's page is compared by raw
        // coordinates; a block provably on another page is not order
        // evidence, and an empty or page-ambiguous block holds the proof.
        if block.pages.is_empty()
            || (block.pages.len() > 1 && block.pages.contains(&candidate.page))
        {
            return Some(false);
        }
        if !block.pages.contains(&candidate.page) {
            continue;
        }
        // A block is empty only when it has no comparable tokens, no source
        // map entries and no raw or canonical text. An opaque or unmapped
        // token, or a retained source origin, keeps it an obstacle even when
        // its display text is empty.
        let no_tokens = candidate.side.canonical[index].is_empty();
        let no_source = block.raw.source_map.is_empty() && block.canonical.source_map.is_empty();
        if no_tokens && no_source && block.raw.text.is_empty() && block.canonical.text.is_empty() {
            continue;
        }
        let Some(signatures) = block.position_signatures.as_deref() else {
            // The obstacle relation cannot be proven without geometry.
            return Some(false);
        };
        if signatures.len() != candidate.side.canonical[index].len() {
            return Some(false);
        }
        if !charge(remaining, signatures.len().saturating_add(1)) {
            return None;
        }
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for signature in signatures {
            let baseline = signature.baseline();
            bounds = Some(match bounds {
                None => (baseline.x, baseline.y, baseline.x, baseline.y),
                Some((min_x, min_y, max_x, max_y)) => (
                    min_x.min(baseline.x),
                    min_y.min(baseline.y),
                    max_x.max(baseline.x),
                    max_y.max(baseline.y),
                ),
            });
        }
        let Some(obstacle) = bounds else {
            // A non-empty source block without geometry cannot be cleared.
            return Some(false);
        };
        // Closed-interval semantics: a zero-width (point) obstacle inside the
        // band still blocks the gap.
        if obstacle.0.max(band_min) > obstacle.2.min(band_max) {
            continue;
        }
        if obstacle.1.max(gap.0) <= obstacle.3.min(gap.1) {
            return Some(false);
        }
    }
    Some(true)
}

/// Which raw displacement key one whole-block equality is proven from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TranslationMode {
    /// One whole-view candidate moved by one non-zero uniform translation.
    NonzeroSingleton,
    /// One whole original block member sits still and is supported by an
    /// independently established stationary neighbour.
    StationaryMember,
}

/// Whether one member range covers the whole original block on this side.
fn whole_original_member(side: &Side<'_>, view: &View, member: usize) -> bool {
    let Some(&index) = view.block_indices.get(member) else {
        return false;
    };
    let Some(range) = view.block_ranges.get(member) else {
        return false;
    };
    side.canonical
        .get(index)
        .is_some_and(|tokens| tokens.len() == range.len())
}

/// Whether the candidate's own pair carries the mode's required raw key.
fn mode_key_matches(
    mode: TranslationMode,
    old_view: &View,
    old_range: &std::ops::Range<usize>,
    new_view: &View,
    new_range: &std::ops::Range<usize>,
    key: crate::model::Vec2,
) -> bool {
    match mode {
        TranslationMode::NonzeroSingleton => {
            exact_translation(old_view, old_range, new_view, new_range) == Some(key)
        }
        TranslationMode::StationaryMember => {
            finite_translation(old_view, old_range, new_view, new_range) == Some(key)
        }
    }
}

/// Whether one adjacent established translation may support the mode's key.
fn mode_supports(mode: TranslationMode, translation: crate::model::Vec2) -> bool {
    let stationary = translation.x == 0.0 && translation.y == 0.0;
    match mode {
        TranslationMode::NonzeroSingleton => !stationary,
        TranslationMode::StationaryMember => stationary,
    }
}

/// Discovers whole single-block lines whose content is equal on both sides and
/// whose positions differ by one exact translation carried by an independently
/// established neighbour block.
///
/// The candidate must be a complete source-bounded untrusted singleton on both
/// sides with horizontal left-to-right text, one page and one exact position
/// per token. Every token's raw baseline difference must be the same finite
/// non-zero vector (bit-exact) and the directions must be bit-identical. One
/// and the same established neighbour must support the move on both sides: it
/// must be adjacent in source order or the proven nearest source boundary in
/// the same column band, on the same page, with disjoint sources and the same
/// raw translation, and the candidate's whole-block order and intersection
/// relation to that neighbour must be unchanged between the two sides. The new
/// side must contain exactly one occurrence under the translation key and the
/// old side no same-position duplicate; an occurrence without comparable
/// metadata vetoes.
///
/// Stationary (zero-translation) anchors can never support a move, because the
/// candidate's own translation is non-zero and bit-exact, but they always stay
/// in the reference set: their whole-block order and intersection relation to
/// the candidate and the supporting anchor is checked like every other
/// established correspondence. A stationary anchor adjacent on one side only
/// is therefore harmless and handled symmetrically: it contributes no key on
/// either side, and the reference check still sees it on both sides.
///
/// A reference whose view metadata lacks positions is still checked from its
/// own source position signatures when they are available; only a reference
/// with no geometry at all holds the candidate, and it is never dropped
/// silently. The move itself is the evidence: the caller records it as an
/// assumption, and nothing is adopted on proximity or tolerance. `None` is
/// never returned; an exhausted budget drops only this pass's additions.
pub(super) fn discover_translations(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    established: &[EstablishedBlock],
    remaining_work: &mut usize,
    max_ranges: usize,
) -> Result<Vec<LocalDomain>> {
    discover_translations_mode(
        sides,
        recovery,
        established,
        remaining_work,
        max_ranges,
        TranslationMode::NonzeroSingleton,
        None,
    )
}

#[cfg(test)]
pub(super) fn discover_stationary_members(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    established: &[EstablishedBlock],
    remaining_work: &mut usize,
    max_ranges: usize,
) -> Result<Vec<LocalDomain>> {
    discover_translations_mode(
        sides,
        recovery,
        established,
        remaining_work,
        max_ranges,
        TranslationMode::StationaryMember,
        None,
    )
}

/// Stationary-member discovery with a reject-only candidate mask.
///
/// `candidate_mask[side][block_index] == false` removes that whole original
/// block from candidacy on that side. The mask never shrinks the view
/// population or the reference set: masked blocks still support anchors, stay
/// in every reference check and still compete as occurrences.
pub(super) fn discover_stationary_members_masked(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    established: &[EstablishedBlock],
    remaining_work: &mut usize,
    max_ranges: usize,
    candidate_mask: &[Vec<bool>; 2],
) -> Result<Vec<LocalDomain>> {
    discover_translations_mode(
        sides,
        recovery,
        established,
        remaining_work,
        max_ranges,
        TranslationMode::StationaryMember,
        Some(candidate_mask),
    )
}

fn discover_translations_mode(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    established: &[EstablishedBlock],
    remaining_work: &mut usize,
    max_ranges: usize,
    mode: TranslationMode,
    candidate_mask: Option<&[Vec<bool>; 2]>,
) -> Result<Vec<LocalDomain>> {
    if max_ranges == 0
        || *remaining_work == 0
        || established.is_empty()
        || sides.iter().any(|side| side.blocks.is_empty())
    {
        return Ok(Vec::new());
    }
    if recovery.old_trusted_run_intervals.len() != sides[0].blocks.len()
        || recovery.new_trusted_run_intervals.len() != sides[1].blocks.len()
    {
        return Err(super::invalid(
            "trusted run interval metadata must match normalized blocks",
        ));
    }
    let old_descriptors = recovery
        .old_trusted_run_evidence
        .map(|evidence| evidence.descriptors);
    let new_descriptors = recovery
        .new_trusted_run_evidence
        .map(|evidence| evidence.descriptors);
    let Some(old_views) = build_views(
        sides[0],
        recovery.old_trusted_run_intervals,
        old_descriptors,
        remaining_work,
    )?
    else {
        return Ok(Vec::new());
    };
    let Some(new_views) = build_views(
        sides[1],
        recovery.new_trusted_run_intervals,
        new_descriptors,
        remaining_work,
    )?
    else {
        return Ok(Vec::new());
    };
    if old_views.is_empty() || new_views.is_empty() {
        return Ok(Vec::new());
    }
    if !charge(
        remaining_work,
        sides[0].blocks.len().saturating_add(sides[1].blocks.len()),
    ) {
        return Ok(Vec::new());
    }
    let mut view_of_block = [
        vec![usize::MAX; sides[0].blocks.len()],
        vec![usize::MAX; sides[1].blocks.len()],
    ];
    for (side, side_views) in [&old_views, &new_views].into_iter().enumerate() {
        for (view_index, view) in side_views.iter().enumerate() {
            for &block_index in &view.block_indices {
                if let Some(slot) = view_of_block[side].get_mut(block_index) {
                    *slot = view_index;
                }
            }
        }
    }
    // Resolve every established neighbour to its views and raw translation.
    let mut neighbours = Vec::new();
    for neighbour in established {
        if !charge(remaining_work, 1) {
            return Ok(Vec::new());
        }
        let Some(&old_index) = sides[0].index.get(&neighbour.old_block) else {
            continue;
        };
        let Some(&new_index) = sides[1].index.get(&neighbour.new_block) else {
            continue;
        };
        let old_view_index = view_of_block[0][old_index];
        let new_view_index = view_of_block[1][new_index];
        if old_view_index == usize::MAX || new_view_index == usize::MAX {
            continue;
        }
        let old_view = &old_views[old_view_index];
        let new_view = &new_views[new_view_index];
        if !charge(
            remaining_work,
            old_view
                .block_indices
                .len()
                .saturating_add(new_view.block_indices.len()),
        ) {
            return Ok(Vec::new());
        }
        let Some(old_block) = old_view
            .block_indices
            .iter()
            .position(|&block| block == old_index)
        else {
            continue;
        };
        let Some(new_block) = new_view
            .block_indices
            .iter()
            .position(|&block| block == new_index)
        else {
            continue;
        };
        let old_range = old_view.block_ranges[old_block].clone();
        let new_range = new_view.block_ranges[new_block].clone();
        if !charge(
            remaining_work,
            old_range.len().saturating_add(new_range.len()),
        ) {
            return Ok(Vec::new());
        }
        // The uniform translation and the order geometry are separate
        // evidence: a non-uniform transform stays a reference whenever its
        // whole-block geometry is complete, and only a uniform entry may
        // support a move.
        let translation = finite_translation(old_view, &old_range, new_view, &new_range);
        neighbours.push(EstablishedNeighbour {
            old_index,
            new_index,
            old_member: old_block,
            new_member: new_block,
            old_view: old_view_index,
            new_view: new_view_index,
            old_range,
            new_range,
            translation,
            old_pages: sides[0].blocks[old_index].pages.clone(),
            new_pages: sides[1].blocks[new_index].pages.clone(),
        });
    }
    if neighbours.is_empty() {
        return Ok(Vec::new());
    }
    let mut domains = Vec::new();
    'views: for (old_view_index, old_view) in old_views.iter().enumerate() {
        if mode == TranslationMode::NonzeroSingleton
            && (!matches!(old_view.kind, ViewKind::Untrusted(_)) || !old_view.source_bounded)
        {
            continue;
        }
        for block in 0..old_view.block_ranges.len() {
            if !positioned_block(old_view, block) {
                continue;
            }
            let old_range = old_view.block_ranges[block].clone();
            match mode {
                TranslationMode::NonzeroSingleton => {
                    if old_view.block_indices.len() != 1
                        || old_range.start != 0
                        || old_range.end != old_view.group.tokens.len()
                    {
                        continue;
                    }
                }
                TranslationMode::StationaryMember => {
                    if !whole_original_member(sides[0], old_view, block) {
                        continue;
                    }
                }
            }
            if !charge(remaining_work, old_range.len().saturating_add(1)) {
                return Ok(Vec::new());
            }
            if old_view.token_positions[old_range.clone()]
                .iter()
                .any(Option::is_none)
                || old_view.token_pages[old_range.clone()]
                    .iter()
                    .any(Option::is_none)
            {
                continue;
            }
            if domains.len() >= max_ranges {
                break 'views;
            }
            let candidate_index = old_view.block_indices[block];
            if mode == TranslationMode::StationaryMember
                && candidate_mask
                    .is_some_and(|mask| !mask[0].get(candidate_index).copied().unwrap_or(false))
            {
                continue;
            }
            let Some(candidate_page) = single_page(old_view, &old_range) else {
                continue;
            };
            let Some(candidate_projection) = project_span(
                sides[0],
                &old_view.group.span(old_range.start, old_range.end),
                remaining_work,
            )?
            else {
                return Ok(Vec::new());
            };
            let Some(candidate_old_bounds) = baseline_bounds(old_view, &old_range, remaining_work)
            else {
                return Ok(Vec::new());
            };
            // The old side collects the adjacent established anchors that
            // carry a non-zero uniform support key; stationary entries remain
            // references and never compete for the key. A candidate with no
            // adjacent support key or with two different adjacent support keys
            // stays unresolved.
            if !charge(remaining_work, neighbours.len()) {
                return Ok(Vec::new());
            }
            let mut old_adjacent = Vec::new();
            let mut old_adjacent_key: Option<crate::model::Vec2> = None;
            let mut old_ambiguous = false;
            let mut old_geometry_unknown = false;
            for (index, neighbour) in neighbours.iter().enumerate() {
                match reference_page(&neighbour.old_pages, &neighbour.new_pages, candidate_page) {
                    ReferencePage::Same => {}
                    ReferencePage::Other => continue,
                    ReferencePage::Unknown => {
                        // The page relation cannot be proven; never drop the
                        // reference silently.
                        old_geometry_unknown = true;
                        continue;
                    }
                }
                let Some(neighbour_bounds) = reference_bounds(
                    sides[0],
                    &old_views[neighbour.old_view],
                    &neighbour.old_range,
                    neighbour.old_index,
                    remaining_work,
                ) else {
                    return Ok(Vec::new());
                };
                let Some(neighbour_bounds) = neighbour_bounds else {
                    // The relation to this adjacent established
                    // correspondence cannot be checked; never drop it
                    // silently.
                    old_geometry_unknown = true;
                    continue;
                };
                let index_adjacent = neighbour.old_index.abs_diff(candidate_index) == 1;
                let geometric_adjacent = if index_adjacent {
                    true
                } else {
                    match band_nearest(
                        BandSide {
                            side: sides[0],
                            view: old_view,
                            range: &old_range,
                            block: candidate_index,
                            page: candidate_page,
                        },
                        candidate_old_bounds,
                        BandSide {
                            side: sides[0],
                            view: &old_views[neighbour.old_view],
                            range: &neighbour.old_range,
                            block: neighbour.old_index,
                            page: candidate_page,
                        },
                        neighbour_bounds,
                        remaining_work,
                    ) {
                        Some(value) => value,
                        None => return Ok(Vec::new()),
                    }
                };
                if !geometric_adjacent {
                    continue;
                }
                let Some(neighbour_projection) = project_span(
                    sides[0],
                    &old_views[neighbour.old_view]
                        .group
                        .span(neighbour.old_range.start, neighbour.old_range.end),
                    remaining_work,
                )?
                else {
                    return Ok(Vec::new());
                };
                if projected_overlap(&candidate_projection, &neighbour_projection) {
                    continue;
                }
                let Some(translation) = neighbour.translation else {
                    // A non-uniform transform is a reference only.
                    continue;
                };
                if !mode_supports(mode, translation) {
                    continue;
                }
                match old_adjacent_key {
                    None => old_adjacent_key = Some(translation),
                    Some(previous) if !same_translation(previous, translation) => {
                        old_ambiguous = true;
                    }
                    Some(_) => {}
                }
                old_adjacent.push(index);
            }
            if old_geometry_unknown || old_ambiguous {
                continue;
            }
            let Some(old_adjacent_key) = old_adjacent_key else {
                continue;
            };
            if !mode_supports(mode, old_adjacent_key) {
                continue;
            }
            let key = old_adjacent_key;
            let supporting = old_adjacent
                .iter()
                .copied()
                .filter(|&index| {
                    neighbours[index]
                        .translation
                        .is_some_and(|translation| same_translation(translation, key))
                })
                .collect::<Vec<_>>();
            if supporting.is_empty() {
                continue;
            }
            let old_occurrences = match mode {
                TranslationMode::NonzeroSingleton => translated_occurrences(
                    &old_views,
                    old_view,
                    &old_range,
                    old_view_index,
                    crate::model::Vec2 { x: 0.0, y: 0.0 },
                    remaining_work,
                )?,
                TranslationMode::StationaryMember => positioned_occurrences(
                    &old_views,
                    old_view,
                    &old_range,
                    old_view_index,
                    remaining_work,
                )?,
            };
            let Some(old_occurrences) = old_occurrences else {
                return Ok(Vec::new());
            };
            if old_occurrences.same != 0 || old_occurrences.unknown {
                continue;
            }
            let new_occurrences = match mode {
                TranslationMode::NonzeroSingleton => translated_occurrences(
                    &new_views,
                    old_view,
                    &old_range,
                    usize::MAX,
                    key,
                    remaining_work,
                )?,
                TranslationMode::StationaryMember => positioned_occurrences(
                    &new_views,
                    old_view,
                    &old_range,
                    usize::MAX,
                    remaining_work,
                )?,
            };
            let Some(new_occurrences) = new_occurrences else {
                return Ok(Vec::new());
            };
            if new_occurrences.same != 1 || new_occurrences.unknown {
                continue;
            }
            let Some((new_view_index, new_range, true)) = new_occurrences.matched else {
                continue;
            };
            let new_view = &new_views[new_view_index];
            if mode == TranslationMode::NonzeroSingleton && !new_view.source_bounded {
                continue;
            }
            match mode {
                TranslationMode::NonzeroSingleton => {
                    if new_view.block_indices.len() != 1 {
                        continue;
                    }
                }
                TranslationMode::StationaryMember => {
                    let Some(member) = new_view
                        .block_ranges
                        .iter()
                        .position(|range| *range == new_range)
                    else {
                        continue;
                    };
                    if !whole_original_member(sides[1], new_view, member) {
                        continue;
                    }
                }
            }
            // Re-verify the whole-block translation immediately before the
            // domain is added: full token sequence and one raw finite non-zero
            // translation over every token.
            if old_view.group.tokens[old_range.clone()] != new_view.group.tokens[new_range.clone()]
            {
                continue;
            }
            if !charge(
                remaining_work,
                old_range
                    .len()
                    .saturating_add(new_range.len())
                    .saturating_add(1),
            ) {
                return Ok(Vec::new());
            }
            if !mode_key_matches(mode, old_view, &old_range, new_view, &new_range, key) {
                continue;
            }
            let Some(candidate_new_bounds) = baseline_bounds(new_view, &new_range, remaining_work)
            else {
                return Ok(Vec::new());
            };
            // The new side must agree on the same unique translation among
            // its adjacent established anchors, and one and the same anchor
            // entry must be adjacent on both sides with that key. Two
            // different adjacent translations on either side hold the
            // candidate, so the proof is symmetric under a side swap.
            let Some(new_candidate_member) = new_view
                .block_ranges
                .iter()
                .position(|range| *range == new_range)
            else {
                continue;
            };
            let Some(new_candidate_index) =
                new_view.block_indices.get(new_candidate_member).copied()
            else {
                continue;
            };
            if mode == TranslationMode::StationaryMember
                && candidate_mask
                    .is_some_and(|mask| !mask[1].get(new_candidate_index).copied().unwrap_or(false))
            {
                continue;
            }
            let Some(new_candidate_page) = single_page(new_view, &new_range) else {
                continue;
            };
            let Some(new_candidate_projection) = project_span(
                sides[1],
                &new_view.group.span(new_range.start, new_range.end),
                remaining_work,
            )?
            else {
                return Ok(Vec::new());
            };
            if !charge(remaining_work, neighbours.len()) {
                return Ok(Vec::new());
            }
            let mut new_adjacent = Vec::new();
            let mut new_adjacent_key: Option<crate::model::Vec2> = None;
            let mut new_ambiguous = false;
            let mut new_geometry_unknown = false;
            for (index, neighbour) in neighbours.iter().enumerate() {
                match reference_page(
                    &neighbour.old_pages,
                    &neighbour.new_pages,
                    new_candidate_page,
                ) {
                    ReferencePage::Same => {}
                    ReferencePage::Other => continue,
                    ReferencePage::Unknown => {
                        new_geometry_unknown = true;
                        continue;
                    }
                }
                let Some(neighbour_bounds) = reference_bounds(
                    sides[1],
                    &new_views[neighbour.new_view],
                    &neighbour.new_range,
                    neighbour.new_index,
                    remaining_work,
                ) else {
                    return Ok(Vec::new());
                };
                let Some(neighbour_bounds) = neighbour_bounds else {
                    new_geometry_unknown = true;
                    continue;
                };
                let index_adjacent = neighbour.new_index.abs_diff(new_candidate_index) == 1;
                let geometric_adjacent = if index_adjacent {
                    true
                } else {
                    match band_nearest(
                        BandSide {
                            side: sides[1],
                            view: new_view,
                            range: &new_range,
                            block: new_candidate_index,
                            page: new_candidate_page,
                        },
                        candidate_new_bounds,
                        BandSide {
                            side: sides[1],
                            view: &new_views[neighbour.new_view],
                            range: &neighbour.new_range,
                            block: neighbour.new_index,
                            page: new_candidate_page,
                        },
                        neighbour_bounds,
                        remaining_work,
                    ) {
                        Some(value) => value,
                        None => return Ok(Vec::new()),
                    }
                };
                if !geometric_adjacent {
                    continue;
                }
                let Some(neighbour_projection) = project_span(
                    sides[1],
                    &new_views[neighbour.new_view]
                        .group
                        .span(neighbour.new_range.start, neighbour.new_range.end),
                    remaining_work,
                )?
                else {
                    return Ok(Vec::new());
                };
                if projected_overlap(&new_candidate_projection, &neighbour_projection) {
                    continue;
                }
                let Some(translation) = neighbour.translation else {
                    continue;
                };
                if !mode_supports(mode, translation) {
                    continue;
                }
                match new_adjacent_key {
                    None => new_adjacent_key = Some(translation),
                    Some(previous) if !same_translation(previous, translation) => {
                        new_ambiguous = true;
                    }
                    Some(_) => {}
                }
                new_adjacent.push(index);
            }
            if new_geometry_unknown || new_ambiguous {
                continue;
            }
            match new_adjacent_key {
                Some(adjacent) if same_translation(adjacent, key) => {}
                _ => continue,
            }
            let Some(support_index) = supporting
                .iter()
                .copied()
                .find(|index| new_adjacent.contains(index))
            else {
                continue;
            };
            // The whole-block raw baseline order and intersection relation of
            // the candidate and of the supporting anchor to every established
            // correspondence, stationary ones included, must be unchanged
            // between the two sides. This detects a crossing of an
            // independently established line that the shared translation
            // cannot see. Global reading-order uncertainty is never lifted
            // here: any order change relative to an established correspondence
            // holds the candidate.
            let support = &neighbours[support_index];
            let Some(support_old_bounds) = baseline_bounds(
                &old_views[support.old_view],
                &support.old_range,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let Some(support_new_bounds) = baseline_bounds(
                &new_views[support.new_view],
                &support.new_range,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            if !charge(remaining_work, neighbours.len()) {
                return Ok(Vec::new());
            }
            let mut neighbour_valid = true;
            for reference in &neighbours {
                match reference_page(&reference.old_pages, &reference.new_pages, candidate_page) {
                    ReferencePage::Same => {}
                    ReferencePage::Other => {
                        // Both sides provably do not cover the candidate's
                        // page, so raw coordinates are not order evidence.
                        continue;
                    }
                    ReferencePage::Unknown => {
                        // The page relation cannot be proven; never drop the
                        // reference silently and accept the candidate.
                        neighbour_valid = false;
                        break;
                    }
                }
                let Some(reference_old_bounds) = reference_bounds(
                    sides[0],
                    &old_views[reference.old_view],
                    &reference.old_range,
                    reference.old_index,
                    remaining_work,
                ) else {
                    return Ok(Vec::new());
                };
                let Some(reference_new_bounds) = reference_bounds(
                    sides[1],
                    &new_views[reference.new_view],
                    &reference.new_range,
                    reference.new_index,
                    remaining_work,
                ) else {
                    return Ok(Vec::new());
                };
                let (Some(reference_old_bounds), Some(reference_new_bounds)) =
                    (reference_old_bounds, reference_new_bounds)
                else {
                    // No geometry at all for this same-page established
                    // correspondence; never drop it silently and accept the
                    // candidate.
                    neighbour_valid = false;
                    break;
                };
                if reference.old_view == old_view_index
                    && reference.new_view == new_view_index
                    && reference.old_member.cmp(&block)
                        != reference.new_member.cmp(&new_candidate_member)
                {
                    // The candidate and the reference are members of one and
                    // the same view on both sides, so their member order is
                    // fixed by that view. A reversed order means the run's
                    // own order contract is violated and the candidate is
                    // held, whatever the raw coordinates say.
                    neighbour_valid = false;
                    break;
                }
                if !same_relative_geometry(
                    candidate_old_bounds,
                    reference_old_bounds,
                    candidate_new_bounds,
                    reference_new_bounds,
                ) || !same_relative_geometry(
                    support_old_bounds,
                    reference_old_bounds,
                    support_new_bounds,
                    reference_new_bounds,
                ) {
                    neighbour_valid = false;
                    break;
                }
            }
            if !neighbour_valid {
                continue;
            }
            let old_span = old_view.group.span(old_range.start, old_range.end);
            let new_span = new_view.group.span(new_range.start, new_range.end);
            if !compatible_roles(sides, [&old_span, &new_span], remaining_work)?
                || super::span_has_source_issues(sides[0], &old_span, remaining_work)?
                || super::span_has_source_issues(sides[1], &new_span, remaining_work)?
            {
                continue;
            }
            domains.push(LocalDomain {
                old_span,
                new_span,
                source_bounded: true,
            });
        }
    }
    if mode == TranslationMode::StationaryMember && *remaining_work == 0 {
        // A cut anywhere in this pass drops every domain it found, so a
        // mid-budget exhaustion never leaves a partial stationary proof.
        return Ok(Vec::new());
    }
    Ok(domains)
}

/// One resolved established correspondence used as a bracket boundary.
struct BracketAnchor {
    old_index: usize,
    new_index: usize,
    old_view: usize,
    new_view: usize,
    old_range: std::ops::Range<usize>,
    new_range: std::ops::Range<usize>,
    old_pages: Vec<u32>,
    new_pages: Vec<u32>,
}

/// One side's block geometry: the source side, its views and the block-to-view
/// and block-to-range maps built once per pass.
#[derive(Clone, Copy)]
struct SideGeometry<'a> {
    side: &'a Side<'a>,
    views: &'a [View],
    view_of_block: &'a [usize],
    range_of_block: &'a [Option<std::ops::Range<usize>>],
    page: u32,
}

/// Whether exactly one source block lies strictly inside one bracket region.
///
/// The two boundary blocks themselves are excluded explicitly. Every other
/// source block on the candidate's page is classified: a block provably on
/// another page is skipped; an empty or page-ambiguous block holds; a block
/// without geometry holds; a block whose baseline x-interval does not overlap
/// the band is ignored; a block whose baseline y-interval does not intersect
/// the open vertical region is ignored, so touching a boundary from outside is
/// not inside. A block that intersects the region and is fully contained is an
/// additional candidate, so a second one holds; a block that straddles either
/// boundary holds because the region's source closure does not hold.
/// `Some(None)` reports that the region is not unique or cannot be proven;
/// `None` reports an exhausted shared work budget.
fn region_unique(
    geometry: SideGeometry<'_>,
    boundaries: [usize; 2],
    band: (f64, f64),
    region: (f64, f64),
    remaining: &mut usize,
) -> Option<Option<usize>> {
    let side = geometry.side;
    let views = geometry.views;
    let view_of_block = geometry.view_of_block;
    let range_of_block = geometry.range_of_block;
    let candidate_page = geometry.page;
    if !charge(remaining, side.blocks.len()) {
        return None;
    }
    let mut unique = None;
    for (index, block) in side.blocks.iter().enumerate() {
        if boundaries.contains(&index) {
            continue;
        }
        if block.pages.is_empty()
            || (block.pages.len() > 1 && block.pages.contains(&candidate_page))
        {
            return Some(None);
        }
        if !block.pages.contains(&candidate_page) {
            continue;
        }
        let view_index = view_of_block[index];
        let Some(range) = range_of_block[index].as_ref() else {
            // No view carries this same-page block; the region cannot be
            // proven.
            return Some(None);
        };
        if view_index == usize::MAX {
            return Some(None);
        }
        let view = &views[view_index];
        let Some(bounds) = reference_bounds(side, view, range, index, remaining)? else {
            // No geometry at all for this same-page block; the region cannot
            // be proven.
            return Some(None);
        };
        if bounds.0.max(band.0) > bounds.2.min(band.1) {
            continue;
        }
        // The open vertical interval excludes both boundary lines: a block
        // that only touches a boundary from outside does not intersect it.
        if bounds.3 <= region.0 || bounds.1 >= region.1 {
            continue;
        }
        if bounds.1 <= region.0 || bounds.3 >= region.1 {
            // The block straddles a boundary, so the source closure of the
            // region does not hold.
            return Some(None);
        }
        if unique.is_some() {
            return Some(None);
        }
        unique = Some(index);
    }
    Some(unique)
}

/// Whether another whole source-bounded line on `page` has the same token
/// sequence as the candidate or as its new counterpart.
///
/// The bracketed proof pins the region geometrically and never selects by
/// text, but a duplicate whole line elsewhere on the page could make the
/// region mirror a different physical line. The guard is deliberately
/// conservative and only withholds the candidate; it never picks one. The
/// token construction and comparison are charged by token length, not by
/// block count. `None` reports an exhausted shared work budget.
fn bracketed_tokens_duplicated(
    sides: [&Side<'_>; 2],
    page: u32,
    old_index: usize,
    new_index: usize,
    old_tokens: &[ComparableToken],
    new_tokens: &[ComparableToken],
    remaining: &mut usize,
) -> Option<bool> {
    if !charge(
        remaining,
        old_tokens
            .len()
            .saturating_add(new_tokens.len())
            .saturating_add(2),
    ) {
        return None;
    }
    for (side_index, tokens, excluded) in [
        (0usize, old_tokens, old_index),
        (1usize, new_tokens, new_index),
    ] {
        if !charge(remaining, sides[side_index].blocks.len()) {
            return None;
        }
        for (index, block) in sides[side_index].blocks.iter().enumerate() {
            if index == excluded || block.pages.len() != 1 || block.pages[0] != page {
                continue;
            }
            if !source_bounded_block(block, remaining) {
                continue;
            }
            let Ok(block_tokens) = block.canonical.comparable_tokens() else {
                continue;
            };
            if !charge(
                remaining,
                block_tokens
                    .len()
                    .saturating_add(tokens.len())
                    .saturating_add(1),
            ) {
                return None;
            }
            if block_tokens.as_slice() == tokens {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// The nearest established boundary above or below a candidate in one band,
/// together with whether a second boundary sits at the same distance.
struct NearestBoundary {
    index: usize,
    y: f64,
    tied: bool,
}

/// Updates a nearest-boundary slot with one strictly above or below anchor.
///
/// `above` selects the smallest facing edge, the boundary closest above the
/// candidate; otherwise the largest facing edge, the boundary closest below
/// it. A second anchor with a bit-identical facing edge makes the choice
/// ambiguous and the proof is withheld instead of depending on the evidence
/// order.
fn update_nearest(slot: &mut Option<NearestBoundary>, index: usize, y: f64, above: bool) {
    match slot {
        None => {
            *slot = Some(NearestBoundary {
                index,
                y,
                tied: false,
            });
        }
        Some(nearest) if y == nearest.y => nearest.tied = true,
        Some(nearest) if (above && y < nearest.y) || (!above && y > nearest.y) => {
            *slot = Some(NearestBoundary {
                index,
                y,
                tied: false,
            });
        }
        Some(_) => {}
    }
}

/// The nearest established boundary above and below a candidate on one side.
///
/// Every established reference on the candidate's page whose baseline
/// x-interval overlaps the candidate's x-interval competes. The nearest above
/// is the one with the smallest facing edge and the nearest below the one with
/// the largest facing edge; a second reference at the same facing edge makes
/// the choice ambiguous. The same function, the same x-overlap criterion and
/// the same tie condition run on both sides, and the caller requires one and
/// the same reference to win on both. A reference provably on another page is
/// skipped; an unknown page relation or missing reference geometry holds the
/// proof instead of dropping the reference. `None` reports an exhausted
/// budget; `Some(None)` reports that the nearest boundaries cannot be
/// established; `Some(Some((above, below)))` returns the anchor indices.
fn nearest_boundaries(
    geometry: SideGeometry<'_>,
    side_index: usize,
    anchors: &[BracketAnchor],
    candidate_index: usize,
    candidate_bounds: (f64, f64, f64, f64),
    remaining: &mut usize,
) -> Option<Option<(usize, usize)>> {
    let side = geometry.side;
    let views = geometry.views;
    let view_of_block = geometry.view_of_block;
    let range_of_block = geometry.range_of_block;
    let candidate_page = geometry.page;
    let mut above: Option<NearestBoundary> = None;
    let mut below: Option<NearestBoundary> = None;
    if !charge(remaining, anchors.len()) {
        return None;
    }
    for (index, anchor) in anchors.iter().enumerate() {
        let anchor_index = if side_index == 0 {
            anchor.old_index
        } else {
            anchor.new_index
        };
        if anchor_index == candidate_index {
            continue;
        }
        match reference_page(&anchor.old_pages, &anchor.new_pages, candidate_page) {
            ReferencePage::Same => {}
            ReferencePage::Other => continue,
            ReferencePage::Unknown => return Some(None),
        }
        let view_index = view_of_block[anchor_index];
        let Some(range) = range_of_block[anchor_index].as_ref() else {
            return Some(None);
        };
        if view_index == usize::MAX {
            return Some(None);
        }
        let Some(anchor_bounds) =
            reference_bounds(side, &views[view_index], range, anchor_index, remaining)?
        else {
            // No geometry at all for this established reference; the nearest
            // boundary cannot be chosen from an incomplete inspection set.
            return Some(None);
        };
        if candidate_bounds.0.max(anchor_bounds.0) > candidate_bounds.2.min(anchor_bounds.2) {
            continue;
        }
        if anchor_bounds.1 > candidate_bounds.3 {
            update_nearest(&mut above, index, anchor_bounds.1, true);
        } else if anchor_bounds.3 < candidate_bounds.1 {
            update_nearest(&mut below, index, anchor_bounds.3, false);
        }
    }
    let (Some(above), Some(below)) = (above, below) else {
        return Some(None);
    };
    if above.tied || below.tied {
        return Some(None);
    }
    Some(Some((above.index, below.index)))
}

/// Discovers a local domain for a whole source-bounded line that is the unique
/// source block between two independently established correspondences in the
/// same column band.
///
/// The candidate must be a whole source-bounded untrusted singleton with
/// horizontal text, one page and complete per-token metadata. The nearest
/// established whole-block correspondence strictly above and strictly below it
/// in the same baseline x-band, on the candidate's page and with a complete
/// source geometry, define a region; the same selection function with the same
/// x-overlap criterion and tie condition must pick one and the same boundary
/// correspondence on both sides, and an equal-distance second boundary
/// withholds the proof instead of resolving by evidence order. On each side
/// every source block whose baseline x-interval overlaps the anchors' band and
/// whose baseline y-interval intersects the open anchors' interval must be
/// exactly one fully contained block, and the new side's unique block must
/// itself be a whole source-bounded singleton on the same page. Missing
/// geometry, an empty or page-ambiguous block, an additional candidate or
/// point obstacle, a boundary that straddles the region, a boundary order
/// change, a crossing of any other established reference, a whole-line token
/// duplicate elsewhere on the page or a non-unique region holds the proof.
///
/// The tokens are not compared here and no text similarity is used: the
/// boundaries prove the region correspondence, and the ordinary local
/// assessment compares the tokens with the strict minimal-edit uniqueness.
/// The anchors are independently established before this pass, so the proof
/// never depends on the candidate, and the global reading order is not
/// promoted. Every established reference stays in the inspection set even
/// when it cannot support a boundary; a reference whose page or geometry
/// cannot be proven holds the candidate. `None` is never returned; an
/// exhausted budget drops only this pass's additions.
pub(super) fn discover_bracketed_domains(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    established: &[EstablishedBlock],
    remaining_work: &mut usize,
    max_ranges: usize,
) -> Result<Vec<LocalDomain>> {
    if max_ranges == 0
        || *remaining_work == 0
        || established.is_empty()
        || sides.iter().any(|side| side.blocks.is_empty())
    {
        return Ok(Vec::new());
    }
    if recovery.old_trusted_run_intervals.len() != sides[0].blocks.len()
        || recovery.new_trusted_run_intervals.len() != sides[1].blocks.len()
    {
        return Err(super::invalid(
            "trusted run interval metadata must match normalized blocks",
        ));
    }
    let old_descriptors = recovery
        .old_trusted_run_evidence
        .map(|evidence| evidence.descriptors);
    let new_descriptors = recovery
        .new_trusted_run_evidence
        .map(|evidence| evidence.descriptors);
    let Some(old_views) = build_views(
        sides[0],
        recovery.old_trusted_run_intervals,
        old_descriptors,
        remaining_work,
    )?
    else {
        return Ok(Vec::new());
    };
    let Some(new_views) = build_views(
        sides[1],
        recovery.new_trusted_run_intervals,
        new_descriptors,
        remaining_work,
    )?
    else {
        return Ok(Vec::new());
    };
    if old_views.is_empty() || new_views.is_empty() {
        return Ok(Vec::new());
    }
    if !charge(
        remaining_work,
        sides[0].blocks.len().saturating_add(sides[1].blocks.len()),
    ) {
        return Ok(Vec::new());
    }
    // Block-to-view and block-to-range maps are built once so the per-block
    // classification never scans the view list; the build itself is charged.
    let mut view_of_block = [
        vec![usize::MAX; sides[0].blocks.len()],
        vec![usize::MAX; sides[1].blocks.len()],
    ];
    let mut range_of_block: [Vec<Option<std::ops::Range<usize>>>; 2] = [
        vec![None; sides[0].blocks.len()],
        vec![None; sides[1].blocks.len()],
    ];
    for (side, side_views) in [&old_views, &new_views].into_iter().enumerate() {
        for (view_index, view) in side_views.iter().enumerate() {
            for (position, &block_index) in view.block_indices.iter().enumerate() {
                if let Some(slot) = view_of_block[side].get_mut(block_index) {
                    *slot = view_index;
                }
                if let Some(slot) = range_of_block[side].get_mut(block_index) {
                    *slot = Some(view.block_ranges[position].clone());
                }
            }
        }
    }
    // Resolve every established reference to its views, ranges and source
    // pages. A reference that cannot be resolved is never dropped silently:
    // the pass holds every candidate instead of proving with an incomplete
    // inspection set.
    let mut anchors = Vec::new();
    let mut reference_unresolved = false;
    for neighbour in established {
        if !charge(remaining_work, 1) {
            return Ok(Vec::new());
        }
        let Some(&old_index) = sides[0].index.get(&neighbour.old_block) else {
            reference_unresolved = true;
            continue;
        };
        let Some(&new_index) = sides[1].index.get(&neighbour.new_block) else {
            reference_unresolved = true;
            continue;
        };
        let old_view_index = view_of_block[0][old_index];
        let new_view_index = view_of_block[1][new_index];
        if old_view_index == usize::MAX || new_view_index == usize::MAX {
            reference_unresolved = true;
            continue;
        }
        if !charge(
            remaining_work,
            old_views[old_view_index]
                .block_indices
                .len()
                .saturating_add(new_views[new_view_index].block_indices.len()),
        ) {
            return Ok(Vec::new());
        }
        let (Some(old_range), Some(new_range)) = (
            range_of_block[0][old_index].clone(),
            range_of_block[1][new_index].clone(),
        ) else {
            reference_unresolved = true;
            continue;
        };
        anchors.push(BracketAnchor {
            old_index,
            new_index,
            old_view: old_view_index,
            new_view: new_view_index,
            old_range,
            new_range,
            old_pages: sides[0].blocks[old_index].pages.clone(),
            new_pages: sides[1].blocks[new_index].pages.clone(),
        });
    }
    if reference_unresolved || anchors.is_empty() {
        return Ok(Vec::new());
    }
    let mut domains = Vec::new();
    'views: for old_view in &old_views {
        if !matches!(old_view.kind, ViewKind::Untrusted(_)) || !old_view.source_bounded {
            continue;
        }
        for block in 0..old_view.block_ranges.len() {
            if !positioned_block(old_view, block) {
                continue;
            }
            if old_view.block_indices.len() != 1 {
                continue;
            }
            let old_range = old_view.block_ranges[block].clone();
            if old_range.start != 0 || old_range.end != old_view.group.tokens.len() {
                continue;
            }
            if !charge(remaining_work, old_range.len().saturating_add(1)) {
                return Ok(Vec::new());
            }
            if old_view.token_positions[old_range.clone()]
                .iter()
                .any(Option::is_none)
                || old_view.token_pages[old_range.clone()]
                    .iter()
                    .any(Option::is_none)
            {
                continue;
            }
            if domains.len() >= max_ranges {
                break 'views;
            }
            let candidate_index = old_view.block_indices[0];
            let Some(candidate_page) = single_page(old_view, &old_range) else {
                continue;
            };
            let Some(candidate_bounds) = baseline_bounds(old_view, &old_range, remaining_work)
            else {
                return Ok(Vec::new());
            };
            // The nearest established boundary strictly above and below the
            // candidate on this side, chosen by the same function, the same
            // x-overlap criterion and the same tie condition as the new side.
            let Some(old_nearest) = nearest_boundaries(
                SideGeometry {
                    side: sides[0],
                    views: &old_views,
                    view_of_block: &view_of_block[0],
                    range_of_block: &range_of_block[0],
                    page: candidate_page,
                },
                0,
                &anchors,
                candidate_index,
                candidate_bounds,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let Some((above_index, below_index)) = old_nearest else {
                continue;
            };
            let above_anchor = &anchors[above_index];
            let below_anchor = &anchors[below_index];
            // Both boundaries need complete geometry on both sides and must
            // keep their relative order on the new side.
            let Some(above_old) = reference_bounds(
                sides[0],
                &old_views[above_anchor.old_view],
                &above_anchor.old_range,
                above_anchor.old_index,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let Some(below_old) = reference_bounds(
                sides[0],
                &old_views[below_anchor.old_view],
                &below_anchor.old_range,
                below_anchor.old_index,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let Some(above_new) = reference_bounds(
                sides[1],
                &new_views[above_anchor.new_view],
                &above_anchor.new_range,
                above_anchor.new_index,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let Some(below_new) = reference_bounds(
                sides[1],
                &new_views[below_anchor.new_view],
                &below_anchor.new_range,
                below_anchor.new_index,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let (Some(above_old), Some(below_old), Some(above_new), Some(below_new)) =
                (above_old, below_old, above_new, below_new)
            else {
                continue;
            };
            if above_old.1 <= below_old.3 || above_new.1 <= below_new.3 {
                continue;
            }
            let old_band = (above_old.0.max(below_old.0), above_old.2.min(below_old.2));
            let new_band = (above_new.0.max(below_new.0), above_new.2.min(below_new.2));
            if old_band.0 >= old_band.1 || new_band.0 >= new_band.1 {
                continue;
            }
            let old_region = (below_old.3, above_old.1);
            let new_region = (below_new.3, above_new.1);
            // The candidate must lie strictly inside its own region and band.
            if candidate_bounds.0.max(old_band.0) > candidate_bounds.2.min(old_band.1)
                || candidate_bounds.1 <= old_region.0
                || candidate_bounds.3 >= old_region.1
            {
                continue;
            }
            let Some(old_unique) = region_unique(
                SideGeometry {
                    side: sides[0],
                    views: &old_views,
                    view_of_block: &view_of_block[0],
                    range_of_block: &range_of_block[0],
                    page: candidate_page,
                },
                [above_anchor.old_index, below_anchor.old_index],
                old_band,
                old_region,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            if old_unique != Some(candidate_index) {
                continue;
            }
            let Some(new_unique) = region_unique(
                SideGeometry {
                    side: sides[1],
                    views: &new_views,
                    view_of_block: &view_of_block[1],
                    range_of_block: &range_of_block[1],
                    page: candidate_page,
                },
                [above_anchor.new_index, below_anchor.new_index],
                new_band,
                new_region,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            let Some(new_index) = new_unique else {
                continue;
            };
            let new_view_index = view_of_block[1][new_index];
            if new_view_index == usize::MAX {
                continue;
            }
            let new_view = &new_views[new_view_index];
            if !matches!(new_view.kind, ViewKind::Untrusted(_)) || !new_view.source_bounded {
                continue;
            }
            if new_view.block_indices.len() != 1 || new_view.block_indices[0] != new_index {
                continue;
            }
            if !positioned_block(new_view, 0) {
                continue;
            }
            let new_range = new_view.block_ranges[0].clone();
            if new_range.start != 0 || new_range.end != new_view.group.tokens.len() {
                continue;
            }
            if !charge(
                remaining_work,
                new_range.len().saturating_mul(2).saturating_add(1),
            ) {
                return Ok(Vec::new());
            }
            if new_view.token_positions[new_range.clone()]
                .iter()
                .any(Option::is_none)
                || new_view.token_pages[new_range.clone()]
                    .iter()
                    .any(Option::is_none)
            {
                continue;
            }
            if single_page(new_view, &new_range) != Some(candidate_page) {
                continue;
            }
            let Some(new_candidate_bounds) = baseline_bounds(new_view, &new_range, remaining_work)
            else {
                return Ok(Vec::new());
            };
            // The new candidate must lie strictly inside the new region and
            // band, so the same boundary pair brackets it on both sides.
            if new_candidate_bounds.0.max(new_band.0) > new_candidate_bounds.2.min(new_band.1)
                || new_candidate_bounds.1 <= new_region.0
                || new_candidate_bounds.3 >= new_region.1
            {
                continue;
            }
            // The same selection function must pick one and the same
            // boundary correspondence on the new side; a different or
            // ambiguous nearest boundary withholds the proof.
            let Some(new_nearest) = nearest_boundaries(
                SideGeometry {
                    side: sides[1],
                    views: &new_views,
                    view_of_block: &view_of_block[1],
                    range_of_block: &range_of_block[1],
                    page: candidate_page,
                },
                1,
                &anchors,
                new_index,
                new_candidate_bounds,
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            if new_nearest != Some((above_index, below_index)) {
                continue;
            }
            // Every established reference, including ones that cannot support
            // a boundary, is checked for a relative-geometry change against
            // the candidate and against both boundaries; a missing page or
            // geometry holds the candidate instead of being dropped.
            let mut reference_valid = true;
            if !charge(remaining_work, anchors.len()) {
                return Ok(Vec::new());
            }
            for reference in &anchors {
                match reference_page(&reference.old_pages, &reference.new_pages, candidate_page) {
                    ReferencePage::Same => {}
                    ReferencePage::Other => continue,
                    ReferencePage::Unknown => {
                        reference_valid = false;
                        break;
                    }
                }
                let Some(reference_old) = reference_bounds(
                    sides[0],
                    &old_views[reference.old_view],
                    &reference.old_range,
                    reference.old_index,
                    remaining_work,
                ) else {
                    return Ok(Vec::new());
                };
                let Some(reference_new) = reference_bounds(
                    sides[1],
                    &new_views[reference.new_view],
                    &reference.new_range,
                    reference.new_index,
                    remaining_work,
                ) else {
                    return Ok(Vec::new());
                };
                let (Some(reference_old), Some(reference_new)) = (reference_old, reference_new)
                else {
                    reference_valid = false;
                    break;
                };
                if !same_relative_geometry(
                    candidate_bounds,
                    reference_old,
                    new_candidate_bounds,
                    reference_new,
                ) || !same_relative_geometry(above_old, reference_old, above_new, reference_new)
                    || !same_relative_geometry(below_old, reference_old, below_new, reference_new)
                {
                    reference_valid = false;
                    break;
                }
            }
            if !reference_valid {
                continue;
            }
            let Some(duplicated) = bracketed_tokens_duplicated(
                sides,
                candidate_page,
                candidate_index,
                new_index,
                &old_view.group.tokens[old_range.clone()],
                &new_view.group.tokens[new_range.clone()],
                remaining_work,
            ) else {
                return Ok(Vec::new());
            };
            if duplicated {
                continue;
            }
            let old_span = old_view.group.span(old_range.start, old_range.end);
            let new_span = new_view.group.span(new_range.start, new_range.end);
            if !compatible_roles(sides, [&old_span, &new_span], remaining_work)?
                || super::span_has_source_issues(sides[0], &old_span, remaining_work)?
                || super::span_has_source_issues(sides[1], &new_span, remaining_work)?
            {
                continue;
            }
            domains.push(LocalDomain {
                old_span,
                new_span,
                source_bounded: true,
            });
        }
    }
    Ok(domains)
}

fn split_at_barriers(
    sides: [&Side<'_>; 2],
    views: [&View; 2],
    anchors: &[AnchorHit],
    remaining: &mut usize,
) -> Result<Option<Vec<LocalDomain>>> {
    let mut parts = Vec::new();
    let mut current: Option<LocalDomain> = None;
    for anchor in anchors {
        let next = LocalDomain {
            old_span: views[0].group.span(anchor.old_start, anchor.old_end),
            new_span: views[1].group.span(anchor.new_start, anchor.new_end),
            source_bounded: views[0].source_bounded && views[1].source_bounded,
        };
        if let Some(previous) = current.take() {
            let combined = LocalDomain {
                old_span: views[0]
                    .group
                    .span(previous.old_span.comparable_range.start, anchor.old_end),
                new_span: views[1]
                    .group
                    .span(previous.new_span.comparable_range.start, anchor.new_end),
                source_bounded: views[0].source_bounded && views[1].source_bounded,
            };
            if compatible_roles(sides, [&combined.old_span, &combined.new_span], remaining)?
                && !super::span_has_source_issues(sides[0], &combined.old_span, remaining)?
                && !super::span_has_source_issues(sides[1], &combined.new_span, remaining)?
            {
                current = Some(combined);
                continue;
            }
            parts.push(previous);
        }
        if *remaining == 0 {
            return Ok(None);
        }
        current = Some(next);
    }
    parts.extend(current);
    Ok(Some(parts))
}

fn build_views(
    side: &Side<'_>,
    intervals: &[Option<TrustedRunInterval>],
    descriptors: Option<&[TrustedRunDescriptor]>,
    remaining_work: &mut usize,
) -> Result<Option<Vec<View>>> {
    let mut runs = HashMap::<TrustedRunId, Vec<RunMember>>::new();
    let mut invalid_runs = HashSet::<TrustedRunId>::new();
    for (block_index, interval) in intervals.iter().copied().enumerate() {
        let Some(interval) = interval else {
            continue;
        };
        if interval.start >= interval.end {
            invalid_runs.insert(interval.run_id);
            continue;
        }
        runs.entry(interval.run_id).or_default().push(RunMember {
            block_index,
            interval,
        });
    }
    for run_id in invalid_runs {
        runs.remove(&run_id);
    }

    let mut views = Vec::new();
    let mut trusted_block_indices = HashSet::new();
    for (run_id, mut members) in runs {
        members.sort_unstable_by_key(|member| {
            (
                member.interval.start,
                member.interval.end,
                member.block_index,
            )
        });
        if !complete_run(&members) || !descriptor_allows(descriptors, run_id, &members) {
            continue;
        }
        let block_indices = members
            .iter()
            .map(|member| member.block_index)
            .collect::<Vec<_>>();
        trusted_block_indices.extend(block_indices.iter().copied());
        let block_ids = block_indices
            .iter()
            .map(|&index| side.blocks[index].block)
            .collect::<Vec<_>>();
        let group = side.canonical_group(&block_ids, Some(BlockSeparator::Space));
        if !charge_group(remaining_work, &group) {
            return Ok(None);
        }
        let block_bounded = block_indices
            .iter()
            .map(|&block_index| source_bounded_block(&side.blocks[block_index], remaining_work))
            .collect::<Vec<_>>();
        let mut block_candidates = Vec::with_capacity(block_indices.len());
        for (&block_index, &bounded) in block_indices.iter().zip(&block_bounded) {
            let Some(candidate) = block_candidate(side, block_index, bounded, remaining_work)
            else {
                return Ok(None);
            };
            block_candidates.push(candidate);
        }
        let Some((token_positions, token_pages, block_ranges)) = view_token_metadata(
            side,
            &block_indices,
            &block_bounded,
            Some(BlockSeparator::Space),
            remaining_work,
        ) else {
            return Ok(None);
        };
        views.push(View {
            kind: ViewKind::Trusted(run_id),
            source_order: block_indices.iter().copied().min().unwrap_or(usize::MAX),
            block_indices,
            group,
            source_bounded: false,
            horizontal_text: false,
            position_signatures: Vec::new(),
            page: None,
            block_ranges,
            block_candidates,
            token_positions,
            token_pages,
        });
    }

    for (block_index, _) in intervals.iter().enumerate() {
        if trusted_block_indices.contains(&block_index) {
            continue;
        }
        let block_id = side.blocks[block_index].block;
        let group = side.canonical_group(&[block_id], None);
        if !charge_group(remaining_work, &group) {
            return Ok(None);
        }
        let source_bounded = source_bounded_block(&side.blocks[block_index], remaining_work);
        let mut horizontal_text = false;
        let mut position_signatures = Vec::new();
        let mut page = None;
        if source_bounded {
            let block = &side.blocks[block_index];
            let Some(signatures) = block.position_signatures.as_deref() else {
                // `source_bounded_block` guarantees signatures; fail closed.
                *remaining_work = 0;
                return Ok(None);
            };
            if !charge(remaining_work, signatures.len()) {
                return Ok(None);
            }
            horizontal_text = signatures.iter().all(horizontal_direction);
            // One first-source position per canonical token, on one page.
            if signatures.len() == block.canonical.text.chars().count()
                && let [page_number] = block.pages.as_slice()
            {
                position_signatures = signatures.to_vec();
                page = Some(*page_number);
            }
        }
        let Some(block_candidate) =
            block_candidate(side, block_index, source_bounded, remaining_work)
        else {
            return Ok(None);
        };
        let Some((token_positions, token_pages, block_ranges)) = view_token_metadata(
            side,
            &[block_index],
            &[source_bounded],
            None,
            remaining_work,
        ) else {
            return Ok(None);
        };
        views.push(View {
            kind: ViewKind::Untrusted(block_index),
            source_order: block_index,
            block_indices: vec![block_index],
            group,
            source_bounded,
            horizontal_text,
            position_signatures,
            page,
            block_ranges,
            block_candidates: vec![block_candidate],
            token_positions,
            token_pages,
        });
    }
    views.sort_unstable_by_key(|view| {
        (
            view.source_order,
            view.block_indices.len(),
            view.block_indices.first().copied().unwrap_or(usize::MAX),
        )
    });
    Ok(Some(views))
}

fn complete_run(members: &[RunMember]) -> bool {
    let Some(first) = members.first() else {
        return false;
    };
    first.interval.start == 0
        && members.windows(2).all(|pair| {
            pair[0].interval.end == pair[1].interval.start
                && pair[0].interval.end > pair[0].interval.start
        })
}

/// Matches the layout's axis-alignment tolerance for a single normalized
/// direction component without importing the layout constant.
const HORIZONTAL_DIRECTION_TOLERANCE: f64 = 1.0e-6;

/// Exact per-token source metadata of one view: position signature and page
/// per canonical token.
type ViewTokenMetadata = (
    Vec<Option<crate::normalize::PositionSignature>>,
    Vec<Option<u32>>,
    Vec<std::ops::Range<usize>>,
);

/// Exact per-token source metadata of one view, mirroring the separator
/// insertion of `canonical_group`: the first source glyph's position signature
/// and single page behind every canonical token. A token reports `None` when
/// its block lacks complete source evidence (issues, unmapped characters,
/// non-whitespace normalization, line or page breaks, multiple pages, missing
/// signatures), so an occurrence whose source attribution cannot be proven is
/// never compared as if its position were known. Separator tokens inserted
/// between blocks carry no metadata. `None` reports an exhausted shared work
/// budget.
fn view_token_metadata(
    side: &Side<'_>,
    block_indices: &[usize],
    bounded: &[bool],
    separator: Option<BlockSeparator>,
    remaining: &mut usize,
) -> Option<ViewTokenMetadata> {
    let mut positions = Vec::new();
    let mut pages = Vec::new();
    let mut block_ranges = Vec::new();
    let mut preceding_space = false;
    for (position, (&block_index, &bounded)) in block_indices.iter().zip(bounded).enumerate() {
        let block = &side.blocks[block_index];
        let tokens = &side.canonical[block_index];
        let mut insert_space = false;
        if position > 0 {
            insert_space = separator.map(|separator| separator.at(position - 1))
                == Some(BlockSeparator::Space)
                && !preceding_space
                && !tokens.first().is_some_and(super::space_token);
            if insert_space {
                positions.push(None);
                pages.push(None);
            }
        }
        if !charge(remaining, tokens.len().saturating_add(1)) {
            return None;
        }
        let signatures = bounded
            .then_some(block.position_signatures.as_deref())
            .flatten()
            .filter(|signatures| signatures.len() == tokens.len());
        let page = bounded
            .then_some((block.pages.len() == 1).then(|| block.pages[0]))
            .flatten();
        block_ranges.push(positions.len()..positions.len() + tokens.len());
        for index in 0..tokens.len() {
            positions.push(signatures.map(|signatures| signatures[index]));
            pages.push(page);
        }
        preceding_space = tokens
            .last()
            .map_or(insert_space || preceding_space, super::space_token);
    }
    Some((positions, pages, block_ranges))
}

/// A complete source-bounded original block with horizontal left-to-right
/// text, one page and one exact position per canonical token may close a
/// positioned equality. `None` reports an exhausted shared work budget.
fn block_candidate(
    side: &Side<'_>,
    block_index: usize,
    bounded: bool,
    remaining: &mut usize,
) -> Option<bool> {
    if !bounded {
        return Some(false);
    }
    let block = &side.blocks[block_index];
    if block.canonical.text.is_empty() || !left_to_right_text(block) || block.pages.len() != 1 {
        return Some(false);
    }
    let tokens = &side.canonical[block_index];
    let Some(signatures) = block.position_signatures.as_deref() else {
        return Some(false);
    };
    if signatures.len() != tokens.len() {
        return Some(false);
    }
    if !charge(remaining, signatures.len()) {
        return None;
    }
    Some(signatures.iter().all(horizontal_direction))
}

/// True when the canonical text has no right-to-left or explicit
/// bidirectional content, mirroring the layout's line direction classification.
/// Neutral text such as bare digits stays eligible; RTL, mixed and
/// bidi-controlled text does not.
fn left_to_right_text(block: &BlockText) -> bool {
    use unicode_bidi::BidiClass;
    block.canonical.text.chars().all(|character| {
        !matches!(
            unicode_bidi::bidi_class(character),
            BidiClass::R
                | BidiClass::AL
                | BidiClass::LRE
                | BidiClass::LRI
                | BidiClass::LRO
                | BidiClass::RLE
                | BidiClass::RLI
                | BidiClass::RLO
                | BidiClass::FSI
                | BidiClass::PDI
                | BidiClass::PDF
        )
    })
}

fn horizontal_direction(signature: &crate::normalize::PositionSignature) -> bool {
    let direction = signature.direction();
    if !direction.x.is_finite() || !direction.y.is_finite() {
        return false;
    }
    let length_squared = direction.x * direction.x + direction.y * direction.y;
    if !length_squared.is_finite() || length_squared <= f64::EPSILON {
        return false;
    }
    let normalized_y = direction.y / length_squared.sqrt();
    normalized_y.is_finite()
        && normalized_y.abs() <= HORIZONTAL_DIRECTION_TOLERANCE
        && direction.x > 0.0
}

fn view_is_anchorable(view: &View) -> bool {
    matches!(view.kind, ViewKind::Trusted(_)) || view.source_bounded
}

/// A complete single line can propose its common ending as a second anchor.
/// The line boundary alone never closes a domain: the ending must occur exactly
/// once across the available views on each side, just like the initial anchor.
fn add_source_end_anchors(
    views: [&[View]; 2],
    pairs: &mut BTreeMap<(usize, usize), Vec<AnchorHit>>,
    remaining: &mut usize,
) -> Result<bool> {
    for (&(old_view, new_view), hits) in pairs.iter_mut() {
        let old = &views[0][old_view];
        let new = &views[1][new_view];
        if !old.source_bounded || !new.source_bounded {
            continue;
        }
        let mut length = 0;
        for (old_token, new_token) in old
            .group
            .tokens
            .iter()
            .rev()
            .zip(new.group.tokens.iter().rev())
        {
            if !charge(remaining, 1) {
                return Ok(false);
            }
            if old_token != new_token {
                break;
            }
            length += 1;
        }
        if length == 0 {
            continue;
        }
        let old_start = old.group.tokens.len() - length;
        let new_start = new.group.tokens.len() - length;
        // A source-bounded view whose entire content is a common suffix needs
        // no preceding anchor: the suffix itself is the whole view, and the
        // unique-occurrence search below still proves that occurrence.
        let whole_view = old_start == 0 && new_start == 0;
        if !whole_view
            && !hits
                .iter()
                .any(|hit| hit.old_end <= old_start && hit.new_end <= new_start)
        {
            continue;
        }
        let tokens = &old.group.tokens[old_start..];
        let mut occurrences = [None, None];
        for side in 0..2 {
            occurrences[side] = match search_views(views[side], tokens, remaining)? {
                SearchResult::Complete(summary) => unique_occurrence(summary),
                SearchResult::BudgetExceeded => return Ok(false),
            };
        }
        let [Some(old_end), Some(new_end)] = occurrences else {
            continue;
        };
        if old_end.view_index != old_view
            || old_end.start != old_start
            || new_end.view_index != new_view
            || new_end.start != new_start
        {
            continue;
        }
        hits.try_reserve(1)
            .map_err(|_| super::allocation_error("source ending anchors"))?;
        hits.push(AnchorHit {
            input_index: usize::MAX,
            old_view,
            new_view,
            old_start,
            old_end: old.group.tokens.len(),
            new_start,
            new_end: new.group.tokens.len(),
        });
    }
    Ok(true)
}

fn source_bounded_block(block: &BlockText, remaining: &mut usize) -> bool {
    if !block.issues.is_empty()
        || block
            .normalization_events
            .iter()
            .any(|event| event.kind != NormalizationKind::WhitespaceCollapse)
        || !block.canonical.unmapped.is_empty()
        || !block.line_breaks.as_ref().is_some_and(Vec::is_empty)
        || !block.page_breaks.as_ref().is_some_and(Vec::is_empty)
        || block.pages.len() != 1
        || block.font_size_signatures.is_none()
        || block.position_signatures.is_none()
    {
        return false;
    }
    let scalar_count = block.canonical.text.chars().count();
    let source_count = block
        .canonical
        .source_map
        .iter()
        .fold(0usize, |total, entry| {
            total.saturating_add(entry.source.atoms.len())
        });
    let mut sources = HashSet::new();
    if !charge(remaining, source_count) || sources.try_reserve(source_count).is_err() {
        *remaining = 0;
        return false;
    }
    let mut next_start = 0usize;
    for entry in &block.canonical.source_map {
        if entry.output_range.start != next_start
            || entry
                .output_range
                .end
                .saturating_sub(entry.output_range.start)
                != 1
            || entry.output_range.end > scalar_count
            || entry.source.atoms.is_empty()
            || entry.source.atoms.iter().any(|atom| !sources.insert(atom))
        {
            return false;
        }
        next_start = entry.output_range.end;
    }
    next_start == scalar_count
}

fn descriptor_allows(
    descriptors: Option<&[TrustedRunDescriptor]>,
    run_id: TrustedRunId,
    members: &[RunMember],
) -> bool {
    let Some(descriptors) = descriptors else {
        return true;
    };
    let Some(descriptor) = descriptors
        .iter()
        .find(|descriptor| descriptor.id == run_id)
    else {
        return false;
    };
    if descriptor.block_indices.is_empty() && descriptor.trusted_block_indices.is_empty() {
        return true;
    }
    let members = members
        .iter()
        .map(|member| member.block_index)
        .collect::<BTreeSet<_>>();
    let blocks = descriptor
        .block_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let trusted = descriptor
        .trusted_block_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    members == blocks && members == trusted
}

fn compatible_roles(
    sides: [&Side<'_>; 2],
    spans: [&TextSpan; 2],
    remaining_work: &mut usize,
) -> Result<bool> {
    let old = span_role(sides[0], spans[0], remaining_work)?;
    let new = span_role(sides[1], spans[1], remaining_work)?;
    Ok(matches!((old, new), (Some(old), Some(new)) if old.is_alignment_compatible(new)))
}

fn span_role(
    side: &Side<'_>,
    span: &TextSpan,
    remaining_work: &mut usize,
) -> Result<Option<BlockRole>> {
    if !charge(remaining_work, span.blocks.len()) {
        return Ok(None);
    }
    // A run may contain headers and body text while retaining known internal
    // order. Restrict role compatibility to the projected interval, but keep
    // the complete run visible when searching for competing occurrences.
    let projected = super::project(side, span)?;
    let Some(first) = projected.first() else {
        return Ok(None);
    };
    let role = side.blocks[first.block_index].role;
    Ok(projected
        .iter()
        .all(|interval| side.blocks[interval.block_index].role == role)
        .then_some(role))
}

fn charge_group(remaining_work: &mut usize, group: &GroupText) -> bool {
    let cost = group.tokens.len().saturating_add(group.blocks.len());
    charge(remaining_work, cost)
}

fn anchor_tokens(
    side: &Side<'_>,
    span: &TextSpan,
    remaining_work: &mut usize,
) -> Result<Option<Vec<ComparableToken>>> {
    let mut seen = HashSet::new();
    if span.blocks.is_empty()
        || span
            .blocks
            .iter()
            .any(|block| !side.index.contains_key(block) || !seen.insert(*block))
    {
        return Ok(None);
    }
    if super::span_has_source_issues(side, span, remaining_work)? {
        return Ok(None);
    }
    let group = side.canonical_group(&span.blocks, span.separator);
    let comparable_start = span
        .comparable_range
        .start
        .checked_sub(group.comparable_origin);
    let comparable_end = span
        .comparable_range
        .end
        .checked_sub(group.comparable_origin);
    let canonical_start = span
        .canonical_range
        .start
        .checked_sub(group.canonical_origin);
    let canonical_end = span.canonical_range.end.checked_sub(group.canonical_origin);
    let (Some(comparable_start), Some(comparable_end), Some(canonical_start), Some(canonical_end)) = (
        comparable_start,
        comparable_end,
        canonical_start,
        canonical_end,
    ) else {
        return Ok(None);
    };
    if comparable_start >= comparable_end
        || comparable_end > group.tokens.len()
        || canonical_start != group.scalar_boundaries[comparable_start]
        || canonical_end != group.scalar_boundaries[comparable_end]
    {
        return Ok(None);
    }
    let mut tokens = Vec::new();
    tokens
        .try_reserve_exact(comparable_end - comparable_start)
        .map_err(|_| Error::LimitExceeded {
            resource: "assessment local-domain anchor tokens",
            limit: comparable_end - comparable_start,
        })?;
    tokens.extend_from_slice(&group.tokens[comparable_start..comparable_end]);
    Ok(Some(tokens))
}

fn search_views(
    views: &[View],
    needle: &[ComparableToken],
    remaining_work: &mut usize,
) -> Result<SearchResult> {
    let mut summary = OccurrenceSummary::default();
    if needle.is_empty() {
        return Ok(SearchResult::Complete(summary));
    }
    if !charge(remaining_work, needle.len()) {
        return Ok(SearchResult::BudgetExceeded);
    }
    let mut prefix = Vec::new();
    prefix
        .try_reserve_exact(needle.len())
        .map_err(|_| super::allocation_error("local anchor search prefix"))?;
    prefix.resize(needle.len(), 0);
    for index in 1..needle.len() {
        let Some(matched) = advance_match(
            needle,
            &prefix,
            prefix[index - 1],
            &needle[index],
            remaining_work,
        ) else {
            return Ok(SearchResult::BudgetExceeded);
        };
        prefix[index] = matched;
    }
    for (view_index, view) in views.iter().enumerate() {
        let haystack = &view.group.tokens;
        if needle.len() > haystack.len() {
            continue;
        }
        let mut matched = 0;
        for (index, token) in haystack.iter().enumerate() {
            let Some(next) = advance_match(needle, &prefix, matched, token, remaining_work) else {
                return Ok(SearchResult::BudgetExceeded);
            };
            matched = next;
            if matched == needle.len() {
                let end = index + 1;
                summary.count = summary.count.saturating_add(1);
                summary.first.get_or_insert(Occurrence {
                    view_index,
                    start: end - needle.len(),
                    end,
                });
                if summary.count == 2 {
                    return Ok(SearchResult::Complete(summary));
                }
                matched = prefix[matched - 1];
            }
        }
    }
    Ok(SearchResult::Complete(summary))
}

fn advance_match(
    needle: &[ComparableToken],
    prefix: &[usize],
    mut matched: usize,
    token: &ComparableToken,
    remaining_work: &mut usize,
) -> Option<usize> {
    loop {
        if !charge(remaining_work, 1) {
            return None;
        }
        if needle[matched] == *token {
            return Some(matched + 1);
        }
        if matched == 0 {
            return Some(0);
        }
        matched = prefix[matched - 1];
    }
}

fn unique_occurrence(summary: OccurrenceSummary) -> Option<Occurrence> {
    (summary.count == 1).then_some(summary.first?)
}

fn ordered_chains(
    pairs: BTreeMap<(usize, usize), Vec<AnchorHit>>,
    remaining: &mut usize,
    limit: usize,
) -> Option<Vec<Vec<AnchorHit>>> {
    let mut anchors = Vec::new();
    for mut pair in pairs.into_values() {
        pair.sort_unstable_by_key(|anchor| (anchor.old_start, anchor.new_start, anchor.old_end));
        merge_collinear(&mut pair);
        if anchors.len().saturating_add(pair.len()) > limit {
            *remaining = 0;
            return None;
        }
        anchors.extend(pair);
    }
    let count = anchors.len();
    if !charge(
        remaining,
        count
            .saturating_mul(count.checked_ilog2().unwrap_or(0) as usize + 4)
            .saturating_mul(2),
    ) {
        return None;
    }
    let mut next = [vec![None; count], vec![None; count]];
    for (side, links) in next.iter_mut().enumerate() {
        let mut order = (0..count).collect::<Vec<_>>();
        let coordinates = |index: usize| {
            let a = anchors[index];
            if side == 0 {
                (a.old_view, a.old_start, a.old_end)
            } else {
                (a.new_view, a.new_start, a.new_end)
            }
        };
        order.sort_unstable_by_key(|&index| coordinates(index));
        for pair in order.windows(2) {
            let a = coordinates(pair[0]);
            let b = coordinates(pair[1]);
            if a.0 == b.0 && a.2 <= b.1 {
                links[pair[0]] = Some(pair[1]);
            }
        }
    }
    let mut previous = vec![false; count];
    for (&old_next, &new_next) in next[0].iter().zip(&next[1]) {
        if old_next == new_next
            && let Some(following) = old_next
        {
            previous[following] = true;
        }
    }
    let mut chains = Vec::new();
    for (start, has_previous) in previous.into_iter().enumerate() {
        if has_previous {
            continue;
        }
        let mut chain = vec![anchors[start]];
        let mut index = start;
        while next[0][index] == next[1][index] {
            let Some(following) = next[0][index] else {
                break;
            };
            chain.push(anchors[following]);
            index = following;
        }
        chains.push(chain);
    }
    Some(chains)
}

type OrderedDomain = ((usize, usize, usize, usize), LocalDomain);

fn competing_domains(
    sides: [&Side<'_>; 2],
    domains: &[OrderedDomain],
    remaining: &mut usize,
    limit: usize,
) -> Result<Option<HashSet<usize>>> {
    let mut competing = HashSet::new();
    for (side_index, side) in sides.into_iter().enumerate() {
        let mut intervals = Vec::new();
        for (index, (_, domain)) in domains.iter().enumerate() {
            let span = if side_index == 0 {
                &domain.old_span
            } else {
                &domain.new_span
            };
            if !charge(remaining, span.blocks.len()) {
                return Ok(None);
            }
            let projected = super::project(side, span)?;
            if intervals.len().saturating_add(projected.len()) > limit {
                *remaining = 0;
                return Ok(None);
            }
            intervals.extend(projected.into_iter().map(|interval| (interval, index)));
        }
        if !charge(
            remaining,
            intervals
                .len()
                .saturating_mul(intervals.len().checked_ilog2().unwrap_or(0) as usize + 1),
        ) {
            return Ok(None);
        }
        intervals.sort_unstable_by_key(|(range, index)| {
            (range.block_index, range.start, range.end, *index)
        });
        let mut previous: Option<(super::SourceInterval, usize)> = None;
        for (range, index) in intervals {
            if let Some((prior, owner)) = previous
                && prior.block_index == range.block_index
                && prior.end > range.start
            {
                competing.insert(owner);
                competing.insert(index);
                if prior.end >= range.end {
                    continue;
                }
            }
            previous = Some((range, index));
        }
    }
    Ok(Some(competing))
}

fn close_domain(
    old_views: &[View],
    new_views: &[View],
    anchors: &mut Vec<AnchorHit>,
    remaining: &mut usize,
) -> Option<LocalDomain> {
    anchors.sort_unstable_by_key(|anchor| {
        (
            anchor.old_start,
            anchor.new_start,
            anchor.old_end,
            anchor.new_end,
            anchor.input_index,
        )
    });
    merge_collinear(anchors);
    let first = anchors.first().copied()?;
    let old_view = old_views.get(first.old_view)?;
    let new_view = new_views.get(first.new_view)?;
    if anchors.len() == 1 {
        let anchor = anchors[0];
        if old_view.source_bounded || new_view.source_bounded {
            // A single anchor cannot close the unanchored remainder of a
            // source-bounded view. When the anchor covers the whole view there
            // is no remainder, and the anchor's unique source range already
            // proves the view's equality.
            let signatures = old_view.position_signatures.len();
            let covers_whole_view = anchor.old_start == 0
                && anchor.old_end == old_view.group.tokens.len()
                && anchor.new_start == 0
                && anchor.new_end == new_view.group.tokens.len()
                && old_view.horizontal_text
                && new_view.horizontal_text
                && !old_view.position_signatures.is_empty()
                && old_view.page.is_some()
                && old_view.page == new_view.page
                && (charge(remaining, signatures)
                    && old_view.position_signatures == new_view.position_signatures);
            if !covers_whole_view {
                return None;
            }
        }
        // One independently unique anchor proves only its own equal source
        // range. It cannot close either adjacent gap or the rest of the run;
        // a source-bounded view is admitted only when the anchor is that whole
        // view, so no unanchored remainder exists.
        return Some(LocalDomain {
            old_span: old_view.group.span(anchor.old_start, anchor.old_end),
            new_span: new_view.group.span(anchor.new_start, anchor.new_end),
            source_bounded: old_view.source_bounded && new_view.source_bounded,
        });
    }

    if anchors.windows(2).any(|pair| {
        pair[0].old_end > pair[1].old_start
            || pair[0].new_end > pair[1].new_start
            || pair[0].new_start >= pair[1].new_start
    }) {
        return None;
    }
    let last = anchors.last().copied()?;
    if first.old_start >= last.old_end || first.new_start >= last.new_end {
        return None;
    }
    Some(LocalDomain {
        old_span: old_view.group.span(first.old_start, last.old_end),
        new_span: new_view.group.span(first.new_start, last.new_end),
        source_bounded: old_view.source_bounded && new_view.source_bounded,
    })
}

fn merge_collinear(anchors: &mut Vec<AnchorHit>) {
    let mut retained = 0;
    for index in 0..anchors.len() {
        let next = anchors[index];
        if retained > 0 {
            let previous = &mut anchors[retained - 1];
            if previous.old_view == next.old_view
                && previous.new_view == next.new_view
                && next.old_start >= previous.old_start
                && next.new_start >= previous.new_start
                && next.old_start < previous.old_end
                && next.new_start < previous.new_end
                && next.old_start - previous.old_start == next.new_start - previous.new_start
            {
                previous.old_end = previous.old_end.max(next.old_end);
                previous.new_end = previous.new_end.max(next.new_end);
                continue;
            }
        }
        anchors[retained] = next;
        retained += 1;
    }
    anchors.truncate(retained);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::{BlockId, BlockRole},
        model::{GlyphId, Vec2},
        normalize::{
            FontSizeSignature, MappedText, PositionSignature, ScalarRange, SourceMapEntry,
            TextSource, TextSourceAtom,
        },
    };

    fn discover(
        sides: [&Side<'_>; 2],
        recovery: SentenceRecoveryInput<'_>,
        exact_anchors: &[(TextSpan, TextSpan)],
        remaining_work: &mut usize,
        max_ranges: usize,
    ) -> Result<Vec<LocalDomain>> {
        super::discover(sides, recovery, exact_anchors, remaining_work, max_ranges)
            .map(|discovery| discovery.domains)
    }

    fn mapped(text: &str) -> MappedText {
        MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        }
    }

    fn block(id: u64, text: &str) -> crate::normalize::BlockText {
        let canonical = mapped(text);
        let tokens = text.chars().map(ComparableToken::Scalar).collect();
        crate::normalize::BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: text.to_owned(),
            matching_tokens: tokens,
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

    fn sourced_block(id: u64, text: &str) -> crate::normalize::BlockText {
        let source_map = text
            .chars()
            .enumerate()
            .map(|(index, _)| SourceMapEntry {
                output_range: ScalarRange {
                    start: index,
                    end: index + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(id * 1000 + index as u64 + 1))]
                        .into(),
                },
            })
            .collect::<Vec<_>>();
        let canonical = MappedText {
            text: text.to_owned(),
            source_map,
            unmapped: Vec::new(),
        };
        let tokens = canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens");
        let font_size = FontSizeSignature::new(&[10.0]).expect("valid font size");
        let position = PositionSignature::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid position");
        crate::normalize::BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: text.to_owned(),
            matching_tokens: tokens.clone(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: Some(vec![font_size; tokens.len()]),
            position_signatures: Some(vec![position; tokens.len()]),
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    fn side(blocks: &[crate::normalize::BlockText]) -> Side<'_> {
        super::super::super::SidePlan::inspect("test", blocks)
            .expect("test blocks are valid")
            .materialize()
            .expect("test blocks materialize")
    }

    fn span(blocks: &[u64], start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: blocks.iter().copied().map(BlockId).collect(),
            separator: (blocks.len() > 1).then_some(BlockSeparator::Space),
            canonical_range: ScalarRange { start, end },
            comparable_range: super::super::super::TokenRange { start, end },
        }
    }

    fn recovery<'a>(
        old: &'a [Option<TrustedRunInterval>],
        new: &'a [Option<TrustedRunInterval>],
    ) -> SentenceRecoveryInput<'a> {
        SentenceRecoveryInput {
            old_trusted_run_intervals: old,
            new_trusted_run_intervals: new,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        }
    }

    #[test]
    fn source_bounded_block_accepts_complete_single_line_evidence() {
        assert!(source_bounded_block(
            &sourced_block(1, "source"),
            &mut 10_000
        ));
    }

    #[test]
    fn source_bounded_block_rejects_incomplete_source_evidence() {
        let mut block = sourced_block(1, "source");
        block.canonical.source_map.pop();
        assert!(!source_bounded_block(&block, &mut 10_000));

        let mut block = sourced_block(2, "source");
        block.line_breaks = Some(vec![3]);
        assert!(!source_bounded_block(&block, &mut 10_000));
    }

    #[test]
    fn source_bounded_block_rejects_shared_scalar_sources() {
        let mut block = sourced_block(1, "AfiB");
        block.canonical.source_map[2].source = block.canonical.source_map[1].source.clone();
        assert!(!source_bounded_block(&block, &mut 10_000));

        let mut block = sourced_block(1, "AfiB");
        block.canonical.source_map[1].output_range.end = 3;
        block.canonical.source_map.remove(2);
        assert!(!source_bounded_block(&block, &mut 10_000));
    }

    #[test]
    fn source_bounded_views_need_an_independent_unique_ending() {
        for repeated_ending in [false, true] {
            let mut old_blocks = vec![sourced_block(1, "Unique header alpha FIN.")];
            let mut new_blocks = vec![sourced_block(101, "Unique header beta FIN.")];
            if repeated_ending {
                old_blocks.push(sourced_block(2, "Gamma FIN."));
                new_blocks.push(sourced_block(102, "Gamma FIN."));
            }
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let old_intervals = vec![None; old_blocks.len()];
            let new_intervals = vec![None; new_blocks.len()];
            let mut input = recovery(&old_intervals, &new_intervals);
            input.min_tokens = 12;
            let domains = discover([&old, &new], input, &[], &mut 100_000, 100)
                .expect("bounded source ending search");
            if repeated_ending {
                // The changed line (alpha versus beta) still cannot close: no
                // independent unique ending anchor exists. The identical
                // repeated line is a complete source-bounded view whose
                // tokens, page and exact per-token positions match on both
                // sides, so it closes by the positioned key rather than by
                // text-only uniqueness.
                assert_eq!(domains.len(), 1, "{domains:?}");
                assert_eq!(
                    super::super::span_tokens(&old, &domains[0].old_span).expect("old tokens"),
                    old_blocks[1]
                        .canonical
                        .comparable_tokens()
                        .expect("old source")
                );
                assert!(
                    domains.iter().all(|domain| {
                        super::super::span_tokens(&old, &domain.old_span).expect("old tokens")
                            != old_blocks[0]
                                .canonical
                                .comparable_tokens()
                                .expect("old source")
                    }),
                    "the changed line must not close"
                );
            } else {
                assert_eq!(domains.len(), 1);
                assert_eq!(
                    super::super::span_tokens(&old, &domains[0].old_span).expect("old tokens"),
                    old_blocks[0]
                        .canonical
                        .comparable_tokens()
                        .expect("old source")
                );
                assert_eq!(
                    super::super::span_tokens(&new, &domains[0].new_span).expect("new tokens"),
                    new_blocks[0]
                        .canonical
                        .comparable_tokens()
                        .expect("new source")
                );
            }
        }
    }

    fn positioned_block(
        id: u64,
        text: &str,
        x: f64,
        y: f64,
        page: u32,
    ) -> crate::normalize::BlockText {
        let mut block = sourced_block(id, text);
        let position =
            PositionSignature::new(Vec2 { x, y }, Vec2 { x: 1.0, y: 0.0 }).expect("valid position");
        let tokens = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        block.position_signatures = Some(vec![position; tokens]);
        block.pages = vec![page];
        block
    }

    /// A source-backed block whose tokens advance along x from a start
    /// position, so its baseline geometry is an interval instead of a point.
    fn spread_block(
        id: u64,
        text: &str,
        x: f64,
        y: f64,
        page: u32,
        advance: f64,
    ) -> crate::normalize::BlockText {
        let mut block = sourced_block(id, text);
        let tokens = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        let signatures = (0..tokens)
            .map(|index| {
                PositionSignature::new(
                    Vec2 {
                        x: x + index as f64 * advance,
                        y,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("valid position")
            })
            .collect::<Vec<_>>();
        block.position_signatures = Some(signatures);
        block.pages = vec![page];
        block
    }

    fn whole_view_positioned_domain(
        domains: &[LocalDomain],
        old_blocks: &[crate::normalize::BlockText],
        new_blocks: &[crate::normalize::BlockText],
        old_index: usize,
        new_index: usize,
    ) -> bool {
        let old_len = old_blocks[old_index]
            .canonical
            .comparable_tokens()
            .expect("old tokens")
            .len();
        let new_len = new_blocks[new_index]
            .canonical
            .comparable_tokens()
            .expect("new tokens")
            .len();
        domains.iter().any(|domain| {
            domain.source_bounded
                && domain.old_span.blocks == [old_blocks[old_index].block]
                && domain.new_span.blocks == [new_blocks[new_index].block]
                && domain.old_span.comparable_range.start == 0
                && domain.old_span.comparable_range.end == old_len
                && domain.new_span.comparable_range.start == 0
                && domain.new_span.comparable_range.end == new_len
        })
    }

    fn positioned_view(
        text: &str,
        positions: Vec<Option<f64>>,
        pages: Vec<Option<u32>>,
        candidate: bool,
    ) -> View {
        let tokens = text
            .chars()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let len = tokens.len();
        let group = GroupText::try_new(vec![BlockId(1)], None, tokens, None, None, None, None)
            .expect("fixture group");
        View {
            kind: ViewKind::Untrusted(0),
            source_order: 0,
            block_indices: vec![0],
            group,
            source_bounded: true,
            horizontal_text: true,
            position_signatures: Vec::new(),
            page: None,
            block_ranges: std::iter::once(0..len).collect(),
            block_candidates: vec![candidate],
            token_positions: positions
                .into_iter()
                .map(|position| {
                    position.map(|x| {
                        PositionSignature::new(Vec2 { x, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 })
                            .expect("valid position")
                    })
                })
                .collect(),
            token_pages: pages,
        }
    }

    #[test]
    fn positioned_occurrences_separates_definitive_mismatches_from_unknowns() -> Result<()> {
        let needle = positioned_view(
            "ABC",
            vec![Some(0.0), Some(1.0), Some(2.0)],
            vec![Some(0), Some(0), Some(0)],
            true,
        );
        // A proven different position must not be vetoed by a later offset
        // without metadata.
        let views = [positioned_view(
            "ABC",
            vec![Some(10.0), None, Some(12.0)],
            vec![Some(0), None, Some(0)],
            false,
        )];
        let mut budget = 100_000;
        let result = positioned_occurrences(&views, &needle, &(0..3), usize::MAX, &mut budget)?
            .expect("the search completes");
        assert_eq!(
            result.same, 0,
            "a proven different position does not compete"
        );
        assert!(
            !result.unknown,
            "a definitive mismatch must not become unknown through later missing metadata"
        );
        // Missing metadata before the mismatch must not hide the mismatch.
        let views = [positioned_view(
            "ABC",
            vec![None, Some(11.0), Some(12.0)],
            vec![None, Some(0), Some(0)],
            false,
        )];
        let mut budget = 100_000;
        let result = positioned_occurrences(&views, &needle, &(0..3), usize::MAX, &mut budget)?
            .expect("the search completes");
        assert_eq!(result.same, 0);
        assert!(
            !result.unknown,
            "a later definitive mismatch must still be found"
        );
        // An occurrence that is only unverifiable still vetoes.
        let views = [positioned_view(
            "ABC",
            vec![None, Some(1.0), Some(2.0)],
            vec![None, Some(0), Some(0)],
            false,
        )];
        let mut budget = 100_000;
        let result = positioned_occurrences(&views, &needle, &(0..3), usize::MAX, &mut budget)?
            .expect("the search completes");
        assert!(result.unknown, "unverifiable metadata still vetoes");
        // A complete same-position duplicate still competes.
        let views = [positioned_view(
            "ABC",
            vec![Some(0.0), Some(1.0), Some(2.0)],
            vec![Some(0), Some(0), Some(0)],
            false,
        )];
        let mut budget = 100_000;
        let result = positioned_occurrences(&views, &needle, &(0..3), usize::MAX, &mut budget)?
            .expect("the search completes");
        assert_eq!(result.same, 1);
        assert!(!result.unknown);
        // An exhausted budget yields no partial proof.
        let mut budget = 0;
        assert!(
            positioned_occurrences(&views, &needle, &(0..3), usize::MAX, &mut budget)?.is_none()
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_closes_a_line_repeated_inside_a_longer_view() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Self-Employment Tax", 10.0, 700.0, 0),
            positioned_block(2, "Total Self-Employment Tax and credits", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Self-Employment Tax", 10.0, 700.0, 0),
            positioned_block(102, "Total Self-Employment Tax and credits", 10.0, 680.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = vec![None; 2];
        let mut input = recovery(&intervals, &intervals);
        input.min_tokens = 12;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "the repeated short line must close by its exact positioned key: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_rejects_repeated_and_one_sided_keys() -> Result<()> {
        let text = "Repeated positioned line";
        for (old_count, new_count) in [(2usize, 1usize), (1, 2)] {
            let mut old_blocks = vec![positioned_block(1, text, 10.0, 700.0, 0)];
            let mut new_blocks = vec![positioned_block(101, text, 10.0, 700.0, 0)];
            for index in 1..old_count {
                old_blocks.push(positioned_block(index as u64 + 1, text, 10.0, 700.0, 0));
            }
            for index in 1..new_count {
                new_blocks.push(positioned_block(index as u64 + 101, text, 10.0, 700.0, 0));
            }
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let intervals = vec![None; old_blocks.len()];
            let new_intervals = vec![None; new_blocks.len()];
            let input = recovery(&intervals, &new_intervals);
            let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
            assert!(
                !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
                "a repeated or one-sided positioned key must not close: {domains:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn positioned_equality_rejects_swaps_pages_moves_and_token_mismatches() -> Result<()> {
        let cases: [(
            &str,
            Vec<crate::normalize::BlockText>,
            Vec<crate::normalize::BlockText>,
        ); 4] = [
            (
                "position swap",
                vec![
                    positioned_block(1, "Alpha line", 10.0, 700.0, 0),
                    positioned_block(2, "Beta line", 10.0, 680.0, 0),
                ],
                vec![
                    positioned_block(101, "Alpha line", 10.0, 680.0, 0),
                    positioned_block(102, "Beta line", 10.0, 700.0, 0),
                ],
            ),
            (
                "cross page",
                vec![positioned_block(1, "Alpha line", 10.0, 700.0, 0)],
                vec![positioned_block(101, "Alpha line", 10.0, 700.0, 1)],
            ),
            (
                "moved line",
                vec![positioned_block(1, "Alpha line", 10.0, 700.0, 0)],
                vec![positioned_block(101, "Alpha line", 10.0, 680.0, 0)],
            ),
            (
                "same position different tokens",
                vec![positioned_block(1, "Alpha line", 10.0, 700.0, 0)],
                vec![positioned_block(101, "Beta line", 10.0, 700.0, 0)],
            ),
        ];
        for (name, old_blocks, new_blocks) in cases {
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let intervals = vec![None; old_blocks.len()];
            let new_intervals = vec![None; new_blocks.len()];
            let input = recovery(&intervals, &new_intervals);
            let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
            assert!(
                !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
                "{name}: {domains:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn positioned_equality_rejects_non_horizontal_and_incomplete_evidence() -> Result<()> {
        // A vertical direction is not eligible even with matching tokens.
        let mut vertical = positioned_block(1, "Vertical line", 10.0, 700.0, 0);
        let direction = PositionSignature::new(Vec2 { x: 10.0, y: 700.0 }, Vec2 { x: 0.0, y: 1.0 })
            .expect("valid position");
        let tokens = vertical
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        vertical.position_signatures = Some(vec![direction; tokens]);
        let mut vertical_new = positioned_block(101, "Vertical line", 10.0, 700.0, 0);
        vertical_new.position_signatures = Some(vec![direction; tokens]);
        let old_blocks = [vertical];
        let new_blocks = [vertical_new];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = vec![None; 1];
        let input = recovery(&intervals, &intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "vertical text must not close: {domains:?}"
        );

        // An incomplete source map is not source-bounded.
        let mut incomplete = positioned_block(1, "Alpha line", 10.0, 700.0, 0);
        incomplete.canonical.source_map.pop();
        let complete = positioned_block(101, "Alpha line", 10.0, 700.0, 0);
        let old_blocks = [incomplete];
        let new_blocks = [complete];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = vec![None; 1];
        let input = recovery(&intervals, &intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "incomplete source evidence must not close: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_never_adopts_a_substring_occurrence() -> Result<()> {
        // A short line whose tokens are a prefix of a longer line at the same
        // position must not expand the longer view into a whole-view equality.
        let old_blocks = [positioned_block(1, "Alpha", 10.0, 700.0, 0)];
        let new_blocks = [positioned_block(101, "Alpha extra", 10.0, 700.0, 0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None];
        let mut input = recovery(&intervals, &intervals);
        input.min_tokens = 12;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a substring occurrence must not close a whole view: {domains:?}"
        );

        // Two short lines whose concatenation is one longer line with aligned
        // positions: both partial occurrences compete and neither closes.
        let old_blocks = [
            positioned_block(1, "Alpha", 10.0, 700.0, 0),
            positioned_block(2, "Beta", 10.0, 680.0, 0),
        ];
        let new_blocks = [positioned_block(101, "Alpha Beta", 10.0, 700.0, 0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let new_intervals = [None];
        let input = recovery(&intervals, &new_intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        // Exact anchor sub-ranges may close, but neither short line may adopt
        // the longer view as a whole-view positioned equality.
        assert!(
            !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0)
                && !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 1, 0),
            "concatenated short lines must not close against one long view: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn projected_overlap_compares_source_ranges_across_block_shapes() -> Result<()> {
        let blocks = [sourced_block(1, "Alpha"), sourced_block(2, "")];
        let side = side(&blocks);
        let single = side.canonical_group(&[BlockId(1)], None).full_span();
        let multi = side
            .canonical_group(&[BlockId(1), BlockId(2)], Some(BlockSeparator::Space))
            .full_span();
        let mut budget = 100_000;
        let single_projection = project_span(&side, &single, &mut budget)?.expect("projection");
        let multi_projection = project_span(&side, &multi, &mut budget)?.expect("projection");
        assert!(
            projected_overlap(&single_projection, &multi_projection),
            "different block shapes over the same source range must overlap: \
             {single_projection:?} {multi_projection:?}"
        );
        Ok(())
    }

    #[test]
    fn view_token_metadata_matches_group_tokens_around_empty_blocks() -> Result<()> {
        let blocks = [
            sourced_block(1, ""),
            positioned_block(2, "Alpha ", 10.0, 700.0, 0),
            sourced_block(3, ""),
            positioned_block(4, "Beta", 20.0, 680.0, 0),
            sourced_block(5, ""),
        ];
        let source = side(&blocks);
        let ids = [BlockId(1), BlockId(2), BlockId(3), BlockId(4), BlockId(5)];
        for separator in [BlockSeparator::Space, BlockSeparator::Concatenate] {
            let group = source.canonical_group(&ids, Some(separator));
            let bounded = vec![true; blocks.len()];
            let (positions, pages, block_ranges) = view_token_metadata(
                &source,
                &[0, 1, 2, 3, 4],
                &bounded,
                Some(separator),
                &mut 100_000,
            )
            .expect("metadata fits its budget");
            assert_eq!(block_ranges.len(), blocks.len(), "{separator:?}");
            assert_eq!(positions.len(), group.tokens.len(), "{separator:?}");
            assert_eq!(pages.len(), group.tokens.len(), "{separator:?}");
            for (block_index, block) in blocks.iter().enumerate() {
                let tokens = block
                    .canonical
                    .comparable_tokens()
                    .expect("source-backed fixture tokens");
                if tokens.is_empty() {
                    continue;
                }
                let offset = group
                    .tokens
                    .windows(tokens.len())
                    .position(|window| window == tokens.as_slice())
                    .unwrap_or_else(|| panic!("{separator:?} block {block_index} is in the group"));
                for (index, signature) in block
                    .position_signatures
                    .as_deref()
                    .expect("fixture signatures")
                    .iter()
                    .enumerate()
                {
                    assert_eq!(
                        positions[offset + index],
                        Some(*signature),
                        "{separator:?} block {block_index} token {index}"
                    );
                    assert_eq!(
                        pages[offset + index],
                        Some(block.pages[0]),
                        "{separator:?} block {block_index} token {index}"
                    );
                }
            }
        }
        // A trailing space followed by an empty block must not add a separator
        // after the empty block: the state update mirrors `project`.
        let blocks = [
            positioned_block(1, "Alpha ", 10.0, 700.0, 0),
            sourced_block(2, ""),
            positioned_block(3, "Beta", 20.0, 680.0, 0),
        ];
        let source = side(&blocks);
        let ids = [BlockId(1), BlockId(2), BlockId(3)];
        let group = source.canonical_group(&ids, Some(BlockSeparator::Space));
        let bounded = vec![true; blocks.len()];
        let (positions, pages, block_ranges) = view_token_metadata(
            &source,
            &[0, 1, 2],
            &bounded,
            Some(BlockSeparator::Space),
            &mut 100_000,
        )
        .expect("metadata fits its budget");
        assert_eq!(block_ranges.len(), blocks.len());
        assert_eq!(positions.len(), group.tokens.len());
        assert_eq!(pages.len(), group.tokens.len());
        assert_eq!(
            group.tokens,
            "Alpha Beta"
                .chars()
                .map(ComparableToken::Scalar)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    fn block_positioned_domain(
        domains: &[LocalDomain],
        old: &super::super::Side<'_>,
        new: &super::super::Side<'_>,
        old_block: BlockId,
        new_block: BlockId,
        len: usize,
    ) -> Result<bool> {
        for domain in domains {
            if !domain.source_bounded {
                continue;
            }
            let old_intervals = super::super::project(old, &domain.old_span)?;
            let new_intervals = super::super::project(new, &domain.new_span)?;
            if old_intervals
                == [SourceInterval {
                    block_index: old.index[&old_block],
                    start: 0,
                    end: len,
                }]
                && new_intervals
                    == [SourceInterval {
                        block_index: new.index[&new_block],
                        start: 0,
                        end: len,
                    }]
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[test]
    fn positioned_equality_requires_both_whole_views_without_an_equal_mapping() -> Result<()> {
        // Old is one whole line; new is a trusted run of that line plus a
        // tail. The leading line matches by position, but the new side is a
        // member of a longer view and no equal mapping ties the blocks, so
        // neither direction may close.
        let line_blocks = [positioned_block(1, "Short line", 10.0, 700.0, 0)];
        let run_blocks = [
            positioned_block(101, "Short line", 10.0, 700.0, 0),
            positioned_block(102, "Tail line", 10.0, 680.0, 0),
        ];
        let old = side(&line_blocks);
        let new = side(&run_blocks);
        let old_intervals = [None];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2)];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 12;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(1), BlockId(101), 10)?,
            "a member of a longer new view must not close without an equal mapping: {domains:?}"
        );

        // Reverse: old is the run, new is the single line.
        let old = side(&run_blocks);
        let new = side(&line_blocks);
        let old_intervals = [interval(2, 0, 1), interval(2, 1, 2)];
        let new_intervals = [None];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 12;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(101), BlockId(1), 10)?,
            "a member of a longer old view must not close without an equal mapping: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_recovers_a_complete_block_inside_an_equal_run_domain() -> Result<()> {
        // Unique head and tail anchors chain across the repeated middle, so
        // the run is one proven equal domain and each repeated middle block is
        // a complete original block at the same offset on both sides.
        let old_blocks = [
            positioned_block(1, "Unique head line", 10.0, 760.0, 0),
            positioned_block(2, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(3, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(4, "Unique tail line", 10.0, 700.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Unique head line", 10.0, 760.0, 0),
            positioned_block(102, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(103, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(104, "Unique tail line", 10.0, 700.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 3;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 18)?
                && block_positioned_domain(&domains, &old, &new, BlockId(3), BlockId(103), 18)?,
            "complete blocks inside an equal run domain must close: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_holds_residue_boundaries() -> Result<()> {
        // The repeated middle blocks swap positions: the run is still equal,
        // but the matching occurrence sits at a different offset, so the
        // residue is not the same mapping.
        let old_blocks = [
            positioned_block(1, "Unique head line", 10.0, 760.0, 0),
            positioned_block(2, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(3, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(4, "Unique tail line", 10.0, 700.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Unique head line", 10.0, 760.0, 0),
            positioned_block(102, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(103, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(104, "Unique tail line", 10.0, 700.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 3;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 18)?
                && !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(103), 18)?
                && !block_positioned_domain(&domains, &old, &new, BlockId(3), BlockId(102), 18)?
                && !block_positioned_domain(&domains, &old, &new, BlockId(3), BlockId(103), 18)?,
            "a shifted correspondence must not close: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_holds_residue_duplicates_and_missing_metadata() -> Result<()> {
        // A same-position duplicate inside the equal run vetoes the residue.
        let duplicate_old = [
            positioned_block(1, "Unique head line", 10.0, 760.0, 0),
            positioned_block(2, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(3, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(4, "Unique tail line", 10.0, 700.0, 0),
        ];
        let duplicate_new = [
            positioned_block(101, "Unique head line", 10.0, 760.0, 0),
            positioned_block(102, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(103, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(104, "Unique tail line", 10.0, 700.0, 0),
        ];
        let old = side(&duplicate_old);
        let new = side(&duplicate_new);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 3;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 18)?,
            "a same-position duplicate must veto the residue: {domains:?}"
        );

        // One member lacks position evidence, so its occurrence cannot be
        // compared and vetoes the residue instead of being ignored.
        let mut incomplete = positioned_block(2, "Repeat middle line", 10.0, 740.0, 0);
        incomplete.position_signatures = None;
        let old_blocks = [
            positioned_block(1, "Unique head line", 10.0, 760.0, 0),
            incomplete,
            positioned_block(3, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(4, "Unique tail line", 10.0, 700.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Unique head line", 10.0, 760.0, 0),
            positioned_block(102, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(103, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(104, "Unique tail line", 10.0, 700.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let input = recovery(&old_intervals, &new_intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(3), BlockId(103), 18)?,
            "missing metadata must veto the residue: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_closes_a_complete_block_in_a_single_block_trusted_run() -> Result<()> {
        // The trusted member's text also occurs at another position, so no
        // content-unique anchor closes it; its own exact positioned key is the
        // one-to-one proof. The other occurrence differs in position and does
        // not veto.
        let old_blocks = [
            positioned_block(1, "Repeated member line", 10.0, 700.0, 0),
            positioned_block(2, "Repeated member line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Repeated member line", 10.0, 700.0, 0),
            positioned_block(102, "Repeated member line", 10.0, 680.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), None];
        let new_intervals = [interval(2, 0, 1), None];
        let input = recovery(&old_intervals, &new_intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "a complete block inside a single-block trusted run must close: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_holds_single_block_trusted_run_boundaries() -> Result<()> {
        // A moved position, different tokens and a same-position duplicate
        // from another source all keep the trusted member unresolved.
        let cases: [(
            &str,
            Vec<crate::normalize::BlockText>,
            Vec<crate::normalize::BlockText>,
        ); 3] = [
            (
                "moved",
                vec![positioned_block(1, "Trusted member line", 10.0, 700.0, 0)],
                vec![positioned_block(101, "Trusted member line", 10.0, 680.0, 0)],
            ),
            (
                "different tokens",
                vec![positioned_block(1, "Trusted member line", 10.0, 700.0, 0)],
                vec![positioned_block(101, "Other member line", 10.0, 700.0, 0)],
            ),
            (
                "same position duplicate source",
                vec![
                    positioned_block(1, "Trusted member line", 10.0, 700.0, 0),
                    positioned_block(2, "Trusted member line", 10.0, 700.0, 0),
                ],
                vec![positioned_block(101, "Trusted member line", 10.0, 700.0, 0)],
            ),
        ];
        for (name, old_blocks, new_blocks) in cases {
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let old_intervals = vec![interval(1, 0, 1); old_blocks.len()];
            let new_intervals = vec![interval(2, 0, 1); new_blocks.len()];
            let input = recovery(&old_intervals, &new_intervals);
            let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
            assert!(
                !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
                "{name}: {domains:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn positioned_equality_never_adopts_a_member_of_a_longer_trusted_run() -> Result<()> {
        // A multi-block trusted run is a longer view: one of its members may
        // not be treated as a whole block, even with matching positions.
        let old_blocks = [
            positioned_block(1, "Trusted member line", 10.0, 700.0, 0),
            positioned_block(2, "Trusted tail line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Trusted member line", 10.0, 700.0, 0),
            positioned_block(102, "Trusted tail line", 10.0, 680.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2)];
        let input = recovery(&old_intervals, &new_intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "a member of a longer trusted run must not close as a whole block: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_vetoes_a_same_position_duplicate_inside_a_trusted_view() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Self-Employment Tax", 10.0, 700.0, 0),
            positioned_block(2, "Self-Employment Tax", 10.0, 700.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Self-Employment Tax", 10.0, 700.0, 0),
            positioned_block(102, "Self-Employment Tax", 10.0, 700.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        // The duplicate lives inside a complete single-block trusted run: it
        // cannot close a domain itself, but it still competes by exact
        // position, so the eligible line must not close.
        let old_intervals = [None, interval(1, 0, 1)];
        let new_intervals = [None, interval(2, 0, 1)];
        let input = recovery(&old_intervals, &new_intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "a same-position duplicate inside a trusted view must veto: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_vetoes_an_unknown_metadata_duplicate() -> Result<()> {
        let mut duplicate = positioned_block(2, "Self-Employment Tax", 10.0, 700.0, 0);
        duplicate.position_signatures = None;
        let old_blocks = [
            positioned_block(1, "Self-Employment Tax", 10.0, 700.0, 0),
            duplicate.clone(),
        ];
        let new_blocks = [
            positioned_block(101, "Self-Employment Tax", 10.0, 700.0, 0),
            duplicate,
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            !whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "a duplicate without comparable metadata must veto: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_pass_exhaustion_keeps_completed_anchor_domains() -> Result<()> {
        // Unique head and tail anchors chain across the repeated middle, so
        // the run closes as one source-bounded false anchor domain and the
        // middle blocks are residue candidates. A positive budget one unit
        // short must keep the anchor domain and commit none of the pending
        // residue additions.
        let old_blocks = [
            positioned_block(1, "Unique head line", 10.0, 760.0, 0),
            positioned_block(2, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(3, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(4, "Unique tail line", 10.0, 700.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Unique head line", 10.0, 760.0, 0),
            positioned_block(102, "Repeat middle line", 10.0, 740.0, 0),
            positioned_block(103, "Repeat middle line", 10.0, 720.0, 0),
            positioned_block(104, "Unique tail line", 10.0, 700.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 3;
        let mut full_budget = usize::MAX;
        let full = discover([&old, &new], input, &[], &mut full_budget, 100)?;
        let used = usize::MAX - full_budget;
        assert!(used > 1);
        assert!(
            full.iter().any(|domain| !domain.source_bounded)
                && full.iter().any(|domain| domain.source_bounded),
            "the fixture must produce an anchor domain and residue additions: {full:?}"
        );
        // The wide anchor domain (comparable range 9..57 over all four
        // blocks) is the equal domain that makes the middle blocks residue
        // candidates; it must survive the exhausted pass unchanged.
        let anchor = full
            .iter()
            .find(|domain| {
                !domain.source_bounded
                    && domain.old_span.blocks == [BlockId(1), BlockId(2), BlockId(3), BlockId(4)]
                    && domain.old_span.comparable_range
                        == crate::diff::TokenRange { start: 9, end: 57 }
            })
            .expect("the fixture must close its wide anchor domain");
        let mut budget = used - 1;
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 3;
        let retained = discover([&old, &new], input, &[], &mut budget, 100)?;
        assert_eq!(budget, 0);
        assert!(
            !retained.is_empty(),
            "an exhausted positioned pass must keep the completed anchor domain: {retained:?}"
        );
        assert!(
            retained.iter().all(|domain| !domain.source_bounded),
            "an exhausted positioned pass must not commit pending additions: {retained:?}"
        );
        assert!(
            retained.len() < full.len(),
            "the exhausted run must drop every pending addition: {retained:?}"
        );
        assert!(
            retained.contains(anchor),
            "an exhausted positioned pass must keep the completed anchor domain: {retained:?}"
        );
        Ok(())
    }

    #[test]
    fn positioned_equality_respects_the_work_budget() -> Result<()> {
        let old_blocks = [positioned_block(1, "Budgeted line", 10.0, 700.0, 0)];
        let new_blocks = [positioned_block(101, "Budgeted line", 10.0, 700.0, 0)];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = vec![None; 1];
        let input = recovery(&intervals, &intervals);
        let mut budget = 0;
        let domains = discover([&old, &new], input, &[], &mut budget, 100)?;
        assert!(domains.is_empty());
        assert_eq!(budget, 0);
        Ok(())
    }

    #[test]
    fn anchored_translation_closes_a_singleton_with_an_established_neighbour() -> Result<()> {
        // The candidate is an untrusted singleton whose whole content is equal
        // and whose every token moved by one raw translation. The neighbour
        // correspondence is already established and carries the same
        // translation, so the move is proven by the neighbour relation and not
        // by proximity or tolerance.
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 1, 1),
            "an established neighbour with the same raw translation must close the singleton: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_requires_the_same_anchor_on_both_sides() -> Result<()> {
        // The old-adjacent anchor supplies the key but is not adjacent on the
        // new side; a different established block is new-adjacent with another
        // translation. Stitching the two would close a move that no single
        // correspondence supports, so the proof must be withheld.
        let old_blocks = [
            positioned_block(1, "Upper anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
            positioned_block(3, "Middle anchor line", 10.0, 660.0, 0),
            positioned_block(4, "Lower anchor line", 10.0, 640.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Upper anchor line", 10.0, 699.5, 0),
            positioned_block(104, "Middle anchor line", 10.0, 690.0, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
            positioned_block(103, "Lower anchor line", 10.0, 659.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "two different anchors must not be stitched into one proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_closes_a_whole_block_supported_by_a_stationary_neighbour() -> Result<()> {
        // Both sides group three blocks into one trusted run. The middle block
        // sits still: its token positions are identical on both sides and its
        // member range covers the whole original block. The head block is an
        // independently established correspondence that is stationary and
        // adjacent to the candidate on both sides. The tail filler differs
        // between the sides, so the closure cannot rely on a view-wide
        // equality. An established reference on another page must be skipped
        // rather than held.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
            spread_block(4, "Other page line", 10.0, 500.0, 1, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler tail line", 10.0, 660.0, 0, 1.5),
            spread_block(104, "Other page line", 10.0, 500.0, 1, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            None,
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            None,
        ];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
        ];

        let moved = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            moved.is_empty(),
            "the non-zero discovery must not close a stationary member: {moved:?}"
        );

        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert_eq!(
            domains.len(),
            1,
            "only the supported stationary member may close: {domains:?}"
        );
        assert!(
            block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "the stationary member must close as one whole original block: {domains:?}"
        );

        // The same fixture proves the symmetric direction: swap the sides and
        // their trusted runs; the candidate is now the old-side member.
        let swapped = discover_stationary_members(
            [&new, &old],
            recovery(&new_intervals, &old_intervals),
            &[EstablishedBlock {
                old_block: BlockId(101),
                new_block: BlockId(1),
            }],
            &mut 100_000,
            100,
        )?;
        assert_eq!(
            swapped.len(),
            1,
            "the swap must close symmetrically: {swapped:?}"
        );
        assert!(
            block_positioned_domain(&swapped, &new, &old, BlockId(102), BlockId(2), 22)?,
            "the swapped candidate must close as one whole original block: {swapped:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_closes_a_whole_view_block_against_a_run_member() -> Result<()> {
        // The old side keeps both blocks in plain untrusted views; only the
        // new side groups them into a trusted run with an extra filler. The
        // still candidate is a whole original block on both sides and the
        // established head anchor is adjacent to it on both sides, so the
        // run membership on one side must not block the proof.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [None, None];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert_eq!(
            domains.len(),
            1,
            "the whole-view candidate must close against the run member: {domains:?}"
        );
        assert!(
            block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "the candidate must close as one whole original block: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_when_trusted_run_order_is_reversed() -> Result<()> {
        // Candidate and reference are members of the same trusted run on both
        // sides, but their order inside the run is reversed: the candidate is
        // before the reference on the old side and after it on the new side.
        // The run's order contract is therefore violated and the member must
        // be held.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Stationary reference line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(103, "Stationary reference line", 10.0, 660.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a reversed trusted-run order must hold the member: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_when_the_support_is_not_adjacent_on_one_side() -> Result<()> {
        // The support is index-adjacent to the candidate on the old side only.
        // On the new side a different source block sits in the same band
        // between them, so neither the index adjacency nor the band nearest
        // relation finds the support next to the candidate.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(104, "Middle filler line", 10.0, 690.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a support that is not adjacent on one side must not close the member: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_rejects_the_candidates_own_correspondence() -> Result<()> {
        // The only established entry is the candidate's own pair. A
        // correspondence cannot prove its own stillness, so nothing closes.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(2),
            new_block: BlockId(102),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "the candidate must not support itself: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_when_the_candidate_direction_is_reversed() -> Result<()> {
        // The new-side candidate keeps the same start point and token count
        // but advances in the opposite direction, so its per-token
        // displacement is not one uniform zero translation.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let mut new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let tokens = new_blocks[1]
            .canonical
            .comparable_tokens()
            .expect("fixture tokens")
            .len();
        new_blocks[1].position_signatures = Some(
            (0..tokens)
                .map(|index| {
                    PositionSignature::new(
                        Vec2 {
                            x: 10.0 - index as f64 * 1.5,
                            y: 680.0,
                        },
                        Vec2 { x: 1.0, y: 0.0 },
                    )
                    .expect("valid position")
                })
                .collect(),
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a reversed candidate direction must not close as stationary: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_mask_only_removes_candidates() -> Result<()> {
        // Masking the candidate itself must remove it, while masking the
        // established support block must change nothing: the mask never
        // shrinks the reference set.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler tail line", 10.0, 660.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let masked_candidate = [vec![true, false, true], vec![true, false, true]];
        let domains = discover_stationary_members_masked(
            [&old, &new],
            input,
            &established,
            &mut 100_000,
            100,
            &masked_candidate,
        )?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a masked candidate must not close: {domains:?}"
        );
        let masked_support = [vec![false, true, true], vec![false, true, true]];
        let domains = discover_stationary_members_masked(
            [&old, &new],
            input,
            &established,
            &mut 100_000,
            100,
            &masked_support,
        )?;
        assert!(
            block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a masked support must still carry the reference: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_mask_keeps_masked_occurrences_competing() -> Result<()> {
        // The duplicated still position stays a competitor even when its
        // block is masked: the mask only removes candidacy.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
            spread_block(104, "Stationary member line", 10.0, 680.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let mask = [vec![true, true, true], vec![true, true, true, false]];
        let domains = discover_stationary_members_masked(
            [&old, &new],
            input,
            &established,
            &mut 100_000,
            100,
            &mask,
        )?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a masked duplicate must still compete: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_requires_an_independent_stationary_support() -> Result<()> {
        // Same trusted runs as the positive fixture, but nothing is
        // established: no correspondence proves the stillness, so neither
        // discovery may close anything.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let domains = discover_stationary_members([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an unsupported stationary member must not close: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_requires_stillness_in_the_candidate_itself() -> Result<()> {
        // The candidate's whole block moved by one raw unit on the new side
        // while its supporting neighbour stays still. The stillness key is
        // absent, so the stationary discovery must not close it.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 681.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a moved candidate must not close as a stationary member: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_when_the_candidate_metadata_is_unknown() -> Result<()> {
        // The new-side candidate keeps its source evidence but loses every
        // position signature. Its geometry cannot be verified, so the
        // candidate is held instead of being closed on the support alone.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let mut new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        new_blocks[1].position_signatures = None;
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "unknown candidate geometry must hold the member: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_when_the_same_position_is_duplicated() -> Result<()> {
        // A second new-side block repeats the candidate's whole text at the
        // identical position, so the still candidate has two competing
        // occurrences and cannot be pinned to one block.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
            spread_block(104, "Stationary member line", 10.0, 680.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a duplicated still position must not close one arbitrary copy: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_skips_an_other_page_support() -> Result<()> {
        // The established anchor repeats the candidate's neighbour text and
        // geometry on another page. It cannot support a same-page stillness,
        // so the candidate stays open.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 1, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an other-page anchor must not support the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_on_an_unknown_page_support() -> Result<()> {
        // The established anchor keeps its positions but loses its page. An
        // unknown page is not silently treated as a matching page, so the
        // candidate is held.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        let mut new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower filler line", 10.0, 660.0, 0, 1.5),
        ];
        new_blocks[0].pages = Vec::new();
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an unknown-page anchor must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_holds_when_an_established_reference_crosses_the_candidate() -> Result<()> {
        // Candidate and support both sit still. A third established
        // correspondence moves from below the candidate to above it, so the
        // candidate crosses an already decided reference and must be held.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Moving reference line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Moving reference line", 10.0, 720.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            !block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a crossed moving reference must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_closes_when_a_moving_reference_stays_on_its_side() -> Result<()> {
        // The same fixture, but the moving reference stays below the
        // candidate on both sides, so the established order relation is kept
        // and the candidate closes.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Moving reference line", 10.0, 660.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Moving reference line", 10.0, 640.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            block_positioned_domain(&domains, &old, &new, BlockId(2), BlockId(102), 22)?,
            "a moving reference on its own side must not block the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn stationary_member_never_emits_a_partial_domain_on_budget_exhaustion() -> Result<()> {
        // Two independently supported stationary candidates. The smallest
        // budget that closes both is found first; one unit below it the pass
        // must return nothing at all instead of the candidate that happened
        // to be found first. The cut phase is not asserted beyond this
        // observation.
        let old_blocks = [
            spread_block(1, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(2, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(3, "Lower anchor line", 10.0, 620.0, 0, 1.5),
            spread_block(4, "Second member line", 10.0, 600.0, 0, 1.5),
        ];
        let new_blocks = [
            spread_block(101, "Upper anchor line", 10.0, 700.0, 0, 1.5),
            spread_block(102, "Stationary member line", 10.0, 680.0, 0, 1.5),
            spread_block(103, "Lower anchor line", 10.0, 620.0, 0, 1.5),
            spread_block(104, "Second member line", 10.0, 600.0, 0, 1.5),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let mut low = 0;
        let mut high = 100_000;
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            let mut remaining = mid;
            let domains = discover_stationary_members(
                [&old, &new],
                input,
                &established,
                &mut remaining,
                100,
            )?;
            if domains.len() == 2 {
                high = mid;
            } else {
                low = mid;
            }
        }
        let required = high;
        assert!(required > 0, "the fixture cannot be free");
        let mut below = required - 1;
        let domains =
            discover_stationary_members([&old, &new], input, &established, &mut below, 100)?;
        assert!(
            domains.is_empty(),
            "exhausting the budget below {required} must not emit a partial domain: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_an_established_line_is_crossed() -> Result<()> {
        // The support anchor and the target move together by -200, but the
        // target crosses an independently established stationary line: it is
        // between the support and the stationary line on the old side and on
        // the far side of both on the new side. The stationary correspondence
        // is a separate entry with a zero translation.
        let old_blocks = [
            positioned_block(1, "Support anchor line", 10.0, 300.0, 0),
            positioned_block(2, "Moved target line", 10.0, 290.0, 0),
            positioned_block(3, "Stationary established line", 10.0, 200.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Support anchor line", 10.0, 100.0, 0),
            positioned_block(102, "Moved target line", 10.0, 90.0, 0),
            positioned_block(103, "Stationary established line", 10.0, 200.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a candidate crossing an established stationary line must stay open: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_a_crossed_line_has_a_nonuniform_transform() -> Result<()> {
        // The crossed stationary line is an independently established
        // correspondence whose own transform is not uniform: one glyph of the
        // old side moved slightly in x. Its geometry metadata is complete, so
        // it must stay a reference for the order check instead of being
        // silently dropped.
        let mut stationary = positioned_block(3, "Stationary established line", 10.0, 200.0, 0);
        let mut signatures = stationary
            .position_signatures
            .clone()
            .expect("fixture positions");
        let last = signatures.len() - 1;
        let baseline = signatures[last].baseline();
        signatures[last] = PositionSignature::new(
            Vec2 {
                x: baseline.x + 0.5,
                y: baseline.y,
            },
            Vec2 { x: 1.0, y: 0.0 },
        )
        .expect("valid shifted position");
        stationary.position_signatures = Some(signatures);
        let old_blocks = [
            positioned_block(1, "Support anchor line", 10.0, 300.0, 0),
            positioned_block(2, "Moved target line", 10.0, 290.0, 0),
            stationary,
        ];
        let new_blocks = [
            positioned_block(101, "Support anchor line", 10.0, 100.0, 0),
            positioned_block(102, "Moved target line", 10.0, 90.0, 0),
            positioned_block(103, "Stationary established line", 10.0, 200.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a non-uniform established reference must still catch the crossing: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_a_crossed_reference_lacks_positions() -> Result<()> {
        // The crossed established line keeps its known page but loses its
        // position signatures, so the view metadata cannot locate it. The
        // source page still proves it is on the candidate's page, and the
        // incomplete geometry must hold the candidate instead of dropping the
        // reference.
        let mut stationary = positioned_block(3, "Stationary established line", 10.0, 200.0, 0);
        stationary.position_signatures = None;
        assert_eq!(stationary.pages, [0]);
        let old_blocks = [
            positioned_block(1, "Support anchor line", 10.0, 300.0, 0),
            positioned_block(2, "Moved target line", 10.0, 290.0, 0),
            stationary,
        ];
        let new_blocks = [
            positioned_block(101, "Support anchor line", 10.0, 100.0, 0),
            positioned_block(102, "Moved target line", 10.0, 90.0, 0),
            positioned_block(103, "Stationary established line", 10.0, 200.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a same-page reference without positions must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_skips_a_reference_on_another_page() -> Result<()> {
        // The extra established correspondence provably lives on page one on
        // both sides, so its raw coordinates are not order evidence for the
        // page-zero candidate and it must not hold the proof.
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
            positioned_block(3, "Other page anchor line", 10.0, 500.0, 1),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
            positioned_block(103, "Other page anchor line", 10.0, 100.0, 1),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 1, 1),
            "a known other-page reference must not hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_a_reference_page_is_unknown() -> Result<()> {
        // The extra established correspondence spans two pages, so it cannot
        // be proven to be on the candidate's page or on another one. The
        // candidate is held instead of comparing unrelated coordinates.
        let mut stationary = positioned_block(3, "Stationary established line", 10.0, 200.0, 0);
        stationary.pages = vec![0, 1];
        stationary.page_breaks = Some(vec![1]);
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
            stationary,
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
            positioned_block(103, "Stationary established line", 10.0, 200.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an unknown-page reference must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_is_symmetric_under_a_side_swap() -> Result<()> {
        // A supports the target on both sides with one translation, while a
        // second established anchor is adjacent on the new side only with a
        // different translation. The new-side ambiguity must hold the
        // candidate, and swapping the two sides must give the same verdict.
        let old_blocks = [
            positioned_block(1, "Support anchor line", 10.0, 300.0, 0),
            positioned_block(2, "Moved target line", 10.0, 290.0, 0),
            positioned_block(3, "Middle anchor line", 10.0, 280.0, 0),
            positioned_block(4, "Trailing anchor line", 10.0, 200.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Support anchor line", 10.0, 100.0, 0),
            positioned_block(102, "Moved target line", 10.0, 90.0, 0),
            positioned_block(103, "Trailing anchor line", 10.0, 80.0, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [None, None, None, None];
        let new_intervals = [None, None, None];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(103),
            },
        ];
        let forward = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            forward.is_empty(),
            "a new-side-only adjacent anchor with another translation must hold: {forward:?}"
        );
        let swapped_established = [
            EstablishedBlock {
                old_block: BlockId(101),
                new_block: BlockId(1),
            },
            EstablishedBlock {
                old_block: BlockId(103),
                new_block: BlockId(4),
            },
        ];
        let swapped = discover_translations(
            [&new, &old],
            recovery(&new_intervals, &old_intervals),
            &swapped_established,
            &mut 100_000,
            100,
        )?;
        assert!(
            swapped.is_empty(),
            "the swapped sides must give the same verdict: {swapped:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_closes_under_a_side_swap() -> Result<()> {
        // The positive case is symmetric: swapping the two sides still closes
        // the same correspondence with the mirrored spans.
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let forward_established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let forward =
            discover_translations([&old, &new], input, &forward_established, &mut 100_000, 100)?;
        assert_eq!(forward.len(), 1, "{forward:?}");
        assert_eq!(forward[0].old_span.blocks, [BlockId(2)]);
        assert_eq!(forward[0].new_span.blocks, [BlockId(102)]);
        let swapped_established = [EstablishedBlock {
            old_block: BlockId(101),
            new_block: BlockId(1),
        }];
        let swapped =
            discover_translations([&new, &old], input, &swapped_established, &mut 100_000, 100)?;
        assert_eq!(swapped.len(), 1, "{swapped:?}");
        assert_eq!(swapped[0].old_span.blocks, [BlockId(102)]);
        assert_eq!(swapped[0].new_span.blocks, [BlockId(2)]);
        Ok(())
    }

    #[test]
    fn anchored_translation_rejects_an_overflowing_translation() -> Result<()> {
        // Finite baselines can still overflow their difference. The neighbour
        // pair overflows to -inf on every token; that is never a valid key.
        let old_blocks = [
            positioned_block(1, "Upper overflow anchor", 10.0, 1.0e308, 0),
            positioned_block(2, "Moved overflow line", 10.0, 1.0e308, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Upper overflow anchor", 10.0, -1.0e308, 0),
            positioned_block(102, "Moved overflow line", 10.0, -1.0e308, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an overflowing baseline difference must never become a key: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_respects_a_mid_budget_cut() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let mut full_budget = usize::MAX;
        let full = discover_translations([&old, &new], input, &established, &mut full_budget, 100)?;
        let used = usize::MAX - full_budget;
        assert_eq!(full.len(), 1, "{full:?}");
        assert!(used > 1);
        // A positive budget one unit short cuts the optional pass: it commits
        // none of its additions instead of leaving a partial proof behind.
        let mut budget = used - 1;
        let domains = discover_translations([&old, &new], input, &established, &mut budget, 100)?;
        assert!(domains.is_empty(), "{domains:?}");
        assert_eq!(budget, 0);
        Ok(())
    }

    #[test]
    fn anchored_translation_follows_a_same_band_neighbour_across_other_columns() -> Result<()> {
        // The target and its support sit in the same right-hand column band
        // and the support is the nearest source boundary below the target, but
        // two left-column lines sit between them in the block order. The
        // proven band relation, not the block index, must admit the support.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "a same-band nearest neighbour must support the move: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_a_point_obstacle_blocks_the_band() -> Result<()> {
        // A one-token source block has a zero-width baseline x interval. It
        // sits inside the column band and inside the gap, so it still blocks
        // the nearest-boundary proof.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "X", 350.0, 685.0, 0, 6.0),
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "X", 350.0, 685.0, 0, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a zero-width obstacle inside the band must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_a_same_band_line_intervenes() -> Result<()> {
        // A third source line sits in the same column band between the target
        // and the support, so the support is not the nearest source boundary.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "Intervening right column line", 300.0, 685.0, 0, 6.0),
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "Intervening right column line", 300.0, 685.0, 0, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a same-band intervening line must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_a_band_obstacle_lacks_geometry() -> Result<()> {
        // The intervening line keeps its position in the block list but loses
        // its geometry, so the band closure cannot be proven.
        let mut obstacle = spread_block(4, "Intervening right column line", 300.0, 685.0, 0, 6.0);
        obstacle.position_signatures = None;
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            obstacle,
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(104, "Intervening right column line", 300.0, 685.0, 0, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an obstacle without geometry must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_only_one_side_is_geometrically_adjacent() -> Result<()> {
        // The obstacle exists only in the new document, so the band relation
        // holds on the old side but not on the new one.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(104, "Intervening right column line", 300.0, 685.0, 0, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [None, None, None];
        let new_intervals = [None, None, None, None];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a one-sided geometric adjacency must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_follows_a_same_band_neighbour_under_a_side_swap() -> Result<()> {
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
        ];
        let forward = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert_eq!(forward.len(), 1, "{forward:?}");
        let swapped_established = [
            EstablishedBlock {
                old_block: BlockId(102),
                new_block: BlockId(2),
            },
            EstablishedBlock {
                old_block: BlockId(103),
                new_block: BlockId(3),
            },
            EstablishedBlock {
                old_block: BlockId(104),
                new_block: BlockId(4),
            },
        ];
        let swapped =
            discover_translations([&new, &old], input, &swapped_established, &mut 100_000, 100)?;
        assert_eq!(swapped.len(), 1, "{swapped:?}");
        assert_eq!(swapped[0].old_span.blocks, [BlockId(101)]);
        assert_eq!(swapped[0].new_span.blocks, [BlockId(1)]);
        Ok(())
    }

    #[test]
    fn anchored_translation_respects_a_mid_budget_cut_with_a_band_neighbour() -> Result<()> {
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
        ];
        let mut full_budget = usize::MAX;
        let full = discover_translations([&old, &new], input, &established, &mut full_budget, 100)?;
        let used = usize::MAX - full_budget;
        assert_eq!(full.len(), 1, "{full:?}");
        assert!(used > 1);
        let mut budget = used - 1;
        let domains = discover_translations([&old, &new], input, &established, &mut budget, 100)?;
        assert!(domains.is_empty(), "{domains:?}");
        assert_eq!(budget, 0);
        Ok(())
    }

    #[test]
    fn anchored_translation_checks_a_reference_without_view_geometry() -> Result<()> {
        // The stationary reference keeps its source position signatures but is
        // not a complete single-line view (a soft line break), so the view
        // metadata carries no positions. Its own signatures must still catch
        // the crossing instead of the reference disappearing.
        let mut stationary = spread_block(3, "Stationary established line", 300.0, 200.0, 0, 6.0);
        stationary.line_breaks = Some(vec![1]);
        let old_blocks = [
            spread_block(1, "Support anchor line", 300.0, 300.0, 0, 6.0),
            spread_block(2, "Moved target line", 300.0, 290.0, 0, 6.0),
            stationary,
        ];
        let new_blocks = [
            spread_block(101, "Support anchor line", 300.0, 100.0, 0, 6.0),
            spread_block(102, "Moved target line", 300.0, 90.0, 0, 6.0),
            spread_block(103, "Stationary established line", 300.0, 200.0, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a reference without view geometry must still catch the crossing: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_is_independent_of_evidence_order() -> Result<()> {
        // The same logical evidence set in a different order must give the
        // same result.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let ascending = [
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
        ];
        let descending = [
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
        ];
        let first = discover_translations([&old, &new], input, &ascending, &mut 100_000, 100)?;
        let second = discover_translations([&old, &new], input, &descending, &mut 100_000, 100)?;
        assert_eq!(first, second, "evidence order must not change the result");
        assert_eq!(first.len(), 1, "{first:?}");
        Ok(())
    }

    #[test]
    fn anchored_translation_treats_a_one_sided_stationary_anchor_symmetrically() -> Result<()> {
        // A stationary anchor is adjacent to the target on the new side only.
        // It never competes for the translation key and its order relation is
        // checked as a reference, so the clean band support still closes.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "Support right column line", 300.0, 670.0, 0, 6.0),
            spread_block(5, "Stationary right column line", 300.0, 650.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(105, "Stationary right column line", 300.0, 650.0, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
            EstablishedBlock {
                old_block: BlockId(4),
                new_block: BlockId(104),
            },
            EstablishedBlock {
                old_block: BlockId(5),
                new_block: BlockId(105),
            },
        ];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert_eq!(domains.len(), 1, "{domains:?}");
        assert_eq!(domains[0].old_span.blocks, [BlockId(1)]);
        assert_eq!(domains[0].new_span.blocks, [BlockId(101)]);
        Ok(())
    }

    #[test]
    fn anchored_translation_skips_an_obstacle_on_another_page() -> Result<()> {
        // The block at the band's gap coordinates provably lives on another
        // page, so its raw coordinates are not order evidence and it must not
        // hold the candidate.
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(4, "X", 350.0, 685.0, 1, 6.0),
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "X", 350.0, 685.0, 1, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 0, 0),
            "an obstacle provably on another page must not hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_an_obstacle_page_is_unknown() -> Result<()> {
        // The block at the band's gap coordinates has no page evidence, so it
        // cannot be proven to be on another page and the candidate is held.
        let mut obstacle = spread_block(4, "X", 350.0, 685.0, 0, 6.0);
        obstacle.pages = Vec::new();
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            obstacle,
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "X", 350.0, 685.0, 0, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "an obstacle without page evidence must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_holds_when_an_obstacle_keeps_source_without_text() -> Result<()> {
        // The block keeps its source map but has no comparable tokens or
        // geometry, so it is still a source obstacle and cannot be cleared.
        let mut obstacle = spread_block(4, "X", 350.0, 685.0, 0, 6.0);
        obstacle.canonical.text = String::new();
        obstacle.raw.text = String::new();
        obstacle.matching = String::new();
        obstacle.matching_tokens = Vec::new();
        obstacle.position_signatures = Some(Vec::new());
        obstacle.font_size_signatures = Some(Vec::new());
        let old_blocks = [
            spread_block(1, "Target right column line", 300.0, 700.0, 0, 6.0),
            spread_block(2, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(3, "Left column second line", 10.0, 680.0, 0, 6.0),
            obstacle,
            spread_block(5, "Support right column line", 300.0, 670.0, 0, 6.0),
        ];
        let new_blocks = [
            spread_block(101, "Target right column line", 300.0, 699.5, 0, 6.0),
            spread_block(102, "Left column first line", 10.0, 690.0, 0, 6.0),
            spread_block(103, "Left column second line", 10.0, 680.0, 0, 6.0),
            spread_block(104, "X", 350.0, 685.0, 0, 6.0),
            spread_block(105, "Support right column line", 300.0, 669.5, 0, 6.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(5),
            new_block: BlockId(105),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a source block without text or geometry must hold the candidate: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_requires_an_established_neighbour() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let domains = discover_translations([&old, &new], input, &[], &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a moved singleton without established neighbour evidence stays open: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_requires_the_same_transform() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.75, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a candidate with a different transform than the neighbour stays open: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_rejects_a_competing_occurrence() -> Result<()> {
        // Two new-side occurrences sit at the same translated position, so the
        // translation key is not one-to-one and the proof must be withheld.
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Repeated moved line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Repeated moved line", 10.0, 679.5, 0),
            positioned_block(103, "Repeated moved line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [None, None];
        let new_intervals = [None, None, None];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a second occurrence under the same translation key vetoes the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_requires_a_whole_singleton() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [None, interval(1, 0, 1)];
        let new_intervals = [None, None];
        let input = recovery(&old_intervals, &new_intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains = discover_translations([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a member of a longer view is never expanded into a whole-view equality: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn anchored_translation_respects_the_work_budget() -> Result<()> {
        let old_blocks = [
            positioned_block(1, "Neighbour anchor line", 10.0, 700.0, 0),
            positioned_block(2, "Moved singleton line", 10.0, 680.0, 0),
        ];
        let new_blocks = [
            positioned_block(101, "Neighbour anchor line", 10.0, 699.5, 0),
            positioned_block(102, "Moved singleton line", 10.0, 679.5, 0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let mut budget = 1;
        let domains = discover_translations([&old, &new], input, &established, &mut budget, 100)?;
        assert!(domains.is_empty());
        assert_eq!(budget, 0);
        Ok(())
    }

    fn bracketed_fixture(
        candidate_old: &str,
        candidate_new: &str,
        extra_old: &[crate::normalize::BlockText],
        extra_new: &[crate::normalize::BlockText],
    ) -> (
        Vec<crate::normalize::BlockText>,
        Vec<crate::normalize::BlockText>,
    ) {
        let mut old_blocks = vec![
            spread_block(1, "Upper boundary line", 10.0, 700.0, 0, 5.0),
            spread_block(2, candidate_old, 10.0, 680.0, 0, 5.0),
            spread_block(3, "Lower boundary line", 10.0, 660.0, 0, 5.0),
        ];
        let mut new_blocks = vec![
            spread_block(101, "Upper boundary line", 10.0, 700.0, 0, 5.0),
            spread_block(102, candidate_new, 10.0, 680.0, 0, 5.0),
            spread_block(103, "Lower boundary line", 10.0, 660.0, 0, 5.0),
        ];
        old_blocks.extend_from_slice(extra_old);
        new_blocks.extend_from_slice(extra_new);
        (old_blocks, new_blocks)
    }

    fn bracketed_anchors() -> [EstablishedBlock; 2] {
        [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ]
    }

    /// A source-backed block whose tokens span a raw vertical interval at one
    /// x, so its baseline geometry crosses a horizontal boundary instead of
    /// touching it as a point.
    fn interval_block(
        id: u64,
        text: &str,
        x: f64,
        y_min: f64,
        y_max: f64,
        page: u32,
    ) -> crate::normalize::BlockText {
        let mut block = sourced_block(id, text);
        let tokens = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        let last = (tokens.saturating_sub(1)).max(1) as f64;
        let signatures = (0..tokens)
            .map(|index| {
                let ratio = index as f64 / last;
                PositionSignature::new(
                    Vec2 {
                        x: x + index as f64,
                        y: y_min + (y_max - y_min) * ratio,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("valid position")
            })
            .collect::<Vec<_>>();
        block.position_signatures = Some(signatures);
        block.pages = vec![page];
        block
    }

    fn bracketed_domains(
        old_blocks: &[crate::normalize::BlockText],
        new_blocks: &[crate::normalize::BlockText],
        established: &[EstablishedBlock],
    ) -> Result<Vec<LocalDomain>> {
        let old = side(old_blocks);
        let new = side(new_blocks);
        let intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let input = recovery(&intervals, &new_intervals);
        discover_bracketed_domains([&old, &new], input, established, &mut 100_000, 100)
    }

    #[test]
    fn bracketed_domain_holds_when_a_block_straddles_the_upper_boundary() -> Result<()> {
        // The obstacle's baseline interval starts inside the region and ends
        // above the upper boundary, so the region's source closure does not
        // hold even though its text differs from the candidate.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[interval_block(
                4,
                "Upper straddle line",
                10.0,
                690.0,
                710.0,
                0,
            )],
            &[interval_block(
                104,
                "Upper straddle line",
                10.0,
                690.0,
                710.0,
                0,
            )],
        );
        let domains = bracketed_domains(&old_blocks, &new_blocks, &bracketed_anchors())?;
        assert!(
            domains.is_empty(),
            "a block straddling the upper boundary must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_block_straddles_the_lower_boundary() -> Result<()> {
        // The obstacle's baseline interval starts below the lower boundary and
        // ends inside the region, so the region's source closure does not hold.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[interval_block(
                4,
                "Lower straddle line",
                10.0,
                650.0,
                670.0,
                0,
            )],
            &[interval_block(
                104,
                "Lower straddle line",
                10.0,
                650.0,
                670.0,
                0,
            )],
        );
        let domains = bracketed_domains(&old_blocks, &new_blocks, &bracketed_anchors())?;
        assert!(
            domains.is_empty(),
            "a block straddling the lower boundary must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_block_straddles_both_boundaries() -> Result<()> {
        // The obstacle spans the whole region and crosses both boundaries.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[interval_block(
                4,
                "Full straddle line",
                10.0,
                640.0,
                710.0,
                0,
            )],
            &[interval_block(
                104,
                "Full straddle line",
                10.0,
                640.0,
                710.0,
                0,
            )],
        );
        let domains = bracketed_domains(&old_blocks, &new_blocks, &bracketed_anchors())?;
        assert!(
            domains.is_empty(),
            "a block straddling both boundaries must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_only_one_side_has_a_straddling_block() -> Result<()> {
        // The straddling block exists only on the old side, so the two sides
        // do not agree on the region's source closure.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[interval_block(
                4,
                "Upper straddle line",
                10.0,
                690.0,
                710.0,
                0,
            )],
            &[],
        );
        let domains = bracketed_domains(&old_blocks, &new_blocks, &bracketed_anchors())?;
        assert!(
            domains.is_empty(),
            "a one-sided straddling block must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_ignores_a_point_touching_the_upper_boundary() -> Result<()> {
        // A zero-width block exactly on the upper boundary's line does not
        // intersect the open region, so the candidate stays unique.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[positioned_block(4, "Touch line", 10.0, 700.0, 0)],
            &[positioned_block(104, "Touch line", 10.0, 700.0, 0)],
        );
        let domains = bracketed_domains(&old_blocks, &new_blocks, &bracketed_anchors())?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 1, 1),
            "a point touching the boundary must not hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_reference_ties_with_the_upper_boundary() -> Result<()> {
        // A second established line shares the upper boundary's baseline y, so
        // the nearest boundary is ambiguous and the proof must not depend on
        // the evidence order.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[spread_block(4, "Tied reference line", 10.0, 700.0, 0, 5.0)],
            &[spread_block(
                104,
                "Tied reference line",
                10.0,
                700.0,
                0,
                5.0,
            )],
        );
        let mut established = bracketed_anchors().to_vec();
        established.push(EstablishedBlock {
            old_block: BlockId(4),
            new_block: BlockId(104),
        });
        let domains = bracketed_domains(&old_blocks, &new_blocks, &established)?;
        assert!(
            domains.is_empty(),
            "an equal-distance boundary must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_reference_crosses_the_candidate() -> Result<()> {
        // The distant established line is above the candidate on the old side
        // and below it on the new side, so the candidate crosses an
        // independently established correspondence even though the bracketed
        // cell itself is unique on both sides.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[spread_block(
                4,
                "Distant reference line",
                10.0,
                900.0,
                0,
                5.0,
            )],
            &[spread_block(
                104,
                "Distant reference line",
                10.0,
                500.0,
                0,
                5.0,
            )],
        );
        let mut established = bracketed_anchors().to_vec();
        established.push(EstablishedBlock {
            old_block: BlockId(4),
            new_block: BlockId(104),
        });
        let domains = bracketed_domains(&old_blocks, &new_blocks, &established)?;
        assert!(
            domains.is_empty(),
            "a crossed established reference must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_reference_lacks_geometry() -> Result<()> {
        // The extra established correspondence has no position signatures at
        // all, so its relation cannot be checked; the candidate is held
        // instead of dropping the reference from the inspection set.
        let mut old_reference = spread_block(4, "Geometry-free reference", 10.0, 900.0, 0, 5.0);
        old_reference.position_signatures = None;
        let mut new_reference = spread_block(104, "Geometry-free reference", 10.0, 900.0, 0, 5.0);
        new_reference.position_signatures = None;
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[old_reference],
            &[new_reference],
        );
        let mut established = bracketed_anchors().to_vec();
        established.push(EstablishedBlock {
            old_block: BlockId(4),
            new_block: BlockId(104),
        });
        let domains = bracketed_domains(&old_blocks, &new_blocks, &established)?;
        assert!(
            domains.is_empty(),
            "a reference without geometry must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_reference_page_is_unknown() -> Result<()> {
        // The extra established correspondence spans two pages on both sides,
        // so it cannot be proven to be on the candidate's page or on another
        // one; the candidate is held instead of comparing unrelated
        // coordinates.
        let mut old_reference = spread_block(4, "Unknown page reference", 10.0, 900.0, 0, 5.0);
        old_reference.pages = vec![0, 1];
        old_reference.page_breaks = Some(vec![1]);
        let mut new_reference = spread_block(104, "Unknown page reference", 10.0, 900.0, 0, 5.0);
        new_reference.pages = vec![0, 1];
        new_reference.page_breaks = Some(vec![1]);
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[old_reference],
            &[new_reference],
        );
        let mut established = bracketed_anchors().to_vec();
        established.push(EstablishedBlock {
            old_block: BlockId(4),
            new_block: BlockId(104),
        });
        let domains = bracketed_domains(&old_blocks, &new_blocks, &established)?;
        assert!(
            domains.is_empty(),
            "an unknown reference page must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_is_independent_of_evidence_order() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[],
            &[],
        );
        let forward = bracketed_anchors();
        let reversed = [forward[1], forward[0]];
        let domains_forward = bracketed_domains(&old_blocks, &new_blocks, &forward)?;
        let domains_reversed = bracketed_domains(&old_blocks, &new_blocks, &reversed)?;
        assert!(
            whole_view_positioned_domain(&domains_forward, &old_blocks, &new_blocks, 1, 1),
            "the positive fixture must close in both orders: {domains_forward:?}"
        );
        assert_eq!(
            domains_forward, domains_reversed,
            "the evidence order must not change the decision"
        );
        // The ambiguous tie holds in both orders as well.
        let (tied_old, tied_new) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[spread_block(4, "Tied reference line", 10.0, 700.0, 0, 5.0)],
            &[spread_block(
                104,
                "Tied reference line",
                10.0,
                700.0,
                0,
                5.0,
            )],
        );
        let mut established = bracketed_anchors().to_vec();
        established.push(EstablishedBlock {
            old_block: BlockId(4),
            new_block: BlockId(104),
        });
        let tied_forward = bracketed_domains(&tied_old, &tied_new, &established)?;
        let established_reversed = established.iter().rev().copied().collect::<Vec<_>>();
        let tied_reversed = bracketed_domains(&tied_old, &tied_new, &established_reversed)?;
        assert!(
            tied_forward.is_empty() && tied_reversed.is_empty(),
            "an ambiguous tie must hold in both orders: {tied_forward:?} {tied_reversed:?}"
        );
        Ok(())
    }

    /// A source-backed block whose tokens span an explicit baseline box, so
    /// its bounds are exactly the requested x and y intervals.
    fn box_block(
        id: u64,
        text: &str,
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
        page: u32,
    ) -> crate::normalize::BlockText {
        let mut block = sourced_block(id, text);
        let tokens = block
            .canonical
            .comparable_tokens()
            .expect("source-backed fixture tokens")
            .len();
        let last = (tokens.saturating_sub(1)).max(1) as f64;
        let signatures = (0..tokens)
            .map(|index| {
                let ratio = index as f64 / last;
                PositionSignature::new(
                    Vec2 {
                        x: x_min + (x_max - x_min) * ratio,
                        y: y_min + (y_max - y_min) * ratio,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("valid position")
            })
            .collect::<Vec<_>>();
        block.position_signatures = Some(signatures);
        block.pages = vec![page];
        block
    }

    /// The asymmetric nearest-boundary fixture: the upper anchor A and the
    /// third reference C share a facing edge on the new side but not on the
    /// old side, while C stays outside the boundaries' common band.
    fn asymmetric_nearest_fixture() -> (
        Vec<crate::normalize::BlockText>,
        Vec<crate::normalize::BlockText>,
    ) {
        let old_blocks = vec![
            box_block(1, "Upper anchor line", 0.0, 60.0, 700.0, 720.0, 0),
            box_block(2, "Lower anchor line", 40.0, 100.0, 600.0, 600.0, 0),
            box_block(3, "Outer reference line", 70.0, 100.0, 705.0, 725.0, 0),
            box_block(4, "Filing year 2024 statement", 0.0, 100.0, 650.0, 650.0, 0),
        ];
        let new_blocks = vec![
            box_block(101, "Upper anchor line", 0.0, 60.0, 700.0, 720.0, 0),
            box_block(102, "Lower anchor line", 40.0, 100.0, 600.0, 600.0, 0),
            box_block(103, "Outer reference line", 70.0, 100.0, 700.0, 725.0, 0),
            box_block(
                104,
                "Filing year 2025 statement",
                0.0,
                100.0,
                650.0,
                650.0,
                0,
            ),
        ];
        (old_blocks, new_blocks)
    }

    fn asymmetric_anchors() -> [EstablishedBlock; 3] {
        [
            EstablishedBlock {
                old_block: BlockId(1),
                new_block: BlockId(101),
            },
            EstablishedBlock {
                old_block: BlockId(2),
                new_block: BlockId(102),
            },
            EstablishedBlock {
                old_block: BlockId(3),
                new_block: BlockId(103),
            },
        ]
    }

    #[test]
    fn bracketed_domain_holds_when_the_nearest_boundary_differs_between_sides() -> Result<()> {
        // A and C share the facing edge 700 on the new side, so the nearest
        // upper boundary is ambiguous there; C stays outside the boundaries'
        // common band. The same decision must come out in both directions and
        // in both evidence orders.
        let (old_blocks, new_blocks) = asymmetric_nearest_fixture();
        let established = asymmetric_anchors();
        let forward = bracketed_domains(&old_blocks, &new_blocks, &established)?;
        let established_reversed = established.iter().rev().copied().collect::<Vec<_>>();
        let forward_reversed = bracketed_domains(&old_blocks, &new_blocks, &established_reversed)?;
        let swapped = [
            EstablishedBlock {
                old_block: BlockId(101),
                new_block: BlockId(1),
            },
            EstablishedBlock {
                old_block: BlockId(102),
                new_block: BlockId(2),
            },
            EstablishedBlock {
                old_block: BlockId(103),
                new_block: BlockId(3),
            },
        ];
        let reverse = bracketed_domains(&new_blocks, &old_blocks, &swapped)?;
        assert!(
            forward.is_empty(),
            "the ambiguous new-side boundary must hold forward: {forward:?}"
        );
        assert!(
            forward_reversed.is_empty(),
            "the evidence order must not change the hold: {forward_reversed:?}"
        );
        assert!(
            reverse.is_empty(),
            "the same decision must come out in the other direction: {reverse:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_closes_a_single_token_replacement() -> Result<()> {
        // The candidate is an untrusted whole source-bounded singleton whose
        // only difference is a one-token replacement. Two established
        // correspondences bracket it in the same column band, so the region
        // correspondence is proven by the boundaries and the ordinary local
        // assessment can compare the tokens without any text-based selection.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[],
            &[],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 1, 1),
            "two established boundaries must close the bracketed replacement: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_requires_both_boundaries() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[],
            &[],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [EstablishedBlock {
            old_block: BlockId(1),
            new_block: BlockId(101),
        }];
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a single boundary must not prove a region: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_point_obstacle_enters_the_region() -> Result<()> {
        // A same-band point obstacle strictly between the boundaries means the
        // region is not unique even though its text differs from the
        // candidate.
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[positioned_block(4, "Extra uncertain line", 10.0, 670.0, 0)],
            &[positioned_block(
                104,
                "Extra uncertain line",
                10.0,
                670.0,
                0,
            )],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a point obstacle inside the region must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_a_region_block_lacks_geometry() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[block(4, "Extra line without geometry")],
            &[block(104, "Extra line without geometry")],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a region block without geometry must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_the_boundaries_swap_order() -> Result<()> {
        let old_blocks = [
            spread_block(1, "Upper boundary line", 10.0, 700.0, 0, 5.0),
            spread_block(2, "Filing year 2024 statement", 10.0, 680.0, 0, 5.0),
            spread_block(3, "Lower boundary line", 10.0, 660.0, 0, 5.0),
        ];
        let new_blocks = [
            spread_block(101, "Upper boundary line", 10.0, 660.0, 0, 5.0),
            spread_block(102, "Filing year 2025 statement", 10.0, 680.0, 0, 5.0),
            spread_block(103, "Lower boundary line", 10.0, 700.0, 0, 5.0),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "boundaries that swap order must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_skips_a_region_block_on_another_page() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[positioned_block(4, "Other page line", 10.0, 670.0, 1)],
            &[positioned_block(104, "Other page line", 10.0, 670.0, 1)],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &old_blocks, &new_blocks, 1, 1),
            "a provably other-page block must not hold the region: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_holds_when_the_candidate_text_is_duplicated_elsewhere() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[positioned_block(
                4,
                "Filing year 2024 statement",
                10.0,
                400.0,
                0,
            )],
            &[positioned_block(
                104,
                "Filing year 2025 statement",
                10.0,
                400.0,
                0,
            )],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut 100_000, 100)?;
        assert!(
            domains.is_empty(),
            "a whole-line duplicate elsewhere on the page must hold the proof: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_is_symmetric_under_a_side_swap() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[],
            &[],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = [
            EstablishedBlock {
                old_block: BlockId(101),
                new_block: BlockId(1),
            },
            EstablishedBlock {
                old_block: BlockId(103),
                new_block: BlockId(3),
            },
        ];
        let domains =
            discover_bracketed_domains([&new, &old], input, &established, &mut 100_000, 100)?;
        assert!(
            whole_view_positioned_domain(&domains, &new_blocks, &old_blocks, 1, 1),
            "the bracketed proof must mirror under a side swap: {domains:?}"
        );
        Ok(())
    }

    #[test]
    fn bracketed_domain_respects_a_mid_budget_cut() -> Result<()> {
        let (old_blocks, new_blocks) = bracketed_fixture(
            "Filing year 2024 statement",
            "Filing year 2025 statement",
            &[],
            &[],
        );
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [None, None, None];
        let input = recovery(&intervals, &intervals);
        let established = bracketed_anchors();
        let mut budget = 1;
        let domains =
            discover_bracketed_domains([&old, &new], input, &established, &mut budget, 100)?;
        assert!(domains.is_empty());
        assert_eq!(budget, 0);
        Ok(())
    }

    #[test]
    fn source_bounded_whole_view_anchor_closes_its_own_domain() {
        let old_blocks = [sourced_block(1, "Identical whole line evidence.")];
        let new_blocks = [sourced_block(101, "Identical whole line evidence.")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 12;

        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)
            .expect("whole-view anchor search stays within budget");

        assert_eq!(domains.len(), 1);
        assert_eq!(
            super::super::span_tokens(&old, &domains[0].old_span).expect("old tokens"),
            old_blocks[0]
                .canonical
                .comparable_tokens()
                .expect("old source")
        );
        assert_eq!(
            super::super::span_tokens(&new, &domains[0].new_span).expect("new tokens"),
            new_blocks[0]
                .canonical
                .comparable_tokens()
                .expect("new source")
        );
    }

    #[test]
    fn source_bounded_whole_view_anchor_requires_equal_positions() {
        let old_blocks = [sourced_block(1, "Identical whole line evidence.")];
        let mut moved = sourced_block(101, "Identical whole line evidence.");
        let position = PositionSignature::new(Vec2 { x: 0.0, y: 20.0 }, Vec2 { x: 1.0, y: 0.0 })
            .expect("valid moved position");
        moved.position_signatures = Some(vec![position; moved.canonical.text.chars().count()]);
        let new_blocks = [moved];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 12;

        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)
            .expect("moved whole-view search stays within budget");

        assert!(
            domains.is_empty(),
            "a moved singleton must keep its move or order obligation"
        );
    }

    #[test]
    fn source_bounded_whole_view_anchor_requires_the_same_page() {
        let old_blocks = [sourced_block(1, "Identical whole line evidence.")];
        let mut moved = sourced_block(101, "Identical whole line evidence.");
        moved.pages = vec![1];
        let new_blocks = [moved];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 12;

        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)
            .expect("cross-page whole-view search stays within budget");

        assert!(
            domains.is_empty(),
            "a same-coordinate singleton on another page must keep an obligation"
        );
    }

    #[test]
    fn source_bounded_partial_anchor_does_not_close_a_domain() {
        let old_blocks = [sourced_block(1, "ABCDEFGHxxxxx")];
        let new_blocks = [sourced_block(101, "ABCDEFGHyyyyy")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = vec![None; old_blocks.len()];
        let new_intervals = vec![None; new_blocks.len()];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 8;

        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)
            .expect("partial anchor search stays within budget");

        assert!(
            domains.is_empty(),
            "a partial source-bounded anchor leaves an unanchored remainder"
        );
    }

    #[test]
    fn source_bounded_views_do_not_bridge_a_line_wrap() {
        let old_blocks = [sourced_block(1, "stable"), sourced_block(2, "suffix")];
        let new_blocks = [sourced_block(101, "stable suffix")];
        assert!(source_bounded_block(&old_blocks[0], &mut 10_000));
        assert!(source_bounded_block(&old_blocks[1], &mut 10_000));
        assert!(source_bounded_block(&new_blocks[0], &mut 10_000));
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [None, None];
        let new_intervals = [None];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 1;

        let domains = discover([&old, &new], input, &[], &mut 10_000, 10)
            .expect("line-wrap fixture remains within the local discovery budget");

        assert!(domains.iter().all(|domain| {
            super::super::span_tokens(&old, &domain.old_span).expect("old range")
                == super::super::span_tokens(&new, &domain.new_span).expect("new range")
        }));
    }

    fn interval(run_id: u64, start: usize, end: usize) -> Option<TrustedRunInterval> {
        Some(TrustedRunInterval {
            run_id: TrustedRunId(run_id),
            start,
            end,
        })
    }

    fn mixed_role_descriptor(run_id: u64, count: usize) -> TrustedRunDescriptor {
        use crate::model::{PageId, Rect, Vec2};

        TrustedRunDescriptor {
            id: TrustedRunId(run_id),
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 { x: 100.0, y: 100.0 },
            },
            block_indices: (0..count).collect(),
            trusted_block_indices: (0..count).collect(),
            role: None,
            source_region_ids: Vec::new(),
        }
    }

    #[test]
    fn a_mixed_role_run_retains_its_ordered_body_domain() {
        let mut old_blocks = vec![
            block(1, "header"),
            block(2, "A"),
            block(3, "old"),
            block(4, "C"),
        ];
        let mut new_blocks = vec![
            block(101, "header"),
            block(102, "A"),
            block(103, "new"),
            block(104, "C"),
        ];
        old_blocks[0].role = BlockRole::RepeatedHeader;
        new_blocks[0].role = BlockRole::RepeatedHeader;
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let new_intervals = [
            interval(2, 0, 1),
            interval(2, 1, 2),
            interval(2, 2, 3),
            interval(2, 3, 4),
        ];
        let old_descriptors = [mixed_role_descriptor(1, 4)];
        let new_descriptors = [mixed_role_descriptor(2, 4)];
        let mut input = recovery(&old_intervals, &new_intervals);
        input.old_trusted_run_evidence = Some(super::super::super::TrustedRunRecoveryInput {
            descriptors: &old_descriptors,
            raw_region_edges: &[],
        });
        input.new_trusted_run_evidence = Some(super::super::super::TrustedRunRecoveryInput {
            descriptors: &new_descriptors,
            raw_region_edges: &[],
        });
        let anchors = [
            (span(&[2], 0, 1), span(&[102], 0, 1)),
            (span(&[4], 0, 1), span(&[104], 0, 1)),
        ];
        let domains = discover([&old, &new], input, &anchors, &mut 10_000, 10)
            .expect("mixed roles do not remove known internal order");
        assert!(
            domains.iter().all(
                |domain| domain.old_span.comparable_range.end <= "header".len()
                    || domain.old_span.comparable_range.start >= "header ".len()
            )
        );
        let changed = domains
            .iter()
            .find(|domain| {
                super::super::span_tokens(&old, &domain.old_span).expect("old range")
                    != super::super::span_tokens(&new, &domain.new_span).expect("new range")
            })
            .expect("body content comparison survives the role boundary");
        assert_eq!(changed.old_span.comparable_range.start, "header ".len());
        assert_eq!(
            changed.old_span.comparable_range.end,
            "header A old C".len()
        );
    }

    #[test]
    fn mixed_role_views_keep_competing_occurrences_across_role_boundaries() {
        let mut old_blocks = vec![block(1, "A"), block(2, "B"), block(3, "A B"), block(4, "C")];
        let mut new_blocks = vec![
            block(101, "A"),
            block(102, "B"),
            block(103, "A B"),
            block(104, "C"),
        ];
        old_blocks[0].role = BlockRole::RepeatedHeader;
        new_blocks[0].role = BlockRole::RepeatedHeader;
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [
            interval(1, 0, 1),
            interval(1, 1, 2),
            interval(1, 2, 3),
            interval(1, 3, 4),
        ];
        let anchors = [
            (span(&[3], 0, 3), span(&[103], 0, 3)),
            (span(&[4], 0, 1), span(&[104], 0, 1)),
        ];
        let domains = discover(
            [&old, &new],
            recovery(&intervals, &intervals),
            &anchors,
            &mut 10_000,
            10,
        )
        .expect("competing source occurrences remain visible");
        assert_eq!(domains.len(), 1);
        assert_eq!(
            domains[0].old_span.comparable_range.end - domains[0].old_span.comparable_range.start,
            1
        );
    }

    #[test]
    fn disjoint_intervals_survive_a_run_split_or_merge() {
        let old_blocks = [
            block(1, "A"),
            block(2, "old"),
            block(3, "C"),
            block(4, "M"),
            block(5, "former"),
            block(6, "Z"),
        ];
        let new_blocks = [
            block(101, "A"),
            block(102, "new"),
            block(103, "C"),
            block(104, "M"),
            block(105, "latter"),
            block(106, "Z"),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = (0..6)
            .map(|start| interval(1, start, start + 1))
            .collect::<Vec<_>>();
        let new_intervals = [
            interval(11, 0, 1),
            interval(11, 1, 2),
            interval(11, 2, 3),
            interval(12, 0, 1),
            interval(12, 1, 2),
            interval(12, 2, 3),
        ];
        let anchors = [1, 3, 4, 6].map(|id| (span(&[id], 0, 1), span(&[id + 100], 0, 1)));
        for reversed in [false, true] {
            let (sides, mut input, anchors) = if reversed {
                (
                    [&new, &old],
                    recovery(&new_intervals, &old_intervals),
                    anchors
                        .iter()
                        .map(|(a, b)| (b.clone(), a.clone()))
                        .collect::<Vec<_>>(),
                )
            } else {
                (
                    [&old, &new],
                    recovery(&old_intervals, &new_intervals),
                    anchors.to_vec(),
                )
            };
            input.min_tokens = 16;
            let domains = discover(sides, input, &anchors, &mut 100_000, 100)
                .expect("disjoint anchored intervals retain correspondence");
            assert_eq!(domains.len(), 2, "reversed={reversed}");
            for domain in domains {
                assert_ne!(
                    super::super::span_tokens(sides[0], &domain.old_span).expect("old projection"),
                    super::super::span_tokens(sides[1], &domain.new_span).expect("new projection")
                );
            }
        }
    }

    #[test]
    fn overlapping_correspondences_do_not_close_the_changed_gaps() {
        let old_blocks = [block(1, "A old B middle C tail D")];
        let new_blocks = [block(101, "A new C"), block(102, "B other D")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1)];
        let new_intervals = [interval(11, 0, 1), interval(12, 0, 1)];
        let anchors = [(0, 101, 0), (6, 102, 0), (15, 101, 6), (22, 102, 8)]
            .map(|(a, b, c)| (span(&[1], a, a + 1), span(&[b], c, c + 1)));
        let mut input = recovery(&old_intervals, &new_intervals);
        input.min_tokens = 16;
        let domains = discover([&old, &new], input, &anchors, &mut 100_000, 100)
            .expect("competing interval evidence is a normal outcome");
        assert_eq!(domains.len(), 4);
        for domain in domains {
            assert_eq!(
                super::super::span_tokens(&old, &domain.old_span).expect("old projection"),
                super::super::span_tokens(&new, &domain.new_span).expect("new projection")
            );
            assert_eq!(
                domain.old_span.comparable_range.end - domain.old_span.comparable_range.start,
                1
            );
        }
    }

    #[test]
    fn one_sided_edits_use_the_closed_parent_alignment_in_both_directions() {
        use crate::alignment::{
            Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        };
        use crate::diff::{ChangeEvent, ChangeKind, Confidence, DiffOptions};
        for reverse in [false, true] {
            let old_blocks = [
                block(1, if reverse { "aXb" } else { "ab" }),
                block(2, "noise"),
            ];
            let new_blocks = [
                block(101, if reverse { "ab" } else { "aXb" }),
                block(102, "noise"),
            ];
            let old = side(&old_blocks);
            let new = side(&new_blocks);
            let alignment = Alignment {
                spans: vec![AlignmentSpan {
                    kind: AlignmentKind::Unresolved,
                    old: old_blocks.iter().map(|b| b.block).collect(),
                    new: new_blocks.iter().map(|b| b.block).collect(),
                    score: 0.0,
                    canonical_similarity: 0.0,
                    score_margin: None,
                    confidence: AlignmentConfidence::Low,
                    evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                    old_separator: Some(BlockSeparator::Space),
                    new_separator: Some(BlockSeparator::Space),
                }],
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            let change = ChangeEvent::single_occurrence(
                if reverse {
                    ChangeKind::Deletion
                } else {
                    ChangeKind::Insertion
                },
                reverse.then(|| span(&[1], 1, 2)),
                (!reverse).then(|| span(&[101], 1, 2)),
                Confidence::High,
                Vec::new(),
            );
            let intervals = [interval(1, 0, 1), interval(2, 0, 1)];
            let comparison = super::super::finish(
                [&old, &new],
                &alignment,
                Some(recovery(&intervals, &intervals)),
                None,
                None,
                super::super::ProposedComparison {
                    changes: vec![change.clone()],
                    proven_changed_regions: Vec::new(),
                    formatting_changes: Vec::new(),
                    unresolved_regions: Vec::new(),
                },
                DiffOptions::default(),
            )
            .expect("a local insertion or deletion has an exact parent boundary");
            assert_eq!(
                comparison.changes,
                vec![change],
                "reverse={reverse}; assessment={:#?}",
                comparison.assessment
            );
            assert!(comparison.change_candidates.is_empty());
            assert!(
                comparison
                    .assessment
                    .as_ref()
                    .expect("assessment is retained")
                    .work_by_stage
                    .local_views
                    > 0
            );
        }
    }

    #[test]
    fn local_recovery_retires_an_overlapping_unlocalized_change_proof() {
        use crate::alignment::{
            Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        };
        use crate::diff::{ChangedRegionProof, Confidence, DiffOptions, ProvenChangedRegion};

        let old_blocks = [block(1, "aXb"), block(2, "noise")];
        let new_blocks = [block(101, "aYb"), block(102, "noise")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: vec![crate::layout::BlockId(1), crate::layout::BlockId(2)],
                new: vec![crate::layout::BlockId(101), crate::layout::BlockId(102)],
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: Some(BlockSeparator::Space),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let intervals = [interval(1, 0, 1), interval(2, 0, 1)];
        let comparison = super::super::finish(
            [&old, &new],
            &alignment,
            Some(recovery(&intervals, &intervals)),
            None,
            None,
            super::super::ProposedComparison {
                changes: Vec::new(),
                proven_changed_regions: vec![ProvenChangedRegion {
                    old_span: Some(span(&[1], 0, 3)),
                    new_span: Some(span(&[101], 0, 3)),
                    confidence: Confidence::High,
                    proof: ChangedRegionProof::ExactTokenMultisetMismatch,
                }],
                formatting_changes: Vec::new(),
                unresolved_regions: Vec::new(),
            },
            DiffOptions::default(),
        )
        .expect("local recovery supersedes the unlocalized proof");
        assert_eq!(comparison.changes.len(), 1);
        assert_eq!(
            comparison.changes[0].kind,
            crate::diff::ChangeKind::Replacement
        );
        let occurrence = &comparison.changes[0].occurrences[0];
        assert_eq!(occurrence.old_span, Some(span(&[1], 1, 2)));
        assert_eq!(occurrence.new_span, Some(span(&[101], 1, 2)));
        assert!(comparison.proven_changed_regions.is_empty());
        comparison
            .assessment
            .as_ref()
            .expect("assessment evidence")
            .validate(&comparison)
            .expect("source ownership remains exclusive");
    }

    #[test]
    fn source_windows_recover_anchors_missing_from_sentence_proposals() {
        let old_blocks = [block(
            1,
            "Methods for obtaining these assurances are provided in NIST Special Publication (SP) 800-89, Recommendation for Obtaining Assurances for Digital Signature Applications.",
        )];
        let new_blocks = [block(
            101,
            "Methods for obtaining these assurances are provided in SP 800-89, Recommendation for Obtaining Assurances for Digital Signature Applications.",
        )];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let intervals = [interval(1, 0, 1)];
        let mut input = recovery(&intervals, &intervals);
        input.min_tokens = 16;
        let domains = discover([&old, &new], input, &[], &mut 100_000, 100)
            .expect("anchors do not depend on a sentence proposal");
        let changed = domains
            .iter()
            .filter(|domain| {
                super::super::span_tokens(&old, &domain.old_span).expect("old range")
                    != super::super::span_tokens(&new, &domain.new_span).expect("new range")
            })
            .collect::<Vec<_>>();
        assert_eq!(changed.len(), 1);
        assert!(
            changed[0].old_span.comparable_range.start
                < "Methods for obtaining these assurances are provided in ".len()
        );
        assert!(changed[0].old_span.comparable_range.end > "Methods for obtaining these assurances are provided in NIST Special Publication (SP)".len());
    }

    #[test]
    fn anchor_search_counts_overlaps_and_bounds_repeated_prefix_work() {
        for (text, anchor, count, start) in [
            ("abababa".to_owned(), "ababa".to_owned(), 2, 0),
            (
                format!("{}b", "a".repeat(10_000)),
                format!("{}b", "a".repeat(100)),
                1,
                9_900,
            ),
        ] {
            let blocks = [block(1, &text)];
            let source = side(&blocks);
            let views = build_views(&source, &[None], None, &mut 30_000)
                .expect("valid source views")
                .expect("view construction fits its budget");
            let needle = anchor
                .chars()
                .map(ComparableToken::Scalar)
                .collect::<Vec<_>>();
            let SearchResult::Complete(result) =
                search_views(&views, &needle, &mut 30_000).expect("bounded anchor search")
            else {
                panic!("a repeated prefix must fit linear search work");
            };
            assert_eq!(result.count, count);
            assert_eq!(result.first.expect("anchor occurs").start, start);
            assert!(matches!(
                search_views(&views, &needle, &mut 1).expect("budget is explicit"),
                SearchResult::BudgetExceeded
            ));
        }
    }

    #[test]
    fn source_window_budget_counts_only_inspected_tokens() {
        let old = vec![ComparableToken::Scalar('a'); 128];
        let mut new = old.clone();
        new[0] = ComparableToken::Scalar('b');
        let mut budget = 2;
        assert_eq!(
            super::super::tokens_equal_with_budget(&old, &new, &mut budget),
            Some(false)
        );
        assert_eq!(budget, 0);

        let mut budget = 128;
        assert_eq!(
            super::super::tokens_equal_with_budget(&old, &old, &mut budget),
            None
        );
        let mut budget = 129;
        assert_eq!(
            super::super::tokens_equal_with_budget(&old, &old, &mut budget),
            Some(true)
        );
        assert_eq!(budget, 0);
    }

    #[test]
    fn optional_view_budget_preserves_an_ordinary_established_change() {
        use crate::{
            alignment::{
                Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
            },
            diff::{ChangeEvent, ChangeKind, Confidence, DiffOptions},
        };
        let old_blocks = [
            block(1, "Left boundary."),
            block(2, "abc"),
            block(3, "Right boundary."),
            block(4, &"old noise ".repeat(200)),
        ];
        let new_blocks = [
            block(101, "Left boundary."),
            block(102, "axc"),
            block(103, "Right boundary."),
            block(104, &"new noise ".repeat(200)),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = Alignment {
            spans: (0..4)
                .map(|index| AlignmentSpan {
                    kind: if index == 3 {
                        AlignmentKind::Unresolved
                    } else {
                        AlignmentKind::Match
                    },
                    old: vec![old_blocks[index].block],
                    new: vec![new_blocks[index].block],
                    score: 1.0,
                    canonical_similarity: 1.0,
                    score_margin: Some(1.0),
                    confidence: AlignmentConfidence::High,
                    evidence: if index == 3 {
                        vec![AlignmentEvidence::ReadingOrderUnknown]
                    } else {
                        Vec::new()
                    },
                    old_separator: None,
                    new_separator: None,
                })
                .collect(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let expected = ChangeEvent::single_occurrence(
            ChangeKind::Replacement,
            Some(span(&[2], 1, 2)),
            Some(span(&[102], 1, 2)),
            Confidence::High,
            Vec::new(),
        );
        let proposed = || super::super::ProposedComparison {
            changes: vec![expected.clone()],
            proven_changed_regions: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions: Vec::new(),
        };
        let baseline = super::super::finish(
            [&old, &new],
            &alignment,
            None,
            None,
            None,
            proposed(),
            DiffOptions::default(),
        )
        .expect("ordinary output");
        assert_eq!(baseline.changes.as_slice(), std::slice::from_ref(&expected));
        let baseline_work = baseline
            .assessment
            .as_ref()
            .expect("baseline assessment")
            .work_used;
        let intervals = [
            interval(1, 0, 1),
            interval(2, 0, 1),
            interval(3, 0, 1),
            None,
        ];
        let result = super::super::finish(
            [&old, &new],
            &alignment,
            Some(recovery(&intervals, &intervals)),
            None,
            None,
            proposed(),
            DiffOptions {
                max_assessment_work: baseline_work + 100,
                ..DiffOptions::default()
            },
        )
        .expect("optional limit remains reportable");
        assert_eq!(result.changes, baseline.changes);
        assert_eq!(result.old_coverage, baseline.old_coverage);
        assert_eq!(result.new_coverage, baseline.new_coverage);
        let assessment = result.assessment.as_ref().expect("assessment");
        assert_eq!(assessment.work_used, assessment.work_limit);
        assert!(assessment.relations.iter().any(|relation| {
            relation.search == super::super::SearchCompleteness::Incomplete
                && relation
                    .reasons
                    .contains(&super::super::AssessmentReason::WorkLimit)
        }));
    }

    #[test]
    fn an_ambiguous_gap_in_one_run_does_not_hide_a_separately_anchored_edit() {
        use crate::{
            alignment::{
                Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
            },
            diff::{ChangeKind, DiffOptions},
        };
        let old_text = "Unique opening boundary stays stable. X Middle boundary stays stable. echo echo Final boundary stays stable.";
        let new_text = "Unique opening boundary stays stable. Y Middle boundary stays stable. echo echo echo Final boundary stays stable.";
        let old_blocks = [
            block(1, old_text),
            block(2, "Unrelated surrounding evidence."),
        ];
        let new_blocks = [
            block(101, new_text),
            block(102, "Unrelated surrounding evidence."),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: old_blocks.iter().map(|block| block.block).collect(),
                new: new_blocks.iter().map(|block| block.block).collect(),
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: Some(BlockSeparator::Space),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let intervals = [interval(1, 0, 1), interval(2, 0, 1)];
        let mut input = recovery(&intervals, &intervals);
        input.min_tokens = 12;
        let result = super::super::finish(
            [&old, &new],
            &alignment,
            Some(input),
            None,
            None,
            super::super::ProposedComparison {
                changes: Vec::new(),
                proven_changed_regions: Vec::new(),
                formatting_changes: Vec::new(),
                unresolved_regions: Vec::new(),
            },
            DiffOptions::default(),
        )
        .expect("independent local comparison");
        assert_eq!(result.changes.len(), 1, "{:#?}", result.changes);
        assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
        let occurrence = &result.changes[0].occurrences[0];
        let old_start = old_text.find(" X ").expect("fixed marker") + 1;
        let new_start = new_text.find(" Y ").expect("fixed marker") + 1;
        assert_eq!(
            occurrence.old_span,
            Some(span(&[1], old_start, old_start + 1))
        );
        assert_eq!(
            occurrence.new_span,
            Some(span(&[101], new_start, new_start + 1))
        );
        assert!(!result.unresolved_regions.is_empty());
    }

    #[test]
    fn interleaved_global_blocks_use_run_ordinals_for_domain_spans() {
        let old_blocks = vec![
            block(1, "A"),
            block(2, "noise"),
            block(3, "old"),
            block(4, "other"),
            block(5, "C"),
        ];
        let new_blocks = vec![
            block(101, "A"),
            block(102, "noise"),
            block(103, "new"),
            block(104, "other"),
            block(105, "C"),
        ];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [
            interval(1, 0, 1),
            interval(2, 0, 1),
            interval(1, 1, 2),
            interval(2, 1, 2),
            interval(1, 2, 3),
        ];
        let new_intervals = [
            interval(11, 0, 1),
            interval(12, 0, 1),
            interval(11, 1, 2),
            interval(12, 1, 2),
            interval(11, 2, 3),
        ];
        let anchors = vec![
            (span(&[5], 0, 1), span(&[105], 0, 1)),
            (span(&[1], 0, 1), span(&[101], 0, 1)),
        ];
        let mut budget = 10_000;
        let domains = discover(
            [&old, &new],
            recovery(&old_intervals, &new_intervals),
            &anchors,
            &mut budget,
            10,
        )
        .expect("domain discovery succeeds");
        assert_eq!(domains.len(), 2);
        assert_eq!(
            domains[0].old_span.blocks,
            [BlockId(1), BlockId(3), BlockId(5)]
        );
        assert_eq!(
            domains[0].new_span.blocks,
            [BlockId(101), BlockId(103), BlockId(105)]
        );
        assert_eq!(
            domains[0].old_span.comparable_range,
            super::super::super::TokenRange { start: 0, end: 7 }
        );
        use super::super::{Assessor, ProposedRelation, RelationOutcome};
        use crate::alignment::{
            Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        };
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: old_blocks.iter().map(|block| block.block).collect(),
                new: new_blocks.iter().map(|block| block.block).collect(),
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
                old_separator: Some(BlockSeparator::Space),
                new_separator: Some(BlockSeparator::Space),
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let proposals = anchors
            .into_iter()
            .map(|(old, new)| ProposedRelation {
                old: Some(old),
                new: Some(new),
                span_indices: [Some(0), Some(0)],
                exact_recovery: true,
            })
            .collect::<Vec<_>>();
        let replacement = ProposedRelation {
            old: Some(span(&[3], 0, 3)),
            new: Some(span(&[103], 0, 3)),
            span_indices: [Some(0), Some(0)],
            exact_recovery: false,
        };
        let mut baseline = Assessor::new(
            [&old, &new],
            &alignment,
            Some(recovery(&old_intervals, &new_intervals)),
            crate::diff::DiffOptions::default(),
        )
        .expect("common-assessment baseline is valid");
        let baseline_index = baseline
            .assess(&replacement)
            .expect("baseline assessment succeeds");
        assert_ne!(
            baseline.records[baseline_index].outcome,
            RelationOutcome::Established
        );
        let mut assessor = Assessor::new(
            [&old, &new],
            &alignment,
            Some(recovery(&old_intervals, &new_intervals)),
            crate::diff::DiffOptions::default(),
        )
        .expect("source-backed assessment fixture is valid");
        assessor
            .discover_local_domains(&proposals)
            .expect("source-backed assessment fixture is valid");
        let index = assessor
            .assess(&replacement)
            .expect("source-backed assessment fixture is valid");
        assert_eq!(
            assessor.records[index].outcome,
            RelationOutcome::Established
        );
        let parent = assessor.records[index]
            .parent
            .expect("source-backed assessment fixture is valid");
        assert_eq!(
            assessor.records[parent].old_span,
            Some(domains[0].old_span.clone())
        );
        assert!(assessor.records[parent].parent.is_none());
    }

    #[test]
    fn a_whole_block_anchor_competes_with_text_across_soft_boundaries() {
        let old_blocks = vec![block(1, "ab"), block(2, "a"), block(3, "b")];
        let new_blocks = vec![block(101, "ab"), block(102, "a"), block(103, "c")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let alignment = crate::alignment::Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let assessor = super::super::Assessor::new(
            [&old, &new],
            &alignment,
            None,
            crate::diff::DiffOptions::default(),
        )
        .expect("source-backed assessment fixture is valid");
        assert!(!assessor.anchors.contains(&(0, 0)));
    }

    #[test]
    fn crossing_anchors_prove_only_their_equal_ranges() {
        let old_blocks = vec![block(1, "A"), block(2, "B")];
        let new_blocks = vec![block(101, "B"), block(102, "A")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2)];
        let anchors = vec![
            (span(&[1], 0, 1), span(&[102], 0, 1)),
            (span(&[2], 0, 1), span(&[101], 0, 1)),
        ];
        let mut budget = 10_000;
        let domains = discover(
            [&old, &new],
            recovery(&old_intervals, &new_intervals),
            &anchors,
            &mut budget,
            10,
        )
        .expect("domain discovery succeeds");
        assert_eq!(domains.len(), 2);
        for domain in domains {
            assert_eq!(
                super::super::span_tokens(&old, &domain.old_span).expect("old projection"),
                super::super::span_tokens(&new, &domain.new_span).expect("new projection")
            );
            assert_eq!(
                domain.old_span.comparable_range.end - domain.old_span.comparable_range.start,
                1
            );
        }
    }

    #[test]
    fn one_full_anchor_can_close_a_complete_run() {
        let old_blocks = vec![block(1, "A B")];
        let new_blocks = vec![block(101, "A B")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1)];
        let new_intervals = [interval(2, 0, 1)];
        let anchors = vec![(span(&[1], 0, 3), span(&[101], 0, 3))];
        let mut budget = 10_000;
        let domains = discover(
            [&old, &new],
            recovery(&old_intervals, &new_intervals),
            &anchors,
            &mut budget,
            10,
        )
        .expect("domain discovery succeeds");
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].old_span.comparable_range.start, 0);
        assert_eq!(domains[0].old_span.comparable_range.end, 3);
    }

    #[test]
    fn budget_exhaustion_does_not_accept_partial_domains() {
        let old_blocks = vec![block(1, "A"), block(2, "B"), block(3, "C")];
        let new_blocks = vec![block(101, "A"), block(102, "B"), block(103, "C")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(1, 2, 3)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2), interval(2, 2, 3)];
        let anchors = vec![
            (span(&[1], 0, 1), span(&[101], 0, 1)),
            (span(&[3], 0, 1), span(&[103], 0, 1)),
        ];
        let mut budget = 1;
        let domains = discover(
            [&old, &new],
            recovery(&old_intervals, &new_intervals),
            &anchors,
            &mut budget,
            10,
        )
        .expect("budget exhaustion is a recoverable search result");
        assert!(domains.is_empty());
    }

    #[test]
    fn incomplete_run_blocks_compete_with_trusted_occurrences() {
        let old_blocks = vec![block(1, "A"), block(2, "B"), block(3, "A")];
        let new_blocks = vec![block(101, "A"), block(102, "B")];
        let old = side(&old_blocks);
        let new = side(&new_blocks);
        let old_intervals = [interval(1, 0, 1), interval(1, 1, 2), interval(9, 1, 2)];
        let new_intervals = [interval(2, 0, 1), interval(2, 1, 2)];
        let anchors = vec![
            (span(&[1], 0, 1), span(&[101], 0, 1)),
            (span(&[2], 0, 1), span(&[102], 0, 1)),
        ];
        let mut budget = 10_000;
        let domains = discover(
            [&old, &new],
            recovery(&old_intervals, &new_intervals),
            &anchors,
            &mut budget,
            10,
        )
        .expect("domain discovery succeeds");
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].old_span, span(&[1, 2], 2, 3));
        assert_eq!(domains[0].new_span, span(&[101, 102], 2, 3));
    }
}
