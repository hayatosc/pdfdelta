use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand, ValueEnum};
use pdfdelta_bench::{
    candidate_eval::{CandidateEvalRecord, evaluate_candidate_generation, write_candidates_json},
    canonical::CanonicalRenderDocument,
    cases::built_in_cases,
    evaluator::{EvaluationRecord, evaluate_case},
    mutation::RenderPlan,
    renderers::{RenderLimits, RendererKind},
    revisions::{
        PairSet, normalize_output_destination, publish_new_file, run_revision_benchmark,
        summarize_reports, write_reports_json, write_summary_json,
    },
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
    /// Render one structured canonical YAML document to a new PDF file.
    Render {
        /// Canonical YAML document to render.
        input: PathBuf,
        /// PDF construction path used for the generated fixture.
        #[arg(long, value_enum, default_value_t = RendererChoice::LopdfTj)]
        renderer: RendererChoice,
        /// New PDF destination; existing paths are never replaced.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Evaluate candidate generation recall and visit pressure on every
    /// built-in case across both PDF renderers.
    Candidates {
        /// Comma-separated top-K values whose recall is reported, in the
        /// given order (each greater than zero, no duplicates).
        #[arg(long, default_value = "5,10")]
        top_k: String,
        /// Write a machine-readable report of every record to a new JSON file.
        #[arg(long)]
        json_output: Option<PathBuf>,
    },
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
        /// Write a compact machine-readable summary report of every pair to a new JSON file.
        #[arg(long)]
        summary_json_output: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RendererChoice {
    LopdfTj,
    ClassicXrefTj,
}

impl RendererChoice {
    const fn kind(self) -> RendererKind {
        match self {
            Self::LopdfTj => RendererKind::LopdfTj,
            Self::ClassicXrefTj => RendererKind::ClassicXrefTj,
        }
    }
}

const CANONICAL_RENDER_LINE_GAP: u16 = 30;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let result = match cli.command {
        None | Some(Command::Verify) => verify(&mut stdout),
        Some(Command::Render {
            input,
            renderer,
            output,
        }) => render_fixture(&mut stdout, &input, renderer, &output),
        Some(Command::Candidates { top_k, json_output }) => match parse_top_k(&top_k) {
            Ok(top_k) => candidates(&mut stdout, &top_k, json_output.as_deref()),
            Err(error) => Err(error),
        },
        Some(Command::Revisions {
            manifest,
            cache_dir,
            set,
            pair,
            limit_scale,
            checksums_only,
            json_output,
            summary_json_output,
        }) => revisions(
            &mut stdout,
            &manifest,
            &cache_dir,
            &set,
            pair.as_deref(),
            limit_scale,
            checksums_only,
            json_output.as_deref(),
            summary_json_output.as_deref(),
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

fn render_fixture<W: Write>(
    writer: &mut W,
    input: &Path,
    renderer: RendererChoice,
    output: &Path,
) -> Result<u8, String> {
    let yaml = fs::read_to_string(input)
        .map_err(|error| format!("cannot read canonical YAML {}: {error}", input.display()))?;
    let document = CanonicalRenderDocument::from_yaml(&yaml).map_err(|error| error.to_string())?;
    let plan = RenderPlan::new(vec![document.render_lines()], CANONICAL_RENDER_LINE_GAP)
        .map_err(|error| error.to_string())?;
    let renderer = renderer.kind();
    let pdf = renderer
        .render(&plan, RenderLimits::default())
        .map_err(|error| error.to_string())?;
    publish_new_file(output, &pdf).map_err(|error| error.to_string())?;

    writeln!(
        writer,
        "rendered {} with {} to {} ({} bytes)",
        input.display(),
        renderer.name(),
        output.display(),
        pdf.len()
    )
    .map_err(|error| format!("cannot write render summary: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush render summary: {error}"))?;
    Ok(0)
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

fn parse_top_k(raw: &str) -> Result<Vec<usize>, String> {
    if raw.trim().is_empty() {
        return Err("--top-k must list at least one K value".to_owned());
    }
    let mut values = Vec::new();
    for token in raw.split(',') {
        let k = token
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("--top-k value {token:?} is not a positive integer"))?;
        if k == 0 {
            return Err("--top-k values must be greater than zero".to_owned());
        }
        if values.contains(&k) {
            return Err(format!("--top-k value {k} is listed more than once"));
        }
        values.push(k);
    }
    Ok(values)
}

fn candidates<W: Write>(
    writer: &mut W,
    top_k: &[usize],
    json_output: Option<&Path>,
) -> Result<u8, String> {
    let cases = built_in_cases().map_err(|error| error.to_string())?;
    let total = cases
        .len()
        .checked_mul(RendererKind::all().len())
        .ok_or_else(|| "candidate evaluation matrix size overflowed".to_owned())?;
    let mut records = Vec::new();
    let mut execution_error = false;

    for case in &cases {
        for renderer in RendererKind::all() {
            match evaluate_candidate_generation(case, renderer, top_k) {
                Ok(record) => {
                    writeln!(writer, "{}", candidate_report_line(&record)).map_err(|error| {
                        format!("cannot write candidate evaluation result: {error}")
                    })?;
                    records.push(record);
                }
                Err(error) => {
                    execution_error = true;
                    writeln!(
                        writer,
                        "FAIL case={} renderer={} error={error}",
                        case.name(),
                        renderer.name()
                    )
                    .map_err(|error| {
                        format!("cannot write candidate evaluation result: {error}")
                    })?;
                }
            }
        }
    }
    writeln!(writer, "{}", summarize_candidates(&records, total))
        .map_err(|error| format!("cannot write candidate evaluation summary: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush candidate evaluation output: {error}"))?;

    if let Some(path) = json_output {
        if execution_error {
            return Err(format!(
                "candidate evaluation had execution errors; JSON artifact {} withheld",
                path.display()
            ));
        }
        write_candidates_json(path, &records).map_err(|error| error.to_string())?;
    }

    Ok(if execution_error {
        2
    } else if records.iter().any(|record| !record.healthy()) {
        1
    } else {
        0
    })
}

fn candidate_report_line(record: &CandidateEvalRecord) -> String {
    let top_k = record
        .top_k
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let recall = if record.top_k.len() == record.recall_at_k.len()
        && record.recall_at_k.len() == record.oracle_recall_at_k.len()
    {
        record
            .recall_at_k
            .iter()
            .zip(&record.oracle_recall_at_k)
            .map(|(inverted, oracle)| format!("{inverted:.3}/{oracle:.3}"))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "mismatch".to_owned()
    };
    let minhash_recall = if record.top_k.len() == record.minhash_recall_at_k.len()
        && record.minhash_recall_at_k.len() == record.oracle_recall_at_k.len()
    {
        record
            .minhash_recall_at_k
            .iter()
            .zip(&record.oracle_recall_at_k)
            .map(|(minhash, oracle)| format!("{minhash:.3}/{oracle:.3}"))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "mismatch".to_owned()
    };
    format!(
        "OK case={} renderer={} top_k={} recall={} minhash_recall={} candidates={}/{}/{} minhash_candidates={}/{}/{} inverted_index_build_latency_ns={} inverted_full_query_latency_ns={} minhash_index_build_latency_ns={} minhash_full_query_latency_ns={} oracle_index_build_latency_ns={} oracle_full_query_latency_ns={} visits={}/{}/{}/{}/{}/{} minhash_visits={}/{}/{}/{}/{}/{} ngram={}/{}/{} shared={} top10={} n50={} n90={} df_p50={} df_p95={} df_max={}",
        record.case_name,
        record.renderer.name(),
        top_k,
        recall,
        minhash_recall,
        record.candidate_count_p50,
        record.candidate_count_p95,
        record.candidate_count_max,
        record.minhash_candidate_count_p50,
        record.minhash_candidate_count_p95,
        record.minhash_candidate_count_max,
        record.index_build_latency_ns,
        record.query_latency_ns,
        record.minhash_index_build_latency_ns,
        record.minhash_query_latency_ns,
        record.oracle_index_build_latency_ns,
        record.oracle_query_latency_ns,
        record.estimated_visits_p50,
        record.estimated_visits_p95,
        record.estimated_visits_max,
        record.estimated_visits_upper_bound_total,
        record.max_candidate_visits,
        record.estimated_visits_upper_bound_exceeds_limit,
        record.minhash_estimated_visits_p50,
        record.minhash_estimated_visits_p95,
        record.minhash_estimated_visits_max,
        record.minhash_estimated_visits_upper_bound_total,
        record.max_candidate_visits,
        record.minhash_estimated_visits_upper_bound_exceeds_limit,
        record.ngram_posting_visits_total,
        record.dominant_ngram_visits,
        record.dominant_ngram_df,
        record.shared_ngram_count,
        record.top_10_ngram_visits,
        record.ngrams_for_50_percent_visits,
        record.ngrams_for_90_percent_visits,
        record.shared_ngram_df_p50,
        record.shared_ngram_df_p95,
        record.shared_ngram_df_max,
    )
}

fn summarize_candidates(records: &[CandidateEvalRecord], total: usize) -> String {
    let unhealthy = records.iter().filter(|record| !record.healthy()).count();
    let mut summary = format!("{}/{} candidate evaluations OK", records.len(), total);
    if unhealthy > 0 {
        summary.push_str(&format!("; {unhealthy} below exhaustive oracle recall"));
    }
    summary
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
    summary_json_output: Option<&Path>,
) -> Result<u8, String> {
    if let (Some(full_path), Some(summary_path)) = (json_output, summary_json_output) {
        let norm_full = normalize_output_destination(full_path).map_err(|e| e.to_string())?;
        let norm_summary = normalize_output_destination(summary_path).map_err(|e| e.to_string())?;
        if norm_full == norm_summary {
            return Err(format!(
                "--json-output and --summary-json-output must specify distinct paths; got conflicting destination {}",
                full_path.display()
            ));
        }
    }
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

    if let Some(path) = summary_json_output {
        write_summary_json(path, &reports).map_err(|error| error.to_string())?;
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
    match (
        record.candidate_visits,
        record.candidate_visits_required,
        record.max_candidate_visits,
    ) {
        (Some(visits), Some(required), Some(limit)) => {
            line.push_str(&format!(
                " candidate_visits={visits}/{limit} required_candidate_visits={required}"
            ));
            match (
                record.candidate_visits_required_exact,
                record.candidate_visits_required_ngram,
                record.candidate_visits_required_short_fallback,
            ) {
                (Some(exact), Some(ngram), Some(short_fallback)) => {
                    line.push_str(&format!(
                        " required_candidate_components=exact:{exact},ngram:{ngram},short_fallback:{short_fallback}"
                    ));
                }
                (None, None, None) => {}
                _ => line.push_str(" required_candidate_components=incomplete"),
            }
        }
        (Some(visits), None, Some(limit)) => {
            line.push_str(&format!(
                " candidate_visits={visits}/{limit} required_candidate_visits=unavailable"
            ));
        }
        (None, None, None) => {}
        _ => line.push_str(" candidate_visits=incomplete"),
    }
    if let Some(pressure) = &record.candidate_visit_pressure {
        line.push_str(&format!(
            " visit_pressure=p50:{},p95:{},max:{},upper:{},limit:{},upper_exceeds:{} ngram_pressure=total:{},dominant:{},df:{},shared:{},top10:{},n50:{},n90:{},df_p50:{},df_p95:{},df_max:{}",
            pressure.estimated_visits_p50,
            pressure.estimated_visits_p95,
            pressure.estimated_visits_max,
            pressure.estimated_visits_upper_bound_total,
            pressure.max_candidate_visits,
            pressure.estimated_visits_upper_bound_exceeds_limit,
            pressure.ngram_posting_visits_total,
            pressure.dominant_ngram_visits,
            pressure.dominant_ngram_df,
            pressure.shared_ngram_count,
            pressure.top_10_ngram_visits,
            pressure.ngrams_for_50_percent_visits,
            pressure.ngrams_for_90_percent_visits,
            pressure.shared_ngram_df_p50,
            pressure.shared_ngram_df_p95,
            pressure.shared_ngram_df_max,
        ));
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

#[cfg(test)]
mod tests {
    use pdfdelta_bench::{
        candidate_eval::{CandidateEvalRecord, CandidateVisitPressure},
        revisions::{PairRole, PairRunReport, PairRunStatus, PairSet},
    };

    use super::*;

    fn sample_pair_report() -> PairRunReport {
        PairRunReport {
            pair_id: "p".to_owned(),
            set: PairSet::Dev.label(),
            role: PairRole::Standard.label(),
            document_type: "t".to_owned(),
            in_scope: true,
            status: PairRunStatus::Ok,
            provenance_verified: true,
            compared: false,
            extraction_complete: None,
            comparison_complete: None,
            extraction_issues: Vec::new(),
            coverage_old: None,
            coverage_new: None,
            coverage_comparison: None,
            unresolved_regions: None,
            unresolved_old_token_share: None,
            unresolved_new_token_share: None,
            reported_content_changes: None,
            formatting_only_changes: None,
            uncertain_changes: None,
            reported_changes_preview: Vec::new(),
            quality: None,
            quality_skipped_reason: None,
            resource_limit_failure: None,
            candidate_visits: None,
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            max_candidate_visits: None,
            candidate_visit_pressure: None,
            runtime_ms: 0,
            limit_scale_used: 1.0,
            failure: None,
        }
    }

    fn sample_record(recall_at_k: Vec<f64>, oracle_recall_at_k: Vec<f64>) -> CandidateEvalRecord {
        CandidateEvalRecord {
            case_name: "case".to_owned(),
            renderer: RendererKind::LopdfTj,
            top_k: vec![5, 10],
            old_blocks: 3,
            new_blocks: 3,
            counterpart_old_blocks: 3,
            unmatched_old_blocks: 0,
            recall_at_k: recall_at_k.clone(),
            minhash_recall_at_k: recall_at_k,
            oracle_recall_at_k,
            candidate_count_p50: 1,
            candidate_count_p95: 1,
            candidate_count_max: 1,
            minhash_candidate_count_p50: 1,
            minhash_candidate_count_p95: 1,
            minhash_candidate_count_max: 1,
            oracle_candidate_count_p50: 3,
            oracle_candidate_count_p95: 3,
            oracle_candidate_count_max: 3,
            index_build_latency_ns: 11,
            query_latency_ns: 12,
            minhash_index_build_latency_ns: 13,
            minhash_query_latency_ns: 14,
            oracle_index_build_latency_ns: 15,
            oracle_query_latency_ns: 16,
            estimated_visits_p50: 1,
            estimated_visits_p95: 1,
            estimated_visits_max: 1,
            estimated_visits_upper_bound_total: 3,
            max_candidate_visits: 1_000_000,
            estimated_visits_upper_bound_exceeds_limit: false,
            minhash_estimated_visits_p50: 1,
            minhash_estimated_visits_p95: 1,
            minhash_estimated_visits_max: 1,
            minhash_estimated_visits_upper_bound_total: 3,
            minhash_estimated_visits_upper_bound_exceeds_limit: false,
            ngram_posting_visits_total: 3,
            dominant_ngram_visits: 1,
            dominant_ngram_df: 1,
            shared_ngram_count: 3,
            top_10_ngram_visits: 3,
            ngrams_for_50_percent_visits: 2,
            ngrams_for_90_percent_visits: 3,
            shared_ngram_df_p50: 1,
            shared_ngram_df_p95: 1,
            shared_ngram_df_max: 1,
        }
    }

    #[test]
    fn parse_top_k_accepts_comma_separated_values_in_order() {
        assert_eq!(parse_top_k("5,10"), Ok(vec![5, 10]));
        assert_eq!(parse_top_k(" 5 , 10 "), Ok(vec![5, 10]));
        assert_eq!(parse_top_k("10,5"), Ok(vec![10, 5]));
    }

    #[test]
    fn parse_top_k_rejects_empty_zero_duplicate_and_non_numeric_values() {
        assert!(parse_top_k("").is_err());
        assert!(parse_top_k("0").is_err());
        assert!(parse_top_k("5,0").is_err());
        assert!(parse_top_k("5,,10").is_err());
        assert!(parse_top_k("five").is_err());
        assert!(parse_top_k("5,5").is_err());
    }

    #[test]
    fn candidate_report_line_marks_recall_mismatch_instead_of_truncating() {
        let aligned = sample_record(vec![1.0, 1.0], vec![1.0, 1.0]);
        assert!(candidate_report_line(&aligned).contains("recall=1.000/1.000,1.000/1.000"));

        let mismatched = sample_record(vec![1.0], vec![1.0, 1.0]);
        assert!(candidate_report_line(&mismatched).contains("recall=mismatch"));
    }

    #[test]
    fn candidate_report_line_displays_generator_latency_observations() {
        let line = candidate_report_line(&sample_record(vec![1.0, 1.0], vec![1.0, 1.0]));
        assert!(line.contains("inverted_index_build_latency_ns=11"));
        assert!(line.contains("inverted_full_query_latency_ns=12"));
        assert!(line.contains("minhash_index_build_latency_ns=13"));
        assert!(line.contains("minhash_full_query_latency_ns=14"));
        assert!(line.contains("oracle_index_build_latency_ns=15"));
        assert!(line.contains("oracle_full_query_latency_ns=16"));
    }

    #[test]
    fn revision_report_line_displays_candidate_visits_contract() {
        let mut both = sample_pair_report();
        both.candidate_visits = Some(42);
        both.candidate_visits_required = Some(84);
        both.candidate_visits_required_exact = Some(20);
        both.candidate_visits_required_ngram = Some(40);
        both.candidate_visits_required_short_fallback = Some(24);
        both.max_candidate_visits = Some(1_000_000);
        let line = revision_report_line(&both);
        assert!(line.contains("candidate_visits=42/1000000"));
        assert!(line.contains("required_candidate_visits=84"));
        assert!(line.contains("required_candidate_components=exact:20,ngram:40,short_fallback:24"));

        let mut generic = sample_pair_report();
        generic.candidate_visits = Some(42);
        generic.candidate_visits_required = Some(84);
        generic.max_candidate_visits = Some(1_000_000);
        let line = revision_report_line(&generic);
        assert!(line.contains("required_candidate_visits=84"));
        assert!(!line.contains("required_candidate_components"));

        let mut unavailable = sample_pair_report();
        unavailable.candidate_visits = Some(42);
        unavailable.max_candidate_visits = Some(1_000_000);
        let line = revision_report_line(&unavailable);
        assert!(line.contains("candidate_visits=42/1000000"));
        assert!(line.contains("required_candidate_visits=unavailable"));
        assert!(!line.contains("incomplete"));

        let none = sample_pair_report();
        assert!(!revision_report_line(&none).contains("candidate_visits"));

        let mut partial = sample_pair_report();
        partial.candidate_visits = Some(42);
        assert!(revision_report_line(&partial).contains("candidate_visits=incomplete"));

        let mut partial_components = sample_pair_report();
        partial_components.candidate_visits = Some(42);
        partial_components.candidate_visits_required = Some(84);
        partial_components.candidate_visits_required_exact = Some(20);
        partial_components.max_candidate_visits = Some(1_000_000);
        assert!(
            revision_report_line(&partial_components)
                .contains("required_candidate_components=incomplete")
        );
    }

    #[test]
    fn revision_report_line_displays_pressure_when_present() {
        let mut with_pressure = sample_pair_report();
        with_pressure.candidate_visit_pressure = Some(CandidateVisitPressure {
            estimated_visits_p50: 1,
            estimated_visits_p95: 2,
            estimated_visits_max: 3,
            estimated_visits_upper_bound_total: 9,
            max_candidate_visits: 8,
            estimated_visits_upper_bound_exceeds_limit: true,
            ngram_posting_visits_total: 9,
            dominant_ngram_visits: 9,
            dominant_ngram_df: 3,
            shared_ngram_count: 1,
            top_10_ngram_visits: 9,
            ngrams_for_50_percent_visits: 1,
            ngrams_for_90_percent_visits: 1,
            shared_ngram_df_p50: 3,
            shared_ngram_df_p95: 3,
            shared_ngram_df_max: 3,
        });
        let line = revision_report_line(&with_pressure);
        assert!(
            line.contains("visit_pressure=p50:1,p95:2,max:3,upper:9,limit:8,upper_exceeds:true")
        );
        assert!(
            line.contains(
                "ngram_pressure=total:9,dominant:9,df:3,shared:1,top10:9,n50:1,n90:1,df_p50:3,df_p95:3,df_max:3"
            )
        );

        let without_pressure = sample_pair_report();
        assert!(!revision_report_line(&without_pressure).contains("visit_pressure="));
    }

    #[test]
    fn revisions_rejects_identical_and_aliased_json_and_summary_output_paths() {
        let mut temp_dir = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-cli-norm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        temp_dir.push(&unique_id);
        std::fs::create_dir_all(temp_dir.join("sub")).expect("create test dir");

        let mut writer = Vec::new();

        // 1. Identical path
        let path = temp_dir.join("sub").join("report.json");
        let error = revisions(
            &mut writer,
            Path::new("manifest.tsv"),
            Path::new("cache"),
            "all",
            None,
            None,
            true,
            Some(&path),
            Some(&path),
        )
        .expect_err("must reject identical paths");
        assert!(
            error.contains("--json-output and --summary-json-output must specify distinct paths")
        );

        // 2. Relative alias (sub/report.json vs sub/./report.json)
        let path_alias = temp_dir.join("sub").join(".").join("report.json");
        let error = revisions(
            &mut writer,
            Path::new("manifest.tsv"),
            Path::new("cache"),
            "all",
            None,
            None,
            true,
            Some(&path),
            Some(&path_alias),
        )
        .expect_err("must reject aliased paths");
        assert!(
            error.contains("--json-output and --summary-json-output must specify distinct paths")
        );

        // 3. Symlink parent alias
        #[cfg(unix)]
        {
            let symlink_sub = temp_dir.join("symlink_sub");
            if std::os::unix::fs::symlink(temp_dir.join("sub"), &symlink_sub).is_ok() {
                let path_symlink = symlink_sub.join("report.json");
                let error = revisions(
                    &mut writer,
                    Path::new("manifest.tsv"),
                    Path::new("cache"),
                    "all",
                    None,
                    None,
                    true,
                    Some(&path),
                    Some(&path_symlink),
                )
                .expect_err("must reject symlink parent aliased paths");
                assert!(error.contains(
                    "--json-output and --summary-json-output must specify distinct paths"
                ));
            }
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
