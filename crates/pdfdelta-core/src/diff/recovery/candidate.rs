use std::{collections::HashMap, mem::size_of};

#[cfg(test)]
use std::collections::HashSet;

use super::score::{
    LINE_NGRAM_SIZE, MIN_WORD_SCORE_EDGE_EVIDENCE, line_trigram_candidate_meets_threshold,
    ngram_count,
};
use crate::diff::sentence::{
    OccurrenceRole, RecoveryBudget, RecoveryUnitKind, SentenceEvidenceToken, SentenceOccurrence,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::diff) enum CandidatePostingBucket {
    Global,
    Span(Option<usize>),
    Paired(PairedInterval),
    PairedStream(usize),
}

#[derive(Clone, Copy)]
pub(in crate::diff) enum CandidatePostingIndexScope<'a> {
    Global,
    Span,
    Paired(&'a [Option<PairedInterval>]),
    PairedStream(&'a [Option<PairedInterval>]),
}

pub(in crate::diff) struct UnitCandidateIndex {
    pub(in crate::diff) edge_postings: HashMap<
        (
            CandidatePostingBucket,
            RecoveryUnitKind,
            OccurrenceRole,
            SentenceEvidenceToken,
        ),
        Vec<EdgePosting>,
    >,
    pub(in crate::diff) line_trigram_postings: HashMap<
        (
            CandidatePostingBucket,
            RecoveryUnitKind,
            OccurrenceRole,
            [SentenceEvidenceToken; LINE_NGRAM_SIZE],
        ),
        Vec<LineTrigramPosting>,
    >,
    line_trigram_counts: Vec<usize>,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct EdgePosting(usize);

impl EdgePosting {
    const SIDE_BITS: u32 = 2;
    const SIDE_MASK: usize = (1 << Self::SIDE_BITS) - 1;

    fn new(occurrence_index: usize, sides: EdgePostingSides) -> Option<Self> {
        if occurrence_index > usize::MAX >> Self::SIDE_BITS {
            return None;
        }
        Some(Self(
            (occurrence_index << Self::SIDE_BITS) | usize::from(sides.0),
        ))
    }

    fn occurrence_index(self) -> usize {
        self.0 >> Self::SIDE_BITS
    }

    fn sides(self) -> EdgePostingSides {
        EdgePostingSides((self.0 & Self::SIDE_MASK) as u8)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EdgePostingSides(u8);

impl EdgePostingSides {
    const FIRST: Self = Self(1);
    const LAST: Self = Self(2);
    const BOTH: Self = Self(Self::FIRST.0 | Self::LAST.0);

    fn contains(self, side: Self) -> bool {
        self.0 & side.0 != 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::diff) struct AlignedSentenceEdgeFacts {
    prefix_equal: bool,
    suffix_equal: bool,
}

fn edge_posting(postings: &[EdgePosting], occurrence_index: usize) -> Option<&EdgePosting> {
    postings
        .binary_search_by_key(&occurrence_index, |posting| posting.occurrence_index())
        .ok()
        .and_then(|index| postings.get(index))
}

impl AlignedSentenceEdgeFacts {
    pub(in crate::diff) fn prefix_equal(self) -> bool {
        self.prefix_equal
    }

    pub(in crate::diff) fn suffix_equal(self) -> bool {
        self.suffix_equal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SentenceEdgeSignatureScope {
    Global,
    Span,
    Paired,
    PairedStream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SentenceEdgeSignatureKey {
    group: usize,
    signature: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SentenceEdgeSignatureBuildEntry {
    key: SentenceEdgeSignatureKey,
    occurrence_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SentenceEdgeSignatureRange {
    key: SentenceEdgeSignatureKey,
    start: usize,
}

#[derive(Default)]
struct SentenceEdgeSignaturePostings {
    ranges: Vec<SentenceEdgeSignatureRange>,
    occurrences: Vec<usize>,
}

struct SentenceEdgeSignatureBuild {
    scope: SentenceEdgeSignatureScope,
    own: Vec<SentenceEdgeSignatureBuildEntry>,
    all: Vec<SentenceEdgeSignatureBuildEntry>,
    posting_items: usize,
    depth_1_posting_items: usize,
    depth_2_to_3_posting_items: usize,
    depth_4_plus_posting_items: usize,
}

impl SentenceEdgeSignatureBuild {
    fn empty(scope: SentenceEdgeSignatureScope) -> Self {
        Self {
            scope,
            own: Vec::new(),
            all: Vec::new(),
            posting_items: 0,
            depth_1_posting_items: 0,
            depth_2_to_3_posting_items: 0,
            depth_4_plus_posting_items: 0,
        }
    }

    fn active_metrics(
        &self,
    ) -> Result<SentenceEdgeSignatureIndexMetrics, SentenceEdgeSignatureIndexError> {
        let estimated_logical_bytes = size_of::<SentenceEdgeSignatureIndex>()
            .checked_add(
                self.own
                    .capacity()
                    .checked_add(self.all.capacity())
                    .and_then(|capacity| {
                        capacity.checked_mul(size_of::<SentenceEdgeSignatureBuildEntry>())
                    })
                    .ok_or_else(signature_metric_overflow)?,
            )
            .ok_or_else(signature_metric_overflow)?;
        Ok(SentenceEdgeSignatureIndexMetrics {
            posting_items: self.posting_items,
            own_key_capacity: self.own.capacity(),
            all_key_capacity: self.all.capacity(),
            own_posting_items: self.own.len(),
            all_posting_items: self.all.len(),
            depth_1_posting_items: self.depth_1_posting_items,
            depth_2_to_3_posting_items: self.depth_2_to_3_posting_items,
            depth_4_plus_posting_items: self.depth_4_plus_posting_items,
            estimated_logical_bytes,
            ..SentenceEdgeSignatureIndexMetrics::default()
        })
    }
}

fn sentence_edge_signature_temp_bytes(
    own_capacity: usize,
    all_capacity: usize,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    size_of::<SentenceEdgeSignatureIndex>()
        .checked_add(
            own_capacity
                .checked_add(all_capacity)
                .and_then(|capacity| {
                    capacity.checked_mul(size_of::<SentenceEdgeSignatureBuildEntry>())
                })
                .ok_or_else(signature_metric_overflow)?,
        )
        .ok_or_else(signature_metric_overflow)
}

fn reserve_sentence_edge_signature_build(
    build: &mut SentenceEdgeSignatureBuild,
    own: usize,
    all: usize,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    build.own.try_reserve_exact(own).map_err(|_| {
        SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: 0,
            attempted: own,
        }
    })?;
    build.all.try_reserve_exact(all).map_err(|_| {
        SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: build.own.capacity(),
            attempted: own.saturating_add(all),
        }
    })
}

fn reserve_sentence_edge_signature_build_with_limits(
    build: &mut SentenceEdgeSignatureBuild,
    own: usize,
    all: usize,
    limits: SentenceEdgeSignatureIndexBuildLimits,
) -> Result<(), SentenceEdgeSignatureIndexBuildError> {
    build.own.try_reserve_exact(own).map_err(|_| {
        SentenceEdgeSignatureIndexBuildError::Index(
            SentenceEdgeSignatureIndexError::AllocationFailure {
                examined: 0,
                attempted: own,
            },
        )
    })?;
    let active = build
        .active_metrics()
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    let own_actual_peak = sentence_edge_signature_temp_bytes(build.own.capacity(), all)
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    if own_actual_peak > limits.estimated_logical_bytes {
        return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined: active.estimated_logical_bytes,
            attempted: own_actual_peak,
            progress: active,
        });
    }

    build.all.try_reserve_exact(all).map_err(|_| {
        SentenceEdgeSignatureIndexBuildError::Index(
            SentenceEdgeSignatureIndexError::AllocationFailure {
                examined: build.own.capacity(),
                attempted: own.saturating_add(all),
            },
        )
    })?;
    let active = build
        .active_metrics()
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    if active.estimated_logical_bytes > limits.estimated_logical_bytes {
        return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined: own_actual_peak,
            attempted: active.estimated_logical_bytes,
            progress: active,
        });
    }
    Ok(())
}

fn collect_sentence_edge_signature_build(
    build: &mut SentenceEdgeSignatureBuild,
    occurrences: &[SentenceOccurrence],
    scope: CandidatePostingIndexScope<'_>,
    signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
        if occurrence.kind != RecoveryUnitKind::Sentence {
            continue;
        }
        let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
            continue;
        };
        let Some(bucket) = candidate_posting_bucket(scope, occurrence_index, occurrence).ok_or(
            SentenceEdgeSignatureIndexError::InvalidScope {
                examined: build.posting_items,
                attempted: build.posting_items,
            },
        )?
        else {
            continue;
        };
        collect_sentence_edge_signature_unit(
            build,
            bucket,
            role,
            &occurrence.tokens,
            occurrence_index,
            signature_step,
            allocation_failure_after,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_sentence_edge_signature_unit(
    build: &mut SentenceEdgeSignatureBuild,
    bucket: CandidatePostingBucket,
    role: OccurrenceRole,
    tokens: &[SentenceEvidenceToken],
    occurrence_index: usize,
    signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    let own_depth = sentence_edge_signature_depth(tokens.len()).ok_or(
        SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: build.posting_items,
            attempted: build.posting_items,
        },
    )?;
    let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
    let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
    for depth in 1..=own_depth {
        prefix = signature_step(prefix, tokens[depth - 1]);
        suffix = signature_step(suffix, tokens[tokens.len() - depth]);
        push_sentence_edge_signature_build_entry(
            build,
            depth == own_depth,
            bucket,
            role,
            depth,
            prefix,
            occurrence_index,
            allocation_failure_after,
        )?;
        if suffix != prefix {
            push_sentence_edge_signature_build_entry(
                build,
                depth == own_depth,
                bucket,
                role,
                depth,
                suffix,
                occurrence_index,
                allocation_failure_after,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_sentence_edge_signature_build_entry(
    build: &mut SentenceEdgeSignatureBuild,
    own: bool,
    bucket: CandidatePostingBucket,
    role: OccurrenceRole,
    depth: usize,
    signature: u64,
    occurrence_index: usize,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    let attempted = build.posting_items.checked_add(1).ok_or(
        SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: build.posting_items,
            attempted: usize::MAX,
        },
    )?;
    if allocation_failure_after.is_some_and(|limit| build.posting_items >= limit) {
        return Err(SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: build.posting_items,
            attempted,
        });
    }
    let key = sentence_edge_signature_key(
        build.scope,
        bucket,
        role,
        depth,
        signature,
        build.posting_items,
        attempted,
    )?;
    let entries = if own { &mut build.own } else { &mut build.all };
    if entries.len() == entries.capacity() {
        return Err(SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: build.posting_items,
            attempted,
        });
    }
    entries.push(SentenceEdgeSignatureBuildEntry {
        key,
        occurrence_index,
    });
    let depth_items = match sentence_edge_signature_depth_band(depth) {
        Some(SentenceEdgeSignatureDepthBand::One) => &mut build.depth_1_posting_items,
        Some(SentenceEdgeSignatureDepthBand::TwoToThree) => &mut build.depth_2_to_3_posting_items,
        Some(SentenceEdgeSignatureDepthBand::FourOrMore) => &mut build.depth_4_plus_posting_items,
        None => return Err(signature_metric_overflow()),
    };
    *depth_items = depth_items
        .checked_add(1)
        .ok_or_else(signature_metric_overflow)?;
    build.posting_items = attempted;
    Ok(())
}

fn count_distinct_signature_build_keys(entries: &[SentenceEdgeSignatureBuildEntry]) -> usize {
    entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| *index == 0 || entries[*index - 1].key != entry.key)
        .count()
}

fn sentence_edge_signature_transition_bytes(
    build: &SentenceEdgeSignatureBuild,
    own_range_capacity: usize,
    all_range_capacity: usize,
    posting_capacity: usize,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    build
        .active_metrics()?
        .estimated_logical_bytes
        .checked_add(
            own_range_capacity
                .checked_add(all_range_capacity)
                .and_then(|capacity| capacity.checked_mul(size_of::<SentenceEdgeSignatureRange>()))
                .ok_or_else(signature_metric_overflow)?,
        )
        .and_then(|bytes| {
            posting_capacity
                .checked_mul(size_of::<usize>())
                .and_then(|posting_bytes| bytes.checked_add(posting_bytes))
        })
        .ok_or_else(signature_metric_overflow)
}

fn sentence_edge_signature_transition_progress(
    build: &SentenceEdgeSignatureBuild,
    own_range_capacity: usize,
    all_range_capacity: usize,
    own_occurrence_capacity: usize,
    all_occurrence_capacity: usize,
    own_distinct: usize,
    all_distinct: usize,
) -> Result<SentenceEdgeSignatureIndexMetrics, SentenceEdgeSignatureIndexError> {
    let mut progress = build.active_metrics()?;
    progress.own_distinct_keys = own_distinct;
    progress.all_distinct_keys = all_distinct;
    progress.own_key_capacity = progress
        .own_key_capacity
        .checked_add(own_range_capacity)
        .ok_or_else(signature_metric_overflow)?;
    progress.all_key_capacity = progress
        .all_key_capacity
        .checked_add(all_range_capacity)
        .ok_or_else(signature_metric_overflow)?;
    progress.posting_capacity_items = own_occurrence_capacity
        .checked_add(all_occurrence_capacity)
        .ok_or_else(signature_metric_overflow)?;
    progress.estimated_logical_bytes = sentence_edge_signature_transition_bytes(
        build,
        own_range_capacity,
        all_range_capacity,
        progress.posting_capacity_items,
    )?;
    Ok(progress)
}

fn validate_sentence_edge_signature_transition_peak(
    build: &SentenceEdgeSignatureBuild,
    progress: SentenceEdgeSignatureIndexMetrics,
    attempted_own_range_capacity: usize,
    attempted_all_range_capacity: usize,
    attempted_posting_capacity: usize,
    limits: Option<SentenceEdgeSignatureIndexBuildLimits>,
) -> Result<(), SentenceEdgeSignatureIndexBuildError> {
    let attempted = sentence_edge_signature_transition_bytes(
        build,
        attempted_own_range_capacity,
        attempted_all_range_capacity,
        attempted_posting_capacity,
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    if limits.is_some_and(|limits| attempted > limits.estimated_logical_bytes) {
        return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined: progress.estimated_logical_bytes,
            attempted,
            progress,
        });
    }
    Ok(())
}

fn finalize_sentence_edge_signature_build(
    mut build: SentenceEdgeSignatureBuild,
    limits: Option<SentenceEdgeSignatureIndexBuildLimits>,
) -> Result<SentenceEdgeSignatureIndex, SentenceEdgeSignatureIndexBuildError> {
    build.own.sort_unstable();
    build.all.sort_unstable();
    let own_distinct = count_distinct_signature_build_keys(&build.own);
    let all_distinct = count_distinct_signature_build_keys(&build.all);
    let mut progress = build
        .active_metrics()
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    progress.own_distinct_keys = own_distinct;
    progress.all_distinct_keys = all_distinct;
    let distinct_keys = own_distinct.checked_add(all_distinct).ok_or(
        SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
            examined: usize::MAX,
            attempted: usize::MAX,
            progress,
        },
    )?;
    if let Some(limits) = limits.filter(|limits| distinct_keys > limits.distinct_keys) {
        return Err(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
            examined: limits.distinct_keys,
            attempted: distinct_keys,
            progress,
        });
    }
    validate_sentence_edge_signature_transition_peak(
        &build,
        progress,
        own_distinct,
        all_distinct,
        build.posting_items,
        limits,
    )?;

    let mut own = SentenceEdgeSignaturePostings::default();
    let mut all = SentenceEdgeSignaturePostings::default();
    let attempted = sentence_edge_signature_transition_bytes(&build, own_distinct, 0, 0)
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    reserve_final_sentence_edge_signature_ranges(&mut own, own_distinct, build.posting_items)
        .map_err(
            |_| SentenceEdgeSignatureIndexBuildError::AllocationFailure {
                examined: progress.estimated_logical_bytes,
                attempted,
                progress,
            },
        )?;
    progress = sentence_edge_signature_transition_progress(
        &build,
        own.ranges.capacity(),
        0,
        0,
        0,
        own_distinct,
        all_distinct,
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    validate_sentence_edge_signature_transition_peak(
        &build,
        progress,
        own.ranges.capacity(),
        all_distinct,
        build.posting_items,
        limits,
    )?;
    let attempted =
        sentence_edge_signature_transition_bytes(&build, own.ranges.capacity(), all_distinct, 0)
            .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    reserve_final_sentence_edge_signature_ranges(&mut all, all_distinct, build.posting_items)
        .map_err(
            |_| SentenceEdgeSignatureIndexBuildError::AllocationFailure {
                examined: progress.estimated_logical_bytes,
                attempted,
                progress,
            },
        )?;
    progress = sentence_edge_signature_transition_progress(
        &build,
        own.ranges.capacity(),
        all.ranges.capacity(),
        0,
        0,
        own_distinct,
        all_distinct,
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    validate_sentence_edge_signature_transition_peak(
        &build,
        progress,
        own.ranges.capacity(),
        all.ranges.capacity(),
        build.posting_items,
        limits,
    )?;
    let attempted = sentence_edge_signature_transition_bytes(
        &build,
        own.ranges.capacity(),
        all.ranges.capacity(),
        build.own.len(),
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    reserve_final_sentence_edge_signature_occurrences(
        &mut own,
        build.own.len(),
        build.posting_items,
    )
    .map_err(
        |_| SentenceEdgeSignatureIndexBuildError::AllocationFailure {
            examined: progress.estimated_logical_bytes,
            attempted,
            progress,
        },
    )?;
    progress = sentence_edge_signature_transition_progress(
        &build,
        own.ranges.capacity(),
        all.ranges.capacity(),
        own.occurrences.capacity(),
        0,
        own_distinct,
        all_distinct,
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    let remaining_occurrences = own
        .occurrences
        .capacity()
        .checked_add(build.all.len())
        .ok_or_else(|| SentenceEdgeSignatureIndexBuildError::Index(signature_metric_overflow()))?;
    validate_sentence_edge_signature_transition_peak(
        &build,
        progress,
        own.ranges.capacity(),
        all.ranges.capacity(),
        remaining_occurrences,
        limits,
    )?;
    let attempted = sentence_edge_signature_transition_bytes(
        &build,
        own.ranges.capacity(),
        all.ranges.capacity(),
        remaining_occurrences,
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    reserve_final_sentence_edge_signature_occurrences(
        &mut all,
        build.all.len(),
        build.posting_items,
    )
    .map_err(
        |_| SentenceEdgeSignatureIndexBuildError::AllocationFailure {
            examined: progress.estimated_logical_bytes,
            attempted,
            progress,
        },
    )?;
    progress = sentence_edge_signature_transition_progress(
        &build,
        own.ranges.capacity(),
        all.ranges.capacity(),
        own.occurrences.capacity(),
        all.occurrences.capacity(),
        own_distinct,
        all_distinct,
    )
    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    validate_sentence_edge_signature_transition_peak(
        &build,
        progress,
        own.ranges.capacity(),
        all.ranges.capacity(),
        own.occurrences
            .capacity()
            .checked_add(all.occurrences.capacity())
            .ok_or_else(|| {
                SentenceEdgeSignatureIndexBuildError::Index(signature_metric_overflow())
            })?,
        limits,
    )?;
    fill_final_sentence_edge_signature_postings(&build.own, &mut own);
    fill_final_sentence_edge_signature_postings(&build.all, &mut all);
    let mut index = SentenceEdgeSignatureIndex {
        scope: build.scope,
        own_depth_postings: own,
        all_depth_postings: all,
        metrics: SentenceEdgeSignatureIndexMetrics {
            posting_items: build.posting_items,
            own_distinct_keys: own_distinct,
            all_distinct_keys: all_distinct,
            own_posting_items: build.own.len(),
            all_posting_items: build.all.len(),
            depth_1_posting_items: build.depth_1_posting_items,
            depth_2_to_3_posting_items: build.depth_2_to_3_posting_items,
            depth_4_plus_posting_items: build.depth_4_plus_posting_items,
            ..SentenceEdgeSignatureIndexMetrics::default()
        },
    };
    index
        .refresh_capacity_metrics()
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    let shape = sentence_edge_signature_range_metrics(&index.own_depth_postings)
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    let all_shape = sentence_edge_signature_range_metrics(&index.all_depth_postings)
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
    index.metrics.largest_posting = shape.largest_posting.max(all_shape.largest_posting);
    if let Some(limits) = limits {
        validate_sentence_edge_signature_build_limits(index.metrics, limits)?;
    }
    Ok(index)
}

fn reserve_final_sentence_edge_signature_ranges(
    postings: &mut SentenceEdgeSignaturePostings,
    ranges: usize,
    attempted: usize,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    postings.ranges.try_reserve_exact(ranges).map_err(|_| {
        SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: 0,
            attempted,
        }
    })
}

fn reserve_final_sentence_edge_signature_occurrences(
    postings: &mut SentenceEdgeSignaturePostings,
    occurrences: usize,
    attempted: usize,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    postings
        .occurrences
        .try_reserve_exact(occurrences)
        .map_err(|_| SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: 0,
            attempted,
        })
}

fn fill_final_sentence_edge_signature_postings(
    source: &[SentenceEdgeSignatureBuildEntry],
    target: &mut SentenceEdgeSignaturePostings,
) {
    for (offset, entry) in source.iter().enumerate() {
        if offset == 0 || source[offset - 1].key != entry.key {
            target.ranges.push(SentenceEdgeSignatureRange {
                key: entry.key,
                start: offset,
            });
        }
        target.occurrences.push(entry.occurrence_index);
    }
}

/// Diagnostic index for sentence pairs that can satisfy the edge-evidence gate.
///
/// This remains separate from [`UnitCandidateIndex`] so production candidate
/// construction has no additional allocation or runtime work.
#[allow(dead_code)]
pub(in crate::diff) struct SentenceEdgeSignatureIndex {
    scope: SentenceEdgeSignatureScope,
    own_depth_postings: SentenceEdgeSignaturePostings,
    all_depth_postings: SentenceEdgeSignaturePostings,
    metrics: SentenceEdgeSignatureIndexMetrics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::diff) struct SentenceEdgeSignatureIndexMetrics {
    pub(in crate::diff) posting_items: usize,
    pub(in crate::diff) own_distinct_keys: usize,
    pub(in crate::diff) all_distinct_keys: usize,
    /// Reserved own-depth key slots in the active build representation.
    ///
    /// Completed indexes report compact range capacity. Stopped builds report
    /// temporary-entry capacity plus any compact range capacity already
    /// allocated during the final transition.
    pub(in crate::diff) own_key_capacity: usize,
    /// Reserved all-depth key slots in the active build representation.
    ///
    /// Completed indexes report compact range capacity. Stopped builds report
    /// temporary-entry capacity plus any compact range capacity already
    /// allocated during the final transition.
    pub(in crate::diff) all_key_capacity: usize,
    pub(in crate::diff) own_posting_items: usize,
    pub(in crate::diff) all_posting_items: usize,
    /// Reserved occurrence-index slots across both posting arrays.
    pub(in crate::diff) posting_capacity_items: usize,
    pub(in crate::diff) largest_posting: usize,
    pub(in crate::diff) depth_1_posting_items: usize,
    pub(in crate::diff) depth_2_to_3_posting_items: usize,
    pub(in crate::diff) depth_4_plus_posting_items: usize,
    /// Logical bytes derived from the active index-build representation.
    ///
    /// Completed indexes count `size_of::<SentenceEdgeSignatureIndex>()`, the
    /// retained capacity of both compact range tables, and both occurrence
    /// arrays. Build progress includes the same capacities, so construction
    /// peaks are not hidden.
    /// Allocator metadata is intentionally excluded because the standard
    /// library does not expose it.
    pub(in crate::diff) estimated_logical_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::diff) struct SentenceEdgeSignatureQueryMetrics {
    pub(in crate::diff) posting_visits: usize,
    pub(in crate::diff) candidate_union: usize,
    pub(in crate::diff) depth_band: Option<SentenceEdgeSignatureDepthBand>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum SentenceEdgeSignatureDepthBand {
    One,
    TwoToThree,
    FourOrMore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct SentenceEdgeSignatureIndexBuildLimits {
    pub(in crate::diff) posting_items: usize,
    pub(in crate::diff) distinct_keys: usize,
    pub(in crate::diff) estimated_logical_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum SentenceEdgeSignatureIndexBuildError {
    PostingLimit {
        examined: usize,
        attempted: usize,
    },
    DistinctKeyLimit {
        examined: usize,
        attempted: usize,
        progress: SentenceEdgeSignatureIndexMetrics,
    },
    EstimatedByteLimit {
        examined: usize,
        attempted: usize,
        progress: SentenceEdgeSignatureIndexMetrics,
    },
    AllocationFailure {
        examined: usize,
        attempted: usize,
        progress: SentenceEdgeSignatureIndexMetrics,
    },
    Index(SentenceEdgeSignatureIndexError),
}

#[allow(dead_code)]
impl SentenceEdgeSignatureIndexBuildError {
    pub(in crate::diff) fn work(self) -> (usize, usize) {
        match self {
            Self::PostingLimit {
                examined,
                attempted,
            }
            | Self::DistinctKeyLimit {
                examined,
                attempted,
                ..
            }
            | Self::EstimatedByteLimit {
                examined,
                attempted,
                ..
            }
            | Self::AllocationFailure {
                examined,
                attempted,
                ..
            } => (examined, attempted),
            Self::Index(error) => error.work(),
        }
    }
}

#[derive(Default)]
struct SentenceEdgeSignatureRangeMetrics {
    distinct_keys: usize,
    largest_posting: usize,
}

impl SentenceEdgeSignatureIndexBuildError {
    pub(in crate::diff) fn progress(self) -> Option<SentenceEdgeSignatureIndexMetrics> {
        match self {
            Self::DistinctKeyLimit { progress, .. }
            | Self::EstimatedByteLimit { progress, .. }
            | Self::AllocationFailure { progress, .. } => Some(progress),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum SentenceEdgeSignatureIndexError {
    PostingLimit { examined: usize, attempted: usize },
    QueryVisitLimit { examined: usize, attempted: usize },
    AllocationFailure { examined: usize, attempted: usize },
    CounterOverflow { examined: usize, attempted: usize },
    InvalidScope { examined: usize, attempted: usize },
}

impl SentenceEdgeSignatureIndexError {
    pub(in crate::diff) fn work(self) -> (usize, usize) {
        match self {
            Self::PostingLimit {
                examined,
                attempted,
            }
            | Self::QueryVisitLimit {
                examined,
                attempted,
            }
            | Self::AllocationFailure {
                examined,
                attempted,
            }
            | Self::CounterOverflow {
                examined,
                attempted,
            }
            | Self::InvalidScope {
                examined,
                attempted,
            } => (examined, attempted),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct LineTrigramPosting {
    pub(in crate::diff) occurrence_index: usize,
    pub(in crate::diff) multiplicity: usize,
}

#[derive(Clone, Copy, Default)]
pub(in crate::diff) struct UnitCandidateQueryMetrics {
    pub(in crate::diff) largest_edge_posting: usize,
    pub(in crate::diff) edge_query_union: usize,
    pub(in crate::diff) line_trigram_only_query_union: usize,
    pub(in crate::diff) same_known_edge_query_union: usize,
    pub(in crate::diff) ambiguous_edge_query_union: usize,
    pub(in crate::diff) same_known_line_trigram_only_query_union: usize,
    pub(in crate::diff) ambiguous_line_trigram_only_query_union: usize,
}

#[derive(Clone, Copy)]
pub(in crate::diff) enum CandidatePostingKind {
    Edge,
    LineTrigram,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum NearSearchScope {
    PairedInterval,
    PairedCrossIntervalVeto,
    SameOrAmbiguousSpan,
    CrossSpan,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::diff) struct NearSearchWorkSplit {
    pub(in crate::diff) same_known: usize,
    pub(in crate::diff) ambiguous: usize,
    pub(in crate::diff) shared: usize,
}

impl NearSearchWorkSplit {
    pub(in crate::diff) fn shared(amount: usize) -> Self {
        Self {
            shared: amount,
            ..Self::default()
        }
    }

    pub(in crate::diff) fn total(self) -> Option<usize> {
        self.same_known
            .checked_add(self.ambiguous)?
            .checked_add(self.shared)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) enum NearSearchWorkClass {
    SameKnown,
    Ambiguous,
    Shared,
}

impl NearSearchWorkClass {
    pub(in crate::diff) fn split(self, amount: usize) -> NearSearchWorkSplit {
        match self {
            Self::SameKnown => NearSearchWorkSplit {
                same_known: amount,
                ..NearSearchWorkSplit::default()
            },
            Self::Ambiguous => NearSearchWorkSplit {
                ambiguous: amount,
                ..NearSearchWorkSplit::default()
            },
            Self::Shared => NearSearchWorkSplit::shared(amount),
        }
    }
}

impl UnitCandidateIndex {
    pub(in crate::diff) fn contains_sentence_edge_candidate(
        &self,
        query: &SentenceOccurrence,
        candidate_index: usize,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
    ) -> bool {
        if query.kind != RecoveryUnitKind::Sentence {
            return false;
        }
        let Some(role) = query.role.map(OccurrenceRole::from) else {
            return false;
        };
        let (Some(&first), Some(&last)) = (query.tokens.first(), query.tokens.last()) else {
            return false;
        };
        [Some(bucket), additional_bucket]
            .into_iter()
            .flatten()
            .any(|bucket| {
                [first, last].into_iter().any(|edge| {
                    self.edge_postings
                        .get(&(bucket, query.kind, role, edge))
                        .is_some_and(|postings| edge_posting(postings, candidate_index).is_some())
                })
            })
    }

    /// Returns exact aligned-edge equality facts for an indexed sentence candidate.
    ///
    /// `false` proves inequality because facts are returned only when the
    /// candidate occurs in the complete exact-token posting union for the
    /// requested buckets. Candidates known only through another index return
    /// `None` instead.
    pub(in crate::diff) fn aligned_sentence_edge_facts(
        &self,
        query: &SentenceOccurrence,
        candidate_index: usize,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
    ) -> Option<AlignedSentenceEdgeFacts> {
        let role = query.role.map(OccurrenceRole::from)?;
        let first = *query.tokens.first()?;
        let last = *query.tokens.last()?;
        self.aligned_sentence_edge_facts_for_edges(
            query.kind,
            role,
            first,
            last,
            candidate_index,
            bucket,
            additional_bucket,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn aligned_sentence_edge_facts_for_edges(
        &self,
        kind: RecoveryUnitKind,
        role: OccurrenceRole,
        first: SentenceEvidenceToken,
        last: SentenceEvidenceToken,
        candidate_index: usize,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
    ) -> Option<AlignedSentenceEdgeFacts> {
        if kind != RecoveryUnitKind::Sentence {
            return None;
        }
        let mut indexed = false;
        let mut facts = AlignedSentenceEdgeFacts::default();
        for bucket in [Some(bucket), additional_bucket].into_iter().flatten() {
            if let Some(posting) = self
                .edge_postings
                .get(&(bucket, kind, role, first))
                .and_then(|postings| edge_posting(postings, candidate_index))
            {
                indexed = true;
                facts.prefix_equal |= posting.sides().contains(EdgePostingSides::FIRST);
            }
            if let Some(posting) = self
                .edge_postings
                .get(&(bucket, kind, role, last))
                .and_then(|postings| edge_posting(postings, candidate_index))
            {
                indexed = true;
                facts.suffix_equal |= posting.sides().contains(EdgePostingSides::LAST);
            }
        }
        indexed.then_some(facts)
    }

    pub(in crate::diff) fn new(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
    ) -> Option<Self> {
        let mut index = Self {
            edge_postings: HashMap::new(),
            line_trigram_postings: HashMap::new(),
            line_trigram_counts: Vec::new(),
        };
        index
            .line_trigram_counts
            .try_reserve_exact(occurrences.len())
            .ok()?;
        index.line_trigram_counts.resize(occurrences.len(), 0);
        for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
            let bucket = match scope {
                CandidatePostingIndexScope::Global => CandidatePostingBucket::Global,
                CandidatePostingIndexScope::Span => {
                    CandidatePostingBucket::Span(occurrence.span_index)
                }
                CandidatePostingIndexScope::Paired(intervals) => {
                    let Some(interval) = intervals.get(occurrence_index).copied()? else {
                        continue;
                    };
                    CandidatePostingBucket::Paired(interval)
                }
                CandidatePostingIndexScope::PairedStream(intervals) => {
                    let Some(interval) = intervals.get(occurrence_index).copied()? else {
                        continue;
                    };
                    CandidatePostingBucket::PairedStream(interval.pair_index)
                }
            };
            let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
                continue;
            };
            index.push_occurrence_edges(
                bucket,
                occurrence.kind,
                role,
                &occurrence.tokens,
                occurrence_index,
            )?;
            if occurrence.kind == RecoveryUnitKind::Line {
                index.line_trigram_counts[occurrence_index] =
                    ngram_count(occurrence.tokens.len(), LINE_NGRAM_SIZE)?;
                for window in occurrence.tokens.windows(LINE_NGRAM_SIZE) {
                    let trigram =
                        <[SentenceEvidenceToken; LINE_NGRAM_SIZE]>::try_from(window).ok()?;
                    index.push_line_trigram_posting(
                        (bucket, occurrence.kind, role, trigram),
                        occurrence_index,
                    )?;
                }
            }
        }
        Some(index)
    }

    fn push_occurrence_edges(
        &mut self,
        bucket: CandidatePostingBucket,
        kind: RecoveryUnitKind,
        role: OccurrenceRole,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
    ) -> Option<()> {
        let first = *tokens.first()?;
        let last = *tokens.last()?;
        let first_sides = if last == first {
            EdgePostingSides::BOTH
        } else {
            EdgePostingSides::FIRST
        };
        self.push_edge_posting((bucket, kind, role, first), occurrence_index, first_sides)?;
        if last != first {
            self.push_edge_posting(
                (bucket, kind, role, last),
                occurrence_index,
                EdgePostingSides::LAST,
            )?;
        }
        Some(())
    }

    fn push_edge_posting(
        &mut self,
        key: (
            CandidatePostingBucket,
            RecoveryUnitKind,
            OccurrenceRole,
            SentenceEvidenceToken,
        ),
        occurrence_index: usize,
        sides: EdgePostingSides,
    ) -> Option<()> {
        let posting = EdgePosting::new(occurrence_index, sides)?;
        if !self.edge_postings.contains_key(&key) {
            self.edge_postings.try_reserve(1).ok()?;
            self.edge_postings.insert(key, Vec::new());
        }
        let postings = self.edge_postings.get_mut(&key)?;
        postings.try_reserve(1).ok()?;
        postings.push(posting);
        Some(())
    }

    fn push_line_trigram_posting(
        &mut self,
        key: (
            CandidatePostingBucket,
            RecoveryUnitKind,
            OccurrenceRole,
            [SentenceEvidenceToken; LINE_NGRAM_SIZE],
        ),
        occurrence_index: usize,
    ) -> Option<()> {
        if !self.line_trigram_postings.contains_key(&key) {
            self.line_trigram_postings.try_reserve(1).ok()?;
            self.line_trigram_postings.insert(key, Vec::new());
        }
        let postings = self.line_trigram_postings.get_mut(&key)?;
        // Occurrences are indexed in order, so equal windows are adjacent within
        // one posting list and can retain multiplicity without duplicate entries.
        if let Some(posting) = postings
            .last_mut()
            .filter(|posting| posting.occurrence_index == occurrence_index)
        {
            posting.multiplicity = posting.multiplicity.checked_add(1)?;
            return Some(());
        }
        postings.try_reserve(1).ok()?;
        postings.push(LineTrigramPosting {
            occurrence_index,
            multiplicity: 1,
        });
        Some(())
    }

    #[cfg(test)]
    pub(in crate::diff) fn collect_plausible_occurrences(
        &self,
        plausible: &mut Vec<usize>,
        occurrence: &SentenceOccurrence,
        candidate_occurrences: &[SentenceOccurrence],
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        budget: &mut RecoveryBudget,
    ) -> Option<UnitCandidateQueryMetrics> {
        self.collect_plausible_occurrences_in_scope(
            plausible,
            occurrence,
            candidate_occurrences,
            bucket,
            additional_bucket,
            budget,
            NearSearchScope::SameOrAmbiguousSpan,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::diff) fn collect_plausible_occurrences_in_scope(
        &self,
        plausible: &mut Vec<usize>,
        occurrence: &SentenceOccurrence,
        candidate_occurrences: &[SentenceOccurrence],
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        budget: &mut RecoveryBudget,
        scope: NearSearchScope,
    ) -> Option<UnitCandidateQueryMetrics> {
        plausible.clear();
        let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
            return Some(UnitCandidateQueryMetrics::default());
        };
        let first = *occurrence.tokens.first()?;
        let last = *occurrence.tokens.last()?;
        // A pair satisfying the prefix/suffix threshold must share at least one
        // edge token, so this index prunes work without reducing candidate recall.
        let buckets = [Some(bucket), additional_bucket];
        let (primary_edge_postings, additional_edge_postings, largest_edge_posting) =
            buckets.into_iter().enumerate().try_fold(
                (0usize, 0usize, 0usize),
                |(primary, additional, largest), (bucket_index, bucket)| {
                    let Some(bucket) = bucket else {
                        return Some((primary, additional, largest));
                    };
                    let first_len = self
                        .edge_postings
                        .get(&(bucket, occurrence.kind, role, first))
                        .map_or(0, Vec::len);
                    let last_len = if first == last {
                        0
                    } else {
                        self.edge_postings
                            .get(&(bucket, occurrence.kind, role, last))
                            .map_or(0, Vec::len)
                    };
                    let amount = first_len.checked_add(last_len)?;
                    Some(if bucket_index == 0 {
                        (
                            primary.checked_add(amount)?,
                            additional,
                            largest.max(first_len).max(last_len),
                        )
                    } else {
                        (
                            primary,
                            additional.checked_add(amount)?,
                            largest.max(first_len).max(last_len),
                        )
                    })
                },
            )?;
        let raw_edge_postings = primary_edge_postings.checked_add(additional_edge_postings)?;
        let subdivides_span_buckets = scope == NearSearchScope::SameOrAmbiguousSpan
            && matches!(bucket, CandidatePostingBucket::Span(Some(_)))
            && matches!(additional_bucket, Some(CandidatePostingBucket::Span(None)));
        let edge_split = if subdivides_span_buckets {
            NearSearchWorkSplit {
                same_known: primary_edge_postings,
                ambiguous: additional_edge_postings,
                shared: 0,
            }
        } else {
            NearSearchWorkSplit::shared(raw_edge_postings)
        };
        if !budget.charge_candidate_posting_visits_in_scope_split(
            raw_edge_postings,
            occurrence.kind,
            CandidatePostingKind::Edge,
            scope,
            edge_split,
        ) {
            return None;
        }
        plausible.try_reserve(raw_edge_postings).ok()?;
        for bucket in buckets.into_iter().flatten() {
            if let Some(postings) = self
                .edge_postings
                .get(&(bucket, occurrence.kind, role, first))
            {
                plausible.extend(postings.iter().map(|posting| posting.occurrence_index()));
            }
            if first != last
                && let Some(postings) =
                    self.edge_postings
                        .get(&(bucket, occurrence.kind, role, last))
            {
                plausible.extend(postings.iter().map(|posting| posting.occurrence_index()));
            }
        }
        plausible.sort_unstable();
        plausible.dedup();
        let edge_query_union = plausible.len();
        let edge_query_split =
            split_query_candidates(plausible, occurrence.span_index, candidate_occurrences)?;
        let mut line_trigram_only_query_union = 0;
        let mut final_query_split = edge_query_split;
        if occurrence.kind == RecoveryUnitKind::Line {
            let trigram_count = ngram_count(occurrence.tokens.len(), LINE_NGRAM_SIZE)?;
            if !budget.charge_candidate_posting_visits_in_scope_split(
                trigram_count,
                occurrence.kind,
                CandidatePostingKind::LineTrigram,
                scope,
                NearSearchWorkSplit::shared(trigram_count),
            ) {
                return None;
            }
            let mut query_trigrams = HashMap::new();
            query_trigrams.try_reserve(trigram_count).ok()?;
            for window in occurrence.tokens.windows(LINE_NGRAM_SIZE) {
                let trigram = <[SentenceEvidenceToken; LINE_NGRAM_SIZE]>::try_from(window).ok()?;
                let count = query_trigrams.entry(trigram).or_insert(0usize);
                *count = count.checked_add(1)?;
            }
            // Distinct query keys visit each posting list at most once, bounding
            // temporary entries by the already bounded index corpus.
            let (primary_postings, ambiguous_postings) = query_trigrams.iter().try_fold(
                (0usize, 0usize),
                |(primary_count, additional_count), (trigram, _)| {
                    Some((
                        primary_count.checked_add(
                            self.line_trigram_postings
                                .get(&(bucket, occurrence.kind, role, *trigram))
                                .map_or(0, Vec::len),
                        )?,
                        additional_count.checked_add(additional_bucket.map_or(
                            0,
                            |additional_bucket| {
                                self.line_trigram_postings
                                    .get(&(additional_bucket, occurrence.kind, role, *trigram))
                                    .map_or(0, Vec::len)
                            },
                        ))?,
                    ))
                },
            )?;
            let additional_postings = primary_postings.checked_add(ambiguous_postings)?;
            let posting_split = if subdivides_span_buckets {
                NearSearchWorkSplit {
                    same_known: primary_postings,
                    ambiguous: ambiguous_postings,
                    shared: 0,
                }
            } else {
                NearSearchWorkSplit::shared(additional_postings)
            };
            if !budget.charge_candidate_posting_visits_in_scope_split(
                additional_postings,
                occurrence.kind,
                CandidatePostingKind::LineTrigram,
                scope,
                posting_split,
            ) {
                return None;
            }
            let mut candidate_shared = HashMap::<usize, usize>::new();
            candidate_shared.try_reserve(additional_postings).ok()?;
            for (trigram, query_multiplicity) in query_trigrams {
                for bucket in buckets.into_iter().flatten() {
                    if let Some(postings) =
                        self.line_trigram_postings
                            .get(&(bucket, occurrence.kind, role, trigram))
                    {
                        for posting in postings {
                            let shared = candidate_shared
                                .entry(posting.occurrence_index)
                                .or_insert(0);
                            *shared =
                                shared.checked_add(query_multiplicity.min(posting.multiplicity))?;
                        }
                    }
                }
            }
            let mut trigram_candidates = Vec::new();
            trigram_candidates
                .try_reserve(candidate_shared.len())
                .ok()?;
            for (occurrence_index, shared) in candidate_shared {
                let candidate_ngrams = *self.line_trigram_counts.get(occurrence_index)?;
                if line_trigram_candidate_meets_threshold(shared, trigram_count, candidate_ngrams)?
                {
                    trigram_candidates.push(occurrence_index);
                }
            }
            trigram_candidates.sort_unstable();
            plausible.try_reserve(trigram_candidates.len()).ok()?;
            plausible.extend(trigram_candidates);
            plausible.sort_unstable();
            plausible.dedup();
            line_trigram_only_query_union = plausible.len().checked_sub(edge_query_union)?;
            final_query_split =
                split_query_candidates(plausible, occurrence.span_index, candidate_occurrences)?;
        }
        Some(UnitCandidateQueryMetrics {
            largest_edge_posting,
            edge_query_union,
            line_trigram_only_query_union,
            same_known_edge_query_union: edge_query_split.same_known,
            ambiguous_edge_query_union: edge_query_split.ambiguous,
            same_known_line_trigram_only_query_union: final_query_split
                .same_known
                .checked_sub(edge_query_split.same_known)?,
            ambiguous_line_trigram_only_query_union: final_query_split
                .ambiguous
                .checked_sub(edge_query_split.ambiguous)?,
        })
    }
}

#[allow(dead_code)]
impl SentenceEdgeSignatureIndex {
    pub(in crate::diff) fn posting_upper_bound(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
    ) -> Result<usize, SentenceEdgeSignatureIndexError> {
        let (own, all) = Self::posting_upper_bounds(occurrences, scope)?;
        own.checked_add(all)
            .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 0,
                attempted: usize::MAX,
            })
    }

    fn posting_upper_bounds(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
    ) -> Result<(usize, usize), SentenceEdgeSignatureIndexError> {
        let scope_kind = sentence_edge_signature_scope(scope);
        occurrences.iter().enumerate().try_fold(
            (0usize, 0usize),
            |(own, all), (occurrence_index, occurrence)| {
                if occurrence.kind != RecoveryUnitKind::Sentence || occurrence.role.is_none() {
                    return Ok((own, all));
                }
                let Some(bucket) = candidate_posting_bucket(scope, occurrence_index, occurrence)
                    .ok_or(SentenceEdgeSignatureIndexError::InvalidScope {
                        examined: 0,
                        attempted: own.saturating_add(all),
                    })?
                else {
                    return Ok((own, all));
                };
                compact_sentence_edge_signature_bucket(
                    scope_kind,
                    bucket,
                    0,
                    own.saturating_add(all),
                )?;
                let depth = sentence_edge_signature_depth(occurrence.tokens.len()).ok_or(
                    SentenceEdgeSignatureIndexError::CounterOverflow {
                        examined: 0,
                        attempted: own.saturating_add(all),
                    },
                )?;
                let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
                let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
                let mut postings = 0usize;
                for current in 1..=depth {
                    prefix = sentence_edge_signature_step(prefix, occurrence.tokens[current - 1]);
                    suffix = sentence_edge_signature_step(
                        suffix,
                        occurrence.tokens[occurrence.tokens.len() - current],
                    );
                    postings = postings
                        .checked_add(usize::from(prefix != suffix) + 1)
                        .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
                            examined: 0,
                            attempted: own.saturating_add(all),
                        })?;
                }
                let own_postings = usize::from(depth > 0)
                    .checked_mul(usize::from(prefix != suffix) + 1)
                    .ok_or_else(signature_metric_overflow)?;
                let all_postings = postings
                    .checked_sub(own_postings)
                    .ok_or_else(signature_metric_overflow)?;
                Ok((
                    own.checked_add(own_postings)
                        .ok_or_else(signature_metric_overflow)?,
                    all.checked_add(all_postings)
                        .ok_or_else(signature_metric_overflow)?,
                ))
            },
        )
    }

    pub(in crate::diff) fn new_bounded(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
        posting_limit: usize,
    ) -> Result<Self, SentenceEdgeSignatureIndexError> {
        let attempted = Self::posting_upper_bound(occurrences, scope)?;
        if attempted > posting_limit {
            return Err(SentenceEdgeSignatureIndexError::PostingLimit {
                examined: 0,
                attempted,
            });
        }
        Self::try_new_with_signature_step(occurrences, scope, sentence_edge_signature_step, None)
    }

    pub(in crate::diff) fn new_with_limits(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
        limits: SentenceEdgeSignatureIndexBuildLimits,
    ) -> Result<Self, SentenceEdgeSignatureIndexBuildError> {
        let attempted = Self::posting_upper_bound(occurrences, scope)
            .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        if attempted > limits.posting_items {
            return Err(SentenceEdgeSignatureIndexBuildError::PostingLimit {
                examined: 0,
                attempted,
            });
        }
        let (own_upper_bound, all_upper_bound) = Self::posting_upper_bounds(occurrences, scope)
            .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        let scope_kind = sentence_edge_signature_scope(scope);
        let mut build = SentenceEdgeSignatureBuild::empty(scope_kind);
        let minimum_temp_bytes =
            sentence_edge_signature_temp_bytes(own_upper_bound, all_upper_bound)
                .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        if minimum_temp_bytes > limits.estimated_logical_bytes {
            return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
                examined: size_of::<SentenceEdgeSignatureIndex>(),
                attempted: minimum_temp_bytes,
                progress: build
                    .active_metrics()
                    .map_err(SentenceEdgeSignatureIndexBuildError::Index)?,
            });
        }
        reserve_sentence_edge_signature_build_with_limits(
            &mut build,
            own_upper_bound,
            all_upper_bound,
            limits,
        )?;
        collect_sentence_edge_signature_build(
            &mut build,
            occurrences,
            scope,
            sentence_edge_signature_step,
            None,
        )
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        finalize_sentence_edge_signature_build(build, Some(limits))
    }

    #[cfg(test)]
    pub(in crate::diff) fn new_with_allocation_failure_after(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
        examined_postings: usize,
    ) -> Result<Self, SentenceEdgeSignatureIndexError> {
        Self::try_new_with_signature_step(
            occurrences,
            scope,
            sentence_edge_signature_step,
            Some(examined_postings),
        )
    }

    pub(in crate::diff) fn new(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
    ) -> Option<Self> {
        Self::try_new_with_signature_step(occurrences, scope, sentence_edge_signature_step, None)
            .ok()
    }

    fn try_new_with_signature_step(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
        allocation_failure_after: Option<usize>,
    ) -> Result<Self, SentenceEdgeSignatureIndexError> {
        let (own_upper_bound, all_upper_bound) = Self::posting_upper_bounds(occurrences, scope)?;
        let mut build = SentenceEdgeSignatureBuild::empty(sentence_edge_signature_scope(scope));
        reserve_sentence_edge_signature_build(&mut build, own_upper_bound, all_upper_bound)?;
        collect_sentence_edge_signature_build(
            &mut build,
            occurrences,
            scope,
            signature_step,
            allocation_failure_after,
        )?;
        finalize_sentence_edge_signature_build(build, None).map_err(|error| match error {
            SentenceEdgeSignatureIndexBuildError::Index(error) => error,
            _ => signature_metric_overflow(),
        })
    }

    fn empty(scope: SentenceEdgeSignatureScope) -> Self {
        Self {
            scope,
            own_depth_postings: SentenceEdgeSignaturePostings::default(),
            all_depth_postings: SentenceEdgeSignaturePostings::default(),
            metrics: SentenceEdgeSignatureIndexMetrics::default(),
        }
    }

    pub(in crate::diff) fn metrics(&self) -> SentenceEdgeSignatureIndexMetrics {
        self.metrics
    }

    pub(in crate::diff) fn collect_plausible_occurrences(
        &self,
        plausible: &mut Vec<usize>,
        occurrence: &SentenceOccurrence,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
    ) -> Option<SentenceEdgeSignatureQueryMetrics> {
        plausible.clear();
        if occurrence.kind != RecoveryUnitKind::Sentence {
            return Some(SentenceEdgeSignatureQueryMetrics::default());
        }
        let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
            return Some(SentenceEdgeSignatureQueryMetrics::default());
        };
        self.collect_sentence_tokens(
            plausible,
            &occurrence.tokens,
            role,
            bucket,
            additional_bucket,
            sentence_edge_signature_step,
        )
    }

    pub(in crate::diff) fn collect_plausible_occurrences_bounded(
        &self,
        plausible: &mut Vec<usize>,
        occurrence: &SentenceOccurrence,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        posting_visit_limit: usize,
    ) -> Result<SentenceEdgeSignatureQueryMetrics, SentenceEdgeSignatureIndexError> {
        let attempted = self.query_posting_visits(occurrence, bucket, additional_bucket)?;
        if attempted > posting_visit_limit {
            return Err(SentenceEdgeSignatureIndexError::QueryVisitLimit {
                examined: 0,
                attempted,
            });
        }
        if occurrence.kind != RecoveryUnitKind::Sentence {
            plausible.clear();
            return Ok(SentenceEdgeSignatureQueryMetrics::default());
        }
        let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
            plausible.clear();
            return Ok(SentenceEdgeSignatureQueryMetrics::default());
        };
        self.collect_sentence_tokens_bounded(
            plausible,
            &occurrence.tokens,
            role,
            bucket,
            additional_bucket,
            sentence_edge_signature_step,
            None,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::diff) fn collect_with_allocation_failure_after(
        &self,
        plausible: &mut Vec<usize>,
        occurrence: &SentenceOccurrence,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        examined_postings: usize,
    ) -> Result<SentenceEdgeSignatureQueryMetrics, SentenceEdgeSignatureIndexError> {
        let role = occurrence.role.map(OccurrenceRole::from).ok_or(
            SentenceEdgeSignatureIndexError::InvalidScope {
                examined: 0,
                attempted: 0,
            },
        )?;
        self.collect_sentence_tokens_bounded(
            plausible,
            &occurrence.tokens,
            role,
            bucket,
            additional_bucket,
            sentence_edge_signature_step,
            Some(examined_postings),
        )
    }

    fn query_posting_visits(
        &self,
        occurrence: &SentenceOccurrence,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
    ) -> Result<usize, SentenceEdgeSignatureIndexError> {
        if occurrence.kind != RecoveryUnitKind::Sentence {
            return Ok(0);
        }
        let Some(role) = occurrence.role.map(OccurrenceRole::from) else {
            return Ok(0);
        };
        let depth = sentence_edge_signature_depth(occurrence.tokens.len()).ok_or(
            SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 0,
                attempted: 0,
            },
        )?;
        let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
        let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
        let mut visits = 0usize;
        let buckets = self.compact_query_buckets(bucket, additional_bucket, 0, 0)?;
        for current in 1..=depth {
            prefix = sentence_edge_signature_step(prefix, occurrence.tokens[current - 1]);
            suffix = sentence_edge_signature_step(
                suffix,
                occurrence.tokens[occurrence.tokens.len() - current],
            );
            for bucket in buckets.into_iter().flatten() {
                visits = count_signature_postings(
                    &self.own_depth_postings,
                    visits,
                    bucket,
                    role,
                    current,
                    prefix,
                    suffix,
                )?;
                if current == depth {
                    visits = count_signature_postings(
                        &self.all_depth_postings,
                        visits,
                        bucket,
                        role,
                        current,
                        prefix,
                        suffix,
                    )?;
                }
            }
        }
        Ok(visits)
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_sentence_tokens(
        &self,
        plausible: &mut Vec<usize>,
        tokens: &[SentenceEvidenceToken],
        role: OccurrenceRole,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    ) -> Option<SentenceEdgeSignatureQueryMetrics> {
        self.collect_sentence_tokens_bounded(
            plausible,
            tokens,
            role,
            bucket,
            additional_bucket,
            signature_step,
            None,
        )
        .ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_sentence_tokens_bounded(
        &self,
        plausible: &mut Vec<usize>,
        tokens: &[SentenceEvidenceToken],
        role: OccurrenceRole,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
        allocation_failure_after: Option<usize>,
    ) -> Result<SentenceEdgeSignatureQueryMetrics, SentenceEdgeSignatureIndexError> {
        plausible.clear();
        let query_depth = sentence_edge_signature_depth(tokens.len()).ok_or(
            SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 0,
                attempted: 0,
            },
        )?;
        let mut posting_visits = 0usize;
        let mut attempted = 0usize;
        let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
        let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
        let buckets = self.compact_query_buckets(bucket, additional_bucket, 0, 0)?;
        for depth in 1..=query_depth {
            prefix = signature_step(prefix, tokens[depth - 1]);
            suffix = signature_step(suffix, tokens[tokens.len() - depth]);
            for candidate_bucket in buckets.into_iter().flatten() {
                collect_sentence_edge_signatures(
                    &self.own_depth_postings,
                    plausible,
                    &mut posting_visits,
                    candidate_bucket,
                    role,
                    depth,
                    prefix,
                    suffix,
                    &mut attempted,
                    allocation_failure_after,
                )?;
                if depth == query_depth {
                    collect_sentence_edge_signatures(
                        &self.all_depth_postings,
                        plausible,
                        &mut posting_visits,
                        candidate_bucket,
                        role,
                        depth,
                        prefix,
                        suffix,
                        &mut attempted,
                        allocation_failure_after,
                    )?;
                }
            }
        }
        plausible.sort_unstable();
        plausible.dedup();
        Ok(SentenceEdgeSignatureQueryMetrics {
            posting_visits,
            candidate_union: plausible.len(),
            depth_band: sentence_edge_signature_depth_band(query_depth),
        })
    }

    fn compact_query_buckets(
        &self,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
        examined: usize,
        attempted: usize,
    ) -> Result<[Option<usize>; 2], SentenceEdgeSignatureIndexError> {
        Ok([
            Some(compact_sentence_edge_signature_bucket(
                self.scope, bucket, examined, attempted,
            )?),
            additional_bucket
                .map(|bucket| {
                    compact_sentence_edge_signature_bucket(self.scope, bucket, examined, attempted)
                })
                .transpose()?,
        ])
    }

    fn refresh_capacity_metrics(&mut self) -> Result<(), SentenceEdgeSignatureIndexError> {
        let own_key_capacity = self.own_depth_postings.ranges.capacity();
        let all_key_capacity = self.all_depth_postings.ranges.capacity();
        let posting_capacity_items = self
            .own_depth_postings
            .occurrences
            .capacity()
            .checked_add(self.all_depth_postings.occurrences.capacity())
            .ok_or_else(signature_metric_overflow)?;
        let estimated_logical_bytes = sentence_edge_signature_estimated_logical_bytes(
            own_key_capacity,
            all_key_capacity,
            posting_capacity_items,
        )?;
        self.metrics.own_key_capacity = own_key_capacity;
        self.metrics.all_key_capacity = all_key_capacity;
        self.metrics.posting_capacity_items = posting_capacity_items;
        self.metrics.estimated_logical_bytes = estimated_logical_bytes;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn count_signature_postings(
    postings: &SentenceEdgeSignaturePostings,
    mut count: usize,
    bucket: usize,
    role: OccurrenceRole,
    depth: usize,
    prefix: u64,
    suffix: u64,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    count = count
        .checked_add(
            signature_posting(
                postings,
                sentence_edge_signature_key_from_bucket(bucket, role, depth, prefix, 0, 0)?,
            )
            .len(),
        )
        .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: 0,
            attempted: usize::MAX,
        })?;
    if suffix != prefix {
        count = count
            .checked_add(
                signature_posting(
                    postings,
                    sentence_edge_signature_key_from_bucket(bucket, role, depth, suffix, 0, 0)?,
                )
                .len(),
            )
            .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 0,
                attempted: usize::MAX,
            })?;
    }
    Ok(count)
}

fn sentence_edge_signature_depth_band(depth: usize) -> Option<SentenceEdgeSignatureDepthBand> {
    match depth {
        0 => None,
        1 => Some(SentenceEdgeSignatureDepthBand::One),
        2..=3 => Some(SentenceEdgeSignatureDepthBand::TwoToThree),
        _ => Some(SentenceEdgeSignatureDepthBand::FourOrMore),
    }
}

fn signature_posting(
    postings: &SentenceEdgeSignaturePostings,
    key: SentenceEdgeSignatureKey,
) -> &[usize] {
    let index = postings.ranges.partition_point(|entry| entry.key < key);
    let Some(range) = postings.ranges.get(index).filter(|entry| entry.key == key) else {
        return &[];
    };
    let end = postings
        .ranges
        .get(index + 1)
        .map_or(postings.occurrences.len(), |entry| entry.start);
    postings.occurrences.get(range.start..end).unwrap_or(&[])
}

fn sentence_edge_signature_range_metrics(
    postings: &SentenceEdgeSignaturePostings,
) -> Result<SentenceEdgeSignatureRangeMetrics, SentenceEdgeSignatureIndexError> {
    let mut metrics = SentenceEdgeSignatureRangeMetrics::default();
    for (index, range) in postings.ranges.iter().enumerate() {
        let end = postings
            .ranges
            .get(index + 1)
            .map_or(postings.occurrences.len(), |entry| entry.start);
        let posting_len = end
            .checked_sub(range.start)
            .ok_or_else(signature_metric_overflow)?;
        metrics.distinct_keys = metrics
            .distinct_keys
            .checked_add(1)
            .ok_or_else(signature_metric_overflow)?;
        metrics.largest_posting = metrics.largest_posting.max(posting_len);
    }
    Ok(metrics)
}

fn sentence_edge_signature_estimated_logical_bytes(
    own_key_capacity: usize,
    all_key_capacity: usize,
    posting_capacity_items: usize,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    let key_capacity = own_key_capacity
        .checked_add(all_key_capacity)
        .ok_or_else(signature_metric_overflow)?;
    let key_bytes = key_capacity
        .checked_mul(size_of::<SentenceEdgeSignatureRange>())
        .ok_or_else(signature_metric_overflow)?;
    let posting_bytes = posting_capacity_items
        .checked_mul(size_of::<usize>())
        .ok_or_else(signature_metric_overflow)?;
    size_of::<SentenceEdgeSignatureIndex>()
        .checked_add(key_bytes)
        .and_then(|bytes| bytes.checked_add(posting_bytes))
        .ok_or_else(signature_metric_overflow)
}

fn signature_metric_overflow() -> SentenceEdgeSignatureIndexError {
    SentenceEdgeSignatureIndexError::CounterOverflow {
        examined: 0,
        attempted: usize::MAX,
    }
}

fn validate_sentence_edge_signature_build_limits(
    metrics: SentenceEdgeSignatureIndexMetrics,
    limits: SentenceEdgeSignatureIndexBuildLimits,
) -> Result<(), SentenceEdgeSignatureIndexBuildError> {
    if metrics.posting_items > limits.posting_items {
        return Err(SentenceEdgeSignatureIndexBuildError::PostingLimit {
            examined: metrics.posting_items,
            attempted: metrics.posting_items,
        });
    }
    let distinct_keys = metrics
        .own_distinct_keys
        .checked_add(metrics.all_distinct_keys)
        .ok_or(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
            examined: usize::MAX,
            attempted: usize::MAX,
            progress: metrics,
        })?;
    if distinct_keys > limits.distinct_keys {
        return Err(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
            examined: distinct_keys,
            attempted: distinct_keys,
            progress: metrics,
        });
    }
    if metrics.estimated_logical_bytes > limits.estimated_logical_bytes {
        return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined: metrics.estimated_logical_bytes,
            attempted: metrics.estimated_logical_bytes,
            progress: metrics,
        });
    }
    Ok(())
}

#[allow(dead_code)]
pub(in crate::diff) const SENTENCE_EDGE_SIGNATURE_SEED: u64 = 0xcbf2_9ce4_8422_2325;
#[allow(dead_code)]
const SENTENCE_EDGE_SIGNATURE_PRIME: u64 = 0x0000_0100_0000_01b3;

#[allow(dead_code)]
pub(in crate::diff) fn sentence_edge_signature_depth(shorter_len: usize) -> Option<usize> {
    let numerator = shorter_len.checked_mul(usize::from(MIN_WORD_SCORE_EDGE_EVIDENCE))?;
    let required = numerator
        .checked_div(10_000)?
        .checked_add(usize::from(numerator % 10_000 != 0))?;
    required
        .checked_div(2)?
        .checked_add(usize::from(required % 2 != 0))
}

#[allow(dead_code)]
pub(in crate::diff) fn sentence_edge_signature_step(
    state: u64,
    token: SentenceEvidenceToken,
) -> u64 {
    let token = match token {
        SentenceEvidenceToken::Scalar(scalar) => u64::from(u32::from(scalar)),
        SentenceEvidenceToken::Unmapped {
            font_fingerprint,
            glyph_id,
        } => font_fingerprint.rotate_left(17) ^ u64::from(glyph_id) ^ (1_u64 << 63),
    };
    (state ^ token).wrapping_mul(SENTENCE_EDGE_SIGNATURE_PRIME)
}

#[allow(dead_code)]
fn candidate_posting_bucket(
    scope: CandidatePostingIndexScope<'_>,
    occurrence_index: usize,
    occurrence: &SentenceOccurrence,
) -> Option<Option<CandidatePostingBucket>> {
    match scope {
        CandidatePostingIndexScope::Global => Some(Some(CandidatePostingBucket::Global)),
        CandidatePostingIndexScope::Span => {
            Some(Some(CandidatePostingBucket::Span(occurrence.span_index)))
        }
        CandidatePostingIndexScope::Paired(intervals) => Some(
            intervals
                .get(occurrence_index)
                .copied()?
                .map(CandidatePostingBucket::Paired),
        ),
        CandidatePostingIndexScope::PairedStream(intervals) => Some(
            intervals
                .get(occurrence_index)
                .copied()?
                .map(|interval| CandidatePostingBucket::PairedStream(interval.pair_index)),
        ),
    }
}

fn sentence_edge_signature_scope(
    scope: CandidatePostingIndexScope<'_>,
) -> SentenceEdgeSignatureScope {
    match scope {
        CandidatePostingIndexScope::Global => SentenceEdgeSignatureScope::Global,
        CandidatePostingIndexScope::Span => SentenceEdgeSignatureScope::Span,
        CandidatePostingIndexScope::Paired(_) => SentenceEdgeSignatureScope::Paired,
        CandidatePostingIndexScope::PairedStream(_) => SentenceEdgeSignatureScope::PairedStream,
    }
}

fn compact_sentence_edge_signature_bucket(
    scope: SentenceEdgeSignatureScope,
    bucket: CandidatePostingBucket,
    examined: usize,
    attempted: usize,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    let invalid = || SentenceEdgeSignatureIndexError::InvalidScope {
        examined,
        attempted,
    };
    match (scope, bucket) {
        (SentenceEdgeSignatureScope::Global, CandidatePostingBucket::Global) => Ok(0),
        (SentenceEdgeSignatureScope::Span, CandidatePostingBucket::Span(None)) => Ok(0),
        (SentenceEdgeSignatureScope::Span, CandidatePostingBucket::Span(Some(span))) => span
            .checked_add(1)
            .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined,
                attempted,
            }),
        (
            SentenceEdgeSignatureScope::Paired,
            CandidatePostingBucket::Paired(PairedInterval {
                pair_index,
                interval_index,
            }),
        ) => checked_sentence_edge_signature_pair(pair_index, interval_index, examined, attempted),
        (
            SentenceEdgeSignatureScope::PairedStream,
            CandidatePostingBucket::PairedStream(stream),
        ) => Ok(stream),
        _ => Err(invalid()),
    }
}

fn checked_sentence_edge_signature_pair(
    left: usize,
    right: usize,
    examined: usize,
    attempted: usize,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    let overflow = || SentenceEdgeSignatureIndexError::CounterOverflow {
        examined,
        attempted,
    };
    let diagonal = left.checked_add(right).ok_or_else(overflow)?;
    let next = diagonal.checked_add(1).ok_or_else(overflow)?;
    let (factor_left, factor_right) = if diagonal % 2 == 0 {
        (diagonal / 2, next)
    } else {
        (diagonal, next / 2)
    };
    factor_left
        .checked_mul(factor_right)
        .and_then(|value| value.checked_add(right))
        .ok_or_else(overflow)
}

fn sentence_edge_signature_key(
    scope: SentenceEdgeSignatureScope,
    bucket: CandidatePostingBucket,
    role: OccurrenceRole,
    depth: usize,
    signature: u64,
    examined: usize,
    attempted: usize,
) -> Result<SentenceEdgeSignatureKey, SentenceEdgeSignatureIndexError> {
    let bucket = compact_sentence_edge_signature_bucket(scope, bucket, examined, attempted)?;
    sentence_edge_signature_key_from_bucket(bucket, role, depth, signature, examined, attempted)
}

fn sentence_edge_signature_key_from_bucket(
    bucket: usize,
    role: OccurrenceRole,
    depth: usize,
    signature: u64,
    examined: usize,
    attempted: usize,
) -> Result<SentenceEdgeSignatureKey, SentenceEdgeSignatureIndexError> {
    const ROLE_COUNT: usize = 3;
    let role = match role {
        OccurrenceRole::Body => 0,
        OccurrenceRole::RepeatedHeader => 1,
        OccurrenceRole::RepeatedFooter => 2,
    };
    let depth_role = depth
        .checked_mul(ROLE_COUNT)
        .and_then(|value| value.checked_add(role))
        .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
            examined,
            attempted,
        })?;
    Ok(SentenceEdgeSignatureKey {
        group: checked_sentence_edge_signature_pair(bucket, depth_role, examined, attempted)?,
        signature,
    })
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn collect_sentence_edge_signatures(
    postings: &SentenceEdgeSignaturePostings,
    plausible: &mut Vec<usize>,
    posting_visits: &mut usize,
    bucket: usize,
    role: OccurrenceRole,
    depth: usize,
    prefix: u64,
    suffix: u64,
    attempted: &mut usize,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    collect_sentence_edge_signature(
        postings,
        plausible,
        posting_visits,
        sentence_edge_signature_key_from_bucket(
            bucket,
            role,
            depth,
            prefix,
            *posting_visits,
            *attempted,
        )?,
        attempted,
        allocation_failure_after,
    )?;
    if suffix != prefix {
        collect_sentence_edge_signature(
            postings,
            plausible,
            posting_visits,
            sentence_edge_signature_key_from_bucket(
                bucket,
                role,
                depth,
                suffix,
                *posting_visits,
                *attempted,
            )?,
            attempted,
            allocation_failure_after,
        )?;
    }
    Ok(())
}

#[allow(dead_code)]
fn collect_sentence_edge_signature(
    postings: &SentenceEdgeSignaturePostings,
    plausible: &mut Vec<usize>,
    posting_visits: &mut usize,
    key: SentenceEdgeSignatureKey,
    attempted: &mut usize,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    let posting = signature_posting(postings, key);
    if posting.is_empty() {
        return Ok(());
    }
    *attempted = attempted.checked_add(posting.len()).ok_or(
        SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: *posting_visits,
            attempted: usize::MAX,
        },
    )?;
    if allocation_failure_after.is_some_and(|limit| *posting_visits >= limit) {
        return Err(SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: *posting_visits,
            attempted: *attempted,
        });
    }
    plausible.try_reserve(posting.len()).map_err(|_| {
        SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: *posting_visits,
            attempted: *attempted,
        }
    })?;
    plausible.extend_from_slice(posting);
    *posting_visits = posting_visits.checked_add(posting.len()).ok_or(
        SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: *posting_visits,
            attempted: *attempted,
        },
    )?;
    Ok(())
}

pub(in crate::diff) fn split_query_candidates(
    candidates: &[usize],
    query_span: Option<usize>,
    occurrences: &[SentenceOccurrence],
) -> Option<NearSearchWorkSplit> {
    split_query_candidates_if(candidates, query_span, occurrences, |_| true)
}

pub(in crate::diff) fn split_query_candidates_if(
    candidates: &[usize],
    query_span: Option<usize>,
    occurrences: &[SentenceOccurrence],
    mut include: impl FnMut(usize) -> bool,
) -> Option<NearSearchWorkSplit> {
    candidates.iter().try_fold(
        NearSearchWorkSplit::default(),
        |mut split, occurrence_index| {
            if !include(*occurrence_index) {
                return Some(split);
            }
            let class = same_or_ambiguous_work_class(
                query_span,
                occurrences.get(*occurrence_index)?.span_index,
            );
            let count = match class {
                NearSearchWorkClass::SameKnown => &mut split.same_known,
                NearSearchWorkClass::Ambiguous => &mut split.ambiguous,
                NearSearchWorkClass::Shared => &mut split.shared,
            };
            *count = count.checked_add(1)?;
            Some(split)
        },
    )
}

pub(in crate::diff) fn same_or_ambiguous_work_class(
    query_span: Option<usize>,
    candidate_span: Option<usize>,
) -> NearSearchWorkClass {
    match (query_span, candidate_span) {
        (Some(query), Some(candidate)) if query == candidate => NearSearchWorkClass::SameKnown,
        (Some(_), None) => NearSearchWorkClass::Ambiguous,
        _ => NearSearchWorkClass::Shared,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::diff) struct PairedInterval {
    pub(in crate::diff) pair_index: usize,
    pub(in crate::diff) interval_index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_sequence(values: &[u8]) -> Vec<SentenceEvidenceToken> {
        values
            .iter()
            .map(|value| SentenceEvidenceToken::Scalar(char::from(b'a' + value)))
            .collect()
    }

    fn scalar(value: char) -> SentenceEvidenceToken {
        SentenceEvidenceToken::Scalar(value)
    }

    fn empty_unit_candidate_index() -> UnitCandidateIndex {
        UnitCandidateIndex {
            edge_postings: HashMap::new(),
            line_trigram_postings: HashMap::new(),
            line_trigram_counts: Vec::new(),
        }
    }

    fn push_test_edges(
        index: &mut UnitCandidateIndex,
        bucket: CandidatePostingBucket,
        kind: RecoveryUnitKind,
        role: OccurrenceRole,
        occurrence_index: usize,
        first: SentenceEvidenceToken,
        last: SentenceEvidenceToken,
    ) {
        index
            .push_occurrence_edges(bucket, kind, role, &[first, last], occurrence_index)
            .expect("test edge postings fit");
    }

    fn test_aligned_facts(
        index: &UnitCandidateIndex,
        first: char,
        last: char,
        occurrence_index: usize,
        bucket: CandidatePostingBucket,
        additional_bucket: Option<CandidatePostingBucket>,
    ) -> Option<AlignedSentenceEdgeFacts> {
        index.aligned_sentence_edge_facts_for_edges(
            RecoveryUnitKind::Sentence,
            OccurrenceRole::Body,
            scalar(first),
            scalar(last),
            occurrence_index,
            bucket,
            additional_bucket,
        )
    }

    #[test]
    fn exact_edge_postings_report_aligned_and_cross_orientation_facts() {
        let bucket = CandidatePostingBucket::Global;
        let mut index = empty_unit_candidate_index();
        for (occurrence_index, first, last) in [
            (0, 'a', 'x'),
            (1, 'y', 'z'),
            (2, 'a', 'z'),
            (3, 'z', 'a'),
            (4, 'a', 'a'),
        ] {
            push_test_edges(
                &mut index,
                bucket,
                RecoveryUnitKind::Sentence,
                OccurrenceRole::Body,
                occurrence_index,
                scalar(first),
                scalar(last),
            );
        }

        for (occurrence_index, prefix_equal, suffix_equal) in [
            (0, true, false),
            (1, false, true),
            (2, true, true),
            (3, false, false),
        ] {
            assert_eq!(
                test_aligned_facts(&index, 'a', 'z', occurrence_index, bucket, None),
                Some(AlignedSentenceEdgeFacts {
                    prefix_equal,
                    suffix_equal,
                })
            );
        }
        assert_eq!(
            test_aligned_facts(&index, 'a', 'a', 4, bucket, None),
            Some(AlignedSentenceEdgeFacts {
                prefix_equal: true,
                suffix_equal: true,
            })
        );
        assert_eq!(index.edge_postings.values().map(Vec::len).sum::<usize>(), 9);
        assert_eq!(
            index
                .edge_postings
                .get(&(
                    bucket,
                    RecoveryUnitKind::Sentence,
                    OccurrenceRole::Body,
                    scalar('a'),
                ))
                .expect("shared edge posting exists")
                .iter()
                .filter(|posting| posting.occurrence_index() == 4)
                .count(),
            1
        );
    }

    #[test]
    fn edge_posting_packs_one_word_and_rejects_index_overflow() {
        assert_eq!(size_of::<EdgePosting>(), size_of::<usize>());
        let largest = usize::MAX >> EdgePosting::SIDE_BITS;
        let posting =
            EdgePosting::new(largest, EdgePostingSides::BOTH).expect("largest packed index fits");
        assert_eq!(posting.occurrence_index(), largest);
        assert_eq!(posting.sides(), EdgePostingSides::BOTH);
        assert!(EdgePosting::new(largest + 1, EdgePostingSides::FIRST).is_none());

        let mut index = empty_unit_candidate_index();
        assert!(
            index
                .push_edge_posting(
                    (
                        CandidatePostingBucket::Global,
                        RecoveryUnitKind::Sentence,
                        OccurrenceRole::Body,
                        scalar('a'),
                    ),
                    largest + 1,
                    EdgePostingSides::FIRST,
                )
                .is_none()
        );
        assert!(index.edge_postings.is_empty());
    }

    #[test]
    fn exact_edge_facts_or_buckets_and_preserve_posting_order() {
        let primary = CandidatePostingBucket::Span(Some(0));
        let additional = CandidatePostingBucket::Span(None);
        let mut index = empty_unit_candidate_index();
        for occurrence_index in [0, 1, 2, 3] {
            index
                .push_edge_posting(
                    (
                        primary,
                        RecoveryUnitKind::Sentence,
                        OccurrenceRole::Body,
                        scalar('a'),
                    ),
                    occurrence_index,
                    EdgePostingSides::FIRST,
                )
                .expect("ordered posting fits");
        }
        index
            .push_edge_posting(
                (
                    additional,
                    RecoveryUnitKind::Sentence,
                    OccurrenceRole::Body,
                    scalar('z'),
                ),
                2,
                EdgePostingSides::LAST,
            )
            .expect("additional posting fits");
        let postings = index
            .edge_postings
            .get(&(
                primary,
                RecoveryUnitKind::Sentence,
                OccurrenceRole::Body,
                scalar('a'),
            ))
            .expect("primary postings exist");
        assert_eq!(
            postings
                .iter()
                .map(|posting| posting.occurrence_index())
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        let mut union = postings
            .iter()
            .map(|posting| posting.occurrence_index())
            .collect::<Vec<_>>();
        union.extend(
            index
                .edge_postings
                .get(&(
                    additional,
                    RecoveryUnitKind::Sentence,
                    OccurrenceRole::Body,
                    scalar('z'),
                ))
                .expect("additional postings exist")
                .iter()
                .map(|posting| posting.occurrence_index()),
        );
        assert_eq!(union.len(), 5);
        union.sort_unstable();
        union.dedup();
        assert_eq!(union, [0, 1, 2, 3]);
        assert_eq!(
            test_aligned_facts(&index, 'a', 'z', 2, primary, Some(additional)),
            Some(AlignedSentenceEdgeFacts {
                prefix_equal: true,
                suffix_equal: true,
            })
        );
    }

    #[test]
    fn exact_edge_facts_require_matching_sentence_role_and_kind() {
        let bucket = CandidatePostingBucket::Global;
        let mut index = empty_unit_candidate_index();
        push_test_edges(
            &mut index,
            bucket,
            RecoveryUnitKind::Sentence,
            OccurrenceRole::RepeatedHeader,
            0,
            scalar('a'),
            scalar('z'),
        );
        push_test_edges(
            &mut index,
            bucket,
            RecoveryUnitKind::Line,
            OccurrenceRole::Body,
            1,
            scalar('a'),
            scalar('z'),
        );

        assert_eq!(test_aligned_facts(&index, 'a', 'z', 0, bucket, None), None);
        assert_eq!(
            index.aligned_sentence_edge_facts_for_edges(
                RecoveryUnitKind::Line,
                OccurrenceRole::Body,
                scalar('a'),
                scalar('z'),
                1,
                bucket,
                None,
            ),
            None
        );
    }

    fn empty_signature_index() -> SentenceEdgeSignatureIndex {
        SentenceEdgeSignatureIndex::empty(SentenceEdgeSignatureScope::Global)
    }

    fn signature_key(depth: usize, signature: u64) -> SentenceEdgeSignatureKey {
        sentence_edge_signature_key_from_bucket(0, OccurrenceRole::Body, depth, signature, 0, 0)
            .expect("small signature key fits")
    }

    fn build_body_sentences(
        sentences: &[Vec<SentenceEvidenceToken>],
    ) -> SentenceEdgeSignatureIndex {
        let mut build = SentenceEdgeSignatureBuild::empty(SentenceEdgeSignatureScope::Global);
        let (own_postings, all_postings) = sentences
            .iter()
            .try_fold((0usize, 0usize), |(own, all), tokens| {
                let depth = sentence_edge_signature_depth(tokens.len())?;
                let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
                let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
                let mut count = 0usize;
                for current in 1..=depth {
                    prefix = sentence_edge_signature_step(prefix, tokens[current - 1]);
                    suffix = sentence_edge_signature_step(suffix, tokens[tokens.len() - current]);
                    count = count.checked_add(usize::from(prefix != suffix) + 1)?;
                }
                let own_count = usize::from(depth > 0) * (usize::from(prefix != suffix) + 1);
                Some((
                    own.checked_add(own_count)?,
                    all.checked_add(count.checked_sub(own_count)?)?,
                ))
            })
            .expect("small posting bounds fit");
        reserve_sentence_edge_signature_build(&mut build, own_postings, all_postings)
            .expect("small test build reserves");
        for (occurrence_index, tokens) in sentences.iter().enumerate() {
            collect_sentence_edge_signature_unit(
                &mut build,
                CandidatePostingBucket::Global,
                OccurrenceRole::Body,
                tokens,
                occurrence_index,
                sentence_edge_signature_step,
                None,
            )
            .expect("small test sentence collects");
        }
        finalize_sentence_edge_signature_build(build, None).expect("small test index finalizes")
    }

    fn edge_distinct_tokens(len: usize) -> Vec<SentenceEvidenceToken> {
        let mut tokens = vec![SentenceEvidenceToken::Scalar('a'); len];
        if let Some(last) = tokens.last_mut() {
            *last = SentenceEvidenceToken::Scalar('b');
        }
        tokens
    }

    fn query_body_sentence(
        index: &SentenceEdgeSignatureIndex,
        tokens: &[SentenceEvidenceToken],
    ) -> (Vec<usize>, SentenceEdgeSignatureQueryMetrics) {
        let mut candidates = Vec::new();
        let metrics = index
            .collect_sentence_tokens(
                &mut candidates,
                tokens,
                OccurrenceRole::Body,
                CandidatePostingBucket::Global,
                None,
                sentence_edge_signature_step,
            )
            .expect("small test query fits");
        (candidates, metrics)
    }

    fn exact_edge_gate(left: &[SentenceEvidenceToken], right: &[SentenceEvidenceToken]) -> bool {
        let shorter = left.len().min(right.len());
        if shorter == 0 {
            return false;
        }
        let mut prefix = 0;
        while prefix < shorter && left[prefix] == right[prefix] {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < shorter - prefix
            && left[left.len() - suffix - 1] == right[right.len() - suffix - 1]
        {
            suffix += 1;
        }
        (prefix + suffix) * 10_000 / shorter >= usize::from(MIN_WORD_SCORE_EDGE_EVIDENCE)
    }

    fn all_binary_sequences(max_len: usize) -> Vec<Vec<SentenceEvidenceToken>> {
        let mut sequences = vec![Vec::new()];
        for len in 1..=max_len {
            for bits in 0..(1usize << len) {
                let values = (0..len)
                    .map(|offset| u8::from(bits & (1 << offset) != 0))
                    .collect::<Vec<_>>();
                sequences.push(token_sequence(&values));
            }
        }
        sequences
    }

    #[test]
    fn sentence_edge_signature_index_never_drops_an_exact_gate_pair() {
        assert_eq!(sentence_edge_signature_depth(0), Some(0));
        assert_eq!(sentence_edge_signature_depth(1), Some(1));
        assert_eq!(sentence_edge_signature_depth(7), Some(2));
        assert_eq!(sentence_edge_signature_depth(usize::MAX), None);

        let sequences = all_binary_sequences(7);
        let index = build_body_sentences(&sequences);

        for query in &sequences {
            let (candidates, metrics) = query_body_sentence(&index, query);
            assert_eq!(metrics.candidate_union, candidates.len());
            assert!(candidates.windows(2).all(|pair| pair[0] < pair[1]));
            for (candidate_index, candidate) in sequences.iter().enumerate() {
                if exact_edge_gate(query, candidate) {
                    assert!(
                        candidates.binary_search(&candidate_index).is_ok(),
                        "missing query length {} and candidate length {}",
                        query.len(),
                        candidate.len()
                    );
                }
            }
        }
    }

    #[test]
    fn sentence_edge_signature_index_covers_prefix_suffix_and_combined_edges() {
        let candidates = [
            token_sequence(&[0, 1, 1, 2, 2, 2, 2, 2, 2, 2]),
            token_sequence(&[2, 2, 2, 2, 2, 2, 2, 1, 1, 0]),
            token_sequence(&[0, 1, 2, 2, 2, 2, 2, 2, 2, 0]),
            token_sequence(&[0, 1, 2, 2, 2, 2, 2, 2, 2, 2]),
            token_sequence(&[1]),
            Vec::new(),
        ];
        let query = token_sequence(&[0, 1, 1, 1, 1, 1, 1, 1, 1, 0]);
        let index = build_body_sentences(&candidates);

        let (found, _) = query_body_sentence(&index, &query);

        assert_eq!(found, vec![0, 1, 2, 3]);
        assert!(exact_edge_gate(&query, &candidates[0]));
        assert!(exact_edge_gate(&query, &candidates[1]));
        assert!(exact_edge_gate(&query, &candidates[2]));
        assert!(!exact_edge_gate(&query, &candidates[3]));
        assert!(query_body_sentence(&index, &[]).0.is_empty());
        assert!(
            query_body_sentence(&index, &token_sequence(&[1]))
                .0
                .contains(&4)
        );
    }

    #[test]
    fn sentence_edge_signature_index_isolates_buckets_roles_and_lines() {
        let tokens = token_sequence(&[0, 1, 2, 0]);
        let mut build = SentenceEdgeSignatureBuild::empty(SentenceEdgeSignatureScope::Span);
        reserve_sentence_edge_signature_build(&mut build, 3, 0).expect("test build reserves");
        for (bucket, role, occurrence_index) in [
            (CandidatePostingBucket::Span(None), OccurrenceRole::Body, 0),
            (
                CandidatePostingBucket::Span(Some(0)),
                OccurrenceRole::Body,
                1,
            ),
            (
                CandidatePostingBucket::Span(None),
                OccurrenceRole::RepeatedHeader,
                2,
            ),
        ] {
            collect_sentence_edge_signature_unit(
                &mut build,
                bucket,
                role,
                &tokens,
                occurrence_index,
                sentence_edge_signature_step,
                None,
            )
            .expect("test sentence collects");
        }
        let index =
            finalize_sentence_edge_signature_build(build, None).expect("test index finalizes");

        let mut ambiguous_body = Vec::new();
        index
            .collect_sentence_tokens(
                &mut ambiguous_body,
                &tokens,
                OccurrenceRole::Body,
                CandidatePostingBucket::Span(None),
                None,
                sentence_edge_signature_step,
            )
            .expect("ambiguous-span query succeeds");
        assert_eq!(ambiguous_body, vec![0]);

        let mut both_buckets = Vec::new();
        index
            .collect_sentence_tokens(
                &mut both_buckets,
                &tokens,
                OccurrenceRole::Body,
                CandidatePostingBucket::Span(None),
                Some(CandidatePostingBucket::Span(Some(0))),
                sentence_edge_signature_step,
            )
            .expect("two-bucket query succeeds");
        assert_eq!(both_buckets, vec![0, 1]);

        let mut header = Vec::new();
        index
            .collect_sentence_tokens(
                &mut header,
                &tokens,
                OccurrenceRole::RepeatedHeader,
                CandidatePostingBucket::Span(None),
                None,
                sentence_edge_signature_step,
            )
            .expect("header query succeeds");
        assert_eq!(header, vec![2]);
    }

    #[test]
    fn signature_collisions_only_add_candidates_and_work_is_reported() {
        fn collide(_: u64, _: SentenceEvidenceToken) -> u64 {
            0
        }

        let candidate = token_sequence(&[0, 0, 0, 0]);
        let query = token_sequence(&[1, 1, 1, 1]);
        let mut build = SentenceEdgeSignatureBuild::empty(SentenceEdgeSignatureScope::Global);
        reserve_sentence_edge_signature_build(&mut build, 1, 0).expect("test build reserves");
        collect_sentence_edge_signature_unit(
            &mut build,
            CandidatePostingBucket::Global,
            OccurrenceRole::Body,
            &candidate,
            0,
            collide,
            None,
        )
        .expect("colliding signature is collected");
        let index = finalize_sentence_edge_signature_build(build, None)
            .expect("colliding signature is indexed");
        let mut found = Vec::new();
        let query_metrics = index
            .collect_sentence_tokens(
                &mut found,
                &query,
                OccurrenceRole::Body,
                CandidatePostingBucket::Global,
                None,
                collide,
            )
            .expect("colliding signature query succeeds");

        assert!(!exact_edge_gate(&query, &candidate));
        assert_eq!(found, vec![0]);
        assert_eq!(query_metrics.candidate_union, 1);
        assert!(query_metrics.posting_visits >= query_metrics.candidate_union);
        assert!(index.metrics().posting_items >= query_metrics.posting_visits);
    }

    #[test]
    fn signature_shape_metrics_cover_capacities_largest_posting_and_depth_bands() {
        let sentences = [
            edge_distinct_tokens(1),
            edge_distinct_tokens(7),
            edge_distinct_tokens(20),
            edge_distinct_tokens(21),
        ];
        let index = build_body_sentences(&sentences);
        let metrics = index.metrics();

        assert_eq!(metrics.own_posting_items, 7);
        assert_eq!(metrics.all_posting_items, 12);
        assert_eq!(metrics.posting_items, 19);
        assert_eq!(metrics.depth_1_posting_items, 7);
        assert_eq!(metrics.depth_2_to_3_posting_items, 10);
        assert_eq!(metrics.depth_4_plus_posting_items, 2);
        assert_eq!(
            metrics.depth_1_posting_items
                + metrics.depth_2_to_3_posting_items
                + metrics.depth_4_plus_posting_items,
            metrics.posting_items
        );
        assert_eq!(
            metrics.own_distinct_keys,
            index.own_depth_postings.ranges.len()
        );
        assert_eq!(
            metrics.all_distinct_keys,
            index.all_depth_postings.ranges.len()
        );
        assert_eq!(
            metrics.own_key_capacity,
            index.own_depth_postings.ranges.capacity()
        );
        assert_eq!(
            metrics.all_key_capacity,
            index.all_depth_postings.ranges.capacity()
        );
        assert_eq!(metrics.largest_posting, 3);
        assert!(metrics.posting_capacity_items >= metrics.posting_items);
        assert!(
            metrics.own_key_capacity + metrics.all_key_capacity < metrics.posting_capacity_items
        );

        let expected_bytes = size_of::<SentenceEdgeSignatureIndex>()
            + (metrics.own_key_capacity + metrics.all_key_capacity)
                * size_of::<SentenceEdgeSignatureRange>()
            + metrics.posting_capacity_items * size_of::<usize>();
        assert_eq!(metrics.estimated_logical_bytes, expected_bytes);
    }

    #[test]
    fn equal_prefix_suffix_signature_is_stored_once() {
        let palindrome = token_sequence(&[0, 1, 0, 1, 0, 1, 0]);
        let index = build_body_sentences(&[palindrome]);
        let metrics = index.metrics();

        assert_eq!(metrics.posting_items, 2);
        assert_eq!(metrics.own_posting_items, 1);
        assert_eq!(metrics.all_posting_items, 1);
        assert_eq!(metrics.own_distinct_keys, 1);
        assert_eq!(metrics.all_distinct_keys, 1);
    }

    #[test]
    fn bounded_temp_reserve_stops_before_allocating_all_namespace() {
        let own = 3;
        let all = 5;
        let minimum =
            sentence_edge_signature_temp_bytes(own, all).expect("small temporary allocation fits");
        let mut build = SentenceEdgeSignatureBuild::empty(SentenceEdgeSignatureScope::Global);
        let error = reserve_sentence_edge_signature_build_with_limits(
            &mut build,
            own,
            all,
            SentenceEdgeSignatureIndexBuildLimits {
                posting_items: usize::MAX,
                distinct_keys: usize::MAX,
                estimated_logical_bytes: minimum - 1,
            },
        )
        .expect_err("one byte below the projected temporary allocation stops");

        assert_eq!(build.all.capacity(), 0);
        assert!(build.own.capacity() >= own);
        assert!(matches!(
            error,
            SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
                examined,
                attempted,
                progress,
            } if examined == progress.estimated_logical_bytes
                && attempted >= minimum
                && attempted > examined
                && progress.all_key_capacity == 0
        ));
    }

    #[test]
    fn empty_and_one_token_sentences_have_bounded_metrics() {
        let empty = build_body_sentences(&[]);
        assert_eq!(empty.metrics().posting_items, 0);
        assert_eq!(
            empty.metrics().estimated_logical_bytes,
            size_of::<SentenceEdgeSignatureIndex>()
        );

        let one = build_body_sentences(&[token_sequence(&[0])]);
        assert_eq!(one.metrics().posting_items, 1);
        assert_eq!(one.metrics().depth_1_posting_items, 1);
        assert_eq!(one.metrics().largest_posting, 1);
        assert_eq!(query_body_sentence(&one, &[]).0, Vec::<usize>::new());
        assert_eq!(query_body_sentence(&one, &token_sequence(&[0])).0, vec![0]);
    }

    #[test]
    fn signature_insertion_reports_allocation_and_counter_failures() {
        let tokens = token_sequence(&[0, 1, 2, 3]);
        let mut build = SentenceEdgeSignatureBuild::empty(SentenceEdgeSignatureScope::Global);
        reserve_sentence_edge_signature_build(&mut build, 2, 0).expect("test build reserves");
        let allocation = collect_sentence_edge_signature_unit(
            &mut build,
            CandidatePostingBucket::Global,
            OccurrenceRole::Body,
            &tokens,
            0,
            sentence_edge_signature_step,
            Some(0),
        )
        .expect_err("injected allocation failure must stop insertion");
        assert_eq!(
            allocation,
            SentenceEdgeSignatureIndexError::AllocationFailure {
                examined: 0,
                attempted: 1,
            }
        );

        build.posting_items = usize::MAX;
        let overflow = collect_sentence_edge_signature_unit(
            &mut build,
            CandidatePostingBucket::Global,
            OccurrenceRole::Body,
            &tokens,
            0,
            sentence_edge_signature_step,
            None,
        )
        .expect_err("counter overflow must stop insertion");
        assert_eq!(
            overflow,
            SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: usize::MAX,
                attempted: usize::MAX,
            }
        );
    }

    #[test]
    fn signature_query_deduplicates_and_sorts_candidate_indices() {
        let tokens = token_sequence(&[0, 1, 2, 0]);
        let mut build = SentenceEdgeSignatureBuild::empty(SentenceEdgeSignatureScope::Global);
        reserve_sentence_edge_signature_build(&mut build, 3, 0).expect("test build reserves");
        for occurrence_index in [3, 1, 2] {
            collect_sentence_edge_signature_unit(
                &mut build,
                CandidatePostingBucket::Global,
                OccurrenceRole::Body,
                &tokens,
                occurrence_index,
                sentence_edge_signature_step,
                None,
            )
            .expect("test sentence collects");
        }
        let index =
            finalize_sentence_edge_signature_build(build, None).expect("test index finalizes");

        assert_eq!(query_body_sentence(&index, &tokens).0, vec![1, 2, 3]);
    }

    #[test]
    fn compact_signature_posting_ranges_preserve_key_boundaries_and_order() {
        let low = signature_key(1, 10);
        let middle = signature_key(1, 20);
        let high = signature_key(2, 10);
        let mut source = vec![
            SentenceEdgeSignatureBuildEntry {
                key: middle,
                occurrence_index: 4,
            },
            SentenceEdgeSignatureBuildEntry {
                key: low,
                occurrence_index: 3,
            },
            SentenceEdgeSignatureBuildEntry {
                key: high,
                occurrence_index: 1,
            },
            SentenceEdgeSignatureBuildEntry {
                key: middle,
                occurrence_index: 2,
            },
        ];
        source.sort_unstable();
        let mut entries = SentenceEdgeSignaturePostings::default();
        reserve_final_sentence_edge_signature_ranges(&mut entries, 3, 4)
            .expect("posting ranges reserve");
        reserve_final_sentence_edge_signature_occurrences(&mut entries, 4, 4)
            .expect("posting occurrences reserve");
        fill_final_sentence_edge_signature_postings(&source, &mut entries);

        assert_eq!(signature_posting(&entries, low), [3]);
        assert_eq!(signature_posting(&entries, middle), [2, 4]);
        assert_eq!(signature_posting(&entries, high), [1]);
        assert!(signature_posting(&entries, signature_key(1, 15)).is_empty());
    }

    #[test]
    fn compact_signature_keys_keep_full_values_and_scope_local_buckets() {
        assert_eq!(size_of::<SentenceEdgeSignatureKey>(), 16);
        assert_eq!(size_of::<SentenceEdgeSignatureBuildEntry>(), 24);
        assert_eq!(size_of::<SentenceEdgeSignatureRange>(), 24);
        assert_eq!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::Global,
                CandidatePostingBucket::Global,
                0,
                0,
            ),
            Ok(0)
        );
        assert_eq!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::Span,
                CandidatePostingBucket::Span(None),
                0,
                0,
            ),
            Ok(0)
        );
        assert_eq!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::Span,
                CandidatePostingBucket::Span(Some(0)),
                0,
                0,
            ),
            Ok(1)
        );
        assert_eq!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::PairedStream,
                CandidatePostingBucket::PairedStream(usize::MAX),
                0,
                0,
            ),
            Ok(usize::MAX)
        );
        assert!(matches!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::Global,
                CandidatePostingBucket::Span(None),
                7,
                9,
            ),
            Err(SentenceEdgeSignatureIndexError::InvalidScope {
                examined: 7,
                attempted: 9,
            })
        ));
        assert!(matches!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::Span,
                CandidatePostingBucket::Span(Some(usize::MAX)),
                7,
                9,
            ),
            Err(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 7,
                attempted: 9,
            })
        ));
    }

    #[test]
    fn paired_scope_bucket_encoding_is_collision_free() {
        let mut encoded = HashSet::new();
        for pair_index in 0..64 {
            for interval_index in 0..64 {
                let bucket = compact_sentence_edge_signature_bucket(
                    SentenceEdgeSignatureScope::Paired,
                    CandidatePostingBucket::Paired(PairedInterval {
                        pair_index,
                        interval_index,
                    }),
                    0,
                    0,
                )
                .expect("small paired bucket fits");
                assert!(encoded.insert(bucket));
            }
        }
        assert!(matches!(
            compact_sentence_edge_signature_bucket(
                SentenceEdgeSignatureScope::Paired,
                CandidatePostingBucket::Paired(PairedInterval {
                    pair_index: usize::MAX,
                    interval_index: 1,
                }),
                0,
                0,
            ),
            Err(SentenceEdgeSignatureIndexError::CounterOverflow { .. })
        ));
    }

    #[test]
    fn compact_signature_group_encoding_is_collision_free() {
        let mut encoded = HashSet::new();
        for bucket in 0..32 {
            for role in [
                OccurrenceRole::Body,
                OccurrenceRole::RepeatedHeader,
                OccurrenceRole::RepeatedFooter,
            ] {
                for depth in 1..32 {
                    let key = sentence_edge_signature_key(
                        SentenceEdgeSignatureScope::PairedStream,
                        CandidatePostingBucket::PairedStream(bucket),
                        role,
                        depth,
                        7,
                        0,
                        0,
                    )
                    .expect("small compact group fits");
                    assert!(encoded.insert(key.group));
                }
            }
        }
        assert!(matches!(
            sentence_edge_signature_key_from_bucket(
                usize::MAX,
                OccurrenceRole::RepeatedFooter,
                usize::MAX,
                7,
                4,
                5,
            ),
            Err(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 4,
                attempted: 5,
            })
        ));
    }

    #[test]
    fn signature_query_rejects_bucket_from_another_scope() {
        let index = empty_signature_index();
        let mut candidates = Vec::new();
        let error = index
            .collect_sentence_tokens_bounded(
                &mut candidates,
                &token_sequence(&[0, 1, 2, 0]),
                OccurrenceRole::Body,
                CandidatePostingBucket::Span(None),
                None,
                sentence_edge_signature_step,
                None,
            )
            .expect_err("scope mismatch fails closed");
        assert!(matches!(
            error,
            SentenceEdgeSignatureIndexError::InvalidScope { .. }
        ));
    }
}
