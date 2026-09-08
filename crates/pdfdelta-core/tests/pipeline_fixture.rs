use pdfdelta_core::{
    Error, Result,
    alignment::AlignmentOptions,
    diff::{
        ChangeKind, ChangeTag, Comparison, Confidence, DiffOptions, FormattingReason, TokenRange,
    },
    layout::{
        BlockId, BlockOptions, LineOptions, LineTextDirection, reconstruct_blocks,
        reconstruct_lines,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
    pipeline::{
        PipelineDiagnostics, PipelineErrorKind, PipelineOptions, PipelinePhase,
        PipelinePhaseStatus, compare_extraction_outcomes,
        compare_extraction_outcomes_with_alignment_diagnostics,
        compare_extraction_outcomes_with_atomic_edits,
        compare_extraction_outcomes_with_diagnostics, compare_glyph_documents,
    },
    report::{DifferenceStatus, DocumentSide, summarize},
    source::{ExtractionIssue, ExtractionIssueKind, ExtractionOutcome, ExtractionScope},
};

#[test]
fn ignores_line_wrap_only_changes() -> Result<()> {
    let old = document(&[line("A simple release note remains stable", 0, 100.0)]);
    let new = document(&[
        line("A simple release note", 0, 100.0),
        line("remains stable", 0, 88.0),
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert_eq!(
        comparison.formatting_changes[0].reasons,
        [FormattingReason::Normalization, FormattingReason::LineBreak]
    );
    Ok(())
}

#[test]
fn reports_page_break_only_as_formatting() -> Result<()> {
    let text = [
        "First line keeps a steady cadence",
        "Second line keeps a steady cadence",
        "Third line keeps a steady cadence",
        "Fourth line keeps a steady cadence",
    ];
    let old = document(&[
        line(text[0], 0, 100.0),
        line(text[1], 0, 88.0),
        line(text[2], 0, 76.0),
        line(text[3], 0, 64.0),
    ]);
    let new = document(&[
        line(text[0], 0, 100.0),
        line(text[1], 0, 88.0),
        line(text[2], 1, 100.0),
        line(text[3], 1, 88.0),
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert_eq!(
        comparison.formatting_changes[0].reasons,
        [FormattingReason::PageBreak]
    );
    Ok(())
}

#[test]
fn reports_font_size_only_as_formatting() -> Result<()> {
    let old = document(&[line("Stable release note", 0, 100.0)]);
    let mut new_glyphs = old.items().to_vec();
    for glyph in &mut new_glyphs {
        glyph.font_size = 14.0;
    }
    let new = Document::new(new_glyphs);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert_eq!(
        comparison.formatting_changes[0].reasons,
        [FormattingReason::FontSize]
    );
    Ok(())
}

#[test]
fn reports_uniform_position_translation_as_formatting() -> Result<()> {
    let old = document(&[line("Stable release note", 0, 100.0)]);
    let mut new_glyphs = old.items().to_vec();
    for glyph in &mut new_glyphs {
        glyph.baseline.x += 66.0;
        glyph.bbox.min.x += 66.0;
        glyph.bbox.max.x += 66.0;
    }
    let new = Document::new(new_glyphs);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    assert_eq!(comparison.formatting_changes.len(), 1);
    assert_eq!(
        comparison.formatting_changes[0].reasons,
        [FormattingReason::Position]
    );
    Ok(())
}

#[test]
fn reports_one_generic_numeric_replacement() -> Result<()> {
    let old = paragraphs(&[
        "Opening paragraph establishes context",
        "Release 10 remains available",
        "Closing paragraph confirms context",
    ]);
    let new = paragraphs(&[
        "Opening paragraph establishes context",
        "Release 20 remains available",
        "Closing paragraph confirms context",
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change(&comparison, ChangeKind::Replacement);
    Ok(())
}

#[test]
fn tags_character_width_replacement_end_to_end() -> Result<()> {
    let old = paragraphs(&[
        "Opening paragraph establishes context",
        "Version １２ remains stable",
        "Closing paragraph confirms context",
    ]);
    let new = paragraphs(&[
        "Opening paragraph establishes context",
        "Version 12 remains stable",
        "Closing paragraph confirms context",
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change(&comparison, ChangeKind::Replacement);
    assert_eq!(comparison.changes[0].tags, [ChangeTag::CharacterWidth]);
    Ok(())
}

#[test]
fn replacement_outcome_retains_each_glyph_evidence_once() -> Result<()> {
    let old_document = paragraphs(&["Release 10 remains available"]);
    let new_document = paragraphs(&["Release 20 remains available"]);
    let old_count = old_document.items().len();
    let new_count = new_document.items().len();

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old_document),
        ExtractionOutcome::complete(new_document),
        PipelineOptions::default(),
    )?;

    assert_eq!(outcome.old_glyph_evidence.len(), old_count);
    assert_eq!(outcome.new_glyph_evidence.len(), new_count);
    assert_unique_glyph_evidence(&outcome.old_glyph_evidence);
    assert_unique_glyph_evidence(&outcome.new_glyph_evidence);
    Ok(())
}

#[test]
fn compares_mixed_axis_aligned_orientations() -> Result<()> {
    let document = document(&[
        line("Body text remains stable", 0, 100.0),
        vertical_line("Print or type.", 0, 20.0, 20.0),
    ]);

    let comparison = compare_glyph_documents(&document, &document, PipelineOptions::default())?;

    assert_unknown_reading_order(&comparison);
    Ok(())
}

#[test]
fn self_compares_tilted_text() -> Result<()> {
    let mut tilted = line("Tilted text remains stable", 0, 80.0);
    tilted.direction = Vec2 {
        x: 0.999_657_376_647_797_5,
        y: 0.026_174_974_950_197_414,
    };
    let document = document(&[line("Body text remains stable", 0, 100.0), tilted]);

    let comparison = compare_glyph_documents(&document, &document, PipelineOptions::default())?;

    assert_unknown_reading_order(&comparison);
    Ok(())
}

#[test]
fn positive_x_hebrew_text_has_unknown_reading_order() -> Result<()> {
    let document = document(&[line("שלום", 0, 100.0)]);
    let lines = reconstruct_lines(&document, LineOptions::default())?;

    assert_eq!(lines.len(), 1);
    assert!(lines[0].direction.x > 0.0);
    assert_eq!(lines[0].text_direction, LineTextDirection::RightToLeft);

    let comparison = compare_glyph_documents(&document, &document, PipelineOptions::default())?;
    assert_unknown_reading_order(&comparison);
    Ok(())
}

#[test]
fn reports_content_change_in_rotated_label() -> Result<()> {
    let old = document(&[vertical_line("Print or type.", 0, 20.0, 20.0)]);
    let new = document(&[vertical_line("Print or tyqe.", 0, 20.0, 20.0)]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_unknown_reading_order(&comparison);
    Ok(())
}

#[test]
fn moved_rotated_label_remains_content_equivalent() -> Result<()> {
    let old = document(&[vertical_line("Print or type.", 0, 20.0, 20.0)]);
    let new = document(&[vertical_line("Print or type.", 0, 80.0, 120.0)]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_unknown_reading_order(&comparison);
    Ok(())
}

#[test]
fn ignores_known_two_column_reflow() -> Result<()> {
    let old = document(&[
        line("First paragraph remains stable", 0, 300.0),
        line("Second paragraph remains stable", 0, 270.0),
        line("Third paragraph remains stable", 0, 240.0),
        line("Fourth paragraph remains stable", 0, 210.0),
    ]);
    let new = document(&[
        line_at("First paragraph remains stable", 0, 0.0, 300.0),
        line_at("Second paragraph remains stable", 0, 0.0, 270.0),
        line_at("Third paragraph remains stable", 0, 300.0, 300.0),
        line_at("Fourth paragraph remains stable", 0, 300.0, 270.0),
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    Ok(())
}

#[test]
fn reports_replacement_inside_known_two_column_order() -> Result<()> {
    let old = document(&[
        line_at("Opening paragraph remains stable", 0, 0.0, 300.0),
        line_at("Context paragraph remains stable", 0, 0.0, 270.0),
        line_at("Release 10 remains available", 0, 300.0, 300.0),
        line_at("Closing paragraph remains stable", 0, 300.0, 270.0),
    ]);
    let new = document(&[
        line_at("Opening paragraph remains stable", 0, 0.0, 300.0),
        line_at("Context paragraph remains stable", 0, 0.0, 270.0),
        line_at("Release 20 remains available", 0, 300.0, 300.0),
        line_at("Closing paragraph remains stable", 0, 300.0, 270.0),
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change(&comparison, ChangeKind::Replacement);
    Ok(())
}

#[test]
fn rotated_margin_region_does_not_hide_a_same_page_replacement() -> Result<()> {
    let old = document(&[
        line_at("Opening anchor remains stable", 0, 0.0, 300.0),
        line_at("Release 10 remains available", 0, 0.0, 280.0),
        line_at("Closing anchor remains stable", 0, 0.0, 260.0),
        vertical_line("Side A", 0, 400.0, 260.0),
        vertical_line("Side B", 0, 420.0, 260.0),
    ]);
    let new = document(&[
        line_at("Opening anchor remains stable", 0, 0.0, 300.0),
        line_at("Release 20 remains available", 0, 0.0, 280.0),
        line_at("Closing anchor remains stable", 0, 0.0, 260.0),
        vertical_line("Side A", 0, 400.0, 260.0),
        vertical_line("Side B", 0, 420.0, 260.0),
    ]);

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )?;
    let comparison = &outcome.comparison;

    assert_single_change_with_unresolved(comparison, ChangeKind::Replacement);
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert_eq!(
        comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    );
    let unresolved_blocks = &comparison.unresolved_regions[0]
        .old_span
        .as_ref()
        .expect("old-side label evidence should be retained")
        .blocks;
    let unresolved_text = outcome
        .old_blocks
        .iter()
        .filter(|block| unresolved_blocks.contains(&block.block))
        .map(|block| block.canonical.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(unresolved_text.contains("Side A"));
    assert!(!unresolved_text.contains("Release 10"));
    Ok(())
}

#[test]
fn complete_unknown_order_recovers_unique_modified_sentences() -> Result<()> {
    let old = document(&[
        line_at("Left context remains open", 0, 0.0, 300.0),
        line_at("Right context remains open", 0, 300.0, 300.0),
        line_at("Left remainder stays open", 0, 0.0, 288.0),
        line_at("Right remainder stays open", 0, 300.0, 288.0),
        line_at("The legacy sentence is removed.", 0, 0.0, 276.0),
        line_at("Right tail remains open", 0, 300.0, 276.0),
    ]);
    let new = document(&[
        line_at("Left context remains open", 0, 0.0, 300.0),
        line_at("Right context remains open", 0, 300.0, 300.0),
        line_at("Left remainder stays open", 0, 0.0, 288.0),
        line_at("Right remainder stays open", 0, 300.0, 288.0),
        line_at("A fresh sentence is inserted.", 0, 0.0, 276.0),
        line_at("Right tail remains open", 0, 300.0, 276.0),
    ]);
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            anchor_min_tokens: 4,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };

    let comparison = compare_glyph_documents(&old, &new, options)?;

    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert_eq!(comparison.changes[0].kind, ChangeKind::Replacement);
    let established = &comparison.changes[0].occurrences[0];
    assert_eq!(
        established
            .old_span
            .as_ref()
            .map(|span| span.comparable_range),
        Some(TokenRange { start: 52, end: 62 })
    );
    assert_eq!(
        established
            .new_span
            .as_ref()
            .map(|span| span.comparable_range),
        Some(TokenRange { start: 52, end: 59 })
    );
    assert_eq!(comparison.change_candidates.len(), 2, "{comparison:#?}");
    assert!(
        comparison
            .change_candidates
            .iter()
            .all(|candidate| candidate.change.kind == ChangeKind::Replacement)
    );
    assert!(
        comparison
            .change_candidates
            .iter()
            .all(|candidate| candidate.change.confidence == Confidence::Medium)
    );
    let ranges = comparison
        .change_candidates
        .iter()
        .flat_map(|candidate| candidate.change.occurrences.iter())
        .map(|occurrence| {
            (
                occurrence
                    .old_span
                    .as_ref()
                    .map(|span| span.comparable_range),
                occurrence
                    .new_span
                    .as_ref()
                    .map(|span| span.comparable_range),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ranges,
        [
            (
                Some(TokenRange { start: 75, end: 75 }),
                Some(TokenRange { start: 72, end: 76 }),
            ),
            (
                Some(TokenRange { start: 76, end: 82 }),
                Some(TokenRange { start: 77, end: 80 }),
            ),
        ]
    );
    assert_eq!(comparison.unresolved_regions.len(), 4);
    assert!(
        comparison
            .unresolved_regions
            .iter()
            .all(|region| region.evidence
                == [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown])
    );
    assert_eq!(
        comparison
            .unresolved_regions
            .iter()
            .filter_map(|region| region.old_span.as_ref())
            .map(|span| span.blocks.clone())
            .collect::<Vec<_>>(),
        vec![vec![BlockId(0)], vec![BlockId(0)]]
    );
    assert_eq!(
        comparison
            .unresolved_regions
            .iter()
            .filter_map(|region| region.new_span.as_ref())
            .map(|span| span.blocks.clone())
            .collect::<Vec<_>>(),
        vec![vec![BlockId(0)], vec![BlockId(0)]]
    );
    assert!(
        comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    Ok(())
}

#[test]
fn localized_extraction_gap_disables_sentence_recovery() -> Result<()> {
    let old_document = document(&[
        line_at("Left context remains open", 0, 0.0, 300.0),
        line_at("Right context remains open", 0, 300.0, 300.0),
        line_at("Left remainder stays open", 0, 0.0, 288.0),
        line_at("Right remainder stays open", 0, 300.0, 288.0),
        line_at("The legacy sentence is removed.", 0, 0.0, 276.0),
        line_at("Right tail remains open", 0, 300.0, 276.0),
    ]);
    let retained_before = old_document.items().len() / 2;
    let old = ExtractionOutcome::new(
        old_document,
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::GlyphGap { retained_before },
            "content stream is incomplete",
        )?],
    )?;
    let new = ExtractionOutcome::complete(document(&[
        line_at("Left context remains open", 0, 0.0, 300.0),
        line_at("Right context remains open", 0, 300.0, 300.0),
        line_at("Left remainder stays open", 0, 0.0, 288.0),
        line_at("Right remainder stays open", 0, 300.0, 288.0),
        line_at("A fresh sentence is inserted.", 0, 0.0, 276.0),
        line_at("Right tail remains open", 0, 300.0, 276.0),
    ]));
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            anchor_min_tokens: 4,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };

    let outcome = compare_extraction_outcomes(old, new, options)?;

    assert!(outcome.comparison.changes.is_empty());
    assert!(!outcome.comparison.unresolved_regions.is_empty());
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    Ok(())
}

#[test]
fn supported_sentence_recovers_beside_mixed_orientation_text() -> Result<()> {
    let old = document(&[
        line("The legacy sentence is removed.", 0, 300.0),
        vertical_line("Unresolved side label", 0, 400.0, 100.0),
    ]);
    let new = document(&[
        line("A fresh sentence is inserted.", 0, 300.0),
        vertical_line("Unresolved side label", 0, 400.0, 100.0),
    ]);
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            anchor_min_tokens: 4,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        options,
    )?;
    let mut report = Vec::new();
    pdfdelta_core::report::write_json(
        &mut report,
        &outcome.old_blocks,
        &outcome.new_blocks,
        &outcome.old_glyph_evidence,
        &outcome.new_glyph_evidence,
        &outcome.comparison,
        &outcome.extraction,
    )?;
    let report: serde_json::Value =
        serde_json::from_slice(&report).expect("report should be valid JSON");
    assert_eq!(report["changes"], serde_json::json!([]));
    let candidates = report["change_candidates"]
        .as_array()
        .expect("change candidates should be an array");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["kind"], "deletion");
    assert_eq!(candidates[0]["confidence"], "high");
    assert_eq!(
        candidates[0]["occurrences"][0]["old_span"]["text"],
        "The legacy sentence is removed."
    );
    assert_eq!(
        candidates[0]["occurrences"][0]["old_span"]["comparable_range"],
        serde_json::json!({"start": 0, "end": 31})
    );
    assert_eq!(
        candidates[0]["occurrences"][0]["new_span"],
        serde_json::Value::Null
    );
    assert_eq!(candidates[1]["kind"], "insertion");
    assert_eq!(candidates[1]["confidence"], "high");
    assert_eq!(
        candidates[1]["occurrences"][0]["old_span"],
        serde_json::Value::Null
    );
    assert_eq!(
        candidates[1]["occurrences"][0]["new_span"]["text"],
        "A fresh sentence is inserted."
    );
    assert_eq!(
        candidates[1]["occurrences"][0]["new_span"]["comparable_range"],
        serde_json::json!({"start": 0, "end": 29})
    );
    assert_eq!(
        report["unresolved_regions"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        report["unresolved_regions"][0]["old_span"]["blocks"],
        serde_json::json!([0, 1])
    );
    assert_eq!(
        report["unresolved_regions"][0]["new_span"]["blocks"],
        serde_json::json!([0, 1])
    );
    assert_eq!(
        report["unresolved_regions"][0]["evidence"],
        serde_json::json!(["reading_order_unknown"])
    );
    assert_eq!(report["summary"]["comparison_complete"], false);
    Ok(())
}

#[test]
fn inferred_order_local_recovery_preserves_source_evidence() -> Result<()> {
    let old = document(&[
        line_at("Left alpha context remains open", 0, 0.0, 300.0),
        line_at("Right alpha context remains open", 0, 400.0, 300.0),
        line_at("Left alpha remainder stays open", 0, 0.0, 288.0),
        line_at("Right alpha remainder stays open", 0, 400.0, 288.0),
        line("A distant appendix explains orbital mechanics", 1, 100.0),
        line("and archived calculations use cobalt notation.", 1, 88.0),
        line(
            "Released widgets retain a robust catalog code of 10",
            1,
            600.0,
        ),
        line(
            "and operators apply durable labels before shipment.",
            1,
            588.0,
        ),
    ]);
    let new = document(&[
        line_at("Left beta context remains open", 0, 0.0, 300.0),
        line_at("Right beta context remains open", 0, 400.0, 300.0),
        line_at("Left beta remainder stays open", 0, 0.0, 288.0),
        line_at("Right beta remainder stays open", 0, 400.0, 288.0),
        line("A distant appendix explains orbital mechanics", 1, 100.0),
        line("and archived calculations use cobalt notation.", 1, 88.0),
        line(
            "Released widgets retain a robust catalog code of 20",
            1,
            600.0,
        ),
        line(
            "and operators apply durable labels before shipment.",
            1,
            588.0,
        ),
    ]);
    let old_lines = reconstruct_lines(&old, LineOptions::default())?
        .into_iter()
        .filter(|line| line.page == PageId(1))
        .collect::<Vec<_>>();
    let graph = pdfdelta_core::layout::partition_regions(
        PageId(1),
        &old_lines,
        pdfdelta_core::layout::RegionOptions::default(),
    )?;
    assert!(matches!(
        graph.reading_order,
        pdfdelta_core::layout::ReadingOrder::Inferred(_)
    ));
    let expected_glyphs = [&old, &new].map(|document| {
        document.items().iter().find(|glyph| {
            matches!(&glyph.text, DecodedText::Mapped(text) if text == "1" || text == "2")
        }).expect("unique changed digit").id
    });
    let mut diagnostics = PipelineDiagnostics::new();
    let traced = compare_extraction_outcomes_with_atomic_edits(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
        &mut diagnostics,
    )?;
    assert!(!traced.recovered_atomic_diffs.is_empty());
    let inferred_blocks = traced
        .outcome
        .old_blocks
        .iter()
        .filter(|block| block.pages.contains(&1))
        .map(|block| block.block)
        .collect::<std::collections::HashSet<_>>();
    assert!(traced.recovered_atomic_diffs.iter().any(|recovered| {
        recovered
            .old_context
            .blocks
            .iter()
            .any(|block| inferred_blocks.contains(block))
    }));
    let comparison = &traced.outcome.comparison;
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    let recovered = &comparison.changes[0];
    assert_eq!(recovered.kind, ChangeKind::Replacement);
    assert_eq!(recovered.confidence, Confidence::High);
    assert_eq!(recovered.occurrences.len(), 1);
    let occurrence = &recovered.occurrences[0];
    for (blocks, glyphs, span, expected) in [
        (
            &traced.outcome.old_blocks,
            &traced.outcome.old_glyph_evidence,
            occurrence.old_span.as_ref(),
            expected_glyphs[0],
        ),
        (
            &traced.outcome.new_blocks,
            &traced.outcome.new_glyph_evidence,
            occurrence.new_span.as_ref(),
            expected_glyphs[1],
        ),
    ] {
        let evidence = pdfdelta_core::report::project_span_sources(
            blocks,
            glyphs,
            span.expect("numeric replacement span"),
        )?;
        assert!(matches!(
            evidence.as_slice(),
            [pdfdelta_core::report::SpanSourceEvidence::Glyph { glyph_id, .. }]
                if *glyph_id == expected
        ));
    }
    assert!(
        comparison
            .assessment
            .as_ref()
            .expect("engine assessment")
            .relations
            .iter()
            .any(|relation| relation
                .reasons
                .contains(&pdfdelta_core::diff::AssessmentReason::InferredReadingOrder))
    );
    assert!(
        comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    Ok(())
}

#[test]
fn single_leaf_inferred_order_allows_an_independently_anchored_change() -> Result<()> {
    let old = document(&[
        line("A stable footer remains here.", 0, 100.0),
        line("A stable heading opens the catalog.", 0, 700.0),
        line("Released widgets retain catalog code 10", 0, 600.0),
        line("and operators apply labels before shipment.", 0, 588.0),
    ]);
    let new = document(&[
        line("A stable footer remains here.", 0, 100.0),
        line("A stable heading opens the catalog.", 0, 700.0),
        line("Released widgets retain catalog code 20", 0, 600.0),
        line("and operators apply labels before shipment.", 0, 588.0),
    ]);
    let lines = reconstruct_lines(&old, LineOptions::default())?;
    let graph = pdfdelta_core::layout::partition_regions(
        PageId(0),
        &lines,
        pdfdelta_core::layout::RegionOptions::default(),
    )?;
    assert_eq!(graph.regions.len(), 1);
    assert!(matches!(
        graph.reading_order,
        pdfdelta_core::layout::ReadingOrder::Inferred(_)
    ));
    let mut diagnostics = PipelineDiagnostics::new();
    let traced = compare_extraction_outcomes_with_atomic_edits(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
        &mut diagnostics,
    )?;
    let comparison = &traced.outcome.comparison;
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert!(comparison.change_candidates.is_empty());
    let change = &comparison.changes[0];
    assert_eq!(change.kind, ChangeKind::Replacement);
    assert_eq!(change.confidence, Confidence::High);
    let occurrence = &change.occurrences[0];
    assert_eq!(
        occurrence
            .old_span
            .as_ref()
            .map(|span| (span.blocks.clone(), span.comparable_range)),
        Some((
            vec![BlockId(0), BlockId(1)],
            TokenRange { start: 73, end: 74 }
        ))
    );
    assert_eq!(
        occurrence
            .new_span
            .as_ref()
            .map(|span| (span.blocks.clone(), span.comparable_range)),
        Some((
            vec![BlockId(0), BlockId(1)],
            TokenRange { start: 73, end: 74 }
        ))
    );
    assert_eq!(comparison.unresolved_regions.len(), 4);
    assert!(comparison.unresolved_regions.iter().all(|region| {
        [region.old_span.as_ref(), region.new_span.as_ref()]
            .into_iter()
            .flatten()
            .all(|span| {
                span.blocks == [BlockId(1)]
                    && (span.comparable_range.end <= 37 || span.comparable_range.start >= 38)
            })
            && region
                .evidence
                .contains(&pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderInferred)
    }));
    assert!(
        comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    Ok(())
}

#[test]
fn partial_render_order_uncertainty_recovers_one_line_beside_a_safe_replacement() -> Result<()> {
    let old = document(&[
        line("Opening anchor remains stable", 0, 148.0),
        line_at("Boundary anchor remains stable", 0, 20.0, 124.0),
        line("Release 10 remains available", 0, 112.0),
        line("Omitted middle evidence remains stable", 0, 136.0),
        line_at("Closing anchor remains stable", 0, 20.0, 100.0),
    ]);
    let new = document(&[
        line("Opening anchor remains stable", 0, 148.0),
        line_at("Boundary anchor remains stable", 0, 20.0, 124.0),
        line("Release 20 remains available", 0, 112.0),
        line("Omitted middle evidence remains stable", 0, 136.0),
        line_at("Closing anchor remains stable", 0, 20.0, 100.0),
    ]);
    let old_lines = reconstruct_lines(&old, LineOptions::default())?;
    let omitted = old_lines
        .iter()
        .find(|line| (line.baseline.y - 136.0).abs() <= f64::EPSILON)
        .expect("fixture should reconstruct the omitted middle line")
        .id;
    let old_structural_blocks = reconstruct_blocks(&old, &old_lines, BlockOptions::default())?;

    assert!(
        old_structural_blocks
            .iter()
            .any(|block| block.lines == [omitted])
    );
    let mut retained = old_structural_blocks
        .iter()
        .flat_map(|block| block.lines.iter().copied())
        .collect::<Vec<_>>();
    retained.sort_unstable_by_key(|line_id| line_id.0);
    let mut expected = old_lines.iter().map(|line| line.id).collect::<Vec<_>>();
    expected.sort_unstable_by_key(|line_id| line_id.0);
    assert_eq!(retained, expected);

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )?;

    let comparison = &outcome.comparison;
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert_eq!(comparison.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(comparison.changes[0].confidence, Confidence::Medium);
    let occurrence = &comparison.changes[0].occurrences[0];
    assert_eq!(
        occurrence
            .old_span
            .as_ref()
            .map(|span| (span.blocks.clone(), span.comparable_range)),
        Some((vec![BlockId(3)], TokenRange { start: 8, end: 9 }))
    );
    assert_eq!(
        occurrence
            .new_span
            .as_ref()
            .map(|span| (span.blocks.clone(), span.comparable_range)),
        Some((vec![BlockId(3)], TokenRange { start: 8, end: 9 }))
    );
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert_eq!(
        comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    );
    assert_eq!(
        comparison.unresolved_regions[0]
            .old_span
            .as_ref()
            .expect("uncertain middle evidence should remain on the old side")
            .blocks,
        [BlockId(1)]
    );
    assert_eq!(
        comparison.unresolved_regions[0]
            .new_span
            .as_ref()
            .expect("uncertain middle evidence should remain on the new side")
            .blocks,
        [BlockId(1)]
    );
    assert!(
        comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    Ok(())
}

#[test]
fn pipeline_atomic_trace_retains_recovered_replacement_edits() -> Result<()> {
    let old = document(&[
        line_at("Left context remains open", 0, 0.0, 300.0),
        line_at("Right context remains open", 0, 300.0, 300.0),
        line_at("Left remainder stays open", 0, 0.0, 288.0),
        line_at("Right remainder stays open", 0, 300.0, 288.0),
        line_at("The legacy sentence is removed.", 0, 0.0, 276.0),
        line_at("Right tail remains open", 0, 300.0, 276.0),
    ]);
    let new = document(&[
        line_at("Left context remains open", 0, 0.0, 300.0),
        line_at("Right context remains open", 0, 300.0, 300.0),
        line_at("Left remainder stays open", 0, 0.0, 288.0),
        line_at("Right remainder stays open", 0, 300.0, 288.0),
        line_at("A fresh sentence is inserted.", 0, 0.0, 276.0),
        line_at("Right tail remains open", 0, 300.0, 276.0),
    ]);
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            anchor_min_tokens: 4,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };
    let comparison = compare_glyph_documents(&old, &new, options)?;
    let mut diagnostics = PipelineDiagnostics::new();

    let traced = compare_extraction_outcomes_with_atomic_edits(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        options,
        &mut diagnostics,
    )?;

    assert_eq!(traced.outcome.comparison, comparison);
    assert!(traced.alignment.is_some());
    let [recovered] = traced.recovered_atomic_diffs.as_slice() else {
        panic!(
            "expected one recovered replacement trace, got {:#?}",
            traced.recovered_atomic_diffs
        );
    };
    assert!(!recovered.edits.is_empty());
    assert!(
        recovered
            .edits
            .iter()
            .all(|edit| edit.old.is_empty() != edit.new.is_empty())
    );
    let old_context_tokens =
        recovered.old_context.comparable_range.end - recovered.old_context.comparable_range.start;
    let new_context_tokens =
        recovered.new_context.comparable_range.end - recovered.new_context.comparable_range.start;
    assert!(
        recovered.edits.iter().all(|edit| {
            edit.old.end <= old_context_tokens && edit.new.end <= new_context_tokens
        })
    );
    Ok(())
}

#[test]
fn consecutive_partial_render_order_lines_are_singletons_without_evidence_loss() -> Result<()> {
    let document = document(&[
        line("Opening anchor remains stable", 0, 148.0),
        line("Release 10 remains available", 0, 112.0),
        line("Closing anchor remains stable", 0, 100.0),
        line("First omitted evidence remains stable", 0, 136.0),
        line("Second omitted evidence remains stable", 0, 124.0),
    ]);
    let lines = reconstruct_lines(&document, LineOptions::default())?;
    let omitted = [136.0, 124.0].map(|baseline| {
        lines
            .iter()
            .find(|line| (line.baseline.y - baseline).abs() <= f64::EPSILON)
            .expect("fixture should reconstruct each omitted line")
            .id
    });
    let blocks = reconstruct_blocks(&document, &lines, BlockOptions::default())?;

    for line_id in omitted {
        assert!(blocks.iter().any(|block| block.lines == [line_id]));
    }
    let mut retained = blocks
        .iter()
        .flat_map(|block| block.lines.iter().copied())
        .collect::<Vec<_>>();
    retained.sort_unstable_by_key(|line_id| line_id.0);
    let mut expected = lines.iter().map(|line| line.id).collect::<Vec<_>>();
    expected.sort_unstable_by_key(|line_id| line_id.0);
    assert_eq!(retained, expected);
    Ok(())
}

#[test]
fn unsupported_line_keeps_partial_row_major_order_uncertain() -> Result<()> {
    let old = document(&[
        line_at("Left row one remains stable", 0, 0.0, 300.0),
        line_at("Right row one remains stable", 0, 300.0, 300.0),
        line_at("Left row two remains stable", 0, 0.0, 260.0),
        line_at("Release 10 remains available", 0, 300.0, 260.0),
        line_at("Left row three remains stable", 0, 0.0, 220.0),
        line_at("Right row three remains stable", 0, 300.0, 220.0),
        vertical_line("Side A", 0, 500.0, 220.0),
        vertical_line("Side B", 0, 520.0, 220.0),
    ]);
    let new = document(&[
        line_at("Left row one remains stable", 0, 0.0, 300.0),
        line_at("Right row one remains stable", 0, 300.0, 300.0),
        line_at("Left row two remains stable", 0, 0.0, 260.0),
        line_at("Release 20 remains available", 0, 300.0, 260.0),
        line_at("Left row three remains stable", 0, 0.0, 220.0),
        line_at("Right row three remains stable", 0, 300.0, 220.0),
        vertical_line("Side A", 0, 500.0, 220.0),
        vertical_line("Side B", 0, 520.0, 220.0),
    ]);

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )?;

    let comparison = &outcome.comparison;
    assert!(comparison.changes.is_empty(), "{outcome:#?}");
    assert_eq!(comparison.change_candidates.len(), 1, "{outcome:#?}");
    let candidate = &comparison.change_candidates[0];
    assert_eq!(candidate.change.kind, ChangeKind::Replacement);
    assert_eq!(candidate.change.confidence, Confidence::Medium);
    let occurrence = &candidate.change.occurrences[0];
    assert_eq!(
        occurrence
            .old_span
            .as_ref()
            .map(|span| (span.blocks.clone(), span.comparable_range)),
        Some((vec![BlockId(4)], TokenRange { start: 8, end: 9 }))
    );
    assert_eq!(
        occurrence
            .new_span
            .as_ref()
            .map(|span| (span.blocks.clone(), span.comparable_range)),
        Some((vec![BlockId(4)], TokenRange { start: 8, end: 9 }))
    );
    assert_eq!(comparison.unresolved_regions.len(), 4);
    assert!(comparison.unresolved_regions.iter().all(|region| {
        region.evidence == [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    }));
    let unresolved_blocks = outcome
        .comparison
        .unresolved_regions
        .iter()
        .filter_map(|region| region.old_span.as_ref())
        .map(|span| span.blocks.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        unresolved_blocks,
        vec![vec![BlockId(5), BlockId(6)], vec![BlockId(4)]]
    );
    let new_unresolved_blocks = outcome
        .comparison
        .unresolved_regions
        .iter()
        .filter_map(|region| region.new_span.as_ref())
        .map(|span| span.blocks.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        new_unresolved_blocks,
        vec![vec![BlockId(5), BlockId(6)], vec![BlockId(4)]]
    );
    let unresolved_text = outcome
        .old_blocks
        .iter()
        .filter(|block| {
            unresolved_blocks
                .iter()
                .any(|blocks| blocks.contains(&block.block))
        })
        .map(|block| block.canonical.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(unresolved_text.contains("Side A"));
    assert!(unresolved_text.contains("Side B"));
    assert!(unresolved_text.contains("Release 10"));
    Ok(())
}

#[test]
fn unknown_reading_order_is_page_scoped_while_safe_changes_continue() -> Result<()> {
    let old = document(&[
        line_at("Opening anchor remains stable", 0, 0.0, 300.0),
        line_at("Ambiguous body remains stable", 1, 200.0, 300.0),
        vertical_line("Side label", 1, 400.0, 100.0),
        line_at("Boundary anchor remains stable", 2, 0.0, 300.0),
        line_at("Release 10 remains available", 3, 200.0, 300.0),
        line_at("Closing anchor remains stable", 4, 0.0, 300.0),
    ]);
    let new = document(&[
        line_at("Opening anchor remains stable", 0, 0.0, 300.0),
        line_at("Ambiguous body remains stable", 1, 200.0, 300.0),
        vertical_line("Side label", 1, 400.0, 100.0),
        line_at("Boundary anchor remains stable", 2, 0.0, 300.0),
        line_at("Release 20 remains available", 3, 200.0, 300.0),
        line_at("Closing anchor remains stable", 4, 0.0, 300.0),
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change_with_unresolved(&comparison, ChangeKind::Replacement);
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert_eq!(
        comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    );
    assert!(
        comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    Ok(())
}

#[test]
fn recovered_unknown_line_does_not_mark_an_independent_cross_page_block() -> Result<()> {
    let old = document(&[
        line("Page zero first continuation line", 0, 100.0),
        line("Page zero second continuation line", 0, 88.0),
        line("Page zero third continuation line", 0, 76.0),
        line("Page one first continuation line", 1, 100.0),
        line("Page one second continuation line", 1, 88.0),
        line("Page one third continuation line", 1, 76.0),
        vertical_line("Ambiguous side label", 1, 400.0, -200.0),
        line("Boundary anchor remains stable", 2, 100.0),
        line("Release 10 remains available", 3, 100.0),
        line("Closing anchor remains stable", 4, 100.0),
    ]);
    let new = document(&[
        line("Page zero first continuation line", 0, 100.0),
        line("Page zero second continuation line", 0, 88.0),
        line("Page zero third continuation line", 0, 76.0),
        line("Page one first continuation line", 1, 100.0),
        line("Page one second continuation line", 1, 88.0),
        line("Page one third continuation line", 1, 76.0),
        vertical_line("Ambiguous side label", 1, 400.0, -200.0),
        line("Boundary anchor remains stable", 2, 100.0),
        line("Release 20 remains available", 3, 100.0),
        line("Closing anchor remains stable", 4, 100.0),
    ]);

    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )?;

    let cross_page_block = outcome
        .old_blocks
        .iter()
        .find(|block| block.pages == [0, 1])
        .expect("fixture cadence should reconstruct one cross-page body block");
    assert_eq!(outcome.comparison.changes.len(), 1, "{outcome:#?}");
    assert_eq!(outcome.comparison.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
    let region = &outcome.comparison.unresolved_regions[0];
    assert_eq!(
        region.evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    );
    for span in [region.old_span.as_ref(), region.new_span.as_ref()] {
        assert_eq!(
            span.map(|span| (&span.blocks, span.comparable_range)),
            Some((&vec![BlockId(1)], TokenRange { start: 0, end: 20 }))
        );
    }
    let assessment = outcome
        .comparison
        .assessment
        .as_ref()
        .expect("engine assessment");
    for partition in [&assessment.old_resolution, &assessment.new_resolution] {
        let ranges = partition
            .iter()
            .filter(|range| range.block == cross_page_block.block)
            .collect::<Vec<_>>();
        assert!(!ranges.is_empty());
        assert!(
            ranges
                .iter()
                .all(|range| range.state == pdfdelta_core::diff::ResolutionState::Equal)
        );
    }
    assert!(outcome.extraction.old_complete);
    assert!(outcome.extraction.new_complete);
    assert!(
        outcome
            .comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        outcome
            .comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    Ok(())
}

#[test]
fn reports_one_generic_paragraph_insertion() -> Result<()> {
    let old = paragraphs(&[
        "Opening paragraph remains stable",
        "Closing paragraph remains stable",
    ]);
    let new = paragraphs(&[
        "Opening paragraph remains stable",
        "Inserted paragraph contains generic text",
        "Closing paragraph remains stable",
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change(&comparison, ChangeKind::Insertion);
    Ok(())
}

#[test]
fn reports_one_generic_paragraph_deletion() -> Result<()> {
    let old = paragraphs(&[
        "Opening paragraph remains stable",
        "Inserted paragraph contains generic text",
        "Closing paragraph remains stable",
    ]);
    let new = paragraphs(&[
        "Opening paragraph remains stable",
        "Closing paragraph remains stable",
    ]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change(&comparison, ChangeKind::Deletion);
    Ok(())
}

#[test]
fn filters_non_painting_modes_and_preserves_painting_modes() -> Result<()> {
    let old = paragraphs(&["Visible paragraph remains stable"]);
    let old_evidence = old.clone();
    let ignored = document(&[
        line("Visible paragraph remains stable", 0, 300.0),
        line("Invisible evidence is ignored", 0, 270.0).with_render_mode(TextRenderMode::Invisible),
        line("Clip evidence is ignored", 0, 240.0).with_render_mode(TextRenderMode::Clip),
    ]);
    let ignored_evidence = ignored.clone();

    let comparison = compare_glyph_documents(&old, &ignored, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    assert_eq!(old, old_evidence);
    assert_eq!(ignored, ignored_evidence);

    for mode in [
        TextRenderMode::Fill,
        TextRenderMode::Stroke,
        TextRenderMode::FillAndStroke,
        TextRenderMode::FillAndClip,
        TextRenderMode::StrokeAndClip,
        TextRenderMode::FillStrokeAndClip,
    ] {
        let painted = document(&[
            line("Visible paragraph remains stable", 0, 300.0),
            line("Added painted paragraph", 0, 270.0).with_render_mode(mode),
        ]);
        let comparison = compare_glyph_documents(&old, &painted, PipelineOptions::default())?;
        assert_single_change(&comparison, ChangeKind::Insertion);
    }
    Ok(())
}

#[test]
fn excludes_fully_crop_box_hidden_glyphs_but_keeps_partial_glyphs() -> Result<()> {
    let old = document(&[line("Visible paragraph remains stable", 0, 300.0)]);
    let candidate = document(&[
        line("Visible paragraph remains stable", 0, 300.0),
        line("Crop boundary evidence", 0, 270.0),
    ]);

    let mut outside_glyphs = candidate.clone().into_items();
    for glyph in outside_glyphs
        .iter_mut()
        .filter(|glyph| glyph.baseline.y == 270.0)
    {
        glyph.crop_status = GlyphCropStatus::Outside;
    }
    let outside = Document::new(outside_glyphs);
    let outside_evidence = outside.clone();
    let comparison = compare_glyph_documents(&old, &outside, PipelineOptions::default())?;
    assert_no_content_changes(&comparison);
    assert_eq!(outside, outside_evidence);

    let mut partial_glyphs = candidate.into_items();
    for glyph in partial_glyphs
        .iter_mut()
        .filter(|glyph| glyph.baseline.y == 270.0)
    {
        glyph.crop_status = GlyphCropStatus::PartiallyOutside;
    }
    let partial = Document::new(partial_glyphs);
    let comparison = compare_glyph_documents(&old, &partial, PipelineOptions::default())?;
    assert_single_change(&comparison, ChangeKind::Insertion);
    Ok(())
}

#[test]
fn excludes_fully_path_clipped_glyphs_but_keeps_partial_glyphs() -> Result<()> {
    let old = document(&[line("Visible paragraph remains stable", 0, 300.0)]);
    let candidate = document(&[
        line("Visible paragraph remains stable", 0, 300.0),
        line("Path boundary evidence", 0, 270.0),
    ]);

    let mut outside_glyphs = candidate.clone().into_items();
    for glyph in outside_glyphs
        .iter_mut()
        .filter(|glyph| glyph.baseline.y == 270.0)
    {
        glyph.path_clip_status = GlyphPathClipStatus::Outside;
    }
    let outside = Document::new(outside_glyphs);
    let outside_evidence = outside.clone();
    let comparison = compare_glyph_documents(&old, &outside, PipelineOptions::default())?;
    assert_no_content_changes(&comparison);
    assert_eq!(outside, outside_evidence);

    let mut partial_glyphs = candidate.into_items();
    for glyph in partial_glyphs
        .iter_mut()
        .filter(|glyph| glyph.baseline.y == 270.0)
    {
        glyph.path_clip_status = GlyphPathClipStatus::PartiallyOutside;
    }
    let partial = Document::new(partial_glyphs);
    let comparison = compare_glyph_documents(&old, &partial, PipelineOptions::default())?;
    assert_single_change(&comparison, ChangeKind::Insertion);
    Ok(())
}

#[test]
fn compares_empty_documents() -> Result<()> {
    let empty = Document::new(Vec::new());

    let comparison = compare_glyph_documents(&empty, &empty, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
    Ok(())
}

#[test]
fn compares_complete_extraction_outcomes_with_the_existing_pipeline() -> Result<()> {
    let old = ExtractionOutcome::complete(paragraphs(&[
        "Opening paragraph establishes context",
        "Release 10 remains available",
        "Closing paragraph confirms context",
    ]));
    let new = ExtractionOutcome::complete(paragraphs(&[
        "Opening paragraph establishes context",
        "Release 20 remains available",
        "Closing paragraph confirms context",
    ]));

    let mut diagnostics = PipelineDiagnostics::new();
    let (outcome, alignment) = compare_extraction_outcomes_with_alignment_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    assert_single_change(&outcome.comparison, ChangeKind::Replacement);
    assert_eq!(
        outcome.extraction,
        pdfdelta_core::report::ExtractionStatus::complete()
    );
    assert!(alignment.is_some());
    Ok(())
}

#[test]
fn records_each_completed_pipeline_phase_with_bounded_metrics() -> Result<()> {
    let old = ExtractionOutcome::complete(paragraphs(&["Stable old paragraph remains visible"]));
    let new = ExtractionOutcome::complete(paragraphs(&["Stable new paragraph remains visible"]));
    let mut diagnostics = PipelineDiagnostics::new();

    compare_extraction_outcomes_with_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    let records = diagnostics.records();
    assert!(records.len() <= 20, "diagnostics must remain phase-bounded");
    assert!(records.iter().all(|record| {
        record.status == PipelinePhaseStatus::Completed && record.error.is_none()
    }));
    for phase in [
        PipelinePhase::ConfigurationValidation,
        PipelinePhase::CompletenessGate,
        PipelinePhase::PreLayoutBudget,
        PipelinePhase::LineReconstruction,
        PipelinePhase::BlockReconstruction,
        PipelinePhase::Normalization,
        PipelinePhase::DiffTokenBudget,
        PipelinePhase::NgramBudget,
        PipelinePhase::FeatureBuild,
        PipelinePhase::CandidateIndex,
        PipelinePhase::Alignment,
        PipelinePhase::ExactDiff,
    ] {
        assert!(records.iter().any(|record| record.phase == phase));
    }
    let old_lines = records
        .iter()
        .find(|record| {
            record.phase == PipelinePhase::LineReconstruction
                && record.side == Some(DocumentSide::Old)
        })
        .expect("old line reconstruction should be recorded");
    assert!(
        old_lines
            .metrics
            .painting_glyphs
            .expect("painting glyph count should be recorded")
            > 0
    );
    assert_eq!(old_lines.metrics.lines, Some(1));
    let exact_diff = records
        .iter()
        .find(|record| record.phase == PipelinePhase::ExactDiff)
        .expect("exact diff should be recorded");
    assert_eq!(exact_diff.metrics.changes, Some(1));
    assert_eq!(exact_diff.metrics.proven_changed_regions, Some(0));
    Ok(())
}

#[test]
fn page_scoped_issue_records_incomplete_gate_and_downstream_successes() -> Result<()> {
    let old = ExtractionOutcome::new(
        paragraphs(&["Partial evidence remains visible"]),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Page(PageId(1)),
            "document evidence is incomplete",
        )?],
    )?;
    let new = ExtractionOutcome::complete(paragraphs(&["Complete evidence remains visible"]));
    let mut diagnostics = PipelineDiagnostics::new();

    compare_extraction_outcomes_with_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    let gate = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::CompletenessGate)
        .expect("completeness gate should be recorded");
    assert_eq!(gate.status, PipelinePhaseStatus::Incomplete);
    for phase in [
        PipelinePhase::LineReconstruction,
        PipelinePhase::Normalization,
        PipelinePhase::Alignment,
        PipelinePhase::ExactDiff,
    ] {
        assert!(diagnostics.records().iter().any(|record| {
            record.phase == phase && record.status == PipelinePhaseStatus::Completed
        }));
    }
    Ok(())
}

#[test]
fn document_scoped_issue_normalizes_retained_evidence_without_matching() -> Result<()> {
    let old = ExtractionOutcome::new(
        Document::new(Vec::new()),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unsupported,
            ExtractionScope::Document,
            "document evidence is unavailable",
        )?],
    )?;
    let new = ExtractionOutcome::complete(paragraphs(&["Complete evidence remains visible"]));
    let mut diagnostics = PipelineDiagnostics::new();

    compare_extraction_outcomes_with_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    assert_eq!(
        diagnostics.records()[1].status,
        PipelinePhaseStatus::Incomplete
    );
    for phase in [
        PipelinePhase::PreLayoutBudget,
        PipelinePhase::LineReconstruction,
        PipelinePhase::Normalization,
    ] {
        assert!(
            diagnostics
                .records()
                .iter()
                .any(|record| record.phase == phase
                    && record.status == PipelinePhaseStatus::Completed)
        );
    }
    assert!(
        !diagnostics
            .records()
            .iter()
            .any(|record| record.phase == PipelinePhase::Alignment)
    );
    Ok(())
}

#[test]
fn retains_the_active_phase_error_snapshot() {
    let document = paragraphs(&["A generic English paragraph exceeds the token budget"]);
    let old = ExtractionOutcome::complete(document.clone());
    let new = ExtractionOutcome::complete(document);
    let options = PipelineOptions {
        diff: DiffOptions {
            max_tokens: 1,
            ..DiffOptions::default()
        },
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();

    assert!(
        compare_extraction_outcomes_with_diagnostics(old, new, options, &mut diagnostics).is_err()
    );

    let failure = diagnostics
        .records()
        .last()
        .expect("failed phase should be retained");
    assert_eq!(failure.phase, PipelinePhase::PreLayoutBudget);
    assert_eq!(failure.side, Some(DocumentSide::Old));
    assert_eq!(failure.status, PipelinePhaseStatus::Failed);
    let error = failure
        .error
        .as_ref()
        .expect("failure should include an error");
    assert_eq!(error.kind, PipelineErrorKind::LimitExceeded);
    assert_eq!(error.resource, Some("diff raw evidence tokens"));
    assert_eq!(error.limit, Some(1));
    assert!(error.message.contains("exceeded its limit"));
}

#[test]
fn attributes_invalid_options_to_configuration_validation() {
    let document = paragraphs(&["Stable evidence"]);
    let options = PipelineOptions {
        ngram_size: 0,
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();

    assert!(
        compare_extraction_outcomes_with_diagnostics(
            ExtractionOutcome::complete(document.clone()),
            ExtractionOutcome::complete(document),
            options,
            &mut diagnostics,
        )
        .is_err()
    );
    assert_eq!(diagnostics.records().len(), 1);
    assert_eq!(
        diagnostics.records()[0].phase,
        PipelinePhase::ConfigurationValidation
    );
    assert_eq!(diagnostics.records()[0].status, PipelinePhaseStatus::Failed);
}

#[test]
fn retains_completed_old_budget_when_new_side_exceeds_limit() {
    let options = PipelineOptions {
        diff: DiffOptions {
            max_tokens: 5,
            ..DiffOptions::default()
        },
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();

    assert!(
        compare_extraction_outcomes_with_diagnostics(
            ExtractionOutcome::complete(paragraphs(&["id"])),
            ExtractionOutcome::complete(paragraphs(&["longtext"])),
            options,
            &mut diagnostics,
        )
        .is_err()
    );
    let budgets = diagnostics
        .records()
        .iter()
        .filter(|record| record.phase == PipelinePhase::PreLayoutBudget)
        .collect::<Vec<_>>();
    assert_eq!(budgets.len(), 2);
    assert_eq!(budgets[0].side, Some(DocumentSide::Old));
    assert_eq!(budgets[0].status, PipelinePhaseStatus::Completed);
    assert_eq!(budgets[1].side, Some(DocumentSide::New));
    assert_eq!(budgets[1].status, PipelinePhaseStatus::Failed);
}

#[test]
fn retains_completed_side_estimates_when_aggregate_ngram_budget_fails() {
    let options = PipelineOptions {
        ngram_size: 3,
        max_ngram_token_elements: 7,
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();

    assert!(
        compare_extraction_outcomes_with_diagnostics(
            ExtractionOutcome::complete(paragraphs(&["id"])),
            ExtractionOutcome::complete(paragraphs(&["id"])),
            options,
            &mut diagnostics,
        )
        .is_err()
    );
    let budgets = diagnostics
        .records()
        .iter()
        .filter(|record| record.phase == PipelinePhase::NgramBudget)
        .collect::<Vec<_>>();
    assert_eq!(budgets.len(), 3);
    assert_eq!(budgets[0].side, Some(DocumentSide::Old));
    assert_eq!(budgets[0].status, PipelinePhaseStatus::Completed);
    assert_eq!(budgets[1].side, Some(DocumentSide::New));
    assert_eq!(budgets[1].status, PipelinePhaseStatus::Completed);
    assert_eq!(budgets[2].side, None);
    assert_eq!(budgets[2].status, PipelinePhaseStatus::Failed);
}

#[test]
fn records_candidate_visits_on_completed_alignment() -> Result<()> {
    let old = ExtractionOutcome::complete(paragraphs(&["Stable old paragraph remains visible"]));
    let new = ExtractionOutcome::complete(paragraphs(&["Stable new paragraph remains visible"]));
    let mut diagnostics = PipelineDiagnostics::new();

    compare_extraction_outcomes_with_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    let alignment = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
        .expect("alignment should be recorded");
    assert_eq!(alignment.status, PipelinePhaseStatus::Completed);
    let visits = alignment
        .metrics
        .candidate_visits
        .expect("candidate visits should be recorded");
    assert!(visits > 0, "non-anchor old blocks must be charged");
    assert_eq!(
        alignment.metrics.candidate_visits_required,
        Some(visits),
        "attempted charge must equal the required sum on success"
    );
    let (exact, ngram, short_fallback) = (
        alignment
            .metrics
            .candidate_visits_required_exact
            .expect("inverted index reports an exact component"),
        alignment
            .metrics
            .candidate_visits_required_ngram
            .expect("inverted index reports an ngram component"),
        alignment
            .metrics
            .candidate_visits_required_short_fallback
            .expect("inverted index reports a short fallback component"),
    );
    assert_eq!(
        exact + ngram + short_fallback,
        visits,
        "required components must sum to the required total"
    );
    assert_eq!(
        alignment.metrics.max_candidate_visits,
        Some(AlignmentOptions::default().max_candidate_visits)
    );
    Ok(())
}

#[test]
fn records_attempted_candidate_visits_when_alignment_limit_fails() -> Result<()> {
    let old = ExtractionOutcome::complete(paragraphs(&["Stable old paragraph remains visible"]));
    let new = ExtractionOutcome::complete(paragraphs(&["Stable new paragraph remains visible"]));

    // Measure the charge under the default budget first.
    let mut diagnostics = PipelineDiagnostics::new();
    compare_extraction_outcomes_with_diagnostics(
        old.clone(),
        new.clone(),
        PipelineOptions::default(),
        &mut diagnostics,
    )?;
    let charge = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
        .expect("alignment should be recorded")
        .metrics
        .candidate_visits
        .expect("candidate visits should be recorded");
    assert!(charge > 1, "fixture must charge at least two visits");

    // A bounded search retains a partial comparison and its attempted charge.
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            max_candidate_visits: charge - 1,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();
    let outcome =
        compare_extraction_outcomes_with_diagnostics(old, new, options, &mut diagnostics)?;
    assert!(!outcome.comparison.unresolved_regions.is_empty());
    let failure = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
        .expect("alignment failure should be recorded");
    assert_eq!(failure.status, PipelinePhaseStatus::Incomplete);
    assert_eq!(failure.metrics.candidate_visits, Some(charge));
    assert_eq!(
        failure.metrics.candidate_visits_required,
        Some(charge),
        "the full required sum completes when no later estimate errors"
    );
    let (exact, ngram, short_fallback) = (
        failure
            .metrics
            .candidate_visits_required_exact
            .expect("inverted index reports an exact component"),
        failure
            .metrics
            .candidate_visits_required_ngram
            .expect("inverted index reports an ngram component"),
        failure
            .metrics
            .candidate_visits_required_short_fallback
            .expect("inverted index reports a short fallback component"),
    );
    assert_eq!(
        exact + ngram + short_fallback,
        charge,
        "required components must sum to the required total"
    );
    assert_eq!(failure.metrics.max_candidate_visits, Some(charge - 1));
    assert!(failure.error.is_none());
    Ok(())
}

#[test]
fn records_zero_candidate_visits_for_identical_documents() -> Result<()> {
    let document = paragraphs(&["Stable paragraph remains visible"]);
    let mut diagnostics = PipelineDiagnostics::new();

    compare_extraction_outcomes_with_diagnostics(
        ExtractionOutcome::complete(document.clone()),
        ExtractionOutcome::complete(document),
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    let alignment = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
        .expect("alignment should be recorded");
    assert_eq!(alignment.metrics.candidate_visits, Some(0));
    assert_eq!(alignment.metrics.candidate_visits_required, Some(0));
    // Identity alignment never consults the generator, so the components
    // are reported as zero.
    assert_eq!(alignment.metrics.candidate_visits_required_exact, Some(0));
    assert_eq!(alignment.metrics.candidate_visits_required_ngram, Some(0));
    assert_eq!(
        alignment.metrics.candidate_visits_required_short_fallback,
        Some(0)
    );
    assert_eq!(
        alignment.metrics.max_candidate_visits,
        Some(AlignmentOptions::default().max_candidate_visits)
    );
    Ok(())
}

#[test]
fn page_scoped_gap_suppresses_only_its_anchor_window() -> Result<()> {
    let old = ExtractionOutcome::new(
        paragraphs(&["Retained old paragraph remains available"]),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Page(PageId(1)),
            "page structure is ambiguous",
        )?],
    )?;
    let new = ExtractionOutcome::complete(paragraphs(&[
        "Retained old paragraph remains available",
        "Additional new paragraph is visible",
    ]));

    let mut diagnostics = PipelineDiagnostics::new();
    let (outcome, alignment) = compare_extraction_outcomes_with_alignment_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    assert!(outcome.comparison.changes.is_empty());
    assert!(outcome.comparison.formatting_changes.is_empty());
    assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
    assert_eq!(
        outcome.comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ExtractionGap]
    );
    assert!(alignment.is_some());
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    assert!(outcome.comparison.new_coverage.resolved_tokens > 0);
    assert!(
        outcome.comparison.new_coverage.resolved_tokens
            < outcome.comparison.new_coverage.total_tokens
    );
    assert_eq!(
        outcome.comparison.new_coverage.ratio,
        Some(
            outcome.comparison.new_coverage.resolved_tokens as f64
                / outcome.comparison.new_coverage.total_tokens as f64
        )
    );
    assert!(!outcome.extraction.old_complete);
    assert!(outcome.extraction.new_complete);
    assert_eq!(outcome.extraction.issues.len(), 1);
    assert_eq!(outcome.extraction.issues[0].side, DocumentSide::Old);
    assert!(!outcome.old_glyph_evidence.is_empty());
    assert!(!outcome.new_glyph_evidence.is_empty());
    assert_unique_glyph_evidence(&outcome.old_glyph_evidence);
    assert_unique_glyph_evidence(&outcome.new_glyph_evidence);
    assert_eq!(
        outcome.extraction.issues[0].kind,
        ExtractionIssueKind::Unresolved
    );
    let summary = summarize(&outcome.comparison, &outcome.extraction)?;
    assert!(!summary.comparison_complete);
    assert_eq!(summary.unresolved_extraction_issues, 1);
    assert_eq!(summary.difference_status, DifferenceStatus::Indeterminate);
    Ok(())
}

#[test]
fn preserves_replacement_outside_a_page_tree_gap() -> Result<()> {
    let old = ExtractionOutcome::new(
        document(&[
            line("Opening anchor remains exactly stable", 0, 300.0),
            line("Boundary anchor remains exactly stable", 2, 300.0),
            line("Release 10 remains available", 3, 300.0),
            line("Closing anchor remains exactly stable", 4, 300.0),
        ]),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::PageGap { retained_before: 1 },
            "old page could not be extracted",
        )?],
    )?;
    let mut new_lines = vec![line("Opening anchor remains exactly stable", 0, 300.0)];
    for (index, text) in [
        "Release 10 remains nearly available alpha",
        "Release 10 remains nearly available beta",
        "Release 10 remains nearly available gamma",
        "Release 10 remains nearly available delta",
        "Release 10 remains nearly available epsilon",
        "Release 10 remains nearly available zeta",
    ]
    .into_iter()
    .enumerate()
    {
        new_lines.push(line(text, 1, 300.0 - index as f64 * 30.0));
    }
    new_lines.extend([
        line("Boundary anchor remains exactly stable", 2, 300.0),
        line("Release 20 remains available", 3, 300.0),
        line("Closing anchor remains exactly stable", 4, 300.0),
    ]);
    let new = ExtractionOutcome::complete(document(&new_lines));
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            candidate_limit: 1,
            max_candidate_visits: 100,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();

    let outcome =
        compare_extraction_outcomes_with_diagnostics(old, new, options, &mut diagnostics)?;

    assert_eq!(outcome.comparison.changes.len(), 1, "{outcome:#?}");
    assert_eq!(outcome.comparison.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
    assert_eq!(
        outcome.comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ExtractionGap]
    );
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    assert!(outcome.comparison.new_coverage.ratio.is_some());
    assert_eq!(
        outcome.new_blocks.len(),
        10,
        "fixture requires six gap blocks"
    );
    let candidate_index = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::CandidateIndex)
        .expect("candidate index should be recorded");
    assert_eq!(candidate_index.metrics.indexed_features, Some(4));
    let alignment = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
        .expect("alignment should be recorded");
    let visits = alignment
        .metrics
        .candidate_visits
        .expect("completed alignment should record candidate visits");
    assert!(visits < 100);
    assert_eq!(alignment.metrics.candidate_visits_required, Some(visits));
    Ok(())
}

#[test]
fn preserves_replacement_outside_a_localized_glyph_gap() -> Result<()> {
    let old_document = document(&[
        line("Opening anchor remains exactly stable", 0, 300.0),
        line("Boundary anchor remains exactly stable", 0, 200.0),
        line("Release 10 remains available", 1, 300.0),
        line("Closing anchor remains exactly stable", 2, 300.0),
    ]);
    let retained_before = old_document
        .items()
        .iter()
        .take_while(|glyph| glyph.page == PageId(0) && glyph.baseline.y > 250.0)
        .count();
    let old = ExtractionOutcome::new(
        old_document,
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::GlyphGap { retained_before },
            "Form content could not be extracted",
        )?],
    )?;
    let new = ExtractionOutcome::complete(document(&[
        line("Opening anchor remains exactly stable", 0, 300.0),
        line("Unknown inserted Form content", 0, 250.0),
        line("Boundary anchor remains exactly stable", 0, 200.0),
        line("Release 20 remains available", 1, 300.0),
        line("Closing anchor remains exactly stable", 2, 300.0),
    ]));

    let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())?;

    assert_eq!(outcome.comparison.changes.len(), 1, "{outcome:#?}");
    assert_eq!(outcome.comparison.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
    assert_eq!(
        outcome.comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ExtractionGap]
    );
    Ok(())
}

#[test]
fn includes_a_block_straddling_a_glyph_gap_in_the_unresolved_window() -> Result<()> {
    let old_document = document(&[
        line("Retained text crosses the unknown Form boundary", 0, 300.0),
        line("Boundary anchor remains exactly stable", 1, 300.0),
        line("Release 10 remains available", 2, 300.0),
        line("Closing anchor remains exactly stable", 3, 300.0),
    ]);
    let old = ExtractionOutcome::new(
        old_document.clone(),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::GlyphGap {
                retained_before: old_document
                    .items()
                    .iter()
                    .take_while(|glyph| glyph.page == PageId(0))
                    .count()
                    / 2,
            },
            "Form content could not be extracted",
        )?],
    )?;
    let new = ExtractionOutcome::complete(document(&[
        line("Retained text crosses the unknown Form boundary", 0, 300.0),
        line("Boundary anchor remains exactly stable", 1, 300.0),
        line("Release 20 remains available", 2, 300.0),
        line("Closing anchor remains exactly stable", 3, 300.0),
    ]));

    let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())?;

    assert_eq!(outcome.comparison.changes.len(), 1, "{outcome:#?}");
    assert_eq!(outcome.comparison.changes[0].kind, ChangeKind::Replacement);
    assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
    assert_eq!(
        outcome.comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ExtractionGap]
    );
    assert!(
        outcome.comparison.unresolved_regions[0]
            .old_span
            .as_ref()
            .is_some_and(|span| span.blocks.iter().any(|block| block.0 == 0)),
        "the unresolved window must cover the block that straddles the glyph gap"
    );
    Ok(())
}

#[test]
fn page_scoped_gap_without_anchors_emits_no_false_changes() -> Result<()> {
    let old = ExtractionOutcome::new(
        paragraphs(&["Retained old alpha content", "Retained old beta content"]),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Page(PageId(1)),
            "old page could not be extracted",
        )?],
    )?;
    let new = ExtractionOutcome::complete(document(&[
        line("Unbounded new gamma content", 0, 300.0),
        line("Unbounded counterpart page content", 1, 300.0),
    ]));
    let options = PipelineOptions {
        alignment: AlignmentOptions {
            max_candidate_visits: 1,
            max_dp_cells: 1,
            ..AlignmentOptions::default()
        },
        ..PipelineOptions::default()
    };
    let mut diagnostics = PipelineDiagnostics::new();

    let outcome =
        compare_extraction_outcomes_with_diagnostics(old, new, options, &mut diagnostics)?;

    assert!(outcome.comparison.changes.is_empty());
    assert_eq!(outcome.comparison.unresolved_regions.len(), 1);
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    assert_eq!(outcome.comparison.new_coverage.ratio, Some(0.0));
    let alignment = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
        .expect("alignment should be recorded");
    assert_eq!(alignment.metrics.candidate_visits, Some(0));
    assert_eq!(alignment.metrics.candidate_visits_required, Some(0));
    Ok(())
}

#[test]
fn does_not_infer_insertion_from_empty_document_scoped_issue() -> Result<()> {
    let old = ExtractionOutcome::new(
        Document::new(Vec::new()),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unsupported,
            ExtractionScope::Document,
            "document feature is not supported",
        )?],
    )?;
    let new = ExtractionOutcome::complete(paragraphs(&["Visible new paragraph remains available"]));

    let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())?;

    assert!(outcome.comparison.changes.is_empty());
    assert_eq!(outcome.comparison.old_coverage.total_tokens, 0);
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    assert!(outcome.comparison.new_coverage.total_tokens > 0);
    assert_eq!(outcome.comparison.new_coverage.ratio, Some(0.0));
    Ok(())
}

#[test]
fn malformed_subtree_count_issue_suppresses_all_diffs() -> Result<()> {
    let old = ExtractionOutcome::new(
        paragraphs(&[
            "Opening anchor remains exactly stable",
            "Release 10 remains available",
            "Closing anchor remains exactly stable",
        ]),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Document,
            "reading page tree Count: expected integer",
        )?],
    )?;
    let new = ExtractionOutcome::complete(paragraphs(&[
        "Opening anchor remains exactly stable",
        "Release 20 remains available",
        "Closing anchor remains exactly stable",
    ]));

    let mut diagnostics = PipelineDiagnostics::new();
    let (outcome, alignment) = compare_extraction_outcomes_with_alignment_diagnostics(
        old,
        new,
        PipelineOptions::default(),
        &mut diagnostics,
    )?;

    assert!(outcome.comparison.changes.is_empty());
    assert!(!outcome.comparison.unresolved_regions.is_empty());
    assert!(alignment.is_none());
    assert!(!outcome.old_blocks.is_empty());
    assert!(!outcome.new_blocks.is_empty());
    assert!(!outcome.old_glyph_evidence.is_empty());
    assert!(!outcome.new_glyph_evidence.is_empty());
    assert_unique_glyph_evidence(&outcome.old_glyph_evidence);
    assert_unique_glyph_evidence(&outcome.new_glyph_evidence);
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    assert_eq!(outcome.comparison.new_coverage.ratio, Some(0.0));
    Ok(())
}

#[test]
fn enforces_raw_token_lower_bound_before_layout() {
    let document = paragraphs(&["A generic English paragraph exceeds the token budget"]);
    let options = PipelineOptions {
        diff: DiffOptions {
            max_tokens: 1,
            ..DiffOptions::default()
        },
        ..PipelineOptions::default()
    };

    assert!(matches!(
        compare_glyph_documents(&document, &document, options),
        Err(Error::LimitExceeded {
            resource: "diff raw evidence tokens",
            limit: 1,
        })
    ));
}

#[test]
fn enforces_exact_raw_budget_before_building_features() {
    let document = document(&[line("Generic word-", 0, 100.0), line("break", 0, 88.0)]);
    let options = PipelineOptions {
        diff: DiffOptions {
            max_tokens: 34,
            ..DiffOptions::default()
        },
        ..PipelineOptions::default()
    };

    assert!(matches!(
        compare_glyph_documents(&document, &document, options),
        Err(Error::LimitExceeded {
            resource: "diff raw evidence tokens",
            limit: 34,
        })
    ));
}

#[test]
fn partial_outcomes_reject_every_invalid_pipeline_option_group() -> Result<()> {
    let partial = ExtractionOutcome::new(
        paragraphs(&["Retained partial evidence remains available"]),
        vec![ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Page(PageId(1)),
            "page structure is ambiguous",
        )?],
    )?;
    let complete =
        ExtractionOutcome::complete(paragraphs(&["Retained partial evidence remains available"]));
    let cases = [
        (
            PipelineOptions {
                ngram_size: 0,
                ..PipelineOptions::default()
            },
            "ngram_size",
        ),
        (
            PipelineOptions {
                line: LineOptions {
                    max_baseline_distance_ratio: -1.0,
                    ..LineOptions::default()
                },
                ..PipelineOptions::default()
            },
            "max_baseline_distance_ratio",
        ),
        (
            PipelineOptions {
                block: BlockOptions {
                    repeated_min_pages: 1,
                    ..BlockOptions::default()
                },
                ..PipelineOptions::default()
            },
            "repeated_min_pages",
        ),
        (
            PipelineOptions {
                alignment: AlignmentOptions {
                    candidate_limit: 0,
                    ..AlignmentOptions::default()
                },
                ..PipelineOptions::default()
            },
            "candidate_limit",
        ),
        (
            PipelineOptions {
                alignment: AlignmentOptions {
                    strong_match_score: 0.1,
                    ..AlignmentOptions::default()
                },
                ..PipelineOptions::default()
            },
            "strong_match_score",
        ),
        (
            PipelineOptions {
                diff: DiffOptions {
                    max_tokens: 0,
                    ..DiffOptions::default()
                },
                ..PipelineOptions::default()
            },
            "max_tokens",
        ),
        (
            PipelineOptions {
                max_ngram_token_elements: 0,
                ..PipelineOptions::default()
            },
            "max_ngram_token_elements",
        ),
    ];

    for (options, expected) in cases {
        let partial_error = compare_extraction_outcomes(partial.clone(), complete.clone(), options)
            .expect_err("partial comparison should reject invalid pipeline options");
        let complete_error =
            compare_glyph_documents(partial.document(), complete.document(), options)
                .expect_err("complete comparison should reject invalid pipeline options");
        assert_eq!(partial_error, complete_error);
        assert!(
            matches!(&partial_error, Error::InvalidConfiguration(message) if message.contains(expected)),
            "expected {expected:?} in {partial_error}"
        );
    }
    Ok(())
}

#[test]
fn bounds_short_and_windowed_ngram_token_elements() -> Result<()> {
    for (text, ngram_size, at_limit) in [("id", 3, 8), ("helloworld", 6, 60)] {
        let document = paragraphs(&[text]);
        let options = PipelineOptions {
            ngram_size,
            max_ngram_token_elements: at_limit,
            ..PipelineOptions::default()
        };

        let comparison = compare_glyph_documents(&document, &document, options)?;
        assert_no_content_changes(&comparison);

        let over_limit = PipelineOptions {
            max_ngram_token_elements: at_limit - 1,
            ..options
        };
        assert!(matches!(
            compare_glyph_documents(&document, &document, over_limit),
            Err(Error::LimitExceeded {
                resource: "alignment n-gram token elements",
                limit,
            }) if limit == at_limit - 1
        ));
    }
    Ok(())
}

#[test]
fn defaults_cover_measured_unicode_standard_budgets() {
    let options = PipelineOptions::default();

    assert_eq!(options.diff.max_tokens, 5_100_000);
    assert_eq!(options.max_ngram_token_elements, 15_300_000);
}

#[test]
fn limit_scale_changes_only_comparison_resource_budgets() {
    let baseline = PipelineOptions::default();
    let scaled = baseline
        .scaled_limits(4.0)
        .expect("a finite scale above one should be valid");
    let expected = PipelineOptions {
        max_ngram_token_elements: baseline.max_ngram_token_elements * 4,
        alignment: AlignmentOptions {
            max_candidate_visits: baseline.alignment.max_candidate_visits * 4,
            max_dp_cells: baseline.alignment.max_dp_cells * 4,
            ..baseline.alignment
        },
        diff: DiffOptions {
            max_tokens: baseline.diff.max_tokens * 4,
            max_edit_distance: (baseline.diff.max_edit_distance * 4).min(4_000),
            max_assessment_work: baseline.diff.max_assessment_work * 4,
            max_assessment_ranges: baseline.diff.max_assessment_ranges * 4,
            ..baseline.diff
        },
        ..baseline
    };

    assert_eq!(scaled, expected);
    assert_eq!(baseline.scaled_limits(1.0), Ok(baseline));
    assert_eq!(
        baseline
            .scaled_limits(512.0)
            .expect("large benchmark scales should remain valid")
            .diff
            .max_edit_distance,
        4_000
    );
    for invalid in [0.999, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            baseline.scaled_limits(invalid),
            Err(Error::InvalidConfiguration(message)) if message.contains("limit scale")
        ));
    }
}

#[test]
fn validates_ngram_pipeline_configuration_in_feature_order() {
    let empty = Document::new(Vec::new());
    let invalid_size = PipelineOptions {
        ngram_size: 0,
        max_ngram_token_elements: 0,
        ..PipelineOptions::default()
    };
    assert!(matches!(
        compare_glyph_documents(&empty, &empty, invalid_size),
        Err(Error::InvalidConfiguration(message)) if message.contains("ngram_size")
    ));

    let invalid_limit = PipelineOptions {
        max_ngram_token_elements: 0,
        ..PipelineOptions::default()
    };
    assert!(matches!(
        compare_glyph_documents(&empty, &empty, invalid_limit),
        Err(Error::InvalidConfiguration(message))
            if message.contains("max_ngram_token_elements")
    ));
}

fn assert_no_content_changes(comparison: &Comparison) {
    assert!(comparison.changes.is_empty());
    assert!(comparison.unresolved_regions.is_empty());
    assert_eq!(comparison.old_coverage.ratio, Some(1.0));
    assert_eq!(comparison.new_coverage.ratio, Some(1.0));
}

fn assert_single_change(comparison: &Comparison, kind: ChangeKind) {
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert_eq!(comparison.changes[0].kind, kind);
    assert!(comparison.unresolved_regions.is_empty());
    assert_eq!(comparison.old_coverage.ratio, Some(1.0));
    assert_eq!(comparison.new_coverage.ratio, Some(1.0));
}

fn assert_single_change_with_unresolved(comparison: &Comparison, kind: ChangeKind) {
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert_eq!(comparison.changes[0].kind, kind);
}

fn assert_unknown_reading_order(comparison: &Comparison) {
    assert!(comparison.changes.is_empty());
    assert_eq!(comparison.unresolved_regions.len(), 1);
    assert_eq!(
        comparison.unresolved_regions[0].evidence,
        [pdfdelta_core::alignment::AlignmentEvidence::ReadingOrderUnknown]
    );
    assert!(
        comparison
            .old_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
    assert!(
        comparison
            .new_coverage
            .ratio
            .is_some_and(|ratio| ratio < 1.0)
    );
}

fn assert_unique_glyph_evidence(evidence: &[pdfdelta_core::model::GlyphEvidence]) {
    for (index, glyph) in evidence.iter().enumerate() {
        assert!(
            evidence[..index]
                .iter()
                .all(|previous| previous.id != glyph.id),
            "duplicate glyph evidence id {}",
            glyph.id.0
        );
    }
}

fn paragraphs(text: &[&str]) -> Document<Glyph> {
    let lines = text
        .iter()
        .enumerate()
        .map(|(index, text)| line(text, 0, 300.0 - index as f64 * 30.0))
        .collect::<Vec<_>>();
    document(&lines)
}

#[derive(Clone, Copy)]
struct LineSpec<'a> {
    text: &'a str,
    page: u32,
    x: f64,
    y: f64,
    direction: Vec2,
    render_mode: TextRenderMode,
}

impl LineSpec<'_> {
    fn with_render_mode(mut self, render_mode: TextRenderMode) -> Self {
        self.render_mode = render_mode;
        self
    }
}

fn line(text: &str, page: u32, y: f64) -> LineSpec<'_> {
    LineSpec {
        text,
        page,
        x: 0.0,
        y,
        direction: Vec2 { x: 1.0, y: 0.0 },
        render_mode: TextRenderMode::Fill,
    }
}

fn line_at(text: &str, page: u32, x: f64, y: f64) -> LineSpec<'_> {
    LineSpec {
        x,
        ..line(text, page, y)
    }
}

fn vertical_line(text: &str, page: u32, x: f64, y: f64) -> LineSpec<'_> {
    LineSpec {
        text,
        page,
        x,
        y,
        direction: Vec2 { x: 0.0, y: 1.0 },
        render_mode: TextRenderMode::Fill,
    }
}

fn document(lines: &[LineSpec<'_>]) -> Document<Glyph> {
    let mut glyphs = Vec::new();
    let mut next_id = 1_u64;
    for (line_index, line) in lines.iter().enumerate() {
        let mut inline_offset = 0.0;
        for character in line.text.chars() {
            if character == ' ' {
                inline_offset += 5.0;
                continue;
            }

            let x = line.x + inline_offset * line.direction.x;
            let y = line.y + inline_offset * line.direction.y;
            let (width, height) = if line.direction.x == 0.0 {
                (10.0, 5.0)
            } else {
                (5.0, 10.0)
            };

            let id = GlyphId(next_id);
            glyphs.push(Glyph {
                id,
                text: DecodedText::Mapped(character.to_string()),
                raw_code: character.to_string().into_bytes(),
                page: PageId(line.page),
                bbox: Rect {
                    min: Vec2 { x, y },
                    max: Vec2 {
                        x: x + width,
                        y: y + height,
                    },
                },
                baseline: Vec2 { x, y },
                direction: line.direction,
                font_id: FontId(1),
                font_size: 10.0,
                render_order: u32::try_from(next_id).expect("fixture glyph id should fit in u32"),
                render_mode: line.render_mode,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: u32::try_from(line_index + 1)
                            .expect("fixture line index should fit in u32"),
                        generation: 0,
                    },
                    operator_index: u32::try_from(next_id)
                        .expect("fixture glyph id should fit in u32"),
                },
            });
            next_id += 1;
            inline_offset += 6.0;
        }
    }
    Document::new(glyphs)
}
