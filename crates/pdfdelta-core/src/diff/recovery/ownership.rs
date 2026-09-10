//! Bounded verification and aggregation for recovery token ownership.

use super::container::StructuralContainerMetrics;
use super::section_pairing::{
    ExactRangeParentOutcome, SectionPairingAnalysis, SectionPairingMetrics,
    SectionPairingProposalOutcome,
};
#[cfg(test)]
const RECOVERY_OWNERSHIP_SAMPLE_LIMIT: usize = 1;

const RECOVERY_LEAF_KIND_COUNT: usize = 8;
const RECOVERY_GAP_REASON_COUNT: usize = 8;
// Each side is capped at half of the combined 64 MiB proof-ledger payload budget.
// Allocator bookkeeping is outside this observable capacity-based bound.
const RECOVERY_OWNERSHIP_LEDGER_SIDE_BYTES_LIMIT: usize = 32 * 1024 * 1024;

/// Structural role shared by every token in one eligible block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnershipRole {
    Body,
    RepeatedHeader,
    RepeatedFooter,
}

/// Metadata for one block whose canonical and comparable token spaces must be owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryEligibleBlock<'a> {
    pub block_id: u64,
    pub canonical_tokens: usize,
    pub comparable_tokens: usize,
    /// Canonical scalar boundary for every comparable-token boundary.
    pub comparable_to_canonical: &'a [usize],
    pub trusted: bool,
    pub role: RecoveryOwnershipRole,
}

/// Recovery unit that owns an unresolved token range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryLeafKind {
    SentenceBody,
    LineBody,
    TrustedRunResidual,
    Heading,
    ListItem,
    Footnote,
    CodeLine,
    TableCell,
}

impl RecoveryLeafKind {
    const fn index(self) -> usize {
        match self {
            Self::SentenceBody => 0,
            Self::LineBody => 1,
            Self::TrustedRunResidual => 2,
            Self::Heading => 3,
            Self::ListItem => 4,
            Self::Footnote => 5,
            Self::CodeLine => 6,
            Self::TableCell => 7,
        }
    }
}

/// Barrier that prevents an eligible token range from becoming a recovery unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryGapReason {
    NoTrustedRun,
    MixedTrustedRuns,
    OrdinalGap,
    RoleBoundary,
    LocationProjectionFailed,
    NormalizationIssue,
    UnmappedChangedEvidence,
    UnsupportedLinePolicy,
}

impl RecoveryGapReason {
    const fn index(self) -> usize {
        match self {
            Self::NoTrustedRun => 0,
            Self::MixedTrustedRuns => 1,
            Self::OrdinalGap => 2,
            Self::RoleBoundary => 3,
            Self::LocationProjectionFailed => 4,
            Self::NormalizationIssue => 5,
            Self::UnmappedChangedEvidence => 6,
            Self::UnsupportedLinePolicy => 7,
        }
    }
}

/// Exclusive owner of one range in the recovery partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnership {
    Accepted,
    Leaf(RecoveryLeafKind),
    Gap(RecoveryGapReason),
}

/// One ordered, block-local ownership range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipRange {
    /// Index into the ordered eligible-block input.
    pub block_index: usize,
    /// Canonical scalar range. This may be zero-width when the range owns only
    /// unmapped comparable tokens located at one scalar boundary.
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub ownership: RecoveryOwnership,
    pub context: RecoveryOwnershipContext,
}

/// Structural evidence retained for one ownership range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipContext {
    pub trusted_run_id: Option<u64>,
    pub ordinal_start: Option<usize>,
    pub ordinal_end: Option<usize>,
    pub region_id: Option<u64>,
    pub page: Option<u32>,
    pub bbox: Option<RecoveryOwnershipRect>,
}

/// Equality-comparable normalized geometry for ownership diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipRect {
    pub min_x_bits: u64,
    pub min_y_bits: u64,
    pub max_x_bits: u64,
    pub max_y_bits: u64,
}

/// Hard bounds for one side's ownership verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipLimits {
    pub max_blocks: usize,
    pub max_ranges: usize,
    pub max_canonical_tokens: usize,
    pub max_comparable_tokens: usize,
}

/// Resource whose configured ownership bound was exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnershipResource {
    Blocks,
    Ranges,
    CanonicalTokens,
    ComparableTokens,
    NormalizationIssues,
    ProjectionEvidenceItems,
    IssueProjectionWork,
}

/// Failure to retain the private proof ledger after the public partition was verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryOwnershipLedgerError {
    ResourceLimit { actual: usize, limit: usize },
    AllocationFailure,
    CounterOverflow,
    InconsistentContext,
}

/// Invalid range property detected before aggregation can be published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnershipRangeError {
    Empty,
    CanonicalReversed,
    CanonicalOutOfBounds,
    ComparableOutOfBounds,
    CanonicalBoundaryMismatch,
}

/// Whole-partition invariant that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnershipInvariant {
    RangeBlockOrder,
    CanonicalCoverage,
    ComparableCoverage,
    MissingEligibleBlock,
    OverlappingOwnership,
    UnsafeAcceptedRange,
}

/// Invalid comparable-to-canonical boundary evidence for one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnershipBlockError {
    BoundaryCount,
    FirstBoundary,
    LastBoundary,
    NonMonotoneBoundary,
    BoundaryStepTooLarge,
}

/// Failure that makes the complete ownership partition unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOwnershipError {
    ResourceLimit {
        resource: RecoveryOwnershipResource,
        actual: usize,
        limit: usize,
    },
    AllocationFailure,
    CounterOverflow,
    InvalidBlock {
        block_index: usize,
        error: RecoveryOwnershipBlockError,
    },
    InvalidRange {
        range_index: usize,
        error: RecoveryOwnershipRangeError,
    },
    InvariantViolation {
        block_index: Option<usize>,
        range_index: Option<usize>,
        invariant: RecoveryOwnershipInvariant,
    },
    CommittedTokenMismatch,
}

/// Trusted/untrusted comparable-token split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipTrustMetrics {
    pub trusted_tokens: usize,
    pub untrusted_tokens: usize,
}

/// Comparable-token split by structural block role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipRoleMetrics {
    pub body_tokens: usize,
    pub repeated_header_tokens: usize,
    pub repeated_footer_tokens: usize,
}

/// Aggregate for one ownership class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipMetrics {
    pub ranges: usize,
    pub canonical_tokens: usize,
    pub comparable_tokens: usize,
    pub max_comparable_tokens: usize,
    pub trust: RecoveryOwnershipTrustMetrics,
    pub roles: RecoveryOwnershipRoleMetrics,
}

/// Deterministic prefix sample retained from the verified ordered partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipSample {
    pub block_id: u64,
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub ownership: RecoveryOwnership,
    pub trusted: bool,
    pub role: RecoveryOwnershipRole,
    pub context: RecoveryOwnershipContext,
}

/// Complete, verified ownership metrics for both document sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipPartitionMetrics {
    pub old: RecoveryOwnershipSideMetrics,
    pub new: RecoveryOwnershipSideMetrics,
}

/// Complete, verified metrics for one document side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipSideMetrics {
    pub total: RecoveryOwnershipMetrics,
    pub accepted: RecoveryOwnershipMetrics,
    pub leaves: [RecoveryOwnershipMetrics; RECOVERY_LEAF_KIND_COUNT],
    pub gaps: [RecoveryOwnershipMetrics; RECOVERY_GAP_REASON_COUNT],
}

/// Heap-backed deterministic samples kept outside copyable aggregate metrics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipSideSamples {
    pub values: Vec<RecoveryOwnershipSample>,
}

/// Block metadata retained once for the proof-only ownership ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryOwnershipLedgerBlock {
    pub block_id: u64,
    pub trusted: bool,
    pub role: RecoveryOwnershipRole,
    pub context: RecoveryOwnershipContext,
}

/// One compact block-local range in the proof-only ownership ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryOwnershipLedgerRange {
    pub block_index: usize,
    pub canonical_start: usize,
    pub canonical_end: usize,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub ownership: RecoveryOwnership,
}

/// Complete ordered ownership ranges retained after partition verification.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecoveryOwnershipLedger {
    pub blocks: Vec<RecoveryOwnershipLedgerBlock>,
    pub ranges: Vec<RecoveryOwnershipLedgerRange>,
}

/// Verified aggregates and bounded samples for one document side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipSideAnalysis {
    pub metrics: RecoveryOwnershipSideMetrics,
    pub samples: RecoveryOwnershipSideSamples,
}

/// Heap-backed deterministic ownership samples for both document sides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryOwnershipPartitionSamples {
    pub old: RecoveryOwnershipSideSamples,
    pub new: RecoveryOwnershipSideSamples,
}

/// Heap-backed verified ownership diagnostics for both document sides.
///
/// This stays outside [`crate::diff::SentenceRecoveryMetrics`] so copying the
/// constant-space recovery counters does not copy the comparatively large
/// per-kind and per-reason aggregate arrays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryOwnershipPartitionAnalysis {
    sides: Vec<RecoveryOwnershipSideAnalysis>,
    structural_container_metrics: Option<StructuralContainerMetrics>,
    section_pairing_analysis: Option<SectionPairingAnalysis>,
}

impl RecoveryOwnershipPartitionAnalysis {
    /// Allocates the two-side payload without relying on an infallible heap
    /// allocation at an untrusted-input boundary.
    pub fn try_new(
        old: RecoveryOwnershipSideAnalysis,
        new: RecoveryOwnershipSideAnalysis,
    ) -> Result<Self, std::collections::TryReserveError> {
        let mut sides = Vec::new();
        sides.try_reserve_exact(2)?;
        sides.push(old);
        sides.push(new);
        Ok(Self {
            sides,
            structural_container_metrics: None,
            section_pairing_analysis: None,
        })
    }

    #[must_use]
    pub fn old_side(&self) -> &RecoveryOwnershipSideAnalysis {
        &self.sides[0]
    }

    #[must_use]
    pub fn new_side(&self) -> &RecoveryOwnershipSideAnalysis {
        &self.sides[1]
    }

    #[must_use]
    pub fn metrics(&self) -> RecoveryOwnershipPartitionMetrics {
        RecoveryOwnershipPartitionMetrics {
            old: self.old_side().metrics,
            new: self.new_side().metrics,
        }
    }
    /// Returns behavior-neutral structural-container diagnostics derived from
    /// this verified ownership partition.
    #[must_use]
    pub fn structural_container_metrics(&self) -> Option<StructuralContainerMetrics> {
        self.structural_container_metrics
    }

    pub(crate) fn set_structural_container_metrics(&mut self, metrics: StructuralContainerMetrics) {
        self.structural_container_metrics = Some(metrics);
    }

    /// Returns behavior-neutral section-pairing diagnostics derived from the
    /// normalized blocks and trusted-run evidence.
    #[must_use]
    pub fn section_pairing_metrics(&self) -> Option<SectionPairingMetrics> {
        self.section_pairing_analysis
            .as_ref()
            .map(|analysis| analysis.metrics)
    }

    /// Returns the atomic section-pairing proposal outcome, when diagnostics ran.
    #[must_use]
    pub fn section_pairing_proposal_outcome(&self) -> Option<&SectionPairingProposalOutcome> {
        self.section_pairing_analysis
            .as_ref()
            .map(|analysis| &analysis.proposal_outcome)
    }

    /// Returns the atomic exact-range parent classification, when diagnostics ran.
    #[must_use]
    pub fn exact_range_parent_outcome(&self) -> Option<&ExactRangeParentOutcome> {
        self.section_pairing_analysis
            .as_ref()
            .map(|analysis| &analysis.exact_range_parent_outcome)
    }

    pub(crate) fn set_section_pairing_analysis(&mut self, analysis: SectionPairingAnalysis) {
        self.section_pairing_analysis = Some(analysis);
    }
}

impl Default for RecoveryOwnershipSideMetrics {
    fn default() -> Self {
        Self {
            total: RecoveryOwnershipMetrics::default(),
            accepted: RecoveryOwnershipMetrics::default(),
            leaves: [RecoveryOwnershipMetrics::default(); RECOVERY_LEAF_KIND_COUNT],
            gaps: [RecoveryOwnershipMetrics::default(); RECOVERY_GAP_REASON_COUNT],
        }
    }
}

impl RecoveryOwnershipSideMetrics {
    #[must_use]
    pub fn leaf(self, kind: RecoveryLeafKind) -> RecoveryOwnershipMetrics {
        self.leaves[kind.index()]
    }

    #[must_use]
    pub fn gap(self, reason: RecoveryGapReason) -> RecoveryOwnershipMetrics {
        self.gaps[reason.index()]
    }
}

/// Verifies and aggregates a complete ordered ownership partition.
///
/// No metrics are returned unless every eligible block is covered exactly once
/// in both canonical and comparable token space. Ranges must be grouped by the
/// input block order and must be contiguous within each block.
///
/// # Errors
///
/// Returns [`RecoveryOwnershipError`] when a configured resource bound is
/// exceeded, allocation or checked arithmetic fails, coordinate evidence is
/// invalid, or either token space is not covered exactly. Every error requires
/// the caller to discard the whole partition.
#[cfg(test)]
pub fn verify_recovery_ownership_partition(
    blocks: &[RecoveryEligibleBlock<'_>],
    ranges: &[RecoveryOwnershipRange],
    limits: RecoveryOwnershipLimits,
) -> Result<RecoveryOwnershipSideMetrics, RecoveryOwnershipError> {
    analyze_recovery_ownership_partition(blocks, ranges, limits).map(|analysis| analysis.metrics)
}

/// Verifies and aggregates a complete partition while retaining one sample
/// for every observed ownership class.
///
/// # Errors
///
/// Returns [`RecoveryOwnershipError`] when a configured resource bound is
/// exceeded, allocation or checked arithmetic fails, coordinate evidence is
/// invalid, or either token space is not covered exactly. No aggregate or
/// sample is returned for a partial partition.
pub fn analyze_recovery_ownership_partition(
    blocks: &[RecoveryEligibleBlock<'_>],
    ranges: &[RecoveryOwnershipRange],
    limits: RecoveryOwnershipLimits,
) -> Result<RecoveryOwnershipSideAnalysis, RecoveryOwnershipError> {
    enforce_count_limit(
        RecoveryOwnershipResource::Blocks,
        blocks.len(),
        limits.max_blocks,
    )?;
    enforce_count_limit(
        RecoveryOwnershipResource::Ranges,
        ranges.len(),
        limits.max_ranges,
    )?;
    let mut expected_canonical = 0usize;
    let mut expected_comparable = 0usize;
    for (block_index, block) in blocks.iter().enumerate() {
        validate_block(block, block_index)?;
        expected_canonical = expected_canonical
            .checked_add(block.canonical_tokens)
            .ok_or(RecoveryOwnershipError::CounterOverflow)?;
        expected_comparable = expected_comparable
            .checked_add(block.comparable_tokens)
            .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    }
    enforce_count_limit(
        RecoveryOwnershipResource::CanonicalTokens,
        expected_canonical,
        limits.max_canonical_tokens,
    )?;
    enforce_count_limit(
        RecoveryOwnershipResource::ComparableTokens,
        expected_comparable,
        limits.max_comparable_tokens,
    )?;

    let mut metrics = RecoveryOwnershipSideMetrics::default();
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(1 + RECOVERY_LEAF_KIND_COUNT + RECOVERY_GAP_REASON_COUNT)
        .map_err(|_| RecoveryOwnershipError::AllocationFailure)?;
    let mut sampled = [false; 1 + RECOVERY_LEAF_KIND_COUNT + RECOVERY_GAP_REASON_COUNT];
    let mut range_index = 0usize;
    for (block_index, block) in blocks.iter().enumerate() {
        let mut canonical_cursor = 0usize;
        let mut comparable_cursor = 0usize;
        while let Some(range) = ranges.get(range_index) {
            if range.block_index < block_index {
                return Err(invariant_error(
                    Some(block_index),
                    Some(range_index),
                    RecoveryOwnershipInvariant::RangeBlockOrder,
                ));
            }
            if range.block_index > block_index {
                break;
            }
            validate_range(range, block, range_index)?;
            if range.canonical_start != canonical_cursor {
                return Err(invariant_error(
                    Some(block_index),
                    Some(range_index),
                    RecoveryOwnershipInvariant::CanonicalCoverage,
                ));
            }
            if range.comparable_start != comparable_cursor {
                return Err(invariant_error(
                    Some(block_index),
                    Some(range_index),
                    RecoveryOwnershipInvariant::ComparableCoverage,
                ));
            }
            canonical_cursor = range.canonical_end;
            comparable_cursor = range.comparable_end;
            aggregate_range(&mut metrics, &mut samples, &mut sampled, block, *range)?;
            range_index = range_index
                .checked_add(1)
                .ok_or(RecoveryOwnershipError::CounterOverflow)?;
        }
        if canonical_cursor != block.canonical_tokens {
            return Err(invariant_error(
                Some(block_index),
                None,
                RecoveryOwnershipInvariant::CanonicalCoverage,
            ));
        }
        if comparable_cursor != block.comparable_tokens {
            return Err(invariant_error(
                Some(block_index),
                None,
                RecoveryOwnershipInvariant::ComparableCoverage,
            ));
        }
    }
    if range_index != ranges.len() {
        return Err(invariant_error(
            None,
            Some(range_index),
            RecoveryOwnershipInvariant::RangeBlockOrder,
        ));
    }
    if metrics.total.canonical_tokens != expected_canonical {
        return Err(invariant_error(
            None,
            None,
            RecoveryOwnershipInvariant::CanonicalCoverage,
        ));
    }
    if metrics.total.comparable_tokens != expected_comparable {
        return Err(invariant_error(
            None,
            None,
            RecoveryOwnershipInvariant::ComparableCoverage,
        ));
    }
    Ok(RecoveryOwnershipSideAnalysis {
        metrics,
        samples: RecoveryOwnershipSideSamples { values: samples },
    })
}

pub(crate) fn build_recovery_ownership_ledger(
    blocks: &[RecoveryEligibleBlock<'_>],
    ranges: &[RecoveryOwnershipRange],
) -> Result<RecoveryOwnershipLedger, RecoveryOwnershipLedgerError> {
    enforce_ledger_byte_limit(blocks.len(), ranges.len())?;

    let mut ledger_blocks = Vec::new();
    ledger_blocks
        .try_reserve_exact(blocks.len())
        .map_err(|_| RecoveryOwnershipLedgerError::AllocationFailure)?;
    let mut ledger_ranges = Vec::new();
    ledger_ranges
        .try_reserve_exact(ranges.len())
        .map_err(|_| RecoveryOwnershipLedgerError::AllocationFailure)?;
    enforce_ledger_byte_limit(ledger_blocks.capacity(), ledger_ranges.capacity())?;
    let mut range_index = 0usize;
    for (block_index, block) in blocks.iter().enumerate() {
        let context = ranges
            .get(range_index)
            .filter(|range| range.block_index == block_index)
            .map_or_else(RecoveryOwnershipContext::default, |range| range.context);
        ledger_blocks.push(RecoveryOwnershipLedgerBlock {
            block_id: block.block_id,
            trusted: block.trusted,
            role: block.role,
            context,
        });
        while let Some(range) = ranges
            .get(range_index)
            .filter(|range| range.block_index == block_index)
        {
            if range.context != context {
                return Err(RecoveryOwnershipLedgerError::InconsistentContext);
            }
            ledger_ranges.push(RecoveryOwnershipLedgerRange {
                block_index: range.block_index,
                canonical_start: range.canonical_start,
                canonical_end: range.canonical_end,
                comparable_start: range.comparable_start,
                comparable_end: range.comparable_end,
                ownership: range.ownership,
            });
            range_index = range_index
                .checked_add(1)
                .ok_or(RecoveryOwnershipLedgerError::CounterOverflow)?;
        }
    }
    Ok(RecoveryOwnershipLedger {
        blocks: ledger_blocks,
        ranges: ledger_ranges,
    })
}

fn enforce_ledger_byte_limit(
    block_count: usize,
    range_count: usize,
) -> Result<(), RecoveryOwnershipLedgerError> {
    let block_bytes = block_count
        .checked_mul(std::mem::size_of::<RecoveryOwnershipLedgerBlock>())
        .ok_or(RecoveryOwnershipLedgerError::CounterOverflow)?;
    let range_bytes = range_count
        .checked_mul(std::mem::size_of::<RecoveryOwnershipLedgerRange>())
        .ok_or(RecoveryOwnershipLedgerError::CounterOverflow)?;
    let ledger_bytes = block_bytes
        .checked_add(range_bytes)
        .ok_or(RecoveryOwnershipLedgerError::CounterOverflow)?;
    if ledger_bytes > RECOVERY_OWNERSHIP_LEDGER_SIDE_BYTES_LIMIT {
        return Err(RecoveryOwnershipLedgerError::ResourceLimit {
            actual: ledger_bytes,
            limit: RECOVERY_OWNERSHIP_LEDGER_SIDE_BYTES_LIMIT,
        });
    }
    Ok(())
}

fn enforce_count_limit(
    resource: RecoveryOwnershipResource,
    actual: usize,
    limit: usize,
) -> Result<(), RecoveryOwnershipError> {
    if actual > limit {
        return Err(RecoveryOwnershipError::ResourceLimit {
            resource,
            actual,
            limit,
        });
    }
    Ok(())
}

fn validate_block(
    block: &RecoveryEligibleBlock<'_>,
    block_index: usize,
) -> Result<(), RecoveryOwnershipError> {
    let expected_boundaries = block
        .comparable_tokens
        .checked_add(1)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    if block.comparable_to_canonical.len() != expected_boundaries {
        return Err(invalid_block(
            block_index,
            RecoveryOwnershipBlockError::BoundaryCount,
        ));
    }
    if block.comparable_to_canonical.first() != Some(&0) {
        return Err(invalid_block(
            block_index,
            RecoveryOwnershipBlockError::FirstBoundary,
        ));
    }
    if block.comparable_to_canonical.last() != Some(&block.canonical_tokens) {
        return Err(invalid_block(
            block_index,
            RecoveryOwnershipBlockError::LastBoundary,
        ));
    }
    for pair in block.comparable_to_canonical.windows(2) {
        let Some(step) = pair[1].checked_sub(pair[0]) else {
            return Err(invalid_block(
                block_index,
                RecoveryOwnershipBlockError::NonMonotoneBoundary,
            ));
        };
        if step > 1 {
            return Err(invalid_block(
                block_index,
                RecoveryOwnershipBlockError::BoundaryStepTooLarge,
            ));
        }
    }
    Ok(())
}

fn validate_range(
    range: &RecoveryOwnershipRange,
    block: &RecoveryEligibleBlock<'_>,
    range_index: usize,
) -> Result<(), RecoveryOwnershipError> {
    if range.canonical_start > range.canonical_end {
        return Err(RecoveryOwnershipError::InvalidRange {
            range_index,
            error: RecoveryOwnershipRangeError::CanonicalReversed,
        });
    }
    if range.comparable_start >= range.comparable_end {
        return Err(RecoveryOwnershipError::InvalidRange {
            range_index,
            error: RecoveryOwnershipRangeError::Empty,
        });
    }
    if range.canonical_end > block.canonical_tokens {
        return Err(RecoveryOwnershipError::InvalidRange {
            range_index,
            error: RecoveryOwnershipRangeError::CanonicalOutOfBounds,
        });
    }
    if range.comparable_end > block.comparable_tokens {
        return Err(RecoveryOwnershipError::InvalidRange {
            range_index,
            error: RecoveryOwnershipRangeError::ComparableOutOfBounds,
        });
    }
    if block.comparable_to_canonical.get(range.comparable_start) != Some(&range.canonical_start)
        || block.comparable_to_canonical.get(range.comparable_end) != Some(&range.canonical_end)
    {
        return Err(RecoveryOwnershipError::InvalidRange {
            range_index,
            error: RecoveryOwnershipRangeError::CanonicalBoundaryMismatch,
        });
    }
    Ok(())
}

fn aggregate_range(
    side: &mut RecoveryOwnershipSideMetrics,
    samples: &mut Vec<RecoveryOwnershipSample>,
    sampled: &mut [bool; 1 + RECOVERY_LEAF_KIND_COUNT + RECOVERY_GAP_REASON_COUNT],
    block: &RecoveryEligibleBlock<'_>,
    range: RecoveryOwnershipRange,
) -> Result<(), RecoveryOwnershipError> {
    let canonical_tokens = range
        .canonical_end
        .checked_sub(range.canonical_start)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    let comparable_tokens = range
        .comparable_end
        .checked_sub(range.comparable_start)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    add_metrics(&mut side.total, block, canonical_tokens, comparable_tokens)?;
    let sample = RecoveryOwnershipSample {
        block_id: block.block_id,
        canonical_start: range.canonical_start,
        canonical_end: range.canonical_end,
        comparable_start: range.comparable_start,
        comparable_end: range.comparable_end,
        ownership: range.ownership,
        trusted: block.trusted,
        role: block.role,
        context: range.context,
    };
    let sample_index = match range.ownership {
        RecoveryOwnership::Accepted => {
            add_metrics(
                &mut side.accepted,
                block,
                canonical_tokens,
                comparable_tokens,
            )?;
            0
        }
        RecoveryOwnership::Leaf(kind) => {
            add_metrics(
                &mut side.leaves[kind.index()],
                block,
                canonical_tokens,
                comparable_tokens,
            )?;
            1 + kind.index()
        }
        RecoveryOwnership::Gap(reason) => {
            add_metrics(
                &mut side.gaps[reason.index()],
                block,
                canonical_tokens,
                comparable_tokens,
            )?;
            1 + RECOVERY_LEAF_KIND_COUNT + reason.index()
        }
    };
    if !sampled[sample_index] {
        samples.push(sample);
        sampled[sample_index] = true;
    }
    Ok(())
}

fn add_metrics(
    metrics: &mut RecoveryOwnershipMetrics,
    block: &RecoveryEligibleBlock<'_>,
    canonical_tokens: usize,
    comparable_tokens: usize,
) -> Result<(), RecoveryOwnershipError> {
    metrics.ranges = metrics
        .ranges
        .checked_add(1)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    metrics.canonical_tokens = metrics
        .canonical_tokens
        .checked_add(canonical_tokens)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    metrics.comparable_tokens = metrics
        .comparable_tokens
        .checked_add(comparable_tokens)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    metrics.max_comparable_tokens = metrics.max_comparable_tokens.max(comparable_tokens);
    let trust = if block.trusted {
        &mut metrics.trust.trusted_tokens
    } else {
        &mut metrics.trust.untrusted_tokens
    };
    *trust = trust
        .checked_add(comparable_tokens)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    let role = match block.role {
        RecoveryOwnershipRole::Body => &mut metrics.roles.body_tokens,
        RecoveryOwnershipRole::RepeatedHeader => &mut metrics.roles.repeated_header_tokens,
        RecoveryOwnershipRole::RepeatedFooter => &mut metrics.roles.repeated_footer_tokens,
    };
    *role = role
        .checked_add(comparable_tokens)
        .ok_or(RecoveryOwnershipError::CounterOverflow)?;
    Ok(())
}

fn invariant_error(
    block_index: Option<usize>,
    range_index: Option<usize>,
    invariant: RecoveryOwnershipInvariant,
) -> RecoveryOwnershipError {
    RecoveryOwnershipError::InvariantViolation {
        block_index,
        range_index,
        invariant,
    }
}

fn invalid_block(block_index: usize, error: RecoveryOwnershipBlockError) -> RecoveryOwnershipError {
    RecoveryOwnershipError::InvalidBlock { block_index, error }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn identity_boundaries() -> [usize; 1_025] {
        let mut boundaries = [0; 1_025];
        let mut index = 0;
        while index < boundaries.len() {
            boundaries[index] = index;
            index += 1;
        }
        boundaries
    }

    const IDENTITY_BOUNDARIES: [usize; 1_025] = identity_boundaries();

    const LIMITS: RecoveryOwnershipLimits = RecoveryOwnershipLimits {
        max_blocks: 32,
        max_ranges: 64,
        max_canonical_tokens: 1_024,
        max_comparable_tokens: 1_024,
    };

    fn block(
        block_id: u64,
        tokens: usize,
        trusted: bool,
        role: RecoveryOwnershipRole,
    ) -> RecoveryEligibleBlock<'static> {
        RecoveryEligibleBlock {
            block_id,
            canonical_tokens: tokens,
            comparable_tokens: tokens,
            comparable_to_canonical: &IDENTITY_BOUNDARIES[..tokens + 1],
            trusted,
            role,
        }
    }

    fn range(
        block_index: usize,
        start: usize,
        end: usize,
        ownership: RecoveryOwnership,
    ) -> RecoveryOwnershipRange {
        RecoveryOwnershipRange {
            block_index,
            canonical_start: start,
            canonical_end: end,
            comparable_start: start,
            comparable_end: end,
            ownership,
            context: RecoveryOwnershipContext::default(),
        }
    }

    #[test]
    fn section_pairing_analysis_is_absent_until_diagnostic_gate_runs() {
        let side = RecoveryOwnershipSideAnalysis {
            metrics: RecoveryOwnershipSideMetrics::default(),
            samples: RecoveryOwnershipSideSamples::default(),
        };
        let analysis = RecoveryOwnershipPartitionAnalysis::try_new(side.clone(), side)
            .expect("two-side allocation fits");

        assert_eq!(analysis.section_pairing_metrics(), None);
        assert_eq!(analysis.section_pairing_proposal_outcome(), None);
    }

    #[test]
    fn verifies_complete_partition_and_aggregates_classes() {
        let blocks = [
            block(1, 5, true, RecoveryOwnershipRole::Body),
            block(2, 4, false, RecoveryOwnershipRole::RepeatedFooter),
        ];
        let ranges = [
            range(0, 0, 2, RecoveryOwnership::Accepted),
            range(
                0,
                2,
                5,
                RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
            ),
            range(
                1,
                0,
                4,
                RecoveryOwnership::Gap(RecoveryGapReason::NoTrustedRun),
            ),
        ];

        let analysis = analyze_recovery_ownership_partition(&blocks, &ranges, LIMITS)
            .expect("partition is valid");
        let ledger = build_recovery_ownership_ledger(&blocks, &ranges)
            .expect("verified partition fits the private ledger budget");
        let metrics = analysis.metrics;

        assert_eq!(metrics.total.ranges, 3);
        assert_eq!(metrics.total.comparable_tokens, 9);
        assert_eq!(metrics.total.max_comparable_tokens, 4);
        assert_eq!(metrics.total.trust.trusted_tokens, 5);
        assert_eq!(metrics.total.trust.untrusted_tokens, 4);
        assert_eq!(metrics.total.roles.body_tokens, 5);
        assert_eq!(metrics.total.roles.repeated_footer_tokens, 4);
        assert_eq!(metrics.accepted.comparable_tokens, 2);
        assert_eq!(
            metrics
                .leaf(RecoveryLeafKind::SentenceBody)
                .comparable_tokens,
            3
        );
        assert_eq!(
            metrics
                .gap(RecoveryGapReason::NoTrustedRun)
                .comparable_tokens,
            4
        );
        assert_eq!(
            analysis.samples.values,
            ranges
                .iter()
                .map(|range| RecoveryOwnershipSample {
                    block_id: blocks[range.block_index].block_id,
                    canonical_start: range.canonical_start,
                    canonical_end: range.canonical_end,
                    comparable_start: range.comparable_start,
                    comparable_end: range.comparable_end,
                    ownership: range.ownership,
                    trusted: blocks[range.block_index].trusted,
                    role: blocks[range.block_index].role,
                    context: range.context,
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ledger.ranges,
            ranges
                .iter()
                .map(|range| RecoveryOwnershipLedgerRange {
                    block_index: range.block_index,
                    canonical_start: range.canonical_start,
                    canonical_end: range.canonical_end,
                    comparable_start: range.comparable_start,
                    comparable_end: range.comparable_end,
                    ownership: range.ownership,
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn retains_every_ownership_class_in_partition_order() {
        let blocks = [
            block(10, 4, true, RecoveryOwnershipRole::Body),
            block(20, 2, false, RecoveryOwnershipRole::RepeatedHeader),
        ];
        let context = RecoveryOwnershipContext {
            trusted_run_id: Some(7),
            ordinal_start: Some(2),
            ordinal_end: Some(4),
            region_id: Some(11),
            page: Some(3),
            bbox: None,
        };
        let ranges = [
            RecoveryOwnershipRange {
                context,
                ..range(0, 0, 1, RecoveryOwnership::Accepted)
            },
            RecoveryOwnershipRange {
                context,
                ..range(
                    0,
                    1,
                    3,
                    RecoveryOwnership::Leaf(RecoveryLeafKind::TrustedRunResidual),
                )
            },
            RecoveryOwnershipRange {
                context,
                ..range(
                    0,
                    3,
                    4,
                    RecoveryOwnership::Gap(RecoveryGapReason::OrdinalGap),
                )
            },
            range(1, 0, 2, RecoveryOwnership::Leaf(RecoveryLeafKind::LineBody)),
        ];

        let analysis = analyze_recovery_ownership_partition(&blocks, &ranges, LIMITS)
            .expect("partition is valid");
        let ledger = build_recovery_ownership_ledger(&blocks, &ranges)
            .expect("verified partition fits the private ledger budget");

        assert_eq!(analysis.metrics.total.ranges, ranges.len());
        assert_eq!(ledger.blocks.len(), blocks.len());
        assert_eq!(ledger.ranges.len(), ranges.len());
        assert_eq!(ledger.blocks[0].block_id, 10);
        assert_eq!(ledger.blocks[0].context, context);
        assert_eq!(
            ledger
                .ranges
                .iter()
                .map(|entry| entry.ownership)
                .collect::<Vec<_>>(),
            ranges
                .iter()
                .map(|range| range.ownership)
                .collect::<Vec<_>>()
        );
        assert_eq!(ledger.ranges[3].block_index, 1);
        assert_eq!(ledger.blocks[1].block_id, 20);
        assert!(!ledger.blocks[1].trusted);
        assert_eq!(ledger.blocks[1].role, RecoveryOwnershipRole::RepeatedHeader);
    }

    #[test]
    fn does_not_return_a_ledger_for_invalid_or_resource_stopped_partitions() {
        let blocks = [block(1, 4, true, RecoveryOwnershipRole::Body)];
        let overlapping = [
            range(0, 0, 3, RecoveryOwnership::Accepted),
            range(
                0,
                2,
                4,
                RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
            ),
        ];
        assert!(analyze_recovery_ownership_partition(&blocks, &overlapping, LIMITS).is_err());

        let complete = [range(0, 0, 4, RecoveryOwnership::Accepted)];
        let stopped = analyze_recovery_ownership_partition(
            &blocks,
            &complete,
            RecoveryOwnershipLimits {
                max_ranges: 0,
                ..LIMITS
            },
        );
        assert_eq!(
            stopped,
            Err(RecoveryOwnershipError::ResourceLimit {
                resource: RecoveryOwnershipResource::Ranges,
                actual: 1,
                limit: 0,
            })
        );
    }

    #[test]
    fn ledger_byte_limit_stops_before_allocation() {
        let range_size = std::mem::size_of::<RecoveryOwnershipLedgerRange>();
        let range_count = RECOVERY_OWNERSHIP_LEDGER_SIDE_BYTES_LIMIT / range_size + 1;

        assert_eq!(
            enforce_ledger_byte_limit(0, range_count),
            Err(RecoveryOwnershipLedgerError::ResourceLimit {
                actual: range_count * range_size,
                limit: RECOVERY_OWNERSHIP_LEDGER_SIDE_BYTES_LIMIT,
            })
        );
    }

    #[test]
    fn rejects_empty_and_out_of_bounds_ranges() {
        let blocks = [block(1, 3, true, RecoveryOwnershipRole::Body)];
        let empty = [range(0, 0, 0, RecoveryOwnership::Accepted)];
        assert_eq!(
            verify_recovery_ownership_partition(&blocks, &empty, LIMITS),
            Err(RecoveryOwnershipError::InvalidRange {
                range_index: 0,
                error: RecoveryOwnershipRangeError::Empty,
            })
        );

        let out_of_bounds = [range(0, 0, 4, RecoveryOwnership::Accepted)];
        assert_eq!(
            verify_recovery_ownership_partition(&blocks, &out_of_bounds, LIMITS),
            Err(RecoveryOwnershipError::InvalidRange {
                range_index: 0,
                error: RecoveryOwnershipRangeError::CanonicalOutOfBounds,
            })
        );
    }

    #[test]
    fn rejects_gaps_overlaps_and_block_reordering() {
        let blocks = [block(1, 4, true, RecoveryOwnershipRole::Body)];
        for ranges in [
            vec![range(0, 0, 2, RecoveryOwnership::Accepted)],
            vec![
                range(0, 0, 3, RecoveryOwnership::Accepted),
                range(0, 2, 4, RecoveryOwnership::Accepted),
            ],
            vec![
                range(0, 0, 2, RecoveryOwnership::Accepted),
                range(0, 3, 4, RecoveryOwnership::Accepted),
            ],
        ] {
            assert!(matches!(
                verify_recovery_ownership_partition(&blocks, &ranges, LIMITS),
                Err(RecoveryOwnershipError::InvariantViolation {
                    invariant: RecoveryOwnershipInvariant::CanonicalCoverage,
                    ..
                })
            ));
        }

        let blocks = [
            block(1, 1, true, RecoveryOwnershipRole::Body),
            block(2, 1, true, RecoveryOwnershipRole::Body),
        ];
        let reversed = [
            range(1, 0, 1, RecoveryOwnership::Accepted),
            range(0, 0, 1, RecoveryOwnership::Accepted),
        ];
        assert!(matches!(
            verify_recovery_ownership_partition(&blocks, &reversed, LIMITS),
            Err(RecoveryOwnershipError::InvariantViolation {
                invariant: RecoveryOwnershipInvariant::CanonicalCoverage,
                ..
            })
        ));
    }

    #[test]
    fn verifies_canonical_and_comparable_coverage_independently() {
        let blocks = [RecoveryEligibleBlock {
            block_id: 1,
            canonical_tokens: 3,
            comparable_tokens: 4,
            comparable_to_canonical: &[0, 1, 2, 3, 3],
            trusted: true,
            role: RecoveryOwnershipRole::Body,
        }];
        let ranges = [RecoveryOwnershipRange {
            block_index: 0,
            canonical_start: 0,
            canonical_end: 3,
            comparable_start: 0,
            comparable_end: 4,
            ownership: RecoveryOwnership::Gap(RecoveryGapReason::UnmappedChangedEvidence),
            context: RecoveryOwnershipContext::default(),
        }];

        let metrics = verify_recovery_ownership_partition(&blocks, &ranges, LIMITS)
            .expect("both token spaces are fully covered");

        assert_eq!(metrics.total.canonical_tokens, 3);
        assert_eq!(metrics.total.comparable_tokens, 4);
    }

    #[test]
    fn permits_zero_width_canonical_range_for_unmapped_token_ownership() {
        let blocks = [RecoveryEligibleBlock {
            block_id: 1,
            canonical_tokens: 2,
            comparable_tokens: 3,
            comparable_to_canonical: &[0, 1, 1, 2],
            trusted: true,
            role: RecoveryOwnershipRole::Body,
        }];
        let ranges = [
            range(0, 0, 1, RecoveryOwnership::Accepted),
            RecoveryOwnershipRange {
                block_index: 0,
                canonical_start: 1,
                canonical_end: 1,
                comparable_start: 1,
                comparable_end: 2,
                ownership: RecoveryOwnership::Gap(RecoveryGapReason::UnmappedChangedEvidence),
                context: RecoveryOwnershipContext::default(),
            },
            RecoveryOwnershipRange {
                block_index: 0,
                canonical_start: 1,
                canonical_end: 2,
                comparable_start: 2,
                comparable_end: 3,
                ownership: RecoveryOwnership::Accepted,
                context: RecoveryOwnershipContext::default(),
            },
        ];

        let metrics = verify_recovery_ownership_partition(&blocks, &ranges, LIMITS)
            .expect("unmapped token owns comparable space at a scalar boundary");

        assert_eq!(metrics.total.canonical_tokens, 2);
        assert_eq!(metrics.total.comparable_tokens, 3);
        assert_eq!(
            metrics
                .gap(RecoveryGapReason::UnmappedChangedEvidence)
                .canonical_tokens,
            0
        );
        assert_eq!(
            metrics
                .gap(RecoveryGapReason::UnmappedChangedEvidence)
                .comparable_tokens,
            1
        );
    }

    #[test]
    fn rejects_reversed_canonical_range() {
        let blocks = [block(1, 3, true, RecoveryOwnershipRole::Body)];
        let ranges = [RecoveryOwnershipRange {
            block_index: 0,
            canonical_start: 2,
            canonical_end: 1,
            comparable_start: 0,
            comparable_end: 1,
            ownership: RecoveryOwnership::Accepted,
            context: RecoveryOwnershipContext::default(),
        }];

        assert_eq!(
            verify_recovery_ownership_partition(&blocks, &ranges, LIMITS),
            Err(RecoveryOwnershipError::InvalidRange {
                range_index: 0,
                error: RecoveryOwnershipRangeError::CanonicalReversed,
            })
        );
    }

    #[test]
    fn rejects_ranges_incompatible_with_exact_boundary_evidence() {
        let blocks = [RecoveryEligibleBlock {
            block_id: 1,
            canonical_tokens: 2,
            comparable_tokens: 3,
            comparable_to_canonical: &[0, 0, 1, 2],
            trusted: true,
            role: RecoveryOwnershipRole::Body,
        }];
        let impossible = [
            RecoveryOwnershipRange {
                block_index: 0,
                canonical_start: 0,
                canonical_end: 1,
                comparable_start: 0,
                comparable_end: 1,
                ownership: RecoveryOwnership::Accepted,
                context: RecoveryOwnershipContext::default(),
            },
            RecoveryOwnershipRange {
                block_index: 0,
                canonical_start: 1,
                canonical_end: 2,
                comparable_start: 1,
                comparable_end: 3,
                ownership: RecoveryOwnership::Accepted,
                context: RecoveryOwnershipContext::default(),
            },
        ];

        assert_eq!(
            verify_recovery_ownership_partition(&blocks, &impossible, LIMITS),
            Err(RecoveryOwnershipError::InvalidRange {
                range_index: 0,
                error: RecoveryOwnershipRangeError::CanonicalBoundaryMismatch,
            })
        );
    }

    #[test]
    fn rejects_invalid_comparable_boundary_maps_with_typed_errors() {
        let cases = [
            (
                RecoveryEligibleBlock {
                    block_id: 1,
                    canonical_tokens: 1,
                    comparable_tokens: 1,
                    comparable_to_canonical: &[0],
                    trusted: true,
                    role: RecoveryOwnershipRole::Body,
                },
                RecoveryOwnershipBlockError::BoundaryCount,
            ),
            (
                RecoveryEligibleBlock {
                    block_id: 1,
                    canonical_tokens: 1,
                    comparable_tokens: 1,
                    comparable_to_canonical: &[1, 1],
                    trusted: true,
                    role: RecoveryOwnershipRole::Body,
                },
                RecoveryOwnershipBlockError::FirstBoundary,
            ),
            (
                RecoveryEligibleBlock {
                    block_id: 1,
                    canonical_tokens: 1,
                    comparable_tokens: 1,
                    comparable_to_canonical: &[0, 0],
                    trusted: true,
                    role: RecoveryOwnershipRole::Body,
                },
                RecoveryOwnershipBlockError::LastBoundary,
            ),
            (
                RecoveryEligibleBlock {
                    block_id: 1,
                    canonical_tokens: 1,
                    comparable_tokens: 3,
                    comparable_to_canonical: &[0, 1, 0, 1],
                    trusted: true,
                    role: RecoveryOwnershipRole::Body,
                },
                RecoveryOwnershipBlockError::NonMonotoneBoundary,
            ),
            (
                RecoveryEligibleBlock {
                    block_id: 1,
                    canonical_tokens: 2,
                    comparable_tokens: 1,
                    comparable_to_canonical: &[0, 2],
                    trusted: true,
                    role: RecoveryOwnershipRole::Body,
                },
                RecoveryOwnershipBlockError::BoundaryStepTooLarge,
            ),
        ];

        for (block, error) in cases {
            assert_eq!(
                verify_recovery_ownership_partition(&[block], &[], LIMITS),
                Err(RecoveryOwnershipError::InvalidBlock {
                    block_index: 0,
                    error,
                })
            );
        }
    }

    #[test]
    fn input_order_does_not_depend_on_numeric_block_ids() {
        let blocks = [
            block(10, 1, true, RecoveryOwnershipRole::Body),
            block(3, 1, true, RecoveryOwnershipRole::Body),
        ];
        let ranges = [
            range(0, 0, 1, RecoveryOwnership::Accepted),
            range(1, 0, 1, RecoveryOwnership::Accepted),
        ];

        let metrics = verify_recovery_ownership_partition(&blocks, &ranges, LIMITS)
            .expect("slice order defines deterministic document order");

        assert_eq!(metrics.total.comparable_tokens, 2);
    }

    #[test]
    fn enforces_resource_limits_before_publishing_metrics() {
        let blocks = [block(1, 5, true, RecoveryOwnershipRole::Body)];
        let ranges = [range(0, 0, 5, RecoveryOwnership::Accepted)];
        let limits = RecoveryOwnershipLimits {
            max_comparable_tokens: 4,
            ..LIMITS
        };

        assert_eq!(
            verify_recovery_ownership_partition(&blocks, &ranges, limits),
            Err(RecoveryOwnershipError::ResourceLimit {
                resource: RecoveryOwnershipResource::ComparableTokens,
                actual: 5,
                limit: 4,
            })
        );
    }

    #[test]
    fn reports_counter_overflow_without_partial_metrics() {
        let blocks = [RecoveryEligibleBlock {
            block_id: 1,
            canonical_tokens: 0,
            comparable_tokens: usize::MAX,
            comparable_to_canonical: &[],
            trusted: true,
            role: RecoveryOwnershipRole::Body,
        }];

        assert_eq!(
            verify_recovery_ownership_partition(
                &blocks,
                &[],
                RecoveryOwnershipLimits {
                    max_blocks: usize::MAX,
                    max_ranges: usize::MAX,
                    max_canonical_tokens: usize::MAX,
                    max_comparable_tokens: usize::MAX,
                },
            ),
            Err(RecoveryOwnershipError::CounterOverflow)
        );
    }

    #[test]
    fn samples_are_a_fixed_deterministic_prefix() {
        let blocks = [block(
            1,
            RECOVERY_OWNERSHIP_SAMPLE_LIMIT + 2,
            true,
            RecoveryOwnershipRole::Body,
        )];
        let mut ranges = (0..RECOVERY_OWNERSHIP_SAMPLE_LIMIT)
            .map(|index| range(0, index, index + 1, RecoveryOwnership::Accepted))
            .collect::<Vec<_>>();
        ranges.push(range(
            0,
            RECOVERY_OWNERSHIP_SAMPLE_LIMIT,
            RECOVERY_OWNERSHIP_SAMPLE_LIMIT + 1,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody),
        ));
        ranges.push(range(
            0,
            RECOVERY_OWNERSHIP_SAMPLE_LIMIT + 1,
            RECOVERY_OWNERSHIP_SAMPLE_LIMIT + 2,
            RecoveryOwnership::Gap(RecoveryGapReason::NoTrustedRun),
        ));

        let analysis = analyze_recovery_ownership_partition(&blocks, &ranges, LIMITS)
            .expect("partition is valid");

        assert_eq!(analysis.samples.values.len(), 3);
        assert_eq!(analysis.samples.values[0].comparable_start, 0);
        assert_eq!(
            analysis.samples.values[1].ownership,
            RecoveryOwnership::Leaf(RecoveryLeafKind::SentenceBody)
        );
        assert_eq!(
            analysis.samples.values[2].ownership,
            RecoveryOwnership::Gap(RecoveryGapReason::NoTrustedRun)
        );
    }
}
