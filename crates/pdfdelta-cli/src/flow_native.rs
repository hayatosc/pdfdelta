//! Native schema-13 adapter for the flow text engine.
//!
//! `--native-text-only` writes the compact native flow envelope. The report
//! contains only what flow computed: token counts, extraction completeness,
//! content/formatting/uncertain counts, the verified change script, formatting
//! evidence, extraction issues, and layout-only counts. It has no proof-era
//! assessment, candidate, proven-region, unresolved-region, or alignment
//! coverage positions.

use std::{io::Write, ops::Range, path::Path};

use pdfdelta_core::source::ExtractionIssue;
use pdfdelta_flow::{ChangeAssessment, Confidence, FlowChange, FlowError, FlowStream};
use serde_json::{Value, json};

use crate::{
    flow_cmd::{self, FlowComparison, FlowComparisonOptions},
    fs::{write_output_atomically, write_text_report_atomically},
};

/// One side's extraction and flow comparison inputs.
pub(crate) struct NativeFlowOptions<'a> {
    pub(crate) limit_scale: f64,
    pub(crate) old_password: Option<&'a str>,
    pub(crate) new_password: Option<&'a str>,
    pub(crate) old_font_identities: &'a pdfdelta_core::source::ExternalFontIdentities,
    pub(crate) new_font_identities: &'a pdfdelta_core::source::ExternalFontIdentities,
    pub(crate) cache: Option<&'a crate::extraction_cache::ExtractionCache>,
    pub(crate) json_path: Option<&'a Path>,
    pub(crate) output_path: Option<&'a Path>,
    pub(crate) quiet: bool,
    pub(crate) color: bool,
    pub(crate) full_json: bool,
    pub(crate) trace_json: Option<&'a Path>,
    pub(crate) agent_review_dir: Option<&'a Path>,
    /// Explicit case budget for an agent review bundle.
    pub(crate) agent_case_budget: usize,
}

/// Runs the flow engine on the main native-text path.
///
/// Returns the shared flow exit code: a detected change selects exit 1 even
/// when paint or visual evidence was not examined; broken extraction evidence
/// keeps exit 3; exit 3 is otherwise used for unexamined-only incompleteness.
///
/// # Errors
///
/// Returns a message when input extraction, flow verification, or report
/// writing fails. The caller maps it to exit code 2.
pub(crate) fn run(
    old_path: &Path,
    new_path: &Path,
    options: &NativeFlowOptions<'_>,
    writer: &mut impl Write,
) -> Result<u8, FlowError> {
    if let Some(path) = options.trace_json
        && path.exists()
    {
        return Err(FlowError::backend(format!(
            "refusing to overwrite existing flow trace report {}: output path already exists",
            path.display()
        )));
    }
    if let Some(directory) = options.agent_review_dir {
        crate::review::bundle::validate_destination(
            directory,
            &[options.json_path, options.output_path, options.trace_json],
        )?;
    }
    let mut trace = options
        .trace_json
        .map(|_| crate::flow_trace::FlowTrace::new(old_path, new_path));
    let comparison = match flow_cmd::compare_flow(
        old_path,
        new_path,
        &FlowComparisonOptions {
            limit_scale: options.limit_scale,
            retain_sources: options.full_json || options.agent_review_dir.is_some(),
            retain_source_glyphs: options.full_json,
            retain_input: options.agent_review_dir.is_some(),
            old_password: options.old_password,
            new_password: options.new_password,
            old_font_identities: options.old_font_identities,
            new_font_identities: options.new_font_identities,
            cache: options.cache,
        },
        trace.as_mut(),
    ) {
        Ok(comparison) => comparison,
        Err(error) => {
            if let Some(trace) = trace.as_mut() {
                trace.finish(2, false, false);
                crate::flow_trace::write_atomically(
                    options.trace_json.expect("trace exists"),
                    trace,
                )?;
            }
            return Err(error);
        }
    };
    let detected = comparison.non_layout_changes() > 0;
    let exit = crate::flow_exit::select(
        detected,
        comparison.text_broken_evidence(),
        comparison.text_unexamined_evidence(),
    );
    let mut text = render_text(&comparison, options.color);
    if exit.basis == "detected_with_unexamined_evidence" {
        text.push('\n');
        text.push_str(flow_cmd::DETECTED_WITH_UNEXAMINED_NOTE);
        text.push('\n');
    }
    if let Some(path) = options.output_path {
        write_text_report_atomically(path, &text).map_err(FlowError::backend)?;
    } else if !options.quiet {
        writer
            .write_all(text.as_bytes())
            .map_err(|error| FlowError::backend(format!("failed to write flow report: {error}")))?;
    }
    let report_start = std::time::Instant::now();
    if let Some(path) = options.json_path {
        if options.full_json {
            crate::flow_full::write_report(&comparison, path).map_err(FlowError::backend)?;
        } else {
            let report = native_report(&comparison);
            write_output_atomically(path, "native flow JSON report", |output| {
                serde_json::to_writer(output, &report)
                    .map_err(|error| format!("failed to serialize native flow report: {error}"))
            })
            .map_err(FlowError::backend)?;
        }
    }
    if let Some(directory) = options.agent_review_dir {
        write_agent_review(
            directory,
            &comparison,
            exit.code,
            options,
            old_path,
            new_path,
        )?;
    }
    if let Some(trace) = trace.as_mut() {
        trace.complete(
            "report",
            None,
            [
                ("text_bytes", text.len()),
                ("json", usize::from(options.json_path.is_some())),
                ("full_json", usize::from(options.full_json)),
            ],
            report_start.elapsed(),
        );
        trace.finish(exit.code, !comparison.text_complete(), false);
        crate::flow_trace::write_atomically(options.trace_json.expect("trace exists"), trace)?;
    }
    Ok(exit.code)
}

/// Publishes a flow-native agent review bundle for this run.
///
/// The plan is projected from the comparison the process already produced, so
/// its cases and source references point at exactly this run's evidence.
fn write_agent_review(
    directory: &Path,
    comparison: &FlowComparison,
    exit: u8,
    options: &NativeFlowOptions<'_>,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), FlowError> {
    let Some((old, new)) = crate::flow_review::source_bytes(comparison) else {
        return Err(FlowError::backend("review source bytes were not retained"));
    };
    let owned = crate::flow_review::project(comparison);
    let review = owned.review(comparison);
    let plan = pdfdelta_core::review::plan_flow_native(
        &pdfdelta_core::review::FlowNativeReview {
            identity: crate::flow_review::native_identity(comparison, options.limit_scale, true),
            outcome: crate::flow_review::outcome(comparison, exit),
            flow: review,
        },
        pdfdelta_core::review::PlannerLimits {
            max_cases: options.agent_case_budget,
            ..pdfdelta_core::review::PlannerLimits::default()
        },
    );
    // Input names are caller-supplied text; they are never used to choose an
    // artifact path.
    crate::agent_review::write(
        directory,
        &plan,
        crate::agent_review::SourceDocument {
            name: old_path,
            bytes: &old,
        },
        crate::agent_review::SourceDocument {
            name: new_path,
            bytes: &new,
        },
        &[],
    )
}

/// Renders the flow report with the report's ANSI conventions when colour is
/// requested.
///
/// The flow report is line-oriented rather than a unified diff, so the painter
/// assigns meaning to its own markers: `old` lines red, `new` lines green,
/// delete/replace summaries and incomplete warnings yellow, moves magenta, and
/// section headers bold. With colour disabled the output is byte-identical to
/// the plain report.
pub(crate) fn render_text(comparison: &FlowComparison, color: bool) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "text engine: flow ({} detected changes, {} formatting-only)\n",
        comparison.non_layout_changes(),
        comparison.formatting.len()
    ));
    text.push_str(&flow_cmd::render_text(comparison));
    if color {
        flow_cmd::paint_report(&text)
    } else {
        text
    }
}

/// Builds the compact native schema-13 artifact for a flow comparison.
pub(crate) fn native_report(comparison: &FlowComparison) -> Value {
    let old = &comparison.old_side;
    let new = &comparison.new_side;
    let complete = comparison.text_complete();
    let mut changes = Vec::new();
    let mut reading_order = Vec::new();
    for (index, change) in comparison.diff.changes.iter().enumerate() {
        if matches!(change, FlowChange::LayoutOnly { .. }) {
            continue;
        }
        if matches!(change, FlowChange::ReadingOrder { .. }) {
            reading_order.push(change_json(
                change,
                comparison.assessments.get(index),
                &old.stream,
                &new.stream,
            ));
            continue;
        }
        changes.push(change_json(
            change,
            comparison.assessments.get(index),
            &old.stream,
            &new.stream,
        ));
    }
    let formatting = comparison
        .formatting
        .iter()
        .map(|change| {
            let mut value = json!({
                "old_span": span_json(&old.stream, change.old.clone()),
                "new_span": span_json(&new.stream, change.new.clone()),
                "reasons": change
                    .reasons
                    .iter()
                    .map(|reason| reason.label())
                    .collect::<Vec<_>>(),
            });
            if let (Some(old_size), Some(new_size)) = (change.old_size, change.new_size) {
                value["old_size"] = json!(old_size);
                value["new_size"] = json!(new_size);
            }
            value
        })
        .collect::<Vec<_>>();
    let (old_issues, new_issues) = extraction_issues(old, new);
    let (unsupported, unresolved, paint_issues) = evidence_issue_counts(old, new);
    let uncertain = comparison
        .assessments
        .iter()
        .filter(|assessment| assessment.confidence == Confidence::Low)
        .count();
    let total_old = old.stream.tokens.len();
    let total_new = new.stream.tokens.len();
    let old_complete = old.is_complete();
    let new_complete = new.is_complete();
    let detected = comparison.non_layout_changes() > 0;
    let exit = crate::flow_exit::select(
        detected,
        comparison.text_broken_evidence(),
        comparison.text_unexamined_evidence(),
    );
    let status = flow_cmd::difference_status(comparison, detected);
    let summary = json!({
        "content_changes": comparison.non_layout_changes(),
        "formatting_only_changes": comparison.formatting.len(),
        "reading_order_changes": comparison.stats.reading_order.changes,
        "uncertain_changes": uncertain,
        "unsupported_extraction_issues": unsupported,
        "unresolved_extraction_issues": unresolved,
        "unexamined_paint_issues": paint_issues,
        "comparison_complete": complete,
        "difference_status": status,
        "old_tokens": total_old,
        "new_tokens": total_new,
    });
    let report = json!({
        "schema_version": 13,
        "engine": "flow",
        "difference_status": status,
        "exit_basis": exit.basis,
        "comparison_scope": {"supported_text": true, "images_compared": false},
        "summary": summary,
        "changes": changes,
        "reading_order_changes": reading_order,
        "formatting_only_changes": formatting,
        "extraction": {
            "old_complete": old_complete,
            "new_complete": new_complete,
            "issues": old_issues.into_iter().chain(new_issues).collect::<Vec<_>>(),
        },
        "flow_summary": {
            "reading_order_changes": comparison.stats.reading_order.changes,
            "reading_order_old_tokens": comparison.stats.reading_order.old_tokens,
            "reading_order_new_tokens": comparison.stats.reading_order.new_tokens,
            "layout_only_changes": comparison.stats.layout_only.changes,
            "layout_only_old_tokens": comparison.stats.layout_only.old_tokens,
            "layout_only_new_tokens": comparison.stats.layout_only.new_tokens,
        },
    });
    json!({
        "artifact_format": "pdfdelta-native-compact",
        "artifact_version": 3,
        "source_schema_version": 13,
        "engine": "flow",
        "omitted_fields": {
            "sources": "compact reports omit per-glyph sources; use --full-json",
        },
        "report": report,
    })
}

/// Counts extraction issues by class and separates the paint-gate issues.
///
/// Paint-gate issues record evidence that was not examined, not extraction that
/// failed. Their own counter keeps consumers from treating unexamined paint as
/// broken text extraction.
fn evidence_issue_counts(old: &flow_cmd::Side, new: &flow_cmd::Side) -> (usize, usize, usize) {
    let paint_issues = old.paint_issues.saturating_add(new.paint_issues);
    let mut unsupported = 0usize;
    let mut unresolved = 0usize;
    for issue in old.issues.iter().chain(&new.issues) {
        match issue.kind() {
            pdfdelta_core::source::ExtractionIssueKind::Unsupported => unsupported += 1,
            pdfdelta_core::source::ExtractionIssueKind::Unresolved => unresolved += 1,
        }
    }
    (
        unsupported,
        unresolved.saturating_sub(paint_issues),
        paint_issues,
    )
}

fn extraction_issues(old: &flow_cmd::Side, new: &flow_cmd::Side) -> (Vec<Value>, Vec<Value>) {
    let map = |side: &str, issues: &[ExtractionIssue]| {
        issues
            .iter()
            .map(|issue| {
                json!({
                    "side": side,
                    "kind": match issue.kind() {
                        pdfdelta_core::source::ExtractionIssueKind::Unsupported => "unsupported",
                        pdfdelta_core::source::ExtractionIssueKind::Unresolved => "unresolved",
                    },
                    "message": issue.description(),
                })
            })
            .collect::<Vec<_>>()
    };
    (map("old", &old.issues), map("new", &new.issues))
}

fn change_json(
    change: &FlowChange,
    assessment: Option<&ChangeAssessment>,
    old: &FlowStream,
    new: &FlowStream,
) -> Value {
    let (kind, old_range, new_range) = change_ranges(change);
    let old_span = if old_range.is_empty() {
        Value::Null
    } else {
        span_json(old, old_range)
    };
    let new_span = if new_range.is_empty() {
        Value::Null
    } else {
        span_json(new, new_range)
    };
    let mut value = json!({
        "kind": kind,
        "old_span": old_span,
        "new_span": new_span,
    });
    if let Some(assessment) = assessment {
        value["confidence"] = json!(assessment.confidence.label());
        value["ambiguity"] = json!(
            assessment
                .ambiguity
                .iter()
                .map(|reason| reason.label())
                .collect::<Vec<_>>()
        );
    }
    value
}

fn span_json(stream: &FlowStream, range: Range<usize>) -> Value {
    let tokens = &stream.tokens[range.clone()];
    let text = tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut pages = Vec::new();
    for token in tokens {
        let page = u64::from(token.page.0) + 1;
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    json!({
        "pages": pages,
        "text": text,
        "token_range": {"start": range.start, "end": range.end},
    })
}

fn change_ranges(change: &FlowChange) -> (&'static str, Range<usize>, Range<usize>) {
    match change {
        FlowChange::Delete { old } => ("deletion", old.clone(), 0..0),
        FlowChange::Insert { new } => ("insertion", 0..0, new.clone()),
        FlowChange::Replace { old, new } => ("replacement", old.clone(), new.clone()),
        FlowChange::Move { old, new } => ("move", old.clone(), new.clone()),
        FlowChange::ReadingOrder { old, new } => ("reading-order", old.clone(), new.clone()),
        FlowChange::LayoutOnly { old, new } => ("layout-only", old.clone(), new.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(paint_issues: usize, issues: Vec<ExtractionIssue>) -> flow_cmd::Side {
        flow_cmd::Side {
            stream: FlowStream::default(),
            issues,
            paint_issues,
            order: Vec::new(),
            image_identity_pages: std::collections::BTreeSet::new(),
            image_identity_rules: std::collections::BTreeMap::new(),
            glyph_name_identity_pages: std::collections::BTreeSet::new(),
        }
    }

    fn issue(kind: pdfdelta_core::source::ExtractionIssueKind, text: &str) -> ExtractionIssue {
        ExtractionIssue::new(kind, pdfdelta_core::source::ExtractionScope::Document, text)
            .expect("issue")
    }

    #[test]
    fn paint_gate_issues_are_counted_separately_from_extraction() {
        use pdfdelta_core::source::ExtractionIssueKind;
        let old = side(
            1,
            vec![
                issue(ExtractionIssueKind::Unresolved, "non-text paint differs"),
                issue(ExtractionIssueKind::Unresolved, "a glyph is missing"),
            ],
        );
        let new = side(
            1,
            vec![issue(
                ExtractionIssueKind::Unresolved,
                "non-text paint differs",
            )],
        );
        assert_eq!(evidence_issue_counts(&old, &new), (0, 1, 2));
        let unsupported = side(
            0,
            vec![issue(
                ExtractionIssueKind::Unsupported,
                "unsupported filter",
            )],
        );
        assert_eq!(
            evidence_issue_counts(&side(0, Vec::new()), &unsupported),
            (1, 0, 0)
        );
    }
}
