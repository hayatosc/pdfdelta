//! Source ownership for a token interval and its retained complement.

use std::{collections::BTreeSet, ops::Range};

use crate::{Error, Result};

use super::{GraphNode, NodeContent, SourceRef, TextNormalization, TextView, evidence::invalid};

/// An immutable partition of one retained text view's source ownership.
///
/// Token positions and their full origin vectors remain borrowed from `parent`,
/// preserving repeated scalar-to-glyph references. Synthetic token origins are
/// context, never ownership. Sources without a source-backed token remain in
/// the complement, including when the selected interval contains every token.
///
/// This proves accounting only. It establishes neither correspondence, order
/// between nodes, inventory completeness, nor freedom from rival partitions.
/// The value cannot be deserialized as a trusted comparison certificate.
#[derive(Debug)]
pub struct TextSourcePartition<'a> {
    parent: &'a GraphNode,
    view: &'a TextView,
    selected: Range<usize>,
    owned: BTreeSet<SourceRef>,
}

impl<'a> TextSourcePartition<'a> {
    /// Partitions a retained view without dividing any source-backed glyph.
    /// `remaining` charges parent sources, tokens and every origin reference
    /// before allocating ownership sets. Failed attempts retain their charge.
    ///
    /// # Errors
    /// Returns a resource-limit error when the shared work budget is exhausted,
    /// invalid evidence for malformed projections/cuts, and unresolved evidence
    /// when a glyph has source-backed occurrences on both sides of the cut.
    pub fn new(
        parent: &'a GraphNode,
        selected: Range<usize>,
        remaining: &mut usize,
    ) -> Result<Self> {
        let NodeContent::Text { view } = &parent.content else {
            return Err(invalid("source partition requires a text view"));
        };
        if selected.start > selected.end
            || selected.end > view.tokens.len()
            || view.origins.len() != view.tokens.len()
            || view.source_backed.len() != view.tokens.len()
        {
            return Err(invalid(
                "source partition has an invalid token projection or cut",
            ));
        }
        let limit = *remaining;
        let charge = |remaining: &mut usize, count: usize| -> Result<()> {
            *remaining = remaining.checked_sub(count).ok_or_else(|| {
                *remaining = 0;
                Error::LimitExceeded {
                    resource: "text source partition work",
                    limit,
                }
            })?;
            Ok(())
        };
        charge(remaining, parent.sources.len())?;
        charge(remaining, view.tokens.len())?;
        for origins in &view.origins {
            charge(remaining, origins.len())?;
        }
        let sources: BTreeSet<_> = parent.sources.iter().copied().collect();
        if sources.len() != parent.sources.len()
            || view
                .origins
                .iter()
                .zip(&view.source_backed)
                .any(|(origins, backed)| {
                    origins.is_empty()
                        || (*backed && origins.iter().any(|source| !sources.contains(source)))
                        || origins.iter().copied().collect::<BTreeSet<_>>().len() != origins.len()
                })
        {
            return Err(invalid(
                "source partition has duplicate or foreign source references",
            ));
        }
        let owned: BTreeSet<_> = view.origins[selected.clone()]
            .iter()
            .zip(&view.source_backed[selected.clone()])
            .filter(|(_, backed)| **backed)
            .flat_map(|(origins, _)| origins.iter().copied())
            .collect();
        if view
            .origins
            .iter()
            .zip(&view.source_backed)
            .enumerate()
            .any(|(position, (origins, backed))| {
                *backed
                    && !selected.contains(&position)
                    && origins.iter().any(|source| owned.contains(source))
            })
        {
            return Err(Error::Unresolved(
                "source partition divides a shared glyph".into(),
            ));
        }
        Ok(Self {
            parent,
            view,
            selected,
            owned,
        })
    }

    #[must_use]
    pub fn parent(&self) -> &'a GraphNode {
        self.parent
    }

    #[must_use]
    pub fn selected_range(&self) -> Range<usize> {
        self.selected.clone()
    }

    /// Each owned source occurs once, in the parent's retained reference order.
    pub fn selected_sources(&self) -> impl Iterator<Item = SourceRef> + '_ {
        self.parent
            .sources
            .iter()
            .copied()
            .filter(|source| self.owned.contains(source))
    }

    /// Together with [`Self::selected_sources`], exactly partitions the parent.
    /// The complement may include sources represented only by synthetic context.
    pub fn remaining_sources(&self) -> impl Iterator<Item = SourceRef> + '_ {
        self.parent
            .sources
            .iter()
            .copied()
            .filter(|source| !self.owned.contains(source))
    }

    /// Both intervals belong to one complement; they need not have disjoint
    /// source projections from each other. Empty intervals remain explicit.
    #[must_use]
    pub fn remaining_ranges(&self) -> [Range<usize>; 2] {
        [
            0..self.selected.start,
            self.selected.end..self.view.tokens.len(),
        ]
    }

    /// Retains the full scalar-to-source multiplicity, including context.
    #[must_use]
    pub fn selected_origins(&self) -> &'a [Vec<SourceRef>] {
        &self.view.origins[self.selected.clone()]
    }

    /// Materializes the local comparison view, preserving optional positions.
    /// Synthetic origins may refer outside its owned sources. This is a local
    /// view of the retained parent, not a replacement node for the source graph.
    ///
    /// # Errors
    /// Returns unresolved evidence if normalization has no validated projection.
    pub fn selected_node(&self) -> Result<GraphNode> {
        let optional = self.view.optional_tokens().ok_or_else(|| {
            Error::Unresolved("source partition normalization remains unresolved".into())
        })?;
        let mut view = TextView {
            tokens: self.view.tokens[self.selected.clone()].to_vec(),
            origins: self.selected_origins().to_vec(),
            source_backed: self.view.source_backed[self.selected.clone()].to_vec(),
            normalization: TextNormalization::Exact,
        };
        let optional: Vec<_> = optional[self.selected.clone()]
            .iter()
            .enumerate()
            .filter_map(|(position, optional)| optional.then_some(position))
            .collect();
        if !optional.is_empty() {
            view.bind_optional_positions(optional);
        }
        let mut node = self.parent.clone();
        node.sources = self.selected_sources().collect();
        node.content = NodeContent::Text { view };
        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::{LocalComparisonLimits, NodeId, NodeKind, ViewBasis, compare_text_group_views},
        model::GlyphId,
        normalize::ComparableToken,
    };

    fn source(id: u64) -> SourceRef {
        SourceRef::Native { glyph: GlyphId(id) }
    }

    fn parent() -> GraphNode {
        let mut view = TextView {
            tokens: "fi x".chars().map(ComparableToken::Scalar).collect(),
            origins: vec![
                vec![source(1)],
                vec![source(1)],
                vec![source(1), source(2), source(3)],
                vec![source(2)],
            ],
            source_backed: vec![true, true, false, true],
            normalization: TextNormalization::Exact,
        };
        view.bind_optional_positions(vec![2]);
        GraphNode {
            id: NodeId(0),
            kind: NodeKind::Paragraph,
            pages: vec![],
            sources: vec![source(1), source(2), source(3)],
            identity: None,
            basis: ViewBasis::NativeLayout,
            content: NodeContent::Text { view },
        }
    }

    #[test]
    fn every_legal_cut_preserves_ownership_and_origin_multiplicity() {
        let node = parent();
        let NodeContent::Text { view } = &node.content else {
            unreachable!()
        };
        for start in 0..=4 {
            for end in start..=4 {
                let partition = TextSourcePartition::new(&node, start..end, &mut 100);
                let divides_ligature = (start..end).contains(&0) != (start..end).contains(&1);
                if divides_ligature {
                    assert!(matches!(partition, Err(Error::Unresolved(_))));
                    continue;
                }
                let partition = partition.expect("all remaining intervals preserve whole glyphs");
                let selected: BTreeSet<_> = partition.selected_sources().collect();
                let remainder: BTreeSet<_> = partition.remaining_sources().collect();
                assert!(selected.is_disjoint(&remainder));
                assert_eq!(
                    selected.union(&remainder).copied().collect::<Vec<_>>(),
                    node.sources
                );
                assert!(remainder.contains(&source(3)));
                let [before, after] = partition.remaining_ranges();
                let reconstructed: Vec<_> = view.origins[before]
                    .iter()
                    .chain(partition.selected_origins())
                    .chain(&view.origins[after])
                    .cloned()
                    .collect();
                assert_eq!(reconstructed, view.origins);
                assert!(std::ptr::eq(partition.parent(), &raw const node));
            }
        }
    }

    #[test]
    fn only_synthetic_tokens_cannot_acquire_neighbor_sources() {
        let node = parent();
        let partition =
            TextSourcePartition::new(&node, 2..3, &mut 100).expect("synthetic interval");
        assert_eq!(partition.selected_sources().count(), 0);
        assert_eq!(
            partition.remaining_sources().collect::<Vec<_>>(),
            node.sources
        );
        assert_eq!(
            partition.selected_origins(),
            &[vec![source(1), source(2), source(3)]]
        );
    }

    #[test]
    fn malformed_or_unbudgeted_partitions_cannot_produce_a_view() {
        let node = parent();
        let mut work = 0;
        assert!(matches!(
            TextSourcePartition::new(&node, 0..4, &mut work),
            Err(Error::LimitExceeded { .. })
        ));
        assert_eq!(work, 0);
        assert!(TextSourcePartition::new(&node, 0..5, &mut 100).is_err());
        let mut duplicate = node.clone();
        duplicate.sources.push(source(1));
        assert!(TextSourcePartition::new(&duplicate, 0..4, &mut 100).is_err());
        let mut foreign = node.clone();
        foreign.sources.retain(|reference| *reference != source(2));
        assert!(TextSourcePartition::new(&foreign, 0..4, &mut 100).is_err());
        let mut malformed = node.clone();
        let NodeContent::Text { view } = &mut malformed.content else {
            unreachable!()
        };
        view.source_backed.pop();
        assert!(TextSourcePartition::new(&malformed, 0..4, &mut 100).is_err());
    }

    #[test]
    fn local_diff_masks_exclude_complement_and_synthetic_context() {
        let old = parent();
        let mut new = parent();
        let NodeContent::Text { view } = &mut new.content else {
            unreachable!()
        };
        view.tokens[3] = ComparableToken::Scalar('y');
        view.bind_optional_positions(vec![2]);
        let old = TextSourcePartition::new(&old, 2..4, &mut 100).expect("old partition");
        let new = TextSourcePartition::new(&new, 2..4, &mut 100).expect("new partition");
        let a = old.selected_node().expect("validated normalization");
        let b = new.selected_node().expect("validated normalization");
        let comparison = compare_text_group_views(&[&a], &[&b], LocalComparisonLimits::default())
            .expect("exact local comparison");
        assert!(comparison.compared);
        let mask = comparison
            .text_mask
            .expect("one literal replacement has a mask");
        assert_eq!(mask.old.len(), 1);
        assert_eq!(mask.new.len(), 1);
        assert_eq!(mask.old[0].sources, [source(2)]);
        assert_eq!(mask.new[0].sources, [source(2)]);
        assert_eq!(
            old.remaining_sources().collect::<Vec<_>>(),
            [source(1), source(3)]
        );
        assert_eq!(
            new.remaining_sources().collect::<Vec<_>>(),
            [source(1), source(3)]
        );
    }

    #[test]
    fn nested_partitions_retain_external_context_without_owning_it() {
        let node = parent();
        let outer = TextSourcePartition::new(&node, 2..4, &mut 100)
            .expect("outer partition")
            .selected_node()
            .expect("local view");
        let inner = TextSourcePartition::new(&outer, 0..2, &mut 100)
            .expect("synthetic context may refer to the outer complement");
        assert_eq!(inner.selected_sources().collect::<Vec<_>>(), [source(2)]);
        assert_eq!(inner.remaining_sources().count(), 0);
        assert_eq!(
            inner.selected_origins()[0],
            [source(1), source(2), source(3)]
        );
    }
}
