use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use clap::{Parser, Subcommand};
use pdfdelta_core::{
    Error,
    diff::Comparison,
    model::{DecodedText, Document, Glyph, TextRenderMode},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::{
        PipelineDiagnostics, PipelineOptions, compare_extraction_outcomes_with_diagnostics,
    },
    report::{ExtractionStatus, exit_status, render_text, summarize, write_json},
    source::{
        ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionIssue, ExtractionIssueKind,
        ExtractionLimits, ExtractionOutcome, ExtractionScope,
    },
};

use crate::trace::{ExecutionTrace, TraceSide};

mod trace;

static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser)]
#[command(
    name = "pdfdelta",
    version,
    about = "Compare meaningful text changes between two PDF documents",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(value_name = "OLD_PDF")]
    old: Option<PathBuf>,

    #[arg(value_name = "NEW_PDF")]
    new: Option<PathBuf>,

    #[arg(long, value_name = "PATH", requires = "new")]
    json: Option<PathBuf>,

    /// Write a phase-by-phase diagnostic trace to a new JSON file.
    #[arg(long, value_name = "PATH", requires = "new")]
    trace_json: Option<PathBuf>,

    #[arg(long, requires = "new")]
    strict: bool,

    /// Read the old PDF password from a file.
    #[arg(long, value_name = "PATH", requires = "new")]
    old_password_file: Option<PathBuf>,

    /// Read the new PDF password from a file.
    #[arg(long, value_name = "PATH", requires = "new")]
    new_password_file: Option<PathBuf>,

    /// Assert an external font identity as BASE_FONT=IDENTITY for the old PDF.
    #[arg(long, value_name = "BASE_FONT=IDENTITY", requires = "new")]
    old_font_identity: Vec<String>,

    /// Assert an external font identity as BASE_FONT=IDENTITY for the new PDF.
    #[arg(long, value_name = "BASE_FONT=IDENTITY", requires = "new")]
    new_font_identity: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect evidence extracted from one PDF.
    Inspect {
        document: PathBuf,

        /// Print the selected parser backend and parsed document summary.
        #[arg(long)]
        backend_info: bool,

        /// Print extracted glyph evidence.
        #[arg(long)]
        glyphs: bool,

        /// Read the PDF password from a file.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,

        /// Assert an external font identity as BASE_FONT=IDENTITY.
        #[arg(long, value_name = "BASE_FONT=IDENTITY")]
        font_identity: Vec<String>,
    },
}

#[derive(Clone, Copy)]
struct CompareCommand<'a> {
    old_path: Option<&'a Path>,
    new_path: Option<&'a Path>,
    json_path: Option<&'a Path>,
    trace_path: Option<&'a Path>,
    strict: bool,
    old_password_file: Option<&'a Path>,
    new_password_file: Option<&'a Path>,
    old_font_identities: &'a [String],
    new_font_identities: &'a [String],
}

#[derive(Clone, Copy)]
struct ComparisonInput<'a> {
    path: &'a Path,
    password_file: Option<&'a Path>,
    font_identities: &'a [String],
}

fn main() -> ExitCode {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Inspect {
            document,
            backend_info,
            glyphs,
            password_file,
            font_identity,
        }) => match inspect_document(
            &document,
            backend_info,
            glyphs,
            password_file.as_deref(),
            &font_identity,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                report_fatal_error(&mut stderr, &error);
                ExitCode::from(2)
            }
        },
        None => match compare_documents(
            CompareCommand {
                old_path: cli.old.as_deref(),
                new_path: cli.new.as_deref(),
                json_path: cli.json.as_deref(),
                trace_path: cli.trace_json.as_deref(),
                strict: cli.strict,
                old_password_file: cli.old_password_file.as_deref(),
                new_password_file: cli.new_password_file.as_deref(),
                old_font_identities: &cli.old_font_identity,
                new_font_identities: &cli.new_font_identity,
            },
            &mut stderr,
        ) {
            Ok(status) => ExitCode::from(status),
            Err(error) => {
                report_fatal_error(&mut stderr, &error);
                ExitCode::from(2)
            }
        },
    }
}

fn compare_documents<W: Write>(
    command: CompareCommand<'_>,
    diagnostics: &mut W,
) -> Result<u8, String> {
    let old_path = command
        .old_path
        .ok_or_else(|| "cannot compare PDFs: OLD_PDF is required".to_owned())?;
    let new_path = command
        .new_path
        .ok_or_else(|| "cannot compare PDFs: NEW_PDF is required".to_owned())?;
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
    if let (Some(json_path), Some(trace_path)) = (command.json_path, command.trace_path)
        && output_paths_refer_to_same_file(trace_path, json_path, "trace/report output collision")?
    {
        return Err(format!(
            "refusing trace output {} because it refers to the JSON report {}",
            trace_path.display(),
            json_path.display()
        ));
    }

    let mut trace = ExecutionTrace::new(old_path, new_path, command.strict);
    let output_validation: Result<(), String> = (|| {
        if let Some(json_path) = command.json_path {
            ensure_output_does_not_alias_input(json_path, old_path, new_path)?;
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
        command.json_path,
        command.strict,
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

fn compare_documents_traced<W: Write>(
    old_input: ComparisonInput<'_>,
    new_input: ComparisonInput<'_>,
    json_path: Option<&Path>,
    strict: bool,
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
    let old = extract_comparison_outcome(
        "old",
        TraceSide::Old,
        old_input.path,
        parse_limits,
        old_password.as_deref(),
        &old_font_identities,
        trace,
    )?;
    report_extraction_issues(diagnostics, "old", old_input.path, old.issues())?;
    let new = extract_comparison_outcome(
        "new",
        TraceSide::New,
        new_input.path,
        parse_limits,
        new_password.as_deref(),
        &new_font_identities,
        trace,
    )?;
    report_extraction_issues(diagnostics, "new", new_input.path, new.issues())?;
    let mut pipeline_diagnostics = PipelineDiagnostics::new();
    let outcome_result = compare_extraction_outcomes_with_diagnostics(
        old,
        new,
        PipelineOptions::default(),
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
        exit_status(&outcome.comparison, &outcome.extraction, strict).map_err(|error| {
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

    let report_result = if let Some(json_path) = json_path {
        write_json_atomically(json_path, &outcome.comparison, &outcome.extraction)
    } else {
        render_text(&outcome.comparison, &outcome.extraction)
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
                stdout.write_all(report.as_bytes()).map_err(|error| {
                    format!("cannot write comparison report to stdout: {error}")
                })?;
                stdout
                    .flush()
                    .map_err(|error| format!("cannot flush comparison report to stdout: {error}"))
            })
    };
    if let Err(error) = report_result {
        trace.fail_message("report", None, "report", &error);
        return Err(error);
    }
    trace.complete("report", None, []);

    Ok((status.code(), !summary.comparison_complete))
}

fn extract_comparison_outcome(
    side: &str,
    trace_side: TraceSide,
    path: &Path,
    parse_limits: ParseLimits,
    password: Option<&str>,
    external_font_identities: &ExternalFontIdentities,
    trace: &mut ExecutionTrace,
) -> Result<ExtractionOutcome, String> {
    let bytes = match read_limited_typed(path, parse_limits.max_input_bytes) {
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

    let parsed = match parse_lopdf(bytes, parse_limits, password) {
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
        external_font_identities,
    ) {
        Ok(outcome) => {
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

fn document_issue_outcome(error: Error) -> Result<ExtractionOutcome, Error> {
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

fn report_extraction_issues<W: Write>(
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

fn report_fatal_error<W: Write>(writer: &mut W, error: &str) {
    let _ = writeln!(writer, "{error}");
    let _ = writer.flush();
}

fn write_json_atomically(
    output_path: &Path,
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<(), String> {
    write_output_atomically(output_path, "JSON report", |temporary_file| {
        write_json(temporary_file, comparison, extraction).map_err(|error| {
            format!(
                "cannot render JSON comparison report for {}: {error}",
                output_path.display()
            )
        })
    })
}

fn write_trace_atomically(output_path: &Path, trace: &ExecutionTrace) -> Result<(), String> {
    write_output_atomically(output_path, "trace report", |temporary_file| {
        trace.write_json(temporary_file).map_err(|error| {
            format!(
                "cannot render diagnostic trace for {}: {error}",
                output_path.display()
            )
        })
    })
}

fn write_output_atomically(
    output_path: &Path,
    output_kind: &'static str,
    write: impl FnOnce(&mut File) -> Result<(), String>,
) -> Result<(), String> {
    let (temporary_path, mut temporary_file) =
        create_temporary_output_for(output_path, output_kind)?;
    let prepare_result = (|| {
        write(&mut temporary_file)?;
        temporary_file.flush().map_err(|error| {
            format!(
                "cannot flush temporary {output_kind} for {}: {error}",
                output_path.display()
            )
        })?;
        temporary_file.sync_all().map_err(|error| {
            format!(
                "cannot sync temporary {output_kind} for {}: {error}",
                output_path.display()
            )
        })
    })();
    drop(temporary_file);

    if let Err(error) = prepare_result {
        return Err(error_with_temporary_cleanup(
            &temporary_path,
            output_kind,
            error,
        ));
    }

    if let Err(error) = fs::hard_link(&temporary_path, output_path) {
        let message = if error.kind() == io::ErrorKind::AlreadyExists {
            format!(
                "refusing to overwrite existing {output_kind} {}: output path already exists",
                output_path.display()
            )
        } else {
            format!(
                "cannot publish {output_kind} {} atomically without replacing an existing file: {error}",
                output_path.display()
            )
        };
        return Err(error_with_temporary_cleanup(
            &temporary_path,
            output_kind,
            message,
        ));
    }

    fs::remove_file(&temporary_path).map_err(|error| {
        format!(
            "{output_kind} {} was published without overwriting an existing file, but temporary output {} could not be removed: {error}",
            output_path.display(),
            temporary_path.display()
        )
    })
}

fn error_with_temporary_cleanup(
    temporary_path: &Path,
    output_kind: &str,
    primary_error: String,
) -> String {
    match fs::remove_file(temporary_path) {
        Ok(()) => primary_error,
        Err(cleanup_error) => format!(
            "{primary_error}; temporary {output_kind} {} could not be removed: {cleanup_error}",
            temporary_path.display()
        ),
    }
}

#[cfg(test)]
fn create_temporary_output(output_path: &Path) -> Result<(PathBuf, File), String> {
    create_temporary_output_for(output_path, "JSON report")
}

fn create_temporary_output_for(
    output_path: &Path,
    output_kind: &str,
) -> Result<(PathBuf, File), String> {
    output_path.file_name().ok_or_else(|| {
        format!(
            "{output_kind} path must name a file: {}",
            output_path.display()
        )
    })?;
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary_name = format!(".pdfdelta-{}-{sequence}.tmp", std::process::id());
        let temporary_path = parent.join(temporary_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        match options.open(&temporary_path) {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot create temporary {output_kind} next to {}: {error}",
                    output_path.display()
                ));
            }
        }
    }

    Err(format!(
        "cannot create a unique temporary {output_kind} next to {}",
        output_path.display()
    ))
}

fn ensure_output_does_not_alias_input(
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    ensure_named_output_does_not_alias_input("JSON output", output_path, old_path, new_path)
}

fn ensure_trace_does_not_alias_input(
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    ensure_named_output_does_not_alias_input("trace output", output_path, old_path, new_path)
}

fn ensure_named_output_does_not_alias_input(
    output_kind: &str,
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    for (side, input_path) in [("old", old_path), ("new", new_path)] {
        if paths_refer_to_same_file(output_path, input_path, "input collision")? {
            return Err(format!(
                "refusing {output_kind} {} because it refers to the {side} PDF {}",
                output_path.display(),
                input_path.display()
            ));
        }
    }
    Ok(())
}

fn paths_refer_to_same_file(
    output_path: &Path,
    input_path: &Path,
    context: &str,
) -> Result<bool, String> {
    if output_path == input_path {
        return Ok(true);
    }

    let output_metadata = match fs::metadata(output_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "cannot inspect output path {} for {context}: {error}",
                output_path.display()
            ));
        }
    };
    let input_metadata = fs::metadata(input_path).map_err(|error| {
        format!(
            "cannot inspect input path {} for {context}: {error}",
            input_path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if output_metadata.dev() == input_metadata.dev()
            && output_metadata.ino() == input_metadata.ino()
        {
            return Ok(true);
        }
    }

    let output_canonical = fs::canonicalize(output_path).map_err(|error| {
        format!(
            "cannot resolve output path {} for {context}: {error}",
            output_path.display()
        )
    })?;
    let input_canonical = fs::canonicalize(input_path).map_err(|error| {
        format!(
            "cannot resolve input path {} for {context}: {error}",
            input_path.display()
        )
    })?;
    Ok(output_canonical == input_canonical)
}

fn output_paths_refer_to_same_file(
    first_path: &Path,
    second_path: &Path,
    context: &str,
) -> Result<bool, String> {
    if first_path == second_path {
        return Ok(true);
    }
    match fs::metadata(second_path) {
        Ok(_) => paths_refer_to_same_file(first_path, second_path, context),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect output path {} for {context}: {error}",
            second_path.display()
        )),
    }
}

fn inspect_document(
    path: &Path,
    backend_info: bool,
    glyphs: bool,
    password_file: Option<&Path>,
    font_identity: &[String],
) -> Result<(), String> {
    let backend_info = backend_info || !glyphs;
    let limits = ParseLimits::default();
    let bytes = read_limited(path, limits.max_input_bytes)?;
    let password = password_file.map(read_password_file).transpose()?;
    let external_font_identities = parse_external_font_identities(font_identity)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if backend_info {
        inspect_backend(
            path,
            Arc::clone(&bytes),
            limits,
            password.as_deref(),
            &mut stdout,
        )?;
    }
    if glyphs {
        inspect_glyphs(
            path,
            bytes,
            limits,
            password.as_deref(),
            &external_font_identities,
            &mut stdout,
        )?;
    }
    stdout.flush().map_err(|error| {
        format!(
            "cannot flush inspection output for {}: {error}",
            path.display()
        )
    })
}

fn inspect_backend<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    limits: ParseLimits,
    password: Option<&str>,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, limits, password)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let version = pdf.version();
    let page_count = pdf
        .pages()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .len();

    write_inspection_line(writer, path, format_args!("backend: {}", LopdfParser::NAME))?;
    write_inspection_line(
        writer,
        path,
        format_args!("pdf-version: {}.{}", version.major, version.minor),
    )?;
    write_inspection_line(writer, path, format_args!("pages: {page_count}"))?;
    for issue in pdf.issues() {
        write_inspection_line(
            writer,
            path,
            format_args!("parser-issue: unresolved: {}", issue.description()),
        )?;
    }
    Ok(())
}

fn inspect_glyphs<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    parse_limits: ParseLimits,
    password: Option<&str>,
    external_font_identities: &ExternalFontIdentities,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, parse_limits, password)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let outcome = ContentStreamGlyphExtractor
        .extract_outcome_with_external_font_identities(
            pdf.as_ref(),
            ExtractionLimits::default(),
            external_font_identities,
        )
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    for issue in outcome.issues() {
        write_inspection_line(
            writer,
            path,
            format_args!(
                "extraction-issue: {:?}: {}",
                issue.scope(),
                issue.description()
            ),
        )?;
    }
    let document = outcome.document();

    write_inspection_line(
        writer,
        path,
        format_args!("glyphs: {}", document.items().len()),
    )?;
    for glyph in document.items() {
        let glyph = format_glyph(glyph);
        write_inspection_line(writer, path, format_args!("{glyph}"))?;
    }
    Ok(())
}

fn write_inspection_line<W: Write>(
    writer: &mut W,
    path: &Path,
    line: std::fmt::Arguments<'_>,
) -> Result<(), String> {
    writeln!(writer, "{line}").map_err(|error| {
        format!(
            "cannot write inspection output for {}: {error}",
            path.display()
        )
    })
}

fn format_glyph(glyph: &Glyph) -> String {
    let text = match &glyph.text {
        DecodedText::Mapped(text) => format!("text={text:?}"),
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => format!(
            "unmapped-font-hash={} unmapped-glyph-id={glyph_id}",
            lowercase_hex(&font_hash.0)
        ),
    };
    format!(
        "glyph id={} page={} {} raw-hex={} bbox=({},{},{},{}) baseline=({},{}) direction=({},{}) font-id={} font-size={} render-order={} render-mode={} content-stream-object={} content-stream-generation={} operator-index={}",
        glyph.id.0,
        glyph.page.0,
        text,
        lowercase_hex(&glyph.raw_code),
        glyph.bbox.min.x,
        glyph.bbox.min.y,
        glyph.bbox.max.x,
        glyph.bbox.max.y,
        glyph.baseline.x,
        glyph.baseline.y,
        glyph.direction.x,
        glyph.direction.y,
        glyph.font_id.0,
        glyph.font_size,
        glyph.render_order,
        render_mode_name(glyph.render_mode),
        glyph.provenance.content_stream.object_number,
        glyph.provenance.content_stream.generation,
        glyph.provenance.operator_index,
    )
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn render_mode_name(mode: TextRenderMode) -> &'static str {
    match mode {
        TextRenderMode::Fill => "fill",
        TextRenderMode::Stroke => "stroke",
        TextRenderMode::FillAndStroke => "fill-and-stroke",
        TextRenderMode::Invisible => "invisible",
        TextRenderMode::FillAndClip => "fill-and-clip",
        TextRenderMode::StrokeAndClip => "stroke-and-clip",
        TextRenderMode::FillStrokeAndClip => "fill-stroke-and-clip",
        TextRenderMode::Clip => "clip",
    }
}

#[derive(Debug)]
enum InputReadError {
    Io(String),
    InvalidConfiguration(String),
    LimitExceeded { message: String, limit: usize },
}

impl fmt::Display for InputReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::InvalidConfiguration(message)
            | Self::LimitExceeded { message, .. } => formatter.write_str(message),
        }
    }
}

fn read_limited(path: &Path, max_bytes: usize) -> Result<Arc<[u8]>, String> {
    read_limited_typed(path, max_bytes).map_err(|error| error.to_string())
}

const MAX_PASSWORD_FILE_BYTES: usize = 4_096;

fn read_password_file(path: &Path) -> Result<String, String> {
    let bytes = read_limited_typed(path, MAX_PASSWORD_FILE_BYTES)
        .map_err(|error| format!("cannot read password file {}: {error}", path.display()))?;
    let mut bytes = bytes.as_ref();
    if let Some(without_newline) = bytes.strip_suffix(b"\r\n") {
        bytes = without_newline;
    } else if let Some(without_newline) = bytes.strip_suffix(b"\n") {
        bytes = without_newline;
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| format!("password file {} is not valid UTF-8", path.display()))
}

fn parse_external_font_identities(values: &[String]) -> Result<ExternalFontIdentities, String> {
    let mut identities = ExternalFontIdentities::default();
    for value in values {
        let (base_font, identity) = value.split_once('=').ok_or_else(|| {
            format!("external font identity must use BASE_FONT=IDENTITY, got {value:?}")
        })?;
        let base_font = base_font.strip_prefix('/').unwrap_or(base_font);
        identities
            .insert(base_font.as_bytes(), identity.as_bytes())
            .map_err(|error| format!("invalid external font identity for /{base_font}: {error}"))?;
    }
    Ok(identities)
}

fn parse_lopdf(
    bytes: Arc<[u8]>,
    limits: ParseLimits,
    password: Option<&str>,
) -> pdfdelta_core::Result<Box<dyn pdfdelta_core::pdf::ParsedPdf>> {
    match password {
        Some(password) => LopdfParser.parse_with_password(bytes, limits, password),
        None => LopdfParser.parse(bytes, limits),
    }
}

fn read_limited_typed(path: &Path, max_bytes: usize) -> Result<Arc<[u8]>, InputReadError> {
    let file = File::open(path)
        .map_err(|error| InputReadError::Io(format!("cannot open {}: {error}", path.display())))?;
    let read_limit = u64::try_from(max_bytes)
        .map_err(|_| {
            InputReadError::InvalidConfiguration(
                "configured PDF input limit does not fit in u64".to_owned(),
            )
        })?
        .saturating_add(1);
    let mut reader = file.take(read_limit);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| InputReadError::Io(format!("cannot read {}: {error}", path.display())))?;
    if bytes.len() > max_bytes {
        return Err(InputReadError::LimitExceeded {
            message: format!(
                "cannot read {}: PDF input exceeds the {max_bytes}-byte limit",
                path.display()
            ),
            limit: max_bytes,
        });
    }
    Ok(Arc::from(bytes))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use clap::Parser;
    use pdfdelta_core::{
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        pdf::ObjectRef,
        source::{ExtractionIssue, ExtractionIssueKind, ExtractionScope},
    };

    use super::{
        Cli, Command, InputReadError, format_glyph, read_limited_typed, report_extraction_issues,
        report_fatal_error, write_inspection_line,
    };

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
    fn parses_strict_comparison_mode() {
        let cli = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--strict"])
            .expect("strict comparison arguments should parse");

        assert!(cli.strict);
        assert_eq!(cli.old.as_deref(), Some(std::path::Path::new("old.pdf")));
        assert_eq!(cli.new.as_deref(), Some(std::path::Path::new("new.pdf")));
    }

    #[test]
    fn strict_mode_requires_a_new_document() {
        assert!(Cli::try_parse_from(["pdfdelta", "old.pdf", "--strict"]).is_err());
    }

    #[test]
    fn parses_side_specific_password_and_font_identity_inputs() {
        let cli = Cli::try_parse_from([
            "pdfdelta",
            "old.pdf",
            "new.pdf",
            "--old-password-file",
            "old.password",
            "--new-password-file",
            "new.password",
            "--old-font-identity",
            "TraditionalArabic=windows-v1",
            "--new-font-identity",
            "TraditionalArabic=windows-v1",
        ])
        .expect("explicit comparison identities should parse");

        assert_eq!(
            cli.old_password_file.as_deref(),
            Some(std::path::Path::new("old.password"))
        );
        assert_eq!(
            cli.new_password_file.as_deref(),
            Some(std::path::Path::new("new.password"))
        );
        assert_eq!(cli.old_font_identity, ["TraditionalArabic=windows-v1"]);
        assert_eq!(cli.new_font_identity, ["TraditionalArabic=windows-v1"]);
    }

    #[test]
    fn parses_backend_info_inspection() {
        let cli = Cli::try_parse_from(["pdfdelta", "inspect", "document.pdf", "--backend-info"])
            .expect("backend inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: true,
                glyphs: false,
                ..
            })
        ));
    }

    #[test]
    fn parses_glyph_inspection() {
        let cli = Cli::try_parse_from(["pdfdelta", "inspect", "document.pdf", "--glyphs"])
            .expect("glyph inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: false,
                glyphs: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_inspection_password_and_font_identity_inputs() {
        let cli = Cli::try_parse_from([
            "pdfdelta",
            "inspect",
            "document.pdf",
            "--glyphs",
            "--password-file",
            "document.password",
            "--font-identity",
            "TraditionalArabic=windows-v1",
        ])
        .expect("inspection identity inputs should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                password_file: Some(path),
                font_identity,
                ..
            }) if path == std::path::Path::new("document.password")
                && font_identity == ["TraditionalArabic=windows-v1"]
        ));
    }

    #[test]
    fn parses_backend_and_glyph_inspection() {
        let cli = Cli::try_parse_from([
            "pdfdelta",
            "inspect",
            "document.pdf",
            "--backend-info",
            "--glyphs",
        ])
        .expect("combined inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: true,
                glyphs: true,
                ..
            })
        ));
    }

    #[test]
    fn inspection_write_failure_returns_contextual_error() {
        let error = write_inspection_line(
            &mut BrokenPipeWriter,
            std::path::Path::new("fixture.pdf"),
            format_args!("backend: test"),
        )
        .expect_err("broken inspection writer should fail");

        assert!(error.contains("cannot write inspection output for fixture.pdf"));
        assert!(error.contains("broken pipe"));
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
    fn classifies_input_size_limits_separately_from_io_failures() {
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-input-limit-test-{}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, b"four").expect("input limit fixture should be written");

        let error = read_limited_typed(&path, 3).expect_err("input should exceed the limit");
        std::fs::remove_file(path).expect("input limit fixture should be removed");

        assert!(matches!(
            error,
            InputReadError::LimitExceeded { limit: 3, .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn temporary_json_report_has_private_permissions() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let output_path = std::env::temp_dir().join(format!(
            "pdfdelta-temporary-mode-test-{}.json",
            std::process::id()
        ));
        let (temporary_path, temporary_file) = super::create_temporary_output(&output_path)
            .expect("temporary JSON report should be created");
        let mode = temporary_file
            .metadata()
            .expect("temporary JSON metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        drop(temporary_file);
        fs::remove_file(temporary_path).expect("temporary JSON report should be removed");

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn formats_glyph_evidence_stably() {
        let glyph = Glyph {
            id: GlyphId(7),
            text: DecodedText::Mapped("English\nA".to_owned()),
            raw_code: vec![0x41, 0x0a, 0xff],
            page: PageId(2),
            bbox: Rect {
                min: Vec2 { x: 10.25, y: 20.5 },
                max: Vec2 { x: 16.75, y: 30.0 },
            },
            baseline: Vec2 { x: 1.0, y: 0.0 },
            direction: Vec2 { x: 0.0, y: -1.0 },
            font_id: FontId(3),
            font_size: 11.5,
            render_order: 4,
            render_mode: TextRenderMode::FillAndStroke,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 12,
                    generation: 2,
                },
                operator_index: 9,
            },
        };

        assert_eq!(
            format_glyph(&glyph),
            "glyph id=7 page=2 text=\"English\\nA\" raw-hex=410aff bbox=(10.25,20.5,16.75,30) baseline=(1,0) direction=(0,-1) font-id=3 font-size=11.5 render-order=4 render-mode=fill-and-stroke content-stream-object=12 content-stream-generation=2 operator-index=9"
        );
    }
}
