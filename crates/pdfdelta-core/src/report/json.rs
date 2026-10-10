use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use serde::{Serialize, Serializer, ser::SerializeSeq};
use sha2::{Digest, Sha256};

use crate::{
    Error, Result,
    alignment::{AlignmentEvidence, BlockSeparator, CandidateSource},
    diff::{
        ChangeCandidate, ChangeEvent, ChangedRegionProof, Comparison, Coverage, FormattingChange,
        FormattingReason, ProvenChangedRegion, RelationAssessment, ResolutionRange,
        ResolutionState, TextSpan, UnresolvedRegion,
    },
    model::{GlyphEvidence, Rect},
    normalize::BlockText,
    source::ExtractionScope,
};

use super::{
    ExtractionStatus, ReportSummary, SideIndex, SpanSourceEvidence, SpanSourceProjectionLimits,
    SpanSourceProjector, assessment_reason, assumption, change_kind, change_tag, confidence,
    issue_kind_name, lowercase_hex, relation_outcome, search_completeness, side_name, summarize,
};

const SCHEMA_VERSION: u32 = 11;

pub fn write_json<W: Write>(
    mut writer: W,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<()> {
    let report = build_report(
        old_blocks,
        new_blocks,
        old_glyph_evidence,
        new_glyph_evidence,
        comparison,
        extraction,
        false,
    )?;
    serde_json::to_writer_pretty(&mut writer, &report)
        .map_err(|error| Error::Report(error.to_string()))?;
    writer
        .write_all(b"\n")
        .map_err(|error| Error::Report(error.to_string()))
}

/// Writes compact native diagnostics without constructing a full JSON report.
///
/// The version-2 compact envelope preserves every semantic report field. Each
/// source array becomes counts, pages, first/last samples, and a SHA-256 of its
/// compact JSON serialization in native field order. Source arrays are released
/// after each projection instead of being retained throughout the report.
/// The hash identifies source evidence, not a hypothetical full report file.
///
/// # Errors
///
/// Returns the same validation and projection errors as [`write_json`], or a
/// report error if serialization or writing fails.
pub fn write_compact_json<W: Write>(
    mut writer: W,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<()> {
    let report = build_report(
        old_blocks,
        new_blocks,
        old_glyph_evidence,
        new_glyph_evidence,
        comparison,
        extraction,
        true,
    )?;
    let artifact = JsonCompactArtifact {
        artifact_format: "pdfdelta-native-compact",
        artifact_version: 2,
        source_schema_version: SCHEMA_VERSION,
        omitted_fields: JsonOmittedFields {
            sources: "source_summary; full glyph geometry and operator provenance require regeneration",
        },
        report,
    };
    serde_json::to_writer(&mut writer, &artifact)
        .map_err(|error| Error::Report(error.to_string()))?;
    writer
        .write_all(b"\n")
        .map_err(|error| Error::Report(error.to_string()))
}

fn build_report<'a>(
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    comparison: &Comparison,
    extraction: &'a ExtractionStatus,
    compact: bool,
) -> Result<JsonReport<'a>> {
    let summary = summarize(comparison, extraction)?;
    let old = SideIndex::new(old_blocks)?;
    let new = SideIndex::new(new_blocks)?;
    let old_sources = ReportSources {
        projector: SpanSourceProjector::new(
            old_blocks,
            old_glyph_evidence,
            SpanSourceProjectionLimits::default(),
        )?,
        compact,
    };
    let new_sources = ReportSources {
        projector: SpanSourceProjector::new(
            new_blocks,
            new_glyph_evidence,
            SpanSourceProjectionLimits::default(),
        )?,
        compact,
    };
    JsonReport::new(
        comparison,
        extraction,
        summary,
        &old,
        &new,
        &old_sources,
        &new_sources,
    )
}

#[derive(Serialize)]
struct JsonCompactArtifact<'a> {
    artifact_format: &'static str,
    artifact_version: u32,
    source_schema_version: u32,
    omitted_fields: JsonOmittedFields,
    report: JsonReport<'a>,
}

#[derive(Serialize)]
struct JsonOmittedFields {
    sources: &'static str,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u32,
    difference_status: &'static str,
    comparison_scope: JsonComparisonScope,
    assessment: Option<JsonAssessment>,
    summary: JsonSummary,
    changes: Vec<JsonChange>,
    change_candidates: Vec<JsonChangeCandidate>,
    proven_changed_regions: Vec<JsonProvenChangedRegion>,
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
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
    ) -> Result<Self> {
        let changes = comparison
            .changes
            .iter()
            .map(|change| JsonChange::new(change, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        let change_candidates = comparison
            .change_candidates
            .iter()
            .map(|candidate| {
                JsonChangeCandidate::new(candidate, comparison, old, new, old_sources, new_sources)
            })
            .collect::<Result<Vec<_>>>()?;
        let formatting_only_changes = comparison
            .formatting_changes
            .iter()
            .map(|change| JsonFormattingChange::new(change, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        let proven_changed_regions = comparison
            .proven_changed_regions
            .iter()
            .map(|region| JsonProvenChangedRegion::new(region, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        let unresolved_regions = comparison
            .unresolved_regions
            .iter()
            .map(|region| JsonUnresolvedRegion::new(region, old, new, old_sources, new_sources))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            difference_status: summary.difference_status.as_str(),
            comparison_scope: summary.comparison_scope.into(),
            assessment: comparison
                .assessment
                .as_ref()
                .map(|assessment| {
                    JsonAssessment::new(assessment, old, new, old_sources, new_sources)
                })
                .transpose()?,
            summary: JsonSummary::new(summary, comparison),
            changes,
            change_candidates,
            proven_changed_regions,
            formatting_only_changes,
            unresolved_regions,
            extraction: JsonExtraction::new(extraction),
        })
    }
}

#[derive(Serialize)]
struct JsonSummary {
    established_changes: usize,
    content_changes: usize,
    proven_changed_regions: usize,
    formatting_only_changes: usize,
    uncertain_changes: usize,
    unresolved_regions: usize,
    unsupported_extraction_issues: usize,
    unresolved_extraction_issues: usize,
    comparison_complete: bool,
    difference_status: &'static str,
    comparison_scope: JsonComparisonScope,
    tentative_candidates: usize,
    old_alignment_coverage: JsonCoverage,
    new_alignment_coverage: JsonCoverage,
    comparison_coverage_ratio: Option<f64>,
}

impl JsonSummary {
    fn new(summary: ReportSummary, comparison: &Comparison) -> Self {
        Self {
            established_changes: summary.established_changes,
            content_changes: summary.content_changes,
            proven_changed_regions: summary.proven_changed_regions,
            formatting_only_changes: summary.formatting_only_changes,
            uncertain_changes: summary.uncertain_changes,
            unresolved_regions: summary.unresolved_regions,
            unsupported_extraction_issues: summary.unsupported_extraction_issues,
            unresolved_extraction_issues: summary.unresolved_extraction_issues,
            comparison_complete: summary.comparison_complete,
            difference_status: summary.difference_status.as_str(),
            comparison_scope: summary.comparison_scope.into(),
            tentative_candidates: summary.tentative_candidates,
            old_alignment_coverage: comparison.old_coverage.into(),
            new_alignment_coverage: comparison.new_coverage.into(),
            comparison_coverage_ratio: summary.comparison_coverage,
        }
    }
}

#[derive(Serialize)]
struct JsonComparisonScope {
    supported_text: bool,
    images_compared: bool,
}

impl From<super::ComparisonScope> for JsonComparisonScope {
    fn from(scope: super::ComparisonScope) -> Self {
        Self {
            supported_text: scope.supported_text,
            images_compared: scope.images_compared,
        }
    }
}

#[derive(Serialize)]
struct JsonAssessment {
    policy_version: u32,
    work_limit: usize,
    work_used: usize,
    work_by_stage: JsonAssessmentWork,
    anchor_work: JsonAnchorWork,
    local_view_work: JsonLocalViewWork,
    #[serde(skip_serializing_if = "Option::is_none")]
    suffix_reuse_work: Option<JsonSuffixReuseWork>,
    candidates_truncated: bool,
    old_resolution: Vec<JsonResolutionRange>,
    new_resolution: Vec<JsonResolutionRange>,
    relations: Vec<JsonRelationAssessment>,
    review_units: Vec<JsonReviewUnit>,
}

#[derive(Serialize)]
struct JsonEditCountBounds {
    lower: usize,
    upper: usize,
}

impl From<crate::diff::EditCountBounds> for JsonEditCountBounds {
    fn from(bounds: crate::diff::EditCountBounds) -> Self {
        Self {
            lower: bounds.lower,
            upper: bounds.upper,
        }
    }
}

#[derive(Serialize)]
struct JsonReviewUnit {
    relation: usize,
    alignment_policy: &'static str,
    search: &'static str,
    normalization_hypotheses: usize,
    normalization_old: Vec<JsonTextSpan>,
    normalization_new: Vec<JsonTextSpan>,
    changed_count: Option<JsonEditCountBounds>,
    unresolved_changed_count: Option<JsonEditCountBounds>,
    mandatory_old: Vec<JsonTextSpan>,
    mandatory_new: Vec<JsonTextSpan>,
}

impl JsonAssessment {
    fn new(
        assessment: &crate::diff::ComparisonAssessment,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
    ) -> Result<Self> {
        Ok(Self {
            policy_version: assessment.policy_version,
            work_limit: assessment.work_limit,
            work_used: assessment.work_used,
            work_by_stage: assessment.work_by_stage.into(),
            anchor_work: assessment.anchor_work.into(),
            local_view_work: assessment.local_view_work.into(),
            suffix_reuse_work: (assessment.suffix_reuse_work.attempts != 0)
                .then_some(assessment.suffix_reuse_work.into()),
            candidates_truncated: assessment.candidates_truncated,
            review_units: assessment
                .review_units
                .iter()
                .map(|unit| {
                    Ok(JsonReviewUnit {
                        relation: unit.relation,
                        alignment_policy: match unit.policy {
                            crate::diff::AlignmentPolicy::LiteralMinimal => "literal_minimal",
                        },
                        search: search_completeness(unit.search),
                        normalization_hypotheses: unit.normalization_hypotheses,
                        normalization_old: unit
                            .normalization_old
                            .iter()
                            .map(|span| JsonTextSpan::new(span, old, old_sources))
                            .collect::<Result<Vec<_>>>()?,
                        normalization_new: unit
                            .normalization_new
                            .iter()
                            .map(|span| JsonTextSpan::new(span, new, new_sources))
                            .collect::<Result<Vec<_>>>()?,
                        changed_count: unit.changed_count.map(Into::into),
                        unresolved_changed_count: unit.unresolved_changed_count.map(Into::into),
                        mandatory_old: unit
                            .mandatory_old
                            .iter()
                            .map(|span| JsonTextSpan::new(span, old, old_sources))
                            .collect::<Result<Vec<_>>>()?,
                        mandatory_new: unit
                            .mandatory_new
                            .iter()
                            .map(|span| JsonTextSpan::new(span, new, new_sources))
                            .collect::<Result<Vec<_>>>()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            old_resolution: assessment
                .old_resolution
                .iter()
                .map(|range| JsonResolutionRange::new(range, old, old_sources))
                .collect::<Result<Vec<_>>>()?,
            new_resolution: assessment
                .new_resolution
                .iter()
                .map(|range| JsonResolutionRange::new(range, new, new_sources))
                .collect::<Result<Vec<_>>>()?,
            relations: assessment
                .relations
                .iter()
                .map(|relation| {
                    JsonRelationAssessment::new(relation, old, new, old_sources, new_sources)
                })
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

#[derive(Serialize)]
struct JsonLocalViewWork {
    budget_cap: usize,
    settlement_reserve: usize,
    cap_exhausted: bool,
    refused_request_size: Option<usize>,
    source_issue_index: usize,
    build_views: usize,
    seed_search: usize,
    explicit_anchor_search: usize,
    domain_construction: usize,
    footer_search: usize,
}

impl From<crate::diff::LocalViewWork> for JsonLocalViewWork {
    fn from(work: crate::diff::LocalViewWork) -> Self {
        Self {
            budget_cap: work.budget_cap,
            settlement_reserve: work.settlement_reserve,
            cap_exhausted: work.cap_exhausted,
            refused_request_size: work.refused_request_size,
            source_issue_index: work.source_issue_index,
            build_views: work.build_views,
            seed_search: work.seed_search,
            explicit_anchor_search: work.explicit_anchor_search,
            domain_construction: work.domain_construction,
            footer_search: work.footer_search,
        }
    }
}

#[derive(Serialize)]
struct JsonAnchorWork {
    candidate_index: usize,
    occurrence_index: usize,
    occurrence_comparison: usize,
    order_uniqueness: usize,
    starts_examined: usize,
    budget_exhausted: bool,
    refused_request: usize,
    refused_remainder: usize,
    order_unique: Option<bool>,
    verified_anchors: usize,
    sidecar_index: usize,
    sidecar_refused_request: usize,
}

impl From<crate::diff::AnchorWork> for JsonAnchorWork {
    fn from(work: crate::diff::AnchorWork) -> Self {
        Self {
            candidate_index: work.candidate_index,
            occurrence_index: work.occurrence_index,
            occurrence_comparison: work.occurrence_comparison,
            order_uniqueness: work.order_uniqueness,
            starts_examined: work.starts_examined,
            budget_exhausted: work.budget_exhausted,
            refused_request: work.refused_request,
            refused_remainder: work.refused_remainder,
            order_unique: work.order_unique,
            verified_anchors: work.verified_anchors,
            sidecar_index: work.sidecar_index,
            sidecar_refused_request: work.sidecar_refused_request,
        }
    }
}

#[derive(Serialize)]
struct JsonSuffixReuseWork {
    attempts: usize,
    allowance_used: usize,
    recomputed_cells: usize,
    binding_work: usize,
    accounting_work: usize,
    unused_allowance: usize,
}

impl From<crate::diff::SuffixReuseWork> for JsonSuffixReuseWork {
    fn from(work: crate::diff::SuffixReuseWork) -> Self {
        Self {
            attempts: work.attempts,
            allowance_used: work.allowance_used,
            recomputed_cells: work.recomputed_cells,
            binding_work: work.binding_work,
            accounting_work: work.accounting_work,
            unused_allowance: work.unused_allowance,
        }
    }
}

#[derive(Serialize)]
struct JsonAssessmentWork {
    anchor_verification: usize,
    local_views: usize,
    localization: usize,
    emission: usize,
}

impl From<crate::diff::AssessmentWork> for JsonAssessmentWork {
    fn from(work: crate::diff::AssessmentWork) -> Self {
        Self {
            anchor_verification: work.anchor_verification,
            local_views: work.local_views,
            localization: work.localization,
            emission: work.emission,
        }
    }
}

#[derive(Serialize)]
struct JsonResolutionRange {
    block: u64,
    comparable_range: JsonRange,
    canonical_range: JsonRange,
    state: &'static str,
    #[serde(flatten)]
    sources: JsonSourceFields,
}

impl JsonResolutionRange {
    fn new(
        range: &ResolutionRange,
        side: &SideIndex<'_>,
        sources: &ReportSources<'_>,
    ) -> Result<Self> {
        let span = TextSpan {
            blocks: vec![range.block],
            separator: None,
            canonical_range: range.canonical_range,
            comparable_range: range.comparable_range,
        };
        side.resolve(&span)?;
        let projected = sources.projector.project_resolution_range(&span)?;
        let sources = JsonSourceFields::new(projected, sources.compact)?;
        Ok(Self {
            block: range.block.0,
            comparable_range: JsonRange {
                start: range.comparable_range.start,
                end: range.comparable_range.end,
            },
            canonical_range: JsonRange {
                start: range.canonical_range.start,
                end: range.canonical_range.end,
            },
            state: resolution_state(range.state),
            sources,
        })
    }
}

#[derive(Serialize)]
struct JsonRelationAssessment {
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
    parent: Option<usize>,
    outcome: &'static str,
    search: &'static str,
    reasons: Vec<&'static str>,
    assumptions: Vec<&'static str>,
}

impl JsonRelationAssessment {
    fn new(
        relation: &RelationAssessment,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
    ) -> Result<Self> {
        Ok(Self {
            old_span: relation
                .old_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, old, old_sources))
                .transpose()?,
            new_span: relation
                .new_span
                .as_ref()
                .map(|span| JsonTextSpan::new(span, new, new_sources))
                .transpose()?,
            parent: relation.parent,
            outcome: relation_outcome(relation.outcome),
            search: search_completeness(relation.search),
            reasons: relation
                .reasons
                .iter()
                .copied()
                .map(assessment_reason)
                .collect(),
            assumptions: relation
                .assumptions
                .iter()
                .copied()
                .map(assumption)
                .collect(),
        })
    }
}

#[derive(Serialize)]
struct JsonProvenChangedRegion {
    old_span: Option<JsonTextSpan>,
    new_span: Option<JsonTextSpan>,
    proof: &'static str,
    confidence: &'static str,
}

impl JsonProvenChangedRegion {
    fn new(
        region: &ProvenChangedRegion,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
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
            proof: match region.proof {
                ChangedRegionProof::ExactTokenMultisetMismatch => "exact_token_multiset_mismatch",
                ChangedRegionProof::OneSidedNonEmptyRange => "one_sided_non_empty_range",
            },
            confidence: confidence(region.confidence),
        })
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
                        ExtractionScope::GlyphGap { .. } | ExtractionScope::PageGlyphGap { .. } => {
                            "glyph_gap"
                        }
                    },
                    page: match issue.scope {
                        ExtractionScope::Document
                        | ExtractionScope::PageGap { .. }
                        | ExtractionScope::GlyphGap { .. } => None,
                        ExtractionScope::Page(page)
                        | ExtractionScope::PageGlyphGap { page, .. } => Some(page.0),
                    },
                    retained_pages_before: match issue.scope {
                        ExtractionScope::PageGap { retained_before } => Some(retained_before),
                        ExtractionScope::Document
                        | ExtractionScope::Page(_)
                        | ExtractionScope::GlyphGap { .. }
                        | ExtractionScope::PageGlyphGap { .. } => None,
                    },
                    retained_glyphs_before: match issue.scope {
                        ExtractionScope::GlyphGap { retained_before }
                        | ExtractionScope::PageGlyphGap {
                            retained_before, ..
                        } => Some(retained_before),
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
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
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
struct JsonChangeCandidate {
    kind: &'static str,
    occurrences: Vec<JsonChangeOccurrence>,
    confidence: &'static str,
    tags: Vec<&'static str>,
    relation: usize,
    alternative_group: usize,
    reasons: Vec<&'static str>,
    assumptions: Vec<&'static str>,
    outcome: &'static str,
    search: &'static str,
    parent: Option<usize>,
}

impl JsonChangeCandidate {
    fn new(
        candidate: &ChangeCandidate,
        comparison: &Comparison,
        old: &SideIndex<'_>,
        new: &SideIndex<'_>,
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
    ) -> Result<Self> {
        let relation = comparison
            .assessment
            .as_ref()
            .and_then(|assessment| assessment.relations.get(candidate.relation))
            .ok_or_else(|| {
                Error::InvalidConfiguration(
                    "candidate refers to a missing assessment relation".to_owned(),
                )
            })?;
        let change = JsonChange::new(&candidate.change, old, new, old_sources, new_sources)?;
        Ok(Self {
            kind: change.kind,
            occurrences: change.occurrences,
            confidence: change.confidence,
            tags: change.tags,
            relation: candidate.relation,
            alternative_group: candidate.alternative_group,
            reasons: relation
                .reasons
                .iter()
                .copied()
                .map(assessment_reason)
                .collect(),
            assumptions: relation
                .assumptions
                .iter()
                .copied()
                .map(assumption)
                .collect(),
            outcome: relation_outcome(relation.outcome),
            search: search_completeness(relation.search),
            parent: relation.parent,
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
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
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
        old_sources: &ReportSources<'_>,
        new_sources: &ReportSources<'_>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    block_separators: Option<Vec<&'static str>>,
    canonical_range: JsonRange,
    comparable_range: JsonRange,
    text: String,
    unmapped_tokens: Vec<JsonUnmappedToken>,
    #[serde(flatten)]
    sources: JsonSourceFields,
}

impl JsonTextSpan {
    fn new(span: &TextSpan, side: &SideIndex<'_>, sources: &ReportSources<'_>) -> Result<Self> {
        let resolved = side.resolve(span)?;
        let projected = sources.projector.project(span)?;
        let sources = JsonSourceFields::new(projected, sources.compact)?;
        Ok(Self {
            blocks: span.blocks.iter().map(|block| block.0).collect(),
            pages: resolved.pages,
            block_separator: span.separator.map(block_separator),
            block_separators: span.separator.and_then(|separator| {
                matches!(separator, BlockSeparator::PerBoundary(_)).then(|| {
                    (0..span.blocks.len().saturating_sub(1))
                        .map(|index| block_separator(separator.at(index)))
                        .collect()
                })
            }),
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

struct ReportSources<'a> {
    projector: SpanSourceProjector<'a>,
    compact: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum JsonSourceFields {
    Full {
        sources: Vec<JsonSpanSource>,
    },
    Compact {
        source_summary: Box<JsonSourceSummary>,
    },
}

impl JsonSourceFields {
    fn new(sources: Vec<SpanSourceEvidence>, compact: bool) -> Result<Self> {
        if compact {
            Ok(Self::Compact {
                source_summary: Box::new(JsonSourceSummary::new(&sources)?),
            })
        } else {
            Ok(Self::Full {
                sources: sources.into_iter().map(JsonSpanSource::from).collect(),
            })
        }
    }
}

#[derive(Serialize)]
struct JsonSourceSummary {
    canonical_array_sha256: String,
    canonical_array_bytes: usize,
    item_count: usize,
    kind_counts: BTreeMap<&'static str, usize>,
    page_ids: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    samples: Option<JsonSourceSamples>,
}

#[derive(Serialize)]
struct JsonSourceSamples {
    first: JsonSpanSource,
    last: JsonSpanSource,
}

impl JsonSourceSummary {
    fn new(sources: &[SpanSourceEvidence]) -> Result<Self> {
        let mut kind_counts = BTreeMap::new();
        let mut pages = BTreeSet::new();
        for source in sources {
            let kind = match source {
                SpanSourceEvidence::Glyph { page, .. } => {
                    pages.insert(page.0);
                    "glyph"
                }
                SpanSourceEvidence::SyntheticSpace { .. } => "synthetic_space",
                SpanSourceEvidence::LineBreak { .. } => "line_break",
                SpanSourceEvidence::BlockSeparatorSpace => "block_separator_space",
            };
            *kind_counts.entry(kind).or_insert(0) += 1;
        }
        let mut digest = SourceDigest {
            hash: Sha256::new(),
            bytes: 0,
        };
        serde_json::to_writer(&mut digest, &JsonSourceArray(sources))
            .map_err(|error| Error::Report(error.to_string()))?;
        Ok(Self {
            canonical_array_sha256: lowercase_hex(&digest.hash.finalize()),
            canonical_array_bytes: digest.bytes,
            item_count: sources.len(),
            kind_counts,
            page_ids: pages.into_iter().collect(),
            samples: sources
                .first()
                .zip(sources.last())
                .map(|(first, last)| JsonSourceSamples {
                    first: JsonSpanSource::from(*first),
                    last: JsonSpanSource::from(*last),
                }),
        })
    }
}

/// Serialize into the digest one source at a time without allocating its JSON.
struct JsonSourceArray<'a>(&'a [SpanSourceEvidence]);

impl Serialize for JsonSourceArray<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for source in self.0 {
            sequence.serialize_element(&JsonSpanSource::from(*source))?;
        }
        sequence.end()
    }
}

struct SourceDigest {
    hash: Sha256,
    bytes: usize,
}

impl Write for SourceDigest {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("source JSON byte count overflow"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
        BlockSeparator::PerBoundary(_) => "per_boundary",
    }
}

fn resolution_state(state: ResolutionState) -> &'static str {
    match state {
        ResolutionState::Equal => "equal",
        ResolutionState::Changed => "changed",
        ResolutionState::Unresolved => "unresolved",
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
        AlignmentEvidence::SearchIncomplete => "search_incomplete".to_owned(),
        AlignmentEvidence::Anchor => "anchor".to_owned(),
        AlignmentEvidence::AnchorInterval => "anchor_interval".to_owned(),
        AlignmentEvidence::NeighborConsistency => "neighbor_consistency".to_owned(),
        AlignmentEvidence::NumericMask => "numeric_mask".to_owned(),
        AlignmentEvidence::SplitMerge => "split_merge".to_owned(),
        AlignmentEvidence::NormalizationIssue => "normalization_issue".to_owned(),
        AlignmentEvidence::ExtractionGap => "extraction_gap".to_owned(),
        AlignmentEvidence::ReadingOrderUnknown => "reading_order_unknown".to_owned(),
        AlignmentEvidence::ReadingOrderInferred => "reading_order_inferred".to_owned(),
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
    fn compact_source_digest_matches_streamed_native_array() -> Result<()> {
        let glyph_sources: Vec<_> = (0..400u32)
            .map(|index| SpanSourceEvidence::Glyph {
                glyph_id: crate::model::GlyphId(u64::from(index) + 1000),
                page: crate::model::PageId(index % 4),
                bbox: Rect {
                    min: crate::model::Vec2 {
                        x: -0.125 * f64::from(index),
                        y: 0.25,
                    },
                    max: crate::model::Vec2 {
                        x: 1.0 + f64::from(index),
                        y: 7.125,
                    },
                },
                content_stream: crate::pdf::ObjectRef {
                    object_number: 127,
                    generation: 1,
                },
                operator_index: index * 7 + 3,
            })
            .collect();
        for sources in [
            glyph_sources,
            vec![],
            vec![
                SpanSourceEvidence::BlockSeparatorSpace,
                SpanSourceEvidence::SyntheticSpace {
                    preceding_glyph_id: crate::model::GlyphId(1),
                    following_glyph_id: crate::model::GlyphId(2),
                },
            ],
        ] {
            let expected = serde_json::to_vec(
                &sources
                    .iter()
                    .copied()
                    .map(JsonSpanSource::from)
                    .collect::<Vec<_>>(),
            )
            .expect("native source serialization");
            let summary = JsonSourceSummary::new(&sources)?;
            assert_eq!(
                summary.canonical_array_sha256,
                lowercase_hex(&Sha256::digest(&expected))
            );
            assert_eq!(summary.canonical_array_bytes, expected.len());
            assert_eq!(summary.item_count, sources.len());
        }
        Ok(())
    }

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
