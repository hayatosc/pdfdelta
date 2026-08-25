use std::collections::HashMap;

use crate::{Error, Result, layout::BlockId, normalize::ComparableToken};

use super::features::{BlockFeatures, validate_feature_ids};

pub const DEFAULT_ANCHOR_MIN_TOKENS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExactAnchor {
    pub old: BlockId,
    pub new: BlockId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonotoneAnchorChain {
    pub main_chain: Vec<ExactAnchor>,
    pub move_candidates: Vec<ExactAnchor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorIntervalWindow {
    pub old_range: (usize, usize),
    pub new_range: (usize, usize),
    pub left_anchor: Option<ExactAnchor>,
    pub right_anchor: Option<ExactAnchor>,
}

/// Generates high-confidence 1:1 exact anchors between old and new block features.
///
/// Anchors must:
/// - Have identical canonical token sequences
/// - Appear exactly once in `old` and exactly once in `new`
/// - Have at least `min_token_count` tokens
/// - Have no normalization issues (e.g. unmapped tokens or ambiguous line breaks)
pub fn exact_anchors(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    min_token_count: usize,
) -> Result<Vec<ExactAnchor>> {
    if min_token_count == 0 {
        return Err(Error::InvalidConfiguration(
            "anchor min_token_count must be greater than zero".to_owned(),
        ));
    }
    validate_feature_ids("old", old)?;
    validate_feature_ids("new", new)?;

    let old_counts = exact_token_counts(old, min_token_count);
    let new_counts = exact_token_counts(new, min_token_count);
    let new_blocks = new
        .iter()
        .filter(|features| {
            !features.has_normalization_issues && features.canonical_tokens.len() >= min_token_count
        })
        .map(|features| (features.canonical_tokens.clone(), features.block))
        .collect::<HashMap<_, _>>();

    Ok(old
        .iter()
        .filter(|features| {
            !features.has_normalization_issues && features.canonical_tokens.len() >= min_token_count
        })
        .filter_map(|features| {
            let tokens = &features.canonical_tokens;
            (old_counts.get(tokens) == Some(&1) && new_counts.get(tokens) == Some(&1)).then(|| {
                ExactAnchor {
                    old: features.block,
                    new: new_blocks[tokens],
                }
            })
        })
        .collect())
}

/// Computes the maximum monotone anchor chain using an $O(N \log N)$ Fenwick-tree LIS on new-block positions.
///
/// Anchors not included in the monotone main chain are preserved as move candidates.
pub fn select_monotone_anchor_chain(
    anchors: &[ExactAnchor],
    old: &[BlockFeatures],
    new: &[BlockFeatures],
) -> Result<MonotoneAnchorChain> {
    let old_indices = old
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let new_indices = new
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();

    let mut positioned = Vec::with_capacity(anchors.len());
    for anchor in anchors {
        let old_index = old_indices.get(&anchor.old).copied().ok_or_else(|| {
            Error::Unresolved(format!(
                "anchor references unknown old block {}",
                anchor.old.0
            ))
        })?;
        let new_index = new_indices.get(&anchor.new).copied().ok_or_else(|| {
            Error::Unresolved(format!(
                "anchor references unknown new block {}",
                anchor.new.0
            ))
        })?;
        positioned.push((*anchor, old_index, new_index));
    }

    positioned.sort_by(|(a1, o1, n1), (a2, o2, n2)| {
        o1.cmp(o2)
            .then(n1.cmp(n2))
            .then(a1.old.0.cmp(&a2.old.0))
            .then(a1.new.0.cmp(&a2.new.0))
    });

    if positioned.is_empty() {
        return Ok(MonotoneAnchorChain {
            main_chain: Vec::new(),
            move_candidates: Vec::new(),
        });
    }

    let mut sorted_new_indices = positioned
        .iter()
        .map(|(_, _, new_index)| *new_index)
        .collect::<Vec<_>>();
    sorted_new_indices.sort_unstable();
    sorted_new_indices.dedup();

    let mut previous = vec![None; positioned.len()];
    let mut fenwick = vec![None; sorted_new_indices.len() + 1];
    let mut chain_end = None;
    for (index, (_, _, new_index)) in positioned.iter().enumerate() {
        let rank = sorted_new_indices.partition_point(|candidate| candidate < new_index);
        let predecessor = query_chain_tip(&fenwick, rank);
        let tip = ChainTip {
            length: predecessor.map_or(1, |tip| tip.length + 1),
            position: index,
        };
        previous[index] = predecessor.map(|tip| tip.position);
        update_chain_tip(&mut fenwick, rank + 1, tip);
        chain_end = preferred_chain_tip(chain_end, Some(tip));
    }

    let Some(mut end) = chain_end.map(|tip| tip.position) else {
        return Ok(MonotoneAnchorChain {
            main_chain: Vec::new(),
            move_candidates: Vec::new(),
        });
    };

    let mut selected = vec![false; positioned.len()];
    loop {
        selected[end] = true;
        let Some(parent) = previous[end] else {
            break;
        };
        end = parent;
    }

    let mut main_chain = Vec::new();
    let mut move_candidates = Vec::new();
    for (index, (anchor, _, _)) in positioned.into_iter().enumerate() {
        if selected[index] {
            main_chain.push(anchor);
        } else {
            move_candidates.push(anchor);
        }
    }

    Ok(MonotoneAnchorChain {
        main_chain,
        move_candidates,
    })
}

#[derive(Clone, Copy)]
struct ChainTip {
    length: usize,
    position: usize,
}

fn query_chain_tip(tree: &[Option<ChainTip>], mut end: usize) -> Option<ChainTip> {
    let mut best = None;
    while end > 0 {
        best = preferred_chain_tip(best, tree[end]);
        end &= end - 1;
    }
    best
}

fn update_chain_tip(tree: &mut [Option<ChainTip>], mut index: usize, tip: ChainTip) {
    while index < tree.len() {
        tree[index] = preferred_chain_tip(tree[index], Some(tip));
        let lsb = index & (!index + 1);
        index += lsb;
    }
}

fn preferred_chain_tip(current: Option<ChainTip>, candidate: Option<ChainTip>) -> Option<ChainTip> {
    match (current, candidate) {
        (None, candidate) => candidate,
        (current, None) => current,
        (Some(current), Some(candidate))
            if candidate.length > current.length
                || candidate.length == current.length && candidate.position < current.position =>
        {
            Some(candidate)
        }
        (current, Some(_)) => current,
    }
}

/// Partitions old and new block sequences into bounded alignment interval windows around the monotone main chain.
pub fn partition_anchor_windows(
    main_chain: &[ExactAnchor],
    old: &[BlockFeatures],
    new: &[BlockFeatures],
) -> Result<Vec<AnchorIntervalWindow>> {
    let old_indices = old
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let new_indices = new
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();

    let mut windows = Vec::with_capacity(main_chain.len() + 1);
    let mut old_start = 0;
    let mut new_start = 0;
    let mut prev_anchor: Option<ExactAnchor> = None;

    for &anchor in main_chain {
        let old_anchor = old_indices.get(&anchor.old).copied().ok_or_else(|| {
            Error::Unresolved(format!(
                "main chain anchor references unknown old block {}",
                anchor.old.0
            ))
        })?;
        let new_anchor = new_indices.get(&anchor.new).copied().ok_or_else(|| {
            Error::Unresolved(format!(
                "main chain anchor references unknown new block {}",
                anchor.new.0
            ))
        })?;

        if old_anchor < old_start || new_anchor < new_start {
            return Err(Error::Unresolved(
                "main chain violated monotonicity during window partitioning".to_owned(),
            ));
        }

        windows.push(AnchorIntervalWindow {
            old_range: (old_start, old_anchor),
            new_range: (new_start, new_anchor),
            left_anchor: prev_anchor,
            right_anchor: Some(anchor),
        });

        old_start = old_anchor + 1;
        new_start = new_anchor + 1;
        prev_anchor = Some(anchor);
    }

    windows.push(AnchorIntervalWindow {
        old_range: (old_start, old.len()),
        new_range: (new_start, new.len()),
        left_anchor: prev_anchor,
        right_anchor: None,
    });

    Ok(windows)
}

fn exact_token_counts(
    features: &[BlockFeatures],
    min_token_count: usize,
) -> HashMap<Vec<ComparableToken>, usize> {
    let mut counts = HashMap::new();
    for features in features.iter().filter(|features| {
        !features.has_normalization_issues && features.canonical_tokens.len() >= min_token_count
    }) {
        *counts.entry(features.canonical_tokens.clone()).or_default() += 1;
    }
    counts
}
