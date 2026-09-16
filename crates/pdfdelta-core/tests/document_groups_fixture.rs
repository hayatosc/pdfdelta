use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, CorrespondenceScope, DocumentComparisonLimits, DocumentGraph,
        DocumentView, DocumentViewComparison, EdgeKind, EvidenceStore, HierarchyLimits,
        InterpretationStatus, NodeContent, NodeId, SourceRef, StructuredEvidence, StructuredValue,
        TypedOperation, ViewBasis, compare_document_views, compare_text_group_views,
    },
    model::Document,
    pipeline::PipelineOptions,
};

fn fixture(parts: &[&str]) -> (EvidenceStore, DocumentGraph) {
    let store = EvidenceStore {
        revision: "group-fixture".into(),
        native: Document::new(Vec::new()),
        pages: Vec::new(),
        backends: vec![BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "fixture".into(),
            version: "1".into(),
            profile: "source-structure".into(),
            model: None,
        }],
        rendered: Vec::new(),
        inventories: Vec::new(),
        key_inventories: Vec::new(),
        native_structures: Vec::new(),
        issues: Vec::new(),
        structured: parts
            .iter()
            .enumerate()
            .map(|(index, text)| StructuredEvidence {
                id: index as u64,
                page: None,
                bounds: None,
                object: None,
                backend: 0,
                value: StructuredValue::StructureElement {
                    content: None,
                    identifier: None,
                    glyphs: Vec::new(),
                    role: "paragraph".into(),
                    text: Some((*text).into()),
                    parent: None,
                    order: Some(index as u32),
                },
            })
            .collect(),
    };
    let graph = graph(&store);
    (store, graph)
}

fn graph(store: &EvidenceStore) -> DocumentGraph {
    let limits = DocumentComparisonLimits::default();
    DocumentGraph::from_evidence(
        store,
        PipelineOptions::default(),
        limits.evidence,
        limits.graph,
    )
    .expect("derive ordered text views")
}

#[test]
fn truncated_short_groups_preserve_long_independent_literals_in_both_directions() {
    let old = fixture(&[
        "A retained longer independent paragraph.",
        "abcd",
        "ef",
        "gh",
    ]);
    let new = fixture(&[
        "A retained longer independent paragraph.",
        "ab",
        "cd",
        "efgh",
    ]);
    for (left, right) in [(&old, &new), (&new, &old)] {
        let scope = CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        };
        let limits = DocumentComparisonLimits::default();
        let full = pdfdelta_core::document::propose_scope_correspondences(
            &left.1,
            &right.1,
            scope,
            limits.matching,
        )
        .expect("complete split and merge population");
        assert!(full.exhaustive);
        assert_eq!(full.proposals.len(), 3);
        let mut bounded = limits;
        bounded.matching.max_proposals = 1;
        let partial = pdfdelta_core::document::propose_scope_correspondences(
            &left.1,
            &right.1,
            scope,
            bounded.matching,
        )
        .expect("limited group population");
        assert!(!partial.exhaustive);
        assert_eq!(partial.proposals.len(), 1);
        let pending = partial
            .incomplete_nodes
            .as_ref()
            .expect("bounded omitted endpoints");
        for omitted in full
            .proposals
            .iter()
            .filter(|proposal| !partial.proposals.contains(proposal))
        {
            assert!(omitted.old.iter().all(|node| pending.old.contains(node)));
            assert!(omitted.new.iter().all(|node| pending.new.contains(node)));
        }
        assert!(!pending.old.contains(&partial.proposals[0].old[0]));
        assert!(!pending.new.contains(&partial.proposals[0].new[0]));
        let compared = compare(left, right, bounded);
        assert_eq!(compared.comparisons().count(), 1);
        assert!(
            compared
                .comparisons()
                .all(|pair| pair.compared && pair.operation.is_none())
        );
    }
}

#[test]
fn short_omitted_group_members_still_block_long_overlapping_views() {
    let mut old = fixture(&[
        "A retained longer independent paragraph.",
        "abcd",
        "ef",
        "gh",
    ]);
    let new = fixture(&[
        "A retained longer independent paragraph.",
        "ab",
        "cd",
        "efgh",
    ]);
    old.1
        .source_conflicts
        .push(pdfdelta_core::document::SourceConflict {
            sources: vec![
                SourceRef::Structured { element: 0 },
                SourceRef::Structured { element: 1 },
            ],
            reason: "short and long interpretations share physical evidence".into(),
        });
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_proposals = 1;
    let result = compare(&old, &new, limits);
    assert!(result.comparisons().all(|pair| !pair.compared));
    assert!(result.scopes[0].result.accepted_correspondences.is_empty());
}

#[test]
fn shared_prefix_search_preserves_independent_literals_within_its_budget() {
    let texts: Vec<_> = (0..150)
        .map(|index| format!("shared {index:03} {}", "x".repeat(index)))
        .collect();
    let parts: Vec<_> = texts.iter().map(String::as_str).collect();
    let old = fixture(&parts);
    let new = fixture(&parts);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 100_000;
    let result = compare(&old, &new, limits);
    assert!(result.scopes[0].result.candidates.exhaustive);
    assert!(result.scopes[0].result.candidates.group_token_checks < 100_000);
    assert_eq!(result.comparisons().count(), parts.len());
    assert!(
        result
            .comparisons()
            .all(|comparison| comparison.compared && comparison.operation.is_none())
    );
}

#[test]
fn bounded_prefix_index_preserves_long_literals_without_full_trie_expansion() {
    let texts: Vec<_> = (0..30)
        .map(|index| format!("{index:04}{}", "x".repeat(4000)))
        .collect();
    let parts: Vec<_> = texts.iter().map(String::as_str).collect();
    let old = fixture(&parts);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 10_000;
    let result = pdfdelta_core::document::propose_scope_correspondences(
        &old.1,
        &old.1,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits.matching,
    )
    .expect("bounded group index");
    assert!(
        result.exhaustive,
        "index={}, groups={}, constraints={}, pairs={}, proposals={}",
        result.index_work,
        result.group_token_checks,
        result.group_constraint_checks,
        result.examined_pairs,
        result.proposals.len()
    );
    assert_eq!(result.proposals.len(), parts.len());
    assert!(
        result
            .proposals
            .iter()
            .all(|proposal| proposal.old == proposal.new)
    );
    assert!(result.group_token_checks <= limits.matching.max_group_token_checks);
}

#[test]
fn short_group_prefix_collisions_require_the_remaining_original_tokens() {
    for (middle, expected) in [("correct", 1), ("changed", 0)] {
        let whole = "abcdefghcorrect tail";
        let part = format!("abcdefgh{middle}");
        let old = fixture(&[whole]);
        let new = fixture(&[&part, " tail"]);
        for (left, right) in [(&old.1, &new.1), (&new.1, &old.1)] {
            let result = pdfdelta_core::document::propose_scope_correspondences(
                left,
                right,
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                DocumentComparisonLimits::default().matching,
            )
            .expect("verify complete prefix");
            assert!(result.exhaustive);
            assert_eq!(result.proposals.len(), expected);
            assert!(
                result
                    .proposals
                    .iter()
                    .all(|proposal| proposal.old.len() + proposal.new.len() == 3)
            );
        }
    }
}

#[test]
fn complete_group_text_does_not_bypass_source_exclusion() {
    let old = fixture(&["abcdefghcorrect tail"]);
    let mut new = fixture(&["abcdefghcorrect", " tail"]);
    new.1
        .source_conflicts
        .push(pdfdelta_core::document::SourceConflict {
            sources: vec![
                SourceRef::Structured { element: 0 },
                SourceRef::Structured { element: 1 },
            ],
            reason: "competing views of one source region".into(),
        });
    for (left, right) in [(&old.1, &new.1), (&new.1, &old.1)] {
        let result = pdfdelta_core::document::propose_scope_correspondences(
            left,
            right,
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            DocumentComparisonLimits::default().matching,
        )
        .expect("validate exact group sources");
        assert!(result.exhaustive);
        assert!(result.proposals.is_empty());
    }
}

#[test]
fn impossible_successors_exclude_repeated_long_prefixes_before_full_comparison() {
    let prefix = format!("same-key{}", "x".repeat(504));
    let whole = format!("{prefix}A");
    let parts: Vec<_> = (0..24).flat_map(|_| [prefix.as_str(), "B"]).collect();
    let old = fixture(&[&whole]);
    let new = fixture(&parts);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 500;
    let result = pdfdelta_core::document::propose_scope_correspondences(
        &old.1,
        &new.1,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits.matching,
    )
    .expect("exclude incompatible continuations");
    assert!(result.exhaustive);
    assert!(result.proposals.is_empty());
    assert!(result.group_token_checks <= limits.matching.max_group_token_checks);
}

#[test]
fn indexed_successor_tokens_preserve_groups_among_repeated_incompatible_starts() {
    let old = fixture(&[" AX"; 40]);
    let mut parts: Vec<_> = (0..64).flat_map(|_| [" ", "B"]).collect();
    parts.extend([" ", "AX"]);
    let new = fixture(&parts);
    let scope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };
    for (left, right) in [(&old.1, &new.1), (&new.1, &old.1)] {
        let mut limits = DocumentComparisonLimits::default().matching;
        let full =
            pdfdelta_core::document::propose_scope_correspondences(left, right, scope, limits)
                .expect("complete group population");
        assert!(full.exhaustive);
        assert_eq!(full.proposals.len(), 40);
        limits.max_group_token_checks = 2_000;
        let bounded =
            pdfdelta_core::document::propose_scope_correspondences(left, right, scope, limits)
                .expect("index incompatible continuations once");
        assert!(bounded.exhaustive);
        assert_eq!(bounded.proposals, full.proposals);
    }
}

#[test]
fn successor_lookahead_retains_every_compatible_branch_and_its_ambiguity() {
    let prefix = "12345678suffix";
    let whole = format!("{prefix}A");
    let old = fixture(&[&whole]);
    let mut new = fixture(&[prefix, "B", "A", "A"]);
    let id = |element| {
        new.1
            .nodes
            .iter()
            .find(|node| node.sources == [SourceRef::Structured { element }])
            .expect("source node")
            .id
    };
    let first = id(0);
    let alternatives = [id(2), id(3)];
    for successor in alternatives {
        new.1.edges.push(pdfdelta_core::document::GraphEdge {
            from: first,
            to: successor,
            kind: EdgeKind::Precedes,
            sources: Vec::new(),
            basis: ViewBasis::SourceStructure,
        });
    }
    let scope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };
    let limits = DocumentComparisonLimits::default().matching;
    let result =
        pdfdelta_core::document::propose_scope_correspondences(&old.1, &new.1, scope, limits)
            .expect("complete successor population");
    assert!(result.exhaustive);
    assert_eq!(result.proposals.len(), 2);
    for successor in alternatives {
        assert!(
            result
                .proposals
                .iter()
                .any(|proposal| proposal.new == [first, successor])
        );
    }
    let matching = pdfdelta_core::document::solve_correspondence_scope(
        &old.1,
        &new.1,
        scope,
        &result.proposals,
        limits,
    )
    .expect("retain ambiguous exact groups");
    assert!(
        matching
            .components
            .iter()
            .all(|component| component.mandatory.is_empty())
    );
}

#[test]
fn group_successor_mismatches_charge_only_examined_tokens() {
    let old_texts: Vec<_> = (0..20)
        .map(|index| format!(" A{index:02}{}", "a".repeat(1024)))
        .collect();
    let new_texts: Vec<_> = (0..20)
        .flat_map(|index| [" ".into(), format!("B{index:02}{}", "b".repeat(1024))])
        .collect();
    let old = fixture(&old_texts.iter().map(String::as_str).collect::<Vec<_>>());
    let new = fixture(&new_texts.iter().map(String::as_str).collect::<Vec<_>>());
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 100_000;
    let result = pdfdelta_core::document::propose_scope_correspondences(
        &old.1,
        &new.1,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits.matching,
    )
    .expect("enumerate source groups");
    assert!(result.exhaustive);
    assert!(result.proposals.is_empty());
    assert!(result.group_token_checks < limits.matching.max_group_token_checks);
}

fn compare(
    old: &(EvidenceStore, DocumentGraph),
    new: &(EvidenceStore, DocumentGraph),
    limits: DocumentComparisonLimits,
) -> DocumentViewComparison {
    compare_document_views(
        DocumentView {
            evidence: &old.0,
            graph: &old.1,
        },
        DocumentView {
            evidence: &new.0,
            graph: &new.1,
        },
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("compare grouped source views")
}

fn inventoried_fixture(parts: &[&str]) -> (EvidenceStore, DocumentGraph) {
    let mut result = fixture(parts);
    result
        .0
        .inventories
        .push(pdfdelta_core::document::ChannelInventory {
            page: None,
            channel: pdfdelta_core::document::Channel::Text,
            backend: 0,
            sources: result
                .0
                .structured
                .iter()
                .map(|item| SourceRef::Structured { element: item.id })
                .collect(),
            complete: true,
        });
    result
}

#[test]
fn closed_id_free_scope_change_does_not_own_its_context() {
    let old = inventoried_fixture(&["Start boundary.", "a", "End boundary."]);
    let new = inventoried_fixture(&["Start boundary.", "aa", "End boundary."]);
    let comparison = compare(&old, &new, DocumentComparisonLimits::default());
    let reviews = &comparison.scopes[0].result.text_scope_reviews;
    assert_eq!(reviews.len(), 1, "{:#?}", comparison.scopes[0]);
    let review = &reviews[0];
    assert_eq!(review.convention, "closed-retained-order-interval-v1");
    for (graph, members, sources, boundaries) in [
        (
            &old.1,
            &review.comparison.old,
            &review.old_sources,
            &review.old_boundaries,
        ),
        (
            &new.1,
            &review.comparison.new,
            &review.new_sources,
            &review.new_boundaries,
        ),
    ] {
        let expected: Vec<_> = members
            .iter()
            .flat_map(|id| {
                graph
                    .nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .expect("member")
                    .sources
                    .iter()
                    .copied()
            })
            .collect();
        assert_eq!(*sources, expected);
        assert!(!sources.is_empty());
        assert!(boundaries.iter().all(|boundary| !boundary.is_empty()));
        assert!(
            boundaries
                .iter()
                .flatten()
                .all(|source| !sources.contains(source))
        );
    }
    assert_eq!(
        review.comparison.interpretation,
        InterpretationStatus::ConditionalOnCorrespondence
    );
    assert!(matches!(
        review.comparison.operation,
        Some(TypedOperation::TextChanged { .. })
    ));
    let mask = review
        .comparison
        .text_mask
        .as_ref()
        .expect("conditional mask");
    assert!(
        mask.old.is_empty() && mask.new.is_empty(),
        "a -> aa has no unique insertion position"
    );
    assert!(
        comparison
            .comparisons()
            .filter(|local| local.operation.is_some())
            .all(|local| local.interpretation == InterpretationStatus::Inferred)
    );
    let mut without_reviews = comparison.clone();
    without_reviews.scopes[0].result.text_scope_reviews.clear();
    let coverage = |result: &DocumentViewComparison| {
        pdfdelta_core::document::document_coverage(
            DocumentView {
                evidence: &old.0,
                graph: &old.1,
            },
            DocumentView {
                evidence: &new.0,
                graph: &new.1,
            },
            result,
            &std::collections::BTreeSet::from([pdfdelta_core::document::Channel::Text]),
        )
    };
    assert_eq!(coverage(&comparison), coverage(&without_reviews));
    assert!(!coverage(&comparison)[0].complete);
}

#[test]
fn closed_scope_split_and_reverse_keep_finite_members_and_source_masks() {
    let old = inventoried_fixture(&[
        "Start boundary.",
        "The annual fee is 100 dollars.",
        "End boundary.",
    ]);
    let mut new = inventoried_fixture(&[
        "Start boundary.",
        "The annual ",
        "fee is 200 dollars.",
        "End boundary.",
    ]);
    new.1.nodes.reverse();
    new.1.edges.reverse();
    for (a, b) in [(&old, &new), (&new, &old)] {
        let result = compare(a, b, DocumentComparisonLimits::default());
        let reviews = &result.scopes[0].result.text_scope_reviews;
        assert_eq!(reviews.len(), 1);
        let review = &reviews[0];
        assert_eq!(review.comparison.old.len(), a.0.structured.len() - 2);
        assert_eq!(review.comparison.new.len(), b.0.structured.len() - 2);
        let mask = review
            .comparison
            .text_mask
            .as_ref()
            .expect("split source mask");
        assert_eq!(mask.old.len(), 1);
        assert_eq!(mask.new.len(), 1);
        assert_eq!(mask.old[0].position, 18);
        assert_eq!(mask.new[0].position, 18);
    }
}

#[test]
fn anchors_do_not_close_detached_incomplete_or_repeated_scopes() {
    let old = inventoried_fixture(&["Start boundary.", "old value", "End boundary."]);
    let clean = inventoried_fixture(&["Start boundary.", "new value", "End boundary."]);
    let mut detached = clean.clone();
    detached
        .1
        .edges
        .retain(|edge| edge.kind != EdgeKind::Precedes);
    let mut missing_inventory = clean.clone();
    missing_inventory.0.inventories.clear();
    let mut partial = clean.clone();
    partial.1.relations_complete = false;
    let repeated = inventoried_fixture(&[
        "Start boundary.",
        "new value",
        "End boundary.",
        "Start boundary.",
        "new value",
        "End boundary.",
    ]);
    for new in [&detached, &missing_inventory, &partial, &repeated] {
        let result = compare(&old, new, DocumentComparisonLimits::default());
        assert!(result.scopes[0].result.text_scope_reviews.is_empty());
    }
}

#[test]
fn closed_scope_content_does_not_require_interior_correspondence_enumeration() {
    let old = inventoried_fixture(&["Start boundary.", "a", "End boundary."]);
    let new = inventoried_fixture(&["Start boundary.", "aa", "End boundary."]);
    let mut limits = DocumentComparisonLimits::default();
    limits.text.max_token_visits = 0;
    let result = compare(&old, &new, limits);
    let scope = &result.scopes[0].result;
    assert!(!scope.candidates.exhaustive);
    assert_eq!(scope.text_scope_reviews.len(), 1);
    let review = &scope.text_scope_reviews[0];
    assert_eq!(review.candidate_search_exhaustive, Some(false));
    let mask = review
        .comparison
        .text_mask
        .as_ref()
        .expect("conditional mask");
    assert!(mask.old.is_empty() && mask.new.is_empty());
    assert!(!scope.unresolved.is_empty());

    limits.matching.max_group_token_checks = 0;
    let result = compare(&old, &new, limits);
    assert!(
        result.scopes[0].result.text_scope_reviews.is_empty(),
        "unsearched source competitors still prevent boundary certification"
    );
}

#[test]
fn scope_review_parent_inference_is_preserved() {
    let nest = |mut fixture: (EvidenceStore, DocumentGraph)| {
        let id = NodeId(
            fixture
                .1
                .nodes
                .iter()
                .map(|node| node.id.0)
                .max()
                .expect("fixture root")
                + 1,
        );
        for edge in &mut fixture.1.edges {
            if edge.kind == EdgeKind::Contains && edge.from == NodeId(0) {
                edge.from = id;
            }
        }
        fixture.1.nodes.push(pdfdelta_core::document::GraphNode {
            id,
            kind: pdfdelta_core::document::NodeKind::Section,
            pages: Vec::new(),
            sources: Vec::new(),
            identity: Some(pdfdelta_core::document::IdentityKey {
                namespace: "inferred-section".into(),
                value: "scope".into(),
            }),
            basis: ViewBasis::ReconstructedStructure,
            content: NodeContent::Container,
        });
        fixture.1.edges.push(pdfdelta_core::document::GraphEdge {
            from: NodeId(0),
            to: id,
            kind: EdgeKind::Contains,
            sources: Vec::new(),
            basis: ViewBasis::ReconstructedStructure,
        });
        fixture
    };
    let old = nest(inventoried_fixture(&[
        "Start boundary.",
        "a",
        "End boundary.",
    ]));
    let new = nest(inventoried_fixture(&[
        "Start boundary.",
        "aa",
        "End boundary.",
    ]));
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    let reviews: Vec<_> = result
        .scopes
        .iter()
        .flat_map(|scope| &scope.result.text_scope_reviews)
        .collect();
    assert_eq!(reviews.len(), 1);
    assert_eq!(
        reviews[0].comparison.interpretation,
        InterpretationStatus::Inferred
    );
}

#[test]
fn scope_content_copy_is_not_an_exact_insertion_history() {
    let old = inventoried_fixture(&["Start boundary.", "x", "End boundary."]);
    let new = inventoried_fixture(&["Start boundary.", "x", "x", "End boundary."]);
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert_eq!(reviews.len(), 1);
    let mask = reviews[0]
        .comparison
        .text_mask
        .as_ref()
        .expect("ambiguous copy mask");
    assert_eq!(mask.claims.changed_source_lower, 1);
    assert!(mask.old.is_empty() && mask.new.is_empty());
    assert_eq!(reviews[0].comparison.new.len(), 2);
    assert!(
        !result.scopes[0]
            .result
            .counterpart_decisions
            .unresolved
            .is_empty()
    );
}

#[test]
fn scope_reviews_reject_acquisition_issues_and_interleaved_boundaries() {
    let old = inventoried_fixture(&[
        "Left alpha.",
        "old",
        "Right alpha.",
        "Left beta.",
        "second",
        "Right beta.",
    ]);
    let crossed = inventoried_fixture(&[
        "Left alpha.",
        "new",
        "Left beta.",
        "Right alpha.",
        "second",
        "Right beta.",
    ]);
    let result = compare(&old, &crossed, DocumentComparisonLimits::default());
    assert!(result.scopes[0].result.text_scope_reviews.is_empty());
    let mut issue = inventoried_fixture(&[
        "Left alpha.",
        "new",
        "Right alpha.",
        "Left beta.",
        "second",
        "Right beta.",
    ]);
    issue.0.issues.push(pdfdelta_core::document::EvidenceIssue {
        page: None,
        channel: pdfdelta_core::document::Channel::Text,
        sources: Vec::new(),
        boundary: None,
        kind: pdfdelta_core::document::EvidenceFailure::Unresolved,
        reason: "unknown missing text".into(),
    });
    let result = compare(&old, &issue, DocumentComparisonLimits::default());
    assert!(result.scopes[0].result.text_scope_reviews.is_empty());
}

#[test]
fn split_and_merge_preserve_text_and_all_member_references() {
    let whole = fixture(&["α100"]);
    let mut parts = fixture(&["α", "100"]);
    parts.0.structured.reverse();
    parts.1 = graph(&parts.0);
    for (old, new) in [(&whole, &parts), (&parts, &whole)] {
        let result = compare(old, new, DocumentComparisonLimits::default());
        assert!(result.search_resolved());
        let pairs: Vec<_> = result.comparisons().collect();
        assert_eq!(pairs.len(), 1);
        let pair = pairs[0];
        assert_eq!(pair.old.len(), old.0.structured.len());
        assert_eq!(pair.new.len(), new.0.structured.len());
        assert!(pair.compared);
        assert!(pair.operation.is_none());
        assert_eq!(
            pair.interpretation,
            InterpretationStatus::ConditionalOnCorrespondence
        );
        assert_eq!(
            pair.text_mask
                .as_ref()
                .expect("exact text mask")
                .claims
                .changed_source_upper,
            0
        );
        let sources: std::collections::BTreeSet<_> = pair
            .new
            .iter()
            .flat_map(|id| {
                new.1
                    .nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .expect("retained group member")
                    .sources
                    .iter()
                    .copied()
            })
            .collect();
        assert_eq!(sources.len(), new.0.structured.len());
    }
}

#[test]
fn changed_split_and_merge_preserve_exact_sources_and_inferred_boundaries() {
    let whole = fixture(&["The annual fee is 100 dollars."]);
    let mut parts = fixture(&["The annual ", "fee is 200 dollars."]);
    parts.1.nodes.reverse();
    parts.1.edges.reverse();
    for (old, new) in [(&whole, &parts), (&parts, &whole)] {
        let result = compare(old, new, DocumentComparisonLimits::default());
        let pairs: Vec<_> = result.comparisons().collect();
        assert_eq!(pairs.len(), 1);
        let pair = pairs[0];
        assert_eq!(pair.old.len(), old.0.structured.len());
        assert_eq!(pair.new.len(), new.0.structured.len());
        assert_eq!(pair.interpretation, InterpretationStatus::Inferred);
        assert!(pair.compared);
        let mask = pair
            .text_mask
            .as_ref()
            .expect("local literal character mask");
        assert_eq!(mask.old.len(), 1);
        assert_eq!(mask.new.len(), 1);
        assert_eq!(mask.old[0].position, 18);
        assert_eq!(mask.new[0].position, 18);
        assert!(
            !result.scopes[0]
                .result
                .counterpart_decisions
                .unresolved
                .is_empty()
        );
        assert_eq!(result.keyed_element_operations().count(), 0);
    }
}

#[test]
fn nonexact_groups_require_order_and_do_not_invent_boundary_spaces() {
    let whole = fixture(&["The annual fee is 100 dollars."]);
    let mut parts = fixture(&["The annual", "fee is 200 dollars."]);
    let result = compare(&whole, &parts, DocumentComparisonLimits::default());
    let group = result.scopes[0]
        .result
        .candidates
        .proposals
        .iter()
        .find(|candidate| candidate.new.len() == 2)
        .expect("retained ordered group");
    let nodes: std::collections::BTreeMap<_, _> =
        parts.1.nodes.iter().map(|node| (node.id, node)).collect();
    let old_node = whole
        .1
        .nodes
        .iter()
        .find(|node| node.id == group.old[0])
        .expect("retained whole-text endpoint");
    let local = compare_text_group_views(
        &[old_node],
        &group.new.iter().map(|id| nodes[id]).collect::<Vec<_>>(),
        Default::default(),
    )
    .expect("literal boundary comparison");
    let mask = local.text_mask.expect("changed boundary mask");
    assert!(mask.old.iter().any(|token| token.position == 10));

    parts.1.edges.retain(|edge| edge.kind != EdgeKind::Precedes);
    let result = compare(&whole, &parts, DocumentComparisonLimits::default());
    assert!(
        result.scopes[0]
            .result
            .candidates
            .proposals
            .iter()
            .all(|candidate| { candidate.old.len() == 1 && candidate.new.len() == 1 })
    );
}

#[test]
fn nonexact_group_budget_preserves_independent_source_correspondences() {
    let old = fixture(&["fixed anchor", "The annual fee is 100 dollars."]);
    let new = fixture(&["fixed anchor", "The annual ", "fee is 200 dollars."]);
    let mut limits = DocumentComparisonLimits::default();
    let source = pdfdelta_core::document::propose_scope_correspondences(
        &old.1,
        &new.1,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits.matching,
    )
    .expect("complete source search");
    assert!(source.exhaustive);
    limits.matching.max_group_token_checks = source.group_token_checks;
    let result = compare(&old, &new, limits);
    assert!(!result.scopes[0].result.text_search.exhaustive);
    let pairs: Vec<_> = result.comparisons().collect();
    assert_eq!(pairs.len(), 1);
    assert!(pairs[0].compared && pairs[0].operation.is_none());
    assert_eq!(
        pairs[0].interpretation,
        InterpretationStatus::ConditionalOnCorrespondence
    );
    assert!(
        result.scopes[0]
            .result
            .candidates
            .proposals
            .iter()
            .all(|candidate| { candidate.supplier != "retained-order-trigram-group-v1" })
    );
}

#[test]
fn archived_partition_supports_split_and_merge_without_inventing_order() {
    use pdfdelta_core::document::{AlternativeViews, GraphEdge, NodeKind};
    let mut whole = fixture(&["α100"]);
    let original = whole
        .1
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Paragraph)
        .expect("original paragraph")
        .clone();
    let parent = NodeId(whole.1.nodes.len() as u64);
    let archive = NodeId(parent.0 + 1);
    let other = NodeId(parent.0 + 2);
    for (id, kind, sources) in [
        (parent, NodeKind::Table, original.sources.clone()),
        (archive, NodeKind::Unknown, Vec::new()),
    ] {
        let mut node = whole.1.nodes[0].clone();
        node.id = id;
        node.kind = kind;
        node.sources = sources;
        node.basis = ViewBasis::ReconstructedStructure;
        whole.1.nodes.push(node);
    }
    let mut alternate = original.clone();
    alternate.id = other;
    alternate.kind = NodeKind::Cell;
    alternate.basis = ViewBasis::ReconstructedStructure;
    whole.1.nodes.push(alternate);
    whole
        .1
        .edges
        .retain(|edge| !(edge.kind == EdgeKind::Contains && edge.to == original.id));
    for (from, to) in [
        (NodeId(0), parent),
        (parent, archive),
        (archive, original.id),
        (parent, other),
    ] {
        whole.1.edges.push(GraphEdge {
            from,
            to,
            kind: EdgeKind::Contains,
            sources: Vec::new(),
            basis: ViewBasis::ReconstructedStructure,
        });
    }
    whole.1.alternatives.push(AlternativeViews {
        parent,
        partitions: vec![vec![original.id], vec![other]],
    });
    let mut parts = fixture(&["α", "100"]);
    for (old, new) in [(&whole, &parts), (&parts, &whole)] {
        let result = compare(old, new, DocumentComparisonLimits::default());
        assert!(result.comparisons().any(|pair| pair.compared
            && pair.operation.is_none()
            && pair.interpretation == InterpretationStatus::Inferred
            && (pair.old.len() == 2 || pair.new.len() == 2)));
    }
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 0;
    let result = compare(&whole, &parts, limits);
    assert!(!result.search_resolved());
    assert!(
        result
            .comparisons()
            .all(|pair| pair.old.len() == 1 && pair.new.len() == 1)
    );
    parts.1.edges.retain(|edge| edge.kind != EdgeKind::Precedes);
    let result = compare(&whole, &parts, DocumentComparisonLimits::default());
    assert!(
        result
            .comparisons()
            .all(|pair| pair.old.len() == 1 && pair.new.len() == 1)
    );
}

#[test]
fn missing_order_and_missing_spaces_cannot_be_invented_by_grouping() {
    let whole = fixture(&["α100"]);
    let mut parts = fixture(&["α", "100"]);
    parts.1.edges.retain(|edge| edge.kind != EdgeKind::Precedes);
    // A nonexact one-to-one proposal may remain inferred; it cannot establish
    // a joined literal comparison without the missing source order.
    assert!(
        compare(&whole, &parts, DocumentComparisonLimits::default())
            .comparisons()
            .all(|pair| pair.interpretation == InterpretationStatus::Inferred
                && pair.old.len() == 1
                && pair.new.len() == 1)
    );
    let spaced = fixture(&["α 100"]);
    let parts = fixture(&["α", "100"]);
    assert!(
        compare(&spaced, &parts, DocumentComparisonLimits::default())
            .comparisons()
            .all(|pair| pair.interpretation == InterpretationStatus::Inferred
                && pair.old.len() == 1
                && pair.new.len() == 1)
    );
}

#[test]
fn group_truncation_does_not_turn_a_surviving_candidate_into_a_proof() {
    let whole = fixture(&["α100"]);
    let parts = fixture(&["α", "100"]);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 0;
    let result = compare(&whole, &parts, limits);
    assert!(!result.scopes[0].result.candidates.exhaustive);
    assert_eq!(result.comparisons().count(), 0);
    assert!(!result.search_resolved());
}

#[test]
fn source_aliases_cannot_be_consumed_twice_by_a_group() {
    let whole = fixture(&["aa"]);
    let mut parts = fixture(&["a", "a"]);
    for node in &mut parts.1.nodes {
        if let NodeContent::Text { view } = &mut node.content {
            node.sources = vec![SourceRef::Structured { element: 0 }];
            view.origins = vec![node.sources.clone()];
        }
    }
    let result = compare(&whole, &parts, DocumentComparisonLimits::default());
    assert_eq!(result.comparisons().count(), 0);
}

#[test]
fn model_order_remains_inferred_even_when_group_text_is_exact() {
    let whole = fixture(&["α100"]);
    let mut parts = fixture(&["α", "100"]);
    parts.0.backends.push(BackendIdentity {
        kind: BackendKind::StructureModel,
        name: "fixture-order".into(),
        version: "1".into(),
        profile: "order-candidate".into(),
        model: Some("fixture".into()),
    });
    for edge in &mut parts.1.edges {
        if edge.kind == EdgeKind::Precedes {
            edge.basis = ViewBasis::Model { backend: 1 };
        }
    }
    let result = compare(&whole, &parts, DocumentComparisonLimits::default());
    let pair = result
        .comparisons()
        .next()
        .expect("ordered group comparison");
    assert!(pair.compared);
    assert_eq!(pair.interpretation, InterpretationStatus::Inferred);
}

#[test]
fn grouped_local_change_masks_keep_the_original_fragment_origin() {
    let old = fixture(&["value100"]);
    let new = fixture(&["value", "200"]);
    let nodes = |graph: &DocumentGraph| {
        graph
            .nodes
            .iter()
            .filter(|node| matches!(node.content, NodeContent::Text { .. }))
            .map(|node| node.id)
            .collect::<Vec<_>>()
    };
    let old_ids = nodes(&old.1);
    let new_ids = nodes(&new.1);
    let old_nodes: Vec<_> = old_ids
        .iter()
        .map(|id| {
            old.1
                .nodes
                .iter()
                .find(|node| node.id == *id)
                .expect("old member")
        })
        .collect();
    let new_nodes: Vec<_> = new_ids
        .iter()
        .map(|id| {
            new.1
                .nodes
                .iter()
                .find(|node| node.id == *id)
                .expect("new member")
        })
        .collect();
    let result = compare_text_group_views(
        &old_nodes,
        &new_nodes,
        DocumentComparisonLimits::default().local,
    )
    .expect("local comparison under a declared group correspondence");
    assert!(matches!(
        result.operation,
        Some(TypedOperation::TextChanged { .. })
    ));
    assert_eq!(result.old, old_ids);
    assert_eq!(result.new, new_ids);
    let mask = result.text_mask.expect("grouped exact mask");
    assert_eq!(mask.new.len(), 1);
    assert_eq!(mask.new[0].position, 5);
    assert_eq!(
        mask.new[0].sources,
        vec![SourceRef::Structured { element: 1 }]
    );
}

#[test]
fn paragraph_grouping_cannot_erase_a_label_relationship() {
    use pdfdelta_core::document::GraphEdge;
    let mut old = fixture(&["ab", "footnote"]);
    let mut new = fixture(&["a", "b", "footnote"]);
    for (store, graph) in [&mut old, &mut new] {
        let text_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| matches!(node.content, NodeContent::Text { .. }))
            .map(|node| node.id)
            .collect();
        assert_eq!(text_nodes.len(), store.structured.len());
        graph.edges.push(GraphEdge {
            from: text_nodes[0],
            to: *text_nodes.last().expect("footnote"),
            kind: EdgeKind::LabelFor,
            sources: Vec::new(),
            basis: ViewBasis::SourceStructure,
        });
    }
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    assert!(
        result
            .comparisons()
            .all(|pair| pair.compared && pair.operation.is_none())
    );
    assert!(
        result
            .relations()
            .all(|relation| relation.kind != EdgeKind::LabelFor)
    );
    assert!(
        result
            .relation_unresolved
            .iter()
            .any(|reason| reason.contains("endpoints"))
    );
}

#[test]
fn inferred_text_candidates_do_not_overload_independent_exact_paragraphs() {
    let document = fixture(&[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ]);
    let result = compare(&document, &document, DocumentComparisonLimits::default());
    assert!(result.search_resolved());
    assert_eq!(result.comparisons().count(), 8);
    assert!(
        result
            .comparisons()
            .all(|pair| pair.compared && pair.operation.is_none())
    );
}

#[test]
fn nonidentical_paragraphs_reach_exact_masks_as_inferred_correspondences() {
    let old = fixture(&["The fee is 100 dollars."]);
    let new = fixture(&["The fee is 200 dollars."]);
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    let decisions = &result.scopes[0].result.counterpart_decisions.unresolved;
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].explanations,
        [
            pdfdelta_core::document::CounterpartExplanation::Correspondence,
            pdfdelta_core::document::CounterpartExplanation::SeparatePresence,
        ]
    );
    assert_eq!(result.keyed_element_operations().count(), 0);
    let pairs: Vec<_> = result.comparisons().collect();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].interpretation, InterpretationStatus::Inferred);
    assert!(matches!(
        pairs[0].operation,
        Some(TypedOperation::TextChanged { .. })
    ));
    let mask = pairs[0].text_mask.as_ref().expect("local exact mask");
    assert_eq!(mask.old.len(), 1);
    assert_eq!(mask.new.len(), 1);
    assert_eq!(mask.old[0].position, 11);
    assert_eq!(mask.new[0].position, 11);
}

#[test]
fn equal_text_similarity_rivals_remain_ambiguous() {
    let old = fixture(&["Version 100"]);
    let new = fixture(&["Version 200", "Version 200"]);
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    assert!(result.scopes[0].result.text_search.exhaustive);
    assert!(result.comparisons().next().is_none());
    assert!(!result.search_resolved());
}

#[test]
fn source_pruning_does_not_treat_overlap_as_transitive_equivalence() {
    use pdfdelta_core::document::{NodeKind, SourceConflict};
    let mut old = fixture(&["anchor", "unused view", "The fee is 100 dollars."]);
    let mut new = fixture(&["anchor", "unused view", "The fee is 200 dollars."]);
    for (_, graph) in [&mut old, &mut new] {
        let node = graph
            .nodes
            .iter_mut()
            .find(|node| node.sources == [SourceRef::Structured { element: 1 }])
            .expect("unused composite view");
        node.kind = NodeKind::Unknown;
        node.content = NodeContent::Unknown;
        for pair in [[0, 1], [1, 2]] {
            graph.source_conflicts.push(SourceConflict {
                sources: pair
                    .map(|element| SourceRef::Structured { element })
                    .to_vec(),
                reason: "two disjoint pieces overlap an unused composite".into(),
            });
        }
    }
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    assert_eq!(
        result
            .comparisons()
            .filter(|pair| pair.operation.is_some()
                && pair.interpretation == InterpretationStatus::Inferred)
            .count(),
        1
    );
}

#[test]
fn inferred_tie_breaks_keep_source_alignment_inferred() {
    use pdfdelta_core::document::{FieldValue, SourceConflict};
    let mut old = fixture(&["anchor", "anchox"]);
    let mut new = fixture(&["anchor", "anchor"]);
    for (store, target) in [&mut old, &mut new] {
        store.backends.push(BackendIdentity {
            kind: BackendKind::StructureModel,
            name: "fixture-model".into(),
            version: "1".into(),
            profile: "field".into(),
            model: Some("fixture".into()),
        });
        store.structured.push(StructuredEvidence {
            id: 9,
            page: None,
            bounds: None,
            object: None,
            backend: 1,
            value: StructuredValue::FormField {
                field_type: None,
                name: "bias".into(),
                value: FieldValue::Text("stable".into()),
                widgets: Vec::new(),
                button_states: Vec::new(),
            },
        });
        *target = graph(store);
    }
    new.1.source_conflicts.push(SourceConflict {
        sources: vec![
            SourceRef::Structured { element: 0 },
            SourceRef::Structured { element: 9 },
        ],
        reason: "model field competes with one literal reading".into(),
    });
    let result = compare(&old, &new, DocumentComparisonLimits::default());
    assert!(
        result.scopes[0]
            .result
            .text_search
            .protected_correspondences
            .is_empty()
    );
    assert!(result.comparisons().next().is_some());
    assert!(
        result
            .comparisons()
            .all(|pair| pair.interpretation
                == pdfdelta_core::document::InterpretationStatus::Inferred)
    );
}

#[test]
fn text_budget_exhaustion_retains_proved_anchors_and_independent_field_changes() {
    use pdfdelta_core::document::FieldValue;
    let mut old = fixture(&["anchor", "The fee is 100 dollars."]);
    let mut new = fixture(&["anchor", "The fee is 200 dollars."]);
    for ((store, target), text) in [(&mut old, "100"), (&mut new, "200")] {
        store.structured.push(StructuredEvidence {
            id: 9,
            page: None,
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::FormField {
                field_type: None,
                name: "independent".into(),
                value: FieldValue::Text(text.into()),
                widgets: Vec::new(),
                button_states: Vec::new(),
            },
        });
        *target = graph(store);
    }
    let mut limits = DocumentComparisonLimits::default();
    limits.text.max_token_visits = 0;
    let result = compare(&old, &new, limits);
    assert!(!result.scopes[0].result.text_search.exhaustive);
    assert_eq!(result.comparisons().count(), 2);
    assert_eq!(
        result
            .comparisons()
            .filter(|pair| pair.operation.is_some())
            .count(),
        1
    );
    assert!(result.comparisons().all(|pair| pair.compared
        && pair.interpretation == InterpretationStatus::ConditionalOnCorrespondence));
    assert!(!result.search_resolved());
}
