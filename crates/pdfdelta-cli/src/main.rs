use std::{
    collections::BTreeSet,
    io::{self, Write},
    process::ExitCode,
};

use clap::{CommandFactory, Parser};
use pdfdelta_diff::Error as DiffError;

mod agent_review;
mod args;
mod compare;
mod evidence_compare;
mod evidence_text;
mod extraction_cache;
mod flow_cmd;
mod flow_exit;
mod flow_full;
mod flow_native;
mod flow_review;
mod flow_sources;
mod flow_trace;
mod fs;
mod image_hashes;
mod inspect;
mod native_worker;
mod render;
mod review;
mod text_envelope;
mod widgets;
#[cfg(target_os = "macos")]
mod worker_allocator;

#[cfg(target_os = "macos")]
#[global_allocator]
static WORKER_ALLOCATOR: worker_allocator::WorkerAllocator = worker_allocator::WorkerAllocator;

use crate::{
    args::{Cli, Command, CompareCommand, Engine, resolve_color},
    compare::{ExitStatus, report_fatal_error},
    inspect::inspect_document,
};

/// Answers one bounded query against an existing bundle.
///
/// A successful read exits 0 whatever the original comparison concluded: the
/// engine's own status travels inside the payload, so a caller never confuses
/// "this bundle was readable" with "the comparison was complete".
fn review_query<W: Write>(action: args::ReviewCommand, diagnostics: &mut W) -> ExitCode {
    let answer = match action {
        args::ReviewCommand::List {
            directory,
            cursor,
            max_output_bytes,
        } => agent_review::list(&directory, cursor.as_deref(), max_output_bytes),
        args::ReviewCommand::Import {
            directory,
            decisions,
            output,
            max_output_bytes,
        } => agent_review::import(&directory, &decisions, &output, max_output_bytes),
        args::ReviewCommand::Render {
            directory,
            case,
            output,
            max_output_bytes,
        } => agent_review::render(&directory, &case, &output, max_output_bytes),
        args::ReviewCommand::Show {
            directory,
            case,
            detail,
            cursor,
            max_output_bytes,
        } => agent_review::show(
            &directory,
            &case,
            detail.into(),
            cursor.as_deref(),
            max_output_bytes,
        ),
    };
    match answer {
        Ok(payload) => match agent_review::emit(&payload) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                report_fatal_error(diagnostics, &error);
                ExitCode::from(ExitStatus::ExecutionError.code())
            }
        },
        Err(error) => {
            // The failure itself is a bounded JSON document, so a caller can
            // parse the refusal with the same reader it uses for answers.
            let encoded = serde_json::to_vec(&error)
                .unwrap_or_else(|_| br#"{"error":"encoding_failed"}"#.to_vec());
            let _ = agent_review::emit(&encoded);
            ExitCode::from(ExitStatus::ExecutionError.code())
        }
    }
}

/// Restarts the comparison with a single glibc malloc arena.
///
/// A multi-million-glyph flow comparison keeps both sides' reading-order
/// candidates alive while the second side is extracted. glibc retains the
/// first side's freed extraction buffers in per-thread arenas, so with several
/// arenas the second extraction can exceed a bounded process memory budget
/// even though the buffers are no longer in use. A single arena lets the
/// allocator reuse those pages.
///
/// The restart happens after argument parsing but before any comparison work,
/// keeps the same arguments and standard streams, and only sets an
/// environment variable for this invocation. The default engine and an
/// explicit `--engine flow` both qualify; a removed proof selection and other
/// subcommands are never restarted. An explicit `MALLOC_ARENA_MAX` is never
/// overridden, and platforms without the glibc arena setting run unchanged.
#[cfg(unix)]
fn restart_flow_with_bounded_allocator_arenas(cli: &Cli) {
    use std::os::unix::process::CommandExt;

    if std::env::var_os("MALLOC_ARENA_MAX").is_some() {
        return;
    }
    let uses_flow = match &cli.command {
        Some(Command::Flow { .. }) => true,
        Some(_) => false,
        None => cli.engine != Some(Engine::Proof),
    };
    if !uses_flow {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let error = std::process::Command::new(executable)
        .args(std::env::args_os().skip(1))
        .env("MALLOC_ARENA_MAX", "1")
        .exec();
    eprintln!("pdfdelta: could not restart flow with bounded allocator arenas: {error}");
}

/// Leaves the allocator configuration unchanged on non-Unix platforms.
#[cfg(not(unix))]
fn restart_flow_with_bounded_allocator_arenas(_cli: &Cli) {}

/// Explains that the proof engine was removed and names its replacements.
const PROOF_ENGINE_REMOVED: &str = "the proof engine was removed; use the flow engine (the default or `--engine flow`) with `--trace-json`, `--compact-json`, `--full-json`, `--review`, or `--agent-review`, or use `pdfdelta flow` for a token-stream report";

fn main() -> ExitCode {
    let cli = Cli::parse();
    restart_flow_with_bounded_allocator_arenas(&cli);
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    if cli.engine == Some(Engine::Proof) {
        report_fatal_error(&mut stderr, PROOF_ENGINE_REMOVED);
        return ExitCode::from(ExitStatus::ExecutionError.code());
    }
    match cli.command {
        Some(Command::TextEnvelope {
            input,
            max_work,
            max_cells,
            max_output_bytes,
        }) => match text_envelope::query(&input, max_work, max_cells, max_output_bytes) {
            Ok(payload) => match agent_review::emit(&payload) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    report_fatal_error(&mut stderr, &error);
                    ExitCode::from(ExitStatus::ExecutionError.code())
                }
            },
            Err(error) => {
                report_fatal_error(&mut stderr, &error);
                ExitCode::from(ExitStatus::ExecutionError.code())
            }
        },
        Some(Command::HashImages { cpu_seconds }) => match image_hashes::worker(cpu_seconds) {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => ExitCode::from(code),
        },
        Some(Command::HashImagePage { page, cpu_seconds }) => {
            match image_hashes::worker_page(page, cpu_seconds) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => ExitCode::from(code),
            }
        }
        Some(Command::HashImagePageEncoded { page, cpu_seconds }) => {
            match image_hashes::worker_encoded_page(page, cpu_seconds) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => ExitCode::from(code),
            }
        }
        Some(Command::AcquireNative) => match native_worker::worker() {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => ExitCode::from(code),
        },
        Some(Command::RenderPage {
            page,
            pages,
            width,
            height,
            object_number,
            generation,
            cpu_seconds,
        }) => match render::worker(
            page,
            pages,
            width,
            height,
            object_number,
            generation,
            cpu_seconds,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => ExitCode::from(code),
        },
        Some(Command::Inspect {
            document,
            backend_info,
            glyphs,
            objects,
            svg,
            password_file,
            font_identity,
        }) => match inspect_document(
            &document,
            backend_info,
            glyphs,
            objects,
            svg.as_deref(),
            password_file.as_deref(),
            &font_identity,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                report_fatal_error(&mut stderr, &error);
                ExitCode::from(ExitStatus::ExecutionError.code())
            }
        },
        Some(Command::Flow {
            old,
            new,
            json,
            tokens,
            geometry,
            limit_scale,
        }) => {
            let color = resolve_color(cli.color);
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match flow_cmd::run(
                &old,
                &new,
                &flow_cmd::FlowRunOptions {
                    json_path: json.as_deref(),
                    include_tokens: tokens,
                    include_geometry: geometry,
                    limit_scale,
                    color,
                },
                &mut stdout,
            ) {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    report_fatal_error(&mut stderr, &error.to_string());
                    ExitCode::from(ExitStatus::ExecutionError.code())
                }
            }
        }
        Some(Command::Review { action }) => review_query(action, &mut stderr),
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            clap_complete::generate(shell, &mut cmd, name, &mut stdout);
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        None => {
            let command = CompareCommand {
                old_path: cli.old.as_deref(),
                new_path: cli.new.as_deref(),
                trace_path: cli.trace_json.as_deref(),
                old_password_file: cli.old_password_file.as_deref(),
                new_password_file: cli.new_password_file.as_deref(),
                old_font_identities: &cli.old_font_identity,
                new_font_identities: &cli.new_font_identity,
                limit_scale: cli.limit_scale,
                extraction_cache_dir: cli.extraction_cache_dir.as_deref(),
                options: args::ComparisonOptions {
                    json_path: cli.json.as_deref(),
                    review_dir: cli.review.as_deref(),
                    agent_review_dir: cli.agent_review.as_deref(),
                    limit_scale: cli.limit_scale,
                    output_path: cli.output.as_deref(),
                    quiet: cli.quiet,
                    color: cli.color,
                },
            };
            let result = (|| -> Result<u8, DiffError> {
                let old = cli.old.as_deref().ok_or_else(|| {
                    DiffError::backend("cannot compare PDFs: OLD_PDF is required")
                })?;
                let new = cli.new.as_deref().ok_or_else(|| {
                    DiffError::backend("cannot compare PDFs: NEW_PDF is required")
                })?;
                let old_password = cli
                    .old_password_file
                    .as_deref()
                    .map(crate::fs::read_password_file)
                    .transpose()
                    .map_err(DiffError::backend)?;
                let new_password = cli
                    .new_password_file
                    .as_deref()
                    .map(crate::fs::read_password_file)
                    .transpose()
                    .map_err(DiffError::backend)?;
                let old_fonts = crate::fs::parse_external_font_identities(&cli.old_font_identity)
                    .map_err(DiffError::backend)?;
                let new_fonts = crate::fs::parse_external_font_identities(&cli.new_font_identity)
                    .map_err(DiffError::backend)?;
                let cache = cli
                    .extraction_cache_dir
                    .as_deref()
                    .map(crate::extraction_cache::ExtractionCache::new);
                if cli.native_text_only {
                    // The native path writes the same report, text, trace, and
                    // bundle destinations as the shared-evidence path, so it
                    // applies the same alias and collision checks.
                    compare::validate_destinations(command)?;
                    // A report written to a file has no terminal to inspect,
                    // so `auto` stays plain there; stdout keeps the same
                    // auto/always/never contract.
                    let color = if cli.output.is_some() {
                        cli.color == crate::args::ColorChoice::Always
                    } else {
                        resolve_color(cli.color)
                    };
                    flow_native::run(
                        old,
                        new,
                        &flow_native::NativeFlowOptions {
                            limit_scale: cli.limit_scale,
                            old_password: old_password.as_deref(),
                            new_password: new_password.as_deref(),
                            old_font_identities: &old_fonts,
                            new_font_identities: &new_fonts,
                            cache: cache.as_ref(),
                            json_path: cli.json.as_deref(),
                            output_path: cli.output.as_deref(),
                            quiet: cli.quiet,
                            color,
                            full_json: cli.full_json,
                            trace_json: cli.trace_json.as_deref(),
                            agent_review_dir: cli.agent_review.as_deref(),
                            agent_case_budget: cli.agent_case_budget,
                        },
                        &mut std::io::stdout(),
                    )
                } else {
                    let channels: BTreeSet<_> =
                        cli.channels.iter().copied().map(Into::into).collect();
                    if let Some(path) = cli.trace_json.as_deref()
                        && path.exists()
                    {
                        return Err(DiffError::backend(format!(
                            "refusing to overwrite existing flow trace report {}: output path already exists",
                            path.display()
                        )));
                    }
                    let mut flow_trace = cli
                        .trace_json
                        .as_deref()
                        .map(|_| crate::flow_trace::FlowTrace::new(old, new, cli.limit_scale));
                    let flow = if channels.contains(&pdfdelta_core::document::Channel::Text) {
                        match flow_cmd::compare_flow(
                            old,
                            new,
                            &flow_cmd::FlowComparisonOptions {
                                limit_scale: cli.limit_scale,
                                // The agent bundle cites exact glyph sources
                                // and publishes the input bytes; the HTML
                                // bundle publishes the bytes but reads
                                // geometry from tokens. A full report needs
                                // the glyph records themselves.
                                retain_sources: cli.agent_review.is_some(),
                                retain_source_glyphs: false,
                                retain_input: cli.review.is_some() || cli.agent_review.is_some(),
                                old_password: old_password.as_deref(),
                                new_password: new_password.as_deref(),
                                old_font_identities: &old_fonts,
                                new_font_identities: &new_fonts,
                                cache: cache.as_ref(),
                            },
                            flow_trace.as_mut(),
                        ) {
                            Ok(flow) => Some(flow),
                            Err(error) => {
                                if let Some(trace) = flow_trace.as_mut() {
                                    trace.finish(2, false, true);
                                    crate::flow_trace::write_atomically(
                                        cli.trace_json.as_deref().expect("trace exists"),
                                        trace,
                                    )?;
                                }
                                return Err(error);
                            }
                        }
                    } else {
                        None
                    };
                    let channel_start = std::time::Instant::now();
                    let channel_count = channels.len();
                    // The flow trace is written here; the shared evidence
                    // path must not also emit a trace for the same path.
                    let mut flow_command = command;
                    flow_command.trace_path = None;
                    let result = compare::compare_documents_with_evidence(
                        flow_command,
                        &evidence_compare::EvidenceOptions {
                            channels,
                            flow_text: flow.as_ref(),
                            agent_case_budget: cli.agent_case_budget,
                        },
                    );
                    if let Some(trace) = flow_trace.as_mut() {
                        match &result {
                            Ok((status, render_cpu, image_cpu)) => {
                                trace.record_render_cpu(
                                    crate::flow_trace::TraceSide::Old,
                                    &render_cpu.old,
                                );
                                trace.record_render_cpu(
                                    crate::flow_trace::TraceSide::New,
                                    &render_cpu.new,
                                );
                                trace.record_image_cpu(
                                    crate::flow_trace::TraceSide::Old,
                                    "document",
                                    &image_cpu.old,
                                );
                                trace.record_image_cpu(
                                    crate::flow_trace::TraceSide::New,
                                    "document",
                                    &image_cpu.new,
                                );
                                let render_pages =
                                    render_cpu.old.pages.saturating_add(render_cpu.new.pages);
                                let rendered_pages = render_cpu
                                    .old
                                    .rendered_pages
                                    .saturating_add(render_cpu.new.rendered_pages);
                                let failed_pages = render_cpu
                                    .old
                                    .failed_pages
                                    .saturating_add(render_cpu.new.failed_pages);
                                let cpu_micros = render_cpu
                                    .old
                                    .used_micros
                                    .saturating_add(render_cpu.new.used_micros);
                                let max_page_micros = render_cpu
                                    .old
                                    .max_page_micros()
                                    .max(render_cpu.new.max_page_micros());
                                trace.complete(
                                    "channel_comparison",
                                    None,
                                    [
                                        ("channels", channel_count),
                                        ("render_pages", render_pages),
                                        ("render_rendered_pages", rendered_pages),
                                        ("render_failed_pages", failed_pages),
                                        (
                                            "render_cpu_micros",
                                            usize::try_from(cpu_micros).unwrap_or(usize::MAX),
                                        ),
                                        (
                                            "render_page_cpu_max_micros",
                                            usize::try_from(max_page_micros).unwrap_or(usize::MAX),
                                        ),
                                    ],
                                    channel_start.elapsed(),
                                );
                                trace.complete(
                                    "report",
                                    None,
                                    [("json", usize::from(cli.json.is_some()))],
                                    std::time::Duration::ZERO,
                                );
                                trace.finish(*status, *status == 3, true);
                            }
                            Err(error) => {
                                trace.fail("channel_comparison", None, error);
                                trace.finish(2, false, true);
                            }
                        }
                        crate::flow_trace::write_atomically(
                            cli.trace_json.as_deref().expect("trace exists"),
                            trace,
                        )?;
                    }
                    result.map(|(status, _render_cpu, _image_cpu)| status)
                }
            })();
            match result {
                Ok(status) => ExitCode::from(status),
                Err(error) => {
                    report_fatal_error(&mut stderr, &error.to_string());
                    ExitCode::from(ExitStatus::ExecutionError.code())
                }
            }
        }
    }
}
