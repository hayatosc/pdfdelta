use std::collections::{BTreeMap, BTreeSet};

use super::{DocumentGraph, EdgeKind, GraphNode, NodeId, SourceRef};

/// A truncated supplier may hide a rival for these nodes. Propagation uses
/// physical-source conflicts and containment, leaving independent fields usable.
fn dependency_closure(
    graph: &DocumentGraph,
    eligible: impl Fn(&GraphNode) -> bool,
    transitive_conflicts: bool,
) -> BTreeSet<NodeId> {
    let mut selected: BTreeSet<_> = graph
        .nodes
        .iter()
        .filter(|node| eligible(node))
        .map(|node| node.id)
        .collect();
    if selected.is_empty() {
        return selected;
    }
    let mut children: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let mut parents: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for edge in &graph.edges {
        if edge.kind == EdgeKind::Contains {
            children.entry(edge.from).or_default().push(edge.to);
            parents.entry(edge.to).or_default().push(edge.from);
        }
    }
    let mut pending: Vec<_> = selected.iter().copied().collect();
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            if selected.insert(*child) {
                pending.push(*child);
            }
        }
    }
    let mut sources: BTreeSet<SourceRef> = graph
        .nodes
        .iter()
        .filter(|node| selected.contains(&node.id))
        .flat_map(|node| node.sources.iter().copied())
        .collect();
    let mut conflicts: BTreeMap<SourceRef, Vec<usize>> = BTreeMap::new();
    for (index, conflict) in graph.source_conflicts.iter().enumerate() {
        for source in &conflict.sources {
            conflicts.entry(*source).or_default().push(index);
        }
    }
    let mut visited = BTreeSet::new();
    let mut pending: Vec<_> = sources.iter().copied().collect();
    while let Some(source) = pending.pop() {
        for index in conflicts.get(&source).into_iter().flatten() {
            if !visited.insert(*index) {
                continue;
            }
            for other in &graph.source_conflicts[*index].sources {
                if sources.insert(*other) && transitive_conflicts {
                    pending.push(*other);
                }
            }
        }
    }
    let mut affected = selected;
    affected.extend(
        graph
            .nodes
            .iter()
            .filter(|node| node.sources.iter().any(|source| sources.contains(source)))
            .map(|node| node.id),
    );
    let mut pending: Vec<_> = affected.iter().copied().collect();
    while let Some(child) = pending.pop() {
        for parent in parents.get(&child).into_iter().flatten() {
            if affected.insert(*parent) {
                pending.push(*parent);
            }
        }
    }
    affected
}

pub(super) fn candidate_dependencies(
    graph: &DocumentGraph,
    eligible: impl Fn(&GraphNode) -> bool,
) -> BTreeSet<NodeId> {
    dependency_closure(graph, eligible, true)
}

/// Forced ownership blocks direct overlaps, not every node in a conflict chain:
/// two disjoint pieces can both overlap a third, unused composite view.
pub(super) fn occupied_conflicts(
    graph: &DocumentGraph,
    occupied: impl Fn(&GraphNode) -> bool,
) -> BTreeSet<NodeId> {
    dependency_closure(graph, occupied, false)
}
