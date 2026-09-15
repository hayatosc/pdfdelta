//! Convex clips retain their original half-planes rather than rounded intersection
//! vertices. Uncertain arithmetic never certifies glyph visibility or absence.

use std::{cmp::Ordering, sync::Arc};

use super::{Error, GlyphPathClipStatus, PathSegment, Rect, Result, Vec2, bounding_rect};

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum ClipRegion {
    #[default]
    Unbounded,
    Rectangle(Rect),
    Convex(Arc<ConvexClip>),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ConvexClip {
    bounds: Rect,
    quads: Vec<Quad>,
}

/// Four counterclockwise vertices with strictly convex turns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Quad([Vec2; 4]);

impl Quad {
    pub(super) fn new(mut points: [Vec2; 4]) -> Option<Self> {
        let turns = std::array::from_fn::<_, 4, _>(|i| {
            side(points[i], points[(i + 1) % 4], points[(i + 2) % 4])
        });
        if turns.iter().all(|turn| *turn == Some(Ordering::Less)) {
            points.reverse();
        } else if !turns.iter().all(|turn| *turn == Some(Ordering::Greater)) {
            return None;
        }
        Some(Self(points))
    }

    fn rectangle(rect: Rect) -> Self {
        Self(corners(rect))
    }

    fn edges(self) -> impl Iterator<Item = (Vec2, Vec2)> {
        (0..4).map(move |i| (self.0[i], self.0[(i + 1) % 4]))
    }
}

impl ClipRegion {
    /// Bounds both retained-constraint copying and four-corner classification.
    /// The caller charges this against a document-wide extraction work ceiling.
    pub(super) fn work(&self) -> usize {
        match self {
            Self::Convex(clip) => clip.quads.len().saturating_mul(16),
            _ => 0,
        }
    }

    pub(super) fn intersect_rectangle(&self, rectangle: Rect) -> Self {
        if empty(rectangle) {
            return Self::Empty;
        }
        match self {
            Self::Unbounded => Self::Rectangle(rectangle),
            Self::Empty => Self::Empty,
            Self::Rectangle(current) => {
                let bounds = intersection(*current, rectangle);
                if empty(bounds) {
                    Self::Empty
                } else {
                    Self::Rectangle(bounds)
                }
            }
            Self::Convex(_) => self.intersect_quad(Quad::rectangle(rectangle)),
        }
    }

    pub(super) fn intersect_quad(&self, quad: Quad) -> Self {
        let bounds = bounding_rect(quad.0);
        let (bounds, mut quads) = match self {
            Self::Empty => return Self::Empty,
            Self::Unbounded => (bounds, Vec::new()),
            Self::Rectangle(rectangle) => {
                let bounds = intersection(bounds, *rectangle);
                if empty(bounds) {
                    return Self::Empty;
                }
                (bounds, vec![Quad::rectangle(*rectangle)])
            }
            Self::Convex(clip) => {
                let bounds = intersection(bounds, clip.bounds);
                if empty(bounds) {
                    return Self::Empty;
                }
                (bounds, clip.quads.clone())
            }
        };
        quads.push(quad);
        Self::Convex(Arc::new(ConvexClip { bounds, quads }))
    }

    pub(super) fn segment_is_visible(&self, segment: PathSegment) -> bool {
        match self {
            Self::Unbounded => true,
            Self::Empty => false,
            Self::Rectangle(rect) => inside(segment.from, *rect) && inside(segment.to, *rect),
            Self::Convex(clip) => [segment.from, segment.to].into_iter().all(|point| {
                clip.quads
                    .iter()
                    .flat_map(|quad| quad.edges())
                    .all(|(a, b)| {
                        matches!(side(a, b, point), Some(Ordering::Equal | Ordering::Greater))
                    })
            }),
        }
    }

    pub(super) fn glyph_status(&self, glyph: Rect) -> Result<GlyphPathClipStatus> {
        match self {
            Self::Unbounded => Ok(GlyphPathClipStatus::Unclipped),
            Self::Empty => Ok(GlyphPathClipStatus::Outside),
            Self::Rectangle(clip) => Ok(rectangle_status(glyph, *clip)),
            Self::Convex(clip) => clip.glyph_status(glyph),
        }
    }
}

impl ConvexClip {
    fn glyph_status(&self, glyph: Rect) -> Result<GlyphPathClipStatus> {
        if disjoint(glyph, self.bounds) {
            return Ok(GlyphPathClipStatus::Outside);
        }
        let points = corners(glyph);
        let mut all_inside = true;
        let mut strictly_inside = [true; 4];
        let mut any_outside = false;
        for (a, b) in self.quads.iter().flat_map(|quad| quad.edges()) {
            let signs = points.map(|point| side(a, b, point));
            if signs
                .iter()
                .all(|sign| matches!(sign, Some(Ordering::Less | Ordering::Equal)))
            {
                return Ok(GlyphPathClipStatus::Outside);
            }
            for (i, sign) in signs.into_iter().enumerate() {
                match sign {
                    Some(Ordering::Greater) => {}
                    Some(Ordering::Equal) => strictly_inside[i] = false,
                    Some(Ordering::Less) => {
                        strictly_inside[i] = false;
                        all_inside = false;
                        any_outside = true;
                    }
                    None => {
                        strictly_inside[i] = false;
                        all_inside = false;
                    }
                }
            }
        }
        if all_inside {
            Ok(GlyphPathClipStatus::Inside)
        } else if any_outside && strictly_inside.into_iter().any(|inside| inside) {
            // A strict interior corner and an exterior corner certify that the
            // glyph rectangle has both included and excluded portions.
            Ok(GlyphPathClipStatus::PartiallyOutside)
        } else {
            Err(Error::Unresolved(
                "glyph relationship to convex clipping region is uncertain".into(),
            ))
        }
    }
}

fn corners(rect: Rect) -> [Vec2; 4] {
    [
        rect.min,
        Vec2 {
            x: rect.max.x,
            y: rect.min.y,
        },
        rect.max,
        Vec2 {
            x: rect.min.x,
            y: rect.max.y,
        },
    ]
}

fn empty(rect: Rect) -> bool {
    rect.max.x <= rect.min.x || rect.max.y <= rect.min.y
}

fn intersection(a: Rect, b: Rect) -> Rect {
    Rect {
        min: Vec2 {
            x: a.min.x.max(b.min.x),
            y: a.min.y.max(b.min.y),
        },
        max: Vec2 {
            x: a.max.x.min(b.max.x),
            y: a.max.y.min(b.max.y),
        },
    }
}

fn inside(point: Vec2, rect: Rect) -> bool {
    point.x >= rect.min.x && point.x <= rect.max.x && point.y >= rect.min.y && point.y <= rect.max.y
}

fn rectangle_status(glyph: Rect, clip: Rect) -> GlyphPathClipStatus {
    if disjoint(glyph, clip) {
        GlyphPathClipStatus::Outside
    } else if inside(glyph.min, clip) && inside(glyph.max, clip) {
        GlyphPathClipStatus::Inside
    } else {
        GlyphPathClipStatus::PartiallyOutside
    }
}

fn disjoint(glyph: Rect, clip: Rect) -> bool {
    glyph.max.x <= clip.min.x
        || glyph.min.x >= clip.max.x
        || glyph.max.y <= clip.min.y
        || glyph.min.y >= clip.max.y
}

// Each arithmetic step is rounded outwards. A determinant interval containing
// zero is uncertain, except for collinearity established without arithmetic.
fn side(a: Vec2, b: Vec2, p: Vec2) -> Option<Ordering> {
    if ![a.x, a.y, b.x, b.y, p.x, p.y]
        .into_iter()
        .all(f64::is_finite)
    {
        return None;
    }
    if p == a || p == b || (a.x == b.x && p.x == a.x) || (a.y == b.y && p.y == a.y) {
        return Some(Ordering::Equal);
    }
    let left = multiply(difference(b.x, a.x)?, difference(p.y, a.y)?)?;
    let right = multiply(difference(b.y, a.y)?, difference(p.x, a.x)?)?;
    let low = (left.0 - right.1).next_down();
    let high = (left.1 - right.0).next_up();
    if !low.is_finite() || !high.is_finite() {
        return None;
    }
    if low > 0.0 {
        Some(Ordering::Greater)
    } else if high < 0.0 {
        Some(Ordering::Less)
    } else {
        None
    }
}

fn difference(a: f64, b: f64) -> Option<(f64, f64)> {
    let value = a - b;
    let range = (value.next_down(), value.next_up());
    (range.0.is_finite() && range.1.is_finite()).then_some(range)
}

fn multiply(a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
    let products = [a.0 * b.0, a.0 * b.1, a.1 * b.0, a.1 * b.1];
    if !products.into_iter().all(f64::is_finite) {
        return None;
    }
    let low = products
        .into_iter()
        .fold(f64::INFINITY, f64::min)
        .next_down();
    let high = products
        .into_iter()
        .fold(f64::NEG_INFINITY, f64::max)
        .next_up();
    (low.is_finite() && high.is_finite()).then_some((low, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f64, y: f64) -> Vec2 {
        Vec2 { x, y }
    }
    fn rect(x: f64, y: f64, xx: f64, yy: f64) -> Rect {
        Rect {
            min: point(x, y),
            max: point(xx, yy),
        }
    }
    fn diamond(x: f64) -> Quad {
        Quad::new([
            point(x - 4.0, 0.0),
            point(x, -4.0),
            point(x + 4.0, 0.0),
            point(x, 4.0),
        ])
        .expect("strict convex diamond")
    }

    #[test]
    fn convex_intersections_keep_every_half_plane() -> Result<()> {
        let first = ClipRegion::Unbounded.intersect_quad(diamond(0.0));
        let second = first.intersect_quad(diamond(4.0));
        assert_eq!(
            second.glyph_status(rect(1.8, -0.2, 2.2, 0.2))?,
            GlyphPathClipStatus::Inside
        );
        assert_eq!(
            second.glyph_status(rect(-2.1, -0.1, -1.9, 0.1))?,
            GlyphPathClipStatus::Outside
        );
        assert_eq!(
            first.glyph_status(rect(-2.1, -0.1, -1.9, 0.1))?,
            GlyphPathClipStatus::Inside
        );
        let third = second.intersect_rectangle(rect(2.0, -1.0, 3.0, 1.0));
        assert_eq!(
            third.glyph_status(rect(1.8, -0.2, 2.2, 0.2))?,
            GlyphPathClipStatus::PartiallyOutside
        );
        let empty = third.intersect_rectangle(rect(10.0, 10.0, 20.0, 20.0));
        assert_eq!(empty, ClipRegion::Empty);
        assert_eq!(empty.intersect_quad(diamond(0.0)), ClipRegion::Empty);
        Ok(())
    }

    #[test]
    fn uncertain_boundary_and_unproved_overlap_remain_unresolved() {
        assert_eq!(
            side(point(0.0, 0.0), point(1.0, 1.0), point(0.5, 0.5)),
            None
        );
        assert_eq!(
            side(point(-f64::MAX, 0.0), point(f64::MAX, 1.0), point(0.0, 1.0)),
            None
        );
        let clip = ClipRegion::Unbounded.intersect_quad(diamond(0.0));
        assert!(matches!(
            clip.glyph_status(rect(-10.0, -10.0, 10.0, 10.0)),
            Err(Error::Unresolved(_))
        ));
        assert!(!clip.segment_is_visible(PathSegment {
            from: point(2.0, 2.0),
            to: point(3.0, 1.0)
        }));
    }

    #[test]
    fn rejects_concave_crossed_degenerate_and_nonfinite_quads() {
        for points in [
            [
                point(0.0, 0.0),
                point(4.0, 0.0),
                point(1.0, 1.0),
                point(0.0, 4.0),
            ],
            [
                point(0.0, 0.0),
                point(4.0, 4.0),
                point(0.0, 4.0),
                point(4.0, 0.0),
            ],
            [
                point(0.0, 0.0),
                point(1.0, 1.0),
                point(2.0, 2.0),
                point(3.0, 3.0),
            ],
            [
                point(f64::INFINITY, 0.0),
                point(1.0, 1.0),
                point(2.0, 2.0),
                point(3.0, 3.0),
            ],
        ] {
            assert!(Quad::new(points).is_none());
        }
    }

    #[test]
    fn rectangle_clips_preserve_zero_width_glyph_classification() -> Result<()> {
        let clip = ClipRegion::Rectangle(rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(
            clip.glyph_status(rect(5.0, 2.0, 5.0, 3.0))?,
            GlyphPathClipStatus::Inside
        );
        Ok(())
    }
}
