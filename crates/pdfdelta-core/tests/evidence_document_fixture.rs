use std::sync::Arc;

use lopdf::{Document as Pdf, Object, Stream, dictionary};
use pdfdelta_core::{
    Error,
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, ComparisonContract,
        EvidenceLimits, EvidenceStore, PageEvidence, Raster, RenderedEvidence, SourceRef,
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
