use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, CorrespondenceScope, DocumentComparisonLimits, DocumentGraph,
        EdgeKind, EvidenceStore, NodeId, SourceRef, StructuredEvidence, StructuredValue, ViewBasis,
    },
    model::Document,
    options::PipelineOptions,
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
                    declared_text: None,
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
