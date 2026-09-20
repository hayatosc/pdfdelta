use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use pdfdelta_core::{
    Error,
    model::Document,
    pdf::ParseLimits,
    pipeline::{
        PipelineDiagnostics, PipelineOptions,
        compare_extraction_outcomes_with_sentence_edge_gate_shadow_diagnostics,
    },
    report::{DifferenceStatus, ReportSummary, TextReportOptions, render_text, summarize},
    source::{
        ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionIssue, ExtractionIssueKind,
        ExtractionLimits, ExtractionOutcome, ExtractionScope,
    },
};

use crate::{
    args::{ColorChoice, CompareCommand, ComparisonInput, ComparisonOptions, resolve_color},
    evidence_text::escape_terminal_controls,
    extraction_cache::ExtractionCache,
    fs::{
        InputReadError, ensure_named_output_does_not_alias_input,
        ensure_output_does_not_alias_input, ensure_trace_does_not_alias_input,
        parse_external_font_identities, parse_lopdf, paths_refer_to_same_file, read_limited_typed,
        read_password_file, write_json_atomically, write_text_report_atomically,
        write_trace_atomically,
    },
    trace::{ExecutionTrace, TraceSide, duration_metric},
};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitStatus {
    NoContentChanges = 0,
    ContentChanges = 1,
    ExecutionError = 2,
    IncompleteComparison = 3,
}

impl ExitStatus {
    pub(crate) const fn code(self) -> u8 {
        self as u8
    }
}

/// Converts a validated core result into the CLI's process status policy.
/// Fatal execution errors are returned through `Result` and become code 2 in
/// `main`; an incomplete comparison takes precedence over detected changes.
fn exit_status(summary: &ReportSummary) -> ExitStatus {
    if summary.comparison_complete {
        match summary.difference_status {
            DifferenceStatus::Detected => ExitStatus::ContentChanges,
            DifferenceStatus::NoContentChange => ExitStatus::NoContentChanges,
            DifferenceStatus::Indeterminate => ExitStatus::IncompleteComparison,
        }
    } else {
        ExitStatus::IncompleteComparison
    }
}

pub fn compare_documents<W: Write>(
    command: CompareCommand<'_>,
    diagnostics: &mut W,
) -> Result<u8, String> {
    compare_documents_inner(command, None, diagnostics)
}

pub fn compare_documents_with_evidence<W: Write>(
    command: CompareCommand<'_>,
    evidence: &crate::evidence_compare::EvidenceOptions,
    diagnostics: &mut W,
) -> Result<u8, String> {
    compare_documents_inner(command, Some(evidence), diagnostics)
}

fn compare_documents_inner<W: Write>(
    command: CompareCommand<'_>,
    evidence: Option<&crate::evidence_compare::EvidenceOptions>,
    diagnostics: &mut W,
) -> Result<u8, String> {
    let old_path = command
        .old_path
        .ok_or_else(|| "cannot compare PDFs: OLD_PDF is required".to_owned())?;
    let new_path = command
        .new_path
        .ok_or_else(|| "cannot compare PDFs: NEW_PDF is required".to_owned())?;

    if old_path == Path::new("-") && new_path == Path::new("-") {
        return Err("cannot read both OLD_PDF and NEW_PDF from standard input".to_owned());
    }
    let pipeline_options = PipelineOptions::default()
        .scaled_limits(command.limit_scale)
        .map_err(|error| format!("cannot compare PDFs: {error}"))?;

    let old_input = ComparisonInput {
        path: old_path,
        password_file: command.old_password_file,
        font_identities: command.old_font_identities,
    };
    let new_input = ComparisonInput {
        path: new_path,
        password_file: command.new_password_file,
        font_identities: command.new_font_identities,
    };

    if let Some(trace_path) = command.trace_path {
        ensure_trace_does_not_alias_input(trace_path, old_path, new_path)?;
    }
    // One guard per pair of output destinations; the pair table keeps the
    // exact refusal messages next to the aliasing check they motivate.
    const TRACE_REPORT: &str = "trace/report output collision";
    const REPORT_JSON: &str = "report/json output collision";
    let report_output = command.options.output_path;
    let json_output = command.options.json_path;
    let trace_output = command.trace_path;
    for directory in [command.options.review_dir, command.options.agent_review_dir]
        .into_iter()
        .flatten()
    {
        crate::review::bundle::validate_destination(
            directory,
            &[json_output, report_output, trace_output],
        )?;
    }
    for (left, right, label, left_noun, right_noun) in [
        (
            trace_output,
            json_output,
            TRACE_REPORT,
            "trace output",
            "JSON report",
        ),
        (
            trace_output,
            report_output,
            TRACE_REPORT,
            "trace output",
            "text report",
        ),
        (
            report_output,
            json_output,
            REPORT_JSON,
            "text report output",
            "JSON report",
        ),
    ] {
        if let (Some(left), Some(right)) = (left, right)
            && paths_refer_to_same_file(left, right, label)?
        {
            return Err(format!(
                "refusing {left_noun} {} because it refers to the {right_noun} {}",
                left.display(),
                right.display()
            ));
        }
    }

    if let Some(cache_dir) = command.extraction_cache_dir {
        for (destination, noun) in [
            (trace_output, "trace output"),
            (json_output, "JSON report"),
            (report_output, "text report output"),
        ] {
            if let Some(destination) = destination
                && paths_refer_to_same_file(
                    destination,
                    cache_dir,
                    "extraction cache/output collision",
                )?
            {
                return Err(format!(
                    "refusing {noun} {} because it refers to the extraction cache directory {}",
                    destination.display(),
                    cache_dir.display()
                ));
            }
        }
    }

    let mut trace = ExecutionTrace::new(old_path, new_path, command.options.strict);
    let output_validation: Result<(), String> = (|| {
        if let Some(json_path) = command.options.json_path {
            ensure_output_does_not_alias_input(json_path, old_path, new_path)?;
        }
        if let Some(output_path) = command.options.output_path {
            ensure_named_output_does_not_alias_input(
                "text report output",
                output_path,
                old_path,
                new_path,
            )?;
        }
        Ok(())
    })();
    if let Err(primary) = output_validation {
        trace.fail_message("output_validation", None, "invalid_configuration", &primary);
        trace.finish(Err(()), false);
        return match command.trace_path {
            Some(path) => match write_trace_atomically(path, &trace) {
                Ok(()) => Err(primary),
                Err(trace_error) => Err(format!("{primary}; additionally, {trace_error}")),
            },
            None => Err(primary),
        };
    }
    trace.complete("output_validation", None, []);
    let comparison = if let Some(evidence) = evidence {
        crate::evidence_compare::compare(
            old_input,
            new_input,
            pipeline_options,
            command.options,
            command.extraction_cache_dir,
            evidence,
            &mut trace,
        )
    } else {
        compare_documents_traced(
            old_input,
            new_input,
            pipeline_options,
            command.options,
            command.extraction_cache_dir,
            diagnostics,
            &mut trace,
        )
    };
    let incomplete = comparison.as_ref().is_ok_and(|(_, incomplete)| *incomplete);
    trace.finish(
        comparison
            .as_ref()
            .map(|&(status, _)| status)
            .map_err(|_| ()),
        incomplete,
    );
    let trace_result = match command.trace_path {
        Some(path) => write_trace_atomically(path, &trace),
        None => Ok(()),
    };

    match (comparison, trace_result) {
        (Ok((status, _)), Ok(())) => Ok(status),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(trace_error)) => Err(trace_error),
        (Err(primary), Err(trace_error)) => Err(format!("{primary}; additionally, {trace_error}")),
    }
}

pub fn compare_documents_traced<W: Write>(
    old_input: ComparisonInput<'_>,
    new_input: ComparisonInput<'_>,
    pipeline_options: PipelineOptions,
    options: ComparisonOptions<'_>,
    extraction_cache_dir: Option<&Path>,
    diagnostics: &mut W,
    trace: &mut ExecutionTrace,
) -> Result<(u8, bool), String> {
    let parse_limits = ParseLimits::default();
    let old_password = old_input
        .password_file
        .map(read_password_file)
        .transpose()?;
    let new_password = new_input
        .password_file
        .map(read_password_file)
        .transpose()?;
    let old_font_identities = parse_external_font_identities(old_input.font_identities)?;
    let new_font_identities = parse_external_font_identities(new_input.font_identities)?;
    let extraction_cache = extraction_cache_dir.map(ExtractionCache::new);
    // Both sides extract concurrently; read, parse, and glyph extraction are
    // independent per side. Each side records into its own trace buffer so
    // the shared trace can be appended old-first afterwards, reproducing the
    // sequential phase sequence exactly, and issue reporting stays in
    // old-then-new order. When the old side fails, the new side's trace
    // buffer is discarded so the trace and reported error match the
    // sequential run, where the new side never ran.
    let mut old_trace = ExecutionTrace::new(old_input.path, new_input.path, false);
    let mut new_trace = ExecutionTrace::new(old_input.path, new_input.path, false);
    let retain_input = options.agent_review_dir.is_some();
    let extract = |input: &ComparisonInput<'_>,
                   password: Option<&str>,
                   font_identities: &ExternalFontIdentities,
                   side: &str,
                   trace_side: TraceSide,
                   trace: &mut ExecutionTrace,
                   retained: &mut Option<Arc<[u8]>>| {
        extract_comparison_outcome(
            side,
            trace_side,
            input.path,
            &ExtractionContext {
                parse_limits,
                password,
                external_font_identities: font_identities,
                cache: extraction_cache.as_ref(),
                retain_input,
            },
            trace,
            retained,
        )
    };
    let mut old_bytes = None;
    let mut new_bytes = None;
    let (old_result, new_result) = rayon::join(
        || {
            extract(
                &old_input,
                old_password.as_deref(),
                &old_font_identities,
                "old",
                TraceSide::Old,
                &mut old_trace,
                &mut old_bytes,
            )
        },
        || {
            extract(
                &new_input,
                new_password.as_deref(),
                &new_font_identities,
                "new",
                TraceSide::New,
                &mut new_trace,
                &mut new_bytes,
            )
        },
    );
    let old = match old_result {
        Ok(outcome) => {
            trace.extend_trace(old_trace);
            report_extraction_issues(diagnostics, "old", old_input.path, outcome.issues())?;
            outcome
        }
        Err(error) => {
            trace.extend_trace(old_trace);
            return Err(error);
        }
    };
    let new = match new_result {
        Ok(outcome) => {
            trace.extend_trace(new_trace);
            report_extraction_issues(diagnostics, "new", new_input.path, outcome.issues())?;
            outcome
        }
        Err(error) => {
            trace.extend_trace(new_trace);
            return Err(error);
        }
    };
    let mut pipeline_diagnostics = PipelineDiagnostics::new();
    let outcome_result = compare_extraction_outcomes_with_sentence_edge_gate_shadow_diagnostics(
        old,
        new,
        pipeline_options,
        &mut pipeline_diagnostics,
    );
    trace.extend_pipeline(&pipeline_diagnostics);
    let outcome = outcome_result.map_err(|error| {
        format!(
            "cannot compare old PDF {} with new PDF {}: {error}",
            old_input.path.display(),
            new_input.path.display()
        )
    })?;
    let summary = summarize(&outcome.comparison, &outcome.extraction).map_err(|error| {
        format!(
            "cannot summarize comparison for {} and {}: {error}",
            old_input.path.display(),
            new_input.path.display()
        )
    })?;
    let status = exit_status(&summary);

    if let Some(json_path) = options.json_path
        && let Err(error) = write_json_atomically(
            json_path,
            &outcome.old_blocks,
            &outcome.new_blocks,
            &outcome.old_glyph_evidence,
            &outcome.new_glyph_evidence,
            &outcome.comparison,
            &outcome.extraction,
        )
    {
        trace.fail_message("report", None, "report", &error);
        return Err(error);
    }

    if let Some(directory) = options.agent_review_dir {
        let (Some(old_bytes), Some(new_bytes)) = (&old_bytes, &new_bytes) else {
            return Err("review source bytes were not retained".into());
        };
        write_native_bundle(
            directory,
            &outcome,
            &summary,
            (old_input.path, old_bytes),
            (new_input.path, new_bytes),
            options.limit_scale,
        )?;
    }

    let old_label = old_input.path.display().to_string();
    let new_label = new_input.path.display().to_string();
    let render_report = |color| {
        render_text(
            &outcome.old_blocks,
            &outcome.new_blocks,
            &outcome.comparison,
            &outcome.extraction,
            &TextReportOptions {
                old_label: &old_label,
                new_label: &new_label,
                color,
            },
        )
        .map_err(|error| {
            format!(
                "cannot render comparison report for {} and {}: {error}",
                old_input.path.display(),
                new_input.path.display()
            )
        })
    };

    if let Some(output_path) = options.output_path {
        let report = render_report(matches!(options.color, ColorChoice::Always))?;

        if let Err(error) = write_text_report_atomically(output_path, &report) {
            trace.fail_message("report", None, "report", &error);
            return Err(error);
        }
    }

    if !options.quiet && options.json_path.is_none() && options.output_path.is_none() {
        let report_result = render_report(resolve_color(options.color)).and_then(|report| {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            stdout
                .write_all(report.as_bytes())
                .map_err(|error| format!("cannot write comparison report to stdout: {error}"))?;
            stdout
                .flush()
                .map_err(|error| format!("cannot flush comparison report to stdout: {error}"))
        });
        if let Err(error) = report_result {
            trace.fail_message("report", None, "report", &error);
            return Err(error);
        }
    }
    trace.complete("report", None, []);

    Ok((status.code(), !summary.comparison_complete))
}

/// Everything a single-side extraction needs besides the file itself.
/// Publishes the native-glyph review bundle for one comparison.
///
/// The plan is projected from this run's own blocks and glyph evidence, and the
/// published bytes are the ones extraction read, so every reference in the
/// bundle points at the material this comparison examined.
fn write_native_bundle(
    directory: &Path,
    outcome: &pdfdelta_core::pipeline::ComparisonOutcome,
    summary: &ReportSummary,
    old: (&Path, &[u8]),
    new: (&Path, &[u8]),
    limit_scale: f64,
) -> Result<(), String> {
    use pdfdelta_core::review::{
        BundleIdentity, EngineOutcome, EngineStatus, NativeTextReview, POLICY_VERSION,
        PipelineContract, PlannerLimits, REVIEW_SCHEMA, plan_native_text,
    };
    use sha2::{Digest, Sha256};

    let digest = |bytes: &[u8]| crate::fs::lowercase_hex(&Sha256::digest(bytes));
    let changed = summary.difference_status == DifferenceStatus::Detected;
    let identity = BundleIdentity {
        policy_version: POLICY_VERSION,
        schema: REVIEW_SCHEMA.into(),
        old_sha256: digest(old.1),
        new_sha256: digest(new.1),
        old_bytes: old.1.len(),
        new_bytes: new.1.len(),
        options: format!("native_text_only;limit_scale={limit_scale}"),
        // This contract has no evidence store, so the normalization policy
        // version stands in as the snapshot identity.
        old_revision: format!(
            "native-text-policy-{}",
            pdfdelta_core::diff::ASSESSMENT_POLICY_VERSION
        ),
        new_revision: format!(
            "native-text-policy-{}",
            pdfdelta_core::diff::ASSESSMENT_POLICY_VERSION
        ),
        backends: Vec::new(),
        pipeline: PipelineContract::NativeText,
        selected_channels: std::collections::BTreeSet::from([
            pdfdelta_core::document::Channel::Text,
        ]),
    };
    let plan = plan_native_text(
        &NativeTextReview {
            identity,
            outcome: EngineOutcome {
                status: if summary.comparison_complete {
                    if changed {
                        EngineStatus::CompleteChanged
                    } else {
                        EngineStatus::CompleteUnchanged
                    }
                } else {
                    EngineStatus::Incomplete
                },
                comparison_complete: summary.comparison_complete,
                typed_changes: outcome.comparison.changes.len(),
                inferred_changes: 0,
                scope_content_changes: 0,
                inferred_scope_changes: 0,
            },
            comparison: &outcome.comparison,
            old_blocks: &outcome.old_blocks,
            new_blocks: &outcome.new_blocks,
            old_glyphs: &outcome.old_glyph_evidence,
            new_glyphs: &outcome.new_glyph_evidence,
            extraction: &outcome.extraction,
        },
        PlannerLimits::default(),
    );
    crate::agent_review::write(
        directory,
        &plan,
        crate::agent_review::SourceDocument {
            name: old.0,
            bytes: old.1,
        },
        crate::agent_review::SourceDocument {
            name: new.0,
            bytes: new.1,
        },
        // This contract retains no rasters of its own.
        &[],
    )
}

pub struct ExtractionContext<'a> {
    pub parse_limits: ParseLimits,
    pub password: Option<&'a str>,
    pub external_font_identities: &'a ExternalFontIdentities,
    pub cache: Option<&'a ExtractionCache>,
    /// Retain the exact acquired bytes for a bundle that publishes them.
    pub retain_input: bool,
}

/// Extracts one side, optionally retaining the exact bytes it read.
///
/// The retained bytes are the ones extraction actually used, so a bundle that
/// publishes them cannot disagree with the comparison that was performed.
pub fn extract_comparison_outcome(
    side: &str,
    trace_side: TraceSide,
    path: &Path,
    context: &ExtractionContext<'_>,
    trace: &mut ExecutionTrace,
    retained: &mut Option<Arc<[u8]>>,
) -> Result<ExtractionOutcome, String> {
    // Wall-clock durations mirror the pipeline phases' duration_us metric so
    // the extraction cost is visible in the trace; the metric is
    // nondeterministic by design (see the README trace notes).
    let read_started = std::time::Instant::now();
    let bytes = match read_limited_typed(path, context.parse_limits.max_input_bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            match &error {
                InputReadError::Io(_) => {
                    trace.fail_message("input_read", Some(trace_side), "io", &error.to_string());
                }
                InputReadError::InvalidConfiguration(_) => trace.fail_message(
                    "input_read",
                    Some(trace_side),
                    "invalid_configuration",
                    &error.to_string(),
                ),
                InputReadError::LimitExceeded { limit, .. } => trace.fail_limit(
                    "input_read",
                    Some(trace_side),
                    &error.to_string(),
                    "PDF input bytes",
                    *limit,
                ),
            }
            return Err(format!(
                "cannot load {side} PDF {}: {error}",
                path.display()
            ));
        }
    };
    trace.complete(
        "input_read",
        Some(trace_side),
        [
            ("input_bytes", bytes.len()),
            ("duration_us", duration_metric(read_started.elapsed())),
        ],
    );

    if context.retain_input {
        *retained = Some(bytes.clone());
    }

    // A cache hit is exactly equivalent to re-running parse and extraction,
    // because the key covers every extraction-determining input. The traced
    // phases inside `extract_fresh` therefore run only on a miss.
    let parse_bytes = bytes.clone();
    let mut extract_fresh = || -> Result<ExtractionOutcome, String> {
        let parse_started = std::time::Instant::now();
        let parsed = match parse_lopdf(parse_bytes.clone(), context.parse_limits, context.password)
        {
            Ok(parsed) => {
                let version = parsed.version();
                trace.complete(
                    "pdf_parse",
                    Some(trace_side),
                    [
                        ("pdf_version_major", usize::from(version.major)),
                        ("pdf_version_minor", usize::from(version.minor)),
                        ("duration_us", duration_metric(parse_started.elapsed())),
                    ],
                );
                parsed
            }
            Err(error @ (Error::Unsupported(_) | Error::Unresolved(_))) => {
                trace.incomplete_core("pdf_parse", Some(trace_side), &error);
                return document_issue_outcome(error).map_err(|error| {
                    format!(
                        "cannot parse or extract {side} PDF {}: {error}",
                        path.display()
                    )
                });
            }
            Err(error) => {
                trace.fail_core("pdf_parse", Some(trace_side), &error);
                return Err(format!(
                    "cannot parse or extract {side} PDF {}: {error}",
                    path.display()
                ));
            }
        };

        let extraction_started = std::time::Instant::now();
        match ContentStreamGlyphExtractor.extract_outcome_with_external_font_identities(
            parsed.as_ref(),
            ExtractionLimits::default(),
            context.external_font_identities,
        ) {
            Ok(outcome) => {
                let extraction_duration = extraction_started.elapsed();
                if outcome.is_complete() {
                    trace.complete(
                        "glyph_extraction",
                        Some(trace_side),
                        [
                            ("glyphs", outcome.document().items().len()),
                            ("issues", 0),
                            ("duration_us", duration_metric(extraction_duration)),
                        ],
                    );
                } else {
                    trace.incomplete_extraction(
                        trace_side,
                        outcome.document().items().len(),
                        outcome.issues(),
                        Some(extraction_duration),
                    );
                }
                Ok(outcome)
            }
            Err(error) => {
                trace.fail_core("glyph_extraction", Some(trace_side), &error);
                Err(format!(
                    "cannot parse or extract {side} PDF {}: {error}",
                    path.display()
                ))
            }
        }
    };

    let (outcome, cached) = match context.cache {
        Some(cache) => cache.get_or_extract(
            &bytes,
            &context.parse_limits,
            context.password,
            context.external_font_identities,
            extract_fresh,
        )?,
        None => (extract_fresh()?, false),
    };
    if cached {
        trace.skip_phase("pdf_parse", Some(trace_side), "extraction_cache_hit");
        if outcome.is_complete() {
            trace.complete(
                "glyph_extraction",
                Some(trace_side),
                [
                    ("glyphs", outcome.document().items().len()),
                    ("issues", outcome.issues().len()),
                ],
            );
        } else {
            trace.incomplete_extraction(
                trace_side,
                outcome.document().items().len(),
                outcome.issues(),
                None,
            );
        }
    }
    Ok(outcome)
}

pub fn document_issue_outcome(error: Error) -> Result<ExtractionOutcome, Error> {
    let (kind, description, fallback) = match error {
        Error::Unsupported(description) => (
            ExtractionIssueKind::Unsupported,
            description,
            "unsupported extraction feature",
        ),
        Error::Unresolved(description) => (
            ExtractionIssueKind::Unresolved,
            description,
            "unresolved extraction content",
        ),
        error => return Err(error),
    };
    let description = if description.trim().is_empty() {
        fallback.to_owned()
    } else {
        description
    };
    ExtractionOutcome::new(
        Document::new(Vec::new()),
        vec![ExtractionIssue::new(
            kind,
            ExtractionScope::Document,
            description,
        )?],
    )
}

pub fn report_extraction_issues<W: Write>(
    writer: &mut W,
    side: &str,
    path: &Path,
    issues: &[ExtractionIssue],
) -> Result<(), String> {
    if issues.is_empty() {
        return Ok(());
    }
    for issue in issues {
        let kind = match issue.kind() {
            ExtractionIssueKind::Unsupported => "unsupported",
            ExtractionIssueKind::Unresolved => "unresolved",
        };
        // Issue descriptions can quote PDF-derived names, so they are escaped
        // before reaching a terminal.
        let description = escape_terminal_controls(issue.description());
        match issue.scope() {
            ExtractionScope::Document => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=document): {description}",
                path.display(),
            ),
            ExtractionScope::Page(page) => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=page, page={}): {description}",
                path.display(),
                u64::from(page.0) + 1,
            ),
            ExtractionScope::PageGap { retained_before } => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=page-gap, retained-pages-before={retained_before}): {description}",
                path.display(),
            ),
            ExtractionScope::GlyphGap { retained_before } => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=glyph-gap, retained-glyphs-before={retained_before}): {description}",
                path.display(),
            ),
            _ => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=unknown): {description}",
                path.display(),
            ),
        }
        .map_err(|error| {
            format!(
                "cannot write extraction diagnostics for {side} PDF {}: {error}",
                path.display()
            )
        })?;
    }
    writer.flush().map_err(|error| {
        format!(
            "cannot flush extraction diagnostics for {side} PDF {}: {error}",
            path.display()
        )
    })
}

pub fn report_fatal_error<W: Write>(writer: &mut W, error: &str) {
    let _ = writeln!(writer, "{}", escape_terminal_controls(error));
    let _ = writer.flush();
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use pdfdelta_core::{
        model::PageId,
        source::{ExtractionIssue, ExtractionIssueKind, ExtractionScope},
    };

    use super::{report_extraction_issues, report_fatal_error};

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct BrokenFlushWriter;

    impl Write for BrokenFlushWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn extraction_diagnostic_write_failure_returns_contextual_error() {
        let issue = ExtractionIssue::new(
            ExtractionIssueKind::Unsupported,
            ExtractionScope::Document,
            "fixture feature is unsupported",
        )
        .expect("fixture extraction issue should be valid");

        let error = report_extraction_issues(
            &mut BrokenPipeWriter,
            "old",
            std::path::Path::new("fixture.pdf"),
            &[issue],
        )
        .expect_err("broken diagnostic writer should fail the comparison boundary");

        assert!(error.contains("cannot write extraction diagnostics for old PDF fixture.pdf"));
        assert!(error.contains("broken pipe"));
    }

    #[test]
    fn extraction_diagnostic_flush_failure_returns_contextual_error() {
        let issue = ExtractionIssue::new(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Page(PageId(4)),
            "fixture page is unresolved",
        )
        .expect("fixture extraction issue should be valid");

        let error = report_extraction_issues(
            &mut BrokenFlushWriter,
            "new",
            std::path::Path::new("fixture.pdf"),
            &[issue],
        )
        .expect_err("broken diagnostic flush should fail the comparison boundary");

        assert!(error.contains("cannot flush extraction diagnostics for new PDF fixture.pdf"));
        assert!(error.contains("broken pipe"));
    }

    #[test]
    fn final_error_diagnostic_ignores_writer_failure() {
        report_fatal_error(&mut BrokenPipeWriter, "comparison failed");
    }

    #[test]
    fn terminal_diagnostics_escape_pdf_derived_control_characters() {
        let issue = ExtractionIssue::new(
            ExtractionIssueKind::Unsupported,
            ExtractionScope::Document,
            "font subtype /\u{1b}[31mX is not supported",
        )
        .expect("fixture extraction issue should be valid");
        let mut diagnostics = Vec::new();
        report_extraction_issues(
            &mut diagnostics,
            "old",
            std::path::Path::new("fixture.pdf"),
            &[issue],
        )
        .expect("diagnostics should write");
        let text = String::from_utf8(diagnostics).expect("diagnostics are UTF-8");
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("\\u{1b}"), "{text:?}");

        let mut fatal = Vec::new();
        report_fatal_error(&mut fatal, "cannot parse /\u{202e}font");
        let text = String::from_utf8(fatal).expect("fatal error is UTF-8");
        assert!(!text.contains('\u{202e}'), "{text:?}");
        assert!(text.contains("\\u{202e}"), "{text:?}");
    }
}
