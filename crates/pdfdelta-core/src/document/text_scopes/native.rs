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

mod census;
mod hanging;
mod projection;
mod segments;

pub use segments::{NativeRegion, NativeRegionChain, NativeRegionChains, NativeTransition};

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Closure {
    WholePage,
    BoundedPaint,
    Segmented(NativeRegionChain),
}

impl Closure {
    pub(super) fn bounded_paint(&self) -> bool {
        match self {
            Self::WholePage => false,
            Self::BoundedPaint => true,
            Self::Segmented(chain) => chain.regions.iter().any(|region| region.bounded_paint),
        }
    }

    pub(super) fn chain(self) -> Option<NativeRegionChain> {
        match self {
            Self::Segmented(chain) => Some(chain),
            _ => None,
        }
    }
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
    rows: bool,
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
    let mut connected = BTreeSet::new();
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
            connected.extend([edge.from, edge.to]);
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
    segments::bridge(view, &nodes, &blocked, &mut next, &mut previous, remaining);
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
        if !run.is_empty() {
            runs.push(run);
        }
    }
    if rows {
        let _ = hanging::attach(sources, &mut runs, &connected, remaining);
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
    top: f64,
    bottom: f64,
}

impl<'a> Sources<'a> {
    pub(super) fn census(
        &self,
        view: DocumentView<'_>,
        root: NodeId,
        path: &[&GraphNode],
        remaining: &mut usize,
        rows: bool,
    ) -> Option<(Closure, Vec<SourceRef>, bool)> {
        census::checked(self, view, root, path, remaining, rows)
    }

    pub(super) fn project_census(
        &self,
        node: &GraphNode,
        padding: &[SourceRef],
        remaining: &mut usize,
    ) -> Option<(GraphNode, Option<Vec<Option<usize>>>)> {
        let (mut projected, boundaries) = projection::expanded(node, &self.glyphs, remaining)?;
        if !padding.is_empty() {
            let NodeContent::Text { view } = &mut projected.content else {
                return None;
            };
            spend(
                remaining,
                view.tokens
                    .len()
                    .saturating_mul(padding.len().saturating_add(1)),
            )?;
            let mut optional = view.optional_tokens()?;
            for (position, origins) in view.origins.iter().enumerate() {
                if origins.iter().any(|source| padding.contains(source)) {
                    optional[position] = true;
                }
            }
            view.bind_optional_positions(
                optional
                    .into_iter()
                    .enumerate()
                    .filter_map(|(position, optional)| optional.then_some(position))
                    .collect(),
            );
        }
        Some((projected, boundaries))
    }
    pub(super) fn project(
        &self,
        node: &GraphNode,
        remaining: &mut usize,
    ) -> Option<(GraphNode, Vec<std::ops::Range<usize>>)> {
        projection::checked(node, &self.glyphs, remaining)
    }

    /// Returns physical row boundaries for an exact, fully backed projection.
    /// The local validator also supports reversible spacing families, which
    /// require a separate occurrence census before supplying cut candidates.
    pub(super) fn exact_rows(
        &self,
        node: &GraphNode,
        remaining: &mut usize,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        let NodeContent::Text { view } = &node.content else {
            return None;
        };
        if view.optional_tokens()?.iter().any(|optional| *optional)
            || view.source_backed.iter().any(|backed| !backed)
        {
            return None;
        }
        Some(projection::checked(node, &self.glyphs, remaining)?.1)
    }

    pub(super) fn spacing(
        &self,
        nodes: &[&GraphNode],
        remaining: &mut usize,
    ) -> Option<Vec<super::SpaceBoundary>> {
        use super::SpaceOrigin;
        let mut result = Vec::new();
        let mut offset = 0;
        for node in nodes {
            let NodeContent::Text { view } = &node.content else {
                return None;
            };
            spend(remaining, view.tokens.len())?;
            for (position, token) in view.tokens.iter().enumerate() {
                if !token
                    .as_scalar()
                    .is_some_and(|scalar| scalar.is_ascii_whitespace())
                {
                    continue;
                }
                let sources = &view.origins[position];
                spend(remaining, sources.len())?;
                let origin = if view.source_backed[position] {
                    SpaceOrigin::LiteralGlyph
                } else if let [
                    SourceRef::Native { glyph: a },
                    SourceRef::Native { glyph: b },
                ] = sources.as_slice()
                {
                    let (a, b) = (self.glyphs.get(a)?, self.glyphs.get(b)?);
                    if a.page != b.page {
                        SpaceOrigin::PageSeparator
                    } else if a.baseline.y != b.baseline.y {
                        SpaceOrigin::LineSeparator
                    } else if a.direction == b.direction {
                        SpaceOrigin::ReconstructedGap
                    } else {
                        SpaceOrigin::Ambiguous
                    }
                } else {
                    SpaceOrigin::Ambiguous
                };
                result.push(super::SpaceBoundary {
                    position: offset + position,
                    origin,
                    sources: sources.clone(),
                });
            }
            offset += view.tokens.len();
        }
        Some(result)
    }

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
                current.top = current.top.max(glyph.baseline.y);
                current.bottom = current.bottom.min(glyph.baseline.y);
            } else {
                geometry = Some(NodeGeometry {
                    page: *page,
                    bounds: glyph.bbox,
                    first_render: glyph.render_order,
                    last_render: glyph.render_order,
                    top: glyph.baseline.y,
                    bottom: glyph.baseline.y,
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
                // Independently painted blocks can occur in either stream order
                // (for example, a footer painted before body text). Interleaved
                // render ranges still need a stronger discovery profile. The
                // complete spatial source/paint band must prove every candidate.
                if (a.first_render <= b.last_render && b.first_render <= a.last_render)
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
        let first = path.first()?;
        if path.iter().any(|node| node.pages != first.pages) {
            return segments::closed(self, view, root, path, remaining).map(Closure::Segmented);
        }
        self.closed_page(view, root, path, remaining)
    }

    fn closed_page(
        &self,
        view: DocumentView<'_>,
        root: NodeId,
        path: &[&GraphNode],
        remaining: &mut usize,
    ) -> Option<Closure> {
        self.closed_page_with_padding(view, root, path, &BTreeSet::new(), false, remaining)
    }

    fn closed_page_with_padding(
        &self,
        view: DocumentView<'_>,
        root: NodeId,
        path: &[&GraphNode],
        census_padding: &BTreeSet<SourceRef>,
        row_edges: bool,
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
        let mut previous_top = f64::INFINITY;
        let mut previous_right = f64::INFINITY;
        let mut entry_left = f64::NEG_INFINITY;
        let mut exit_right = f64::INFINITY;
        for (index, node) in path.iter().enumerate() {
            if node.pages != [*page]
                || node.sources.is_empty()
                || spend(remaining, node.sources.len()).is_none()
            {
                return None;
            }
            let mut top = f64::NEG_INFINITY;
            let mut bottom = f64::INFINITY;
            let mut left = f64::INFINITY;
            let mut right = f64::NEG_INFINITY;
            for source in &node.sources {
                let SourceRef::Native { glyph } = source else {
                    return None;
                };
                let glyph = self.glyphs.get(glyph)?;
                if glyph.page != *page
                    || glyph.direction != (Vec2 { x: 1.0, y: 0.0 })
                    || glyph.crop_status != GlyphCropStatus::Inside
                    || !(matches!(
                        glyph.path_clip_status,
                        GlyphPathClipStatus::Unclipped | GlyphPathClipStatus::Inside
                    ) || (glyph.path_clip_status == GlyphPathClipStatus::PartiallyOutside
                        && census_padding.contains(source)))
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
                left = left.min(glyph.bbox.min.x);
                right = right.max(glyph.bbox.max.x);
            }
            // The legacy band remains strictly descending. A census-only row
            // profile also admits a monoline prefix wholly left of the next
            // block on the exact same baseline, without a distance tolerance.
            let same_row_prefix = row_edges
                && previous_top == previous_bottom
                && top == previous_bottom
                && previous_right < left;
            if top >= previous_bottom && !same_row_prefix {
                return None;
            }
            if row_edges && (index == 0 || index + 1 == path.len()) {
                if top != bottom {
                    return None;
                }
                if index == 0 {
                    entry_left = left;
                }
                if index + 1 == path.len() {
                    exit_right = right;
                }
            }
            previous_top = top;
            previous_bottom = bottom;
            previous_right = right;
            min_y = min_y.min(bottom);
            max_y = max_y.max(top);
        }
        for glyph in self.pages.get(page).into_iter().flatten() {
            spend(remaining, 1)?;
            // In the row profile, known glyphs strictly outside the two outer
            // row cuts are not omissions. Touching or interior glyphs still
            // have to belong to the path; paint keeps the full band check.
            if glyph.baseline.y >= min_y
                && glyph.baseline.y <= max_y
                && glyph.bbox.max.x >= min_x
                && glyph.bbox.min.x <= max_x
                && !sources.contains(&SourceRef::Native { glyph: glyph.id })
                && !(row_edges
                    && ((glyph.baseline.y == max_y && glyph.bbox.max.x < entry_left)
                        || (glyph.baseline.y == min_y && glyph.bbox.min.x > exit_right)))
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
