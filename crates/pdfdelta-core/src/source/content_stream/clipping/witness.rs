//! Exact point witnesses for partially clipped glyph boxes. Closed control
//! hulls authorize chord homotopies only when they avoid the query point.
//! Equal probe values never authorize whole-box visibility or absence.
use crate::{
    Error, Result,
    model::{GlyphPathClipStatus, PageId, Rect},
    pdf::{
        ObjectRef,
        content::{Matrix, Operand, Operation},
    },
};
use std::sync::Arc;
type P = [f64; 2];
type I = [i128; 2];
const DEPTH: u32 = 3;
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Edge {
    Line(P, P),
    Cubic([P; 4]),
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreparedPath {
    edges: Vec<Edge>,
    even_odd: bool,
}
#[derive(Clone, Debug, PartialEq)]
enum IntEdge {
    Line(I, I),
    Cubic([I; 4]),
}
#[derive(Debug)]
struct Budget {
    used: usize,
    limit: usize,
    exhausted: bool,
}
impl Budget {
    fn new(used: usize, limit: usize) -> Self {
        Self {
            used,
            limit,
            exhausted: false,
        }
    }
    fn charge(&mut self, n: usize) -> Option<()> {
        if let Some(total) = self
            .used
            .checked_add(n)
            .filter(|&total| total <= self.limit)
        {
            self.used = total;
            Some(())
        } else {
            self.exhausted = true;
            None
        }
    }
}
fn dyadic(x: f64) -> Option<(i128, i32)> {
    if !x.is_finite() {
        return None;
    }
    if x == 0.0 {
        return Some((0, 0));
    }
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 2047) as i32;
    let mut mantissa = bits & ((1_u64 << 52) - 1);
    let mut power = if exponent == 0 {
        -1074
    } else {
        mantissa |= 1_u64 << 52;
        exponent - 1023 - 52
    };
    let trailing = mantissa.trailing_zeros();
    mantissa >>= trailing;
    power += trailing as i32;
    Some((
        if bits >> 63 == 0 {
            i128::from(mantissa)
        } else {
            -i128::from(mantissa)
        },
        power,
    ))
}
fn integer(x: f64, power: i32) -> Option<i128> {
    let (n, e) = dyadic(x)?;
    if n == 0 {
        return Some(0);
    }
    let shift = u32::try_from(e.checked_sub(power)?).ok()?;
    if shift >= 127 {
        return None;
    }
    n.checked_mul(1_i128.checked_shl(shift)?)
}
fn orient(a: I, b: I, c: I, budget: &mut Budget) -> Option<i128> {
    budget.charge(96)?;
    let x = b[0].checked_sub(a[0])?;
    let y = b[1].checked_sub(a[1])?;
    let u = c[0].checked_sub(a[0])?;
    let v = c[1].checked_sub(a[1])?;
    x.checked_mul(v)?.checked_sub(y.checked_mul(u)?)
}
fn shifted(p: I, q: I) -> Option<I> {
    Some([p[0].checked_sub(q[0])?, p[1].checked_sub(q[1])?])
}
fn midpoint(a: I, b: I) -> Option<I> {
    let x = a[0].checked_add(b[0])?;
    let y = a[1].checked_add(b[1])?;
    if x % 2 != 0 || y % 2 != 0 {
        return None;
    }
    Some([x / 2, y / 2])
}
fn hull_avoids_origin(curve: [I; 4], budget: &mut Budget) -> Option<bool> {
    if (0..2).any(|axis| curve.iter().all(|p| p[axis] > 0) || curve.iter().all(|p| p[axis] < 0)) {
        return Some(true);
    }
    for i in 0..4 {
        for j in i + 1..4 {
            if curve[i] == curve[j] {
                continue;
            }
            let q = orient(curve[i], curve[j], [0, 0], budget)?;
            if q == 0 {
                continue;
            }
            let mut lower = false;
            let mut upper = false;
            for p in curve {
                let s = orient(curve[i], curve[j], p, budget)?;
                lower |= s < 0;
                upper |= s > 0;
            }
            if (q < 0 && !lower) || (q > 0 && !upper) {
                return Some(true);
            }
        }
    }
    Some(false)
}
fn line_winding(a: I, b: I, budget: &mut Budget) -> Option<i64> {
    budget.charge(32)?;
    if a == b {
        return if a == [0, 0] { None } else { Some(0) };
    }
    let low = a[1].min(b[1]);
    let high = a[1].max(b[1]);
    if low > 0 || high < 0 {
        return Some(0);
    }
    let xmin = a[0].min(b[0]);
    let xmax = a[0].max(b[0]);
    let straddles = (a[1] <= 0 && b[1] > 0) || (b[1] <= 0 && a[1] > 0);
    if xmax < 0 {
        return Some(0);
    }
    if xmin > 0 {
        return Some(if straddles {
            if b[1] > a[1] { 1 } else { -1 }
        } else {
            0
        });
    }
    let s = orient(a, b, [0, 0], budget)?;
    if s == 0 && xmin <= 0 && 0 <= xmax && low <= 0 && 0 <= high {
        return None;
    }
    Some(if a[1] <= 0 && 0 < b[1] && s > 0 {
        1
    } else if b[1] <= 0 && 0 < a[1] && s < 0 {
        -1
    } else {
        0
    })
}
fn curve_winding(curve: [I; 4], depth: u32, budget: &mut Budget) -> Option<i64> {
    budget.charge(64)?;
    if hull_avoids_origin(curve, budget)? {
        return line_winding(curve[0], curve[3], budget);
    }
    if depth == DEPTH {
        return None;
    }
    budget.charge(192)?;
    let [a, b, c, d] = curve;
    let e = midpoint(a, b)?;
    let f = midpoint(b, c)?;
    let g = midpoint(c, d)?;
    let h = midpoint(e, f)?;
    let i = midpoint(f, g)?;
    let j = midpoint(h, i)?;
    curve_winding([a, e, h, j], depth + 1, budget)?.checked_add(curve_winding(
        [j, i, g, d],
        depth + 1,
        budget,
    )?)
}
fn point_state(clips: &[(Vec<IntEdge>, bool)], point: I, budget: &mut Budget) -> Option<bool> {
    let mut held = false;
    for (edges, even_odd) in clips {
        let mut winding = 0_i64;
        let mut complete = true;
        // Every root is visited and priced, even if another edge is held.
        for edge in edges {
            budget.charge(32)?;
            let result = match edge {
                IntEdge::Line(a, b) => shifted(*a, point)
                    .zip(shifted(*b, point))
                    .and_then(|(a, b)| line_winding(a, b, budget)),
                IntEdge::Cubic(c) => {
                    let points = c.map(|p| shifted(p, point));
                    if let [Some(a), Some(b), Some(c), Some(d)] = points {
                        curve_winding([a, b, c, d], 0, budget)
                    } else {
                        None
                    }
                }
            };
            match result {
                Some(n) => winding = winding.checked_add(n)?,
                None => complete = false,
            }
        }
        if complete {
            let inside = if *even_odd {
                winding % 2 != 0
            } else {
                winding != 0
            };
            if !inside {
                return Some(false);
            }
        } else {
            held = true;
        }
    }
    if held { None } else { Some(true) }
}
fn partial(
    clips: &[&PreparedPath],
    bbox: [f64; 4],
    steps: usize,
    budget: &mut Budget,
) -> Option<(bool, usize, usize, usize)> {
    if steps == 0
        || !bbox.into_iter().all(f64::is_finite)
        || bbox[0] >= bbox[2]
        || bbox[1] >= bbox[3]
    {
        return None;
    }
    let population = steps
        .checked_add(1)?
        .checked_mul(steps.checked_add(1)?)?
        .checked_add(4)?;
    budget.charge(population.checked_mul(256)?)?;
    budget.charge(clips.len().checked_mul(128)?)?;
    let edge_count = clips
        .iter()
        .try_fold(0_usize, |n, c| n.checked_add(c.edges.len()))?;
    budget.charge(edge_count.checked_mul(16)?)?;
    let point_counts = clips
        .iter()
        .map(|c| {
            c.edges.iter().try_fold(0_usize, |n, e| {
                n.checked_add(match e {
                    Edge::Line(..) => 2,
                    Edge::Cubic(..) => 4,
                })
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let point_count = point_counts
        .iter()
        .try_fold(0_usize, |n, c| n.checked_add(*c))?;
    budget.charge(point_count.checked_mul(128)?)?;
    let points = clips
        .iter()
        .flat_map(|c| {
            c.edges.iter().flat_map(|e| match e {
                Edge::Line(a, b) => vec![*a, *b],
                Edge::Cubic(c) => c.to_vec(),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(points.len(), point_count);
    let mut roi = bbox;
    let mut cursor = 0_usize;
    for count in point_counts {
        let end = cursor.checked_add(count)?;
        let controls = &points[cursor..end];
        if controls.is_empty() {
            return None;
        }
        for axis in 0..2 {
            let low = controls
                .iter()
                .map(|p| p[axis])
                .fold(f64::INFINITY, f64::min);
            let high = controls
                .iter()
                .map(|p| p[axis])
                .fold(f64::NEG_INFINITY, f64::max);
            roi[axis] = roi[axis].max(low);
            roi[axis + 2] = roi[axis + 2].min(high);
        }
        cursor = end;
    }
    if roi[0] >= roi[2] || roi[1] >= roi[3] {
        return None;
    }
    let mut order = (0..=steps)
        .flat_map(|i| (0..=steps).map(move |j| (i, j)))
        .collect::<Vec<_>>();
    let center = steps / 2;
    order.sort_unstable_by_key(|&(i, j)| {
        let x = i.abs_diff(center);
        let y = j.abs_diff(center);
        (x * x + y * y, i, j)
    });
    let mut queries = Vec::with_capacity(population);
    queries.extend([
        [bbox[0], bbox[1]],
        [bbox[0], bbox[3]],
        [bbox[2], bbox[1]],
        [bbox[2], bbox[3]],
    ]);
    for (i, j) in order {
        let p = [
            roi[0] + (roi[2] - roi[0]) * i as f64 / steps as f64,
            roi[1] + (roi[3] - roi[1]) * j as f64 / steps as f64,
        ];
        if !p.into_iter().all(f64::is_finite)
            || p[0] < bbox[0]
            || p[0] > bbox[2]
            || p[1] < bbox[1]
            || p[1] > bbox[3]
        {
            return None;
        }
        queries.push(p);
    }
    assert_eq!(queries.len(), population);
    let power = points
        .iter()
        .chain(queries.iter())
        .flat_map(|p| p.iter())
        .filter(|x| **x != 0.0)
        .map(|x| dyadic(*x).map(|(_, e)| e))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min()?
        .checked_sub((3 * DEPTH) as i32)?;
    let convert = |p: P| Some([integer(p[0], power)?, integer(p[1], power)?]);
    let mut normalized = Vec::with_capacity(clips.len());
    for clip in clips {
        let mut edges = Vec::with_capacity(clip.edges.len());
        for e in &clip.edges {
            edges.push(match e {
                Edge::Line(a, b) => IntEdge::Line(convert(*a)?, convert(*b)?),
                Edge::Cubic(c) => {
                    let p = c.map(convert);
                    if let [Some(a), Some(b), Some(c), Some(d)] = p {
                        IntEdge::Cubic([a, b, c, d])
                    } else {
                        return None;
                    }
                }
            });
        }
        normalized.push((edges, clip.even_odd));
    }
    let (mut inside, mut outside, mut held) = (0, 0, 0);
    for query in queries {
        let state = point_state(&normalized, convert(query)?, budget);
        match state {
            Some(true) => inside += 1,
            Some(false) => outside += 1,
            None => held += 1,
        }
        if inside > 0 && outside > 0 {
            break;
        }
    }
    Some((inside > 0 && outside > 0, inside, outside, held))
}

/// The exact native invocation and operation interval owning one immutable path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::source::content_stream) struct SourceOrigin {
    pub stream: ObjectRef,
    pub page: PageId,
    pub invocation: usize,
    pub first_operator: u32,
    pub terminator: u32,
}

/// Original typed operations remain owned beside their complete finite geometry.
/// Only clones of this same owner share preparation; no geometric-key cache exists.
#[derive(Debug, PartialEq)]
pub(in crate::source::content_stream) struct FrozenPath {
    operations: Vec<Operation>,
    geometry: PreparedPath,
    origin: SourceOrigin,
}

impl FrozenPath {
    /// Prepays inspection of every operation, capacity and numeric operand.
    pub(in crate::source::content_stream) fn validation_work(n: usize) -> Option<usize> {
        n.checked_mul(128)?.checked_add(128)
    }

    /// Called after validation work is prepaid, before ownership or allocation.
    /// Capacities, including the original buffers and new headers, are charged.
    pub(in crate::source::content_stream) fn retention_work(
        operations: &[Operation],
    ) -> Option<usize> {
        if operations.is_empty() {
            return None;
        }
        let mut bytes = 0_usize;
        for (index, op) in operations.iter().enumerate() {
            let expected = match op.operator.as_slice() {
                b"m" | b"l" => 2,
                b"c" => 6,
                b"v" | b"y" | b"re" => 4,
                b"h" => 0,
                b"W" | b"W*" if index + 1 == operations.len() => 0,
                _ => return None,
            };
            if op.operands.len() != expected
                || !op
                    .operands
                    .iter()
                    .all(|v| matches!(v,Operand::Number(x)if x.is_finite()))
            {
                return None;
            }
            bytes = bytes
                .checked_add(std::mem::size_of::<Operation>())?
                .checked_add(op.operator.capacity())?
                .checked_add(
                    op.operands
                        .capacity()
                        .checked_mul(std::mem::size_of::<Operand>())?,
                )?;
        }
        if !matches!(operations.last()?.operator.as_slice(), b"W" | b"W*") {
            return None;
        }
        let headers = operations
            .len()
            .checked_mul(std::mem::size_of::<Operation>())?;
        bytes
            .checked_add(headers)?
            .checked_mul(2)?
            .checked_add(1024)
    }

    /// Includes all floating transforms, closure validation and geometry storage.
    pub(in crate::source::content_stream) fn normalization_work(
        operations: &[Operation],
    ) -> Option<usize> {
        operations
            .len()
            .checked_mul(256)?
            .checked_add(Self::edge_capacity(operations)?.checked_mul(256)?)?
            .checked_add(1024)
    }

    fn edge_capacity(operations: &[Operation]) -> Option<usize> {
        operations.iter().try_fold(0_usize, |count, operation| {
            count.checked_add(match operation.operator.as_slice() {
                b"re" => 4,
                b"l" | b"h" | b"c" | b"v" | b"y" => 1,
                b"m" | b"W" | b"W*" => 0,
                _ => return None,
            })
        })
    }

    /// Requires the complete retention and normalization bounds to be prepaid.
    /// The source run has one fixed transform and ends with its fill-rule operator.
    pub(in crate::source::content_stream) fn new(
        operations: Vec<Operation>,
        matrix: Matrix,
        origin: SourceOrigin,
        even_odd: bool,
    ) -> Option<Self> {
        Self::retention_work(&operations)?;
        if !operations
            .windows(2)
            .all(|pair| pair[0].index.checked_add(1) == Some(pair[1].index))
        {
            return None;
        }
        if operations.is_empty()
            || origin.invocation == 0
            || origin.first_operator != operations.first()?.index
            || operations.last()?.index.checked_add(1) != Some(origin.terminator)
        {
            return None;
        }
        if (operations.last()?.operator.as_slice() == b"W*") != even_odd {
            return None;
        }
        let mut edges = Vec::with_capacity(Self::edge_capacity(&operations)?);
        let (mut current, mut start): (Option<P>, Option<P>) = (None, None);
        for op in &operations {
            let mut values = [0.0; 6];
            if op.operands.len() > values.len() {
                return None;
            }
            for (slot, operand) in values.iter_mut().zip(&op.operands) {
                let Operand::Number(x) = operand else {
                    return None;
                };
                if !x.is_finite() {
                    return None;
                }
                *slot = *x;
            }
            let point = |x, y| matrix.transform_point(x, y).ok().map(|(x, y)| [x, y]);
            match op.operator.as_slice() {
                b"m" => {
                    if current.is_some() && current != start {
                        return None;
                    }
                    start = Some(point(values[0], values[1])?);
                    current = start;
                }
                b"l" => {
                    let to = point(values[0], values[1])?;
                    edges.push(Edge::Line(current?, to));
                    current = Some(to);
                }
                b"c" => {
                    let controls = [
                        current?,
                        point(values[0], values[1])?,
                        point(values[2], values[3])?,
                        point(values[4], values[5])?,
                    ];
                    edges.push(Edge::Cubic(controls));
                    current = Some(controls[3]);
                }
                b"v" | b"y" => {
                    let to = point(values[2], values[3])?;
                    let control = point(values[0], values[1])?;
                    let controls = if op.operator == b"v" {
                        [current?, current?, control, to]
                    } else {
                        [current?, control, to, to]
                    };
                    edges.push(Edge::Cubic(controls));
                    current = Some(to);
                }
                b"h" => {
                    if current != start {
                        edges.push(Edge::Line(current?, start?));
                    }
                    current = start;
                }
                b"re" => {
                    if current.is_some() && current != start {
                        return None;
                    }
                    let (x, y, w, h) = (values[0], values[1], values[2], values[3]);
                    let corners = [
                        point(x, y)?,
                        point(x + w, y)?,
                        point(x + w, y + h)?,
                        point(x, y + h)?,
                    ];
                    for index in 0..4 {
                        edges.push(Edge::Line(corners[index], corners[(index + 1) % 4]));
                    }
                    start = Some(corners[0]);
                    current = start;
                }
                b"W" | b"W*" => {}
                _ => return None,
            }
        }
        if current != start || edges.is_empty() {
            return None;
        }
        Some(Self {
            operations,
            geometry: PreparedPath { edges, even_odd },
            origin,
        })
    }

    fn geometry(&self, page: PageId) -> Option<&PreparedPath> {
        if self.origin.page != page || self.operations.is_empty() {
            None
        } else {
            Some(&self.geometry)
        }
    }
}

/// A closed physical path constraint under independently witnessed predecessors.
/// Crops are carried separately in each graphics state rather than in this owner.
#[derive(Debug, PartialEq)]
pub(in crate::source::content_stream) struct PathWitness {
    source: Arc<FrozenPath>,
    parent: Option<Arc<Self>>,
    reason: Arc<str>,
    depth: usize,
    total_edges: usize,
    total_points: usize,
}
impl PathWitness {
    pub(in crate::source::content_stream) fn new(
        source: Arc<FrozenPath>,
        parent: Option<Arc<Self>>,
        reason: Arc<str>,
        depth_limit: usize,
    ) -> Option<Self> {
        let depth = parent
            .as_ref()
            .map_or(Some(1), |p| p.depth.checked_add(1))?;
        if depth > depth_limit {
            return None;
        }
        let total_edges = source
            .geometry
            .edges
            .len()
            .checked_add(parent.as_ref().map_or(0, |p| p.total_edges))?;
        let points = source.geometry.edges.iter().try_fold(0_usize, |n, e| {
            n.checked_add(match e {
                Edge::Line(..) => 2,
                Edge::Cubic(..) => 4,
            })
        })?;
        let total_points = points.checked_add(parent.as_ref().map_or(0, |p| p.total_points))?;
        Some(Self {
            source,
            parent,
            reason,
            depth,
            total_edges,
            total_points,
        })
    }
    pub(in crate::source::content_stream) fn depth(&self) -> usize {
        self.depth
    }
    pub(in crate::source::content_stream) fn reason(&self) -> Arc<str> {
        Arc::clone(&self.reason)
    }

    /// Only opposite strict witnesses certify a partial box. Sampling failure,
    /// arithmetic overflow and equal probe values preserve the original refusal.
    /// Actual shared-work exhaustion remains a resource-limit error.
    ///
    /// # Errors
    /// Returns the original unsupported reason if no complete certificate is
    /// affordable or established. Exhausting active work returns a resource limit.
    pub(in crate::source::content_stream) fn glyph_status(
        &self,
        glyph: Rect,
        crop: Option<Rect>,
        page: PageId,
        used: &mut usize,
        limit: usize,
    ) -> Result<GlyphPathClipStatus> {
        let held = || Error::Unsupported(self.reason.to_string());
        let points = self
            .total_points
            .checked_add(if crop.is_some() { 8 } else { 0 });
        let edges = self
            .total_edges
            .checked_add(if crop.is_some() { 4 } else { 0 });
        let metadata = self.depth.checked_mul(128).and_then(|v| v.checked_add(512));
        let minimum = points.zip(edges).zip(metadata).and_then(|((p, e), m)| {
            85_usize
                .checked_mul(256)?
                .checked_add(p.checked_mul(128)?)?
                .checked_add(e.checked_mul(16)?)?
                .checked_add(self.depth.checked_add(1)?.checked_mul(128)?)?
                .checked_add(m)
        });
        if minimum.is_none_or(|n| n > limit.saturating_sub(*used)) {
            return Err(held());
        }
        let mut budget = Budget::new(*used, limit);
        let result = (|| {
            budget.charge(metadata?)?;
            let rectangle = crop.map(|r| PreparedPath {
                edges: vec![
                    Edge::Line([r.min.x, r.min.y], [r.max.x, r.min.y]),
                    Edge::Line([r.max.x, r.min.y], [r.max.x, r.max.y]),
                    Edge::Line([r.max.x, r.max.y], [r.min.x, r.max.y]),
                    Edge::Line([r.min.x, r.max.y], [r.min.x, r.min.y]),
                ],
                even_odd: false,
            });
            let mut paths = Vec::with_capacity(self.depth + 1);
            let mut node = Some(self);
            while let Some(path) = node {
                paths.push(path.source.geometry(page)?);
                node = path.parent.as_deref();
            }
            if let Some(rectangle) = &rectangle {
                paths.push(rectangle);
            }
            partial(
                &paths,
                [glyph.min.x, glyph.min.y, glyph.max.x, glyph.max.y],
                8,
                &mut budget,
            )
        })();
        *used = budget.used;
        if budget.exhausted {
            return Err(Error::LimitExceeded {
                resource: "convex clipping work",
                limit,
            });
        }
        if matches!(result, Some((true, ..))) {
            Ok(GlyphPathClipStatus::PartiallyOutside)
        } else {
            Err(held())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::content::{ContentBudget, ContentLimits, ContentParser};
    fn source(bytes: &[u8]) -> Option<FrozenPath> {
        let mut parser = ContentParser::with_budgets(
            ContentLimits::default(),
            ContentBudget::for_operators(1000),
            ContentBudget::for_operands(10000),
        );
        let operations = parser.parse_fragment(bytes).ok()?;
        parser.finish().ok()?;
        let terminator = operations.last()?.index + 1;
        let even_odd = operations.last()?.operator == b"W*";
        FrozenPath::new(
            operations,
            Matrix::IDENTITY,
            SourceOrigin {
                stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                page: PageId(0),
                invocation: 1,
                first_operator: 0,
                terminator,
            },
            even_odd,
        )
    }
    #[test]
    fn curve_witness_does_not_replace_a_curved_loop_by_its_zero_chord() {
        let mut budget = Budget::new(0, 5_000_000);
        let curve = [[0, 0], [1024, 1536], [-1024, 1536], [0, 0]]
            .map(|p| shifted(p, [0, 512]).expect("small exact fixture offset"));
        assert_eq!(curve_winding(curve, 0, &mut budget), Some(1));
    }
    #[test]
    fn curve_witness_dyadics_preserve_ulps_and_reject_shift_overflow() {
        let x = 1.0_f64;
        assert_ne!(integer(x, -61), integer(x.next_up(), -61));
        assert!(integer(f64::MAX, -1074).is_none());
        assert!(integer(1.0, -127).is_none());
        let mut budget = Budget::new(0, 1000);
        assert!(orient([i128::MAX, 0], [0, i128::MAX], [-1, -1], &mut budget).is_none());
        assert!(!budget.exhausted);
    }
    #[test]
    fn curve_witness_source_rejects_open_balanced_or_foreign_operations() {
        assert!(source(b"0 0 m 2 3 -2 3 0 0 c h W").is_some());
        for bytes in [
            b"0 0 m 2 3 -2 3 1 0 c W".as_slice(),
            b"0 0 m 1 0 l 1 0 m 0 0 l W",
            b"0 0 m 2 3 -2 3 0 0 c 1 0 0 1 0 0 cm h W",
            b"0 m W",
            b"0 0 m 1 0 l W W",
        ] {
            assert!(source(bytes).is_none(), "{bytes:?}");
        }
    }
    #[test]
    fn curve_witness_never_infers_whole_status_from_equal_point_values() {
        let source = source(b"0 0 m 10 0 l 10 10 l 0 10 l h W").expect("closed rectangle fixture");
        for (bbox, expected) in [
            ([3., 3., 4., 4.], Some((false, 13, 0, 0))),
            ([20., 20., 21., 21.], None),
        ] {
            let mut budget = Budget::new(0, 5_000_000);
            assert_eq!(partial(&[&source.geometry], bbox, 2, &mut budget), expected);
        }
    }
    #[test]
    fn curve_witness_all_false_points_in_a_hole_are_not_an_outside_certificate() {
        for bytes in [
            b"0 0 10 10 re 2 2 m 2 8 l 8 8 l 8 2 l h W".as_slice(),
            b"0 0 10 10 re 2 2 6 6 re W*",
        ] {
            let source = source(bytes).expect("closed ring fixture");
            let mut budget = Budget::new(0, 5_000_000);
            assert_eq!(
                partial(&[&source.geometry], [3., 3., 4., 4.], 2, &mut budget),
                Some((false, 0, 13, 0))
            );
        }
    }

    #[test]
    fn curve_witness_active_exhaustion_keeps_the_shared_resource_error() {
        let source = Arc::new(
            source(b"40 40 m 140 40 l 140 140 l 40 140 40 40 40 40 c h W")
                .expect("closed cap fixture"),
        );
        let witness = PathWitness::new(source, None, Arc::from("original held clip"), 8)
            .expect("one source owner");
        let glyph = Rect {
            min: crate::model::Vec2 { x: 35.0, y: 72.0 },
            max: crate::model::Vec2 { x: 55.0, y: 112.0 },
        };
        // Preparation fits, but the third root edge cannot run without payment.
        let mut used = 0;
        assert!(matches!(
            witness.glyph_status(glyph, None, PageId(0), &mut used, 23_728),
            Err(Error::LimitExceeded {
                resource: "convex clipping work",
                limit: 23_728
            })
        ));
        assert_eq!(used, 23_728);
        let mut used = 0;
        assert_eq!(
            witness
                .glyph_status(glyph, None, PageId(0), &mut used, 5_000_000)
                .expect("strict opposite witnesses"),
            GlyphPathClipStatus::PartiallyOutside
        );
        assert!(used > 23_728);
        let mut used = 0;
        assert!(matches!(
            witness.glyph_status(glyph, None, PageId(1), &mut used, 5_000_000),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn curve_witness_budget_failure_is_distinct_from_arithmetic_refusal() {
        let mut budget = Budget::new(0, 0);
        assert!(
            curve_winding([[0, 0], [512, 512], [1024, 512], [1024, 0]], 0, &mut budget).is_none()
        );
        assert!(budget.exhausted);
        assert_eq!(budget.used, 0);
    }
    #[test]
    fn curve_witness_line_boundary_is_held_even_at_degenerate_vertices() {
        let mut budget = Budget::new(0, 1000);
        assert_eq!(line_winding([0, 0], [0, 0], &mut budget), None);
        assert_eq!(line_winding([0, -1], [0, 1], &mut budget), None);
    }
    #[test]
    fn curve_witness_retention_prices_capacities_and_origin_ownership() {
        let mut source = source(b"0 0 m 2 3 -2 3 0 0 c h W").expect("closed cubic fixture");
        let before =
            FrozenPath::retention_work(&source.operations).expect("bounded fixture storage");
        source.operations[1].operands.reserve(100);
        let after =
            FrozenPath::retention_work(&source.operations).expect("bounded fixture storage");
        assert!(after > before);
        assert!(source.geometry(PageId(1)).is_none());
        assert!(source.geometry(PageId(0)).is_some());
        let owner = Arc::new(source);
        let first = Arc::new(
            PathWitness::new(Arc::clone(&owner), None, Arc::from("held"), 1).expect("one owner"),
        );
        assert!(PathWitness::new(Arc::clone(&owner), Some(first), Arc::from("held"), 1).is_none());
        let source = owner;
        let mut operations = source.operations.clone();
        operations[1].index += 1;
        assert!(FrozenPath::new(operations, Matrix::IDENTITY, source.origin, false).is_none());
    }
    #[test]
    fn curve_witness_prepays_all_rectangle_edges_and_retained_capacity() {
        let source =
            source(b"0 0 1 1 re 2 2 1 1 re 4 4 1 1 re W").expect("three rectangles fixture");
        assert_eq!(FrozenPath::edge_capacity(&source.operations), Some(12));
        assert!(source.geometry.edges.capacity() >= 12);
        assert_eq!(
            FrozenPath::normalization_work(&source.operations),
            Some(4 * 256 + 12 * 256 + 1024)
        );
        assert!(
            FrozenPath::normalization_work(&source.operations)
                .expect("bounded fixture normalization")
                >= source.geometry.edges.capacity() * std::mem::size_of::<Edge>()
        );
    }
}
