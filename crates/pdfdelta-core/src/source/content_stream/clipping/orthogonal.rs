//! Exact coordinate arrangements, not sampled pixels or rounded midpoints.
use super::{
    Error, GlyphPathClipStatus, PathSegment, Rect, Result, Vec2, contains, disjoint, empty, inside,
    intersection,
};
use std::sync::{Arc, OnceLock};

/// Frozen source geometry and fill rule for one graphics-state owner. Only a
/// glyph demand activates its proof; passive drawing uncertainty is retained.
#[derive(Debug, PartialEq)]
pub(in crate::source::content_stream) struct PendingOrthogonal {
    segments: Vec<PathSegment>,
    even_odd: bool,
    reason: Arc<str>,
    proof: OnceLock<Option<Arc<OrthogonalGrid>>>,
}
impl PendingOrthogonal {
    pub(in crate::source::content_stream) fn retention_work(n: usize) -> Option<usize> {
        n.checked_mul(64)?.checked_add(512)
    }
    pub(in crate::source::content_stream) fn new(
        segments: &[PathSegment],
        even_odd: bool,
        reason: Arc<str>,
    ) -> Self {
        Self {
            segments: segments.to_vec(),
            even_odd,
            reason,
            proof: OnceLock::new(),
        }
    }
    pub(in crate::source::content_stream) fn segments(&self) -> &[PathSegment] {
        &self.segments
    }
    pub(in crate::source::content_stream) fn even_odd(&self) -> bool {
        self.even_odd
    }
    pub(in crate::source::content_stream) fn reason(&self) -> Arc<str> {
        Arc::clone(&self.reason)
    }
    pub(in crate::source::content_stream) fn cached(&self) -> Option<&Option<Arc<OrthogonalGrid>>> {
        self.proof.get()
    }
    pub(in crate::source::content_stream) fn cache(&self, value: Option<Arc<OrthogonalGrid>>) {
        let _ = self.proof.set(value);
    }
}

/// Open cell interiors have constant winding: every boundary coordinate is
/// retained. The caller must certify literal closure of each drawn subpath.
#[derive(Debug, PartialEq)]
pub(in crate::source::content_stream) struct OrthogonalGrid {
    xs: Vec<f64>,
    ys: Vec<f64>,
    filled: Vec<u8>,
}

impl OrthogonalGrid {
    /// Upper bound for edge validation/flow, insertion sorting and retained
    /// coordinate storage. Winding and cell storage are priced separately.
    /// Overflow means no affordable proof; no allocation or scan is attempted.
    pub(in crate::source::content_stream) fn preparation_work(n: usize) -> Option<usize> {
        n.checked_mul(n)?
            .checked_mul(128)?
            .checked_add(n.checked_mul(64)?)?
            .checked_add(512)
    }

    /// Called only after the complete preparation bound is prepaid and each source
    /// subpath is certified closed. A redundant directed-flow check protects
    /// this geometry constructor; it cannot replace that source certificate.
    pub(in crate::source::content_stream) fn prepare(
        segments: &[PathSegment],
    ) -> Option<PreparedOrthogonal<'_>> {
        if segments.is_empty()
            || segments.iter().any(|s| {
                ![s.from.x, s.from.y, s.to.x, s.to.y]
                    .into_iter()
                    .all(f64::is_finite)
                    || (s.from.x == s.to.x) == (s.from.y == s.to.y)
            })
        {
            return None;
        }
        for s in segments {
            let outgoing = segments.iter().filter(|t| t.from == s.from).count();
            let incoming = segments.iter().filter(|t| t.to == s.from).count();
            if outgoing != incoming {
                return None;
            }
        }
        fn coordinates(mut values: Vec<f64>) -> Vec<f64> {
            // Deterministic quadratic bound, without auxiliary sort storage.
            for i in 1..values.len() {
                let mut j = i;
                while j > 0 && values[j - 1] > values[j] {
                    values.swap(j - 1, j);
                    j -= 1;
                }
            }
            values.dedup_by(|a, b| *a == *b);
            values
        }
        let xs = coordinates(segments.iter().map(|s| s.from.x).collect());
        let ys = coordinates(segments.iter().map(|s| s.from.y).collect());
        Some(PreparedOrthogonal { segments, xs, ys })
    }

    #[cfg(test)]
    fn from_segments(segments: &[PathSegment], even_odd: bool) -> Option<Self> {
        Self::prepare(segments)?.finish(even_odd)
    }

    pub(in crate::source::content_stream) fn is_empty(&self) -> bool {
        !self.filled.contains(&1)
    }

    pub(in crate::source::content_stream) fn bounds(&self) -> Rect {
        Rect {
            min: Vec2 {
                x: self.xs[0],
                y: self.ys[0],
            },
            max: Vec2 {
                x: self.xs[self.xs.len() - 1],
                y: self.ys[self.ys.len() - 1],
            },
        }
    }

    /// Covers every cell's classification/intersection and fixed Arc/crop
    /// metadata. Coordinate capacity is at most the prepaid input edge count.
    pub(in crate::source::content_stream) fn work(&self) -> usize {
        self.filled.len().saturating_mul(64).saturating_add(128)
    }

    fn cells(&self) -> impl Iterator<Item = (Rect, bool)> + '_ {
        self.ys
            .windows(2)
            .flat_map(|y| {
                self.xs.windows(2).map(move |x| Rect {
                    min: Vec2 { x: x[0], y: y[0] },
                    max: Vec2 { x: x[1], y: y[1] },
                })
            })
            .zip(self.filled.iter().map(|&v| v != 0))
    }

    pub(in crate::source::content_stream) fn glyph_status(
        &self,
        glyph: Rect,
        crop: Rect,
    ) -> Result<GlyphPathClipStatus> {
        if empty(glyph) {
            return Err(Error::Unresolved(
                "degenerate glyph bounds for orthogonal clipping".into(),
            ));
        }
        let mut wholly_inside = contains(crop, glyph);
        let mut any_filled = false;
        for (cell, filled) in self.cells() {
            let cell = intersection(cell, crop);
            if empty(cell) || disjoint(glyph, cell) {
                continue;
            }
            if filled {
                any_filled = true;
            } else {
                wholly_inside = false;
            }
        }
        Ok(if !any_filled {
            GlyphPathClipStatus::Outside
        } else if wholly_inside {
            GlyphPathClipStatus::Inside
        } else {
            GlyphPathClipStatus::PartiallyOutside
        })
    }

    pub(in crate::source::content_stream) fn segment_is_visible(
        &self,
        segment: PathSegment,
        crop: Rect,
    ) -> bool {
        self.cells().any(|(cell, filled)| {
            let cell = intersection(cell, crop);
            filled && !empty(cell) && inside(segment.from, cell) && inside(segment.to, cell)
        })
    }
}

/// Coordinates validated and owned after the first prepaid phase. The exact
/// cell and crossing populations are known before the second phase allocates
/// cells or evaluates winding. No candidate population is discarded.
pub(in crate::source::content_stream) struct PreparedOrthogonal<'a> {
    segments: &'a [PathSegment],
    xs: Vec<f64>,
    ys: Vec<f64>,
}
impl PreparedOrthogonal<'_> {
    pub(in crate::source::content_stream) fn finish_work(&self) -> Option<usize> {
        let n = self.segments.len();
        let cells = self
            .xs
            .len()
            .saturating_sub(1)
            .checked_mul(self.ys.len().saturating_sub(1))?;
        cells
            .checked_mul(n)?
            .checked_mul(16)?
            .checked_add(cells.checked_mul(64)?)?
            .checked_add(128)
    }
    pub(in crate::source::content_stream) fn finish(
        self,
        even_odd: bool,
    ) -> Option<OrthogonalGrid> {
        let mut filled =
            Vec::with_capacity(self.xs.len().saturating_sub(1) * self.ys.len().saturating_sub(1));
        for y in self.ys.windows(2) {
            for x in self.xs.windows(2) {
                let mut winding = 0_i64;
                for s in self.segments {
                    // Every edge coordinate is a cell boundary. This entire
                    // open interval is on one side of each edge: no midpoint,
                    // subtraction, epsilon, area sum or numeric ray sample.
                    if s.from.x == s.to.x
                        && s.from.x >= x[1]
                        && s.from.y.min(s.to.y) <= y[0]
                        && s.from.y.max(s.to.y) >= y[1]
                    {
                        winding = winding.checked_add(if s.to.y > s.from.y { 1 } else { -1 })?;
                    }
                }
                filled.push(u8::from(if even_odd {
                    winding % 2 != 0
                } else {
                    winding != 0
                }));
            }
        }
        Some(OrthogonalGrid {
            xs: self.xs,
            ys: self.ys,
            filled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(x: f64, y: f64) -> Vec2 {
        Vec2 { x, y }
    }
    fn r(x: f64, y: f64, xx: f64, yy: f64) -> Rect {
        Rect {
            min: p(x, y),
            max: p(xx, yy),
        }
    }
    fn ring(points: &[(f64, f64)]) -> Vec<PathSegment> {
        (0..points.len())
            .map(|i| PathSegment {
                from: p(points[i].0, points[i].1),
                to: p(
                    points[(i + 1) % points.len()].0,
                    points[(i + 1) % points.len()].1,
                ),
            })
            .collect()
    }
    fn square() -> Vec<PathSegment> {
        ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)])
    }

    #[test]
    fn orthogonal_cells_preserve_winding_parity_holes_and_coincident_edges() -> Result<()> {
        let outer = square();
        let inner = ring(&[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        for even_odd in [false, true] {
            let mut edges = outer.clone();
            edges.extend(inner.clone());
            let grid =
                OrthogonalGrid::from_segments(&edges, even_odd).expect("closed orthogonal rings");
            assert_eq!(
                grid.glyph_status(r(1.2, 1.2, 2.8, 2.8), grid.bounds())?,
                if even_odd {
                    GlyphPathClipStatus::Outside
                } else {
                    GlyphPathClipStatus::Inside
                }
            );
            let mut reversed = outer.clone();
            reversed.extend(inner.iter().rev().map(|s| PathSegment {
                from: s.to,
                to: s.from,
            }));
            let hole =
                OrthogonalGrid::from_segments(&reversed, even_odd).expect("opposite winding");
            assert_eq!(
                hole.glyph_status(r(1.2, 1.2, 2.8, 2.8), hole.bounds())?,
                GlyphPathClipStatus::Outside
            );
            assert_eq!(
                hole.glyph_status(r(0.2, 0.2, 0.8, 0.8), hole.bounds())?,
                GlyphPathClipStatus::Inside
            );
            assert!(!hole.segment_is_visible(
                PathSegment {
                    from: p(0.5, 2.0),
                    to: p(3.5, 2.0)
                },
                hole.bounds()
            ));
        }
        let mut double = outer.clone();
        double.extend(outer);
        assert!(
            OrthogonalGrid::from_segments(&double, true)
                .expect("coincident rings")
                .is_empty()
        );
        assert!(
            !OrthogonalGrid::from_segments(&double, false)
                .expect("same winding")
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn orthogonal_cells_match_independent_union_oracle_with_overlapping_bounds() -> Result<()> {
        // Stepped notch plus a detached island inside its outer bounds.
        let mut edges = ring(&[
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 3.0),
            (2.0, 3.0),
            (2.0, 2.0),
            (3.0, 2.0),
            (3.0, 0.0),
            (4.0, 0.0),
            (4.0, 4.0),
            (0.0, 4.0),
        ]);
        edges.extend(ring(&[(1.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0)]));
        for rule in [false, true] {
            let grid = OrthogonalGrid::from_segments(&edges, rule).expect("coordinate arrangement");
            for x0 in -1..5 {
                for x1 in x0 + 1..=5 {
                    for y0 in -1..5 {
                        for y1 in y0 + 1..=5 {
                            let mut count = 0;
                            for x in x0..x1 {
                                for y in y0..y1 {
                                    let included = (0..4).contains(&x)
                                        && (0..4).contains(&y)
                                        && (x == 0
                                            || x == 3
                                            || y >= 3
                                            || (x == 2 && y >= 2)
                                            || (x == 1 && y == 0));
                                    count += i32::from(included);
                                }
                            }
                            let expected = if count == 0 {
                                GlyphPathClipStatus::Outside
                            } else if count == (x1 - x0) * (y1 - y0) {
                                GlyphPathClipStatus::Inside
                            } else {
                                GlyphPathClipStatus::PartiallyOutside
                            };
                            assert_eq!(
                                grid.glyph_status(
                                    r(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1)),
                                    grid.bounds()
                                )?,
                                expected
                            );
                        }
                    }
                }
            }
            assert_eq!(
                grid.glyph_status(r(0.2, 0.2, 0.8, 0.8), r(2.0, 0.0, 4.0, 4.0))?,
                GlyphPathClipStatus::Outside
            );
            assert!(
                grid.glyph_status(r(1.0, 1.0, 1.0, 2.0), grid.bounds())
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn orthogonal_cells_reject_invalid_edges_and_bound_proof_metadata() {
        assert!(OrthogonalGrid::preparation_work(usize::MAX).is_none());
        let mut open = square();
        open.pop();
        assert!(OrthogonalGrid::from_segments(&open, false).is_none());
        let mut skew = square();
        skew[0].to.y = skew[0].to.y.next_up();
        assert!(OrthogonalGrid::from_segments(&skew, false).is_none());
        let mut nan = square();
        nan[0].from.x = f64::NAN;
        assert!(OrthogonalGrid::from_segments(&nan, false).is_none());
        let right = 1.0_f64.next_up();
        let thin = ring(&[(1.0, 0.0), (right, 0.0), (right, 1.0), (1.0, 1.0)]);
        let thin = OrthogonalGrid::from_segments(&thin, false).expect("one-ULP strip");
        assert_eq!(
            thin.glyph_status(r(1.0, 0.25, right, 0.75), thin.bounds())
                .expect("finite box"),
            GlyphPathClipStatus::Inside
        );
        let huge = ring(&[
            (-f64::MAX, 0.0),
            (f64::MAX, 0.0),
            (f64::MAX, 1.0),
            (-f64::MAX, 1.0),
        ]);
        let huge = OrthogonalGrid::from_segments(&huge, false).expect("no overflowing midpoint");
        assert_eq!(
            huge.glyph_status(r(-1.0, 0.25, 1.0, 0.75), huge.bounds())
                .expect("finite box"),
            GlyphPathClipStatus::Inside
        );
        let grid = OrthogonalGrid::from_segments(&square(), false).expect("one cell");
        assert!(
            grid.work() >= grid.filled.capacity() + 8 * (grid.xs.capacity() + grid.ys.capacity())
        );
        assert!(OrthogonalGrid::preparation_work(4).expect("small bound") > grid.work());
    }
}
