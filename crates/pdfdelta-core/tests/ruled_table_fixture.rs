use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, CorrespondenceScope, DocumentComparisonLimits, DocumentGraph,
        DocumentView, EdgeKind, EvidenceLimits, EvidenceStore, GraphLimits, HierarchyLimits,
        InterpretationStatus, NodeContent, NodeId, NodeKind, PageEvidence, StructuredEvidence,
        StructuredValue, TypedOperation, compare_document_views, refine_table_views,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2, VectorLine, VectorLineId,
    },
    pdf::ObjectRef,
    pipeline::PipelineOptions,
};

fn table(
    values: [&str; 2],
    scale: f64,
    middle_border: bool,
    duplicate_labels: bool,
) -> EvidenceStore {
    let provenance = GlyphProvenance {
        content_stream: ObjectRef {
            object_number: 1,
            generation: 0,
        },
        operator_index: 0,
    };
    let texts = [
        "Item",
        "Quantity",
        "Material A",
        values[0],
        if duplicate_labels {
            "Material A"
        } else {
            "Material B"
        },
        values[1],
    ];
    let glyphs = texts
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let x = (5.0 + 100.0 * (index % 2) as f64) * scale;
            let y = (66.0 - 30.0 * (index / 2) as f64) * scale;
            Glyph {
                id: GlyphId(index as u64),
                text: DecodedText::Mapped(text.into()),
                raw_code: vec![index as u8],
                page: PageId(0),
                bbox: Rect {
                    min: Vec2 { x, y },
                    max: Vec2 {
                        x: x + 60.0 * scale,
                        y: y + 12.0 * scale,
                    },
                },
                baseline: Vec2 {
                    x,
                    y: y + 2.0 * scale,
                },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(0),
                font_size: 12.0 * scale,
                render_order: index as u32,
                render_mode: TextRenderMode::Fill,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance,
            }
        })
        .collect();
    let mut borders = Vec::new();
    let mut line = |x1, y1, x2, y2| {
        borders.push(VectorLine {
            id: VectorLineId(borders.len() as u64),
            page: PageId(0),
            from: Vec2 {
                x: x1 * scale,
                y: y1 * scale,
            },
            to: Vec2 {
                x: x2 * scale,
                y: y2 * scale,
            },
            width: 0.5 * scale,
            render_order: 6,
            provenance,
        });
    };
    for y in [0.0, 30.0, 60.0, 90.0] {
        line(0.0, y, 200.0, y);
    }
    for x in [0.0, 100.0, 200.0] {
        if x == 100.0 && !middle_border {
            continue;
        }
        // TeX draws a separate vertical segment for each row.
        for y in [0.0, 30.0, 60.0] {
            line(x, y, x, y + 30.0);
        }
    }
    EvidenceStore {
        revision: "programmatic-table".into(),
        native: Document::with_vector_lines(glyphs, borders),
        backends: vec![BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "fixture".into(),
            version: "1".into(),
            profile: "raw-glyphs".into(),
            model: None,
        }],
        pages: vec![PageEvidence {
            page: PageId(0),
            bounds: Some(Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 {
                    x: 220.0 * scale,
                    y: 120.0 * scale,
                },
            }),
        }],
        rendered: Vec::new(),
        structured: Vec::new(),
        inventories: Vec::new(),
        issues: Vec::new(),
    }
}

fn graph(store: &EvidenceStore) -> DocumentGraph {
    DocumentGraph::from_native(
        store,
        PipelineOptions::default(),
        EvidenceLimits::default(),
        GraphLimits::default(),
    )
    .expect("reconstruct native table evidence")
}

#[test]
fn swapped_values_follow_row_labels_and_preserve_original_partitions() {
    for scale in [0.5, 1.0, 3.0] {
        let old = table(["100 kg", "20 kg"], scale, true, false);
        let new = table(["20 kg", "100 kg"], scale, true, false);
        let old_graph = graph(&old);
        let new_graph = graph(&new);
        assert_eq!(
            old_graph
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Table)
                .count(),
            1
        );
        assert_eq!(old_graph.alternatives.len(), 1);
        assert_eq!(old_graph.alternatives[0].partitions.len(), 2);
        assert_eq!(old.native.items().len(), 6);
        let comparison = compare_document_views(
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
        .expect("compare rows through the shared table-axis solver");
        let changes: Vec<_> = comparison
            .comparisons()
            .filter_map(|pair| {
                pair.operation
                    .as_ref()
                    .map(|operation| (operation, pair.interpretation))
            })
            .collect();
        assert_eq!(changes.len(), 2);
        for (before, after) in [("100 kg", "20 kg"), ("20 kg", "100 kg")] {
            assert!(changes.contains(&(
                &TypedOperation::TextChanged {
                    old: Some(before.into()),
                    new: Some(after.into())
                },
                InterpretationStatus::Inferred
            )));
        }
    }
}

#[test]
fn missing_separator_or_duplicate_row_labels_keep_original_layout() {
    for (middle_border, duplicate_labels) in [(false, false), (true, true)] {
        let store = table(["100 kg", "20 kg"], 1.0, middle_border, duplicate_labels);
        let graph = graph(&store);
        assert!(!graph.nodes.iter().any(|node| node.kind == NodeKind::Table));
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.kind == NodeKind::Paragraph)
        );
        assert_eq!(store.native.items().len(), 6);
    }
}

#[test]
fn optional_table_view_budget_keeps_native_text_available() {
    let baseline = graph(&table(["100 kg", "20 kg"], 1.0, false, false));
    let store = table(["100 kg", "20 kg"], 1.0, true, false);
    let graph = DocumentGraph::from_native(
        &store,
        PipelineOptions::default(),
        EvidenceLimits::default(),
        GraphLimits {
            max_nodes: baseline.nodes.len(),
            ..GraphLimits::default()
        },
    )
    .expect("optional grid views do not consume the native graph budget");
    assert!(!graph.nodes.iter().any(|node| node.kind == NodeKind::Table));
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Paragraph)
    );
    assert!(!graph.relations_complete);
}

#[test]
fn renamed_column_keeps_row_values_in_their_matched_axes() {
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let mut new = table(["20 kg", "100 kg"], 1.0, true, false);
    let mut glyphs = new.native.items().to_vec();
    glyphs[1].text = DecodedText::Mapped("Amount".into());
    new.native = Document::with_vector_lines(glyphs, new.native.vector_lines().to_vec());
    let old_graph = graph(&old);
    let new_graph = graph(&new);
    let comparison = compare_document_views(
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
    .expect("compare changed column label and swapped quantities");
    let changes: Vec<_> = comparison
        .comparisons()
        .filter_map(|pair| {
            pair.operation
                .as_ref()
                .map(|operation| (operation, pair.interpretation))
        })
        .collect();
    assert_eq!(changes.len(), 3);
    for (before, after) in [
        ("Quantity", "Amount"),
        ("100 kg", "20 kg"),
        ("20 kg", "100 kg"),
    ] {
        assert!(changes.contains(&(
            &TypedOperation::TextChanged {
                old: Some(before.into()),
                new: Some(after.into())
            },
            InterpretationStatus::Inferred
        )));
    }
}

#[test]
fn renamed_column_without_axis_context_does_not_match_values_by_equality() {
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let mut new = table(["20 kg", "100 kg"], 1.0, true, false);
    let mut glyphs = new.native.items().to_vec();
    glyphs[1].text = DecodedText::Mapped("Amount".into());
    new.native = Document::with_vector_lines(glyphs, new.native.vector_lines().to_vec());
    let mut old_graph = graph(&old);
    let mut new_graph = graph(&new);
    for graph in [&mut old_graph, &mut new_graph] {
        let columns: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Column)
            .map(|node| node.id)
            .collect();
        graph
            .edges
            .retain(|edge| edge.kind != EdgeKind::Precedes || !columns.contains(&edge.from));
    }
    let comparison = compare_document_views(
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
    .expect("missing axis context remains unpaired");
    let quantities: Vec<_> = old_graph
        .nodes
        .iter()
        .filter(|node| {
            node.kind == NodeKind::Cell
                && matches!(&node.content, NodeContent::Text { view }
            if matches!(view.display_text().as_deref(), Some("100 kg" | "20 kg")))
        })
        .map(|node| node.id)
        .collect();
    assert_eq!(quantities.len(), 2);
    assert!(
        comparison.comparisons().all(|pair| {
            !pair.compared || pair.old.iter().all(|node| !quantities.contains(node))
        })
    );
}

#[test]
fn structural_search_limit_preserves_independent_native_text() {
    let mut old = table(["100 kg", "20 kg"], 1.0, true, false);
    let mut new = table(["20 kg", "100 kg"], 1.0, true, false);
    for store in [&mut old, &mut new] {
        let mut glyphs = store.native.items().to_vec();
        let mut independent = glyphs[0].clone();
        independent.id = GlyphId(6);
        independent.raw_code = vec![6];
        independent.render_order = 100;
        independent.text = DecodedText::Mapped("independent".into());
        independent.bbox.min.y = 150.0;
        independent.bbox.max.y = 162.0;
        independent.baseline.y = 152.0;
        glyphs.push(independent);
        store.native = Document::with_vector_lines(glyphs, store.native.vector_lines().to_vec());
        store.pages[0].bounds.as_mut().expect("page bounds").max.y = 200.0;
    }
    let old_graph = graph(&old);
    let new_graph = graph(&new);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_token_checks = 0;
    let comparison = compare_document_views(
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
    .expect("optional structural exhaustion is local");
    assert!(!comparison.search_resolved());
    assert!(
        comparison.scopes.iter().any(|scope| {
            scope
                .result
                .unresolved
                .iter()
                .any(|reason| reason == "structural candidate enumeration is incomplete")
        }),
        "{comparison:#?}"
    );
    let independent = old_graph
        .nodes
        .iter()
        .find(|node| {
            matches!(&node.content, NodeContent::Text { view }
            if view.display_text().as_deref() == Some("independent"))
        })
        .expect("independent native paragraph");
    assert!(
        comparison.comparisons().any(|pair| {
            pair.compared && pair.old == [independent.id] && pair.operation.is_none()
        })
    );
}

#[test]
fn original_partition_compares_when_only_one_side_has_a_ruled_grid() {
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let new = table(["100 kg", "20 kg"], 1.0, false, false);
    let old_graph = graph(&old);
    let new_graph = graph(&new);
    assert_eq!(old_graph.alternatives.len(), 1);
    assert!(new_graph.alternatives.is_empty());
    let original = &old_graph.alternatives[0].partitions[0];
    let comparison = compare_document_views(
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
    .expect("original text remains available when the table view has no counterpart");
    for node in original {
        assert!(
            comparison.comparisons().any(|pair| pair.compared
                && pair.old.contains(node)
                && pair.operation.is_none()
                && pair.interpretation == InterpretationStatus::Inferred),
            "missing {node:?}: {comparison:#?}"
        );
    }
}

#[test]
fn counterpart_axes_repartition_missing_borders_without_matching_values() {
    for rename_header in [false, true] {
        let old = table(["100 kg", "20 kg"], 1.0, true, false);
        let mut new = table(["20 kg", "100 kg"], 1.0, false, false);
        let mut glyphs = new.native.items().to_vec();
        // A row label can start one representable step left of its header.
        glyphs[2].bbox.min.x = glyphs[2].bbox.min.x.next_down();
        if rename_header {
            glyphs[1].text = DecodedText::Mapped("Amount".into());
        }
        new.native = Document::new(glyphs);
        new.structured.push(StructuredEvidence {
            id: 0,
            page: Some(PageId(0)),
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::StructureElement {
                role: "TD".into(),
                identifier: None,
                text: None,
                glyphs: vec![GlyphId(3)],
                parent: None,
                order: None,
            },
        });
        let mut old_graph = graph(&old);
        let mut new_graph = DocumentGraph::from_evidence(
            &new,
            PipelineOptions::default(),
            EvidenceLimits::default(),
            GraphLimits::default(),
        )
        .expect("retain overlapping tag membership");
        let refinements = refine_table_views(
            &mut old_graph,
            &mut new_graph,
            &old,
            &new,
            PipelineOptions::default(),
            DocumentComparisonLimits::default(),
        )
        .expect("counterpart geometry");
        assert!(refinements.old.is_empty());
        assert_eq!(refinements.new.len(), 1);
        assert!(
            refinements.new[0]
                .counterpart_sources
                .iter()
                .any(|source| matches!(
                    source,
                    pdfdelta_core::document::SourceRef::NativeVector { .. }
                ))
        );
        assert!(
            refinements.new[0]
                .anchor_sources
                .iter()
                .all(|source| matches!(source, pdfdelta_core::document::SourceRef::Native { .. }))
        );
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
        .expect("compare refined rows");
        let changes: Vec<_> = result
            .comparisons()
            .filter_map(|pair| {
                pair.operation
                    .as_ref()
                    .map(|operation| (operation, pair.interpretation))
            })
            .collect();
        assert_eq!(changes.len(), 2 + usize::from(rename_header));
        for (old, new) in [("100 kg", "20 kg"), ("20 kg", "100 kg")] {
            assert!(changes.contains(&(
                &TypedOperation::TextChanged {
                    old: Some(old.into()),
                    new: Some(new.into())
                },
                InterpretationStatus::Inferred
            )));
        }
        if rename_header {
            assert!(changes.contains(&(
                &TypedOperation::TextChanged {
                    old: Some("Quantity".into()),
                    new: Some("Amount".into())
                },
                InterpretationStatus::Inferred,
            )));
        }
    }
}

#[test]
fn installed_counterpart_views_preserve_native_evidence_and_original_graph() {
    use pdfdelta_core::document::SourceRef;
    for scale in [0.5, 1.0, 3.0] {
        for rename_header in [false, true] {
            let old = table(["100 kg", "20 kg"], scale, true, false);
            let mut new = table(["20 kg", "100 kg"], scale, false, false);
            let mut glyphs = new.native.items().to_vec();
            // A row label can start one representable step left of its header.
            glyphs[2].bbox.min.x = glyphs[2].bbox.min.x.next_down();
            if rename_header {
                glyphs[1].text = DecodedText::Mapped("Amount".into());
            }
            new.native = Document::new(glyphs);
            new.structured.push(StructuredEvidence {
                id: 0,
                page: Some(PageId(0)),
                bounds: None,
                object: None,
                backend: 0,
                value: StructuredValue::StructureElement {
                    role: "TD".into(),
                    identifier: None,
                    text: None,
                    glyphs: vec![GlyphId(3)],
                    parent: None,
                    order: None,
                },
            });
            let mut old_graph = graph(&old);
            let mut new_graph = DocumentGraph::from_evidence(
                &new,
                PipelineOptions::default(),
                EvidenceLimits::default(),
                GraphLimits::default(),
            )
            .expect("retain overlapping tag membership");
            let old_nodes_before = old_graph.nodes.clone();
            let nodes_before = new_graph.nodes.clone();
            let edges_before = new_graph.edges.clone();

            let refinements = refine_table_views(
                &mut old_graph,
                &mut new_graph,
                &old,
                &new,
                PipelineOptions::default(),
                DocumentComparisonLimits::default(),
            )
            .expect("counterpart geometry");

            assert!(
                refinements.exhaustive,
                "scale {scale} rename {rename_header}"
            );
            assert_eq!(refinements.new.len(), 1);
            let view = &refinements.new[0];
            assert!(
                old_graph
                    .nodes
                    .iter()
                    .any(|node| node.id == view.counterpart_table && node.kind == NodeKind::Table),
                "counterpart table must exist in the old graph"
            );
            assert!(
                new_graph
                    .nodes
                    .iter()
                    .any(|node| node.id == view.target_table && node.kind == NodeKind::Table),
                "installed table must exist in the refined graph"
            );
            assert!(!view.counterpart_sources.is_empty());
            assert!(!view.anchor_sources.is_empty());
            for source in &view.anchor_sources {
                let SourceRef::Native { glyph } = source else {
                    panic!("anchor sources must be native glyphs: {source:?}");
                };
                assert!(
                    new.native.items().iter().any(|item| item.id == *glyph),
                    "anchor source {glyph:?} is missing from the new store"
                );
            }
            for node in &nodes_before {
                assert!(
                    new_graph
                        .nodes
                        .iter()
                        .any(|candidate| candidate.id == node.id),
                    "refinement removed original node {:?}",
                    node.id
                );
            }
            // Layout nodes covered by the grid are re-parented under an archive
            // node, so their original Contains edge may be replaced; every
            // replaced target must keep a Contains parent.
            for edge in &edges_before {
                if new_graph.edges.contains(edge) {
                    continue;
                }
                assert!(
                    new_graph.edges.iter().any(|candidate| {
                        candidate.kind == EdgeKind::Contains && candidate.to == edge.to
                    }),
                    "re-parented original edge {edge:?} lost its Contains parent"
                );
            }
            assert_eq!(
                old_graph.nodes, old_nodes_before,
                "a rejected old-side proposal must not mutate the old graph"
            );
            new_graph
                .validate(&new, EvidenceLimits::default(), GraphLimits::default())
                .expect("refined graph must stay valid against its evidence");
        }
    }
}

#[test]
fn counterpart_axes_reject_duplicate_labels_and_crossing_glyphs() {
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let duplicated = table(["Material A", "20 kg"], 1.0, false, false);
    let mut crossing = table(["100 kg", "20 kg"], 1.0, false, false);
    let mut glyphs = crossing.native.items().to_vec();
    glyphs[3].bbox.min.x = 50.0;
    crossing.native = Document::new(glyphs);
    for new in [duplicated, crossing] {
        let mut old_graph = graph(&old);
        let mut new_graph = graph(&new);
        let before = new_graph.nodes.clone();
        let result = refine_table_views(
            &mut old_graph,
            &mut new_graph,
            &old,
            &new,
            PipelineOptions::default(),
            DocumentComparisonLimits::default(),
        )
        .expect("reject unsupported partition");
        assert!(result.new.is_empty());
        assert!(result.exhaustive);
        assert_eq!(new_graph.nodes, before);
    }
}

#[test]
fn counterpart_storage_limit_does_not_report_exhaustive_refinement() {
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let new = table(["100 kg", "20 kg"], 1.0, false, false);
    let mut old_graph = graph(&old);
    let mut new_graph = graph(&new);
    let before = new_graph.nodes.clone();
    let mut limits = DocumentComparisonLimits::default();
    limits.graph.max_edges = old_graph.edges.len().max(new_graph.edges.len());
    let result = refine_table_views(
        &mut old_graph,
        &mut new_graph,
        &old,
        &new,
        PipelineOptions::default(),
        limits,
    )
    .expect("valid inputs survive optional storage exhaustion");
    assert!(result.new.is_empty());
    assert!(!result.exhaustive);
    assert!(!new_graph.relations_complete);
    assert_eq!(new_graph.nodes, before);
}

#[test]
fn counterpart_missing_target_page_bounds_does_not_report_exhaustive_refinement() {
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let mut new = table(["100 kg", "20 kg"], 1.0, false, false);
    for page in &mut new.pages {
        page.bounds = None;
    }
    let mut old_graph = graph(&old);
    let mut new_graph = graph(&new);
    let before = new_graph.nodes.clone();
    let result = refine_table_views(
        &mut old_graph,
        &mut new_graph,
        &old,
        &new,
        PipelineOptions::default(),
        DocumentComparisonLimits::default(),
    )
    .expect("missing page geometry is an outcome, not an error");
    assert!(result.new.is_empty());
    assert!(!result.exhaustive);
    assert_eq!(new_graph.nodes, before);
}

#[test]
fn counterpart_missing_header_does_not_choose_between_native_views() {
    use pdfdelta_core::document::{GraphEdge, SourceRef, ViewBasis};
    use pdfdelta_core::normalize::ComparableToken;
    let old = table(["100 kg", "20 kg"], 1.0, true, false);
    let mut new = table(["100 kg", "20 kg"], 1.0, false, false);
    let mut glyphs = new.native.items().to_vec();
    glyphs[1].text = DecodedText::Mapped("Amount".into());
    new.native = Document::new(glyphs.clone());
    let mut old_graph = graph(&old);
    let mut new_graph = graph(&new);
    let mut extra = glyphs[1].clone();
    extra.id = GlyphId(6);
    extra.render_order = 6;
    extra.bbox.min.x = 175.0;
    extra.bbox.max.x = 215.0;
    extra.baseline.x = 175.0;
    glyphs.push(extra);
    new.native = Document::new(glyphs);
    let mut extra_view = new_graph
        .nodes
        .iter()
        .find(|node| {
            node.sources
                .contains(&SourceRef::Native { glyph: GlyphId(1) })
        })
        .expect("native header source view")
        .clone();
    let id = NodeId(new_graph.nodes.len() as u64);
    let source = SourceRef::Native { glyph: GlyphId(6) };
    extra_view.id = id;
    extra_view.sources = vec![source];
    let NodeContent::Text { view } = &mut extra_view.content else {
        panic!("native text")
    };
    view.tokens = "Amount".chars().map(ComparableToken::Scalar).collect();
    view.origins = vec![vec![source]; view.tokens.len()];
    view.source_backed = vec![true; view.tokens.len()];
    new_graph.nodes.push(extra_view);
    new_graph.edges.push(GraphEdge {
        from: NodeId(0),
        to: id,
        kind: EdgeKind::Contains,
        sources: Vec::new(),
        basis: ViewBasis::NativeLayout,
    });
    let before = new_graph.nodes.clone();
    let result = refine_table_views(
        &mut old_graph,
        &mut new_graph,
        &old,
        &new,
        PipelineOptions::default(),
        DocumentComparisonLimits::default(),
    )
    .expect("ambiguous header evidence remains available");
    assert!(result.new.is_empty());
    assert!(result.exhaustive);
    assert_eq!(new_graph.nodes, before);
}
