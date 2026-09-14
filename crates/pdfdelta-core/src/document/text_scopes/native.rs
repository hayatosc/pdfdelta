//! Local native text closure does not require unrelated page orders to be known.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Result,
    model::{
        Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::ComparableToken,
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
mod paint_rows;
mod projection;
mod segments;

pub use segments::{NativeRegion, NativeRegionChain, NativeRegionChains, NativeTransition};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RowOrder {
    Spatial,
    Paint,
}

impl RowOrder {
    pub(super) fn convention(self) -> &'static str {
        match self {
            Self::Spatial => "horizontal-row-boundaries-v1",
            Self::Paint => "horizontal-paint-row-boundaries-v1",
        }
    }
}

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
    spend(remaining, 1)?;
    if shorter.len() >= longer.len() {
        return Some(false);
    }
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
        if continuation_matches(&view.tokens, remainder, remaining)? {
            return Some(true);
        }
    }
    Some(false)
}

/// Match a remainder with no outer ASCII spaces, charging each inspected token.
/// A mismatch needs no allocation or inspection of the rest of the candidate;
/// a match still requires every token, including trailing spaces, to be mapped.
fn continuation_matches(
    tokens: &[ComparableToken],
    remainder: &str,
    remaining: &mut usize,
) -> Option<bool> {
    let mut expected = remainder.chars();
    let mut leading = true;
    for token in tokens {
        spend(remaining, 4)?;
        let value = token.as_scalar();
        if leading && value == Some(' ') {
            continue;
        }
        leading = false;
        match expected.next() {
            Some(expected) if value != Some(expected) => return Some(false),
            None if value != Some(' ') => return Some(false),
            _ => {}
        }
    }
    Some(expected.next().is_none())
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
    let mut paint_pages = BTreeSet::new();
    for edge in &view.graph.edges {
        if edge.kind != EdgeKind::Precedes {
            continue;
        }
        // A native edge to excluded context ends the discovery run without
        // removing its adjacent paragraph. Source closure still rejects any
        // excluded glyph inside a proposed interval.
        if edge.basis != ViewBasis::NativeLayout {
            blocked.extend([edge.from, edge.to]);
        } else if nodes.contains_key(&edge.from) && nodes.contains_key(&edge.to) {
            if let Some((a, b)) =
                paint_rows::edge_glyphs(sources, nodes[&edge.from], nodes[&edge.to])
            {
                // Reuse the row-order witnesses already acquired for this edge.
                // Diagonal jumps over other paint or against paint order can
                // displace the same-column successor. Splitting proves no order;
                // every new interval still needs complete source and paint closure.
                if a.page == b.page
                    && a.direction == (Vec2 { x: 1.0, y: 0.0 })
                    && b.direction == (Vec2 { x: 1.0, y: 0.0 })
                    && a.bbox.min.y > b.bbox.max.y
                    && ((a.bbox.max.x < b.bbox.min.x
                        && a.render_order
                            .checked_add(1)
                            .is_some_and(|next| next < b.render_order))
                        || (b.bbox.max.x < a.bbox.min.x && b.render_order < a.render_order))
                {
                    continue;
                }
                if let Some(page) = paint_rows::reverse_row(a, b) {
                    paint_pages.insert(page);
                }
            }
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
    segments::bridge(
        sources,
        view,
        &nodes,
        &blocked,
        &mut next,
        &mut previous,
        remaining,
    );
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
    let _ = paint_rows::repair(sources, &mut runs, &paint_pages, remaining);
    if rows {
        let _ = hanging::attach(sources, &mut runs, &connected, remaining);
    }
    Ok(runs)
}

pub(super) struct Sources<'a> {
    glyphs: BTreeMap<GlyphId, &'a Glyph>,
    pages: BTreeMap<PageId, Vec<&'a [Glyph]>>,
    native_order: Option<Vec<segments::Membership<'a>>>,
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
    pub(super) fn page_sources_match(
        &self,
        page: PageId,
        population: &BTreeSet<SourceRef>,
        remaining: &mut usize,
    ) -> Option<bool> {
        let spans = self.pages.get(&page)?;
        let count: usize = spans.iter().map(|span| span.len()).sum();
        spend(remaining, count)?;
        Some(
            count == population.len()
                && spans
                    .iter()
                    .flat_map(|span| span.iter())
                    .all(|glyph| population.contains(&SourceRef::Native { glyph: glyph.id })),
        )
    }

    pub(super) fn acquire_native_order(&mut self, view: DocumentView<'a>, remaining: &mut usize) {
        self.native_order = segments::native_memberships(view, remaining);
    }
    pub(super) fn census(
        &self,
        view: DocumentView<'_>,
        root: NodeId,
        path: &[&GraphNode],
        remaining: &mut usize,
        rows: bool,
        paint: bool,
    ) -> Option<(Closure, Vec<SourceRef>, Option<RowOrder>)> {
        census::checked(self, view, root, path, remaining, rows, paint)
    }

    pub(super) fn project_census(
        &self,
        node: &GraphNode,
        padding: &[SourceRef],
        paint_order: bool,
        remaining: &mut usize,
    ) -> Option<(GraphNode, Option<Vec<Option<usize>>>)> {
        let (mut projected, boundaries) =
            projection::expanded(node, &self.glyphs, remaining, paint_order)?;
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
        paint_order: bool,
        remaining: &mut usize,
    ) -> Option<(GraphNode, Vec<std::ops::Range<usize>>)> {
        projection::checked_order(node, &self.glyphs, remaining, paint_order)
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
        let glyphs = view.evidence.native.items();
        spend(remaining, glyphs.len())?;
        let mut index = Self {
            glyphs: BTreeMap::new(),
            pages: BTreeMap::new(),
            native_order: None,
        };
        let mut start = 0;
        for (position, glyph) in glyphs.iter().enumerate() {
            index.glyphs.insert(glyph.id, glyph);
            if glyph.page != glyphs[start].page {
                spend(remaining, 1)?;
                index
                    .pages
                    .entry(glyphs[start].page)
                    .or_default()
                    .push(&glyphs[start..position]);
                start = position;
            }
        }
        if start < glyphs.len() {
            spend(remaining, 1)?;
            index
                .pages
                .entry(glyphs[start].page)
                .or_default()
                .push(&glyphs[start..]);
        }
        // Each raw glyph is indexed once; each contiguous page span stores one
        // borrowed slice. Noncontiguous spans preserve arbitrary input order,
        // including unassigned glyphs after a different page's evidence.
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
        self.closed_page_with_padding(view, root, path, &BTreeSet::new(), None, remaining)
    }

    fn closed_page_with_padding(
        &self,
        view: DocumentView<'_>,
        root: NodeId,
        path: &[&GraphNode],
        census_padding: &BTreeSet<SourceRef>,
        row_order: Option<RowOrder>,
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
        let row_edges = row_order.is_some();
        let paint_order = row_order == Some(RowOrder::Paint);
        if paint_order {
            paint_rows::separated(self, path, remaining)?;
        }
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
        let mut previous_render = None;
        let mut previous_baseline = f64::INFINITY;
        let mut entry_left = f64::NEG_INFINITY;
        let mut entry_right = f64::INFINITY;
        let mut exit_left = f64::NEG_INFINITY;
        let mut exit_right = f64::INFINITY;
        let mut entry_render = u32::MAX;
        let mut exit_render = 0;
        let mut first_row = None;
        let mut first_row_bounds = (f64::INFINITY, f64::NEG_INFINITY);
        let mut last_row_bounds = (f64::INFINITY, f64::NEG_INFINITY);
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
                if paint_order {
                    if previous_render.is_some_and(|order| order >= glyph.render_order)
                        || !glyph.baseline.y.is_finite()
                        || (glyph.baseline.y > previous_baseline
                            && !paint_rows::same_baseline(glyph.baseline.y, previous_baseline))
                        || ![
                            glyph.bbox.min.x,
                            glyph.bbox.min.y,
                            glyph.bbox.max.x,
                            glyph.bbox.max.y,
                        ]
                        .iter()
                        .all(|value| value.is_finite())
                    {
                        return None;
                    }
                    previous_render = Some(glyph.render_order);
                    if !paint_rows::same_baseline(previous_baseline, glyph.baseline.y) {
                        last_row_bounds = (f64::INFINITY, f64::NEG_INFINITY);
                        previous_baseline = glyph.baseline.y;
                    }
                    last_row_bounds.0 = last_row_bounds.0.min(glyph.bbox.min.x);
                    last_row_bounds.1 = last_row_bounds.1.max(glyph.bbox.max.x);
                    if paint_rows::same_baseline(
                        *first_row.get_or_insert(glyph.baseline.y),
                        glyph.baseline.y,
                    ) {
                        first_row_bounds.0 = first_row_bounds.0.min(glyph.bbox.min.x);
                        first_row_bounds.1 = first_row_bounds.1.max(glyph.bbox.max.x);
                    }
                    entry_render = entry_render.min(glyph.render_order);
                    exit_render = exit_render.max(glyph.render_order);
                }
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
            if !paint_order && top >= previous_bottom && !same_row_prefix {
                return None;
            }
            if row_edges && (index == 0 || index + 1 == path.len()) {
                if top != bottom && !(paint_order && paint_rows::same_baseline(top, bottom)) {
                    return None;
                }
                if index == 0 {
                    entry_left = left;
                    entry_right = right;
                }
                if index + 1 == path.len() {
                    exit_left = left;
                    exit_right = right;
                }
            }
            previous_top = top;
            previous_bottom = bottom;
            previous_right = right;
            min_y = min_y.min(bottom);
            max_y = max_y.max(top);
        }
        if paint_order {
            (entry_left, entry_right) = first_row_bounds;
            (exit_left, exit_right) = last_row_bounds;
        }
        for glyph in self
            .pages
            .get(page)
            .into_iter()
            .flatten()
            .flat_map(|run| *run)
        {
            spend(remaining, 1)?;
            let outside_row = if paint_order {
                let before = paint_rows::same_baseline(glyph.baseline.y, max_y)
                    && glyph.render_order < entry_render
                    && (glyph.bbox.max.x < entry_left || glyph.bbox.min.x > entry_right);
                let after = paint_rows::same_baseline(glyph.baseline.y, min_y)
                    && glyph.render_order > exit_render
                    && (glyph.bbox.max.x < exit_left || glyph.bbox.min.x > exit_right);
                (before || after)
                    && matches!(&glyph.text, crate::model::DecodedText::Mapped(_))
                    && glyph.direction == (Vec2 { x: 1.0, y: 0.0 })
                    && glyph.crop_status == GlyphCropStatus::Inside
                    && matches!(
                        glyph.path_clip_status,
                        GlyphPathClipStatus::Unclipped | GlyphPathClipStatus::Inside
                    )
                    && matches!(
                        glyph.render_mode,
                        TextRenderMode::Fill
                            | TextRenderMode::Stroke
                            | TextRenderMode::FillAndStroke
                    )
            } else {
                row_edges
                    && ((glyph.baseline.y == max_y && glyph.bbox.max.x < entry_left)
                        || (glyph.baseline.y == min_y && glyph.bbox.min.x > exit_right))
            };
            // In the row profile, known glyphs strictly outside the two outer
            // row cuts are not omissions. Touching or interior glyphs still
            // have to belong to the path; paint keeps the full band check.
            if (glyph.baseline.y >= min_y
                || (paint_order && paint_rows::same_baseline(glyph.baseline.y, min_y)))
                && (glyph.baseline.y <= max_y
                    || (paint_order && paint_rows::same_baseline(glyph.baseline.y, max_y)))
                && glyph.bbox.max.x >= min_x
                && glyph.bbox.min.x <= max_x
                && !sources.contains(&SourceRef::Native { glyph: glyph.id })
                && !outside_row
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
        spend(remaining, glyphs.iter().map(|run| run.len()).sum())?;
        let expected: BTreeSet<_> = glyphs
            .iter()
            .flat_map(|run| *run)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DocumentGraph, EvidenceStore};

    #[test]
    fn continuation_stream_matches_materialized_text_with_unmapped_and_unicode_tokens() {
        let alphabet = [
            ComparableToken::Scalar(' '),
            ComparableToken::Scalar('a'),
            ComparableToken::Scalar('日'),
            ComparableToken::Scalar('\t'),
            ComparableToken::Unmapped {
                font_hash: crate::model::FontProgramHash(vec![1]),
                glyph_id: 7,
            },
        ];
        for length in 0..=4 {
            for mut index in 0..alphabet.len().pow(length) {
                let tokens: Vec<_> = (0..length)
                    .map(|_| {
                        let token = alphabet[index % alphabet.len()].clone();
                        index /= alphabet.len();
                        token
                    })
                    .collect();
                let text: Option<String> = tokens.iter().map(ComparableToken::as_scalar).collect();
                for remainder in ["", "a", "日", "\t", "a a", "a日", "日\ta"] {
                    let expected = text
                        .as_ref()
                        .is_some_and(|text| text.trim_matches(' ') == remainder);
                    let mut work = 4 * tokens.len();
                    assert_eq!(
                        continuation_matches(&tokens, remainder, &mut work),
                        Some(expected),
                        "{tokens:?} versus {remainder:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn continuation_mismatch_is_bounded_but_a_match_checks_the_entire_candidate() {
        let tokens = vec![ComparableToken::Scalar('x'); 100_000];
        let mut work = 4;
        assert_eq!(continuation_matches(&tokens, "a", &mut work), Some(false));
        assert_eq!(work, 0);
        let tokens: Vec<_> = " a ".chars().map(ComparableToken::Scalar).collect();
        let mut work = 8;
        assert_eq!(continuation_matches(&tokens, "a", &mut work), None);
        let mut work = 12;
        assert_eq!(continuation_matches(&tokens, "a", &mut work), Some(true));
    }

    #[test]
    fn continuation_budget_distinguishes_impossible_lengths_from_unknown_matches() {
        let evidence = EvidenceStore {
            revision: String::new(),
            backends: Vec::new(),
            pages: Vec::new(),
            native: crate::model::Document::new(Vec::new()),
            rendered: Vec::new(),
            structured: Vec::new(),
            inventories: Vec::new(),
            key_inventories: Vec::new(),
            native_structures: Vec::new(),
            issues: Vec::new(),
        };
        let graph = DocumentGraph::default();
        let view = DocumentView {
            evidence: &evidence,
            graph: &graph,
        };
        for (shorter, longer) in [("abc", "ab"), ("abc", "xyz"), ("", "")] {
            let mut work = 1;
            assert_eq!(
                external_continuation(view, &[], shorter, longer, &mut work),
                Some(false)
            );
            assert_eq!(work, 0);
        }
        let mut work = 1;
        assert_eq!(
            external_continuation(view, &[], "ab", "abc", &mut work),
            None
        );
    }
}
