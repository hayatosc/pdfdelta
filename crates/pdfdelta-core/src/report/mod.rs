mod json;

use crate::{
    Error, Result,
    diff::{ChangeKind, Comparison, Confidence, TextSpan},
};

pub use json::write_json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentSide {
    Old,
    New,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedRegion {
    pub side: DocumentSide,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionStatus {
    pub old_complete: bool,
    pub new_complete: bool,
    pub unsupported_regions: Vec<UnsupportedRegion>,
}

impl ExtractionStatus {
    pub fn complete() -> Self {
        Self {
            old_complete: true,
            new_complete: true,
            unsupported_regions: Vec::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        let old_regions = self
            .unsupported_regions
            .iter()
            .filter(|region| region.side == DocumentSide::Old)
            .count();
        let new_regions = self
            .unsupported_regions
            .iter()
            .filter(|region| region.side == DocumentSide::New)
            .count();

        validate_side_status("old", self.old_complete, old_regions)?;
        validate_side_status("new", self.new_complete, new_regions)?;
        if self
            .unsupported_regions
            .iter()
            .any(|region| region.description.trim().is_empty())
        {
            return Err(Error::InvalidConfiguration(
                "unsupported extraction regions require a description".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for ExtractionStatus {
    fn default() -> Self {
        Self::complete()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReportSummary {
    pub content_changes: usize,
    pub formatting_only_changes: usize,
    pub uncertain_changes: usize,
    pub unresolved_regions: usize,
    pub unsupported_regions: usize,
    pub old_extraction_complete: bool,
    pub new_extraction_complete: bool,
    pub old_alignment_coverage: f64,
    pub new_alignment_coverage: f64,
    pub comparison_coverage: f64,
    pub comparison_complete: bool,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    NoContentChanges = 0,
    ContentChanges = 1,
    ExecutionError = 2,
    IncompleteComparison = 3,
}

impl ExitStatus {
    pub const fn code(self) -> u8 {
        self as u8
    }
}

pub fn summarize(comparison: &Comparison, extraction: &ExtractionStatus) -> Result<ReportSummary> {
    extraction.validate()?;
    validate_comparison(comparison)?;
    validate_coverage(
        "old",
        comparison.old_coverage.resolved_tokens,
        comparison.old_coverage.total_tokens,
        comparison.old_coverage.ratio,
    )?;
    validate_coverage(
        "new",
        comparison.new_coverage.resolved_tokens,
        comparison.new_coverage.total_tokens,
        comparison.new_coverage.ratio,
    )?;

    let comparison_complete = comparison.unresolved_regions.is_empty()
        && comparison.old_coverage.resolved_tokens == comparison.old_coverage.total_tokens
        && comparison.new_coverage.resolved_tokens == comparison.new_coverage.total_tokens
        && extraction.old_complete
        && extraction.new_complete;
    Ok(ReportSummary {
        content_changes: comparison.changes.len(),
        formatting_only_changes: comparison.formatting_changes.len(),
        uncertain_changes: comparison
            .changes
            .iter()
            .filter(|change| change.confidence == Confidence::Low)
            .count(),
        unresolved_regions: comparison.unresolved_regions.len(),
        unsupported_regions: extraction.unsupported_regions.len(),
        old_extraction_complete: extraction.old_complete,
        new_extraction_complete: extraction.new_complete,
        old_alignment_coverage: comparison.old_coverage.ratio,
        new_alignment_coverage: comparison.new_coverage.ratio,
        comparison_coverage: comparison
            .old_coverage
            .ratio
            .min(comparison.new_coverage.ratio),
        comparison_complete,
    })
}

pub fn render_text(comparison: &Comparison, extraction: &ExtractionStatus) -> Result<String> {
    use std::fmt::Write;

    let summary = summarize(comparison, extraction)?;
    let mut output = format!(
        "Content changes:          {}\n\
         Formatting-only changes:  {}\n\
         Uncertain changes:        {}\n\
         Unresolved regions:       {}\n\
         Extraction complete:      old={}, new={}\n\
         Alignment coverage:       old={:.1}%, new={:.1}%\n\
         Comparison coverage:      {:.1}%\n",
        summary.content_changes,
        summary.formatting_only_changes,
        summary.uncertain_changes,
        summary.unresolved_regions,
        yes_no(summary.old_extraction_complete),
        yes_no(summary.new_extraction_complete),
        percentage(summary.old_alignment_coverage),
        percentage(summary.new_alignment_coverage),
        percentage(summary.comparison_coverage),
    );
    for region in &extraction.unsupported_regions {
        writeln!(
            output,
            "Unsupported region ({}): {}",
            side_name(region.side),
            region.description
        )
        .map_err(|error| Error::Report(error.to_string()))?;
    }
    Ok(output)
}

pub fn exit_status(
    comparison: &Comparison,
    extraction: &ExtractionStatus,
    strict: bool,
) -> Result<ExitStatus> {
    let summary = summarize(comparison, extraction)?;
    if strict && !summary.comparison_complete {
        Ok(ExitStatus::IncompleteComparison)
    } else if summary.content_changes > 0 {
        Ok(ExitStatus::ContentChanges)
    } else {
        Ok(ExitStatus::NoContentChanges)
    }
}

fn validate_side_status(side: &str, complete: bool, region_count: usize) -> Result<()> {
    if complete && region_count != 0 {
        return Err(Error::InvalidConfiguration(format!(
            "{side} extraction cannot be complete with unsupported regions"
        )));
    }
    if !complete && region_count == 0 {
        return Err(Error::InvalidConfiguration(format!(
            "incomplete {side} extraction requires an unsupported region"
        )));
    }
    Ok(())
}

fn validate_comparison(comparison: &Comparison) -> Result<()> {
    for change in &comparison.changes {
        let valid_shape = match change.kind {
            ChangeKind::Replacement | ChangeKind::Move => {
                change.old_span.is_some() && change.new_span.is_some()
            }
            ChangeKind::Insertion => change.old_span.is_none() && change.new_span.is_some(),
            ChangeKind::Deletion => change.old_span.is_some() && change.new_span.is_none(),
        };
        if !valid_shape {
            return Err(Error::InvalidConfiguration(format!(
                "invalid {:?} change span shape",
                change.kind
            )));
        }
        for span in change.old_span.iter().chain(change.new_span.iter()) {
            validate_change_span(span)?;
        }
    }

    for change in &comparison.formatting_changes {
        validate_text_span("formatting change", &change.old_span)?;
        validate_text_span("formatting change", &change.new_span)?;
        if change.reasons.is_empty() {
            return Err(Error::InvalidConfiguration(
                "formatting changes require at least one reason".to_owned(),
            ));
        }
    }

    for region in &comparison.unresolved_regions {
        if region.old_span.is_none() && region.new_span.is_none() {
            return Err(Error::InvalidConfiguration(
                "unresolved regions require at least one text span".to_owned(),
            ));
        }
        for span in region.old_span.iter().chain(region.new_span.iter()) {
            validate_text_span("unresolved region", span)?;
        }
    }
    Ok(())
}

fn validate_change_span(span: &TextSpan) -> Result<()> {
    validate_text_span("change", span)?;
    if span.comparable_range.start == span.comparable_range.end {
        return Err(Error::InvalidConfiguration(
            "content change spans require at least one comparable token".to_owned(),
        ));
    }
    Ok(())
}

fn validate_text_span(context: &str, span: &TextSpan) -> Result<()> {
    if span.blocks.is_empty() {
        return Err(Error::InvalidConfiguration(format!(
            "{context} text spans require at least one block"
        )));
    }
    if span.canonical_range.start > span.canonical_range.end
        || span.comparable_range.start > span.comparable_range.end
    {
        return Err(Error::InvalidConfiguration(format!(
            "{context} text span ranges must be ordered"
        )));
    }
    Ok(())
}

fn validate_coverage(side: &str, resolved: usize, total: usize, ratio: f64) -> Result<()> {
    let expected_ratio = if total == 0 {
        1.0
    } else {
        resolved as f64 / total as f64
    };
    if resolved > total
        || !ratio.is_finite()
        || !(0.0..=1.0).contains(&ratio)
        || (ratio - expected_ratio).abs() > 1.0e-12
    {
        return Err(Error::InvalidConfiguration(format!(
            "invalid {side} alignment coverage"
        )));
    }
    Ok(())
}

fn percentage(ratio: f64) -> f64 {
    ratio * 100.0
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(crate) fn side_name(side: DocumentSide) -> &'static str {
    match side {
        DocumentSide::Old => "old",
        DocumentSide::New => "new",
    }
}
