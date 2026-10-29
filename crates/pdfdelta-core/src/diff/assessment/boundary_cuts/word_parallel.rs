//! The same forbidden-edge predicate with word-sized LCS row differences.
//!
//! A bit at column j marks a one-unit increase in that row's prefix score.
//! Removing the two edges adjacent to a cut changes only the matching mask;
//! it never selects a preferred optimal matching or proves source closure.

use std::{cmp::Ordering, ops::Range};

use super::{MAX_MEMORY_BYTES, Outcome, charge, invalid, paid_equal};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug)]
struct Plan {
    words: usize,
    work: usize,
    bytes: usize,
}

/// Selects a bounded word implementation only when its complete prepaid work
/// is lower than the scalar implementation's existing reservation.
///
/// Preparation includes sorting, symbol lookup, masks and all live buffers.
/// The worst-case alphabet has one symbol per new token. Budget refusal,
/// allocation failure and an unsupported shape cannot yield a proof verdict.
pub(in super::super) fn certify<T: Ord>(
    old: &[T],
    new: &[T],
    target: &[Range<usize>; 2],
    remaining: &mut usize,
) -> Result<Outcome> {
    let cuts = [
        [target[0].start, target[1].start],
        [target[0].end, target[1].end],
    ];
    let count = cuts
        .iter()
        .filter(|&&cut| cut != [0, 0] && cut != [old.len(), new.len()])
        .count();
    let Some(plan) = selected_plan(old.len(), new.len(), count, MAX_MEMORY_BYTES) else {
        return super::certify(old, new, target, remaining);
    };
    certify_planned(old, new, target, remaining, plan)
}

fn selected_plan(m: usize, n: usize, count: usize, limit: usize) -> Option<Plan> {
    if m < 64 || n < 64 || count == 0 {
        return None;
    }
    let plan = plan(m, n, count + 1)?;
    let columns = n.checked_add(1)?;
    let scalar = m
        .checked_mul(n)?
        .checked_mul(count + 1)?
        .checked_add(columns.checked_mul(count + 3)?)?
        .checked_add(m.checked_mul(count + 1)?)?;
    (plan.bytes <= limit && plan.work < scalar).then_some(plan)
}

fn plan(m: usize, n: usize, passes: usize) -> Option<Plan> {
    let words = n.checked_add(63)? / 64;
    let levels = if n <= 1 {
        0
    } else {
        (n - 1).ilog2() as usize + 1
    };
    let mask_slots = n.checked_mul(words)?;
    let index_slots = n.checked_mul(3)?.checked_add(m)?;
    let bytes = index_slots
        .checked_mul(std::mem::size_of::<usize>())?
        .checked_add(mask_slots.checked_add(words)?.checked_mul(8)?)?;
    // Index initialization 2N; merge comparisons/moves 2NL; group scans,
    // symbol entries and mask writes 8N; row lookup/record M(L+3).
    // Each score pass pays reset/popcount 3W, row setup 8M and 24 per word
    // for loads, the forbidden bit, shift carry, two subtractions and borrow.
    let setup = 64usize
        .checked_add(n.checked_mul(2)?)?
        .checked_add(n.checked_mul(levels)?.checked_mul(2)?)?
        .checked_add(n.checked_mul(8)?)?
        .checked_add(m.checked_mul(levels.checked_add(3)?)?)?
        .checked_add(mask_slots)?
        .checked_add(words)?;
    let per_pass = words
        .checked_mul(3)?
        .checked_add(m.checked_mul(8)?)?
        .checked_add(m.checked_mul(words)?.checked_mul(24)?)?;
    let work = setup.checked_add(passes.checked_mul(per_pass)?)?;
    Some(Plan { words, work, bytes })
}

fn certify_planned<T: Ord>(
    old: &[T],
    new: &[T],
    target: &[Range<usize>; 2],
    remaining: &mut usize,
    plan: Plan,
) -> Result<Outcome> {
    let Some(a) = old.get(target[0].clone()) else {
        return Err(invalid("boundary cut exceeds the old token range"));
    };
    let Some(b) = new.get(target[1].clone()) else {
        return Err(invalid("boundary cut exceeds the new token range"));
    };
    for (a, b) in [(a, b), (old, new)] {
        match paid_equal(a, b, remaining) {
            None => return Ok(Outcome::BudgetExceeded),
            Some(true) => return Ok(Outcome::Rejected),
            Some(false) => {}
        }
    }
    if !charge(remaining, plan.work) {
        return Ok(Outcome::BudgetExceeded);
    }
    let (masks, rows) = prepare(old, new, plan.words)?;
    let mut state = filled(plan.words, 0u64)?;
    let optimum = score(&masks, &rows, None, &mut state);
    for cut in [
        [target[0].start, target[1].start],
        [target[0].end, target[1].end],
    ] {
        if cut != [0, 0]
            && cut != [old.len(), new.len()]
            && score(&masks, &rows, Some(cut), &mut state) == optimum
        {
            return Ok(Outcome::Rejected);
        }
    }
    Ok(Outcome::Proven)
}

fn filled<T: Clone>(length: usize, value: T) -> Result<Vec<T>> {
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .map_err(|_| Error::Unresolved("universal boundary word allocation".to_owned()))?;
    data.resize(length, value);
    Ok(data)
}

fn prepare<T: Ord>(old: &[T], new: &[T], words: usize) -> Result<(Vec<u64>, Vec<usize>)> {
    let mut indices = filled(new.len(), 0usize)?;
    for (index, slot) in indices.iter_mut().enumerate() {
        *slot = index;
    }
    let mut scratch = filled(new.len(), 0usize)?;
    let mut width = 1usize;
    while width < new.len() {
        let mut start = 0usize;
        while start < new.len() {
            let middle = start.saturating_add(width).min(new.len());
            let end = middle.saturating_add(width).min(new.len());
            let (mut left, mut right) = (start, middle);
            for slot in &mut scratch[start..end] {
                if right == end || (left < middle && new[indices[left]] <= new[indices[right]]) {
                    *slot = indices[left];
                    left += 1;
                } else {
                    *slot = indices[right];
                    right += 1;
                }
            }
            start = end;
        }
        std::mem::swap(&mut indices, &mut scratch);
        width = width.saturating_mul(2);
    }
    let mut symbols = Vec::new();
    symbols
        .try_reserve_exact(new.len())
        .map_err(|_| Error::Unresolved("universal boundary symbol allocation".to_owned()))?;
    for &index in &indices {
        if symbols
            .last()
            .is_none_or(|&previous| new[previous] != new[index])
        {
            symbols.push(index);
        }
    }
    let mut masks = filled(symbols.len() * words, 0u64)?;
    let mut group = 0;
    for (position, &index) in indices.iter().enumerate() {
        if position != 0 && new[indices[position - 1]] != new[index] {
            group += 1;
        }
        masks[group * words + index / 64] |= 1u64 << (index % 64);
    }
    let mut rows = filled(old.len(), usize::MAX)?;
    for (token, row) in old.iter().zip(&mut rows) {
        let (mut low, mut high) = (0, symbols.len());
        while low < high {
            let middle = low + (high - low) / 2;
            match new[symbols[middle]].cmp(token) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => {
                    *row = middle * words;
                    break;
                }
            }
        }
    }
    Ok((masks, rows))
}

fn score(masks: &[u64], rows: &[usize], avoid: Option<[usize; 2]>, state: &mut [u64]) -> usize {
    state.fill(0);
    for (i, &row) in rows.iter().enumerate() {
        let blocked = avoid.and_then(|[a, b]| {
            if i == a {
                Some(b)
            } else if i + 1 == a {
                b.checked_sub(1)
            } else {
                None
            }
        });
        let mut shift_carry = 1u64;
        let mut borrow = false;
        for (word, slot) in state.iter_mut().enumerate() {
            let mut matches = if row == usize::MAX {
                0
            } else {
                masks[row + word]
            };
            if let Some(column) = blocked.filter(|column| column / 64 == word) {
                matches &= !(1u64 << (column % 64));
            }
            let previous = *slot;
            let x = previous | matches;
            let y = (previous << 1) | shift_carry;
            shift_carry = previous >> 63;
            let (difference, first_borrow) = x.overflowing_sub(y);
            let (difference, second_borrow) = difference.overflowing_sub(u64::from(borrow));
            borrow = first_borrow || second_borrow;
            *slot = x & !difference;
        }
    }
    state.iter().map(|word| word.count_ones() as usize).sum()
}

#[cfg(test)]
mod tests;
