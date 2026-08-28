use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    layout::{
        Line, LineId, LineTextDirection,
        geometry::{interval_overlap_ratio, is_horizontal, length_squared, normalize},
    },
    model::{PageId, Rect, Vec2, VectorLine},
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadingOrder {
    Known(Vec<RegionId>),
    /// A row-major order proven across parallel regions. Region order alone
    /// cannot represent alternating label/value or old/new rows.
    KnownLines(Vec<LineId>),
    #[default]
    Unknown,
}

const MIN_PARALLEL_ROW_PAIRS: usize = 3;
const MIN_PARALLEL_ROW_OVERLAP_RATIO: f64 = 0.8;
const MIN_PARALLEL_ROW_GAP_HEIGHT_RATIO: f64 = 0.8;
const MIN_LEAF_ROW_OVERLAP_RATIO: f64 = 0.75;
const MAX_LEAF_ROW_BASELINE_DISTANCE_HEIGHT_RATIO: f64 = 0.1;
const MAX_PROVEN_LEAF_ROW_LINES: usize = 16;
const MAX_ACTIVE_LEAF_ROWS: usize = 16;
const VECTOR_AXIS_TOLERANCE_RATIO: f64 = 1.0e-9;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionGraph {
    pub regions: Vec<Region>,
    pub edges: Vec<(RegionId, RegionId, RegionRelation)>,
    pub reading_order: ReadingOrder,
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
    partition_regions_with_vector_lines(page, lines, &[], options)
}

/// Partitions lines while retaining straight vector evidence for conservative
/// reading-order decisions.
///
/// Vector lines can prove a dense two-column grid, but they never create text
/// regions or override ambiguous line geometry.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] for invalid options and
/// [`Error::LimitExceeded`] when recursive partitioning exceeds the configured
/// depth.
pub fn partition_regions_with_vector_lines(
    page: PageId,
    lines: &[Line],
    vector_lines: &[VectorLine],
    options: RegionOptions,
) -> Result<RegionGraph> {
    let line_refs: Vec<_> = lines.iter().collect();
    let vector_line_refs = vector_lines.iter().collect::<Vec<_>>();
    partition_regions_from_refs(page, &line_refs, &vector_line_refs, options)
        .map(|partition| partition.graph)
}

pub(super) struct RegionPartition {
    pub graph: RegionGraph,
    pub uncertain_line_ids: Vec<LineId>,
    pub trusted_runs: Vec<TrustedLineRun>,
}

/// A maximal contiguous selected segment within one leaf region.
///
/// Relative order between separate runs is unknown. Unsupported or dropped
/// lines are barriers and never occur between members of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrustedLineRun {
    pub line_ids: Vec<LineId>,
}

pub(super) fn partition_regions_from_refs(
    page: PageId,
    lines: &[&Line],
    vector_lines: &[&VectorLine],
    options: RegionOptions,
) -> Result<RegionPartition> {
    let mut edges = Vec::new();
    let regions = partition_regions_inner(page, lines, options, Some(&mut edges))?;
    let (reading_order, uncertain_line_ids, trusted_runs) =
        classify_reading_order(lines, vector_lines, &regions, &edges);
    Ok(RegionPartition {
        graph: RegionGraph {
            regions,
            edges,
            reading_order,
        },
        uncertain_line_ids,
        trusted_runs,
    })
}

fn classify_reading_order(
    lines: &[&Line],
    vector_lines: &[&VectorLine],
    regions: &[Region],
    edges: &[(RegionId, RegionId, RegionRelation)],
) -> (ReadingOrder, Vec<LineId>, Vec<TrustedLineRun>) {
    let lines_by_id = lines
        .iter()
        .map(|line| (line.id, *line))
        .collect::<HashMap<_, _>>();
    let supported_regions = regions
        .iter()
        .filter_map(|region| {
            let line_ids = trusted_region_line_ids(region, &lines_by_id);
            (!line_ids.is_empty()).then_some(Region {
                id: region.id,
                page: region.page,
                bbox: region.bbox,
                line_ids,
            })
        })
        .collect::<Vec<_>>();
    let trusted_line_ids = supported_regions
        .iter()
        .flat_map(|region| region.line_ids.iter().copied())
        .collect::<HashSet<_>>();
    let trusted_runs = trusted_region_segments(regions, &trusted_line_ids);
    let supported_ids = supported_regions
        .iter()
        .map(|region| region.id)
        .collect::<HashSet<_>>();
    let supported_edges = edges
        .iter()
        .copied()
        .filter(|(source, target, _)| {
            supported_ids.contains(source) && supported_ids.contains(target)
        })
        .collect::<Vec<_>>();
    let supported_order = classify_supported_region_order(
        &supported_regions,
        &supported_edges,
        &lines_by_id,
        vector_lines,
    );

    if matches!(supported_order, ReadingOrder::Unknown) {
        return (ReadingOrder::Unknown, sorted_line_ids(lines), trusted_runs);
    }
    let uncertain_line_ids = lines
        .iter()
        .map(|line| line.id)
        .filter(|line_id| !trusted_line_ids.contains(line_id))
        .collect::<Vec<_>>();
    if uncertain_line_ids.is_empty() {
        let trusted_runs = match &supported_order {
            ReadingOrder::KnownLines(line_ids) if !line_ids.is_empty() => vec![TrustedLineRun {
                line_ids: line_ids.clone(),
            }],
            ReadingOrder::KnownLines(_) => Vec::new(),
            _ => trusted_runs,
        };
        (supported_order, Vec::new(), trusted_runs)
    } else if matches!(supported_order, ReadingOrder::KnownLines(_)) {
        // Block reconstruction cannot merge a proven row-major line order
        // with unsupported lines without inventing their relative position.
        (ReadingOrder::Unknown, sorted_line_ids(lines), trusted_runs)
    } else {
        (ReadingOrder::Unknown, uncertain_line_ids, trusted_runs)
    }
}

fn trusted_region_segments(
    regions: &[Region],
    selected_line_ids: &HashSet<LineId>,
) -> Vec<TrustedLineRun> {
    let mut runs = Vec::new();
    for region in regions {
        let mut current = Vec::new();
        for line_id in &region.line_ids {
            if selected_line_ids.contains(line_id) {
                current.push(*line_id);
            } else if !current.is_empty() {
                runs.push(TrustedLineRun {
                    line_ids: std::mem::take(&mut current),
                });
            }
        }
        if !current.is_empty() {
            runs.push(TrustedLineRun { line_ids: current });
        }
    }
    runs
}

fn classify_supported_region_order(
    regions: &[Region],
    edges: &[(RegionId, RegionId, RegionRelation)],
    lines_by_id: &HashMap<LineId, &Line>,
    vector_lines: &[&VectorLine],
) -> ReadingOrder {
    if regions.is_empty() {
        return ReadingOrder::Known(Vec::new());
    }
    if let [region] = regions {
        return if region_lines_are_monotone(region, lines_by_id) {
            ReadingOrder::Known(vec![region.id])
        } else {
            ReadingOrder::Unknown
        };
    }
    if let [left, right] = regions {
        if is_supported_two_column_graph(left, right, edges)
            && region_lines_are_monotone(left, lines_by_id)
            && region_lines_are_monotone(right, lines_by_id)
            && regions_are_rendered_in_order(left, right, lines_by_id)
        {
            return ReadingOrder::Known(vec![left.id, right.id]);
        }
        return parallel_row_order(left, right, edges, lines_by_id, vector_lines)
            .map_or(ReadingOrder::Unknown, ReadingOrder::KnownLines);
    }

    banded_two_column_order(regions, edges, lines_by_id)
        .or_else(|| uniquely_rendered_spatial_order(regions, edges, lines_by_id))
        .map_or(ReadingOrder::Unknown, ReadingOrder::Known)
}

fn trusted_region_line_ids(region: &Region, lines: &HashMap<LineId, &Line>) -> Vec<LineId> {
    let supported = region
        .line_ids
        .iter()
        .copied()
        .filter(|line_id| {
            lines
                .get(line_id)
                .is_some_and(|line| line_is_supported(line))
        })
        .collect::<Vec<_>>();
    longest_render_monotone_subsequence(&supported, lines)
}

fn longest_render_monotone_subsequence(
    line_ids: &[LineId],
    lines: &HashMap<LineId, &Line>,
) -> Vec<LineId> {
    if line_ids.is_empty() {
        return Vec::new();
    }
    let mut predecessors = vec![None; line_ids.len()];
    let mut tails = Vec::<(u32, usize)>::new();
    for (current, line_id) in line_ids.iter().enumerate() {
        let line = lines[line_id];
        let start = *line.render_order.start();
        let end = *line.render_order.end();
        let tail_position = tails.partition_point(|(tail_end, _)| *tail_end < start);
        if tail_position > 0 {
            predecessors[current] = Some(tails[tail_position - 1].1);
        }
        if tail_position == tails.len() {
            tails.push((end, current));
        } else if end < tails[tail_position].0 {
            tails[tail_position] = (end, current);
        }
    }
    let mut cursor = tails
        .last()
        .map(|(_, index)| *index)
        .expect("a non-empty line sequence has a longest subsequence");
    let mut trusted = Vec::with_capacity(tails.len());
    loop {
        trusted.push(line_ids[cursor]);
        let Some(previous) = predecessors[cursor] else {
            break;
        };
        cursor = previous;
    }
    trusted.reverse();
    trusted
}

fn line_is_supported(line: &Line) -> bool {
    if !line_geometry_is_finite(line) || length_squared(line.direction) <= f64::EPSILON {
        return false;
    }
    let direction = normalize(line.direction);
    is_horizontal(direction)
        && direction.x > 0.0
        && matches!(
            line.text_direction,
            LineTextDirection::LeftToRight | LineTextDirection::Neutral
        )
}

fn sorted_line_ids(lines: &[&Line]) -> Vec<LineId> {
    let mut line_ids = lines.iter().map(|line| line.id).collect::<Vec<_>>();
    line_ids.sort_unstable_by_key(|line_id| line_id.0);
    line_ids.dedup();
    line_ids
}

fn parallel_row_order(
    left: &Region,
    right: &Region,
    edges: &[(RegionId, RegionId, RegionRelation)],
    lines: &HashMap<LineId, &Line>,
    vector_lines: &[&VectorLine],
) -> Option<Vec<LineId>> {
    let ruled_grid = ruled_grid_proves_parallel_rows(left, right, lines, vector_lines);
    let minimum_pairs = if ruled_grid {
        2
    } else {
        MIN_PARALLEL_ROW_PAIRS
    };
    if !is_supported_two_column_graph(left, right, edges)
        || !edges.contains(&(left.id, right.id, RegionRelation::Aligned))
        || left.line_ids.len() != right.line_ids.len()
        || left.line_ids.len() < minimum_pairs
        || !region_lines_are_monotone(left, lines)
        || !region_lines_are_monotone(right, lines)
        || (!ruled_grid
            && (!parallel_rows_are_separated(left, lines)
                || !parallel_rows_are_separated(right, lines)))
    {
        return None;
    }

    let mut order = Vec::with_capacity(left.line_ids.len().checked_mul(2)?);
    for (&left_id, &right_id) in left.line_ids.iter().zip(&right.line_ids) {
        let left_line = lines.get(&left_id)?;
        let right_line = lines.get(&right_id)?;
        let overlap = interval_overlap_ratio(
            (left_line.bbox.min.y, left_line.bbox.max.y),
            (right_line.bbox.min.y, right_line.bbox.max.y),
        );
        if overlap < MIN_PARALLEL_ROW_OVERLAP_RATIO {
            return None;
        }
        order.extend([left_id, right_id]);
    }

    order
        .windows(2)
        .all(|pair| {
            let Some(previous) = lines.get(&pair[0]) else {
                return false;
            };
            let Some(next) = lines.get(&pair[1]) else {
                return false;
            };
            previous.render_order.end() < next.render_order.start()
        })
        .then_some(order)
}

fn parallel_rows_are_separated(region: &Region, lines: &HashMap<LineId, &Line>) -> bool {
    region.line_ids.windows(2).all(|pair| {
        let Some(previous) = lines.get(&pair[0]) else {
            return false;
        };
        let Some(next) = lines.get(&pair[1]) else {
            return false;
        };
        let previous_height = previous.bbox.max.y - previous.bbox.min.y;
        let next_height = next.bbox.max.y - next.bbox.min.y;
        let reference_height = previous_height.min(next_height);
        let gap = previous.bbox.min.y - next.bbox.max.y;
        reference_height > f64::EPSILON
            && gap >= MIN_PARALLEL_ROW_GAP_HEIGHT_RATIO * reference_height
    })
}

fn ruled_grid_proves_parallel_rows(
    left: &Region,
    right: &Region,
    lines: &HashMap<LineId, &Line>,
    vector_lines: &[&VectorLine],
) -> bool {
    if left.page != right.page
        || left.line_ids.len() != right.line_ids.len()
        || left.line_ids.len() < 2
        || left.bbox.max.x > right.bbox.min.x
    {
        return false;
    }
    let table = Rect {
        min: Vec2 {
            x: left.bbox.min.x,
            y: left.bbox.min.y.min(right.bbox.min.y),
        },
        max: Vec2 {
            x: right.bbox.max.x,
            y: left.bbox.max.y.max(right.bbox.max.y),
        },
    };
    let tolerance = ((table.max.x - table.min.x).hypot(table.max.y - table.min.y)
        * VECTOR_AXIS_TOLERANCE_RATIO)
        .max(f64::EPSILON);
    let has_vertical_separator = vector_lines.iter().any(|line| {
        if line.page != left.page || !vector_line_is_vertical(line, tolerance) {
            return false;
        }
        let x = (line.from.x + line.to.x) / 2.0;
        let (min_y, max_y) = ordered_pair(line.from.y, line.to.y);
        x >= left.bbox.max.x - tolerance
            && x <= right.bbox.min.x + tolerance
            && min_y <= table.min.y + tolerance
            && max_y >= table.max.y - tolerance
    });
    if !has_vertical_separator {
        return false;
    }

    left.line_ids
        .windows(2)
        .zip(right.line_ids.windows(2))
        .all(|(left_pair, right_pair)| {
            let Some(upper_left) = lines.get(&left_pair[0]) else {
                return false;
            };
            let Some(lower_left) = lines.get(&left_pair[1]) else {
                return false;
            };
            let Some(upper_right) = lines.get(&right_pair[0]) else {
                return false;
            };
            let Some(lower_right) = lines.get(&right_pair[1]) else {
                return false;
            };
            let upper_bottom = upper_left.bbox.min.y.min(upper_right.bbox.min.y);
            let lower_top = lower_left.bbox.max.y.max(lower_right.bbox.max.y);
            if lower_top > upper_bottom + tolerance {
                return false;
            }
            vector_lines.iter().any(|line| {
                if line.page != left.page || !vector_line_is_horizontal(line, tolerance) {
                    return false;
                }
                let y = (line.from.y + line.to.y) / 2.0;
                let (min_x, max_x) = ordered_pair(line.from.x, line.to.x);
                y >= lower_top - tolerance
                    && y <= upper_bottom + tolerance
                    && min_x <= table.min.x + tolerance
                    && max_x >= table.max.x - tolerance
            })
        })
}

fn vector_line_is_horizontal(line: &VectorLine, tolerance: f64) -> bool {
    vector_line_is_finite(line)
        && (line.to.y - line.from.y).abs() <= tolerance
        && (line.to.x - line.from.x).abs() > tolerance
}

fn vector_line_is_vertical(line: &VectorLine, tolerance: f64) -> bool {
    vector_line_is_finite(line)
        && (line.to.x - line.from.x).abs() <= tolerance
        && (line.to.y - line.from.y).abs() > tolerance
}

fn vector_line_is_finite(line: &VectorLine) -> bool {
    [line.from.x, line.from.y, line.to.x, line.to.y, line.width]
        .into_iter()
        .all(f64::is_finite)
        && line.width >= 0.0
}

fn ordered_pair(first: f64, second: f64) -> (f64, f64) {
    (first.min(second), first.max(second))
}

fn banded_two_column_order(
    regions: &[Region],
    edges: &[(RegionId, RegionId, RegionRelation)],
    lines: &HashMap<LineId, &Line>,
) -> Option<Vec<RegionId>> {
    let column_pairs = edges
        .iter()
        .filter_map(|(source, target, relation)| {
            (*relation == RegionRelation::LeftOf).then_some((*source, *target))
        })
        .collect::<HashSet<_>>();
    if column_pairs.len() != 1 {
        return None;
    }
    let (left_id, right_id) = column_pairs.into_iter().next()?;
    let left = regions.iter().find(|region| region.id == left_id)?;
    let right = regions.iter().find(|region| region.id == right_id)?;
    if !is_supported_two_column_graph(left, right, edges)
        || regions
            .iter()
            .any(|region| !region_lines_are_monotone(region, lines))
    {
        return None;
    }

    uniquely_rendered_spatial_order(regions, edges, lines)
}

fn uniquely_rendered_spatial_order(
    regions: &[Region],
    edges: &[(RegionId, RegionId, RegionRelation)],
    lines: &HashMap<LineId, &Line>,
) -> Option<Vec<RegionId>> {
    if regions
        .iter()
        .any(|region| !region_lines_are_monotone(region, lines))
    {
        return None;
    }
    let order = unique_spatial_order(regions, edges)?;
    if order != regions.iter().map(|region| region.id).collect::<Vec<_>>() {
        return None;
    }
    let regions_by_id = regions
        .iter()
        .map(|region| (region.id, region))
        .collect::<HashMap<_, _>>();
    order
        .windows(2)
        .all(|pair| {
            let Some(previous) = regions_by_id.get(&pair[0]) else {
                return false;
            };
            let Some(next) = regions_by_id.get(&pair[1]) else {
                return false;
            };
            regions_are_rendered_in_order(previous, next, lines)
        })
        .then_some(order)
}

fn unique_spatial_order(
    regions: &[Region],
    edges: &[(RegionId, RegionId, RegionRelation)],
) -> Option<Vec<RegionId>> {
    let region_ids = regions
        .iter()
        .map(|region| region.id)
        .collect::<HashSet<_>>();
    let mut indegrees = region_ids
        .iter()
        .copied()
        .map(|region_id| (region_id, 0usize))
        .collect::<HashMap<_, _>>();
    let mut successors = HashMap::<RegionId, HashSet<RegionId>>::new();
    for &(source, target, relation) in edges {
        if !matches!(relation, RegionRelation::Above | RegionRelation::LeftOf) {
            continue;
        }
        if !region_ids.contains(&source) || !region_ids.contains(&target) {
            return None;
        }
        if successors.entry(source).or_default().insert(target) {
            let indegree = indegrees.get_mut(&target)?;
            *indegree = indegree.checked_add(1)?;
        }
    }

    let mut remaining = region_ids;
    let mut order = Vec::with_capacity(regions.len());
    while !remaining.is_empty() {
        let mut roots = remaining
            .iter()
            .copied()
            .filter(|region_id| indegrees.get(region_id) == Some(&0));
        let root = roots.next()?;
        if roots.next().is_some() {
            return None;
        }
        remaining.remove(&root);
        order.push(root);
        if let Some(targets) = successors.get(&root) {
            for target in targets {
                let indegree = indegrees.get_mut(target)?;
                *indegree = indegree.checked_sub(1)?;
            }
        }
    }
    Some(order)
}

fn region_lines_are_monotone(region: &Region, lines: &HashMap<LineId, &Line>) -> bool {
    let is_exact_spatial_order = region.line_ids.windows(2).all(|pair| {
        let Some(previous) = lines.get(&pair[0]) else {
            return false;
        };
        let Some(next) = lines.get(&pair[1]) else {
            return false;
        };
        previous.bbox.max.y >= next.bbox.max.y
            && previous.render_order.end() < next.render_order.start()
    });
    if is_exact_spatial_order {
        return true;
    }

    let Some(region_lines) = region
        .line_ids
        .iter()
        .map(|line_id| lines.get(line_id).copied())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    proven_row_cluster_order(&region_lines).is_some_and(|order| {
        order == region.line_ids && region_lines_are_render_monotone(region, lines)
    })
}

fn region_lines_are_render_monotone(region: &Region, lines: &HashMap<LineId, &Line>) -> bool {
    region.line_ids.windows(2).all(|pair| {
        let Some(previous) = lines.get(&pair[0]) else {
            return false;
        };
        let Some(next) = lines.get(&pair[1]) else {
            return false;
        };
        previous.render_order.end() < next.render_order.start()
    })
}

fn proven_row_cluster_order(lines: &[&Line]) -> Option<Vec<LineId>> {
    if lines.is_empty() || lines.iter().any(|line| !line_is_supported(line)) {
        return None;
    }
    let mut spatial = lines.to_vec();
    spatial.sort_by(|left, right| {
        right
            .bbox
            .max
            .y
            .total_cmp(&left.bbox.max.y)
            .then(left.bbox.min.x.total_cmp(&right.bbox.min.x))
            .then(left.id.0.cmp(&right.id.0))
    });

    let spatial_order = spatial.iter().map(|line| line.id).collect::<Vec<_>>();
    let mut rows = Vec::<Vec<&Line>>::new();
    let mut active_rows = Vec::<usize>::new();
    for line in spatial {
        if line.bbox.min.x > line.bbox.max.x || line.bbox.min.y >= line.bbox.max.y {
            return None;
        }
        active_rows.retain(|row_index| {
            rows[*row_index]
                .iter()
                .any(|member| line.bbox.max.y > member.bbox.min.y)
        });
        let mut matching_row = None;
        for &row_index in &active_rows {
            let row = &rows[row_index];
            let strong_matches = row
                .iter()
                .filter(|member| {
                    interval_overlap_ratio(
                        (member.bbox.min.y, member.bbox.max.y),
                        (line.bbox.min.y, line.bbox.max.y),
                    ) >= MIN_LEAF_ROW_OVERLAP_RATIO
                        && baselines_prove_same_row(member, line)
                })
                .count();
            if strong_matches == 0 {
                continue;
            }
            if strong_matches != row.len() || matching_row.replace(row_index).is_some() {
                return None;
            }
        }
        if let Some(row_index) = matching_row {
            let row = &mut rows[row_index];
            if row.len() >= MAX_PROVEN_LEAF_ROW_LINES {
                return None;
            }
            row.push(line);
        } else {
            if active_rows.len() >= MAX_ACTIVE_LEAF_ROWS {
                return None;
            }
            let row_index = rows.len();
            rows.push(vec![line]);
            active_rows.push(row_index);
        }
    }

    let mut ordered_lines = Vec::with_capacity(lines.len());
    for row in &mut rows {
        row.sort_by(|left, right| {
            left.bbox
                .min
                .x
                .total_cmp(&right.bbox.min.x)
                .then(left.bbox.max.x.total_cmp(&right.bbox.max.x))
                .then(left.id.0.cmp(&right.id.0))
        });
        if row.windows(2).any(|pair| {
            pair[0].bbox.max.x > pair[1].bbox.min.x
                || pair[0].render_order.end() >= pair[1].render_order.start()
        }) {
            return None;
        }
        ordered_lines.extend(row.iter().copied());
    }
    let order = ordered_lines.iter().map(|line| line.id).collect::<Vec<_>>();
    (order != spatial_order).then_some(order)
}

fn baselines_prove_same_row(left: &Line, right: &Line) -> bool {
    let reference_height =
        (left.bbox.max.y - left.bbox.min.y).min(right.bbox.max.y - right.bbox.min.y);
    reference_height > f64::EPSILON
        && (left.baseline.y - right.baseline.y).abs()
            <= MAX_LEAF_ROW_BASELINE_DISTANCE_HEIGHT_RATIO * reference_height
}

fn is_supported_two_column_graph(
    left: &Region,
    right: &Region,
    edges: &[(RegionId, RegionId, RegionRelation)],
) -> bool {
    edges.contains(&(left.id, right.id, RegionRelation::LeftOf))
        && edges.contains(&(right.id, left.id, RegionRelation::RightOf))
        && !edges.iter().any(|(source, target, relation)| {
            ((*source == left.id && *target == right.id)
                || (*source == right.id && *target == left.id))
                && matches!(relation, RegionRelation::Above | RegionRelation::Below)
        })
}

fn regions_are_rendered_in_order(
    left: &Region,
    right: &Region,
    lines: &HashMap<LineId, &Line>,
) -> bool {
    let left_last = left
        .line_ids
        .iter()
        .filter_map(|line_id| lines.get(line_id))
        .map(|line| *line.render_order.end())
        .max();
    let right_first = right
        .line_ids
        .iter()
        .filter_map(|line_id| lines.get(line_id))
        .map(|line| *line.render_order.start())
        .min();
    matches!((left_last, right_first), (Some(left), Some(right)) if left < right)
}

fn line_geometry_is_finite(line: &Line) -> bool {
    [
        line.bbox.min.x,
        line.bbox.min.y,
        line.bbox.max.x,
        line.bbox.max.y,
        line.baseline.x,
        line.baseline.y,
        line.direction.x,
        line.direction.y,
    ]
    .into_iter()
    .all(f64::is_finite)
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
    let leaf_lines = sorted_indices
        .iter()
        .map(|index| lines[*index])
        .collect::<Vec<_>>();
    if let Some(row_order) = proven_row_cluster_order(&leaf_lines) {
        let indices_by_id = sorted_indices
            .iter()
            .map(|index| (lines[*index].id, *index))
            .collect::<HashMap<_, _>>();
        if indices_by_id.len() == sorted_indices.len() {
            sorted_indices = row_order
                .iter()
                .filter_map(|line_id| indices_by_id.get(line_id).copied())
                .collect();
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: u64, min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Line {
        Line {
            id: LineId(id),
            page: PageId(0),
            glyphs: Vec::new(),
            synthetic_spaces: Vec::new(),
            bbox: Rect {
                min: Vec2 { x: min_x, y: min_y },
                max: Vec2 { x: max_x, y: max_y },
            },
            baseline: Vec2 { x: min_x, y: min_y },
            direction: Vec2 { x: 1.0, y: 0.0 },
            text_direction: LineTextDirection::LeftToRight,
            render_order: id as u32..=id as u32,
        }
    }

    fn partition(lines: &[Line]) -> RegionPartition {
        let line_refs = lines.iter().collect::<Vec<_>>();
        partition_regions_from_refs(PageId(0), &line_refs, &[], RegionOptions::default())
            .expect("region fixture should partition")
    }

    fn leaf_region(id: u64, line_ids: &[u64]) -> Region {
        Region {
            id: RegionId(id),
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 { x: 1.0, y: 1.0 },
            },
            line_ids: line_ids.iter().copied().map(LineId).collect(),
        }
    }

    #[test]
    fn selected_lines_form_maximal_region_local_segments() {
        let regions = [leaf_region(0, &[1, 2, 3, 4])];
        let selected = HashSet::from([LineId(1), LineId(3), LineId(4)]);

        assert_eq!(
            trusted_region_segments(&regions, &selected),
            vec![
                TrustedLineRun {
                    line_ids: vec![LineId(1)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(3), LineId(4)],
                },
            ]
        );
    }

    #[test]
    fn unsupported_middle_lines_split_each_column_independently() {
        let regions = [leaf_region(0, &[1, 3, 5]), leaf_region(1, &[2, 4, 6])];
        let selected = HashSet::from([LineId(1), LineId(2), LineId(5), LineId(6)]);

        assert_eq!(
            trusted_region_segments(&regions, &selected),
            vec![
                TrustedLineRun {
                    line_ids: vec![LineId(1)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(5)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(2)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(6)],
                },
            ]
        );
    }

    #[test]
    fn ambiguous_regions_preserve_disjoint_internal_line_runs() {
        let lines = vec![
            line(1, 50.0, 700.0, 150.0, 712.0),
            line(2, 250.0, 700.0, 350.0, 712.0),
            line(3, 50.0, 680.0, 150.0, 692.0),
            line(4, 250.0, 680.0, 350.0, 692.0),
            line(5, 50.0, 660.0, 150.0, 672.0),
            line(6, 250.0, 660.0, 350.0, 672.0),
        ];

        let partition = partition(&lines);

        assert_eq!(partition.graph.reading_order, ReadingOrder::Unknown);
        assert_eq!(
            partition.uncertain_line_ids,
            (1..=6).map(LineId).collect::<Vec<_>>()
        );
        assert_eq!(
            partition.trusted_runs,
            vec![
                TrustedLineRun {
                    line_ids: vec![LineId(1), LineId(3), LineId(5)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(2), LineId(4), LineId(6)],
                },
            ]
        );
        let distinct_line_ids = partition
            .trusted_runs
            .iter()
            .flat_map(|run| run.line_ids.iter().copied())
            .collect::<HashSet<_>>();
        assert_eq!(distinct_line_ids.len(), 6);
    }

    #[test]
    fn partial_known_lines_preserve_region_local_runs() {
        let mut lines = [
            line(1, 50.0, 700.0, 150.0, 712.0),
            line(2, 250.0, 700.0, 350.0, 712.0),
            line(3, 50.0, 675.0, 150.0, 687.0),
            line(4, 250.0, 675.0, 350.0, 687.0),
            line(5, 50.0, 650.0, 150.0, 662.0),
            line(6, 250.0, 650.0, 350.0, 662.0),
            line(7, 50.0, 625.0, 150.0, 637.0),
            line(8, 250.0, 625.0, 350.0, 637.0),
        ];
        lines[6].direction = Vec2 { x: 0.0, y: 0.0 };
        lines[7].direction = Vec2 { x: 0.0, y: 0.0 };

        let partition = partition(&lines);

        assert_eq!(partition.graph.reading_order, ReadingOrder::Unknown);
        assert_eq!(
            partition.uncertain_line_ids,
            (1..=8).map(LineId).collect::<Vec<_>>()
        );
        assert_eq!(
            partition.trusted_runs,
            vec![
                TrustedLineRun {
                    line_ids: vec![LineId(1), LineId(3), LineId(5)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(2), LineId(4), LineId(6)],
                },
            ]
        );
    }

    #[test]
    fn lmis_dropped_middle_line_splits_partial_known_run() {
        let mut lines = vec![
            line(1, 50.0, 700.0, 150.0, 712.0),
            line(2, 50.0, 680.0, 150.0, 692.0),
            line(3, 50.0, 660.0, 150.0, 672.0),
        ];
        lines[0].render_order = 1..=1;
        lines[1].render_order = 10..=10;
        lines[2].render_order = 3..=3;

        let partition = partition(&lines);

        assert_eq!(partition.graph.reading_order, ReadingOrder::Unknown);
        assert_eq!(partition.uncertain_line_ids, vec![LineId(2)]);
        assert_eq!(
            partition.trusted_runs,
            vec![
                TrustedLineRun {
                    line_ids: vec![LineId(1)],
                },
                TrustedLineRun {
                    line_ids: vec![LineId(3)],
                },
            ]
        );
    }

    #[test]
    fn fully_known_line_order_preserves_global_row_major_run() {
        let lines = vec![
            line(1, 50.0, 700.0, 150.0, 712.0),
            line(2, 250.0, 700.0, 350.0, 712.0),
            line(3, 50.0, 675.0, 150.0, 687.0),
            line(4, 250.0, 675.0, 350.0, 687.0),
            line(5, 50.0, 650.0, 150.0, 662.0),
            line(6, 250.0, 650.0, 350.0, 662.0),
        ];

        let partition = partition(&lines);

        assert_eq!(
            partition.graph.reading_order,
            ReadingOrder::KnownLines((1..=6).map(LineId).collect())
        );
        assert!(partition.uncertain_line_ids.is_empty());
        assert_eq!(
            partition.trusted_runs,
            vec![TrustedLineRun {
                line_ids: (1..=6).map(LineId).collect(),
            }]
        );
    }

    #[test]
    fn jittered_glossary_rows_use_render_consistent_horizontal_order() {
        let mut lines = [
            line(592, 90.0, 340.0, 350.0, 348.540),
            line(593, 50.0, 340.0, 70.0, 348.396),
            line(594, 90.0, 332.92, 350.0, 341.08),
            line(595, 90.0, 320.0, 350.0, 328.540),
            line(596, 50.0, 320.0, 70.0, 328.396),
            line(597, 90.0, 300.0, 350.0, 308.540),
            line(598, 50.0, 300.0, 70.0, 308.396),
        ];
        let expected = [593, 592, 594, 596, 595, 598, 597].map(LineId);
        for (render_order, line_id) in expected.iter().enumerate() {
            let line = lines
                .iter_mut()
                .find(|line| line.id == *line_id)
                .expect("expected line should exist");
            line.render_order = render_order as u32..=render_order as u32;
        }
        let line_refs = lines.iter().collect::<Vec<_>>();
        let partition = partition_regions_from_refs(
            PageId(0),
            &line_refs,
            &[],
            RegionOptions {
                min_partition_lines: 10,
                ..RegionOptions::default()
            },
        )
        .expect("glossary fixture should partition");

        assert_eq!(partition.graph.regions[0].line_ids, expected);
        assert!(matches!(
            partition.graph.reading_order,
            ReadingOrder::Known(_)
        ));
        assert!(partition.uncertain_line_ids.is_empty());
        assert_eq!(
            partition.trusted_runs,
            vec![TrustedLineRun {
                line_ids: expected.to_vec(),
            }]
        );
    }

    #[test]
    fn ambiguous_vertical_overlap_chain_remains_unknown() {
        let mut lines = vec![
            line(1, 10.0, 10.0, 20.0, 20.0),
            line(3, 30.0, 8.0, 40.0, 20.0),
            line(2, 50.0, 7.0, 60.0, 17.0),
        ];
        for line in &mut lines {
            line.baseline.y = 10.0;
        }

        let partition = partition(&lines);

        assert_eq!(partition.graph.reading_order, ReadingOrder::Unknown);
        assert!(!partition.uncertain_line_ids.is_empty());
    }

    #[test]
    fn vertically_offset_overlapping_lines_remain_unknown() {
        let lines = vec![
            line(1, 10.0, 10.0, 20.0, 20.0),
            line(2, 30.0, 8.0, 100.0, 20.1),
        ];
        let line_refs = lines.iter().collect::<Vec<_>>();

        assert_eq!(proven_row_cluster_order(&line_refs), None);
        assert_eq!(partition(&lines).graph.reading_order, ReadingOrder::Unknown);
    }

    #[test]
    fn local_row_proof_survives_a_distant_render_barrier() {
        let mut lines = vec![
            line(1, 10.0, 10.0, 20.0, 20.0),
            line(2, 30.0, 10.0, 100.0, 20.1),
            line(3, 10.0, 0.0, 100.0, 5.0),
        ];
        lines[0].render_order = 1..=1;
        lines[1].render_order = 2..=2;
        lines[2].render_order = 0..=0;

        let partition = partition(&lines);

        assert_eq!(
            partition.graph.regions[0].line_ids,
            [LineId(1), LineId(2), LineId(3)]
        );
        assert_eq!(partition.graph.reading_order, ReadingOrder::Unknown);
        assert_eq!(partition.uncertain_line_ids, vec![LineId(3)]);
        assert_eq!(
            partition.trusted_runs,
            vec![TrustedLineRun {
                line_ids: vec![LineId(1), LineId(2)],
            }]
        );
    }

    #[test]
    fn horizontally_overlapping_row_lines_remain_unknown() {
        let lines = vec![
            line(2, 10.0, 10.0, 60.0, 20.0),
            line(1, 50.0, 10.0, 100.0, 20.0),
        ];

        let partition = partition(&lines);

        assert_eq!(partition.graph.reading_order, ReadingOrder::Unknown);
        assert!(!partition.uncertain_line_ids.is_empty());
    }

    #[test]
    fn unsupported_lines_cannot_prove_jittered_row_order() {
        let mut lines = vec![
            line(1, 10.0, 10.0, 20.0, 20.0),
            line(2, 30.0, 10.0, 100.0, 20.1),
        ];
        lines[1].direction = Vec2 { x: 0.0, y: 1.0 };
        let line_refs = lines.iter().collect::<Vec<_>>();

        assert_eq!(proven_row_cluster_order(&line_refs), None);
        assert_eq!(partition(&lines).graph.reading_order, ReadingOrder::Unknown);
    }
}
