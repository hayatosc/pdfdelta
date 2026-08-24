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
        DocumentSide, ExitStatus, ExtractionIssueRecord, ExtractionStatus, exit_status,
        render_text, summarize, write_json,
    },
    source::{ExtractionIssueKind, ExtractionScope},
};

#[test]
fn text_report_always_states_completeness_and_coverage() -> Result<()> {
    let report = render_text(&[], &[], &empty_comparison(), &ExtractionStatus::complete())?;

    assert_eq!(
        report,
        "Content changes:          0\n\
         Formatting-only changes:  0\n\
         Uncertain changes:        0\n\
         Unresolved regions:       0\n\
         Unsupported extraction:   0\n\
         Unresolved extraction:    0\n\
         Extraction complete:      old=yes, new=yes\n\
         Alignment coverage:       old=100.0%, new=100.0%\n\
         Comparison coverage:      100.0%\n"
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
    let report = render_text(&[], &[], &comparison, &extraction)?;

    assert!(report.contains("Extraction complete:      old=yes, new=no"));
    assert!(report.contains("Unsupported extraction:   0"));
    assert!(report.contains("Unresolved extraction:    1"));
    assert!(report.contains("Alignment coverage:       old=100.0%, new=unknown"));
    assert!(report.contains("Comparison coverage:      unknown"));
    assert!(report.contains(
        "Extraction issue (side=new, kind=unresolved, scope=page, page=2): ambiguous text stream"
    ));
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
        render_text(&[], &[], &comparison, &ExtractionStatus::complete()),
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
fn text_report_shows_changed_content_for_replacement_insertion_and_deletion() -> Result<()> {
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
    )?;

    assert!(
        report.contains("Change 1: replacement (confidence=high)\n"),
        "{report}"
    );
    assert!(
        report.contains("old blocks=[2] pages=[0]: Release 10 remains available\n"),
        "{report}"
    );
    assert!(
        report.contains("new blocks=[102] pages=[0]: Release 20 remains available\n"),
        "{report}"
    );
    assert!(
        report.contains("Change 2: insertion (confidence=high)\n"),
        "{report}"
    );
    assert!(
        report.contains("new blocks=[103] pages=[0]: Inserted paragraph appears here\n"),
        "{report}"
    );
    assert!(
        report.contains("Change 3: deletion (confidence=medium)\n"),
        "{report}"
    );
    assert!(
        report.contains("old blocks=[3] pages=[0]: Removed paragraph disappears\n"),
        "{report}"
    );
    Ok(())
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
    )?;

    // The leading and trailing unmapped glyphs are marked at their exact
    // positions instead of implying that 'a' and 'b' are contiguous.
    assert!(
        report.contains("old blocks=[9] pages=[4]: <unmapped:7:01020304>ab<unmapped:11:09>\n"),
        "{report}"
    );
    assert!(
        report.contains("new blocks=[109] pages=[0]: Release 20\n"),
        "{report}"
    );
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
