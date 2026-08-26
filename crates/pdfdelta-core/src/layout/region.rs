use crate::{
    Error, Result,
    layout::{Line, LineId, geometry::interval_overlap_ratio},
    model::{PageId, Rect, Vec2},
    validate::{validate_non_negative, validate_unit_interval},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionRelation {
    Above,
    Below,
    LeftOf,
    RightOf,
    Aligned,
    SameColumn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub id: RegionId,
    pub page: PageId,
    pub bbox: Rect,
    pub line_ids: Vec<LineId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionGraph {
    pub regions: Vec<Region>,
    pub edges: Vec<(RegionId, RegionId, RegionRelation)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionOptions {
    /// Minimum horizontal gap between columns as a fraction of total bounding width.
    pub min_vertical_gap_ratio: f64,
    /// Minimum vertical gap between bands as a fraction of total bounding height.
    pub min_horizontal_gap_ratio: f64,
    /// Minimum number of lines required to partition a sub-region.
    pub min_partition_lines: usize,
    /// Maximum recursive call depth, counting the page root as depth 1.
    pub max_recursion_depth: usize,
}

impl Default for RegionOptions {
    fn default() -> Self {
        Self {
            min_vertical_gap_ratio: 0.03,
            min_horizontal_gap_ratio: 0.05,
            min_partition_lines: 2,
            max_recursion_depth: 256,
        }
    }
}

pub fn validate_region_options(options: RegionOptions) -> Result<()> {
    validate_unit_interval("min_vertical_gap_ratio", options.min_vertical_gap_ratio)?;
    validate_unit_interval("min_horizontal_gap_ratio", options.min_horizontal_gap_ratio)?;
    validate_non_negative("min_partition_lines", options.min_partition_lines as f64)?;
    if options.min_partition_lines == 0 {
        return Err(Error::InvalidConfiguration(
            "min_partition_lines must be at least 1".to_owned(),
        ));
    }
    if options.max_recursion_depth == 0 {
        return Err(Error::InvalidConfiguration(
            "max_recursion_depth must be at least 1".to_owned(),
        ));
    }
    Ok(())
}

/// Partitions a page's lines into structural regions using recursive XY-Cut.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] for invalid options and
/// [`Error::LimitExceeded`] when recursive partitioning exceeds the configured
/// depth.
pub fn partition_regions(
    page: PageId,
    lines: &[Line],
    options: RegionOptions,
) -> Result<RegionGraph> {
    let mut edges = Vec::new();
    let line_refs: Vec<_> = lines.iter().collect();
    let regions = partition_regions_inner(page, &line_refs, options, Some(&mut edges))?;
    Ok(RegionGraph { regions, edges })
}

pub(super) fn partition_regions_without_edges(
    page: PageId,
    lines: &[&Line],
    options: RegionOptions,
) -> Result<Vec<Region>> {
    partition_regions_inner(page, lines, options, None)
}

fn partition_regions_inner(
    page: PageId,
    lines: &[&Line],
    options: RegionOptions,
    edges: Option<&mut Vec<(RegionId, RegionId, RegionRelation)>>,
) -> Result<Vec<Region>> {
    validate_region_options(options)?;
    if lines.is_empty() {
        return Ok(Vec::new());
    }

    let mut build = RegionBuild {
        next_id: 1,
        regions: Vec::new(),
        edges,
    };
    let initial_indices: Vec<usize> = (0..lines.len()).collect();
    xy_cut_recursive(page, lines, &initial_indices, options, 1, &mut build)?;

    Ok(build.regions)
}

struct RegionBuild<'a> {
    next_id: u64,
    regions: Vec<Region>,
    edges: Option<&'a mut Vec<(RegionId, RegionId, RegionRelation)>>,
}

fn xy_cut_recursive(
    page: PageId,
    lines: &[&Line],
    indices: &[usize],
    options: RegionOptions,
    depth: usize,
    build: &mut RegionBuild<'_>,
) -> Result<()> {
    if indices.is_empty() {
        return Ok(());
    }

    // Try horizontal split first (Above / Below bands).
    if let Some((top_indices, bottom_indices)) = try_horizontal_cut(lines, indices, options) {
        let next_depth = next_recursion_depth(depth, options.max_recursion_depth)?;
        let top_region_start = build.regions.len();
        xy_cut_recursive(page, lines, &top_indices, options, next_depth, build)?;
        let top_region_end = build.regions.len();

        let bottom_region_start = build.regions.len();
        xy_cut_recursive(page, lines, &bottom_indices, options, next_depth, build)?;
        let bottom_region_end = build.regions.len();

        let regions = &build.regions;
        if let Some(edges) = build.edges.as_mut() {
            // Add spatial graph relationships between top and bottom sub-regions.
            for top_idx in top_region_start..top_region_end {
                for bot_idx in bottom_region_start..bottom_region_end {
                    let top = &regions[top_idx];
                    let bot = &regions[bot_idx];
                    let top_id = top.id;
                    let bot_id = bot.id;
                    edges.push((top_id, bot_id, RegionRelation::Above));
                    edges.push((bot_id, top_id, RegionRelation::Below));
                    let horizontal_overlap = interval_overlap_ratio(
                        (top.bbox.min.x, top.bbox.max.x),
                        (bot.bbox.min.x, bot.bbox.max.x),
                    );
                    if horizontal_overlap > 0.5 {
                        edges.push((top_id, bot_id, RegionRelation::SameColumn));
                        edges.push((bot_id, top_id, RegionRelation::SameColumn));
                    }
                }
            }
        }
        return Ok(());
    }

    // Try vertical split (LeftOf / RightOf columns).
    if let Some((left_indices, right_indices)) = try_vertical_cut(lines, indices, options) {
        let next_depth = next_recursion_depth(depth, options.max_recursion_depth)?;
        let left_region_start = build.regions.len();
        xy_cut_recursive(page, lines, &left_indices, options, next_depth, build)?;
        let left_region_end = build.regions.len();

        let right_region_start = build.regions.len();
        xy_cut_recursive(page, lines, &right_indices, options, next_depth, build)?;
        let right_region_end = build.regions.len();

        let regions = &build.regions;
        if let Some(edges) = build.edges.as_mut() {
            for left_idx in left_region_start..left_region_end {
                for right_idx in right_region_start..right_region_end {
                    let left = &regions[left_idx];
                    let right = &regions[right_idx];
                    let left_id = left.id;
                    let right_id = right.id;
                    edges.push((left_id, right_id, RegionRelation::LeftOf));
                    edges.push((right_id, left_id, RegionRelation::RightOf));
                    let vertical_overlap = interval_overlap_ratio(
                        (left.bbox.min.y, left.bbox.max.y),
                        (right.bbox.min.y, right.bbox.max.y),
                    );
                    if vertical_overlap > 0.5 {
                        edges.push((left_id, right_id, RegionRelation::Aligned));
                        edges.push((right_id, left_id, RegionRelation::Aligned));
                    }
                }
            }
        }
        return Ok(());
    }

    // Leaf region (no further cut found).
    let region_id = RegionId(build.next_id);
    build.next_id += 1;
    let bbox = compute_bounding_box(lines, indices);
    let mut sorted_indices = indices.to_vec();
    // Sort lines inside leaf region in natural top-to-bottom, left-to-right order.
    sorted_indices.sort_by(|&a, &b| {
        lines[b]
            .bbox
            .max
            .y
            .total_cmp(&lines[a].bbox.max.y)
            .then(lines[a].bbox.min.x.total_cmp(&lines[b].bbox.min.x))
            .then(lines[a].id.0.cmp(&lines[b].id.0))
    });
    let line_ids = sorted_indices.iter().map(|&i| lines[i].id).collect();
    build.regions.push(Region {
        id: region_id,
        page,
        bbox,
        line_ids,
    });
    Ok(())
}

fn next_recursion_depth(depth: usize, limit: usize) -> Result<usize> {
    depth
        .checked_add(1)
        .filter(|next| *next <= limit)
        .ok_or(Error::LimitExceeded {
            resource: "region XY-Cut recursion depth",
            limit,
        })
}

fn try_vertical_cut(
    lines: &[&Line],
    indices: &[usize],
    options: RegionOptions,
) -> Option<(Vec<usize>, Vec<usize>)> {
    if indices.len() < options.min_partition_lines * 2 {
        return None;
    }
    let bbox = compute_bounding_box(lines, indices);
    let total_width = bbox.max.x - bbox.min.x;
    if !total_width.is_finite() || total_width <= 0.0 {
        return None;
    }

    let median_height = compute_median_height(lines, indices)?;

    // Collect horizontal projection intervals of all lines.
    let mut intervals: Vec<(f64, f64, usize)> = indices
        .iter()
        .map(|&i| (lines[i].bbox.min.x, lines[i].bbox.max.x, i))
        .collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Find the largest vertical whitespace gap across the whole height.
    let mut max_gap = 0.0;
    let mut best_split_x = 0.0;

    let mut current_max_x = intervals[0].1;
    for window in intervals.windows(2) {
        current_max_x = current_max_x.max(window[0].1);
        let next_min_x = window[1].0;
        let gap = next_min_x - current_max_x;
        if gap > max_gap {
            max_gap = gap;
            best_split_x = current_max_x + gap / 2.0;
        }
    }

    let min_gap_required = (total_width * options.min_vertical_gap_ratio).max(median_height * 0.8);
    if max_gap >= min_gap_required && best_split_x > bbox.min.x && best_split_x < bbox.max.x {
        let mut left = Vec::new();
        let mut right = Vec::new();
        for &idx in indices {
            let mid_x = (lines[idx].bbox.min.x + lines[idx].bbox.max.x) / 2.0;
            if mid_x <= best_split_x {
                left.push(idx);
            } else {
                right.push(idx);
            }
        }
        if left.len() >= options.min_partition_lines && right.len() >= options.min_partition_lines {
            return Some((left, right));
        }
    }
    None
}

fn try_horizontal_cut(
    lines: &[&Line],
    indices: &[usize],
    options: RegionOptions,
) -> Option<(Vec<usize>, Vec<usize>)> {
    if indices.len() < options.min_partition_lines * 2 {
        return None;
    }
    let bbox = compute_bounding_box(lines, indices);
    let total_height = bbox.max.y - bbox.min.y;
    if !total_height.is_finite() || total_height <= 0.0 {
        return None;
    }

    let median_height = compute_median_height(lines, indices)?;

    // Lines sorted by vertical extent (y descending: top to bottom in PDF coordinates).
    let mut intervals: Vec<(f64, f64, usize)> = indices
        .iter()
        .map(|&i| (lines[i].bbox.min.y, lines[i].bbox.max.y, i))
        .collect();
    intervals.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut max_gap = 0.0;
    let mut best_split_y = 0.0;

    let mut current_min_y = intervals[0].0;
    for window in intervals.windows(2) {
        current_min_y = current_min_y.min(window[0].0);
        let next_max_y = window[1].1;
        let gap = current_min_y - next_max_y;
        if gap > max_gap {
            max_gap = gap;
            best_split_y = current_min_y - gap / 2.0;
        }
    }

    // A structural horizontal cut must exceed normal line leading (at least 2.5x median line height).
    let min_gap_required =
        (total_height * options.min_horizontal_gap_ratio).max(median_height * 2.5);
    if max_gap >= min_gap_required && best_split_y > bbox.min.y && best_split_y < bbox.max.y {
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for &idx in indices {
            let mid_y = (lines[idx].bbox.min.y + lines[idx].bbox.max.y) / 2.0;
            if mid_y >= best_split_y {
                top.push(idx);
            } else {
                bottom.push(idx);
            }
        }
        if top.len() >= options.min_partition_lines && bottom.len() >= options.min_partition_lines {
            return Some((top, bottom));
        }
    }
    None
}

fn compute_median_height(lines: &[&Line], indices: &[usize]) -> Option<f64> {
    let mut heights: Vec<f64> = indices
        .iter()
        .map(|&i| (lines[i].bbox.max.y - lines[i].bbox.min.y).abs())
        .filter(|&h| h.is_finite() && h > 0.0)
        .collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(f64::total_cmp);
    Some(heights[heights.len() / 2])
}

fn compute_bounding_box(lines: &[&Line], indices: &[usize]) -> Rect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for &i in indices {
        min_x = min_x.min(lines[i].bbox.min.x);
        min_y = min_y.min(lines[i].bbox.min.y);
        max_x = max_x.max(lines[i].bbox.max.x);
        max_y = max_y.max(lines[i].bbox.max.y);
    }

    if min_x.is_infinite() {
        Rect {
            min: Vec2 { x: 0.0, y: 0.0 },
            max: Vec2 { x: 0.0, y: 0.0 },
        }
    } else {
        Rect {
            min: Vec2 { x: min_x, y: min_y },
            max: Vec2 { x: max_x, y: max_y },
        }
    }
}
