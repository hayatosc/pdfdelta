use std::{
    collections::{HashMap, HashSet},
    io::Write,
};

use serde::Serialize;

use crate::{
    Error, Result,
    alignment::{AlignmentEvidence, BlockSeparator, CandidateSource},
    diff::{
        Change, Comparison, Coverage, FormattingChange, FormattingReason, TextSpan,
        UnresolvedRegion,
    },
    model::{GlyphEvidence, GlyphId, Rect},
    normalize::{BlockText, ComparableToken, ScalarRange, TextSource, TextSourceAtom},
    source::ExtractionScope,
};

use super::{
    ExtractionStatus, ReportSummary, SideIndex, change_kind, change_tag, confidence,
    issue_kind_name, lowercase_hex, side_name, summarize,
};

const SCHEMA_VERSION: u32 = 7;

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
    let old_glyphs = GlyphEvidenceIndex::new(old_glyph_evidence)?;
    let new_glyphs = GlyphEvidenceIndex::new(new_glyph_evidence)?;
    let report = JsonReport::new(
        comparison,
        extraction,
        summary,
        &old,
        &new,
        &old_glyphs,
        &new_glyphs,
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
        old_glyphs: &GlyphEvidenceIndex<'_>,
        new_glyphs: &GlyphEvidenceIndex<'_>,
    ) -> Result<Self> {
        let changes = comparison
            .changes
            .iter()
            .map(|change| JsonChange::new(change, old, new, old_glyphs, new_glyphs))
            .collect::<Result<Vec<_>>>()?;
        let formatting_only_changes = comparison
            .formatting_changes
            .iter()
            .map(|change| JsonFormattingChange::new(change, old, new, old_glyphs, new_glyphs))
            .collect::<Result<Vec<_>>>()?;
        let unresolved_regions = comparison
            .unresolved_regions
            .iter()
            .map(|region| JsonUnresolvedRegion::new(region, old, new, old_glyphs, new_glyphs))
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
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
    confidence: &'static str,
    tags: Vec<&'static str>,
}

impl JsonChange {
    fn new(
        change: &Change,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_glyphs: &GlyphEvidenceIndex<'_>,
        new_glyphs: &GlyphEvidenceIndex<'_>,
    ) -> Result<Self> {
        Ok(Self {
            kind: change_kind(change.kind),
            old_span: change
                .old_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, old, old_glyphs))
                .transpose()?,
            new_span: change
                .new_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, new, new_glyphs))
                .transpose()?,
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
        old_glyphs: &GlyphEvidenceIndex<'_>,
        new_glyphs: &GlyphEvidenceIndex<'_>,
    ) -> Result<Self> {
        Ok(Self {
            old_span: JsonTextSpan::new(&change.old_span, old, old_glyphs)?,
            new_span: JsonTextSpan::new(&change.new_span, new, new_glyphs)?,
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
        old_glyphs: &GlyphEvidenceIndex<'_>,
        new_glyphs: &GlyphEvidenceIndex<'_>,
    ) -> Result<Self> {
        Ok(Self {
            old_span: region
                .old_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, old, old_glyphs))
                .transpose()?,
            new_span: region
                .new_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, new, new_glyphs))
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
    fn new(span: &TextSpan, side: &SideIndex<'_>, glyphs: &GlyphEvidenceIndex<'_>) -> Result<Self> {
        let resolved = side.resolve(span)?;
        let sources = project_span_sources(span, side, glyphs)?;
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

struct GlyphEvidenceIndex<'a> {
    glyphs: HashMap<GlyphId, &'a GlyphEvidence>,
}

impl<'a> GlyphEvidenceIndex<'a> {
    fn new(evidence: &'a [GlyphEvidence]) -> Result<Self> {
        let mut glyphs = HashMap::with_capacity(evidence.len());
        for glyph in evidence {
            if glyphs.insert(glyph.id, glyph).is_some() {
                return Err(Error::Report(format!(
                    "duplicate glyph evidence id {}",
                    glyph.id.0
                )));
            }
        }
        Ok(Self { glyphs })
    }

    fn resolve(&self, id: GlyphId) -> Result<&GlyphEvidence> {
        self.glyphs
            .get(&id)
            .copied()
            .ok_or_else(|| Error::Report(format!("missing glyph evidence for id {}", id.0)))
    }
}

enum ProjectedTokenSource {
    Text(TextSource),
    BlockSeparatorSpace,
}

struct ProjectedToken {
    token: ComparableToken,
    canonical_position: usize,
    source: ProjectedTokenSource,
}

struct PositionedSource {
    canonical_position: usize,
    tie_break: u8,
    sequence: usize,
    source: ProjectedTokenSource,
}

fn project_span_sources(
    span: &TextSpan,
    side: &SideIndex<'_>,
    glyphs: &GlyphEvidenceIndex<'_>,
) -> Result<Vec<JsonSpanSource>> {
    let mut tokens = Vec::new();
    let mut event_sources = Vec::new();
    let mut canonical_offset = 0;
    for (position, block_id) in span.blocks.iter().copied().enumerate() {
        let block = side.block(block_id)?;
        let block_tokens = block.canonical.comparable_tokens_with_sources()?;
        if position > 0
            && separator_inserts_space(
                span.separator.unwrap_or(BlockSeparator::Concatenate),
                tokens.last().map(|token: &ProjectedToken| &token.token),
                block_tokens.first().map(|(token, _)| token),
            )
        {
            tokens.push(ProjectedToken {
                token: ComparableToken::Scalar(' '),
                canonical_position: canonical_offset,
                source: ProjectedTokenSource::BlockSeparatorSpace,
            });
            canonical_offset += 1;
        }
        let block_scalar_count = block.canonical.text.chars().count();
        let block_start = canonical_offset;
        for (token, source) in block_tokens {
            let is_scalar = token.is_scalar();
            tokens.push(ProjectedToken {
                canonical_position: canonical_offset,
                source: ProjectedTokenSource::Text(source),
                token,
            });
            if is_scalar {
                canonical_offset += 1;
            }
        }
        for (sequence, event) in block.normalization_events.iter().enumerate() {
            if event.canonical_range.start > event.canonical_range.end
                || event.canonical_range.end > block_scalar_count
            {
                return Err(Error::InvalidConfiguration(
                    "normalization event range exceeds its normalized block".to_owned(),
                ));
            }
            let event_range = ScalarRange {
                start: block_start + event.canonical_range.start,
                end: block_start + event.canonical_range.end,
            };
            if normalization_event_selected(event_range, span.canonical_range) {
                event_sources.push(PositionedSource {
                    canonical_position: event_range.start,
                    tie_break: if event_range.start == event_range.end {
                        0
                    } else {
                        2
                    },
                    sequence,
                    source: ProjectedTokenSource::Text(event.source.clone()),
                });
            }
        }
    }
    if span.comparable_range.end > tokens.len() {
        return Err(Error::InvalidConfiguration(
            "text span range exceeds the normalized block evidence".to_owned(),
        ));
    }
    let canonical_start = tokens[..span.comparable_range.start]
        .iter()
        .filter(|token| token.token.is_scalar())
        .count();
    let canonical_end = tokens[..span.comparable_range.end]
        .iter()
        .filter(|token| token.token.is_scalar())
        .count();
    if canonical_start != span.canonical_range.start || canonical_end != span.canonical_range.end {
        return Err(Error::InvalidConfiguration(
            "text span canonical and comparable ranges select different scalar evidence".to_owned(),
        ));
    }

    let mut positioned = tokens[span.comparable_range.start..span.comparable_range.end]
        .iter()
        .enumerate()
        .map(|(sequence, token)| PositionedSource {
            canonical_position: token.canonical_position,
            tie_break: 1,
            sequence,
            source: match &token.source {
                ProjectedTokenSource::Text(source) => ProjectedTokenSource::Text(source.clone()),
                ProjectedTokenSource::BlockSeparatorSpace => {
                    ProjectedTokenSource::BlockSeparatorSpace
                }
            },
        })
        .chain(event_sources)
        .collect::<Vec<_>>();
    positioned.sort_by_key(|source| (source.canonical_position, source.tie_break, source.sequence));

    let mut output = Vec::new();
    let mut seen_atoms = HashSet::new();
    let mut seen_glyphs = HashSet::new();
    for positioned_source in positioned {
        match positioned_source.source {
            ProjectedTokenSource::BlockSeparatorSpace => {
                output.push(JsonSpanSource::BlockSeparatorSpace);
            }
            ProjectedTokenSource::Text(source) => {
                for atom in source.atoms {
                    if seen_atoms.insert(atom.clone()) {
                        match atom {
                            TextSourceAtom::Glyph(id) => {
                                push_glyph_source(&mut output, &mut seen_glyphs, id, glyphs)?;
                            }
                            TextSourceAtom::SyntheticSpace {
                                preceding,
                                following,
                            } => {
                                output.push(JsonSpanSource::SyntheticSpace {
                                    preceding_glyph_id: preceding.0,
                                    following_glyph_id: following.0,
                                });
                                push_glyph_source(
                                    &mut output,
                                    &mut seen_glyphs,
                                    preceding,
                                    glyphs,
                                )?;
                                push_glyph_source(
                                    &mut output,
                                    &mut seen_glyphs,
                                    following,
                                    glyphs,
                                )?;
                            }
                            TextSourceAtom::LineBreak {
                                preceding,
                                following,
                            } => {
                                output.push(JsonSpanSource::LineBreak {
                                    preceding_glyph_id: preceding.0,
                                    following_glyph_id: following.0,
                                });
                                push_glyph_source(
                                    &mut output,
                                    &mut seen_glyphs,
                                    preceding,
                                    glyphs,
                                )?;
                                push_glyph_source(
                                    &mut output,
                                    &mut seen_glyphs,
                                    following,
                                    glyphs,
                                )?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(output)
}

fn separator_inserts_space(
    separator: BlockSeparator,
    previous: Option<&ComparableToken>,
    next: Option<&ComparableToken>,
) -> bool {
    let mut boundary = previous.cloned().into_iter().collect::<Vec<_>>();
    let unseparated_len = boundary.len() + usize::from(next.is_some());
    separator.append(&mut boundary, next.map_or(&[], std::slice::from_ref));
    boundary.len() > unseparated_len
}

/// Selects deleted or transformed normalization evidence without attributing
/// a boundary-only event to an adjacent nonempty change.
fn normalization_event_selected(event: ScalarRange, span: ScalarRange) -> bool {
    if span.start == span.end {
        return span.start == event.start || span.start == event.end;
    }
    if event.start == event.end {
        return span.start < event.start && event.start < span.end;
    }
    event.start < span.end && event.end > span.start
}

fn push_glyph_source(
    output: &mut Vec<JsonSpanSource>,
    seen: &mut HashSet<GlyphId>,
    id: GlyphId,
    glyphs: &GlyphEvidenceIndex<'_>,
) -> Result<()> {
    if seen.insert(id) {
        output.push(JsonSpanSource::glyph(glyphs.resolve(id)?));
    }
    Ok(())
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

impl JsonSpanSource {
    fn glyph(glyph: &GlyphEvidence) -> Self {
        Self::Glyph {
            glyph_id: glyph.id.0,
            page: glyph.page.0,
            bbox: glyph.bbox.into(),
            content_stream: JsonObjectRef {
                object_number: glyph.provenance.content_stream.object_number,
                generation: glyph.provenance.content_stream.generation,
            },
            operator_index: glyph.provenance.operator_index,
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
