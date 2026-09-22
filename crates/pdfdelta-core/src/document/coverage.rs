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
    /// Stored-field references accounted for by validated native key presence.
    /// Paragraph identity never discharges its glyph or relationship evidence.
    #[serde(default)]
    pub old_presence_sources: usize,
    #[serde(default)]
    pub new_presence_sources: usize,
    pub old_uncompared_sources: usize,
    pub new_uncompared_sources: usize,
    pub complete: bool,
}

/// One side's source accounting for one channel, with the references retained.
///
/// Counts alone cannot say *which* evidence remains unexamined, so a reviewer's
/// obligations cannot be enumerated from [`ChannelCoverage`]. This type keeps
/// the sets that produce those counts. Membership is discovery and comparison
/// bookkeeping, never ownership of the material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideSourceAccounting {
    /// Whether channel discovery itself closed. When false, the amount of
    /// undiscovered evidence is unknown and `discovered` is not a denominator.
    pub inventory_complete: bool,
    pub discovered: BTreeSet<SourceRef>,
    /// References reached by a conditional comparison. This may include
    /// references outside `discovered`, which discharge no discovery obligation.
    pub compared: BTreeSet<SourceRef>,
    /// Stored-field references accounted for by validated native key presence.
    pub presence: BTreeSet<SourceRef>,
}

impl SideSourceAccounting {
    /// Discovered references that no conditional comparison or key presence
    /// accounted for, in stable order.
    ///
    /// These are exactly the obligations a review packet must explain, either
    /// as a case or as an explicit gap.
    pub fn uncompared(&self) -> impl Iterator<Item = SourceRef> + '_ {
        self.discovered
            .iter()
            .filter(|source| !self.compared.contains(source) && !self.presence.contains(source))
            .copied()
    }

    fn compared_count(&self) -> usize {
        self.discovered.intersection(&self.compared).count()
    }
}

/// Both sides' source accounting for one selected channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSourceAccounting {
    pub channel: Channel,
    pub old: SideSourceAccounting,
    pub new: SideSourceAccounting,
}

impl ChannelSourceAccounting {
    /// The same completeness rule [`ChannelCoverage::complete`] reports.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.old.inventory_complete
            && self.new.inventory_complete
            && self.old.uncompared().next().is_none()
            && self.new.uncompared().next().is_none()
    }

    fn coverage(&self) -> ChannelCoverage {
        let old_compared_sources = self.old.compared_count();
        let new_compared_sources = self.new.compared_count();
        let old_presence_sources = self.old.presence.len();
        let new_presence_sources = self.new.presence.len();
        ChannelCoverage {
            channel: self.channel,
            old_inventory_complete: self.old.inventory_complete,
            new_inventory_complete: self.new.inventory_complete,
            old_discovered_sources: self.old.discovered.len(),
            new_discovered_sources: self.new.discovered.len(),
            old_compared_sources,
            new_compared_sources,
            old_presence_sources,
            new_presence_sources,
            old_uncompared_sources: self.old.discovered.len()
                - old_compared_sources
                - old_presence_sources,
            new_uncompared_sources: self.new.discovered.len()
                - new_compared_sources
                - new_presence_sources,
            complete: self.complete(),
        }
    }
}

/// Enumerate the discovered, compared, and unexamined references per channel.
///
/// Inputs must be the validated stores and graphs used to produce `comparison`.
/// This is the same accounting [`document_coverage`] summarizes, exposed so
/// that unexamined evidence can be located rather than merely counted.
#[must_use]
pub fn document_source_accounting(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    comparison: &DocumentViewComparison,
    channels: &BTreeSet<Channel>,
) -> Vec<ChannelSourceAccounting> {
    channels
        .iter()
        .map(|channel| {
            let (old_inventory, old_discovered, old_compared) =
                side_coverage(old, comparison, *channel, true);
            let (new_inventory, new_discovered, new_compared) =
                side_coverage(new, comparison, *channel, false);
            let presence = |old: bool, discovered: &BTreeSet<_>, compared: &BTreeSet<_>| {
                if *channel != Channel::Forms {
                    return BTreeSet::new();
                }
                let Some(keys) = &comparison.key_presence else {
                    return BTreeSet::new();
                };
                comparison
                    .keyed_element_operations()
                    .filter(|operation| {
                        let claim = &keys.claims[operation.claim];
                        claim.domain == super::KeyDomain::PdfFieldName
                            && (claim.side == super::PresenceSide::Old) == old
                            && discovered.contains(&operation.identity_source)
                            && !compared.contains(&operation.identity_source)
                    })
                    .map(|operation| operation.identity_source)
                    .collect()
            };
            let old_presence = presence(true, &old_discovered, &old_compared);
            let new_presence = presence(false, &new_discovered, &new_compared);
            ChannelSourceAccounting {
                channel: *channel,
                old: SideSourceAccounting {
                    inventory_complete: old_inventory,
                    discovered: old_discovered,
                    compared: old_compared,
                    presence: old_presence,
                },
                new: SideSourceAccounting {
                    inventory_complete: new_inventory,
                    discovered: new_discovered,
                    compared: new_compared,
                    presence: new_presence,
                },
            }
        })
        .collect()
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
    document_source_accounting(old, new, comparison, channels)
        .iter()
        .map(ChannelSourceAccounting::coverage)
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
    if channel == Channel::Text {
        compared.extend(
            comparison
                .scopes
                .iter()
                .filter(|scope| {
                    scope.interpretation == InterpretationStatus::ConditionalOnCorrespondence
                })
                .flat_map(|scope| &scope.result.native_text_domains)
                .flat_map(|domain| domain.sources(old))
                .copied(),
        );
        compared.extend(
            comparison
                .scopes
                .iter()
                .filter(|scope| {
                    scope.interpretation == InterpretationStatus::ConditionalOnCorrespondence
                })
                .flat_map(|scope| &scope.result.native_text_intervals)
                .flat_map(|interval| interval.sources(old))
                .copied(),
        );
    }
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
