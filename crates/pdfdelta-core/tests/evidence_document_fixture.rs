use std::sync::Arc;

use lopdf::{Document as Pdf, Object, Stream, dictionary};
use pdfdelta_core::{
    Error,
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, ComparisonContract,
        EvidenceBoundary, EvidenceLimits, EvidenceStore, PageEvidence, Raster, RenderedEvidence,
        SourceRef,
    },
    model::{Document, GlyphId, PageId, Rect, Vec2},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    source::{
        ContentStreamGlyphExtractor, ExtractionIssue, ExtractionIssueKind, ExtractionLimits,
        ExtractionOutcome, ExtractionScope, GlyphExtractor,
    },
};

fn backend(kind: BackendKind) -> BackendIdentity {
    BackendIdentity {
        kind,
        name: "fixture-provider".into(),
        version: "1".into(),
        profile: "fixture-profile-v1".into(),
        model: None,
    }
}

fn page(index: u32) -> PageEvidence {
    PageEvidence {
        page: PageId(index),
        bounds: Some(Rect {
            min: Vec2 { x: 0.0, y: 0.0 },
            max: Vec2 { x: 100.0, y: 100.0 },
        }),
    }
}

fn image_pdf(pixel: [u8; 3], content: &[u8], nested: bool) -> Vec<u8> {
    let mut pdf = Pdf::with_version("1.5");
    let pages = pdf.new_object_id();
    let image = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB",
        },
        pixel.to_vec(),
    ));
    let font = pdf.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let mut resources = dictionary! {
        "XObject" => dictionary! { "I" => image },
        "Font" => dictionary! { "F1" => font },
    };
    let contents = if nested {
        let form = pdf.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), Object::from(100)],
                "Resources" => resources,
            },
            content.to_vec(),
        ));
        resources = dictionary! { "XObject" => dictionary! { "F" => form } };
        pdf.add_object(Stream::new(dictionary! {}, b"/F Do".to_vec()))
    } else {
        pdf.add_object(Stream::new(dictionary! {}, content.to_vec()))
    };
    let page = pdf.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(100), Object::from(100)],
        "Resources" => resources,
        "Contents" => contents,
    });
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
        }),
    );
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).expect("serialize fixture PDF");
    bytes
}

#[test]
fn shared_local_comparison_requires_bound_source_normalization_proof() {
    use pdfdelta_core::document::{
        DocumentGraph, GraphLimits, NodeContent, NodeId, TextNormalization, compare_local_views,
        compare_text_group_views,
    };
    let make = |content: &[u8], work| {
        let pdf = LopdfParser
            .parse(
                Arc::from(image_pdf([0, 0, 0], content, false)),
                ParseLimits::default(),
            )
            .expect("PDF");
        let extraction = ContentStreamGlyphExtractor
            .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
            .expect("glyphs");
        let store = EvidenceStore::from_native(
            "fixture".into(),
            backend(BackendKind::NativeParser),
            vec![page(0)],
            extraction,
            EvidenceLimits::default(),
        )
        .expect("store");
        let graph = DocumentGraph::from_evidence(
            &store,
            Default::default(),
            Default::default(),
            GraphLimits {
                max_normalization_work: work,
                ..Default::default()
            },
        )
        .expect("graph");
        let node = graph
            .nodes
            .into_iter()
            .find(|node| matches!(node.content, NodeContent::Text { .. }))
            .expect("text node");
        (store, node)
    };
    let wrapped = b"BT /F1 8 Tf 10 70 Td (inter-) Tj 0 -10 Td (national cat) Tj ET";
    let (old_store, old) = make(wrapped, 1_000_000);
    let (new_store, new) = make(b"BT /F1 8 Tf 10 70 Td (international dog) Tj ET", 1_000_000);
    let NodeContent::Text { view } = &old.content else {
        unreachable!()
    };
    assert!(
        matches!(
            view.normalization,
            TextNormalization::Alternatives {
                certificate: Some(_),
                ..
            }
        ),
        "{view:?}"
    );
    let graph = DocumentGraph::from_evidence(
        &old_store,
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .expect("source-normalized graph");
    let scope = pdfdelta_core::document::compare_document_views(
        pdfdelta_core::document::DocumentView {
            evidence: &old_store,
            graph: &graph,
        },
        pdfdelta_core::document::DocumentView {
            evidence: &old_store,
            graph: &graph,
        },
        pdfdelta_core::document::CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        Default::default(),
        Default::default(),
    )
    .expect("an identical interpretation family is valid comparison input");
    assert_eq!(scope.comparisons().count(), 1);
    assert!(scope.comparisons().all(|local| local.operation.is_none()));
    let comparison = compare_local_views(&old, &new, &old_store, &new_store, Default::default())
        .expect("comparison");
    assert!(comparison.compared);
    let mask = comparison.text_mask.expect("source-backed mask");
    assert!(
        !mask.old.iter().any(|token| token.position == 5),
        "ambiguous hyphen cannot be mandatory"
    );
    assert!(mask.claims.changed_source_lower > 0);

    let (_, unchanged) = make(b"BT /F1 8 Tf 10 70 Td (international cat) Tj ET", 1_000_000);
    let ambiguous =
        compare_local_views(&old, &unchanged, &old_store, &new_store, Default::default())
            .expect("ambiguous comparison");
    assert!(!ambiguous.compared);
    assert!(ambiguous.operation.is_none());
    let claims = ambiguous.text_mask.expect("bounded family").claims;
    assert_eq!(claims.changed_source_lower, 0);
    assert!(claims.changed_source_upper > 0);

    let (_, mut suffix) = make(b"BT /F1 8 Tf 10 70 Td (suffix) Tj ET", 1_000_000);
    suffix.id = NodeId(100);
    let group = compare_text_group_views(&[&old, &suffix], &[&new, &suffix], Default::default())
        .expect("group comparison");
    assert!(group.compared);
    assert_eq!(group.text_mask.expect("group mask").old, mask.old);

    let mut changed = old.clone();
    let NodeContent::Text { view } = &mut changed.content else {
        unreachable!()
    };
    view.origins[0] = view.origins[1].clone();
    let invalid = compare_local_views(&changed, &new, &old_store, &new_store, Default::default())
        .expect("unresolved");
    assert!(!invalid.compared);
    assert!(invalid.text_mask.is_none());

    let reloaded =
        serde_json::from_slice(&serde_json::to_vec(&old).expect("serialize")).expect("reload");
    let untrusted =
        compare_local_views(&reloaded, &new, &old_store, &new_store, Default::default())
            .expect("unresolved");
    assert!(!untrusted.compared);
    assert!(untrusted.text_mask.is_none());

    let (_, limited) = make(wrapped, 0);
    let limited = compare_local_views(&limited, &new, &old_store, &new_store, Default::default())
        .expect("unresolved");
    assert!(!limited.compared);
    assert!(limited.text_mask.is_none());
}

#[test]
fn native_image_only_pdf_keeps_visual_channel_unexamined() {
    let old_bytes = image_pdf([255, 0, 0], b"/I Do", false);
    let new_bytes = image_pdf([0, 0, 255], b"/I Do", false);
    assert_ne!(old_bytes, new_bytes);
    for (revision, bytes) in [("old", old_bytes), ("new", new_bytes)] {
        let pdf = LopdfParser
            .parse(Arc::from(bytes), ParseLimits::default())
            .expect("parse image PDF");
        let extractor = ContentStreamGlyphExtractor;
        let extraction = extractor
            .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
            .expect("native extraction");
        assert!(extraction.is_complete());
        assert!(extraction.document().items().is_empty());
        let bounds = extractor
            .page_bounds(pdf.as_ref(), ExtractionLimits::default(), 1)
            .expect("page inventory");
        assert_eq!(bounds.len(), 1);
        let store = EvidenceStore::from_native(
            revision.into(),
            backend(BackendKind::NativeParser),
            vec![PageEvidence {
                page: PageId(0),
                bounds: Some(bounds[0].clone().expect("page geometry")),
            }],
            extraction,
            EvidenceLimits::default(),
        )
        .expect("native adapter");
        assert_eq!(
            store.native.last_non_text_paint(),
            &std::collections::BTreeMap::from([(PageId(0), 0)])
        );
        assert!(!store.inventory_complete(Some(PageId(0)), Channel::Text));
        assert!(!store.inventory_complete(Some(PageId(0)), Channel::Visual));
        assert!(!store.inventory_complete(Some(PageId(0)), Channel::Forms));
        assert!(
            !ComparisonContract::default()
                .channels
                .iter()
                .all(|channel| store.inventory_complete(Some(PageId(0)), *channel))
        );
    }
}

#[test]
fn page_local_native_failure_preserves_independent_page_inventory() {
    let extraction = ExtractionOutcome::new(
        Document::new(Vec::new()),
        vec![
            ExtractionIssue::new(
                ExtractionIssueKind::Unsupported,
                ExtractionScope::Page(PageId(0)),
                "unsupported page-local clipping",
            )
            .expect("issue"),
        ],
    )
    .expect("incomplete native document");
    let store = EvidenceStore::from_native(
        "revision".into(),
        backend(BackendKind::NativeParser),
        vec![page(0), page(1)],
        extraction,
        EvidenceLimits::default(),
    )
    .expect("store");
    assert!(!store.inventory_complete(Some(PageId(0)), Channel::Text));
    assert!(store.inventory_complete(Some(PageId(1)), Channel::Text));
    assert!(!store.inventory_complete(Some(PageId(1)), Channel::Visual));
}

#[test]
fn native_gap_boundaries_preserve_neighbors_without_inventing_missing_sources() {
    let pdf = LopdfParser
        .parse(
            Arc::from(image_pdf(
                [0, 0, 0],
                b"BT /F1 12 Tf 10 40 Td (AB) Tj ET",
                false,
            )),
            ParseLimits::default(),
        )
        .expect("PDF");
    let extracted = ContentStreamGlyphExtractor
        .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
        .expect("glyphs");
    for cross_page in [false, true] {
        let mut glyphs = extracted.document().items().to_vec();
        assert_eq!(glyphs.len(), 2);
        glyphs[0].id = GlyphId(71);
        glyphs[1].id = GlyphId(503);
        if cross_page {
            glyphs[1].page = PageId(1);
        }
        for retained_before in 0..=2 {
            let outcome = ExtractionOutcome::new(
                Document::new(glyphs.clone()),
                vec![
                    ExtractionIssue::new(
                        ExtractionIssueKind::Unresolved,
                        ExtractionScope::GlyphGap { retained_before },
                        "unreadable operator",
                    )
                    .expect("issue"),
                ],
            )
            .expect("outcome");
            let mut store = EvidenceStore::from_native(
                "gap".into(),
                backend(BackendKind::NativeParser),
                vec![page(0), page(1)],
                outcome,
                EvidenceLimits::default(),
            )
            .expect("store");
            assert_eq!(
                store.issues[0].boundary,
                Some(EvidenceBoundary::GlyphGap {
                    retained_before,
                    before: retained_before.checked_sub(1).map(|index| glyphs[index].id),
                    after: glyphs.get(retained_before).map(|glyph| glyph.id),
                })
            );
            assert_eq!(
                store.issues[0].page,
                (!cross_page && retained_before == 1).then_some(PageId(0))
            );
            assert!(store.issues[0].sources.is_empty());
            assert!(!store.inventory_complete(None, Channel::Text));
            if cross_page {
                assert!(!store.inventory_complete(Some(PageId(0)), Channel::Text));
                assert!(!store.inventory_complete(Some(PageId(1)), Channel::Text));
            }
            let roundtrip: EvidenceStore =
                serde_json::from_slice(&serde_json::to_vec(&store).expect("serialize"))
                    .expect("deserialize");
            roundtrip
                .validate(EvidenceLimits::default())
                .expect("valid boundary");
            let Some(EvidenceBoundary::GlyphGap { before, .. }) = &mut store.issues[0].boundary
            else {
                panic!("boundary")
            };
            *before = Some(GlyphId(999));
            assert!(store.validate(EvidenceLimits::default()).is_err());
        }
    }
}

#[test]
fn page_and_empty_glyph_gaps_keep_document_edge_evidence() {
    for retained_before in 0..=2 {
        let outcome = ExtractionOutcome::new(
            Document::new(Vec::new()),
            vec![
                ExtractionIssue::new(
                    ExtractionIssueKind::Unresolved,
                    ExtractionScope::PageGap { retained_before },
                    "unreadable page branch",
                )
                .expect("issue"),
            ],
        )
        .expect("outcome");
        let store = EvidenceStore::from_native(
            "page gap".into(),
            backend(BackendKind::NativeParser),
            vec![page(0), page(1)],
            outcome,
            EvidenceLimits::default(),
        )
        .expect("store");
        assert_eq!(
            store.issues[0].boundary,
            Some(EvidenceBoundary::PageGap {
                retained_before,
                before: retained_before
                    .checked_sub(1)
                    .map(|index| PageId(index as u32)),
                after: (retained_before < 2).then_some(PageId(retained_before as u32)),
            })
        );
        assert_eq!(store.issues[0].page, None);
        assert!(!store.inventory_complete(Some(PageId(1)), Channel::Text));
    }
    let outcome = ExtractionOutcome::new(
        Document::new(Vec::new()),
        vec![
            ExtractionIssue::new(
                ExtractionIssueKind::Unresolved,
                ExtractionScope::GlyphGap { retained_before: 0 },
                "no retained glyphs",
            )
            .expect("issue"),
        ],
    )
    .expect("outcome");
    let store = EvidenceStore::from_native(
        "empty gap".into(),
        backend(BackendKind::NativeParser),
        Vec::new(),
        outcome,
        EvidenceLimits::default(),
    )
    .expect("store");
    assert_eq!(
        store.issues[0].boundary,
        Some(EvidenceBoundary::GlyphGap {
            retained_before: 0,
            before: None,
            after: None
        })
    );
    let bad_page_gap = ExtractionOutcome::new(
        Document::new(Vec::new()),
        vec![
            ExtractionIssue::new(
                ExtractionIssueKind::Unresolved,
                ExtractionScope::PageGap { retained_before: 1 },
                "missing page inventory",
            )
            .expect("issue"),
        ],
    )
    .expect("outcome");
    assert!(
        EvidenceStore::from_native(
            "bad gap".into(),
            backend(BackendKind::NativeParser),
            Vec::new(),
            bad_page_gap,
            EvidenceLimits::default()
        )
        .is_err()
    );
}

#[test]
fn shared_comparison_blocks_gap_crossing_text_and_keeps_independent_fields() {
    use pdfdelta_core::document::{
        CorrespondenceScope, DocumentComparisonLimits, DocumentGraph, DocumentView, FieldValue,
        NodeId, StructuredEvidence, StructuredValue, TypedOperation, compare_document_views,
        compare_scope_views,
    };
    let pdf = LopdfParser
        .parse(
            Arc::from(image_pdf(
                [0, 0, 0],
                b"BT /F1 12 Tf 10 40 Td (AB) Tj ET",
                false,
            )),
            ParseLimits::default(),
        )
        .expect("PDF");
    let make = |revision: &str, field: &str, gap: bool| {
        let extraction = ContentStreamGlyphExtractor
            .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
            .expect("glyphs");
        let extraction = if gap {
            ExtractionOutcome::new(
                extraction.document().clone(),
                vec![
                    ExtractionIssue::new(
                        ExtractionIssueKind::Unresolved,
                        ExtractionScope::GlyphGap { retained_before: 1 },
                        "unreadable form",
                    )
                    .expect("issue"),
                ],
            )
            .expect("partial extraction")
        } else {
            extraction
        };
        let mut store = EvidenceStore::from_native(
            revision.into(),
            backend(BackendKind::NativeParser),
            vec![page(0)],
            extraction,
            EvidenceLimits::default(),
        )
        .expect("store");
        store.structured.push(StructuredEvidence {
            id: 0,
            page: None,
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::FormField {
                name: "independent".into(),
                field_type: Some(b"Tx".to_vec()),
                value: FieldValue::Text(field.into()),
                widgets: Vec::new(),
                button_states: Vec::new(),
            },
        });
        let graph = DocumentGraph::from_evidence(
            &store,
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .expect("graph");
        (store, graph)
    };
    let new = make("new", "200", false);
    for gap in [false, true] {
        let old = make("old", "100", gap);
        let result = compare_scope_views(
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
        )
        .expect("comparison");
        let document = compare_document_views(
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
            Default::default(),
        )
        .expect("document comparison");
        assert_eq!(
            document.scopes[0].result.extraction_dependencies,
            result.extraction_dependencies
        );
        assert!(
            result
                .comparisons
                .iter()
                .any(|comparison| comparison.compared
                    && matches!(
                        comparison.operation,
                        Some(TypedOperation::ValueChanged { .. })
                    ))
        );
        if gap {
            assert_eq!(result.extraction_dependencies.len(), 1);
            let dependency = &result.extraction_dependencies[0];
            assert_eq!(dependency.old_issues, [0]);
            assert!(dependency.new_issues.is_empty());
            assert!(!dependency.work_limited);
            let blocked = &result.comparisons[dependency.comparison];
            assert!(!blocked.compared);
            assert!(blocked.operation.is_none());
            assert!(blocked.text_mask.is_none());
        } else {
            assert!(result.extraction_dependencies.is_empty());
            assert!(
                result
                    .comparisons
                    .iter()
                    .all(|comparison| comparison.compared)
            );
        }
    }
}

#[test]
fn rendered_evidence_requires_valid_origin_dimensions_and_bounded_payloads() {
    let mut store = EvidenceStore::from_native(
        "revision".into(),
        backend(BackendKind::NativeParser),
        vec![page(0)],
        ExtractionOutcome::complete(Document::new(Vec::new())),
        EvidenceLimits::default(),
    )
    .expect("store");
    store.backends.push(backend(BackendKind::Renderer));
    store.rendered.push(RenderedEvidence {
        id: 7,
        page: PageId(0),
        backend: 1,
        composited_page: true,
        polygon: vec![
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 100.0, y: 0.0 },
            Vec2 { x: 100.0, y: 100.0 },
            Vec2 { x: 0.0, y: 100.0 },
        ],
        raster: Raster {
            width: 1,
            height: 1,
            rgb: vec![255, 0, 0],
        },
    });
    store.inventories.push(ChannelInventory {
        page: Some(PageId(0)),
        channel: Channel::Visual,
        backend: 1,
        sources: vec![SourceRef::Rendered { region: 7 }],
        complete: true,
    });
    store
        .validate(EvidenceLimits::default())
        .expect("valid raster source");
    assert!(store.inventory_complete(Some(PageId(0)), Channel::Visual));
    assert!(matches!(
        store.validate(EvidenceLimits {
            max_raster_bytes: 2,
            ..EvidenceLimits::default()
        }),
        Err(Error::LimitExceeded {
            resource: "evidence raster bytes",
            ..
        })
    ));
    store.rendered[0].raster.rgb.pop();
    assert!(store.validate(EvidenceLimits::default()).is_err());
    store.rendered[0].raster.rgb.push(0);
    store.inventories[1].sources[0] = SourceRef::Native { glyph: GlyphId(7) };
    assert!(
        store.validate(EvidenceLimits::default()).is_err(),
        "a rendered ID cannot impersonate a glyph"
    );
    store.inventories[1].sources[0] = SourceRef::Rendered { region: 7 };
    store.backends[1].kind = BackendKind::Ocr;
    assert!(
        store.validate(EvidenceLimits::default()).is_err(),
        "recognition is not rendering provenance"
    );
}

#[test]
fn stored_field_without_a_widget_retains_document_scoped_evidence() {
    use pdfdelta_core::document::{FieldValue, StructuredEvidence, StructuredValue};

    let mut store = EvidenceStore::from_native(
        "revision".into(),
        backend(BackendKind::NativeParser),
        vec![page(0)],
        ExtractionOutcome::complete(Document::new(Vec::new())),
        EvidenceLimits::default(),
    )
    .expect("native store");
    store.structured.push(StructuredEvidence {
        id: 1,
        page: None,
        bounds: None,
        object: None,
        backend: 0,
        value: StructuredValue::FormField {
            field_type: None,
            name: "Invoice.Total".into(),
            value: FieldValue::Text("100".into()),
            widgets: Vec::new(),
            button_states: Vec::new(),
        },
    });
    store.inventories.push(ChannelInventory {
        page: None,
        channel: Channel::Forms,
        backend: 0,
        sources: vec![SourceRef::Structured { element: 1 }],
        complete: true,
    });
    store
        .validate(EvidenceLimits::default())
        .expect("field remains present without a page");
    assert!(store.inventory_complete(None, Channel::Forms));
    assert!(store.inventory_complete(Some(PageId(0)), Channel::Forms));
    assert!(!store.inventory_complete(None, Channel::Visual));
}

#[test]
fn paint_acquisition_tracks_drawing_instead_of_unused_resources_or_paths() {
    for (content, nested, expected) in [
        (b"/I Do".as_slice(), true, true),
        (
            b"BI /W 1 /H 1 /BPC 8 /CS /RGB ID abc EI".as_slice(),
            false,
            true,
        ),
        (b"".as_slice(), false, false),
        (b"10 10 m 20 20 l S".as_slice(), false, true),
        (b"10 10 20 20 re f".as_slice(), false, true),
        (b"10 10 m 20 10 20 20 10 20 c f".as_slice(), false, true),
        (b"10 10 20 20 re W n".as_slice(), false, false),
        (b"10 10 m 20 20 l n".as_slice(), false, false),
        (b"S".as_slice(), false, false),
        (b"/Shade sh".as_slice(), false, true),
    ] {
        let pdf = LopdfParser
            .parse(
                Arc::from(image_pdf([255, 0, 0], content, nested)),
                ParseLimits::default(),
            )
            .expect("parse PDF");
        let extraction = ContentStreamGlyphExtractor
            .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
            .expect("extract PDF");
        assert!(extraction.is_complete());
        assert_eq!(
            extraction
                .document()
                .last_non_text_paint()
                .contains_key(&PageId(0)),
            expected
        );
        let store = EvidenceStore::from_native(
            "fixture".into(),
            backend(BackendKind::NativeParser),
            vec![page(0)],
            extraction,
            EvidenceLimits::default(),
        )
        .expect("native adapter");
        assert_eq!(
            store.inventory_complete(Some(PageId(0)), Channel::Text),
            !expected
        );
    }
}

#[test]
fn paint_boundary_preserves_glyph_order_in_pages_and_nested_forms() {
    for nested in [false, true] {
        let bytes = image_pdf(
            [255, 0, 0],
            b"BT /F1 12 Tf 10 10 Td (A) Tj ET /I Do BT /F1 12 Tf 30 30 Td (B) Tj ET",
            nested,
        );
        let pdf = LopdfParser
            .parse(Arc::from(bytes), ParseLimits::default())
            .expect("parse PDF");
        let extraction = ContentStreamGlyphExtractor
            .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
            .expect("extract PDF");
        assert!(extraction.is_complete());
        let document = extraction.document();
        assert_eq!(document.items().len(), 2);
        assert_eq!(document.items()[0].render_order, 0);
        assert_eq!(document.items()[1].render_order, 1);
        assert_eq!(document.last_non_text_paint().get(&PageId(0)), Some(&1));
    }
}

#[test]
fn acquired_paint_bounds_enclose_images_fills_caps_and_bounded_joins() {
    for nested in [false, true] {
        for (content, expected) in [
            ("20 0 0 30 10 15 cm /I Do", Some((10.0, 15.0, 30.0, 45.0))),
            ("10 15 20 30 re f", Some((10.0, 15.0, 30.0, 45.0))),
            (
                "3 0 0 2 0 0 cm 2 w 2 J 10 10 m 20 10 l S",
                Some((27.0, 18.0, 63.0, 22.0)),
            ),
            ("0 w 10 10 m 20 20 l S", None),
            ("10 10 20 20 re S", Some((0.0, 0.0, 40.0, 40.0))),
            (
                "10 10 m 20 10 20 20 10 20 c f",
                Some(if nested {
                    (0.0, 0.0, 100.0, 100.0)
                } else {
                    (10.0, 10.0, 20.0, 20.0)
                }),
            ),
            ("/Shade sh", None),
        ] {
            // Unknown primitive extent stays unknown on a page; a Form supplies
            // its independent clipping bound without resolving the primitive.
            let expected = expected.or_else(|| nested.then_some((0.0, 0.0, 100.0, 100.0)));
            let pdf = LopdfParser
                .parse(
                    Arc::from(image_pdf([0; 3], content.as_bytes(), nested)),
                    ParseLimits::default(),
                )
                .expect("valid paint fixture");
            let extraction = ContentStreamGlyphExtractor
                .extract_outcome(pdf.as_ref(), ExtractionLimits::default())
                .expect("valid paint fixture");
            assert!(extraction.is_complete());
            let paints = extraction
                .document()
                .non_text_paint_bounds()
                .expect("valid paint fixture");
            assert_eq!(paints.len(), 1);
            assert_ne!(paints[0].content_stream.object_number, 0);
            if let Some((min_x, min_y, max_x, max_y)) = expected {
                let bounds = paints[0].bounds.expect(content);
                assert!(
                    bounds.min.x <= min_x
                        && bounds.min.y <= min_y
                        && bounds.max.x >= max_x
                        && bounds.max.y >= max_y,
                    "{content}: {bounds:?}"
                );
                assert!(bounds.min.x > min_x - 10.0 && bounds.max.x < max_x + 10.0);
            } else {
                assert_eq!(paints[0].bounds, None, "{content}");
            }
        }
    }
}
