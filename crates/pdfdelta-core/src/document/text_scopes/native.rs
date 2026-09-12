//! Local native text closure does not require unrelated page orders to be known.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Result,
    model::{
        Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus, PageId, Rect, TextRenderMode, Vec2,
    },
};

use super::{
    DocumentComparisonLimits, DocumentView, EdgeKind, GraphNode, NodeContent, NodeId, SourceRef,
    source_children, spend,
};
use crate::document::{
    BackendKind, Channel, EvidenceBoundary, EvidenceFailure, NodeKind, ViewBasis,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Closure {
    WholePage,
    BoundedPaint,
}

/// An exact external continuation defeats an interval's apparent truncation.
/// This only declines a review; it does not establish a move or own its sources.
pub(super) fn external_continuation(
    view: DocumentView<'_>,
    interior: &[&GraphNode],
    shorter: &str,
    longer: &str,
    remaining: &mut usize,
) -> Option<bool> {
    spend(
        remaining,
        shorter.len().saturating_mul(2).saturating_add(longer.len()),
    )?;
    let remainder = longer
        .strip_prefix(shorter)
        .or_else(|| longer.strip_suffix(shorter));
    let Some(remainder) = remainder
        .map(|text| text.trim_matches(' '))
        .filter(|text| !text.is_empty())
    else {
        return Some(false);
    };
    for node in &view.graph.nodes {
        spend(remaining, interior.len().saturating_add(1))?;
        if interior.iter().any(|member| member.id == node.id) {
            continue;
        }
        let NodeContent::Text { view } = &node.content else {
            continue;
        };
        spend(remaining, view.tokens.len().saturating_mul(4))?;
        if view
            .display_text()
            .is_some_and(|text| text.trim_matches(' ') == remainder)
        {
            return Some(true);
        }
    }
    Some(false)
}

/// Runs are discovery paths only. A path becomes a closed interval only after
/// its complete native source band has been checked by [`Sources::closed`].
pub(super) fn runs<'a>(
    view: DocumentView<'a>,
    root: NodeId,
    sources: &Sources<'a>,
    limits: DocumentComparisonLimits,
    remaining: &mut usize,
) -> Result<Vec<Vec<&'a GraphNode>>> {
    if spend(
        remaining,
        view.graph
            .nodes
            .len()
            .saturating_add(view.graph.edges.len()),
    )
    .is_none()
    {
        return Ok(Vec::new());
    }
    let nodes: BTreeMap<_, _> = source_children(view.graph, root, limits.matching.channels)?
        .into_iter()
        .filter(|node| {
            node.basis == ViewBasis::NativeLayout
                && node.kind == NodeKind::Paragraph
                && matches!(node.content, NodeContent::Text { .. })
        })
        .map(|node| (node.id, node))
        .collect();
    let mut next: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    let mut previous: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    let mut blocked = BTreeSet::new();
    for edge in &view.graph.edges {
        if edge.kind != EdgeKind::Precedes {
            continue;
        }
        if edge.basis != ViewBasis::NativeLayout
            || !nodes.contains_key(&edge.from)
            || !nodes.contains_key(&edge.to)
        {
            blocked.extend([edge.from, edge.to]);
        } else {
            next.entry(edge.from).or_default().insert(edge.to);
            previous.entry(edge.to).or_default().insert(edge.from);
        }
    }
    for (&node, edges) in next.iter().chain(&previous) {
        if edges.len() != 1 {
            blocked.insert(node);
        }
    }
    if sources
        .bridge_runs(&nodes, &blocked, &mut next, &mut previous, remaining)
        .is_none()
    {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    let mut visited = BTreeSet::new();
    for &start in nodes.keys() {
        if blocked.contains(&start)
            || previous
                .get(&start)
                .is_some_and(|edges| edges.iter().any(|node| !blocked.contains(node)))
        {
            continue;
        }
        let mut run = Vec::new();
        let mut cursor = start;
        while !blocked.contains(&cursor) && visited.insert(cursor) {
            run.push(nodes[&cursor]);
            let Some(successor) = next.get(&cursor).and_then(|edges| edges.first()) else {
                break;
            };
            cursor = *successor;
        }
        if run.len() >= 3 {
            runs.push(run);
        }
    }
    Ok(runs)
}

pub(super) struct Sources<'a> {
    glyphs: BTreeMap<GlyphId, &'a Glyph>,
    pages: BTreeMap<PageId, Vec<&'a Glyph>>,
}

struct NodeGeometry {
    page: PageId,
    bounds: Rect,
    first_render: u32,
    last_render: u32,
}

impl<'a> Sources<'a> {
    fn geometry(&self, node: &GraphNode) -> Option<NodeGeometry> {
        let [page] = node.pages.as_slice() else {
            return None;
        };
        let mut geometry = None::<NodeGeometry>;
        for source in &node.sources {
            let SourceRef::Native { glyph } = source else {
                return None;
            };
            let glyph = self.glyphs.get(glyph)?;
            if glyph.page != *page || glyph.direction != (Vec2 { x: 1.0, y: 0.0 }) {
                return None;
            }
            if let Some(current) = &mut geometry {
                current.bounds.min.x = current.bounds.min.x.min(glyph.bbox.min.x);
                current.bounds.min.y = current.bounds.min.y.min(glyph.bbox.min.y);
                current.bounds.max.x = current.bounds.max.x.max(glyph.bbox.max.x);
                current.bounds.max.y = current.bounds.max.y.max(glyph.bbox.max.y);
                current.first_render = current.first_render.min(glyph.render_order);
                current.last_render = current.last_render.max(glyph.render_order);
            } else {
                geometry = Some(NodeGeometry {
                    page: *page,
                    bounds: glyph.bbox,
                    first_render: glyph.render_order,
                    last_render: glyph.render_order,
                });
            }
        }
        geometry
    }

    fn bridge_runs(
        &self,
        nodes: &BTreeMap<NodeId, &GraphNode>,
        blocked: &BTreeSet<NodeId>,
        next: &mut BTreeMap<NodeId, BTreeSet<NodeId>>,
        previous: &mut BTreeMap<NodeId, BTreeSet<NodeId>>,
        remaining: &mut usize,
    ) -> Option<()> {
        let mut geometry = BTreeMap::new();
        let mut page_nodes = BTreeMap::<PageId, Vec<NodeId>>::new();
        for (&id, node) in nodes {
            if blocked.contains(&id) || (next.contains_key(&id) && previous.contains_key(&id)) {
                continue;
            }
            spend(remaining, node.sources.len())?;
            if let Some(bounds) = self.geometry(node) {
                if !previous.contains_key(&id) {
                    page_nodes.entry(bounds.page).or_default().push(id);
                }
                geometry.insert(id, bounds);
            }
        }
        for ids in page_nodes.values_mut() {
            let mut work = 0usize;
            ids.sort_by(|a, b| {
                work += 1;
                geometry[b]
                    .bounds
                    .max
                    .y
                    .total_cmp(&geometry[a].bounds.max.y)
            });
            spend(remaining, work)?;
        }
        let mut proposals = BTreeMap::<NodeId, Vec<NodeId>>::new();
        // Cross-page endpoints cannot be adjacent here. The page index avoids
        // spending the discovery budget on those impossible pairs.
        for (&from, a) in &geometry {
            if next.contains_key(&from) {
                continue;
            }
            let mut best = None::<(NodeId, f64)>;
            let mut tied = false;
            let Some(starts) = page_nodes.get(&a.page) else {
                continue;
            };
            let mut work = 0usize;
            let first = starts.partition_point(|id| {
                work += 1;
                geometry[id].bounds.max.y >= a.bounds.min.y
            });
            spend(remaining, work)?;
            for &to in &starts[first..] {
                spend(remaining, 1)?;
                let b = &geometry[&to];
                if best.is_some_and(|(_, top)| b.bounds.max.y < top) {
                    break;
                }
                if a.last_render >= b.first_render
                    || a.bounds.min.x >= b.bounds.max.x
                    || a.bounds.max.x <= b.bounds.min.x
                {
                    continue;
                }
                match best {
                    Some((_, top)) if b.bounds.max.y == top => tied = true,
                    _ => {
                        best = Some((to, b.bounds.max.y));
                        tied = false;
                    }
                }
            }
            if let Some((to, _)) = best
                && !tied
            {
                proposals.entry(to).or_default().push(from);
            }
        }
        for (to, candidates) in proposals {
            spend(remaining, candidates.len().saturating_mul(2))?;
            let from = candidates.iter().min_by(|a, b| {
                geometry[a]
                    .bounds
                    .min
                    .y
                    .total_cmp(&geometry[b].bounds.min.y)
            })?;
            let nearest = geometry[from].bounds.min.y;
            if candidates
                .iter()
                .filter(|id| geometry[id].bounds.min.y == nearest)
                .count()
                != 1
            {
                continue;
            }
            // These are candidate discovery edges only. The unchanged boundary
            // checks and complete source/paint-band closure remain mandatory.
            next.entry(*from).or_default().insert(to);
            previous.entry(to).or_default().insert(*from);
        }
        Some(())
    }

    pub(super) fn new(view: DocumentView<'a>, remaining: &mut usize) -> Option<Self> {
        spend(
            remaining,
            view.evidence.native.items().len().saturating_mul(2),
        )?;
        let mut index = Self {
            glyphs: BTreeMap::new(),
            pages: BTreeMap::new(),
        };
        for glyph in view.evidence.native.items() {
            index.glyphs.insert(glyph.id, glyph);
            index.pages.entry(glyph.page).or_default().push(glyph);
        }
        Some(index)
    }

    /// Every native glyph whose baseline is in this horizontal page band must
    /// belong to the retained path, including both boundary nodes. This catches
    /// detached, omitted, duplicated, clipped and differently directed material
    /// without treating two matching anchors as a semantic identity certificate.
    pub(super) fn closed(
        &self,
        view: DocumentView<'_>,
        root: NodeId,
        path: &[&GraphNode],
        remaining: &mut usize,
    ) -> Option<Closure> {
        let [page] = path[0].pages.as_slice() else {
            return None;
        };
        spend(
            remaining,
            view.evidence
                .inventories
                .len()
                .saturating_add(view.evidence.issues.len()),
        )?;
        let mut sources = BTreeSet::new();
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut ink_bottom = f64::INFINITY;
        let mut ink_top = f64::NEG_INFINITY;
        let mut previous_bottom = f64::INFINITY;
        for node in path {
            if node.pages != [*page]
                || node.sources.is_empty()
                || spend(remaining, node.sources.len()).is_none()
            {
                return None;
            }
            let mut top = f64::NEG_INFINITY;
            let mut bottom = f64::INFINITY;
            for source in &node.sources {
                let SourceRef::Native { glyph } = source else {
                    return None;
                };
                let glyph = self.glyphs.get(glyph)?;
                if glyph.page != *page
                    || glyph.direction != (Vec2 { x: 1.0, y: 0.0 })
                    || glyph.crop_status != GlyphCropStatus::Inside
                    || !matches!(
                        glyph.path_clip_status,
                        GlyphPathClipStatus::Unclipped | GlyphPathClipStatus::Inside
                    )
                    || !matches!(
                        glyph.render_mode,
                        TextRenderMode::Fill
                            | TextRenderMode::Stroke
                            | TextRenderMode::FillAndStroke
                    )
                    || !sources.insert(*source)
                {
                    return None;
                }
                min_x = min_x.min(glyph.bbox.min.x);
                max_x = max_x.max(glyph.bbox.max.x);
                ink_bottom = ink_bottom.min(glyph.bbox.min.y);
                ink_top = ink_top.max(glyph.bbox.max.y);
                top = top.max(glyph.baseline.y);
                bottom = bottom.min(glyph.baseline.y);
            }
            // Deliberate: the first native convention admits strictly descending
            // horizontal blocks; rotated or interleaved baselines need another
            // source closure, not a guessed order or a pixel-distance threshold.
            if top >= previous_bottom {
                return None;
            }
            previous_bottom = bottom;
            min_y = min_y.min(bottom);
            max_y = max_y.max(top);
        }
        for glyph in self.pages.get(page).into_iter().flatten() {
            spend(remaining, 1)?;
            if glyph.baseline.y >= min_y
                && glyph.baseline.y <= max_y
                && glyph.bbox.max.x >= min_x
                && glyph.bbox.min.x <= max_x
                && !sources.contains(&SourceRef::Native { glyph: glyph.id })
            {
                return None;
            }
        }
        let members: BTreeSet<_> = path.iter().map(|node| node.id).collect();
        for alternative in &view.graph.alternatives {
            if alternative.parent == root || members.contains(&alternative.parent) {
                return None;
            }
            for partition in &alternative.partitions {
                if spend(remaining, partition.len()).is_none()
                    || partition.iter().any(|id| members.contains(id))
                {
                    return None;
                }
            }
        }
        for conflict in &view.graph.source_conflicts {
            if spend(remaining, conflict.sources.len()).is_none()
                || conflict
                    .sources
                    .iter()
                    .any(|source| sources.contains(source))
            {
                return None;
            }
        }
        if view.evidence.inventory_complete(Some(*page), Channel::Text) {
            Some(Closure::WholePage)
        } else {
            self.paint_closed(
                view,
                *page,
                Rect {
                    min: Vec2 {
                        x: min_x,
                        y: ink_bottom,
                    },
                    max: Vec2 {
                        x: max_x,
                        y: ink_top,
                    },
                },
                remaining,
            )
        }
    }

    fn paint_closed(
        &self,
        view: DocumentView<'_>,
        page: PageId,
        band: Rect,
        remaining: &mut usize,
    ) -> Option<Closure> {
        let evidence = view.evidence;
        let paints = evidence.native.non_text_paint_bounds()?;
        if !evidence.native.last_non_text_paint().contains_key(&page)
            || evidence.issues.iter().any(|issue| {
                if issue.channel != Channel::Text
                    || (issue.page.is_some() && issue.page != Some(page))
                {
                    return false;
                }
                // A failed invocation remains an incomplete inventory. Only an
                // explicit finite bound can make its missing ink local; the
                // paint loop below must still prove separation from this band.
                !(issue.page == Some(page)
                    && issue.sources.is_empty()
                    && matches!(issue.kind, EvidenceFailure::Unsupported | EvidenceFailure::Unresolved)
                    && matches!(issue.boundary,
                        Some(EvidenceBoundary::PageGlyphGap { page: failed_page, paint_index: Some(index), .. })
                            if failed_page == page && paints.get(index)
                                .is_some_and(|paint| paint.page == page && paint.bounds.is_some())))
            })
        {
            return None;
        }
        let glyphs = self.pages.get(&page)?;
        spend(remaining, glyphs.len())?;
        let expected: BTreeSet<_> = glyphs
            .iter()
            .map(|glyph| SourceRef::Native { glyph: glyph.id })
            .collect();
        let mut found = false;
        for inventory in &evidence.inventories {
            if inventory.channel != Channel::Text
                || (inventory.page.is_some() && inventory.page != Some(page))
            {
                continue;
            }
            // Only a native acquisition with explicitly bounded opaque paint can
            // explain this local exception. Other incomplete providers or a
            // missing native glyph remain uncertainty, even outside the band.
            if inventory.page != Some(page)
                || evidence.backends.get(inventory.backend)?.kind != BackendKind::NativeParser
            {
                return None;
            }
            spend(remaining, inventory.sources.len())?;
            if inventory.sources.len() != expected.len()
                || inventory.sources.iter().copied().collect::<BTreeSet<_>>() != expected
            {
                return None;
            }
            found = true;
        }
        if !found {
            return None;
        }
        for paint in paints {
            spend(remaining, 1)?;
            if paint.page != page {
                continue;
            }
            let bounds = paint.bounds?;
            if bounds.max.x >= band.min.x
                && bounds.min.x <= band.max.x
                && bounds.max.y >= band.min.y
                && bounds.min.y <= band.max.y
            {
                return None;
            }
        }
        Some(Closure::BoundedPaint)
    }
}
