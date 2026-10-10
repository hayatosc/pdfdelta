use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use super::{AnchorHit, Occurrence, OccurrenceSummary, View, charge, unique_occurrence};
use crate::{Result, normalize::ComparableToken};

#[derive(Default)]
struct Seed {
    old: OccurrenceSummary,
    new: OccurrenceSummary,
}

/// Samples bounded anchor candidates, then counts every occurrence of each
/// retained fingerprint on both sides. Fingerprints never establish equality:
/// collisions either reject a candidate or fail the final token comparison.
pub(super) fn discover(
    views: [&[View]; 2],
    width: usize,
    remaining: &mut usize,
    limit: usize,
) -> Result<Option<Vec<AnchorHit>>> {
    let mut seeds = HashMap::<u64, Seed>::new();
    for side in views {
        if !windows(side, width, remaining, |hash, occurrence, _work| {
            if occurrence.start % width == 0 && seeds.len() < limit {
                seeds
                    .try_reserve(1)
                    .map_err(|_| super::super::allocation_error("local anchor seeds"))?;
                seeds.entry(hash).or_default();
            }
            Ok(true)
        })? {
            return Ok(None);
        }
    }
    for (side, views) in views.into_iter().enumerate() {
        if !windows(views, width, remaining, |hash, occurrence, _work| {
            if let Some(seed) = seeds.get_mut(&hash) {
                let summary = if side == 0 {
                    &mut seed.old
                } else {
                    &mut seed.new
                };
                summary.count = summary.count.saturating_add(1).min(2);
                summary.first.get_or_insert(occurrence);
            }
            Ok(true)
        })? {
            return Ok(None);
        }
    }
    let mut hits = Vec::new();
    hits.try_reserve_exact(seeds.len())
        .map_err(|_| super::super::allocation_error("local anchor hits"))?;
    for seed in seeds.into_values() {
        let (Some(old), Some(new)) = (unique_occurrence(seed.old), unique_occurrence(seed.new))
        else {
            continue;
        };
        let old_tokens = &views[0][old.view_index].group.tokens[old.start..old.end];
        let new_tokens = &views[1][new.view_index].group.tokens[new.start..new.end];
        let Some(equal) = super::super::tokens_equal_with_budget(old_tokens, new_tokens, remaining)
        else {
            return Ok(None);
        };
        if equal {
            hits.push(AnchorHit {
                input_index: usize::MAX,
                old_view: old.view_index,
                new_view: new.view_index,
                old_start: old.start,
                old_end: old.end,
                new_start: new.start,
                new_end: new.end,
            });
        }
    }
    hits.sort_unstable_by_key(|hit| (hit.old_view, hit.new_view, hit.old_start, hit.new_start));
    Ok(Some(hits))
}

/// Searches every start at the original width, using only a deferred budget.
/// Retention limits may reduce candidates; occurrence counts still cover all
/// views. Every retained match needs both global uniqueness and token equality.
pub(super) fn discover_all_starts(
    views: [&[View]; 2],
    width: usize,
    remaining: &mut usize,
    limit: usize,
) -> Result<Option<Vec<AnchorHit>>> {
    const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
    if width == 0 || !charge(remaining, views[0].len().saturating_add(views[1].len())) {
        return Ok(None);
    }
    let Some(windows_count) = views
        .into_iter()
        .flatten()
        .try_fold(0_usize, |total, view| {
            total.checked_add(
                view.group
                    .tokens
                    .len()
                    .checked_sub(width)
                    .map_or(0, |n| n + 1),
            )
        })
    else {
        return Ok(None);
    };
    let retained = windows_count.min(limit);
    if retained == 0 {
        return Ok(Some(Vec::new()));
    }
    // Twice the next power of two covers hash-table spare capacity and
    // control bytes; the hit list is reserved once and never grows again.
    let Some(slots) = retained
        .checked_next_power_of_two()
        .and_then(|n| n.checked_mul(2))
    else {
        return Ok(None);
    };
    let slot_bytes = std::mem::size_of::<(u64, Seed)>() + 1;
    let Some(bytes) = slots.checked_mul(slot_bytes).and_then(|n| {
        retained
            .checked_mul(std::mem::size_of::<AnchorHit>())
            .and_then(|hits| n.checked_add(hits))
    }) else {
        return Ok(None);
    };
    // Charge initialization and traversal before either allocation. Moving
    // each retained entry out of the map is included in this allowance.
    let setup = bytes.checked_add(retained.saturating_mul(2));
    if bytes > MEMORY_LIMIT || setup.is_none_or(|n| !charge(remaining, n)) {
        return Ok(None);
    }
    let mut seeds = HashMap::<u64, Seed>::new();
    if seeds.try_reserve(retained).is_err() || seeds.capacity() > slots {
        return Ok(None);
    }
    let mut hits = Vec::new();
    if hits.try_reserve_exact(retained).is_err() || hits.capacity() > retained {
        return Ok(None);
    }
    for side in views {
        if !windows(side, width, remaining, |hash, _, work| {
            if !charge(work, 1) {
                return Ok(false);
            }
            if seeds.len() < retained {
                seeds.entry(hash).or_default();
            }
            Ok(true)
        })? {
            return Ok(None);
        }
    }
    for (side, side_views) in views.into_iter().enumerate() {
        if !windows(side_views, width, remaining, |hash, occurrence, work| {
            if !charge(work, 1) {
                return Ok(false);
            }
            if let Some(seed) = seeds.get_mut(&hash) {
                let summary = if side == 0 {
                    &mut seed.old
                } else {
                    &mut seed.new
                };
                summary.count = summary.count.saturating_add(1).min(2);
                summary.first.get_or_insert(occurrence);
            }
            Ok(true)
        })? {
            return Ok(None);
        }
    }
    for seed in seeds.into_values() {
        let (Some(old), Some(new)) = (unique_occurrence(seed.old), unique_occurrence(seed.new))
        else {
            continue;
        };
        let old_tokens = &views[0][old.view_index].group.tokens[old.start..old.end];
        let new_tokens = &views[1][new.view_index].group.tokens[new.start..new.end];
        let Some(equal) = super::super::tokens_equal_with_budget(old_tokens, new_tokens, remaining)
        else {
            return Ok(None);
        };
        if equal {
            hits.push(AnchorHit {
                input_index: usize::MAX,
                old_view: old.view_index,
                new_view: new.view_index,
                old_start: old.start,
                old_end: old.end,
                new_start: new.start,
                new_end: new.end,
            });
        }
    }
    // Insertion sorting has a deterministic pair-count bound. Reserve every
    // possible comparison and fixed-size swap before starting; the recorded
    // work stays independent of randomized hash-table iteration order.
    let sort_cost = hits
        .len()
        .checked_mul(hits.len().saturating_sub(1))
        .and_then(|n| n.checked_mul(10));
    if sort_cost.is_none_or(|cost| !charge(remaining, cost)) {
        return Ok(None);
    }
    let key = |hit: &AnchorHit| (hit.old_view, hit.new_view, hit.old_start, hit.new_start);
    for i in 1..hits.len() {
        let mut j = i;
        while j > 0 && key(&hits[j - 1]) > key(&hits[j]) {
            hits.swap(j - 1, j);
            j -= 1;
        }
    }
    Ok(Some(hits))
}

fn token_hash(token: &ComparableToken) -> u64 {
    let mut hash = DefaultHasher::new();
    token.hash(&mut hash);
    hash.finish()
}

fn windows(
    views: &[View],
    width: usize,
    remaining: &mut usize,
    mut visit: impl FnMut(u64, Occurrence, &mut usize) -> Result<bool>,
) -> Result<bool> {
    if width == 0 || !charge(remaining, width) {
        return Ok(false);
    }
    const BASE: u64 = 0x9e37_79b1_85eb_ca87;
    let power = (1..width).fold(1_u64, |power, _| power.wrapping_mul(BASE));
    for (view_index, view) in views.iter().enumerate() {
        let tokens = &view.group.tokens;
        if tokens.len() < width {
            continue;
        }
        if !charge(remaining, width) {
            return Ok(false);
        }
        let mut hash = tokens[..width].iter().fold(0_u64, |hash, token| {
            hash.wrapping_mul(BASE).wrapping_add(token_hash(token))
        });
        for start in 0..=tokens.len() - width {
            if !charge(remaining, 3) {
                return Ok(false);
            }
            if start > 0 {
                hash = hash
                    .wrapping_sub(token_hash(&tokens[start - 1]).wrapping_mul(power))
                    .wrapping_mul(BASE)
                    .wrapping_add(token_hash(&tokens[start + width - 1]));
            }
            if !visit(
                hash,
                Occurrence {
                    view_index,
                    start,
                    end: start + width,
                },
                remaining,
            )? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Batched global occurrence search for explicit equal anchors.
///
/// Each query supplies one equal needle (the caller has already verified
/// `old == new`). Long needles (full length >= `width`) are grouped by the
/// rolling-hash fingerprint of their first `width` tokens and found in one
/// scan per side with the existing rolling windows; every fingerprint hit is
/// exact-verified for the complete needle tokens with charged work. Shorter
/// needles fall back to the existing KMP `search_views`. Fingerprint
/// collisions only cost work or reject a candidate, never prove equality.
/// Occurrences are counted across all views (untrusted included) capped at
/// two, retaining the first in `(view, start)` order. Budget exhaustion
/// returns `Ok(None)` and never fabricates matches.
pub(super) fn search_explicit_anchors(
    views: [&[View]; 2],
    needles: &[(usize, &[ComparableToken])],
    width: usize,
    remaining: &mut usize,
) -> Result<Option<Vec<(usize, OccurrenceSummary, OccurrenceSummary)>>> {
    search_explicit_anchors_with_limit(
        views,
        needles,
        width,
        remaining,
        super::EXPLICIT_ANCHOR_RETAINED_LIMIT,
    )
}

pub(super) fn search_explicit_anchors_with_limit(
    views: [&[View]; 2],
    needles: &[(usize, &[ComparableToken])],
    width: usize,
    remaining: &mut usize,
    retained_limit: usize,
) -> Result<Option<Vec<(usize, OccurrenceSummary, OccurrenceSummary)>>> {
    if needles.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let setup_charge = needles.len().saturating_mul(4);
    if !charge(remaining, setup_charge) {
        return Ok(None);
    }
    let mut summed_retained_bytes = 0usize;
    for (_, needle) in needles {
        if !charge(remaining, needle.len()) {
            return Ok(None);
        }
        let Some(query_bytes) = super::explicit_anchor_retained_bytes(needle, needle.len(), 1)
        else {
            return Ok(None);
        };
        summed_retained_bytes = match summed_retained_bytes.checked_add(query_bytes) {
            Some(total) if total <= retained_limit => total,
            _ => return Ok(None),
        };
    }
    let mut results = Vec::new();
    results
        .try_reserve_exact(needles.len())
        .map_err(|_| super::super::allocation_error("explicit anchor results"))?;
    for (input_index, needle) in needles {
        results.push((
            *input_index,
            OccurrenceSummary::default(),
            OccurrenceSummary::default(),
        ));
        if width == 0 || needle.is_empty() || needle.len() < width {
            let old = super::search_views(views[0], needle, remaining)?;
            let new = super::search_views(views[1], needle, remaining)?;
            let (Some(old), Some(new)) = (complete(old), complete(new)) else {
                return Ok(None);
            };
            let last = results.last_mut().expect("result was just pushed");
            last.1 = old;
            last.2 = new;
        }
    }
    if width == 0 {
        return Ok(Some(results));
    }
    let long_count = needles
        .iter()
        .filter(|(_, needle)| needle.len() >= width)
        .count();
    if long_count == 0 {
        return Ok(Some(results));
    }
    let setup = long_count.saturating_add(1);
    if !charge(remaining, setup) {
        return Ok(None);
    }
    let mut long = Vec::new();
    long.try_reserve_exact(long_count)
        .map_err(|_| super::super::allocation_error("explicit anchor query list"))?;
    for (index, (_, needle)) in needles.iter().enumerate() {
        if needle.len() >= width {
            long.push(index);
        }
    }
    let mut buckets = HashMap::<u64, Vec<usize>>::new();
    buckets
        .try_reserve(long.len())
        .map_err(|_| super::super::allocation_error("explicit anchor buckets"))?;
    for index in long {
        let (_, needle) = &needles[index];
        let hash = prefix_hash(&needle[..width]);
        let bucket = buckets.entry(hash).or_default();
        bucket
            .try_reserve(1)
            .map_err(|_| super::super::allocation_error("explicit anchor bucket"))?;
        bucket.push(index);
    }
    for (side, side_views) in views.into_iter().enumerate() {
        if !windows(side_views, width, remaining, |hash, occurrence, work| {
            let Some(indices) = buckets.get(&hash) else {
                return Ok(true);
            };
            for index in indices {
                if !charge(work, 1) {
                    return Ok(false);
                }
                let (_, needle) = &needles[*index];
                if needle.len() < width {
                    continue;
                }
                let summary_full = if side == 0 {
                    results[*index].1.count >= 2
                } else {
                    results[*index].2.count >= 2
                };
                if summary_full {
                    continue;
                }
                let tokens = &side_views[occurrence.view_index].group.tokens;
                let Some(end) = occurrence.start.checked_add(needle.len()) else {
                    continue;
                };
                if end > tokens.len() {
                    continue;
                }
                // The query visit above already charges the length/bounds
                // check. Charge only inspected tokens, so a hash-prefix hit
                // that disagrees early does not pay for an unvisited suffix.
                let mut equal = true;
                for (actual, expected) in tokens[occurrence.start..end].iter().zip(*needle) {
                    if !charge(work, 1) {
                        return Ok(false);
                    }
                    if actual != expected {
                        equal = false;
                        break;
                    }
                }
                if !equal {
                    continue;
                }
                let summary = if side == 0 {
                    &mut results[*index].1
                } else {
                    &mut results[*index].2
                };
                summary.count = summary.count.saturating_add(1).min(2);
                summary.first.get_or_insert(Occurrence {
                    view_index: occurrence.view_index,
                    start: occurrence.start,
                    end,
                });
            }
            Ok(true)
        })? {
            return Ok(None);
        }
    }
    Ok(Some(results))
}

fn complete(result: super::SearchResult) -> Option<OccurrenceSummary> {
    match result {
        super::SearchResult::Complete(summary) => Some(summary),
        super::SearchResult::BudgetExceeded => None,
    }
}

fn prefix_hash(tokens: &[ComparableToken]) -> u64 {
    const BASE: u64 = 0x9e37_79b1_85eb_ca87;
    tokens.iter().fold(0_u64, |hash, token| {
        hash.wrapping_mul(BASE).wrapping_add(token_hash(token))
    })
}
