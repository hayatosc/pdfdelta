//! Typed relationships compare only after both endpoint correspondences survive
//! common ownership resolution. Missing edges require complete graph inventories.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    DocumentView, DocumentViewComparison, EdgeKind, GraphEdge, InterpretationStatus, NodeId,
    ParentCorrespondence, SourceRef, ViewBasis,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationComparison {
    pub kind: EdgeKind,
    pub old: [Vec<NodeId>; 2],
    pub new: [Vec<NodeId>; 2],
    pub old_present: bool,
    pub new_present: bool,
    pub old_sources: Vec<SourceRef>,
    pub new_sources: Vec<SourceRef>,
    /// Endpoint matches retain their scope and all enclosing parent premises.
    pub dependencies: Vec<ParentCorrespondence>,
    pub interpretation: InterpretationStatus,
}

impl RelationComparison {
    #[must_use]
    pub fn changed(&self) -> bool {
        self.old_present != self.new_present
    }
}

fn semantic(edge: &GraphEdge) -> bool {
    match edge.kind {
        // Native page/block containment and order support text layout; they
        // do not assert semantic membership or a tagged procedure's steps.
        EdgeKind::Contains | EdgeKind::Precedes => edge.basis != ViewBasis::NativeLayout,
        _ => true,
    }
}

pub(super) fn compare_relations(
    old_view: DocumentView<'_>,
    new_view: DocumentView<'_>,
    result: &mut DocumentViewComparison,
    max_work: usize,
) {
    let old = old_view.graph;
    let new = new_view.graph;
    let work = old
        .edges
        .len()
        .saturating_add(new.edges.len())
        .saturating_mul(4)
        .saturating_add(old.nodes.len())
        .saturating_add(new.nodes.len());
    if work > max_work {
        result
            .relation_unresolved
            .push("typed relationship enumeration exceeded its work budget".into());
        return;
    }
    let work = old
        .edges
        .iter()
        .chain(&new.edges)
        .fold(work, |total, edge| total.saturating_add(edge.sources.len()));
    if work > max_work {
        result
            .relation_unresolved
            .push("typed relationship source references exceeded the work budget".into());
        return;
    }
    let mut remaining = max_work - work;
    let inventory_complete = |view: DocumentView<'_>| {
        view.graph.relations_complete
            && !view
                .graph
                .edges
                .iter()
                .any(|edge| semantic(edge) && edge.basis.is_inferred())
            && (view
                .evidence
                .inventory_complete(None, super::Channel::Relations)
                || (!view.evidence.pages.is_empty()
                    && view.evidence.pages.iter().all(|page| {
                        view.evidence
                            .inventory_complete(Some(page.page), super::Channel::Relations)
                    })))
    };
    let old_complete = inventory_complete(old_view);
    let new_complete = inventory_complete(new_view);
    let mut forward = BTreeMap::new();
    let mut old_members = BTreeMap::new();
    let mut new_members = BTreeMap::new();
    let mut grouped = BTreeSet::new();
    let mut unsafe_groups = BTreeSet::new();
    let mut reverse = BTreeMap::new();
    let mut dependencies = BTreeMap::new();
    let mut inferred = BTreeSet::new();
    let root = result.scopes[0].result.matching.scope;
    forward.insert(root.old, root.old);
    old_members.insert(root.old, vec![root.old]);
    new_members.insert(root.old, vec![root.new]);
    reverse.insert(root.new, root.old);
    let old_nodes: BTreeMap<_, _> = old.nodes.iter().map(|node| (node.id, node)).collect();
    let new_nodes: BTreeMap<_, _> = new.nodes.iter().map(|node| (node.id, node)).collect();
    let mut old_scope =
        super::matching::descendants(&super::matching::containment(old), root.old, &old_nodes)
            .expect("validated root scope");
    let mut new_scope =
        super::matching::descendants(&super::matching::containment(new), root.new, &new_nodes)
            .expect("validated root scope");
    old_scope.insert(root.old);
    new_scope.insert(root.new);
    if old_nodes[&root.old].basis.is_inferred() || new_nodes[&root.new].basis.is_inferred() {
        inferred.insert(root.old);
    }
    for (scope_index, scope) in result.scopes.iter().enumerate() {
        for &index in &scope.result.accepted_correspondences {
            let proposal = &scope.result.candidates.proposals[index];
            let a = proposal.old[0];
            for &node in &proposal.old {
                forward.insert(node, a);
            }
            for &node in &proposal.new {
                reverse.insert(node, a);
            }
            old_members.insert(a, proposal.old.clone());
            new_members.insert(a, proposal.new.clone());
            if proposal.old.len() > 1 || proposal.new.len() > 1 {
                grouped.insert(a);
                if proposal
                    .old
                    .iter()
                    .any(|id| old_nodes[id].kind != super::NodeKind::Paragraph)
                    || proposal
                        .new
                        .iter()
                        .any(|id| new_nodes[id].kind != super::NodeKind::Paragraph)
                {
                    unsafe_groups.insert(a);
                }
            }
            dependencies.insert(
                a,
                ParentCorrespondence {
                    scope: scope_index,
                    proposal: index,
                },
            );
            if scope.interpretation == InterpretationStatus::Inferred
                || scope.result.matching.inferred_proposals.contains(&index)
            {
                inferred.insert(a);
            }
        }
    }
    let mut left = BTreeMap::<_, Vec<&GraphEdge>>::new();
    let mut right = BTreeMap::<_, Vec<&GraphEdge>>::new();
    let mut unmatched = false;
    for (graph, map, output, domain) in [
        (old, &forward, &mut left, &old_scope),
        (new, &reverse, &mut right, &new_scope),
    ] {
        for edge in &graph.edges {
            if !semantic(edge) || (!domain.contains(&edge.from) && !domain.contains(&edge.to)) {
                continue;
            }
            let a = map.get(&edge.from);
            let b = map.get(&edge.to);
            match (a, b) {
                (Some(&a), Some(&b)) => {
                    if unsafe_groups.contains(&a)
                        || unsafe_groups.contains(&b)
                        || ((grouped.contains(&a) || grouped.contains(&b))
                            && !matches!(edge.kind, EdgeKind::Contains | EdgeKind::Precedes))
                    {
                        unmatched = true;
                        continue;
                    }
                    // The accepted paragraph group already validates internal
                    // fragment order; splitting it does not add a semantic step.
                    if a == b && edge.from != edge.to && edge.kind == EdgeKind::Precedes {
                        continue;
                    }
                    let key = (a, b, edge.kind);
                    output.entry(key).or_default().push(edge);
                }
                _ => unmatched = true,
            }
        }
    }
    if unmatched {
        result.relation_unresolved.push(
            "typed relationships have endpoints without established relationship-compatible correspondences"
                .into(),
        );
    }
    let keys = left
        .keys()
        .chain(right.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut incomplete = false;
    for (a, b, kind) in keys {
        let key = (a, b, kind);
        let old_edges = left.get(&key);
        let new_edges = right.get(&key);
        if (old_edges.is_none() && !old_complete) || (new_edges.is_none() && !new_complete) {
            incomplete = true;
            continue;
        }
        let member_work = old_members[&a]
            .len()
            .saturating_add(old_members[&b].len())
            .saturating_add(new_members[&a].len())
            .saturating_add(new_members[&b].len());
        let Some(next) = remaining.checked_sub(member_work) else {
            result
                .relation_unresolved
                .push("typed relationship group endpoints exceeded the work budget".into());
            return;
        };
        remaining = next;
        let sources = |edges: Option<&Vec<&GraphEdge>>| {
            edges
                .into_iter()
                .flatten()
                .flat_map(|edge| edge.sources.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let is_inferred = inferred.contains(&root.old)
            || inferred.contains(&a)
            || inferred.contains(&b)
            || old_edges
                .into_iter()
                .chain(new_edges)
                .flatten()
                .any(|edge| edge.basis.is_inferred());
        result.relations.push(RelationComparison {
            kind,
            old: [old_members[&a].clone(), old_members[&b].clone()],
            new: [new_members[&a].clone(), new_members[&b].clone()],
            old_present: old_edges.is_some(),
            new_present: new_edges.is_some(),
            old_sources: sources(old_edges),
            new_sources: sources(new_edges),
            dependencies: [a, b]
                .into_iter()
                .filter_map(|id| dependencies.get(&id).copied())
                .collect(),
            interpretation: if is_inferred {
                InterpretationStatus::Inferred
            } else {
                InterpretationStatus::ConditionalOnCorrespondence
            },
        });
    }
    if incomplete {
        result.relation_unresolved.push(
            "an absent typed relationship cannot be established from incomplete graph relations"
                .into(),
        );
    }
}
