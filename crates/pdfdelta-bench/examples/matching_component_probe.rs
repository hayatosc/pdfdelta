//! Reconstruct one selected native candidate set and expose its source constraints.
//! This diagnostic does not turn serialized candidates into correspondence facts.

use std::{collections::BTreeSet, env, fs::File, io::Read, sync::Arc};

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, CorrespondenceProposal, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, EdgeKind, EvidenceStore, MatchingChannels,
        NodeContent, NodeId, PageEvidence, refine_table_views, solve_correspondence_scope,
    },
    model::PageId,
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::PipelineOptions,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn bytes(path: &str, limit: usize) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut result)?;
    if result.len() > limit {
        return Err("diagnostic input byte limit".into());
    }
    Ok(result)
}

fn acquire(path: &str, limits: DocumentComparisonLimits) -> Result<EvidenceStore> {
    let parse_limits = ParseLimits::default();
    let bytes = bytes(path, parse_limits.max_input_bytes)?;
    let revision = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let pdf = LopdfParser.parse(Arc::from(bytes), parse_limits)?;
    let extractor = ContentStreamGlyphExtractor;
    let pages = extractor
        .page_frames(
            pdf.as_ref(),
            ExtractionLimits::default(),
            limits.evidence.max_pages,
        )?
        .into_iter()
        .enumerate()
        .map(|(index, frame)| PageEvidence {
            page: PageId(index as u32),
            bounds: frame.ok().map(|frame| frame.canonical_bounds()),
        })
        .collect();
    let outcome = extractor.extract_outcome(pdf.as_ref(), ExtractionLimits::default())?;
    Ok(EvidenceStore::from_native(
        revision,
        BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "pdfdelta-native".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            profile: "matching-component-diagnostic".into(),
            model: None,
        },
        pages,
        outcome,
        limits.evidence,
    )?)
}

fn constraints(graph: &DocumentGraph, ids: BTreeSet<NodeId>) -> Result<Value> {
    let nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter(|node| ids.contains(&node.id))
        .collect();
    if nodes.len() > 512 || nodes.iter().map(|n| n.sources.len()).sum::<usize>() > 100_000 {
        return Err("diagnostic selected-source limit".into());
    }
    let mut work = 1_000_000usize;
    let sources: BTreeSet<_> = nodes
        .iter()
        .flat_map(|node| node.sources.iter().copied())
        .collect();
    let mut shared = Vec::new();
    for (index, a) in nodes.iter().enumerate() {
        work = work
            .checked_sub(a.sources.len())
            .ok_or("diagnostic source intersection work limit")?;
        let a_sources: BTreeSet<_> = a.sources.iter().collect();
        for b in nodes.iter().skip(index + 1) {
            work = work
                .checked_sub(b.sources.len() + 1)
                .ok_or("diagnostic source intersection work limit")?;
            let intersection: Vec<_> = b.sources.iter().filter(|s| a_sources.contains(s)).collect();
            if !intersection.is_empty() {
                shared.push(json!({"nodes": [a.id, b.id], "sources": intersection}));
            }
        }
    }
    Ok(json!({
        "nodes": nodes,
        "shared_sources": shared,
        "contains_edges": graph.edges.iter().filter(|edge| edge.kind == EdgeKind::Contains && (ids.contains(&edge.from) || ids.contains(&edge.to))).collect::<Vec<_>>(),
        "alternatives": graph.alternatives.iter().filter(|a| ids.contains(&a.parent) || a.partitions.iter().flatten().any(|id| ids.contains(id))).collect::<Vec<_>>(),
        "source_conflicts": graph.source_conflicts.iter().filter(|c| c.sources.iter().any(|s| sources.contains(s))).collect::<Vec<_>>(),
    }))
}

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if let [mode, old, new, old_page, new_page] = args.as_slice()
        && mode == "--pages"
    {
        return pages(
            old,
            new,
            PageId(old_page.parse()?),
            PageId(new_page.parse()?),
            None,
        );
    }
    if let [mode, old, new, old_page, new_page, gap] = args.as_slice()
        && mode == "--pages-gap"
    {
        return pages(
            old,
            new,
            PageId(old_page.parse()?),
            PageId(new_page.parse()?),
            Some(gap.parse()?),
        );
    }
    let [old_path, new_path, proposals_path] = args.as_slice() else {
        return Err("usage: matching_component_probe OLD.pdf NEW.pdf PROPOSALS.json".into());
    };
    let proposals: Vec<CorrespondenceProposal> =
        serde_json::from_slice(&bytes(proposals_path, 4 * 1024 * 1024)?)?;
    if proposals.is_empty() || proposals.len() > 256 {
        return Err("diagnostic requires 1..=256 proposals".into());
    }
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.channels = MatchingChannels::from(&BTreeSet::from([Channel::Text]));
    let old = acquire(old_path, limits)?;
    let new = acquire(new_path, limits)?;
    let pipeline = PipelineOptions::default();
    let mut old_graph =
        DocumentGraph::from_evidence(&old, pipeline, limits.evidence, limits.graph)?;
    let mut new_graph =
        DocumentGraph::from_evidence(&new, pipeline, limits.evidence, limits.graph)?;
    let tables = refine_table_views(&mut old_graph, &mut new_graph, &old, &new, pipeline, limits)?;
    let matching = solve_correspondence_scope(
        &old_graph,
        &new_graph,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        &proposals,
        limits.matching,
    )?;
    let old_ids = proposals
        .iter()
        .flat_map(|p| p.old.iter().copied())
        .collect();
    let new_ids = proposals
        .iter()
        .flat_map(|p| p.new.iter().copied())
        .collect();
    println!(
        "{}",
        serde_json::to_string(&json!({
            "old_revision": old.revision, "new_revision": new.revision,
            "native_glyph_counts": [old.native.items().len(), new.native.items().len()],
            "table_refinements": tables, "matching": matching,
            "old": constraints(&old_graph, old_ids)?, "new": constraints(&new_graph, new_ids)?,
            "certifies_recovery": false,
        }))?
    );
    Ok(())
}

fn pages(old: &str, new: &str, old_page: PageId, new_page: PageId, gap: Option<f64>) -> Result<()> {
    let limits = DocumentComparisonLimits::default();
    let mut pipeline = PipelineOptions::default();
    if let Some(gap) = gap {
        if !gap.is_finite() || !(0.0..=2.0).contains(&gap) {
            return Err("diagnostic gap ratio must be finite and in 0..=2".into());
        }
        pipeline.block.max_vertical_gap_height_ratio = gap;
    }
    let old = acquire(old, limits)?;
    let new = acquire(new, limits)?;
    let mut a = DocumentGraph::from_evidence(&old, pipeline, limits.evidence, limits.graph)?;
    let mut b = DocumentGraph::from_evidence(&new, pipeline, limits.evidence, limits.graph)?;
    let tables = refine_table_views(&mut a, &mut b, &old, &new, pipeline, limits)?;
    let select = |graph: &DocumentGraph, evidence: &EvidenceStore, page: PageId| -> Result<Value> {
        if !evidence.pages.iter().any(|p| p.page == page) {
            return Err("diagnostic page is outside the document".into());
        }
        let ids: BTreeSet<_> = graph
            .nodes
            .iter()
            .filter(|n| n.pages.contains(&page) && matches!(n.content, NodeContent::Text { .. }))
            .map(|n| n.id)
            .collect();
        let glyphs: Vec<_> = evidence
            .native
            .items()
            .iter()
            .filter(|g| g.page == page)
            .collect();
        if glyphs.len() > 50_000 {
            return Err("diagnostic page glyph limit".into());
        }
        Ok(json!({
            "revision": evidence.revision,
            "native_glyph_count": evidence.native.items().len(),
            "page": page, "glyphs": glyphs,
            "paint": evidence.native.non_text_paint_bounds().map(|p| p.iter().filter(|p| p.page == page).collect::<Vec<_>>()),
            "constraints": constraints(graph, ids.clone())?,
            "order_edges": graph.edges.iter().filter(|e| e.kind == EdgeKind::Precedes && (ids.contains(&e.from) || ids.contains(&e.to))).collect::<Vec<_>>(),
        }))
    };
    println!(
        "{}",
        serde_json::to_string(&json!({
            "old": select(&a, &old, old_page)?, "new": select(&b, &new, new_page)?,
            "table_refinements": tables, "certifies_recovery": false,
            "max_vertical_gap_height_ratio": pipeline.block.max_vertical_gap_height_ratio,
        }))?
    );
    Ok(())
}
