//! Bounded, behavior-neutral section-pairing diagnostics.

use std::collections::HashMap;

use crate::{
    alignment::{Alignment, AlignmentKind},
    layout::{BlockRole, TrustedRunId, TrustedRunInterval},
    normalize::{BlockText, ComparableToken},
};

use super::super::{SentenceRecoveryInput, Side};

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
    paragraphs: Vec<usize>,
    strong_paragraphs: Vec<usize>,
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
}

#[derive(Clone, Copy)]
enum ParentRelation {
    Consistent,
    Changed,
    Unknown,
}

/// Diagnoses conservative Section/Paragraph pairing without changing comparison output.
///
/// Any resource or input failure discards all partial counters.
pub(in crate::diff) fn analyze_section_pairing_shadow(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: SentenceRecoveryInput<'_>,
    limits: SectionPairingLimits,
) -> SectionPairingMetrics {
    match analyze(sides, alignment, recovery, limits) {
        Ok(metrics) => SectionPairingMetrics {
            complete: true,
            ..metrics
        },
        Err(reason) => SectionPairingMetrics {
            complete: false,
            stop_reason: Some(reason),
            ..SectionPairingMetrics::default()
        },
    }
}

fn analyze(
    sides: [&Side<'_>; 2],
    alignment: &Alignment,
    recovery: SentenceRecoveryInput<'_>,
    limits: SectionPairingLimits,
) -> Result<SectionPairingMetrics, SectionPairingStopReason> {
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
        });
    }

    classify_pair_topology(&pairs, &structures, &mut metrics)?;
    let old_to_new = pair_map(&pairs, structures[0].sections.len(), false)?;
    let new_to_old = pair_map(&pairs, structures[1].sections.len(), true)?;
    let mut gap_context = GapAnalysisContext {
        structures: &structures,
        sides,
        alignment,
        memberships: &memberships,
        limits,
        paragraph_work: ParagraphWork::default(),
    };
    for pair in &pairs {
        let relation = parent_relation(pair, &structures, &old_to_new, &new_to_old);
        record_parent_relation(&mut metrics, pair.strong, relation)?;
        analyze_paragraph_gaps(*pair, &mut gap_context, &mut metrics)?;
    }
    metrics.paragraph_pair_visits_attempted = gap_context.paragraph_work.pair_visits_attempted;
    metrics.paragraph_pair_visits_examined = gap_context.paragraph_work.pair_visits_examined;
    metrics.paragraph_token_comparisons_attempted =
        gap_context.paragraph_work.comparisons_attempted;
    metrics.paragraph_token_comparisons_examined = gap_context.paragraph_work.comparisons_examined;
    Ok(metrics)
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
            sections[parent].paragraphs.push(block.block_index);
        }
        if let Some(parent) = strong_parent {
            strong_paragraph_memberships = checked_inc(strong_paragraph_memberships)?;
            sections[parent]
                .strong_paragraphs
                .try_reserve(1)
                .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
            sections[parent].strong_paragraphs.push(block.block_index);
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
) -> ParentRelation {
    let old = &structures[0].sections[pair.old];
    let new = &structures[1].sections[pair.new];
    let (Some(old_parent), Some(new_parent)) = (if pair.strong {
        (old.strong_parent, new.strong_parent)
    } else {
        (old.parent, new.parent)
    }) else {
        return match (old.parent, new.parent) {
            (None, None) => ParentRelation::Consistent,
            _ => ParentRelation::Unknown,
        };
    };
    let old_mapping = old_to_new.get(old_parent).copied().flatten();
    let new_mapping = new_to_old.get(new_parent).copied().flatten();
    if pair.strong {
        match (old_mapping, new_mapping) {
            (Some((mapped_new, true)), Some((mapped_old, true)))
                if mapped_new == new_parent && mapped_old == old_parent =>
            {
                ParentRelation::Consistent
            }
            (Some((mapped_new, true)), Some((mapped_old, true)))
                if old_to_new.get(mapped_old).copied().flatten() == Some((new_parent, true))
                    && new_to_old.get(mapped_new).copied().flatten()
                        == Some((old_parent, true)) =>
            {
                ParentRelation::Changed
            }
            _ => ParentRelation::Unknown,
        }
    } else {
        match old_mapping {
            Some((mapped, _)) if mapped == new_parent => ParentRelation::Consistent,
            Some(_) => ParentRelation::Changed,
            None => ParentRelation::Unknown,
        }
    }
}

fn record_parent_relation(
    metrics: &mut SectionPairingMetrics,
    strong: bool,
    relation: ParentRelation,
) -> Result<(), SectionPairingStopReason> {
    let target = match (strong, relation) {
        (true, ParentRelation::Consistent) => &mut metrics.parent_consistent_pairs,
        (true, ParentRelation::Changed) => &mut metrics.parent_changed_pairs,
        (true, ParentRelation::Unknown) => &mut metrics.parent_unknown_pairs,
        (false, ParentRelation::Consistent) => &mut metrics.number_only_parent_consistent,
        (false, ParentRelation::Changed) => &mut metrics.number_only_parent_changed,
        (false, ParentRelation::Unknown) => &mut metrics.number_only_parent_unknown,
    };
    *target = checked_inc(*target)?;
    Ok(())
}

fn classify_pair_topology(
    pairs: &[SectionPair],
    structures: &[SideStructure; 2],
    metrics: &mut SectionPairingMetrics,
) -> Result<(), SectionPairingStopReason> {
    let mut selected = Vec::new();
    selected
        .try_reserve(pairs.len())
        .map_err(|_| SectionPairingStopReason::AllocationFailure)?;
    selected.extend(pairs.iter().filter(|pair| pair.strong).copied());
    selected.sort_by_key(|pair| {
        let old = &structures[0].sections[pair.old];
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
    for pair in &selected {
        let old_run = structures[0].sections[pair.old].run_id;
        let new_run = structures[1].sections[pair.new].run_id;
        update_run_partner(&mut old_partners, old_run, new_run);
        update_run_partner(&mut new_partners, new_run, old_run);
    }

    let mut start = 0usize;
    while start < selected.len() {
        let pair = selected[start];
        let old_run = structures[0].sections[pair.old].run_id;
        let new_run = structures[1].sections[pair.new].run_id;
        let reciprocal = old_partners.get(&old_run) == Some(&Some(new_run))
            && new_partners.get(&new_run) == Some(&Some(old_run));
        let mut end = start + 1;
        while end < selected.len()
            && structures[0].sections[selected[end].old].run_id == old_run
            && structures[1].sections[selected[end].new].run_id == new_run
        {
            end += 1;
        }
        if !reciprocal || end - start < 2 {
            metrics.topology_unknown_pairs =
                checked_add(metrics.topology_unknown_pairs, end - start)?;
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
        for (index, pair) in group.iter().enumerate().rev() {
            minimum = minimum.min(structures[1].sections[pair.new].ordinal_start);
            suffix_min[index] = minimum;
        }
        let mut prefix_max = 0usize;
        for (index, pair) in group.iter().enumerate() {
            let ordinal = structures[1].sections[pair.new].ordinal_start;
            let crossing = (index > 0 && prefix_max > ordinal)
                || (index + 1 < group.len() && suffix_min[index + 1] < ordinal);
            if crossing {
                metrics.crossing_pairs = checked_inc(metrics.crossing_pairs)?;
            } else {
                metrics.monotone_pairs = checked_inc(metrics.monotone_pairs)?;
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
}

#[derive(Default)]
struct ParagraphWork {
    pair_visits_attempted: usize,
    pair_visits_examined: usize,
    comparisons_attempted: usize,
    comparisons_examined: usize,
}

fn analyze_paragraph_gaps(
    pair: SectionPair,
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
        let old_tokens = &context.sides[0].canonical[*old_block];
        let mut old_occurrences = 0usize;
        for block in old {
            if exact_tokens(
                &context.sides[0].canonical[*block],
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
                &context.sides[1].canonical[*block],
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
            && context.sides[0].canonical[old[old_start]]
                != context.sides[1].canonical[new[new_start]]
        {
            let same_unresolved = context.memberships[0][old[old_start]]
                .zip(context.memberships[1][new[new_start]])
                .is_some_and(|(old_span, new_span)| {
                    old_span == new_span
                        && context.alignment.spans[old_span].kind == AlignmentKind::Unresolved
                });
            record_changed_one_to_one(metrics, pair.strong, same_unresolved)?;
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
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
            ]),
            input(&old_intervals, &new_intervals),
            limits(),
        );

        assert!(result.complete);
        assert_eq!(result.exact_heading_pairs, 1);
        assert_eq!(result.strong_heading_pairs, 1);
        assert_eq!(result.changed_one_to_one_gaps, 1);
        assert_eq!(result.changed_one_to_one_same_unresolved_span, 1);
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
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![span(AlignmentKind::Match, &[1, 2], &[11])]),
            input(&old_intervals, &new_intervals),
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
                limits,
            );
            assert_eq!(
                result,
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
        let result = analyze_section_pairing_shadow(
            [&side(&old), &side(&new)],
            &alignment(vec![
                span(AlignmentKind::Match, &[1], &[11]),
                span(AlignmentKind::Unresolved, &[2], &[12]),
            ]),
            input(&old_intervals, &new_intervals),
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
                    paragraphs: Vec::new(),
                    strong_paragraphs: Vec::new(),
                })
                .collect(),
            paragraph_count: 0,
            strong_paragraph_memberships: 0,
            number_only_paragraph_memberships: 0,
        };
        let structures = [structure(false), structure(true)];
        let pairs = [
            SectionPair {
                old: 0,
                new: 1,
                strong: true,
            },
            SectionPair {
                old: 1,
                new: 0,
                strong: true,
            },
            SectionPair {
                old: 2,
                new: 2,
                strong: true,
            },
        ];
        let mut metrics = SectionPairingMetrics::default();

        classify_pair_topology(&pairs, &structures, &mut metrics).expect("topology fits");

        assert_eq!(metrics.crossing_pairs, 2);
        assert_eq!(metrics.monotone_pairs, 1);
        assert_eq!(metrics.topology_unknown_pairs, 0);
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
        let pairs = [
            SectionPair {
                old: 0,
                new: 0,
                strong: true,
            },
            SectionPair {
                old: 1,
                new: 1,
                strong: true,
            },
        ];
        let mut metrics = SectionPairingMetrics::default();

        classify_pair_topology(&pairs, &structures, &mut metrics).expect("topology fits");

        assert_eq!(metrics.monotone_pairs, 0);
        assert_eq!(metrics.crossing_pairs, 0);
        assert_eq!(metrics.topology_unknown_pairs, 2);
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
            },
            SectionPair {
                old: 1,
                new: 1,
                strong: true,
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
            ParentRelation::Unknown
        ));
    }
}
