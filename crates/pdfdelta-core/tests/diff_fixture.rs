use pdfdelta_core::{
    Error, Result,
    alignment::{
        Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentSpan,
        BlockSeparator, ExactAnchor,
    },
    diff::{ChangeKind, Confidence, DiffOptions, FormattingReason, TokenRange, compare_aligned},
    layout::BlockId,
    model::FontProgramHash,
    normalize::{BlockText, ComparableToken, MappedText, ScalarRange, TextSource, UnmappedToken},
};

#[test]
fn exact_diff_never_uses_masked_matching_text() -> Result<()> {
    let old = block_with_matching(1, "Count: 10 files", "Count: <NUM> files", true);
    let new = block_with_matching(101, "Count: 20 files", "Count: <NUM> files", true);
    let alignment = aligned(vec![matched(&[1], &[101])]);

    let result = compare_aligned(&[old], &[new], &alignment, DiffOptions::default())?;

    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(
        result.changes[0]
            .old_span
            .as_ref()
            .expect("replacement should have an old span")
            .canonical_range,
        ScalarRange { start: 7, end: 8 }
    );
    assert_eq!(
        result.changes[0]
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
fn promotes_an_exact_move_candidate() -> Result<()> {
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

    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].kind, ChangeKind::Move);
    assert!(result.changes[0].old_span.is_some());
    assert!(result.changes[0].new_span.is_some());
    assert_eq!(result.old_coverage.ratio, Some(1.0));
    assert_eq!(result.new_coverage.ratio, Some(1.0));
    Ok(())
}

#[test]
fn keeps_a_non_exact_move_candidate_as_deletion_and_insertion() -> Result<()> {
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

    assert_eq!(
        result
            .changes
            .iter()
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        [ChangeKind::Deletion, ChangeKind::Insertion]
    );
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
        change
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
            result.changes[0]
                .old_span
                .as_ref()
                .expect("replacement should have an old span")
                .separator,
            Some(separator)
        );
        assert_eq!(
            result.changes[0]
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
fn reports_aligned_insertions_and_deletions_as_resolved() -> Result<()> {
    let old = [block(1, "A"), block(2, "removed")];
    let new = [block(101, "A"), block(102, "inserted")];
    let alignment = aligned(vec![
        matched(&[1], &[101]),
        one_sided(AlignmentKind::Deletion, &[2], &[]),
        one_sided(AlignmentKind::Insertion, &[], &[102]),
    ]);

    let result = compare_aligned(&old, &new, &alignment, DiffOptions::default())?;

    assert_eq!(result.changes.len(), 2);
    assert_eq!(result.changes[0].kind, ChangeKind::Deletion);
    assert_eq!(result.changes[1].kind, ChangeKind::Insertion);
    assert_eq!(result.old_coverage.resolved_tokens, 8);
    assert_eq!(result.old_coverage.total_tokens, 8);
    assert_eq!(result.new_coverage.resolved_tokens, 9);
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
    let old_span = change
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

    assert!(result.unresolved_regions.is_empty());
    assert!(!result.changes.is_empty());
    assert!(
        result
            .changes
            .iter()
            .all(|change| change.confidence == Confidence::Low)
    );
    assert_eq!(
        result.old_coverage.resolved_tokens,
        result.old_coverage.total_tokens
    );
    assert_eq!(
        result.new_coverage.resolved_tokens,
        result.new_coverage.total_tokens
    );
    Ok(())
}

#[test]
fn keeps_diffing_an_implausible_match_with_strong_alignment_evidence() -> Result<()> {
    let old = [block(1, "aaaabbbbccccdddd")];
    let new = [block(101, "aaaaXXXXXXXXYYYY")];
    let mut span = matched(&[1], &[101]);
    span.score = 0.9;
    span.canonical_similarity = 0.9;

    let result = compare_aligned(&old, &new, &aligned(vec![span]), DiffOptions::default())?;

    // The plausibility gate is scoped to weak (low-confidence) matches;
    // strongly evidenced matches still produce their token-level changes.
    assert!(!result.changes.is_empty());
    assert!(result.unresolved_regions.is_empty());
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

fn block_with_matching(
    id: u64,
    canonical: &str,
    matching: &str,
    numeric_mask_applied: bool,
) -> BlockText {
    BlockText {
        block: BlockId(id),
        raw: mapped(canonical),
        canonical: mapped(canonical),
        matching: matching.to_owned(),
        matching_tokens: matching.chars().map(ComparableToken::Scalar).collect(),
        numeric_mask_applied,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: Vec::new(),
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
            source: TextSource { atoms: Vec::new() },
        }],
    };
    BlockText {
        block: BlockId(id),
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
