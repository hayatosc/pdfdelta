use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Result;

use super::{
    CorrespondenceScope, DocumentGraph, EvidenceLimits, EvidenceStore, GraphLimits,
    InterpretationStatus, LocalComparisonLimits, LocalViewComparison, MatchingLimits, NodeContent,
    NodeId, ScopeMatching, ScopeProposals, VisualCandidateLimits, VisualCandidateSearch,
    compare_local_views, compare_text_group_views, dependencies::candidate_dependencies,
    propose_scope_correspondences, solve_correspondence_scope, visual::append_visual_candidates,
};

#[derive(Clone, Copy)]
pub struct DocumentView<'a> {
    pub evidence: &'a EvidenceStore,
    pub graph: &'a DocumentGraph,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DocumentComparisonLimits {
    pub evidence: EvidenceLimits,
    pub graph: GraphLimits,
    pub matching: MatchingLimits,
    pub local: LocalComparisonLimits,
    pub visual: VisualCandidateLimits,
    pub text: super::TextCandidateLimits,
    pub keys: super::KeyPresenceLimits,
    pub extraction: super::ExtractionDependencyLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeViewComparison {
    pub candidates: ScopeProposals,
    pub matching: ScopeMatching,
    /// Histories that weighted matching alone cannot distinguish. They do not
    /// supply absence witnesses or discharge comparison coverage.
    #[serde(default)]
    pub counterpart_decisions: super::CounterpartDecisions,
    pub visual_search: VisualCandidateSearch,
    pub text_search: super::TextCandidateSearch,
    pub comparisons: Vec<LocalViewComparison>,
    /// Mandatory candidate indexes admitted after dependency checks. Retained
    /// across hierarchy traversal for cross-scope relationship comparison.
    pub accepted_correspondences: Vec<usize>,
    /// Candidate indexes requiring a more specific child scope or group view.
    pub structural_correspondences: Vec<usize>,
    /// Conditional local results never imply that an entire PDF is complete.
    pub unresolved: Vec<String>,
    #[serde(default)]
    pub extraction_dependencies: Vec<super::ExtractionDependency>,
}

#[derive(Clone, Copy, Debug)]
pub struct HierarchyLimits {
    /// Includes the root scope.
    pub max_scopes: usize,
    pub max_depth: usize,
}

impl Default for HierarchyLimits {
    fn default() -> Self {
        Self {
            max_scopes: 1024,
            max_depth: 64,
        }
    }
}

/// A child result depends on this proposal in an earlier scope result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentCorrespondence {
    pub scope: usize,
    pub proposal: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparedScope {
    pub parent: Option<ParentCorrespondence>,
    pub depth: usize,
    pub interpretation: InterpretationStatus,
    pub result: ScopeViewComparison,
}

/// Flat storage keeps traversal, serialization, and destruction stack-bounded.
/// Parent references retain the premises of every local result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentViewComparison {
    pub scopes: Vec<ComparedScope>,
    pub relations: Vec<super::RelationComparison>,
    pub relation_unresolved: Vec<String>,
    /// Raw key-domain facts do not discharge text or semantic relationship claims.
    #[serde(default)]
    pub key_presence: Option<super::KeyPresenceComparison>,
}

impl DocumentViewComparison {
    /// Named-element membership changes under the native key contract. These
    /// own identity evidence only and do not contribute character/value masks.
    pub fn keyed_element_operations(&self) -> impl Iterator<Item = &super::KeyedElementOperation> {
        self.key_presence
            .iter()
            .flat_map(|keys| keys.scoped.iter())
            .flat_map(|scoped| &scoped.operations)
    }

    pub fn comparisons(&self) -> impl Iterator<Item = &LocalViewComparison> {
        self.scopes
            .iter()
            .flat_map(|scope| &scope.result.comparisons)
    }

    pub fn relations(&self) -> impl Iterator<Item = &super::RelationComparison> {
        self.relations.iter()
    }

    /// Unmatched evidence and channel inventories remain caller obligations.
    pub fn search_resolved(&self) -> bool {
        self.relation_unresolved.is_empty()
            && self.scopes.iter().all(|scope| {
                scope.result.unresolved.is_empty()
                    && scope.result.structural_correspondences.is_empty()
            })
    }
}

/// Descends through mandatory container correspondences using the same solver
/// at each level. Independent child scopes retain separate matching budgets.
/// Parent inference propagates to descendants; a local exact diff cannot make
/// an inferred parent correspondence certain. Unvisited scopes remain pending.
///
/// # Errors
/// Rejects invalid evidence or graphs and a zero scope budget. Local search
/// limits are reported on the affected scope without discarding its siblings.
pub fn compare_document_views(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    root: CorrespondenceScope,
    limits: DocumentComparisonLimits,
    hierarchy: HierarchyLimits,
) -> Result<DocumentViewComparison> {
    if hierarchy.max_scopes == 0 {
        return Err(super::evidence::invalid(
            "document comparison requires a root scope budget",
        ));
    }
    old.graph
        .validate(old.evidence, limits.evidence, limits.graph)?;
    new.graph
        .validate(new.evidence, limits.evidence, limits.graph)?;
    let old_nodes: BTreeMap<_, _> = old.graph.nodes.iter().map(|node| (node.id, node)).collect();
    let new_nodes: BTreeMap<_, _> = new.graph.nodes.iter().map(|node| (node.id, node)).collect();
    let mut document = DocumentViewComparison {
        key_presence: Some(super::compare_document_keys(
            old.evidence,
            new.evidence,
            &[
                (
                    super::KeyDomain::PdfFieldName,
                    limits.matching.channels.forms,
                ),
                (
                    super::KeyDomain::PdfStructureId,
                    limits.matching.channels.text || limits.matching.channels.relations,
                ),
            ]
            .into_iter()
            .filter_map(|(domain, selected)| selected.then_some(domain))
            .collect(),
            limits.keys,
        )),
        relations: Vec::new(),
        relation_unresolved: Vec::new(),
        scopes: vec![ComparedScope {
            parent: None,
            depth: 0,
            interpretation: InterpretationStatus::ConditionalOnCorrespondence,
            result: compare_validated_scope(old, new, root, &[], limits)?,
        }],
    };
    let mut cursor = 0;
    while cursor < document.scopes.len() {
        let pending = document.scopes[cursor]
            .result
            .structural_correspondences
            .clone();
        for index in pending {
            let parent = &document.scopes[cursor];
            let proposal = &parent.result.candidates.proposals[index];
            if proposal.old.len() != 1 || proposal.new.len() != 1 {
                continue;
            }
            let a = old_nodes[&proposal.old[0]];
            let b = new_nodes[&proposal.new[0]];
            if !matches!(
                (&a.content, &b.content),
                (NodeContent::Container, NodeContent::Container)
            ) {
                continue;
            }
            if parent.depth == hierarchy.max_depth || document.scopes.len() == hierarchy.max_scopes
            {
                document.scopes[cursor]
                    .result
                    .unresolved
                    .push("hierarchy traversal budget left child scopes unexamined".into());
                break;
            }
            let depth = parent.depth + 1;
            let interpretation = if parent.interpretation == InterpretationStatus::Inferred
                || parent.result.matching.inferred_proposals.contains(&index)
            {
                InterpretationStatus::Inferred
            } else {
                InterpretationStatus::ConditionalOnCorrespondence
            };
            let axes: Vec<_> = parent
                .result
                .accepted_correspondences
                .iter()
                .filter_map(|index| {
                    let pair = &parent.result.candidates.proposals[*index];
                    (pair.old.len() == 1
                        && pair.new.len() == 1
                        && old_nodes[&pair.old[0]].kind == super::NodeKind::Column
                        && new_nodes[&pair.new[0]].kind == super::NodeKind::Column)
                        .then(|| (pair.old[0], pair.new[0]))
                })
                .collect();
            let mut result = match compare_validated_scope(
                old,
                new,
                CorrespondenceScope {
                    old: a.id,
                    new: b.id,
                },
                &axes,
                limits,
            ) {
                Ok(result) => result,
                Err(error @ (crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_))) => {
                    document.scopes[cursor]
                        .result
                        .unresolved
                        .push(format!("child correspondence {index}: {error}",));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if interpretation == InterpretationStatus::Inferred {
                for comparison in &mut result.comparisons {
                    comparison.interpretation = InterpretationStatus::Inferred;
                }
            }
            document.scopes.push(ComparedScope {
                parent: Some(ParentCorrespondence {
                    scope: cursor,
                    proposal: index,
                }),
                depth,
                interpretation,
                result,
            });
            document.scopes[cursor]
                .result
                .structural_correspondences
                .retain(|pending| *pending != index);
        }
        cursor += 1;
    }
    super::extraction_dependencies::apply(
        old,
        new,
        document.scopes.iter_mut().map(|scope| &mut scope.result),
        limits.extraction,
    );
    if limits.matching.channels.relations {
        super::relations::compare_relations(
            old,
            new,
            &mut document,
            limits.matching.max_pair_checks,
        );
    }
    super::scoped_keys::apply(old, new, &mut document, limits.keys);
    Ok(document)
}

/// Validates evidence, enumerates candidates, resolves common ownership, then
/// applies the type-specific comparison to mandatory local pairs. No supplier
/// can bypass the solver. Incomplete enumeration suppresses every component that
/// could depend on an omitted candidate. Without a bounded endpoint population,
/// the entire scope remains unresolved; independent completed components may
/// otherwise proceed. Missing visual rivals follow their evidence dependencies.
///
/// This is a scoped result, not document-wide completeness. Callers must resolve
/// child scopes, account for unmatched evidence and selected channel inventories,
/// and preserve the parent correspondence premise.
///
/// # Errors
/// Rejects invalid evidence/graphs, invalid scope references, and malformed local
/// inputs. Local resource limits are retained without discarding independent pairs.
pub fn compare_scope_views(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    scope: CorrespondenceScope,
    limits: DocumentComparisonLimits,
) -> Result<ScopeViewComparison> {
    old.graph
        .validate(old.evidence, limits.evidence, limits.graph)?;
    new.graph
        .validate(new.evidence, limits.evidence, limits.graph)?;
    let mut result = compare_validated_scope(old, new, scope, &[], limits)?;
    super::extraction_dependencies::apply(
        old,
        new,
        std::iter::once(&mut result),
        limits.extraction,
    );
    Ok(result)
}

fn compare_validated_scope(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    scope: CorrespondenceScope,
    axes: &[(NodeId, NodeId)],
    limits: DocumentComparisonLimits,
) -> Result<ScopeViewComparison> {
    let mut candidates =
        propose_scope_correspondences(old.graph, new.graph, scope, limits.matching)?;
    let source_candidates_exhaustive = candidates.exhaustive;
    let incomplete_source_nodes = candidates.incomplete_nodes.clone();
    let text_search = super::text_candidates::append_text_candidates(
        old.graph,
        new.graph,
        scope,
        &mut candidates,
        limits.matching,
        limits.text,
    )?;
    let structure_search = super::structure_candidates::append(
        old.graph,
        new.graph,
        scope,
        axes,
        &text_search.protected_correspondences,
        &mut candidates,
        limits.matching,
    )?;
    let visual_search = append_visual_candidates(
        old,
        new,
        scope,
        &mut candidates,
        limits.matching,
        limits.visual,
    )?;
    candidates.examined_pairs = candidates
        .examined_pairs
        .saturating_add(visual_search.examined_pairs)
        .saturating_add(text_search.examined_pairs);
    candidates.exhaustive &= visual_search.exhaustive && text_search.exhaustive;
    let matching = solve_correspondence_scope(
        old.graph,
        new.graph,
        scope,
        &candidates.proposals,
        limits.matching,
    )?;
    let mut result = ScopeViewComparison {
        counterpart_decisions: matching.counterpart_decisions(),
        candidates,
        matching,
        visual_search,
        text_search,
        comparisons: Vec::new(),
        accepted_correspondences: Vec::new(),
        structural_correspondences: Vec::new(),
        unresolved: Vec::new(),
        extraction_dependencies: Vec::new(),
    };
    if !source_candidates_exhaustive && incomplete_source_nodes.is_none() {
        result
            .unresolved
            .push("scope candidate enumeration is incomplete".into());
        return Ok(result);
    }
    let pending_source = if source_candidates_exhaustive {
        Default::default()
    } else {
        result.unresolved.push(
            "source candidate enumeration is incomplete in retained dependency regions".into(),
        );
        super::dependencies::pending_source_proposals(
            old.graph,
            new.graph,
            &result.matching,
            &result.candidates.proposals,
            incomplete_source_nodes.as_ref(),
        )
    };
    let old_nodes: BTreeMap<_, _> = old.graph.nodes.iter().map(|node| (node.id, node)).collect();
    let new_nodes: BTreeMap<_, _> = new.graph.nodes.iter().map(|node| (node.id, node)).collect();
    let (mut old_pending_dependencies, mut new_pending_dependencies) =
        if result.visual_search.exhaustive {
            (Default::default(), Default::default())
        } else {
            result
                .unresolved
                .push("visual candidate enumeration is incomplete".into());
            (
                candidate_dependencies(old.graph, |node| {
                    matches!(node.content, NodeContent::Visual { .. })
                }),
                candidate_dependencies(new.graph, |node| {
                    matches!(node.content, NodeContent::Visual { .. })
                }),
            )
        };
    if !result.text_search.exhaustive {
        result
            .unresolved
            .push("text candidate enumeration is incomplete".into());
        old_pending_dependencies.extend(candidate_dependencies(old.graph, |node| {
            result.text_search.old_nodes.contains(&node.id)
        }));
        new_pending_dependencies.extend(candidate_dependencies(new.graph, |node| {
            result.text_search.new_nodes.contains(&node.id)
        }));
    }
    if !structure_search.exhaustive {
        result
            .unresolved
            .push("structural candidate enumeration is incomplete".into());
        old_pending_dependencies.extend(candidate_dependencies(old.graph, |node| {
            structure_search.old.contains(&node.id)
        }));
        new_pending_dependencies.extend(candidate_dependencies(new.graph, |node| {
            structure_search.new.contains(&node.id)
        }));
    }
    for component in &result.matching.components {
        if component
            .proposals
            .iter()
            .any(|index| pending_source.contains(index))
        {
            result
                .unresolved
                .push("a correspondence component depends on omitted source candidates".into());
            continue;
        }
        if !component.exhaustive {
            result
                .unresolved
                .push("a correspondence conflict component exceeded its search budget".into());
        } else if component.mandatory.is_empty() {
            result
                .unresolved
                .push("a correspondence conflict component has competing optima".into());
        }
        for index in &component.mandatory {
            let proposal = &result.candidates.proposals[*index];
            // Source-only mandatory matches survive every inferred supplier.
            // Other claims still depend on complete rival enumeration.
            if !result.text_search.protected_correspondences.contains(index)
                && (proposal
                    .old
                    .iter()
                    .any(|node| old_pending_dependencies.contains(node))
                    || proposal
                        .new
                        .iter()
                        .any(|node| new_pending_dependencies.contains(node)))
            {
                result.unresolved.push(format!(
                    "correspondence {index} depends on unexamined correspondence rivals",
                ));
                continue;
            }
            let a = old_nodes[&proposal.old[0]];
            let b = new_nodes[&proposal.new[0]];
            if matches!(
                (&a.content, &b.content),
                (NodeContent::Container, NodeContent::Container)
            ) {
                result.accepted_correspondences.push(*index);
                result.structural_correspondences.push(*index);
                continue;
            }
            let local = if proposal.old.len() != 1 || proposal.new.len() != 1 {
                let a: Vec<_> = proposal.old.iter().map(|node| old_nodes[node]).collect();
                let b: Vec<_> = proposal.new.iter().map(|node| new_nodes[node]).collect();
                compare_text_group_views(&a, &b, limits.local)
            } else {
                compare_local_views(a, b, old.evidence, new.evidence, limits.local)
            };
            match local {
                Ok(mut comparison) => {
                    if result.matching.inferred_proposals.contains(index) {
                        comparison.interpretation = InterpretationStatus::Inferred;
                    }
                    result.accepted_correspondences.push(*index);
                    result.comparisons.push(comparison);
                }
                Err(error @ (crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_))) => {
                    result
                        .unresolved
                        .push(format!("local correspondence {index}: {error}"));
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(result)
}
