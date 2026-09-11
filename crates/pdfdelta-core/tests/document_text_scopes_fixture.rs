use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, DocumentViewComparison, EdgeKind,
        EvidenceFailure, EvidenceIssue, EvidenceStore, GraphEdge, GraphNode, HierarchyLimits,
        NodeContent, NodeId, NodeKind, PageEvidence, SourceRef, TextNormalization, TextView,
        ViewBasis, compare_document_views,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::ComparableToken,
    pdf::ObjectRef,
};

type Fixture = (EvidenceStore, DocumentGraph);

fn fixture(interior: &str) -> Fixture {
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
    for (row, text) in ["First boundary.", interior, "Last boundary."]
        .into_iter()
        .enumerate()
    {
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

fn append_unassigned(fixture: &mut Fixture, page: PageId, y: f64) {
    let mut glyphs = fixture.0.native.items().to_vec();
    let mut glyph = glyphs[0].clone();
    glyph.id = GlyphId(1000);
    glyph.page = page;
    glyph.baseline.y = y;
    glyph.bbox.min.y = y;
    glyph.bbox.max.y = y + 10.0;
    let source = SourceRef::Native { glyph: glyph.id };
    if let Some(inventory) = fixture
        .0
        .inventories
        .iter_mut()
        .find(|item| item.page == Some(page))
    {
        inventory.sources.push(source);
    } else {
        fixture.0.inventories.push(ChannelInventory {
            page: Some(page),
            channel: Channel::Text,
            backend: 0,
            sources: vec![source],
            complete: true,
        });
    }
    glyphs.push(glyph);
    fixture.0.native = Document::new(glyphs);
    if !fixture.0.pages.iter().any(|item| item.page == page) {
        fixture.0.pages.push(PageEvidence { page, bounds: None });
    }
}

#[test]
fn native_interval_survives_unrelated_pages_storage_order_and_reversal() {
    let mut old = fixture("a");
    let mut new = fixture("aa");
    assert!(!old.1.relations_complete);
    let baseline = compare(&old, &new);
    let reviews = &baseline.scopes[0].result.text_scope_reviews;
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].convention, "closed-native-baseline-interval-v1");
    assert_eq!(reviews[0].old_sources.len(), 1);
    assert_eq!(reviews[0].new_sources.len(), 2);
    assert_eq!(
        compare(&new, &old).scopes[0]
            .result
            .text_scope_reviews
            .len(),
        1
    );
    for fixture in [&mut old, &mut new] {
        append_unassigned(fixture, PageId(1), 70.0);
        let mut glyphs = fixture.0.native.items().to_vec();
        glyphs.reverse();
        fixture.0.native = Document::new(glyphs);
        fixture.1.nodes.reverse();
        fixture.1.edges.reverse();
    }
    let reordered = compare(&old, &new);
    let scope = &reordered.scopes[0].result;
    let mut actual = scope.text_scope_reviews.clone();
    assert_eq!(actual.len(), 1);
    for (before, after) in reviews[0].boundaries.iter().zip(actual[0].boundaries) {
        assert_eq!(
            baseline.scopes[0].result.candidates.proposals[*before],
            scope.candidates.proposals[after]
        );
    }
    // Proposal indexes address their report-local array, not persistent identity.
    actual[0].boundaries = reviews[0].boundaries;
    assert_eq!(&actual, reviews);
}

#[test]
fn native_interval_rejects_omissions_unsafe_visibility_and_order_competitors() {
    for mutation in 0..8 {
        let old = fixture("a");
        let mut new = fixture("aa");
        match mutation {
            0 => append_unassigned(&mut new, PageId(0), 80.0),
            1 => new.0.issues.push(EvidenceIssue {
                page: Some(PageId(0)),
                channel: Channel::Text,
                sources: vec![],
                boundary: None,
                kind: EvidenceFailure::Unresolved,
                reason: "unlocated extraction gap".into(),
            }),
            2 => new.1.edges.push(GraphEdge {
                from: NodeId(1),
                to: NodeId(3),
                kind: EdgeKind::Precedes,
                sources: vec![],
                basis: ViewBasis::NativeLayout,
            }),
            3 => {
                new.1
                    .edges
                    .iter_mut()
                    .find(|edge| edge.kind == EdgeKind::Precedes)
                    .expect("fixture precedence edge")
                    .basis = ViewBasis::ReconstructedStructure
            }
            7 => {
                new.1.relations_complete = true;
                append_unassigned(&mut new, PageId(0), 80.0);
            }
            _ => {
                let mut glyphs = new.0.native.items().to_vec();
                let glyph = &mut glyphs["First boundary.".len()];
                match mutation {
                    4 => glyph.direction = Vec2 { x: 0.0, y: 1.0 },
                    5 => glyph.crop_status = GlyphCropStatus::PartiallyOutside,
                    6 => glyph.render_mode = TextRenderMode::Invisible,
                    _ => unreachable!(),
                }
                new.0.native = Document::new(glyphs);
            }
        }
        assert!(
            compare(&old, &new).scopes[0]
                .result
                .text_scope_reviews
                .is_empty(),
            "mutation {mutation}"
        );
    }
}
