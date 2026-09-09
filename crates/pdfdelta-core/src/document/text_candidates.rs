//! Literal token features rank candidates without rewriting text or proving identity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    CorrespondenceProposal, CorrespondenceScope, DocumentGraph, GraphNode, MatchingLimits,
    NodeContent, NodeKind, ProposalBasis, ScopeProposals, TextNormalization,
    matching::selected_children,
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
        && matches!(&node.content, NodeContent::Text { view } if !view.tokens.is_empty() && view.normalization == TextNormalization::Exact)
}

struct Features<'a> {
    node: &'a GraphNode,
    tokens: &'a [ComparableToken],
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
        exhaustive: candidates.exhaustive,
    };
    if !search.exhaustive {
        return Ok(search);
    }
    let mut left = selected_children(old, scope.old, matching.channels)?;
    let mut right = selected_children(new, scope.new, matching.channels)?;
    if !left.iter().any(|node| eligible(node)) || !right.iter().any(|node| eligible(node)) {
        return Ok(search);
    }
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
    let (Some(left), Some(right)) = (
        features(&left, &mut search, limits),
        features(&right, &mut search, limits),
    ) else {
        candidates.proposals.truncate(start);
        search.exhaustive = false;
        return Ok(search);
    };
    let existing: BTreeSet<_> = candidates
        .proposals
        .iter()
        .filter(|proposal| proposal.old.len() == 1 && proposal.new.len() == 1)
        .map(|proposal| (proposal.old[0], proposal.new[0]))
        .collect();
    'pairs: for a in &left {
        for b in &right {
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
            candidates.proposals.push(CorrespondenceProposal {
                old: vec![a.node.id],
                new: vec![b.node.id],
                basis: if a.tokens == b.tokens {
                    ProposalBasis::LiteralContent
                } else {
                    ProposalBasis::TextSimilarity
                },
                supplier: if a.tokens == b.tokens {
                    "literal-view-equality-v1"
                } else {
                    "literal-trigram-dice-v1"
                }
                .into(),
                weight: if a.tokens == b.tokens { 1 } else { weight },
            });
        }
    }
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

/// A source correspondence present in every optimum of the first two objective
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
    if matching.conflict_search_complete {
        search
            .protected_correspondences
            .extend(matching.source_only_mandatory);
    }
    Ok(())
}
