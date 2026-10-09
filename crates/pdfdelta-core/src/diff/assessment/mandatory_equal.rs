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
    let capacity = source.len().checked_add(2)?;
    let mut output = Vec::new();
    output.try_reserve_exact(capacity).ok()?;
    let mut found = false;
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
    found.then_some(output)
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
    items.checked_mul(128)
}

fn shallow_words<T>() -> usize {
    std::mem::size_of::<T>().div_ceil(std::mem::size_of::<usize>())
}

/// Exact requested capacities for the optional page-shift publication route.
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
    per_entry_bytes: usize,
    projected_intervals: usize,
    evidence_work: usize,
}

impl DiagnosticAllowance {
    fn bytes(&self) -> Option<usize> {
        self.upper_count.checked_mul(self.per_entry_bytes)
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

struct EqualRun<'a, 'source> {
    page_shifted: bool,
    parent: usize,
    region: usize,
    views: &'a [BorrowedGroupView<'source>; 2],
    ranges: [Range<usize>; 2],
    domain_bytes: usize,
    groups: &'a [GroupMaterializationPlan; 2],
}

/// Bounds possible final diagnostic output without generating it twice.
/// A retained projected interval can intersect only one unresolved partition,
/// so original/aligned regions + unresolved partitions + projected intervals
/// bounds every possible fallback emit, including invalidated paired regions.
/// Existing alignment/full-relation construction stays the baseline path;
/// this prepays potential newly required evidence scans and shallow outputs.
fn diagnostic_allowance(
    assessor: &mut Assessor<'_, '_>,
    partitions: [&[ResolutionRange]; 2],
    original: &[UnresolvedRegion],
) -> Option<DiagnosticAllowance> {
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
    for parts in partitions {
        count = count.checked_add(
            parts
                .iter()
                .filter(|part| part.state == ResolutionState::Unresolved)
                .count(),
        )?;
    }
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
    let bytes = count.checked_mul(unit)?;
    if bytes > COARSE_MEMORY_BYTES {
        return None;
    }
    let work =
        count.checked_mul(evidence_work.checked_add(shallow_words::<UnresolvedRegion>())?)?;
    if !charge_work(&mut assessor.remaining_work, work) {
        return None;
    }
    Some(DiagnosticAllowance {
        upper_count: count,
        per_entry_bytes: unit,
        projected_intervals: projected,
        evidence_work,
    })
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
        let Some(source_bytes) =
            literal_source_capacity_bound(self.sides, &mut self.remaining_work)
        else {
            return Ok(());
        };
        let Some(diagnostics) =
            diagnostic_allowance(self, [&*partitions[0], &*partitions[1]], original)
        else {
            return Ok(());
        };
        let mut resources = TailResources {
            source_bytes,
            retained_children: 0,
            group_peak_bytes: 0,
            diagnostics,
        };
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
                            fresh = match semantic::mandatory_match_analysis_with_memory_limit(
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
            if !charge_work(&mut self.remaining_work, partitions[side].len()) {
                return Ok(false);
            }
            if !partitions[side].iter().any(|part| {
                part.block == spans[side].blocks[0]
                    && part.state == ResolutionState::Unresolved
                    && part.comparable_range.start <= intervals[side].start
                    && intervals[side].end <= part.comparable_range.end
            }) {
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
                if !charge_work(
                    &mut self.remaining_work,
                    span.blocks.len().saturating_mul(usize::BITS as usize + 8),
                ) {
                    return Ok(false);
                }
                let Some(other) = coarse_project(self.sides[side], span) else {
                    return Ok(false);
                };
                if other.iter().any(|other| {
                    selected.block_index == other.block_index
                        && selected.start < other.end
                        && other.start < selected.end
                }) {
                    return Ok(false);
                }
            }
        }
        let assumptions = self.records[parent_index]
            .assumptions
            .len()
            .saturating_add(if page_shifted { 2 } else { 1 });
        // Fixed logical metadata allowance: three capacity/limit/target rows
        // and checked transient-byte/copy sums. Only this new route pays it.
        if page_shifted && !charge_work(&mut self.remaining_work, 128) {
            return Ok(false);
        }
        let Some(child_bytes) = assumptions
            .checked_mul(std::mem::size_of::<ComparisonAssumption>())
            .and_then(|bytes| bytes.checked_add(ids.checked_mul(std::mem::size_of::<BlockId>())?))
        else {
            return Ok(false);
        };
        let Some(mut next_retained_children) = retained_children.checked_add(child_bytes) else {
            return Ok(false);
        };
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
        if new_bytes > COARSE_MEMORY_BYTES {
            return Ok(false);
        }
        let mut growth = if page_shifted {
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
            Some(plan)
        } else {
            None
        };
        let commit_work = if let Some(plan) = growth {
            partitions[0]
                .len()
                .saturating_add(partitions[1].len())
                .saturating_mul(shallow_words::<ResolutionRange>().saturating_add(2))
                .saturating_add(plan.copy_work)
                .saturating_add(regions.len().saturating_mul(usize::BITS as usize + 8))
                .saturating_add(assumptions)
                .saturating_add(8)
                // Every new header is written, even when backing arrays stay.
                .saturating_add(shallow_words::<RelationAssessment>())
                .saturating_add(2usize.saturating_mul(shallow_words::<SourceInterval>()))
                // Post-reserve metadata, actual-capacity ledger and a possible
                // minimum-target downgrade are paid before publication-capacity allocations.
                .saturating_add(128)
        } else {
            partitions[0]
                .len()
                .saturating_add(partitions[1].len())
                .saturating_mul(shallow_words::<ResolutionRange>().saturating_add(2))
                .saturating_add(
                    ownership[0]
                        .accepted
                        .len()
                        .saturating_add(ownership[1].accepted.len())
                        .saturating_mul(shallow_words::<SourceInterval>()),
                )
                .saturating_add(
                    self.records
                        .len()
                        .saturating_mul(shallow_words::<RelationAssessment>()),
                )
                .saturating_add(regions.len().saturating_mul(usize::BITS as usize + 8))
                .saturating_add(assumptions)
                .saturating_add(8)
        };
        if !charge_work(&mut self.remaining_work, commit_work) {
            return Ok(false);
        }
        let Some(next_old) = equal_partition(
            partitions[0],
            old.blocks[0],
            intervals[0].start..intervals[0].end,
        ) else {
            return Ok(false);
        };
        let Some(next_new) = equal_partition(
            partitions[1],
            new.blocks[0],
            intervals[1].start..intervals[1].end,
        ) else {
            return Ok(false);
        };
        // Splitting one unresolved range creates at most one additional
        // whole-block materialization per side. Its nested copies and final
        // diagnostic scans/capacity are approved before any source verdict.
        let mut extra = 0usize;
        let mut regeneration_work = 0usize;
        for (side, next) in [&next_old, &next_new].into_iter().enumerate() {
            let before = partitions[side]
                .iter()
                .filter(|p| p.state == ResolutionState::Unresolved)
                .count();
            let after = next
                .iter()
                .filter(|p| p.state == ResolutionState::Unresolved)
                .count();
            let additional = after.saturating_sub(before);
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
        if let Some(plan) = &mut growth {
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
            .saturating_add(
                extra.saturating_mul(
                    resources
                        .diagnostics
                        .evidence_work
                        .saturating_add(shallow_words::<UnresolvedRegion>()),
                ),
            )
            .saturating_add(2usize.saturating_mul(resources.diagnostics.projected_intervals))
            .saturating_add(partitions[0].len().saturating_add(partitions[1].len()));
        if !charge_work(&mut self.remaining_work, regeneration_work) {
            return Ok(false);
        }
        let mut child_assumptions = Vec::new();
        if let Some(plan) = growth {
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
        } else if child_assumptions.try_reserve_exact(assumptions).is_err()
            || self.records.try_reserve_exact(1).is_err()
            || ownership.iter_mut().any(|owner| {
                owner.accepted.len() >= self.options.max_assessment_ranges
                    || owner.accepted.try_reserve_exact(1).is_err()
            })
        {
            return Ok(false);
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
