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
pub(super) mod inferred;
mod native;
pub use native::{NativeRegion, NativeRegionChain, NativeRegionChains, NativeTransition};

pub use cuts::{
    CutCorrespondence, CutEvidence, SourceCut, SourceCutBoundaryPadding, SourceCutEdgeRefinement,
    SourceCutPageEdge, SourceCutPopulation, SourceCutRange, SourceCutRowEndpoint,
    SourceCutRowOrder, SourceCutSearch, SourceFragment,
};

mod domains;
pub use domains::NativeTextDomainEquality;
mod intervals;
pub use intervals::NativeTextIntervalComparison;

/// Content of corresponding intervals under the stated comparison convention.
/// Inferred paragraph groups have no certified interval boundaries. Otherwise,
/// the enclosing scope retains the parent correspondence. Boundary indexes
/// refer to its candidate proposals; member nodes retain all source projections.
/// Neither this unit nor its conditional masks owns changed sources or discharges
/// strict coverage. An inferred parent keeps this comparison inferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopeReview {
    pub convention: String,
    /// Whole-node or contiguous-group proposal indexes. Every member of each
    /// boundary group is excluded from the interior. Source-cut ranges carry
    /// their two explicit certificates in `source_cuts` instead of inventing proposal IDs.
    /// Inferred paragraph groups leave this empty and certify no boundaries.
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
    native: (
        &super::evidence::NativeIndex<'_>,
        &super::evidence::NativeIndex<'_>,
    ),
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
) -> Result<()> {
    domains::append(old, new, native, result, parent, limits)?;
    let mut remaining = limits.matching.max_ownership_visits;
    let mut equal = Vec::new();
    append_pass(
        (old, new),
        native,
        result,
        parent,
        limits,
        (Discovery::Page, &mut equal),
        &mut remaining,
    )?;
    if remaining == 0 || !limits.matching.channels.text {
        // Optional cut discovery cannot cancel the independently bounded proof
        // of a range already found. Its incomplete search status is retained.
        return intervals::append(old, new, native, result, parent, limits, &equal);
    }
    // Additional row discovery cannot spend the budget reserved for established
    // comparisons. Both passes share one cap and retain the earlier reviews.
    let prior = result.source_cut_search.take();
    let before = remaining;
    append_pass(
        (old, new),
        native,
        result,
        parent,
        limits,
        (Discovery::Rows, &mut equal),
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
            (old, new),
            native,
            result,
            parent,
            limits,
            (Discovery::NativeStructure, &mut equal),
            &mut remaining,
        )?;
        merge_cut_search(result, prior, before - remaining);
    }
    intervals::append(old, new, native, result, parent, limits, &equal)
}

#[derive(Clone, Copy)]
enum Discovery {
    Page,
    Rows,
    NativeStructure,
}

// The borrowed nodes keep each proof bound to one immutable document pair and
// ordered path. Node IDs alone cannot identify sliced or projected views.
struct NativeClosureProof<'a> {
    old: Vec<&'a GraphNode>,
    new: Vec<&'a GraphNode>,
    bounded_paint: [bool; 2],
}

type NativeProofs<'a> = BTreeMap<[usize; 2], NativeClosureProof<'a>>;

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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BoundarySpan {
    run: usize,
    first: usize,
    last: usize,
}

impl BoundarySpan {
    fn contiguous(nodes: &[NodeId], positions: &BTreeMap<NodeId, (usize, usize)>) -> Option<Self> {
        let &(run, first) = positions.get(nodes.first()?)?;
        for (offset, node) in nodes.iter().enumerate() {
            if positions.get(node) != Some(&(run, first.checked_add(offset)?)) {
                return None;
            }
        }
        Some(Self {
            run,
            first,
            last: first.checked_add(nodes.len() - 1)?,
        })
    }

    fn len(self) -> usize {
        self.last - self.first + 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct IntervalAnchor {
    old: BoundarySpan,
    new: BoundarySpan,
    proposal: usize,
}

impl IntervalAnchor {
    fn singleton(self) -> bool {
        self.old.len() == 1 && self.new.len() == 1
    }
}

fn append_pass(
    (old, new): (DocumentView<'_>, DocumentView<'_>),
    native: (
        &super::evidence::NativeIndex<'_>,
        &super::evidence::NativeIndex<'_>,
    ),
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    (discovery, equal): (Discovery, &mut Vec<intervals::EqualCandidate>),
    remaining: &mut usize,
) -> Result<()> {
    let rows = matches!(discovery, Discovery::Rows);
    if !limits.matching.channels.text {
        return Ok(());
    }
    let scope = result.matching.scope;
    let (left, right, sources) = if let (Some(left), Some(right)) = (
        closed_order(old, scope.old, limits, remaining)?,
        closed_order(new, scope.new, limits, remaining)?,
    ) {
        (vec![left], vec![right], None)
    } else {
        let mut old_sources = native::Sources::new(native.0);
        let mut new_sources = native::Sources::new(native.1);
        if matches!(discovery, Discovery::NativeStructure) {
            old_sources.acquire_native_order(old, remaining);
            new_sources.acquire_native_order(new, remaining);
        }
        let mut boundaries = [BTreeSet::new(), BTreeSet::new()];
        for &index in result
            .accepted_correspondences
            .iter()
            .chain(&result.text_boundary_correspondences)
        {
            let proposal = &result.candidates.proposals[index];
            if spend(
                remaining,
                proposal.old.len().saturating_add(proposal.new.len()),
            )
            .is_none()
            {
                return Ok(());
            }
            boundaries[0].extend(&proposal.old);
            boundaries[1].extend(&proposal.new);
        }
        (
            native::runs(
                old,
                scope.old,
                &old_sources,
                limits,
                rows,
                &boundaries[0],
                remaining,
            )?,
            native::runs(
                new,
                scope.new,
                &new_sources,
                limits,
                rows,
                &boundaries[1],
                remaining,
            )?,
            Some((old_sources, new_sources)),
        )
    };
    let native = sources.is_some();
    let mut native_proofs = NativeProofs::new();
    let mut plain_censuses = NativeProofs::new();
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
        .filter(|comparison| {
            comparison.compared && comparison.text_mask.is_some() && comparison.operation.is_none()
        })
        .map(|comparison| (comparison.old.as_slice(), comparison.new.as_slice()))
        .collect();
    let mut anchors = Vec::new();
    let mut interval_anchors = Vec::new();
    // Accepted comparisons have already discharged their own source-candidate
    // dependencies. Optional interior hypotheses cannot invalidate protected
    // higher-priority boundaries and do not identify edits inside this range.
    for &index in result
        .accepted_correspondences
        .iter()
        .chain(&result.text_boundary_correspondences)
    {
        let proposal = &result.candidates.proposals[index];
        let unchanged = unchanged.contains(&(proposal.old.as_slice(), proposal.new.as_slice()));
        let padded = match (proposal.old.as_slice(), proposal.new.as_slice()) {
            ([a], [b]) => padding_boundaries.contains(&(*a, *b)),
            _ => false,
        };
        if !result.matching.source_only_mandatory.contains(&index)
            || result.matching.inferred_proposals.contains(&index)
            || (!unchanged && !padded)
        {
            continue;
        }
        let grouped = proposal.old.len() != 1 || proposal.new.len() != 1;
        if grouped
            && spend(
                remaining,
                proposal
                    .old
                    .len()
                    .saturating_add(proposal.new.len())
                    .saturating_mul(
                        old_positions
                            .len()
                            .saturating_add(new_positions.len())
                            .saturating_add(1)
                            .ilog2() as usize
                            + 1,
                    ),
            )
            .is_none()
        {
            continue;
        }
        let (Some(a), Some(b)) = (
            BoundarySpan::contiguous(&proposal.old, &old_positions),
            BoundarySpan::contiguous(&proposal.new, &new_positions),
        ) else {
            continue;
        };
        let anchor = IntervalAnchor {
            old: a,
            new: b,
            proposal: index,
        };
        if anchor.singleton() {
            anchors.push(((a.run, a.first), (b.run, b.first), index));
        }
        // A group boundary contributes its entire contiguous source span.
        // Interior ranges start after the entry group and end before the exit
        // group; neither endpoint is replaced by one arbitrarily chosen node.
        interval_anchors.push(anchor);
    }
    anchors.sort_unstable();
    interval_anchors.sort_unstable();
    if rows || interval_anchors.len() < 2 {
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
                &native_proofs,
                &mut plain_censuses,
                &anchors,
                rows,
                pass,
                remaining,
            )?;
            merge_cut_search(result, prior, before - *remaining);
        }
        return Ok(());
    }
    let singleton_anchors: Vec<_> = interval_anchors
        .iter()
        .copied()
        .filter(|anchor| anchor.singleton())
        .collect();
    let new_singleton_anchors: BTreeSet<_> = anchors.iter().map(|anchor| anchor.1).collect();
    let new_anchors: BTreeSet<_> = interval_anchors
        .iter()
        .flat_map(|anchor| {
            (anchor.new.first..=anchor.new.last).map(|index| (anchor.new.run, index))
        })
        .collect();
    // Established whole-node and raw-interval discovery precede narrower
    // component views. Whole-node presence gets its turn before finer source
    // boundaries; it uses the same accepted anchors and full closure checks.
    // In particular, a margin-free view must not consume the
    // budget needed by an existing full paint-order range.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum IntervalPass {
        Whole,
        Component,
        Empty,
        Refine,
        DeferredComponent,
        EmptyComponent,
    }
    let mut defer_components = false;
    for pass in [
        IntervalPass::Whole,
        IntervalPass::Component,
        IntervalPass::Empty,
        IntervalPass::Refine,
        IntervalPass::DeferredComponent,
        IntervalPass::EmptyComponent,
    ] {
        let empty_sided = matches!(pass, IntervalPass::Empty | IntervalPass::EmptyComponent);
        let inset = matches!(
            pass,
            IntervalPass::Component
                | IntervalPass::DeferredComponent
                | IntervalPass::EmptyComponent
        );
        if pass == IntervalPass::DeferredComponent && !defer_components {
            continue;
        }
        if pass == IntervalPass::Component {
            // A global normalization failure need not invalidate the retained
            // raw interval. Recheck its source projection and closure before
            // lexical searches spend the shared budget on finer boundaries.
            // Preserve three quarters for established finer ranges: a coarse
            // recovery must not consume their entire discovery budget.
            let prior = result.source_cut_search.take();
            let before = *remaining;
            let allowance = before / 4;
            let mut raw_remaining = allowance;
            cuts::append(
                old,
                new,
                result,
                parent,
                limits,
                &left,
                &right,
                sources.as_ref(),
                &native_proofs,
                &mut plain_censuses,
                &anchors,
                rows,
                cuts::Pass::WholeIntervals,
                &mut raw_remaining,
            )?;
            *remaining -= allowance - raw_remaining;
            if let Some(search) = &mut result.source_cut_search {
                search.budget_at_entry = before;
            }
            merge_cut_search(result, prior, before - *remaining);
            // With no whole-node review, the established next step is general
            // source-cut discovery. A new component must not insert an expensive
            // refinement stage ahead of those previously reachable ranges.
            defer_components = !result
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_none());
            if defer_components {
                continue;
            }
        }
        if (pass == IntervalPass::Refine
            || (pass == IntervalPass::EmptyComponent && defer_components))
            && sources.is_some()
            && result
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_none())
        {
            // A raw cut certificate can support finer literal-space edges even
            // when a whole-node review already covers the same source interval.
            // Reuse the acquired sources after all nonempty whole-node reviews,
            // before new boundary searches spend their shared remaining budget.
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
                &native_proofs,
                &mut plain_censuses,
                &anchors,
                false,
                cuts::Pass::RefineExisting,
                remaining,
            )?;
            merge_cut_search(result, prior, before - *remaining);
        }
        if pass == IntervalPass::Refine {
            // Unmasked raw intervals need a turn before general page and
            // lexical discovery exhausts the remaining work. Keep half of that
            // work for those established searches; the total cap is unchanged.
            let prior = result.source_cut_search.take();
            let before = *remaining;
            let allowance = before / 2;
            let mut enclosing_remaining = allowance;
            cuts::append(
                old,
                new,
                result,
                parent,
                limits,
                &left,
                &right,
                sources.as_ref(),
                &native_proofs,
                &mut plain_censuses,
                &anchors,
                rows,
                cuts::Pass::EnclosingIntervals,
                &mut enclosing_remaining,
            )?;
            *remaining -= allowance - enclosing_remaining;
            if let Some(search) = &mut result.source_cut_search {
                search.budget_at_entry = before;
            }
            merge_cut_search(result, prior, before - *remaining);
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
                &native_proofs,
                &mut plain_censuses,
                &anchors,
                rows,
                cuts::Pass::Standard,
                remaining,
            )?;
            merge_cut_search(result, prior, before - *remaining);
        }
        if pass == IntervalPass::Refine {
            continue;
        }
        // Keep established singleton-bounded extents before adding ranges
        // around group boundaries. An equal interior group does not invalidate
        // the wider non-owning comparison or justify dropping its context.
        let mut candidates = singleton_anchors
            .windows(2)
            .chain(
                interval_anchors
                    .windows(2)
                    .filter(|pair| !pair[0].singleton() || !pair[1].singleton()),
            )
            .map(|pair| (pair[0], pair[1], None));
        let mut deferred = std::collections::VecDeque::new();
        while let Some((entry, exit, pending)) = candidates.next().or_else(|| deferred.pop_front())
        {
            let count_fallback = pending.is_some();
            let (ar0, a0, br0, b0) = (
                &entry.old.run,
                &entry.old.last,
                &entry.new.run,
                &entry.new.last,
            );
            let (ar1, a1, br1, b1) = (
                &exit.old.run,
                &exit.old.first,
                &exit.new.run,
                &exit.new.first,
            );
            let (first, last) = (&entry.proposal, &exit.proposal);
            let singleton = entry.singleton() && exit.singleton();
            // Component lanes trim individual endpoints; a group boundary
            // needs its complete span and uses the whole-interval path.
            if inset && !singleton {
                continue;
            }
            let protected = if singleton {
                &new_singleton_anchors
            } else {
                &new_anchors
            };
            if ar0 != ar1
                || br0 != br1
                || a1 <= a0
                || b1 <= b0
                || (a1 == &(a0 + 1) && b1 == &(b0 + 1))
                || protected
                    .range((*br0, b0 + 1)..(*br1, *b1))
                    .next()
                    .is_some()
            {
                continue;
            }
            let left = &left[*ar0];
            let right = &right[*br0];
            let mut old_lane = Vec::new();
            let mut new_lane = Vec::new();
            let original_old = &left[entry.old.first..=exit.old.last];
            let original_new = &right[entry.new.first..=exit.new.last];
            if inset {
                if spend(remaining, result.text_scope_reviews.len()).is_none() {
                    break;
                }
                if result.text_scope_reviews.iter().any(|review| {
                    review.source_cuts.is_none() && review.boundaries == [*first, *last]
                }) {
                    continue;
                }
                let Some((old_sources, new_sources)) = &sources else {
                    break;
                };
                let (Some(a), Some(b)) = (
                    old_sources.boundary_lane(original_old, remaining),
                    new_sources.boundary_lane(original_new, remaining),
                ) else {
                    continue;
                };
                if a.len() == original_old.len() && b.len() == original_new.len() {
                    continue;
                }
                old_lane = a;
                new_lane = b;
            }
            let old_path = if inset { &old_lane } else { original_old };
            let new_path = if inset { &new_lane } else { original_new };
            let a = &old_path[entry.old.len()..old_path.len() - exit.old.len()];
            let b = &new_path[entry.new.len()..new_path.len() - exit.new.len()];
            if !singleton {
                let work = result
                    .text_scope_reviews
                    .iter()
                    .fold(0_usize, |work, review| {
                        work.saturating_add(1)
                            .saturating_add(a.len().saturating_mul(review.comparison.old.len()))
                            .saturating_add(b.len().saturating_mul(review.comparison.new.len()))
                    });
                if spend(remaining, work).is_none() {
                    continue;
                }
                // Whole-node reviews retain every source of their member
                // nodes. Rechecking an already covered subrange adds no source
                // coverage; preserve that work for newly bounded content.
                if result.text_scope_reviews.iter().any(|review| {
                    review.source_cuts.is_none()
                        && review.boundaries.len() == 2
                        && review.comparison.compared
                        && review.comparison.interpretation
                            == InterpretationStatus::ConditionalOnCorrespondence
                        && review.presence == interval_presence(a.is_empty(), b.is_empty())
                        && a.iter()
                            .all(|node| review.comparison.old.contains(&node.id))
                        && b.iter()
                            .all(|node| review.comparison.new.contains(&node.id))
                }) {
                    continue;
                }
            }
            // A reconstructed component is ready for the same closure checks
            // whether one interior is empty or both contain text. Deferring an
            // empty component would repeat this work after finer searches.
            if (a.is_empty() || b.is_empty()) != empty_sided && pass != IntervalPass::Component {
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
            // Both ordered masks and count fallbacks must retain the same
            // source-side hyphen interpretations. Token addresses stay fixed.
            // Normalization shares the local proof budget with the diff kernel.
            let mut local_limits = limits.local;
            let mut normalization_work = local_limits.proof_work;
            let normalized;
            let normalized_refs;
            let (a, b) = if let Some((old_sources, new_sources)) = &sources {
                let (Some(old), Some(new)) = (
                    old_sources.hyphen_alternatives(a, &mut normalization_work),
                    new_sources.hyphen_alternatives(b, &mut normalization_work),
                ) else {
                    continue;
                };
                normalized = (old, new);
                local_limits.proof_work = normalization_work;
                normalized_refs = (
                    normalized
                        .0
                        .iter()
                        .map(std::convert::AsRef::as_ref)
                        .collect::<Vec<_>>(),
                    normalized
                        .1
                        .iter()
                        .map(std::convert::AsRef::as_ref)
                        .collect::<Vec<_>>(),
                );
                (normalized_refs.0.as_slice(), normalized_refs.1.as_slice())
            } else {
                (a, b)
            };
            let comparison = if let Some(comparison) = pending {
                Ok(comparison)
            } else if native {
                super::operations::compare_native_text_range(a, b, local_limits)
            } else {
                compare_text_group_views(a, b, limits.local)
            };
            match comparison {
                Ok(mut comparison) if comparison.compared && comparison.operation.is_some() => {
                    if !singleton && let Some(sources) = &sources {
                        if count_fallback {
                            if !use_native_count(&mut comparison, (a, b), limits.local, remaining)?
                            {
                                continue;
                            }
                        } else if comparison.text_mask.is_some()
                            && !native_order_valid(sources, (a, b), remaining)
                        {
                            // Keep ordered changes ahead of count fallbacks
                            // for interleaved layout text. Retained payloads
                            // and both proof routes share the same work cap.
                            let work = a.iter().chain(b).fold(
                                a.len().saturating_add(b.len()),
                                |work, node| {
                                    let tokens = match &node.content {
                                        NodeContent::Text { view } => view.tokens.len(),
                                        _ => 0,
                                    };
                                    work.saturating_add(tokens)
                                        .saturating_add(node.sources.len())
                                },
                            );
                            if spend(remaining, work).is_some() {
                                deferred.push_back((entry, exit, Some(comparison)));
                            }
                            continue;
                        }
                    }
                    // A failed normalization comparison cannot produce a review.
                    // Preserve the shared closure budget for admissible claims and
                    // later source-cut candidates; every emitted claim still needs
                    // the same complete source and paint checks.
                    let mut bounded_paint = false;
                    let mut native_regions = None;
                    let mut reusable_closure = None;
                    if let Some((old_sources, new_sources)) = &sources {
                        let Some(old_closure) =
                            old_sources.closed(old, scope.old, old_path, remaining)
                        else {
                            continue;
                        };
                        let Some(new_closure) =
                            new_sources.closed(new, scope.new, new_path, remaining)
                        else {
                            continue;
                        };
                        bounded_paint = old_closure.bounded_paint() || new_closure.bounded_paint();
                        // Segmented proofs carry an owned transition chain;
                        // leave their reconstruction on the existing path.
                        if !matches!(old_closure, native::Closure::Segmented(_))
                            && !matches!(new_closure, native::Closure::Segmented(_))
                        {
                            reusable_closure =
                                Some([old_closure.bounded_paint(), new_closure.bounded_paint()]);
                        }
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
                    let mut review = TextScopeReview {
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
                        old_boundaries: [entry.old, exit.old].map(|span| {
                            left[span.first..=span.last]
                                .iter()
                                .flat_map(|node| node.sources.iter().copied())
                                .collect()
                        }),
                        new_boundaries: [entry.new, exit.new].map(|span| {
                            right[span.first..=span.last]
                                .iter()
                                .flat_map(|node| node.sources.iter().copied())
                                .collect()
                        }),
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
                    // Source-cut discovery addresses singleton endpoints, so
                    // it cannot reuse a group span as a singleton proof.
                    if singleton && let Some(bounded_paint) = reusable_closure {
                        let work = old_path
                            .len()
                            .saturating_add(new_path.len())
                            .saturating_add(
                                native_proofs.len().saturating_add(1).ilog2() as usize + 1,
                            );
                        if spend(remaining, work).is_some() {
                            native_proofs.insert(
                                [*first, *last],
                                NativeClosureProof {
                                    old: old_path.to_vec(),
                                    new: new_path.to_vec(),
                                    bounded_paint,
                                },
                            );
                        }
                    }
                    if singleton
                        && let Some(sources) = &sources
                        && !validate_native_order(
                            &mut review.comparison,
                            sources,
                            (a, b),
                            limits.local,
                            remaining,
                        )?
                    {
                        continue;
                    }
                    result.text_scope_reviews.push(review);
                }
                Ok(comparison)
                    if native
                        && parent == InterpretationStatus::ConditionalOnCorrespondence
                        && comparison.compared
                        && comparison.operation.is_none()
                        && comparison.unresolved.is_empty()
                        && comparison
                            .text_mask
                            .as_ref()
                            .is_some_and(|mask| mask.claims.changed_source_upper == 0) =>
                {
                    // Retain only a locator. The ownership pass reconstructs and
                    // revalidates native sources; no equal B event is published.
                    if spend(
                        remaining,
                        comparison
                            .old
                            .len()
                            .saturating_add(comparison.new.len())
                            .saturating_add(1),
                    )
                    .is_some()
                    {
                        equal.push(intervals::EqualCandidate {
                            boundaries: [*first, *last],
                            old: comparison.old,
                            new: comparison.new,
                        });
                    }
                }
                Ok(_) | Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {}
                Err(error) => return Err(error),
            }
        }
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
        &native_proofs,
        &mut plain_censuses,
        &anchors,
        rows,
        cuts::Pass::Paint,
        remaining,
    )?;
    merge_cut_search(result, prior, before - *remaining);

    Ok(())
}

/// A reconstructed token order cannot supply an exact mask without a checked
/// source projection. Preserve an order-independent count witness when possible.
fn validate_native_order(
    comparison: &mut LocalViewComparison,
    sources: &(native::Sources<'_>, native::Sources<'_>),
    nodes: (&[&GraphNode], &[&GraphNode]),
    limits: super::LocalComparisonLimits,
    remaining: &mut usize,
) -> Result<bool> {
    if comparison.text_mask.is_none() || native_order_valid(sources, nodes, remaining) {
        return Ok(true);
    }
    use_native_count(comparison, nodes, limits, remaining)
}

fn native_order_valid(
    (old_sources, new_sources): &(native::Sources<'_>, native::Sources<'_>),
    (old, new): (&[&GraphNode], &[&GraphNode]),
    remaining: &mut usize,
) -> bool {
    [(old_sources, old), (new_sources, new)]
        .into_iter()
        .all(|(sources, nodes)| {
            nodes.iter().all(|node| {
                sources
                    .project_census(node, &[], false, remaining)
                    .is_some()
            })
        })
}

fn use_native_count(
    comparison: &mut LocalViewComparison,
    (old, new): (&[&GraphNode], &[&GraphNode]),
    limits: super::LocalComparisonLimits,
    remaining: &mut usize,
) -> Result<bool> {
    let Some(proof) = super::operations::native_count_change(old, new, limits, remaining)? else {
        return Ok(false);
    };
    comparison.text_mask = None;
    comparison.text_change_proof = Some(proof);
    comparison.unresolved.push(
        "native token order is unresolved; source multiplicity proves change without a mask".into(),
    );
    Ok(true)
}
