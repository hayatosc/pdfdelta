use pdfdelta_core::{
    Error, Result,
    alignment::{AlignmentEvidence, BlockSeparator, CandidateSource},
    diff::{
        Change, ChangeKind, ChangeOccurrence, ChangeTag, ChangedRegionProof, Comparison,
        Confidence, Coverage, FormattingChange, FormattingReason, ProvenChangedRegion, TextSpan,
        TokenRange, UnresolvedRegion,
    },
    layout::BlockId,
    model::{
        DecodedText, Document, FontId, FontProgramHash, Glyph, GlyphCropStatus, GlyphEvidence,
        GlyphId, GlyphPathClipStatus, GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    normalize::{
        BlockText, MappedText, NormalizationEvent, NormalizationKind, ScalarRange, SourceMapEntry,
        TextSource, TextSourceAtom, UnmappedToken,
    },
    pdf::ObjectRef,
    report::{
        DocumentSide, ExitStatus, ExtractionIssueRecord, ExtractionStatus, SpanSourceEvidence,
        SpanSourceProjectionLimits, TextReportOptions, exit_status, project_span_sources,
        project_span_sources_with_limits, render_glyph_overlay_svg, render_text, summarize,
        write_glyph_overlay_svg, write_json,
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
        "content changes: 0 · proven changed regions: 0 · formatting-only: 0 · uncertain: 0 · \
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
fn json_serializes_layout_formatting_reasons() -> Result<()> {
    let old_blocks = [block_with_pages(1, "stable text", &[0])];
    let new_blocks = [block_with_pages(101, "stable text", &[0, 1])];
    let mut comparison = empty_comparison();
    comparison.formatting_changes.push(FormattingChange {
        old_span: full_span(1, "stable text"),
        new_span: full_span(101, "stable text"),
        confidence: Confidence::High,
        reasons: vec![
            FormattingReason::FontSize,
            FormattingReason::Position,
            FormattingReason::LineBreak,
            FormattingReason::PageBreak,
        ],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;

    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");
    assert_eq!(
        json["formatting_only_changes"][0]["reasons"],
        serde_json::json!(["font_size", "position", "line_break", "page_break"])
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
fn proven_content_difference_changes_non_strict_exit_status() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.proven_changed_regions.push(ProvenChangedRegion {
        old_span: Some(span(1, 0, 4, 0, 4)),
        new_span: Some(span(101, 0, 4, 0, 4)),
        proof: ChangedRegionProof::ExactTokenMultisetMismatch,
        confidence: Confidence::High,
    });
    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), false)?,
        ExitStatus::ContentChanges
    );
    assert_eq!(
        exit_status(&comparison, &ExtractionStatus::complete(), true)?,
        ExitStatus::IncompleteComparison
    );
    let summary = summarize(&comparison, &ExtractionStatus::complete())?;
    assert_eq!(summary.content_changes, 0);
    assert!(!summary.comparison_complete);
    Ok(())
}

#[test]
fn reports_proven_content_difference_separately_from_exact_changes() -> Result<()> {
    let old_blocks = vec![block_with_text(12, "alpha beta")];
    let new_blocks = vec![block_with_text(112, "alpha delta")];
    let mut comparison = empty_comparison();
    comparison.proven_changed_regions.push(ProvenChangedRegion {
        old_span: Some(full_span(12, "alpha beta")),
        new_span: Some(full_span(112, "alpha delta")),
        proof: ChangedRegionProof::ExactTokenMultisetMismatch,
        confidence: Confidence::High,
    });

    let text = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;
    assert!(text.contains("content changes: 0 · proven changed regions: 1"));
    assert!(text.contains("PROVEN CONTENT DIFFERENCE"));
    assert!(text.contains("exact token multiset mismatch; confidence: high"));

    let mut output = Vec::new();
    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");
    assert_eq!(json["schema_version"], 10);
    assert_eq!(json["summary"]["content_changes"], 0);
    assert_eq!(json["summary"]["proven_changed_regions"], 1);
    assert_eq!(
        json["proven_changed_regions"][0]["proof"],
        "exact_token_multiset_mismatch"
    );
    Ok(())
}

#[test]
fn rejects_proven_change_with_a_proof_mismatched_span_shape() {
    let mut comparison = empty_comparison();
    comparison.proven_changed_regions.push(ProvenChangedRegion {
        old_span: Some(span(1, 0, 4, 0, 4)),
        new_span: None,
        proof: ChangedRegionProof::ExactTokenMultisetMismatch,
        confidence: Confidence::High,
    });

    assert!(matches!(
        summarize(&comparison, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("span shape does not match its proof")
    ));
}

#[test]
fn accepts_one_sided_non_empty_proven_change() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.proven_changed_regions.push(ProvenChangedRegion {
        old_span: None,
        new_span: Some(span(101, 0, 4, 0, 4)),
        proof: ChangedRegionProof::OneSidedNonEmptyRange,
        confidence: Confidence::High,
    });

    let summary = summarize(&comparison, &ExtractionStatus::complete())?;
    assert_eq!(summary.proven_changed_regions, 1);
    assert!(!summary.comparison_complete);
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
        report.contains(
            "content changes: 0 · proven changed regions: 0 · formatting-only: 0 · uncertain: 0"
        ),
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
fn reports_page_tree_gap_scope_without_synthesizing_a_page() -> Result<()> {
    let extraction = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unresolved,
            scope: ExtractionScope::PageGap { retained_before: 2 },
            description: "broken page tree branch".to_owned(),
        }],
    };
    let mut comparison = empty_comparison();
    comparison.old_coverage.ratio = None;

    let text = render_text(&[], &[], &comparison, &extraction, &plain_options())?;
    assert!(
        text.contains("scope=page-gap, retained-pages-before=2"),
        "{text}"
    );

    let mut output = Vec::new();
    write_json(&mut output, &[], &[], &[], &[], &comparison, &extraction)?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");
    assert_eq!(json["schema_version"], 10);
    assert_eq!(json["extraction"]["issues"][0]["scope"], "page_gap");
    assert_eq!(json["extraction"]["issues"][0]["retained_pages_before"], 2);
    assert!(json["extraction"]["issues"][0].get("page").is_none());
    Ok(())
}

#[test]
fn reports_localized_glyph_gap_scope() -> Result<()> {
    let extraction = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unsupported,
            scope: ExtractionScope::GlyphGap { retained_before: 3 },
            description: "unsupported Form content".to_owned(),
        }],
    };
    let mut comparison = empty_comparison();
    comparison.old_coverage.ratio = None;

    let text = render_text(&[], &[], &comparison, &extraction, &plain_options())?;
    assert!(
        text.contains("scope=glyph-gap, retained-glyphs-before=3"),
        "{text}"
    );

    let mut output = Vec::new();
    write_json(&mut output, &[], &[], &[], &[], &comparison, &extraction)?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");
    assert_eq!(json["extraction"]["issues"][0]["scope"], "glyph_gap");
    assert_eq!(json["extraction"]["issues"][0]["retained_glyphs_before"], 3);
    assert!(json["extraction"]["issues"][0].get("page").is_none());
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
        &[],
        &[],
        &comparison,
        &extraction,
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 10);
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
        json["changes"][0]["occurrences"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(json["changes"][0].get("old_span").is_none());
    assert!(json["changes"][0].get("new_span").is_none());
    let old_span = &json["changes"][0]["occurrences"][0]["old_span"];
    assert_eq!(old_span["block_separator"], serde_json::Value::Null);
    assert_eq!(old_span["canonical_range"]["start"], 1);
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
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 10);
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
fn mixed_boundary_patterns_survive_text_and_source_projection() -> Result<()> {
    let blocks = [
        sourced_block(1, "New", vec![glyph_entry(0, 3, 1)]),
        sourced_block(2, "York", vec![glyph_entry(0, 4, 2)]),
        sourced_block(3, "市", vec![glyph_entry(0, 1, 3)]),
    ];
    let span = TextSpan {
        blocks: vec![BlockId(1), BlockId(2), BlockId(3)],
        separator: Some(BlockSeparator::PerBoundary([true, false])),
        canonical_range: ScalarRange { start: 0, end: 9 },
        comparable_range: TokenRange { start: 0, end: 9 },
    };
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(span),
        new_span: None,
        evidence: vec![AlignmentEvidence::TextSimilarity],
    });
    let mut output = Vec::new();
    write_json(
        &mut output,
        &blocks,
        &[],
        &[glyph_evidence(1), glyph_evidence(2), glyph_evidence(3)],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let span = &json["unresolved_regions"][0]["old_span"];
    assert_eq!(span["text"], "New York市");
    assert_eq!(
        span["block_separators"],
        serde_json::json!(["space", "concatenate"])
    );
    let sources = span["sources"].as_array().expect("source evidence");
    assert_eq!(
        sources
            .iter()
            .filter(|source| source["kind"] == "block_separator_space")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn json_report_serializes_extraction_gap_evidence() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(full_span(12, "alpha beta gamma")),
        new_span: Some(full_span(112, "alpha delta gamma")),
        evidence: vec![AlignmentEvidence::ExtractionGap],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &[block_with_text(12, "alpha beta gamma")],
        &[block_with_text(112, "alpha delta gamma")],
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(
        json["unresolved_regions"][0]["evidence"][0],
        "extraction_gap"
    );
    Ok(())
}

#[test]
fn json_report_serializes_unknown_reading_order_evidence() -> Result<()> {
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(full_span(12, "alpha beta gamma")),
        new_span: Some(full_span(112, "alpha beta gamma")),
        evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &[block_with_text(12, "alpha beta gamma")],
        &[block_with_text(112, "alpha beta gamma")],
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 10);
    assert_eq!(
        json["unresolved_regions"][0]["evidence"][0],
        "reading_order_unknown"
    );
    Ok(())
}

#[test]
fn json_report_projects_replacement_glyph_provenance() -> Result<()> {
    let old_blocks = [sourced_block(1, "a", vec![glyph_entry(0, 1, 1)])];
    let new_blocks = [sourced_block(101, "b", vec![glyph_entry(0, 1, 2)])];
    let old_glyphs = [glyph_evidence(1)];
    let new_glyphs = [glyph_evidence(2)];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(1, "a")),
            new_span: Some(full_span(101, "b")),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &old_glyphs,
        &new_glyphs,
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let source = &json["changes"][0]["occurrences"][0]["old_span"]["sources"][0];

    assert_eq!(json["schema_version"], 10);
    assert_eq!(source["kind"], "glyph");
    assert_eq!(source["glyph_id"], 1);
    assert_eq!(source["page"], 0);
    assert_eq!(source["bbox"]["min"]["x"], 1.0);
    assert_eq!(source["bbox"]["max"]["y"], 4.0);
    assert_eq!(source["content_stream"]["object_number"], 11);
    assert_eq!(source["content_stream"]["generation"], 0);
    assert_eq!(source["operator_index"], 21);
    Ok(())
}

#[test]
fn json_report_preserves_zero_length_replacement_boundary() -> Result<()> {
    let old_blocks = [block_with_text(1, "x,")];
    let new_blocks = [block_with_text(101, "x")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(span(1, 1, 2, 1, 2)),
            new_span: Some(span(101, 1, 1, 1, 1)),
        }],
        confidence: Confidence::Medium,
        tags: Vec::new(),
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let occurrence = &json["changes"][0]["occurrences"][0];

    assert_eq!(occurrence["old_span"]["text"], ",");
    assert_eq!(occurrence["new_span"]["text"], "");
    assert_eq!(occurrence["new_span"]["canonical_range"]["start"], 1);
    assert_eq!(occurrence["new_span"]["canonical_range"]["end"], 1);
    Ok(())
}

#[test]
fn json_report_deduplicates_expansion_and_preserves_contraction_sources() -> Result<()> {
    let old_blocks = [sourced_block(
        1,
        "ffi",
        vec![SourceMapEntry {
            output_range: ScalarRange { start: 0, end: 3 },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(1))].into(),
            },
        }],
    )];
    let new_blocks = [sourced_block(
        101,
        "é",
        vec![SourceMapEntry {
            output_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: vec![
                    TextSourceAtom::Glyph(GlyphId(2)),
                    TextSourceAtom::Glyph(GlyphId(3)),
                ]
                .into(),
            },
        }],
    )];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(1, "ffi")),
            new_span: Some(full_span(101, "é")),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &[glyph_evidence(1)],
        &[glyph_evidence(2), glyph_evidence(3)],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");

    assert_eq!(
        json["changes"][0]["occurrences"][0]["old_span"]["sources"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        json["changes"][0]["occurrences"][0]["new_span"]["sources"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    Ok(())
}

#[test]
fn json_report_keeps_synthetic_unmapped_and_separator_sources_distinct() -> Result<()> {
    let old_blocks = [sourced_block(
        1,
        " ",
        vec![SourceMapEntry {
            output_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: vec![TextSourceAtom::SyntheticSpace {
                    preceding: GlyphId(1),
                    following: GlyphId(2),
                }]
                .into(),
            },
        }],
    )];
    let unmapped = BlockText {
        block: BlockId(101),
        canonical: MappedText {
            text: String::new(),
            source_map: Vec::new(),
            unmapped: vec![UnmappedToken {
                scalar_index: 0,
                font_hash: FontProgramHash(vec![1]),
                glyph_id: 9,
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(3))].into(),
                },
            }],
        },
        ..block_with_text(101, "")
    };
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(1, " ")),
            new_span: Some(TextSpan {
                blocks: vec![BlockId(101)],
                separator: None,
                canonical_range: ScalarRange { start: 0, end: 0 },
                comparable_range: TokenRange { start: 0, end: 1 },
            }),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    let mut output = Vec::new();
    write_json(
        &mut output,
        &old_blocks,
        &[unmapped],
        &[glyph_evidence(1), glyph_evidence(2)],
        &[glyph_evidence(3)],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let old_sources = json["changes"][0]["occurrences"][0]["old_span"]["sources"]
        .as_array()
        .expect("sources array");

    assert_eq!(old_sources[0]["kind"], "synthetic_space");
    assert!(old_sources[0].get("content_stream").is_none());
    assert_eq!(old_sources[1]["kind"], "glyph");
    assert_eq!(old_sources[2]["kind"], "glyph");
    assert_eq!(
        json["changes"][0]["occurrences"][0]["new_span"]["sources"][0]["glyph_id"],
        3
    );

    let grouped_blocks = [
        sourced_block(10, "a", vec![glyph_entry(0, 1, 1)]),
        sourced_block(11, "b", vec![glyph_entry(0, 1, 2)]),
    ];
    let mut grouped = empty_comparison();
    grouped.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(TextSpan {
            blocks: vec![BlockId(10), BlockId(11)],
            separator: Some(BlockSeparator::Space),
            canonical_range: ScalarRange { start: 0, end: 3 },
            comparable_range: TokenRange { start: 0, end: 3 },
        }),
        new_span: None,
        evidence: vec![AlignmentEvidence::TextSimilarity],
    });
    let mut grouped_output = Vec::new();
    write_json(
        &mut grouped_output,
        &grouped_blocks,
        &[],
        &[glyph_evidence(1), glyph_evidence(2)],
        &[],
        &grouped,
        &ExtractionStatus::complete(),
    )?;
    let grouped_json: serde_json::Value =
        serde_json::from_slice(&grouped_output).expect("valid grouped JSON report");
    assert!(
        grouped_json["unresolved_regions"][0]["old_span"]["sources"]
            .as_array()
            .expect("sources array")
            .iter()
            .any(|source| source["kind"] == "block_separator_space")
    );
    Ok(())
}

#[test]
fn json_group_sources_skip_separator_space_at_existing_whitespace_boundaries() -> Result<()> {
    let cases = [
        (
            "trailing whitespace",
            vec![
                sourced_block(10, "a ", vec![glyph_entry(0, 1, 1), glyph_entry(1, 2, 2)]),
                sourced_block(11, "b", vec![glyph_entry(0, 1, 3)]),
            ],
            vec![glyph_evidence(1), glyph_evidence(2), glyph_evidence(3)],
            [1, 2, 3],
        ),
        (
            "leading whitespace",
            vec![
                sourced_block(20, "a", vec![glyph_entry(0, 1, 4)]),
                sourced_block(21, " b", vec![glyph_entry(0, 1, 5), glyph_entry(1, 2, 6)]),
            ],
            vec![glyph_evidence(4), glyph_evidence(5), glyph_evidence(6)],
            [4, 5, 6],
        ),
    ];

    for (case, blocks, glyphs, expected_glyphs) in cases {
        let mut comparison = empty_comparison();
        comparison.unresolved_regions.push(UnresolvedRegion {
            old_span: Some(TextSpan {
                blocks: blocks.iter().map(|block| block.block).collect(),
                separator: Some(BlockSeparator::Space),
                canonical_range: ScalarRange { start: 0, end: 3 },
                comparable_range: TokenRange { start: 0, end: 3 },
            }),
            new_span: None,
            evidence: vec![AlignmentEvidence::TextSimilarity],
        });
        let mut output = Vec::new();
        write_json(
            &mut output,
            &blocks,
            &[],
            &glyphs,
            &[],
            &comparison,
            &ExtractionStatus::complete(),
        )?;
        let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
        let span = &json["unresolved_regions"][0]["old_span"];
        assert_eq!(span["text"], "a b", "{case}");
        assert_eq!(span["block_separator"], "space", "{case}");
        let sources = span["sources"].as_array().expect("sources array");
        assert_eq!(sources.len(), expected_glyphs.len(), "{case}");
        for (source, glyph_id) in sources.iter().zip(expected_glyphs) {
            assert_eq!(source["kind"], "glyph", "{case}");
            assert_eq!(source["glyph_id"], glyph_id, "{case}");
        }
    }
    Ok(())
}

#[test]
fn json_report_rejects_duplicate_and_unknown_glyph_evidence() {
    let blocks = [sourced_block(1, "a", vec![glyph_entry(0, 1, 1)])];
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(full_span(1, "a")),
        new_span: None,
        evidence: vec![AlignmentEvidence::TextSimilarity],
    });

    let duplicate = write_json(
        Vec::new(),
        &blocks,
        &[],
        &[glyph_evidence(1), glyph_evidence(1)],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    );
    let unknown = write_json(
        Vec::new(),
        &blocks,
        &[],
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    );

    assert!(
        matches!(duplicate, Err(Error::Report(message)) if message.contains("duplicate glyph evidence"))
    );
    assert!(
        matches!(unknown, Err(Error::Report(message)) if message.contains("missing glyph evidence"))
    );
}

#[test]
fn source_projection_matches_json_source_order_and_fields() -> Result<()> {
    let blocks = [sourced_block(
        1,
        " ",
        vec![SourceMapEntry {
            output_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: vec![TextSourceAtom::SyntheticSpace {
                    preceding: GlyphId(1),
                    following: GlyphId(2),
                }]
                .into(),
            },
        }],
    )];
    let glyphs = [glyph_evidence(1), glyph_evidence(2)];
    let span = full_span(1, " ");
    let sources = project_span_sources(&blocks, &glyphs, &span)?;

    assert_eq!(
        sources,
        vec![
            SpanSourceEvidence::SyntheticSpace {
                preceding_glyph_id: GlyphId(1),
                following_glyph_id: GlyphId(2),
            },
            SpanSourceEvidence::Glyph {
                glyph_id: GlyphId(1),
                page: PageId(0),
                bbox: glyphs[0].bbox,
                content_stream: glyphs[0].provenance.content_stream,
                operator_index: glyphs[0].provenance.operator_index,
            },
            SpanSourceEvidence::Glyph {
                glyph_id: GlyphId(2),
                page: PageId(0),
                bbox: glyphs[1].bbox,
                content_stream: glyphs[1].provenance.content_stream,
                operator_index: glyphs[1].provenance.operator_index,
            },
        ]
    );

    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(span),
        new_span: None,
        evidence: vec![AlignmentEvidence::TextSimilarity],
    });
    let mut output = Vec::new();
    write_json(
        &mut output,
        &blocks,
        &[],
        &glyphs,
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let json_sources = json["unresolved_regions"][0]["old_span"]["sources"]
        .as_array()
        .expect("sources array");
    assert_eq!(json_sources.len(), sources.len());
    assert_eq!(json_sources[0]["kind"], "synthetic_space");
    assert_eq!(json_sources[1]["kind"], "glyph");
    assert_eq!(json_sources[1]["glyph_id"], 1);
    assert_eq!(json_sources[2]["kind"], "glyph");
    assert_eq!(json_sources[2]["glyph_id"], 2);
    Ok(())
}

#[test]
fn source_projection_rejects_invalid_ranges_duplicates_and_limits() {
    let blocks = [sourced_block(1, "a", vec![glyph_entry(0, 1, 1)])];
    let glyphs = [glyph_evidence(1)];
    let invalid_span = TextSpan {
        canonical_range: ScalarRange { start: 0, end: 0 },
        ..full_span(1, "a")
    };
    let reversed_span = TextSpan {
        comparable_range: TokenRange { start: 1, end: 0 },
        ..full_span(1, "a")
    };
    let out_of_bounds_span = TextSpan {
        comparable_range: TokenRange { start: 2, end: 2 },
        ..full_span(1, "a")
    };
    let duplicate_blocks = [blocks[0].clone(), blocks[0].clone()];
    let duplicate_glyphs = [glyphs[0], glyphs[0]];
    let limited_block = sourced_block(
        2,
        " ",
        vec![SourceMapEntry {
            output_range: ScalarRange { start: 0, end: 1 },
            source: TextSource {
                atoms: vec![TextSourceAtom::SyntheticSpace {
                    preceding: GlyphId(1),
                    following: GlyphId(2),
                }]
                .into(),
            },
        }],
    );
    let limits = SpanSourceProjectionLimits {
        max_comparable_tokens: 1,
        max_evidence_items: 2,
    };

    assert!(matches!(
        project_span_sources(&blocks, &glyphs, &invalid_span),
        Err(Error::InvalidConfiguration(message))
            if message.contains("canonical and comparable ranges")
    ));
    assert!(matches!(
        project_span_sources(&blocks, &glyphs, &reversed_span),
        Err(Error::InvalidConfiguration(message)) if message.contains("ranges must be ordered")
    ));
    assert!(matches!(
        project_span_sources(&blocks, &glyphs, &out_of_bounds_span),
        Err(Error::InvalidConfiguration(message)) if message.contains("range exceeds")
    ));
    assert!(matches!(
        project_span_sources(&duplicate_blocks, &glyphs, &full_span(1, "a")),
        Err(Error::InvalidConfiguration(message)) if message.contains("duplicate block id")
    ));
    assert!(matches!(
        project_span_sources(&blocks, &duplicate_glyphs, &full_span(1, "a")),
        Err(Error::Report(message)) if message.contains("duplicate glyph evidence")
    ));
    assert!(matches!(
        project_span_sources_with_limits(
            &[limited_block],
            &[glyph_evidence(1), glyph_evidence(2)],
            &full_span(2, " "),
            limits,
        ),
        Err(Error::LimitExceeded {
            resource: "span source output evidence",
            limit: 2
        })
    ));

    let atom_heavy_block = sourced_block(3, "abc", vec![glyph_entry(0, 3, 1)]);
    assert!(matches!(
        project_span_sources_with_limits(
            &[atom_heavy_block],
            &[glyph_evidence(1)],
            &full_span(3, "abc"),
            SpanSourceProjectionLimits {
                max_comparable_tokens: 3,
                max_evidence_items: 2,
            },
        ),
        Err(Error::LimitExceeded {
            resource: "span source atom traversal",
            limit: 2
        })
    ));
}

#[test]
fn source_projection_preserves_boundary_event_order_without_stable_sort() -> Result<()> {
    let mut first = sourced_block(1, "a", Vec::new());
    first.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::SoftLineBreak,
        raw_range: ScalarRange { start: 1, end: 1 },
        canonical_range: ScalarRange { start: 1, end: 1 },
        source: TextSource {
            atoms: vec![TextSourceAtom::LineBreak {
                preceding: GlyphId(1),
                following: GlyphId(2),
            }]
            .into(),
        },
    });
    let mut second = sourced_block(2, "b", Vec::new());
    second.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::SoftLineBreak,
        raw_range: ScalarRange { start: 0, end: 0 },
        canonical_range: ScalarRange { start: 0, end: 0 },
        source: TextSource {
            atoms: vec![TextSourceAtom::SyntheticSpace {
                preceding: GlyphId(3),
                following: GlyphId(4),
            }]
            .into(),
        },
    });
    let sources = project_span_sources(
        &[first, second],
        &[
            glyph_evidence(1),
            glyph_evidence(2),
            glyph_evidence(3),
            glyph_evidence(4),
        ],
        &TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(BlockSeparator::Concatenate),
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: TokenRange { start: 0, end: 2 },
        },
    )?;

    assert!(matches!(sources[0], SpanSourceEvidence::LineBreak { .. }));
    assert!(matches!(
        sources[1],
        SpanSourceEvidence::Glyph {
            glyph_id: GlyphId(1),
            ..
        }
    ));
    assert!(matches!(
        sources[2],
        SpanSourceEvidence::Glyph {
            glyph_id: GlyphId(2),
            ..
        }
    ));
    assert!(matches!(
        sources[3],
        SpanSourceEvidence::SyntheticSpace { .. }
    ));
    assert!(matches!(
        sources[4],
        SpanSourceEvidence::Glyph {
            glyph_id: GlyphId(3),
            ..
        }
    ));
    assert!(matches!(
        sources[5],
        SpanSourceEvidence::Glyph {
            glyph_id: GlyphId(4),
            ..
        }
    ));
    Ok(())
}

#[test]
fn source_projection_separator_preflight_accepts_exact_token_limit() -> Result<()> {
    let blocks = [
        sourced_block(1, "a", vec![glyph_entry(0, 1, 1)]),
        sourced_block(2, "b", vec![glyph_entry(0, 1, 2)]),
    ];
    let sources = project_span_sources_with_limits(
        &blocks,
        &[glyph_evidence(1), glyph_evidence(2)],
        &TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(BlockSeparator::Concatenate),
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: TokenRange { start: 0, end: 2 },
        },
        SpanSourceProjectionLimits {
            max_comparable_tokens: 2,
            max_evidence_items: 8,
        },
    )?;

    assert_eq!(sources.len(), 2);
    Ok(())
}

#[test]
fn json_report_projects_deleted_line_break_and_hyphenation_evidence() -> Result<()> {
    let mut cjk = sourced_block(
        1,
        "東京特許",
        (0..4)
            .map(|index| glyph_entry(index, index + 1, index as u64 + 1))
            .collect(),
    );
    cjk.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::SoftLineBreak,
        raw_range: ScalarRange { start: 2, end: 3 },
        canonical_range: ScalarRange { start: 2, end: 2 },
        source: TextSource {
            atoms: vec![TextSourceAtom::LineBreak {
                preceding: GlyphId(2),
                following: GlyphId(3),
            }]
            .into(),
        },
    });
    let mut hyphenation = sourced_block(
        2,
        "administration",
        (0..14)
            .map(|index| glyph_entry(index, index + 1, index as u64 + 10))
            .collect(),
    );
    hyphenation.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::HyphenationJoin,
        raw_range: ScalarRange { start: 7, end: 9 },
        canonical_range: ScalarRange { start: 7, end: 7 },
        source: TextSource {
            atoms: vec![
                TextSourceAtom::Glyph(GlyphId(99)),
                TextSourceAtom::LineBreak {
                    preceding: GlyphId(16),
                    following: GlyphId(17),
                },
            ]
            .into(),
        },
    });
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.extend([
        UnresolvedRegion {
            old_span: Some(full_span(1, "東京特許")),
            new_span: None,
            evidence: vec![AlignmentEvidence::TextSimilarity],
        },
        UnresolvedRegion {
            old_span: Some(full_span(2, "administration")),
            new_span: None,
            evidence: vec![AlignmentEvidence::TextSimilarity],
        },
    ]);
    let mut evidence = (1..=4).map(glyph_evidence).collect::<Vec<_>>();
    evidence.extend((10..=23).map(glyph_evidence));
    evidence.push(glyph_evidence(99));
    let mut output = Vec::new();

    write_json(
        &mut output,
        &[cjk, hyphenation],
        &[],
        &evidence,
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let cjk_sources = json["unresolved_regions"][0]["old_span"]["sources"]
        .as_array()
        .expect("CJK sources");
    let hyphen_sources = json["unresolved_regions"][1]["old_span"]["sources"]
        .as_array()
        .expect("hyphenation sources");

    let cjk_preceding = source_position(cjk_sources, "glyph", Some(2));
    let cjk_break = source_position(cjk_sources, "line_break", None);
    let cjk_following = source_position(cjk_sources, "glyph", Some(3));
    assert!(cjk_preceding < cjk_break && cjk_break < cjk_following);

    let hyphen_preceding = source_position(hyphen_sources, "glyph", Some(16));
    let deleted_hyphen = source_position(hyphen_sources, "glyph", Some(99));
    let hyphen_break = source_position(hyphen_sources, "line_break", None);
    let hyphen_following = source_position(hyphen_sources, "glyph", Some(17));
    assert!(hyphen_preceding < deleted_hyphen);
    assert!(deleted_hyphen < hyphen_break);
    assert!(hyphen_break < hyphen_following);
    Ok(())
}

#[test]
fn json_report_orders_concatenated_boundary_events_before_the_next_token() -> Result<()> {
    let mut previous = sourced_block(1, "a", vec![glyph_entry(0, 1, 1)]);
    previous.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::SoftLineBreak,
        raw_range: ScalarRange { start: 1, end: 1 },
        canonical_range: ScalarRange { start: 1, end: 1 },
        source: TextSource {
            atoms: vec![TextSourceAtom::Glyph(GlyphId(99))].into(),
        },
    });
    let mut next = sourced_block(2, "b", vec![glyph_entry(0, 1, 2)]);
    next.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::Nfc,
        raw_range: ScalarRange { start: 0, end: 0 },
        canonical_range: ScalarRange { start: 0, end: 0 },
        source: TextSource {
            atoms: vec![TextSourceAtom::Glyph(GlyphId(100))].into(),
        },
    });
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(TextSpan {
            blocks: vec![BlockId(1), BlockId(2)],
            separator: Some(BlockSeparator::Concatenate),
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: TokenRange { start: 0, end: 2 },
        }),
        new_span: None,
        evidence: vec![AlignmentEvidence::TextSimilarity],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &[previous, next],
        &[],
        &[
            glyph_evidence(1),
            glyph_evidence(2),
            glyph_evidence(99),
            glyph_evidence(100),
        ],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let sources = json["unresolved_regions"][0]["old_span"]["sources"]
        .as_array()
        .expect("sources array");
    let glyph_ids = sources
        .iter()
        .filter_map(|source| source["glyph_id"].as_u64())
        .collect::<Vec<_>>();

    assert_eq!(glyph_ids, [1, 99, 100, 2]);
    Ok(())
}

#[test]
fn json_report_attributes_zero_width_events_only_to_interior_or_matching_points() -> Result<()> {
    let mut block = sourced_block(
        1,
        "abcd",
        (0..4)
            .map(|index| glyph_entry(index, index + 1, index as u64 + 1))
            .collect(),
    );
    block.normalization_events.push(NormalizationEvent {
        kind: NormalizationKind::SoftLineBreak,
        raw_range: ScalarRange { start: 2, end: 3 },
        canonical_range: ScalarRange { start: 2, end: 2 },
        source: TextSource {
            atoms: vec![TextSourceAtom::LineBreak {
                preceding: GlyphId(2),
                following: GlyphId(3),
            }]
            .into(),
        },
    });
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.extend([
        UnresolvedRegion {
            old_span: Some(TextSpan {
                blocks: vec![BlockId(1)],
                separator: None,
                canonical_range: ScalarRange { start: 2, end: 2 },
                comparable_range: TokenRange { start: 2, end: 2 },
            }),
            new_span: None,
            evidence: vec![AlignmentEvidence::TextSimilarity],
        },
        UnresolvedRegion {
            old_span: Some(TextSpan {
                blocks: vec![BlockId(1)],
                separator: None,
                canonical_range: ScalarRange { start: 0, end: 2 },
                comparable_range: TokenRange { start: 0, end: 2 },
            }),
            new_span: None,
            evidence: vec![AlignmentEvidence::TextSimilarity],
        },
    ]);
    let mut output = Vec::new();

    write_json(
        &mut output,
        &[block],
        &[],
        &(1..=4).map(glyph_evidence).collect::<Vec<_>>(),
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON report");
    let point_sources = json["unresolved_regions"][0]["old_span"]["sources"]
        .as_array()
        .expect("point sources");
    let adjacent_sources = json["unresolved_regions"][1]["old_span"]["sources"]
        .as_array()
        .expect("adjacent sources");

    assert!(
        point_sources
            .iter()
            .any(|source| source["kind"] == "line_break")
    );
    assert!(
        adjacent_sources
            .iter()
            .all(|source| source["kind"] != "line_break")
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

    write_json(&mut output, &[], &[], &[], &[], &comparison, &extraction)?;
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
    let mut missing_occurrence = content_comparison();
    missing_occurrence.changes[0].occurrences.clear();
    assert!(matches!(
        summarize(&missing_occurrence, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("at least one occurrence")
    ));

    let mut missing_span = empty_comparison();
    missing_span.changes.push(Change {
        kind: ChangeKind::Insertion,
        occurrences: vec![ChangeOccurrence {
            old_span: None,
            new_span: None,
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    assert!(matches!(
        summarize(&missing_span, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("invalid Insertion change span shape")
    ));

    let mut reversed_range = content_comparison();
    reversed_range.changes[0].occurrences[0]
        .old_span
        .as_mut()
        .expect("fixture replacement should have an old span")
        .canonical_range = ScalarRange { start: 2, end: 1 };
    assert!(matches!(
        summarize(&reversed_range, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message)) if message.contains("ranges must be ordered")
    ));

    let mut empty_blocks = content_comparison();
    empty_blocks.changes[0].occurrences[0]
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
    single_block_separator.changes[0].occurrences[0]
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
    missing_multi_block_separator.changes[0].occurrences[0]
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

    let mut one_sided_exact_replacement = content_comparison();
    let empty_new = one_sided_exact_replacement.changes[0].occurrences[0]
        .new_span
        .as_mut()
        .expect("fixture replacement should have a new span");
    empty_new.canonical_range = ScalarRange { start: 1, end: 1 };
    empty_new.comparable_range = TokenRange { start: 1, end: 1 };
    assert!(summarize(&one_sided_exact_replacement, &ExtractionStatus::complete()).is_ok());

    let mut mismatched_empty_coordinates = content_comparison();
    let mismatched_new = mismatched_empty_coordinates.changes[0].occurrences[0]
        .new_span
        .as_mut()
        .expect("fixture replacement should have a new span");
    mismatched_new.canonical_range = ScalarRange { start: 0, end: 1 };
    mismatched_new.comparable_range = TokenRange { start: 1, end: 1 };
    assert!(matches!(
        summarize(
            &mismatched_empty_coordinates,
            &ExtractionStatus::complete()
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("zero-width canonical ranges")
    ));

    let mut empty_change = one_sided_exact_replacement;
    let empty_old = empty_change.changes[0].occurrences[0]
        .old_span
        .as_mut()
        .expect("fixture replacement should have an old span");
    empty_old.canonical_range = ScalarRange { start: 1, end: 1 };
    empty_old.comparable_range = TokenRange { start: 1, end: 1 };
    assert!(matches!(
        summarize(&empty_change, &ExtractionStatus::complete()),
        Err(Error::InvalidConfiguration(message))
            if message.contains("changed comparable tokens")
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(2, "Release 10 remains available")),
            new_span: Some(full_span(102, "Release 20 remains available")),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Insertion,
        occurrences: vec![ChangeOccurrence {
            old_span: None,
            new_span: Some(full_span(103, "Inserted paragraph appears here")),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Deletion,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(3, "Removed paragraph disappears")),
            new_span: None,
        }],
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
        "content changes: 3 · proven changed regions: 0 · formatting-only: 0 · uncertain: 0 · \
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
fn text_report_renders_change_tags_in_hunk_headers() -> Result<()> {
    let old_blocks = vec![block_with_text(2, "Ａ")];
    let new_blocks = vec![block_with_text(102, "A")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(2, "Ａ")),
            new_span: Some(full_span(102, "A")),
        }],
        confidence: Confidence::High,
        tags: vec![ChangeTag::CharacterWidth, ChangeTag::OcrConfusion],
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert!(
        report.contains(
            "@@ page 1 · old block 2 -> new block 102 · confidence: high · \
             tags: character_width,ocr_confusion @@"
        ),
        "{report}"
    );
    Ok(())
}

#[test]
fn text_report_keeps_different_tag_sets_in_separate_hunks() -> Result<()> {
    let old_blocks = vec![block_with_text(7, "abc")];
    let new_blocks = vec![block_with_text(107, "xyz")];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(7, 0, 1)),
            new_span: Some(range_span(107, 0, 1)),
        }],
        confidence: Confidence::High,
        tags: vec![ChangeTag::CharacterWidth],
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(7, 2, 3)),
            new_span: Some(range_span(107, 2, 3)),
        }],
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

    assert_eq!(report.matches("@@ page 1").count(), 2, "{report}");
    assert_eq!(
        report.matches("tags: character_width").count(),
        1,
        "{report}"
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(7, 14, 15)),
            new_span: Some(range_span(9, 14, 15)),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(7, 15, 16)),
            new_span: Some(range_span(9, 15, 16)),
        }],
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
fn text_report_groups_unsorted_occurrences_by_compatible_block_coordinates() -> Result<()> {
    let old_blocks = vec![
        block_with_text(7, "alpha Release 10 omega"),
        block_with_text(8, "beta 3"),
    ];
    let new_blocks = vec![
        block_with_text(107, "alpha Release 21 omega"),
        block_with_text(108, "beta 4"),
    ];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![
            ChangeOccurrence {
                old_span: Some(range_span(7, 15, 16)),
                new_span: Some(range_span(107, 15, 16)),
            },
            ChangeOccurrence {
                old_span: Some(range_span(8, 5, 6)),
                new_span: Some(range_span(108, 5, 6)),
            },
            ChangeOccurrence {
                old_span: Some(range_span(7, 14, 15)),
                new_span: Some(range_span(107, 14, 15)),
            },
        ],
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

    assert!(
        report.contains("old groups [block 7; block 8] -> new groups [block 107; block 108]"),
        "{report}"
    );
    assert_eq!(
        report.matches("- alpha Release 10 omega").count(),
        1,
        "{report}"
    );
    assert_eq!(
        report.matches("+ alpha Release 21 omega").count(),
        1,
        "{report}"
    );
    assert_eq!(report.matches("- beta 3").count(), 1, "{report}");
    assert_eq!(report.matches("+ beta 4").count(), 1, "{report}");
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(5, 0, 1)),
            new_span: Some(range_span(105, 0, 1)),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(5, 88, 89)),
            new_span: Some(range_span(105, 88, 89)),
        }],
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(11, 81, 82)),
            new_span: Some(range_span(111, 81, 82)),
        }],
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(3, "Shared paragraph text")),
            new_span: Some(full_span(8, "Shared paragraph text")),
        }],
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
fn text_report_sorts_and_deduplicates_multi_occurrence_move_pages() -> Result<()> {
    let old_blocks = vec![
        block_with_pages(3, "First moved text", &[4]),
        block_with_pages(4, "Second moved text", &[1]),
    ];
    let new_blocks = vec![
        block_with_pages(8, "First moved text", &[5]),
        block_with_pages(9, "Second moved text", &[2]),
    ];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Move,
        occurrences: vec![
            ChangeOccurrence {
                old_span: Some(full_span(3, "First moved text")),
                new_span: Some(full_span(8, "First moved text")),
            },
            ChangeOccurrence {
                old_span: Some(full_span(4, "Second moved text")),
                new_span: Some(full_span(9, "Second moved text")),
            },
            ChangeOccurrence {
                old_span: Some(full_span(3, "First moved text")),
                new_span: Some(full_span(8, "First moved text")),
            },
        ],
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
        report.contains("~ moved from pages 2,5 to pages 3,6"),
        "{report}"
    );
    assert!(
        report.contains("old groups [block 3; block 4] -> new groups [block 8; block 9]"),
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
        "content changes: 0 · proven changed regions: 0 · formatting-only: 0 · uncertain: 0 · \
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
fn text_report_renders_extraction_gap_evidence() -> Result<()> {
    let old_blocks = vec![block_with_text(12, "alpha beta gamma")];
    let new_blocks = vec![block_with_text(112, "alpha delta gamma")];
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(full_span(12, "alpha beta gamma")),
        new_span: Some(full_span(112, "alpha delta gamma")),
        evidence: vec![AlignmentEvidence::ExtractionGap],
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert!(
        report.contains("? could not safely align this region (evidence: extraction_gap)"),
        "{report}"
    );
    Ok(())
}

#[test]
fn text_report_renders_unknown_reading_order_evidence() -> Result<()> {
    let old_blocks = vec![block_with_text(12, "alpha beta gamma")];
    let new_blocks = vec![block_with_text(112, "alpha beta gamma")];
    let mut comparison = empty_comparison();
    comparison.unresolved_regions.push(UnresolvedRegion {
        old_span: Some(full_span(12, "alpha beta gamma")),
        new_span: Some(full_span(112, "alpha beta gamma")),
        evidence: vec![AlignmentEvidence::ReadingOrderUnknown],
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert!(
        report.contains("? could not safely align this region (evidence: reading_order_unknown)"),
        "{report}"
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(7, 14, 16)),
            new_span: Some(range_span(9, 14, 16)),
        }],
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(TextSpan {
                blocks: vec![BlockId(9)],
                separator: None,
                canonical_range: ScalarRange { start: 0, end: 2 },
                comparable_range: TokenRange { start: 0, end: 4 },
            }),
            new_span: Some(full_span(109, "Release 20")),
        }],
        confidence: Confidence::Low,
        tags: vec![ChangeTag::OcrConfusion],
    });
    let mut output = Vec::new();

    write_json(
        &mut output,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("report should be valid JSON");

    assert_eq!(json["schema_version"], 10);
    assert_eq!(json["changes"][0]["kind"], "replacement");
    assert_eq!(json["changes"][0]["confidence"], "low");
    assert_eq!(json["changes"][0]["tags"][0], "ocr_confusion");
    let old_span = &json["changes"][0]["occurrences"][0]["old_span"];
    assert_eq!(old_span["blocks"][0], 9);
    assert_eq!(old_span["pages"][0], 4);
    assert_eq!(old_span["text"], "ab");
    // Two distinct unmapped glyphs keep their stable identity and their
    // order relative to the resolved scalars (leading and trailing).
    let unmapped = &old_span["unmapped_tokens"];
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
    let new_span = &json["changes"][0]["occurrences"][0]["new_span"];
    assert_eq!(new_span["pages"][0], 0);
    assert_eq!(new_span["text"], "Release 20");
    assert_eq!(
        new_span["unmapped_tokens"]
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(TextSpan {
                blocks: vec![BlockId(9)],
                separator: None,
                canonical_range: ScalarRange { start: 0, end: 2 },
                comparable_range: TokenRange { start: 0, end: 4 },
            }),
            new_span: Some(full_span(109, "Release 20")),
        }],
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
        occurrences: vec![ChangeOccurrence {
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
        }],
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
        "content changes: 1 · proven changed regions: 0 · formatting-only: 0 · uncertain: 1 · \
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(range_span(7, 0, 9)),
            new_span: None,
        }],
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(TextSpan {
                blocks: vec![BlockId(7)],
                separator: None,
                canonical_range: ScalarRange { start: 0, end: 9 },
                comparable_range: TokenRange { start: 0, end: 3 },
            }),
            new_span: None,
        }],
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
            occurrences: vec![ChangeOccurrence {
                old_span: Some(range_span(7, 0, 1)),
                new_span: None,
            }],
            confidence: Confidence::High,
            tags: Vec::new(),
        });
        comparison.changes.push(Change {
            kind: ChangeKind::Deletion,
            occurrences: vec![ChangeOccurrence {
                old_span: Some(second),
                new_span: None,
            }],
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

    // Second span's range is inverted (start > end).
    let mut second = range_span(7, 2, 3);
    second.canonical_range = ScalarRange { start: 5, end: 2 };
    assert!(matches!(
        render_text(
            &old_blocks,
            &new_blocks,
            &comparable_overrun(second),
            &ExtractionStatus::complete(),
            &plain_options(),
        ),
        Err(Error::InvalidConfiguration(message))
            if message.contains("must be ordered")
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(group_span(&[1, 2], Some(BlockSeparator::Space))),
            new_span: Some(group_span(&[101, 102], Some(BlockSeparator::Space))),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            // Same blocks but a different separator means a different coordinate
            // system, so the adjacent edit must not coalesce into one hunk.
            old_span: Some(group_span(&[1, 2], Some(BlockSeparator::Concatenate))),
            new_span: Some(group_span(&[101, 102], Some(BlockSeparator::Concatenate))),
        }],
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
        proven_changed_regions: Vec::new(),
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
        occurrences: vec![ChangeOccurrence {
            old_span: Some(span(1, 1, 2, 1, 2)),
            new_span: Some(span(101, 1, 2, 1, 2)),
        }],
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
        role: pdfdelta_core::layout::BlockRole::Body,
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
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: None,
        page_breaks: None,
    }
}

fn sourced_block(id: u64, text: &str, source_map: Vec<SourceMapEntry>) -> BlockText {
    BlockText {
        canonical: MappedText {
            text: text.to_owned(),
            source_map,
            unmapped: Vec::new(),
        },
        ..block_with_text(id, text)
    }
}

fn glyph_entry(start: usize, end: usize, glyph: u64) -> SourceMapEntry {
    SourceMapEntry {
        output_range: ScalarRange { start, end },
        source: TextSource {
            atoms: vec![TextSourceAtom::Glyph(GlyphId(glyph))].into(),
        },
    }
}

fn glyph_evidence(id: u64) -> GlyphEvidence {
    GlyphEvidence {
        id: GlyphId(id),
        page: PageId(0),
        bbox: Rect {
            min: Vec2 { x: 1.0, y: 2.0 },
            max: Vec2 { x: 3.0, y: 4.0 },
        },
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 10 + id as u32,
                generation: 0,
            },
            operator_index: 20 + id as u32,
        },
    }
}

fn source_position(sources: &[serde_json::Value], kind: &str, glyph_id: Option<u64>) -> usize {
    sources
        .iter()
        .position(|source| {
            source["kind"] == kind && glyph_id.is_none_or(|glyph_id| source["glyph_id"] == glyph_id)
        })
        .expect("expected source should be present")
}

/// Canonical text "ab" with two distinct unmapped glyph tokens: one before
/// 'a' and one after 'b', exercising leading/trailing positions and order.
fn unmapped_block_fixture(id: u64) -> BlockText {
    BlockText {
        block: BlockId(id),
        role: pdfdelta_core::layout::BlockRole::Body,
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
                    source: TextSource {
                        atoms: Vec::new().into(),
                    },
                },
                UnmappedToken {
                    scalar_index: 2,
                    font_hash: FontProgramHash(vec![9]),
                    glyph_id: 11,
                    source: TextSource {
                        atoms: Vec::new().into(),
                    },
                },
            ],
        },
        matching: String::new(),
        matching_tokens: Vec::new(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: vec![4],
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: None,
        page_breaks: None,
    }
}

/// A block whose comparable evidence is a single unmapped glyph with no
/// canonical scalars at all.
fn unmapped_only_block(id: u64, hash: Vec<u8>, glyph_id: u16) -> BlockText {
    BlockText {
        block: BlockId(id),
        role: pdfdelta_core::layout::BlockRole::Body,
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
                source: TextSource {
                    atoms: Vec::new().into(),
                },
            }],
        },
        matching: String::new(),
        matching_tokens: Vec::new(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: if id > 100 { vec![0] } else { vec![4] },
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: None,
        page_breaks: None,
    }
}

#[test]
fn svg_render_creates_valid_overlay_with_provenance_and_geometry() -> Result<()> {
    let glyphs = vec![
        Glyph {
            id: GlyphId(1),
            text: DecodedText::Mapped("Hello".to_owned()),
            raw_code: vec![0x48, 0x65, 0x6c, 0x6c, 0x6f],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 72.0, y: 700.0 },
                max: Vec2 { x: 120.0, y: 712.0 },
            },
            baseline: Vec2 { x: 72.0, y: 702.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(5),
            font_size: 12.0,
            render_order: 1,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::PartiallyOutside,
            path_clip_status: GlyphPathClipStatus::PartiallyOutside,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 10,
                    generation: 0,
                },
                operator_index: 3,
            },
        },
        Glyph {
            id: GlyphId(2),
            text: DecodedText::Unmapped {
                font_hash: FontProgramHash(vec![0xab, 0xcd, 0xef, 0x01]),
                glyph_id: 42,
            },
            raw_code: vec![0x00, 0x2a],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 125.0, y: 700.0 },
                max: Vec2 { x: 135.0, y: 712.0 },
            },
            baseline: Vec2 { x: 125.0, y: 702.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(6),
            font_size: 12.0,
            render_order: 2,
            render_mode: TextRenderMode::Invisible,
            crop_status: GlyphCropStatus::Outside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 10,
                    generation: 0,
                },
                operator_index: 4,
            },
        },
    ];

    let document = Document::new(glyphs);
    let svg = render_glyph_overlay_svg(&document)?;

    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.ends_with("</svg>\n"));
    assert!(svg.contains("id=\"page-1\""));
    assert!(svg.contains("Hello"));
    assert!(svg.contains("data-glyph-id=\"1\""));
    assert!(svg.contains("data-cs-num=\"10\""));
    assert!(svg.contains("data-op-idx=\"3\""));
    assert!(svg.contains("glyph-unmapped"));
    assert!(svg.contains("glyph-invisible"));
    assert!(svg.contains("glyph-crop-partial"));
    assert!(svg.contains("glyph-crop-outside"));
    assert!(svg.contains("glyph-path-clip-partial"));
    assert!(svg.contains("data-crop-status=\"PartiallyOutside\""));
    assert!(svg.contains("data-path-clip-status=\"PartiallyOutside\""));
    assert!(svg.contains("U+002A:abcdef01"));
    Ok(())
}

#[test]
fn svg_render_handles_empty_document_and_multipage() -> Result<()> {
    let empty_doc = Document::new(Vec::new());
    let empty_svg = render_glyph_overlay_svg(&empty_doc)?;
    assert!(empty_svg.contains("id=\"page-1\""));
    assert!(empty_svg.contains("Page 1 (glyphs: 0)"));

    let multipage_glyphs = vec![
        Glyph {
            id: GlyphId(1),
            text: DecodedText::Mapped("Page 1 text".to_owned()),
            raw_code: vec![1],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 50.0, y: 500.0 },
                max: Vec2 { x: 100.0, y: 510.0 },
            },
            baseline: Vec2 { x: 50.0, y: 500.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: 1,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: 0,
            },
        },
        Glyph {
            id: GlyphId(2),
            text: DecodedText::Mapped("Page 2 rotated".to_owned()),
            raw_code: vec![2],
            page: PageId(1),
            bbox: Rect {
                min: Vec2 { x: 50.0, y: 500.0 },
                max: Vec2 { x: 60.0, y: 550.0 },
            },
            baseline: Vec2 { x: 50.0, y: 500.0 },
            direction: Vec2 { x: 0.0, y: 1.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: 1,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 2,
                    generation: 0,
                },
                operator_index: 0,
            },
        },
    ];

    let multi_doc = Document::new(multipage_glyphs);
    let multi_svg = render_glyph_overlay_svg(&multi_doc)?;
    assert!(multi_svg.contains("id=\"page-1\""));
    assert!(multi_svg.contains("id=\"page-2\""));
    assert!(multi_svg.contains("rotate(-90.00"));
    Ok(())
}

fn sample_svg_glyph(id: u64, min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Glyph {
    Glyph {
        id: GlyphId(id),
        text: DecodedText::Mapped("A".to_owned()),
        raw_code: vec![65],
        page: PageId(0),
        bbox: Rect {
            min: Vec2 { x: min_x, y: min_y },
            max: Vec2 { x: max_x, y: max_y },
        },
        baseline: Vec2 { x: min_x, y: min_y },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size: 10.0,
        render_order: 1,
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        path_clip_status: GlyphPathClipStatus::Unclipped,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: 0,
        },
    }
}

#[test]
fn svg_render_and_write_reject_non_finite_geometry() {
    let test_cases = vec![
        ("nan in bbox.min.x", {
            let mut g = sample_svg_glyph(1, 50.0, 500.0, 100.0, 510.0);
            g.bbox.min.x = f64::NAN;
            g
        }),
        ("infinity in bbox.max.y", {
            let mut g = sample_svg_glyph(2, 50.0, 500.0, 100.0, 510.0);
            g.bbox.max.y = f64::INFINITY;
            g
        }),
        ("neg_infinity in bbox.min.y", {
            let mut g = sample_svg_glyph(3, 50.0, 500.0, 100.0, 510.0);
            g.bbox.min.y = f64::NEG_INFINITY;
            g
        }),
        ("nan in baseline.x", {
            let mut g = sample_svg_glyph(4, 50.0, 500.0, 100.0, 510.0);
            g.baseline.x = f64::NAN;
            g
        }),
        ("infinity in direction.y", {
            let mut g = sample_svg_glyph(5, 50.0, 500.0, 100.0, 510.0);
            g.direction.y = f64::INFINITY;
            g
        }),
        ("nan in font_size", {
            let mut g = sample_svg_glyph(6, 50.0, 500.0, 100.0, 510.0);
            g.font_size = f64::NAN;
            g
        }),
    ];

    for (desc, glyph) in test_cases {
        let expected_glyph_id = format!("glyph {}", glyph.id.0);
        let doc = Document::new(vec![glyph]);

        // render_glyph_overlay_svg must return Error::Report with "non-finite" and GlyphId
        let render_res = render_glyph_overlay_svg(&doc);
        match render_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
                assert!(
                    msg.contains(&expected_glyph_id),
                    "[{desc}] error message '{msg}' should contain '{expected_glyph_id}'"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }

        // write_glyph_overlay_svg must fail before writing partial output
        let mut buffer = Vec::new();
        let write_res = write_glyph_overlay_svg(&doc, &mut buffer);
        match write_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
                assert!(
                    msg.contains(&expected_glyph_id),
                    "[{desc}] error message '{msg}' should contain '{expected_glyph_id}'"
                );
                assert!(
                    buffer.is_empty(),
                    "[{desc}] writer buffer must remain empty on validation failure"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }
    }
}

#[test]
fn svg_render_and_write_reject_negative_font_size() {
    let mut glyph = sample_svg_glyph(42, 50.0, 500.0, 100.0, 510.0);
    glyph.font_size = -12.0;
    let doc = Document::new(vec![glyph]);

    let render_res = render_glyph_overlay_svg(&doc);
    match render_res {
        Err(Error::Report(msg)) => {
            assert!(
                msg.contains("negative font size"),
                "error '{msg}' should mention negative font size"
            );
            assert!(
                msg.contains("glyph 42"),
                "error '{msg}' should contain glyph 42"
            );
        }
        other => panic!("expected Error::Report for negative font size, got {other:?}"),
    }

    let mut buffer = Vec::new();
    let write_res = write_glyph_overlay_svg(&doc, &mut buffer);
    match write_res {
        Err(Error::Report(msg)) => {
            assert!(
                msg.contains("negative font size"),
                "error '{msg}' should mention negative font size"
            );
            assert!(
                msg.contains("glyph 42"),
                "error '{msg}' should contain glyph 42"
            );
            assert!(
                buffer.is_empty(),
                "writer buffer must remain empty on negative font size failure"
            );
        }
        other => panic!("expected Error::Report for negative font size, got {other:?}"),
    }
}

#[test]
fn svg_render_and_write_reject_huge_finite_derived_overflow() {
    let test_cases = vec![
        (
            "huge bbox width overflow",
            sample_svg_glyph(10, -1e308, 0.0, 1e308, 10.0),
        ),
        ("huge font_size baseline endpoint overflow", {
            let mut g = sample_svg_glyph(11, 0.0, 0.0, 10.0, 10.0);
            g.baseline.x = 1e308;
            g.font_size = 1e308;
            g
        }),
    ];

    for (desc, glyph) in test_cases {
        let doc = Document::new(vec![glyph]);

        let render_res = render_glyph_overlay_svg(&doc);
        match render_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }

        let mut buffer = Vec::new();
        let write_res = write_glyph_overlay_svg(&doc, &mut buffer);
        match write_res {
            Err(Error::Report(msg)) => {
                assert!(
                    msg.contains("non-finite"),
                    "[{desc}] error message '{msg}' should mention non-finite"
                );
                assert!(
                    buffer.is_empty(),
                    "[{desc}] writer buffer must remain empty on derived overflow"
                );
            }
            other => panic!("[{desc}] expected Error::Report, got {other:?}"),
        }
    }
}

#[test]
fn svg_render_accepts_valid_edge_cases_like_zero_width_and_height() -> Result<()> {
    // Glyph with zero width and zero height
    let zero_dim_glyph = sample_svg_glyph(1, 50.0, 500.0, 50.0, 500.0);
    let doc = Document::new(vec![zero_dim_glyph]);

    let svg = render_glyph_overlay_svg(&doc)?;
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
        "SVG header must be present"
    );
    assert!(
        svg.contains("width=\"0.50\" height=\"0.50\""),
        "Zero-dimension glyph must be clamped to 0.50 display size"
    );
    assert!(svg.ends_with("</svg>\n"));
    Ok(())
}

#[test]
fn svg_render_and_write_handle_max_page_id_without_overflow() -> Result<()> {
    let mut glyph = sample_svg_glyph(1, 50.0, 500.0, 100.0, 510.0);
    glyph.page = PageId(u32::MAX);
    let doc = Document::new(vec![glyph]);

    let svg = render_glyph_overlay_svg(&doc)?;
    assert!(
        svg.contains("id=\"page-4294967296\""),
        "SVG must render PageId(u32::MAX) as 4294967296 without wrapping or panic"
    );
    assert!(
        svg.contains("Page 4294967296 (glyphs: 1)"),
        "SVG label must display 4294967296"
    );
    assert!(
        svg.contains("data-page=\"4294967296\""),
        "data-page must serialize 4294967296"
    );
    assert!(
        svg.contains("(Page 4294967296)"),
        "title text must display (Page 4294967296)"
    );

    let mut buffer = Vec::new();
    write_glyph_overlay_svg(&doc, &mut buffer)?;
    let write_svg = String::from_utf8(buffer).expect("valid utf-8");
    assert!(write_svg.contains("id=\"page-4294967296\""));
    Ok(())
}

#[test]
fn text_and_json_report_renders_promoted_move_with_formatting_normalization_change() -> Result<()> {
    let old_blocks = vec![block_with_pages(
        3,
        "Moved paragraph text line one line two",
        &[1],
    )];
    let new_blocks = vec![block_with_pages(
        8,
        "Moved paragraph text line one line two",
        &[4],
    )];
    let mut comparison = empty_comparison();
    comparison.changes.push(Change {
        kind: ChangeKind::Move,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(3, "Moved paragraph text line one line two")),
            new_span: Some(full_span(8, "Moved paragraph text line one line two")),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });
    comparison.formatting_changes.push(FormattingChange {
        old_span: full_span(3, "Moved paragraph text line one line two"),
        new_span: full_span(8, "Moved paragraph text line one line two"),
        confidence: Confidence::High,
        reasons: vec![FormattingReason::Normalization],
    });

    // 1. Summary validation
    let summary = summarize(&comparison, &ExtractionStatus::complete())?;
    assert_eq!(summary.content_changes, 1);
    assert_eq!(summary.formatting_only_changes, 1);
    assert_eq!(summary.uncertain_changes, 0);
    assert_eq!(summary.unresolved_regions, 0);

    // 2. Text report rendering
    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;
    assert!(
        report.contains(
            "content changes: 1 · proven changed regions: 0 · formatting-only: 1 · uncertain: 0 · unresolved regions: 0"
        ),
        "{report}"
    );
    assert!(report.contains("~ moved from page 2 to page 5"), "{report}");
    assert!(
        report.contains("@@ pages 2,5 · old block 3 -> new block 8 · confidence: high @@"),
        "{report}"
    );

    // 3. JSON report rendering
    let mut json_bytes = Vec::new();
    write_json(
        &mut json_bytes,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json_text = String::from_utf8(json_bytes).expect("JSON should be valid UTF-8");
    assert!(json_text.contains("\"content_changes\": 1"));
    assert!(json_text.contains("\"formatting_only_changes\": 1"));
    assert!(json_text.contains("\"kind\": \"move\""));
    assert!(json_text.contains("\"reasons\": [\n        \"normalization\"\n      ]"));

    // 4. Exit status
    let status = exit_status(&comparison, &ExtractionStatus::complete(), false)?;
    assert_eq!(status, ExitStatus::ContentChanges);
    assert_eq!(status.code(), 1);

    Ok(())
}

#[test]
fn text_report_handles_max_page_id_without_overflow() -> Result<()> {
    let extraction = ExtractionStatus {
        old_complete: false,
        new_complete: true,
        issues: vec![ExtractionIssueRecord {
            side: DocumentSide::Old,
            kind: ExtractionIssueKind::Unsupported,
            scope: ExtractionScope::Page(PageId(u32::MAX)),
            description: "unsupported feature on huge page".to_owned(),
        }],
    };

    let old_blocks = vec![block_with_pages(1, "Old block on max page", &[u32::MAX])];
    let new_blocks = vec![block_with_pages(101, "New block on max page", &[u32::MAX])];
    let mut comparison = empty_comparison();
    let old_tokens = old_blocks[0].matching_tokens.len();
    let new_tokens = new_blocks[0].matching_tokens.len();
    comparison.old_coverage.total_tokens = old_tokens;
    comparison.old_coverage.resolved_tokens = old_tokens;
    comparison.old_coverage.ratio = None;
    comparison.new_coverage.total_tokens = new_tokens;
    comparison.new_coverage.resolved_tokens = new_tokens;
    comparison.changes.push(Change {
        kind: ChangeKind::Replacement,
        occurrences: vec![ChangeOccurrence {
            old_span: Some(full_span(1, "Old block on max page")),
            new_span: Some(full_span(101, "New block on max page")),
        }],
        confidence: Confidence::High,
        tags: Vec::new(),
    });

    let report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &extraction,
        &plain_options(),
    )?;

    assert!(
        report.contains("scope=page, page=4294967296"),
        "Extraction scope must display page=4294967296 without overflow: {report}"
    );
    assert!(
        report.contains("@@ page 4294967296 · old block 1 -> new block 101"),
        "Hunk header must display page 4294967296 without overflow: {report}"
    );
    Ok(())
}

#[test]
fn svg_render_escapes_xml_10_forbidden_controls_and_preserves_whitespace() -> Result<()> {
    let forbidden_and_special_text = "Hello\0World\x01\x08\x0B\x0C\x0E\x1F\u{FFFE}\u{FFFF} & <tag> \"quotes\" 'apos' \t \r \n \u{D7FF} \u{E000} \u{FFFD} \u{1F600} \u{1FFFE} \x7F \u{0080} \u{009F} <U+0000> End";
    let glyph = Glyph {
        id: GlyphId(1),
        page: PageId(0),
        text: DecodedText::Mapped(forbidden_and_special_text.to_owned()),
        raw_code: vec![0x41],
        font_id: FontId(1),
        font_size: 12.0,
        bbox: Rect {
            min: Vec2 { x: 50.0, y: 100.0 },
            max: Vec2 { x: 200.0, y: 120.0 },
        },
        baseline: Vec2 { x: 50.0, y: 100.0 },
        direction: Vec2 { x: 1.0, y: 0.0 },
        render_order: 1,
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        path_clip_status: GlyphPathClipStatus::Unclipped,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: 0,
        },
    };

    let doc = Document::new(vec![glyph]);
    let svg = render_glyph_overlay_svg(&doc)?;

    // 1. Forbidden C0 controls and non-characters are converted to &lt;U+XXXX&gt;
    assert!(
        svg.contains("&lt;U+0000&gt;"),
        "NULL must be escaped to &lt;U+0000&gt;"
    );
    assert!(
        svg.contains("&lt;U+0001&gt;"),
        "SOH must be escaped to &lt;U+0001&gt;"
    );
    assert!(
        svg.contains("&lt;U+0008&gt;"),
        "BS must be escaped to &lt;U+0008&gt;"
    );
    assert!(
        svg.contains("&lt;U+000B&gt;"),
        "VT must be escaped to &lt;U+000B&gt;"
    );
    assert!(
        svg.contains("&lt;U+000C&gt;"),
        "FF must be escaped to &lt;U+000C&gt;"
    );
    assert!(
        svg.contains("&lt;U+000E&gt;"),
        "SO must be escaped to &lt;U+000E&gt;"
    );
    assert!(
        svg.contains("&lt;U+001F&gt;"),
        "US must be escaped to &lt;U+001F&gt;"
    );
    assert!(
        svg.contains("&lt;U+FFFE&gt;"),
        "U+FFFE non-character must be escaped to &lt;U+FFFE&gt;"
    );
    assert!(
        svg.contains("&lt;U+FFFF&gt;"),
        "U+FFFF non-character must be escaped to &lt;U+FFFF&gt;"
    );

    // 2. Standard XML special characters are escaped
    assert!(svg.contains("&amp;"), "ampersand must be escaped to &amp;");
    assert!(
        svg.contains("&lt;tag&gt;"),
        "tag brackets must be escaped to &lt;tag&gt;"
    );
    assert!(
        svg.contains("&quot;quotes&quot;"),
        "double quotes must be escaped to &quot;"
    );
    assert!(
        svg.contains("&apos;apos&apos;"),
        "single quotes must be escaped to &apos;"
    );

    // 3. Allowed whitespace controls and XML 1.0 boundary scalar values are preserved as-is
    assert!(
        !svg.contains("&lt;U+0009&gt;"),
        "TAB must not be replaced by a marker"
    );
    assert!(
        !svg.contains("&lt;U+000A&gt;"),
        "LF must not be replaced by a marker"
    );
    assert!(
        !svg.contains("&lt;U+000D&gt;"),
        "CR must not be replaced by a marker"
    );
    assert!(
        svg.contains('\u{D7FF}'),
        "U+D7FF boundary must be preserved without conversion to marker"
    );
    assert!(
        svg.contains('\u{E000}'),
        "U+E000 boundary must be preserved without conversion to marker"
    );
    assert!(
        svg.contains('\u{FFFD}'),
        "U+FFFD replacement char must be preserved without conversion to marker"
    );
    assert!(
        svg.contains('\u{1F600}'),
        "Supplementary plane emoji U+1F600 must be preserved"
    );
    assert!(
        svg.contains('\u{1FFFE}'),
        "Supplementary plane U+1FFFE must be preserved"
    );
    assert!(
        svg.contains('\x7F'),
        "DEL 0x7F must be preserved according to XML 1.0 Char definition"
    );
    assert!(
        svg.contains('\u{0080}'),
        "C1 control U+0080 must be preserved according to XML 1.0 Char definition"
    );
    assert!(
        svg.contains('\u{009F}'),
        "C1 control U+009F must be preserved according to XML 1.0 Char definition"
    );

    // 4. Deterministic output
    let svg2 = render_glyph_overlay_svg(&doc)?;
    assert_eq!(svg, svg2, "SVG serialization must be 100% deterministic");
    Ok(())
}

#[test]
fn reports_render_and_serialize_calibrated_confidence_levels() -> Result<()> {
    let old_blocks = vec![
        block_with_pages(1, "Exact anchor block text", &[1]),
        block_with_pages(2, "Strong fuzzy block text", &[1]),
        block_with_pages(3, "Weak fuzzy block text", &[1]),
    ];
    let new_blocks = vec![
        block_with_pages(101, "Exact anchor block text", &[1]),
        block_with_pages(102, "Strong fuzzy edited text", &[1]),
        block_with_pages(103, "Weak fuzzy edited text", &[1]),
    ];

    let old_tokens: usize = old_blocks.iter().map(|b| b.matching_tokens.len()).sum();
    let new_tokens: usize = new_blocks.iter().map(|b| b.matching_tokens.len()).sum();

    let comparison = Comparison {
        changes: vec![
            Change {
                kind: ChangeKind::Replacement,
                occurrences: vec![ChangeOccurrence {
                    old_span: Some(full_span(2, "Strong fuzzy block text")),
                    new_span: Some(full_span(102, "Strong fuzzy edited text")),
                }],
                confidence: Confidence::Medium,
                tags: Vec::new(),
            },
            Change {
                kind: ChangeKind::Replacement,
                occurrences: vec![ChangeOccurrence {
                    old_span: Some(full_span(3, "Weak fuzzy block text")),
                    new_span: Some(full_span(103, "Weak fuzzy edited text")),
                }],
                confidence: Confidence::Low,
                tags: Vec::new(),
            },
        ],
        proven_changed_regions: Vec::new(),
        formatting_changes: vec![FormattingChange {
            old_span: full_span(1, "Exact anchor block text"),
            new_span: full_span(101, "Exact anchor block text"),
            confidence: Confidence::High,
            reasons: vec![FormattingReason::Normalization],
        }],
        unresolved_regions: Vec::new(),
        old_coverage: Coverage {
            resolved_tokens: old_tokens,
            total_tokens: old_tokens,
            ratio: Some(1.0),
        },
        new_coverage: Coverage {
            resolved_tokens: new_tokens,
            total_tokens: new_tokens,
            ratio: Some(1.0),
        },
    };

    let summary = summarize(&comparison, &ExtractionStatus::complete())?;
    assert_eq!(summary.content_changes, 2);
    assert_eq!(summary.formatting_only_changes, 1);
    assert_eq!(
        summary.uncertain_changes, 1,
        "Low confidence change must count as uncertain"
    );

    let text_report = render_text(
        &old_blocks,
        &new_blocks,
        &comparison,
        &ExtractionStatus::complete(),
        &plain_options(),
    )?;

    assert!(text_report.contains("uncertain: 1"), "{text_report}");
    assert!(
        text_report.contains("@@ page 2 · old block 2 -> new block 102 · confidence: medium @@"),
        "{text_report}"
    );
    assert!(
        text_report.contains("@@ page 2 · old block 3 -> new block 103 · confidence: low @@"),
        "{text_report}"
    );

    let mut json_bytes = Vec::new();
    write_json(
        &mut json_bytes,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&json_bytes).expect("report should be valid JSON");

    assert_eq!(json["summary"]["uncertain_changes"], 1);
    assert_eq!(json["changes"][0]["confidence"], "medium");
    assert_eq!(json["changes"][1]["confidence"], "low");
    assert_eq!(json["formatting_only_changes"][0]["confidence"], "high");

    Ok(())
}

#[test]
fn formatting_changes_with_low_confidence_do_not_increment_uncertain_changes() -> Result<()> {
    let old_blocks = vec![block_with_pages(1, "Combined text line one line two", &[1])];
    let new_blocks = vec![
        block_with_pages(101, "Combined text line one", &[1]),
        block_with_pages(102, "line two", &[1]),
    ];

    let old_tokens: usize = old_blocks.iter().map(|b| b.matching_tokens.len()).sum();
    let new_tokens: usize = new_blocks.iter().map(|b| b.matching_tokens.len()).sum();

    // Low confidence formatting-only change (e.g. split/merge with normalization issue)
    let comparison = Comparison {
        changes: Vec::new(),
        proven_changed_regions: Vec::new(),
        formatting_changes: vec![FormattingChange {
            old_span: full_span(1, "Combined text line one line two"),
            new_span: full_span(101, "Combined text line one"),
            confidence: Confidence::Low,
            reasons: vec![FormattingReason::BlockStructure],
        }],
        unresolved_regions: Vec::new(),
        old_coverage: Coverage {
            resolved_tokens: old_tokens,
            total_tokens: old_tokens,
            ratio: Some(1.0),
        },
        new_coverage: Coverage {
            resolved_tokens: new_tokens,
            total_tokens: new_tokens,
            ratio: Some(1.0),
        },
    };

    let summary = summarize(&comparison, &ExtractionStatus::complete())?;
    assert_eq!(summary.content_changes, 0);
    assert_eq!(summary.formatting_only_changes, 1);
    assert_eq!(
        summary.uncertain_changes, 0,
        "Low confidence formatting changes belong to formatting_only_changes and must not increment uncertain_changes"
    );

    let mut json_bytes = Vec::new();
    write_json(
        &mut json_bytes,
        &old_blocks,
        &new_blocks,
        &[],
        &[],
        &comparison,
        &ExtractionStatus::complete(),
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&json_bytes).expect("report should be valid JSON");

    assert_eq!(json["summary"]["content_changes"], 0);
    assert_eq!(json["summary"]["formatting_only_changes"], 1);
    assert_eq!(json["summary"]["uncertain_changes"], 0);
    assert_eq!(json["formatting_only_changes"][0]["confidence"], "low");

    Ok(())
}
