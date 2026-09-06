use std::{cmp::Ordering, collections::HashMap, fmt::Write as _, io::Write, ops::Range};

use pdfdelta_core::{
    alignment::{AlignmentConfidence, BlockSeparator},
    diff::{
        AtomicEdit, Comparison, RecoveryGapReason, RecoveryLeafKind, RecoveryOwnership,
        SectionHeadingEvidence, SectionPairTopology, SectionPairingMetrics, SectionPairingProposal,
        SectionPairingProposalOutcome, SectionPairingProposalSide,
        SectionPairingProposalStopReason, SectionPairingView, SectionParentRelation,
        SectionProposalEdits, TextSpan,
    },
    model::{GlyphEvidence, Rect},
    normalize::{BlockText, ComparableToken},
    report::{SpanSourceEvidence, SpanSourceProjectionLimits, SpanSourceProjector},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SAMPLE_LIMIT: usize = 256;
const TEXT_EDGE_SCALARS: usize = 512;
const MAX_PROPOSALS: usize = 4_096;
const MAX_TEXT_SCALARS: usize = 4_000_000;
const MAX_PROJECTION_EVIDENCE: usize = 2_000_000;
const MAX_OUTPUT_ITEMS: usize = 1_000_000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROJECTED_COMPARABLE_TOKENS: usize = 5_100_000;
const MAX_INPUT_BLOCKS: usize = 1_000_000;
const MAX_INPUT_GLYPHS: usize = 8_000_000;
const MAX_INPUT_CHANGES: usize = 1_000_000;
const MAX_INPUT_OCCURRENCES: usize = 4_000_000;
const MAX_OVERLAP_BLOCK_VISITS: usize = 16_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPairingProposalReviewStopReasonReport {
    SectionAnalysisUnavailable,
    ProposalLimit,
    ProposalTokenLimit,
    ProposalWorkLimit,
    ProposalPayloadLimit,
    ProposalEditLimit,
    AmbiguousProposal,
    AllocationFailure,
    MissingOwnershipBlock,
    InvalidOwnershipRange,
    IncompleteOwnership,
    OwnershipOverlap,
    InvalidSourceRange,
    CounterOverflow,
    DiffFailure,
    InvariantViolation,
    TextLimit,
    SourceEvidenceLimit,
    OutputLimit,
    SourceProjectionLimit,
    SourceProjectionFailed,
    InvalidProposal,
    InputLimit,
    OverlapWorkLimit,
    FingerprintLimit,
}

impl From<SectionPairingProposalStopReason> for SectionPairingProposalReviewStopReasonReport {
    fn from(reason: SectionPairingProposalStopReason) -> Self {
        match reason {
            SectionPairingProposalStopReason::SectionAnalysisUnavailable => {
                Self::SectionAnalysisUnavailable
            }
            SectionPairingProposalStopReason::ProposalLimit => Self::ProposalLimit,
            SectionPairingProposalStopReason::ProposalTokenLimit => Self::ProposalTokenLimit,
            SectionPairingProposalStopReason::ProposalWorkLimit => Self::ProposalWorkLimit,
            SectionPairingProposalStopReason::ProposalPayloadLimit => Self::ProposalPayloadLimit,
            SectionPairingProposalStopReason::ProposalEditLimit => Self::ProposalEditLimit,
            SectionPairingProposalStopReason::AmbiguousProposal => Self::AmbiguousProposal,
            SectionPairingProposalStopReason::AllocationFailure => Self::AllocationFailure,
            SectionPairingProposalStopReason::MissingOwnershipBlock => Self::MissingOwnershipBlock,
            SectionPairingProposalStopReason::InvalidOwnershipRange => Self::InvalidOwnershipRange,
            SectionPairingProposalStopReason::IncompleteOwnership => Self::IncompleteOwnership,
            SectionPairingProposalStopReason::OwnershipOverlap => Self::OwnershipOverlap,
            SectionPairingProposalStopReason::InvalidSourceRange => Self::InvalidSourceRange,
            SectionPairingProposalStopReason::CounterOverflow => Self::CounterOverflow,
            SectionPairingProposalStopReason::DiffFailure => Self::DiffFailure,
            SectionPairingProposalStopReason::InvariantViolation => Self::InvariantViolation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SectionPairingProposalReviewBundleReport {
    Complete {
        total: usize,
        strong: usize,
        number_only: usize,
        ownership_adoptable: usize,
        exact_edits: usize,
        edit_distance_exceeded: usize,
        existing_change_overlap: usize,
        initial_structural_gate: usize,
        sample_limit: usize,
        truncated: bool,
        fingerprint_sha256: String,
        samples: Vec<SectionPairingProposalReviewSampleReport>,
    },
    Unavailable {
        reason: SectionPairingProposalReviewStopReasonReport,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPairingViewReport {
    Strong,
    NumberOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionHeadingEvidenceReport {
    Exact,
    NumberStripped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionParentRelationReport {
    Consistent,
    Changed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPairTopologyReport {
    Monotone,
    Crossing,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentConfidenceReport {
    High,
    Medium,
    Low,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingTextSpanReport {
    pub blocks: Vec<u64>,
    pub separator: Option<SectionPairingBlockSeparatorReport>,
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPairingBlockSeparatorReport {
    Concatenate,
    Space,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingBoundedTextReport {
    pub prefix: String,
    pub suffix: String,
    pub total_scalars: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SectionPairingRectReport {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SectionPairingSourcePageReport {
    pub page: u32,
    pub bbox: SectionPairingRectReport,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SectionPairingSourceReport {
    pub evidence_items: usize,
    pub pages: Vec<SectionPairingSourcePageReport>,
    pub fingerprint_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingOwnershipTotalsReport {
    pub accepted_tokens: usize,
    pub leaf_tokens: usize,
    pub gap_tokens: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SectionPairingOwnershipReport {
    Accepted,
    Leaf { leaf: RecoveryLeafKindReport },
    Gap { reason: RecoveryGapReasonReport },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryLeafKindReport {
    SentenceBody,
    LineBody,
    TrustedRunResidual,
    Heading,
    ListItem,
    Footnote,
    CodeLine,
    TableCell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryGapReasonReport {
    NoTrustedRun,
    MixedTrustedRuns,
    OrdinalGap,
    RoleBoundary,
    LocationProjectionFailed,
    NormalizationIssue,
    UnmappedChangedEvidence,
    UnsupportedLinePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingOwnershipRangeReport {
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub ownership: SectionPairingOwnershipReport,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SectionPairingLocatedTextReport {
    pub span: SectionPairingTextSpanReport,
    pub text: SectionPairingBoundedTextReport,
    pub source: SectionPairingSourceReport,
    pub page: u32,
    pub trusted_run_id: u64,
    pub ordinal_start: usize,
    pub ordinal_end: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SectionPairingProposalSideReport {
    pub heading: SectionPairingLocatedTextReport,
    pub paragraph: SectionPairingLocatedTextReport,
    pub ownership: SectionPairingOwnershipTotalsReport,
    pub ownership_ranges: Vec<SectionPairingOwnershipRangeReport>,
    pub adoptable_ownership: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingAtomicEditReport {
    pub old_start: usize,
    pub old_end: usize,
    pub new_start: usize,
    pub new_end: usize,
    pub old_span: SectionPairingTextSpanReport,
    pub new_span: SectionPairingTextSpanReport,
    pub old_text: SectionPairingBoundedTextReport,
    pub new_text: SectionPairingBoundedTextReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SectionPairingEditsReport {
    Exact {
        edits: Vec<SectionPairingAtomicEditReport>,
    },
    EditDistanceExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingOverlapReport {
    pub event_indices: Vec<usize>,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SectionPairingAdoptabilityReport {
    pub ownership_adoptable: bool,
    pub exact_nonempty_edits: bool,
    pub no_existing_change_overlap: bool,
    pub initial_structural_gate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SectionPairingProposalReviewSampleReport {
    pub view: SectionPairingViewReport,
    pub heading_evidence: SectionHeadingEvidenceReport,
    pub parent_relation: SectionParentRelationReport,
    pub topology: SectionPairTopologyReport,
    pub heading_match_span: usize,
    pub heading_match_confidence: AlignmentConfidenceReport,
    pub unresolved_span: usize,
    pub old: SectionPairingProposalSideReport,
    pub new: SectionPairingProposalSideReport,
    pub edits: SectionPairingEditsReport,
    pub existing_change_overlap: SectionPairingOverlapReport,
    pub adoptability: SectionPairingAdoptabilityReport,
}

pub(super) fn build_section_pairing_proposal_review_bundle(
    outcome: Option<&SectionPairingProposalOutcome>,
    metrics: Option<SectionPairingMetrics>,
    comparison: &Comparison,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
) -> Result<Option<Box<SectionPairingProposalReviewBundleReport>>, String> {
    let (Some(outcome), Some(metrics)) = (outcome, metrics) else {
        return if outcome.is_none() && metrics.is_none() {
            Ok(None)
        } else {
            Err("section-pairing proposal outcome and metrics availability disagree".to_owned())
        };
    };
    let report = match outcome {
        SectionPairingProposalOutcome::Unavailable(reason) => {
            SectionPairingProposalReviewBundleReport::Unavailable {
                reason: (*reason).into(),
            }
        }
        SectionPairingProposalOutcome::Complete(proposals) => {
            validate_proposal_count(proposals, metrics)?;
            match build_complete(
                proposals,
                comparison,
                old_blocks,
                new_blocks,
                old_glyph_evidence,
                new_glyph_evidence,
                SAMPLE_LIMIT,
            ) {
                Ok(report) => report,
                Err(reason) => SectionPairingProposalReviewBundleReport::Unavailable { reason },
            }
        }
    };
    validate_section_pairing_proposal_review_bundle(&report, outcome, metrics)?;
    Ok(Some(Box::new(report)))
}

pub(super) fn validate_section_pairing_proposal_review_bundle(
    report: &SectionPairingProposalReviewBundleReport,
    outcome: &SectionPairingProposalOutcome,
    metrics: SectionPairingMetrics,
) -> Result<(), String> {
    match (report, outcome) {
        (
            SectionPairingProposalReviewBundleReport::Unavailable { reason },
            SectionPairingProposalOutcome::Unavailable(core_reason),
        ) if *reason == (*core_reason).into() => Ok(()),
        (
            SectionPairingProposalReviewBundleReport::Unavailable { .. },
            SectionPairingProposalOutcome::Complete(proposals),
        ) => validate_proposal_count(proposals, metrics),
        (
            SectionPairingProposalReviewBundleReport::Complete {
                total,
                strong,
                number_only,
                ownership_adoptable,
                exact_edits,
                edit_distance_exceeded,
                existing_change_overlap,
                initial_structural_gate,
                sample_limit,
                truncated,
                fingerprint_sha256,
                samples,
            },
            SectionPairingProposalOutcome::Complete(proposals),
        ) => {
            validate_proposal_count(proposals, metrics)?;
            let expected = expected_proposals(metrics)?;
            if *total != expected
                || *strong != metrics.changed_one_to_one_same_unresolved_span
                || *number_only != metrics.number_only_changed_one_to_one_same_unresolved_span
                || *sample_limit != SAMPLE_LIMIT
                || samples.len() != (*total).min(*sample_limit)
                || *truncated != (*total > *sample_limit)
                || !valid_sha256(fingerprint_sha256)
                || *ownership_adoptable > *total
                || exact_edits.checked_add(*edit_distance_exceeded) != Some(*total)
                || *existing_change_overlap > *total
                || *initial_structural_gate > *total
            {
                return Err("section-pairing proposal review accounting is inconsistent".to_owned());
            }
            if samples
                .iter()
                .filter(|sample| sample.view == SectionPairingViewReport::Strong)
                .count()
                > *strong
                || samples
                    .iter()
                    .filter(|sample| sample.view == SectionPairingViewReport::NumberOnly)
                    .count()
                    > *number_only
            {
                return Err(
                    "section-pairing proposal review sample views are inconsistent".to_owned(),
                );
            }
            for sample in samples {
                validate_sample_report(sample)?;
            }
            Ok(())
        }
        _ => Err("section-pairing proposal review outcome is inconsistent".to_owned()),
    }
}

fn expected_proposals(metrics: SectionPairingMetrics) -> Result<usize, String> {
    metrics
        .changed_one_to_one_same_unresolved_span
        .checked_add(metrics.number_only_changed_one_to_one_same_unresolved_span)
        .ok_or_else(|| "section-pairing proposal counter overflow".to_owned())
}

fn validate_proposal_count(
    proposals: &[SectionPairingProposal],
    metrics: SectionPairingMetrics,
) -> Result<(), String> {
    if proposals.len() != expected_proposals(metrics)? {
        return Err("section-pairing proposal count disagrees with same-span counters".to_owned());
    }
    let strong = proposals
        .iter()
        .filter(|proposal| proposal.view == SectionPairingView::Strong)
        .count();
    if strong != metrics.changed_one_to_one_same_unresolved_span
        || proposals.len() - strong != metrics.number_only_changed_one_to_one_same_unresolved_span
    {
        return Err("section-pairing proposal view counts disagree with metrics".to_owned());
    }
    Ok(())
}

fn build_complete(
    proposals: &[SectionPairingProposal],
    comparison: &Comparison,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    sample_limit: usize,
) -> Result<SectionPairingProposalReviewBundleReport, SectionPairingProposalReviewStopReasonReport>
{
    if proposals.len() > MAX_PROPOSALS {
        return Err(SectionPairingProposalReviewStopReasonReport::ProposalLimit);
    }
    let mut occurrence_count = 0usize;
    for change in &comparison.changes {
        occurrence_count = occurrence_count
            .checked_add(change.occurrences.len())
            .ok_or(SectionPairingProposalReviewStopReasonReport::InputLimit)?;
    }
    validate_input_counts(
        old_blocks.len(),
        new_blocks.len(),
        old_glyph_evidence.len(),
        new_glyph_evidence.len(),
        comparison.changes.len(),
        occurrence_count,
    )?;
    let old_map = block_map(old_blocks)?;
    let new_map = block_map(new_blocks)?;
    let limits = SpanSourceProjectionLimits {
        max_comparable_tokens: MAX_PROJECTED_COMPARABLE_TOKENS,
        max_evidence_items: MAX_PROJECTION_EVIDENCE,
    };
    let old_projector = SpanSourceProjector::new(old_blocks, old_glyph_evidence, limits)
        .map_err(map_projection_error)?;
    let new_projector = SpanSourceProjector::new(new_blocks, new_glyph_evidence, limits)
        .map_err(map_projection_error)?;
    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(proposals.len())
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    ordered.extend(proposals);
    ordered.sort_unstable_by(|left, right| compare_proposals(left, right));

    let mut budget = ReviewBudget::default();
    let mut hasher = Sha256::new();
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(proposals.len().min(sample_limit))
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    let mut strong = 0usize;
    let mut number_only = 0usize;
    let mut ownership_adoptable = 0usize;
    let mut exact_edits = 0usize;
    let mut edit_distance_exceeded = 0usize;
    let mut existing_change_overlap = 0usize;
    let mut initial_structural_gate = 0usize;
    for (index, proposal) in ordered.into_iter().enumerate() {
        let sample = build_sample(
            proposal,
            comparison,
            [&old_map, &new_map],
            [&old_projector, &new_projector],
            &mut budget,
        )?;
        match proposal.view {
            SectionPairingView::Strong => strong = add(strong, 1)?,
            SectionPairingView::NumberOnly => number_only = add(number_only, 1)?,
        }
        if sample.adoptability.ownership_adoptable {
            ownership_adoptable = add(ownership_adoptable, 1)?;
        }
        match proposal.edits {
            SectionProposalEdits::Exact(_) => exact_edits = add(exact_edits, 1)?,
            SectionProposalEdits::EditDistanceExceeded => {
                edit_distance_exceeded = add(edit_distance_exceeded, 1)?
            }
        }
        if sample.existing_change_overlap.count != 0 {
            existing_change_overlap = add(existing_change_overlap, 1)?;
        }
        if sample.adoptability.initial_structural_gate {
            initial_structural_gate = add(initial_structural_gate, 1)?;
        }
        hash_full_proposal(
            &mut hasher,
            proposal,
            &sample,
            [&old_map, &new_map],
            &mut budget,
        )?;
        if index < sample_limit {
            budget.charge_output(1)?;
            samples.push(sample);
        }
    }
    let fingerprint_sha256 = hex_digest(hasher.finalize().as_slice())?;
    Ok(SectionPairingProposalReviewBundleReport::Complete {
        total: proposals.len(),
        strong,
        number_only,
        ownership_adoptable,
        exact_edits,
        edit_distance_exceeded,
        existing_change_overlap,
        initial_structural_gate,
        sample_limit,
        truncated: proposals.len() > sample_limit,
        fingerprint_sha256,
        samples,
    })
}

fn validate_input_counts(
    old_blocks: usize,
    new_blocks: usize,
    old_glyphs: usize,
    new_glyphs: usize,
    changes: usize,
    occurrences: usize,
) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
    if old_blocks > MAX_INPUT_BLOCKS
        || new_blocks > MAX_INPUT_BLOCKS
        || old_glyphs > MAX_INPUT_GLYPHS
        || new_glyphs > MAX_INPUT_GLYPHS
        || changes > MAX_INPUT_CHANGES
        || occurrences > MAX_INPUT_OCCURRENCES
    {
        Err(SectionPairingProposalReviewStopReasonReport::InputLimit)
    } else {
        Ok(())
    }
}

fn build_sample(
    proposal: &SectionPairingProposal,
    comparison: &Comparison,
    maps: [&HashMap<u64, &BlockText>; 2],
    projectors: [&SpanSourceProjector<'_>; 2],
    budget: &mut ReviewBudget,
) -> Result<SectionPairingProposalReviewSampleReport, SectionPairingProposalReviewStopReasonReport>
{
    let (old, old_tokens) = side_report(&proposal.old, maps[0], projectors[0], budget)?;
    let (new, new_tokens) = side_report(&proposal.new, maps[1], projectors[1], budget)?;
    let edits = edits_report(proposal, &old_tokens, &new_tokens, budget)?;
    let overlap = overlap_report(proposal, comparison, budget)?;
    let ownership_adoptable = proposal.old.adoptable_ownership && proposal.new.adoptable_ownership;
    let exact_nonempty_edits =
        matches!(&proposal.edits, SectionProposalEdits::Exact(edits) if !edits.is_empty());
    let no_existing_change_overlap = overlap.count == 0;
    let initial_structural_gate = proposal.view == SectionPairingView::Strong
        && proposal.heading_evidence == SectionHeadingEvidence::Exact
        && proposal.parent_relation == SectionParentRelation::Consistent
        && proposal.topology == SectionPairTopology::Monotone
        && proposal.heading_match_confidence == AlignmentConfidence::High
        && ownership_adoptable
        && exact_nonempty_edits
        && no_existing_change_overlap;
    let report = SectionPairingProposalReviewSampleReport {
        view: proposal.view.into(),
        heading_evidence: proposal.heading_evidence.into(),
        parent_relation: proposal.parent_relation.into(),
        topology: proposal.topology.into(),
        heading_match_span: proposal.heading_match_span_index,
        heading_match_confidence: proposal.heading_match_confidence.into(),
        unresolved_span: proposal.unresolved_span_index,
        old,
        new,
        edits,
        existing_change_overlap: overlap,
        adoptability: SectionPairingAdoptabilityReport {
            ownership_adoptable,
            exact_nonempty_edits,
            no_existing_change_overlap,
            initial_structural_gate,
        },
    };
    validate_sample_report(&report)
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::InvalidProposal)?;
    Ok(report)
}

fn side_report(
    side: &SectionPairingProposalSide,
    blocks: &HashMap<u64, &BlockText>,
    projector: &SpanSourceProjector<'_>,
    budget: &mut ReviewBudget,
) -> Result<
    (SectionPairingProposalSideReport, Vec<ComparableToken>),
    SectionPairingProposalReviewStopReasonReport,
> {
    if side.heading_ordinal_start >= side.heading_ordinal_end
        || side.paragraph_ordinal_start >= side.paragraph_ordinal_end
    {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidProposal);
    }
    let (heading, _) = located_text(
        &side.heading_span,
        side.heading_page,
        side.heading_trusted_run_id,
        side.heading_ordinal_start,
        side.heading_ordinal_end,
        blocks,
        projector,
        budget,
    )?;
    let (paragraph, paragraph_tokens) = located_text(
        &side.paragraph_span,
        side.paragraph_page,
        side.paragraph_trusted_run_id,
        side.paragraph_ordinal_start,
        side.paragraph_ordinal_end,
        blocks,
        projector,
        budget,
    )?;
    let mut ranges = Vec::new();
    budget.charge_output(side.ownership_ranges.len())?;
    ranges
        .try_reserve_exact(side.ownership_ranges.len())
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    let mut cursor = 0usize;
    let mut accepted = 0usize;
    let mut leaf = 0usize;
    let mut gap = 0usize;
    for range in &side.ownership_ranges {
        if range.canonical_start > range.canonical_end
            || range.comparable_start != cursor
            || range.comparable_start > range.comparable_end
            || range.comparable_end > paragraph_tokens.len()
        {
            return Err(SectionPairingProposalReviewStopReasonReport::InvalidOwnershipRange);
        }
        let count = range.comparable_end - range.comparable_start;
        match range.ownership {
            RecoveryOwnership::Accepted => accepted = add(accepted, count)?,
            RecoveryOwnership::Leaf(_) => leaf = add(leaf, count)?,
            RecoveryOwnership::Gap(_) => gap = add(gap, count)?,
        }
        ranges.push(SectionPairingOwnershipRangeReport {
            canonical_start: range.canonical_start,
            canonical_end: range.canonical_end,
            comparable_start: range.comparable_start,
            comparable_end: range.comparable_end,
            ownership: range.ownership.into(),
        });
        cursor = range.comparable_end;
    }
    if cursor != paragraph_tokens.len()
        || accepted != side.ownership.accepted_tokens
        || leaf != side.ownership.leaf_tokens
        || gap != side.ownership.gap_tokens
        || side.adoptable_ownership != (accepted == 0 && gap == 0)
    {
        return Err(SectionPairingProposalReviewStopReasonReport::IncompleteOwnership);
    }
    Ok((
        SectionPairingProposalSideReport {
            heading,
            paragraph,
            ownership: SectionPairingOwnershipTotalsReport {
                accepted_tokens: accepted,
                leaf_tokens: leaf,
                gap_tokens: gap,
            },
            ownership_ranges: ranges,
            adoptable_ownership: side.adoptable_ownership,
        },
        paragraph_tokens,
    ))
}

#[allow(clippy::too_many_arguments)]
fn located_text(
    span: &TextSpan,
    page: u32,
    trusted_run_id: u64,
    ordinal_start: usize,
    ordinal_end: usize,
    blocks: &HashMap<u64, &BlockText>,
    projector: &SpanSourceProjector<'_>,
    budget: &mut ReviewBudget,
) -> Result<
    (SectionPairingLocatedTextReport, Vec<ComparableToken>),
    SectionPairingProposalReviewStopReasonReport,
> {
    let tokens = span_tokens(span, blocks, budget)?;
    let text = bounded_text(
        &tokens[span.comparable_range.start..span.comparable_range.end],
        budget,
    )?;
    let source = source_report(projector, span, page, budget)?;
    Ok((
        SectionPairingLocatedTextReport {
            span: span_report(span)?,
            text,
            source,
            page,
            trusted_run_id,
            ordinal_start,
            ordinal_end,
        },
        tokens,
    ))
}

fn span_tokens(
    span: &TextSpan,
    blocks: &HashMap<u64, &BlockText>,
    budget: &mut ReviewBudget,
) -> Result<Vec<ComparableToken>, SectionPairingProposalReviewStopReasonReport> {
    if span.blocks.len() != 1
        || span.separator.is_some()
        || span.canonical_range.start != 0
        || span.comparable_range.start != 0
    {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
    }
    let block = blocks
        .get(&span.blocks[0].0)
        .ok_or(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange)?;
    let tokens = block
        .canonical
        .comparable_tokens()
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::InvalidSourceRange)?;
    if tokens.iter().any(|token| token.as_scalar().is_none())
        || span.comparable_range.end != tokens.len()
        || span.canonical_range.end != block.canonical.text.chars().count()
        || span.canonical_range.end != tokens.len()
    {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
    }
    budget.charge_text(tokens.len())?;
    Ok(tokens)
}

fn bounded_text(
    tokens: &[ComparableToken],
    budget: &mut ReviewBudget,
) -> Result<SectionPairingBoundedTextReport, SectionPairingProposalReviewStopReasonReport> {
    let retained = tokens.len().min(TEXT_EDGE_SCALARS * 2);
    budget.charge_output_bytes(
        retained
            .checked_mul(4)
            .ok_or(SectionPairingProposalReviewStopReasonReport::OutputLimit)?,
    )?;
    let (prefix_len, suffix_start) = if tokens.len() <= TEXT_EDGE_SCALARS * 2 {
        (tokens.len(), tokens.len())
    } else {
        (TEXT_EDGE_SCALARS, tokens.len() - TEXT_EDGE_SCALARS)
    };
    let prefix = scalar_string(&tokens[..prefix_len])?;
    let suffix = scalar_string(&tokens[suffix_start..])?;
    Ok(SectionPairingBoundedTextReport {
        prefix,
        suffix,
        total_scalars: tokens.len(),
        truncated: tokens.len() > TEXT_EDGE_SCALARS * 2,
    })
}

fn scalar_string(
    tokens: &[ComparableToken],
) -> Result<String, SectionPairingProposalReviewStopReasonReport> {
    let capacity = tokens
        .len()
        .checked_mul(4)
        .ok_or(SectionPairingProposalReviewStopReasonReport::OutputLimit)?;
    let mut text = String::new();
    text.try_reserve(capacity)
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    for token in tokens {
        text.push(
            token
                .as_scalar()
                .ok_or(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange)?,
        );
    }
    Ok(text)
}

fn source_report(
    projector: &SpanSourceProjector<'_>,
    span: &TextSpan,
    expected_page: u32,
    budget: &mut ReviewBudget,
) -> Result<SectionPairingSourceReport, SectionPairingProposalReviewStopReasonReport> {
    let evidence = projector.project(span).map_err(map_projection_error)?;
    budget.charge_source(evidence.len())?;
    let fingerprint_sha256 = source_evidence_fingerprint(&evidence)?;
    let mut page_bbox: Option<(u32, Rect)> = None;
    for item in &evidence {
        if let SpanSourceEvidence::Glyph { page, bbox, .. } = item {
            if !valid_rect(*bbox) {
                return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
            }
            match &mut page_bbox {
                Some((current_page, current_bbox)) if *current_page == page.0 => {
                    *current_bbox = union_rect(*current_bbox, *bbox);
                }
                None => page_bbox = Some((page.0, *bbox)),
                Some(_) => {
                    return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
                }
            }
        }
    }
    let Some((page, bbox)) = page_bbox.filter(|(page, _)| *page == expected_page) else {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
    };
    budget.charge_output(1)?;
    let mut pages = Vec::new();
    pages
        .try_reserve_exact(1)
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    pages.push(SectionPairingSourcePageReport {
        page,
        bbox: SectionPairingRectReport {
            min_x: bbox.min.x,
            min_y: bbox.min.y,
            max_x: bbox.max.x,
            max_y: bbox.max.y,
        },
    });
    Ok(SectionPairingSourceReport {
        evidence_items: evidence.len(),
        pages,
        fingerprint_sha256,
    })
}

fn edits_report(
    proposal: &SectionPairingProposal,
    old: &[ComparableToken],
    new: &[ComparableToken],
    budget: &mut ReviewBudget,
) -> Result<SectionPairingEditsReport, SectionPairingProposalReviewStopReasonReport> {
    let SectionProposalEdits::Exact(edits) = &proposal.edits else {
        return Ok(SectionPairingEditsReport::EditDistanceExceeded);
    };
    validate_atomic_edits(edits, old, new)?;
    let mut reports = Vec::new();
    budget.charge_output(edits.len())?;
    reports
        .try_reserve_exact(edits.len())
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    for edit in edits {
        let old_range = edit.old.clone();
        let new_range = edit.new.clone();
        reports.push(SectionPairingAtomicEditReport {
            old_start: old_range.start,
            old_end: old_range.end,
            new_start: new_range.start,
            new_end: new_range.end,
            old_span: child_span_report(&proposal.old.paragraph_span, old_range.clone())?,
            new_span: child_span_report(&proposal.new.paragraph_span, new_range.clone())?,
            old_text: bounded_text(&old[old_range], budget)?,
            new_text: bounded_text(&new[new_range], budget)?,
        });
    }
    Ok(SectionPairingEditsReport::Exact { edits: reports })
}

fn validate_atomic_edits(
    edits: &[AtomicEdit],
    old: &[ComparableToken],
    new: &[ComparableToken],
) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
    if edits.is_empty() {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidProposal);
    }
    let mut old_cursor = 0usize;
    let mut new_cursor = 0usize;
    for edit in edits {
        if !valid_range(&edit.old, old.len())
            || !valid_range(&edit.new, new.len())
            || edit.old.is_empty() == edit.new.is_empty()
            || edit.old.start < old_cursor
            || edit.new.start < new_cursor
            || edit.old.start - old_cursor != edit.new.start - new_cursor
            || old[old_cursor..edit.old.start] != new[new_cursor..edit.new.start]
        {
            return Err(SectionPairingProposalReviewStopReasonReport::InvalidProposal);
        }
        old_cursor = edit.old.end;
        new_cursor = edit.new.end;
    }
    if old.len() - old_cursor != new.len() - new_cursor || old[old_cursor..] != new[new_cursor..] {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidProposal);
    }
    Ok(())
}

fn overlap_report(
    proposal: &SectionPairingProposal,
    comparison: &Comparison,
    budget: &mut ReviewBudget,
) -> Result<SectionPairingOverlapReport, SectionPairingProposalReviewStopReasonReport> {
    let mut indices = Vec::new();
    for (index, change) in comparison.changes.iter().enumerate() {
        budget.charge_overlap_visit(1)?;
        let mut overlaps = false;
        for occurrence in &change.occurrences {
            budget.charge_overlap_visit(1)?;
            if let Some(span) = &occurrence.old_span {
                overlaps = spans_conflict_budgeted(&proposal.old.paragraph_span, span, budget)?;
            }
            if !overlaps && let Some(span) = &occurrence.new_span {
                overlaps = spans_conflict_budgeted(&proposal.new.paragraph_span, span, budget)?;
            }
            if overlaps {
                break;
            }
        }
        if overlaps {
            budget.charge_output(1)?;
            indices
                .try_reserve(1)
                .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
            indices.push(index);
        }
    }
    Ok(SectionPairingOverlapReport {
        count: indices.len(),
        event_indices: indices,
    })
}

fn spans_conflict_budgeted(
    left: &TextSpan,
    right: &TextSpan,
    budget: &mut ReviewBudget,
) -> Result<bool, SectionPairingProposalReviewStopReasonReport> {
    for left_block in &left.blocks {
        for right_block in &right.blocks {
            budget.charge_overlap_visit(1)?;
            if left_block == right_block {
                if left.blocks.len() != 1 || right.blocks.len() != 1 {
                    return Ok(true);
                }
                return Ok(ranges_conflict(
                    &(left.comparable_range.start..left.comparable_range.end),
                    &(right.comparable_range.start..right.comparable_range.end),
                ));
            }
        }
    }
    Ok(false)
}

fn ranges_conflict(left: &Range<usize>, right: &Range<usize>) -> bool {
    if left.is_empty() && right.is_empty() {
        return left.start == right.start;
    }
    if left.is_empty() {
        return right.start <= left.start && left.start < right.end;
    }
    if right.is_empty() {
        return left.start <= right.start && right.start < left.end;
    }
    left.start < right.end && right.start < left.end
}

fn child_span_report(
    parent: &TextSpan,
    range: Range<usize>,
) -> Result<SectionPairingTextSpanReport, SectionPairingProposalReviewStopReasonReport> {
    if parent.blocks.len() != 1 || range.end > parent.comparable_range.end {
        return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
    }
    Ok(SectionPairingTextSpanReport {
        blocks: vec![parent.blocks[0].0],
        separator: None,
        canonical_start: range.start,
        canonical_end: range.end,
        comparable_start: range.start,
        comparable_end: range.end,
    })
}

fn span_report(
    span: &TextSpan,
) -> Result<SectionPairingTextSpanReport, SectionPairingProposalReviewStopReasonReport> {
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(span.blocks.len())
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    blocks.extend(span.blocks.iter().map(|block| block.0));
    Ok(SectionPairingTextSpanReport {
        blocks,
        separator: span.separator.map(Into::into),
        canonical_start: span.canonical_range.start,
        canonical_end: span.canonical_range.end,
        comparable_start: span.comparable_range.start,
        comparable_end: span.comparable_range.end,
    })
}

fn block_map(
    blocks: &[BlockText],
) -> Result<HashMap<u64, &BlockText>, SectionPairingProposalReviewStopReasonReport> {
    let mut map = HashMap::new();
    map.try_reserve(blocks.len())
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    for block in blocks {
        if map.insert(block.block.0, block).is_some() {
            return Err(SectionPairingProposalReviewStopReasonReport::InvalidSourceRange);
        }
    }
    Ok(map)
}

fn compare_proposals(left: &SectionPairingProposal, right: &SectionPairingProposal) -> Ordering {
    view_rank(left.view)
        .cmp(&view_rank(right.view))
        .then_with(|| left.unresolved_span_index.cmp(&right.unresolved_span_index))
        .then_with(|| {
            left.heading_match_span_index
                .cmp(&right.heading_match_span_index)
        })
        .then_with(|| compare_span(&left.old.heading_span, &right.old.heading_span))
        .then_with(|| compare_span(&left.old.paragraph_span, &right.old.paragraph_span))
        .then_with(|| compare_span(&left.new.heading_span, &right.new.heading_span))
        .then_with(|| compare_span(&left.new.paragraph_span, &right.new.paragraph_span))
        .then_with(|| compare_proposal_edits(&left.edits, &right.edits))
}

fn compare_span(left: &TextSpan, right: &TextSpan) -> Ordering {
    left.blocks
        .iter()
        .map(|block| block.0)
        .cmp(right.blocks.iter().map(|block| block.0))
        .then_with(|| {
            left.comparable_range
                .start
                .cmp(&right.comparable_range.start)
        })
        .then_with(|| left.comparable_range.end.cmp(&right.comparable_range.end))
}

fn compare_proposal_edits(left: &SectionProposalEdits, right: &SectionProposalEdits) -> Ordering {
    match (left, right) {
        (SectionProposalEdits::Exact(left), SectionProposalEdits::Exact(right)) => {
            left.iter().map(edit_key).cmp(right.iter().map(edit_key))
        }
        (SectionProposalEdits::Exact(_), SectionProposalEdits::EditDistanceExceeded) => {
            Ordering::Less
        }
        (SectionProposalEdits::EditDistanceExceeded, SectionProposalEdits::Exact(_)) => {
            Ordering::Greater
        }
        _ => Ordering::Equal,
    }
}

fn edit_key(edit: &AtomicEdit) -> (usize, usize, usize, usize) {
    (edit.old.start, edit.old.end, edit.new.start, edit.new.end)
}

fn hash_full_proposal(
    hasher: &mut Sha256,
    proposal: &SectionPairingProposal,
    report: &SectionPairingProposalReviewSampleReport,
    maps: [&HashMap<u64, &BlockText>; 2],
    budget: &mut ReviewBudget,
) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
    let remaining = MAX_OUTPUT_BYTES
        .checked_sub(budget.output_bytes)
        .ok_or(SectionPairingProposalReviewStopReasonReport::FingerprintLimit)?;
    let written = stream_report_into_fingerprint(hasher, report, remaining)?;
    budget
        .charge_output_bytes(written)
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::FingerprintLimit)?;
    for (side, map) in [(&proposal.old, maps[0]), (&proposal.new, maps[1])] {
        for span in [&side.heading_span, &side.paragraph_span] {
            let tokens = span_tokens(span, map, budget)?;
            hash_usize(hasher, tokens.len())?;
            for token in tokens {
                hash_u64(
                    hasher,
                    u64::from(
                        token
                            .as_scalar()
                            .ok_or(SectionPairingProposalReviewStopReasonReport::InvalidProposal)?
                            as u32,
                    ),
                );
            }
        }
    }
    Ok(())
}

fn stream_report_into_fingerprint(
    hasher: &mut Sha256,
    report: &SectionPairingProposalReviewSampleReport,
    limit: usize,
) -> Result<usize, SectionPairingProposalReviewStopReasonReport> {
    let mut counter = CountingWriter::new(limit);
    serde_json::to_writer(&mut counter, report).map_err(|_| {
        if counter.limit_exceeded {
            SectionPairingProposalReviewStopReasonReport::FingerprintLimit
        } else {
            SectionPairingProposalReviewStopReasonReport::InvalidProposal
        }
    })?;
    hash_usize(hasher, counter.written)?;
    let mut writer = FingerprintWriter::new(hasher, limit);
    serde_json::to_writer(&mut writer, report).map_err(|_| {
        if writer.limit_exceeded {
            SectionPairingProposalReviewStopReasonReport::FingerprintLimit
        } else {
            SectionPairingProposalReviewStopReasonReport::InvalidProposal
        }
    })?;
    Ok(writer.written)
}

struct CountingWriter {
    limit: usize,
    written: usize,
    limit_exceeded: bool,
}

impl CountingWriter {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            written: 0,
            limit_exceeded: false,
        }
    }
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.written.checked_add(bytes.len()) else {
            self.limit_exceeded = true;
            return Err(std::io::Error::other("fingerprint byte limit exceeded"));
        };
        if next > self.limit {
            self.limit_exceeded = true;
            return Err(std::io::Error::other("fingerprint byte limit exceeded"));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct FingerprintWriter<'a> {
    hasher: &'a mut Sha256,
    limit: usize,
    written: usize,
    limit_exceeded: bool,
}

impl<'a> FingerprintWriter<'a> {
    fn new(hasher: &'a mut Sha256, limit: usize) -> Self {
        Self {
            hasher,
            limit,
            written: 0,
            limit_exceeded: false,
        }
    }
}

impl Write for FingerprintWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.written.checked_add(bytes.len()) else {
            self.limit_exceeded = true;
            return Err(std::io::Error::other("fingerprint byte limit exceeded"));
        };
        if next > self.limit {
            self.limit_exceeded = true;
            return Err(std::io::Error::other("fingerprint byte limit exceeded"));
        }
        self.hasher.update(bytes);
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hash_usize(
    hasher: &mut Sha256,
    value: usize,
) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
    hash_u64(
        hasher,
        u64::try_from(value)
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::CounterOverflow)?,
    );
    Ok(())
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

fn source_evidence_fingerprint(
    evidence: &[SpanSourceEvidence],
) -> Result<String, SectionPairingProposalReviewStopReasonReport> {
    let mut hasher = Sha256::new();
    hash_usize(&mut hasher, evidence.len())?;
    for item in evidence {
        match item {
            SpanSourceEvidence::Glyph {
                glyph_id,
                page,
                bbox,
                content_stream,
                operator_index,
            } => {
                hash_u64(&mut hasher, 0);
                hash_u64(&mut hasher, glyph_id.0);
                hash_u64(&mut hasher, u64::from(page.0));
                for value in [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y] {
                    hash_u64(&mut hasher, value.to_bits());
                }
                hash_u64(&mut hasher, u64::from(content_stream.object_number));
                hash_u64(&mut hasher, u64::from(content_stream.generation));
                hash_u64(&mut hasher, u64::from(*operator_index));
            }
            SpanSourceEvidence::SyntheticSpace {
                preceding_glyph_id,
                following_glyph_id,
            } => {
                hash_u64(&mut hasher, 1);
                hash_u64(&mut hasher, preceding_glyph_id.0);
                hash_u64(&mut hasher, following_glyph_id.0);
            }
            SpanSourceEvidence::LineBreak {
                preceding_glyph_id,
                following_glyph_id,
            } => {
                hash_u64(&mut hasher, 2);
                hash_u64(&mut hasher, preceding_glyph_id.0);
                hash_u64(&mut hasher, following_glyph_id.0);
            }
            SpanSourceEvidence::BlockSeparatorSpace => hash_u64(&mut hasher, 3),
        }
    }
    hex_digest(hasher.finalize().as_slice())
}

fn hex_digest(digest: &[u8]) -> Result<String, SectionPairingProposalReviewStopReasonReport> {
    let mut output = String::new();
    output
        .try_reserve_exact(digest.len() * 2)
        .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    for byte in digest {
        write!(&mut output, "{byte:02x}")
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::AllocationFailure)?;
    }
    Ok(output)
}

fn validate_sample_report(sample: &SectionPairingProposalReviewSampleReport) -> Result<(), String> {
    for side in [&sample.old, &sample.new] {
        let paragraph_tokens = side
            .paragraph
            .span
            .comparable_end
            .checked_sub(side.paragraph.span.comparable_start)
            .ok_or_else(|| "section-pairing paragraph span is reversed".to_owned())?;
        if side.heading.ordinal_start >= side.heading.ordinal_end
            || side.paragraph.ordinal_start >= side.paragraph.ordinal_end
            || side.heading.span.blocks.is_empty()
            || side.paragraph.span.blocks.is_empty()
            || side
                .ownership
                .accepted_tokens
                .checked_add(side.ownership.leaf_tokens)
                .and_then(|sum| sum.checked_add(side.ownership.gap_tokens))
                != Some(paragraph_tokens)
            || side.adoptable_ownership
                != (side.ownership.accepted_tokens == 0 && side.ownership.gap_tokens == 0)
            || side.heading.source.pages.len() != 1
            || side.paragraph.source.pages.len() != 1
            || side.heading.source.pages[0].page != side.heading.page
            || side.paragraph.source.pages[0].page != side.paragraph.page
            || side.heading.source.evidence_items == 0
            || side.paragraph.source.evidence_items == 0
            || !valid_sha256(&side.heading.source.fingerprint_sha256)
            || !valid_sha256(&side.paragraph.source.fingerprint_sha256)
            || !valid_rect_report(side.heading.source.pages[0].bbox)
            || !valid_rect_report(side.paragraph.source.pages[0].bbox)
        {
            return Err("section-pairing proposal side report is inconsistent".to_owned());
        }
        validate_text(&side.heading.text, &side.heading.span)?;
        validate_text(&side.paragraph.text, &side.paragraph.span)?;
        let mut comparable_cursor = side.paragraph.span.comparable_start;
        let mut canonical_cursor = side.paragraph.span.canonical_start;
        let mut accepted = 0usize;
        let mut leaf = 0usize;
        let mut gap = 0usize;
        for range in &side.ownership_ranges {
            if range.comparable_start != comparable_cursor
                || range.canonical_start != canonical_cursor
                || range.comparable_start > range.comparable_end
                || range.canonical_start > range.canonical_end
                || range.comparable_end > side.paragraph.span.comparable_end
                || range.canonical_end > side.paragraph.span.canonical_end
            {
                return Err("section-pairing ownership report is inconsistent".to_owned());
            }
            let count = range
                .comparable_end
                .checked_sub(range.comparable_start)
                .ok_or_else(|| "section-pairing ownership range is reversed".to_owned())?;
            match range.ownership {
                SectionPairingOwnershipReport::Accepted => {
                    accepted = checked_add_report(accepted, count)?
                }
                SectionPairingOwnershipReport::Leaf { .. } => {
                    leaf = checked_add_report(leaf, count)?
                }
                SectionPairingOwnershipReport::Gap { .. } => gap = checked_add_report(gap, count)?,
            }
            comparable_cursor = range.comparable_end;
            canonical_cursor = range.canonical_end;
        }
        if comparable_cursor != side.paragraph.span.comparable_end
            || canonical_cursor != side.paragraph.span.canonical_end
            || accepted != side.ownership.accepted_tokens
            || leaf != side.ownership.leaf_tokens
            || gap != side.ownership.gap_tokens
        {
            return Err("section-pairing ownership report is incomplete".to_owned());
        }
    }
    let exact_nonempty = match &sample.edits {
        SectionPairingEditsReport::Exact { edits } => {
            if edits.is_empty() {
                return Err("section-pairing exact edit report is empty".to_owned());
            }
            let mut old_cursor = 0usize;
            let mut new_cursor = 0usize;
            for edit in edits {
                if edit.old_start > edit.old_end
                    || edit.new_start > edit.new_end
                    || (edit.old_start == edit.old_end) == (edit.new_start == edit.new_end)
                    || edit.old_span.comparable_start != edit.old_start
                    || edit.old_span.comparable_end != edit.old_end
                    || edit.new_span.comparable_start != edit.new_start
                    || edit.new_span.comparable_end != edit.new_end
                    || edit.old_start < old_cursor
                    || edit.new_start < new_cursor
                    || edit.old_end > sample.old.paragraph.text.total_scalars
                    || edit.new_end > sample.new.paragraph.text.total_scalars
                    || edit.old_text.total_scalars != edit.old_end - edit.old_start
                    || edit.new_text.total_scalars != edit.new_end - edit.new_start
                {
                    return Err("section-pairing edit report is inconsistent".to_owned());
                }
                old_cursor = edit.old_end;
                new_cursor = edit.new_end;
            }
            true
        }
        SectionPairingEditsReport::EditDistanceExceeded => false,
    };
    if sample.existing_change_overlap.count != sample.existing_change_overlap.event_indices.len()
        || sample.adoptability.ownership_adoptable
            != (sample.old.adoptable_ownership && sample.new.adoptable_ownership)
        || sample.adoptability.exact_nonempty_edits != exact_nonempty
        || sample.adoptability.no_existing_change_overlap
            != (sample.existing_change_overlap.count == 0)
    {
        return Err("section-pairing proposal audit flags are inconsistent".to_owned());
    }
    let gate = sample.view == SectionPairingViewReport::Strong
        && sample.heading_evidence == SectionHeadingEvidenceReport::Exact
        && sample.parent_relation == SectionParentRelationReport::Consistent
        && sample.topology == SectionPairTopologyReport::Monotone
        && sample.heading_match_confidence == AlignmentConfidenceReport::High
        && sample.adoptability.ownership_adoptable
        && sample.adoptability.exact_nonempty_edits
        && sample.adoptability.no_existing_change_overlap;
    if sample.adoptability.initial_structural_gate != gate {
        return Err("section-pairing initial structural gate is inconsistent".to_owned());
    }
    Ok(())
}

fn checked_add_report(left: usize, right: usize) -> Result<usize, String> {
    left.checked_add(right)
        .ok_or_else(|| "section-pairing report counter overflow".to_owned())
}

fn valid_rect_report(rect: SectionPairingRectReport) -> bool {
    rect.min_x.is_finite()
        && rect.min_y.is_finite()
        && rect.max_x.is_finite()
        && rect.max_y.is_finite()
        && rect.min_x <= rect.max_x
        && rect.min_y <= rect.max_y
}

fn validate_text(
    text: &SectionPairingBoundedTextReport,
    span: &SectionPairingTextSpanReport,
) -> Result<(), String> {
    let expected = span
        .canonical_end
        .checked_sub(span.canonical_start)
        .ok_or_else(|| "section-pairing text span is reversed".to_owned())?;
    let retained = text.prefix.chars().count() + text.suffix.chars().count();
    if text.total_scalars != expected
        || text.truncated != (expected > TEXT_EDGE_SCALARS * 2)
        || retained != expected.min(TEXT_EDGE_SCALARS * 2)
    {
        return Err("section-pairing bounded text is inconsistent".to_owned());
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_range(range: &Range<usize>, limit: usize) -> bool {
    range.start <= range.end && range.end <= limit
}

fn valid_rect(rect: Rect) -> bool {
    rect.min.x.is_finite()
        && rect.min.y.is_finite()
        && rect.max.x.is_finite()
        && rect.max.y.is_finite()
        && rect.min.x <= rect.max.x
        && rect.min.y <= rect.max.y
}

fn union_rect(left: Rect, right: Rect) -> Rect {
    Rect {
        min: pdfdelta_core::model::Vec2 {
            x: left.min.x.min(right.min.x),
            y: left.min.y.min(right.min.y),
        },
        max: pdfdelta_core::model::Vec2 {
            x: left.max.x.max(right.max.x),
            y: left.max.y.max(right.max.y),
        },
    }
}

fn map_projection_error(
    error: pdfdelta_core::Error,
) -> SectionPairingProposalReviewStopReasonReport {
    match error {
        pdfdelta_core::Error::LimitExceeded { .. } => {
            SectionPairingProposalReviewStopReasonReport::SourceProjectionLimit
        }
        _ => SectionPairingProposalReviewStopReasonReport::SourceProjectionFailed,
    }
}

fn add(left: usize, right: usize) -> Result<usize, SectionPairingProposalReviewStopReasonReport> {
    left.checked_add(right)
        .ok_or(SectionPairingProposalReviewStopReasonReport::CounterOverflow)
}

#[derive(Default)]
struct ReviewBudget {
    text_scalars: usize,
    source_evidence: usize,
    output_items: usize,
    output_bytes: usize,
    overlap_visits: usize,
}

impl ReviewBudget {
    fn charge_text(
        &mut self,
        amount: usize,
    ) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
        charge(&mut self.text_scalars, amount, MAX_TEXT_SCALARS)
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::TextLimit)
    }

    fn charge_source(
        &mut self,
        amount: usize,
    ) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
        charge(&mut self.source_evidence, amount, MAX_PROJECTION_EVIDENCE)
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::SourceEvidenceLimit)
    }

    fn charge_output(
        &mut self,
        amount: usize,
    ) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
        charge(&mut self.output_items, amount, MAX_OUTPUT_ITEMS)
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::OutputLimit)
    }

    fn charge_output_bytes(
        &mut self,
        amount: usize,
    ) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
        charge(&mut self.output_bytes, amount, MAX_OUTPUT_BYTES)
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::OutputLimit)
    }

    fn charge_overlap_visit(
        &mut self,
        amount: usize,
    ) -> Result<(), SectionPairingProposalReviewStopReasonReport> {
        charge(&mut self.overlap_visits, amount, MAX_OVERLAP_BLOCK_VISITS)
            .map_err(|_| SectionPairingProposalReviewStopReasonReport::OverlapWorkLimit)
    }
}

fn charge(current: &mut usize, amount: usize, limit: usize) -> Result<(), ()> {
    let next = current.checked_add(amount).ok_or(())?;
    if next > limit {
        return Err(());
    }
    *current = next;
    Ok(())
}

impl From<SectionPairingView> for SectionPairingViewReport {
    fn from(value: SectionPairingView) -> Self {
        match value {
            SectionPairingView::Strong => Self::Strong,
            SectionPairingView::NumberOnly => Self::NumberOnly,
        }
    }
}

impl From<SectionHeadingEvidence> for SectionHeadingEvidenceReport {
    fn from(value: SectionHeadingEvidence) -> Self {
        match value {
            SectionHeadingEvidence::Exact => Self::Exact,
            SectionHeadingEvidence::NumberStripped => Self::NumberStripped,
        }
    }
}

impl From<SectionParentRelation> for SectionParentRelationReport {
    fn from(value: SectionParentRelation) -> Self {
        match value {
            SectionParentRelation::Consistent => Self::Consistent,
            SectionParentRelation::Changed => Self::Changed,
            SectionParentRelation::Unknown => Self::Unknown,
        }
    }
}

impl From<SectionPairTopology> for SectionPairTopologyReport {
    fn from(value: SectionPairTopology) -> Self {
        match value {
            SectionPairTopology::Monotone => Self::Monotone,
            SectionPairTopology::Crossing => Self::Crossing,
            SectionPairTopology::Unknown => Self::Unknown,
        }
    }
}

impl From<AlignmentConfidence> for AlignmentConfidenceReport {
    fn from(value: AlignmentConfidence) -> Self {
        match value {
            AlignmentConfidence::High => Self::High,
            AlignmentConfidence::Medium => Self::Medium,
            AlignmentConfidence::Low => Self::Low,
        }
    }
}

impl From<BlockSeparator> for SectionPairingBlockSeparatorReport {
    fn from(value: BlockSeparator) -> Self {
        match value {
            BlockSeparator::Concatenate => Self::Concatenate,
            BlockSeparator::Space => Self::Space,
        }
    }
}

impl From<RecoveryOwnership> for SectionPairingOwnershipReport {
    fn from(value: RecoveryOwnership) -> Self {
        match value {
            RecoveryOwnership::Accepted => Self::Accepted,
            RecoveryOwnership::Leaf(leaf) => Self::Leaf { leaf: leaf.into() },
            RecoveryOwnership::Gap(reason) => Self::Gap {
                reason: reason.into(),
            },
        }
    }
}

impl From<RecoveryLeafKind> for RecoveryLeafKindReport {
    fn from(value: RecoveryLeafKind) -> Self {
        match value {
            RecoveryLeafKind::SentenceBody => Self::SentenceBody,
            RecoveryLeafKind::LineBody => Self::LineBody,
            RecoveryLeafKind::TrustedRunResidual => Self::TrustedRunResidual,
            RecoveryLeafKind::Heading => Self::Heading,
            RecoveryLeafKind::ListItem => Self::ListItem,
            RecoveryLeafKind::Footnote => Self::Footnote,
            RecoveryLeafKind::CodeLine => Self::CodeLine,
            RecoveryLeafKind::TableCell => Self::TableCell,
        }
    }
}

impl From<RecoveryGapReason> for RecoveryGapReasonReport {
    fn from(value: RecoveryGapReason) -> Self {
        match value {
            RecoveryGapReason::NoTrustedRun => Self::NoTrustedRun,
            RecoveryGapReason::MixedTrustedRuns => Self::MixedTrustedRuns,
            RecoveryGapReason::OrdinalGap => Self::OrdinalGap,
            RecoveryGapReason::RoleBoundary => Self::RoleBoundary,
            RecoveryGapReason::LocationProjectionFailed => Self::LocationProjectionFailed,
            RecoveryGapReason::NormalizationIssue => Self::NormalizationIssue,
            RecoveryGapReason::UnmappedChangedEvidence => Self::UnmappedChangedEvidence,
            RecoveryGapReason::UnsupportedLinePolicy => Self::UnsupportedLinePolicy,
        }
    }
}

fn view_rank(view: SectionPairingView) -> u8 {
    match view {
        SectionPairingView::Strong => 0,
        SectionPairingView::NumberOnly => 1,
    }
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        diff::{ChangeEvent, ChangeKind, Confidence, TokenRange},
        layout::{BlockId, BlockRole},
        model::{GlyphId, GlyphProvenance, PageId, Vec2},
        normalize::{MappedText, ScalarRange, SourceMapEntry, TextSource, TextSourceAtom},
        pdf::ObjectRef,
    };

    use super::*;

    fn empty_comparison() -> Comparison {
        Comparison {
            changes: Vec::new(),
            proven_changed_regions: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions: Vec::new(),
            old_coverage: pdfdelta_core::diff::Coverage {
                resolved_tokens: 0,
                total_tokens: 0,
                ratio: None,
            },
            new_coverage: pdfdelta_core::diff::Coverage {
                resolved_tokens: 0,
                total_tokens: 0,
                ratio: None,
            },
        }
    }

    fn span(block: u64, text: &str) -> TextSpan {
        let len = text.chars().count();
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: len },
            comparable_range: TokenRange { start: 0, end: len },
        }
    }

    fn evidence_block(block: u64, text: &str, page: u32) -> (BlockText, Vec<GlyphEvidence>) {
        let mut source_map = Vec::new();
        let mut glyphs = Vec::new();
        for (index, _) in text.chars().enumerate() {
            let glyph_id = GlyphId(block * 100 + index as u64);
            source_map.push(SourceMapEntry {
                output_range: ScalarRange {
                    start: index,
                    end: index + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(glyph_id)].into(),
                },
            });
            glyphs.push(GlyphEvidence {
                id: glyph_id,
                page: PageId(page),
                bbox: Rect {
                    min: Vec2 {
                        x: index as f64,
                        y: 2.0,
                    },
                    max: Vec2 {
                        x: index as f64 + 1.0,
                        y: 3.0,
                    },
                },
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: block as u32,
                        generation: 0,
                    },
                    operator_index: index as u32,
                },
            });
        }
        let mapped = || MappedText {
            text: text.to_owned(),
            source_map: source_map.clone(),
            unmapped: Vec::new(),
        };
        (
            BlockText {
                block: BlockId(block),
                role: BlockRole::Body,
                raw: mapped(),
                canonical: mapped(),
                matching: text.to_owned(),
                matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
                numeric_mask_applied: false,
                normalization_events: Vec::new(),
                issues: Vec::new(),
                pages: vec![page],
                font_size_signatures: None,
                position_signatures: None,
                line_breaks: None,
                page_breaks: None,
            },
            glyphs,
        )
    }

    fn proposal_side(
        heading_block: u64,
        heading: &str,
        paragraph_block: u64,
        paragraph: &str,
    ) -> SectionPairingProposalSide {
        let len = paragraph.chars().count();
        SectionPairingProposalSide {
            heading_span: span(heading_block, heading),
            paragraph_span: span(paragraph_block, paragraph),
            heading_page: 1,
            paragraph_page: 1,
            heading_trusted_run_id: heading_block,
            paragraph_trusted_run_id: paragraph_block,
            heading_ordinal_start: 0,
            heading_ordinal_end: 1,
            paragraph_ordinal_start: 1,
            paragraph_ordinal_end: 2,
            ownership: pdfdelta_core::diff::SectionProposalOwnership {
                accepted_tokens: 0,
                leaf_tokens: len,
                gap_tokens: 0,
            },
            ownership_ranges: vec![pdfdelta_core::diff::SectionProposalOwnershipRange {
                canonical_start: 0,
                canonical_end: len,
                comparable_start: 0,
                comparable_end: len,
                ownership: RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
            }],
            adoptable_ownership: true,
        }
    }

    fn exact_proposal(unresolved_span: usize) -> SectionPairingProposal {
        SectionPairingProposal {
            view: SectionPairingView::Strong,
            heading_evidence: SectionHeadingEvidence::Exact,
            parent_relation: SectionParentRelation::Consistent,
            topology: SectionPairTopology::Monotone,
            heading_match_span_index: unresolved_span + 10,
            heading_match_confidence: AlignmentConfidence::High,
            unresolved_span_index: unresolved_span,
            old: proposal_side(1, "1 Scope", 2, "abc"),
            new: proposal_side(11, "1 Scope", 12, "axc"),
            edits: SectionProposalEdits::Exact(vec![
                AtomicEdit {
                    old: 1..2,
                    new: 1..1,
                },
                AtomicEdit {
                    old: 2..2,
                    new: 1..2,
                },
            ]),
        }
    }

    fn proposal_evidence() -> (
        Vec<BlockText>,
        Vec<BlockText>,
        Vec<GlyphEvidence>,
        Vec<GlyphEvidence>,
    ) {
        let (old_heading, mut old_glyphs) = evidence_block(1, "1 Scope", 1);
        let (old_paragraph, mut old_paragraph_glyphs) = evidence_block(2, "abc", 1);
        old_glyphs.append(&mut old_paragraph_glyphs);
        let (new_heading, mut new_glyphs) = evidence_block(11, "1 Scope", 1);
        let (new_paragraph, mut new_paragraph_glyphs) = evidence_block(12, "axc", 1);
        new_glyphs.append(&mut new_paragraph_glyphs);
        (
            vec![old_heading, old_paragraph],
            vec![new_heading, new_paragraph],
            old_glyphs,
            new_glyphs,
        )
    }

    fn one_strong_metrics() -> SectionPairingMetrics {
        SectionPairingMetrics {
            complete: true,
            changed_one_to_one_same_unresolved_span: 1,
            ..SectionPairingMetrics::default()
        }
    }

    #[test]
    fn empty_complete_bundle_is_valid_and_deterministic() {
        let metrics = SectionPairingMetrics {
            complete: true,
            ..SectionPairingMetrics::default()
        };
        let outcome = SectionPairingProposalOutcome::Complete(Vec::new());
        let comparison = empty_comparison();
        let first = build_section_pairing_proposal_review_bundle(
            Some(&outcome),
            Some(metrics),
            &comparison,
            &[],
            &[],
            &[],
            &[],
        )
        .expect("empty bundle builds");
        let second = build_section_pairing_proposal_review_bundle(
            Some(&outcome),
            Some(metrics),
            &comparison,
            &[],
            &[],
            &[],
            &[],
        )
        .expect("empty bundle rebuilds");
        assert_eq!(first, second);
        let Some(report) = first else {
            panic!("diagnostics produce a bundle");
        };
        assert!(
            validate_section_pairing_proposal_review_bundle(&report, &outcome, metrics).is_ok()
        );
        let SectionPairingProposalReviewBundleReport::Complete {
            total,
            fingerprint_sha256,
            samples,
            ..
        } = report.as_ref()
        else {
            panic!("empty complete outcome stays complete");
        };
        assert_eq!(*total, 0);
        assert!(samples.is_empty());
        assert!(valid_sha256(fingerprint_sha256));
    }

    #[test]
    fn unavailable_core_reasons_serialize_as_typed_atomic_reports() {
        let reasons = [
            SectionPairingProposalStopReason::SectionAnalysisUnavailable,
            SectionPairingProposalStopReason::ProposalLimit,
            SectionPairingProposalStopReason::ProposalTokenLimit,
            SectionPairingProposalStopReason::ProposalWorkLimit,
            SectionPairingProposalStopReason::ProposalPayloadLimit,
            SectionPairingProposalStopReason::ProposalEditLimit,
            SectionPairingProposalStopReason::AmbiguousProposal,
            SectionPairingProposalStopReason::AllocationFailure,
            SectionPairingProposalStopReason::MissingOwnershipBlock,
            SectionPairingProposalStopReason::InvalidOwnershipRange,
            SectionPairingProposalStopReason::IncompleteOwnership,
            SectionPairingProposalStopReason::OwnershipOverlap,
            SectionPairingProposalStopReason::InvalidSourceRange,
            SectionPairingProposalStopReason::CounterOverflow,
            SectionPairingProposalStopReason::DiffFailure,
            SectionPairingProposalStopReason::InvariantViolation,
        ];
        for reason in reasons {
            let outcome = SectionPairingProposalOutcome::Unavailable(reason);
            let metrics = SectionPairingMetrics {
                complete: true,
                ..SectionPairingMetrics::default()
            };
            let report = build_section_pairing_proposal_review_bundle(
                Some(&outcome),
                Some(metrics),
                &empty_comparison(),
                &[],
                &[],
                &[],
                &[],
            )
            .expect("typed stop builds")
            .expect("diagnostics produce bundle");
            let value = serde_json::to_value(report).expect("stop serializes");
            assert_eq!(value["status"], "unavailable");
            assert_eq!(value.as_object().map(|object| object.len()), Some(2));
            assert_eq!(object_keys(&value), ["reason", "status"]);
        }
    }

    #[test]
    fn count_and_view_mismatches_fail_the_contract() {
        let strong = SectionPairingMetrics {
            complete: true,
            changed_one_to_one_same_unresolved_span: 1,
            ..SectionPairingMetrics::default()
        };
        let empty = SectionPairingProposalOutcome::Complete(Vec::new());
        assert!(
            build_section_pairing_proposal_review_bundle(
                Some(&empty),
                Some(strong),
                &empty_comparison(),
                &[],
                &[],
                &[],
                &[],
            )
            .is_err()
        );
        assert!(
            build_section_pairing_proposal_review_bundle(
                None,
                Some(SectionPairingMetrics::default()),
                &empty_comparison(),
                &[],
                &[],
                &[],
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn no_diagnostics_produce_no_bundle() {
        assert_eq!(
            build_section_pairing_proposal_review_bundle(
                None,
                None,
                &empty_comparison(),
                &[],
                &[],
                &[],
                &[],
            ),
            Ok(None)
        );
    }

    #[test]
    fn exact_edit_reports_changed_text_minimal_spans_and_gate() {
        let proposal = exact_proposal(3);
        let outcome = SectionPairingProposalOutcome::Complete(vec![proposal]);
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let bundle = build_section_pairing_proposal_review_bundle(
            Some(&outcome),
            Some(one_strong_metrics()),
            &empty_comparison(),
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        )
        .expect("valid proposal builds")
        .expect("diagnostics produce a bundle");
        let SectionPairingProposalReviewBundleReport::Complete {
            exact_edits,
            initial_structural_gate,
            samples,
            ..
        } = bundle.as_ref()
        else {
            panic!("valid proposal is complete");
        };
        assert_eq!(*exact_edits, 1);
        assert_eq!(*initial_structural_gate, 1);
        let SectionPairingEditsReport::Exact { edits } = &samples[0].edits else {
            panic!("exact edits are retained");
        };
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].old_text.prefix, "b");
        assert_eq!(edits[0].new_text.prefix, "");
        assert_eq!(edits[1].old_text.prefix, "");
        assert_eq!(edits[1].new_text.prefix, "x");
        assert_eq!(edits[0].old_span.comparable_start, 1);
        assert_eq!(edits[1].new_span.comparable_end, 2);

        let value = serde_json::to_value(bundle).expect("bundle serializes");
        assert_eq!(
            object_keys(&value),
            [
                "edit_distance_exceeded",
                "exact_edits",
                "existing_change_overlap",
                "fingerprint_sha256",
                "initial_structural_gate",
                "number_only",
                "ownership_adoptable",
                "sample_limit",
                "samples",
                "status",
                "strong",
                "total",
                "truncated",
            ]
        );
        assert_eq!(
            object_keys(&value["samples"][0]),
            [
                "adoptability",
                "edits",
                "existing_change_overlap",
                "heading_evidence",
                "heading_match_confidence",
                "heading_match_span",
                "new",
                "old",
                "parent_relation",
                "topology",
                "unresolved_span",
                "view",
            ]
        );
        assert_eq!(
            object_keys(&value["samples"][0]["old"]),
            [
                "adoptable_ownership",
                "heading",
                "ownership",
                "ownership_ranges",
                "paragraph",
            ]
        );
        let old = &value["samples"][0]["old"];
        assert_eq!(
            object_keys(&old["heading"]),
            [
                "ordinal_end",
                "ordinal_start",
                "page",
                "source",
                "span",
                "text",
                "trusted_run_id",
            ]
        );
        assert_eq!(
            object_keys(&old["heading"]["source"]),
            ["evidence_items", "fingerprint_sha256", "pages"]
        );
        assert_eq!(
            object_keys(&old["heading"]["source"]["pages"][0]),
            ["bbox", "page"]
        );
        assert_eq!(
            object_keys(&old["heading"]["source"]["pages"][0]["bbox"]),
            ["max_x", "max_y", "min_x", "min_y"]
        );
        assert_eq!(
            object_keys(&old["heading"]["span"]),
            [
                "blocks",
                "canonical_end",
                "canonical_start",
                "comparable_end",
                "comparable_start",
                "separator",
            ]
        );
        assert_eq!(
            object_keys(&old["heading"]["text"]),
            ["prefix", "suffix", "total_scalars", "truncated"]
        );
        assert_eq!(
            object_keys(&old["ownership"]),
            ["accepted_tokens", "gap_tokens", "leaf_tokens"]
        );
        assert_eq!(
            object_keys(&old["ownership_ranges"][0]),
            [
                "canonical_end",
                "canonical_start",
                "comparable_end",
                "comparable_start",
                "ownership",
            ]
        );
        assert_eq!(
            object_keys(&old["ownership_ranges"][0]["ownership"]),
            ["kind", "leaf"]
        );
        let sample = &value["samples"][0];
        assert_eq!(object_keys(&sample["edits"]), ["edits", "status"]);
        assert_eq!(
            object_keys(&sample["edits"]["edits"][0]),
            [
                "new_end",
                "new_span",
                "new_start",
                "new_text",
                "old_end",
                "old_span",
                "old_start",
                "old_text",
            ]
        );
        assert_eq!(
            object_keys(&sample["existing_change_overlap"]),
            ["count", "event_indices"]
        );
        assert_eq!(
            object_keys(&sample["adoptability"]),
            [
                "exact_nonempty_edits",
                "initial_structural_gate",
                "no_existing_change_overlap",
                "ownership_adoptable",
            ]
        );
        let accepted = serde_json::to_value(SectionPairingOwnershipReport::Accepted)
            .expect("accepted ownership serializes");
        let gap = serde_json::to_value(SectionPairingOwnershipReport::Gap {
            reason: RecoveryGapReasonReport::OrdinalGap,
        })
        .expect("gap ownership serializes");
        assert_eq!(object_keys(&accepted), ["kind"]);
        assert_eq!(object_keys(&gap), ["kind", "reason"]);
    }

    #[test]
    fn distance_exceeded_and_existing_overlap_are_audited() {
        let mut proposal = exact_proposal(3);
        proposal.edits = SectionProposalEdits::EditDistanceExceeded;
        let outcome = SectionPairingProposalOutcome::Complete(vec![proposal]);
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let mut comparison = empty_comparison();
        comparison.changes.push(ChangeEvent::single_occurrence(
            ChangeKind::Replacement,
            Some(span(2, "abc")),
            Some(span(12, "axc")),
            Confidence::High,
            Vec::new(),
        ));
        let bundle = build_section_pairing_proposal_review_bundle(
            Some(&outcome),
            Some(one_strong_metrics()),
            &comparison,
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        )
        .expect("distance-exceeded proposal builds")
        .expect("diagnostics produce a bundle");
        let SectionPairingProposalReviewBundleReport::Complete {
            edit_distance_exceeded,
            existing_change_overlap,
            initial_structural_gate,
            samples,
            ..
        } = bundle.as_ref()
        else {
            panic!("valid proposal is complete");
        };
        assert_eq!(*edit_distance_exceeded, 1);
        assert_eq!(*existing_change_overlap, 1);
        assert_eq!(*initial_structural_gate, 0);
        assert_eq!(samples[0].existing_change_overlap.event_indices, vec![0]);
        assert!(matches!(
            samples[0].edits,
            SectionPairingEditsReport::EditDistanceExceeded
        ));
        let value = serde_json::to_value(&samples[0].edits).expect("edits serialize");
        assert_eq!(object_keys(&value), ["status"]);
    }

    #[test]
    fn malformed_ownership_ordinal_and_edit_fail_atomically() {
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        for malformed in [0_u8, 1, 2] {
            let mut proposal = exact_proposal(3);
            match malformed {
                0 => proposal.old.paragraph_ordinal_end = proposal.old.paragraph_ordinal_start,
                1 => proposal.old.ownership_ranges[0].comparable_start = 1,
                2 => {
                    proposal.edits = SectionProposalEdits::Exact(vec![AtomicEdit {
                        old: 4..5,
                        new: 1..1,
                    }]);
                }
                _ => unreachable!(),
            }
            let outcome = SectionPairingProposalOutcome::Complete(vec![proposal]);
            let report = build_section_pairing_proposal_review_bundle(
                Some(&outcome),
                Some(one_strong_metrics()),
                &empty_comparison(),
                &old_blocks,
                &new_blocks,
                &old_glyphs,
                &new_glyphs,
            )
            .expect("invalid proposal maps to atomic unavailable")
            .expect("diagnostics produce a bundle");
            assert!(matches!(
                report.as_ref(),
                SectionPairingProposalReviewBundleReport::Unavailable {
                    reason: SectionPairingProposalReviewStopReasonReport::InvalidProposal
                        | SectionPairingProposalReviewStopReasonReport::InvalidOwnershipRange
                }
            ));
        }
    }

    #[test]
    fn proposal_order_is_deterministic_and_full_evidence_changes_fingerprint() {
        let first = exact_proposal(7);
        let mut second = exact_proposal(2);
        second.heading_match_span_index = 4;
        let metrics = SectionPairingMetrics {
            complete: true,
            changed_one_to_one_same_unresolved_span: 2,
            ..SectionPairingMetrics::default()
        };
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let build = |proposals: Vec<SectionPairingProposal>| {
            build_section_pairing_proposal_review_bundle(
                Some(&SectionPairingProposalOutcome::Complete(proposals)),
                Some(metrics),
                &empty_comparison(),
                &old_blocks,
                &new_blocks,
                &old_glyphs,
                &new_glyphs,
            )
            .expect("valid proposals build")
            .expect("diagnostics produce a bundle")
        };
        let forward = build(vec![first.clone(), second.clone()]);
        let reverse = build(vec![second.clone(), first.clone()]);
        assert_eq!(forward, reverse);

        let mut changed = second;
        changed.heading_match_span_index += 1;
        let changed_bundle = build(vec![first, changed]);
        let fingerprint = |bundle: &SectionPairingProposalReviewBundleReport| match bundle {
            SectionPairingProposalReviewBundleReport::Complete {
                fingerprint_sha256, ..
            } => fingerprint_sha256.clone(),
            SectionPairingProposalReviewBundleReport::Unavailable { .. } => {
                panic!("valid proposals remain complete")
            }
        };
        assert_ne!(fingerprint(&forward), fingerprint(&changed_bundle));
    }

    #[test]
    fn truncated_bundle_fingerprint_covers_unsampled_proposals() {
        let first = exact_proposal(1);
        let mut second = exact_proposal(2);
        second.heading_match_span_index = 12;
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let build = |tail: SectionPairingProposal| {
            build_complete(
                &[first.clone(), tail],
                &empty_comparison(),
                &old_blocks,
                &new_blocks,
                &old_glyphs,
                &new_glyphs,
                1,
            )
            .expect("valid truncated bundle builds")
        };
        let baseline = build(second.clone());
        second.heading_match_span_index += 1;
        let changed = build(second);
        let fields = |bundle: &SectionPairingProposalReviewBundleReport| match bundle {
            SectionPairingProposalReviewBundleReport::Complete {
                truncated,
                samples,
                fingerprint_sha256,
                ..
            } => (*truncated, samples.len(), fingerprint_sha256.clone()),
            SectionPairingProposalReviewBundleReport::Unavailable { .. } => {
                panic!("valid proposals remain complete")
            }
        };
        let baseline = fields(&baseline);
        let changed = fields(&changed);
        assert_eq!((baseline.0, baseline.1), (true, 1));
        assert_eq!((changed.0, changed.1), (true, 1));
        assert_ne!(baseline.2, changed.2);
    }

    #[test]
    fn validator_rejects_tampered_complete_accounting_and_sample() {
        let outcome = SectionPairingProposalOutcome::Complete(vec![exact_proposal(3)]);
        let metrics = one_strong_metrics();
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let bundle = build_section_pairing_proposal_review_bundle(
            Some(&outcome),
            Some(metrics),
            &empty_comparison(),
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        )
        .expect("valid proposal builds")
        .expect("diagnostics produce a bundle");
        let mut tampered = bundle.as_ref().clone();
        let SectionPairingProposalReviewBundleReport::Complete {
            ownership_adoptable,
            ..
        } = &mut tampered
        else {
            panic!("valid proposal is complete")
        };
        *ownership_adoptable = 2;
        assert!(
            validate_section_pairing_proposal_review_bundle(&tampered, &outcome, metrics).is_err()
        );

        let mut tampered = bundle.as_ref().clone();
        let SectionPairingProposalReviewBundleReport::Complete { samples, .. } = &mut tampered
        else {
            panic!("valid proposal is complete")
        };
        samples[0].old.ownership_ranges[0].comparable_start = 1;
        assert!(
            validate_section_pairing_proposal_review_bundle(&tampered, &outcome, metrics).is_err()
        );
    }

    #[test]
    fn resource_guards_stop_before_unbounded_work() {
        assert_eq!(
            validate_input_counts(MAX_INPUT_BLOCKS + 1, 0, 0, 0, 0, 0),
            Err(SectionPairingProposalReviewStopReasonReport::InputLimit)
        );

        let outcome = SectionPairingProposalOutcome::Complete(vec![exact_proposal(3)]);
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let bundle = build_section_pairing_proposal_review_bundle(
            Some(&outcome),
            Some(one_strong_metrics()),
            &empty_comparison(),
            &old_blocks,
            &new_blocks,
            &old_glyphs,
            &new_glyphs,
        )
        .expect("valid proposal builds")
        .expect("diagnostics produce a bundle");
        let SectionPairingProposalReviewBundleReport::Complete { samples, .. } = bundle.as_ref()
        else {
            panic!("valid proposal is complete")
        };
        assert_eq!(
            stream_report_into_fingerprint(&mut Sha256::new(), &samples[0], 1),
            Err(SectionPairingProposalReviewStopReasonReport::FingerprintLimit)
        );

        let proposal = exact_proposal(3);
        let mut comparison = empty_comparison();
        comparison.changes.push(ChangeEvent::single_occurrence(
            ChangeKind::Replacement,
            Some(span(2, "abc")),
            Some(span(12, "axc")),
            Confidence::High,
            Vec::new(),
        ));
        let mut budget = ReviewBudget {
            overlap_visits: MAX_OVERLAP_BLOCK_VISITS,
            ..ReviewBudget::default()
        };
        assert_eq!(
            overlap_report(&proposal, &comparison, &mut budget),
            Err(SectionPairingProposalReviewStopReasonReport::OverlapWorkLimit)
        );
    }

    #[test]
    fn source_provenance_only_mutation_changes_full_fingerprint() {
        let outcome = SectionPairingProposalOutcome::Complete(vec![exact_proposal(3)]);
        let (old_blocks, new_blocks, old_glyphs, new_glyphs) = proposal_evidence();
        let build = |glyphs: &[GlyphEvidence]| {
            build_section_pairing_proposal_review_bundle(
                Some(&outcome),
                Some(one_strong_metrics()),
                &empty_comparison(),
                &old_blocks,
                &new_blocks,
                glyphs,
                &new_glyphs,
            )
            .expect("valid proposal builds")
            .expect("diagnostics produce a bundle")
        };
        let first = build(&old_glyphs);
        let mut changed_glyphs = old_glyphs.clone();
        changed_glyphs[0].provenance.operator_index += 1;
        let second = build(&changed_glyphs);
        let fingerprint = |bundle: &SectionPairingProposalReviewBundleReport| match bundle {
            SectionPairingProposalReviewBundleReport::Complete {
                fingerprint_sha256, ..
            } => fingerprint_sha256.clone(),
            SectionPairingProposalReviewBundleReport::Unavailable { .. } => {
                panic!("valid source evidence remains complete")
            }
        };
        assert_ne!(fingerprint(&first), fingerprint(&second));
        let SectionPairingProposalReviewBundleReport::Complete { samples, .. } = first.as_ref()
        else {
            panic!("valid proposal is complete")
        };
        assert_ne!(
            samples[0].old.heading.source.fingerprint_sha256,
            samples[0].new.heading.source.fingerprint_sha256
        );
    }

    fn object_keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys = value
            .as_object()
            .expect("value is an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys
    }
}
