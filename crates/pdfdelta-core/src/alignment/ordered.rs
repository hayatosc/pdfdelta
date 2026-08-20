use std::collections::{HashMap, HashSet};

use crate::{Error, Result, layout::BlockId};

use super::{
    BlockFeatures, BlockSeparator, CandidateGenerator, CandidateSource, ExactAnchor,
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
    pub old_separator: Option<BlockSeparator>,
    pub new_separator: Option<BlockSeparator>,
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
    pub max_candidate_visits: usize,
    pub anchor_min_tokens: usize,
    pub max_dp_cells: usize,
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
            max_candidate_visits: 1_000_000,
            anchor_min_tokens: super::DEFAULT_ANCHOR_MIN_TOKENS,
            max_dp_cells: 1_000_000,
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

pub(crate) fn validate_alignment_options(options: AlignmentOptions) -> Result<()> {
    if options.candidate_limit == 0 {
        return Err(Error::InvalidConfiguration(
            "alignment candidate_limit must be greater than zero".to_owned(),
        ));
    }
    if options.max_candidate_visits == 0 {
        return Err(Error::InvalidConfiguration(
            "alignment max_candidate_visits must be greater than zero".to_owned(),
        ));
    }
    if options.anchor_min_tokens == 0 {
        return Err(Error::InvalidConfiguration(
            "alignment anchor_min_tokens must be greater than zero".to_owned(),
        ));
    }
    if options.max_dp_cells == 0 {
        return Err(Error::InvalidConfiguration(
            "alignment max_dp_cells must be greater than zero".to_owned(),
        ));
    }
    for (name, value) in [
        ("min_match_score", options.min_match_score),
        ("min_score_margin", options.min_score_margin),
        (
            "min_masked_canonical_similarity",
            options.min_masked_canonical_similarity,
        ),
    ] {
        validate_unit_interval(name, value)?;
    }
    for (name, value) in [
        ("gap_penalty", options.gap_penalty),
        ("split_merge_penalty", options.split_merge_penalty),
        ("matching_weight", options.matching_weight),
        ("canonical_weight", options.canonical_weight),
    ] {
        validate_non_negative(name, value)?;
    }
    if (options.matching_weight + options.canonical_weight - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
        return Err(Error::InvalidConfiguration(
            "alignment text weights must sum to 1".to_owned(),
        ));
    }
    Ok(())
}

pub fn align_ordered(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
) -> Result<Alignment> {
    validate_alignment_options(options)?;
    validate_features("old", old)?;
    validate_features("new", new)?;
    validate_shared_ngram_size(old, new)?;

    let new_indices = new
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let candidate_map = collect_candidates(
        old,
        &new_indices,
        generator,
        options.candidate_limit,
        options.max_candidate_visits,
    )?;
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
    let mut remaining_dp_cells = options.max_dp_cells;
    for anchor in &main_anchors {
        let old_anchor = old_indices[&anchor.old];
        let new_anchor = new_indices[&anchor.new];
        spans.extend(align_interval(
            &old[old_start..old_anchor],
            &new[new_start..new_anchor],
            &candidate_map,
            options,
            &mut remaining_dp_cells,
            IntervalContext {
                bounded_by_anchors: has_left_anchor,
                move_old: &move_old,
                move_new: &move_new,
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
        &mut remaining_dp_cells,
        IntervalContext {
            bounded_by_anchors: false,
            move_old: &move_old,
            move_new: &move_new,
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
    max_visits: usize,
) -> Result<CandidateMap> {
    let mut remaining_visits = max_visits;
    for features in old {
        let visits = generator.estimated_visits(features, limit)?;
        remaining_visits = remaining_visits
            .checked_sub(visits)
            .ok_or(Error::LimitExceeded {
                resource: "alignment candidate visits",
                limit: max_visits,
            })?;
    }

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
    let mut sorted_new_indices = positioned
        .iter()
        .map(|(_, _, new_index)| *new_index)
        .collect::<Vec<_>>();
    sorted_new_indices.sort_unstable();
    sorted_new_indices.dedup();

    let mut previous = vec![None; positioned.len()];
    let mut fenwick = vec![None; sorted_new_indices.len() + 1];
    let mut chain_end = None;
    for (index, (_, _, new_index)) in positioned.iter().enumerate() {
        let rank = sorted_new_indices.partition_point(|candidate| candidate < new_index);
        let predecessor = query_chain_tip(&fenwick, rank);
        let tip = ChainTip {
            length: predecessor.map_or(1, |tip| tip.length + 1),
            position: index,
        };
        previous[index] = predecessor.map(|tip| tip.position);
        update_chain_tip(&mut fenwick, rank + 1, tip);
        chain_end = preferred_chain_tip(chain_end, Some(tip));
    }

    let Some(mut end) = chain_end.map(|tip| tip.position) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut selected = vec![false; positioned.len()];
    loop {
        selected[end] = true;
        let Some(parent) = previous[end] else {
            break;
        };
        end = parent;
    }

    let mut main = Vec::new();
    let mut moves = Vec::new();
    for (index, (anchor, _, _)) in positioned.into_iter().enumerate() {
        if selected[index] {
            main.push(anchor);
        } else {
            moves.push(anchor);
        }
    }
    Ok((main, moves))
}

#[derive(Clone, Copy)]
struct ChainTip {
    length: usize,
    position: usize,
}

fn query_chain_tip(tree: &[Option<ChainTip>], mut end: usize) -> Option<ChainTip> {
    let mut best = None;
    while end > 0 {
        best = preferred_chain_tip(best, tree[end]);
        end &= end - 1;
    }
    best
}

fn update_chain_tip(tree: &mut [Option<ChainTip>], mut index: usize, tip: ChainTip) {
    while index < tree.len() {
        tree[index] = preferred_chain_tip(tree[index], Some(tip));
        index += index & index.wrapping_neg();
    }
}

fn preferred_chain_tip(current: Option<ChainTip>, candidate: Option<ChainTip>) -> Option<ChainTip> {
    match (current, candidate) {
        (None, candidate) => candidate,
        (current, None) => current,
        (Some(current), Some(candidate))
            if candidate.length > current.length
                || candidate.length == current.length && candidate.position < current.position =>
        {
            Some(candidate)
        }
        (current, Some(_)) => current,
    }
}

#[derive(Clone, Copy)]
struct IntervalContext<'a> {
    bounded_by_anchors: bool,
    move_old: &'a HashSet<BlockId>,
    move_new: &'a HashSet<BlockId>,
}

fn align_interval(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    candidates: &CandidateMap,
    options: AlignmentOptions,
    remaining_dp_cells: &mut usize,
    context: IntervalContext<'_>,
) -> Result<Vec<AlignmentSpan>> {
    if old.is_empty() && new.is_empty() {
        return Ok(Vec::new());
    }

    let width = new.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: "alignment DP cells",
        limit: options.max_dp_cells,
    })?;
    let height = old.len().checked_add(1).ok_or(Error::LimitExceeded {
        resource: "alignment DP cells",
        limit: options.max_dp_cells,
    })?;
    let cell_count = height.checked_mul(width).ok_or(Error::LimitExceeded {
        resource: "alignment DP cells",
        limit: options.max_dp_cells,
    })?;
    if cell_count > *remaining_dp_cells {
        return Err(Error::LimitExceeded {
            resource: "alignment DP cells",
            limit: options.max_dp_cells,
        });
    }
    *remaining_dp_cells -= cell_count;
    let mut cells = Vec::new();
    cells
        .try_reserve_exact(cell_count)
        .map_err(|_| Error::LimitExceeded {
            resource: "alignment DP cells",
            limit: options.max_dp_cells,
        })?;
    cells.resize(cell_count, Cell::default());
    cells[0].best = 0.0;

    for old_index in 0..=old.len() {
        for new_index in 0..=new.len() {
            let from = old_index * width + new_index;
            if !cells[from].best.is_finite() {
                continue;
            }
            let (old_affected, new_affected, evidence) =
                affected_at(old.get(old_index), new.get(new_index), context);
            if old_affected || new_affected {
                let old_count = usize::from(old_affected);
                let new_count = usize::from(new_affected);
                let affected_count = u8::from(old_affected) + u8::from(new_affected);
                propose(
                    &mut cells,
                    from,
                    (old_index + old_count) * width + new_index + new_count,
                    -options.gap_penalty * f64::from(affected_count),
                    Transition::Unresolved {
                        old_count,
                        new_count,
                        evidence,
                    },
                );
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
                && !contains_affected(
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 2],
                    context,
                )
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
                && !contains_affected(
                    &old[old_index..old_index + 2],
                    &new[new_index..new_index + 1],
                    context,
                )
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
    Unresolved {
        old_count: usize,
        new_count: usize,
        evidence: Vec<AlignmentEvidence>,
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
    context: IntervalContext<'_>,
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
            Transition::Unresolved {
                old_count,
                new_count,
                evidence,
            } => {
                let old_start = old_index - old_count;
                let new_start = new_index - new_count;
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Unresolved,
                    old: old[old_start..old_index]
                        .iter()
                        .map(|features| features.block)
                        .collect(),
                    new: new[new_start..new_index]
                        .iter()
                        .map(|features| features.block)
                        .collect(),
                    score: 0.0,
                    confidence: AlignmentConfidence::Low,
                    evidence,
                    old_separator: None,
                    new_separator: None,
                });
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
                    old_separator: None,
                    new_separator: None,
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
                    old_separator: None,
                    new_separator: None,
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
    context: IntervalContext<'_>,
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
        old_separator: score.old_separator,
        new_separator: score.new_separator,
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
        old_separator: None,
        new_separator: None,
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
        old_separator: None,
        new_separator: None,
    }
}

fn affected_at(
    old: Option<&BlockFeatures>,
    new: Option<&BlockFeatures>,
    context: IntervalContext<'_>,
) -> (bool, bool, Vec<AlignmentEvidence>) {
    let old_normalization = old.is_some_and(|features| features.has_normalization_issues);
    let new_normalization = new.is_some_and(|features| features.has_normalization_issues);
    let old_move = old.is_some_and(|features| context.move_old.contains(&features.block));
    let new_move = new.is_some_and(|features| context.move_new.contains(&features.block));
    let mut evidence = Vec::with_capacity(2);
    if old_normalization || new_normalization {
        evidence.push(AlignmentEvidence::NormalizationIssue);
    }
    if old_move || new_move {
        evidence.push(AlignmentEvidence::MoveCandidate);
    }
    (
        old_normalization || old_move,
        new_normalization || new_move,
        evidence,
    )
}

fn contains_affected(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    context: IntervalContext<'_>,
) -> bool {
    old.iter().any(|features| {
        features.has_normalization_issues || context.move_old.contains(&features.block)
    }) || new.iter().any(|features| {
        features.has_normalization_issues || context.move_new.contains(&features.block)
    })
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
        old_separator: None,
        new_separator: None,
    }
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
