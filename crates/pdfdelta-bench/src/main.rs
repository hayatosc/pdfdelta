use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use pdfdelta_bench::{
    cases::built_in_cases,
    evaluator::{EvaluationRecord, evaluate_case},
    renderers::RendererKind,
};
use pdfdelta_core::diff::ChangeKind;

#[derive(Debug, Parser)]
#[command(
    name = "pdfbench",
    version,
    about = "Verify pdfdelta acceptance cases across reproducible PDF renderers"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run all built-in acceptance cases across both PDF renderers.
    Verify,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let result = match cli.command {
        None | Some(Command::Verify) => verify(&mut stdout),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = writeln!(stderr, "{error}");
            let _ = stderr.flush();
            ExitCode::from(2)
        }
    }
}

fn verify<W: Write>(writer: &mut W) -> Result<u8, String> {
    let cases = built_in_cases().map_err(|error| error.to_string())?;
    let total = cases
        .len()
        .checked_mul(RendererKind::all().len())
        .ok_or_else(|| "benchmark matrix size overflowed".to_owned())?;
    let mut passed = 0_usize;
    let mut execution_error = false;

    for case in &cases {
        for renderer in RendererKind::all() {
            match evaluate_case(case, renderer) {
                Ok(record) => {
                    if record.passed {
                        passed += 1;
                    }
                    write_record(writer, &record)?;
                }
                Err(error) => {
                    execution_error = true;
                    writeln!(
                        writer,
                        "FAIL case={} renderer={} error={error}",
                        case.name(),
                        renderer.name()
                    )
                    .map_err(|write_error| {
                        format!("cannot write benchmark result: {write_error}")
                    })?;
                }
            }
        }
    }
    writeln!(writer, "{passed}/{total} passed")
        .map_err(|error| format!("cannot write benchmark summary: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush benchmark output: {error}"))?;

    Ok(if execution_error {
        2
    } else if passed == total {
        0
    } else {
        1
    })
}

fn write_record<W: Write>(writer: &mut W, record: &EvaluationRecord) -> Result<(), String> {
    let status = if record.passed { "PASS" } else { "FAIL" };
    writeln!(
        writer,
        "{status} case={} renderer={} expected={} actual={} coverage={:.3}/{:.3} detail={}",
        record.case_name,
        record.renderer.name(),
        record.expected.label(),
        actual_label(&record.actual_kinds),
        record.old_coverage,
        record.new_coverage,
        record.detail
    )
    .map_err(|error| format!("cannot write benchmark result: {error}"))
}

fn actual_label(kinds: &[ChangeKind]) -> String {
    match kinds {
        [] => "none".to_owned(),
        [ChangeKind::Replacement] => "replacement".to_owned(),
        [ChangeKind::Insertion] => "insertion".to_owned(),
        [ChangeKind::Deletion] => "deletion".to_owned(),
        [ChangeKind::Move] => "move".to_owned(),
        _ => format!("{}-changes", kinds.len()),
    }
}
