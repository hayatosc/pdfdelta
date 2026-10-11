use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use super::{AnchorHit, Occurrence, OccurrenceSummary, View, charge, unique_occurrence};
use crate::{Result, normalize::ComparableToken};

/// Provenance occupies the original input-index slot. Only the completed
/// global scanners below can create a seed witness; source ends are distinct.
#[derive(Clone, Copy)]
pub(super) struct AnchorOrigin(usize);

pub(super) struct GlobalSeedWitness(AnchorHit);

impl AnchorOrigin {
    fn completed_global_seed() -> Self {
        Self(usize::MAX)
    }

    /// # Panics
    /// Panics if an input index occupies either reserved provenance value.
    pub(super) fn explicit(index: usize) -> Self {
        assert!(index < usize::MAX - 1);
        Self(index)
    }

    pub(super) fn source_end() -> Self {
        Self(usize::MAX - 1)
    }

    pub(super) fn query_index(self) -> Option<usize> {
        (self.0 < usize::MAX - 1).then_some(self.0)
    }

    pub(super) fn sort_key(self) -> usize {
        self.query_index().unwrap_or(usize::MAX)
    }
}

/// The sealed witness carries the same anchor whose completed provenance was
/// checked; callers cannot substitute an unrelated span for a valid witness.
pub(super) fn completed_seed(hit: AnchorHit) -> Option<GlobalSeedWitness> {
    (hit.input_index.0 == usize::MAX).then_some(GlobalSeedWitness(hit))
}
impl GlobalSeedWitness {
    pub(super) fn anchor(self) -> AnchorHit {
        self.0
    }
}

#[derive(Default)]
struct Seed {
    old: OccurrenceSummary,
    new: OccurrenceSummary,
}

/// The deferred search has one fixed width, so each end is implicit. A zero
/// count marks an absent occurrence without an additional optional payload.
#[derive(Default)]
struct DeferredOccurrence {
    view_index: usize,
    start: usize,
    count: u8,
}

impl DeferredOccurrence {
    fn observe(&mut self, occurrence: Occurrence) {
        if self.count == 0 {
            self.view_index = occurrence.view_index;
            self.start = occurrence.start;
        }
        self.count = self.count.saturating_add(1).min(2);
    }

    fn unique(self, width: usize) -> Option<Occurrence> {
        (self.count == 1).then(|| Occurrence {
            view_index: self.view_index,
            start: self.start,
            // Every start was checked by the unchanged rolling-window scan.
            end: self.start + width,
        })
    }
}

#[derive(Default)]
struct DeferredSeed {
    old: DeferredOccurrence,
    new: DeferredOccurrence,
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
                input_index: AnchorOrigin::completed_global_seed(),
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
    // Twice the next power of two, including the table's minimum allocation,
    // covers spare capacity and control bytes. The hit list never grows again.
    let Some(slots) = retained
        .checked_next_power_of_two()
        .and_then(|n| n.checked_mul(2))
        .map(|n| n.max(4))
    else {
        return Ok(None);
    };
    let slot_bytes = std::mem::size_of::<(u64, DeferredSeed)>() + 1;
    // Keep conservative control-tail, alignment and allocator headroom for
    // both allocations, including tables smaller than a SIMD control group.
    let Some(bytes) = slots
        .checked_mul(slot_bytes)
        .and_then(|n| n.checked_add(256))
        .and_then(|n| {
            retained
                .checked_mul(std::mem::size_of::<AnchorHit>())
                .and_then(|hits| n.checked_add(hits))
        })
    else {
        return Ok(None);
    };
    // Charge initialization and traversal before either allocation. Moving
    // each retained entry out of the map and reconstructing its two implicit
    // ends are included in this allowance.
    let setup = bytes.checked_add(retained.saturating_mul(6));
    if bytes > MEMORY_LIMIT || setup.is_none_or(|n| !charge(remaining, n)) {
        return Ok(None);
    }
    let mut seeds = HashMap::<u64, DeferredSeed>::new();
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
                if !charge(work, 3) {
                    return Ok(false);
                }
                summary.observe(occurrence);
            }
            Ok(true)
        })? {
            return Ok(None);
        }
    }
    for seed in seeds.into_values() {
        let (Some(old), Some(new)) = (seed.old.unique(width), seed.new.unique(width)) else {
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
                input_index: AnchorOrigin::completed_global_seed(),
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
/// exact-verified for the complete needle tokens with charged work. Repeated
/// scalar needles reuse completed two-sided counts. Four or more shorter
/// scalar-prefix needles share an exact prefix dispatch; small batches and
/// opaque prefixes retain KMP. Fingerprint collisions only cost work or
/// reject a candidate, never prove equality.
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
    let mut scalar_queries = 0usize;
    let mut wide_short_queries = 0usize;
    for (_, needle) in needles {
        if needle.len() > 1 && needle.len() < width && needle[0].is_scalar() {
            wide_short_queries += 1;
        }
        if width > 1 && matches!(needle, [ComparableToken::Scalar(_)]) {
            scalar_queries += 1;
        }
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
    // A scalar cache stores only fixed-size keys and completed capped counts.
    // At least two queries amortize the minimum hash-table allocation within
    // the existing 512-byte/query metadata bound; no payload limit is reset.
    let mut scalar_cache = HashMap::<char, [OccurrenceSummary; 2]>::new();
    if scalar_queries >= 2 {
        scalar_cache
            .try_reserve(scalar_queries)
            .map_err(|_| super::super::allocation_error("scalar anchor cache"))?;
        if scalar_cache.capacity() > scalar_queries.saturating_mul(2) {
            return Ok(None);
        }
    }
    for (input_index, needle) in needles {
        results.push((
            *input_index,
            OccurrenceSummary::default(),
            OccurrenceSummary::default(),
        ));
        if scalar_queries >= 2
            && let [ComparableToken::Scalar(scalar)] = needle
        {
            // The original two KMP searches each prepay one scalar prefix.
            // Spend the same two units on the fixed-size cache lookup/insert;
            // a miss scans the identical views in the identical token order.
            if !charge(remaining, 2) {
                return Ok(None);
            }
            let summaries = if let Some(summaries) = scalar_cache.get(scalar) {
                *summaries
            } else {
                let Some(old) = search_scalar(views[0], *scalar, remaining) else {
                    return Ok(None);
                };
                let Some(new) = search_scalar(views[1], *scalar, remaining) else {
                    return Ok(None);
                };
                let summaries = [old, new];
                scalar_cache.insert(*scalar, summaries);
                summaries
            };
            let last = results.last_mut().expect("result was just pushed");
            last.1 = summaries[0];
            last.2 = summaries[1];
        } else if wide_short_queries >= 4
            && needle.len() > 1
            && needle.len() < width
            && needle[0].is_scalar()
        {
            // Filled by one source-ordered scan after the scalar queries.
        } else if width == 0 || needle.is_empty() || needle.len() < width {
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
    if wide_short_queries >= 4
        && !search_short_prefixes(
            views,
            needles,
            width,
            wide_short_queries,
            &mut results,
            remaining,
        )?
    {
        return Ok(None);
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

/// Multiple short lengths share a first-token dispatch, followed by complete
/// token comparison. Hash collisions never establish a match. The last three
/// unfinished queries resume KMP at the exact checked start-position cut.
fn search_short_prefixes(
    views: [&[View]; 2],
    needles: &[(usize, &[ComparableToken])],
    width: usize,
    count: usize,
    results: &mut [(usize, OccurrenceSummary, OccurrenceSummary)],
    remaining: &mut usize,
) -> Result<bool> {
    #[derive(Default)]
    struct Bucket {
        indices: Vec<usize>,
        active: usize,
    }
    // Includes metadata initialization, first-token hashing and insertion.
    if !charge(remaining, count.saturating_mul(5).saturating_add(1)) {
        return Ok(false);
    }
    let mut buckets = HashMap::<char, Bucket>::new();
    buckets
        .try_reserve(count)
        .map_err(|_| super::super::allocation_error("short prefix buckets"))?;
    if buckets.capacity() > count.saturating_mul(2) {
        return Ok(false);
    }
    for (index, (_, needle)) in needles.iter().enumerate() {
        if needle.len() <= 1 || needle.len() >= width || !needle[0].is_scalar() {
            continue;
        }
        let bucket = buckets
            .entry(needle[0].as_scalar().expect("scalar prefix admitted"))
            .or_default();
        bucket
            .indices
            .try_reserve(1)
            .map_err(|_| super::super::allocation_error("short prefix indices"))?;
        bucket.indices.push(index);
    }
    for (side, side_views) in views.into_iter().enumerate() {
        let mut active = count;
        if !charge(remaining, buckets.len().saturating_add(1)) {
            return Ok(false);
        }
        for bucket in buckets.values_mut() {
            bucket.active = bucket.indices.len();
        }
        'scan: {
            for (view_index, view) in side_views.iter().enumerate() {
                for start in 0..view.group.tokens.len() {
                    if active == 0 {
                        break 'scan;
                    }
                    if active <= 3 {
                        // Preserve the original query slots, including repeated IDs.
                        if !charge(remaining, needles.len()) {
                            return Ok(false);
                        }
                        for (index, (_, needle)) in needles.iter().enumerate() {
                            if needle.len() <= 1 || needle.len() >= width || !needle[0].is_scalar()
                            {
                                continue;
                            }
                            let summary = if side == 0 {
                                results[index].1
                            } else {
                                results[index].2
                            };
                            if summary.count >= 2 {
                                continue;
                            }
                            let Some(summary) = complete(super::search_views_from(
                                side_views,
                                needle,
                                (view_index, start),
                                summary,
                                remaining,
                            )?) else {
                                return Ok(false);
                            };
                            if side == 0 {
                                results[index].1 = summary;
                            } else {
                                results[index].2 = summary;
                            }
                        }
                        break 'scan;
                    }
                    // One source-token visit and one fixed-size character lookup;
                    // opaque font payloads are never hashed by this dispatch.
                    if !charge(remaining, 1) {
                        return Ok(false);
                    }
                    let Some(scalar) = view.group.tokens[start].as_scalar() else {
                        continue;
                    };
                    if !charge(remaining, 1) {
                        return Ok(false);
                    }
                    let Some(bucket) = buckets.get_mut(&scalar) else {
                        continue;
                    };
                    if !charge(remaining, 1) {
                        return Ok(false);
                    }
                    if bucket.active == 0 {
                        continue;
                    }
                    for &index in &bucket.indices {
                        if !charge(remaining, 1) {
                            return Ok(false);
                        }
                        let summary = if side == 0 {
                            &mut results[index].1
                        } else {
                            &mut results[index].2
                        };
                        if summary.count >= 2 {
                            continue;
                        }
                        let needle = needles[index].1;
                        let Some(end) = start
                            .checked_add(needle.len())
                            .filter(|&end| end <= view.group.tokens.len())
                        else {
                            continue;
                        };
                        let mut equal = true;
                        for (actual, expected) in view.group.tokens[start..end].iter().zip(needle) {
                            if !charge(remaining, 1) {
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
                        summary.count += 1;
                        summary.first.get_or_insert(Occurrence {
                            view_index,
                            start,
                            end,
                        });
                        if summary.count == 2 {
                            if !charge(remaining, 2) {
                                return Ok(false);
                            }
                            bucket.active -= 1;
                            active -= 1;
                        }
                    }
                }
            }
        }
    }
    Ok(true)
}

/// Single-scalar KMP with its two-sided prefix cost prepaid by the caller.
/// Each inspected token costs the same one comparison as `search_views`;
/// every view, including untrusted ones, participates until count reaches two.
fn search_scalar(views: &[View], scalar: char, remaining: &mut usize) -> Option<OccurrenceSummary> {
    let mut summary = OccurrenceSummary::default();
    for (view_index, view) in views.iter().enumerate() {
        for (start, token) in view.group.tokens.iter().enumerate() {
            if !charge(remaining, 1) {
                return None;
            }
            if *token == ComparableToken::Scalar(scalar) {
                summary.count += 1;
                summary.first.get_or_insert(Occurrence {
                    view_index,
                    start,
                    end: start + 1,
                });
                if summary.count == 2 {
                    return Some(summary);
                }
            }
        }
    }
    Some(summary)
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
