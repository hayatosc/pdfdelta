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
        if !windows(side, width, remaining, |hash, occurrence| {
            if occurrence.start % width == 0 && seeds.len() < limit {
                seeds
                    .try_reserve(1)
                    .map_err(|_| super::super::allocation_error("local anchor seeds"))?;
                seeds.entry(hash).or_default();
            }
            Ok(())
        })? {
            return Ok(None);
        }
    }
    for (side, views) in views.into_iter().enumerate() {
        if !windows(views, width, remaining, |hash, occurrence| {
            if let Some(seed) = seeds.get_mut(&hash) {
                let summary = if side == 0 {
                    &mut seed.old
                } else {
                    &mut seed.new
                };
                summary.count = summary.count.saturating_add(1).min(2);
                summary.first.get_or_insert(occurrence);
            }
            Ok(())
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

fn token_hash(token: &ComparableToken) -> u64 {
    let mut hash = DefaultHasher::new();
    token.hash(&mut hash);
    hash.finish()
}

fn windows(
    views: &[View],
    width: usize,
    remaining: &mut usize,
    mut visit: impl FnMut(u64, Occurrence) -> Result<()>,
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
            visit(
                hash,
                Occurrence {
                    view_index,
                    start,
                    end: start + width,
                },
            )?;
        }
    }
    Ok(true)
}
