use pdfdelta_core::{
    Error, Result,
    alignment::{AlignmentEvidence, BlockSeparator, CandidateSource},
    diff::{
        Change, ChangeKind, ChangeTag, Comparison, Confidence, Coverage, FormattingChange,
        FormattingReason, TextSpan, TokenRange, UnresolvedRegion,
    },
    layout::BlockId,
    model::{FontProgramHash, PageId},
    normalize::{BlockText, MappedText, ScalarRange, TextSource, UnmappedToken},
    report::{
        DocumentSide, ExitStatus, ExtractionIssueRecord, ExtractionStatus, TextReportOptions,
        exit_status, render_text, summarize, write_json,
    },
    source::{ExtractionIssueKind, ExtractionScope},
};

fn plain_options() -> TextReportOptions<'static> {
    TextReportOptions {
        old_label: "old.pdf",
        new_label: "new.pdf",
        color: false,
    }
}

#[test]
fn text_report_always_states_completeness_and_coverage() -> Result<()> {
    let report = render_text(
        &[],
        &[],
        &empty_comparison(),
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert_eq!(
        report,
        "content changes: 0 · formatting-only: 0 · uncertain: 0 · \
         unresolved regions: 0 · coverage 100.0%\n\
         \n\
         --- old.pdf\n\
         +++ new.pdf\n"
    );
    assert!(!report.contains("No differences found"));
    Ok(())
}

#[test]
fn formatting_only_changes_do_not_change_the_exit_status() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.formatting_changes.push(FormattingChange {
        old_span: span(1, 0, 5, 0, 5),
        new_span: span(101, 0, 5, 0, 5),
        confidence: Confidence::High,
        reasons: vec![FormattingReason::BlockStructure],
    });

    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), false)?,
        ExitStatus::NoContentChanges
    );
    Ok(())
}

#[test]
fn strict_incompleteness_takes_priority_over_content_changes() -> Result<()> {
    let mut comparison = content_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(span(2, 0, 4, 0, 4)),
        new_span: Some(span(102, 0, 4, 0, 4)),
        evidence: vec![AlignmentEvidence::NormalizationIssue],
    });

    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), false)?,
        ExitStatus::ContentChanges
    );
    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), true)?,
        ExitStatus::IncompleteComparison
    );
    assert_eq!(ExitStatus::ExecutionError.code(), 2);
    assert_eq!(ExitStatus::IncompleteComparison.code(), 3);
    Ok(())
}

#[test]
fn strict_mode_rejects_incomplete_alignment_coverage() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.old_coverage = Coverage {
        resolved_tokens: 1,
        total_tokens: 2,
        ratio: Some(0.5),
    };

    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), false)?,
        ExitStatus::NoContentChanges
    );
    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), true)?,
        ExitStatus::IncompleteComparison
    );
    Ok(())
}

#[test]
fn incomplete_extraction_requires_a_described_region() {
    let missing_region = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: Vec::new(),
    };
    assert!(matches!(
        summarize(&empty_comparison(), &missing_region),
        Err(Error::InvalidConfiguration(message))
            if message.contains("incomplete old extraction")
    ));

    let blank_description = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unsupported,
            scope: ExtractionScope::Page(PageId(0)),
            description: "  ".to_owned(),
        }],
    };
    assert!(matches!(
        summarize(&empty_comparison(), &blank_description),
        Err(Error::InvalidConfiguration(message)) if message.contains("require a description")
    ));

    let complete_with_issue = ExtractionStatus {
        old_complete: true,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unsupported,
            scope: ExtractionScope::Page(PageId(0)),
            description: "unsupported page feature".to_owned(),
        }],
    };
    assert!(matches!(
        summarize(&empty_comparison(), &complete_with_issue),
        Err(Error::InvalidConfiguration(message)) if message.contains("cannot be complete with issues")
    ));
}

#[test]
fn reports_incomplete_extraction_in_text_and_strict_status() -> Result<()> {
    let extraction = ExtractionStatus {
        old_complete: true,
        new_complete: false,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::New,
            kind: ExtractionIssueKind::Unresolved,
            scope: ExtractionScope::Page(PageId(2)),
            description: "ambiguous text stream".to_owned(),
        }],
    };

    let mut comparison = empty_comparison();
    comparison.new_coverage.ratio = None;
    let report = render_text(&[], &[], &comparison, &extraction, &plain_options())?;

    assert!(
        report.contains("content changes: 0 · formatting-only: 0 · uncertain: 0"),
        "{report}"
    );
    assert!(report.contains("coverage unknown"), "{report}");
    assert!(
        report.contains("extraction incomplete: old=yes, new=no"),
        "{report}"
    );
    assert!(
        report.contains("! extraction issue (side=new, kind=unresolved, scope=page, page=3): ambiguous text stream"),
        "{report}"
    );
    assert_eq!(
        exit_status(&comparison, &extraction, true)?,
        ExitStatus::IncompleteComparison
    );
    Ok(())
}

#[test]
fn json_report_preserves_ranges_evidence_and_side_specific_coverage() -> Result<()> {
    let mut comparison = content_comparison();
    comparison.old_coverage = Coverage {
        resolved_tokens: 2,
        total_tokens: 3,
        ratio: None,
    };
    comparison.new_coverage = Coverage {
        resolved_tokens: 1,
        total_tokens: 2,
        ratio: Some(0.5),
    };
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(span(2, 0, 1, 0, 1)),
        new_span: None,
        evidence: vec![AlignmentEvidence::CandidateSource(
            CandidateSource::NGramInvertedIndex,
        )],
    });
    let extraction = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unsupported,
            scope: ExtractionScope::Page(PageId(4)),
            description: "unknown stream operator".to_owned(),
        }],
    };
    let mut output = Vec::new();

    write_json(
        &mut output,
        &fixture_blocks(&[1, 2]),
        &fixture_blocks(&[101]),
        &comparison,
        &extraction,
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 5);
    assert_eq!(json["summary"]["content_changes"], 1);
    assert_eq!(
        json["summary"]["old_alignment_coverage"]["resolved_tokens"],
        2
    );
    assert_eq!(
        json["summary"]["old_alignment_coverage"]["ratio"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["summary"]["comparison_coverage_ratio"],
        serde_json::Value::Null
    );
    assert_eq!(json["summary"]["new_alignment_coverage"]["total_tokens"], 2);
    assert_eq!(json["changes"][0]["kind"], "replacement");
    assert_eq!(
        json["changes"][0]["old_span"]["block_separator"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["changes"][0]["old_span"]["canonical_range"]["start"],
        1
    );
    assert_eq!(json["changes"][0]["tags"][0], "character_width");
    assert_eq!(
        json["unresolved_regions"][0]["evidence"][0],
        "candidate_source:ngram_inverted_index"
    );
    assert_eq!(json["extraction"]["old_complete"], false);
    assert_eq!(json["extraction"]["issues"][0]["kind"], "unsupported");
    assert_eq!(json["extraction"]["issues"][0]["scope"], "page");
    assert_eq!(json["extraction"]["issues"][0]["page"], 4);
    assert_eq!(
        json["extraction"]["issues"][0]["description"],
        "unknown stream operator"
    );
    Ok(())
}

#[test]
fn json_report_serializes_multi_block_separators() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.formatting_changes.push(FormattingChange {
        old_span: group_span(&[1, 2], Some(BlockSeparator::Space)),
        new_span: group_span(&[101, 102], Some(BlockSeparator::Concatenate)),
        confidence: Confidence::High,
        reasons: vec![FormattingReason::BlockStructure],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &fixture_blocks(&[1, 2]),
        &fixture_blocks(&[101, 102]),
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 5);
    assert_eq!(
        json["formatting_only_changes"][0]["old_span"]["block_separator"],
        "space"
    );
    assert_eq!(
        json["formatting_only_changes"][0]["new_span"]["block_separator"],
        "concatenate"
    );
    Ok(())
}

#[test]
fn json_report_counts_typed_extraction_issues_and_omits_document_page() -> Result<()> {
    let extraction = ExtractionStatus {
        old_complete: false,
        new_complete: false,
        issues: vec![
            ExtractionIssueRecord {
                side: DocumentSide::Old,
                kind: ExtractionIssueKind::Unsupported,
                scope: ExtractionScope::Page(PageId(3)),
                description: "unsupported page feature".to_owned(),
            },
            ExtractionIssueRecord {
                side: DocumentSide::New,
                kind: ExtractionIssueKind::Unresolved,
                scope: ExtractionScope::Document,
                description: "ambiguous document structure".to_owned(),
            },
        ],
    };
    let mut comparison = empty_comparison();
    comparison.old_coverage.ratio = None;
    comparison.new_coverage.ratio = None;
    let mut output = Vec::new();

    write_json(&mut output, &[], &[], &comparison, &extraction)?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["summary"]["unsupported_extraction_issues"], 1);
    assert_eq!(json["summary"]["unresolved_extraction_issues"], 1);
    assert_eq!(json["extraction"]["issues"][0]["side"], "old");
    assert_eq!(json["extraction"]["issues"][0]["page"], 3);
    assert_eq!(json["extraction"]["issues"][1]["side"], "new");
    assert_eq!(json["extraction"]["issues"][1]["scope"], "document");
    assert!(json["extraction"]["issues"][1].get("page").is_none());
    Ok(())
}

#[test]
fn accepts_document_issues_with_partial_evidence() {
    let mixed_scopes = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![
            ExtractionIssueRecord {
                side: DocumentSide::Old,
                kind: ExtractionIssueKind::Unsupported,
                scope: ExtractionScope::Document,
                description: "unsupported document feature".to_owned(),
            },
            ExtractionIssueRecord {
                side: DocumentSide::Old,
                kind: ExtractionIssueKind::Unresolved,
                scope: ExtractionScope::Page(PageId(0)),
                description: "ambiguous page content".to_owned(),
            },
        ],
    };
    let mut mixed_comparison = empty_comparison();
    mixed_comparison.old_coverage.ratio = None;
    let summary = summarize(&mixed_comparison, &mixed_scopes)
        .expect("document and page issues may coexist for partial evidence");
    assert!(!summary.comparison_complete);
    assert_eq!(summary.unsupported_extraction_issues, 1);
    assert_eq!(summary.unresolved_extraction_issues, 1);

    let document_issue_with_evidence = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unsupported,
            scope: ExtractionScope::Document,
            description: "unsupported document feature".to_owned(),
        }],
    };
    let mut comparison = empty_comparison();
    comparison.old_coverage = Coverage {
        resolved_tokens: 0,
        total_tokens: 1,
        ratio: None,
    };
    let summary = summarize(&comparison, &document_issue_with_evidence)
        .expect("document issues may retain extracted alignment evidence");
    assert!(!summary.comparison_complete);
    assert_eq!(summary.old_alignment_coverage, None);
}

#[test]
fn rejects_inconsistent_coverage_before_rendering() {
    let mut comparison = empty_comparison();
    comparison.old_coverage = Coverage {
        resolved_tokens: 1,
        total_tokens: 2,
        ratio: Some(1.0),
    };

    assert!(matches!(
        render_text(
            &[],
            &[],
            &comparison,
            &ExtractionStatus::complete(),
            &plain_options(),
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("invalid old alignment coverage")
    ));
}

#[test]
fn rejects_invalid_public_change_and_region_shapes() {
    let mut missing_span = empty_comparison();
    missing_span.changes.push(Change {
        kind: ChangeKind::Insertion,
        old_span: None,
        new_span: None,
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    assert!(matches!(
        summarize(&missing_span, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("invalid Insertion change span shape")
    ));

    let mut reversed_range = content_comparison();
    reversed_range.changes[0]
        .old_span
        .as_mut()
        .expect("fixture replacement should have an old span")
        .canonical_range = ScalarRange { start: 2, end: 1 };
    assert!(matches!(
        summarize(&reversed_range, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message)) if message.contains("ranges must be ordered")
    ));

    let mut empty_blocks = content_comparison();
    empty_blocks.changes[0]
        .new_span
        .as_mut()
        .expect("fixture replacement should have a new span")
        .blocks
        .clear();
    assert!(matches!(
        summarize(&empty_blocks, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("at least one block")
    ));

    let mut single_block_separator = content_comparison();
    single_block_separator.changes[0]
        .old_span
        .as_mut()
        .expect("fixture replacement should have an old span")
        .separator = Some(BlockSeparator::Space);
    assert!(matches!(
        summarize(
            &single_block_separator,
            &ExtractionStatus::complete()
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("single-block") && message.contains("separator")
    ));

    let mut missing_multi_block_separator = content_comparison();
    missing_multi_block_separator.changes[0]
        .old_span
        .as_mut()
        .expect("fixture replacement should have an old span")
        .blocks
        .push(BlockId(2));
    assert!(matches!(
        summarize(
            &missing_multi_block_separator,
            &ExtractionStatus::complete()
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("multi-block") && message.contains("separator")
    ));

    let mut empty_change = content_comparison();
    empty_change.changes[0]
        .new_span
        .as_mut()
        .expect("fixture replacement should have a new span")
        .comparable_range = TokenRange { start: 1, end: 1 };
    assert!(matches!(
        summarize(&empty_change, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("at least one comparable token")
    ));

    let mut missing_reason = empty_comparison();
    missing_reason.formatting_changes.push(FormattingChange {
        old_span: span(1, 0, 1, 0, 1),
        new_span: span(101, 0, 1, 0, 1),
        confidence: Confidence::High,
        reasons: Vec::new(),
    });
    assert!(matches!(
        summarize(&missing_reason, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("at least one reason")
    ));

    let mut empty_region = empty_comparison();
    empty_region.unresolved_regions.push(UnresolvedRegion {
        old_span: None,
        new_span: None,
        evidence: Vec::new(),
    });
    assert!(matches!(
        summarize(&empty_region, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("at least one text span")
    ));
}

#[test]
fn text_report_renders_replacement_insertion_and_deletion_hunks() -> Result<()> {
    let old_blocks = vec![
        block_with_text(2, "Release 10 remains available"),
        block_with_text(3, "Removed paragraph disappears"),
    ];
    let new_blocks = vec![
        block_with_text(102, "Release 20 remains available"),
        block_with_text(103, "Inserted paragraph appears here"),
    ];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(full_span(2, "Release 10 remains available")),
        new_span: Some(full_span(102, "Release 20 remains available")),
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Insertion,
        old_span: None,
        new_span: Some(full_span(103, "Inserted paragraph appears here")),
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Deletion,
        old_span: Some(full_span(3, "Removed paragraph disappears")),
        new_span: None,
        confidence: Confidence::Medium,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert_eq!(
        report,
        "content changes: 3 · formatting-only: 0 · uncertain: 0 · \
         unresolved regions: 0 · coverage 100.0%\n\
         \n\
         --- old.pdf\n\
         +++ new.pdf\n\
         \n\
         @@ page 1 · old block 2 -> new block 102 · confidence: high @@\n\
         - Release 10 remains available\n\
         + Release 20 remains available\n\
         \n\
         @@ page 1 · new block 103 · confidence: high @@\n\
         + Inserted paragraph appears here\n\
         \n\
         @@ page 1 · old block 3 · confidence: medium @@\n\
         - Removed paragraph disappears\n"
    );
    Ok(())
}

#[test]
fn text_report_coalesces_adjacent_edits_without_mutating_the_comparison() -> Result<()> {
    let old_blocks = vec![block_with_text(7, "alpha Release 10 omega")];
    let new_blocks = vec![block_with_text(9, "alpha Release 21 omega")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(range_span(7, 14, 15)),
        new_span: Some(range_span(9, 14, 15)),
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(range_span(7, 15, 16)),
        new_span: Some(range_span(9, 15, 16)),
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    let changes_before = comparison.changes.clone();

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    // Two scalar-level replacements separated by zero equal tokens coalesce
    // into ONE presentation hunk with a single adjacent -/+ pair.
    assert!(report.matches("@@ page 1").count() == 1, "{report}");
    assert!(
        report.contains("- alpha Release 10 omega\n+ alpha Release 21 omega\n"),
        "{report}"
    );
    assert_eq!(
        comparison.changes, changes_before,
        "rendering must not mutate the comparison"
    );
    Ok(())
}

#[test]
fn text_report_keeps_distant_edits_in_separate_hunks() -> Result<()> {
    let filler = "x".repeat(40);
    let old_text = format!("start {filler} middle {filler} end");
    let old_blocks = vec![block_with_text(5, &old_text)];
    let new_blocks = vec![block_with_text(105, &old_text)];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(range_span(5, 0, 1)),
        new_span: Some(range_span(105, 0, 1)),
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(range_span(5, 88, 89)),
        new_span: Some(range_span(105, 88, 89)),
        confidence: Confidence::High,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    // The edits are separated by more than the coalescing threshold of
    // unchanged tokens, so they stay in distinct hunks.
    assert!(report.matches("@@ page 1").count() == 2, "{report}");
    Ok(())
}

#[test]
fn text_report_bounds_context_for_tiny_edits_inside_long_blocks() -> Result<()> {
    let long_prefix = "a".repeat(80);
    let long_suffix = "z".repeat(80);
    let old_blocks = vec![block_with_text(
        11,
        &format!("{long_prefix} 4{long_suffix}"),
    )];
    let new_blocks = vec![block_with_text(
        111,
        &format!("{long_prefix} 5{long_suffix}"),
    )];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(range_span(11, 81, 82)),
        new_span: Some(range_span(111, 81, 82)),
        confidence: Confidence::Medium,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    // A one-scalar year-style edit shows bounded surrounding context instead
    // of either the bare scalar or the whole oversized block: 31 leading 'a'
    // scalars, the changed digit, then 32 trailing 'z' scalars, with `...`
    // marking both elisions.
    let expected_minus = format!("- ... {} 4{} ...\n", "a".repeat(31), "z".repeat(32),);
    let expected_plus = format!("+ ... {} 5{} ...\n", "a".repeat(31), "z".repeat(32),);
    assert!(report.contains(&expected_minus), "{report}");
    assert!(report.contains(&expected_plus), "{report}");
    Ok(())
}

#[test]
fn text_report_renders_move_marker_and_locations() -> Result<()> {
    let old_blocks = vec![block_with_pages(3, "Shared paragraph text", &[1])];
    let new_blocks = vec![block_with_pages(8, "Shared paragraph text", &[4])];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Move,
        old_span: Some(full_span(3, "Shared paragraph text")),
        new_span: Some(full_span(8, "Shared paragraph text")),
        confidence: Confidence::Medium,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert!(
        report.contains("@@ pages 2,5 · old block 3 -> new block 8 · confidence: medium @@"),
        "{report}"
    );
    assert!(report.contains("~ moved from page 2 to page 5"), "{report}");
    assert!(
        report.contains("- Shared paragraph text\n+ Shared paragraph text"),
        "{report}"
    );
    Ok(())
}

#[test]
fn text_report_renders_unresolved_regions_explicitly() -> Result<()> {
    let old_blocks = vec![block_with_text(12, "alpha beta gamma")];
    let new_blocks = vec![block_with_text(112, "alpha delta gamma")];
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(full_span(12, "alpha beta gamma")),
        new_span: Some(full_span(112, "alpha delta gamma")),
        evidence: vec![AlignmentEvidence::TextSimilarity],
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert_eq!(
        report,
        "content changes: 0 · formatting-only: 0 · uncertain: 0 · \
         unresolved regions: 1 · coverage 100.0%\n\
         \n\
         --- old.pdf\n\
         +++ new.pdf\n\
         \n\
         @@ page 1 · UNRESOLVED @@\n\
         ? could not safely align this region (evidence: text_similarity)\n\
         ? old: alpha beta gamma\n\
         ? new: alpha delta gamma\n"
    );
    Ok(())
}

#[test]
fn text_report_color_supplements_markers_and_can_be_disabled() -> Result<()> {
    let old_blocks = vec![block_with_text(7, "alpha Release 10 omega")];
    let new_blocks = vec![block_with_text(9, "alpha Release 21 omega")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(range_span(7, 14, 16)),
        new_span: Some(range_span(9, 14, 16)),
        confidence: Confidence::High,
        tags: Vec::new(),
    });

    let plain = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &TextReportOptions {
            color: false,
            ..plain_options()
        },
    )?;
    let colored = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &TextReportOptions {
            color: true,
            ..plain_options()
        },
    )?;

    assert!(!plain.contains('\u{1b}'), "{plain}");
    // Color wraps the same readable content; stripping ANSI escape sequences
    // restores the plain rendering byte-for-byte.
    let stripped = strip_ansi(&colored);
    assert_eq!(plain, stripped);
    assert!(colored.contains("\u{1b}[31m- "), "{colored}");
    assert!(colored.contains("\u{1b}[32m+ "), "{colored}");
    assert!(colored.contains("\u{1b}[1;31m10\u{1b}[0m"), "{colored}");
    assert!(colored.contains("\u{1b}[1;32m21\u{1b}[0m"), "{colored}");
    assert!(colored.contains("\u{1b}[1;36m@@ page 1"), "{colored}");
    Ok(())
}

fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for skip in chars.by_ref() {
                if skip == 'm' {
                    break;
                }
            }
        } else {
            stripped.push(ch);
        }
    }
    stripped
}

#[test]
fn json_report_resolves_span_text_pages_and_unmapped_tokens() -> Result<()> {
    let old_blocks = vec![unmapped_block_fixture(9)];
    let new_blocks = vec![block_with_text(109, "Release 20")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(TextSpan {
            blocks: vec![BlockId(9)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: TokenRange { start: 0, end: 4 },
        }),
        new_span: Some(full_span(109, "Release 20")),
        confidence: Confidence::Low,
        tags: vec![ChangeTag::OcrConfusion],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 5);
    assert_eq!(json["changes"][0]["kind"], "replacement");
    assert_eq!(json["changes"][0]["confidence"], "low");
    assert_eq!(json["changes"][0]["tags"][0], "ocr_confusion");
    assert_eq!(json["changes"][0]["old_span"]["blocks"][0], 9);
    assert_eq!(json["changes"][0]["old_span"]["pages"][0], 4);
    assert_eq!(json["changes"][0]["old_span"]["text"], "ab");
    // Two distinct unmapped glyphs keep their stable identity and their
    // order relative to the resolved scalars (leading and trailing).
    let unmapped = &json["changes"][0]["old_span"]["unmapped_tokens"];
    assert_eq!(
        unmapped.as_array().expect("unmapped tokens").len(),
        2,
        "{json:#}"
    );
    assert_eq!(unmapped[0]["scalar_offset"], 0);
    assert_eq!(unmapped[0]["font_hash"], "0102030405");
    assert_eq!(unmapped[0]["glyph_id"], 7);
    assert_eq!(unmapped[1]["scalar_offset"], 2);
    assert_eq!(unmapped[1]["font_hash"], "09");
    assert_eq!(unmapped[1]["glyph_id"], 11);
    assert_eq!(json["changes"][0]["new_span"]["pages"][0], 0);
    assert_eq!(json["changes"][0]["new_span"]["text"], "Release 20");
    assert_eq!(
        json["changes"][0]["new_span"]["unmapped_tokens"]
            .as_array()
            .expect("unmapped tokens")
            .len(),
        0
    );
    Ok(())
}

#[test]
fn text_report_marks_unmapped_glyph_positions_in_order() -> Result<()> {
    let old_blocks = vec![unmapped_block_fixture(9)];
    let new_blocks = vec![block_with_text(109, "Release 20")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(TextSpan {
            blocks: vec![BlockId(9)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: TokenRange { start: 0, end: 4 },
        }),
        new_span: Some(full_span(109, "Release 20")),
        confidence: Confidence::Low,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    // The leading and trailing unmapped glyphs are marked at their exact
    // positions inside the -/+ lines instead of implying that 'a' and 'b'
    // are contiguous.
    assert!(
        report.contains("- <unmapped:7:01020304>ab<unmapped:11:09>\n"),
        "{report}"
    );
    assert!(report.contains("+ Release 20\n"), "{report}");
    Ok(())
}

#[test]
fn text_report_renders_pure_unmapped_replacement_in_the_changed_segment() -> Result<()> {
    let old_blocks = vec![unmapped_only_block(9, vec![1, 2, 3, 4, 5], 7)];
    let new_blocks = vec![unmapped_only_block(109, vec![9], 12)];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        // Unmapped-only edits legally carry a zero-width canonical range;
        // the changed segment must still identify both placeholders.
        old_span: Some(TextSpan {
            blocks: vec![BlockId(9)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 0 },
            comparable_range: TokenRange { start: 0, end: 1 },
        }),
        new_span: Some(TextSpan {
            blocks: vec![BlockId(109)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 0 },
            comparable_range: TokenRange { start: 0, end: 1 },
        }),
        confidence: Confidence::Low,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert_eq!(
        report,
        "content changes: 1 · formatting-only: 0 · uncertain: 1 · \
         unresolved regions: 0 · coverage 100.0%\n\
         \n\
         --- old.pdf\n\
         +++ new.pdf\n\
         \n\
         @@ pages 1,5 · old block 9 -> new block 109 · confidence: low @@\n\
         - <unmapped:7:01020304>\n\
         + <unmapped:12:09>\n"
    );
    Ok(())
}

#[test]
fn text_report_rejects_spans_that_exceed_the_block_evidence() -> Result<()> {
    let old_blocks = vec![block_with_text(7, "abc")];
    let new_blocks = vec![block_with_text(107, "abd")];

    // Comparable range beyond the block's token evidence fails like JSON.
    let mut comparable_overrun = empty_comparison();
    comparable_overrun.changes.push(Change {
        kind: ChangeKind::Deletion,
        old_span: Some(range_span(7, 0, 9)),
        new_span: None,
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    assert!(matches!(
        render_text(
            &old_blocks,
            &new_blocks,
            &comparable_overrun,
            &ExtractionStatus::complete(),
            &plain_options(),
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("exceeds the normalized block evidence")
    ));

    // Canonical range beyond the block's scalar evidence fails like JSON.
    let mut canonical_overrun = empty_comparison();
    canonical_overrun.changes.push(Change {
        kind: ChangeKind::Deletion,
        old_span: Some(TextSpan {
            blocks: vec![BlockId(7)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 9 },
            comparable_range: TokenRange { start: 0, end: 3 },
        }),
        new_span: None,
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    assert!(matches!(
        render_text(
            &old_blocks,
            &new_blocks,
            &canonical_overrun,
            &ExtractionStatus::complete(),
            &plain_options(),
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("exceeds the normalized block evidence")
    ));
    Ok(())
}

#[test]
fn text_report_rejects_out_of_range_second_coalesced_span() -> Result<()> {
    let old_blocks = vec![block_with_text(7, "abc")];
    let new_blocks = vec![block_with_text(107, "abc")];

    // The first span is fully valid and inside the coalescing gap; the second
    // span's bounds must be checked too before its range feeds the merged
    // run, or it would silently clamp instead of failing loudly.
    let comparable_overrun = |second: TextSpan| {
        let mut comparison = empty_comparison();
        comparison.changes.push(Change {
            kind: ChangeKind::Deletion,
            old_span: Some(range_span(7, 0, 1)),
            new_span: None,
            confidence: Confidence::High,
            tags: Vec::new(),
        });
        comparison.changes.push(Change {
            kind: ChangeKind::Deletion,
            old_span: Some(second),
            new_span: None,
            confidence: Confidence::High,
            tags: Vec::new(),
        });
        comparison
    };

    // Second span's comparable range exceeds the block's token evidence.
    let mut second = range_span(7, 2, 3);
    second.comparable_range = TokenRange { start: 2, end: 99 };
    assert!(matches!(
        render_text(
            &old_blocks,
            &new_blocks,
            &comparable_overrun(second),
            &ExtractionStatus::complete(),
            &plain_options(),
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("exceeds the normalized block evidence")
    ));

    // Second span's canonical range exceeds the block's scalar evidence.
    let mut second = range_span(7, 2, 3);
    second.canonical_range = ScalarRange { start: 0, end: 99 };
    assert!(matches!(
        render_text(
            &old_blocks,
            &new_blocks,
            &comparable_overrun(second),
            &ExtractionStatus::complete(),
            &plain_options(),
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("exceeds the normalized block evidence")
    ));
    Ok(())
}

#[test]
fn separator_mismatch_keeps_adjacent_edits_in_separate_hunks() -> Result<()> {
    let old_blocks = fixture_blocks(&[1, 2]);
    let new_blocks = fixture_blocks(&[101, 102]);
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(group_span(&[1, 2], Some(BlockSeparator::Space))),
        new_span: Some(group_span(&[101, 102], Some(BlockSeparator::Space))),
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        // Same blocks but a different separator means a different coordinate
        // system, so the adjacent edit must not coalesce into one hunk.
        old_span: Some(group_span(&[1, 2], Some(BlockSeparator::Concatenate))),
        new_span: Some(group_span(&[101, 102], Some(BlockSeparator::Concatenate))),
        confidence: Confidence::High,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert!(report.matches("@@ page 1").count() == 2, "{report}");
    Ok(())
}

fn empty_comparison() -> Comparison {
    Comparison {
        changes: Vec::new(),
        formatting_changes: Vec::new(),
        unresolved_regions: Vec::new(),
        old_coverage: Coverage {
            resolved_tokens: 0,
            total_tokens: 0,
            ratio: Some(1.0),
        },
        new_coverage: Coverage {
            resolved_tokens: 0,
            total_tokens: 0,
            ratio: Some(1.0),
        },
    }
}

fn content_comparison() -> Comparison {
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        old_span: Some(span(1, 1, 2, 1, 2)),
        new_span: Some(span(101, 1, 2, 1, 2)),
        confidence: Confidence::Low,
        tags: vec![ChangeTag::CharacterWidth],
    });
    comparison
}

fn span(
    block: u64,
    scalar_start: usize,
    scalar_end: usize,
    token_start: usize,
    token_end: usize,
) -> TextSpan {
    TextSpan {
        blocks: vec![BlockId(block)],
        separator: None,
        canonical_range: ScalarRange {
            start: scalar_start,
            end: scalar_end,
        },
        comparable_range: TokenRange {
            start: token_start,
            end: token_end,
        },
    }
}

fn group_span(blocks: &[u64], separator: Option<BlockSeparator>) -> TextSpan {
    TextSpan {
        blocks: blocks.iter().copied().map(BlockId).collect(),
        separator,
        canonical_range: ScalarRange { start: 0, end: 1 },
        comparable_range: TokenRange { start: 0, end: 1 },
    }
}

fn full_span(block: u64, text: &str) -> TextSpan {
    let count = text.chars().count();
    TextSpan {
        blocks: vec![BlockId(block)],
        separator: None,
        canonical_range: ScalarRange {
            start: 0,
            end: count,
        },
        comparable_range: TokenRange {
            start: 0,
            end: count,
        },
    }
}

/// A scalar-indexed span over a single block, with the comparable range kept
/// equal to the canonical range (valid while the fixture block has no
/// unmapped tokens).
fn range_span(block: u64, start: usize, end: usize) -> TextSpan {
    TextSpan {
        blocks: vec![BlockId(block)],
        separator: None,
        canonical_range: ScalarRange { start, end },
        comparable_range: TokenRange { start, end },
    }
}

fn block_with_pages(id: u64, text: &str, pages: &[u32]) -> BlockText {
    let mut block = block_with_text(id, text);
    block.pages = pages.to_vec();
    block
}

fn fixture_blocks(ids: &[u64]) -> Vec<BlockText> {
    ids.iter()
        .copied()
        .map(|id| block_with_text(id, "0123456789"))
        .collect()
}

fn block_with_text(id: u64, text: &str) -> BlockText {
    BlockText {
        block: BlockId(id),
        raw: MappedText {
            text: String::new(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        },
        canonical: MappedText {
            text: text.to_owned(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        },
        matching: String::new(),
        matching_tokens: Vec::new(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: vec![0],
    }
}

/// Canonical text "ab" with two distinct unmapped glyph tokens: one before
/// 'a' and one after 'b', exercising leading/trailing positions and order.
fn unmapped_block_fixture(id: u64) -> BlockText {
    BlockText {
        block: BlockId(id),
        raw: MappedText {
            text: String::new(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        },
        canonical: MappedText {
            text: "ab".to_owned(),
            source_map: Vec::new(),
            unmapped: vec![
                UnmappedToken {
                    scalar_index: 0,
                    font_hash: FontProgramHash(vec![1, 2, 3, 4, 5]),
                    glyph_id: 7,
                    source: TextSource { atoms: Vec::new() },
                },
                UnmappedToken {
                    scalar_index: 2,
                    font_hash: FontProgramHash(vec![9]),
                    glyph_id: 11,
                    source: TextSource { atoms: Vec::new() },
                },
            ],
        },
        matching: String::new(),
        matching_tokens: Vec::new(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: vec![4],
    }
}

/// A block whose comparable evidence is a single unmapped glyph with no
/// canonical scalars at all.
fn unmapped_only_block(id: u64, hash: Vec<u8>, glyph_id: u16) -> BlockText {
    BlockText {
        block: BlockId(id),
        raw: MappedText {
            text: String::new(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        },
        canonical: MappedText {
            text: String::new(),
            source_map: Vec::new(),
            unmapped: vec![UnmappedToken {
                scalar_index: 0,
                font_hash: FontProgramHash(hash),
                glyph_id,
                source: TextSource { atoms: Vec::new() },
            }],
        },
        matching: String::new(),
        matching_tokens: Vec::new(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: if id > 100 { vec![0] } else { vec![4] },
    }
}
