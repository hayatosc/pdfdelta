//! Local native text closure does not require unrelated page orders to be known.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Result,
    model::{Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus, PageId, TextRenderMode, Vec2},
};

use super::{
    DocumentComparisonLimits, DocumentView, EdgeKind, GraphNode, NodeContent, NodeId, SourceRef,
    source_children, spend,
};
use crate::document::{Channel, NodeKind, ViewBasis};

/// Runs are discovery paths only. A path becomes a closed interval only after
/// its complete native source band has been checked by [`Sources::closed`].
pub(super) fn runs<'a>(
    view: DocumentView<'a>,
    root: NodeId,
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

impl<'a> Sources<'a> {
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
    ) -> bool {
        let [page] = path[0].pages.as_slice() else {
            return false;
        };
        if spend(
            remaining,
            view.evidence
                .inventories
                .len()
                .saturating_add(view.evidence.issues.len()),
        )
        .is_none()
            || !view.evidence.inventory_complete(Some(*page), Channel::Text)
        {
            return false;
        }
        let mut sources = BTreeSet::new();
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut previous_bottom = f64::INFINITY;
        for node in path {
            if node.pages != [*page]
                || node.sources.is_empty()
                || spend(remaining, node.sources.len()).is_none()
            {
                return false;
            }
            let mut top = f64::NEG_INFINITY;
            let mut bottom = f64::INFINITY;
            for source in &node.sources {
                let SourceRef::Native { glyph } = source else {
                    return false;
                };
                let Some(glyph) = self.glyphs.get(glyph) else {
                    return false;
                };
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
                    return false;
                }
                min_x = min_x.min(glyph.bbox.min.x);
                max_x = max_x.max(glyph.bbox.max.x);
                top = top.max(glyph.baseline.y);
                bottom = bottom.min(glyph.baseline.y);
            }
            // Deliberate: the first native convention admits strictly descending
            // horizontal blocks; rotated or interleaved baselines need another
            // source closure, not a guessed order or a pixel-distance threshold.
            if top >= previous_bottom {
                return false;
            }
            previous_bottom = bottom;
            min_y = min_y.min(bottom);
            max_y = max_y.max(top);
        }
        for glyph in self.pages.get(page).into_iter().flatten() {
            if spend(remaining, 1).is_none() {
                return false;
            }
            if glyph.baseline.y >= min_y
                && glyph.baseline.y <= max_y
                && glyph.bbox.max.x >= min_x
                && glyph.bbox.min.x <= max_x
                && !sources.contains(&SourceRef::Native { glyph: glyph.id })
            {
                return false;
            }
        }
        let members: BTreeSet<_> = path.iter().map(|node| node.id).collect();
        for alternative in &view.graph.alternatives {
            if alternative.parent == root || members.contains(&alternative.parent) {
                return false;
            }
            for partition in &alternative.partitions {
                if spend(remaining, partition.len()).is_none()
                    || partition.iter().any(|id| members.contains(id))
                {
                    return false;
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
                return false;
            }
        }
        true
    }
}
