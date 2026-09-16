//! Literal token features rank candidates without rewriting text or proving identity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    CorrespondenceProposal, CorrespondenceScope, DocumentGraph, GraphNode, MatchingLimits,
    NodeContent, NodeKind, ProposalBasis, ScopeProposals, matching::selected_children,
};
use crate::{Result, normalize::ComparableToken};

#[derive(Clone, Copy, Debug)]
pub struct TextCandidateLimits {
    pub max_token_visits: usize,
    pub max_feature_entries: usize,
}

impl Default for TextCandidateLimits {
    fn default() -> Self {
        Self {
            max_token_visits: 4_000_000,
            max_feature_entries: 200_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextCandidateSearch {
    pub examined_pairs: usize,
    pub source_ownership_visits: usize,
    pub source_conflict_checks: usize,
    pub source_search_states: usize,
    pub protected_correspondences: BTreeSet<usize>,
    pub old_nodes: BTreeSet<super::NodeId>,
    pub new_nodes: BTreeSet<super::NodeId>,
    pub token_visits: usize,
    pub feature_entries: usize,
    /// Posting entries transferred from right-side feature maps; bounded by feature_entries.
    #[serde(default)]
    pub index_entries: usize,
    pub exhaustive: bool,
}

pub(super) fn eligible(node: &GraphNode) -> bool {
    node.identity.is_none()
        && matches!(
            node.kind,
            NodeKind::Paragraph
                | NodeKind::Header
                | NodeKind::Footer
                | NodeKind::Caption
                | NodeKind::ListItem
                | NodeKind::Code
                | NodeKind::Formula
        )
        && matches!(&node.content, NodeContent::Text { view } if !view.tokens.is_empty() && view.has_validated_normalization())
}

struct Features<'a> {
    node: &'a GraphNode,
    tokens: &'a [ComparableToken],
    normalization_exact: bool,
    grams: BTreeMap<&'a [ComparableToken], usize>,
    count: usize,
}

fn features<'a>(
    nodes: &[&'a GraphNode],
    search: &mut TextCandidateSearch,
    limits: TextCandidateLimits,
) -> Option<Vec<Features<'a>>> {
    let mut output = Vec::new();
    for &node in nodes.iter().filter(|node| eligible(node)) {
        let NodeContent::Text { view } = &node.content else {
            unreachable!()
        };
        let width = 3.min(view.tokens.len());
        let work = view.tokens.len().saturating_mul(width);
        if work > limits.max_token_visits.saturating_sub(search.token_visits) {
            return None;
        }
        search.token_visits += work;
        let mut grams = BTreeMap::new();
        for gram in view.tokens.windows(width) {
            if !grams.contains_key(gram) {
                if search.feature_entries == limits.max_feature_entries {
                    return None;
                }
                search.feature_entries += 1;
            }
            *grams.entry(gram).or_insert(0usize) += 1;
        }
        output.push(Features {
            node,
            tokens: &view.tokens,
            normalization_exact: view.normalization == super::TextNormalization::Exact,
            grams,
            count: view.tokens.len() - width + 1,
        });
    }
    Some(output)
}

/// Enumerates same-kind, unkeyed pairs not dominated by mandatory source matches,
/// including pairs with zero feature overlap.
/// Multiset Dice similarity of literal trigrams supplies only an inferred weight.
/// Declared archives also supply exact pairs and ordered split/merge views. On any
/// truncation this supplier withdraws its proposals; callers retain dependencies.
pub(super) fn append_text_candidates(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    candidates: &mut ScopeProposals,
    matching: MatchingLimits,
    limits: TextCandidateLimits,
) -> Result<TextCandidateSearch> {
    let mut search = TextCandidateSearch {
        examined_pairs: 0,
        source_ownership_visits: 0,
        source_conflict_checks: 0,
        source_search_states: 0,
        protected_correspondences: BTreeSet::new(),
        old_nodes: BTreeSet::new(),
        new_nodes: BTreeSet::new(),
        token_visits: 0,
        feature_entries: 0,
        index_entries: 0,
        exhaustive: true,
    };
    let mut left = selected_children(old, scope.old, matching.channels)?;
    let mut right = selected_children(new, scope.new, matching.channels)?;
    if !left.iter().any(|node| eligible(node)) || !right.iter().any(|node| eligible(node)) {
        return Ok(search);
    }
    search.exhaustive = candidates.exhaustive;
    search.old_nodes.extend(
        left.iter()
            .filter(|node| eligible(node))
            .map(|node| node.id),
    );
    search.new_nodes.extend(
        right
            .iter()
            .filter(|node| eligible(node))
            .map(|node| node.id),
    );
    match protect_source_correspondences(old, new, scope, candidates, matching, &mut search) {
        Ok(()) => {}
        Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {
            search.exhaustive = false;
            return Ok(search);
        }
        Err(error) => return Err(error),
    }
    let protected_old: BTreeSet<_> = search
        .protected_correspondences
        .iter()
        .flat_map(|&index| candidates.proposals[index].old.iter().copied())
        .collect();
    let protected_new: BTreeSet<_> = search
        .protected_correspondences
        .iter()
        .flat_map(|&index| candidates.proposals[index].new.iter().copied())
        .collect();
    let blocked_old =
        super::dependencies::occupied_conflicts(old, |node| protected_old.contains(&node.id));
    let blocked_new =
        super::dependencies::occupied_conflicts(new, |node| protected_new.contains(&node.id));
    search.old_nodes.retain(|node| !blocked_old.contains(node));
    search.new_nodes.retain(|node| !blocked_new.contains(node));
    if !search.exhaustive {
        return Ok(search);
    }
    left.retain(|node| search.old_nodes.contains(&node.id));
    right.retain(|node| search.new_nodes.contains(&node.id));
    if left.is_empty() || right.is_empty() {
        return Ok(search);
    }
    let start = candidates.proposals.len();
    if !old.alternatives.is_empty() || !new.alternatives.is_empty() {
        super::groups::append_exact_groups(
            old,
            new,
            scope,
            candidates,
            matching,
            Some((&search.old_nodes, &search.new_nodes)),
        )?;
        if !candidates.exhaustive {
            candidates.proposals.truncate(start);
            search.exhaustive = false;
            return Ok(search);
        }
    }
    super::groups::append_nonexact_groups(
        old,
        new,
        scope,
        candidates,
        matching,
        (&search.old_nodes, &search.new_nodes),
    )?;
    if !candidates.exhaustive {
        candidates.proposals.truncate(start);
        search.exhaustive = false;
        return Ok(search);
    }
    let (Some(left), Some(mut right)) = (
        features(&left, &mut search, limits),
        features(&right, &mut search, limits),
    ) else {
        candidates.proposals.truncate(start);
        search.exhaustive = false;
        return Ok(search);
    };
    append_indexed_pairs(&left, &mut right, candidates, &mut search, matching, limits);
    if !search.exhaustive {
        candidates.proposals.truncate(start);
    }
    Ok(search)
}

fn record_source_work(search: &mut TextCandidateSearch, matching: &super::ScopeMatching) {
    search.source_ownership_visits = search
        .source_ownership_visits
        .saturating_add(matching.ownership_visits);
    search.source_conflict_checks = search
        .source_conflict_checks
        .saturating_add(matching.conflict_checks);
    search.source_search_states = matching
        .components
        .iter()
        .fold(search.source_search_states, |total, component| {
            total.saturating_add(component.explored_states)
        });
}

fn charge_index_work(
    search: &mut TextCandidateSearch,
    limits: TextCandidateLimits,
    work: usize,
) -> bool {
    if work > limits.max_token_visits.saturating_sub(search.token_visits) {
        search.exhaustive = false;
        return false;
    }
    search.token_visits += work;
    true
}

fn append_indexed_pairs(
    left: &[Features<'_>],
    right: &mut [Features<'_>],
    candidates: &mut ScopeProposals,
    search: &mut TextCandidateSearch,
    matching: MatchingLimits,
    limits: TextCandidateLimits,
) {
    let existing: BTreeSet<_> = candidates
        .proposals
        .iter()
        .filter(|proposal| proposal.old.len() == 1 && proposal.new.len() == 1)
        .map(|proposal| (proposal.old[0], proposal.new[0]))
        .collect();
    let mut kinds = BTreeMap::<NodeKind, Vec<usize>>::new();
    let mut postings = BTreeMap::<(NodeKind, &[ComparableToken]), Vec<(usize, usize)>>::new();
    for (index, feature) in right.iter_mut().enumerate() {
        kinds.entry(feature.node.kind).or_default().push(index);
        // Transfer the bounded per-node maps rather than retaining a second copy.
        for (gram, count) in std::mem::take(&mut feature.grams) {
            if !charge_index_work(search, limits, gram.len()) {
                return;
            }
            postings
                .entry((feature.node.kind, gram))
                .or_default()
                .push((index, count));
            search.index_entries += 1;
        }
    }
    let mut common = vec![0usize; right.len()];
    for a in left {
        let Some(compatible) = kinds.get(&a.node.kind) else {
            continue;
        };
        if !charge_index_work(search, limits, right.len()) {
            return;
        }
        common.fill(0);
        for (gram, count) in &a.grams {
            if !charge_index_work(search, limits, gram.len()) {
                return;
            }
            if let Some(entries) = postings.get(&(a.node.kind, *gram)) {
                if !charge_index_work(search, limits, entries.len()) {
                    return;
                }
                for &(index, other_count) in entries {
                    common[index] += (*count).min(other_count);
                }
            }
        }
        // Enumerate the full compatible complement, including zero common grams.
        for &index in compatible {
            if search.examined_pairs == matching.max_pair_checks {
                search.exhaustive = false;
                return;
            }
            search.examined_pairs += 1;
            let b = &right[index];
            if existing.contains(&(a.node.id, b.node.id)) {
                continue;
            }
            if candidates.proposals.len() == matching.max_proposals {
                search.exhaustive = false;
                return;
            }
            let could_be_literal = a.normalization_exact
                && b.normalization_exact
                && a.tokens.len() == b.tokens.len()
                && common[index] == a.count
                && common[index] == b.count;
            if could_be_literal && !charge_index_work(search, limits, a.tokens.len()) {
                return;
            }
            let literal = could_be_literal && a.tokens == b.tokens;
            let weight = 1
                + ((common[index] as u128 * 2_000_000) / (a.count as u128 + b.count as u128))
                    as u32;
            candidates.proposals.push(CorrespondenceProposal {
                old: vec![a.node.id],
                new: vec![b.node.id],
                basis: if literal {
                    ProposalBasis::LiteralContent
                } else {
                    ProposalBasis::TextSimilarity
                },
                supplier: if literal {
                    "literal-view-equality-v1"
                } else {
                    "literal-trigram-dice-v1"
                }
                .into(),
                weight: if literal { 1 } else { weight },
            });
        }
    }
}

/// A source correspondence present in every optimum of the first three objective
/// classes cannot be displaced by any added inferred score. Pruning its physical
/// conflicts therefore preserves all possible optimal text correspondences.
fn protect_source_correspondences(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    candidates: &ScopeProposals,
    limits: MatchingLimits,
    search: &mut TextCandidateSearch,
) -> Result<()> {
    let matching =
        super::solve_correspondence_scope(old, new, scope, &candidates.proposals, limits)?;
    record_source_work(search, &matching);
    let pending = if candidates.exhaustive {
        BTreeSet::new()
    } else {
        super::dependencies::pending_source_proposals(
            old,
            new,
            &matching,
            &candidates.proposals,
            candidates.incomplete_nodes.as_ref(),
        )
    };
    search.protected_correspondences.extend(
        matching
            .source_only_mandatory
            .into_iter()
            .filter(|index| !pending.contains(index)),
    );
    Ok(())
}

#[cfg(test)]
fn append_dense_pairs(
    left: &[Features<'_>],
    right: &[Features<'_>],
    candidates: &mut ScopeProposals,
    search: &mut TextCandidateSearch,
    matching: MatchingLimits,
    limits: TextCandidateLimits,
) {
    let existing: BTreeSet<_> = candidates
        .proposals
        .iter()
        .filter(|proposal| proposal.old.len() == 1 && proposal.new.len() == 1)
        .map(|proposal| (proposal.old[0], proposal.new[0]))
        .collect();
    'pairs: for a in left {
        for b in right {
            if search.examined_pairs == matching.max_pair_checks {
                search.exhaustive = false;
                break 'pairs;
            }
            search.examined_pairs += 1;
            if a.node.kind != b.node.kind {
                continue;
            }
            let work = a
                .tokens
                .len()
                .min(b.tokens.len())
                .saturating_add(a.grams.len().saturating_mul(3));
            if work > limits.max_token_visits.saturating_sub(search.token_visits) {
                search.exhaustive = false;
                break 'pairs;
            }
            search.token_visits += work;
            if existing.contains(&(a.node.id, b.node.id)) {
                continue;
            }
            if candidates.proposals.len() == matching.max_proposals {
                search.exhaustive = false;
                break 'pairs;
            }
            let common = a
                .grams
                .iter()
                .map(|(gram, count)| (*count).min(b.grams.get(gram).copied().unwrap_or(0)))
                .sum::<usize>();
            let weight =
                1 + ((common as u128 * 2_000_000) / (a.count as u128 + b.count as u128)) as u32;
            let literal = a.normalization_exact && b.normalization_exact && a.tokens == b.tokens;
            candidates.proposals.push(CorrespondenceProposal {
                old: vec![a.node.id],
                new: vec![b.node.id],
                basis: if literal {
                    ProposalBasis::LiteralContent
                } else {
                    ProposalBasis::TextSimilarity
                },
                supplier: if literal {
                    "literal-view-equality-v1"
                } else {
                    "literal-trigram-dice-v1"
                }
                .into(),
                weight: if literal { 1 } else { weight },
            });
        }
    }
}

#[cfg(test)]
mod measurement;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        EdgeKind, GraphEdge, NodeId, SourceRef, TextNormalization, TextView, ViewBasis,
        propose_scope_correspondences, solve_correspondence_scope,
    };

    const SCOPE: CorrespondenceScope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };

    pub(super) fn graph(texts: &[&str]) -> DocumentGraph {
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
            let source = SourceRef::Structured { element: id.0 };
            let tokens: Vec<_> = text.chars().map(ComparableToken::Scalar).collect();
            let view = TextView {
                origins: vec![vec![source]; tokens.len()],
                source_backed: vec![true; tokens.len()],
                tokens,
                normalization: TextNormalization::Exact,
            };
            graph.nodes.push(GraphNode {
                id,
                kind: NodeKind::Paragraph,
                pages: Vec::new(),
                sources: vec![source],
                identity: None,
                basis: ViewBasis::SourceStructure,
                content: NodeContent::Text { view },
            });
            graph.edges.push(GraphEdge {
                from: NodeId(0),
                to: id,
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::SourceStructure,
            });
        }
        graph
    }

    pub(super) fn search() -> TextCandidateSearch {
        TextCandidateSearch {
            examined_pairs: 0,
            source_ownership_visits: 0,
            source_conflict_checks: 0,
            source_search_states: 0,
            protected_correspondences: BTreeSet::new(),
            old_nodes: BTreeSet::new(),
            new_nodes: BTreeSet::new(),
            token_visits: 0,
            feature_entries: 0,
            index_entries: 0,
            exhaustive: true,
        }
    }

    pub(super) fn candidates() -> ScopeProposals {
        ScopeProposals {
            proposals: Vec::new(),
            examined_pairs: 0,
            index_work: 0,
            group_token_checks: 0,
            group_constraint_checks: 0,
            exhaustive: true,
            incomplete_nodes: None,
        }
    }

    fn pairs(
        old: &DocumentGraph,
        new: &DocumentGraph,
        indexed: bool,
    ) -> (ScopeProposals, TextCandidateSearch) {
        let left: Vec<_> = old.nodes.iter().skip(1).collect();
        let right: Vec<_> = new.nodes.iter().skip(1).collect();
        let mut search = search();
        let limits = TextCandidateLimits::default();
        let left = features(&left, &mut search, limits).expect("bounded features");
        let mut right = features(&right, &mut search, limits).expect("bounded features");
        let mut candidates = candidates();
        if indexed {
            append_indexed_pairs(
                &left,
                &mut right,
                &mut candidates,
                &mut search,
                MatchingLimits::default(),
                limits,
            );
        } else {
            append_dense_pairs(
                &left,
                &right,
                &mut candidates,
                &mut search,
                MatchingLimits::default(),
                limits,
            );
        }
        (candidates, search)
    }

    fn oracle_mandatory(proposals: &[CorrespondenceProposal]) -> BTreeSet<usize> {
        let mut best = [0u64; 2];
        let mut mandatory = BTreeSet::new();
        for mask in 0..(1usize << proposals.len()) {
            let mut old = BTreeSet::new();
            let mut new = BTreeSet::new();
            let mut chosen = BTreeSet::new();
            let mut score = [0u64; 2];
            let mut valid = true;
            for (index, proposal) in proposals.iter().enumerate() {
                if mask & (1 << index) == 0 {
                    continue;
                }
                if !old.insert(proposal.old[0]) || !new.insert(proposal.new[0]) {
                    valid = false;
                    break;
                }
                chosen.insert(index);
                let priority = usize::from(proposal.basis != ProposalBasis::LiteralContent);
                score[priority] += u64::from(proposal.weight);
            }
            if !valid || score < best {
                continue;
            }
            if score > best {
                best = score;
                mandatory = chosen;
            } else {
                mandatory.retain(|index| chosen.contains(index));
            }
        }
        mandatory
    }

    #[test]
    fn indexed_universe_matches_dense_weights_order_and_exhaustive_assignments() {
        let texts = [
            "a",
            "aa",
            "aaa",
            "aabaa",
            "abaab",
            "xyz",
            "日本語",
            "日本文",
        ];
        for seed in 0..96 {
            let mut old = graph(&[
                texts[seed % 8],
                texts[(seed / 3 + 1) % 8],
                texts[(seed / 7 + 3) % 8],
            ]);
            let mut new = graph(&[
                texts[(seed + 2) % 8],
                texts[(seed / 5) % 8],
                texts[(seed / 11 + 4) % 8],
            ]);
            if seed % 3 == 0 {
                old.nodes[2].kind = NodeKind::Header;
            }
            if seed % 5 == 0 {
                new.nodes[3].kind = NodeKind::Header;
            }
            let (dense, dense_search) = pairs(&old, &new, false);
            let (indexed, indexed_search) = pairs(&old, &new, true);
            assert!(dense_search.exhaustive && indexed_search.exhaustive);
            assert_eq!(dense.proposals, indexed.proposals, "seed={seed}");
            let matching = solve_correspondence_scope(
                &old,
                &new,
                SCOPE,
                &indexed.proposals,
                MatchingLimits::default(),
            )
            .expect("assignment");
            assert!(
                matching.conflict_search_complete
                    && matching.components.iter().all(|c| c.exhaustive)
            );
            let mandatory: BTreeSet<_> = matching
                .components
                .iter()
                .flat_map(|c| c.mandatory.iter().copied())
                .collect();
            assert_eq!(mandatory, oracle_mandatory(&dense.proposals), "seed={seed}");
        }
    }

    #[test]
    fn zero_overlap_and_equal_multisets_do_not_prove_literal_equality() {
        let (zero, _) = pairs(&graph(&["abc"]), &graph(&["xyz"]), true);
        assert_eq!(zero.proposals.len(), 1);
        assert_eq!(zero.proposals[0].weight, 1);
        assert_eq!(zero.proposals[0].basis, ProposalBasis::TextSimilarity);
        // These strings have the same trigram multiset in a different order.
        let old = graph(&["aabaa"]);
        let new = graph(&["abaab"]);
        let (indexed, _) = pairs(&old, &new, true);
        let (dense, _) = pairs(&old, &new, false);
        assert_eq!(indexed.proposals, dense.proposals);
        assert_eq!(indexed.proposals[0].basis, ProposalBasis::TextSimilarity);
    }

    #[test]
    fn normalization_families_keep_the_same_literal_feature_objective() {
        let mut old = graph(&["-abc", "xy"]);
        let new = graph(&["abc", "-abc"]);
        if let NodeContent::Text { view } = &mut old.nodes[1].content {
            view.bind_optional_positions(vec![0]);
        }
        assert!(eligible(&old.nodes[1]));
        assert_eq!(
            pairs(&old, &new, false).0.proposals,
            pairs(&old, &new, true).0.proposals
        );
    }

    #[test]
    fn budgets_withdraw_the_text_suffix_but_keep_source_candidates() {
        let mut old = graph(&["abc", "def", "key old"]);
        let mut new = graph(&["xyz", "uvw", "key new"]);
        let key = super::super::IdentityKey {
            namespace: "fixture".into(),
            value: "key".into(),
        };
        old.nodes[3].identity = Some(key.clone());
        new.nodes[3].identity = Some(key);
        for resource in 0..3 {
            let matching = MatchingLimits {
                max_pair_checks: if resource == 0 { 1 } else { 1_000_000 },
                max_proposals: if resource == 1 { 2 } else { 10_000 },
                ..MatchingLimits::default()
            };
            let mut proposals = propose_scope_correspondences(&old, &new, SCOPE, matching)
                .expect("source candidates");
            let original = proposals.proposals.clone();
            assert_eq!(original.len(), 1);
            let limits = TextCandidateLimits {
                max_token_visits: if resource == 2 { 40 } else { 4_000_000 },
                ..TextCandidateLimits::default()
            };
            let result =
                append_text_candidates(&old, &new, SCOPE, &mut proposals, matching, limits)
                    .expect("bounded search");
            assert!(!result.exhaustive, "resource={resource}");
            assert_eq!(proposals.proposals, original);
        }
    }
}
