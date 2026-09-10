use std::{collections::BTreeMap, sync::Arc};

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, DocumentComparisonLimits, DocumentGraph, EvidenceStore,
        NodeContent, PageEvidence, SourceRef,
    },
    model::PageId,
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::PipelineOptions,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("PDF path is required")?;
    let parse_limits = ParseLimits::default();
    if std::fs::metadata(&path)?.len() > parse_limits.max_input_bytes as u64 {
        return Err("PDF input exceeds parser limit".into());
    }
    let bytes: Arc<[u8]> = std::fs::read(path)?.into();
    let parsed = LopdfParser.parse(bytes, parse_limits)?;
    let pages = parsed
        .pages()?
        .iter()
        .enumerate()
        .map(|(index, _)| {
            Ok(PageEvidence {
                page: PageId(u32::try_from(index)?),
                bounds: None,
            })
        })
        .collect::<Result<Vec<_>, std::num::TryFromIntError>>()?;
    let extraction = ContentStreamGlyphExtractor
        .extract_outcome(parsed.as_ref(), ExtractionLimits::default())?;
    let limits = DocumentComparisonLimits::default();
    let store = EvidenceStore::from_native(
        "source-only-diagnostic".into(),
        BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "lopdf".into(),
            version: "frozen-library".into(),
            profile: "native-base-node-source-map".into(),
            model: None,
        },
        pages,
        extraction,
        limits.evidence,
    )?;
    let graph = DocumentGraph::from_native(
        &store,
        PipelineOptions::default(),
        limits.evidence,
        limits.graph,
    )?;
    let raw: BTreeMap<_, _> = store
        .native
        .items()
        .iter()
        .filter_map(|glyph| {
            let pdfdelta_core::model::DecodedText::Mapped(text) = &glyph.text else {
                return None;
            };
            let mut chars = text.chars();
            let value = chars.next()?;
            chars.next().is_none().then_some((glyph.id, value))
        })
        .collect();
    let mut nodes = BTreeMap::new();
    for node in &graph.nodes {
        let NodeContent::Text { view } = &node.content else {
            continue;
        };
        if node.basis.is_inferred()
            || node
                .sources
                .iter()
                .any(|s| !matches!(s, SourceRef::Native { .. }))
        {
            continue;
        }
        let glyphs: Vec<_> = node
            .sources
            .iter()
            .filter_map(|s| match s {
                SourceRef::Native { glyph } => Some(glyph.0),
                _ => None,
            })
            .collect();
        let mut bindings = BTreeMap::<_, Vec<_>>::new();
        if view.normalization == pdfdelta_core::document::TextNormalization::Exact {
            for (position, token) in view.tokens.iter().enumerate() {
                let pdfdelta_core::normalize::ComparableToken::Scalar(value) = token else {
                    continue;
                };
                if !view.source_backed[position] || view.origins[position].is_empty() {
                    continue;
                }
                if view.origins[position].iter().all(|source| {
                    matches!(source, SourceRef::Native { glyph } if raw.get(glyph) == Some(value))
                }) {
                    for source in &view.origins[position] {
                        let SourceRef::Native { glyph } = source else { unreachable!() };
                        bindings.entry(glyph.0).or_default().push(serde_json::json!({"token": position, "value": value}));
                    }
                }
            }
        }
        bindings.retain(|_, positions| positions.len() == 1);
        nodes.insert(
            node.id.0,
            serde_json::json!({
                "glyphs": glyphs, "token_count": view.tokens.len(), "pages": node.pages,
                "identity": node.identity, "text": view.display_text(),
                "single_scalar_bindings": bindings,
            }),
        );
    }
    serde_json::to_writer_pretty(
        std::io::stdout(),
        &serde_json::json!({
            "contract": "native-base-source-snapshot-v1",
            "limits": "frozen defaults; pipeline scale 1.0",
            "nodes": nodes, "extraction_issues": store.issues,
            "note": "Source map only: no comparison, no coverage or no-change claim. Appended provider nodes are excluded.",
        }),
    )?;
    Ok(())
}
