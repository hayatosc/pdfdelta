//! Source-side validation for reversible local text views. This does not prove
//! visibility, population closure or correspondence to the opposing revision.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use super::{Glyph, GlyphId, GraphNode, NodeContent, SourceRef, spend};
use crate::{document::TextNormalization, model::DecodedText, normalize::ligature_expansion};

/// Validate every retained token against its physical glyph. No token or source
/// is invented, discarded or reassigned. Only the existing Latin ligature fold
/// and source-checked separator/hyphen alternatives are admitted.
pub(super) fn checked(
    node: &GraphNode,
    glyphs: &BTreeMap<GlyphId, &Glyph>,
    remaining: &mut usize,
) -> Option<(GraphNode, Vec<Range<usize>>)> {
    let NodeContent::Text { view } = &node.content else {
        return None;
    };
    spend(
        remaining,
        view.tokens
            .len()
            .saturating_mul(8)
            .saturating_add(node.sources.len()),
    )?;
    if view.tokens.is_empty()
        || view.tokens.len() != view.origins.len()
        || view.tokens.len() != view.source_backed.len()
    {
        return None;
    }
    let mut groups: Vec<(Range<usize>, &Glyph)> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = 0;
    while cursor < view.tokens.len() {
        if !view.source_backed[cursor] {
            cursor += 1;
            continue;
        }
        let [SourceRef::Native { glyph: id }] = view.origins[cursor].as_slice() else {
            return None;
        };
        let glyph = *glyphs.get(id)?;
        if !glyph.baseline.x.is_finite() || !glyph.baseline.y.is_finite() || !seen.insert(*id) {
            return None;
        }
        let DecodedText::Mapped(text) = &glyph.text else {
            return None;
        };
        spend(remaining, text.len().saturating_mul(4))?;
        let mut expanded = String::new();
        for scalar in text.chars() {
            if let Some(replacement) = ligature_expansion(scalar) {
                expanded.push_str(replacement);
            } else {
                expanded.push(scalar);
            }
        }
        let start = cursor;
        while cursor < view.tokens.len()
            && view.source_backed[cursor]
            && view.origins[cursor] == [SourceRef::Native { glyph: *id }]
        {
            cursor += 1;
        }
        let matches = |text: &str| {
            view.tokens[start..cursor]
                .iter()
                .map(|token| token.as_scalar())
                .eq(text.chars().map(Some))
        };
        if text.is_empty() || (!matches(text) && !matches(&expanded)) {
            return None;
        }
        if groups.last().is_some_and(|(_, previous)| {
            previous.page != glyph.page
                || previous.baseline.y < glyph.baseline.y
                || (previous.baseline.y == glyph.baseline.y
                    && previous.baseline.x >= glyph.baseline.x)
        }) {
            return None;
        }
        groups.push((start..cursor, glyph));
    }
    let source_set: BTreeSet<_> = node.sources.iter().copied().collect();
    if source_set.len() != node.sources.len()
        || seen.len() != node.sources.len()
        || node.sources.iter().any(|source| match source {
            SourceRef::Native { glyph } => !seen.contains(glyph),
            _ => true,
        })
    {
        return None;
    }
    let mut optional = view
        .optional_tokens()
        .unwrap_or_else(|| vec![false; view.tokens.len()]);
    let mut rows = Vec::new();
    let mut row_start = 0;
    let (first, _) = groups.first()?;
    let (last, _) = groups.last()?;
    if first.start != 0 || last.end != view.tokens.len() {
        return None;
    }
    for pair in groups.windows(2) {
        let [(left, a), (right, b)] = pair else {
            unreachable!()
        };
        for (position, is_optional) in optional
            .iter_mut()
            .enumerate()
            .take(right.start)
            .skip(left.end)
        {
            if view.source_backed[position]
                || !view.tokens[position]
                    .as_scalar()
                    .is_some_and(|scalar| scalar.is_ascii_whitespace())
                || view.origins[position]
                    != [
                        SourceRef::Native { glyph: a.id },
                        SourceRef::Native { glyph: b.id },
                    ]
            {
                return None;
            }
            *is_optional = true;
        }
        if a.baseline.y != b.baseline.y {
            rows.push(row_start..right.start);
            row_start = right.start;
            if left.len() == 1 && view.tokens[left.start].as_scalar() == Some('-') {
                optional[left.start] = true;
            }
        }
    }
    rows.push(row_start..view.tokens.len());
    let mut result = node.clone();
    let NodeContent::Text { view: local } = &mut result.content else {
        unreachable!()
    };
    local.normalization = TextNormalization::Exact;
    let optional: Vec<_> = optional
        .into_iter()
        .enumerate()
        .filter_map(|(index, optional)| optional.then_some(index))
        .collect();
    if !optional.is_empty() {
        local.bind_optional_positions(optional);
    }
    Some((result, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::{NodeId, NodeKind, TextView, ViewBasis},
        model::{
            FontId, GlyphCropStatus, GlyphPathClipStatus, GlyphProvenance, PageId, Rect,
            TextRenderMode, Vec2,
        },
        normalize::ComparableToken,
        pdf::ObjectRef,
    };

    fn fixture() -> (GraphNode, Vec<Glyph>) {
        let glyphs: Vec<_> = ["ﬁ", "-", "x"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                let baseline = Vec2 {
                    x: if index == 1 { 10.0 } else { 0.0 },
                    y: if index == 2 { 20.0 } else { 40.0 },
                };
                Glyph {
                    id: GlyphId(index as u64),
                    text: DecodedText::Mapped(text.into()),
                    raw_code: text.as_bytes().to_vec(),
                    page: PageId(0),
                    bbox: Rect {
                        min: baseline,
                        max: Vec2 {
                            x: baseline.x + 8.0,
                            y: baseline.y + 10.0,
                        },
                    },
                    baseline,
                    direction: Vec2 { x: 1.0, y: 0.0 },
                    font_id: FontId(1),
                    font_size: 10.0,
                    render_order: index as u32,
                    render_mode: TextRenderMode::Fill,
                    crop_status: GlyphCropStatus::Inside,
                    path_clip_status: GlyphPathClipStatus::Unclipped,
                    provenance: GlyphProvenance {
                        content_stream: ObjectRef {
                            object_number: 1,
                            generation: 0,
                        },
                        operator_index: index as u32,
                    },
                }
            })
            .collect();
        let sources: Vec<_> = glyphs
            .iter()
            .map(|glyph| SourceRef::Native { glyph: glyph.id })
            .collect();
        (
            GraphNode {
                id: NodeId(1),
                kind: NodeKind::Paragraph,
                pages: vec![PageId(0)],
                sources: sources.clone(),
                identity: None,
                basis: ViewBasis::NativeLayout,
                content: NodeContent::Text {
                    view: TextView {
                        tokens: "fi-\nx".chars().map(ComparableToken::Scalar).collect(),
                        origins: vec![
                            vec![sources[0]],
                            vec![sources[0]],
                            vec![sources[1]],
                            vec![sources[1], sources[2]],
                            vec![sources[2]],
                        ],
                        source_backed: vec![true, true, true, false, true],
                        normalization: TextNormalization::Unresolved {
                            reason: "global line normalization unresolved".into(),
                        },
                    },
                },
            },
            glyphs,
        )
    }

    #[test]
    fn raw_projection_preserves_tokens_and_checks_local_spacing_family() {
        let (node, glyphs) = fixture();
        let map = glyphs.iter().map(|glyph| (glyph.id, glyph)).collect();
        let (local, rows) = checked(&node, &map, &mut 10_000).expect("source-checked projection");
        assert_eq!(rows, [0..4, 4..5]);
        assert_eq!(local.sources, node.sources);
        assert_eq!(local.id, node.id);
        let (NodeContent::Text { view: local }, NodeContent::Text { view: original }) =
            (&local.content, &node.content)
        else {
            unreachable!()
        };
        assert_eq!(local.tokens, original.tokens);
        assert_eq!(local.origins, original.origins);
        assert_eq!(local.source_backed, original.source_backed);
        assert_eq!(
            local.optional_tokens(),
            Some(vec![false, false, true, true, false])
        );
        assert!(original.optional_tokens().is_none());
        assert!(checked(&node, &map, &mut 0).is_none());
    }

    #[test]
    fn raw_projection_rejects_missing_repeated_or_reordered_sources() {
        for mutation in 0..8 {
            let (mut node, mut glyphs) = fixture();
            let NodeContent::Text { view } = &mut node.content else {
                unreachable!()
            };
            match mutation {
                0 => view.tokens[1] = ComparableToken::Scalar('x'),
                1 => view.origins[3].reverse(),
                2 => view.tokens[3] = ComparableToken::Scalar('!'),
                3 => {
                    node.sources.pop();
                }
                4 => node.sources.push(node.sources[0]),
                5 => glyphs[2].baseline.y = 50.0,
                6 => glyphs[1].baseline.x = f64::NAN,
                7 => view.origins[4] = view.origins[0].clone(),
                _ => unreachable!(),
            }
            let map = glyphs.iter().map(|glyph| (glyph.id, glyph)).collect();
            assert!(
                checked(&node, &map, &mut 10_000).is_none(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn raw_projection_also_preserves_an_unexpanded_ligature() {
        let (mut node, glyphs) = fixture();
        let NodeContent::Text { view } = &mut node.content else {
            unreachable!()
        };
        view.tokens[0] = ComparableToken::Scalar('ﬁ');
        view.tokens.remove(1);
        view.origins.remove(1);
        view.source_backed.remove(1);
        let map = glyphs.iter().map(|glyph| (glyph.id, glyph)).collect();
        let (local, _) = checked(&node, &map, &mut 10_000).expect("unaltered raw ligature");
        let NodeContent::Text { view } = local.content else {
            unreachable!()
        };
        assert_eq!(view.tokens[0], ComparableToken::Scalar('ﬁ'));
    }
}
