//! Source-backed footer view fixtures over programmatically constructed graphs.

use pdfdelta_core::{
    Result,
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
    source::ExtractionOutcome,
};

#[test]
fn native_graph_supplies_source_backed_catalog_footer_views() -> Result<()> {
    use pdfdelta_core::document::{
        BackendIdentity, BackendKind, CorrespondenceScope, DocumentGraph, EvidenceLimits,
        EvidenceStore, GraphLimits, MatchingLimits, NodeContent, NodeId, NodeKind, PageEvidence,
        ProposalBasis, propose_scope_correspondences, solve_correspondence_scope,
    };
    let make = |page, split| -> Result<DocumentGraph> {
        let mut lines = vec![line("Body text remains separate", page, 300.0)];
        if split {
            lines.extend([
                line_at("Form 5678 (2031)", page, 420.0, 30.0),
                line_at(
                    "Reviewed catalog instructions remain available.",
                    page,
                    0.0,
                    30.1,
                ),
                line_at("Cat. No. 12A34", page, 320.0, 30.2),
            ]);
        } else {
            lines.push(line(
                "Reviewed catalog instructions remain available. Cat. No. 12A34 Form 5678 (2030)",
                page,
                30.0,
            ));
        }
        let store = EvidenceStore::from_native(
            format!("revision-{page}"),
            BackendIdentity {
                kind: BackendKind::NativeParser,
                name: "fixture".into(),
                version: "1".into(),
                profile: "native".into(),
                model: None,
            },
            vec![PageEvidence {
                page: PageId(page),
                bounds: None,
            }],
            ExtractionOutcome::complete(document(&lines)),
            EvidenceLimits::default(),
        )?;
        DocumentGraph::from_native(
            &store,
            pdfdelta_core::options::PipelineOptions::default(),
            EvidenceLimits::default(),
            GraphLimits::default(),
        )
    };
    let old = make(0, false)?;
    let new = make(1, true)?;
    for graph in [&old, &new] {
        let footer = graph
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Footer && node.identity.is_some())
            .expect("keyed footer view");
        assert!(!footer.sources.is_empty());
        let NodeContent::Text { view } = &footer.content else {
            panic!("footer is a text view")
        };
        assert!(
            view.tokens
                .iter()
                .filter_map(pdfdelta_core::normalize::ComparableToken::as_scalar)
                .collect::<String>()
                .contains("Cat. No. 12A34 Form 5678")
        );
        assert!(view.origins.iter().all(|origins| !origins.is_empty()));
    }
    let scope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };
    let limits = MatchingLimits::default();
    let proposals = propose_scope_correspondences(&old, &new, scope, limits)?;
    assert!(proposals.exhaustive);
    let keyed = proposals
        .proposals
        .iter()
        .position(|proposal| proposal.basis == ProposalBasis::ScopedIdentity)
        .expect("common footer candidate");
    let matching = solve_correspondence_scope(&old, &new, scope, &proposals.proposals, limits)?;
    assert!(
        matching
            .components
            .iter()
            .any(|component| component.exhaustive && component.mandatory.contains(&keyed))
    );
    Ok(())
}

struct LineSpec<'a> {
    text: &'a str,
    page: u32,
    x: f64,
    y: f64,
    direction: Vec2,
    render_mode: TextRenderMode,
}

fn line(text: &str, page: u32, y: f64) -> LineSpec<'_> {
    LineSpec {
        text,
        page,
        x: 0.0,
        y,
        direction: Vec2 { x: 1.0, y: 0.0 },
        render_mode: TextRenderMode::Fill,
    }
}

fn line_at(text: &str, page: u32, x: f64, y: f64) -> LineSpec<'_> {
    LineSpec {
        x,
        ..line(text, page, y)
    }
}

fn document(lines: &[LineSpec<'_>]) -> Document<Glyph> {
    let mut glyphs = Vec::new();
    let mut next_id = 1_u64;
    for (line_index, line) in lines.iter().enumerate() {
        let mut inline_offset = 0.0;
        for character in line.text.chars() {
            if character == ' ' {
                inline_offset += 5.0;
                continue;
            }

            let x = line.x + inline_offset * line.direction.x;
            let y = line.y + inline_offset * line.direction.y;
            let (width, height) = if line.direction.x == 0.0 {
                (10.0, 5.0)
            } else {
                (5.0, 10.0)
            };

            let id = GlyphId(next_id);
            glyphs.push(Glyph {
                id,
                text: DecodedText::Mapped(character.to_string()),
                raw_code: character.to_string().into_bytes(),
                page: PageId(line.page),
                bbox: Rect {
                    min: Vec2 { x, y },
                    max: Vec2 {
                        x: x + width,
                        y: y + height,
                    },
                },
                baseline: Vec2 { x, y },
                direction: line.direction,
                font_id: FontId(1),
                font_size: 10.0,
                render_order: u32::try_from(next_id).expect("fixture glyph id should fit in u32"),
                render_mode: line.render_mode,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: u32::try_from(line_index + 1)
                            .expect("fixture line index should fit in u32"),
                        generation: 0,
                    },
                    operator_index: u32::try_from(next_id)
                        .expect("fixture glyph id should fit in u32"),
                },
            });
            next_id += 1;
            inline_offset += 6.0;
        }
    }
    Document::new(glyphs)
}
