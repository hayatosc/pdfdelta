//! Shared CLI orchestration for the flow evidence comparison.
//!
//! Output destinations are validated here before any PDF is read. The
//! comparison itself, including report publication, lives in
//! [`crate::evidence_compare`].

use std::{io::Write, path::Path};

use pdfdelta_core::options::PipelineOptions;
use pdfdelta_flow::FlowError;

use crate::{
    args::{CompareCommand, ComparisonInput},
    evidence_text::escape_terminal_controls,
    fs::{
        ensure_named_output_does_not_alias_input, ensure_output_does_not_alias_input,
        ensure_trace_does_not_alias_input, paths_refer_to_same_file,
    },
};

/// Fatal CLI failures share one process status. Content-change and
/// incomplete-comparison statuses are chosen by the flow exit policy.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitStatus {
    ExecutionError = 2,
}

impl ExitStatus {
    pub(crate) const fn code(self) -> u8 {
        self as u8
    }
}

/// Validates output destinations before a comparison reads any input.
///
/// Every CLI comparison path uses the same checks so a native-text run cannot
/// overwrite an input or an extraction cache that the shared-evidence path
/// would refuse.
pub fn validate_destinations(command: CompareCommand<'_>) -> Result<(), FlowError> {
    let old_path = command
        .old_path
        .ok_or_else(|| FlowError::backend("cannot compare PDFs: OLD_PDF is required"))?;
    let new_path = command
        .new_path
        .ok_or_else(|| FlowError::backend("cannot compare PDFs: NEW_PDF is required"))?;
    if old_path == Path::new("-") && new_path == Path::new("-") {
        return Err(FlowError::backend(
            "cannot read both OLD_PDF and NEW_PDF from standard input",
        ));
    }
    if let Some(trace_path) = command.trace_path {
        ensure_trace_does_not_alias_input(trace_path, old_path, new_path)
            .map_err(FlowError::backend)?;
    }
    // One guard per pair of output destinations; the pair table keeps the
    // exact refusal messages next to the aliasing check they motivate.
    const TRACE_REPORT: &str = "trace/report output collision";
    const REPORT_JSON: &str = "report/json output collision";
    let report_output = command.options.output_path;
    let json_output = command.options.json_path;
    let trace_output = command.trace_path;
    for directory in [command.options.review_dir, command.options.agent_review_dir]
        .into_iter()
        .flatten()
    {
        crate::review::bundle::validate_destination(
            directory,
            &[json_output, report_output, trace_output],
        )?;
    }
    for (left, right, label, left_noun, right_noun) in [
        (
            trace_output,
            json_output,
            TRACE_REPORT,
            "trace output",
            "JSON report",
        ),
        (
            trace_output,
            report_output,
            TRACE_REPORT,
            "trace output",
            "text report",
        ),
        (
            report_output,
            json_output,
            REPORT_JSON,
            "text report output",
            "JSON report",
        ),
    ] {
        if let (Some(left), Some(right)) = (left, right)
            && paths_refer_to_same_file(left, right, label).map_err(FlowError::backend)?
        {
            return Err(FlowError::backend(format!(
                "refusing {left_noun} {} because it refers to the {right_noun} {}",
                left.display(),
                right.display()
            )));
        }
    }

    if let Some(cache_dir) = command.extraction_cache_dir {
        for (destination, noun) in [
            (trace_output, "trace output"),
            (json_output, "JSON report"),
            (report_output, "text report output"),
        ] {
            if let Some(destination) = destination
                && paths_refer_to_same_file(
                    destination,
                    cache_dir,
                    "extraction cache/output collision",
                )
                .map_err(FlowError::backend)?
            {
                return Err(FlowError::backend(format!(
                    "refusing {noun} {} because it refers to the extraction cache directory {}",
                    destination.display(),
                    cache_dir.display()
                )));
            }
        }
    }

    if let Some(json_path) = command.options.json_path {
        ensure_output_does_not_alias_input(json_path, old_path, new_path)
            .map_err(FlowError::backend)?;
    }
    if let Some(output_path) = command.options.output_path {
        ensure_named_output_does_not_alias_input(
            "text report output",
            output_path,
            old_path,
            new_path,
        )
        .map_err(FlowError::backend)?;
    }

    Ok(())
}

pub fn compare_documents_with_evidence(
    command: CompareCommand<'_>,
    evidence: &crate::evidence_compare::EvidenceOptions<'_>,
) -> Result<u8, FlowError> {
    let old_path = command
        .old_path
        .ok_or_else(|| FlowError::backend("cannot compare PDFs: OLD_PDF is required"))?;
    let new_path = command
        .new_path
        .ok_or_else(|| FlowError::backend("cannot compare PDFs: NEW_PDF is required"))?;

    if old_path == Path::new("-") && new_path == Path::new("-") {
        return Err(FlowError::backend(
            "cannot read both OLD_PDF and NEW_PDF from standard input",
        ));
    }
    let pipeline_options = PipelineOptions::default()
        .scaled_limits(command.limit_scale)
        .map_err(|error| FlowError::from(error).with_context("cannot compare PDFs"))?;

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

    validate_destinations(command)?;

    let (status, _incomplete) = crate::evidence_compare::compare(
        old_input,
        new_input,
        pipeline_options,
        command.options,
        command.extraction_cache_dir,
        evidence,
    )?;
    Ok(status)
}

pub fn report_fatal_error<W: Write>(writer: &mut W, error: &str) {
    let _ = writeln!(writer, "{}", escape_terminal_controls(error));
    let _ = writer.flush();
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::report_fatal_error;

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn final_error_diagnostic_ignores_writer_failure() {
        report_fatal_error(&mut BrokenPipeWriter, "comparison failed");
    }

    #[test]
    fn terminal_diagnostics_escape_pdf_derived_control_characters() {
        let mut fatal = Vec::new();
        report_fatal_error(&mut fatal, "cannot parse /\u{202e}font");
        let text = String::from_utf8(fatal).expect("fatal error is UTF-8");
        assert!(!text.contains('\u{202e}'), "{text:?}");
        assert!(text.contains("\\u{202e}"), "{text:?}");
    }
}
