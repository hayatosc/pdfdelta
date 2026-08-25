use std::cell::RefCell;

use pdfdelta_core::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentOptions,
        AlignmentSpan, BlockFeatures, BlockSeparator, Candidate, CandidateGenerator,
        CandidateSource, ExactAnchor, ExactHash, InvertedIndexCandidateGenerator, align_ordered,
        build_block_features, exact_anchors, partition_anchor_windows,
        select_monotone_anchor_chain,
    },
    diff::{ChangeKind, Confidence, DiffOptions, compare_aligned},
    layout::BlockId,
    model::FontProgramHash,
    normalize::{
        BlockText, ComparableToken, MappedText, NormalizationIssue, NormalizationIssueKind,
        ScalarRange, TextSource, UnmappedToken,
    },
    report::{ExtractionStatus, summarize},
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
    assert_anchor_evidence_matches_main_anchors(&alignment);
}

#[test]
fn aligns_identical_repeated_short_features_as_identity() {
    let features = build_block_features(
        &[
            block_text(1, "same"),
            block_text(2, "same"),
            block_text(3, "same"),
        ],
        3,
    )
    .expect("features should build");

    let alignment = align_ordered(&features, &features, &EmptyGenerator, options())
        .expect("identical features should align");

    assert!(alignment.main_anchors.is_empty());
    assert!(alignment.move_candidates.is_empty());
    assert_eq!(alignment.spans.len(), features.len());
    for (span, features) in alignment.spans.iter().zip(&features) {
        assert_eq!(span.kind, AlignmentKind::Match);
        assert_eq!(span.old, [features.block]);
        assert_eq!(span.new, [features.block]);
        assert_eq!(span.score, 1.0);
        assert_eq!(span.confidence, AlignmentConfidence::High);
        assert_eq!(span.evidence, [AlignmentEvidence::ExactCanonical]);
        assert_eq!(span.old_separator, None);
        assert_eq!(span.new_separator, None);
    }
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
fn aligns_an_exact_english_block_split_as_one_to_three() {
    let old = vec![
        block_text(1, OPENING),
        block_text(2, "project release log"),
        block_text(3, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, "project"),
        block_text(103, "release"),
        block_text(104, "log"),
        block_text(105, CLOSING),
    ];
    let alignment = align(old.clone(), new.clone());

    let split = &alignment.spans[1];
    assert_eq!(split.kind, AlignmentKind::Match);
    assert_eq!(split.old, [BlockId(2)]);
    assert_eq!(split.new, [BlockId(102), BlockId(103), BlockId(104)]);
    assert_eq!(split.new_separator, Some(BlockSeparator::Space));
    assert!(split.evidence.contains(&AlignmentEvidence::SplitMerge));

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("three-block reflow should compare");
    assert!(comparison.changes.is_empty());
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert!(comparison.unresolved_regions.is_empty());
}

#[test]
fn aligns_a_masked_replacement_split_as_one_to_three_between_anchors() {
    let old = vec![
        block_text(1, OPENING),
        block_text_with_matching(
            2,
            "Release 10 final notes",
            "Release <NUM> final notes",
            true,
        ),
        block_text(3, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, "Release"),
        block_text_with_matching(103, "20 final", "<NUM> final", true),
        block_text(104, "notes"),
        block_text(105, CLOSING),
    ];
    let alignment = align(old.clone(), new.clone());

    let split = &alignment.spans[1];
    assert_eq!(split.kind, AlignmentKind::Match);
    assert_eq!(split.old, [BlockId(2)]);
    assert_eq!(split.new, [BlockId(102), BlockId(103), BlockId(104)]);
    assert!(split.evidence.contains(&AlignmentEvidence::NumericMask));
    assert!(split.evidence.contains(&AlignmentEvidence::AnchorInterval));

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("masked three-block replacement should compare exactly");
    assert_eq!(comparison.changes.len(), 1);
    assert_eq!(comparison.changes[0].kind, ChangeKind::Replacement);
    assert!(comparison.unresolved_regions.is_empty());
}

#[test]
fn aligns_an_exact_english_block_merge_as_three_to_one() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "project"),
            block_text(3, "release"),
            block_text(4, "log"),
            block_text(5, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "project release log"),
            block_text(103, CLOSING),
        ],
    );

    let merge = &alignment.spans[1];
    assert_eq!(merge.kind, AlignmentKind::Match);
    assert_eq!(merge.old, [BlockId(2), BlockId(3), BlockId(4)]);
    assert_eq!(merge.new, [BlockId(102)]);
    assert_eq!(merge.old_separator, Some(BlockSeparator::Space));
    assert!(merge.evidence.contains(&AlignmentEvidence::SplitMerge));
}

#[test]
fn aligns_a_masked_replacement_merge_as_three_to_one_between_anchors() {
    let old = vec![
        block_text(1, OPENING),
        block_text(2, "Release"),
        block_text_with_matching(3, "10 final", "<NUM> final", true),
        block_text(4, "notes"),
        block_text(5, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text_with_matching(
            102,
            "Release 20 final notes",
            "Release <NUM> final notes",
            true,
        ),
        block_text(103, CLOSING),
    ];
    let alignment = align(old.clone(), new.clone());

    let merge = &alignment.spans[1];
    assert_eq!(merge.kind, AlignmentKind::Match);
    assert_eq!(merge.old, [BlockId(2), BlockId(3), BlockId(4)]);
    assert_eq!(merge.new, [BlockId(102)]);
    assert!(merge.evidence.contains(&AlignmentEvidence::NumericMask));
    assert!(merge.evidence.contains(&AlignmentEvidence::AnchorInterval));

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("masked three-block merge should compare exactly");
    assert_eq!(comparison.changes.len(), 1);
    assert_eq!(comparison.changes[0].kind, ChangeKind::Replacement);
    assert!(comparison.unresolved_regions.is_empty());
}

#[test]
fn aligns_an_exact_block_split_after_the_final_anchor() {
    let old = vec![block_text(1, OPENING), block_text(2, "project log")];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, "project"),
        block_text(103, "log"),
    ];
    let alignment = align(old.clone(), new.clone());

    let split = &alignment.spans[1];
    assert_eq!(split.kind, AlignmentKind::Match);
    assert_eq!(split.old, [BlockId(2)]);
    assert_eq!(split.new, [BlockId(102), BlockId(103)]);
    assert_eq!(split.new_separator, Some(BlockSeparator::Space));
    assert!(split.evidence.contains(&AlignmentEvidence::SplitMerge));
    assert!(!split.evidence.contains(&AlignmentEvidence::AnchorInterval));

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("trailing reflow should compare");
    assert!(comparison.changes.is_empty());
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert!(comparison.unresolved_regions.is_empty());
}

#[test]
fn aligns_an_exact_block_merge_before_the_first_anchor() {
    let old = vec![
        block_text(1, "project"),
        block_text(2, "log"),
        block_text(3, CLOSING),
    ];
    let new = vec![block_text(101, "project log"), block_text(102, CLOSING)];
    let alignment = align(old.clone(), new.clone());

    let merge = &alignment.spans[0];
    assert_eq!(merge.kind, AlignmentKind::Match);
    assert_eq!(merge.old, [BlockId(1), BlockId(2)]);
    assert_eq!(merge.new, [BlockId(101)]);
    assert_eq!(merge.old_separator, Some(BlockSeparator::Space));
    assert!(merge.evidence.contains(&AlignmentEvidence::SplitMerge));
    assert!(!merge.evidence.contains(&AlignmentEvidence::AnchorInterval));

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("leading reflow should compare");
    assert!(comparison.changes.is_empty());
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert!(comparison.unresolved_regions.is_empty());
}

#[test]
fn does_not_use_a_trailing_anchor_for_a_masked_split() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text_with_matching(2, "Release 10 notes", "Release <NUM> notes", true),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "Release"),
            block_text_with_matching(103, "20 notes", "<NUM> notes", true),
        ],
    );

    assert!(!alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old.len() == 1 && span.new.len() == 2
    }));
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
fn preserves_a_short_unique_match_outside_the_primary_anchor_policy() {
    let old = build_block_features(&[block_text(1, "stable"), block_text(2, "old only")], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(101, "stable"), block_text(102, "new only")], 3)
        .expect("new features should build");
    let mut options = options();
    options.anchor_min_tokens = 100;

    let alignment =
        align_ordered(&old, &new, &EmptyGenerator, options).expect("alignment should succeed");

    assert!(alignment.main_anchors.is_empty());
    assert!(alignment.move_candidates.is_empty());
    assert_anchor_evidence_matches_main_anchors(&alignment);
    assert_eq!(alignment.spans.len(), 2);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);
    assert_eq!(alignment.spans[0].new, [BlockId(101)]);
    assert_eq!(alignment.spans[1].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert_eq!(alignment.spans[1].new, [BlockId(102)]);
}

#[test]
fn preserves_multiple_short_unique_matches_between_ambiguous_edits() {
    let old = build_block_features(
        &[
            block_text(1, "alpha"),
            block_text(2, "old first"),
            block_text(3, "beta"),
            block_text(4, "old second"),
        ],
        3,
    )
    .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(101, "alpha"),
            block_text(102, "new first"),
            block_text(103, "beta"),
            block_text(104, "new second"),
        ],
        3,
    )
    .expect("new features should build");
    let mut options = options();
    options.anchor_min_tokens = 100;

    let alignment =
        align_ordered(&old, &new, &EmptyGenerator, options).expect("alignment should succeed");

    for (old, new) in [(1, 101), (3, 103)] {
        assert!(alignment.spans.iter().any(|span| {
            span.kind == AlignmentKind::Match
                && span.old == [BlockId(old)]
                && span.new == [BlockId(new)]
        }));
    }
    assert_eq!(
        alignment
            .spans
            .iter()
            .filter(|span| span.kind == AlignmentKind::Unresolved)
            .count(),
        2
    );
}

#[test]
fn selects_a_monotonic_subset_of_crossing_short_exact_matches() {
    let old = build_block_features(&[block_text(1, "alpha"), block_text(2, "beta")], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(101, "beta"), block_text(102, "alpha")], 3)
        .expect("new features should build");
    let mut options = options();
    options.anchor_min_tokens = 100;

    let alignment =
        align_ordered(&old, &new, &EmptyGenerator, options).expect("alignment should succeed");

    assert_eq!(
        alignment
            .spans
            .iter()
            .filter(|span| span.kind == AlignmentKind::Match)
            .count(),
        1
    );
    assert!(alignment.move_candidates.is_empty());
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

    assert_eq!(alignment.spans.len(), 3);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);
    assert_eq!(alignment.spans[0].new, [BlockId(101)]);

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[1].old, [BlockId(2), BlockId(3)]);
    assert_eq!(alignment.spans[1].new, [BlockId(102), BlockId(103)]);

    assert_eq!(alignment.spans[2].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[2].old, [BlockId(4)]);
    assert_eq!(alignment.spans[2].new, [BlockId(104)]);
}

#[test]
fn unsupported_masked_match_does_not_collapse_adjacent_certain_deletion_or_exact_match() {
    let deleted = block_text(1, "Deleted Section Header");
    let old_masked = block_text_with_matching(
        2,
        "The archive contains 10 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let old_exact = block_text(3, "Stable Ending Section Content Paragraph");

    let new_masked = block_text_with_matching(
        102,
        "The archive contains 20 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let new_exact = block_text(103, "Stable Ending Section Content Paragraph");

    let old = vec![deleted, old_masked, old_exact];
    let new = vec![new_masked, new_exact];

    let alignment = align(old.clone(), new.clone());

    // Spans must NOT be collapsed into 1 giant Unresolved span.
    // Must contain:
    // 0: Deletion (block 1)
    // 1: Unresolved (block 2 <-> 102)
    // 2: Match (block 3 <-> 103)
    assert_eq!(alignment.spans.len(), 3);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Deletion);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert_eq!(alignment.spans[1].new, [BlockId(102)]);
    assert!(
        alignment.spans[1]
            .evidence
            .contains(&AlignmentEvidence::NumericMask)
    );

    assert_eq!(alignment.spans[2].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[2].old, [BlockId(3)]);
    assert_eq!(alignment.spans[2].new, [BlockId(103)]);

    // Downstream compare_aligned must detect the known content change (Deletion)
    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("comparison should succeed");

    assert_eq!(comparison.changes.len(), 1);
    assert_eq!(comparison.changes[0].kind, ChangeKind::Deletion);
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert!(
        comparison
            .old_coverage
            .ratio
            .expect("old coverage ratio should exist")
            < 1.0
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .expect("new coverage ratio should exist")
            < 1.0
    );
}

#[test]
fn unsupported_masked_match_does_not_collapse_adjacent_exact_match_or_certain_insertion() {
    let old_exact = block_text(1, "Stable Leading Section Content Paragraph");
    let old_masked = block_text_with_matching(
        2,
        "The archive contains 10 files.",
        "The archive contains <NUM> files.",
        true,
    );

    let new_exact = block_text(101, "Stable Leading Section Content Paragraph");
    let new_masked = block_text_with_matching(
        102,
        "The archive contains 20 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let inserted = block_text(103, "Inserted Section Header");

    let old = vec![old_exact, old_masked];
    let new = vec![new_exact, new_masked, inserted];

    let alignment = align(old.clone(), new.clone());

    assert_eq!(alignment.spans.len(), 3);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);
    assert_eq!(alignment.spans[0].new, [BlockId(101)]);

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert_eq!(alignment.spans[1].new, [BlockId(102)]);

    assert_eq!(alignment.spans[2].kind, AlignmentKind::Insertion);
    assert_eq!(alignment.spans[2].new, [BlockId(103)]);

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("comparison should succeed");

    assert_eq!(comparison.changes.len(), 1);
    assert_eq!(comparison.changes[0].kind, ChangeKind::Insertion);
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert!(
        comparison
            .old_coverage
            .ratio
            .expect("old coverage ratio should exist")
            < 1.0
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .expect("new coverage ratio should exist")
            < 1.0
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

    assert_eq!(alignment.spans.len(), 3);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Deletion);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);

    assert_eq!(alignment.spans[1].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert_eq!(alignment.spans[1].new, [BlockId(101)]);

    assert_eq!(alignment.spans[2].kind, AlignmentKind::Insertion);
    assert_eq!(alignment.spans[2].new, [BlockId(102)]);
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
        span.kind == AlignmentKind::Deletion
            && span.evidence.contains(&AlignmentEvidence::MoveCandidate)
    }));
    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Insertion
            && span.evidence.contains(&AlignmentEvidence::MoveCandidate)
    }));
}

#[test]
fn confines_an_off_lis_anchor_to_move_candidate_spans() {
    let moved = "Moved unique anchor paragraph";
    let boundary = "Stable boundary anchor paragraph";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, moved),
        block_text(3, "Alpha stays"),
        block_text(4, "Beta stays"),
        block_text(5, "Gamma stays"),
        block_text(6, boundary),
        block_text(7, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(103, "Alpha stays"),
        block_text(104, "Beta stays"),
        block_text(105, "Gamma stays"),
        block_text(106, boundary),
        block_text(107, CLOSING),
        block_text(102, moved),
    ];
    let alignment = align(old.clone(), new.clone());

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
    let moved_spans = alignment
        .spans
        .iter()
        .filter(|span| span.evidence.contains(&AlignmentEvidence::MoveCandidate))
        .collect::<Vec<_>>();
    assert_eq!(moved_spans.len(), 2, "{:#?}", alignment.spans);
    assert!(moved_spans.iter().any(|span| {
        span.kind == AlignmentKind::Deletion && span.old == [BlockId(2)] && span.new.is_empty()
    }));
    assert!(moved_spans.iter().any(|span| {
        span.kind == AlignmentKind::Insertion && span.old.is_empty() && span.new == [BlockId(102)]
    }));

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("move fallback should compare");
    assert_eq!(
        comparison
            .changes
            .iter()
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Move]
    );
    assert!(comparison.unresolved_regions.is_empty());
    assert!(
        summarize(&comparison, &ExtractionStatus::complete())
            .expect("move fallback summary should validate")
            .comparison_complete
    );
}

#[test]
fn promotes_crossed_move_candidates_from_the_same_intervals() {
    let first_move = "First moved unique paragraph remains exact";
    let second_move = "Second moved unique paragraph remains exact";
    let middle = "Middle stable anchor paragraph remains exact";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, first_move),
        block_text(3, middle),
        block_text(4, CLOSING),
        block_text(5, second_move),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(105, second_move),
        block_text(103, middle),
        block_text(104, CLOSING),
        block_text(102, first_move),
    ];
    let alignment = align(old.clone(), new.clone());

    assert_eq!(alignment.move_candidates.len(), 2);
    assert_eq!(
        alignment
            .spans
            .iter()
            .filter(|span| span.evidence.contains(&AlignmentEvidence::MoveCandidate))
            .count(),
        4,
        "{:#?}",
        alignment.spans
    );

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("crossed move candidates should compare");
    assert_eq!(
        comparison
            .changes
            .iter()
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Move, ChangeKind::Move]
    );
    assert!(comparison.unresolved_regions.is_empty());
    assert_eq!(comparison.old_coverage.ratio, Some(1.0));
    assert_eq!(comparison.new_coverage.ratio, Some(1.0));
}

#[test]
fn preserves_crossed_move_candidates_beside_unmatched_blocks() {
    let first_move = "First moved unique paragraph remains exact";
    let second_move = "Second moved unique paragraph remains exact";
    let middle = "Middle stable anchor paragraph remains exact";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, first_move),
        block_text(3, "Old unmatched paragraph"),
        block_text(4, middle),
        block_text(5, CLOSING),
        block_text(6, second_move),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(103, "New unrelated paragraph"),
        block_text(106, second_move),
        block_text(104, middle),
        block_text(105, CLOSING),
        block_text(102, first_move),
    ];
    let alignment = align(old.clone(), new.clone());

    assert_eq!(alignment.move_candidates.len(), 2);
    assert_eq!(
        alignment
            .spans
            .iter()
            .filter(|span| span.evidence.contains(&AlignmentEvidence::MoveCandidate))
            .count(),
        4,
        "{:#?}",
        alignment.spans
    );

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("move candidates beside unmatched blocks should compare");
    assert_eq!(
        comparison
            .changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Move)
            .count(),
        2
    );
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert_eq!(
        comparison.unresolved_regions[0]
            .old_span
            .as_ref()
            .expect("old unmatched span should remain")
            .blocks,
        [BlockId(3)]
    );
    assert_eq!(
        comparison.unresolved_regions[0]
            .new_span
            .as_ref()
            .expect("new unmatched span should remain")
            .blocks,
        [BlockId(103)]
    );
}

#[test]
fn preserves_crossed_move_candidates_beside_normalization_issues() {
    let first_move = "First moved unique paragraph remains exact";
    let second_move = "Second moved unique paragraph remains exact";
    let middle = "Middle stable anchor paragraph remains exact";
    let old_text = vec![
        block_text(1, OPENING),
        block_text(2, first_move),
        block_text(3, "Old uncertain paragraph"),
        block_text(4, middle),
        block_text(5, CLOSING),
        block_text(6, second_move),
    ];
    let new_text = vec![
        block_text(101, OPENING),
        block_text(103, "New uncertain paragraph"),
        block_text(106, second_move),
        block_text(104, middle),
        block_text(105, CLOSING),
        block_text(102, first_move),
    ];
    let mut old = build_block_features(&old_text, 3).expect("old features should build");
    let mut new = build_block_features(&new_text, 3).expect("new features should build");
    old[2].has_normalization_issues = true;
    new[1].has_normalization_issues = true;
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert_eq!(alignment.move_candidates.len(), 2);
    assert_eq!(
        alignment
            .spans
            .iter()
            .filter(|span| span.evidence.contains(&AlignmentEvidence::MoveCandidate))
            .count(),
        4,
        "{:#?}",
        alignment.spans
    );

    let comparison = compare_aligned(&old_text, &new_text, &alignment, DiffOptions::default())
        .expect("move candidates beside normalization issues should compare");
    assert_eq!(
        comparison
            .changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Move)
            .count(),
        2
    );
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert_eq!(
        comparison.unresolved_regions[0]
            .old_span
            .as_ref()
            .expect("old uncertain span should remain")
            .blocks,
        [BlockId(3)]
    );
    assert_eq!(
        comparison.unresolved_regions[0]
            .new_span
            .as_ref()
            .expect("new uncertain span should remain")
            .blocks,
        [BlockId(103)]
    );
}

#[test]
fn recovers_a_secondary_anchor_between_preserved_move_candidates() {
    let first_move = "First moved unique paragraph remains exact";
    let second_move = "Second moved unique paragraph remains exact";
    let middle = "Middle stable anchor paragraph remains exact";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, first_move),
        block_text(3, "Old leading edit"),
        block_text(4, "S"),
        block_text(5, "Old trailing edit"),
        block_text(6, middle),
        block_text(7, CLOSING),
        block_text(8, second_move),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(103, "New leading edit"),
        block_text(104, "S"),
        block_text(105, "New trailing edit"),
        block_text(108, second_move),
        block_text(106, middle),
        block_text(107, CLOSING),
        block_text(102, first_move),
    ];
    let alignment = align(old.clone(), new.clone());

    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old == [BlockId(4)] && span.new == [BlockId(104)]
    }));
    assert_eq!(
        alignment
            .spans
            .iter()
            .filter(|span| span.evidence.contains(&AlignmentEvidence::MoveCandidate))
            .count(),
        4,
        "{:#?}",
        alignment.spans
    );

    let comparison = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("secondary anchor between move candidates should compare");
    assert_eq!(
        comparison
            .changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Move)
            .count(),
        2
    );
    assert!(
        summarize(&comparison, &ExtractionStatus::complete())
            .expect("secondary-anchor summary should validate")
            .comparison_complete
    );
}

#[test]
fn recovers_a_secondary_anchor_inside_normalization_issues() {
    let old_text = [
        block_text(1, "Old leading uncertainty"),
        block_text(2, "S"),
        block_text(3, "Old trailing uncertainty"),
    ];
    let new_text = [
        block_text(101, "New leading uncertainty"),
        block_text(102, "S"),
        block_text(103, "New trailing uncertainty"),
    ];
    let mut old = build_block_features(&old_text, 3).expect("old features should build");
    let mut new = build_block_features(&new_text, 3).expect("new features should build");
    old[0].has_normalization_issues = true;
    old[2].has_normalization_issues = true;
    new[0].has_normalization_issues = true;
    new[2].has_normalization_issues = true;
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old == [BlockId(2)] && span.new == [BlockId(102)]
    }));
    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Unresolved
            && span
                .evidence
                .contains(&AlignmentEvidence::NormalizationIssue)
    }));
}

#[test]
fn keeps_a_secondary_anchor_absorbed_by_an_exact_merge() {
    let alignment = align(
        vec![
            block_text(1, OPENING),
            block_text(2, "foo"),
            block_text(3, "bar"),
            block_text(4, CLOSING),
        ],
        vec![
            block_text(101, OPENING),
            block_text(102, "foobar"),
            block_text(103, "foo"),
            block_text(104, CLOSING),
        ],
    );

    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match
            && span.old == [BlockId(2), BlockId(3)]
            && span.new == [BlockId(102)]
            && span.old_separator == Some(BlockSeparator::Concatenate)
    }));
    assert!(
        alignment
            .spans
            .iter()
            .any(|span| { span.kind == AlignmentKind::Insertion && span.new == [BlockId(103)] })
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
    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match
            && span.old == [BlockId(3)]
            && span.new == [BlockId(103)]
            && span.evidence.contains(&AlignmentEvidence::ExactCanonical)
    }));
    assert!(
        alignment
            .spans
            .iter()
            .all(|span| span.kind == AlignmentKind::Match)
    );
}

#[test]
fn keeps_unequal_normalization_issue_blocks_unresolved() {
    let old_text = [block_text(1, "Ambiguous old paragraph")];
    let new_text = [block_text(101, "Ambiguous new paragraph")];
    let mut old = build_block_features(&old_text, 3).expect("old features should build");
    let mut new = build_block_features(&new_text, 3).expect("new features should build");
    old[0].has_normalization_issues = true;
    new[0].has_normalization_issues = true;
    new[0].exact_hash = old[0].exact_hash;
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
    assert!(
        alignment.spans[0]
            .evidence
            .contains(&AlignmentEvidence::NormalizationIssue)
    );
}

#[test]
fn keeps_one_sided_normalization_issue_unresolved() {
    let old_text = [block_text(1, "Ambiguous paragraph")];
    let new_text = [block_text(101, "Ambiguous paragraph")];
    let mut old = build_block_features(&old_text, 3).expect("old features should build");
    let new = build_block_features(&new_text, 3).expect("new features should build");
    old[0].has_normalization_issues = true;
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert!(
        alignment.spans.iter().any(|span| {
            span.kind == AlignmentKind::Unresolved
                && span.old == [BlockId(1)]
                && span.new == [BlockId(101)]
                && span
                    .evidence
                    .contains(&AlignmentEvidence::NormalizationIssue)
        }),
        "{:#?}",
        alignment.spans
    );
}

#[test]
fn matches_stable_unmapped_tokens_with_issues_by_exact_vector() {
    let old = build_block_features(&[unmapped_block_text(1, vec![1, 2, 3], 42)], 3)
        .expect("old features should build");
    let new = build_block_features(&[unmapped_block_text(101, vec![1, 2, 3], 42)], 3)
        .expect("new features should build");

    assert!(old[0].has_normalization_issues);
    assert!(new[0].has_normalization_issues);

    let alignment = align_ordered(&old, &new, &EmptyGenerator, options())
        .expect("stable unmapped evidence should align");

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[0].score, 1.0);
    assert_eq!(
        alignment.spans[0].evidence,
        [AlignmentEvidence::ExactCanonical]
    );
}

#[test]
fn keeps_changed_unmapped_font_programs_unresolved() {
    let old = build_block_features(&[unmapped_block_text(1, vec![1, 2, 3], 42)], 3)
        .expect("old features should build");
    let new = build_block_features(&[unmapped_block_text(101, vec![4, 5, 6], 42)], 3)
        .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
    assert!(
        alignment.spans[0]
            .evidence
            .contains(&AlignmentEvidence::NormalizationIssue)
    );
}

#[test]
fn keeps_empty_normalization_issue_evidence_unresolved() {
    let issue = NormalizationIssue {
        kind: NormalizationIssueKind::AmbiguousLineBreak,
        raw_range: ScalarRange { start: 0, end: 0 },
        source: TextSource { atoms: Vec::new() },
    };
    let mut old_text = block_text(1, "");
    old_text.issues.push(issue.clone());
    let mut new_text = block_text(101, "");
    new_text.issues.push(issue);
    let old = build_block_features(std::slice::from_ref(&old_text), 3)
        .expect("old features should build");
    let new = build_block_features(std::slice::from_ref(&new_text), 3)
        .expect("new features should build");

    let alignment = align_ordered(&old, &new, &EmptyGenerator, options())
        .expect("empty issue evidence should remain classifiable");

    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Unresolved);
    assert_eq!(alignment.spans[0].confidence, AlignmentConfidence::Low);
    assert!(
        alignment.spans[0]
            .evidence
            .contains(&AlignmentEvidence::NormalizationIssue)
    );
    assert!(
        !alignment.spans[0]
            .evidence
            .contains(&AlignmentEvidence::ExactCanonical)
    );
    let comparison = compare_aligned(&[old_text], &[new_text], &alignment, DiffOptions::default())
        .expect("empty unresolved evidence should remain reportable");
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert!(
        !summarize(&comparison, &ExtractionStatus::complete())
            .expect("comparison summary should validate")
            .comparison_complete
    );
}

#[test]
fn aligns_duplicate_normalization_issues_around_an_insertion() {
    let mut old = build_block_features(&[block_text(1, "A"), block_text(2, "B")], 3)
        .expect("old features should build");
    let mut new = build_block_features(
        &[
            block_text(101, "A"),
            block_text(102, "A"),
            block_text(103, "B"),
        ],
        3,
    )
    .expect("new features should build");
    old.iter_mut()
        .chain(&mut new)
        .for_each(|features| features.has_normalization_issues = true);
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert!(
        alignment.spans.iter().any(|span| {
            span.kind == AlignmentKind::Match
                && span.old == [BlockId(1)]
                && span.new == [BlockId(101)]
        }),
        "{:#?}",
        alignment.spans
    );
    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old == [BlockId(2)] && span.new == [BlockId(103)]
    }));
    let unresolved = alignment
        .spans
        .iter()
        .filter(|span| span.kind == AlignmentKind::Unresolved)
        .collect::<Vec<_>>();
    assert_eq!(unresolved.len(), 1, "{:#?}", alignment.spans);
    assert!(unresolved[0].old.is_empty());
    assert_eq!(unresolved[0].new, [BlockId(102)]);
    assert!(
        unresolved[0]
            .evidence
            .contains(&AlignmentEvidence::NormalizationIssue)
    );
}

#[test]
fn aligns_duplicate_normalization_issues_around_a_deletion() {
    let mut old = build_block_features(
        &[block_text(1, "A"), block_text(2, "A"), block_text(3, "B")],
        3,
    )
    .expect("old features should build");
    let mut new = build_block_features(&[block_text(101, "A"), block_text(102, "B")], 3)
        .expect("new features should build");
    old.iter_mut()
        .chain(&mut new)
        .for_each(|features| features.has_normalization_issues = true);
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old == [BlockId(1)] && span.new == [BlockId(101)]
    }));
    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old == [BlockId(3)] && span.new == [BlockId(102)]
    }));
    let unresolved = alignment
        .spans
        .iter()
        .filter(|span| span.kind == AlignmentKind::Unresolved)
        .collect::<Vec<_>>();
    assert_eq!(unresolved.len(), 1, "{:#?}", alignment.spans);
    assert_eq!(unresolved[0].old, [BlockId(2)]);
    assert!(unresolved[0].new.is_empty());
    assert!(
        unresolved[0]
            .evidence
            .contains(&AlignmentEvidence::NormalizationIssue)
    );
}

#[test]
fn preserves_move_candidates_beside_exact_normalization_matches() {
    let moved = "Moved unique anchor paragraph";
    let mut old = build_block_features(
        &[
            block_text(1, OPENING),
            block_text(2, "Ambiguous stable paragraph"),
            block_text(3, moved),
            block_text(4, CLOSING),
        ],
        3,
    )
    .expect("old features should build");
    let mut new = build_block_features(
        &[
            block_text(101, OPENING),
            block_text(102, "Ambiguous stable paragraph"),
            block_text(104, CLOSING),
            block_text(103, moved),
        ],
        3,
    )
    .expect("new features should build");
    old[1].has_normalization_issues = true;
    new[1].has_normalization_issues = true;
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");

    let alignment =
        align_ordered(&old, &new, &generator, options()).expect("alignment should succeed");

    assert!(alignment.spans.iter().any(|span| {
        span.kind == AlignmentKind::Match && span.old == [BlockId(2)] && span.new == [BlockId(102)]
    }));
    assert!(alignment.spans.iter().any(|span| {
        matches!(
            span.kind,
            AlignmentKind::Deletion | AlignmentKind::Insertion
        ) && span.evidence.contains(&AlignmentEvidence::MoveCandidate)
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
fn charges_partition_fallback_against_the_same_dp_cell_budget() {
    let old = build_block_features(&[block_text(1, "stable"), block_text(2, "old only")], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(101, "stable"), block_text(102, "new only")], 3)
        .expect("new features should build");
    let mut at_limit = options();
    at_limit.anchor_min_tokens = 100;
    at_limit.max_dp_cells = 13;

    align_ordered(&old, &new, &EmptyGenerator, at_limit)
        .expect("initial and fallback cells should fit exactly");

    let mut over_limit = at_limit;
    over_limit.max_dp_cells = 12;
    assert!(matches!(
        align_ordered(&old, &new, &EmptyGenerator, over_limit),
        Err(Error::LimitExceeded {
            resource: "alignment DP cells",
            limit: 12,
        })
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
        estimated: RefCell::new(Vec::new()),
        generated: RefCell::new(Vec::new()),
    };
    assert!(matches!(
        align_ordered(&old, &new, &tracking, over_limit),
        Err(Error::LimitExceeded {
            resource: "alignment candidate visits",
            limit: 29,
        })
    ));
    assert!(tracking.generated.borrow().is_empty());

    let mut zero_limit = at_limit;
    zero_limit.max_candidate_visits = 0;
    assert!(matches!(
        align_ordered(&old, &new, &generator, zero_limit),
        Err(Error::InvalidConfiguration(message)) if message.contains("max_candidate_visits")
    ));
}

#[test]
fn skips_candidate_work_for_identical_unique_anchors() {
    let old = build_block_features(&[block_text(1, OPENING), block_text(2, CLOSING)], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(101, OPENING), block_text(102, CLOSING)], 3)
        .expect("new features should build");
    let tracking = TrackingGenerator::new(usize::MAX);
    let mut low_limit = options();
    low_limit.max_candidate_visits = 1;

    let alignment = align_ordered(&old, &new, &tracking, low_limit)
        .expect("main anchors should not consume candidate visits");

    assert_eq!(alignment.main_anchors.len(), 2);
    assert!(tracking.estimated.borrow().is_empty());
    assert!(tracking.generated.borrow().is_empty());
}

#[test]
fn queries_secondary_partitions_before_ambiguity_fallback() {
    let old = build_block_features(&[block_text(1, "stable"), block_text(2, "old only")], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(101, "stable"), block_text(102, "new only")], 3)
        .expect("new features should build");
    let tracking = TrackingGenerator::new(1);
    let mut options = options();
    options.anchor_min_tokens = 100;

    align_ordered(&old, &new, &tracking, options).expect("alignment should succeed");

    assert_eq!(*tracking.estimated.borrow(), [BlockId(1), BlockId(2)]);
    assert_eq!(*tracking.generated.borrow(), [BlockId(1), BlockId(2)]);
}

#[test]
fn leaves_a_non_ambiguous_short_exact_candidate_on_the_normal_path() {
    let old =
        build_block_features(&[block_text(1, "stable")], 3).expect("old features should build");
    let new =
        build_block_features(&[block_text(101, "stable")], 3).expect("new features should build");
    let tracking = TrackingMatchGenerator::new(BlockId(101));
    let mut options = options();
    options.anchor_min_tokens = 100;

    let alignment =
        align_ordered(&old, &new, &tracking, options).expect("alignment should succeed");

    assert!(alignment.main_anchors.is_empty());
    assert_eq!(*tracking.estimated.borrow(), [BlockId(1)]);
    assert_eq!(*tracking.generated.borrow(), [BlockId(1)]);
    assert_eq!(alignment.spans.len(), 1);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert!(
        alignment.spans[0]
            .evidence
            .contains(&AlignmentEvidence::CandidateSource(
                CandidateSource::ShortBlockFallback
            ))
    );
    assert!(
        !alignment.spans[0]
            .evidence
            .contains(&AlignmentEvidence::Anchor)
    );
}

#[test]
fn queries_only_unanchored_interval_blocks() {
    let old = build_block_features(
        &[
            block_text(1, OPENING),
            block_text(2, "Old interval"),
            block_text(3, CLOSING),
        ],
        3,
    )
    .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(101, OPENING),
            block_text(102, "New interval"),
            block_text(103, CLOSING),
        ],
        3,
    )
    .expect("new features should build");
    let tracking = TrackingGenerator::new(1);

    align_ordered(&old, &new, &tracking, options()).expect("alignment should succeed");

    assert_eq!(*tracking.estimated.borrow(), [BlockId(2)]);
    assert_eq!(*tracking.generated.borrow(), [BlockId(2)]);
}

#[test]
fn preflights_remaining_candidate_visits_atomically_at_the_boundary() {
    let old = build_block_features(
        &[
            block_text(1, OPENING),
            block_text(2, "Old interval one"),
            block_text(3, CLOSING),
            block_text(4, "Old interval two"),
            block_text(5, "Final unique anchor paragraph"),
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
            block_text(105, "Final unique anchor paragraph"),
        ],
        3,
    )
    .expect("new features should build");
    let over_limit = TrackingGenerator::new(6);
    let mut options = options();
    options.max_candidate_visits = 11;

    assert!(matches!(
        align_ordered(&old, &new, &over_limit, options),
        Err(Error::LimitExceeded {
            resource: "alignment candidate visits",
            limit: 11,
        })
    ));
    assert_eq!(*over_limit.estimated.borrow(), [BlockId(2), BlockId(4)]);
    assert!(over_limit.generated.borrow().is_empty());

    let at_limit = TrackingGenerator::new(6);
    options.max_candidate_visits = 12;
    align_ordered(&old, &new, &at_limit, options)
        .expect("remaining candidate estimates should fit exactly");
    assert_eq!(*at_limit.estimated.borrow(), [BlockId(2), BlockId(4)]);
    assert_eq!(*at_limit.generated.borrow(), [BlockId(2), BlockId(4)]);
}

#[test]
fn still_queries_off_lis_exact_move_anchors() {
    let first = "First unique anchor paragraph";
    let second = "Second unique anchor paragraph";
    let old = build_block_features(&[block_text(1, first), block_text(2, second)], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(101, second), block_text(102, first)], 3)
        .expect("new features should build");
    let tracking = TrackingGenerator::new(1);

    let alignment =
        align_ordered(&old, &new, &tracking, options()).expect("move should remain classifiable");

    let move_old = alignment
        .move_candidates
        .iter()
        .map(|anchor| anchor.old)
        .collect::<Vec<_>>();
    assert_eq!(*tracking.estimated.borrow(), move_old);
    assert_eq!(*tracking.generated.borrow(), move_old);
    assert_eq!(move_old.len(), 1);
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

const WEAK_PLAUSIBLE_OLD: &str = "total schedule amount line enter 28 b standard here";
const WEAK_PLAUSIBLE_NEW: &str = "total schedule amount line enter 61 c premium elsewhere";
// A rotated, partially rewritten form row: enough shared trigrams to be
// admitted as a weak non-exact match, but most canonical tokens changed.
const WEAK_IMPLAUSIBLE_OLD: &str = "alpha beta gamma delta epsilon zeta eta theta income total";
const WEAK_IMPLAUSIBLE_NEW: &str = "gamma delta epsilon zeta eta theta alpha beta revenue owed";

#[test]
fn calibrates_weak_non_exact_matches_below_the_strong_score_threshold() {
    let old = vec![
        block_text(1, OPENING),
        block_text(2, WEAK_PLAUSIBLE_OLD),
        block_text(3, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, WEAK_PLAUSIBLE_NEW),
        block_text(103, CLOSING),
    ];

    let alignment = align(old.clone(), new.clone());
    let span = &alignment.spans[1];
    assert_eq!(span.kind, AlignmentKind::Match);
    assert!(!span.evidence.contains(&AlignmentEvidence::ExactCanonical));
    let defaults = options();
    assert!(span.score >= defaults.min_match_score);
    assert!(span.score < defaults.strong_match_score);
    // A weak-but-usable correspondence near the admission threshold is not
    // automatically Medium confidence (issue #6).
    assert_eq!(span.confidence, AlignmentConfidence::Low);

    // Dropping the strong threshold to its lowest legal value (the admission
    // threshold) upgrades the same weak span to Medium...
    let mut permissive = options();
    permissive.strong_match_score = permissive.min_match_score;
    assert_eq!(
        align_with(old.clone(), new.clone(), permissive).spans[1].confidence,
        AlignmentConfidence::Medium
    );

    // ...and raising it above every achievable score keeps it Low.
    let mut strict = options();
    strict.strong_match_score = 1.0;
    assert_eq!(
        align_with(old, new, strict).spans[1].confidence,
        AlignmentConfidence::Low
    );
}

#[test]
fn an_irs_style_layout_shift_degrades_to_unresolved_instead_of_change_soup() {
    let old_blocks = vec![
        block_text(1, OPENING),
        block_text(2, WEAK_IMPLAUSIBLE_OLD),
        block_text(3, CLOSING),
    ];
    let new_blocks = vec![
        block_text(101, OPENING),
        block_text(102, WEAK_IMPLAUSIBLE_NEW),
        block_text(103, CLOSING),
    ];

    let alignment = align(old_blocks.clone(), new_blocks.clone());
    let span = &alignment.spans[1];
    assert_eq!(span.kind, AlignmentKind::Match);
    assert_eq!(span.confidence, AlignmentConfidence::Low);

    let result = compare_aligned(&old_blocks, &new_blocks, &alignment, DiffOptions::default())
        .expect("the comparison should survive the implausible span");

    // The implausible match degrades into one honest unresolved region.
    assert!(result.changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 1);
    assert_eq!(
        result.unresolved_regions[0]
            .old_span
            .as_ref()
            .expect("old")
            .blocks,
        [BlockId(2)]
    );
    assert_eq!(
        result.unresolved_regions[0]
            .new_span
            .as_ref()
            .expect("new")
            .blocks,
        [BlockId(102)]
    );
    // Only the exact anchors count as resolved coverage.
    let anchor_tokens = OPENING.chars().count() + CLOSING.chars().count();
    assert_eq!(result.old_coverage.resolved_tokens, anchor_tokens);
    assert_eq!(result.new_coverage.resolved_tokens, anchor_tokens);

    // With the plausibility gate disabled the same span shreds into
    // fragmented low-confidence changes instead of one unresolved region.
    let gate_disabled_options = DiffOptions {
        max_weak_match_change_ratio: 1.0,
        ..DiffOptions::default()
    };
    let ungated = compare_aligned(&old_blocks, &new_blocks, &alignment, gate_disabled_options)
        .expect("the gated-off comparison should succeed");
    assert!(ungated.changes.len() >= 2);
    assert!(
        ungated
            .changes
            .iter()
            .all(|change| change.confidence == Confidence::Low)
    );
}

#[test]
fn retains_a_weak_plausible_match_as_uncertain_changes() {
    let old_blocks = vec![
        block_text(1, OPENING),
        block_text(2, WEAK_PLAUSIBLE_OLD),
        block_text(3, CLOSING),
    ];
    let new_blocks = vec![
        block_text(101, OPENING),
        block_text(102, WEAK_PLAUSIBLE_NEW),
        block_text(103, CLOSING),
    ];
    let alignment = align(old_blocks.clone(), new_blocks.clone());
    assert_eq!(alignment.spans[1].confidence, AlignmentConfidence::Low);

    let result = compare_aligned(&old_blocks, &new_blocks, &alignment, DiffOptions::default())
        .expect("a plausible weak match should diff");
    assert!(result.unresolved_regions.is_empty());
    assert!(!result.changes.is_empty());
    assert!(
        result
            .changes
            .iter()
            .all(|change| change.confidence == Confidence::Low)
    );
    let summary =
        summarize(&result, &ExtractionStatus::complete()).expect("the summary should build");
    assert_eq!(summary.uncertain_changes, result.changes.len());
}

fn align(old: Vec<BlockText>, new: Vec<BlockText>) -> Alignment {
    align_with(old, new, options())
}

fn align_with(old: Vec<BlockText>, new: Vec<BlockText>, opts: AlignmentOptions) -> Alignment {
    let old = build_block_features(&old, 3).expect("old features should build");
    let new = build_block_features(&new, 3).expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    align_ordered(&old, &new, &generator, opts).expect("alignment should succeed")
}

fn assert_anchor_evidence_matches_main_anchors(alignment: &Alignment) {
    let evidenced = alignment
        .spans
        .iter()
        .filter(|span| span.evidence.contains(&AlignmentEvidence::Anchor))
        .map(|span| ExactAnchor {
            old: span.old[0],
            new: span.new[0],
        })
        .collect::<Vec<_>>();
    assert_eq!(evidenced, alignment.main_anchors);
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
    estimated: RefCell<Vec<BlockId>>,
    generated: RefCell<Vec<BlockId>>,
}

struct TrackingMatchGenerator {
    candidate: BlockId,
    estimated: RefCell<Vec<BlockId>>,
    generated: RefCell<Vec<BlockId>>,
}

impl TrackingMatchGenerator {
    fn new(candidate: BlockId) -> Self {
        Self {
            candidate,
            estimated: RefCell::new(Vec::new()),
            generated: RefCell::new(Vec::new()),
        }
    }
}

impl CandidateGenerator for TrackingMatchGenerator {
    fn estimated_visits(&self, old: &BlockFeatures, _limit: usize) -> Result<usize> {
        self.estimated.borrow_mut().push(old.block);
        Ok(1)
    }

    fn candidates(&self, old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
        self.generated.borrow_mut().push(old.block);
        Ok(vec![Candidate {
            block: self.candidate,
            sources: vec![CandidateSource::ShortBlockFallback],
            coarse_score: 1.0,
        }])
    }
}

impl TrackingGenerator {
    fn new(visits: usize) -> Self {
        Self {
            visits,
            estimated: RefCell::new(Vec::new()),
            generated: RefCell::new(Vec::new()),
        }
    }
}

impl CandidateGenerator for TrackingGenerator {
    fn estimated_visits(&self, old: &BlockFeatures, _limit: usize) -> Result<usize> {
        self.estimated.borrow_mut().push(old.block);
        Ok(self.visits)
    }

    fn candidates(&self, old: &BlockFeatures, _limit: usize) -> Result<Vec<Candidate>> {
        self.generated.borrow_mut().push(old.block);
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

fn unmapped_block_text(id: u64, font_hash: Vec<u8>, glyph_id: u16) -> BlockText {
    let font_hash = FontProgramHash(font_hash);
    let unmapped = UnmappedToken {
        scalar_index: 0,
        font_hash: font_hash.clone(),
        glyph_id,
        source: TextSource { atoms: vec![] },
    };
    let mapped = MappedText {
        text: String::new(),
        source_map: vec![],
        unmapped: vec![unmapped],
    };
    BlockText {
        block: BlockId(id),
        raw: mapped.clone(),
        canonical: mapped,
        matching: String::new(),
        matching_tokens: vec![ComparableToken::Unmapped {
            font_hash,
            glyph_id,
        }],
        numeric_mask_applied: false,
        normalization_events: vec![],
        issues: vec![],
        pages: Vec::new(),
    }
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
        pages: Vec::new(),
    }
}

fn mapped_text(text: &str) -> MappedText {
    MappedText {
        text: text.to_owned(),
        source_map: vec![],
        unmapped: vec![],
    }
}

#[test]
fn monotone_anchor_chain_preserves_order_around_insertions_and_deletions() {
    let old_blocks = [
        block_text(1, "Unique First Chapter Heading Title Section A"),
        block_text(2, "Middle paragraph that will be deleted here"),
        block_text(3, "Unique Concluding Chapter Summary Section B"),
    ];
    let new_blocks = [
        block_text(101, "Unique First Chapter Heading Title Section A"),
        block_text(102, "Brand new inserted replacement paragraph text"),
        block_text(103, "Another newly inserted text paragraph here"),
        block_text(104, "Unique Concluding Chapter Summary Section B"),
    ];

    let old_features = build_block_features(&old_blocks, 3).expect("features should build");
    let new_features = build_block_features(&new_blocks, 3).expect("features should build");

    let anchors = exact_anchors(&old_features, &new_features, 10).expect("anchors should extract");
    assert_eq!(anchors.len(), 2);
    assert_eq!(
        anchors[0],
        ExactAnchor {
            old: BlockId(1),
            new: BlockId(101)
        }
    );
    assert_eq!(
        anchors[1],
        ExactAnchor {
            old: BlockId(3),
            new: BlockId(104)
        }
    );

    let chain = select_monotone_anchor_chain(&anchors, &old_features, &new_features)
        .expect("chain should select");
    assert_eq!(chain.main_chain.len(), 2);
    assert_eq!(chain.main_chain, anchors);
    assert!(chain.move_candidates.is_empty());

    let windows = partition_anchor_windows(&chain.main_chain, &old_features, &new_features)
        .expect("windows should partition");
    assert_eq!(windows.len(), 3);

    // Window 0: before first anchor (empty)
    assert_eq!(windows[0].old_range, (0, 0));
    assert_eq!(windows[0].new_range, (0, 0));
    assert_eq!(windows[0].left_anchor, None);
    assert_eq!(
        windows[0].right_anchor,
        Some(ExactAnchor {
            old: BlockId(1),
            new: BlockId(101)
        })
    );

    // Window 1: between anchor 1 and anchor 2 (contains deleted block 2 and inserted blocks 102, 103)
    assert_eq!(windows[1].old_range, (1, 2)); // Block 2
    assert_eq!(windows[1].new_range, (1, 3)); // Blocks 102, 103
    assert_eq!(
        windows[1].left_anchor,
        Some(ExactAnchor {
            old: BlockId(1),
            new: BlockId(101)
        })
    );
    assert_eq!(
        windows[1].right_anchor,
        Some(ExactAnchor {
            old: BlockId(3),
            new: BlockId(104)
        })
    );

    // Window 2: after second anchor (empty)
    assert_eq!(windows[2].old_range, (3, 3));
    assert_eq!(windows[2].new_range, (4, 4));
    assert_eq!(
        windows[2].left_anchor,
        Some(ExactAnchor {
            old: BlockId(3),
            new: BlockId(104)
        })
    );
    assert_eq!(windows[2].right_anchor, None);
}

#[test]
fn monotone_anchor_chain_retains_relocated_anchor_as_move_candidate() {
    // Old: A (1), B (2), C (3), D (4)
    // New: A (101), C (103), D (104), B (102) -> B moved to the end!
    let old_blocks = [
        block_text(1, "Unique Section Header Alpha Number 001"),
        block_text(2, "Relocated Movable Paragraph Content Beta 002"),
        block_text(3, "Unique Section Header Gamma Number 003"),
        block_text(4, "Unique Section Header Delta Number 004"),
    ];
    let new_blocks = [
        block_text(101, "Unique Section Header Alpha Number 001"),
        block_text(103, "Unique Section Header Gamma Number 003"),
        block_text(104, "Unique Section Header Delta Number 004"),
        block_text(102, "Relocated Movable Paragraph Content Beta 002"),
    ];

    let old_features = build_block_features(&old_blocks, 3).expect("features should build");
    let new_features = build_block_features(&new_blocks, 3).expect("features should build");

    let anchors = exact_anchors(&old_features, &new_features, 5).expect("anchors should extract");
    assert_eq!(anchors.len(), 4);

    let chain = select_monotone_anchor_chain(&anchors, &old_features, &new_features)
        .expect("chain should select");

    // Main chain must be monotonic: A -> A (0->0), C -> C (2->1), D -> D (3->2)
    assert_eq!(chain.main_chain.len(), 3);
    assert_eq!(
        chain.main_chain[0],
        ExactAnchor {
            old: BlockId(1),
            new: BlockId(101)
        }
    );
    assert_eq!(
        chain.main_chain[1],
        ExactAnchor {
            old: BlockId(3),
            new: BlockId(103)
        }
    );
    assert_eq!(
        chain.main_chain[2],
        ExactAnchor {
            old: BlockId(4),
            new: BlockId(104)
        }
    );

    // Relocated B -> B (1->3) must be preserved in move_candidates, not discarded!
    assert_eq!(chain.move_candidates.len(), 1);
    assert_eq!(
        chain.move_candidates[0],
        ExactAnchor {
            old: BlockId(2),
            new: BlockId(102)
        }
    );
}

#[test]
fn exact_anchors_rejects_duplicate_and_ambiguous_blocks() {
    let mut ambiguous_block = block_text(2, "Unique middle paragraph with issue");
    ambiguous_block.issues.push(NormalizationIssue {
        kind: NormalizationIssueKind::AmbiguousLineBreak,
        raw_range: ScalarRange { start: 5, end: 6 },
        source: TextSource { atoms: vec![] },
    });

    let old_blocks = [
        block_text(1, "Repeated Header Line Across Multiple Pages"),
        ambiguous_block,
        block_text(3, "Repeated Header Line Across Multiple Pages"),
        block_text(4, "Truly Unique Concluding Paragraph Here"),
    ];
    let new_blocks = [
        block_text(101, "Repeated Header Line Across Multiple Pages"),
        block_text(102, "Truly Unique Concluding Paragraph Here"),
    ];

    let old_features = build_block_features(&old_blocks, 3).expect("features should build");
    let new_features = build_block_features(&new_blocks, 3).expect("features should build");

    let anchors = exact_anchors(&old_features, &new_features, 3).expect("anchors should extract");

    // Only block 4 <-> 102 should be selected:
    // - Block 1 & 3 are duplicates in old -> rejected
    // - Block 2 has normalization issues -> rejected
    assert_eq!(anchors.len(), 1);
    assert_eq!(
        anchors[0],
        ExactAnchor {
            old: BlockId(4),
            new: BlockId(102)
        }
    );
}

#[test]
fn partition_anchor_windows_handles_empty_and_unanchored_documents() {
    let old_blocks = [
        block_text(1, "First unmatched content"),
        block_text(2, "Second unmatched content"),
    ];
    let new_blocks = [block_text(101, "Different new content")];

    let old_features = build_block_features(&old_blocks, 3).expect("features should build");
    let new_features = build_block_features(&new_blocks, 3).expect("features should build");

    // Zero anchors -> exactly 1 window covering (0..2, 0..1)
    let windows = partition_anchor_windows(&[], &old_features, &new_features)
        .expect("empty chain windows should partition");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].old_range, (0, 2));
    assert_eq!(windows[0].new_range, (0, 1));
    assert_eq!(windows[0].left_anchor, None);
    assert_eq!(windows[0].right_anchor, None);
}

#[test]
fn localized_insertion_and_deletion_between_stable_anchors_preserves_anchor_intervals() {
    // 1. Localized deletion between anchors
    let old_del = vec![
        block_text(1, OPENING),
        block_text(2, "Middle paragraph to be deleted from old version"),
        block_text(3, CLOSING),
    ];
    let new_del = vec![block_text(101, OPENING), block_text(103, CLOSING)];

    let align_del = align(old_del.clone(), new_del.clone());
    assert_eq!(align_del.main_anchors.len(), 2);
    assert_eq!(align_del.spans.len(), 3);
    assert_eq!(align_del.spans[0].kind, AlignmentKind::Match);
    assert_eq!(align_del.spans[1].kind, AlignmentKind::Deletion);
    assert_eq!(align_del.spans[1].old, [BlockId(2)]);
    assert_eq!(align_del.spans[2].kind, AlignmentKind::Match);

    let diff_del = compare_aligned(&old_del, &new_del, &align_del, DiffOptions::default())
        .expect("comparison should succeed");
    assert!(
        diff_del
            .changes
            .iter()
            .any(|c| c.kind == ChangeKind::Deletion)
    );

    // 2. Localized insertion between anchors
    let old_ins = vec![block_text(1, OPENING), block_text(3, CLOSING)];
    let new_ins = vec![
        block_text(101, OPENING),
        block_text(102, "Brand new inserted paragraph in new version"),
        block_text(103, CLOSING),
    ];

    let align_ins = align(old_ins.clone(), new_ins.clone());
    assert_eq!(align_ins.main_anchors.len(), 2);
    assert_eq!(align_ins.spans.len(), 3);
    assert_eq!(align_ins.spans[0].kind, AlignmentKind::Match);
    assert_eq!(align_ins.spans[1].kind, AlignmentKind::Insertion);
    assert_eq!(align_ins.spans[1].new, [BlockId(102)]);
    assert_eq!(align_ins.spans[2].kind, AlignmentKind::Match);

    let diff_ins = compare_aligned(&old_ins, &new_ins, &align_ins, DiffOptions::default())
        .expect("comparison should succeed");
    assert!(
        diff_ins
            .changes
            .iter()
            .any(|c| c.kind == ChangeKind::Insertion)
    );
}

#[test]
fn no_anchor_fallback_preserves_prior_candidate_dp_behavior() {
    // Both sides contain short text under anchor_min_tokens (12)
    let old = vec![block_text(1, "alpha one"), block_text(2, "beta two")];
    let new = vec![block_text(101, "alpha one"), block_text(102, "beta two")];

    let alignment = align(old, new);

    assert!(
        alignment.main_anchors.is_empty(),
        "Short blocks must not form anchors"
    );
    assert_eq!(alignment.spans.len(), 2);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);
    assert_eq!(alignment.spans[0].new, [BlockId(101)]);
    assert_eq!(alignment.spans[1].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[1].old, [BlockId(2)]);
    assert_eq!(alignment.spans[1].new, [BlockId(102)]);
}

#[test]
fn split_merge_grouping_is_recoverable_inside_anchor_bounded_interval() {
    // 2:1 merge between anchors
    let old = vec![
        block_text(1, OPENING),
        block_text(2, "project"),
        block_text(3, "log"),
        block_text(4, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, "project log"),
        block_text(103, CLOSING),
    ];

    let alignment = align(old, new);

    assert_eq!(alignment.main_anchors.len(), 2);
    assert_eq!(alignment.spans.len(), 3);
    assert_eq!(alignment.spans[0].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[0].old, [BlockId(1)]);
    assert_eq!(alignment.spans[0].new, [BlockId(101)]);

    let merge_span = &alignment.spans[1];
    assert_eq!(merge_span.kind, AlignmentKind::Match);
    assert_eq!(merge_span.old, [BlockId(2), BlockId(3)]);
    assert_eq!(merge_span.new, [BlockId(102)]);
    assert!(merge_span.evidence.contains(&AlignmentEvidence::SplitMerge));
    assert!(
        merge_span
            .evidence
            .contains(&AlignmentEvidence::AnchorInterval)
    );

    assert_eq!(alignment.spans[2].kind, AlignmentKind::Match);
    assert_eq!(alignment.spans[2].old, [BlockId(4)]);
    assert_eq!(alignment.spans[2].new, [BlockId(103)]);
}

#[test]
fn ordinary_ordered_match_cannot_cross_trusted_anchor_boundary() {
    // Short non-anchor text (under anchor_min_tokens = 12)
    let non_anchor_old = "short line A";
    let non_anchor_new = "short line B";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, non_anchor_old),
        block_text(3, CLOSING),
        block_text(4, "End of document tail section content text"),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, CLOSING),
        block_text(103, non_anchor_new),
        block_text(104, "End of document tail section content text"),
    ];

    let alignment = align(old, new);

    assert_eq!(alignment.main_anchors.len(), 3);
    assert_eq!(
        alignment.main_anchors[0],
        ExactAnchor {
            old: BlockId(1),
            new: BlockId(101)
        }
    );
    assert_eq!(
        alignment.main_anchors[1],
        ExactAnchor {
            old: BlockId(3),
            new: BlockId(102)
        }
    );
    assert_eq!(
        alignment.main_anchors[2],
        ExactAnchor {
            old: BlockId(4),
            new: BlockId(104)
        }
    );

    // Because block 2 is in Window 1 (before Anchor 3 in old) and block 103 is in Window 2 (after Anchor 3 in new):
    // The match cannot cross the anchor boundary; instead, block 2 is deletion and block 103 is insertion.
    assert!(
        alignment
            .spans
            .iter()
            .any(|s| s.kind == AlignmentKind::Deletion && s.old == [BlockId(2)])
    );
    assert!(
        alignment
            .spans
            .iter()
            .any(|s| s.kind == AlignmentKind::Insertion && s.new == [BlockId(103)])
    );
}

#[test]
fn relocated_paragraph_across_stable_anchors_promotes_to_move_change() {
    let moved_text = "Relocated Unique Paragraph Content Alpha 01 Number 999";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, moved_text),
        block_text(3, "Stable Intermediate Paragraph Section Alpha One"),
        block_text(4, "Stable Intermediate Paragraph Section Beta Two"),
        block_text(5, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(103, "Stable Intermediate Paragraph Section Alpha One"),
        block_text(104, "Stable Intermediate Paragraph Section Beta Two"),
        block_text(105, CLOSING),
        block_text(102, moved_text),
    ];

    let alignment = align(old.clone(), new.clone());

    // Monotone main chain preserves anchors 1, 3, 4, 5 (length 4)
    assert_eq!(alignment.main_anchors.len(), 4);
    // Relocated block 2 <-> 102 is preserved in move_candidates
    assert_eq!(alignment.move_candidates.len(), 1);
    assert_eq!(
        alignment.move_candidates[0],
        ExactAnchor {
            old: BlockId(2),
            new: BlockId(102)
        }
    );

    // Downstream compare_aligned promotes it to ChangeKind::Move
    let diff = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("diff comparison should succeed");

    let moves = diff
        .changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Move)
        .collect::<Vec<_>>();
    assert_eq!(
        moves.len(),
        1,
        "Relocated unique paragraph must promote to Move"
    );
    assert!(moves[0].old_span.is_some());
    assert!(moves[0].new_span.is_some());
}

#[test]
fn ordinary_insertion_and_deletion_are_not_promoted_to_move() {
    let old = vec![
        block_text(1, OPENING),
        block_text(2, "Original distinct paragraph content to be deleted"),
        block_text(3, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, "Brand new distinct paragraph content to be inserted"),
        block_text(103, CLOSING),
    ];

    let alignment = align(old.clone(), new.clone());
    assert!(alignment.move_candidates.is_empty());

    let diff = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("diff should succeed");
    assert!(!diff.changes.iter().any(|c| c.kind == ChangeKind::Move));
}

#[test]
fn repeated_identical_paragraphs_remain_ambiguous_without_false_move() {
    let duplicate_text = "Repeated disclaimer paragraph text across the document";
    let old = vec![
        block_text(1, OPENING),
        block_text(2, duplicate_text),
        block_text(3, duplicate_text),
        block_text(4, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, CLOSING),
        block_text(103, duplicate_text),
    ];

    let alignment = align(old.clone(), new.clone());
    // Duplicate text is excluded from exact anchors and move candidates
    assert!(alignment.move_candidates.is_empty());

    let diff = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("diff should succeed");
    assert!(
        !diff.changes.iter().any(|c| c.kind == ChangeKind::Move),
        "Ambiguous duplicate text must never be promoted to Move"
    );
}

#[test]
fn moved_paragraph_with_internal_edit_remains_exact_diff_eligible() {
    let old = vec![
        block_text(1, OPENING),
        block_text(2, "Unique Paragraph Initial Version Alpha Beta Gamma"),
        block_text(3, CLOSING),
    ];
    let new = vec![
        block_text(101, OPENING),
        block_text(102, CLOSING),
        block_text(103, "Unique Paragraph Initial Version Alpha Delta Gamma"),
    ];

    let alignment = align(old.clone(), new.clone());
    let diff = compare_aligned(&old, &new, &alignment, DiffOptions::default())
        .expect("diff should succeed");

    // Non-exact match is not promoted to Move; instead it is evaluated via exact diff
    assert!(!diff.changes.iter().any(|c| c.kind == ChangeKind::Move));
    assert!(
        diff.changes
            .iter()
            .any(|c| c.kind == ChangeKind::Deletion || c.kind == ChangeKind::Insertion)
    );
}

#[test]
fn degraded_masked_split_and_merge_compare_successfully_without_separator_error() {
    let old1 = block_text_with_matching(
        1,
        "The archive count: 10 files.",
        "The archive count: <NUM> files.",
        true,
    );
    let old2 = block_text_with_matching(2, "log 10 files", "log <NUM> files", true);

    let new1 = block_text_with_matching(101, "The archive", "The archive", false);
    let new2 = block_text_with_matching(102, "count: 20 files.", "count: <NUM> files.", true);

    let old1_tokens = old1.canonical.text.chars().count();
    let old2_tokens = old2.canonical.text.chars().count();
    let new1_tokens = new1.canonical.text.chars().count();
    let new2_tokens = new2.canonical.text.chars().count();

    // 1. Split degraded to Unresolved (old: 1, new: 101, 102)
    let alignment_split = Alignment {
        spans: vec![AlignmentSpan {
            kind: AlignmentKind::Unresolved,
            old: vec![BlockId(1)],
            new: vec![BlockId(101), BlockId(102)],
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: vec![
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::NumericMask,
                AlignmentEvidence::SplitMerge,
            ],
            old_separator: None,
            new_separator: None,
        }],
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
    };

    let diff_split = compare_aligned(
        std::slice::from_ref(&old1),
        &[new1.clone(), new2],
        &alignment_split,
        DiffOptions::default(),
    )
    .expect("diff comparison on degraded split must succeed with Unresolved outcome");

    assert_eq!(diff_split.unresolved_regions.len(), 1);
    assert!(diff_split.changes.is_empty());
    assert_eq!(diff_split.old_coverage.total_tokens, old1_tokens);
    assert_eq!(
        diff_split.new_coverage.total_tokens,
        new1_tokens + new2_tokens
    );

    // 2. Merge degraded to Unresolved (old: 1, 2, new: 101)
    let alignment_merge = Alignment {
        spans: vec![AlignmentSpan {
            kind: AlignmentKind::Unresolved,
            old: vec![BlockId(1), BlockId(2)],
            new: vec![BlockId(101)],
            score: 0.0,
            canonical_similarity: 0.0,
            score_margin: None,
            confidence: AlignmentConfidence::Low,
            evidence: vec![
                AlignmentEvidence::TextSimilarity,
                AlignmentEvidence::NumericMask,
                AlignmentEvidence::SplitMerge,
            ],
            old_separator: None,
            new_separator: None,
        }],
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
    };

    let diff_merge = compare_aligned(
        &[old1, old2],
        std::slice::from_ref(&new1),
        &alignment_merge,
        DiffOptions::default(),
    )
    .expect("diff comparison on degraded merge must succeed with Unresolved outcome");

    assert_eq!(diff_merge.unresolved_regions.len(), 1);
    assert!(diff_merge.changes.is_empty());
    assert_eq!(
        diff_merge.old_coverage.total_tokens,
        old1_tokens + old2_tokens
    );
    assert_eq!(diff_merge.new_coverage.total_tokens, new1_tokens);
}
