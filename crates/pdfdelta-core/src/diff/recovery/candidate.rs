use std::{collections::HashMap, mem::size_of};

use super::score::{
    LINE_NGRAM_SIZE, MIN_WORD_SCORE_EDGE_EVIDENCE, line_trigram_candidate_meets_threshold,
    ngram_count,
};
use crate::diff::sentence::{
    OccurrenceRole, RecoveryBudget, RecoveryUnitKind, SentenceEvidenceToken, SentenceOccurrence,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
        Vec<usize>,
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

#[allow(dead_code)]
type SentenceEdgeSignatureKey = (CandidatePostingBucket, OccurrenceRole, usize, u64);

/// Diagnostic index for sentence pairs that can satisfy the edge-evidence gate.
///
/// This remains separate from [`UnitCandidateIndex`] so production candidate
/// construction has no additional allocation or runtime work.
#[allow(dead_code)]
pub(in crate::diff) struct SentenceEdgeSignatureIndex {
    own_depth_postings: HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    all_depth_postings: HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    metrics: SentenceEdgeSignatureIndexMetrics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::diff) struct SentenceEdgeSignatureIndexMetrics {
    pub(in crate::diff) posting_items: usize,
    pub(in crate::diff) own_distinct_keys: usize,
    pub(in crate::diff) all_distinct_keys: usize,
    pub(in crate::diff) own_key_capacity: usize,
    pub(in crate::diff) all_key_capacity: usize,
    pub(in crate::diff) own_posting_items: usize,
    pub(in crate::diff) all_posting_items: usize,
    pub(in crate::diff) posting_capacity_items: usize,
    pub(in crate::diff) largest_posting: usize,
    pub(in crate::diff) depth_1_posting_items: usize,
    pub(in crate::diff) depth_2_to_3_posting_items: usize,
    pub(in crate::diff) depth_4_plus_posting_items: usize,
    /// Logical index bytes derived from map and posting capacities.
    ///
    /// The estimate is `size_of::<SentenceEdgeSignatureIndex>()`, plus each
    /// map's capacity multiplied by the key and `Vec<usize>` sizes, plus the
    /// aggregate posting-vector capacity multiplied by `size_of::<usize>()`.
    /// Allocator metadata and hash-table control bytes are intentionally
    /// excluded because the standard library does not expose them.
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
    PostingLimit { examined: usize, attempted: usize },
    DistinctKeyLimit { examined: usize, attempted: usize },
    EstimatedByteLimit { examined: usize, attempted: usize },
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
            }
            | Self::EstimatedByteLimit {
                examined,
                attempted,
            } => (examined, attempted),
            Self::Index(error) => error.work(),
        }
    }
}

#[derive(Default)]
struct SentenceEdgeSignaturePostingMetrics {
    posting_items: usize,
    posting_capacity_items: usize,
    largest_posting: usize,
    depth_1_posting_items: usize,
    depth_2_to_3_posting_items: usize,
    depth_4_plus_posting_items: usize,
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
            let first = *occurrence.tokens.first()?;
            let last = *occurrence.tokens.last()?;
            index.push_edge_posting((bucket, occurrence.kind, role, first), occurrence_index)?;
            if last != first {
                index.push_edge_posting((bucket, occurrence.kind, role, last), occurrence_index)?;
            }
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

    fn push_edge_posting(
        &mut self,
        key: (
            CandidatePostingBucket,
            RecoveryUnitKind,
            OccurrenceRole,
            SentenceEvidenceToken,
        ),
        occurrence_index: usize,
    ) -> Option<()> {
        if !self.edge_postings.contains_key(&key) {
            self.edge_postings.try_reserve(1).ok()?;
            self.edge_postings.insert(key, Vec::new());
        }
        let postings = self.edge_postings.get_mut(&key)?;
        postings.try_reserve(1).ok()?;
        postings.push(occurrence_index);
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
                plausible.extend_from_slice(postings);
            }
            if first != last
                && let Some(postings) =
                    self.edge_postings
                        .get(&(bucket, occurrence.kind, role, last))
            {
                plausible.extend_from_slice(postings);
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
        occurrences
            .iter()
            .enumerate()
            .try_fold(0usize, |total, (occurrence_index, occurrence)| {
                if occurrence.kind != RecoveryUnitKind::Sentence || occurrence.role.is_none() {
                    return Ok(total);
                }
                if candidate_posting_bucket(scope, occurrence_index, occurrence)
                    .ok_or(SentenceEdgeSignatureIndexError::InvalidScope {
                        examined: 0,
                        attempted: total,
                    })?
                    .is_none()
                {
                    return Ok(total);
                }
                let depth = sentence_edge_signature_depth(occurrence.tokens.len()).ok_or(
                    SentenceEdgeSignatureIndexError::CounterOverflow {
                        examined: 0,
                        attempted: total,
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
                            attempted: total,
                        })?;
                }
                total.checked_add(postings).ok_or(
                    SentenceEdgeSignatureIndexError::CounterOverflow {
                        examined: 0,
                        attempted: usize::MAX,
                    },
                )
            })
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
        let mut index = Self {
            own_depth_postings: HashMap::new(),
            all_depth_postings: HashMap::new(),
            metrics: SentenceEdgeSignatureIndexMetrics::default(),
        };
        index
            .refresh_shape_metrics()
            .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        validate_sentence_edge_signature_build_limits(index.metrics, limits)?;
        let mut attempted = 0;
        for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
            let Some(bucket) = candidate_posting_bucket(scope, occurrence_index, occurrence)
                .ok_or(SentenceEdgeSignatureIndexBuildError::Index(
                    SentenceEdgeSignatureIndexError::InvalidScope {
                        examined: index.metrics.posting_items,
                        attempted,
                    },
                ))?
            else {
                continue;
            };
            index.insert_unit_with_limits(
                bucket,
                occurrence.kind,
                occurrence.role.map(OccurrenceRole::from),
                &occurrence.tokens,
                occurrence_index,
                sentence_edge_signature_step,
                &mut attempted,
                limits,
            )?;
        }
        Ok(index)
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
        let mut index = Self {
            own_depth_postings: HashMap::new(),
            all_depth_postings: HashMap::new(),
            metrics: SentenceEdgeSignatureIndexMetrics::default(),
        };
        let mut attempted = 0usize;
        for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
            let Some(bucket) = candidate_posting_bucket(scope, occurrence_index, occurrence)
                .ok_or(SentenceEdgeSignatureIndexError::InvalidScope {
                    examined: index.metrics.posting_items,
                    attempted,
                })?
            else {
                continue;
            };
            index.insert_unit_bounded(
                bucket,
                occurrence.kind,
                occurrence.role.map(OccurrenceRole::from),
                &occurrence.tokens,
                occurrence_index,
                signature_step,
                &mut attempted,
                allocation_failure_after,
            )?;
        }
        index.refresh_shape_metrics()?;
        Ok(index)
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
        for current in 1..=depth {
            prefix = sentence_edge_signature_step(prefix, occurrence.tokens[current - 1]);
            suffix = sentence_edge_signature_step(
                suffix,
                occurrence.tokens[occurrence.tokens.len() - current],
            );
            for bucket in [Some(bucket), additional_bucket].into_iter().flatten() {
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

    fn insert_unit(
        &mut self,
        bucket: CandidatePostingBucket,
        kind: RecoveryUnitKind,
        role: Option<OccurrenceRole>,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    ) -> Option<()> {
        let mut attempted = self.metrics.posting_items;
        self.insert_unit_bounded(
            bucket,
            kind,
            role,
            tokens,
            occurrence_index,
            signature_step,
            &mut attempted,
            None,
        )
        .and_then(|()| self.refresh_shape_metrics())
        .ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_unit_bounded(
        &mut self,
        bucket: CandidatePostingBucket,
        kind: RecoveryUnitKind,
        role: Option<OccurrenceRole>,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
        attempted: &mut usize,
        allocation_failure_after: Option<usize>,
    ) -> Result<(), SentenceEdgeSignatureIndexError> {
        if kind != RecoveryUnitKind::Sentence {
            return Ok(());
        }
        let Some(role) = role else {
            return Ok(());
        };
        self.insert_sentence_bounded(
            bucket,
            role,
            tokens,
            occurrence_index,
            signature_step,
            attempted,
            allocation_failure_after,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_unit_with_limits(
        &mut self,
        bucket: CandidatePostingBucket,
        kind: RecoveryUnitKind,
        role: Option<OccurrenceRole>,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
        attempted: &mut usize,
        limits: SentenceEdgeSignatureIndexBuildLimits,
    ) -> Result<(), SentenceEdgeSignatureIndexBuildError> {
        if kind != RecoveryUnitKind::Sentence {
            return Ok(());
        }
        let Some(role) = role else {
            return Ok(());
        };
        self.insert_sentence_with_limits(
            bucket,
            role,
            tokens,
            occurrence_index,
            signature_step,
            attempted,
            limits,
        )
    }

    fn insert_sentence(
        &mut self,
        bucket: CandidatePostingBucket,
        role: OccurrenceRole,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    ) -> Option<()> {
        let mut attempted = self.metrics.posting_items;
        self.insert_sentence_bounded(
            bucket,
            role,
            tokens,
            occurrence_index,
            signature_step,
            &mut attempted,
            None,
        )
        .and_then(|()| self.refresh_shape_metrics())
        .ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_sentence_bounded(
        &mut self,
        bucket: CandidatePostingBucket,
        role: OccurrenceRole,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
        attempted: &mut usize,
        allocation_failure_after: Option<usize>,
    ) -> Result<(), SentenceEdgeSignatureIndexError> {
        let own_depth = sentence_edge_signature_depth(tokens.len()).ok_or(
            SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: self.metrics.posting_items,
                attempted: *attempted,
            },
        )?;
        let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
        let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
        for depth in 1..=own_depth {
            prefix = signature_step(prefix, tokens[depth - 1]);
            suffix = signature_step(suffix, tokens[tokens.len() - depth]);
            let postings = if depth == own_depth {
                &mut self.own_depth_postings
            } else {
                &mut self.all_depth_postings
            };
            push_sentence_edge_signatures(
                postings,
                &mut self.metrics,
                bucket,
                role,
                depth,
                prefix,
                suffix,
                occurrence_index,
                attempted,
                allocation_failure_after,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_sentence_with_limits(
        &mut self,
        bucket: CandidatePostingBucket,
        role: OccurrenceRole,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
        attempted: &mut usize,
        limits: SentenceEdgeSignatureIndexBuildLimits,
    ) -> Result<(), SentenceEdgeSignatureIndexBuildError> {
        let own_depth = sentence_edge_signature_depth(tokens.len()).ok_or(
            SentenceEdgeSignatureIndexBuildError::Index(
                SentenceEdgeSignatureIndexError::CounterOverflow {
                    examined: self.metrics.posting_items,
                    attempted: *attempted,
                },
            ),
        )?;
        let mut prefix = SENTENCE_EDGE_SIGNATURE_SEED;
        let mut suffix = SENTENCE_EDGE_SIGNATURE_SEED;
        for depth in 1..=own_depth {
            prefix = signature_step(prefix, tokens[depth - 1]);
            suffix = signature_step(suffix, tokens[tokens.len() - depth]);
            self.push_sentence_edge_signature_with_limits(
                depth == own_depth,
                (bucket, role, depth, prefix),
                occurrence_index,
                attempted,
                limits,
            )?;
            if suffix != prefix {
                self.push_sentence_edge_signature_with_limits(
                    depth == own_depth,
                    (bucket, role, depth, suffix),
                    occurrence_index,
                    attempted,
                    limits,
                )?;
            }
        }
        Ok(())
    }

    fn push_sentence_edge_signature_with_limits(
        &mut self,
        own_depth: bool,
        key: SentenceEdgeSignatureKey,
        occurrence_index: usize,
        attempted: &mut usize,
        limits: SentenceEdgeSignatureIndexBuildLimits,
    ) -> Result<(), SentenceEdgeSignatureIndexBuildError> {
        let attempted_postings = self.metrics.posting_items.checked_add(1).ok_or(
            SentenceEdgeSignatureIndexBuildError::Index(
                SentenceEdgeSignatureIndexError::CounterOverflow {
                    examined: self.metrics.posting_items,
                    attempted: usize::MAX,
                },
            ),
        )?;
        if attempted_postings > limits.posting_items {
            return Err(SentenceEdgeSignatureIndexBuildError::PostingLimit {
                examined: self.metrics.posting_items,
                attempted: attempted_postings,
            });
        }

        let postings = if own_depth {
            &mut self.own_depth_postings
        } else {
            &mut self.all_depth_postings
        };
        let distinct_keys = self
            .metrics
            .own_distinct_keys
            .checked_add(self.metrics.all_distinct_keys)
            .ok_or(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
                examined: usize::MAX,
                attempted: usize::MAX,
            })?;
        let attempted_keys = distinct_keys
            .checked_add(usize::from(!postings.contains_key(&key)))
            .ok_or(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
                examined: distinct_keys,
                attempted: usize::MAX,
            })?;
        if attempted_keys > limits.distinct_keys {
            return Err(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
                examined: distinct_keys,
                attempted: attempted_keys,
            });
        }

        let examined_bytes = self.metrics.estimated_logical_bytes;
        let previous_posting_capacity = postings.get(&key).map_or(0, Vec::capacity);
        let new_key = !postings.contains_key(&key);
        // `HashMap` and `Vec` do not expose their next growth capacities. Check
        // every completed insertion so a byte-limit failure stops after at most
        // one collection growth step instead of constructing the full index.
        push_sentence_edge_signature(
            postings,
            &mut self.metrics,
            key,
            occurrence_index,
            attempted,
            None,
        )
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        let posting = postings
            .get(&key)
            .ok_or(SentenceEdgeSignatureIndexBuildError::Index(
                signature_metric_overflow(),
            ))?;
        let posting_capacity = posting.capacity();
        let posting_len = posting.len();
        let key_capacity = postings.capacity();
        self.record_signature_shape_change(
            own_depth,
            key.2,
            new_key,
            key_capacity,
            previous_posting_capacity,
            posting_capacity,
            posting_len,
        )
        .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        if self.metrics.estimated_logical_bytes > limits.estimated_logical_bytes {
            return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
                examined: examined_bytes,
                attempted: self.metrics.estimated_logical_bytes,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_signature_shape_change(
        &mut self,
        own_depth: bool,
        depth: usize,
        new_key: bool,
        key_capacity: usize,
        previous_posting_capacity: usize,
        posting_capacity: usize,
        posting_len: usize,
    ) -> Result<(), SentenceEdgeSignatureIndexError> {
        let (distinct_keys, recorded_key_capacity, posting_items) = if own_depth {
            (
                &mut self.metrics.own_distinct_keys,
                &mut self.metrics.own_key_capacity,
                &mut self.metrics.own_posting_items,
            )
        } else {
            (
                &mut self.metrics.all_distinct_keys,
                &mut self.metrics.all_key_capacity,
                &mut self.metrics.all_posting_items,
            )
        };
        *distinct_keys = distinct_keys
            .checked_add(usize::from(new_key))
            .ok_or_else(signature_metric_overflow)?;
        *recorded_key_capacity = key_capacity;
        *posting_items = posting_items
            .checked_add(1)
            .ok_or_else(signature_metric_overflow)?;
        let capacity_growth = posting_capacity
            .checked_sub(previous_posting_capacity)
            .ok_or_else(signature_metric_overflow)?;
        self.metrics.posting_capacity_items = self
            .metrics
            .posting_capacity_items
            .checked_add(capacity_growth)
            .ok_or_else(signature_metric_overflow)?;
        self.metrics.largest_posting = self.metrics.largest_posting.max(posting_len);
        let depth_items = match sentence_edge_signature_depth_band(depth) {
            Some(SentenceEdgeSignatureDepthBand::One) => &mut self.metrics.depth_1_posting_items,
            Some(SentenceEdgeSignatureDepthBand::TwoToThree) => {
                &mut self.metrics.depth_2_to_3_posting_items
            }
            Some(SentenceEdgeSignatureDepthBand::FourOrMore) => {
                &mut self.metrics.depth_4_plus_posting_items
            }
            None => return Err(signature_metric_overflow()),
        };
        *depth_items = depth_items
            .checked_add(1)
            .ok_or_else(signature_metric_overflow)?;
        self.metrics.estimated_logical_bytes = sentence_edge_signature_estimated_logical_bytes(
            self.metrics.own_key_capacity,
            self.metrics.all_key_capacity,
            self.metrics.posting_capacity_items,
        )?;
        Ok(())
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
        for depth in 1..=query_depth {
            prefix = signature_step(prefix, tokens[depth - 1]);
            suffix = signature_step(suffix, tokens[tokens.len() - depth]);
            for candidate_bucket in [Some(bucket), additional_bucket].into_iter().flatten() {
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

    fn refresh_shape_metrics(&mut self) -> Result<(), SentenceEdgeSignatureIndexError> {
        let own = sentence_edge_signature_posting_metrics(&self.own_depth_postings)?;
        let all = sentence_edge_signature_posting_metrics(&self.all_depth_postings)?;
        let posting_items = own
            .posting_items
            .checked_add(all.posting_items)
            .ok_or_else(signature_metric_overflow)?;
        let posting_capacity_items = own
            .posting_capacity_items
            .checked_add(all.posting_capacity_items)
            .ok_or_else(signature_metric_overflow)?;
        let own_key_capacity = self.own_depth_postings.capacity();
        let all_key_capacity = self.all_depth_postings.capacity();
        let estimated_logical_bytes = sentence_edge_signature_estimated_logical_bytes(
            own_key_capacity,
            all_key_capacity,
            posting_capacity_items,
        )?;
        self.metrics = SentenceEdgeSignatureIndexMetrics {
            posting_items,
            own_distinct_keys: self.own_depth_postings.len(),
            all_distinct_keys: self.all_depth_postings.len(),
            own_key_capacity,
            all_key_capacity,
            own_posting_items: own.posting_items,
            all_posting_items: all.posting_items,
            posting_capacity_items,
            largest_posting: own.largest_posting.max(all.largest_posting),
            depth_1_posting_items: own
                .depth_1_posting_items
                .checked_add(all.depth_1_posting_items)
                .ok_or_else(signature_metric_overflow)?,
            depth_2_to_3_posting_items: own
                .depth_2_to_3_posting_items
                .checked_add(all.depth_2_to_3_posting_items)
                .ok_or_else(signature_metric_overflow)?,
            depth_4_plus_posting_items: own
                .depth_4_plus_posting_items
                .checked_add(all.depth_4_plus_posting_items)
                .ok_or_else(signature_metric_overflow)?,
            estimated_logical_bytes,
        };
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn count_signature_postings(
    postings: &HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    mut count: usize,
    bucket: CandidatePostingBucket,
    role: OccurrenceRole,
    depth: usize,
    prefix: u64,
    suffix: u64,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    count = count
        .checked_add(
            postings
                .get(&(bucket, role, depth, prefix))
                .map_or(0, Vec::len),
        )
        .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: 0,
            attempted: usize::MAX,
        })?;
    if suffix != prefix {
        count = count
            .checked_add(
                postings
                    .get(&(bucket, role, depth, suffix))
                    .map_or(0, Vec::len),
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

fn sentence_edge_signature_posting_metrics(
    postings: &HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
) -> Result<SentenceEdgeSignaturePostingMetrics, SentenceEdgeSignatureIndexError> {
    postings.iter().try_fold(
        SentenceEdgeSignaturePostingMetrics::default(),
        |mut metrics, ((_, _, depth, _), posting)| {
            metrics.posting_items = metrics
                .posting_items
                .checked_add(posting.len())
                .ok_or_else(signature_metric_overflow)?;
            metrics.posting_capacity_items = metrics
                .posting_capacity_items
                .checked_add(posting.capacity())
                .ok_or_else(signature_metric_overflow)?;
            metrics.largest_posting = metrics.largest_posting.max(posting.len());
            let depth_items = match sentence_edge_signature_depth_band(*depth) {
                Some(SentenceEdgeSignatureDepthBand::One) => &mut metrics.depth_1_posting_items,
                Some(SentenceEdgeSignatureDepthBand::TwoToThree) => {
                    &mut metrics.depth_2_to_3_posting_items
                }
                Some(SentenceEdgeSignatureDepthBand::FourOrMore) => {
                    &mut metrics.depth_4_plus_posting_items
                }
                None => return Err(signature_metric_overflow()),
            };
            *depth_items = depth_items
                .checked_add(posting.len())
                .ok_or_else(signature_metric_overflow)?;
            Ok(metrics)
        },
    )
}

fn sentence_edge_signature_estimated_logical_bytes(
    own_key_capacity: usize,
    all_key_capacity: usize,
    posting_capacity_items: usize,
) -> Result<usize, SentenceEdgeSignatureIndexError> {
    let key_capacity = own_key_capacity
        .checked_add(all_key_capacity)
        .ok_or_else(signature_metric_overflow)?;
    let key_and_posting_size = size_of::<SentenceEdgeSignatureKey>()
        .checked_add(size_of::<Vec<usize>>())
        .ok_or_else(signature_metric_overflow)?;
    let key_bytes = key_capacity
        .checked_mul(key_and_posting_size)
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
        })?;
    if distinct_keys > limits.distinct_keys {
        return Err(SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
            examined: distinct_keys,
            attempted: distinct_keys,
        });
    }
    if metrics.estimated_logical_bytes > limits.estimated_logical_bytes {
        return Err(SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined: metrics.estimated_logical_bytes,
            attempted: metrics.estimated_logical_bytes,
        });
    }
    Ok(())
}

#[allow(dead_code)]
const SENTENCE_EDGE_SIGNATURE_SEED: u64 = 0xcbf2_9ce4_8422_2325;
#[allow(dead_code)]
const SENTENCE_EDGE_SIGNATURE_PRIME: u64 = 0x0000_0100_0000_01b3;

#[allow(dead_code)]
fn sentence_edge_signature_depth(shorter_len: usize) -> Option<usize> {
    let numerator = shorter_len.checked_mul(usize::from(MIN_WORD_SCORE_EDGE_EVIDENCE))?;
    let required = numerator
        .checked_div(10_000)?
        .checked_add(usize::from(numerator % 10_000 != 0))?;
    required
        .checked_div(2)?
        .checked_add(usize::from(required % 2 != 0))
}

#[allow(dead_code)]
fn sentence_edge_signature_step(state: u64, token: SentenceEvidenceToken) -> u64 {
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

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn push_sentence_edge_signatures(
    postings: &mut HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    metrics: &mut SentenceEdgeSignatureIndexMetrics,
    bucket: CandidatePostingBucket,
    role: OccurrenceRole,
    depth: usize,
    prefix: u64,
    suffix: u64,
    occurrence_index: usize,
    attempted: &mut usize,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    push_sentence_edge_signature(
        postings,
        metrics,
        (bucket, role, depth, prefix),
        occurrence_index,
        attempted,
        allocation_failure_after,
    )?;
    if suffix != prefix {
        push_sentence_edge_signature(
            postings,
            metrics,
            (bucket, role, depth, suffix),
            occurrence_index,
            attempted,
            allocation_failure_after,
        )?;
    }
    Ok(())
}

#[allow(dead_code)]
fn push_sentence_edge_signature(
    postings: &mut HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    metrics: &mut SentenceEdgeSignatureIndexMetrics,
    key: SentenceEdgeSignatureKey,
    occurrence_index: usize,
    attempted: &mut usize,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    *attempted =
        attempted
            .checked_add(1)
            .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: metrics.posting_items,
                attempted: usize::MAX,
            })?;
    if allocation_failure_after.is_some_and(|limit| metrics.posting_items >= limit) {
        return Err(SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: metrics.posting_items,
            attempted: *attempted,
        });
    }
    if !postings.contains_key(&key) {
        postings.try_reserve(1).map_err(|_| {
            SentenceEdgeSignatureIndexError::AllocationFailure {
                examined: metrics.posting_items,
                attempted: *attempted,
            }
        })?;
        postings.insert(key, Vec::new());
    }
    let posting =
        postings
            .get_mut(&key)
            .ok_or(SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: metrics.posting_items,
                attempted: *attempted,
            })?;
    posting
        .try_reserve(1)
        .map_err(|_| SentenceEdgeSignatureIndexError::AllocationFailure {
            examined: metrics.posting_items,
            attempted: *attempted,
        })?;
    let next = metrics.posting_items.checked_add(1).ok_or(
        SentenceEdgeSignatureIndexError::CounterOverflow {
            examined: metrics.posting_items,
            attempted: *attempted,
        },
    )?;
    posting.push(occurrence_index);
    metrics.posting_items = next;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn collect_sentence_edge_signatures(
    postings: &HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    plausible: &mut Vec<usize>,
    posting_visits: &mut usize,
    bucket: CandidatePostingBucket,
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
        (bucket, role, depth, prefix),
        attempted,
        allocation_failure_after,
    )?;
    if suffix != prefix {
        collect_sentence_edge_signature(
            postings,
            plausible,
            posting_visits,
            (bucket, role, depth, suffix),
            attempted,
            allocation_failure_after,
        )?;
    }
    Ok(())
}

#[allow(dead_code)]
fn collect_sentence_edge_signature(
    postings: &HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    plausible: &mut Vec<usize>,
    posting_visits: &mut usize,
    key: SentenceEdgeSignatureKey,
    attempted: &mut usize,
    allocation_failure_after: Option<usize>,
) -> Result<(), SentenceEdgeSignatureIndexError> {
    let Some(posting) = postings.get(&key) else {
        return Ok(());
    };
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

    fn empty_signature_index() -> SentenceEdgeSignatureIndex {
        SentenceEdgeSignatureIndex {
            own_depth_postings: HashMap::new(),
            all_depth_postings: HashMap::new(),
            metrics: SentenceEdgeSignatureIndexMetrics::default(),
        }
    }

    fn unlimited_signature_build_limits() -> SentenceEdgeSignatureIndexBuildLimits {
        SentenceEdgeSignatureIndexBuildLimits {
            posting_items: usize::MAX,
            distinct_keys: usize::MAX,
            estimated_logical_bytes: usize::MAX,
        }
    }

    fn build_body_sentences_with_limits(
        sentences: &[Vec<SentenceEvidenceToken>],
        limits: SentenceEdgeSignatureIndexBuildLimits,
    ) -> Result<SentenceEdgeSignatureIndex, SentenceEdgeSignatureIndexBuildError> {
        let mut index = empty_signature_index();
        index
            .refresh_shape_metrics()
            .map_err(SentenceEdgeSignatureIndexBuildError::Index)?;
        validate_sentence_edge_signature_build_limits(index.metrics, limits)?;
        let mut attempted = 0;
        for (occurrence_index, tokens) in sentences.iter().enumerate() {
            index.insert_unit_with_limits(
                CandidatePostingBucket::Global,
                RecoveryUnitKind::Sentence,
                Some(OccurrenceRole::Body),
                tokens,
                occurrence_index,
                sentence_edge_signature_step,
                &mut attempted,
                limits,
            )?;
        }
        Ok(index)
    }

    fn edge_distinct_tokens(len: usize) -> Vec<SentenceEvidenceToken> {
        let mut tokens = vec![SentenceEvidenceToken::Scalar('a'); len];
        if let Some(last) = tokens.last_mut() {
            *last = SentenceEvidenceToken::Scalar('b');
        }
        tokens
    }

    fn build_error(
        result: Result<SentenceEdgeSignatureIndex, SentenceEdgeSignatureIndexBuildError>,
    ) -> SentenceEdgeSignatureIndexBuildError {
        match result {
            Ok(_) => panic!("expected signature-index construction to fail"),
            Err(error) => error,
        }
    }

    fn insert_body_sentence(
        index: &mut SentenceEdgeSignatureIndex,
        occurrence_index: usize,
        tokens: &[SentenceEvidenceToken],
    ) {
        index
            .insert_unit(
                CandidatePostingBucket::Global,
                RecoveryUnitKind::Sentence,
                Some(OccurrenceRole::Body),
                tokens,
                occurrence_index,
                sentence_edge_signature_step,
            )
            .expect("small test index fits");
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
        let mut index = empty_signature_index();
        for (occurrence_index, tokens) in sequences.iter().enumerate() {
            insert_body_sentence(&mut index, occurrence_index, tokens);
        }

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
        let mut index = empty_signature_index();
        for (occurrence_index, tokens) in candidates.iter().enumerate() {
            insert_body_sentence(&mut index, occurrence_index, tokens);
        }

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
        let mut index = empty_signature_index();
        index
            .insert_unit(
                CandidatePostingBucket::Global,
                RecoveryUnitKind::Sentence,
                Some(OccurrenceRole::Body),
                &tokens,
                0,
                sentence_edge_signature_step,
            )
            .expect("global body sentence is indexed");
        index
            .insert_unit(
                CandidatePostingBucket::Span(None),
                RecoveryUnitKind::Sentence,
                Some(OccurrenceRole::Body),
                &tokens,
                1,
                sentence_edge_signature_step,
            )
            .expect("span body sentence is indexed");
        index
            .insert_unit(
                CandidatePostingBucket::Global,
                RecoveryUnitKind::Sentence,
                Some(OccurrenceRole::RepeatedHeader),
                &tokens,
                2,
                sentence_edge_signature_step,
            )
            .expect("header sentence is indexed");
        index
            .insert_unit(
                CandidatePostingBucket::Global,
                RecoveryUnitKind::Line,
                Some(OccurrenceRole::Body),
                &tokens,
                3,
                sentence_edge_signature_step,
            )
            .expect("line exclusion succeeds");

        let (global_body, _) = query_body_sentence(&index, &tokens);
        assert_eq!(global_body, vec![0]);

        let mut both_buckets = Vec::new();
        index
            .collect_sentence_tokens(
                &mut both_buckets,
                &tokens,
                OccurrenceRole::Body,
                CandidatePostingBucket::Global,
                Some(CandidatePostingBucket::Span(None)),
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
                CandidatePostingBucket::Global,
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
        let mut index = empty_signature_index();
        index
            .insert_sentence(
                CandidatePostingBucket::Global,
                OccurrenceRole::Body,
                &candidate,
                0,
                collide,
            )
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
        let index =
            build_body_sentences_with_limits(&sentences, unlimited_signature_build_limits())
                .expect("small shape-metric index fits");
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
        assert_eq!(metrics.own_distinct_keys, index.own_depth_postings.len());
        assert_eq!(metrics.all_distinct_keys, index.all_depth_postings.len());
        assert_eq!(
            metrics.own_key_capacity,
            index.own_depth_postings.capacity()
        );
        assert_eq!(
            metrics.all_key_capacity,
            index.all_depth_postings.capacity()
        );
        assert_eq!(metrics.largest_posting, 3);
        assert!(metrics.posting_capacity_items >= metrics.posting_items);

        let expected_bytes = size_of::<SentenceEdgeSignatureIndex>()
            + (metrics.own_key_capacity + metrics.all_key_capacity)
                * (size_of::<SentenceEdgeSignatureKey>() + size_of::<Vec<usize>>())
            + metrics.posting_capacity_items * size_of::<usize>();
        assert_eq!(metrics.estimated_logical_bytes, expected_bytes);
    }

    #[test]
    fn equal_prefix_suffix_signature_is_stored_once() {
        let palindrome = token_sequence(&[0, 1, 0, 1, 0, 1, 0]);
        let index =
            build_body_sentences_with_limits(&[palindrome], unlimited_signature_build_limits())
                .expect("palindrome index fits");
        let metrics = index.metrics();

        assert_eq!(metrics.posting_items, 2);
        assert_eq!(metrics.own_posting_items, 1);
        assert_eq!(metrics.all_posting_items, 1);
        assert_eq!(metrics.own_distinct_keys, 1);
        assert_eq!(metrics.all_distinct_keys, 1);
    }

    #[test]
    fn signature_build_limits_accept_exact_boundaries_and_reject_one_below() {
        let sentences = [edge_distinct_tokens(7), edge_distinct_tokens(21)];
        let baseline =
            build_body_sentences_with_limits(&sentences, unlimited_signature_build_limits())
                .expect("baseline index fits");
        let metrics = baseline.metrics();
        let distinct_keys = metrics.own_distinct_keys + metrics.all_distinct_keys;
        let exact = SentenceEdgeSignatureIndexBuildLimits {
            posting_items: metrics.posting_items,
            distinct_keys,
            estimated_logical_bytes: metrics.estimated_logical_bytes,
        };

        let exact_index = build_body_sentences_with_limits(&sentences, exact)
            .expect("all exact limits admit the index");
        assert_eq!(exact_index.metrics(), metrics);

        let posting_error = build_error(build_body_sentences_with_limits(
            &sentences,
            SentenceEdgeSignatureIndexBuildLimits {
                posting_items: metrics.posting_items - 1,
                ..exact
            },
        ));
        assert_eq!(
            posting_error,
            SentenceEdgeSignatureIndexBuildError::PostingLimit {
                examined: metrics.posting_items - 1,
                attempted: metrics.posting_items,
            }
        );

        let key_error = build_error(build_body_sentences_with_limits(
            &sentences,
            SentenceEdgeSignatureIndexBuildLimits {
                distinct_keys: distinct_keys - 1,
                ..exact
            },
        ));
        assert_eq!(
            key_error,
            SentenceEdgeSignatureIndexBuildError::DistinctKeyLimit {
                examined: distinct_keys - 1,
                attempted: distinct_keys,
            }
        );

        let byte_error = build_error(build_body_sentences_with_limits(
            &sentences,
            SentenceEdgeSignatureIndexBuildLimits {
                estimated_logical_bytes: metrics.estimated_logical_bytes - 1,
                ..exact
            },
        ));
        let SentenceEdgeSignatureIndexBuildError::EstimatedByteLimit {
            examined,
            attempted,
        } = byte_error
        else {
            panic!("expected an estimated-byte limit error");
        };
        assert!(examined < metrics.estimated_logical_bytes);
        assert_eq!(attempted, metrics.estimated_logical_bytes);
    }

    #[test]
    fn empty_and_one_token_sentences_have_bounded_metrics() {
        let empty = build_body_sentences_with_limits(&[], unlimited_signature_build_limits())
            .expect("empty index fits");
        assert_eq!(empty.metrics().posting_items, 0);
        assert_eq!(
            empty.metrics().estimated_logical_bytes,
            size_of::<SentenceEdgeSignatureIndex>()
        );

        let one = build_body_sentences_with_limits(
            &[token_sequence(&[0])],
            unlimited_signature_build_limits(),
        )
        .expect("one-token index fits");
        assert_eq!(one.metrics().posting_items, 1);
        assert_eq!(one.metrics().depth_1_posting_items, 1);
        assert_eq!(one.metrics().largest_posting, 1);
        assert_eq!(query_body_sentence(&one, &[]).0, Vec::<usize>::new());
        assert_eq!(query_body_sentence(&one, &token_sequence(&[0])).0, vec![0]);
    }

    #[test]
    fn signature_insertion_reports_allocation_and_counter_failures() {
        let tokens = token_sequence(&[0, 1, 2, 3]);
        let mut index = empty_signature_index();
        let mut attempted = 0;
        let allocation = index
            .insert_sentence_bounded(
                CandidatePostingBucket::Global,
                OccurrenceRole::Body,
                &tokens,
                0,
                sentence_edge_signature_step,
                &mut attempted,
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

        let mut attempted = usize::MAX;
        let overflow = index
            .insert_sentence_bounded(
                CandidatePostingBucket::Global,
                OccurrenceRole::Body,
                &tokens,
                0,
                sentence_edge_signature_step,
                &mut attempted,
                None,
            )
            .expect_err("counter overflow must stop insertion");
        assert_eq!(
            overflow,
            SentenceEdgeSignatureIndexError::CounterOverflow {
                examined: 0,
                attempted: usize::MAX,
            }
        );
    }

    #[test]
    fn signature_query_deduplicates_and_sorts_candidate_indices() {
        let tokens = token_sequence(&[0, 1, 2, 0]);
        let mut index = empty_signature_index();
        for occurrence_index in [3, 1, 2] {
            insert_body_sentence(&mut index, occurrence_index, &tokens);
        }

        assert_eq!(query_body_sentence(&index, &tokens).0, vec![1, 2, 3]);
    }
}
