//! A complete native page can close a local search around one accepted anchor.
//! Page indices alone never establish the correspondence between two pages.

use super::{
    Anchor, BTreeMap, BTreeSet, Channel, CutMaps, DocumentComparisonLimits, DocumentView,
    GraphNode, InterpretationStatus, NodeId, OriginalCuts, Result, ScopeViewComparison,
    SourceCutPageEdge, SourceCutPopulation, compare_population, native, spend,
};
use crate::model::PageId;

fn paths<'a>(runs: &[Vec<&'a GraphNode>]) -> BTreeMap<PageId, Vec<&'a GraphNode>> {
    let mut paths = BTreeMap::<_, Vec<_>>::new();
    let mut split = BTreeSet::new();
    for run in runs {
        let mut previous = None;
        for &node in run {
            let [page] = node.pages.as_slice() else {
                previous = None;
                continue;
            };
            if previous != Some(*page) && paths.contains_key(page) {
                split.insert(*page);
            }
            paths.entry(*page).or_default().push(node);
            previous = Some(*page);
        }
    }
    paths.retain(|page, _| !split.contains(page));
    paths
}

fn project(
    view: DocumentView<'_>,
    root: NodeId,
    page: PageId,
    path: &[&GraphNode],
    sources: &native::Sources<'_>,
    remaining: &mut usize,
) -> Option<(Vec<GraphNode>, OriginalCuts)> {
    if !view.evidence.inventory_complete(Some(page), Channel::Text) {
        return None;
    }
    let mut nodes = Vec::new();
    let mut maps = OriginalCuts::new();
    let mut population = BTreeSet::new();
    for node in path {
        spend(remaining, node.sources.len())?;
        if node
            .sources
            .iter()
            .any(|source| !population.insert(*source))
        {
            return None;
        }
        let (projected, map) = sources.project_census(node, &[], false, remaining)?;
        if let Some(map) = map {
            maps.insert(node.id, map);
        }
        nodes.push(projected);
    }
    if !sources.page_sources_match(page, &population, remaining)? {
        return None;
    }
    // Check all source, partition and paint dependencies before a substring can
    // inherit closure. A matching heading does not discharge any of them.
    sources.closed(view, root, path, remaining)?;
    Some((nodes, maps))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    left: &[Vec<&GraphNode>],
    right: &[Vec<&GraphNode>],
    sources: Option<&(native::Sources<'_>, native::Sources<'_>)>,
    anchors: &[Anchor],
    outer_edges: bool,
    remaining: &mut usize,
) -> Result<()> {
    let Some((old_sources, new_sources)) = sources else {
        return Ok(());
    };
    if old.evidence.pages.len() == 1 && new.evidence.pages.len() == 1 {
        return Ok(());
    }
    let count: usize = left.iter().chain(right).map(Vec::len).sum();
    if spend(
        remaining,
        count.saturating_add(anchors.len().saturating_mul(4)),
    )
    .is_none()
    {
        return Ok(());
    }
    let (old_paths, new_paths) = (paths(left), paths(right));
    let mut pairs = BTreeMap::<_, Vec<_>>::new();
    for &(a, b, proposal) in anchors {
        let (old_node, new_node) = (left[a.0][a.1], right[b.0][b.1]);
        let ([old_page], [new_page]) = (old_node.pages.as_slice(), new_node.pages.as_slice())
        else {
            continue;
        };
        pairs
            .entry((*old_page, *new_page))
            .or_default()
            .push((old_node.id, new_node.id, proposal));
    }
    for ((a, b), paired_anchors) in pairs {
        if (paired_anchors.len() > 1) != outer_edges {
            continue;
        }
        let (Some(old_path), Some(new_path)) = (old_paths.get(&a), new_paths.get(&b)) else {
            continue;
        };
        let (Some((old_nodes, old_maps)), Some((new_nodes, new_maps))) = (
            project(
                old,
                result.matching.scope.old,
                a,
                old_path,
                old_sources,
                remaining,
            ),
            project(
                new,
                result.matching.scope.new,
                b,
                new_path,
                new_sources,
                remaining,
            ),
        ) else {
            continue;
        };
        if spend(remaining, old_nodes.len().saturating_add(new_nodes.len())).is_none() {
            break;
        }
        let old_positions: BTreeMap<_, _> = old_nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        let new_positions: BTreeMap<_, _> = new_nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        let page_anchors: Vec<_> = paired_anchors
            .iter()
            .filter_map(|&(a, b, p)| {
                Some((
                    (0, *old_positions.get(&a)?),
                    (0, *new_positions.get(&b)?),
                    p,
                ))
            })
            .collect();
        let mut choices = Vec::new();
        if page_anchors.len() > 1
            && spend(remaining, page_anchors.len().saturating_mul(4)).is_none()
        {
            break;
        }
        let local_boundaries: BTreeSet<_> = page_anchors.iter().map(|anchor| anchor.2).collect();
        if page_anchors.len() == 1 {
            choices.push((page_anchors[0], None));
        } else {
            // Both source paths must agree on their outermost anchor. An
            // interior interval retains its existing continuation guards.
            for (edge, reverse) in [
                (SourceCutPageEdge::After, true),
                (SourceCutPageEdge::Before, false),
            ] {
                let select = |side: bool| {
                    page_anchors
                        .iter()
                        .min_by_key(|&&(a, b, _)| {
                            let position = if side { b.1 } else { a.1 };
                            if reverse {
                                usize::MAX - position
                            } else {
                                position
                            }
                        })
                        .copied()
                };
                if let (Some(a), Some(b)) = (select(false), select(true))
                    && a == b
                {
                    choices.push((a, Some(edge)));
                }
            }
        }
        let maps = CutMaps {
            old: old_maps,
            new: new_maps,
        };
        for (anchor, edge) in choices {
            let boundary = anchor.2;
            let accepted: BTreeSet<_> = result
                .accepted_correspondences
                .iter()
                .chain(&result.text_boundary_correspondences)
                .copied()
                .collect();
            if spend(remaining, accepted.len()).is_none() {
                break;
            }
            let mut external_boundaries = Vec::new();
            let mut another_anchor = false;
            for index in accepted {
                if !result.matching.source_only_mandatory.contains(&index)
                    || result.matching.inferred_proposals.contains(&index)
                {
                    continue;
                }
                let proposal = &result.candidates.proposals[index];
                let ([old_node], [new_node]) = (proposal.old.as_slice(), proposal.new.as_slice())
                else {
                    continue;
                };
                let included = (
                    old_positions.contains_key(old_node),
                    new_positions.contains_key(new_node),
                );
                if included == (true, true) && index != boundary {
                    if edge.is_some() && local_boundaries.contains(&index) {
                        external_boundaries.push(index);
                    } else {
                        another_anchor = true;
                        break;
                    }
                }
                if included.0 != included.1 {
                    external_boundaries.push(index);
                }
            }
            if another_anchor {
                continue;
            }
            let population = SourceCutPopulation::AnchoredPage {
                boundary,
                edge,
                old_page: a,
                new_page: b,
                old: old_nodes.iter().map(|node| node.id).collect(),
                new: new_nodes.iter().map(|node| node.id).collect(),
                external_boundaries,
            };
            compare_population(
                old,
                new,
                result,
                parent,
                limits,
                &[old_nodes.iter().collect()],
                &[new_nodes.iter().collect()],
                sources,
                &[anchor],
                population,
                &maps,
                false,
                false,
                false,
                remaining,
            )?;
        }
    }
    Ok(())
}
