//! Counterpart table axes propose reversible, inferred glyph partitions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    model::{Glyph, GlyphId, PageId, Rect},
    normalize::ComparableToken,
    pipeline::PipelineOptions,
};

use super::{
    DocumentComparisonLimits, DocumentGraph, EdgeKind, EvidenceStore, NodeContent, NodeId,
    NodeKind, SourceRef, TextNormalization, ViewBasis,
    ruled_tables::{Grid, GridInstallation, install_grid},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterpartTableView {
    pub counterpart_table: NodeId,
    pub target_table: NodeId,
    /// References in the counterpart document; never treated as target evidence.
    pub counterpart_sources: Vec<SourceRef>,
    /// Native target glyphs supporting the located header and row labels.
    pub anchor_sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableRefinements {
    pub old: Vec<CounterpartTableView>,
    pub new: Vec<CounterpartTableView>,
    pub exhaustive: bool,
}

struct Template {
    table: NodeId,
    columns: Vec<String>,
    rows: Vec<String>,
    sources: Vec<SourceRef>,
}

#[derive(Clone)]
struct Anchor {
    page: PageId,
    bounds: Rect,
    sources: BTreeSet<SourceRef>,
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

/// Proposes table views from unchanged, uniquely located axis labels and target
/// glyph geometry. Values are not used to locate or score a partition. Both
/// template sets are captured before refinement, preventing self-reinforcement.
/// One changed column label may be read literally from a single residual native
/// header view; competing views are rejected without choosing by text similarity.
/// Original blocks remain alternative partitions, and all new views are inferred.
///
/// # Errors
/// Rejects invalid input evidence/graphs or failed native cell reconstruction.
/// Bounded search exhaustion is reported without deleting original evidence.
pub fn refine_table_views(
    old: &mut DocumentGraph,
    new: &mut DocumentGraph,
    old_evidence: &EvidenceStore,
    new_evidence: &EvidenceStore,
    options: PipelineOptions,
    limits: DocumentComparisonLimits,
) -> Result<TableRefinements> {
    old.validate(old_evidence, limits.evidence, limits.graph)?;
    new.validate(new_evidence, limits.evidence, limits.graph)?;
    let mut budget = limits.graph.max_references;
    let old_templates = templates(old, &mut budget);
    let new_templates = templates(new, &mut budget);
    let (old_views, old_exhaustive) = apply(
        old,
        old_evidence,
        new_templates,
        options,
        limits,
        &mut budget,
    )?;
    let (new_views, new_exhaustive) = apply(
        new,
        new_evidence,
        old_templates,
        options,
        limits,
        &mut budget,
    )?;
    old.validate(old_evidence, limits.evidence, limits.graph)?;
    new.validate(new_evidence, limits.evidence, limits.graph)?;
    Ok(TableRefinements {
        old: old_views,
        new: new_views,
        exhaustive: old_exhaustive && new_exhaustive && budget != 0,
    })
}

fn templates(graph: &DocumentGraph, budget: &mut usize) -> Vec<Template> {
    let mut output = Vec::new();
    let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (node.id, node)).collect();
    for table in graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Table)
    {
        if !spend(
            budget,
            graph.edges.len().saturating_add(table.sources.len()),
        ) {
            break;
        }
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut sources: BTreeSet<_> = table.sources.iter().copied().collect();
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.from == table.id && edge.kind == EdgeKind::Contains)
        {
            let node = nodes[&edge.to];
            let Some(key) = &node.identity else { continue };
            if !spend(
                budget,
                key.value
                    .len()
                    .saturating_add(edge.sources.len())
                    .saturating_add(node.sources.len()),
            ) {
                return output;
            }
            match node.kind {
                NodeKind::Column => columns.push(key.value.clone()),
                NodeKind::Row => rows.push(key.value.clone()),
                _ => continue,
            }
            sources.extend(edge.sources.iter().chain(&node.sources).copied());
        }
        if columns.len() >= 2 && rows.len() >= 2 && columns.len().saturating_mul(rows.len()) <= 1024
        {
            output.push(Template {
                table: table.id,
                columns,
                rows,
                sources: sources.into_iter().collect(),
            });
        }
    }
    output
}

fn locate(
    label: &str,
    graph: &DocumentGraph,
    glyphs: &BTreeMap<GlyphId, &Glyph>,
    budget: &mut usize,
) -> Option<Anchor> {
    let wanted: Vec<_> = label.chars().map(ComparableToken::Scalar).collect();
    if wanted.is_empty() {
        return None;
    }
    let mut found: Option<Anchor> = None;
    for node in graph
        .nodes
        .iter()
        .filter(|node| node.basis == ViewBasis::NativeLayout)
    {
        let NodeContent::Text { view } = &node.content else {
            continue;
        };
        if view.normalization != TextNormalization::Exact {
            continue;
        }
        for (offset, tokens) in view.tokens.windows(wanted.len()).enumerate() {
            if !spend(budget, wanted.len()) {
                return None;
            }
            if tokens != wanted {
                continue;
            }
            let mut sources = BTreeSet::new();
            for origin in &view.origins[offset..offset + wanted.len()] {
                if !spend(budget, origin.len()) {
                    return None;
                }
                for source in origin {
                    if !matches!(source, SourceRef::Native { .. }) {
                        return None;
                    }
                    sources.insert(*source);
                }
            }
            let anchor = anchor_from_sources(sources, glyphs)?;
            if found
                .as_ref()
                .is_some_and(|previous| previous.sources != anchor.sources)
            {
                return None;
            }
            found = Some(anchor);
        }
    }
    found
}

fn anchor_from_sources(
    sources: BTreeSet<SourceRef>,
    glyphs: &BTreeMap<GlyphId, &Glyph>,
) -> Option<Anchor> {
    let mut bounds: Option<(PageId, Rect)> = None;
    for source in &sources {
        let SourceRef::Native { glyph } = source else {
            return None;
        };
        let glyph = glyphs.get(glyph)?;
        if let Some((page, area)) = &mut bounds {
            if *page != glyph.page {
                return None;
            }
            area.min.x = area.min.x.min(glyph.bbox.min.x);
            area.min.y = area.min.y.min(glyph.bbox.min.y);
            area.max.x = area.max.x.max(glyph.bbox.max.x);
            area.max.y = area.max.y.max(glyph.bbox.max.y);
        } else {
            bounds = Some((glyph.page, glyph.bbox));
        }
    }
    let (page, bounds) = bounds?;
    Some(Anchor {
        page,
        bounds,
        sources,
    })
}

// A missing column label may be read from one residual native header view.
// Its literal text is retained; neither cell values nor edit cost select it.
fn unmatched_header(
    graph: &DocumentGraph,
    glyphs: &BTreeMap<GlyphId, &Glyph>,
    anchors: &BTreeMap<&String, Anchor>,
    corner: &Anchor,
    row_right: f64,
    budget: &mut usize,
) -> Option<(Anchor, String)> {
    let known: BTreeSet<_> = anchors
        .values()
        .flat_map(|anchor| anchor.sources.iter().copied())
        .collect();
    let mut found: Option<(Anchor, String)> = None;
    for node in graph
        .nodes
        .iter()
        .filter(|node| node.basis == ViewBasis::NativeLayout)
    {
        let NodeContent::Text { view } = &node.content else {
            continue;
        };
        if view.normalization != TextNormalization::Exact {
            continue;
        }
        if !spend(budget, node.sources.len().saturating_add(view.tokens.len())) {
            return None;
        }
        let sources: BTreeSet<_> = node
            .sources
            .iter()
            .copied()
            .filter(|source| !known.contains(source))
            .collect();
        let Some(anchor) = anchor_from_sources(sources, glyphs) else {
            continue;
        };
        if anchor.page != corner.page
            || anchor.bounds.min.x <= row_right
            || anchor.sources.iter().any(|source| {
                let SourceRef::Native { glyph } = source else {
                    return true;
                };
                let bounds = glyphs[glyph].bbox;
                bounds.max.y <= corner.bounds.min.y || bounds.min.y >= corner.bounds.max.y
            })
        {
            continue;
        }
        let mut text = String::new();
        for (token, origins) in view.tokens.iter().zip(&view.origins) {
            if !spend(budget, origins.len()) {
                return None;
            }
            if !origins.is_empty() && origins.iter().all(|source| anchor.sources.contains(source)) {
                let ComparableToken::Scalar(character) = token else {
                    return None;
                };
                text.push(*character);
            }
        }
        if text.trim().is_empty() {
            continue;
        }
        if found
            .as_ref()
            .is_some_and(|(previous, label)| previous.sources != anchor.sources || label != &text)
        {
            return None;
        }
        found = Some((anchor, text));
    }
    found
}

fn proposal_grid(
    template: &Template,
    graph: &DocumentGraph,
    store: &EvidenceStore,
    glyphs: &BTreeMap<GlyphId, &Glyph>,
    budget: &mut usize,
) -> Option<Grid> {
    let common: Vec<_> = template
        .columns
        .iter()
        .filter(|label| template.rows.contains(label))
        .collect();
    if common.len() != 1 {
        return None;
    }
    let mut anchors = BTreeMap::new();
    for label in &template.rows {
        anchors.insert(label, locate(label, graph, glyphs, budget)?);
    }
    let mut missing = Vec::new();
    for label in &template.columns {
        if anchors.contains_key(label) {
            continue;
        }
        if let Some(anchor) = locate(label, graph, glyphs, budget) {
            anchors.insert(label, anchor);
        } else {
            missing.push(label);
        }
    }
    let replacement = match missing.as_slice() {
        [] => None,
        [label] => {
            let row_right = template
                .rows
                .iter()
                .map(|label| anchors[label].bounds.max.x)
                .fold(f64::NEG_INFINITY, f64::max);
            let (anchor, text) = unmatched_header(
                graph,
                glyphs,
                &anchors,
                &anchors[common[0]],
                row_right,
                budget,
            )?;
            anchors.insert(*label, anchor);
            Some((*label, text))
        }
        _ => return None,
    };
    let mut columns: Vec<_> = template
        .columns
        .iter()
        .map(|label| (label, &anchors[label]))
        .collect();
    let mut rows: Vec<_> = template
        .rows
        .iter()
        .map(|label| (label, &anchors[label]))
        .collect();
    columns.sort_by(|a, b| a.1.bounds.min.x.total_cmp(&b.1.bounds.min.x));
    rows.sort_by(|a, b| b.1.bounds.max.y.total_cmp(&a.1.bounds.max.y));
    if columns[0].0 != common[0] || rows[0].0 != common[0] {
        return None;
    }
    let page = columns[0].1.page;
    if anchors.values().any(|anchor| anchor.page != page) {
        return None;
    }
    let frame = store.pages.iter().find(|item| item.page == page)?.bounds?;
    let header_low = columns
        .iter()
        .map(|(_, anchor)| anchor.bounds.min.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let header_high = columns
        .iter()
        .map(|(_, anchor)| anchor.bounds.max.y)
        .fold(f64::INFINITY, f64::min);
    if header_low >= header_high {
        return None;
    }
    let row_right = rows
        .iter()
        .map(|(_, anchor)| anchor.bounds.max.x)
        .fold(f64::NEG_INFINITY, f64::max);
    if row_right >= columns[1].1.bounds.min.x {
        return None;
    }
    let row_column_cut = f64::midpoint(row_right, columns[1].1.bounds.min.x);
    let last = rows.last()?.1;
    if !spend(
        budget,
        store.native.items().len().saturating_mul(columns.len() + 1),
    ) {
        return None;
    }
    let bottom = store
        .native
        .items()
        .iter()
        .filter(|glyph| {
            glyph.page == page
                && glyph.bbox.max.y < last.bounds.min.y
                && glyph.bbox.min.x < row_column_cut
        })
        .map(|glyph| glyph.bbox.max.y)
        .fold(frame.min.y, f64::max);
    let top = columns
        .iter()
        .map(|(_, anchor)| anchor.bounds.max.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut ys = vec![top];
    ys.extend(rows.iter().skip(1).map(|(_, anchor)| anchor.bounds.max.y));
    ys.push(bottom);
    if ys.windows(2).any(|pair| pair[0] <= pair[1]) {
        return None;
    }
    let mut xs = vec![
        rows.iter()
            .map(|(_, anchor)| anchor.bounds.min.x)
            .fold(f64::INFINITY, f64::min),
    ];
    for adjacent in columns.windows(2) {
        let low = adjacent[0].1.bounds.max.x;
        let high = adjacent[1].1.bounds.min.x;
        if low >= high {
            return None;
        }
        let mut spans: Vec<_> = store
            .native
            .items()
            .iter()
            .filter(|glyph| {
                glyph.page == page
                    && glyph.bbox.min.y >= bottom
                    && glyph.bbox.max.y <= top
                    && glyph.bbox.max.x > low
                    && glyph.bbox.min.x < high
            })
            .map(|glyph| (glyph.bbox.min.x.max(low), glyph.bbox.max.x.min(high)))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cursor = low;
        let mut gap = (low, low);
        for (start, end) in spans.into_iter().chain(std::iter::once((high, high))) {
            if start - cursor > gap.1 - gap.0 {
                gap = (cursor, start);
            }
            cursor = cursor.max(end);
        }
        if gap.0 >= gap.1 {
            return None;
        }
        xs.push(f64::midpoint(gap.0, gap.1));
    }
    xs.push(frame.max.x);
    let structure_sources = anchors
        .values()
        .flat_map(|anchor| anchor.sources.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Some(Grid {
        page,
        xs,
        ys,
        tolerance: 0.0,
        structure_sources,
        expected_labels: Some((
            columns
                .iter()
                .map(|(label, _)| {
                    replacement
                        .as_ref()
                        .filter(|(original, _)| original == label)
                        .map_or_else(|| (*label).clone(), |(_, text)| text.clone())
                })
                .collect(),
            rows.iter().map(|(label, _)| (*label).clone()).collect(),
        )),
    })
}

fn apply(
    graph: &mut DocumentGraph,
    store: &EvidenceStore,
    templates: Vec<Template>,
    options: PipelineOptions,
    limits: DocumentComparisonLimits,
    budget: &mut usize,
) -> Result<(Vec<CounterpartTableView>, bool)> {
    let mut output = Vec::new();
    let mut exhaustive = true;
    let glyphs: BTreeMap<_, _> = store
        .native
        .items()
        .iter()
        .map(|glyph| (glyph.id, glyph))
        .collect();
    for template in templates {
        let Some(grid) = proposal_grid(&template, graph, store, &glyphs, budget) else {
            continue;
        };
        let anchor_sources = grid.structure_sources.clone();
        match install_grid(graph, store, grid, options, limits.graph, budget)? {
            GridInstallation::Installed(table) => output.push(CounterpartTableView {
                counterpart_table: template.table,
                target_table: table,
                counterpart_sources: template.sources,
                anchor_sources,
            }),
            GridInstallation::Unsupported => {}
            GridInstallation::Incomplete => exhaustive = false,
        }
    }
    if *budget == 0 {
        graph.relations_complete = false;
    }
    Ok((output, exhaustive && *budget != 0))
}
