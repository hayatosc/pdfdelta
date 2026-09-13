//! Conditional content edges inside an already compared finite source interval.
//! Only mandatory literal spaces can move outside the additional local view.

use super::*;

struct Trimmed {
    entry: Position,
    exit: Position,
    padding: [Vec<SourceFragment>; 2],
}

fn fragments(
    runs: &[Vec<&GraphNode>],
    first: Position,
    last: Position,
    maps: &OriginalCuts,
) -> Option<Vec<SourceFragment>> {
    let mut fragments = Vec::new();
    for (index, &node) in runs[first.0]
        .iter()
        .enumerate()
        .take(last.1 + 1)
        .skip(first.1)
    {
        let NodeContent::Text { view } = &node.content else {
            return None;
        };
        let start = if index == first.1 { first.2 } else { 0 };
        let end = if index == last.1 {
            last.2
        } else {
            view.tokens.len()
        };
        if start == end {
            continue;
        }
        let part = slice(node, start, end)?;
        fragments.push(SourceFragment {
            node: node.id,
            tokens: [
                original_boundary(maps, node.id, start)?,
                original_boundary(maps, node.id, end)?,
            ],
            sources: part.sources,
        });
    }
    Some(fragments)
}

fn trim(
    runs: &[Vec<&GraphNode>],
    first: Position,
    last: Position,
    maps: &OriginalCuts,
    remaining: &mut usize,
) -> Option<Trimmed> {
    let mut content = None;
    let mut end = None;
    let mut prefix_end = None;
    let mut suffix_start = None;
    for (index, &node) in runs[first.0]
        .iter()
        .enumerate()
        .take(last.1 + 1)
        .skip(first.1)
    {
        let NodeContent::Text { view } = &node.content else {
            return None;
        };
        spend(
            remaining,
            view.tokens
                .len()
                .saturating_add(node.sources.len())
                .saturating_mul(8),
        )?;
        let optional = view.optional_tokens()?;
        let start = if index == first.1 { first.2 } else { 0 };
        let stop = if index == last.1 {
            last.2
        } else {
            view.tokens.len()
        };
        for (position, is_optional) in optional.iter().enumerate().take(stop).skip(start) {
            let mandatory_space = view.tokens[position].as_scalar() == Some(' ')
                && view.source_backed[position]
                && !is_optional;
            if !mandatory_space {
                content.get_or_insert((first.0, index, position));
                end = Some((first.0, index, position + 1));
                suffix_start = None;
            } else if content.is_none() {
                prefix_end = Some((first.0, index, position + 1));
            } else {
                suffix_start.get_or_insert((first.0, index, position));
            }
        }
    }
    // A whole padding node ends at a different coordinate from the next
    // node's start. Use the padding edge so its certificate names the exact
    // same cut as the removed fragment, without consuming another source.
    let (entry, exit) = (prefix_end.unwrap_or(content?), suffix_start.unwrap_or(end?));
    // A contracted source token has no legal cut between its physical spaces.
    // Both retained body and removed padding must project without shared glyphs.
    original_boundary(maps, runs[entry.0][entry.1].id, entry.2)?;
    original_boundary(maps, runs[exit.0][exit.1].id, exit.2)?;
    interior(runs, entry, exit)?;
    Some(Trimmed {
        entry,
        exit,
        padding: [
            fragments(runs, first, entry, maps)?,
            fragments(runs, exit, last, maps)?,
        ],
    })
}

type Refined = (Boundary, Boundary, Option<Box<SourceCutEdgeRefinement>>);

pub(super) fn refine(
    left: &[Vec<&GraphNode>],
    right: &[Vec<&GraphNode>],
    entry: &Boundary,
    exit: &Boundary,
    maps: &CutMaps,
    remaining: &mut usize,
) -> Option<Refined> {
    let old = trim(left, entry.old, exit.old, &maps.old, remaining)?;
    let new = trim(right, entry.new, exit.new, &maps.new, remaining)?;
    if old.padding.iter().chain(&new.padding).all(Vec::is_empty) {
        return None;
    }
    let boundary = |a: Position, b: Position, edge: usize, original: &Boundary| {
        if old.padding[edge].is_empty() && new.padding[edge].is_empty() {
            return Some(original.clone());
        }
        let a = if old.padding[edge].is_empty() {
            original.old
        } else {
            a
        };
        let b = if new.padding[edge].is_empty() {
            original.new
        } else {
            b
        };
        Some(Boundary {
            old: a,
            new: b,
            certificate: CutCorrespondence {
                old: SourceCut {
                    node: left[a.0][a.1].id,
                    token_boundary: original_boundary(&maps.old, left[a.0][a.1].id, a.2)?,
                },
                new: SourceCut {
                    node: right[b.0][b.1].id,
                    token_boundary: original_boundary(&maps.new, right[b.0][b.1].id, b.2)?,
                },
                evidence: CutEvidence::CorrespondingContentEdge,
            },
            old_sources: old.padding[edge]
                .iter()
                .flat_map(|fragment| fragment.sources.iter().copied())
                .collect(),
            new_sources: new.padding[edge]
                .iter()
                .flat_map(|fragment| fragment.sources.iter().copied())
                .collect(),
        })
    };
    Some((
        boundary(old.entry, new.entry, 0, entry)?,
        boundary(old.exit, new.exit, 1, exit)?,
        Some(Box::new(SourceCutEdgeRefinement {
            convention: "mandatory-literal-space-content-edges-v1".into(),
            enclosing: [entry.certificate.clone(), exit.certificate.clone()],
            old_padding: old.padding,
            new_padding: new.padding,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::NodeKind, model::GlyphId};

    #[test]
    fn whitespace_only_nodes_share_the_reported_padding_cut() {
        for texts in [[" ", "Budget 10.", "  "], ["  ", "Budget 20.", " "]] {
            let mut next_glyph = 0;
            let nodes: Vec<_> = texts
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let sources: Vec<_> = text
                        .chars()
                        .map(|_| {
                            let source = SourceRef::Native {
                                glyph: GlyphId(next_glyph),
                            };
                            next_glyph += 1;
                            source
                        })
                        .collect();
                    GraphNode {
                        id: NodeId(index as u64),
                        kind: NodeKind::Paragraph,
                        pages: Vec::new(),
                        identity: None,
                        basis: ViewBasis::NativeLayout,
                        content: NodeContent::Text {
                            view: TextView {
                                tokens: text.chars().map(ComparableToken::Scalar).collect(),
                                origins: sources.iter().map(|source| vec![*source]).collect(),
                                source_backed: vec![true; sources.len()],
                                normalization: TextNormalization::Exact,
                            },
                        },
                        sources,
                    }
                })
                .collect();
            let runs = vec![nodes.iter().collect()];
            let trimmed = trim(
                &runs,
                (0, 0, 0),
                (0, 2, texts[2].len()),
                &OriginalCuts::new(),
                &mut 10_000,
            )
            .expect("mandatory padding");
            assert_eq!(trimmed.entry, (0, 0, texts[0].len()));
            assert_eq!(trimmed.exit, (0, 2, 0));
            let body = interior(&runs, trimmed.entry, trimmed.exit).expect("legal body");
            assert_eq!(
                body.iter()
                    .flat_map(|node| &node.sources)
                    .collect::<Vec<_>>(),
                nodes[1].sources.iter().collect::<Vec<_>>()
            );
            assert_eq!(trimmed.padding[0][0].sources, nodes[0].sources);
            assert_eq!(trimmed.padding[1][0].sources, nodes[2].sources);
        }
    }
}
