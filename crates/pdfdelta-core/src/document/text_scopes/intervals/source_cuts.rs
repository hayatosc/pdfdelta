//! Reconstruct source-cut domains from immutable native populations.

use super::*;
use crate::{document::TextView, normalize::ComparableToken};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CutPartition {
    range: SourceCutRange,
    old_remainder: Vec<Remainder>,
    new_remainder: Vec<Remainder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Remainder {
    parent: NodeId,
    sources: Vec<SourceRef>,
}

struct Population {
    nodes: Vec<GraphNode>,
    maps: Vec<Option<Vec<Option<usize>>>>,
    offsets: Vec<usize>,
    tokens: Vec<ComparableToken>,
    optional: Vec<bool>,
}

fn text(node: &GraphNode) -> Option<&TextView> {
    match &node.content {
        NodeContent::Text { view } => Some(view),
        _ => None,
    }
}

impl Population {
    fn new(
        view: DocumentView<'_>,
        root: NodeId,
        ids: &[NodeId],
        native: &native::Sources<'_>,
        remaining: &mut usize,
    ) -> Option<Self> {
        spend(remaining, view.graph.nodes.len().saturating_add(ids.len()))?;
        let nodes: BTreeMap<_, _> = view
            .graph
            .nodes
            .iter()
            .map(|node| (node.id, node))
            .collect();
        let path: Vec<_> = ids
            .iter()
            .map(|id| nodes.get(id).copied())
            .collect::<Option<_>>()?;
        native.closed(view, root, &path, remaining)?;
        let mut result = Self {
            nodes: Vec::new(),
            maps: Vec::new(),
            offsets: Vec::new(),
            tokens: Vec::new(),
            optional: Vec::new(),
        };
        let mut population = BTreeSet::new();
        for node in path {
            let (projected, map) = native.project_census(node, &[], false, remaining)?;
            let view = text(&projected)?;
            let partition =
                TextSourcePartition::new(&projected, 0..view.tokens.len(), remaining).ok()?;
            if partition.remaining_sources().next().is_some() {
                return None;
            }
            for source in partition.selected_sources() {
                spend(remaining, 1)?;
                if !population.insert(source) {
                    return None;
                }
            }
            spend(remaining, view.tokens.len().saturating_mul(2))?;
            result.offsets.push(result.tokens.len());
            result.tokens.extend_from_slice(&view.tokens);
            result.optional.extend(view.optional_tokens()?);
            result.nodes.push(projected);
            result.maps.push(map);
        }
        Some(result)
    }

    fn locate(&self, cut: &SourceCut, remaining: &mut usize) -> Option<(usize, usize)> {
        spend(remaining, self.nodes.len())?;
        let index = self.nodes.iter().position(|node| node.id == cut.node)?;
        let position = if let Some(map) = &self.maps[index] {
            spend(remaining, map.len())?;
            map.iter()
                .position(|position| *position == Some(cut.token_boundary))?
        } else {
            cut.token_boundary
        };
        (position <= text(&self.nodes[index])?.tokens.len()).then_some((index, position))
    }

    fn fragment(
        &self,
        fragment: &SourceFragment,
        remaining: &mut usize,
    ) -> Option<Vec<ComparableToken>> {
        let (index, start) = self.locate(
            &SourceCut {
                node: fragment.node,
                token_boundary: fragment.tokens[0],
            },
            remaining,
        )?;
        let (_, end) = self.locate(
            &SourceCut {
                node: fragment.node,
                token_boundary: fragment.tokens[1],
            },
            remaining,
        )?;
        let node = &self.nodes[index];
        let view = text(node)?;
        let partition = TextSourcePartition::new(node, start..end, remaining).ok()?;
        spend(
            remaining,
            end.checked_sub(start)?
                .saturating_add(fragment.sources.len()),
        )?;
        if !partition
            .selected_sources()
            .eq(fragment.sources.iter().copied())
            || !view.source_backed[start..end].iter().all(|backed| *backed)
            || self.optional[self.offsets[index] + start..self.offsets[index] + end]
                .iter()
                .any(|optional| *optional)
        {
            return None;
        }
        Some(view.tokens[start..end].to_vec())
    }

    fn select(
        &self,
        entry: &SourceCut,
        exit: &SourceCut,
        remaining: &mut usize,
    ) -> Option<(Vec<GraphNode>, Vec<SourceRef>, Vec<Remainder>)> {
        let (a, start) = self.locate(entry, remaining)?;
        let (b, end) = self.locate(exit, remaining)?;
        let start = self.offsets[a].checked_add(start)?;
        let end = self.offsets[b].checked_add(end)?;
        if start > end {
            return None;
        }
        let mut selected = Vec::new();
        let mut sources = Vec::new();
        let mut remainders = Vec::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let length = text(node)?.tokens.len();
            let low = start.saturating_sub(self.offsets[index]).min(length);
            let high = end.saturating_sub(self.offsets[index]).min(length);
            let partition = TextSourcePartition::new(node, low..high, remaining).ok()?;
            sources.extend(partition.selected_sources());
            remainders.push(Remainder {
                parent: node.id,
                sources: partition.remaining_sources().collect(),
            });
            if low != high {
                selected.push(partition.selected_node().ok()?);
            }
        }
        Some((selected, sources, remainders))
    }

    fn inside_frame(
        &self,
        entry: &SourceCut,
        exit: &SourceCut,
        remaining: &mut usize,
    ) -> Option<()> {
        let (a, start) = self.locate(entry, remaining)?;
        let (b, end) = self.locate(exit, remaining)?;
        let first_end = text(self.nodes.first()?)?.tokens.len();
        let last_start = *self.offsets.last()?;
        (self.offsets[a].checked_add(start)? >= first_end
            && self.offsets[b].checked_add(end)? <= last_start)
            .then_some(())
    }
}

fn boundary(
    cut: &CutCorrespondence,
    populations: [&Population; 2],
    outer: [usize; 2],
    result: &ScopeViewComparison,
    remaining: &mut usize,
) -> Option<()> {
    match &cut.evidence {
        CutEvidence::AcceptedBoundary { proposal } => {
            if !outer.contains(proposal) {
                return None;
            }
            let proposal = result.candidates.proposals.get(*proposal)?;
            if proposal.old != [cut.old.node] || proposal.new != [cut.new.node] {
                return None;
            }
            let mut ends = Vec::new();
            for (population, point) in populations.into_iter().zip([&cut.old, &cut.new]) {
                let (index, position) = population.locate(point, remaining)?;
                let view = text(&population.nodes[index])?;
                spend(remaining, view.tokens.len())?;
                if position != 0 && position != view.tokens.len() {
                    return None;
                }
                ends.push(position == view.tokens.len());
            }
            // Equality of the native boundary bodies is checked independently.
            // Their exterior literal padding remains in the parent complement.
            (ends[0] == ends[1]).then_some(())
        }
        CutEvidence::UniqueNativeFragment { old, new } => {
            let mut patterns = Vec::new();
            let mut ends = Vec::new();
            for ((population, fragment), point) in populations
                .into_iter()
                .zip([old, new])
                .zip([&cut.old, &cut.new])
            {
                if point.node != fragment.node {
                    return None;
                }
                let edge = fragment
                    .tokens
                    .iter()
                    .position(|boundary| *boundary == point.token_boundary)?;
                ends.push(edge);
                let pattern = population.fragment(fragment, remaining)?;
                spend(remaining, population.optional.len())?;
                if !super::super::cuts::unique_pattern(
                    &population.tokens,
                    &population.optional,
                    population.optional.iter().all(|optional| !optional),
                    &pattern,
                    remaining,
                )? {
                    return None;
                }
                patterns.push(pattern);
            }
            (ends[0] == ends[1] && patterns[0] == patterns[1]).then_some(())
        }
        CutEvidence::CorrespondingContentEdge => None,
    }
}

fn outer_content(
    populations: [&Population; 2],
    last: bool,
    padding: bool,
    remaining: &mut usize,
) -> Option<()> {
    let mut bodies = Vec::new();
    for population in populations {
        let index = if last {
            population.nodes.len().checked_sub(1)?
        } else {
            0
        };
        let node = population.nodes.get(index)?;
        let view = text(node)?;
        spend(remaining, view.tokens.len().saturating_mul(4))?;
        let range = if padding {
            let start = view
                .tokens
                .iter()
                .position(|token| token.as_scalar() != Some(' '))?;
            let end = view
                .tokens
                .iter()
                .rposition(|token| token.as_scalar() != Some(' '))?
                + 1;
            start..end
        } else {
            0..view.tokens.len()
        };
        TextSourcePartition::new(node, range.clone(), remaining).ok()?;
        if range.is_empty()
            || !view.source_backed[range.clone()]
                .iter()
                .all(|backed| *backed)
            || population.optional
                [population.offsets[index] + range.start..population.offsets[index] + range.end]
                .iter()
                .any(|optional| *optional)
        {
            return None;
        }
        bodies.push(&view.tokens[range]);
    }
    (bodies[0] == bodies[1]).then_some(())
}

pub(super) fn prepare_equal(
    views: [DocumentView<'_>; 2],
    native: [&native::Sources<'_>; 2],
    result: &ScopeViewComparison,
    candidate: &super::EqualCandidate,
    remaining: &mut usize,
) -> Option<Prepared> {
    let first = result.candidates.proposals.get(candidate.boundaries[0])?;
    let last = result.candidates.proposals.get(candidate.boundaries[1])?;
    let ([a0], [a1], [b0], [b1]) = (
        first.old.as_slice(),
        last.old.as_slice(),
        first.new.as_slice(),
        last.new.as_slice(),
    ) else {
        return None;
    };
    let mut paths = [Vec::new(), Vec::new()];
    let mut starts = [
        SourceCut {
            node: *a0,
            token_boundary: 0,
        },
        SourceCut {
            node: *b0,
            token_boundary: 0,
        },
    ];
    for (side, first, last, members) in
        [(0, *a0, *a1, &candidate.old), (1, *b0, *b1, &candidate.new)]
    {
        spend(
            remaining,
            views[side]
                .graph
                .nodes
                .len()
                .saturating_add(members.len())
                .saturating_add(2),
        )?;
        let node = views[side]
            .graph
            .nodes
            .iter()
            .find(|node| node.id == first)?;
        starts[side].token_boundary = text(node)?.tokens.len();
        paths[side].push(first);
        paths[side].extend(members);
        paths[side].push(last);
    }
    let [old, new] = paths;
    let [old_start, new_start] = starts;
    // The existing validator proves both boundary bodies in the complete
    // interval population. It retains the full boundary parents as remainders;
    // no standalone paragraph ownership or new occurrence search is required.
    prepare(
        views,
        native,
        result,
        &SourceCutRange {
            convention: "unique-native-fragment-cuts-v2".into(),
            projection: "retained-glyph-whitespace-expansion-v1".into(),
            population: SourceCutPopulation::MatchedInterval {
                boundaries: candidate.boundaries,
                old,
                new,
                native_regions: None,
                boundary_padding: None,
                row_order: None,
            },
            entry: CutCorrespondence {
                old: old_start,
                new: new_start,
                evidence: CutEvidence::AcceptedBoundary {
                    proposal: candidate.boundaries[0],
                },
            },
            exit: CutCorrespondence {
                old: SourceCut {
                    node: *a1,
                    token_boundary: 0,
                },
                new: SourceCut {
                    node: *b1,
                    token_boundary: 0,
                },
                evidence: CutEvidence::AcceptedBoundary {
                    proposal: candidate.boundaries[1],
                },
            },
            edge_refinement: None,
        },
        remaining,
    )
}

pub(super) fn prepare(
    views: [DocumentView<'_>; 2],
    native: [&native::Sources<'_>; 2],
    result: &ScopeViewComparison,
    range: &SourceCutRange,
    remaining: &mut usize,
) -> Option<Prepared> {
    let SourceCutPopulation::MatchedInterval {
        boundaries,
        old,
        new,
        native_regions: None,
        boundary_padding: None,
        row_order: None,
    } = &range.population
    else {
        return None;
    };
    if range.edge_refinement.is_some() {
        return None;
    }
    spend(
        remaining,
        result
            .matching
            .source_only_mandatory
            .len()
            .saturating_add(result.matching.inferred_proposals.len())
            .saturating_add(result.accepted_correspondences.len())
            .saturating_add(result.text_boundary_correspondences.len())
            .saturating_mul(2),
    )?;
    if !boundaries.iter().all(|index| {
        result.matching.source_only_mandatory.contains(index)
            && !result.matching.inferred_proposals.contains(index)
            && (result.accepted_correspondences.contains(index)
                || result.text_boundary_correspondences.contains(index))
    }) {
        return None;
    }
    let first = result.candidates.proposals.get(boundaries[0])?;
    let last = result.candidates.proposals.get(boundaries[1])?;
    for (path, first, last) in [(old, &first.old, &last.old), (new, &first.new, &last.new)] {
        if first.len() != 1
            || last.len() != 1
            || path.first() != first.first()
            || path.last() != last.last()
        {
            return None;
        }
    }
    let a = Population::new(
        views[0],
        result.matching.scope.old,
        old,
        native[0],
        remaining,
    )?;
    let b = Population::new(
        views[1],
        result.matching.scope.new,
        new,
        native[1],
        remaining,
    )?;
    outer_content(
        [&a, &b],
        false,
        first.basis == ProposalBasis::LiteralContentWithPadding,
        remaining,
    )?;
    outer_content(
        [&a, &b],
        true,
        last.basis == ProposalBasis::LiteralContentWithPadding,
        remaining,
    )?;
    boundary(&range.entry, [&a, &b], *boundaries, result, remaining)?;
    boundary(&range.exit, [&a, &b], *boundaries, result, remaining)?;
    a.inside_frame(&range.entry.old, &range.exit.old, remaining)?;
    b.inside_frame(&range.entry.new, &range.exit.new, remaining)?;
    let (old, old_sources, old_remainder) =
        a.select(&range.entry.old, &range.exit.old, remaining)?;
    let (new, new_sources, new_remainder) =
        b.select(&range.entry.new, &range.exit.new, remaining)?;
    Some(Prepared {
        boundaries: *boundaries,
        old,
        new,
        old_sources,
        new_sources,
        cut_partition: Some(CutPartition {
            range: range.clone(),
            old_remainder,
            new_remainder,
        }),
    })
}
