use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Result;

use super::{
    CorrespondenceProposal, CorrespondenceScope, DocumentView, MatchingLimits, NodeContent,
    ProposalBasis, ScopeProposals, matching::selected_children,
};

#[derive(Clone, Copy, Debug)]
pub struct VisualCandidateLimits {
    /// Total sample positions inspected across candidate pairs in one scope.
    pub max_pixel_comparisons: usize,
}

impl Default for VisualCandidateLimits {
    fn default() -> Self {
        Self {
            max_pixel_comparisons: 32_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualCandidateSearch {
    pub examined_pairs: usize,
    pub compared_pixels: usize,
    pub exhaustive: bool,
}

/// Adds same-grid visual proposals to the common candidate pool. The fixed-point
/// objective counts equal RGB samples; it is not a probability or an identity
/// claim. Page ordinals are deliberately absent from candidate generation.
/// Caller validation guarantees nonempty, correctly sized raster payloads.
pub(super) fn append_visual_candidates(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    scope: CorrespondenceScope,
    candidates: &mut ScopeProposals,
    matching: MatchingLimits,
    limits: VisualCandidateLimits,
) -> Result<VisualCandidateSearch> {
    let left = selected_children(old.graph, scope.old, matching.channels)?;
    let right = selected_children(new.graph, scope.new, matching.channels)?;
    let old_regions: BTreeMap<_, _> = old
        .evidence
        .rendered
        .iter()
        .map(|region| (region.id, region))
        .collect();
    let new_regions: BTreeMap<_, _> = new
        .evidence
        .rendered
        .iter()
        .map(|region| (region.id, region))
        .collect();
    let mut search = VisualCandidateSearch {
        examined_pairs: 0,
        compared_pixels: 0,
        exhaustive: true,
    };
    if !candidates.exhaustive {
        search.exhaustive = false;
        return Ok(search);
    }
    let start = candidates.proposals.len();
    'pairs: for a in &left {
        let NodeContent::Visual { region: a_region } = a.content else {
            continue;
        };
        for b in &right {
            let NodeContent::Visual { region: b_region } = b.content else {
                continue;
            };
            if search.examined_pairs == matching.max_pair_checks {
                search.exhaustive = false;
                break 'pairs;
            }
            search.examined_pairs += 1;
            // A known label cannot be replaced with a visually similar rival.
            if a.kind != b.kind || a.identity.is_some() || b.identity.is_some() {
                continue;
            }
            let a_region = old_regions[&a_region];
            let b_region = new_regions[&b_region];
            if old.evidence.backends[a_region.backend] != new.evidence.backends[b_region.backend]
                || a_region.raster.width != b_region.raster.width
                || a_region.raster.height != b_region.raster.height
                || a_region.composited_page != b_region.composited_page
            {
                continue;
            }
            let pixels = a_region.raster.rgb.len() / 3;
            if candidates.proposals.len() == matching.max_proposals
                || pixels
                    > limits
                        .max_pixel_comparisons
                        .saturating_sub(search.compared_pixels)
            {
                search.exhaustive = false;
                break 'pairs;
            }
            search.compared_pixels += pixels;
            let matching_pixels = a_region
                .raster
                .rgb
                .as_chunks::<3>()
                .0
                .iter()
                .zip(b_region.raster.rgb.as_chunks::<3>().0)
                .filter(|(a, b)| a == b)
                .count();
            let weight = 1 + ((matching_pixels as u128 * 1_000_000) / pixels as u128) as u32;
            candidates.proposals.push(CorrespondenceProposal {
                old: vec![a.id],
                new: vec![b.id],
                basis: ProposalBasis::VisualSimilarity,
                supplier: "same-grid-rgb-equality-v1".into(),
                weight,
            });
        }
    }
    if !search.exhaustive {
        candidates.proposals.truncate(start);
    }
    Ok(search)
}
