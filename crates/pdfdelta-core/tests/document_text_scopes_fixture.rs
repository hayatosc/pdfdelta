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
        GlyphProvenance, NonTextPaint, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::ComparableToken,
    pdf::ObjectRef,
};

type Fixture = (EvidenceStore, DocumentGraph);

fn fixture(interior: &str) -> Fixture {
    fixture_with_boundaries(interior, "First boundary.", "Last boundary.")
}

fn fixture_with_boundaries(interior: &str, first: &str, last: &str) -> Fixture {
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
    for (row, text) in [first, interior, last].into_iter().enumerate() {
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
fn native_interval_does_not_turn_an_external_continuation_into_a_content_change() {
    let old = fixture("Shared prefix. Additional sentence.");
    let short = fixture("Shared prefix.");
    assert_eq!(
        compare(&old, &short).scopes[0]
            .result
            .text_scope_reviews
            .len(),
        1
    );

    for (continuation, expected_reviews) in
        [("Additional sentence.", 0), ("Unrelated sentence.", 1)]
    {
        let mut new = short.clone();
        let source = fixture(continuation);
        let mut node = source.1.nodes[2].clone();
        let mut glyphs = new.0.native.items().to_vec();
        let mut origins = Vec::new();
        for atom in &node.sources {
            let SourceRef::Native { glyph } = atom else {
                unreachable!()
            };
            let mut glyph = source
                .0
                .native
                .items()
                .iter()
                .find(|item| item.id == *glyph)
                .expect("continuation glyph belongs to the fixture")
                .clone();
            glyph.id = GlyphId(glyphs.len() as u64);
            glyph.page = PageId(1);
            glyph.render_order = glyphs.len() as u32;
            origins.push(vec![SourceRef::Native { glyph: glyph.id }]);
            glyphs.push(glyph);
        }
        node.id = NodeId(4);
        node.pages = vec![PageId(1)];
        node.sources = origins.iter().flatten().copied().collect();
        let NodeContent::Text { view } = &mut node.content else {
            unreachable!()
        };
        view.origins = origins;
        new.0.native = Document::new(glyphs);
        new.0.pages.push(PageEvidence {
            page: PageId(1),
            bounds: None,
        });
        new.0.inventories.push(ChannelInventory {
            page: Some(PageId(1)),
            channel: Channel::Text,
            backend: 0,
            sources: node.sources.clone(),
            complete: true,
        });
        new.1.nodes.push(node);
        new.1.edges.push(GraphEdge {
            from: NodeId(0),
            to: NodeId(4),
            kind: EdgeKind::Contains,
            sources: vec![],
            basis: ViewBasis::NativeLayout,
        });
        for (left, right) in [(&old, &new), (&new, &old)] {
            let result = compare(left, right);
            assert_eq!(
                result.scopes[0].result.text_scope_reviews.len(),
                expected_reviews
            );
            let coverage = pdfdelta_core::document::document_coverage(
                DocumentView {
                    evidence: &left.0,
                    graph: &left.1,
                },
                DocumentView {
                    evidence: &right.0,
                    graph: &right.1,
                },
                &result,
                &[Channel::Text].into(),
            );
            assert!(!coverage[0].complete);
        }
    }
}

#[test]
fn native_interval_does_not_report_ascii_spacing_alone_as_content() {
    for (a, b) in [("ab", "a b"), ("ab", "ab "), ("ab", " ac")] {
        let old = fixture(a);
        let new = fixture(b);
        let comparison = compare(&old, &new);
        let scope = &comparison.scopes[0].result;
        assert_eq!(scope.text_scope_reviews.len(), usize::from(b == " ac"));
        let coverage = pdfdelta_core::document::document_coverage(
            DocumentView {
                evidence: &old.0,
                graph: &old.1,
            },
            DocumentView {
                evidence: &new.0,
                graph: &new.1,
            },
            &comparison,
            &[Channel::Text].into(),
        );
        assert!(!coverage[0].complete);
        assert_eq!(
            compare(&new, &old).scopes[0]
                .result
                .text_scope_reviews
                .len(),
            scope.text_scope_reviews.len()
        );
    }
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

#[test]
fn bounded_paint_closes_only_the_retained_local_interval() {
    let old = fixture("a");
    let mut new = fixture("aa");
    with_paint(
        &mut new,
        Some(Rect {
            min: Vec2 { x: 0.0, y: 120.0 },
            max: Vec2 { x: 200.0, y: 140.0 },
        }),
    );
    let result = compare(&old, &new);
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert_eq!(reviews.len(), 1);
    assert_eq!(
        reviews[0].convention,
        "closed-native-paint-bounds-interval-v1"
    );
    assert!(!new.0.inventory_complete(Some(PageId(0)), Channel::Text));
    assert_eq!(
        compare(&new, &old).scopes[0]
            .result
            .text_scope_reviews
            .len(),
        1
    );
}

#[test]
fn paint_closure_rejects_unknown_bounds_boundary_ink_and_incomplete_sources() {
    for mutation in 0..6 {
        let old = fixture("a");
        let mut new = fixture("aa");
        let mut bounds = Rect {
            min: Vec2 { x: 0.0, y: 120.0 },
            max: Vec2 { x: 200.0, y: 140.0 },
        };
        if mutation == 1 {
            // Above the anchor baseline, but still covering its actual ink.
            bounds.min.y = 105.0;
        }
        with_paint(&mut new, (mutation != 0).then_some(bounds));
        match mutation {
            2 => {
                new.0.native = Document::new(new.0.native.items().to_vec())
                    .with_last_non_text_paint([(PageId(0), 0)].into())
            }
            3 => {
                new.0.inventories[0].sources.pop();
            }
            4 => new.0.issues.push(EvidenceIssue {
                page: Some(PageId(0)),
                channel: Channel::Text,
                sources: vec![],
                boundary: None,
                kind: EvidenceFailure::Unresolved,
                reason: "unlocated extraction gap".into(),
            }),
            5 => {
                append_unassigned(&mut new, PageId(0), 80.0);
                with_paint(&mut new, Some(bounds));
            }
            _ => {}
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

#[test]
fn padding_boundaries_keep_whole_paragraphs_uncompared_and_close_the_interior() {
    let old = fixture("a");
    let new = fixture_with_boundaries("aa", "First boundary. ", "Last boundary. ");
    let comparison = compare(&old, &new);
    let result = &comparison.scopes[0].result;
    assert_eq!(result.text_scope_reviews.len(), 1);
    let review = &result.text_scope_reviews[0];
    assert_eq!(review.convention, "closed-native-padding-interval-v1");
    assert_eq!(review.old_sources.len(), 1);
    assert_eq!(review.new_sources.len(), 2);
    assert_eq!(result.text_boundary_correspondences.len(), 2);
    assert!(
        result
            .accepted_correspondences
            .iter()
            .all(|index| result.candidates.proposals[*index].basis
                != pdfdelta_core::document::ProposalBasis::LiteralContentWithPadding)
    );
    assert!(
        result
            .comparisons
            .iter()
            .all(|pair| pair.old != [NodeId(1)] && pair.old != [NodeId(3)])
    );
    let coverage = pdfdelta_core::document::document_coverage(
        DocumentView {
            evidence: &old.0,
            graph: &old.1,
        },
        DocumentView {
            evidence: &new.0,
            graph: &new.1,
        },
        &comparison,
        &[Channel::Text].into(),
    );
    assert_eq!(coverage[0].old_compared_sources, 0);
    assert_eq!(coverage[0].new_compared_sources, 0);
    assert!(!coverage[0].complete);
    assert!(!comparison.search_resolved());
    assert!(
        result
            .unresolved
            .iter()
            .any(|reason| reason.contains("whole paragraph sources remain uncompared"))
    );
    for sources in &review.new_boundaries {
        assert!(sources.iter().any(|source| {
            let SourceRef::Native { glyph } = source else {
                return false;
            };
            new.0
                .native
                .items()
                .iter()
                .any(|item| item.id == *glyph && item.text == DecodedText::Mapped(" ".into()))
        }));
    }
    assert_eq!(
        compare(&new, &old).scopes[0]
            .result
            .text_scope_reviews
            .len(),
        1
    );
}

#[test]
fn padding_premise_does_not_ignore_internal_spaces_or_resolve_duplicate_bodies() {
    for new in [
        fixture_with_boundaries("aa", "First  boundary. ", "Last boundary. "),
        fixture_with_boundaries("First boundary.  ", "First boundary. ", "Last boundary. "),
    ] {
        let old = fixture("a");
        assert!(
            compare(&old, &new).scopes[0]
                .result
                .text_scope_reviews
                .is_empty()
        );
    }
}

#[test]
fn whole_literal_correspondence_precedes_padding_even_at_extreme_weight() {
    use pdfdelta_core::document::{
        MatchingLimits, ProposalBasis, propose_scope_correspondences, solve_correspondence_scope,
    };
    let old = fixture("a");
    let new = fixture_with_boundaries("First boundary. ", "First boundary.", "Last boundary.");
    let scope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };
    let limits = MatchingLimits::default();
    let mut candidates = propose_scope_correspondences(&old.1, &new.1, scope, limits)
        .expect("complete candidate index");
    for proposal in &mut candidates.proposals {
        if proposal.basis == ProposalBasis::LiteralContentWithPadding {
            proposal.weight = u32::MAX;
        }
    }
    let matching = solve_correspondence_scope(&old.1, &new.1, scope, &candidates.proposals, limits)
        .expect("solve exact before padded literals");
    assert!(matching.source_only_mandatory.iter().any(|index| {
        let proposal = &candidates.proposals[*index];
        proposal.old == [NodeId(1)]
            && proposal.new == [NodeId(1)]
            && proposal.basis == ProposalBasis::LiteralContent
    }));
    assert!(!matching.source_only_mandatory.iter().any(
        |index| candidates.proposals[*index].basis == ProposalBasis::LiteralContentWithPadding
    ));
    let forged = pdfdelta_core::document::CorrespondenceProposal {
        old: vec![NodeId(2)],
        new: vec![NodeId(2)],
        basis: ProposalBasis::LiteralContentWithPadding,
        supplier: "forged-padding".into(),
        weight: 1,
    };
    assert!(solve_correspondence_scope(&old.1, &new.1, scope, &[forged], limits).is_err());
}
