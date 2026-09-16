//! Reversible discovery of isolated prefixes painted separately from their body.
//! Exact baseline agreement and left-to-right separation propose an insertion;
//! comparison still requires complete source and paint closure of the interval.

use super::{BTreeMap, BTreeSet, GraphNode, NodeId, SourceRef, Sources, spend};

pub(super) fn attach(
    sources: &Sources<'_>,
    runs: &mut Vec<Vec<&GraphNode>>,
    connected: &BTreeSet<NodeId>,
    remaining: &mut usize,
) -> Option<()> {
    let nodes: BTreeMap<_, _> = runs.iter().flatten().map(|node| (node.id, *node)).collect();
    if nodes.keys().all(|id| connected.contains(id)) {
        return Some(());
    }
    let key = |page, y: f64| (page, if y == 0.0 { 0 } else { y.to_bits() });
    let mut starts = BTreeMap::<_, Vec<&GraphNode>>::new();
    for node in nodes.values().filter(|node| connected.contains(&node.id)) {
        spend(remaining, 1)?;
        let Some(SourceRef::Native { glyph }) = node.sources.first() else {
            continue;
        };
        let glyph = sources.glyphs.get(glyph)?;
        if glyph.baseline.y.is_finite() {
            starts
                .entry(key(glyph.page, glyph.baseline.y))
                .or_default()
                .push(node);
        }
    }
    let mut proposals = BTreeMap::<NodeId, Vec<NodeId>>::new();
    for prefix in nodes.values().filter(|node| !connected.contains(&node.id)) {
        spend(remaining, 2)?;
        let (Some(SourceRef::Native { glyph: first }), Some(SourceRef::Native { glyph: last })) =
            (prefix.sources.first(), prefix.sources.last())
        else {
            continue;
        };
        let (first, last) = (sources.glyphs.get(first)?, sources.glyphs.get(last)?);
        if first.page != last.page || first.baseline.y != last.baseline.y {
            continue;
        }
        spend(remaining, prefix.sources.len())?;
        let Some(a) = sources.geometry(prefix) else {
            continue;
        };
        if a.top != a.bottom || !a.top.is_finite() {
            continue;
        }
        let mut target = None;
        let mut ambiguous = false;
        for body in starts.get(&key(a.page, a.top)).into_iter().flatten() {
            spend(remaining, body.sources.len())?;
            let Some(b) = sources.geometry(body) else {
                continue;
            };
            if b.page == a.page && b.top == a.top && a.bounds.max.x < b.bounds.min.x {
                ambiguous |= target.is_some();
                target = Some(body.id);
            }
        }
        if !ambiguous && let Some(target) = target {
            proposals.entry(target).or_default().push(prefix.id);
        }
    }
    let insertions: BTreeMap<_, _> = proposals
        .into_iter()
        .filter_map(|(body, prefixes)| (prefixes.len() == 1).then_some((body, prefixes[0])))
        .collect();
    if insertions.is_empty() {
        return Some(());
    }
    let moved: BTreeSet<_> = insertions.values().copied().collect();
    spend(remaining, nodes.len().saturating_mul(3))?;
    for run in runs.iter_mut() {
        *run = run
            .iter()
            .filter(|node| !moved.contains(&node.id))
            .flat_map(|node| {
                insertions
                    .get(&node.id)
                    .map(|id| nodes[id])
                    .into_iter()
                    .chain(std::iter::once(*node))
            })
            .collect();
    }
    runs.retain(|run| !run.is_empty());
    Some(())
}
