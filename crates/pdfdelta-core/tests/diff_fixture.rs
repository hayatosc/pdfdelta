use pdfdelta_core::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        BlockSeparator, ExactAnchor,
    },
    diff::{
        ChangeKind, ChangeTag, ComparisonAssumption, Confidence, DiffOptions, FormattingReason,
        TokenRange, compare_aligned,
    },
    layout::BlockId,
    model::{FontProgramHash, Vec2},
    normalize::{
        BlockText, ComparableToken, FontSizeSignature, MappedText, PositionSignature, ScalarRange,
        TextSource, UnmappedToken,
    },
};

#[test]
fn exact_diff_never_uses_masked_matching_text() -> Result<()> {
    let old = block_with_matching(1, "Count: 10 files", "Count: <NUM> files", true);
    let new = block_with_matching(101, "Count: 20 files", "Count: <NUM> files", true);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].occurrences.len(), 1);
    assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(
        result.changes[0].occurrences[0]
            .old_span
            .as_ref()
            .expect("replacement should have an old span")
            .canonical_range,
        ScalarRange { start: 7, end: 8 }
    );
    assert_eq!(
        result.changes[0].occurrences[0]
            .new_span
            .as_ref()
            .expect("replacement should have a new span")
            .canonical_range,
        ScalarRange { start: 7, end: 8 }
    );
    assert_eq!(result.old_coverage.ratio, Some(1.0));
    assert_eq!(result.new_coverage.ratio, Some(1.0));
    Ok(())
}

#[test]
fn ambiguous_positions_retain_non_owning_change_count_claims() -> Result<()> {
    let result = compare_aligned(
        &[block(1, "a")],
        &[block(101, "aa")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;
    let assessment = result
        .assessment
        .as_ref()
        .expect("comparison has an assessment");
    let unit = assessment
        .review_units
        .iter()
        .find(|unit| unit.changed_count.is_some())
        .expect("closed ambiguous domain retains a count claim");
    assert_eq!(
        unit.changed_count,
        Some(pdfdelta_core::diff::EditCountBounds { lower: 1, upper: 1 })
    );
    assert_eq!(unit.unresolved_changed_count, unit.changed_count);
    assert!(unit.mandatory_old.is_empty());
    assert!(unit.mandatory_new.is_empty());
    assert_eq!(
        unit.search,
        pdfdelta_core::diff::SearchCompleteness::Complete
    );
    assert!(result.changes.is_empty());
    assert!(
        assessment
            .old_resolution
            .iter()
            .chain(&assessment.new_resolution)
            .all(|range| range.state == pdfdelta_core::diff::ResolutionState::Unresolved)
    );
    assessment.validate(&result)?;
    Ok(())
}

#[test]
fn mandatory_source_ranges_do_not_claim_ambiguous_repeated_characters() -> Result<()> {
    let result = compare_aligned(
        &[block(1, "a")],
        &[block(101, "baac")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;
    let assessment = result
        .assessment
        .as_ref()
        .expect("comparison has an assessment");
    let unit = assessment
        .review_units
        .iter()
        .find(|unit| unit.changed_count.is_some())
        .expect("closed ambiguous domain retains claims");
    assert_eq!(
        unit.changed_count,
        Some(pdfdelta_core::diff::EditCountBounds { lower: 3, upper: 3 })
    );
    assert!(unit.mandatory_old.is_empty());
    let ranges = unit
        .mandatory_new
        .iter()
        .map(|span| span.comparable_range)
        .collect::<Vec<_>>();
    assert_eq!(
        ranges,
        vec![
            TokenRange { start: 0, end: 1 },
            TokenRange { start: 3, end: 4 }
        ]
    );
    assert!(result.changes.is_empty());
    Ok(())
}

#[test]
fn review_count_detects_a_difference_with_equal_token_multisets() -> Result<()> {
    let result = compare_aligned(
        &[block(1, "red blue red")],
        &[block(101, "red red blue")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;
    assert!(result.changes.is_empty());
    assert!(result.proven_changed_regions.is_empty());
    let summary = pdfdelta_core::report::summarize(
        &result,
        &pdfdelta_core::report::ExtractionStatus::default(),
    )?;
    assert_eq!(
        summary.difference_status,
        pdfdelta_core::report::DifferenceStatus::Detected
    );
    assert!(!summary.comparison_complete);
    Ok(())
}

#[test]
fn date_replacement_keeps_literal_mask_variation_unlocalized() -> Result<()> {
    let prefix = "W3C Working Draft ";
    let old_date = "27 September 2006";
    let new_date = "02 November 2006";
    let old = block(1, &format!("{prefix}{old_date}"));
    let new = block(101, &format!("{prefix}{new_date}"));

    let result = compare_aligned(
        &[old],
        &[new],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    assert!(result.changes.is_empty());
    let assessment = result
        .assessment
        .as_ref()
        .expect("comparison has an assessment");
    let unit = assessment
        .review_units
        .iter()
        .find(|unit| unit.changed_count.is_some())
        .expect("ambiguous date replacement retains a count claim");
    assert_eq!(unit.unresolved_changed_count, unit.changed_count);
    assert_eq!(
        unit.mandatory_old
            .iter()
            .map(|span| span.comparable_range)
            .collect::<Vec<_>>(),
        vec![
            TokenRange { start: 19, end: 20 },
            TokenRange { start: 21, end: 22 },
            TokenRange { start: 23, end: 25 },
        ]
    );
    assert_eq!(
        unit.mandatory_new
            .iter()
            .map(|span| span.comparable_range)
            .collect::<Vec<_>>(),
        vec![
            TokenRange { start: 18, end: 19 },
            TokenRange { start: 21, end: 24 },
        ]
    );
    assert_eq!(result.change_candidates.len(), 4);
    assert_eq!(
        result
            .change_candidates
            .iter()
            .map(|candidate| {
                let occurrence = &candidate.change.occurrences[0];
                (
                    occurrence
                        .old_span
                        .as_ref()
                        .expect("candidate has an old span")
                        .comparable_range,
                    occurrence
                        .new_span
                        .as_ref()
                        .expect("candidate has a new span")
                        .comparable_range,
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                TokenRange { start: 18, end: 18 },
                TokenRange { start: 18, end: 19 }
            ),
            (
                TokenRange { start: 19, end: 20 },
                TokenRange { start: 20, end: 20 }
            ),
            (
                TokenRange { start: 21, end: 22 },
                TokenRange { start: 21, end: 24 }
            ),
            (
                TokenRange { start: 23, end: 26 },
                TokenRange { start: 25, end: 25 }
            ),
        ]
    );
    Ok(())
}

#[test]
fn does_not_extend_a_mixed_replacement_at_asymmetric_word_boundaries() -> Result<()> {
    for (old_text, new_text, expected) in [
        ("XaBTAIL", "a!TAIL", vec![(0..1, 0..0), (2..3, 1..2)]),
        ("a!TAIL", "XaBTAIL", vec![(0..0, 0..1), (1..2, 2..3)]),
    ] {
        let result = compare_aligned(
            &[block(1, old_text)],
            &[block(101, new_text)],
            &aligned(vec![matched(&[1], &[101])]),
            DiffOptions::default(),
        )?;

        assert_eq!(result.changes.len(), 1, "{old_text:?} -> {new_text:?}");
        let change = &result.changes[0];
        assert_eq!(change.kind, ChangeKind::Replacement);
        let actual = change
            .occurrences
            .iter()
            .map(|occurrence| {
                (
                    occurrence
                        .old_span
                        .as_ref()
                        .expect("replacement should have an old span")
                        .canonical_range,
                    occurrence
                        .new_span
                        .as_ref()
                        .expect("replacement should have a new span")
                        .canonical_range,
                )
            })
            .map(|(old, new)| (old.start..old.end, new.start..new.end))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
    Ok(())
}

#[test]
fn does_not_extend_mixed_replacements_through_non_ascii_boundaries() -> Result<()> {
    for (old_text, new_text) in [
        ("Xあ本文は同じ", "あY本文は同じ"),
        ("Xe\u{301}TAIL", "eY\u{301}TAIL"),
    ] {
        let result = compare_aligned(
            &[block(1, old_text)],
            &[block(101, new_text)],
            &aligned(vec![matched(&[1], &[101])]),
            DiffOptions::default(),
        )?;

        assert_eq!(result.changes.len(), 1, "{old_text:?} -> {new_text:?}");
        let change = &result.changes[0];
        assert_eq!(change.kind, ChangeKind::Replacement);
        let actual = change
            .occurrences
            .iter()
            .map(|occurrence| {
                (
                    occurrence
                        .old_span
                        .as_ref()
                        .expect("replacement should have an old span")
                        .canonical_range,
                    occurrence
                        .new_span
                        .as_ref()
                        .expect("replacement should have a new span")
                        .canonical_range,
                )
            })
            .map(|(old, new)| (old.start..old.end, new.start..new.end))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![(0..1, 0..0), (2..2, 1..2)],
            "{old_text:?} -> {new_text:?}"
        );
    }
    Ok(())
}

#[test]
fn tags_replacements_explained_only_by_character_width() -> Result<()> {
    for (old_text, new_text) in [
        ("ＡＢＣ", "ABC"),
        ("１２", "12"),
        ("ｶﾞ", "ガ"),
        ("ﾊﾟ", "パ"),
        ("xＡy", "xAy"),
        ("\u{3000}", " "),
    ] {
        let result = compare_aligned(
            &[block(1, old_text)],
            &[block(101, new_text)],
            &aligned(vec![matched(&[1], &[101])]),
            DiffOptions::default(),
        )?;

        assert_eq!(result.changes.len(), 1, "{old_text:?} -> {new_text:?}");
        assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
        assert_eq!(
            result.changes[0].tags,
            [ChangeTag::CharacterWidth],
            "{old_text:?} -> {new_text:?}"
        );
    }
    Ok(())
}

#[test]
fn does_not_tag_other_compatibility_or_semantic_replacements() -> Result<()> {
    for (old_text, new_text) in [
        ("①", "1"),
        ("Ⅳ", "IV"),
        ("ﬁ", "fi"),
        ("²", "2"),
        ("㍑", "リットル"),
        ("Ａ①", "A1"),
        ("ガ", "カ"),
    ] {
        let result = compare_aligned(
            &[block(1, old_text)],
            &[block(101, new_text)],
            &aligned(vec![matched(&[1], &[101])]),
            DiffOptions::default(),
        )?;

        assert!(!result.changes.is_empty(), "{old_text:?} -> {new_text:?}");
        assert!(
            result.changes.iter().all(|change| change.tags.is_empty()),
            "{old_text:?} -> {new_text:?}: {:#?}",
            result.changes
        );
    }
    Ok(())
}

#[test]
fn does_not_tag_replacements_with_unmapped_tokens() -> Result<()> {
    let result = compare_aligned(
        &[unmapped_block(1, 10)],
        &[unmapped_block(101, 20)],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
    assert!(result.changes[0].tags.is_empty());
    Ok(())
}

#[test]
fn retains_an_exact_move_candidate_without_crossing_evidence() -> Result<()> {
    let old = block(1, "Moved unique paragraph");
    let new = block(101, "Moved unique paragraph");
    let mut deletion = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut insertion = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion.evidence.push(AlignmentEvidence::MoveCandidate);
    let alignment = Alignment {
        spans: vec![deletion, insertion],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.change_candidates.len(), 1);
    assert_eq!(result.change_candidates[0].change.kind, ChangeKind::Move);
    assert_eq!(
        result.change_candidates[0].change.confidence,
        Confidence::Medium
    );
    assert_eq!(result.change_candidates[0].change.occurrences.len(), 1);
    assert_eq!(
        result.change_candidates[0].change.occurrences[0]
            .old_span
            .as_ref()
            .expect("move candidate should retain its old span")
            .blocks,
        [BlockId(1)]
    );
    assert_eq!(
        result.change_candidates[0].change.occurrences[0]
            .new_span
            .as_ref()
            .expect("move candidate should retain its new span")
            .blocks,
        [BlockId(101)]
    );
    assert!(
        result.formatting_changes.is_empty(),
        "Exact identical raw move must not emit formatting changes"
    );
    assert_eq!(result.unresolved_regions.len(), 2);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn retains_a_non_exact_move_as_deletion_and_insertion_candidates() -> Result<()> {
    let old = block(1, "Old paragraph");
    let new = block(101, "New paragraph");
    let mut deletion = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut insertion = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion.evidence.push(AlignmentEvidence::MoveCandidate);
    let alignment = Alignment {
        spans: vec![deletion, insertion],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(
        result
            .change_candidates
            .iter()
            .map(|candidate| candidate.change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Deletion, ChangeKind::Insertion]
    );
    assert_eq!(result.unresolved_regions.len(), 2);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn change_ranges_use_unicode_scalar_indices() -> Result<()> {
    let old = block_with_matching(1, "é10", "same alignment key", false);
    let new = block_with_matching(101, "é20", "same alignment key", false);
    let result = compare_aligned(
        &[old],
        &[new],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    let change = &result.changes[0];
    assert_eq!(
        change.occurrences[0]
            .old_span
            .as_ref()
            .expect("replacement should have an old span")
            .canonical_range,
        ScalarRange { start: 1, end: 2 }
    );
    Ok(())
}

#[test]
fn reports_block_split_as_formatting_only() -> Result<()> {
    let old = block(1, "project log");
    let new = [block(101, "project"), block(102, "log")];
    let mut span = matched(&[1], &[101, 102]);
    span.new_separator = Some(BlockSeparator::Space);
    span.evidence.push(AlignmentEvidence::SplitMerge);

    let result = compare_aligned(&[old], &new, &aligned(vec![span]), DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(result.formatting_changes[0].old_span.separator, None);
    assert_eq!(
        result.formatting_changes[0].new_span.separator,
        Some(BlockSeparator::Space)
    );
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::BlockStructure]
    );
    assert_eq!(result.old_coverage.total_tokens, 11);
    assert_eq!(result.new_coverage.total_tokens, 10);
    assert_eq!(result.new_coverage.resolved_tokens, 10);
    Ok(())
}

#[test]
fn reports_moved_page_break_as_formatting_only() -> Result<()> {
    let old = block_with_page_breaks(1, "alpha beta gamma", &[0, 1], &[6]);
    let new = block_with_page_breaks(101, "alpha beta gamma", &[4, 5], &[11]);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::PageBreak]
    );
    Ok(())
}

#[test]
fn reports_moved_line_break_as_formatting_only() -> Result<()> {
    let old = block_with_line_breaks(1, "alpha beta gamma", &[6]);
    let new = block_with_line_breaks(101, "alpha beta gamma", &[11]);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::LineBreak]
    );
    Ok(())
}

#[test]
fn reports_font_size_change_as_formatting_only() -> Result<()> {
    let old = block_with_font_size(1, "stable text", 10.0);
    let new = block_with_font_size(101, "stable text", 14.0);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::FontSize]
    );
    Ok(())
}

#[test]
fn reports_uniform_position_translation_as_formatting_only() -> Result<()> {
    let old = block_with_position(1, "stable text", 0.0, 100.0);
    let new = block_with_position(101, "stable text", 66.0, 100.0);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::Position]
    );
    Ok(())
}

#[test]
fn does_not_guess_position_change_for_non_translation_geometry() -> Result<()> {
    let old = block_with_position(1, "stable text", 0.0, 100.0);
    let mut new = block_with_position(101, "stable text", 66.0, 100.0);
    new.position_signatures
        .as_mut()
        .expect("fixture has position evidence")[3] = position(85.0, 100.0);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert!(result.formatting_changes.is_empty());
    Ok(())
}

#[test]
fn block_separator_preserves_equal_font_size_evidence() -> Result<()> {
    let old = block_with_font_size(1, "project log", 10.0);
    let new = [
        block_with_font_size(101, "project", 10.0),
        block_with_font_size(102, "log", 10.0),
    ];
    let mut span = matched(&[1], &[101, 102]);
    span.new_separator = Some(BlockSeparator::Space);

    let result = compare_aligned(&[old], &new, &aligned(vec![span]), DiffOptions::default())?;

    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::BlockStructure]
    );
    Ok(())
}

#[test]
fn ignores_uniform_page_number_shift() -> Result<()> {
    let old = block_with_page_breaks(1, "stable text", &[0], &[]);
    let new = block_with_page_breaks(101, "stable text", &[9], &[]);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert!(result.formatting_changes.is_empty());
    Ok(())
}

#[test]
fn rejects_inconsistent_page_break_metadata() {
    let old = block_with_page_breaks(1, "stable text", &[0, 1], &[]);
    let new = block_with_page_breaks(101, "stable text", &[0], &[]);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    assert!(matches!(
        compare_aligned(&[old], &[new], &alignment, DiffOptions::default()),
        Err(Error::Unresolved(message)) if message.contains("page-break count")
    ));
}

#[test]
fn rejects_invalid_line_break_offset() {
    let old = block_with_line_breaks(1, "stable text", &[0]);
    let new = block_with_line_breaks(101, "stable text", &[]);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    assert!(matches!(
        compare_aligned(&[old], &[new], &alignment, DiffOptions::default()),
        Err(Error::Unresolved(message)) if message.contains("line-break offsets")
    ));
}

#[test]
fn rejects_incomplete_font_size_signature_vector() {
    let mut old = block_with_font_size(1, "stable text", 10.0);
    old.font_size_signatures
        .as_mut()
        .expect("fixture has font-size evidence")
        .pop();
    let new = block_with_font_size(101, "stable text", 10.0);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    assert!(matches!(
        compare_aligned(&[old], &[new], &alignment, DiffOptions::default()),
        Err(Error::Unresolved(message)) if message.contains("font-size signature count")
    ));
}

#[test]
fn rejects_incomplete_position_signature_vector() {
    let old = block_with_position(1, "stable text", 0.0, 100.0);
    let mut new = block_with_position(101, "stable text", 0.0, 100.0);
    new.position_signatures
        .as_mut()
        .expect("fixture has position evidence")
        .pop();
    let alignment = aligned(vec![matched(&[1], &[101])]);

    assert!(matches!(
        compare_aligned(&[old], &[new], &alignment, DiffOptions::default()),
        Err(Error::Unresolved(message)) if message.contains("position signature count")
    ));
}

#[test]
fn reports_page_transition_between_aligned_blocks() -> Result<()> {
    let old = [
        block_with_page_breaks(1, "alpha", &[0], &[]),
        block_with_page_breaks(2, "beta", &[0], &[]),
    ];
    let new = [
        block_with_page_breaks(101, "alpha", &[7], &[]),
        block_with_page_breaks(102, "beta", &[8], &[]),
    ];
    let mut span = matched(&[1, 2], &[101, 102]);
    span.old_separator = Some(BlockSeparator::Space);
    span.new_separator = Some(BlockSeparator::Space);

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::PageBreak]
    );
    Ok(())
}

#[test]
fn content_spans_retain_the_exact_multi_block_separator() -> Result<()> {
    for (separator, old) in [
        (
            BlockSeparator::Concatenate,
            [block(1, "alpha c"), block(2, "at")],
        ),
        (BlockSeparator::Space, [block(1, "alpha"), block(2, "cat")]),
    ] {
        let mut span = matched(&[1, 2], &[101]);
        span.old_separator = Some(separator);
        let result = compare_aligned(
            &old,
            &[block(101, "alpha cut")],
            &aligned(vec![span]),
            DiffOptions::default(),
        )?;

        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
        assert_eq!(
            result.changes[0].occurrences[0]
                .old_span
                .as_ref()
                .expect("replacement should have an old span")
                .separator,
            Some(separator)
        );
        assert_eq!(
            result.changes[0].occurrences[0]
                .new_span
                .as_ref()
                .expect("replacement should have a new span")
                .separator,
            None
        );
    }
    Ok(())
}

#[test]
fn reports_canonical_normalization_as_formatting_only() -> Result<()> {
    let old = block_with_raw(1, "cafe\u{301}", "café");
    let new = block_with_raw(101, "café", "café");
    let result = compare_aligned(
        &[old],
        &[new],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    assert!(result.changes.is_empty());
    assert_eq!(result.formatting_changes.len(), 1);
    assert_eq!(
        result.formatting_changes[0].reasons,
        [FormattingReason::Normalization]
    );
    Ok(())
}

#[test]
fn retains_unbounded_aligned_insertions_and_deletions_as_candidates() -> Result<()> {
    let old = [block(1, "A"), block(2, "removed")];
    let new = [block(101, "A"), block(102, "inserted")];
    let alignment = aligned(vec![
        matched(&[1], &[101]),
        one_sided(AlignmentKind::Deletion, &[2], &[]),
        one_sided(AlignmentKind::Insertion, &[], &[102]),
    ]);

    let result = compare_aligned(&old, &new, &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(
        result
            .change_candidates
            .iter()
            .map(|candidate| candidate.change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Deletion, ChangeKind::Insertion]
    );
    assert_eq!(result.unresolved_regions.len(), 2);
    assert_eq!(result.old_coverage.resolved_tokens, 1);
    assert_eq!(result.old_coverage.total_tokens, 8);
    assert_eq!(result.new_coverage.resolved_tokens, 1);
    assert_eq!(result.new_coverage.total_tokens, 9);
    Ok(())
}

#[test]
fn ignores_empty_one_sided_blocks_as_content_changes() -> Result<()> {
    let result = compare_aligned(
        &[],
        &[block(101, "")],
        &aligned(vec![one_sided(AlignmentKind::Insertion, &[], &[101])]),
        DiffOptions::default(),
    )?;

    assert!(result.changes.is_empty());
    assert_eq!(result.new_coverage.total_tokens, 0);
    assert_eq!(result.new_coverage.ratio, Some(1.0));
    Ok(())
}

#[test]
fn excludes_unresolved_tokens_from_side_specific_coverage() -> Result<()> {
    let old = [block(1, "A"), block(2, "hidden")];
    let new = [block(101, "A"), block(102, "unknown")];
    let alignment = aligned(vec![matched(&[1], &[101]), unresolved(&[2], &[102])]);

    let result = compare_aligned(&old, &new, &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 1);
    assert_eq!(result.old_coverage.resolved_tokens, 1);
    assert_eq!(result.old_coverage.total_tokens, 7);
    assert_eq!(result.new_coverage.resolved_tokens, 1);
    assert_eq!(result.new_coverage.total_tokens, 8);
    Ok(())
}

#[test]
fn unresolved_multi_block_spans_record_effective_concatenate_separator() -> Result<()> {
    let old = [block(1, "alpha"), block(2, "beta")];
    let result = compare_aligned(
        &old,
        &[block(101, "unknown")],
        &aligned(vec![unresolved(&[1, 2], &[101])]),
        DiffOptions::default(),
    )?;

    let region = &result.unresolved_regions[0];
    assert_eq!(
        region
            .old_span
            .as_ref()
            .expect("unresolved region should have an old span")
            .separator,
        Some(BlockSeparator::Concatenate)
    );
    assert_eq!(
        region
            .new_span
            .as_ref()
            .expect("unresolved region should have a new span")
            .separator,
        None
    );
    Ok(())
}

#[test]
fn preserves_unmapped_identity_and_ranges() -> Result<()> {
    let unchanged_old = unmapped_block(1, 7);
    let unchanged_new = unmapped_block(101, 7);
    let unchanged = compare_aligned(
        &[unchanged_old],
        &[unchanged_new],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;
    assert!(unchanged.changes.is_empty());

    let changed = compare_aligned(
        &[unmapped_block(1, 7)],
        &[unmapped_block(101, 8)],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;
    let change = &changed.changes[0];
    let old_span = change.occurrences[0]
        .old_span
        .as_ref()
        .expect("unmapped replacement should have an old span");
    assert_eq!(old_span.canonical_range, ScalarRange { start: 0, end: 0 });
    assert_eq!(old_span.comparable_range, TokenRange { start: 0, end: 1 });
    Ok(())
}

#[test]
fn coalesces_each_contiguous_edit_run() -> Result<()> {
    let result = compare_aligned(
        &[block(1, "abXcdYef")],
        &[block(101, "abQcdRef")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    assert_eq!(result.changes.len(), 2);
    assert!(
        result
            .changes
            .iter()
            .all(|change| change.kind == ChangeKind::Replacement)
    );
    Ok(())
}

#[test]
fn groups_replacements_across_a_single_equal_scalar() -> Result<()> {
    let result = compare_aligned(
        &[block(1, "abXcYef")],
        &[block(101, "abQcRef")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    assert_eq!(result.changes.len(), 1);
    assert_eq!(
        result.changes[0]
            .occurrences
            .iter()
            .map(|occurrence| {
                (
                    occurrence
                        .old_span
                        .as_ref()
                        .expect("replacement should have an old span")
                        .canonical_range,
                    occurrence
                        .new_span
                        .as_ref()
                        .expect("replacement should have a new span")
                        .canonical_range,
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                ScalarRange { start: 2, end: 3 },
                ScalarRange { start: 2, end: 3 }
            ),
            (
                ScalarRange { start: 4, end: 5 },
                ScalarRange { start: 4, end: 5 }
            ),
        ]
    );
    Ok(())
}

#[test]
fn excludes_trailing_equal_tokens_from_a_grouped_replacement() -> Result<()> {
    let result = compare_aligned(
        &[block(1, "abXcYtail")],
        &[block(101, "abQcRtail")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    )?;

    assert_eq!(result.changes.len(), 1);
    assert_eq!(
        result.changes[0]
            .occurrences
            .iter()
            .map(|occurrence| {
                (
                    occurrence
                        .old_span
                        .as_ref()
                        .expect("replacement should have an old span")
                        .canonical_range,
                    occurrence
                        .new_span
                        .as_ref()
                        .expect("replacement should have a new span")
                        .canonical_range,
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                ScalarRange { start: 2, end: 3 },
                ScalarRange { start: 2, end: 3 }
            ),
            (
                ScalarRange { start: 4, end: 5 },
                ScalarRange { start: 4, end: 5 }
            ),
        ]
    );
    Ok(())
}

#[test]
fn degrades_a_weak_match_whose_tokens_mostly_changed() -> Result<()> {
    let old = [block(1, "aaaabbbbccccdddd")];
    let new = [block(101, "aaaaXXXXXXXXYYYY")];
    let mut span = matched(&[1], &[101]);
    span.score = 0.6;
    span.canonical_similarity = 0.6;
    span.confidence = AlignmentConfidence::Low;

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 1);
    assert_eq!(
        result.unresolved_regions[0].evidence,
        [
            AlignmentEvidence::TextSimilarity,
            AlignmentEvidence::DiffRejectedAsImplausible,
        ]
    );
    // The degraded span must not count as resolved coverage.
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn retains_a_short_clean_replacement_at_the_exact_ratio_limit() -> Result<()> {
    // "Xaaa" -> "Yaaa": one hunk over four tokens, changed ratio exactly at
    // the allowed limit. The hunk density must not degrade it (issue #6
    // review: one hunk / four tokens exceeded the density ceiling).
    let old = [block(1, "Xaaa")];
    let new = [block(101, "Yaaa")];
    let mut span = matched(&[1], &[101]);
    span.score = 0.6;
    span.canonical_similarity = 0.6;
    span.confidence = AlignmentConfidence::Low;

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    assert!(result.unresolved_regions.is_empty());
    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(result.changes[0].confidence, Confidence::Low);
    assert_eq!(result.old_coverage.resolved_tokens, 4);
    assert_eq!(result.new_coverage.resolved_tokens, 4);
    Ok(())
}

#[test]
fn degrades_a_weak_match_fragmented_into_many_tiny_hunks() -> Result<()> {
    let old = [block(1, "aaaXaaaXaaaXaaaXaaa")];
    let new = [block(101, "aaaYaaaYaaaYaaaYaaa")];
    let mut span = matched(&[1], &[101]);
    span.score = 0.6;
    span.canonical_similarity = 0.6;
    span.confidence = AlignmentConfidence::Low;

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    // The changed-token ratio alone stays under the configured limit; the
    // hunk density is what exposes this as character-fragment soup.
    assert!(result.changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 1);
    Ok(())
}

#[test]
fn retains_a_weak_match_with_sparse_edits_as_uncertain_changes() -> Result<()> {
    let old = [block(
        1,
        "The quick brown fox jumps over the lazy dog tonight",
    )];
    let new = [block(
        101,
        "The quick brown fox jumps over the lazy cat tomorrow",
    )];
    let mut span = matched(&[1], &[101]);
    span.score = 0.8;
    span.canonical_similarity = 0.8;
    span.confidence = AlignmentConfidence::Low;

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.change_candidates.len(), 2);
    assert!(result.change_candidates.iter().all(|candidate| {
        candidate.change.kind == ChangeKind::Replacement
            && candidate.change.confidence == Confidence::Low
    }));
    assert_eq!(result.unresolved_regions.len(), 1);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn degrades_an_implausible_match_despite_strong_alignment_evidence() -> Result<()> {
    let old = [block(1, "aaaabbbbccccdddd")];
    let new = [block(101, "aaaaXXXXXXXXYYYY")];
    let mut span = matched(&[1], &[101]);
    span.score = 0.9;
    span.canonical_similarity = 0.9;

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 1);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    assert_eq!(
        result.unresolved_regions[0].evidence,
        [
            AlignmentEvidence::TextSimilarity,
            AlignmentEvidence::DiffRejectedAsImplausible,
        ]
    );
    Ok(())
}

#[test]
fn rejects_incomplete_alignment_partitions() {
    let result = compare_aligned(
        &[block(1, "A"), block(2, "B")],
        &[block(101, "A")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions::default(),
    );

    assert!(matches!(
        result,
        Err(Error::Unresolved(message)) if message.contains("assigned 1 of 2 old blocks")
    ));
}

#[test]
fn rejects_out_of_order_alignment_partitions() {
    let result = compare_aligned(
        &[block(1, "A"), block(2, "B")],
        &[block(101, "A"), block(102, "B")],
        &aligned(vec![matched(&[2], &[101]), matched(&[1], &[102])]),
        DiffOptions::default(),
    );

    assert!(matches!(
        result,
        Err(Error::Unresolved(message))
            if message.contains("source index 1, expected index 0")
    ));
}

#[test]
fn rejects_multi_block_one_sided_alignment_spans() {
    let mut insertion = one_sided(AlignmentKind::Insertion, &[], &[101, 102]);
    insertion.new_separator = Some(BlockSeparator::Space);
    let result = compare_aligned(
        &[],
        &[block(101, ""), block(102, "")],
        &aligned(vec![insertion]),
        DiffOptions::default(),
    );

    assert!(matches!(
        result,
        Err(Error::Unresolved(message))
            if message.contains("invalid Insertion alignment span shape")
    ));
}

#[test]
fn enforces_token_and_edit_distance_limits() {
    let alignment = aligned(vec![matched(&[1], &[101])]);
    let token_limited = compare_aligned(
        &[block(1, "abc")],
        &[block(101, "xyz")],
        &alignment,
        DiffOptions {
            max_tokens: 5,
            max_edit_distance: 10,
            ..DiffOptions::default()
        },
    );
    assert!(matches!(
        token_limited,
        Err(Error::LimitExceeded {
            resource: "diff comparable tokens",
            limit: 5
        })
    ));

    let distance_limited = compare_aligned(
        &[block(1, "abc")],
        &[block(101, "xyz")],
        &alignment,
        DiffOptions {
            max_tokens: 10,
            max_edit_distance: 2,
            ..DiffOptions::default()
        },
    )
    .expect("an edit distance overrun should degrade the span, not the comparison");
    assert!(distance_limited.changes.is_empty());
    assert_eq!(distance_limited.unresolved_regions.len(), 1);
    assert_eq!(
        distance_limited.unresolved_regions[0].evidence,
        [
            AlignmentEvidence::TextSimilarity,
            AlignmentEvidence::DiffEditDistanceExceeded,
        ]
    );
    assert!(
        distance_limited
            .unresolved_regions
            .first()
            .expect("the degraded span should be reported")
            .old_span
            .is_some()
    );
    // The degraded matched span must not count as resolved coverage.
    assert_eq!(distance_limited.old_coverage.total_tokens, 3);
    assert_eq!(distance_limited.old_coverage.resolved_tokens, 0);
    assert_eq!(distance_limited.new_coverage.total_tokens, 3);
    assert_eq!(distance_limited.new_coverage.resolved_tokens, 0);
}

#[test]
fn zero_edit_distance_limit_allows_only_identical_input() -> Result<()> {
    let alignment = aligned(vec![matched(&[1], &[101])]);
    let options = DiffOptions {
        max_tokens: 10,
        max_edit_distance: 0,
        ..DiffOptions::default()
    };

    let identical = compare_aligned(
        &[block(1, "abc")],
        &[block(101, "abc")],
        &alignment,
        options,
    )?;
    assert!(identical.changes.is_empty());
    assert_eq!(identical.old_coverage.resolved_tokens, 3);
    assert_eq!(identical.new_coverage.resolved_tokens, 3);

    let differing = compare_aligned(
        &[block(1, "abc")],
        &[block(101, "abd")],
        &alignment,
        options,
    )
    .expect("an edit distance overrun should degrade the span, not the comparison");
    assert!(differing.changes.is_empty());
    assert_eq!(differing.unresolved_regions.len(), 1);
    // The degraded matched span must not count as resolved coverage.
    assert_eq!(differing.old_coverage.total_tokens, 3);
    assert_eq!(differing.old_coverage.resolved_tokens, 0);
    assert_eq!(differing.new_coverage.total_tokens, 3);
    assert_eq!(differing.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn applies_the_token_budget_to_raw_evidence_before_diffing() {
    let result = compare_aligned(
        &[block_with_raw(1, "many raw layout spaces", "A")],
        &[block(101, "A")],
        &aligned(vec![matched(&[1], &[101])]),
        DiffOptions {
            max_tokens: 2,
            max_edit_distance: 2,
            ..DiffOptions::default()
        },
    );

    assert!(matches!(
        result,
        Err(Error::LimitExceeded {
            resource: "diff raw evidence tokens",
            limit: 2
        })
    ));
}

#[test]
fn empty_documents_have_complete_alignment_coverage() -> Result<()> {
    let result = compare_aligned(&[], &[], &aligned(Vec::new()), DiffOptions::default())?;

    assert_eq!(result.old_coverage.ratio, Some(1.0));
    assert_eq!(result.new_coverage.ratio, Some(1.0));
    Ok(())
}

fn block(id: u64, text: &str) -> BlockText {
    block_with_matching(id, text, text, false)
}

fn block_with_raw(id: u64, raw: &str, canonical: &str) -> BlockText {
    let mut block = block(id, canonical);
    block.raw = mapped(raw);
    block
}

fn block_with_page_breaks(id: u64, text: &str, pages: &[u32], page_breaks: &[usize]) -> BlockText {
    let mut block = block(id, text);
    block.pages = pages.to_vec();
    block.page_breaks = Some(page_breaks.to_vec());
    block
}

fn block_with_line_breaks(id: u64, text: &str, line_breaks: &[usize]) -> BlockText {
    let mut block = block(id, text);
    block.pages = vec![0];
    block.line_breaks = Some(line_breaks.to_vec());
    block.page_breaks = Some(Vec::new());
    block
}

fn block_with_font_size(id: u64, text: &str, font_size: f64) -> BlockText {
    let mut block = block(id, text);
    let signature = FontSizeSignature::new(&[font_size]).expect("fixture font size is valid");
    block.font_size_signatures = Some(vec![signature; text.chars().count()]);
    block
}

fn block_with_position(id: u64, text: &str, x: f64, y: f64) -> BlockText {
    let mut block = block(id, text);
    block.pages = vec![0];
    block.position_signatures = Some(
        text.chars()
            .enumerate()
            .map(|(index, _)| position(x + index as f64 * 6.0, y))
            .collect(),
    );
    block.line_breaks = Some(Vec::new());
    block.page_breaks = Some(Vec::new());
    block
}

fn position(x: f64, y: f64) -> PositionSignature {
    PositionSignature::new(Vec2 { x, y }, Vec2 { x: 1.0, y: 0.0 })
        .expect("fixture position is valid")
}

fn block_with_matching(
    id: u64,
    canonical: &str,
    matching: &str,
    numeric_mask_applied: bool,
) -> BlockText {
    BlockText {
        block: BlockId(id),
        role: pdfdelta_core::layout::BlockRole::Body,
        raw: mapped(canonical),
        canonical: mapped(canonical),
        matching: matching.to_owned(),
        matching_tokens: matching.chars().map(ComparableToken::Scalar).collect(),
        numeric_mask_applied,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: Vec::new(),
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: None,
        page_breaks: None,
    }
}

fn unmapped_block(id: u64, glyph_id: u16) -> BlockText {
    let mapped = MappedText {
        text: String::new(),
        source_map: Vec::new(),
        unmapped: vec![UnmappedToken {
            scalar_index: 0,
            font_hash: FontProgramHash(vec![1, 2, 3]),
            glyph_id,
            source: TextSource {
                atoms: Vec::new().into(),
            },
        }],
    };
    BlockText {
        block: BlockId(id),
        role: pdfdelta_core::layout::BlockRole::Body,
        raw: mapped.clone(),
        canonical: mapped,
        matching: String::new(),
        matching_tokens: vec![ComparableToken::Unmapped {
            font_hash: FontProgramHash(vec![1, 2, 3]),
            glyph_id,
        }],
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: Vec::new(),
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: None,
        page_breaks: None,
    }
}

fn mapped(text: &str) -> MappedText {
    MappedText {
        text: text.to_owned(),
        source_map: Vec::new(),
        unmapped: Vec::new(),
    }
}

fn aligned(spans: Vec<AlignmentSpan>) -> Alignment {
    Alignment {
        spans,
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
    }
}

fn matched(old: &[u64], new: &[u64]) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Match,
        old: ids(old),
        new: ids(new),
        score: 1.0,
        canonical_similarity: 1.0,
        score_margin: None,
        confidence: AlignmentConfidence::High,
        evidence: vec![AlignmentEvidence::TextSimilarity],
        old_separator: None,
        new_separator: None,
    }
}

fn one_sided(kind: AlignmentKind, old: &[u64], new: &[u64]) -> AlignmentSpan {
    AlignmentSpan {
        kind,
        old: ids(old),
        new: ids(new),
        score: 0.0,
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: AlignmentConfidence::Medium,
        evidence: Vec::new(),
        old_separator: None,
        new_separator: None,
    }
}

fn unresolved(old: &[u64], new: &[u64]) -> AlignmentSpan {
    AlignmentSpan {
        kind: AlignmentKind::Unresolved,
        old: ids(old),
        new: ids(new),
        score: 0.0,
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: AlignmentConfidence::Low,
        evidence: vec![AlignmentEvidence::NormalizationIssue],
        old_separator: None,
        new_separator: None,
    }
}

fn ids(values: &[u64]) -> Vec<BlockId> {
    values.iter().copied().map(BlockId).collect()
}

#[test]
fn rejects_competing_duplicate_move_candidates_as_deletion_and_insertion() -> Result<()> {
    // Block 1 (old) is associated with two different new blocks (101 and 102) -> ambiguous!
    let old = block(1, "Moved paragraph text");
    let new1 = block(101, "Moved paragraph text");
    let new2 = block(102, "Moved paragraph text");

    let mut deletion = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut insertion1 = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion1.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut insertion2 = one_sided(AlignmentKind::Insertion, &[], &[102]);
    insertion2.evidence.push(AlignmentEvidence::MoveCandidate);

    let alignment = Alignment {
        spans: vec![deletion, insertion1, insertion2],
        main_anchors: Vec::new(),
        move_candidates: vec![
            ExactAnchor {
                old: BlockId(1),
                new: BlockId(101),
            },
            ExactAnchor {
                old: BlockId(1),
                new: BlockId(102),
            },
        ],
    };

    let result = compare_aligned(&[old], &[new1, new2], &alignment, DiffOptions::default())?;

    // Must NOT promote to Move because of competing candidates and duplicate new text!
    assert!(
        !result.changes.iter().any(|c| c.kind == ChangeKind::Move),
        "Ambiguous competing move candidates must not be promoted to Move"
    );
    assert!(result.changes.is_empty());
    assert_eq!(
        result
            .change_candidates
            .iter()
            .map(|candidate| candidate.change.kind)
            .collect::<Vec<_>>(),
        [
            ChangeKind::Deletion,
            ChangeKind::Insertion,
            ChangeKind::Insertion
        ]
    );
    assert_eq!(result.unresolved_regions.len(), 3);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn rejects_repeated_identical_blocks_from_move_promotion() -> Result<()> {
    // Old has 2 identical blocks; new has 1 -> ambiguous which was moved
    let old1 = block(1, "Repeated disclaimer block");
    let old2 = block(2, "Repeated disclaimer block");
    let new = block(101, "Repeated disclaimer block");

    let mut deletion1 = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion1.evidence.push(AlignmentEvidence::MoveCandidate);
    let deletion2 = one_sided(AlignmentKind::Deletion, &[2], &[]);
    let mut insertion = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion.evidence.push(AlignmentEvidence::MoveCandidate);

    let alignment = Alignment {
        spans: vec![deletion1, deletion2, insertion],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result = compare_aligned(&[old1, old2], &[new], &alignment, DiffOptions::default())?;

    assert!(
        !result.changes.iter().any(|c| c.kind == ChangeKind::Move),
        "Duplicate blocks must not be promoted to Move"
    );
    Ok(())
}

#[test]
fn retains_a_move_with_raw_normalization_difference_as_a_candidate() -> Result<()> {
    let old = block_with_raw(
        1,
        "Moved paragraph line one\nline two",
        "Moved paragraph line one line two",
    );
    let new = block(101, "Moved paragraph line one line two");
    let mut deletion = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut insertion = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion.evidence.push(AlignmentEvidence::MoveCandidate);
    let alignment = Alignment {
        spans: vec![deletion, insertion],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.change_candidates.len(), 1);
    assert_eq!(result.change_candidates[0].change.kind, ChangeKind::Move);
    assert_eq!(
        result.change_candidates[0].change.confidence,
        Confidence::Medium
    );
    assert!(result.formatting_changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 2);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn retains_asymmetric_move_evidence_as_deletion_and_insertion_candidates() -> Result<()> {
    // Case A: Deletion lacks MoveCandidate evidence
    let old_a = block(1, "Moved paragraph text");
    let new_a = block(101, "Moved paragraph text");
    let deletion_a = one_sided(AlignmentKind::Deletion, &[1], &[]);
    let mut insertion_a = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion_a.evidence.push(AlignmentEvidence::MoveCandidate);
    let alignment_a = Alignment {
        spans: vec![deletion_a, insertion_a],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result_a = compare_aligned(&[old_a], &[new_a], &alignment_a, DiffOptions::default())?;
    assert!(
        !result_a.changes.iter().any(|c| c.kind == ChangeKind::Move),
        "Asymmetric move candidate (missing deletion evidence) must not promote to Move"
    );
    assert!(result_a.changes.is_empty());
    assert_eq!(
        result_a
            .change_candidates
            .iter()
            .map(|candidate| candidate.change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Deletion, ChangeKind::Insertion]
    );
    assert_eq!(result_a.unresolved_regions.len(), 2);

    // Case B: Insertion lacks MoveCandidate evidence
    let old_b = block(1, "Moved paragraph text");
    let new_b = block(101, "Moved paragraph text");
    let mut deletion_b = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion_b.evidence.push(AlignmentEvidence::MoveCandidate);
    let insertion_b = one_sided(AlignmentKind::Insertion, &[], &[101]);
    let alignment_b = Alignment {
        spans: vec![deletion_b, insertion_b],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result_b = compare_aligned(&[old_b], &[new_b], &alignment_b, DiffOptions::default())?;
    assert!(
        !result_b.changes.iter().any(|c| c.kind == ChangeKind::Move),
        "Asymmetric move candidate (missing insertion evidence) must not promote to Move"
    );
    assert!(result_b.changes.is_empty());
    assert_eq!(
        result_b
            .change_candidates
            .iter()
            .map(|candidate| candidate.change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Deletion, ChangeKind::Insertion]
    );
    assert_eq!(result_b.unresolved_regions.len(), 2);
    Ok(())
}

fn multi_unmapped_block(id: u64, font_hash: Vec<u8>, glyph_ids: &[u16]) -> BlockText {
    let font_hash = FontProgramHash(font_hash);
    let mut unmapped = Vec::new();
    let mut matching_tokens = Vec::new();
    for &glyph_id in glyph_ids {
        unmapped.push(UnmappedToken {
            scalar_index: 0,
            font_hash: font_hash.clone(),
            glyph_id,
            source: TextSource {
                atoms: vec![].into(),
            },
        });
        matching_tokens.push(ComparableToken::Unmapped {
            font_hash: font_hash.clone(),
            glyph_id,
        });
    }
    let mapped = MappedText {
        text: String::new(),
        source_map: vec![],
        unmapped,
    };
    BlockText {
        block: BlockId(id),
        role: pdfdelta_core::layout::BlockRole::Body,
        raw: mapped.clone(),
        canonical: mapped,
        matching: String::new(),
        matching_tokens,
        numeric_mask_applied: false,
        normalization_events: vec![],
        issues: vec![],
        pages: vec![0],
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: None,
        page_breaks: None,
    }
}

#[test]
fn exact_diff_matches_stable_unmapped_block_with_zero_changes() -> Result<()> {
    let hash = vec![0x11, 0x22, 0x33];
    let old = multi_unmapped_block(1, hash.clone(), &[100, 101, 102]);
    let new = multi_unmapped_block(101, hash, &[100, 101, 102]);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(
        result.changes.is_empty(),
        "identical unmapped tokens must produce 0 changes"
    );
    assert!(
        result.formatting_changes.is_empty(),
        "identical unmapped raw evidence must produce 0 formatting changes"
    );
    assert_eq!(result.old_coverage.ratio, Some(1.0));
    assert_eq!(result.new_coverage.ratio, Some(1.0));
    assert_eq!(result.old_coverage.total_tokens, 3);
    assert_eq!(result.old_coverage.resolved_tokens, 3);
    Ok(())
}

#[test]
fn retains_an_exact_unmapped_move_candidate_without_crossing_evidence() -> Result<()> {
    let hash = vec![0x55, 0x66, 0x77];
    let old = multi_unmapped_block(1, hash.clone(), &[10, 20, 30]);
    let new = multi_unmapped_block(101, hash, &[10, 20, 30]);
    let mut deletion = one_sided(AlignmentKind::Deletion, &[1], &[]);
    deletion.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut insertion = one_sided(AlignmentKind::Insertion, &[], &[101]);
    insertion.evidence.push(AlignmentEvidence::MoveCandidate);
    let alignment = Alignment {
        spans: vec![deletion, insertion],
        main_anchors: Vec::new(),
        move_candidates: vec![ExactAnchor {
            old: BlockId(1),
            new: BlockId(101),
        }],
    };

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert!(result.changes.is_empty());
    assert_eq!(result.change_candidates.len(), 1);
    assert_eq!(result.change_candidates[0].change.kind, ChangeKind::Move);
    assert_eq!(
        result.change_candidates[0].change.confidence,
        Confidence::Medium
    );
    assert_eq!(result.change_candidates[0].change.occurrences.len(), 1);
    assert_eq!(
        result.change_candidates[0].change.occurrences[0]
            .old_span
            .as_ref()
            .expect("move candidate should retain its old span")
            .comparable_range,
        TokenRange { start: 0, end: 3 }
    );
    assert_eq!(
        result.change_candidates[0].change.occurrences[0]
            .new_span
            .as_ref()
            .expect("move candidate should retain its new span")
            .comparable_range,
        TokenRange { start: 0, end: 3 }
    );
    assert!(result.formatting_changes.is_empty());
    assert_eq!(result.unresolved_regions.len(), 2);
    assert_eq!(result.old_coverage.resolved_tokens, 0);
    assert_eq!(result.new_coverage.resolved_tokens, 0);
    Ok(())
}

#[test]
fn confidence_calibration_spans_and_diff_changes_taxonomy() -> Result<()> {
    // 1. Exact canonical 1:1 clean match with raw normalization difference -> FormattingChange with High confidence
    let old_b1 = block_with_raw(1, "Café", "Café");
    let new_b1 = block_with_raw(101, "Cafe\u{0301}", "Café"); // combining accent in raw, identical canonical
    let span_exact_clean = AlignmentSpan {
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
    };

    // 2. Strong unique fuzzy replacement -> Change with Medium confidence
    let old_b2 = block(2, "The quick fox jumps high");
    let new_b2 = block(102, "The quick fox jumps low");
    let span_fuzzy_strong = AlignmentSpan {
        kind: AlignmentKind::Match,
        old: vec![BlockId(2)],
        new: vec![BlockId(102)],
        score: 0.90,
        canonical_similarity: 0.90,
        score_margin: Some(0.15),
        confidence: AlignmentConfidence::Medium,
        evidence: vec![AlignmentEvidence::TextSimilarity],
        old_separator: None,
        new_separator: None,
    };

    // 3. Low-margin / weak fuzzy replacement -> Change with Low confidence
    let old_b3 = block(3, "Alpha Bravo Charlie");
    let new_b3 = block(103, "Alpha Zulu Charlie");
    let span_fuzzy_weak = AlignmentSpan {
        kind: AlignmentKind::Match,
        old: vec![BlockId(3)],
        new: vec![BlockId(103)],
        score: 0.70,
        canonical_similarity: 0.70,
        score_margin: Some(0.01),
        confidence: AlignmentConfidence::Low,
        evidence: vec![AlignmentEvidence::TextSimilarity],
        old_separator: None,
        new_separator: None,
    };

    // 4. Clean split/merge -> Change or formatting with Medium confidence
    let old_b4 = block(4, "Combined paragraph line one line two");
    let new_b4a = block(104, "Combined paragraph line one");
    let new_b4b = block(105, "line two");
    let span_split = AlignmentSpan {
        kind: AlignmentKind::Match,
        old: vec![BlockId(4)],
        new: vec![BlockId(104), BlockId(105)],
        score: 1.0,
        canonical_similarity: 1.0,
        score_margin: None,
        confidence: AlignmentConfidence::Medium,
        evidence: vec![
            AlignmentEvidence::ExactCanonical,
            AlignmentEvidence::SplitMerge,
        ],
        old_separator: None,
        new_separator: Some(BlockSeparator::Space),
    };

    // 5. Clean Promoted Move -> Move Change with Medium confidence
    let old_b5 = block(5, "Relocated unique section");
    let new_b5 = block(106, "Relocated unique section");
    let mut del_b5 = one_sided(AlignmentKind::Deletion, &[5], &[]);
    del_b5.confidence = AlignmentConfidence::Medium;
    del_b5.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut ins_b5 = one_sided(AlignmentKind::Insertion, &[], &[106]);
    ins_b5.confidence = AlignmentConfidence::Medium;
    ins_b5.evidence.push(AlignmentEvidence::MoveCandidate);

    // 6. Promoted Move with weak/issue deletion -> Move Change with Low confidence (bounded by weakest component)
    let old_b6 = block(6, "Relocated with issue");
    let new_b6 = block(107, "Relocated with issue");
    let mut del_b6 = one_sided(AlignmentKind::Deletion, &[6], &[]);
    del_b6.confidence = AlignmentConfidence::Low;
    del_b6.evidence.push(AlignmentEvidence::MoveCandidate);
    let mut ins_b6 = one_sided(AlignmentKind::Insertion, &[], &[107]);
    ins_b6.confidence = AlignmentConfidence::Medium;
    ins_b6.evidence.push(AlignmentEvidence::MoveCandidate);

    // 7. Clean Deletion & Insertion -> Deletion / Insertion Change with Medium confidence
    let old_b7 = block(7, "Deleted normal paragraph");
    let new_b7 = block(108, "Inserted normal paragraph");
    let mut del_b7 = one_sided(AlignmentKind::Deletion, &[7], &[]);
    del_b7.confidence = AlignmentConfidence::Medium;
    let mut ins_b7 = one_sided(AlignmentKind::Insertion, &[], &[108]);
    ins_b7.confidence = AlignmentConfidence::Medium;

    // 8. Deletion with Low confidence (issue) -> Deletion Change with Low confidence
    let old_b8 = block(8, "Deleted issue paragraph");
    let mut del_b8 = one_sided(AlignmentKind::Deletion, &[8], &[]);
    del_b8.confidence = AlignmentConfidence::Low;

    // 9. Unresolved region -> UnresolvedRegion in diff, no false changes
    let old_b9 = block(9, "Unresolved corrupted block");
    let new_b9 = block(109, "Unresolved corrupted block replacement");
    let span_unresolved = AlignmentSpan {
        kind: AlignmentKind::Unresolved,
        old: vec![BlockId(9)],
        new: vec![BlockId(109)],
        score: 0.0,
        canonical_similarity: 0.0,
        score_margin: None,
        confidence: AlignmentConfidence::Low,
        evidence: vec![AlignmentEvidence::NormalizationIssue],
        old_separator: None,
        new_separator: None,
    };

    let old_blocks = vec![
        old_b1, old_b2, old_b3, old_b4, old_b5, old_b6, old_b7, old_b8, old_b9,
    ];
    let new_blocks = vec![
        new_b1, new_b2, new_b3, new_b4a, new_b4b, new_b5, new_b6, new_b7, new_b9,
    ];
    let alignment = Alignment {
        spans: vec![
            span_exact_clean,
            span_fuzzy_strong,
            span_fuzzy_weak,
            span_split,
            del_b5,
            ins_b5,
            del_b6,
            ins_b6,
            del_b7,
            ins_b7,
            del_b8,
            span_unresolved,
        ],
        main_anchors: Vec::new(),
        move_candidates: vec![
            ExactAnchor {
                old: BlockId(5),
                new: BlockId(106),
            },
            ExactAnchor {
                old: BlockId(6),
                new: BlockId(107),
            },
        ],
    };
    let diff = compare_aligned(&old_blocks, &new_blocks, &alignment, DiffOptions::default())?;

    // Verify FormattingChanges: clean exact normalization (High) and split/merge structure (Medium)
    assert_eq!(diff.formatting_changes.len(), 2);
    assert_eq!(diff.formatting_changes[0].confidence, Confidence::High);
    assert_eq!(
        diff.formatting_changes[0].reasons,
        [FormattingReason::Normalization]
    );
    assert_eq!(diff.formatting_changes[1].confidence, Confidence::Medium);
    assert_eq!(
        diff.formatting_changes[1].reasons,
        [FormattingReason::BlockStructure]
    );

    // Verify established changes by kind and confidence
    let fuzzy_strong = diff
        .changes
        .iter()
        .find(|c| {
            c.occurrences[0]
                .old_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(2)])
        })
        .expect("fuzzy strong change must be found");
    assert_eq!(fuzzy_strong.kind, ChangeKind::Replacement);
    assert_eq!(fuzzy_strong.confidence, Confidence::Medium);

    let fuzzy_weak = diff
        .changes
        .iter()
        .find(|c| {
            c.occurrences[0]
                .old_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(3)])
        })
        .expect("fuzzy weak change must be found");
    assert_eq!(fuzzy_weak.kind, ChangeKind::Replacement);
    assert_eq!(fuzzy_weak.confidence, Confidence::Low);

    let move_clean = diff
        .change_candidates
        .iter()
        .find(|candidate| {
            candidate.change.occurrences[0]
                .old_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(5)])
        })
        .expect("clean move candidate must be found");
    assert_eq!(move_clean.change.kind, ChangeKind::Move);
    assert_eq!(move_clean.change.confidence, Confidence::Medium);

    let move_issue = diff
        .change_candidates
        .iter()
        .find(|candidate| {
            candidate.change.occurrences[0]
                .old_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(6)])
        })
        .expect("issue move candidate must be found");
    assert_eq!(move_issue.change.kind, ChangeKind::Move);
    assert_eq!(move_issue.change.confidence, Confidence::Low); // Bounded by weakest side

    let del_clean = diff
        .change_candidates
        .iter()
        .find(|candidate| {
            candidate.change.occurrences[0]
                .old_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(7)])
        })
        .expect("clean deletion candidate must be found");
    assert_eq!(del_clean.change.kind, ChangeKind::Deletion);
    assert_eq!(del_clean.change.confidence, Confidence::Medium);

    let ins_clean = diff
        .change_candidates
        .iter()
        .find(|candidate| {
            candidate.change.occurrences[0]
                .new_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(108)])
        })
        .expect("clean insertion candidate must be found");
    assert_eq!(ins_clean.change.kind, ChangeKind::Insertion);
    assert_eq!(ins_clean.change.confidence, Confidence::Medium);

    let del_issue = diff
        .change_candidates
        .iter()
        .find(|candidate| {
            candidate.change.occurrences[0]
                .old_span
                .as_ref()
                .is_some_and(|s| s.blocks == [BlockId(8)])
        })
        .expect("issue deletion candidate must be found");
    assert_eq!(del_issue.change.kind, ChangeKind::Deletion);
    assert_eq!(del_issue.change.confidence, Confidence::Low);

    // Verify UnresolvedRegion
    assert_eq!(diff.unresolved_regions.len(), 8);
    let unresolved_issue = diff
        .unresolved_regions
        .iter()
        .find(|region| {
            region
                .old_span
                .as_ref()
                .is_some_and(|span| span.blocks == [BlockId(9)])
        })
        .expect("issue region must remain unresolved");
    assert_eq!(
        unresolved_issue
            .old_span
            .as_ref()
            .expect("old span must be present")
            .blocks,
        [BlockId(9)]
    );
    assert_eq!(
        unresolved_issue
            .new_span
            .as_ref()
            .expect("new span must be present")
            .blocks,
        [BlockId(109)]
    );

    Ok(())
}

#[test]
fn mandatory_matching_equality_establishes_fixed_equal_children() -> Result<()> {
    // Repeated equal "Q" blocks avoid globally unique block anchors, so the
    // parent domain stays ambiguous at the middle deletion while every maximum
    // matching still forces both boundary Q pairs.
    let old = [block(1, "Q"), block(2, "a a"), block(3, "Q")];
    let new = [block(101, "Q"), block(102, "a"), block(103, "Q")];
    let alignment = aligned(vec![
        matched(&[1], &[101]),
        matched(&[2], &[102]),
        matched(&[3], &[103]),
    ]);

    let result = compare_aligned(&old, &new, &alignment, DiffOptions::default())?;
    let assessment = result.assessment.as_ref().expect("assessment");

    let mandatory = assessment
        .relations
        .iter()
        .filter(|relation| {
            relation.outcome == pdfdelta_core::diff::RelationOutcome::Established
                && relation
                    .assumptions
                    .contains(&ComparisonAssumption::MandatoryMatchingEquality)
                && relation
                    .old_span
                    .as_ref()
                    .is_some_and(|span| span.blocks == [BlockId(1)])
                && relation
                    .new_span
                    .as_ref()
                    .is_some_and(|span| span.blocks == [BlockId(101)])
        })
        .count();
    assert_eq!(
        mandatory, 1,
        "the first fixed equal child must be established: {:#?}",
        assessment.relations
    );
    let second = assessment.relations.iter().any(|relation| {
        relation.outcome == pdfdelta_core::diff::RelationOutcome::Established
            && relation
                .assumptions
                .contains(&ComparisonAssumption::MandatoryMatchingEquality)
            && relation
                .old_span
                .as_ref()
                .is_some_and(|span| span.blocks == [BlockId(3)])
    });
    assert!(second, "the trailing fixed equal child must be established");
    let ambiguous_middle = assessment.relations.iter().any(|relation| {
        relation.outcome == pdfdelta_core::diff::RelationOutcome::Tentative
            && relation
                .reasons
                .contains(&pdfdelta_core::diff::AssessmentReason::AmbiguousEditLocation)
            && relation
                .old_span
                .as_ref()
                .is_some_and(|span| span.blocks == [BlockId(2)])
    });
    assert!(
        ambiguous_middle,
        "the ambiguous middle deletion must stay tentative: {:#?}",
        assessment.relations
    );
    Ok(())
}

#[test]
fn repeated_equal_text_without_mandatory_positions_is_not_promoted() -> Result<()> {
    // Three separate equal "Q" blocks against two: the deletion is ambiguous
    // among the repeated Q blocks, so an equal Q child has no mandatory
    // source position even though its text is equal on both sides.
    let old = [block(1, "Q"), block(2, "Q"), block(3, "Q")];
    let new = [block(101, "Q"), block(102, "Q")];
    let alignment = aligned(vec![
        matched(&[1], &[101]),
        matched(&[2], &[102]),
        one_sided(AlignmentKind::Deletion, &[3], &[]),
    ]);

    let result = compare_aligned(&old, &new, &alignment, DiffOptions::default())?;
    let assessment = result.assessment.as_ref().expect("assessment");
    assert!(
        !assessment.relations.iter().any(|relation| {
            relation
                .assumptions
                .contains(&ComparisonAssumption::MandatoryMatchingEquality)
        }),
        "repeated equal text without fixed positions must not be promoted: {:#?}",
        assessment.relations
    );
    let ambiguous_parent = assessment.relations.iter().any(|relation| {
        relation.outcome == pdfdelta_core::diff::RelationOutcome::Tentative
            && relation
                .reasons
                .contains(&pdfdelta_core::diff::AssessmentReason::AmbiguousEditLocation)
    });
    assert!(
        ambiguous_parent,
        "a repeated-Q relation must stay ambiguous: {:#?}",
        assessment.relations
    );
    let q_child_tentative = assessment.relations.iter().any(|relation| {
        relation.outcome == pdfdelta_core::diff::RelationOutcome::Tentative
            && relation
                .old_span
                .as_ref()
                .is_some_and(|span| span.blocks == [BlockId(1)])
            && relation
                .new_span
                .as_ref()
                .is_some_and(|span| span.blocks == [BlockId(101)])
    });
    assert!(
        q_child_tentative,
        "the equal Q child without a mandatory position stays tentative: {:#?}",
        assessment.relations
    );
    Ok(())
}

#[test]
fn missing_parent_premises_block_mandatory_matching_promotion() -> Result<()> {
    // The same shape as the positive fixture, but the parent carries a
    // normalization issue, so the optional mandatory-match proof must not run.
    let old = [block(1, "Q"), block(2, "a a"), block(3, "Q")];
    let new = [block(101, "Q"), block(102, "a"), block(103, "Q")];
    let mut issue = matched(&[2], &[102]);
    issue.evidence = vec![AlignmentEvidence::NormalizationIssue];
    let alignment = aligned(vec![matched(&[1], &[101]), issue, matched(&[3], &[103])]);

    let result = compare_aligned(&old, &new, &alignment, DiffOptions::default())?;
    let assessment = result.assessment.as_ref().expect("assessment");
    assert!(
        assessment.relations.iter().any(|relation| {
            relation
                .reasons
                .contains(&pdfdelta_core::diff::AssessmentReason::NormalizationUncertainty)
        }),
        "the parent must carry its normalization premise: {:#?}",
        assessment.relations
    );
    assert!(
        !assessment.relations.iter().any(|relation| {
            relation
                .assumptions
                .contains(&ComparisonAssumption::MandatoryMatchingEquality)
        }),
        "a parent with a normalization premise must not promote: {:#?}",
        assessment.relations
    );
    Ok(())
}
