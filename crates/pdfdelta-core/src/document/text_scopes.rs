//! Non-owning content comparisons between certified boundaries in a closed
//! retained-order parent. This convention compares finite observable intervals;
//! it does not identify individual interior paragraphs or an author's edits.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    Channel, DocumentComparisonLimits, DocumentView, EdgeKind, GraphNode, InterpretationStatus,
    LocalViewComparison, NodeContent, NodeId, ProposalBasis, ScopeViewComparison, SourceRef,
    TypedOperation, ViewBasis, compare_text_group_views, matching::source_children,
};
use crate::Result;

mod cuts;
mod native;
pub use native::{NativeRegion, NativeRegionChain, NativeRegionChains, NativeTransition};

pub use cuts::{
    CutCorrespondence, CutEvidence, SourceCut, SourceCutBoundaryPadding, SourceCutEdgeRefinement,
    SourceCutPopulation, SourceCutRange, SourceCutRowEndpoint, SourceCutRowOrder, SourceCutSearch,
    SourceFragment,
};

/// Content of corresponding intervals under the stated comparison convention.
/// The enclosing scope retains the parent correspondence. Boundary indexes
/// refer to its candidate proposals; member nodes retain all source projections.
/// Neither this unit nor its conditional masks owns changed sources or discharges
/// strict coverage. An inferred parent keeps this comparison inferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopeReview {
    pub convention: String,
    /// Legacy whole-node proposal indexes. Source-cut ranges carry their two
    /// explicit certificates in `source_cuts` instead of inventing proposal IDs.
    pub boundaries: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cuts: Option<SourceCutRange>,
    /// Presence in this corresponding interval only. This does not establish
    /// document-wide novelty, deletion or the absence of an external copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<TextScopePresence>,
    /// Explicit tag order and individually closed page regions, when the
    /// enclosing interval crosses pages. Sources remain non-owning references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_regions: Option<NativeRegionChains>,
    /// Complete interior evidence, including unchanged context. These lists are
    /// range locators, never changed masks or exclusive source ownership.
    pub old_sources: Vec<SourceRef>,
    pub new_sources: Vec<SourceRef>,
    pub old_boundaries: [Vec<SourceRef>; 2],
    pub new_boundaries: [Vec<SourceRef>; 2],
    /// Search status of the enclosing supplier universe, including optional
    /// interior correspondence hypotheses. It does not replace the accepted
    /// source-boundary decisions. Older reports leave this observation unknown.
    #[serde(default)]
    pub candidate_search_exhaustive: Option<bool>,
    /// Space provenance in the concatenated review text. Layout-derived entries
    /// retain both spacing interpretations; they are not literal space glyphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<TextScopeSpacing>,
    pub comparison: LocalViewComparison,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopeSpacing {
    pub convention: String,
    pub old: Vec<SpaceBoundary>,
    pub new: Vec<SpaceBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopePresence {
    pub convention: String,
    pub present: super::PresenceSide,
}

fn interval_presence(old_empty: bool, new_empty: bool) -> Option<TextScopePresence> {
    let present = match (old_empty, new_empty) {
        (true, false) => super::PresenceSide::New,
        (false, true) => super::PresenceSide::Old,
        _ => return None,
    };
    Some(TextScopePresence {
        convention: "closed-native-interval-presence-v1".into(),
        present,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceBoundary {
    pub position: usize,
    pub origin: SpaceOrigin,
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceOrigin {
    LiteralGlyph,
    ReconstructedGap,
    LineSeparator,
    PageSeparator,
    Ambiguous,
}

fn spend(remaining: &mut usize, work: usize) -> Option<()> {
    *remaining = remaining.checked_sub(work)?;
    Some(())
}

/// A unique path is insufficient: it must cover every selected child of the
/// parent. Otherwise a detached paragraph could lie inside the proposed window.
/// Alternative partitions and shared physical sources require a different
/// closure proof and are deliberately not admitted by this convention.
fn closed_order<'a>(
    view: DocumentView<'a>,
    root: NodeId,
    limits: DocumentComparisonLimits,
    remaining: &mut usize,
) -> Result<Option<Vec<&'a GraphNode>>> {
    let graph = view.graph;
    if !graph.relations_complete
        || spend(
            remaining,
            graph.nodes.len().saturating_add(graph.edges.len()),
        )
        .is_none()
    {
        return Ok(None);
    }
    let members = source_children(graph, root, limits.matching.channels)?;
    let nodes: BTreeMap<_, _> = members.iter().map(|node| (node.id, *node)).collect();
    if nodes.len() < 3 {
        return Ok(None);
    }
    let mut sources = BTreeSet::new();
    let mut pages = BTreeSet::new();
    for node in &members {
        if node.basis != ViewBasis::SourceStructure
            || !matches!(node.content, NodeContent::Text { .. })
            || node.sources.is_empty()
            || spend(
                remaining,
                node.sources.len().saturating_add(node.pages.len()),
            )
            .is_none()
            || node.sources.iter().any(|source| !sources.insert(*source))
        {
            return Ok(None);
        }
        pages.extend(node.pages.iter().copied().map(Some));
        if node.pages.is_empty() {
            pages.insert(None);
        }
    }
    for page in pages {
        if spend(
            remaining,
            view.evidence
                .inventories
                .len()
                .saturating_add(view.evidence.issues.len()),
        )
        .is_none()
            || !view.evidence.inventory_complete(page, Channel::Text)
        {
            return Ok(None);
        }
    }
    for alternative in &graph.alternatives {
        for partition in &alternative.partitions {
            if spend(remaining, partition.len()).is_none()
                || partition.iter().any(|id| nodes.contains_key(id))
                || nodes.contains_key(&alternative.parent)
            {
                return Ok(None);
            }
        }
    }
    for conflict in &graph.source_conflicts {
        if spend(remaining, conflict.sources.len()).is_none()
            || conflict
                .sources
                .iter()
                .any(|source| sources.contains(source))
        {
            return Ok(None);
        }
    }
    let mut next = BTreeMap::new();
    let mut previous = BTreeMap::new();
    for edge in &graph.edges {
        if edge.kind != EdgeKind::Precedes
            || (!nodes.contains_key(&edge.from) && !nodes.contains_key(&edge.to))
        {
            continue;
        }
        if edge.basis.is_inferred()
            || !nodes.contains_key(&edge.from)
            || !nodes.contains_key(&edge.to)
            || next.insert(edge.from, edge.to).is_some()
            || previous.insert(edge.to, edge.from).is_some()
        {
            return Ok(None);
        }
    }
    let starts: Vec<_> = nodes
        .keys()
        .filter(|id| !previous.contains_key(id))
        .copied()
        .collect();
    let &[mut cursor] = starts.as_slice() else {
        return Ok(None);
    };
    let mut ordered = Vec::new();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(cursor) {
            return Ok(None);
        }
        ordered.push(nodes[&cursor]);
        let Some(successor) = next.get(&cursor) else {
            break;
        };
        cursor = *successor;
    }
    Ok((ordered.len() == nodes.len()).then_some(ordered))
}

pub(super) fn append(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
) -> Result<()> {
    let mut remaining = limits.matching.max_ownership_visits;
    append_pass(
        old,
        new,
        result,
        parent,
        limits,
        Discovery::Page,
        &mut remaining,
    )?;
    if remaining == 0 || !limits.matching.channels.text {
        return Ok(());
    }
    // Additional row discovery cannot spend the budget reserved for established
    // comparisons. Both passes share one cap and retain the earlier reviews.
    let prior = result.source_cut_search.take();
    let before = remaining;
    append_pass(
        old,
        new,
        result,
        parent,
        limits,
        Discovery::Rows,
        &mut remaining,
    )?;
    merge_cut_search(result, prior, before - remaining);
    if remaining != 0
        && [old, new].iter().any(|view| {
            view.evidence.native_structures.iter().any(|inventory| {
                inventory.complete
                    && inventory
                        .parents
                        .as_ref()
                        .is_some_and(|parents| !parents.is_empty())
            })
        })
    {
        let prior = result.source_cut_search.take();
        let before = remaining;
        append_pass(
            old,
            new,
            result,
            parent,
            limits,
            Discovery::NativeStructure,
            &mut remaining,
        )?;
        merge_cut_search(result, prior, before - remaining);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Discovery {
    Page,
    Rows,
    NativeStructure,
}

fn merge_cut_search(result: &mut ScopeViewComparison, prior: Option<SourceCutSearch>, work: usize) {
    if let Some(mut prior) = prior {
        if let Some(additional) = result.source_cut_search.take() {
            prior.exhaustive &= additional.exhaustive;
            prior.examined_fragments += additional.examined_fragments;
            prior.paired_boundaries += additional.paired_boundaries;
        } else {
            prior.exhaustive = false;
        }
        prior.work = prior.work.saturating_add(work);
        result.source_cut_search = Some(prior);
    }
}

fn append_pass(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    discovery: Discovery,
    remaining: &mut usize,
) -> Result<()> {
    let rows = matches!(discovery, Discovery::Rows);
    if !limits.matching.channels.text {
        return Ok(());
    }
    let scope = result.matching.scope;
    let (left, right, sources) = match (
        closed_order(old, scope.old, limits, remaining)?,
        closed_order(new, scope.new, limits, remaining)?,
    ) {
        (Some(left), Some(right)) => (vec![left], vec![right], None),
        _ => {
            let (Some(mut old_sources), Some(mut new_sources)) = (
                native::Sources::new(old, remaining),
                native::Sources::new(new, remaining),
            ) else {
                return Ok(());
            };
            if matches!(discovery, Discovery::NativeStructure) {
                old_sources.acquire_native_order(old, remaining);
                new_sources.acquire_native_order(new, remaining);
            }
            (
                native::runs(old, scope.old, &old_sources, limits, rows, remaining)?,
                native::runs(new, scope.new, &new_sources, limits, rows, remaining)?,
                Some((old_sources, new_sources)),
            )
        }
    };
    let native = sources.is_some();
    let positions = |runs: &[Vec<&GraphNode>]| {
        runs.iter()
            .enumerate()
            .flat_map(|(run, nodes)| {
                nodes
                    .iter()
                    .enumerate()
                    .map(move |(i, node)| (node.id, (run, i)))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let old_positions = positions(&left);
    let new_positions = positions(&right);
    let padding_boundaries: BTreeSet<_> = result
        .text_boundary_correspondences
        .iter()
        .filter_map(|index| {
            let proposal = &result.candidates.proposals[*index];
            match (proposal.old.as_slice(), proposal.new.as_slice()) {
                ([a], [b]) => Some((*a, *b)),
                _ => None,
            }
        })
        .collect();
    let unchanged: BTreeSet<_> = result
        .comparisons
        .iter()
        .filter(|comparison| comparison.compared && comparison.text_mask.is_some())
        .filter_map(
            |comparison| match (comparison.old.as_slice(), comparison.new.as_slice()) {
                ([a], [b]) if comparison.operation.is_none() => Some((*a, *b)),
                _ => None,
            },
        )
        .collect();
    let mut anchors = Vec::new();
    // Accepted comparisons have already discharged their own source-candidate
    // dependencies. Optional interior hypotheses cannot invalidate protected
    // higher-priority boundaries and do not identify edits inside this range.
    for &index in result
        .accepted_correspondences
        .iter()
        .chain(&result.text_boundary_correspondences)
    {
        let proposal = &result.candidates.proposals[index];
        let ([a], [b]) = (proposal.old.as_slice(), proposal.new.as_slice()) else {
            continue;
        };
        if !result.matching.source_only_mandatory.contains(&index)
            || result.matching.inferred_proposals.contains(&index)
            || (!unchanged.contains(&(*a, *b)) && !padding_boundaries.contains(&(*a, *b)))
        {
            continue;
        }
        if let (Some(&a), Some(&b)) = (old_positions.get(a), new_positions.get(b)) {
            anchors.push((a, b, index));
        }
    }
    anchors.sort_unstable();
    if rows || anchors.len() < 2 {
        for pass in [cuts::Pass::Standard, cuts::Pass::Paint] {
            let prior = result.source_cut_search.take();
            let before = *remaining;
            cuts::append(
                old,
                new,
                result,
                parent,
                limits,
                &left,
                &right,
                sources.as_ref(),
                &anchors,
                rows,
                pass,
                remaining,
            )?;
            merge_cut_search(result, prior, before - *remaining);
        }
        return Ok(());
    }
    let new_anchors: BTreeSet<_> = anchors.iter().map(|anchor| anchor.1).collect();
    // Preserve the existing two-sided recovery budget before attempting new
    // empty-sided claims. All stages still share the same finite work cap.
    for empty_sided in [false, true] {
        if empty_sided {
            cuts::append(
                old,
                new,
                result,
                parent,
                limits,
                &left,
                &right,
                sources.as_ref(),
                &anchors,
                rows,
                cuts::Pass::Standard,
                remaining,
            )?;
        }
        for pair in anchors.windows(2) {
            let [((ar0, a0), (br0, b0), first), ((ar1, a1), (br1, b1), last)] = pair else {
                unreachable!()
            };
            if ar0 != ar1
                || br0 != br1
                || a1 <= a0
                || b1 <= b0
                || (a1 == &(a0 + 1) && b1 == &(b0 + 1))
                || new_anchors
                    .range((*br0, b0 + 1)..(*br1, *b1))
                    .next()
                    .is_some()
            {
                continue;
            }
            let left = &left[*ar0];
            let right = &right[*br0];
            let a = &left[(a0 + 1)..*a1];
            let b = &right[(b0 + 1)..*b1];
            if (a.is_empty() || b.is_empty()) != empty_sided {
                continue;
            }
            // Empty intervals currently use the native source/paint closure profile.
            // The strict structured-group API requires two nonempty groups.
            if !native && (a.is_empty() || b.is_empty()) {
                continue;
            }
            if a.len() > limits.matching.max_group_nodes
                || b.len() > limits.matching.max_group_nodes
            {
                continue;
            }
            // The initial contract admits only the same declared text kind. It does
            // not silently interpret a semantic heading as a body paragraph.
            let Some(kind) = a.first().or_else(|| b.first()).map(|node| node.kind) else {
                continue;
            };
            if a.iter().chain(b).any(|node| node.kind != kind) {
                continue;
            }
            let padding_boundary = [*first, *last].into_iter().any(|index| {
                result.candidates.proposals[index].basis == ProposalBasis::LiteralContentWithPadding
            });
            let comparison = if native {
                super::operations::compare_native_text_range(a, b, limits.local)
            } else {
                compare_text_group_views(a, b, limits.local)
            };
            match comparison {
                Ok(mut comparison) if comparison.compared && comparison.operation.is_some() => {
                    // A failed normalization comparison cannot produce a review.
                    // Preserve the shared closure budget for admissible claims and
                    // later source-cut candidates; every emitted claim still needs
                    // the same complete source and paint checks.
                    let mut bounded_paint = false;
                    let mut native_regions = None;
                    if let Some((old_sources, new_sources)) = &sources {
                        let Some(old_closure) =
                            old_sources.closed(old, scope.old, &left[*a0..=*a1], remaining)
                        else {
                            continue;
                        };
                        let Some(new_closure) =
                            new_sources.closed(new, scope.new, &right[*b0..=*b1], remaining)
                        else {
                            continue;
                        };
                        bounded_paint = old_closure.bounded_paint() || new_closure.bounded_paint();
                        let (old, new) = (old_closure.chain(), new_closure.chain());
                        if old.is_some() || new.is_some() {
                            native_regions = Some(NativeRegionChains { old, new });
                        }
                    }
                    let spacing = if let Some((old_sources, new_sources)) = &sources {
                        let (Some(old_spacing), Some(new_spacing)) = (
                            old_sources.spacing(a, remaining),
                            new_sources.spacing(b, remaining),
                        ) else {
                            continue;
                        };
                        Some(TextScopeSpacing {
                            convention: "source-space-interpretations-v1".into(),
                            old: old_spacing,
                            new: new_spacing,
                        })
                    } else {
                        None
                    };
                    // A continuation outside this range can defeat apparent local
                    // truncation. Spacing uncertainty is handled by the source-side
                    // interpretation family, never by deleting literal spaces here.
                    if native
                        && !a.is_empty()
                        && !b.is_empty()
                        && let Some(TypedOperation::TextChanged {
                            old: Some(old_text),
                            new: Some(new_text),
                        }) = &comparison.operation
                        && (native::external_continuation(new, b, new_text, old_text, remaining)
                            != Some(false)
                            || native::external_continuation(old, a, old_text, new_text, remaining)
                                != Some(false))
                    {
                        continue;
                    }
                    if parent == InterpretationStatus::Inferred {
                        comparison.interpretation = InterpretationStatus::Inferred;
                    }
                    let review = TextScopeReview {
                        convention: if padding_boundary && bounded_paint {
                            "closed-native-paint-bounds-padding-interval-v1"
                        } else if padding_boundary {
                            "closed-native-padding-interval-v1"
                        } else if bounded_paint {
                            "closed-native-paint-bounds-interval-v1"
                        } else if native {
                            "closed-native-baseline-interval-v1"
                        } else {
                            "closed-retained-order-interval-v1"
                        }
                        .into(),
                        boundaries: vec![*first, *last],
                        source_cuts: None,
                        presence: interval_presence(a.is_empty(), b.is_empty()),
                        native_regions,
                        old_sources: a
                            .iter()
                            .flat_map(|node| node.sources.iter().copied())
                            .collect(),
                        new_sources: b
                            .iter()
                            .flat_map(|node| node.sources.iter().copied())
                            .collect(),
                        old_boundaries: [left[*a0].sources.clone(), left[*a1].sources.clone()],
                        new_boundaries: [right[*b0].sources.clone(), right[*b1].sources.clone()],
                        candidate_search_exhaustive: Some(result.candidates.exhaustive),
                        spacing,
                        comparison,
                    };
                    if empty_sided {
                        let work = result.text_scope_reviews.len().saturating_mul(
                            review
                                .old_sources
                                .len()
                                .saturating_add(review.new_sources.len())
                                .saturating_add(1),
                        );
                        if spend(remaining, work).is_none()
                            || result.text_scope_reviews.iter().any(|previous| {
                                previous.old_sources == review.old_sources
                                    && previous.new_sources == review.new_sources
                            })
                        {
                            continue;
                        }
                    }
                    result.text_scope_reviews.push(review);
                }
                Ok(_) | Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }
    if sources.is_some()
        && result
            .text_scope_reviews
            .iter()
            .any(|review| review.source_cuts.is_none())
    {
        // A raw cut certificate can support finer literal-space edges even
        // when a whole-node review already covers the same source interval.
        // Reuse the acquired sources and defer this work until prior reviews
        // are complete, under the same remaining budget.
        let prior = result.source_cut_search.take();
        let before = *remaining;
        cuts::append(
            old,
            new,
            result,
            parent,
            limits,
            &left,
            &right,
            sources.as_ref(),
            &anchors,
            false,
            cuts::Pass::RefineExisting,
            remaining,
        )?;
        merge_cut_search(result, prior, before - *remaining);
    }
    // Existing whole-node and content-edge reviews retain priority. Additional
    // paint-order closure shares their source index and remaining work cap.
    let prior = result.source_cut_search.take();
    let before = *remaining;
    cuts::append(
        old,
        new,
        result,
        parent,
        limits,
        &left,
        &right,
        sources.as_ref(),
        &anchors,
        rows,
        cuts::Pass::Paint,
        remaining,
    )?;
    merge_cut_search(result, prior, before - *remaining);

    Ok(())
}
