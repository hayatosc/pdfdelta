use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::{CommandFactory, Parser};

mod args;
mod compare;
mod fs;
mod inspect;
mod trace;

use crate::{
    args::{Cli, Command, CompareCommand},
    compare::{compare_documents, report_fatal_error},
    inspect::inspect_document,
};

fn main() -> ExitCode {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let cli = Cli::parse();
    match cli.command {
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
                ExitCode::from(2)
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
        None => match compare_documents(
            CompareCommand {
                old_path: cli.old.as_deref(),
                new_path: cli.new.as_deref(),
                trace_path: cli.trace_json.as_deref(),
                old_password_file: cli.old_password_file.as_deref(),
                new_password_file: cli.new_password_file.as_deref(),
                old_font_identities: &cli.old_font_identity,
                new_font_identities: &cli.new_font_identity,
                limit_scale: cli.limit_scale,
                options: args::ComparisonOptions {
                    json_path: cli.json.as_deref(),
                    output_path: cli.output.as_deref(),
                    strict: cli.strict,
                    quiet: cli.quiet,
                    color: cli.color,
                },
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
