//! A declared native tag sequence can order independently closed page regions.
//! Page adjacency and render order alone never establish these transitions.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use super::*;
use crate::document::StructuredValue;

mod order;

pub(super) struct Membership<'a> {
    structure: SourceRef,
    glyphs: Cow<'a, [GlyphId]>,
    offset: usize,
    convention: &'static str,
}

pub(super) fn native_memberships<'a>(
    view: DocumentView<'a>,
    remaining: &mut usize,
) -> Option<Vec<Membership<'a>>> {
    order::acquire(view, remaining)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRegion {
    pub page: PageId,
    pub nodes: Vec<NodeId>,
    pub bounded_paint: bool,
    /// A page-local row census may order disjoint same-baseline boundary nodes.
    /// The native membership must still bind the complete inter-page sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_order: Option<String>,
}

/// The two adjacent glyphs have consecutive positions in the retained native
/// structure membership. The full path is checked against that sequence too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTransition {
    pub before: SourceRef,
    pub after: SourceRef,
    pub structure: SourceRef,
    pub position: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRegionChain {
    pub convention: String,
    pub regions: Vec<NativeRegion>,
    pub transitions: Vec<NativeTransition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRegionChains {
    pub old: Option<NativeRegionChain>,
    pub new: Option<NativeRegionChain>,
}

fn memberships<'a>(view: DocumentView<'a>, remaining: &mut usize) -> Option<Vec<Membership<'a>>> {
    if view.evidence.structured.is_empty() {
        return Some(Vec::new());
    }
    spend(
        remaining,
        view.evidence
            .inventories
            .len()
            .saturating_add(view.evidence.issues.len()),
    )?;
    // A partial tag tree can hide an overlapping or contrary declaration.
    if !view.evidence.inventory_complete(None, Channel::Relations) {
        return None;
    }
    let mut acquired = BTreeSet::new();
    for inventory in &view.evidence.inventories {
        if inventory.page.is_none()
            && inventory.channel == Channel::Relations
            && inventory.complete
            && view.evidence.backends.get(inventory.backend)?.kind == BackendKind::NativeParser
        {
            spend(
                remaining,
                inventory.sources.len().saturating_mul(
                    view.evidence.structured.len().saturating_add(1).ilog2() as usize + 1,
                ),
            )?;
            acquired.extend(
                inventory
                    .sources
                    .iter()
                    .map(|source| (inventory.backend, *source)),
            );
        }
    }
    let mut result = Vec::new();
    let mut owned = BTreeSet::new();
    for element in &view.evidence.structured {
        spend(remaining, 1)?;
        let StructuredValue::StructureElement { glyphs, .. } = &element.value else {
            continue;
        };
        if view.evidence.backends.get(element.backend)?.kind != BackendKind::NativeParser {
            continue;
        }
        let source = SourceRef::Structured {
            element: element.id,
        };
        spend(
            remaining,
            acquired.len().saturating_add(1).ilog2() as usize + 1,
        )?;
        if !acquired.contains(&(element.backend, source)) {
            return None;
        }
        let work = glyphs.len().saturating_mul(
            view.evidence.native.items().len().saturating_add(1).ilog2() as usize + 1,
        );
        spend(remaining, work)?;
        if glyphs.iter().any(|glyph| !owned.insert(*glyph)) {
            return None;
        }
        if !glyphs.is_empty() {
            result.push(Membership {
                structure: source,
                glyphs: Cow::Borrowed(glyphs),
                offset: 0,
                convention: "native-tag-ordered-page-regions-v1",
            });
        }
    }
    Some(result)
}

/// Candidate links only: complete page bands and the complete selected tag
/// subsequence still have to pass `closed` under matched outer boundaries.
pub(super) fn bridge(
    sources: &Sources<'_>,
    view: DocumentView<'_>,
    nodes: &BTreeMap<NodeId, &GraphNode>,
    blocked: &BTreeSet<NodeId>,
    next: &mut BTreeMap<NodeId, BTreeSet<NodeId>>,
    previous: &mut BTreeMap<NodeId, BTreeSet<NodeId>>,
    remaining: &mut usize,
) {
    let legacy = sources
        .native_order
        .is_none()
        .then(|| memberships(view, remaining))
        .flatten();
    let Some(memberships) = sources.native_order.as_deref().or(legacy.as_deref()) else {
        return;
    };
    if memberships.is_empty() {
        return;
    }
    let mut starts = BTreeMap::new();
    let mut ends = BTreeMap::new();
    for (&id, node) in nodes {
        if spend(
            remaining,
            (nodes.len().saturating_add(1).ilog2() as usize + 1) * 4,
        )
        .is_none()
        {
            return;
        }
        if blocked.contains(&id) {
            continue;
        }
        if !previous.contains_key(&id)
            && let Some(source) = node.sources.first()
        {
            starts.entry(*source).or_insert_with(Vec::new).push(id);
        }
        if !next.contains_key(&id)
            && let Some(source) = node.sources.last()
        {
            ends.entry(*source).or_insert_with(Vec::new).push(id);
        }
    }
    let mut proposals = Vec::new();
    for membership in memberships {
        for pair in membership.glyphs.windows(2) {
            if spend(remaining, ends.len().saturating_add(1).ilog2() as usize + 1).is_none() {
                return;
            }
            let Some(a) = ends.get(&SourceRef::Native { glyph: pair[0] }) else {
                continue;
            };
            if spend(
                remaining,
                starts.len().saturating_add(1).ilog2() as usize + 1,
            )
            .is_none()
            {
                return;
            }
            let Some(b) = starts.get(&SourceRef::Native { glyph: pair[1] }) else {
                continue;
            };
            let ([a], [b]) = (a.as_slice(), b.as_slice()) else {
                continue;
            };
            if spend(
                remaining,
                (nodes.len().saturating_add(1).ilog2() as usize + 1) * 2,
            )
            .is_none()
            {
                return;
            }
            if a != b && nodes[a].pages != nodes[b].pages {
                proposals.push((*a, *b));
            }
        }
    }
    for (a, b) in proposals {
        next.entry(a).or_default().insert(b);
        previous.entry(b).or_default().insert(a);
    }
}

pub(super) fn closed(
    sources: &Sources<'_>,
    view: DocumentView<'_>,
    root: NodeId,
    path: &[&GraphNode],
    remaining: &mut usize,
) -> Option<NativeRegionChain> {
    let legacy = sources
        .native_order
        .is_none()
        .then(|| memberships(view, remaining))
        .flatten();
    let memberships = sources.native_order.as_deref().or(legacy.as_deref())?;
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut offsets = Vec::new();
    let mut regions = Vec::new();
    let mut first = 0;
    for end in 1..=path.len() {
        if end < path.len() && path[end].pages == path[first].pages {
            continue;
        }
        let nodes = &path[first..end];
        let [page] = nodes[0].pages.as_slice() else {
            return None;
        };
        let (closure, row_order) =
            if let Some(closure) = sources.closed_page(view, root, nodes, remaining) {
                (closure, None)
            } else {
                let closure = sources.closed_page_with_padding(
                    view,
                    root,
                    nodes,
                    &BTreeSet::new(),
                    Some(RowOrder::Spatial),
                    remaining,
                )?;
                (closure, Some(RowOrder::Spatial.convention().to_owned()))
            };
        for node in nodes {
            spend(
                remaining,
                node.sources.len().saturating_mul(
                    view.evidence.native.items().len().saturating_add(1).ilog2() as usize + 2,
                ),
            )?;
            for source in &node.sources {
                let SourceRef::Native { glyph } = source else {
                    return None;
                };
                if !seen.insert(*glyph) {
                    return None;
                }
                selected.push(*glyph);
            }
        }
        regions.push(NativeRegion {
            page: *page,
            nodes: nodes.iter().map(|node| node.id).collect(),
            bounded_paint: closure.bounded_paint(),
            row_order,
        });
        if end < path.len() {
            offsets.push(selected.len());
        }
        first = end;
    }
    if regions.len() < 2 {
        return None;
    }
    for membership in memberships {
        let glyphs = &membership.glyphs;
        spend(remaining, glyphs.len().saturating_add(selected.len()))?;
        let Some(start) = glyphs
            .iter()
            .position(|glyph| Some(glyph) == selected.first())
        else {
            continue;
        };
        if glyphs.get(start..start.checked_add(selected.len())?) != Some(selected.as_slice()) {
            continue;
        }
        return Some(NativeRegionChain {
            convention: membership.convention.into(),
            regions,
            transitions: offsets
                .into_iter()
                .map(|offset| NativeTransition {
                    before: SourceRef::Native {
                        glyph: selected[offset - 1],
                    },
                    after: SourceRef::Native {
                        glyph: selected[offset],
                    },
                    structure: membership.structure,
                    position: membership.offset + start + offset,
                })
                .collect(),
        });
    }
    None
}
