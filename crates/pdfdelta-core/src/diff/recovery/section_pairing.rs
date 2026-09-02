//! Bounded, behavior-neutral section-pairing diagnostics.

use std::collections::HashMap;

use crate::{
    alignment::{Alignment, AlignmentConfidence, AlignmentKind},
    diff::{AtomicEdit, TextSpan, TokenRange},
    layout::{BlockRole, TrustedRunId, TrustedRunInterval},
    normalize::{BlockText, ComparableToken, ScalarRange},
};

use super::{
    super::{SentenceRecoveryInput, Side},
    ownership::{RecoveryOwnership, RecoveryOwnershipLedger},
};

const MAX_SECTION_PROPOSALS: usize = 4_096;
const MAX_SECTION_PROPOSAL_TOKENS: usize = 1_000_000;
const MAX_SECTION_PROPOSAL_WORK: usize = 64_000_000;
const MAX_SECTION_PROPOSAL_EDITS: usize = 131_072;
const MAX_SECTION_PROPOSAL_PAYLOAD_ITEMS: usize = 262_144;
const MAX_SECTION_PROPOSAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_SECTION_LEDGER_INDEX_ENTRIES: usize = 262_144;
// This intentionally overestimates a hash-table entry including control bytes
// and spare capacity so the diagnostic never relies on allocator internals.
const ESTIMATED_HASH_ENTRY_BYTES: usize = 64;

/// Typed reason why complete section-pairing diagnostics are unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionPairingStopReason {
    BlockLimit,
    TokenLimit,
    FontEvidenceLimit,
    ContainerLimit,
    SpanLimit,
    ParagraphLimit,
    ParagraphPairLimit,
    ParagraphComparisonLimit,
    GapLimit,
    AllocationFailure,
    CounterOverflow,
    InvalidInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::diff) struct SectionPairingLimits {
    pub max_blocks: usize,
    pub max_tokens: usize,
    pub max_font_evidence: usize,
    pub max_containers: usize,
    pub max_spans: usize,
    pub max_paragraphs: usize,
    pub max_paragraph_pair_visits: usize,
    pub max_paragraph_token_comparisons: usize,
    pub max_gaps: usize,
    pub max_proposals: usize,
    pub max_proposal_tokens: usize,
    pub max_proposal_work: usize,
    pub max_proposal_edits: usize,
    pub max_proposal_payload_items: usize,
    pub max_proposal_estimated_bytes: usize,
    pub max_ledger_index_entries: usize,
}

impl SectionPairingLimits {
    pub(in crate::diff) fn from_max_tokens(max_tokens: usize) -> Self {
        Self {
            max_blocks: max_tokens,
            max_tokens,
            max_font_evidence: max_tokens.saturating_mul(4),
            max_containers: max_tokens,
            max_spans: max_tokens,
            max_paragraphs: max_tokens,
            max_paragraph_pair_visits: max_tokens.saturating_mul(4),
            max_paragraph_token_comparisons: max_tokens.saturating_mul(4),
            max_gaps: max_tokens,
            max_proposals: max_tokens.min(MAX_SECTION_PROPOSALS),
            max_proposal_tokens: max_tokens
                .saturating_mul(2)
                .min(MAX_SECTION_PROPOSAL_TOKENS),
            max_proposal_work: max_tokens
                .saturating_mul(max_tokens.min(2_048).saturating_add(1))
                .min(MAX_SECTION_PROPOSAL_WORK),
            max_proposal_edits: max_tokens.saturating_mul(2).min(MAX_SECTION_PROPOSAL_EDITS),
            max_proposal_payload_items: max_tokens
                .saturating_mul(4)
                .min(MAX_SECTION_PROPOSAL_PAYLOAD_ITEMS),
            max_proposal_estimated_bytes: MAX_SECTION_PROPOSAL_BYTES,
            max_ledger_index_entries: max_tokens.min(MAX_SECTION_LEDGER_INDEX_ENTRIES),
        }
    }
}

/// Heap-free counters from the section-pairing shadow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionPairingMetrics {
    pub complete: bool,
    pub stop_reason: Option<SectionPairingStopReason>,

    pub old_sections: usize,
    pub new_sections: usize,
    pub old_paragraphs: usize,
    pub new_paragraphs: usize,
    pub old_strong_paragraph_memberships: usize,
    pub new_strong_paragraph_memberships: usize,
    pub old_number_only_paragraph_memberships: usize,
    pub new_number_only_paragraph_memberships: usize,
    pub match_spans_examined: usize,
    pub ambiguous_span_vetoes: usize,

    pub exact_heading_pairs: usize,
    pub stripped_heading_pairs: usize,
    pub strong_heading_pairs: usize,
    pub number_only_section_pairs: usize,

    pub parent_consistent_pairs: usize,
    pub parent_changed_pairs: usize,
    pub parent_unknown_pairs: usize,
    pub number_only_parent_consistent: usize,
    pub number_only_parent_changed: usize,
    pub number_only_parent_unknown: usize,

    pub monotone_pairs: usize,
    pub crossing_pairs: usize,
    pub topology_unknown_pairs: usize,
    pub strong_paragraph_anchor_pairs: usize,
    pub number_only_paragraph_anchor_pairs: usize,
    pub paragraph_pair_visits_attempted: usize,
    pub paragraph_pair_visits_examined: usize,
    pub paragraph_token_comparisons_attempted: usize,
    pub paragraph_token_comparisons_examined: usize,
    pub paragraph_anchor_crossing_vetoes: usize,

    pub insertion_gaps: usize,
    pub deletion_gaps: usize,
    pub one_to_one_gaps: usize,
    pub many_to_many_gaps: usize,
    pub changed_one_to_one_gaps: usize,
    pub changed_one_to_one_same_unresolved_span: usize,

    pub number_only_insertion_gaps: usize,
    pub number_only_deletion_gaps: usize,
    pub number_only_one_to_one_gaps: usize,
    pub number_only_many_to_many_gaps: usize,
    pub number_only_changed_one_to_one_gaps: usize,
    pub number_only_changed_one_to_one_same_unresolved_span: usize,
}

/// Structural evidence view used to derive a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionPairingView {
    Strong,
    NumberOnly,
}

/// Exact evidence that paired two section headings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionHeadingEvidence {
    Exact,
    NumberStripped,
}

/// Relationship between the paired sections' structural parents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionParentRelation {
    Consistent,
    Changed,
    Unknown,
}

/// Trusted-run ordering evidence available for a section pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionPairTopology {
    Monotone,
    Crossing,
    Unknown,
}

/// Exact ownership of every comparable token in a proposed paragraph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionProposalOwnership {
    pub accepted_tokens: usize,
    pub leaf_tokens: usize,
    pub gap_tokens: usize,
}

/// One concrete block-local ownership range retained for audit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionProposalOwnershipRange {
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub ownership: RecoveryOwnership,
}

/// Source-backed evidence for one side of a section-pairing proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionPairingProposalSide {
    pub heading_span: TextSpan,
    pub paragraph_span: TextSpan,
    pub heading_page: u32,
    pub paragraph_page: u32,
    pub heading_trusted_run_id: u64,
    pub paragraph_trusted_run_id: u64,
    pub heading_ordinal_start: usize,
    pub heading_ordinal_end: usize,
    pub paragraph_ordinal_start: usize,
    pub paragraph_ordinal_end: usize,
    pub ownership: SectionProposalOwnership,
    pub ownership_ranges: Vec<SectionProposalOwnershipRange>,
    /// Only leaf-owned paragraphs are eligible for a future behavior change.
    pub adoptable_ownership: bool,
}

/// Bounded exact-diff result retained by one proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionProposalEdits {
    Exact(Vec<AtomicEdit>),
    EditDistanceExceeded,
}

/// Behavior-neutral evidence for one changed paragraph slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionPairingProposal {
    pub view: SectionPairingView,
    pub heading_evidence: SectionHeadingEvidence,
    pub parent_relation: SectionParentRelation,
    pub topology: SectionPairTopology,
    pub heading_match_span_index: usize,
    pub heading_match_confidence: AlignmentConfidence,
    pub unresolved_span_index: usize,
    pub old: SectionPairingProposalSide,
    pub new: SectionPairingProposalSide,
    pub edits: SectionProposalEdits,
}

/// Typed reason why the atomic proposal set is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionPairingProposalStopReason {
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
}

/// Atomic outcome of proposal collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionPairingProposalOutcome {
    Complete(Vec<SectionPairingProposal>),
    Unavailable(SectionPairingProposalStopReason),
}

/// Complete section-pairing counters and independently atomic proposals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionPairingAnalysis {
    pub metrics: SectionPairingMetrics,
    pub proposal_outcome: SectionPairingProposalOutcome,
}

impl std::ops::Deref for SectionPairingAnalysis {
    type Target = SectionPairingMetrics;

    fn deref(&self) -> &Self::Target {
        &self.metrics
    }
}

#[derive(Clone)]
struct SafeBlock {
    block_index: usize,
    page: u32,
    run_id: TrustedRunId,
    ordinal_start: usize,
    ordinal_end: usize,
    font_median: f64,
    numbering: Option<Numbering>,
    single_line: bool,
}

#[derive(Clone, Copy)]
struct Numbering {
    level: u8,
    prefix_tokens: usize,
}

#[derive(Clone)]
struct Section {
    block_index: usize,
    parent: Option<usize>,
    strong_parent: Option<usize>,
    prominent: bool,
    run_id: TrustedRunId,
    ordinal_start: usize,
    ordinal_end: usize,
    page: u32,
    paragraphs: Vec<Paragraph>,
    strong_paragraphs: Vec<Paragraph>,
}

#[derive(Clone, Copy)]
struct Paragraph {
    block_index: usize,
    page: u32,
    run_id: TrustedRunId,
    ordinal_start: usize,
    ordinal_end: usize,
}

struct SideStructure {
    sections: Vec<Section>,
    paragraph_count: usize,
    strong_paragraph_memberships: usize,
    number_only_paragraph_memberships: usize,
}

#[derive(Clone, Copy)]
struct SectionPair {
    old: usize,
    new: usize,
    strong: bool,
    heading_evidence: SectionHeadingEvidence,
    match_span_index: usize,
    match_confidence: AlignmentConfidence,
    topology: SectionPairTopology,
}

/// Diagnoses conservative Section/Paragraph pairing without changing comparison output.
///
/// Any resource or input failure discards all partial counters.
pub(in crate::diff) fn analyze_section_pairing_shadow(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: SentenceRecoveryInput<'_>,
    ledgers: Option<[&RecoveryOwnershipLedger; 2]>,
    max_edit_distance: usize,
    limits: SectionPairingLimits,
) -> SectionPairingAnalysis {
    match analyze(
        sides,
        alignment,
        recovery,
        ledgers,
        max_edit_distance,
        limits,
    ) {
        Ok((metrics, proposal_outcome)) => SectionPairingAnalysis {
            metrics: SectionPairingMetrics {
                complete: true,
                ..metrics
            },
            proposal_outcome,
        },
        Err(reason) => SectionPairingAnalysis {
            metrics: SectionPairingMetrics {
                complete: false,
                stop_reason: Some(reason),
                ..SectionPairingMetrics::default()
            },
            proposal_outcome: SectionPairingProposalOutcome::Unavailable(
                SectionPairingProposalStopReason::SectionAnalysisUnavailable,
            ),
        },
    }
}

fn analyze(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: SentenceRecoveryInput<'_>,
    ledgers: Option<[&RecoveryOwnershipLedger; 2]>,
    max_edit_distance: usize,
    limits: SectionPairingLimits,
) -> Result<(SectionPairingMetrics, SectionPairingProposalOutcome), SectionPairingStopReason> {
    let structures = [
        build_structure(sides[0], recovery.old_trusted_run_intervals, limits)?,
        build_structure(sides[1], recovery.new_trusted_run_intervals, limits)?,
    ];
    enforce(
        alignment.spans.len(),
        limits.max_spans,
        SectionPairingStopReason::SpanLimit,
    )?;
    let memberships = [
        span_membership(sides[0], alignment, true)?,
        span_membership(sides[1], alignment, false)?,
    ];
    let section_buckets = [
        section_buckets(&structures[0], &memberships[0], alignment.spans.len())?,
        section_buckets(&structures[1], &memberships[1], alignment.spans.len())?,
    ];

    let mut metrics = SectionPairingMetrics {
        old_sections: structures[0].sections.len(),
        new_sections: structures[1].sections.len(),
        old_paragraphs: structures[0].paragraph_count,
        new_paragraphs: structures[1].paragraph_count,
        old_strong_paragraph_memberships: structures[0].strong_paragraph_memberships,
        new_strong_paragraph_memberships: structures[1].strong_paragraph_memberships,
        old_number_only_paragraph_memberships: structures[0].number_only_paragraph_memberships,
        new_number_only_paragraph_memberships: structures[1].number_only_paragraph_memberships,
        ..SectionPairingMetrics::default()
    };
    let mut pairs = Vec::new();
    pairs
        .try_reserve(
            structures[0]
                .sections
                .len()
                .min(structures[1].sections.len()),
        )
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;

    for (span_index, span) in alignment.spans.iter().enumerate() {
        if span.kind != AlignmentKind::Match {
            continue;
        }
        metrics.match_spans_examined = checked_inc(metrics.match_spans_examined)?;
        let old_sections = section_buckets[0][span_index];
        let new_sections = section_buckets[1][span_index];
        if old_sections.count > 1 || new_sections.count > 1 {
            metrics.ambiguous_span_vetoes = checked_inc(metrics.ambiguous_span_vetoes)?;
            continue;
        }
        let (Some(old_section), 1, Some(new_section), 1) = (
            old_sections.first,
            old_sections.count,
            new_sections.first,
            new_sections.count,
        ) else {
            continue;
        };
        let old = &structures[0].sections[old_section];
        let new = &structures[1].sections[new_section];
        let old_tokens = &sides[0].canonical[old.block_index];
        let new_tokens = &sides[1].canonical[new.block_index];
        let exact = old_tokens == new_tokens;
        let old_stripped = stripped_heading_tokens(&sides[0].blocks[old.block_index], old_tokens);
        let new_stripped = stripped_heading_tokens(&sides[1].blocks[new.block_index], new_tokens);
        let stripped = !exact && !old_stripped.is_empty() && old_stripped == new_stripped;
        if !exact && !stripped {
            continue;
        }
        if exact {
            metrics.exact_heading_pairs = checked_inc(metrics.exact_heading_pairs)?;
        } else {
            metrics.stripped_heading_pairs = checked_inc(metrics.stripped_heading_pairs)?;
        }
        let strong = old.prominent && new.prominent;
        if strong {
            metrics.strong_heading_pairs = checked_inc(metrics.strong_heading_pairs)?;
        } else {
            metrics.number_only_section_pairs = checked_inc(metrics.number_only_section_pairs)?;
        }
        pairs
            .try_reserve(1)
            .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
        pairs.push(SectionPair {
            old: old_section,
            new: new_section,
            strong,
            heading_evidence: if exact {
                SectionHeadingEvidence::Exact
            } else {
                SectionHeadingEvidence::NumberStripped
            },
            match_span_index: span_index,
            match_confidence: span.confidence,
            topology: SectionPairTopology::Unknown,
        });
    }

    classify_pair_topology(&mut pairs, &structures, &mut metrics)?;
    let old_to_new = pair_map(&pairs, structures[0].sections.len(), false)?;
    let new_to_old = pair_map(&pairs, structures[1].sections.len(), true)?;
    let mut gap_context = GapAnalysisContext {
        structures: &structures,
        sides,
        alignment,
        memberships: &memberships,
        limits,
        paragraph_work: ParagraphWork::default(),
        proposal_collector: ProposalCollector::new(ledgers, max_edit_distance, limits),
    };
    for pair in &pairs {
        let relation = parent_relation(pair, &structures, &old_to_new, &new_to_old);
        record_parent_relation(&mut metrics, pair.strong, relation)?;
        analyze_paragraph_gaps(*pair, relation, &mut gap_context, &mut metrics)?;
    }
    metrics.paragraph_pair_visits_attempted = gap_context.paragraph_work.pair_visits_attempted;
    metrics.paragraph_pair_visits_examined = gap_context.paragraph_work.pair_visits_examined;
    metrics.paragraph_token_comparisons_attempted =
        gap_context.paragraph_work.comparisons_attempted;
    metrics.paragraph_token_comparisons_examined = gap_context.paragraph_work.comparisons_examined;
    Ok((metrics, gap_context.proposal_collector.finish()))
}

fn build_structure(
    side: &Side<'_>,
    intervals: &[Option<TrustedRunInterval>],
    limits: SectionPairingLimits,
) -> Result<SideStructure, SectionPairingStopReason> {
    if side.blocks.len() != intervals.len() {
        return Err(SectionPairingStopReason::InvalidInput);
    }
    enforce(
        side.blocks.len(),
        limits.max_blocks,
        SectionPairingStopReason::BlockLimit,
    )?;
    let mut token_count = 0usize;
    let mut font_count = 0usize;
    let mut safe = Vec::new();
    safe.try_reserve(side.blocks.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    for (block_index, (block, interval)) in side.blocks.iter().zip(intervals).enumerate() {
        token_count = checked_add(token_count, side.canonical[block_index].len())?;
        enforce(
            token_count,
            limits.max_tokens,
            SectionPairingStopReason::TokenLimit,
        )?;
        if let Some(block) = safe_block(
            block_index,
            block,
            &side.canonical[block_index],
            *interval,
            &mut font_count,
            limits.max_font_evidence,
        )? {
            safe.push(block);
        }
    }
    let medians = page_body_font_medians(&safe)?;
    let mut sections = Vec::new();
    sections
        .try_reserve(safe.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    let mut stack: Vec<(TrustedRunId, u8, usize, usize)> = Vec::new();
    stack
        .try_reserve(safe.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    let mut strong_stack: Vec<(TrustedRunId, u8, usize, usize)> = Vec::new();
    strong_stack
        .try_reserve(safe.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    let mut previous = None;
    let mut paragraph_count = 0usize;
    let mut strong_paragraph_memberships = 0usize;
    let mut number_only_paragraph_memberships = 0usize;
    for block in safe {
        let continuous = previous == Some((block.run_id, block.ordinal_start));
        if !continuous {
            stack.clear();
            strong_stack.clear();
        }
        previous = Some((block.run_id, block.ordinal_end));
        if let Some(numbering) = block.numbering.filter(|_| block.single_line) {
            while stack
                .last()
                .is_some_and(|(_, level, _, _)| *level >= numbering.level)
            {
                stack.pop();
            }
            let parent = stack.last().map(|(_, _, section, _)| *section);
            enforce(
                checked_inc(sections.len())?,
                limits.max_containers,
                SectionPairingStopReason::ContainerLimit,
            )?;
            let prominent = medians
                .get(&block.page)
                .is_some_and(|values| block.font_median > values[values.len() / 2]);
            let strong_parent = if prominent {
                while strong_stack
                    .last()
                    .is_some_and(|(_, level, _, _)| *level >= numbering.level)
                {
                    strong_stack.pop();
                }
                strong_stack.last().map(|(_, _, section, _)| *section)
            } else {
                None
            };
            sections
                .try_reserve(1)
                .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
            let section_index = sections.len();
            sections.push(Section {
                block_index: block.block_index,
                parent,
                strong_parent,
                prominent,
                run_id: block.run_id,
                ordinal_start: block.ordinal_start,
                ordinal_end: block.ordinal_end,
                page: block.page,
                paragraphs: Vec::new(),
                strong_paragraphs: Vec::new(),
            });
            stack.push((
                block.run_id,
                numbering.level,
                section_index,
                block.ordinal_end,
            ));
            if prominent {
                strong_stack.push((
                    block.run_id,
                    numbering.level,
                    section_index,
                    block.ordinal_end,
                ));
            }
            continue;
        }
        let parent = stack.last().map(|(_, _, parent, _)| *parent);
        let strong_parent = strong_stack.last().map(|(_, _, parent, _)| *parent);
        if parent.is_none() && strong_parent.is_none() {
            continue;
        }
        paragraph_count = checked_inc(paragraph_count)?;
        enforce(
            paragraph_count,
            limits.max_paragraphs,
            SectionPairingStopReason::ParagraphLimit,
        )?;
        if let Some(parent) = parent {
            number_only_paragraph_memberships = checked_inc(number_only_paragraph_memberships)?;
            sections[parent]
                .paragraphs
                .try_reserve(1)
                .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
            sections[parent].paragraphs.push(Paragraph {
                block_index: block.block_index,
                page: block.page,
                run_id: block.run_id,
                ordinal_start: block.ordinal_start,
                ordinal_end: block.ordinal_end,
            });
        }
        if let Some(parent) = strong_parent {
            strong_paragraph_memberships = checked_inc(strong_paragraph_memberships)?;
            sections[parent]
                .strong_paragraphs
                .try_reserve(1)
                .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
            sections[parent].strong_paragraphs.push(Paragraph {
                block_index: block.block_index,
                page: block.page,
                run_id: block.run_id,
                ordinal_start: block.ordinal_start,
                ordinal_end: block.ordinal_end,
            });
        }
    }
    Ok(SideStructure {
        sections,
        paragraph_count,
        strong_paragraph_memberships,
        number_only_paragraph_memberships,
    })
}

fn safe_block(
    block_index: usize,
    block: &BlockText,
    tokens: &[ComparableToken],
    interval: Option<TrustedRunInterval>,
    font_count: &mut usize,
    max_font_evidence: usize,
) -> Result<Option<SafeBlock>, SectionPairingStopReason> {
    let Some(interval) = interval else {
        return Ok(None);
    };
    if block.role != BlockRole::Body
        || !block.issues.is_empty()
        || block.pages.len() != 1
        || tokens.is_empty()
        || tokens
            .iter()
            .any(|token| matches!(token, ComparableToken::Unmapped { .. }))
        || interval.start >= interval.end
        || !source_map_is_complete(block, tokens.len())
    {
        return Ok(None);
    }
    let Some(signatures) = block
        .font_size_signatures
        .as_ref()
        .filter(|values| values.len() == tokens.len())
    else {
        return Ok(None);
    };
    let mut sizes = Vec::new();
    for signature in signatures {
        for size in signature.values() {
            *font_count = checked_inc(*font_count)?;
            enforce(
                *font_count,
                max_font_evidence,
                SectionPairingStopReason::FontEvidenceLimit,
            )?;
            sizes
                .try_reserve(1)
                .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
            sizes.push(size);
        }
    }
    if sizes.is_empty() {
        return Ok(None);
    }
    sizes.sort_by(f64::total_cmp);
    Ok(Some(SafeBlock {
        block_index,
        page: block.pages[0],
        run_id: interval.run_id,
        ordinal_start: interval.start,
        ordinal_end: interval.end,
        font_median: sizes[sizes.len() / 2],
        numbering: numbering(&block.canonical.text),
        single_line: block.line_breaks.as_ref().is_some_and(Vec::is_empty),
    }))
}

fn source_map_is_complete(block: &BlockText, token_count: usize) -> bool {
    let mut cursor = 0usize;
    for entry in &block.canonical.source_map {
        if entry.output_range.start != cursor
            || entry.output_range.start >= entry.output_range.end
            || entry.output_range.end > token_count
            || entry.source.atoms.is_empty()
        {
            return false;
        }
        cursor = entry.output_range.end;
    }
    cursor == token_count
}

fn page_body_font_medians(
    blocks: &[SafeBlock],
) -> Result<HashMap<u32, Vec<f64>>, SectionPairingStopReason> {
    let mut medians = HashMap::<u32, Vec<f64>>::new();
    medians
        .try_reserve(blocks.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    for block in blocks.iter().filter(|block| block.numbering.is_none()) {
        medians
            .entry(block.page)
            .or_default()
            .try_reserve(1)
            .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
        medians
            .get_mut(&block.page)
            .ok_or(SectionPairingStopReason::AllocationFailure)?
            .push(block.font_median);
    }
    for values in medians.values_mut() {
        values.sort_by(f64::total_cmp);
    }
    Ok(medians)
}

fn numbering(text: &str) -> Option<Numbering> {
    let original = text;
    let text = original.trim_start();
    let leading = original[..original.len() - text.len()].chars().count();
    if text
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("appendix"))
        && text[8..].chars().next().is_none_or(char::is_whitespace)
    {
        let bytes = 8 + text[8..]
            .chars()
            .take_while(|value| value.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>();
        return Some(Numbering {
            level: 1,
            prefix_tokens: leading + text[..bytes].chars().count(),
        });
    }
    let mut chars = text.char_indices().peekable();
    let first = chars.peek()?.1;
    let level = if first.is_ascii_alphabetic() {
        chars.next();
        let (_, dot) = chars.next()?;
        if dot != '.' {
            return None;
        }
        1
    } else if first.is_ascii_digit() {
        let mut value = 1u8;
        while chars.next_if(|(_, ch)| ch.is_ascii_digit()).is_some() {}
        while let Some((_, '.')) = chars.peek().copied() {
            chars.next();
            if !chars.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) {
                break;
            }
            value = value.checked_add(1)?;
            while chars.next_if(|(_, ch)| ch.is_ascii_digit()).is_some() {}
        }
        value
    } else {
        return None;
    };
    if chars
        .peek()
        .is_some_and(|(_, ch)| !ch.is_whitespace() && *ch != ':' && *ch != '-')
    {
        return None;
    }
    while chars
        .next_if(|(_, ch)| ch.is_whitespace() || *ch == ':' || *ch == '-')
        .is_some()
    {}
    let byte_end = chars.peek().map_or(text.len(), |(index, _)| *index);
    Some(Numbering {
        level,
        prefix_tokens: leading + text[..byte_end].chars().count(),
    })
}

fn stripped_heading_tokens<'a>(
    block: &BlockText,
    tokens: &'a [ComparableToken],
) -> &'a [ComparableToken] {
    let prefix = numbering(&block.canonical.text).map_or(0, |numbering| numbering.prefix_tokens);
    tokens.get(prefix..).unwrap_or(&[])
}

fn span_membership(
    side: &Side<'_>,
    alignment: &Alignment,
    old: bool,
) -> Result<Vec<Option<usize>>, SectionPairingStopReason> {
    let mut memberships = Vec::new();
    memberships
        .try_reserve_exact(side.blocks.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    memberships.resize(side.blocks.len(), None);
    for (span_index, span) in alignment.spans.iter().enumerate() {
        let blocks = if old { &span.old } else { &span.new };
        for block in blocks {
            let index = *side
                .index
                .get(block)
                .ok_or(SectionPairingStopReason::InvalidInput)?;
            if memberships[index].replace(span_index).is_some() {
                return Err(SectionPairingStopReason::InvalidInput);
            }
        }
    }
    Ok(memberships)
}

#[derive(Clone, Copy, Default)]
struct SpanSections {
    first: Option<usize>,
    count: usize,
}

fn section_buckets(
    structure: &SideStructure,
    membership: &[Option<usize>],
    span_count: usize,
) -> Result<Vec<SpanSections>, SectionPairingStopReason> {
    let mut buckets = Vec::new();
    buckets
        .try_reserve_exact(span_count)
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    buckets.resize(span_count, SpanSections::default());
    for (section_index, section) in structure.sections.iter().enumerate() {
        let Some(span_index) = membership[section.block_index] else {
            continue;
        };
        let bucket = buckets
            .get_mut(span_index)
            .ok_or(SectionPairingStopReason::InvalidInput)?;
        bucket.first.get_or_insert(section_index);
        bucket.count = checked_inc(bucket.count)?;
    }
    Ok(buckets)
}

fn pair_map(
    pairs: &[SectionPair],
    section_count: usize,
    reverse: bool,
) -> Result<Vec<Option<(usize, bool)>>, SectionPairingStopReason> {
    let mut map = Vec::new();
    map.try_reserve_exact(section_count)
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    map.resize(section_count, None);
    for pair in pairs {
        let (source, target) = if reverse {
            (pair.new, pair.old)
        } else {
            (pair.old, pair.new)
        };
        map[source] = Some((target, pair.strong));
    }
    Ok(map)
}

fn parent_relation(
    pair: &SectionPair,
    structures: &[SideStructure; 2],
    old_to_new: &[Option<(usize, bool)>],
    new_to_old: &[Option<(usize, bool)>],
) -> SectionParentRelation {
    let old = &structures[0].sections[pair.old];
    let new = &structures[1].sections[pair.new];
    let (Some(old_parent), Some(new_parent)) = (if pair.strong {
        (old.strong_parent, new.strong_parent)
    } else {
        (old.parent, new.parent)
    }) else {
        return match (old.parent, new.parent) {
            (None, None) => SectionParentRelation::Consistent,
            _ => SectionParentRelation::Unknown,
        };
    };
    let old_mapping = old_to_new.get(old_parent).copied().flatten();
    let new_mapping = new_to_old.get(new_parent).copied().flatten();
    if pair.strong {
        match (old_mapping, new_mapping) {
            (Some((mapped_new, true)), Some((mapped_old, true)))
                if mapped_new == new_parent && mapped_old == old_parent =>
            {
                SectionParentRelation::Consistent
            }
            (Some((mapped_new, true)), Some((mapped_old, true)))
                if old_to_new.get(mapped_old).copied().flatten() == Some((new_parent, true))
                    && new_to_old.get(mapped_new).copied().flatten()
                        == Some((old_parent, true)) =>
            {
                SectionParentRelation::Changed
            }
            _ => SectionParentRelation::Unknown,
        }
    } else {
        match old_mapping {
            Some((mapped, _)) if mapped == new_parent => SectionParentRelation::Consistent,
            Some(_) => SectionParentRelation::Changed,
            None => SectionParentRelation::Unknown,
        }
    }
}

fn record_parent_relation(
    metrics: &mut SectionPairingMetrics,
    strong: bool,
    relation: SectionParentRelation,
) -> Result<(), SectionPairingStopReason> {
    let target = match (strong, relation) {
        (true, SectionParentRelation::Consistent) => &mut metrics.parent_consistent_pairs,
        (true, SectionParentRelation::Changed) => &mut metrics.parent_changed_pairs,
        (true, SectionParentRelation::Unknown) => &mut metrics.parent_unknown_pairs,
        (false, SectionParentRelation::Consistent) => &mut metrics.number_only_parent_consistent,
        (false, SectionParentRelation::Changed) => &mut metrics.number_only_parent_changed,
        (false, SectionParentRelation::Unknown) => &mut metrics.number_only_parent_unknown,
    };
    *target = checked_inc(*target)?;
    Ok(())
}

fn classify_pair_topology(
    pairs: &mut [SectionPair],
    structures: &[SideStructure; 2],
    metrics: &mut SectionPairingMetrics,
) -> Result<(), SectionPairingStopReason> {
    let mut selected = Vec::new();
    selected
        .try_reserve(pairs.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    selected.extend(
        pairs
            .iter()
            .enumerate()
            .filter_map(|(index, pair)| pair.strong.then_some(index)),
    );
    selected.sort_by_key(|index| {
        let old = &structures[0].sections[pairs[*index].old];
        (old.run_id.0, old.ordinal_start)
    });

    let mut old_partners = HashMap::<TrustedRunId, Option<TrustedRunId>>::new();
    let mut new_partners = HashMap::<TrustedRunId, Option<TrustedRunId>>::new();
    old_partners
        .try_reserve(selected.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    new_partners
        .try_reserve(selected.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    for index in &selected {
        let pair = pairs[*index];
        let old_run = structures[0].sections[pair.old].run_id;
        let new_run = structures[1].sections[pair.new].run_id;
        update_run_partner(&mut old_partners, old_run, new_run);
        update_run_partner(&mut new_partners, new_run, old_run);
    }

    let mut start = 0usize;
    while start < selected.len() {
        let pair = pairs[selected[start]];
        let old_run = structures[0].sections[pair.old].run_id;
        let new_run = structures[1].sections[pair.new].run_id;
        let reciprocal = old_partners.get(&old_run) == Some(&Some(new_run))
            && new_partners.get(&new_run) == Some(&Some(old_run));
        let mut end = start + 1;
        while end < selected.len()
            && structures[0].sections[pairs[selected[end]].old].run_id == old_run
            && structures[1].sections[pairs[selected[end]].new].run_id == new_run
        {
            end += 1;
        }
        if !reciprocal || end - start < 2 {
            metrics.topology_unknown_pairs =
                checked_add(metrics.topology_unknown_pairs, end - start)?;
            for index in &selected[start..end] {
                pairs[*index].topology = SectionPairTopology::Unknown;
            }
            start = end;
            continue;
        }
        let group = &selected[start..end];
        let mut suffix_min = Vec::new();
        suffix_min
            .try_reserve_exact(group.len())
            .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
        suffix_min.resize(group.len(), usize::MAX);
        let mut minimum = usize::MAX;
        for (index, pair_index) in group.iter().enumerate().rev() {
            minimum = minimum.min(structures[1].sections[pairs[*pair_index].new].ordinal_start);
            suffix_min[index] = minimum;
        }
        let mut prefix_max = 0usize;
        for (index, pair_index) in group.iter().enumerate() {
            let ordinal = structures[1].sections[pairs[*pair_index].new].ordinal_start;
            let crossing = (index > 0 && prefix_max > ordinal)
                || (index + 1 < group.len() && suffix_min[index + 1] < ordinal);
            if crossing {
                metrics.crossing_pairs = checked_inc(metrics.crossing_pairs)?;
                pairs[*pair_index].topology = SectionPairTopology::Crossing;
            } else {
                metrics.monotone_pairs = checked_inc(metrics.monotone_pairs)?;
                pairs[*pair_index].topology = SectionPairTopology::Monotone;
            }
            prefix_max = prefix_max.max(ordinal);
        }
        start = end;
    }
    Ok(())
}

fn update_run_partner(
    partners: &mut HashMap<TrustedRunId, Option<TrustedRunId>>,
    source: TrustedRunId,
    target: TrustedRunId,
) {
    partners
        .entry(source)
        .and_modify(|partner| {
            if *partner != Some(target) {
                *partner = None;
            }
        })
        .or_insert(Some(target));
}

struct GapAnalysisContext<'a, 'side> {
    structures: &'a [SideStructure; 2],
    sides: [&'a Side<'side>; 2],
    alignment: &'a Alignment,
    memberships: &'a [Vec<Option<usize>>; 2],
    limits: SectionPairingLimits,
    paragraph_work: ParagraphWork,
    proposal_collector: ProposalCollector<'a>,
}

#[derive(Default)]
struct ParagraphWork {
    pair_visits_attempted: usize,
    pair_visits_examined: usize,
    comparisons_attempted: usize,
    comparisons_examined: usize,
}

struct ProposalInput<'a, 'side> {
    pair: SectionPair,
    parent_relation: SectionParentRelation,
    old_section: &'a Section,
    new_section: &'a Section,
    old_paragraph: Paragraph,
    new_paragraph: Paragraph,
    unresolved_span_index: usize,
    sides: [&'a Side<'side>; 2],
}

struct ProposalCollector<'a> {
    ledgers: Option<[&'a RecoveryOwnershipLedger; 2]>,
    ledger_indexes: [HashMap<u64, usize>; 2],
    proposals: Vec<SectionPairingProposal>,
    used_pairs: HashMap<(u64, u64), ProposalIdentity>,
    used_old: HashMap<u64, u64>,
    used_new: HashMap<u64, u64>,
    stop_reason: Option<SectionPairingProposalStopReason>,
    total_tokens: usize,
    total_work: usize,
    total_edits: usize,
    total_payload_items: usize,
    estimated_bytes: usize,
    max_edit_distance: usize,
    limits: SectionPairingLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProposalIdentity {
    view: SectionPairingView,
    heading_evidence: SectionHeadingEvidence,
    parent_relation: SectionParentRelation,
    topology: SectionPairTopology,
    heading_match_span_index: usize,
    heading_match_confidence: AlignmentConfidence,
    unresolved_span_index: usize,
    old_heading_block: u64,
    new_heading_block: u64,
}

impl<'a> ProposalCollector<'a> {
    fn new(
        ledgers: Option<[&'a RecoveryOwnershipLedger; 2]>,
        max_edit_distance: usize,
        limits: SectionPairingLimits,
    ) -> Self {
        let mut collector = Self {
            ledgers,
            ledger_indexes: [HashMap::new(), HashMap::new()],
            proposals: Vec::new(),
            used_pairs: HashMap::new(),
            used_old: HashMap::new(),
            used_new: HashMap::new(),
            stop_reason: None,
            total_tokens: 0,
            total_work: 0,
            total_edits: 0,
            total_payload_items: 0,
            estimated_bytes: 0,
            max_edit_distance,
            limits,
        };
        let Some(ledgers) = ledgers else {
            collector.stop(SectionPairingProposalStopReason::MissingOwnershipBlock);
            return collector;
        };
        let ledger_entries = match proposal_add(ledgers[0].blocks.len(), ledgers[1].blocks.len()) {
            Ok(entries) => entries,
            Err(reason) => {
                collector.stop(reason);
                return collector;
            }
        };
        let ledger_bytes = ledger_entries.checked_mul(ESTIMATED_HASH_ENTRY_BYTES);
        if ledger_entries > limits.max_ledger_index_entries || ledger_bytes.is_none() {
            collector.stop(SectionPairingProposalStopReason::ProposalPayloadLimit);
            return collector;
        }
        let Some(ledger_bytes) = ledger_bytes else {
            collector.stop(SectionPairingProposalStopReason::ProposalPayloadLimit);
            return collector;
        };
        if let Err(reason) = collector.charge_bytes(ledger_bytes) {
            collector.stop(reason);
            return collector;
        }
        for (side, ledger) in ledgers.iter().enumerate() {
            if collector.ledger_indexes[side]
                .try_reserve(ledger.blocks.len())
                .is_err()
            {
                collector.stop(SectionPairingProposalStopReason::AllocationFailure);
                break;
            }
            for (index, block) in ledger.blocks.iter().enumerate() {
                if collector.ledger_indexes[side]
                    .insert(block.block_id, index)
                    .is_some()
                {
                    collector.stop(SectionPairingProposalStopReason::InvariantViolation);
                    break;
                }
            }
        }
        collector
    }

    fn collect(&mut self, input: ProposalInput<'_, '_>) {
        if self.stop_reason.is_some() {
            return;
        }
        if let Err(reason) = self.try_collect(input) {
            self.stop(reason);
        }
    }

    fn try_collect(
        &mut self,
        input: ProposalInput<'_, '_>,
    ) -> Result<(), SectionPairingProposalStopReason> {
        let old_block = input.sides[0].blocks[input.old_paragraph.block_index]
            .block
            .0;
        let new_block = input.sides[1].blocks[input.new_paragraph.block_index]
            .block
            .0;
        let identity = ProposalIdentity {
            view: if input.pair.strong {
                SectionPairingView::Strong
            } else {
                SectionPairingView::NumberOnly
            },
            heading_evidence: input.pair.heading_evidence,
            parent_relation: input.parent_relation,
            topology: input.pair.topology,
            heading_match_span_index: input.pair.match_span_index,
            heading_match_confidence: input.pair.match_confidence,
            unresolved_span_index: input.unresolved_span_index,
            old_heading_block: input.sides[0].blocks[input.old_section.block_index].block.0,
            new_heading_block: input.sides[1].blocks[input.new_section.block_index].block.0,
        };
        if let Some(previous) = self.used_pairs.get(&(old_block, new_block)) {
            return if *previous == identity {
                Ok(())
            } else {
                Err(SectionPairingProposalStopReason::AmbiguousProposal)
            };
        }
        if self
            .used_old
            .get(&old_block)
            .is_some_and(|paired| *paired != new_block)
            || self
                .used_new
                .get(&new_block)
                .is_some_and(|paired| *paired != old_block)
        {
            return Err(SectionPairingProposalStopReason::AmbiguousProposal);
        }
        let next_count = proposal_add(self.proposals.len(), 1)?;
        if next_count > self.limits.max_proposals {
            return Err(SectionPairingProposalStopReason::ProposalLimit);
        }
        let old_tokens = &input.sides[0].canonical[input.old_paragraph.block_index];
        let new_tokens = &input.sides[1].canonical[input.new_paragraph.block_index];
        let input_tokens = proposal_add(old_tokens.len(), new_tokens.len())?;
        self.total_tokens = proposal_add(self.total_tokens, input_tokens)?;
        if self.total_tokens > self.limits.max_proposal_tokens {
            return Err(SectionPairingProposalStopReason::ProposalTokenLimit);
        }
        let work = proposal_myers_work(input_tokens, self.max_edit_distance)?;
        self.total_work = proposal_add(self.total_work, work)?;
        if self.total_work > self.limits.max_proposal_work {
            return Err(SectionPairingProposalStopReason::ProposalWorkLimit);
        }
        let old = proposal_side(
            input.sides[0],
            input.old_section,
            input.old_paragraph,
            self.ledgers.expect("validated above")[0],
            &self.ledger_indexes[0],
        )?;
        let new = proposal_side(
            input.sides[1],
            input.new_section,
            input.new_paragraph,
            self.ledgers.expect("validated above")[1],
            &self.ledger_indexes[1],
        )?;
        let payload_items = proposal_add(
            old.ownership_ranges.capacity(),
            new.ownership_ranges.capacity(),
        )?;
        self.total_payload_items = proposal_add(self.total_payload_items, payload_items)?;
        if self.total_payload_items > self.limits.max_proposal_payload_items {
            return Err(SectionPairingProposalStopReason::ProposalPayloadLimit);
        }
        let hash_bytes = 3usize
            .checked_mul(ESTIMATED_HASH_ENTRY_BYTES)
            .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
        let span_capacity = old
            .heading_span
            .blocks
            .capacity()
            .checked_add(old.paragraph_span.blocks.capacity())
            .and_then(|value| value.checked_add(new.heading_span.blocks.capacity()))
            .and_then(|value| value.checked_add(new.paragraph_span.blocks.capacity()))
            .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
        let span_bytes = span_capacity
            .checked_mul(std::mem::size_of::<crate::layout::BlockId>())
            .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
        let fixed_bytes = proposal_add(hash_bytes, span_bytes)?;
        let ownership_bytes = payload_items
            .checked_mul(std::mem::size_of::<SectionProposalOwnershipRange>())
            .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
        self.charge_bytes(proposal_add(fixed_bytes, ownership_bytes)?)?;
        let edits = match super::super::myers::diff(old_tokens, new_tokens, self.max_edit_distance)
        {
            Ok(Some(edits)) => {
                self.total_edits = proposal_add(self.total_edits, edits.len())?;
                if self.total_edits > self.limits.max_proposal_edits {
                    return Err(SectionPairingProposalStopReason::ProposalEditLimit);
                }
                let edit_bytes = edits
                    .capacity()
                    .checked_mul(std::mem::size_of::<AtomicEdit>())
                    .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
                self.charge_bytes(edit_bytes)?;
                SectionProposalEdits::Exact(edits)
            }
            Ok(None) => SectionProposalEdits::EditDistanceExceeded,
            Err(_) => return Err(SectionPairingProposalStopReason::DiffFailure),
        };
        let previous_proposal_capacity = self.proposals.capacity();
        self.proposals
            .try_reserve(1)
            .map_err(|_| SectionPairingProposalStopReason::AllocationFailure)?;
        let added_proposal_capacity = self
            .proposals
            .capacity()
            .checked_sub(previous_proposal_capacity)
            .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
        self.charge_bytes(
            added_proposal_capacity
                .checked_mul(std::mem::size_of::<SectionPairingProposal>())
                .ok_or(SectionPairingProposalStopReason::CounterOverflow)?,
        )?;
        self.used_pairs
            .try_reserve(1)
            .map_err(|_| SectionPairingProposalStopReason::AllocationFailure)?;
        self.used_old
            .try_reserve(1)
            .map_err(|_| SectionPairingProposalStopReason::AllocationFailure)?;
        self.used_new
            .try_reserve(1)
            .map_err(|_| SectionPairingProposalStopReason::AllocationFailure)?;
        self.used_pairs.insert((old_block, new_block), identity);
        self.used_old.insert(old_block, new_block);
        self.used_new.insert(new_block, old_block);
        self.proposals.push(SectionPairingProposal {
            view: identity.view,
            heading_evidence: input.pair.heading_evidence,
            parent_relation: input.parent_relation,
            topology: input.pair.topology,
            heading_match_span_index: input.pair.match_span_index,
            heading_match_confidence: input.pair.match_confidence,
            unresolved_span_index: input.unresolved_span_index,
            old,
            new,
            edits,
        });
        Ok(())
    }

    fn stop(&mut self, reason: SectionPairingProposalStopReason) {
        self.proposals.clear();
        self.stop_reason.get_or_insert(reason);
    }

    fn charge_bytes(&mut self, bytes: usize) -> Result<(), SectionPairingProposalStopReason> {
        self.estimated_bytes = proposal_add(self.estimated_bytes, bytes)?;
        if self.estimated_bytes > self.limits.max_proposal_estimated_bytes {
            return Err(SectionPairingProposalStopReason::ProposalPayloadLimit);
        }
        Ok(())
    }

    fn finish(self) -> SectionPairingProposalOutcome {
        match self.stop_reason {
            Some(reason) => SectionPairingProposalOutcome::Unavailable(reason),
            None => SectionPairingProposalOutcome::Complete(self.proposals),
        }
    }
}

fn proposal_add(left: usize, right: usize) -> Result<usize, SectionPairingProposalStopReason> {
    left.checked_add(right)
        .ok_or(SectionPairingProposalStopReason::CounterOverflow)
}

fn proposal_myers_work(
    input_tokens: usize,
    max_edit_distance: usize,
) -> Result<usize, SectionPairingProposalStopReason> {
    let steps = input_tokens
        .min(max_edit_distance)
        .checked_add(1)
        .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
    let triangular = steps
        .checked_mul(
            steps
                .checked_add(1)
                .ok_or(SectionPairingProposalStopReason::CounterOverflow)?,
        )
        .and_then(|value| value.checked_div(2))
        .ok_or(SectionPairingProposalStopReason::CounterOverflow)?;
    input_tokens
        .checked_mul(steps)
        .and_then(|value| value.checked_add(triangular))
        .ok_or(SectionPairingProposalStopReason::CounterOverflow)
}

fn proposal_side(
    side: &Side<'_>,
    section: &Section,
    paragraph: Paragraph,
    ledger: &RecoveryOwnershipLedger,
    ledger_index: &HashMap<u64, usize>,
) -> Result<SectionPairingProposalSide, SectionPairingProposalStopReason> {
    let heading_span = whole_block_span(side, section.block_index)?;
    let paragraph_span = whole_block_span(side, paragraph.block_index)?;
    let block_id = side.blocks[paragraph.block_index].block.0;
    let ledger_block_index = ledger_index
        .get(&block_id)
        .copied()
        .ok_or(SectionPairingProposalStopReason::MissingOwnershipBlock)?;
    if ledger
        .blocks
        .get(ledger_block_index)
        .map(|block| block.block_id)
        != Some(block_id)
    {
        return Err(SectionPairingProposalStopReason::MissingOwnershipBlock);
    }
    let token_len = side.canonical[paragraph.block_index].len();
    let canonical_len = side.blocks[paragraph.block_index]
        .canonical
        .text
        .chars()
        .count();
    let mut ranges = Vec::new();
    let mut ownership = SectionProposalOwnership::default();
    let mut cursor = 0usize;
    for range in ledger
        .ranges
        .iter()
        .filter(|range| range.block_index == ledger_block_index)
    {
        if range.canonical_start > range.canonical_end
            || range.canonical_end > canonical_len
            || range.comparable_start > range.comparable_end
            || range.comparable_end > token_len
        {
            return Err(SectionPairingProposalStopReason::InvalidOwnershipRange);
        }
        if range.comparable_start < cursor {
            return Err(SectionPairingProposalStopReason::OwnershipOverlap);
        }
        if range.comparable_start != cursor {
            return Err(SectionPairingProposalStopReason::IncompleteOwnership);
        }
        let count = range.comparable_end - range.comparable_start;
        match range.ownership {
            RecoveryOwnership::Accepted => {
                ownership.accepted_tokens = proposal_add(ownership.accepted_tokens, count)?
            }
            RecoveryOwnership::Leaf(_) => {
                ownership.leaf_tokens = proposal_add(ownership.leaf_tokens, count)?
            }
            RecoveryOwnership::Gap(_) => {
                ownership.gap_tokens = proposal_add(ownership.gap_tokens, count)?
            }
        }
        ranges
            .try_reserve(1)
            .map_err(|_| SectionPairingProposalStopReason::AllocationFailure)?;
        ranges.push(SectionProposalOwnershipRange {
            canonical_start: range.canonical_start,
            canonical_end: range.canonical_end,
            comparable_start: range.comparable_start,
            comparable_end: range.comparable_end,
            ownership: range.ownership,
        });
        cursor = range.comparable_end;
    }
    if cursor != token_len
        || proposal_add(
            proposal_add(ownership.accepted_tokens, ownership.leaf_tokens)?,
            ownership.gap_tokens,
        )? != token_len
    {
        return Err(SectionPairingProposalStopReason::IncompleteOwnership);
    }
    Ok(SectionPairingProposalSide {
        heading_span,
        paragraph_span,
        heading_page: section.page,
        paragraph_page: paragraph.page,
        heading_trusted_run_id: section.run_id.0,
        paragraph_trusted_run_id: paragraph.run_id.0,
        heading_ordinal_start: section.ordinal_start,
        heading_ordinal_end: section.ordinal_end,
        paragraph_ordinal_start: paragraph.ordinal_start,
        paragraph_ordinal_end: paragraph.ordinal_end,
        ownership,
        ownership_ranges: ranges,
        adoptable_ownership: ownership.accepted_tokens == 0 && ownership.gap_tokens == 0,
    })
}

fn whole_block_span(
    side: &Side<'_>,
    block_index: usize,
) -> Result<TextSpan, SectionPairingProposalStopReason> {
    let block = side
        .blocks
        .get(block_index)
        .ok_or(SectionPairingProposalStopReason::InvalidSourceRange)?;
    let tokens = side
        .canonical
        .get(block_index)
        .ok_or(SectionPairingProposalStopReason::InvalidSourceRange)?;
    if !source_map_is_complete(block, tokens.len()) {
        return Err(SectionPairingProposalStopReason::InvalidSourceRange);
    }
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(1)
        .map_err(|_| SectionPairingProposalStopReason::AllocationFailure)?;
    blocks.push(block.block);
    Ok(TextSpan {
        blocks,
        separator: None,
        canonical_range: ScalarRange {
            start: 0,
            end: block.canonical.text.chars().count(),
        },
        comparable_range: TokenRange {
            start: 0,
            end: tokens.len(),
        },
    })
}

fn analyze_paragraph_gaps(
    pair: SectionPair,
    parent_relation: SectionParentRelation,
    context: &mut GapAnalysisContext<'_, '_>,
    metrics: &mut SectionPairingMetrics,
) -> Result<(), SectionPairingStopReason> {
    let old_section = &context.structures[0].sections[pair.old];
    let new_section = &context.structures[1].sections[pair.new];
    let old = if pair.strong {
        &old_section.strong_paragraphs
    } else {
        &old_section.paragraphs
    };
    let new = if pair.strong {
        &new_section.strong_paragraphs
    } else {
        &new_section.paragraphs
    };
    let mut anchors = Vec::new();
    anchors
        .try_reserve(old.len().min(new.len()))
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    for (old_ordinal, old_block) in old.iter().enumerate() {
        let old_tokens = &context.sides[0].canonical[old_block.block_index];
        let mut old_occurrences = 0usize;
        for block in old {
            if exact_tokens(
                &context.sides[0].canonical[block.block_index],
                old_tokens,
                &mut context.paragraph_work,
                context.limits,
            )? {
                old_occurrences = checked_inc(old_occurrences)?;
            }
        }
        if old_occurrences != 1 {
            continue;
        }
        let mut matched = None;
        let mut match_count = 0usize;
        for (new_ordinal, block) in new.iter().enumerate() {
            if exact_tokens(
                &context.sides[1].canonical[block.block_index],
                old_tokens,
                &mut context.paragraph_work,
                context.limits,
            )? {
                match_count = checked_inc(match_count)?;
                matched.get_or_insert(new_ordinal);
            }
        }
        if let (Some(new_ordinal), 1) = (matched, match_count) {
            anchors
                .try_reserve(1)
                .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
            anchors.push((old_ordinal, new_ordinal));
        }
    }
    let crossing = anchors.windows(2).any(|pair| pair[0].1 >= pair[1].1);
    if crossing {
        metrics.paragraph_anchor_crossing_vetoes =
            checked_inc(metrics.paragraph_anchor_crossing_vetoes)?;
        return Ok(());
    }
    let anchor_target = if pair.strong {
        &mut metrics.strong_paragraph_anchor_pairs
    } else {
        &mut metrics.number_only_paragraph_anchor_pairs
    };
    *anchor_target = checked_add(*anchor_target, anchors.len())?;
    let mut boundaries = Vec::new();
    boundaries
        .try_reserve(checked_add(anchors.len(), 2)?)
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    boundaries.push((usize::MAX, usize::MAX));
    boundaries.extend(anchors);
    boundaries.push((old.len(), new.len()));
    enforce(
        boundaries.len() - 1,
        context.limits.max_gaps,
        SectionPairingStopReason::GapLimit,
    )?;
    for window in boundaries.windows(2) {
        let old_start = if window[0].0 == usize::MAX {
            0
        } else {
            window[0].0 + 1
        };
        let new_start = if window[0].1 == usize::MAX {
            0
        } else {
            window[0].1 + 1
        };
        let old_end = window[1].0;
        let new_end = window[1].1;
        let old_len = old_end.saturating_sub(old_start);
        let new_len = new_end.saturating_sub(new_start);
        if old_len == 0 && new_len == 0 {
            continue;
        }
        record_gap(metrics, pair.strong, old_len, new_len)?;
        if old_len == 1
            && new_len == 1
            && context.sides[0].canonical[old[old_start].block_index]
                != context.sides[1].canonical[new[new_start].block_index]
        {
            let same_unresolved = context.memberships[0][old[old_start].block_index]
                .zip(context.memberships[1][new[new_start].block_index])
                .is_some_and(|(old_span, new_span)| {
                    old_span == new_span
                        && context.alignment.spans[old_span].kind == AlignmentKind::Unresolved
                });
            record_changed_one_to_one(metrics, pair.strong, same_unresolved)?;
            if let Some(span_index) =
                context.memberships[0][old[old_start].block_index].filter(|_| same_unresolved)
            {
                context.proposal_collector.collect(ProposalInput {
                    pair,
                    parent_relation,
                    old_section,
                    new_section,
                    old_paragraph: old[old_start],
                    new_paragraph: new[new_start],
                    unresolved_span_index: span_index,
                    sides: context.sides,
                });
            }
        }
    }
    Ok(())
}

fn exact_tokens(
    left: &[ComparableToken],
    right: &[ComparableToken],
    work: &mut ParagraphWork,
    limits: SectionPairingLimits,
) -> Result<bool, SectionPairingStopReason> {
    work.pair_visits_attempted = checked_inc(work.pair_visits_attempted)?;
    enforce(
        work.pair_visits_attempted,
        limits.max_paragraph_pair_visits,
        SectionPairingStopReason::ParagraphPairLimit,
    )?;
    work.pair_visits_examined = checked_inc(work.pair_visits_examined)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        work.comparisons_attempted = checked_inc(work.comparisons_attempted)?;
        enforce(
            work.comparisons_attempted,
            limits.max_paragraph_token_comparisons,
            SectionPairingStopReason::ParagraphComparisonLimit,
        )?;
        work.comparisons_examined = checked_inc(work.comparisons_examined)?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn record_gap(
    metrics: &mut SectionPairingMetrics,
    strong: bool,
    old_len: usize,
    new_len: usize,
) -> Result<(), SectionPairingStopReason> {
    let target = match (strong, old_len, new_len) {
        (true, 0, _) => &mut metrics.insertion_gaps,
        (true, _, 0) => &mut metrics.deletion_gaps,
        (true, 1, 1) => &mut metrics.one_to_one_gaps,
        (true, _, _) => &mut metrics.many_to_many_gaps,
        (false, 0, _) => &mut metrics.number_only_insertion_gaps,
        (false, _, 0) => &mut metrics.number_only_deletion_gaps,
        (false, 1, 1) => &mut metrics.number_only_one_to_one_gaps,
        (false, _, _) => &mut metrics.number_only_many_to_many_gaps,
    };
    *target = checked_inc(*target)?;
    Ok(())
}

fn record_changed_one_to_one(
    metrics: &mut SectionPairingMetrics,
    strong: bool,
    same_unresolved: bool,
) -> Result<(), SectionPairingStopReason> {
    let target = if strong {
        &mut metrics.changed_one_to_one_gaps
    } else {
        &mut metrics.number_only_changed_one_to_one_gaps
    };
    *target = checked_inc(*target)?;
    if same_unresolved {
        let target = if strong {
            &mut metrics.changed_one_to_one_same_unresolved_span
        } else {
            &mut metrics.number_only_changed_one_to_one_same_unresolved_span
        };
        *target = checked_inc(*target)?;
    }
    Ok(())
}

fn checked_inc(value: usize) -> Result<usize, SectionPairingStopReason> {
    value
        .checked_add(1)
        .ok_or(SectionPairingStopReason::CounterOverflow)
}

fn checked_add(left: usize, right: usize) -> Result<usize, SectionPairingStopReason> {
    left.checked_add(right)
        .ok_or(SectionPairingStopReason::CounterOverflow)
}

fn enforce(
    actual: usize,
    limit: usize,
    reason: SectionPairingStopReason,
) -> Result<(), SectionPairingStopReason> {
    if actual > limit { Err(reason) } else { Ok(()) }
}
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::diff::recovery::ownership::{
        RecoveryLeafKind, RecoveryOwnershipContext, RecoveryOwnershipLedgerBlock,
        RecoveryOwnershipLedgerRange, RecoveryOwnershipRole,
    };
    use crate::{
        alignment::{AlignmentConfidence, AlignmentEvidence, AlignmentSpan, BlockSeparator},
        layout::{BlockId, TrustedRunId},
        model::GlyphId,
        normalize::{
            FontSizeSignature, MappedText, ScalarRange, SourceMapEntry, TextSource, TextSourceAtom,
        },
    };

    use super::*;

    fn block(id: u64, text: &str, size: f64) -> BlockText {
        let mapped = || MappedText {
            text: text.to_owned(),
            source_map: text
                .chars()
                .enumerate()
                .map(|(index, _)| SourceMapEntry {
                    output_range: ScalarRange {
                        start: index,
                        end: index + 1,
                    },
                    source: TextSource {
                        atoms: vec![TextSourceAtom::Glyph(GlyphId((index + 1) as u64))],
                    },
                })
                .collect(),
            unmapped: Vec::new(),
        };
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: mapped(),
            canonical: mapped(),
            matching: text.to_owned(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![1],
            font_size_signatures: Some(
                text.chars()
                    .map(|_| FontSizeSignature::new(&[size]).expect("valid font size"))
                    .collect(),
            ),
            position_signatures: None,
            line_breaks: Some(Vec::new()),
            page_breaks: Some(Vec::new()),
        }
    }

    fn side(blocks: &[BlockText]) -> Side<'_> {
        let canonical = blocks
            .iter()
            .map(|block| block.canonical.comparable_tokens().expect("mapped text"))
            .collect::<Vec<_>>();
        Side {
            blocks,
            index: blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.block, index))
                .collect::<HashMap<_, _>>(),
            total_tokens: canonical.iter().map(Vec::len).sum(),
            canonical,
        }
    }

    fn intervals(count: usize) -> Vec<Option<TrustedRunInterval>> {
        (0..count)
            .map(|ordinal| {
                Some(TrustedRunInterval {
                    run_id: TrustedRunId(1),
                    start: ordinal,
                    end: ordinal + 1,
                })
            })
            .collect()
    }

    fn span(kind: AlignmentKind, old: &[u64], new: &[u64]) -> AlignmentSpan {
        AlignmentSpan {
            kind,
            old: old.iter().copied().map(BlockId).collect(),
            new: new.iter().copied().map(BlockId).collect(),
            score: 1.0,
            canonical_similarity: 1.0,
            score_margin: Some(1.0),
            confidence: AlignmentConfidence::High,
            evidence: if kind == AlignmentKind::Unresolved {
                vec![AlignmentEvidence::ReadingOrderUnknown]
            } else {
                Vec::new()
            },
            old_separator: Some(BlockSeparator::Space),
            new_separator: Some(BlockSeparator::Space),
        }
    }

    fn alignment(spans: Vec<AlignmentSpan>) -> Alignment {
        Alignment {
            spans,
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    fn input<'a>(
        old: &'a [Option<TrustedRunInterval>],
        new: &'a [Option<TrustedRunInterval>],
    ) -> SentenceRecoveryInput<'a> {
        SentenceRecoveryInput {
            old_trusted_run_intervals: old,
            new_trusted_run_intervals: new,
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        }
    }

    fn limits() -> SectionPairingLimits {
        SectionPairingLimits::from_max_tokens(10_000)
    }

    fn ledger(blocks: &[BlockText], ownership: RecoveryOwnership) -> RecoveryOwnershipLedger {
        RecoveryOwnershipLedger {
            blocks: blocks
                .iter()
                .map(|block| RecoveryOwnershipLedgerBlock {
                    block_id: block.block.0,
                    trusted: true,
                    role: RecoveryOwnershipRole::Body,
                    context: RecoveryOwnershipContext::default(),
                })
                .collect(),
            ranges: blocks
                .iter()
                .enumerate()
                .map(|(block_index, block)| RecoveryOwnershipLedgerRange {
                    block_index,
                    canonical_start: 0,
                    canonical_end: block.canonical.text.chars().count(),
                    comparable_start: 0,
                    comparable_end: block.canonical.comparable_tokens().expect("mapped").len(),
                    ownership,
                })
                .collect(),
        }
    }

    #[test]
    fn exact_heading_pair_exposes_changed_one_to_one_gap() {
        let old = vec![
            block(1, "1 Introduction", 20.0),
            block(2, "Old paragraph.", 10.0),
        ];
        let new = vec![
            block(11, "1 Introduction", 20.0),
            block(12, "New paragraph.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let mut old_ledger = ledger(
            &old,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        old_ledger.blocks.reverse();
        for range in &mut old_ledger.ranges {
            range.block_index = old_ledger.blocks.len() - 1 - range.block_index;
        }
        let new_ledger = ledger(
            &new,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.exact_heading_pairs, 1);
        assert_eq!(result.strong_heading_pairs, 1);
        assert_eq!(result.changed_one_to_one_gaps, 1);
        assert_eq!(result.changed_one_to_one_same_unresolved_span, 1);
        let SectionPairingProposalOutcome::Complete(proposals) = &result.proposal_outcome else {
            panic!("proposals must be complete");
        };
        assert_eq!(proposals.len(), 1);
        let proposal = &proposals[0];
        assert_eq!(proposal.view, SectionPairingView::Strong);
        assert_eq!(proposal.heading_evidence, SectionHeadingEvidence::Exact);
        assert_eq!(proposal.parent_relation, SectionParentRelation::Consistent);
        assert_eq!(proposal.topology, SectionPairTopology::Unknown);
        assert_eq!(proposal.heading_match_span_index, 0);
        assert_eq!(proposal.unresolved_span_index, 1);
        assert_eq!(proposal.old.heading_span.blocks, vec![BlockId(1)]);
        assert_eq!(proposal.old.paragraph_span.blocks, vec![BlockId(2)]);
        assert_eq!(proposal.old.heading_ordinal_start, 0);
        assert_eq!(proposal.old.paragraph_ordinal_start, 1);
        assert_eq!(proposal.old.ownership.leaf_tokens, "Old paragraph.".len());
        assert!(proposal.old.adoptable_ownership);
        assert!(matches!(proposal.edits, SectionProposalEdits::Exact(_)));
    }

    #[test]
    fn proposal_records_distance_exceeded_and_accepted_ownership() {
        let old = vec![block(1, "1 Scope", 20.0), block(2, "Old text.", 10.0)];
        let new = vec![block(11, "1 Scope", 20.0), block(12, "New words.", 10.0)];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let old_ledger = ledger(&old, RecoveryOwnership::Accepted);
        let new_ledger = ledger(&new, RecoveryOwnership::Accepted);
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            1,
            limits(),
        );

        let SectionPairingProposalOutcome::Complete(proposals) = result.proposal_outcome else {
            panic!("proposals must be complete");
        };
        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].old.ownership.accepted_tokens,
            "Old text.".len()
        );
        assert!(!proposals[0].old.adoptable_ownership);
        assert_eq!(
            proposals[0].edits,
            SectionProposalEdits::EditDistanceExceeded
        );
    }

    #[test]
    fn proposal_resource_stop_discards_partial_output() {
        let old = vec![
            block(1, "1 First", 20.0),
            block(2, "Old one.", 10.0),
            block(3, "2 Second", 20.0),
            block(4, "Old two.", 10.0),
        ];
        let new = vec![
            block(11, "1 First", 20.0),
            block(12, "New one.", 10.0),
            block(13, "2 Second", 20.0),
            block(14, "New two.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let old_ledger = ledger(
            &old,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let new_ledger = ledger(
            &new,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let complete = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
                span(AlignmentKind::Match, &[3], &[13]),
                span(AlignmentKind::Unresolved, &[4], &[14]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            limits(),
        );
        let SectionPairingProposalOutcome::Complete(proposals) = complete.proposal_outcome else {
            panic!("proposals must be complete");
        };
        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].old.paragraph_span.blocks, vec![BlockId(2)]);
        assert_eq!(proposals[1].old.paragraph_span.blocks, vec![BlockId(4)]);
        assert!(
            proposals
                .iter()
                .all(|proposal| proposal.topology == SectionPairTopology::Monotone)
        );
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
                span(AlignmentKind::Match, &[3], &[13]),
                span(AlignmentKind::Unresolved, &[4], &[14]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            SectionPairingLimits {
                max_proposals: 1,
                ..limits()
            },
        );

        assert!(result.metrics.complete);
        assert_eq!(result.metrics.changed_one_to_one_same_unresolved_span, 2);
        assert_eq!(
            result.proposal_outcome,
            SectionPairingProposalOutcome::Unavailable(
                SectionPairingProposalStopReason::ProposalLimit
            )
        );

        let first_tokens = old[1].canonical.text.len() + new[1].canonical.text.len();
        let work_limited = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
                span(AlignmentKind::Match, &[3], &[13]),
                span(AlignmentKind::Unresolved, &[4], &[14]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            SectionPairingLimits {
                max_proposal_work: proposal_myers_work(first_tokens, 2_048).expect("work fits"),
                ..limits()
            },
        );
        assert_eq!(
            work_limited.proposal_outcome,
            SectionPairingProposalOutcome::Unavailable(
                SectionPairingProposalStopReason::ProposalWorkLimit
            )
        );

        let payload_limited = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
                span(AlignmentKind::Match, &[3], &[13]),
                span(AlignmentKind::Unresolved, &[4], &[14]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            SectionPairingLimits {
                max_proposal_payload_items: 2,
                ..limits()
            },
        );
        assert_eq!(
            payload_limited.proposal_outcome,
            SectionPairingProposalOutcome::Unavailable(
                SectionPairingProposalStopReason::ProposalPayloadLimit
            )
        );
    }

    #[test]
    fn conflicting_dual_view_proposals_fail_closed() {
        let old = vec![
            block(1, "1 Root", 20.0),
            block(2, "1.1 Weak", 10.0),
            block(3, "Old paragraph.", 10.0),
        ];
        let new = vec![
            block(11, "1 Root", 20.0),
            block(12, "1.1 Weak", 10.0),
            block(13, "New paragraph.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let old_ledger = ledger(
            &old,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let new_ledger = ledger(
            &new,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Match, &[2], &[12]),
                span(AlignmentKind::Unresolved, &[3], &[13]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            limits(),
        );

        assert!(result.metrics.complete);
        assert_eq!(
            result.proposal_outcome,
            SectionPairingProposalOutcome::Unavailable(
                SectionPairingProposalStopReason::AmbiguousProposal
            )
        );
    }

    #[test]
    fn myers_work_bound_includes_triangular_trace_cost() {
        assert_eq!(proposal_myers_work(10, 2), Ok(36));
        assert_eq!(proposal_myers_work(10, 0), Ok(11));
    }

    #[test]
    fn default_proposal_limits_are_hard_capped() {
        let limits = SectionPairingLimits::from_max_tokens(5_100_000);

        assert_eq!(limits.max_proposals, MAX_SECTION_PROPOSALS);
        assert_eq!(limits.max_proposal_tokens, MAX_SECTION_PROPOSAL_TOKENS);
        assert_eq!(limits.max_proposal_work, MAX_SECTION_PROPOSAL_WORK);
        assert_eq!(limits.max_proposal_edits, MAX_SECTION_PROPOSAL_EDITS);
        assert_eq!(
            limits.max_proposal_payload_items,
            MAX_SECTION_PROPOSAL_PAYLOAD_ITEMS
        );
        assert_eq!(
            limits.max_proposal_estimated_bytes,
            MAX_SECTION_PROPOSAL_BYTES
        );
        assert_eq!(
            limits.max_ledger_index_entries,
            MAX_SECTION_LEDGER_INDEX_ENTRIES
        );
    }

    #[test]
    fn duplicate_sections_in_one_match_span_are_vetoed() {
        let old = vec![
            block(1, "1 Introduction", 20.0),
            block(2, "2 Introduction", 20.0),
            block(3, "Body.", 10.0),
        ];
        let new = vec![block(11, "1 Introduction", 20.0), block(12, "Body.", 10.0)];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let old_ledger = ledger(
            &old,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let new_ledger = ledger(
            &new,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![span(AlignmentKind::Match, &[1, 2], &[11])]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.ambiguous_span_vetoes, 1);
        assert_eq!(result.strong_heading_pairs, 0);
    }

    #[test]
    fn number_stripped_heading_text_pairs_exactly() {
        let old = vec![block(1, "1 Introduction", 20.0), block(2, "Body.", 10.0)];
        let new = vec![block(11, "2 Introduction", 20.0), block(12, "Body.", 10.0)];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![span(AlignmentKind::Match, &[1], &[11])]),
            input(&old_intervals, &new_intervals),
            None,
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.stripped_heading_pairs, 1);
        assert_eq!(result.strong_heading_pairs, 1);
    }

    #[test]
    fn crossing_paragraph_anchors_veto_gap_analysis() {
        let old = vec![
            block(1, "1 Introduction", 20.0),
            block(2, "Alpha.", 10.0),
            block(3, "Beta.", 10.0),
        ];
        let new = vec![
            block(11, "1 Introduction", 20.0),
            block(12, "Beta.", 10.0),
            block(13, "Alpha.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2, 3], &[12, 13]),
            ]),
            input(&old_intervals, &new_intervals),
            None,
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.paragraph_anchor_crossing_vetoes, 1);
        assert_eq!(
            result.insertion_gaps
                + result.deletion_gaps
                + result.one_to_one_gaps
                + result.many_to_many_gaps,
            0
        );
    }

    #[test]
    fn resource_stops_discard_partial_metrics() {
        let old = vec![block(1, "1 Introduction", 20.0), block(2, "Old.", 10.0)];
        let new = vec![block(11, "1 Introduction", 20.0), block(12, "New.", 10.0)];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let alignment = alignment(vec![
            span(AlignmentKind::Match, &[1], &[11]),
            span(AlignmentKind::Unresolved, &[2], &[12]),
        ]);
        let base = limits();
        let cases = [
            (
                SectionPairingLimits {
                    max_blocks: 0,
                    ..base
                },
                SectionPairingStopReason::BlockLimit,
            ),
            (
                SectionPairingLimits {
                    max_tokens: 0,
                    ..base
                },
                SectionPairingStopReason::TokenLimit,
            ),
            (
                SectionPairingLimits {
                    max_font_evidence: 0,
                    ..base
                },
                SectionPairingStopReason::FontEvidenceLimit,
            ),
            (
                SectionPairingLimits {
                    max_containers: 0,
                    ..base
                },
                SectionPairingStopReason::ContainerLimit,
            ),
            (
                SectionPairingLimits {
                    max_spans: 0,
                    ..base
                },
                SectionPairingStopReason::SpanLimit,
            ),
            (
                SectionPairingLimits {
                    max_paragraphs: 0,
                    ..base
                },
                SectionPairingStopReason::ParagraphLimit,
            ),
            (
                SectionPairingLimits {
                    max_paragraph_pair_visits: 0,
                    ..base
                },
                SectionPairingStopReason::ParagraphPairLimit,
            ),
            (
                SectionPairingLimits {
                    max_paragraph_token_comparisons: 0,
                    ..base
                },
                SectionPairingStopReason::ParagraphComparisonLimit,
            ),
            (
                SectionPairingLimits {
                    max_gaps: 0,
                    ..base
                },
                SectionPairingStopReason::GapLimit,
            ),
        ];

        for (limits, reason) in cases {
            let result = analyze_section_pairing_shadow(
                [&side(&old), &side(&new)],
                &alignment,
                input(&old_intervals, &new_intervals),
                None,
                2_048,
                limits,
            );
            assert_eq!(
                result.metrics,
                SectionPairingMetrics {
                    complete: false,
                    stop_reason: Some(reason),
                    ..SectionPairingMetrics::default()
                }
            );
        }
    }

    #[test]
    fn number_only_heading_keeps_changed_gap_non_adoptable() {
        let old = vec![
            block(1, "1 Introduction", 10.0),
            block(2, "Old paragraph.", 10.0),
        ];
        let new = vec![
            block(11, "1 Introduction", 10.0),
            block(12, "New paragraph.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let old_ledger = ledger(
            &old,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let new_ledger = ledger(
            &new,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        );
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
            ]),
            input(&old_intervals, &new_intervals),
            Some([&old_ledger, &new_ledger]),
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.strong_heading_pairs, 0);
        assert_eq!(result.number_only_section_pairs, 1);
        assert_eq!(result.changed_one_to_one_gaps, 0);
        assert_eq!(result.changed_one_to_one_same_unresolved_span, 0);
        assert_eq!(result.number_only_changed_one_to_one_gaps, 1);
        assert_eq!(
            result.number_only_changed_one_to_one_same_unresolved_span,
            1
        );
        let SectionPairingProposalOutcome::Complete(proposals) = &result.proposal_outcome else {
            panic!("proposals must be complete");
        };
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].view, SectionPairingView::NumberOnly);
    }

    #[test]
    fn paragraph_is_measured_once_in_each_section_view() {
        let old = vec![
            block(1, "1 Root", 20.0),
            block(2, "1.1 Weak", 10.0),
            block(3, "Same paragraph.", 10.0),
        ];
        let new = vec![
            block(11, "1 Root", 20.0),
            block(12, "1.1 Weak", 10.0),
            block(13, "Same paragraph.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Match, &[2], &[12]),
                span(AlignmentKind::Match, &[3], &[13]),
            ]),
            input(&old_intervals, &new_intervals),
            None,
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.old_paragraphs, 1);
        assert_eq!(result.new_paragraphs, 1);
        assert_eq!(result.old_strong_paragraph_memberships, 1);
        assert_eq!(result.new_strong_paragraph_memberships, 1);
        assert_eq!(result.old_number_only_paragraph_memberships, 1);
        assert_eq!(result.new_number_only_paragraph_memberships, 1);
        assert_eq!(result.strong_paragraph_anchor_pairs, 1);
        assert_eq!(result.number_only_paragraph_anchor_pairs, 1);
    }

    #[test]
    fn weak_heading_does_not_change_strong_paragraph_ownership() {
        let old = vec![block(1, "1 Root", 20.0), block(2, "Old paragraph.", 10.0)];
        let new = vec![
            block(11, "1 Root", 20.0),
            block(12, "2 Weak", 10.0),
            block(13, "New paragraph.", 10.0),
        ];
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12, 13]),
            ]),
            input(&old_intervals, &new_intervals),
            None,
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.strong_heading_pairs, 1);
        assert_eq!(result.changed_one_to_one_gaps, 1);
        assert_eq!(result.changed_one_to_one_same_unresolved_span, 1);
    }

    #[test]
    fn heading_without_source_map_is_not_admitted() {
        let mut old = vec![block(1, "1 Introduction", 20.0), block(2, "Body.", 10.0)];
        let new = vec![block(11, "1 Introduction", 20.0), block(12, "Body.", 10.0)];
        old[0].canonical.source_map.clear();
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![span(AlignmentKind::Match, &[1], &[11])]),
            input(&old_intervals, &new_intervals),
            None,
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.old_sections, 0);
        assert_eq!(result.new_sections, 1);
        assert_eq!(result.strong_heading_pairs, 0);
    }

    #[test]
    fn heading_with_incomplete_source_map_is_not_admitted() {
        let mut old = vec![block(1, "1 Introduction", 20.0), block(2, "Body.", 10.0)];
        let new = vec![block(11, "1 Introduction", 20.0), block(12, "Body.", 10.0)];
        old[0].canonical.source_map.pop();
        let old_intervals = intervals(old.len());
        let new_intervals = intervals(new.len());
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![span(AlignmentKind::Match, &[1], &[11])]),
            input(&old_intervals, &new_intervals),
            None,
            2_048,
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.old_sections, 0);
        assert_eq!(result.new_sections, 1);
        assert_eq!(result.strong_heading_pairs, 0);
    }

    #[test]
    fn topology_marks_every_inversion_participant() {
        let structure = |new: bool| SideStructure {
            sections: (0..3)
                .map(|index| Section {
                    block_index: index,
                    parent: None,
                    strong_parent: None,
                    prominent: true,
                    run_id: TrustedRunId(if new { 2 } else { 1 }),
                    ordinal_start: index,
                    ordinal_end: index + 1,
                    page: 1,
                    paragraphs: Vec::new(),
                    strong_paragraphs: Vec::new(),
                })
                .collect(),
            paragraph_count: 0,
            strong_paragraph_memberships: 0,
            number_only_paragraph_memberships: 0,
        };
        let structures = [structure(false), structure(true)];
        let mut pairs = [
            SectionPair {
                old: 0,
                new: 1,
                strong: true,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 0,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
            SectionPair {
                old: 1,
                new: 0,
                strong: true,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 1,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
            SectionPair {
                old: 2,
                new: 2,
                strong: true,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 2,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
        ];
        let mut metrics = SectionPairingMetrics::default();

        classify_pair_topology(&mut pairs, &structures, &mut metrics).expect("topology fits");

        assert_eq!(metrics.crossing_pairs, 2);
        assert_eq!(metrics.monotone_pairs, 1);
        assert_eq!(metrics.topology_unknown_pairs, 0);
        assert_eq!(pairs[0].topology, SectionPairTopology::Crossing);
        assert_eq!(pairs[1].topology, SectionPairTopology::Crossing);
        assert_eq!(pairs[2].topology, SectionPairTopology::Monotone);
    }

    #[test]
    fn topology_is_unknown_without_order_evidence_in_a_run_pair() {
        let section = |run_id, ordinal_start| Section {
            block_index: ordinal_start,
            parent: None,
            strong_parent: None,
            prominent: true,
            run_id: TrustedRunId(run_id),
            ordinal_start,
            ordinal_end: ordinal_start + 1,
            page: 1,
            paragraphs: Vec::new(),
            strong_paragraphs: Vec::new(),
        };
        let structures = [
            SideStructure {
                sections: vec![section(1, 0), section(2, 1)],
                paragraph_count: 0,
                strong_paragraph_memberships: 0,
                number_only_paragraph_memberships: 0,
            },
            SideStructure {
                sections: vec![section(11, 0), section(12, 1)],
                paragraph_count: 0,
                strong_paragraph_memberships: 0,
                number_only_paragraph_memberships: 0,
            },
        ];
        let mut pairs = [
            SectionPair {
                old: 0,
                new: 0,
                strong: true,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 0,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
            SectionPair {
                old: 1,
                new: 1,
                strong: true,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 1,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
        ];
        let mut metrics = SectionPairingMetrics::default();

        classify_pair_topology(&mut pairs, &structures, &mut metrics).expect("topology fits");

        assert_eq!(metrics.monotone_pairs, 0);
        assert_eq!(metrics.crossing_pairs, 0);
        assert_eq!(metrics.topology_unknown_pairs, 2);
        assert!(
            pairs
                .iter()
                .all(|pair| pair.topology == SectionPairTopology::Unknown)
        );
    }

    #[test]
    fn weak_parent_pair_cannot_validate_strong_child_parent() {
        let structure = || SideStructure {
            sections: vec![
                Section {
                    block_index: 0,
                    parent: None,
                    strong_parent: None,
                    prominent: false,
                    run_id: TrustedRunId(1),
                    ordinal_start: 0,
                    ordinal_end: 1,
                    page: 1,
                    paragraphs: Vec::new(),
                    strong_paragraphs: Vec::new(),
                },
                Section {
                    block_index: 1,
                    parent: Some(0),
                    strong_parent: None,
                    prominent: true,
                    run_id: TrustedRunId(1),
                    ordinal_start: 1,
                    ordinal_end: 2,
                    page: 1,
                    paragraphs: Vec::new(),
                    strong_paragraphs: Vec::new(),
                },
            ],
            paragraph_count: 0,
            strong_paragraph_memberships: 0,
            number_only_paragraph_memberships: 0,
        };
        let structures = [structure(), structure()];
        let pairs = [
            SectionPair {
                old: 0,
                new: 0,
                strong: false,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 0,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
            SectionPair {
                old: 1,
                new: 1,
                strong: true,
                heading_evidence: SectionHeadingEvidence::Exact,
                match_span_index: 1,
                match_confidence: AlignmentConfidence::High,
                topology: SectionPairTopology::Unknown,
            },
        ];
        let map = pair_map(&pairs, 2, false).expect("pair map fits");

        assert!(matches!(
            parent_relation(
                &pairs[1],
                &structures,
                &map,
                &pair_map(&pairs, 2, true).expect("reverse map fits")
            ),
            SectionParentRelation::Unknown
        ));
    }
}
