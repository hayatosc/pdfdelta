use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    model::{DecodedText, Document, FontId, Glyph, GlyphId, Rect, Vec2},
};

use super::{
    Line, LineId,
    geometry::{
        directions_are_compatible, interval_gap, interval_overlap_ratio, is_axis_aligned,
        is_horizontal, length_squared, median, normalize, perpendicular, projected_extent,
        projected_interval,
    },
};

const WEIGHT_SUM_TOLERANCE: f64 = 1.0e-9;
const GEOMETRY_TOLERANCE: f64 = 1.0e-9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRole {
    Body,
    RepeatedHeader,
    RepeatedFooter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub lines: Vec<LineId>,
    pub role: BlockRole,
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

pub fn reconstruct_blocks(
    document: &Document<Glyph>,
    lines: &[Line],
    options: BlockOptions,
) -> Result<Vec<Block>> {
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

    stats.sort_by(|left, right| {
        left.line
            .page
            .0
            .cmp(&right.line.page.0)
            .then(right.line.bbox.max.y.total_cmp(&left.line.bbox.max.y))
            .then(left.line.bbox.min.x.total_cmp(&right.line.bbox.min.x))
            .then(left.line.id.0.cmp(&right.line.id.0))
    });

    let pages = page_groups(&stats);
    let roles = detect_repeated_margins(&stats, &pages, options);
    let body_indices: Vec<_> = roles
        .iter()
        .enumerate()
        .filter_map(|(index, role)| (*role == BlockRole::Body).then_some(index))
        .collect();

    let mut pending = Vec::<PendingBlock>::new();
    for (position, index) in body_indices.iter().copied().enumerate() {
        let joins_previous =
            position > 0 && should_join_body(&stats, &body_indices, position, options)?;
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

    Ok(pending
        .into_iter()
        .enumerate()
        .map(|(index, block)| Block {
            id: BlockId(index as u64),
            lines: block.lines,
            role: block.role,
        })
        .collect())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
        );
        let median_font_size = median(glyphs.iter().map(|glyph| glyph.font_size).collect());
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

    let mut candidates = HashMap::<MarginKey, Vec<usize>>::new();
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
        return should_join(previous, current, options);
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
        || !should_join(previous_neighbor, previous, options)?
        || !should_join(current, current_neighbor, options)?
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

fn should_join(
    previous: &LineStats<'_>,
    current: &LineStats<'_>,
    options: BlockOptions,
) -> Result<bool> {
    if previous.line.page != current.line.page
        || !is_horizontal(previous.direction)
        || !is_horizontal(current.direction)
        || !directions_are_compatible(previous.direction, current.direction)
    {
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

fn index_glyphs(document: &Document<Glyph>) -> Result<HashMap<GlyphId, &Glyph>> {
    let mut glyphs = HashMap::with_capacity(document.items().len());
    for glyph in document.items() {
        if glyphs.insert(glyph.id, glyph).is_some() {
            return Err(Error::Unresolved(format!(
                "duplicate glyph id {}",
                glyph.id.0
            )));
        }
    }
    Ok(glyphs)
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
    if !is_axis_aligned(line.direction) {
        return Err(Error::Unsupported(format!(
            "line {} uses a non-axis-aligned writing direction",
            line.id.0
        )));
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
    if !is_axis_aligned(glyph.direction)
        || !directions_are_compatible(line.direction, glyph.direction)
    {
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
    previous.median_font_size.min(current.median_font_size)
        / previous.median_font_size.max(current.median_font_size)
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

fn validate_non_negative(name: &str, value: f64) -> Result<()> {
    if value.is_finite() && value >= 0.0 {
        return Ok(());
    }
    Err(Error::InvalidConfiguration(format!(
        "{name} must be finite and non-negative"
    )))
}

fn validate_unit_interval(name: &str, value: f64) -> Result<()> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        return Ok(());
    }
    Err(Error::InvalidConfiguration(format!(
        "{name} must be between 0 and 1"
    )))
}
