//! Local text extents cannot bridge known extraction gaps. This does not close
//! incomplete inventories or the search for undiscovered correspondence rivals.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DocumentView, EvidenceBoundary, NodeContent, NodeId, ScopeViewComparison, SourceRef};
use crate::model::GlyphId;

#[derive(Clone, Copy, Debug)]
pub struct ExtractionDependencyLimits {
    pub max_work: usize,
}

impl Default for ExtractionDependencyLimits {
    fn default() -> Self {
        Self {
            max_work: 1_000_000,
        }
    }
}

/// Issue indexes refer to the immutable old/new evidence stores. A limited check
/// has no absence-of-dependency meaning, even when its issue lists are empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionDependency {
    pub comparison: usize,
    pub old_issues: Vec<usize>,
    pub new_issues: Vec<usize>,
    pub work_limited: bool,
}

struct Gaps {
    glyphs: BTreeMap<GlyphId, (usize, usize)>,
    glyph_gaps: Vec<(usize, usize)>,
    page_gaps: Vec<(usize, usize)>,
}

fn charge(remaining: &mut usize, work: usize) -> Option<()> {
    if work > *remaining {
        *remaining = 0;
        return None;
    }
    *remaining -= work;
    Some(())
}

impl Gaps {
    fn new(view: DocumentView<'_>, remaining: &mut usize) -> Option<Self> {
        let store = view.evidence;
        charge(remaining, store.issues.len())?;
        let mut result = Self {
            glyphs: BTreeMap::new(),
            glyph_gaps: Vec::new(),
            page_gaps: Vec::new(),
        };
        for (index, issue) in store.issues.iter().enumerate() {
            match issue.boundary {
                Some(EvidenceBoundary::GlyphGap {
                    retained_before, ..
                })
                | Some(EvidenceBoundary::PageGlyphGap {
                    retained_before, ..
                }) => result.glyph_gaps.push((retained_before, index)),
                Some(EvidenceBoundary::PageGap {
                    retained_before, ..
                }) => result.page_gaps.push((retained_before, index)),
                None => {}
            }
        }
        if result.glyph_gaps.is_empty() && result.page_gaps.is_empty() {
            return Some(result);
        }
        charge(remaining, store.native.items().len())?;
        charge(remaining, store.pages.len())?;
        let pages: BTreeMap<_, _> = store
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| (page.page, index))
            .collect();
        result.glyphs = store
            .native
            .items()
            .iter()
            .enumerate()
            .map(|(index, glyph)| (glyph.id, (index, pages[&glyph.page])))
            .collect();
        result.glyph_gaps.sort_unstable();
        result.page_gaps.sort_unstable();
        Some(result)
    }

    fn dependencies(
        &self,
        nodes: &[NodeId],
        sources: &BTreeMap<NodeId, Option<&[SourceRef]>>,
        remaining: &mut usize,
    ) -> Option<Vec<usize>> {
        if self.glyph_gaps.is_empty() && self.page_gaps.is_empty() {
            return Some(Vec::new());
        }
        let mut extent = None;
        for node in nodes {
            charge(remaining, 1)?;
            let Some(refs) = sources.get(node)? else {
                continue;
            };
            charge(remaining, refs.len())?;
            for source in *refs {
                if let SourceRef::Native { glyph } = source {
                    let (position, page) = *self.glyphs.get(glyph)?;
                    let (min, max, first_page, last_page) =
                        extent.get_or_insert((position, position, page, page));
                    *min = (*min).min(position);
                    *max = (*max).max(position);
                    *first_page = (*first_page).min(page);
                    *last_page = (*last_page).max(page);
                }
            }
        }
        let Some((min, max, first_page, last_page)) = extent else {
            return Some(Vec::new());
        };
        let mut issues = Vec::new();
        for (gaps, start, end) in [
            (&self.glyph_gaps, min, max),
            (&self.page_gaps, first_page, last_page),
        ] {
            let from = gaps.partition_point(|(boundary, _)| *boundary <= start);
            let to = gaps.partition_point(|(boundary, _)| *boundary <= end);
            charge(remaining, to - from)?;
            issues.extend(gaps[from..to].iter().map(|(_, issue)| *issue));
        }
        issues.sort_unstable();
        issues.dedup();
        Some(issues)
    }
}

pub(super) fn apply<'a>(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    scopes: impl Iterator<Item = &'a mut ScopeViewComparison>,
    limits: ExtractionDependencyLimits,
) {
    let mut remaining = limits.max_work;
    let a = Gaps::new(old, &mut remaining);
    let b = Gaps::new(new, &mut remaining);
    if a.as_ref()
        .is_some_and(|gaps| gaps.glyph_gaps.is_empty() && gaps.page_gaps.is_empty())
        && b.as_ref()
            .is_some_and(|gaps| gaps.glyph_gaps.is_empty() && gaps.page_gaps.is_empty())
    {
        return;
    }
    let old_sources = source_map(old, &mut remaining);
    let new_sources = source_map(new, &mut remaining);
    for scope in scopes {
        for (index, comparison) in scope.comparisons.iter_mut().enumerate() {
            // Value and visual channels do not acquire native glyph-gap dependencies.
            if comparison
                .old
                .iter()
                .all(|node| matches!(old_sources.get(node), Some(None)))
                && comparison
                    .new
                    .iter()
                    .all(|node| matches!(new_sources.get(node), Some(None)))
            {
                continue;
            }
            let old_issues = a
                .as_ref()
                .and_then(|gaps| gaps.dependencies(&comparison.old, &old_sources, &mut remaining));
            let new_issues = b
                .as_ref()
                .and_then(|gaps| gaps.dependencies(&comparison.new, &new_sources, &mut remaining));
            let limited = old_issues.is_none() || new_issues.is_none();
            let old_issues = old_issues.unwrap_or_default();
            let new_issues = new_issues.unwrap_or_default();
            if !limited && old_issues.is_empty() && new_issues.is_empty() {
                continue;
            }
            comparison.operation = None;
            comparison.text_mask = None;
            comparison.compared = false;
            let reason = if limited {
                "extraction dependency search exceeded its work budget"
            } else {
                "text comparison crosses an unresolved extraction boundary"
            };
            comparison.unresolved.push(reason.into());
            scope
                .unresolved
                .push(format!("local comparison {index}: {reason}"));
            scope.extraction_dependencies.push(ExtractionDependency {
                comparison: index,
                old_issues,
                new_issues,
                work_limited: limited,
            });
        }
    }
}

fn source_map<'a>(
    view: DocumentView<'a>,
    remaining: &mut usize,
) -> BTreeMap<NodeId, Option<&'a [SourceRef]>> {
    let mut sources = BTreeMap::new();
    for node in &view.graph.nodes {
        if charge(remaining, 1).is_none() {
            break;
        }
        sources.insert(
            node.id,
            matches!(node.content, NodeContent::Text { .. }).then_some(node.sources.as_slice()),
        );
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_union_crosses_gaps_even_when_members_do_not() {
        let gaps = Gaps {
            glyphs: BTreeMap::from([(GlyphId(71), (0, 0)), (GlyphId(503), (1, 1))]),
            glyph_gaps: vec![(1, 7)],
            page_gaps: vec![(1, 8)],
        };
        let a = [SourceRef::Native { glyph: GlyphId(71) }];
        let b = [SourceRef::Native {
            glyph: GlyphId(503),
        }];
        let nodes = BTreeMap::from([
            (NodeId(1), Some(a.as_slice())),
            (NodeId(2), Some(b.as_slice())),
        ]);
        assert_eq!(
            gaps.dependencies(&[NodeId(1)], &nodes, &mut 100),
            Some(vec![])
        );
        assert_eq!(
            gaps.dependencies(&[NodeId(2)], &nodes, &mut 100),
            Some(vec![])
        );
        assert_eq!(
            gaps.dependencies(&[NodeId(1), NodeId(2)], &nodes, &mut 100),
            Some(vec![7, 8])
        );
        assert_eq!(
            gaps.dependencies(&[NodeId(1), NodeId(2)], &nodes, &mut 0),
            None
        );
    }
}
