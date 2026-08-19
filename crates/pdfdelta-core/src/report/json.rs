use std::io::Write;

use serde::Serialize;

use crate::{
    Error, Result,
    alignment::{AlignmentEvidence, CandidateSource},
    diff::{
        Change, ChangeKind, ChangeTag, Comparison, Confidence, Coverage, FormattingChange,
        FormattingReason, TextSpan, UnresolvedRegion,
    },
};

use super::{ExtractionStatus, ReportSummary, side_name, summarize};

const SCHEMA_VERSION: u32 = 1;

pub fn write_json<W: Write>(
    mut writer: W,
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<()> {
    let summary = summarize(comparison, extraction)?;
    let report = JsonReport::new(comparison, extraction, summary);
    serde_json::to_writer_pretty(&mut writer, &report)
        .map_err(|error| Error::Report(error.to_string()))?;
    writer
        .write_all(b"\n")
        .map_err(|error| Error::Report(error.to_string()))
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u32,
    summary: JsonSummary,
    changes: Vec<JsonChange>,
    formatting_only_changes: Vec<JsonFormattingChange>,
    unresolved_regions: Vec<JsonUnresolvedRegion>,
    extraction: JsonExtraction<'a>,
}

impl<'a> JsonReport<'a> {
    fn new(
        comparison: &Comparison,
        extraction: &'a ExtractionStatus,
        summary: ReportSummary,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            summary: JsonSummary::new(summary, comparison),
            changes: comparison.changes.iter().map(Into::into).collect(),
            formatting_only_changes: comparison
                .formatting_changes
                .iter()
                .map(Into::into)
                .collect(),
            unresolved_regions: comparison
                .unresolved_regions
                .iter()
                .map(Into::into)
                .collect(),
            extraction: JsonExtraction::new(extraction),
        }
    }
}

#[derive(Serialize)]
struct JsonSummary {
    content_changes: usize,
    formatting_only_changes: usize,
    uncertain_changes: usize,
    unresolved_regions: usize,
    unsupported_regions: usize,
    comparison_complete: bool,
    old_alignment_coverage: JsonCoverage,
    new_alignment_coverage: JsonCoverage,
    comparison_coverage_ratio: f64,
}

impl JsonSummary {
    fn new(summary: ReportSummary, comparison: &Comparison) -> Self {
        Self {
            content_changes: summary.content_changes,
            formatting_only_changes: summary.formatting_only_changes,
            uncertain_changes: summary.uncertain_changes,
            unresolved_regions: summary.unresolved_regions,
            unsupported_regions: summary.unsupported_regions,
            comparison_complete: summary.comparison_complete,
            old_alignment_coverage: comparison.old_coverage.into(),
            new_alignment_coverage: comparison.new_coverage.into(),
            comparison_coverage_ratio: summary.comparison_coverage,
        }
    }
}

#[derive(Serialize)]
struct JsonCoverage {
    resolved_tokens: usize,
    total_tokens: usize,
    ratio: f64,
}

impl From<Coverage> for JsonCoverage {
    fn from(coverage: Coverage) -> Self {
        Self {
            resolved_tokens: coverage.resolved_tokens,
            total_tokens: coverage.total_tokens,
            ratio: coverage.ratio,
        }
    }
}

#[derive(Serialize)]
struct JsonExtraction<'a> {
    old_complete: bool,
    new_complete: bool,
    unsupported_regions: Vec<JsonUnsupportedRegion<'a>>,
}

impl<'a> JsonExtraction<'a> {
    fn new(extraction: &'a ExtractionStatus) -> Self {
        Self {
            old_complete: extraction.old_complete,
            new_complete: extraction.new_complete,
            unsupported_regions: extraction
                .unsupported_regions
                .iter()
                .map(|region| JsonUnsupportedRegion {
                    side: side_name(region.side),
                    description: &region.description,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonUnsupportedRegion<'a> {
    side: &'static str,
    description: &'a str,
}

#[derive(Serialize)]
struct JsonChange {
    kind: &'static str,
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
    confidence: &'static str,
    tags: Vec<&'static str>,
}

impl From<&Change> for JsonChange {
    fn from(change: &Change) -> Self {
        Self {
            kind: change_kind(change.kind),
            old_span: change.old_span.as_ref().map(Into::into),
            new_span: change.new_span.as_ref().map(Into::into),
            confidence: confidence(change.confidence),
            tags: change.tags.iter().copied().map(change_tag).collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonFormattingChange {
    old_span: JsonTextSpan,
    new_span: JsonTextSpan,
    confidence: &'static str,
    reasons: Vec<&'static str>,
}

impl From<&FormattingChange> for JsonFormattingChange {
    fn from(change: &FormattingChange) -> Self {
        Self {
            old_span: (&change.old_span).into(),
            new_span: (&change.new_span).into(),
            confidence: confidence(change.confidence),
            reasons: change
                .reasons
                .iter()
                .copied()
                .map(formatting_reason)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonUnresolvedRegion {
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
    evidence: Vec<String>,
}

impl From<&UnresolvedRegion> for JsonUnresolvedRegion {
    fn from(region: &UnresolvedRegion) -> Self {
        Self {
            old_span: region.old_span.as_ref().map(Into::into),
            new_span: region.new_span.as_ref().map(Into::into),
            evidence: region.evidence.iter().copied().map(evidence).collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonTextSpan {
    blocks: Vec<u64>,
    canonical_range: JsonRange,
    comparable_range: JsonRange,
}

impl From<&TextSpan> for JsonTextSpan {
    fn from(span: &TextSpan) -> Self {
        Self {
            blocks: span.blocks.iter().map(|block| block.0).collect(),
            canonical_range: JsonRange {
                start: span.canonical_range.start,
                end: span.canonical_range.end,
            },
            comparable_range: JsonRange {
                start: span.comparable_range.start,
                end: span.comparable_range.end,
            },
        }
    }
}

#[derive(Serialize)]
struct JsonRange {
    start: usize,
    end: usize,
}

fn change_kind(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}

fn confidence(value: Confidence) -> &'static str {
    match value {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

fn change_tag(tag: ChangeTag) -> &'static str {
    match tag {
        ChangeTag::CharacterWidth => "character_width",
        ChangeTag::OcrConfusion => "ocr_confusion",
    }
}

fn formatting_reason(reason: FormattingReason) -> &'static str {
    match reason {
        FormattingReason::Normalization => "normalization",
        FormattingReason::BlockStructure => "block_structure",
    }
}

fn evidence(value: AlignmentEvidence) -> String {
    match value {
        AlignmentEvidence::ExactCanonical => "exact_canonical".to_owned(),
        AlignmentEvidence::TextSimilarity => "text_similarity".to_owned(),
        AlignmentEvidence::Anchor => "anchor".to_owned(),
        AlignmentEvidence::AnchorInterval => "anchor_interval".to_owned(),
        AlignmentEvidence::NeighborConsistency => "neighbor_consistency".to_owned(),
        AlignmentEvidence::NumericMask => "numeric_mask".to_owned(),
        AlignmentEvidence::SplitMerge => "split_merge".to_owned(),
        AlignmentEvidence::NormalizationIssue => "normalization_issue".to_owned(),
        AlignmentEvidence::MoveCandidate => "move_candidate".to_owned(),
        AlignmentEvidence::CandidateSource(source) => {
            format!("candidate_source:{}", candidate_source(source))
        }
    }
}

fn candidate_source(source: CandidateSource) -> &'static str {
    match source {
        CandidateSource::Exact => "exact",
        CandidateSource::NGramInvertedIndex => "ngram_inverted_index",
        CandidateSource::ShortBlockFallback => "short_block_fallback",
        CandidateSource::Exhaustive => "exhaustive",
    }
}
