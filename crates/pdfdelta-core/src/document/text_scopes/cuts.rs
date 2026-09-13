//! Reversible native row cuts. Discovery never mutates the retained graph.

use super::*;
use crate::document::{TextNormalization, TextView};
use crate::normalize::ComparableToken;

/// A token boundary in an existing retained source view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCut {
    pub node: NodeId,
    pub token_boundary: usize,
}

/// Half-open token interval and its physical source projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFragment {
    pub node: NodeId,
    pub tokens: [usize; 2],
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CutEvidence {
    AcceptedBoundary {
        proposal: usize,
    },
    UniqueNativeFragment {
        old: SourceFragment,
        new: SourceFragment,
    },
}

/// Equality and occurrence closure are local to the declared source universe;
/// this convention does not identify a semantic paragraph or author intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutCorrespondence {
    pub old: SourceCut,
    pub new: SourceCut,
    pub evidence: CutEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutRange {
    pub convention: String,
    pub projection: String,
    pub population: SourceCutPopulation,
    pub entry: CutCorrespondence,
    pub exit: CutCorrespondence,
}

/// The finite universe in which cut occurrences were counted. An interval's
/// correspondence remains conditional on its accepted enclosing boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceCutPopulation {
    CompletePage,
    MatchedInterval {
        boundaries: [usize; 2],
        old: Vec<NodeId>,
        new: Vec<NodeId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutSearch {
    pub convention: String,
    pub exhaustive: bool,
    pub examined_fragments: usize,
    pub paired_boundaries: usize,
    pub work: usize,
    pub budget_at_entry: usize,
}

type Position = (usize, usize, usize);
type Anchor = ((usize, usize), (usize, usize), usize);

struct Boundary {
    old: Position,
    new: Position,
    certificate: CutCorrespondence,
    old_sources: Vec<SourceRef>,
    new_sources: Vec<SourceRef>,
}

struct Row<'a> {
    position: (usize, usize),
    node: &'a GraphNode,
    range: std::ops::Range<usize>,
    tokens: Vec<ComparableToken>,
    before: bool,
    after: bool,
}

fn rows<'a>(
    runs: &[Vec<&'a GraphNode>],
    sources: &native::Sources<'_>,
    word_edges: bool,
    excluded: &BTreeSet<NodeId>,
    remaining: &mut usize,
) -> Option<Vec<Row<'a>>> {
    let mut result = Vec::new();
    for (run, nodes) in runs.iter().enumerate() {
        for (index, &node) in nodes.iter().enumerate() {
            if excluded.contains(&node.id) {
                continue;
            }
            let (_, rows) = sources.project(node, remaining)?;
            let NodeContent::Text { view } = &node.content else {
                continue;
            };
            let optional = view.optional_tokens()?;
            let mut candidates = Vec::new();
            for range in rows {
                if optional[range.clone()].iter().all(|optional| !optional) {
                    candidates.push((range, true, true));
                }
            }
            // Partial-row words only contribute the node's exterior edge.
            // A common prefix inside a changed row must not shrink its extent.
            let separator = |index: usize| {
                optional[index]
                    || view.tokens[index]
                        .as_scalar()
                        .is_some_and(|scalar| scalar.is_ascii_whitespace())
            };
            let first_end = (0..view.tokens.len())
                .find(|&index| separator(index))
                .unwrap_or(view.tokens.len());
            if word_edges && first_end > 0 {
                candidates.push((0..first_end, true, false));
            }
            let last_start = (0..view.tokens.len())
                .rfind(|&index| separator(index))
                .map_or(0, |index| index + 1);
            if word_edges && last_start < view.tokens.len() {
                candidates.push((last_start..view.tokens.len(), false, true));
            }
            for (range, before, after) in candidates {
                spend(
                    remaining,
                    range
                        .len()
                        .saturating_add(node.sources.len())
                        .saturating_add(view.tokens.len()),
                )?;
                if slice(node, range.start, range.end).is_none() {
                    continue;
                }
                result.push(Row {
                    position: (run, index),
                    node,
                    tokens: view.tokens[range.clone()].to_vec(),
                    range,
                    before,
                    after,
                });
            }
        }
    }
    Some(result)
}

/// Census the complete raw path across node partitions. A copy straddling two
/// retained nodes must prevent uniqueness just like a copy inside one node.
fn unique(
    tokens: &[ComparableToken],
    optional: &[bool],
    row: &Row<'_>,
    remaining: &mut usize,
) -> Option<bool> {
    // Each state retains at most two physical starts. Distinct skip/keep paths
    // with the same first/last token are one occurrence; two starts are already
    // enough to defeat uniqueness. A completed match never consumes padding.
    let mut states: BTreeMap<usize, [Option<usize>; 2]> = BTreeMap::new();
    let mut count = 0;
    for (position, token) in tokens.iter().enumerate() {
        let lookup_work = row.tokens.len().ilog2() as usize + 1;
        spend(
            remaining,
            1 + states.len().saturating_mul(lookup_work).saturating_mul(8),
        )?;
        let mut next = if optional[position] {
            states.clone()
        } else {
            BTreeMap::new()
        };
        for (&matched, starts) in &states {
            if *token == row.tokens[matched] {
                for start in starts.iter().copied().flatten() {
                    insert_start(next.entry(matched + 1).or_insert([None; 2]), start);
                }
            }
        }
        if *token == row.tokens[0] {
            spend(remaining, lookup_work.saturating_mul(4))?;
            insert_start(next.entry(1).or_insert([None; 2]), position);
        }
        count += next
            .remove(&row.tokens.len())
            .unwrap_or([None; 2])
            .iter()
            .flatten()
            .count();
        if count > 1 {
            return Some(false);
        }
        states = next;
    }
    Some(count == 1)
}

fn insert_start(starts: &mut [Option<usize>; 2], start: usize) {
    if !starts.contains(&Some(start))
        && let Some(slot) = starts.iter_mut().find(|slot| slot.is_none())
    {
        *slot = Some(start);
    }
}

fn fragment(row: &Row<'_>) -> SourceFragment {
    let NodeContent::Text { view } = &row.node.content else {
        unreachable!()
    };
    SourceFragment {
        node: row.node.id,
        tokens: [row.range.start, row.range.end],
        sources: view.origins[row.range.clone()]
            .iter()
            .flatten()
            .copied()
            .collect(),
    }
}

fn slice(node: &GraphNode, start: usize, end: usize) -> Option<GraphNode> {
    let NodeContent::Text { view } = &node.content else {
        return None;
    };
    let optional = view.optional_tokens()?;
    let selected: BTreeSet<_> = view.origins[start..end]
        .iter()
        .zip(&view.source_backed[start..end])
        .filter(|(_, backed)| **backed)
        .flat_map(|(origins, _)| origins.iter().copied())
        .collect();
    // A glyph may produce multiple scalars. Its complete projection must stay
    // on one side of the cut, and synthetic neighbors must not add ownership.
    if view
        .origins
        .iter()
        .zip(&view.source_backed)
        .enumerate()
        .filter(|(index, (_, backed))| **backed && (*index < start || *index >= end))
        .flat_map(|(_, (origins, _))| origins)
        .any(|source| selected.contains(source))
    {
        return None;
    }
    let mut sliced = TextView {
        tokens: view.tokens[start..end].to_vec(),
        origins: view.origins[start..end].to_vec(),
        source_backed: view.source_backed[start..end].to_vec(),
        normalization: TextNormalization::Exact,
    };
    let optional: Vec<_> = optional[start..end]
        .iter()
        .enumerate()
        .filter_map(|(index, optional)| optional.then_some(index))
        .collect();
    if !optional.is_empty() {
        sliced.bind_optional_positions(optional);
    }
    let mut result = node.clone();
    result.sources.retain(|source| selected.contains(source));
    result.content = NodeContent::Text { view: sliced };
    Some(result)
}

fn interior(runs: &[Vec<&GraphNode>], first: Position, last: Position) -> Option<Vec<GraphNode>> {
    if first.0 != last.0 || first > last {
        return None;
    }
    let mut result = Vec::new();
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
        if start < end {
            result.push(slice(node, start, end)?);
        }
    }
    Some(result)
}

fn same_raw_text(left: &[&GraphNode], right: &[&GraphNode], remaining: &mut usize) -> Option<bool> {
    spend(remaining, left.len().saturating_add(right.len()))?;
    fn views<'a>(nodes: &[&'a GraphNode]) -> Option<Vec<&'a TextView>> {
        nodes
            .iter()
            .map(|node| match &node.content {
                NodeContent::Text { view } => Some(view),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
    }
    let (left, right) = (views(left)?, views(right)?);
    let work = left
        .iter()
        .chain(&right)
        .map(|view| view.tokens.len())
        .fold(0usize, usize::saturating_add);
    spend(remaining, work)?;
    Some(
        left.iter()
            .flat_map(|view| &view.tokens)
            .eq(right.iter().flat_map(|view| &view.tokens)),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    left: &[Vec<&GraphNode>],
    right: &[Vec<&GraphNode>],
    sources: Option<&(native::Sources<'_>, native::Sources<'_>)>,
    anchors: &[Anchor],
    remaining: &mut usize,
) -> Result<()> {
    let Some((old_sources, new_sources)) = sources else {
        return Ok(());
    };
    let initial = *remaining;
    if population(old, result.matching.scope.old, left, old_sources, remaining).is_some()
        && population(
            new,
            result.matching.scope.new,
            right,
            new_sources,
            remaining,
        )
        .is_some()
    {
        compare_population(
            old,
            new,
            result,
            parent,
            limits,
            left,
            right,
            sources,
            anchors,
            SourceCutPopulation::CompletePage,
            remaining,
        )?;
        if let Some(search) = &mut result.source_cut_search {
            search.work = initial - *remaining;
        }
        return Ok(());
    }
    let mut aggregate = SourceCutSearch {
        convention: "matched-interval-native-fragment-cuts-v2".into(),
        exhaustive: anchors.len() >= 2,
        examined_fragments: 0,
        paired_boundaries: 0,
        work: 0,
        budget_at_entry: initial,
    };
    for pair in anchors.windows(2) {
        let [(a, b, entry), (c, d, exit)] = pair else {
            unreachable!()
        };
        if a.0 != c.0 || b.0 != d.0 || a.1 >= c.1 || b.1 >= d.1 {
            continue;
        }
        if c.1 == a.1 + 1 && d.1 == b.1 + 1 {
            continue;
        }
        let work = (c.1 - a.1 + 1)
            .saturating_add(d.1 - b.1 + 1)
            .saturating_mul(4);
        if spend(remaining, work).is_none() {
            aggregate.exhaustive = false;
            break;
        }
        let left = [left[a.0][a.1..=c.1].to_vec()];
        let right = [right[b.0][b.1..=d.1].to_vec()];
        match same_raw_text(&left[0], &right[0], remaining) {
            Some(true) => continue,
            None => {
                aggregate.exhaustive = false;
                continue;
            }
            Some(false) => {}
        }
        if old_sources
            .closed(old, result.matching.scope.old, &left[0], remaining)
            .is_none()
            || new_sources
                .closed(new, result.matching.scope.new, &right[0], remaining)
                .is_none()
        {
            aggregate.exhaustive = false;
            continue;
        }
        let anchors = [
            ((0, 0), (0, 0), *entry),
            ((0, left[0].len() - 1), (0, right[0].len() - 1), *exit),
        ];
        let population = SourceCutPopulation::MatchedInterval {
            boundaries: [*entry, *exit],
            old: left[0].iter().map(|node| node.id).collect(),
            new: right[0].iter().map(|node| node.id).collect(),
        };
        let project =
            |nodes: &[&GraphNode], sources: &native::Sources<'_>, remaining: &mut usize| {
                nodes
                    .iter()
                    .map(|node| {
                        sources
                            .project(node, remaining)
                            .map(|projection| projection.0)
                    })
                    .collect::<Option<Vec<_>>>()
            };
        let (Some(old_nodes), Some(new_nodes)) = (
            project(&left[0], old_sources, remaining),
            project(&right[0], new_sources, remaining),
        ) else {
            aggregate.exhaustive = false;
            continue;
        };
        let left = vec![old_nodes.iter().collect()];
        let right = vec![new_nodes.iter().collect()];
        compare_population(
            old, new, result, parent, limits, &left, &right, sources, &anchors, population,
            remaining,
        )?;
        if let Some(search) = result.source_cut_search.take() {
            aggregate.exhaustive &= search.exhaustive;
            aggregate.examined_fragments += search.examined_fragments;
            aggregate.paired_boundaries += search.paired_boundaries;
        }
    }
    aggregate.work = initial - *remaining;
    result.source_cut_search = Some(aggregate);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compare_population(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    left: &[Vec<&GraphNode>],
    right: &[Vec<&GraphNode>],
    sources: Option<&(native::Sources<'_>, native::Sources<'_>)>,
    anchors: &[Anchor],
    population: SourceCutPopulation,
    remaining: &mut usize,
) -> Result<()> {
    let Some((old_sources, new_sources)) = sources else {
        return Ok(());
    };
    let initial = *remaining;
    let mut search = SourceCutSearch {
        convention: "unique-native-fragment-cuts-v2".into(),
        exhaustive: false,
        examined_fragments: 0,
        paired_boundaries: 0,
        work: 0,
        budget_at_entry: initial,
    };
    let outcome = discover(
        left,
        right,
        old_sources,
        new_sources,
        anchors,
        matches!(population, SourceCutPopulation::MatchedInterval { .. }),
        &mut search,
        remaining,
    );
    search.work = initial - *remaining;
    let Some(mut boundaries) = outcome else {
        result.source_cut_search = Some(search);
        return Ok(());
    };
    let sort_work = boundaries
        .len()
        .saturating_mul(boundaries.len().saturating_add(1).ilog2() as usize + 1);
    if spend(remaining, sort_work).is_none() {
        search.work = initial - *remaining;
        result.source_cut_search = Some(search);
        return Ok(());
    }
    search.exhaustive = true;
    boundaries.sort_by_key(|boundary| boundary.old);
    boundaries.dedup_by(|a, b| a.old == b.old && a.new == b.new);
    search.work = initial - *remaining;
    search.paired_boundaries = boundaries.len();
    result.source_cut_search = Some(search);
    for pair in boundaries.windows(2) {
        if spend(remaining, boundaries.len()).is_none() {
            break;
        }
        let [entry, exit] = pair else { unreachable!() };
        if entry.new > exit.new
            || entry.old.0 != exit.old.0
            || entry.new.0 != exit.new.0
            || matches!(
                (&entry.certificate.evidence, &exit.certificate.evidence),
                (
                    CutEvidence::AcceptedBoundary { .. },
                    CutEvidence::AcceptedBoundary { .. }
                )
            )
        {
            continue;
        }
        // Crossing paired cuts mean this range lacks a consistent ordering.
        if boundaries.iter().any(|other| {
            other.new > entry.new
                && other.new < exit.new
                && (other.old <= entry.old || other.old >= exit.old)
        }) {
            continue;
        }
        let projection_work = left[entry.old.0][entry.old.1..=exit.old.1]
            .iter()
            .chain(&right[entry.new.0][entry.new.1..=exit.new.1])
            .map(|node| node.sources.len().saturating_mul(8))
            .fold(0usize, usize::saturating_add);
        if spend(remaining, projection_work).is_none() {
            break;
        }
        let (Some(a), Some(b)) = (
            interior(left, entry.old, exit.old),
            interior(right, entry.new, exit.new),
        ) else {
            continue;
        };
        let Some(kind) = a.first().or_else(|| b.first()).map(|node| node.kind) else {
            continue;
        };
        if a.len() > limits.matching.max_group_nodes
            || b.len() > limits.matching.max_group_nodes
            || a.iter().chain(&b).any(|node| node.kind != kind)
        {
            continue;
        }
        // Contiguous subpaths inherit the enclosing population's source and
        // paint closure: their bands are subsets of its checked band. Legal
        // slices retain the source order and never introduce another glyph.
        let a: Vec<_> = a.iter().collect();
        let b: Vec<_> = b.iter().collect();
        let old_extent: Vec<_> = a
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        let new_extent: Vec<_> = b
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        if result
            .text_scope_reviews
            .iter()
            .any(|review| review.old_sources == old_extent && review.new_sources == new_extent)
        {
            continue;
        }
        let mut comparison =
            match super::super::operations::compare_native_text_range(&a, &b, limits.local) {
                Ok(comparison) if comparison.compared && comparison.operation.is_some() => {
                    comparison
                }
                Ok(_) | Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {
                    continue;
                }
                Err(error) => return Err(error),
            };
        if !a.is_empty()
            && !b.is_empty()
            && let Some(TypedOperation::TextChanged {
                old: Some(a_text),
                new: Some(b_text),
            }) = &comparison.operation
            && (native::external_continuation(old, &a, a_text, b_text, remaining) != Some(false)
                || native::external_continuation(new, &b, b_text, a_text, remaining) != Some(false))
        {
            continue;
        }
        let (Some(old_spacing), Some(new_spacing)) = (
            old_sources.spacing(&a, remaining),
            new_sources.spacing(&b, remaining),
        ) else {
            continue;
        };
        if parent == InterpretationStatus::Inferred {
            comparison.interpretation = parent;
        }
        result.text_scope_reviews.push(TextScopeReview {
            convention: "closed-native-source-cut-interval-v1".into(),
            boundaries: Vec::new(),
            source_cuts: Some(SourceCutRange {
                convention: "unique-native-fragment-cuts-v2".into(),
                projection: "retained-glyph-ligatures-spacing-v1".into(),
                population: population.clone(),
                entry: entry.certificate.clone(),
                exit: exit.certificate.clone(),
            }),
            old_sources: old_extent,
            presence: interval_presence(a.is_empty(), b.is_empty()),
            new_sources: new_extent,
            old_boundaries: [entry.old_sources.clone(), exit.old_sources.clone()],
            new_boundaries: [entry.new_sources.clone(), exit.new_sources.clone()],
            candidate_search_exhaustive: Some(result.candidates.exhaustive),
            spacing: Some(TextScopeSpacing {
                convention: "source-space-interpretations-v1".into(),
                old: old_spacing,
                new: new_spacing,
            }),
            comparison,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn discover(
    left: &[Vec<&GraphNode>],
    right: &[Vec<&GraphNode>],
    old_sources: &native::Sources<'_>,
    new_sources: &native::Sources<'_>,
    anchors: &[Anchor],
    word_edges: bool,
    search: &mut SourceCutSearch,
    remaining: &mut usize,
) -> Option<Vec<Boundary>> {
    // The caller establishes a complete page or a closed matched interval.
    // Copies outside an interval cannot establish semantic identity; this
    // finite correspondence is explicitly conditional on its outer matches.
    let tokens = |runs: &[Vec<&GraphNode>], remaining: &mut usize| {
        let mut tokens = Vec::new();
        let mut optional = Vec::new();
        for node in runs.iter().flatten() {
            let NodeContent::Text { view } = &node.content else {
                return None;
            };
            spend(remaining, view.tokens.len())?;
            tokens.extend_from_slice(&view.tokens);
            optional.extend(view.optional_tokens()?);
        }
        Some((tokens, optional))
    };
    let (old_tokens, old_optional) = tokens(left, remaining)?;
    let (new_tokens, new_optional) = tokens(right, remaining)?;
    let mut boundaries = Vec::new();
    let mut old_anchors = BTreeSet::new();
    let mut new_anchors = BTreeSet::new();
    for &(a, b, proposal) in anchors {
        let (old_node, new_node) = (left[a.0][a.1], right[b.0][b.1]);
        let (NodeContent::Text { view: av }, NodeContent::Text { view: bv }) =
            (&old_node.content, &new_node.content)
        else {
            continue;
        };
        spend(
            remaining,
            av.tokens.len().min(bv.tokens.len()).saturating_add(
                old_node
                    .sources
                    .len()
                    .saturating_add(new_node.sources.len())
                    .saturating_mul(2),
            ),
        )?;
        // Padding correspondence does not make its raw prefix/suffix equal.
        // Such a boundary may still need a checked inner cut for literal space.
        if av.tokens == bv.tokens {
            old_anchors.insert(old_node.id);
            new_anchors.insert(new_node.id);
        }
        for (ap, bp) in [(0, 0), (av.tokens.len(), bv.tokens.len())] {
            boundaries.push(Boundary {
                old: (a.0, a.1, ap),
                new: (b.0, b.1, bp),
                certificate: CutCorrespondence {
                    old: SourceCut {
                        node: old_node.id,
                        token_boundary: ap,
                    },
                    new: SourceCut {
                        node: new_node.id,
                        token_boundary: bp,
                    },
                    evidence: CutEvidence::AcceptedBoundary { proposal },
                },
                old_sources: old_node.sources.clone(),
                new_sources: new_node.sources.clone(),
            });
        }
    }
    let a = rows(left, old_sources, word_edges, &old_anchors, remaining)?;
    let b = rows(right, new_sources, word_edges, &new_anchors, remaining)?;
    search.examined_fragments = a.len() + b.len();
    for a in &a {
        let mut old_unique = None;
        for b in &b {
            spend(
                remaining,
                a.tokens.len().min(b.tokens.len()).saturating_add(1),
            )?;
            if a.tokens != b.tokens {
                continue;
            }
            let old_unique = match old_unique {
                Some(value) => value,
                None => {
                    let value = unique(&old_tokens, &old_optional, a, remaining)?;
                    old_unique = Some(value);
                    value
                }
            };
            if !old_unique || !unique(&new_tokens, &new_optional, b, remaining)? {
                continue;
            }
            let (af, bf) = (fragment(a), fragment(b));
            for (ap, bp, allowed) in [
                (a.range.start, b.range.start, a.before && b.before),
                (a.range.end, b.range.end, a.after && b.after),
            ] {
                if !allowed {
                    continue;
                }
                boundaries.push(Boundary {
                    old: (a.position.0, a.position.1, ap),
                    new: (b.position.0, b.position.1, bp),
                    certificate: CutCorrespondence {
                        old: SourceCut {
                            node: a.node.id,
                            token_boundary: ap,
                        },
                        new: SourceCut {
                            node: b.node.id,
                            token_boundary: bp,
                        },
                        evidence: CutEvidence::UniqueNativeFragment {
                            old: af.clone(),
                            new: bf.clone(),
                        },
                    },
                    old_sources: af.sources.clone(),
                    new_sources: bf.sources.clone(),
                });
            }
        }
    }
    Some(boundaries)
}

fn population(
    view: DocumentView<'_>,
    root: NodeId,
    runs: &[Vec<&GraphNode>],
    sources: &native::Sources<'_>,
    remaining: &mut usize,
) -> Option<()> {
    let [run] = runs else { return None };
    let [page] = view.evidence.pages.as_slice() else {
        return None;
    };
    if !view
        .evidence
        .inventory_complete(Some(page.page), Channel::Text)
    {
        return None;
    }
    let mut projection = BTreeSet::new();
    for node in run {
        sources.exact_rows(node, remaining)?;
        spend(remaining, node.sources.len())?;
        if node
            .sources
            .iter()
            .any(|source| !projection.insert(*source))
        {
            return None;
        }
    }
    spend(remaining, view.evidence.native.items().len())?;
    if projection.len() != view.evidence.native.items().len()
        || view
            .evidence
            .native
            .items()
            .iter()
            .any(|glyph| !projection.contains(&SourceRef::Native { glyph: glyph.id }))
    {
        return None;
    }
    // The complete path also discharges partition, paint and detached-source
    // dependencies; discovery edges alone are never a closure certificate.
    sources.closed(view, root, run, remaining)?;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::NodeKind, model::GlyphId};

    fn source(id: u64) -> SourceRef {
        SourceRef::Native { glyph: GlyphId(id) }
    }

    fn node() -> GraphNode {
        let mut view = TextView {
            tokens: "fi x".chars().map(ComparableToken::Scalar).collect(),
            origins: vec![
                vec![source(1)],
                vec![source(1)],
                vec![source(1), source(2)],
                vec![source(2)],
            ],
            source_backed: vec![true, true, false, true],
            normalization: TextNormalization::Exact,
        };
        view.bind_optional_positions(vec![2]);
        GraphNode {
            id: NodeId(7),
            kind: NodeKind::Paragraph,
            pages: vec![],
            sources: vec![source(1), source(2)],
            identity: None,
            basis: ViewBasis::NativeLayout,
            content: NodeContent::Text { view },
        }
    }

    #[test]
    fn cuts_preserve_shared_glyphs_and_nonowning_separator_context() {
        let original = node();
        assert!(slice(&original, 0, 1).is_none());
        assert!(slice(&original, 1, 4).is_none());
        let left = slice(&original, 0, 2).expect("complete ligature projection");
        let right = slice(&original, 2, 4).expect("separator references are context");
        assert_eq!(left.id, original.id);
        assert_eq!(right.id, original.id);
        assert_eq!(left.sources, [source(1)]);
        assert_eq!(right.sources, [source(2)]);
        let NodeContent::Text { view } = right.content else {
            unreachable!()
        };
        assert_eq!(view.origins[0], [source(1), source(2)]);
        assert_eq!(view.optional_tokens(), Some(vec![true, false]));
        assert_eq!(view.source_backed, [false, true]);
        assert_eq!(original, node());
    }

    #[test]
    fn occurrence_census_matches_all_spacing_interpretations() {
        let node = node();
        for length in 1..=4 {
            for spelling in 0..1usize << length {
                let tokens: Vec<_> = (0..length)
                    .map(|index| {
                        ComparableToken::Scalar(if spelling & (1 << index) == 0 {
                            'a'
                        } else {
                            'b'
                        })
                    })
                    .collect();
                for choices in 0..1usize << length {
                    let optional: Vec<_> = (0..length)
                        .map(|index| choices & (1 << index) != 0)
                        .collect();
                    for pattern in ["a", "b", "aa", "ab", "ba", "bb", "aba"] {
                        let row = Row {
                            position: (0, 0),
                            node: &node,
                            range: 0..pattern.len(),
                            tokens: pattern.chars().map(ComparableToken::Scalar).collect(),
                            before: true,
                            after: true,
                        };
                        let mut occurrences = BTreeSet::new();
                        for retained in 0..1usize << length {
                            if (0..length)
                                .any(|index| !optional[index] && retained & (1 << index) == 0)
                            {
                                continue;
                            }
                            let indices: Vec<_> = (0..length)
                                .filter(|index| retained & (1 << index) != 0)
                                .collect();
                            for window in indices.windows(row.tokens.len()) {
                                if window.iter().map(|&index| &tokens[index]).eq(&row.tokens) {
                                    occurrences.insert((window[0], window[window.len() - 1]));
                                }
                            }
                        }
                        assert_eq!(
                            unique(&tokens, &optional, &row, &mut 100_000),
                            Some(occurrences.len() == 1),
                            "length {length}, spelling {spelling}, choices {choices}, pattern {pattern}"
                        );
                        assert!(unique(&tokens, &optional, &row, &mut 0).is_none());
                    }
                }
            }
        }
    }
}
