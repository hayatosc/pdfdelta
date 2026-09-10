use std::collections::{BTreeMap, BTreeSet, HashMap};

mod nonexact;
pub(super) use nonexact::append_nonexact_groups;

use crate::Result;

use super::{
    CorrespondenceProposal, CorrespondenceScope, DocumentGraph, EdgeKind, GraphNode,
    MatchingLimits, NodeContent, NodeId, ProposalBasis, ScopeProposals, SourceRef,
    TextNormalization,
    matching::{selected_children, source_children},
};

/// Enumerates exact 1:N/N:1 leaf-text views along retained order edges. Prefix
/// matching prunes candidates but never edits evidence or supplies missing spaces.
pub(super) fn append_exact_groups(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    result: &mut ScopeProposals,
    limits: MatchingLimits,
    optional_nodes: Option<(&BTreeSet<NodeId>, &BTreeSet<NodeId>)>,
) -> Result<()> {
    let old_nodes = leaves(old, scope.old, limits, optional_nodes.map(|nodes| nodes.0))?;
    let new_nodes = leaves(new, scope.new, limits, optional_nodes.map(|nodes| nodes.1))?;
    let mut existing: BTreeSet<_> = result
        .proposals
        .iter()
        .map(|proposal| (proposal.old.clone(), proposal.new.clone()))
        .collect();
    for (whole, parts, graph, reverse) in [
        (&old_nodes, &new_nodes, new, false),
        (&new_nodes, &old_nodes, old, true),
    ] {
        let mut next: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
        for edge in &graph.edges {
            if edge.kind == EdgeKind::Precedes
                && parts.contains_key(&edge.from)
                && parts.contains_key(&edge.to)
            {
                next.entry(edge.from).or_default().insert(edge.to);
            }
        }
        if next.is_empty() {
            continue;
        }
        let Some(starts) = PrefixIndex::new(&next, parts, result, limits) else {
            unfinished(result, &old_nodes, &new_nodes);
            return Ok(());
        };
        for node in whole.values() {
            let tokens = text(node);
            let Some(prefixes) = starts.prefixes(node, result, limits) else {
                unfinished(result, &old_nodes, &new_nodes);
                return Ok(());
            };
            for start in prefixes {
                let mut pending = vec![(vec![start], 0)];
                while let Some((group, offset)) = pending.pop() {
                    if result.examined_pairs == limits.max_pair_checks {
                        unfinished(result, &old_nodes, &new_nodes);
                        return Ok(());
                    }
                    result.examined_pairs += 1;
                    let last = group[group.len() - 1];
                    let part = parts[&last];
                    let part_tokens = text(part);
                    if node.kind != part.kind || part_tokens.len() > tokens.len() - offset {
                        continue;
                    }
                    // The trie already verified the initial member against
                    // the immutable whole. Later members retain exact checks.
                    if group.len() > 1 {
                        match equal_tokens(
                            part_tokens,
                            &tokens[offset..offset + part_tokens.len()],
                            result,
                            limits,
                        ) {
                            Some(true) => {}
                            Some(false) => continue,
                            None => {
                                unfinished(result, &old_nodes, &new_nodes);
                                return Ok(());
                            }
                        }
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
                    let end = offset + part_tokens.len();
                    if end == tokens.len() {
                        if group.len() > 1 {
                            let (old, new) = if reverse {
                                (group, vec![node.id])
                            } else {
                                (vec![node.id], group)
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
                                basis: ProposalBasis::LiteralContent,
                                supplier: "retained-order-exact-group-v1".into(),
                                weight: 1,
                            });
                        }
                        continue;
                    }
                    if let Some(successors) = next.get(&last) {
                        if group.len() >= limits.max_group_nodes {
                            unfinished(result, &old_nodes, &new_nodes);
                            return Ok(());
                        }
                        for successor in successors {
                            if !group.contains(successor) {
                                if pending.len()
                                    >= limits.max_pair_checks.saturating_sub(result.examined_pairs)
                                {
                                    unfinished(result, &old_nodes, &new_nodes);
                                    return Ok(());
                                }
                                let mut extended = group.clone();
                                extended.push(*successor);
                                pending.push((extended, end));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn unfinished(
    result: &mut ScopeProposals,
    old: &BTreeMap<NodeId, &GraphNode>,
    new: &BTreeMap<NodeId, &GraphNode>,
) {
    // Every unvisited group belongs to these fully enumerated leaf populations.
    // Preserve any earlier, wider unknown region rather than replacing it.
    if result.exhaustive && result.incomplete_nodes.is_none() {
        result.incomplete_nodes = Some(Default::default());
    }
    if let Some(pending) = &mut result.incomplete_nodes {
        pending.old.extend(old.keys().copied());
        pending.new.extend(new.keys().copied());
    }
    result.exhaustive = false;
}

#[derive(Default)]
struct PrefixNode<'a> {
    next: HashMap<&'a crate::normalize::ComparableToken, usize>,
    starts: Vec<NodeId>,
}

struct PrefixIndex<'a> {
    roots: BTreeMap<super::NodeKind, usize>,
    nodes: Vec<PrefixNode<'a>>,
}

impl<'a> PrefixIndex<'a> {
    fn new(
        next: &BTreeMap<NodeId, BTreeSet<NodeId>>,
        parts: &BTreeMap<NodeId, &'a GraphNode>,
        result: &mut ScopeProposals,
        limits: MatchingLimits,
    ) -> Option<Self> {
        let mut index = Self {
            roots: BTreeMap::new(),
            nodes: Vec::new(),
        };
        for start in next.keys() {
            let node = parts[start];
            if !charge_tokens(result, limits, text(node).len().saturating_add(1)) {
                return None;
            }
            let mut current = *index.roots.entry(node.kind).or_insert_with(|| {
                index.nodes.push(PrefixNode::default());
                index.nodes.len() - 1
            });
            for token in text(node) {
                current = if let Some(child) = index.nodes[current].next.get(token) {
                    *child
                } else {
                    let child = index.nodes.len();
                    index.nodes.push(PrefixNode::default());
                    index.nodes[current].next.insert(token, child);
                    child
                };
            }
            index.nodes[current].starts.push(*start);
        }
        Some(index)
    }

    fn prefixes(
        &self,
        whole: &GraphNode,
        result: &mut ScopeProposals,
        limits: MatchingLimits,
    ) -> Option<Vec<NodeId>> {
        let mut starts = Vec::new();
        let Some(mut current) = self.roots.get(&whole.kind).copied() else {
            return Some(starts);
        };
        let tokens = text(whole);
        // A group has at least two nonempty members, so its first member must
        // be a proper prefix. Hash-map equality checks the original tokens.
        for token in tokens.iter().take(tokens.len().saturating_sub(1)) {
            if !charge_tokens(result, limits, 1) {
                return None;
            }
            let Some(child) = self.nodes[current].next.get(token) else {
                break;
            };
            current = *child;
            let terminal = &self.nodes[current].starts;
            if !charge_tokens(result, limits, terminal.len()) {
                return None;
            }
            starts.extend(terminal);
        }
        starts.sort_unstable();
        Some(starts)
    }
}

fn charge_tokens(result: &mut ScopeProposals, limits: MatchingLimits, work: usize) -> bool {
    if work
        > limits
            .max_group_token_checks
            .saturating_sub(result.group_token_checks)
    {
        return false;
    }
    result.group_token_checks += work;
    true
}

fn equal_tokens(
    a: &[crate::normalize::ComparableToken],
    b: &[crate::normalize::ComparableToken],
    result: &mut ScopeProposals,
    limits: MatchingLimits,
) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    for (a, b) in a.iter().zip(b) {
        if !charge_tokens(result, limits, 1) {
            return None;
        }
        if a != b {
            return Some(false);
        }
    }
    Some(true)
}

fn leaves<'a>(
    graph: &'a DocumentGraph,
    root: NodeId,
    limits: MatchingLimits,
    optional_nodes: Option<&BTreeSet<NodeId>>,
) -> Result<BTreeMap<NodeId, &'a GraphNode>> {
    let parents: BTreeSet<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .map(|edge| edge.from)
        .collect();
    let children = if optional_nodes.is_some() {
        selected_children(graph, root, limits.channels)?
    } else {
        source_children(graph, root, limits.channels)?
    };
    Ok(children.into_iter().filter(|node| {
        optional_nodes.is_none_or(|allowed| allowed.contains(&node.id)) &&
        node.kind != super::NodeKind::Cell && node.identity.is_none() && !parents.contains(&node.id)
            && matches!(&node.content, NodeContent::Text { view } if !view.tokens.is_empty() && view.normalization == TextNormalization::Exact)
    }).map(|node| (node.id, node)).collect())
}

fn text(node: &GraphNode) -> &[crate::normalize::ComparableToken] {
    match &node.content {
        NodeContent::Text { view } => &view.tokens,
        _ => &[],
    }
}

fn overlaps(
    group: &[NodeId],
    nodes: &BTreeMap<NodeId, &GraphNode>,
    graph: &DocumentGraph,
    checks: &mut usize,
    limit: usize,
) -> Option<bool> {
    let mut sources = BTreeSet::<SourceRef>::new();
    for node in group {
        for source in &nodes[node].sources {
            if *checks == limit {
                return None;
            }
            *checks += 1;
            if !sources.insert(*source) {
                return Some(true);
            }
        }
    }
    for conflict in &graph.source_conflicts {
        let mut covered = 0;
        for source in &conflict.sources {
            if *checks == limit {
                return None;
            }
            *checks += 1;
            covered += usize::from(sources.contains(source));
            if covered > 1 {
                return Some(true);
            }
        }
    }
    super::matching::leaf_partitions_compatible(group, graph, checks, limit)
        .map(|compatible| !compatible)
}
