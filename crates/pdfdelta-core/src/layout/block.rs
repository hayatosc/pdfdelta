use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{
    Error, Result,
    model::{DecodedText, Document, FontId, Glyph, GlyphId, PageId, Rect, Vec2, index_glyphs},
    validate::{validate_non_negative, validate_unit_interval},
};

use super::{
    Line, LineId,
    geometry::{
        directions_are_compatible, interval_gap, interval_overlap_ratio, is_horizontal,
        length_squared, median, normalize, perpendicular, projected_extent, projected_interval,
    },
};

const WEIGHT_SUM_TOLERANCE: f64 = 1.0e-9;
const GEOMETRY_TOLERANCE: f64 = 1.0e-9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockRole {
    Body,
    RepeatedHeader,
    RepeatedFooter,
}

impl BlockRole {
    /// Returns whether blocks with these roles may participate in one alignment match.
    pub fn is_alignment_compatible(self, other: Self) -> bool {
        self == other
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub lines: Vec<LineId>,
    pub role: BlockRole,
}

/// Identifies a trusted line run within one reconstructed document side.
///
/// Numeric ID order is deterministic but does not establish reading order
/// between distinct runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TrustedRunId(pub u64);

/// Identifies a contiguous half-open ordinal interval within one trusted line run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrustedRunInterval {
    pub(crate) run_id: TrustedRunId,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) struct BlockReconstruction {
    pub blocks: Vec<Block>,
    pub issues: Vec<LayoutIssue>,
    /// Parallel to `blocks`; mixed, discontinuous, or untrusted blocks have no interval.
    pub trusted_run_intervals: Vec<Option<TrustedRunInterval>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LayoutIssue {
    UnknownReadingOrder { page: PageId, line_ids: Vec<LineId> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockOptions {
    pub vertical_proximity_weight: f64,
    pub horizontal_overlap_weight: f64,
    pub indent_similarity_weight: f64,
    pub font_continuity_weight: f64,
    pub min_join_score: f64,
    pub max_vertical_gap_height_ratio: f64,
    pub max_indent_height_ratio: f64,
    pub min_horizontal_overlap_ratio: f64,
    pub min_font_similarity: f64,
    pub max_cross_page_indent_height_ratio: f64,
    pub min_cross_page_horizontal_overlap_ratio: f64,
    pub min_cross_page_font_similarity: f64,
    pub max_cross_page_cadence_difference: f64,
    pub repeated_edge_line_limit: usize,
    pub repeated_min_pages: usize,
    pub min_repeated_margin_font_similarity: f64,
}

impl Default for BlockOptions {
    fn default() -> Self {
        Self {
            vertical_proximity_weight: 0.25,
            horizontal_overlap_weight: 0.25,
            indent_similarity_weight: 0.25,
            font_continuity_weight: 0.25,
            min_join_score: 0.75,
            max_vertical_gap_height_ratio: 0.8,
            max_indent_height_ratio: 0.5,
            min_horizontal_overlap_ratio: 0.8,
            min_font_similarity: 0.8,
            max_cross_page_indent_height_ratio: 0.25,
            min_cross_page_horizontal_overlap_ratio: 0.9,
            min_cross_page_font_similarity: 0.9,
            max_cross_page_cadence_difference: 0.1,
            repeated_edge_line_limit: 1,
            repeated_min_pages: 3,
            min_repeated_margin_font_similarity: 0.95,
        }
    }
}

pub(crate) fn validate_block_options(options: BlockOptions) -> Result<()> {
    let weights = [
        (
            "vertical_proximity_weight",
            options.vertical_proximity_weight,
        ),
        (
            "horizontal_overlap_weight",
            options.horizontal_overlap_weight,
        ),
        ("indent_similarity_weight", options.indent_similarity_weight),
        ("font_continuity_weight", options.font_continuity_weight),
    ];
    for (name, value) in weights {
        validate_non_negative(name, value)?;
    }
    let weight_sum = weights.iter().map(|(_, value)| value).sum::<f64>();
    if (weight_sum - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
        return Err(Error::InvalidConfiguration(
            "block score weights must sum to 1".to_owned(),
        ));
    }

    validate_unit_interval("min_join_score", options.min_join_score)?;
    validate_non_negative(
        "max_vertical_gap_height_ratio",
        options.max_vertical_gap_height_ratio,
    )?;
    validate_non_negative("max_indent_height_ratio", options.max_indent_height_ratio)?;
    validate_unit_interval(
        "min_horizontal_overlap_ratio",
        options.min_horizontal_overlap_ratio,
    )?;
    validate_unit_interval("min_font_similarity", options.min_font_similarity)?;
    validate_non_negative(
        "max_cross_page_indent_height_ratio",
        options.max_cross_page_indent_height_ratio,
    )?;
    validate_unit_interval(
        "min_cross_page_horizontal_overlap_ratio",
        options.min_cross_page_horizontal_overlap_ratio,
    )?;
    validate_unit_interval(
        "min_cross_page_font_similarity",
        options.min_cross_page_font_similarity,
    )?;
    validate_non_negative(
        "max_cross_page_cadence_difference",
        options.max_cross_page_cadence_difference,
    )?;
    // deliberate: repeated_edge_line_limit == 0 disables repeated-edge role detection.
    if options.repeated_min_pages < 2 {
        return Err(Error::InvalidConfiguration(
            "repeated_min_pages must be at least 2".to_owned(),
        ));
    }
    validate_unit_interval(
        "min_repeated_margin_font_similarity",
        options.min_repeated_margin_font_similarity,
    )?;
    Ok(())
}

/// Reconstructs best-effort structural blocks while preserving every input
/// line and glyph assignment.
///
/// Reading-order uncertainty is intentionally not exposed by this structural
/// wrapper. Use the top-level comparison pipeline when uncertain layout must
/// be isolated from safely comparable content.
///
/// # Errors
///
/// Returns an error when options are invalid, evidence is inconsistent, or a
/// configured reconstruction resource limit is exceeded.
pub fn reconstruct_blocks(
    document: &Document<Glyph>,
    lines: &[Line],
    options: BlockOptions,
) -> Result<Vec<Block>> {
    reconstruct_blocks_with_issues(document, lines, options).map(|result| result.blocks)
}

pub(crate) fn reconstruct_blocks_with_issues(
    document: &Document<Glyph>,
    lines: &[Line],
    options: BlockOptions,
) -> Result<BlockReconstruction> {
    validate_block_options(options)?;
    let glyphs = index_glyphs(document)?;
    let mut line_ids = HashSet::with_capacity(lines.len());
    let mut assigned_glyphs = HashSet::with_capacity(glyphs.len());
    let mut stats = Vec::with_capacity(lines.len());

    for line in lines {
        if !line_ids.insert(line.id) {
            return Err(Error::Unresolved(format!(
                "duplicate line id {}",
                line.id.0
            )));
        }
        stats.push(LineStats::new(line, &glyphs, &mut assigned_glyphs)?);
    }
    if assigned_glyphs.len() != glyphs.len() {
        return Err(Error::Unresolved(
            "one or more document glyphs are not assigned to a line".to_owned(),
        ));
    }

    let mut stats_by_line_id = HashMap::with_capacity(stats.len());
    for line_stats in stats {
        stats_by_line_id.insert(line_stats.line.id, line_stats);
    }

    let mut page_lines_map = BTreeMap::<u32, Vec<&Line>>::new();
    for line in lines {
        page_lines_map.entry(line.page.0).or_default().push(line);
    }

    let mut ordered_stats = Vec::with_capacity(lines.len());
    let mut partial_uncertain_line_ids = HashSet::new();
    let mut trusted_run_positions_by_line_id = HashMap::new();
    let mut next_trusted_run_id = 0;
    let mut issues = Vec::new();
    for (page_num, page_lines) in page_lines_map {
        let page = PageId(page_num);
        let page_vector_lines = document
            .vector_lines()
            .iter()
            .filter(|line| line.page == page)
            .collect::<Vec<_>>();
        let partition = super::region::partition_regions_from_refs(
            page,
            &page_lines,
            &page_vector_lines,
            super::region::RegionOptions::default(),
        )?;
        assign_trusted_run_positions(
            &mut trusted_run_positions_by_line_id,
            &partition.trusted_runs,
            &mut next_trusted_run_id,
        )?;
        // Partial uncertainty remains deterministic evidence serialization, not proven order.
        // Barriers only keep already-classified uncertain lines from contaminating trusted blocks.
        if !partition.uncertain_line_ids.is_empty()
            && partition.uncertain_line_ids.len() < page_lines.len()
        {
            partial_uncertain_line_ids.extend(partition.uncertain_line_ids.iter().copied());
        }
        let graph = partition.graph;
        match &graph.reading_order {
            super::region::ReadingOrder::KnownLines(line_ids) => {
                append_region_order(
                    &mut stats_by_line_id,
                    &mut ordered_stats,
                    line_ids.iter().copied(),
                )?;
            }
            reading_order => {
                if matches!(reading_order, super::region::ReadingOrder::Unknown) {
                    issues.push(LayoutIssue::UnknownReadingOrder {
                        page,
                        line_ids: partition.uncertain_line_ids,
                    });
                }
                for region in &graph.regions {
                    append_region_order(
                        &mut stats_by_line_id,
                        &mut ordered_stats,
                        region.line_ids.iter().copied(),
                    )?;
                }
            }
        }
    }
    validate_region_order(&stats_by_line_id, &ordered_stats, lines.len())?;
    let partial_uncertain_indices = ordered_stats
        .iter()
        .enumerate()
        .filter_map(|(index, stats)| {
            partial_uncertain_line_ids
                .contains(&stats.line.id)
                .then_some(index)
        })
        .collect::<BTreeSet<_>>();
    let stats = ordered_stats;

    let pages = page_groups(&stats);
    let roles = detect_repeated_margins(&stats, &pages, options);
    let body_indices: Vec<_> = roles
        .iter()
        .enumerate()
        .filter_map(|(index, role)| (*role == BlockRole::Body).then_some(index))
        .collect();

    let mut pending = Vec::<PendingBlock>::new();
    for (position, index) in body_indices.iter().copied().enumerate() {
        let joins_previous = position > 0
            && !crosses_partial_uncertainty(
                body_indices[position - 1],
                body_indices[position],
                &partial_uncertain_indices,
            )
            && should_join_body(&stats, &body_indices, position, options)?;
        if joins_previous && let Some(block) = pending.last_mut() {
            block.lines.push(stats[index].line.id);
            continue;
        }
        pending.push(PendingBlock {
            order: index,
            lines: vec![stats[index].line.id],
            role: BlockRole::Body,
        });
    }

    for (index, role) in roles.into_iter().enumerate() {
        if role != BlockRole::Body {
            pending.push(PendingBlock {
                order: index,
                lines: vec![stats[index].line.id],
                role,
            });
        }
    }
    pending.sort_by_key(|block| block.order);

    let blocks = pending
        .into_iter()
        .enumerate()
        .map(|(index, block)| Block {
            id: BlockId(index as u64),
            lines: block.lines,
            role: block.role,
        })
        .collect::<Vec<_>>();
    let trusted_run_intervals =
        block_trusted_run_intervals(&blocks, &trusted_run_positions_by_line_id);
    Ok(BlockReconstruction {
        blocks,
        issues,
        trusted_run_intervals,
    })
}

fn assign_trusted_run_positions(
    run_positions_by_line_id: &mut HashMap<LineId, (TrustedRunId, usize)>,
    trusted_runs: &[super::region::TrustedLineRun],
    next_run_id: &mut u64,
) -> Result<()> {
    for run in trusted_runs {
        if run.line_ids.is_empty() {
            continue;
        }
        let following_run_id = next_run_id
            .checked_add(1)
            .ok_or_else(|| Error::Unresolved("trusted line run id space exhausted".to_owned()))?;
        let mut run_line_ids = HashSet::with_capacity(run.line_ids.len());
        for line_id in &run.line_ids {
            if run_positions_by_line_id.contains_key(line_id) || !run_line_ids.insert(*line_id) {
                return Err(Error::Unresolved(format!(
                    "line {} is assigned to multiple trusted runs",
                    line_id.0
                )));
            }
        }
        let run_id = TrustedRunId(*next_run_id);
        run_positions_by_line_id.extend(
            run.line_ids
                .iter()
                .enumerate()
                .map(|(ordinal, line_id)| (*line_id, (run_id, ordinal))),
        );
        *next_run_id = following_run_id;
    }
    Ok(())
}

fn block_trusted_run_intervals(
    blocks: &[Block],
    run_positions_by_line_id: &HashMap<LineId, (TrustedRunId, usize)>,
) -> Vec<Option<TrustedRunInterval>> {
    blocks
        .iter()
        .map(|block| {
            let &(run_id, start) = run_positions_by_line_id.get(block.lines.first()?)?;
            let contiguous = block.lines.iter().enumerate().all(|(offset, line_id)| {
                start.checked_add(offset).is_some_and(|ordinal| {
                    run_positions_by_line_id.get(line_id) == Some(&(run_id, ordinal))
                })
            });
            let end = start.checked_add(block.lines.len())?;
            contiguous.then_some(TrustedRunInterval { run_id, start, end })
        })
        .collect()
}

fn crosses_partial_uncertainty(
    previous: usize,
    current: usize,
    uncertain_indices: &BTreeSet<usize>,
) -> bool {
    uncertain_indices.range(previous..=current).next().is_some()
}

fn append_region_order<T>(
    values_by_line_id: &mut HashMap<LineId, T>,
    ordered_values: &mut Vec<T>,
    line_ids: impl IntoIterator<Item = LineId>,
) -> Result<()> {
    for line_id in line_ids {
        let value = values_by_line_id.remove(&line_id).ok_or_else(|| {
            Error::Unresolved(format!(
                "region partition returned duplicate or unknown line id {}",
                line_id.0
            ))
        })?;
        ordered_values.push(value);
    }
    Ok(())
}

fn validate_region_order<T>(
    values_by_line_id: &HashMap<LineId, T>,
    ordered_values: &[T],
    expected_count: usize,
) -> Result<()> {
    if !values_by_line_id.is_empty() || ordered_values.len() != expected_count {
        return Err(Error::Unresolved(format!(
            "region partition preserved {} of {expected_count} lines; {} line ids remain unassigned",
            ordered_values.len(),
            values_by_line_id.len()
        )));
    }
    Ok(())
}

struct PendingBlock {
    order: usize,
    lines: Vec<LineId>,
    role: BlockRole,
}

struct LineStats<'a> {
    line: &'a Line,
    signature: Vec<SignatureToken>,
    direction: Vec2,
    inline_interval: (f64, f64),
    inline_start: f64,
    median_height: f64,
    median_font_size: f64,
    dominant_font: FontId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SignatureToken {
    Text(DecodedText),
    SyntheticSpace,
}

impl Ord for SignatureToken {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => compare_decoded_text(a, b),
            (Self::Text(_), Self::SyntheticSpace) => std::cmp::Ordering::Less,
            (Self::SyntheticSpace, Self::Text(_)) => std::cmp::Ordering::Greater,
            (Self::SyntheticSpace, Self::SyntheticSpace) => std::cmp::Ordering::Equal,
        }
    }
}

impl PartialOrd for SignatureToken {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_decoded_text(a: &DecodedText, b: &DecodedText) -> std::cmp::Ordering {
    match (a, b) {
        (DecodedText::Mapped(s1), DecodedText::Mapped(s2)) => s1.cmp(s2),
        (DecodedText::Mapped(_), DecodedText::Unmapped { .. }) => std::cmp::Ordering::Less,
        (DecodedText::Unmapped { .. }, DecodedText::Mapped(_)) => std::cmp::Ordering::Greater,
        (
            DecodedText::Unmapped {
                font_hash: h1,
                glyph_id: g1,
            },
            DecodedText::Unmapped {
                font_hash: h2,
                glyph_id: g2,
            },
        ) => h1.cmp(h2).then_with(|| g1.cmp(g2)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum MarginEdge {
    Header,
    Footer,
}

impl MarginEdge {
    fn role(self) -> BlockRole {
        match self {
            Self::Header => BlockRole::RepeatedHeader,
            Self::Footer => BlockRole::RepeatedFooter,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct MarginKey {
    edge: MarginEdge,
    ordinal: usize,
    signature: Vec<SignatureToken>,
}

impl<'a> LineStats<'a> {
    fn new(
        line: &'a Line,
        glyph_index: &HashMap<GlyphId, &'a Glyph>,
        assigned_glyphs: &mut HashSet<GlyphId>,
    ) -> Result<Self> {
        validate_line_geometry(line)?;
        if line.glyphs.is_empty() {
            return Err(invalid_line(line, "contains no glyphs"));
        }

        let mut glyphs = Vec::with_capacity(line.glyphs.len());
        for glyph_id in &line.glyphs {
            let Some(glyph) = glyph_index.get(glyph_id).copied() else {
                return Err(invalid_line(
                    line,
                    &format!("references unknown glyph {}", glyph_id.0),
                ));
            };
            if !assigned_glyphs.insert(*glyph_id) {
                return Err(invalid_line(line, &format!("reuses glyph {}", glyph_id.0)));
            }
            validate_line_glyph(line, glyph)?;
            glyphs.push(glyph);
        }
        validate_synthetic_spaces(line)?;
        if glyphs.is_empty() {
            return Err(invalid_line(line, "contains no glyphs"));
        }
        let glyph_bbox = glyph_union(&glyphs);
        if !rect_approximately_equal(line.bbox, glyph_bbox) {
            return Err(invalid_line(
                line,
                "bounding box does not match its glyph union",
            ));
        }

        let direction = normalize(line.direction);
        let inline_interval = projected_interval(line.bbox, direction);
        let inline_extent = projected_extent(line.bbox, direction);
        if !inline_interval.0.is_finite()
            || !inline_interval.1.is_finite()
            || !inline_extent.is_finite()
        {
            return Err(invalid_line(line, "has non-finite projected geometry"));
        }
        let median_height = median(
            glyphs
                .iter()
                .map(|glyph| glyph.bbox.max.y - glyph.bbox.min.y)
                .collect(),
        )
        .expect("validated lines contain at least one glyph");
        let median_font_size = median(glyphs.iter().map(|glyph| glyph.font_size).collect())
            .expect("validated lines contain at least one glyph");
        if !median_height.is_finite() || !median_font_size.is_finite() {
            return Err(invalid_line(line, "has non-finite derived metrics"));
        }
        let dominant_font = dominant_font(&glyphs);
        let signature = line_signature(line, &glyphs);

        Ok(Self {
            line,
            signature,
            direction,
            inline_interval,
            inline_start: inline_interval.0,
            median_height,
            median_font_size,
            dominant_font,
        })
    }
}

fn line_signature(line: &Line, glyphs: &[&Glyph]) -> Vec<SignatureToken> {
    let spaces: HashSet<_> = line
        .synthetic_spaces
        .iter()
        .map(|space| (space.preceding, space.following))
        .collect();
    let mut signature = Vec::with_capacity(glyphs.len() + spaces.len());
    for (index, glyph) in glyphs.iter().enumerate() {
        signature.push(SignatureToken::Text(glyph.text.clone()));
        if let Some(following) = glyphs.get(index + 1)
            && spaces.contains(&(glyph.id, following.id))
        {
            signature.push(SignatureToken::SyntheticSpace);
        }
    }
    signature
}

fn page_groups(stats: &[LineStats<'_>]) -> Vec<Vec<usize>> {
    let mut pages = Vec::<Vec<usize>>::new();
    for (index, stat) in stats.iter().enumerate() {
        if let Some(page) = pages.last_mut()
            && stats[page[0]].line.page == stat.line.page
        {
            page.push(index);
        } else {
            pages.push(vec![index]);
        }
    }
    pages
}

fn detect_repeated_margins(
    stats: &[LineStats<'_>],
    pages: &[Vec<usize>],
    options: BlockOptions,
) -> Vec<BlockRole> {
    let mut roles = vec![BlockRole::Body; stats.len()];
    if options.repeated_edge_line_limit == 0 {
        return roles;
    }

    let mut candidates = BTreeMap::<MarginKey, Vec<usize>>::new();
    for page in pages {
        let horizontal: Vec<_> = page
            .iter()
            .copied()
            .filter(|index| is_horizontal(stats[*index].direction))
            .collect();
        if horizontal.len() <= options.repeated_edge_line_limit.saturating_mul(2) {
            continue;
        }
        for ordinal in 0..options.repeated_edge_line_limit {
            let header = horizontal[ordinal];
            candidates
                .entry(MarginKey {
                    edge: MarginEdge::Header,
                    ordinal,
                    signature: stats[header].signature.clone(),
                })
                .or_default()
                .push(header);

            let footer = horizontal[horizontal.len() - ordinal - 1];
            candidates
                .entry(MarginKey {
                    edge: MarginEdge::Footer,
                    ordinal,
                    signature: stats[footer].signature.clone(),
                })
                .or_default()
                .push(footer);
        }
    }

    for (key, mut remaining) in candidates {
        // Greedily clusters candidates with similar margin fonts using deterministic page ordering.
        while let Some(reference) = remaining.pop() {
            let mut cluster = vec![reference];
            let mut different_style = Vec::new();
            for candidate in remaining {
                if font_similarity(&stats[reference], &stats[candidate])
                    >= options.min_repeated_margin_font_similarity
                {
                    cluster.push(candidate);
                } else {
                    different_style.push(candidate);
                }
            }
            if cluster.len() >= options.repeated_min_pages {
                for index in cluster {
                    roles[index] = key.edge.role();
                }
            }
            remaining = different_style;
        }
    }

    roles
}

fn should_join_body(
    stats: &[LineStats<'_>],
    body_indices: &[usize],
    position: usize,
    options: BlockOptions,
) -> Result<bool> {
    let previous = &stats[body_indices[position - 1]];
    let current = &stats[body_indices[position]];
    if previous.line.page == current.line.page {
        return should_join(previous, current, stats, options);
    }

    if previous.line.page.0.checked_add(1) != Some(current.line.page.0) {
        return Ok(false);
    }

    let Some(previous_neighbor_position) = position.checked_sub(2) else {
        return Ok(false);
    };
    let Some(current_neighbor_index) = body_indices.get(position + 1).copied() else {
        return Ok(false);
    };
    let previous_neighbor = &stats[body_indices[previous_neighbor_position]];
    let current_neighbor = &stats[current_neighbor_index];
    if previous_neighbor.line.page != previous.line.page
        || current_neighbor.line.page != current.line.page
        || !should_join(previous_neighbor, previous, stats, options)?
        || !should_join(current, current_neighbor, stats, options)?
    {
        return Ok(false);
    }

    should_join_across_page(
        previous_neighbor,
        previous,
        current,
        current_neighbor,
        options,
    )
}

fn should_join_across_page(
    previous_neighbor: &LineStats<'_>,
    previous: &LineStats<'_>,
    current: &LineStats<'_>,
    current_neighbor: &LineStats<'_>,
    options: BlockOptions,
) -> Result<bool> {
    if !is_horizontal(previous.direction)
        || !is_horizontal(current.direction)
        || !directions_are_compatible(previous.direction, current.direction)
    {
        return Ok(false);
    }

    let height = (previous.median_height / 2.0 + current.median_height / 2.0).max(f64::EPSILON);
    let horizontal_overlap =
        interval_overlap_ratio(previous.inline_interval, current.inline_interval);
    let indent_ratio = (previous.inline_start - current.inline_start).abs() / height;
    let font_similarity = font_similarity(previous, current);
    let previous_cadence = normalized_line_gap(previous_neighbor, previous);
    let current_cadence = normalized_line_gap(current, current_neighbor);
    let cadence_difference = (previous_cadence - current_cadence).abs();
    let components = [
        height,
        horizontal_overlap,
        indent_ratio,
        font_similarity,
        previous_cadence,
        current_cadence,
        cadence_difference,
    ];
    if components.iter().any(|value| !value.is_finite()) {
        return Err(Error::Unresolved(format!(
            "lines {} and {} produce non-finite cross-page metrics",
            previous.line.id.0, current.line.id.0
        )));
    }

    Ok(
        horizontal_overlap >= options.min_cross_page_horizontal_overlap_ratio
            && indent_ratio <= options.max_cross_page_indent_height_ratio
            && font_similarity >= options.min_cross_page_font_similarity
            && cadence_difference <= options.max_cross_page_cadence_difference,
    )
}

fn normalized_line_gap(first: &LineStats<'_>, second: &LineStats<'_>) -> f64 {
    let height = (first.median_height / 2.0 + second.median_height / 2.0).max(f64::EPSILON);
    interval_gap(
        (first.line.bbox.min.y, first.line.bbox.max.y),
        (second.line.bbox.min.y, second.line.bbox.max.y),
    ) / height
}

// Relative geometry tolerances for row-band alignment:
// - vertical_overlap >= 0.4: line bounding boxes must overlap by at least 40% of the shorter line height.
// - baseline_dist <= 0.35 * min_height: baseline vertical distance tolerance for mixed-size fonts in the same row.
// - horizontal_gap >= 0.2 * min_font_size: minimum relative gutter (0.2em) separating distinct columns.
// - interval_overlap_ratio > 0.3: horizontal overlap threshold for clustering lines into the same physical column.
// Any retuning of these geometric tolerances must be justified by fixture or benchmark evidence.
fn is_same_row_band(target: &LineStats<'_>, candidate: &LineStats<'_>) -> bool {
    if candidate.line.page != target.line.page {
        return false;
    }
    if !is_horizontal(candidate.direction)
        || !directions_are_compatible(target.direction, candidate.direction)
    {
        return false;
    }
    let vertical_overlap = interval_overlap_ratio(
        (target.line.bbox.min.y, target.line.bbox.max.y),
        (candidate.line.bbox.min.y, candidate.line.bbox.max.y),
    );
    let baseline_dist = (target.line.baseline.y - candidate.line.baseline.y).abs();
    let min_height = target.median_height.min(candidate.median_height);
    vertical_overlap >= 0.4 || (min_height > 0.0 && baseline_dist <= 0.35 * min_height)
}

fn find_aligned_peers(target: &LineStats<'_>, stats: &[LineStats<'_>]) -> Vec<usize> {
    let mut peers = Vec::new();
    for (index, candidate) in stats.iter().enumerate() {
        if candidate.line.id == target.line.id || !is_same_row_band(target, candidate) {
            continue;
        }
        let horizontal_gap = interval_gap(target.inline_interval, candidate.inline_interval);
        let min_font_size = target.median_font_size.min(candidate.median_font_size);
        if horizontal_gap >= 0.2 * min_font_size {
            peers.push(index);
        }
    }
    peers
}

fn max_column_width(target: &LineStats<'_>, stats: &[LineStats<'_>]) -> f64 {
    let mut max_width = target.line.bbox.max.x - target.line.bbox.min.x;
    for candidate in stats {
        if !is_same_row_band(target, candidate) {
            continue;
        }
        let overlap = interval_overlap_ratio(target.inline_interval, candidate.inline_interval);
        if overlap > 0.3 {
            let width = candidate.line.bbox.max.x - candidate.line.bbox.min.x;
            max_width = max_width.max(width);
        }
    }
    max_width
}

fn crosses_grid_cell_boundary(
    previous: &LineStats<'_>,
    current: &LineStats<'_>,
    stats: &[LineStats<'_>],
) -> bool {
    let prev_peers = find_aligned_peers(previous, stats);
    let curr_peers = find_aligned_peers(current, stats);

    // If previous and current share an aligned peer line, they are lines in the
    // same multi-line cell of that row band.
    if !prev_peers.is_empty()
        && !curr_peers.is_empty()
        && prev_peers.iter().any(|p| curr_peers.contains(p))
    {
        return false;
    }

    let min_font_size = previous.median_font_size.min(current.median_font_size);
    // Relative prose-width ceiling: 12.0em (12.0 * min_font_size) distinguishes
    // short tabular/label cells (typically <= 6-8em) from wrap-capable prose columns
    // (typically >= 15-20em). Documented fixture ceiling is backed by block_fixture
    // test cases (narrow cells <= 8em, prose columns >= 15em). Any future retuning
    // for narrow multi-column prose (e.g. 4-column newspaper layouts below 12em)
    // must be justified by benchmark fixture evidence of intermediate column widths.
    let min_prose_width = 12.0 * min_font_size;

    let col_width_prev = max_column_width(previous, stats);
    let col_width_curr = max_column_width(current, stats);
    let candidate_col_width = col_width_prev.max(col_width_curr);

    // If the candidate column is narrow (not wide enough for prose), then being
    // aligned with distinct peer lines indicates separate grid/table/form cells.
    if candidate_col_width < min_prose_width {
        return !prev_peers.is_empty() && !curr_peers.is_empty();
    }

    // If the candidate column is wide enough for prose:
    // Check if the aligned peers form a multi-column table grid across rows.
    // Case 1: Multi-column table (3+ columns with 2+ distinct narrow peer columns).
    struct PeerColumn {
        interval: (f64, f64),
        max_width: f64,
    }

    let mut peer_columns: Vec<PeerColumn> = Vec::new();
    for &idx in prev_peers.iter().chain(curr_peers.iter()) {
        let peer = &stats[idx];
        let width = peer.line.bbox.max.x - peer.line.bbox.min.x;
        let interval = peer.inline_interval;
        if let Some(col) = peer_columns
            .iter_mut()
            .find(|col| interval_overlap_ratio(col.interval, interval) > 0.3)
        {
            col.max_width = col.max_width.max(width);
            col.interval.0 = col.interval.0.min(interval.0);
            col.interval.1 = col.interval.1.max(interval.1);
        } else {
            peer_columns.push(PeerColumn {
                interval,
                max_width: width,
            });
        }
    }

    let narrow_peer_columns = peer_columns
        .iter()
        .filter(|col| col.max_width < min_prose_width)
        .count();

    if narrow_peer_columns >= 2 {
        return true;
    }

    // Two-column table with a wide description column and one narrow peer column.
    if !curr_peers.is_empty() {
        let has_curr_narrow_peer = curr_peers
            .iter()
            .any(|&idx| stats[idx].line.bbox.max.x - stats[idx].line.bbox.min.x < min_prose_width);
        if has_curr_narrow_peer {
            for candidate in stats {
                if candidate.line.page != previous.line.page
                    || candidate.line.bbox.min.y <= previous.line.bbox.min.y
                {
                    continue;
                }
                // Preceding line in the same candidate column
                if interval_overlap_ratio(previous.inline_interval, candidate.inline_interval) > 0.5
                {
                    let cand_peers = find_aligned_peers(candidate, stats);
                    if prev_peers.is_empty() {
                        // Pattern 1 (Top-aligned price): prev_peers is empty, cand line has a narrow peer in the same peer column.
                        let has_top_narrow_peer = cand_peers.iter().any(|&cand_p_idx| {
                            let cand_peer = &stats[cand_p_idx];
                            let is_narrow = cand_peer.line.bbox.max.x - cand_peer.line.bbox.min.x
                                < min_prose_width;
                            let in_same_col = curr_peers.iter().any(|&curr_p_idx| {
                                interval_overlap_ratio(
                                    cand_peer.inline_interval,
                                    stats[curr_p_idx].inline_interval,
                                ) > 0.3
                            });
                            let is_different_line = curr_peers
                                .iter()
                                .all(|&curr_p_idx| stats[curr_p_idx].line.id != cand_peer.line.id);
                            is_narrow && in_same_col && is_different_line
                        });
                        if has_top_narrow_peer {
                            return true;
                        }
                    } else {
                        // prev_peers is non-empty (e.g. contains P_prev):
                        let prev_narrow_peer = prev_peers.iter().find(|&&p_idx| {
                            let p = &stats[p_idx];
                            let is_narrow = p.line.bbox.max.x - p.line.bbox.min.x < min_prose_width;
                            let in_same_col = curr_peers.iter().any(|&curr_p_idx| {
                                interval_overlap_ratio(
                                    p.inline_interval,
                                    stats[curr_p_idx].inline_interval,
                                ) > 0.3
                            });
                            let is_different_line = curr_peers
                                .iter()
                                .all(|&curr_p_idx| stats[curr_p_idx].line.id != p.line.id);
                            is_narrow && in_same_col && is_different_line
                        });
                        if let Some(&prev_p_idx) = prev_narrow_peer {
                            let prev_peer_stat = &stats[prev_p_idx];
                            // Pattern 2 (Bottom-aligned price): cand line in the preceding cell has NO peer in this peer column.
                            let cand_has_no_peer = cand_peers.iter().all(|&cand_p_idx| {
                                interval_overlap_ratio(
                                    stats[cand_p_idx].inline_interval,
                                    prev_peer_stat.inline_interval,
                                ) <= 0.3
                            });
                            // Pattern 3 (Tall/spanning price): cand line ALSO shares the same tall/spanning peer.
                            let cand_shares_spanning_peer = cand_peers.contains(&prev_p_idx);

                            if cand_has_no_peer || cand_shares_spanning_peer {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    false
}

fn should_join(
    previous: &LineStats<'_>,
    current: &LineStats<'_>,
    stats: &[LineStats<'_>],
    options: BlockOptions,
) -> Result<bool> {
    if previous.line.page != current.line.page
        || !is_horizontal(previous.direction)
        || !is_horizontal(current.direction)
        || !directions_are_compatible(previous.direction, current.direction)
    {
        return Ok(false);
    }

    if crosses_grid_cell_boundary(previous, current, stats) {
        return Ok(false);
    }

    let height = (previous.median_height / 2.0 + current.median_height / 2.0).max(f64::EPSILON);
    let vertical_gap = interval_gap(
        (previous.line.bbox.min.y, previous.line.bbox.max.y),
        (current.line.bbox.min.y, current.line.bbox.max.y),
    );
    let vertical_gap_ratio = vertical_gap / height;
    let horizontal_overlap =
        interval_overlap_ratio(previous.inline_interval, current.inline_interval);
    let indent_ratio = (previous.inline_start - current.inline_start).abs() / height;
    let font_similarity = font_similarity(previous, current);
    let components = [
        height,
        vertical_gap,
        vertical_gap_ratio,
        horizontal_overlap,
        indent_ratio,
        font_similarity,
    ];
    if components.iter().any(|value| !value.is_finite()) {
        return Err(Error::Unresolved(format!(
            "lines {} and {} produce non-finite block metrics",
            previous.line.id.0, current.line.id.0
        )));
    }

    if vertical_gap_ratio > options.max_vertical_gap_height_ratio
        || indent_ratio > options.max_indent_height_ratio
        || horizontal_overlap < options.min_horizontal_overlap_ratio
        || font_similarity < options.min_font_similarity
    {
        return Ok(false);
    }

    let vertical_proximity = closeness(vertical_gap_ratio, options.max_vertical_gap_height_ratio);
    let indent_similarity = closeness(indent_ratio, options.max_indent_height_ratio);
    let score = options.vertical_proximity_weight * vertical_proximity
        + options.horizontal_overlap_weight * horizontal_overlap
        + options.indent_similarity_weight * indent_similarity
        + options.font_continuity_weight * font_similarity;
    if !score.is_finite() {
        return Err(Error::Unresolved(format!(
            "lines {} and {} produce a non-finite block score",
            previous.line.id.0, current.line.id.0
        )));
    }
    Ok(score >= options.min_join_score)
}

fn validate_line_geometry(line: &Line) -> Result<()> {
    let values = [
        line.bbox.min.x,
        line.bbox.min.y,
        line.bbox.max.x,
        line.bbox.max.y,
        line.baseline.x,
        line.baseline.y,
        line.direction.x,
        line.direction.y,
    ];
    if values.iter().any(|value| !value.is_finite()) {
        return Err(invalid_line(line, "has non-finite geometry"));
    }
    if line.bbox.min.x > line.bbox.max.x || line.bbox.min.y >= line.bbox.max.y {
        return Err(invalid_line(line, "has an invalid bounding box"));
    }
    if length_squared(line.direction) <= f64::EPSILON {
        return Err(invalid_line(line, "has zero writing direction"));
    }
    Ok(())
}

fn validate_line_glyph(line: &Line, glyph: &Glyph) -> Result<()> {
    if glyph.page != line.page {
        return Err(invalid_line(
            line,
            &format!("contains glyph {} from a different page", glyph.id.0),
        ));
    }
    let geometry = [
        glyph.bbox.min.x,
        glyph.bbox.min.y,
        glyph.bbox.max.x,
        glyph.bbox.max.y,
        glyph.baseline.x,
        glyph.baseline.y,
        glyph.direction.x,
        glyph.direction.y,
    ];
    if geometry.iter().any(|value| !value.is_finite())
        || glyph.bbox.min.x > glyph.bbox.max.x
        || glyph.bbox.min.y >= glyph.bbox.max.y
        || length_squared(glyph.direction) <= f64::EPSILON
    {
        return Err(invalid_line(
            line,
            &format!("contains glyph {} with invalid geometry", glyph.id.0),
        ));
    }
    if !directions_are_compatible(line.direction, glyph.direction) {
        return Err(invalid_line(
            line,
            &format!("contains glyph {} with inconsistent direction", glyph.id.0),
        ));
    }
    if !glyph.font_size.is_finite() || glyph.font_size <= 0.0 {
        return Err(invalid_line(
            line,
            &format!("contains glyph {} with invalid font size", glyph.id.0),
        ));
    }
    if !rect_contains(line.bbox, glyph.bbox) {
        return Err(invalid_line(
            line,
            &format!("bounding box excludes glyph {}", glyph.id.0),
        ));
    }
    let direction = normalize(glyph.direction);
    let inline_extent = projected_extent(glyph.bbox, direction);
    let cross_extent = projected_extent(glyph.bbox, perpendicular(direction));
    if !inline_extent.is_finite() || !cross_extent.is_finite() || cross_extent <= f64::EPSILON {
        return Err(invalid_line(
            line,
            &format!(
                "contains glyph {} with invalid projected geometry",
                glyph.id.0
            ),
        ));
    }
    Ok(())
}

fn validate_synthetic_spaces(line: &Line) -> Result<()> {
    let adjacent: HashSet<_> = line
        .glyphs
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect();
    let mut seen = HashSet::with_capacity(line.synthetic_spaces.len());
    for space in &line.synthetic_spaces {
        let pair = (space.preceding, space.following);
        if !adjacent.contains(&pair) || !seen.insert(pair) {
            return Err(invalid_line(
                line,
                "has a duplicate or non-adjacent synthetic space",
            ));
        }
    }
    Ok(())
}

fn rect_contains(outer: Rect, inner: Rect) -> bool {
    outer.min.x <= inner.min.x + GEOMETRY_TOLERANCE
        && outer.min.y <= inner.min.y + GEOMETRY_TOLERANCE
        && outer.max.x + GEOMETRY_TOLERANCE >= inner.max.x
        && outer.max.y + GEOMETRY_TOLERANCE >= inner.max.y
}

fn glyph_union(glyphs: &[&Glyph]) -> Rect {
    let mut bbox = glyphs[0].bbox;
    for glyph in &glyphs[1..] {
        bbox.min.x = bbox.min.x.min(glyph.bbox.min.x);
        bbox.min.y = bbox.min.y.min(glyph.bbox.min.y);
        bbox.max.x = bbox.max.x.max(glyph.bbox.max.x);
        bbox.max.y = bbox.max.y.max(glyph.bbox.max.y);
    }
    bbox
}

fn rect_approximately_equal(left: Rect, right: Rect) -> bool {
    approximately_equal(left.min.x, right.min.x)
        && approximately_equal(left.min.y, right.min.y)
        && approximately_equal(left.max.x, right.max.x)
        && approximately_equal(left.max.y, right.max.y)
}

fn approximately_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= GEOMETRY_TOLERANCE
}

fn dominant_font(glyphs: &[&Glyph]) -> FontId {
    let mut counts = HashMap::<FontId, usize>::new();
    let mut dominant = glyphs[0].font_id;
    let mut dominant_count = 0;
    for glyph in glyphs {
        let count = counts.entry(glyph.font_id).or_default();
        *count += 1;
        if *count > dominant_count {
            dominant = glyph.font_id;
            dominant_count = *count;
        }
    }
    dominant
}

fn font_similarity(previous: &LineStats<'_>, current: &LineStats<'_>) -> f64 {
    if previous.dominant_font != current.dominant_font {
        return 0.0;
    }
    let max_size = previous.median_font_size.max(current.median_font_size);
    if max_size <= f64::EPSILON {
        return 1.0;
    }
    previous.median_font_size.min(current.median_font_size) / max_size
}

fn closeness(value: f64, maximum: f64) -> f64 {
    if maximum <= f64::EPSILON {
        f64::from(value <= f64::EPSILON)
    } else {
        (1.0 - value / maximum).clamp(0.0, 1.0)
    }
}

fn invalid_line(line: &Line, reason: &str) -> Error {
    Error::Unresolved(format!("line {} {reason}", line.id.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::region::TrustedLineRun, model::FontProgramHash};
    use std::cmp::Ordering;

    fn block(id: u64, lines: &[u64]) -> Block {
        Block {
            id: BlockId(id),
            lines: lines.iter().copied().map(LineId).collect(),
            role: BlockRole::Body,
        }
    }

    #[test]
    fn trusted_run_intervals_are_deterministic_and_preserve_final_blocks() {
        let trusted_runs = vec![
            TrustedLineRun {
                line_ids: vec![LineId(1), LineId(2)],
            },
            TrustedLineRun {
                line_ids: vec![LineId(3), LineId(4)],
            },
        ];
        let blocks = vec![block(0, &[1, 2]), block(1, &[3, 4])];
        let original_blocks = blocks.clone();
        let mut first_assignment = HashMap::new();
        let mut first_next_id = 0;
        assign_trusted_run_positions(&mut first_assignment, &trusted_runs, &mut first_next_id)
            .expect("disjoint trusted runs should be assigned");
        let mut second_assignment = HashMap::new();
        let mut second_next_id = 0;
        assign_trusted_run_positions(&mut second_assignment, &trusted_runs, &mut second_next_id)
            .expect("repeated assignment should succeed");

        let first_metadata = block_trusted_run_intervals(&blocks, &first_assignment);
        let second_metadata = block_trusted_run_intervals(&blocks, &second_assignment);

        assert_eq!(
            first_metadata,
            vec![
                Some(TrustedRunInterval {
                    run_id: TrustedRunId(0),
                    start: 0,
                    end: 2,
                }),
                Some(TrustedRunInterval {
                    run_id: TrustedRunId(1),
                    start: 0,
                    end: 2,
                }),
            ]
        );
        assert_eq!(second_assignment, first_assignment);
        assert_eq!(second_metadata, first_metadata);
        assert_eq!(blocks, original_blocks);
    }

    #[test]
    fn only_contiguous_increasing_lines_have_a_trusted_run_interval() {
        let run_positions_by_line_id = HashMap::from([
            (LineId(1), (TrustedRunId(0), 0)),
            (LineId(2), (TrustedRunId(0), 1)),
            (LineId(3), (TrustedRunId(0), 3)),
            (LineId(4), (TrustedRunId(1), 0)),
            (LineId(6), (TrustedRunId(0), usize::MAX)),
        ]);
        let blocks = vec![
            block(0, &[1, 2]),
            block(1, &[1, 3]),
            block(2, &[2, 1]),
            block(3, &[1, 4]),
            block(4, &[1, 5]),
            block(5, &[6]),
            block(6, &[]),
        ];

        assert_eq!(
            block_trusted_run_intervals(&blocks, &run_positions_by_line_id),
            vec![
                Some(TrustedRunInterval {
                    run_id: TrustedRunId(0),
                    start: 0,
                    end: 2,
                }),
                None,
                None,
                None,
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    fn duplicate_trusted_line_assignment_is_rejected() {
        let trusted_runs = vec![
            TrustedLineRun {
                line_ids: vec![LineId(1), LineId(2)],
            },
            TrustedLineRun {
                line_ids: vec![LineId(2), LineId(3)],
            },
        ];
        let mut runs_by_line_id = HashMap::new();
        let mut next_run_id = 0;

        let error =
            assign_trusted_run_positions(&mut runs_by_line_id, &trusted_runs, &mut next_run_id)
                .expect_err("a line cannot belong to two trusted runs");

        assert!(
            matches!(error, Error::Unresolved(message) if message.contains("assigned to multiple trusted runs"))
        );
    }

    #[test]
    fn region_ordering_rejects_missing_and_duplicate_line_ids() {
        let mut missing_values = HashMap::from([(LineId(1), "first"), (LineId(2), "second")]);
        let mut missing_order = Vec::new();
        append_region_order(&mut missing_values, &mut missing_order, [LineId(1)])
            .expect("the first region id should be accepted");
        let missing = validate_region_order(&missing_values, &missing_order, 2)
            .expect_err("an omitted line id must be rejected");
        assert!(
            matches!(missing, Error::Unresolved(message) if message.contains("remain unassigned"))
        );

        let mut duplicate_values = HashMap::from([(LineId(1), "first"), (LineId(2), "second")]);
        let mut duplicate_order = Vec::new();
        let duplicate = append_region_order(
            &mut duplicate_values,
            &mut duplicate_order,
            [LineId(1), LineId(1)],
        )
        .expect_err("a duplicate line id must be rejected");
        assert!(
            matches!(duplicate, Error::Unresolved(message) if message.contains("duplicate or unknown"))
        );
    }

    #[test]
    fn one_partial_uncertain_line_splits_trusted_runs() {
        let uncertain = BTreeSet::from([2]);

        assert!(!crosses_partial_uncertainty(0, 1, &uncertain));
        assert!(crosses_partial_uncertainty(1, 2, &uncertain));
        assert!(crosses_partial_uncertainty(2, 3, &uncertain));
        assert!(!crosses_partial_uncertainty(3, 4, &uncertain));
        assert!(crosses_partial_uncertainty(1, 4, &uncertain));
    }

    #[test]
    fn consecutive_partial_uncertain_lines_each_block_adjacent_joins() {
        let uncertain = BTreeSet::from([2, 3]);

        assert!(crosses_partial_uncertainty(1, 2, &uncertain));
        assert!(crosses_partial_uncertainty(2, 3, &uncertain));
        assert!(crosses_partial_uncertainty(3, 4, &uncertain));
        assert!(!crosses_partial_uncertainty(0, 1, &uncertain));
        assert!(!crosses_partial_uncertainty(4, 5, &uncertain));
    }

    #[test]
    fn signature_token_and_decoded_text_ordering_satisfies_total_order_laws() {
        let tokens = vec![
            SignatureToken::Text(DecodedText::Mapped("Alpha".to_owned())),
            SignatureToken::Text(DecodedText::Mapped("Beta".to_owned())),
            SignatureToken::Text(DecodedText::Unmapped {
                font_hash: FontProgramHash(vec![0x01, 0x02]),
                glyph_id: 10,
            }),
            SignatureToken::Text(DecodedText::Unmapped {
                font_hash: FontProgramHash(vec![0x01, 0x02]),
                glyph_id: 20,
            }),
            SignatureToken::Text(DecodedText::Unmapped {
                font_hash: FontProgramHash(vec![0x03, 0x04]),
                glyph_id: 5,
            }),
            SignatureToken::SyntheticSpace,
        ];

        // 1. Reflexivity and Eq consistency: a.cmp(a) == Equal, and a == a
        for a in &tokens {
            assert_eq!(a.cmp(a), Ordering::Equal);
            assert_eq!(a, a);
        }

        // 2. Consistency with Eq and Antisymmetry for all pairs (a, b)
        for (i, a) in tokens.iter().enumerate() {
            for (j, b) in tokens.iter().enumerate() {
                let cmp_ab = a.cmp(b);
                let cmp_ba = b.cmp(a);

                // Antisymmetry: a.cmp(b) == b.cmp(a).reverse()
                assert_eq!(
                    cmp_ab,
                    cmp_ba.reverse(),
                    "Antisymmetry violated for index ({i}, {j})"
                );

                // Eq consistency: a.cmp(b) == Equal <=> a == b
                if a == b {
                    assert_eq!(cmp_ab, Ordering::Equal);
                } else {
                    assert_ne!(cmp_ab, Ordering::Equal);
                }
            }
        }

        // 3. Transitivity: if a <= b and b <= c, then a <= c
        for a in &tokens {
            for b in &tokens {
                for c in &tokens {
                    if a <= b && b <= c {
                        assert!(a <= c, "Transitivity <= violated for {a:?}, {b:?}, {c:?}");
                    }
                    if a < b && b < c {
                        assert!(
                            a < c,
                            "Strict transitivity < violated for {a:?}, {b:?}, {c:?}"
                        );
                    }
                }
            }
        }

        // 4. Variant tag order: Mapped < Unmapped < SyntheticSpace
        assert!(tokens[0] < tokens[2]); // Mapped < Unmapped
        assert!(tokens[0] < tokens[5]); // Mapped < SyntheticSpace
        assert!(tokens[2] < tokens[5]); // Unmapped < SyntheticSpace
    }
}
