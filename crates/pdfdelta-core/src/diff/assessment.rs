//! Evidence assessment and exclusive source ownership for comparison results.
//!
//! Candidate ranking is deliberately separate from relation acceptance. An
//! exact edit script describes a proposed pair; it does not establish that
//! the pair belongs to the same source-backed comparison domain.

mod claims;
mod document_claims;
mod equal_fragment;
mod exact;
mod footers;
mod hypotheses;
mod local;
mod mandatory_equal;
mod normalization;
mod raw_source;
mod review;
mod semantic;
mod suffix;
mod validation;
mod views;

pub use document_claims::{LocalTextClaims, LocalTextSide, local_text_claims};

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use super::{
    ChangeEvent, ChangeKind, Comparison, Coverage, DiffOptions, FormattingChange, GroupText,
    ProvenChangedRegion, SentenceRecoveryInput, Side, TextSpan, TokenRange, UnresolvedRegion,
    myers, sentence,
};
use crate::{
    Error, Result,
    alignment::{Alignment, AlignmentEvidence, AlignmentKind, BlockSeparator},
    layout::BlockId,
    normalize::{ComparableToken, ScalarRange},
};
use claims::{allocation_error, charge, invalid, limit_error};

/// Version of the evidence and resolution-accounting policy.
pub const ASSESSMENT_POLICY_VERSION: u32 = 1;

/// Enumerates every binary word up to `max_length`, shortest first.
///
/// The exact and semantic uniqueness tests use the same oracle corpus; this
/// lives with the assessment parent so both tests share one definition.
#[cfg(test)]
pub(super) fn all_words(max_length: usize) -> Vec<Vec<u8>> {
    fn append(words: &mut Vec<Vec<u8>>, current: &mut Vec<u8>, remaining: usize) {
        if remaining == 0 {
            words.push(current.clone());
            return;
        }
        for token in 0..=1 {
            current.push(token);
            append(words, current, remaining - 1);
            current.pop();
        }
    }
    let mut words = Vec::new();
    for length in 0..=max_length {
        append(&mut words, &mut Vec::new(), length);
    }
    words
}

/// Why a proposed correspondence cannot establish a localized change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssessmentReason {
    UnknownReadingOrder,
    InferredReadingOrder,
    ExtractionGap,
    NormalizationUncertainty,
    CompetingCorrespondence,
    AmbiguousEditLocation,
    SearchIncomplete,
    DomainNotClosed,
    SourceEvidenceMissing,
    WorkLimit,
    OutputLimit,
}

/// A model assumption retained separately from exact token evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComparisonAssumption {
    /// The supplied block order is a supported reading order for this domain.
    InputReadingOrder,
    /// Comparison uses reversible canonical normalization rather than raw text.
    CanonicalNormalization,
    /// Unmapped token identity depends on the supplied font-program identity.
    UnmappedFontIdentity,
    /// A source-backed layout view supplied a synthetic inter-block separator.
    ReconstructedSpacing,
    /// Ordered correspondence does not cross declared extraction,
    /// normalization, or reading-order barriers outside this domain.
    LocalEvidenceBoundaries,
    /// Every retained discretionary line-end hyphen interpretation is included.
    AlternativeLineBreakNormalization,
    /// An exhaustive terminal-line candidate population supplied catalog/form
    /// keys and source context to the common ownership solver. Page ordinals
    /// are evidence attributes, not identity keys.
    CatalogFooterCorrespondence,
    /// A closed single-block line is related by one raw rigid translation
    /// shared with an independently established neighbour correspondence.
    /// The move itself is part of the evidence; no reading-order relaxation
    /// and no pixel tolerance is involved.
    RigidTranslation,
    /// A whole source-bounded line is the unique source block between two
    /// independently established correspondences in the same column band on
    /// both sides. The region correspondence is proven by the boundaries; the
    /// tokens are still compared with the strict minimal-edit uniqueness and
    /// the global reading order is not promoted.
    BracketedRegion,
    /// A closed source-bounded line whose edit location is ambiguous has one
    /// maximum equal-token matching whose matched tokens keep an exactly equal
    /// raw displacement from the anchor common to every maximum matching. The
    /// correspondence closure is independent; only the edit location is
    /// proven by the exact displacement.
    ExactTextDisplacement,
    /// A maximal literal, same-direction, exact-delta suffix of a source block
    /// reaching both original block ends is related by one raw rigid
    /// translation that matches an independently established whole-block
    /// correspondence with the same exact delta on the same page. The suffix
    /// premise claims only the selected source correspondence: the unselected
    /// same-block prefix, the outside endpoints of cut-adjacent breaks and any
    /// geometry outside the range stay unclaimed, and no reading order or
    /// pixel tolerance is involved.
    RigidSuffixTranslation,
    /// Every token of a localized child span lies on a mandatory matched pair
    /// of the parent domain's maximum LCS matchings: the equal correspondence
    /// is fixed on every optimal path even though the parent edit location
    /// stays ambiguous.
    MandatoryMatchingEquality,
    /// Literal mandatory equal pairs belong to an independently isolated,
    /// complete exact whole single-block parent whose two source blocks occur
    /// on different pages. The original full-parent matching supplies the
    /// correspondence; strict raw/canonical sources and no-sharing checks on
    /// both documents still apply. This neither resolves the remaining edit
    /// ambiguity nor grants ownership outside the selected paired child.
    PageShiftedMandatoryMatchingEquality,
    /// Two consecutive equal-token pairs occur on every maximum matching of
    /// the complete, source-closed parent domain. The child is their interior
    /// containing range, excluding both endpoints. This establishes boundary
    /// correspondence only: it neither fixes individual edits nor grants any
    /// token or glyph ownership.
    MandatoryMatchingBoundaries,
    /// The candidate sits still and is proven by an independently
    /// established stationary neighbour correspondence; raw coordinates are
    /// never used to adopt the move.
    StationaryNeighbour,
    /// A whole source-bounded original member sits still, is carried by an
    /// independently established stationary neighbour, and occupies the same
    /// page and per-token position column on both sides while its token text
    /// differs. The position column is correspondence evidence for the exact
    /// diff, not an equality proof; the differing text is reported as a
    /// replacement.
    PositionedReplacement,
    /// One exact whole-block pair whose raw source projection is isomorphic.
    RawSourceEquality,
    /// A strict-closed equal domain whose selected tokens each carry one real
    /// glyph with one contiguous single-scalar raw counterpart and bit-exact
    /// finite horizontal positions was adopted for its own projected source
    /// intervals. The surrounding unresolved regions, the candidate and
    /// changed-ownership protections and the global reading order are
    /// unchanged.
    EqualFragmentSourcePositions,
    /// A strict-closed equal domain whose selected tokens each carry one real
    /// glyph with bit-exact finite horizontal positions was adopted for its own
    /// projected source intervals, with one or more internal raw gaps certified
    /// as deleted soft line breaks. Each certified gap is exactly one raw
    /// newline scalar whose single raw source atom is a `LineBreak` naming the
    /// two adjacent selected glyphs, and exactly one structurally validated
    /// `SoftLineBreak` event deletes that raw range to a zero-length canonical
    /// range at the following selected scalar; the certified break offsets
    /// relative to the selected interval agree on both sides. The surrounding
    /// unresolved regions, the candidate and changed-ownership protections and
    /// the global reading order are unchanged.
    EqualFragmentInternalDeletedBreak,
}

/// Whether the exact search required for a relation finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchCompleteness {
    /// All alternatives in the declared correspondence model were examined.
    Complete,
    /// The claim needs work that was not completed within the shared budget.
    Incomplete,
}

/// Result of assessing one proposed relation before final event emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationOutcome {
    Established,
    Tentative,
}

/// Source-backed domain and evidence for one correspondence claim.
///
/// `parent` refers to an earlier relation in the same assessment. A tentative
/// parent cannot establish a child. Scores and confidence labels are not
/// acceptance evidence. Source locations are document-local, not stable
/// identities across revisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationAssessment {
    pub old_span: Option<TextSpan>,
    pub new_span: Option<TextSpan>,
    pub parent: Option<usize>,
    pub outcome: RelationOutcome,
    pub search: SearchCompleteness,
    pub assumptions: Vec<ComparisonAssumption>,
    pub reasons: Vec<AssessmentReason>,
}

/// A proposed localized edit that remains unresolved.
///
/// `relation` indexes the owning comparison's relation assessments. Candidates
/// in the same alternative group compete; their spans may overlap. Neither
/// their confidence labels nor their exact edit ranges resolve source tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeCandidate {
    pub change: ChangeEvent,
    pub relation: usize,
    pub alternative_group: usize,
}

/// Exclusive resolution state of an extracted comparable-token interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionState {
    Equal,
    Changed,
    Unresolved,
}

/// One block-local interval in a complete, non-overlapping side partition.
///
/// Synthetic separators between blocks never contribute to this interval's
/// token count. Canonical bounds preserve projection for unmapped tokens,
/// including intervals with a nonempty token range and zero scalar width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionRange {
    pub block: BlockId,
    pub comparable_range: TokenRange,
    pub canonical_range: ScalarRange,
    pub state: ResolutionState,
}

/// Consumption of the shared assessment budget, in execution-stage order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssessmentWork {
    pub anchor_verification: usize,
    pub local_views: usize,
    pub localization: usize,
    pub emission: usize,
}

impl AssessmentWork {
    fn total(self) -> Option<usize> {
        self.anchor_verification
            .checked_add(self.local_views)?
            .checked_add(self.localization)?
            .checked_add(self.emission)
    }
}

/// Accounting for attempts to complete a reused exact suffix table.
///
/// `allowance_used` is the conservative search allowance consumed by these
/// attempts. `recomputed_cells + binding_work + accounting_work` is their actual charged logical
/// work, excluding the exact cells already paid before retention. The difference
/// is `unused_allowance`; it cannot fund a later adaptive search. These are
/// logical operation units, not CPU instructions, and cover only suffix reuse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SuffixReuseWork {
    pub attempts: usize,
    pub allowance_used: usize,
    pub recomputed_cells: usize,
    pub binding_work: usize,
    /// Prepaid metadata operations for checked receipt construction and accumulation.
    pub accounting_work: usize,
    pub unused_allowance: usize,
}

impl SuffixReuseWork {
    pub(super) const ACCOUNTING_WORK: usize = 16;

    fn consistent(self) -> bool {
        self.recomputed_cells
            .checked_add(self.binding_work)
            .and_then(|actual| actual.checked_add(self.accounting_work))
            .and_then(|actual| actual.checked_add(self.unused_allowance))
            == Some(self.allowance_used)
            && self.attempts.checked_mul(3) == Some(self.binding_work)
            && self.attempts.checked_mul(Self::ACCOUNTING_WORK) == Some(self.accounting_work)
            && (self.attempts != 0 || self.allowance_used == 0)
    }

    fn record(&mut self, allowance: usize, actual: usize) -> Result<()> {
        let recomputed = actual
            .checked_sub(3 + Self::ACCOUNTING_WORK)
            .ok_or_else(|| invalid("invalid suffix binding work"))?;
        let unused = allowance
            .checked_sub(actual)
            .ok_or_else(|| invalid("suffix work exceeds allowance"))?;
        let add = |left: usize, right: usize| {
            left.checked_add(right)
                .ok_or_else(|| limit_error("suffix reuse work"))
        };
        let next = Self {
            attempts: add(self.attempts, 1)?,
            allowance_used: add(self.allowance_used, allowance)?,
            recomputed_cells: add(self.recomputed_cells, recomputed)?,
            binding_work: add(self.binding_work, 3)?,
            accounting_work: add(self.accounting_work, Self::ACCOUNTING_WORK)?,
            unused_allowance: add(self.unused_allowance, unused)?,
        };
        debug_assert!(next.consistent());
        *self = next;
        Ok(())
    }
}

/// Budget accounting for global anchors and constructor sidecar preparation.
///
/// Charges are logical proof work, not CPU instructions. A refused charge can
/// consume the remaining budget without executing its requested comparisons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnchorWork {
    /// Whole-block candidate indexing charges.
    pub candidate_index: usize,
    /// Flattened document and token-posting construction charges.
    pub occurrence_index: usize,
    /// Full sequence verification charges.
    pub occurrence_comparison: usize,
    /// Increasing spine uniqueness charges.
    pub order_uniqueness: usize,
    /// Candidate starting positions checked, including refused comparisons.
    pub starts_examined: usize,
    /// Anchor proof returned because its budget could not complete a phase.
    pub budget_exhausted: bool,
    /// Size of the first request refused inside the anchor proof.
    pub refused_request: usize,
    /// Remaining budget consumed by that refusal without doing its work.
    pub refused_remainder: usize,
    /// Completed order uniqueness result; absent when not reached or refused.
    pub order_unique: Option<bool>,
    /// Anchors shared by every maximum increasing spine. A nonzero count
    /// does not imply that the whole spine or any interval is unambiguous.
    pub verified_anchors: usize,
    /// Raw displacement sidecar index charges in constructor initialization.
    pub sidecar_index: usize,
    /// First refused sidecar index reservation, separate from the anchor proof.
    pub sidecar_refused_request: usize,
}

impl AnchorWork {
    fn total(self) -> Option<usize> {
        self.candidate_index
            .checked_add(self.occurrence_index)?
            .checked_add(self.occurrence_comparison)?
            .checked_add(self.order_uniqueness)?
            .checked_add(self.sidecar_index)
    }
}

/// Charges inside optional local correspondence discovery, not elapsed CPU work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalViewWork {
    /// Work available to optional discovery after the settlement reserve.
    pub budget_cap: usize,
    /// Shared work retained for mandatory localization and emission.
    pub settlement_reserve: usize,
    /// The optional cap reached zero; independent of the restored shared budget.
    pub cap_exhausted: bool,
    /// Exact refused request size, when observed; current view helpers do not
    /// expose this receipt, so absence does not establish that no request failed.
    pub refused_request_size: Option<usize>,
    /// Shared source issue projection index.
    pub source_issue_index: usize,
    /// Trusted run and native order view construction.
    pub build_views: usize,
    /// Automatically discovered exact anchor search.
    pub seed_search: usize,
    /// Verification of supplied exact recovery anchors.
    pub explicit_anchor_search: usize,
    /// Remaining anchor preparation and domain construction.
    pub domain_construction: usize,
    /// Catalog footer correspondence discovery.
    pub footer_search: usize,
}

impl LocalViewWork {
    fn total(self) -> usize {
        self.source_issue_index
            + self.build_views
            + self.seed_search
            + self.explicit_anchor_search
            + self.domain_construction
            + self.footer_search
    }
}

/// Evidence and source ownership produced by the shared comparison boundary.
///
/// The old/new partitions each cover all extracted comparable tokens exactly
/// once. Candidate locations remain unresolved. A containing changed-region
/// proof owns nothing; separately complete mandatory literal equalities may
/// resolve paired subruns inside its exact whole parent. The ambiguous edit
/// locations remain unresolved. These partitions do not establish extraction
/// completeness or visual equivalence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonAssessment {
    pub policy_version: u32,
    pub relations: Vec<RelationAssessment>,
    pub old_resolution: Vec<ResolutionRange>,
    pub new_resolution: Vec<ResolutionRange>,
    pub work_limit: usize,
    /// Consumed search allowance, including refused charges and unused suffix reservations.
    /// This is not a measurement of executed CPU instructions.
    pub work_used: usize,
    pub work_by_stage: AssessmentWork,
    /// Fine-grained accounting within the global anchor proof.
    pub anchor_work: AnchorWork,
    /// Fine-grained charges for optional local correspondence discovery.
    pub local_view_work: LocalViewWork,
    /// Actual suffix continuation fees and separately withheld search allowance.
    pub suffix_reuse_work: SuffixReuseWork,
    /// Additional candidate descriptions were omitted within output limits.
    pub candidates_truncated: bool,
    /// Additional programmatic evidence for emitted local changes. Standard
    /// reports summarize the corresponding relations rather than these edits.
    pub localized_edits: Vec<LocalizedEditScript>,
    /// Non-owning quantitative claims under each established domain's premises.
    pub review_units: Vec<ReviewUnit>,
}

/// The objective whose complete solution set a claim quantifies over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignmentPolicy {
    /// Minimize insertions plus deletions of comparable tokens.
    LiteralMinimal,
}

/// Inclusive limits on changed source tokens across all permitted alignments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditCountBounds {
    pub lower: usize,
    pub upper: usize,
}

/// A source-backed comparison context that never owns or recolors tokens.
///
/// The indexed relation supplies correspondence, normalization, and source
/// completeness premises. Counts exclude synthetic separators. Mandatory
/// ranges are facts under those same premises, not additional change events.
/// `unresolved_changed_count` queries the final unresolved partition directly;
/// it does not reuse a whole-domain proof after subtracting unrelated children.
/// Every present count is a completed query. `search` is complete only when
/// all requested count and mandatory-position queries finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewUnit {
    pub relation: usize,
    pub policy: AlignmentPolicy,
    pub search: SearchCompleteness,
    /// Number of old/new normalization interpretation pairs quantified over.
    pub normalization_hypotheses: usize,
    /// Source tokens with retain/skip alternatives; all combinations are kept.
    pub normalization_old: Vec<TextSpan>,
    pub normalization_new: Vec<TextSpan>,
    pub changed_count: Option<EditCountBounds>,
    pub unresolved_changed_count: Option<EditCountBounds>,
    pub mandatory_old: Vec<TextSpan>,
    pub mandatory_new: Vec<TextSpan>,
}

/// One optimal edit witness for a completed local comparison.
///
/// When several optimal paths exist, their emitted semantic ranges agree;
/// this witness does not identify an author's historical editing sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalizedEditScript {
    /// Established relation whose old/new spans bound this comparison.
    pub relation: usize,
    /// Contiguous indices in [`Comparison::changes`] emitted from this script.
    pub changes: Range<usize>,
    /// Coordinates start at zero within the relation's old/new spans.
    pub edits: Vec<super::AtomicEdit>,
}

impl ComparisonAssessment {
    /// Checks the structural evidence and exclusive token accounting.
    ///
    /// Caller-supplied assessments remain assertions about source evidence.
    /// This validation does not independently extract the original PDF.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] for invalid relation references,
    /// contradictory outcomes, overlapping partitions, or coverage mismatch.
    pub fn validate(&self, comparison: &Comparison) -> Result<()> {
        if self.policy_version != ASSESSMENT_POLICY_VERSION
            || self.work_used > self.work_limit
            || self.work_by_stage.total() != Some(self.work_used)
            || !self.suffix_reuse_work.consistent()
            || self.suffix_reuse_work.allowance_used > self.work_used
            || self
                .anchor_work
                .total()
                .is_none_or(|total| total > self.work_by_stage.anchor_verification)
        {
            return Err(invalid(
                "invalid assessment policy version or work accounting",
            ));
        }
        for (index, relation) in self.relations.iter().enumerate() {
            if relation.old_span.is_none()
                && relation.new_span.is_none()
                && !validation::is_output_limit_sentinel(relation)
            {
                return Err(invalid("assessment relations require a source range"));
            }
            if let Some(parent) = relation.parent {
                if parent >= index {
                    return Err(invalid("assessment parents must precede their children"));
                }
                if relation.outcome == RelationOutcome::Established
                    && self.relations[parent].outcome != RelationOutcome::Established
                {
                    return Err(invalid(
                        "a tentative parent cannot establish a child relation",
                    ));
                }
            }
            match relation.outcome {
                RelationOutcome::Established
                    if !relation.reasons.is_empty()
                        || relation.search != SearchCompleteness::Complete =>
                {
                    return Err(invalid(
                        "established relations require complete, uncontradicted evidence",
                    ));
                }
                RelationOutcome::Tentative if relation.reasons.is_empty() => {
                    return Err(invalid("tentative relations require an uncertainty reason"));
                }
                _ => {}
            }
        }
        for unit in &self.review_units {
            let relation = self
                .relations
                .get(unit.relation)
                .ok_or_else(|| invalid("review unit refers to a missing relation"))?;
            if relation.outcome != RelationOutcome::Established {
                return Err(invalid("review claims require an established domain"));
            }
            if unit.normalization_hypotheses == 0 {
                return Err(invalid("review claims require a nonempty hypothesis set"));
            }
            if unit.search == SearchCompleteness::Complete
                && (unit.changed_count.is_none() || unit.unresolved_changed_count.is_none())
            {
                return Err(invalid("complete review claims require both count queries"));
            }
            for bounds in [unit.changed_count, unit.unresolved_changed_count]
                .into_iter()
                .flatten()
            {
                if bounds.lower > bounds.upper {
                    return Err(invalid("review count bounds are reversed"));
                }
            }
            if let (Some(total), Some(residual)) =
                (unit.changed_count, unit.unresolved_changed_count)
                && residual.upper > total.upper
            {
                return Err(invalid("residual count exceeds the whole-domain count"));
            }
        }
        for candidate in &comparison.change_candidates {
            let relation = self
                .relations
                .get(candidate.relation)
                .ok_or_else(|| invalid("candidate refers to a missing relation"))?;
            if relation.outcome != RelationOutcome::Tentative {
                return Err(invalid("candidate relation must remain tentative"));
            }
            if candidate.alternative_group >= self.relations.len() {
                return Err(invalid("candidate alternative group is out of bounds"));
            }
        }
        let mut previous_change_end = 0;
        for trace in &self.localized_edits {
            let relation = self
                .relations
                .get(trace.relation)
                .ok_or_else(|| invalid("localized edits refer to a missing relation"))?;
            let (Some(old), Some(new)) = (&relation.old_span, &relation.new_span) else {
                return Err(invalid("localized edit contexts require both source sides"));
            };
            if relation.outcome != RelationOutcome::Established
                || trace.changes.start < previous_change_end
                || trace.changes.start >= trace.changes.end
                || trace.changes.end > comparison.changes.len()
                || trace.edits.is_empty()
            {
                return Err(invalid(
                    "localized edits require distinct emitted changes and an established context",
                ));
            }
            let lengths = [
                old.comparable_range
                    .end
                    .checked_sub(old.comparable_range.start),
                new.comparable_range
                    .end
                    .checked_sub(new.comparable_range.start),
            ];
            let [Some(old_len), Some(new_len)] = lengths else {
                return Err(invalid("localized edit context is reversed"));
            };
            let mut cursor = [0, 0];
            for edit in &trace.edits {
                if edit.old.start > edit.old.end
                    || edit.new.start > edit.new.end
                    || edit.old.end > old_len
                    || edit.new.end > new_len
                    || edit.old.start < cursor[0]
                    || edit.new.start < cursor[1]
                    || edit.old.is_empty() == edit.new.is_empty()
                    || edit.old.start - cursor[0] != edit.new.start - cursor[1]
                {
                    return Err(invalid("localized edit coordinates are inconsistent"));
                }
                cursor = [edit.old.end, edit.new.end];
            }
            if old_len - cursor[0] != new_len - cursor[1]
                || comparison.changes[trace.changes.clone()]
                    .iter()
                    .any(|change| change.kind == ChangeKind::Move)
            {
                return Err(invalid(
                    "localized edits cannot establish movement or unequal trailing ranges",
                ));
            }
            for change in &comparison.changes[trace.changes.clone()] {
                for occurrence in &change.occurrences {
                    for (span, context) in [
                        (occurrence.old_span.as_ref(), old),
                        (occurrence.new_span.as_ref(), new),
                    ] {
                        if let Some(span) = span
                            && (span.blocks != context.blocks
                                || span.separator != context.separator
                                || span.comparable_range.start < context.comparable_range.start
                                || span.comparable_range.end > context.comparable_range.end
                                || span.canonical_range.start < context.canonical_range.start
                                || span.canonical_range.end > context.canonical_range.end)
                        {
                            return Err(invalid(
                                "localized change is outside its comparison context",
                            ));
                        }
                    }
                }
            }
            previous_change_end = trace.changes.end;
        }
        validate_partition(&self.old_resolution, comparison.old_coverage)?;
        validate_partition(&self.new_resolution, comparison.new_coverage)
    }
}

fn validate_partition(ranges: &[ResolutionRange], coverage: Coverage) -> Result<()> {
    let mut previous: Option<&ResolutionRange> = None;
    let mut seen_blocks = HashSet::new();
    let mut total = 0usize;
    let mut resolved = 0usize;
    for range in ranges {
        if range.comparable_range.start >= range.comparable_range.end
            || range.canonical_range.start > range.canonical_range.end
            || range.canonical_range.end - range.canonical_range.start
                > range.comparable_range.end - range.comparable_range.start
        {
            return Err(invalid("invalid assessment resolution interval"));
        }
        match previous {
            Some(previous) if previous.block == range.block => {
                if previous.comparable_range.end != range.comparable_range.start
                    || previous.canonical_range.end != range.canonical_range.start
                {
                    return Err(invalid("assessment partition has an overlap or gap"));
                }
            }
            _ => {
                if range.comparable_range.start != 0
                    || range.canonical_range.start != 0
                    || !seen_blocks.insert(range.block)
                {
                    return Err(invalid("assessment block partition is not contiguous"));
                }
            }
        }
        let count = range.comparable_range.end - range.comparable_range.start;
        total = total
            .checked_add(count)
            .ok_or_else(|| invalid("assessment token count overflow"))?;
        if range.state != ResolutionState::Unresolved {
            resolved = resolved
                .checked_add(count)
                .ok_or_else(|| invalid("assessment resolved count overflow"))?;
        }
        previous = Some(range);
    }
    if total != coverage.total_tokens || resolved != coverage.resolved_tokens {
        return Err(invalid(
            "assessment ownership does not match reported coverage",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceInterval {
    block_index: usize,
    start: usize,
    end: usize,
}

/// Projects a grouped span without assigning synthetic separator tokens to a
/// source block. Both scalar and comparable coordinates must describe the same
/// interval, including zero-width scalar intervals containing unmapped tokens.
fn project(side: &Side<'_>, span: &TextSpan) -> Result<Vec<SourceInterval>> {
    if span
        .separator
        .is_some_and(|separator| !separator.valid_for(span.blocks.len()))
    {
        return Err(invalid(
            "assessment separator does not match its source blocks",
        ));
    }
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(span.blocks.len())
        .map_err(|_| allocation_error("source projection"))?;
    let mut token_offset = 0usize;
    let mut scalar_offset = 0usize;
    let mut projected_start = None;
    let mut projected_end = None;
    let mut preceding_space = false;
    let mut seen = HashSet::new();
    for (position, block) in span.blocks.iter().enumerate() {
        let &block_index = side
            .index
            .get(block)
            .ok_or_else(|| invalid("assessment refers to an unknown block"))?;
        if !seen.insert(block_index) {
            return Err(invalid("assessment span repeats a source block"));
        }
        let tokens = &side.canonical[block_index];
        let separator = position > 0
            && span.separator.map(|separator| separator.at(position - 1))
                == Some(BlockSeparator::Space)
            && !preceding_space
            && !tokens.first().is_some_and(space_token);
        if separator {
            record_boundary(
                span,
                token_offset,
                scalar_offset,
                &mut projected_start,
                &mut projected_end,
            );
            token_offset += 1;
            scalar_offset += 1;
        }
        let block_start = token_offset;
        let block_end = block_start + tokens.len();
        for boundary in [span.comparable_range.start, span.comparable_range.end] {
            if boundary >= block_start && boundary <= block_end {
                let scalar = scalar_offset
                    + block_scalar_boundary(
                        &side.blocks[block_index].canonical,
                        boundary - block_start,
                    );
                record_boundary(
                    span,
                    boundary,
                    scalar,
                    &mut projected_start,
                    &mut projected_end,
                );
            }
        }
        token_offset = block_end;
        scalar_offset += tokens.len() - side.blocks[block_index].canonical.unmapped.len();
        let start = span.comparable_range.start.max(block_start);
        let end = span.comparable_range.end.min(token_offset);
        if start < end {
            ranges.push(SourceInterval {
                block_index,
                start: start - block_start,
                end: end - block_start,
            });
        }
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, space_token);
    }
    record_boundary(
        span,
        token_offset,
        scalar_offset,
        &mut projected_start,
        &mut projected_end,
    );
    if span.comparable_range.start > span.comparable_range.end
        || projected_start != Some(span.canonical_range.start)
        || projected_end != Some(span.canonical_range.end)
        || (span.blocks.len() > 1 && span.separator.is_none())
        || (span.blocks.len() <= 1 && span.separator.is_some())
    {
        return Err(invalid("assessment source coordinates do not agree"));
    }
    Ok(ranges)
}

fn block_scalar_boundary(text: &crate::normalize::MappedText, token_boundary: usize) -> usize {
    let mut start = 0;
    let mut end = text.unmapped.len();
    while start < end {
        let middle = start + (end - start) / 2;
        // Materialization validates sorted scalar positions. Each preceding
        // unmapped token adds one comparable position but no scalar width.
        if text.unmapped[middle].scalar_index + middle < token_boundary {
            start = middle + 1;
        } else {
            end = middle;
        }
    }
    token_boundary - start
}

/// One discovery pass's cache of checked normalization issue ranges.
///
/// The cache borrows the two sides it belongs to and indexes every entry by
/// that side's own block index, so an entry can never be reused by another
/// side, another comparison or another block. Each entry stores the exact
/// result of `BlockText::checked_normalization_issue_ranges`: the complete
/// validated ranges, or the fact that the validation failed. A failed
/// validation keeps its issue veto and is never treated as an empty issue
/// list.
struct SourceIssueCache<'a> {
    sides: [SourceIssueCacheSide<'a>; 2],
}

/// One side's entry table. The table borrows its side, so the entries cannot
/// outlive the side and cannot be used for another comparison.
struct SourceIssueCacheSide<'a> {
    side: &'a Side<'a>,
    entries: Vec<Option<CachedIssueRanges>>,
    /// Diagnostic-only hit count for the unit tests; release builds do not
    /// carry it.
    #[cfg(test)]
    hits: std::cell::Cell<usize>,
}

impl SourceIssueCacheSide<'_> {
    /// Diagnostic-only hit counter for the unit tests.
    #[cfg(test)]
    fn note_hit(&self) {
        self.hits.set(self.hits.get().saturating_add(1));
    }
}

/// The exact result of one checked issue-range validation.
enum CachedIssueRanges {
    /// The complete validated issue ranges of the block.
    Ranges(Vec<ScalarRange>),
    /// The validation failed; the block keeps its issue veto.
    Invalid,
}

impl<'a> SourceIssueCache<'a> {
    /// Reserves one entry per block on each side for one discovery pass.
    ///
    /// Returns `Ok(None)` when the shared work budget cannot cover the bounded
    /// per-block reservation; the caller then returns an empty discovery and
    /// confirms no new domain. Nothing is stored before the reservation is
    /// paid, so an exhausted budget never becomes a cached result.
    ///
    /// # Errors
    ///
    /// Returns the assessment allocation error when an entry table cannot be
    /// reserved.
    fn new(sides: [&'a Side<'a>; 2], remaining: &mut usize) -> Result<Option<Self>> {
        let mut cache = Self {
            sides: [
                SourceIssueCacheSide {
                    side: sides[0],
                    entries: Vec::new(),
                    #[cfg(test)]
                    hits: std::cell::Cell::new(0),
                },
                SourceIssueCacheSide {
                    side: sides[1],
                    entries: Vec::new(),
                    #[cfg(test)]
                    hits: std::cell::Cell::new(0),
                },
            ],
        };
        for (index, side) in sides.into_iter().enumerate() {
            if !charge(remaining, side.blocks.len()) {
                return Ok(None);
            }
            let entries = &mut cache.sides[index].entries;
            entries
                .try_reserve_exact(side.blocks.len())
                .map_err(|_| allocation_error("source issue cache entries"))?;
            entries.resize_with(side.blocks.len(), || None);
        }
        Ok(Some(cache))
    }

    /// The entry table of one side.
    fn side(&mut self, index: usize) -> &mut SourceIssueCacheSide<'a> {
        &mut self.sides[index]
    }
}

/// Cached variant of [`span_has_source_issues`] for one discovery pass.
///
/// The cache carries its own side, so the veto is always evaluated against
/// that side and the entries can never be reused for another side or another
/// comparison.
fn span_has_source_issues_cached(
    cache: &mut SourceIssueCacheSide<'_>,
    span: &TextSpan,
    remaining: &mut usize,
) -> Result<bool> {
    let side = cache.side;
    span_has_source_issues_inner(side, span, remaining, Some(cache))
}

fn span_has_source_issues(side: &Side<'_>, span: &TextSpan, remaining: &mut usize) -> Result<bool> {
    span_has_source_issues_inner(side, span, remaining, None)
}

/// Shared source-issue veto with an optional per-block cache.
///
/// The uncached call keeps the original contract: the full pre-validation
/// charge is paid before every checked validation. A cached call pays one
/// bounded lookup, pays the full pre-validation charge only on the first
/// validation of a block, and pays a bounded overlap scan on a hit. Every
/// budget failure reports "has issues" and stores nothing, so an exhausted
/// budget never becomes a cached valid or invalid result.
fn span_has_source_issues_inner(
    side: &Side<'_>,
    span: &TextSpan,
    remaining: &mut usize,
    mut cache: Option<&mut SourceIssueCacheSide>,
) -> Result<bool> {
    for interval in project(side, span)? {
        let block = &side.blocks[interval.block_index];
        if block.issues.is_empty() {
            continue;
        }
        if let Some(cache) = cache.as_deref() {
            match cache.entries[interval.block_index].as_ref() {
                Some(CachedIssueRanges::Invalid) => {
                    #[cfg(test)]
                    cache.note_hit();
                    // The validation already failed; the bounded lookup is
                    // charged and the veto stands either way.
                    let _charged = charge(remaining, 1);
                    return Ok(true);
                }
                Some(CachedIssueRanges::Ranges(ranges)) => {
                    #[cfg(test)]
                    cache.note_hit();
                    // A hit pays the lookup, the range scan and the two
                    // binary searches over the canonical unmapped table.
                    if !charge(remaining, overlap_scan_cost(ranges, &block.canonical)) {
                        return Ok(true);
                    }
                    if issue_ranges_overlap(ranges, &block.canonical, &interval) {
                        return Ok(true);
                    }
                    continue;
                }
                None => {
                    // A miss pays the bounded lookup before validating.
                    if !charge(remaining, 1) {
                        return Ok(true);
                    }
                }
            }
        }
        let source_items = block
            .raw
            .text
            .len()
            .saturating_add(block.canonical.text.len())
            .saturating_add(block.raw.source_map.len())
            .saturating_add(block.canonical.source_map.len())
            .saturating_add(block.normalization_events.len())
            .saturating_add(block.canonical.unmapped.len());
        if !charge(
            remaining,
            source_items.saturating_mul(block.issues.len().saturating_add(1)),
        ) {
            return Ok(true);
        }
        let Ok(ranges) = block.checked_normalization_issue_ranges() else {
            if let Some(cache) = cache.as_deref_mut()
                && charge(remaining, 1)
            {
                cache.entries[interval.block_index] = Some(CachedIssueRanges::Invalid);
            }
            return Ok(true);
        };
        let has_issues = issue_ranges_overlap(&ranges, &block.canonical, &interval);
        if let Some(cache) = cache.as_deref_mut() {
            // Insertion and retained memory are charged; a failed charge
            // stores nothing and holds the veto.
            if !charge(remaining, ranges.len().saturating_add(1)) {
                return Ok(true);
            }
            cache.entries[interval.block_index] = Some(CachedIssueRanges::Ranges(ranges));
        }
        if has_issues {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Bounded cost of one cached overlap scan: the lookup, the range scan and
/// the two binary searches over the canonical unmapped-token table.
///
/// One `block_scalar_boundary` search runs at most `floor(log2(n)) + 1`
/// iterations over `n` unmapped tokens (zero iterations for `n == 0`), so the
/// charge follows the real loop bound instead of a lower approximation.
fn overlap_scan_cost(ranges: &[ScalarRange], canonical: &crate::normalize::MappedText) -> usize {
    let unmapped = canonical.unmapped.len();
    let searches = if unmapped == 0 {
        0
    } else {
        unmapped.ilog2() as usize + 1
    };
    ranges
        .len()
        .saturating_add(1)
        .saturating_add(searches.saturating_mul(2))
}

/// Exact original overlap test between one block's issue ranges and one
/// projected interval, including the zero-length range and empty canonical
/// interval rules.
fn issue_ranges_overlap(
    ranges: &[ScalarRange],
    canonical: &crate::normalize::MappedText,
    interval: &SourceInterval,
) -> bool {
    let canonical_start = block_scalar_boundary(canonical, interval.start);
    let canonical_end = block_scalar_boundary(canonical, interval.end);
    ranges.iter().any(|issue| {
        let start = if issue.start == issue.end {
            issue.start.saturating_sub(1)
        } else {
            issue.start
        };
        let end = if issue.start == issue.end {
            issue.end.saturating_add(1)
        } else {
            issue.end
        };
        if canonical_start == canonical_end {
            start <= canonical_start && canonical_start <= end
        } else {
            start < canonical_end && canonical_start < end
        }
    })
}

fn record_boundary(
    span: &TextSpan,
    token: usize,
    scalar: usize,
    start: &mut Option<usize>,
    end: &mut Option<usize>,
) {
    if span.comparable_range.start == token {
        *start = Some(scalar);
    }
    if span.comparable_range.end == token {
        *end = Some(scalar);
    }
}

fn space_token(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(' '))
}

/// Accepted contexts and their changed subranges are collected separately.
/// Subtracting final unresolved intervals is never used to infer equality.
struct Ownership {
    accepted: Vec<SourceInterval>,
    changed: Vec<SourceInterval>,
}

impl Ownership {
    fn new() -> Self {
        Self {
            accepted: Vec::new(),
            changed: Vec::new(),
        }
    }

    fn accept(&mut self, side: &Side<'_>, span: &TextSpan, range_limit: usize) -> Result<()> {
        let projected = project(side, span)?;
        reserve_ranges(&mut self.accepted, projected.len(), range_limit)?;
        self.accepted.extend(projected);
        Ok(())
    }

    fn change(&mut self, side: &Side<'_>, span: &TextSpan, range_limit: usize) -> Result<()> {
        let projected = project(side, span)?;
        reserve_ranges(&mut self.changed, projected.len(), range_limit)?;
        self.changed.extend(projected);
        Ok(())
    }

    fn exclude(&mut self, side: &Side<'_>, span: &TextSpan, range_limit: usize) -> Result<()> {
        for excluded in project(side, span)? {
            let mut output = Vec::new();
            for interval in &self.accepted {
                if interval.block_index != excluded.block_index
                    || interval.end <= excluded.start
                    || excluded.end <= interval.start
                {
                    reserve_ranges(&mut output, 1, range_limit)?;
                    output.push(*interval);
                } else {
                    if interval.start < excluded.start {
                        reserve_ranges(&mut output, 1, range_limit)?;
                        output.push(SourceInterval {
                            end: excluded.start,
                            ..*interval
                        });
                    }
                    if excluded.end < interval.end {
                        reserve_ranges(&mut output, 1, range_limit)?;
                        output.push(SourceInterval {
                            start: excluded.end,
                            ..*interval
                        });
                    }
                }
            }
            self.accepted = output;
        }
        Ok(())
    }

    #[cfg(test)]
    fn finish(self, side: &Side<'_>, range_limit: usize) -> Result<Vec<ResolutionRange>> {
        self.resolution(side, range_limit)
    }

    /// Computes the resolution partition without consuming the ownership.
    ///
    /// The deferred equal-fragment tail pass compares the partition before
    /// and after its commit, so the computation is a read-only snapshot. Only
    /// the two interval lists are cloned; the partition is rebuilt from them.
    fn resolution(&self, side: &Side<'_>, range_limit: usize) -> Result<Vec<ResolutionRange>> {
        let mut accepted = self.accepted.clone();
        let mut changed = self.changed.clone();
        merge_intervals(&mut accepted);
        merge_intervals(&mut changed);
        let mut accepted_by_block = HashMap::<usize, Vec<SourceInterval>>::new();
        let mut changed_by_block = HashMap::<usize, Vec<SourceInterval>>::new();
        for interval in accepted {
            accepted_by_block
                .entry(interval.block_index)
                .or_default()
                .push(interval);
        }
        for interval in changed {
            changed_by_block
                .entry(interval.block_index)
                .or_default()
                .push(interval);
        }
        let mut result = Vec::new();
        for (block_index, tokens) in side.canonical.iter().enumerate() {
            if tokens.is_empty() {
                continue;
            }
            let accepted = accepted_by_block
                .get(&block_index)
                .map_or(&[][..], Vec::as_slice);
            let changed = changed_by_block
                .get(&block_index)
                .map_or(&[][..], Vec::as_slice);
            let mut boundaries = vec![0, tokens.len()];
            for interval in accepted.iter().chain(changed) {
                boundaries.push(interval.start);
                boundaries.push(interval.end);
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            let mut scalar_start = 0usize;
            let mut accepted_cursor = 0usize;
            let mut changed_cursor = 0usize;
            for pair in boundaries.windows(2) {
                let [start, end] = [pair[0], pair[1]];
                let established = interval_contains(accepted, &mut accepted_cursor, start, end);
                let is_changed = interval_contains(changed, &mut changed_cursor, start, end);
                if is_changed && !established {
                    return Err(invalid(
                        "changed source tokens have no accepted correspondence",
                    ));
                }
                let state = if is_changed {
                    ResolutionState::Changed
                } else if established {
                    ResolutionState::Equal
                } else {
                    ResolutionState::Unresolved
                };
                let scalar_end = scalar_start
                    + tokens[start..end]
                        .iter()
                        .filter(|token| matches!(token, ComparableToken::Scalar(_)))
                        .count();
                let block = side.blocks[block_index].block;
                if let Some(previous) =
                    result.last_mut().filter(|previous: &&mut ResolutionRange| {
                        previous.block == block && previous.state == state
                    })
                {
                    previous.comparable_range.end = end;
                    previous.canonical_range.end = scalar_end;
                } else {
                    reserve_ranges(&mut result, 1, range_limit)?;
                    result.push(ResolutionRange {
                        block,
                        comparable_range: TokenRange { start, end },
                        canonical_range: ScalarRange {
                            start: scalar_start,
                            end: scalar_end,
                        },
                        state,
                    });
                }
                scalar_start = scalar_end;
            }
        }
        Ok(result)
    }
}

fn interval_contains(
    intervals: &[SourceInterval],
    cursor: &mut usize,
    start: usize,
    end: usize,
) -> bool {
    while intervals
        .get(*cursor)
        .is_some_and(|interval| interval.end <= start)
    {
        *cursor += 1;
    }
    intervals
        .get(*cursor)
        .is_some_and(|interval| interval.start <= start && interval.end >= end)
}

fn merge_intervals(intervals: &mut Vec<SourceInterval>) {
    intervals.sort_unstable_by_key(|interval| (interval.block_index, interval.start, interval.end));
    let mut retained = 0usize;
    for index in 0..intervals.len() {
        let interval = intervals[index];
        if retained > 0
            && intervals[retained - 1].block_index == interval.block_index
            && intervals[retained - 1].end >= interval.start
        {
            intervals[retained - 1].end = intervals[retained - 1].end.max(interval.end);
        } else {
            intervals[retained] = interval;
            retained += 1;
        }
    }
    intervals.truncate(retained);
}

fn reserve_ranges<T>(output: &mut Vec<T>, additional: usize, limit: usize) -> Result<()> {
    if output
        .len()
        .checked_add(additional)
        .is_none_or(|count| count > limit)
    {
        return Err(Error::LimitExceeded {
            resource: "assessment output ranges",
            limit,
        });
    }
    output
        .try_reserve_exact(additional)
        .map_err(|_| allocation_error("output ranges"))
}

/// Output of proposal discovery, before any relation becomes public evidence.
pub(super) struct ProposedComparison {
    pub changes: Vec<ChangeEvent>,
    pub proven_changed_regions: Vec<ProvenChangedRegion>,
    pub formatting_changes: Vec<FormattingChange>,
    pub unresolved_regions: Vec<UnresolvedRegion>,
}

struct ProposedRelation {
    old: Option<TextSpan>,
    new: Option<TextSpan>,
    span_indices: [Option<usize>; 2],
    exact_recovery: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProposalKey {
    old: Option<TextSpan>,
    new: Option<TextSpan>,
    span_indices: [Option<usize>; 2],
}

impl From<&ProposedRelation> for ProposalKey {
    fn from(proposal: &ProposedRelation) -> Self {
        Self {
            old: proposal.old.clone(),
            new: proposal.new.clone(),
            span_indices: proposal.span_indices,
        }
    }
}

fn full_relation_span(
    side: &Side<'_>,
    blocks: &[BlockId],
    separator: Option<BlockSeparator>,
) -> Option<TextSpan> {
    (!blocks.is_empty())
        .then(|| super::try_group_full_span(side, blocks, separator))
        .flatten()
}

fn recovered_span(recovered: &sentence::RecoveredSentence) -> TextSpan {
    TextSpan {
        blocks: recovered.blocks.clone(),
        separator: recovered.separator,
        canonical_range: recovered.canonical,
        comparable_range: recovered.comparable,
    }
}

/// Returns the side that carries no source tokens and no incompleteness
/// evidence, if any.
///
/// A side is proven empty when it has zero canonical tokens, no block issues
/// and no extraction-gap evidence anywhere in the alignment: the extraction
/// completed and produced no native text. A side with unmapped tokens, block
/// issues or any extraction gap is never treated as empty, and the gap check
/// is deliberately global because an empty side carries no blocks that could
/// attribute a gap to it.
fn proven_empty_side(sides: [&Side<'_>; 2], alignment: &Alignment) -> Option<usize> {
    (0..2).find(|&side| {
        sides[side].total_tokens == 0
            && sides[side]
                .blocks
                .iter()
                .all(|block| block.issues.is_empty())
            && !alignment
                .spans
                .iter()
                .any(|span| span.evidence.contains(&AlignmentEvidence::ExtractionGap))
    })
}

fn collect_relations(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: Option<&sentence::SentenceRecoveryPlan>,
    proposed: &ProposedComparison,
    limit: usize,
) -> Result<(Vec<ProposedRelation>, bool)> {
    let [old, new] = sides;
    let mut relations = Vec::new();
    for (index, span) in alignment.spans.iter().enumerate() {
        if span.kind == AlignmentKind::Unresolved {
            continue;
        }
        let old_span = full_relation_span(old, &span.old, span.old_separator);
        let new_span = full_relation_span(new, &span.new, span.new_separator);
        if (old_span.is_none() || new_span.is_none())
            && proposed.changes.iter().any(|change| {
                change.kind == ChangeKind::Move
                    && change.occurrences.iter().any(|occurrence| {
                        (old_span.is_some() && occurrence.old_span == old_span)
                            || (new_span.is_some() && occurrence.new_span == new_span)
                    })
            })
        {
            continue;
        }
        if proposed
            .unresolved_regions
            .iter()
            .any(|region| region.old_span == old_span && region.new_span == new_span)
        {
            continue;
        }
        if relations.len() == limit {
            return Ok((relations, true));
        }
        reserve_ranges(&mut relations, 1, limit)?;
        relations.push(ProposedRelation {
            old: old_span,
            new: new_span,
            span_indices: [
                (!span.old.is_empty()).then_some(index),
                (!span.new.is_empty()).then_some(index),
            ],
            exact_recovery: span.evidence.contains(&AlignmentEvidence::ExactCanonical),
        });
    }
    if let Some(plan) = recovery {
        for matched in &plan.matches {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(
                &mut relations,
                Some(&matched.old),
                Some(&matched.new),
                true,
                limit,
            )?;
        }
        for replacement in &plan.replacements {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(
                &mut relations,
                Some(&replacement.old),
                Some(&replacement.new),
                false,
                limit,
            )?;
        }
        for replacement in &plan.anchored_replacements {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(
                &mut relations,
                Some(&replacement.old),
                Some(&replacement.new),
                false,
                limit,
            )?;
        }
        for deletion in &plan.deletions {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(&mut relations, Some(deletion), None, false, limit)?;
        }
        for insertion in &plan.insertions {
            if relations.len() == limit {
                return Ok((relations, true));
            }
            push_recovered_relation(&mut relations, None, Some(insertion), false, limit)?;
        }
        // Cross-span recovery stores each side independently. Re-establish the
        // pair from exact source tokens instead of treating positional zip as
        // correspondence evidence.
        let mut old_keys =
            HashMap::<Vec<ComparableToken>, Vec<&sentence::RecoveredSentence>>::new();
        let mut new_keys =
            HashMap::<Vec<ComparableToken>, Vec<&sentence::RecoveredSentence>>::new();
        for (side, recovered, keys) in [
            (old, &plan.cross_span_match_old, &mut old_keys),
            (new, &plan.cross_span_match_new, &mut new_keys),
        ] {
            for part in recovered {
                let span = recovered_span(part);
                let tokens = span_tokens(side, &span)?;
                keys.entry(tokens).or_default().push(part);
            }
        }
        // Iterate source order; hash-map iteration must not affect group IDs.
        for part in &plan.cross_span_match_old {
            let key = span_tokens(old, &recovered_span(part))?;
            if let (Some(old_parts), Some(new_parts)) = (old_keys.get(&key), new_keys.get(&key))
                && old_parts.len() == 1
                && new_parts.len() == 1
            {
                if relations.len() == limit {
                    return Ok((relations, true));
                }
                push_recovered_relation(
                    &mut relations,
                    Some(part),
                    Some(new_parts[0]),
                    true,
                    limit,
                )?;
            }
        }
    }
    let mut seen = relations
        .iter()
        .map(ProposalKey::from)
        .collect::<HashSet<_>>();
    for change in &proposed.changes {
        if change.kind == ChangeKind::Move {
            continue;
        }
        for occurrence in &change.occurrences {
            let proposal = ProposedRelation {
                old: occurrence.old_span.clone(),
                new: occurrence.new_span.clone(),
                span_indices: occurrence_indices(
                    alignment,
                    [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()],
                ),
                exact_recovery: false,
            };
            if !seen.insert(ProposalKey::from(&proposal)) {
                continue;
            }
            if relations.len() == limit {
                return Ok((relations, true));
            }
            reserve_ranges(&mut relations, 1, limit)?;
            relations.push(proposal);
        }
    }
    Ok((relations, false))
}

fn push_recovered_relation(
    output: &mut Vec<ProposedRelation>,
    old: Option<&sentence::RecoveredSentence>,
    new: Option<&sentence::RecoveredSentence>,
    exact: bool,
    limit: usize,
) -> Result<()> {
    reserve_ranges(output, 1, limit)?;
    output.push(ProposedRelation {
        old: old.map(recovered_span),
        new: new.map(recovered_span),
        span_indices: [
            old.map(|part| part.span_index),
            new.map(|part| part.span_index),
        ],
        exact_recovery: exact,
    });
    Ok(())
}

fn span_tokens(side: &Side<'_>, span: &TextSpan) -> Result<Vec<ComparableToken>> {
    project(side, span)?;
    let group = side.canonical_group(&span.blocks, span.separator);
    let tokens = group
        .tokens
        .get(span.comparable_range.start..span.comparable_range.end)
        .ok_or_else(|| invalid("assessment span exceeds its source tokens"))?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(tokens.len())
        .map_err(|_| allocation_error("span tokens"))?;
    copy.extend_from_slice(tokens);
    Ok(copy)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DomainKey {
    local: Option<(TextSpan, TextSpan)>,
    old: Range<usize>,
    new: Range<usize>,
    old_separator: BlockSeparator,
    new_separator: BlockSeparator,
}

/// Exact key lengths and broader refusal diagnostics must never be confused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProofScope {
    ExactKey,
    BroadRootRefusal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DomainState {
    Ready,
    Stopped(usize),
}

struct DomainProof {
    scope: ProofScope,
    relation: usize,
    unique: bool,
    search: SearchCompleteness,
    edits: Vec<super::AtomicEdit>,
    lengths: [usize; 2],
    strict_unique: bool,
    stable_events: Option<Vec<ProjectedEvent>>,
}

impl DomainProof {
    fn exact_lengths(&self) -> Option<[usize; 2]> {
        (self.scope == ProofScope::ExactKey).then_some(self.lengths)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedEvent {
    kind: ChangeKind,
    occurrences: Vec<ProjectedOccurrence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedOccurrence {
    old: Option<Vec<SourceInterval>>,
    new: Option<Vec<SourceInterval>>,
}

fn projected_event(sides: [&Side<'_>; 2], event: &ChangeEvent) -> Result<ProjectedEvent> {
    Ok(ProjectedEvent {
        kind: event.kind,
        occurrences: event
            .occurrences
            .iter()
            .map(|occurrence| {
                Ok(ProjectedOccurrence {
                    old: occurrence
                        .old_span
                        .as_ref()
                        .map(|span| project(sides[0], span))
                        .transpose()?,
                    new: occurrence
                        .new_span
                        .as_ref()
                        .map(|span| project(sides[1], span))
                        .transpose()?,
                })
            })
            .collect::<Result<_>>()?,
    })
}

fn charge_work(remaining: &mut usize, count: usize) -> bool {
    if let Some(next) = remaining.checked_sub(count) {
        *remaining = next;
        true
    } else {
        *remaining = 0;
        false
    }
}

/// Counts requested capacities of the review-owned cache that remains live
/// alongside the optional views and DP. Length metadata is paid before each
/// nested walk; no edit witness or source content is copied or inspected.
fn coarse_reviewed_domain_bytes(
    domains: &[(DomainKey, DomainProof)],
    capacity: usize,
    remaining: &mut usize,
) -> Option<usize> {
    if capacity < domains.len() || !charge_work(remaining, domains.len()) {
        return None;
    }
    let mut bytes = capacity.checked_mul(std::mem::size_of::<(DomainKey, DomainProof)>())?;
    let add = |bytes: &mut usize, count: usize, item_size: usize| -> Option<()> {
        *bytes = bytes.checked_add(count.checked_mul(item_size)?)?;
        (*bytes <= COARSE_MEMORY_BYTES).then_some(())
    };
    add(&mut bytes, 0, 0)?;
    for (key, proof) in domains {
        if let Some((old, new)) = &key.local {
            add(
                &mut bytes,
                old.blocks.capacity(),
                std::mem::size_of::<BlockId>(),
            )?;
            add(
                &mut bytes,
                new.blocks.capacity(),
                std::mem::size_of::<BlockId>(),
            )?;
        }
        add(
            &mut bytes,
            proof.edits.capacity(),
            std::mem::size_of::<super::AtomicEdit>(),
        )?;
        if let Some(events) = &proof.stable_events {
            add(
                &mut bytes,
                events.capacity(),
                std::mem::size_of::<ProjectedEvent>(),
            )?;
            if !charge_work(remaining, events.len()) {
                return None;
            }
            for event in events {
                add(
                    &mut bytes,
                    event.occurrences.capacity(),
                    std::mem::size_of::<ProjectedOccurrence>(),
                )?;
                if !charge_work(remaining, event.occurrences.len()) {
                    return None;
                }
                for occurrence in &event.occurrences {
                    for intervals in [&occurrence.old, &occurrence.new].into_iter().flatten() {
                        add(
                            &mut bytes,
                            intervals.capacity(),
                            std::mem::size_of::<SourceInterval>(),
                        )?;
                    }
                }
            }
        }
    }
    Some(bytes)
}

/// Selects an exhaustive occurrence index using the least frequent needle token.
/// The caller charges the needle scan before using the result.
fn rarest_posting<'a, T: Eq + std::hash::Hash>(
    needle: &[T],
    postings: &'a HashMap<&T, Vec<usize>>,
) -> (usize, &'a [usize]) {
    needle
        .iter()
        .enumerate()
        .map(|(offset, token)| {
            (
                offset,
                postings.get(token).map(Vec::as_slice).unwrap_or_default(),
            )
        })
        .min_by_key(|(_, positions)| positions.len())
        .unwrap_or((0, &[]))
}

/// Every full occurrence contains every adjacent token pair at its offset.
/// The pair key is exact; it does not use a fingerprint as evidence.
fn rarest_pair_posting<'a, T: Eq + std::hash::Hash>(
    needle: &'a [T],
    postings: &'a HashMap<(&T, &T), Vec<usize>>,
) -> (usize, &'a [usize]) {
    needle
        .windows(2)
        .enumerate()
        .map(|(offset, pair)| {
            (
                offset,
                postings
                    .get(&(&pair[0], &pair[1]))
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        })
        .min_by_key(|(_, positions)| positions.len())
        .unwrap_or((0, &[]))
}

#[cfg(test)]
mod occurrence_index_tests {
    use super::{all_words, rarest_pair_posting, rarest_posting};
    use std::collections::HashMap;

    #[test]
    fn internal_postings_preserve_every_overlapping_occurrence() {
        for haystack in all_words(7) {
            let mut postings = HashMap::<&u8, Vec<usize>>::new();
            for (index, token) in haystack.iter().enumerate() {
                postings.entry(token).or_default().push(index);
            }
            let mut pairs = HashMap::<(&u8, &u8), Vec<usize>>::new();
            for (index, pair) in haystack.windows(2).enumerate() {
                pairs.entry((&pair[0], &pair[1])).or_default().push(index);
            }
            for needle in all_words(4).into_iter().filter(|word| !word.is_empty()) {
                let expected = haystack
                    .windows(needle.len())
                    .enumerate()
                    .filter_map(|(start, window)| (window == needle).then_some(start))
                    .collect::<Vec<_>>();
                let (offset, positions) = rarest_posting(&needle, &postings);
                let actual = positions
                    .iter()
                    .filter_map(|position| {
                        let start = position.checked_sub(offset)?;
                        (haystack.get(start..start + needle.len()) == Some(needle.as_slice()))
                            .then_some(start)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "haystack={haystack:?} needle={needle:?}");
                if needle.len() >= 2 {
                    let (offset, positions) = rarest_pair_posting(&needle, &pairs);
                    let actual = positions
                        .iter()
                        .filter_map(|position| {
                            let start = position.checked_sub(offset)?;
                            (haystack.get(start..start + needle.len()) == Some(needle.as_slice()))
                                .then_some(start)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        actual, expected,
                        "pair index: haystack={haystack:?} needle={needle:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn absent_internal_token_and_start_underflow_do_not_create_matches() {
        let haystack = *b"aba";
        let postings = HashMap::from([(&haystack[0], vec![0, 2]), (&haystack[1], vec![1])]);
        assert_eq!(rarest_posting(b"ac", &postings), (1, &[][..]));
        assert_eq!(rarest_posting(b"aab", &postings), (2, &[1][..]));
    }
}

fn tokens_equal_with_budget(
    old: &[ComparableToken],
    new: &[ComparableToken],
    remaining: &mut usize,
) -> Option<bool> {
    if !charge(remaining, 1) {
        return None;
    }
    if old.len() != new.len() {
        return Some(false);
    }
    // Most source windows disagree near the beginning. Charge the inspected
    // prefix rather than exhausting the budget on an unvisited suffix.
    for (old, new) in old.iter().zip(new) {
        if !charge(remaining, 1) {
            return None;
        }
        if old != new {
            return Some(false);
        }
    }
    Some(true)
}

fn semantic_signature(
    sides: [&Side<'_>; 2],
    groups: [&GroupText; 2],
    edits: &[super::AtomicEdit],
    remaining: &mut usize,
    limit: usize,
) -> Result<Option<Vec<ProjectedEvent>>> {
    let [old, new] = groups;
    let token_work = old.tokens.len().saturating_add(new.tokens.len());
    if !charge(remaining, token_work.saturating_add(edits.len())) {
        return Ok(None);
    }
    let mut output = Vec::new();
    let mut error = None;
    let mut visit = |hunk: super::SemanticHunk, _kind: ChangeKind| {
        if output.len() >= limit || !charge(remaining, token_work) {
            return false;
        }
        let mut events = Vec::new();
        super::append_semantic_hunk(old, new, edits, hunk, super::Confidence::High, &mut events);
        let Some(event) = events.first() else {
            return true;
        };
        if event.occurrences.len() > limit || !charge(remaining, event.occurrences.len()) {
            return false;
        }
        match projected_event(sides, event) {
            Ok(event) => {
                output.push(event);
                true
            }
            Err(cause) => {
                error = Some(cause);
                false
            }
        }
    };
    let complete = visit_domain_hunks(old, new, edits, &mut visit);
    if let Some(error) = error {
        return Err(error);
    }
    Ok(complete.then_some(output))
}

fn visit_domain_hunks(
    old: &GroupText,
    new: &GroupText,
    edits: &[super::AtomicEdit],
    mut visit: impl FnMut(super::SemanticHunk, ChangeKind) -> bool,
) -> bool {
    match super::beneficial_line_grouped_ranges(old, new, edits) {
        Some(groups) => groups
            .into_iter()
            .all(|(old, new)| visit(super::SemanticHunk { old, new }, ChangeKind::Replacement)),
        None => super::visit_semantic_hunks(&old.tokens, &new.tokens, edits, |hunk| {
            let kind =
                super::change_kind(hunk.old.start, hunk.new.start, hunk.old.end, hunk.new.end)
                    .expect("a semantic hunk changes at least one side");
            visit(hunk, kind)
        }),
    }
}

fn block_extent(side: &Side<'_>, span: &TextSpan) -> Result<Range<usize>> {
    let first = *span
        .blocks
        .first()
        .ok_or_else(|| invalid("empty source block list"))?;
    let mut start = *side
        .index
        .get(&first)
        .ok_or_else(|| invalid("unknown source block"))?;
    let mut end = start + 1;
    for block in &span.blocks {
        let index = *side
            .index
            .get(block)
            .ok_or_else(|| invalid("unknown source block"))?;
        start = start.min(index);
        end = end.max(index + 1);
    }
    Ok(start..end)
}

fn domain_group(side: &Side<'_>, range: Range<usize>, separator: BlockSeparator) -> GroupText {
    let blocks = side.blocks[range]
        .iter()
        .map(|block| block.block)
        .collect::<Vec<_>>();
    side.canonical_group(&blocks, (blocks.len() > 1).then_some(separator))
}

fn domain_separator(
    separator: Option<BlockSeparator>,
    proposal: Range<usize>,
    domain: Range<usize>,
) -> BlockSeparator {
    let separator = separator.unwrap_or(BlockSeparator::Space);
    // A mixed pattern names the two boundaries of its original group. It
    // cannot describe a wider or shifted proof domain. Use the ordinary
    // domain spacing model; localization still checks the proposal's own
    // pattern and rejects an incompatible correspondence.
    if matches!(separator, BlockSeparator::PerBoundary(_)) && proposal != domain {
        BlockSeparator::Space
    } else {
        separator
    }
}

fn proof_groups(sides: [&Side<'_>; 2], key: &DomainKey) -> Result<[GroupText; 2]> {
    let Some((old, new)) = &key.local else {
        return Ok([
            domain_group(sides[0], key.old.clone(), key.old_separator),
            domain_group(sides[1], key.new.clone(), key.new_separator),
        ]);
    };
    let group = |side: usize, span: &TextSpan| -> Result<GroupText> {
        GroupText::try_new(
            span.blocks.clone(),
            span.separator,
            span_tokens(sides[side], span)?,
            None,
            None,
            None,
            None,
        )
        .and_then(|group| {
            group.with_origins(span.canonical_range.start, span.comparable_range.start)
        })
        .ok_or_else(|| allocation_error("local domain source group"))
    };
    Ok([group(0, old)?, group(1, new)?])
}

/// Token-only view of an already closed source span. References preserve full
/// token equality (including unmapped font identity) without cloning payloads.
/// Formatting and line grouping are deliberately absent: only matching and
/// scalar/comparable cuts consume this view.
struct BorrowedGroupView<'a> {
    blocks: Vec<BlockId>,
    separator: Option<BlockSeparator>,
    tokens: Vec<&'a ComparableToken>,
    scalar_boundaries: Vec<usize>,
    selected: Range<usize>,
    retained_bytes: usize,
}

struct BorrowedGroupPlan {
    token_capacity: usize,
    bytes: usize,
    copy_work: usize,
}

const COARSE_MEMORY_BYTES: usize = 64 * 1024 * 1024;
static COARSE_SPACE: ComparableToken = ComparableToken::Scalar(' ');

fn plan_borrowed_group(
    side: &Side<'_>,
    span: &TextSpan,
    remaining: &mut usize,
) -> std::result::Result<BorrowedGroupPlan, MaterializationRefusal> {
    use MaterializationRefusal::{Memory, Unavailable, Work};
    let n = span.blocks.len();
    let scan_work = n
        .checked_mul(n)
        .and_then(|v| v.checked_add(n.checked_mul(4)?))
        .and_then(|v| v.checked_add(1))
        .ok_or(Memory)?;
    if !charge_work(remaining, scan_work) {
        return Err(Work);
    }
    let separator = super::effective_group_separator(n, span.separator);
    if separator.is_some_and(|s| !s.valid_for(n)) {
        return Err(Unavailable);
    }
    let mut token_capacity = n.saturating_sub(1);
    for (position, id) in span.blocks.iter().enumerate() {
        if span.blocks[..position].contains(id) {
            return Err(Unavailable);
        }
        let index = *side.index.get(id).ok_or(Unavailable)?;
        token_capacity = token_capacity
            .checked_add(side.canonical[index].len())
            .ok_or(Memory)?;
    }
    let bytes = token_capacity
        .checked_mul(std::mem::size_of::<&ComparableToken>() + std::mem::size_of::<usize>())
        .and_then(|v| v.checked_add(std::mem::size_of::<usize>()))
        .and_then(|v| v.checked_add(n.checked_mul(std::mem::size_of::<BlockId>())?))
        .ok_or(Memory)?;
    let copy_work = token_capacity
        .checked_mul(2)
        .and_then(|v| v.checked_add(n.checked_mul(2)?))
        .ok_or(Memory)?;
    Ok(BorrowedGroupPlan {
        token_capacity,
        bytes,
        copy_work,
    })
}

fn build_borrowed_group<'a>(
    side: &'a Side<'_>,
    span: &TextSpan,
    plan: &BorrowedGroupPlan,
) -> std::result::Result<BorrowedGroupView<'a>, MaterializationRefusal> {
    use MaterializationRefusal::{Memory, Unavailable};
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(span.blocks.len())
        .map_err(|_| Memory)?;
    blocks.extend_from_slice(&span.blocks);
    let separator = super::effective_group_separator(blocks.len(), span.separator);
    let mut tokens: Vec<&ComparableToken> = Vec::new();
    tokens
        .try_reserve_exact(plan.token_capacity)
        .map_err(|_| Memory)?;
    let mut scalar_boundaries = Vec::new();
    scalar_boundaries
        .try_reserve_exact(plan.token_capacity.checked_add(1).ok_or(Memory)?)
        .map_err(|_| Memory)?;
    scalar_boundaries.push(0);
    let mut scalars = 0usize;
    for (position, id) in blocks.iter().enumerate() {
        let next = &side.canonical[*side.index.get(id).ok_or(Unavailable)?];
        if position > 0
            && separator.is_some_and(|s| s.at(position - 1) == BlockSeparator::Space)
            && !tokens
                .last()
                .is_some_and(|token| super::is_space_token(token))
            && !next.first().is_some_and(super::is_space_token)
        {
            tokens.push(&COARSE_SPACE);
            scalars = scalars.checked_add(1).ok_or(Memory)?;
            scalar_boundaries.push(scalars);
        }
        for token in next {
            tokens.push(token);
            if matches!(token, ComparableToken::Scalar(_)) {
                scalars = scalars.checked_add(1).ok_or(Memory)?;
            }
            scalar_boundaries.push(scalars);
        }
    }
    let selected = span.comparable_range.start..span.comparable_range.end;
    if selected.start > selected.end
        || selected.end > tokens.len()
        || scalar_boundaries[selected.start] != span.canonical_range.start
        || scalar_boundaries[selected.end] != span.canonical_range.end
    {
        return Err(Unavailable);
    }
    Ok(BorrowedGroupView {
        blocks,
        separator,
        tokens,
        scalar_boundaries,
        selected,
        retained_bytes: plan.bytes,
    })
}

fn borrowed_group_pair<'a>(
    sides: [&'a Side<'_>; 2],
    spans: [&TextSpan; 2],
    remaining: &mut usize,
    memory_limit: usize,
) -> std::result::Result<[BorrowedGroupView<'a>; 2], MaterializationRefusal> {
    use MaterializationRefusal::{Memory, Work};
    let plans = [
        plan_borrowed_group(sides[0], spans[0], remaining)?,
        plan_borrowed_group(sides[1], spans[1], remaining)?,
    ];
    if plans[0].bytes.checked_add(plans[1].bytes).ok_or(Memory)? > memory_limit {
        return Err(Memory);
    }
    let copy_work = plans[0]
        .copy_work
        .checked_add(plans[1].copy_work)
        .ok_or(Memory)?;
    if !charge_work(remaining, copy_work) {
        return Err(Work);
    }
    Ok([
        build_borrowed_group(sides[0], spans[0], &plans[0])?,
        build_borrowed_group(sides[1], spans[1], &plans[1])?,
    ])
}

impl BorrowedGroupView<'_> {
    fn selected_tokens(&self) -> &[&ComparableToken] {
        &self.tokens[self.selected.clone()]
    }

    fn try_span(&self, range: Range<usize>) -> Option<TextSpan> {
        if range.start > range.end || range.end > self.selected.len() {
            return None;
        }
        let start = self.selected.start.checked_add(range.start)?;
        let end = self.selected.start.checked_add(range.end)?;
        Some(TextSpan {
            blocks: super::try_copy_slice(&self.blocks)?,
            separator: self.separator,
            canonical_range: ScalarRange {
                start: *self.scalar_boundaries.get(start)?,
                end: *self.scalar_boundaries.get(end)?,
            },
            comparable_range: TokenRange { start, end },
        })
    }
}

/// Keeps unsupported projection joins out of optional coarse proofs. The
/// canonical builder recognizes all whitespace; the legacy projector uses a
/// literal space. Disagreement is held here without changing either contract.
fn coarse_project(side: &Side<'_>, span: &TextSpan) -> Option<Vec<SourceInterval>> {
    if span
        .separator
        .is_some_and(|separator| !separator.valid_for(span.blocks.len()))
    {
        return None;
    }
    let mut previous = None;
    for (position, block) in span.blocks.iter().enumerate() {
        let next = side.canonical.get(*side.index.get(block)?)?;
        if position > 0
            && span
                .separator
                .is_some_and(|s| s.at(position - 1) == BlockSeparator::Space)
        {
            let canonical = !previous.is_some_and(super::is_space_token)
                && !next.first().is_some_and(super::is_space_token);
            let projected =
                !previous.is_some_and(space_token) && !next.first().is_some_and(space_token);
            if canonical != projected {
                return None;
            }
            if canonical {
                previous = Some(&COARSE_SPACE);
            }
        }
        if let Some(last) = next.last() {
            previous = Some(last);
        }
    }
    project(side, span).ok()
}

/// Conservative source check for a coarse cut. Every selected nonwhitespace
/// scalar must have one literal raw glyph counterpart. Source-map entries
/// crossing a cut and glyphs reused elsewhere are held, even though this proof
/// assigns no ownership. Whitespace-only and reconstructed-layout changes are
/// intentionally outside this rule.
fn coarse_source_guard(
    side: &Side<'_>,
    span: &TextSpan,
    tokens: &[&ComparableToken],
    remaining: &mut usize,
) -> Result<bool> {
    use crate::{model::GlyphId, normalize::TextSourceAtom};
    if !charge_work(
        remaining,
        tokens.len().saturating_add(
            side.blocks
                .len()
                .saturating_mul(3)
                .saturating_add(span.blocks.len()),
        ),
    ) {
        return Ok(false);
    }
    if tokens
        .iter()
        .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    {
        return Ok(false);
    }
    let selected_count = tokens
        .iter()
        .filter(|token| matches!(token, ComparableToken::Scalar(c) if !c.is_whitespace()))
        .count();
    if selected_count == 0
        || selected_count
            .checked_mul(128)
            .is_none_or(|bytes| bytes > COARSE_MEMORY_BYTES)
    {
        return Ok(false);
    }
    let mut metadata_work = side.blocks.len();
    for block in side.blocks {
        for mapped in [&block.raw, &block.canonical] {
            let Some(next) = metadata_work
                .checked_add(mapped.source_map.len())
                .and_then(|v| v.checked_add(mapped.unmapped.len()))
            else {
                return Ok(false);
            };
            metadata_work = next;
        }
    }
    if !charge_work(remaining, metadata_work) {
        return Ok(false);
    }
    let mut walk_work = metadata_work;
    for block in side.blocks {
        for mapped in [&block.raw, &block.canonical] {
            let Some(next) = walk_work.checked_add(mapped.text.len()) else {
                return Ok(false);
            };
            walk_work = next;
            for source in mapped
                .source_map
                .iter()
                .map(|entry| &entry.source)
                .chain(mapped.unmapped.iter().map(|entry| &entry.source))
            {
                let Some(next) = walk_work.checked_add(source.atoms.len()) else {
                    return Ok(false);
                };
                walk_work = next;
            }
        }
    }
    let Some(work) = walk_work
        .checked_mul(3)
        .and_then(|v| v.checked_add(span.blocks.len().checked_mul(usize::BITS as usize + 4)?))
        .and_then(|v| v.checked_add(tokens.len()))
    else {
        return Ok(false);
    };
    if !charge_work(remaining, work) {
        return Ok(false);
    }
    let Some(intervals) = coarse_project(side, span) else {
        return Ok(false);
    };
    let mut expected = tokens.iter().filter_map(|token| match token {
        ComparableToken::Scalar(c) if !c.is_whitespace() => Some(*c),
        _ => None,
    });
    let mut selected = HashMap::<GlyphId, (char, usize, usize)>::new();
    if selected.try_reserve(selected_count).is_err() {
        return Ok(false);
    }
    for interval in intervals {
        let block = &side.blocks[interval.block_index];
        let start = block_scalar_boundary(&block.canonical, interval.start);
        let end = block_scalar_boundary(&block.canonical, interval.end);
        for entry in &block.canonical.source_map {
            if entry.output_range.start < end
                && start < entry.output_range.end
                && (entry.output_range.start < start || entry.output_range.end > end)
            {
                return Ok(false);
            }
        }
        let mut entries = block.canonical.source_map.iter().peekable();
        for (scalar, character) in block.canonical.text.chars().enumerate() {
            if scalar < start || scalar >= end || character.is_whitespace() {
                continue;
            }
            if expected.next() != Some(character) {
                return Ok(false);
            }
            while entries
                .peek()
                .is_some_and(|entry| entry.output_range.end <= scalar)
            {
                entries.next();
            }
            let Some(entry) = entries.peek() else {
                return Ok(false);
            };
            if entry.output_range.start != scalar || entry.output_range.end != scalar + 1 {
                return Ok(false);
            }
            let [TextSourceAtom::Glyph(glyph)] = entry.source.atoms.as_slice() else {
                return Ok(false);
            };
            if selected.insert(*glyph, (character, 0, 0)).is_some() {
                return Ok(false);
            }
        }
    }
    if expected.next().is_some() || selected.len() != selected_count {
        return Ok(false);
    }
    for block in side.blocks {
        for (is_raw, mapped) in [(true, &block.raw), (false, &block.canonical)] {
            let mut characters = mapped.text.chars().enumerate().peekable();
            for entry in &mapped.source_map {
                while characters
                    .peek()
                    .is_some_and(|(index, _)| *index < entry.output_range.start)
                {
                    characters.next();
                }
                for atom in &entry.source.atoms {
                    let TextSourceAtom::Glyph(glyph) = atom else {
                        continue;
                    };
                    let Some((literal, raw_count, canonical_count)) = selected.get_mut(glyph)
                    else {
                        continue;
                    };
                    if entry.output_range.end != entry.output_range.start + 1
                        || entry.source.atoms.len() != 1
                        || characters.peek().map(|(_, character)| *character) != Some(*literal)
                    {
                        return Ok(false);
                    }
                    let count = if is_raw { raw_count } else { canonical_count };
                    *count += 1;
                    if *count > 1 {
                        return Ok(false);
                    }
                }
            }
            for unmapped in &mapped.unmapped {
                if unmapped.source.atoms.iter().any(|atom| matches!(atom, TextSourceAtom::Glyph(glyph) if selected.contains_key(glyph))) { return Ok(false); }
            }
        }
    }
    Ok(selected
        .values()
        .all(|(_, raw, canonical)| *raw == 1 && *canonical == 1))
}

fn coarse_substantive_mismatch(
    old: &[&ComparableToken],
    new: &[&ComparableToken],
    remaining: &mut usize,
) -> bool {
    let Some(count) = old.len().checked_add(new.len()) else {
        return false;
    };
    if count
        .checked_mul(128)
        .is_none_or(|bytes| bytes > COARSE_MEMORY_BYTES)
        || !charge_work(remaining, count.saturating_mul(3))
    {
        return false;
    }
    if old
        .iter()
        .chain(new)
        .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    {
        return false;
    }
    let mut counts = HashMap::<&ComparableToken, i64>::new();
    if counts.try_reserve(count).is_err() {
        return false;
    }
    for (tokens, delta) in [(old, 1), (new, -1)] {
        for token in tokens {
            if matches!(token, ComparableToken::Scalar(c) if !c.is_whitespace()) {
                *counts.entry(token).or_default() += delta;
            }
        }
    }
    counts.values().any(|count| *count != 0)
}

/// Checks current ownership and previously reported coarse regions without
/// mutating either. Synthetic separators never create source intervals.
fn coarse_gap_is_unresolved(
    sides: [&Side<'_>; 2],
    spans: [&TextSpan; 2],
    partitions: [&[ResolutionRange]; 2],
    regions: &[ProvenChangedRegion],
    remaining: &mut usize,
) -> Result<bool> {
    if !charge_work(remaining, regions.len().saturating_mul(2)) {
        return Ok(false);
    }
    for side in 0..2 {
        let n = spans[side].blocks.len();
        let Some(work) = n.checked_mul(usize::BITS as usize + n + partitions[side].len() + 4)
        else {
            return Ok(false);
        };
        if !charge_work(remaining, work) {
            return Ok(false);
        }
        let Some(intervals) = coarse_project(sides[side], spans[side]) else {
            return Ok(false);
        };
        if intervals.is_empty()
            || !intervals.iter().all(|interval| {
                partitions[side].iter().any(|part| {
                    part.block == sides[side].blocks[interval.block_index].block
                        && part.state == ResolutionState::Unresolved
                        && part.comparable_range.start <= interval.start
                        && part.comparable_range.end >= interval.end
                })
            })
        {
            return Ok(false);
        }
        for region in regions {
            let other = if side == 0 {
                &region.old_span
            } else {
                &region.new_span
            };
            let Some(other) = other else {
                continue;
            };
            let m = other.blocks.len();
            if m > n {
                return Ok(false);
            }
            let Some(work) = m.checked_mul(usize::BITS as usize + m + intervals.len() + 4) else {
                return Ok(false);
            };
            if !charge_work(remaining, work) {
                return Ok(false);
            }
            if m.checked_mul(128)
                .is_none_or(|bytes| bytes > COARSE_MEMORY_BYTES)
            {
                return Ok(false);
            }
            let Some(other) = coarse_project(sides[side], other) else {
                return Ok(false);
            };
            if intervals.iter().any(|a| {
                other
                    .iter()
                    .any(|b| a.block_index == b.block_index && a.start < b.end && b.start < a.end)
            }) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn nonempty_span(group: &GroupText) -> Option<TextSpan> {
    (!group.blocks.is_empty()).then(|| group.full_span())
}

fn assumptions(groups: [&GroupText; 2]) -> Vec<ComparisonAssumption> {
    let mut result = vec![
        ComparisonAssumption::InputReadingOrder,
        ComparisonAssumption::CanonicalNormalization,
    ];
    if groups.iter().any(|group| {
        group.separator.is_some_and(|separator| {
            (0..group.blocks.len().saturating_sub(1))
                .any(|boundary| separator.at(boundary) == BlockSeparator::Space)
        })
    }) {
        result.push(ComparisonAssumption::ReconstructedSpacing);
    }
    if groups.iter().any(|group| {
        group
            .tokens
            .iter()
            .any(|token| !matches!(token, ComparableToken::Scalar(_)))
    }) {
        result.push(ComparisonAssumption::UnmappedFontIdentity);
    }
    result
}

/// Full source scope without cloning its comparable tokens or formatting data.
///
/// `SidePlan` materializes exactly one canonical token for each scalar and each
/// unmapped entry. Consequently scalar counts can be recovered from the two
/// validated lengths without walking the token contents. Empty blocks retain
/// the preceding token's whitespace state, just as canonical group assembly does.
#[derive(Debug, PartialEq, Eq)]
struct LightGroupMetadata {
    span: TextSpan,
    has_unmapped: bool,
    reconstructed_spacing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaterializationRefusal {
    Work,
    Memory,
    Unavailable,
}

/// Charges a conservative logical bound before allocating or visiting blocks.
/// The memory bound covers the retained block identifiers; token payloads are
/// neither visited nor copied here. Failure never yields a partial scope.
fn light_group_metadata(
    side: &Side<'_>,
    range: Range<usize>,
    separator: BlockSeparator,
    remaining: &mut usize,
    memory_limit: usize,
) -> std::result::Result<LightGroupMetadata, MaterializationRefusal> {
    let blocks = side
        .blocks
        .get(range.clone())
        .ok_or(MaterializationRefusal::Unavailable)?;
    let canonical = side
        .canonical
        .get(range)
        .ok_or(MaterializationRefusal::Unavailable)?;
    let separator = super::effective_group_separator(blocks.len(), Some(separator));
    if separator.is_some_and(|separator| !separator.valid_for(blocks.len())) {
        return Err(MaterializationRefusal::Unavailable);
    }
    let bytes = blocks
        .len()
        .checked_mul(std::mem::size_of::<BlockId>())
        .ok_or(MaterializationRefusal::Memory)?;
    if bytes > memory_limit {
        return Err(MaterializationRefusal::Memory);
    }
    let work = blocks
        .len()
        .checked_mul(4)
        .and_then(|work| work.checked_add(1))
        .ok_or(MaterializationRefusal::Work)?;
    if !charge_work(remaining, work) {
        return Err(MaterializationRefusal::Work);
    }
    let mut identifiers = Vec::new();
    identifiers
        .try_reserve_exact(blocks.len())
        .map_err(|_| MaterializationRefusal::Memory)?;
    let mut token_count = 0usize;
    let mut scalar_count = 0usize;
    let mut last_is_space = None;
    let mut has_unmapped = false;
    let mut reconstructed_spacing = false;
    for (position, (block, next)) in blocks.iter().zip(canonical).enumerate() {
        let space_boundary = position > 0
            && separator
                .is_some_and(|separator| separator.at(position - 1) == BlockSeparator::Space);
        reconstructed_spacing |= space_boundary;
        if space_boundary
            && last_is_space != Some(true)
            && !next.first().is_some_and(super::is_space_token)
        {
            token_count = token_count
                .checked_add(1)
                .ok_or(MaterializationRefusal::Unavailable)?;
            scalar_count = scalar_count
                .checked_add(1)
                .ok_or(MaterializationRefusal::Unavailable)?;
            last_is_space = Some(true);
        }
        let block_scalars = next
            .len()
            .checked_sub(block.canonical.unmapped.len())
            .ok_or(MaterializationRefusal::Unavailable)?;
        token_count = token_count
            .checked_add(next.len())
            .ok_or(MaterializationRefusal::Unavailable)?;
        scalar_count = scalar_count
            .checked_add(block_scalars)
            .ok_or(MaterializationRefusal::Unavailable)?;
        has_unmapped |= !block.canonical.unmapped.is_empty();
        if let Some(last) = next.last() {
            last_is_space = Some(super::is_space_token(last));
        }
        identifiers.push(block.block);
    }
    Ok(LightGroupMetadata {
        span: TextSpan {
            blocks: identifiers,
            separator,
            canonical_range: ScalarRange {
                start: 0,
                end: scalar_count,
            },
            comparable_range: TokenRange {
                start: 0,
                end: token_count,
            },
        },
        has_unmapped,
        reconstructed_spacing,
    })
}

fn light_group_assumptions(
    groups: [&LightGroupMetadata; 2],
) -> std::result::Result<Vec<ComparisonAssumption>, MaterializationRefusal> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(4)
        .map_err(|_| MaterializationRefusal::Memory)?;
    result.extend([
        ComparisonAssumption::InputReadingOrder,
        ComparisonAssumption::CanonicalNormalization,
    ]);
    if groups.iter().any(|group| group.reconstructed_spacing) {
        result.push(ComparisonAssumption::ReconstructedSpacing);
    }
    if groups.iter().any(|group| group.has_unmapped) {
        result.push(ComparisonAssumption::UnmappedFontIdentity);
    }
    Ok(result)
}

/// Conservative requested capacities, including nested payloads and temporary
/// font unions. The runtime pair preflight must combine both sides' bytes.
struct GroupMaterializationPlan {
    tokens: usize,
    lines: usize,
    pages: usize,
    bytes: usize,
    copy_work: usize,
}

fn plan_group_materialization(
    side: &Side<'_>,
    range: Range<usize>,
    metadata: &LightGroupMetadata,
    remaining: &mut usize,
    memory_limit: usize,
) -> std::result::Result<GroupMaterializationPlan, MaterializationRefusal> {
    let blocks = side
        .blocks
        .get(range)
        .ok_or(MaterializationRefusal::Unavailable)?;
    let scan_work = blocks
        .len()
        .checked_mul(8)
        .ok_or(MaterializationRefusal::Work)?;
    if !charge_work(remaining, scan_work) {
        return Err(MaterializationRefusal::Work);
    }
    let add = |a: usize, b: usize| a.checked_add(b).ok_or(MaterializationRefusal::Memory);
    let mul = |a: usize, b: usize| a.checked_mul(b).ok_or(MaterializationRefusal::Memory);
    let tokens = metadata.span.comparable_range.end;
    let mut lines = blocks.len().saturating_sub(1);
    let mut pages = lines;
    let mut font_entries = 0;
    let mut unmapped_entries = 0;
    for block in blocks {
        lines = add(lines, block.line_breaks.as_ref().map_or(0, Vec::len))?;
        pages = add(pages, block.page_breaks.as_ref().map_or(0, Vec::len))?;
        font_entries = add(
            font_entries,
            block.font_size_signatures.as_ref().map_or(0, Vec::len),
        )?;
        unmapped_entries = add(unmapped_entries, block.canonical.unmapped.len())?;
    }
    // Both the group IDs and the later fallible relation-span ID copy can be
    // live together. Metadata identifiers become the group IDs by ownership.
    let mut bytes = mul(mul(blocks.len(), 2)?, std::mem::size_of::<BlockId>())?;
    bytes = add(bytes, mul(5, std::mem::size_of::<ComparisonAssumption>())?)?;
    bytes = add(bytes, mul(tokens, std::mem::size_of::<ComparableToken>())?)?;
    bytes = add(bytes, mul(add(tokens, 1)?, std::mem::size_of::<usize>())?)?;
    bytes = add(
        bytes,
        mul(
            tokens,
            std::mem::size_of::<crate::normalize::FontSizeSignature>(),
        )?,
    )?;
    bytes = add(
        bytes,
        mul(
            tokens,
            std::mem::size_of::<Option<crate::normalize::PositionSignature>>(),
        )?,
    )?;
    bytes = add(
        bytes,
        mul(add(lines, pages)?, std::mem::size_of::<usize>())?,
    )?;
    if bytes > memory_limit {
        return Err(MaterializationRefusal::Memory);
    }
    // Each nested length is O(1), but visiting all owning entries is itself paid.
    if !charge_work(remaining, add(font_entries, unmapped_entries)?) {
        return Err(MaterializationRefusal::Work);
    }
    let mut font_bits = 0;
    let mut hash_bytes = 0;
    for block in blocks {
        if let Some(signatures) = &block.font_size_signatures {
            for signature in signatures {
                font_bits = add(font_bits, signature.represented_size_count())?;
            }
        }
        for token in &block.canonical.unmapped {
            hash_bytes = add(hash_bytes, token.font_hash.0.len())?;
        }
    }
    // Each signature can supply at most two adjacent separator unions. Their
    // requested input-sum capacities remain counted even when values deduplicate.
    let copied_font_bits = mul(font_bits, 3)?;
    bytes = add(bytes, mul(copied_font_bits, std::mem::size_of::<u64>())?)?;
    bytes = add(bytes, hash_bytes)?;
    if bytes > memory_limit {
        return Err(MaterializationRefusal::Memory);
    }
    let copy_work = add(
        add(mul(tokens, 5)?, mul(blocks.len(), 8)?)?,
        add(add(lines, pages)?, add(copied_font_bits, hash_bytes)?)?,
    )?;
    Ok(GroupMaterializationPlan {
        tokens,
        lines,
        pages,
        bytes,
        copy_work,
    })
}

/// Copies all retained group fields after paying their conservative bound.
/// Every nested owning allocation is fallible; canonical source order and
/// formatting/line grouping semantics match ordinary group construction.
fn materialize_paid_group(
    side: &Side<'_>,
    range: Range<usize>,
    metadata: LightGroupMetadata,
    plan: &GroupMaterializationPlan,
    remaining: &mut usize,
) -> std::result::Result<GroupText, MaterializationRefusal> {
    use crate::{model::FontProgramHash, normalize::FontSizeSignature};

    if !charge_work(remaining, plan.copy_work) {
        return Err(MaterializationRefusal::Work);
    }
    let blocks = side
        .blocks
        .get(range.clone())
        .ok_or(MaterializationRefusal::Unavailable)?;
    let canonical = side
        .canonical
        .get(range)
        .ok_or(MaterializationRefusal::Unavailable)?;
    let reserve = |capacity| {
        let mut values = Vec::new();
        values
            .try_reserve_exact(capacity)
            .map_err(|_| MaterializationRefusal::Memory)?;
        Ok::<_, MaterializationRefusal>(values)
    };
    let mut tokens = reserve(plan.tokens)?;
    let mut font_sizes: Option<Vec<FontSizeSignature>> = Some(reserve_group_field(plan.tokens)?);
    let mut positions = Some(reserve_group_field(plan.tokens)?);
    let mut lines = Some(reserve_group_field(plan.lines)?);
    let mut pages = Some(reserve_group_field(plan.pages)?);
    let mut previous_page = None;
    for (position, (block, next)) in blocks.iter().zip(canonical).enumerate() {
        let preceding_count = tokens.len();
        if position > 0
            && metadata
                .span
                .separator
                .is_some_and(|separator| separator.at(position - 1) == BlockSeparator::Space)
            && !tokens.last().is_some_and(super::is_space_token)
            && !next.first().is_some_and(super::is_space_token)
        {
            tokens.push(ComparableToken::Scalar(' '));
        }
        let block_start = tokens.len();
        let inserted = block_start - preceding_count;
        for token in next {
            tokens.push(match token {
                ComparableToken::Scalar(scalar) => ComparableToken::Scalar(*scalar),
                ComparableToken::Unmapped {
                    font_hash,
                    glyph_id,
                } => {
                    let mut bytes = Vec::new();
                    bytes
                        .try_reserve_exact(font_hash.0.len())
                        .map_err(|_| MaterializationRefusal::Memory)?;
                    bytes.extend_from_slice(&font_hash.0);
                    ComparableToken::Unmapped {
                        font_hash: FontProgramHash(bytes),
                        glyph_id: *glyph_id,
                    }
                }
            });
        }
        match (&mut font_sizes, &block.font_size_signatures) {
            (Some(combined), Some(next_sizes)) => {
                if inserted == 1 {
                    if let (Some(left), Some(right)) = (combined.last(), next_sizes.first()) {
                        let union = left
                            .try_union(right)
                            .ok_or(MaterializationRefusal::Memory)?;
                        combined.push(union);
                    } else {
                        font_sizes = None;
                    }
                }
                if let Some(combined) = &mut font_sizes {
                    for signature in next_sizes {
                        combined.push(
                            signature
                                .try_clone()
                                .ok_or(MaterializationRefusal::Memory)?,
                        );
                    }
                }
            }
            _ => font_sizes = None,
        }
        match (&mut positions, &block.position_signatures) {
            (Some(combined), Some(next_positions)) => {
                if inserted == 1 {
                    combined.push(None);
                }
                combined.extend(next_positions.iter().copied().map(Some));
            }
            _ => positions = None,
        }
        if position > 0 {
            if let Some(breaks) = &mut lines {
                breaks.push(block_start);
            }
            match (previous_page, block.pages.first().copied()) {
                (Some(previous), Some(current)) if previous != current => {
                    if let Some(breaks) = &mut pages {
                        breaks.push(block_start);
                    }
                }
                (Some(_), Some(_)) => {}
                _ => pages = None,
            }
        }
        for (combined, next_breaks) in [
            (&mut lines, &block.line_breaks),
            (&mut pages, &block.page_breaks),
        ] {
            match (combined.as_mut(), next_breaks) {
                (Some(breaks), Some(next_breaks)) => {
                    for offset in next_breaks {
                        breaks.push(
                            block_start
                                .checked_add(*offset)
                                .ok_or(MaterializationRefusal::Unavailable)?,
                        );
                    }
                }
                _ => *combined = None,
            }
        }
        previous_page = block.pages.last().copied();
    }
    if tokens.len() != plan.tokens {
        return Err(MaterializationRefusal::Unavailable);
    }
    GroupText::try_new(
        metadata.span.blocks,
        metadata.span.separator,
        tokens,
        font_sizes,
        positions,
        lines,
        pages,
    )
    .ok_or(MaterializationRefusal::Memory)
}

fn reserve_group_field<T>(capacity: usize) -> std::result::Result<Vec<T>, MaterializationRefusal> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| MaterializationRefusal::Memory)?;
    Ok(values)
}

/// The paid pair bound includes these token/block visits and a five-entry
/// assumption vector, including a possible isolated-domain boundary premise.
fn paid_group_assumptions(groups: [&GroupText; 2]) -> Result<Vec<ComparisonAssumption>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(5)
        .map_err(|_| allocation_error("paid domain assumptions"))?;
    result.extend([
        ComparisonAssumption::InputReadingOrder,
        ComparisonAssumption::CanonicalNormalization,
    ]);
    if groups.iter().any(|group| {
        group.separator.is_some_and(|separator| {
            (0..group.blocks.len().saturating_sub(1))
                .any(|boundary| separator.at(boundary) == BlockSeparator::Space)
        })
    }) {
        result.push(ComparisonAssumption::ReconstructedSpacing);
    }
    if groups
        .iter()
        .any(|group| group.tokens.iter().any(|token| !token.is_scalar()))
    {
        result.push(ComparisonAssumption::UnmappedFontIdentity);
    }
    Ok(result)
}

/// Both sides' capacities and copy work are approved before either token group
/// is copied. The local copy allowance only consumes the already-paid work.
fn paid_global_groups(
    sides: [&Side<'_>; 2],
    key: &DomainKey,
    remaining: &mut usize,
    memory_limit: usize,
) -> std::result::Result<[GroupText; 2], MaterializationRefusal> {
    if key.local.is_some() {
        return Err(MaterializationRefusal::Unavailable);
    }
    let identifier_bytes = key
        .old
        .len()
        .checked_add(key.new.len())
        .and_then(|count| count.checked_mul(std::mem::size_of::<BlockId>()))
        .ok_or(MaterializationRefusal::Memory)?;
    if identifier_bytes > memory_limit {
        return Err(MaterializationRefusal::Memory);
    }
    let old = light_group_metadata(
        sides[0],
        key.old.clone(),
        key.old_separator,
        remaining,
        memory_limit,
    )?;
    let new = light_group_metadata(
        sides[1],
        key.new.clone(),
        key.new_separator,
        remaining,
        memory_limit,
    )?;
    let old_plan =
        plan_group_materialization(sides[0], key.old.clone(), &old, remaining, memory_limit)?;
    let new_plan =
        plan_group_materialization(sides[1], key.new.clone(), &new, remaining, memory_limit)?;
    let bytes = old_plan
        .bytes
        .checked_add(new_plan.bytes)
        .ok_or(MaterializationRefusal::Memory)?;
    if bytes > memory_limit {
        return Err(MaterializationRefusal::Memory);
    }
    let copy_work = old_plan
        .copy_work
        .checked_add(new_plan.copy_work)
        .ok_or(MaterializationRefusal::Work)?;
    if !charge_work(remaining, copy_work) {
        return Err(MaterializationRefusal::Work);
    }
    let mut copy_allowance = copy_work;
    Ok([
        materialize_paid_group(
            sides[0],
            key.old.clone(),
            old,
            &old_plan,
            &mut copy_allowance,
        )?,
        materialize_paid_group(
            sides[1],
            key.new.clone(),
            new,
            &new_plan,
            &mut copy_allowance,
        )?,
    ])
}

/// Conclusion of the per-path proposal proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProposalProof {
    /// Every optimal path agrees on one non-empty strict hunk signature.
    Invariant,
    /// The traversal completed without such an agreement.
    NotInvariant,
    /// The traversal ran out of work before reaching a conclusion.
    Exhausted,
    /// Bounded materialization was unavailable without exhausting work.
    Resource,
}

/// Localized target ranges and their strict changed hunks for one path.
type ProposalHunkSignature = (
    Range<usize>,
    Range<usize>,
    Vec<(Range<usize>, Range<usize>)>,
);

/// Per-path signature of one proposal's strict changed hunks.
///
/// A path only contributes a signature when every changed hunk inside the
/// target ranges is fully contained; a hunk crossing a boundary, a missing
/// target range or no changed hunk at all is absent and never counts as a
/// proof on its own.
fn target_hunk_signature(
    target_old: &Range<usize>,
    target_new: &Range<usize>,
    edits: &[super::AtomicEdit],
) -> Option<ProposalHunkSignature> {
    let mut hunks = Vec::new();
    let mut crossing = false;
    super::visit_atomic_hunks(edits, |hunk| {
        let old_overlap = hunk.old.start < target_old.end && target_old.start < hunk.old.end;
        let new_overlap = hunk.new.start < target_new.end && target_new.start < hunk.new.end;
        let old_inside = target_old.start <= hunk.old.start && hunk.old.end <= target_old.end;
        let new_inside = target_new.start <= hunk.new.start && hunk.new.end <= target_new.end;
        if (old_overlap && !old_inside) || (new_overlap && !new_inside) {
            crossing = true;
            return false;
        }
        if old_inside && new_inside {
            hunks.push((hunk.old.clone(), hunk.new.clone()));
        }
        true
    });
    if crossing || hunks.is_empty() {
        return None;
    }
    Some((target_old.clone(), target_new.clone(), hunks))
}

fn point_on_script(point: [usize; 2], edits: &[super::AtomicEdit], lengths: [usize; 2]) -> bool {
    let mut cursor = [0, 0];
    let mut index = 0;
    while index < edits.len() {
        let first = &edits[index];
        if point[0] >= cursor[0]
            && point[0] <= first.old.start
            && point[1] >= cursor[1]
            && point[1] <= first.new.start
            && point[0] - cursor[0] == point[1] - cursor[1]
        {
            return true;
        }
        let start = [first.old.start, first.new.start];
        let mut end = [first.old.end, first.new.end];
        index += 1;
        while let Some(next) = edits.get(index) {
            if [next.old.start, next.new.start] != end {
                break;
            }
            end = [next.old.end, next.new.end];
            index += 1;
        }
        // Edit-step permutations inside one hunk are not correspondence
        // boundaries. Only its two outer corners have stable coordinates.
        if point == start || point == end {
            return true;
        }
        cursor = end;
    }
    point[0] >= cursor[0]
        && point[0] <= lengths[0]
        && point[1] >= cursor[1]
        && point[1] <= lengths[1]
        && point[0] - cursor[0] == point[1] - cursor[1]
}

fn locate_in_group(
    side: &Side<'_>,
    span: Option<&TextSpan>,
    group: &GroupText,
) -> Result<Option<Range<usize>>> {
    let Some(span) = span else {
        return Ok(group.tokens.is_empty().then_some(0..0));
    };
    project(side, span)?;
    let Some(start_block) = group
        .blocks
        .iter()
        .position(|block| span.blocks.first() == Some(block))
    else {
        return Ok(None);
    };
    let end_block = start_block + span.blocks.len();
    if group.blocks.get(start_block..end_block) != Some(span.blocks.as_slice())
        || (span.blocks.len() > 1
            && span.separator
                != group
                    .separator
                    .map(|separator| separator.subspan(start_block, span.blocks.len())))
    {
        return Ok(None);
    }
    let prefix = side.canonical_group(
        &group.blocks[..start_block],
        group
            .separator
            .map(|separator| separator.subspan(0, start_block)),
    );
    let local = side.canonical_group(&span.blocks, span.separator);
    let separator = usize::from(
        start_block > 0
            && group
                .separator
                .map(|separator| separator.at(start_block - 1))
                == Some(BlockSeparator::Space)
            && !prefix.tokens.last().is_some_and(space_token)
            && !local.tokens.first().is_some_and(space_token),
    );
    let Some(start) = (prefix.tokens.len() + separator + span.comparable_range.start)
        .checked_sub(group.comparable_origin)
    else {
        return Ok(None);
    };
    let Some(end) = (prefix.tokens.len() + separator + span.comparable_range.end)
        .checked_sub(group.comparable_origin)
    else {
        return Ok(None);
    };
    Ok((end <= group.tokens.len()).then_some(start..end))
}

fn localize_proposal(
    sides: [&Side<'_>; 2],
    spans: [Option<&TextSpan>; 2],
    groups: [&GroupText; 2],
    edits: &[super::AtomicEdit],
) -> Result<[Option<Range<usize>>; 2]> {
    let mut ranges = [
        locate_in_group(sides[0], spans[0], groups[0])?,
        locate_in_group(sides[1], spans[1], groups[1])?,
    ];
    for missing in 0..2 {
        let present = 1 - missing;
        if spans[missing].is_some() || ranges[missing].is_some() {
            continue;
        }
        let Some(changed) = ranges[present].as_ref() else {
            continue;
        };
        if changed.is_empty() {
            continue;
        }
        let mut boundary = None;
        // Only a pure insertion/deletion in the parent's established edit
        // script supplies the absent side. Nearby equal text is insufficient.
        super::visit_atomic_hunks(edits, |hunk| {
            let pair = [hunk.old, hunk.new];
            if pair[missing].is_empty()
                && pair[present].start <= changed.start
                && changed.end <= pair[present].end
            {
                boundary = Some(pair[missing].clone());
            }
            true
        });
        ranges[missing] = boundary;
    }
    Ok(ranges)
}

fn contains_span(
    side: &Side<'_>,
    outer: Option<&TextSpan>,
    inner: Option<&TextSpan>,
) -> Result<bool> {
    match (outer, inner) {
        (_, None) => Ok(true),
        (None, Some(_)) => Ok(false),
        (Some(outer), Some(inner)) => {
            // A structural miss cannot become contained after canonicalization.
            // Keep the inner projection validation even on that miss, and use
            // the ordinary path for unknown outer IDs or malformed separators
            // so it retains their original validation and failure behavior.
            if outer
                .blocks
                .iter()
                .all(|block| side.index.contains_key(block))
                && outer
                    .separator
                    .is_none_or(|separator| separator.valid_for(outer.blocks.len()))
                && !span_may_contain(Some(outer), Some(inner))
            {
                project(side, inner)?;
                return Ok(false);
            }
            let group = side.canonical_group(&outer.blocks, outer.separator);
            Ok(
                locate_in_group(side, Some(inner), &group)?.is_some_and(|range| {
                    range.start >= outer.comparable_range.start
                        && range.end <= outer.comparable_range.end
                }),
            )
        }
    }
}

/// Necessary structural precondition of [`contains_span`].
///
/// [`locate_in_group`] builds the group from the outer span's blocks in their
/// stored order (`GroupText::new(blocks.to_vec(), ..)`), finds the first
/// position of the inner span's first block, and requires the following slice
/// to equal the inner blocks exactly. This mirrors that check without assuming
/// anything about the numeric order of block ids, so a valid span whose ids are
/// not monotonically increasing is never rejected. Failing the check, the exact
/// range comparison cannot succeed; callers charge the structural visit
/// explicitly before skipping the expensive canonicalization.
fn span_may_contain(outer: Option<&TextSpan>, inner: Option<&TextSpan>) -> bool {
    match (outer, inner) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(outer), Some(inner)) => {
            let Some(first) = inner.blocks.first() else {
                return false;
            };
            let Some(start) = outer.blocks.iter().position(|block| block == first) else {
                return false;
            };
            outer.blocks.get(start..start + inner.blocks.len()) == Some(inner.blocks.as_slice())
        }
    }
}

fn occurrence_indices(alignment: &Alignment, spans: [Option<&TextSpan>; 2]) -> [Option<usize>; 2] {
    std::array::from_fn(|side| {
        spans[side].and_then(|span| {
            alignment.spans.iter().position(|aligned| {
                let blocks = if side == 0 {
                    &aligned.old
                } else {
                    &aligned.new
                };
                span.blocks
                    .first()
                    .is_some_and(|block| blocks.contains(block))
            })
        })
    })
}

fn candidate_groups(
    sides: [&Side<'_>; 2],
    candidates: &mut [ChangeCandidate],
    limit: usize,
) -> Result<bool> {
    let mut intervals = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        for occurrence in &candidate.change.occurrences {
            for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                .into_iter()
                .enumerate()
            {
                if let Some(span) = span {
                    let projected = project(sides[side], span)?;
                    if intervals.len().saturating_add(projected.len()) > limit.saturating_mul(2) {
                        return Ok(true);
                    }
                    for interval in projected {
                        intervals.push((
                            side,
                            interval.block_index,
                            interval.start,
                            interval.end,
                            index,
                        ));
                    }
                }
            }
        }
    }
    intervals.sort_unstable();
    let mut parents = (0..candidates.len()).collect::<Vec<_>>();
    let root = |parents: &mut [usize], mut index: usize| {
        while parents[index] != index {
            parents[index] = parents[parents[index]];
            index = parents[index];
        }
        index
    };
    let mut previous: Option<(usize, usize, usize, usize)> = None;
    for (side, block, start, end, index) in intervals {
        if let Some((previous_side, previous_block, previous_end, previous_index)) = previous
            && side == previous_side
            && block == previous_block
            && start < previous_end
        {
            let left = root(&mut parents, previous_index);
            let right = root(&mut parents, index);
            parents[left.max(right)] = left.min(right);
            previous = Some((side, block, previous_end.max(end), index));
        } else {
            previous = Some((side, block, end, index));
        }
    }
    let groups = (0..candidates.len())
        .map(|index| root(&mut parents, index))
        .collect::<Vec<_>>();
    for (candidate, group) in candidates.iter_mut().zip(groups) {
        candidate.alternative_group = group;
    }
    Ok(false)
}

fn unresolved_output(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    partitions: [&[ResolutionRange]; 2],
    mut original: Vec<UnresolvedRegion>,
    limit: usize,
) -> Result<Vec<UnresolvedRegion>> {
    let mut output = Vec::new();
    let mut retained = [Vec::<SourceInterval>::new(), Vec::new()];
    for aligned in &alignment.spans {
        let old_span = full_relation_span(sides[0], &aligned.old, aligned.old_separator);
        let new_span = full_relation_span(sides[1], &aligned.new, aligned.new_separator);
        if sides[0].source_token_count(&aligned.old) > 0
            || sides[1].source_token_count(&aligned.new) > 0
        {
            original.push(UnresolvedRegion {
                old_span,
                new_span,
                evidence: aligned.evidence.clone(),
            });
        }
    }
    for region in original {
        let mut projected = [Vec::new(), Vec::new()];
        let mut valid = true;
        for (side, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
            .into_iter()
            .enumerate()
        {
            if let Some(span) = span {
                projected[side] = project(sides[side], span)?;
                valid &= projected[side].iter().all(|interval| {
                    let block = sides[side].blocks[interval.block_index].block;
                    partitions[side].iter().any(|part| {
                        part.block == block
                            && part.state == ResolutionState::Unresolved
                            && part.comparable_range.start <= interval.start
                            && part.comparable_range.end >= interval.end
                    }) && !retained[side].iter().any(|previous| {
                        previous.block_index == interval.block_index
                            && previous.start < interval.end
                            && interval.start < previous.end
                    })
                });
            }
        }
        if valid
            && (projected.iter().any(|ranges| !ranges.is_empty())
                || region.evidence.contains(&AlignmentEvidence::ExtractionGap)
                || region
                    .evidence
                    .contains(&AlignmentEvidence::NormalizationIssue))
        {
            reserve_ranges(&mut output, 1, limit)?;
            output.push(region);
            for (side, projected) in projected.into_iter().enumerate() {
                retained[side].extend(projected);
            }
        }
    }
    for side in 0..2 {
        merge_intervals(&mut retained[side]);
        // Adjacent unresolved partitions often revisit one source block. Keep
        // only its immutable reasons, in original alignment order. A miss
        // clears the previous value; this never caches ownership or a verdict.
        let mut evidence_block = None;
        let mut block_evidence = Vec::new();
        for range in partitions[side]
            .iter()
            .filter(|range| range.state == ResolutionState::Unresolved)
        {
            let block_index = sides[side].index[&range.block];
            let mut cursor = range.comparable_range.start;
            let group = sides[side].canonical_group(&[range.block], None);
            let mut emit = |start, end| -> Result<()> {
                if start == end {
                    return Ok(());
                }
                let span = group.span(start, end);
                if evidence_block != Some(range.block) {
                    block_evidence.clear();
                    for aligned in &alignment.spans {
                        let blocks = if side == 0 {
                            &aligned.old
                        } else {
                            &aligned.new
                        };
                        if blocks.contains(&range.block) {
                            for reason in &aligned.evidence {
                                if !block_evidence.contains(reason) {
                                    block_evidence.push(*reason);
                                }
                            }
                        }
                    }
                    evidence_block = Some(range.block);
                }
                reserve_ranges(&mut output, 1, limit)?;
                output.push(UnresolvedRegion {
                    old_span: (side == 0).then(|| span.clone()),
                    new_span: (side == 1).then_some(span),
                    evidence: block_evidence.clone(),
                });
                Ok(())
            };
            for previous in retained[side]
                .iter()
                .filter(|previous| previous.block_index == block_index)
            {
                if previous.end <= cursor || previous.start >= range.comparable_range.end {
                    continue;
                }
                emit(cursor, previous.start.max(cursor))?;
                cursor = cursor.max(previous.end);
            }
            emit(cursor, range.comparable_range.end)?;
        }
    }
    Ok(output)
}

/// Optional raw per-glyph displacement evidence for the two sides.
///
/// The assessment borrows the sidecars; a comparison without them keeps the
/// previous behaviour and never runs the exact-displacement rule. The token
/// mapping is built later from the same shared work budget.
#[derive(Clone, Copy)]
pub(crate) struct ExactDisplacementInput<'a> {
    pub old: &'a [crate::model::GlyphDisplacement],
    pub new: &'a [crate::model::GlyphDisplacement],
}

/// The only transition from discovered proposals to public comparison output.
pub(super) fn finish(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: Option<SentenceRecoveryInput<'_>>,
    recovery_plan: Option<&sentence::SentenceRecoveryPlan>,
    exact_displacement: Option<ExactDisplacementInput<'_>>,
    proposed: ProposedComparison,
    options: DiffOptions,
) -> Result<Comparison> {
    let (proposals, proposals_truncated) = collect_relations(
        sides,
        alignment,
        recovery_plan,
        &proposed,
        options.max_assessment_ranges,
    )?;
    let mut assessor =
        Assessor::new_with_evidence(sides, alignment, recovery, options, exact_displacement)?;
    let after_anchors = assessor.remaining_work;
    let mut accepted = Vec::new();
    for proposal in &proposals {
        let index = assessor.assess_cached(proposal)?;
        if assessor.records[index].outcome == RelationOutcome::Established {
            if assessor.validate_semantic_emission(index, &proposed.changes)? {
                accepted.push(index);
            } else {
                assessor.reject_semantic(proposal);
            }
        }
    }
    for change in &proposed.changes {
        if change.kind != ChangeKind::Move {
            continue;
        }
        for occurrence in &change.occurrences {
            let proposal = ProposedRelation {
                old: occurrence.old_span.clone(),
                new: occurrence.new_span.clone(),
                span_indices: occurrence_indices(
                    alignment,
                    [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()],
                ),
                exact_recovery: false,
            };
            assessor.assess_move_cached(&proposal)?;
        }
    }
    let after_initial_localization = assessor.remaining_work;
    assessor.discover_local_domains(&proposals)?;
    let after_views = assessor.remaining_work;
    if assessor.remaining_work > 0
        && (!assessor.local_domains.is_empty() || !assessor.local_anchors.is_empty())
    {
        for proposal in &proposals {
            // Preserve completed exact anchors before spending optional work
            // on edited domains. Local content is emitted from the parent's
            // verified edit script after ordinary result emission.
            if !proposal.exact_recovery {
                continue;
            }
            let Some(previous) = assessor.cached_relation(proposal) else {
                continue;
            };
            if assessor.semantic_rejected(proposal) {
                continue;
            }
            if assessor.records[previous].outcome != RelationOutcome::Tentative {
                continue;
            }
            let index = assessor.reassess(proposal)?;
            if assessor.records[index].outcome == RelationOutcome::Established {
                if assessor.validate_semantic_emission(index, &proposed.changes)? {
                    accepted.push(index);
                } else {
                    assessor.reject_semantic(proposal);
                }
            }
        }
    }
    let after_localization = assessor.remaining_work;
    let mut ownership = [Ownership::new(), Ownership::new()];
    for &index in &accepted {
        let relation = &assessor.records[index];
        for (side, span) in [relation.old_span.as_ref(), relation.new_span.as_ref()]
            .into_iter()
            .enumerate()
        {
            if let Some(span) = span {
                ownership[side].accept(sides[side], span, options.max_assessment_ranges)?;
            }
        }
    }
    let mut changes = Vec::new();
    let mut candidates = Vec::new();
    let mut candidates_truncated = proposals_truncated;
    for change in proposed.changes {
        let mut established = Vec::new();
        for occurrence in &change.occurrences {
            let proposal = ProposedRelation {
                old: occurrence.old_span.clone(),
                new: occurrence.new_span.clone(),
                span_indices: occurrence_indices(
                    alignment,
                    [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()],
                ),
                exact_recovery: false,
            };
            let cached_relation = assessor.cached_relation(&proposal);
            let cached_move = if change.kind == ChangeKind::Move {
                assessor.cached_move(&proposal)
            } else {
                None
            };
            let semantic_rejected = assessor.semantic_rejected(&proposal);
            let mut owner = if semantic_rejected {
                None
            } else if let Some(move_result) = cached_move {
                move_result
            } else {
                cached_relation.filter(|&index| {
                    assessor
                        .records
                        .get(index)
                        .is_some_and(|relation| relation.outcome == RelationOutcome::Established)
                })
            };
            if owner.is_none()
                && !semantic_rejected
                && !(change.kind == ChangeKind::Move && cached_move.is_some())
            {
                for &index in &accepted {
                    let relation = &assessor.records[index];
                    if contains_span(
                        sides[0],
                        relation.old_span.as_ref(),
                        occurrence.old_span.as_ref(),
                    )? && contains_span(
                        sides[1],
                        relation.new_span.as_ref(),
                        occurrence.new_span.as_ref(),
                    )? {
                        owner = Some(index);
                        break;
                    }
                }
            }
            if !semantic_rejected && change.kind == ChangeKind::Move && cached_move.is_none() {
                owner = assessor.assess_move_cached(&proposal)?;
                if let Some(index) = owner {
                    assessor.remember_relation(&proposal, index);
                    for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(span) = span {
                            ownership[side].accept(
                                sides[side],
                                span,
                                options.max_assessment_ranges,
                            )?;
                        }
                    }
                }
            }
            if !semantic_rejected && let Some(Some(_)) = cached_move {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        ownership[side].accept(sides[side], span, options.max_assessment_ranges)?;
                    }
                }
            }
            if owner.is_some() {
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        ownership[side].change(sides[side], span, options.max_assessment_ranges)?;
                    }
                }
                established.push(occurrence.clone());
            } else {
                let mut relation = cached_relation.unwrap_or(assessor.assess_cached(&proposal)?);
                if assessor.records[relation].outcome == RelationOutcome::Established {
                    let mut record = assessor.records[relation].clone();
                    record.parent = Some(relation);
                    record.outcome = RelationOutcome::Tentative;
                    record.reasons.push(AssessmentReason::DomainNotClosed);
                    relation = assessor.record(record)?;
                }
                let alternative_group = assessor.records[relation].parent.unwrap_or(relation);
                for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(span) = span {
                        ownership[side].exclude(
                            sides[side],
                            span,
                            options.max_assessment_ranges,
                        )?;
                    }
                }
                if candidates.len() < options.max_assessment_ranges {
                    candidates.push(ChangeCandidate {
                        change: ChangeEvent {
                            occurrences: vec![occurrence.clone()],
                            ..change.clone()
                        },
                        relation,
                        alternative_group,
                    });
                } else {
                    candidates_truncated = true;
                }
            }
        }
        if !established.is_empty() {
            changes.push(ChangeEvent {
                occurrences: established,
                ..change
            });
        }
    }
    let mut formatting_changes = Vec::new();
    candidates_truncated |=
        assessor.recover_local(&mut ownership, &mut changes, &mut candidates)?;
    assessor.recover_closed_domains(&mut ownership, &mut changes, &candidates)?;
    if assessor.output_stop.is_none() {
        candidates_truncated |=
            assessor.recover_stationary_members(&mut ownership, &mut changes, &mut candidates)?;
        candidates_truncated |= assessor.recover_raw_source_equalities(
            &mut ownership,
            &mut changes,
            &mut candidates,
        )?;
        candidates_truncated |= assessor.recover_positioned_replacements(
            &mut ownership,
            &mut changes,
            &mut candidates,
        )?;
    }
    for formatting in proposed.formatting_changes {
        for &index in &accepted {
            let relation = &assessor.records[index];
            if contains_span(
                sides[0],
                relation.old_span.as_ref(),
                Some(&formatting.old_span),
            )? && contains_span(
                sides[1],
                relation.new_span.as_ref(),
                Some(&formatting.new_span),
            )? {
                formatting_changes.push(formatting);
                break;
            }
        }
    }
    // The early partitions let the existing proof and emission passes test
    // their whole-domain claims before the deferred equal-fragment tail pass
    // changes ownership. The final partitions are recomputed from the
    // committed ownership after that pass, which never adopts a fragment that
    // would resolve part of a proven changed region.
    let early_old_resolution = ownership[0].resolution(sides[0], options.max_assessment_ranges)?;
    let early_new_resolution = ownership[1].resolution(sides[1], options.max_assessment_ranges)?;
    let early_partitions = [&early_old_resolution[..], &early_new_resolution[..]];
    let mut proven_changed_regions = Vec::new();
    // Local recovery can resolve part of an earlier unlocalized proof. Its
    // original whole-domain proof cannot be reused for the remaining ranges.
    let entirely_unresolved =
        |partitions: [&[ResolutionRange]; 2], spans: [Option<&TextSpan>; 2]| -> Result<bool> {
            for (side, span) in spans.into_iter().enumerate() {
                let Some(span) = span else {
                    continue;
                };
                let partition = partitions[side];
                if !project(sides[side], span)?.iter().all(|interval| {
                    partition.iter().any(|part| {
                        part.block == sides[side].blocks[interval.block_index].block
                            && part.state == ResolutionState::Unresolved
                            && part.comparable_range.start <= interval.start
                            && part.comparable_range.end >= interval.end
                    })
                }) {
                    return Ok(false);
                }
            }
            Ok(true)
        };
    for region in proposed.proven_changed_regions {
        let proposal = ProposedRelation {
            old: region.old_span.clone(),
            new: region.new_span.clone(),
            span_indices: occurrence_indices(
                alignment,
                [region.old_span.as_ref(), region.new_span.as_ref()],
            ),
            exact_recovery: false,
        };
        let key = assessor.domain_key(&proposal)?;
        if matches!(assessor.prove_domain(&key)?, DomainState::Stopped(_)) {
            break;
        }
        let domain = &assessor.records[assessor.domains[&key].relation];
        if domain.outcome == RelationOutcome::Established
            && domain.old_span == region.old_span
            && domain.new_span == region.new_span
            && entirely_unresolved(
                early_partitions,
                [region.old_span.as_ref(), region.new_span.as_ref()],
            )?
        {
            proven_changed_regions.push(region);
        }
    }
    let domain_indices = assessor
        .domains
        .values()
        .filter(|proof| proof.scope == ProofScope::ExactKey)
        .map(|proof| proof.relation)
        .collect::<HashSet<_>>();
    // Source order makes emission independent of hash-map iteration order.
    for index in 0..assessor.records.len() {
        if !domain_indices.contains(&index) {
            continue;
        }
        let relation = &assessor.records[index];
        if relation.outcome != RelationOutcome::Established {
            continue;
        }
        if !entirely_unresolved(
            early_partitions,
            [relation.old_span.as_ref(), relation.new_span.as_ref()],
        )? || proven_changed_regions.iter().any(|region| {
            region.old_span == relation.old_span && region.new_span == relation.new_span
        }) {
            continue;
        }
        let old_span = relation.old_span.clone();
        let new_span = relation.new_span.clone();
        let old_tokens = old_span
            .as_ref()
            .map(|span| span_tokens(sides[0], span))
            .transpose()?
            .unwrap_or_default();
        let new_tokens = new_span
            .as_ref()
            .map(|span| span_tokens(sides[1], span))
            .transpose()?
            .unwrap_or_default();
        if !assessor.charge(old_tokens.len().saturating_add(new_tokens.len())) {
            continue;
        }
        let mut counts = HashMap::<&ComparableToken, i64>::new();
        for token in &old_tokens {
            *counts.entry(token).or_default() += 1;
        }
        for token in &new_tokens {
            *counts.entry(token).or_default() -= 1;
        }
        if counts.values().all(|count| *count == 0) {
            continue;
        }
        proven_changed_regions.push(ProvenChangedRegion {
            old_span,
            new_span,
            confidence: super::Confidence::High,
            proof: if old_tokens.is_empty() || new_tokens.is_empty() {
                super::ChangedRegionProof::OneSidedNonEmptyRange
            } else {
                super::ChangedRegionProof::ExactTokenMultisetMismatch
            },
        });
    }
    // Deferred equal-fragment adoption: after every existing recovery, proof
    // and emission pass has run, spend only the budget they left over. The
    // pass re-checks the latest ownership and never rewrites a record, so an
    // exhausted budget leaves the remaining fragments pending.
    assessor.recover_equal_fragments(&mut ownership, &candidates, &proven_changed_regions)?;
    assessor.recover_suffix_translations(
        &mut ownership,
        &candidates,
        &proven_changed_regions,
        options.max_assessment_ranges,
    )?;
    // Retain the original ownership while existing review/coarse work uses
    // exactly the same snapshots. The last optional equality pass prepares
    // both updated partitions before publishing any new source ownership.
    let mut old_resolution = ownership[0].resolution(sides[0], options.max_assessment_ranges)?;
    let mut new_resolution = ownership[1].resolution(sides[1], options.max_assessment_ranges)?;
    let coverage = |partition: &[ResolutionRange], total| {
        super::coverage(
            partition
                .iter()
                .filter(|range| range.state != ResolutionState::Unresolved)
                .map(|range| range.comparable_range.end - range.comparable_range.start)
                .sum(),
            total,
        )
    };
    if candidate_groups(sides, &mut candidates, options.max_assessment_ranges)? {
        candidates.clear();
        candidates_truncated = true;
    }
    let mut comparison = Comparison {
        changes,
        change_candidates: candidates,
        proven_changed_regions,
        formatting_changes,
        unresolved_regions: Vec::new(),
        old_coverage: coverage(&old_resolution, sides[0].total_tokens),
        new_coverage: coverage(&new_resolution, sides[1].total_tokens),
        assessment: None,
    };
    // Optional claims use only the remaining shared budget, so they cannot
    // displace already completed localization or change emission.
    let reviewed = review::collect(&mut assessor, [&old_resolution, &new_resolution])?;
    // The exact-displacement proof is a property of the dependency path: every
    // relation that depends on a proven domain inherits the assumption, even
    // when its own spans are only a subspan of the proven domain. Parents
    // always precede their children, so one forward pass is transitive.
    for index in 0..assessor.records.len() {
        let Some(parent) = assessor.records[index].parent else {
            continue;
        };
        let inherited = assessor.records[parent]
            .assumptions
            .contains(&ComparisonAssumption::ExactTextDisplacement)
            && !assessor.records[index]
                .assumptions
                .contains(&ComparisonAssumption::ExactTextDisplacement);
        if inherited {
            assessor.records[index]
                .assumptions
                .push(ComparisonAssumption::ExactTextDisplacement);
        }
    }
    assessor.collect_mandatory_changed_regions_from_review(
        &reviewed.domains,
        reviewed.domains.capacity(),
        [&old_resolution, &new_resolution],
        &mut comparison.proven_changed_regions,
    )?;
    assessor.recover_mandatory_coarse_equalities(
        &reviewed.domains,
        reviewed.domains.capacity(),
        &mut ownership,
        [&mut old_resolution, &mut new_resolution],
        mandatory_equal::EqualConstraints {
            candidates: &comparison.change_candidates,
            regions: &comparison.proven_changed_regions,
            original: &proposed.unresolved_regions,
        },
    )?;
    comparison.old_coverage = coverage(&old_resolution, sides[0].total_tokens);
    comparison.new_coverage = coverage(&new_resolution, sides[1].total_tokens);
    comparison.unresolved_regions = unresolved_output(
        sides,
        alignment,
        [&old_resolution, &new_resolution],
        proposed.unresolved_regions,
        options.max_assessment_ranges,
    )?;
    let review_units = reviewed.units;
    drop(reviewed.domains);
    let assessment = ComparisonAssessment {
        policy_version: ASSESSMENT_POLICY_VERSION,
        relations: assessor.records,
        old_resolution,
        new_resolution,
        work_limit: options.max_assessment_work,
        work_used: options.max_assessment_work - assessor.remaining_work,
        work_by_stage: AssessmentWork {
            anchor_verification: options.max_assessment_work - after_anchors,
            local_views: after_initial_localization - after_views,
            localization: (after_anchors - after_initial_localization)
                + (after_views - after_localization),
            emission: after_localization - assessor.remaining_work,
        },
        anchor_work: assessor.anchor_work,
        local_view_work: assessor.local_view_work,
        suffix_reuse_work: assessor.suffix_reuse_work,
        candidates_truncated: candidates_truncated || assessor.output_stop.is_some(),
        localized_edits: assessor.localized_edits,
        review_units,
    };
    validation::validate(sides, &assessment, &comparison)?;
    comparison.assessment = Some(assessment);
    Ok(comparison)
}

/// The outcome of the proposal-path boundary proof.
enum BoundaryDisplacement {
    /// Every optimal script fixes the proposal's two boundary points with
    /// no crossing hunk, and the local line resolves uniquely.
    Proven(Box<BoundaryProof>),
    /// The all-path check or the local resolution did not prove a cut.
    NotProven,
    /// The rule is unavailable for this proposal.
    Unavailable,
    /// The shared budget ended before the proof finished.
    Budget,
    /// Bounded materialization was unavailable without exhausting work.
    Resource,
}

/// The independent boundary cut and the local exact-displacement proof.
struct BoundaryProof {
    key: DomainKey,
    edits: Vec<super::AtomicEdit>,
    events: Vec<ProjectedEvent>,
}

/// The outcome of one exact-displacement attempt.
enum ExactDisplacementStep {
    /// The unique maximum matching produced a charged edit witness and its
    /// stable event signature.
    Resolved(Vec<super::AtomicEdit>, Vec<ProjectedEvent>),
    /// The ordinary semantic path decides.
    Hold,
    /// The shared budget ended; the search is incomplete.
    Budget,
}

/// Child local key plus its localized group ranges.
type ForcedEqualChild = (DomainKey, Range<usize>, Range<usize>);

struct Assessor<'a, 'document> {
    sides: [&'a Side<'document>; 2],
    alignment: &'a Alignment,
    recovery: Option<SentenceRecoveryInput<'a>>,
    options: DiffOptions,
    /// Raw displacement sidecars for the exact-displacement rule.
    exact_displacement: Option<ExactDisplacementInput<'a>>,
    /// Glyph-to-record index per side, built once with the shared budget.
    exact_records: [HashMap<crate::model::GlyphId, usize>; 2],
    remaining_work: usize,
    anchors: Vec<(usize, usize)>,
    /// Every verified source pair outside the common maximum-path boundaries
    /// when the global spine is ambiguous, including nonmaximum competitors.
    anchor_alternatives: Vec<(usize, usize)>,
    anchor_work: AnchorWork,
    local_view_work: LocalViewWork,
    suffix_reuse_work: SuffixReuseWork,
    deny_token_cache: views::DenyTokenCache<'a, 'document>,
    domains: HashMap<DomainKey, DomainProof>,
    /// Relations established through the mandatory-matching equality proof,
    /// which need the wider overlap veto during semantic validation.
    forced_equal_relations: std::collections::HashSet<usize>,
    /// Mandatory matched pairs per closed domain, computed at most once per
    /// key. `None` records an unavailable analysis; callers keep the ordinary
    /// proof paths.
    mandatory_analyses:
        HashMap<DomainKey, Option<std::sync::Arc<semantic::MandatoryMatchAnalysis>>>,
    records: Vec<RelationAssessment>,
    output_stop: Option<usize>,
    root_relation: Option<usize>,
    /// One bounded diagnostic-only root scope shared by actually refused keys.
    materialization_stop: Option<usize>,
    optional_search_stop: Option<usize>,
    root_reasons: Vec<AssessmentReason>,
    semantic_acceptance: HashMap<usize, DomainKey>,
    local_domains: Vec<views::LocalDomain>,
    local_anchors: Vec<views::LocalDomain>,
    /// Spans of local domains discovered by the anchored rigid translation
    /// pass, recorded so the proven relation carries the move as an explicit
    /// assumption.
    anchored_translations: Vec<(TextSpan, TextSpan)>,
    /// Whole old/new spans proven stationary through an established
    /// neighbour correspondence, kept for report assumptions.
    stationary_members: Vec<(TextSpan, TextSpan)>,
    positioned_replacements: Vec<(TextSpan, TextSpan)>,
    raw_source_equalities: Vec<(TextSpan, TextSpan)>,
    /// Spans of local domains discovered by the bracketed-region pass,
    /// recorded so the proven relation carries the geometric boundaries as an
    /// explicit assumption.
    bracketed_domains: Vec<(TextSpan, TextSpan)>,
    /// Spans of closed lines whose edit location was resolved by the exact
    /// raw displacement rule.
    exact_displacements: Vec<(TextSpan, TextSpan)>,
    footer_domains: Vec<views::LocalDomain>,
    localized_edits: Vec<LocalizedEditScript>,
    localized_edit_count: usize,
    proposal_relations: HashMap<ProposalKey, usize>,
    semantic_rejections: HashSet<ProposalKey>,
    move_relations: HashMap<ProposalKey, Option<usize>>,
    /// Lazily created cache for the local source-issue veto.
    ///
    /// The discovery pass keeps its own cache; this one is created on the
    /// first local key that needs the veto and lives only as long as the
    /// assessor. A comparison that never evaluates such a key never pays for
    /// it.
    issue_cache: Option<SourceIssueCache<'a>>,
    /// Lazily created cache for the equal-fragment proof.
    ///
    /// It borrows the two immutable sides and stores only selection-independent
    /// evidence (the document-wide glyph sharing index, event structures and
    /// checked issue projections). A comparison that never proves a
    /// strict-closed equal domain never pays for it.
    equal_fragment_cache: Option<equal_fragment::EqualFragmentCache<'a>>,
    /// Relations of strict-closed equal fragments recorded by the local
    /// recovery for the deferred tail proof.
    ///
    /// The relation record already owns the span pair, so the deferred list
    /// stores only indices and adds no second variable-length source copy.
    /// Recording spends no proof budget and never pushes out an earlier
    /// recovery.
    equal_fragment_candidates: Vec<usize>,
    /// The comparison side that is proven empty, if any.
    ///
    /// A side is proven empty when the extraction produced no canonical
    /// tokens, no block issues and no extraction-gap evidence anywhere in the
    /// alignment. The value is fixed for one comparison and lets a one-sided
    /// proposal use its own extent as its closed domain and take the existing
    /// one-sided edit path instead of a Myers search it cannot match.
    empty_side: Option<usize>,
}

impl<'a, 'document> Assessor<'a, 'document> {
    fn validate_semantic_emission(
        &mut self,
        index: usize,
        changes: &[ChangeEvent],
    ) -> Result<bool> {
        let Some(key) = self.semantic_acceptance.get(&index).cloned() else {
            return Ok(true);
        };
        if self.domains[&key].scope != ProofScope::ExactKey {
            return Ok(false);
        }
        // A forced-equal claim needs the wider overlap veto: the contained
        // comparison below cannot see a change or move that only partially
        // overlaps the claim. Any overlapping occurrence keeps the claim
        // tentative so existing changed or moved ownership is never erased.
        if self.forced_equal_relations.contains(&index) {
            let relation_spans = [
                self.records[index].old_span.clone(),
                self.records[index].new_span.clone(),
            ];
            let mut accepted = [Vec::new(), Vec::new()];
            for (side, span) in relation_spans.iter().enumerate() {
                let Some(span) = span else {
                    continue;
                };
                if !self.charge(span.blocks.len()) {
                    self.records[index].outcome = RelationOutcome::Tentative;
                    self.records[index].search = SearchCompleteness::Incomplete;
                    self.records[index]
                        .reasons
                        .push(AssessmentReason::WorkLimit);
                    return Ok(false);
                }
                accepted[side] = project(self.sides[side], span)?;
            }
            for change in changes {
                for occurrence in &change.occurrences {
                    for (side, span) in [occurrence.old_span.as_ref(), occurrence.new_span.as_ref()]
                        .into_iter()
                        .enumerate()
                    {
                        let Some(span) = span else {
                            continue;
                        };
                        if !self.charge(span.blocks.len()) {
                            self.records[index].outcome = RelationOutcome::Tentative;
                            self.records[index].search = SearchCompleteness::Incomplete;
                            self.records[index]
                                .reasons
                                .push(AssessmentReason::WorkLimit);
                            return Ok(false);
                        }
                        let source = project(self.sides[side], span)?;
                        let Some(overlap) =
                            local::overlaps(&source, &accepted[side], &mut self.remaining_work)
                        else {
                            self.records[index].outcome = RelationOutcome::Tentative;
                            self.records[index].search = SearchCompleteness::Incomplete;
                            self.records[index]
                                .reasons
                                .push(AssessmentReason::WorkLimit);
                            return Ok(false);
                        };
                        if overlap {
                            self.records[index].outcome = RelationOutcome::Tentative;
                            self.records[index]
                                .reasons
                                .push(AssessmentReason::AmbiguousEditLocation);
                            return Ok(false);
                        }
                    }
                }
            }
        }
        let expected = self.domains[&key]
            .stable_events
            .clone()
            .expect("semantic proof has a signature");
        let old = self.records[index].old_span.clone();
        let new = self.records[index].new_span.clone();
        let context_work = old
            .as_ref()
            .map_or(0, |span| self.sides[0].source_token_count(&span.blocks))
            .saturating_add(
                new.as_ref()
                    .map_or(0, |span| self.sides[1].source_token_count(&span.blocks)),
            );
        let mut actual = Vec::new();
        for change in changes {
            if change.kind == ChangeKind::Move {
                continue;
            }
            let mut contained = Vec::new();
            for occurrence in &change.occurrences {
                // Every candidate occurrence visit is charged before the
                // structural check, so the repeated scan of the change list
                // stays inside the shared budget even when most occurrences
                // cannot belong to this relation. Containment requires the
                // occurrence's blocks to be a contiguous, in-order subsequence
                // of the relation's own blocks; a failure cannot enter the
                // contained set and skips the expensive canonicalization.
                let structural_work = old
                    .as_ref()
                    .map_or(1, |span| span.blocks.len())
                    .saturating_add(new.as_ref().map_or(1, |span| span.blocks.len()));
                if !self.charge(structural_work) {
                    self.records[index].outcome = RelationOutcome::Tentative;
                    self.records[index].search = SearchCompleteness::Incomplete;
                    self.records[index]
                        .reasons
                        .push(AssessmentReason::WorkLimit);
                    return Ok(false);
                }
                if !span_may_contain(old.as_ref(), occurrence.old_span.as_ref())
                    || !span_may_contain(new.as_ref(), occurrence.new_span.as_ref())
                {
                    continue;
                }
                let source_work = occurrence
                    .old_span
                    .as_ref()
                    .map_or(0, |span| self.sides[0].source_token_count(&span.blocks))
                    .saturating_add(
                        occurrence
                            .new_span
                            .as_ref()
                            .map_or(0, |span| self.sides[1].source_token_count(&span.blocks)),
                    );
                if !self.charge(context_work.saturating_add(source_work).saturating_mul(2)) {
                    self.records[index].outcome = RelationOutcome::Tentative;
                    self.records[index].search = SearchCompleteness::Incomplete;
                    self.records[index]
                        .reasons
                        .push(AssessmentReason::WorkLimit);
                    return Ok(false);
                }
                if contains_span(self.sides[0], old.as_ref(), occurrence.old_span.as_ref())?
                    && contains_span(self.sides[1], new.as_ref(), occurrence.new_span.as_ref())?
                {
                    contained.push(occurrence.clone());
                }
            }
            if !contained.is_empty() {
                actual.push(projected_event(
                    self.sides,
                    &ChangeEvent {
                        occurrences: contained,
                        ..change.clone()
                    },
                )?);
            }
        }
        if actual == expected {
            return Ok(true);
        }
        self.records[index].outcome = RelationOutcome::Tentative;
        self.records[index]
            .reasons
            .push(AssessmentReason::AmbiguousEditLocation);
        Ok(false)
    }

    fn assess_move(&mut self, proposal: &ProposedRelation) -> Result<Option<usize>> {
        let (Some(old), Some(new)) = (&proposal.old, &proposal.new) else {
            return Ok(None);
        };
        let tokens = span_tokens(self.sides[0], old)?;
        if tokens.is_empty() || tokens != span_tokens(self.sides[1], new)? {
            return Ok(None);
        }
        let extents = self.proposal_extents(proposal)?;
        let crossed = self
            .anchors
            .iter()
            .copied()
            .filter(|&(a, b)| {
                (extents[0].end <= a && extents[1].start > b)
                    || (extents[0].start > a && extents[1].end <= b)
            })
            .collect::<Vec<_>>();
        let mut isolated = None;
        for (a, b) in crossed {
            if !self.charge(self.alignment.spans.len().saturating_add(1)) {
                return Ok(None);
            }
            let key = DomainKey {
                local: None,
                old: extents[0].start.min(a)..extents[0].end.max(a + 1),
                new: extents[1].start.min(b)..extents[1].end.max(b + 1),
                old_separator: old.separator.unwrap_or(BlockSeparator::Space),
                new_separator: new.separator.unwrap_or(BlockSeparator::Space),
            };
            let (reasons, local) = self.domain_reasons(&key)?;
            if reasons.is_empty() {
                isolated = Some(local);
                break;
            }
        }
        let Some(isolated) = isolated else {
            return Ok(None);
        };
        let mut groups = Vec::new();
        for side in 0..2 {
            let group = domain_group(
                self.sides[side],
                0..self.sides[side].blocks.len(),
                BlockSeparator::Space,
            );
            if !self.charge(group.tokens.len().saturating_mul(tokens.len())) {
                return Ok(None);
            }
            if group
                .tokens
                .windows(tokens.len())
                .filter(|window| *window == tokens.as_slice())
                .count()
                != 1
            {
                return Ok(None);
            }
            groups.push(group);
        }
        let mut move_assumptions = assumptions([&groups[0], &groups[1]]);
        if isolated {
            move_assumptions.push(ComparisonAssumption::LocalEvidenceBoundaries);
        }
        self.record(RelationAssessment {
            old_span: Some(old.clone()),
            new_span: Some(new.clone()),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: move_assumptions,
            reasons: Vec::new(),
        })
        .map(Some)
    }
    fn record_optional_search_stop(&mut self, reason: AssessmentReason) -> Result<()> {
        let index = if let Some(index) = self.optional_search_stop {
            index
        } else {
            let root = self.root_relation()?;
            if self.records[root].outcome == RelationOutcome::Established {
                // Optional exploration is not a dependency of this completed
                // proof or its established children. Its stop is independent.
                let mut stopped = self.records[root].clone();
                stopped.parent = None;
                stopped.outcome = RelationOutcome::Tentative;
                stopped.search = SearchCompleteness::Incomplete;
                stopped.reasons = vec![reason];
                self.record(stopped)?
            } else {
                root
            }
        };
        self.optional_search_stop = Some(index);
        // Output-limit sentinels have an exact reason contract, including for
        // empty source ranges. Their existing incomplete status reports this.
        if self.output_stop == Some(index) {
            return Ok(());
        }
        self.records[index].search = SearchCompleteness::Incomplete;
        if !self.records[index].reasons.contains(&reason) {
            self.records[index].reasons.push(reason);
        }
        Ok(())
    }

    fn discover_local_domains(&mut self, proposals: &[ProposedRelation]) -> Result<()> {
        let available = self.remaining_work;
        // This schedules work, not evidence: optional exploration cannot spend
        // the entire shared budget before already accepted relations settle.
        let reserve = (self.options.max_assessment_work / 16).min(available);
        let cap = available - reserve;
        self.local_view_work.budget_cap = cap;
        self.local_view_work.settlement_reserve = reserve;
        self.remaining_work = cap;
        let result = self.discover_local_domains_bounded(proposals);
        let spent = cap
            .checked_sub(self.remaining_work)
            .ok_or_else(|| invalid("optional discovery increased its work budget"))?;
        self.local_view_work.cap_exhausted = self.remaining_work == 0;
        self.remaining_work = available
            .checked_sub(spent)
            .ok_or_else(|| invalid("optional discovery exceeded its shared budget"))?;
        result?;
        if self.local_view_work.cap_exhausted {
            self.record_optional_search_stop(AssessmentReason::WorkLimit)?;
        }
        Ok(())
    }

    fn discover_local_domains_bounded(&mut self, proposals: &[ProposedRelation]) -> Result<()> {
        if self.source_reasons().is_empty() {
            let before = self.remaining_work;
            self.discover_ordered_domains()?;
            self.local_view_work.domain_construction += before - self.remaining_work;
            return self.discover_footer_domains();
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let anchors = proposals
            .iter()
            .filter(|proposal| proposal.exact_recovery)
            .filter_map(|proposal| {
                Some((
                    proposal.old.as_ref()?.clone(),
                    proposal.new.as_ref()?.clone(),
                ))
            })
            .collect::<Vec<_>>();
        let before = self.remaining_work;
        let discovery = views::discover_recording(
            self.sides,
            recovery,
            &anchors,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
            &mut self.local_view_work,
            self.anchor_work.order_unique == Some(false) && !self.anchors.is_empty(),
        )?;
        self.local_view_work.domain_construction =
            before - self.remaining_work - self.local_view_work.total();
        self.local_domains = discovery.domains;
        self.local_anchors = discovery.anchors;
        self.discover_footer_domains()?;
        if self.remaining_work == 0 {
            // The root already retains reading-order uncertainty. Record the
            // unfinished optional search without changing established local
            // relations whose checks completed before this attempt.
            self.record_optional_search_stop(AssessmentReason::WorkLimit)?;
        }
        Ok(())
    }

    fn discover_footer_domains(&mut self) -> Result<()> {
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let before = self.remaining_work;
        let discovery = footers::discover(
            self.sides,
            recovery,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
        )?;
        self.local_view_work.footer_search += before - self.remaining_work;
        self.footer_domains = discovery.domains;
        if !discovery.complete {
            let reason = if discovery.work_limited || self.remaining_work == 0 {
                AssessmentReason::WorkLimit
            } else {
                AssessmentReason::SearchIncomplete
            };
            self.record_optional_search_stop(reason)?;
        }
        for domain in &self.footer_domains {
            if self.local_domains.len() == self.options.max_assessment_ranges {
                break;
            }
            if !self.local_domains.contains(domain) {
                self.local_domains.push(domain.clone());
            }
        }
        Ok(())
    }

    fn source_reasons(&self) -> Vec<AssessmentReason> {
        self.root_reasons.clone()
    }

    /// Returns whether one local side span carries a source issue.
    ///
    /// The per-comparison cache is created on first use and only when a local
    /// key actually needs the veto. When the bounded reservation cannot be
    /// paid the veto holds immediately instead of running an uncharged
    /// projection, and every budget failure keeps the veto rather than
    /// clearing it.
    fn local_span_has_source_issues(&mut self, side: usize, span: &TextSpan) -> Result<bool> {
        if self.issue_cache.is_none() {
            let sides = self.sides;
            let Some(cache) = SourceIssueCache::new(sides, &mut self.remaining_work)? else {
                // The shared budget is exhausted: no source issue may be
                // cleared from an unpayable reservation.
                return Ok(true);
            };
            self.issue_cache = Some(cache);
        }
        match &mut self.issue_cache {
            Some(cache) => {
                span_has_source_issues_cached(cache.side(side), span, &mut self.remaining_work)
            }
            None => Ok(true),
        }
    }

    fn domain_reasons(&mut self, key: &DomainKey) -> Result<(Vec<AssessmentReason>, bool)> {
        let mut reasons = self.source_reasons();
        if key.local.is_none() {
            match self.global_anchor_competition(key) {
                Some(true) => reasons.push(AssessmentReason::CompetingCorrespondence),
                None => reasons.push(AssessmentReason::WorkLimit),
                Some(false) => {}
            }
        }
        if key.local.is_some() {
            reasons.retain(|reason| {
                !matches!(
                    reason,
                    AssessmentReason::UnknownReadingOrder | AssessmentReason::InferredReadingOrder
                )
            });
        }
        let has_barrier = reasons.iter().any(|reason| {
            matches!(
                reason,
                AssessmentReason::ExtractionGap
                    | AssessmentReason::NormalizationUncertainty
                    | AssessmentReason::UnknownReadingOrder
                    | AssessmentReason::InferredReadingOrder
            )
        });
        if !has_barrier {
            return Ok((reasons, false));
        }
        // The paired raw-source proof is the only evidence that may lift the
        // normalization-issue barrier, and only for the exact whole-block pair
        // it proved. Extraction gaps and every other reason stay in force, and
        // no other span inherits the proof.
        let raw_proven = key.local.as_ref().is_some_and(|(old, new)| {
            self.raw_source_equalities
                .iter()
                .any(|(raw_old, raw_new)| raw_old == old && raw_new == new)
        });
        let local_issue = if raw_proven {
            false
        } else if let Some((old, new)) = &key.local {
            self.local_span_has_source_issues(0, old)?
                || self.local_span_has_source_issues(1, new)?
        } else {
            true
        };
        let intersects = |blocks: &[BlockId], side: usize| {
            if let Some((old, new)) = &key.local {
                let local = if side == 0 { old } else { new };
                return blocks.iter().any(|block| local.blocks.contains(block));
            }
            let range = if side == 0 { &key.old } else { &key.new };
            blocks
                .iter()
                .any(|block| range.contains(&self.sides[side].index[block]))
        };
        let touches_barrier = self.alignment.spans.iter().any(|span| {
            span.evidence.iter().any(|reason| {
                matches!(reason, AlignmentEvidence::ExtractionGap)
                    || (local_issue && *reason == AlignmentEvidence::NormalizationIssue)
                    || (key.local.is_none()
                        && matches!(
                            reason,
                            AlignmentEvidence::ReadingOrderUnknown
                                | AlignmentEvidence::ReadingOrderInferred
                        ))
            }) && (intersects(&span.old, 0) || intersects(&span.new, 1))
        }) || self.sides.iter().enumerate().any(|(side, source)| {
            source.blocks.iter().any(|block| {
                local_issue && !block.issues.is_empty() && intersects(&[block.block], side)
            })
        });
        if has_barrier && !touches_barrier {
            reasons.retain(|reason| {
                !matches!(
                    reason,
                    AssessmentReason::ExtractionGap
                        | AssessmentReason::NormalizationUncertainty
                        | AssessmentReason::UnknownReadingOrder
                        | AssessmentReason::InferredReadingOrder
                )
            });
            return Ok((reasons, true));
        }
        Ok((reasons, false))
    }

    /// A global window cannot choose between competing source pairs merely
    /// because a character LCS favors a longer block. Independent local
    /// closures carry their own source correspondence and use a local key.
    fn global_anchor_competition(&mut self, key: &DomainKey) -> Option<bool> {
        for index in 0..self.anchor_alternatives.len() {
            if !self.charge(1) {
                return None;
            }
            let (old, new) = self.anchor_alternatives[index];
            if key.old.contains(&old) || key.new.contains(&new) {
                return Some(true);
            }
        }
        Some(false)
    }

    fn inspect_source_reasons(&self) -> Vec<AssessmentReason> {
        let mut reasons = Vec::new();
        for span in &self.alignment.spans {
            for evidence in &span.evidence {
                let reason = match evidence {
                    AlignmentEvidence::ReadingOrderUnknown => {
                        Some(AssessmentReason::UnknownReadingOrder)
                    }
                    AlignmentEvidence::ReadingOrderInferred => {
                        Some(AssessmentReason::InferredReadingOrder)
                    }
                    AlignmentEvidence::ExtractionGap => Some(AssessmentReason::ExtractionGap),
                    AlignmentEvidence::NormalizationIssue => {
                        Some(AssessmentReason::NormalizationUncertainty)
                    }
                    _ => None,
                };
                if let Some(reason) = reason
                    && !reasons.contains(&reason)
                {
                    reasons.push(reason);
                }
            }
        }
        if self
            .sides
            .iter()
            .any(|side| side.blocks.iter().any(|block| !block.issues.is_empty()))
            && !reasons.contains(&AssessmentReason::NormalizationUncertainty)
        {
            reasons.push(AssessmentReason::NormalizationUncertainty);
        }
        if let Some(recovery) = self.recovery {
            let intervals = [
                recovery.old_trusted_run_intervals,
                recovery.new_trusted_run_intervals,
            ];
            let complete_run = |side: usize| {
                if self.sides[side].blocks.is_empty() {
                    return true;
                }
                let Some(first) = intervals[side].first().copied().flatten() else {
                    return false;
                };
                if first.start != 0 {
                    return false;
                }
                let evidence = if side == 0 {
                    recovery.old_trusted_run_evidence
                } else {
                    recovery.new_trusted_run_evidence
                };
                if let Some(evidence) = evidence {
                    let Some(descriptor) = evidence
                        .descriptors
                        .iter()
                        .find(|descriptor| descriptor.id == first.run_id)
                    else {
                        return false;
                    };
                    if !descriptor.block_indices.is_empty()
                        || !descriptor.trusted_block_indices.is_empty()
                    {
                        let complete = |indices: &[usize]| {
                            (0..self.sides[side].blocks.len()).eq(indices.iter().copied())
                        };
                        if descriptor.role.is_none()
                            || !complete(&descriptor.block_indices)
                            || !complete(&descriptor.trusted_block_indices)
                        {
                            return false;
                        }
                    }
                }
                let mut end = first.start;
                intervals[side].iter().all(|interval| {
                    interval.is_some_and(|interval| {
                        let contiguous = interval.run_id == first.run_id && interval.start == end;
                        end = interval.end;
                        contiguous
                    })
                })
            };
            // A run covering every extracted block supplies internal order
            // even when layout could not establish order between regions.
            if complete_run(0) && complete_run(1) {
                reasons.retain(|reason| {
                    !matches!(
                        reason,
                        AssessmentReason::UnknownReadingOrder
                            | AssessmentReason::InferredReadingOrder
                    )
                });
            }
        }
        reasons
    }

    fn proposal_extents(&self, proposal: &ProposedRelation) -> Result<[Range<usize>; 2]> {
        let mut extents = [0..0, 0..0];
        for (side_index, span) in [&proposal.old, &proposal.new].into_iter().enumerate() {
            if let Some(span) = span {
                extents[side_index] = block_extent(self.sides[side_index], span)?;
            } else {
                let index = proposal.span_indices[1 - side_index].unwrap_or(0);
                let position = self.alignment.spans[..index]
                    .iter()
                    .map(|span| {
                        if side_index == 0 {
                            span.old.len()
                        } else {
                            span.new.len()
                        }
                    })
                    .sum();
                extents[side_index] = position..position;
            }
        }
        Ok(extents)
    }

    fn domain_key(&self, proposal: &ProposedRelation) -> Result<DomainKey> {
        if proposal.old.is_some() || proposal.new.is_some() {
            let anchors = if proposal.exact_recovery {
                self.local_anchors.as_slice()
            } else {
                &[]
            };
            let local_key = |local: &views::LocalDomain| -> Result<DomainKey> {
                Ok(DomainKey {
                    local: Some((local.old_span.clone(), local.new_span.clone())),
                    old: block_extent(self.sides[0], &local.old_span)?,
                    new: block_extent(self.sides[1], &local.new_span)?,
                    old_separator: local.old_span.separator.unwrap_or(BlockSeparator::Space),
                    new_separator: local.new_span.separator.unwrap_or(BlockSeparator::Space),
                })
            };
            // Local emission already supplies a discovered domain verbatim.
            // Avoid rebuilding every other view's tokens to locate that key.
            if let Some(local) = anchors.iter().chain(&self.local_domains).find(|local| {
                proposal.old.as_ref() == Some(&local.old_span)
                    && proposal.new.as_ref() == Some(&local.new_span)
            }) {
                return local_key(local);
            }
            for local in anchors.iter().chain(&self.local_domains) {
                if contains_span(self.sides[0], Some(&local.old_span), proposal.old.as_ref())?
                    && contains_span(self.sides[1], Some(&local.new_span), proposal.new.as_ref())?
                {
                    return local_key(local);
                }
            }
        }
        // A proposal that the one-sided alignment already names as a whole
        // insertion or deletion is its own closed domain: the other side
        // carries no tokens, so there is no correspondence to bound it and
        // the anchor windows cannot narrow it. Recovery proposals that only
        // sit inside a wider unresolved window keep the ordinary domain.
        let present = self.empty_side.map(|empty| 1 - empty);
        if let Some(present) = present
            && proposal.span_indices[present]
                .and_then(|index| self.alignment.spans.get(index))
                .is_some_and(|span| {
                    matches!(
                        span.kind,
                        AlignmentKind::Insertion | AlignmentKind::Deletion
                    )
                })
        {
            let [old, new] = self.proposal_extents(proposal)?;
            let (old, new) = if self.empty_side == Some(0) {
                (0..0, new)
            } else {
                (old, 0..0)
            };
            return Ok(DomainKey {
                old_separator: domain_separator(
                    proposal.old.as_ref().and_then(|span| span.separator),
                    old.clone(),
                    old.clone(),
                ),
                new_separator: domain_separator(
                    proposal.new.as_ref().and_then(|span| span.separator),
                    new.clone(),
                    new.clone(),
                ),
                local: None,
                old,
                new,
            });
        }
        let [old, new] = self.proposal_extents(proposal)?;
        let mut starts = [0, 0];
        let mut ends = [self.sides[0].blocks.len(), self.sides[1].blocks.len()];
        for &(a, b) in &self.anchors {
            if old == (a..a + 1) && new == (b..b + 1) {
                starts = [a, b];
                ends = [a + 1, b + 1];
                break;
            }
            if a < old.start && b < new.start {
                starts = [a + 1, b + 1];
            }
            if a >= old.end && b >= new.end {
                ends = [a, b];
                break;
            }
        }
        Ok(DomainKey {
            local: None,
            old: starts[0]..ends[0],
            new: starts[1]..ends[1],
            old_separator: domain_separator(
                proposal.old.as_ref().and_then(|span| span.separator),
                old,
                starts[0]..ends[0],
            ),
            new_separator: domain_separator(
                proposal.new.as_ref().and_then(|span| span.separator),
                new,
                starts[1]..ends[1],
            ),
        })
    }

    /// Runs the bounded semantic uniqueness check over one group pair.
    ///
    /// The projected event signature is built by the shared signature
    /// callback. A unique outcome records the script and its signature, a
    /// budget failure reports an incomplete search, and an ambiguous or
    /// unproven result leaves the caller's state untouched. A one-sided group
    /// pair takes the existing one-sided edit path inside the check, so an
    /// empty side never pays for a Myers search it cannot match.
    fn semantic_witness(
        &mut self,
        old: &GroupText,
        new: &GroupText,
        unique: &mut bool,
        stable_events: &mut Option<Vec<ProjectedEvent>>,
        edits: &mut Vec<super::AtomicEdit>,
        search: &mut SearchCompleteness,
    ) -> Result<()> {
        self.semantic_witness_retained([old, new], unique, stable_events, edits, search, None)
    }

    fn semantic_witness_retained(
        &mut self,
        groups: [&GroupText; 2],
        unique: &mut bool,
        stable_events: &mut Option<Vec<ProjectedEvent>>,
        edits: &mut Vec<super::AtomicEdit>,
        search: &mut SearchCompleteness,
        retained: Option<exact::RetainedSuffix<'_, crate::normalize::ComparableToken>>,
    ) -> Result<()> {
        let [old, new] = groups;
        let sides = self.sides;
        let limit = self.options.max_assessment_ranges;
        match semantic::check_hunks_retained(
            &old.tokens,
            &new.tokens,
            &mut self.remaining_work,
            retained,
            &mut self.suffix_reuse_work,
            |edits, remaining| semantic_signature(sides, [old, new], edits, remaining, limit),
        ) {
            Ok(semantic::Outcome::Unique {
                signature,
                edits: witness,
            }) => {
                *unique = true;
                *stable_events = Some(signature);
                *edits = witness;
            }
            Ok(semantic::Outcome::Ambiguous) => {}
            Ok(semantic::Outcome::BudgetExceeded)
            | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                *search = SearchCompleteness::Incomplete;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn bounded_global_materialization(&self, key: &DomainKey) -> bool {
        key.local.is_none()
            && self.anchor_work.order_unique == Some(false)
            && !self.anchors.is_empty()
    }

    /// Creates the existing broad output sentinel before optional proof work
    /// when no ordinary relation can fit. Cached proofs may still be reused
    /// without calling this helper when no new record is requested.
    fn output_stop_at_capacity(&mut self) -> Result<Option<usize>> {
        if let Some(index) = self.output_stop {
            return Ok(Some(index));
        }
        if self.records.len() < self.options.max_assessment_ranges.saturating_sub(1) {
            return Ok(None);
        }
        // At this capacity, record replaces its argument with the established
        // broad sentinel shape; no placeholder scope enters the report.
        let index = self.record(RelationAssessment {
            old_span: None,
            new_span: None,
            parent: None,
            outcome: RelationOutcome::Tentative,
            search: SearchCompleteness::Incomplete,
            assumptions: Vec::new(),
            reasons: Vec::new(),
        })?;
        Ok(Some(index))
    }

    /// Central paid route for full global proof groups in the new scope.
    /// Legacy unique and local domains retain their existing construction.
    fn scoped_proof_groups(
        &mut self,
        key: &DomainKey,
    ) -> Result<std::result::Result<[GroupText; 2], MaterializationRefusal>> {
        if self
            .domains
            .get(key)
            .is_some_and(|proof| proof.scope != ProofScope::ExactKey)
        {
            return Ok(Err(MaterializationRefusal::Unavailable));
        }
        if self.bounded_global_materialization(key) {
            if self.output_stop.is_some() {
                return Ok(Err(MaterializationRefusal::Unavailable));
            }
            Ok(paid_global_groups(
                self.sides,
                key,
                &mut self.remaining_work,
                64 * 1024 * 1024,
            ))
        } else {
            Ok(Ok(proof_groups(self.sides, key)?))
        }
    }

    /// Refusal diagnostics copy only cached root identifiers, with a separate
    /// bounded allocation allowance. They spend no proof budget, inspect no
    /// source contents and cannot authorize localization or ownership.
    fn refuse_global_materialization(
        &mut self,
        key: &DomainKey,
        parent: usize,
        refusal: MaterializationRefusal,
    ) -> Result<DomainState> {
        if let Some(index) = self.output_stop {
            return Ok(DomainState::Stopped(index));
        }
        let relation = if let Some(index) = self.materialization_stop {
            if refusal == MaterializationRefusal::Work
                && !self.records[index]
                    .reasons
                    .contains(&AssessmentReason::WorkLimit)
            {
                self.records[index]
                    .reasons
                    .try_reserve(1)
                    .map_err(|_| allocation_error("materialization refusal reasons"))?;
                self.records[index]
                    .reasons
                    .push(AssessmentReason::WorkLimit);
            }
            index
        } else {
            let root = &self.records[parent];
            let identifiers = root
                .old_span
                .as_ref()
                .map_or(0, |span| span.blocks.len())
                .checked_add(root.new_span.as_ref().map_or(0, |span| span.blocks.len()))
                .and_then(|count| count.checked_mul(std::mem::size_of::<BlockId>()))
                .ok_or_else(|| allocation_error("materialization diagnostic scope"))?;
            let metadata_bytes = root
                .reasons
                .len()
                .checked_add(4)
                .and_then(|count| count.checked_mul(std::mem::size_of::<AssessmentReason>()))
                .and_then(|bytes| {
                    root.assumptions
                        .len()
                        .checked_mul(std::mem::size_of::<ComparisonAssumption>())
                        .and_then(|assumptions| bytes.checked_add(assumptions))
                })
                .and_then(|bytes| bytes.checked_add(identifiers))
                .ok_or_else(|| allocation_error("materialization diagnostic scope"))?;
            if metadata_bytes > 64 * 1024 * 1024 {
                return Err(allocation_error("materialization diagnostic scope"));
            }
            let copy_span = |span: &Option<TextSpan>| -> Result<Option<TextSpan>> {
                span.as_ref()
                    .map(|span| {
                        Ok(TextSpan {
                            blocks: super::try_copy_slice(&span.blocks).ok_or_else(|| {
                                allocation_error("materialization diagnostic blocks")
                            })?,
                            separator: span.separator,
                            canonical_range: span.canonical_range,
                            comparable_range: span.comparable_range,
                        })
                    })
                    .transpose()
            };
            let old_span = copy_span(&root.old_span)?;
            let new_span = copy_span(&root.new_span)?;
            let assumptions = super::try_copy_slice(&root.assumptions)
                .ok_or_else(|| allocation_error("materialization diagnostic assumptions"))?;
            let mut reasons = Vec::new();
            reasons
                .try_reserve_exact(root.reasons.len() + 4)
                .map_err(|_| allocation_error("materialization diagnostic reasons"))?;
            reasons.extend_from_slice(&root.reasons);
            // Every verified alternative is inside the whole-root diagnostic
            // scope. This is a genuine broad competition claim, not a claim
            // that each refused exact key contains a competing occurrence.
            if !self.anchor_alternatives.is_empty() {
                reasons.push(AssessmentReason::CompetingCorrespondence);
            }
            if refusal == MaterializationRefusal::Work {
                reasons.push(AssessmentReason::WorkLimit);
            }
            reasons.extend([
                AssessmentReason::DomainNotClosed,
                AssessmentReason::SearchIncomplete,
            ]);
            let index = self.record(RelationAssessment {
                old_span,
                new_span,
                parent: Some(parent),
                outcome: RelationOutcome::Tentative,
                search: SearchCompleteness::Incomplete,
                assumptions,
                reasons,
            })?;
            if self.output_stop.is_some() {
                return Ok(DomainState::Stopped(index));
            }
            self.materialization_stop = Some(index);
            index
        };
        let span_length = |span: &Option<TextSpan>| {
            span.as_ref().map_or(0, |span| {
                span.comparable_range.end - span.comparable_range.start
            })
        };
        let lengths = [
            span_length(&self.records[relation].old_span),
            span_length(&self.records[relation].new_span),
        ];
        let next_count = self
            .domains
            .len()
            .checked_add(1)
            .ok_or_else(|| allocation_error("materialization refusal aliases"))?;
        if next_count
            .checked_mul(std::mem::size_of::<(DomainKey, DomainProof)>() * 2)
            .is_none_or(|bytes| bytes > 64 * 1024 * 1024)
        {
            return Err(allocation_error("materialization refusal aliases"));
        }
        self.domains
            .try_reserve(1)
            .map_err(|_| allocation_error("materialization refusal aliases"))?;
        self.domains.insert(
            key.clone(),
            DomainProof {
                scope: ProofScope::BroadRootRefusal,
                relation,
                unique: false,
                strict_unique: false,
                search: SearchCompleteness::Incomplete,
                edits: Vec::new(),
                stable_events: None,
                lengths,
            },
        );
        Ok(DomainState::Ready)
    }

    fn prove_domain(&mut self, key: &DomainKey) -> Result<DomainState> {
        if let Some(index) = self.output_stop {
            return Ok(DomainState::Stopped(index));
        }
        if self.domains.contains_key(key) {
            return Ok(DomainState::Ready);
        }
        if self.bounded_global_materialization(key)
            && let Some(index) = self.output_stop_at_capacity()?
        {
            return Ok(DomainState::Stopped(index));
        }
        let parent = self.root_relation()?;
        if self.output_stop.is_some() {
            return Ok(DomainState::Stopped(parent));
        }
        let bounded = self.bounded_global_materialization(key);
        if bounded && let Some(index) = self.output_stop_at_capacity()? {
            return Ok(DomainState::Stopped(index));
        }
        let (groups, mut reasons, isolated) = if bounded {
            let (mut reasons, isolated) = self.domain_reasons(key)?;
            let metadata = (|| {
                let bytes = key
                    .old
                    .len()
                    .checked_add(key.new.len())
                    .and_then(|count| count.checked_mul(std::mem::size_of::<BlockId>()))
                    .ok_or(MaterializationRefusal::Memory)?;
                if bytes > 64 * 1024 * 1024 {
                    return Err(MaterializationRefusal::Memory);
                }
                Ok([
                    light_group_metadata(
                        self.sides[0],
                        key.old.clone(),
                        key.old_separator,
                        &mut self.remaining_work,
                        64 * 1024 * 1024,
                    )?,
                    light_group_metadata(
                        self.sides[1],
                        key.new.clone(),
                        key.new_separator,
                        &mut self.remaining_work,
                        64 * 1024 * 1024,
                    )?,
                ])
            })();
            let metadata = match metadata {
                Ok(metadata) => metadata,
                Err(refusal) => return self.refuse_global_materialization(key, parent, refusal),
            };
            if !reasons.is_empty() {
                let mut domain_assumptions = light_group_assumptions([&metadata[0], &metadata[1]])
                    .map_err(|_| allocation_error("lightweight domain assumptions"))?;
                if isolated {
                    domain_assumptions
                        .try_reserve(1)
                        .map_err(|_| allocation_error("lightweight domain assumptions"))?;
                    domain_assumptions.push(ComparisonAssumption::LocalEvidenceBoundaries);
                }
                let search = if reasons.contains(&AssessmentReason::WorkLimit) {
                    SearchCompleteness::Incomplete
                } else {
                    SearchCompleteness::Complete
                };
                reasons
                    .try_reserve(1)
                    .map_err(|_| allocation_error("lightweight domain reasons"))?;
                reasons.push(AssessmentReason::DomainNotClosed);
                let [old, new] = metadata;
                let lengths = [old.span.comparable_range.end, new.span.comparable_range.end];
                let relation = self.record(RelationAssessment {
                    old_span: (!old.span.blocks.is_empty()).then_some(old.span),
                    new_span: (!new.span.blocks.is_empty()).then_some(new.span),
                    parent: (!isolated).then_some(parent),
                    outcome: RelationOutcome::Tentative,
                    search,
                    assumptions: domain_assumptions,
                    reasons,
                })?;
                if self.output_stop.is_some() {
                    return Ok(DomainState::Stopped(relation));
                }
                self.domains
                    .try_reserve(1)
                    .map_err(|_| allocation_error("lightweight domains"))?;
                self.domains.insert(
                    key.clone(),
                    DomainProof {
                        scope: ProofScope::ExactKey,
                        relation,
                        unique: false,
                        strict_unique: false,
                        search,
                        edits: Vec::new(),
                        lengths,
                        stable_events: None,
                    },
                );
                return Ok(DomainState::Ready);
            }
            // Release the first identifier pair before fresh paid construction.
            drop(metadata);
            let groups = match paid_global_groups(
                self.sides,
                key,
                &mut self.remaining_work,
                64 * 1024 * 1024,
            ) {
                Ok(groups) => groups,
                Err(refusal) => return self.refuse_global_materialization(key, parent, refusal),
            };
            (groups, reasons, isolated)
        } else {
            let groups = proof_groups(self.sides, key)?;
            let (reasons, isolated) = self.domain_reasons(key)?;
            (groups, reasons, isolated)
        };
        let [old, new] = groups;
        let mut unique = false;
        let mut strict_unique = false;
        let mut stable_events = None;
        let mut edits = Vec::new();
        let closure_incomplete = reasons.contains(&AssessmentReason::WorkLimit);
        let mut search = if closure_incomplete {
            SearchCompleteness::Incomplete
        } else {
            SearchCompleteness::Complete
        };
        let mut exact_displacement_proof = false;
        if reasons.is_empty() {
            let exact = exact::check_retaining(&old.tokens, &new.tokens, &mut self.remaining_work);
            let mut retained = None;
            let exact = exact.map(|checked| {
                retained = checked.suffix;
                checked.outcome
            });
            match exact {
                Ok(exact::ExactUniqueness::Unique) => {
                    strict_unique = true;
                    if self.empty_side.is_some() && (old.tokens.is_empty() || new.tokens.is_empty())
                    {
                        // A one-sided domain has exactly one shortest script:
                        // the whole non-empty side is inserted or deleted. The
                        // existing one-sided path builds that script and its
                        // event signature directly, so the bounded Myers
                        // search and its quadratic charge are never spent on
                        // an empty side.
                        self.semantic_witness(
                            &old,
                            &new,
                            &mut unique,
                            &mut stable_events,
                            &mut edits,
                            &mut search,
                        )?;
                    } else {
                        // Charge the bounded reconstruction as well as the
                        // exhaustive uniqueness search before running Myers.
                        let bound = old
                            .tokens
                            .len()
                            .saturating_add(new.tokens.len())
                            .saturating_mul(
                                self.options
                                    .max_edit_distance
                                    .min(old.tokens.len().saturating_add(new.tokens.len()))
                                    .saturating_add(1),
                            );
                        if old.tokens == new.tokens {
                            unique = true;
                        } else if self.charge(bound) {
                            match myers::diff(
                                &old.tokens,
                                &new.tokens,
                                self.options.max_edit_distance,
                            ) {
                                Ok(Some(script)) => {
                                    unique = true;
                                    edits = script;
                                }
                                Ok(None)
                                | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                                    search = SearchCompleteness::Incomplete;
                                }
                                Err(error) => return Err(error),
                            }
                        } else {
                            search = SearchCompleteness::Incomplete;
                        }
                    }
                }
                Ok(exact::ExactUniqueness::Ambiguous) => {
                    // A whole-block displacement search can allocate its own
                    // bounded proof matrix. Release the retained table first
                    // so its original live-memory allowance never overlaps.
                    if self.exact_displacement.is_some()
                        && old.blocks.len() == 1
                        && new.blocks.len() == 1
                    {
                        drop(retained.take());
                    }
                    match self.try_exact_displacement(&old, &new)? {
                        ExactDisplacementStep::Resolved(witness, signature) => {
                            drop(retained.take());
                            unique = true;
                            stable_events = Some(signature);
                            edits = witness;
                            exact_displacement_proof = true;
                            self.exact_displacements.push((
                                old.span(0, old.tokens.len()),
                                new.span(0, new.tokens.len()),
                            ));
                        }
                        ExactDisplacementStep::Budget => {
                            drop(retained.take());
                            search = SearchCompleteness::Incomplete;
                        }
                        ExactDisplacementStep::Hold => {
                            self.semantic_witness_retained(
                                [&old, &new],
                                &mut unique,
                                &mut stable_events,
                                &mut edits,
                                &mut search,
                                retained.take(),
                            )?;
                        }
                    }
                }
                Ok(exact::ExactUniqueness::BudgetExceeded)
                | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                    search = SearchCompleteness::Incomplete;
                }
                Err(error) => return Err(error),
            }
        }
        // Domain correspondence and edit localization are separate claims.
        // A closed domain remains valid when its internal LCS is ambiguous.
        let closed = reasons.is_empty();
        if !closed {
            reasons.push(AssessmentReason::DomainNotClosed);
        }
        let mut domain_assumptions = if bounded {
            paid_group_assumptions([&old, &new])?
        } else {
            assumptions([&old, &new])
        };
        if isolated {
            domain_assumptions.push(ComparisonAssumption::LocalEvidenceBoundaries);
        }
        if key.local.as_ref().is_some_and(|(old, new)| {
            self.footer_domains
                .iter()
                .any(|domain| &domain.old_span == old && &domain.new_span == new)
        }) {
            domain_assumptions.push(ComparisonAssumption::CatalogFooterCorrespondence);
        }
        if key.local.as_ref().is_some_and(|(old, new)| {
            self.anchored_translations
                .iter()
                .any(|(anchored_old, anchored_new)| anchored_old == old && anchored_new == new)
        }) {
            domain_assumptions.push(ComparisonAssumption::RigidTranslation);
        }
        if key.local.as_ref().is_some_and(|(old, new)| {
            self.bracketed_domains
                .iter()
                .any(|(bracketed_old, bracketed_new)| bracketed_old == old && bracketed_new == new)
        }) {
            domain_assumptions.push(ComparisonAssumption::BracketedRegion);
        }
        if key.local.as_ref().is_some_and(|(old, new)| {
            self.raw_source_equalities
                .iter()
                .any(|(raw_old, raw_new)| raw_old == old && raw_new == new)
        }) {
            domain_assumptions.push(ComparisonAssumption::RawSourceEquality);
        }
        if key.local.as_ref().is_some_and(|(old, new)| {
            self.stationary_members
                .iter()
                .any(|(stationary_old, stationary_new)| {
                    stationary_old == old && stationary_new == new
                })
        }) {
            domain_assumptions.push(ComparisonAssumption::StationaryNeighbour);
        }
        if key.local.as_ref().is_some_and(|(old, new)| {
            self.positioned_replacements
                .iter()
                .any(|(replacement_old, replacement_new)| {
                    replacement_old == old && replacement_new == new
                })
        }) {
            domain_assumptions.push(ComparisonAssumption::PositionedReplacement);
        }
        if exact_displacement_proof
            || key.local.as_ref().is_some_and(|(old, new)| {
                self.exact_displacements
                    .iter()
                    .any(|(exact_old, exact_new)| exact_old == old && exact_new == new)
            })
        {
            domain_assumptions.push(ComparisonAssumption::ExactTextDisplacement);
        }
        let relation = self.record(RelationAssessment {
            old_span: if bounded {
                if old.blocks.is_empty() {
                    None
                } else {
                    Some(
                        old.try_span(0, old.tokens.len())
                            .ok_or_else(|| allocation_error("paid old domain span"))?,
                    )
                }
            } else {
                nonempty_span(&old)
            },
            new_span: if bounded {
                if new.blocks.is_empty() {
                    None
                } else {
                    Some(
                        new.try_span(0, new.tokens.len())
                            .ok_or_else(|| allocation_error("paid new domain span"))?,
                    )
                }
            } else {
                nonempty_span(&new)
            },
            parent: (!isolated && key.local.is_none()).then_some(parent),
            outcome: if closed {
                RelationOutcome::Established
            } else {
                RelationOutcome::Tentative
            },
            search: if closure_incomplete {
                SearchCompleteness::Incomplete
            } else {
                SearchCompleteness::Complete
            },
            assumptions: domain_assumptions,
            reasons,
        })?;
        if self.output_stop.is_some() {
            return Ok(DomainState::Stopped(relation));
        }
        if search == SearchCompleteness::Incomplete {
            // The domain boundary remains established; dependent localization
            // records carry the incomplete-search reason.
            unique = false;
        }
        if bounded {
            self.domains
                .try_reserve(1)
                .map_err(|_| allocation_error("paid domains"))?;
        }
        self.domains.insert(
            key.clone(),
            DomainProof {
                scope: ProofScope::ExactKey,
                relation,
                unique,
                strict_unique,
                stable_events,
                search,
                edits,
                lengths: [old.tokens.len(), new.tokens.len()],
            },
        );
        Ok(DomainState::Ready)
    }

    /// Attempts the exact raw displacement resolution for one closed whole
    /// source-bound line.
    ///
    /// The projection and every search and arithmetic step are charged to the
    /// shared budget. A missing sidecar, a non-line domain, an ambiguous or
    /// duplicated projection and every hold reason fall back to the ordinary
    /// semantic path; an exhausted budget reports incomplete.
    fn try_exact_displacement(
        &mut self,
        old: &GroupText,
        new: &GroupText,
    ) -> Result<ExactDisplacementStep> {
        let Some(input) = self.exact_displacement else {
            return Ok(ExactDisplacementStep::Hold);
        };
        if old.blocks.len() != 1 || new.blocks.len() != 1 {
            return Ok(ExactDisplacementStep::Hold);
        }
        let (Some(&old_block), Some(&new_block)) = (old.blocks.first(), new.blocks.first()) else {
            return Ok(ExactDisplacementStep::Hold);
        };
        let Some(&old_index) = self.sides[0].index.get(&old_block) else {
            return Ok(ExactDisplacementStep::Hold);
        };
        let Some(&new_index) = self.sides[1].index.get(&new_block) else {
            return Ok(ExactDisplacementStep::Hold);
        };
        if old.tokens.len() != self.sides[0].canonical[old_index].len()
            || new.tokens.len() != self.sides[1].canonical[new_index].len()
        {
            return Ok(ExactDisplacementStep::Hold);
        }
        let Some(old_evidence) = self.line_evidence(0, old_index, input.old, old.tokens.len())?
        else {
            return Ok(if self.remaining_work == 0 {
                ExactDisplacementStep::Budget
            } else {
                ExactDisplacementStep::Hold
            });
        };
        let Some(new_evidence) = self.line_evidence(1, new_index, input.new, new.tokens.len())?
        else {
            return Ok(if self.remaining_work == 0 {
                ExactDisplacementStep::Budget
            } else {
                ExactDisplacementStep::Hold
            });
        };
        match exact_displacement::resolve(
            &old.tokens,
            &new.tokens,
            &old_evidence,
            &new_evidence,
            &mut self.remaining_work,
        ) {
            exact_displacement::Resolution::Unique(matching) => {
                let Some(witness) = exact_displacement::edits_from_matching(
                    &matching,
                    old.tokens.len(),
                    new.tokens.len(),
                    self.options.max_edit_distance,
                    &mut self.remaining_work,
                ) else {
                    return Ok(if self.remaining_work == 0 {
                        ExactDisplacementStep::Budget
                    } else {
                        ExactDisplacementStep::Hold
                    });
                };
                // Reuse the ordinary signature processing so the witness keeps
                // the same invariants as the semantic path.
                let sides = self.sides;
                let limit = self.options.max_assessment_ranges;
                match semantic_signature(
                    sides,
                    [old, new],
                    &witness,
                    &mut self.remaining_work,
                    limit,
                )? {
                    Some(signature) => Ok(ExactDisplacementStep::Resolved(witness, signature)),
                    None if self.remaining_work == 0 => Ok(ExactDisplacementStep::Budget),
                    None => Ok(ExactDisplacementStep::Hold),
                }
            }
            exact_displacement::Resolution::Hold(exact_displacement::HoldReason::Budget) => {
                Ok(ExactDisplacementStep::Budget)
            }
            exact_displacement::Resolution::Hold(_) => Ok(ExactDisplacementStep::Hold),
        }
    }

    /// Projects the raw sidecar onto the canonical tokens of one block.
    ///
    /// Only a scalar token whose source is exactly one glyph used by no other
    /// token gets a record; a synthesized separator, an unmapped token, a
    /// multi-glyph source, a shared glyph and a missing record stay without
    /// evidence. `None` reports a projection failure or an exhausted budget.
    fn line_evidence<'b>(
        &mut self,
        side: usize,
        block_index: usize,
        records: &'b [crate::model::GlyphDisplacement],
        token_count: usize,
    ) -> Result<Option<Vec<Option<&'b crate::model::GlyphDisplacement>>>> {
        if token_count > 512 {
            return Ok(None);
        }
        let block = &self.sides[side].blocks[block_index];
        // Charge the shared upper bound before any source is cloned.
        if !self.charge(crate::normalize::token_source_upper_bound(block)?) {
            return Ok(None);
        }
        let Ok(sources) = block.canonical.comparable_tokens_with_sources() else {
            return Ok(None);
        };
        if sources.len() != token_count {
            return Ok(None);
        }
        // Every glyph source atom counts, so a glyph shared with a multi-source
        // or unmapped token cannot prove a scalar token either.
        let mut usage = HashMap::<crate::model::GlyphId, usize>::new();
        let mut glyphs = Vec::with_capacity(sources.len());
        for (token, source) in sources {
            for atom in &source.atoms {
                if let crate::normalize::TextSourceAtom::Glyph(glyph) = atom {
                    *usage.entry(*glyph).or_default() += 1;
                }
            }
            let glyph = match token {
                crate::normalize::ComparableToken::Scalar(_) => {
                    let mut atoms = source.atoms.iter();
                    match (atoms.next(), atoms.next()) {
                        (Some(crate::normalize::TextSourceAtom::Glyph(glyph)), None) => {
                            Some(*glyph)
                        }
                        _ => None,
                    }
                }
                crate::normalize::ComparableToken::Unmapped { .. } => None,
            };
            glyphs.push(glyph);
        }
        if !self.charge(glyphs.len().saturating_mul(2)) {
            return Ok(None);
        }
        let mapped = glyphs
            .into_iter()
            .map(|glyph| {
                glyph
                    .filter(|glyph| usage.get(glyph).copied() == Some(1))
                    .and_then(|glyph| self.exact_records[side].get(&glyph).copied())
                    .map(|index| &records[index])
                    .filter(|record| block.pages.is_empty() || block.pages.contains(&record.page.0))
            })
            .collect();
        Ok(Some(mapped))
    }

    /// Checks whether the strict changed hunks inside one proposal are the
    /// same on every optimal edit path of its closed domain.
    ///
    /// Only a closed domain with a completed search reaches this check. Every
    /// path must localize the proposal to the same ranges and contain the same
    /// changed hunks fully inside those ranges; a boundary-crossing hunk, a
    /// missing range or no changed hunk at all is absent, and an all-absent
    /// domain never counts as a proof.
    ///
    /// Work exhaustion is reported separately from a negative answer so the
    /// caller can record an incomplete child relation instead of presenting a
    /// completed search that never finished.
    fn proposal_edits_are_invariant(
        &mut self,
        proposal: &ProposedRelation,
        key: &DomainKey,
    ) -> Result<ProposalProof> {
        let [old, new] = match self.scoped_proof_groups(key)? {
            Ok(groups) => groups,
            Err(MaterializationRefusal::Work) => return Ok(ProposalProof::Exhausted),
            Err(_) => return Ok(ProposalProof::Resource),
        };
        let lengths = [old.tokens.len(), new.tokens.len()];
        if self.witnessed_impossible_cut(key, proposal, [&old, &new])? {
            return Ok(ProposalProof::NotInvariant);
        }
        let token_work = lengths[0].saturating_add(lengths[1]);
        let sides = self.sides;
        if !charge(&mut self.remaining_work, token_work) {
            return Ok(ProposalProof::Exhausted);
        }
        let groups = [&old, &new];
        let target = [proposal.old.as_ref(), proposal.new.as_ref()];
        let outcome = semantic::check_hunks(
            &old.tokens,
            &new.tokens,
            &mut self.remaining_work,
            |edits, remaining| {
                if !charge(remaining, token_work.saturating_add(edits.len())) {
                    return Ok(None);
                }
                let ranges = localize_proposal(sides, target, groups, edits)?;
                let [Some(old_range), Some(new_range)] = ranges else {
                    return Ok(Some(None));
                };
                if !point_on_script([old_range.start, new_range.start], edits, lengths)
                    || !point_on_script([old_range.end, new_range.end], edits, lengths)
                {
                    return Ok(Some(None));
                }
                Ok(Some(target_hunk_signature(&old_range, &new_range, edits)))
            },
        )?;
        Ok(match outcome {
            semantic::Outcome::Unique {
                signature: Some(_), ..
            } => ProposalProof::Invariant,
            semantic::Outcome::Unique { .. } | semantic::Outcome::Ambiguous => {
                ProposalProof::NotInvariant
            }
            semantic::Outcome::BudgetExceeded => ProposalProof::Exhausted,
        })
    }

    /// Proves one whole single-block line independently of its parent domain.
    ///
    /// A multi-block parent domain may stay ambiguous while a child line is
    /// still pinned by every optimal script: when all maximum equal-token
    /// matchings localize the proposal to the same two boundary points and no
    /// changed hunk crosses those points, the line correspondence is fixed by
    /// the parent's boundary evidence alone. Only a unanimous `true` across
    /// every optimal path counts; a single dissenting path, a mixed signature
    /// or an exhausted search leaves the line unresolved. The local line then
    /// resolves through the exact-displacement rule, which is sound because
    /// the fixed cut makes its maximum matching local to the line.
    fn boundary_displacement_proof(
        &mut self,
        proposal: &ProposedRelation,
        key: &DomainKey,
    ) -> Result<BoundaryDisplacement> {
        if self.exact_displacement.is_none() {
            return Ok(BoundaryDisplacement::Unavailable);
        }
        let (Some(old_span), Some(new_span)) = (proposal.old.as_ref(), proposal.new.as_ref())
        else {
            return Ok(BoundaryDisplacement::Unavailable);
        };
        let old_extent = block_extent(self.sides[0], old_span)?;
        let new_extent = block_extent(self.sides[1], new_span)?;
        if old_extent.len() != 1 || new_extent.len() != 1 {
            return Ok(BoundaryDisplacement::Unavailable);
        }
        let groups = match self.scoped_proof_groups(key)? {
            Ok(groups) => groups,
            Err(MaterializationRefusal::Work) => return Ok(BoundaryDisplacement::Budget),
            Err(_) => return Ok(BoundaryDisplacement::Resource),
        };
        let lengths = [groups[0].tokens.len(), groups[1].tokens.len()];
        let token_work = lengths[0].saturating_add(lengths[1]);
        let sides = self.sides;
        if !charge(&mut self.remaining_work, token_work) {
            return Ok(BoundaryDisplacement::Budget);
        }
        let mut exhausted = false;
        if self.witnessed_impossible_cut(key, proposal, [&groups[0], &groups[1]])? {
            return Ok(BoundaryDisplacement::NotProven);
        }
        let target = [proposal.old.as_ref(), proposal.new.as_ref()];
        let outcome = semantic::check_hunks(
            &groups[0].tokens,
            &groups[1].tokens,
            &mut self.remaining_work,
            |edits, remaining| {
                if !charge(remaining, token_work.saturating_add(edits.len())) {
                    return Ok(None);
                }
                let ranges = localize_proposal(sides, target, [&groups[0], &groups[1]], edits)?;
                let [Some(old_range), Some(new_range)] = ranges else {
                    return Ok(Some(None));
                };
                if !point_on_script([old_range.start, new_range.start], edits, lengths)
                    || !point_on_script([old_range.end, new_range.end], edits, lengths)
                {
                    return Ok(Some(None));
                }
                // The signature is the cut itself: every optimal path must
                // agree on the same two boundary coordinates, while the
                // internal hunk positions stay free.
                Ok(Some(
                    target_hunk_signature(&old_range, &new_range, edits)
                        .is_some()
                        .then_some((
                            (old_range.start, new_range.start),
                            (old_range.end, new_range.end),
                        )),
                ))
            },
        )?;
        let cut = match outcome {
            semantic::Outcome::Unique {
                signature: Some(cut),
                ..
            } => cut,
            semantic::Outcome::BudgetExceeded => {
                exhausted = true;
                ((0, 0), (0, 0))
            }
            semantic::Outcome::Unique {
                signature: None, ..
            }
            | semantic::Outcome::Ambiguous => {
                return Ok(BoundaryDisplacement::NotProven);
            }
        };
        if exhausted || self.remaining_work == 0 {
            return Ok(BoundaryDisplacement::Budget);
        }
        let ((old_start, new_start), (old_end, new_end)) = cut;
        if old_start >= old_end || new_start >= new_end {
            return Ok(BoundaryDisplacement::NotProven);
        }
        let local_key = DomainKey {
            local: Some((old_span.clone(), new_span.clone())),
            old: old_extent,
            new: new_extent,
            old_separator: old_span.separator.unwrap_or(BlockSeparator::Space),
            new_separator: new_span.separator.unwrap_or(BlockSeparator::Space),
        };
        let [old, new] = proof_groups(self.sides, &local_key)?;
        match self.try_exact_displacement(&old, &new)? {
            ExactDisplacementStep::Resolved(edits, events) => {
                Ok(BoundaryDisplacement::Proven(Box::new(BoundaryProof {
                    key: local_key,
                    edits,
                    events,
                })))
            }
            ExactDisplacementStep::Hold => Ok(BoundaryDisplacement::NotProven),
            ExactDisplacementStep::Budget => Ok(BoundaryDisplacement::Budget),
        }
    }

    fn assess(&mut self, proposal: &ProposedRelation) -> Result<usize> {
        if let Some(index) = self.output_stop {
            return Ok(index);
        }
        let key = self.domain_key(proposal)?;
        if self.bounded_global_materialization(&key)
            && let Some(index) = self.output_stop_at_capacity()?
        {
            return Ok(index);
        }
        if let DomainState::Stopped(index) = self.prove_domain(&key)? {
            return Ok(index);
        }
        let parent = self.domains[&key].relation;

        let exact_scope = self.domains[&key].scope == ProofScope::ExactKey;

        let proof_unique = self.domains[&key].unique;
        let proof_search = self.domains[&key].search;
        let proof_strict_unique = self.domains[&key].strict_unique;
        let mut reasons = self.records[parent].reasons.clone();
        let mut search = if proof_search == SearchCompleteness::Incomplete
            || reasons.contains(&AssessmentReason::WorkLimit)
        {
            SearchCompleteness::Incomplete
        } else {
            SearchCompleteness::Complete
        };
        // The optional mandatory-match proof only applies to the intended
        // ambiguous whole multiblock domain with an empty-reason, completed
        // parent search; it cannot supply missing order, normalization or
        // closure premises of its own.
        let forced_equal_eligible = exact_scope
            && reasons.is_empty()
            && !proof_unique
            && key.local.is_none()
            && key.old.len() > 1
            && key.new.len() > 1
            && proof_search == SearchCompleteness::Complete;
        let mut forced_equal_refusal = None;
        let forced_equal = if forced_equal_eligible {
            self.forced_equal_child(&key, proposal, &mut forced_equal_refusal)?
        } else {
            None
        };
        let will_stop = self.output_stop.is_some()
            || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1);
        let forced_equal_applies = forced_equal.is_some() && !will_stop;
        let mut targeted_invariant = false;
        let mut exact_local_proof = None;
        let mut boundary_budget = false;
        let mut boundary_resource = false;
        if exact_scope
            && reasons.is_empty()
            && !proof_unique
            && key.local.is_none()
            && key.old.len() > 1
            && key.new.len() > 1
            && proof_search == SearchCompleteness::Complete
            && !forced_equal_applies
        {
            match self.boundary_displacement_proof(proposal, &key)? {
                BoundaryDisplacement::Proven(proof) => {
                    targeted_invariant = true;
                    exact_local_proof = Some((proof.key, proof.edits, proof.events));
                }
                BoundaryDisplacement::Budget => boundary_budget = true,
                BoundaryDisplacement::Resource => boundary_resource = true,
                BoundaryDisplacement::NotProven | BoundaryDisplacement::Unavailable => {}
            }
        }
        if boundary_budget {
            reasons.push(AssessmentReason::WorkLimit);
            search = SearchCompleteness::Incomplete;
        }
        if boundary_resource {
            reasons.push(AssessmentReason::SearchIncomplete);
            search = SearchCompleteness::Incomplete;
        }
        if reasons.is_empty() && !proof_unique && !targeted_invariant && !forced_equal_applies {
            // A specific proposal may still be identical on every optimal path
            // of an otherwise ambiguous domain; only then is it established.
            //
            // The proof is limited to whole-view domains spanning more than one
            // block because those domains close a region whose block
            // correspondence is already carried by alignment evidence, so a
            // proposal's own strict hunks can be proven while the remaining
            // blocks keep their candidates. Single-block domains are semantic
            // event units whose atomic ranges stay coupled until the event's
            // script is unique, and local sentence domains stay coupled to the
            // veto obligations of their enclosing uncertain region.
            let multi_block = key.local.is_none() && key.old.len() > 1 && key.new.len() > 1;
            if multi_block && proof_search == SearchCompleteness::Complete {
                match self.proposal_edits_are_invariant(proposal, &key)? {
                    ProposalProof::Invariant => targeted_invariant = true,
                    ProposalProof::NotInvariant => {
                        reasons.push(AssessmentReason::AmbiguousEditLocation);
                        search = SearchCompleteness::Complete;
                    }
                    ProposalProof::Exhausted => {
                        // The domain's own proof stays complete; only this
                        // proposal's targeted search ran out of work.
                        reasons.push(AssessmentReason::WorkLimit);
                        search = SearchCompleteness::Incomplete;
                    }
                    ProposalProof::Resource => {
                        reasons.push(AssessmentReason::SearchIncomplete);
                        search = SearchCompleteness::Incomplete;
                    }
                }
            } else {
                reasons.push(if proof_search == SearchCompleteness::Incomplete {
                    AssessmentReason::WorkLimit
                } else {
                    AssessmentReason::AmbiguousEditLocation
                });
                search = proof_search;
            }
        }
        if let Some(refusal) = forced_equal_refusal
            && !targeted_invariant
            && !forced_equal_applies
        {
            let reason = if refusal == MaterializationRefusal::Work {
                AssessmentReason::WorkLimit
            } else {
                AssessmentReason::SearchIncomplete
            };
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
            search = SearchCompleteness::Incomplete;
        }
        if reasons.is_empty()
            && !targeted_invariant
            && !forced_equal_applies
            && !proof_strict_unique
            && (proposal.old != self.records[parent].old_span
                || proposal.new != self.records[parent].new_span)
        {
            reasons.push(AssessmentReason::AmbiguousEditLocation);
        }
        if reasons.is_empty() && !targeted_invariant && !forced_equal_applies {
            // The per-path proof already established localization and hunk
            // containment on every optimal script; the single representative
            // script is empty for ambiguous domains and cannot add evidence.
            match self.scoped_proof_groups(&key)? {
                Err(refusal) => {
                    reasons.push(if refusal == MaterializationRefusal::Work {
                        AssessmentReason::WorkLimit
                    } else {
                        AssessmentReason::SearchIncomplete
                    });
                    search = SearchCompleteness::Incomplete;
                }
                Ok(groups) => {
                    let proof = &self.domains[&key];
                    let ranges = localize_proposal(
                        self.sides,
                        [proposal.old.as_ref(), proposal.new.as_ref()],
                        [&groups[0], &groups[1]],
                        &proof.edits,
                    )?;
                    match ranges {
                        [Some(old), Some(new)]
                            if proof.exact_lengths().is_some_and(|lengths| {
                                point_on_script([old.start, new.start], &proof.edits, lengths)
                                    && point_on_script([old.end, new.end], &proof.edits, lengths)
                            }) => {}
                        _ => reasons.push(AssessmentReason::CompetingCorrespondence),
                    }
                }
            }
        }
        let local_proof_applies = exact_local_proof.is_some() && !will_stop;
        let semantic_proof = exact_scope
            && !local_proof_applies
            && reasons.is_empty()
            && self.domains[&key].stable_events.is_some();
        let mut assumptions = self.records[parent].assumptions.clone();
        if local_proof_applies {
            assumptions.push(ComparisonAssumption::ExactTextDisplacement);
        }
        if forced_equal_applies {
            assumptions.push(ComparisonAssumption::MandatoryMatchingEquality);
        }
        let index = self.record(RelationAssessment {
            old_span: proposal.old.clone(),
            new_span: proposal.new.clone(),
            parent: Some(parent),
            outcome: if reasons.is_empty() {
                RelationOutcome::Established
            } else {
                RelationOutcome::Tentative
            },
            search,
            assumptions,
            reasons,
        })?;
        if let Some((local_key, edits, events)) = exact_local_proof
            && local_proof_applies
        {
            // The local proof is a separate domain record so dependent
            // relations can trace the boundary evidence instead of the
            // ambiguous parent script.
            self.domains.insert(
                local_key.clone(),
                DomainProof {
                    scope: ProofScope::ExactKey,
                    relation: index,
                    unique: true,
                    search: SearchCompleteness::Complete,
                    edits,
                    lengths: [
                        self.records[index].old_span.as_ref().map_or(0, |span| {
                            span.comparable_range.end - span.comparable_range.start
                        }),
                        self.records[index].new_span.as_ref().map_or(0, |span| {
                            span.comparable_range.end - span.comparable_range.start
                        }),
                    ],
                    strict_unique: false,
                    stable_events: Some(events),
                },
            );
            self.semantic_acceptance.insert(index, local_key);
        } else if semantic_proof {
            self.semantic_acceptance.insert(index, key);
        }
        if let Some((local_key, _old_range, _new_range)) =
            forced_equal.filter(|_| forced_equal_applies)
        {
            // Register the fixed equal correspondence with an empty event
            // signature: validate_semantic_emission still vetoes any contained
            // non-move change that overlaps this claim.
            let lengths = local_key.local.as_ref().map_or([0, 0], |(old, new)| {
                [
                    old.comparable_range.end - old.comparable_range.start,
                    new.comparable_range.end - new.comparable_range.start,
                ]
            });
            self.domains.insert(
                local_key.clone(),
                DomainProof {
                    scope: ProofScope::ExactKey,
                    relation: index,
                    unique: true,
                    search: SearchCompleteness::Complete,
                    edits: Vec::new(),
                    lengths,
                    strict_unique: false,
                    stable_events: Some(Vec::new()),
                },
            );
            self.semantic_acceptance.insert(index, local_key);
            self.forced_equal_relations.insert(index);
        }
        Ok(index)
    }

    /// Decisive negative for a fixed boundary cut, cache-only on the miss path.
    ///
    /// A mandatory equal pair crossing either cut point means no optimal path
    /// passes through that point, so the boundary and invariant proofs cannot
    /// succeed. Locating the fixed cut ranges is itself optional work and runs
    /// only when both proposal spans exist and the domain analysis is already
    /// cached; its group-build and query work is preflighted and charged before
    /// it happens, and an unaffordable call leaves the shared remainder
    /// untouched so the ordinary proof path runs unchanged. Each cut query is
    /// one `partition_point` search, so the query charge is a conservative
    /// `2 * usize::BITS` comparisons for the two cut points. Real
    /// localization errors propagate unchanged.
    ///
    /// # Errors
    ///
    /// Returns the localization errors from [`locate_in_group`].
    fn witnessed_impossible_cut(
        &mut self,
        key: &DomainKey,
        proposal: &ProposedRelation,
        groups: [&GroupText; 2],
    ) -> Result<bool> {
        let (Some(old_span), Some(new_span)) = (proposal.old.as_ref(), proposal.new.as_ref())
        else {
            return Ok(false);
        };
        let Some(Some(analysis)) = self.mandatory_analyses.get(key) else {
            return Ok(false);
        };
        // `locate_in_group` scans the parent group's block list and rebuilds
        // prefix and child groups, so the bound counts parent blocks (including
        // empty-token blocks) and parent tokens plus the child blocks.
        let build_work = groups[0]
            .blocks
            .len()
            .saturating_add(groups[1].blocks.len())
            .saturating_add(groups[0].tokens.len())
            .saturating_add(groups[1].tokens.len())
            .saturating_add(old_span.blocks.len())
            .saturating_add(new_span.blocks.len());
        let query_work = usize::BITS as usize * 2;
        let work = build_work.saturating_add(query_work);
        if self.remaining_work < work {
            return Ok(false);
        }
        charge(&mut self.remaining_work, work);
        let Some(old_range) = locate_in_group(self.sides[0], Some(old_span), groups[0])? else {
            return Ok(false);
        };
        let Some(new_range) = locate_in_group(self.sides[1], Some(new_span), groups[1])? else {
            return Ok(false);
        };
        Ok(analysis
            .crossing_witness(old_range.start, new_range.start)
            .is_some()
            || analysis
                .crossing_witness(old_range.end, new_range.end)
                .is_some())
    }

    /// Detects a localized child whose tokens are all mandatory matched pairs
    /// of the parent domain, so its equal correspondence is fixed on every
    /// optimal path while the parent edit location stays ambiguous.
    ///
    /// Returns the child local key and localized ranges. The optional analysis
    /// never turns a resource failure into a fatal error and never erases the
    /// shared remainder when it is unaffordable.
    fn forced_equal_child(
        &mut self,
        key: &DomainKey,
        proposal: &ProposedRelation,
        materialization_refusal: &mut Option<MaterializationRefusal>,
    ) -> Result<Option<ForcedEqualChild>> {
        let (Some(old_span), Some(new_span)) = (proposal.old.as_ref(), proposal.new.as_ref())
        else {
            return Ok(None);
        };
        if self.bounded_global_materialization(key)
            && !self.charge(
                key.old
                    .len()
                    .saturating_add(key.new.len())
                    .saturating_mul(4),
            )
        {
            *materialization_refusal = Some(MaterializationRefusal::Work);
            return Ok(None);
        }
        // Building the proof groups materializes the full parent key, not the
        // child spans, so the bound is computed from the key's blocks before
        // any group is built. Locate, clone and extent work is charged next.
        let parent_blocks = [
            &self.sides[0].blocks[key.old.clone()],
            &self.sides[1].blocks[key.new.clone()],
        ];
        let parent_work = parent_blocks
            .iter()
            .map(|blocks| blocks.len().saturating_mul(2))
            .fold(0usize, usize::saturating_add);
        let parent_tokens = parent_blocks
            .iter()
            .enumerate()
            .map(|(side, blocks)| {
                blocks.iter().fold(0usize, |total, block| {
                    let index = self.sides[side].index[&block.block];
                    total.saturating_add(self.sides[side].canonical[index].len())
                })
            })
            .fold(0usize, usize::saturating_add);
        // `locate_in_group` rebuilds the whole prefix group and the child
        // group for each side after `proof_groups` already materialized both
        // parents, so the parent token work is counted twice and the child
        // token/separator work once.
        let child_tokens = self.sides[0]
            .source_token_count(&old_span.blocks)
            .saturating_add(self.sides[1].source_token_count(&new_span.blocks));
        let locate_work = old_span
            .blocks
            .len()
            .saturating_add(new_span.blocks.len())
            .saturating_mul(4);
        let build_work = parent_work
            .saturating_add(parent_tokens.saturating_mul(2))
            .saturating_add(child_tokens)
            .saturating_add(locate_work);
        if !self.charge(build_work) {
            return Ok(None);
        }
        let [old, new] = match self.scoped_proof_groups(key)? {
            Ok(groups) => groups,
            Err(refusal) => {
                *materialization_refusal = Some(refusal);
                return Ok(None);
            }
        };
        let Some(old_range) = locate_in_group(self.sides[0], Some(old_span), &old)? else {
            return Ok(None);
        };
        let Some(new_range) = locate_in_group(self.sides[1], Some(new_span), &new)? else {
            return Ok(None);
        };
        if old_range.len() != new_range.len() || old_range.is_empty() {
            return Ok(None);
        }
        let equality_work = old_range.len();
        if !self.charge(equality_work) {
            return Ok(None);
        }
        if old.tokens[old_range.clone()] != new.tokens[new_range.clone()] {
            return Ok(None);
        }
        if !self.mandatory_analyses.contains_key(key) {
            let analysis = match semantic::mandatory_match_analysis(
                &old.tokens,
                &new.tokens,
                &mut self.remaining_work,
            ) {
                Ok(analysis) => analysis.map(std::sync::Arc::new),
                Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => None,
                Err(error) => return Err(error),
            };
            self.mandatory_analyses.insert(key.clone(), analysis);
        }
        let analysis = match self.mandatory_analyses.get(key) {
            Some(Some(analysis)) => std::sync::Arc::clone(analysis),
            _ => return Ok(None),
        };
        // The cache covers the parent domain, so each child token query is a
        // binary search over parent-sized storage; charge one comparison per
        // bit of index width before querying.
        let query_work = old_range.len().saturating_mul(usize::BITS as usize);
        if !self.charge(query_work) {
            return Ok(None);
        }
        if analysis.mandatory_diagonal_count(old_range.start, new_range.start, old_range.len())
            != old_range.len()
        {
            return Ok(None);
        }
        let extent = |side: usize, span: &TextSpan| -> Option<Range<usize>> {
            let mut start = None::<usize>;
            let mut end = None::<usize>;
            for block in &span.blocks {
                let index = *self.sides[side].index.get(block)?;
                start = Some(start.map_or(index, |current: usize| current.min(index)));
                end = Some(end.map_or(index + 1, |current: usize| current.max(index + 1)));
            }
            start.zip(end).map(|(start, end)| start..end)
        };
        let (Some(old_extent), Some(new_extent)) = (extent(0, old_span), extent(1, new_span))
        else {
            return Ok(None);
        };
        let local_key = DomainKey {
            local: Some((old_span.clone(), new_span.clone())),
            old: old_extent,
            new: new_extent,
            old_separator: key.old_separator,
            new_separator: key.new_separator,
        };
        Ok(Some((local_key, old_range, new_range)))
    }

    fn cached_relation(&self, proposal: &ProposedRelation) -> Option<usize> {
        self.proposal_relations
            .get(&ProposalKey::from(proposal))
            .copied()
    }

    fn remember_relation(&mut self, proposal: &ProposedRelation, index: usize) {
        self.proposal_relations
            .insert(ProposalKey::from(proposal), index);
    }

    fn reject_semantic(&mut self, proposal: &ProposedRelation) {
        self.semantic_rejections.insert(ProposalKey::from(proposal));
    }

    fn semantic_rejected(&self, proposal: &ProposedRelation) -> bool {
        self.semantic_rejections
            .contains(&ProposalKey::from(proposal))
    }

    fn cached_move(&self, proposal: &ProposedRelation) -> Option<Option<usize>> {
        self.move_relations
            .get(&ProposalKey::from(proposal))
            .copied()
    }

    fn assess_move_cached(&mut self, proposal: &ProposedRelation) -> Result<Option<usize>> {
        let key = ProposalKey::from(proposal);
        if let Some(&result) = self.move_relations.get(&key) {
            return Ok(result);
        }
        let result = if self.semantic_rejected(proposal) {
            None
        } else {
            self.assess_move(proposal)?
        };
        self.move_relations.insert(key, result);
        Ok(result)
    }

    fn assess_cached(&mut self, proposal: &ProposedRelation) -> Result<usize> {
        let key = ProposalKey::from(proposal);
        if let Some(&index) = self.proposal_relations.get(&key) {
            return Ok(index);
        }
        let index = self.assess(proposal)?;
        self.proposal_relations.insert(key, index);
        Ok(index)
    }

    fn reassess(&mut self, proposal: &ProposedRelation) -> Result<usize> {
        let key = ProposalKey::from(proposal);
        let index = self.assess(proposal)?;
        self.proposal_relations.insert(key, index);
        Ok(index)
    }

    #[cfg(test)]
    fn new(
        sides: [&'a Side<'document>; 2],
        alignment: &'a Alignment,
        recovery: Option<SentenceRecoveryInput<'a>>,
        options: DiffOptions,
    ) -> Result<Self> {
        Self::new_with_evidence(sides, alignment, recovery, options, None)
    }

    fn new_with_evidence(
        sides: [&'a Side<'document>; 2],
        alignment: &'a Alignment,
        recovery: Option<SentenceRecoveryInput<'a>>,
        options: DiffOptions,
        exact_displacement: Option<ExactDisplacementInput<'a>>,
    ) -> Result<Self> {
        let mut assessor = Self {
            sides,
            alignment,
            recovery,
            options,
            exact_displacement,
            exact_records: [HashMap::new(), HashMap::new()],
            remaining_work: options.max_assessment_work,
            anchors: Vec::new(),
            anchor_alternatives: Vec::new(),
            anchor_work: AnchorWork::default(),
            local_view_work: LocalViewWork::default(),
            suffix_reuse_work: SuffixReuseWork::default(),
            deny_token_cache: views::DenyTokenCache::new(sides),
            domains: HashMap::new(),
            forced_equal_relations: std::collections::HashSet::new(),
            mandatory_analyses: HashMap::new(),
            records: Vec::new(),
            output_stop: None,
            root_relation: None,
            materialization_stop: None,
            optional_search_stop: None,
            root_reasons: Vec::new(),
            semantic_acceptance: HashMap::new(),
            local_domains: Vec::new(),
            local_anchors: Vec::new(),
            anchored_translations: Vec::new(),
            stationary_members: Vec::new(),
            positioned_replacements: Vec::new(),
            raw_source_equalities: Vec::new(),
            bracketed_domains: Vec::new(),
            exact_displacements: Vec::new(),
            footer_domains: Vec::new(),
            localized_edits: Vec::new(),
            localized_edit_count: 0,
            proposal_relations: HashMap::new(),
            semantic_rejections: HashSet::new(),
            move_relations: HashMap::new(),
            issue_cache: None,
            equal_fragment_cache: None,
            equal_fragment_candidates: Vec::new(),
            empty_side: proven_empty_side(sides, alignment),
        };
        assessor.root_reasons = assessor.inspect_source_reasons();
        assessor.anchors = assessor.verified_anchors()?;

        if let Some(input) = assessor.exact_displacement {
            let mut complete = true;
            for (side, side_records) in [input.old, input.new].into_iter().enumerate() {
                let before = assessor.remaining_work;
                let paid = assessor.charge(side_records.len());
                assessor.anchor_work.sidecar_index += before - assessor.remaining_work;
                if !paid {
                    assessor.anchor_work.sidecar_refused_request = side_records.len();
                    complete = false;
                    break;
                }
                for (index, record) in side_records.iter().enumerate() {
                    assessor.exact_records[side].insert(record.glyph, index);
                }
            }
            if !complete {
                assessor.exact_displacement = None;
            }
        }
        Ok(assessor)
    }

    fn root_relation(&mut self) -> Result<usize> {
        if let Some(index) = self.root_relation {
            return Ok(index);
        }
        let old = domain_group(
            self.sides[0],
            0..self.sides[0].blocks.len(),
            BlockSeparator::Space,
        );
        let new = domain_group(
            self.sides[1],
            0..self.sides[1].blocks.len(),
            BlockSeparator::Space,
        );
        let reasons = self.source_reasons();
        let index = self.record(RelationAssessment {
            old_span: nonempty_span(&old),
            new_span: nonempty_span(&new),
            parent: None,
            outcome: if reasons.is_empty() {
                RelationOutcome::Established
            } else {
                RelationOutcome::Tentative
            },
            search: SearchCompleteness::Complete,
            assumptions: assumptions([&old, &new]),
            reasons,
        })?;
        self.root_relation = Some(index);
        Ok(index)
    }

    /// Optional nonowning content evidence after all existing recovery and
    /// review work. Complete parent correspondence and all-path matching cuts
    /// establish the containing region, never a chosen minimal glyph mask.
    fn collect_mandatory_changed_regions_from_review(
        &mut self,
        domains: &[(DomainKey, DomainProof)],
        domain_capacity: usize,
        partitions: [&[ResolutionRange]; 2],
        output: &mut Vec<ProvenChangedRegion>,
    ) -> Result<()> {
        if self.output_stop.is_some()
            || self.remaining_work == 0
            || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
        {
            return Ok(());
        }
        let Some(domain_bytes) =
            coarse_reviewed_domain_bytes(domains, domain_capacity, &mut self.remaining_work)
        else {
            return Ok(());
        };
        let mut retained_output_bytes = 0usize;
        for (key, proof) in domains {
            if self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
                || output.len() >= self.options.max_assessment_ranges
                || self.remaining_work == 0
            {
                break;
            }
            if proof.scope != ProofScope::ExactKey || proof.search != SearchCompleteness::Complete {
                continue;
            }
            let parent = &self.records[proof.relation];
            if parent.outcome != RelationOutcome::Established
                || parent.search != SearchCompleteness::Complete
                || !parent.reasons.is_empty()
            {
                continue;
            }
            // A complete unique literal script with no edits cannot contain
            // a substantively changed gap. Pay its cached-header check before
            // avoiding a redundant DP that could starve later source equality.
            // Semantic event invariance alone does not certify literal equality.
            if !charge_work(&mut self.remaining_work, 1) {
                break;
            }
            if proof.unique && proof.strict_unique && proof.edits.is_empty() {
                continue;
            }
            let (Some(old_span), Some(new_span)) = (&parent.old_span, &parent.new_span) else {
                continue;
            };
            let Some(view_limit) = COARSE_MEMORY_BYTES
                .checked_sub(domain_bytes)
                .and_then(|bytes| bytes.checked_sub(retained_output_bytes))
            else {
                break;
            };
            let output_before_domain = retained_output_bytes;
            let Ok(views) = borrowed_group_pair(
                self.sides,
                [old_span, new_span],
                &mut self.remaining_work,
                view_limit,
            ) else {
                continue;
            };
            let [old, new] = [views[0].selected_tokens(), views[1].selected_tokens()];
            if proof.exact_lengths() != Some([old.len(), new.len()]) {
                continue;
            }
            let ids = views[0].blocks.len().checked_add(views[1].blocks.len());
            let assumption_count = self.records[proof.relation]
                .assumptions
                .len()
                .checked_add(1);
            let Some(base_bytes) = ids
                .and_then(|ids| ids.checked_mul(4 * std::mem::size_of::<BlockId>() + 128))
                .and_then(|bytes| {
                    bytes.checked_add(
                        assumption_count?
                            .checked_mul(std::mem::size_of::<ComparisonAssumption>())?,
                    )
                })
                .and_then(|bytes| bytes.checked_add(views[0].retained_bytes))
                .and_then(|bytes| bytes.checked_add(views[1].retained_bytes))
                .and_then(|bytes| bytes.checked_add(domain_bytes))
                .and_then(|bytes| bytes.checked_add(retained_output_bytes))
                .and_then(|bytes| {
                    bytes.checked_add(
                        std::mem::size_of::<RelationAssessment>()
                            + std::mem::size_of::<ProvenChangedRegion>(),
                    )
                })
                .and_then(|bytes| {
                    bytes.checked_add(old.len().checked_add(new.len())?.checked_mul(128)?)
                })
            else {
                continue;
            };
            let Some(analysis_limit) = COARSE_MEMORY_BYTES.checked_sub(base_bytes) else {
                continue;
            };
            let fresh;
            let analysis = match self.mandatory_analyses.get(key) {
                Some(Some(analysis)) => {
                    if analysis
                        .pairs()
                        .len()
                        .checked_mul(std::mem::size_of::<(usize, usize)>())
                        .is_none_or(|bytes| bytes > analysis_limit)
                    {
                        continue;
                    }
                    analysis.as_ref()
                }
                Some(None) => continue,
                None => {
                    fresh = match semantic::mandatory_match_analysis_with_memory_limit(
                        old,
                        new,
                        &mut self.remaining_work,
                        analysis_limit,
                    ) {
                        Ok(Some(analysis)) => analysis,
                        Ok(None) | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    &fresh
                }
            };
            if !charge_work(&mut self.remaining_work, analysis.pairs().len()) {
                continue;
            }
            for pair in analysis.pairs().windows(2) {
                if self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
                    || output.len() >= self.options.max_assessment_ranges
                {
                    break;
                }
                let Some(live_bytes) = base_bytes
                    .checked_add(retained_output_bytes - output_before_domain)
                    .and_then(|bytes| {
                        bytes.checked_add(
                            analysis
                                .pairs()
                                .len()
                                .checked_mul(std::mem::size_of::<(usize, usize)>())?,
                        )
                    })
                else {
                    break;
                };
                if live_bytes > COARSE_MEMORY_BYTES {
                    break;
                }
                let ranges = [pair[0].0..pair[1].0 - 1, pair[0].1..pair[1].1 - 1];
                // This rule reports only two-sided, substantively differing
                // interiors. Mandatory endpoints are excluded from both cuts.
                if ranges.iter().any(Range::is_empty) {
                    continue;
                }
                let [old_gap, new_gap] = [&old[ranges[0].clone()], &new[ranges[1].clone()]];
                if !coarse_substantive_mismatch(old_gap, new_gap, &mut self.remaining_work) {
                    continue;
                }
                let ids = views[0].blocks.len().saturating_add(views[1].blocks.len());
                let assumption_count = self.records[proof.relation]
                    .assumptions
                    .len()
                    .saturating_add(1);
                let Some(copy_bytes) = ids
                    .checked_mul(4 * std::mem::size_of::<BlockId>())
                    .and_then(|bytes| {
                        bytes.checked_add(
                            assumption_count
                                .checked_mul(std::mem::size_of::<ComparisonAssumption>())?,
                        )
                    })
                else {
                    continue;
                };
                if copy_bytes > COARSE_MEMORY_BYTES
                    || !charge_work(
                        &mut self.remaining_work,
                        ids.saturating_mul(4).saturating_add(assumption_count),
                    )
                {
                    continue;
                }
                let (Some(old_span), Some(new_span)) = (
                    views[0].try_span(ranges[0].clone()),
                    views[1].try_span(ranges[1].clone()),
                ) else {
                    continue;
                };
                if !coarse_gap_is_unresolved(
                    self.sides,
                    [&old_span, &new_span],
                    partitions,
                    output,
                    &mut self.remaining_work,
                )? || !coarse_source_guard(
                    self.sides[0],
                    &old_span,
                    old_gap,
                    &mut self.remaining_work,
                )? || !coarse_source_guard(
                    self.sides[1],
                    &new_span,
                    new_gap,
                    &mut self.remaining_work,
                )? {
                    continue;
                }
                let mut assumptions = Vec::new();
                if assumptions.try_reserve_exact(assumption_count).is_err() {
                    continue;
                }
                assumptions.extend_from_slice(&self.records[proof.relation].assumptions);
                assumptions.push(ComparisonAssumption::MandatoryMatchingBoundaries);
                let copy = |span: &TextSpan| -> Option<TextSpan> {
                    Some(TextSpan {
                        blocks: super::try_copy_slice(&span.blocks)?,
                        separator: span.separator,
                        canonical_range: span.canonical_range,
                        comparable_range: span.comparable_range,
                    })
                };
                let (Some(record_old), Some(record_new)) = (copy(&old_span), copy(&new_span))
                else {
                    continue;
                };
                // Both output reservations precede either append. The new
                // correspondence is not inserted into semantic acceptance or
                // localized-edit maps and cannot claim token ownership.
                if self.records.try_reserve_exact(1).is_err()
                    || output.try_reserve_exact(1).is_err()
                {
                    continue;
                }
                let Some(retained) = ids
                    .checked_mul(2 * std::mem::size_of::<BlockId>())
                    .and_then(|bytes| {
                        bytes.checked_add(
                            assumption_count
                                .checked_mul(std::mem::size_of::<ComparisonAssumption>())?,
                        )
                    })
                    .and_then(|bytes| {
                        bytes.checked_add(
                            std::mem::size_of::<RelationAssessment>()
                                + std::mem::size_of::<ProvenChangedRegion>(),
                        )
                    })
                    .and_then(|bytes| bytes.checked_add(retained_output_bytes))
                else {
                    continue;
                };
                retained_output_bytes = retained;
                self.records.push(RelationAssessment {
                    old_span: Some(record_old),
                    new_span: Some(record_new),
                    parent: Some(proof.relation),
                    outcome: RelationOutcome::Established,
                    search: SearchCompleteness::Complete,
                    assumptions,
                    reasons: Vec::new(),
                });
                output.push(ProvenChangedRegion {
                    old_span: Some(old_span),
                    new_span: Some(new_span),
                    confidence: super::Confidence::High,
                    proof: super::ChangedRegionProof::ExactTokenMultisetMismatch,
                });
            }
        }
        Ok(())
    }

    /// Direct collector fixtures retain the cache after simulating review's
    /// move-only handoff. Production always uses the actual reviewed vector.
    #[cfg(test)]
    fn collect_mandatory_changed_regions(
        &mut self,
        partitions: [&[ResolutionRange]; 2],
        output: &mut Vec<ProvenChangedRegion>,
    ) -> Result<()> {
        let mut domains: Vec<_> = std::mem::take(&mut self.domains).into_iter().collect();
        domains.sort_unstable_by_key(|(_, proof)| proof.relation);
        let result = self.collect_mandatory_changed_regions_from_review(
            &domains,
            domains.capacity(),
            partitions,
            output,
        );
        self.domains.extend(domains);
        result
    }

    fn charge(&mut self, work: usize) -> bool {
        if let Some(remaining) = self.remaining_work.checked_sub(work) {
            self.remaining_work = remaining;
            true
        } else {
            self.remaining_work = 0;
            false
        }
    }

    fn record(&mut self, record: RelationAssessment) -> Result<usize> {
        if let Some(index) = self.output_stop {
            return Ok(index);
        }
        if self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1) {
            let old = domain_group(
                self.sides[0],
                0..self.sides[0].blocks.len(),
                BlockSeparator::Space,
            );
            let new = domain_group(
                self.sides[1],
                0..self.sides[1].blocks.len(),
                BlockSeparator::Space,
            );
            let index = self.records.len();
            reserve_ranges(&mut self.records, 1, self.options.max_assessment_ranges)?;
            self.records.push(RelationAssessment {
                old_span: nonempty_span(&old),
                new_span: nonempty_span(&new),
                parent: None,
                outcome: RelationOutcome::Tentative,
                search: SearchCompleteness::Incomplete,
                assumptions: Vec::new(),
                reasons: vec![AssessmentReason::OutputLimit],
            });
            self.output_stop = Some(index);
            return Ok(index);
        }
        reserve_ranges(&mut self.records, 1, self.options.max_assessment_ranges)?;
        let index = self.records.len();
        self.records.push(record);
        Ok(index)
    }

    fn verified_anchors(&mut self) -> Result<Vec<(usize, usize)>> {
        let [old, new] = self.sides;
        let mut counts = [
            HashMap::<&[ComparableToken], Vec<usize>>::new(),
            HashMap::new(),
        ];
        for (side, keys) in self.sides.into_iter().zip(&mut counts) {
            for (index, tokens) in side.canonical.iter().enumerate() {
                let before = self.remaining_work;
                let paid = self.charge(tokens.len().saturating_add(1));
                self.anchor_work.candidate_index += before - self.remaining_work;
                if !paid {
                    self.anchor_work.budget_exhausted = true;
                    self.anchor_work.refused_request = tokens.len().saturating_add(1);
                    self.anchor_work.refused_remainder = before;
                    return Ok(Vec::new());
                }
                if !tokens.is_empty() {
                    keys.entry(tokens).or_default().push(index);
                }
            }
        }
        let mut anchors = Vec::new();
        for (old_index, tokens) in old.canonical.iter().enumerate() {
            if let (Some(old_positions), Some(new_positions)) = (
                counts[0].get(tokens.as_slice()),
                counts[1].get(tokens.as_slice()),
            ) && old_positions.len() == 1
                && new_positions.len() == 1
            {
                let new_index = new_positions[0];
                if old.blocks[old_index].issues.is_empty()
                    && new.blocks[new_index].issues.is_empty()
                    && old.blocks[old_index]
                        .role
                        .is_alignment_compatible(new.blocks[new_index].role)
                {
                    anchors.push((old_index, new_index));
                }
            }
        }
        // Whole-block identity is only a candidate boundary: an equal source
        // sequence spanning removable block boundaries also competes with it.
        for side in self.sides {
            for separator in [BlockSeparator::Space, BlockSeparator::Concatenate] {
                // An empty candidate set supplies no boundaries. There are no
                // occurrence queries to index, and later source-domain checks
                // still decide whether any proposed relation can be proved.
                if anchors.is_empty() {
                    return Ok(Vec::new());
                }
                let before = self.remaining_work;
                let paid = self.charge(side.total_tokens.saturating_add(side.blocks.len()));
                self.anchor_work.occurrence_index += before - self.remaining_work;
                if !paid {
                    self.anchor_work.budget_exhausted = true;
                    self.anchor_work.refused_request =
                        side.total_tokens.saturating_add(side.blocks.len());
                    self.anchor_work.refused_remainder = before;
                    return Ok(Vec::new());
                }
                let group = domain_group(side, 0..side.blocks.len(), separator);
                let mut postings = HashMap::<&ComparableToken, Vec<usize>>::new();
                for (position, token) in group.tokens.iter().enumerate() {
                    postings.entry(token).or_default().push(position);
                }
                let before = self.remaining_work;
                let pair_work = group.tokens.len().saturating_sub(1);
                let paid = self.charge(pair_work);
                self.anchor_work.occurrence_index += before - self.remaining_work;
                if !paid {
                    self.anchor_work.budget_exhausted = true;
                    self.anchor_work.refused_request = pair_work;
                    self.anchor_work.refused_remainder = before;
                    return Ok(Vec::new());
                }
                let mut pair_postings = HashMap::new();
                for (position, pair) in group.tokens.windows(2).enumerate() {
                    pair_postings
                        .entry((&pair[0], &pair[1]))
                        .or_insert_with(Vec::new)
                        .push(position);
                }
                let mut verified = Vec::new();
                for anchor in anchors {
                    let needle = &old.canonical[anchor.0];
                    // Every complete occurrence must contain this internal
                    // token or adjacent pair at the chosen offset. Enumerating all its postings
                    // therefore removes impossible starts without removing an
                    // alternative correspondence; full equality still decides.
                    let before = self.remaining_work;
                    let paid = self.charge(needle.len());
                    self.anchor_work.occurrence_index += before - self.remaining_work;
                    if !paid {
                        self.anchor_work.budget_exhausted = true;
                        self.anchor_work.refused_request = needle.len();
                        self.anchor_work.refused_remainder = before;
                        return Ok(Vec::new());
                    }
                    let (offset, positions) = if needle.len() >= 2 {
                        rarest_pair_posting(needle, &pair_postings)
                    } else {
                        rarest_posting(needle, &postings)
                    };
                    let mut count = 0;
                    for &position in positions {
                        let Some(start) = position.checked_sub(offset) else {
                            continue;
                        };
                        let candidate = group
                            .tokens
                            .get(start..start.saturating_add(needle.len()))
                            .unwrap_or_default();
                        self.anchor_work.starts_examined += 1;
                        let before = self.remaining_work;
                        let equal =
                            tokens_equal_with_budget(candidate, needle, &mut self.remaining_work);
                        self.anchor_work.occurrence_comparison += before - self.remaining_work;
                        match equal {
                            None => {
                                self.anchor_work.budget_exhausted = true;
                                self.anchor_work.refused_request = 1;
                                return Ok(Vec::new());
                            }
                            Some(false) => {}
                            Some(true) => {
                                count += 1;
                                if count == 2 {
                                    break;
                                }
                            }
                        }
                    }
                    if count == 1 {
                        verified.push(anchor);
                    }
                }
                anchors = verified;
            }
        }
        let positions = anchors.iter().map(|&(_, new)| new).collect::<Vec<_>>();
        let before = self.remaining_work;
        let uniqueness = exact::increasing_spine(
            &positions,
            &mut self.remaining_work,
            &mut self.anchor_work.refused_request,
        )?;
        self.anchor_work.order_uniqueness += before - self.remaining_work;
        self.anchor_work.budget_exhausted = uniqueness == exact::ExactUniqueness::BudgetExceeded;
        self.anchor_work.order_unique = match uniqueness {
            exact::ExactUniqueness::Unique => Some(true),
            exact::ExactUniqueness::Ambiguous => Some(false),
            exact::ExactUniqueness::BudgetExceeded => None,
        };
        if uniqueness == exact::ExactUniqueness::BudgetExceeded {
            return Ok(Vec::new());
        }
        if uniqueness == exact::ExactUniqueness::Ambiguous {
            let before = self.remaining_work;
            let mandatory = exact::mandatory_increasing_spine(
                &positions,
                &mut self.remaining_work,
                &mut self.anchor_work.refused_request,
            )?;
            self.anchor_work.order_uniqueness += before - self.remaining_work;
            let Some(mandatory) = mandatory else {
                self.anchor_work.budget_exhausted = true;
                return Ok(Vec::new());
            };
            // The closure guard retains every noncommon verified source pair,
            // a superset of these maximum-path alternatives.
            drop(mandatory.competing);
            let projection_work = anchors.len();
            let before = self.remaining_work;
            let paid = self.charge(projection_work);
            self.anchor_work.order_uniqueness += before - self.remaining_work;
            if !paid {
                self.anchor_work.budget_exhausted = true;
                self.anchor_work.refused_request = projection_work;
                self.anchor_work.refused_remainder = before;
                return Ok(Vec::new());
            }
            // The returned indices and their projected pairs coexist here;
            // bound both, as well as using fallible result allocation.
            let projection_bytes = anchors.len().checked_mul(
                2 * std::mem::size_of::<usize>() + 2 * std::mem::size_of::<(usize, usize)>(),
            );
            if projection_bytes.is_none_or(|bytes| bytes > 64 * 1024 * 1024) {
                return Err(Error::LimitExceeded {
                    resource: "mandatory anchor projection memory",
                    limit: 64 * 1024 * 1024,
                });
            }
            // Common boundaries narrow correspondence windows only. Internal
            // alternatives and source barriers still reach the domain proof.
            let mut result = Vec::new();
            result
                .try_reserve_exact(mandatory.mandatory.len())
                .map_err(|_| allocation_error("mandatory anchor projection"))?;
            let mut alternatives = Vec::new();
            alternatives
                .try_reserve_exact(anchors.len() - mandatory.mandatory.len())
                .map_err(|_| allocation_error("competing anchor projection"))?;
            let mut common = mandatory.mandatory.into_iter().peekable();
            for (index, pair) in anchors.into_iter().enumerate() {
                if common.peek() == Some(&index) {
                    result.push(pair);
                    common.next();
                } else {
                    alternatives.push(pair);
                }
            }
            self.anchor_alternatives = alternatives;
            self.anchor_work.verified_anchors = result.len();
            return Ok(result);
        }
        // The exact check establishes a unique increasing spine. Reconstruct
        // that spine without choosing an arbitrary equal-length alternative.
        let mut tails = Vec::<usize>::new();
        let mut previous = vec![None; anchors.len()];
        for index in 0..anchors.len() {
            let position = tails.partition_point(|&tail| anchors[tail].1 < anchors[index].1);
            if position > 0 {
                previous[index] = Some(tails[position - 1]);
            }
            if position == tails.len() {
                tails.push(index);
            } else {
                tails[position] = index;
            }
        }
        let mut result = Vec::new();
        let mut cursor = tails.last().copied();
        while let Some(index) = cursor {
            result.push(anchors[index]);
            cursor = previous[index];
        }
        result.reverse();
        self.anchor_work.verified_anchors = result.len();
        Ok(result)
    }
}

#[cfg(test)]
mod proposal_signature_tests {
    use super::target_hunk_signature;
    use crate::diff::AtomicEdit;

    fn substitution(old_start: usize, new_start: usize) -> [AtomicEdit; 2] {
        [
            AtomicEdit {
                old: old_start..old_start + 1,
                new: new_start..new_start,
            },
            AtomicEdit {
                old: old_start + 1..old_start + 1,
                new: new_start..new_start + 1,
            },
        ]
    }

    #[test]
    fn contained_substitution_is_one_hunk() {
        let edits = substitution(2, 2);
        assert_eq!(
            target_hunk_signature(&(2..3), &(2..3), &edits),
            Some((2..3, 2..3, vec![(2..3, 2..3)]))
        );
    }

    #[test]
    fn hunks_crossing_a_target_boundary_are_absent() {
        let crossing_start = [
            AtomicEdit {
                old: 1..3,
                new: 1..1,
            },
            AtomicEdit {
                old: 3..3,
                new: 1..2,
            },
        ];
        assert_eq!(
            target_hunk_signature(&(2..3), &(1..2), &crossing_start),
            None
        );
        let crossing_end = [
            AtomicEdit {
                old: 3..4,
                new: 3..3,
            },
            AtomicEdit {
                old: 4..4,
                new: 3..5,
            },
        ];
        assert_eq!(target_hunk_signature(&(3..4), &(3..4), &crossing_end), None);
    }

    #[test]
    fn hunks_outside_the_target_are_absent() {
        let edits = substitution(0, 0);
        assert_eq!(target_hunk_signature(&(3..4), &(3..4), &edits), None);
    }

    #[test]
    fn empty_sides_localize_insertions_and_deletions() {
        let insertion = [
            AtomicEdit {
                old: 2..2,
                new: 2..3,
            },
            AtomicEdit {
                old: 2..2,
                new: 3..4,
            },
        ];
        assert_eq!(
            target_hunk_signature(&(2..2), &(2..4), &insertion),
            Some((2..2, 2..4, vec![(2..2, 2..4)]))
        );
        let deletion = [
            AtomicEdit {
                old: 2..3,
                new: 2..2,
            },
            AtomicEdit {
                old: 3..4,
                new: 2..2,
            },
        ];
        assert_eq!(
            target_hunk_signature(&(2..4), &(2..2), &deletion),
            Some((2..4, 2..2, vec![(2..4, 2..2)]))
        );
    }

    #[test]
    fn start_and_end_targets_keep_their_hunks() {
        let start = substitution(0, 0);
        assert_eq!(
            target_hunk_signature(&(0..1), &(0..1), &start),
            Some((0..1, 0..1, vec![(0..1, 0..1)]))
        );
        let end = substitution(3, 3);
        assert_eq!(
            target_hunk_signature(&(3..4), &(3..4), &end),
            Some((3..4, 3..4, vec![(3..4, 3..4)]))
        );
    }

    #[test]
    fn equal_run_points_are_correspondence_boundaries() {
        assert!(super::point_on_script([1, 1], &[], [2, 2]));
        assert!(super::point_on_script([2, 2], &[], [2, 2]));
        assert!(!super::point_on_script([3, 3], &[], [2, 2]));
        assert!(!super::point_on_script([2, 1], &[], [2, 2]));
    }

    #[test]
    fn hunk_corners_are_boundaries_but_interiors_are_not() {
        let edits = substitution(1, 1);
        assert!(super::point_on_script([1, 1], &edits, [3, 3]));
        assert!(super::point_on_script([2, 2], &edits, [3, 3]));
        assert!(!super::point_on_script([1, 2], &edits, [3, 3]));
        assert!(!super::point_on_script([2, 1], &edits, [3, 3]));
    }

    #[test]
    fn leading_and_trailing_runs_follow_the_script_cursor() {
        let edits = substitution(2, 2);
        assert!(super::point_on_script([0, 0], &edits, [4, 4]));
        assert!(super::point_on_script([1, 1], &edits, [4, 4]));
        assert!(super::point_on_script([2, 2], &edits, [4, 4]));
        assert!(super::point_on_script([3, 3], &edits, [4, 4]));
        assert!(super::point_on_script([4, 4], &edits, [4, 4]));
        assert!(!super::point_on_script([3, 4], &edits, [4, 4]));
    }

    #[test]
    fn repeated_positions_select_only_contained_hunks() {
        let first = substitution(1, 1);
        let second = substitution(3, 3);
        let edits = [
            first[0].clone(),
            first[1].clone(),
            second[0].clone(),
            second[1].clone(),
        ];
        assert_eq!(
            target_hunk_signature(&(1..2), &(1..2), &edits),
            Some((1..2, 1..2, vec![(1..2, 1..2)]))
        );
        assert_eq!(
            target_hunk_signature(&(0..4), &(0..4), &edits),
            Some((0..4, 0..4, vec![(1..2, 1..2), (3..4, 3..4)]))
        );
    }
}

mod exact_displacement;

#[cfg(test)]
mod separator_tests {
    use super::{BlockSeparator, domain_separator};

    #[test]
    fn mixed_separators_remain_bound_to_their_original_group() {
        let separator = BlockSeparator::PerBoundary([false, true]);
        let mixed = Some(separator);
        assert_eq!(domain_separator(mixed, 2..5, 2..5), separator);
        assert_eq!(domain_separator(mixed, 2..5, 0..6), BlockSeparator::Space);
        assert_eq!(domain_separator(mixed, 2..5, 1..4), BlockSeparator::Space);
        assert_eq!(
            domain_separator(Some(BlockSeparator::Concatenate), 2..5, 0..6),
            BlockSeparator::Concatenate,
        );
    }
}

#[cfg(test)]
mod assessor_issue_cache_tests {
    use super::*;
    use crate::{
        alignment::{AlignmentConfidence, AlignmentSpan},
        layout::BlockRole,
        model::GlyphId,
        normalize::{
            MappedText, NormalizationIssue, NormalizationIssueKind, SourceMapEntry, TextSource,
            TextSourceAtom,
        },
    };

    /// A block whose only issue projects to canonical range 1..2.
    fn issue_block(id: u64) -> crate::normalize::BlockText {
        let entry = |index: usize, glyph: u64| SourceMapEntry {
            output_range: ScalarRange {
                start: index,
                end: index + 1,
            },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(glyph))].into(),
            },
        };
        let canonical = MappedText {
            text: "ABC".to_owned(),
            source_map: vec![entry(0, 1), entry(1, 2), entry(2, 3)],
            unmapped: Vec::new(),
        };
        let tokens = canonical.comparable_tokens().expect("issue tokens");
        crate::normalize::BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: "ABC".to_owned(),
            matching_tokens: tokens,
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: vec![NormalizationIssue {
                kind: NormalizationIssueKind::AmbiguousLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(2))].into(),
                },
            }],
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    /// The same block without any issue.
    fn plain_block(id: u64) -> crate::normalize::BlockText {
        let mut block = issue_block(id);
        block.issues = Vec::new();
        block
    }

    fn anchor_block(id: u64, text: &str) -> crate::normalize::BlockText {
        let mut block = plain_block(id);
        block.canonical.text = text.to_owned();
        block.canonical.source_map = text
            .chars()
            .enumerate()
            .map(|(index, _)| SourceMapEntry {
                output_range: ScalarRange {
                    start: index,
                    end: index + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(id * 100 + index as u64))].into(),
                },
            })
            .collect();
        block.raw = block.canonical.clone();
        block.matching = text.to_owned();
        block.matching_tokens = block.canonical.comparable_tokens().expect("anchor tokens");
        block
    }

    /// Installs a declared complete, source-closed parent for isolated collector
    /// tests. The collector must not infer closure from token similarity itself.
    fn h8_closed_parent<'a, 'document>(
        sides: [&'a Side<'document>; 2],
        alignment: &'a Alignment,
    ) -> Result<Assessor<'a, 'document>> {
        let mut assessor =
            Assessor::new_with_evidence(sides, alignment, None, DiffOptions::default(), None)?;
        assessor.records.clear();
        assessor.domains.clear();
        assessor.mandatory_analyses.clear();
        assessor.output_stop = None;
        assessor.remaining_work = 1_000_000;
        let spans = sides.map(|side| {
            side.canonical_group(
                &side
                    .blocks
                    .iter()
                    .map(|block| block.block)
                    .collect::<Vec<_>>(),
                Some(BlockSeparator::Space),
            )
            .full_span()
        });
        let key = DomainKey {
            local: Some((spans[0].clone(), spans[1].clone())),
            old: 0..sides[0].blocks.len(),
            new: 0..sides[1].blocks.len(),
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        };
        assessor.records.push(RelationAssessment {
            old_span: Some(spans[0].clone()),
            new_span: Some(spans[1].clone()),
            parent: None,
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            assumptions: vec![
                ComparisonAssumption::InputReadingOrder,
                ComparisonAssumption::CanonicalNormalization,
                ComparisonAssumption::LocalEvidenceBoundaries,
            ],
            reasons: Vec::new(),
        });
        assessor.domains.insert(
            key,
            DomainProof {
                scope: ProofScope::ExactKey,
                relation: 0,
                unique: false,
                search: SearchCompleteness::Complete,
                edits: Vec::new(),
                lengths: spans.map(|span| span.comparable_range.end),
                strict_unique: false,
                stable_events: None,
            },
        );
        Ok(assessor)
    }

    fn h8_partition(
        block: BlockId,
        rows: &[(usize, usize, ResolutionState)],
    ) -> Vec<ResolutionRange> {
        rows.iter()
            .map(|&(start, end, state)| ResolutionRange {
                block,
                comparable_range: TokenRange { start, end },
                canonical_range: ScalarRange { start, end },
                state,
            })
            .collect()
    }

    #[test]
    fn cached_equal_domain_does_not_starve_later_mandatory_source_equalities() -> Result<()> {
        let unchanged = "a".repeat(60);
        let old_blocks = [anchor_block(1, &unchanged), anchor_block(2, "[20] AUTHOR")];
        let new_blocks = [
            anchor_block(101, &unchanged),
            anchor_block(102, "[22] AUTHOR"),
        ];
        let old =
            super::super::SidePlan::inspect("old budget fixture", &old_blocks)?.materialize()?;
        let new =
            super::super::SidePlan::inspect("new budget fixture", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run = |include_equal_domain: bool,
                   equal_unique: bool,
                   equal_strict: bool|
         -> Result<([Vec<ResolutionRange>; 2], usize)> {
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            assessor.records.clear();
            assessor.domains.clear();
            assessor.remaining_work = 16_000;
            let mut domains = Vec::new();
            for index in 0..2 {
                let spans = [&old, &new].map(|side| {
                    side.canonical_group(&[side.blocks[index].block], None)
                        .full_span()
                });
                assessor.records.push(RelationAssessment {
                    old_span: Some(spans[0].clone()),
                    new_span: Some(spans[1].clone()),
                    parent: None,
                    outcome: RelationOutcome::Established,
                    search: SearchCompleteness::Complete,
                    assumptions: vec![
                        ComparisonAssumption::InputReadingOrder,
                        ComparisonAssumption::CanonicalNormalization,
                        ComparisonAssumption::LocalEvidenceBoundaries,
                    ],
                    reasons: Vec::new(),
                });
                if index == 0 && !include_equal_domain {
                    continue;
                }
                domains.push((
                    DomainKey {
                        local: Some((spans[0].clone(), spans[1].clone())),
                        old: index..index + 1,
                        new: index..index + 1,
                        old_separator: BlockSeparator::Concatenate,
                        new_separator: BlockSeparator::Concatenate,
                    },
                    DomainProof {
                        scope: ProofScope::ExactKey,
                        relation: index,
                        unique: index == 0 && equal_unique,
                        search: SearchCompleteness::Complete,
                        edits: Vec::new(),
                        lengths: spans.map(|span| span.comparable_range.end),
                        strict_unique: index == 0 && equal_strict,
                        stable_events: None,
                    },
                ));
            }
            let mut ownership = [Ownership::new(), Ownership::new()];
            for (side, index) in [&old, &new].into_iter().enumerate() {
                let span = index
                    .canonical_group(&[index.blocks[0].block], None)
                    .full_span();
                ownership[side].accept(index, &span, 100)?;
            }
            let mut partitions = [
                ownership[0].resolution(&old, 100)?,
                ownership[1].resolution(&new, 100)?,
            ];
            let parent = &assessor.records[1];
            let mut regions = vec![ProvenChangedRegion {
                old_span: parent.old_span.clone(),
                new_span: parent.new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            }];
            assessor.collect_mandatory_changed_regions_from_review(
                &domains,
                domains.capacity(),
                [&partitions[0], &partitions[1]],
                &mut regions,
            )?;
            let [old_parts, new_parts] = &mut partitions;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut ownership,
                [old_parts, new_parts],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &regions,
                    original: &[],
                },
            )?;
            assert_eq!(regions[0].old_span, assessor.records[1].old_span);
            assert_eq!(regions[0].new_span, assessor.records[1].new_span);
            Ok((partitions, 16_000 - assessor.remaining_work))
        };
        let (baseline, baseline_work) = run(false, true, true)?;
        let (with_equal, with_equal_work) = run(true, true, true)?;
        assert!(baseline_work <= 16_000 && with_equal_work <= 16_000);
        // Empty cached edits can also be a refused or ambiguous proof. Such
        // headers must retain the ordinary analysis rather than taking the shortcut.
        for (unique, strict) in [(false, true), (true, false), (false, false)] {
            let (uncertified, work) = run(true, unique, strict)?;
            assert!(work > with_equal_work && work <= 16_000);
            for (side, parts) in uncertified.iter().enumerate() {
                let block = [BlockId(2), BlockId(102)][side];
                assert!(
                    parts
                        .iter()
                        .filter(|range| range.block == block)
                        .all(|range| { range.state == ResolutionState::Unresolved })
                );
            }
        }
        for side in 0..2 {
            let block = [BlockId(2), BlockId(102)][side];
            let selected = |parts: &[ResolutionRange]| {
                parts
                    .iter()
                    .filter(|range| range.block == block)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert!(
                selected(&baseline[side])
                    .iter()
                    .any(|range| range.state == ResolutionState::Equal)
            );
            assert_eq!(selected(&with_equal[side]), selected(&baseline[side]));
        }
        Ok(())
    }

    #[test]
    fn h11_whole_coarse_mandatory_equality_keeps_ambiguous_digits_unresolved() -> Result<()> {
        let old_blocks = [anchor_block(1, "[20] AUTHOR")];
        let new_blocks = [anchor_block(101, "[22] AUTHOR")];
        let old = super::super::SidePlan::inspect("H11 old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("H11 new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run =
            |budget,
             cap,
             refusal: u8|
             -> Result<(Assessor<'_, '_>, [Vec<ResolutionRange>; 2], [Ownership; 2])> {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                assessor.remaining_work = budget;
                assessor.options.max_assessment_ranges = cap;
                let mut domains = std::mem::take(&mut assessor.domains)
                    .into_iter()
                    .collect::<Vec<_>>();
                match refusal {
                    1 => domains[0].1.search = SearchCompleteness::Incomplete,
                    2 => domains[0].1.scope = ProofScope::BroadRootRefusal,
                    3 => assessor.records[0]
                        .reasons
                        .push(AssessmentReason::NormalizationUncertainty),
                    4 => {
                        domains[0].0.local = None;
                        assessor.records[0]
                            .assumptions
                            .retain(|item| *item != ComparisonAssumption::LocalEvidenceBoundaries);
                    }
                    6 => assessor.records[0]
                        .assumptions
                        .push(ComparisonAssumption::AlternativeLineBreakNormalization),
                    7 => domains[0].0.local = None,
                    8 => {
                        domains[0].0.local = None;
                        assessor.records[0].parent = Some(0);
                    }
                    9 => {
                        domains[0].0.local = None;
                        domains[0].0.old = 0..2;
                    }
                    10 => {
                        domains[0].0.local = None;
                        domains[0].0.old = 1..2;
                    }
                    11 => {
                        domains[0].0.local = None;
                        assessor.records[0]
                            .old_span
                            .as_mut()
                            .expect("the declared parent has both spans")
                            .comparable_range
                            .start = 1;
                    }
                    12 => {
                        domains[0].0.local = None;
                        domains[0].0.new = 0..0;
                    }
                    _ => {}
                }
                let parent = &assessor.records[0];
                let mut regions = vec![ProvenChangedRegion {
                    old_span: parent.old_span.clone(),
                    new_span: parent.new_span.clone(),
                    confidence: super::super::Confidence::High,
                    proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                }];
                if refusal == 5 {
                    regions.clear();
                }
                let mut partitions = [
                    h8_partition(BlockId(1), &[(0, 11, ResolutionState::Unresolved)]),
                    h8_partition(BlockId(101), &[(0, 11, ResolutionState::Unresolved)]),
                ];
                let mut ownership = [Ownership::new(), Ownership::new()];
                let parent_before = assessor.records[0].clone();
                let [old_parts, new_parts] = &mut partitions;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut ownership,
                    [old_parts, new_parts],
                    mandatory_equal::EqualConstraints {
                        candidates: &[],
                        regions: &regions,
                        original: &[],
                    },
                )?;
                assert_eq!(assessor.records[0], parent_before);
                assert!(assessor.semantic_acceptance.is_empty());
                assert!(assessor.localized_edits.is_empty());
                for child in &assessor.records[1..] {
                    assert_eq!(child.parent, Some(0));
                    let pair = [
                        child
                            .old_span
                            .as_ref()
                            .expect("the complete fixture has both source spans")
                            .comparable_range,
                        child
                            .new_span
                            .as_ref()
                            .expect("the complete fixture has both source spans")
                            .comparable_range,
                    ];
                    assert!(
                        [(0, 1), (3, 4), (5, 11)]
                            .iter()
                            .any(|&(start, end)| pair == [TokenRange { start, end }; 2])
                    );
                }
                Ok((assessor, partitions, ownership))
            };
        let (full, partitions, ownership) = run(1_000_000, 100, 0)?;
        assert_eq!(full.records.len(), 4);
        assert_eq!(ownership[0].accepted.len(), 3);
        assert_eq!(ownership[1].accepted.len(), 3);
        for partition in &partitions {
            let equal = partition
                .iter()
                .filter(|part| part.state == ResolutionState::Equal)
                .map(|part| part.comparable_range.end - part.comparable_range.start)
                .sum::<usize>();
            assert_eq!(equal, 8); // '[', ']', AUTHOR; ambiguous label and space stay held.
            assert!(
                partition
                    .iter()
                    .any(|part| part.state == ResolutionState::Unresolved
                        && part.comparable_range.start <= 1
                        && part.comparable_range.end >= 3)
            );
        }
        let used = 1_000_000 - full.remaining_work;
        for budget in 0..=used {
            let (partial, _, owners) = run(budget, 100, 0)?;
            assert!(partial.records.len() <= full.records.len());
            assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
            assert_eq!(partial.records.len() - 1, owners[0].accepted.len());
        }
        let (isolated, isolated_parts, isolated_owners) = run(1_000_000, 100, 7)?;
        assert_eq!(isolated.records, full.records);
        assert_eq!(isolated_parts, partitions);
        assert_eq!(isolated_owners[0].accepted, ownership[0].accepted);
        assert_eq!(isolated_owners[1].accepted, ownership[1].accepted);
        let isolated_used = 1_000_000 - isolated.remaining_work;
        for budget in 0..=isolated_used {
            let (partial, _, owners) = run(budget, 100, 7)?;
            assert!(partial.records.len() <= isolated.records.len());
            assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
            assert_eq!(partial.records.len() - 1, owners[0].accepted.len());
        }
        for cap in 0..=2 {
            let (held, _, owners) = run(1_000_000, cap, 0)?;
            assert_eq!(held.records.len(), 1);
            assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
            assert_eq!(held.remaining_work, 1_000_000);
        }
        for refusal in (1..=6).chain(8..=12) {
            let (held, _, owners) = run(1_000_000, 100, refusal)?;
            assert_eq!(held.records.len(), 1);
            assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        }
        Ok(())
    }

    #[test]
    fn h11_complete_parent_keeps_repeated_mates_candidates_and_ancestors_held() -> Result<()> {
        for (old_text, new_text, expected) in [
            ("A22B", "A2B", vec![(0, 1, 0, 1), (3, 4, 2, 3)]),
            ("AAB", "AB", vec![(2, 3, 1, 2)]),
        ] {
            let old_blocks = [anchor_block(1, old_text)];
            let new_blocks = [anchor_block(101, new_text)];
            let old = super::super::SidePlan::inspect("H11 old", &old_blocks)?.materialize()?;
            let new = super::super::SidePlan::inspect("H11 new", &new_blocks)?.materialize()?;
            let alignment = Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            for veto in 0..4 {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                let mut domains = std::mem::take(&mut assessor.domains)
                    .into_iter()
                    .collect::<Vec<_>>();
                if veto == 1 {
                    // The actual direct-parent witness has an incomplete ancestor.
                    let mut ancestor = assessor.records[0].clone();
                    ancestor.outcome = RelationOutcome::Tentative;
                    ancestor.search = SearchCompleteness::Incomplete;
                    ancestor.reasons = vec![AssessmentReason::WorkLimit];
                    assessor.records.insert(0, ancestor);
                    assessor.records[1].parent = Some(0);
                    domains[0].1.relation = 1;
                }
                let parent = &assessor.records[domains[0].1.relation];
                let region = ProvenChangedRegion {
                    old_span: parent.old_span.clone(),
                    new_span: parent.new_span.clone(),
                    confidence: super::super::Confidence::High,
                    proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                };
                let mut candidates = Vec::new();
                if veto == 2 {
                    // Candidate locations still veto even mathematically mandatory A.
                    let mut a = parent
                        .old_span
                        .clone()
                        .expect("the complete fixture has both source spans");
                    a.canonical_range = ScalarRange { start: 0, end: 1 };
                    a.comparable_range = TokenRange { start: 0, end: 1 };
                    let mut b = parent
                        .new_span
                        .clone()
                        .expect("the complete fixture has both source spans");
                    b.canonical_range = ScalarRange { start: 0, end: 1 };
                    b.comparable_range = TokenRange { start: 0, end: 1 };
                    candidates.push(ChangeCandidate {
                        change: super::super::Change::single_occurrence(
                            ChangeKind::Replacement,
                            Some(a),
                            Some(b),
                            super::super::Confidence::Low,
                            Vec::new(),
                        ),
                        relation: 0,
                        alternative_group: 0,
                    });
                }
                if veto == 3 {
                    assessor.output_stop = Some(0);
                }
                let mut partitions = [
                    h8_partition(
                        BlockId(1),
                        &[(0, old_text.len(), ResolutionState::Unresolved)],
                    ),
                    h8_partition(
                        BlockId(101),
                        &[(0, new_text.len(), ResolutionState::Unresolved)],
                    ),
                ];
                let mut owners = [Ownership::new(), Ownership::new()];
                let records_before = assessor.records.clone();
                let candidates_before = candidates.clone();
                let [a, b] = &mut partitions;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut owners,
                    [a, b],
                    mandatory_equal::EqualConstraints {
                        candidates: &candidates,
                        regions: &[region],
                        original: &[],
                    },
                )?;
                assert_eq!(&assessor.records[..records_before.len()], records_before);
                assert_eq!(candidates, candidates_before);
                let actual = assessor.records[records_before.len()..]
                    .iter()
                    .map(|child| {
                        let a = child
                            .old_span
                            .as_ref()
                            .expect("the complete fixture has both source spans")
                            .comparable_range;
                        let b = child
                            .new_span
                            .as_ref()
                            .expect("the complete fixture has both source spans")
                            .comparable_range;
                        (a.start, a.end, b.start, b.end)
                    })
                    .collect::<Vec<_>>();
                let wanted = if veto == 1 || veto == 3 {
                    Vec::new()
                } else if veto == 2 {
                    expected
                        .iter()
                        .copied()
                        .filter(|pair| pair.0 != 0)
                        .collect()
                } else {
                    expected.clone()
                };
                assert_eq!(actual, wanted, "{old_text}->{new_text}, veto {veto}");
                assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
            }
        }
        Ok(())
    }

    #[test]
    fn unresolved_fragments_keep_ordered_evidence_across_block_revisits() -> Result<()> {
        let old_blocks = [anchor_block(1, "ABCDEF"), anchor_block(2, "GH")];
        let new_blocks = [anchor_block(101, "IJKLMN")];
        let old = super::super::SidePlan::inspect("fragment old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("fragment new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].old = vec![BlockId(1), BlockId(2)];
        alignment.spans[0].evidence = vec![
            AlignmentEvidence::ReadingOrderUnknown,
            AlignmentEvidence::ExactCanonical,
            AlignmentEvidence::ReadingOrderUnknown,
        ];
        let mut duplicate = alignment.spans[0].clone();
        duplicate.old = vec![BlockId(2), BlockId(1)];
        duplicate.evidence = vec![
            AlignmentEvidence::NormalizationIssue,
            AlignmentEvidence::ExactCanonical,
        ];
        let mut other = duplicate.clone();
        other.old = vec![BlockId(2)];
        other.evidence = vec![AlignmentEvidence::ReadingOrderInferred];
        alignment.spans.extend([duplicate, other]);
        let mut old_parts = h8_partition(BlockId(1), &[(0, 3, ResolutionState::Unresolved)]);
        old_parts.extend(h8_partition(
            BlockId(2),
            &[(0, 2, ResolutionState::Unresolved)],
        ));
        old_parts.extend(h8_partition(
            BlockId(1),
            &[(3, 6, ResolutionState::Unresolved)],
        ));
        let new_parts = h8_partition(BlockId(101), &[(0, 6, ResolutionState::Unresolved)]);
        let original = vec![
            UnresolvedRegion {
                old_span: Some(local_span(1, 1, 2)),
                new_span: None,
                evidence: vec![AlignmentEvidence::CandidateCompetition],
            },
            UnresolvedRegion {
                old_span: Some(local_span(1, 4, 5)),
                new_span: None,
                evidence: Vec::new(),
            },
            UnresolvedRegion {
                old_span: None,
                new_span: Some(local_span(101, 2, 3)),
                evidence: Vec::new(),
            },
            UnresolvedRegion {
                old_span: None,
                new_span: None,
                evidence: vec![AlignmentEvidence::ExtractionGap],
            },
        ];
        let block_evidence = vec![
            AlignmentEvidence::ReadingOrderUnknown,
            AlignmentEvidence::ExactCanonical,
            AlignmentEvidence::NormalizationIssue,
        ];
        let mut other_evidence = block_evidence.clone();
        other_evidence.push(AlignmentEvidence::ReadingOrderInferred);
        let mut expected = original.clone();
        for (block, start, end) in [(1, 0, 1), (1, 2, 3), (2, 0, 2), (1, 3, 4), (1, 5, 6)] {
            expected.push(UnresolvedRegion {
                old_span: Some(local_span(block, start, end)),
                new_span: None,
                evidence: if block == 1 {
                    block_evidence.clone()
                } else {
                    other_evidence.clone()
                },
            });
        }
        for (start, end) in [(0, 2), (3, 6)] {
            expected.push(UnresolvedRegion {
                old_span: None,
                new_span: Some(local_span(101, start, end)),
                evidence: other_evidence.clone(),
            });
        }
        assert_eq!(
            unresolved_output(
                [&old, &new],
                &alignment,
                [&old_parts, &new_parts],
                original.clone(),
                11
            )?,
            expected
        );
        assert!(matches!(
            unresolved_output(
                [&old, &new],
                &alignment,
                [&old_parts, &new_parts],
                original,
                10
            ),
            Err(Error::LimitExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn repeated_alignment_diagnostics_leave_work_for_physical_equalities() -> Result<()> {
        let old_blocks = [anchor_block(1, "XABY")];
        let new_blocks = [anchor_block(101, "ZABW")];
        let old = super::super::SidePlan::inspect("budget old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("budget new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].evidence = vec![AlignmentEvidence::ReadingOrderInferred];
        alignment.spans = vec![alignment.spans[0].clone(); 100];
        let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
        assessor.remaining_work = 40_000;
        let domains = std::mem::take(&mut assessor.domains)
            .into_iter()
            .collect::<Vec<_>>();
        let parent = assessor.records[0].clone();
        let original = vec![UnresolvedRegion {
            old_span: parent.old_span.clone(),
            new_span: parent.new_span.clone(),
            evidence: Vec::new(),
        }];
        let coarse = vec![ProvenChangedRegion {
            old_span: parent.old_span.clone(),
            new_span: parent.new_span.clone(),
            confidence: super::super::Confidence::High,
            proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
        }];
        let mut parts = [
            h8_partition(BlockId(1), &[(0, 4, ResolutionState::Unresolved)]),
            h8_partition(BlockId(101), &[(0, 4, ResolutionState::Unresolved)]),
        ];
        let mut owners = [Ownership::new(), Ownership::new()];
        let [a, b] = &mut parts;
        assessor.recover_mandatory_coarse_equalities(
            &domains,
            domains.capacity(),
            &mut owners,
            [a, b],
            mandatory_equal::EqualConstraints {
                candidates: &[],
                regions: &coarse,
                original: &original,
            },
        )?;
        assert_eq!(assessor.records[0], parent);
        for (side, block) in [(0, BlockId(1)), (1, BlockId(101))] {
            let equal = parts[side]
                .iter()
                .filter(|part| part.state == ResolutionState::Equal)
                .collect::<Vec<_>>();
            assert_eq!(equal.len(), 1, "physical AB equality on side {side}");
            assert_eq!(equal[0].block, block);
            assert_eq!(equal[0].comparable_range, TokenRange { start: 1, end: 3 });
            assert_eq!(owners[side].accepted.len(), 1);
        }
        assert!(assessor.remaining_work > 0);
        Ok(())
    }

    #[test]
    fn h11_optional_diagnostic_limit_preserves_the_baseline_unresolved_region() -> Result<()> {
        let old_blocks = [anchor_block(1, "XABY")];
        let new_blocks = [anchor_block(101, "ZABW")];
        let old = super::super::SidePlan::inspect("H11 old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("H11 new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
        assessor.options.max_assessment_ranges = 3;
        let domains = std::mem::take(&mut assessor.domains)
            .into_iter()
            .collect::<Vec<_>>();
        let parent = assessor.records[0].clone();
        let original = vec![UnresolvedRegion {
            old_span: parent.old_span.clone(),
            new_span: parent.new_span.clone(),
            evidence: Vec::new(),
        }];
        let coarse = vec![ProvenChangedRegion {
            old_span: parent.old_span.clone(),
            new_span: parent.new_span.clone(),
            confidence: super::super::Confidence::High,
            proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
        }];
        let mut parts = [
            h8_partition(BlockId(1), &[(0, 4, ResolutionState::Unresolved)]),
            h8_partition(BlockId(101), &[(0, 4, ResolutionState::Unresolved)]),
        ];
        let baseline = unresolved_output(
            [&old, &new],
            &alignment,
            [&parts[0], &parts[1]],
            original.clone(),
            3,
        )?;
        assert_eq!(baseline.len(), 1);
        // AB's mandatory equality would require four unilateral residuals,
        // exceeding the output cap despite room for a relation certificate.
        let mut owners = [Ownership::new(), Ownership::new()];
        let [a, b] = &mut parts;
        assessor.recover_mandatory_coarse_equalities(
            &domains,
            domains.capacity(),
            &mut owners,
            [a, b],
            mandatory_equal::EqualConstraints {
                candidates: &[],
                regions: &coarse,
                original: &original,
            },
        )?;
        assert_eq!(assessor.records, vec![parent]);
        assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        let output = unresolved_output(
            [&old, &new],
            &alignment,
            [&parts[0], &parts[1]],
            original,
            3,
        )?;
        assert_eq!(output, baseline);
        Ok(())
    }

    #[test]
    fn h14_final_singletons_preserve_maximal_proofs_and_ligature_refusals() -> Result<()> {
        // One decoded ligature owns both f/i scalars. CD and KL have distinct
        // literal singleton glyphs; only the fiCD maximal run lacks a source cut.
        let mut old_blocks = [anchor_block(1, "XfiCD KL Y")];
        let mut new_blocks = [anchor_block(101, "ZfiCD KL W")];
        for block in [&mut old_blocks[0], &mut new_blocks[0]] {
            block.canonical.source_map[1].output_range = ScalarRange { start: 1, end: 3 };
            block.canonical.source_map.remove(2);
            block.raw = block.canonical.clone();
        }
        let old =
            super::super::SidePlan::inspect("H14 ligature old", &old_blocks)?.materialize()?;
        let new =
            super::super::SidePlan::inspect("H14 ligature new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run =
            |budget,
             cap,
             veto|
             -> Result<(Assessor<'_, '_>, [Vec<ResolutionRange>; 2], [Ownership; 2])> {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                assessor.remaining_work = budget;
                assessor.options.max_assessment_ranges = cap;
                let domains = std::mem::take(&mut assessor.domains)
                    .into_iter()
                    .collect::<Vec<_>>();
                if veto == 2 {
                    assessor.records[0]
                        .reasons
                        .push(AssessmentReason::NormalizationUncertainty);
                }
                let parent = assessor.records[0].clone();
                let mut candidates = Vec::new();
                if veto == 1 {
                    let mut old = parent.old_span.clone().expect("paired parent");
                    let mut new = parent.new_span.clone().expect("paired parent");
                    for span in [&mut old, &mut new] {
                        span.comparable_range = TokenRange { start: 3, end: 4 };
                        span.canonical_range = ScalarRange { start: 3, end: 4 };
                    }
                    candidates.push(ChangeCandidate {
                        change: super::super::Change::single_occurrence(
                            ChangeKind::Replacement,
                            Some(old),
                            Some(new),
                            super::super::Confidence::Low,
                            Vec::new(),
                        ),
                        relation: 0,
                        alternative_group: 0,
                    });
                }
                let candidates_before = candidates.clone();
                let coarse = [ProvenChangedRegion {
                    old_span: parent.old_span.clone(),
                    new_span: parent.new_span.clone(),
                    confidence: super::super::Confidence::High,
                    proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                }];
                let mut parts = [
                    h8_partition(BlockId(1), &[(0, 10, ResolutionState::Unresolved)]),
                    h8_partition(BlockId(101), &[(0, 10, ResolutionState::Unresolved)]),
                ];
                let mut owners = [Ownership::new(), Ownership::new()];
                let [a, b] = &mut parts;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut owners,
                    [a, b],
                    mandatory_equal::EqualConstraints {
                        candidates: &candidates,
                        regions: &coarse,
                        original: &[],
                    },
                )?;
                assert_eq!(assessor.records[0], parent);
                assert_eq!(candidates, candidates_before);
                assert!(assessor.semantic_acceptance.is_empty());
                assert!(assessor.localized_edits.is_empty());
                let actual = assessor.records[1..]
                    .iter()
                    .map(|child| {
                        assert_eq!(child.outcome, RelationOutcome::Established);
                        assert_eq!(child.search, SearchCompleteness::Complete);
                        assert!(child.reasons.is_empty());
                        let old = child.old_span.as_ref().expect("paired certificate");
                        let new = child.new_span.as_ref().expect("paired certificate");
                        assert_eq!(old.comparable_range, new.comparable_range);
                        (old.comparable_range.start, old.comparable_range.end)
                    })
                    .collect::<Vec<_>>();
                // Literal source gold: KL is attempted by the original full pass;
                // later C,D are safe singleton cuts, f/i remain an indivisible glyph.
                assert!(
                    actual
                        .iter()
                        .all(|range| [(6, 8), (3, 4), (4, 5)].contains(range))
                );
                assert!(actual.is_empty() || actual[0] == (6, 8));
                assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
                assert_eq!(owners[0].accepted.len(), actual.len());
                Ok((assessor, parts, owners))
            };
        let (full, parts, _) = run(1_000_000, 100, 0)?;
        let actual = full.records[1..]
            .iter()
            .map(|child| {
                let range = child
                    .old_span
                    .as_ref()
                    .expect("paired certificate")
                    .comparable_range;
                (range.start, range.end)
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, vec![(6, 8), (3, 4), (4, 5)]);
        for side in &parts {
            assert!(
                side.iter()
                    .any(|part| part.state == ResolutionState::Unresolved
                        && part.comparable_range.start <= 1
                        && part.comparable_range.end >= 3)
            );
        }
        let used = 1_000_000 - full.remaining_work;
        for budget in (0..=used).step_by(97).chain([used.saturating_sub(1), used]) {
            let (partial, _, owners) = run(budget, 100, 0)?;
            assert!(partial.records.len() <= full.records.len());
            assert_eq!(partial.records.len() - 1, owners[0].accepted.len());
        }
        for cap in 0..=2 {
            let (held, _, owners) = run(1_000_000, cap, 0)?;
            assert_eq!(held.records.len(), 1);
            assert_eq!(held.remaining_work, 1_000_000);
            assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        }
        let (candidate_veto, _, _) = run(1_000_000, 100, 1)?;
        assert_eq!(candidate_veto.records.len(), 2); // KL only; C candidate prevents the whole-run source probe.
        let (dirty, _, owners) = run(1_000_000, 100, 2)?;
        assert_eq!(dirty.records.len(), 1);
        assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        Ok(())
    }

    #[test]
    fn h14_final_singletons_keep_deleted_break_edges_and_shared_sources_unresolved() -> Result<()> {
        use crate::normalize::{NormalizationEvent, NormalizationKind};
        for shared in [false, true] {
            let mut old_blocks = [anchor_block(1, "XABCDEY")];
            let mut new_blocks = [anchor_block(101, "ZABCDEW")];
            for block in [&mut old_blocks[0], &mut new_blocks[0]] {
                if shared {
                    // B/C name the same glyph twice. No singleton may own it.
                    let source = block.canonical.source_map[2].source.clone();
                    block.canonical.source_map[3].source = source;
                    block.raw = block.canonical.clone();
                } else {
                    // The deleted raw break is between B/C; its endpoints and
                    // widened normalization cut must remain held on both sides.
                    let [TextSourceAtom::Glyph(before)] =
                        block.canonical.source_map[2].source.atoms.as_slice()
                    else {
                        panic!("fixture glyph")
                    };
                    let [TextSourceAtom::Glyph(after)] =
                        block.canonical.source_map[3].source.atoms.as_slice()
                    else {
                        panic!("fixture glyph")
                    };
                    let source = TextSource {
                        atoms: vec![TextSourceAtom::LineBreak {
                            preceding: *before,
                            following: *after,
                        }]
                        .into(),
                    };
                    block.raw.text.insert(3, '\n');
                    for entry in &mut block.raw.source_map[3..] {
                        entry.output_range.start += 1;
                        entry.output_range.end += 1;
                    }
                    block.raw.source_map.insert(
                        3,
                        SourceMapEntry {
                            output_range: ScalarRange { start: 3, end: 4 },
                            source: source.clone(),
                        },
                    );
                    block.normalization_events.push(NormalizationEvent {
                        kind: NormalizationKind::SoftLineBreak,
                        raw_range: ScalarRange { start: 3, end: 4 },
                        canonical_range: ScalarRange { start: 3, end: 3 },
                        source,
                    });
                }
            }
            let old =
                super::super::SidePlan::inspect("H14 boundary old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H14 boundary new", &new_blocks)?.materialize()?;
            let alignment = Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            let domains = std::mem::take(&mut assessor.domains)
                .into_iter()
                .collect::<Vec<_>>();
            let parent = assessor.records[0].clone();
            let coarse = [ProvenChangedRegion {
                old_span: parent.old_span.clone(),
                new_span: parent.new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            }];
            let mut parts = [
                h8_partition(BlockId(1), &[(0, 7, ResolutionState::Unresolved)]),
                h8_partition(BlockId(101), &[(0, 7, ResolutionState::Unresolved)]),
            ];
            let mut owners = [Ownership::new(), Ownership::new()];
            let [a, b] = &mut parts;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut owners,
                [a, b],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &coarse,
                    original: &[],
                },
            )?;
            assert_eq!(assessor.records[0], parent);
            let ranges = assessor.records[1..]
                .iter()
                .map(|child| {
                    let old = child
                        .old_span
                        .as_ref()
                        .expect("paired equality")
                        .comparable_range;
                    let new = child
                        .new_span
                        .as_ref()
                        .expect("paired equality")
                        .comparable_range;
                    assert_eq!(old, new);
                    (old.start, old.end)
                })
                .collect::<Vec<_>>();
            assert_eq!(ranges, vec![(1, 2), (4, 5), (5, 6)], "shared={shared}");
            for side in &parts {
                assert!(
                    side.iter()
                        .any(|part| part.state == ResolutionState::Unresolved
                            && part.comparable_range.start <= 2
                            && part.comparable_range.end >= 4)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn h27_necessary_pages_come_from_actual_singleton_key_blocks() -> Result<()> {
        let run = |old_pages: &[u32],
                   new_pages: &[u32],
                   old_range: Range<usize>,
                   new_range: Range<usize>|
         -> Result<bool> {
            let mut old_blocks = [anchor_block(1, "X"), anchor_block(2, "A")];
            let mut new_blocks = [anchor_block(101, "Y"), anchor_block(102, "B")];
            // Unrelated first blocks deliberately suggest the opposite result.
            old_blocks[0].pages = vec![10];
            new_blocks[0].pages = vec![11];
            old_blocks[1].pages = old_pages.to_vec();
            new_blocks[1].pages = new_pages.to_vec();
            let old =
                super::super::SidePlan::inspect("H27 old headers", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H27 new headers", &new_blocks)?.materialize()?;
            let key = DomainKey {
                local: None,
                old: old_range,
                new: new_range,
                old_separator: BlockSeparator::Space,
                new_separator: BlockSeparator::Space,
            };
            Ok(mandatory_equal::page_shift_key_pages_differ(
                [&old, &new],
                &key,
            ))
        };
        assert!(!run(&[7], &[7], 1..2, 1..2)?);
        assert!(run(&[7], &[8], 1..2, 1..2)?);
        assert!(run(&[0], &[1], 1..2, 1..2)?);
        for (old_pages, new_pages) in [
            (&[][..], &[8][..]),
            (&[7][..], &[][..]),
            (&[7, 9][..], &[8][..]),
            (&[7][..], &[8, 9][..]),
        ] {
            assert!(!run(old_pages, new_pages, 1..2, 1..2)?);
        }
        for range in [0..2, 1..1, 2..3, Range { start: 2, end: 1 }] {
            assert!(!run(&[7], &[8], range.clone(), 1..2)?);
            assert!(!run(&[7], &[8], 1..2, range)?);
        }
        Ok(())
    }

    #[test]
    fn h27_page_necessity_funds_late_original_gold_after_same_page_coarse_roots() -> Result<()> {
        type Outcome = (
            Vec<RelationAssessment>,
            [Vec<ResolutionRange>; 2],
            [Ownership; 2],
            usize,
        );
        const ROOTS: usize = 33;
        let old_blocks = (0..ROOTS)
            .map(|i| {
                anchor_block(
                    (i + 1) as u64,
                    if i == 0 {
                        "K22L"
                    } else if i + 1 == ROOTS {
                        "A22B"
                    } else {
                        "X"
                    },
                )
            })
            .collect::<Vec<_>>();
        let new_blocks = (0..ROOTS)
            .map(|i| {
                let mut block = anchor_block(
                    (i + 101) as u64,
                    if i == 0 {
                        "K2L"
                    } else if i + 1 == ROOTS {
                        "A2B"
                    } else {
                        "Y"
                    },
                );
                block.pages = vec![u32::from(i + 1 == ROOTS)];
                block
            })
            .collect::<Vec<_>>();
        let old = super::super::SidePlan::inspect("H27 old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("H27 new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run = |budget, cap, veto| -> Result<Outcome> {
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            assessor.records.clear();
            assessor.domains.clear();
            assessor.remaining_work = budget;
            assessor.options.max_assessment_ranges = cap;
            let mut domains = Vec::new();
            for index in 0..ROOTS {
                let spans = [
                    old.canonical_group(&[BlockId((index + 1) as u64)], None)
                        .full_span(),
                    new.canonical_group(&[BlockId((index + 101) as u64)], None)
                        .full_span(),
                ];
                let lengths = [spans[0].comparable_range.end, spans[1].comparable_range.end];
                assessor.records.push(RelationAssessment {
                    old_span: Some(spans[0].clone()),
                    new_span: Some(spans[1].clone()),
                    parent: None,
                    outcome: RelationOutcome::Established,
                    search: SearchCompleteness::Complete,
                    assumptions: vec![
                        ComparisonAssumption::InputReadingOrder,
                        ComparisonAssumption::CanonicalNormalization,
                        ComparisonAssumption::LocalEvidenceBoundaries,
                    ],
                    reasons: Vec::new(),
                });
                domains.push((
                    DomainKey {
                        local: None,
                        old: index..index + 1,
                        new: index..index + 1,
                        old_separator: BlockSeparator::Space,
                        new_separator: BlockSeparator::Space,
                    },
                    DomainProof {
                        scope: ProofScope::ExactKey,
                        relation: index,
                        unique: false,
                        search: SearchCompleteness::Complete,
                        edits: Vec::new(),
                        lengths,
                        strict_unique: false,
                        stable_events: None,
                    },
                ));
            }
            let region_parents = assessor.records.clone();
            if veto == 7 {
                // A named parent cannot borrow page metadata from a different
                // actual key block: the unchanged isolation check must hold it.
                assessor.records[ROOTS - 1]
                    .new_span
                    .as_mut()
                    .expect("paired")
                    .blocks[0] = BlockId(101);
            }
            let parents = assessor.records.clone();
            let region = |index: usize| ProvenChangedRegion {
                old_span: region_parents[index].old_span.clone(),
                new_span: region_parents[index].new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            };
            // Every earlier clean root has a genuine same-page whole coarse
            // region, so H26 presence passes but its full page-shift guard fails.
            let mut regions = (0..ROOTS - 1).map(region).collect::<Vec<_>>();
            let mut target = region(ROOTS - 1);
            match veto {
                1 => {
                    target
                        .old_span
                        .as_mut()
                        .expect("paired")
                        .comparable_range
                        .start = 1;
                }
                2 => {
                    target.new_span.as_mut().expect("paired").separator =
                        Some(BlockSeparator::Space);
                }
                3 => {
                    target.new_span = None;
                }
                4 => {
                    target
                        .new_span
                        .as_mut()
                        .expect("paired")
                        .blocks
                        .push(BlockId(101));
                }
                5 => {
                    target
                        .new_span
                        .as_mut()
                        .expect("paired")
                        .canonical_range
                        .end = 2;
                }
                6 => {
                    target.proof = super::super::ChangedRegionProof::OneSidedNonEmptyRange;
                }
                _ => {}
            }
            regions.push(target);
            let regions_before = regions.clone();
            let mut parts = [
                (0..ROOTS)
                    .flat_map(|i| {
                        h8_partition(
                            BlockId((i + 1) as u64),
                            &[(
                                0,
                                if i == 0 || i + 1 == ROOTS { 4 } else { 1 },
                                ResolutionState::Unresolved,
                            )],
                        )
                    })
                    .collect::<Vec<_>>(),
                (0..ROOTS)
                    .flat_map(|i| {
                        h8_partition(
                            BlockId((i + 101) as u64),
                            &[(
                                0,
                                if i == 0 || i + 1 == ROOTS { 3 } else { 1 },
                                ResolutionState::Unresolved,
                            )],
                        )
                    })
                    .collect::<Vec<_>>(),
            ];
            let mut owners = [Ownership::new(), Ownership::new()];
            let [a, b] = &mut parts;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut owners,
                [a, b],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &regions,
                    original: &[],
                },
            )?;
            assert_eq!(assessor.records[..ROOTS], parents);
            assert_eq!(regions, regions_before);
            assert!(assessor.semantic_acceptance.is_empty());
            assert!(assessor.localized_edits.is_empty());
            let gold = [
                (
                    TokenRange { start: 0, end: 1 },
                    TokenRange { start: 0, end: 1 },
                ),
                (
                    TokenRange { start: 3, end: 4 },
                    TokenRange { start: 2, end: 3 },
                ),
            ];
            for child in &assessor.records[ROOTS..] {
                let page_shifted = child.parent == Some(ROOTS - 1);
                assert!(page_shifted || child.parent == Some(0));
                assert_eq!(child.outcome, RelationOutcome::Established);
                assert_eq!(child.search, SearchCompleteness::Complete);
                assert!(child.reasons.is_empty());
                assert_eq!(
                    child
                        .assumptions
                        .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality),
                    page_shifted
                );
                let old = child.old_span.as_ref().expect("paired");
                let new = child.new_span.as_ref().expect("paired");
                assert_eq!(
                    old.blocks,
                    [BlockId(if page_shifted { ROOTS as u64 } else { 1 })]
                );
                assert_eq!(
                    new.blocks,
                    [BlockId(if page_shifted {
                        (ROOTS + 100) as u64
                    } else {
                        101
                    })]
                );
                assert!(gold.contains(&(old.comparable_range, new.comparable_range)));
            }
            assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
            assert_eq!(owners[0].accepted.len(), assessor.records.len() - ROOTS);
            Ok((assessor.records, parts, owners, assessor.remaining_work))
        };
        let (full, parts, owners, remaining) = run(1_000_000, 1000, 0)?;
        // Independent original-parent matching gold: A/B are mandatory;
        // either old '2' can match the sole new '2', so both old digits stay U.
        assert_eq!(full.len(), ROOTS + 4);
        assert_eq!(full[ROOTS].parent, Some(0));
        assert_eq!(full[ROOTS + 1].parent, Some(0));
        assert_eq!(full[ROOTS + 2].parent, Some(ROOTS - 1));
        assert_eq!(full[ROOTS + 3].parent, Some(ROOTS - 1));
        assert_eq!(
            full[ROOTS]
                .old_span
                .as_ref()
                .expect("paired")
                .comparable_range,
            TokenRange { start: 0, end: 1 }
        );
        assert_eq!(
            full[ROOTS + 1]
                .old_span
                .as_ref()
                .expect("paired")
                .comparable_range,
            TokenRange { start: 3, end: 4 }
        );
        assert_eq!(owners[0].accepted.len(), 4);
        assert!(parts[0].iter().any(|p| p.block == BlockId(ROOTS as u64)
            && p.state == ResolutionState::Unresolved
            && p.comparable_range.start <= 1
            && p.comparable_range.end >= 3));
        assert!(
            parts[1]
                .iter()
                .any(|p| p.block == BlockId((ROOTS + 100) as u64)
                    && p.state == ResolutionState::Unresolved
                    && p.comparable_range.start <= 1
                    && p.comparable_range.end >= 2)
        );
        // The earlier same-page K/L proof remains first and its repeated digits
        // also remain unresolved under the independent original-parent gold.
        for (side, block, end) in [(0, BlockId(1), 3), (1, BlockId(101), 2)] {
            assert!(parts[side].iter().any(|p| p.block == block
                && p.state == ResolutionState::Unresolved
                && p.comparable_range.start <= 1
                && p.comparable_range.end >= end));
        }
        let used = 1_000_000 - remaining;
        // The old H26 page-shift region fee alone, before the late parent,
        // exceeded this complete fixture budget. Every same-page root passes
        // isolation/length/presence, but none can pass the unchanged page guard.
        let old_impossible_scan_fee = (ROOTS - 1) * (ROOTS * (3 * 3 + 64) + ROOTS);
        assert!(used < old_impossible_scan_fee);
        for budget in
            (0..=used)
                .step_by((used / 24).max(1))
                .chain([15, 16, used.saturating_sub(1), used])
        {
            let (partial, _, owners, _) = run(budget, 1000, 0)?;
            assert_eq!(partial[ROOTS..], full[ROOTS..partial.len()]);
            assert_eq!(owners[0].accepted.len(), partial.len() - ROOTS);
        }
        for veto in 1..=7 {
            let (held, _, owners, _) = run(1_000_000, 1000, veto)?;
            // Unprojectable other-coarse obligations fail closed globally:
            // even the otherwise valid earlier K/L cannot be published.
            let prior = if matches!(veto, 1 | 2 | 4 | 5) { 0 } else { 2 };
            assert_eq!(held.len(), ROOTS + prior, "veto={veto}");
            assert!(owners.iter().all(|o| o.accepted.len() == prior));
            assert!(held[ROOTS..].iter().all(|child| child.parent == Some(0)));
        }
        for cap in 0..=ROOTS + 1 {
            let (held, _, owners, remaining) = run(1_000_000, cap, 0)?;
            assert_eq!(held.len(), ROOTS);
            assert_eq!(remaining, 1_000_000);
            assert!(owners.iter().all(|o| o.accepted.is_empty()));
        }
        Ok(())
    }

    #[test]
    fn h26_coarse_presence_keeps_original_parent_gold_and_all_refusal_shapes() -> Result<()> {
        type Outcome = (
            Vec<RelationAssessment>,
            [Vec<ResolutionRange>; 2],
            [Ownership; 2],
            usize,
        );
        const ROOTS: usize = 13;
        let old_blocks = (0..ROOTS)
            .map(|i| anchor_block((i + 1) as u64, if i + 1 == ROOTS { "A22B" } else { "X" }))
            .collect::<Vec<_>>();
        let new_blocks = (0..ROOTS)
            .map(|i| {
                let mut block =
                    anchor_block((i + 101) as u64, if i + 1 == ROOTS { "A2B" } else { "Y" });
                block.pages = vec![1];
                block
            })
            .collect::<Vec<_>>();
        let old = super::super::SidePlan::inspect("H26 old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("H26 new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run = |budget, cap, veto| -> Result<Outcome> {
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            assessor.records.clear();
            assessor.domains.clear();
            assessor.remaining_work = budget;
            assessor.options.max_assessment_ranges = cap;
            let mut domains = Vec::new();
            for index in 0..ROOTS {
                let spans = [
                    old.canonical_group(&[BlockId((index + 1) as u64)], None)
                        .full_span(),
                    new.canonical_group(&[BlockId((index + 101) as u64)], None)
                        .full_span(),
                ];
                let lengths = [spans[0].comparable_range.end, spans[1].comparable_range.end];
                assessor.records.push(RelationAssessment {
                    old_span: Some(spans[0].clone()),
                    new_span: Some(spans[1].clone()),
                    parent: None,
                    outcome: RelationOutcome::Established,
                    search: SearchCompleteness::Complete,
                    assumptions: vec![
                        ComparisonAssumption::InputReadingOrder,
                        ComparisonAssumption::CanonicalNormalization,
                        ComparisonAssumption::LocalEvidenceBoundaries,
                    ],
                    reasons: Vec::new(),
                });
                domains.push((
                    DomainKey {
                        local: None,
                        old: index..index + 1,
                        new: index..index + 1,
                        old_separator: BlockSeparator::Space,
                        new_separator: BlockSeparator::Space,
                    },
                    DomainProof {
                        scope: ProofScope::ExactKey,
                        relation: index,
                        unique: false,
                        search: SearchCompleteness::Complete,
                        edits: Vec::new(),
                        lengths,
                        strict_unique: false,
                        stable_events: None,
                    },
                ));
            }
            let parents = assessor.records.clone();
            let region = |index: usize| ProvenChangedRegion {
                old_span: parents[index].old_span.clone(),
                new_span: parents[index].new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            };
            // Eight valid unrelated obligations exercise the header walk. The
            // other eleven clean roots have no matching region on either side.
            let mut regions = vec![region(0); 8];
            let mut target = region(ROOTS - 1);
            match veto {
                1 => {
                    target
                        .old_span
                        .as_mut()
                        .expect("paired")
                        .comparable_range
                        .start = 1;
                }
                2 => {
                    target.new_span.as_mut().expect("paired").separator =
                        Some(BlockSeparator::Space);
                }
                3 => target.new_span = None,
                4 => target
                    .new_span
                    .as_mut()
                    .expect("paired")
                    .blocks
                    .push(BlockId(101)),
                5 => {
                    target
                        .new_span
                        .as_mut()
                        .expect("paired")
                        .canonical_range
                        .end = 2;
                }
                6 => target.proof = super::super::ChangedRegionProof::OneSidedNonEmptyRange,
                _ => {}
            }
            regions.push(target);
            let regions_before = regions.clone();
            let mut parts = [
                (0..ROOTS)
                    .flat_map(|i| {
                        h8_partition(
                            BlockId((i + 1) as u64),
                            &[(
                                0,
                                if i + 1 == ROOTS { 4 } else { 1 },
                                ResolutionState::Unresolved,
                            )],
                        )
                    })
                    .collect::<Vec<_>>(),
                (0..ROOTS)
                    .flat_map(|i| {
                        h8_partition(
                            BlockId((i + 101) as u64),
                            &[(
                                0,
                                if i + 1 == ROOTS { 3 } else { 1 },
                                ResolutionState::Unresolved,
                            )],
                        )
                    })
                    .collect::<Vec<_>>(),
            ];
            let mut owners = [Ownership::new(), Ownership::new()];
            let [a, b] = &mut parts;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut owners,
                [a, b],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &regions,
                    original: &[],
                },
            )?;
            assert_eq!(assessor.records[..ROOTS], parents);
            assert_eq!(regions, regions_before);
            assert!(assessor.semantic_acceptance.is_empty());
            assert!(assessor.localized_edits.is_empty());
            let gold = [
                (
                    TokenRange { start: 0, end: 1 },
                    TokenRange { start: 0, end: 1 },
                ),
                (
                    TokenRange { start: 3, end: 4 },
                    TokenRange { start: 2, end: 3 },
                ),
            ];
            for child in &assessor.records[ROOTS..] {
                assert_eq!(child.parent, Some(ROOTS - 1));
                assert_eq!(child.outcome, RelationOutcome::Established);
                assert_eq!(child.search, SearchCompleteness::Complete);
                assert!(child.reasons.is_empty());
                assert!(
                    child
                        .assumptions
                        .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality)
                );
                let old = child.old_span.as_ref().expect("paired");
                let new = child.new_span.as_ref().expect("paired");
                assert_eq!(old.blocks, [BlockId(ROOTS as u64)]);
                assert_eq!(new.blocks, [BlockId((ROOTS + 100) as u64)]);
                assert!(gold.contains(&(old.comparable_range, new.comparable_range)));
            }
            assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
            assert_eq!(owners[0].accepted.len(), assessor.records.len() - ROOTS);
            Ok((assessor.records, parts, owners, assessor.remaining_work))
        };
        let (full, parts, owners, remaining) = run(1_000_000, 1000, 0)?;
        // Independent original-parent matching gold: A/B are mandatory;
        // either old '2' can match the sole new '2', so both old digits stay U.
        assert_eq!(full.len(), ROOTS + 2);
        assert_eq!(
            full[ROOTS]
                .old_span
                .as_ref()
                .expect("paired")
                .comparable_range,
            TokenRange { start: 0, end: 1 }
        );
        assert_eq!(
            full[ROOTS + 1]
                .old_span
                .as_ref()
                .expect("paired")
                .comparable_range,
            TokenRange { start: 3, end: 4 }
        );
        assert_eq!(owners[0].accepted.len(), 2);
        assert!(parts[0].iter().any(|p| p.block == BlockId(ROOTS as u64)
            && p.state == ResolutionState::Unresolved
            && p.comparable_range.start <= 1
            && p.comparable_range.end >= 3));
        assert!(
            parts[1]
                .iter()
                .any(|p| p.block == BlockId((ROOTS + 100) as u64)
                    && p.state == ResolutionState::Unresolved
                    && p.comparable_range.start <= 1
                    && p.comparable_range.end >= 2)
        );
        let used = 1_000_000 - remaining;
        for budget in (0..=used).step_by(97).chain([used.saturating_sub(1), used]) {
            let (partial, _, owners, _) = run(budget, 1000, 0)?;
            assert_eq!(partial[ROOTS..], full[ROOTS..partial.len()]);
            assert_eq!(owners[0].accepted.len(), partial.len() - ROOTS);
        }
        for veto in 1..=6 {
            let (held, _, owners, _) = run(1_000_000, 1000, veto)?;
            assert_eq!(held.len(), ROOTS, "veto={veto}");
            assert!(owners.iter().all(|o| o.accepted.is_empty()));
        }
        for cap in 0..=ROOTS + 1 {
            let (held, _, owners, remaining) = run(1_000_000, cap, 0)?;
            assert_eq!(held.len(), ROOTS);
            assert_eq!(remaining, 1_000_000);
            assert!(owners.iter().all(|o| o.accepted.is_empty()));
        }
        Ok(())
    }

    #[test]
    fn h22_growth_many_original_parent_runs_keep_atomic_gold_and_strict_mode() -> Result<()> {
        struct Outcome {
            records: Vec<RelationAssessment>,
            parts: [Vec<ResolutionRange>; 2],
            owners: [Ownership; 2],
            remaining: usize,
            record_capacity: usize,
        }
        // Independent full-parent gold: every uppercase letter occurs once;
        // every new '2' has two old partners between the same mandatory letters.
        let a = "A22B C22D E22F G22H I22J K22L M22N O22P Q22R S22T";
        let b = "A2B C2D E2F G2H I2J K2L M2N O2P Q2R S2T";
        let gold = (0..10)
            .flat_map(|i| [(5 * i, 4 * i), (5 * i + 3, 4 * i + 2)])
            .collect::<Vec<_>>();
        for page_shifted in [false, true] {
            let old_blocks = [anchor_block(1, a)];
            let mut new_blocks = [anchor_block(101, b)];
            new_blocks[0].pages = vec![u32::from(page_shifted)];
            let old = super::super::SidePlan::inspect("H22 old", &old_blocks)?.materialize()?;
            let new = super::super::SidePlan::inspect("H22 new", &new_blocks)?.materialize()?;
            let alignment = Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            let run = |budget, cap| -> Result<Outcome> {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                assessor.remaining_work = budget;
                assessor.options.max_assessment_ranges = cap;
                // This guarantees initially full record storage, independently
                // of the fixture builder's incidental allocation history.
                assessor.records = assessor.records.into_boxed_slice().into_vec();
                assert_eq!(assessor.records.len(), assessor.records.capacity());
                let mut domains = std::mem::take(&mut assessor.domains)
                    .into_iter()
                    .collect::<Vec<_>>();
                domains[0].0.local = None;
                let parent = assessor.records[0].clone();
                let regions = [ProvenChangedRegion {
                    old_span: parent.old_span.clone(),
                    new_span: parent.new_span.clone(),
                    confidence: super::super::Confidence::High,
                    proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                }];
                let mut parts = [
                    h8_partition(BlockId(1), &[(0, a.len(), ResolutionState::Unresolved)]),
                    h8_partition(BlockId(101), &[(0, b.len(), ResolutionState::Unresolved)]),
                ];
                let mut owners = [Ownership::new(), Ownership::new()];
                // Deliberately asymmetric: one owner starts with spare slots.
                owners[0]
                    .accepted
                    .try_reserve_exact(3)
                    .expect("fixture capacity");
                let [old_parts, new_parts] = &mut parts;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut owners,
                    [old_parts, new_parts],
                    mandatory_equal::EqualConstraints {
                        candidates: &[],
                        regions: &regions,
                        original: &[],
                    },
                )?;
                assert_eq!(assessor.records[0], parent);
                assert!(assessor.semantic_acceptance.is_empty());
                assert!(assessor.localized_edits.is_empty());
                assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
                assert_eq!(assessor.records.len(), owners[0].accepted.len() + 1);
                for child in &assessor.records[1..] {
                    assert_eq!(child.parent, Some(0));
                    assert_eq!(child.outcome, RelationOutcome::Established);
                    assert_eq!(child.search, SearchCompleteness::Complete);
                    assert!(child.reasons.is_empty());
                    assert_eq!(
                        child
                            .assumptions
                            .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality),
                        page_shifted
                    );
                    let old = child.old_span.as_ref().expect("old");
                    let new = child.new_span.as_ref().expect("new");
                    assert!(gold.iter().any(|&(x, y)| old.comparable_range
                        == TokenRange {
                            start: x,
                            end: x + 1
                        }
                        && new.comparable_range
                            == TokenRange {
                                start: y,
                                end: y + 1
                            }));
                    for (side, span) in [&old, &new].into_iter().enumerate() {
                        assert!(parts[side].iter().any(|part| part.block == span.blocks[0]
                            && part.state == ResolutionState::Equal
                            && part.comparable_range == span.comparable_range));
                    }
                }
                let capacity = assessor.records.capacity();
                Ok(Outcome {
                    records: assessor.records,
                    parts,
                    owners,
                    remaining: assessor.remaining_work,
                    record_capacity: capacity,
                })
            };
            let Outcome {
                records: full,
                parts,
                owners,
                remaining,
                record_capacity: capacity,
            } = run(1_000_000, 100)?;
            let actual = full[1..]
                .iter()
                .map(|child| {
                    (
                        child.old_span.as_ref().expect("old").comparable_range.start,
                        child.new_span.as_ref().expect("new").comparable_range.start,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, gold);
            assert_eq!(owners[0].accepted.len(), 20);
            for side in &parts {
                assert!(
                    side.iter()
                        .any(|part| part.state == ResolutionState::Unresolved)
                );
            }
            if page_shifted {
                assert!(capacity > full.len());
                assert!(
                    owners
                        .iter()
                        .all(|owner| owner.accepted.capacity() > owner.accepted.len())
                );
                let used = 1_000_000 - remaining;
                for budget in (0..=used)
                    .step_by(257)
                    .chain([used.saturating_sub(1), used])
                {
                    let Outcome {
                        records: partial,
                        owners: partial_owners,
                        ..
                    } = run(budget, 100)?;
                    assert_eq!(&partial[1..], &full[1..partial.len()]);
                    assert_eq!(
                        partial_owners[0].accepted.len(),
                        partial_owners[1].accepted.len()
                    );
                }
            } else {
                // Legacy minimum-only reserve calls and fees stay in their
                // protected branch; this fixture observes their exact growth.
                assert_eq!(capacity, full.len());
                assert!(
                    owners
                        .iter()
                        .all(|owner| owner.accepted.capacity() == owner.accepted.len())
                );
            }
            for cap in 0..=2 {
                let Outcome {
                    records: held,
                    owners: held_owners,
                    remaining,
                    ..
                } = run(1_000_000, cap)?;
                assert_eq!(held.len(), 1);
                assert_eq!(remaining, 1_000_000);
                assert!(held_owners.iter().all(|owner| owner.accepted.is_empty()));
            }
        }
        Ok(())
    }

    #[test]
    fn h19_page_shift_requires_isolated_exact_whole_parent_and_atomic_publication() -> Result<()> {
        let old_blocks = [anchor_block(1, "[20] AUTHOR")];
        let mut new_blocks = [anchor_block(101, "[22] AUTHOR")];
        new_blocks[0].pages = vec![1];
        let old = super::super::SidePlan::inspect("H19 old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("H19 new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run =
            |budget,
             cap,
             veto|
             -> Result<(Assessor<'_, '_>, [Vec<ResolutionRange>; 2], [Ownership; 2])> {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                assessor.remaining_work = budget;
                assessor.options.max_assessment_ranges = cap;
                let mut domains = std::mem::take(&mut assessor.domains)
                    .into_iter()
                    .collect::<Vec<_>>();
                domains[0].0.local = None;
                match veto {
                    1 => {
                        domains[0].0.local = Some((
                            assessor.records[0].old_span.clone().expect("parent"),
                            assessor.records[0].new_span.clone().expect("parent"),
                        ));
                    }
                    2 => domains[0].1.search = SearchCompleteness::Incomplete,
                    3 => domains[0].1.scope = ProofScope::BroadRootRefusal,
                    4 => assessor.records[0]
                        .assumptions
                        .retain(|a| *a != ComparisonAssumption::LocalEvidenceBoundaries),
                    5 => assessor.records[0].parent = Some(0),
                    6 => {
                        assessor.records[0]
                            .old_span
                            .as_mut()
                            .expect("parent")
                            .comparable_range
                            .start = 1;
                    }
                    7 => domains[0].0.old = 0..2,
                    8 => domains[0].1.lengths[0] = 10,
                    9 => assessor.records[0]
                        .reasons
                        .push(AssessmentReason::UnknownReadingOrder),
                    11 => assessor.records[0]
                        .assumptions
                        .push(ComparisonAssumption::AlternativeLineBreakNormalization),
                    12 => assessor.records[0]
                        .assumptions
                        .push(ComparisonAssumption::PageShiftedMandatoryMatchingEquality),
                    _ => {}
                }
                let parent = assessor.records[0].clone();
                let mut regions = vec![ProvenChangedRegion {
                    old_span: parent.old_span.clone(),
                    new_span: parent.new_span.clone(),
                    confidence: super::super::Confidence::High,
                    proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                }];
                if veto == 10 {
                    regions.clear();
                }
                if veto == 13 {
                    regions[0]
                        .old_span
                        .as_mut()
                        .expect("coarse")
                        .comparable_range
                        .end = 10;
                }
                let mut candidates = Vec::new();
                if veto == 14 {
                    let mut spans = [
                        parent.old_span.clone().expect("parent"),
                        parent.new_span.clone().expect("parent"),
                    ];
                    for span in &mut spans {
                        span.canonical_range = ScalarRange { start: 5, end: 11 };
                        span.comparable_range = TokenRange { start: 5, end: 11 };
                    }
                    candidates.push(ChangeCandidate {
                        change: super::super::Change::single_occurrence(
                            ChangeKind::Replacement,
                            Some(spans[0].clone()),
                            Some(spans[1].clone()),
                            super::super::Confidence::Low,
                            Vec::new(),
                        ),
                        relation: 0,
                        alternative_group: 0,
                    });
                }
                let candidates_before = candidates.clone();
                let regions_before = regions.clone();
                let mut parts = [
                    h8_partition(BlockId(1), &[(0, 11, ResolutionState::Unresolved)]),
                    h8_partition(BlockId(101), &[(0, 11, ResolutionState::Unresolved)]),
                ];
                if veto == 15 {
                    parts[0] = h8_partition(
                        BlockId(1),
                        &[
                            (0, 5, ResolutionState::Unresolved),
                            (5, 11, ResolutionState::Changed),
                        ],
                    );
                }
                let mut owners = [Ownership::new(), Ownership::new()];
                let [a, b] = &mut parts;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut owners,
                    [a, b],
                    mandatory_equal::EqualConstraints {
                        candidates: &candidates,
                        regions: &regions,
                        original: &[],
                    },
                )?;
                assert_eq!(assessor.records[0], parent);
                assert_eq!(regions, regions_before);
                assert_eq!(candidates, candidates_before);
                assert!(assessor.semantic_acceptance.is_empty());
                assert!(assessor.localized_edits.is_empty());
                for child in &assessor.records[1..] {
                    assert_eq!(child.parent, Some(0));
                    assert_eq!(child.outcome, RelationOutcome::Established);
                    assert_eq!(child.search, SearchCompleteness::Complete);
                    assert!(child.reasons.is_empty());
                    assert!(
                        child
                            .assumptions
                            .contains(&ComparisonAssumption::MandatoryMatchingEquality)
                    );
                    assert!(
                        child
                            .assumptions
                            .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality)
                    );
                    let pair = [
                        child.old_span.as_ref().expect("paired").comparable_range,
                        child.new_span.as_ref().expect("paired").comparable_range,
                    ];
                    assert!(
                        [(0, 1), (3, 4), (5, 11)]
                            .iter()
                            .any(|&(start, end)| pair == [TokenRange { start, end }; 2])
                    );
                }
                assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
                assert_eq!(owners[0].accepted.len(), assessor.records.len() - 1);
                Ok((assessor, parts, owners))
            };
        let (full, parts, owners) = run(1_000_000, 100, 0)?;
        let actual = full.records[1..]
            .iter()
            .map(|child| {
                let r = child.old_span.as_ref().expect("paired").comparable_range;
                (r.start, r.end)
            })
            .collect::<Vec<_>>();
        // Independent original-parent gold: the old '2' can match either new
        // '2'; only '[', ']', and the AUTHOR suffix have mandatory partners.
        assert_eq!(actual, vec![(0, 1), (3, 4), (5, 11)]);
        for side in &parts {
            assert!(side.iter().any(|p| p.state == ResolutionState::Unresolved
                && p.comparable_range.start <= 1
                && p.comparable_range.end >= 3));
        }
        assert_eq!(owners[0].accepted.len(), 3);
        let used = 1_000_000 - full.remaining_work;
        for budget in (0..=used).step_by(31).chain([used.saturating_sub(1), used]) {
            let (partial, _, _) = run(budget, 100, 0)?;
            assert!(partial.records.len() <= full.records.len());
        }
        for cap in 0..=2 {
            let (held, _, owner) = run(1_000_000, cap, 0)?;
            assert_eq!(held.records.len(), 1);
            assert_eq!(held.remaining_work, 1_000_000);
            assert!(owner.iter().all(|o| o.accepted.is_empty()));
        }
        for veto in 1..=13 {
            let (held, _, owner) = run(1_000_000, 100, veto)?;
            assert_eq!(held.records.len(), 1, "veto {veto}");
            assert!(owner.iter().all(|o| o.accepted.is_empty()));
        }
        for veto in [14, 15] {
            let (held, _, _) = run(1_000_000, 100, veto)?;
            assert_eq!(held.records.len(), 3);
        }
        Ok(())
    }

    #[test]
    fn h19_page_shift_uses_original_parent_matchings_for_repeated_symbols() -> Result<()> {
        for (a, b, gold) in [
            ("A22B", "A2B", vec![(0, 1, 0, 1), (3, 4, 2, 3)]),
            ("AAB", "AB", vec![(2, 3, 1, 2)]),
        ] {
            let old_blocks = [anchor_block(1, a)];
            let mut new_blocks = [anchor_block(101, b)];
            new_blocks[0].pages = vec![1];
            let old =
                super::super::SidePlan::inspect("H19 repeated old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H19 repeated new", &new_blocks)?.materialize()?;
            let alignment = Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            let mut domains = std::mem::take(&mut assessor.domains)
                .into_iter()
                .collect::<Vec<_>>();
            domains[0].0.local = None;
            let parent = assessor.records[0].clone();
            let regions = [ProvenChangedRegion {
                old_span: parent.old_span.clone(),
                new_span: parent.new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            }];
            let mut parts = [
                h8_partition(BlockId(1), &[(0, a.len(), ResolutionState::Unresolved)]),
                h8_partition(BlockId(101), &[(0, b.len(), ResolutionState::Unresolved)]),
            ];
            let mut owners = [Ownership::new(), Ownership::new()];
            let [old_parts, new_parts] = &mut parts;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut owners,
                [old_parts, new_parts],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &regions,
                    original: &[],
                },
            )?;
            assert_eq!(assessor.records[0], parent);
            let actual = assessor.records[1..]
                .iter()
                .map(|child| {
                    let old = child.old_span.as_ref().expect("paired").comparable_range;
                    let new = child.new_span.as_ref().expect("paired").comparable_range;
                    (old.start, old.end, new.start, new.end)
                })
                .collect::<Vec<_>>();
            // Enumerating the original maximum matchings gives exactly these
            // pairs. Neither an arbitrary mate nor compressed residual text
            // may certify any of the repeated A/2 symbols.
            assert_eq!(actual, gold);
            assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
        }
        Ok(())
    }

    #[test]
    fn h19_page_shift_waits_for_every_existing_same_page_certificate() -> Result<()> {
        let mut old_blocks = [anchor_block(1, "XfiCD KL Y"), anchor_block(2, "[30] OTHER")];
        let mut new_blocks = [
            anchor_block(101, "ZfiCD KL W"),
            anchor_block(102, "[33] OTHER"),
        ];
        // The older same-page parent requires the final H14 singleton pass:
        // KL is source-complete immediately; fiCD lacks a ligature-safe cut.
        for block in [&mut old_blocks[0], &mut new_blocks[0]] {
            block.canonical.source_map[1].output_range = ScalarRange { start: 1, end: 3 };
            block.canonical.source_map.remove(2);
            block.raw = block.canonical.clone();
        }
        new_blocks[1].pages = vec![1];
        let old =
            super::super::SidePlan::inspect("H19 priority old", &old_blocks)?.materialize()?;
        let new =
            super::super::SidePlan::inspect("H19 priority new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run =
            |budget| -> Result<Assessor<'_, '_>> {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                assessor.records.clear();
                assessor.domains.clear();
                assessor.remaining_work = budget;
                let mut domains = Vec::new();
                let mut regions = Vec::new();
                for (index, (a, b)) in [(BlockId(1), BlockId(101)), (BlockId(2), BlockId(102))]
                    .into_iter()
                    .enumerate()
                {
                    let spans = [
                        old.canonical_group(&[a], None).full_span(),
                        new.canonical_group(&[b], None).full_span(),
                    ];
                    let lengths = [spans[0].comparable_range.end, spans[1].comparable_range.end];
                    let parent = RelationAssessment {
                        old_span: Some(spans[0].clone()),
                        new_span: Some(spans[1].clone()),
                        parent: None,
                        outcome: RelationOutcome::Established,
                        search: SearchCompleteness::Complete,
                        assumptions: vec![
                            ComparisonAssumption::InputReadingOrder,
                            ComparisonAssumption::CanonicalNormalization,
                            ComparisonAssumption::LocalEvidenceBoundaries,
                        ],
                        reasons: Vec::new(),
                    };
                    regions.push(ProvenChangedRegion {
                        old_span: parent.old_span.clone(),
                        new_span: parent.new_span.clone(),
                        confidence: super::super::Confidence::High,
                        proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                    });
                    assessor.records.push(parent);
                    domains.push((
                        DomainKey {
                            local: None,
                            old: index..index + 1,
                            new: index..index + 1,
                            old_separator: BlockSeparator::Space,
                            new_separator: BlockSeparator::Space,
                        },
                        DomainProof {
                            scope: ProofScope::ExactKey,
                            relation: index,
                            unique: false,
                            search: SearchCompleteness::Complete,
                            edits: Vec::new(),
                            lengths,
                            strict_unique: false,
                            stable_events: None,
                        },
                    ));
                }
                let parents = assessor.records.clone();
                let regions_before = regions.clone();
                let mut parts = [
                    h8_partition(BlockId(1), &[(0, 10, ResolutionState::Unresolved)]),
                    h8_partition(BlockId(101), &[(0, 10, ResolutionState::Unresolved)]),
                ];
                parts[0].extend(h8_partition(
                    BlockId(2),
                    &[(0, 10, ResolutionState::Unresolved)],
                ));
                parts[1].extend(h8_partition(
                    BlockId(102),
                    &[(0, 10, ResolutionState::Unresolved)],
                ));
                let mut owners = [Ownership::new(), Ownership::new()];
                let [a, b] = &mut parts;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut owners,
                    [a, b],
                    mandatory_equal::EqualConstraints {
                        candidates: &[],
                        regions: &regions,
                        original: &[],
                    },
                )?;
                assert_eq!(&assessor.records[..2], parents.as_slice());
                assert_eq!(regions, regions_before);
                assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
                let children = &assessor.records[2..];
                if children.iter().any(|child| child.parent == Some(1)) {
                    assert!(children.len() >= 4);
                    assert!(children[..3].iter().all(|child| child.parent == Some(0)
                        && !child.assumptions.contains(
                            &ComparisonAssumption::PageShiftedMandatoryMatchingEquality
                        )));
                    assert!(children[3..].iter().all(|child| child.parent == Some(1)
                        && child.assumptions.contains(
                            &ComparisonAssumption::PageShiftedMandatoryMatchingEquality
                        )));
                }
                for child in children {
                    let range = child.old_span.as_ref().expect("paired").comparable_range;
                    let gold = if child.parent == Some(0) {
                        [(6, 8), (3, 4), (4, 5)]
                    } else {
                        [(0, 1), (3, 4), (5, 10)]
                    };
                    assert!(gold.contains(&(range.start, range.end)));
                }
                Ok(assessor)
            };
        let full = run(1_000_000)?;
        assert_eq!(full.records.len(), 8);
        let older = &full.records[2..5];
        let older_ranges = older
            .iter()
            .map(|child| {
                let span = child.old_span.as_ref().expect("paired older certificate");
                (span.comparable_range.start, span.comparable_range.end)
            })
            .collect::<Vec<_>>();
        assert_eq!(older_ranges, vec![(6, 8), (3, 4), (4, 5)]);
        let used = 1_000_000 - full.remaining_work;
        for budget in (0..=used).step_by(53).chain([used.saturating_sub(1), used]) {
            let partial = run(budget)?;
            let count = partial.records.len().saturating_sub(2).min(3);
            assert_eq!(&partial.records[2..2 + count], &older[..count]);
        }
        Ok(())
    }

    #[test]
    fn h19_page_shift_keeps_multiblock_and_multiple_page_parents_unowned() -> Result<()> {
        for multiblock in [false, true] {
            let mut old_blocks = vec![anchor_block(1, "A22B")];
            let mut new_blocks = vec![anchor_block(101, "A2B")];
            new_blocks[0].pages = vec![1];
            if multiblock {
                old_blocks.push(anchor_block(2, "TAIL"));
                let mut extra = anchor_block(102, "TAIL");
                extra.pages = vec![1];
                new_blocks.push(extra);
            } else {
                old_blocks[0].pages = vec![0, 2];
                old_blocks[0].page_breaks = Some(vec![2]);
            }
            let old =
                super::super::SidePlan::inspect("H19 held old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H19 held new", &new_blocks)?.materialize()?;
            let alignment = Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            let mut domains = std::mem::take(&mut assessor.domains)
                .into_iter()
                .collect::<Vec<_>>();
            domains[0].0.local = None;
            let parent = assessor.records[0].clone();
            let regions = [ProvenChangedRegion {
                old_span: parent.old_span.clone(),
                new_span: parent.new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            }];
            let mut parts = [Vec::new(), Vec::new()];
            for (side, blocks) in [&old_blocks, &new_blocks].into_iter().enumerate() {
                for block in blocks {
                    parts[side].extend(h8_partition(
                        block.block,
                        &[(
                            0,
                            block.canonical.text.chars().count(),
                            ResolutionState::Unresolved,
                        )],
                    ));
                }
            }
            let before = parts.clone();
            let mut owners = [Ownership::new(), Ownership::new()];
            let [a, b] = &mut parts;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut owners,
                [a, b],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &regions,
                    original: &[],
                },
            )?;
            assert_eq!(assessor.records, vec![parent]);
            assert_eq!(parts, before);
            assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        }
        Ok(())
    }

    #[test]
    fn h19_page_shift_singletons_keep_ligature_and_candidate_refusals() -> Result<()> {
        // One decoded ligature owns both f/i scalars. CD and KL have distinct
        // literal singleton glyphs; only the fiCD maximal run lacks a source cut.
        let mut old_blocks = [anchor_block(1, "XfiCD KL Y")];
        let mut new_blocks = [anchor_block(101, "ZfiCD KL W")];
        for block in [&mut old_blocks[0], &mut new_blocks[0]] {
            block.canonical.source_map[1].output_range = ScalarRange { start: 1, end: 3 };
            block.canonical.source_map.remove(2);
            block.raw = block.canonical.clone();
        }
        new_blocks[0].pages = vec![1];
        let old =
            super::super::SidePlan::inspect("H14 ligature old", &old_blocks)?.materialize()?;
        let new =
            super::super::SidePlan::inspect("H14 ligature new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let run =
            |budget,
             cap,
             veto|
             -> Result<(Assessor<'_, '_>, [Vec<ResolutionRange>; 2], [Ownership; 2])> {
                let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
                assessor.remaining_work = budget;
                assessor.options.max_assessment_ranges = cap;
                let mut domains = std::mem::take(&mut assessor.domains)
                    .into_iter()
                    .collect::<Vec<_>>();
                domains[0].0.local = None;
                if veto == 2 {
                    assessor.records[0]
                        .reasons
                        .push(AssessmentReason::NormalizationUncertainty);
                }
                let parent = assessor.records[0].clone();
                let mut candidates = Vec::new();
                if veto == 1 {
                    let mut old = parent.old_span.clone().expect("paired parent");
                    let mut new = parent.new_span.clone().expect("paired parent");
                    for span in [&mut old, &mut new] {
                        span.comparable_range = TokenRange { start: 3, end: 4 };
                        span.canonical_range = ScalarRange { start: 3, end: 4 };
                    }
                    candidates.push(ChangeCandidate {
                        change: super::super::Change::single_occurrence(
                            ChangeKind::Replacement,
                            Some(old),
                            Some(new),
                            super::super::Confidence::Low,
                            Vec::new(),
                        ),
                        relation: 0,
                        alternative_group: 0,
                    });
                }
                let candidates_before = candidates.clone();
                let coarse = [ProvenChangedRegion {
                    old_span: parent.old_span.clone(),
                    new_span: parent.new_span.clone(),
                    confidence: super::super::Confidence::High,
                    proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
                }];
                let mut parts = [
                    h8_partition(BlockId(1), &[(0, 10, ResolutionState::Unresolved)]),
                    h8_partition(BlockId(101), &[(0, 10, ResolutionState::Unresolved)]),
                ];
                let mut owners = [Ownership::new(), Ownership::new()];
                let [a, b] = &mut parts;
                assessor.recover_mandatory_coarse_equalities(
                    &domains,
                    domains.capacity(),
                    &mut owners,
                    [a, b],
                    mandatory_equal::EqualConstraints {
                        candidates: &candidates,
                        regions: &coarse,
                        original: &[],
                    },
                )?;
                assert_eq!(assessor.records[0], parent);
                assert_eq!(candidates, candidates_before);
                assert!(assessor.semantic_acceptance.is_empty());
                assert!(assessor.localized_edits.is_empty());
                let actual = assessor.records[1..]
                    .iter()
                    .map(|child| {
                        assert_eq!(child.outcome, RelationOutcome::Established);
                        assert_eq!(child.search, SearchCompleteness::Complete);
                        assert!(child.reasons.is_empty());
                        let old = child.old_span.as_ref().expect("paired certificate");
                        let new = child.new_span.as_ref().expect("paired certificate");
                        assert_eq!(old.comparable_range, new.comparable_range);
                        (old.comparable_range.start, old.comparable_range.end)
                    })
                    .collect::<Vec<_>>();
                // Literal source gold: KL is attempted by the original full pass;
                // later C,D are safe singleton cuts, f/i remain an indivisible glyph.
                assert!(
                    actual
                        .iter()
                        .all(|range| [(6, 8), (3, 4), (4, 5)].contains(range))
                );
                assert!(actual.is_empty() || actual[0] == (6, 8));
                assert_eq!(owners[0].accepted.len(), owners[1].accepted.len());
                assert_eq!(owners[0].accepted.len(), actual.len());
                Ok((assessor, parts, owners))
            };
        let (full, parts, _) = run(1_000_000, 100, 0)?;
        let actual = full.records[1..]
            .iter()
            .map(|child| {
                let range = child
                    .old_span
                    .as_ref()
                    .expect("paired certificate")
                    .comparable_range;
                (range.start, range.end)
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, vec![(6, 8), (3, 4), (4, 5)]);
        for side in &parts {
            assert!(
                side.iter()
                    .any(|part| part.state == ResolutionState::Unresolved
                        && part.comparable_range.start <= 1
                        && part.comparable_range.end >= 3)
            );
        }
        let used = 1_000_000 - full.remaining_work;
        for budget in (0..=used).step_by(97).chain([used.saturating_sub(1), used]) {
            let (partial, _, owners) = run(budget, 100, 0)?;
            assert!(partial.records.len() <= full.records.len());
            assert_eq!(partial.records.len() - 1, owners[0].accepted.len());
        }
        for cap in 0..=2 {
            let (held, _, owners) = run(1_000_000, cap, 0)?;
            assert_eq!(held.records.len(), 1);
            assert_eq!(held.remaining_work, 1_000_000);
            assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        }
        let (candidate_veto, _, _) = run(1_000_000, 100, 1)?;
        assert_eq!(candidate_veto.records.len(), 2); // KL only; C candidate prevents the whole-run source probe.
        let (dirty, _, owners) = run(1_000_000, 100, 2)?;
        assert_eq!(dirty.records.len(), 1);
        assert!(owners.iter().all(|owner| owner.accepted.is_empty()));
        Ok(())
    }

    #[test]
    fn h19_page_shift_singletons_keep_deleted_break_and_sharing_refusals() -> Result<()> {
        use crate::normalize::{NormalizationEvent, NormalizationKind};
        for shared in [false, true] {
            let mut old_blocks = [anchor_block(1, "XABCDEY")];
            let mut new_blocks = [anchor_block(101, "ZABCDEW")];
            for block in [&mut old_blocks[0], &mut new_blocks[0]] {
                if shared {
                    // B/C name the same glyph twice. No singleton may own it.
                    let source = block.canonical.source_map[2].source.clone();
                    block.canonical.source_map[3].source = source;
                    block.raw = block.canonical.clone();
                } else {
                    // The deleted raw break is between B/C; its endpoints and
                    // widened normalization cut must remain held on both sides.
                    let [TextSourceAtom::Glyph(before)] =
                        block.canonical.source_map[2].source.atoms.as_slice()
                    else {
                        panic!("fixture glyph")
                    };
                    let [TextSourceAtom::Glyph(after)] =
                        block.canonical.source_map[3].source.atoms.as_slice()
                    else {
                        panic!("fixture glyph")
                    };
                    let source = TextSource {
                        atoms: vec![TextSourceAtom::LineBreak {
                            preceding: *before,
                            following: *after,
                        }]
                        .into(),
                    };
                    block.raw.text.insert(3, '\n');
                    for entry in &mut block.raw.source_map[3..] {
                        entry.output_range.start += 1;
                        entry.output_range.end += 1;
                    }
                    block.raw.source_map.insert(
                        3,
                        SourceMapEntry {
                            output_range: ScalarRange { start: 3, end: 4 },
                            source: source.clone(),
                        },
                    );
                    block.normalization_events.push(NormalizationEvent {
                        kind: NormalizationKind::SoftLineBreak,
                        raw_range: ScalarRange { start: 3, end: 4 },
                        canonical_range: ScalarRange { start: 3, end: 3 },
                        source,
                    });
                }
            }
            new_blocks[0].pages = vec![1];
            let old =
                super::super::SidePlan::inspect("H14 boundary old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H14 boundary new", &new_blocks)?.materialize()?;
            let alignment = Alignment {
                spans: Vec::new(),
                main_anchors: Vec::new(),
                move_candidates: Vec::new(),
            };
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            let mut domains = std::mem::take(&mut assessor.domains)
                .into_iter()
                .collect::<Vec<_>>();
            domains[0].0.local = None;
            let parent = assessor.records[0].clone();
            let coarse = [ProvenChangedRegion {
                old_span: parent.old_span.clone(),
                new_span: parent.new_span.clone(),
                confidence: super::super::Confidence::High,
                proof: super::super::ChangedRegionProof::ExactTokenMultisetMismatch,
            }];
            let mut parts = [
                h8_partition(BlockId(1), &[(0, 7, ResolutionState::Unresolved)]),
                h8_partition(BlockId(101), &[(0, 7, ResolutionState::Unresolved)]),
            ];
            let mut owners = [Ownership::new(), Ownership::new()];
            let [a, b] = &mut parts;
            assessor.recover_mandatory_coarse_equalities(
                &domains,
                domains.capacity(),
                &mut owners,
                [a, b],
                mandatory_equal::EqualConstraints {
                    candidates: &[],
                    regions: &coarse,
                    original: &[],
                },
            )?;
            assert_eq!(assessor.records[0], parent);
            let ranges = assessor.records[1..]
                .iter()
                .map(|child| {
                    let old = child
                        .old_span
                        .as_ref()
                        .expect("paired equality")
                        .comparable_range;
                    let new = child
                        .new_span
                        .as_ref()
                        .expect("paired equality")
                        .comparable_range;
                    assert_eq!(old, new);
                    (old.start, old.end)
                })
                .collect::<Vec<_>>();
            assert_eq!(ranges, vec![(1, 2), (4, 5), (5, 6)], "shared={shared}");
            for side in &parts {
                assert!(
                    side.iter()
                        .any(|part| part.state == ResolutionState::Unresolved
                            && part.comparable_range.start <= 2
                            && part.comparable_range.end >= 4)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn h11_source_capacity_includes_vectors_before_malformed_map_refusal() -> Result<()> {
        let text = "A".repeat(1024);
        for multisource in [false, true] {
            let mut old_blocks = [anchor_block(1, &text)];
            let mut new_blocks = [anchor_block(101, &text)];
            for block in [&mut old_blocks[0], &mut new_blocks[0]] {
                if multisource {
                    block.canonical.source_map.truncate(1);
                    block.canonical.source_map[0].output_range = ScalarRange {
                        start: 0,
                        end: 1024,
                    };
                    block.raw = block.canonical.clone();
                } else {
                    block.canonical.source_map.clear();
                    block.raw.source_map.clear();
                }
            }
            let old =
                super::super::SidePlan::inspect("H11 malformed old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H11 malformed new", &new_blocks)?.materialize()?;
            let mut remaining = 10_000;
            let bytes =
                mandatory_equal::literal_source_capacity_bound([&old, &new], &mut remaining)
                    .expect("metadata has a bounded scratch allowance");
            let before_refusal =
                2 * 1024 * (std::mem::size_of::<char>() + std::mem::size_of::<GlyphId>());
            assert!(bytes >= before_refusal);
            let spans = [
                old.canonical_group(&[BlockId(1)], None).full_span(),
                new.canonical_group(&[BlockId(101)], None).full_span(),
            ];
            let mut cache = equal_fragment::EqualFragmentCache::new([&old, &new]);
            assert_eq!(
                cache.prove_sources_for_mandatory_pairs([&spans[0], &spans[1]], &mut remaining)?,
                equal_fragment::FragmentVerdict::Held(
                    equal_fragment::FragmentHold::CanonicalSource
                )
            );
        }
        Ok(())
    }

    #[test]
    fn h8_reviewed_payload_counts_nested_capacities_before_optional_work() {
        let span = TextSpan {
            blocks: vec![BlockId(1)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 1 },
            comparable_range: TokenRange { start: 0, end: 1 },
        };
        let intervals = vec![SourceInterval {
            block_index: 0,
            start: 0,
            end: 1,
        }];
        let domains = vec![(
            DomainKey {
                local: Some((span.clone(), span)),
                old: 0..1,
                new: 0..1,
                old_separator: BlockSeparator::Concatenate,
                new_separator: BlockSeparator::Concatenate,
            },
            DomainProof {
                scope: ProofScope::ExactKey,
                relation: 0,
                unique: false,
                search: SearchCompleteness::Complete,
                edits: vec![super::super::AtomicEdit {
                    old: 0..1,
                    new: 0..1,
                }],
                lengths: [1, 1],
                strict_unique: false,
                stable_events: Some(vec![ProjectedEvent {
                    kind: ChangeKind::Replacement,
                    occurrences: vec![ProjectedOccurrence {
                        old: Some(intervals.clone()),
                        new: Some(intervals),
                    }],
                }]),
            },
        )];
        let expected = std::mem::size_of::<(DomainKey, DomainProof)>()
            + 2 * std::mem::size_of::<BlockId>()
            + std::mem::size_of::<super::super::AtomicEdit>()
            + std::mem::size_of::<ProjectedEvent>()
            + std::mem::size_of::<ProjectedOccurrence>()
            + 2 * std::mem::size_of::<SourceInterval>();
        for prefix in 0..3 {
            let mut remaining = prefix;
            assert_eq!(
                coarse_reviewed_domain_bytes(&domains, domains.capacity(), &mut remaining),
                None
            );
            assert_eq!(remaining, 0);
        }
        let mut remaining = 3;
        assert_eq!(
            coarse_reviewed_domain_bytes(&domains, domains.capacity(), &mut remaining),
            Some(expected)
        );
        assert_eq!(remaining, 0);
        let mut remaining = 100;
        assert_eq!(
            coarse_reviewed_domain_bytes(
                &domains,
                COARSE_MEMORY_BYTES / std::mem::size_of::<(DomainKey, DomainProof)>() + 1,
                &mut remaining,
            ),
            None
        );
        assert_eq!(remaining, 99);
        assert_eq!(
            coarse_reviewed_domain_bytes(&[], 0, &mut remaining),
            Some(0)
        );
        assert_eq!(remaining, 99);
    }

    #[test]
    fn h8_actual_part_v_collector_preserves_ambiguity_and_ownership() -> Result<()> {
        // Development source quotation of the complete Schedule C Part V
        // parent. This tests containing-range evidence, not a chosen deletion.
        const OLD: &str = "Other Expenses. List below business expenses not included on lines 8–26, line 27b, or line 30. 48 Total other expenses. Enter here and on line 27a . . . . . . . . . . . . . . . . 48 Schedule C (Form 1040) 2024";
        const NEW: &str = "Other Expenses. List below business expenses not included on lines 8-27a, or line 30. 48 Total other expenses. Enter here and on line 27b . . . . . . . . . . . . . . . . 48 Schedule C (Form 1040) 2025";
        let old_blocks = [anchor_block(1, OLD)];
        let new_blocks = [anchor_block(101, NEW)];
        let old = super::super::SidePlan::inspect("H8 old Part V", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("H8 new Part V", &new_blocks)?.materialize()?;
        assert_eq!(old.total_tokens, 209);
        assert_eq!(new.total_tokens, 200);
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        use ResolutionState::{Changed, Equal, Unresolved};
        let partitions = [
            h8_partition(
                BlockId(1),
                &[
                    (0, 2, Unresolved),
                    (2, 66, Equal),
                    (66, 80, Unresolved),
                    (80, 81, Changed),
                    (81, 95, Unresolved),
                    (95, 209, Equal),
                ],
            ),
            h8_partition(
                BlockId(101),
                &[
                    (0, 2, Unresolved),
                    (2, 66, Equal),
                    (66, 71, Unresolved),
                    (71, 72, Changed),
                    (72, 86, Unresolved),
                    (86, 200, Equal),
                ],
            ),
        ];
        let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
        let parent = assessor.records[0].clone();
        let semantic_before = assessor.semantic_acceptance.clone();
        let localized_before = assessor.localized_edits.len();
        let mut output = Vec::new();
        assessor
            .collect_mandatory_changed_regions([&partitions[0], &partitions[1]], &mut output)?;
        assert_eq!(output.len(), 1);
        assert_eq!(
            output[0]
                .old_span
                .as_ref()
                .expect("old gap")
                .comparable_range,
            TokenRange { start: 68, end: 79 }
        );
        assert_eq!(
            output[0]
                .new_span
                .as_ref()
                .expect("new gap")
                .comparable_range,
            TokenRange { start: 68, end: 70 }
        );
        assert_eq!(
            output[0].proof,
            super::super::ChangedRegionProof::ExactTokenMultisetMismatch
        );
        assert_eq!(assessor.records[0], parent);
        assert_eq!(assessor.semantic_acceptance, semantic_before);
        assert_eq!(assessor.localized_edits.len(), localized_before);
        // The production review pass moves the cache out of the assessor.
        // Its retained vector, rather than the now-empty map, supplies the
        // final coarse pass after all earlier proof work has finished.
        let mut through_review = h8_closed_parent([&old, &new], &alignment)?;
        let reviewed = review::collect(&mut through_review, [&partitions[0], &partitions[1]])?;
        assert!(through_review.domains.is_empty());
        assert_eq!(reviewed.domains.len(), 1);
        let reviewed_parent = through_review.records[0].clone();
        let mut production_output = Vec::new();
        through_review.collect_mandatory_changed_regions_from_review(
            &reviewed.domains,
            reviewed.domains.capacity(),
            [&partitions[0], &partitions[1]],
            &mut production_output,
        )?;
        assert_eq!(production_output, output);
        assert_eq!(through_review.records[0], reviewed_parent);
        assert!(through_review.domains.is_empty());
        assert!(
            !assessor
                .domains
                .values()
                .next()
                .expect("parent proof")
                .unique
        );
        let child = assessor.records.last().expect("boundary child");
        assert_eq!(child.parent, Some(0));
        assert!(
            child
                .assumptions
                .contains(&ComparisonAssumption::MandatoryMatchingBoundaries)
        );
        assert!(
            !child
                .assumptions
                .contains(&ComparisonAssumption::MandatoryMatchingEquality)
        );
        let required = 1_000_000 - assessor.remaining_work;
        let mut prefixes = (0..400).collect::<Vec<_>>();
        prefixes.extend((0..20).map(|bit| 1usize << bit));
        prefixes.extend([required.saturating_sub(1), required]);
        for prefix in prefixes {
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            assessor.remaining_work = prefix;
            let mut partial = Vec::new();
            assessor.collect_mandatory_changed_regions(
                [&partitions[0], &partitions[1]],
                &mut partial,
            )?;
            assert!(partial.is_empty() || partial == output);
            assert_eq!(assessor.records[0], parent);
            assert_eq!(assessor.semantic_acceptance, semantic_before);
        }
        for failure in 0..7 {
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            match failure {
                0 => assessor.records[0]
                    .reasons
                    .push(AssessmentReason::ExtractionGap),
                1 => assessor.records[0].search = SearchCompleteness::Incomplete,
                2 => {
                    assessor.domains.values_mut().next().expect("proof").search =
                        SearchCompleteness::Incomplete;
                }
                3 => {
                    assessor.domains.values_mut().next().expect("proof").scope =
                        ProofScope::BroadRootRefusal;
                }
                4 => assessor.records[0].outcome = RelationOutcome::Tentative,
                5 => assessor.records[0]
                    .reasons
                    .push(AssessmentReason::NormalizationUncertainty),
                _ => assessor.output_stop = Some(0),
            }
            let parent = assessor.records[0].clone();
            let mut held = Vec::new();
            assessor
                .collect_mandatory_changed_regions([&partitions[0], &partitions[1]], &mut held)?;
            assert!(held.is_empty());
            assert_eq!(assessor.records[0], parent);
        }
        for cap in 0..=2 {
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            assessor.options.max_assessment_ranges = cap;
            let mut held = Vec::new();
            assessor
                .collect_mandatory_changed_regions([&partitions[0], &partitions[1]], &mut held)?;
            assert!(held.is_empty());
            assert_eq!(assessor.remaining_work, 1_000_000);
            assert_eq!(assessor.records.as_slice(), std::slice::from_ref(&parent));
        }
        let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
        let mut already = output.clone();
        assessor
            .collect_mandatory_changed_regions([&partitions[0], &partitions[1]], &mut already)?;
        assert_eq!(already, output);
        let mut resolved = partitions.clone();
        resolved[0] = h8_partition(
            BlockId(1),
            &[
                (0, 68, Equal),
                (68, 69, Changed),
                (69, 80, Unresolved),
                (80, 209, Equal),
            ],
        );
        let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
        let mut held = Vec::new();
        assessor.collect_mandatory_changed_regions([&resolved[0], &resolved[1]], &mut held)?;
        assert!(held.is_empty());
        Ok(())
    }

    #[test]
    fn h8_collector_reports_containing_range_without_choosing_repeated_glyphs() -> Result<()> {
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        for (old_text, new_text, expected) in [("A22B", "A2B", true), ("AXYB", "AYXB", false)] {
            let old_blocks = [anchor_block(1, old_text)];
            let new_blocks = [anchor_block(101, new_text)];
            let old =
                super::super::SidePlan::inspect("H8 ambiguous old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H8 ambiguous new", &new_blocks)?.materialize()?;
            let partitions = [
                h8_partition(
                    BlockId(1),
                    &[(0, old.total_tokens, ResolutionState::Unresolved)],
                ),
                h8_partition(
                    BlockId(101),
                    &[(0, new.total_tokens, ResolutionState::Unresolved)],
                ),
            ];
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            let mut output = Vec::new();
            assessor
                .collect_mandatory_changed_regions([&partitions[0], &partitions[1]], &mut output)?;
            assert_eq!(!output.is_empty(), expected);
            assert!(assessor.semantic_acceptance.is_empty());
            assert!(assessor.localized_edits.is_empty());
            if expected {
                assert_eq!(output.len(), 1);
                assert_eq!(
                    output[0]
                        .old_span
                        .as_ref()
                        .expect("old interior")
                        .comparable_range,
                    TokenRange { start: 1, end: 3 }
                );
                assert_eq!(
                    output[0]
                        .new_span
                        .as_ref()
                        .expect("new interior")
                        .comparable_range,
                    TokenRange { start: 1, end: 2 }
                );
                assert!(
                    !assessor
                        .domains
                        .values()
                        .next()
                        .expect("proof")
                        .strict_unique
                );
            }
        }
        Ok(())
    }

    #[test]
    fn h8_collector_holds_reflow_and_unsupported_projection() -> Result<()> {
        use ResolutionState::Unresolved;
        let alignment = Alignment {
            spans: Vec::new(),
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        for (old_texts, new_texts) in [
            (vec!["A  B"], vec!["A B"]),
            (vec!["A\nB"], vec!["AB"]),
            (vec!["A\n", "XZB"], vec!["A\n", "YB"]),
            (vec!["A\t", "XZB"], vec!["A\t", "YB"]),
            (vec!["AXZB"], vec!["AYB"]),
        ] {
            let mut old_blocks = old_texts
                .iter()
                .enumerate()
                .map(|(index, text)| anchor_block(index as u64 + 1, text))
                .collect::<Vec<_>>();
            let new_blocks = new_texts
                .iter()
                .enumerate()
                .map(|(index, text)| anchor_block(index as u64 + 101, text))
                .collect::<Vec<_>>();
            if old_texts == ["AXZB"] {
                old_blocks[0].canonical.source_map[1].source.atoms =
                    vec![TextSourceAtom::SyntheticSpace {
                        preceding: GlyphId(100),
                        following: GlyphId(102),
                    }]
                    .into();
            }
            let old =
                super::super::SidePlan::inspect("H8 reflow old", &old_blocks)?.materialize()?;
            let new =
                super::super::SidePlan::inspect("H8 reflow new", &new_blocks)?.materialize()?;
            let partitions = [&old, &new].map(|side| {
                side.blocks
                    .iter()
                    .enumerate()
                    .flat_map(|(index, block)| {
                        h8_partition(block.block, &[(0, side.canonical[index].len(), Unresolved)])
                    })
                    .collect::<Vec<_>>()
            });
            let mut assessor = h8_closed_parent([&old, &new], &alignment)?;
            let parent = assessor.records[0].clone();
            let mut output = Vec::new();
            assessor
                .collect_mandatory_changed_regions([&partitions[0], &partitions[1]], &mut output)?;
            assert!(output.is_empty());
            assert_eq!(assessor.records, [parent]);
        }
        Ok(())
    }

    #[test]
    fn h8_source_guard_rejects_synthetic_shared_and_cut_sources() -> Result<()> {
        let original = anchor_block(1, "A–26, line 27B");
        let check = |blocks: &[crate::normalize::BlockText], range: Range<usize>| -> Result<bool> {
            let side = super::super::SidePlan::inspect("H8 source", blocks)?.materialize()?;
            let group = side.canonical_group(&[blocks[0].block], None);
            let span = group.span(range.start, range.end);
            let tokens = group.tokens[range].iter().collect::<Vec<_>>();
            coarse_source_guard(&side, &span, &tokens, &mut { usize::MAX })
        };
        assert!(check(std::slice::from_ref(&original), 1..12)?);
        let mut synthetic = original.clone();
        synthetic.canonical.source_map[1].source.atoms = vec![TextSourceAtom::SyntheticSpace {
            preceding: GlyphId(100),
            following: GlyphId(102),
        }]
        .into();
        assert!(!check(&[synthetic], 1..12)?);
        let mut shared = original.clone();
        shared.canonical.source_map[0].source = shared.canonical.source_map[1].source.clone();
        assert!(!check(&[shared], 1..12)?);
        let mut raw_changed = original.clone();
        raw_changed.raw.text = "A-26, line 27B".to_owned();
        assert!(!check(&[raw_changed], 1..12)?);
        let mut crossing = original.clone();
        crossing.canonical.source_map[0].output_range.end = 2;
        crossing.canonical.source_map.remove(1);
        assert!(!check(&[crossing], 1..12)?);
        let mut outside = anchor_block(2, "other");
        outside.canonical.source_map[0].source = original.canonical.source_map[1].source.clone();
        outside.raw = outside.canonical.clone();
        assert!(!check(&[original.clone(), outside], 1..12)?);
        let old = [
            ComparableToken::Scalar('A'),
            ComparableToken::Scalar(' '),
            ComparableToken::Scalar('B'),
        ];
        let new = [ComparableToken::Scalar('A'), ComparableToken::Scalar('B')];
        assert!(!coarse_substantive_mismatch(
            &old.iter().collect::<Vec<_>>(),
            &new.iter().collect::<Vec<_>>(),
            &mut { usize::MAX }
        ));
        let unmapped = [ComparableToken::Unmapped {
            font_hash: crate::model::FontProgramHash(vec![7; 4096]),
            glyph_id: 11,
        }];
        assert!(!coarse_substantive_mismatch(
            &unmapped.iter().collect::<Vec<_>>(),
            &new.iter().collect::<Vec<_>>(),
            &mut { usize::MAX }
        ));
        let new = [
            ComparableToken::Scalar('A'),
            ComparableToken::Scalar('-'),
            ComparableToken::Scalar('B'),
        ];
        assert!(coarse_substantive_mismatch(
            &old.iter().collect::<Vec<_>>(),
            &new.iter().collect::<Vec<_>>(),
            &mut { usize::MAX }
        ));
        let side = super::super::SidePlan::inspect("H8 prefixes", std::slice::from_ref(&original))?
            .materialize()?;
        let group = side.canonical_group(&[original.block], None);
        let span = group.span(1, 12);
        let tokens = group.tokens[1..12].iter().collect::<Vec<_>>();
        let mut work = usize::MAX;
        assert!(coarse_source_guard(&side, &span, &tokens, &mut work)?);
        let required = usize::MAX - work;
        for prefix in 0..required {
            assert!(!coarse_source_guard(
                &side,
                &span,
                &tokens,
                &mut prefix.clone()
            )?);
        }
        Ok(())
    }

    #[test]
    fn h8_borrowed_views_match_legacy_full_and_partial_groups() -> Result<()> {
        use crate::{model::FontProgramHash, normalize::UnmappedToken};
        let mut blocks = [
            anchor_block(1, ""),
            anchor_block(2, "A雪 "),
            anchor_block(3, ""),
            anchor_block(4, "🙂B"),
        ];
        blocks[2].canonical.unmapped.push(UnmappedToken {
            scalar_index: 0,
            font_hash: FontProgramHash(vec![3; 257]),
            glyph_id: 19,
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(900))].into(),
            },
        });
        blocks[2].raw = blocks[2].canonical.clone();
        let side = super::super::SidePlan::inspect("H8 view oracle", &blocks)?.materialize()?;
        let id_orders = [
            vec![],
            vec![BlockId(2)],
            vec![BlockId(1), BlockId(3), BlockId(2)],
            vec![BlockId(4), BlockId(1), BlockId(2)],
        ];
        let mut cases = 0;
        for ids in &id_orders {
            for separator in [
                BlockSeparator::Space,
                BlockSeparator::Concatenate,
                BlockSeparator::PerBoundary([false, true]),
                BlockSeparator::PerBoundary([true, false]),
            ] {
                let legacy = side.canonical_group(ids, Some(separator));
                for start in 0..=legacy.tokens.len() {
                    for end in start..=legacy.tokens.len() {
                        let parent = legacy.span(start, end);
                        let key = DomainKey {
                            local: Some((parent.clone(), parent.clone())),
                            old: 0..side.blocks.len(),
                            new: 0..side.blocks.len(),
                            old_separator: separator,
                            new_separator: separator,
                        };
                        let [expected, _] = proof_groups([&side, &side], &key)?;
                        let [actual, _] = borrowed_group_pair(
                            [&side, &side],
                            [&parent, &parent],
                            &mut { usize::MAX },
                            COARSE_MEMORY_BYTES,
                        )
                        .expect("paid pair");
                        assert!(
                            actual
                                .selected_tokens()
                                .iter()
                                .copied()
                                .eq(expected.tokens.iter())
                        );
                        for cut_start in 0..=expected.tokens.len() {
                            for cut_end in cut_start..=expected.tokens.len() {
                                assert_eq!(
                                    actual.try_span(cut_start..cut_end),
                                    expected.try_span(cut_start, cut_end)
                                );
                            }
                        }
                        cases += 1;
                    }
                }
            }
        }
        assert!(cases > 100);
        let full = side
            .canonical_group(&id_orders[2], Some(BlockSeparator::Space))
            .full_span();
        let mut paid = usize::MAX;
        borrowed_group_pair(
            [&side, &side],
            [&full, &full],
            &mut paid,
            COARSE_MEMORY_BYTES,
        )
        .expect("paid pair");
        let required = usize::MAX - paid;
        for prefix in 0..required {
            let mut remaining = prefix;
            assert!(matches!(
                borrowed_group_pair(
                    [&side, &side],
                    [&full, &full],
                    &mut remaining,
                    COARSE_MEMORY_BYTES
                ),
                Err(MaterializationRefusal::Work)
            ));
            assert_eq!(remaining, 0);
        }
        let mut remaining = usize::MAX;
        assert!(matches!(
            borrowed_group_pair([&side, &side], [&full, &full], &mut remaining, 0),
            Err(MaterializationRefusal::Memory)
        ));
        assert_eq!(
            usize::MAX - remaining,
            2 * (full.blocks.len().pow(2) + 4 * full.blocks.len() + 1)
        );
        Ok(())
    }

    /// Independent pre-optimization control; retain the actual group/locator
    /// route rather than restating the structural shortcut's implementation.
    fn legacy_contains_span(
        side: &Side<'_>,
        outer: Option<&TextSpan>,
        inner: Option<&TextSpan>,
    ) -> Result<bool> {
        match (outer, inner) {
            (_, None) => Ok(true),
            (None, Some(_)) => Ok(false),
            (Some(outer), Some(inner)) => {
                let group = side.canonical_group(&outer.blocks, outer.separator);
                Ok(
                    locate_in_group(side, Some(inner), &group)?.is_some_and(|range| {
                        range.start >= outer.comparable_range.start
                            && range.end <= outer.comparable_range.end
                    }),
                )
            }
        }
    }

    #[test]
    fn containment_miss_preserves_legacy_ranges_and_projection_errors() -> Result<()> {
        use crate::{model::FontProgramHash, normalize::UnmappedToken};

        const TEXTS: [&str; 9] = ["", "A", " ", "\n", "\t", "\u{a0}", "雪🙂", "AB", ""];
        let lists: &[&[u64]] = &[
            &[],
            &[5],
            &[2],
            &[10],
            &[5, 2],
            &[5, 2, 9],
            &[5, 9],
            &[9, 2],
            &[5, 5],
        ];
        let separators = [
            None,
            Some(BlockSeparator::Concatenate),
            Some(BlockSeparator::Space),
            Some(BlockSeparator::PerBoundary([false, true])),
            Some(BlockSeparator::PerBoundary([true, false])),
        ];
        let mut cases = 0;
        for offset in 0..TEXTS.len() {
            let mut blocks = [5, 2, 9, 10]
                .into_iter()
                .enumerate()
                .map(|(index, id)| anchor_block(id, TEXTS[(offset + index) % TEXTS.len()]))
                .collect::<Vec<_>>();
            if offset % 2 == 0 {
                for scalar_index in [0, blocks[2].canonical.text.chars().count()] {
                    blocks[2].canonical.unmapped.push(UnmappedToken {
                        scalar_index,
                        font_hash: FontProgramHash(vec![3, 7, 11]),
                        glyph_id: 17,
                        source: TextSource {
                            atoms: Vec::new().into(),
                        },
                    });
                }
                blocks[2].raw = blocks[2].canonical.clone();
            }
            let side =
                super::super::SidePlan::inspect("containment oracle", &blocks)?.materialize()?;
            for &outer_ids in lists {
                for separator in separators {
                    if separator.is_some_and(|value| !value.valid_for(outer_ids.len())) {
                        continue;
                    }
                    let ids = outer_ids.iter().copied().map(BlockId).collect::<Vec<_>>();
                    let outer_group = side.canonical_group(&ids, separator);
                    let full_outer = outer_group.full_span();
                    let mut unvalidated_outer_ranges = full_outer.clone();
                    unvalidated_outer_ranges.canonical_range = ScalarRange {
                        start: 700,
                        end: 701,
                    };
                    unvalidated_outer_ranges.comparable_range.start =
                        full_outer.comparable_range.end;
                    for outer in [&full_outer, &unvalidated_outer_ranges] {
                        for &inner_ids in lists {
                            for inner_separator in separators {
                                if inner_separator
                                    .is_some_and(|value| !value.valid_for(inner_ids.len()))
                                {
                                    continue;
                                }
                                let ids =
                                    inner_ids.iter().copied().map(BlockId).collect::<Vec<_>>();
                                let group = side.canonical_group(&ids, inner_separator);
                                let full_inner = group.full_span();
                                let mut invalid_scalar = full_inner.clone();
                                invalid_scalar.canonical_range.end += 1;
                                let mut invalid_tokens = full_inner.clone();
                                invalid_tokens.comparable_range.end += 1;
                                let partial = group
                                    .span(group.tokens.len().saturating_sub(1), group.tokens.len());
                                let empty = group.span(0, 0);
                                for inner in [
                                    None,
                                    Some(&full_inner),
                                    Some(&partial),
                                    Some(&empty),
                                    Some(&invalid_scalar),
                                    Some(&invalid_tokens),
                                ] {
                                    assert_eq!(
                                        contains_span(&side, Some(outer), inner),
                                        legacy_contains_span(&side, Some(outer), inner),
                                        "offset={offset}, outer={outer:?}, inner={inner:?}"
                                    );
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(cases > 50_000);
        Ok(())
    }

    #[test]
    fn containment_miss_keeps_invalid_inner_errors_and_outer_fallbacks() -> Result<()> {
        let blocks = vec![
            anchor_block(5, "A"),
            anchor_block(2, "B"),
            anchor_block(9, "C"),
            anchor_block(10, "D"),
        ];
        let side =
            super::super::SidePlan::inspect("containment negatives", &blocks)?.materialize()?;
        let outer = side.canonical_group(&[BlockId(5)], None).full_span();
        let unrelated = side.canonical_group(&[BlockId(10)], None).full_span();
        assert!(!contains_span(&side, Some(&outer), Some(&unrelated))?);
        let mut unknown = unrelated.clone();
        unknown.blocks = vec![BlockId(77)];
        let mut invalid = unrelated.clone();
        invalid.comparable_range.end = 2;
        let mut malformed = unrelated.clone();
        malformed.blocks = vec![BlockId(10), BlockId(2)];
        malformed.separator = Some(BlockSeparator::PerBoundary([true, false]));
        let mut duplicate = unrelated.clone();
        duplicate.blocks = vec![BlockId(10), BlockId(10)];
        for inner in [&unknown, &invalid, &malformed, &duplicate] {
            let expected = legacy_contains_span(&side, Some(&outer), Some(inner));
            assert!(
                expected.is_err(),
                "invalid source span must remain rejected"
            );
            assert_eq!(contains_span(&side, Some(&outer), Some(inner)), expected);
            assert!(!contains_span(&side, None, Some(inner))?);
        }
        assert!(contains_span(&side, Some(&outer), None)?);
        assert!(contains_span(&side, None, None)?);
        // PerBoundary is malformed for two blocks but the legacy constructor
        // can still assemble that prefix; preserve its inner validation result.
        let mut short_pattern = outer.clone();
        short_pattern.blocks = vec![BlockId(5), BlockId(2)];
        short_pattern.separator = Some(BlockSeparator::PerBoundary([true, false]));
        assert_eq!(
            contains_span(&side, Some(&short_pattern), Some(&unrelated)),
            legacy_contains_span(&side, Some(&short_pattern), Some(&unrelated))
        );
        // Unknown outer IDs and a pattern used past its two stored joins are
        // internal invariant violations. The shortcut must not mask the same
        // legacy panic by returning a structural false before construction.
        let mut unknown_outer = outer.clone();
        unknown_outer.blocks = vec![BlockId(77)];
        let mut long_pattern = outer;
        long_pattern.blocks = vec![BlockId(5), BlockId(2), BlockId(9), BlockId(10)];
        long_pattern.separator = Some(BlockSeparator::PerBoundary([true, false]));
        for invalid_outer in [&unknown_outer, &long_pattern] {
            assert!(
                std::panic::catch_unwind(|| legacy_contains_span(
                    &side,
                    Some(invalid_outer),
                    Some(&unrelated)
                ))
                .is_err()
            );
            assert!(
                std::panic::catch_unwind(|| contains_span(
                    &side,
                    Some(invalid_outer),
                    Some(&unrelated)
                ))
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn light_global_metadata_matches_canonical_group_oracle() -> Result<()> {
        use crate::{model::FontProgramHash, normalize::UnmappedToken};

        const TEXTS: [&str; 8] = ["", "A", " ", "\n", "雪🙂", "123", "XY", ""];
        const SEPARATORS: [BlockSeparator; 6] = [
            BlockSeparator::Concatenate,
            BlockSeparator::Space,
            BlockSeparator::PerBoundary([false, false]),
            BlockSeparator::PerBoundary([false, true]),
            BlockSeparator::PerBoundary([true, false]),
            BlockSeparator::PerBoundary([true, true]),
        ];
        let mut cases = 0;
        for block_count in 0..=3 {
            for mut choices in 0..TEXTS.len().pow(block_count) {
                let mut blocks = Vec::new();
                for position in 0..block_count {
                    let choice = choices % TEXTS.len();
                    choices /= TEXTS.len();
                    let mut block = anchor_block(u64::from(position) + 1, TEXTS[choice]);
                    if choice >= 6 {
                        for scalar_index in [0, TEXTS[choice].chars().count()] {
                            block.canonical.unmapped.push(UnmappedToken {
                                scalar_index,
                                font_hash: FontProgramHash(vec![3, 7, 11]),
                                glyph_id: 17,
                                source: TextSource {
                                    atoms: Vec::new().into(),
                                },
                            });
                        }
                        block.raw = block.canonical.clone();
                    }
                    // Matching masks must never substitute for canonical evidence.
                    block.numeric_mask_applied = choice == 5;
                    block.matching_tokens = Vec::new();
                    let token_count = block.canonical.comparable_token_count()?;
                    if choice != 1 {
                        block.font_size_signatures = Some(
                            (0..token_count)
                                .map(|index| {
                                    crate::normalize::FontSizeSignature::new(&[
                                        9.0,
                                        12.0 + index as f64,
                                        9.0,
                                    ])
                                    .expect("font signature")
                                })
                                .collect(),
                        );
                        block.position_signatures = Some(
                            (0..token_count)
                                .map(|index| {
                                    crate::normalize::PositionSignature::new(
                                        crate::model::Vec2 {
                                            x: index as f64,
                                            y: f64::from(position),
                                        },
                                        crate::model::Vec2 { x: 1.0, y: 0.0 },
                                    )
                                    .expect("position signature")
                                })
                                .collect(),
                        );
                        block.line_breaks = Some((1..token_count).step_by(2).collect());
                        block.page_breaks = Some((1..token_count).step_by(3).collect());
                    }
                    if choice == 3 {
                        block.pages.clear();
                        block.page_breaks = None;
                    } else {
                        block.pages = (0..=block.page_breaks.as_ref().map_or(0, Vec::len))
                            .map(|offset| position + offset as u32)
                            .collect();
                    }
                    blocks.push(block);
                }
                let side =
                    super::super::SidePlan::inspect("metadata oracle", &blocks)?.materialize()?;
                for separator in SEPARATORS {
                    if !separator.valid_for(blocks.len()) {
                        continue;
                    }
                    let ids = blocks.iter().map(|block| block.block).collect::<Vec<_>>();
                    let legacy = side.canonical_group(&ids, Some(separator));
                    let required_work = blocks.len() * 4 + 1;
                    let mut remaining = required_work;
                    let light = light_group_metadata(
                        &side,
                        0..blocks.len(),
                        separator,
                        &mut remaining,
                        64 * 1024 * 1024,
                    )
                    .expect("paid metadata");
                    assert_eq!(remaining, 0);
                    assert_eq!(light.span, legacy.full_span());
                    assert_eq!(
                        light_group_assumptions([&light, &light]).expect("assumptions"),
                        assumptions([&legacy, &legacy]),
                    );
                    assert_eq!(
                        (!light.span.blocks.is_empty()).then_some(light.span.clone()),
                        nonempty_span(&legacy),
                    );
                    let mut remaining = usize::MAX;
                    let plan = plan_group_materialization(
                        &side,
                        0..blocks.len(),
                        &light,
                        &mut remaining,
                        usize::MAX,
                    )
                    .expect("group plan");
                    assert!(plan.bytes >= blocks.len() * std::mem::size_of::<BlockId>());
                    let mut copy_remaining = plan.copy_work;
                    let paid = materialize_paid_group(
                        &side,
                        0..blocks.len(),
                        light,
                        &plan,
                        &mut copy_remaining,
                    )
                    .expect("paid group");
                    assert_eq!(copy_remaining, 0);
                    assert_group_fields_equal(&paid, &legacy);
                    let full = legacy.full_span();
                    let mut view_work = usize::MAX;
                    let [view, _] = borrowed_group_pair(
                        [&side, &side],
                        [&full, &full],
                        &mut view_work,
                        COARSE_MEMORY_BYTES,
                    )
                    .expect("view oracle pair");
                    assert!(
                        view.selected_tokens()
                            .iter()
                            .copied()
                            .eq(legacy.tokens.iter())
                    );
                    for start in 0..=legacy.tokens.len() {
                        for end in start..=legacy.tokens.len() {
                            assert_eq!(view.try_span(start..end), legacy.try_span(start, end));
                        }
                    }

                    for prefix in 0..required_work {
                        let mut remaining = prefix;
                        assert_eq!(
                            light_group_metadata(
                                &side,
                                0..blocks.len(),
                                separator,
                                &mut remaining,
                                usize::MAX,
                            ),
                            Err(MaterializationRefusal::Work)
                        );
                        assert_eq!(remaining, 0);
                    }
                    if !blocks.is_empty() {
                        let mut remaining = required_work;
                        assert_eq!(
                            light_group_metadata(
                                &side,
                                0..blocks.len(),
                                separator,
                                &mut remaining,
                                blocks.len() * std::mem::size_of::<BlockId>() - 1,
                            ),
                            Err(MaterializationRefusal::Memory)
                        );
                        assert_eq!(remaining, required_work);
                    }
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 3_218);
        Ok(())
    }

    fn assert_group_fields_equal(actual: &GroupText, expected: &GroupText) {
        assert_eq!(actual.blocks, expected.blocks);
        assert_eq!(actual.separator, expected.separator);
        assert_eq!(actual.tokens, expected.tokens);
        assert_eq!(actual.font_size_signatures, expected.font_size_signatures);
        assert_eq!(actual.position_signatures, expected.position_signatures);
        assert_eq!(actual.line_breaks, expected.line_breaks);
        assert_eq!(actual.page_breaks, expected.page_breaks);
        assert_eq!(actual.scalar_boundaries, expected.scalar_boundaries);
        assert_eq!(actual.canonical_origin, expected.canonical_origin);
        assert_eq!(actual.comparable_origin, expected.comparable_origin);
    }

    #[test]
    fn light_global_metadata_rejects_invalid_ranges_and_separator_patterns() -> Result<()> {
        let blocks = [anchor_block(1, "A"), anchor_block(2, "B")];
        let side = super::super::SidePlan::inspect("metadata refusal", &blocks)?.materialize()?;
        let mut remaining = 100;
        for (range, separator) in [
            (0..3, BlockSeparator::Space),
            (Range { start: 2, end: 1 }, BlockSeparator::Space),
            (0..2, BlockSeparator::PerBoundary([true, false])),
        ] {
            assert_eq!(
                light_group_metadata(&side, range, separator, &mut remaining, usize::MAX,),
                Err(MaterializationRefusal::Unavailable)
            );
            assert_eq!(remaining, 100);
        }
        let mut metadata = light_group_metadata(
            &side,
            0..2,
            BlockSeparator::Space,
            &mut remaining,
            usize::MAX,
        )
        .expect("valid metadata");
        metadata.span.comparable_range.end = usize::MAX;
        assert!(matches!(
            plan_group_materialization(&side, 0..2, &metadata, &mut remaining, usize::MAX),
            Err(MaterializationRefusal::Memory)
        ));
        assert!(remaining > 0, "capacity overflow is not work exhaustion");
        Ok(())
    }

    #[test]
    fn paid_global_groups_preflight_both_sides_and_nested_payloads() -> Result<()> {
        use crate::{
            model::FontProgramHash,
            normalize::{FontSizeSignature, UnmappedToken},
        };
        let mut old_block = anchor_block(1, "AB");
        old_block.canonical.unmapped.push(UnmappedToken {
            scalar_index: 1,
            font_hash: FontProgramHash(vec![37; 1_000]),
            glyph_id: 9,
            source: TextSource {
                atoms: Vec::new().into(),
            },
        });
        old_block.raw = old_block.canonical.clone();
        let signature = FontSizeSignature::new(&(1..=32).map(f64::from).collect::<Vec<_>>())
            .expect("nested signature");
        old_block.font_size_signatures = Some(vec![signature; 3]);
        let mut new_block = old_block.clone();
        new_block.block = BlockId(101);
        let old_blocks = [old_block];
        let new_blocks = [new_block];
        let old = super::super::SidePlan::inspect("old paid", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new paid", &new_blocks)?.materialize()?;
        let key = DomainKey {
            local: None,
            old: 0..1,
            new: 0..1,
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        };
        let mut remaining = usize::MAX;
        let metadata = light_group_metadata(
            &old,
            0..1,
            BlockSeparator::Space,
            &mut remaining,
            usize::MAX,
        )
        .expect("metadata");
        let plan = plan_group_materialization(&old, 0..1, &metadata, &mut remaining, usize::MAX)
            .expect("plan");
        assert!(plan.bytes >= 1_000 + 3 * 32 * 3 * std::mem::size_of::<u64>());
        let pair_bytes = plan.bytes * 2;
        let mut remaining = usize::MAX;
        let groups = paid_global_groups([&old, &new], &key, &mut remaining, pair_bytes)
            .expect("exact combined memory threshold");
        let required_work = usize::MAX - remaining;
        assert_group_fields_equal(&groups[0], &old.canonical_group(&[BlockId(1)], None));
        assert_group_fields_equal(&groups[1], &new.canonical_group(&[BlockId(101)], None));
        for prefix in 0..required_work {
            let mut remaining = prefix;
            assert!(matches!(
                paid_global_groups([&old, &new], &key, &mut remaining, pair_bytes),
                Err(MaterializationRefusal::Work)
            ));
            assert_eq!(remaining, 0);
        }
        let mut remaining = required_work + 100;
        assert!(matches!(
            paid_global_groups([&old, &new], &key, &mut remaining, pair_bytes - 1),
            Err(MaterializationRefusal::Memory)
        ));
        assert!(
            remaining > 0,
            "memory refusal must not disable later affordable work"
        );
        let mut remaining = 10_000;
        assert!(matches!(
            paid_global_groups([&old, &new], &key, &mut remaining, 1_000),
            Err(MaterializationRefusal::Memory)
        ));
        assert!(remaining > 0);
        Ok(())
    }

    #[test]
    fn paid_global_groups_match_long_uniform_groups() -> Result<()> {
        use crate::{
            model::FontProgramHash,
            normalize::{FontSizeSignature, UnmappedToken},
        };
        let mut blocks = Vec::new();
        for position in 0..12 {
            let text = ["", "A", "", "雪", " ", "", "BC", "", "", "🙂", "", ""][position];
            let mut block = anchor_block(position as u64 + 1, text);
            if position == 7 {
                block.canonical.unmapped.push(UnmappedToken {
                    scalar_index: 0,
                    font_hash: FontProgramHash(vec![17; 31]),
                    glyph_id: 9,
                    source: TextSource {
                        atoms: Vec::new().into(),
                    },
                });
                block.raw = block.canonical.clone();
            }
            let count = block.canonical.comparable_token_count()?;
            block.font_size_signatures = Some(vec![
                FontSizeSignature::new(&[9.0, 12.0])
                    .expect("sizes");
                count
            ]);
            block.line_breaks = Some(Vec::new());
            block.page_breaks = Some(Vec::new());
            block.pages = vec![position as u32 / 3];
            blocks.push(block);
        }
        let side = super::super::SidePlan::inspect("long groups", &blocks)?.materialize()?;
        for count in [8, 12] {
            for separator in [BlockSeparator::Space, BlockSeparator::Concatenate] {
                let key = DomainKey {
                    local: None,
                    old: 0..count,
                    new: 0..count,
                    old_separator: separator,
                    new_separator: separator,
                };
                let legacy = domain_group(&side, 0..count, separator);
                let mut remaining = 100_000;
                let paid =
                    paid_global_groups([&side, &side], &key, &mut remaining, 64 * 1024 * 1024)
                        .expect("long paid group");
                assert_group_fields_equal(&paid[0], &legacy);
                assert_group_fields_equal(&paid[1], &legacy);
            }
        }
        Ok(())
    }

    #[test]
    fn refused_global_keys_share_noncertifying_diagnostic_and_keep_root() -> Result<()> {
        let old_blocks = [
            anchor_block(1, "LLL"),
            anchor_block(2, "AAA"),
            anchor_block(3, "BBB"),
            anchor_block(4, "RRR"),
        ];
        let new_blocks = [
            anchor_block(101, "LLL"),
            anchor_block(102, "BBB"),
            anchor_block(103, "AAA"),
            anchor_block(104, "RRR"),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].old = old_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].new = new_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].evidence.clear();
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let root = assessor.root_relation()?;
        let root_record = assessor.records[root].clone();
        let memory_key = DomainKey {
            local: None,
            old: 1..3,
            new: 1..3,
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        };
        let prior_work = assessor.remaining_work;
        assert_eq!(
            assessor.refuse_global_materialization(
                &memory_key,
                root,
                MaterializationRefusal::Memory
            )?,
            DomainState::Ready
        );
        assert_eq!(assessor.remaining_work, prior_work);
        let stop = assessor.materialization_stop.expect("shared stop");
        assert!(
            !assessor.records[stop]
                .reasons
                .contains(&AssessmentReason::WorkLimit)
        );
        let boundary_key = DomainKey {
            local: None,
            old: 0..1,
            new: 0..1,
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        };
        assert_eq!(assessor.prove_domain(&boundary_key)?, DomainState::Ready);
        assert!(
            assessor.domains[&boundary_key].unique,
            "an earlier memory refusal must not disable affordable independent proofs"
        );
        assessor.remaining_work = 0;
        for range in [1..2, 2..3, 3..4, 0..4] {
            let key = DomainKey {
                local: None,
                old: range.clone(),
                new: range,
                old_separator: BlockSeparator::Space,
                new_separator: BlockSeparator::Space,
            };
            assert_eq!(assessor.prove_domain(&key)?, DomainState::Ready);
            let proof = &assessor.domains[&key];
            assert_eq!(proof.relation, stop);
            assert_eq!(proof.scope, ProofScope::BroadRootRefusal);
            assert_eq!(proof.exact_lengths(), None);
            assert!(
                !proof.unique
                    && !proof.strict_unique
                    && proof.edits.is_empty()
                    && proof.stable_events.is_none()
            );
            assert_eq!(proof.search, SearchCompleteness::Incomplete);
        }
        assert_eq!(assessor.records[root], root_record);
        assert_eq!(
            assessor.records.len(),
            3,
            "root, shared stop and affordable exact domain only"
        );
        assert_eq!(assessor.records[stop].parent, Some(root));
        assert_eq!(assessor.records[stop].old_span, root_record.old_span);
        assert_eq!(assessor.records[stop].new_span, root_record.new_span);
        assert!(
            assessor.records[stop]
                .reasons
                .contains(&AssessmentReason::WorkLimit)
        );
        assert!(
            assessor.records[stop]
                .reasons
                .contains(&AssessmentReason::CompetingCorrespondence)
        );
        let proposal = ProposedRelation {
            old: Some(old.canonical_group(&[BlockId(4)], None).full_span()),
            new: Some(new.canonical_group(&[BlockId(104)], None).full_span()),
            span_indices: [Some(0), Some(0)],
            exact_recovery: true,
        };
        let child = assessor.assess(&proposal)?;
        assert_eq!(assessor.records[child].parent, Some(stop));
        assert_eq!(assessor.records[child].old_span, proposal.old);
        assert_eq!(assessor.records[child].new_span, proposal.new);
        assert_eq!(assessor.records[child].outcome, RelationOutcome::Tentative);
        assert_eq!(
            assessor.records[child].search,
            SearchCompleteness::Incomplete
        );
        assert_eq!(assessor.records[root], root_record);
        alignment.spans[0].evidence = vec![
            AlignmentEvidence::ReadingOrderUnknown,
            AlignmentEvidence::ExtractionGap,
            AlignmentEvidence::NormalizationIssue,
        ];
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let root = assessor.root_relation()?;
        let root_record = assessor.records[root].clone();
        assessor.remaining_work = 0;
        assert_eq!(assessor.prove_domain(&memory_key)?, DomainState::Ready);
        let stop = assessor
            .materialization_stop
            .expect("source-barrier refusal");
        for reason in [
            AssessmentReason::UnknownReadingOrder,
            AssessmentReason::ExtractionGap,
            AssessmentReason::NormalizationUncertainty,
            AssessmentReason::WorkLimit,
        ] {
            assert!(assessor.records[stop].reasons.contains(&reason));
        }
        assert_eq!(assessor.records[root], root_record);
        Ok(())
    }

    #[test]
    fn bounded_global_output_stop_precedes_root_and_cached_domain() -> Result<()> {
        let old_blocks = [
            anchor_block(1, "LLL"),
            anchor_block(2, "AAA"),
            anchor_block(3, "BBB"),
            anchor_block(4, "RRR"),
        ];
        let new_blocks = [
            anchor_block(101, "LLL"),
            anchor_block(102, "BBB"),
            anchor_block(103, "AAA"),
            anchor_block(104, "RRR"),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].old = old_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].new = new_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].evidence.clear();
        let key = DomainKey {
            local: None,
            old: 0..1,
            new: 0..1,
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        };
        for cap in [1, 2] {
            for available_work in [0, 10_000] {
                let options = DiffOptions {
                    max_assessment_ranges: cap,
                    ..DiffOptions::default()
                };
                let mut assessor = Assessor::new([&old, &new], &alignment, None, options)?;
                assessor.remaining_work = available_work;
                assert_eq!(assessor.prove_domain(&key)?, DomainState::Stopped(cap - 1));
                assert_eq!(assessor.remaining_work, available_work);
                assert!(assessor.domains.is_empty());
                assert!(assessor.materialization_stop.is_none());
                let sentinel = &assessor.records[cap - 1];
                assert_eq!(sentinel.reasons, vec![AssessmentReason::OutputLimit]);
                assert_eq!(sentinel.parent, None);
                assert_eq!(sentinel.search, SearchCompleteness::Incomplete);
                assert!(sentinel.assumptions.is_empty());
                assert_eq!(
                    sentinel.old_span,
                    Some(domain_group(&old, 0..4, BlockSeparator::Space).full_span())
                );
                assert_eq!(
                    sentinel.new_span,
                    Some(domain_group(&new, 0..4, BlockSeparator::Space).full_span())
                );
                let work = assessor.remaining_work;
                assert_eq!(assessor.prove_domain(&key)?, DomainState::Stopped(cap - 1));
                assert_eq!(assessor.remaining_work, work);
                assert_eq!(assessor.records.len(), cap);
            }
        }
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assert_eq!(assessor.prove_domain(&key)?, DomainState::Ready);
        let cached = assessor.domains[&key].relation;
        assessor.options.max_assessment_ranges = assessor.records.len() + 1;
        assert_eq!(assessor.prove_domain(&key)?, DomainState::Ready);
        let available_work = assessor.remaining_work;
        let proposal = ProposedRelation {
            old: Some(old.canonical_group(&[BlockId(1)], None).full_span()),
            new: Some(new.canonical_group(&[BlockId(101)], None).full_span()),
            span_indices: [Some(0), Some(0)],
            exact_recovery: true,
        };
        let sentinel = assessor.assess(&proposal)?;
        assert_eq!(assessor.remaining_work, available_work);
        assert!(matches!(
            assessor.scoped_proof_groups(&key)?,
            Err(MaterializationRefusal::Unavailable)
        ));
        assert_eq!(assessor.remaining_work, available_work);
        assert_eq!(assessor.prove_domain(&key)?, DomainState::Stopped(sentinel));
        assert_eq!(assessor.domains[&key].relation, cached);
        Ok(())
    }

    #[test]
    fn mandatory_global_anchors_keep_internal_alternatives_and_source_barriers() -> Result<()> {
        let old_blocks = [
            anchor_block(1, "LLL"),
            anchor_block(2, "AAA"),
            anchor_block(3, "BBB"),
            anchor_block(4, "RRR"),
        ];
        let new_blocks = [
            anchor_block(101, "LLL"),
            anchor_block(102, "BBB"),
            anchor_block(103, "AAA"),
            anchor_block(104, "RRR"),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        for barrier in [
            None,
            Some(AlignmentEvidence::ReadingOrderUnknown),
            Some(AlignmentEvidence::ExtractionGap),
        ] {
            let mut alignment = issue_alignment();
            alignment.spans[0].old = old_blocks.iter().map(|block| block.block).collect();
            alignment.spans[0].new = new_blocks.iter().map(|block| block.block).collect();
            alignment.spans[0].evidence = barrier.into_iter().collect();
            let mut assessor =
                Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
            assert_eq!(assessor.anchors, vec![(0, 0), (3, 3)]);
            assert_eq!(assessor.anchor_work.order_unique, Some(false));
            let proposal = ProposedRelation {
                old: Some(
                    old.canonical_group(&[BlockId(2), BlockId(3)], Some(BlockSeparator::Space))
                        .full_span(),
                ),
                new: Some(
                    new.canonical_group(&[BlockId(102), BlockId(103)], Some(BlockSeparator::Space))
                        .full_span(),
                ),
                span_indices: [Some(0), Some(0)],
                exact_recovery: false,
            };
            let key = assessor.domain_key(&proposal)?;
            assert_eq!((&key.old, &key.new), (&(1..3), &(1..3)));
            assessor.prove_domain(&key)?;
            let proof = &assessor.domains[&key];
            assert!(
                !proof.unique,
                "an internal swap must retain competing edit paths"
            );
            assert!(!proof.strict_unique);
            let relation = &assessor.records[proof.relation];
            assert_eq!(relation.outcome, RelationOutcome::Tentative);
            assert!(
                relation
                    .reasons
                    .contains(&AssessmentReason::CompetingCorrespondence)
            );
            assert!(
                relation
                    .reasons
                    .contains(&AssessmentReason::DomainNotClosed)
            );
            if barrier.is_none() {
                let boundary = ProposedRelation {
                    old: Some(old.canonical_group(&[BlockId(1)], None).full_span()),
                    new: Some(new.canonical_group(&[BlockId(101)], None).full_span()),
                    span_indices: [Some(0), Some(0)],
                    exact_recovery: false,
                };
                let boundary_key = assessor.domain_key(&boundary)?;
                assessor.prove_domain(&boundary_key)?;
                assert!(assessor.domains[&boundary_key].strict_unique);
                assert_eq!(
                    assessor.records[assessor.domains[&boundary_key].relation].outcome,
                    RelationOutcome::Established
                );
            }
        }
        Ok(())
    }

    #[test]
    fn mandatory_global_anchor_windows_retain_crossing_correspondences() -> Result<()> {
        let old_blocks = [
            anchor_block(1, "LLL"),
            anchor_block(2, "AAA"),
            anchor_block(3, "BBB"),
            anchor_block(4, "CCC"),
            anchor_block(5, "RRR"),
        ];
        let new_blocks = [
            anchor_block(101, "LLL"),
            anchor_block(102, "CCC"),
            anchor_block(103, "BBB"),
            anchor_block(104, "RRR"),
            anchor_block(105, "AAA"),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].old = old_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].new = new_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].evidence = vec![AlignmentEvidence::ReadingOrderUnknown];
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assert_eq!(assessor.anchors, vec![(0, 0), (4, 3)]);
        assert_eq!(assessor.anchor_alternatives, vec![(1, 4), (2, 2), (3, 1)]);
        let proposal = ProposedRelation {
            old: Some(old.canonical_group(&[BlockId(2)], None).full_span()),
            new: Some(new.canonical_group(&[BlockId(105)], None).full_span()),
            span_indices: [Some(0), Some(0)],
            exact_recovery: true,
        };
        let key = assessor.domain_key(&proposal)?;
        assert_eq!((&key.old, &key.new), (&(1..5), &(1..5)));
        assessor.prove_domain(&key)?;
        assert!(!assessor.domains[&key].unique);
        assert!(
            assessor.records[assessor.domains[&key].relation]
                .reasons
                .contains(&AssessmentReason::DomainNotClosed)
        );
        Ok(())
    }

    #[test]
    fn mandatory_global_anchors_reject_repeated_needles_outside_the_window() -> Result<()> {
        let old_blocks = [
            anchor_block(1, "LLL"),
            anchor_block(2, "AAA"),
            anchor_block(3, "BBB"),
            anchor_block(4, "RRR"),
            anchor_block(5, "xAAAy"),
        ];
        let new_blocks = [
            anchor_block(101, "LLL"),
            anchor_block(102, "BBB"),
            anchor_block(103, "AAA"),
            anchor_block(104, "RRR"),
            anchor_block(105, "z"),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        let assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assert_eq!(assessor.anchors, vec![(0, 0), (2, 1), (3, 3)]);
        assert_eq!(assessor.anchor_work.order_unique, Some(true));
        Ok(())
    }

    #[test]
    fn mandatory_global_anchors_do_not_replace_block_correspondence_with_character_weight()
    -> Result<()> {
        let old_blocks = [
            anchor_block(1, "LLL"),
            anchor_block(2, "AAAAAAAAAAAA"),
            anchor_block(3, "B"),
            anchor_block(4, "RRR"),
        ];
        let new_blocks = [
            anchor_block(101, "LLL"),
            anchor_block(102, "B"),
            anchor_block(103, "AAAAAAAAAAAA"),
            anchor_block(104, "RRR"),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].old = old_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].new = new_blocks.iter().map(|block| block.block).collect();
        alignment.spans[0].evidence.clear();
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        assert_eq!(assessor.anchors, vec![(0, 0), (3, 3)]);
        assessor.discover_ordered_domains()?;
        assert!(
            assessor.local_domains.is_empty(),
            "global ambiguity cannot become a local closure"
        );
        let proposal = ProposedRelation {
            old: Some(
                old.canonical_group(&[BlockId(2), BlockId(3)], Some(BlockSeparator::Space))
                    .full_span(),
            ),
            new: Some(
                new.canonical_group(&[BlockId(102), BlockId(103)], Some(BlockSeparator::Space))
                    .full_span(),
            ),
            span_indices: [Some(0), Some(0)],
            exact_recovery: false,
        };
        let key = assessor.domain_key(&proposal)?;
        let [old_group, new_group] = proof_groups(assessor.sides, &key)?;
        assert_eq!(
            exact::check(&old_group.tokens, &new_group.tokens, &mut 1_000_000)?,
            exact::ExactUniqueness::Unique
        );
        assessor.prove_domain(&key)?;
        assert!(!assessor.domains[&key].unique);
        assert_eq!(
            assessor.records[assessor.domains[&key].relation].outcome,
            RelationOutcome::Tentative
        );
        // A fresh key with an unfinished alternative scan cannot claim even
        // complete closure search, including when no source flag is present.
        assessor.domains.clear();
        assessor.remaining_work = 0;
        assessor.prove_domain(&key)?;
        let refused = &assessor.records[assessor.domains[&key].relation];
        assert_eq!(refused.search, SearchCompleteness::Incomplete);
        assert!(refused.reasons.contains(&AssessmentReason::WorkLimit));
        let parent = assessor.domains[&key].relation;
        let root = assessor.root_relation()?;
        let root_record = assessor.records[root].clone();
        assert_eq!(root_record.outcome, RelationOutcome::Established);
        let child = assessor.assess(&proposal)?;
        assert_eq!(assessor.records[child].parent, Some(parent));
        assert_eq!(assessor.records[child].outcome, RelationOutcome::Tentative);
        assert!(
            assessor.records[child]
                .reasons
                .contains(&AssessmentReason::WorkLimit)
        );
        assert_eq!(
            assessor.records[child].search,
            SearchCompleteness::Incomplete
        );
        assert_eq!(assessor.records[root], root_record);
        assert!(!assessor.domains[&key].unique);
        assert!(!assessor.domains[&key].strict_unique);
        Ok(())
    }

    #[test]
    fn inherited_source_uncertainty_keeps_completed_child_search() -> Result<()> {
        let old_blocks = [issue_block(1)];
        let new_blocks = [issue_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let proposal = ProposedRelation {
            old: Some(old.canonical_group(&[BlockId(1)], None).full_span()),
            new: Some(new.canonical_group(&[BlockId(101)], None).full_span()),
            span_indices: [Some(0), Some(0)],
            exact_recovery: false,
        };
        let child = assessor.assess(&proposal)?;
        let record = &assessor.records[child];
        let parent = &assessor.records[record.parent.expect("domain parent")];
        assert_eq!(parent.search, SearchCompleteness::Complete);
        assert!(
            parent
                .reasons
                .contains(&AssessmentReason::NormalizationUncertainty)
        );
        assert_eq!(record.search, SearchCompleteness::Complete);
        assert_eq!(record.outcome, RelationOutcome::Tentative);
        assert!(
            record
                .reasons
                .contains(&AssessmentReason::NormalizationUncertainty)
        );
        assert!(!record.reasons.contains(&AssessmentReason::WorkLimit));
        Ok(())
    }

    #[test]
    fn retained_suffix_distinct_domains_preserve_source_proof_and_cache_costs() -> Result<()> {
        let old_blocks = [
            anchor_block(1, &"AB".repeat(64)),
            anchor_block(11, &"CD".repeat(64)),
        ];
        let new_blocks = [
            anchor_block(101, &"AB".repeat(63)),
            anchor_block(111, &"CD".repeat(63)),
        ];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].old = vec![BlockId(1), BlockId(11)];
        alignment.spans[0].new = vec![BlockId(101), BlockId(111)];
        alignment.spans[0].evidence.clear();
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        for (index, (old_id, new_id)) in [(1, 101), (11, 111)].into_iter().enumerate() {
            let key = DomainKey {
                local: Some((local_span(old_id, 0, 128), local_span(new_id, 0, 126))),
                old: index..index + 1,
                new: index..index + 1,
                old_separator: BlockSeparator::Space,
                new_separator: BlockSeparator::Space,
            };
            let before = assessor.remaining_work;
            assert_eq!(assessor.prove_domain(&key)?, DomainState::Ready);
            let after = assessor.remaining_work;
            assert_eq!(before - after, 42048);
            assert_eq!(assessor.suffix_reuse_work.attempts, index + 1);
            assert_eq!(
                assessor.suffix_reuse_work.allowance_used,
                16128 * (index + 1)
            );
            assert_eq!(
                assessor.suffix_reuse_work.recomputed_cells,
                8002 * (index + 1)
            );
            assert_eq!(assessor.suffix_reuse_work.binding_work, 3 * (index + 1));
            assert_eq!(assessor.suffix_reuse_work.accounting_work, 16 * (index + 1));
            assert_eq!(
                assessor.suffix_reuse_work.unused_allowance,
                8107 * (index + 1)
            );
            assert_eq!(assessor.prove_domain(&key)?, DomainState::Ready);
            assert_eq!(assessor.remaining_work, after);
            let [old_group, new_group] = proof_groups(assessor.sides, &key)?;
            let oracle = semantic::check_hunks(
                &old_group.tokens,
                &new_group.tokens,
                &mut 1_000_000,
                |edits, work| {
                    semantic_signature(
                        assessor.sides,
                        [&old_group, &new_group],
                        edits,
                        work,
                        assessor.options.max_assessment_ranges,
                    )
                },
            )?;
            let proof = assessor.domains.get(&key).expect("cached domain");
            assert!(!proof.strict_unique);
            match oracle {
                semantic::Outcome::Unique { signature, edits } => {
                    assert!(proof.unique);
                    assert_eq!(proof.stable_events.as_ref(), Some(&signature));
                    assert_eq!(proof.edits, edits);
                }
                semantic::Outcome::Ambiguous => {
                    assert!(!proof.unique);
                    assert!(proof.stable_events.is_none());
                }
                semantic::Outcome::BudgetExceeded => {
                    assert!(!proof.unique);
                    assert_eq!(proof.search, SearchCompleteness::Incomplete);
                }
            }
            eprintln!(
                "RETAINED_SUFFIX_DOMAIN index={index} original_cost=42048 allowance_cost={} actual_suffix_cells=8002 unused_allowance=8107 binding_paid=3 accounting_paid=16 triggering_cell_recomputed=true source_signature_oracle_preserved=true same_key_repeat_work=0",
                before - after
            );
        }
        Ok(())
    }

    fn issue_alignment() -> Alignment {
        Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: vec![BlockId(1)],
                new: vec![BlockId(101)],
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: vec![AlignmentEvidence::NormalizationIssue],
                old_separator: None,
                new_separator: None,
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        }
    }

    fn local_span(block: u64, start: usize, end: usize) -> TextSpan {
        TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: TokenRange { start, end },
        }
    }

    fn local_key(old: TextSpan, new: TextSpan) -> DomainKey {
        DomainKey {
            local: Some((old, new)),
            old: 0..0,
            new: 0..0,
            old_separator: BlockSeparator::Concatenate,
            new_separator: BlockSeparator::Concatenate,
        }
    }

    #[test]
    fn optional_discovery_refusal_preserves_settlement_work_and_incomplete_search() -> Result<()> {
        let old_blocks = [issue_block(1)];
        let new_blocks = [issue_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        let intervals = [None];
        let recovery = super::super::SentenceRecoveryInput {
            old_trusted_run_intervals: &intervals,
            new_trusted_run_intervals: &intervals,
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        for (limit, available, reserved) in [(160, 11, 10), (15, 1, 0)] {
            let options = DiffOptions {
                max_assessment_work: limit,
                ..DiffOptions::default()
            };
            let mut assessor = Assessor::new_with_evidence(
                [&old, &new],
                &alignment,
                Some(recovery),
                options,
                None,
            )?;
            assessor.remaining_work = available;
            // The source issue index cannot fit in the one-unit optional cap.
            assessor.discover_local_domains(&[])?;
            assert_eq!(assessor.remaining_work, reserved);
            assert_eq!(assessor.local_view_work.budget_cap, 1);
            assert_eq!(assessor.local_view_work.settlement_reserve, reserved);
            assert!(assessor.local_view_work.cap_exhausted);
            assert_eq!(assessor.local_view_work.total(), 1);
            assert!(assessor.local_domains.is_empty());
            let root = assessor.root_relation()?;
            assert_eq!(
                assessor.records[root].search,
                SearchCompleteness::Incomplete
            );
            assert_ne!(assessor.records[root].outcome, RelationOutcome::Established);
        }
        Ok(())
    }

    #[test]
    fn optional_discovery_stop_preserves_established_root_and_deduplicates() -> Result<()> {
        let old_blocks = [plain_block(1)];
        let new_blocks = [plain_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].evidence.clear();
        let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
        let root = assessor.root_relation()?;
        assert_eq!(assessor.records[root].outcome, RelationOutcome::Established);
        let completed = assessor.records[root].clone();
        assessor.record_optional_search_stop(AssessmentReason::SearchIncomplete)?;
        let count = assessor.records.len();
        assessor.record_optional_search_stop(AssessmentReason::WorkLimit)?;
        assert_eq!(assessor.records.len(), count);
        assert_eq!(assessor.records[root], completed);
        let stopped = &assessor.records[assessor
            .optional_search_stop
            .expect("optional stop recorded")];
        assert_eq!(stopped.parent, None);
        assert_eq!(stopped.outcome, RelationOutcome::Tentative);
        assert_eq!(stopped.search, SearchCompleteness::Incomplete);
        assert_eq!(
            stopped.reasons,
            vec![
                AssessmentReason::SearchIncomplete,
                AssessmentReason::WorkLimit
            ]
        );
        Ok(())
    }

    #[test]
    fn optional_discovery_footer_refusal_preserves_established_proof_and_reserve() -> Result<()> {
        let footer_block = |id| {
            let mut block = plain_block(id);
            block.canonical.text = "Cat.".to_owned();
            let mut entry = block.canonical.source_map[2].clone();
            entry.output_range = ScalarRange { start: 3, end: 4 };
            block.canonical.source_map.push(entry);
            block.raw = block.canonical.clone();
            block.matching = block.canonical.text.clone();
            block.matching_tokens = block
                .canonical
                .comparable_tokens()
                .expect("footer fixture tokens");
            block
        };
        let old_blocks = [footer_block(1)];
        let new_blocks = [footer_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans[0].evidence.clear();
        let intervals = [None];
        let recovery = super::super::SentenceRecoveryInput {
            old_trusted_run_intervals: &intervals,
            new_trusted_run_intervals: &intervals,
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let options = DiffOptions {
            max_assessment_work: 160,
            ..DiffOptions::default()
        };
        let mut assessor = Assessor::new([&old, &new], &alignment, Some(recovery), options)?;
        let root = assessor.root_relation()?;
        assert_eq!(assessor.records[root].outcome, RelationOutcome::Established);
        let completed = assessor.records[root].clone();
        assessor.remaining_work = 1;
        assessor.discover_footer_domains()?;
        assert_eq!(assessor.remaining_work, 0);
        assert_eq!(assessor.local_view_work.footer_search, 1);
        assert_eq!(assessor.records[root], completed);
        assert!(
            assessor.records[assessor
                .optional_search_stop
                .expect("optional stop recorded")]
            .reasons
            .contains(&AssessmentReason::WorkLimit)
        );
        assessor.remaining_work = 11;
        assessor.discover_local_domains(&[])?;
        assert_eq!(assessor.remaining_work, 10);
        assert!(assessor.local_view_work.cap_exhausted);
        assert_eq!(assessor.records[root], completed);
        Ok(())
    }

    #[test]
    fn optional_discovery_stop_preserves_empty_output_limit_sentinel() -> Result<()> {
        let old = super::super::SidePlan::inspect("old", &[])?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &[])?.materialize()?;
        let mut alignment = issue_alignment();
        alignment.spans.clear();
        let options = DiffOptions {
            max_assessment_ranges: 1,
            ..DiffOptions::default()
        };
        let mut assessor = Assessor::new([&old, &new], &alignment, None, options)?;
        assessor.record_optional_search_stop(AssessmentReason::WorkLimit)?;
        assessor.record_optional_search_stop(AssessmentReason::SearchIncomplete)?;
        assert_eq!(assessor.records.len(), 1);
        assert!(validation::is_output_limit_sentinel(&assessor.records[0]));
        assert_eq!(
            assessor.records[0].reasons,
            vec![AssessmentReason::OutputLimit]
        );
        Ok(())
    }

    #[test]
    fn optional_discovery_error_restores_unspent_shared_work() -> Result<()> {
        let old_blocks = [issue_block(1)];
        let new_blocks = [issue_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        let options = DiffOptions {
            max_assessment_work: 160,
            ..DiffOptions::default()
        };
        let recovery = super::super::SentenceRecoveryInput {
            old_trusted_run_intervals: &[],
            new_trusted_run_intervals: &[],
            old_native_order_blocks: &[],
            new_native_order_blocks: &[],
            old_trusted_run_evidence: None,
            new_trusted_run_evidence: None,
            min_tokens: 1,
            enable_known_span_sentence_shadow: false,
            enable_sentence_edge_gate_shadow: false,
        };
        let mut assessor =
            Assessor::new_with_evidence([&old, &new], &alignment, Some(recovery), options, None)?;
        assessor.remaining_work = 11;
        assert!(assessor.discover_local_domains(&[]).is_err());
        assert_eq!(assessor.remaining_work, 11);
        assert_eq!(assessor.local_view_work.total(), 0);
        Ok(())
    }

    #[test]
    fn witnessed_impossible_cut_vetoes_the_invariant_proof_on_the_real_path() -> Result<()> {
        // Unequal parents keep a mandatory equal prefix so the off-diagonal
        // start cut has a crossing witness, while the later differing token
        // forces path enumeration when the veto is disabled. At the test
        // budget the optional localization charge fits but the 24x24 suffix
        // table does not, so only the witness can answer without enumeration.
        let blocks: Vec<_> = (1u64..=8).map(plain_block).collect();
        let mut new_blocks: Vec<_> = (101u64..=107).map(plain_block).collect();
        let ab_d_block = |id: u64| {
            let mut block = plain_block(id);
            block.canonical.text = "ABD".to_owned();
            block.raw.text = "ABD".to_owned();
            block.matching = "ABD".to_owned();
            block.matching_tokens = block
                .canonical
                .comparable_tokens()
                .expect("ABD comparable tokens");
            block
        };
        new_blocks.push(ab_d_block(108));
        let old = super::super::SidePlan::inspect("old", &blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: (1u64..=8).map(BlockId).collect(),
                new: (101u64..=108).map(BlockId).collect(),
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: Vec::new(),
                old_separator: None,
                new_separator: None,
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let key = DomainKey {
            local: None,
            old: 0..8,
            new: 0..8,
            old_separator: BlockSeparator::Concatenate,
            new_separator: BlockSeparator::Concatenate,
        };
        let off_diagonal = ProposedRelation {
            old: Some(local_span(1, 1, 2)),
            new: Some(local_span(101, 0, 1)),
            span_indices: [None, None],
            exact_recovery: false,
        };
        let diagonal = ProposedRelation {
            old: Some(local_span(1, 0, 2)),
            new: Some(local_span(101, 0, 2)),
            span_indices: [None, None],
            exact_recovery: false,
        };
        fn fresh<'a, 'document>(
            old: &'a super::super::Side<'document>,
            new: &'a super::super::Side<'document>,
            alignment: &'a Alignment,
        ) -> Result<Assessor<'a, 'document>> {
            Assessor::new_with_evidence([old, new], alignment, None, DiffOptions::default(), None)
        }
        // The cached negative answers NotInvariant within 200 units while the
        // same state without the veto exhausts on the suffix table.
        let mut veto = fresh(&old, &new, &alignment)?;
        assert!(
            veto.forced_equal_child(&key, &diagonal, &mut None)?
                .is_some(),
            "the diagonal sibling must populate the cached analysis"
        );
        assert!(veto.mandatory_analyses.contains_key(&key));
        veto.remaining_work = 200;
        assert_eq!(
            veto.proposal_edits_are_invariant(&off_diagonal, &key)?,
            ProposalProof::NotInvariant,
            "the cached crossing witness must veto without enumeration budget"
        );
        let mut fallback = fresh(&old, &new, &alignment)?;
        assert!(
            fallback
                .forced_equal_child(&key, &diagonal, &mut None)?
                .is_some()
        );
        fallback.remaining_work = 200;
        assert_eq!(
            fallback.proposal_edits_are_invariant(&diagonal, &key)?,
            ProposalProof::Exhausted
        );
        // Cached but unaffordable optional work preserves the remainder: the
        // helper refuses without spending and the ordinary path then charges.
        let mut tight = fresh(&old, &new, &alignment)?;
        assert!(
            tight
                .forced_equal_child(&key, &diagonal, &mut None)?
                .is_some()
        );
        tight.remaining_work = 10;
        let tight_before = tight.remaining_work;
        let [tight_old, tight_new] = proof_groups(tight.sides, &key)?;
        assert!(!tight.witnessed_impossible_cut(&key, &off_diagonal, [&tight_old, &tight_new])?);
        assert_eq!(
            tight.remaining_work, tight_before,
            "an unaffordable optional localization must not spend"
        );
        // Boundary consumer: empty sidecars activate the negative-only path,
        // so no exact-displacement mapping work is needed to reach the veto.
        let mut boundary = Assessor::new_with_evidence(
            [&old, &new],
            &alignment,
            None,
            DiffOptions::default(),
            Some(ExactDisplacementInput { old: &[], new: &[] }),
        )?;
        assert!(
            boundary
                .forced_equal_child(&key, &diagonal, &mut None)?
                .is_some()
        );
        assert!(boundary.mandatory_analyses.contains_key(&key));
        boundary.remaining_work = 250;
        assert!(
            matches!(
                boundary.boundary_displacement_proof(&off_diagonal, &key)?,
                BoundaryDisplacement::NotProven
            ),
            "the boundary consumer must reject a witnessed cut before enumeration"
        );
        // Cache miss falls back unchanged.
        let mut miss = Assessor::new_with_evidence(
            [&old, &new],
            &alignment,
            None,
            DiffOptions::default(),
            None,
        )?;
        miss.remaining_work = 0;
        assert_eq!(
            miss.proposal_edits_are_invariant(&off_diagonal, &key)?,
            ProposalProof::Exhausted
        );
        Ok(())
    }

    #[test]
    fn forced_equal_claim_is_vetoed_by_crossing_change_and_move() -> Result<()> {
        use crate::diff::{Change, ChangeKind, ChangeOccurrence, Confidence};
        let blocks = [plain_block(1), plain_block(2)];
        let new_blocks = [plain_block(101), plain_block(102)];
        let old = super::super::SidePlan::inspect("old", &blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = Alignment {
            spans: vec![AlignmentSpan {
                kind: AlignmentKind::Unresolved,
                old: vec![BlockId(1)],
                new: vec![BlockId(101)],
                score: 0.0,
                canonical_similarity: 0.0,
                score_margin: None,
                confidence: AlignmentConfidence::Low,
                evidence: Vec::new(),
                old_separator: None,
                new_separator: None,
            }],
            main_anchors: Vec::new(),
            move_candidates: Vec::new(),
        };
        let relation_old = local_span(1, 1, 3);
        let relation_new = local_span(101, 1, 3);
        let key = |relation_old: &TextSpan, relation_new: &TextSpan| DomainKey {
            local: Some((relation_old.clone(), relation_new.clone())),
            old: 0..1,
            new: 0..1,
            old_separator: BlockSeparator::Concatenate,
            new_separator: BlockSeparator::Concatenate,
        };
        let make_assessor = || -> Result<Assessor<'_, '_>> {
            let mut assessor = Assessor::new_with_evidence(
                [&old, &new],
                &alignment,
                None,
                DiffOptions::default(),
                None,
            )?;
            let index = assessor.records.len();
            assessor.records.push(RelationAssessment {
                old_span: Some(relation_old.clone()),
                new_span: Some(relation_new.clone()),
                parent: None,
                outcome: RelationOutcome::Established,
                search: SearchCompleteness::Complete,
                assumptions: vec![ComparisonAssumption::MandatoryMatchingEquality],
                reasons: Vec::new(),
            });
            let local_key = key(&relation_old, &relation_new);
            assessor.domains.insert(
                local_key.clone(),
                DomainProof {
                    scope: ProofScope::ExactKey,
                    relation: index,
                    unique: true,
                    search: SearchCompleteness::Complete,
                    edits: Vec::new(),
                    lengths: [0, 0],
                    strict_unique: false,
                    stable_events: Some(Vec::new()),
                },
            );
            assessor.semantic_acceptance.insert(index, local_key);
            assessor.forced_equal_relations.insert(index);
            Ok(assessor)
        };
        // The crossing occurrence overlaps tokens 1..2 of the claim without
        // either span containing the other, so the old contained comparison
        // and the old validator never see it.
        let crossing = Change {
            kind: ChangeKind::Replacement,
            occurrences: vec![ChangeOccurrence {
                old_span: Some(local_span(1, 0, 2)),
                new_span: Some(local_span(101, 0, 2)),
            }],
            confidence: Confidence::High,
            tags: Vec::new(),
        };
        assert!(
            !contains_span(
                &old,
                Some(&relation_old),
                crossing.occurrences[0].old_span.as_ref()
            )?,
            "the crossing occurrence must not be contained"
        );
        let mut assessor = make_assessor()?;
        assert!(
            !assessor.validate_semantic_emission(0, &[crossing])?,
            "a crossing replacement must veto the forced-equal claim"
        );
        assert_eq!(assessor.records[0].outcome, RelationOutcome::Tentative);
        assert!(
            assessor.records[0]
                .reasons
                .contains(&AssessmentReason::AmbiguousEditLocation)
        );
        // The old validator skips moves entirely, so only the wider veto can
        // protect the claim from a crossing move.
        let crossing_move = Change {
            kind: ChangeKind::Move,
            occurrences: vec![ChangeOccurrence {
                old_span: Some(local_span(1, 0, 2)),
                new_span: Some(local_span(102, 0, 2)),
            }],
            confidence: Confidence::High,
            tags: Vec::new(),
        };
        let mut moved = make_assessor()?;
        assert!(
            !moved.validate_semantic_emission(0, &[crossing_move])?,
            "a crossing move must veto the forced-equal claim"
        );
        assert_eq!(moved.records[0].outcome, RelationOutcome::Tentative);
        // A change on an unrelated block does not overlap and stays clean.
        let unrelated = Change {
            kind: ChangeKind::Replacement,
            occurrences: vec![ChangeOccurrence {
                old_span: Some(local_span(2, 0, 1)),
                new_span: Some(local_span(102, 0, 1)),
            }],
            confidence: Confidence::High,
            tags: Vec::new(),
        };
        let mut clean = make_assessor()?;
        assert!(
            clean.validate_semantic_emission(0, &[unrelated])?,
            "an unrelated change must not veto the claim"
        );
        Ok(())
    }

    #[test]
    fn assessor_local_issue_cache_matches_uncached_across_prime_order() -> Result<()> {
        let old_blocks = [issue_block(1)];
        let new_blocks = [issue_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        // Canonical 0..1 avoids the issue at 1..2; canonical 1..3 touches it.
        let avoiding = local_key(local_span(1, 0, 1), local_span(101, 0, 1));
        let touching = local_key(local_span(1, 1, 3), local_span(101, 1, 3));

        let mut first = Assessor::new_with_evidence(
            [&old, &new],
            &alignment,
            None,
            DiffOptions::default(),
            None,
        )?;
        let avoiding_first = first.domain_reasons(&avoiding)?;
        let touching_first = first.domain_reasons(&touching)?;
        let avoiding_again = first.domain_reasons(&avoiding)?;

        let mut second = Assessor::new_with_evidence(
            [&old, &new],
            &alignment,
            None,
            DiffOptions::default(),
            None,
        )?;
        let touching_second = second.domain_reasons(&touching)?;
        let avoiding_second = second.domain_reasons(&avoiding)?;

        assert_eq!(avoiding_first, avoiding_again);
        assert_eq!(avoiding_first, avoiding_second);
        assert_eq!(touching_first, touching_second);
        assert!(avoiding_first.1, "the avoiding interval clears the barrier");
        assert_eq!(avoiding_first.0, Vec::<AssessmentReason>::new());
        assert!(!touching_first.1, "the touching interval keeps the barrier");
        assert_eq!(
            touching_first.0,
            vec![AssessmentReason::NormalizationUncertainty]
        );

        // The repeated validations were served by the cache: two old-side
        // hits and one new-side hit.
        let cache = first.issue_cache.as_ref().expect("cache was created");
        assert_eq!(cache.sides[0].hits.get(), 2);
        assert_eq!(cache.sides[1].hits.get(), 1);
        Ok(())
    }

    #[test]
    fn assessor_local_issue_cache_does_not_leak_raw_proof() -> Result<()> {
        let old_blocks = [issue_block(1)];
        let new_blocks = [issue_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        // The whole block overlaps the issue, so without the proof it keeps
        // the normalization barrier.
        let whole = local_key(local_span(1, 0, 3), local_span(101, 0, 3));
        let child = local_key(local_span(1, 1, 3), local_span(101, 1, 3));
        let mut assessor = Assessor::new_with_evidence(
            [&old, &new],
            &alignment,
            None,
            DiffOptions::default(),
            None,
        )?;
        let before = assessor.domain_reasons(&whole)?;
        assert!(!before.1, "the whole pair starts with the barrier");
        assert_eq!(before.0, vec![AssessmentReason::NormalizationUncertainty]);
        // The exact whole pair is the only span the raw proof may lift.
        assessor.raw_source_equalities = vec![whole.local.clone().expect("local pair")];
        let proven = assessor.domain_reasons(&whole)?;
        assert!(proven.1, "the exact proven pair keeps its exception");
        assert_eq!(proven.0, Vec::<AssessmentReason>::new());
        // A contained child that still touches the issue keeps the barrier.
        let child_after = assessor.domain_reasons(&child)?;
        assert!(
            !child_after.1,
            "the proof must not leak to a contained child"
        );
        assert_eq!(
            child_after.0,
            vec![AssessmentReason::NormalizationUncertainty]
        );
        Ok(())
    }

    #[test]
    fn assessor_local_issue_cache_holds_on_short_budget() -> Result<()> {
        let old_blocks = [issue_block(1)];
        let new_blocks = [issue_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        let avoiding = local_key(local_span(1, 0, 1), local_span(101, 0, 1));
        let touching = local_key(local_span(1, 1, 3), local_span(101, 1, 3));

        // A cold validation miss that cannot pay the pre-validation charge
        // holds, and a repeat does not promote the key.
        let options = DiffOptions {
            max_assessment_work: 12,
            ..DiffOptions::default()
        };
        let mut miss = Assessor::new_with_evidence([&old, &new], &alignment, None, options, None)?;
        let first = miss.domain_reasons(&touching)?;
        let second = miss.domain_reasons(&touching)?;
        assert_eq!(first, second, "a repeat must not promote the key");
        assert!(
            !first.1,
            "an exhausted validation must not clear the barrier"
        );
        assert_eq!(first.0, vec![AssessmentReason::NormalizationUncertainty]);

        // A primed hit that cannot pay the overlap scan holds as well.
        let mut primed = Assessor::new_with_evidence(
            [&old, &new],
            &alignment,
            None,
            DiffOptions::default(),
            None,
        )?;
        let with_budget = primed.domain_reasons(&avoiding)?;
        assert!(with_budget.1, "the avoiding interval clears the barrier");
        assert_eq!(with_budget.0, Vec::<AssessmentReason>::new());
        primed.remaining_work = 1;
        let hit_short = primed.domain_reasons(&avoiding)?;
        assert!(!hit_short.1, "an unpayable hit keeps the barrier");
        assert_eq!(
            hit_short.0,
            vec![AssessmentReason::NormalizationUncertainty]
        );
        Ok(())
    }

    #[test]
    fn assessor_local_issue_cache_holds_when_reservation_fails() -> Result<()> {
        let old_blocks = [plain_block(1)];
        let new_blocks = [plain_block(101)];
        let old = super::super::SidePlan::inspect("old", &old_blocks)?.materialize()?;
        let new = super::super::SidePlan::inspect("new", &new_blocks)?.materialize()?;
        let alignment = issue_alignment();
        let touching = local_key(local_span(1, 1, 3), local_span(101, 1, 3));
        // The anchor verification leaves one work unit, so the per-side cache
        // reservation cannot be paid.
        let options = DiffOptions {
            max_assessment_work: 9,
            ..DiffOptions::default()
        };
        let mut assessor =
            Assessor::new_with_evidence([&old, &new], &alignment, None, options, None)?;
        let reasons = assessor.domain_reasons(&touching)?;
        assert!(!reasons.1, "an unpayable reservation must keep the barrier");
        assert_eq!(reasons.0, vec![AssessmentReason::NormalizationUncertainty]);
        assert!(
            assessor.issue_cache.is_none(),
            "the cache was never created"
        );
        Ok(())
    }
}

#[cfg(test)]
mod span_may_contain_tests {
    use super::{BlockId, ScalarRange, TextSpan, TokenRange, span_may_contain};

    fn span(blocks: &[u64]) -> TextSpan {
        TextSpan {
            blocks: blocks.iter().copied().map(BlockId).collect(),
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 10 },
            comparable_range: TokenRange { start: 0, end: 10 },
        }
    }

    #[test]
    fn structural_precondition_is_order_independent_and_contiguous() {
        // Block ids are not monotone; the check follows the stored span order
        // exactly like the canonical group built from the outer span.
        let outer = span(&[5, 2, 9]);
        assert!(span_may_contain(Some(&outer), Some(&span(&[2, 9]))));
        assert!(span_may_contain(Some(&outer), Some(&span(&[5]))));
        assert!(span_may_contain(Some(&outer), Some(&span(&[5, 2, 9]))));
        assert!(!span_may_contain(Some(&outer), Some(&span(&[5, 9]))));
        assert!(!span_may_contain(Some(&outer), Some(&span(&[9, 2]))));
        assert!(!span_may_contain(Some(&outer), Some(&span(&[3]))));
    }

    #[test]
    fn structural_precondition_handles_missing_and_empty_sides() {
        let outer = span(&[1]);
        assert!(span_may_contain(Some(&outer), None));
        assert!(span_may_contain(None, None));
        assert!(!span_may_contain(None, Some(&outer)));
        assert!(!span_may_contain(Some(&outer), Some(&span(&[]))));
    }
}
