//! Token alignment.
//!
//! [`align`] returns equal token pairs that are strictly increasing on both
//! sides. Alignment is a quality concern only: any strictly increasing pair
//! list is correct, so the algorithm may leave gaps unmatched, and unmatched
//! gaps become a replacement in the script.
//!
//! Small gaps are aligned with an exact information-weighted longest common
//! subsequence. Tokens that occur almost everywhere (TOC dot leaders,
//! separators) carry weight 1, while rare words carry proportionally more, so a
//! shifted page number cannot outvote the title it belongs to. Large gaps first
//! anchor on unique n-grams; every anchor is extended to its maximal equal run
//! and the chain is selected by total matched information instead of run
//! length. Gaps without anchors fall back to an exact Myers-style bisection
//! bounded by a step budget; when that budget is exhausted the gap stays
//! unmatched.

use std::collections::HashMap;

/// Maximum number of dynamic-programming cells spent on exact LCS per gap.
pub const SMALL_GAP_CELL_BUDGET: usize = 4_000_000;

/// Largest n-gram order considered for unique anchors.
const MAX_ANCHOR_NGRAM: usize = 3;

/// Maximum number of edit steps the Myers fallback explores.
const MYERS_MAX_EDITS: usize = 20_000;

/// Maximum number of token comparisons the Myers fallback spends per gap.
const MYERS_STEP_BUDGET: usize = 64_000_000;

/// Returns equal pairs `(old_index, new_index)`, sorted and strictly increasing
/// on both sides, using unit token weights.
#[must_use]
pub fn align(old: &[u32], new: &[u32]) -> Vec<(usize, usize)> {
    align_with_fixed(old, new, &[], &[])
}

/// Returns equal pairs maximizing matched information weight.
///
/// `weights` is indexed by token id; missing ids weigh 1. Use
/// [`information_weights`] to derive weights from the token frequency of one
/// document pair.
#[must_use]
pub fn align_weighted(old: &[u32], new: &[u32], weights: &[u64]) -> Vec<(usize, usize)> {
    align_with_fixed(old, new, weights, &[])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlignmentMode {
    /// Exact information-weighted LCS for small gaps (the public alignment).
    Information,
    /// Long-run and block-boundary aware alignment used by the diff engine.
    Runs,
}

/// Memoizes the alignment of one document segment by its absolute bounds.
///
/// A promotion loop re-aligns the same promoted set plus a few new pairs. The
/// segments outside the new pairs are byte-identical slices whose alignment
/// cannot change, so every earlier result stays valid. A forced pair splits one
/// segment into new ranges; the old range is simply no longer requested, so the
/// cache stays close to one entry per segment of the final round.
pub(crate) struct SegmentCache {
    entries: HashMap<SegmentKey, Vec<(usize, usize)>>,
}

/// Mode, absolute start pair, and old/new lengths of one aligned segment.
type SegmentKey = (u8, usize, usize, usize, usize);

impl SegmentCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&self, key: SegmentKey) -> Option<&Vec<(usize, usize)>> {
        self.entries.get(&key)
    }

    fn insert(&mut self, key: SegmentKey, matches: Vec<(usize, usize)>) {
        self.entries.insert(key, matches);
    }
}

/// Derives integer information weights from the shared token vocabulary.
///
/// Each token weighs `max(1, round(8 * log2(total / count)))`, where `count` is
/// its combined frequency on both sides. Frequent punctuation therefore weighs
/// 1 and rare words weigh substantially more. The scale is fixed, so weights
/// are comparable across document pairs and require no per-document tuning.
#[must_use]
pub fn information_weights(old: &[u32], new: &[u32]) -> Vec<u64> {
    let mut counts: Vec<u64> = Vec::new();
    for &id in old.iter().chain(new) {
        let index = usize::try_from(id).expect("token id fits usize");
        if index >= counts.len() {
            counts.resize(index + 1, 0);
        }
        counts[index] = counts[index].saturating_add(1);
    }
    let total = u64::try_from(old.len().saturating_add(new.len())).unwrap_or(u64::MAX);
    counts
        .into_iter()
        .map(|count| {
            let ratio = total as f64 / count.max(1) as f64;
            ((8.0 * ratio.log2()).round() as u64).max(1)
        })
        .collect()
}

/// Returns equal pairs with `fixed` pairs forced into the result.
///
/// `fixed` must be strictly increasing on both sides and equal. Alignment runs
/// independently on the ranges between consecutive fixed pairs; fixed pairs
/// are never matched twice.
#[must_use]
pub fn align_with_fixed(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    fixed: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    align_with_fixed_impl(
        old,
        new,
        weights,
        fixed,
        AlignmentMode::Information,
        None,
        None,
    )
}

/// The run-aware alignment strategy: prefer long equal runs in small gaps.
///
/// A gap with a repeated paragraph pattern can have two alignments of nearly
/// equal information weight: one matching many short identical phrases in
/// place, and one matching fewer but longer runs. The longer runs keep
/// paragraph boundaries intact, which lets move extraction recognise a whole
/// relocated paragraph instead of turning its unique words into replacements.
/// Small gaps therefore take the anchor-chain result when anchors exist,
/// even if a token LCS would match one or two more low-information tokens.
#[must_use]
pub(crate) fn align_runs(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    fixed: &[(usize, usize)],
    old_boundaries: &[bool],
    new_boundaries: &[bool],
) -> Vec<(usize, usize)> {
    align_with_fixed_impl(
        old,
        new,
        weights,
        fixed,
        AlignmentMode::Runs,
        Some((old_boundaries, new_boundaries)),
        None,
    )
}

/// Information alignment that reuses unchanged segments of the previous round.
pub(crate) fn align_with_fixed_cached(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    fixed: &[(usize, usize)],
    segments: &mut SegmentCache,
) -> Vec<(usize, usize)> {
    if fixed.len() < SEGMENT_CACHE_MIN_FIXED {
        return align_with_fixed(old, new, weights, fixed);
    }
    align_with_fixed_impl(
        old,
        new,
        weights,
        fixed,
        AlignmentMode::Information,
        None,
        Some(segments),
    )
}

/// Run-aware alignment that reuses unchanged segments of the previous round.
pub(crate) fn align_runs_cached(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    fixed: &[(usize, usize)],
    old_boundaries: &[bool],
    new_boundaries: &[bool],
    segments: &mut SegmentCache,
) -> Vec<(usize, usize)> {
    if fixed.len() < SEGMENT_CACHE_MIN_FIXED {
        return align_runs(old, new, weights, fixed, old_boundaries, new_boundaries);
    }
    align_with_fixed_impl(
        old,
        new,
        weights,
        fixed,
        AlignmentMode::Runs,
        Some((old_boundaries, new_boundaries)),
        Some(segments),
    )
}

/// Below this many forced pairs the segments are too few to be worth caching.
const SEGMENT_CACHE_MIN_FIXED: usize = 64;

fn align_with_fixed_impl(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    fixed: &[(usize, usize)],
    mode: AlignmentMode,
    boundaries: Option<(&[bool], &[bool])>,
    mut segments: Option<&mut SegmentCache>,
) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    for &(old_pair, new_pair) in fixed {
        if old_pair < old_cursor
            || new_pair < new_cursor
            || old_pair >= old.len()
            || new_pair >= new.len()
            || old[old_pair] != new[new_pair]
        {
            continue;
        }
        if old_pair == old_cursor && new_pair == new_cursor {
            // A run of consecutive forced pairs leaves no gap to align. Handling
            // it without entering the segment machinery keeps documents with
            // tens of thousands of forced pairs linear in the gaps instead of
            // paying a per-pair allocation.
            matches.push((old_pair, new_pair));
            old_cursor += 1;
            new_cursor += 1;
            continue;
        }
        let key = (
            mode as u8,
            old_cursor,
            new_cursor,
            old_pair - old_cursor,
            new_pair - new_cursor,
        );
        if let Some(cached) = segments.as_ref().and_then(|cache| cache.get(key)) {
            matches.extend_from_slice(cached);
        } else {
            let start = matches.len();
            align_segment(
                &SegmentContext {
                    old: &old[old_cursor..old_pair],
                    new: &new[new_cursor..new_pair],
                    weights,
                    old_base: old_cursor,
                    new_base: new_cursor,
                    mode,
                    boundaries: boundaries.map(|(old_boundaries, new_boundaries)| {
                        (
                            &old_boundaries[old_cursor..old_pair],
                            &new_boundaries[new_cursor..new_pair],
                        )
                    }),
                },
                &mut matches,
            );
            if let Some(cache) = segments.as_mut() {
                cache.insert(key, matches[start..].to_vec());
            }
        }
        matches.push((old_pair, new_pair));
        old_cursor = old_pair + 1;
        new_cursor = new_pair + 1;
    }
    let key = (
        mode as u8,
        old_cursor,
        new_cursor,
        old.len() - old_cursor,
        new.len() - new_cursor,
    );
    if let Some(cached) = segments.as_ref().and_then(|cache| cache.get(key)) {
        matches.extend_from_slice(cached);
    } else {
        let start = matches.len();
        align_segment(
            &SegmentContext {
                old: &old[old_cursor..],
                new: &new[new_cursor..],
                weights,
                old_base: old_cursor,
                new_base: new_cursor,
                mode,
                boundaries: boundaries.map(|(old_boundaries, new_boundaries)| {
                    (&old_boundaries[old_cursor..], &new_boundaries[new_cursor..])
                }),
            },
            &mut matches,
        );
        if let Some(cache) = segments.as_mut() {
            cache.insert(key, matches[start..].to_vec());
        }
    }
    matches.sort_unstable();
    matches.dedup();
    // Segment results are concatenated across forced boundaries and cached
    // independently; a lexicographic sort can leave two pairs that share an
    // old index or cross on the new side. A spine must be strictly increasing
    // in both coordinates so gap refinement and script classification can use
    // contiguous ranges. Dropping a non-monotone pair is safe: it is an equal
    // pair inside a gap, and residual LCS can match it again when the gap is
    // refined.
    let mut monotone = Vec::with_capacity(matches.len());
    for pair in matches {
        if monotone
            .last()
            .is_none_or(|&(old, new)| pair.0 > old && pair.1 > new)
        {
            monotone.push(pair);
        }
    }
    monotone
}

struct SegmentContext<'a> {
    old: &'a [u32],
    new: &'a [u32],
    weights: &'a [u64],
    old_base: usize,
    new_base: usize,
    mode: AlignmentMode,
    boundaries: Option<(&'a [bool], &'a [bool])>,
}

fn align_segment(context: &SegmentContext<'_>, matches: &mut Vec<(usize, usize)>) {
    let SegmentContext {
        old,
        new,
        weights,
        old_base,
        new_base,
        mode,
        boundaries,
    } = *context;
    let (old_boundaries, new_boundaries) = boundaries.unzip();
    let mut work = vec![(0, old.len(), 0, new.len())];
    while let Some((old_start, old_end, new_start, new_end)) = work.pop() {
        if old_start > old_end || new_start > new_end || old_end > old.len() || new_end > new.len()
        {
            continue;
        }
        if old_start == old_end || new_start == new_end {
            continue;
        }
        let (mut old_start, mut old_end) = (old_start, old_end);
        let (mut new_start, mut new_end) = (new_start, new_end);
        if mode == AlignmentMode::Information {
            while old_start < old_end && new_start < new_end && old[old_start] == new[new_start] {
                matches.push((old_base + old_start, new_base + new_start));
                old_start += 1;
                new_start += 1;
            }
            while old_start < old_end && new_start < new_end && old[old_end - 1] == new[new_end - 1]
            {
                old_end -= 1;
                new_end -= 1;
                matches.push((old_base + old_end, new_base + new_end));
            }
            if old_start == old_end || new_start == new_end {
                continue;
            }
        }
        let old_gap = &old[old_start..old_end];
        let new_gap = &new[new_start..new_end];
        let small = small_gap_cells(old_gap.len(), new_gap.len())
            .is_some_and(|cells| cells <= SMALL_GAP_CELL_BUDGET);
        if small && mode == AlignmentMode::Information {
            bounded_lcs(
                old_gap,
                new_gap,
                weights,
                old_base + old_start,
                new_base + new_start,
                matches,
            );
            continue;
        }
        let mut anchor_matches = Vec::new();
        let mut anchor_work = Vec::new();
        let mut anchored = false;
        for order in 1..=MAX_ANCHOR_NGRAM {
            let runs = match order {
                1 => unique_ngram_runs::<1>(
                    old_gap,
                    new_gap,
                    weights,
                    if mode == AlignmentMode::Runs {
                        old_boundaries.map(|boundaries| &boundaries[old_start..old_end])
                    } else {
                        None
                    },
                    if mode == AlignmentMode::Runs {
                        new_boundaries.map(|boundaries| &boundaries[new_start..new_end])
                    } else {
                        None
                    },
                ),
                2 => unique_ngram_runs::<2>(
                    old_gap,
                    new_gap,
                    weights,
                    if mode == AlignmentMode::Runs {
                        old_boundaries.map(|boundaries| &boundaries[old_start..old_end])
                    } else {
                        None
                    },
                    if mode == AlignmentMode::Runs {
                        new_boundaries.map(|boundaries| &boundaries[new_start..new_end])
                    } else {
                        None
                    },
                ),
                _ => unique_ngram_runs::<3>(
                    old_gap,
                    new_gap,
                    weights,
                    if mode == AlignmentMode::Runs {
                        old_boundaries.map(|boundaries| &boundaries[old_start..old_end])
                    } else {
                        None
                    },
                    if mode == AlignmentMode::Runs {
                        new_boundaries.map(|boundaries| &boundaries[new_start..new_end])
                    } else {
                        None
                    },
                ),
            };
            if let Some(runs) = runs {
                emit_anchor_chain(
                    old_base + old_start,
                    old_base + old_end,
                    new_base + new_start,
                    new_base + new_end,
                    &runs,
                    &mut anchor_matches,
                    &mut anchor_work,
                );
                anchored = true;
                break;
            }
        }
        if anchored && !small {
            matches.extend(anchor_matches);
            work.extend(anchor_work);
            continue;
        }
        if small {
            let mut lcs_matches = Vec::new();
            bounded_lcs(
                old_gap,
                new_gap,
                weights,
                old_base + old_start,
                new_base + new_start,
                &mut lcs_matches,
            );
            if anchored {
                matches.extend(anchor_matches);
                work.extend(anchor_work);
            } else {
                matches.extend(lcs_matches);
            }
            continue;
        }
        bounded_myers(
            old_gap,
            new_gap,
            old_base + old_start,
            new_base + new_start,
            matches,
        );
    }
}

fn token_weight(weights: &[u64], id: u32) -> u64 {
    usize::try_from(id)
        .ok()
        .and_then(|index| weights.get(index).copied())
        .unwrap_or(1)
}

fn small_gap_cells(old_len: usize, new_len: usize) -> Option<usize> {
    (old_len + 1).checked_mul(new_len + 1)
}

/// Maximum cells the residual-gap refinement may spend across one document
/// pair. Each individual gap is still capped by `SMALL_GAP_CELL_BUDGET`; a gap
/// that does not fit is left unchanged.
const RESIDUAL_GAP_TOTAL_CELLS: usize = 32_000_000;

/// Plain unit-weight LCS for one residual Replace/Delete/Insert gap.
///
/// The caller keeps every existing weighted spine pair; these matches are only
/// added in the gaps between them.
pub(crate) fn residual_lcs(old: &[u32], new: &[u32], budget: &mut usize) -> Vec<(usize, usize)> {
    let Some(cells) = small_gap_cells(old.len(), new.len()) else {
        return Vec::new();
    };
    if cells > SMALL_GAP_CELL_BUDGET || cells > *budget {
        return Vec::new();
    }
    *budget -= cells;
    let mut matches = Vec::new();
    bounded_lcs(old, new, &[], 0, 0, &mut matches);
    matches
}

pub(crate) const fn residual_gap_total_cells() -> usize {
    RESIDUAL_GAP_TOTAL_CELLS
}

/// One maximal equal run found around a unique n-gram anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnchorRun {
    old: usize,
    new: usize,
    len: usize,
    weight: u64,
}

/// Finds maximal equal runs growing from every unique n-gram anchor and
/// returns the heaviest increasing chain of non-overlapping runs.
fn unique_ngram_runs<const N: usize>(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    old_boundaries: Option<&[bool]>,
    new_boundaries: Option<&[bool]>,
) -> Option<Vec<AnchorRun>> {
    if old.len() < N || new.len() < N {
        return None;
    }
    let mut old_seen: HashMap<[u32; N], (u32, usize)> = HashMap::new();
    for index in 0..=old.len() - N {
        let entry = old_seen.entry(ngram::<N>(old, index)).or_insert((0, index));
        entry.0 = entry.0.saturating_add(1);
    }
    let mut new_seen: HashMap<[u32; N], u32> = HashMap::new();
    for index in 0..=new.len() - N {
        let entry = new_seen.entry(ngram::<N>(new, index)).or_insert(0);
        *entry = entry.saturating_add(1);
    }
    let mut anchors: Vec<(usize, usize)> = Vec::new();
    for index in 0..=new.len() - N {
        let key = ngram::<N>(new, index);
        if new_seen.get(&key) != Some(&1) {
            continue;
        }
        let Some(&(count, position)) = old_seen.get(&key) else {
            continue;
        };
        if count == 1 {
            anchors.push((position, index));
        }
    }
    if anchors.is_empty() {
        return None;
    }

    let mut runs: Vec<AnchorRun> = Vec::with_capacity(anchors.len());
    for &(old_anchor, new_anchor) in &anchors {
        let mut start_old = old_anchor;
        let mut start_new = new_anchor;
        // A repeated token immediately before a run is anchored by the equal
        // continuation inside the run, so extending left through it is safe.
        while start_old > 0 && start_new > 0 && old[start_old - 1] == new[start_new - 1] {
            start_old -= 1;
            start_new -= 1;
        }
        let mut end_old = old_anchor + N;
        let mut end_new = new_anchor + N;
        while end_old < old.len() && end_new < new.len() && old[end_old] == new[end_new] {
            // A block-start token at the far end has no equal continuation to
            // anchor it here; including it can steal the token from a longer
            // run that starts there. Stop before it so the next run keeps its
            // own block boundary.
            let continues = end_old + 1 < old.len()
                && end_new + 1 < new.len()
                && old[end_old + 1] == new[end_new + 1];
            if !continues
                && (old_boundaries.is_some_and(|boundaries| boundaries[end_old])
                    || new_boundaries.is_some_and(|boundaries| boundaries[end_new]))
            {
                break;
            }
            end_old += 1;
            end_new += 1;
        }
        runs.push(AnchorRun {
            old: start_old,
            new: start_new,
            len: end_old - start_old,
            weight: run_weight(&old[start_old..end_old], weights),
        });
    }
    runs.sort_unstable_by_key(|run| (run.old, run.new, run.len));
    runs.dedup();

    // Overlapping maximum runs with different diagonals cannot both be part of
    // one strictly increasing pair list; prefer longer runs, then earlier ones.
    let mut kept: Vec<AnchorRun> = Vec::with_capacity(runs.len());
    runs.sort_unstable_by(|left, right| {
        right
            .len
            .cmp(&left.len)
            .then_with(|| left.old.cmp(&right.old))
            .then_with(|| left.new.cmp(&right.new))
    });
    for run in runs {
        let overlaps = kept.iter().any(|other| {
            run.old < other.old + other.len
                && other.old < run.old + run.len
                && run.new < other.new + other.len
                && other.new < run.new + run.len
        });
        if !overlaps {
            kept.push(run);
        }
    }
    kept.sort_unstable_by_key(|run| run.old);
    Some(heaviest_increasing_chain(&kept))
}

fn run_weight(ids: &[u32], weights: &[u64]) -> u64 {
    ids.iter()
        .map(|&id| token_weight(weights, id))
        .fold(0u64, u64::saturating_add)
}

/// Selects a strictly increasing chain of runs maximizing total weight.
///
/// `runs` must be sorted by old start and pairwise disjoint on both sides.
fn heaviest_increasing_chain(runs: &[AnchorRun]) -> Vec<AnchorRun> {
    if runs.is_empty() {
        return Vec::new();
    }
    let mut coordinates = runs.iter().map(|run| run.new).collect::<Vec<_>>();
    coordinates.sort_unstable();
    coordinates.dedup();
    let mut tree = Fenwick::new(coordinates.len());
    let mut previous = vec![None; runs.len()];
    let mut best_weight = 0;
    let mut best_index = 0;
    for (index, run) in runs.iter().enumerate() {
        let position = coordinates
            .binary_search(&run.new)
            .expect("run start is one of the compressed coordinates");
        let (weight, predecessor) = tree.query(position);
        previous[index] = predecessor;
        let weight = weight.saturating_add(run.weight);
        tree.update(position, weight, index);
        if weight > best_weight {
            best_weight = weight;
            best_index = index;
        }
    }
    let mut chain = Vec::new();
    let mut cursor = Some(best_index);
    while let Some(index) = cursor {
        chain.push(runs[index]);
        cursor = previous[index];
    }
    chain.reverse();
    chain
}

/// Prefix-maximum Fenwick tree over total run weights.
struct Fenwick {
    tree: Vec<u64>,
    index: Vec<Option<usize>>,
}

impl Fenwick {
    fn new(len: usize) -> Self {
        Self {
            tree: vec![0; len + 1],
            index: vec![None; len + 1],
        }
    }

    /// Returns the best weight strictly before `position`.
    fn query(&self, position: usize) -> (u64, Option<usize>) {
        let mut cursor = position;
        let mut weight = 0;
        let mut index = None;
        while cursor > 0 {
            if self.tree[cursor] > weight {
                weight = self.tree[cursor];
                index = self.index[cursor];
            }
            cursor &= cursor - 1;
        }
        (weight, index)
    }

    fn update(&mut self, position: usize, weight: u64, index: usize) {
        let mut cursor = position + 1;
        while cursor < self.tree.len() {
            if weight > self.tree[cursor] {
                self.tree[cursor] = weight;
                self.index[cursor] = Some(index);
            }
            cursor += cursor.isolate_lowest_one();
        }
    }
}

/// Pushes one anchor chain into the worklist and emits its runs.
fn emit_anchor_chain(
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
    runs: &[AnchorRun],
    matches: &mut Vec<(usize, usize)>,
    work: &mut Vec<(usize, usize, usize, usize)>,
) {
    let mut previous_old = old_start;
    let mut previous_new = new_start;
    for run in runs {
        let run_old = old_start + run.old;
        let run_new = new_start + run.new;
        if run_old < previous_old || run_new < previous_new {
            continue;
        }
        work.push((previous_old, run_old, previous_new, run_new));
        for offset in 0..run.len {
            matches.push((run_old + offset, run_new + offset));
        }
        previous_old = run_old + run.len;
        previous_new = run_new + run.len;
    }
    work.push((previous_old, old_end, previous_new, new_end));
}

fn ngram<const N: usize>(values: &[u32], start: usize) -> [u32; N] {
    let mut key = [0; N];
    key.copy_from_slice(&values[start..start + N]);
    key
}

/// Exact weighted LCS for small gaps, bounded by [`SMALL_GAP_CELL_BUDGET`].
fn bounded_lcs(
    old: &[u32],
    new: &[u32],
    weights: &[u64],
    old_start: usize,
    new_start: usize,
    matches: &mut Vec<(usize, usize)>,
) {
    let (old_len, new_len) = (old.len(), new.len());
    if old_len == 0 || new_len == 0 {
        return;
    }
    let Some(cells) = small_gap_cells(old_len, new_len) else {
        return;
    };
    if cells > SMALL_GAP_CELL_BUDGET {
        return;
    }
    let width = new_len + 1;
    let mut table = vec![0u64; cells];
    for i in (0..old_len).rev() {
        for j in (0..new_len).rev() {
            let skip = table[(i + 1) * width + j].max(table[i * width + j + 1]);
            table[i * width + j] = if old[i] == new[j] {
                table[(i + 1) * width + j + 1]
                    .saturating_add(token_weight(weights, old[i]))
                    .max(skip)
            } else {
                skip
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < old_len && j < new_len {
        if old[i] == new[j]
            && table[i * width + j]
                == table[(i + 1) * width + j + 1].saturating_add(token_weight(weights, old[i]))
        {
            matches.push((old_start + i, new_start + j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
}

/// Exact shortest edit script via Myers' bisection, bounded by work budgets.
///
/// Emits every equal pair of one shortest script. Returns `false` when the
/// edit step or comparison budget is exhausted; already emitted pairs remain
/// valid, and the caller leaves the rest of the gap unmatched.
fn bounded_myers(
    old: &[u32],
    new: &[u32],
    old_start: usize,
    new_start: usize,
    matches: &mut Vec<(usize, usize)>,
) -> bool {
    let mut budget = MYERS_STEP_BUDGET;
    let mut stack = vec![(0_usize, old.len(), 0_usize, new.len())];
    while let Some((mut old_lo, mut old_hi, mut new_lo, mut new_hi)) = stack.pop() {
        while old_lo < old_hi && new_lo < new_hi && old[old_lo] == new[new_lo] {
            matches.push((old_start + old_lo, new_start + new_lo));
            old_lo += 1;
            new_lo += 1;
        }
        while old_lo < old_hi && new_lo < new_hi && old[old_hi - 1] == new[new_hi - 1] {
            old_hi -= 1;
            new_hi -= 1;
            matches.push((old_start + old_hi, new_start + new_hi));
        }
        if old_lo == old_hi || new_lo == new_hi {
            continue;
        }
        let Some((split_old, split_new)) =
            myers_middle_snake(&old[old_lo..old_hi], &new[new_lo..new_hi], &mut budget)
        else {
            return false;
        };
        let split_old = old_lo + split_old;
        let split_new = new_lo + split_new;
        if (split_old == old_lo && split_new == new_lo)
            || (split_old == old_hi && split_new == new_hi)
        {
            return false;
        }
        stack.push((split_old, old_hi, split_new, new_hi));
        stack.push((old_lo, split_old, new_lo, split_new));
    }
    true
}

/// Finds one split point on a shortest edit path through the gap.
fn myers_middle_snake(old: &[u32], new: &[u32], budget: &mut usize) -> Option<(usize, usize)> {
    let old_len = isize::try_from(old.len()).ok()?;
    let new_len = isize::try_from(new.len()).ok()?;
    let max_edits = ((old_len + new_len + 1) / 2).min(isize::try_from(MYERS_MAX_EDITS).ok()?);
    if max_edits <= 0 {
        return None;
    }
    let offset = max_edits;
    let width = usize::try_from(2 * max_edits + 1).ok()?;
    let index = |diagonal: isize| -> Option<usize> {
        usize::try_from(diagonal + offset)
            .ok()
            .filter(|value| *value < width)
    };
    let mut forward = vec![-1_isize; width];
    let mut backward = vec![-1_isize; width];
    forward[index(1)?] = 0;
    backward[index(1)?] = 0;
    let delta = old_len - new_len;
    let odd = delta.rem_euclid(2) != 0;
    let (mut forward_lo, mut forward_hi) = (0_isize, 0_isize);
    let (mut backward_lo, mut backward_hi) = (0_isize, 0_isize);

    for distance in 0..=max_edits {
        let mut diagonal = -distance + forward_lo;
        while diagonal <= distance - forward_hi {
            let current = index(diagonal)?;
            // Out-of-range neighbours are only read when the short-circuit
            // above proves they are not used.
            let previous = index(diagonal - 1).unwrap_or(0);
            let next = index(diagonal + 1).unwrap_or(0);
            let mut x = if diagonal == -distance
                || (diagonal != distance && forward[previous] < forward[next])
            {
                forward[next]
            } else {
                forward[previous] + 1
            };
            let mut y = x - diagonal;
            let (mut snake_x, mut snake_y) = (x, y);
            while snake_x < old_len
                && snake_y < new_len
                && old[usize::try_from(snake_x).ok()?] == new[usize::try_from(snake_y).ok()?]
            {
                snake_x += 1;
                snake_y += 1;
            }
            *budget = budget.saturating_sub(usize::try_from(snake_x - x).ok()? + 1);
            if *budget == 0 {
                return None;
            }
            x = snake_x;
            y = snake_y;
            forward[current] = x;
            if x > old_len {
                forward_hi += 2;
            } else if y > new_len {
                forward_lo += 2;
            } else if odd
                && let Some(mirror) = index(delta - diagonal)
                && backward[mirror] != -1
            {
                let backward_x = old_len - backward[mirror];
                if x >= backward_x {
                    return Some((usize::try_from(x).ok()?, usize::try_from(y).ok()?));
                }
            }
            diagonal += 2;
        }

        let mut diagonal = -distance + backward_lo;
        while diagonal <= distance - backward_hi {
            let current = index(diagonal)?;
            let previous = index(diagonal - 1).unwrap_or(0);
            let next = index(diagonal + 1).unwrap_or(0);
            let mut x = if diagonal == -distance
                || (diagonal != distance && backward[previous] < backward[next])
            {
                backward[next]
            } else {
                backward[previous] + 1
            };
            let mut y = x - diagonal;
            let (mut snake_x, mut snake_y) = (x, y);
            while snake_x < old_len
                && snake_y < new_len
                && old[usize::try_from(old_len - snake_x - 1).ok()?]
                    == new[usize::try_from(new_len - snake_y - 1).ok()?]
            {
                snake_x += 1;
                snake_y += 1;
            }
            *budget = budget.saturating_sub(usize::try_from(snake_x - x).ok()? + 1);
            if *budget == 0 {
                return None;
            }
            x = snake_x;
            y = snake_y;
            backward[current] = x;
            if x > old_len {
                backward_hi += 2;
            } else if y > new_len {
                backward_lo += 2;
            } else if !odd
                && let Some(mirror) = index(delta - diagonal)
                && forward[mirror] != -1
            {
                let forward_x = forward[mirror];
                let forward_y = forward_x - (delta - diagonal);
                let backward_x = old_len - x;
                if forward_x >= backward_x {
                    return Some((
                        usize::try_from(forward_x).ok()?,
                        usize::try_from(forward_y).ok()?,
                    ));
                }
            }
            diagonal += 2;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_identical_sequences_entirely() {
        let pairs = align(&[1, 2, 3], &[1, 2, 3]);
        assert_eq!(pairs, [(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn matches_the_gap_around_one_edit() {
        let pairs = align(&[1, 2, 3, 4], &[1, 2, 9, 4]);
        assert_eq!(pairs, [(0, 0), (1, 1), (3, 3)]);
    }

    #[test]
    fn an_ngram_repeated_in_the_new_side_is_not_an_anchor() {
        let runs = unique_ngram_runs::<1>(&[1, 2, 3], &[1, 2, 5, 2], &[], None, None);
        // Only "1" is unique on both sides, and its run grows through "2".
        assert_eq!(
            runs,
            Some(vec![AnchorRun {
                old: 0,
                new: 0,
                len: 2,
                weight: 2,
            }])
        );
    }

    #[test]
    fn information_weights_prefer_rare_tokens() {
        let old = [0, 0, 0, 1, 2];
        let new = [0, 0, 1, 2, 3];
        let weights = information_weights(&old, &new);
        assert!(weights[1] > weights[0]);
        assert!(weights[2] >= weights[1]);
        assert!(weights[3] >= weights[2]);
    }

    #[test]
    fn weighted_lcs_keeps_the_title_next_to_shifted_dot_leaders() {
        // A TOC line: dot leaders, then the title, then the page number. In
        // the new version the page number moved into the dot leader run, so a
        // unit LCS can match more dots by abandoning the title. Dots repeat on
        // both sides and weigh 1; the title words are rare.
        let old = {
            let mut tokens = vec![9; 30];
            tokens.extend([10, 11, 12, 44]);
            tokens
        };
        let new = {
            let mut tokens = vec![9; 29];
            tokens.push(45);
            tokens.extend(vec![9; 30]);
            tokens.extend([10, 11, 12]);
            tokens
        };
        let weights = information_weights(&old, &new);
        let pairs = align_weighted(&old, &new, &weights);
        let title_old = old.len() - 4;
        let title_new = new.len() - 3;
        for offset in 0..3 {
            assert!(
                pairs.contains(&(title_old + offset, title_new + offset)),
                "the title must stay matched to itself: {pairs:?}"
            );
        }
    }

    #[test]
    fn repeated_tokens_fall_back_to_lcs() {
        let pairs = align(&[7, 7, 7, 8], &[7, 7, 9, 8]);
        assert!(pairs.windows(2).all(|window| window[0] < window[1]));
        assert!(pairs.iter().any(|&(old, new)| old == 3 && new == 3));
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn anchors_are_unique_in_both_sides_and_chain_in_order() {
        let mut old = Vec::new();
        let mut new = Vec::new();
        for index in 0..500u32 {
            old.push(index);
            new.push(index);
        }
        let pairs = align(&old, &new);
        assert_eq!(pairs.len(), 500);
        assert!(pairs.windows(2).all(|window| window[0] < window[1]));
    }

    #[test]
    fn shifted_page_numbers_do_not_outweigh_words() {
        // A table-of-contents pattern: words are repeated, page numbers are
        // unique on each side but shifted. The chain must follow the words.
        let old = vec![
            10, 413, 11, 12, 13, 14, 15, 414, 16, 415, 17, 416, 18, 19, 20, 417,
        ];
        let new = vec![
            10, 415, 11, 12, 13, 14, 15, 416, 16, 417, 17, 418, 18, 19, 20, 419,
        ];
        let pairs = align(&old, &new);
        for (old_index, new_index) in &pairs {
            assert!(
                old[*old_index] >= 10 && new[*new_index] >= 10,
                "numbers must not pair across shifted entries"
            );
        }
        // Every word token pairs; all five shifted page numbers stay unmatched.
        assert_eq!(pairs.len(), old.len() - 5);
    }

    #[test]
    fn small_gaps_are_exact_even_with_unique_anchors() {
        let old = vec![1, 2, 3, 4, 5, 9, 6, 7, 8];
        let new = vec![1, 2, 3, 4, 5, 8, 6, 7, 10];
        let pairs = align(&old, &new);
        assert_eq!(pairs.len(), 7);
        assert_eq!(pairs[5], (6, 6));
    }

    #[test]
    fn longest_increasing_subsequence_is_strictly_increasing() {
        let selected = longest_increasing_subsequence(&[3, 1, 2, 8, 5, 6]);
        assert_eq!(selected, [(1, 1), (2, 2), (4, 5), (5, 6)]);
    }

    fn longest_increasing_subsequence(values: &[usize]) -> Vec<(usize, usize)> {
        let mut tails: Vec<usize> = Vec::new();
        let mut previous: Vec<Option<usize>> = vec![None; values.len()];
        for (index, &value) in values.iter().enumerate() {
            let position = tails.partition_point(|&tail| values[tail] < value);
            if position > 0 {
                previous[index] = Some(tails[position - 1]);
            }
            if position == tails.len() {
                tails.push(index);
            } else {
                tails[position] = index;
            }
        }
        let mut selected = Vec::with_capacity(tails.len());
        let mut cursor = tails.last().copied();
        while let Some(index) = cursor {
            selected.push((index, values[index]));
            cursor = previous[index];
        }
        selected.reverse();
        selected
    }

    fn lcs_len(old: &[u32], new: &[u32]) -> usize {
        let width = new.len() + 1;
        let mut table = vec![0usize; (old.len() + 1) * width];
        for i in (0..old.len()).rev() {
            for j in (0..new.len()).rev() {
                table[i * width + j] = if old[i] == new[j] {
                    table[(i + 1) * width + j + 1] + 1
                } else {
                    table[(i + 1) * width + j].max(table[i * width + j + 1])
                };
            }
        }
        table[0]
    }

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
    }

    #[test]
    fn randomized_alignments_are_strictly_increasing_and_equal() {
        for seed in 0..100 {
            let mut lcg = Lcg(seed);
            let old_len = (lcg.next() >> 33) as usize % 40;
            let new_len = (lcg.next() >> 33) as usize % 40;
            let old = (0..old_len)
                .map(|_| (lcg.next() >> 33) as u32 % 12)
                .collect::<Vec<_>>();
            let new = (0..new_len)
                .map(|_| (lcg.next() >> 33) as u32 % 12)
                .collect::<Vec<_>>();
            let pairs = align(&old, &new);
            for &(i, j) in &pairs {
                assert_eq!(old[i], new[j], "seed {seed}");
            }
            assert!(
                pairs
                    .windows(2)
                    .all(|window| { window[0].0 < window[1].0 && window[0].1 < window[1].1 })
            );
        }
    }

    #[test]
    fn small_alignments_are_optimal() {
        for seed in 0..200 {
            let mut lcg = Lcg(seed ^ 0x9e37_79b9);
            let old_len = (lcg.next() >> 33) as usize % 30;
            let new_len = (lcg.next() >> 33) as usize % 30;
            let old = (0..old_len)
                .map(|_| (lcg.next() >> 33) as u32 % 5)
                .collect::<Vec<_>>();
            let new = (0..new_len)
                .map(|_| (lcg.next() >> 33) as u32 % 5)
                .collect::<Vec<_>>();
            let pairs = align(&old, &new);
            assert_eq!(pairs.len(), lcs_len(&old, &new), "seed {seed}");
        }
    }

    #[test]
    fn residual_lcs_matches_low_weight_tokens() {
        let old = [0, 1, 2, 3, 4];
        let new = [0, 2, 1, 3, 4];
        let mut budget = RESIDUAL_GAP_TOTAL_CELLS;
        let pairs = residual_lcs(&old, &new, &mut budget);
        assert_eq!(pairs.len(), 4);
        assert!(pairs.iter().all(|&(i, j)| old[i] == new[j]));
        assert!(pairs.windows(2).all(|window| window[0] < window[1]));
        assert!(budget < RESIDUAL_GAP_TOTAL_CELLS);
    }

    #[test]
    fn cached_segment_alignment_matches_a_fresh_alignment() {
        // The cache must return exactly the segmentation result of
        // `align_with_fixed`, including the segments it reuses from an earlier
        // round whose forced pairs were a subset of this one.
        let mut lcg = Lcg(11);
        let old = (0..600)
            .map(|_| (lcg.next() >> 33) as u32 % 23)
            .collect::<Vec<_>>();
        let mut new = old.clone();
        for index in (0..600).step_by(7) {
            new[index] = (lcg.next() >> 33) as u32 % 29;
        }
        let weights = vec![1u64; 64];
        let mut segments = SegmentCache::new();
        let mut warm = Vec::new();
        for index in (0..600).step_by(6) {
            if old[index] == new[index] {
                warm.push((index, index));
            }
        }
        assert!(warm.len() >= SEGMENT_CACHE_MIN_FIXED);
        let first = align_with_fixed_cached(&old, &new, &weights, &warm, &mut segments);
        assert_eq!(first, align_with_fixed(&old, &new, &weights, &warm));
        // A superset of the forced pairs must reuse the unchanged segments and
        // still match the uncached result exactly.
        let mut denser = warm.clone();
        for index in (0..600).step_by(6) {
            if old[index] == new[index] && !denser.contains(&(index, index)) {
                denser.push((index, index));
            }
        }
        denser.sort_unstable();
        let second = align_with_fixed_cached(&old, &new, &weights, &denser, &mut segments);
        assert_eq!(second, align_with_fixed(&old, &new, &weights, &denser));
    }

    #[test]
    fn myers_fallback_finds_a_shortest_script() {
        let mut lcg = Lcg(7);
        let old = (0..400)
            .map(|_| (lcg.next() >> 33) as u32 % 9)
            .collect::<Vec<_>>();
        let new = (0..380)
            .map(|_| (lcg.next() >> 33) as u32 % 9)
            .collect::<Vec<_>>();
        let mut matches = Vec::new();
        assert!(bounded_myers(&old, &new, 0, 0, &mut matches));
        assert_eq!(matches.len(), lcs_len(&old, &new));
    }
}
