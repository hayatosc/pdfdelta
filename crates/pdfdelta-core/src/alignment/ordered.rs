use std::collections::{HashMap, HashSet};

use crate::{Error, Result, layout::BlockId};

use super::{
    BlockFeatures, CandidateGenerator, CandidateSource, ExactAnchor,
    features::exact_anchors,
    score::{GroupScore, ScoreOptions, score_groups},
};

const WEIGHT_SUM_TOLERANCE: f64 = 1.0e-9;
const SCORE_TOLERANCE: f64 = 1.0e-12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignmentKind {
    Match,
    Deletion,
    Insertion,
    Unresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignmentConfidence {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignmentEvidence {
    ExactCanonical,
    TextSimilarity,
    Anchor,
    AnchorInterval,
    NeighborConsistency,
    NumericMask,
    SplitMerge,
    NormalizationIssue,
    MoveCandidate,
    CandidateSource(CandidateSource),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentSpan {
    pub kind: AlignmentKind,
    pub old: Vec<BlockId>,
    pub new: Vec<BlockId>,
    pub score: f64,
    pub confidence: AlignmentConfidence,
    pub evidence: Vec<AlignmentEvidence>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alignment {
    pub spans: Vec<AlignmentSpan>,
    pub main_anchors: Vec<ExactAnchor>,
    pub move_candidates: Vec<ExactAnchor>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentOptions {
    pub candidate_limit: usize,
    pub anchor_min_tokens: usize,
    pub min_match_score: f64,
    pub min_score_margin: f64,
    pub gap_penalty: f64,
    pub split_merge_penalty: f64,
    pub matching_weight: f64,
    pub canonical_weight: f64,
    pub min_masked_canonical_similarity: f64,
}

impl Default for AlignmentOptions {
    fn default() -> Self {
        Self {
            candidate_limit: 32,
            anchor_min_tokens: super::DEFAULT_ANCHOR_MIN_TOKENS,
            min_match_score: 0.55,
            min_score_margin: 0.08,
            gap_penalty: 0.35,
            split_merge_penalty: 0.05,
            matching_weight: 0.65,
            canonical_weight: 0.35,
            min_masked_canonical_similarity: 0.5,
        }
    }
}

impl AlignmentOptions {
    fn validate(self) -> Result<Self> {
        if self.candidate_limit == 0 {
            return Err(Error::InvalidConfiguration(
                "alignment candidate_limit must be greater than zero".to_owned(),
            ));
        }
        if self.anchor_min_tokens == 0 {
            return Err(Error::InvalidConfiguration(
                "alignment anchor_min_tokens must be greater than zero".to_owned(),
            ));
        }
        for (name, value) in [
            ("min_match_score", self.min_match_score),
            ("min_score_margin", self.min_score_margin),
            (
                "min_masked_canonical_similarity",
                self.min_masked_canonical_similarity,
            ),
        ] {
            validate_unit_interval(name, value)?;
        }
        for (name, value) in [
            ("gap_penalty", self.gap_penalty),
            ("split_merge_penalty", self.split_merge_penalty),
            ("matching_weight", self.matching_weight),
            ("canonical_weight", self.canonical_weight),
        ] {
            validate_non_negative(name, value)?;
        }
        if (self.matching_weight + self.canonical_weight - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
            return Err(Error::InvalidConfiguration(
                "alignment text weights must sum to 1".to_owned(),
            ));
        }
        Ok(self)
    }
}

pub fn align_ordered(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
) -> Result<Alignment> {
    let options = options.validate()?;
    validate_features("old", old)?;
    validate_features("new", new)?;
    validate_shared_ngram_size(old, new)?;

    let new_indices = new
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let candidate_map = collect_candidates(old, &new_indices, generator, options.candidate_limit)?;
    let all_anchors = exact_anchors(old, new, options.anchor_min_tokens)?;
    let (main_anchors, move_candidates) = anchor_chain(&all_anchors, old, &new_indices)?;
    let old_indices = old
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let move_old = move_candidates
        .iter()
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let move_new = move_candidates
        .iter()
        .map(|anchor| anchor.new)
        .collect::<HashSet<_>>();

    let mut spans = Vec::new();
    let mut old_start = 0;
    let mut new_start = 0;
    let mut has_left_anchor = false;
    for anchor in &main_anchors {
        let old_anchor = old_indices[&anchor.old];
        let new_anchor = new_indices[&anchor.new];
        spans.extend(align_interval(
            &old[old_start..old_anchor],
            &new[new_start..new_anchor],
            &candidate_map,
            options,
            IntervalContext {
                bounded_by_anchors: has_left_anchor,
                contains_move_candidate: contains_move_candidate(
                    &old[old_start..old_anchor],
                    &new[new_start..new_anchor],
                    &move_old,
                    &move_new,
                ),
            },
        )?);
        spans.push(anchor_span(*anchor));
        old_start = old_anchor + 1;
        new_start = new_anchor + 1;
        has_left_anchor = true;
    }
    spans.extend(align_interval(
        &old[old_start..],
        &new[new_start..],
        &candidate_map,
        options,
        IntervalContext {
            bounded_by_anchors: false,
            contains_move_candidate: contains_move_candidate(
                &old[old_start..],
                &new[new_start..],
                &move_old,
                &move_new,
            ),
        },
    )?);
    refine_masked_matches(&mut spans);

    Ok(Alignment {
        spans,
        main_anchors,
        move_candidates,
    })
}

type CandidateMap = HashMap<BlockId, HashMap<BlockId, Vec<CandidateSource>>>;

fn collect_candidates(
    old: &[BlockFeatures],
    new_indices: &HashMap<BlockId, usize>,
    generator: &dyn CandidateGenerator,
    limit: usize,
) -> Result<CandidateMap> {
    let mut all = HashMap::with_capacity(old.len());
    for features in old {
        let mut by_block = HashMap::<BlockId, Vec<CandidateSource>>::new();
        for candidate in generator
            .candidates(features, limit)?
            .into_iter()
            .take(limit)
        {
            if !candidate.coarse_score.is_finite() {
                return Err(Error::Unresolved(format!(
                    "candidate generator returned a non-finite score for block {}",
                    candidate.block.0
                )));
            }
            if !new_indices.contains_key(&candidate.block) {
                return Err(Error::Unresolved(format!(
                    "candidate generator returned unknown new block {}",
                    candidate.block.0
                )));
            }
            let sources = by_block.entry(candidate.block).or_default();
            sources.extend(candidate.sources);
            sources.sort_unstable();
            sources.dedup();
        }
        all.insert(features.block, by_block);
    }
    Ok(all)
}

fn anchor_chain(
    anchors: &[ExactAnchor],
    old: &[BlockFeatures],
    new_indices: &HashMap<BlockId, usize>,
) -> Result<(Vec<ExactAnchor>, Vec<ExactAnchor>)> {
    let old_indices = old
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let mut positioned = Vec::with_capacity(anchors.len());
    for anchor in anchors {
        let old_index = old_indices.get(&anchor.old).copied().ok_or_else(|| {
            Error::Unresolved(format!(
                "anchor references unknown old block {}",
                anchor.old.0
            ))
        })?;
        let new_index = new_indices.get(&anchor.new).copied().ok_or_else(|| {
            Error::Unresolved(format!(
                "anchor references unknown new block {}",
                anchor.new.0
            ))
        })?;
        positioned.push((*anchor, old_index, new_index));
    }
    positioned.sort_by_key(|(_, old_index, _)| *old_index);

    if positioned.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut lengths = vec![1_usize; positioned.len()];
    let mut previous = vec![None; positioned.len()];
    for index in 0..positioned.len() {
        for candidate in 0..index {
            if positioned[candidate].2 < positioned[index].2
                && lengths[candidate] + 1 > lengths[index]
            {
                lengths[index] = lengths[candidate] + 1;
                previous[index] = Some(candidate);
            }
        }
    }
    let mut end = 0;
    for index in 1..lengths.len() {
        if lengths[index] > lengths[end] {
            end = index;
        }
    }
    let mut selected = HashSet::new();
    loop {
        selected.insert(end);
        let Some(parent) = previous[end] else {
            break;
        };
        end = parent;
    }

    let mut main = Vec::new();
    let mut moves = Vec::new();
    for (index, (anchor, _, _)) in positioned.into_iter().enumerate() {
        if selected.contains(&index) {
            main.push(anchor);
        } else {
            moves.push(anchor);
        }
    }
    Ok((main, moves))
}

#[derive(Clone, Copy)]
struct IntervalContext {
    bounded_by_anchors: bool,
    contains_move_candidate: bool,
}

fn align_interval(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    candidates: &CandidateMap,
    options: AlignmentOptions,
    context: IntervalContext,
) -> Result<Vec<AlignmentSpan>> {
    if old.is_empty() && new.is_empty() {
        return Ok(Vec::new());
    }
    if context.contains_move_candidate {
        return Ok(vec![unresolved_span(
            old,
            new,
            AlignmentEvidence::MoveCandidate,
        )]);
    }
    if old
        .iter()
        .chain(new)
        .any(|features| features.has_normalization_issues)
    {
        return Ok(vec![unresolved_span(
            old,
            new,
            AlignmentEvidence::NormalizationIssue,
        )]);
    }

    let width = new.len() + 1;
    let mut cells = vec![Cell::default(); (old.len() + 1) * width];
    cells[0].best = 0.0;

    for old_index in 0..=old.len() {
        for new_index in 0..=new.len() {
            let from = old_index * width + new_index;
            if !cells[from].best.is_finite() {
                continue;
            }
            if old_index < old.len() {
                propose(
                    &mut cells,
                    from,
                    (old_index + 1) * width + new_index,
                    -options.gap_penalty,
                    Transition::Deletion,
                );
            }
            if new_index < new.len() {
                propose(
                    &mut cells,
                    from,
                    old_index * width + new_index + 1,
                    -options.gap_penalty,
                    Transition::Insertion,
                );
            }
            if old_index < old.len()
                && new_index < new.len()
                && let Some(sources) = group_candidate_sources(
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 1],
                    candidates,
                )
            {
                propose_group_match(
                    &mut cells,
                    from,
                    (old_index + 1) * width + new_index + 1,
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 1],
                    sources,
                    options,
                );
            }
            if context.bounded_by_anchors
                && old_index < old.len()
                && new_index + 1 < new.len()
                && let Some(sources) = group_candidate_sources(
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 2],
                    candidates,
                )
            {
                propose_group_match(
                    &mut cells,
                    from,
                    (old_index + 1) * width + new_index + 2,
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 2],
                    sources,
                    options,
                );
            }
            if context.bounded_by_anchors
                && old_index + 1 < old.len()
                && new_index < new.len()
                && let Some(sources) = group_candidate_sources(
                    &old[old_index..old_index + 2],
                    &new[new_index..new_index + 1],
                    candidates,
                )
            {
                propose_group_match(
                    &mut cells,
                    from,
                    (old_index + 2) * width + new_index + 1,
                    &old[old_index..old_index + 2],
                    &new[new_index..new_index + 1],
                    sources,
                    options,
                );
            }
        }
    }

    let final_cell = &cells[old.len() * width + new.len()];
    if final_cell.second.is_finite()
        && final_cell.best - final_cell.second < options.min_score_margin
    {
        return Ok(vec![unresolved_span(
            old,
            new,
            AlignmentEvidence::TextSimilarity,
        )]);
    }
    backtrack(old, new, &cells, width, context)
}

#[derive(Clone)]
enum Transition {
    Match {
        old_count: usize,
        new_count: usize,
        group_score: GroupScore,
        sources: Vec<CandidateSource>,
    },
    Deletion,
    Insertion,
}

#[derive(Clone)]
struct Cell {
    best: f64,
    second: f64,
    transition: Option<Transition>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            best: f64::NEG_INFINITY,
            second: f64::NEG_INFINITY,
            transition: None,
        }
    }
}

fn propose(cells: &mut [Cell], from: usize, to: usize, reward: f64, transition: Transition) {
    let best = cells[from].best + reward;
    let second = cells[from].second + reward;
    update_cell(&mut cells[to], best, Some(transition));
    if second.is_finite() {
        update_cell(&mut cells[to], second, None);
    }
}

fn propose_group_match(
    cells: &mut [Cell],
    from: usize,
    to: usize,
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    sources: Vec<CandidateSource>,
    options: AlignmentOptions,
) {
    let group_score = score_groups(
        old,
        new,
        ScoreOptions {
            matching_weight: options.matching_weight,
            canonical_weight: options.canonical_weight,
            min_score_margin: options.min_score_margin,
        },
    );
    if group_score.score < options.min_match_score
        || group_score.separator_ambiguous
        || group_score.numeric_mask
            && !group_score.exact_canonical
            && group_score.canonical_similarity < options.min_masked_canonical_similarity
    {
        return;
    }
    let split_merge = old.len() != new.len();
    let reward = group_score.score
        - if split_merge {
            options.split_merge_penalty
        } else {
            0.0
        };
    let transition = Transition::Match {
        old_count: old.len(),
        new_count: new.len(),
        group_score,
        sources,
    };
    propose(cells, from, to, reward, transition);
}

fn update_cell(cell: &mut Cell, score: f64, transition: Option<Transition>) {
    if score > cell.best + SCORE_TOLERANCE {
        cell.second = cell.best;
        cell.best = score;
        if let Some(transition) = transition {
            cell.transition = Some(transition);
        }
    } else if (score - cell.best).abs() <= SCORE_TOLERANCE {
        cell.second = cell.best;
    } else if score > cell.second {
        cell.second = score;
    }
}

fn backtrack(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    cells: &[Cell],
    width: usize,
    context: IntervalContext,
) -> Result<Vec<AlignmentSpan>> {
    let mut old_index = old.len();
    let mut new_index = new.len();
    let mut reversed = Vec::new();

    while old_index > 0 || new_index > 0 {
        let transition = cells[old_index * width + new_index]
            .transition
            .clone()
            .ok_or_else(|| Error::Unresolved("alignment DP lost its predecessor".to_owned()))?;
        match transition {
            Transition::Match {
                old_count,
                new_count,
                group_score,
                sources,
            } => {
                let old_start = old_index - old_count;
                let new_start = new_index - new_count;
                reversed.push(match_span(
                    &old[old_start..old_index],
                    &new[new_start..new_index],
                    group_score,
                    sources,
                    context,
                ));
                old_index = old_start;
                new_index = new_start;
            }
            Transition::Deletion => {
                old_index -= 1;
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Deletion,
                    old: vec![old[old_index].block],
                    new: Vec::new(),
                    score: 0.0,
                    confidence: AlignmentConfidence::Medium,
                    evidence: Vec::new(),
                });
            }
            Transition::Insertion => {
                new_index -= 1;
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Insertion,
                    old: Vec::new(),
                    new: vec![new[new_index].block],
                    score: 0.0,
                    confidence: AlignmentConfidence::Medium,
                    evidence: Vec::new(),
                });
            }
        }
    }
    reversed.reverse();
    Ok(reversed)
}

fn match_span(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    score: GroupScore,
    sources: Vec<CandidateSource>,
    context: IntervalContext,
) -> AlignmentSpan {
    let split_merge = old.len() != new.len();
    let mut evidence = vec![if score.exact_canonical {
        AlignmentEvidence::ExactCanonical
    } else {
        AlignmentEvidence::TextSimilarity
    }];
    if split_merge {
        evidence.push(AlignmentEvidence::SplitMerge);
    }
    evidence.extend(sources.into_iter().map(AlignmentEvidence::CandidateSource));
    if context.bounded_by_anchors {
        evidence.push(AlignmentEvidence::AnchorInterval);
    }
    if score.numeric_mask {
        evidence.push(AlignmentEvidence::NumericMask);
    }

    AlignmentSpan {
        kind: AlignmentKind::Match,
        old: old.iter().map(|features| features.block).collect(),
        new: new.iter().map(|features| features.block).collect(),
        score: score.score,
        confidence: if score.exact_canonical && !split_merge {
            AlignmentConfidence::High
        } else {
            AlignmentConfidence::Medium
        },
        evidence,
    }
}

fn group_candidate_sources(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    candidates: &CandidateMap,
) -> Option<Vec<CandidateSource>> {
    let mut sources = Vec::new();
    for old in old {
        let Some(by_new) = candidates.get(&old.block) else {
            continue;
        };
        for new in new {
            if let Some(pair_sources) = by_new.get(&new.block) {
                sources.extend(pair_sources.iter().copied());
            }
        }
    }
    if sources.is_empty() {
        return None;
    }
    sources.sort_unstable();
    sources.dedup();
    Some(sources)
}

fn anchor_span(anchor: ExactAnchor) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Match,
        old: vec![anchor.old],
        new: vec![anchor.new],
        score: 1.0,
        confidence: AlignmentConfidence::High,
        evidence: vec![AlignmentEvidence::ExactCanonical, AlignmentEvidence::Anchor],
    }
}

fn unresolved_span(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    evidence: AlignmentEvidence,
) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Unresolved,
        old: old.iter().map(|features| features.block).collect(),
        new: new.iter().map(|features| features.block).collect(),
        score: 0.0,
        confidence: AlignmentConfidence::Low,
        evidence: vec![evidence],
    }
}

fn refine_masked_matches(spans: &mut Vec<AlignmentSpan>) {
    let independently_supported = spans
        .iter()
        .map(|span| {
            span.kind == AlignmentKind::Match
                && (!span.evidence.contains(&AlignmentEvidence::NumericMask)
                    || span.evidence.contains(&AlignmentEvidence::ExactCanonical)
                    || span.evidence.contains(&AlignmentEvidence::AnchorInterval))
        })
        .collect::<Vec<_>>();
    let masked_non_exact = spans
        .iter()
        .map(|span| {
            span.kind == AlignmentKind::Match
                && span.evidence.contains(&AlignmentEvidence::NumericMask)
                && !span.evidence.contains(&AlignmentEvidence::ExactCanonical)
        })
        .collect::<Vec<_>>();
    let mut neighbor_supported = vec![false; spans.len()];
    let mut unsupported = vec![false; spans.len()];

    for index in 0..spans.len() {
        if !masked_non_exact[index] {
            continue;
        }
        if spans[index]
            .evidence
            .contains(&AlignmentEvidence::AnchorInterval)
        {
            continue;
        }
        let has_previous_match = index > 0 && independently_supported[index - 1];
        let has_following_match = independently_supported.get(index + 1) == Some(&true);
        if has_previous_match && has_following_match {
            neighbor_supported[index] = true;
        } else {
            unsupported[index] = true;
        }
    }

    let original = std::mem::take(spans);
    let mut refined = Vec::with_capacity(original.len());
    let mut start = 0;
    while start < original.len() {
        if original[start]
            .evidence
            .contains(&AlignmentEvidence::Anchor)
        {
            refined.push(original[start].clone());
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < original.len() && !original[end].evidence.contains(&AlignmentEvidence::Anchor) {
            end += 1;
        }
        if unsupported[start..end]
            .iter()
            .any(|unsupported| *unsupported)
        {
            refined.push(unresolved_alignment_interval(&original[start..end]));
        } else {
            for index in start..end {
                let mut span = original[index].clone();
                if neighbor_supported[index] {
                    span.evidence.push(AlignmentEvidence::NeighborConsistency);
                }
                refined.push(span);
            }
        }
        start = end;
    }
    *spans = refined;
}

fn unresolved_alignment_interval(spans: &[AlignmentSpan]) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Unresolved,
        old: spans
            .iter()
            .flat_map(|span| span.old.iter().copied())
            .collect(),
        new: spans
            .iter()
            .flat_map(|span| span.new.iter().copied())
            .collect(),
        score: 0.0,
        confidence: AlignmentConfidence::Low,
        evidence: vec![AlignmentEvidence::NumericMask],
    }
}

fn contains_move_candidate(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    move_old: &HashSet<BlockId>,
    move_new: &HashSet<BlockId>,
) -> bool {
    old.iter()
        .any(|features| move_old.contains(&features.block))
        || new
            .iter()
            .any(|features| move_new.contains(&features.block))
}

fn validate_features(side: &str, features: &[BlockFeatures]) -> Result<()> {
    let mut ids = HashSet::with_capacity(features.len());
    for features in features {
        if !ids.insert(features.block) {
            return Err(Error::Unresolved(format!(
                "duplicate {side} alignment block id {}",
                features.block.0
            )));
        }
        if features.ngram_size == 0 {
            return Err(Error::InvalidConfiguration(format!(
                "{side} alignment feature ngram_size must be non-zero"
            )));
        }
    }
    Ok(())
}

fn validate_shared_ngram_size(old: &[BlockFeatures], new: &[BlockFeatures]) -> Result<()> {
    let configured = old
        .first()
        .or_else(|| new.first())
        .map(|features| features.ngram_size);
    if configured.is_some_and(|size| {
        old.iter()
            .chain(new)
            .any(|features| features.ngram_size != size)
    }) {
        return Err(Error::InvalidConfiguration(
            "all alignment features must use the same ngram_size".to_owned(),
        ));
    }
    Ok(())
}

fn validate_unit_interval(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(Error::InvalidConfiguration(format!(
            "{name} must be finite and between 0 and 1"
        )));
    }
    Ok(())
}

fn validate_non_negative(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::InvalidConfiguration(format!(
            "{name} must be finite and non-negative"
        )));
    }
    Ok(())
}
