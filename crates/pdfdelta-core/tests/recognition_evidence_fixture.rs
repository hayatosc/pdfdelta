use pdfdelta_core::{
    document::*,
    model::{Document, PageId, Rect, Vec2},
    pipeline::PipelineOptions,
};

fn fixture(text: &str) -> EvidenceStore {
    let render = RenderedEvidence {
        id: 7,
        page: PageId(0),
        backend: 0,
        polygon: vec![
            Vec2 { x: 0.0, y: 20.0 },
            Vec2 { x: 20.0, y: 20.0 },
            Vec2 { x: 20.0, y: 0.0 },
            Vec2 { x: 0.0, y: 0.0 },
        ],
        raster: Raster {
            width: 20,
            height: 20,
            rgb: vec![255; 1200],
        },
        composited_page: true,
    };
    let pixel_bounds = [0, 0, 10, 10];
    let bounds = render
        .pixel_bounds_in_page(pixel_bounds)
        .expect("valid crop");
    EvidenceStore {
        revision: "recognition-fixture".into(),
        native: Document::new(Vec::new()),
        backends: vec![
            BackendIdentity {
                kind: BackendKind::Renderer,
                name: "render".into(),
                version: "1".into(),
                profile: "rgb".into(),
                model: None,
            },
            BackendIdentity {
                kind: BackendKind::Ocr,
                name: "recognizer".into(),
                version: "1".into(),
                profile: "block".into(),
                model: Some("fixture-hash".into()),
            },
        ],
        pages: vec![PageEvidence {
            page: PageId(0),
            bounds: Some(Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 { x: 20.0, y: 20.0 },
            }),
        }],
        rendered: vec![render],
        structured: vec![StructuredEvidence {
            id: 8,
            page: Some(PageId(0)),
            bounds: Some(bounds),
            object: None,
            backend: 1,
            value: StructuredValue::RecognizedText {
                text: text.into(),
                region: 7,
                pixel_bounds,
                words: vec![RecognizedWord {
                    text: text.into(),
                    pixel_bounds: [1, 1, 9, 9],
                    confidence: Some(90.0),
                }],
            },
        }],
        inventories: vec![ChannelInventory {
            page: Some(PageId(0)),
            channel: Channel::Text,
            backend: 1,
            sources: vec![SourceRef::Structured { element: 8 }],
            complete: false,
        }],
        issues: Vec::new(),
    }
}

fn graph(store: &EvidenceStore) -> DocumentGraph {
    DocumentGraph::from_evidence(
        store,
        PipelineOptions::default(),
        EvidenceLimits::default(),
        GraphLimits::default(),
    )
    .expect("derive grounded recognition view")
}

fn field_fixture(width: u32) -> EvidenceStore {
    let mut store = fixture("100");
    store.backends.push(BackendIdentity {
        kind: BackendKind::NativeParser,
        name: "form".into(),
        version: "1".into(),
        profile: "native-fields".into(),
        model: None,
    });
    let pixels = [0, 0, width, 10];
    let bounds = store.rendered[0]
        .pixel_bounds_in_page(pixels)
        .expect("widget bounds");
    store.rendered.push(RenderedEvidence {
        id: 9,
        page: PageId(0),
        backend: 0,
        polygon: vec![
            Vec2 {
                x: bounds.min.x,
                y: bounds.max.y,
            },
            bounds.max,
            Vec2 {
                x: bounds.max.x,
                y: bounds.min.y,
            },
            bounds.min,
        ],
        raster: Raster {
            width,
            height: 10,
            rgb: vec![255; width as usize * 10 * 3],
        },
        composited_page: false,
    });
    store.structured.push(StructuredEvidence {
        id: 10,
        page: None,
        bounds: None,
        object: None,
        backend: 2,
        value: StructuredValue::FormField {
            name: "fee".into(),
            field_type: Some(b"Tx".to_vec()),
            value: FieldValue::Text("200".into()),
            button_states: Vec::new(),
            widgets: vec![FormWidget {
                object: None,
                page: Some(PageId(0)),
                bounds: Some(bounds),
                normal_appearance: Some(pdfdelta_core::pdf::ObjectRef {
                    object_number: 2,
                    generation: 0,
                }),
                crop: Some(WidgetCrop {
                    page_region: 7,
                    region: 9,
                    pixel_bounds: pixels,
                }),
                unresolved: None,
            }],
        },
    });
    store
}

#[test]
fn form_readings_preserve_literal_sources_and_reject_ambiguous_associations() {
    let store = field_fixture(10);
    let original = store.clone();
    let result =
        assess_form_appearances(&store, FormReadingLimits::default()).expect("form reading");
    assert_eq!(store, original);
    assert_eq!(
        result.observations[0].status,
        FormReadingStatus::DifferentLiteralReading
    );
    assert_eq!(
        result.observations[0].interpretation,
        InterpretationStatus::Inferred
    );
    assert_eq!(result.observations[0].readings, vec![8]);
    assert!(
        result.observations[0]
            .sources
            .contains(&SourceRef::Structured { element: 10 })
    );
    assert!(
        result.observations[0]
            .sources
            .contains(&SourceRef::Rendered { region: 7 })
    );

    let boundary = assess_form_appearances(&field_fixture(8), FormReadingLimits::default())
        .expect("crossing reading");
    assert_eq!(
        boundary.observations[0].status,
        FormReadingStatus::Unresolved
    );
    for mutation in 0..4 {
        let mut variant = store.clone();
        match mutation {
            0 => {
                let mut competing = variant.structured[0].clone();
                competing.id = 11;
                variant.structured.push(competing);
            }
            1 => {
                let mut other_crop = variant.rendered[1].clone();
                other_crop.id = 12;
                variant.rendered.push(other_crop);
                let mut other_field = variant.structured[1].clone();
                other_field.id = 13;
                if let StructuredValue::FormField { widgets, .. } = &mut other_field.value {
                    widgets[0].crop.as_mut().expect("crop").region = 12;
                }
                variant.structured.push(other_field);
            }
            2 => {
                variant.structured.remove(0);
                variant.inventories.clear();
            }
            _ => {
                if let StructuredValue::FormField { field_type, .. } =
                    &mut variant.structured[1].value
                {
                    *field_type = Some(b"Ch".to_vec());
                }
            }
        }
        let result = assess_form_appearances(&variant, FormReadingLimits::default())
            .expect("retained uncertainty");
        assert!(
            result
                .observations
                .iter()
                .all(|observation| observation.status == FormReadingStatus::Unresolved),
            "mutation {mutation}: {result:?}"
        );
    }
}

#[test]
fn form_reading_budget_failure_stays_on_its_source_raster() {
    let mut store = field_fixture(10);
    let mut other = field_fixture(10);
    other.pages[0].page = PageId(1);
    for region in &mut other.rendered {
        region.id += 100;
        region.page = PageId(1);
    }
    for element in &mut other.structured {
        element.id += 100;
        if element.page.is_some() {
            element.page = Some(PageId(1));
        }
        match &mut element.value {
            StructuredValue::RecognizedText { region, .. } => *region += 100,
            StructuredValue::FormField { widgets, .. } => {
                for widget in widgets {
                    widget.page = Some(PageId(1));
                    let crop = widget.crop.as_mut().expect("crop");
                    crop.page_region += 100;
                    crop.region += 100;
                }
            }
            _ => unreachable!(),
        }
    }
    store.pages.extend(other.pages);
    store.rendered.extend(other.rendered);
    store.structured.extend(other.structured);
    let result = assess_form_appearances(
        &store,
        FormReadingLimits {
            max_checks: 1,
            ..FormReadingLimits::default()
        },
    )
    .expect("bounded reading search");
    assert!(!result.exhaustive);
    assert_eq!(result.unexamined_regions, vec![107]);
    assert_eq!(
        result.observations[0].status,
        FormReadingStatus::DifferentLiteralReading
    );
    assert_eq!(result.observations[1].status, FormReadingStatus::Unresolved);
    let result = assess_form_appearances(
        &store,
        FormReadingLimits {
            max_observations: 0,
            ..FormReadingLimits::default()
        },
    )
    .expect("bounded output");
    assert!(!result.exhaustive);
    assert!(result.observations.is_empty());
}

#[test]
fn recognition_retains_raster_grounding_and_enters_the_common_solver_as_inferred() {
    let old = fixture("Revenue 100\n");
    let new = fixture("Revenue 200\n");
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
    .expect("compare recognition candidates");
    let changes: Vec<_> = result
        .scopes
        .iter()
        .flat_map(|scope| &scope.result.comparisons)
        .filter(|pair| matches!(pair.operation, Some(TypedOperation::TextChanged { .. })))
        .collect();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].interpretation, InterpretationStatus::Inferred);
    let node = old_graph
        .nodes
        .iter()
        .find(|node| matches!(node.content, NodeContent::Text { .. }))
        .expect("recognized text view");
    assert_eq!(node.basis, ViewBasis::Recognition { backend: 1 });
    assert_eq!(node.sources, vec![SourceRef::Structured { element: 8 }]);
    assert!(old.native.items().is_empty());
    assert!(!old.inventories[0].complete);
    let encoded = serde_json::to_vec(&old).expect("serialize recognition evidence");
    let decoded: EvidenceStore =
        serde_json::from_slice(&encoded).expect("deserialize recognition evidence");
    assert_eq!(decoded, old);
    let mut forged = old_graph.clone();
    forged
        .nodes
        .iter_mut()
        .find(|node| matches!(node.basis, ViewBasis::Recognition { .. }))
        .expect("recognized node")
        .basis = ViewBasis::SourceStructure;
    assert!(
        forged
            .validate(&old, EvidenceLimits::default(), GraphLimits::default())
            .is_err()
    );
}

#[test]
fn recognition_rejects_missing_rasters_forged_frames_and_invalid_words() {
    let original = fixture("100");
    let mut unknown_confidence = original.clone();
    if let StructuredValue::RecognizedText { words, .. } =
        &mut unknown_confidence.structured[0].value
    {
        words[0].confidence = None;
    }
    assert!(
        unknown_confidence
            .validate(EvidenceLimits::default())
            .is_ok()
    );
    assert_eq!(
        original.structured[0].bounds,
        Some(Rect {
            min: Vec2 { x: 0.0, y: 10.0 },
            max: Vec2 { x: 10.0, y: 20.0 }
        })
    );
    for mutation in 0..7 {
        let mut store = original.clone();
        match mutation {
            0 => store.rendered.clear(),
            1 => store.structured[0].backend = 0,
            2 => store.structured[0].bounds = None,
            3 => store.rendered[0].polygon.swap(0, 1),
            4 => store.structured[0].page = None,
            5 | 6 => {
                if let StructuredValue::RecognizedText { words, .. } =
                    &mut store.structured[0].value
                {
                    if mutation == 5 {
                        words[0].confidence = Some(f64::NAN);
                    } else {
                        words[0].pixel_bounds = [0, 0, 11, 10];
                    }
                }
            }
            _ => unreachable!(),
        }
        assert!(
            store.validate(EvidenceLimits::default()).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn overlapping_recognition_creates_pairwise_conflicts_without_merging_native_ownership() {
    use pdfdelta_core::{
        model::{
            DecodedText, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
            GlyphProvenance, TextRenderMode,
        },
        pdf::ObjectRef,
    };
    let mut store = fixture("AA");
    store.native = Document::new(
        (0..3)
            .map(|index| {
                let x = if index < 2 {
                    f64::from(index) * 3.0 + 1.0
                } else {
                    15.0
                };
                Glyph {
                    id: GlyphId(u64::from(index)),
                    text: DecodedText::Mapped("A".into()),
                    raw_code: vec![65],
                    page: PageId(0),
                    bbox: Rect {
                        min: Vec2 { x, y: 12.0 },
                        max: Vec2 {
                            x: x + 2.0,
                            y: 18.0,
                        },
                    },
                    baseline: Vec2 { x, y: 12.0 },
                    direction: Vec2 { x: 1.0, y: 0.0 },
                    font_id: FontId(0),
                    font_size: 6.0,
                    render_order: index,
                    render_mode: TextRenderMode::Fill,
                    crop_status: GlyphCropStatus::Inside,
                    path_clip_status: GlyphPathClipStatus::Unclipped,
                    provenance: GlyphProvenance {
                        content_stream: ObjectRef {
                            object_number: 1,
                            generation: 0,
                        },
                        operator_index: index,
                    },
                }
            })
            .collect(),
    );
    let derived = graph(&store);
    assert_eq!(derived.source_conflicts.len(), 2);
    for (index, conflict) in derived.source_conflicts.iter().enumerate() {
        assert_eq!(
            conflict.sources,
            vec![
                SourceRef::Structured { element: 8 },
                SourceRef::Native {
                    glyph: GlyphId(index as u64)
                }
            ]
        );
    }
    let limits = GraphLimits {
        max_references: 1,
        ..GraphLimits::default()
    };
    assert!(
        DocumentGraph::from_evidence(
            &store,
            PipelineOptions::default(),
            EvidenceLimits::default(),
            limits
        )
        .is_err()
    );
}
