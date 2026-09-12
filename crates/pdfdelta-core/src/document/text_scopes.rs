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

mod native;

/// Content of corresponding intervals under the stated comparison convention.
/// The enclosing scope retains the parent correspondence. Boundary indexes
/// refer to its candidate proposals; member nodes retain all source projections.
/// Neither this unit nor its conditional masks owns changed sources or discharges
/// strict coverage. An inferred parent keeps this comparison inferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopeReview {
    pub convention: String,
    pub boundaries: [usize; 2],
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
    pub comparison: LocalViewComparison,
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
    if !limits.matching.channels.text {
        return Ok(());
    }
    let mut remaining = limits.matching.max_ownership_visits;
    let scope = result.matching.scope;
    let (left, right, sources) = match (
        closed_order(old, scope.old, limits, &mut remaining)?,
        closed_order(new, scope.new, limits, &mut remaining)?,
    ) {
        (Some(left), Some(right)) => (vec![left], vec![right], None),
        _ => {
            let (Some(old_sources), Some(new_sources)) = (
                native::Sources::new(old, &mut remaining),
                native::Sources::new(new, &mut remaining),
            ) else {
                return Ok(());
            };
            (
                native::runs(old, scope.old, &old_sources, limits, &mut remaining)?,
                native::runs(new, scope.new, &new_sources, limits, &mut remaining)?,
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
    if anchors.len() < 2 {
        return Ok(());
    }
    let new_anchors: BTreeSet<_> = anchors.iter().map(|anchor| anchor.1).collect();
    for pair in anchors.windows(2) {
        let [((ar0, a0), (br0, b0), first), ((ar1, a1), (br1, b1), last)] = pair else {
            unreachable!()
        };
        if ar0 != ar1
            || br0 != br1
            || a1 <= &(a0 + 1)
            || b1 <= &(b0 + 1)
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
        if a.len() > limits.matching.max_group_nodes || b.len() > limits.matching.max_group_nodes {
            continue;
        }
        // The initial contract admits only the same declared text kind. It does
        // not silently interpret a semantic heading as a body paragraph.
        if a.iter().chain(b).any(|node| node.kind != a[0].kind) {
            continue;
        }
        let mut bounded_paint = false;
        if let Some((old_sources, new_sources)) = &sources {
            let Some(old_closure) =
                old_sources.closed(old, scope.old, &left[*a0..=*a1], &mut remaining)
            else {
                continue;
            };
            let Some(new_closure) =
                new_sources.closed(new, scope.new, &right[*b0..=*b1], &mut remaining)
            else {
                continue;
            };
            bounded_paint = old_closure == native::Closure::BoundedPaint
                || new_closure == native::Closure::BoundedPaint;
        }
        let padding_boundary = [*first, *last].into_iter().any(|index| {
            result.candidates.proposals[index].basis == ProposalBasis::LiteralContentWithPadding
        });
        match compare_text_group_views(a, b, limits.local) {
            Ok(mut comparison) if comparison.compared && comparison.operation.is_some() => {
                // Native word spaces can be reconstructed from geometry on one
                // side and explicit glyphs on the other. Such differences alone
                // do not justify a content review; retain the uncompared interval
                // without changing its text, masks, or strict source coverage.
                if native
                    && let Some(TypedOperation::TextChanged {
                        old: Some(old_text),
                        new: Some(new_text),
                    }) = &comparison.operation
                    && (old_text
                        .chars()
                        .filter(|scalar| *scalar != ' ')
                        .eq(new_text.chars().filter(|scalar| *scalar != ' '))
                        || native::external_continuation(
                            new,
                            b,
                            new_text,
                            old_text,
                            &mut remaining,
                        ) != Some(false)
                        || native::external_continuation(
                            old,
                            a,
                            old_text,
                            new_text,
                            &mut remaining,
                        ) != Some(false))
                {
                    continue;
                }
                if parent == InterpretationStatus::Inferred {
                    comparison.interpretation = InterpretationStatus::Inferred;
                }
                result.text_scope_reviews.push(TextScopeReview {
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
                    boundaries: [*first, *last],
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
                    comparison,
                });
            }
            Ok(_) | Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
