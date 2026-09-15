use std::collections::{BTreeMap, BTreeSet};

mod assignment;
mod candidates;
mod decisions;
mod index;

pub use decisions::*;

use serde::{Deserialize, Serialize};

use crate::Result;

use super::{
    DocumentGraph, GraphNode, NodeId, SourceRef,
    evidence::{bounded, invalid},
};

/// The solver establishes claims only inside this declared correspondence.
/// Scope discovery and completeness of input evidence are separate obligations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrespondenceScope {
    pub old: NodeId,
    pub new: NodeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalBasis {
    ScopedIdentity,
    TableCellIdentity,
    LiteralContent,
    /// Exact native paragraph text after excluding only edge U+0020 tokens
    /// from the boundary premise. This supplies only a non-owning boundary;
    /// complete paragraph sources and edge spaces remain uncompared.
    LiteralContentWithPadding,
    StructuralNeighbor,
    VisualSimilarity,
    TextSimilarity,
    Model,
}

/// Suppliers propose correspondence; they cannot emit accepted changes.
/// A group represents a reversible split/merge of distinct source material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrespondenceProposal {
    pub old: Vec<NodeId>,
    pub new: Vec<NodeId>,
    pub basis: ProposalBasis,
    pub supplier: String,
    /// An explicit search objective, never a source fact or confidence probability.
    pub weight: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchingLimits {
    pub channels: MatchingChannels,
    pub max_proposals: usize,
    pub max_group_nodes: usize,
    pub max_group_token_checks: usize,
    /// Bounds key hashing and retrieved literal verification independently of
    /// optional group enumeration.
    pub max_index_work: usize,
    pub max_pair_checks: usize,
    /// Bounds ownership/partition indexing and, separately per component,
    /// partition-membership checks during correspondence search.
    pub max_ownership_visits: usize,
    pub max_states_per_component: usize,
    /// Bounds assignment construction, augmentation, and all edge-exclusion
    /// certificates, including the separate source-only solve.
    pub max_assignment_work_per_component: usize,
    /// Maximum unresolved proposals per search after forced higher-priority
    /// correspondences eliminate incompatible lower-priority candidates.
    pub max_component_proposals: usize,
}

impl Default for MatchingLimits {
    fn default() -> Self {
        Self {
            channels: MatchingChannels::default(),
            max_proposals: 10_000,
            max_group_nodes: 32,
            max_group_token_checks: 1_000_000,
            max_index_work: 1_000_000,
            max_pair_checks: 1_000_000,
            max_ownership_visits: 1_000_000,
            max_states_per_component: 100_000,
            max_assignment_work_per_component: 32_000_000,
            max_component_proposals: 24,
        }
    }
}

/// Selected content families. Relation/presentation requests retain all content
/// as potential correspondence context, even when its local diff is not selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchingChannels {
    pub text: bool,
    pub visual: bool,
    pub forms: bool,
    pub relations: bool,
    pub presentation: bool,
}

impl Default for MatchingChannels {
    fn default() -> Self {
        Self {
            text: true,
            visual: true,
            forms: true,
            relations: true,
            presentation: true,
        }
    }
}

impl From<&BTreeSet<super::Channel>> for MatchingChannels {
    fn from(channels: &BTreeSet<super::Channel>) -> Self {
        use super::Channel;
        Self {
            text: channels.contains(&Channel::Text),
            visual: channels.contains(&Channel::Visual),
            forms: channels.contains(&Channel::Forms),
            relations: channels.contains(&Channel::Relations),
            presentation: channels.contains(&Channel::Presentation),
        }
    }
}

/// Selects content, linked field appearances, and required containing context.
/// Relationship/presentation requests include all supporting content views.
pub fn selected_nodes(graph: &DocumentGraph, channels: MatchingChannels) -> BTreeSet<NodeId> {
    if channels.relations
        || channels.presentation
        || (channels.text && channels.visual && channels.forms)
    {
        return graph.nodes.iter().map(|node| node.id).collect();
    }
    let mut selected: BTreeSet<_> = graph
        .nodes
        .iter()
        .filter(|node| match node.content.channel() {
            Some(super::Channel::Text) => channels.text,
            Some(super::Channel::Visual) => channels.visual,
            Some(super::Channel::Forms) => channels.forms,
            _ => false,
        })
        .map(|node| node.id)
        .collect();
    let mut parents: BTreeMap<_, Vec<_>> = BTreeMap::new();
    if channels.forms {
        for edge in &graph.edges {
            if edge.kind == super::EdgeKind::AppearanceFor && selected.contains(&edge.to) {
                selected.insert(edge.from);
            }
        }
    }
    for edge in &graph.edges {
        if edge.kind == super::EdgeKind::Contains {
            parents.entry(edge.to).or_default().push(edge.from);
        }
    }
    let mut pending: Vec<_> = selected.iter().copied().collect();
    while let Some(child) = pending.pop() {
        for parent in parents.get(&child).into_iter().flatten() {
            if selected.insert(*parent) {
                pending.push(*parent);
            }
        }
    }
    selected
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchingAlgorithm {
    #[default]
    SubsetSearch,
    BipartiteAssignment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchingComponent {
    pub proposals: Vec<usize>,
    /// Present in every optimum of the declared weighted ownership objective.
    /// Truncated searches retain only independently certified priority-prefix
    /// choices. Neither this nor uniqueness proves supplier premises.
    pub mandatory: Vec<usize>,
    pub explored_states: usize,
    #[serde(default)]
    pub assignment_work: usize,
    #[serde(default)]
    pub algorithm: MatchingAlgorithm,
    /// Whether the entire component search completed. Certified mandatory
    /// prefix choices remain valid when the residual search is incomplete.
    pub exhaustive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeMatching {
    pub channels: MatchingChannels,
    pub objective: MatchingObjective,
    pub scope: CorrespondenceScope,
    pub components: Vec<MatchingComponent>,
    pub conflict_checks: usize,
    pub ownership_visits: usize,
    /// Mandatory even when inferred suppliers are removed from the objective.
    /// Missing entries are not proven independent of inferred tie breakers.
    pub source_only_mandatory: BTreeSet<usize>,
    /// Proposals whose identity, membership, order, or supplier remains inferred.
    pub inferred_proposals: BTreeSet<usize>,
    /// False when at least one indexed dependency component has unfinished
    /// conflict checks. Other exhaustive components retain their certificates.
    pub conflict_search_complete: bool,
}

/// Source-backed scoped identities precede source-backed literal content, then
/// native text with edge padding, inferred structural correspondence, inferred
/// literal content, then other
/// inferred proposals. Weights are
/// maximized lexicographically in that order.
/// This preserves item membership when unchanged fragments compete with a
/// changed keyed value. It does not prove the supplied identity interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchingObjective {
    ScopedIdentityThenLiteralThenInferredStructureV3,
    ScopedIdentityThenLiteralThenPaddingThenInferredStructureV4,
}

/// Endpoint populations covering every omitted candidate in one source search.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteCandidateNodes {
    pub old: BTreeSet<NodeId>,
    pub new: BTreeSet<NodeId>,
}

/// Candidate enumeration is tracked separately from solver search completeness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeProposals {
    pub proposals: Vec<CorrespondenceProposal>,
    pub examined_pairs: usize,
    #[serde(default)]
    pub index_work: usize,
    /// Conservative token-comparison work charged by exact group generation.
    pub group_token_checks: usize,
    pub group_constraint_checks: usize,
    pub exhaustive: bool,
    /// When enumeration is incomplete, `Some` bounds every omitted candidate's
    /// endpoints. `None` means no such localization was established. This is a
    /// fresh-search result, not a certificate reusable with another graph.
    #[serde(default)]
    pub incomplete_nodes: Option<IncompleteCandidateNodes>,
}

/// Generates direct semantic-child candidates, traversing physical page wrappers.
/// Keyed values are matched by their keys, never by equality with another field's
/// value. Further hierarchy levels are solved inside their own matched scopes.
/// Exact leaf text also proposes ordered 1:N and N:1 views without changing tokens.
///
/// # Errors
/// Returns an error for an unknown scope or a graph with dangling child edges.
pub fn propose_scope_correspondences(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    limits: MatchingLimits,
) -> Result<ScopeProposals> {
    let left = source_children(old, scope.old, limits.channels)?;
    let right = source_children(new, scope.new, limits.channels)?;
    let mut result = ScopeProposals {
        proposals: Vec::new(),
        examined_pairs: 0,
        index_work: 0,
        group_token_checks: 0,
        group_constraint_checks: 0,
        exhaustive: true,
        incomplete_nodes: None,
    };
    let needs_cells = left
        .iter()
        .any(|node| node.kind == super::NodeKind::Cell && node.identity.is_none())
        && right
            .iter()
            .any(|node| node.kind == super::NodeKind::Cell && node.identity.is_none());
    let mut key_budget = limits.max_ownership_visits;
    let cell_keys = if needs_cells {
        match (
            super::tables::cell_keys(old, scope.old, &mut key_budget),
            super::tables::cell_keys(new, scope.new, &mut key_budget),
        ) {
            (Some(a), Some(b)) if a.exhaustive && b.exhaustive => Some((a, b)),
            _ => {
                result.group_constraint_checks = limits.max_ownership_visits - key_budget;
                result.exhaustive = false;
                return Ok(result);
            }
        }
    } else {
        None
    };
    result.group_constraint_checks = limits.max_ownership_visits - key_budget;
    let mut buckets = BTreeMap::<_, (Vec<&GraphNode>, Vec<&GraphNode>)>::new();
    for (nodes, reverse) in [(&left, false), (&right, true)] {
        for node in nodes {
            let cells = cell_keys
                .as_ref()
                .map(|keys| if reverse { &keys.1 } else { &keys.0 });
            let Some(keys) = candidates::keys(node, cells, &mut result, limits) else {
                return Ok(result);
            };
            for key in keys {
                let bucket = buckets.entry(key).or_default();
                if reverse {
                    bucket.1.push(node);
                } else {
                    bucket.0.push(node);
                }
            }
        }
    }
    let mut buckets: Vec<_> = buckets
        .into_values()
        .filter(|(a, b)| !a.is_empty() && !b.is_empty())
        .collect();
    // Scheduling small complete buckets first changes only which work fits the
    // budget. Omitted buckets retain all endpoints, never top-k uniqueness.
    buckets.sort_by_key(|(a, b)| a.len().saturating_mul(b.len()));
    result.incomplete_nodes = Some(IncompleteCandidateNodes::default());
    let mut emitted = BTreeSet::new();
    'buckets: for (left, right) in buckets {
        let pairs = left.len().saturating_mul(right.len());
        if pairs > limits.max_proposals.saturating_sub(result.proposals.len())
            || pairs > limits.max_pair_checks.saturating_sub(result.examined_pairs)
        {
            candidates::defer_bucket(&mut result, &left, &right);
            continue;
        }
        for a in &left {
            for b in &right {
                if !emitted.insert((a.id, b.id)) {
                    continue;
                }
                result.examined_pairs += 1;
                if a.kind != b.kind {
                    continue;
                }
                let basis = if a.identity.is_some() && a.identity == b.identity {
                    ProposalBasis::ScopedIdentity
                } else if a.kind == super::NodeKind::Cell {
                    let Some((old_keys, new_keys)) = &cell_keys else {
                        continue;
                    };
                    let (Some(old_key), Some(new_key)) =
                        (old_keys.keys.get(&a.id), new_keys.keys.get(&b.id))
                    else {
                        continue;
                    };
                    let work = old_key.bytes().saturating_add(new_key.bytes());
                    if work
                        > limits
                            .max_group_token_checks
                            .saturating_sub(result.group_token_checks)
                    {
                        candidates::defer_bucket(&mut result, &left, &right);
                        continue 'buckets;
                    }
                    result.group_token_checks += work;
                    if old_key.identity != new_key.identity {
                        continue;
                    }
                    ProposalBasis::TableCellIdentity
                } else if a.identity.is_none() && b.identity.is_none() {
                    let work = text_tokens(a).len().saturating_add(text_tokens(b).len());
                    if work > limits.max_index_work.saturating_sub(result.index_work) {
                        // The key population is complete. All omitted rivals
                        // remain inside this bucket even when verification stops.
                        candidates::defer_bucket(&mut result, &left, &right);
                        continue 'buckets;
                    }
                    result.index_work += work;
                    if literal_equal(a, b) {
                        ProposalBasis::LiteralContent
                    } else if let (Some(a), Some(b)) = (padding_body(a), padding_body(b))
                        && a == b
                    {
                        ProposalBasis::LiteralContentWithPadding
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };
                result.proposals.push(CorrespondenceProposal {
                    old: vec![a.id],
                    new: vec![b.id],
                    basis,
                    supplier: "typed-scope-v1".into(),
                    // Prefer a retained key over optional structural similarity,
                    // including when both interpretations remain inferred.
                    weight: if matches!(
                        basis,
                        ProposalBasis::LiteralContent | ProposalBasis::LiteralContentWithPadding
                    ) {
                        1
                    } else {
                        2
                    },
                });
            }
        }
    }
    let old_order: BTreeMap<_, _> = left
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    let new_order: BTreeMap<_, _> = right
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    result
        .proposals
        .sort_by_key(|proposal| (old_order[&proposal.old[0]], new_order[&proposal.new[0]]));
    super::groups::append_exact_groups(old, new, scope, &mut result, limits, None)?;
    Ok(result)
}

fn literal_equal(a: &GraphNode, b: &GraphNode) -> bool {
    match (&a.content, &b.content) {
        (super::NodeContent::Text { view: a }, super::NodeContent::Text { view: b }) => {
            !a.tokens.is_empty()
                && a.tokens == b.tokens
                && a.normalization == super::TextNormalization::Exact
                && b.normalization == super::TextNormalization::Exact
        }
        _ => false,
    }
}

pub(super) fn selected_children(
    graph: &DocumentGraph,
    root: NodeId,
    channels: MatchingChannels,
) -> Result<Vec<&GraphNode>> {
    scope_children(graph, root, channels, true)
}

pub(super) fn source_children(
    graph: &DocumentGraph,
    root: NodeId,
    channels: MatchingChannels,
) -> Result<Vec<&GraphNode>> {
    scope_children(graph, root, channels, false)
}

fn scope_children(
    graph: &DocumentGraph,
    root: NodeId,
    channels: MatchingChannels,
    include_archives: bool,
) -> Result<Vec<&GraphNode>> {
    let selected = selected_nodes(graph, channels);
    Ok(semantic_children(graph, root, include_archives)?
        .into_iter()
        .filter(|node| selected.contains(&node.id))
        .collect())
}

fn semantic_children(
    graph: &DocumentGraph,
    root: NodeId,
    include_archives: bool,
) -> Result<Vec<&GraphNode>> {
    let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (node.id, node)).collect();
    if !nodes.contains_key(&root) {
        return Err(invalid("unknown correspondence scope"));
    }
    let mut children: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for edge in &graph.edges {
        if edge.kind == super::EdgeKind::Contains {
            children.entry(edge.from).or_default().push(edge.to);
        }
    }
    let partitions: BTreeSet<_> = graph
        .alternatives
        .iter()
        .filter(|_| include_archives)
        .flat_map(|alternative| {
            alternative.partitions.iter().map(|partition| {
                let mut members = partition.clone();
                members.sort_unstable();
                (alternative.parent, members)
            })
        })
        .collect();
    let mut archives = BTreeSet::new();
    // Only a complete declared partition makes an untyped container transparent.
    // Other unknown containers retain their semantic scope boundary.
    for (parent, members) in children.iter().filter(|_| include_archives) {
        for member in members {
            let Some(node) = nodes.get(member) else {
                continue;
            };
            if node.kind != super::NodeKind::Unknown
                || node.identity.is_some()
                || !matches!(node.content, super::NodeContent::Container)
            {
                continue;
            }
            let Some(contents) = children.get(member) else {
                continue;
            };
            let mut contents = contents.clone();
            contents.sort_unstable();
            if partitions.contains(&(*parent, contents)) {
                archives.insert(*member);
            }
        }
    }
    let mut result = Vec::new();
    let mut pending = vec![root];
    let mut visited = BTreeSet::from([root]);
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            let Some(node) = nodes.get(child) else {
                return Err(invalid("dangling scope child"));
            };
            if !visited.insert(*child) {
                return Err(invalid("repeated scope child"));
            }
            if node.kind == super::NodeKind::Page || archives.contains(child) {
                pending.push(*child);
            } else {
                result.push(*node);
                for archive in children.get(child).into_iter().flatten() {
                    if archives.contains(archive) && visited.insert(*archive) {
                        pending.push(*archive);
                    }
                }
            }
        }
    }
    Ok(result)
}

struct Ownership {
    nodes: BTreeSet<NodeId>,
    sources: BTreeSet<SourceRef>,
    /// Alternative-group index to partitions compatible with this proposal.
    partitions: BTreeMap<usize, BTreeSet<usize>>,
}

/// Resolves competing suppliers with one source-ownership objective.
///
/// This function expects graphs already validated against their evidence stores.
/// It checks proposal scope and ownership itself. Results are conditional on the
/// supplied candidates; callers must independently track candidate enumeration,
/// extraction completeness, and inferred structure. Page identifiers are never
/// correspondence keys. Exhaustion affects only the connected conflict component.
///
/// # Errors
/// Rejects unknown/out-of-scope nodes, overlapping groups, empty proposals,
/// incompatible kinds, duplicate correspondences, and excessive input sizes.
pub fn solve_correspondence_scope(
    old: &DocumentGraph,
    new: &DocumentGraph,
    scope: CorrespondenceScope,
    proposals: &[CorrespondenceProposal],
    limits: MatchingLimits,
) -> Result<ScopeMatching> {
    bounded(
        proposals.len(),
        limits.max_proposals,
        "correspondence proposals",
    )?;
    let old_nodes: BTreeMap<_, _> = old.nodes.iter().map(|node| (node.id, node)).collect();
    let new_nodes: BTreeMap<_, _> = new.nodes.iter().map(|node| (node.id, node)).collect();
    let old_containment = containment(old);
    let new_containment = containment(new);
    let mut old_scope = descendants(&old_containment, scope.old, &old_nodes)?;
    let mut new_scope = descendants(&new_containment, scope.new, &new_nodes)?;
    let old_selected = selected_nodes(old, limits.channels);
    let new_selected = selected_nodes(new, limits.channels);
    old_scope.retain(|node| old_selected.contains(node));
    new_scope.retain(|node| new_selected.contains(node));
    let old_children: BTreeSet<_> = selected_children(old, scope.old, limits.channels)?
        .iter()
        .map(|node| node.id)
        .collect();
    let new_children: BTreeSet<_> = selected_children(new, scope.new, limits.channels)?
        .iter()
        .map(|node| node.id)
        .collect();
    let mut signatures = BTreeSet::new();
    let mut ownership = Vec::with_capacity(proposals.len());
    let mut source_premises = Vec::with_capacity(proposals.len());
    let mut assignment_eligible = Vec::with_capacity(proposals.len());
    let mut ownership_budget = limits.max_ownership_visits;
    let cell_keys = if proposals
        .iter()
        .any(|proposal| proposal.basis == ProposalBasis::TableCellIdentity)
    {
        let a = super::tables::cell_keys(old, scope.old, &mut ownership_budget);
        let b = super::tables::cell_keys(new, scope.new, &mut ownership_budget);
        match (a, b) {
            (Some(a), Some(b)) if a.exhaustive && b.exhaustive => Some((a, b)),
            (Some(_), Some(_)) => {
                return Err(crate::Error::Unresolved(
                    "table axis identity population is incomplete".into(),
                ));
            }
            _ => {
                return Err(crate::Error::LimitExceeded {
                    resource: "table cell identity validation",
                    limit: limits.max_ownership_visits,
                });
            }
        }
    } else {
        None
    };

    for proposal in proposals {
        if proposal.weight == 0 || proposal.supplier.is_empty() || proposal.supplier.len() > 1024 {
            return Err(invalid("invalid correspondence objective or supplier"));
        }
        let mut left = group_ownership(
            &proposal.old,
            &old_nodes,
            &old_scope,
            &old_containment,
            &old.source_conflicts,
            limits,
            &mut ownership_budget,
        )?;
        let mut right = group_ownership(
            &proposal.new,
            &new_nodes,
            &new_scope,
            &new_containment,
            &new.source_conflicts,
            limits,
            &mut ownership_budget,
        )?;
        if !constrain_partitions(&mut left, &old.alternatives, limits, &mut ownership_budget)?
            || !constrain_partitions(&mut right, &new.alternatives, limits, &mut ownership_budget)?
        {
            return Err(invalid(
                "correspondence group mixes incompatible partitions",
            ));
        }
        validate_group_order(&proposal.old, old)?;
        validate_group_order(&proposal.new, new)?;
        validate_premise(
            proposal,
            &old_nodes,
            &new_nodes,
            &old_children,
            &new_children,
        )?;
        let cell_source_backed = if proposal.basis == ProposalBasis::TableCellIdentity {
            if proposal.old.len() != 1 || proposal.new.len() != 1 {
                return Err(invalid("cell identity requires one cell on each side"));
            }
            let Some((a, b)) = &cell_keys else {
                return Err(invalid("missing cell identity index"));
            };
            let (Some(a), Some(b)) = (a.keys.get(&proposal.old[0]), b.keys.get(&proposal.new[0]))
            else {
                return Err(invalid(
                    "cell identity lacks unique row and column membership",
                ));
            };
            ownership_budget = ownership_budget
                .checked_sub(a.bytes().saturating_add(b.bytes()))
                .ok_or(crate::Error::LimitExceeded {
                    resource: "table cell identity validation",
                    limit: limits.max_ownership_visits,
                })?;
            if a.identity != b.identity {
                return Err(invalid("cell supplier changes row or column identity"));
            }
            a.source_backed && b.source_backed
        } else {
            true
        };
        source_premises.push(
            cell_source_backed
                && left.partitions.iter().all(|(group, allowed)| {
                    allowed.len() == old.alternatives[*group].partitions.len()
                })
                && right.partitions.iter().all(|(group, allowed)| {
                    allowed.len() == new.alternatives[*group].partitions.len()
                })
                && !group_order_is_inferred(&proposal.old, old)
                && !group_order_is_inferred(&proposal.new, new)
                && matches!(
                    proposal.basis,
                    ProposalBasis::ScopedIdentity
                        | ProposalBasis::TableCellIdentity
                        | ProposalBasis::LiteralContent
                        | ProposalBasis::LiteralContentWithPadding
                )
                && proposal
                    .old
                    .iter()
                    .all(|id| !old_nodes[id].basis.is_inferred())
                && proposal
                    .new
                    .iter()
                    .all(|id| !new_nodes[id].basis.is_inferred()),
        );
        let kinds: BTreeSet<_> = proposal
            .old
            .iter()
            .map(|id| old_nodes[id].kind)
            .chain(proposal.new.iter().map(|id| new_nodes[id].kind))
            .collect();
        if kinds.len() != 1 {
            return Err(invalid("correspondence combines incompatible node kinds"));
        }
        if !signatures.insert((left.nodes.clone(), right.nodes.clone())) {
            return Err(invalid(
                "duplicate correspondence must combine supplier evidence",
            ));
        }
        assignment_eligible.push(
            proposal.old.len() == 1
                && proposal.new.len() == 1
                && left.nodes.len() == 1
                && right.nodes.len() == 1
                && left.partitions.is_empty()
                && right.partitions.is_empty()
                && !old_containment.contains_key(&proposal.old[0])
                && !new_containment.contains_key(&proposal.new[0])
                && !matches!(
                    old_nodes[&proposal.old[0]].content,
                    super::NodeContent::Container
                )
                && !matches!(
                    new_nodes[&proposal.new[0]].content,
                    super::NodeContent::Container
                ),
        );
        ownership.push((left, right));
    }
    let mut conflicts = vec![BTreeSet::new(); proposals.len()];
    let dependencies = index::dependencies(
        &ownership,
        old,
        new,
        &mut assignment_eligible,
        limits,
        &mut ownership_budget,
    )?;
    let mut conflict_checks = 0;
    let mut conflict_search_complete = true;
    let mut unseen: BTreeSet<_> = (0..proposals.len()).collect();
    let mut components = Vec::new();
    let mut source_only_mandatory = BTreeSet::new();
    let mut inferred_proposals: BTreeSet<_> = source_premises
        .iter()
        .enumerate()
        .filter_map(|(index, backed)| (!backed).then_some(index))
        .collect();
    while let Some(start) = unseen.pop_first() {
        let mut component = BTreeSet::from([start]);
        let mut pending = vec![start];
        while let Some(index) = pending.pop() {
            for neighbor in &dependencies[index] {
                if unseen.remove(neighbor) {
                    component.insert(*neighbor);
                    pending.push(*neighbor);
                }
            }
        }
        let indices: Vec<_> = component.into_iter().collect();
        let is_assignment = indices.iter().all(|index| assignment_eligible[*index]);
        if !is_assignment {
            let count = indices.len();
            // Divide an even factor first so the triangular count cannot
            // overflow merely because the undivided product is too large.
            let required = (count / 2).checked_mul(count - 1 + count % 2);
            if required.is_none_or(|checks| {
                checks > limits.max_pair_checks.saturating_sub(conflict_checks)
            }) {
                conflict_search_complete = false;
                // A partial conflict graph supplies no comparison. The complete
                // ownership index separates this component from all others, so
                // skip unusable work and preserve their remaining check budget.
                components.push(MatchingComponent {
                    proposals: indices,
                    mandatory: Vec::new(),
                    explored_states: 0,
                    assignment_work: 0,
                    algorithm: MatchingAlgorithm::SubsetSearch,
                    exhaustive: false,
                });
                continue;
            }
            for (offset, a) in indices.iter().copied().enumerate() {
                for b in indices.iter().copied().skip(offset + 1) {
                    conflict_checks += 1;
                    if overlaps(&ownership[a].0, &ownership[b].0, old)
                        || overlaps(&ownership[a].1, &ownership[b].1, new)
                    {
                        conflicts[a].insert(b);
                        conflicts[b].insert(a);
                    }
                }
            }
        }
        let solve = |indices, limits: MatchingLimits| {
            if is_assignment {
                assignment::solve(
                    indices,
                    proposals,
                    &source_premises,
                    limits.max_assignment_work_per_component,
                )
            } else {
                solve_component(
                    indices,
                    proposals,
                    &source_premises,
                    &conflicts,
                    &ownership,
                    limits,
                )
            }
        };
        let mut result = solve(indices.clone(), limits);
        if indices.iter().all(|index| source_premises[*index]) {
            source_only_mandatory.extend(result.mandatory.iter().copied());
        } else {
            let source_indices: Vec<_> = indices
                .into_iter()
                .filter(|index| source_premises[*index])
                .collect();
            if !source_indices.is_empty() {
                let source_result = solve(
                    source_indices,
                    MatchingLimits {
                        max_states_per_component: limits
                            .max_states_per_component
                            .saturating_sub(result.explored_states),
                        max_assignment_work_per_component: limits
                            .max_assignment_work_per_component
                            .saturating_sub(result.assignment_work),
                        ..limits
                    },
                );
                result.explored_states += source_result.explored_states;
                result.assignment_work += source_result.assignment_work;
                source_only_mandatory.extend(source_result.mandatory);
            }
            inferred_proposals.extend(
                result
                    .mandatory
                    .iter()
                    .copied()
                    .filter(|index| !source_only_mandatory.contains(index)),
            );
        }
        components.push(result);
    }
    Ok(ScopeMatching {
        channels: limits.channels,
        objective: MatchingObjective::ScopedIdentityThenLiteralThenPaddingThenInferredStructureV4,
        scope,
        components,
        conflict_checks,
        ownership_visits: limits.max_ownership_visits - ownership_budget,
        source_only_mandatory,
        inferred_proposals,
        conflict_search_complete,
    })
}

fn validate_premise(
    proposal: &CorrespondenceProposal,
    old: &BTreeMap<NodeId, &GraphNode>,
    new: &BTreeMap<NodeId, &GraphNode>,
    old_children: &BTreeSet<NodeId>,
    new_children: &BTreeSet<NodeId>,
) -> Result<()> {
    if matches!(
        proposal.basis,
        ProposalBasis::ScopedIdentity
            | ProposalBasis::TableCellIdentity
            | ProposalBasis::LiteralContent
            | ProposalBasis::LiteralContentWithPadding
    ) && (proposal.old.iter().any(|id| !old_children.contains(id))
        || proposal.new.iter().any(|id| !new_children.contains(id)))
    {
        return Err(invalid(
            "source correspondence crosses its semantic parent scope",
        ));
    }
    match proposal.basis {
        ProposalBasis::ScopedIdentity => {
            if proposal.old.len() != 1
                || proposal.new.len() != 1
                || old[&proposal.old[0]].identity.is_none()
                || old[&proposal.old[0]].identity != new[&proposal.new[0]].identity
            {
                return Err(invalid("identity supplier premise does not hold"));
            }
        }
        ProposalBasis::LiteralContentWithPadding => {
            let ([a], [b]) = (proposal.old.as_slice(), proposal.new.as_slice()) else {
                return Err(invalid(
                    "padding premise requires individual native paragraphs",
                ));
            };
            let (Some(a), Some(b)) = (padding_body(old[a]), padding_body(new[b])) else {
                return Err(invalid(
                    "padding premise requires exact nonempty native paragraph bodies",
                ));
            };
            if a != b {
                return Err(invalid("padding supplier changes interior source tokens"));
            }
        }
        ProposalBasis::LiteralContent => {
            if proposal
                .old
                .iter()
                .any(|id| old[id].kind == super::NodeKind::Cell)
                || proposal
                    .new
                    .iter()
                    .any(|id| new[id].kind == super::NodeKind::Cell)
            {
                return Err(invalid(
                    "cell correspondence requires scoped item or row/column identity",
                ));
            }
            let text = |node: &&GraphNode| match &node.content {
                super::NodeContent::Text { view } => {
                    !view.tokens.is_empty() && view.normalization == super::TextNormalization::Exact
                }
                _ => false,
            };
            if !proposal.old.iter().all(|id| text(&old[id]))
                || !proposal.new.iter().all(|id| text(&new[id]))
            {
                return Err(invalid(
                    "literal supplier requires exact nonempty text views",
                ));
            }
            if !proposal
                .old
                .iter()
                .flat_map(|id| text_tokens(old[id]))
                .eq(proposal.new.iter().flat_map(|id| text_tokens(new[id])))
            {
                return Err(invalid("literal supplier changes source tokens"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// A matching feature only: edge spaces stay in the original view and diff.
fn padding_body(node: &GraphNode) -> Option<&[crate::normalize::ComparableToken]> {
    use crate::{
        document::{NodeContent, NodeKind, TextNormalization, ViewBasis},
        normalize::ComparableToken,
    };
    if node.kind != NodeKind::Paragraph
        || node.identity.is_some()
        || node.basis != ViewBasis::NativeLayout
    {
        return None;
    }
    let NodeContent::Text { view } = &node.content else {
        return None;
    };
    if view.normalization != TextNormalization::Exact {
        return None;
    }
    let start = view
        .tokens
        .iter()
        .position(|token| *token != ComparableToken::Scalar(' '))?;
    let end = view
        .tokens
        .iter()
        .rposition(|token| *token != ComparableToken::Scalar(' '))?
        + 1;
    Some(&view.tokens[start..end])
}

fn text_tokens(node: &GraphNode) -> &[crate::normalize::ComparableToken] {
    match &node.content {
        super::NodeContent::Text { view } => &view.tokens,
        _ => &[],
    }
}

fn validate_group_order(group: &[NodeId], graph: &DocumentGraph) -> Result<()> {
    for pair in group.windows(2) {
        if !graph.edges.iter().any(|edge| {
            edge.kind == super::EdgeKind::Precedes && edge.from == pair[0] && edge.to == pair[1]
        }) {
            return Err(invalid("split/merge group lacks a retained order relation"));
        }
    }
    Ok(())
}

pub(super) fn group_order_is_inferred(group: &[NodeId], graph: &DocumentGraph) -> bool {
    if group.len() < 2 {
        return false;
    }
    if group.windows(2).any(|pair| {
        !graph.edges.iter().any(|edge| {
            edge.kind == super::EdgeKind::Precedes
                && edge.from == pair[0]
                && edge.to == pair[1]
                && !edge.basis.is_inferred()
        })
    }) {
        return true;
    }
    let positions: BTreeMap<_, _> = group
        .iter()
        .enumerate()
        .map(|(index, node)| (*node, index))
        .collect();
    graph.edges.iter().any(|edge| {
        edge.kind == super::EdgeKind::Precedes
            && positions
                .get(&edge.from)
                .zip(positions.get(&edge.to))
                .is_some_and(|(from, to)| from >= to)
    })
}

pub(super) fn containment(graph: &DocumentGraph) -> BTreeMap<NodeId, Vec<NodeId>> {
    let mut children: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for edge in &graph.edges {
        if edge.kind == super::EdgeKind::Contains {
            children.entry(edge.from).or_default().push(edge.to);
        }
    }
    children
}

pub(super) fn descendants(
    children: &BTreeMap<NodeId, Vec<NodeId>>,
    root: NodeId,
    nodes: &BTreeMap<NodeId, &GraphNode>,
) -> Result<BTreeSet<NodeId>> {
    if !nodes.contains_key(&root) {
        return Err(invalid("unknown correspondence scope"));
    }
    let mut result = BTreeSet::from([root]);
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            if result.insert(*child) {
                pending.push(*child);
            }
        }
    }
    result.remove(&root);
    Ok(result)
}

fn group_ownership(
    group: &[NodeId],
    nodes: &BTreeMap<NodeId, &GraphNode>,
    scope: &BTreeSet<NodeId>,
    children: &BTreeMap<NodeId, Vec<NodeId>>,
    conflicts: &[super::SourceConflict],
    limits: MatchingLimits,
    budget: &mut usize,
) -> Result<Ownership> {
    bounded(group.len(), limits.max_group_nodes, "correspondence group")?;
    if group.is_empty() {
        return Err(invalid("empty correspondence group"));
    }
    let mut result = Ownership {
        nodes: BTreeSet::new(),
        sources: BTreeSet::new(),
        partitions: BTreeMap::new(),
    };
    for id in group {
        let Some(node) = nodes.get(id) else {
            return Err(invalid("unknown correspondence node"));
        };
        if !scope.contains(id) || !result.nodes.insert(*id) {
            return Err(invalid("duplicate or out-of-scope correspondence node"));
        }
        let mut visited = BTreeSet::from([*id]);
        let mut pending = vec![*id];
        let mut sources = BTreeSet::new();
        while let Some(descendant) = pending.pop() {
            charge_ownership(budget, limits)?;
            if descendant != node.id && !result.nodes.insert(descendant) {
                return Err(invalid("correspondence group consumes a descendant twice"));
            }
            for source in &nodes[&descendant].sources {
                charge_ownership(budget, limits)?;
                sources.insert(*source);
            }
            for child in children.get(&descendant).into_iter().flatten() {
                if !scope.contains(child) {
                    continue;
                }
                charge_ownership(budget, limits)?;
                if visited.insert(*child) {
                    pending.push(*child);
                }
            }
        }
        if !result.sources.is_empty() {
            for conflict in conflicts {
                let mut current = false;
                let mut previous = false;
                for source in &conflict.sources {
                    charge_ownership(budget, limits)?;
                    current |= sources.contains(source);
                    previous |= result.sources.contains(source);
                    if current && previous {
                        return Err(invalid(
                            "correspondence group consumes conflicting physical sources",
                        ));
                    }
                }
            }
        }
        for source in sources {
            if !result.sources.insert(source) {
                return Err(invalid("correspondence group consumes evidence twice"));
            }
        }
    }
    Ok(result)
}

fn charge_ownership(budget: &mut usize, limits: MatchingLimits) -> Result<()> {
    *budget = budget.checked_sub(1).ok_or(crate::Error::LimitExceeded {
        resource: "correspondence descendant ownership",
        limit: limits.max_ownership_visits,
    })?;
    Ok(())
}

fn constrain_partitions(
    ownership: &mut Ownership,
    alternatives: &[super::AlternativeViews],
    limits: MatchingLimits,
    budget: &mut usize,
) -> Result<bool> {
    for (group, alternative) in alternatives.iter().enumerate() {
        charge_ownership(budget, limits)?;
        // Owning the whole parent does not choose its internal partition yet.
        if ownership.nodes.contains(&alternative.parent) {
            continue;
        }
        let mut memberships: BTreeMap<NodeId, BTreeSet<usize>> = BTreeMap::new();
        for (partition, members) in alternative.partitions.iter().enumerate() {
            charge_ownership(budget, limits)?;
            for member in members {
                charge_ownership(budget, limits)?;
                if ownership.nodes.contains(member) {
                    memberships.entry(*member).or_default().insert(partition);
                }
            }
        }
        let mut memberships = memberships.into_values();
        let Some(mut allowed) = memberships.next() else {
            continue;
        };
        for membership in memberships {
            allowed.retain(|partition| membership.contains(partition));
        }
        if allowed.is_empty() {
            return Ok(false);
        }
        ownership.partitions.insert(group, allowed);
    }
    Ok(true)
}

/// Leaf-group suppliers use the same partition constraint as the common solver.
pub(super) fn leaf_partitions_compatible(
    group: &[NodeId],
    graph: &DocumentGraph,
    checks: &mut usize,
    limit: usize,
) -> Option<bool> {
    let mut ownership = Ownership {
        nodes: group.iter().copied().collect(),
        sources: BTreeSet::new(),
        partitions: BTreeMap::new(),
    };
    let mut remaining = limit.saturating_sub(*checks);
    let result = constrain_partitions(
        &mut ownership,
        &graph.alternatives,
        MatchingLimits {
            max_ownership_visits: limit,
            ..MatchingLimits::default()
        },
        &mut remaining,
    );
    *checks = limit - remaining;
    result.ok()
}

/// Pairwise intersections are insufficient when a view belongs to several
/// partitions: all selected correspondences must share one possible partition.
fn partitions_compatible(
    index: usize,
    selected: &BTreeSet<usize>,
    ownership: &[(Ownership, Ownership)],
    budget: &mut usize,
) -> Option<bool> {
    for old in [true, false] {
        let side = |index: usize| {
            if old {
                &ownership[index].0
            } else {
                &ownership[index].1
            }
        };
        for (group, allowed) in &side(index).partitions {
            let mut possible = false;
            for partition in allowed {
                *budget = budget.checked_sub(1)?;
                let mut compatible = true;
                for selected in selected {
                    *budget = budget.checked_sub(1)?;
                    if side(*selected)
                        .partitions
                        .get(group)
                        .is_some_and(|choices| !choices.contains(partition))
                    {
                        compatible = false;
                        break;
                    }
                }
                if compatible {
                    possible = true;
                    break;
                }
            }
            if !possible {
                return Some(false);
            }
        }
    }
    Some(true)
}

fn overlaps(a: &Ownership, b: &Ownership, graph: &DocumentGraph) -> bool {
    !a.nodes.is_disjoint(&b.nodes)
        || !a.sources.is_disjoint(&b.sources)
        || graph.source_conflicts.iter().any(|conflict| {
            conflict
                .sources
                .iter()
                .any(|source| a.sources.contains(source))
                && conflict
                    .sources
                    .iter()
                    .any(|source| b.sources.contains(source))
        })
}

fn solve_component(
    indices: Vec<usize>,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
    conflicts: &[BTreeSet<usize>],
    ownership: &[(Ownership, Ownership)],
    limits: MatchingLimits,
) -> MatchingComponent {
    let mut remaining = indices.clone();
    let mut forced = BTreeSet::new();
    let mut explored_states = 0;
    let mut partition_budget = limits.max_ownership_visits;
    while remaining.len() > limits.max_component_proposals {
        let rank = remaining
            .iter()
            .map(|index| objective_class(*index, proposals, source_premises))
            .min();
        let prefix: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|index| Some(objective_class(*index, proposals, source_premises)) == rank)
            .collect();
        if prefix.len() > limits.max_component_proposals {
            break;
        }
        let prefix_result = search_component(
            prefix,
            proposals,
            source_premises,
            conflicts,
            ownership,
            MatchingLimits {
                max_states_per_component: limits
                    .max_states_per_component
                    .saturating_sub(explored_states),
                ..limits
            },
            &forced,
        );
        explored_states += prefix_result.explored_states;
        let newly_forced: Vec<_> = prefix_result
            .mandatory
            .into_iter()
            .filter(|index| !forced.contains(index))
            .collect();
        if !prefix_result.exhaustive || newly_forced.is_empty() {
            break;
        }
        forced.extend(newly_forced);
        let mut retained = Vec::new();
        for index in remaining {
            if forced.contains(&index) || !conflicts[index].is_disjoint(&forced) {
                continue;
            }
            match partitions_compatible(index, &forced, ownership, &mut partition_budget) {
                Some(true) => retained.push(index),
                Some(false) => {}
                None => {
                    return MatchingComponent {
                        proposals: indices,
                        mandatory: forced.iter().copied().collect(),
                        explored_states,
                        assignment_work: 0,
                        algorithm: MatchingAlgorithm::SubsetSearch,
                        exhaustive: false,
                    };
                }
            }
        }
        remaining = retained;
    }
    let mut result = search_component(
        remaining,
        proposals,
        source_premises,
        conflicts,
        ownership,
        MatchingLimits {
            max_states_per_component: limits
                .max_states_per_component
                .saturating_sub(explored_states),
            ..limits
        },
        &forced,
    );
    result.proposals = indices;
    result.explored_states += explored_states;
    result
}

fn objective_class(
    index: usize,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
) -> usize {
    match (source_premises[index], proposals[index].basis) {
        (true, ProposalBasis::ScopedIdentity | ProposalBasis::TableCellIdentity) => 0,
        (true, ProposalBasis::LiteralContentWithPadding) => 2,
        (true, _) => 1,
        (
            false,
            ProposalBasis::ScopedIdentity
            | ProposalBasis::TableCellIdentity
            | ProposalBasis::StructuralNeighbor,
        ) => 3,
        (false, ProposalBasis::LiteralContent) => 4,
        (false, _) => 5,
    }
}

fn search_component(
    indices: Vec<usize>,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
    conflicts: &[BTreeSet<usize>],
    ownership: &[(Ownership, Ownership)],
    limits: MatchingLimits,
    forced: &BTreeSet<usize>,
) -> MatchingComponent {
    if indices.len() > limits.max_component_proposals {
        return MatchingComponent {
            proposals: indices,
            mandatory: forced.iter().copied().collect(),
            explored_states: 0,
            assignment_work: 0,
            algorithm: MatchingAlgorithm::SubsetSearch,
            exhaustive: false,
        };
    }
    // Iterative search avoids a stack overflow on adversarial conflict chains.
    let mut pending = vec![(0, [0_u64; 6], forced.clone())];
    let mut best = [0; 6];
    let mut mandatory: Option<BTreeSet<usize>> = None;
    let mut explored_states = 0;
    let mut partition_budget = limits.max_ownership_visits;
    while let Some((offset, score, selected)) = pending.pop() {
        if explored_states == limits.max_states_per_component {
            return MatchingComponent {
                proposals: indices,
                mandatory: forced.iter().copied().collect(),
                explored_states,
                assignment_work: 0,
                algorithm: MatchingAlgorithm::SubsetSearch,
                exhaustive: false,
            };
        }
        explored_states += 1;
        if offset == indices.len() {
            if mandatory.is_none() || score > best {
                best = score;
                mandatory = Some(selected);
            } else if score == best
                && let Some(shared) = &mut mandatory
            {
                shared.retain(|index| selected.contains(index));
            }
            continue;
        }
        let index = indices[offset];
        pending.push((offset + 1, score, selected.clone()));
        if conflicts[index].is_disjoint(&selected) {
            match partitions_compatible(index, &selected, ownership, &mut partition_budget) {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    return MatchingComponent {
                        proposals: indices,
                        mandatory: forced.iter().copied().collect(),
                        explored_states,
                        assignment_work: 0,
                        algorithm: MatchingAlgorithm::SubsetSearch,
                        exhaustive: false,
                    };
                }
            }
            let mut included = selected;
            included.insert(index);
            let weight = u64::from(proposals[index].weight);
            let mut score = score;
            score[objective_class(index, proposals, source_premises)] += weight;
            pending.push((offset + 1, score, included));
        }
    }
    MatchingComponent {
        proposals: indices,
        mandatory: mandatory.unwrap_or_default().into_iter().collect(),
        explored_states,
        assignment_work: 0,
        algorithm: MatchingAlgorithm::SubsetSearch,
        exhaustive: true,
    }
}
