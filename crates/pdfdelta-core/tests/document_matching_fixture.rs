use pdfdelta_core::{
    document::{
        AlternativeViews, CorrespondenceProposal, CorrespondenceScope, DocumentGraph, EdgeKind,
        FieldValue, GraphEdge, GraphNode, IdentityKey, MatchingAlgorithm, MatchingLimits,
        NodeContent, NodeId, NodeKind, ProposalBasis, SourceConflict, SourceRef, ViewBasis,
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

fn dense_assignment(
    size: usize,
    diagonal_weight: u32,
) -> (DocumentGraph, Vec<CorrespondenceProposal>) {
    let fields = vec![("unkeyed", "value"); size];
    let mut graph = graph(&fields, 0);
    for node in &mut graph.nodes {
        node.identity = None;
    }
    let mut proposals = Vec::new();
    for old in 1..=size {
        for new in 1..=size {
            proposals.push(CorrespondenceProposal {
                old: vec![NodeId(old as u64)],
                new: vec![NodeId(new as u64)],
                basis: ProposalBasis::Model,
                supplier: "assignment-fixture".into(),
                weight: if old == new { diagonal_weight } else { 1 },
            });
        }
    }
    (graph, proposals)
}

#[test]
fn positive_unique_model_match_retains_the_unmatched_history_at_every_weight() {
    use pdfdelta_core::document::{
        CounterpartDecisionMissing, CounterpartDecisionPolicy, CounterpartExplanation,
    };

    let old = graph(&[("old", "unrelated old value")], 0);
    let new = graph(&[("new", "entirely different new value")], 0);
    for weight in [1, 100, u32::MAX] {
        let proposals = vec![CorrespondenceProposal {
            old: vec![NodeId(1)],
            new: vec![NodeId(1)],
            basis: ProposalBasis::Model,
            supplier: "unrelated-1x1".into(),
            weight,
        }];
        let matching =
            solve_correspondence_scope(&old, &new, SCOPE, &proposals, MatchingLimits::default())
                .expect("valid 1x1 matching");
        assert_eq!(matching.components[0].mandatory, [0]);
        let decisions = matching.counterpart_decisions();
        assert_eq!(
            decisions.policy,
            CounterpartDecisionPolicy::PreserveUnmatchedAlternativesV1
        );
        assert_eq!(decisions.unresolved.len(), 1);
        let decision = &decisions.unresolved[0];
        assert_eq!(decision.proposal, 0);
        assert_eq!(
            decision.explanations,
            [
                CounterpartExplanation::Correspondence,
                CounterpartExplanation::SeparatePresence,
            ]
        );
        assert_eq!(
            decision.missing,
            [CounterpartDecisionMissing::IndependentCorrespondenceEvidence]
        );
    }
}

#[test]
fn incomplete_and_tied_matching_retain_unmatched_explanations() {
    use pdfdelta_core::document::CounterpartDecisionMissing;

    let (graph, proposals) = dense_assignment(2, 1);
    for budget in [
        0,
        MatchingLimits::default().max_assignment_work_per_component,
    ] {
        let matching = solve_correspondence_scope(
            &graph,
            &graph,
            SCOPE,
            &proposals,
            MatchingLimits {
                max_assignment_work_per_component: budget,
                ..MatchingLimits::default()
            },
        )
        .expect("valid competing assignments");
        let decisions = matching.counterpart_decisions();
        assert_eq!(decisions.unresolved.len(), proposals.len());
        assert!(decisions.unresolved.iter().all(|decision| {
            decision
                .missing
                .contains(&CounterpartDecisionMissing::ResolvedRivals)
        }));
    }
}

#[test]
fn independent_dense_assignments_bypass_subset_and_pairwise_caps() {
    for size in [5, 20, 100] {
        let (graph, proposals) = dense_assignment(size, 1_000_001);
        let result = solve_correspondence_scope(
            &graph,
            &graph,
            SCOPE,
            &proposals,
            MatchingLimits {
                max_pair_checks: 0,
                max_component_proposals: 0,
                max_states_per_component: 0,
                ..MatchingLimits::default()
            },
        )
        .expect("valid assignment fixture");
        assert!(result.conflict_search_complete);
        assert_eq!(result.conflict_checks, 0);
        assert!(result.source_only_mandatory.is_empty());
        assert_eq!(result.components.len(), 1);
        let component = &result.components[0];
        assert_eq!(component.algorithm, MatchingAlgorithm::BipartiteAssignment);
        assert!(
            component.exhaustive,
            "size={size}, work={}",
            component.assignment_work
        );
        assert_eq!(
            component.mandatory,
            (0..size)
                .map(|index| index * (size + 1))
                .collect::<Vec<_>>()
        );
        assert!(
            component
                .mandatory
                .iter()
                .all(|index| result.inferred_proposals.contains(index))
        );
        eprintln!(
            "size={size} proposals={} assignment_work={} ownership_visits={}",
            proposals.len(),
            component.assignment_work,
            result.ownership_visits
        );
    }
}

#[test]
fn tied_assignments_and_truncated_certificates_never_claim_a_unique_pair() {
    let (graph, mut proposals) = dense_assignment(5, 1);
    for _ in 0..2 {
        let result = solve_correspondence_scope(
            &graph,
            &graph,
            SCOPE,
            &proposals,
            MatchingLimits::default(),
        )
        .expect("valid assignment fixture");
        assert!(result.components[0].exhaustive);
        assert!(result.components[0].mandatory.is_empty());
        proposals.reverse();
    }
    let (_, proposals) = dense_assignment(5, 100);
    let result = solve_correspondence_scope(
        &graph,
        &graph,
        SCOPE,
        &proposals,
        MatchingLimits {
            max_assignment_work_per_component: 200,
            ..MatchingLimits::default()
        },
    )
    .expect("valid assignment fixture");
    assert!(!result.components[0].exhaustive);
    assert!(result.components[0].mandatory.is_empty());
}

#[test]
fn scoped_identity_index_does_not_spend_budget_on_unrelated_pairs() {
    let names: Vec<_> = (0..1000).map(|index| format!("field-{index}")).collect();
    let old_fields: Vec<_> = names.iter().map(|name| (name.as_str(), "before")).collect();
    let new_fields: Vec<_> = names
        .iter()
        .rev()
        .map(|name| (name.as_str(), "after"))
        .collect();
    let old = graph(&old_fields, 0);
    let new = graph(&new_fields, 1);
    let limits = MatchingLimits {
        max_pair_checks: 1000,
        ..MatchingLimits::default()
    };
    let candidates =
        propose_scope_correspondences(&old, &new, SCOPE, limits).expect("valid assignment fixture");
    assert!(candidates.exhaustive);
    assert_eq!(candidates.examined_pairs, 1000);
    assert_eq!(candidates.proposals.len(), 1000);
    for (index, proposal) in candidates.proposals.iter().enumerate() {
        assert_eq!(proposal.new, vec![NodeId((1000 - index) as u64)]);
    }
    let result = solve_correspondence_scope(&old, &new, SCOPE, &candidates.proposals, limits)
        .expect("valid assignment fixture");
    assert_eq!(result.source_only_mandatory.len(), 1000);
}

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
    assert_eq!(
        result.components[0].algorithm,
        MatchingAlgorithm::SubsetSearch
    );
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
fn certified_priority_prefix_survives_an_unfinished_residual_component() {
    let mut old = graph(&[("anchor", "same"), ("b", "same"), ("c", "same")], 0);
    old.nodes[3].sources = old.nodes[2].sources.clone();
    let new = old.clone();
    let proposals = [(1, 1), (1, 2), (2, 1), (2, 2), (2, 3), (3, 2), (3, 3)]
        .into_iter()
        .enumerate()
        .map(|(index, (old, new))| CorrespondenceProposal {
            old: vec![NodeId(old)],
            new: vec![NodeId(new)],
            basis: if index == 0 {
                ProposalBasis::ScopedIdentity
            } else {
                ProposalBasis::Model
            },
            supplier: "priority-prefix-fixture".into(),
            weight: if index == 0 { 1 } else { u32::MAX },
        })
        .collect::<Vec<_>>();
    for cap in [1, 24] {
        let result = solve_correspondence_scope(
            &old,
            &new,
            SCOPE,
            &proposals,
            MatchingLimits {
                max_component_proposals: cap,
                ..MatchingLimits::default()
            },
        )
        .expect("complete ownership and priority-prefix evidence");
        assert_eq!(result.components.len(), 1);
        assert_eq!(result.components[0].exhaustive, cap == 24);
        assert_eq!(result.components[0].mandatory, vec![0]);
        assert!(result.source_only_mandatory.contains(&0));
        assert!(
            result
                .counterpart_decisions()
                .unresolved
                .iter()
                .all(|decision| decision.proposal != 0)
        );
    }
    let stopped = solve_correspondence_scope(
        &old,
        &new,
        SCOPE,
        &proposals,
        MatchingLimits {
            max_component_proposals: 1,
            max_states_per_component: 1,
            ..MatchingLimits::default()
        },
    )
    .expect("unfinished prefix remains uncertified");
    assert!(stopped.components[0].mandatory.is_empty());
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
            max_assignment_work_per_component: 1,
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
            MatchingObjective::ScopedIdentityThenLiteralThenPaddingThenInferredStructureV4
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
        max_assignment_work_per_component: 32,
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
    // Shared source ownership requires the general conflict checks even though
    // every proposal names one node on each side.
    let mut shared = old.clone();
    shared.nodes[2].sources = shared.nodes[1].sources.clone();
    let stopped = solve_correspondence_scope(
        &shared,
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
fn exhausted_conflict_checks_preserve_disjoint_assignment_components() {
    let mut old = graph(
        &[("shared-a", "x"), ("shared-b", "x"), ("independent", "1")],
        0,
    );
    let new = graph(
        &[("shared-a", "y"), ("shared-b", "y"), ("independent", "2")],
        0,
    );
    old.nodes[2].sources = old.nodes[1].sources.clone();
    let mut proposals = propose_scope_correspondences(&old, &new, SCOPE, MatchingLimits::default())
        .expect("enumerate fields")
        .proposals;
    for _ in 0..2 {
        let result = solve_correspondence_scope(
            &old,
            &new,
            SCOPE,
            &proposals,
            MatchingLimits {
                max_pair_checks: 0,
                ..MatchingLimits::default()
            },
        )
        .expect("retain independently completed components");
        let independent = proposals
            .iter()
            .position(|proposal| proposal.old == [NodeId(3)])
            .expect("independent field");
        assert!(!result.conflict_search_complete);
        assert_eq!(result.source_only_mandatory, [independent].into());
        assert_eq!(result.components.iter().filter(|c| c.exhaustive).count(), 1);
        assert!(
            result
                .components
                .iter()
                .filter(|c| !c.exhaustive)
                .all(|c| c.mandatory.is_empty())
        );
        assert!(
            result
                .counterpart_decisions()
                .unresolved
                .iter()
                .all(|decision| decision.proposal != independent)
        );
        proposals.reverse();
    }
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
    assert!(matched.components.is_empty());
    // A partially retained duplicate-key bucket must not manufacture a unique
    // pair. Its complete endpoint sets remain unresolved without subset search.
    let pending = candidates
        .incomplete_nodes
        .expect("complete omitted bucket");
    assert_eq!(pending.old.into_iter().collect::<Vec<_>>(), vec![NodeId(1)]);
    assert_eq!(
        pending.new.into_iter().collect::<Vec<_>>(),
        vec![NodeId(1), NodeId(2)]
    );
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
