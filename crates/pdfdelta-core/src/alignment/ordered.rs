use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    layout::BlockId,
    validate::{validate_non_negative, validate_unit_interval},
};

use super::{
    BlockFeatures, BlockSeparator, CandidateGenerator, CandidateSource, ExactAnchor,
    anchor::{exact_anchors, partition_anchor_windows, select_monotone_anchor_chain},
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
    /// Canonical token similarity of the winning group variant.
    pub canonical_similarity: f64,
    /// Total-score margin to the best competing transition that reached this
    /// span's DP cell; `None` when no competing path reached the cell.
    pub score_margin: Option<f64>,
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
    /// Non-exact matches scoring at least this strongly keep `Medium`
    /// confidence; weaker admitted matches are calibrated to `Low`.
    pub strong_match_score: f64,
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
            strong_match_score: 0.85,
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
        ("strong_match_score", options.strong_match_score),
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
    if options.strong_match_score < options.min_match_score {
        return Err(Error::InvalidConfiguration(
            "alignment strong_match_score must be greater than or equal to min_match_score"
                .to_owned(),
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
    align_ordered_with_metrics(old, new, generator, options).result
}

/// Candidate visit accounting for one alignment attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AlignmentVisitMetrics {
    /// Sum of `CandidateGenerator::estimated_visits` charged against
    /// `max_candidate_visits` for non-anchor old blocks. On a limit
    /// failure this is the attempted cumulative charge including the block
    /// that exceeded the budget; on an earlier error it is the charge
    /// accumulated before the failure.
    pub candidate_visits: usize,
    /// Checked sum of `CandidateGenerator::estimated_visits` over every
    /// non-anchor old block, independent of the budget: the full candidate
    /// work the alignment would need. `Some` when the full sum completed
    /// (including on a limit failure); `None` when an estimate error or
    /// overflow made the sum unavailable, or the candidate preflight was
    /// never reached (e.g. an earlier alignment error). Identity alignment
    /// is `Some(0)`.
    pub candidate_visits_required: Option<usize>,
    /// Exact-match posting visits of the required sum; `Some` only when
    /// every non-anchor old block reported a breakdown and every component
    /// sum completed. Identity alignment is `Some(0)`.
    pub candidate_visits_required_exact: Option<usize>,
    /// N-gram posting visits of the required sum; `Some` under the same
    /// conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_ngram: Option<usize>,
    /// Short-block fallback visits of the required sum; `Some` under the
    /// same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_short_fallback: Option<usize>,
    /// The `AlignmentOptions::max_candidate_visits` budget the charge was
    /// compared against.
    pub max_candidate_visits: usize,
}

/// Alignment result plus the candidate visit charge of the attempt.
pub(crate) struct AlignmentAttempt {
    pub result: Result<Alignment>,
    pub visit_metrics: AlignmentVisitMetrics,
}

/// Crate-private measured alignment path used by the pipeline; the public
/// `align_ordered` wrapper returns only the alignment.
pub(crate) fn align_ordered_with_metrics(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
) -> AlignmentAttempt {
    let mut candidate_visits = 0_usize;
    let mut candidate_visits_required = None;
    let mut candidate_visits_required_exact = None;
    let mut candidate_visits_required_ngram = None;
    let mut candidate_visits_required_short_fallback = None;
    let result = align_ordered_inner(
        old,
        new,
        generator,
        options,
        &mut candidate_visits,
        &mut candidate_visits_required,
        &mut candidate_visits_required_exact,
        &mut candidate_visits_required_ngram,
        &mut candidate_visits_required_short_fallback,
    );
    AlignmentAttempt {
        result,
        visit_metrics: AlignmentVisitMetrics {
            candidate_visits,
            candidate_visits_required,
            candidate_visits_required_exact,
            candidate_visits_required_ngram,
            candidate_visits_required_short_fallback,
            max_candidate_visits: options.max_candidate_visits,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn align_ordered_inner(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
    candidate_visits: &mut usize,
    candidate_visits_required: &mut Option<usize>,
    candidate_visits_required_exact: &mut Option<usize>,
    candidate_visits_required_ngram: &mut Option<usize>,
    candidate_visits_required_short_fallback: &mut Option<usize>,
) -> Result<Alignment> {
    validate_alignment_options(options)?;
    validate_features("old", old)?;
    validate_features("new", new)?;
    validate_shared_ngram_size(old, new)?;

    if old == new {
        *candidate_visits_required = Some(0);
        *candidate_visits_required_exact = Some(0);
        *candidate_visits_required_ngram = Some(0);
        *candidate_visits_required_short_fallback = Some(0);
        return Ok(identity_alignment(old));
    }

    let new_indices = new
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let old_indices = old
        .iter()
        .enumerate()
        .map(|(index, features)| (features.block, index))
        .collect::<HashMap<_, _>>();
    let all_anchors = exact_anchors(old, new, options.anchor_min_tokens)?;
    let chain = select_monotone_anchor_chain(&all_anchors, old, new)?;
    let main_anchors = chain.main_chain;
    let move_candidates = chain.move_candidates;
    let secondary_chains = secondary_anchor_chains(
        old,
        new,
        &all_anchors,
        &main_anchors,
        &old_indices,
        &new_indices,
    )?;
    let main_anchor_old = main_anchors
        .iter()
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let candidate_map = collect_candidates(
        old,
        &new_indices,
        &main_anchor_old,
        generator,
        options.candidate_limit,
        options.max_candidate_visits,
        candidate_visits,
        candidate_visits_required,
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
    )?;
    let move_old = move_candidates
        .iter()
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let move_new = move_candidates
        .iter()
        .map(|anchor| anchor.new)
        .collect::<HashSet<_>>();

    let windows = partition_anchor_windows(&main_anchors, old, new)?;
    let mut spans = Vec::new();
    let mut remaining_dp_cells = options.max_dp_cells;

    for (interval_index, window) in windows.iter().enumerate() {
        let old_interval = &old[window.old_range.0..window.old_range.1];
        let new_interval = &new[window.new_range.0..window.new_range.1];
        let has_left = window.left_anchor.is_some();
        let has_right = window.right_anchor.is_some();

        spans.extend(align_interval_with_partition_fallback(
            old_interval,
            new_interval,
            &candidate_map,
            options,
            &mut remaining_dp_cells,
            IntervalContext {
                allow_split_merge: has_left || has_right,
                bounded_by_anchors: has_left && has_right,
                move_old: &move_old,
                move_new: &move_new,
            },
            PartitionFallback {
                anchors: &secondary_chains[interval_index],
                old_offset: window.old_range.0,
                new_offset: window.new_range.0,
                old_indices: &old_indices,
                new_indices: &new_indices,
            },
        )?);

        if let Some(right_anchor) = window.right_anchor {
            spans.push(anchor_span(right_anchor));
        }
    }
    refine_masked_matches(&mut spans);

    Ok(Alignment {
        spans,
        main_anchors,
        move_candidates,
    })
}

fn identity_alignment(old: &[BlockFeatures]) -> Alignment {
    Alignment {
        spans: old
            .iter()
            .map(|features| AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![features.block],
                new: vec![features.block],
                score: 1.0,
                canonical_similarity: 1.0,
                score_margin: None,
                confidence: if features.has_normalization_issues {
                    AlignmentConfidence::Medium
                } else {
                    AlignmentConfidence::High
                },
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: None,
                new_separator: None,
            })
            .collect(),
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
    }
}

fn secondary_anchor_chains(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    primary_anchors: &[ExactAnchor],
    main_anchors: &[ExactAnchor],
    old_indices: &HashMap<BlockId, usize>,
    new_indices: &HashMap<BlockId, usize>,
) -> Result<Vec<Vec<ExactAnchor>>> {
    let primary_old = primary_anchors
        .iter()
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let old_boundaries = main_anchors
        .iter()
        .map(|anchor| old_indices[&anchor.old])
        .collect::<Vec<_>>();
    let new_boundaries = main_anchors
        .iter()
        .map(|anchor| new_indices[&anchor.new])
        .collect::<Vec<_>>();
    let mut candidates = vec![Vec::new(); main_anchors.len() + 1];
    for anchor in exact_anchors(old, new, 1)?
        .into_iter()
        .filter(|anchor| !primary_old.contains(&anchor.old))
    {
        let old_interval =
            old_boundaries.partition_point(|index| index < &old_indices[&anchor.old]);
        let new_interval =
            new_boundaries.partition_point(|index| index < &new_indices[&anchor.new]);
        if old_interval == new_interval {
            candidates[old_interval].push(anchor);
        }
    }

    candidates
        .iter()
        .map(|anchors| {
            if anchors.is_empty() {
                Ok(Vec::new())
            } else {
                select_monotone_anchor_chain(anchors, old, new).map(|chain| chain.main_chain)
            }
        })
        .collect()
}

struct PartitionFallback<'a> {
    anchors: &'a [ExactAnchor],
    old_offset: usize,
    new_offset: usize,
    old_indices: &'a HashMap<BlockId, usize>,
    new_indices: &'a HashMap<BlockId, usize>,
}

fn align_interval_with_partition_fallback(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    candidates: &CandidateMap,
    options: AlignmentOptions,
    remaining_dp_cells: &mut usize,
    context: IntervalContext<'_>,
    fallback: PartitionFallback<'_>,
) -> Result<Vec<AlignmentSpan>> {
    let initial = align_interval(old, new, candidates, options, remaining_dp_cells, context)?;
    if fallback.anchors.is_empty() || !is_full_text_similarity_collapse(&initial, old, new) {
        return Ok(preserve_collapsed_moves(initial, old, new, context));
    }

    let retry_context = IntervalContext {
        allow_split_merge: false,
        bounded_by_anchors: false,
        move_old: context.move_old,
        move_new: context.move_new,
    };
    let mut spans = Vec::new();
    let mut old_start = 0;
    let mut new_start = 0;
    for anchor in fallback.anchors {
        let old_anchor = fallback.old_indices[&anchor.old] - fallback.old_offset;
        let new_anchor = fallback.new_indices[&anchor.new] - fallback.new_offset;
        spans.extend(align_interval_preserving_moves(
            &old[old_start..old_anchor],
            &new[new_start..new_anchor],
            candidates,
            options,
            remaining_dp_cells,
            retry_context,
        )?);
        spans.push(partition_span(*anchor));
        old_start = old_anchor + 1;
        new_start = new_anchor + 1;
    }
    spans.extend(align_interval_preserving_moves(
        &old[old_start..],
        &new[new_start..],
        candidates,
        options,
        remaining_dp_cells,
        retry_context,
    )?);
    Ok(spans)
}

fn align_interval_preserving_moves(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    candidates: &CandidateMap,
    options: AlignmentOptions,
    remaining_dp_cells: &mut usize,
    context: IntervalContext<'_>,
) -> Result<Vec<AlignmentSpan>> {
    let spans = align_interval(old, new, candidates, options, remaining_dp_cells, context)?;
    Ok(preserve_collapsed_moves(spans, old, new, context))
}

fn preserve_collapsed_moves(
    spans: Vec<AlignmentSpan>,
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    context: IntervalContext<'_>,
) -> Vec<AlignmentSpan> {
    if is_full_text_similarity_collapse(&spans, old, new)
        && let Some(preserved) = preserve_move_candidates(old, new, context)
    {
        preserved
    } else {
        spans
    }
}

fn is_full_text_similarity_collapse(
    spans: &[AlignmentSpan],
    old: &[BlockFeatures],
    new: &[BlockFeatures],
) -> bool {
    let [span] = spans else {
        return false;
    };
    span.kind == AlignmentKind::Unresolved
        && span.evidence == [AlignmentEvidence::TextSimilarity]
        && span
            .old
            .iter()
            .copied()
            .eq(old.iter().map(|features| features.block))
        && span
            .new
            .iter()
            .copied()
            .eq(new.iter().map(|features| features.block))
}

type CandidateMap = HashMap<BlockId, HashMap<BlockId, Vec<CandidateSource>>>;

#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    old: &[BlockFeatures],
    new_indices: &HashMap<BlockId, usize>,
    main_anchor_old: &HashSet<BlockId>,
    generator: &dyn CandidateGenerator,
    limit: usize,
    max_visits: usize,
    candidate_visits: &mut usize,
    candidate_visits_required: &mut Option<usize>,
    candidate_visits_required_exact: &mut Option<usize>,
    candidate_visits_required_ngram: &mut Option<usize>,
    candidate_visits_required_short_fallback: &mut Option<usize>,
) -> Result<CandidateMap> {
    // The required sum is the checked total over every non-anchor old block
    // and is unavailable (`None`) whenever any estimate errors or the sum
    // overflows. The attempted charge is frozen at the first budget exceed
    // while the required sum keeps accumulating, so a limit failure still
    // reports the full candidate work the alignment would have needed. The
    // exact/ngram/short-fallback components are `Some` only when every
    // block reported a breakdown and every component sum completed; an
    // unknown generator, estimate error, or overflow leaves all three
    // `None` so partial component state is never reported.
    *candidate_visits_required = None;
    *candidate_visits_required_exact = None;
    *candidate_visits_required_ngram = None;
    *candidate_visits_required_short_fallback = None;
    let mut remaining_visits = max_visits;
    let mut exceeded = false;
    let mut required_visits = 0_usize;
    let mut required_exact = 0_usize;
    let mut required_ngram = 0_usize;
    let mut required_short_fallback = 0_usize;
    let mut breakdown_complete = true;
    for features in old
        .iter()
        .filter(|features| !main_anchor_old.contains(&features.block))
    {
        let estimate = match generator.estimate_visits(features, limit) {
            Ok(estimate) => estimate,
            Err(error) => {
                // Error precedence: a limit failure already detected takes
                // precedence over a later estimate error, and the required
                // sum is then incomplete.
                if exceeded {
                    return Err(Error::LimitExceeded {
                        resource: "alignment candidate visits",
                        limit: max_visits,
                    });
                }
                return Err(error);
            }
        };
        let visits = estimate.total;
        if !exceeded {
            // The attempted cumulative charge includes the block that
            // exceeds the budget so the recorded metric explains the
            // failure; on overflow the charge accumulated so far is
            // retained.
            *candidate_visits =
                candidate_visits
                    .checked_add(visits)
                    .ok_or(Error::LimitExceeded {
                        resource: "alignment candidate visits",
                        limit: max_visits,
                    })?;
            if remaining_visits < visits {
                exceeded = true;
            } else {
                remaining_visits -= visits;
            }
        }
        required_visits = required_visits
            .checked_add(visits)
            .ok_or(Error::LimitExceeded {
                resource: "alignment candidate visits",
                limit: max_visits,
            })?;
        match estimate.breakdown {
            Some(breakdown) => {
                required_exact =
                    required_exact
                        .checked_add(breakdown.exact)
                        .ok_or(Error::LimitExceeded {
                            resource: "alignment candidate visits",
                            limit: max_visits,
                        })?;
                required_ngram =
                    required_ngram
                        .checked_add(breakdown.ngram)
                        .ok_or(Error::LimitExceeded {
                            resource: "alignment candidate visits",
                            limit: max_visits,
                        })?;
                required_short_fallback = required_short_fallback
                    .checked_add(breakdown.short_fallback)
                    .ok_or(Error::LimitExceeded {
                        resource: "alignment candidate visits",
                        limit: max_visits,
                    })?;
            }
            None => breakdown_complete = false,
        }
    }
    *candidate_visits_required = Some(required_visits);
    if breakdown_complete {
        *candidate_visits_required_exact = Some(required_exact);
        *candidate_visits_required_ngram = Some(required_ngram);
        *candidate_visits_required_short_fallback = Some(required_short_fallback);
    }
    if exceeded {
        return Err(Error::LimitExceeded {
            resource: "alignment candidate visits",
            limit: max_visits,
        });
    }

    let mut all = HashMap::with_capacity(old.len().saturating_sub(main_anchor_old.len()));
    for features in old
        .iter()
        .filter(|features| !main_anchor_old.contains(&features.block))
    {
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

#[derive(Clone, Copy)]
struct IntervalContext<'a> {
    allow_split_merge: bool,
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
            let old_move_only = is_move_candidate_only(old.get(old_index), context.move_old);
            let new_move_only = is_move_candidate_only(new.get(new_index), context.move_new);
            let move_pair = old_move_only && new_move_only;
            let exact_normalization = old
                .get(old_index)
                .zip(new.get(new_index))
                .filter(|(old, new)| is_exact_normalization_pair(old, new, context));
            if let Some((old, new)) = exact_normalization {
                propose(
                    &mut cells,
                    from,
                    (old_index + 1) * width + new_index + 1,
                    1.0,
                    Transition::Match {
                        old_count: 1,
                        new_count: 1,
                        group_score: GroupScore {
                            score: 1.0,
                            canonical_similarity: 1.0,
                            exact_canonical: true,
                            numeric_mask: old.numeric_mask_applied || new.numeric_mask_applied,
                            separator_ambiguous: false,
                            old_separator: None,
                            new_separator: None,
                        },
                        sources: Vec::new(),
                    },
                );
            }
            // Retain shift alternatives, but prefer the current exact pair by more than the
            // configured ambiguity margin when duplicate issue blocks are interchangeable.
            let skip_exact_penalty = if exact_normalization.is_some() {
                options.min_score_margin + 2.0 * SCORE_TOLERANCE
            } else {
                0.0
            };
            if !new_move_only {
                if old_affected {
                    let transition = if old_move_only {
                        Transition::Deletion {
                            move_candidate: true,
                        }
                    } else {
                        let (_, _, evidence) = affected_at(old.get(old_index), None, context);
                        Transition::Unresolved {
                            old_count: 1,
                            new_count: 0,
                            evidence,
                        }
                    };
                    propose(
                        &mut cells,
                        from,
                        (old_index + 1) * width + new_index,
                        -options.gap_penalty - skip_exact_penalty,
                        transition,
                    );
                } else if old_index < old.len() {
                    propose(
                        &mut cells,
                        from,
                        (old_index + 1) * width + new_index,
                        -options.gap_penalty,
                        Transition::Deletion {
                            move_candidate: false,
                        },
                    );
                }
            }
            if !old_move_only {
                if new_affected {
                    let transition = if new_move_only {
                        Transition::Insertion {
                            move_candidate: true,
                        }
                    } else {
                        let (_, _, evidence) = affected_at(None, new.get(new_index), context);
                        Transition::Unresolved {
                            old_count: 0,
                            new_count: 1,
                            evidence,
                        }
                    };
                    propose(
                        &mut cells,
                        from,
                        old_index * width + new_index + 1,
                        -options.gap_penalty - skip_exact_penalty,
                        transition,
                    );
                } else if new_index < new.len() {
                    propose(
                        &mut cells,
                        from,
                        old_index * width + new_index + 1,
                        -options.gap_penalty,
                        Transition::Insertion {
                            move_candidate: false,
                        },
                    );
                }
            }
            if move_pair {
                propose(
                    &mut cells,
                    from,
                    (old_index + 1) * width + new_index + 1,
                    -2.0 * options.gap_penalty,
                    Transition::MoveCandidates,
                );
            } else if !old_move_only
                && !new_move_only
                && (old_affected || new_affected)
                && old_index < old.len()
                && new_index < new.len()
            {
                propose(
                    &mut cells,
                    from,
                    (old_index + 1) * width + new_index + 1,
                    -options.gap_penalty,
                    Transition::Unresolved {
                        old_count: 1,
                        new_count: 1,
                        evidence,
                    },
                );
            }
            if old_index < old.len()
                && new_index < new.len()
                && !old_affected
                && !new_affected
                && let Some(sources) = group_candidate_sources(
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 1],
                    candidates,
                )
            {
                propose_group_match(
                    &mut cells,
                    (from, (old_index + 1) * width + new_index + 1),
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 1],
                    sources,
                    options,
                    false,
                );
            }
            if context.allow_split_merge
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
                    (from, (old_index + 1) * width + new_index + 2),
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 2],
                    sources,
                    options,
                    !context.bounded_by_anchors,
                );
            }
            if context.allow_split_merge
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
                    (from, (old_index + 2) * width + new_index + 1),
                    &old[old_index..old_index + 2],
                    &new[new_index..new_index + 1],
                    sources,
                    options,
                    !context.bounded_by_anchors,
                );
            }
            if context.allow_split_merge
                && old_index < old.len()
                && new_index + 2 < new.len()
                && !contains_affected(
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 3],
                    context,
                )
                && let Some(sources) = group_candidate_sources(
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 3],
                    candidates,
                )
            {
                propose_group_match(
                    &mut cells,
                    (from, (old_index + 1) * width + new_index + 3),
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 3],
                    sources,
                    options,
                    !context.bounded_by_anchors,
                );
            }
            if context.allow_split_merge
                && old_index + 2 < old.len()
                && new_index < new.len()
                && !contains_affected(
                    &old[old_index..old_index + 3],
                    &new[new_index..new_index + 1],
                    context,
                )
                && let Some(sources) = group_candidate_sources(
                    &old[old_index..old_index + 3],
                    &new[new_index..new_index + 1],
                    candidates,
                )
            {
                propose_group_match(
                    &mut cells,
                    (from, (old_index + 3) * width + new_index + 1),
                    &old[old_index..old_index + 3],
                    &new[new_index..new_index + 1],
                    sources,
                    options,
                    !context.bounded_by_anchors,
                );
            }
        }
    }

    let final_cell = &cells[old.len() * width + new.len()];
    if (!context.bounded_by_anchors
        && final_cell.second.is_finite()
        && final_cell.best - final_cell.second < options.min_score_margin)
        || (final_cell.second.is_finite()
            && (final_cell.best - final_cell.second).abs() <= SCORE_TOLERANCE)
    {
        return Ok(vec![unresolved_span(
            old,
            new,
            AlignmentEvidence::TextSimilarity,
        )]);
    }
    backtrack(old, new, &cells, width, options, context)
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
    Deletion {
        move_candidate: bool,
    },
    Insertion {
        move_candidate: bool,
    },
    MoveCandidates,
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
    edge: (usize, usize),
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    sources: Vec<CandidateSource>,
    options: AlignmentOptions,
    require_exact_canonical: bool,
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
    if require_exact_canonical && !group_score.exact_canonical
        || group_score.score < options.min_match_score
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
    propose(cells, edge.0, edge.1, reward, transition);
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
    options: AlignmentOptions,
    context: IntervalContext<'_>,
) -> Result<Vec<AlignmentSpan>> {
    let mut old_index = old.len();
    let mut new_index = new.len();
    let mut reversed = Vec::new();

    while old_index > 0 || new_index > 0 {
        let cell = &cells[old_index * width + new_index];
        // The runner-up total at this cell is the best competing transition
        // that could have produced this span; a razor-thin margin means the
        // chosen correspondence was locally ambiguous.
        let score_margin = cell
            .second
            .is_finite()
            .then(|| (cell.best - cell.second).max(0.0));
        match cell
            .transition
            .clone()
            .ok_or_else(|| Error::Unresolved("alignment DP lost its predecessor".to_owned()))?
        {
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
                    score_margin,
                    options,
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
                    canonical_similarity: 0.0,
                    score_margin: None,
                    confidence: AlignmentConfidence::Low,
                    evidence,
                    old_separator: None,
                    new_separator: None,
                });
                old_index = old_start;
                new_index = new_start;
            }
            Transition::Deletion { move_candidate } => {
                old_index -= 1;
                let confidence = calibrated_one_sided_confidence(
                    old[old_index].has_normalization_issues,
                    score_margin,
                    &options,
                );
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Deletion,
                    old: vec![old[old_index].block],
                    new: Vec::new(),
                    score: 0.0,
                    canonical_similarity: 0.0,
                    score_margin,
                    confidence,
                    evidence: if move_candidate {
                        vec![AlignmentEvidence::MoveCandidate]
                    } else {
                        Vec::new()
                    },
                    old_separator: None,
                    new_separator: None,
                });
            }
            Transition::Insertion { move_candidate } => {
                new_index -= 1;
                let confidence = calibrated_one_sided_confidence(
                    new[new_index].has_normalization_issues,
                    score_margin,
                    &options,
                );
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Insertion,
                    old: Vec::new(),
                    new: vec![new[new_index].block],
                    score: 0.0,
                    canonical_similarity: 0.0,
                    score_margin,
                    confidence,
                    evidence: if move_candidate {
                        vec![AlignmentEvidence::MoveCandidate]
                    } else {
                        Vec::new()
                    },
                    old_separator: None,
                    new_separator: None,
                });
            }
            Transition::MoveCandidates => {
                old_index -= 1;
                new_index -= 1;
                let ins_confidence = calibrated_one_sided_confidence(
                    new[new_index].has_normalization_issues,
                    score_margin,
                    &options,
                );
                let del_confidence = calibrated_one_sided_confidence(
                    old[old_index].has_normalization_issues,
                    score_margin,
                    &options,
                );
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Insertion,
                    old: Vec::new(),
                    new: vec![new[new_index].block],
                    score: 0.0,
                    canonical_similarity: 0.0,
                    score_margin,
                    confidence: ins_confidence,
                    evidence: vec![AlignmentEvidence::MoveCandidate],
                    old_separator: None,
                    new_separator: None,
                });
                reversed.push(AlignmentSpan {
                    kind: AlignmentKind::Deletion,
                    old: vec![old[old_index].block],
                    new: Vec::new(),
                    score: 0.0,
                    canonical_similarity: 0.0,
                    score_margin,
                    confidence: del_confidence,
                    evidence: vec![AlignmentEvidence::MoveCandidate],
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
    score_margin: Option<f64>,
    options: AlignmentOptions,
    context: IntervalContext<'_>,
) -> AlignmentSpan {
    let split_merge = old.len() != new.len();
    let has_normalization_issues = old.iter().any(|b| b.has_normalization_issues)
        || new.iter().any(|b| b.has_normalization_issues);
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
        canonical_similarity: score.canonical_similarity,
        score_margin,
        confidence: calibrated_match_confidence(
            &score,
            split_merge,
            score_margin,
            has_normalization_issues,
            &options,
        ),
        evidence,
        old_separator: score.old_separator,
        new_separator: score.new_separator,
    }
}

/// Calibrates match confidence based on exactness, score margin, and normalization issues.
fn calibrated_match_confidence(
    score: &GroupScore,
    split_merge: bool,
    score_margin: Option<f64>,
    has_normalization_issues: bool,
    options: &AlignmentOptions,
) -> AlignmentConfidence {
    if score.exact_canonical && !split_merge && !has_normalization_issues {
        return AlignmentConfidence::High;
    }
    let weak_score = !score.exact_canonical && score.score < options.strong_match_score;
    let ambiguous_margin = score_margin.is_some_and(|margin| margin < options.min_score_margin);
    if (!score.exact_canonical && (weak_score || ambiguous_margin || has_normalization_issues))
        || (split_merge && has_normalization_issues)
    {
        AlignmentConfidence::Low
    } else {
        AlignmentConfidence::Medium
    }
}

/// Calibrates deletion/insertion confidence based on margin and normalization issues.
fn calibrated_one_sided_confidence(
    has_normalization_issues: bool,
    score_margin: Option<f64>,
    options: &AlignmentOptions,
) -> AlignmentConfidence {
    let ambiguous_margin = score_margin.is_some_and(|margin| margin < options.min_score_margin);
    if has_normalization_issues || ambiguous_margin {
        AlignmentConfidence::Low
    } else {
        AlignmentConfidence::Medium
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
        canonical_similarity: 1.0,
        score_margin: None,
        confidence: AlignmentConfidence::High,
        evidence: vec![AlignmentEvidence::ExactCanonical, AlignmentEvidence::Anchor],
        old_separator: None,
        new_separator: None,
    }
}

fn partition_span(anchor: ExactAnchor) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Match,
        old: vec![anchor.old],
        new: vec![anchor.new],
        score: 1.0,
        canonical_similarity: 1.0,
        score_margin: None,
        confidence: AlignmentConfidence::High,
        evidence: vec![AlignmentEvidence::ExactCanonical],
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
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: AlignmentConfidence::Low,
        evidence: vec![evidence],
        old_separator: None,
        new_separator: None,
    }
}

fn preserve_move_candidates(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    context: IntervalContext<'_>,
) -> Option<Vec<AlignmentSpan>> {
    if !old
        .iter()
        .any(|block| context.move_old.contains(&block.block))
        && !new
            .iter()
            .any(|block| context.move_new.contains(&block.block))
    {
        return None;
    }

    let mut spans = Vec::new();
    let mut old_start = 0;
    let mut new_start = 0;
    while old_start < old.len() || new_start < new.len() {
        if old
            .get(old_start)
            .is_some_and(|block| context.move_old.contains(&block.block))
        {
            spans.push(move_deletion_span(
                old[old_start].block,
                old[old_start].has_normalization_issues,
            ));
            old_start += 1;
            continue;
        }
        if new
            .get(new_start)
            .is_some_and(|block| context.move_new.contains(&block.block))
        {
            spans.push(move_insertion_span(
                new[new_start].block,
                new[new_start].has_normalization_issues,
            ));
            new_start += 1;
            continue;
        }

        let old_end = old[old_start..]
            .iter()
            .position(|block| context.move_old.contains(&block.block))
            .map_or(old.len(), |offset| old_start + offset);
        let new_end = new[new_start..]
            .iter()
            .position(|block| context.move_new.contains(&block.block))
            .map_or(new.len(), |offset| new_start + offset);
        let evidence = if old[old_start..old_end]
            .iter()
            .chain(&new[new_start..new_end])
            .any(|block| block.has_normalization_issues)
        {
            AlignmentEvidence::NormalizationIssue
        } else {
            AlignmentEvidence::TextSimilarity
        };
        spans.push(unresolved_span(
            &old[old_start..old_end],
            &new[new_start..new_end],
            evidence,
        ));
        old_start = old_end;
        new_start = new_end;
    }
    Some(spans)
}

fn move_deletion_span(block: BlockId, has_normalization_issues: bool) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Deletion,
        old: vec![block],
        new: Vec::new(),
        score: 0.0,
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: if has_normalization_issues {
            AlignmentConfidence::Low
        } else {
            AlignmentConfidence::Medium
        },
        evidence: vec![AlignmentEvidence::MoveCandidate],
        old_separator: None,
        new_separator: None,
    }
}

fn move_insertion_span(block: BlockId, has_normalization_issues: bool) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Insertion,
        old: Vec::new(),
        new: vec![block],
        score: 0.0,
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: if has_normalization_issues {
            AlignmentConfidence::Low
        } else {
            AlignmentConfidence::Medium
        },
        evidence: vec![AlignmentEvidence::MoveCandidate],
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

fn is_move_candidate_only(
    features: Option<&BlockFeatures>,
    move_candidates: &HashSet<BlockId>,
) -> bool {
    features.is_some_and(|features| {
        !features.has_normalization_issues && move_candidates.contains(&features.block)
    })
}

fn is_exact_normalization_pair(
    old: &BlockFeatures,
    new: &BlockFeatures,
    context: IntervalContext<'_>,
) -> bool {
    old.has_normalization_issues
        && new.has_normalization_issues
        && !old.canonical_tokens.is_empty()
        && !new.canonical_tokens.is_empty()
        && !context.move_old.contains(&old.block)
        && !context.move_new.contains(&new.block)
        && old.canonical_tokens == new.canonical_tokens
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
        let has_previous_match = index > 0 && independently_supported[index - 1];
        let has_following_match = independently_supported.get(index + 1) == Some(&true);
        if has_previous_match && has_following_match {
            neighbor_supported[index] = true;
        } else if !spans[index]
            .evidence
            .contains(&AlignmentEvidence::AnchorInterval)
        {
            unsupported[index] = true;
        }
    }

    let original = std::mem::take(spans);
    let mut refined = Vec::with_capacity(original.len());
    let mut index = 0;
    while index < original.len() {
        if unsupported[index] {
            let start = index;
            while index < original.len() && unsupported[index] {
                index += 1;
            }
            refined.push(unresolved_alignment_interval(&original[start..index]));
        } else {
            let mut span = original[index].clone();
            if neighbor_supported[index] {
                span.evidence.push(AlignmentEvidence::NeighborConsistency);
            }
            refined.push(span);
            index += 1;
        }
    }
    *spans = refined;
}

fn unresolved_alignment_interval(spans: &[AlignmentSpan]) -> AlignmentSpan {
    let mut evidence = Vec::new();
    for span in spans {
        for ev in &span.evidence {
            if !evidence.contains(ev) {
                evidence.push(*ev);
            }
        }
    }
    if !evidence.contains(&AlignmentEvidence::NumericMask) {
        evidence.push(AlignmentEvidence::NumericMask);
    }
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
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: AlignmentConfidence::Low,
        evidence,
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use crate::normalize::ComparableToken;

    use super::*;
    use crate::alignment::{Candidate, CandidateVisitBreakdown, CandidateVisitEstimate, ExactHash};

    fn feature(block: u64, key: u64) -> BlockFeatures {
        let scalar = char::from_u32(0x1000 + key as u32).expect("fixture key should be valid");
        let tokens = vec![ComparableToken::Scalar(scalar)];
        BlockFeatures {
            block: BlockId(block),
            exact_hash: ExactHash(key),
            canonical_tokens: tokens.clone(),
            matching_tokens: tokens,
            ngrams: Default::default(),
            ngram_size: 3,
            numeric_mask_applied: false,
            has_normalization_issues: false,
        }
    }

    fn feature_with_issue(block: u64, key: u64, has_normalization_issues: bool) -> BlockFeatures {
        let mut f = feature(block, key);
        f.has_normalization_issues = has_normalization_issues;
        f
    }

    struct FixedVisitsGenerator {
        visits: usize,
        estimated: RefCell<Vec<BlockId>>,
        generated: RefCell<Vec<BlockId>>,
    }

    impl FixedVisitsGenerator {
        fn new(visits: usize) -> Self {
            Self {
                visits,
                estimated: RefCell::new(Vec::new()),
                generated: RefCell::new(Vec::new()),
            }
        }
    }

    impl CandidateGenerator for FixedVisitsGenerator {
        fn estimated_visits(&self, old: &BlockFeatures, _limit: usize) -> Result<usize> {
            self.estimated.borrow_mut().push(old.block);
            Ok(self.visits)
        }

        fn candidates(&self, old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
            self.generated.borrow_mut().push(old.block);
            Ok(Vec::new())
        }
    }

    struct ErroringGenerator {
        visits: usize,
        error_block: BlockId,
    }

    impl CandidateGenerator for ErroringGenerator {
        fn estimated_visits(&self, old: &BlockFeatures, _limit: usize) -> Result<usize> {
            if old.block == self.error_block {
                Err(Error::Unresolved(format!(
                    "estimate failed for block {}",
                    old.block.0
                )))
            } else {
                Ok(self.visits)
            }
        }

        fn candidates(&self, _old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
            Ok(Vec::new())
        }
    }

    struct FixedBreakdownGenerator {
        exact: usize,
        ngram: usize,
        short_fallback: usize,
        estimated: RefCell<Vec<BlockId>>,
        generated: RefCell<Vec<BlockId>>,
    }

    impl FixedBreakdownGenerator {
        fn new(exact: usize, ngram: usize, short_fallback: usize) -> Self {
            Self {
                exact,
                ngram,
                short_fallback,
                estimated: RefCell::new(Vec::new()),
                generated: RefCell::new(Vec::new()),
            }
        }
    }

    impl CandidateGenerator for FixedBreakdownGenerator {
        fn estimated_visits(&self, old: &BlockFeatures, limit: usize) -> Result<usize> {
            Ok(self.estimate_visits(old, limit)?.total)
        }

        fn estimate_visits(
            &self,
            old: &BlockFeatures,
            _limit: usize,
        ) -> Result<CandidateVisitEstimate> {
            self.estimated.borrow_mut().push(old.block);
            Ok(CandidateVisitEstimate {
                total: self.exact + self.ngram + self.short_fallback,
                breakdown: Some(CandidateVisitBreakdown {
                    exact: self.exact,
                    ngram: self.ngram,
                    short_fallback: self.short_fallback,
                }),
            })
        }

        fn candidates(&self, old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
            self.generated.borrow_mut().push(old.block);
            Ok(Vec::new())
        }
    }

    fn distinct_features() -> (Vec<BlockFeatures>, Vec<BlockFeatures>) {
        let old = vec![feature(1, 1), feature(2, 2), feature(3, 3)];
        let new = vec![feature(101, 101), feature(102, 102), feature(103, 103)];
        (old, new)
    }

    #[test]
    fn required_candidate_visits_sums_all_blocks_when_limit_exceeds_midway() {
        let (old, new) = distinct_features();
        let generator = FixedVisitsGenerator::new(6);
        let options = AlignmentOptions {
            max_candidate_visits: 11,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(matches!(
            attempt.result,
            Err(Error::LimitExceeded {
                resource: "alignment candidate visits",
                limit: 11,
            })
        ));
        // The attempted charge is frozen at the first budget exceed (two
        // blocks of six visits) while the required sum covers all three.
        assert_eq!(attempt.visit_metrics.candidate_visits, 12);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, Some(18));
        // The generic generator reports no breakdown, so the components
        // stay unavailable even though the required sum completed.
        assert_eq!(attempt.visit_metrics.candidate_visits_required_exact, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_ngram, None);
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            None
        );
        assert_eq!(attempt.visit_metrics.max_candidate_visits, 11);
        assert_eq!(
            *generator.estimated.borrow(),
            [BlockId(1), BlockId(2), BlockId(3)]
        );
        assert!(generator.generated.borrow().is_empty());
    }

    #[test]
    fn required_candidate_visits_matches_attempted_on_success() {
        let (old, new) = distinct_features();
        let generator = FixedVisitsGenerator::new(6);
        let options = AlignmentOptions {
            max_candidate_visits: 18,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(attempt.result.is_ok());
        assert_eq!(attempt.visit_metrics.candidate_visits, 18);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, Some(18));
        assert_eq!(attempt.visit_metrics.candidate_visits_required_exact, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_ngram, None);
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            None
        );
        assert_eq!(
            *generator.generated.borrow(),
            [BlockId(1), BlockId(2), BlockId(3)]
        );
    }

    #[test]
    fn required_components_sum_to_total_and_survive_a_limit_failure() {
        let (old, new) = distinct_features();
        let generator = FixedBreakdownGenerator::new(2, 3, 1);
        let options = AlignmentOptions {
            max_candidate_visits: 11,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(matches!(
            attempt.result,
            Err(Error::LimitExceeded {
                resource: "alignment candidate visits",
                limit: 11,
            })
        ));
        // The attempted charge is frozen at the first budget exceed (two
        // blocks of six visits) while the required sum and its components
        // cover all three blocks.
        assert_eq!(attempt.visit_metrics.candidate_visits, 12);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, Some(18));
        assert_eq!(
            attempt.visit_metrics.candidate_visits_required_exact,
            Some(6)
        );
        assert_eq!(
            attempt.visit_metrics.candidate_visits_required_ngram,
            Some(9)
        );
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            Some(3)
        );
        assert_eq!(
            *generator.estimated.borrow(),
            [BlockId(1), BlockId(2), BlockId(3)]
        );
        assert!(generator.generated.borrow().is_empty());
    }

    #[test]
    fn required_components_match_attempted_on_success() {
        let (old, new) = distinct_features();
        let generator = FixedBreakdownGenerator::new(2, 3, 1);
        let options = AlignmentOptions {
            max_candidate_visits: 18,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(attempt.result.is_ok());
        assert_eq!(attempt.visit_metrics.candidate_visits, 18);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, Some(18));
        assert_eq!(
            attempt.visit_metrics.candidate_visits_required_exact,
            Some(6)
        );
        assert_eq!(
            attempt.visit_metrics.candidate_visits_required_ngram,
            Some(9)
        );
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            Some(3)
        );
        assert_eq!(
            *generator.generated.borrow(),
            [BlockId(1), BlockId(2), BlockId(3)]
        );
    }

    #[test]
    fn required_candidate_visits_is_zero_for_identity_alignment() {
        let (old, _) = distinct_features();
        let generator = FixedVisitsGenerator::new(6);

        let attempt =
            align_ordered_with_metrics(&old, &old, &generator, AlignmentOptions::default());

        assert!(attempt.result.is_ok());
        assert_eq!(attempt.visit_metrics.candidate_visits, 0);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, Some(0));
        // Identity alignment never consults the generator, so the
        // components are reported as zero.
        assert_eq!(
            attempt.visit_metrics.candidate_visits_required_exact,
            Some(0)
        );
        assert_eq!(
            attempt.visit_metrics.candidate_visits_required_ngram,
            Some(0)
        );
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            Some(0)
        );
        assert!(generator.estimated.borrow().is_empty());
        assert!(generator.generated.borrow().is_empty());
    }

    #[test]
    fn identity_alignment_calibrates_each_span_independently() {
        let blocks = vec![
            feature_with_issue(1, 10, false),
            feature_with_issue(2, 20, true),
            feature_with_issue(3, 30, false),
        ];
        let generator = FixedVisitsGenerator::new(0);

        let alignment = align_ordered(&blocks, &blocks, &generator, AlignmentOptions::default())
            .expect("identity alignment should succeed");

        assert_eq!(alignment.spans.len(), 3);
        assert_eq!(alignment.spans[0].confidence, AlignmentConfidence::High);
        assert_eq!(alignment.spans[1].confidence, AlignmentConfidence::Medium);
        assert_eq!(alignment.spans[2].confidence, AlignmentConfidence::High);
    }

    #[test]
    fn pre_collect_error_leaves_required_candidate_visits_unavailable() {
        let (old, new) = distinct_features();
        let generator = FixedVisitsGenerator::new(6);
        let options = AlignmentOptions {
            max_candidate_visits: 0,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(matches!(
            attempt.result,
            Err(Error::InvalidConfiguration(message)) if message.contains("max_candidate_visits")
        ));
        assert_eq!(attempt.visit_metrics.candidate_visits, 0);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_exact, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_ngram, None);
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            None
        );
    }

    #[test]
    fn estimate_error_before_budget_exceed_keeps_the_original_error() {
        let (old, new) = distinct_features();
        let generator = ErroringGenerator {
            visits: 6,
            error_block: BlockId(1),
        };
        let options = AlignmentOptions {
            max_candidate_visits: 100,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(matches!(attempt.result, Err(Error::Unresolved(_))));
        assert_eq!(attempt.visit_metrics.candidate_visits, 0);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_exact, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_ngram, None);
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            None
        );
    }

    #[test]
    fn estimate_error_after_budget_exceed_reports_limit_without_required_sum() {
        let (old, new) = distinct_features();
        let generator = ErroringGenerator {
            visits: 6,
            error_block: BlockId(3),
        };
        let options = AlignmentOptions {
            max_candidate_visits: 11,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(matches!(
            attempt.result,
            Err(Error::LimitExceeded {
                resource: "alignment candidate visits",
                limit: 11,
            })
        ));
        assert_eq!(attempt.visit_metrics.candidate_visits, 12);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_exact, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_ngram, None);
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            None
        );
    }

    #[test]
    fn required_sum_overflow_after_budget_exceed_reports_limit_without_required() {
        let old = vec![feature(1, 1), feature(2, 2), feature(3, 3), feature(4, 4)];
        let new = vec![
            feature(101, 101),
            feature(102, 102),
            feature(103, 103),
            feature(104, 104),
        ];
        let visits = usize::MAX / 4 + 1;
        let generator = FixedVisitsGenerator::new(visits);
        let options = AlignmentOptions {
            max_candidate_visits: 2 * visits,
            ..AlignmentOptions::default()
        };

        let attempt = align_ordered_with_metrics(&old, &new, &generator, options);

        assert!(matches!(
            attempt.result,
            Err(Error::LimitExceeded {
                resource: "alignment candidate visits",
                limit,
            }) if limit == 2 * visits
        ));
        assert_eq!(attempt.visit_metrics.candidate_visits, 3 * visits);
        assert_eq!(attempt.visit_metrics.candidate_visits_required, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_exact, None);
        assert_eq!(attempt.visit_metrics.candidate_visits_required_ngram, None);
        assert_eq!(
            attempt
                .visit_metrics
                .candidate_visits_required_short_fallback,
            None
        );
    }

    fn group_score(score: f64, exact_canonical: bool) -> GroupScore {
        GroupScore {
            score,
            canonical_similarity: score,
            exact_canonical,
            numeric_mask: false,
            separator_ambiguous: false,
            old_separator: None,
            new_separator: None,
        }
    }

    #[test]
    fn calibrates_non_exact_confidence_from_the_score_band() {
        let options = AlignmentOptions::default();

        assert_eq!(
            calibrated_match_confidence(&group_score(0.84, false), false, None, false, &options),
            AlignmentConfidence::Low
        );
        assert_eq!(
            calibrated_match_confidence(&group_score(0.85, false), false, None, false, &options),
            AlignmentConfidence::Medium
        );
    }

    #[test]
    fn calibrates_a_thin_local_margin_as_low_confidence() {
        let options = AlignmentOptions::default();

        // Thin margin strictly below threshold (< min_score_margin) -> Low
        assert_eq!(
            calibrated_match_confidence(
                &group_score(0.9, false),
                false,
                Some(options.min_score_margin - SCORE_TOLERANCE),
                false,
                &options
            ),
            AlignmentConfidence::Low
        );
        // Exact threshold boundary (margin == min_score_margin) is not ambiguous under `<` -> Medium
        assert_eq!(
            calibrated_match_confidence(
                &group_score(0.9, false),
                false,
                Some(options.min_score_margin),
                false,
                &options
            ),
            AlignmentConfidence::Medium
        );
        // Clearly above threshold boundary -> Medium
        assert_eq!(
            calibrated_match_confidence(
                &group_score(0.9, false),
                false,
                Some(options.min_score_margin + 0.10),
                false,
                &options
            ),
            AlignmentConfidence::Medium
        );
        // Clean exact split/merge is not downgraded by a thin margin.
        assert_eq!(
            calibrated_match_confidence(&group_score(1.0, true), true, Some(0.0), false, &options),
            AlignmentConfidence::Medium
        );
        // Split/merge with normalization issues degrades to Low.
        assert_eq!(
            calibrated_match_confidence(&group_score(1.0, true), true, None, true, &options),
            AlignmentConfidence::Low
        );
    }

    #[test]
    fn one_sided_confidence_calibrates_from_issues_and_competing_margin() {
        let options = AlignmentOptions::default();

        // Uncontested clean one-sided span -> Medium
        assert_eq!(
            calibrated_one_sided_confidence(false, None, &options),
            AlignmentConfidence::Medium
        );
        assert_eq!(
            calibrated_one_sided_confidence(false, Some(options.min_score_margin), &options),
            AlignmentConfidence::Medium
        );
        // Contested one-sided span (near tie with runner-up) -> Low
        assert_eq!(
            calibrated_one_sided_confidence(
                false,
                Some(options.min_score_margin - SCORE_TOLERANCE),
                &options
            ),
            AlignmentConfidence::Low
        );
        // Issue-bearing one-sided span -> Low
        assert_eq!(
            calibrated_one_sided_confidence(true, None, &options),
            AlignmentConfidence::Low
        );
        assert_eq!(
            calibrated_one_sided_confidence(true, Some(0.50), &options),
            AlignmentConfidence::Low
        );
    }

    #[test]
    fn exact_matches_calibration_respects_normalization_issues() {
        let options = AlignmentOptions::default();

        // Clean exact 1:1 match -> High
        assert_eq!(
            calibrated_match_confidence(&group_score(1.0, true), false, Some(0.0), false, &options),
            AlignmentConfidence::High
        );
        // Exact 1:1 match with normalization issues -> Medium (cannot be High)
        assert_eq!(
            calibrated_match_confidence(&group_score(1.0, true), false, Some(0.0), true, &options),
            AlignmentConfidence::Medium
        );
        // Fuzzy match with normalization issues -> Low
        assert_eq!(
            calibrated_match_confidence(&group_score(0.95, false), false, None, true, &options),
            AlignmentConfidence::Low
        );
    }

    #[test]
    fn confidence_calibration_satisfies_monotonicity_laws() {
        let options = AlignmentOptions::default();
        let scores = [0.50, 0.75, 0.84, 0.85, 0.95, 1.0];
        let margins = [
            None,
            Some(0.0),
            Some(0.01),
            Some(0.04),
            Some(0.05),
            Some(0.10),
            Some(0.50),
        ];

        fn ordinal(c: AlignmentConfidence) -> u8 {
            match c {
                AlignmentConfidence::Low => 0,
                AlignmentConfidence::Medium => 1,
                AlignmentConfidence::High => 2,
            }
        }

        // Monotonicity: adding normalization issues never raises confidence
        for score in scores {
            for exact in [false, true] {
                for split in [false, true] {
                    for margin in margins {
                        let clean = calibrated_match_confidence(
                            &group_score(score, exact),
                            split,
                            margin,
                            false,
                            &options,
                        );
                        let with_issues = calibrated_match_confidence(
                            &group_score(score, exact),
                            split,
                            margin,
                            true,
                            &options,
                        );
                        assert!(
                            ordinal(clean) >= ordinal(with_issues),
                            "Clean confidence {clean:?} must be >= issue confidence {with_issues:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn partition_anchor_supports_an_adjacent_masked_match() {
        let partition = ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        };
        let main = ExactAnchor {
            old: BlockId(3),
            new: BlockId(103),
        };
        let mut spans = vec![
            partition_span(partition),
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(2)],
                new: vec![BlockId(102)],
                score: 0.9,
                canonical_similarity: 0.9,
                score_margin: None,
                confidence: AlignmentConfidence::Medium,
                evidence: vec![
                    AlignmentEvidence::TextSimilarity,
                    AlignmentEvidence::NumericMask,
                ],
                old_separator: None,
                new_separator: None,
            },
            anchor_span(main),
        ];

        refine_masked_matches(&mut spans);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].kind, AlignmentKind::Match);
        assert!(
            spans[1]
                .evidence
                .contains(&AlignmentEvidence::NeighborConsistency)
        );
    }

    #[test]
    fn masked_refinement_preserves_move_candidate_boundaries() {
        let mut spans = vec![
            move_deletion_span(BlockId(1), false),
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(2)],
                new: vec![BlockId(102)],
                score: 0.9,
                canonical_similarity: 0.9,
                score_margin: None,
                confidence: AlignmentConfidence::Medium,
                evidence: vec![
                    AlignmentEvidence::TextSimilarity,
                    AlignmentEvidence::NumericMask,
                ],
                old_separator: None,
                new_separator: None,
            },
            move_insertion_span(BlockId(101), false),
        ];

        refine_masked_matches(&mut spans);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].kind, AlignmentKind::Deletion);
        assert!(
            spans[0]
                .evidence
                .contains(&AlignmentEvidence::MoveCandidate)
        );
        assert_eq!(spans[1].kind, AlignmentKind::Unresolved);
        assert_eq!(
            spans[1].evidence,
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::NumericMask
            ]
        );
        assert_eq!(spans[2].kind, AlignmentKind::Insertion);
        assert!(
            spans[2]
                .evidence
                .contains(&AlignmentEvidence::MoveCandidate)
        );
    }

    #[test]
    fn masked_refinement_preserves_deletion_and_match_boundaries() {
        let mut spans = vec![
            AlignmentSpan {
                kind: AlignmentKind::Deletion,
                old: vec![BlockId(1)],
                new: Vec::new(),
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::High,
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: None,
                new_separator: None,
            },
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(2)],
                new: vec![BlockId(102)],
                score: 0.9,
                canonical_similarity: 0.9,
                score_margin: None,
                confidence: AlignmentConfidence::Medium,
                evidence: vec![
                    AlignmentEvidence::TextSimilarity,
                    AlignmentEvidence::NumericMask,
                ],
                old_separator: None,
                new_separator: None,
            },
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(3)],
                new: vec![BlockId(103)],
                score: 1.0,
                canonical_similarity: 1.0,
                score_margin: None,
                confidence: AlignmentConfidence::High,
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: None,
                new_separator: None,
            },
        ];

        refine_masked_matches(&mut spans);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].kind, AlignmentKind::Deletion);
        assert_eq!(spans[1].kind, AlignmentKind::Unresolved);
        assert_eq!(spans[1].score, 0.0);
        assert_eq!(spans[1].canonical_similarity, 0.0);
        assert_eq!(spans[1].score_margin, None);
        assert_eq!(spans[1].confidence, AlignmentConfidence::Low);
        assert_eq!(spans[1].old_separator, None);
        assert_eq!(spans[1].new_separator, None);
        assert_eq!(
            spans[1].evidence,
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::NumericMask
            ]
        );
        assert_eq!(spans[2].kind, AlignmentKind::Match);
    }

    #[test]
    fn masked_refinement_resets_separators_on_degraded_split_or_merge() {
        let mut spans = vec![AlignmentSpan {
            kind: AlignmentKind::Match,
            old: vec![BlockId(1)],
            new: vec![BlockId(101), BlockId(102)],
            score: 0.95,
            canonical_similarity: 0.95,
            score_margin: None,
            confidence: AlignmentConfidence::Medium,
            evidence: vec![
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::NumericMask,
            ],
            old_separator: None,
            new_separator: Some(BlockSeparator::Space),
        }];

        refine_masked_matches(&mut spans);

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(spans[0].score, 0.0);
        assert_eq!(spans[0].canonical_similarity, 0.0);
        assert_eq!(spans[0].confidence, AlignmentConfidence::Low);
        assert_eq!(spans[0].old_separator, None);
        assert_eq!(spans[0].new_separator, None);
    }

    #[test]
    fn anchor_interval_masked_match_receives_neighbor_consistency_when_supported() {
        let mut spans = vec![
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(1)],
                new: vec![BlockId(101)],
                score: 1.0,
                canonical_similarity: 1.0,
                score_margin: None,
                confidence: AlignmentConfidence::High,
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: None,
                new_separator: None,
            },
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(2)],
                new: vec![BlockId(102)],
                score: 0.9,
                canonical_similarity: 0.9,
                score_margin: None,
                confidence: AlignmentConfidence::Medium,
                evidence: vec![
                    AlignmentEvidence::TextSimilarity,
                    AlignmentEvidence::NumericMask,
                    AlignmentEvidence::AnchorInterval,
                ],
                old_separator: None,
                new_separator: None,
            },
            AlignmentSpan {
                kind: AlignmentKind::Match,
                old: vec![BlockId(3)],
                new: vec![BlockId(103)],
                score: 1.0,
                canonical_similarity: 1.0,
                score_margin: None,
                confidence: AlignmentConfidence::High,
                evidence: vec![AlignmentEvidence::ExactCanonical],
                old_separator: None,
                new_separator: None,
            },
        ];

        refine_masked_matches(&mut spans);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].kind, AlignmentKind::Match);
        assert!(
            spans[1]
                .evidence
                .contains(&AlignmentEvidence::NeighborConsistency)
        );
    }
}
