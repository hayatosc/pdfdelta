use std::{
    io::{self, Write},
    path::Path,
};

use pdfdelta_core::{
    Error,
    model::Document,
    pdf::ParseLimits,
    pipeline::{
        PipelineDiagnostics, PipelineOptions,
        compare_extraction_outcomes_with_sentence_edge_gate_shadow_diagnostics,
    },
    report::{TextReportOptions, exit_status, render_text, summarize},
    source::{
        ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionIssue, ExtractionIssueKind,
        ExtractionLimits, ExtractionOutcome, ExtractionScope,
    },
};

use crate::{
    args::{ColorChoice, CompareCommand, ComparisonInput, ComparisonOptions, resolve_color},
    extraction_cache::{ExtractionCache, cache_key},
    fs::{
        InputReadError, ensure_named_output_does_not_alias_input,
        ensure_output_does_not_alias_input, ensure_trace_does_not_alias_input,
        output_paths_refer_to_same_file, parse_external_font_identities, parse_lopdf,
        read_limited_typed, read_password_file, write_json_atomically,
        write_text_report_atomically, write_trace_atomically,
    },
    trace::{ExecutionTrace, TraceSide},
};

pub fn compare_documents<W: Write>(
    command: CompareCommand<'_>,
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
    if let (Some(json_path), Some(trace_path)) = (command.options.json_path, command.trace_path)
        && output_paths_refer_to_same_file(trace_path, json_path, "trace/report output collision")?
    {
        return Err(format!(
            "refusing trace output {} because it refers to the JSON report {}",
            trace_path.display(),
            json_path.display()
        ));
    }
    if let (Some(output_path), Some(trace_path)) = (command.options.output_path, command.trace_path)
        && output_paths_refer_to_same_file(
            trace_path,
            output_path,
            "trace/report output collision",
        )?
    {
        return Err(format!(
            "refusing trace output {} because it refers to the text report {}",
            trace_path.display(),
            output_path.display()
        ));
    }
    if let (Some(output_path), Some(json_path)) =
        (command.options.output_path, command.options.json_path)
        && output_paths_refer_to_same_file(output_path, json_path, "report/json output collision")?
    {
        return Err(format!(
            "refusing text report output {} because it refers to the JSON report {}",
            output_path.display(),
            json_path.display()
        ));
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
    let comparison = compare_documents_traced(
        old_input,
        new_input,
        pipeline_options,
        command.options,
        command.extraction_cache_dir,
        diagnostics,
        &mut trace,
    );
    let incomplete = comparison
        .as_ref()
        .map(|(_, incomplete)| *incomplete)
        .unwrap_or(false);
    trace.finish(
        comparison
            .as_ref()
            .map(|(status, _)| *status)
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
    let old = extract_comparison_outcome(
        "old",
        TraceSide::Old,
        old_input.path,
        ExtractionContext {
            parse_limits,
            password: old_password.as_deref(),
            external_font_identities: &old_font_identities,
            cache: extraction_cache.as_ref(),
        },
        trace,
    )?;
    report_extraction_issues(diagnostics, "old", old_input.path, old.issues())?;
    let new = extract_comparison_outcome(
        "new",
        TraceSide::New,
        new_input.path,
        ExtractionContext {
            parse_limits,
            password: new_password.as_deref(),
            external_font_identities: &new_font_identities,
            cache: extraction_cache.as_ref(),
        },
        trace,
    )?;
    report_extraction_issues(diagnostics, "new", new_input.path, new.issues())?;
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
    let status =
        exit_status(&outcome.comparison, &outcome.extraction, options.strict).map_err(|error| {
            format!(
                "cannot determine comparison status for {} and {}: {error}",
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

    if let Some(output_path) = options.output_path {
        let report = render_text(
            &outcome.old_blocks,
            &outcome.new_blocks,
            &outcome.comparison,
            &outcome.extraction,
            &TextReportOptions {
                old_label: &old_input.path.display().to_string(),
                new_label: &new_input.path.display().to_string(),
                color: matches!(options.color, ColorChoice::Always),
            },
        )
        .map_err(|error| {
            format!(
                "cannot render comparison report for {} and {}: {error}",
                old_input.path.display(),
                new_input.path.display()
            )
        })?;

        if let Err(error) = write_text_report_atomically(output_path, &report) {
            trace.fail_message("report", None, "report", &error);
            return Err(error);
        }
    }

    if !options.quiet && options.json_path.is_none() && options.output_path.is_none() {
        let report_result = render_text(
            &outcome.old_blocks,
            &outcome.new_blocks,
            &outcome.comparison,
            &outcome.extraction,
            &TextReportOptions {
                old_label: &old_input.path.display().to_string(),
                new_label: &new_input.path.display().to_string(),
                color: resolve_color(options.color),
            },
        )
        .map_err(|error| {
            format!(
                "cannot render comparison report for {} and {}: {error}",
                old_input.path.display(),
                new_input.path.display()
            )
        })
        .and_then(|report| {
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
pub struct ExtractionContext<'a> {
    pub parse_limits: ParseLimits,
    pub password: Option<&'a str>,
    pub external_font_identities: &'a ExternalFontIdentities,
    pub cache: Option<&'a ExtractionCache>,
}

pub fn extract_comparison_outcome(
    side: &str,
    trace_side: TraceSide,
    path: &Path,
    context: ExtractionContext<'_>,
    trace: &mut ExecutionTrace,
) -> Result<ExtractionOutcome, String> {
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
        [("input_bytes", bytes.len())],
    );

    // The cache key covers the complete set of extraction-determining inputs,
    // so a hit is exactly equivalent to re-running parse and extraction. Any
    // cache failure falls through to the normal path below.
    let cache_entry = context.cache.map(|cache| {
        let key = cache_key(
            &bytes,
            &context.parse_limits,
            &ExtractionLimits::default(),
            context.password,
            context.external_font_identities,
        );
        (cache, key)
    });
    if let Some((cache, key)) = &cache_entry
        && let Some(outcome) = cache.load(key, &ExtractionLimits::default())
    {
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
            );
        }
        return Ok(outcome);
    }

    let parsed = match parse_lopdf(bytes, context.parse_limits, context.password) {
        Ok(parsed) => {
            let version = parsed.version();
            trace.complete(
                "pdf_parse",
                Some(trace_side),
                [
                    ("pdf_version_major", usize::from(version.major)),
                    ("pdf_version_minor", usize::from(version.minor)),
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

    match ContentStreamGlyphExtractor.extract_outcome_with_external_font_identities(
        parsed.as_ref(),
        ExtractionLimits::default(),
        context.external_font_identities,
    ) {
        Ok(outcome) => {
            if let Some((cache, key)) = &cache_entry
                && outcome.is_complete()
            {
                cache.store(key, &outcome);
            }
            if outcome.is_complete() {
                trace.complete(
                    "glyph_extraction",
                    Some(trace_side),
                    [("glyphs", outcome.document().items().len()), ("issues", 0)],
                );
            } else {
                trace.incomplete_extraction(
                    trace_side,
                    outcome.document().items().len(),
                    outcome.issues(),
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
        match issue.scope() {
            ExtractionScope::Document => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=document): {}",
                path.display(),
                issue.description()
            ),
            ExtractionScope::Page(page) => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=page, page={}): {}",
                path.display(),
                page.0,
                issue.description()
            ),
            ExtractionScope::PageGap { retained_before } => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=page-gap, retained-pages-before={retained_before}): {}",
                path.display(),
                issue.description()
            ),
            ExtractionScope::GlyphGap { retained_before } => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=glyph-gap, retained-glyphs-before={retained_before}): {}",
                path.display(),
                issue.description()
            ),
            _ => writeln!(
                writer,
                "extraction issue for {side} PDF {} (kind={kind}, scope=unknown): {}",
                path.display(),
                issue.description()
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
    let _ = writeln!(writer, "{error}");
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
}
