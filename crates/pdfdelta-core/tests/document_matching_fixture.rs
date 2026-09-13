use pdfdelta_core::{
    document::{
        AlternativeViews, CorrespondenceProposal, CorrespondenceScope, DocumentGraph, EdgeKind,
        FieldValue, GraphEdge, GraphNode, IdentityKey, MatchingLimits, NodeContent, NodeId,
        NodeKind, ProposalBasis, SourceConflict, SourceRef, ViewBasis,
        propose_scope_correspondences, solve_correspondence_scope,
    },
    model::PageId,
};

fn graph(fields: &[(&str, &str)], page: u32) -> DocumentGraph {
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
    for (index, (name, value)) in fields.iter().enumerate() {
        let id = NodeId(index as u64 + 1);
        graph.nodes.push(GraphNode {
            id,
            kind: NodeKind::Field,
            pages: vec![PageId(page)],
            sources: vec![SourceRef::Structured { element: id.0 }],
            identity: Some(IdentityKey {
                namespace: "field".into(),
                value: (*name).into(),
            }),
            basis: ViewBasis::SourceStructure,
            content: NodeContent::Value {
                value: FieldValue::Text((*value).into()),
            },
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

const SCOPE: CorrespondenceScope = CorrespondenceScope {
    old: NodeId(0),
    new: NodeId(0),
};

#[test]
fn disjoint_sources_cannot_mix_incompatible_partitions() {
    let mut old = graph(&[("a", "a"), ("b", "b"), ("c", "a"), ("d", "b")], 0);
    old.nodes[3].sources = old.nodes[1].sources.clone();
    old.nodes[4].sources = old.nodes[2].sources.clone();
    old.nodes[0].sources = vec![
        SourceRef::Structured { element: 1 },
        SourceRef::Structured { element: 2 },
    ];
    old.alternatives.push(AlternativeViews {
        parent: NodeId(0),
        partitions: vec![vec![NodeId(1), NodeId(2)], vec![NodeId(3), NodeId(4)]],
    });
    let new = old.clone();
    let mut proposals: Vec<_> = [1, 4]
        .into_iter()
        .map(|id| CorrespondenceProposal {
            old: vec![NodeId(id)],
            new: vec![NodeId(id)],
            basis: ProposalBasis::Model,
            supplier: "alternative-views".into(),
            weight: 1,
        })
        .collect();
    let result =
        solve_correspondence_scope(&old, &new, SCOPE, &proposals, MatchingLimits::default())
            .expect("partition rivals share a solver component");
    assert_eq!(result.components.len(), 1);
    assert!(result.components[0].exhaustive);
    assert!(result.components[0].mandatory.is_empty());
    proposals[1].old = vec![NodeId(2)];
    proposals[1].new = vec![NodeId(2)];
    let result =
        solve_correspondence_scope(&old, &new, SCOPE, &proposals, MatchingLimits::default())
            .expect("members of one partition remain compatible");
    assert_eq!(result.components[0].mandatory, vec![0, 1]);
}

#[test]
fn shared_views_require_one_partition_for_the_whole_selection() {
    let mut old = graph(
        &[
            ("a", "a"),
            ("b", "b"),
            ("c", "c"),
            ("d", "a"),
            ("e", "b"),
            ("f", "c"),
            ("independent", "x"),
        ],
        0,
    );
    for (copy, original) in [(4, 1), (5, 2), (6, 3)] {
        old.nodes[copy].sources = old.nodes[original].sources.clone();
    }
    old.nodes[0].sources = (1..=3)
        .map(|element| SourceRef::Structured { element })
        .collect();
    old.alternatives.push(AlternativeViews {
        parent: NodeId(0),
        partitions: vec![
            vec![NodeId(1), NodeId(2), NodeId(6)],
            vec![NodeId(4), NodeId(2), NodeId(3)],
            vec![NodeId(1), NodeId(5), NodeId(3)],
        ],
    });
    let new = old.clone();
    let proposals: Vec<_> = [1, 2, 3, 7]
        .into_iter()
        .map(|id| CorrespondenceProposal {
            old: vec![NodeId(id)],
            new: vec![NodeId(id)],
            basis: ProposalBasis::Model,
            supplier: "shared-alternative-views".into(),
            weight: 1,
        })
        .collect();
    for max_component_proposals in [24, 2] {
        let result = solve_correspondence_scope(
            &old,
            &new,
            SCOPE,
            &proposals,
            MatchingLimits {
                max_component_proposals,
                ..MatchingLimits::default()
            },
        )
        .expect("partition dependencies stay local");
        assert_eq!(result.components.len(), 2);
        assert_eq!(
            result.components[0].exhaustive,
            max_component_proposals == 24
        );
        assert!(result.components[0].mandatory.is_empty());
        assert_eq!(result.components[1].mandatory, vec![3]);
    }
}

#[test]
fn forced_literal_prefix_preserves_the_full_objective_under_a_small_component_cap() {
    use pdfdelta_core::{
        document::{TextNormalization, TextView},
        normalize::ComparableToken,
    };
    let mut old = graph(&[("a", "first"), ("b", "second")], 0);
    for node in old.nodes.iter_mut().skip(1) {
        let NodeContent::Value {
            value: FieldValue::Text(text),
        } = &node.content
        else {
            unreachable!()
        };
        node.content = NodeContent::Text {
            view: TextView {
                tokens: text.chars().map(ComparableToken::Scalar).collect(),
                origins: text.chars().map(|_| node.sources.clone()).collect(),
                source_backed: vec![true; text.chars().count()],
                normalization: TextNormalization::Exact,
            },
        };
        node.kind = NodeKind::Paragraph;
        node.identity = None;
        node.basis = ViewBasis::Model { backend: 0 };
    }
    let new = old.clone();
    let proposals: Vec<_> = [1, 2]
        .into_iter()
        .flat_map(|old| {
            [1, 2].into_iter().map(move |new| CorrespondenceProposal {
                old: vec![NodeId(old)],
                new: vec![NodeId(new)],
                basis: if old == new {
                    ProposalBasis::LiteralContent
                } else {
                    ProposalBasis::Model
                },
                supplier: "prefix-control".into(),
                weight: if old == new { 1 } else { u32::MAX },
            })
        })
        .collect();
    for max_component_proposals in [2, 24] {
        let result = solve_correspondence_scope(
            &old,
            &new,
            SCOPE,
            &proposals,
            MatchingLimits {
                max_component_proposals,
                ..MatchingLimits::default()
            },
        )
        .expect("bounded exact prefix");
        assert!(result.components[0].exhaustive);
        assert_eq!(result.components[0].mandatory, vec![0, 3]);
    }
    let result = solve_correspondence_scope(
        &old,
        &new,
        SCOPE,
        &proposals,
        MatchingLimits {
            max_component_proposals: 2,
            max_states_per_component: 1,
            ..MatchingLimits::default()
        },
    )
    .expect("prefix exhaustion remains visible");
    assert!(!result.components[0].exhaustive);
    assert!(result.components[0].mandatory.is_empty());
}

#[test]
fn swapped_values_follow_labels_across_page_changes() {
    let old = graph(&[("revenue", "100"), ("profit", "20")], 0);
    let new = graph(&[("profit", "100"), ("revenue", "20")], 3);
    let limits = MatchingLimits::default();
    let candidates = propose_scope_correspondences(&old, &new, SCOPE, limits)
        .expect("valid scoped correspondence");
    assert!(candidates.exhaustive);
    assert_eq!(candidates.proposals.len(), 2);
    assert_eq!(candidates.proposals[0].new, vec![NodeId(2)]);
    assert_eq!(candidates.proposals[1].new, vec![NodeId(1)]);
    let matched = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("valid scoped correspondence");
    assert_eq!(
        matched
            .components
            .iter()
            .map(|c| c.mandatory.len())
            .sum::<usize>(),
        2
    );
}

#[test]
fn keyed_value_membership_precedes_overlapping_literal_fragments() {
    use pdfdelta_core::{
        document::{MatchingObjective, TextNormalization, TextView},
        normalize::ComparableToken,
    };
    let make = |value| {
        let mut graph = graph(&[("amount", value), ("", "Revenue"), ("", "USD")], 0);
        for node in graph.nodes.iter_mut().skip(1) {
            let NodeContent::Value {
                value: FieldValue::Text(text),
            } = &node.content
            else {
                unreachable!()
            };
            node.content = NodeContent::Text {
                view: TextView {
                    tokens: text.chars().map(ComparableToken::Scalar).collect(),
                    origins: text.chars().map(|_| node.sources.clone()).collect(),
                    source_backed: vec![true; text.chars().count()],
                    normalization: TextNormalization::Exact,
                },
            };
            if node.id != NodeId(1) {
                node.identity = None;
            }
        }
        // The keyed item and its literal label/unit are alternative views of
        // overlapping evidence, so accepting them all would double-count it.
        graph.nodes[1].sources.extend([
            SourceRef::Structured { element: 2 },
            SourceRef::Structured { element: 3 },
        ]);
        graph
    };
    let mut old = make("100");
    let new = make("200");
    let limits = MatchingLimits::default();
    let mut candidates =
        propose_scope_correspondences(&old, &new, SCOPE, limits).expect("candidate supply");
    assert!(candidates.exhaustive);
    assert_eq!(candidates.proposals.len(), 3);
    for proposal in &mut candidates.proposals {
        if proposal.basis == ProposalBasis::LiteralContent {
            proposal.weight = u32::MAX;
        }
    }
    for _ in 0..2 {
        let matching = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
            .expect("common solver");
        assert_eq!(
            matching.objective,
            MatchingObjective::ScopedIdentityThenLiteralThenInferredStructureV3
        );
        assert_eq!(matching.components.len(), 1);
        let component = &matching.components[0];
        assert!(component.exhaustive);
        assert_eq!(component.mandatory.len(), 1);
        assert_eq!(
            candidates.proposals[component.mandatory[0]].basis,
            ProposalBasis::ScopedIdentity
        );
        candidates.proposals.reverse();
    }
    // A model-supplied key must not gain source-identity priority.
    old.nodes[1].basis = ViewBasis::Model { backend: 0 };
    let matching = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("inferred identity");
    assert_eq!(matching.components[0].mandatory.len(), 2);
    assert!(
        matching.components[0]
            .mandatory
            .iter()
            .all(|&index| candidates.proposals[index].basis == ProposalBasis::LiteralContent)
    );
    for node in old.nodes.iter_mut().skip(1) {
        node.basis = ViewBasis::Model { backend: 0 };
    }
    let matching = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("structural membership precedes literal scores within inference");
    assert_eq!(matching.components[0].mandatory.len(), 1);
    assert_eq!(
        candidates.proposals[matching.components[0].mandatory[0]].basis,
        ProposalBasis::ScopedIdentity
    );
}

#[test]
fn duplicate_keys_remain_ambiguous_without_blocking_other_fields() {
    let old = graph(&[("repeat", "x"), ("independent", "1")], 0);
    let new = graph(&[("repeat", "x"), ("repeat", "y"), ("independent", "2")], 0);
    let limits = MatchingLimits::default();
    let candidates = propose_scope_correspondences(&old, &new, SCOPE, limits)
        .expect("valid scoped correspondence");
    let matched = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("valid scoped correspondence");
    assert_eq!(matched.components.len(), 2);
    assert!(matched.components[0].mandatory.is_empty());
    assert_eq!(matched.components[1].mandatory, vec![2]);
}

#[test]
fn truncated_conflict_component_does_not_claim_uniqueness() {
    let old = graph(&[("repeat", "x"), ("independent", "1")], 0);
    let new = graph(&[("repeat", "x"), ("repeat", "y"), ("independent", "2")], 0);
    let limits = MatchingLimits {
        max_component_proposals: 1,
        ..MatchingLimits::default()
    };
    let candidates = propose_scope_correspondences(&old, &new, SCOPE, limits)
        .expect("valid scoped correspondence");
    let matched = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("valid scoped correspondence");
    assert!(!matched.components[0].exhaustive);
    assert!(matched.components[0].mandatory.is_empty());
    assert!(matched.components[1].exhaustive);
    assert_eq!(matched.components[1].mandatory, vec![2]);
    let stopped = solve_correspondence_scope(
        &old,
        &new,
        SCOPE,
        &candidates.proposals,
        MatchingLimits {
            max_pair_checks: 0,
            ..limits
        },
    )
    .expect("valid scoped correspondence");
    assert!(!stopped.conflict_search_complete);
    assert!(stopped.components.iter().all(|c| c.mandatory.is_empty()));
}

#[test]
fn physical_source_conflict_prevents_double_counting_different_origins() {
    let mut old = graph(&[("a", "x"), ("b", "x")], 0);
    let new = graph(&[("a", "y"), ("b", "y")], 0);
    old.source_conflicts.push(SourceConflict {
        sources: vec![
            SourceRef::Structured { element: 1 },
            SourceRef::Structured { element: 2 },
        ],
        reason: "two readings of one visible field".into(),
    });
    let limits = MatchingLimits::default();
    let candidates = propose_scope_correspondences(&old, &new, SCOPE, limits)
        .expect("valid scoped correspondence");
    let matched = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("valid scoped correspondence");
    assert_eq!(matched.components.len(), 1);
    assert!(matched.components[0].mandatory.is_empty());
}

#[test]
fn one_group_cannot_consume_conflicting_physical_sources() {
    let mut old = graph(&[("a", "x")], 0);
    let new = graph(&[("a", "x")], 0);
    old.nodes[1]
        .sources
        .push(SourceRef::Structured { element: 2 });
    old.source_conflicts.push(SourceConflict {
        sources: vec![
            SourceRef::Structured { element: 1 },
            SourceRef::Structured { element: 2 },
        ],
        reason: "two overlapping widget crops".into(),
    });
    let limits = MatchingLimits::default();
    let candidates = propose_scope_correspondences(&old, &new, SCOPE, limits)
        .expect("valid scoped correspondence");
    assert!(
        solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits).is_err(),
        "one group must not consume both sources of a conflict"
    );
}

#[test]
fn suppliers_cannot_forge_identity_or_consume_sources_twice() {
    let old = graph(&[("a", "x"), ("b", "x")], 0);
    let new = graph(&[("a", "y"), ("b", "y")], 0);
    let mut proposal = CorrespondenceProposal {
        old: vec![NodeId(1)],
        new: vec![NodeId(2)],
        basis: ProposalBasis::ScopedIdentity,
        supplier: "untrusted-supplier".into(),
        weight: 1,
    };
    assert!(
        solve_correspondence_scope(
            &old,
            &new,
            SCOPE,
            std::slice::from_ref(&proposal),
            MatchingLimits::default()
        )
        .is_err()
    );
    proposal.basis = ProposalBasis::Model;
    proposal.old.push(NodeId(1));
    assert!(
        solve_correspondence_scope(&old, &new, SCOPE, &[proposal], MatchingLimits::default())
            .is_err()
    );
}

#[test]
fn candidate_truncation_is_distinct_from_solver_exhaustion() {
    let old = graph(&[("repeat", "x")], 0);
    let new = graph(&[("repeat", "x"), ("repeat", "y")], 0);
    let limits = MatchingLimits {
        max_proposals: 1,
        ..MatchingLimits::default()
    };
    let candidates = propose_scope_correspondences(&old, &new, SCOPE, limits)
        .expect("valid scoped correspondence");
    assert!(!candidates.exhaustive);
    let matched = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("valid scoped correspondence");
    assert!(matched.components[0].exhaustive);
    // Conditional on an incomplete proposal set, this cannot establish uniqueness.
    assert_eq!(matched.components[0].mandatory, vec![0]);
}

#[test]
fn split_merge_preserves_tokens_sources_and_declared_local_order() {
    use pdfdelta_core::{
        document::{TextNormalization, TextView},
        normalize::ComparableToken,
    };

    fn text_graph(parts: &[&str]) -> DocumentGraph {
        let fields: Vec<_> = parts.iter().map(|part| ("", *part)).collect();
        let mut result = graph(&fields, 0);
        for (node, text) in result.nodes.iter_mut().skip(1).zip(parts) {
            node.kind = NodeKind::Paragraph;
            node.identity = None;
            node.content = NodeContent::Text {
                view: TextView {
                    tokens: text.chars().map(ComparableToken::Scalar).collect(),
                    origins: text.chars().map(|_| node.sources.clone()).collect(),
                    source_backed: vec![true; text.chars().count()],
                    normalization: TextNormalization::Exact,
                },
            };
        }
        for index in 1..parts.len() {
            result.edges.push(GraphEdge {
                from: NodeId(index as u64),
                to: NodeId(index as u64 + 1),
                kind: EdgeKind::Precedes,
                sources: Vec::new(),
                basis: ViewBasis::NativeLayout,
            });
        }
        result
    }
    let old = text_graph(&["abc"]);
    let mut new = text_graph(&["ab", "c"]);
    let proposal = CorrespondenceProposal {
        old: vec![NodeId(1)],
        new: vec![NodeId(1), NodeId(2)],
        basis: ProposalBasis::LiteralContent,
        supplier: "local-partition-v1".into(),
        weight: 1,
    };
    let result = solve_correspondence_scope(
        &old,
        &new,
        SCOPE,
        std::slice::from_ref(&proposal),
        MatchingLimits::default(),
    )
    .expect("valid scoped correspondence");
    assert_eq!(result.components[0].mandatory, vec![0]);
    new.source_conflicts.push(SourceConflict {
        sources: vec![
            SourceRef::Structured { element: 1 },
            SourceRef::Structured { element: 2 },
        ],
        reason: "two fragment views overlap the same physical material".into(),
    });
    assert!(
        solve_correspondence_scope(
            &old,
            &new,
            SCOPE,
            std::slice::from_ref(&proposal),
            MatchingLimits::default()
        )
        .is_err()
    );
    new.source_conflicts.clear();
    new.edges.retain(|edge| edge.kind != EdgeKind::Precedes);
    assert!(
        solve_correspondence_scope(&old, &new, SCOPE, &[proposal], MatchingLimits::default())
            .is_err()
    );
}
