use crate::{
    Error, Result,
    layout::{Line, LineId},
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
}

impl Default for RegionOptions {
    fn default() -> Self {
        Self {
            min_vertical_gap_ratio: 0.03,
            min_horizontal_gap_ratio: 0.05,
            min_partition_lines: 2,
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
    Ok(())
}

/// Partitions a page's lines into structural regions using recursive XY-Cut and builds a RegionGraph.
pub fn partition_regions(
    page: PageId,
    lines: &[Line],
    options: RegionOptions,
) -> Result<RegionGraph> {
    validate_region_options(options)?;
    if lines.is_empty() {
        return Ok(RegionGraph::default());
    }

    let mut next_id = 1_u64;
    let mut regions = Vec::new();
    let mut edges = Vec::new();

    let initial_indices: Vec<usize> = (0..lines.len()).collect();
    xy_cut_recursive(
        page,
        lines,
        &initial_indices,
        options,
        &mut next_id,
        &mut regions,
        &mut edges,
    );

    Ok(RegionGraph { regions, edges })
}

fn xy_cut_recursive(
    page: PageId,
    lines: &[Line],
    indices: &[usize],
    options: RegionOptions,
    next_id: &mut u64,
    regions: &mut Vec<Region>,
    edges: &mut Vec<(RegionId, RegionId, RegionRelation)>,
) {
    if indices.is_empty() {
        return;
    }

    // Try horizontal split first (Above / Below bands).
    if let Some((top_indices, bottom_indices)) = try_horizontal_cut(lines, indices, options) {
        let top_region_start = regions.len();
        xy_cut_recursive(page, lines, &top_indices, options, next_id, regions, edges);
        let top_region_end = regions.len();

        let bottom_region_start = regions.len();
        xy_cut_recursive(
            page,
            lines,
            &bottom_indices,
            options,
            next_id,
            regions,
            edges,
        );
        let bottom_region_end = regions.len();

        // Add spatial graph relationships between top and bottom sub-regions.
        for top_idx in top_region_start..top_region_end {
            for bot_idx in bottom_region_start..bottom_region_end {
                let top_id = regions[top_idx].id;
                let bot_id = regions[bot_idx].id;
                edges.push((top_id, bot_id, RegionRelation::Above));
                edges.push((bot_id, top_id, RegionRelation::Below));
            }
        }
        return;
    }

    // Try vertical split (LeftOf / RightOf columns).
    if let Some((left_indices, right_indices)) = try_vertical_cut(lines, indices, options) {
        let left_region_start = regions.len();
        xy_cut_recursive(page, lines, &left_indices, options, next_id, regions, edges);
        let left_region_end = regions.len();

        let right_region_start = regions.len();
        xy_cut_recursive(
            page,
            lines,
            &right_indices,
            options,
            next_id,
            regions,
            edges,
        );
        let right_region_end = regions.len();

        for left_idx in left_region_start..left_region_end {
            for right_idx in right_region_start..right_region_end {
                let left_id = regions[left_idx].id;
                let right_id = regions[right_idx].id;
                edges.push((left_id, right_id, RegionRelation::LeftOf));
                edges.push((right_id, left_id, RegionRelation::RightOf));
                edges.push((left_id, right_id, RegionRelation::SameColumn));
            }
        }
        return;
    }

    // Leaf region (no further cut found).
    let region_id = RegionId(*next_id);
    *next_id += 1;
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
    regions.push(Region {
        id: region_id,
        page,
        bbox,
        line_ids,
    });
}

fn try_vertical_cut(
    lines: &[Line],
    indices: &[usize],
    options: RegionOptions,
) -> Option<(Vec<usize>, Vec<usize>)> {
    if indices.len() < options.min_partition_lines * 2 {
        return None;
    }
    let bbox = compute_bounding_box(lines, indices);
    let total_width = bbox.max.x - bbox.min.x;
    if total_width <= 0.0 {
        return None;
    }

    let median_height = compute_median_height(lines, indices);

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
    lines: &[Line],
    indices: &[usize],
    options: RegionOptions,
) -> Option<(Vec<usize>, Vec<usize>)> {
    if indices.len() < options.min_partition_lines * 2 {
        return None;
    }
    let bbox = compute_bounding_box(lines, indices);
    let total_height = bbox.max.y - bbox.min.y;
    if total_height <= 0.0 {
        return None;
    }

    let median_height = compute_median_height(lines, indices);

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

fn compute_median_height(lines: &[Line], indices: &[usize]) -> f64 {
    let mut heights: Vec<f64> = indices
        .iter()
        .map(|&i| (lines[i].bbox.max.y - lines[i].bbox.min.y).abs())
        .filter(|&h| h > 0.0)
        .collect();
    if heights.is_empty() {
        return 12.0;
    }
    heights.sort_by(|a, b| a.total_cmp(b));
    heights[heights.len() / 2]
}

fn compute_bounding_box(lines: &[Line], indices: &[usize]) -> Rect {
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
