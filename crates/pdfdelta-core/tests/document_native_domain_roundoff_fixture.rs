use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, DocumentViewComparison, EdgeKind,
        EvidenceStore, GraphEdge, GraphNode, HierarchyLimits, NodeContent, NodeId, NodeKind,
        PageEvidence, SourceRef, TextNormalization, TextView, ViewBasis, compare_document_views,
        document_coverage,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, NonTextPaint, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::ComparableToken,
    pdf::ObjectRef,
};

type Fixture = (EvidenceStore, DocumentGraph);

fn fixture_rows(rows: &[&str]) -> Fixture {
    let mut graph = DocumentGraph::default();
    graph.nodes.push(GraphNode {
        id: NodeId(0),
        kind: NodeKind::Document,
        pages: vec![],
        sources: vec![],
        identity: None,
        basis: ViewBasis::SourceStructure,
        content: NodeContent::Container,
    });
    let mut glyphs = Vec::new();
    for (row, text) in rows.iter().enumerate() {
        let mut sources = Vec::new();
        for (column, scalar) in text.chars().enumerate() {
            let id = glyphs.len() as u64;
            let baseline = Vec2 {
                x: column as f64 * 10.0,
                y: 100.0 - row as f64 * 30.0,
            };
            sources.push(SourceRef::Native { glyph: GlyphId(id) });
            glyphs.push(Glyph {
                id: GlyphId(id),
                text: DecodedText::Mapped(scalar.to_string()),
                raw_code: scalar.to_string().into_bytes(),
                page: PageId(0),
                bbox: Rect {
                    min: baseline,
                    max: Vec2 {
                        x: baseline.x + 8.0,
                        y: baseline.y + 10.0,
                    },
                },
                baseline,
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(1),
                font_size: 10.0,
                render_order: id as u32,
                render_mode: TextRenderMode::Fill,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: 1,
                        generation: 0,
                    },
                    operator_index: id as u32,
                },
            });
        }
        let id = NodeId(row as u64 + 1);
        graph.nodes.push(GraphNode {
            id,
            kind: NodeKind::Paragraph,
            pages: vec![PageId(0)],
            identity: None,
            basis: ViewBasis::NativeLayout,
            content: NodeContent::Text {
                view: TextView {
                    tokens: text.chars().map(ComparableToken::Scalar).collect(),
                    origins: sources.iter().map(|source| vec![*source]).collect(),
                    source_backed: vec![true; sources.len()],
                    normalization: TextNormalization::Exact,
                },
            },
            sources,
        });
        graph.edges.push(GraphEdge {
            from: NodeId(0),
            to: id,
            kind: EdgeKind::Contains,
            sources: vec![],
            basis: ViewBasis::NativeLayout,
        });
        if row > 0 {
            graph.edges.push(GraphEdge {
                from: NodeId(row as u64),
                to: id,
                kind: EdgeKind::Precedes,
                sources: vec![],
                basis: ViewBasis::NativeLayout,
            });
        }
    }
    let sources = glyphs
        .iter()
        .map(|glyph| SourceRef::Native { glyph: glyph.id })
        .collect();
    (
        EvidenceStore {
            revision: "native-scope-fixture".into(),
            backends: vec![BackendIdentity {
                kind: BackendKind::NativeParser,
                name: "fixture".into(),
                version: "1".into(),
                profile: "raw-horizontal-glyphs".into(),
                model: None,
            }],
            pages: vec![PageEvidence {
                page: PageId(0),
                bounds: None,
            }],
            native: Document::new(glyphs),
            rendered: vec![],
            structured: vec![],
            key_inventories: vec![],
            native_structures: vec![],
            issues: vec![],
            inventories: vec![ChannelInventory {
                page: Some(PageId(0)),
                channel: Channel::Text,
                backend: 0,
                sources,
                complete: true,
            }],
        },
        graph,
    )
}

fn compare(old: &Fixture, new: &Fixture) -> DocumentViewComparison {
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
        HierarchyLimits::default(),
    )
    .expect("compare source-bound native views")
}

fn with_paint(fixture: &mut Fixture, bounds: Option<Rect>) {
    fixture.0.native = fixture
        .0
        .native
        .clone()
        .with_non_text_paint_bounds(vec![NonTextPaint {
            page: PageId(0),
            render_order: 0,
            bounds,
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: 0,
        }]);
    fixture.0.inventories[0].complete = false;
}

fn document_view(fixture: &Fixture) -> DocumentView<'_> {
    DocumentView {
        evidence: &fixture.0,
        graph: &fixture.1,
    }
}

fn rounded_pair(position: usize) -> (Fixture, Fixture) {
    let old = fixture_rows(&["First boundary.", "a", "Last boundary."]);
    let mut new = fixture_rows(&["First boundary. ", "aa", "Last boundary. "]);
    new.0.native = new.0.native.map_items(|mut glyph| {
        if glyph.id == GlyphId(position as u64) {
            glyph.baseline.y = glyph.baseline.y.next_up().next_up().next_up();
        }
        glyph
    });
    (old, new)
}

#[test]
fn rounded_domains_preserve_exact_body_coverage_in_both_directions() {
    for position in [1, 5, 15] {
        let (old, new) = rounded_pair(position);
        for (a, b, rounded_side) in [(&old, &new, "new"), (&new, &old, "old")] {
            let result = compare(a, b);
            let domains = &result.scopes[0].result.native_text_domains;
            assert_eq!(domains.len(), 2);
            let serialized = serde_json::to_value(domains).expect("serialize observations");
            let key = format!("{rounded_side}_row_order");
            let painted = serialized
                .as_array()
                .expect("domain array")
                .iter()
                .filter(|domain| domain[key.as_str()] == "horizontal-paint-row-boundaries-v1")
                .count();
            assert_eq!(painted, 1);
            let coverage = document_coverage(
                document_view(a),
                document_view(b),
                &result,
                &[Channel::Text].into(),
            );
            let expected = "First boundary.Last boundary.".chars().count();
            assert_eq!(coverage[0].old_compared_sources, expected);
            assert_eq!(coverage[0].new_compared_sources, expected);
            assert!(
                !coverage[0].complete,
                "padding and changed body stay unresolved"
            );
            assert!(!result.search_resolved());
        }
    }
}

#[test]
fn roundoff_fallback_preserves_order_visibility_and_acquisition_obligations() {
    for case in [
        "material_offset",
        "reverse_paint",
        "changed_reading",
        "clipped",
        "nearby_omission",
        "overlapping_paint",
        "unknown_paint",
    ] {
        let (old, mut new) = rounded_pair(5);
        let mut glyphs = new.0.native.items().to_vec();
        match case {
            "material_offset" => glyphs[5].baseline.y = 100.01,
            "reverse_paint" => {
                let previous = glyphs[4].render_order;
                glyphs[4].render_order = glyphs[5].render_order;
                glyphs[5].render_order = previous;
            }
            "changed_reading" => glyphs[5].text = DecodedText::Mapped("X".into()),
            "clipped" => glyphs[5].path_clip_status = GlyphPathClipStatus::PartiallyOutside,
            "nearby_omission" => {
                // Outside the original exact band but inside the row envelope.
                // A projection-only fallback would silently omit this glyph.
                let mut missing = glyphs[0].clone();
                missing.id = GlyphId(1000);
                missing.render_order = 1000;
                missing.baseline.y = glyphs[5].baseline.y.next_up();
                new.0.inventories[0]
                    .sources
                    .push(SourceRef::Native { glyph: missing.id });
                glyphs.push(missing);
            }
            "overlapping_paint" | "unknown_paint" => {}
            _ => unreachable!(),
        }
        new.0.native = Document::new(glyphs);
        if case == "overlapping_paint" {
            with_paint(
                &mut new,
                Some(Rect {
                    min: Vec2 { x: 0.0, y: 100.0 },
                    max: Vec2 { x: 200.0, y: 110.0 },
                }),
            );
        } else if case == "unknown_paint" {
            with_paint(&mut new, None);
        }
        for (a, b) in [(&old, &new), (&new, &old)] {
            let result = compare(a, b);
            let expected = usize::from(case != "unknown_paint");
            assert_eq!(
                result.scopes[0].result.native_text_domains.len(),
                expected,
                "{case}"
            );
            assert!(!result.search_resolved(), "{case}");
        }
    }
}

#[test]
fn rounded_domain_audit_cannot_restore_proof_authority() {
    let (old, new) = rounded_pair(5);
    let result = compare(&old, &new);
    assert_eq!(result.scopes[0].result.native_text_domains.len(), 2);
    let reloaded: DocumentViewComparison = serde_json::from_value(
        serde_json::to_value(&result).expect("serialize domain observations"),
    )
    .expect("reload observations without proof authority");
    let coverage = document_coverage(
        document_view(&old),
        document_view(&new),
        &reloaded,
        &[Channel::Text].into(),
    );
    assert_eq!(coverage[0].old_compared_sources, 0);
    assert_eq!(coverage[0].new_compared_sources, 0);
    assert!(!coverage[0].complete);
}

#[test]
fn ordinary_domains_keep_their_unmodified_observation_shape() {
    let old = fixture_rows(&["First boundary.", "a", "Last boundary."]);
    let new = fixture_rows(&["First boundary. ", "aa", "Last boundary. "]);
    let result = compare(&old, &new);
    assert_eq!(result.scopes[0].result.native_text_domains.len(), 2);
    let domains = serde_json::to_value(&result.scopes[0].result.native_text_domains)
        .expect("serialize ordinary domains");
    for domain in domains.as_array().expect("domain array") {
        assert!(domain.get("old_row_order").is_none());
        assert!(domain.get("new_row_order").is_none());
    }
}

#[test]
fn zero_proof_budget_cannot_authorize_rounded_domains() {
    let (old, new) = rounded_pair(5);
    let mut limits = DocumentComparisonLimits::default();
    limits.local.proof_work = 0;
    let result = compare_document_views(
        document_view(&old),
        document_view(&new),
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("bounded comparison");
    assert!(result.scopes[0].result.native_text_domains.is_empty());
    assert!(!result.search_resolved());
}
