use std::cell::Cell;

use pdfdelta_core::{
    Error, Result,
    alignment::{
        Alignment, AlignmentEvidence, AlignmentKind, AlignmentOptions, BlockFeatures,
        BlockSeparator, Candidate, CandidateGenerator, CandidateSource, ExactHash,
        InvertedIndexCandidateGenerator, align_ordered, build_block_features,
    },
    layout::BlockId,
    normalize::{BlockText, ComparableToken, MappedText},
};

const OPENING: &str = "Opening anchor paragraph";
const CLOSING: &str = "Closing anchor paragraph";

#[test]
fn aligns_ordered_exact_blocks_as_identity() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "Stable middle paragraph"),
            block_text(3, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "Stable middle paragraph"),
            block_text(103, CLOSING),
        ],
    );

    assert_eq!(alignment.spans.len(), 3);
    assert!(
        alignment
            .spans
            .iter()
            .all(|span| span.kind == AlignmentKind::Match && span.score == 1.0)
    );
    assert_eq!(alignment.main_anchors.len(), 3);
}

#[test]
fn classifies_an_insertion_between_exact_anchors() {
    let alignment = align(
        vec![block_text(1, OPENING), block_text(2, CLOSING)],
        vec![
            block_text(101, OPENING),
            block_text(102, "A newly inserted paragraph"),
            block_text(103, CLOSING),
        ],
    );

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Insertion);
    assert!(alignment.spans[1].old.is_empty());
    assert_eq!(alignment.spans[1].new, [BlockId(102)]);
}

#[test]
fn classifies_a_deletion_between_exact_anchors() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "A paragraph that was removed"),
            block_text(3, CLOSING),
        ],
        vec![block_text(101, OPENING), block_text(102, CLOSING)],
    );

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Deletion);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert!(alignment.spans[1].new.is_empty());
}

#[test]
fn aligns_an_english_block_split_as_one_to_two() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "project log"),
            block_text(3, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "project"),
            block_text(103, "log"),
            block_text(104, CLOSING),
        ],
    );

    let split = &alignment.spans[1];
    assert_eq!(split.kind, AlignmentKind::Match);
    assert_eq!(split.old, [BlockId(2)]);
    assert_eq!(split.new, [BlockId(102), BlockId(103)]);
    assert_eq!(split.score, 1.0);
    assert_eq!(split.new_separator, Some(BlockSeparator::Space));
    assert!(split.evidence.contains(&AlignmentEvidence::SplitMerge));
}

#[test]
fn aligns_a_cjk_block_merge_without_inserting_a_space() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "設定を変更して"),
            block_text(3, "保存する。"),
            block_text(4, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "設定を変更して保存する。"),
            block_text(103, CLOSING),
        ],
    );

    let merge = &alignment.spans[1];
    assert_eq!(merge.kind, AlignmentKind::Match);
    assert_eq!(merge.old, [BlockId(2), BlockId(3)]);
    assert_eq!(merge.new, [BlockId(102)]);
    assert_eq!(merge.score, 1.0);
    assert_eq!(merge.old_separator, Some(BlockSeparator::Concatenate));
}

#[test]
fn leaves_duplicate_tie_regions_unresolved() {
    let repeated = "Repeated ambiguous paragraph";
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, repeated),
            block_text(3, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, repeated),
            block_text(103, repeated),
            block_text(104, CLOSING),
        ],
    );

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert_eq!(alignment.spans[1].new, [BlockId(102), BlockId(103)]);
}

#[test]
fn accepts_numeric_mask_matches_only_with_anchor_context() {
    let old_count = block_text_with_matching(
        2,
        "The archive contains 10 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let new_count = block_text_with_matching(
        102,
        "The archive contains 20 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let alignment = align(
        vec![block_text(1, OPENING), old_count, block_text(3, CLOSING)],
        vec![
            block_text(101, OPENING),
            new_count,
            block_text(103, CLOSING),
        ],
    );

    let count = &alignment.spans[1];
    assert_eq!(count.kind, AlignmentKind::Match);
    assert!(count.evidence.contains(&AlignmentEvidence::NumericMask));
    assert!(count.evidence.contains(&AlignmentEvidence::AnchorInterval));

    let unsupported = align(
        vec![block_text_with_matching(
            1,
            "The archive contains 10 files.",
            "The archive contains <NUM> files.",
            true,
        )],
        vec![block_text_with_matching(
            101,
            "The archive contains 20 files.",
            "The archive contains <NUM> files.",
            true,
        )],
    );
    assert_eq!(unsupported.spans[0].kind, AlignmentKind::Unresolved);
}

#[test]
fn masked_matches_cannot_confirm_each_other_as_neighbors() {
    let alignment = align(
        vec![
            block_text(1, "A"),
            block_text_with_matching(
                2,
                "The archive contains 10 files.",
                "The archive contains <NUM> files.",
                true,
            ),
            block_text_with_matching(
                3,
                "The backup contains 30 images.",
                "The backup contains <NUM> images.",
                true,
            ),
            block_text(4, "B"),
        ],
        vec![
            block_text(101, "A"),
            block_text_with_matching(
                102,
                "The archive contains 20 files.",
                "The archive contains <NUM> files.",
                true,
            ),
            block_text_with_matching(
                103,
                "The backup contains 40 images.",
                "The backup contains <NUM> images.",
                true,
            ),
            block_text(104, "B"),
        ],
    );

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
    assert_eq!(
        alignment.spans[0].old,
        [BlockId(1), BlockId(2), BlockId(3), BlockId(4)]
    );
    assert_eq!(
        alignment.spans[0].new,
        [BlockId(101), BlockId(102), BlockId(103), BlockId(104)]
    );
}

#[test]
fn rejected_masked_paths_cannot_displace_a_crossing_fuzzy_match() {
    let alignment = align(
        vec![
            block_text(1, "Project instructions are provided here."),
            block_text_with_matching(
                2,
                "The archive contains 10 files.",
                "The archive contains <NUM> files.",
                true,
            ),
        ],
        vec![
            block_text_with_matching(
                101,
                "The archive contains 20 files.",
                "The archive contains <NUM> files.",
                true,
            ),
            block_text(102, "Project guidance is provided here."),
        ],
    );

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[0].old, [BlockId(1), BlockId(2)]);
    assert_eq!(alignment.spans[0].new, [BlockId(101), BlockId(102)]);
}

#[test]
fn does_not_split_or_merge_outside_a_bounded_anchor_interval() {
    let alignment = align(
        vec![block_text(1, "project log")],
        vec![block_text(101, "project"), block_text(102, "log")],
    );

    assert!(!alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old.len() == 1 && span.new.len() == 2
    }));
}

#[test]
fn preserves_crossing_exact_anchors_as_move_candidates() {
    let first = "First unique anchor paragraph";
    let second = "Second unique anchor paragraph";
    let alignment = align(
        vec![block_text(1, first), block_text(2, second)],
        vec![block_text(101, second), block_text(102, first)],
    );

    assert_eq!(alignment.main_anchors.len(), 1);
    assert_eq!(alignment.move_candidates.len(), 1);
    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Unresolved
            && span.evidence.contains(&AlignmentEvidence::MoveCandidate)
    }));
}

#[test]
fn confines_an_off_lis_anchor_to_move_candidate_spans() {
    let moved = "Moved unique anchor paragraph";
    let boundary = "Stable boundary anchor paragraph";
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, moved),
            block_text(3, "Alpha stays"),
            block_text(4, "Beta stays"),
            block_text(5, "Gamma stays"),
            block_text(6, boundary),
            block_text(7, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(103, "Alpha stays"),
            block_text(104, "Beta stays"),
            block_text(105, "Gamma stays"),
            block_text(106, boundary),
            block_text(107, CLOSING),
            block_text(102, moved),
        ],
    );

    assert_eq!(
        alignment.move_candidates,
        [pdfdelta_core::alignment::ExactAnchor {
            old: BlockId(2),
            new: BlockId(102),
        }]
    );
    for (old, new) in [(3, 103), (4, 104), (5, 105)] {
        assert!(alignment.spans.iter().any(|span| {
            span.kind == AlignmentKind::Match
                && span.old == [BlockId(old)]
                && span.new == [BlockId(new)]
        }));
    }
    let unresolved = alignment
        .spans
        .iter()
        .filter(|span| span.kind == AlignmentKind::Unresolved)
        .collect::<Vec<_>>();
    assert_eq!(unresolved.len(), 2, "{:#?}", alignment.spans);
    assert_eq!(
        unresolved
            .iter()
            .flat_map(|span| span.old.iter().copied())
            .collect::<Vec<_>>(),
        [BlockId(2)]
    );
    assert_eq!(
        unresolved
            .iter()
            .flat_map(|span| span.new.iter().copied())
            .collect::<Vec<_>>(),
        [BlockId(102)]
    );
    assert!(
        unresolved
            .iter()
            .all(|span| { span.evidence.contains(&AlignmentEvidence::MoveCandidate) })
    );
}

#[test]
fn confines_normalization_issues_to_the_affected_blocks() {
    let old_text = [
        block_text(1, OPENING),
        block_text(2, "Alpha stays"),
        block_text(3, "Ambiguous wrapped paragraph"),
        block_text(4, "Beta stays"),
        block_text(5, CLOSING),
    ];
    let new_text = [
        block_text(101, OPENING),
        block_text(102, "Alpha stays"),
        block_text(103, "Ambiguous wrapped paragraph"),
        block_text(104, "Beta stays"),
        block_text(105, CLOSING),
    ];
    let mut old = build_block_features(&old_text, 3).expect("old features should build");
    let mut new = build_block_features(&new_text, 3).expect("new features should build");
    old[2].has_normalization_issues = true;
    new[2].has_normalization_issues = true;
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    for (old, new) in [(2, 102), (4, 104)] {
        assert!(alignment.spans.iter().any(|span| {
            span.kind == AlignmentKind::Match
                && span.old == [BlockId(old)]
                && span.new == [BlockId(new)]
        }));
    }
    let unresolved = alignment
        .spans
        .iter()
        .filter(|span| span.kind == AlignmentKind::Unresolved)
        .collect::<Vec<_>>();
    assert_eq!(unresolved.len(), 1, "{:#?}", alignment.spans);
    assert_eq!(unresolved[0].old, [BlockId(3)]);
    assert_eq!(unresolved[0].new, [BlockId(103)]);
    assert!(
        unresolved[0]
            .evidence
            .contains(&AlignmentEvidence::NormalizationIssue)
    );
}

#[test]
fn ignores_misleading_candidate_scores_when_text_does_not_match() {
    let old = build_block_features(&[block_text(1, "Completely different old paragraph")], 3)
        .expect("old features should build");
    let new = build_block_features(
        &[block_text(101, "Nothing in this new paragraph agrees")],
        3,
    )
    .expect("new features should build");
    let generator = MisleadingGenerator {
        candidate: BlockId(101),
    };

    let alignment = align_ordered(&old, &new, &generator, options())
        .expect("alignment should remain conservative");

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
}

#[test]
fn produces_the_same_result_when_candidate_order_changes() {
    let old = build_block_features(&[block_text(1, "alpha beta gamma delta")], 3)
        .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(101, "alpha zeta gamma delta"),
            block_text(102, "alpha beta gamma delta"),
        ],
        3,
    )
    .expect("new features should build");
    let forward = OrderedGenerator {
        candidates: vec![BlockId(101), BlockId(102)],
    };
    let reverse = OrderedGenerator {
        candidates: vec![BlockId(102), BlockId(101)],
    };

    let mut no_anchor_options = options();
    no_anchor_options.anchor_min_tokens = 100;
    let first = align_ordered(&old, &new, &forward, no_anchor_options)
        .expect("forward alignment should work");
    let second = align_ordered(&old, &new, &reverse, no_anchor_options)
        .expect("reverse alignment should work");

    assert_eq!(first, second);
}

#[test]
fn bounds_aggregate_dp_cells_across_anchor_intervals() {
    let old = build_block_features(
        &[
            block_text(1, OPENING),
            block_text(2, "Old interval one"),
            block_text(3, CLOSING),
            block_text(4, "Old interval two"),
            block_text(5, "Final anchor paragraph"),
        ],
        3,
    )
    .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(101, OPENING),
            block_text(102, "New interval one"),
            block_text(103, CLOSING),
            block_text(104, "New interval two"),
            block_text(105, "Final anchor paragraph"),
        ],
        3,
    )
    .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    let mut at_limit = options();
    at_limit.max_dp_cells = 8;

    align_ordered(&old, &new, &generator, at_limit)
        .expect("two four-cell anchor intervals should fit exactly");

    let mut over_limit = at_limit;
    over_limit.max_dp_cells = 7;
    assert!(matches!(
        align_ordered(&old, &new, &generator, over_limit),
        Err(Error::LimitExceeded {
            resource: "alignment DP cells",
            limit: 7,
        })
    ));

    let mut zero_limit = at_limit;
    zero_limit.max_dp_cells = 0;
    assert!(matches!(
        align_ordered(&old, &new, &generator, zero_limit),
        Err(Error::InvalidConfiguration(message)) if message.contains("max_dp_cells")
    ));
}

#[test]
fn bounds_aggregate_candidate_visits_before_generation() {
    let old = build_block_features(&[block_text(1, "id"), block_text(2, "id")], 3)
        .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(101, "id"),
            block_text(102, "id"),
            block_text(103, "id"),
        ],
        3,
    )
    .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    let mut at_limit = options();
    at_limit.max_candidate_visits = 30;

    align_ordered(&old, &new, &generator, at_limit)
        .expect("two fifteen-visit short-block queries should fit exactly");

    let mut over_limit = at_limit;
    over_limit.max_candidate_visits = 29;
    assert!(matches!(
        align_ordered(&old, &new, &generator, over_limit),
        Err(Error::LimitExceeded {
            resource: "alignment candidate visits",
            limit: 29,
        })
    ));

    let tracking = TrackingGenerator {
        visits: 15,
        calls: Cell::new(0),
    };
    assert!(matches!(
        align_ordered(&old, &new, &tracking, over_limit),
        Err(Error::LimitExceeded {
            resource: "alignment candidate visits",
            limit: 29,
        })
    ));
    assert_eq!(tracking.calls.get(), 0);

    let mut zero_limit = at_limit;
    zero_limit.max_candidate_visits = 0;
    assert!(matches!(
        align_ordered(&old, &new, &generator, zero_limit),
        Err(Error::InvalidConfiguration(message)) if message.contains("max_candidate_visits")
    ));
}

#[test]
fn chains_many_unique_anchors_in_n_log_n_time() {
    const ANCHOR_COUNT: usize = 30_000;
    let old = (0..ANCHOR_COUNT)
        .map(|index| anchor_feature(index as u64 + 1, index))
        .collect::<Vec<_>>();
    let identity = (0..ANCHOR_COUNT)
        .map(|index| anchor_feature(index as u64 + 100_001, index))
        .collect::<Vec<_>>();
    let reversed = (0..ANCHOR_COUNT)
        .map(|index| anchor_feature(index as u64 + 200_001, ANCHOR_COUNT - index - 1))
        .collect::<Vec<_>>();
    let generator = EmptyGenerator;
    let options = AlignmentOptions {
        anchor_min_tokens: 1,
        ..AlignmentOptions::default()
    };

    let identity =
        align_ordered(&old, &identity, &generator, options).expect("identity anchors should align");
    assert_eq!(identity.main_anchors.len(), ANCHOR_COUNT);
    assert!(identity.move_candidates.is_empty());

    let reversed = align_ordered(&old, &reversed, &generator, options)
        .expect("reversed anchors should classify");
    assert_eq!(reversed.main_anchors.len(), 1);
    assert_eq!(reversed.main_anchors[0].old, BlockId(1));
    assert_eq!(reversed.move_candidates.len(), ANCHOR_COUNT - 1);
}

fn align(old: Vec<BlockText>, new: Vec<BlockText>) -> Alignment {
    let old = build_block_features(&old, 3).expect("old features should build");
    let new = build_block_features(&new, 3).expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    align_ordered(&old, &new, &generator, options()).expect("alignment should succeed")
}

fn options() -> AlignmentOptions {
    AlignmentOptions {
        anchor_min_tokens: 12,
        ..AlignmentOptions::default()
    }
}

struct MisleadingGenerator {
    candidate: BlockId,
}

impl CandidateGenerator for MisleadingGenerator {
    fn estimated_visits(&self, _old: &BlockFeatures, limit: usize) -> Result<usize> {
        Ok(usize::from(limit > 0))
    }

    fn candidates(&self, _old: &BlockFeatures, limit: usize) -> Result<Vec<Candidate>> {
        Ok((limit > 0)
            .then(|| Candidate {
                block: self.candidate,
                sources: vec![CandidateSource::ShortBlockFallback],
                coarse_score: 1.0,
            })
            .into_iter()
            .collect())
    }
}

struct OrderedGenerator {
    candidates: Vec<BlockId>,
}

struct TrackingGenerator {
    visits: usize,
    calls: Cell<usize>,
}

impl CandidateGenerator for TrackingGenerator {
    fn estimated_visits(&self, _old: &BlockFeatures, _limit: usize) -> Result<usize> {
        Ok(self.visits)
    }

    fn candidates(&self, _old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
        self.calls.set(self.calls.get() + 1);
        Ok(Vec::new())
    }
}

struct EmptyGenerator;

impl CandidateGenerator for EmptyGenerator {
    fn estimated_visits(&self, _old: &BlockFeatures, _limit: usize) -> Result<usize> {
        Ok(0)
    }

    fn candidates(&self, _old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
        Ok(Vec::new())
    }
}

impl CandidateGenerator for OrderedGenerator {
    fn estimated_visits(&self, _old: &BlockFeatures, limit: usize) -> Result<usize> {
        Ok(self.candidates.len().min(limit))
    }

    fn candidates(&self, _old: &BlockFeatures, limit: usize) -> Result<Vec<Candidate>> {
        Ok(self
            .candidates
            .iter()
            .take(limit)
            .map(|block| Candidate {
                block: *block,
                sources: vec![CandidateSource::Exhaustive],
                coarse_score: 0.5,
            })
            .collect())
    }
}

fn block_text(id: u64, text: &str) -> BlockText {
    block_text_with_matching(id, text, text, false)
}

fn anchor_feature(block: u64, key: usize) -> BlockFeatures {
    let scalar = char::from_u32(0x1000 + key as u32).expect("fixture anchor key should be valid");
    let tokens = vec![ComparableToken::Scalar(scalar)];
    BlockFeatures {
        block: BlockId(block),
        exact_hash: ExactHash(key as u64),
        canonical_tokens: tokens.clone(),
        matching_tokens: tokens,
        ngrams: Default::default(),
        ngram_size: 3,
        numeric_mask_applied: false,
        has_normalization_issues: false,
    }
}

fn block_text_with_matching(
    id: u64,
    canonical: &str,
    matching: &str,
    numeric_mask_applied: bool,
) -> BlockText {
    BlockText {
        block: BlockId(id),
        raw: mapped_text(canonical),
        canonical: mapped_text(canonical),
        matching: matching.to_owned(),
        matching_tokens: matching.chars().map(ComparableToken::Scalar).collect(),
        numeric_mask_applied,
        normalization_events: vec![],
        issues: vec![],
    }
}

fn mapped_text(text: &str) -> MappedText {
    MappedText {
        text: text.to_owned(),
        source_map: vec![],
        unmapped: vec![],
    }
}
