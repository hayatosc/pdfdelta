use std::io::Write;

use serde::Serialize;

use crate::{
    Error, Result,
    alignment::{AlignmentEvidence, BlockSeparator, CandidateSource},
    diff::{
        ChangeEvent, Comparison, Coverage, FormattingChange, FormattingReason, TextSpan,
        UnresolvedRegion,
    },
    model::{GlyphEvidence, Rect},
    normalize::BlockText,
    source::ExtractionScope,
};

use super::{
    ExtractionStatus, ReportSummary, SideIndex, SpanSourceEvidence, SpanSourceProjectionLimits,
    SpanSourceProjector, change_kind, change_tag, confidence, issue_kind_name, lowercase_hex,
    side_name, summarize,
};

const SCHEMA_VERSION: u32 = 8;

pub fn write_json<W: Write>(
    mut writer: W,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<()> {
    let summary = summarize(comparison, extraction)?;
    let old = SideIndex::new(old_blocks)?;
    let new = SideIndex::new(new_blocks)?;
    let old_sources = SpanSourceProjector::new(
        old_blocks,
        old_glyph_evidence,
        SpanSourceProjectionLimits::default(),
    )?;
    let new_sources = SpanSourceProjector::new(
        new_blocks,
        new_glyph_evidence,
        SpanSourceProjectionLimits::default(),
    )?;
    let report = JsonReport::new(
        comparison,
        extraction,
        summary,
        &old,
        &new,
        &old_sources,
        &new_sources,
    )?;
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
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &SpanSourceProjector<'_>,
        new_sources: &SpanSourceProjector<'_>,
    ) -> Result<Self> {
        let changes = comparison
            .changes
            .iter()
            .map(|change| JsonChange::new(change, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        let formatting_only_changes = comparison
            .formatting_changes
            .iter()
            .map(|change| JsonFormattingChange::new(change, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        let unresolved_regions = comparison
            .unresolved_regions
            .iter()
            .map(|region| JsonUnresolvedRegion::new(region, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            summary: JsonSummary::new(summary, comparison),
            changes,
            formatting_only_changes,
            unresolved_regions,
            extraction: JsonExtraction::new(extraction),
        })
    }
}

#[derive(Serialize)]
struct JsonSummary {
    content_changes: usize,
    formatting_only_changes: usize,
    uncertain_changes: usize,
    unresolved_regions: usize,
    unsupported_extraction_issues: usize,
    unresolved_extraction_issues: usize,
    comparison_complete: bool,
    old_alignment_coverage: JsonCoverage,
    new_alignment_coverage: JsonCoverage,
    comparison_coverage_ratio: Option<f64>,
}

impl JsonSummary {
    fn new(summary: ReportSummary, comparison: &Comparison) -> Self {
        Self {
            content_changes: summary.content_changes,
            formatting_only_changes: summary.formatting_only_changes,
            uncertain_changes: summary.uncertain_changes,
            unresolved_regions: summary.unresolved_regions,
            unsupported_extraction_issues: summary.unsupported_extraction_issues,
            unresolved_extraction_issues: summary.unresolved_extraction_issues,
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
    ratio: Option<f64>,
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
    issues: Vec<JsonExtractionIssue<'a>>,
}

impl<'a> JsonExtraction<'a> {
    fn new(extraction: &'a ExtractionStatus) -> Self {
        Self {
            old_complete: extraction.old_complete,
            new_complete: extraction.new_complete,
            issues: extraction
                .issues
                .iter()
                .map(|issue| JsonExtractionIssue {
                    side: side_name(issue.side),
                    kind: issue_kind_name(issue.kind),
                    scope: match issue.scope {
                        ExtractionScope::Document => "document",
                        ExtractionScope::Page(_) => "page",
                        ExtractionScope::PageGap { .. } => "page_gap",
                        ExtractionScope::GlyphGap { .. } => "glyph_gap",
                    },
                    page: match issue.scope {
                        ExtractionScope::Document
                        | ExtractionScope::PageGap { .. }
                        | ExtractionScope::GlyphGap { .. } => None,
                        ExtractionScope::Page(page) => Some(page.0),
                    },
                    retained_pages_before: match issue.scope {
                        ExtractionScope::PageGap { retained_before } => Some(retained_before),
                        ExtractionScope::Document
                        | ExtractionScope::Page(_)
                        | ExtractionScope::GlyphGap { .. } => None,
                    },
                    retained_glyphs_before: match issue.scope {
                        ExtractionScope::GlyphGap { retained_before } => Some(retained_before),
                        ExtractionScope::Document
                        | ExtractionScope::Page(_)
                        | ExtractionScope::PageGap { .. } => None,
                    },
                    description: &issue.description,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonExtractionIssue<'a> {
    side: &'static str,
    kind: &'static str,
    scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained_pages_before: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained_glyphs_before: Option<usize>,
    description: &'a str,
}

#[derive(Serialize)]
struct JsonChange {
    kind: &'static str,
    occurrences: Vec<JsonChangeOccurrence>,
    confidence: &'static str,
    tags: Vec<&'static str>,
}

#[derive(Serialize)]
struct JsonChangeOccurrence {
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
}

impl JsonChange {
    fn new(
        change: &ChangeEvent,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &SpanSourceProjector<'_>,
        new_sources: &SpanSourceProjector<'_>,
    ) -> Result<Self> {
        Ok(Self {
            kind: change_kind(change.kind),
            occurrences: change
                .occurrences
                .iter()
                .map(|occurrence| {
                    Ok(JsonChangeOccurrence {
                        old_span: occurrence
                            .old_span
                            .as_ref()
                            .map(|span| JsonTextSpan::new(span, old, old_sources))
                            .transpose()?,
                        new_span: occurrence
                            .new_span
                            .as_ref()
                            .map(|span| JsonTextSpan::new(span, new, new_sources))
                            .transpose()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            confidence: confidence(change.confidence),
            tags: change.tags.iter().copied().map(change_tag).collect(),
        })
    }
}

#[derive(Serialize)]
struct JsonFormattingChange {
    old_span: JsonTextSpan,
    new_span: JsonTextSpan,
    confidence: &'static str,
    reasons: Vec<&'static str>,
}

impl JsonFormattingChange {
    fn new(
        change: &FormattingChange,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &SpanSourceProjector<'_>,
        new_sources: &SpanSourceProjector<'_>,
    ) -> Result<Self> {
        Ok(Self {
            old_span: JsonTextSpan::new(&change.old_span, old, old_sources)?,
            new_span: JsonTextSpan::new(&change.new_span, new, new_sources)?,
            confidence: confidence(change.confidence),
            reasons: change
                .reasons
                .iter()
                .copied()
                .map(formatting_reason)
                .collect(),
        })
    }
}

#[derive(Serialize)]
struct JsonUnresolvedRegion {
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
    evidence: Vec<String>,
}

impl JsonUnresolvedRegion {
    fn new(
        region: &UnresolvedRegion,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &SpanSourceProjector<'_>,
        new_sources: &SpanSourceProjector<'_>,
    ) -> Result<Self> {
        Ok(Self {
            old_span: region
                .old_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, old, old_sources))
                .transpose()?,
            new_span: region
                .new_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, new, new_sources))
                .transpose()?,
            evidence: region.evidence.iter().copied().map(evidence).collect(),
        })
    }
}

#[derive(Serialize)]
struct JsonTextSpan {
    blocks: Vec<u64>,
    pages: Vec<u32>,
    block_separator: Option<&'static str>,
    canonical_range: JsonRange,
    comparable_range: JsonRange,
    text: String,
    unmapped_tokens: Vec<JsonUnmappedToken>,
    sources: Vec<JsonSpanSource>,
}

impl JsonTextSpan {
    fn new(
        span: &TextSpan,
        side: &SideIndex<'_>,
        sources: &SpanSourceProjector<'_>,
    ) -> Result<Self> {
        let resolved = side.resolve(span)?;
        let sources = sources
            .project(span)?
            .into_iter()
            .map(JsonSpanSource::from)
            .collect();
        Ok(Self {
            blocks: span.blocks.iter().map(|block| block.0).collect(),
            pages: resolved.pages,
            block_separator: span.separator.map(block_separator),
            canonical_range: JsonRange {
                start: span.canonical_range.start,
                end: span.canonical_range.end,
            },
            comparable_range: JsonRange {
                start: span.comparable_range.start,
                end: span.comparable_range.end,
            },
            text: resolved.text,
            unmapped_tokens: resolved
                .unmapped
                .iter()
                .map(|token| JsonUnmappedToken {
                    scalar_offset: token.scalar_offset,
                    font_hash: lowercase_hex(&token.font_hash.0),
                    glyph_id: token.glyph_id,
                })
                .collect(),
            sources,
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsonSpanSource {
    Glyph {
        glyph_id: u64,
        page: u32,
        bbox: JsonRect,
        content_stream: JsonObjectRef,
        operator_index: u32,
    },
    SyntheticSpace {
        preceding_glyph_id: u64,
        following_glyph_id: u64,
    },
    LineBreak {
        preceding_glyph_id: u64,
        following_glyph_id: u64,
    },
    BlockSeparatorSpace,
}

impl From<SpanSourceEvidence> for JsonSpanSource {
    fn from(source: SpanSourceEvidence) -> Self {
        match source {
            SpanSourceEvidence::Glyph {
                glyph_id,
                page,
                bbox,
                content_stream,
                operator_index,
            } => Self::Glyph {
                glyph_id: glyph_id.0,
                page: page.0,
                bbox: bbox.into(),
                content_stream: JsonObjectRef {
                    object_number: content_stream.object_number,
                    generation: content_stream.generation,
                },
                operator_index,
            },
            SpanSourceEvidence::SyntheticSpace {
                preceding_glyph_id,
                following_glyph_id,
            } => Self::SyntheticSpace {
                preceding_glyph_id: preceding_glyph_id.0,
                following_glyph_id: following_glyph_id.0,
            },
            SpanSourceEvidence::LineBreak {
                preceding_glyph_id,
                following_glyph_id,
            } => Self::LineBreak {
                preceding_glyph_id: preceding_glyph_id.0,
                following_glyph_id: following_glyph_id.0,
            },
            SpanSourceEvidence::BlockSeparatorSpace => Self::BlockSeparatorSpace,
        }
    }
}

#[derive(Serialize)]
struct JsonRect {
    min: JsonPoint,
    max: JsonPoint,
}

impl From<Rect> for JsonRect {
    fn from(rect: Rect) -> Self {
        Self {
            min: JsonPoint {
                x: rect.min.x,
                y: rect.min.y,
            },
            max: JsonPoint {
                x: rect.max.x,
                y: rect.max.y,
            },
        }
    }
}

#[derive(Serialize)]
struct JsonPoint {
    x: f64,
    y: f64,
}

#[derive(Serialize)]
struct JsonObjectRef {
    object_number: u32,
    generation: u16,
}

/// Stable, lossless identity of one unmapped glyph token inside the span.
#[derive(Serialize)]
struct JsonUnmappedToken {
    /// Scalar offset within `text` where this glyph sits (0 = before all
    /// scalars; equal to `text` char count = after all scalars).
    scalar_offset: usize,
    font_hash: String,
    glyph_id: u16,
}

fn block_separator(separator: BlockSeparator) -> &'static str {
    match separator {
        BlockSeparator::Concatenate => "concatenate",
        BlockSeparator::Space => "space",
    }
}

#[derive(Serialize)]
struct JsonRange {
    start: usize,
    end: usize,
}

fn formatting_reason(reason: FormattingReason) -> &'static str {
    match reason {
        FormattingReason::Normalization => "normalization",
        FormattingReason::BlockStructure => "block_structure",
        FormattingReason::FontSize => "font_size",
        FormattingReason::Position => "position",
        FormattingReason::LineBreak => "line_break",
        FormattingReason::PageBreak => "page_break",
    }
}

fn evidence(value: AlignmentEvidence) -> String {
    match value {
        AlignmentEvidence::ExactCanonical => "exact_canonical".to_owned(),
        AlignmentEvidence::TextSimilarity => "text_similarity".to_owned(),
        AlignmentEvidence::CandidateSetEmpty => "candidate_set_empty".to_owned(),
        AlignmentEvidence::CandidateScoringRejected => "candidate_scoring_rejected".to_owned(),
        AlignmentEvidence::CandidateCompetition => "candidate_competition".to_owned(),
        AlignmentEvidence::DiffEditDistanceExceeded => "diff_edit_distance_exceeded".to_owned(),
        AlignmentEvidence::DiffRejectedAsImplausible => "diff_rejected_as_implausible".to_owned(),
        AlignmentEvidence::Anchor => "anchor".to_owned(),
        AlignmentEvidence::AnchorInterval => "anchor_interval".to_owned(),
        AlignmentEvidence::NeighborConsistency => "neighbor_consistency".to_owned(),
        AlignmentEvidence::NumericMask => "numeric_mask".to_owned(),
        AlignmentEvidence::SplitMerge => "split_merge".to_owned(),
        AlignmentEvidence::NormalizationIssue => "normalization_issue".to_owned(),
        AlignmentEvidence::ExtractionGap => "extraction_gap".to_owned(),
        AlignmentEvidence::ReadingOrderUnknown => "reading_order_unknown".to_owned(),
        AlignmentEvidence::MoveCandidate => "move_candidate".to_owned(),
        AlignmentEvidence::CandidateSource(source) => {
            format!("candidate_source:{}", candidate_source(source))
        }
    }
}

/// Comma-joined human labels for the evidence recorded on one unresolved
/// region; the JSON report keeps the structured list instead.
pub(crate) fn evidence_label_list(items: &[AlignmentEvidence]) -> String {
    items
        .iter()
        .map(|value| evidence(*value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn candidate_source(source: CandidateSource) -> &'static str {
    match source {
        CandidateSource::Exact => "exact",
        CandidateSource::NGramInvertedIndex => "ngram_inverted_index",
        CandidateSource::MinHashLsh => "minhash_lsh",
        CandidateSource::ShortBlockFallback => "short_block_fallback",
        CandidateSource::Exhaustive => "exhaustive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_evidence_uses_snake_case_json_strings() {
        assert_eq!(
            evidence(AlignmentEvidence::CandidateSetEmpty),
            "candidate_set_empty"
        );
        assert_eq!(
            evidence(AlignmentEvidence::CandidateScoringRejected),
            "candidate_scoring_rejected"
        );
        assert_eq!(
            evidence(AlignmentEvidence::CandidateCompetition),
            "candidate_competition"
        );
        assert_eq!(
            evidence(AlignmentEvidence::DiffEditDistanceExceeded),
            "diff_edit_distance_exceeded"
        );
        assert_eq!(
            evidence(AlignmentEvidence::DiffRejectedAsImplausible),
            "diff_rejected_as_implausible"
        );
    }
}
