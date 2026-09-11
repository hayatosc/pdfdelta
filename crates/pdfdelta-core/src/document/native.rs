use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Result,
    layout::{BlockRole, reconstruct_blocks_with_issues, reconstruct_lines},
    model::PageId,
    normalize::{BlockText, TextSourceAtom, normalize_blocks},
    pipeline::PipelineOptions,
    source::{ExtractionIssueKind, ExtractionOutcome, ExtractionScope},
};

use super::{
    BackendIdentity, BackendKind, Channel, ChannelInventory, DocumentGraph, EdgeKind,
    EvidenceFailure, EvidenceIssue, EvidenceLimits, EvidenceStore, GraphEdge, GraphLimits,
    GraphNode, NodeContent, NodeId, NodeKind, PageEvidence, SourceRef, TextNormalization, TextView,
    ViewBasis,
    evidence::{bounded, invalid},
};

pub(super) fn block_text_view(
    block: &BlockText,
    limits: GraphLimits,
    tokens_used: &mut usize,
    references_used: &mut usize,
    normalization_work: &mut usize,
) -> Result<(TextView, Vec<SourceRef>)> {
    let pairs = block.canonical.comparable_tokens_with_sources()?;
    super::graph::charge(
        tokens_used,
        pairs.len(),
        limits.max_tokens,
        "graph text tokens",
    )?;
    let mut tokens = Vec::with_capacity(pairs.len());
    let mut origins = Vec::with_capacity(pairs.len());
    let mut source_backed = Vec::with_capacity(pairs.len());
    let mut all_sources = BTreeSet::new();
    for (token, source) in pairs {
        let mut refs = BTreeSet::new();
        let mut backed = false;
        for atom in source.atoms {
            match atom {
                TextSourceAtom::Glyph(glyph) => {
                    refs.insert(SourceRef::Native { glyph });
                    backed = true;
                }
                TextSourceAtom::SyntheticSpace {
                    preceding,
                    following,
                }
                | TextSourceAtom::LineBreak {
                    preceding,
                    following,
                } => {
                    refs.insert(SourceRef::Native { glyph: preceding });
                    refs.insert(SourceRef::Native { glyph: following });
                }
            }
        }
        super::graph::charge(
            references_used,
            refs.len(),
            limits.max_references,
            "graph references",
        )?;
        all_sources.extend(refs.iter().copied());
        tokens.push(token);
        origins.push(refs.into_iter().collect());
        source_backed.push(backed);
    }
    let mut view = TextView {
        tokens,
        origins,
        source_backed,
        normalization: if block.issues.is_empty() {
            TextNormalization::Exact
        } else {
            TextNormalization::Unresolved {
                reason: "source normalization retains competing interpretations".into(),
            }
        },
    };
    view.certify_normalization(block, normalization_work);
    Ok((view, all_sources.into_iter().collect()))
}

impl EvidenceStore {
    /// Adapts a native extraction without asserting that visual, form, or
    /// relationship channels were inspected. Pages containing non-text paint
    /// cannot establish complete text inventory through native glyphs alone.
    /// The original document is moved,
    /// preserving raw glyph and vector evidence without normalization.
    ///
    /// # Errors
    /// Returns validation/resource errors for malformed provider identities,
    /// pages, glyphs, dependencies, or aggregate evidence limits.
    pub fn from_native(
        revision: String,
        backend: BackendIdentity,
        pages: Vec<PageEvidence>,
        extraction: ExtractionOutcome,
        limits: EvidenceLimits,
    ) -> Result<Self> {
        if backend.kind != BackendKind::NativeParser {
            return Err(invalid("native adapter requires a native parser identity"));
        }
        bounded(pages.len(), limits.max_pages, "evidence pages")?;
        bounded(
            extraction.document().items().len(),
            limits.max_items,
            "evidence items",
        )?;
        let (native, extraction_issues) = extraction.into_parts();
        let mut by_page = BTreeMap::<PageId, Vec<SourceRef>>::new();
        for glyph in native.items() {
            by_page
                .entry(glyph.page)
                .or_default()
                .push(SourceRef::Native { glyph: glyph.id });
        }
        let issues = extraction_issues
            .into_iter()
            .map(|issue| {
                let (kind, scope, reason) = issue.into_parts();
                let boundary = match scope {
                    ExtractionScope::GlyphGap { retained_before } => Some(
                        super::EvidenceBoundary::glyph_gap(&native, retained_before)?,
                    ),
                    ExtractionScope::PageGlyphGap {
                        page,
                        retained_before,
                    } => Some(super::EvidenceBoundary::page_glyph_gap(
                        &native,
                        page,
                        retained_before,
                    )?),
                    ExtractionScope::PageGap { retained_before } => {
                        Some(super::EvidenceBoundary::page_gap(&pages, retained_before)?)
                    }
                    _ => None,
                };
                let page = match (&boundary, scope) {
                    (Some(boundary), _) => boundary.page_scope(&native),
                    (_, ExtractionScope::Page(page)) => Some(page),
                    _ => None,
                };
                Ok(EvidenceIssue {
                    page,
                    channel: Channel::Text,
                    sources: Vec::new(),
                    boundary,
                    kind: match kind {
                        ExtractionIssueKind::Unsupported => EvidenceFailure::Unsupported,
                        ExtractionIssueKind::Unresolved => EvidenceFailure::Unresolved,
                    },
                    reason,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let incomplete_document = issues.iter().any(|issue| issue.page.is_none());
        let incomplete_pages = issues
            .iter()
            .filter_map(|issue| issue.page)
            .collect::<BTreeSet<_>>();
        let inventories = pages
            .iter()
            .map(|page| ChannelInventory {
                page: Some(page.page),
                channel: Channel::Text,
                backend: 0,
                sources: by_page.remove(&page.page).unwrap_or_default(),
                complete: !incomplete_document
                    && !incomplete_pages.contains(&page.page)
                    && !native.last_non_text_paint().contains_key(&page.page),
            })
            .collect();
        let store = Self {
            revision,
            backends: vec![backend],
            pages,
            native,
            rendered: Vec::new(),
            structured: Vec::new(),
            inventories,
            key_inventories: Vec::new(),
            issues,
        };
        store.validate(limits)?;
        Ok(store)
    }
}

impl DocumentGraph {
    /// Builds reversible text views through the existing layout/normalization
    /// adapter. Distinct trusted runs do not imply one global reading order.
    ///
    /// # Errors
    /// Preserves native layout and normalization errors and validates all graph
    /// references and configured limits before returning the proposed views.
    pub fn from_native(
        store: &EvidenceStore,
        options: PipelineOptions,
        evidence_limits: EvidenceLimits,
        graph_limits: GraphLimits,
    ) -> Result<Self> {
        store.validate(evidence_limits)?;
        let lines = reconstruct_lines(&store.native, options.line)?;
        let reconstructed = reconstruct_blocks_with_issues(&store.native, &lines, options.block)?;
        let blocks = normalize_blocks(&store.native, &lines, &reconstructed.blocks)?;
        let count = store
            .pages
            .len()
            .checked_add(blocks.len())
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| invalid("native graph node count overflows"))?;
        bounded(count, graph_limits.max_nodes, "graph nodes")?;
        let mut graph = Self {
            nodes: vec![GraphNode {
                id: NodeId(0),
                kind: NodeKind::Document,
                pages: Vec::new(),
                sources: Vec::new(),
                identity: None,
                basis: ViewBasis::NativeLayout,
                content: NodeContent::Container,
            }],
            relations_complete: reconstructed.issues.is_empty(),
            ..Self::default()
        };
        let mut page_nodes = BTreeMap::new();
        for page in &store.pages {
            let id = NodeId(graph.nodes.len() as u64);
            page_nodes.insert(page.page, id);
            graph.nodes.push(GraphNode {
                id,
                kind: NodeKind::Page,
                pages: vec![page.page],
                sources: Vec::new(),
                identity: None,
                basis: ViewBasis::NativeLayout,
                content: NodeContent::Container,
            });
            graph.edges.push(GraphEdge {
                from: NodeId(0),
                to: id,
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::NativeLayout,
            });
        }
        let block_start = graph.nodes.len();
        let mut tokens_used = 0usize;
        let mut references_used = 0usize;
        let mut normalization_work = graph_limits.max_normalization_work;
        for block in &blocks {
            let (view, sources) = block_text_view(
                block,
                graph_limits,
                &mut tokens_used,
                &mut references_used,
                &mut normalization_work,
            )?;
            let id = NodeId(graph.nodes.len() as u64);
            let kind = match block.role {
                BlockRole::Body => NodeKind::Paragraph,
                BlockRole::RepeatedHeader => NodeKind::Header,
                BlockRole::RepeatedFooter => NodeKind::Footer,
            };
            let pages = block.pages.iter().copied().map(PageId).collect::<Vec<_>>();
            let parent = if pages.len() == 1 {
                page_nodes.get(&pages[0]).copied().unwrap_or(NodeId(0))
            } else {
                NodeId(0)
            };
            graph.nodes.push(GraphNode {
                id,
                kind,
                pages,
                sources,
                identity: None,
                basis: ViewBasis::NativeLayout,
                content: NodeContent::Text { view },
            });
            graph.edges.push(GraphEdge {
                from: parent,
                to: id,
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::NativeLayout,
            });
        }
        graph.add_native_footer_views(
            &blocks,
            block_start,
            options,
            graph_limits,
            &mut tokens_used,
            &mut references_used,
        )?;
        let mut ordered = BTreeSet::new();
        for run in &reconstructed.trusted_run_descriptors {
            let mut intervals = run
                .trusted_block_indices
                .iter()
                .filter_map(|index| {
                    reconstructed
                        .trusted_run_intervals
                        .get(*index)
                        .copied()
                        .flatten()
                        .filter(|interval| interval.run_id == run.id)
                        .map(|interval| (*index, interval))
                })
                .collect::<Vec<_>>();
            intervals.sort_unstable_by_key(|(_, interval)| interval.start);
            for pair in intervals.windows(2) {
                if pair[0].1.end != pair[1].1.start {
                    continue;
                }
                let from = NodeId((block_start + pair[0].0) as u64);
                let to = NodeId((block_start + pair[1].0) as u64);
                if from != to && ordered.insert((from, to)) {
                    graph.edges.push(GraphEdge {
                        from,
                        to,
                        kind: EdgeKind::Precedes,
                        sources: Vec::new(),
                        basis: ViewBasis::NativeLayout,
                    });
                }
            }
        }
        // Normalization, visibility rules, or failed layout may leave original
        // evidence outside text views. Keep it explicitly represented.
        let represented = graph
            .nodes
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut omitted = BTreeMap::<PageId, Vec<SourceRef>>::new();
        for (source, page) in store.source_pages().filter(|(source, _)| {
            matches!(
                source,
                SourceRef::Native { .. } | SourceRef::NativeVector { .. }
            )
        }) {
            if !represented.contains(&source)
                && let Some(page) = page
            {
                omitted.entry(page).or_default().push(source);
            }
        }
        bounded(
            graph
                .nodes
                .len()
                .checked_add(omitted.len())
                .ok_or_else(|| invalid("native graph count overflows"))?,
            graph_limits.max_nodes,
            "graph nodes",
        )?;
        for (page, sources) in omitted {
            let id = NodeId(graph.nodes.len() as u64);
            graph.nodes.push(GraphNode {
                id,
                kind: NodeKind::Unknown,
                pages: vec![page],
                sources,
                identity: None,
                basis: ViewBasis::NativeLayout,
                content: NodeContent::Unknown,
            });
            graph.edges.push(GraphEdge {
                from: page_nodes.get(&page).copied().unwrap_or(NodeId(0)),
                to: id,
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::NativeLayout,
            });
        }
        super::ruled_tables::append(&mut graph, store, options, graph_limits)?;
        graph.validate(store, evidence_limits, graph_limits)?;
        Ok(graph)
    }
}
