use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, DocumentViewComparison, EdgeKind,
        EvidenceBoundary, EvidenceFailure, EvidenceIssue, EvidenceStore, GraphEdge, GraphNode,
        HierarchyLimits, NodeContent, NodeId, NodeKind, PageEvidence, SourceRef,
        StructuredEvidence, StructuredValue, TextNormalization, TextView, ViewBasis,
        compare_document_views,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, NonTextPaint, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::ComparableToken,
    pdf::ObjectRef,
};

type Fixture = (EvidenceStore, DocumentGraph);

fn mixed_native_page_break(fixture: &mut Fixture) {
    use pdfdelta_core::{
        document::{NativeStructureInventory, NativeStructureKid, NativeStructureParent},
        model::MarkedContent,
    };
    tagged_page_break(fixture);
    let items = fixture.0.native.items().to_vec();
    let page_break = items
        .iter()
        .position(|glyph| glyph.page == PageId(1))
        .expect("page break");
    let SourceRef::Native { glyph: tail } = fixture.1.nodes.last().expect("tail").sources[0] else {
        unreachable!()
    };
    let tail = items
        .iter()
        .position(|glyph| glyph.id == tail)
        .expect("tail source");
    let length = items.len();
    fixture.0.native = Document::new(items).with_marked_content(vec![
        MarkedContent {
            page: PageId(0),
            form: None,
            mcid: 0,
            glyph_range: 0..page_break,
            complete: true,
        },
        MarkedContent {
            page: PageId(1),
            form: None,
            mcid: 0,
            glyph_range: page_break..tail,
            complete: true,
        },
        MarkedContent {
            page: PageId(1),
            form: None,
            mcid: 1,
            glyph_range: tail..length,
            complete: true,
        },
    ]);
    let object = |number| ObjectRef {
        object_number: number,
        generation: 0,
    };
    fixture.0.structured[0].object = Some(object(100));
    let StructuredValue::StructureElement {
        glyphs, content, ..
    } = &mut fixture.0.structured[0].value
    else {
        unreachable!()
    };
    glyphs.clear();
    *content = Some(vec![
        NativeStructureKid::MarkedContent { sequence: 0 },
        NativeStructureKid::Element { element: 1 },
        NativeStructureKid::MarkedContent { sequence: 2 },
        NativeStructureKid::MarkedContent { sequence: 2 },
    ]);
    fixture.0.structured.push(StructuredEvidence {
        id: 1,
        page: None,
        bounds: None,
        object: Some(object(101)),
        backend: 0,
        value: StructuredValue::StructureElement {
            role: "Span".into(),
            identifier: None,
            text: None,
            glyphs: Vec::new(),
            content: Some(vec![NativeStructureKid::MarkedContent { sequence: 1 }]),
            parent: Some(0),
            order: Some(1),
        },
    });
    let relations = fixture.0.inventories.last_mut().expect("relations");
    relations.complete = false;
    relations.sources.push(SourceRef::Structured { element: 1 });
    fixture.0.issues.push(EvidenceIssue {
        boundary: None,
        page: None,
        channel: Channel::Relations,
        sources: Vec::new(),
        kind: EvidenceFailure::Unresolved,
        reason: "Semantic relationships remain unexamined".into(),
    });
    fixture.0.native_structures.push(NativeStructureInventory {
        backend: 0,
        root: Some(object(99)),
        roots: vec![0],
        complete: true,
        parents: Some(vec![
            NativeStructureParent {
                sequence: 0,
                owner: object(100),
            },
            NativeStructureParent {
                sequence: 1,
                owner: object(101),
            },
            NativeStructureParent {
                sequence: 2,
                owner: object(100),
            },
        ]),
    });
}

#[test]
fn mixed_native_order_closes_only_the_unique_parent_bound_source_interval() {
    use pdfdelta_core::document::NativeStructureKid;
    let old = fixture_rows(&["BEGIN", "Budget 10.", "Cost 10.", "END", "TAIL"]);
    let mut new = fixture_rows(&["BEGIN", "Budget 20.", "Cost 20.", "END", "TAIL"]);
    mixed_native_page_break(&mut new);
    let marks = new.0.native.marked_content().to_vec();
    let mut glyphs = new.0.native.items().to_vec();
    let count = glyphs.len() as u32;
    for glyph in &mut glyphs {
        glyph.render_order = count - glyph.render_order;
    }
    new.0.native = Document::new(glyphs).with_marked_content(marks);
    let result = compare(&old, &new);
    let expected: Vec<_> = new.1.nodes[2..4]
        .iter()
        .flat_map(|node| node.sources.clone())
        .collect();
    assert!(
        result
            .scopes
            .iter()
            .flat_map(|scope| &scope.result.text_scope_reviews)
            .any(|review| review.new_sources == expected
                && review.native_regions.as_ref().is_some_and(|chains| chains
                    .new
                    .as_ref()
                    .is_some_and(
                        |chain| chain.convention == "native-k-parent-bound-page-regions-v1"
                    )))
    );
    assert!(!new.0.inventory_complete(None, Channel::Relations));
    for fault in [
        "incomplete",
        "parent-tree",
        "owner",
        "annotation",
        "duplicate",
        "physical-gap",
    ] {
        let mut bad = new.clone();
        match fault {
            "incomplete" => bad.0.native_structures[0].complete = false,
            "parent-tree" => bad.0.native_structures[0].parents = None,
            "owner" => {
                bad.0.native_structures[0]
                    .parents
                    .as_mut()
                    .expect("parents")[1]
                    .owner
                    .object_number = 100
            }
            "annotation" => {
                let StructuredValue::StructureElement {
                    content: Some(content),
                    ..
                } = &mut bad.0.structured[0].value
                else {
                    unreachable!()
                };
                content.insert(
                    1,
                    NativeStructureKid::Annotation {
                        object: ObjectRef {
                            object_number: 200,
                            generation: 0,
                        },
                        page: PageId(0),
                    },
                );
                let StructuredValue::StructureElement { order, .. } =
                    &mut bad.0.structured[1].value
                else {
                    unreachable!()
                };
                *order = Some(2);
            }
            "duplicate" => {
                let mut duplicate = bad.0.structured[1].clone();
                duplicate.id = 2;
                duplicate.object = Some(ObjectRef {
                    object_number: 102,
                    generation: 0,
                });
                let StructuredValue::StructureElement { parent, order, .. } = &mut duplicate.value
                else {
                    unreachable!()
                };
                *parent = None;
                *order = Some(1);
                bad.0.structured.push(duplicate);
                bad.0.native_structures[0].roots.push(2);
                bad.0
                    .inventories
                    .last_mut()
                    .expect("relations")
                    .sources
                    .push(SourceRef::Structured { element: 2 });
            }
            "physical-gap" => {
                let marks = bad.0.native.marked_content().to_vec();
                append_unassigned(&mut bad, PageId(1), 85.0);
                bad.0.native = bad.0.native.clone().with_marked_content(marks);
            }
            _ => unreachable!(),
        }
        assert!(
            compare(&old, &bad)
                .scopes
                .iter()
                .flat_map(|scope| &scope.result.text_scope_reviews)
                .all(|review| review.native_regions.is_none()),
            "{fault}"
        );
    }
}

fn tagged_page_break(fixture: &mut Fixture) {
    let mut glyphs = fixture.0.native.items().to_vec();
    let start = match fixture.1.nodes[3].sources[0] {
        SourceRef::Native { glyph } => glyph,
        _ => unreachable!(),
    };
    for glyph in &mut glyphs {
        if glyph.id >= start {
            glyph.page = PageId(1);
            glyph.baseline.y += 60.0;
            glyph.bbox.min.y += 60.0;
            glyph.bbox.max.y += 60.0;
        }
    }
    for node in &mut fixture.1.nodes[3..] {
        node.pages = vec![PageId(1)];
    }
    fixture
        .1
        .edges
        .retain(|edge| !(edge.kind == EdgeKind::Precedes && edge.from == NodeId(2)));
    fixture.0.structured.push(StructuredEvidence {
        id: 0,
        page: None,
        bounds: None,
        object: None,
        backend: 0,
        value: StructuredValue::StructureElement {
            content: None,
            role: "P".into(),
            identifier: None,
            text: None,
            glyphs: glyphs.iter().map(|glyph| glyph.id).collect(),
            parent: None,
            order: Some(0),
        },
    });
    fixture.0.native = Document::new(glyphs);
    fixture.0.pages.push(PageEvidence {
        page: PageId(1),
        bounds: None,
    });
    fixture.0.inventories[0].page = None;
    fixture.0.inventories.push(ChannelInventory {
        page: None,
        channel: Channel::Relations,
        backend: 0,
        sources: vec![SourceRef::Structured { element: 0 }],
        complete: true,
    });
}

#[test]
fn native_tag_order_closes_page_regions_without_render_order_assumptions() {
    let old = fixture_rows(&["BEGIN", "Budget 10.", "Cost 10.", "END"]);
    let mut new = fixture_rows(&["BEGIN", "Budget 20.", "Cost 20.", "END"]);
    tagged_page_break(&mut new);
    // Render-object order is not the declared tagged reading order.
    let count = new.0.native.items().len() as u32;
    let mut glyphs = new.0.native.items().to_vec();
    for glyph in &mut glyphs {
        glyph.render_order = count - glyph.render_order;
    }
    new.0.native = Document::new(glyphs);
    let result = compare(&old, &new);
    let reviews: Vec<_> = result
        .scopes
        .iter()
        .flat_map(|scope| &scope.result.text_scope_reviews)
        .collect();
    assert!(!reviews.is_empty());
    let proof = reviews
        .iter()
        .find_map(|review| review.native_regions.as_ref())
        .expect("tag transition certificate");
    assert!(proof.old.is_none());
    let chain = proof.new.as_ref().expect("new page chain");
    assert_eq!(chain.regions.len(), 2);
    assert_eq!(chain.transitions.len(), 1);
    let expected: Vec<_> = new.1.nodes[2..4]
        .iter()
        .flat_map(|node| node.sources.iter().copied())
        .collect();
    assert!(reviews.iter().any(|review| review.new_sources == expected));
    assert_eq!(
        chain.transitions[0].structure,
        SourceRef::Structured { element: 0 }
    );
    let mut same = old.clone();
    tagged_page_break(&mut same);
    assert!(
        compare(&old, &same)
            .scopes
            .iter()
            .all(|scope| scope.result.text_scope_reviews.is_empty())
    );
}

#[test]
fn page_adjacency_and_incomplete_or_contrary_tags_do_not_close_transitions() {
    let old = fixture_rows(&["BEGIN", "Budget 10.", "Cost 10.", "END"]);
    let mut new = fixture_rows(&["BEGIN", "Budget 20.", "Cost 20.", "END"]);
    tagged_page_break(&mut new);
    for mutation in 0..6 {
        let mut invalid = new.clone();
        match mutation {
            0 => {
                invalid.0.structured.clear();
                invalid
                    .0
                    .inventories
                    .last_mut()
                    .expect("relation inventory")
                    .sources
                    .clear();
            }
            1 => {
                invalid
                    .0
                    .inventories
                    .last_mut()
                    .expect("relation inventory")
                    .complete = false
            }
            2 => {
                let StructuredValue::StructureElement { glyphs, .. } =
                    &mut invalid.0.structured[0].value
                else {
                    unreachable!()
                };
                glyphs.swap(0, 1);
            }
            3 => {
                let mut duplicate = invalid.0.structured[0].clone();
                duplicate.id = 1;
                invalid.0.structured.push(duplicate);
            }
            4 => append_unassigned(&mut invalid, PageId(1), 85.0),
            _ => invalid.0.inventories[0].complete = false,
        }
        assert!(
            compare(&old, &invalid)
                .scopes
                .iter()
                .flat_map(|scope| &scope.result.text_scope_reviews)
                .all(|review| review.native_regions.is_none()),
            "mutation {mutation}"
        );
    }
}

#[test]
fn source_cut_subranges_retain_their_enclosing_page_transition_certificate() {
    let old = fixture_rows(&["BEGIN", "Budget 10.", "Cost 10.", "END", "STOP"]);
    let mut new = fixture_rows(&["BEGIN", "Budget 20.", "Cost 20.", "END", "STOP"]);
    tagged_page_break(&mut new);
    let expected = new.1.nodes[3].sources.clone();
    merge_following_node(&mut new, 3);
    let result = compare(&old, &new);
    assert!(
        result
            .scopes
            .iter()
            .flat_map(|scope| &scope.result.text_scope_reviews)
            .any(|review| review.source_cuts.is_some()
                && review.new_sources == expected
                && review
                    .native_regions
                    .as_ref()
                    .is_some_and(|chains| chains.new.is_some()))
    );
}

#[test]
fn an_empty_structured_group_does_not_abort_the_native_only_presence_search() {
    let mut old = fixture_rows(&["BEGIN", "END", "STOP"]);
    let mut new = fixture_rows(&["BEGIN", "Added.", "END", "STOP"]);
    for fixture in [&mut old, &mut new] {
        for node in &mut fixture.1.nodes {
            node.basis = ViewBasis::SourceStructure;
        }
        for edge in &mut fixture.1.edges {
            edge.basis = ViewBasis::SourceStructure;
        }
    }
    let result = compare(&old, &new);
    assert!(
        result
            .scopes
            .iter()
            .flat_map(|scope| &scope.result.text_scope_reviews)
            .all(|review| review.presence.is_none())
    );
}

#[test]
fn partially_clipped_boundary_padding_is_census_only_and_cannot_touch_the_body() {
    let old = fixture_with_boundaries("Budget 10.", "BEGIN", "END");
    for mutation in 0..4 {
        let mut new = fixture_with_boundaries("Budget 20.", "BEGIN", "END ");
        let padding = *new.1.nodes[3].sources.last().expect("boundary padding");
        let mut glyphs = new.0.native.items().to_vec();
        let end = glyphs.len() - 1;
        glyphs[end].path_clip_status = GlyphPathClipStatus::PartiallyOutside;
        match mutation {
            1 => glyphs[end - 1].path_clip_status = GlyphPathClipStatus::PartiallyOutside,
            2 => {
                glyphs["BEGIN".len() + "Budget".len()].path_clip_status =
                    GlyphPathClipStatus::PartiallyOutside
            }
            3 => glyphs[end].bbox.max.y = 75.0,
            _ => {}
        }
        new.0.native = Document::new(glyphs);
        let result = compare(&old, &new);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        assert_eq!(
            reviews.len(),
            usize::from(mutation == 0),
            "mutation {mutation}"
        );
        if let Some(review) = reviews.first() {
            assert!(!review.new_sources.contains(&padding));
            let cuts = review
                .source_cuts
                .as_ref()
                .expect("separate census closure");
            let pdfdelta_core::document::SourceCutPopulation::MatchedInterval {
                boundary_padding: Some(proof),
                ..
            } = &cuts.population
            else {
                panic!("explicit uncertain padding proof")
            };
            assert_eq!(proof.new, [padding]);
            assert_eq!(
                new.0
                    .native
                    .items()
                    .last()
                    .expect("retained padding")
                    .path_clip_status,
                GlyphPathClipStatus::PartiallyOutside
            );
        }
    }
}

fn collapse_last_two_literal_spaces(node: &mut GraphNode) {
    let NodeContent::Text { view } = &mut node.content else {
        unreachable!()
    };
    let removed = view.origins.pop().expect("last literal space");
    view.origins
        .last_mut()
        .expect("previous literal space")
        .extend(removed);
    view.tokens.pop();
    view.source_backed.pop();
    view.normalization = TextNormalization::Unresolved {
        reason: "retained whitespace contraction".into(),
    };
}

#[test]
fn source_cuts_expand_literal_space_multiplicity_and_keep_original_token_addresses() {
    for body in ["Value 20.  ", "Value 10. "] {
        let mut old = fixture_rows(&["BEGIN", "Value 10.  ", "END", "STOP"]);
        let mut new = fixture_rows(&["BEGIN", body, "END", "STOP"]);
        let old_extent = old.1.nodes[2].sources.clone();
        let new_extent = new.1.nodes[2].sources.clone();
        collapse_last_two_literal_spaces(&mut old.1.nodes[2]);
        if body.ends_with("  ") {
            collapse_last_two_literal_spaces(&mut new.1.nodes[2]);
        }
        let NodeContent::Text { view } = &new.1.nodes[2].content else {
            unreachable!()
        };
        let original_end = view.tokens.len();
        merge_following_node(&mut new, 2);
        let result = compare(&old, &new);
        let review = result
            .scopes
            .iter()
            .flat_map(|scope| &scope.result.text_scope_reviews)
            .find(|review| review.old_sources == old_extent && review.new_sources == new_extent)
            .expect("raw spaces and unchanged source extents");
        let cuts = review
            .source_cuts
            .as_ref()
            .expect("source cut after expanded spaces");
        assert_eq!(cuts.projection, "retained-glyph-whitespace-expansion-v1");
        assert_eq!(cuts.exit.new.token_boundary, original_end);
        let Some(pdfdelta_core::document::TypedOperation::TextChanged {
            old: Some(old_text),
            new: Some(new_text),
        }) = &review.comparison.operation
        else {
            panic!("exact literal-space content comparison")
        };
        assert_eq!(old_text, "Value 10.  ");
        assert_eq!(new_text, body);
    }
}

fn fixture(interior: &str) -> Fixture {
    fixture_with_boundaries(interior, "First boundary.", "Last boundary.")
}

fn fixture_with_boundaries(interior: &str, first: &str, last: &str) -> Fixture {
    fixture_rows(&[first, interior, last])
}

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
fn exact_masks_do_not_expand_into_unchanged_preceding_paragraphs() {
    let old = fixture_rows(&[
        "HEADER",
        "INTRO",
        "Same opening.",
        "old body",
        "END",
        "TAIL",
    ]);
    let new = fixture_rows(&[
        "HEADER",
        "INTRO",
        "Same opening.",
        "new body",
        "END",
        "TAIL",
    ]);
    for (old, new) in [(&old, &new), (&new, &old)] {
        let result = compare(old, new);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        assert!(!reviews.is_empty());
        assert!(reviews.iter().all(|review| {
            review.old_sources == old.1.nodes[4].sources
                && review.new_sources == new.1.nodes[4].sources
        }));
    }
}

#[test]
fn unresolved_opening_rows_remain_inside_the_enclosing_source_interval() {
    for opening in [
        &["Same opening."][..],
        &["Same opening.", "Same continuation."][..],
    ] {
        for merged in [false, true] {
            let setup = |body| {
                fixture_rows(
                    &["HEADER", "INTRO"]
                        .into_iter()
                        .chain(opening.iter().copied())
                        .chain([body, "END", "TAIL"])
                        .collect::<Vec<_>>(),
                )
            };
            let old_body = "a".repeat(800);
            let new_body = "b".repeat(800);
            let mut old = setup(&old_body);
            let mut new = setup(&new_body);
            for fixture in [&mut old, &mut new] {
                let NodeContent::Text { view } = &mut fixture.1.nodes[opening.len() + 3].content
                else {
                    unreachable!()
                };
                view.normalization = TextNormalization::Unresolved {
                    reason: "Raw body projection requires source validation".into(),
                };
            }
            append_unassigned(&mut old, PageId(1), 70.0);
            append_unassigned(&mut new, PageId(1), 70.0);
            let old_target: Vec<_> = old.1.nodes[3..=opening.len() + 3]
                .iter()
                .flat_map(|n| n.sources.iter().copied())
                .collect();
            let new_target: Vec<_> = new.1.nodes[3..=opening.len() + 3]
                .iter()
                .flat_map(|n| n.sources.iter().copied())
                .collect();
            if merged {
                for _ in opening {
                    merge_following_node(&mut old, 3);
                    merge_following_node(&mut new, 3);
                }
            }
            for reversed in [false, true] {
                let result = if reversed {
                    compare(&new, &old)
                } else {
                    compare(&old, &new)
                };
                assert!(
                    result.scopes[0]
                        .result
                        .text_scope_reviews
                        .iter()
                        .any(|review| {
                            if reversed {
                                review.old_sources == new_target && review.new_sources == old_target
                            } else {
                                review.old_sources == old_target && review.new_sources == new_target
                            }
                        }),
                    "opening {opening:?}, merged {merged}, reversed {reversed}"
                );
            }
        }
    }
}

#[test]
fn native_layout_edges_across_columns_do_not_displace_local_boundaries() {
    let mut old = fixture_rows(&["BEGIN", "old body", "old side", "END", "TAIL"]);
    let mut new = fixture_rows(&["BEGIN", "new body", "new side", "END", "TAIL"]);
    for fixture in [&mut old, &mut new] {
        let side = &fixture.1.nodes[3].sources;
        let mut glyphs = fixture.0.native.items().to_vec();
        for glyph in &mut glyphs {
            if side.contains(&SourceRef::Native { glyph: glyph.id }) {
                glyph.baseline.x += 1000.0;
                glyph.bbox.min.x += 1000.0;
                glyph.bbox.max.x += 1000.0;
            }
        }
        let mut order = 0;
        for is_side in [false, true] {
            for glyph in &mut glyphs {
                if side.contains(&SourceRef::Native { glyph: glyph.id }) == is_side {
                    glyph.render_order = order;
                    order += 1;
                }
            }
        }
        fixture.0.native = Document::new(glyphs);
    }
    for (old, new) in [(&old, &new), (&new, &old)] {
        let result = compare(old, new);
        assert!(
            result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.old_sources == old.1.nodes[2].sources
                        && review.new_sources == new.1.nodes[2].sources
                })
        );
        let mut gap = old.clone();
        append_unassigned(&mut gap, PageId(0), 70.0);
        assert!(
            compare(&gap, new).scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .all(|review| {
                    review.old_sources != old.1.nodes[2].sources
                        || review.new_sources != new.1.nodes[2].sources
                })
        );
    }
}

#[test]
fn equal_native_interval_validates_left_to_right_rows_and_retains_gaps() {
    let mut old = fixture_rows(&["First boundary.", "repeat", "repeat", "Last boundary."]);
    let sources = old.1.nodes[3].sources.clone();
    let mut glyphs = old.0.native.items().to_vec();
    for glyph in &mut glyphs {
        if sources.contains(&SourceRef::Native { glyph: glyph.id }) {
            glyph.baseline.x += 80.0;
            glyph.bbox.min.x += 80.0;
            glyph.bbox.max.x += 80.0;
            glyph.baseline.y += 30.0;
            glyph.bbox.min.y += 30.0;
            glyph.bbox.max.y += 30.0;
        }
    }
    old.0.native = Document::new(glyphs);
    let result = compare(&old, &old);
    let intervals = &result.scopes[0].result.native_text_intervals;
    assert_eq!(intervals.len(), 1);
    assert!(intervals[0].comparison().operation.is_none());
    assert_eq!(intervals[0].comparison().old, [NodeId(2), NodeId(3)]);
    assert!(!result.search_resolved());

    let mut missing = old.clone();
    append_unassigned(&mut missing, PageId(0), 70.0);
    assert!(
        compare(&old, &missing).scopes[0]
            .result
            .native_text_intervals
            .is_empty()
    );

    let mut reversed = old.clone();
    let mut glyphs = reversed.0.native.items().to_vec();
    for glyph in &mut glyphs {
        if sources.contains(&SourceRef::Native { glyph: glyph.id }) {
            glyph.baseline.x -= 160.0;
            glyph.bbox.min.x -= 160.0;
            glyph.bbox.max.x -= 160.0;
        }
    }
    reversed.0.native = Document::new(glyphs);
    assert!(
        compare(&old, &reversed).scopes[0]
            .result
            .native_text_intervals
            .is_empty()
    );
}

#[test]
fn repeated_equal_interiors_reuse_live_padded_boundary_proofs() {
    let old = fixture_rows(&[" First boundary.", "repeat", "repeat", "Last boundary. "]);
    let new = fixture_rows(&["First boundary.", "repeat", "repeat", "Last boundary."]);
    let result = compare(&old, &new);
    assert_eq!(result.scopes[0].result.native_text_domains.len(), 2);
    let intervals = &result.scopes[0].result.native_text_intervals;
    assert_eq!(intervals.len(), 1);
    assert!(intervals[0].comparison().operation.is_none());
    assert_eq!(intervals[0].comparison().old, [NodeId(2), NodeId(3)]);
    let coverage = pdfdelta_core::document::document_coverage(
        DocumentView {
            evidence: &old.0,
            graph: &old.1,
        },
        DocumentView {
            evidence: &new.0,
            graph: &new.1,
        },
        &result,
        &[Channel::Text].into(),
    );
    assert_eq!(coverage[0].old_uncompared_sources, 2);
    assert_eq!(coverage[0].new_uncompared_sources, 0);
    assert!(!coverage[0].complete);
    let mut missing = new.clone();
    append_unassigned(&mut missing, PageId(0), 60.0);
    assert!(
        compare(&old, &missing).scopes[0]
            .result
            .native_text_intervals
            .is_empty()
    );
}

#[test]
fn equal_repeated_native_interior_can_be_compared_without_selecting_occurrences() {
    let old = fixture_rows(&["First boundary.", "repeat", "repeat", "Last boundary."]);
    let new = old.clone();
    let result = compare(&old, &new);
    assert!(result.scopes[0].result.text_scope_reviews.is_empty());
    assert!(
        result.scopes[0]
            .result
            .matching
            .components
            .iter()
            .any(|component| {
                component.exhaustive
                    && component.mandatory.is_empty()
                    && !component.proposals.is_empty()
            })
    );
    assert!(!result.search_resolved());
    let intervals = &result.scopes[0].result.native_text_intervals;
    assert_eq!(intervals.len(), 1);
    let comparison = intervals[0].comparison();
    assert!(comparison.compared);
    assert!(comparison.operation.is_none());
    assert_eq!(
        comparison
            .text_mask
            .as_ref()
            .expect("validated native fixture evidence")
            .claims
            .changed_source_upper,
        0
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
        &result,
        &[Channel::Text].into(),
    );
    assert!(coverage[0].complete);

    let mut missing = new.clone();
    append_unassigned(&mut missing, PageId(0), 60.0);
    assert!(
        compare(&old, &missing).scopes[0]
            .result
            .native_text_intervals
            .is_empty()
    );

    let mut wrong_reading = new.clone();
    let mut glyphs = wrong_reading.0.native.items().to_vec();
    let SourceRef::Native { glyph } = wrong_reading.1.nodes[2].sources[0] else {
        unreachable!()
    };
    glyphs
        .iter_mut()
        .find(|item| item.id == glyph)
        .expect("validated native fixture evidence")
        .text = DecodedText::Mapped("X".into());
    wrong_reading.0.native = Document::new(glyphs);
    assert!(
        compare(&old, &wrong_reading).scopes[0]
            .result
            .native_text_intervals
            .is_empty()
    );
}

#[test]
fn owned_content_change_retains_uncertain_localization_without_owning_synthetic_spaces() {
    for changed in [false, true] {
        let old = fixture("a b");
        let mut new = fixture(if changed { "ac" } else { "ab" });
        let node = &mut new.1.nodes[2];
        let NodeContent::Text { view } = &mut node.content else {
            unreachable!()
        };
        view.tokens.insert(1, ComparableToken::Scalar(' '));
        view.origins.insert(1, node.sources.clone());
        view.source_backed.insert(1, false);
        view.normalization = TextNormalization::Unresolved {
            reason: "Layout spacing requires native source validation".into(),
        };
        let result = compare(&old, &new);
        let intervals = &result.scopes[0].result.native_text_intervals;
        assert_eq!(intervals.len(), usize::from(changed));
        let coverage = pdfdelta_core::document::document_coverage(
            DocumentView {
                evidence: &old.0,
                graph: &old.1,
            },
            DocumentView {
                evidence: &new.0,
                graph: &new.1,
            },
            &result,
            &[Channel::Text].into(),
        );
        assert_eq!(coverage[0].complete, changed);
        if changed {
            let local = intervals[0].comparison();
            assert!(!local.unresolved.is_empty());
            let mask = local.text_mask.as_ref().expect("complete exact proof");
            assert_eq!(mask.claims.changed_source_lower, 2);
            assert_eq!(mask.claims.changed_source_upper, 3);
            assert_eq!(mask.old.len(), 1);
            assert_eq!(mask.new.len(), 1);
            assert_eq!(mask.old[0].sources, [old.1.nodes[2].sources[2]]);
            assert_eq!(mask.new[0].sources, [new.1.nodes[2].sources[1]]);
            assert_eq!(
                coverage[0].old_discovered_sources,
                old.0.native.items().len()
            );
            assert_eq!(
                coverage[0].new_discovered_sources,
                new.0.native.items().len()
            );
        }
    }
}

#[test]
fn native_interval_proof_survives_exhausted_optional_cut_discovery() {
    let old = fixture("ab");
    let new = fixture("a b");
    for budget in [500, 800, 1_000, 1_400, 1_700] {
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_ownership_visits = budget;
        let result = compare_document_views(
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
        .expect("bounded comparison");
        let scope = &result.scopes[0].result;
        assert!(
            scope
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_none())
        );
        assert!(
            !scope
                .source_cut_search
                .as_ref()
                .expect("cut search status")
                .exhaustive
        );
        assert_eq!(
            scope.native_text_intervals.len(),
            1,
            "discovery budget {budget}"
        );
    }
}

#[test]
fn owned_native_intervals_require_live_source_proofs_and_preserve_exact_masks() {
    let old = fixture("ab");
    let new = fixture("a b");
    let result = compare(&old, &new);
    let intervals = &result.scopes[0].result.native_text_intervals;
    assert_eq!(intervals.len(), 1);
    let local = intervals[0].comparison();
    assert!(local.compared);
    assert!(local.unresolved.is_empty());
    assert_eq!(
        local.text_mask,
        result.scopes[0].result.text_scope_reviews[0]
            .comparison
            .text_mask
    );
    let coverage = |comparison: &DocumentViewComparison| {
        pdfdelta_core::document::document_coverage(
            DocumentView {
                evidence: &old.0,
                graph: &old.1,
            },
            DocumentView {
                evidence: &new.0,
                graph: &new.1,
            },
            comparison,
            &[Channel::Text].into(),
        )
        .remove(0)
    };
    let current = coverage(&result);
    assert_eq!(current.old_uncompared_sources, 0);
    assert_eq!(current.new_uncompared_sources, 0);
    let encoded = serde_json::to_vec(&result).expect("encode report");
    let decoded = serde_json::from_slice(&encoded).expect("decode report");
    let reloaded = coverage(&decoded);
    assert_eq!(reloaded.old_uncompared_sources, 2);
    assert_eq!(reloaded.new_uncompared_sources, 3);

    for missing in [false, true] {
        let mut incomplete = fixture("ab");
        if missing {
            append_unassigned(&mut incomplete, PageId(0), 70.0);
        } else {
            let mut glyphs = incomplete.0.native.items().to_vec();
            let SourceRef::Native { glyph } = incomplete.1.nodes[2].sources[0] else {
                unreachable!()
            };
            glyphs
                .iter_mut()
                .find(|item| item.id == glyph)
                .expect("interior glyph")
                .text = DecodedText::Mapped("x".into());
            incomplete.0.native = Document::new(glyphs);
        }
        assert!(
            compare(&incomplete, &new).scopes[0]
                .result
                .native_text_intervals
                .is_empty()
        );
    }
}

#[test]
fn native_interval_survives_unrelated_pages_storage_order_and_reversal() {
    let mut old = fixture("a");
    let mut new = fixture("aa");
    assert!(!old.1.relations_complete);
    let baseline = compare(&old, &new);
    assert_eq!(baseline.scopes[0].result.native_text_intervals.len(), 1);
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
    for (before, after) in reviews[0]
        .boundaries
        .iter()
        .zip(actual[0].boundaries.iter().copied())
    {
        assert_eq!(
            baseline.scopes[0].result.candidates.proposals[*before],
            scope.candidates.proposals[after]
        );
    }
    // Proposal indexes address their report-local array, not persistent identity.
    actual[0].boundaries = reviews[0].boundaries.clone();
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
fn native_interval_retains_literal_space_changes() {
    for (a, b, reviews) in [
        ("ab", "a b", 1),
        ("ab", " ac", 3),
        ("The file is now here.", "The file is nowhere.", 1),
        ("int x;", "intx;", 1),
    ] {
        let old = fixture(a);
        let new = fixture(b);
        let comparison = compare(&old, &new);
        let scope = &comparison.scopes[0].result;
        assert_eq!(scope.text_scope_reviews.len(), reviews, "{a:?} -> {b:?}");
        assert_eq!(
            scope
                .text_scope_reviews
                .iter()
                .filter(|review| review.source_cuts.is_none())
                .count(),
            1
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
        assert_eq!(scope.native_text_intervals.len(), 1);
        assert!(coverage[0].complete);
        assert_eq!(
            compare(&new, &old).scopes[0]
                .result
                .text_scope_reviews
                .len(),
            scope.text_scope_reviews.len()
        );
    }
}

fn merge_last_boundary(fixture: &mut Fixture) {
    merge_following_node(fixture, 2);
}

fn merge_following_node(fixture: &mut Fixture, index: usize) {
    let last = fixture.1.nodes.remove(index + 1);
    let NodeContent::Text { view: last_view } = last.content else {
        panic!("last boundary must contain text");
    };
    let interior = &mut fixture.1.nodes[index];
    let NodeContent::Text { view } = &mut interior.content else {
        panic!("interior must contain text");
    };
    view.tokens.extend(last_view.tokens);
    view.origins.extend(last_view.origins);
    view.source_backed.extend(last_view.source_backed);
    interior.sources.extend(last.sources);
    let retained = interior.id;
    for edge in &mut fixture.1.edges {
        if edge.from == last.id && edge.kind == EdgeKind::Precedes {
            edge.from = retained;
        }
    }
    fixture
        .1
        .edges
        .retain(|edge| edge.from != last.id && edge.to != last.id);
}

#[test]
fn accepted_group_boundaries_retain_the_exact_native_interior() {
    for (merge_entry, merge_exit) in [(true, false), (false, true), (true, true)] {
        let old = fixture_rows(&["Opening ", "context.", "Value 10.", "Ending ", "context."]);
        let mut new = fixture_rows(&["Opening ", "context.", "Value 20.", "Ending ", "context."]);
        if merge_exit {
            merge_following_node(&mut new, 4);
        }
        if merge_entry {
            merge_following_node(&mut new, 1);
        }
        for (old, new) in [(&old, &new), (&new, &old)] {
            let body = |fixture: &Fixture| {
                fixture
                    .1
                    .nodes
                    .iter()
                    .find(|node| node.id == NodeId(3))
                    .expect("retained fixture node")
                    .sources
                    .clone()
            };
            let expected = [body(old), body(new)];
            let result = compare(old, new);
            let scope = &result.scopes[0].result;
            let review = scope
                .text_scope_reviews
                .iter()
                .find(|review| {
                    review.source_cuts.is_none()
                        && review.old_sources == expected[0]
                        && review.new_sources == expected[1]
                })
                .expect("an accepted exact group can bound the same finite interior extent");
            assert!(review.comparison.text_mask.is_some());
            for (edge, index) in review.boundaries.iter().copied().enumerate() {
                let proposal = &scope.candidates.proposals[index];
                assert!(scope.accepted_correspondences.contains(&index));
                assert!(scope.matching.source_only_mandatory.contains(&index));
                for (fixture, ids, sources) in [
                    (old, &proposal.old, &review.old_boundaries[edge]),
                    (new, &proposal.new, &review.new_boundaries[edge]),
                ] {
                    let expected: Vec<_> = ids
                        .iter()
                        .flat_map(|id| {
                            fixture
                                .1
                                .nodes
                                .iter()
                                .find(|node| node.id == *id)
                                .expect("retained fixture node")
                                .sources
                                .iter()
                                .copied()
                        })
                        .collect();
                    assert_eq!(*sources, expected);
                }
            }
            let mut gap = old.clone();
            append_unassigned(&mut gap, PageId(0), 40.0);
            assert!(
                compare(&gap, new).scopes[0]
                    .result
                    .text_scope_reviews
                    .iter()
                    .all(|review| {
                        review.old_sources != expected[0] || review.new_sources != expected[1]
                    }),
                "group boundaries must retain the complete native source census"
            );
        }
    }
}

#[test]
fn accepted_group_boundaries_retain_local_presence_in_both_directions() {
    let empty = fixture_rows(&["Opening ", "context.", "Ending ", "context."]);
    let mut populated = fixture_rows(&["Opening ", "context.", "Value 20.", "Ending ", "context."]);
    let added = populated.1.nodes[3].sources.clone();
    merge_following_node(&mut populated, 4);
    merge_following_node(&mut populated, 1);
    for (old, new, insertion) in [(&empty, &populated, true), (&populated, &empty, false)] {
        let result = compare(old, new);
        let review = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .find(|review| {
                review.source_cuts.is_none()
                    && review.presence.is_some()
                    && if insertion {
                        review.old_sources.is_empty() && review.new_sources == added
                    } else {
                        review.old_sources == added && review.new_sources.is_empty()
                    }
            })
            .expect("accepted groups preserve a closed local insertion or deletion");
        assert!(review.comparison.text_mask.is_some());
        assert_eq!(review.boundaries.len(), 2);
    }
}

#[test]
fn group_boundaries_preserve_the_enclosing_comparison_without_rechecking_covered_nodes() {
    let old = fixture_rows(&[
        "BEGIN",
        "Value 10.",
        "Shared ",
        "context.",
        "Value 11.",
        "END",
    ]);
    let mut new = fixture_rows(&[
        "BEGIN",
        "Value 20.",
        "Shared ",
        "context.",
        "Value 21.",
        "END",
    ]);
    merge_following_node(&mut new, 3);
    let interior = |fixture: &Fixture| {
        fixture
            .1
            .nodes
            .iter()
            .filter(|node| (2..=5).contains(&node.id.0))
            .flat_map(|node| node.sources.iter().copied())
            .collect::<Vec<_>>()
    };
    let result = compare(&old, &new);
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert!(
        reviews.iter().any(|review| {
            review.source_cuts.is_none()
                && review.old_sources == interior(&old)
                && review.new_sources == interior(&new)
        }),
        "new internal boundaries preserve the established finite extent"
    );
    assert_eq!(
        reviews
            .iter()
            .filter(|review| review.source_cuts.is_none())
            .count(),
        1,
        "covered group subranges do not repeat the whole-node comparison"
    );
}

#[test]
fn group_order_preflight_preserves_work_for_a_later_source_change() {
    let mut old = fixture_rows(&[
        "BEGIN",
        "abcd",
        "WXYZ",
        "MIDDLE ",
        "boundary.",
        "Budget 10.",
        "END",
    ]);
    let mut new = fixture_rows(&[
        "BEGIN",
        "abcd",
        "WXYZ",
        "MIDDLE ",
        "boundary.",
        "Budget 20.",
        "END",
    ]);
    merge_following_node(&mut old, 2);
    merge_following_node(&mut new, 2);
    merge_following_node(&mut new, 3);
    let control = old.1.nodes[2].sources.clone();
    let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
        unreachable!()
    };
    let original = view.clone();
    for (position, source) in [0, 4, 1, 5, 2, 6, 3, 7].into_iter().enumerate() {
        view.tokens[position] = original.tokens[source].clone();
        view.origins[position] = original.origins[source].clone();
    }
    let mut glyphs = old.0.native.items().to_vec();
    let template = glyphs[0].clone();
    for index in 0..2_000 {
        let mut glyph = template.clone();
        glyph.id = GlyphId(10_000 + index);
        glyph.render_order = 10_000 + index as u32;
        glyph.baseline.y = 200.0;
        glyph.bbox.min.y = 200.0;
        glyph.bbox.max.y = 210.0;
        old.0.inventories[0]
            .sources
            .push(SourceRef::Native { glyph: glyph.id });
        glyphs.push(glyph);
    }
    old.0.native = Document::new(glyphs);
    for budget in [2_000, 4_000, 8_000] {
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_group_nodes = 2;
        limits.matching.max_ownership_visits = budget;
        let result = compare_document_views(
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
        .expect("bounded grouped comparison");
        let reviews = &result.scopes[0].result.text_scope_reviews;
        let recovered = reviews.iter().any(|review| {
            review.old_sources == old.1.nodes[5].sources
                && review.new_sources == new.1.nodes[4].sources
        });
        assert_eq!(recovered, budget >= 4_000, "budget {budget}");
        assert!(reviews.iter().all(|review| {
            !review
                .old_sources
                .iter()
                .any(|source| control.contains(source))
        }));
    }
    // A deferred unordered range still admits a real source-count change.
    let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
        unreachable!()
    };
    view.tokens[0] = ComparableToken::Scalar('q');
    let SourceRef::Native { glyph: changed } = view.origins[0][0] else {
        unreachable!()
    };
    let mut glyphs = new.0.native.items().to_vec();
    let glyph = glyphs
        .iter_mut()
        .find(|glyph| glyph.id == changed)
        .expect("changed native glyph");
    glyph.text = DecodedText::Mapped("q".into());
    glyph.raw_code = b"q".to_vec();
    new.0.native = Document::new(glyphs);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.max_group_nodes = 2;
    let result = compare_document_views(
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
    .expect("deferred source-count comparison");
    assert!(
        result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| {
                review.old_sources == control
                    && review.comparison.text_mask.is_none()
                    && review.comparison.text_change_proof.is_some()
            })
    );
}

#[test]
fn source_content_edges_preserve_coarse_space_changes_and_exact_body_sources() {
    for (a, b, refined) in [
        ("Value 10.   ", "Value 20.  ", true),
        ("  Value 10. ", " Value 20.  ", true),
        ("Value 10.", "Value 20.  ", true),
        ("Value 10.   ", "Value 10.  ", false),
        ("The file is now here.  ", "The file is nowhere. ", true),
        ("  ", " ", false),
    ] {
        let old = fixture_rows(&["BEGIN", a, "END", "STOP"]);
        let mut new = fixture_rows(&["BEGIN", b, "END", "STOP"]);
        let old_sources = old.1.nodes[2].sources.clone();
        let new_sources = new.1.nodes[2].sources.clone();
        merge_following_node(&mut new, 2);
        let result = compare(&old, &new);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        if !a.trim_matches(' ').is_empty() {
            assert!(
                reviews
                    .iter()
                    .any(|review| review.old_sources == old_sources
                        && review.new_sources == new_sources),
                "coarse {a:?} -> {b:?}"
            );
        }
        let inner: Vec<_> = reviews
            .iter()
            .filter(|review| {
                review
                    .source_cuts
                    .as_ref()
                    .is_some_and(|cuts| cuts.edge_refinement.is_some())
            })
            .collect();
        let inner: Vec<_> = inner
            .into_iter()
            .filter(|review| {
                let extends_to_end = review
                    .old_sources
                    .iter()
                    .any(|source| old.1.nodes[3].sources.contains(source));
                if extends_to_end {
                    // A second closed view may retain END and the literal
                    // spaces before it. Its finite extent must remain exact.
                    let old_expected: Vec<_> = old_sources
                        [a.len() - a.trim_start_matches(' ').len()..]
                        .iter()
                        .chain(&old.1.nodes[3].sources)
                        .copied()
                        .collect();
                    let new_expected =
                        &new.1.nodes[2].sources[b.len() - b.trim_start_matches(' ').len()..];
                    assert_eq!(review.old_sources, old_expected);
                    assert_eq!(review.new_sources, *new_expected);
                }
                !extends_to_end
            })
            .collect();
        assert_eq!(inner.len(), usize::from(refined), "{a:?} -> {b:?}");
        if let Some(review) = inner.first() {
            let old_start = a.len() - a.trim_start_matches(' ').len();
            let new_start = b.len() - b.trim_start_matches(' ').len();
            assert_eq!(
                review.old_sources,
                old_sources[old_start..a.trim_end_matches(' ').len()]
            );
            assert_eq!(
                review.new_sources,
                new_sources[new_start..b.trim_end_matches(' ').len()]
            );
            let proof = review
                .source_cuts
                .as_ref()
                .expect("source cut interval")
                .edge_refinement
                .as_ref()
                .expect("content edge refinement");
            for (padding, sources, body, start) in [
                (
                    &proof.old_padding,
                    &old_sources,
                    &review.old_sources,
                    old_start,
                ),
                (
                    &proof.new_padding,
                    &new_sources,
                    &review.new_sources,
                    new_start,
                ),
            ] {
                let prefix: Vec<_> = padding[0]
                    .iter()
                    .flat_map(|fragment| fragment.sources.iter().copied())
                    .collect();
                assert_eq!(prefix, sources[..start]);
                let rest: Vec<_> = padding[1]
                    .iter()
                    .flat_map(|fragment| fragment.sources.iter().copied())
                    .collect();
                assert_eq!(rest, sources[start + body.len()..]);
            }
        }
    }
}

#[test]
fn detached_margin_nodes_do_not_prevent_a_closed_body_interval() {
    fn center_endpoints(fixture: &mut Fixture) {
        let last = fixture.1.nodes.len() - 1;
        let mut glyphs = fixture.0.native.items().to_vec();
        for index in [1, last] {
            for source in &fixture.1.nodes[index].sources {
                let SourceRef::Native { glyph } = source else {
                    unreachable!()
                };
                let glyph = glyphs
                    .iter_mut()
                    .find(|g| g.id == *glyph)
                    .expect("heading source");
                glyph.baseline.x += 100.0;
                glyph.bbox.min.x += 100.0;
                glyph.bbox.max.x += 100.0;
            }
        }
        fixture.0.native = Document::new(glyphs);
    }
    let setup = || {
        let mut old = fixture_rows(&["BEGIN", "101", "Budget 10. ", "102", "Cost 10. ", "END"]);
        let mut glyphs = old.0.native.items().to_vec();
        for index in [2, 4] {
            for source in &old.1.nodes[index].sources {
                let SourceRef::Native { glyph } = source else {
                    unreachable!()
                };
                let glyph = glyphs
                    .iter_mut()
                    .find(|g| g.id == *glyph)
                    .expect("margin source");
                glyph.baseline.x -= 50.0;
                glyph.bbox.min.x -= 50.0;
                glyph.bbox.max.x -= 50.0;
                glyph.baseline.y -= 30.0;
                glyph.bbox.min.y -= 30.0;
                glyph.bbox.max.y -= 30.0;
            }
        }
        old.0.native = Document::new(glyphs);
        center_endpoints(&mut old);
        old
    };
    let mut new = fixture_rows(&["BEGIN", "Budget 20. ", "Cost 20. ", "END"]);
    center_endpoints(&mut new);
    for mutation in 0..4 {
        let mut old = setup();
        if mutation == 2 {
            append_unassigned(&mut old, PageId(0), 40.0);
        } else if mutation != 0 {
            let mut glyphs = old.0.native.items().to_vec();
            let node = if mutation == 1 { 2 } else { 3 };
            let SourceRef::Native { glyph } = old.1.nodes[node].sources[0] else {
                unreachable!()
            };
            let glyph = glyphs
                .iter_mut()
                .find(|g| g.id == glyph)
                .expect("mutated source");
            if mutation == 1 {
                glyph.bbox.max.x = 0.0;
            } else {
                glyph.path_clip_status = GlyphPathClipStatus::PartiallyOutside;
            }
            old.0.native = Document::new(glyphs);
        }
        let original_nodes = old.1.nodes.clone();
        let expected_old: Vec<_> = [3, 5]
            .into_iter()
            .flat_map(|i| old.1.nodes[i].sources.iter().copied())
            .collect();
        let expected_new: Vec<_> = [2, 3]
            .into_iter()
            .flat_map(|i| new.1.nodes[i].sources.iter().copied())
            .collect();
        let result = compare(&old, &new);
        let found = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| review.old_sources == expected_old && review.new_sources == expected_new);
        assert_eq!(found, mutation == 0, "mutation {mutation}");
        if mutation == 0 {
            assert!(
                result.scopes[0]
                    .result
                    .text_scope_reviews
                    .iter()
                    .any(|review| {
                        review.source_cuts.is_some()
                            && review.old_sources == expected_old[..expected_old.len() - 1]
                            && review.new_sources == expected_new[..expected_new.len() - 1]
                    })
            );
        }
        assert_eq!(old.1.nodes, original_nodes);
    }
}

#[test]
fn isolated_same_row_prefixes_preserve_source_cuts_and_reject_gaps_or_overlap() {
    fn setup(text: &str, detached: bool) -> Fixture {
        let heading = if detached {
            "NEXT revised section"
        } else {
            "NEXT section"
        };
        let mut fixture = fixture_rows(&["BEGIN", "1.", text, heading, "2.", "TAIL", "STOP"]);
        let mut glyphs = fixture.0.native.items().to_vec();
        for node in [3, 6] {
            for source in &fixture.1.nodes[node].sources {
                let SourceRef::Native { glyph } = source else {
                    unreachable!()
                };
                let glyph = glyphs
                    .iter_mut()
                    .find(|g| g.id == *glyph)
                    .expect("fixture source");
                glyph.baseline.x += 50.0;
                glyph.bbox.min.x += 50.0;
                glyph.bbox.max.x += 50.0;
                glyph.baseline.y += 30.0;
                glyph.bbox.min.y += 30.0;
                glyph.bbox.max.y += 30.0;
            }
        }
        if detached {
            fixture.1.edges.retain(|edge| {
                edge.kind != EdgeKind::Precedes
                    || ![NodeId(2), NodeId(5)]
                        .iter()
                        .any(|id| edge.from == *id || edge.to == *id)
            });
            for (from, to) in [(1, 3), (4, 6)] {
                fixture.1.edges.push(GraphEdge {
                    from: NodeId(from),
                    to: NodeId(to),
                    kind: EdgeKind::Precedes,
                    sources: vec![],
                    basis: ViewBasis::NativeLayout,
                });
            }
            let prefixes: Vec<_> = [2, 5]
                .into_iter()
                .flat_map(|index| fixture.1.nodes[index].sources.iter().copied())
                .collect();
            let mut order = 0;
            for prefix in [true, false] {
                for glyph in &mut glyphs {
                    if prefixes.contains(&SourceRef::Native { glyph: glyph.id }) == prefix {
                        glyph.render_order = order;
                        order += 1;
                    }
                }
            }
        }
        fixture.0.native = Document::new(glyphs);
        fixture
    }
    let old = setup("Budget 10.  ", false);
    for mutation in 0..5 {
        let mut new = setup("Budget 20. ", true);
        if mutation == 3 {
            append_unassigned(&mut new, PageId(0), 70.0);
        } else if mutation != 0 {
            let mut glyphs = new.0.native.items().to_vec();
            let node = if mutation == 1 { 2 } else { 3 };
            let SourceRef::Native { glyph } = new.1.nodes[node].sources[0] else {
                unreachable!()
            };
            let glyph = glyphs
                .iter_mut()
                .find(|g| g.id == glyph)
                .expect("mutated fixture source");
            match mutation {
                1 => glyph.bbox.max.x = 50.0,
                2 => {
                    glyph.baseline.y += 0.01;
                    glyph.bbox.min.y += 0.01;
                    glyph.bbox.max.y += 0.01;
                }
                4 => glyph.path_clip_status = GlyphPathClipStatus::PartiallyOutside,
                _ => unreachable!(),
            }
            new.0.native = Document::new(glyphs);
        }
        let result = compare(&old, &new);
        let target = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .find(|review| {
                review.old_sources == old.1.nodes[3].sources[..10]
                    && review.new_sources == new.1.nodes[3].sources[..10]
            });
        assert_eq!(target.is_some(), mutation == 0, "mutation {mutation}");
        if let Some(review) = target {
            let cuts = review.source_cuts.as_ref().expect("source cut recovery");
            let pdfdelta_core::document::SourceCutPopulation::MatchedInterval {
                row_order: Some(order),
                ..
            } = &cuts.population
            else {
                panic!("row source-order certificate");
            };
            assert_eq!(order.convention, "horizontal-row-boundaries-v1");
            assert!(order.old.is_some() && order.new.is_some());
        }
    }
}

#[test]
fn paint_order_rows_keep_labels_after_boundaries_and_reject_unsafe_exclusions() {
    let setup = |old: bool, padded: bool| {
        let mut fixture = if old {
            fixture_rows(&["BEGIN", "101 ", "Budget 10.", "102 ", "END", "103 ", "STOP"])
        } else {
            fixture_rows(&[
                if padded { "BEGIN " } else { "BEGIN" },
                "Budget 20.",
                "END",
                "STOP",
            ])
        };
        let mut glyphs = fixture.0.native.items().to_vec();
        for (index, node) in fixture.1.nodes.iter().enumerate().skip(1) {
            let row = if old { (index - 1) / 2 } else { index - 1 };
            for source in &node.sources {
                let SourceRef::Native { glyph } = source else {
                    unreachable!()
                };
                let glyph = glyphs
                    .iter_mut()
                    .find(|g| g.id == *glyph)
                    .expect("fixture evidence");
                let x = if old && index % 2 == 0 { 0.0 } else { 60.0 };
                let y = 100.0 - row as f64 * 30.0 - glyph.baseline.y;
                glyph.baseline.x += x;
                glyph.bbox.min.x += x;
                glyph.bbox.max.x += x;
                glyph.baseline.y += y;
                glyph.bbox.min.y += y;
                glyph.bbox.max.y += y;
            }
        }
        fixture.0.native = Document::new(glyphs);
        fixture
    };
    let new = setup(false, false);
    for mutation in 0..10 {
        let mut old = setup(true, false);
        let target: Vec<_> = old.1.nodes[2..=4]
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        if mutation == 6 {
            append_unassigned(&mut old, PageId(0), 70.0);
        } else if mutation == 8 {
            old.0.inventories[0].complete = false;
        } else if mutation != 0 {
            let node = match mutation {
                3 | 9 => 6,
                7 => 5,
                _ => 2,
            };
            let SourceRef::Native { glyph } = old.1.nodes[node].sources[0] else {
                unreachable!()
            };
            let mut glyphs = old.0.native.items().to_vec();
            let glyph = glyphs
                .iter_mut()
                .find(|g| g.id == glyph)
                .expect("fixture evidence");
            match mutation {
                1 | 3 => glyph.bbox.max.x = 60.0,
                2 => glyph.render_order = 0,
                4 => glyph.baseline.y += 0.01,
                5 => glyph.path_clip_status = GlyphPathClipStatus::PartiallyOutside,
                9 => {
                    glyph.text = DecodedText::Unmapped {
                        font_hash: pdfdelta_core::model::FontProgramHash(vec![0; 32]),
                        glyph_id: 1,
                    }
                }
                7 => {
                    let y = f64::from_bits(glyph.baseline.y.to_bits() + 3);
                    let delta = y - glyph.baseline.y;
                    glyph.baseline.y = y;
                    glyph.bbox.min.y += delta;
                    glyph.bbox.max.y += delta;
                }
                _ => unreachable!(),
            }
            old.0.native = Document::new(glyphs);
        }
        for reversed in [false, true] {
            let comparison = if reversed {
                compare(&new, &old)
            } else {
                compare(&old, &new)
            };
            let review = comparison.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .find(|review| {
                    if reversed {
                        review.new_sources == target && review.old_sources == new.1.nodes[2].sources
                    } else {
                        review.old_sources == target && review.new_sources == new.1.nodes[2].sources
                    }
                });
            assert_eq!(
                review.is_some(),
                mutation == 0 || mutation == 7,
                "mutation {mutation}, reversed {reversed}"
            );
            if let Some(review) = review {
                let pdfdelta_core::document::SourceCutPopulation::MatchedInterval {
                    row_order: Some(order),
                    ..
                } = &review
                    .source_cuts
                    .as_ref()
                    .expect("fixture evidence")
                    .population
                else {
                    panic!("paint-order proof")
                };
                assert_eq!(order.convention, "horizontal-paint-row-boundaries-v1");
            }
        }
    }

    for merged in [vec![2], vec![2, 2], vec![4], vec![3, 3]] {
        for merge_new in [false, true] {
            let mut old = setup(true, false);
            let mut new = setup(false, false);
            let target: Vec<_> = old.1.nodes[2..=4]
                .iter()
                .flat_map(|node| node.sources.iter().copied())
                .collect();
            let new_target = new.1.nodes[2].sources.clone();
            for index in &merged {
                merge_following_node(&mut old, *index);
            }
            if merge_new {
                merge_following_node(&mut new, 2);
            }
            for reversed in [false, true] {
                let result = if reversed {
                    compare(&new, &old)
                } else {
                    compare(&old, &new)
                };
                assert!(
                    result.scopes[0]
                        .result
                        .text_scope_reviews
                        .iter()
                        .any(|review| {
                            if reversed {
                                review.old_sources == new_target && review.new_sources == target
                            } else {
                                review.old_sources == target && review.new_sources == new_target
                            }
                        }),
                    "merged nodes {merged:?}, new boundary {merge_new}, reversed {reversed}"
                );
            }
        }
    }

    let old = setup(true, false);
    let new = setup(false, true);
    let comparison = compare(&old, &new);
    let target: Vec<_> = old.1.nodes[2..=4]
        .iter()
        .flat_map(|node| node.sources.iter().copied())
        .collect();
    assert!(
        comparison.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| {
                review.old_sources == target && review.new_sources == new.1.nodes[2].sources
            }),
        "padded outer boundary retains its interior comparison"
    );
    for review in &comparison.scopes[0].result.text_scope_reviews {
        let Some(cuts) = &review.source_cuts else {
            continue;
        };
        let pdfdelta_core::document::SourceCutPopulation::MatchedInterval {
            row_order: Some(order),
            ..
        } = &cuts.population
        else {
            continue;
        };
        for (endpoints, body) in [
            (&order.old, &review.old_sources),
            (&order.new, &review.new_sources),
        ] {
            assert!(
                endpoints
                    .iter()
                    .flatten()
                    .flat_map(|end| &end.sources)
                    .all(|source| !body.contains(source)),
                "outer endpoint entered a paint interval"
            );
        }
    }
}

#[test]
fn finer_content_edges_preserve_the_trimmed_enclosing_paragraph() {
    let mut old = fixture_rows(&[
        "BEGIN",
        "An unchanged introduction. ",
        "Budget 10.  ",
        "END",
    ]);
    let mut new = fixture_rows(&["BEGIN", "An unchanged introduction. ", "Budget 20. ", "END"]);
    merge_following_node(&mut old, 2);
    merge_following_node(&mut new, 2);
    append_unassigned(&mut old, PageId(1), 70.0);
    append_unassigned(&mut new, PageId(1), 70.0);
    let result = compare(&old, &new);
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert!(reviews.iter().any(|review| {
        review.old_sources
            == old.1.nodes[2].sources[.."An unchanged introduction. Budget 10.".len()]
            && review.new_sources
                == new.1.nodes[2].sources[.."An unchanged introduction. Budget 20.".len()]
            && review
                .source_cuts
                .as_ref()
                .is_some_and(|cuts| cuts.edge_refinement.is_some())
    }));
}

#[test]
fn whole_node_intervals_can_retain_a_raw_parent_for_content_edge_refinement() {
    let old = fixture_rows(&["BEGIN", "Budget 10.  ", "END"]);
    let new = fixture_rows(&["BEGIN", "Budget 20.", "END"]);
    let original_old = &old.1.nodes[2].sources;
    let original_new = &new.1.nodes[2].sources;
    let result = compare(&old, &new);
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert!(reviews.iter().any(|review| review.source_cuts.is_none()
        && review.old_sources == *original_old
        && review.new_sources == *original_new));
    assert!(reviews.iter().any(|review| {
        review
            .source_cuts
            .as_ref()
            .is_some_and(|cuts| cuts.edge_refinement.is_some())
            && review.old_sources == original_old[.."Budget 10.".len()]
            && review.new_sources == *original_new
    }));
    assert!(reviews.iter().any(|review| {
        review
            .source_cuts
            .as_ref()
            .is_some_and(|cuts| cuts.edge_refinement.is_none())
            && review.old_sources == *original_old
            && review.new_sources == *original_new
    }));
}

#[test]
fn native_line_end_hyphens_do_not_manufacture_range_changes() {
    for spacing_repetitions in [0, 40] {
        for merge in [false, true] {
            for reverse in [false, true] {
                for independent_change in [false, true] {
                    let prefix = "a b ".repeat(spacing_repetitions);
                    let mut old = fixture_rows(&["BEGIN", &format!("{prefix}fix"), "END"]);
                    let mut new = fixture_rows(&[
                        "BEGIN",
                        &format!("{prefix}fi-"),
                        if independent_change { "z" } else { "x" },
                        "END",
                    ]);
                    if merge {
                        merge_following_node(&mut new, 2);
                    }
                    if spacing_repetitions != 0 {
                        reconstruct_interior_spaces(&mut old);
                        reconstruct_interior_spaces(&mut new);
                    }
                    let result = if reverse {
                        compare(&new, &old)
                    } else {
                        compare(&old, &new)
                    };
                    let reviews: Vec<_> = result
                        .scopes
                        .iter()
                        .flat_map(|scope| &scope.result.text_scope_reviews)
                        .collect();
                    assert_eq!(
                        reviews.is_empty(),
                        !independent_change,
                        "spacing={spacing_repetitions}, merge={merge}, reverse={reverse}: {reviews:?}"
                    );
                    if spacing_repetitions != 0 && independent_change {
                        assert!(
                            reviews.iter().any(|review| {
                                review.comparison.text_change_proof.is_some()
                                    && review.comparison.text_mask.is_none()
                            }),
                            "the spacing family must retain its independent count witness"
                        );
                    }
                    for review in reviews {
                        if let Some(proof) = &review.comparison.text_change_proof {
                            assert_ne!(proof.token, ComparableToken::Scalar('-'));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn suffix_cuts_keep_optional_hyphens_in_the_source_population() {
    use pdfdelta_core::document::CutEvidence;
    for duplicate in [false, true] {
        let mut old_rows = vec!["BEGIN", "Budget 10 segmen-", "Costs 30."];
        let mut new_rows = vec!["BEGIN", "Budget 20 segmen-", "Costs 40."];
        if duplicate {
            old_rows.push("Other segmen-");
            new_rows.push("Other segmen-");
        }
        old_rows.push("END");
        new_rows.push("END");
        let mut old = fixture_rows(&old_rows);
        let mut new = fixture_rows(&new_rows);
        for fixture in [&mut old, &mut new] {
            let end = fixture.1.nodes.len() - 1;
            for node in &mut fixture.1.nodes[2..end] {
                let NodeContent::Text { view } = &mut node.content else {
                    unreachable!()
                };
                view.normalization = TextNormalization::Unresolved {
                    reason: "retained line boundary requires native projection".into(),
                };
            }
        }
        let source_end = old.1.nodes[2].sources.len() - 1;
        let suffix = &old.1.nodes[2].sources[source_end - 6..source_end];
        let result = compare(&old, &new);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        let found = reviews
            .iter()
            .filter_map(|review| review.source_cuts.as_ref())
            .any(|cuts| {
                [&cuts.entry, &cuts.exit].iter().any(|cut| {
                    matches!(&cut.evidence, CutEvidence::UniqueNativeFragment { old, .. }
                    if old.sources == suffix)
                })
            });
        assert_eq!(found, !duplicate, "suffix uniqueness: {reviews:?}");
        if !duplicate {
            for (fixture, sources) in [
                (
                    &old,
                    reviews
                        .iter()
                        .flat_map(|review| review.old_sources.iter())
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                ),
                (
                    &new,
                    reviews
                        .iter()
                        .flat_map(|review| review.new_sources.iter())
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                ),
            ] {
                assert!(
                    fixture.1.nodes[2..4]
                        .iter()
                        .flat_map(|node| &node.sources)
                        .all(|source| sources.contains(source))
                );
            }
        }
    }
}

#[test]
fn a_contracted_hyphen_neighbor_preserves_independent_whole_range_changes() {
    let old = fixture("Value 10.");
    let mut new = fixture("Value 20.-  ");
    collapse_last_two_literal_spaces(&mut new.1.nodes[2]);
    let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
        unreachable!()
    };
    view.normalization = TextNormalization::Exact;
    for (old, new) in [(&old, &new), (&new, &old)] {
        let result = compare(old, new);
        assert!(
            result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.source_cuts.is_none()
                        && review.old_sources == old.1.nodes[2].sources
                        && review.new_sources == new.1.nodes[2].sources
                        && review.comparison.compared
                })
        );
    }
}

#[test]
fn a_literal_midline_hyphen_remains_a_native_content_change() {
    let old = fixture("fix");
    let new = fixture("fi-x");
    let result = compare(&old, &new);
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert_eq!(reviews.len(), 1);
    let mask = reviews[0]
        .comparison
        .text_mask
        .as_ref()
        .expect("literal hyphen has an exact source mask");
    assert_eq!(mask.claims.changed_source_lower, 1);
    assert_eq!(mask.claims.changed_source_upper, 1);
}

#[test]
fn interleaved_layout_tokens_do_not_prove_a_native_source_change() {
    for interleaved_paint in [false, true] {
        for reverse in [false, true] {
            for independent_change in [false, true] {
                let mut old =
                    fixture_rows(&["BEGIN", "abcd", "WXYZ", "MIDDLE", "Budget 10.", "END"]);
                let mut new = fixture_rows(&[
                    "BEGIN",
                    "abcd",
                    "WXYZ",
                    "MIDDLE",
                    if independent_change {
                        "Budget 20."
                    } else {
                        "Budget 10."
                    },
                    "END",
                ]);
                merge_following_node(&mut old, 2);
                merge_following_node(&mut new, 2);
                let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
                    unreachable!()
                };
                let original = view.clone();
                for (position, source) in [0, 4, 1, 5, 2, 6, 3, 7].into_iter().enumerate() {
                    view.tokens[position] = original.tokens[source].clone();
                    view.origins[position] = original.origins[source].clone();
                }
                if interleaved_paint {
                    let mut glyphs = new.0.native.items().to_vec();
                    let first = view.origins[0][0];
                    let SourceRef::Native { glyph: first } = first else {
                        unreachable!()
                    };
                    let base = glyphs
                        .iter()
                        .find(|glyph| glyph.id == first)
                        .expect("first interleaved glyph")
                        .render_order;
                    for (position, origins) in view.origins.iter().enumerate() {
                        let SourceRef::Native { glyph: id } = origins[0] else {
                            unreachable!()
                        };
                        glyphs
                            .iter_mut()
                            .find(|glyph| glyph.id == id)
                            .expect("interleaved glyph")
                            .render_order = base + position as u32;
                    }
                    new.0.native = Document::new(glyphs);
                }
                let control = old.1.nodes[2].sources.clone();
                let result = if reverse {
                    compare(&new, &old)
                } else {
                    compare(&old, &new)
                };
                let reviews: Vec<_> = result
                    .scopes
                    .iter()
                    .flat_map(|scope| &scope.result.text_scope_reviews)
                    .collect();
                assert!(
                    reviews.iter().all(|review| {
                        review
                            .old_sources
                            .iter()
                            .chain(&review.new_sources)
                            .all(|source| !control.contains(source))
                    }),
                    "reconstructed order cannot manufacture a change: {reviews:?}"
                );
                assert_eq!(
                    reviews.is_empty(),
                    !independent_change,
                    "an independent, source-ordered change must survive"
                );
            }
        }
    }
}

#[test]
fn raised_native_markers_retain_their_source_order() {
    let mut old = fixture("Reference1 continues.");
    let mut new = fixture("Reference2 continues.");
    for fixture in [&mut old, &mut new] {
        let SourceRef::Native { glyph: marker } = fixture.1.nodes[2].sources[9] else {
            unreachable!()
        };
        let mut glyphs = fixture.0.native.items().to_vec();
        let glyph = glyphs
            .iter_mut()
            .find(|glyph| glyph.id == marker)
            .expect("retained footnote marker");
        glyph.baseline.y += 3.0;
        glyph.bbox.min.y += 3.0;
        glyph.bbox.max.y += 3.0;
        fixture.0.native = Document::new(glyphs);
    }
    let result = compare(&old, &new);
    assert!(result.scopes.iter().any(|scope| {
        scope.result.text_scope_reviews.iter().any(|review| {
            review.source_cuts.is_none()
                && review.old_sources == old.1.nodes[2].sources
                && review.new_sources == new.1.nodes[2].sources
                && review.comparison.text_mask.is_none()
                && review.comparison.text_change_proof.is_some()
        })
    }));
}

#[test]
fn literal_space_projection_does_not_manufacture_a_whole_range_change() {
    let mut old = fixture("Value 10.  ");
    let new = fixture("Value 10.  ");
    collapse_last_two_literal_spaces(&mut old.1.nodes[2]);
    let NodeContent::Text { view } = &mut old.1.nodes[2].content else {
        unreachable!()
    };
    view.normalization = TextNormalization::Exact;
    let result = compare(&old, &new);
    assert!(
        result
            .scopes
            .iter()
            .all(|scope| scope.result.text_scope_reviews.is_empty())
    );
}

#[test]
fn native_context_outside_an_interval_keeps_its_adjacent_boundaries() {
    for (header, footer) in [(true, false), (false, true), (true, true)] {
        let mut old = fixture_rows(&["TITLE", "BEGIN", "Budget 10.  ", "END", "STOP", "FOOT"]);
        let mut new = fixture_rows(&["TITLE", "BEGIN", "Budget 20.", "END", "STOP", "FOOT"]);
        if header {
            old.1.nodes[1].kind = NodeKind::Header;
        }
        if footer {
            old.1.nodes[6].kind = NodeKind::Footer;
        }
        let old_extent = old.1.nodes[3].sources[.."Budget 10.".len()].to_vec();
        let new_extent = new.1.nodes[3].sources.clone();
        merge_following_node(&mut new, 3);
        assert!(
            compare(&old, &new).scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_some()
                    && review.old_sources == old_extent
                    && review.new_sources == new_extent),
            "header={header}, footer={footer}"
        );
        let mut omitted = old.clone();
        append_unassigned(&mut omitted, PageId(0), 25.0);
        assert!(
            compare(&omitted, &new).scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .all(|review| review.old_sources != old_extent || review.new_sources != new_extent)
        );
    }
}

#[test]
fn internal_source_cut_owns_only_the_proven_interior_of_a_shared_parent() {
    for padding in [false, true] {
        let mut old = fixture_rows(&[
            if padding {
                " First boundary."
            } else {
                "First boundary."
            },
            "filler",
            "INNER START",
            "Budget 10.",
            "INNER END",
            "tail",
            if padding {
                "Last boundary. "
            } else {
                "Last boundary."
            },
        ]);
        let mut new = fixture_rows(&[
            "First boundary.",
            "filler",
            "INNER START",
            "Budget 20.",
            "INNER END",
            "tail",
            "Last boundary.",
        ]);
        let old_body = old.1.nodes[4].sources.clone();
        let new_body = new.1.nodes[4].sources.clone();
        for fixture in [&mut old, &mut new] {
            for _ in 0..4 {
                merge_following_node(fixture, 2);
            }
            let node = &mut fixture.1.nodes[2];
            let NodeContent::Text { view } = &mut node.content else {
                unreachable!()
            };
            let boundary = "filler".len();
            view.tokens.insert(boundary, ComparableToken::Scalar(' '));
            view.origins.insert(
                boundary,
                vec![node.sources[boundary - 1], node.sources[boundary]],
            );
            view.source_backed.insert(boundary, false);
            append_unassigned(fixture, PageId(1), 0.0);
        }
        let mut split = new.clone();
        let parent = split.1.nodes[2].clone();
        let NodeContent::Text { view } = &parent.content else {
            unreachable!()
        };
        let cut = view
            .origins
            .iter()
            .position(|origins| origins == &[new_body[0]])
            .expect("body start");
        let mut work = 10_000;
        let left = pdfdelta_core::document::TextSourcePartition::new(&parent, 0..cut, &mut work)
            .expect("left partition")
            .selected_node()
            .expect("left view");
        let mut right = pdfdelta_core::document::TextSourcePartition::new(
            &parent,
            cut..view.tokens.len(),
            &mut work,
        )
        .expect("right partition")
        .selected_node()
        .expect("right view");
        right.id = NodeId(100);
        for edge in &mut split.1.edges {
            if edge.kind == EdgeKind::Precedes && edge.from == parent.id {
                edge.from = right.id;
            }
        }
        split.1.edges.push(GraphEdge {
            from: parent.id,
            to: right.id,
            kind: EdgeKind::Precedes,
            sources: vec![],
            basis: ViewBasis::NativeLayout,
        });
        split.1.edges.push(GraphEdge {
            from: NodeId(0),
            to: right.id,
            kind: EdgeKind::Contains,
            sources: vec![],
            basis: ViewBasis::NativeLayout,
        });
        split.1.nodes[2] = left;
        split.1.nodes.push(right);
        assert_eq!(new.0.native.items(), split.0.native.items());
        for right in [&new, &split] {
            let mut limits = DocumentComparisonLimits::default();
            // The complete parent exceeds this local comparison limit. Its
            // native census remains available to prove a smaller source cut.
            limits.local.max_tokens = 32;
            let result = compare_document_views(
                DocumentView {
                    evidence: &old.0,
                    graph: &old.1,
                },
                DocumentView {
                    evidence: &right.0,
                    graph: &right.1,
                },
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                limits,
                HierarchyLimits::default(),
            )
            .expect("bounded internal domain comparison");
            let encoded = serde_json::to_value(&result.scopes[0].result.native_text_intervals)
                .expect("interval report");
            let intervals = encoded.as_array().expect("intervals");
            assert!(
                intervals.iter().any(|interval| {
                    interval.get("cut_partition").is_some()
                        && interval["old_sources"]
                            == serde_json::to_value(&old_body).expect("old sources")
                        && interval["new_sources"]
                            == serde_json::to_value(&new_body).expect("new sources")
                }),
                "{encoded}"
            );
            for interval in intervals {
                let Some(partition) = interval.get("cut_partition") else {
                    continue;
                };
                for (side, fixture) in [("old", &old), ("new", right)] {
                    let owned: Vec<SourceRef> =
                        serde_json::from_value(interval[format!("{side}_sources")].clone())
                            .expect("owned sources");
                    for remainder in partition[format!("{side}_remainder")]
                        .as_array()
                        .expect("remainders")
                    {
                        let parent: NodeId =
                            serde_json::from_value(remainder["parent"].clone()).expect("parent");
                        let parent = fixture
                            .1
                            .nodes
                            .iter()
                            .find(|node| node.id == parent)
                            .expect("retained parent");
                        let rest: Vec<SourceRef> =
                            serde_json::from_value(remainder["sources"].clone())
                                .expect("remainder");
                        assert!(rest.iter().all(|source| !owned.contains(source)));
                        let mut partitioned: Vec<_> = owned
                            .iter()
                            .copied()
                            .filter(|source| parent.sources.contains(source))
                            .chain(rest)
                            .collect();
                        partitioned.sort();
                        let mut expected = parent.sources.clone();
                        expected.sort();
                        assert_eq!(partitioned, expected);
                    }
                }
            }
        }
    }
}

#[test]
fn padded_source_cut_boundaries_keep_their_spaces_outside_owned_content() {
    let mut old = fixture_with_boundaries("Earlier 10.", " First boundary.", "Last boundary. ");
    let mut new = fixture("Updated 20.");
    for fixture in [&mut old, &mut new] {
        append_unassigned(fixture, PageId(1), 0.0);
    }
    let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
        unreachable!()
    };
    view.normalization = TextNormalization::Unresolved {
        reason: "global layout normalization is not a raw reading".into(),
    };
    let result = compare(&old, &new);
    let encoded =
        serde_json::to_value(&result.scopes[0].result.native_text_intervals).expect("intervals");
    let interval = encoded
        .as_array()
        .expect("array")
        .iter()
        .find(|interval| interval.get("cut_partition").is_some())
        .expect("owned raw cut");
    assert_eq!(
        interval["old_sources"],
        serde_json::to_value(&old.1.nodes[2].sources).expect("old interior")
    );
    assert_eq!(
        interval["new_sources"],
        serde_json::to_value(&new.1.nodes[2].sources).expect("new interior")
    );
    for edge in ["entry", "exit"] {
        assert_eq!(
            interval["cut_partition"]["range"][edge]["evidence"]["kind"],
            "accepted_boundary"
        );
    }
}

#[test]
fn accepted_intervals_recheck_raw_sources_when_group_normalization_is_unresolved() {
    let mut old = fixture("Earlier 10.");
    let mut new = fixture("Updated 20.");
    for fixture in [&mut old, &mut new] {
        append_unassigned(fixture, PageId(1), 0.0);
    }
    let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
        unreachable!()
    };
    view.normalization = TextNormalization::Unresolved {
        reason: "global normalization retains competing interpretations".into(),
    };
    for (a, b) in [(&old, &new), (&new, &old)] {
        let result = compare(a, b);
        assert!(!result.scopes[0].result.native_text_intervals.is_empty());
        assert!(
            result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.source_cuts.is_some()
                        && review.old_sources == a.1.nodes[2].sources
                        && review.new_sources == b.1.nodes[2].sources
                })
        );
    }
    for fault in ["unmapped", "wrong-token", "unsafe-order"] {
        let mut bad = new.clone();
        match fault {
            "unmapped" => {
                let source = bad.1.nodes[2].sources[0];
                bad.0.native = bad.0.native.map_items(|mut glyph| {
                    if source == (SourceRef::Native { glyph: glyph.id }) {
                        glyph.text = DecodedText::Unmapped {
                            font_hash: pdfdelta_core::model::FontProgramHash(vec![0; 32]),
                            glyph_id: 1,
                        };
                    }
                    glyph
                });
            }
            "wrong-token" => {
                let NodeContent::Text { view } = &mut bad.1.nodes[2].content else {
                    unreachable!()
                };
                view.tokens[0] = ComparableToken::Scalar('X');
            }
            "unsafe-order" => {
                let source = bad.1.nodes[2].sources[0];
                bad.0.native = bad.0.native.map_items(|mut glyph| {
                    if source == (SourceRef::Native { glyph: glyph.id }) {
                        glyph.baseline.y = 110.0;
                    }
                    glyph
                });
            }
            _ => unreachable!(),
        }
        assert!(
            compare(&old, &bad).scopes[0]
                .result
                .text_scope_reviews
                .is_empty(),
            "{fault}"
        );
    }
    assert!(
        compare(&new, &new).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
}

#[test]
fn raw_intervals_reject_an_accepted_boundary_crossing_their_new_side() {
    let old = fixture_rows(&["BEGIN", "Earlier 10.", "END", "Moved boundary"]);
    let mut new = fixture_rows(&["BEGIN", "Moved boundary", "Updated 20.", "END"]);
    let NodeContent::Text { view } = &mut new.1.nodes[3].content else {
        unreachable!()
    };
    view.normalization = TextNormalization::Unresolved {
        reason: "global normalization retains competing interpretations".into(),
    };
    let result = compare(&old, &new);
    assert!(result.scopes[0].result.text_scope_reviews.is_empty());
}

#[test]
fn source_cuts_use_a_closed_matched_population_with_unrelated_pages() {
    let mut old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP"]);
    let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP"]);
    let old_extent = old.1.nodes[2].sources.clone();
    let new_extent = new.1.nodes[2].sources.clone();
    merge_following_node(&mut new, 2);
    for fixture in [&mut old, &mut new] {
        append_unassigned(fixture, PageId(1), 0.0);
        fixture
            .0
            .inventories
            .last_mut()
            .expect("unassigned page inventory")
            .complete = false;
    }
    let result = compare(&old, &new);
    let review = result.scopes[0]
        .result
        .text_scope_reviews
        .iter()
        .find(|review| review.source_cuts.is_some())
        .expect("finite source cut inside accepted outer boundaries");
    assert_eq!(review.old_sources, old_extent);
    assert_eq!(review.new_sources, new_extent);
    let population = &review
        .source_cuts
        .as_ref()
        .expect("source cut review")
        .population;
    assert!(
        matches!(population, pdfdelta_core::document::SourceCutPopulation::MatchedInterval { old, new, .. }
        if old == &[NodeId(1), NodeId(2), NodeId(3), NodeId(4)] && new == &[NodeId(1), NodeId(2), NodeId(4)])
    );
    assert!(
        compare(&old, &old).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
}

#[test]
fn outer_boundary_roundoff_requires_a_full_paint_order_proof() {
    for material_offset in [false, true] {
        let mut old = fixture_rows(&["BEGIN", "Budget 10. ", "END "]);
        let new = fixture_rows(&["BEGIN", "Budget 20.", "END"]);
        let mut old_extent = old.1.nodes[2].sources.clone();
        old_extent.pop();
        let new_extent = new.1.nodes[2].sources.clone();
        let SourceRef::Native { glyph: last } =
            *old.1.nodes[3].sources.last().expect("boundary space")
        else {
            unreachable!()
        };
        old.0.native = old.0.native.map_items(|mut glyph| {
            if glyph.id == last {
                glyph.baseline.y = if material_offset {
                    glyph.baseline.y + 0.01
                } else {
                    glyph.baseline.y.next_up().next_up().next_up()
                };
            }
            glyph
        });
        for reversed in [false, true] {
            let (a, b, old_sources, new_sources) = if reversed {
                (&new, &old, &new_extent, &old_extent)
            } else {
                (&old, &new, &old_extent, &new_extent)
            };
            let result = compare(a, b);
            let exact = result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.source_cuts.is_some()
                        && review.old_sources == *old_sources
                        && review.new_sources == *new_sources
                });
            assert_eq!(
                exact, !material_offset,
                "material offset={material_offset}, reversed={reversed}"
            );
        }
    }
}

#[test]
fn interleaved_native_pages_preserve_closure_and_late_unassigned_glyphs() {
    for omitted in [false, true] {
        let mut old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP"]);
        let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP"]);
        let old_extent = old.1.nodes[2].sources.clone();
        let new_extent = new.1.nodes[2].sources.clone();
        merge_following_node(&mut new, 2);
        for fixture in [&mut old, &mut new] {
            append_unassigned(fixture, PageId(1), 0.0);
            let mut glyphs = fixture.0.native.items().to_vec();
            let mut other_page = glyphs.pop().expect("other page glyph");
            other_page.id = GlyphId(900);
            fixture
                .0
                .inventories
                .last_mut()
                .expect("page inventory")
                .sources = vec![SourceRef::Native {
                glyph: other_page.id,
            }];
            glyphs.insert(3, other_page);
            fixture.0.native = Document::new(glyphs);
        }
        if omitted {
            append_unassigned(&mut new, PageId(0), 55.0);
        }
        let result = compare(&old, &new);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        if omitted {
            assert!(
                reviews.is_empty(),
                "a later page span contains an in-band omission"
            );
        } else {
            assert!(reviews.iter().any(|review| {
                review.old_sources == old_extent && review.new_sources == new_extent
            }));
        }
    }
}

#[test]
fn unanchored_pages_do_not_hide_same_page_omissions() {
    for omitted in [false, true] {
        let mut old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP", &"X".repeat(20_000)]);
        let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP", &"Y".repeat(20_000)]);
        let old_extent = old.1.nodes[2].sources.clone();
        let new_extent = new.1.nodes[2].sources.clone();
        for fixture in [&mut old, &mut new] {
            let last = fixture.1.nodes.last_mut().expect("unanchored paragraph");
            last.pages = vec![PageId(1)];
            let sources = last.sources.clone();
            let ids: std::collections::BTreeSet<_> = sources.iter().copied().collect();
            let mut glyphs = fixture.0.native.items().to_vec();
            for glyph in &mut glyphs {
                if ids.contains(&SourceRef::Native { glyph: glyph.id }) {
                    glyph.page = PageId(1);
                }
            }
            fixture.0.native = Document::new(glyphs);
            fixture.0.pages.push(PageEvidence {
                page: PageId(1),
                bounds: None,
            });
            fixture.0.inventories[0]
                .sources
                .retain(|source| !ids.contains(source));
            fixture.0.inventories.push(ChannelInventory {
                page: Some(PageId(1)),
                channel: Channel::Text,
                backend: 0,
                sources,
                complete: true,
            });
        }
        if omitted {
            append_unassigned(&mut new, PageId(0), 55.0);
            let mut glyphs = new.0.native.items().to_vec();
            glyphs.last_mut().expect("omitted glyph").id = GlyphId(50_000);
            new.0.native = Document::new(glyphs);
            *new.0.inventories[0]
                .sources
                .last_mut()
                .expect("omitted source") = SourceRef::Native {
                glyph: GlyphId(50_000),
            };
        }
        let limits = DocumentComparisonLimits::default();
        let result = compare_document_views(
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
        .expect("bounded local comparison");
        let recovered = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| review.old_sources == old_extent && review.new_sources == new_extent);
        assert_eq!(recovered, !omitted, "same-page omission={omitted}");
    }
}

#[test]
fn validated_native_index_keeps_unrelated_pages_out_of_local_search_work() {
    let mut old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP"]);
    let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP"]);
    let old_extent = old.1.nodes[2].sources.clone();
    let new_extent = new.1.nodes[2].sources.clone();
    merge_following_node(&mut new, 2);
    for fixture in [&mut old, &mut new] {
        append_unassigned(fixture, PageId(1), 0.0);
        let mut glyphs = fixture.0.native.items().to_vec();
        let template = glyphs.last().expect("unrelated page glyph").clone();
        let inventory = fixture.0.inventories.last_mut().expect("page inventory");
        for id in 2000..22_000 {
            let mut glyph = template.clone();
            glyph.id = GlyphId(id);
            inventory
                .sources
                .push(SourceRef::Native { glyph: glyph.id });
            glyphs.push(glyph);
        }
        fixture.0.native = Document::new(glyphs);
    }
    for budget in [0, 10_000] {
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_ownership_visits = budget;
        let result = compare_document_views(
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
        .expect("bounded evidence remains valid");
        let recovered = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| review.old_sources == old_extent && review.new_sources == new_extent);
        assert_eq!(recovered, budget != 0, "local work budget {budget}");
    }
    let mut limits = DocumentComparisonLimits::default().evidence;
    limits.max_items = 10_000;
    assert!(
        old.0.validate(limits).is_err(),
        "full evidence limits still apply"
    );
}

#[test]
fn page_paint_index_keeps_late_local_obstructions_and_skips_remote_paint_work() {
    for obstruction in 0..3 {
        let old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP"]);
        let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP"]);
        let old_extent = old.1.nodes[2].sources.clone();
        let new_extent = new.1.nodes[2].sources.clone();
        merge_following_node(&mut new, 2);
        append_unassigned(&mut new, PageId(1), 0.0);
        let disjoint = Rect {
            min: Vec2 { x: 0.0, y: 200.0 },
            max: Vec2 { x: 100.0, y: 210.0 },
        };
        with_paint(&mut new, Some(disjoint));
        let mut paints = new
            .0
            .native
            .non_text_paint_bounds()
            .expect("local paint bounds")
            .to_vec();
        let mut remote = paints[0].clone();
        remote.page = PageId(1);
        remote.bounds = None;
        for order in 1..20_001 {
            remote.render_order = order;
            paints.push(remote.clone());
        }
        let mut last = paints[0].clone();
        last.render_order = 20_001;
        last.bounds = match obstruction {
            0 => Some(disjoint),
            1 => Some(Rect {
                min: Vec2 { x: 0.0, y: 50.0 },
                max: Vec2 { x: 100.0, y: 60.0 },
            }),
            _ => None,
        };
        paints.push(last);
        new.0.native = new.0.native.clone().with_non_text_paint_bounds(paints);
        new.0
            .inventories
            .last_mut()
            .expect("remote page inventory")
            .complete = false;
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_ownership_visits = 10_000;
        let result = compare_document_views(
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
        .expect("bounded paint acquisition");
        let recovered = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| review.old_sources == old_extent && review.new_sources == new_extent);
        assert_eq!(recovered, obstruction == 0, "obstruction {obstruction}");
        limits.evidence.max_items = 10_000;
        assert!(new.0.validate(limits.evidence).is_err());
    }
}

#[test]
fn page_text_evidence_keeps_global_obligations_and_ignores_remote_issues() {
    for obligation in 0..5 {
        let old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP"]);
        let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP"]);
        let old_extent = old.1.nodes[2].sources.clone();
        let new_extent = new.1.nodes[2].sources.clone();
        merge_following_node(&mut new, 2);
        append_unassigned(&mut new, PageId(1), 0.0);
        with_paint(
            &mut new,
            Some(Rect {
                min: Vec2 { x: 0.0, y: 200.0 },
                max: Vec2 { x: 100.0, y: 210.0 },
            }),
        );
        let remote_issue = EvidenceIssue {
            page: Some(PageId(1)),
            channel: Channel::Text,
            sources: Vec::new(),
            boundary: None,
            kind: EvidenceFailure::Unresolved,
            reason: "unresolved remote-page acquisition".into(),
        };
        new.0.issues = vec![remote_issue.clone(); 2_000];
        match obligation {
            1 | 2 | 4 => {
                let mut issue = remote_issue;
                issue.page = (obligation == 1).then_some(PageId(0));
                if obligation == 4 {
                    issue.channel = Channel::Relations;
                }
                new.0.issues.push(issue);
            }
            3 => new.0.inventories.push(ChannelInventory {
                page: None,
                channel: Channel::Text,
                backend: 0,
                sources: Vec::new(),
                complete: false,
            }),
            _ => {}
        }
        let result = compare(&old, &new);
        let recovered = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| review.old_sources == old_extent && review.new_sources == new_extent);
        assert_eq!(
            recovered,
            obligation == 0 || obligation == 4,
            "obligation {obligation}"
        );
    }
}

#[test]
fn source_cut_subpaths_inherit_only_a_closed_outer_paint_band() {
    for mutation in 0..4 {
        let old = fixture_rows(&["BEGIN", "Budget 10.", "END", "STOP"]);
        let mut new = fixture_rows(&["BEGIN", "Budget 20.", "END", "STOP"]);
        merge_following_node(&mut new, 2);
        with_paint(
            &mut new,
            (mutation != 2).then_some(Rect {
                min: Vec2 {
                    x: 0.0,
                    y: if mutation == 1 { 50.0 } else { 200.0 },
                },
                max: Vec2 {
                    x: 100.0,
                    y: if mutation == 1 { 60.0 } else { 210.0 },
                },
            }),
        );
        if mutation == 3 {
            append_unassigned(&mut new, PageId(0), 55.0);
        }
        let result = compare(&old, &new);
        let cuts: Vec<_> = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .filter(|review| review.source_cuts.is_some())
            .collect();
        assert_eq!(
            cuts.len(),
            usize::from(mutation == 0),
            "mutation {mutation}"
        );
        if let Some(review) = cuts.first() {
            assert_eq!(review.old_sources, old.1.nodes[2].sources);
            assert_eq!(review.new_sources.len(), "Budget 20.".len());
        }
    }
}

#[test]
fn private_use_font_characters_do_not_prove_native_content_changes() {
    let old = fixture("0 ≤ n");
    for scalar in [
        '\u{e000}',
        '\u{f0a3}',
        '\u{f8ff}',
        '\u{f0000}',
        '\u{ffffd}',
        '\u{100000}',
        '\u{10fffd}',
    ] {
        let new = fixture(&format!("0 {scalar} n"));
        let raw = new.0.native.items().to_vec();
        assert!(
            compare(&old, &new)
                .scopes
                .iter()
                .all(|scope| scope.result.text_scope_reviews.is_empty()),
            "{scalar:?}"
        );
        assert_eq!(new.0.native.items(), raw);
    }
    assert!(
        !compare(&old, &fixture("0 < n")).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
}

#[test]
fn unmapped_font_identity_changes_are_not_native_content_changes() {
    use pdfdelta_core::model::FontProgramHash;
    let mut old = fixture("{abc}");
    let mut new = fixture("{abc}");
    for (side, fixture) in [&mut old, &mut new].into_iter().enumerate() {
        let node = &mut fixture.1.nodes[2];
        let NodeContent::Text { view } = &mut node.content else {
            unreachable!()
        };
        let mut glyphs = fixture.0.native.items().to_vec();
        for index in [0, 4] {
            let font_hash = FontProgramHash(vec![side as u8; 32]);
            let glyph_id = 102 + index as u16;
            view.tokens[index] = ComparableToken::Unmapped {
                font_hash: font_hash.clone(),
                glyph_id,
            };
            let SourceRef::Native { glyph: id } = node.sources[index] else {
                unreachable!()
            };
            glyphs
                .iter_mut()
                .find(|glyph| glyph.id == id)
                .expect("retained brace glyph")
                .text = DecodedText::Unmapped {
                font_hash,
                glyph_id,
            };
        }
        fixture.0.native = Document::new(glyphs);
    }
    assert!(
        compare(&old, &new).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
    assert_eq!(old.0.native.items().len(), new.0.native.items().len());
    assert!(
        old.0
            .native
            .items()
            .iter()
            .any(|glyph| matches!(glyph.text, DecodedText::Unmapped { .. }))
    );
}

#[test]
fn closed_intervals_support_local_presence_with_whole_and_source_cut_boundaries() {
    use pdfdelta_core::document::{PresenceSide, TypedOperation};
    for text in ["New paragraph.", " "] {
        let old = fixture_rows(&["BEGIN", "END"]);
        let new = fixture_rows(&["BEGIN", text, "END"]);
        let extent = new.1.nodes[2].sources.clone();
        let mut merged = new.clone();
        merge_following_node(&mut merged, 2);
        for new in [&new, &merged] {
            for (a, b, present) in [
                (&old, new, PresenceSide::New),
                (new, &old, PresenceSide::Old),
            ] {
                let result = compare(a, b);
                let reviews = &result.scopes[0].result.text_scope_reviews;
                assert_eq!(
                    reviews.len(),
                    1,
                    "text {text:?}, present {present:?}, nodes {}, search {:?}, accepted {:?}",
                    new.1.nodes.len(),
                    result.scopes[0].result.source_cut_search,
                    result.scopes[0].result.accepted_correspondences
                );
                let review = &reviews[0];
                assert_eq!(
                    review
                        .presence
                        .as_ref()
                        .expect("closed interval presence")
                        .present,
                    present
                );
                let (old_text, new_text) = if present == PresenceSide::New {
                    assert!(review.old_sources.is_empty());
                    assert_eq!(review.new_sources, extent);
                    ("", text)
                } else {
                    assert_eq!(review.old_sources, extent);
                    assert!(review.new_sources.is_empty());
                    (text, "")
                };
                assert_eq!(
                    review.comparison.operation,
                    Some(TypedOperation::TextChanged {
                        old: Some(old_text.into()),
                        new: Some(new_text.into())
                    })
                );
            }
        }
    }
}

#[test]
fn local_presence_requires_a_closed_empty_side_and_independent_endpoints() {
    for mutation in 0..3 {
        let mut old = fixture_rows(&["BEGIN", "END"]);
        let new = if mutation == 2 {
            fixture_rows(&["BEGIN", "X", "END", "END"])
        } else {
            fixture_rows(&["BEGIN", "X", "END"])
        };
        match mutation {
            0 => old.0.inventories[0].complete = false,
            1 => append_unassigned(&mut old, PageId(0), 85.0),
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
fn local_presence_does_not_claim_document_wide_novelty_of_a_copy() {
    let old = fixture_rows(&["COPY", "BEGIN", "END"]);
    let new = fixture_rows(&["COPY", "BEGIN", "COPY", "END"]);
    let result = compare(&old, &new);
    let review = result.scopes[0]
        .result
        .text_scope_reviews
        .iter()
        .find(|review| review.presence.is_some())
        .expect("absence only inside the closed interval");
    assert!(review.old_sources.is_empty());
    assert_eq!(review.new_sources, new.1.nodes[3].sources);
    assert_eq!(
        review
            .presence
            .as_ref()
            .expect("interval presence")
            .convention,
        "closed-native-interval-presence-v1"
    );
}

#[test]
fn source_cuts_preserve_exact_extent_under_raw_evidence_partition_changes() {
    let old = fixture_with_boundaries("Budget 10.", "BEGIN", "END");
    let new = fixture_with_boundaries("Budget 20.", "BEGIN", "END");
    assert_eq!(
        compare(&old, &new).scopes[0]
            .result
            .text_scope_reviews
            .len(),
        1
    );
    let mut merged_old = old.clone();
    let mut merged_new = new.clone();
    for fixture in [&mut merged_old, &mut merged_new] {
        merge_last_boundary(fixture);
    }
    assert_eq!(old.0.native.items(), merged_old.0.native.items());
    assert_eq!(new.0.native.items(), merged_new.0.native.items());
    for (a, b) in [
        (&old, &merged_new),
        (&merged_old, &new),
        (&merged_old, &merged_new),
    ] {
        let result = compare(a, b);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        assert_eq!(reviews.len(), 1);
        assert!(reviews[0].source_cuts.is_some());
        assert_eq!(reviews[0].old_sources, old.1.nodes[2].sources);
        assert_eq!(reviews[0].new_sources, new.1.nodes[2].sources);
        assert_eq!(
            reviews[0].comparison.operation,
            Some(pdfdelta_core::document::TypedOperation::TextChanged {
                old: Some("Budget 10.".into()),
                new: Some("Budget 20.".into()),
            })
        );
    }
    assert!(
        compare(&old, &merged_old).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
}

#[test]
fn source_cut_population_rejects_copies_unknown_sources_and_unsafe_order() {
    for mutation in 0..5 {
        let mut old = fixture_with_boundaries("Budget 10.", "BEGIN", "END");
        let mut new = fixture_with_boundaries(
            if mutation == 0 {
                "Budget 20. END"
            } else {
                "Budget 20."
            },
            "BEGIN",
            "END",
        );
        merge_last_boundary(&mut old);
        merge_last_boundary(&mut new);
        match mutation {
            0 => {}
            1 => new.0.inventories[0].complete = false,
            2 => append_unassigned(&mut new, PageId(0), 10.0),
            3 => {
                let mut glyphs = new.0.native.items().to_vec();
                // Reverse two tokens' physical positions without changing text.
                let x = glyphs[5].baseline.x;
                glyphs[5].baseline.x = glyphs[6].baseline.x;
                glyphs[6].baseline.x = x;
                new.0.native = Document::new(glyphs);
            }
            4 => {
                let NodeContent::Text { view } = &mut new.1.nodes[2].content else {
                    unreachable!()
                };
                view.normalization = TextNormalization::Unresolved {
                    reason: "unproved token projection".into(),
                };
            }
            _ => unreachable!(),
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
fn an_anchored_complete_page_can_supply_a_missing_second_outer_boundary() {
    let mut old = fixture_rows(&[
        "Abstract",
        "  Clients advertise capabilities.",
        "Earlier section",
    ]);
    let mut new = fixture_rows(&[
        "Abstract",
        "  Requestors advertise capabilities.",
        "Later section",
    ]);
    for fixture in [&mut old, &mut new] {
        append_unassigned(fixture, PageId(1), 70.0);
    }
    let expected = |fixture: &Fixture| fixture.1.nodes[2].sources[2..].to_vec();
    for (a, b) in [(&old, &new), (&new, &old)] {
        let result = compare(a, b);
        assert!(
            result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.old_sources == expected(a) && review.new_sources == expected(b)
                })
        );
    }
    assert!(
        compare(&old, &old).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
}

#[test]
fn page_edges_recover_body_outside_multiple_accepted_anchors() {
    for before in [false, true] {
        let mut old_rows = vec![
            "Status",
            "Memo",
            "Abstract",
            "  Clients advertise capabilities.",
            "Earlier section",
        ];
        let mut new_rows = vec![
            "Status",
            "Memo",
            "Abstract",
            "  Requestors advertise capabilities.",
            "Later section",
        ];
        if before {
            old_rows.reverse();
            new_rows.reverse();
        }
        let mut old = fixture_rows(&old_rows);
        let mut new = fixture_rows(&new_rows);
        for fixture in [&mut old, &mut new] {
            append_unassigned(fixture, PageId(1), 70.0);
        }
        let index = if before { 2 } else { 4 };
        for (a, b) in [(&old, &new), (&new, &old)] {
            let result = compare(a, b);
            let review = result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .find(|review| {
                    review.old_sources == a.1.nodes[index].sources[2..]
                        && review.new_sources == b.1.nodes[index].sources[2..]
                })
                .expect("changed body beyond outermost accepted page anchor");
            let pdfdelta_core::document::SourceCutPopulation::AnchoredPage { edge, .. } =
                &review.source_cuts.as_ref().expect("source cuts").population
            else {
                panic!("page population");
            };
            assert_eq!(
                *edge,
                Some(if before {
                    pdfdelta_core::document::SourceCutPageEdge::Before
                } else {
                    pdfdelta_core::document::SourceCutPageEdge::After
                })
            );
        }
    }
}

#[test]
fn page_edge_occurrence_census_keeps_pruned_context() {
    for before in [false, true] {
        let mut old_rows = vec![
            "Former capabilities.",
            "Status",
            "Abstract",
            "  Clients advertise capabilities.",
            "Earlier section",
        ];
        let mut new_rows = vec![
            "Recent capabilities.",
            "Status",
            "Abstract",
            "  Requestors advertise capabilities.",
            "Later section",
        ];
        if before {
            old_rows.reverse();
            new_rows.reverse();
        }
        let mut old = fixture_rows(&old_rows);
        let mut new = fixture_rows(&new_rows);
        for fixture in [&mut old, &mut new] {
            append_unassigned(fixture, PageId(1), 70.0);
        }
        let index = if before { 2 } else { 4 };
        for (a, b) in [(&old, &new), (&new, &old)] {
            let result = compare(a, b);
            // The other page side contains a second physical copy of this
            // suffix even though it cannot supply candidates for this edge.
            let body = &a.1.nodes[index];
            let repeated_suffix = &body.sources[body.sources.len() - "capabilities.".len()..];
            for cuts in result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .filter_map(|review| review.source_cuts.as_ref())
            {
                for boundary in [&cuts.entry, &cuts.exit].into_iter().chain(
                    cuts.edge_refinement
                        .iter()
                        .flat_map(|refinement| &refinement.enclosing),
                ) {
                    if let pdfdelta_core::document::CutEvidence::UniqueNativeFragment {
                        old: fragment,
                        ..
                    } = &boundary.evidence
                    {
                        assert!(fragment.node != body.id || fragment.sources != repeated_suffix);
                    }
                }
            }
        }
    }
}

#[test]
fn page_edges_do_not_bypass_source_closure_or_interior_boundaries() {
    for mutation in 0..5 {
        let mut old = fixture_rows(&[
            "Status",
            "Abstract",
            "  Clients advertise capabilities.",
            "Earlier section",
        ]);
        let mut new = fixture_rows(&[
            "Status",
            "Abstract",
            "  Requestors advertise capabilities.",
            "Later section",
        ]);
        append_unassigned(&mut old, PageId(1), 70.0);
        if mutation != 1 {
            append_unassigned(&mut new, PageId(1), 70.0);
        }
        match mutation {
            0 => new.0.inventories[0].complete = false,
            1 => {
                append_unassigned(&mut new, PageId(0), 10.0);
                new.0.pages.push(PageEvidence {
                    page: PageId(1),
                    bounds: None,
                });
            }
            2 => with_paint(&mut new, None),
            3 => {
                // A later accepted boundary makes this an interior comparison.
                old = fixture_rows(&[
                    "Status",
                    "Abstract",
                    "  Clients advertise capabilities.",
                    "Conclusion",
                ]);
                new = fixture_rows(&[
                    "Status",
                    "Abstract",
                    "  Requestors advertise capabilities.",
                    "Conclusion",
                ]);
                append_unassigned(&mut old, PageId(1), 70.0);
                append_unassigned(&mut new, PageId(1), 70.0);
            }
            4 => {
                new = fixture_rows(&[
                    "Abstract",
                    "Status",
                    "  Requestors advertise capabilities.",
                    "Later section",
                ]);
                append_unassigned(&mut new, PageId(1), 70.0);
            }
            _ => unreachable!(),
        }
        let result = compare(&old, &new);
        assert!(
            !result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.old_sources == old.1.nodes[3].sources[2..]
                        && review.new_sources == new.1.nodes[3].sources[2..]
                        && matches!(
                            review.source_cuts.as_ref().map(|cuts| &cuts.population),
                            Some(pdfdelta_core::document::SourceCutPopulation::AnchoredPage { .. })
                        )
                }),
            "mutation {mutation}"
        );
    }
}

#[test]
fn anchored_page_ranges_can_include_unchanged_interior_rows() {
    let mut old = fixture_rows(&[
        "Abstract",
        "Clients advertise ",
        "capabilities.",
        "Earlier section",
    ]);
    let mut new = fixture_rows(&[
        "Abstract",
        "Requestors advertise ",
        "capabilities.",
        "Later section",
    ]);
    for fixture in [&mut old, &mut new] {
        merge_following_node(fixture, 2);
        append_unassigned(fixture, PageId(1), 70.0);
    }
    let expected = |fixture: &Fixture| fixture.1.nodes[2].sources.clone();
    let result = compare(&old, &new);
    assert!(
        result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .any(|review| {
                review.old_sources == expected(&old) && review.new_sources == expected(&new)
            })
    );
}

#[test]
fn anchored_page_ranges_require_an_anchor_and_complete_safe_source_population() {
    for mutation in 0..5 {
        let mut old = fixture_rows(&[
            "Abstract",
            "  Clients advertise capabilities.",
            "Earlier section",
        ]);
        let mut new = fixture_rows(&[
            if mutation == 0 {
                "Changed heading"
            } else {
                "Abstract"
            },
            "  Requestors advertise capabilities.",
            if mutation == 3 {
                "Later capabilities."
            } else {
                "Later section"
            },
        ]);
        append_unassigned(&mut old, PageId(1), 70.0);
        if mutation == 2 {
            append_unassigned(&mut new, PageId(0), 10.0);
            new.0.pages.push(PageEvidence {
                page: PageId(1),
                bounds: None,
            });
        } else {
            append_unassigned(&mut new, PageId(1), 70.0);
        }
        match mutation {
            1 => new.0.inventories[0].complete = false,
            4 => with_paint(&mut new, None),
            _ => {}
        }
        let result = compare(&old, &new);
        assert!(
            !result.scopes[0]
                .result
                .text_scope_reviews
                .iter()
                .any(|review| {
                    review.old_sources == old.1.nodes[2].sources[2..]
                        && review.new_sources == new.1.nodes[2].sources[2..]
                }),
            "mutation {mutation}"
        );
    }
}

#[test]
fn anchored_page_fallback_excludes_a_heading_moved_to_another_page() {
    let mut old = fixture_rows(&[
        "Moved heading",
        "Abstract",
        "  Clients advertise capabilities.",
        "Earlier section",
    ]);
    let mut new = fixture_rows(&[
        "Abstract",
        "  Requestors advertise capabilities.",
        "Later section",
        "Moved heading",
    ]);
    append_unassigned(&mut old, PageId(1), 70.0);
    let moved = new.1.nodes[4].sources.clone();
    new.1.nodes[4].pages = vec![PageId(1)];
    let mut glyphs = new.0.native.items().to_vec();
    for glyph in &mut glyphs {
        if moved.contains(&SourceRef::Native { glyph: glyph.id }) {
            glyph.page = PageId(1);
        }
    }
    new.0.native = Document::new(glyphs);
    new.0.pages.push(PageEvidence {
        page: PageId(1),
        bounds: None,
    });
    new.0.inventories[0]
        .sources
        .retain(|source| !moved.contains(source));
    new.0.inventories.push(ChannelInventory {
        page: Some(PageId(1)),
        channel: Channel::Text,
        backend: 0,
        sources: moved,
        complete: true,
    });
    for (a, b, old_index, new_index) in [(&old, &new, 3, 2), (&new, &old, 2, 3)] {
        let result = compare(a, b);
        let review = result.scopes[0]
            .result
            .text_scope_reviews
            .iter()
            .find(|review| {
                review.old_sources == a.1.nodes[old_index].sources[2..]
                    && review.new_sources == b.1.nodes[new_index].sources[2..]
            })
            .expect("local body after a moved heading");
        let pdfdelta_core::document::SourceCutPopulation::AnchoredPage {
            external_boundaries,
            ..
        } = &review.source_cuts.as_ref().expect("source cuts").population
        else {
            panic!("anchored page proof");
        };
        assert_eq!(external_boundaries.len(), 1);
        let external = &result.scopes[0].result.candidates.proposals[external_boundaries[0]];
        assert!(
            !review
                .comparison
                .old
                .iter()
                .any(|node| external.old.contains(node))
        );
        assert!(
            !review
                .comparison
                .new
                .iter()
                .any(|node| external.new.contains(node))
        );
    }
}

/// Replace drawn interior spaces with layout tokens whose two real neighbors
/// retain the original geometric gap. No space glyph remains in the inventory.
fn reconstruct_interior_spaces(fixture: &mut Fixture) {
    let node = &mut fixture.1.nodes[2];
    let NodeContent::Text { view } = &mut node.content else {
        panic!("fixture interior must be text");
    };
    let mut removed = Vec::new();
    for position in 1..view.tokens.len().saturating_sub(1) {
        if view.tokens[position] != ComparableToken::Scalar(' ') {
            continue;
        }
        removed.extend(view.origins[position].iter().copied());
        view.origins[position] = vec![view.origins[position - 1][0], view.origins[position + 1][0]];
        view.source_backed[position] = false;
    }
    node.sources.retain(|source| !removed.contains(source));
    let glyphs = fixture
        .0
        .native
        .items()
        .iter()
        .filter(|glyph| !removed.contains(&SourceRef::Native { glyph: glyph.id }))
        .cloned()
        .collect();
    fixture.0.native = Document::new(glyphs);
    for inventory in &mut fixture.0.inventories {
        inventory.sources.retain(|source| !removed.contains(source));
    }
}

#[test]
fn reconstructed_spaces_do_not_erase_independent_source_changes() {
    for (old_text, new_text, changed) in [
        ("a b", "ab", false),
        ("a b", "a b", false),
        ("The file is now here.", "The file is nowhere.", false),
        ("a b 10", "ab 20", true),
    ] {
        let mut old = fixture(old_text);
        reconstruct_interior_spaces(&mut old);
        let new = fixture(new_text);
        for (a, b) in [(&old, &new), (&new, &old)] {
            let comparison = compare(a, b);
            let reviews = &comparison.scopes[0].result.text_scope_reviews;
            assert_eq!(
                reviews.len(),
                usize::from(changed),
                "{old_text:?} -> {new_text:?}"
            );
            if let Some(review) = reviews.first() {
                assert!(
                    review
                        .comparison
                        .unresolved
                        .iter()
                        .any(|reason| reason.contains("spacing"))
                );
            }
        }
    }
}

#[test]
fn native_multiplicity_reservation_preserves_a_near_budget_exact_mask() {
    let old = fixture(&"a".repeat(737));
    let new = fixture(&"b".repeat(654));
    let result = compare(&old, &new);
    let review = result.scopes[0]
        .result
        .text_scope_reviews
        .iter()
        .find(|review| {
            review.old_sources == old.1.nodes[2].sources
                && review.new_sources == new.1.nodes[2].sources
        })
        .expect("full source-backed range");
    let mask = review.comparison.text_mask.as_ref().expect("exact mask");
    assert_eq!(mask.claims.changed_source_lower, 1391);
    assert_eq!(mask.claims.changed_source_upper, 1391);
}

#[test]
fn long_exact_native_ranges_retain_a_change_when_mask_work_is_exhausted() {
    let old = fixture(&"a".repeat(800));
    let new = fixture(&"b".repeat(800));
    let result = compare(&old, &new);
    let review = result.scopes[0]
        .result
        .text_scope_reviews
        .iter()
        .find(|review| {
            review.old_sources == old.1.nodes[2].sources
                && review.new_sources == new.1.nodes[2].sources
        })
        .expect("full source-backed range");
    assert!(review.comparison.compared);
    assert!(review.comparison.text_mask.is_none());
    assert!(review.comparison.text_change_proof.is_some());
    assert!(result.scopes[0].result.native_text_intervals.is_empty());
    assert!(
        compare(&old, &old).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
    let mut unknown = new.clone();
    if let NodeContent::Text { view } = &mut unknown.1.nodes[2].content {
        view.normalization = TextNormalization::Unresolved {
            reason: "unproved projection".into(),
        };
        view.tokens[0] = ComparableToken::Scalar('c');
    }
    assert!(
        compare(&old, &unknown).scopes[0]
            .result
            .text_scope_reviews
            .is_empty()
    );
}

#[test]
fn large_spacing_family_retains_a_proved_change_without_a_false_exact_mask() {
    let mut old = fixture(&format!("{}10", "a b ".repeat(40)));
    let mut new = fixture(&format!("{}20", "a b ".repeat(40)));
    reconstruct_interior_spaces(&mut old);
    reconstruct_interior_spaces(&mut new);
    let result = compare(&old, &new);
    let reviews = &result.scopes[0].result.text_scope_reviews;
    assert_eq!(reviews.len(), 1);
    assert!(reviews[0].comparison.compared);
    assert!(reviews[0].comparison.text_mask.is_none());
    assert!(result.scopes[0].result.native_text_intervals.is_empty());
    assert!(
        reviews[0]
            .comparison
            .unresolved
            .iter()
            .any(|reason| reason.contains("multiplicity"))
    );
    let mut fewer_spaces = fixture(&format!("{}10", "ab ".repeat(40)));
    reconstruct_interior_spaces(&mut fewer_spaces);
    let literal_spaces = fixture(&format!("{}10", "a b ".repeat(40)));
    assert!(
        compare(&fewer_spaces, &literal_spaces).scopes[0]
            .result
            .text_scope_reviews
            .is_empty(),
        "implicit layout gaps cannot make a literal-space count a content witness"
    );
}

#[test]
fn missing_layout_adjacency_needs_monotone_sources_and_a_closed_band() {
    for mutation in 0..6 {
        let old = fixture("a");
        let mut new = fixture("aa");
        new.1
            .edges
            .retain(|edge| !(edge.kind == EdgeKind::Precedes && edge.from == NodeId(1)));
        match mutation {
            1 => append_unassigned(&mut new, PageId(0), 80.0),
            2 => {
                let mut glyphs = new.0.native.items().to_vec();
                glyphs["First boundary.".len()].render_order = 0;
                new.0.native = Document::new(glyphs);
            }
            3 => new.1.edges.push(GraphEdge {
                from: NodeId(1),
                to: NodeId(3),
                kind: EdgeKind::Precedes,
                sources: vec![],
                basis: ViewBasis::NativeLayout,
            }),
            4 => {
                append_unassigned(&mut new, PageId(0), 80.0);
                let mut glyphs = new.0.native.items().to_vec();
                let margin = glyphs.last_mut().expect("margin glyph");
                margin.bbox.min.x = -50.0;
                margin.bbox.max.x = -40.0;
                margin.baseline.x = -50.0;
                margin.direction = Vec2 { x: 0.0, y: 1.0 };
                new.0.native = Document::new(glyphs);
            }
            5 => {
                let mut glyphs = new.0.native.items().to_vec();
                let count = glyphs.len() as u32;
                for glyph in &mut glyphs {
                    glyph.render_order = count - glyph.render_order;
                }
                new.0.native = Document::new(glyphs);
            }
            _ => {}
        }
        let result = compare(&old, &new);
        let reviews = &result.scopes[0].result.text_scope_reviews;
        assert_eq!(
            reviews.len(),
            usize::from(matches!(mutation, 0 | 4 | 5)),
            "mutation {mutation}"
        );
        if let Some(review) = reviews.first() {
            assert_eq!(review.old_sources.len(), 1);
            assert_eq!(review.new_sources.len(), 2);
        }
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
fn bounded_failed_invocation_closes_no_more_than_its_disjoint_local_band() {
    for mutation in 0..5 {
        let old = fixture("a");
        let mut new = fixture("aa");
        with_paint(
            &mut new,
            Some(Rect {
                min: Vec2 {
                    x: 0.0,
                    y: if mutation == 1 { 80.0 } else { 120.0 },
                },
                max: Vec2 { x: 200.0, y: 140.0 },
            }),
        );
        new.0.issues.push(EvidenceIssue {
            page: Some(PageId(0)),
            channel: Channel::Text,
            sources: vec![],
            boundary: Some(EvidenceBoundary::PageGlyphGap {
                page: PageId(0),
                retained_before: 0,
                before: None,
                after: Some(new.0.native.items()[0].id),
                paint_index: (mutation != 2).then_some(if mutation == 3 { 1 } else { 0 }),
            }),
            kind: EvidenceFailure::Unsupported,
            reason: "failed Form invocation with declared clipping bounds".into(),
        });
        if mutation == 4 {
            new.0.issues[0].boundary = None;
        }
        if mutation == 3 {
            assert!(new.0.validate(Default::default()).is_err());
            continue;
        }
        let result = compare(&old, &new);
        let scope = &result.scopes[0].result;
        assert_eq!(
            scope.text_scope_reviews.len(),
            usize::from(mutation == 0),
            "mutation {mutation}"
        );
        assert!(!new.0.inventory_complete(Some(PageId(0)), Channel::Text));
        assert!(
            scope
                .text_scope_reviews
                .iter()
                .all(|review| review.convention == "closed-native-paint-bounds-interval-v1")
        );
    }
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
fn validated_paint_populations_preserve_native_membership_and_provider_obligations() {
    for mutation in 0..4 {
        let old = fixture("a");
        let mut new = fixture("aa");
        with_paint(
            &mut new,
            Some(Rect {
                min: Vec2 { x: 0.0, y: 120.0 },
                max: Vec2 { x: 200.0, y: 140.0 },
            }),
        );
        new.0.inventories[0].sources.reverse();
        match mutation {
            1 => {
                new.0.structured.push(StructuredEvidence {
                    id: 0,
                    page: Some(PageId(0)),
                    bounds: None,
                    object: None,
                    backend: 0,
                    value: StructuredValue::StructureElement {
                        role: "Span".into(),
                        identifier: None,
                        text: None,
                        glyphs: Vec::new(),
                        content: None,
                        parent: None,
                        order: None,
                    },
                });
                // Equal length does not prove an all-native population.
                new.0.inventories[0].sources[0] = SourceRef::Structured { element: 0 };
            }
            2 => new.0.inventories[0].page = None,
            3 => {
                let mut backend = new.0.backends[0].clone();
                backend.name = "second-native-provider".into();
                new.0.backends.push(backend);
                let mut inventory = new.0.inventories[0].clone();
                inventory.backend = 1;
                inventory.sources.pop();
                new.0.inventories.push(inventory);
            }
            _ => {}
        }
        new.0
            .validate(Default::default())
            .expect("valid scoped evidence");
        assert!(!new.0.inventory_complete(Some(PageId(0)), Channel::Text));
        assert_eq!(
            compare(&old, &new).scopes[0]
                .result
                .text_scope_reviews
                .len(),
            usize::from(mutation == 0),
            "population mutation {mutation}"
        );
    }
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
fn native_domain_ownership_requires_local_closure_and_indivisible_sources() {
    for case in [
        "plain",
        "unknown_paint",
        "incomplete",
        "overlap",
        "remote_paint",
        "omitted",
        "inferred",
        "shared_glyph",
        "changed_native_text",
        "reordered_native",
        "partial_native_text",
    ] {
        let old = fixture("a");
        let mut new = fixture_with_boundaries("aa", "First boundary. ", "Last boundary. ");
        let expected = match case {
            "plain" => 2,
            "unknown_paint" => {
                with_paint(&mut new, None);
                0
            }
            "changed_native_text" | "reordered_native" | "partial_native_text" => {
                let mut glyphs = new.0.native.items().to_vec();
                match case {
                    "changed_native_text" => glyphs[0].text = DecodedText::Mapped("X".into()),
                    "partial_native_text" => glyphs[0].text = DecodedText::Mapped("Fi".into()),
                    _ => {
                        let x = glyphs[0].baseline.x;
                        glyphs[0].baseline.x = glyphs[1].baseline.x;
                        glyphs[1].baseline.x = x;
                    }
                }
                new.0.native = Document::new(glyphs);
                1
            }
            "incomplete" => {
                new.0.inventories[0].complete = false;
                0
            }
            "overlap" | "remote_paint" => {
                let y = if case == "overlap" { 100.0 } else { 130.0 };
                with_paint(
                    &mut new,
                    Some(Rect {
                        min: Vec2 { x: 0.0, y },
                        max: Vec2 {
                            x: 200.0,
                            y: y + 10.0,
                        },
                    }),
                );
                if case == "overlap" { 1 } else { 2 }
            }
            "omitted" => {
                append_unassigned(&mut new, PageId(0), 100.0);
                1
            }
            "inferred" => {
                for node in new.1.nodes.iter_mut().skip(1) {
                    node.basis = ViewBasis::ReconstructedStructure;
                }
                0
            }
            "shared_glyph" => {
                // The final punctuation and edge space are one decoded glyph.
                // Trimming the space would split ownership of that glyph.
                let removed = SourceRef::Native { glyph: GlyphId(15) };
                let mut glyphs = new.0.native.items().to_vec();
                glyphs[14].text = DecodedText::Mapped(". ".into());
                glyphs.retain(|glyph| glyph.id != GlyphId(15));
                new.0.native = Document::new(glyphs);
                new.0.inventories[0]
                    .sources
                    .retain(|source| *source != removed);
                let node = &mut new.1.nodes[1];
                node.sources.retain(|source| *source != removed);
                let NodeContent::Text { view } = &mut node.content else {
                    unreachable!()
                };
                view.origins[15] = view.origins[14].clone();
                1
            }
            _ => unreachable!(),
        };
        let comparison = compare(&old, &new);
        assert_eq!(
            comparison.scopes[0].result.native_text_domains.len(),
            expected,
            "{case}"
        );
        assert!(!comparison.search_resolved(), "{case}");
    }
}

#[test]
fn domain_projection_expands_padding_without_erasing_interior_source_spaces() {
    for case in ["padding", "nonspace_padding", "interior"] {
        let old = fixture("a");
        let first = if case == "interior" {
            "First  boundary. "
        } else {
            "First boundary.  "
        };
        let mut new = fixture_with_boundaries("aa", first, "Last boundary. ");
        let position = if case == "interior" { 5 } else { 15 };
        let NodeContent::Text { view } = &mut new.1.nodes[1].content else {
            unreachable!()
        };
        let origins = view.origins.remove(position + 1);
        view.origins[position].extend(origins);
        view.tokens.remove(position + 1);
        view.source_backed.remove(position + 1);
        if case == "nonspace_padding" {
            let mut glyphs = new.0.native.items().to_vec();
            glyphs[16].text = DecodedText::Mapped("X".into());
            new.0.native = Document::new(glyphs);
        }
        let result = compare(&old, &new);
        assert_eq!(
            result.scopes[0].result.native_text_domains.len(),
            if case == "padding" { 2 } else { 1 },
            "{case}"
        );
        assert!(!result.search_resolved());
    }
}

#[test]
fn short_native_domains_use_the_comparator_budget_instead_of_grid_estimates() {
    let old = fixture_with_boundaries("a", "X", "Y");
    let new = fixture_with_boundaries("aa", "X ", "Y ");
    let result = compare(&old, &new);
    assert_eq!(
        result.scopes[0].result.text_boundary_correspondences.len(),
        2
    );
    assert_eq!(result.scopes[0].result.native_text_domains.len(), 2);
    assert!(!result.search_resolved());
}

#[test]
fn padding_domains_account_for_equal_bodies_without_owning_padding_or_reviews() {
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
    assert_eq!(result.native_text_domains.len(), 2);
    let equal_native_characters = "First boundary.Last boundary.".chars().count();
    assert_eq!(coverage[0].old_compared_sources, equal_native_characters);
    assert_eq!(coverage[0].new_compared_sources, equal_native_characters);
    assert_eq!(coverage[0].old_uncompared_sources, 1);
    assert_eq!(coverage[0].new_uncompared_sources, 4);
    let reloaded: DocumentViewComparison = serde_json::from_value(
        serde_json::to_value(&comparison).expect("serialize domain observations"),
    )
    .expect("reload observations without proof authority");
    let reloaded_coverage = pdfdelta_core::document::document_coverage(
        DocumentView {
            evidence: &old.0,
            graph: &old.1,
        },
        DocumentView {
            evidence: &new.0,
            graph: &new.1,
        },
        &reloaded,
        &[Channel::Text].into(),
    );
    assert_eq!(reloaded_coverage[0].old_compared_sources, 0);
    assert_eq!(reloaded_coverage[0].new_compared_sources, 0);
    assert!(!coverage[0].complete);
    assert!(!comparison.search_resolved());
    assert!(result.unresolved.iter().any(|reason| {
        reason.contains("paragraph identity and any unaccounted sources remain unresolved")
    }));
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

fn inferred_merge_fixture() -> (Fixture, Fixture) {
    (
        fixture_rows(&[
            "Purpose of this circular. ",
            "a. Operators receive guidance on how to develop and receive approval for a ",
            "weight and balance control program for aircraft under the applicable regulations. ",
            "b. This circular presents recommendations for using average and estimated weights in the approved control program. ",
            "NOTE: Each aircraft must be weighed at the required inspection interval. ",
        ]),
        fixture_rows(&[
            "Operators receive guidance on how to develop and receive approval for a weight and balance control program for aircraft under the current regulations. This circular presents recommendations for using average and estimated weights in the approved control program. ",
        ]),
    )
}

fn inferred_groups(
    result: &DocumentViewComparison,
) -> Vec<&pdfdelta_core::document::TextScopeReview> {
    result
        .scopes
        .iter()
        .flat_map(|scope| &scope.result.text_scope_reviews)
        .filter(|review| review.convention == "inferred-native-paragraph-group-v1")
        .collect()
}

#[test]
fn inferred_paragraph_merge_retains_labels_and_all_body_sources_without_order_masks() {
    let (old, mut new) = inferred_merge_fixture();
    let mut glyphs = new.0.native.items().to_vec();
    let count = glyphs.len() as u32;
    for glyph in &mut glyphs {
        glyph.render_order = count - glyph.render_order;
    }
    new.0.native = Document::new(glyphs);
    new.0.inventories[0].complete = false;
    let result = compare(&old, &new);
    let reviews = inferred_groups(&result);
    assert_eq!(reviews.len(), 1);
    let review = reviews[0];
    assert_eq!(review.comparison.old, vec![NodeId(2), NodeId(3), NodeId(4)]);
    assert_eq!(review.comparison.new, vec![NodeId(1)]);
    assert_eq!(
        review.old_sources,
        old.1.nodes[2..5]
            .iter()
            .flat_map(|node| node.sources.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(review.new_sources, new.1.nodes[1].sources);
    assert!(review.boundaries.is_empty());
    assert!(review.source_cuts.is_none());
    assert_eq!(review.candidate_search_exhaustive, Some(false));
    assert_eq!(
        review.comparison.interpretation,
        pdfdelta_core::document::InterpretationStatus::Inferred
    );
    assert!(review.comparison.text_mask.is_none());
    assert!(review.comparison.text_change_proof.is_some());
    let Some(pdfdelta_core::document::TypedOperation::TextChanged {
        old: Some(text), ..
    }) = &review.comparison.operation
    else {
        panic!("complete review text")
    };
    assert!(text.starts_with("a. Operators"));
    assert!(text.contains("b. This circular"));
    assert!(!text.contains("NOTE:"));
    assert!(!text.contains("Purpose"));
    assert_eq!(result, compare(&old, &new));
}

#[test]
fn inferred_paragraph_split_is_symmetric() {
    let (parts, whole) = inferred_merge_fixture();
    let result = compare(&whole, &parts);
    let reviews = inferred_groups(&result);
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].comparison.old, vec![NodeId(1)]);
    assert_eq!(
        reviews[0].comparison.new,
        vec![NodeId(2), NodeId(3), NodeId(4)]
    );
}

#[test]
fn inferred_paragraph_merge_does_not_report_unchanged_or_space_only_text() {
    let (old, _) = inferred_merge_fixture();
    let text: String = old.1.nodes[2..5]
        .iter()
        .map(|node| {
            let NodeContent::Text { view } = &node.content else {
                unreachable!()
            };
            view.display_text().expect("mapped fixture text")
        })
        .collect();
    for text in [text.clone(), text.replace(' ', "  ")] {
        let new = fixture_rows(&[&text]);
        assert!(inferred_groups(&compare(&old, &new)).is_empty());
    }
}

#[test]
fn inferred_paragraph_merge_respects_disabled_text_and_zero_search_budget() {
    let (old, new) = inferred_merge_fixture();
    for disabled_text in [false, true] {
        let mut limits = DocumentComparisonLimits::default();
        if disabled_text {
            limits.matching.channels.text = false;
        } else {
            limits.text.max_token_visits = 0;
        }
        let result = compare_document_views(
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
        .expect("bounded comparison");
        assert!(inferred_groups(&result).is_empty());
    }
}

#[test]
fn inferred_paragraph_groups_reject_competing_seeds_and_source_conflicts() {
    let (mut old, new) = inferred_merge_fixture();
    old.1
        .source_conflicts
        .push(pdfdelta_core::document::SourceConflict {
            sources: vec![old.1.nodes[2].sources[0], old.1.nodes[3].sources[0]],
            reason: "Two interpretations of overlapping paint".into(),
        });
    assert!(inferred_groups(&compare(&old, &new)).is_empty());

    let (old, new) = inferred_merge_fixture();
    let texts: Vec<_> = old.1.nodes[1..]
        .iter()
        .map(|node| {
            let NodeContent::Text { view } = &node.content else {
                unreachable!()
            };
            view.display_text().expect("mapped fixture text")
        })
        .collect();
    let rows: Vec<_> = texts.iter().chain(&texts).map(String::as_str).collect();
    let repeated = fixture_rows(&rows);
    assert!(inferred_groups(&compare(&repeated, &new)).is_empty());
}

#[test]
fn inferred_paragraph_groups_exclude_toc_headings_and_incomplete_prose() {
    let old = fixture_rows(&[
        "APPENDIX A. ADDITIONAL REQUIREMENTS FOR PASSENGER WEIGHT IN SMALL CABIN AIRCRAFT (4 pages)............1 ",
    ]);
    let new = fixture_rows(&[
        "APPENDIX B. ADDITIONAL REQUIREMENTS FOR PASSENGER ",
        "WEIGHT IN SMALL CABIN AIRCRAFT ",
    ]);
    assert!(inferred_groups(&compare(&old, &new)).is_empty());
    let (old, new) = inferred_merge_fixture();
    let NodeContent::Text { view } = &new.1.nodes[1].content else {
        unreachable!()
    };
    let text = view.display_text().expect("mapped fixture text");
    let incomplete = fixture_rows(&[text.trim().trim_end_matches('.')]);
    assert!(inferred_groups(&compare(&old, &incomplete)).is_empty());
    let unmapped_identity = fixture_rows(&[&text.replacen("Operators", "\u{e000}perators", 1)]);
    assert!(inferred_groups(&compare(&old, &unmapped_identity)).is_empty());
}

#[test]
fn inferred_paragraph_groups_do_not_turn_continuations_or_neighbor_definitions_into_changes() {
    let old = fixture_rows(&[
        "a. Figure 3 below shows the approved loading ",
        "envelope, based on variations in passenger seating and weight as well as fuel consumption. ",
    ]);
    let new = fixture_rows(&[
        "2. Figure 4, Operational Loading Envelope With a Curtailment for Variations in ",
        "Passenger Seating, shows the approved loading envelope, based on variations in passenger seating and weight as well as fuel consumption. ",
    ]);
    assert!(inferred_groups(&compare(&old, &new)).is_empty());

    let old = fixture_rows(&[
        "1. Maximum taxi weight. The maximum allowable weight for taxiing. ",
        "2. Maximum zero-fuel weight. The maximum permissible weight with no disposable fuel and oil. The manufacturer establishes the applicable aircraft operating limitations. ",
    ]);
    let new = fixture_rows(&[
        "A.1 Maximum Taxi Weight. The maximum allowable weight for taxiing. A.2 Maximum Zero Fuel Weight. The maximum permissible weight ",
        "with no disposable fuel and oil. The manufacturer establishes the applicable aircraft operational limitations. ",
    ]);
    assert!(inferred_groups(&compare(&old, &new)).is_empty());
}

#[test]
fn obstructed_early_interval_preserves_work_for_a_later_closed_change() {
    let mut old = fixture_rows(&["FIRST", "bad old", "SECOND", "good old", "THIRD"]);
    let new = fixture_rows(&["FIRST", "bad new", "SECOND", "good new", "THIRD"]);
    let mut glyphs = old.0.native.items().to_vec();
    let template = glyphs[0].clone();
    for index in 0..2_000 {
        let mut glyph = template.clone();
        glyph.id = GlyphId(10_000 + index);
        glyph.render_order = 10_000 + index as u32;
        glyph.baseline.y = 200.0;
        glyph.bbox.min.y = 200.0;
        glyph.bbox.max.y = 210.0;
        old.0.inventories[0]
            .sources
            .push(SourceRef::Native { glyph: glyph.id });
        glyphs.push(glyph);
    }
    old.0.native = Document::new(glyphs);
    with_paint(
        &mut old,
        Some(Rect {
            min: Vec2 { x: 0.0, y: 75.0 },
            max: Vec2 { x: 100.0, y: 85.0 },
        }),
    );
    for budget in [2_000, 4_000] {
        let mut limits = DocumentComparisonLimits::default();
        limits.matching.max_ownership_visits = budget;
        let result = compare_document_views(
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
        .expect("bounded comparison");
        let reviews = &result.scopes[0].result.text_scope_reviews;
        let recovered = reviews.iter().any(|review| {
            review.old_sources == old.1.nodes[4].sources
                && review.new_sources == new.1.nodes[4].sources
        });
        assert_eq!(recovered, budget == 4_000, "budget {budget}");
        assert!(reviews.iter().all(|review| {
            !review
                .old_sources
                .iter()
                .any(|source| old.1.nodes[2].sources.contains(source))
        }));
    }
}

#[test]
fn whole_presence_keeps_a_turn_before_refining_existing_changes() {
    let old_body = format!("{}old", "a".repeat(64));
    let new_body = format!("{}new", "a".repeat(64));
    for margin_label in [false, true] {
        let mut old = if margin_label {
            fixture_rows(&["FIRST", &old_body, "SECOND", "old label", "THIRD"])
        } else {
            fixture_rows(&["FIRST", &old_body, "SECOND", "THIRD"])
        };
        let mut new = if margin_label {
            fixture_rows(&["FIRST", &new_body, "SECOND", "new label", "added", "THIRD"])
        } else {
            fixture_rows(&["FIRST", &new_body, "SECOND", "added", "THIRD"])
        };
        if margin_label {
            for fixture in [&mut old, &mut new] {
                let mut glyphs = fixture.0.native.items().to_vec();
                for glyph in &mut glyphs {
                    if fixture.1.nodes[4]
                        .sources
                        .contains(&SourceRef::Native { glyph: glyph.id })
                    {
                        glyph.baseline.x -= 100.0;
                        glyph.bbox.min.x -= 100.0;
                        glyph.bbox.max.x -= 100.0;
                        glyph.baseline.y -= 30.0;
                        glyph.bbox.min.y -= 30.0;
                        glyph.bbox.max.y -= 30.0;
                    }
                }
                fixture.0.native = Document::new(glyphs);
            }
        }
        let added = if margin_label { 5 } else { 4 };
        for reverse in [false, true] {
            let (left, right) = if reverse { (&new, &old) } else { (&old, &new) };
            let budgets: &[usize] = if margin_label {
                &[8_000]
            } else {
                &[1_000, 4_000]
            };
            for &budget in budgets {
                let mut limits = DocumentComparisonLimits::default();
                limits.matching.max_ownership_visits = budget;
                let result = compare_document_views(
                    DocumentView {
                        evidence: &left.0,
                        graph: &left.1,
                    },
                    DocumentView {
                        evidence: &right.0,
                        graph: &right.1,
                    },
                    CorrespondenceScope {
                        old: NodeId(0),
                        new: NodeId(0),
                    },
                    limits,
                    HierarchyLimits::default(),
                )
                .expect("bounded comparison");
                let reviews = &result.scopes[0].result.text_scope_reviews;
                let presence = reviews.iter().any(|review| {
                    let (absent, present) = if reverse {
                        (&review.new_sources, &review.old_sources)
                    } else {
                        (&review.old_sources, &review.new_sources)
                    };
                    absent.is_empty() && *present == new.1.nodes[added].sources
                });
                let change = reviews.iter().any(|review| {
                    review.old_sources == left.1.nodes[2].sources
                        && review.new_sources == right.1.nodes[2].sources
                });
                assert_eq!(
                    presence,
                    budget >= 4_000,
                    "presence: label={margin_label}, reverse={reverse}, budget={budget}"
                );
                assert_eq!(
                    change,
                    budget >= 4_000,
                    "change: label={margin_label}, reverse={reverse}, budget={budget}"
                );
            }
        }
    }
}

fn ambiguous_closed_intervals(crossing: bool) -> (Fixture, Fixture) {
    let mut old_rows = Vec::new();
    let mut new_rows = Vec::new();
    for label in ["ALPHA", "BETA", "GAMMA"] {
        old_rows.extend([
            format!("{label} joint segmentation"),
            "[7] J. Dai. Instance-aware semantic segmen-".into(),
            "tation via multi-task network cascades. In CVPR, 2016. 2, 3, 4, 5, 6".into(),
            "[8] R-FCN: Object detection via".into(),
            format!("{label} region-based networks."),
        ]);
        new_rows.extend([
            format!("{label} joint segmentation"),
            "[10] J. Dai. Instance-aware semantic segmen-".into(),
            "tation via multi-task network cascades. In CVPR, 2016. 2, 3, 4, 5, 6".into(),
            "[11] R-FCN: Object detection via".into(),
            format!("{label} region-based networks."),
        ]);
    }
    if crossing {
        old_rows.push("EXTERNAL".into());
        new_rows.push(new_rows[12].clone());
        new_rows[12] = "EXTERNAL".into();
    }
    let mut old = fixture_rows(&old_rows.iter().map(String::as_str).collect::<Vec<_>>());
    let mut new = fixture_rows(&new_rows.iter().map(String::as_str).collect::<Vec<_>>());
    for (side, fixture) in [&mut old, &mut new].into_iter().enumerate() {
        let mut removed = Vec::new();
        let last = if crossing && side == 1 { 16 } else { 13 };
        for index in [3, 8, last] {
            let NodeContent::Text { view } = &mut fixture.1.nodes[index].content else {
                unreachable!()
            };
            view.normalization = TextNormalization::Unresolved {
                reason: "native row boundary is uncertain".into(),
            };
            for position in 1..view.tokens.len() - 1 {
                if view.tokens[position] == ComparableToken::Scalar(' ') {
                    removed.extend(view.origins[position].iter().copied());
                    view.origins[position] =
                        vec![view.origins[position - 1][0], view.origins[position + 1][0]];
                    view.source_backed[position] = false;
                }
            }
            fixture.1.nodes[index]
                .sources
                .retain(|source| !removed.contains(source));
        }
        fixture.0.native = Document::new(
            fixture
                .0
                .native
                .items()
                .iter()
                .filter(|g| !removed.contains(&SourceRef::Native { glyph: g.id }))
                .cloned()
                .collect(),
        );
        for inventory in &mut fixture.0.inventories {
            inventory.sources.retain(|source| !removed.contains(source));
        }
    }
    (old, new)
}

#[test]
fn closed_intervals_reuse_outer_cuts_after_ambiguous_fragment_search() {
    let (old, new) = ambiguous_closed_intervals(false);
    for (a, b) in [(&old, &new), (&new, &old), (&old, &old)] {
        for budget in [0, 80_000] {
            let mut limits = DocumentComparisonLimits::default();
            limits.matching.max_ownership_visits = budget;
            let result = compare_document_views(
                DocumentView {
                    evidence: &a.0,
                    graph: &a.1,
                },
                DocumentView {
                    evidence: &b.0,
                    graph: &b.1,
                },
                CorrespondenceScope {
                    old: NodeId(0),
                    new: NodeId(0),
                },
                limits,
                HierarchyLimits::default(),
            )
            .expect("bounded native comparison");
            let reviews = &result.scopes[0].result.text_scope_reviews;
            let changed = !std::ptr::eq(a, b);
            if !changed || budget == 0 {
                assert!(reviews.is_empty());
                continue;
            }
            for first in [2, 7, 12] {
                let sources = |fixture: &Fixture| {
                    fixture.1.nodes[first..first + 3]
                        .iter()
                        .flat_map(|node| node.sources.iter().copied())
                        .collect::<Vec<_>>()
                };
                let a_sources = sources(a);
                let b_sources = sources(b);
                let review = reviews
                    .iter()
                    .find(|review| {
                        review.old_sources == a_sources && review.new_sources == b_sources
                    })
                    .expect("all three closed interiors retain their independent citation changes");
                let cuts = review.source_cuts.as_ref().expect("native cut certificate");
                assert!(matches!(
                    cuts.entry.evidence,
                    pdfdelta_core::document::CutEvidence::AcceptedBoundary { .. }
                ));
                assert!(matches!(
                    cuts.exit.evidence,
                    pdfdelta_core::document::CutEvidence::AcceptedBoundary { .. }
                ));
                assert!(review.comparison.operation.is_some());
            }
        }
    }
}

#[test]
fn whole_cut_fallback_rejects_an_accepted_counterpart_from_outside() {
    let (old, new) = ambiguous_closed_intervals(true);
    let result = compare(&old, &new);
    let scope = &result.scopes[0].result;
    assert!(
        scope.accepted_correspondences.iter().any(|&index| {
            let proposal = &scope.candidates.proposals[index];
            proposal.old == [NodeId(16)] && proposal.new == [NodeId(13)]
        }),
        "the negative control must retain the crossing literal counterpart"
    );
    let outside = &new.1.nodes[13].sources;
    assert!(
        scope.text_scope_reviews.iter().all(|review| {
            !review
                .new_sources
                .iter()
                .any(|source| outside.contains(source))
        }),
        "a whole interval must not consume an accepted counterpart from outside"
    );
    assert!(
        scope.text_scope_reviews.iter().any(|review| {
            review
                .old_sources
                .iter()
                .any(|source| old.1.nodes[2].sources.contains(source))
        }),
        "independent closed intervals must still be compared"
    );
}
