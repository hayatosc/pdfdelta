use std::{io, process::ExitCode};

use clap::Parser;

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
                color: cli.color,
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
