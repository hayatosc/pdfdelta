//! Optional literal equality inside an independently established whole region.
//!
//! A multiset mismatch proves that a containing region changed, not that every
//! glyph changed. A complete mandatory pairing can independently resolve a
//! literal subrun without selecting the ambiguous edit positions. This pass
//! runs after existing proofs and preserves the containing coarse obligation.

use super::{
    AlignmentEvidence, Assessor, BlockId, BorrowedGroupView, COARSE_MEMORY_BYTES, ChangeCandidate,
    ComparableToken, ComparisonAssumption, DomainKey, DomainProof, Error, GroupMaterializationPlan,
    Ownership, ProofScope, ProvenChangedRegion, Range, RelationAssessment, RelationOutcome,
    ResolutionRange, ResolutionState, Result, ScalarRange, SearchCompleteness, Side,
    SourceInterval, TokenRange, UnresolvedRegion, borrowed_group_pair, charge_work, coarse_project,
    coarse_reviewed_domain_bytes, equal_fragment, light_group_metadata, plan_group_materialization,
    semantic,
};

/// Completed mandatory analyses for the four passes of one equality tail.
///
/// The immutable domain slice and both sides bind every slot. Only complete
/// positive analyses are moved here; a refused, exhausted or failed analysis
/// stays uncached. This stores matching evidence, never a source verdict or
/// ownership decision. All original guards still run before a lookup.
struct TailMandatoryAnalyses<'d, 's, 'b> {
    domains: &'d [(DomainKey, DomainProof)],
    sides: [&'s Side<'b>; 2],
    slots: Vec<Option<semantic::MandatoryMatchAnalysis>>,
    bytes: usize,
}

impl<'d, 's, 'b> TailMandatoryAnalyses<'d, 's, 'b> {
    const MAX_BYTES: usize = 16 * 1024 * 1024;

    /// Attempts a paid, fallible table only after a completed fresh analysis.
    /// Setup must be small relative to both that work and the remaining tail,
    /// so short analyses or a nearly exhausted tail keep the original path.
    fn new(
        domains: &'d [(DomainKey, DomainProof)],
        sides: [&'s Side<'b>; 2],
        completed_work: usize,
        available_bytes: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        let work = domains.len().checked_add(16)?;
        let limit = available_bytes.min(Self::MAX_BYTES);
        let bytes = domains
            .len()
            .checked_mul(std::mem::size_of::<Option<semantic::MandatoryMatchAnalysis>>())?
            .checked_add(std::mem::size_of::<Self>())?;
        if domains.is_empty()
            || bytes > limit
            || work >= completed_work / 4
            || work > *remaining / 64
            || !charge_work(remaining, work)
        {
            return None;
        }
        let mut slots = Vec::new();
        slots.try_reserve_exact(domains.len()).ok()?;
        let bytes = slots
            .capacity()
            .checked_mul(std::mem::size_of::<Option<semantic::MandatoryMatchAnalysis>>())?
            .checked_add(std::mem::size_of::<Self>())?;
        if bytes > limit {
            return None;
        }
        slots.resize_with(domains.len(), || None);
        Some(Self {
            domains,
            sides,
            slots,
            bytes,
        })
    }

    /// A paid lookup cannot substitute another side or a different domain.
    /// Outer `None` means exhaustion; an inner miss keeps the fresh analysis.
    fn get(
        &self,
        sides: [&Side<'_>; 2],
        index: usize,
        key: &DomainKey,
        remaining: &mut usize,
    ) -> Option<Option<&semantic::MandatoryMatchAnalysis>> {
        if !charge_work(remaining, 6) {
            return None;
        }
        if !(0..2).all(|side| std::ptr::eq(self.sides[side], sides[side]))
            || !self
                .domains
                .get(index)
                .is_some_and(|(stored, _)| std::ptr::eq(stored, key))
        {
            return Some(None);
        }
        Some(self.slots.get(index).and_then(Option::as_ref))
    }

    /// Moves an already completed array without cloning any pair. The ledger
    /// includes actual table and pair capacities, and refusal refunds no work.
    fn store(
        &mut self,
        index: usize,
        analysis: semantic::MandatoryMatchAnalysis,
        available_bytes: usize,
        remaining: &mut usize,
    ) -> bool {
        if *remaining < 24 {
            return false;
        }
        if !charge_work(remaining, 24) {
            return false;
        }
        let Some(bytes) = analysis
            .retained_heap_bytes()
            .and_then(|bytes| self.bytes.checked_add(bytes))
        else {
            return false;
        };
        if bytes > available_bytes.min(Self::MAX_BYTES) {
            return false;
        }
        let Some(slot) = self.slots.get_mut(index).filter(|slot| slot.is_none()) else {
            return false;
        };
        *slot = Some(analysis);
        self.bytes = bytes;
        true
    }
}

/// Checks the deliberately narrow whole-parent shape shared by emission and
/// source validation. Synthetic separators, partial blocks and one-sided
/// regions are outside this rule.
pub(super) fn whole_coarse_parent(
    sides: [&Side<'_>; 2],
    parent: &RelationAssessment,
    region: &ProvenChangedRegion,
) -> bool {
    if parent.outcome != RelationOutcome::Established
        || parent.search != SearchCompleteness::Complete
        || !parent.reasons.is_empty()
        || parent
            .assumptions
            .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
    {
        return false;
    }
    let spans = [parent.old_span.as_ref(), parent.new_span.as_ref()];
    let mut pages = [None, None];
    for side in 0..2 {
        let Some(span) = spans[side] else {
            return false;
        };
        let [block] = span.blocks.as_slice() else {
            return false;
        };
        let Some(&index) = sides[side].index.get(block) else {
            return false;
        };
        let source = &sides[side].blocks[index];
        if !source.raw.unmapped.is_empty()
            || !source.canonical.unmapped.is_empty()
            || span.separator.is_some()
            || span.comparable_range.start != 0
            || span.comparable_range.end != sides[side].canonical[index].len()
            || span.canonical_range.start != 0
            || span.canonical_range.end != span.comparable_range.end
        {
            return false;
        }
        let [page] = source.pages.as_slice() else {
            return false;
        };
        pages[side] = Some(*page);
    }
    pages[0] == pages[1]
        && region.proof == super::super::ChangedRegionProof::ExactTokenMultisetMismatch
        && parent.old_span == region.old_span
        && parent.new_span == region.new_span
}

/// The page-shift exception names an isolated whole single-block root on each
/// side, each confined to one page. A changed page ordinal does not override
/// source closure: generation additionally requires a paid exact internal key
/// and mandatory matching of the original full parent token sequences.
fn whole_coarse_page_shift_parent(
    sides: [&Side<'_>; 2],
    parent: &RelationAssessment,
    region: &ProvenChangedRegion,
) -> bool {
    if parent.parent.is_some()
        || !parent
            .assumptions
            .contains(&ComparisonAssumption::LocalEvidenceBoundaries)
        || parent
            .assumptions
            .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality)
        || parent.outcome != RelationOutcome::Established
        || parent.search != SearchCompleteness::Complete
        || !parent.reasons.is_empty()
        || parent
            .assumptions
            .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
    {
        return false;
    }
    let spans = [parent.old_span.as_ref(), parent.new_span.as_ref()];
    let mut pages = [None, None];
    for side in 0..2 {
        let Some(span) = spans[side] else {
            return false;
        };
        let [block] = span.blocks.as_slice() else {
            return false;
        };
        let Some(&index) = sides[side].index.get(block) else {
            return false;
        };
        let source = &sides[side].blocks[index];
        if !source.raw.unmapped.is_empty()
            || !source.canonical.unmapped.is_empty()
            || span.separator.is_some()
            || span.comparable_range.start != 0
            || span.comparable_range.end != sides[side].canonical[index].len()
            || span.canonical_range.start != 0
            || span.canonical_range.end != span.comparable_range.end
        {
            return false;
        }
        let [page] = source.pages.as_slice() else {
            return false;
        };
        pages[side] = Some(*page);
    }
    pages[0] != pages[1]
        && region.proof == super::super::ChangedRegionProof::ExactTokenMultisetMismatch
        && parent.old_span == region.old_span
        && parent.new_span == region.new_span
}

/// A necessary page premise of the unchanged whole-parent page-shift guard.
/// Use only the actual singleton key blocks; named/report spans are not a
/// substitute for these headers. Passing still requires every full guard.
pub(super) fn page_shift_key_pages_differ(sides: [&Side<'_>; 2], key: &DomainKey) -> bool {
    let Some([old]) = sides[0].blocks.get(key.old.clone()) else {
        return false;
    };
    let Some([new]) = sides[1].blocks.get(key.new.clone()) else {
        return false;
    };
    let ([old_page], [new_page]) = (old.pages.as_slice(), new.pages.as_slice()) else {
        return false;
    };
    old_page != new_page
}

/// A necessary header-only condition for the page-shift whole-parent proof.
/// Both named block slices must match before any full region can qualify.
/// Matching headers still require the unchanged full shape and source checks.
fn page_shift_coarse_pair_present(
    parent: &RelationAssessment,
    regions: &[ProvenChangedRegion],
) -> bool {
    let Some((old, new)) = parent.old_span.as_ref().zip(parent.new_span.as_ref()) else {
        return false;
    };
    let ([old_block], [new_block]) = (old.blocks.as_slice(), new.blocks.as_slice()) else {
        return false;
    };
    regions.iter().any(|region| {
        region
            .old_span
            .as_ref()
            .zip(region.new_span.as_ref())
            .is_some_and(|(old, new)| {
                old.blocks.as_slice() == [*old_block] && new.blocks.as_slice() == [*new_block]
            })
    })
}

/// A whole-domain key can still carry an independently isolated local proof.
/// Its marker and exact one-block spans are required; merely having no parent
/// does not establish local correspondence.
fn isolated_whole_key(sides: [&Side<'_>; 2], key: &DomainKey, parent: &RelationAssessment) -> bool {
    if parent.parent.is_some()
        || !parent
            .assumptions
            .contains(&ComparisonAssumption::LocalEvidenceBoundaries)
    {
        return false;
    }
    for (side, (range, span)) in [
        (&key.old, parent.old_span.as_ref()),
        (&key.new, parent.new_span.as_ref()),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(span) = span else { return false };
        let Some([block]) = sides[side].blocks.get(range.clone()) else {
            return false;
        };
        if span.blocks.as_slice() != [block.block]
            || span.separator.is_some()
            || span.comparable_range.start != 0
            || span.comparable_range.end != sides[side].canonical[range.start].len()
            || span.canonical_range.start != 0
            || span.canonical_range.end != span.comparable_range.end
        {
            return false;
        }
    }
    true
}

/// Metadata and literal condition for the validator's coarse exception.
/// Actual mandatory-path and raw-source checks are completed before emission;
/// this verifies that a recorded equality belongs to that exact whole parent.
pub(super) fn compatible_child(
    sides: [&Side<'_>; 2],
    relations: &[RelationAssessment],
    region: &ProvenChangedRegion,
    child_index: usize,
    clean_ancestors: &[bool],
) -> bool {
    let Some(child) = relations.get(child_index) else {
        return false;
    };
    let Some(parent_index) = child.parent.filter(|index| *index < child_index) else {
        return false;
    };
    let parent = &relations[parent_index];
    let page_shifted = child
        .assumptions
        .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality);
    if page_shifted
        && child
            .assumptions
            .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
    {
        return false;
    }
    if child.outcome != RelationOutcome::Established
        || child.search != SearchCompleteness::Complete
        || !child.reasons.is_empty()
        || !child
            .assumptions
            .contains(&ComparisonAssumption::MandatoryMatchingEquality)
        || !(if page_shifted {
            whole_coarse_page_shift_parent(sides, parent, region)
        } else {
            whole_coarse_parent(sides, parent, region)
        })
    {
        return false;
    }
    if clean_ancestors.get(parent_index) != Some(&true) {
        return false;
    }
    let mut selected = [None, None];
    for (side, span) in [child.old_span.as_ref(), child.new_span.as_ref()]
        .into_iter()
        .enumerate()
    {
        let Some(span) = span else { return false };
        let outer = if side == 0 {
            parent.old_span.as_ref()
        } else {
            parent.new_span.as_ref()
        };
        let Some(outer) = outer else { return false };
        if span.blocks != outer.blocks
            || span.separator.is_some()
            || span.comparable_range.start >= span.comparable_range.end
            || span.canonical_range.start != span.comparable_range.start
            || span.canonical_range.end != span.comparable_range.end
        {
            return false;
        }
        let index = sides[side].index[&span.blocks[0]];
        let Some(tokens) = sides[side].canonical[index]
            .get(span.comparable_range.start..span.comparable_range.end)
        else {
            return false;
        };
        if tokens
            .iter()
            .any(|token| !matches!(token, ComparableToken::Scalar(c) if !c.is_whitespace()))
        {
            return false;
        }
        selected[side] = Some(tokens);
    }
    selected[0] == selected[1]
}

/// Fallible partition snapshot for one newly proven, entirely unresolved run.
/// Other blocks and states are copied exactly; the selected block has no
/// unmapped tokens, so scalar and comparable bounds coincide.
fn equal_partition(
    source: &[ResolutionRange],
    block: BlockId,
    range: Range<usize>,
) -> Option<Vec<ResolutionRange>> {
    partition_split::<false>(source, block, range).map(|(items, _)| items)
}

/// Counts only the replacement of the unique unresolved containing interval.
/// The other entries retain their states, including zero-width scalar ranges.
/// A split leaves zero, one or two unresolved children, so the increase is
/// exactly `children.saturating_sub(1)`; no population scan is needed.
fn partition_split<const COUNT: bool>(
    source: &[ResolutionRange],
    block: BlockId,
    range: Range<usize>,
) -> Option<(Vec<ResolutionRange>, usize)> {
    let capacity = source.len().checked_add(2)?;
    let mut output = Vec::new();
    output.try_reserve_exact(capacity).ok()?;
    let mut found = false;
    let mut unresolved_children = 0usize;
    for part in source {
        if part.block == block
            && part.comparable_range.start <= range.start
            && range.end <= part.comparable_range.end
        {
            if found || part.state != ResolutionState::Unresolved {
                return None;
            }
            found = true;
            for (start, end, state) in [
                (
                    part.comparable_range.start,
                    range.start,
                    ResolutionState::Unresolved,
                ),
                (range.start, range.end, ResolutionState::Equal),
                (
                    range.end,
                    part.comparable_range.end,
                    ResolutionState::Unresolved,
                ),
            ] {
                if start < end {
                    if COUNT && state == ResolutionState::Unresolved {
                        unresolved_children += 1;
                    }
                    output.push(ResolutionRange {
                        block,
                        comparable_range: TokenRange { start, end },
                        canonical_range: ScalarRange { start, end },
                        state,
                    });
                }
            }
        } else {
            output.push(part.clone());
        }
    }
    found.then_some((output, unresolved_children.saturating_sub(1)))
}

/// Conservative requested-capacity allowance for the shared literal-source
/// cache and its temporary maps. Source entries are immutable and excluded;
/// 128 bytes per atom/map/event/issue item covers shallow glyph tables and
/// selected-run scratch; each block receives two items for its cache and
/// projection scratch. Canonical and raw singleton entries give four items
/// per potentially proven glyph. Allocator overhead is not a heap guarantee;
/// the enclosing workload remains under the independent 6 GB process limit.
pub(super) fn literal_source_capacity_bound(
    sides: [&Side<'_>; 2],
    remaining: &mut usize,
) -> Option<usize> {
    // Conservative extra per-block range flags and fixed header bookkeeping.
    if !charge_work(remaining, 8) {
        return None;
    }
    let header_bytes = sides[0]
        .blocks
        .len()
        .checked_add(sides[1].blocks.len())?
        .checked_mul(8)?
        .checked_add(8)?;
    let mut items = 0usize;
    for side in sides {
        if !charge_work(remaining, side.blocks.len()) {
            return None;
        }
        items = items.checked_add(side.blocks.len().checked_mul(2)?)?;
        for (index, block) in side.blocks.iter().enumerate() {
            // Selected-length vectors are allocated before a missing or
            // multi-scalar map is held. Count uncovered token capacity even
            // when map headers alone give no bound on those scratch arrays.
            items = items.checked_add(
                side.canonical[index]
                    .len()
                    .saturating_sub(block.canonical.source_map.len()),
            )?;
            for mapped in [&block.raw, &block.canonical] {
                if !charge_work(
                    remaining,
                    mapped.source_map.len().checked_add(mapped.unmapped.len())?,
                ) {
                    return None;
                }
                for source in mapped
                    .source_map
                    .iter()
                    .map(|entry| &entry.source)
                    .chain(mapped.unmapped.iter().map(|entry| &entry.source))
                {
                    items = items.checked_add(source.atoms.len().checked_add(1)?)?;
                }
            }
            if !charge_work(
                remaining,
                block
                    .normalization_events
                    .len()
                    .checked_add(block.issues.len())?,
            ) {
                return None;
            }
            for source in block
                .normalization_events
                .iter()
                .map(|event| &event.source)
                .chain(block.issues.iter().map(|issue| &issue.source))
            {
                items = items.checked_add(source.atoms.len().checked_add(1)?)?;
            }
        }
    }
    items.checked_mul(128)?.checked_add(header_bytes)
}

/// A completed requested-capacity calculation bound to immutable source sides.
/// This stores a numeric allocation bound, never physical matching evidence.
pub(super) struct LiteralCapacity<'a, 'd> {
    sides: [&'a Side<'d>; 2],
    bytes: usize,
}

impl Assessor<'_, '_> {
    /// Reuses only a fully paid scan of these exact immutable sides. Cache
    /// storage remains part of the source allowance on both misses and hits.
    pub(super) fn cached_literal_source_capacity_bound(&mut self) -> Option<usize> {
        if !charge_work(&mut self.remaining_work, 8) {
            return None;
        }
        if let Some(cached) = &self.literal_capacity
            && (0..2).all(|side| std::ptr::eq(cached.sides[side], self.sides[side]))
        {
            return Some(cached.bytes);
        }
        let bytes = literal_source_capacity_bound(self.sides, &mut self.remaining_work)?
            .checked_add(std::mem::size_of_val(&self.literal_capacity))?;
        if !charge_work(&mut self.remaining_work, 6) {
            return None;
        }
        self.literal_capacity = Some(LiteralCapacity {
            sides: self.sides,
            bytes,
        });
        Some(bytes)
    }
}

fn shallow_words<T>() -> usize {
    std::mem::size_of::<T>().div_ceil(std::mem::size_of::<usize>())
}

/// Exact requested capacities for guarded optional child publication.
/// These are allocation requests, not an allocator-overhead or CPU guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PublicationGrowth {
    targets: [usize; 3],
    minimum: [usize; 3],
    copy_work: usize,
    extra_bytes: usize,
}

/// Plans checked geometric growth, falling back to the existing minimum
/// requests when transient old-plus-new backing arrays would exceed the bound.
/// Existing spare capacity is retained and carries no old-array move charge.
fn publication_growth(
    lengths: [usize; 3],
    capacities: [usize; 3],
    output_limit: usize,
    minimum_bytes: usize,
    memory_limit: usize,
) -> Option<PublicationGrowth> {
    if minimum_bytes > memory_limit {
        return None;
    }
    let sizes = [
        std::mem::size_of::<RelationAssessment>(),
        std::mem::size_of::<SourceInterval>(),
        std::mem::size_of::<SourceInterval>(),
    ];
    let words = [
        shallow_words::<RelationAssessment>(),
        shallow_words::<SourceInterval>(),
        shallow_words::<SourceInterval>(),
    ];
    let mut targets = capacities;
    let mut minimum = capacities;
    let mut copy_work = 0usize;
    let mut extra_bytes = 0usize;
    for index in 0..3 {
        let required = lengths[index].checked_add(1)?;
        if lengths[index] > capacities[index] || required > output_limit {
            return None;
        }
        if required > capacities[index] {
            minimum[index] = required;
            targets[index] = capacities[index]
                .checked_mul(2)?
                .max(required)
                .min(output_limit);
            extra_bytes = extra_bytes.checked_add(
                targets[index]
                    .checked_sub(required)?
                    .checked_mul(sizes[index])?,
            )?;
            copy_work = copy_work.checked_add(lengths[index].checked_mul(words[index])?)?;
        }
    }
    if minimum_bytes
        .checked_add(extra_bytes)
        .is_none_or(|bytes| bytes > memory_limit)
    {
        targets = minimum;
        extra_bytes = 0;
    }
    Some(PublicationGrowth {
        targets,
        minimum,
        copy_work,
        extra_bytes,
    })
}

/// Counts both backing arrays when a fallible growth may move a vector.
/// Allocator rounding is excluded; requested capacities and transient old
/// arrays are included rather than assuming in-place reallocation.
fn growth_capacity(current: usize, required: usize) -> Option<usize> {
    if required > current {
        current.checked_add(required)
    } else {
        Some(current)
    }
}

fn publication_memory(
    domain_bytes: usize,
    source_bytes: usize,
    retained_children: usize,
    records: &[RelationAssessment],
    record_capacity: usize,
    ownership: &[Ownership; 2],
    partitions: [&Vec<ResolutionRange>; 2],
) -> Option<usize> {
    let mut bytes = domain_bytes
        .checked_add(source_bytes)?
        .checked_add(retained_children)?;
    bytes = bytes.checked_add(
        growth_capacity(record_capacity, records.len().checked_add(1)?)?
            .checked_mul(std::mem::size_of::<RelationAssessment>())?,
    )?;
    for side in 0..2 {
        bytes = bytes.checked_add(
            partitions[side]
                .capacity()
                .checked_add(partitions[side].len().checked_add(2)?)?
                .checked_mul(std::mem::size_of::<ResolutionRange>())?,
        )?;
        bytes = bytes.checked_add(
            growth_capacity(
                ownership[side].accepted.capacity(),
                ownership[side].accepted.len().checked_add(1)?,
            )?
            .checked_mul(std::mem::size_of::<SourceInterval>())?,
        )?;
    }
    Some(bytes)
}

/// Existing output obligations borrowed by the optional tail. No payload is
/// cloned to prepare the final unresolved diagnostics.
#[derive(Clone, Copy)]
pub(super) struct EqualConstraints<'a> {
    pub candidates: &'a [ChangeCandidate],
    pub regions: &'a [ProvenChangedRegion],
    pub original: &'a [UnresolvedRegion],
}

struct DiagnosticAllowance {
    upper_count: usize,
    #[cfg(test)]
    copy_count: usize,
    per_entry_bytes: usize,
    projected_intervals: usize,
    per_split_work: usize,
    evidence_cache_bytes: usize,
}

impl DiagnosticAllowance {
    fn bytes(&self) -> Option<usize> {
        self.upper_count
            .checked_mul(self.per_entry_bytes)?
            .checked_add(self.evidence_cache_bytes)
    }
}

struct TailResources {
    source_bytes: usize,
    retained_children: usize,
    // Earlier accepted parents remain in the final diagnostic pass. Retain
    // their largest additional group peak while later output payload grows.
    group_peak_bytes: usize,
    diagnostics: DiagnosticAllowance,
}

/// Completed necessary block pairs, never proof or ownership decisions.
/// A missing pair cannot satisfy the original whole single-block predicate.
struct CoarsePairFilter<'a> {
    regions: &'a [ProvenChangedRegion],
    pairs: Vec<[BlockId; 2]>,
    bytes: usize,
}

impl<'a> CoarsePairFilter<'a> {
    fn build(
        regions: &'a [ProvenChangedRegion],
        available_bytes: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        let n = regions.len();
        if n == 0 || n > 64 {
            return None;
        }
        let bytes = n
            .checked_mul(std::mem::size_of::<[BlockId; 2]>())?
            .checked_add(std::mem::size_of::<Self>())?;
        // At most 64 fixed-size pairs: the quadratic comparison allowance
        // covers sorting, plus construction, destruction and header work.
        let work = n
            .checked_mul(n)?
            .checked_mul(3)?
            .checked_add(n.checked_mul(10)?)?
            .checked_add(32)?;
        if bytes > available_bytes || work > *remaining / 8 || !charge_work(remaining, work) {
            return None;
        }
        let mut pairs = Vec::new();
        pairs.try_reserve_exact(n).ok()?;
        if pairs.capacity() != n {
            return None;
        }
        for region in regions {
            if region.proof != super::super::ChangedRegionProof::ExactTokenMultisetMismatch {
                continue;
            }
            let (Some(old), Some(new)) = (&region.old_span, &region.new_span) else {
                continue;
            };
            if let ([old], [new]) = (old.blocks.as_slice(), new.blocks.as_slice()) {
                pairs.push([*old, *new]);
            }
        }
        pairs.sort_unstable();
        Some(Self {
            regions,
            pairs,
            bytes,
        })
    }

    fn may_match(
        &self,
        regions: &[ProvenChangedRegion],
        parent: &RelationAssessment,
        remaining: &mut usize,
    ) -> Option<bool> {
        let comparisons = self
            .pairs
            .len()
            .checked_ilog2()
            .map_or(0, |n| n as usize + 2);
        if !charge_work(remaining, comparisons.checked_mul(3)?.checked_add(8)?)
            || !std::ptr::eq(self.regions, regions)
        {
            return None;
        }
        let (Some(old), Some(new)) = (&parent.old_span, &parent.new_span) else {
            return Some(false);
        };
        let ([old], [new]) = (old.blocks.as_slice(), new.blocks.as_slice()) else {
            return Some(false);
        };
        Some(self.pairs.binary_search(&[*old, *new]).is_ok())
    }
}

struct EqualRun<'a, 'source> {
    page_shifted: bool,
    parent: usize,
    region: usize,
    views: &'a [BorrowedGroupView<'source>; 2],
    ranges: [Range<usize>; 2],
    domain_bytes: usize,
    groups: &'a [GroupMaterializationPlan; 2],
}

/// The original coverage predicate, paying each range actually visited.
/// A missing or exhausted search holds; a hit needs no unvisited suffix.
fn unresolved_cover(
    partitions: &[ResolutionRange],
    block: BlockId,
    selected: &SourceInterval,
    remaining: &mut usize,
) -> Option<bool> {
    for part in partitions {
        if !charge_work(remaining, 1) {
            return None;
        }
        if part.block == block
            && part.state == ResolutionState::Unresolved
            && part.comparable_range.start <= selected.start
            && selected.end <= part.comparable_range.end
        {
            return Some(true);
        }
    }
    Some(false)
}

/// Searches for an actual coverage witness; ordering only guides the probe.
/// A miss or unknown header falls back to the original linear predicate, so
/// unordered, overlapping or incomplete diagnostic partitions retain it too.
pub(super) fn unresolved_cover_probe(
    side: &Side<'_>,
    partitions: &[ResolutionRange],
    block: BlockId,
    selected: &SourceInterval,
    remaining: &mut usize,
) -> Option<bool> {
    if partitions.len() >= 32 && *remaining >= 32 {
        if !charge_work(remaining, 2) {
            return None;
        }
        if let Some(&target) = side.index.get(&block) {
            let (mut low, mut high) = (0, partitions.len());
            while low < high {
                if !charge_work(remaining, 3) {
                    return None;
                }
                let middle = low + (high - low) / 2;
                let part = &partitions[middle];
                if part.block == block
                    && part.state == ResolutionState::Unresolved
                    && part.comparable_range.start <= selected.start
                    && selected.end <= part.comparable_range.end
                {
                    return Some(true);
                }
                let Some(&index) = side.index.get(&part.block) else {
                    break;
                };
                if index < target || (index == target && part.comparable_range.end < selected.end) {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
        }
    }
    unresolved_cover(partitions, block, selected, remaining)
}

/// Exactly the original overlap/invalid-projection veto. A literal singleton
/// needs no duplicate-block set, vector allocation or offset walk. Every
/// coordinate is validated even for an unrelated block; invalid context holds.
pub(super) fn ownership_projection_clear(
    side: &Side<'_>,
    span: &super::TextSpan,
    selected: &SourceInterval,
    remaining: &mut usize,
) -> Option<bool> {
    if !charge_work(remaining, 4) {
        return None;
    }
    if let [block] = span.blocks.as_slice()
        && span.separator.is_none()
        && let Some(&index) = side.index.get(block)
        && side.blocks[index].canonical.unmapped.is_empty()
    {
        // Twelve units cover the range/scalar checks and overlap comparisons
        // after the four-unit selector. No projected buffer is constructed.
        if !charge_work(remaining, 12) {
            return None;
        }
        let range = span.comparable_range;
        if range.start > range.end
            || range.end > side.canonical[index].len()
            || span.canonical_range.start != range.start
            || span.canonical_range.end != range.end
        {
            return None;
        }
        return Some(
            range.start == range.end
                || index != selected.block_index
                || selected.end <= range.start
                || range.end <= selected.start,
        );
    }
    // A tiny literal block list needs only a bounded stack of source indices.
    // This is an implementation choice, not a change to source coordinates.
    if (2..=8).contains(&span.blocks.len())
        && let Some(separator) = span.separator
    {
        if !separator.valid_for(span.blocks.len()) {
            return None;
        }
        // Pay selection of the stack width and the helper's capacity premise.
        if !charge_work(remaining, 2) {
            return None;
        }
        let projection = match span.blocks.len() {
            2 => tiny_literal_projection::<2>(side, span, selected, remaining),
            3..=4 => tiny_literal_projection::<4>(side, span, selected, remaining),
            _ => tiny_literal_projection::<8>(side, span, selected, remaining),
        }?;
        if let LiteralProjection::Clear(clear) = projection {
            return Some(clear);
        }
    }
    // Source order, uniqueness and coordinates are checked in the same paid
    // walk. No result is returned until every block has passed. A nonliteral
    // or unordered list falls back to the unchanged general projection.
    if span.blocks.len() > 8
        && span
            .separator
            .is_some_and(|s| s.valid_for(span.blocks.len()))
    {
        match literal_overlap_stream(
            side,
            span,
            selected,
            span.blocks
                .iter()
                .map(|block| ProjectionBlock::Named(*block)),
            true,
            20,
            remaining,
        )? {
            LiteralProjection::Clear(clear) => return Some(clear),
            LiteralProjection::General => {}
        }
    }

    if !charge_work(
        remaining,
        span.blocks.len().saturating_mul(usize::BITS as usize + 8),
    ) {
        return None;
    }
    let other = coarse_project(side, span)?;
    Some(!other.iter().any(|other| {
        selected.block_index == other.block_index
            && selected.start < other.end
            && other.start < selected.end
    }))
}

fn tiny_literal_projection<const N: usize>(
    side: &Side<'_>,
    span: &super::TextSpan,
    selected: &SourceInterval,
    remaining: &mut usize,
) -> Option<LiteralProjection> {
    if span.blocks.len() > N {
        return Some(LiteralProjection::General);
    }
    // Every initialized stack slot is paid; shorter lists use smaller arrays.
    if !charge_work(remaining, N) {
        return None;
    }
    let mut indices = [0usize; N];
    for (position, block) in span.blocks.iter().enumerate() {
        if !charge_work(remaining, position + 3) {
            return None;
        }
        let &index = side.index.get(block)?;
        if indices[..position].contains(&index) {
            return None;
        }
        indices[position] = index;
        if !side.blocks[index].canonical.unmapped.is_empty() {
            return Some(LiteralProjection::General);
        }
    }
    if N == 2 {
        // Pay both source-index comparisons, including the selected-block
        // case that still takes the original full interval projection.
        if !charge_work(remaining, 2) {
            return None;
        }
        if indices[0] != selected.block_index && indices[1] != selected.block_index {
            // Pay the complete header validation before copying its index pair.
            if !charge_work(remaining, 20) {
                return None;
            }
            return unrelated_literal_pair(side, span, [indices[0], indices[1]]);
        }
    }
    literal_overlap_stream(
        side,
        span,
        selected,
        indices[..span.blocks.len()]
            .iter()
            .copied()
            .map(ProjectionBlock::Known),
        false,
        16,
        remaining,
    )
}

/// Validates the full joined coordinate system even when neither literal
/// source block can overlap the selected block. No clipped intervals are
/// materialized; every source, duplicate and unmapped premise is already paid
/// by the bounded header walk. Range checks remain specific to this span.
fn unrelated_literal_pair(
    side: &Side<'_>,
    span: &super::TextSpan,
    indices: [usize; 2],
) -> Option<LiteralProjection> {
    // The caller prepays twenty units for the index-pair copy, endpoint
    // agreement, both boundary headers, Unicode/projected-space agreement,
    // checked joined length and range bounds before entering this function.
    let range = span.comparable_range;
    if range.start > range.end
        || span.canonical_range.start != range.start
        || span.canonical_range.end != range.end
    {
        return None;
    }
    let old = &side.canonical[indices[0]];
    let new = &side.canonical[indices[1]];
    let mut join = false;
    if span.separator?.at(0) == super::BlockSeparator::Space {
        let canonical = !old.last().is_some_and(super::super::is_space_token)
            && !new.first().is_some_and(super::super::is_space_token);
        let projected = !old.last().is_some_and(super::space_token)
            && !new.first().is_some_and(super::space_token);
        if canonical != projected {
            return None;
        }
        join = canonical;
    }
    let length = old
        .len()
        .checked_add(new.len())?
        .checked_add(usize::from(join))?;
    if range.end > length {
        return None;
    }
    Some(LiteralProjection::Clear(true))
}

enum ProjectionBlock {
    Known(usize),
    Named(BlockId),
}

enum LiteralProjection {
    Clear(bool),
    General,
}

fn literal_overlap_stream(
    side: &Side<'_>,
    span: &super::TextSpan,
    selected: &SourceInterval,
    indices: impl Iterator<Item = ProjectionBlock>,
    ordered_literal: bool,
    per_block_work: usize,
    remaining: &mut usize,
) -> Option<LiteralProjection> {
    let range = span.comparable_range;
    if range.start > range.end
        || span.canonical_range.start != range.start
        || span.canonical_range.end != range.end
    {
        return None;
    }
    let (mut offset, mut clear) = (0usize, true);
    let mut previous = None;
    let mut previous_index = None;
    for (position, index) in indices.enumerate() {
        if !charge_work(remaining, per_block_work) {
            return None;
        }
        let index = match index {
            ProjectionBlock::Known(index) => index,
            ProjectionBlock::Named(block) => *side.index.get(&block)?,
        };
        if ordered_literal {
            if previous_index.is_some_and(|p| p >= index)
                || !side.blocks[index].canonical.unmapped.is_empty()
            {
                return Some(LiteralProjection::General);
            }
            previous_index = Some(index);
        }
        let tokens = &side.canonical[index];
        let mut join = false;
        if position > 0 && span.separator?.at(position - 1) == super::BlockSeparator::Space {
            let canonical = !previous.is_some_and(super::super::is_space_token)
                && !tokens.first().is_some_and(super::super::is_space_token);
            let projected = !previous.is_some_and(super::space_token)
                && !tokens.first().is_some_and(super::space_token);
            if canonical != projected {
                return None;
            }
            join = canonical;
        }
        if join {
            offset = offset.checked_add(1)?;
            previous = Some(&super::COARSE_SPACE);
        }
        let end = offset.checked_add(tokens.len())?;
        let start = range.start.max(offset);
        let stop = range.end.min(end);
        if index == selected.block_index && start < stop {
            clear &= selected.end <= start - offset || stop - offset <= selected.start;
        }
        offset = end;
        if let Some(last) = tokens.last() {
            previous = Some(last);
        }
    }
    if range.end > offset {
        return None;
    }
    Some(LiteralProjection::Clear(clear))
}

/// Bounds possible final diagnostic output without generating it twice.
/// A retained projected interval can intersect only one unresolved partition,
/// so original/aligned regions + unresolved partitions + projected intervals
/// bounds every possible fallback emit, including invalidated paired regions.
/// Existing alignment/full-relation construction stays the baseline path;
/// this prepays at most one immutable evidence scan per unresolved partition,
/// a paid cache lookup and evidence clone per possible output, and the single
/// retained evidence buffer. A block revisit after a miss may scan again.
fn diagnostic_allowance(
    assessor: &mut Assessor<'_, '_>,
    partitions: [&[ResolutionRange]; 2],
    original: &[UnresolvedRegion],
    available_bytes: usize,
) -> Option<DiagnosticAllowance> {
    if let Some(indexed) =
        indexed_diagnostic_allowance(assessor, partitions, original, available_bytes)
    {
        return Some(indexed);
    }
    let mut count = original.len().checked_add(assessor.alignment.spans.len())?;
    let mut projected = 0usize;
    let mut evidence_work = 0usize;
    let mut evidence = Vec::new();
    let metadata = original
        .len()
        .checked_add(assessor.alignment.spans.len())?
        .checked_add(partitions[0].len())?
        .checked_add(partitions[1].len())?;
    if !charge_work(&mut assessor.remaining_work, metadata) {
        return None;
    }
    for region in original {
        for span in [&region.old_span, &region.new_span].into_iter().flatten() {
            projected = projected.checked_add(span.blocks.len())?;
        }
    }
    let mut per_block = [Vec::new(), Vec::new()];
    let block_count = assessor.sides[0]
        .blocks
        .len()
        .checked_add(assessor.sides[1].blocks.len())?;
    if block_count.checked_mul(std::mem::size_of::<usize>())? > COARSE_MEMORY_BYTES
        || !charge_work(&mut assessor.remaining_work, block_count.checked_mul(2)?)
    {
        return None;
    }
    for (side, table) in per_block.iter_mut().enumerate() {
        table
            .try_reserve_exact(assessor.sides[side].blocks.len())
            .ok()?;
        table.resize(assessor.sides[side].blocks.len(), 0usize);
    }
    for aligned in &assessor.alignment.spans {
        projected = projected
            .checked_add(aligned.old.len())?
            .checked_add(aligned.new.len())?;
        evidence_work = evidence_work
            .checked_add(aligned.old.len())?
            .checked_add(aligned.new.len())?
            .checked_add(1)?;
        if !charge_work(
            &mut assessor.remaining_work,
            aligned.evidence.len().checked_mul(usize::BITS as usize)?,
        ) {
            return None;
        }
        let ids = aligned.old.len().checked_add(aligned.new.len())?;
        if !charge_work(
            &mut assessor.remaining_work,
            ids.checked_mul(usize::BITS as usize)?,
        ) {
            return None;
        }
        for (side, blocks) in [&aligned.old, &aligned.new].into_iter().enumerate() {
            for block in blocks {
                let index = *assessor.sides[side].index.get(block)?;
                per_block[side][index] =
                    per_block[side][index].checked_add(aligned.evidence.len())?;
            }
        }
        for item in &aligned.evidence {
            if !evidence.contains(item) {
                // The prepaid dedup walk uses this explicit finite allowance.
                if evidence.len() >= usize::BITS as usize {
                    return None;
                }
                evidence.try_reserve_exact(1).ok()?;
                evidence.push(*item);
            }
        }
    }
    let matched_reasons = per_block
        .iter()
        .flat_map(|table| table.iter())
        .copied()
        .max()
        .unwrap_or(0);
    evidence_work =
        evidence_work.checked_add(matched_reasons.checked_mul(evidence.len().checked_add(1)?)?)?;
    let mut unresolved = 0usize;
    for parts in partitions {
        unresolved = unresolved.checked_add(
            parts
                .iter()
                .filter(|part| part.state == ResolutionState::Unresolved)
                .count(),
        )?;
    }
    count = count.checked_add(unresolved)?;
    count = count.checked_add(projected)?;
    if count > assessor.options.max_assessment_ranges {
        return None;
    }
    // Three backing arrays cover growth, and new one-block spans plus evidence
    // cover fallback payloads. Original immutable nested payloads are excluded.
    let unit = 3usize
        .checked_mul(std::mem::size_of::<UnresolvedRegion>())?
        .checked_add(3 * std::mem::size_of::<BlockId>())?
        .checked_add(3 * std::mem::size_of::<SourceInterval>())?
        .checked_add(
            evidence
                .len()
                .checked_mul(4 * std::mem::size_of::<AlignmentEvidence>())?,
        )?;
    // Only one side's cache is live at a time. Clearing a missed block retains
    // capacity, bounded by the global reason set; eight elements per reason
    // cover initial allocation and overlapping buffers during growth. Include
    // the cache headers and this allowance's additional bookkeeping field.
    let evidence_cache_bytes = evidence
        .len()
        .checked_mul(8 * std::mem::size_of::<AlignmentEvidence>())?
        .checked_add(std::mem::size_of::<Vec<AlignmentEvidence>>())?
        .checked_add(std::mem::size_of::<Option<BlockId>>())?
        .checked_add(std::mem::size_of::<usize>())?;
    let bytes = count.checked_mul(unit)?.checked_add(evidence_cache_bytes)?;
    if bytes > COARSE_MEMORY_BYTES {
        return None;
    }
    // Every output owns its reason copy, including the first cache miss.
    // Three units pay the key comparison, clear and new key assignment. The
    // region's shallow allowance includes the cloned vector's header.
    let output_work = evidence
        .len()
        .checked_mul(shallow_words::<AlignmentEvidence>())?
        .checked_add(3)?
        .checked_add(shallow_words::<UnresolvedRegion>())?;
    let per_split_work = evidence_work.checked_add(output_work)?;
    let work = unresolved
        .checked_mul(evidence_work)?
        .checked_add(count.checked_mul(output_work)?)?
        .checked_add(shallow_words::<Vec<AlignmentEvidence>>())?
        .checked_add(shallow_words::<Option<BlockId>>())?;
    if !charge_work(&mut assessor.remaining_work, work) {
        return None;
    }
    Some(DiagnosticAllowance {
        upper_count: count,
        #[cfg(test)]
        copy_count: count,
        per_entry_bytes: unit,
        projected_intervals: projected,
        per_split_work,
        evidence_cache_bytes,
    })
}

/// Prepays the same possible outputs using completed immutable reason rows.
/// The retained index is consumed by the actual final diagnostic emitter;
/// neither output payload, output limits nor physical ownership is relaxed.
fn indexed_diagnostic_allowance(
    assessor: &mut Assessor<'_, '_>,
    partitions: [&[ResolutionRange]; 2],
    original: &[UnresolvedRegion],
    available_bytes: usize,
) -> Option<DiagnosticAllowance> {
    let metadata = original
        .len()
        .checked_mul(6)?
        .checked_add(assessor.alignment.spans.len().checked_mul(8)?)?
        .checked_add(partitions[0].len())?
        .checked_add(partitions[1].len())?;
    if !charge_work(&mut assessor.remaining_work, metadata) {
        return None;
    }
    let mut projected = 0usize;
    let mut raw_copy = 0usize;
    for region in original {
        raw_copy = raw_copy.max(region.evidence.len());
        for span in [&region.old_span, &region.new_span].into_iter().flatten() {
            projected = projected.checked_add(span.blocks.len())?;
        }
    }
    // A whole aligned side touching current confirmed ownership can never
    // contribute retained intervals after an equality-only tail. If its span
    // copy later fails, that side contributes no intervals either. Keep the
    // other side and every possible region in the output bound unchanged.
    let mut confirmed = [Vec::new(), Vec::new()];
    let scratch_bytes = assessor.sides[0]
        .blocks
        .len()
        .checked_add(assessor.sides[1].blocks.len())?
        .checked_add(2 * std::mem::size_of::<Vec<bool>>())?;
    if scratch_bytes > available_bytes {
        return None;
    }
    for (side, flags) in confirmed.iter_mut().enumerate() {
        let n = assessor.sides[side].blocks.len();
        if !charge_work(
            &mut assessor.remaining_work,
            n.checked_mul(2)?
                .checked_add(partitions[side].len().checked_mul(8)?)?,
        ) {
            return None;
        }
        flags.try_reserve_exact(n).ok()?;
        if flags.capacity() != n {
            return None;
        }
        flags.resize(n, false);
        for range in partitions[side] {
            let &block = assessor.sides[side].index.get(&range.block)?;
            if range.state != ResolutionState::Unresolved
                && range.comparable_range.start < range.comparable_range.end
                && range.comparable_range.end <= assessor.sides[side].canonical[block].len()
            {
                flags[block] = true;
            }
        }
    }
    let mut original_outputs = original.len();
    let mut partial_intervals = 0usize;
    for region in original {
        let mut blocked = false;
        let mut partial = 0usize;
        for (side, span) in [region.old_span.as_ref(), region.new_span.as_ref()]
            .into_iter()
            .enumerate()
        {
            let Some(span) = span else {
                continue;
            };
            if !charge_work(&mut assessor.remaining_work, 14) {
                return None;
            }
            // A contiguous span has at most two partial endpoint blocks.
            // Whole-block intervals can only remove a fallback range, never
            // split it. Their storage and range limits stay fully reserved.
            partial = partial.checked_add(span.blocks.len().min(2))?;
            if span.blocks.len() != 1 {
                continue;
            }
            let &block = assessor.sides[side].index.get(&span.blocks[0])?;
            if span.comparable_range.start == 0
                && span.comparable_range.end == assessor.sides[side].canonical[block].len()
                && span.comparable_range.end > 0
            {
                partial = partial.checked_sub(1)?;
                blocked |= confirmed[side][block];
            }
        }
        if blocked {
            original_outputs = original_outputs.checked_sub(1)?;
            for span in [&region.old_span, &region.new_span].into_iter().flatten() {
                projected = projected.checked_sub(span.blocks.len())?;
            }
        } else {
            partial_intervals = partial_intervals.checked_add(partial)?;
        }
    }
    let mut scan = 0usize;
    let mut aligned_outputs = 0usize;
    for aligned in &assessor.alignment.spans {
        raw_copy = raw_copy.max(aligned.evidence.len());
        let mut blocked_sides = [false; 2];
        let ids = aligned.old.len().checked_add(aligned.new.len())?;
        for (side, ids) in [&aligned.old, &aligned.new].into_iter().enumerate() {
            if !charge_work(
                &mut assessor.remaining_work,
                ids.len().checked_mul(3)?.checked_add(1)?,
            ) {
                return None;
            }
            let mut blocked = false;
            for id in ids {
                let &block = assessor.sides[side].index.get(id)?;
                blocked |= confirmed[side][block];
            }
            blocked_sides[side] = blocked;
            if !blocked {
                projected = projected.checked_add(ids.len())?;
            }
        }
        if !charge_work(
            &mut assessor.remaining_work,
            aligned.evidence.len().checked_mul(2)?.checked_add(4)?,
        ) {
            return None;
        }
        // With both full sides blocked, only a failed pair of span copies
        // could avoid their confirmed tokens. Without an empty-span issue
        // marker that representation cannot survive the original emitter.
        if !blocked_sides.iter().all(|blocked| *blocked)
            || aligned.evidence.contains(&AlignmentEvidence::ExtractionGap)
            || aligned
                .evidence
                .contains(&AlignmentEvidence::NormalizationIssue)
        {
            aligned_outputs = aligned_outputs.checked_add(1)?;
        }
        scan = scan.checked_add(ids.checked_add(1)?)?;
    }
    let unresolved = partitions
        .iter()
        .flat_map(|p| p.iter())
        .filter(|p| p.state == ResolutionState::Unresolved)
        .count();
    let count = original_outputs
        .checked_add(aligned_outputs)?
        .checked_add(unresolved)?
        .checked_add(projected)?;
    let blocks = assessor.sides[0]
        .blocks
        .len()
        .checked_add(assessor.sides[1].blocks.len())?;
    if count > assessor.options.max_assessment_ranges
        || scan.checked_mul(unresolved)? <= blocks.checked_add(scan)?.checked_mul(4)?
    {
        return None;
    }
    let mut fresh = None;
    let index = if let Some(index) = &assessor.diagnostic_evidence {
        if !index.is_bound(assessor.alignment, assessor.sides) {
            return None;
        }
        index
    } else {
        fresh = Some(super::evidence_index::EvidenceIndex::build(
            assessor.alignment,
            assessor.sides,
            available_bytes.checked_sub(scratch_bytes)?,
            &mut assessor.remaining_work,
        )?);
        fresh.as_ref()?
    };
    let reasons = index.reason_count();
    let unit = 3usize
        .checked_mul(std::mem::size_of::<UnresolvedRegion>())?
        .checked_add(3 * std::mem::size_of::<BlockId>())?
        .checked_add(3 * std::mem::size_of::<SourceInterval>())?
        .checked_add(reasons.checked_mul(4 * std::mem::size_of::<AlignmentEvidence>())?)?;
    let cache_bytes = reasons
        .checked_mul(8 * std::mem::size_of::<AlignmentEvidence>())?
        .checked_add(std::mem::size_of::<Vec<AlignmentEvidence>>())?
        .checked_add(std::mem::size_of::<Option<BlockId>>())?
        .checked_add(std::mem::size_of::<usize>())?
        .checked_add(index.bytes())?;
    if count
        .checked_mul(unit)?
        .checked_add(cache_bytes)?
        .checked_add(scratch_bytes)?
        > available_bytes
    {
        return None;
    }
    let output_work = index
        .maximum_copy_len()
        .max(raw_copy)
        .checked_mul(shallow_words::<AlignmentEvidence>())?
        .checked_add(3)?
        .checked_add(shallow_words::<UnresolvedRegion>())?;
    // Twelve units cover immutable binding checks and the source-block lookup.
    // Aligned spans cover whole source blocks. Only original partial
    // endpoints can split a fallback interval; full projected intervals
    // remain in the independent storage/range ledger above.
    // Every aligned reason vector is copied before the emitter filters
    // ownership. Pay all of those copies, including discarded regions.
    let copy_count = original_outputs
        .checked_add(assessor.alignment.spans.len())?
        .checked_add(unresolved)?
        .checked_add(partial_intervals)?;
    let work = unresolved
        .checked_mul(12)?
        .checked_add(copy_count.checked_mul(output_work)?)?
        .checked_add(shallow_words::<Vec<AlignmentEvidence>>())?
        .checked_add(shallow_words::<Option<BlockId>>())?;
    if !charge_work(&mut assessor.remaining_work, work) {
        return None;
    }
    if let Some(index) = fresh {
        assessor.diagnostic_evidence = Some(index);
    }
    Some(DiagnosticAllowance {
        upper_count: count,
        #[cfg(test)]
        copy_count,
        per_entry_bytes: unit,
        projected_intervals: projected,
        per_split_work: 12usize.checked_add(output_work)?,
        evidence_cache_bytes: cache_bytes,
    })
}

#[cfg(test)]
pub(super) fn indexed_copy_count_for_test(
    assessor: &mut Assessor<'_, '_>,
    partitions: [&[ResolutionRange]; 2],
    original: &[UnresolvedRegion],
) -> Option<usize> {
    indexed_diagnostic_allowance(assessor, partitions, original, COARSE_MEMORY_BYTES)
        .map(|value| value.copy_count)
}

impl Assessor<'_, '_> {
    /// Finishes optional whole-coarse literal equality after every previous
    /// recovery, review and coarse proof. Refusal leaves all prior output
    /// untouched; no candidate, changed state or coarse record is removed.
    pub(super) fn recover_mandatory_coarse_equalities(
        &mut self,
        domains: &[(DomainKey, DomainProof)],
        domain_capacity: usize,
        ownership: &mut [Ownership; 2],
        mut partitions: [&mut Vec<ResolutionRange>; 2],
        constraints: EqualConstraints<'_>,
    ) -> Result<()> {
        let EqualConstraints {
            candidates: _,
            regions,
            original,
        } = constraints;
        if regions.is_empty()
            || self.remaining_work == 0
            || self.output_stop.is_some()
            || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
        {
            return Ok(());
        }
        let Some(domain_bytes) =
            coarse_reviewed_domain_bytes(domains, domain_capacity, &mut self.remaining_work)
        else {
            return Ok(());
        };
        let Some(source_bytes) = self.cached_literal_source_capacity_bound() else {
            return Ok(());
        };
        let Some(available_diagnostics) = publication_memory(
            domain_bytes,
            source_bytes,
            0,
            &self.records,
            self.records.capacity(),
            ownership,
            [&*partitions[0], &*partitions[1]],
        )
        .and_then(|bytes| COARSE_MEMORY_BYTES.checked_sub(bytes)) else {
            return Ok(());
        };
        let Some(diagnostics) = diagnostic_allowance(
            self,
            [&*partitions[0], &*partitions[1]],
            original,
            available_diagnostics,
        ) else {
            return Ok(());
        };
        let mut resources = TailResources {
            source_bytes,
            retained_children: 0,
            group_peak_bytes: 0,
            diagnostics,
        };
        // No record, ownership or partition changed during diagnostic setup;
        // reuse its completed live-capacity ledger before adding this filter.
        let filter = if charge_work(&mut self.remaining_work, 6) {
            resources
                .diagnostics
                .bytes()
                .and_then(|bytes| available_diagnostics.checked_sub(bytes))
                .and_then(|bytes| CoarsePairFilter::build(regions, bytes, &mut self.remaining_work))
        } else {
            None
        };
        if let Some(filter) = &filter {
            let Some(source_bytes) = resources.source_bytes.checked_add(filter.bytes) else {
                return Ok(());
            };
            resources.source_bytes = source_bytes;
        }
        let mut tail_analyses = None;
        // Preserve every existing maximal-run attempt across all parents
        // before spending optional work on a smaller source-complete cut.
        // Both passes share the same retained-payload and diagnostic allowance.
        // The page-shift route runs only after both existing same-page passes.
        // It uses the same cache and requested-capacity/publication ledger.
        for (page_shifted, singleton_fallback) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            for (domain_index, (key, proof)) in domains.iter().enumerate() {
                if self.remaining_work == 0
                    || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(1)
                {
                    break;
                }
                if proof.scope != ProofScope::ExactKey
                    || proof.search != SearchCompleteness::Complete
                {
                    continue;
                }
                if page_shifted && key.local.is_some() {
                    continue;
                }
                let parent_index = proof.relation;
                let parent = &self.records[parent_index];
                if !page_shifted
                    && let Some(filter) = &filter
                    && filter.may_match(regions, parent, &mut self.remaining_work) == Some(false)
                {
                    continue;
                }
                if key.local.is_none() {
                    // Pay the marker scan and bounded two-sided field comparisons
                    // before accepting the isolated whole-domain representation.
                    if !charge_work(
                        &mut self.remaining_work,
                        parent.assumptions.len().saturating_add(16),
                    ) {
                        break;
                    }
                    if !isolated_whole_key(self.sides, key, parent) {
                        continue;
                    }
                }
                if page_shifted {
                    if !charge_work(&mut self.remaining_work, 4) {
                        break;
                    }
                    let lengths = parent
                        .old_span
                        .as_ref()
                        .zip(parent.new_span.as_ref())
                        .map(|(old, new)| [old.comparable_range.end, new.comparable_range.end]);
                    if proof.exact_lengths() != lengths {
                        continue;
                    }
                    // Prepay bounded range/header checks before rejecting an
                    // impossible page-shift premise. The full guard is retained.
                    if !charge_work(&mut self.remaining_work, 16) {
                        break;
                    }
                    if !page_shift_key_pages_differ(self.sides, key) {
                        continue;
                    }
                    // Pay the cheap necessary header walk before avoiding a
                    // full region/parent scan that cannot find this block pair.
                    if !charge_work(
                        &mut self.remaining_work,
                        regions.len().saturating_mul(16).saturating_add(16),
                    ) {
                        break;
                    }
                    if !page_shift_coarse_pair_present(parent, regions) {
                        continue;
                    }
                }
                if !charge_work(
                    &mut self.remaining_work,
                    if page_shifted {
                        regions
                            .len()
                            .saturating_mul(
                                parent
                                    .assumptions
                                    .len()
                                    .saturating_mul(3)
                                    .saturating_add(64),
                            )
                            .saturating_add(self.records.len())
                    } else {
                        regions.len().saturating_add(self.records.len())
                    },
                ) {
                    break;
                }
                let Some(region_index) = regions.iter().position(|region| {
                    if page_shifted {
                        whole_coarse_page_shift_parent(self.sides, parent, region)
                    } else {
                        whole_coarse_parent(self.sides, parent, region)
                    }
                }) else {
                    continue;
                };
                // An isolated local parent has no root dependency. Any actual
                // dependency must retain a complete clean internal proof too.
                let mut ancestor = parent.parent;
                let mut child = parent_index;
                let mut clean = true;
                while let Some(index) = ancestor {
                    if index >= child
                        || !charge_work(&mut self.remaining_work, domains.len().saturating_add(1))
                    {
                        clean = false;
                        break;
                    }
                    let record = &self.records[index];
                    clean &= record.outcome == RelationOutcome::Established
                        && record.search == SearchCompleteness::Complete
                        && record.reasons.is_empty()
                        && !record
                            .assumptions
                            .contains(&ComparisonAssumption::AlternativeLineBreakNormalization)
                        && domains.iter().any(|(_, proof)| {
                            proof.relation == index
                                && proof.scope == ProofScope::ExactKey
                                && proof.search == SearchCompleteness::Complete
                        });
                    if !clean {
                        break;
                    }
                    ancestor = record.parent;
                    child = index;
                }
                if !clean {
                    continue;
                }
                let (Some(old_span), Some(new_span)) = (&parent.old_span, &parent.new_span) else {
                    continue;
                };
                let Some(base_bytes) = publication_memory(
                    domain_bytes,
                    resources.source_bytes,
                    resources.retained_children,
                    &self.records,
                    self.records.capacity(),
                    ownership,
                    [&*partitions[0], &*partitions[1]],
                )
                .and_then(|bytes| bytes.checked_add(resources.diagnostics.bytes()?)) else {
                    continue;
                };
                let Some(view_limit) = COARSE_MEMORY_BYTES.checked_sub(base_bytes) else {
                    break;
                };
                let mut groups = [None, None];
                for (side, span) in [old_span, new_span].into_iter().enumerate() {
                    let index = self.sides[side].index[&span.blocks[0]];
                    let Ok(metadata) = light_group_metadata(
                        self.sides[side],
                        index..index + 1,
                        super::BlockSeparator::Concatenate,
                        &mut self.remaining_work,
                        view_limit,
                    ) else {
                        continue;
                    };
                    groups[side] = plan_group_materialization(
                        self.sides[side],
                        index..index + 1,
                        &metadata,
                        &mut self.remaining_work,
                        view_limit,
                    )
                    .ok();
                }
                let [Some(old_group), Some(new_group)] = groups else {
                    continue;
                };
                let groups = [old_group, new_group];
                let group_bytes = groups[0]
                    .bytes
                    .max(groups[1].bytes)
                    .max(resources.group_peak_bytes);
                let Some(view_limit) = view_limit.checked_sub(group_bytes) else {
                    continue;
                };
                let Ok(views) = borrowed_group_pair(
                    self.sides,
                    [old_span, new_span],
                    &mut self.remaining_work,
                    view_limit,
                ) else {
                    continue;
                };
                let tokens = [views[0].selected_tokens(), views[1].selected_tokens()];
                if proof.exact_lengths() != Some([tokens[0].len(), tokens[1].len()]) {
                    continue;
                }
                let Some(analysis_limit) = view_limit
                    .checked_sub(views[0].retained_bytes)
                    .and_then(|bytes| bytes.checked_sub(views[1].retained_bytes))
                else {
                    continue;
                };
                if !charge_work(&mut self.remaining_work, 1) {
                    break;
                }
                let cached = self.mandatory_analyses.get(key).cloned();
                let tail_cached = if cached.is_none() {
                    if let Some(cache) = &tail_analyses {
                        let Some(value) = TailMandatoryAnalyses::get(
                            cache,
                            self.sides,
                            domain_index,
                            key,
                            &mut self.remaining_work,
                        ) else {
                            break;
                        };
                        value
                    } else {
                        None
                    }
                } else {
                    None
                };
                let analysis_start = self.remaining_work;
                let mut fresh = None;
                let analysis = match cached.as_ref() {
                    Some(Some(analysis)) => analysis.as_ref(),
                    Some(None) => continue,
                    None => {
                        if let Some(analysis) = tail_cached {
                            analysis
                        } else {
                            fresh = match semantic::mandatory_literal_analysis_with_memory_limit(
                                tokens[0],
                                tokens[1],
                                &mut self.remaining_work,
                                analysis_limit,
                            ) {
                                Ok(Some(analysis)) => Some(analysis),
                                Ok(None)
                                | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => {
                                    continue;
                                }
                                Err(error) => return Err(error),
                            };
                            fresh.as_ref().expect("the fresh analysis just completed")
                        }
                    }
                };
                let completed_work = analysis_start.saturating_sub(self.remaining_work);
                let Some(proof_bytes) = analysis
                    .pairs()
                    .len()
                    .checked_mul(std::mem::size_of::<(usize, usize)>())
                    .and_then(|bytes| bytes.checked_add(views[0].retained_bytes))
                    .and_then(|bytes| bytes.checked_add(views[1].retained_bytes))
                else {
                    continue;
                };
                if proof_bytes > view_limit {
                    continue;
                }
                if !charge_work(
                    &mut self.remaining_work,
                    analysis.pairs().len().saturating_mul(2),
                ) {
                    break;
                }
                let mut start = 0;
                while start < analysis.pairs().len() {
                    let first_index = start;
                    let first = analysis.pairs()[start];
                    if !matches!(tokens[0][first.0 - 1], ComparableToken::Scalar(c) if !c.is_whitespace())
                    {
                        start += 1;
                        continue;
                    }
                    let mut end = start + 1;
                    while end < analysis.pairs().len() {
                        let before = analysis.pairs()[end - 1];
                        let next = analysis.pairs()[end];
                        if next != (before.0 + 1, before.1 + 1)
                            || !matches!(tokens[0][next.0 - 1], ComparableToken::Scalar(c) if !c.is_whitespace())
                        {
                            break;
                        }
                        end += 1;
                    }
                    let last = analysis.pairs()[end - 1];
                    let ranges = [first.0 - 1..last.0, first.1 - 1..last.1];
                    start = end;
                    if self.remaining_work == 0
                        || self.records.len()
                            >= self.options.max_assessment_ranges.saturating_sub(1)
                    {
                        break;
                    }
                    if singleton_fallback && end - first_index == 1 {
                        continue;
                    }
                    let mut source_held = false;
                    self.commit_mandatory_coarse_equal_run(
                        EqualRun {
                            page_shifted,
                            parent: parent_index,
                            region: region_index,
                            views: &views,
                            ranges,
                            domain_bytes: domain_bytes
                                .saturating_add(proof_bytes)
                                .saturating_add(group_bytes),
                            groups: &groups,
                        },
                        ownership,
                        &mut partitions,
                        constraints,
                        &mut resources,
                        singleton_fallback.then_some(&mut source_held),
                    )?;
                    if singleton_fallback && source_held {
                        for &(old_pair, new_pair) in &analysis.pairs()[first_index..end] {
                            if self.remaining_work == 0
                                || self.records.len()
                                    >= self.options.max_assessment_ranges.saturating_sub(1)
                            {
                                break;
                            }
                            self.commit_mandatory_coarse_equal_run(
                                EqualRun {
                                    page_shifted,
                                    parent: parent_index,
                                    region: region_index,
                                    views: &views,
                                    ranges: [old_pair - 1..old_pair, new_pair - 1..new_pair],
                                    domain_bytes: domain_bytes
                                        .saturating_add(proof_bytes)
                                        .saturating_add(group_bytes),
                                    groups: &groups,
                                },
                                ownership,
                                &mut partitions,
                                constraints,
                                &mut resources,
                                None,
                            )?;
                        }
                    }
                }
                if let Some(analysis) = fresh {
                    // The optional cache is funded after this parent's original
                    // attempts. Count its arrays alongside current views,
                    // groups, diagnostics and retained publication before use.
                    let available = publication_memory(
                        domain_bytes,
                        source_bytes,
                        resources.retained_children,
                        &self.records,
                        self.records.capacity(),
                        ownership,
                        [&*partitions[0], &*partitions[1]],
                    )
                    .and_then(|bytes| bytes.checked_add(resources.diagnostics.bytes()?))
                    .and_then(|bytes| bytes.checked_add(group_bytes))
                    .and_then(|bytes| bytes.checked_add(views[0].retained_bytes))
                    .and_then(|bytes| bytes.checked_add(views[1].retained_bytes))
                    .and_then(|bytes| COARSE_MEMORY_BYTES.checked_sub(bytes));
                    if let Some(available) = available {
                        if tail_analyses.is_none() {
                            tail_analyses = TailMandatoryAnalyses::new(
                                domains,
                                self.sides,
                                completed_work,
                                available.saturating_sub(
                                    analysis.retained_heap_bytes().unwrap_or(usize::MAX),
                                ),
                                &mut self.remaining_work,
                            );
                        }
                        if let Some(cache) = &mut tail_analyses {
                            cache.store(
                                domain_index,
                                analysis,
                                available,
                                &mut self.remaining_work,
                            );
                            resources.source_bytes = source_bytes.saturating_add(cache.bytes);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn commit_mandatory_coarse_equal_run(
        &mut self,
        run: EqualRun<'_, '_>,
        ownership: &mut [Ownership; 2],
        partitions: &mut [&mut Vec<ResolutionRange>; 2],
        constraints: EqualConstraints<'_>,
        resources: &mut TailResources,
        source_held: Option<&mut bool>,
    ) -> Result<bool> {
        let EqualRun {
            page_shifted,
            parent: parent_index,
            region: region_index,
            views,
            ranges,
            domain_bytes,
            groups,
        } = run;
        let EqualConstraints {
            candidates,
            regions,
            original: _,
        } = constraints;
        let source_bytes = resources.source_bytes;
        let retained_children = &mut resources.retained_children;
        let ids = views[0].blocks.len().saturating_add(views[1].blocks.len());
        if !charge_work(
            &mut self.remaining_work,
            ids.saturating_mul(usize::BITS as usize + 8),
        ) {
            return Ok(false);
        }
        let (Some(old), Some(new)) = (
            views[0].try_span(ranges[0].clone()),
            views[1].try_span(ranges[1].clone()),
        ) else {
            return Ok(false);
        };
        let spans = [&old, &new];
        let Some(projected) =
            coarse_project(self.sides[0], &old).zip(coarse_project(self.sides[1], &new))
        else {
            return Ok(false);
        };
        let ([old_interval], [new_interval]) = (projected.0.as_slice(), projected.1.as_slice())
        else {
            return Ok(false);
        };
        let intervals = [*old_interval, *new_interval];
        for side in 0..2 {
            if unresolved_cover_probe(
                self.sides[side],
                partitions[side],
                spans[side].blocks[0],
                &intervals[side],
                &mut self.remaining_work,
            ) != Some(true)
            {
                return Ok(false);
            }
        }
        // Candidate ownership and every other coarse obligation retain their
        // original veto. Only this exact whole parent's coarse proof composes.
        for (side, selected) in intervals.iter().enumerate() {
            for span in candidates
                .iter()
                .flat_map(|candidate| &candidate.change.occurrences)
                .filter_map(|occurrence| {
                    if side == 0 {
                        occurrence.old_span.as_ref()
                    } else {
                        occurrence.new_span.as_ref()
                    }
                })
                .chain(
                    regions
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != region_index)
                        .filter_map(|(_, region)| {
                            if side == 0 {
                                region.old_span.as_ref()
                            } else {
                                region.new_span.as_ref()
                            }
                        }),
                )
            {
                if ownership_projection_clear(
                    self.sides[side],
                    span,
                    selected,
                    &mut self.remaining_work,
                ) != Some(true)
                {
                    return Ok(false);
                }
            }
        }
        let assumptions = self.records[parent_index]
            .assumptions
            .len()
            .saturating_add(if page_shifted { 2 } else { 1 });
        // Fixed logical metadata allowance: three capacity/limit/target rows
        // and checked transient-byte/copy sums. Every planned route pays it.
        if !charge_work(&mut self.remaining_work, 128) {
            return Ok(false);
        }
        let Some(child_bytes) = assumptions
            .checked_mul(std::mem::size_of::<ComparisonAssumption>())
            .and_then(|bytes| bytes.checked_add(ids.checked_mul(std::mem::size_of::<BlockId>())?))
        else {
            return Ok(false);
        };
        if retained_children.checked_add(child_bytes).is_none() {
            return Ok(false);
        }
        let next_retained_children;
        let Some(new_bytes) = publication_memory(
            domain_bytes,
            source_bytes,
            *retained_children,
            &self.records,
            self.records.capacity(),
            ownership,
            [&*partitions[0], &*partitions[1]],
        )
        .and_then(|bytes| bytes.checked_add(resources.diagnostics.bytes()?))
        .and_then(|bytes| {
            bytes.checked_add(assumptions.checked_mul(std::mem::size_of::<ComparisonAssumption>())?)
        })
        .and_then(|bytes| bytes.checked_add(ids.checked_mul(std::mem::size_of::<BlockId>())?)) else {
            return Ok(false);
        };
        let Some(new_bytes) = new_bytes.checked_add(4 * std::mem::size_of::<usize>()) else {
            return Ok(false);
        };
        if new_bytes > COARSE_MEMORY_BYTES {
            return Ok(false);
        }
        let Some(plan) = publication_growth(
            [
                self.records.len(),
                ownership[0].accepted.len(),
                ownership[1].accepted.len(),
            ],
            [
                self.records.capacity(),
                ownership[0].accepted.capacity(),
                ownership[1].accepted.capacity(),
            ],
            self.options.max_assessment_ranges,
            new_bytes,
            COARSE_MEMORY_BYTES,
        ) else {
            return Ok(false);
        };
        let mut growth = plan;
        let partition_count = partitions[0].len().saturating_add(partitions[1].len());
        // Sixteen units cover the two local three-piece counters. The original
        // two population scans remain the route for smaller partitions.
        let counted_split = partition_count.saturating_mul(2) >= 16;
        let count_work = if counted_split {
            16
        } else {
            partition_count.saturating_mul(2)
        };
        let commit_work = partition_count
            .saturating_mul(shallow_words::<ResolutionRange>())
            .saturating_add(count_work)
            // Both checked route selectors are additional paid metadata.
            .saturating_add(4)
            .saturating_add(growth.copy_work)
            .saturating_add(regions.len().saturating_mul(usize::BITS as usize + 8))
            .saturating_add(assumptions)
            .saturating_add(8)
            // Every new header is written, even when backing arrays stay.
            .saturating_add(shallow_words::<RelationAssessment>())
            .saturating_add(2usize.saturating_mul(shallow_words::<SourceInterval>()))
            // Post-reserve metadata, actual-capacity ledger and a possible
            // minimum-target downgrade are paid before publication-capacity allocations.
            .saturating_add(128);
        if !charge_work(&mut self.remaining_work, commit_work) {
            return Ok(false);
        }
        let prepare = |side: usize| {
            let (block, interval) = if side == 0 {
                (old.blocks[0], intervals[0])
            } else {
                (new.blocks[0], intervals[1])
            };
            let range = interval.start..interval.end;
            if counted_split {
                partition_split::<true>(partitions[side], block, range)
            } else {
                equal_partition(partitions[side], block, range).map(|items| (items, 0))
            }
        };
        let (Some((next_old, old_extra)), Some((next_new, new_extra))) = (prepare(0), prepare(1))
        else {
            return Ok(false);
        };
        // Splitting one unresolved range creates at most one additional
        // whole-block materialization per side. Its nested copies and final
        // diagnostic scans/capacity are approved before any source verdict.
        let mut extra = 0usize;
        let mut regeneration_work = 0usize;
        for (side, next) in [&next_old, &next_new].into_iter().enumerate() {
            let additional = if counted_split {
                [old_extra, new_extra][side]
            } else {
                let before = partitions[side]
                    .iter()
                    .filter(|p| p.state == ResolutionState::Unresolved)
                    .count();
                let after = next
                    .iter()
                    .filter(|p| p.state == ResolutionState::Unresolved)
                    .count();
                after.saturating_sub(before)
            };
            extra = extra.saturating_add(additional);
            regeneration_work =
                regeneration_work.saturating_add(additional.saturating_mul(groups[side].copy_work));
        }
        let Some(next_diagnostic_count) = resources.diagnostics.upper_count.checked_add(extra)
        else {
            return Ok(false);
        };
        if next_diagnostic_count > self.options.max_assessment_ranges {
            return Ok(false);
        }
        let Some(extra_bytes) = extra.checked_mul(resources.diagnostics.per_entry_bytes) else {
            return Ok(false);
        };
        {
            let plan = &mut growth;
            // A split's final diagnostic allowance was not known at the first
            // plan. The same paid old-array move suffices for minimum growth.
            if new_bytes
                .checked_add(plan.extra_bytes)
                .and_then(|bytes| bytes.checked_add(extra_bytes))
                .is_none_or(|bytes| bytes > COARSE_MEMORY_BYTES)
            {
                plan.targets = plan.minimum;
                plan.extra_bytes = 0;
            }
        }
        if new_bytes
            .checked_add(extra_bytes)
            .is_none_or(|bytes| bytes > COARSE_MEMORY_BYTES)
        {
            return Ok(false);
        }
        regeneration_work = regeneration_work
            .saturating_add(extra.saturating_mul(resources.diagnostics.per_split_work))
            .saturating_add(2usize.saturating_mul(resources.diagnostics.projected_intervals))
            .saturating_add(partitions[0].len().saturating_add(partitions[1].len()));
        if !charge_work(&mut self.remaining_work, regeneration_work) {
            return Ok(false);
        }
        let mut child_assumptions = Vec::new();
        {
            let plan = growth;
            if child_assumptions.try_reserve_exact(assumptions).is_err()
                || self
                    .records
                    .try_reserve_exact(plan.targets[0] - self.records.len())
                    .is_err()
                || ownership.iter_mut().enumerate().any(|(index, owner)| {
                    owner.accepted.len() >= self.options.max_assessment_ranges
                        || owner
                            .accepted
                            .try_reserve_exact(plan.targets[index + 1] - owner.accepted.len())
                            .is_err()
                })
            {
                return Ok(false);
            }
            // Reserve may report more capacity than requested. Count the
            // actual retained arrays before invoking any source proof.
            let actual_bytes = publication_memory(
                domain_bytes,
                source_bytes,
                *retained_children,
                &self.records,
                self.records.capacity(),
                ownership,
                [&*partitions[0], &*partitions[1]],
            )
            .and_then(|bytes| bytes.checked_add(resources.diagnostics.bytes()?))
            .and_then(|bytes| {
                bytes.checked_add(
                    child_assumptions
                        .capacity()
                        .checked_mul(std::mem::size_of::<ComparisonAssumption>())?,
                )
            })
            .and_then(|bytes| bytes.checked_add(ids.checked_mul(std::mem::size_of::<BlockId>())?))
            .and_then(|bytes| bytes.checked_add(extra_bytes));
            if actual_bytes.is_none_or(|bytes| bytes > COARSE_MEMORY_BYTES) {
                return Ok(false);
            }
            let Some(actual_child_bytes) = child_assumptions
                .capacity()
                .checked_mul(std::mem::size_of::<ComparisonAssumption>())
                .and_then(|bytes| {
                    bytes.checked_add(ids.checked_mul(std::mem::size_of::<BlockId>())?)
                })
            else {
                return Ok(false);
            };
            let Some(actual_retained) = retained_children.checked_add(actual_child_bytes) else {
                return Ok(false);
            };
            next_retained_children = actual_retained;
        }
        child_assumptions.extend_from_slice(&self.records[parent_index].assumptions);
        if !child_assumptions.contains(&ComparisonAssumption::MandatoryMatchingEquality) {
            child_assumptions.push(ComparisonAssumption::MandatoryMatchingEquality);
        }
        if page_shifted
            && !child_assumptions
                .contains(&ComparisonAssumption::PageShiftedMandatoryMatchingEquality)
        {
            child_assumptions.push(ComparisonAssumption::PageShiftedMandatoryMatchingEquality);
        }
        let cache = self
            .equal_fragment_cache
            .get_or_insert_with(|| equal_fragment::EqualFragmentCache::new(self.sides));
        let source_verdict = if page_shifted {
            cache.prove_sources_for_page_shifted_mandatory_pairs(spans, &mut self.remaining_work)
        } else {
            cache.prove_sources_for_mandatory_pairs(spans, &mut self.remaining_work)
        };
        match source_verdict {
            Ok(equal_fragment::FragmentVerdict::Proven) => {}
            Ok(equal_fragment::FragmentVerdict::Held(_)) => {
                if let Some(source_held) = source_held {
                    *source_held = true;
                }
                return Ok(false);
            }
            Ok(equal_fragment::FragmentVerdict::Exhausted)
            | Err(Error::LimitExceeded { .. } | Error::Unresolved(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        // Every retained copy, capacity growth and partition scan is prepaid.
        // Both sides and the certificate publish together, even if the last
        // completed source check used the remaining work exactly.
        ownership[0].accepted.push(intervals[0]);
        ownership[1].accepted.push(intervals[1]);
        *partitions[0] = next_old;
        *partitions[1] = next_new;
        self.records.push(RelationAssessment {
            old_span: Some(old),
            new_span: Some(new),
            outcome: RelationOutcome::Established,
            reasons: Vec::new(),
            assumptions: child_assumptions,
            search: SearchCompleteness::Complete,
            parent: Some(parent_index),
        });
        *retained_children = next_retained_children;
        resources.diagnostics.upper_count = next_diagnostic_count;
        resources.group_peak_bytes = resources
            .group_peak_bytes
            .max(groups[0].bytes.max(groups[1].bytes));
        Ok(true)
    }
}

#[cfg(test)]
mod publication_growth_tests {
    use super::*;

    #[test]
    fn h22_growth_full_spare_and_asymmetric_capacities() {
        let words = shallow_words::<RelationAssessment>();
        let interval = shallow_words::<SourceInterval>();
        let record = RelationAssessment {
            old_span: None,
            new_span: None,
            outcome: RelationOutcome::Tentative,
            reasons: Vec::new(),
            assumptions: Vec::new(),
            search: SearchCompleteness::Incomplete,
            parent: None,
        };
        let mut records = vec![record; 3].into_boxed_slice().into_vec();
        let interval_header = SourceInterval {
            block_index: 0,
            start: 0,
            end: 1,
        };
        let mut old = vec![interval_header; 2].into_boxed_slice().into_vec();
        let mut new = vec![interval_header; 5].into_boxed_slice().into_vec();
        let lengths = [records.len(), old.len(), new.len()];
        let full = publication_growth(
            lengths,
            [records.capacity(), old.capacity(), new.capacity()],
            100,
            1_000,
            10_000,
        )
        .expect("fits");
        assert_eq!(full.targets, [6, 4, 10]);
        assert_eq!(full.minimum, [4, 3, 6]);
        assert_eq!(full.copy_work, 3 * words + 7 * interval);
        records
            .try_reserve_exact(full.targets[0] - records.len())
            .expect("capacity");
        old.try_reserve_exact(full.targets[1] - old.len())
            .expect("capacity");
        new.try_reserve_exact(full.targets[2] - new.len())
            .expect("capacity");
        let spare = publication_growth(
            [3, 2, 5],
            [records.capacity(), old.capacity(), new.capacity()],
            100,
            1_000,
            10_000,
        )
        .expect("spare");
        assert_eq!(spare.copy_work, 0);
        assert_eq!(spare.extra_bytes, 0);
        assert_eq!(
            spare.targets,
            [records.capacity(), old.capacity(), new.capacity()]
        );
        let asymmetric =
            publication_growth([3, 2, 5], [6, 2, 9], 100, 1_000, 10_000).expect("asymmetric");
        assert_eq!(asymmetric.targets, [6, 4, 9]);
        assert_eq!(asymmetric.copy_work, 2 * interval);
    }

    #[test]
    fn ordinary_publication_counts_first_moves_and_reuses_actual_spare_capacity() {
        let record = RelationAssessment {
            old_span: None,
            new_span: None,
            outcome: RelationOutcome::Tentative,
            reasons: Vec::new(),
            assumptions: Vec::new(),
            search: SearchCompleteness::Incomplete,
            parent: None,
        };
        let interval = SourceInterval {
            block_index: 0,
            start: 0,
            end: 1,
        };
        let mut records = vec![record.clone(); 1727].into_boxed_slice().into_vec();
        let mut old = vec![interval; 369].into_boxed_slice().into_vec();
        let mut new = vec![interval; 367].into_boxed_slice().into_vec();
        let plan = publication_growth(
            [records.len(), old.len(), new.len()],
            [records.capacity(), old.capacity(), new.capacity()],
            10_000,
            55_588_277,
            COARSE_MEMORY_BYTES,
        )
        .expect("same measured full capacities");
        assert_eq!(
            plan.copy_work,
            1727 * shallow_words::<RelationAssessment>() + 736 * shallow_words::<SourceInterval>()
        );
        assert!(plan.extra_bytes > 0);
        records
            .try_reserve_exact(plan.targets[0] - records.len())
            .expect("paid publication capacity");
        old.try_reserve_exact(plan.targets[1] - old.len())
            .expect("paid publication capacity");
        new.try_reserve_exact(plan.targets[2] - new.len())
            .expect("paid publication capacity");
        records.push(record);
        old.push(interval);
        new.push(interval);
        let next = publication_growth(
            [records.len(), old.len(), new.len()],
            [records.capacity(), old.capacity(), new.capacity()],
            10_000,
            55_588_277,
            COARSE_MEMORY_BYTES,
        )
        .expect("actual paid backing arrays retained");
        assert_eq!(next.copy_work, 0);
        assert_eq!(next.extra_bytes, 0);
        assert_eq!(
            next.targets,
            [records.capacity(), old.capacity(), new.capacity()]
        );
    }

    #[test]
    fn h22_growth_bounds_keep_minimum_fallback_and_overflow_refusals() {
        let minimum =
            publication_growth([3, 2, 5], [3, 2, 5], 100, 1_000, 1_000).expect("minimum fits");
        assert_eq!(minimum.targets, [4, 3, 6]);
        assert_eq!(minimum.extra_bytes, 0);
        assert!(publication_growth([3, 2, 5], [3, 2, 5], 100, 1_001, 1_000).is_none());
        assert_eq!(
            publication_growth([3, 2, 5], [3, 2, 5], 6, 1_000, 10_000)
                .expect("capped")
                .targets,
            [6, 4, 6]
        );
        assert!(publication_growth([3, 2, 5], [3, 2, 5], 5, 1_000, 10_000).is_none());
        assert!(
            publication_growth(
                [usize::MAX, 0, 0],
                [usize::MAX, 0, 0],
                usize::MAX,
                0,
                usize::MAX
            )
            .is_none()
        );
        assert!(
            publication_growth(
                [usize::MAX - 1, 0, 0],
                [usize::MAX - 1, 0, 0],
                usize::MAX,
                0,
                usize::MAX
            )
            .is_none()
        );
        assert!(publication_growth([3, 2, 5], [2, 2, 5], 100, 0, 10_000).is_none());
    }
}

#[cfg(test)]
mod tail_analysis_tests {
    use super::*;

    fn domains() -> Vec<(DomainKey, DomainProof)> {
        vec![(
            DomainKey {
                local: None,
                old: 0..1,
                new: 0..1,
                old_separator: super::super::BlockSeparator::Concatenate,
                new_separator: super::super::BlockSeparator::Concatenate,
            },
            DomainProof {
                scope: ProofScope::ExactKey,
                relation: 0,
                unique: false,
                search: SearchCompleteness::Complete,
                edits: Vec::new(),
                lengths: [16, 16],
                strict_unique: false,
                stable_events: None,
            },
        )]
    }

    #[test]
    fn completed_matching_reuse_fits_a_budget_that_cannot_recompute() -> Result<()> {
        let side = super::super::super::SidePlan::inspect("tail cache", &[])?.materialize()?;
        let sides = [&side, &side];
        let domains = domains();
        let mut budget = 100_000;
        let before = budget;
        let analysis = semantic::mandatory_match_analysis(
            b"abcdeXghijklmnopq",
            b"abcdeYghijklmnopq",
            &mut budget,
        )?
        .expect("completed analysis");
        let expected = analysis.pairs().to_vec();
        let mut cache =
            TailMandatoryAnalyses::new(&domains, sides, before - budget, 1_000_000, &mut budget)
                .expect("bounded affordable table");
        assert!(cache.store(0, analysis, 1_000_000, &mut budget));
        let mut repeat_budget = 6;
        assert!(
            semantic::mandatory_match_analysis(
                b"abcdeXghijklmnopq",
                b"abcdeYghijklmnopq",
                &mut repeat_budget,
            )?
            .is_none()
        );
        assert_eq!(repeat_budget, 6);
        let reused = cache
            .get(sides, 0, &domains[0].0, &mut repeat_budget)
            .expect("paid query")
            .expect("completed cached analysis");
        assert_eq!(reused.pairs(), expected);
        assert_eq!(repeat_budget, 0);
        assert!(cache.get(sides, 0, &domains[0].0, &mut 5).is_none());
        Ok(())
    }

    #[test]
    fn cache_misses_and_refusals_never_publish_partial_matching() -> Result<()> {
        let side = super::super::super::SidePlan::inspect("tail cache", &[])?.materialize()?;
        let other = super::super::super::SidePlan::inspect("other side", &[])?.materialize()?;
        let domains = domains();
        let sides = [&side, &side];
        let mut budget = 100_000;
        let mut cache = TailMandatoryAnalyses::new(&domains, sides, 10_000, 1_000_000, &mut budget)
            .expect("bounded table");
        assert!(
            cache
                .get(sides, 0, &domains[0].0, &mut budget)
                .expect("the lookup has sufficient work")
                .is_none()
        );
        assert!(semantic::mandatory_match_analysis(b"abc", b"abc", &mut 1)?.is_none());
        assert!(
            cache
                .get(sides, 0, &domains[0].0, &mut budget)
                .expect("the lookup has sufficient work")
                .is_none()
        );
        let analysis = semantic::mandatory_match_analysis(b"abc", b"abc", &mut budget)?
            .expect("complete matching");
        assert!(!cache.store(0, analysis, 0, &mut budget));
        assert!(
            cache
                .get(sides, 0, &domains[0].0, &mut budget)
                .expect("the lookup has sufficient work")
                .is_none()
        );
        let analysis = semantic::mandatory_match_analysis(b"abc", b"abc", &mut budget)?
            .expect("complete matching");
        assert!(cache.store(0, analysis, 1_000_000, &mut budget));
        assert!(
            cache
                .get([&other, &side], 0, &domains[0].0, &mut budget)
                .expect("the lookup has sufficient work")
                .is_none()
        );
        assert!(
            cache
                .get(sides, 0, &domains[0].0.clone(), &mut budget)
                .expect("the lookup has sufficient work")
                .is_none()
        );
        assert!(
            cache
                .get(sides, 1, &domains[0].0, &mut budget)
                .expect("the lookup has sufficient work")
                .is_none()
        );
        for (work, bytes, remaining) in [
            (1, 1_000_000, 100_000),
            (10_000, 0, 100_000),
            (10_000, 1_000_000, 16),
        ] {
            let mut remaining = remaining;
            let before = remaining;
            assert!(
                TailMandatoryAnalyses::new(&domains, sides, work, bytes, &mut remaining).is_none()
            );
            assert_eq!(remaining, before);
        }
        Ok(())
    }
}

#[cfg(test)]
mod coarse_pair_presence_tests {
    use super::*;
    use crate::diff::{ChangedRegionProof, Confidence, TextSpan};

    #[test]
    fn unresolved_cover_keeps_the_predicate_and_pays_only_visited_ranges() {
        let mut parts = (0..100)
            .map(|i| ResolutionRange {
                block: BlockId(i),
                comparable_range: TokenRange { start: 0, end: 4 },
                canonical_range: ScalarRange { start: 0, end: 4 },
                state: ResolutionState::Unresolved,
            })
            .collect::<Vec<_>>();
        parts[2].state = ResolutionState::Equal;
        for block in [0, 2, 5, 99, 100] {
            for (start, end) in [(0, 4), (1, 3), (0, 5)] {
                let selected = SourceInterval {
                    block_index: block as usize,
                    start,
                    end,
                };
                let expected = parts.iter().any(|p| {
                    p.block == BlockId(block)
                        && p.state == ResolutionState::Unresolved
                        && p.comparable_range.start <= start
                        && end <= p.comparable_range.end
                });
                let mut budget = 1_000;
                assert_eq!(
                    unresolved_cover(&parts, BlockId(block), &selected, &mut budget),
                    Some(expected)
                );
                let used = 1_000 - budget;
                assert_eq!(
                    unresolved_cover(&parts, BlockId(block), &selected, &mut (used - 1)),
                    None
                );
                let mut exact = used;
                assert_eq!(
                    unresolved_cover(&parts, BlockId(block), &selected, &mut exact),
                    Some(expected)
                );
                assert_eq!(exact, 0);
                if expected {
                    assert_eq!(used, block as usize + 1);
                } else {
                    assert_eq!(used, parts.len());
                }
            }
        }
        assert_eq!(
            unresolved_cover(
                &[],
                BlockId(0),
                &SourceInterval {
                    block_index: 0,
                    start: 0,
                    end: 1,
                },
                &mut 0
            ),
            Some(false)
        );
    }

    fn filter_region(old: u64, new: u64) -> ProvenChangedRegion {
        let span = |block| TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 4 },
            comparable_range: TokenRange { start: 0, end: 4 },
        };
        ProvenChangedRegion {
            old_span: Some(span(old)),
            new_span: Some(span(new)),
            confidence: Confidence::High,
            proof: ChangedRegionProof::ExactTokenMultisetMismatch,
        }
    }

    #[test]
    fn coarse_filter_is_only_exact_context_negative_membership() {
        let mut regions = vec![
            filter_region(9, 19),
            filter_region(2, 8),
            filter_region(9, 19),
            filter_region(18, 2),
        ];
        let mut wrong = filter_region(1, 101);
        wrong.proof = ChangedRegionProof::OneSidedNonEmptyRange;
        regions.push(wrong);
        let mut multiple = filter_region(1, 101);
        multiple
            .old_span
            .as_mut()
            .expect("old")
            .blocks
            .push(BlockId(9));
        regions.push(multiple);
        let mut remaining = 1_000_000;
        let filter = CoarsePairFilter::build(&regions, 1_000_000, &mut remaining)
            .expect("small completed filter");
        for old in 0..20 {
            for new in (0..20).chain(std::iter::once(101)) {
                let target = filter_region(old, new);
                let mut parent = RelationAssessment {
                    old_span: target.old_span,
                    new_span: target.new_span,
                    outcome: RelationOutcome::Established,
                    search: SearchCompleteness::Complete,
                    reasons: Vec::new(),
                    assumptions: Vec::new(),
                    parent: None,
                };
                let expected = regions.iter().any(|r| {
                    r.proof == ChangedRegionProof::ExactTokenMultisetMismatch
                        && r.old_span == parent.old_span
                        && r.new_span == parent.new_span
                });
                assert_eq!(
                    filter.may_match(&regions, &parent, &mut remaining),
                    Some(expected)
                );
                if expected {
                    // A hit does not certify shape or retain a source verdict.
                    parent
                        .old_span
                        .as_mut()
                        .expect("old")
                        .comparable_range
                        .start = 1;
                    parent.outcome = RelationOutcome::Tentative;
                    assert_eq!(
                        filter.may_match(&regions, &parent, &mut remaining),
                        Some(true)
                    );
                }
                let cloned = regions.clone();
                assert_eq!(filter.may_match(&cloned, &parent, &mut remaining), None);
                parent.old_span = None;
                assert_eq!(
                    filter.may_match(&regions, &parent, &mut remaining),
                    Some(false)
                );
            }
        }
    }

    #[test]
    fn coarse_filter_pays_setup_lookup_and_live_capacity() {
        let regions = vec![
            filter_region(9, 19),
            filter_region(2, 8),
            filter_region(9, 19),
        ];
        let mut budget = 1_000_000;
        let filter = CoarsePairFilter::build(&regions, 1_000_000, &mut budget).expect("filter");
        let work = 1_000_000 - budget;
        for budget in [0, work - 1, 8 * work - 1] {
            assert!(CoarsePairFilter::build(&regions, filter.bytes, &mut { budget }).is_none());
        }
        assert!(CoarsePairFilter::build(&regions, filter.bytes - 1, &mut 1_000_000).is_none());
        let mut exact = 8 * work;
        assert!(CoarsePairFilter::build(&regions, filter.bytes, &mut exact).is_some());
        assert_eq!(exact, 7 * work);
        let target = &regions[0];
        let parent = RelationAssessment {
            old_span: target.old_span.clone(),
            new_span: target.new_span.clone(),
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            reasons: Vec::new(),
            assumptions: Vec::new(),
            parent: None,
        };
        let mut lookup = 1_000_000;
        assert_eq!(filter.may_match(&regions, &parent, &mut lookup), Some(true));
        let lookup_work = 1_000_000 - lookup;
        assert_eq!(
            filter.may_match(&regions, &parent, &mut (lookup_work - 1)),
            None
        );
        let mut exact = lookup_work;
        assert_eq!(filter.may_match(&regions, &parent, &mut exact), Some(true));
        assert_eq!(exact, 0);
        assert!(
            CoarsePairFilter::build(&vec![regions[0].clone(); 65], 1_000_000, &mut 1_000_000)
                .is_none()
        );
    }

    #[test]
    fn h26_presence_is_only_two_sided_named_singleton_necessity() {
        let span = |block| TextSpan {
            blocks: vec![BlockId(block)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 4 },
            comparable_range: TokenRange { start: 0, end: 4 },
        };
        let parent = RelationAssessment {
            old_span: Some(span(1)),
            new_span: Some(span(101)),
            outcome: RelationOutcome::Established,
            search: SearchCompleteness::Complete,
            reasons: Vec::new(),
            assumptions: Vec::new(),
            parent: None,
        };
        let region = ProvenChangedRegion {
            old_span: parent.old_span.clone(),
            new_span: parent.new_span.clone(),
            confidence: Confidence::High,
            proof: ChangedRegionProof::ExactTokenMultisetMismatch,
        };
        assert!(page_shift_coarse_pair_present(
            &parent,
            std::slice::from_ref(&region)
        ));
        assert!(!page_shift_coarse_pair_present(&parent, &[]));
        for side in 0..2 {
            for shape in 0..4 {
                let mut wrong = region.clone();
                let selected = if side == 0 {
                    &mut wrong.old_span
                } else {
                    &mut wrong.new_span
                };
                match shape {
                    0 => *selected = None,
                    1 => selected.as_mut().expect("side").blocks.clear(),
                    2 => selected.as_mut().expect("side").blocks.push(BlockId(9)),
                    _ => selected.as_mut().expect("side").blocks[0] = BlockId(9),
                }
                assert!(!page_shift_coarse_pair_present(&parent, &[wrong]));
            }
        }
        // Header presence intentionally does not establish full shape. These
        // cases must still reach and fail the unchanged downstream guard.
        let mut partial = region.clone();
        partial
            .old_span
            .as_mut()
            .expect("old")
            .comparable_range
            .start = 1;
        partial.new_span.as_mut().expect("new").separator =
            Some(super::super::BlockSeparator::Space);
        partial.proof = ChangedRegionProof::OneSidedNonEmptyRange;
        assert!(page_shift_coarse_pair_present(&parent, &[partial]));
        for side in 0..2 {
            let mut missing = parent.clone();
            if side == 0 {
                missing.old_span = None;
            } else {
                missing.new_span = None;
            }
            assert!(!page_shift_coarse_pair_present(
                &missing,
                std::slice::from_ref(&region)
            ));
        }
    }
}

#[cfg(test)]
mod partition_tests {
    use super::*;

    fn row(block: u64, start: usize, end: usize, state: ResolutionState) -> ResolutionRange {
        ResolutionRange {
            block: BlockId(block),
            comparable_range: TokenRange { start, end },
            canonical_range: ScalarRange { start, end },
            state,
        }
    }

    #[test]
    fn counted_partition_preserves_unrelated_projections_and_all_nonempty_cuts() {
        let mut unmapped = row(9, 3, 7, ResolutionState::Unresolved);
        unmapped.canonical_range = ScalarRange { start: 3, end: 3 };
        let source = vec![
            row(8, 0, 2, ResolutionState::Changed),
            row(1, 0, 10, ResolutionState::Unresolved),
            unmapped,
            row(7, 20, 30, ResolutionState::Equal),
        ];
        for start in 0..10 {
            for end in start + 1..=10 {
                let (actual, extra) = partition_split::<true>(&source, BlockId(1), start..end)
                    .expect("a nonempty cut inside one unresolved interval must split");
                let mut expected = vec![source[0].clone()];
                if start > 0 {
                    expected.push(row(1, 0, start, ResolutionState::Unresolved));
                }
                expected.push(row(1, start, end, ResolutionState::Equal));
                if end < 10 {
                    expected.push(row(1, end, 10, ResolutionState::Unresolved));
                }
                expected.extend_from_slice(&source[2..]);
                assert_eq!(actual, expected);
                let before = source
                    .iter()
                    .filter(|r| r.state == ResolutionState::Unresolved)
                    .count();
                let after = actual
                    .iter()
                    .filter(|r| r.state == ResolutionState::Unresolved)
                    .count();
                assert_eq!(extra, after.saturating_sub(before));
                assert_eq!(
                    equal_partition(&source, BlockId(1), start..end),
                    Some(actual)
                );
            }
        }
        assert!(partition_split::<true>(&source, BlockId(8), 0..1).is_none());
        assert!(partition_split::<true>(&source, BlockId(1), 0..11).is_none());
        let duplicate = vec![source[1].clone(), source[1].clone()];
        assert!(partition_split::<true>(&duplicate, BlockId(1), 2..8).is_none());
    }
}
