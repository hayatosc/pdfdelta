use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, EvidenceLimits, EvidenceStore, FieldValue, GraphNode,
        InterpretationStatus, LocalComparisonLimits, NodeContent, NodeId, NodeKind, PageEvidence,
        PixelRun, Raster, RenderedEvidence, SourceRef, StructuredEvidence, StructuredValue,
        TypedOperation, ViewBasis, compare_local_views,
    },
    model::{Document, PageId, Rect, Vec2},
};

mod hierarchy {
    use super::*;
    use pdfdelta_core::document::{
        CorrespondenceScope, DocumentComparisonLimits, DocumentGraph, DocumentView,
        DocumentViewComparison, EdgeKind, GraphEdge, HierarchyLimits, IdentityKey,
        compare_document_views,
    };

    fn fixture(values: [&str; 2], reverse: bool) -> (EvidenceStore, DocumentGraph) {
        let (mut evidence, mut first) = value(values[0]);
        let (second_store, mut second) = value(values[1]);
        let mut second_source = second_store.structured[0].clone();
        second_source.id = 2;
        evidence.structured.push(second_source);
        second.id = NodeId(2);
        second.sources = vec![SourceRef::Structured { element: 2 }];
        for field in [&mut first, &mut second] {
            field.identity = Some(IdentityKey {
                namespace: "field".into(),
                value: "value".into(),
            });
        }
        let container = |id, kind, name: &str| GraphNode {
            id: NodeId(id),
            kind,
            pages: Vec::new(),
            sources: Vec::new(),
            identity: Some(IdentityKey {
                namespace: "section".into(),
                value: name.into(),
            }),
            basis: ViewBasis::SourceStructure,
            content: NodeContent::Container,
        };
        let mut graph = DocumentGraph {
            nodes: vec![
                container(0, NodeKind::Document, "document"),
                container(10, NodeKind::Section, "annual"),
                container(20, NodeKind::Section, "quarterly"),
                container(11, NodeKind::Form, "amounts"),
                container(21, NodeKind::Form, "amounts"),
                first,
                second,
            ],
            edges: [(0, 10), (0, 20), (10, 11), (20, 21), (11, 1), (21, 2)]
                .into_iter()
                .map(|(from, to)| GraphEdge {
                    from: NodeId(from),
                    to: NodeId(to),
                    kind: EdgeKind::Contains,
                    sources: Vec::new(),
                    basis: ViewBasis::SourceStructure,
                })
                .collect(),
            ..DocumentGraph::default()
        };
        if reverse {
            graph.nodes.reverse();
            graph.edges.reverse();
        }
        (evidence, graph)
    }

    fn compare(
        old: &(EvidenceStore, DocumentGraph),
        new: &(EvidenceStore, DocumentGraph),
        hierarchy: HierarchyLimits,
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
            DocumentComparisonLimits::default(),
            hierarchy,
        )
        .expect("compare nested source-backed fields")
    }

    #[test]
    fn scoped_names_survive_reordering_and_values_swapped_between_sections() {
        let old = fixture(["100", "20"], false);
        let new = fixture(["20", "100"], true);
        let result = compare(&old, &new, HierarchyLimits::default());
        assert_eq!(result.scopes.len(), 5);
        assert!(result.search_resolved());
        let pairs: Vec<_> = result.comparisons().collect();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().all(|pair| pair.old == pair.new));
        assert!(
            pairs
                .iter()
                .all(|pair| matches!(pair.operation, Some(TypedOperation::ValueChanged { .. })))
        );
        for child in result.scopes.iter().skip(1) {
            let premise = child.parent.expect("retained parent correspondence");
            let proposal =
                &result.scopes[premise.scope].result.candidates.proposals[premise.proposal];
            assert_eq!(proposal.old, vec![child.result.matching.scope.old]);
            assert_eq!(proposal.new, vec![child.result.matching.scope.new]);
        }
    }

    #[test]
    fn traversal_limits_retain_pending_children_and_completed_siblings() {
        let old = fixture(["100", "20"], false);
        let new = fixture(["20", "100"], false);
        let shallow = compare(
            &old,
            &new,
            HierarchyLimits {
                max_depth: 1,
                ..HierarchyLimits::default()
            },
        );
        assert_eq!(shallow.scopes.len(), 3);
        assert_eq!(shallow.comparisons().count(), 0);
        assert!(!shallow.search_resolved());
        let bounded = compare(
            &old,
            &new,
            HierarchyLimits {
                max_scopes: 4,
                ..HierarchyLimits::default()
            },
        );
        assert_eq!(bounded.scopes.len(), 4);
        assert_eq!(bounded.comparisons().count(), 1);
        assert!(!bounded.search_resolved());
    }

    #[test]
    fn parent_model_inference_cannot_be_laundered_by_exact_child_values() {
        let old = fixture(["100", "20"], false);
        let mut new = fixture(["20", "100"], false);
        new.0.backends.push(BackendIdentity {
            kind: BackendKind::StructureModel,
            name: "fixture-layout".into(),
            version: "1".into(),
            profile: "fixture".into(),
            model: Some("fixture-model".into()),
        });
        new.1
            .nodes
            .iter_mut()
            .find(|node| node.id == NodeId(10))
            .expect("annual section")
            .basis = ViewBasis::Model { backend: 1 };
        let result = compare(&old, &new, HierarchyLimits::default());
        let annual = result
            .comparisons()
            .find(|pair| pair.old == [NodeId(1)])
            .expect("annual comparison");
        let quarterly = result
            .comparisons()
            .find(|pair| pair.old == [NodeId(2)])
            .expect("quarterly comparison");
        assert_eq!(annual.interpretation, InterpretationStatus::Inferred);
        assert_eq!(
            quarterly.interpretation,
            InterpretationStatus::ConditionalOnCorrespondence
        );
    }

    #[test]
    fn shared_descendant_evidence_competes_before_entering_child_scopes() {
        let mut old = fixture(["100", "100"], false);
        let mut new = fixture(["20", "20"], false);
        for (_, graph) in [&mut old, &mut new] {
            graph
                .nodes
                .iter_mut()
                .find(|node| node.id == NodeId(2))
                .expect("second field")
                .sources = vec![SourceRef::Structured { element: 1 }];
        }
        let result = compare(&old, &new, HierarchyLimits::default());
        assert_eq!(result.scopes.len(), 1);
        assert_eq!(result.comparisons().count(), 0);
        assert!(!result.search_resolved());
        assert!(
            result.scopes[0]
                .result
                .matching
                .components
                .iter()
                .all(|component| component.mandatory.is_empty())
        );
    }
}

fn store() -> EvidenceStore {
    EvidenceStore {
        revision: "fixture".into(),
        native: Document::new(Vec::new()),
        backends: vec![BackendIdentity {
            kind: BackendKind::Renderer,
            name: "fixture".into(),
            version: "1".into(),
            profile: "rgb-grid-v1".into(),
            model: None,
        }],
        pages: vec![PageEvidence {
            page: PageId(0),
            bounds: Some(Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 { x: 3.0, y: 2.0 },
            }),
        }],
        rendered: Vec::new(),
        structured: Vec::new(),
        inventories: Vec::new(),
        key_inventories: Vec::new(),
        issues: Vec::new(),
    }
}

fn value(text: &str) -> (EvidenceStore, GraphNode) {
    let mut store = store();
    store.backends[0].kind = BackendKind::NativeParser;
    let value = FieldValue::Text(text.into());
    store.structured.push(StructuredEvidence {
        id: 1,
        page: None,
        bounds: None,
        object: None,
        backend: 0,
        value: StructuredValue::FormField {
            field_type: None,
            name: "value".into(),
            value: value.clone(),
            widgets: Vec::new(),
            button_states: Vec::new(),
        },
    });
    store
        .validate(EvidenceLimits::default())
        .expect("valid stored field evidence");
    let node = GraphNode {
        id: NodeId(1),
        kind: NodeKind::Field,
        pages: Vec::new(),
        sources: vec![SourceRef::Structured { element: 1 }],
        identity: None,
        basis: ViewBasis::SourceStructure,
        content: NodeContent::Value { value },
    };
    (store, node)
}

#[test]
fn value_unit_does_not_invent_positions_in_repeated_text() {
    let (old_store, old) = value("AAA");
    let (new_store, new) = value("AA");
    let result = compare_local_views(
        &old,
        &new,
        &old_store,
        &new_store,
        LocalComparisonLimits::default(),
    )
    .expect("compare stored field values");
    assert!(result.compared);
    assert_eq!(
        result.operation,
        Some(TypedOperation::ValueChanged {
            old: FieldValue::Text("AAA".into()),
            new: FieldValue::Text("AA".into()),
        })
    );
    let mask = result.text_mask.expect("completed character proof");
    assert_eq!(mask.claims.changed_source_lower, 1);
    assert_eq!(mask.claims.changed_source_upper, 1);
    assert!(mask.old.is_empty());
    assert!(mask.new.is_empty());
    assert_eq!(
        result.interpretation,
        InterpretationStatus::ConditionalOnCorrespondence
    );
}

#[test]
fn unfinished_character_proof_does_not_erase_a_stored_value_change() {
    let (old_store, old) = value("24 Jul 2023");
    let (new_store, new) = value("2 Aug 2023");
    let result = compare_local_views(
        &old,
        &new,
        &old_store,
        &new_store,
        LocalComparisonLimits {
            proof_work: 0,
            ..LocalComparisonLimits::default()
        },
    )
    .expect("typed field comparison");
    assert!(result.compared);
    assert!(matches!(
        result.operation,
        Some(TypedOperation::ValueChanged { .. })
    ));
    assert!(result.text_mask.is_none());
    assert!(!result.unresolved.is_empty());
}

fn image(composited_page: bool, changed: bool) -> (EvidenceStore, GraphNode) {
    let mut store = store();
    let mut rgb = vec![255; 18];
    if changed {
        rgb[15] = 0;
    }
    store.rendered.push(RenderedEvidence {
        id: 1,
        page: PageId(0),
        backend: 0,
        composited_page,
        polygon: vec![
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 3.0, y: 0.0 },
            Vec2 { x: 3.0, y: 2.0 },
            Vec2 { x: 0.0, y: 2.0 },
        ],
        raster: Raster {
            width: 3,
            height: 2,
            rgb,
        },
    });
    store
        .validate(EvidenceLimits::default())
        .expect("valid raster evidence");
    let node = GraphNode {
        id: NodeId(1),
        kind: NodeKind::Figure,
        pages: vec![PageId(0)],
        sources: vec![SourceRef::Rendered { region: 1 }],
        identity: None,
        basis: ViewBasis::RenderedRegion,
        content: NodeContent::Visual { region: 1 },
    };
    (store, node)
}

mod visual_supplier {
    use super::*;
    use pdfdelta_core::{
        document::{
            CorrespondenceScope, DocumentComparisonLimits, DocumentGraph, DocumentView,
            HierarchyLimits, SourceConflict, VisualCandidateLimits, compare_document_views,
        },
        pipeline::PipelineOptions,
    };

    fn graph(store: &EvidenceStore) -> DocumentGraph {
        let limits = DocumentComparisonLimits::default();
        DocumentGraph::from_evidence(
            store,
            PipelineOptions::default(),
            limits.evidence,
            limits.graph,
        )
        .expect("derive visual and field graph")
    }

    #[test]
    fn visual_candidates_follow_content_across_reordered_pages() {
        let (mut old, _) = image(true, false);
        let mut second = old.rendered[0].clone();
        second.id = 2;
        second.page = PageId(1);
        second.raster.rgb.fill(0);
        old.rendered.push(second);
        let mut page = old.pages[0].clone();
        page.page = PageId(1);
        old.pages.push(page);
        let mut new = old.clone();
        new.rendered[0].raster.rgb.fill(0);
        new.rendered[1].raster.rgb.fill(255);
        new.rendered[1].raster.rgb[15] = 0;
        let old_graph = graph(&old);
        let new_graph = graph(&new);
        let result = compare_document_views(
            DocumentView {
                evidence: &old,
                graph: &old_graph,
            },
            DocumentView {
                evidence: &new,
                graph: &new_graph,
            },
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            DocumentComparisonLimits::default(),
            HierarchyLimits::default(),
        )
        .expect("compare same-grid image candidates");
        assert_eq!(result.scopes[0].result.visual_search.compared_pixels, 24);
        assert!(result.scopes[0].result.visual_search.exhaustive);
        let pairs: Vec<_> = result.comparisons().collect();
        assert_eq!(pairs.len(), 2);
        assert!(
            pairs.iter().all(|pair| pair.old != pair.new
                && pair.interpretation == InterpretationStatus::Inferred)
        );
        let change = pairs
            .iter()
            .find(|pair| pair.operation.is_some())
            .expect("one changed page rendering");
        assert_eq!(
            old_graph
                .nodes
                .iter()
                .find(|node| change.old == [node.id])
                .expect("old visual node")
                .content,
            NodeContent::Visual { region: 1 }
        );
        assert_eq!(
            new_graph
                .nodes
                .iter()
                .find(|node| change.new == [node.id])
                .expect("new visual node")
                .content,
            NodeContent::Visual { region: 2 }
        );
        assert_eq!(change.operation, Some(TypedOperation::PageRenderingChanged));
        assert_eq!(
            change
                .pixel_mask
                .as_ref()
                .expect("exact pixel mask")
                .changed_pixels,
            1
        );
        assert!(change.text_mask.is_none());
    }

    fn mixed(text: &str, changed: bool) -> EvidenceStore {
        let (mut store, _) = value(text);
        let (mut raster_store, _) = image(false, changed);
        store.backends.push(raster_store.backends.remove(0));
        raster_store.rendered[0].backend = 1;
        store.rendered = raster_store.rendered;
        store
    }

    #[test]
    fn channel_selection_excludes_unselected_candidates_before_budgeted_search() {
        use pdfdelta_core::document::{Channel, MatchingChannels};
        let with_background = |value, changed| {
            let mut store = mixed(value, changed);
            for id in 2..102 {
                store.structured.push(StructuredEvidence {
                    id,
                    page: None,
                    bounds: None,
                    object: None,
                    backend: 0,
                    value: StructuredValue::StructureElement {
                        content: None,
                        identifier: None,
                        glyphs: Vec::new(),
                        role: "paragraph".into(),
                        text: Some("unselected background".into()),
                        parent: None,
                        order: None,
                    },
                });
            }
            store
        };
        let old = with_background("100", false);
        let new = with_background("20", true);
        let old_graph = graph(&old);
        let new_graph = graph(&new);
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_pair_checks = 4;
        let compare = |limits| {
            compare_document_views(
                DocumentView {
                    evidence: &old,
                    graph: &old_graph,
                },
                DocumentView {
                    evidence: &new,
                    graph: &new_graph,
                },
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                limits,
                HierarchyLimits::default(),
            )
            .expect("bounded selected-channel comparison")
        };
        let all = compare(limits);
        assert!(!all.scopes[0].result.candidates.exhaustive);
        assert_eq!(all.comparisons().count(), 1);
        assert!(matches!(
            all.comparisons()
                .next()
                .expect("independent field")
                .operation,
            Some(TypedOperation::ValueChanged { .. })
        ));
        for channel in [Channel::Forms, Channel::Visual] {
            let channels: std::collections::BTreeSet<_> = [channel].into();
            limits.matching.channels = MatchingChannels::from(&channels);
            let selected = compare(limits);
            assert!(selected.scopes[0].result.candidates.exhaustive);
            assert!(selected.scopes[0].result.candidates.examined_pairs <= 4);
            assert_eq!(
                selected.scopes[0].result.matching.channels,
                limits.matching.channels
            );
            let pairs: Vec<_> = selected.comparisons().collect();
            assert_eq!(pairs.len(), 1);
            assert!(match channel {
                Channel::Forms => matches!(
                    pairs[0].operation,
                    Some(TypedOperation::ValueChanged { .. })
                ),
                Channel::Visual => matches!(
                    pairs[0].operation,
                    Some(TypedOperation::RenderedRegionChanged)
                ),
                _ => unreachable!(),
            });
        }
        let channels: std::collections::BTreeSet<_> = [Channel::Relations].into();
        limits.matching.channels = MatchingChannels::from(&channels);
        assert!(!compare(limits).scopes[0].result.candidates.exhaustive);
    }

    #[test]
    fn visual_score_cannot_displace_a_source_backed_field_identity() {
        let old = mixed("100", false);
        let new = mixed("20", true);
        let mut old_graph = graph(&old);
        old_graph.source_conflicts.push(SourceConflict {
            sources: vec![
                SourceRef::Rendered { region: 1 },
                SourceRef::Structured { element: 1 },
            ],
            reason: "visual and structured views refer to one physical field".into(),
        });
        let new_graph = graph(&new);
        let result = compare_document_views(
            DocumentView {
                evidence: &old,
                graph: &old_graph,
            },
            DocumentView {
                evidence: &new,
                graph: &new_graph,
            },
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            DocumentComparisonLimits::default(),
            HierarchyLimits::default(),
        )
        .expect("arbitrate visual similarity and field identity");
        assert_eq!(result.scopes[0].result.candidates.proposals.len(), 2);
        let pairs: Vec<_> = result.comparisons().collect();
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].interpretation,
            InterpretationStatus::ConditionalOnCorrespondence
        );
        assert!(matches!(
            pairs[0].operation,
            Some(TypedOperation::ValueChanged { .. })
        ));
    }

    #[test]
    fn truncated_visual_search_preserves_only_independent_field_comparisons() {
        for shares_physical_evidence in [false, true] {
            let old = mixed("100", false);
            let new = mixed("20", true);
            let mut old_graph = graph(&old);
            let mut new_graph = graph(&new);
            if shares_physical_evidence {
                for graph in [&mut old_graph, &mut new_graph] {
                    graph.source_conflicts.push(SourceConflict {
                        sources: vec![
                            SourceRef::Rendered { region: 1 },
                            SourceRef::Structured { element: 1 },
                        ],
                        reason: "fixture alternatives refer to the same physical material".into(),
                    });
                }
            }
            let result = compare_document_views(
                DocumentView {
                    evidence: &old,
                    graph: &old_graph,
                },
                DocumentView {
                    evidence: &new,
                    graph: &new_graph,
                },
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                DocumentComparisonLimits {
                    visual: VisualCandidateLimits {
                        max_pixel_comparisons: 0,
                    },
                    ..DocumentComparisonLimits::default()
                },
                HierarchyLimits::default(),
            )
            .expect("partial visual search");
            assert!(!result.scopes[0].result.visual_search.exhaustive);
            assert!(!result.scopes[0].result.candidates.exhaustive);
            assert!(!result.search_resolved());
            assert_eq!(
                result.comparisons().count(),
                usize::from(!shares_physical_evidence)
            );
            assert!(result.comparisons().all(
                |pair| pair.interpretation == InterpretationStatus::ConditionalOnCorrespondence
            ));
        }
    }

    #[test]
    fn incomplete_conflict_checks_preserve_only_independent_field_comparisons() {
        let fields = |text| {
            let (mut store, _) = value(text);
            let template = store.structured[0].clone();
            store.structured = (1..=5)
                .map(|id| {
                    let mut field = template.clone();
                    field.id = id;
                    let StructuredValue::FormField { name, .. } = &mut field.value else {
                        unreachable!()
                    };
                    *name = format!("field-{id}");
                    field
                })
                .collect();
            store
        };
        let old = fields("10");
        let new = fields("20");
        for shares_physical_evidence in [false, true] {
            let mut old_graph = graph(&old);
            let new_graph = graph(&new);
            old_graph.source_conflicts.push(SourceConflict {
                sources: (1..=if shares_physical_evidence { 5 } else { 4 })
                    .map(|element| SourceRef::Structured { element })
                    .collect(),
                reason: "alternative readings of one physical field".into(),
            });
            let mut limits = DocumentComparisonLimits::default();
            limits.matching.max_pair_checks = 5;
            let result = compare_document_views(
                DocumentView {
                    evidence: &old,
                    graph: &old_graph,
                },
                DocumentView {
                    evidence: &new,
                    graph: &new_graph,
                },
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                limits,
                HierarchyLimits::default(),
            )
            .expect("localize incomplete conflict checks");
            assert!(!result.scopes[0].result.matching.conflict_search_complete);
            assert!(!result.search_resolved());
            assert_eq!(
                result.comparisons().count(),
                usize::from(!shares_physical_evidence)
            );
            for pair in result.comparisons() {
                assert!(pair.compared);
                assert_eq!(
                    pair.interpretation,
                    InterpretationStatus::ConditionalOnCorrespondence
                );
                assert!(matches!(
                    pair.operation,
                    Some(TypedOperation::ValueChanged { .. })
                ));
            }
        }
    }

    #[test]
    fn duplicate_visuals_remain_ambiguous_and_profile_mismatch_is_unmatched() {
        let (old, _) = image(false, false);
        let mut new = old.clone();
        let mut duplicate = new.rendered[0].clone();
        duplicate.id = 2;
        new.rendered.push(duplicate);
        let compare = |new: &EvidenceStore| {
            compare_document_views(
                DocumentView {
                    evidence: &old,
                    graph: &graph(&old),
                },
                DocumentView {
                    evidence: new,
                    graph: &graph(new),
                },
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                DocumentComparisonLimits::default(),
                HierarchyLimits::default(),
            )
            .expect("compare visual ambiguity")
        };
        let tied = compare(&new);
        assert_eq!(tied.comparisons().count(), 0);
        assert!(!tied.search_resolved());
        new.backends[0].profile = "different-grid-profile".into();
        let incompatible = compare(&new);
        assert_eq!(incompatible.comparisons().count(), 0);
        assert_eq!(
            incompatible.scopes[0].result.visual_search.compared_pixels,
            0
        );
        assert!(
            incompatible.scopes[0]
                .result
                .candidates
                .proposals
                .is_empty()
        );
    }
}

#[test]
fn exact_pixel_mask_distinguishes_figure_and_whole_page_rendering() {
    for composited in [false, true] {
        let (old_store, old) = image(composited, false);
        let (new_store, new) = image(composited, true);
        let result = compare_local_views(
            &old,
            &new,
            &old_store,
            &new_store,
            LocalComparisonLimits::default(),
        )
        .expect("same-profile raster comparison");
        assert!(result.compared);
        assert_eq!(
            result.operation,
            Some(if composited {
                TypedOperation::PageRenderingChanged
            } else {
                TypedOperation::RenderedRegionChanged
            })
        );
        let mask = result.pixel_mask.expect("pixel mask");
        assert_eq!(mask.changed_pixels, 1);
        assert_eq!(
            mask.runs,
            vec![PixelRun {
                row: 1,
                start: 2,
                end: 3
            }]
        );
    }
}

#[test]
fn render_limits_and_profile_mismatch_are_unresolved_not_equal() {
    let (old_store, old) = image(false, false);
    let (mut new_store, new) = image(false, true);
    let limited = compare_local_views(
        &old,
        &new,
        &old_store,
        &new_store,
        LocalComparisonLimits {
            max_pixel_runs: 0,
            ..LocalComparisonLimits::default()
        },
    )
    .expect("bounded raster comparison");
    assert!(!limited.compared);
    assert!(limited.operation.is_none());
    assert!(limited.pixel_mask.is_none());
    new_store.backends[0].profile = "different-color-profile".into();
    let mismatch = compare_local_views(
        &old,
        &new,
        &old_store,
        &new_store,
        LocalComparisonLimits::default(),
    )
    .expect("incompatible raster comparison");
    assert!(!mismatch.compared);
    assert!(!mismatch.unresolved.is_empty());
}

#[test]
fn scope_pipeline_requires_complete_candidate_enumeration_before_local_results() {
    use pdfdelta_core::document::{
        CorrespondenceScope, DocumentComparisonLimits, DocumentGraph, DocumentView, EdgeKind,
        GraphEdge, IdentityKey, MatchingLimits, compare_scope_views,
    };
    fn field_graph(mut field: GraphNode) -> DocumentGraph {
        field.identity = Some(IdentityKey {
            namespace: "field".into(),
            value: "value".into(),
        });
        DocumentGraph {
            nodes: vec![
                GraphNode {
                    id: NodeId(0),
                    kind: NodeKind::Document,
                    pages: Vec::new(),
                    sources: Vec::new(),
                    identity: None,
                    basis: ViewBasis::SourceStructure,
                    content: NodeContent::Container,
                },
                field,
            ],
            edges: vec![GraphEdge {
                from: NodeId(0),
                to: NodeId(1),
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::SourceStructure,
            }],
            ..DocumentGraph::default()
        }
    }
    let (old_store, old_node) = value("AAA");
    let (mut new_store, new_node) = value("AA");
    let old_graph = field_graph(old_node);
    let mut new_graph = field_graph(new_node);
    let scope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };
    let compare = |new_store: &EvidenceStore, new_graph: &DocumentGraph, limits| {
        compare_scope_views(
            DocumentView {
                evidence: &old_store,
                graph: &old_graph,
            },
            DocumentView {
                evidence: new_store,
                graph: new_graph,
            },
            scope,
            limits,
        )
        .expect("validated scope pipeline")
    };
    let complete = compare(&new_store, &new_graph, DocumentComparisonLimits::default());
    assert_eq!(complete.comparisons.len(), 1);
    assert!(matches!(
        complete.comparisons[0].operation,
        Some(TypedOperation::ValueChanged { .. })
    ));
    let mut duplicate_evidence = new_store.structured[0].clone();
    duplicate_evidence.id = 2;
    new_store.structured.push(duplicate_evidence);
    let mut duplicate_view = new_graph.nodes[1].clone();
    duplicate_view.id = NodeId(2);
    duplicate_view.sources = vec![SourceRef::Structured { element: 2 }];
    new_graph.nodes.push(duplicate_view);
    let mut duplicate_edge = new_graph.edges[0].clone();
    duplicate_edge.to = NodeId(2);
    new_graph.edges.push(duplicate_edge);
    let limited = compare(
        &new_store,
        &new_graph,
        DocumentComparisonLimits {
            matching: MatchingLimits {
                max_proposals: 1,
                ..MatchingLimits::default()
            },
            ..DocumentComparisonLimits::default()
        },
    );
    assert!(!limited.candidates.exhaustive);
    assert!(limited.comparisons.is_empty());
    assert!(!limited.unresolved.is_empty());

    let mut independent_old_store = old_store.clone();
    let mut independent_new_store = new_store.clone();
    let mut independent_old_graph = old_graph.clone();
    let mut independent_new_graph = new_graph.clone();
    for (store, graph) in [
        (&mut independent_old_store, &mut independent_old_graph),
        (&mut independent_new_store, &mut independent_new_graph),
    ] {
        let mut evidence = store.structured[0].clone();
        evidence.id = 3;
        store.structured.push(evidence);
        let mut node = graph.nodes[1].clone();
        node.id = NodeId(3);
        node.sources = vec![SourceRef::Structured { element: 3 }];
        node.identity.as_mut().expect("field identity").value = "independent".into();
        graph.nodes.push(node);
        let mut edge = graph.edges[0].clone();
        edge.to = NodeId(3);
        graph.edges.push(edge);
    }
    let compare_partial = |old_graph: &DocumentGraph| {
        compare_scope_views(
            DocumentView {
                evidence: &independent_old_store,
                graph: old_graph,
            },
            DocumentView {
                evidence: &independent_new_store,
                graph: &independent_new_graph,
            },
            scope,
            DocumentComparisonLimits {
                matching: MatchingLimits {
                    max_proposals: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("partial candidate search")
    };
    let partial = compare_partial(&independent_old_graph);
    assert!(!partial.candidates.exhaustive);
    assert_eq!(partial.comparisons.len(), 1);
    assert_eq!(partial.comparisons[0].old, vec![NodeId(3)]);
    assert!(matches!(
        partial.comparisons[0].operation,
        Some(TypedOperation::ValueChanged { .. })
    ));
    assert!(
        partial
            .candidates
            .incomplete_nodes
            .as_ref()
            .expect("omitted endpoints")
            .new
            .contains(&NodeId(2))
    );

    independent_old_graph
        .source_conflicts
        .push(pdfdelta_core::document::SourceConflict {
            sources: vec![
                SourceRef::Structured { element: 1 },
                SourceRef::Structured { element: 3 },
            ],
            reason: "two alternative readings of shared physical evidence".into(),
        });
    let dependent = compare_partial(&independent_old_graph);
    assert!(dependent.comparisons.is_empty());
}

#[test]
fn stored_evidence_reaches_typed_changes_through_the_common_graph_pipeline() {
    use pdfdelta_core::{
        document::{
            CorrespondenceScope, DocumentComparisonLimits, DocumentGraph, DocumentView,
            GraphLimits, compare_scope_views,
        },
        pipeline::PipelineOptions,
    };
    let (old_store, _) = value("100");
    let (new_store, _) = value("20");
    let old = DocumentGraph::from_evidence(
        &old_store,
        PipelineOptions::default(),
        EvidenceLimits::default(),
        GraphLimits::default(),
    )
    .expect("native and structured graph adapter");
    let new = DocumentGraph::from_evidence(
        &new_store,
        PipelineOptions::default(),
        EvidenceLimits::default(),
        GraphLimits::default(),
    )
    .expect("native and structured graph adapter");
    let result = compare_scope_views(
        DocumentView {
            evidence: &old_store,
            graph: &old,
        },
        DocumentView {
            evidence: &new_store,
            graph: &new,
        },
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        DocumentComparisonLimits::default(),
    )
    .expect("common scope comparison");
    assert_eq!(result.comparisons.len(), 1);
    assert_eq!(
        result.comparisons[0].operation,
        Some(TypedOperation::ValueChanged {
            old: FieldValue::Text("100".into()),
            new: FieldValue::Text("20".into()),
        })
    );
}

mod typed_relations {
    use super::*;
    use pdfdelta_core::document::{
        Channel, ChannelInventory, CorrespondenceScope, DocumentComparisonLimits, DocumentGraph,
        DocumentView, DocumentViewComparison, EdgeKind, GraphEdge, HierarchyLimits, IdentityKey,
        compare_document_views,
    };

    fn fixture(swapped: bool, kind: EdgeKind) -> (EvidenceStore, DocumentGraph) {
        let (mut evidence, mut first) = value("100");
        let (second_store, mut second) = value("20");
        let mut second_evidence = second_store.structured[0].clone();
        second_evidence.id = 2;
        evidence.structured.push(second_evidence);
        second.id = NodeId(2);
        second.sources = vec![SourceRef::Structured { element: 2 }];
        for (node, key) in [(&mut first, "first-value"), (&mut second, "second-value")] {
            node.kind = NodeKind::Cell;
            node.identity = Some(IdentityKey {
                namespace: "cell".into(),
                value: key.into(),
            });
        }
        evidence.inventories.push(ChannelInventory {
            page: None,
            channel: Channel::Relations,
            backend: 0,
            sources: vec![
                SourceRef::Structured { element: 1 },
                SourceRef::Structured { element: 2 },
            ],
            complete: true,
        });
        let container = |id, node_kind, name: &str| GraphNode {
            id: NodeId(id),
            kind: node_kind,
            pages: Vec::new(),
            sources: Vec::new(),
            identity: Some(IdentityKey {
                namespace: "table".into(),
                value: name.into(),
            }),
            basis: ViewBasis::SourceStructure,
            content: NodeContent::Container,
        };
        let axis = if kind == EdgeKind::RowMember {
            NodeKind::Row
        } else {
            NodeKind::Column
        };
        let mut graph = DocumentGraph {
            nodes: vec![
                container(0, NodeKind::Table, "table"),
                container(10, axis, "revenue"),
                container(20, axis, "profit"),
                first,
                second,
            ],
            relations_complete: true,
            ..DocumentGraph::default()
        };
        for id in [10, 20, 1, 2] {
            graph.edges.push(GraphEdge {
                from: NodeId(0),
                to: NodeId(id),
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::SourceStructure,
            });
        }
        for (from, to) in if swapped {
            [(10, 2), (20, 1)]
        } else {
            [(10, 1), (20, 2)]
        } {
            graph.edges.push(GraphEdge {
                from: NodeId(from),
                to: NodeId(to),
                kind,
                sources: vec![SourceRef::Structured { element: to }],
                basis: ViewBasis::SourceStructure,
            });
        }
        (evidence, graph)
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
        .expect("typed relation comparison")
    }

    #[test]
    fn unchanged_values_do_not_hide_swapped_row_or_column_membership() {
        for kind in [EdgeKind::RowMember, EdgeKind::ColumnMember] {
            let old = fixture(false, kind);
            let mut new = fixture(true, kind);
            new.1.nodes.reverse();
            new.1.edges.reverse();
            let result = compare(&old, &new, DocumentComparisonLimits::default());
            assert!(result.search_resolved(), "{:?}", result.relation_unresolved);
            assert!(result.comparisons().all(|pair| pair.operation.is_none()));
            assert_eq!(
                result
                    .relations()
                    .filter(|relation| relation.changed())
                    .count(),
                4
            );
            assert!(
                result
                    .relations()
                    .filter(|relation| relation.kind == kind)
                    .all(|relation| relation.kind == kind
                        && relation.dependencies.len() == 2
                        && relation.interpretation
                            == InterpretationStatus::ConditionalOnCorrespondence)
            );
            let equal = compare(&old, &old, DocumentComparisonLimits::default());
            assert!(equal.search_resolved());
            assert_eq!(
                equal
                    .relations()
                    .filter(|relation| relation.kind == kind)
                    .count(),
                2
            );
            assert!(equal.relations().all(|relation| !relation.changed()));
        }
    }

    #[test]
    fn incomplete_relationship_inventory_does_not_prove_missing_edges() {
        let mut old = fixture(false, EdgeKind::RowMember);
        let mut new = fixture(true, EdgeKind::RowMember);
        old.0.inventories.clear();
        new.0.inventories.clear();
        let result = compare(&old, &new, DocumentComparisonLimits::default());
        assert!(result.relations().all(|relation| !relation.changed()));
        assert!(!result.relation_unresolved.is_empty());
        assert_eq!(result.comparisons().filter(|pair| pair.compared).count(), 2);
    }

    #[test]
    fn nonmembership_relationship_changes_keep_content_and_absence_proofs_separate() {
        for kind in [
            EdgeKind::LabelFor,
            EdgeKind::CaptionFor,
            EdgeKind::RefersTo,
            EdgeKind::AppearanceFor,
            EdgeKind::Precedes,
        ] {
            let old = fixture(false, kind);
            let mut new = fixture(true, kind);
            let result = compare(&old, &new, DocumentComparisonLimits::default());
            assert!(result.comparisons().all(|pair| pair.operation.is_none()));
            let changed: Vec<_> = result.relations().filter(|edge| edge.changed()).collect();
            assert_eq!(changed.len(), 4, "{kind:?}");
            assert!(
                changed
                    .iter()
                    .all(|edge| edge.kind == kind && edge.dependencies.len() == 2)
            );

            new.0.inventories.clear();
            let incomplete = compare(&old, &new, DocumentComparisonLimits::default());
            let additions: Vec<_> = incomplete
                .relations()
                .filter(|edge| edge.changed())
                .collect();
            assert_eq!(additions.len(), 2, "{kind:?}");
            assert!(
                additions
                    .iter()
                    .all(|edge| !edge.old_present && edge.new_present)
            );
            assert!(!incomplete.relation_unresolved.is_empty());
            assert_eq!(
                incomplete
                    .comparisons()
                    .filter(|pair| pair.compared)
                    .count(),
                2
            );
        }
    }

    #[test]
    fn relation_budget_and_ambiguous_endpoints_preserve_independent_value_checks() {
        let mut old = fixture(false, EdgeKind::RowMember);
        let mut new = fixture(true, EdgeKind::RowMember);
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_pair_checks = 32;
        let limited = compare(&old, &new, limits);
        assert!(limited.relations.is_empty());
        assert!(
            limited
                .relation_unresolved
                .iter()
                .any(|reason| reason.contains("work budget"))
        );
        assert_eq!(
            limited.comparisons().filter(|pair| pair.compared).count(),
            2
        );
        for graph in [&mut old.1, &mut new.1] {
            for node in &mut graph.nodes {
                if node.kind == NodeKind::Row {
                    node.identity.as_mut().expect("row key").value = "duplicate".into();
                }
            }
        }
        let ambiguous = compare(&old, &new, DocumentComparisonLimits::default());
        assert!(ambiguous.relations().all(|relation| !relation.changed()));
        assert!(
            ambiguous
                .relation_unresolved
                .iter()
                .any(|reason| reason.contains("endpoints"))
        );
        assert_eq!(
            ambiguous.comparisons().filter(|pair| pair.compared).count(),
            2
        );
    }

    fn table_fixture(values: [&str; 2], nested: bool) -> (EvidenceStore, DocumentGraph) {
        let (mut evidence, mut graph) = fixture(false, EdgeKind::RowMember);
        for (index, text) in values.into_iter().enumerate() {
            let id = index as u64 + 1;
            let value = FieldValue::Text(text.into());
            let StructuredValue::FormField { value: stored, .. } =
                &mut evidence.structured[index].value
            else {
                unreachable!()
            };
            *stored = value.clone();
            let node = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == NodeId(id))
                .expect("cell");
            node.identity = None;
            node.content = NodeContent::Value { value };
        }
        graph.nodes.push(GraphNode {
            id: NodeId(30),
            kind: NodeKind::Column,
            pages: Vec::new(),
            sources: Vec::new(),
            identity: Some(IdentityKey {
                namespace: "column".into(),
                value: "current".into(),
            }),
            basis: ViewBasis::SourceStructure,
            content: NodeContent::Container,
        });
        graph.edges.push(GraphEdge {
            from: NodeId(0),
            to: NodeId(30),
            kind: EdgeKind::Contains,
            sources: Vec::new(),
            basis: ViewBasis::SourceStructure,
        });
        for id in [1, 2] {
            graph.edges.push(GraphEdge {
                from: NodeId(30),
                to: NodeId(id),
                kind: EdgeKind::ColumnMember,
                sources: vec![SourceRef::Structured { element: id }],
                basis: ViewBasis::SourceStructure,
            });
        }
        if nested {
            graph.edges.retain(|edge| edge.kind != EdgeKind::RowMember);
            for edge in &mut graph.edges {
                if edge.kind == EdgeKind::Contains && [NodeId(1), NodeId(2)].contains(&edge.to) {
                    edge.from = NodeId(edge.to.0 * 10);
                }
            }
        }
        (evidence, graph)
    }

    #[test]
    fn cell_values_follow_row_and_column_identity_in_flat_and_nested_tables() {
        use pdfdelta_core::document::ProposalBasis;
        for nested in [false, true] {
            let old = table_fixture(["100", "20"], nested);
            let mut new = table_fixture(["20", "100"], nested);
            new.1.nodes.reverse();
            new.1.edges.reverse();
            let result = compare(&old, &new, DocumentComparisonLimits::default());
            assert!(result.search_resolved(), "{:?}", result.relation_unresolved);
            assert_eq!(
                result
                    .comparisons()
                    .filter(|pair| matches!(
                        pair.operation,
                        Some(TypedOperation::ValueChanged { .. })
                    ) && pair.interpretation
                        == InterpretationStatus::ConditionalOnCorrespondence)
                    .count(),
                2
            );
            assert!(result.relations().all(|relation| !relation.changed()));
            assert_eq!(
                result
                    .scopes
                    .iter()
                    .flat_map(|scope| &scope.result.candidates.proposals)
                    .filter(|proposal| proposal.basis == ProposalBasis::TableCellIdentity)
                    .count(),
                2
            );
        }
    }

    #[test]
    fn incomplete_or_competing_cell_memberships_cannot_fall_back_to_equal_values() {
        let old = table_fixture(["100", "20"], false);
        for missing in [false, true] {
            let mut new = table_fixture(["20", "100"], false);
            if missing {
                new.1
                    .edges
                    .retain(|edge| !(edge.kind == EdgeKind::ColumnMember && edge.to == NodeId(1)));
            } else {
                new.1.edges.push(GraphEdge {
                    from: NodeId(20),
                    to: NodeId(1),
                    kind: EdgeKind::RowMember,
                    sources: vec![SourceRef::Structured { element: 1 }],
                    basis: ViewBasis::SourceStructure,
                });
            }
            let result = compare(&old, &new, DocumentComparisonLimits::default());
            assert!(!result.scopes[0].result.candidates.exhaustive);
            assert!(result.comparisons().next().is_none());
        }
    }

    #[test]
    fn duplicate_axis_keys_and_candidate_budgets_do_not_create_unique_cells() {
        use pdfdelta_core::document::{MatchingLimits, propose_scope_correspondences};
        let old = table_fixture(["100", "20"], false);
        let mut new = table_fixture(["20", "100"], false);
        let mut duplicate = new
            .1
            .nodes
            .iter()
            .find(|node| node.id == NodeId(30))
            .expect("column")
            .clone();
        duplicate.id = NodeId(40);
        new.1.nodes.push(duplicate);
        new.1.edges.push(GraphEdge {
            from: NodeId(0),
            to: NodeId(40),
            kind: EdgeKind::Contains,
            sources: Vec::new(),
            basis: ViewBasis::SourceStructure,
        });
        let result = compare(&old, &new, DocumentComparisonLimits::default());
        assert!(!result.scopes[0].result.candidates.exhaustive);
        assert!(result.comparisons().next().is_none());
        let limited = propose_scope_correspondences(
            &old.1,
            &old.1,
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            MatchingLimits {
                max_ownership_visits: 1,
                ..MatchingLimits::default()
            },
        )
        .expect("bounded candidate supply");
        assert!(!limited.exhaustive);
        assert_eq!(limited.group_constraint_checks, 1);
        assert!(limited.proposals.is_empty());
    }

    #[test]
    fn model_axis_identity_keeps_cell_changes_inferred() {
        let old = table_fixture(["100", "20"], false);
        let mut new = table_fixture(["20", "100"], false);
        new.0.backends.push(BackendIdentity {
            kind: BackendKind::StructureModel,
            name: "axis-model".into(),
            version: "1".into(),
            profile: "table".into(),
            model: Some("fixture".into()),
        });
        new.1
            .nodes
            .iter_mut()
            .find(|node| node.id == NodeId(30))
            .expect("column")
            .basis = ViewBasis::Model { backend: 1 };
        let result = compare(&old, &new, DocumentComparisonLimits::default());
        assert_eq!(
            result
                .comparisons()
                .filter(|pair| pair.operation.is_some()
                    && pair.interpretation == InterpretationStatus::Inferred)
                .count(),
            2
        );
        assert!(
            result
                .comparisons()
                .all(|pair| pair.interpretation == InterpretationStatus::Inferred)
        );
    }

    #[test]
    fn cell_suppliers_cannot_forge_coordinates_or_match_only_values() {
        use pdfdelta_core::document::{
            CorrespondenceProposal, MatchingLimits, ProposalBasis, solve_correspondence_scope,
        };
        let old = table_fixture(["100", "20"], false);
        let new = table_fixture(["20", "100"], false);
        for basis in [
            ProposalBasis::TableCellIdentity,
            ProposalBasis::LiteralContent,
        ] {
            let proposal = CorrespondenceProposal {
                old: vec![NodeId(1)],
                new: vec![NodeId(2)],
                basis,
                supplier: "forged-cell".into(),
                weight: 1,
            };
            assert!(
                solve_correspondence_scope(
                    &old.1,
                    &new.1,
                    CorrespondenceScope {
                        old: NodeId(0),
                        new: NodeId(0)
                    },
                    &[proposal],
                    MatchingLimits::default()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn inferred_relationships_and_unexamined_relations_stay_separate() {
        let old = fixture(false, EdgeKind::RowMember);
        let mut new = fixture(true, EdgeKind::RowMember);
        new.0.backends.push(BackendIdentity {
            kind: BackendKind::StructureModel,
            name: "fixture-model".into(),
            version: "1".into(),
            profile: "relations".into(),
            model: Some("fixture".into()),
        });
        for edge in &mut new.1.edges {
            if edge.kind == EdgeKind::RowMember {
                edge.basis = ViewBasis::Model { backend: 1 };
            }
        }
        let result = compare(&old, &new, DocumentComparisonLimits::default());
        assert_eq!(
            result
                .relations()
                .filter(|relation| relation.changed()
                    && relation.interpretation == InterpretationStatus::Inferred)
                .count(),
            2
        );
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.channels.relations = false;
        let unselected = compare(&old, &new, limits);
        assert!(unselected.relations.is_empty());
        assert!(unselected.relation_unresolved.is_empty());
    }
}
