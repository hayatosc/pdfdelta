use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    Error, Result,
    layout::BlockId,
    validate::{validate_non_negative, validate_unit_interval},
};

use super::{
    AnchorIntervalWindow, BlockFeatures, BlockSeparator, CandidateGenerator, CandidateSource,
    ExactAnchor,
    anchor::{
        exact_anchors, partition_anchor_windows, primary_exact_anchors,
        select_monotone_anchor_chain,
    },
    score::{GroupScore, ScoreOptions, score_groups},
};

const WEIGHT_SUM_TOLERANCE: f64 = 1.0e-9;
const SCORE_TOLERANCE: f64 = 1.0e-12;
// deliberate: keep shorter exact matches confined to collapse fallback; tune
// this direct-boundary threshold from real-PDF benchmark evidence.
const READING_ORDER_BOUNDARY_MIN_TOKENS: usize = 4;

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
#[non_exhaustive]
pub enum AlignmentEvidence {
    ExactCanonical,
    TextSimilarity,
    /// No candidate edge connected the collapsed alignment interval.
    CandidateSetEmpty,
    /// Candidate edges existed, but scoring or admission filters rejected every match proposal.
    CandidateScoringRejected,
    /// Admissible match proposals existed, but competing DP paths remained ambiguous.
    CandidateCompetition,
    /// The token diff exceeded its configured edit-distance bound.
    DiffEditDistanceExceeded,
    /// A low-confidence match failed the token-diff plausibility gate.
    DiffRejectedAsImplausible,
    Anchor,
    AnchorInterval,
    NeighborConsistency,
    NumericMask,
    SplitMerge,
    NormalizationIssue,
    ExtractionGap,
    ReadingOrderUnknown,
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
    /// `max_candidate_visits` for non-anchor old blocks outside forced
    /// uncertainty windows. On a limit
    /// failure this is the attempted cumulative charge including the block
    /// that exceeded the budget; on an earlier error it is the charge
    /// accumulated before the failure.
    pub candidate_visits: usize,
    /// Checked sum of `CandidateGenerator::estimated_visits` over every
    /// eligible old block, independent of the budget: the full candidate
    /// work the alignment would need. `Some` when the full sum completed
    /// (including on a limit failure); `None` when an estimate error or
    /// overflow made the sum unavailable, or the candidate preflight was
    /// never reached (e.g. an earlier alignment error). Identity alignment
    /// is `Some(0)`.
    pub candidate_visits_required: Option<usize>,
    /// Exact-match posting visits of the required sum; `Some` only when
    /// every eligible old block reported a breakdown and every component
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
    /// Exact number of dynamic-programming cells charged during the attempt.
    pub dp_cells: usize,
    /// The `AlignmentOptions::max_dp_cells` budget the charge was compared
    /// against.
    pub max_dp_cells: usize,
}

/// Alignment result plus resource-accounting metrics for the attempt.
pub(crate) struct AlignmentAttempt {
    pub result: Result<Alignment>,
    pub visit_metrics: AlignmentVisitMetrics,
}

/// One validated anchor decision shared by candidate indexing and alignment.
pub(crate) struct AlignmentGapPlan {
    all_anchors: Vec<ExactAnchor>,
    main_anchors: Vec<ExactAnchor>,
    move_candidates: Vec<ExactAnchor>,
    windows: Vec<AnchorIntervalWindow>,
    forced_windows: BTreeMap<usize, ForcedWindowCauses>,
    excluded_old: HashSet<BlockId>,
    excluded_new: HashSet<BlockId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ForcedWindowCauses {
    extraction_gap: bool,
    reading_order_unknown: bool,
}

impl ForcedWindowCauses {
    fn evidence(self) -> Vec<AlignmentEvidence> {
        let mut evidence = Vec::with_capacity(2);
        if self.extraction_gap {
            evidence.push(AlignmentEvidence::ExtractionGap);
        }
        if self.reading_order_unknown {
            evidence.push(AlignmentEvidence::ReadingOrderUnknown);
        }
        evidence
    }
}

impl AlignmentGapPlan {
    pub(crate) fn allows_new_block(&self, block: BlockId) -> bool {
        !self.excluded_new.contains(&block)
    }
}

/// Crate-private measured alignment path used by the pipeline; the public
/// `align_ordered` wrapper returns only the alignment.
pub(crate) fn align_ordered_with_metrics(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
) -> AlignmentAttempt {
    match plan_ordered_gaps(old, new, options, &[], &[], &[], &[], &[], &[]) {
        Ok(plan) => align_ordered_with_metrics_and_gap_plan(old, new, generator, options, plan),
        Err(error) => AlignmentAttempt {
            result: Err(error),
            visit_metrics: empty_visit_metrics(options),
        },
    }
}

/// Builds the anchor-window plan used to isolate extraction and layout
/// uncertainty before the candidate index is constructed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_ordered_gaps(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    options: AlignmentOptions,
    old_gap_boundaries: &[usize],
    new_gap_boundaries: &[usize],
    old_extraction_uncertain_indices: &[usize],
    new_extraction_uncertain_indices: &[usize],
    old_uncertain_indices: &[usize],
    new_uncertain_indices: &[usize],
) -> Result<AlignmentGapPlan> {
    validate_alignment_options(options)?;
    validate_features("old", old)?;
    validate_features("new", new)?;
    validate_shared_ngram_size(old, new)?;
    validate_gap_boundaries("old", old_gap_boundaries, old.len())?;
    validate_gap_boundaries("new", new_gap_boundaries, new.len())?;
    validate_uncertain_indices(
        "old extraction",
        old_extraction_uncertain_indices,
        old.len(),
    )?;
    validate_uncertain_indices(
        "new extraction",
        new_extraction_uncertain_indices,
        new.len(),
    )?;
    validate_uncertain_indices("old", old_uncertain_indices, old.len())?;
    validate_uncertain_indices("new", new_uncertain_indices, new.len())?;

    if old == new
        && old_gap_boundaries.is_empty()
        && new_gap_boundaries.is_empty()
        && old_extraction_uncertain_indices.is_empty()
        && new_extraction_uncertain_indices.is_empty()
        && old_uncertain_indices.is_empty()
        && new_uncertain_indices.is_empty()
    {
        return Ok(AlignmentGapPlan {
            all_anchors: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
            windows: Vec::new(),
            forced_windows: BTreeMap::new(),
            excluded_old: HashSet::new(),
            excluded_new: HashSet::new(),
        });
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
    let uncertain_old = old_uncertain_indices
        .iter()
        .chain(old_extraction_uncertain_indices)
        .copied()
        .collect::<HashSet<_>>();
    let uncertain_new = new_uncertain_indices
        .iter()
        .chain(new_extraction_uncertain_indices)
        .copied()
        .collect::<HashSet<_>>();
    let all_anchors = primary_exact_anchors(old, new, options.anchor_min_tokens)?
        .into_iter()
        .filter(|anchor| {
            !uncertain_old.contains(&old_indices[&anchor.old])
                && !uncertain_new.contains(&new_indices[&anchor.new])
        })
        .collect::<Vec<_>>();
    let chain = select_monotone_anchor_chain(&all_anchors, old, new)?;
    let main_anchors = chain.main_chain;
    let windows = partition_anchor_windows(&main_anchors, old, new)?;
    let mut forced_windows = BTreeMap::<usize, ForcedWindowCauses>::new();
    for index in forced_window_indices(
        &main_anchors,
        old_gap_boundaries,
        new_gap_boundaries,
        &old_indices,
        &new_indices,
    ) {
        forced_windows.entry(index).or_default().extraction_gap = true;
    }
    for index in uncertain_window_indices(
        &main_anchors,
        old_extraction_uncertain_indices,
        new_extraction_uncertain_indices,
        &old_indices,
        &new_indices,
    ) {
        forced_windows.entry(index).or_default().extraction_gap = true;
    }
    for index in uncertain_window_indices(
        &main_anchors,
        old_uncertain_indices,
        new_uncertain_indices,
        &old_indices,
        &new_indices,
    ) {
        forced_windows
            .entry(index)
            .or_default()
            .reading_order_unknown = true;
    }
    let (windows, forced_windows) = refine_reading_order_windows(
        old,
        new,
        &all_anchors,
        windows,
        forced_windows,
        &uncertain_old,
        &uncertain_new,
        old_uncertain_indices,
        new_uncertain_indices,
        &old_indices,
        &new_indices,
    )?;
    let mut excluded_old = windows
        .iter()
        .filter_map(|window| window.right_anchor.map(|anchor| anchor.old))
        .collect::<HashSet<_>>();
    let mut excluded_new = HashSet::new();
    for &index in forced_windows.keys() {
        let window = &windows[index];
        excluded_old.extend(
            old[window.old_range.0..window.old_range.1]
                .iter()
                .map(|features| features.block),
        );
        excluded_new.extend(
            new[window.new_range.0..window.new_range.1]
                .iter()
                .map(|features| features.block),
        );
    }

    Ok(AlignmentGapPlan {
        all_anchors,
        main_anchors,
        move_candidates: chain.move_candidates,
        windows,
        forced_windows,
        excluded_old,
        excluded_new,
    })
}

/// Aligns with a precomputed gap plan so candidate indexing and alignment use
/// the same anchor-window decision.
pub(crate) fn align_ordered_with_metrics_and_gap_plan(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
    plan: AlignmentGapPlan,
) -> AlignmentAttempt {
    let mut visit_metrics = empty_visit_metrics(options);
    let mut dp_cell_budget = DpCellBudget::new(options.max_dp_cells);
    let result = align_ordered_inner(
        old,
        new,
        generator,
        options,
        plan,
        &mut visit_metrics,
        &mut dp_cell_budget,
    );
    visit_metrics.dp_cells = dp_cell_budget.consumed();
    AlignmentAttempt {
        result,
        visit_metrics,
    }
}

fn empty_visit_metrics(options: AlignmentOptions) -> AlignmentVisitMetrics {
    AlignmentVisitMetrics {
        candidate_visits: 0,
        candidate_visits_required: None,
        candidate_visits_required_exact: None,
        candidate_visits_required_ngram: None,
        candidate_visits_required_short_fallback: None,
        max_candidate_visits: options.max_candidate_visits,
        dp_cells: 0,
        max_dp_cells: options.max_dp_cells,
    }
}

struct DpCellBudget {
    initial: usize,
    remaining: usize,
}

impl DpCellBudget {
    fn new(limit: usize) -> Self {
        Self {
            initial: limit,
            remaining: limit,
        }
    }

    fn consumed(&self) -> usize {
        self.initial - self.remaining
    }
}

fn align_ordered_inner(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    options: AlignmentOptions,
    plan: AlignmentGapPlan,
    visit_metrics: &mut AlignmentVisitMetrics,
    dp_cell_budget: &mut DpCellBudget,
) -> Result<Alignment> {
    if old == new && plan.forced_windows.is_empty() {
        visit_metrics.candidate_visits_required = Some(0);
        visit_metrics.candidate_visits_required_exact = Some(0);
        visit_metrics.candidate_visits_required_ngram = Some(0);
        visit_metrics.candidate_visits_required_short_fallback = Some(0);
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
    let interval_anchors = plan
        .windows
        .iter()
        .filter_map(|window| window.right_anchor)
        .collect::<Vec<_>>();
    let secondary_chains = secondary_anchor_chains(
        old,
        new,
        &plan.all_anchors,
        &interval_anchors,
        &old_indices,
        &new_indices,
    )?;
    let candidate_map = collect_candidates(
        old,
        &new_indices,
        &plan.excluded_old,
        generator,
        options.candidate_limit,
        options.max_candidate_visits,
        visit_metrics,
    )?;
    let move_old = plan
        .move_candidates
        .iter()
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let move_new = plan
        .move_candidates
        .iter()
        .map(|anchor| anchor.new)
        .collect::<HashSet<_>>();

    let mut spans = Vec::new();
    let main_anchor_set = plan.main_anchors.iter().copied().collect::<HashSet<_>>();

    for (interval_index, window) in plan.windows.iter().enumerate() {
        let old_interval = &old[window.old_range.0..window.old_range.1];
        let new_interval = &new[window.new_range.0..window.new_range.1];
        let has_left = window.left_anchor.is_some();
        let has_right = window.right_anchor.is_some();

        if let Some(causes) = plan.forced_windows.get(&interval_index) {
            if !old_interval.is_empty() || !new_interval.is_empty() {
                spans.push(unresolved_span_with_evidence(
                    old_interval,
                    new_interval,
                    causes.evidence(),
                ));
            }
        } else {
            spans.extend(align_interval_with_partition_fallback(
                old_interval,
                new_interval,
                &candidate_map,
                options,
                dp_cell_budget,
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
        }

        if let Some(right_anchor) = window.right_anchor {
            spans.push(if main_anchor_set.contains(&right_anchor) {
                anchor_span(right_anchor)
            } else {
                partition_span(right_anchor)
            });
        }
    }
    refine_masked_matches(&mut spans);

    Ok(Alignment {
        spans,
        main_anchors: plan.main_anchors,
        move_candidates: plan.move_candidates,
    })
}

fn validate_gap_boundaries(side: &str, boundaries: &[usize], block_count: usize) -> Result<()> {
    if boundaries.iter().any(|&boundary| boundary > block_count) {
        return Err(Error::Unresolved(format!(
            "{side} extraction gap boundary exceeds the normalized block count"
        )));
    }
    Ok(())
}

fn validate_uncertain_indices(side: &str, indices: &[usize], block_count: usize) -> Result<()> {
    if indices.iter().any(|&index| index >= block_count) {
        return Err(Error::Unresolved(format!(
            "{side} uncertain block index exceeds the normalized block count"
        )));
    }
    Ok(())
}

fn forced_window_indices(
    main_anchors: &[ExactAnchor],
    old_boundaries: &[usize],
    new_boundaries: &[usize],
    old_indices: &HashMap<BlockId, usize>,
    new_indices: &HashMap<BlockId, usize>,
) -> HashSet<usize> {
    let old_anchor_indices = main_anchors
        .iter()
        .map(|anchor| old_indices[&anchor.old])
        .collect::<Vec<_>>();
    let new_anchor_indices = main_anchors
        .iter()
        .map(|anchor| new_indices[&anchor.new])
        .collect::<Vec<_>>();

    old_boundaries
        .iter()
        .map(|boundary| old_anchor_indices.partition_point(|index| index < boundary))
        .chain(
            new_boundaries
                .iter()
                .map(|boundary| new_anchor_indices.partition_point(|index| index < boundary)),
        )
        .collect()
}

fn uncertain_window_indices(
    main_anchors: &[ExactAnchor],
    old_uncertain_indices: &[usize],
    new_uncertain_indices: &[usize],
    old_indices: &HashMap<BlockId, usize>,
    new_indices: &HashMap<BlockId, usize>,
) -> HashSet<usize> {
    forced_window_indices(
        main_anchors,
        old_uncertain_indices,
        new_uncertain_indices,
        old_indices,
        new_indices,
    )
}

/// Subdivides reading-order-only windows without weakening extraction gaps.
#[allow(clippy::too_many_arguments)]
fn refine_reading_order_windows(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    primary_anchors: &[ExactAnchor],
    windows: Vec<AnchorIntervalWindow>,
    forced_windows: BTreeMap<usize, ForcedWindowCauses>,
    uncertain_old: &HashSet<usize>,
    uncertain_new: &HashSet<usize>,
    old_reading_order_uncertain: &[usize],
    new_reading_order_uncertain: &[usize],
    old_indices: &HashMap<BlockId, usize>,
    new_indices: &HashMap<BlockId, usize>,
) -> Result<(
    Vec<AnchorIntervalWindow>,
    BTreeMap<usize, ForcedWindowCauses>,
)> {
    let primary_old = primary_anchors
        .iter()
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let secondary = exact_anchors(old, new, READING_ORDER_BOUNDARY_MIN_TOKENS)?
        .into_iter()
        .filter(|anchor| {
            !primary_old.contains(&anchor.old)
                && !uncertain_old.contains(&old_indices[&anchor.old])
                && !uncertain_new.contains(&new_indices[&anchor.new])
        })
        .collect::<Vec<_>>();
    let mut refined = Vec::new();
    let mut refined_forced = BTreeMap::new();
    let mut old_reading_order_uncertain = old_reading_order_uncertain.to_vec();
    old_reading_order_uncertain.sort_unstable();
    let mut new_reading_order_uncertain = new_reading_order_uncertain.to_vec();
    new_reading_order_uncertain.sort_unstable();

    for (window_index, window) in windows.into_iter().enumerate() {
        let Some(causes) = forced_windows.get(&window_index).copied() else {
            refined.push(window);
            continue;
        };
        if causes.extraction_gap || !causes.reading_order_unknown {
            refined_forced.insert(refined.len(), causes);
            refined.push(window);
            continue;
        }

        let candidates = secondary
            .iter()
            .copied()
            .filter(|anchor| {
                window.old_range.0 <= old_indices[&anchor.old]
                    && old_indices[&anchor.old] < window.old_range.1
                    && window.new_range.0 <= new_indices[&anchor.new]
                    && new_indices[&anchor.new] < window.new_range.1
            })
            .collect::<Vec<_>>();
        let anchors = select_monotone_anchor_chain(&candidates, old, new)?.main_chain;
        if anchors.is_empty() {
            refined_forced.insert(refined.len(), causes);
            refined.push(window);
            continue;
        }

        let mut old_start = window.old_range.0;
        let mut new_start = window.new_range.0;
        let mut left_anchor = window.left_anchor;
        for right_anchor in anchors.into_iter().map(Some).chain([window.right_anchor]) {
            let old_end = right_anchor
                .map(|anchor| old_indices[&anchor.old])
                .unwrap_or(window.old_range.1);
            let new_end = right_anchor
                .map(|anchor| new_indices[&anchor.new])
                .unwrap_or(window.new_range.1);
            let child = AnchorIntervalWindow {
                old_range: (old_start, old_end),
                new_range: (new_start, new_end),
                left_anchor,
                right_anchor,
            };
            if contains_index(&old_reading_order_uncertain, child.old_range)
                || contains_index(&new_reading_order_uncertain, child.new_range)
            {
                refined_forced.insert(refined.len(), causes);
            }
            refined.push(child);
            if let Some(anchor) = right_anchor {
                old_start = old_end + 1;
                new_start = new_end + 1;
                left_anchor = Some(anchor);
            }
        }
    }

    Ok((refined, refined_forced))
}

fn contains_index(indices: &[usize], range: (usize, usize)) -> bool {
    let first = indices.partition_point(|&index| index < range.0);
    indices.get(first).is_some_and(|&index| index < range.1)
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
    interval_anchors: &[ExactAnchor],
    old_indices: &HashMap<BlockId, usize>,
    new_indices: &HashMap<BlockId, usize>,
) -> Result<Vec<Vec<ExactAnchor>>> {
    let primary_old = primary_anchors
        .iter()
        .chain(interval_anchors)
        .map(|anchor| anchor.old)
        .collect::<HashSet<_>>();
    let old_boundaries = interval_anchors
        .iter()
        .map(|anchor| old_indices[&anchor.old])
        .collect::<Vec<_>>();
    let new_boundaries = interval_anchors
        .iter()
        .map(|anchor| new_indices[&anchor.new])
        .collect::<Vec<_>>();
    let mut candidates = vec![Vec::new(); interval_anchors.len() + 1];
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
    dp_cell_budget: &mut DpCellBudget,
    context: IntervalContext<'_>,
    fallback: PartitionFallback<'_>,
) -> Result<Vec<AlignmentSpan>> {
    let initial = align_interval(old, new, candidates, options, dp_cell_budget, context)?;
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
        let old_anchor =
            relative_anchor_index(fallback.old_indices, anchor.old, fallback.old_offset, "old")?;
        let new_anchor =
            relative_anchor_index(fallback.new_indices, anchor.new, fallback.new_offset, "new")?;
        spans.extend(align_interval_preserving_moves(
            &old[old_start..old_anchor],
            &new[new_start..new_anchor],
            candidates,
            options,
            dp_cell_budget,
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
        dp_cell_budget,
        retry_context,
    )?);
    Ok(spans)
}

fn relative_anchor_index(
    indices: &HashMap<BlockId, usize>,
    block: BlockId,
    offset: usize,
    side: &str,
) -> Result<usize> {
    let absolute = indices.get(&block).copied().ok_or_else(|| {
        Error::Unresolved(format!(
            "partition fallback {side} anchor block {} is missing from the index",
            block.0
        ))
    })?;
    absolute.checked_sub(offset).ok_or_else(|| {
        Error::Unresolved(format!(
            "partition fallback {side} anchor block {} precedes interval offset {offset}",
            block.0
        ))
    })
}

fn align_interval_preserving_moves(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    candidates: &CandidateMap,
    options: AlignmentOptions,
    dp_cell_budget: &mut DpCellBudget,
    context: IntervalContext<'_>,
) -> Result<Vec<AlignmentSpan>> {
    let spans = align_interval(old, new, candidates, options, dp_cell_budget, context)?;
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
        && matches!(
            span.evidence.as_slice(),
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::CandidateSetEmpty
                    | AlignmentEvidence::CandidateScoringRejected
                    | AlignmentEvidence::CandidateCompetition
            ]
        )
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

fn collect_candidates(
    old: &[BlockFeatures],
    new_indices: &HashMap<BlockId, usize>,
    main_anchor_old: &HashSet<BlockId>,
    generator: &dyn CandidateGenerator,
    limit: usize,
    max_visits: usize,
    visit_metrics: &mut AlignmentVisitMetrics,
) -> Result<CandidateMap> {
    // The required sum is the checked total over every eligible old block;
    // anchors and forced extraction-gap queries are excluded.
    // and is unavailable (`None`) whenever any estimate errors or the sum
    // overflows. The attempted charge is frozen at the first budget exceed
    // while the required sum keeps accumulating, so a limit failure still
    // reports the full candidate work the alignment would have needed. The
    // exact/ngram/short-fallback components are `Some` only when every
    // block reported a breakdown and every component sum completed; an
    // unknown generator, estimate error, or overflow leaves all three
    // `None` so partial component state is never reported.
    visit_metrics.candidate_visits_required = None;
    visit_metrics.candidate_visits_required_exact = None;
    visit_metrics.candidate_visits_required_ngram = None;
    visit_metrics.candidate_visits_required_short_fallback = None;
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
            visit_metrics.candidate_visits = visit_metrics
                .candidate_visits
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
    visit_metrics.candidate_visits_required = Some(required_visits);
    if breakdown_complete {
        visit_metrics.candidate_visits_required_exact = Some(required_exact);
        visit_metrics.candidate_visits_required_ngram = Some(required_ngram);
        visit_metrics.candidate_visits_required_short_fallback = Some(required_short_fallback);
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
    dp_cell_budget: &mut DpCellBudget,
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
    if cell_count > dp_cell_budget.remaining {
        return Err(Error::LimitExceeded {
            resource: "alignment DP cells",
            limit: options.max_dp_cells,
        });
    }
    dp_cell_budget.remaining -= cell_count;
    let mut cells = Vec::new();
    cells
        .try_reserve_exact(cell_count)
        .map_err(|_| Error::LimitExceeded {
            resource: "alignment DP cells",
            limit: options.max_dp_cells,
        })?;
    cells.resize(cell_count, Cell::default());
    cells[0].best = 0.0;
    let mut has_candidate_edge = false;
    let mut has_match_proposal = false;

    for old_index in 0..=old.len() {
        for new_index in 0..=new.len() {
            let from = old_index * width + new_index;
            if !has_candidate_edge
                && let Some((old, new)) = old.get(old_index).zip(new.get(new_index))
                && candidates
                    .get(&old.block)
                    .is_some_and(|by_new| by_new.contains_key(&new.block))
            {
                has_candidate_edge = true;
            }
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
                has_match_proposal = true;
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
                has_match_proposal |= propose_group_match(
                    &mut cells,
                    (from, (old_index + 1) * width + new_index + 1),
                    &old[old_index..old_index + 1],
                    &new[new_index..new_index + 1],
                    sources,
                    options,
                    false,
                );
            }
            if context.allow_split_merge {
                for (old_count, new_count) in [(1, 2), (2, 1), (1, 3), (3, 1)] {
                    let old_end = old_index + old_count;
                    let new_end = new_index + new_count;
                    if old_end <= old.len()
                        && new_end <= new.len()
                        && !contains_affected(
                            &old[old_index..old_end],
                            &new[new_index..new_end],
                            context,
                        )
                        && let Some(sources) = group_candidate_sources(
                            &old[old_index..old_end],
                            &new[new_index..new_end],
                            candidates,
                        )
                    {
                        has_match_proposal |= propose_group_match(
                            &mut cells,
                            (from, old_end * width + new_end),
                            &old[old_index..old_end],
                            &new[new_index..new_end],
                            sources,
                            options,
                            !context.bounded_by_anchors,
                        );
                    }
                }
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
        let cause = if !has_candidate_edge {
            AlignmentEvidence::CandidateSetEmpty
        } else if !has_match_proposal {
            AlignmentEvidence::CandidateScoringRejected
        } else {
            AlignmentEvidence::CandidateCompetition
        };
        return Ok(vec![unresolved_span_with_evidence(
            old,
            new,
            vec![AlignmentEvidence::TextSimilarity, cause],
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
) -> bool {
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
        return false;
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
    true
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
    unresolved_span_with_evidence(old, new, vec![evidence])
}

fn unresolved_span_with_evidence(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    evidence: Vec<AlignmentEvidence>,
) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Unresolved,
        old: old.iter().map(|features| features.block).collect(),
        new: new.iter().map(|features| features.block).collect(),
        score: 0.0,
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: AlignmentConfidence::Low,
        evidence,
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

    #[test]
    fn partition_fallback_indices_fail_closed() {
        let indices = HashMap::from([(BlockId(1), 4)]);

        assert_eq!(relative_anchor_index(&indices, BlockId(1), 4, "old"), Ok(0));
        assert!(matches!(
            relative_anchor_index(&indices, BlockId(2), 0, "old"),
            Err(Error::Unresolved(message)) if message.contains("missing from the index")
        ));
        assert!(matches!(
            relative_anchor_index(&indices, BlockId(1), 5, "old"),
            Err(Error::Unresolved(message)) if message.contains("precedes interval offset")
        ));
    }

    fn feature(block: u64, key: u64) -> BlockFeatures {
        feature_with_tokens(block, key, &[key])
    }

    fn feature_with_tokens(block: u64, exact_hash: u64, keys: &[u64]) -> BlockFeatures {
        let tokens = keys
            .iter()
            .map(|key| {
                ComparableToken::Scalar(
                    char::from_u32(0x1000 + *key as u32).expect("fixture key should be valid"),
                )
            })
            .collect::<Vec<_>>();
        BlockFeatures {
            block: BlockId(block),
            exact_hash: ExactHash(exact_hash),
            canonical_tokens: tokens.clone(),
            matching_tokens: tokens,
            ngram_counts: Default::default(),
            ngram_size: 3,
            page_position: None,
            numeric_mask_applied: false,
            has_normalization_issues: false,
        }
    }

    fn feature_with_issue(block: u64, key: u64, has_normalization_issues: bool) -> BlockFeatures {
        let mut f = feature(block, key);
        f.has_normalization_issues = has_normalization_issues;
        f
    }

    fn candidate_map(edges: &[(u64, u64)]) -> CandidateMap {
        let mut candidates = CandidateMap::new();
        for &(old, new) in edges {
            candidates
                .entry(BlockId(old))
                .or_default()
                .insert(BlockId(new), vec![CandidateSource::Exhaustive]);
        }
        candidates
    }

    fn align_unbounded_interval(
        old: &[BlockFeatures],
        new: &[BlockFeatures],
        candidates: &CandidateMap,
    ) -> Vec<AlignmentSpan> {
        let options = AlignmentOptions::default();
        let mut dp_cell_budget = DpCellBudget::new(options.max_dp_cells);
        let move_old = HashSet::new();
        let move_new = HashSet::new();
        align_interval(
            old,
            new,
            candidates,
            options,
            &mut dp_cell_budget,
            IntervalContext {
                allow_split_merge: false,
                bounded_by_anchors: false,
                move_old: &move_old,
                move_new: &move_new,
            },
        )
        .expect("test interval should align")
    }

    #[test]
    fn ambiguity_collapse_reports_an_empty_candidate_set() {
        let old = [feature(1, 1)];
        let new = [feature(101, 2)];

        let spans = align_unbounded_interval(&old, &new, &CandidateMap::new());

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(spans[0].old, [BlockId(1)]);
        assert_eq!(spans[0].new, [BlockId(101)]);
        assert_eq!(
            spans[0].evidence,
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::CandidateSetEmpty,
            ]
        );
    }

    #[test]
    fn ambiguity_collapse_reports_rejected_candidate_scoring() {
        let old = [feature(1, 1)];
        let new = [feature(101, 2)];

        let spans = align_unbounded_interval(&old, &new, &candidate_map(&[(1, 101)]));

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(spans[0].old, [BlockId(1)]);
        assert_eq!(spans[0].new, [BlockId(101)]);
        assert_eq!(
            spans[0].evidence,
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::CandidateScoringRejected,
            ]
        );
    }

    #[test]
    fn ambiguity_collapse_reports_competing_admissible_candidates() {
        let old = [feature(1, 1)];
        let new = [feature(101, 1), feature(102, 1)];

        let spans = align_unbounded_interval(&old, &new, &candidate_map(&[(1, 101), (1, 102)]));

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(spans[0].old, [BlockId(1)]);
        assert_eq!(spans[0].new, [BlockId(101), BlockId(102)]);
        assert_eq!(
            spans[0].evidence,
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::CandidateCompetition,
            ]
        );
    }

    #[test]
    fn candidate_diagnostics_preserve_partition_fallback_and_dp_charging() {
        let old = [feature(1, 1), feature(2, 2)];
        let new = [feature(101, 1), feature(102, 3)];
        let anchor = ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        };
        let old_indices = HashMap::from([(BlockId(1), 0), (BlockId(2), 1)]);
        let new_indices = HashMap::from([(BlockId(101), 0), (BlockId(102), 1)]);
        let options = AlignmentOptions::default();
        let mut dp_cell_budget = DpCellBudget::new(options.max_dp_cells);
        let move_old = HashSet::new();
        let move_new = HashSet::new();

        let spans = align_interval_with_partition_fallback(
            &old,
            &new,
            &CandidateMap::new(),
            options,
            &mut dp_cell_budget,
            IntervalContext {
                allow_split_merge: false,
                bounded_by_anchors: false,
                move_old: &move_old,
                move_new: &move_new,
            },
            PartitionFallback {
                anchors: std::slice::from_ref(&anchor),
                old_offset: 0,
                new_offset: 0,
                old_indices: &old_indices,
                new_indices: &new_indices,
            },
        )
        .expect("partition fallback should remain available");

        assert_eq!(dp_cell_budget.consumed(), 13);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0], partition_span(anchor));
        assert_eq!(spans[1].old, [BlockId(2)]);
        assert_eq!(spans[1].new, [BlockId(102)]);
        assert_eq!(
            spans[1].evidence,
            [
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::CandidateSetEmpty,
            ]
        );
    }

    #[test]
    fn candidate_diagnostics_preserve_move_boundaries_after_collapse() {
        let old = [feature(1, 1), feature(2, 2)];
        let new = [feature(101, 3), feature(102, 2)];
        let move_old = HashSet::from([BlockId(2)]);
        let move_new = HashSet::from([BlockId(102)]);
        let collapsed = vec![unresolved_span_with_evidence(
            &old,
            &new,
            vec![
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::CandidateCompetition,
            ],
        )];

        let spans = preserve_collapsed_moves(
            collapsed,
            &old,
            &new,
            IntervalContext {
                allow_split_merge: false,
                bounded_by_anchors: false,
                move_old: &move_old,
                move_new: &move_new,
            },
        );

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(spans[0].old, [BlockId(1)]);
        assert_eq!(spans[0].new, [BlockId(101)]);
        assert_eq!(spans[1].kind, AlignmentKind::Deletion);
        assert_eq!(spans[1].old, [BlockId(2)]);
        assert_eq!(spans[2].kind, AlignmentKind::Insertion);
        assert_eq!(spans[2].new, [BlockId(102)]);
    }

    #[test]
    fn uncertain_blocks_are_not_anchors_and_preserve_all_forced_causes() {
        let old = vec![feature(1, 1), feature(2, 2), feature(3, 3)];
        let new = vec![feature(101, 1), feature(102, 2), feature(103, 3)];
        let options = AlignmentOptions {
            anchor_min_tokens: 1,
            ..AlignmentOptions::default()
        };

        let plan = plan_ordered_gaps(&old, &new, options, &[1], &[], &[], &[], &[1], &[])
            .expect("uncertainty should produce a forced anchor interval");

        assert!(
            plan.all_anchors
                .iter()
                .all(|anchor| anchor.old != BlockId(2) && anchor.new != BlockId(102))
        );
        let causes = plan
            .forced_windows
            .values()
            .next()
            .expect("the uncertain block should force one interval");
        assert_eq!(
            causes.evidence(),
            [
                AlignmentEvidence::ExtractionGap,
                AlignmentEvidence::ReadingOrderUnknown,
            ]
        );
    }

    #[test]
    fn secondary_anchors_localize_reading_order_uncertainty() {
        let old = vec![
            feature_with_tokens(1, 1, &[1, 2, 3, 4, 5]),
            feature_with_tokens(2, 10, &[10, 11, 12, 13]),
            feature(3, 20),
            feature_with_tokens(4, 30, &[30, 31, 32, 33]),
            feature_with_tokens(5, 40, &[40, 41, 42, 43, 44, 45, 46, 47, 48, 49]),
            feature_with_tokens(6, 50, &[50, 51, 52, 53]),
            feature_with_tokens(7, 60, &[60, 61, 62, 63, 64]),
        ];
        let new = vec![
            feature_with_tokens(101, 1, &[1, 2, 3, 4, 5]),
            feature_with_tokens(102, 10, &[10, 11, 12, 13]),
            feature(103, 20),
            feature_with_tokens(104, 30, &[30, 31, 32, 33]),
            feature_with_tokens(105, 41, &[40, 41, 42, 99, 44, 45, 46, 47, 48, 49]),
            feature_with_tokens(106, 50, &[50, 51, 52, 53]),
            feature_with_tokens(107, 60, &[60, 61, 62, 63, 64]),
        ];
        let options = AlignmentOptions {
            anchor_min_tokens: 5,
            ..AlignmentOptions::default()
        };
        let plan = plan_ordered_gaps(&old, &new, options, &[], &[], &[], &[], &[2], &[2])
            .expect("reading-order uncertainty should be localized");

        assert_eq!(
            plan.main_anchors,
            [
                ExactAnchor {
                    old: BlockId(1),
                    new: BlockId(101),
                },
                ExactAnchor {
                    old: BlockId(7),
                    new: BlockId(107),
                },
            ]
        );
        assert_eq!(plan.forced_windows.len(), 1);
        let forced_window = &plan.windows[*plan
            .forced_windows
            .keys()
            .next()
            .expect("one child interval should remain forced")];
        assert_eq!(forced_window.old_range, (2, 3));
        assert_eq!(forced_window.new_range, (2, 3));

        let alignment = align_ordered_with_metrics_and_gap_plan(
            &old,
            &new,
            &ReplacementGenerator,
            options,
            plan,
        )
        .result
        .expect("stable child intervals should remain alignable");
        let unresolved = alignment
            .spans
            .iter()
            .filter(|span| span.kind == AlignmentKind::Unresolved)
            .collect::<Vec<_>>();
        assert_eq!(unresolved.len(), 1, "spans: {:?}", alignment.spans);
        assert_eq!(unresolved[0].old, [BlockId(3)]);
        assert_eq!(unresolved[0].new, [BlockId(103)]);
        assert!(alignment.spans.iter().any(|span| {
            span.kind == AlignmentKind::Match
                && span.old == [BlockId(5)]
                && span.new == [BlockId(105)]
                && !span.evidence.contains(&AlignmentEvidence::ExactCanonical)
        }));
        let anchors = alignment
            .spans
            .iter()
            .filter(|span| span.evidence.contains(&AlignmentEvidence::Anchor))
            .map(|span| ExactAnchor {
                old: span.old[0],
                new: span.new[0],
            })
            .collect::<Vec<_>>();
        assert_eq!(anchors, alignment.main_anchors);
    }

    #[test]
    fn uncertain_exact_text_is_not_a_secondary_anchor() {
        let old = vec![
            feature_with_tokens(1, 10, &[10, 11, 12, 13]),
            feature_with_tokens(2, 20, &[20, 21, 22, 23]),
            feature_with_tokens(3, 30, &[30, 31, 32, 33]),
        ];
        let new = vec![
            feature_with_tokens(101, 10, &[10, 11, 12, 13]),
            feature_with_tokens(102, 20, &[20, 21, 22, 23]),
            feature_with_tokens(103, 30, &[30, 31, 32, 33]),
        ];
        let options = AlignmentOptions {
            anchor_min_tokens: 5,
            ..AlignmentOptions::default()
        };

        let plan = plan_ordered_gaps(&old, &new, options, &[], &[], &[], &[], &[1], &[1])
            .expect("secondary anchors should exclude uncertain blocks");

        assert!(plan.windows.iter().all(|window| {
            window
                .right_anchor
                .is_none_or(|anchor| anchor.old != BlockId(2) && anchor.new != BlockId(102))
        }));
        let forced_window = &plan.windows[*plan
            .forced_windows
            .keys()
            .next()
            .expect("the uncertain exact pair should remain forced")];
        assert_eq!(forced_window.old_range, (1, 2));
        assert_eq!(forced_window.new_range, (1, 2));
    }

    #[test]
    fn unique_one_token_matches_do_not_split_a_forced_window() {
        let old = vec![feature(1, 10), feature(2, 20), feature(3, 30)];
        let new = vec![feature(101, 10), feature(102, 20), feature(103, 30)];
        let options = AlignmentOptions {
            anchor_min_tokens: 2,
            ..AlignmentOptions::default()
        };
        let plan = plan_ordered_gaps(&old, &new, options, &[], &[], &[], &[], &[1], &[1])
            .expect("short exact coincidences must not become direct boundaries");

        assert_eq!(plan.windows.len(), 1);
        assert_eq!(plan.windows[0].old_range, (0, old.len()));
        assert_eq!(plan.windows[0].new_range, (0, new.len()));
        assert_eq!(plan.forced_windows.keys().copied().collect::<Vec<_>>(), [0]);

        let alignment = align_ordered_with_metrics_and_gap_plan(
            &old,
            &new,
            &FixedVisitsGenerator::new(0),
            options,
            plan,
        )
        .result
        .expect("the unpartitioned reading-order window should remain unresolved");
        assert_eq!(alignment.spans.len(), 1);
        assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(alignment.spans[0].old, [BlockId(1), BlockId(2), BlockId(3)]);
        assert_eq!(
            alignment.spans[0].new,
            [BlockId(101), BlockId(102), BlockId(103)]
        );
    }

    #[test]
    fn extraction_gap_windows_are_not_subdivided_by_secondary_anchors() {
        let old = vec![
            feature_with_tokens(1, 10, &[10, 11, 12, 13]),
            feature(2, 20),
            feature_with_tokens(3, 30, &[30, 31, 32, 33]),
        ];
        let new = vec![
            feature_with_tokens(101, 10, &[10, 11, 12, 13]),
            feature(102, 20),
            feature_with_tokens(103, 30, &[30, 31, 32, 33]),
        ];
        let options = AlignmentOptions {
            anchor_min_tokens: 5,
            ..AlignmentOptions::default()
        };
        let plan = plan_ordered_gaps(&old, &new, options, &[], &[], &[1], &[1], &[], &[])
            .expect("extraction uncertainty should remain conservative");

        assert_eq!(plan.windows.len(), 1);
        assert_eq!(plan.windows[0].old_range, (0, old.len()));
        assert_eq!(plan.windows[0].new_range, (0, new.len()));
        assert_eq!(
            plan.forced_windows[&0].evidence(),
            [AlignmentEvidence::ExtractionGap]
        );

        let alignment = align_ordered_with_metrics_and_gap_plan(
            &old,
            &new,
            &FixedVisitsGenerator::new(0),
            options,
            plan,
        )
        .result
        .expect("forced extraction interval should produce an unresolved span");
        assert_eq!(alignment.spans.len(), 1);
        assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
        assert_eq!(alignment.spans[0].old, [BlockId(1), BlockId(2), BlockId(3)]);
        assert_eq!(
            alignment.spans[0].new,
            [BlockId(101), BlockId(102), BlockId(103)]
        );
    }

    struct FixedVisitsGenerator {
        visits: usize,
        estimated: RefCell<Vec<BlockId>>,
        generated: RefCell<Vec<BlockId>>,
    }

    struct ReplacementGenerator;

    impl CandidateGenerator for ReplacementGenerator {
        fn estimated_visits(&self, _old: &BlockFeatures, _limit: usize) -> Result<usize> {
            Ok(1)
        }

        fn candidates(&self, old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
            Ok((old.block == BlockId(5))
                .then_some(Candidate {
                    block: BlockId(105),
                    sources: vec![CandidateSource::Exhaustive],
                    coarse_score: 0.8,
                })
                .into_iter()
                .collect())
        }
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
        assert_eq!(attempt.visit_metrics.dp_cells, 16);
        assert_eq!(
            attempt.visit_metrics.max_dp_cells,
            AlignmentOptions::default().max_dp_cells
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
        assert_eq!(attempt.visit_metrics.dp_cells, 0);
        assert_eq!(
            attempt.visit_metrics.max_dp_cells,
            AlignmentOptions::default().max_dp_cells
        );
    }

    #[test]
    fn anchor_only_alignment_consumes_no_dp_cells() {
        let old = vec![feature(1, 10)];
        let new = vec![feature(101, 10)];
        let options = AlignmentOptions {
            anchor_min_tokens: 1,
            ..AlignmentOptions::default()
        };

        let attempt =
            align_ordered_with_metrics(&old, &new, &FixedVisitsGenerator::new(0), options);

        assert!(attempt.result.is_ok());
        assert_eq!(attempt.visit_metrics.dp_cells, 0);
        assert_eq!(attempt.visit_metrics.max_dp_cells, options.max_dp_cells);
    }

    #[test]
    fn dp_limit_failure_reports_cells_consumed_by_completed_intervals() {
        let old = vec![
            feature(1, 1),
            feature_with_tokens(2, 50, &[50, 51, 52, 53]),
            feature(3, 3),
        ];
        let new = vec![
            feature(101, 101),
            feature_with_tokens(102, 50, &[50, 51, 52, 53]),
            feature(103, 103),
        ];
        let options = AlignmentOptions {
            anchor_min_tokens: 4,
            max_dp_cells: 4,
            ..AlignmentOptions::default()
        };

        let attempt =
            align_ordered_with_metrics(&old, &new, &FixedVisitsGenerator::new(0), options);

        assert!(matches!(
            attempt.result,
            Err(Error::LimitExceeded {
                resource: "alignment DP cells",
                limit: 4,
            })
        ));
        assert_eq!(attempt.visit_metrics.dp_cells, 4);
        assert_eq!(attempt.visit_metrics.max_dp_cells, 4);
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
