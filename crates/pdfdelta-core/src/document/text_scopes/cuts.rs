//! Reversible native row cuts. Discovery never mutates the retained graph.

use super::*;
use crate::document::{TextNormalization, TextView};
use crate::normalize::ComparableToken;

mod edges;
mod pages;

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
    /// A mandatory literal-space edge inside the declared enclosing cuts.
    CorrespondingContentEdge,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_refinement: Option<Box<SourceCutEdgeRefinement>>,
}

/// A second, non-owning comparison of the content between literal edge spaces.
/// The original interval remains reported, including any space-only change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutEdgeRefinement {
    pub convention: String,
    pub enclosing: [CutCorrespondence; 2],
    pub old_padding: [Vec<SourceFragment>; 2],
    pub new_padding: [Vec<SourceFragment>; 2],
}

/// Only content outside the first or last agreed page boundary is eligible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCutPageEdge {
    Before,
    After,
}

/// The finite universe in which cut occurrences were counted. An interval's
/// correspondence remains conditional on its accepted enclosing boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceCutPopulation {
    CompletePage,
    /// Both pages have complete, disjoint native source paths. Their pairing
    /// remains conditional on this accepted text boundary, not page numbers.
    AnchoredPage {
        boundary: usize,
        /// Multiple local anchors permit only the outer side of this boundary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edge: Option<SourceCutPageEdge>,
        old_page: crate::model::PageId,
        new_page: crate::model::PageId,
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        /// Other accepted boundaries stay in the occurrence census but cannot
        /// supply cuts or compared content. This includes counterparts outside
        /// the page pair and local anchors excluded by an outer-edge view.
        external_boundaries: Vec<usize>,
    },
    MatchedInterval {
        boundaries: [usize; 2],
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_regions: Option<Box<NativeRegionChains>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundary_padding: Option<Box<SourceCutBoundaryPadding>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_order: Option<Box<SourceCutRowOrder>>,
    },
}

/// Monoline outer endpoints of a horizontal source interval. Spatial prefix
/// order uses exact baselines; raw paint order permits an eight-machine-epsilon
/// relative envelope for row classification without rounding any coordinates.
/// The convention selects the order of disjoint row pieces. Only glyphs
/// outside the endpoint cuts in that declared order may leave the census;
/// the intervening source and paint band must still close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutRowOrder {
    pub convention: String,
    pub old: Option<[SourceCutRowEndpoint; 2]>,
    pub new: Option<[SourceCutRowEndpoint; 2]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutRowEndpoint {
    pub node: NodeId,
    pub sources: Vec<SourceRef>,
}

/// Partially clipped outer padding participates in the occurrence census with
/// both presence choices, but cannot be consumed by any compared source range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutBoundaryPadding {
    pub convention: String,
    pub old: Vec<SourceRef>,
    pub new: Vec<SourceRef>,
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

fn same_nodes(left: &[&GraphNode], right: &[&GraphNode]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| std::ptr::eq(*a, *b))
}

type OriginalCuts = BTreeMap<NodeId, Vec<Option<usize>>>;

#[derive(Default)]
struct CutMaps {
    old: OriginalCuts,
    new: OriginalCuts,
}

fn original_boundary(maps: &OriginalCuts, node: NodeId, position: usize) -> Option<usize> {
    maps.get(&node)
        .map_or(Some(position), |map| map.get(position).copied().flatten())
}

#[derive(Clone)]
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
    paint_order: bool,
    excluded: &BTreeSet<NodeId>,
    remaining: &mut usize,
) -> Option<Vec<Row<'a>>> {
    let mut result = Vec::new();
    for (run, nodes) in runs.iter().enumerate() {
        for (index, &node) in nodes.iter().enumerate() {
            if excluded.contains(&node.id) {
                continue;
            }
            let (_, rows) = sources.project(node, paint_order, remaining)?;
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
            // A discretionary terminal hyphen must not hide the mandatory
            // suffix. The hyphen stays in the population and outside the
            // fragment, so uniqueness still checks both interpretations.
            let last_end = view.tokens.len()
                - usize::from(
                    optional.last() == Some(&true)
                        && view.tokens.last().and_then(ComparableToken::as_scalar) == Some('-'),
                );
            let last_start = (0..last_end)
                .rfind(|&index| separator(index))
                .map_or(0, |index| index + 1);
            if word_edges && last_start < last_end {
                candidates.push((last_start..last_end, false, true));
            }
            for (range, before, after) in candidates {
                spend(
                    remaining,
                    range
                        .len()
                        .saturating_add(node.sources.len())
                        .saturating_add(view.tokens.len()),
                )?;
                if slice(node, range.start, range.end, remaining).is_none() {
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
    mandatory: bool,
    row: &Row<'_>,
    remaining: &mut usize,
) -> Option<bool> {
    unique_pattern(tokens, optional, mandatory, &row.tokens, remaining)
}

pub(super) fn unique_pattern(
    tokens: &[ComparableToken],
    optional: &[bool],
    mandatory: bool,
    needle: &[ComparableToken],
    remaining: &mut usize,
) -> Option<bool> {
    if mandatory {
        return unique_mandatory(tokens, needle, remaining);
    }
    if needle.is_empty() || optional.len() != tokens.len() {
        return None;
    }
    // Each state retains at most two physical starts. Distinct skip/keep paths
    // with the same first/last token are one occurrence; two starts are already
    // enough to defeat uniqueness. A completed match never consumes padding.
    // Indexed frontiers avoid tree lookups; only active cells are visited and
    // cleared, so long patterns do not require a full scan for every token.
    let width = needle.len().checked_add(1)?;
    spend(remaining, width.saturating_mul(8))?;
    let mut states = vec![[None; 2]; width];
    let mut next = vec![[None; 2]; width];
    let mut active: Vec<usize> = Vec::with_capacity(width);
    let mut next_active = Vec::with_capacity(width);
    let mut count = 0;
    for (position, token) in tokens.iter().enumerate() {
        spend(remaining, 1 + active.len().saturating_mul(16))?;
        let mut add = |matched: usize, start| {
            if next[matched] == [None; 2] && matched < needle.len() {
                next_active.push(matched);
            }
            insert_start(&mut next[matched], start);
        };
        for &matched in &active {
            for start in states[matched].iter().copied().flatten() {
                if optional[position] {
                    add(matched, start);
                }
                if *token == needle[matched] {
                    add(matched + 1, start);
                }
            }
        }
        if *token == needle[0] {
            spend(remaining, 4)?;
            add(1, position);
        }
        count += next[needle.len()].iter().flatten().count();
        if count > 1 {
            return Some(false);
        }
        next[needle.len()] = [None; 2];
        for matched in active.drain(..) {
            states[matched] = [None; 2];
        }
        std::mem::swap(&mut states, &mut next);
        std::mem::swap(&mut active, &mut next_active);
    }
    Some(count == 1)
}

/// Count overlapping literal occurrences in linear work when no source token
/// can be skipped. Optional spacing still uses the full state census above.
fn unique_mandatory(
    tokens: &[ComparableToken],
    needle: &[ComparableToken],
    remaining: &mut usize,
) -> Option<bool> {
    if needle.is_empty() {
        return None;
    }
    spend(remaining, needle.len())?;
    let mut prefixes = vec![0; needle.len()];
    for end in 1..needle.len() {
        let mut length = prefixes[end - 1];
        loop {
            spend(remaining, 1)?;
            if needle[end] == needle[length] {
                prefixes[end] = length + 1;
                break;
            }
            if length == 0 {
                break;
            }
            length = prefixes[length - 1];
        }
    }
    let mut matched = 0;
    let mut count = 0;
    for token in tokens {
        loop {
            spend(remaining, 1)?;
            if *token == needle[matched] {
                matched += 1;
                break;
            }
            if matched == 0 {
                break;
            }
            matched = prefixes[matched - 1];
        }
        if matched == needle.len() {
            count += 1;
            if count > 1 {
                return Some(false);
            }
            matched = prefixes[matched - 1];
        }
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

fn fragment(row: &Row<'_>, maps: &OriginalCuts) -> Option<SourceFragment> {
    let NodeContent::Text { view } = &row.node.content else {
        unreachable!()
    };
    Some(SourceFragment {
        node: row.node.id,
        tokens: [
            original_boundary(maps, row.node.id, row.range.start)?,
            original_boundary(maps, row.node.id, row.range.end)?,
        ],
        sources: view.origins[row.range.clone()]
            .iter()
            .flatten()
            .copied()
            .collect(),
    })
}

fn slice_sources(
    node: &GraphNode,
    start: usize,
    end: usize,
    remaining: &mut usize,
) -> Option<BTreeSet<SourceRef>> {
    let partition = super::super::TextSourcePartition::new(node, start..end, remaining).ok()?;
    Some(partition.selected_sources().collect())
}

fn slice(node: &GraphNode, start: usize, end: usize, remaining: &mut usize) -> Option<GraphNode> {
    let partition = super::super::TextSourcePartition::new(node, start..end, remaining).ok()?;
    partition.selected_node().ok()
}

fn interior<'nodes>(
    runs: &[Vec<&'nodes GraphNode>],
    first: Position,
    last: Position,
    remaining: &mut usize,
) -> Option<Vec<std::borrow::Cow<'nodes, GraphNode>>> {
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
        spend(remaining, 1)?;
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
            if start == 0
                && end == view.tokens.len()
                && matches!(view.normalization, TextNormalization::Exact)
            {
                // The enclosing census already validated this complete view.
                // Only its source references are copied into the later extent.
                spend(remaining, node.sources.len())?;
                result.push(std::borrow::Cow::Borrowed(node));
                continue;
            }
            // Empty boundary endpoints contribute no cloned tokens or sources.
            // Charge each retained slice before validating and allocating it.
            result.push(std::borrow::Cow::Owned(slice(node, start, end, remaining)?));
        }
    }
    Some(result)
}

fn whole_interior<'a, 'b>(
    runs: &'a [Vec<&'b GraphNode>],
    first: Position,
    last: Position,
) -> Option<&'a [&'b GraphNode]> {
    if first.0 != last.0 || first.1 >= last.1 || last.2 != 0 {
        return None;
    }
    let NodeContent::Text { view } = &runs[first.0][first.1].content else {
        return None;
    };
    (first.2 == view.tokens.len()).then(|| &runs[first.0][first.1 + 1..last.1])
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
    // Equal retained tokens do not prove equal literal-space multiplicity when
    // a source-backed token contracts several native glyphs. Project it first.
    if left.iter().chain(&right).any(|view| {
        view.origins
            .iter()
            .zip(&view.source_backed)
            .any(|(origins, backed)| *backed && origins.len() > 1)
    }) {
        return Some(false);
    }
    Some(
        left.iter()
            .flat_map(|view| &view.tokens)
            .eq(right.iter().flat_map(|view| &view.tokens)),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Pass {
    Standard,
    WholeIntervals,
    EnclosingIntervals,
    RefineExisting,
    Paint,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append<'nodes>(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    left: &[Vec<&'nodes GraphNode>],
    right: &[Vec<&'nodes GraphNode>],
    sources: Option<&(native::Sources<'_>, native::Sources<'_>)>,
    proofs: &NativeProofs<'nodes>,
    plain_censuses: &mut NativeProofs<'nodes>,
    anchors: &[Anchor],
    rows: bool,
    pass: Pass,
    remaining: &mut usize,
) -> Result<()> {
    let Some((old_sources, new_sources)) = sources else {
        return Ok(());
    };
    let refine_existing = pass == Pass::RefineExisting;
    let enclosing = pass == Pass::EnclosingIntervals;
    let whole_intervals = pass == Pass::WholeIntervals || enclosing;
    let initial = *remaining;
    if pass == Pass::Standard
        && population(old, result.matching.scope.old, left, old_sources, remaining).is_some()
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
            &CutMaps::default(),
            false,
            false,
            false,
            remaining,
        )?;
        if let Some(search) = &mut result.source_cut_search {
            search.work = initial - *remaining;
        }
        return Ok(());
    }
    let mut aggregate = SourceCutSearch {
        convention: "matched-interval-native-fragment-cuts-v2".into(),
        exhaustive: !whole_intervals && anchors.len() >= 2,
        examined_fragments: 0,
        paired_boundaries: 0,
        work: 0,
        budget_at_entry: initial,
    };
    // A closed outer page region has no second enclosing anchor. Give this
    // independent population a turn before lexical refinement of intervals.
    if pass == Pass::Standard {
        pages::append(
            old, new, result, parent, limits, left, right, sources, anchors, true, remaining,
        )?;
        if let Some(search) = result.source_cut_search.take() {
            aggregate.exhaustive &= search.exhaustive;
            aggregate.examined_fragments += search.examined_fragments;
            aggregate.paired_boundaries += search.paired_boundaries;
        }
    }
    let mut deferred = Vec::new();
    let mut supported_intervals = BTreeSet::new();
    if enclosing {
        let count = result.text_scope_reviews.len();
        if spend(
            remaining,
            count.saturating_mul(count.saturating_add(1).ilog2() as usize + 1),
        )
        .is_none()
        {
            aggregate.exhaustive = false;
            aggregate.work = initial - *remaining;
            result.source_cut_search = Some(aggregate);
            return Ok(());
        }
        for review in &result.text_scope_reviews {
            // Preserve the finite extents of already exact comparisons.
            // Enclosing discovery is only for unresolved raw source views.
            if review.comparison.text_mask.is_some() {
                continue;
            }
            let Some(cuts) = &review.source_cuts else {
                continue;
            };
            if let SourceCutPopulation::MatchedInterval { boundaries, .. } = &cuts.population
                && cuts.edge_refinement.is_none()
                && matches!(cuts.entry.evidence, CutEvidence::AcceptedBoundary { .. })
                && matches!(cuts.exit.evidence, CutEvidence::AcceptedBoundary { .. })
            {
                supported_intervals.insert(*boundaries);
            }
        }
        // An enclosing view needs an established comparison of its inner
        // interval. Missing views retain the original fragment search.
        if supported_intervals.is_empty() {
            aggregate.work = initial - *remaining;
            result.source_cut_search = Some(aggregate);
            return Ok(());
        }
    }
    let mut new_anchors = BTreeSet::new();
    if whole_intervals || pass == Pass::Standard {
        let work = anchors.len().saturating_mul(
            anchors.len().saturating_add(1).ilog2() as usize
                + supported_intervals.len().saturating_add(1).ilog2() as usize
                + 1,
        );
        if spend(remaining, work).is_none() {
            aggregate.exhaustive = false;
            aggregate.work = initial - *remaining;
            result.source_cut_search = Some(aggregate);
            return Ok(());
        }
        new_anchors.extend(anchors.iter().map(|anchor| anchor.1));
    }
    let adjacent = |first: usize, last: usize| {
        let (a, b, _) = anchors[first];
        let (c, d, _) = anchors[last];
        a.0 == c.0 && b.0 == d.0 && a.1 + 1 == c.1 && b.1 + 1 == d.1
    };
    let ends = 0..anchors.len().saturating_sub(1);
    // An unchanged opening row can be an accepted match inside a paragraph.
    // After established intervals, retain enclosing views across a consecutive
    // chain of matched nodes present in the same order on both sides.
    let wider = ends
        .clone()
        .filter(|&last| {
            enclosing
                && !adjacent(last, last + 1)
                && supported_intervals.contains(&[anchors[last].2, anchors[last + 1].2])
        })
        .flat_map(|last| {
            (0..last)
                .rev()
                .take_while(move |&first| adjacent(first, first + 1))
                .map(move |first| (first, last + 1))
        });
    'intervals: for (first, last) in ends
        .filter(|_| !enclosing)
        .map(|first| (first, first + 1))
        .chain(wider)
    {
        let (a, b, entry) = &anchors[first];
        let (c, d, exit) = &anchors[last];
        if a.0 != c.0 || b.0 != d.0 || a.1 >= c.1 || b.1 >= d.1 {
            continue;
        }
        if c.1 == a.1 + 1 && d.1 == b.1 + 1 {
            continue;
        }
        if whole_intervals {
            // An accepted counterpart inside only one side prevents this whole
            // interval from having the same boundary order on both revisions.
            if spend(remaining, last - first).is_none() {
                aggregate.exhaustive = false;
                break;
            }
            if new_anchors
                .range((b.0, b.1 + 1)..*d)
                .take(last - first)
                .count()
                != last - first - 1
            {
                continue;
            }
            if spend(remaining, result.text_scope_reviews.len()).is_none() {
                aggregate.exhaustive = false;
                break;
            }
            if result
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_none() && review.boundaries == [*entry, *exit])
            {
                continue;
            }
        }
        if refine_existing {
            if spend(remaining, result.text_scope_reviews.len()).is_none() {
                aggregate.exhaustive = false;
                break;
            }
            if !result
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_none() && review.boundaries == [*entry, *exit])
            {
                continue;
            }
            if spend(
                remaining,
                (c.1 - a.1).saturating_add(d.1 - b.1).saturating_mul(2),
            )
            .is_none()
            {
                aggregate.exhaustive = false;
                break;
            }
            let has_outer_space = |nodes: &[&GraphNode]| {
                let mut tokens = nodes
                    .iter()
                    .filter_map(|node| match &node.content {
                        NodeContent::Text { view } => Some(view.tokens.iter()),
                        _ => None,
                    })
                    .flatten();
                matches!(tokens.next(), Some(ComparableToken::Scalar(' ')))
                    || matches!(tokens.next_back(), Some(ComparableToken::Scalar(' ')))
            };
            // This pass refines the literal-space edges of a retained review.
            // Without an outer space there is no finer extent to discover;
            // preserve the original review and the budget for other searches.
            if !has_outer_space(&left[a.0][a.1 + 1..c.1])
                && !has_outer_space(&right[b.0][b.1 + 1..d.1])
            {
                continue;
            }
        }
        let whole_fallback = if pass == Pass::Standard {
            if spend(remaining, last - first).is_none() {
                aggregate.exhaustive = false;
                break;
            }
            new_anchors
                .range((b.0, b.1 + 1)..*d)
                .take(last - first)
                .count()
                == last - first - 1
        } else {
            false
        };
        let work = (c.1 - a.1 + 1)
            .saturating_add(d.1 - b.1 + 1)
            .saturating_mul(4);
        if spend(remaining, work).is_none() {
            aggregate.exhaustive = false;
            break;
        }
        let mut left = [left[a.0][a.1..=c.1].to_vec()];
        let mut right = [right[b.0][b.1..=d.1].to_vec()];
        if refine_existing {
            let Some(review) = result.text_scope_reviews.iter().find(|review| {
                review.source_cuts.is_none() && review.boundaries == [*entry, *exit]
            }) else {
                continue;
            };
            for (path, sources, expected) in [
                (&mut left[0], old_sources, &review.old_sources),
                (&mut right[0], new_sources, &review.new_sources),
            ] {
                let work = path.iter().map(|node| node.sources.len()).sum::<usize>();
                if spend(remaining, work.saturating_mul(2)).is_none() {
                    aggregate.exhaustive = false;
                    break 'intervals;
                }
                let matches = |nodes: &[&GraphNode]| {
                    nodes[1..nodes.len() - 1]
                        .iter()
                        .flat_map(|node| &node.sources)
                        .eq(expected)
                };
                if matches(path) {
                    continue;
                }
                let Some(lane) = sources.boundary_lane(path, remaining) else {
                    continue 'intervals;
                };
                if !matches(&lane) {
                    continue 'intervals;
                }
                *path = lane;
            }
        }
        if pass == Pass::Paint {
            let (Some(old_hint), Some(new_hint)) = (
                old_sources.has_ordered_row(&left[0], remaining),
                new_sources.has_ordered_row(&right[0], remaining),
            ) else {
                aggregate.exhaustive = false;
                break;
            };
            if !old_hint && !new_hint {
                continue;
            }
        }
        match same_raw_text(&left[0], &right[0], remaining) {
            Some(true) => continue,
            None => {
                aggregate.exhaustive = false;
                continue;
            }
            Some(false) => {}
        }
        let reusable = if pass == Pass::Standard && !rows {
            &*plain_censuses
        } else {
            proofs
        };
        let cached =
            if (refine_existing || (pass == Pass::Standard && !rows)) && !reusable.is_empty() {
                let work = reusable.len().saturating_add(1).ilog2() as usize
                    + left[0].len()
                    + right[0].len()
                    + 1;
                if spend(remaining, work).is_none() {
                    aggregate.exhaustive = false;
                    break;
                }
                reusable.get(&[*entry, *exit]).filter(|proof| {
                    same_nodes(&proof.old, &left[0]) && same_nodes(&proof.new, &right[0])
                })
            } else {
                None
            };
        let cached = if cached.is_none() && pass == Pass::Standard && !rows && !proofs.is_empty() {
            let work = proofs.len().saturating_add(1).ilog2() as usize
                + left[0].len()
                + right[0].len()
                + 1;
            if spend(remaining, work).is_none() {
                aggregate.exhaustive = false;
                break;
            }
            proofs.get(&[*entry, *exit]).filter(|proof| {
                same_nodes(&proof.old, &left[0])
                    && same_nodes(&proof.new, &right[0])
                    && old_sources.boundary_roundoff(&left[0], remaining) == Some(false)
                    && new_sources.boundary_roundoff(&right[0], remaining) == Some(false)
            })
        } else {
            cached
        };
        // A full plain census already checked boundary roundoff. A legacy
        // closure needs that extra check before supporting raw projection.
        // Both require the same ordered borrowed source population.
        let census = cached.map_or_else(
            || {
                Some((
                    old_sources.census(
                        old,
                        result.matching.scope.old,
                        &left[0],
                        remaining,
                        rows,
                        pass != Pass::RefineExisting,
                    )?,
                    new_sources.census(
                        new,
                        result.matching.scope.new,
                        &right[0],
                        remaining,
                        rows,
                        pass != Pass::RefineExisting,
                    )?,
                ))
            },
            |proof| {
                let closure = |bounded| {
                    (
                        if bounded {
                            native::Closure::BoundedPaint
                        } else {
                            native::Closure::WholePage
                        },
                        Vec::new(),
                        None,
                    )
                };
                Some((
                    closure(proof.bounded_paint[0]),
                    closure(proof.bounded_paint[1]),
                ))
            },
        );
        let Some(((old_closure, old_padding, old_rows), (new_closure, new_padding, new_rows))) =
            census
        else {
            aggregate.exhaustive = false;
            continue;
        };
        // Keep successful whole-pass censuses separate from legacy proofs,
        // which later comparison passes can replace. This representation cannot
        // carry padding, row-order or segmented-region certificates.
        if pass == Pass::WholeIntervals
            && !rows
            && old_padding.is_empty()
            && new_padding.is_empty()
            && old_rows.is_none()
            && new_rows.is_none()
            && matches!(
                &old_closure,
                native::Closure::WholePage | native::Closure::BoundedPaint
            )
            && matches!(
                &new_closure,
                native::Closure::WholePage | native::Closure::BoundedPaint
            )
        {
            let work = left[0]
                .len()
                .saturating_add(right[0].len())
                .saturating_add(plain_censuses.len().saturating_add(1).ilog2() as usize + 1);
            if spend(remaining, work).is_some() {
                plain_censuses.insert(
                    [*entry, *exit],
                    super::NativeClosureProof {
                        old: left[0].clone(),
                        new: right[0].clone(),
                        bounded_paint: [old_closure.bounded_paint(), new_closure.bounded_paint()],
                    },
                );
            }
        }
        if old_rows.is_some() && new_rows.is_some() && old_rows != new_rows {
            // Mixed conventions require a separate certificate on each side.
            aggregate.exhaustive = false;
            continue;
        }
        if pass == Pass::Paint
            && old_rows != Some(native::RowOrder::Paint)
            && new_rows != Some(native::RowOrder::Paint)
        {
            continue;
        }
        let (old_chain, new_chain) = (old_closure.chain(), new_closure.chain());
        let native_regions =
            (old_chain.is_some() || new_chain.is_some()).then_some(Box::new(NativeRegionChains {
                old: old_chain,
                new: new_chain,
            }));
        let anchors = [
            ((0, 0), (0, 0), *entry),
            ((0, left[0].len() - 1), (0, right[0].len() - 1), *exit),
        ];
        let population = SourceCutPopulation::MatchedInterval {
            boundaries: [*entry, *exit],
            old: left[0].iter().map(|node| node.id).collect(),
            new: right[0].iter().map(|node| node.id).collect(),
            native_regions,
            boundary_padding: (!old_padding.is_empty() || !new_padding.is_empty()).then_some(
                Box::new(SourceCutBoundaryPadding {
                    convention: "optional-clipped-boundary-padding-v1".into(),
                    old: old_padding.clone(),
                    new: new_padding.clone(),
                }),
            ),
            row_order: old_rows.or(new_rows).map(|order| {
                let endpoints = |nodes: &[&GraphNode]| {
                    [0, nodes.len() - 1].map(|index| SourceCutRowEndpoint {
                        node: nodes[index].id,
                        sources: nodes[index].sources.clone(),
                    })
                };
                Box::new(SourceCutRowOrder {
                    convention: order.convention().into(),
                    old: old_rows.map(|_| endpoints(&left[0])),
                    new: new_rows.map(|_| endpoints(&right[0])),
                })
            }),
        };
        let paint_frame =
            old_rows == Some(native::RowOrder::Paint) || new_rows == Some(native::RowOrder::Paint);
        let deferred_population = (paint_frame && pass == Pass::Paint).then(|| population.clone());
        let project = |nodes: &[&GraphNode],
                       sources: &native::Sources<'_>,
                       padding: &[SourceRef],
                       remaining: &mut usize| {
            nodes
                .iter()
                .map(|node| {
                    sources.project_census(
                        node,
                        padding,
                        old_rows == Some(native::RowOrder::Paint)
                            || new_rows == Some(native::RowOrder::Paint),
                        remaining,
                    )
                })
                .collect::<Option<Vec<_>>>()
        };
        let (Some(old_projected), Some(new_projected)) = (
            project(&left[0], old_sources, &old_padding, remaining),
            project(&right[0], new_sources, &new_padding, remaining),
        ) else {
            aggregate.exhaustive = false;
            continue;
        };
        let mut maps = CutMaps::default();
        let old_nodes: Vec<_> = old_projected
            .into_iter()
            .map(|(node, map)| {
                if let Some(map) = map {
                    maps.old.insert(node.id, map);
                }
                node
            })
            .collect();
        let new_nodes: Vec<_> = new_projected
            .into_iter()
            .map(|(node, map)| {
                if let Some(map) = map {
                    maps.new.insert(node.id, map);
                }
                node
            })
            .collect();
        let left = vec![old_nodes.iter().collect()];
        let right = vec![new_nodes.iter().collect()];
        let existing_population = (pass == Pass::Standard).then(|| population.clone());
        compare_population(
            old,
            new,
            result,
            parent,
            limits,
            &left,
            &right,
            sources,
            &anchors,
            population,
            &maps,
            refine_existing,
            whole_intervals || (paint_frame && !refine_existing),
            whole_fallback,
            remaining,
        )?;
        if let Some(search) = result.source_cut_search.take() {
            aggregate.exhaustive &= search.exhaustive;
            aggregate.examined_fragments += search.examined_fragments;
            aggregate.paired_boundaries += search.paired_boundaries;
        }
        // Reuse this frame's charged census and projection for an existing
        // whole-node review before moving to another frame. Reacquiring the
        // same proof in a later pass can exhaust the shared work budget.
        if let Some(population) = existing_population {
            if spend(remaining, result.text_scope_reviews.len()).is_none() {
                aggregate.exhaustive = false;
                break;
            }
            if result
                .text_scope_reviews
                .iter()
                .any(|review| review.source_cuts.is_none() && review.boundaries == [*entry, *exit])
            {
                compare_population(
                    old, new, result, parent, limits, &left, &right, sources, &anchors, population,
                    &maps, true, false, false, remaining,
                )?;
                if let Some(search) = result.source_cut_search.take() {
                    aggregate.exhaustive &= search.exhaustive;
                    aggregate.examined_fragments += search.examined_fragments;
                    aggregate.paired_boundaries += search.paired_boundaries;
                }
            }
        }
        if let Some(population) = deferred_population {
            deferred.push((old_nodes, new_nodes, anchors, population, maps));
        }
    }
    // Every eligible paint interval gets its whole comparison before any one
    // such interval can exhaust the budget with lexical fragment discovery.
    // Keep the already charged projections and enclosing closure certificates.
    for (old_nodes, new_nodes, anchors, population, maps) in deferred {
        let left = vec![old_nodes.iter().collect()];
        let right = vec![new_nodes.iter().collect()];
        compare_population(
            old, new, result, parent, limits, &left, &right, sources, &anchors, population, &maps,
            false, false, false, remaining,
        )?;
        if let Some(search) = result.source_cut_search.take() {
            aggregate.exhaustive &= search.exhaustive;
            aggregate.examined_fragments += search.examined_fragments;
            aggregate.paired_boundaries += search.paired_boundaries;
        }
    }
    if pass == Pass::Standard {
        pages::append(
            old, new, result, parent, limits, left, right, sources, anchors, false, remaining,
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
    maps: &CutMaps,
    refine_existing: bool,
    whole_only: bool,
    whole_fallback: bool,
    remaining: &mut usize,
) -> Result<()> {
    let Some((old_sources, new_sources)) = sources else {
        return Ok(());
    };
    let paint_boundaries = matches!(&population,
        SourceCutPopulation::MatchedInterval { row_order: Some(order), .. }
            if order.convention == native::RowOrder::Paint.convention());
    let initial = *remaining;
    let mut search = SourceCutSearch {
        convention: "unique-native-fragment-cuts-v2".into(),
        exhaustive: false,
        examined_fragments: 0,
        paired_boundaries: 0,
        work: 0,
        budget_at_entry: initial,
    };
    let mut excluded = [BTreeSet::new(), BTreeSet::new()];
    if let SourceCutPopulation::AnchoredPage {
        external_boundaries,
        ..
    } = &population
    {
        for &index in external_boundaries {
            let proposal = &result.candidates.proposals[index];
            if spend(
                remaining,
                proposal.old.len().saturating_add(proposal.new.len()),
            )
            .is_none()
            {
                return Ok(());
            }
            excluded[0].extend(&proposal.old);
            excluded[1].extend(&proposal.new);
        }
    }
    if let SourceCutPopulation::AnchoredPage {
        edge: Some(edge), ..
    } = &population
        && let Some(&(a, b, _)) = anchors.first()
    {
        // These fragments would be removed by the outer-edge filter below.
        // Prune only candidate generation; discover still counts occurrences
        // against every token on both complete page paths.
        for ((runs, anchor), excluded) in [(left, a), (right, b)].into_iter().zip(&mut excluded) {
            if spend(remaining, runs.iter().map(Vec::len).sum()).is_none() {
                return Ok(());
            }
            for (run, nodes) in runs.iter().enumerate() {
                for (index, node) in nodes.iter().enumerate() {
                    let outside = match edge {
                        SourceCutPageEdge::After => (run, index) <= anchor,
                        SourceCutPageEdge::Before => (run, index) >= anchor,
                    };
                    if outside {
                        excluded.insert(node.id);
                    }
                }
            }
        }
    }
    let outcome = discover(
        left,
        right,
        old_sources,
        new_sources,
        anchors,
        !matches!(population, SourceCutPopulation::CompletePage),
        maps,
        refine_existing || whole_only,
        paint_boundaries,
        &excluded,
        &mut search,
        remaining,
    );
    search.work = initial - *remaining;
    let Some(mut boundaries) = outcome else {
        result.source_cut_search = Some(search);
        return Ok(());
    };
    if let SourceCutPopulation::AnchoredPage {
        edge: Some(edge), ..
    } = &population
    {
        let Some(&(a, b, _)) = anchors.first() else {
            return Ok(());
        };
        let endpoint = |runs: &[Vec<&GraphNode>], at: (usize, usize)| {
            let NodeContent::Text { view } = &runs[at.0][at.1].content else {
                return None;
            };
            Some((
                at.0,
                at.1,
                if *edge == SourceCutPageEdge::After {
                    view.tokens.len()
                } else {
                    0
                },
            ))
        };
        let (Some(a), Some(b)) = (endpoint(left, a), endpoint(right, b)) else {
            return Ok(());
        };
        if spend(remaining, boundaries.len()).is_none() {
            return Ok(());
        }
        // Keep the complete page occurrence census, but never compare content
        // between accepted boundaries through this outer-edge fallback.
        boundaries.retain(|cut| match edge {
            SourceCutPageEdge::After => cut.old >= a && cut.new >= b,
            SourceCutPageEdge::Before => cut.old <= a && cut.new <= b,
        });
    }
    // If lexical discovery found no finer boundary, reuse the closed frame
    // and its accepted endpoints. The caller checked that no accepted anchor
    // crosses the enclosing interval; no second census or search is needed.
    if whole_fallback && spend(remaining, boundaries.len()).is_none() {
        search.work = initial - *remaining;
        result.source_cut_search = Some(search);
        return Ok(());
    }
    let whole_fallback = whole_fallback
        && boundaries.iter().all(|cut| {
            matches!(
                cut.certificate.evidence,
                CutEvidence::AcceptedBoundary { .. }
            )
        });
    let sort_work = boundaries
        .len()
        .saturating_mul(boundaries.len().saturating_add(1).ilog2() as usize + 1);
    if spend(remaining, sort_work).is_none() {
        search.work = initial - *remaining;
        result.source_cut_search = Some(search);
        return Ok(());
    }
    search.exhaustive = !refine_existing && !whole_only;
    boundaries.sort_by_key(|boundary| boundary.old);
    boundaries.dedup_by(|a, b| a.old == b.old && a.new == b.new);
    search.work = initial - *remaining;
    search.paired_boundaries = boundaries.len();
    result.source_cut_search = Some(search);
    // Refinements are queued after every original interval so a finer view
    // cannot consume the work needed to establish its enclosing comparisons.
    let mut agenda: std::collections::VecDeque<_> = boundaries
        .windows(2)
        .map(|pair| (pair[0].clone(), pair[1].clone(), None, None, false))
        .collect();
    let mut spanning = std::collections::VecDeque::new();
    if matches!(population, SourceCutPopulation::AnchoredPage { .. }) {
        if spend(remaining, boundaries.len()).is_none() {
            return Ok(());
        }
        let mut cuts: Vec<_> = boundaries.iter().collect();
        if let SourceCutPopulation::AnchoredPage {
            edge: Some(edge), ..
        } = &population
        {
            if spend(remaining, sort_work).is_none() {
                return Ok(());
            }
            let endpoint = |runs: &[Vec<&GraphNode>], position: Position| {
                let NodeContent::Text { view } = &runs[position.0][position.1].content else {
                    return false;
                };
                position.2
                    == if *edge == SourceCutPageEdge::After {
                        view.tokens.len()
                    } else {
                        0
                    }
            };
            // Try ranges reaching a retained outer node edge before truncating
            // them at an interior equal row. Node layout only orders the work;
            // every cut still needs the same literal correspondence proof.
            cuts.sort_by(|a, b| {
                let rank = |cut: &&Boundary| !(endpoint(left, cut.old) && endpoint(right, cut.new));
                rank(a).cmp(&rank(b)).then_with(|| match edge {
                    SourceCutPageEdge::After => a.old.cmp(&b.old),
                    SourceCutPageEdge::Before => b.old.cmp(&a.old),
                })
            });
        }
        // Interior unchanged rows may divide a changed paragraph into smaller
        // comparisons. Keep the views from an accepted page anchor to every
        // checked cut, retaining the wider extent under the same certificate.
        for anchor in boundaries.iter().rev() {
            if !matches!(
                anchor.certificate.evidence,
                CutEvidence::AcceptedBoundary { .. }
            ) {
                continue;
            }
            for &cut in &cuts {
                if spend(remaining, 1).is_none() {
                    break;
                }
                let (entry, exit) = if anchor.old < cut.old && anchor.new <= cut.new {
                    (anchor, cut)
                } else if cut.old < anchor.old && cut.new <= anchor.new {
                    (cut, anchor)
                } else {
                    continue;
                };
                let copies = entry
                    .old_sources
                    .len()
                    .saturating_add(entry.new_sources.len())
                    .saturating_add(exit.old_sources.len())
                    .saturating_add(exit.new_sources.len());
                if spend(remaining, copies.saturating_mul(2)).is_none() {
                    break;
                }
                spanning.push_back((entry.clone(), exit.clone(), None, None, false));
            }
        }
    }
    if paint_boundaries && !whole_only {
        // This interval has no legacy whole-node closure. Retain its complete
        // raw comparison before lexical cuts and reversible edge refinements.
        let (NodeContent::Text { view: a }, NodeContent::Text { view: b }) =
            (&left[0][0].content, &right[0][0].content)
        else {
            return Ok(());
        };
        let entry = boundaries.iter().find(|cut| {
            cut.old == (0, 0, a.tokens.len())
                && cut.new == (0, 0, b.tokens.len())
                && matches!(
                    cut.certificate.evidence,
                    CutEvidence::AcceptedBoundary { .. }
                )
        });
        let exit = boundaries.iter().find(|cut| {
            cut.old == (0, left[0].len() - 1, 0)
                && cut.new == (0, right[0].len() - 1, 0)
                && matches!(
                    cut.certificate.evidence,
                    CutEvidence::AcceptedBoundary { .. }
                )
        });
        if let (Some(entry), Some(exit)) = (entry, exit) {
            agenda.push_front((entry.clone(), exit.clone(), None, None, true));
            // A separately painted label may precede a lexical prefix in the
            // interior. Keep anchor-to-cut views so that merging a later
            // boundary into a body node does not discard that label's extent.
            // Established adjacent views and their refinements run first.
            for cut in &boundaries {
                if cut.old > entry.old
                    && cut.old < exit.old
                    && cut.new >= entry.new
                    && cut.new <= exit.new
                {
                    spanning.push_back((entry.clone(), cut.clone(), None, None, false));
                    spanning.push_back((cut.clone(), exit.clone(), None, None, false));
                }
            }
        }
    }
    if matches!(population, SourceCutPopulation::AnchoredPage { .. }) {
        // The page fallback follows established interval comparisons. Give its
        // anchor-spanning views priority over unrelated adjacent fragments.
        spanning.append(&mut agenda);
        std::mem::swap(&mut agenda, &mut spanning);
    }
    while let Some((entry, exit, refinement, pending_parent, whole_frame)) =
        agenda.pop_front().or_else(|| spanning.pop_front())
    {
        if spend(remaining, boundaries.len()).is_none() {
            break;
        }
        if entry.new > exit.new
            || entry.old.0 != exit.old.0
            || entry.new.0 != exit.new.0
            || (!refine_existing
                && !whole_only
                && !(whole_fallback && entry.old.1 < exit.old.1 && entry.new.1 < exit.new.1)
                && !paint_boundaries
                && !matches!(population, SourceCutPopulation::AnchoredPage { .. })
                && matches!(
                    (&entry.certificate.evidence, &exit.certificate.evidence),
                    (
                        CutEvidence::AcceptedBoundary { .. },
                        CutEvidence::AcceptedBoundary { .. }
                    )
                ))
        {
            continue;
        }
        if matches!(population, SourceCutPopulation::AnchoredPage { .. }) {
            let empty = |runs: &[Vec<&GraphNode>], first: Position, last: Position| {
                if first.1 == last.1 {
                    return first.2 == last.2;
                }
                let NodeContent::Text { view } = &runs[first.0][first.1].content else {
                    return false;
                };
                last.1 == first.1 + 1 && first.2 == view.tokens.len() && last.2 == 0
            };
            // Neighboring node coordinates can name the same empty interval.
            // Neither side needs a source projection in that case.
            if empty(left, entry.old, exit.old) && empty(right, entry.new, exit.new) {
                continue;
            }
            let count = (exit.old.1 - entry.old.1 + 1).saturating_add(exit.new.1 - entry.new.1 + 1);
            if spend(remaining, count).is_none() {
                break;
            }
            let crosses = |runs: &[Vec<&GraphNode>],
                           first: Position,
                           last: Position,
                           excluded: &BTreeSet<NodeId>| {
                (first.1..=last.1).any(|index| {
                    let node = runs[first.0][index];
                    let NodeContent::Text { view } = &node.content else {
                        return true;
                    };
                    let start = if index == first.1 { first.2 } else { 0 };
                    let end = if index == last.1 {
                        last.2
                    } else {
                        view.tokens.len()
                    };
                    start < end && excluded.contains(&node.id)
                })
            };
            if crosses(left, entry.old, exit.old, &excluded[0])
                || crosses(right, entry.new, exit.new, &excluded[1])
            {
                continue;
            }
        }
        if paint_boundaries {
            let interior_cut = |runs: &[Vec<&GraphNode>], cut: Position| {
                let NodeContent::Text { view } = &runs[0][0].content else {
                    return false;
                };
                cut >= (0, 0, view.tokens.len()) && cut <= (0, runs[0].len() - 1, 0)
            };
            if !interior_cut(left, entry.old)
                || !interior_cut(left, exit.old)
                || !interior_cut(right, entry.new)
                || !interior_cut(right, exit.new)
            {
                continue;
            }
        }
        // Crossing paired cuts mean this range lacks a consistent ordering.
        if !whole_frame
            && boundaries.iter().any(|other| {
                other.new > entry.new
                    && other.new < exit.new
                    && (other.old <= entry.old || other.old >= exit.old)
            })
        {
            continue;
        }
        let sliced;
        let (a, b): (Vec<_>, Vec<_>) = if refine_existing && refinement.is_none() {
            let (Some(a), Some(b)) = (
                whole_interior(left, entry.old, exit.old),
                whole_interior(right, entry.new, exit.new),
            ) else {
                continue;
            };
            // The enclosing views were already source-validated. Whole-node
            // endpoints need only copy source references, not slice and clone
            // every token before the actual content-edge comparison.
            let work = a
                .iter()
                .chain(b)
                .map(|node| node.sources.len())
                .fold(0usize, usize::saturating_add);
            if spend(remaining, work).is_none() {
                break;
            }
            (a.to_vec(), b.to_vec())
        } else {
            let (Some(a), Some(b)) = (
                interior(left, entry.old, exit.old, remaining),
                interior(right, entry.new, exit.new, remaining),
            ) else {
                continue;
            };
            sliced = (a, b);
            (
                sliced.0.iter().map(std::borrow::Cow::as_ref).collect(),
                sliced.1.iter().map(std::borrow::Cow::as_ref).collect(),
            )
        };
        let Some(kind) = a.first().or_else(|| b.first()).map(|node| node.kind) else {
            continue;
        };
        if a.iter().any(|node| excluded[0].contains(&node.id))
            || b.iter().any(|node| excluded[1].contains(&node.id))
        {
            continue;
        }
        if a.len() > limits.matching.max_group_nodes
            || b.len() > limits.matching.max_group_nodes
            || a.iter().chain(&b).any(|node| node.kind != kind)
        {
            continue;
        }
        // Contiguous subpaths inherit the enclosing population's checked band.
        // Legal slices retain source order; census-only uncertain padding is
        // explicitly excluded below before any content claim is admitted.
        let old_extent: Vec<_> = a
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        let new_extent: Vec<_> = b
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        if let SourceCutPopulation::MatchedInterval {
            boundary_padding: Some(padding),
            ..
        } = &population
        {
            if spend(
                remaining,
                old_extent
                    .len()
                    .saturating_mul(padding.old.len())
                    .saturating_add(new_extent.len().saturating_mul(padding.new.len())),
            )
            .is_none()
            {
                break;
            }
            if old_extent.iter().any(|source| padding.old.contains(source))
                || new_extent.iter().any(|source| padding.new.contains(source))
            {
                continue;
            }
        }
        let duplicate = result
            .text_scope_reviews
            .iter()
            .find(|review| review.old_sources == old_extent && review.new_sources == new_extent);
        let existing_parent = refine_existing && refinement.is_none();
        if existing_parent {
            if duplicate.is_none_or(|review| review.source_cuts.is_some())
                || result.text_scope_reviews.iter().any(|review| {
                    review.source_cuts.is_some()
                        && review.old_sources == old_extent
                        && review.new_sources == new_extent
                })
            {
                continue;
            }
        } else if duplicate.is_some() {
            continue;
        }
        let refined_parent = existing_parent
            .then(|| edges::refine(left, right, &entry, &exit, maps, remaining))
            .flatten();
        if existing_parent && refined_parent.is_none() {
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
        let review = TextScopeReview {
            convention: "closed-native-source-cut-interval-v1".into(),
            boundaries: Vec::new(),
            source_cuts: Some(SourceCutRange {
                convention: "unique-native-fragment-cuts-v2".into(),
                projection: if !maps.old.is_empty() || !maps.new.is_empty() {
                    "retained-glyph-whitespace-expansion-v1"
                } else if matches!(
                    &population,
                    SourceCutPopulation::MatchedInterval {
                        boundary_padding: Some(_),
                        ..
                    }
                ) {
                    "retained-glyph-boundary-padding-v1"
                } else {
                    "retained-glyph-ligatures-spacing-v1"
                }
                .into(),
                population: population.clone(),
                entry: entry.certificate.clone(),
                exit: exit.certificate.clone(),
                edge_refinement: refinement.clone(),
            }),
            old_sources: old_extent,
            presence: interval_presence(a.is_empty(), b.is_empty()),
            native_regions: match &population {
                SourceCutPopulation::MatchedInterval { native_regions, .. } => {
                    native_regions.as_deref().cloned()
                }
                SourceCutPopulation::CompletePage | SourceCutPopulation::AnchoredPage { .. } => {
                    None
                }
            },
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
        };
        if let Some((entry, exit, refinement)) = refined_parent {
            // The extra raw parent is useful only when its finer comparison
            // proves a change. Space-only outer differences keep their original
            // review without an unused duplicate certificate.
            agenda.push_back((entry, exit, refinement, Some(review), whole_frame));
            continue;
        }
        if let Some(parent) = pending_parent {
            result.text_scope_reviews.push(parent);
        }
        result.text_scope_reviews.push(review);
        if refinement.is_none()
            && let Some(refined) = edges::refine(left, right, &entry, &exit, maps, remaining)
        {
            let next = (refined.0, refined.1, refined.2, None, whole_frame);
            if matches!(population, SourceCutPopulation::AnchoredPage { .. }) {
                agenda.push_front(next);
            } else {
                agenda.push_back(next);
            }
        }
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
    maps: &CutMaps,
    anchors_only: bool,
    paint_order: bool,
    excluded: &[BTreeSet<NodeId>; 2],
    search: &mut SourceCutSearch,
    remaining: &mut usize,
) -> Option<Vec<Boundary>> {
    // The caller establishes a complete page or a closed matched interval.
    // Copies outside an interval cannot establish semantic identity; this
    // finite correspondence is explicitly conditional on its outer matches.
    let tokens = |runs: &[Vec<&GraphNode>], remaining: &mut usize| {
        let mut tokens = Vec::new();
        let mut optional = Vec::new();
        let mut mandatory = true;
        for node in runs.iter().flatten() {
            let NodeContent::Text { view } = &node.content else {
                return None;
            };
            spend(remaining, view.tokens.len())?;
            tokens.extend_from_slice(&view.tokens);
            for skippable in view.optional_tokens()? {
                mandatory &= !skippable;
                optional.push(skippable);
            }
        }
        Some((tokens, optional, mandatory))
    };
    let (old_tokens, old_optional, old_mandatory) = tokens(left, remaining)?;
    let (new_tokens, new_optional, new_mandatory) = tokens(right, remaining)?;
    let mut boundaries = Vec::new();
    let mut old_anchors = excluded[0].clone();
    let mut new_anchors = excluded[1].clone();
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
                        token_boundary: original_boundary(&maps.old, old_node.id, ap)?,
                    },
                    new: SourceCut {
                        node: new_node.id,
                        token_boundary: original_boundary(&maps.new, new_node.id, bp)?,
                    },
                    evidence: CutEvidence::AcceptedBoundary { proposal },
                },
                old_sources: old_node.sources.clone(),
                new_sources: new_node.sources.clone(),
            });
        }
    }
    if anchors_only {
        return Some(boundaries);
    }
    let a = rows(
        left,
        old_sources,
        word_edges,
        paint_order,
        &old_anchors,
        remaining,
    )?;
    let b = rows(
        right,
        new_sources,
        word_edges,
        paint_order,
        &new_anchors,
        remaining,
    )?;
    search.examined_fragments = a.len() + b.len();
    // Equal fragments must have equal token counts. Preserve original row
    // order within each bucket and check literal equality after lookup.
    let index_work = b.len().saturating_add(1).ilog2() as usize + 1;
    spend(
        remaining,
        b.len().saturating_mul(index_work).saturating_mul(4),
    )?;
    let mut by_length: BTreeMap<usize, Vec<_>> = BTreeMap::new();
    for row in &b {
        by_length.entry(row.tokens.len()).or_default().push(row);
    }
    for a in &a {
        spend(remaining, index_work.saturating_mul(4))?;
        let Some(candidates) = by_length.get(&a.tokens.len()) else {
            continue;
        };
        let mut old_unique = None;
        for b in candidates {
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
                    let value = unique(&old_tokens, &old_optional, old_mandatory, a, remaining)?;
                    old_unique = Some(value);
                    value
                }
            };
            if !old_unique || !unique(&new_tokens, &new_optional, new_mandatory, b, remaining)? {
                continue;
            }
            let (Some(af), Some(bf)) = (fragment(a, &maps.old), fragment(b, &maps.new)) else {
                continue;
            };
            for (ap, bp, allowed) in [
                (a.range.start, b.range.start, a.before && b.before),
                (a.range.end, b.range.end, a.after && b.after),
            ] {
                if !allowed {
                    continue;
                }
                let (Some(old_boundary), Some(new_boundary)) = (
                    original_boundary(&maps.old, a.node.id, ap),
                    original_boundary(&maps.new, b.node.id, bp),
                ) else {
                    continue;
                };
                boundaries.push(Boundary {
                    old: (a.position.0, a.position.1, ap),
                    new: (b.position.0, b.position.1, bp),
                    certificate: CutCorrespondence {
                        old: SourceCut {
                            node: a.node.id,
                            token_boundary: old_boundary,
                        },
                        new: SourceCut {
                            node: b.node.id,
                            token_boundary: new_boundary,
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

    #[test]
    fn cached_closure_requires_the_same_ordered_borrowed_nodes() {
        let first = node();
        let mut last = node();
        last.id = NodeId(99);
        assert!(same_nodes(&[&first, &last], &[&first, &last]));
        assert!(!same_nodes(&[&first, &last], &[&last, &first]));
        assert!(!same_nodes(&[&first, &last], &[&first]));
        let mut projected = first.clone();
        assert!(!same_nodes(&[&first], &[&projected]));
        projected.sources.truncate(1);
        assert_eq!(projected.id, first.id);
        assert!(!same_nodes(&[&first], &[&projected]));
    }

    #[test]
    fn mandatory_census_has_linear_work_for_repeated_prefixes() {
        let mut tokens = vec![ComparableToken::Scalar('a'); 4096];
        tokens.push(ComparableToken::Scalar('b'));
        let mut needle = vec![ComparableToken::Scalar('a'); 128];
        needle.push(ComparableToken::Scalar('b'));
        let mut remaining = (tokens.len() + needle.len()) * 4;
        assert_eq!(
            unique_mandatory(&tokens, &needle, &mut remaining),
            Some(true)
        );
        assert!(remaining > 0);
        needle.pop();
        assert_eq!(
            unique_mandatory(&tokens, &needle, &mut remaining),
            Some(false)
        );
        assert_eq!(unique_mandatory(&tokens, &[], &mut remaining), None);
    }

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
    fn empty_cut_endpoints_do_not_consume_source_projection_work() {
        let body = node();
        let mut boundary = node();
        boundary.id = NodeId(8);
        boundary.sources = (10..1034).map(source).collect();
        boundary.content = NodeContent::Text {
            view: TextView {
                tokens: vec![ComparableToken::Scalar('a'); 1024],
                origins: boundary
                    .sources
                    .iter()
                    .map(|source| vec![*source])
                    .collect(),
                source_backed: vec![true; 1024],
                normalization: TextNormalization::Exact,
            },
        };
        let runs = vec![vec![&boundary, &body, &boundary]];
        let result = interior(&runs, (0, 0, 1024), (0, 2, 0), &mut 24)
            .expect("only the interior body needs a source projection");
        assert_eq!(
            result.iter().map(|node| node.as_ref()).collect::<Vec<_>>(),
            vec![&body]
        );
        assert!(interior(&runs, (0, 0, 1024), (0, 2, 0), &mut 8).is_none());
        assert!(interior(&runs, (0, 0, 1023), (0, 2, 0), &mut 24).is_none());
        assert_eq!(body, node());
    }

    #[test]
    fn complete_exact_cut_members_reuse_the_validated_view_with_a_bounded_extent_copy() {
        let mut original = node();
        let NodeContent::Text { view } = &mut original.content else {
            unreachable!()
        };
        view.normalization = TextNormalization::Exact;
        let runs = vec![vec![&original]];
        let whole = interior(&runs, (0, 0, 0), (0, 0, 4), &mut 3)
            .expect("a complete exact member needs no token copy");
        assert!(std::ptr::eq(whole[0].as_ref(), &original));
        assert_eq!(
            whole[0].as_ref(),
            &slice(&original, 0, 4, &mut 100).expect("complete exact member has a valid slice")
        );
        assert!(interior(&runs, (0, 0, 0), (0, 0, 4), &mut 0).is_none());
        assert!(interior(&runs, (0, 0, 0), (0, 0, 2), &mut 3).is_none());
        let partial = interior(&runs, (0, 0, 0), (0, 0, 2), &mut 17)
            .expect("partial members still validate and copy their source projection");
        assert!(!std::ptr::eq(partial[0].as_ref(), &original));
        assert_eq!(partial[0].sources, [source(1)]);
    }

    #[test]
    fn cuts_preserve_shared_glyphs_and_nonowning_separator_context() {
        let original = node();
        assert!(slice(&original, 0, 1, &mut 100).is_none());
        assert!(slice(&original, 1, 4, &mut 100).is_none());
        let left = slice(&original, 0, 2, &mut 100).expect("complete ligature projection");
        let right = slice(&original, 2, 4, &mut 100).expect("separator references are context");
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
    fn optional_spacing_census_has_bounded_frontier_work() {
        let node = node();
        let text = format!("{}b", "a ".repeat(128));
        let pattern = format!("{}b", "a".repeat(16));
        let tokens: Vec<_> = text.chars().map(ComparableToken::Scalar).collect();
        let optional: Vec<_> = text.chars().map(|scalar| scalar == ' ').collect();
        let row = Row {
            position: (0, 0),
            node: &node,
            range: 0..pattern.len(),
            tokens: pattern.chars().map(ComparableToken::Scalar).collect(),
            before: true,
            after: true,
        };
        assert_eq!(
            unique(&tokens, &optional, false, &row, &mut 90_000),
            Some(true)
        );
        assert_eq!(unique(&tokens, &optional, false, &row, &mut 100), None);
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
                            unique(
                                &tokens,
                                &optional,
                                !optional.contains(&true),
                                &row,
                                &mut 100_000
                            ),
                            Some(occurrences.len() == 1),
                            "length {length}, spelling {spelling}, choices {choices}, pattern {pattern}"
                        );
                        assert!(
                            unique(&tokens, &optional, !optional.contains(&true), &row, &mut 0)
                                .is_none()
                        );
                    }
                }
            }
        }
    }
}
