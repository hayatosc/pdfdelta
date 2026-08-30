use std::collections::HashMap;

use super::super::sentence::{
    NearSearchScope, NearSearchWorkClass, RecoveryBudget, RecoveryUnitKind, SentenceEvidenceToken,
    SentenceOccurrence,
};

pub(in crate::diff) const MIN_NEAR_SCORE: u16 = 7_000;
pub(in crate::diff) const MIN_WORD_SCORE_EDGE_EVIDENCE: u16 = 3_000;
pub(in crate::diff) const LINE_NGRAM_SIZE: usize = 3;
const MAX_LINE_NEAR_LENGTH_RATIO: usize = 3;

pub(in crate::diff) struct SentenceEdgeEvidence<'a> {
    old: &'a SentenceOccurrence,
    new: &'a SentenceOccurrence,
    budget: &'a mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
    prefix_tokens: usize,
    suffix_tokens: usize,
    shorter_tokens: usize,
    edge_score: u16,
}

#[cfg_attr(not(test), allow(dead_code))]
impl SentenceEdgeEvidence<'_> {
    pub(in crate::diff) fn prefix_tokens(&self) -> usize {
        self.prefix_tokens
    }

    pub(in crate::diff) fn suffix_tokens(&self) -> usize {
        self.suffix_tokens
    }

    pub(in crate::diff) fn shorter_tokens(&self) -> usize {
        self.shorter_tokens
    }

    pub(in crate::diff) fn edge_score(&self) -> u16 {
        self.edge_score
    }
}

#[derive(Clone, Copy)]
pub(in crate::diff) struct RelationFloorProbe {
    pub(in crate::diff) ceiling: u16,
    pub(in crate::diff) complete: bool,
    can_stop: bool,
    pub(in crate::diff) word_scans: usize,
    pub(in crate::diff) stop_opportunities: usize,
    pub(in crate::diff) potential_saved_word_comparisons: usize,
}

impl RelationFloorProbe {
    pub(in crate::diff) fn new(ceiling: u16) -> Self {
        Self {
            ceiling,
            complete: true,
            can_stop: true,
            word_scans: 0,
            stop_opportunities: 0,
            potential_saved_word_comparisons: 0,
        }
    }

    fn record_word_comparison(&mut self) {
        if self.stop_opportunities != 0 {
            let Some(comparisons) = self.potential_saved_word_comparisons.checked_add(1) else {
                self.complete = false;
                return;
            };
            self.potential_saved_word_comparisons = comparisons;
        }
    }

    fn record_upper_bound(&mut self, upper_bound: u16) {
        if self.can_stop && self.stop_opportunities == 0 && upper_bound <= self.ceiling {
            self.stop_opportunities = 1;
        }
    }
}

trait WordMultisetProbe {
    fn start_word_scan(&mut self, _floor: u16) {}
    fn record_word_comparison(&mut self) {}
    fn record_upper_bound(&mut self, _upper_bound: u16) {}
}

impl WordMultisetProbe for () {}

impl WordMultisetProbe for RelationFloorProbe {
    fn start_word_scan(&mut self, floor: u16) {
        self.word_scans = 1;
        self.can_stop = floor <= self.ceiling;
    }

    fn record_word_comparison(&mut self) {
        RelationFloorProbe::record_word_comparison(self);
    }

    fn record_upper_bound(&mut self, upper_bound: u16) {
        RelationFloorProbe::record_upper_bound(self, upper_bound);
    }
}

pub(in crate::diff) fn sentence_similarity_in_scope(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
) -> Option<u16> {
    sentence_similarity_in_scope_attributed(old, new, budget, scope, NearSearchWorkClass::Shared)
}

pub(in crate::diff) fn sentence_similarity_in_scope_attributed(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
) -> Option<u16> {
    let evidence = sentence_edge_evidence(old, new, budget, scope, class)?;
    sentence_similarity_in_scope_attributed_from_edge_evidence(evidence)
}

pub(in crate::diff) fn sentence_similarity_in_scope_attributed_with_probe(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
    relation_floor_probe: &mut RelationFloorProbe,
) -> Option<u16> {
    let evidence = sentence_edge_evidence(old, new, budget, scope, class)?;
    sentence_similarity_in_scope_attributed_from_edge_evidence_impl(evidence, relation_floor_probe)
}

pub(in crate::diff) fn sentence_edge_evidence<'a>(
    old: &'a SentenceOccurrence,
    new: &'a SentenceOccurrence,
    budget: &'a mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
) -> Option<SentenceEdgeEvidence<'a>> {
    let shorter = old.tokens.len().min(new.tokens.len());
    if shorter == 0 {
        return Some(SentenceEdgeEvidence {
            old,
            new,
            budget,
            scope,
            class,
            prefix_tokens: 0,
            suffix_tokens: 0,
            shorter_tokens: 0,
            edge_score: 0,
        });
    }
    if old.kind == RecoveryUnitKind::Line
        && new.kind == RecoveryUnitKind::Line
        && old.tokens.len().max(new.tokens.len())
            > shorter.checked_mul(MAX_LINE_NEAR_LENGTH_RATIO)?
    {
        return Some(SentenceEdgeEvidence {
            old,
            new,
            budget,
            scope,
            class,
            prefix_tokens: 0,
            suffix_tokens: 0,
            shorter_tokens: shorter,
            edge_score: 0,
        });
    }

    let mut prefix = 0usize;
    while prefix < shorter {
        if !budget.charge_comparisons_in_scope_split(1, old.kind, scope, class.split(1)) {
            return None;
        }
        if old.tokens[prefix] != new.tokens[prefix] {
            break;
        }
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < shorter - prefix {
        if !budget.charge_comparisons_in_scope_split(1, old.kind, scope, class.split(1)) {
            return None;
        }
        if old.tokens[old.tokens.len() - suffix - 1] != new.tokens[new.tokens.len() - suffix - 1] {
            break;
        }
        suffix += 1;
    }

    let shared = prefix.checked_add(suffix)?;
    let edge_score = basis_points(shared, shorter)?;
    Some(SentenceEdgeEvidence {
        old,
        new,
        budget,
        scope,
        class,
        prefix_tokens: prefix,
        suffix_tokens: suffix,
        shorter_tokens: shorter,
        edge_score,
    })
}

pub(in crate::diff) fn sentence_similarity_in_scope_attributed_from_edge_evidence(
    evidence: SentenceEdgeEvidence<'_>,
) -> Option<u16> {
    sentence_similarity_in_scope_attributed_from_edge_evidence_impl(evidence, &mut ())
}

fn sentence_similarity_in_scope_attributed_from_edge_evidence_impl<P: WordMultisetProbe>(
    evidence: SentenceEdgeEvidence<'_>,
    relation_floor_probe: &mut P,
) -> Option<u16> {
    let SentenceEdgeEvidence {
        old,
        new,
        budget,
        scope,
        class,
        shorter_tokens,
        edge_score,
        ..
    } = evidence;
    if shorter_tokens == 0 {
        return Some(0);
    }
    if old.kind == RecoveryUnitKind::Line
        && new.kind == RecoveryUnitKind::Line
        && old.tokens.len().max(new.tokens.len())
            > shorter_tokens.checked_mul(MAX_LINE_NEAR_LENGTH_RATIO)?
    {
        return Some(0);
    }

    let mut exact_score = edge_score;
    if old.kind == RecoveryUnitKind::Line && new.kind == RecoveryUnitKind::Line {
        let old_ngrams = ngram_count(old.tokens.len(), LINE_NGRAM_SIZE)?;
        let new_ngrams = ngram_count(new.tokens.len(), LINE_NGRAM_SIZE)?;
        if multiset_dice_upper_bound(old_ngrams, new_ngrams)? > exact_score {
            let line_score = token_ngram_multiset_dice(
                &old.tokens,
                &new.tokens,
                LINE_NGRAM_SIZE,
                old.kind,
                budget,
                scope,
                class,
            )?;
            exact_score = exact_score.max(line_score);
        }
    }
    if edge_score < MIN_WORD_SCORE_EDGE_EVIDENCE {
        return Some(exact_score);
    }
    if multiset_dice_upper_bound(old.word_ranges.len(), new.word_ranges.len())? > exact_score {
        let word_score = word_multiset_dice_impl(
            old,
            new,
            exact_score,
            old.kind,
            budget,
            scope,
            class,
            relation_floor_probe,
        )?;
        exact_score = exact_score.max(word_score);
    }
    Some(exact_score)
}

fn multiset_dice_upper_bound(old_count: usize, new_count: usize) -> Option<u16> {
    let total = old_count.checked_add(new_count)?;
    basis_points(old_count.min(new_count).checked_mul(2)?, total)
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(in crate::diff) fn word_multiset_dice(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    floor: u16,
    kind: RecoveryUnitKind,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
) -> Option<u16> {
    word_multiset_dice_impl(old, new, floor, kind, budget, scope, class, &mut ())
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(in crate::diff) fn word_multiset_dice_with_probe(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    floor: u16,
    kind: RecoveryUnitKind,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
    relation_floor_probe: &mut RelationFloorProbe,
) -> Option<u16> {
    word_multiset_dice_impl(
        old,
        new,
        floor,
        kind,
        budget,
        scope,
        class,
        relation_floor_probe,
    )
}

#[allow(clippy::too_many_arguments)]
fn word_multiset_dice_impl<P: WordMultisetProbe>(
    old: &SentenceOccurrence,
    new: &SentenceOccurrence,
    floor: u16,
    kind: RecoveryUnitKind,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
    relation_floor_probe: &mut P,
) -> Option<u16> {
    let total = old.word_ranges.len().checked_add(new.word_ranges.len())?;
    if total == 0 {
        return Some(floor);
    }
    relation_floor_probe.start_word_scan(floor);
    let mut old_index = 0usize;
    let mut new_index = 0usize;
    let mut shared = 0usize;
    while old_index < old.word_ranges.len() && new_index < new.word_ranges.len() {
        relation_floor_probe.record_word_comparison();
        if !budget.charge_comparisons_in_scope_split(1, kind, scope, class.split(1)) {
            return None;
        }
        let old_word = old.key.get(old.word_ranges[old_index].clone())?;
        let new_word = new.key.get(new.word_ranges[new_index].clone())?;
        match old_word.cmp(new_word) {
            std::cmp::Ordering::Less => old_index += 1,
            std::cmp::Ordering::Greater => new_index += 1,
            std::cmp::Ordering::Equal => {
                shared = shared.checked_add(1)?;
                old_index += 1;
                new_index += 1;
            }
        }
        let remaining_old = old.word_ranges.len().checked_sub(old_index)?;
        let remaining_new = new.word_ranges.len().checked_sub(new_index)?;
        let attainable_shared = shared.checked_add(remaining_old.min(remaining_new))?;
        let upper_bound = basis_points(attainable_shared.checked_mul(2)?, total)?;
        relation_floor_probe.record_upper_bound(upper_bound);
        if upper_bound <= floor {
            return Some(floor);
        }
    }
    Some(floor.max(basis_points(shared.checked_mul(2)?, total)?))
}

fn token_ngram_multiset_dice(
    old: &[SentenceEvidenceToken],
    new: &[SentenceEvidenceToken],
    size: usize,
    kind: RecoveryUnitKind,
    budget: &mut RecoveryBudget,
    scope: NearSearchScope,
    class: NearSearchWorkClass,
) -> Option<u16> {
    let old_windows = ngram_count(old.len(), size)?;
    let new_windows = ngram_count(new.len(), size)?;
    if old_windows == 0 || new_windows == 0 {
        return Some(0);
    }
    let total = old_windows.checked_add(new_windows)?;
    if !budget.charge_comparisons_in_scope_split(total, kind, scope, class.split(total)) {
        return None;
    }

    let mut old_counts = HashMap::<&[SentenceEvidenceToken], usize>::new();
    let mut new_counts = HashMap::<&[SentenceEvidenceToken], usize>::new();
    old_counts.try_reserve(old_windows).ok()?;
    new_counts.try_reserve(new_windows).ok()?;
    for ngram in old.windows(size) {
        let count = old_counts.entry(ngram).or_default();
        *count = count.checked_add(1)?;
    }
    for ngram in new.windows(size) {
        let count = new_counts.entry(ngram).or_default();
        *count = count.checked_add(1)?;
    }
    let (shorter, longer) = if old_counts.len() <= new_counts.len() {
        (&old_counts, &new_counts)
    } else {
        (&new_counts, &old_counts)
    };
    if !budget.charge_comparisons_in_scope_split(
        shorter.len(),
        kind,
        scope,
        class.split(shorter.len()),
    ) {
        return None;
    }
    let shared = shorter.iter().try_fold(0usize, |shared, (ngram, count)| {
        shared.checked_add((*count).min(longer.get(ngram).copied().unwrap_or(0)))
    })?;
    basis_points(shared.checked_mul(2)?, total)
}

pub(in crate::diff) fn ngram_count(token_count: usize, size: usize) -> Option<usize> {
    if size == 0 || token_count < size {
        return Some(0);
    }
    token_count.checked_sub(size)?.checked_add(1)
}

pub(in crate::diff) fn line_trigram_candidate_meets_threshold(
    shared: usize,
    query_ngrams: usize,
    candidate_ngrams: usize,
) -> Option<bool> {
    let total = query_ngrams.checked_add(candidate_ngrams)?;
    if total == 0 {
        return Some(false);
    }
    let score = shared.checked_mul(20_000)?.checked_div(total)?;
    Some(score >= usize::from(MIN_NEAR_SCORE))
}

pub(in crate::diff) fn basis_points(numerator: usize, denominator: usize) -> Option<u16> {
    if denominator == 0 {
        return Some(0);
    }
    let score = numerator.checked_mul(10_000)?.checked_div(denominator)?;
    u16::try_from(score).ok()
}
