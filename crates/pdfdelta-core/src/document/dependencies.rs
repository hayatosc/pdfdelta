use std::collections::{BTreeMap, BTreeSet};

use super::{DocumentGraph, EdgeKind, GraphNode, NodeId, SourceRef};

/// A truncated supplier may hide a rival for these nodes. Propagation uses
/// physical-source conflicts, alternative partitions, and containment, leaving
/// independent fields usable.
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
    let mut conflict_sources: Vec<&[SourceRef]> = graph
        .source_conflicts
        .iter()
        .map(|conflict| conflict.sources.as_slice())
        .collect();
    if transitive_conflicts && !graph.alternatives.is_empty() {
        let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (node.id, node)).collect();
        // Validated partitions preserve their parent's complete source set.
        // Missing a candidate in one partition may displace a disjoint member
        // of another partition, even without a direct physical-source overlap.
        conflict_sources.extend(
            graph
                .alternatives
                .iter()
                .filter_map(|alternative| nodes.get(&alternative.parent))
                .map(|parent| parent.sources.as_slice()),
        );
    }
    let mut conflicts: BTreeMap<SourceRef, Vec<usize>> = BTreeMap::new();
    for (index, sources) in conflict_sources.iter().enumerate() {
        for source in *sources {
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
            for other in conflict_sources[*index] {
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

/// Any omitted edge can change the optimum throughout an existing dependency
/// component. Blocking only its direct endpoints would miss alternating paths
/// through groups or shared sources. Components disjoint from both endpoint
/// closures retain their completed-search guarantees.
pub(super) fn pending_source_proposals(
    old: &DocumentGraph,
    new: &DocumentGraph,
    matching: &super::ScopeMatching,
    proposals: &[super::CorrespondenceProposal],
    pending: Option<&super::IncompleteCandidateNodes>,
) -> BTreeSet<usize> {
    let Some(pending) = pending else {
        return (0..proposals.len()).collect();
    };
    let left = candidate_dependencies(old, |node| pending.old.contains(&node.id));
    let right = candidate_dependencies(new, |node| pending.new.contains(&node.id));
    matching
        .components
        .iter()
        .filter(|component| {
            component.proposals.iter().any(|index| {
                let proposal = &proposals[*index];
                proposal.old.iter().any(|node| left.contains(node))
                    || proposal.new.iter().any(|node| right.contains(node))
            })
        })
        .flat_map(|component| component.proposals.iter().copied())
        .collect()
}

/// Forced ownership blocks direct overlaps, not every node in a conflict chain:
/// two disjoint pieces can both overlap a third, unused composite view.
pub(super) fn occupied_conflicts(
    graph: &DocumentGraph,
    occupied: impl Fn(&GraphNode) -> bool,
) -> BTreeSet<NodeId> {
    dependency_closure(graph, occupied, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        CorrespondenceProposal, CorrespondenceScope, GraphEdge, IncompleteCandidateNodes,
        NodeContent, NodeKind, ProposalBasis, TextNormalization, TextView, ViewBasis,
        solve_correspondence_scope,
    };
    use crate::normalize::ComparableToken;

    fn graph(texts: &[&str]) -> DocumentGraph {
        let mut graph = DocumentGraph::default();
        graph.nodes.push(GraphNode {
            id: NodeId(0),
            kind: NodeKind::Document,
            pages: Vec::new(),
            sources: Vec::new(),
            identity: None,
            basis: ViewBasis::SourceStructure,
            content: NodeContent::Container,
        });
        for (index, text) in texts.iter().enumerate() {
            let id = NodeId(index as u64 + 1);
            let sources = vec![SourceRef::Structured { element: id.0 }];
            let tokens: Vec<_> = text.chars().map(ComparableToken::Scalar).collect();
            graph.nodes.push(GraphNode {
                id,
                kind: NodeKind::Paragraph,
                pages: Vec::new(),
                sources: sources.clone(),
                identity: None,
                basis: ViewBasis::SourceStructure,
                content: NodeContent::Text {
                    view: TextView {
                        origins: vec![sources; tokens.len()],
                        source_backed: vec![true; tokens.len()],
                        tokens,
                        normalization: TextNormalization::Exact,
                    },
                },
            });
            graph.edges.push(GraphEdge {
                from: NodeId(0),
                to: id,
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::SourceStructure,
            });
            if index > 0 {
                graph.edges.push(GraphEdge {
                    from: NodeId(id.0 - 1),
                    to: id,
                    kind: EdgeKind::Precedes,
                    sources: Vec::new(),
                    basis: ViewBasis::SourceStructure,
                });
            }
        }
        graph
    }

    #[test]
    fn omitted_candidate_can_displace_a_distant_group_component_match() {
        let old = graph(&["a", "b", "b"]);
        let new = graph(&["ab", "b", "a"]);
        let proposal = |old: &[u64], new| CorrespondenceProposal {
            old: old.iter().copied().map(NodeId).collect(),
            new: vec![NodeId(new)],
            basis: ProposalBasis::LiteralContent,
            supplier: "dependency-fixture".into(),
            weight: 1,
        };
        let mut proposals = vec![proposal(&[1, 2], 1), proposal(&[2], 2), proposal(&[3], 2)];
        let scope = CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        };
        let partial = solve_correspondence_scope(&old, &new, scope, &proposals, Default::default())
            .expect("partial graph");
        assert!(partial.source_only_mandatory.contains(&2));
        let pending = IncompleteCandidateNodes {
            old: [NodeId(1)].into(),
            new: [NodeId(3)].into(),
        };
        assert!(
            !candidate_dependencies(&old, |node| pending.old.contains(&node.id))
                .contains(&NodeId(3))
        );
        assert!(
            pending_source_proposals(&old, &new, &partial, &proposals, Some(&pending)).contains(&2)
        );

        proposals.push(proposal(&[1], 3));
        let complete =
            solve_correspondence_scope(&old, &new, scope, &proposals, Default::default())
                .expect("completed graph");
        assert!(!complete.source_only_mandatory.contains(&2));
    }

    #[test]
    fn incomplete_partition_candidates_reach_disjoint_sibling_sources() {
        let mut graph = graph(&["a", "b", "a", "b"]);
        graph.nodes[3].sources = graph.nodes[1].sources.clone();
        graph.nodes[4].sources = graph.nodes[2].sources.clone();
        for node in &mut graph.nodes[3..] {
            let NodeContent::Text { view } = &mut node.content else {
                unreachable!()
            };
            view.origins = vec![node.sources.clone(); view.tokens.len()];
        }
        graph.nodes[0].sources = vec![
            SourceRef::Structured { element: 1 },
            SourceRef::Structured { element: 2 },
        ];
        graph.alternatives.push(crate::document::AlternativeViews {
            parent: NodeId(0),
            partitions: vec![vec![NodeId(1), NodeId(2)], vec![NodeId(3), NodeId(4)]],
        });
        let pending = candidate_dependencies(&graph, |node| node.id == NodeId(1));
        assert!(pending.contains(&NodeId(4)));
        let occupied = occupied_conflicts(&graph, |node| node.id == NodeId(1));
        assert!(!occupied.contains(&NodeId(2)));
    }
}
