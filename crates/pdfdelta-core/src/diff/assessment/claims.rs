//! Bounded claims over all literal-minimal alignments.
//!
//! These routines quantify only the alignment policy named by their inputs.
//! They do not infer source correspondence, normalization, or extraction
//! completeness.

use std::{
    cmp::min,
    collections::{HashMap, hash_map::Entry},
    hash::Hash,
    mem::size_of,
};

#[cfg(test)]
use std::cell::Cell;

use crate::{Error, Result};

const MAX_CLAIM_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const CLAIM_CELLS_RESOURCE: &str = "claim DP cells";
const CLAIM_MEMORY_RESOURCE: &str = "claim DP memory";

#[cfg(test)]
thread_local! {
    static CLAIM_MEMORY_LIMIT: Cell<usize> = const { Cell::new(MAX_CLAIM_MEMORY_BYTES) };
}

/// Minimum and maximum selected-token changes across all optimal LCS paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CountBounds {
    pub(super) lower: usize,
    pub(super) upper: usize,
}

/// Positions that are unmatched in every optimal LCS path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MandatoryChanges {
    pub(super) old: Vec<bool>,
    pub(super) new: Vec<bool>,
    pub(super) lcs_length: usize,
}

/// Fused claims for one pair of token sequences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LiteralClaims {
    pub(super) source: CountBounds,
    pub(super) residual: CountBounds,
    pub(super) mandatory: MandatoryChanges,
}

/// Computes all literal-minimal source, residual, and mandatory claims together.
///
/// The exact insertion/deletion distance is found with a bit-parallel LCS
/// pass. Its distance defines a safe diagonal band: every optimal LCS path is
/// contained in that band. A dense grid is selected when the band is broad;
/// otherwise the same recurrence uses only band cells. Both paths retain all
/// optimal ties and therefore have the same claim semantics.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] for mismatched masks,
/// [`Error::LimitExceeded`] for overflowing dimensions or bounded memory, and
/// [`Error::Unresolved`] when a bounded allocation fails.
pub(super) fn literal_claims<T: Eq + Hash>(
    old: &[T],
    new: &[T],
    old_source: &[bool],
    new_source: &[bool],
    old_residual: &[bool],
    new_residual: &[bool],
    remaining_work: &mut usize,
) -> Result<Option<LiteralClaims>> {
    literal_claims_with_preference(
        old,
        new,
        old_source,
        new_source,
        old_residual,
        new_residual,
        remaining_work,
        None,
    )
}

/// Runs the production claim kernel with a selected DP layout for benchmarks.
#[cfg(test)]
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(super) fn literal_claims_for_benchmark<T: Eq + Hash>(
    old: &[T],
    new: &[T],
    old_source: &[bool],
    new_source: &[bool],
    old_residual: &[bool],
    new_residual: &[bool],
    remaining_work: &mut usize,
    force_band: bool,
) -> Result<Option<LiteralClaims>> {
    literal_claims_for_benchmark_with_limit(
        old,
        new,
        old_source,
        new_source,
        old_residual,
        new_residual,
        remaining_work,
        force_band,
        MAX_CLAIM_MEMORY_BYTES,
    )
}

/// Runs the production claim kernel with a selected layout and test-only limit.
#[cfg(test)]
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(super) fn literal_claims_for_benchmark_with_limit<T: Eq + Hash>(
    old: &[T],
    new: &[T],
    old_source: &[bool],
    new_source: &[bool],
    old_residual: &[bool],
    new_residual: &[bool],
    remaining_work: &mut usize,
    force_band: bool,
    memory_limit_bytes: usize,
) -> Result<Option<LiteralClaims>> {
    let previous_limit = CLAIM_MEMORY_LIMIT.with(|limit| limit.replace(memory_limit_bytes));
    let result = literal_claims_with_preference(
        old,
        new,
        old_source,
        new_source,
        old_residual,
        new_residual,
        remaining_work,
        Some(force_band),
    );
    CLAIM_MEMORY_LIMIT.with(|limit| limit.set(previous_limit));
    result
}

#[allow(clippy::too_many_arguments)]
fn literal_claims_with_preference<T: Eq + Hash>(
    old: &[T],
    new: &[T],
    old_source: &[bool],
    new_source: &[bool],
    old_residual: &[bool],
    new_residual: &[bool],
    remaining_work: &mut usize,
    forced_band: Option<bool>,
) -> Result<Option<LiteralClaims>> {
    validate_masks(old, new, old_source, new_source)?;
    validate_masks(old, new, old_residual, new_residual)?;
    let total = old
        .len()
        .checked_add(new.len())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let selected_source = selected_count(old_source, new_source)?;
    let selected_residual = selected_count(old_residual, new_residual)?;
    if selected_source > u32::MAX as usize || selected_residual > u32::MAX as usize {
        return Err(limit_error(CLAIM_CELLS_RESOURCE));
    }
    let linear_work = total
        .checked_mul(5)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    if !charge(remaining_work, linear_work) {
        return Ok(None);
    }

    if old.is_empty() || new.is_empty() {
        return direct_claims(old, new, selected_source, selected_residual, false).map(Some);
    }
    if old == new {
        return direct_claims(old, new, selected_source, selected_residual, true).map(Some);
    }

    let Some(lcs_length) = bitset_lcs(old, new, remaining_work)? else {
        return Ok(None);
    };
    let lcs_length_u32 =
        u32::try_from(lcs_length).map_err(|_| limit_error(CLAIM_CELLS_RESOURCE))?;
    let distance = total
        .checked_sub(
            lcs_length
                .checked_mul(2)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
        )
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let band = BandLayout::band(old.len(), new.len(), distance)?;
    let dense_cells = old.len().checked_add(1).and_then(|rows| {
        new.len()
            .checked_add(1)
            .and_then(|columns| rows.checked_mul(columns))
    });
    let use_band = if let Some(forced_band) = forced_band {
        forced_band
    } else {
        match dense_cells {
            Some(dense_cells) => match layout_memory_bytes(
                old.len()
                    .checked_add(1)
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                dense_cells,
                old.len(),
                new.len(),
            ) {
                Ok(dense_memory) => {
                    band.cell_count
                        .checked_mul(4)
                        .is_some_and(|band_cells| band_cells <= dense_cells)
                        || dense_memory > MAX_CLAIM_MEMORY_BYTES
                }
                Err(_) => true,
            },
            None => true,
        }
    };
    let layout = if use_band {
        band
    } else {
        BandLayout::dense(old.len(), new.len())?
    };
    ensure_memory(layout.memory_bytes()?)?;
    let mut grid = ClaimGrid::new(layout)?;
    let grid_work = grid
        .cells
        .len()
        .checked_mul(2)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    if !charge(remaining_work, grid_work) {
        return Ok(None);
    }
    let endpoint = forward_claims(
        &mut grid,
        old,
        new,
        old_source,
        new_source,
        old_residual,
        new_residual,
    )?;
    if endpoint.lcs != lcs_length_u32 {
        return Err(invalid(
            "fused claim grid disagrees with exact LCS distance",
        ));
    }
    let mandatory = mandatory_claims(&grid, old, new, lcs_length)?;
    Ok(Some(LiteralClaims {
        source: CountBounds {
            lower: selected_source
                .checked_sub(endpoint.source_max as usize)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
            upper: selected_source
                .checked_sub(endpoint.source_min as usize)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
        },
        residual: CountBounds {
            lower: selected_residual
                .checked_sub(endpoint.residual_max as usize)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
            upper: selected_residual
                .checked_sub(endpoint.residual_min as usize)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
        },
        mandatory,
    }))
}

/// Returns selected-token change bounds over every literal-minimal alignment.
///
/// A `true` entry in either mask selects one source position for the query.
/// A matched equal pair contributes one unchanged position for each selected
/// side; skipped positions contribute one changed position. Consequently, the
/// result can count a new-side insertion, an old-side deletion, or both sides
/// of a replacement. The primary objective is always maximum LCS length.
///
/// The dynamic program runs in `O(old.len() * new.len())` time and retains one
/// row of state. A query that cannot reserve its bounded memory or work budget
/// does not produce a claim and returns `Ok(None)`.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] when a mask length does not match
/// its side, [`Error::LimitExceeded`] for overflowing dimensions or the
/// configured memory ceiling, and [`Error::Unresolved`] when allocation fails.
#[cfg(test)]
pub(super) fn count_bounds<T: Eq>(
    old: &[T],
    new: &[T],
    old_mask: &[bool],
    new_mask: &[bool],
    remaining_work: &mut usize,
) -> Result<Option<CountBounds>> {
    validate_masks(old, new, old_mask, new_mask)?;
    let (_, columns, _) = dimensions(old.len(), new.len())?;
    let interior = old
        .len()
        .checked_mul(new.len())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let linear_work = old
        .len()
        .checked_add(new.len())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    if interior > 0 {
        ensure_memory(count_memory_bytes(columns)?)?;
    }
    if !charge(remaining_work, linear_work) {
        return Ok(None);
    }
    let selected = old_mask
        .iter()
        .filter(|selected| **selected)
        .count()
        .checked_add(new_mask.iter().filter(|selected| **selected).count())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;

    if interior == 0 {
        return Ok(Some(CountBounds {
            lower: selected,
            upper: selected,
        }));
    }
    if !charge(remaining_work, interior) {
        return Ok(None);
    }

    let mut row = zeroed_count_cells(columns)?;
    for old_index in 0..old.len() {
        let mut diagonal = CountCell::default();
        for new_count in 1..columns {
            let up = row[new_count];
            let left = row[new_count - 1];
            let mut best = combine(up, left);
            if old[old_index] == new[new_count - 1] {
                let matched = CountCell {
                    lcs: diagonal
                        .lcs
                        .checked_add(1)
                        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                    min_matched: diagonal
                        .min_matched
                        .checked_add(usize::from(old_mask[old_index]))
                        .and_then(|value| value.checked_add(usize::from(new_mask[new_count - 1])))
                        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                    max_matched: diagonal
                        .max_matched
                        .checked_add(usize::from(old_mask[old_index]))
                        .and_then(|value| value.checked_add(usize::from(new_mask[new_count - 1])))
                        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                };
                best = combine(best, matched);
            }
            row[new_count] = best;
            diagonal = up;
        }
    }

    let result = row[columns - 1];
    Ok(Some(CountBounds {
        lower: selected
            .checked_sub(result.max_matched)
            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
        upper: selected
            .checked_sub(result.min_matched)
            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
    }))
}

/// Returns source positions that cannot be matched by any optimal LCS.
///
/// The returned masks are side-local and have exactly the same lengths as the
/// input slices. `true` means that the position is changed on every optimal
/// path; `false` means that at least one optimal path matches it. This uses a
/// bounded forward table and a rolling backward row so the mandatory-position
/// proof remains separate from the row-only count query.
///
/// # Errors
///
/// Returns [`Error::LimitExceeded`] for overflowing dimensions or the
/// configured memory ceiling, and [`Error::Unresolved`] when allocation
/// fails. Budget exhaustion returns `Ok(None)` because no mandatory claim was
/// established.
#[cfg(test)]
pub(super) fn mandatory_changed<T: Eq>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
) -> Result<Option<MandatoryChanges>> {
    let (rows, columns, cells) = dimensions(old.len(), new.len())?;
    ensure_memory(mandatory_memory_bytes(old.len(), columns, cells)?)?;
    let linear_work = old
        .len()
        .checked_add(new.len())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    if !charge(remaining_work, linear_work) {
        return Ok(None);
    }
    let interior = old
        .len()
        .checked_mul(new.len())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let work = interior
        .checked_mul(2)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    if !charge(remaining_work, work) {
        return Ok(None);
    }

    let mut forward = zeroed_usizes(
        rows.checked_mul(columns)
            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
    )?;
    for old_count in 1..rows {
        let row_start = old_count * columns;
        let previous_start = (old_count - 1) * columns;
        for new_count in 1..columns {
            let up = forward[previous_start + new_count];
            let left = forward[row_start + new_count - 1];
            let best = if old[old_count - 1] == new[new_count - 1] {
                forward[previous_start + new_count - 1]
                    .checked_add(1)
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?
                    .max(up)
                    .max(left)
            } else {
                up.max(left)
            };
            forward[row_start + new_count] = best;
        }
    }

    let lcs_length = forward[(rows - 1) * columns + columns - 1];
    let mut old_changed = bools(old.len(), true)?;
    let mut new_changed = bools(new.len(), true)?;
    let mut suffix_next = zeroed_usizes(columns)?;
    let mut suffix_current = zeroed_usizes(columns)?;

    for old_index in (0..old.len()).rev() {
        suffix_current[columns - 1] = 0;
        for new_index in (0..new.len()).rev() {
            let down = suffix_next[new_index];
            let right = suffix_current[new_index + 1];
            let best = if old[old_index] == new[new_index] {
                suffix_next[new_index + 1]
                    .checked_add(1)
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?
                    .max(down)
                    .max(right)
            } else {
                down.max(right)
            };
            suffix_current[new_index] = best;

            if old[old_index] == new[new_index] {
                let prefix = forward[old_index * columns + new_index];
                let through = prefix
                    .checked_add(1)
                    .and_then(|value| value.checked_add(suffix_next[new_index + 1]))
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
                if through == lcs_length {
                    old_changed[old_index] = false;
                    new_changed[new_index] = false;
                }
            }
        }
        std::mem::swap(&mut suffix_next, &mut suffix_current);
    }

    Ok(Some(MandatoryChanges {
        old: old_changed,
        new: new_changed,
        lcs_length,
    }))
}

fn direct_claims<T>(
    old: &[T],
    new: &[T],
    selected_source: usize,
    selected_residual: usize,
    equal: bool,
) -> Result<LiteralClaims> {
    let output_length = old
        .len()
        .checked_add(new.len())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let output_bytes = output_length
        .checked_mul(size_of::<bool>())
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    ensure_memory(output_bytes)?;
    let unmatched = !equal;
    Ok(LiteralClaims {
        source: if equal {
            CountBounds { lower: 0, upper: 0 }
        } else {
            CountBounds {
                lower: selected_source,
                upper: selected_source,
            }
        },
        residual: if equal {
            CountBounds { lower: 0, upper: 0 }
        } else {
            CountBounds {
                lower: selected_residual,
                upper: selected_residual,
            }
        },
        mandatory: MandatoryChanges {
            old: bools(old.len(), unmatched)?,
            new: bools(new.len(), unmatched)?,
            lcs_length: if equal { old.len() } else { 0 },
        },
    })
}

fn selected_count(old_mask: &[bool], new_mask: &[bool]) -> Result<usize> {
    old_mask
        .iter()
        .filter(|selected| **selected)
        .count()
        .checked_add(new_mask.iter().filter(|selected| **selected).count())
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))
}

fn bitset_lcs<T: Eq + Hash>(
    old: &[T],
    new: &[T],
    remaining_work: &mut usize,
) -> Result<Option<usize>> {
    let words = old
        .len()
        .checked_add(63)
        .map(|length| length / 64)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let memory = bitset_memory_bytes::<T>(old.len(), words)?;
    ensure_memory(memory)?;
    let work = old
        .len()
        .checked_add(
            new.len()
                .checked_mul(words)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
        )
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    if !charge(remaining_work, work) {
        return Ok(None);
    }
    if !charge(remaining_work, words) {
        return Ok(None);
    }

    let mut positions = HashMap::<&T, Vec<u64>>::new();
    positions
        .try_reserve(old.len())
        .map_err(|_| allocation_error("claim bitset map"))?;
    for (index, value) in old.iter().enumerate() {
        match positions.entry(value) {
            Entry::Occupied(mut entry) => {
                let words = entry.get_mut();
                words[index / 64] |= 1u64 << (index % 64);
            }
            Entry::Vacant(entry) => {
                if !charge(remaining_work, words) {
                    return Ok(None);
                }
                let mut words_for_value = Vec::new();
                words_for_value
                    .try_reserve_exact(words)
                    .map_err(|_| allocation_error("claim bitset positions"))?;
                words_for_value.resize(words, 0);
                words_for_value[index / 64] |= 1u64 << (index % 64);
                entry.insert(words_for_value);
            }
        }
    }

    let mut state = Vec::new();
    state
        .try_reserve_exact(words)
        .map_err(|_| allocation_error("claim bitset state"))?;
    state.resize(words, 0);
    for value in new {
        let Some(matches) = positions.get(value) else {
            continue;
        };
        let mut shift_carry = 1u64;
        let mut subtraction_borrow = false;
        for word_index in 0..words {
            let previous = state[word_index];
            let x = matches[word_index] | previous;
            let y = (previous << 1) | shift_carry;
            shift_carry = previous >> 63;
            let (difference, borrow) = x.overflowing_sub(y);
            let (difference, borrow_with_carry) =
                difference.overflowing_sub(u64::from(subtraction_borrow));
            subtraction_borrow = borrow || borrow_with_carry;
            state[word_index] = x & !difference;
        }
        if let Some(last) = state.last_mut()
            && let Some(remainder) = old.len().checked_rem(64)
            && remainder != 0
        {
            *last &= (1u64 << remainder) - 1;
        }
    }
    if !charge(remaining_work, words) {
        return Ok(None);
    }

    Ok(Some(state.into_iter().map(u64::count_ones).try_fold(
        0usize,
        |count, bits| {
            count
                .checked_add(bits as usize)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))
        },
    )?))
}

fn bitset_memory_bytes<T>(old_len: usize, words: usize) -> Result<usize> {
    let pattern_words = old_len
        .checked_mul(words)
        .and_then(|count| count.checked_mul(size_of::<u64>()))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let state = words
        .checked_mul(size_of::<u64>())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let map_entry = size_of::<&T>()
        .checked_add(size_of::<Vec<u64>>())
        .and_then(|bytes| bytes.checked_add(size_of::<usize>()))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let map = old_len
        .checked_mul(map_entry)
        .and_then(|bytes| bytes.checked_mul(4))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    pattern_words
        .checked_add(state)
        .and_then(|bytes| bytes.checked_add(map))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))
}

#[derive(Debug, PartialEq, Eq)]
struct BandLayout {
    old_len: usize,
    new_len: usize,
    lower: usize,
    upper: usize,
    dense: bool,
    cell_count: usize,
    row_offsets: Vec<usize>,
}

impl BandLayout {
    fn band(old_len: usize, new_len: usize, distance: usize) -> Result<Self> {
        let difference = old_len.abs_diff(new_len);
        if difference > distance {
            return Err(invalid(
                "LCS distance is smaller than side-length difference",
            ));
        }
        let radius = distance
            .checked_sub(difference)
            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?
            / 2;
        let (lower, upper) = if new_len >= old_len {
            (
                radius,
                difference
                    .checked_add(radius)
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
            )
        } else {
            (
                difference
                    .checked_add(radius)
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                radius,
            )
        };
        Self::new(old_len, new_len, lower, upper, false)
    }

    fn dense(old_len: usize, new_len: usize) -> Result<Self> {
        Self::new(old_len, new_len, old_len, new_len, true)
    }

    fn new(
        old_len: usize,
        new_len: usize,
        lower: usize,
        upper: usize,
        dense: bool,
    ) -> Result<Self> {
        new_len
            .checked_add(1)
            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
        let rows = old_len
            .checked_add(1)
            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
        ensure_memory(layout_memory_bytes(rows, 0, old_len, new_len)?)?;
        let mut cell_count = 0usize;
        for row in 0..rows {
            let (start, end) = row_bounds_for(old_len, new_len, lower, upper, dense, row);
            cell_count = cell_count
                .checked_add(end - start)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
        }
        let memory = layout_memory_bytes(rows, cell_count, old_len, new_len)?;
        ensure_memory(memory)?;
        let mut row_offsets = Vec::new();
        row_offsets
            .try_reserve_exact(rows)
            .map_err(|_| allocation_error("claim DP row offsets"))?;
        let mut offset = 0usize;
        for row in 0..rows {
            row_offsets.push(offset);
            let (start, end) = row_bounds_for(old_len, new_len, lower, upper, dense, row);
            offset = offset
                .checked_add(end - start)
                .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
        }
        Ok(Self {
            old_len,
            new_len,
            lower,
            upper,
            dense,
            cell_count,
            row_offsets,
        })
    }

    fn row_bounds(&self, row: usize) -> (usize, usize) {
        row_bounds_for(
            self.old_len,
            self.new_len,
            self.lower,
            self.upper,
            self.dense,
            row,
        )
    }

    fn index(&self, row: usize, column: usize) -> Option<usize> {
        if row > self.old_len || column > self.new_len {
            return None;
        }
        let (start, end) = self.row_bounds(row);
        if column < start || column >= end {
            return None;
        }
        Some(self.row_offsets[row] + column - start)
    }

    fn memory_bytes(&self) -> Result<usize> {
        layout_memory_bytes(
            self.row_offsets.len(),
            self.cell_count,
            self.old_len,
            self.new_len,
        )
    }
}

fn row_bounds_for(
    old_len: usize,
    new_len: usize,
    lower: usize,
    upper: usize,
    dense: bool,
    row: usize,
) -> (usize, usize) {
    if dense {
        return (0, new_len + 1);
    }
    let start = row.saturating_sub(lower);
    let end = row.saturating_add(upper).min(new_len) + 1;
    debug_assert!(row <= old_len);
    (start, end)
}

fn layout_memory_bytes(rows: usize, cells: usize, old_len: usize, new_len: usize) -> Result<usize> {
    let offsets = rows
        .checked_mul(size_of::<usize>())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let grid = cells
        .checked_mul(size_of::<ClaimCell>())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let suffix = new_len
        .checked_add(1)
        .and_then(|columns| columns.checked_mul(2))
        .and_then(|count| count.checked_mul(size_of::<usize>()))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let output = old_len
        .checked_add(new_len)
        .and_then(|length| length.checked_mul(size_of::<bool>()))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    offsets
        .checked_add(grid)
        .and_then(|bytes| bytes.checked_add(suffix))
        .and_then(|bytes| bytes.checked_add(output))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClaimCell {
    lcs: u32,
    source_min: u32,
    source_max: u32,
    residual_min: u32,
    residual_max: u32,
}

impl ClaimCell {
    const UNREACHABLE: Self = Self {
        lcs: u32::MAX,
        source_min: 0,
        source_max: 0,
        residual_min: 0,
        residual_max: 0,
    };

    fn reachable(self) -> bool {
        self.lcs != u32::MAX
    }
}

struct ClaimGrid {
    layout: BandLayout,
    cells: Vec<ClaimCell>,
}

impl ClaimGrid {
    fn new(layout: BandLayout) -> Result<Self> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(layout.cell_count)
            .map_err(|_| allocation_error("fused claim DP grid"))?;
        cells.resize(layout.cell_count, ClaimCell::UNREACHABLE);
        Ok(Self { layout, cells })
    }

    fn get(&self, row: usize, column: usize) -> Option<ClaimCell> {
        self.layout
            .index(row, column)
            .map(|index| self.cells[index])
    }

    fn set(&mut self, row: usize, column: usize, value: ClaimCell) {
        if let Some(index) = self.layout.index(row, column) {
            self.cells[index] = value;
        }
    }
}

fn forward_claims<T: Eq>(
    grid: &mut ClaimGrid,
    old: &[T],
    new: &[T],
    old_source: &[bool],
    new_source: &[bool],
    old_residual: &[bool],
    new_residual: &[bool],
) -> Result<ClaimCell> {
    for old_count in 0..=old.len() {
        let (start, end) = grid.layout.row_bounds(old_count);
        for new_count in start..end {
            if old_count == 0 && new_count == 0 {
                grid.set(
                    old_count,
                    new_count,
                    ClaimCell {
                        lcs: 0,
                        source_min: 0,
                        source_max: 0,
                        residual_min: 0,
                        residual_max: 0,
                    },
                );
                continue;
            }
            let mut best = ClaimCell::UNREACHABLE;
            if old_count > 0
                && let Some(up) = grid.get(old_count - 1, new_count)
            {
                best = combine_claims(best, up);
            }
            if new_count > 0
                && let Some(left) = grid.get(old_count, new_count - 1)
            {
                best = combine_claims(best, left);
            }
            if old_count > 0
                && new_count > 0
                && old[old_count - 1] == new[new_count - 1]
                && let Some(diagonal) = grid.get(old_count - 1, new_count - 1)
                && diagonal.reachable()
            {
                let source = diagonal
                    .source_min
                    .checked_add(u32::from(old_source[old_count - 1]))
                    .and_then(|value| value.checked_add(u32::from(new_source[new_count - 1])))
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
                let residual = diagonal
                    .residual_min
                    .checked_add(u32::from(old_residual[old_count - 1]))
                    .and_then(|value| value.checked_add(u32::from(new_residual[new_count - 1])))
                    .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
                best = combine_claims(
                    best,
                    ClaimCell {
                        lcs: diagonal
                            .lcs
                            .checked_add(1u32)
                            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                        source_min: source,
                        source_max: diagonal
                            .source_max
                            .checked_add(u32::from(old_source[old_count - 1]))
                            .and_then(|value| {
                                value.checked_add(u32::from(new_source[new_count - 1]))
                            })
                            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                        residual_min: residual,
                        residual_max: diagonal
                            .residual_max
                            .checked_add(u32::from(old_residual[old_count - 1]))
                            .and_then(|value| {
                                value.checked_add(u32::from(new_residual[new_count - 1]))
                            })
                            .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?,
                    },
                );
            }
            grid.set(old_count, new_count, best);
        }
    }
    grid.get(old.len(), new.len())
        .filter(|endpoint| endpoint.reachable())
        .ok_or_else(|| invalid("fused claim grid cannot reach its endpoint"))
}

fn combine_claims(left: ClaimCell, right: ClaimCell) -> ClaimCell {
    match (left.reachable(), right.reachable()) {
        (false, false) => ClaimCell::UNREACHABLE,
        (false, true) => right,
        (true, false) => left,
        (true, true) if left.lcs > right.lcs => left,
        (true, true) if right.lcs > left.lcs => right,
        (true, true) => ClaimCell {
            lcs: left.lcs,
            source_min: min(left.source_min, right.source_min),
            source_max: left.source_max.max(right.source_max),
            residual_min: min(left.residual_min, right.residual_min),
            residual_max: left.residual_max.max(right.residual_max),
        },
    }
}

fn mandatory_claims<T: Eq>(
    grid: &ClaimGrid,
    old: &[T],
    new: &[T],
    lcs_length: usize,
) -> Result<MandatoryChanges> {
    let mut old_changed = bools(old.len(), true)?;
    let mut new_changed = bools(new.len(), true)?;
    let columns = new
        .len()
        .checked_add(1)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let mut suffix_next = zeroed_usizes(columns)?;
    let mut suffix_current = zeroed_usizes(columns)?;

    for old_index in (0..=old.len()).rev() {
        let (start, end) = grid.layout.row_bounds(old_index);
        for new_index in (start..end).rev() {
            let best = if old_index == old.len() && new_index == new.len() {
                Some(0)
            } else {
                let mut best = 0usize;
                let mut reachable = false;
                if old_index < old.len()
                    && grid.layout.index(old_index + 1, new_index).is_some()
                    && suffix_next[new_index] != usize::MAX
                {
                    best = best.max(suffix_next[new_index]);
                    reachable = true;
                }
                if new_index < new.len()
                    && grid.layout.index(old_index, new_index + 1).is_some()
                    && suffix_current[new_index + 1] != usize::MAX
                {
                    best = best.max(suffix_current[new_index + 1]);
                    reachable = true;
                }
                if old_index < old.len()
                    && new_index < new.len()
                    && old[old_index] == new[new_index]
                    && grid.layout.index(old_index + 1, new_index + 1).is_some()
                    && suffix_next[new_index + 1] != usize::MAX
                {
                    let candidate = suffix_next[new_index + 1]
                        .checked_add(1)
                        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
                    best = best.max(candidate);
                    reachable = true;
                }
                reachable.then_some(best)
            };
            suffix_current[new_index] = best.unwrap_or(usize::MAX);

            if old_index < old.len()
                && new_index < new.len()
                && old[old_index] == new[new_index]
                && let Some(prefix) = grid.get(old_index, new_index)
                && prefix.reachable()
                && grid.layout.index(old_index + 1, new_index + 1).is_some()
            {
                let suffix = suffix_next[new_index + 1];
                if suffix != usize::MAX {
                    let suffix =
                        u32::try_from(suffix).map_err(|_| limit_error(CLAIM_CELLS_RESOURCE))?;
                    let through = prefix
                        .lcs
                        .checked_add(1u32)
                        .and_then(|value| value.checked_add(suffix))
                        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
                    if through as usize == lcs_length {
                        old_changed[old_index] = false;
                        new_changed[new_index] = false;
                    }
                }
            }
        }
        std::mem::swap(&mut suffix_next, &mut suffix_current);
    }

    Ok(MandatoryChanges {
        old: old_changed,
        new: new_changed,
        lcs_length,
    })
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct CountCell {
    lcs: usize,
    min_matched: usize,
    max_matched: usize,
}

#[cfg(test)]
fn combine(left: CountCell, right: CountCell) -> CountCell {
    if left.lcs > right.lcs {
        left
    } else if right.lcs > left.lcs {
        right
    } else {
        CountCell {
            lcs: left.lcs,
            min_matched: min(left.min_matched, right.min_matched),
            max_matched: left.max_matched.max(right.max_matched),
        }
    }
}

fn validate_masks<T>(old: &[T], new: &[T], old_mask: &[bool], new_mask: &[bool]) -> Result<()> {
    if old.len() != old_mask.len() || new.len() != new_mask.len() {
        return Err(Error::InvalidConfiguration(
            "claim masks must match their input lengths".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn dimensions(old_len: usize, new_len: usize) -> Result<(usize, usize, usize)> {
    let rows = old_len
        .checked_add(1)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let columns = new_len
        .checked_add(1)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    let cells = rows
        .checked_mul(columns)
        .ok_or(limit_error(CLAIM_CELLS_RESOURCE))?;
    Ok((rows, columns, cells))
}

#[cfg(test)]
fn count_memory_bytes(columns: usize) -> Result<usize> {
    columns
        .checked_mul(size_of::<CountCell>())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))
}

#[cfg(test)]
fn mandatory_memory_bytes(old_len: usize, columns: usize, cells: usize) -> Result<usize> {
    let forward = cells
        .checked_mul(size_of::<usize>())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let suffix = columns
        .checked_mul(2)
        .and_then(|count| count.checked_mul(size_of::<usize>()))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let output = old_len
        .checked_add(columns - 1)
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    let output_bytes = output
        .checked_mul(size_of::<bool>())
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))?;
    forward
        .checked_add(suffix)
        .and_then(|bytes| bytes.checked_add(output_bytes))
        .ok_or(limit_error(CLAIM_MEMORY_RESOURCE))
}

fn ensure_memory(bytes: usize) -> Result<()> {
    let limit = claim_memory_limit();
    if bytes > limit {
        return Err(Error::LimitExceeded {
            resource: CLAIM_MEMORY_RESOURCE,
            limit,
        });
    }
    Ok(())
}

fn claim_memory_limit() -> usize {
    #[cfg(test)]
    {
        CLAIM_MEMORY_LIMIT.with(|limit| limit.get())
    }
    #[cfg(not(test))]
    {
        MAX_CLAIM_MEMORY_BYTES
    }
}

#[cfg(test)]
fn zeroed_count_cells(length: usize) -> Result<Vec<CountCell>> {
    let mut cells = Vec::new();
    cells
        .try_reserve_exact(length)
        .map_err(|_| allocation_error("claim DP row"))?;
    cells.resize(length, CountCell::default());
    Ok(cells)
}

fn zeroed_usizes(length: usize) -> Result<Vec<usize>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| allocation_error("claim DP row"))?;
    values.resize(length, 0);
    Ok(values)
}

fn bools(length: usize, value: bool) -> Result<Vec<bool>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| allocation_error("claim result mask"))?;
    values.resize(length, value);
    Ok(values)
}

fn charge(remaining_work: &mut usize, amount: usize) -> bool {
    match remaining_work.checked_sub(amount) {
        Some(remaining) => {
            *remaining_work = remaining;
            true
        }
        None => {
            *remaining_work = 0;
            false
        }
    }
}

fn limit_error(resource: &'static str) -> Error {
    Error::LimitExceeded {
        resource,
        limit: usize::MAX,
    }
}

fn invalid(message: &str) -> Error {
    Error::InvalidConfiguration(message.to_owned())
}

fn allocation_error(resource: &'static str) -> Error {
    Error::Unresolved(format!("{resource} allocation failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_bounds_handles_empty_sides() {
        let old = *b"ab";
        let new = *b"xyz";
        let mut work = 5;
        let result = count_bounds(&old, &[], &[true, false], &[], &mut work)
            .expect("empty new side has a direct claim");
        assert_eq!(result, Some(CountBounds { lower: 1, upper: 1 }));
        assert_eq!(work, 3);

        let result = count_bounds(&[], &new, &[], &[true, false, true], &mut work)
            .expect("empty old side has a direct claim");
        assert_eq!(result, Some(CountBounds { lower: 2, upper: 2 }));
    }

    #[test]
    fn count_bounds_reports_budget_exhaustion_without_a_claim() {
        let mut work = 3;
        let result = count_bounds(
            b"abc",
            b"xyz",
            &[true, true, true],
            &[true, true, true],
            &mut work,
        )
        .expect("budget exhaustion is a normal unresolved result");
        assert_eq!(result, None);
        assert_eq!(work, 0);
    }

    #[test]
    fn invalid_masks_are_rejected_before_work_is_charged() {
        let mut work = 10;
        let result = count_bounds(b"a", b"a", &[], &[true], &mut work);
        assert_eq!(
            result,
            Err(Error::InvalidConfiguration(
                "claim masks must match their input lengths".to_owned()
            ))
        );
        assert_eq!(work, 10);
    }

    #[test]
    fn mandatory_changed_is_symmetric_for_one_sided_edits() {
        let mut work = usize::MAX;
        let result = mandatory_changed(b"ri", b"rvi", &mut work)
            .expect("small mandatory query should complete")
            .expect("budget should suffice");
        assert_eq!(result.lcs_length, 2);
        assert_eq!(result.old, vec![false, false]);
        assert_eq!(result.new, vec![false, true, false]);

        let mut work = usize::MAX;
        let result = mandatory_changed(b"rvi", b"ri", &mut work)
            .expect("small mandatory query should complete")
            .expect("budget should suffice");
        assert_eq!(result.lcs_length, 2);
        assert_eq!(result.old, vec![false, true, false]);
        assert_eq!(result.new, vec![false, false]);
    }

    #[test]
    fn mandatory_changed_reports_budget_exhaustion_without_a_claim() {
        let mut work = 17;
        let result = mandatory_changed(b"abcd", b"wxyz", &mut work)
            .expect("budget exhaustion is a normal unresolved result");
        assert_eq!(result, None);
        assert_eq!(work, 0);
    }

    #[test]
    fn mandatory_changed_marks_only_positions_absent_from_every_optimal_lcs() {
        let mut work = usize::MAX;
        let result = mandatory_changed(b"ab", b"ba", &mut work)
            .expect("small mandatory query should complete")
            .expect("budget should suffice");
        assert_eq!(result.lcs_length, 1);
        assert_eq!(result.old, vec![false, false]);
        assert_eq!(result.new, vec![false, false]);
    }

    #[test]
    fn exhaustive_binary_strings_match_the_alignment_oracle() {
        let strings = binary_strings();
        let mut pair_count = 0;
        for old in &strings {
            for new in &strings {
                pair_count += 1;
                let alignments = optimal_alignments(old, new);
                let (expected_bounds, expected_old, expected_new, expected_lcs) =
                    oracle(old, new, &alignments, &[], &[]);
                let mut work = usize::MAX;
                let mandatory = mandatory_changed(old, new, &mut work)
                    .expect("the exhaustive query should not fail")
                    .expect("the exhaustive query should fit its budget");
                assert_eq!(mandatory.lcs_length, expected_lcs);
                assert_eq!(mandatory.old, expected_old);
                assert_eq!(mandatory.new, expected_new);

                for old_mask in interval_masks(old.len()) {
                    for new_mask in interval_masks(new.len()) {
                        let expected = oracle(old, new, &alignments, &old_mask, &new_mask).0;
                        let mut work = usize::MAX;
                        let actual = count_bounds(old, new, &old_mask, &new_mask, &mut work)
                            .expect("the exhaustive query should not fail")
                            .expect("the exhaustive query should fit its budget");
                        assert_eq!(actual, expected, "old={old:?}, new={new:?}");
                    }
                }
                assert_eq!(expected_bounds, oracle(old, new, &alignments, &[], &[]).0);
            }
        }
        assert_eq!(pair_count, 961);
    }

    #[test]
    fn dsa_introduction_bounds_and_mandatory_positions_match_the_recorded_probe() {
        let old = DSA_OLD.as_bytes();
        let new = DSA_NEW.as_bytes();
        assert_eq!(old.len(), 1431);
        assert_eq!(new.len(), 2509);
        let mut region = vec![false; new.len()];
        region[1611..1718].fill(true);
        let mut work = usize::MAX;
        let bounds = count_bounds(old, new, &vec![false; old.len()], &region, &mut work)
            .expect("the DSA count query should not fail")
            .expect("the DSA count query should fit its budget");
        assert_eq!(
            bounds,
            CountBounds {
                lower: 103,
                upper: 107
            }
        );

        let mut work = usize::MAX;
        let mandatory = mandatory_changed(old, new, &mut work)
            .expect("the DSA mandatory query should not fail")
            .expect("the DSA mandatory query should fit its budget");
        assert_eq!(mandatory.lcs_length, 1097);
        assert_eq!(
            mandatory.new[1611..1718]
                .iter()
                .filter(|changed| **changed)
                .count(),
            74
        );

        let mut unresolved_region = vec![false; new.len()];
        for (index, unresolved) in unresolved_region
            .iter_mut()
            .enumerate()
            .take(1718)
            .skip(1611)
        {
            *unresolved = !mandatory.new[index];
        }
        let mut work = usize::MAX;
        let remainder = count_bounds(
            old,
            new,
            &vec![false; old.len()],
            &unresolved_region,
            &mut work,
        )
        .expect("the DSA remainder query should not fail")
        .expect("the DSA remainder query should fit its budget");
        assert_eq!(
            remainder,
            CountBounds {
                lower: 29,
                upper: 33
            }
        );
    }

    #[test]
    fn literal_changed_count_is_symmetric_for_an_old_side_deletion() {
        let mut old_mask = vec![false; 3];
        old_mask[1] = true;
        let mut work = usize::MAX;
        let bounds = count_bounds(b"rvi", b"ri", &old_mask, &[false, false], &mut work)
            .expect("the deletion query should not fail")
            .expect("the deletion query should fit its budget");
        assert_eq!(bounds, CountBounds { lower: 1, upper: 1 });

        let residual_new = [true, false, true];
        let mut work = usize::MAX;
        let residual = count_bounds(b"ri", b"rvi", &[false, false], &residual_new, &mut work)
            .expect("the resolved-v remainder query should not fail")
            .expect("the resolved-v remainder query should fit its budget");
        assert_eq!(residual, CountBounds { lower: 0, upper: 0 });
    }

    const DSA_OLD: &str = r####" Federal Information Processing Standards Publication 186-4  July 2013  Specifications for the DIGITAL SIGNATURE STANDARD (DSS)  1. Introduction This Standard defines methods for digital signature generation that can be used for the protection of binary data (commonly called a message), and for the verification and validation of those digital signatures. Three techniques are approved. (1) The Digital Signature Algorithm (DSA) is specified in this Standard. The specification includes criteria for the generation of domain parameters, for the generation of public and private key pairs, and for the generation and verification of digital signatures. (2) The RSA digital signature algorithm is specified in American National Standard (ANS) X9.31 and Public Key Cryptography Standard (PKCS) #1. FIPS 186-4 approves the use of implementations of either or both of these standards and specifies additional requirements. (3) The Elliptic Curve Digital Signature Algorithm (ECDSA) is specified in ANS X9.62. FIPS 186-4 approves the use of ECDSA and specifies additional requirements. Recommended elliptic curves for Federal Government use are provided herein. This Standard includes requirements for obtaining the assurances necessary for valid digital signatures. Methods for obtaining these assurances are provided in NIST Special Publication (SP) 800-89, Recommendation for Obtaining Assurances for Digital Signature Applications. "####;
    const DSA_NEW: &str = r####"FIPS 186-5 DIGITAL SIGNATURE STANDARD (DSS)  1. Introduction This standard defines methods for digital signature generation that can be used for the protection of binary data (commonly called a message) and for the verification and validation of those digital signatures. Three techniques are approved. (1) The RSA digital signature algorithm is specified in the Internet Engineering Task Force Request for Comments (IETF RFC) 8017 [1] and was previously specified in Public Key Cryptography Standard (PKCS) #1 [2]. FIPS 186-5 approves the use of implementations of either or both of these standards and specifies key pair generation, as well as additional requirements. (2) The Elliptic Curve Digital Signature Algorithm (ECDSA) is specified in this standard. ECDSA was originally specified in American National Standards (ANS) X9.62 [3] (withdrawn). A variant of ECDSA with a deterministic signature generation procedure known as deterministic ECDSA is also approved and specified in IETF RFC 6979 [4]. Recommended elliptic curves for Federal Government use of ECDSA (including deterministic ECDSA) are provided in NIST Special Publication (SP) 800-186 [5]. (3) The Edwards Curve Digital Signature Algorithm (EdDSA) is specified in IETF RFC 8032 [6]. FIPS 186-5 approves the use of EdDSA and specifies additional requirements. Recommended elliptic curves for Federal Government use of EdDSA are provided in SP 800-186 [5]. Also included is HashEdDSA, a version of EdDSA where the EdDSA signature is generated on the hash of the message rather than the message itself. The Digital Signature Algorithm (DSA) is no longer specified in this standard and may only be used to verify previously generated digital signatures. Complete specifications may be found in Federal Information Processing Standard (FIPS) 186-4 [7].  This standard includes requirements for obtaining the assurances necessary for valid digital signatures. Methods for obtaining these assurances are provided in SP 800-89, Recommendation for Obtaining Assurances for Digital Signature Applications [8]. Information about the key lengths used for generating and verifying digital signatures and the time frames during which they are assumed to be secure are provided in SP 800-131A [9]. Note that the algorithms in this standard are not expected to provide resistance to attacks from a large-scale quantum computer. Digital signature algorithms that will provide security from quantum computers will be specified in future NIST publications.  "####;

    fn binary_strings() -> Vec<Vec<u8>> {
        let mut strings = Vec::new();
        for length in 0..=4 {
            for bits in 0..(1usize << length) {
                strings.push(
                    (0..length)
                        .map(|index| if bits & (1 << index) == 0 { b'a' } else { b'b' })
                        .collect(),
                );
            }
        }
        strings
    }

    fn interval_masks(length: usize) -> Vec<Vec<bool>> {
        let mut masks = Vec::new();
        for start in 0..=length {
            for end in start..=length {
                let mut mask = vec![false; length];
                mask[start..end].fill(true);
                masks.push(mask);
            }
        }
        masks
    }

    fn lcs_table(old: &[u8], new: &[u8]) -> Vec<Vec<usize>> {
        let mut table = vec![vec![0; new.len() + 1]; old.len() + 1];
        for old_index in (0..old.len()).rev() {
            for new_index in (0..new.len()).rev() {
                table[old_index][new_index] = if old[old_index] == new[new_index] {
                    table[old_index + 1][new_index + 1] + 1
                } else {
                    table[old_index + 1][new_index].max(table[old_index][new_index + 1])
                };
            }
        }
        table
    }

    fn optimal_alignments(old: &[u8], new: &[u8]) -> Vec<Vec<(usize, usize)>> {
        fn visit(
            old: &[u8],
            new: &[u8],
            table: &[Vec<usize>],
            old_index: usize,
            new_index: usize,
            path: &mut Vec<(usize, usize)>,
            output: &mut Vec<Vec<(usize, usize)>>,
        ) {
            if old_index == old.len() || new_index == new.len() {
                output.push(path.clone());
                return;
            }
            let target = table[old_index][new_index];
            if old[old_index] == new[new_index] && table[old_index + 1][new_index + 1] + 1 == target
            {
                path.push((old_index, new_index));
                visit(old, new, table, old_index + 1, new_index + 1, path, output);
                path.pop();
            }
            if table[old_index + 1][new_index] == target {
                visit(old, new, table, old_index + 1, new_index, path, output);
            }
            if table[old_index][new_index + 1] == target {
                visit(old, new, table, old_index, new_index + 1, path, output);
            }
        }

        let table = lcs_table(old, new);
        let mut output = Vec::new();
        visit(old, new, &table, 0, 0, &mut Vec::new(), &mut output);
        output
    }

    fn oracle(
        old: &[u8],
        new: &[u8],
        alignments: &[Vec<(usize, usize)>],
        old_mask: &[bool],
        new_mask: &[bool],
    ) -> (CountBounds, Vec<bool>, Vec<bool>, usize) {
        let selected = old_mask.iter().filter(|selected| **selected).count()
            + new_mask.iter().filter(|selected| **selected).count();
        let mut lower = usize::MAX;
        let mut upper = 0;
        let mut old_matched = vec![false; old.len()];
        let mut new_matched = vec![false; new.len()];
        for alignment in alignments {
            let matched = alignment
                .iter()
                .map(|&(old_index, new_index)| {
                    old_matched[old_index] = true;
                    new_matched[new_index] = true;
                    usize::from(old_mask.get(old_index).copied().unwrap_or(false))
                        + usize::from(new_mask.get(new_index).copied().unwrap_or(false))
                })
                .sum::<usize>();
            let changed = selected - matched;
            lower = lower.min(changed);
            upper = upper.max(changed);
        }
        (
            CountBounds { lower, upper },
            old_matched.into_iter().map(|matched| !matched).collect(),
            new_matched.into_iter().map(|matched| !matched).collect(),
            lcs_table(old, new)[0][0],
        )
    }

    #[test]
    fn fused_claims_match_all_optimal_alignments_for_2883_queries() {
        let strings = binary_strings();
        let mut query_count = 0;
        for old in &strings {
            for new in &strings {
                let alignments = optimal_alignments(old, new);
                let old_all = vec![true; old.len()];
                let new_all = vec![true; new.len()];
                let old_alternating = old
                    .iter()
                    .enumerate()
                    .map(|(index, _)| index % 2 == 0)
                    .collect::<Vec<_>>();
                let new_alternating = new
                    .iter()
                    .enumerate()
                    .map(|(index, _)| index % 2 == 1)
                    .collect::<Vec<_>>();
                let cases = [
                    (
                        old_all.clone(),
                        vec![false; new.len()],
                        vec![false; old.len()],
                        new_all.clone(),
                    ),
                    (
                        vec![false; old.len()],
                        new_all,
                        old_all,
                        vec![false; new.len()],
                    ),
                    (
                        old_alternating.clone(),
                        new_alternating.clone(),
                        old_alternating,
                        new_alternating,
                    ),
                ];
                for (old_source, new_source, old_residual, new_residual) in cases {
                    query_count += 1;
                    let expected_source = oracle(old, new, &alignments, &old_source, &new_source).0;
                    let expected_residual =
                        oracle(old, new, &alignments, &old_residual, &new_residual).0;
                    let (_, expected_old, expected_new, expected_lcs) =
                        oracle(old, new, &alignments, &[], &[]);
                    let mut work = usize::MAX;
                    let actual = literal_claims(
                        old,
                        new,
                        &old_source,
                        &new_source,
                        &old_residual,
                        &new_residual,
                        &mut work,
                    )
                    .expect("the fused query should not fail")
                    .expect("the fused query should fit its budget");
                    assert_eq!(actual.source, expected_source, "old={old:?}, new={new:?}");
                    assert_eq!(
                        actual.residual, expected_residual,
                        "old={old:?}, new={new:?}"
                    );
                    assert_eq!(actual.mandatory.lcs_length, expected_lcs);
                    assert_eq!(actual.mandatory.old, expected_old);
                    assert_eq!(actual.mandatory.new, expected_new);
                }
            }
        }
        assert_eq!(query_count, 2_883);
    }

    #[test]
    fn fused_claims_preserve_repeated_token_ambiguity() {
        let old = b"aa";
        let new = b"a";
        let mut work = usize::MAX;
        let claims = literal_claims(
            old,
            new,
            &[true, true],
            &[true],
            &[true, true],
            &[true],
            &mut work,
        )
        .expect("repeated-token query should not fail")
        .expect("repeated-token query should fit its budget");
        assert_eq!(claims.source, CountBounds { lower: 1, upper: 1 });
        assert_eq!(claims.residual, CountBounds { lower: 1, upper: 1 });
        assert_eq!(claims.mandatory.old, vec![false, false]);
        assert_eq!(claims.mandatory.new, vec![false]);
    }

    #[test]
    fn fused_claims_keep_dsa_probe_values() {
        let old = DSA_OLD.as_bytes();
        let new = DSA_NEW.as_bytes();
        let mut bitset_work = usize::MAX;
        assert_eq!(bitset_lcs(old, new, &mut bitset_work), Ok(Some(1097)));
        let mut source = vec![false; new.len()];
        source[1611..1718].fill(true);
        let mut work = usize::MAX;
        let claims = literal_claims(
            old,
            new,
            &vec![false; old.len()],
            &source,
            &vec![false; old.len()],
            &vec![false; new.len()],
            &mut work,
        )
        .expect("the DSA fused query should not fail")
        .expect("the DSA fused query should fit its budget");
        assert_eq!(
            claims.source,
            CountBounds {
                lower: 103,
                upper: 107
            }
        );
        assert_eq!(claims.mandatory.lcs_length, 1097);
        assert_eq!(
            claims.mandatory.new[1611..1718]
                .iter()
                .filter(|changed| **changed)
                .count(),
            74
        );
    }

    #[test]
    fn fused_claims_keep_hyphen_insertion_ambiguous() {
        let old = b"a-b";
        let new = b"ab-b";
        let mut work = usize::MAX;
        let claims = literal_claims(
            old,
            new,
            &[true, true, true],
            &[true, true, true, true],
            &[false, false, false],
            &[false, false, false, false],
            &mut work,
        )
        .expect("the hyphen query should not fail")
        .expect("the hyphen query should fit its budget");
        assert_eq!(claims.source, CountBounds { lower: 1, upper: 1 });
        assert_eq!(claims.mandatory.old, vec![false, false, false]);
        assert_eq!(claims.mandatory.new, vec![false, true, false, false]);
    }
}
