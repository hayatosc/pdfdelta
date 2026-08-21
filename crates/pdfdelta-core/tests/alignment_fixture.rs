use std::cell::RefCell;

use pdfdelta_core::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentOptions,
        BlockFeatures, BlockSeparator, Candidate, CandidateGenerator, CandidateSource, ExactAnchor,
        ExactHash, InvertedIndexCandidateGenerator, align_ordered, build_block_features,
    },
    diff::{DiffOptions, compare_aligned},
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

fn align(old: Vec<BlockText>, new: Vec<BlockText>) -> Alignment {
    let old = build_block_features(&old, 3).expect("old features should build");
    let new = build_block_features(&new, 3).expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("candidate index should build");
    align_ordered(&old, &new, &generator, options()).expect("alignment should succeed")
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
    }
}

fn mapped_text(text: &str) -> MappedText {
    MappedText {
        text: text.to_owned(),
        source_map: vec![],
        unmapped: vec![],
    }
}
