//! Native key membership operations, distinct from character insertion/deletion.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    DocumentView, DocumentViewComparison, EdgeKind, GraphNode, InterpretationStatus,
    KeyCounterpart, KeyDomain, KeyEvidenceAction, KeyPresenceComparison, KeyPresenceLimits,
    KeyWorkStatus, NodeId, NodeKind, PresenceSide, SourceRef, StructuredValue, ViewBasis,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedKeyPolicy {
    NativeKeyMembershipV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScopedKeyCounterpart {
    Matched { node: NodeId },
    AbsentInScope { witness: usize },
    OutsideScope { node: NodeId, parent: NodeId },
    PresentOnly,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedKeyClaim {
    /// Index in the enclosing raw key comparison.
    pub claim: usize,
    pub node: Option<NodeId>,
    /// Index in the enclosing document comparison's scopes.
    pub scope: Option<usize>,
    pub counterpart: ScopedKeyCounterpart,
}

/// Complete native membership on both sides of an admitted scope premise.
/// The opposite raw population also rules out relocation elsewhere in the
/// document under the declared key contract. This report is not reusable proof
/// for changed evidence or graphs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedKeyWitness {
    pub scope: usize,
    pub domain: KeyDomain,
    pub opposite_population: usize,
    pub old_members: Vec<SourceRef>,
    pub new_members: Vec<SourceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyedElementOperationKind {
    Inserted,
    Removed,
}

/// Addition/removal of a named element under native key identity. Only the
/// identity source is owned by this operation. Review extent is not a character
/// mask: text/value content may have existed under another identity.
/// Field operations belong to the forms channel; structure-ID operations belong
/// to relationships and never become text-only change events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyedElementOperation {
    pub kind: KeyedElementOperationKind,
    pub claim: usize,
    pub node: NodeId,
    pub node_kind: NodeKind,
    pub witness: usize,
    pub identity_source: SourceRef,
    pub review_sources: Vec<SourceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedKeyMissing {
    NativeBinding,
    CompleteMembership,
    ScopeCorrespondence,
    IdentityOwnership,
    MovementResolution,
    RawCounterpart,
    WorkBudget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedKeyObligation {
    pub claim: Option<usize>,
    pub scope: Option<usize>,
    pub missing: ScopedKeyMissing,
    pub next_action: KeyEvidenceAction,
    pub work: KeyWorkStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedKeyComparison {
    pub policy: ScopedKeyPolicy,
    pub claims: Vec<ScopedKeyClaim>,
    pub witnesses: Vec<ScopedKeyWitness>,
    pub operations: Vec<KeyedElementOperation>,
    pub obligations: Vec<ScopedKeyObligation>,
    pub work: usize,
    pub exhaustive: bool,
}

struct Budget {
    remaining: usize,
    used: usize,
}

impl Budget {
    fn spend(&mut self, amount: usize) -> Option<()> {
        self.remaining = self.remaining.checked_sub(amount)?;
        self.used += amount;
        Some(())
    }
}

struct BoundKey<'a> {
    node: &'a GraphNode,
    parent: NodeId,
}

struct Index<'a> {
    root: Option<NodeId>,
    bound: BTreeMap<SourceRef, BoundKey<'a>>,
    members: BTreeMap<(KeyDomain, NodeId), Vec<SourceRef>>,
    invalid: BTreeSet<KeyDomain>,
    conflicted: BTreeSet<SourceRef>,
    consumed: BTreeSet<SourceRef>,
    scopes: BTreeMap<NodeId, Vec<usize>>,
}

fn domain(value: &StructuredValue) -> Option<KeyDomain> {
    match value {
        StructuredValue::FormField { .. } => Some(KeyDomain::PdfFieldName),
        StructuredValue::StructureElement { .. } => Some(KeyDomain::PdfStructureId),
        _ => None,
    }
}

fn binding_matches(node: &GraphNode, value: &StructuredValue, domain: KeyDomain) -> bool {
    if node.basis != ViewBasis::SourceStructure {
        return false;
    }
    let Some(key) = &node.identity else {
        return false;
    };
    let Some(Some(raw)) = super::key_presence::raw_key(value, domain) else {
        return false;
    };
    match value {
        StructuredValue::FormField { .. } => {
            node.kind == NodeKind::Field
                && key.namespace == "pdf-field-name"
                && key.value.as_bytes() == raw
        }
        StructuredValue::StructureElement { role, .. } => {
            node.kind == super::providers::role_kind(role)
                && key.namespace == "pdf-structure-id"
                && key.value.len() == raw.len().saturating_mul(2)
                && raw
                    .iter()
                    .zip(key.value.as_bytes().as_chunks::<2>().0)
                    .all(|(byte, hex)| {
                        *hex == [
                            b"0123456789abcdef"[(byte >> 4) as usize],
                            b"0123456789abcdef"[(byte & 15) as usize],
                        ]
                    })
        }
        _ => false,
    }
}

fn index<'a>(
    view: DocumentView<'a>,
    document: &DocumentViewComparison,
    old: bool,
    budget: &mut Budget,
) -> Option<Index<'a>> {
    let mut nodes = BTreeMap::new();
    let mut owners: BTreeMap<SourceRef, Vec<&GraphNode>> = BTreeMap::new();
    let mut parents: BTreeMap<NodeId, Vec<(NodeId, ViewBasis)>> = BTreeMap::new();
    for node in &view.graph.nodes {
        budget.spend(
            1 + node.sources.len()
                + node
                    .identity
                    .as_ref()
                    .map_or(0, |key| key.namespace.len().saturating_add(key.value.len())),
        )?;
        nodes.insert(node.id, node);
        for source in node
            .sources
            .iter()
            .filter(|source| matches!(source, SourceRef::Structured { .. }))
        {
            owners.entry(*source).or_default().push(node);
        }
    }
    for edge in &view.graph.edges {
        budget.spend(1)?;
        if edge.kind == EdgeKind::Contains {
            parents
                .entry(edge.to)
                .or_default()
                .push((edge.from, edge.basis));
        }
    }
    let mut roots = view.graph.nodes.iter().filter(|node| {
        node.kind == NodeKind::Document
            && !node.basis.is_inferred()
            && !parents.contains_key(&node.id)
    });
    let root = roots
        .next()
        .filter(|_| roots.next().is_none())
        .map(|node| node.id);
    let mut result = Index {
        root,
        bound: BTreeMap::new(),
        members: BTreeMap::new(),
        invalid: BTreeSet::new(),
        conflicted: BTreeSet::new(),
        consumed: BTreeSet::new(),
        scopes: BTreeMap::new(),
    };
    let mut raw_nodes = BTreeMap::new();
    for element in &view.evidence.structured {
        budget.spend(1)?;
        let Some(domain) = domain(&element.value) else {
            continue;
        };
        let source = SourceRef::Structured {
            element: element.id,
        };
        let role_bytes = match &element.value {
            StructuredValue::StructureElement { role, .. } => role.len(),
            _ => 0,
        };
        budget.spend(role_bytes)?;
        let node = owners
            .get(&source)
            .filter(|nodes| nodes.len() == 1)
            .map(|nodes| nodes[0])
            .filter(|node| binding_matches(node, &element.value, domain));
        if let Some(node) = node {
            raw_nodes.insert(source, node);
        } else {
            result.invalid.insert(domain);
        }
    }
    for element in &view.evidence.structured {
        budget.spend(1)?;
        let Some(domain) = domain(&element.value) else {
            continue;
        };
        let source = SourceRef::Structured {
            element: element.id,
        };
        let Some(&node) = raw_nodes.get(&source) else {
            continue;
        };
        let expected = match &element.value {
            StructuredValue::StructureElement {
                parent: Some(parent),
                ..
            } => raw_nodes
                .get(&SourceRef::Structured { element: *parent })
                .map(|node| node.id),
            _ => root,
        };
        let mut current = node.id;
        let actual = loop {
            budget.spend(1)?;
            let Some([(parent, basis)]) = parents.get(&current).map(Vec::as_slice) else {
                break None;
            };
            if basis.is_inferred() {
                break None;
            }
            let parent_node = nodes[parent];
            if parent_node.kind == NodeKind::Page
                && parent_node.basis == ViewBasis::NativeLayout
                && parent_node.sources.is_empty()
            {
                current = *parent;
            } else {
                break Some(*parent);
            }
        };
        if let Some(parent) = actual.filter(|parent| Some(*parent) == expected) {
            result.bound.insert(source, BoundKey { node, parent });
            result
                .members
                .entry((domain, parent))
                .or_default()
                .push(source);
        } else {
            result.invalid.insert(domain);
        }
    }
    for conflict in &view.graph.source_conflicts {
        budget.spend(1 + conflict.sources.len())?;
        result.conflicted.extend(
            conflict
                .sources
                .iter()
                .copied()
                .filter(|source| matches!(source, SourceRef::Structured { .. })),
        );
    }
    for (scope_index, scope) in document.scopes.iter().enumerate() {
        budget.spend(1)?;
        let root = if old {
            scope.result.matching.scope.old
        } else {
            scope.result.matching.scope.new
        };
        result.scopes.entry(root).or_default().push(scope_index);
        let mut used = vec![root];
        for comparison in &scope.result.comparisons {
            let members = if old {
                &comparison.old
            } else {
                &comparison.new
            };
            budget.spend(members.len())?;
            used.extend(members);
        }
        for node in used {
            budget.spend(nodes[&node].sources.len())?;
            result.consumed.extend(
                nodes[&node]
                    .sources
                    .iter()
                    .copied()
                    .filter(|source| matches!(source, SourceRef::Structured { .. })),
            );
        }
    }
    Some(result)
}

impl ScopedKeyComparison {
    fn obligation(
        &mut self,
        claim: Option<usize>,
        scope: Option<usize>,
        missing: ScopedKeyMissing,
    ) {
        let (next_action, work) = match missing {
            ScopedKeyMissing::WorkBudget => (
                KeyEvidenceAction::IncreaseSearchBudget,
                KeyWorkStatus::Limited,
            ),
            ScopedKeyMissing::NativeBinding | ScopedKeyMissing::CompleteMembership => (
                KeyEvidenceAction::RebuildNativeGraph,
                KeyWorkStatus::Incomplete,
            ),
            _ => (KeyEvidenceAction::Unavailable, KeyWorkStatus::Unavailable),
        };
        self.obligations.push(ScopedKeyObligation {
            claim,
            scope,
            missing,
            next_action,
            work,
        });
    }
}

pub(super) fn apply(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    document: &mut DocumentViewComparison,
    limits: KeyPresenceLimits,
) {
    let Some(keys) = &document.key_presence else {
        return;
    };
    let scoped = compare(
        old,
        new,
        document,
        keys,
        limits.max_work.saturating_sub(keys.work),
    );
    if let Some(keys) = &mut document.key_presence {
        keys.scoped = Some(scoped);
    }
}

fn compare(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    document: &DocumentViewComparison,
    keys: &KeyPresenceComparison,
    limit: usize,
) -> ScopedKeyComparison {
    let mut result = ScopedKeyComparison {
        policy: ScopedKeyPolicy::NativeKeyMembershipV1,
        claims: Vec::new(),
        witnesses: Vec::new(),
        operations: Vec::new(),
        obligations: Vec::new(),
        work: 0,
        exhaustive: keys.exhaustive,
    };
    if keys.claims.is_empty() {
        return result;
    }
    let mut budget = Budget {
        remaining: limit,
        used: 0,
    };
    if compare_inner(old, new, document, keys, &mut result, &mut budget).is_none() {
        result.exhaustive = false;
        result.obligation(None, None, ScopedKeyMissing::WorkBudget);
    }
    result.work = budget.used;
    result
}

fn compare_inner(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    document: &DocumentViewComparison,
    keys: &KeyPresenceComparison,
    result: &mut ScopedKeyComparison,
    budget: &mut Budget,
) -> Option<()> {
    let a = index(old, document, true, budget)?;
    let b = index(new, document, false, budget)?;
    let mut matched = BTreeMap::new();
    for claim in &keys.claims {
        budget.spend(1)?;
        if claim.side == PresenceSide::Old
            && let KeyCounterpart::Matched { source } = claim.counterpart
            && let (Some(old), Some(new)) = (a.bound.get(&claim.source), b.bound.get(&source))
        {
            matched.insert(old.node.id, new.node.id);
        }
    }
    let mut witnesses = BTreeMap::new();
    for (claim_index, claim) in keys.claims.iter().enumerate() {
        budget.spend(1)?;
        let (own, other) = if claim.side == PresenceSide::Old {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let mut record = ScopedKeyClaim {
            claim: claim_index,
            node: None,
            scope: None,
            counterpart: ScopedKeyCounterpart::Unknown,
        };
        let Some(bound) = own.bound.get(&claim.source) else {
            result.obligation(Some(claim_index), None, ScopedKeyMissing::NativeBinding);
            result.claims.push(record);
            continue;
        };
        record.node = Some(bound.node.id);
        let scope = own
            .scopes
            .get(&bound.parent)
            .filter(|scopes| scopes.len() == 1)
            .map(|scopes| scopes[0]);
        record.scope = scope;
        let Some(scope_index) = scope else {
            result.obligation(
                Some(claim_index),
                None,
                ScopedKeyMissing::ScopeCorrespondence,
            );
            result.claims.push(record);
            continue;
        };
        let scope = &document.scopes[scope_index];
        let pair = scope.result.matching.scope;
        if scope.interpretation != InterpretationStatus::ConditionalOnCorrespondence
            || !((Some(pair.old) == a.root && Some(pair.new) == b.root)
                || matched.get(&pair.old) == Some(&pair.new))
        {
            result.obligation(
                Some(claim_index),
                record.scope,
                ScopedKeyMissing::ScopeCorrespondence,
            );
            result.claims.push(record);
            continue;
        }
        if own.invalid.contains(&claim.domain) || other.invalid.contains(&claim.domain) {
            result.obligation(
                Some(claim_index),
                record.scope,
                ScopedKeyMissing::CompleteMembership,
            );
            result.claims.push(record);
            continue;
        }
        let opposite_scope = if claim.side == PresenceSide::Old {
            pair.new
        } else {
            pair.old
        };
        match claim.counterpart {
            KeyCounterpart::Matched { source } => {
                if let Some(counterpart) = other.bound.get(&source) {
                    if counterpart.parent == opposite_scope {
                        record.counterpart = ScopedKeyCounterpart::Matched {
                            node: counterpart.node.id,
                        };
                    } else {
                        record.counterpart = ScopedKeyCounterpart::OutsideScope {
                            node: counterpart.node.id,
                            parent: counterpart.parent,
                        };
                        result.obligation(
                            Some(claim_index),
                            record.scope,
                            ScopedKeyMissing::MovementResolution,
                        );
                    }
                } else {
                    result.obligation(
                        Some(claim_index),
                        record.scope,
                        ScopedKeyMissing::NativeBinding,
                    );
                }
            }
            KeyCounterpart::AbsentInDocument { population } => {
                if own.conflicted.contains(&claim.source) || own.consumed.contains(&claim.source) {
                    result.obligation(
                        Some(claim_index),
                        record.scope,
                        ScopedKeyMissing::IdentityOwnership,
                    );
                } else {
                    let witness = if let Some(index) =
                        witnesses.get(&(scope_index, claim.domain, population))
                    {
                        *index
                    } else {
                        let old_members = a
                            .members
                            .get(&(claim.domain, pair.old))
                            .map_or(&[][..], Vec::as_slice);
                        let new_members = b
                            .members
                            .get(&(claim.domain, pair.new))
                            .map_or(&[][..], Vec::as_slice);
                        budget.spend(1 + old_members.len() + new_members.len())?;
                        let index = result.witnesses.len();
                        result.witnesses.push(ScopedKeyWitness {
                            scope: scope_index,
                            domain: claim.domain,
                            opposite_population: population,
                            old_members: old_members.to_vec(),
                            new_members: new_members.to_vec(),
                        });
                        witnesses.insert((scope_index, claim.domain, population), index);
                        index
                    };
                    record.counterpart = ScopedKeyCounterpart::AbsentInScope { witness };
                    let selected = match claim.domain {
                        KeyDomain::PdfFieldName => scope.result.matching.channels.forms,
                        KeyDomain::PdfStructureId => scope.result.matching.channels.relations,
                    };
                    if selected {
                        budget.spend(1 + bound.node.sources.len())?;
                        result.operations.push(KeyedElementOperation {
                            kind: if claim.side == PresenceSide::Old {
                                KeyedElementOperationKind::Removed
                            } else {
                                KeyedElementOperationKind::Inserted
                            },
                            claim: claim_index,
                            node: bound.node.id,
                            node_kind: bound.node.kind,
                            witness,
                            identity_source: claim.source,
                            review_sources: bound.node.sources.clone(),
                        });
                    }
                }
            }
            KeyCounterpart::PresentOnly | KeyCounterpart::Unknown => {
                if matches!(claim.counterpart, KeyCounterpart::PresentOnly) {
                    record.counterpart = ScopedKeyCounterpart::PresentOnly;
                }
                result.obligation(
                    Some(claim_index),
                    record.scope,
                    ScopedKeyMissing::RawCounterpart,
                );
            }
        }
        result.claims.push(record);
    }
    Some(())
}
