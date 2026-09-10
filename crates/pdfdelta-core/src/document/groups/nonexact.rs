use std::collections::{BTreeMap, BTreeSet};

use crate::{Result, normalize::ComparableToken};

use super::{
    CorrespondenceProposal, CorrespondenceScope, DocumentGraph, EdgeKind, MatchingLimits, NodeId,
    ProposalBasis, ScopeProposals, charge_tokens, leaves, overlaps, text, unfinished,
};

/// Enumerates changed 1:N/N:1 views along retained order relations. Concatenation
/// preserves every token, including boundary spaces; no separator is invented.
/// Membership, source conflicts, and alternative partitions use the same checks
/// as exact groups. Correspondence and boundary interpretations remain inferred.
pub(in crate::document) fn append_nonexact_groups(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    result: &mut ScopeProposals,
    limits: MatchingLimits,
    allowed: (&BTreeSet<NodeId>, &BTreeSet<NodeId>),
) -> Result<()> {
    let old_nodes = leaves(old, scope.old, limits, Some(allowed.0))?;
    let new_nodes = leaves(new, scope.new, limits, Some(allowed.1))?;
    let mut existing: BTreeSet<_> = result
        .proposals
        .iter()
        .map(|proposal| (proposal.old.clone(), proposal.new.clone()))
        .collect();
    for (whole, parts, graph, reverse) in [
        (&old_nodes, &new_nodes, new, false),
        (&new_nodes, &old_nodes, old, true),
    ] {
        if whole.is_empty() || parts.len() < 2 {
            continue;
        }
        let mut next: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
        for edge in &graph.edges {
            if edge.kind == EdgeKind::Precedes
                && parts.contains_key(&edge.from)
                && parts.contains_key(&edge.to)
            {
                next.entry(edge.from).or_default().insert(edge.to);
            }
        }
        for &start in next.keys() {
            let mut pending = vec![vec![start]];
            while let Some(group) = pending.pop() {
                if result.examined_pairs == limits.max_pair_checks {
                    unfinished(result, &old_nodes, &new_nodes);
                    return Ok(());
                }
                result.examined_pairs += 1;
                let kind = parts[&start].kind;
                if group.iter().any(|id| parts[id].kind != kind) {
                    continue;
                }
                match overlaps(
                    &group,
                    parts,
                    graph,
                    &mut result.group_constraint_checks,
                    limits.max_ownership_visits,
                ) {
                    Some(false) => {}
                    Some(true) => continue,
                    None => {
                        unfinished(result, &old_nodes, &new_nodes);
                        return Ok(());
                    }
                }
                if group.len() > 1 {
                    let length = group.iter().fold(0usize, |total, id| {
                        total.saturating_add(text(parts[id]).len())
                    });
                    if !charge_tokens(result, limits, length) {
                        unfinished(result, &old_nodes, &new_nodes);
                        return Ok(());
                    }
                    let tokens: Vec<_> = group
                        .iter()
                        .flat_map(|id| text(parts[id]).iter().cloned())
                        .collect();
                    for node in whole.values().filter(|node| node.kind == kind) {
                        if result.examined_pairs == limits.max_pair_checks {
                            unfinished(result, &old_nodes, &new_nodes);
                            return Ok(());
                        }
                        result.examined_pairs += 1;
                        let whole_tokens = text(node);
                        let work = whole_tokens
                            .len()
                            .saturating_add(tokens.len())
                            .saturating_mul(4);
                        if !charge_tokens(result, limits, work) {
                            unfinished(result, &old_nodes, &new_nodes);
                            return Ok(());
                        }
                        if whole_tokens == tokens {
                            continue;
                        }
                        let (old, new) = if reverse {
                            (group.clone(), vec![node.id])
                        } else {
                            (vec![node.id], group.clone())
                        };
                        if !existing.insert((old.clone(), new.clone())) {
                            continue;
                        }
                        if result.proposals.len() == limits.max_proposals {
                            unfinished(result, &old_nodes, &new_nodes);
                            return Ok(());
                        }
                        result.proposals.push(CorrespondenceProposal {
                            old,
                            new,
                            basis: ProposalBasis::TextSimilarity,
                            supplier: "retained-order-trigram-group-v1".into(),
                            weight: weight(whole_tokens, &tokens),
                        });
                    }
                }
                if let Some(successors) = next.get(group.last().expect("nonempty group")) {
                    for successor in successors {
                        if group.contains(successor) {
                            continue;
                        }
                        if group.len() >= limits.max_group_nodes
                            || pending.len()
                                >= limits.max_pair_checks.saturating_sub(result.examined_pairs)
                        {
                            unfinished(result, &old_nodes, &new_nodes);
                            return Ok(());
                        }
                        let mut extended = group.clone();
                        extended.push(*successor);
                        pending.push(extended);
                    }
                }
            }
        }
    }
    Ok(())
}

fn weight(a: &[ComparableToken], b: &[ComparableToken]) -> u32 {
    let grams = |tokens: &[ComparableToken]| {
        let mut counts = BTreeMap::new();
        for gram in tokens.windows(3.min(tokens.len())) {
            *counts.entry(gram.to_vec()).or_insert(0usize) += 1;
        }
        counts
    };
    let left = grams(a);
    let right = grams(b);
    let common: usize = left
        .iter()
        .map(|(gram, count)| (*count).min(right.get(gram).copied().unwrap_or(0)))
        .sum();
    let count = (a.len() - 3.min(a.len()) + 1) as u128 + (b.len() - 3.min(b.len()) + 1) as u128;
    1 + ((common as u128 * 2_000_000) / count) as u32
}
