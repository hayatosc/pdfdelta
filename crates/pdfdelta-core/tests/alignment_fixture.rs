use pdfdelta_core::{
    Result,
    alignment::{
        Alignment, AlignmentEvidence, AlignmentKind, AlignmentOptions, BlockFeatures, Candidate,
        CandidateGenerator, CandidateSource, InvertedIndexCandidateGenerator, align_ordered,
        build_block_features,
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
            block_text(2, "adult dose"),
            block_text(3, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "adult"),
            block_text(103, "dose"),
            block_text(104, CLOSING),
        ],
    );

    let split = &alignment.spans[1];
    assert_eq!(split.kind, AlignmentKind::Match);
    assert_eq!(split.old, [BlockId(2)]);
    assert_eq!(split.new, [BlockId(102), BlockId(103)]);
    assert_eq!(split.score, 1.0);
    assert!(split.evidence.contains(&AlignmentEvidence::SplitMerge));
}

#[test]
fn aligns_a_cjk_block_merge_without_inserting_a_space() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "通常、成人には"),
            block_text(3, "投与する。"),
            block_text(4, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "通常、成人には投与する。"),
            block_text(103, CLOSING),
        ],
    );

    let merge = &alignment.spans[1];
    assert_eq!(merge.kind, AlignmentKind::Match);
    assert_eq!(merge.old, [BlockId(2), BlockId(3)]);
    assert_eq!(merge.new, [BlockId(102)]);
    assert_eq!(merge.score, 1.0);
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
    let old_dose = block_text_with_matching(
        2,
        "The recommended dose is 10 mg daily.",
        "The recommended dose is <NUM> mg daily.",
        true,
    );
    let new_dose = block_text_with_matching(
        102,
        "The recommended dose is 20 mg daily.",
        "The recommended dose is <NUM> mg daily.",
        true,
    );
    let alignment = align(
        vec![block_text(1, OPENING), old_dose, block_text(3, CLOSING)],
        vec![block_text(101, OPENING), new_dose, block_text(103, CLOSING)],
    );

    let dose = &alignment.spans[1];
    assert_eq!(dose.kind, AlignmentKind::Match);
    assert!(dose.evidence.contains(&AlignmentEvidence::NumericMask));
    assert!(dose.evidence.contains(&AlignmentEvidence::AnchorInterval));

    let unsupported = align(
        vec![block_text_with_matching(
            1,
            "The recommended dose is 10 mg daily.",
            "The recommended dose is <NUM> mg daily.",
            true,
        )],
        vec![block_text_with_matching(
            101,
            "The recommended dose is 20 mg daily.",
            "The recommended dose is <NUM> mg daily.",
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
                "The recommended dose is 10 mg daily.",
                "The recommended dose is <NUM> mg daily.",
                true,
            ),
            block_text_with_matching(
                3,
                "The infusion rate is 30 ml hourly.",
                "The infusion rate is <NUM> ml hourly.",
                true,
            ),
            block_text(4, "B"),
        ],
        vec![
            block_text(101, "A"),
            block_text_with_matching(
                102,
                "The recommended dose is 20 mg daily.",
                "The recommended dose is <NUM> mg daily.",
                true,
            ),
            block_text_with_matching(
                103,
                "The infusion rate is 40 ml hourly.",
                "The infusion rate is <NUM> ml hourly.",
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
            block_text(1, "Patient instructions are provided here."),
            block_text_with_matching(
                2,
                "The recommended dose is 10 mg daily.",
                "The recommended dose is <NUM> mg daily.",
                true,
            ),
        ],
        vec![
            block_text_with_matching(
                101,
                "The recommended dose is 20 mg daily.",
                "The recommended dose is <NUM> mg daily.",
                true,
            ),
            block_text(102, "Patient guidance is provided here."),
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
        vec![block_text(1, "adult dose")],
        vec![block_text(101, "adult"), block_text(102, "dose")],
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

impl CandidateGenerator for OrderedGenerator {
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
