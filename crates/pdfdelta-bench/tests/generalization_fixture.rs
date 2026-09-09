use std::collections::BTreeSet;

use pdfdelta_bench::generalization::{
    Dimension, DimensionAnnotation, Fact, GENERALIZATION_SCHEMA_VERSION, GeneralizationAnnotation,
    evaluate,
};
use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, EdgeKind, EvidenceStore, FieldValue,
        GraphEdge, GraphNode, HierarchyLimits, IdentityKey, NodeContent, NodeId, NodeKind,
        SourceRef, StructuredEvidence, StructuredValue, TypedOperation, ViewBasis,
        compare_document_views,
    },
    model::Document,
};

fn fixture(values: [&str; 2], reverse: bool) -> (EvidenceStore, DocumentGraph) {
    let mut evidence = EvidenceStore {
        revision: "programmatic-fields-v1".into(),
        native: Document::new(Vec::new()),
        backends: vec![BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "programmatic-fixture".into(),
            version: "1".into(),
            profile: "stored-values".into(),
            model: None,
        }],
        pages: Vec::new(),
        rendered: Vec::new(),
        structured: Vec::new(),
        inventories: Vec::new(),
        issues: Vec::new(),
    };
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
    for (id, text) in [(1, values[0]), (2, values[1])] {
        let value = FieldValue::Text(text.into());
        let name = format!("field-{id}");
        evidence.structured.push(StructuredEvidence {
            id,
            page: None,
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::FormField {
                name: name.clone(),
                field_type: None,
                value: value.clone(),
                widgets: Vec::new(),
                button_states: Vec::new(),
            },
        });
        graph.nodes.push(GraphNode {
            id: NodeId(id),
            kind: NodeKind::Field,
            pages: Vec::new(),
            sources: vec![SourceRef::Structured { element: id }],
            identity: Some(IdentityKey {
                namespace: "form".into(),
                value: name,
            }),
            basis: ViewBasis::SourceStructure,
            content: NodeContent::Value { value },
        });
        graph.edges.push(GraphEdge {
            from: NodeId(0),
            to: NodeId(id),
            kind: EdgeKind::Contains,
            sources: Vec::new(),
            basis: ViewBasis::SourceStructure,
        });
    }
    if reverse {
        graph.nodes.reverse();
        graph.edges.reverse();
        evidence.structured.reverse();
    }
    evidence.inventories.push(ChannelInventory {
        page: None,
        channel: Channel::Forms,
        backend: 0,
        sources: evidence
            .structured
            .iter()
            .map(|element| SourceRef::Structured {
                element: element.id,
            })
            .collect(),
        complete: true,
    });
    (evidence, graph)
}

#[test]
fn successful_form_comparison_cannot_hide_uncompared_visual_evidence() {
    use pdfdelta_core::{
        document::{EvidenceFailure, EvidenceIssue, PageEvidence, Raster, RenderedEvidence},
        model::{PageId, Vec2},
    };

    let (mut evidence, graph) = fixture(["100", "20"], false);
    evidence.backends.push(BackendIdentity {
        kind: BackendKind::Renderer,
        name: "programmatic-raster".into(),
        version: "1".into(),
        profile: "rgb-1x1".into(),
        model: None,
    });
    evidence.pages.push(PageEvidence {
        page: PageId(0),
        bounds: None,
    });
    evidence.rendered.push(RenderedEvidence {
        id: 1,
        page: PageId(0),
        polygon: vec![
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 1.0, y: 0.0 },
            Vec2 { x: 1.0, y: 1.0 },
            Vec2 { x: 0.0, y: 1.0 },
        ],
        backend: 1,
        raster: Raster {
            width: 1,
            height: 1,
            rgb: vec![0, 0, 0],
        },
        composited_page: true,
    });
    evidence.inventories.push(ChannelInventory {
        page: Some(PageId(0)),
        channel: Channel::Visual,
        backend: 1,
        sources: vec![SourceRef::Rendered { region: 1 }],
        complete: true,
    });
    let annotation = GeneralizationAnnotation {
        schema_version: GENERALIZATION_SCHEMA_VERSION,
        channels: BTreeSet::from([Channel::Forms, Channel::Visual]),
        dimensions: Vec::new(),
    };
    for extraction_issue in [false, true] {
        if extraction_issue {
            evidence.issues.push(EvidenceIssue {
                page: Some(PageId(0)),
                channel: Channel::Visual,
                sources: Vec::new(),
                kind: EvidenceFailure::Unresolved,
                reason: "additional region could not be discovered".into(),
            });
        }
        let view = DocumentView {
            evidence: &evidence,
            graph: &graph,
        };
        let comparison = compare_document_views(
            view,
            view,
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            DocumentComparisonLimits::default(),
            HierarchyLimits::default(),
        )
        .expect("compare form graph with retained visual evidence");
        let result =
            evaluate(&annotation, view, view, &comparison).expect("score all selected channels");
        assert!(!result.comparison_complete);
        let forms = result
            .coverage
            .iter()
            .find(|coverage| coverage.channel == Channel::Forms)
            .expect("forms selected");
        assert!(forms.complete);
        let visual = result
            .coverage
            .iter()
            .find(|coverage| coverage.channel == Channel::Visual)
            .expect("visual selected");
        assert_eq!(visual.old_discovered_sources, 1);
        assert_eq!(visual.old_uncompared_sources, 1);
        assert_eq!(visual.old_compared_sources, 0);
        assert_eq!(visual.old_inventory_complete, !extraction_issue);
        assert_eq!(result.old_evidence_issues, usize::from(extraction_issue));
    }
}

#[test]
fn stored_value_mutations_cross_storage_order_through_common_solver() {
    // This matrix exercises graph storage order, not PDF rendering or producer coverage.
    for (old_values, new_values) in [
        (["100", "20"], ["100", "20"]),
        (["100", "20"], ["20", "100"]),
        (["required", "10 kg"], ["not required", "10 mg"]),
        (["承認", "利益20円"], ["否認", "利益100円"]),
    ] {
        for reverse in [false, true] {
            let old = fixture(old_values, false);
            let new = fixture(new_values, reverse);
            let comparison = compare_document_views(
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
                HierarchyLimits::default(),
            )
            .expect("compare stored values through graph solver");
            let changes = [(1, 0), (2, 1)]
                .into_iter()
                .filter(|(_, index)| old_values[*index] != new_values[*index])
                .map(|(id, index)| Fact::ChangeUnit {
                    old: Some(vec![NodeId(id)]),
                    new: Some(vec![NodeId(id)]),
                    operation: TypedOperation::ValueChanged {
                        old: FieldValue::Text(old_values[index].into()),
                        new: FieldValue::Text(new_values[index].into()),
                    },
                })
                .collect();
            let annotation = GeneralizationAnnotation {
                schema_version: GENERALIZATION_SCHEMA_VERSION,
                channels: BTreeSet::from([Channel::Forms]),
                dimensions: vec![DimensionAnnotation {
                    dimension: Dimension::ChangeUnit,
                    alternatives: vec![changes],
                }],
            };
            let result = evaluate(
                &annotation,
                DocumentView {
                    evidence: &old.0,
                    graph: &old.1,
                },
                DocumentView {
                    evidence: &new.0,
                    graph: &new.1,
                },
                &comparison,
            )
            .expect("evaluate graph changes");
            let score = &result.dimensions[0].alternatives[0];
            assert_eq!(score.false_positive, 0, "{old_values:?} -> {new_values:?}");
            assert_eq!(score.false_negative, 0, "{old_values:?} -> {new_values:?}");
            assert_eq!(result.dimensions[0].inferred_reports, 0);
            assert!(result.comparison_complete);
            assert_eq!(result.coverage[0].old_compared_sources, 2);
            assert_eq!(result.coverage[0].new_uncompared_sources, 0);
        }
    }
}

#[test]
fn date_review_ranges_do_not_inflate_changed_character_scores() {
    for (before, after, length) in [
        ("2026-09-08", "2026-09-09", 10),
        ("期限2026-09-08", "期限2026-09-09", 12),
    ] {
        let old = fixture([before, "stable"], false);
        let new = fixture([after, "stable"], true);
        let old_view = DocumentView {
            evidence: &old.0,
            graph: &old.1,
        };
        let new_view = DocumentView {
            evidence: &new.0,
            graph: &new.1,
        };
        let comparison = compare_document_views(
            old_view,
            new_view,
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            DocumentComparisonLimits::default(),
            HierarchyLimits::default(),
        )
        .expect("compare date values");
        let positions = |position| {
            [true, false].map(|old_side| Fact::TextPosition {
                old: vec![NodeId(1)],
                new: vec![NodeId(1)],
                convention: "literal-minimal-source-tokens-v1".into(),
                old_side,
                position,
            })
        };
        let mut annotation = GeneralizationAnnotation {
            schema_version: GENERALIZATION_SCHEMA_VERSION,
            channels: BTreeSet::from([Channel::Forms]),
            dimensions: vec![
                DimensionAnnotation {
                    dimension: Dimension::DisplayRange,
                    alternatives: vec![
                        [true, false]
                            .map(|old_side| Fact::DisplayRange {
                                old: vec![NodeId(1)],
                                new: vec![NodeId(1)],
                                old_side,
                                start: 0,
                                end: length,
                            })
                            .to_vec(),
                    ],
                },
                DimensionAnnotation {
                    dimension: Dimension::TextPosition,
                    alternatives: vec![positions(length - 1).to_vec()],
                },
            ],
        };
        let evaluate_date = |annotation: &GeneralizationAnnotation| {
            evaluate(annotation, old_view, new_view, &comparison).expect("score date ranges")
        };
        let result = evaluate_date(&annotation);
        for dimension in &result.dimensions {
            assert_eq!(dimension.alternatives[0].true_positive, 2);
            assert_eq!(dimension.alternatives[0].false_positive, 0);
            assert_eq!(dimension.alternatives[0].false_negative, 0);
        }

        // A whole-value display cannot satisfy an annotation for only its last
        // character, while the independent exact character score stays correct.
        for fact in &mut annotation.dimensions[0].alternatives[0] {
            if let Fact::DisplayRange { start, .. } = fact {
                *start = length - 1;
            }
        }
        let result = evaluate_date(&annotation);
        assert_eq!(result.dimensions[0].alternatives[0].true_positive, 0);
        assert_eq!(result.dimensions[0].alternatives[0].false_positive, 2);
        assert_eq!(result.dimensions[0].alternatives[0].false_negative, 2);
        assert_eq!(result.dimensions[1].alternatives[0].true_positive, 2);

        annotation.dimensions[1].alternatives[0] = (0..length).flat_map(positions).collect();
        let result = evaluate_date(&annotation);
        assert_eq!(result.dimensions[1].alternatives[0].true_positive, 2);
        assert_eq!(
            result.dimensions[1].alternatives[0].false_negative,
            2 * (length - 1)
        );
    }
}
