//! Owned native interiors behind independently admitted boundary matches.

use super::*;
use crate::document::{NodeContent, TextNormalization, TextSourcePartition};
use crate::normalize::ComparableToken;

/// Equality of owned native source fragments under the enclosing scope premise.
///
/// Each entire parent passes local native inventory, order, paint and rival-view
/// closure. Only the source-backed unpadded interior is owned. Complements remain
/// explicit obligations; this proves neither paragraph identity nor page inventory.
/// It is independent of non-owning range reviews and supplies no relationship map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTextDomainEquality {
    proposal: usize,
    old: SourceFragment,
    new: SourceFragment,
    old_complement: Vec<SourceRef>,
    new_complement: Vec<SourceRef>,
    /// Source-side row conventions are descriptive, not deserializable proof authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_row_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_row_order: Option<String>,
    /// Reports describe this proof but cannot recreate its in-process validation.
    #[serde(skip)]
    verified: bool,
}

impl NativeTextDomainEquality {
    pub(crate) fn sources(&self, old: bool) -> &[SourceRef] {
        if !self.verified {
            return &[];
        }
        if old {
            &self.old.sources
        } else {
            &self.new.sources
        }
    }
}

fn partition<'a>(node: &'a GraphNode, remaining: &mut usize) -> Option<TextSourcePartition<'a>> {
    let NodeContent::Text { view } = &node.content else {
        return None;
    };
    if view.normalization != TextNormalization::Exact {
        return None;
    }
    spend(remaining, view.tokens.len().saturating_mul(2))?;
    let start = view
        .tokens
        .iter()
        .position(|token| *token != ComparableToken::Scalar(' '))?;
    let end = view
        .tokens
        .iter()
        .rposition(|token| *token != ComparableToken::Scalar(' '))?
        + 1;
    if !view
        .source_backed
        .get(start..end)?
        .iter()
        .all(|backed| *backed)
    {
        return None;
    }
    if view.tokens[start..end].iter().any(|token| {
        token
            .as_scalar()
            .is_none_or(super::super::operations::private_use_scalar)
    }) {
        return None;
    }
    // Unprojected parent evidence could lie between the apparent endpoints.
    // Keep it unresolved rather than interpreting it as exterior padding.
    let whole = TextSourcePartition::new(node, 0..view.tokens.len(), remaining).ok()?;
    if whole.remaining_sources().next().is_some() {
        return None;
    }
    TextSourcePartition::new(node, start..end, remaining).ok()
}

fn validated_partition<'a>(
    node: &'a GraphNode,
    sources: &native::Sources<'_>,
    view: DocumentView<'_>,
    root: NodeId,
    remaining: &mut usize,
) -> Option<(TextSourcePartition<'a>, Option<native::RowOrder>)> {
    // Expand contracted native spaces to validate every complete glyph and
    // its order. Expansion outside the body does not change the owned interval.
    let (projected, row_order) = match sources.project_census(node, &[], false, remaining) {
        Some((projected, _)) => (projected, None),
        None => {
            // A tiny ascending baseline step can defeat the spatial projection.
            // Reusing the paint convention requires its complete source census,
            // including glyphs just outside the exact baseline band. Merely
            // relaxing the projection would miss those neighboring sources.
            if !sources.boundary_roundoff(&[node], remaining)? {
                return None;
            }
            let (_, padding, Some(native::RowOrder::Paint)) =
                sources.census(view, root, &[node], remaining, false, true)?
            else {
                return None;
            };
            if !padding.is_empty() {
                return None;
            }
            let (projected, _) = sources.project_census(node, &[], true, remaining)?;
            (projected, Some(native::RowOrder::Paint))
        }
    };
    let checked = partition(&projected, remaining)?;
    let original = partition(node, remaining)?;
    let (NodeContent::Text { view: a }, NodeContent::Text { view: b }) =
        (&node.content, &projected.content)
    else {
        return None;
    };
    spend(remaining, original.selected_range().len())?;
    if a.tokens[original.selected_range()] != b.tokens[checked.selected_range()] {
        return None;
    }
    Some((original, row_order))
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
) -> Result<()> {
    if !limits.matching.channels.text
        || parent != InterpretationStatus::ConditionalOnCorrespondence
        || result.text_boundary_correspondences.is_empty()
    {
        return Ok(());
    }
    // A separate bounded proof phase leaves discovery's established budget intact.
    let mut remaining = limits.local.proof_work;
    if spend(
        &mut remaining,
        old.graph.nodes.len().saturating_add(new.graph.nodes.len()),
    )
    .is_none()
    {
        return Ok(());
    }
    let old_nodes: BTreeMap<_, _> = old.graph.nodes.iter().map(|node| (node.id, node)).collect();
    let new_nodes: BTreeMap<_, _> = new.graph.nodes.iter().map(|node| (node.id, node)).collect();
    let old_sources = native::Sources::new(native.0);
    let new_sources = native::Sources::new(native.1);
    for &index in &result.text_boundary_correspondences {
        if spend(&mut remaining, 1).is_none() {
            break;
        }
        if !result.matching.source_only_mandatory.contains(&index)
            || result.matching.inferred_proposals.contains(&index)
        {
            continue;
        }
        let proposal = &result.candidates.proposals[index];
        let ([a], [b]) = (proposal.old.as_slice(), proposal.new.as_slice()) else {
            continue;
        };
        let (a, b) = (old_nodes[a], new_nodes[b]);
        if old_sources
            .closed(old, result.matching.scope.old, &[a], &mut remaining)
            .is_none()
            || new_sources
                .closed(new, result.matching.scope.new, &[b], &mut remaining)
                .is_none()
        {
            continue;
        }
        // Closure checks the population and visibility band, not the text
        // projection. Validate full glyph readings and source order before a
        // view can discharge any of that population's source obligations.
        let Some((a, old_row_order)) = validated_partition(
            a,
            &old_sources,
            old,
            result.matching.scope.old,
            &mut remaining,
        ) else {
            continue;
        };
        let Some((b, new_row_order)) = validated_partition(
            b,
            &new_sources,
            new,
            result.matching.scope.new,
            &mut remaining,
        ) else {
            continue;
        };
        let (Ok(left), Ok(right)) = (a.selected_node(), b.selected_node()) else {
            continue;
        };
        // Admission already proves token equality. Reuse the exact comparator
        // to verify the selected projections under its existing text contract.
        let comparison = super::super::operations::compare_local_views_with_work(
            &left,
            &right,
            old.evidence,
            new.evidence,
            limits.local,
            &mut remaining,
        );
        let comparison = match comparison {
            Ok(comparison) => comparison,
            Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => continue,
            Err(error) => return Err(error),
        };
        if !comparison.compared
            || comparison.operation.is_some()
            || !comparison.unresolved.is_empty()
            || comparison.interpretation != InterpretationStatus::ConditionalOnCorrespondence
        {
            continue;
        }
        let fragment = |part: &TextSourcePartition<'_>| SourceFragment {
            node: part.parent().id,
            tokens: [part.selected_range().start, part.selected_range().end],
            sources: part.selected_sources().collect(),
        };
        result.native_text_domains.push(NativeTextDomainEquality {
            proposal: index,
            old: fragment(&a),
            new: fragment(&b),
            old_complement: a.remaining_sources().collect(),
            new_complement: b.remaining_sources().collect(),
            old_row_order: old_row_order.map(|order| order.convention().to_owned()),
            new_row_order: new_row_order.map(|order| order.convention().to_owned()),
            verified: true,
        });
    }
    Ok(())
}
