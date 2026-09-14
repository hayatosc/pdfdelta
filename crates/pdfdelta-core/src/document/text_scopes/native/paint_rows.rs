//! Recover discovery paths when a row's label is painted after its text.
//! These paths retain raw render order; they do not certify source closure.

use super::*;

/// Row classification tolerates only a small relative floating-point envelope.
/// Raw coordinates and the enclosing source/paint band are never rounded.
pub(super) fn same_baseline(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 8.0 * f64::EPSILON * a.abs().max(b.abs())
}

pub(super) fn repair(
    sources: &Sources<'_>,
    runs: &mut Vec<Vec<&GraphNode>>,
    pages: &BTreeSet<PageId>,
    remaining: &mut usize,
) -> Option<()> {
    if pages.is_empty() {
        return Some(());
    }
    let mut pages = pages.clone();
    spend(remaining, runs.iter().map(Vec::len).sum::<usize>())?;
    // Preserve declared multi-page chains. A page-local alternative must not
    // sever a separately established cross-page discovery path.
    for run in runs.iter() {
        if run
            .iter()
            .any(|node| node.pages.len() != 1 || node.pages != run[0].pages)
        {
            for node in run {
                for page in &node.pages {
                    pages.remove(page);
                }
            }
        }
    }
    if pages.is_empty() {
        return Some(());
    }
    let mut ordered = BTreeMap::<PageId, Vec<(&GraphNode, NodeGeometry)>>::new();
    for node in runs.iter().flatten() {
        let [page] = node.pages.as_slice() else {
            continue;
        };
        if !pages.contains(page) {
            continue;
        }
        spend(remaining, node.sources.len())?;
        let Some(geometry) = sources.geometry(node) else {
            // Do not remove an existing path when its replacement is unknown.
            return Some(());
        };
        ordered.entry(*page).or_default().push((node, geometry));
    }
    let mut replacements = BTreeMap::new();
    for (page, nodes) in &mut ordered {
        let mut page_runs = Vec::new();
        let mut work = 0usize;
        nodes.sort_by(|a, b| {
            work += 1;
            a.1.first_render.cmp(&b.1.first_render)
        });
        spend(remaining, work.saturating_add(nodes.len()))?;
        let mut run = Vec::new();
        let mut previous: Option<&NodeGeometry> = None;
        for (node, geometry) in nodes.iter() {
            if previous.is_some_and(|a| {
                a.last_render >= geometry.first_render
                    || (geometry.top > a.bottom && !same_baseline(geometry.top, a.bottom))
            }) {
                page_runs.push(std::mem::take(&mut run));
            }
            run.push(*node);
            previous = Some(geometry);
        }
        if !run.is_empty() {
            page_runs.push(run);
        }
        replacements.insert(*page, page_runs);
    }
    // Publish atomically at each page's original position. Appending repaired
    // pages after every unaffected page would silently change work priority.
    let mut repaired = Vec::new();
    for run in std::mem::take(runs) {
        if let [page] = run[0].pages.as_slice()
            && pages.contains(page)
        {
            if let Some(page_runs) = replacements.remove(page) {
                repaired.extend(page_runs);
            }
        } else {
            repaired.push(run);
        }
    }
    *runs = repaired;
    Some(())
}

/// Validate physical row pieces independently of the layout node partition.
/// A backwards horizontal jump starts another piece in raw paint order. Pieces
/// on the same baseline must be disjoint; ordinary within-piece kerning remains
/// part of the retained glyph evidence.
pub(super) fn separated(
    sources: &Sources<'_>,
    path: &[&GraphNode],
    remaining: &mut usize,
) -> Option<()> {
    fn close_row(pieces: &mut Vec<(f64, f64)>, remaining: &mut usize) -> Option<()> {
        let mut work = 0usize;
        pieces.sort_by(|a, b| {
            work += 1;
            a.0.total_cmp(&b.0)
        });
        spend(remaining, work.saturating_add(pieces.len()))?;
        if pieces.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
            return None;
        }
        pieces.clear();
        Some(())
    }
    let mut pieces = Vec::<(f64, f64)>::new();
    let mut previous = None::<(f64, f64)>;
    let mut row_baseline = None;
    for source in path.iter().flat_map(|node| &node.sources) {
        spend(remaining, 1)?;
        let SourceRef::Native { glyph } = source else {
            return None;
        };
        let glyph = sources.glyphs.get(glyph)?;
        if ![
            glyph.baseline.x,
            glyph.baseline.y,
            glyph.bbox.min.x,
            glyph.bbox.max.x,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return None;
        }
        if row_baseline.is_some_and(|y| !same_baseline(y, glyph.baseline.y)) {
            close_row(&mut pieces, remaining)?;
            row_baseline = None;
        }
        row_baseline.get_or_insert(glyph.baseline.y);
        if pieces.is_empty() || previous.is_some_and(|(x, _)| glyph.baseline.x < x) {
            pieces.push((glyph.bbox.min.x, glyph.bbox.max.x));
        } else {
            let piece = pieces.last_mut()?;
            piece.0 = piece.0.min(glyph.bbox.min.x);
            piece.1 = piece.1.max(glyph.bbox.max.x);
        }
        previous = Some((glyph.baseline.x, glyph.baseline.y));
    }
    close_row(&mut pieces, remaining)
}

pub(super) fn edge_glyphs<'a>(
    sources: &Sources<'a>,
    a: &GraphNode,
    b: &GraphNode,
) -> Option<(&'a Glyph, &'a Glyph)> {
    let (Some(SourceRef::Native { glyph: a }), Some(SourceRef::Native { glyph: b })) =
        (a.sources.last(), b.sources.first())
    else {
        return None;
    };
    Some((*sources.glyphs.get(a)?, *sources.glyphs.get(b)?))
}

pub(super) fn reverse_row(a: &Glyph, b: &Glyph) -> Option<PageId> {
    (a.page == b.page
        && same_baseline(a.baseline.y, b.baseline.y)
        && a.bbox.min.x > b.bbox.max.x
        && a.render_order < b.render_order)
        .then_some(a.page)
}
