//! Coverage counts discovered evidence, independently of change-event recall.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    Channel, DocumentView, DocumentViewComparison, InterpretationStatus, MatchingChannels,
    SourceRef, selected_nodes,
};

/// Counts refer to distinct source references, not characters, pixels, or area.
/// Missing inventories leave the total amount of undiscovered evidence unknown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCoverage {
    pub channel: Channel,
    pub old_inventory_complete: bool,
    pub new_inventory_complete: bool,
    pub old_discovered_sources: usize,
    pub new_discovered_sources: usize,
    pub old_compared_sources: usize,
    pub new_compared_sources: usize,
    pub old_uncompared_sources: usize,
    pub new_uncompared_sources: usize,
    pub complete: bool,
}

/// Measure selected channels against their discovery inventories.
///
/// Inputs must be the validated stores and graphs used to produce `comparison`.
/// Inferred or unsuccessful comparisons do not discharge source obligations.
/// Channel completeness also requires complete discovery on both sides; overall
/// document completeness additionally requires resolved comparison search.
#[must_use]
pub fn document_coverage(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    comparison: &DocumentViewComparison,
    channels: &BTreeSet<Channel>,
) -> Vec<ChannelCoverage> {
    channels
        .iter()
        .map(|channel| {
            let (old_inventory, old_discovered, old_compared) =
                side_coverage(old, comparison, *channel, true);
            let (new_inventory, new_discovered, new_compared) =
                side_coverage(new, comparison, *channel, false);
            let old_uncompared_sources = old_discovered.difference(&old_compared).count();
            let new_uncompared_sources = new_discovered.difference(&new_compared).count();
            ChannelCoverage {
                channel: *channel,
                old_inventory_complete: old_inventory,
                new_inventory_complete: new_inventory,
                old_discovered_sources: old_discovered.len(),
                new_discovered_sources: new_discovered.len(),
                old_compared_sources: old_discovered.len() - old_uncompared_sources,
                new_compared_sources: new_discovered.len() - new_uncompared_sources,
                old_uncompared_sources,
                new_uncompared_sources,
                complete: old_inventory
                    && new_inventory
                    && old_uncompared_sources == 0
                    && new_uncompared_sources == 0,
            }
        })
        .collect()
}

fn side_coverage(
    view: DocumentView<'_>,
    comparison: &DocumentViewComparison,
    channel: Channel,
    old: bool,
) -> (bool, BTreeSet<SourceRef>, BTreeSet<SourceRef>) {
    let store = view.evidence;
    let graph = view.graph;
    let inventory = store.inventory_complete(None, channel)
        || (!store.pages.is_empty()
            && store
                .pages
                .iter()
                .all(|page| store.inventory_complete(Some(page.page), channel)));
    let discovered = store
        .inventories
        .iter()
        .filter(|inventory| inventory.channel == channel)
        .flat_map(|inventory| inventory.sources.iter().copied())
        .collect();
    let compared_nodes: BTreeSet<_> = comparison
        .comparisons()
        .filter(|pair| {
            pair.compared
                && pair.interpretation == InterpretationStatus::ConditionalOnCorrespondence
        })
        .flat_map(|pair| if old { &pair.old } else { &pair.new })
        .copied()
        .collect();
    let channel_nodes = selected_nodes(
        graph,
        MatchingChannels {
            relations: false,
            presentation: false,
            ..MatchingChannels::from(&BTreeSet::from([channel]))
        },
    );
    let mut compared: BTreeSet<_> = graph
        .nodes
        .iter()
        .filter(|node| compared_nodes.contains(&node.id) && channel_nodes.contains(&node.id))
        .flat_map(|node| node.sources.iter().copied())
        .collect();
    if channel == Channel::Relations {
        compared.extend(
            comparison
                .relations()
                .filter(|relation| {
                    relation.interpretation == InterpretationStatus::ConditionalOnCorrespondence
                })
                .flat_map(|relation| {
                    if old {
                        &relation.old_sources
                    } else {
                        &relation.new_sources
                    }
                })
                .copied(),
        );
    }
    (inventory, discovered, compared)
}
