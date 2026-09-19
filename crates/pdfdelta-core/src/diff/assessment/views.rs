use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{
    Error, Result,
    alignment::BlockSeparator,
    layout::{BlockRole, TrustedRunDescriptor, TrustedRunId, TrustedRunInterval},
    normalize::{BlockText, ComparableToken, NormalizationKind},
};

use super::super::{GroupText, SentenceRecoveryInput, Side, TextSpan};
use super::charge;

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
    localized.sort_unstable_by_key(|(key, _)| *key);
    localized.truncate(max_ranges);
    Ok(Discovery {
        domains: localized.into_iter().map(|(_, domain)| domain).collect(),
        anchors: exact_domains,
    })
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
        views.push(View {
            kind: ViewKind::Trusted(run_id),
            source_order: block_indices.iter().copied().min().unwrap_or(usize::MAX),
            block_indices,
            group,
            source_bounded: false,
            horizontal_text: false,
            position_signatures: Vec::new(),
            page: None,
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
        views.push(View {
            kind: ViewKind::Untrusted(block_index),
            source_order: block_index,
            block_indices: vec![block_index],
            group,
            source_bounded,
            horizontal_text,
            position_signatures,
            page,
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
                assert!(
                    domains.is_empty(),
                    "a repeated ending cannot close the changed line"
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
