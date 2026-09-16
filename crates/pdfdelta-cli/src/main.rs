use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::{CommandFactory, Parser};

mod args;
mod compare;
mod evidence_compare;
mod evidence_text;
mod extraction_cache;
mod fs;
mod image_hashes;
mod inspect;
mod native_worker;
mod render;
mod review;
mod trace;
mod widgets;

use crate::{
    args::{Cli, Command, CompareCommand},
    compare::{ExitStatus, compare_documents, report_fatal_error},
    inspect::inspect_document,
};

fn main() -> ExitCode {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::HashImages) => match image_hashes::worker() {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => ExitCode::from(code),
        },
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
        }) => match render::worker(page, pages, width, height, object_number, generation) {
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
                    output_path: cli.output.as_deref(),
                    strict: cli.strict,
                    quiet: cli.quiet,
                    color: cli.color,
                },
            };
            let result = if cli.native_text_only {
                compare_documents(command, &mut stderr)
            } else {
                compare::compare_documents_with_evidence(
                    command,
                    &evidence_compare::EvidenceOptions {
                        channels: cli.channels.iter().copied().map(Into::into).collect(),
                    },
                    &mut stderr,
                )
            };
            match result {
                Ok(status) => ExitCode::from(status),
                Err(error) => {
                    report_fatal_error(&mut stderr, &error);
                    ExitCode::from(ExitStatus::ExecutionError.code())
                }
            }
        }
    }
}
