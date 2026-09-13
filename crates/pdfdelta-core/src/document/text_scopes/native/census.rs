//! Boundary padding can be uncertain without making interior glyphs uncertain.
//! Such sources stay in the census as optional tokens and cannot enter a claim.

use super::*;
use crate::model::DecodedText;

pub(super) fn checked(
    sources: &Sources<'_>,
    view: DocumentView<'_>,
    root: NodeId,
    path: &[&GraphNode],
    remaining: &mut usize,
    rows: bool,
    paint: bool,
) -> Option<(Closure, Vec<SourceRef>, Option<RowOrder>)> {
    if let Some(closure) = sources.closed(view, root, path, remaining) {
        return Some((closure, Vec::new(), None));
    }
    if path.len() < 2 || path.iter().any(|node| node.pages != path[0].pages) {
        return None;
    }
    let mut padding = BTreeSet::new();
    for node in [path[0], path[path.len() - 1]] {
        let NodeContent::Text { view } = &node.content else {
            return None;
        };
        spend(remaining, view.tokens.len().saturating_mul(4))?;
        let space = |index: usize| {
            view.tokens[index]
                .as_scalar()
                .is_some_and(|c| c.is_ascii_whitespace())
        };
        let first = (0..view.tokens.len()).find(|&i| !space(i))?;
        let last = (0..view.tokens.len()).rfind(|&i| !space(i))?;
        for position in (0..first).chain(last + 1..view.tokens.len()) {
            if !view.source_backed[position] {
                continue;
            }
            let [SourceRef::Native { glyph }] = view.origins[position].as_slice() else {
                return None;
            };
            let glyph = sources.glyphs.get(glyph)?;
            if glyph.path_clip_status != GlyphPathClipStatus::PartiallyOutside {
                continue;
            }
            let DecodedText::Mapped(text) = &glyph.text else {
                return None;
            };
            if text.is_empty() || !text.chars().all(|c| c.is_ascii_whitespace()) {
                return None;
            }
            padding.insert(SourceRef::Native { glyph: glyph.id });
        }
    }
    // Partial boundary ink must be disjoint from the interior text's bounds.
    // Touching ink is not rounded away or made safe by the padding convention.
    for source in &padding {
        let SourceRef::Native { glyph } = source else {
            return None;
        };
        let a = sources.glyphs.get(glyph)?.bbox;
        for node in &path[1..path.len() - 1] {
            spend(remaining, node.sources.len())?;
            for source in &node.sources {
                let SourceRef::Native { glyph } = source else {
                    return None;
                };
                let b = sources.glyphs.get(glyph)?.bbox;
                if a.min.x <= b.max.x
                    && b.min.x <= a.max.x
                    && a.min.y <= b.max.y
                    && b.min.y <= a.max.y
                {
                    return None;
                }
            }
        }
    }
    if !padding.is_empty()
        && let Some(closure) =
            sources.closed_page_with_padding(view, root, path, &padding, None, remaining)
    {
        return Some((closure, padding.into_iter().collect(), None));
    }
    if rows
        && let Some(closure) = sources.closed_page_with_padding(
            view,
            root,
            path,
            &padding,
            Some(RowOrder::Spatial),
            remaining,
        )
    {
        return Some((
            closure,
            padding.into_iter().collect(),
            Some(RowOrder::Spatial),
        ));
    }
    if !paint || !sources.has_ordered_row(path, remaining)? {
        return None;
    }
    let closure = sources.closed_page_with_padding(
        view,
        root,
        path,
        &padding,
        Some(RowOrder::Paint),
        remaining,
    )?;
    Some((
        closure,
        padding.into_iter().collect(),
        Some(RowOrder::Paint),
    ))
}

impl Sources<'_> {
    pub(in super::super) fn has_ordered_row(
        &self,
        path: &[&GraphNode],
        remaining: &mut usize,
    ) -> Option<bool> {
        spend(remaining, path.len())?;
        Some(path.windows(2).any(|pair| {
            let (Some(SourceRef::Native { glyph: a }), Some(SourceRef::Native { glyph: b })) =
                (pair[0].sources.last(), pair[1].sources.first())
            else {
                return false;
            };
            let (Some(a), Some(b)) = (self.glyphs.get(a), self.glyphs.get(b)) else {
                return false;
            };
            a.page == b.page
                && paint_rows::same_baseline(a.baseline.y, b.baseline.y)
                && (a.bbox.min.x > b.bbox.max.x || a.bbox.max.x < b.bbox.min.x)
        }))
    }
}
