use std::collections::HashMap;

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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::diff) struct SentenceEdgeSignatureQueryMetrics {
    pub(in crate::diff) posting_visits: usize,
    pub(in crate::diff) candidate_union: usize,
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
    pub(in crate::diff) fn new(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
    ) -> Option<Self> {
        Self::new_with_signature_step(occurrences, scope, sentence_edge_signature_step)
    }

    fn new_with_signature_step(
        occurrences: &[SentenceOccurrence],
        scope: CandidatePostingIndexScope<'_>,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    ) -> Option<Self> {
        let mut index = Self {
            own_depth_postings: HashMap::new(),
            all_depth_postings: HashMap::new(),
            metrics: SentenceEdgeSignatureIndexMetrics::default(),
        };
        for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
            let Some(bucket) = candidate_posting_bucket(scope, occurrence_index, occurrence)?
            else {
                continue;
            };
            index.insert_unit(
                bucket,
                occurrence.kind,
                occurrence.role.map(OccurrenceRole::from),
                &occurrence.tokens,
                occurrence_index,
                signature_step,
            )?;
        }
        Some(index)
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

    fn insert_unit(
        &mut self,
        bucket: CandidatePostingBucket,
        kind: RecoveryUnitKind,
        role: Option<OccurrenceRole>,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    ) -> Option<()> {
        if kind != RecoveryUnitKind::Sentence {
            return Some(());
        }
        let Some(role) = role else {
            return Some(());
        };
        self.insert_sentence(bucket, role, tokens, occurrence_index, signature_step)
    }

    fn insert_sentence(
        &mut self,
        bucket: CandidatePostingBucket,
        role: OccurrenceRole,
        tokens: &[SentenceEvidenceToken],
        occurrence_index: usize,
        signature_step: fn(u64, SentenceEvidenceToken) -> u64,
    ) -> Option<()> {
        let own_depth = sentence_edge_signature_depth(tokens.len())?;
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
            )?;
        }
        Some(())
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
        plausible.clear();
        let query_depth = sentence_edge_signature_depth(tokens.len())?;
        let mut posting_visits = 0usize;
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
                    )?;
                }
            }
        }
        plausible.sort_unstable();
        plausible.dedup();
        Some(SentenceEdgeSignatureQueryMetrics {
            posting_visits,
            candidate_union: plausible.len(),
        })
    }
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
) -> Option<()> {
    push_sentence_edge_signature(
        postings,
        metrics,
        (bucket, role, depth, prefix),
        occurrence_index,
    )?;
    if suffix != prefix {
        push_sentence_edge_signature(
            postings,
            metrics,
            (bucket, role, depth, suffix),
            occurrence_index,
        )?;
    }
    Some(())
}

#[allow(dead_code)]
fn push_sentence_edge_signature(
    postings: &mut HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    metrics: &mut SentenceEdgeSignatureIndexMetrics,
    key: SentenceEdgeSignatureKey,
    occurrence_index: usize,
) -> Option<()> {
    if !postings.contains_key(&key) {
        postings.try_reserve(1).ok()?;
        postings.insert(key, Vec::new());
    }
    let posting = postings.get_mut(&key)?;
    posting.try_reserve(1).ok()?;
    posting.push(occurrence_index);
    metrics.posting_items = metrics.posting_items.checked_add(1)?;
    Some(())
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
) -> Option<()> {
    collect_sentence_edge_signature(
        postings,
        plausible,
        posting_visits,
        (bucket, role, depth, prefix),
    )?;
    if suffix != prefix {
        collect_sentence_edge_signature(
            postings,
            plausible,
            posting_visits,
            (bucket, role, depth, suffix),
        )?;
    }
    Some(())
}

#[allow(dead_code)]
fn collect_sentence_edge_signature(
    postings: &HashMap<SentenceEdgeSignatureKey, Vec<usize>>,
    plausible: &mut Vec<usize>,
    posting_visits: &mut usize,
    key: SentenceEdgeSignatureKey,
) -> Option<()> {
    let Some(posting) = postings.get(&key) else {
        return Some(());
    };
    *posting_visits = posting_visits.checked_add(posting.len())?;
    plausible.try_reserve(posting.len()).ok()?;
    plausible.extend_from_slice(posting);
    Some(())
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
}
