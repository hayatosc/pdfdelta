use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use clap::{Parser, Subcommand, ValueEnum};
use pdfdelta_bench::{
    agent_review::audit_bundle_with_baseline,
    canonical::{CanonicalRenderDocument, MAX_CANONICAL_YAML_BYTES},
    cases::built_in_cases,
    evaluator::{
        EvaluationRecord, GeneratedPrecisionMetrics, evaluate, evaluate_case, evaluate_rendered,
    },
    extraction_conformance::{
        MAX_EXTRACTION_ORACLE_BYTES, evaluate_extraction_conformance,
        evaluate_extraction_conformance_with_mismatch_svg,
    },
    mutation::{Mutation, MutationPlan, RenderPlan},
    publish_new_file,
    renderers::{RenderLimits, RendererKind},
};
use pdfdelta_core::{pdf::ParseLimits, span::ChangeKind};

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
    /// Verify the token-stream flow engine against fixtures with known edits.
    FlowVerify,
    /// Build exact ground truth on real PDFs by page-tree surgery.
    ///
    /// Compares each document with structurally edited copies (page deletion,
    /// insertion, swap, and duplication) and reports token-multiset precision
    /// and recall against the constructed truth as a TSV.
    FlowMetamorphic {
        /// Cache directory scanned for `*-old.pdf` and `*-new.pdf`.
        #[arg(long, default_value = "benchmark/realworld/cache", value_name = "DIR")]
        cache: PathBuf,
        /// Explicit PDF files to test instead of scanning the cache directory.
        #[arg(value_name = "PDF")]
        files: Vec<PathBuf>,
        /// Mark a case taking longer than this as a timeout failure.
        #[arg(
            long,
            default_value_t = pdfdelta_bench::flow_metamorphic::DEFAULT_CASE_TIMEOUT_SECONDS
        )]
        case_timeout_seconds: u64,
        /// Print the deterministic plan without running any case.
        #[arg(long)]
        plan: bool,
        /// Add up to this many single-token text-edit cases per document.
        #[arg(long, default_value_t = 0)]
        text_edits: usize,
        /// Add up to this many page-merge cases per document.
        #[arg(long, default_value_t = 0)]
        merges: usize,
        /// Uniform extraction-limit scale; large documents may need > 1.
        #[arg(long, default_value_t = 1.0)]
        limit_scale: f64,
        /// Write every constructed old/new PDF pair here for reproduction.
        #[arg(long, value_name = "DIR")]
        dump_edited: Option<PathBuf>,
    },
    /// Audit an exported agent review bundle and measure what it costs to read.
    ///
    /// Runs no model and calls no external service; the figures are bytes.
    AuditAgentReview {
        /// Bundle directory written by `pdfdelta --agent-review`.
        #[arg(long)]
        bundle: PathBuf,
        /// The engine's own JSON report for the same comparison, as a baseline.
        #[arg(long)]
        report: Option<PathBuf>,
        /// A host's own usage record, carried through unchanged.
        #[arg(long)]
        host_usage: Option<PathBuf>,
        /// Directory to write the measured baseline text into, for an external
        /// tokenizer to count.
        #[arg(long)]
        baseline_text: Option<PathBuf>,
        /// Write the audit to a new file instead of standard output.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Score a multi-channel CLI report against a hash-bound graph annotation.
    EvaluateDocument {
        /// Versioned annotation with input hashes and pair provenance.
        #[arg(long)]
        annotation: PathBuf,
        /// JSON produced by the multi-channel pdfdelta compare command.
        #[arg(long)]
        report: PathBuf,
    },
    /// Summarize shared-route search, coverage, and inference without annotation scoring.
    SummarizeDocument {
        #[arg(long)]
        report: PathBuf,
    },
    /// Attribute retained native/shared report diagnostics to comparison stages.
    DiagnoseReport {
        #[arg(long)]
        report: PathBuf,
    },
    /// Generate an English/Japanese prose or table matrix with local producers.
    GenerateDocumentMatrix {
        #[arg(long, value_enum, default_value = "prose")]
        kind: pdfdelta_bench::generalization_matrix::MatrixKind,
        #[arg(long)]
        typst: PathBuf,
        #[arg(long)]
        tectonic: PathBuf,
        #[arg(long)]
        tectonic_cache: PathBuf,
        #[arg(long)]
        font_path: PathBuf,
        #[arg(long)]
        first_evaluated: String,
        /// New directory for sources, PDFs, logs, and hash-bound annotations.
        #[arg(long)]
        output: PathBuf,
    },
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
    /// Apply one supported mutation to canonical YAML and evaluate the generated PDF pair.
    EvaluateYaml {
        /// Canonical YAML document used as the old revision.
        input: PathBuf,
        /// PDF construction path used for both generated revisions.
        #[arg(long, value_enum, default_value_t = RendererChoice::LopdfTj)]
        renderer: RendererChoice,
        #[command(subcommand)]
        mutation: YamlMutation,
    },
    /// Evaluate externally rendered revisions against one canonical YAML mutation.
    EvaluateRenderedYaml {
        /// Canonical YAML document used as the old revision.
        input: PathBuf,
        /// Externally rendered PDF for the unmodified canonical document.
        #[arg(long)]
        old_pdf: PathBuf,
        /// Externally rendered PDF after applying the selected mutation.
        #[arg(long)]
        new_pdf: PathBuf,
        /// Auditable renderer name and version shown in the result.
        #[arg(long)]
        renderer: String,
        #[command(subcommand)]
        mutation: YamlMutation,
    },
    /// Compare default glyph extraction with a position-aware external snapshot.
    ExtractionConformance {
        /// PDF consumed by both the external producer and pdfdelta.
        input: PathBuf,
        /// Versioned external extraction snapshot in JSON format.
        #[arg(long)]
        oracle: PathBuf,
        /// Maximum absolute difference accepted for each geometry coordinate.
        #[arg(long, default_value_t = 0.25)]
        geometry_tolerance: f64,
        /// Write a visual expected/actual overlay for a mismatch to a new SVG file.
        #[arg(long)]
        mismatch_svg: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum YamlMutation {
    /// Split one paragraph between words without changing its text.
    LineWrap {
        #[arg(long)]
        paragraph_id: String,
        #[arg(long)]
        after_word: usize,
    },
    /// Start a new page immediately before one source paragraph.
    PageBreak {
        #[arg(long)]
        before_paragraph_id: String,
    },
    /// Change the line gap for every line in the new revision.
    LineHeightChange {
        #[arg(long)]
        new_line_gap: u16,
    },
    /// Change the left margin for every line in the new revision.
    MarginChange {
        #[arg(long)]
        new_margin: u16,
    },
    /// Change the font size for every line in the new revision.
    FontSizeChange {
        #[arg(long)]
        new_font_size: u16,
    },
    /// Change the page dimensions for the new revision.
    PageSizeChange {
        #[arg(long)]
        new_page_width: u16,
        #[arg(long)]
        new_page_height: u16,
    },
    /// Reflow one section's paragraphs into two columns below full-width metadata.
    ColumnChange {
        #[arg(long)]
        section_id: String,
    },
    /// Replace all text in one paragraph.
    TextReplace {
        #[arg(long)]
        paragraph_id: String,
        #[arg(long)]
        new_text: String,
    },
    /// Insert text at a Unicode scalar offset in one paragraph.
    TextInsert {
        #[arg(long)]
        paragraph_id: String,
        #[arg(long)]
        at: usize,
        #[arg(long)]
        text: String,
    },
    /// Delete a half-open Unicode scalar range from one paragraph.
    TextDelete {
        #[arg(long)]
        paragraph_id: String,
        #[arg(long)]
        start: usize,
        #[arg(long)]
        end: usize,
    },
    /// Replace the first ASCII decimal run in one paragraph.
    NumberReplace {
        #[arg(long)]
        paragraph_id: String,
        #[arg(long)]
        new_number: String,
    },
    /// Insert one paragraph at a zero-based index within a section.
    ParagraphInsert {
        #[arg(long)]
        section_id: String,
        #[arg(long)]
        index: usize,
        #[arg(long)]
        paragraph_id: String,
        #[arg(long)]
        text: String,
    },
    /// Delete a paragraph while retaining at least one paragraph in its section.
    ParagraphDelete {
        #[arg(long)]
        paragraph_id: String,
    },
    /// Move one paragraph to a final zero-based index within a section.
    ParagraphMove {
        #[arg(long)]
        paragraph_id: String,
        /// Move into this section instead of reordering within the source section.
        #[arg(long)]
        to_section_id: Option<String>,
        #[arg(long)]
        to_index: usize,
    },
}

impl YamlMutation {
    fn into_mutation(self) -> (&'static str, Mutation) {
        match self {
            Self::LineWrap {
                paragraph_id,
                after_word,
            } => (
                "yaml-line-wrap",
                Mutation::LineWrap {
                    paragraph_id,
                    after_word,
                },
            ),
            Self::PageBreak {
                before_paragraph_id,
            } => (
                "yaml-page-break",
                Mutation::PageBreakBefore {
                    paragraph_id: before_paragraph_id,
                },
            ),
            Self::LineHeightChange { new_line_gap } => (
                "yaml-line-height-change",
                Mutation::LineHeightChange { new_line_gap },
            ),
            Self::MarginChange { new_margin } => {
                ("yaml-margin-change", Mutation::MarginChange { new_margin })
            }
            Self::FontSizeChange { new_font_size } => (
                "yaml-font-size-change",
                Mutation::FontSizeChange { new_font_size },
            ),
            Self::PageSizeChange {
                new_page_width,
                new_page_height,
            } => (
                "yaml-page-size-change",
                Mutation::PageSizeChange {
                    new_page_width,
                    new_page_height,
                },
            ),
            Self::ColumnChange { section_id } => (
                "yaml-column-change",
                Mutation::ColumnChangeInSection { section_id },
            ),
            Self::TextReplace {
                paragraph_id,
                new_text,
            } => (
                "yaml-text-replace",
                Mutation::TextReplace {
                    paragraph_id,
                    new_text,
                },
            ),
            Self::TextInsert {
                paragraph_id,
                at,
                text,
            } => (
                "yaml-text-insert",
                Mutation::TextInsert {
                    paragraph_id,
                    at,
                    text,
                },
            ),
            Self::TextDelete {
                paragraph_id,
                start,
                end,
            } => (
                "yaml-text-delete",
                Mutation::TextDelete {
                    paragraph_id,
                    start,
                    end,
                },
            ),
            Self::NumberReplace {
                paragraph_id,
                new_number,
            } => (
                "yaml-number-replace",
                Mutation::NumberReplace {
                    paragraph_id,
                    new_number,
                },
            ),
            Self::ParagraphInsert {
                section_id,
                index,
                paragraph_id,
                text,
            } => (
                "yaml-paragraph-insert",
                Mutation::ParagraphInsertInSection {
                    section_id,
                    index,
                    paragraph_id,
                    text,
                },
            ),
            Self::ParagraphDelete { paragraph_id } => (
                "yaml-paragraph-delete",
                Mutation::ParagraphDelete { paragraph_id },
            ),
            Self::ParagraphMove {
                paragraph_id,
                to_section_id,
                to_index,
            } => {
                let mutation = match to_section_id {
                    Some(to_section_id) => Mutation::ParagraphMoveToSection {
                        paragraph_id,
                        to_section_id,
                        to_index,
                    },
                    None => Mutation::ParagraphMoveInSection {
                        paragraph_id,
                        to_index,
                    },
                };
                ("yaml-paragraph-move", mutation)
            }
        }
    }
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
        Some(Command::FlowVerify) => (|| {
            let summary = pdfdelta_bench::flow_verify::verify_flow_fixtures()
                .map_err(|error| error.to_string())?;
            for record in &summary.records {
                writeln!(
                    stdout,
                    "{}",
                    pdfdelta_bench::flow_verify::format_record(record)
                )
                .map_err(|error| format!("cannot write flow verification: {error}"))?;
            }
            writeln!(
                stdout,
                "flow verification: {}/{} passed",
                summary.passed,
                summary.total()
            )
            .map_err(|error| format!("cannot write flow verification summary: {error}"))?;
            Ok(u8::from(!summary.all_passed()))
        })(),
        Some(Command::FlowMetamorphic {
            cache,
            files,
            case_timeout_seconds,
            plan,
            text_edits,
            merges,
            limit_scale,
            dump_edited,
        }) => (|| {
            let options = pdfdelta_bench::flow_metamorphic::MetamorphicOptions {
                case_timeout: Duration::from_secs(case_timeout_seconds),
                dump_dir: dump_edited,
                text_edits,
                merges,
                limit_scale,
            };
            if plan {
                let planned =
                    pdfdelta_bench::flow_metamorphic::plan_documents(&cache, &files, &options)
                        .map_err(|error| error.to_string())?;
                writeln!(stdout, "file\tpages\tplan\tcases")
                    .map_err(|error| format!("cannot write plan: {error}"))?;
                for document in &planned {
                    writeln!(
                        stdout,
                        "{}\t{}\t{}\t{}",
                        document.file,
                        document.pages,
                        if document.full_plan {
                            "full"
                        } else {
                            "reduced"
                        },
                        document.cases
                    )
                    .map_err(|error| format!("cannot write plan: {error}"))?;
                }
                writeln!(
                    stdout,
                    "TOTAL\t-\t-\t{}",
                    planned.iter().map(|document| document.cases).sum::<usize>()
                )
                .map_err(|error| format!("cannot write plan: {error}"))?;
                return Ok(0);
            }
            let summary = pdfdelta_bench::flow_metamorphic::run(&cache, &files, &options)
                .map_err(|error| error.to_string())?;
            pdfdelta_bench::flow_metamorphic::write_tsv(&summary, &mut stdout)
                .map_err(|error| format!("cannot write flow metamorphic TSV: {error}"))?;
            eprintln!(
                "flow-metamorphic: {}/{} exact, micro precision {:.4}, recall {:.4}, {} skipped, {:.1}s",
                summary.exact(),
                summary.total(),
                summary.micro_precision(),
                summary.micro_recall(),
                summary.skipped,
                summary.elapsed.as_secs_f64(),
            );
            for (reason, count) in &summary.skips {
                eprintln!("flow-metamorphic skip: {reason}: {count}");
            }
            Ok(0)
        })(),
        Some(Command::AuditAgentReview {
            bundle,
            report,
            host_usage,
            baseline_text,
            output,
        }) => (|| {
            let audit = audit_bundle_with_baseline(
                &bundle,
                report.as_deref(),
                host_usage.as_deref(),
                baseline_text.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            let mut encoded =
                serde_json::to_vec_pretty(&audit).map_err(|error| error.to_string())?;
            encoded.push(b'\n');
            match output {
                Some(path) => {
                    publish_new_file(&path, &encoded).map_err(|error| error.to_string())?;
                }
                None => stdout
                    .write_all(&encoded)
                    .map_err(|error| error.to_string())?,
            }
            // A violated invariant is a failure, so it can gate a change.
            Ok(u8::from(!audit.sound()))
        })(),
        Some(Command::EvaluateDocument { annotation, report }) => {
            evaluate_document(&mut stdout, &annotation, &report)
        }
        Some(Command::SummarizeDocument { report }) => (|| {
            use pdfdelta_bench::generalization_report::{
                MAX_DOCUMENT_REPORT_BYTES, summarize_document_report,
            };
            let bytes = read_bounded_file(&report, MAX_DOCUMENT_REPORT_BYTES, "document report")?;
            let summary = summarize_document_report(&bytes).map_err(|error| error.to_string())?;
            serde_json::to_writer_pretty(&mut stdout, &summary)
                .map_err(|error| error.to_string())?;
            writeln!(stdout).map_err(|error| error.to_string())?;
            Ok(0)
        })(),
        Some(Command::DiagnoseReport { report }) => (|| {
            use pdfdelta_bench::generalization_report::{
                MAX_DIAGNOSTIC_REPORT_BYTES, StageDiagnostics, diagnose_report,
            };
            let result =
                read_bounded_file(&report, MAX_DIAGNOSTIC_REPORT_BYTES, "diagnostic report")
                    .and_then(|bytes| diagnose_report(&bytes).map_err(|error| error.to_string()));
            let (diagnostics, exit_code) = match result {
                Ok(diagnostics) => (diagnostics, 0),
                Err(error) => (StageDiagnostics::reporting_failure(error), 2),
            };
            serde_json::to_writer_pretty(&mut stdout, &diagnostics)
                .map_err(|error| error.to_string())?;
            writeln!(stdout).map_err(|error| error.to_string())?;
            Ok(exit_code)
        })(),
        Some(Command::GenerateDocumentMatrix {
            kind,
            typst,
            tectonic,
            tectonic_cache,
            font_path,
            first_evaluated,
            output,
        }) => pdfdelta_bench::generalization_matrix::generate_matrix(
            kind,
            &output,
            &typst,
            &tectonic,
            &tectonic_cache,
            &font_path,
            &first_evaluated,
        )
        .map_err(|error| error.to_string())
        .and_then(|matrix| {
            writeln!(
                stdout,
                "Generated {} PDFs and {} annotated pairs in {}",
                matrix.documents.len(),
                matrix.pairs.len(),
                output.display()
            )
            .map_err(|error| error.to_string())?;
            Ok(0)
        }),
        Some(Command::Render {
            input,
            renderer,
            output,
        }) => render_fixture(&mut stdout, &input, renderer, &output),
        Some(Command::EvaluateYaml {
            input,
            renderer,
            mutation,
        }) => evaluate_yaml(&mut stdout, &input, renderer, mutation),
        Some(Command::EvaluateRenderedYaml {
            input,
            old_pdf,
            new_pdf,
            renderer,
            mutation,
        }) => evaluate_rendered_yaml(&mut stdout, &input, &old_pdf, &new_pdf, &renderer, mutation),
        Some(Command::ExtractionConformance {
            input,
            oracle,
            geometry_tolerance,
            mismatch_svg,
        }) => run_extraction_conformance(
            &mut stdout,
            &input,
            &oracle,
            geometry_tolerance,
            mismatch_svg.as_deref(),
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

fn evaluate_document<W: Write>(
    writer: &mut W,
    annotation: &Path,
    report: &Path,
) -> Result<u8, String> {
    use pdfdelta_bench::generalization_report::{
        MAX_DOCUMENT_ANNOTATION_BYTES, MAX_DOCUMENT_REPORT_BYTES, evaluate_document_report,
    };
    let annotation = read_bounded_file(
        annotation,
        MAX_DOCUMENT_ANNOTATION_BYTES,
        "document annotation",
    )?;
    let report = read_bounded_file(report, MAX_DOCUMENT_REPORT_BYTES, "document report")?;
    let result =
        evaluate_document_report(&annotation, &report).map_err(|error| error.to_string())?;
    let passed = result.score.exact();
    serde_json::to_writer_pretty(&mut *writer, &result).map_err(|error| error.to_string())?;
    writeln!(writer).map_err(|error| error.to_string())?;
    Ok(u8::from(!passed))
}

fn read_canonical_yaml_file(path: &Path) -> Result<String, String> {
    let bytes = read_bounded_file(path, MAX_CANONICAL_YAML_BYTES, "canonical YAML")?;
    String::from_utf8(bytes)
        .map_err(|error| format!("cannot read canonical YAML {}: {error}", path.display()))
}

fn read_bounded_file(path: &Path, max_bytes: usize, description: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot read {description} {}: {error}", path.display()))?;
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "cannot read {description} {}: {description} must not exceed {max_bytes} bytes",
            path.display()
        ));
    }
    let file = fs::File::open(path)
        .map_err(|error| format!("cannot read {description} {}: {error}", path.display()))?;
    let mut limited = file.take(max_bytes as u64 + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {description} {}: {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "cannot read {description} {}: {description} must not exceed {max_bytes} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

fn run_extraction_conformance<W: Write>(
    writer: &mut W,
    input: &Path,
    oracle: &Path,
    geometry_tolerance: f64,
    mismatch_svg_path: Option<&Path>,
) -> Result<u8, String> {
    let pdf = read_bounded_file(input, ParseLimits::default().max_input_bytes, "PDF")?;
    let oracle_json = read_bounded_file(
        oracle,
        MAX_EXTRACTION_ORACLE_BYTES,
        "extraction oracle JSON",
    )?;
    let pdf = Arc::from(pdf);
    let (record, mismatch_svg) = if mismatch_svg_path.is_some() {
        let diagnostic = evaluate_extraction_conformance_with_mismatch_svg(
            pdf,
            &oracle_json,
            geometry_tolerance,
        )
        .map_err(|error| error.to_string())?;
        (diagnostic.record, diagnostic.mismatch_svg)
    } else {
        let record = evaluate_extraction_conformance(pdf, &oracle_json, geometry_tolerance)
            .map_err(|error| error.to_string())?;
        (record, None)
    };
    if let (Some(path), Some(svg)) = (mismatch_svg_path, mismatch_svg.as_deref()) {
        publish_new_file(path, svg.as_bytes()).map_err(|error| error.to_string())?;
    }
    let status = if record.passed() { "PASS" } else { "FAIL" };
    write!(
        writer,
        "{status} extraction-conformance producer={:?} version={:?} parser_family={:?} expected_glyphs={} actual_glyphs={}",
        record.producer.name,
        record.producer.version,
        record.producer.parser_family,
        record.expected_glyphs,
        record.actual_glyphs
    )
    .map_err(|error| format!("cannot write extraction conformance result: {error}"))?;
    if let Some(mismatch) = &record.mismatch {
        write!(writer, " detail={mismatch}")
            .map_err(|error| format!("cannot write extraction conformance result: {error}"))?;
    }
    writeln!(writer)
        .map_err(|error| format!("cannot write extraction conformance result: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush extraction conformance output: {error}"))?;
    Ok(u8::from(!record.passed()))
}

fn render_fixture<W: Write>(
    writer: &mut W,
    input: &Path,
    renderer: RendererChoice,
    output: &Path,
) -> Result<u8, String> {
    let yaml = read_canonical_yaml_file(input)?;
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

fn evaluate_yaml<W: Write>(
    writer: &mut W,
    input: &Path,
    renderer: RendererChoice,
    mutation: YamlMutation,
) -> Result<u8, String> {
    let (case_name, plan) = yaml_mutation_plan(input, mutation)?;
    let record = evaluate(
        case_name,
        plan.old(),
        plan.new_plan(),
        plan.expectation(),
        renderer.kind(),
    )
    .map_err(|error| error.to_string())?;
    write_record(writer, &record)?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush YAML evaluation output: {error}"))?;
    Ok(u8::from(!record.passed))
}

fn evaluate_rendered_yaml<W: Write>(
    writer: &mut W,
    input: &Path,
    old_pdf: &Path,
    new_pdf: &Path,
    renderer: &str,
    mutation: YamlMutation,
) -> Result<u8, String> {
    let (case_name, plan) = yaml_mutation_plan(input, mutation)?;
    let max_pdf_bytes = RenderLimits::default().max_pdf_bytes;
    let old_pdf = read_bounded_file(old_pdf, max_pdf_bytes, "old rendered PDF")?;
    let new_pdf = read_bounded_file(new_pdf, max_pdf_bytes, "new rendered PDF")?;
    let record = evaluate_rendered(
        case_name,
        plan.old(),
        plan.new_plan(),
        plan.expectation(),
        renderer,
        Arc::from(old_pdf),
        Arc::from(new_pdf),
    )
    .map_err(|error| error.to_string())?;
    write_record(writer, &record)?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush rendered YAML evaluation output: {error}"))?;
    Ok(u8::from(!record.passed))
}

fn yaml_mutation_plan(
    input: &Path,
    mutation: YamlMutation,
) -> Result<(&'static str, MutationPlan), String> {
    let yaml = read_canonical_yaml_file(input)?;
    let document = CanonicalRenderDocument::from_yaml(&yaml).map_err(|error| error.to_string())?;
    let (case_name, mutation) = mutation.into_mutation();
    let plan = mutation
        .apply_to_render_document(&document, CANONICAL_RENDER_LINE_GAP)
        .map_err(|error| error.to_string())?;
    Ok((case_name, plan))
}

fn verify<W: Write>(writer: &mut W) -> Result<u8, String> {
    let cases = built_in_cases().map_err(|error| error.to_string())?;
    let total = cases
        .len()
        .checked_mul(RendererKind::all().len())
        .ok_or_else(|| "benchmark matrix size overflowed".to_owned())?;
    let mut passed = 0_usize;
    let mut execution_error = false;
    let mut precision_records = Vec::with_capacity(total);

    for case in &cases {
        for renderer in RendererKind::all() {
            match evaluate_case(case, renderer) {
                Ok(record) => {
                    if record.passed {
                        passed += 1;
                    }
                    precision_records.push(record.precision);
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
    writeln!(
        writer,
        "author-intent accepted={passed}/{total} renderer cells"
    )
    .map_err(|error| format!("cannot write strict acceptance summary: {error}"))?;
    write_generated_precision_summary(writer, &precision_records, total)?;
    writeln!(writer, "{passed}/{total} passed")
        .map_err(|error| format!("cannot write benchmark summary: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush benchmark output: {error}"))?;

    Ok(if execution_error {
        2
    } else {
        u8::from(passed != total)
    })
}

fn write_generated_precision_summary<W: Write>(
    writer: &mut W,
    records: &[GeneratedPrecisionMetrics],
    total: usize,
) -> Result<(), String> {
    let evaluated = records.len();
    let precision = (evaluated == total)
        .then(|| GeneratedPrecisionMetrics::aggregate(records.iter().copied()))
        .flatten();
    if let Some(precision) = precision {
        let tokens = precision
            .token_metrics
            .expect("complete aggregates contain token metrics");
        writeln!(
            writer,
            "generated precision evaluated={evaluated}/{total} complete events={}/{} reported={} p={:.3} r={:.3} f1={:.3} tokens=tp:{},fp:{},fn:{} p={:.3} r={:.3} f1={:.3} fp_tokens_per_10k_unchanged={:.3}",
            precision.matched_events,
            precision.expected_events,
            precision.reported_events,
            precision.event_precision,
            precision.event_recall,
            precision.event_f1,
            tokens.changed_token_true_positives,
            tokens.changed_token_false_positives,
            tokens.changed_token_false_negatives,
            tokens.changed_token_precision,
            tokens.changed_token_recall,
            tokens.changed_token_f1,
            tokens.false_positive_changed_tokens_per_10k_unchanged_tokens,
        )
        .map_err(|error| format!("cannot write benchmark precision summary: {error}"))?;
    } else {
        writeln!(
            writer,
            "generated precision evaluated={evaluated}/{total} unavailable"
        )
        .map_err(|error| format!("cannot write benchmark precision summary: {error}"))?;
    }
    Ok(())
}

fn ratio_label(ratio: Option<f64>) -> String {
    ratio.map_or_else(|| "unavailable".to_owned(), |value| format!("{value:.3}"))
}

fn write_record<W: Write>(writer: &mut W, record: &EvaluationRecord) -> Result<(), String> {
    let status = if record.passed { "PASS" } else { "FAIL" };
    let token_precision = record.precision.token_metrics.map_or_else(
        || "unavailable".to_owned(),
        |tokens| {
            format!(
                "{}/{}/{}:{:.3}/{:.3}/{:.3},fp10k:{:.3}",
                tokens.changed_token_true_positives,
                tokens.changed_token_false_positives,
                tokens.changed_token_false_negatives,
                tokens.changed_token_precision,
                tokens.changed_token_recall,
                tokens.changed_token_f1,
                tokens.false_positive_changed_tokens_per_10k_unchanged_tokens,
            )
        },
    );
    writeln!(
        writer,
        "{status} case={} renderer={} expected={} actual={} coverage={}/{} precision=events:{}/{}/{}:{:.3}/{:.3}/{:.3},tokens:{} detail={}",
        record.case_name,
        record.renderer,
        record.expected.label(),
        actual_label(&record.actual_kinds),
        ratio_label(record.old_coverage),
        ratio_label(record.new_coverage),
        record.precision.matched_events,
        record.precision.reported_events,
        record.precision.expected_events,
        record.precision.event_precision,
        record.precision.event_recall,
        record.precision.event_f1,
        token_precision,
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

#[cfg(test)]
mod tests {
    use pdfdelta_bench::evaluator::GeneratedTokenMetrics;

    use super::*;

    #[test]
    fn read_bounded_file_accepts_the_limit_and_rejects_one_byte_more() {
        let path = std::env::temp_dir().join(format!(
            "pdfbench-bounded-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::write(&path, b"abc").expect("bounded fixture");
        let exact = read_bounded_file(&path, 3, "bounded fixture").expect("exact limit");
        assert_eq!(exact, b"abc");
        let error = read_bounded_file(&path, 2, "bounded fixture").expect_err("oversized input");
        fs::remove_file(&path).expect("bounded fixture cleanup");
        assert!(error.contains("must not exceed 2 bytes"), "{error}");
    }

    #[test]
    fn generated_precision_summary_is_unavailable_when_evaluation_is_incomplete() {
        let mut output = Vec::new();
        write_generated_precision_summary(&mut output, &[], 1).expect("summary writes");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "generated precision evaluated=0/1 unavailable\n"
        );

        let mut output = Vec::new();
        let incomplete = GeneratedPrecisionMetrics {
            reported_events: 1,
            expected_events: 0,
            matched_events: 0,
            event_precision: 0.0,
            event_recall: 1.0,
            event_f1: 0.0,
            token_metrics: None,
        };
        write_generated_precision_summary(&mut output, &[incomplete], 1).expect("summary writes");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "generated precision evaluated=1/1 unavailable\n"
        );
    }

    #[test]
    fn generated_precision_summary_prints_complete_metrics() {
        let mut output = Vec::new();
        let complete = GeneratedPrecisionMetrics {
            reported_events: 1,
            expected_events: 1,
            matched_events: 1,
            event_precision: 1.0,
            event_recall: 1.0,
            event_f1: 1.0,
            token_metrics: Some(GeneratedTokenMetrics {
                changed_token_true_positives: 2,
                changed_token_false_positives: 0,
                changed_token_false_negatives: 0,
                changed_token_precision: 1.0,
                changed_token_recall: 1.0,
                changed_token_f1: 1.0,
                unchanged_tokens: 18,
                false_positive_changed_tokens_per_10k_unchanged_tokens: 0.0,
            }),
        };
        write_generated_precision_summary(&mut output, &[complete], 1).expect("summary writes");
        assert!(
            String::from_utf8(output)
                .expect("UTF-8")
                .contains("evaluated=1/1 complete events=1/1")
        );
    }
}
