mod extraction_svg;
mod json;
mod svg;
mod text;

use std::collections::{BTreeMap, HashSet};

use crate::{
    Error, Result,
    alignment::BlockSeparator,
    diff::{ChangeKind, ChangeTag, Comparison, Confidence, TextSpan},
    layout::BlockId,
    model::FontProgramHash,
    normalize::{BlockText, ComparableToken},
    source::{ExtractionIssue, ExtractionIssueKind, ExtractionScope},
};

pub use extraction_svg::render_extraction_mismatch_svg;
pub use json::write_json;
pub use svg::{render_glyph_overlay_svg, write_glyph_overlay_svg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentSide {
    Old,
    New,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionIssueRecord {
    pub side: DocumentSide,
    pub kind: ExtractionIssueKind,
    pub scope: ExtractionScope,
    pub description: String,
}

impl ExtractionIssueRecord {
    pub fn from_issue(side: DocumentSide, issue: ExtractionIssue) -> Self {
        let (kind, scope, description) = issue.into_parts();
        Self {
            side,
            kind,
            scope,
            description,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionStatus {
    pub old_complete: bool,
    pub new_complete: bool,
    pub issues: Vec<ExtractionIssueRecord>,
}

impl ExtractionStatus {
    pub fn complete() -> Self {
        Self {
            old_complete: true,
            new_complete: true,
            issues: Vec::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        let old_issues = self
            .issues
            .iter()
            .filter(|issue| issue.side == DocumentSide::Old)
            .count();
        let new_issues = self
            .issues
            .iter()
            .filter(|issue| issue.side == DocumentSide::New)
            .count();

        validate_side_status("old", self.old_complete, old_issues)?;
        validate_side_status("new", self.new_complete, new_issues)?;
        if self
            .issues
            .iter()
            .any(|issue| issue.description.trim().is_empty())
        {
            return Err(Error::InvalidConfiguration(
                "extraction issues require a description".to_owned(),
            ));
        }
        validate_issue_scopes("old", DocumentSide::Old, &self.issues)?;
        validate_issue_scopes("new", DocumentSide::New, &self.issues)?;
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
    /// Number of reportable semantic content changes (replacements, deletions, insertions, moves).
    pub content_changes: usize,
    /// Number of non-content formatting/structural changes between canonically equivalent text spans.
    pub formatting_only_changes: usize,
    /// Number of content changes with [`Confidence::Low`].
    pub uncertain_changes: usize,
    pub unresolved_regions: usize,
    pub unsupported_extraction_issues: usize,
    pub unresolved_extraction_issues: usize,
    pub old_extraction_complete: bool,
    pub new_extraction_complete: bool,
    pub old_alignment_coverage: Option<f64>,
    pub new_alignment_coverage: Option<f64>,
    pub comparison_coverage: Option<f64>,
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
        extraction.old_complete,
    )?;
    validate_coverage(
        "new",
        comparison.new_coverage.resolved_tokens,
        comparison.new_coverage.total_tokens,
        comparison.new_coverage.ratio,
        extraction.new_complete,
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
        unsupported_extraction_issues: extraction
            .issues
            .iter()
            .filter(|issue| issue.kind == ExtractionIssueKind::Unsupported)
            .count(),
        unresolved_extraction_issues: extraction
            .issues
            .iter()
            .filter(|issue| issue.kind == ExtractionIssueKind::Unresolved)
            .count(),
        old_extraction_complete: extraction.old_complete,
        new_extraction_complete: extraction.new_complete,
        old_alignment_coverage: comparison.old_coverage.ratio,
        new_alignment_coverage: comparison.new_coverage.ratio,
        comparison_coverage: comparison
            .old_coverage
            .ratio
            .zip(comparison.new_coverage.ratio)
            .map(|(old, new)| old.min(new)),
        comparison_complete,
    })
}

pub fn render_text(
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    comparison: &Comparison,
    extraction: &ExtractionStatus,
    options: &TextReportOptions<'_>,
) -> Result<String> {
    let summary = summarize(comparison, extraction)?;
    text::render(
        old_blocks, new_blocks, comparison, extraction, &summary, options,
    )
}

/// Presentation knobs for the human-readable unified-diff report. Labels are
/// rendered verbatim in the `---` / `+++` file headers; color only ever
/// supplements the `-` / `+` markers and must never be required to read the
/// output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextReportOptions<'a> {
    pub old_label: &'a str,
    pub new_label: &'a str,
    pub color: bool,
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

/// Read-only view over one side's normalized blocks, used to resolve report
/// spans back to canonical text and page provenance.
pub(crate) struct SideIndex<'a> {
    blocks: BTreeMap<u64, &'a BlockText>,
}

impl<'a> SideIndex<'a> {
    pub(crate) fn new(blocks: &'a [BlockText]) -> Result<Self> {
        let mut indexed = BTreeMap::new();
        for block in blocks {
            if indexed.insert(block.block.0, block).is_some() {
                return Err(Error::InvalidConfiguration(format!(
                    "duplicate block id {}",
                    block.block.0
                )));
            }
        }
        Ok(Self { blocks: indexed })
    }

    pub(crate) fn resolve(&self, span: &TextSpan) -> Result<ResolvedSpan> {
        let (tokens, pages) = self.accumulate(&span.blocks, span.separator)?;
        let scalar_count = tokens.iter().filter(|token| token.is_scalar()).count();
        if span.canonical_range.start > span.canonical_range.end
            || span.comparable_range.start > span.comparable_range.end
            || span.comparable_range.end > tokens.len()
            || span.canonical_range.end > scalar_count
        {
            return Err(Error::InvalidConfiguration(
                "text span range exceeds the normalized block evidence".to_owned(),
            ));
        }
        let text = tokens
            .iter()
            .filter_map(ComparableToken::as_scalar)
            .skip(span.canonical_range.start)
            .take(span.canonical_range.end - span.canonical_range.start)
            .collect();
        let mut unmapped = Vec::new();
        let mut scalar_offset = 0_usize;
        for token in &tokens[span.comparable_range.start..span.comparable_range.end] {
            match token {
                ComparableToken::Scalar(_) => scalar_offset += 1,
                ComparableToken::Unmapped {
                    font_hash,
                    glyph_id,
                } => {
                    unmapped.push(UnmappedSpanToken {
                        scalar_offset,
                        font_hash: font_hash.clone(),
                        glyph_id: *glyph_id,
                    });
                }
            }
        }

        Ok(ResolvedSpan {
            text,
            unmapped,
            pages,
        })
    }

    pub(crate) fn block(&self, block: BlockId) -> Result<&'a BlockText> {
        self.blocks.get(&block.0).copied().ok_or_else(|| {
            Error::InvalidConfiguration(format!(
                "text span references block {} that has no normalized evidence",
                block.0
            ))
        })
    }

    /// Resolves the complete comparable-token sequence of a block group, so
    /// the presentation renderer can show bounded context around an exact
    /// change span without mutating or re-walking the comparison. Token
    /// space is authoritative here: unmapped-only changes legally carry a
    /// zero-width canonical range, so rendering from tokens keeps their
    /// placeholders inside the changed segment.
    pub(crate) fn resolve_group(
        &self,
        blocks: &[BlockId],
        separator: Option<BlockSeparator>,
    ) -> Result<ResolvedGroup> {
        let (tokens, pages) = self.accumulate(blocks, separator)?;
        Ok(ResolvedGroup { tokens, pages })
    }

    fn accumulate(
        &self,
        blocks: &[BlockId],
        separator: Option<BlockSeparator>,
    ) -> Result<(Vec<ComparableToken>, Vec<u32>)> {
        let mut tokens = Vec::new();
        let mut pages = Vec::new();
        for (position, block_id) in blocks.iter().enumerate() {
            let block = self.blocks.get(&block_id.0).ok_or_else(|| {
                Error::InvalidConfiguration(format!(
                    "text span references block {} that has no normalized evidence",
                    block_id.0
                ))
            })?;
            let next = block.canonical.comparable_tokens()?;
            if position == 0 {
                tokens.extend_from_slice(&next);
            } else {
                separator
                    .unwrap_or(BlockSeparator::Concatenate)
                    .append(&mut tokens, &next);
            }
            pages.extend_from_slice(&block.pages);
        }
        pages.sort_unstable();
        pages.dedup();
        Ok((tokens, pages))
    }
}

pub(crate) struct ResolvedSpan {
    pub text: String,
    /// Unmapped glyph tokens inside the span, in comparable-token order.
    pub unmapped: Vec<UnmappedSpanToken>,
    pub pages: Vec<u32>,
}

/// Full block-group evidence for the presentation renderer: the concatenated
/// comparable-token sequence plus deduplicated page provenance.
pub(crate) struct ResolvedGroup {
    pub tokens: Vec<ComparableToken>,
    pub pages: Vec<u32>,
}

/// One unmapped glyph token with its stable identity and the scalar offset
/// within the resolved span text where it sits, so consumers can reconstruct
/// the exact interleaving of mapped scalars and unmapped glyphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnmappedSpanToken {
    pub scalar_offset: usize,
    pub font_hash: FontProgramHash,
    pub glyph_id: u16,
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
            "{side} extraction cannot be complete with issues"
        )));
    }
    if !complete && region_count == 0 {
        return Err(Error::InvalidConfiguration(format!(
            "incomplete {side} extraction requires an issue"
        )));
    }
    Ok(())
}

fn validate_issue_scopes(
    side_name: &str,
    side: DocumentSide,
    issues: &[ExtractionIssueRecord],
) -> Result<()> {
    let side_issues = issues
        .iter()
        .filter(|issue| issue.side == side)
        .collect::<Vec<_>>();
    let mut pages = HashSet::new();
    for issue in side_issues {
        if let ExtractionScope::Page(page) = issue.scope
            && !pages.insert(page)
        {
            return Err(Error::InvalidConfiguration(format!(
                "inconsistent {side_name} extraction issue scopes: page {} is duplicated",
                page.0
            )));
        }
    }
    Ok(())
}

fn validate_comparison(comparison: &Comparison) -> Result<()> {
    for change in &comparison.changes {
        if change.occurrences.is_empty() {
            return Err(Error::InvalidConfiguration(format!(
                "{:?} changes require at least one occurrence",
                change.kind
            )));
        }
        if change.occurrences.iter().any(|occurrence| {
            !crate::diff::valid_change_occurrence_shape(
                change.kind,
                occurrence.old_span.as_ref(),
                occurrence.new_span.as_ref(),
            )
        }) {
            return Err(Error::InvalidConfiguration(format!(
                "invalid {:?} change span shape",
                change.kind
            )));
        }
        for occurrence in &change.occurrences {
            for span in occurrence.old_span.iter().chain(occurrence.new_span.iter()) {
                validate_change_span(span)?;
            }
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
    if span.blocks.len() == 1 && span.separator.is_some() {
        return Err(Error::InvalidConfiguration(format!(
            "{context} single-block text spans cannot have a block separator"
        )));
    }
    if span.blocks.len() > 1 && span.separator.is_none() {
        return Err(Error::InvalidConfiguration(format!(
            "{context} multi-block text spans require a block separator"
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

fn validate_coverage(
    side: &str,
    resolved: usize,
    total: usize,
    ratio: Option<f64>,
    extraction_complete: bool,
) -> Result<()> {
    let expected_ratio = if total == 0 {
        1.0
    } else {
        resolved as f64 / total as f64
    };
    let ratio_is_valid = if extraction_complete {
        ratio.is_some_and(|ratio| {
            ratio.is_finite()
                && (0.0..=1.0).contains(&ratio)
                && (ratio - expected_ratio).abs() <= 1.0e-12
        })
    } else {
        ratio.is_none()
    };
    if resolved > total || !ratio_is_valid {
        return Err(Error::InvalidConfiguration(format!(
            "invalid {side} alignment coverage"
        )));
    }
    Ok(())
}

pub(crate) fn percentage(ratio: Option<f64>) -> String {
    ratio.map_or_else(
        || "unknown".to_owned(),
        |ratio| format!("{:.1}%", ratio * 100.0),
    )
}

pub(crate) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(crate) fn side_name(side: DocumentSide) -> &'static str {
    match side {
        DocumentSide::Old => "old",
        DocumentSide::New => "new",
    }
}

pub(crate) fn issue_kind_name(kind: ExtractionIssueKind) -> &'static str {
    match kind {
        ExtractionIssueKind::Unsupported => "unsupported",
        ExtractionIssueKind::Unresolved => "unresolved",
    }
}

pub(crate) fn change_kind(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}

pub(crate) fn confidence(value: Confidence) -> &'static str {
    match value {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

pub(crate) fn change_tag(tag: ChangeTag) -> &'static str {
    match tag {
        ChangeTag::CharacterWidth => "character_width",
        ChangeTag::OcrConfusion => "ocr_confusion",
    }
}
