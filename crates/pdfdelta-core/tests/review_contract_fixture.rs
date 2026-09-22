//! Contract fixtures for agent review packets.
//!
//! These pin the properties a packet consumer depends on before any planner
//! runs: unexamined evidence can be enumerated rather than only counted, the
//! bundle identity is reproducible from inputs alone, and an external decision
//! is checked against the case it claims to answer.

use std::collections::BTreeSet;

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, EvidenceStore, HierarchyLimits,
        MatchingChannels, NodeId, SourceRef, StructuredEvidence, StructuredValue, UnresolvedReason,
        compare_document_views, document_coverage, document_source_accounting,
        unresolved_classification,
    },
    model::Document,
    pipeline::PipelineOptions,
    review::{
        BundleIdentity, CaseId, POLICY_VERSION, PipelineContract, REVIEW_SCHEMA, SourceAlias,
    },
};

/// Structured paragraphs with a document-wide text inventory covering them all.
///
/// A complete inventory makes every discovered reference an explicit obligation,
/// so anything the comparison does not reach must be enumerable afterwards.
fn fixture(parts: &[&str], inventory_complete: bool) -> (EvidenceStore, DocumentGraph) {
    let structured: Vec<_> = parts
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
                declared_text: None,
                identifier: None,
                glyphs: Vec::new(),
                role: "paragraph".into(),
                text: Some((*text).into()),
                parent: None,
                order: Some(index as u32),
            },
        })
        .collect();
    let store = EvidenceStore {
        revision: "review-contract-fixture".into(),
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
        inventories: vec![ChannelInventory {
            page: None,
            channel: Channel::Text,
            backend: 0,
            sources: structured
                .iter()
                .map(|element| SourceRef::Structured {
                    element: element.id,
                })
                .collect(),
            complete: inventory_complete,
        }],
        key_inventories: Vec::new(),
        native_structures: Vec::new(),
        issues: Vec::new(),
        structured,
    };
    let limits = DocumentComparisonLimits::default();
    let graph = DocumentGraph::from_evidence(
        &store,
        PipelineOptions::default(),
        limits.evidence,
        limits.graph,
    )
    .expect("derive ordered text views");
    (store, graph)
}

#[test]
fn unexamined_evidence_is_enumerable_and_agrees_with_reported_counts() {
    let old = fixture(&["A stable paragraph.", "An old only paragraph."], true);
    let new = fixture(&["A stable paragraph."], true);
    let old_view = DocumentView {
        evidence: &old.0,
        graph: &old.1,
    };
    let new_view = DocumentView {
        evidence: &new.0,
        graph: &new.1,
    };
    let channels = BTreeSet::from([Channel::Text]);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.channels = MatchingChannels::from(&channels);
    let comparison = compare_document_views(
        old_view,
        new_view,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("compare structured paragraphs");

    let coverage = document_coverage(old_view, new_view, &comparison, &channels);
    let accounting = document_source_accounting(old_view, new_view, &comparison, &channels);
    assert_eq!(coverage.len(), accounting.len());

    for (coverage, accounting) in coverage.iter().zip(&accounting) {
        assert_eq!(coverage.channel, accounting.channel);
        assert_eq!(coverage.complete, accounting.complete());
        assert_eq!(
            coverage.old_uncompared_sources,
            accounting.old.uncompared().count(),
            "reported old obligations must be enumerable"
        );
        assert_eq!(
            coverage.new_uncompared_sources,
            accounting.new.uncompared().count(),
            "reported new obligations must be enumerable"
        );
        assert_eq!(
            coverage.old_discovered_sources,
            accounting.old.discovered.len()
        );
        // Enumerated obligations are a subset of what discovery found; nothing
        // is invented for a reviewer that the engine never saw.
        assert!(
            accounting
                .old
                .uncompared()
                .all(|source| accounting.old.discovered.contains(&source))
        );
    }

    let text = accounting
        .iter()
        .find(|channel| channel.channel == Channel::Text)
        .expect("the selected channel is accounted for");
    assert!(
        text.old.uncompared().count() > 0,
        "the deleted paragraph leaves an unexamined obligation"
    );
}

#[test]
fn an_unclosed_inventory_stays_incomplete_even_with_no_unexamined_reference() {
    let old = fixture(&["A stable paragraph."], false);
    let new = fixture(&["A stable paragraph."], false);
    let old_view = DocumentView {
        evidence: &old.0,
        graph: &old.1,
    };
    let new_view = DocumentView {
        evidence: &new.0,
        graph: &new.1,
    };
    let channels = BTreeSet::from([Channel::Text]);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.channels = MatchingChannels::from(&channels);
    let comparison = compare_document_views(
        old_view,
        new_view,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("compare identical paragraphs");

    let accounting = document_source_accounting(old_view, new_view, &comparison, &channels);
    let text = accounting
        .iter()
        .find(|channel| channel.channel == Channel::Text)
        .expect("the selected channel is accounted for");
    assert_eq!(text.old.uncompared().count(), 0);
    assert!(!text.old.inventory_complete);
    assert!(
        !text.complete(),
        "an open inventory is not discharged by comparing what was discovered"
    );
}

#[test]
fn bundle_identity_binds_inputs_options_and_snapshot_but_not_time() {
    let identity = |options: &str| BundleIdentity {
        policy_version: POLICY_VERSION,
        schema: REVIEW_SCHEMA.into(),
        old_sha256: "ab".repeat(32),
        new_sha256: "cd".repeat(32),
        old_bytes: 1024,
        new_bytes: 2048,
        options: options.into(),
        old_revision: "review-contract-fixture".into(),
        new_revision: "review-contract-fixture".into(),
        backends: vec!["fixture/1/source-structure".into()],
        pipeline: PipelineContract::SharedEvidence,
        selected_channels: BTreeSet::from([Channel::Text]),
    };

    assert_eq!(
        identity("channels=text").bundle_id(),
        identity("channels=text").bundle_id(),
        "the same inputs and options reproduce the same bundle identity"
    );
    assert_ne!(
        identity("channels=text").bundle_id(),
        identity("channels=text,visual").bundle_id(),
        "a different evidence selection is a different bundle"
    );
}

#[test]
fn identifiers_and_aliases_reject_unsafe_text() {
    assert!(CaseId::new("R17").is_ok());
    assert!(SourceAlias::new("E31").is_ok());
    for hostile in [
        "R17 && rm -rf /",
        "../../etc/passwd",
        "R\u{202e}17",
        "R\u{0}17",
    ] {
        assert!(
            CaseId::new(hostile).is_err(),
            "identifier {hostile:?} must be rejected rather than escaped downstream"
        );
    }
}

#[test]
fn retained_obligations_are_classified_where_they_are_emitted() {
    let old = fixture(&["First paragraph.", "Second paragraph.", "Third."], true);
    let new = fixture(
        &["First paragraph.", "Second paragraph changed.", "Third."],
        true,
    );
    let old_view = DocumentView {
        evidence: &old.0,
        graph: &old.1,
    };
    let new_view = DocumentView {
        evidence: &new.0,
        graph: &new.1,
    };
    let channels = BTreeSet::from([Channel::Text]);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.channels = MatchingChannels::from(&channels);
    // A candidate budget too small to close enumeration forces the scope to
    // retain an obligation instead of completing.
    limits.matching.max_proposals = 1;
    let comparison = compare_document_views(
        old_view,
        new_view,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("compare under a candidate budget");

    let scope = &comparison.scopes[0].result;
    assert!(
        !scope.unresolved.is_empty(),
        "a truncated enumeration must retain an obligation"
    );
    assert_eq!(
        scope.unresolved.len(),
        scope.obligations.len(),
        "each retained message carries its classification at the same index"
    );
    for index in 0..scope.unresolved.len() {
        assert_ne!(
            unresolved_classification(&scope.obligations, index).reason,
            UnresolvedReason::Other,
            "message {:?} was emitted without a classification",
            scope.unresolved[index]
        );
    }
}
