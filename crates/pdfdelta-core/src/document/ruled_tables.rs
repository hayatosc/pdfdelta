//! Reversible rectangular-grid views from retained straight-path evidence.
//!
//! The first row and column propose header and row-label identities. These are
//! inferred interpretations. Original block views remain a complete alternative;
//! missing borders, ambiguous labels, and crossing glyphs keep the original layout.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Result,
    layout::{Block, BlockId, BlockRole, reconstruct_lines},
    model::{DecodedText, Document, Glyph, PageId, VectorLine},
    normalize::normalize_blocks,
    pipeline::PipelineOptions,
};

use super::{
    AlternativeViews, DocumentGraph, EdgeKind, EvidenceStore, GraphEdge, GraphLimits, GraphNode,
    IdentityKey, NodeContent, NodeId, NodeKind, SourceRef, TextNormalization, TextView, ViewBasis,
    native::block_text_view,
};

const BASIS: ViewBasis = ViewBasis::ReconstructedStructure;
const MAX_CELLS: usize = 1024;

pub(super) struct Grid {
    pub(super) page: PageId,
    pub(super) xs: Vec<f64>,
    pub(super) ys: Vec<f64>,
    pub(super) tolerance: f64,
    pub(super) structure_sources: Vec<SourceRef>,
    pub(super) expected_labels: Option<(Vec<String>, Vec<String>)>,
}

fn spend(budget: &mut usize, amount: usize) -> bool {
    if let Some(left) = budget.checked_sub(amount) {
        *budget = left;
        true
    } else {
        *budget = 0;
        false
    }
}

pub(super) fn append(
    graph: &mut DocumentGraph,
    store: &EvidenceStore,
    options: PipelineOptions,
    limits: GraphLimits,
) -> Result<()> {
    if store.native.vector_lines().is_empty() {
        return Ok(());
    }
    let mut budget = limits.max_references;
    let mut pages = BTreeMap::<PageId, Vec<&VectorLine>>::new();
    for line in store.native.vector_lines() {
        if ![line.from.x, line.from.y, line.to.x, line.to.y, line.width]
            .into_iter()
            .all(f64::is_finite)
        {
            continue;
        }
        pages.entry(line.page).or_default().push(line);
    }
    for (page, lines) in pages {
        for grid in grids(page, &lines, &mut budget) {
            install_grid(graph, store, grid, options, limits, &mut budget)?;
            if budget == 0 {
                graph.relations_complete = false;
                return Ok(());
            }
        }
    }
    Ok(())
}

pub(super) enum GridInstallation {
    Installed(NodeId),
    Unsupported,
    Incomplete,
}

pub(super) fn install_grid(
    graph: &mut DocumentGraph,
    store: &EvidenceStore,
    grid: Grid,
    options: PipelineOptions,
    limits: GraphLimits,
    budget: &mut usize,
) -> Result<GridInstallation> {
    if !spend(
        budget,
        store
            .native
            .items()
            .len()
            .saturating_add(graph.nodes.iter().fold(0_usize, |count, node| {
                count
                    .saturating_add(node.sources.len().saturating_mul(3))
                    .saturating_add(1)
            })),
    ) {
        graph.relations_complete = false;
        return Ok(GridInstallation::Incomplete);
    }
    let cells = match cell_views(&grid, store, options, limits, budget) {
        Ok(Some(cells)) => cells,
        Ok(None) => {
            return Ok(if *budget == 0 {
                GridInstallation::Incomplete
            } else {
                GridInstallation::Unsupported
            });
        }
        Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {
            graph.relations_complete = false;
            return Ok(GridInstallation::Incomplete);
        }
        Err(error) => return Err(error),
    };
    let sources: BTreeSet<_> = cells
        .iter()
        .flat_map(|(_, sources)| sources.iter().copied())
        .collect();
    let mut originals = Vec::new();
    let mut represented = BTreeSet::new();
    let mut valid = true;
    // Native structure tags retain their own overlapping evidence views. Only
    // the layout partition is replaced; inferred partitions still conflict.
    for node in graph
        .nodes
        .iter()
        .filter(|node| node.basis != ViewBasis::SourceStructure)
    {
        if node.sources.iter().any(|source| sources.contains(source)) {
            if node.basis != ViewBasis::NativeLayout
                || !matches!(node.content, NodeContent::Text { .. })
                || node
                    .sources
                    .iter()
                    .any(|source| !sources.contains(source) || !represented.insert(*source))
            {
                valid = false;
                break;
            }
            originals.push(node.id);
        }
    }
    if !valid || represented != sources {
        return Ok(GridInstallation::Unsupported);
    }
    let rows = grid.ys.len() - 1;
    let columns = grid.xs.len() - 1;
    let headers = cells[..columns]
        .iter()
        .map(|(view, _)| view.display_text())
        .collect::<Option<Vec<_>>>();
    let labels = cells
        .chunks(columns)
        .map(|row| row[0].0.display_text())
        .collect::<Option<Vec<_>>>();
    let (Some(headers), Some(labels)) = (headers, labels) else {
        return Ok(GridInstallation::Unsupported);
    };
    if !unique_labels(&headers) || !unique_labels(&labels) {
        return Ok(GridInstallation::Unsupported);
    }
    if grid
        .expected_labels
        .as_ref()
        .is_some_and(|expected| expected.0 != headers || expected.1 != labels)
    {
        return Ok(GridInstallation::Unsupported);
    }
    let added = 2 + rows + columns + cells.len();
    let extra_references = grid
        .structure_sources
        .len()
        .saturating_mul(2 * (rows + columns) + 3 * cells.len() + 1)
        .saturating_add(sources.len().saturating_mul(5))
        .saturating_add(added.saturating_mul(2));
    let existing_labels = graph
        .nodes
        .iter()
        .filter_map(|node| node.identity.as_ref())
        .fold(0_usize, |bytes, key| {
            bytes
                .saturating_add(key.namespace.len())
                .saturating_add(key.value.len())
        });
    let label_bytes = headers
        .iter()
        .chain(&labels)
        .fold(existing_labels, |bytes, label| {
            bytes
                .saturating_add(label.len().saturating_mul(2))
                .saturating_add(64)
        });
    if graph.nodes.len().saturating_add(added) > limits.max_nodes
        || graph
            .edges
            .len()
            .saturating_add(added.saturating_mul(4))
            .saturating_add(originals.len())
            > limits.max_edges
        || label_bytes > limits.max_label_bytes
        || !spend(budget, extra_references)
    {
        graph.relations_complete = false;
        return Ok(GridInstallation::Incomplete);
    }
    let table = NodeId(graph.nodes.len() as u64);
    install(graph, grid, cells, sources, originals, headers, labels);
    Ok(GridInstallation::Installed(table))
}

fn unique_labels(labels: &[String]) -> bool {
    labels.iter().all(|label| !label.trim().is_empty())
        && labels.iter().collect::<BTreeSet<_>>().len() == labels.len()
}

fn grids(page: PageId, lines: &[&VectorLine], budget: &mut usize) -> Vec<Grid> {
    let mut result = Vec::new();
    let mut consumed = BTreeSet::new();
    for seed in lines {
        let left = seed.from.x.min(seed.to.x);
        let right = seed.from.x.max(seed.to.x);
        let tolerance = ((right - left) * 1e-6).max(seed.width.abs());
        if right - left <= tolerance
            || (seed.from.y - seed.to.y).abs() > tolerance
            || consumed.contains(&seed.id)
        {
            continue;
        }
        if !spend(budget, lines.len()) {
            break;
        }
        let mut horizontal: Vec<_> = lines
            .iter()
            .copied()
            .filter(|line| {
                (line.from.y - line.to.y).abs() <= tolerance
                    && (line.from.x.min(line.to.x) - left).abs() <= tolerance
                    && (line.from.x.max(line.to.x) - right).abs() <= tolerance
            })
            .collect();
        horizontal.sort_by(|a, b| b.from.y.total_cmp(&a.from.y));
        horizontal.dedup_by(|a, b| (a.from.y - b.from.y).abs() <= tolerance);
        if horizontal.len() < 3 {
            continue;
        }
        consumed.extend(horizontal.iter().map(|line| line.id));
        let mut start = 0;
        for end in 1..=horizontal.len() {
            let connected = end < horizontal.len()
                && covers(
                    lines,
                    left,
                    horizontal[end].from.y,
                    horizontal[end - 1].from.y,
                    tolerance,
                    budget,
                )
                && covers(
                    lines,
                    right,
                    horizontal[end].from.y,
                    horizontal[end - 1].from.y,
                    tolerance,
                    budget,
                );
            if connected {
                continue;
            }
            if end - start >= 3 && spend(budget, lines.len()) {
                let top = horizontal[start].from.y;
                let bottom = horizontal[end - 1].from.y;
                let mut xs: Vec<_> = lines
                    .iter()
                    .filter(|line| {
                        (line.from.x - line.to.x).abs() <= tolerance
                            && line.from.x >= left - tolerance
                            && line.from.x <= right + tolerance
                    })
                    .map(|line| line.from.x)
                    .collect();
                xs.sort_by(f64::total_cmp);
                xs.dedup_by(|a, b| (*a - *b).abs() <= tolerance);
                xs.retain(|x| covers(lines, *x, bottom, top, tolerance, budget));
                if xs.len() >= 3
                    && (xs[0] - left).abs() <= tolerance
                    && (xs[xs.len() - 1] - right).abs() <= tolerance
                    && (xs.len() - 1).saturating_mul(end - start - 1) <= MAX_CELLS
                {
                    result.push(Grid {
                        page,
                        expected_labels: None,
                        xs,
                        ys: horizontal[start..end]
                            .iter()
                            .map(|line| line.from.y)
                            .collect(),
                        tolerance,
                        structure_sources: lines
                            .iter()
                            .filter(|line| {
                                line.from.x.min(line.to.x) >= left - tolerance
                                    && line.from.x.max(line.to.x) <= right + tolerance
                                    && line.from.y.min(line.to.y) >= bottom - tolerance
                                    && line.from.y.max(line.to.y) <= top + tolerance
                            })
                            .map(|line| SourceRef::NativeVector { line: line.id })
                            .collect(),
                    });
                }
            }
            start = end;
        }
    }
    result
}

fn covers(
    lines: &[&VectorLine],
    x: f64,
    bottom: f64,
    top: f64,
    tolerance: f64,
    budget: &mut usize,
) -> bool {
    if !spend(budget, lines.len()) {
        return false;
    }
    let mut intervals: Vec<_> = lines
        .iter()
        .filter(|line| (line.from.x - x).abs() <= tolerance && (line.to.x - x).abs() <= tolerance)
        .map(|line| (line.from.y.min(line.to.y), line.from.y.max(line.to.y)))
        .collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut reached = bottom;
    for (low, high) in intervals {
        if low > reached + tolerance {
            break;
        }
        reached = reached.max(high);
        if reached >= top - tolerance {
            return true;
        }
    }
    false
}

type CellView = (TextView, Vec<SourceRef>);

fn cell_views(
    grid: &Grid,
    store: &EvidenceStore,
    options: PipelineOptions,
    limits: GraphLimits,
    budget: &mut usize,
) -> Result<Option<Vec<CellView>>> {
    let columns = grid.xs.len() - 1;
    let mut cells: Vec<Vec<Glyph>> = vec![Vec::new(); columns * (grid.ys.len() - 1)];
    for glyph in store
        .native
        .items()
        .iter()
        .filter(|glyph| glyph.page == grid.page)
    {
        let x = (glyph.bbox.min.x + glyph.bbox.max.x) / 2.0;
        let y = (glyph.bbox.min.y + glyph.bbox.max.y) / 2.0;
        if x < grid.xs[0]
            || x > grid.xs[columns]
            || y > grid.ys[0]
            || y < grid.ys[grid.ys.len() - 1]
        {
            continue;
        }
        if !spend(budget, grid.xs.len().saturating_add(grid.ys.len())) {
            return Ok(None);
        }
        let Some(column) = grid
            .xs
            .windows(2)
            .position(|bounds| bounds[0] <= x && x <= bounds[1])
        else {
            return Ok(None);
        };
        let Some(row) = grid
            .ys
            .windows(2)
            .position(|bounds| bounds[1] <= y && y <= bounds[0])
        else {
            return Ok(None);
        };
        let DecodedText::Mapped(text) = &glyph.text else {
            return Ok(None);
        };
        if glyph.bbox.min.x < grid.xs[column] - grid.tolerance
            || glyph.bbox.max.x > grid.xs[column + 1] + grid.tolerance
            || glyph.bbox.min.y < grid.ys[row + 1] - grid.tolerance
            || glyph.bbox.max.y > grid.ys[row] + grid.tolerance
            || glyph.direction.x <= 0.0
            || glyph.direction.y.abs() > glyph.direction.x * 1e-6
            || !spend(
                budget,
                1_usize
                    .saturating_add(text.len())
                    .saturating_add(glyph.raw_code.len()),
            )
        {
            return Ok(None);
        }
        cells[row * columns + column].push(glyph.clone());
    }
    let mut output = Vec::new();
    let mut tokens = 0;
    let mut references = 0;
    for glyphs in cells {
        if glyphs.is_empty() {
            return Ok(None);
        }
        let sources = glyphs
            .iter()
            .map(|glyph| SourceRef::Native { glyph: glyph.id })
            .collect();
        let document = Document::new(glyphs);
        let lines = reconstruct_lines(&document, options.line)?;
        let blocks = [Block {
            id: BlockId(0),
            lines: lines.iter().map(|line| line.id).collect(),
            role: BlockRole::Body,
        }];
        let normalized = normalize_blocks(&document, &lines, &blocks)?;
        let Some(block) = normalized.first() else {
            return Ok(None);
        };
        let (view, _) = block_text_view(block, limits, &mut tokens, &mut references)?;
        if view.normalization != TextNormalization::Exact || view.tokens.is_empty() {
            return Ok(None);
        }
        if !spend(
            budget,
            view.origins
                .iter()
                .fold(view.tokens.len(), |count, origins| {
                    count.saturating_add(origins.len())
                }),
        ) {
            return Ok(None);
        }
        output.push((view, sources));
    }
    Ok(Some(output))
}

fn install(
    graph: &mut DocumentGraph,
    grid: Grid,
    cells: Vec<CellView>,
    sources: BTreeSet<SourceRef>,
    originals: Vec<NodeId>,
    headers: Vec<String>,
    labels: Vec<String>,
) {
    let table = NodeId(graph.nodes.len() as u64);
    let identity = headers
        .iter()
        .map(|header| format!("{}:{header}", header.len()))
        .collect();
    graph.nodes.push(node(
        table,
        NodeKind::Table,
        grid.page,
        sources.into_iter().collect(),
        Some(("ruled-table-headers-v1", identity)),
        NodeContent::Container,
    ));
    graph.edges.push(edge(
        NodeId(0),
        table,
        EdgeKind::Contains,
        grid.structure_sources.clone(),
    ));
    let archive = NodeId(graph.nodes.len() as u64);
    graph.nodes.push(node(
        archive,
        NodeKind::Unknown,
        grid.page,
        Vec::new(),
        None,
        NodeContent::Container,
    ));
    graph
        .edges
        .push(edge(table, archive, EdgeKind::Contains, Vec::new()));
    let original_ids: BTreeSet<_> = originals.iter().copied().collect();
    graph
        .edges
        .retain(|edge| !(edge.kind == EdgeKind::Contains && original_ids.contains(&edge.to)));
    for id in &originals {
        graph
            .edges
            .push(edge(archive, *id, EdgeKind::Contains, Vec::new()));
    }
    let mut columns = Vec::new();
    for header in headers {
        let id = NodeId(graph.nodes.len() as u64);
        columns.push(id);
        graph.nodes.push(node(
            id,
            NodeKind::Column,
            grid.page,
            Vec::new(),
            Some(("ruled-column-label-v1", header)),
            NodeContent::Container,
        ));
        graph.edges.push(edge(
            table,
            id,
            EdgeKind::Contains,
            grid.structure_sources.clone(),
        ));
    }
    let mut rows = Vec::new();
    for label in labels {
        let id = NodeId(graph.nodes.len() as u64);
        rows.push(id);
        graph.nodes.push(node(
            id,
            NodeKind::Row,
            grid.page,
            Vec::new(),
            Some(("ruled-row-label-v1", label)),
            NodeContent::Container,
        ));
        graph.edges.push(edge(
            table,
            id,
            EdgeKind::Contains,
            grid.structure_sources.clone(),
        ));
    }
    for axis in [&rows, &columns] {
        for pair in axis.windows(2) {
            graph.edges.push(edge(
                pair[0],
                pair[1],
                EdgeKind::Precedes,
                grid.structure_sources.clone(),
            ));
        }
    }
    let mut cell_ids = Vec::new();
    for (index, (view, sources)) in cells.into_iter().enumerate() {
        let id = NodeId(graph.nodes.len() as u64);
        cell_ids.push(id);
        graph.nodes.push(node(
            id,
            NodeKind::Cell,
            grid.page,
            sources,
            None,
            NodeContent::Text { view },
        ));
        graph.edges.push(edge(
            rows[index / columns.len()],
            id,
            EdgeKind::Contains,
            grid.structure_sources.clone(),
        ));
        graph.edges.push(edge(
            rows[index / columns.len()],
            id,
            EdgeKind::RowMember,
            grid.structure_sources.clone(),
        ));
        graph.edges.push(edge(
            columns[index % columns.len()],
            id,
            EdgeKind::ColumnMember,
            grid.structure_sources.clone(),
        ));
    }
    graph.alternatives.push(AlternativeViews {
        parent: table,
        partitions: vec![originals, cell_ids],
    });
    graph.relations_complete = false;
}

fn node(
    id: NodeId,
    kind: NodeKind,
    page: PageId,
    sources: Vec<SourceRef>,
    identity: Option<(&str, String)>,
    content: NodeContent,
) -> GraphNode {
    GraphNode {
        id,
        kind,
        pages: vec![page],
        sources,
        identity: identity.map(|(namespace, value)| IdentityKey {
            namespace: namespace.into(),
            value,
        }),
        basis: BASIS,
        content,
    }
}

fn edge(from: NodeId, to: NodeId, kind: EdgeKind, sources: Vec<SourceRef>) -> GraphEdge {
    GraphEdge {
        from,
        to,
        kind,
        sources,
        basis: BASIS,
    }
}
