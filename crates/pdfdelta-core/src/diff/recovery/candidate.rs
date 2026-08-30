use std::collections::HashMap;

use super::score::{LINE_NGRAM_SIZE, line_trigram_candidate_meets_threshold, ngram_count};
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
