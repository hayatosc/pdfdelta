//! Revalidated native intervals, independent of paragraph identity.

use super::*;
use crate::document::TextSourcePartition;

mod source_cuts;

pub(super) struct EqualCandidate {
    pub boundaries: [usize; 2],
    pub old: Vec<NodeId>,
    pub new: Vec<NodeId>,
}

#[derive(Clone, Copy)]
enum Input<'a> {
    Review(&'a TextScopeReview, bool),
    Equal(&'a EqualCandidate),
}

struct Prepared {
    boundaries: [usize; 2],
    old: Vec<GraphNode>,
    new: Vec<GraphNode>,
    old_sources: Vec<SourceRef>,
    new_sources: Vec<SourceRef>,
    cut_partition: Option<source_cuts::CutPartition>,
}

/// A source-conserving native interval under two admitted boundary premises.
/// Boundary sources remain outside the owned interior. This certificate does not
/// discharge paragraph identity, relationship search, or global inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTextIntervalComparison {
    boundaries: [usize; 2],
    old_sources: Vec<SourceRef>,
    new_sources: Vec<SourceRef>,
    comparison: LocalViewComparison,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cut_partition: Option<source_cuts::CutPartition>,
    #[serde(skip)]
    verified: bool,
}

impl NativeTextIntervalComparison {
    /// Exact local result under the retained boundary premises.
    pub fn comparison(&self) -> &LocalViewComparison {
        &self.comparison
    }

    pub(crate) fn sources(&self, old: bool) -> &[SourceRef] {
        if !self.verified {
            &[]
        } else if old {
            &self.old_sources
        } else {
            &self.new_sources
        }
    }
}

fn reserved_sources(
    view: DocumentView<'_>,
    result: &ScopeViewComparison,
    old: bool,
    remaining: &mut usize,
) -> Option<BTreeSet<SourceRef>> {
    let mut ids = BTreeSet::new();
    for comparison in &result.comparisons {
        let nodes = if old {
            &comparison.old
        } else {
            &comparison.new
        };
        spend(remaining, nodes.len().saturating_add(1))?;
        if comparison.compared
            && comparison.interpretation == InterpretationStatus::ConditionalOnCorrespondence
        {
            ids.extend(nodes.iter().copied());
        }
    }
    spend(remaining, view.graph.nodes.len())?;
    let mut reserved = BTreeSet::new();
    for node in &view.graph.nodes {
        if ids.contains(&node.id) {
            spend(remaining, node.sources.len())?;
            reserved.extend(node.sources.iter().copied());
        }
    }
    for domain in &result.native_text_domains {
        spend(remaining, domain.sources(old).len().saturating_add(1))?;
        reserved.extend(domain.sources(old).iter().copied());
    }
    Some(reserved)
}

fn project<'a>(
    view: DocumentView<'a>,
    index: &mut Option<BTreeMap<NodeId, &'a GraphNode>>,
    root: NodeId,
    native: &native::Sources<'_>,
    endpoints: [&[NodeId]; 2],
    interior: &[NodeId],
    remaining: &mut usize,
) -> Option<(Vec<GraphNode>, Vec<SourceRef>)> {
    if index.is_none() {
        spend(remaining, view.graph.nodes.len())?;
        *index = Some(
            view.graph
                .nodes
                .iter()
                .map(|node| (node.id, node))
                .collect(),
        );
    }
    let nodes = index.as_ref()?;
    let lookup_work = nodes.len().saturating_add(1).ilog2() as usize + 1;
    let mut path = Vec::new();
    let mut owned = Vec::new();
    let mut projected = Vec::new();
    let mut population = BTreeSet::new();
    for (group, ids) in [endpoints[0], interior, endpoints[1]]
        .into_iter()
        .enumerate()
    {
        for id in ids {
            spend(remaining, lookup_work)?;
            let node = *nodes.get(id)?;
            let (checked, _) = native.project_interval(node, remaining)?;
            let NodeContent::Text { view: text } = &checked.content else {
                return None;
            };
            let part = TextSourcePartition::new(&checked, 0..text.tokens.len(), remaining).ok()?;
            if part.remaining_sources().next().is_some() {
                return None;
            }
            for source in part.selected_sources() {
                spend(remaining, 1)?;
                if !population.insert(source) {
                    return None;
                }
                if group == 1 {
                    owned.push(source);
                }
            }
            path.push(node);
            if group == 1 {
                projected.push(checked);
            }
        }
    }
    let (_, padding, _) = native.census(view, root, &path, remaining, true, false)?;
    // Whole-node ownership cannot include clipped boundary padding. Spatial
    // row closure still checks every glyph and every intersecting paint effect.
    if !padding.is_empty() {
        return None;
    }
    Some((projected, owned))
}

pub(super) fn append(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    native: (
        &super::super::evidence::NativeIndex<'_>,
        &super::super::evidence::NativeIndex<'_>,
    ),
    result: &mut ScopeViewComparison,
    parent: InterpretationStatus,
    limits: DocumentComparisonLimits,
    equal: &[EqualCandidate],
) -> Result<()> {
    if !limits.matching.channels.text || parent != InterpretationStatus::ConditionalOnCorrespondence
    {
        return Ok(());
    }
    let mut remaining = limits.local.proof_work;
    // Views are immutable throughout this call. Reuse their node lookup, while
    // charging its construction and every lookup to the shared proof budget.
    // Native projection, source ownership and population closure are rechecked.
    let mut node_indexes = [None, None];
    let mut old_native = native::Sources::new(native.0);
    let mut new_native = native::Sources::new(native.1);
    if result
        .text_scope_reviews
        .iter()
        .any(|review| review.native_regions.is_some())
    {
        old_native.acquire_native_order(old, &mut remaining);
        new_native.acquire_native_order(new, &mut remaining);
    }
    let Some(mut owned_old) = reserved_sources(old, result, true, &mut remaining) else {
        return Ok(());
    };
    let Some(mut owned_new) = reserved_sources(new, result, false, &mut remaining) else {
        return Ok(());
    };
    let reviews = [false, true].into_iter().flat_map(|cut_pass| {
        result
            .text_scope_reviews
            .iter()
            .map(move |review| Input::Review(review, cut_pass))
    });
    // Preserve all existing review proof work before examining equal domains.
    for input in reviews.chain(equal.iter().map(Input::Equal)) {
        if spend(&mut remaining, 1).is_none() {
            break;
        }
        if matches!(input, Input::Equal(_))
            && spend(
                &mut remaining,
                result.accepted_correspondences.len().saturating_mul(2),
            )
            .is_none()
        {
            break;
        }
        let prepared = match input {
            Input::Equal(candidate)
                if candidate
                    .boundaries
                    .iter()
                    .any(|index| !result.accepted_correspondences.contains(index)) =>
            {
                source_cuts::prepare_equal(
                    [old, new],
                    [&old_native, &new_native],
                    result,
                    candidate,
                    &mut remaining,
                )
            }
            Input::Equal(candidate) => prepare_whole(
                [old, new],
                &mut node_indexes,
                [&old_native, &new_native],
                result,
                &candidate.boundaries,
                [&candidate.old, &candidate.new],
                &mut remaining,
            ),
            Input::Review(review, cut_pass) => {
                if review.source_cuts.is_some() != cut_pass
                    || review.comparison.interpretation
                        != InterpretationStatus::ConditionalOnCorrespondence
                {
                    continue;
                }
                if let Some(range) = &review.source_cuts {
                    source_cuts::prepare(
                        [old, new],
                        [&old_native, &new_native],
                        result,
                        range,
                        &mut remaining,
                    )
                } else {
                    // Region metadata is not an ownership certificate. The full
                    // path, native transitions and both boundary premises are
                    // reconstructed before any source can enter coverage.
                    prepare_whole(
                        [old, new],
                        &mut node_indexes,
                        [&old_native, &new_native],
                        result,
                        &review.boundaries,
                        [&review.comparison.old, &review.comparison.new],
                        &mut remaining,
                    )
                }
            }
        };
        let Some(Prepared {
            boundaries,
            old: a,
            new: b,
            old_sources: a_sources,
            new_sources: b_sources,
            cut_partition,
        }) = prepared
        else {
            continue;
        };
        if let Input::Review(review, _) = input
            && cut_partition.is_some()
            && (a_sources != review.old_sources || b_sources != review.new_sources)
        {
            continue;
        }
        if a_sources.iter().any(|source| owned_old.contains(source))
            || b_sources.iter().any(|source| owned_new.contains(source))
        {
            continue;
        }
        let a: Vec<_> = a.iter().collect();
        let b: Vec<_> = b.iter().collect();
        let comparison = match super::super::operations::compare_text_groups_with_work(
            &a,
            &b,
            limits.local,
            true,
            &mut remaining,
        ) {
            Ok(comparison) => comparison,
            Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => continue,
            Err(error) => return Err(error),
        };
        if matches!(input, Input::Equal(_)) && comparison.operation.is_some() {
            continue;
        }
        if complete_content_proof(&comparison) {
            owned_old.extend(a_sources.iter().copied());
            owned_new.extend(b_sources.iter().copied());
            result
                .native_text_intervals
                .push(NativeTextIntervalComparison {
                    boundaries,
                    old_sources: a_sources,
                    new_sources: b_sources,
                    comparison,
                    cut_partition,
                    verified: true,
                });
        }
    }
    Ok(())
}

fn complete_content_proof(comparison: &LocalViewComparison) -> bool {
    if !comparison.compared
        || comparison.interpretation != InterpretationStatus::ConditionalOnCorrespondence
    {
        return false;
    }
    let Some(mask) = &comparison.text_mask else {
        // A count witness can prove a change without completing the exact
        // comparison of this domain. It does not discharge source coverage.
        return false;
    };
    // The exact kernel quantifies over the complete validated interpretation
    // family. A positive lower bound proves content changed even when some
    // locations differ between interpretations. Keep every localization issue
    // and the original mandatory mask; neither is converted into certainty.
    mask.claims.changed_source_lower > 0
        || (mask.claims.changed_source_upper == 0 && comparison.unresolved.is_empty())
}

fn prepare_whole<'a>(
    views: [DocumentView<'a>; 2],
    indexes: &mut [Option<BTreeMap<NodeId, &'a GraphNode>>; 2],
    native: [&native::Sources<'_>; 2],
    result: &ScopeViewComparison,
    boundaries: &[usize],
    members: [&[NodeId]; 2],
    remaining: &mut usize,
) -> Option<Prepared> {
    let [entry, exit] = boundaries else {
        return None;
    };
    spend(
        remaining,
        result
            .matching
            .source_only_mandatory
            .len()
            .saturating_add(result.matching.inferred_proposals.len())
            .saturating_add(result.accepted_correspondences.len())
            .saturating_mul(2),
    )?;
    if ![*entry, *exit].iter().all(|index| {
        result.matching.source_only_mandatory.contains(index)
            && !result.matching.inferred_proposals.contains(index)
            && result.accepted_correspondences.contains(index)
    }) {
        return None;
    }
    let first = &result.candidates.proposals[*entry];
    let last = &result.candidates.proposals[*exit];
    let (a, a_sources) = project(
        views[0],
        &mut indexes[0],
        result.matching.scope.old,
        native[0],
        [&first.old, &last.old],
        members[0],
        remaining,
    )?;
    let (b, b_sources) = project(
        views[1],
        &mut indexes[1],
        result.matching.scope.new,
        native[1],
        [&first.new, &last.new],
        members[1],
        remaining,
    )?;
    Some(Prepared {
        boundaries: [*entry, *exit],
        old: a,
        new: b,
        old_sources: a_sources,
        new_sources: b_sources,
        cut_partition: None,
    })
}
