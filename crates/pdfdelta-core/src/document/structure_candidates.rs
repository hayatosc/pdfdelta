//! Inferred structure candidates use retained keys and local axis order, not
//! equality of cell values or the number of edits a correspondence would hide.

use std::collections::{BTreeMap, BTreeSet};

use crate::Result;

use super::{
    CorrespondenceProposal, CorrespondenceScope, DocumentGraph, EdgeKind, GraphNode, IdentityKey,
    MatchingLimits, NodeContent, NodeId, NodeKind, ProposalBasis, ScopeProposals,
    matching::selected_children,
};

pub(super) struct StructureSearch {
    pub exhaustive: bool,
    pub old: BTreeSet<NodeId>,
    pub new: BTreeSet<NodeId>,
}

#[derive(Default)]
struct Context<'a> {
    children: BTreeSet<(NodeKind, &'a IdentityKey)>,
    before: BTreeSet<(NodeKind, &'a IdentityKey)>,
    after: BTreeSet<(NodeKind, &'a IdentityKey)>,
}

fn spend(remaining: &mut usize, amount: usize) -> Option<()> {
    match remaining.checked_sub(amount) {
        Some(left) => {
            *remaining = left;
            Some(())
        }
        None => {
            *remaining = 0;
            None
        }
    }
}

fn contexts<'a>(
    graph: &'a DocumentGraph,
    remaining: &mut usize,
) -> Option<BTreeMap<NodeId, Context<'a>>> {
    spend(
        remaining,
        graph.nodes.len().saturating_add(graph.edges.len()),
    )?;
    let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (node.id, node)).collect();
    let mut contexts: BTreeMap<NodeId, Context<'_>> = BTreeMap::new();
    for edge in &graph.edges {
        if let Some(node) = nodes.get(&edge.to)
            && let Some(key) = &node.identity
        {
            spend(
                remaining,
                key.namespace.len().saturating_add(key.value.len()),
            )?;
            let context = contexts.entry(edge.from).or_default();
            match edge.kind {
                EdgeKind::Contains => {
                    context.children.insert((node.kind, key));
                }
                EdgeKind::Precedes => {
                    context.after.insert((node.kind, key));
                }
                _ => {}
            }
        }
        if edge.kind == EdgeKind::Precedes
            && let Some(node) = nodes.get(&edge.from)
            && let Some(key) = &node.identity
        {
            spend(
                remaining,
                key.namespace.len().saturating_add(key.value.len()),
            )?;
            contexts
                .entry(edge.to)
                .or_default()
                .before
                .insert((node.kind, key));
        }
    }
    Some(contexts)
}

fn compatible(a: &Context<'_>, b: &Context<'_>, remaining: &mut usize) -> Option<bool> {
    let cost = a
        .children
        .iter()
        .chain(&a.before)
        .chain(&a.after)
        .chain(&b.children)
        .chain(&b.before)
        .chain(&b.after)
        .fold(0_usize, |sum, (_, key)| {
            sum.saturating_add(key.namespace.len())
                .saturating_add(key.value.len())
                .saturating_add(1)
        });
    spend(remaining, cost)?;
    Some(
        a.children.intersection(&b.children).take(2).count() == 2
            || ((!a.before.is_empty() || !a.after.is_empty())
                && a.before == b.before
                && a.after == b.after),
    )
}

fn container(node: &GraphNode) -> bool {
    matches!(node.content, NodeContent::Container)
        && matches!(
            node.kind,
            NodeKind::Table | NodeKind::Row | NodeKind::Column | NodeKind::Section | NodeKind::List
        )
}

/// Adds only inferred candidates. Truncation withdraws the entire optional batch;
/// the caller retains dependencies so unrelated source-backed results survive.
pub(super) fn append(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    axes: &[(NodeId, NodeId)],
    protected: &BTreeSet<usize>,
    candidates: &mut ScopeProposals,
    limits: MatchingLimits,
) -> Result<StructureSearch> {
    let mut blocked_old = BTreeSet::new();
    let mut blocked_new = BTreeSet::new();
    for index in protected {
        blocked_old.extend(candidates.proposals[*index].old.iter().copied());
        blocked_new.extend(candidates.proposals[*index].new.iter().copied());
    }
    let eligible =
        |node: &&GraphNode| container(node) || (!axes.is_empty() && node.kind == NodeKind::Cell);
    let left: Vec<_> = selected_children(old, scope.old, limits.channels)?
        .into_iter()
        .filter(eligible)
        .filter(|node| !blocked_old.contains(&node.id))
        .collect();
    let right: Vec<_> = selected_children(new, scope.new, limits.channels)?
        .into_iter()
        .filter(eligible)
        .filter(|node| !blocked_new.contains(&node.id))
        .collect();
    let mut search = StructureSearch {
        exhaustive: true,
        old: left.iter().map(|node| node.id).collect(),
        new: right.iter().map(|node| node.id).collect(),
    };
    if left.is_empty() || right.is_empty() {
        return Ok(search);
    }
    let start = candidates.proposals.len();
    let allowance = limits
        .max_group_token_checks
        .saturating_sub(candidates.group_token_checks);
    let mut remaining = allowance;
    let mut ownership = limits
        .max_ownership_visits
        .saturating_sub(candidates.group_constraint_checks);
    let ownership_start = ownership;
    let mut existing: BTreeSet<_> = candidates
        .proposals
        .iter()
        .filter(|pair| pair.old.len() == 1 && pair.new.len() == 1)
        .map(|pair| (pair.old[0], pair.new[0]))
        .collect();
    let completed = (|| {
        let old_context = contexts(old, &mut remaining)?;
        let new_context = contexts(new, &mut remaining)?;
        let cells = if axes.is_empty() {
            None
        } else {
            let a = super::tables::cell_keys(old, scope.old, &mut ownership)?;
            let b = super::tables::cell_keys(new, scope.new, &mut ownership)?;
            if !a.exhaustive || !b.exhaustive {
                return None;
            }
            Some((a, b))
        };
        for a in &left {
            for b in &right {
                if candidates.examined_pairs == limits.max_pair_checks {
                    return None;
                }
                candidates.examined_pairs += 1;
                if a.kind != b.kind || existing.contains(&(a.id, b.id)) {
                    continue;
                }
                let supported = if a.kind == NodeKind::Cell {
                    let Some((old_cells, new_cells)) = &cells else {
                        continue;
                    };
                    let (Some(old_key), Some(new_key)) =
                        (old_cells.keys.get(&a.id), new_cells.keys.get(&b.id))
                    else {
                        continue;
                    };
                    spend(&mut remaining, axes.len())?;
                    old_key.row == scope.old
                        && new_key.row == scope.new
                        && axes.contains(&(old_key.column, new_key.column))
                } else {
                    let (Some(a), Some(b)) = (old_context.get(&a.id), new_context.get(&b.id))
                    else {
                        continue;
                    };
                    compatible(a, b, &mut remaining)?
                };
                if !supported {
                    continue;
                }
                if candidates.proposals.len() == limits.max_proposals {
                    return None;
                }
                existing.insert((a.id, b.id));
                candidates.proposals.push(CorrespondenceProposal {
                    old: vec![a.id],
                    new: vec![b.id],
                    basis: ProposalBasis::StructuralNeighbor,
                    supplier: "keyed-structure-context-v1".into(),
                    weight: 1,
                });
            }
        }
        Some(())
    })();
    candidates.group_token_checks = candidates
        .group_token_checks
        .saturating_add(allowance - remaining);
    candidates.group_constraint_checks = candidates
        .group_constraint_checks
        .saturating_add(ownership_start - ownership);
    if completed.is_none() {
        candidates.proposals.truncate(start);
        candidates.exhaustive = false;
        search.exhaustive = false;
    }
    Ok(search)
}
