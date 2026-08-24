use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use pdfdelta_bench::{
    cases::built_in_cases,
    evaluator::{EvaluationRecord, evaluate_case},
    renderers::RendererKind,
    revisions::{PairSet, run_revision_benchmark, summarize_reports, write_reports_json},
};
use pdfdelta_core::diff::ChangeKind;

#[derive(Debug, Parser)]
#[command(
    name = "pdfbench",
    version,
    about = "Verify pdfdelta acceptance cases and real-world revision benchmarks"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run all built-in acceptance cases across both PDF renderers.
    Verify,
    /// Evaluate the non-vendored real-world revision-pair benchmark corpus.
    Revisions {
        /// Manifest describing the public revision pairs.
        #[arg(long, default_value = "benchmark/realworld/manifest.tsv")]
        manifest: PathBuf,
        /// Directory holding downloads named <pair>-old.pdf and <pair>-new.pdf.
        #[arg(long)]
        cache_dir: PathBuf,
        /// Restrict the run to one corpus set (dev, holdout, or all).
        #[arg(long, default_value = "all")]
        set: String,
        /// Evaluate a single pair by its manifest id.
        #[arg(long)]
        pair: Option<String>,
        /// Scale the comparison pipeline budgets (n-gram token elements,
        /// alignment candidate visits, alignment DP cells, diff token and
        /// edit-distance limits) uniformly by this factor (>= 1); parser and
        /// extraction limits are untouched. When omitted, every pair uses
        /// the limit_scale_hint recorded in the manifest.
        #[arg(long)]
        limit_scale: Option<f64>,
        /// Verify downloads and checksums without running comparisons.
        #[arg(long)]
        checksums_only: bool,
        /// Write a machine-readable report of every pair to a new JSON file.
        #[arg(long)]
        json_output: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let result = match cli.command {
        None | Some(Command::Verify) => verify(&mut stdout),
        Some(Command::Revisions {
            manifest,
            cache_dir,
            set,
            pair,
            limit_scale,
            checksums_only,
            json_output,
        }) => revisions(
            &mut stdout,
            &manifest,
            &cache_dir,
            &set,
            pair.as_deref(),
            limit_scale,
            checksums_only,
            json_output.as_deref(),
        ),
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

#[allow(clippy::too_many_arguments)]
fn revisions<W: Write>(
    writer: &mut W,
    manifest: &Path,
    cache_dir: &Path,
    set: &str,
    pair: Option<&str>,
    limit_scale: Option<f64>,
    checksums_only: bool,
    json_output: Option<&Path>,
) -> Result<u8, String> {
    let set_filter = match set {
        "all" => None,
        "dev" => Some(PairSet::Dev),
        "holdout" => Some(PairSet::Holdout),
        other => {
            return Err(format!(
                "unknown --set {other:?}: expected dev, holdout, or all"
            ));
        }
    };
    let reports = run_revision_benchmark(
        manifest,
        cache_dir,
        set_filter,
        pair,
        limit_scale,
        !checksums_only,
    )
    .map_err(|error| error.to_string())?;

    for record in &reports {
        writeln!(writer, "{}", revision_report_line(record))
            .map_err(|error| format!("cannot write revision benchmark result: {error}"))?;
    }
    writeln!(writer, "{}", summarize_reports(&reports))
        .map_err(|error| format!("cannot write revision benchmark summary: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush revision benchmark output: {error}"))?;

    if let Some(path) = json_output {
        write_reports_json(path, &reports).map_err(|error| error.to_string())?;
    }

    Ok(if reports.iter().any(|record| !record.healthy()) {
        1
    } else {
        0
    })
}

fn revision_report_line(record: &pdfdelta_bench::revisions::PairRunReport) -> String {
    let mut line = format!(
        "{} pair={} set={} role={}",
        record.status.label(),
        record.pair_id,
        record.set,
        record.role
    );
    if !record.provenance_verified {
        line.push_str(" provenance=unverified");
    } else {
        line.push_str(" provenance=verified");
    }
    if !record.compared {
        line.push_str(" compared=false");
    } else {
        line.push_str(&format!(
            " extracted={} comparison={} coverage=old={},new={},comp={} unresolved={}",
            yes_no(record.extraction_complete),
            yes_no(record.comparison_complete),
            optional_ratio(record.coverage_old),
            optional_ratio(record.coverage_new),
            optional_ratio(record.coverage_comparison),
            record.unresolved_regions.unwrap_or_default()
        ));
        if let Some(quality) = &record.quality {
            line.push_str(&format!(
                " reported={} expected={} recall={} precision={} kind={} hunks/matched={} tinyFP={}",
                quality.reported_changes,
                quality.expected_changes,
                optional_ratio(quality.recall),
                optional_ratio(quality.precision),
                optional_ratio(quality.kind_accuracy),
                optional_ratio(quality.reported_hunks_per_matched_change),
                quality.unmatched_tiny_changes
            ));
        }
    }
    if record.runtime_ms > 0 || record.compared {
        line.push_str(&format!(" runtime_ms={}", record.runtime_ms));
    }
    if let Some(reason) = &record.resource_limit_failure {
        line.push_str(&format!(" limit_failure={reason:?}"));
    }
    if let Some(reason) = &record.quality_skipped_reason {
        line.push_str(&format!(" quality_skipped={reason:?}"));
    }
    if let Some(failure) = &record.failure {
        line.push_str(&format!(" failure={failure:?}"));
    }
    line
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "complete",
        Some(false) => "incomplete",
        None => "unknown",
    }
}

fn optional_ratio(ratio: Option<f64>) -> String {
    ratio.map_or_else(|| "unknown".to_owned(), |ratio| format!("{ratio:.3}"))
}

fn write_record<W: Write>(writer: &mut W, record: &EvaluationRecord) -> Result<(), String> {
    let status = if record.passed { "PASS" } else { "FAIL" };
    writeln!(
        writer,
        "{status} case={} renderer={} expected={} actual={} coverage={}/{} detail={}",
        record.case_name,
        record.renderer.name(),
        record.expected.label(),
        actual_label(&record.actual_kinds),
        coverage_label(record.old_coverage),
        coverage_label(record.new_coverage),
        record.detail
    )
    .map_err(|error| format!("cannot write benchmark result: {error}"))
}

fn coverage_label(ratio: Option<f64>) -> String {
    ratio.map_or_else(|| "unknown".to_owned(), |ratio| format!("{ratio:.3}"))
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
