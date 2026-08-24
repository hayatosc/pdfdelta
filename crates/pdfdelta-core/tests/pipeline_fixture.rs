use pdfdelta_core::{
    Error, Result,
    alignment::AlignmentOptions,
    diff::{ChangeKind, Comparison, DiffOptions, FormattingReason},
    layout::{BlockOptions, LineOptions},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect,
        TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
    pipeline::{
        PipelineDiagnostics, PipelineErrorKind, PipelineOptions, PipelinePhase,
        PipelinePhaseStatus, compare_extraction_outcomes,
        compare_extraction_outcomes_with_diagnostics, compare_glyph_documents,
    },
    report::{DocumentSide, ExitStatus, exit_status, summarize},
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
        [FormattingReason::Normalization]
    );
    Ok(())
}

#[test]
fn ignores_page_break_only_changes() -> Result<()> {
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
    assert!(comparison.formatting_changes.is_empty());
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
fn compares_mixed_axis_aligned_orientations() -> Result<()> {
    let document = document(&[
        line("Body text remains stable", 0, 100.0),
        vertical_line("Print or type.", 0, 20.0, 20.0),
    ]);

    let comparison = compare_glyph_documents(&document, &document, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
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

    assert_no_content_changes(&comparison);
    Ok(())
}

#[test]
fn reports_content_change_in_rotated_label() -> Result<()> {
    let old = document(&[vertical_line("Print or type.", 0, 20.0, 20.0)]);
    let new = document(&[vertical_line("Print or tyqe.", 0, 20.0, 20.0)]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_single_change(&comparison, ChangeKind::Replacement);
    assert!(comparison.changes[0].old_span.is_some());
    assert!(comparison.changes[0].new_span.is_some());
    Ok(())
}

#[test]
fn moved_rotated_label_remains_content_equivalent() -> Result<()> {
    let old = document(&[vertical_line("Print or type.", 0, 20.0, 20.0)]);
    let new = document(&[vertical_line("Print or type.", 0, 80.0, 120.0)]);

    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;

    assert_no_content_changes(&comparison);
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

    let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())?;

    assert_single_change(&outcome.comparison, ChangeKind::Replacement);
    assert_eq!(
        outcome.extraction,
        pdfdelta_core::report::ExtractionStatus::complete()
    );
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
    Ok(())
}

#[test]
fn records_incomplete_gate_without_downstream_successes() -> Result<()> {
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

    assert_eq!(diagnostics.records().len(), 4);
    assert_eq!(
        diagnostics.records()[0].phase,
        PipelinePhase::ConfigurationValidation
    );
    assert_eq!(
        diagnostics.records()[1].phase,
        PipelinePhase::CompletenessGate
    );
    assert_eq!(
        diagnostics.records()[1].status,
        PipelinePhaseStatus::Incomplete
    );
    for (record, side) in diagnostics.records()[2..]
        .iter()
        .zip([DocumentSide::Old, DocumentSide::New])
    {
        assert_eq!(record.phase, PipelinePhase::PreLayoutBudget);
        assert_eq!(record.side, Some(side));
        assert_eq!(record.status, PipelinePhaseStatus::Completed);
        assert!(record.metrics.raw_tokens.is_some());
    }
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
fn suppresses_all_changes_when_extraction_is_incomplete() -> Result<()> {
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

    let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())?;

    assert!(outcome.comparison.changes.is_empty());
    assert!(outcome.comparison.formatting_changes.is_empty());
    assert!(outcome.comparison.unresolved_regions.is_empty());
    assert_eq!(outcome.comparison.old_coverage.ratio, None);
    assert_eq!(outcome.comparison.new_coverage.ratio, Some(0.0));
    assert!(!outcome.extraction.old_complete);
    assert!(outcome.extraction.new_complete);
    assert_eq!(outcome.extraction.issues.len(), 1);
    assert_eq!(outcome.extraction.issues[0].side, DocumentSide::Old);
    assert_eq!(
        outcome.extraction.issues[0].kind,
        ExtractionIssueKind::Unresolved
    );
    let summary = summarize(&outcome.comparison, &outcome.extraction)?;
    assert!(!summary.comparison_complete);
    assert_eq!(summary.unresolved_extraction_issues, 1);
    assert_eq!(
        exit_status(&outcome.comparison, &outcome.extraction, false)?,
        ExitStatus::NoContentChanges
    );
    assert_eq!(
        exit_status(&outcome.comparison, &outcome.extraction, true)?,
        ExitStatus::IncompleteComparison
    );
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
