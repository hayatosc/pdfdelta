use pdfdelta_core::{
    Error, Result,
    alignment::{AlignmentEvidence, CandidateSource},
    diff::{
        Change, ChangeKind, ChangeTag, Comparison, Confidence, Coverage, FormattingChange,
        FormattingReason, TextSpan, TokenRange, UnresolvedRegion,
    },
    layout::BlockId,
    normalize::ScalarRange,
    report::{
        DocumentSide, ExitStatus, ExtractionStatus, UnsupportedRegion, exit_status, render_text,
        summarize, write_json,
    },
};

#[test]
fn text_report_always_states_completeness_and_coverage() -> Result<()> {
    let report = render_text(&empty_comparison(), &ExtractionStatus::complete())?;

    assert_eq!(
        report,
        "Content changes:          0\n\
         Formatting-only changes:  0\n\
         Uncertain changes:        0\n\
         Unresolved regions:       0\n\
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
        ratio: 0.5,
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
        unsupported_regions: Vec::new(),
    };
    assert!(matches!(
        summarize(&empty_comparison(), &missing_region),
        Err(Error::InvalidConfiguration(message))
            if message.contains("incomplete old extraction")
    ));

    let blank_description = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        unsupported_regions: vec![UnsupportedRegion {
            side: DocumentSide::Old,
            description: "  ".to_owned(),
        }],
    };
    assert!(matches!(
        summarize(&empty_comparison(), &blank_description),
        Err(Error::InvalidConfiguration(message)) if message.contains("require a description")
    ));
}

#[test]
fn reports_incomplete_extraction_in_text_and_strict_status() -> Result<()> {
    let extraction = ExtractionStatus {
        old_complete: true,
        new_complete: false,
        unsupported_regions: vec![UnsupportedRegion {
            side: DocumentSide::New,
            description: "unsupported text stream".to_owned(),
        }],
    };

    let report = render_text(&empty_comparison(), &extraction)?;

    assert!(report.contains("Extraction complete:      old=yes, new=no"));
    assert!(report.contains("Unsupported region (new): unsupported text stream"));
    assert_eq!(
        exit_status(&empty_comparison(), &extraction, true)?,
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
        ratio: 2.0 / 3.0,
    };
    comparison.new_coverage = Coverage {
        resolved_tokens: 1,
        total_tokens: 2,
        ratio: 0.5,
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
        unsupported_regions: vec![UnsupportedRegion {
            side: DocumentSide::Old,
            description: "unknown stream operator".to_owned(),
        }],
    };
    let mut output = Vec::new();

    write_json(&mut output, &comparison, &extraction)?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["summary"]["content_changes"], 1);
    assert_eq!(
        json["summary"]["old_alignment_coverage"]["resolved_tokens"],
        2
    );
    assert_eq!(json["summary"]["new_alignment_coverage"]["total_tokens"], 2);
    assert_eq!(json["changes"][0]["kind"], "replacement");
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
    assert_eq!(
        json["extraction"]["unsupported_regions"][0]["description"],
        "unknown stream operator"
    );
    Ok(())
}

#[test]
fn rejects_inconsistent_coverage_before_rendering() {
    let mut comparison = empty_comparison();
    comparison.old_coverage = Coverage {
        resolved_tokens: 1,
        total_tokens: 2,
        ratio: 1.0,
    };

    assert!(matches!(
        render_text(&comparison, &ExtractionStatus::complete()),
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

fn empty_comparison() -> Comparison {
    Comparison {
        changes: Vec::new(),
        formatting_changes: Vec::new(),
        unresolved_regions: Vec::new(),
        old_coverage: Coverage {
            resolved_tokens: 0,
            total_tokens: 0,
            ratio: 1.0,
        },
        new_coverage: Coverage {
            resolved_tokens: 0,
            total_tokens: 0,
            ratio: 1.0,
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
