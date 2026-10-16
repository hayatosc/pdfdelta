//! Convex clips retain their original half-planes rather than rounded intersection
//! vertices. Uncertain arithmetic never certifies glyph visibility or absence.

use std::{cmp::Ordering, sync::Arc};

mod orthogonal;
pub(super) use orthogonal::{OrthogonalGrid, PendingOrthogonal};

use super::{Error, GlyphPathClipStatus, PathSegment, Rect, Result, Vec2, bounding_rect};

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum ClipRegion {
    #[default]
    Unbounded,
    Rectangle(Rect),
    /// Two strictly separated rectangles; the gap remains excluded.
    RectanglePair([Rect; 2]),
    /// Exact rectangular set difference with its certified cutout retained.
    RectangularCutout(RectangularCutout),
    /// Two strictly separated components, retaining the corner and the gap.
    SeparatedCutout(Arc<SeparatedCutout>),
    PendingOrthogonal {
        pending: Arc<PendingOrthogonal>,
        crop: Option<Rect>,
    },
    Orthogonal {
        grid: Arc<OrthogonalGrid>,
        crop: Rect,
    },
    Convex(Arc<ConvexClip>),
    Empty,
    Unsupported(Arc<str>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ConvexClip {
    bounds: Rect,
    quads: Vec<Quad>,
}

/// A rectangular region with a certified rectangular cutout removed. Strict
/// six-vertex corner and eight-vertex edge-notch constructors certify the
/// boundary. Rectangular intersections retain the exact set difference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RectangularCutout {
    bounds: Rect,
    removed: Rect,
}

/// A rectangle and a certified L boundary whose outer bounds are strictly
/// separated. Both fill rules therefore agree regardless of winding.
/// The fixed allocation keeps the graphics-state enum payload unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SeparatedCutout {
    rectangle: Rect,
    corner: RectangularCutout,
}

impl SeparatedCutout {
    /// Prepaid bound for the L proof, separation and fixed allocation/copy.
    /// The caller also retains the ordinary compound-path proof charge.
    pub(super) const PROOF_WORK: usize = RectangularCutout::PROOF_WORK + 128;

    pub(super) fn new(rectangle: Rect, corner: RectangularCutout) -> Option<Self> {
        ([
            rectangle.min.x,
            rectangle.min.y,
            rectangle.max.x,
            rectangle.max.y,
        ]
        .into_iter()
        .all(f64::is_finite)
            && !empty(rectangle)
            && separated(rectangle, corner.bounds))
        .then_some(Self { rectangle, corner })
    }

    fn intersect_rectangle(self, crop: Rect) -> ClipRegion {
        let rectangle = intersection(self.rectangle, crop);
        let corner = self.corner.intersect_rectangle(crop);
        if empty(rectangle) {
            return corner;
        }
        match corner {
            ClipRegion::Empty => ClipRegion::Rectangle(rectangle),
            ClipRegion::Rectangle(other) => rectangle_pair([rectangle, other]),
            ClipRegion::RectangularCutout(corner) => {
                ClipRegion::SeparatedCutout(Arc::new(Self { rectangle, corner }))
            }
            // Corner/rectangle intersection has no other result. Retain
            // uncertainty if its contract is ever extended.
            _ => ClipRegion::Unsupported("uncertified separated corner intersection".into()),
        }
    }

    fn glyph_status(self, glyph: Rect) -> Result<GlyphPathClipStatus> {
        let a = rectangle_status(glyph, self.rectangle);
        let b = self.corner.glyph_status(glyph)?;
        Ok(if [a, b].contains(&GlyphPathClipStatus::Inside) {
            GlyphPathClipStatus::Inside
        } else if a == GlyphPathClipStatus::Outside && b == GlyphPathClipStatus::Outside {
            GlyphPathClipStatus::Outside
        } else {
            // A connected box meeting both strictly separated outer bounds
            // also crosses positive excluded area between the components.
            GlyphPathClipStatus::PartiallyOutside
        })
    }

    fn segment_is_visible(self, segment: PathSegment) -> bool {
        (inside(segment.from, self.rectangle) && inside(segment.to, self.rectangle))
            || self.corner.segment_is_visible(segment)
    }
}

impl RectangularCutout {
    /// Prepaid bound for finite/axis tests, extrema, corner membership and all
    /// twelve cyclic boundary comparisons (at most 72 vertex comparisons),
    /// including fixed metadata construction. No candidate-dependent work is
    /// performed before the caller pays this existing clipping-work budget.
    pub(super) const PROOF_WORK: usize = 512;

    /// Includes finite/axis tests, extrema, two interior vertices, optional
    /// axis transposition, both possible notch sides and all sixteen cyclic
    /// boundary comparisons per side (at most 256 vertex comparisons), plus
    /// the caller's literal closure checks and fixed metadata construction.
    pub(super) const EDGE_PROOF_WORK: usize = 1024;

    /// Certifies one simple eight-edge boundary with a rectangular notch
    /// strictly inside exactly one outer edge. No epsilon, orientation
    /// arithmetic or bounding-box replacement certifies the boundary.
    /// The caller prepays [`Self::EDGE_PROOF_WORK`].
    pub(super) fn from_edge_notch(mut points: [Vec2; 8]) -> Option<Self> {
        if !points.iter().all(|p| p.x.is_finite() && p.y.is_finite())
            || !(0..8).all(|i| {
                let next = points[(i + 1) % 8];
                (points[i].x == next.x) != (points[i].y == next.y)
            })
        {
            return None;
        }
        let mut bounds = Rect {
            min: points[0],
            max: points[0],
        };
        for p in &points[1..] {
            bounds.min.x = bounds.min.x.min(p.x);
            bounds.min.y = bounds.min.y.min(p.y);
            bounds.max.x = bounds.max.x.max(p.x);
            bounds.max.y = bounds.max.y.max(p.y);
        }
        let original_bounds = bounds;
        let mut interiors = [None; 2];
        let mut count = 0;
        for p in points {
            if bounds.min.x < p.x && p.x < bounds.max.x && bounds.min.y < p.y && p.y < bounds.max.y
            {
                if count == 2 {
                    return None;
                }
                interiors[count] = Some(p);
                count += 1;
            }
        }
        let [Some(mut a), Some(mut b)] = interiors else {
            return None;
        };
        fn transpose(p: Vec2) -> Vec2 {
            Vec2 { x: p.y, y: p.x }
        }
        let transposed = if a.y == b.y {
            false
        } else if a.x == b.x {
            true
        } else {
            return None;
        };
        if transposed {
            points = points.map(transpose);
            a = transpose(a);
            b = transpose(b);
            bounds = Rect {
                min: transpose(bounds.min),
                max: transpose(bounds.max),
            };
        }
        let (left, right) = (a.x.min(b.x), a.x.max(b.x));
        if left >= right {
            return None;
        }
        for base in [bounds.min.y, bounds.max.y] {
            let far = if base == bounds.min.y {
                bounds.max.y
            } else {
                bounds.min.y
            };
            let expected = [
                Vec2 {
                    x: bounds.min.x,
                    y: base,
                },
                Vec2 { x: left, y: base },
                Vec2 { x: left, y: a.y },
                Vec2 { x: right, y: a.y },
                Vec2 { x: right, y: base },
                Vec2 {
                    x: bounds.max.x,
                    y: base,
                },
                Vec2 {
                    x: bounds.max.x,
                    y: far,
                },
                Vec2 {
                    x: bounds.min.x,
                    y: far,
                },
            ];
            let matches = [false, true].into_iter().any(|reverse| {
                (0..8).any(|shift| {
                    (0..8).all(|i| {
                        expected[i] == points[(shift + if reverse { 8 - i } else { i }) % 8]
                    })
                })
            });
            if matches {
                let removed = Rect {
                    min: Vec2 {
                        x: left,
                        y: base.min(a.y),
                    },
                    max: Vec2 {
                        x: right,
                        y: base.max(a.y),
                    },
                };
                return Some(Self {
                    bounds: original_bounds,
                    removed: if transposed {
                        Rect {
                            min: transpose(removed.min),
                            max: transpose(removed.max),
                        }
                    } else {
                        removed
                    },
                });
            }
        }
        None
    }

    /// Certifies a simple finite six-edge axis-aligned boundary in either
    /// winding and at any cyclic start. The caller prepays [`Self::PROOF_WORK`].
    pub(super) fn from_l_shape(points: [Vec2; 6]) -> Option<Self> {
        if !points
            .iter()
            .all(|point| point.x.is_finite() && point.y.is_finite())
            || !(0..6).all(|i| {
                let next = points[(i + 1) % 6];
                (points[i].x == next.x) != (points[i].y == next.y)
            })
        {
            return None;
        }
        let mut bounds = Rect {
            min: points[0],
            max: points[0],
        };
        for point in &points[1..] {
            bounds.min.x = bounds.min.x.min(point.x);
            bounds.min.y = bounds.min.y.min(point.y);
            bounds.max.x = bounds.max.x.max(point.x);
            bounds.max.y = bounds.max.y.max(point.y);
        }
        let mut inner = None;
        for point in points {
            if bounds.min.x < point.x
                && point.x < bounds.max.x
                && bounds.min.y < point.y
                && point.y < bounds.max.y
                && inner.replace(point).is_some()
            {
                return None;
            }
        }
        let inner = inner?;
        let mut missing = None;
        for corner in corners(bounds) {
            if !points.contains(&corner) && missing.replace(corner).is_some() {
                return None;
            }
        }
        let missing = missing?;
        let opposite_x = if missing.x == bounds.min.x {
            bounds.max.x
        } else {
            bounds.min.x
        };
        let opposite_y = if missing.y == bounds.min.y {
            bounds.max.y
        } else {
            bounds.min.y
        };
        let expected = [
            Vec2 {
                x: opposite_x,
                y: opposite_y,
            },
            Vec2 {
                x: missing.x,
                y: opposite_y,
            },
            Vec2 {
                x: missing.x,
                y: inner.y,
            },
            inner,
            Vec2 {
                x: inner.x,
                y: missing.y,
            },
            Vec2 {
                x: opposite_x,
                y: missing.y,
            },
        ];
        let cyclic = [false, true].into_iter().any(|reverse| {
            (0..6).any(|shift| {
                (0..6).all(|i| expected[i] == points[(shift + if reverse { 6 - i } else { i }) % 6])
            })
        });
        cyclic.then_some(Self {
            bounds,
            removed: Rect {
                min: Vec2 {
                    x: inner.x.min(missing.x),
                    y: inner.y.min(missing.y),
                },
                max: Vec2 {
                    x: inner.x.max(missing.x),
                    y: inner.y.max(missing.y),
                },
            },
        })
    }

    fn intersect_rectangle(self, rectangle: Rect) -> ClipRegion {
        let bounds = intersection(self.bounds, rectangle);
        if empty(bounds) {
            return ClipRegion::Empty;
        }
        let removed = intersection(self.removed, bounds);
        if empty(removed) {
            return ClipRegion::Rectangle(bounds);
        }
        if contains(removed, bounds) {
            return ClipRegion::Empty;
        }
        ClipRegion::RectangularCutout(Self { bounds, removed })
    }

    fn glyph_status(self, glyph: Rect) -> Result<GlyphPathClipStatus> {
        if empty(glyph) {
            return Err(Error::Unresolved(
                "degenerate glyph bounds for concave clipping".into(),
            ));
        }
        let retained = intersection(self.bounds, glyph);
        if empty(retained) || contains(self.removed, retained) {
            Ok(GlyphPathClipStatus::Outside)
        } else if contains(self.bounds, glyph) && disjoint(glyph, self.removed) {
            Ok(GlyphPathClipStatus::Inside)
        } else {
            // The retained rectangle has positive area outside the cutout.
            // Failing whole inclusion also proves positive excluded area.
            Ok(GlyphPathClipStatus::PartiallyOutside)
        }
    }

    fn segment_is_visible(self, segment: PathSegment) -> bool {
        // These four exact strips partition the rectangular set difference.
        // A segment spanning strips is conservatively held: endpoints alone
        // cannot certify visibility across a concave corner.
        let b = self.bounds;
        let r = self.removed;
        let parts = [
            Rect {
                min: b.min,
                max: Vec2 {
                    x: r.min.x,
                    y: b.max.y,
                },
            },
            Rect {
                min: Vec2 {
                    x: r.max.x,
                    y: b.min.y,
                },
                max: b.max,
            },
            Rect {
                min: Vec2 {
                    x: r.min.x,
                    y: b.min.y,
                },
                max: Vec2 {
                    x: r.max.x,
                    y: r.min.y,
                },
            },
            Rect {
                min: Vec2 {
                    x: r.min.x,
                    y: r.max.y,
                },
                max: Vec2 {
                    x: r.max.x,
                    y: b.max.y,
                },
            },
        ];
        parts
            .into_iter()
            .any(|part| !empty(part) && inside(segment.from, part) && inside(segment.to, part))
    }
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
    /// Keep graphics-state uncertainty until a glyph needs its visibility.
    /// An already empty intersection remains provably empty.
    pub(super) fn intersect_unsupported(&self, reason: String) -> Self {
        match self {
            Self::Empty | Self::Unsupported(_) => self.clone(),
            _ => Self::Unsupported(reason.into()),
        }
    }

    /// Bounds both retained-constraint copying and four-corner classification.
    /// The caller charges this against a document-wide extraction work ceiling.
    pub(super) fn work(&self) -> usize {
        match self {
            Self::Convex(clip) => clip.quads.len().saturating_mul(16),
            Self::RectanglePair(_) => 32,
            // Bounds classification, fixed copying and the conservative
            // four-strip segment proof under the same shared ceiling.
            Self::RectangularCutout(_) => 64,
            Self::Orthogonal { grid, .. } => grid.work(),
            Self::PendingOrthogonal { .. } => 0,
            // Two classifications, retained constraints and bounded allocation
            // on rectangular intersection share the existing work ceiling.
            Self::SeparatedCutout(_) => 192,
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
            Self::Unsupported(_) => self.clone(),
            Self::Rectangle(current) => {
                let bounds = intersection(*current, rectangle);
                if empty(bounds) {
                    Self::Empty
                } else {
                    Self::Rectangle(bounds)
                }
            }
            Self::Convex(_) => self.intersect_quad(Quad::rectangle(rectangle)),
            Self::RectanglePair(parts) => {
                rectangle_pair(parts.map(|part| intersection(part, rectangle)))
            }
            Self::RectangularCutout(clip) => clip.intersect_rectangle(rectangle),
            Self::SeparatedCutout(clip) => clip.intersect_rectangle(rectangle),
            Self::PendingOrthogonal { pending, crop } => {
                let crop = crop.map_or(rectangle, |old| intersection(old, rectangle));
                if empty(crop) {
                    Self::Empty
                } else {
                    Self::PendingOrthogonal {
                        pending: Arc::clone(pending),
                        crop: Some(crop),
                    }
                }
            }
            Self::Orthogonal { grid, crop } => {
                let crop = intersection(*crop, rectangle);
                if empty(crop) {
                    Self::Empty
                } else {
                    Self::Orthogonal {
                        grid: Arc::clone(grid),
                        crop,
                    }
                }
            }
        }
    }

    /// Accepts only separated components under a rectangular prior clip.
    /// Other compositions retain uncertainty instead of approximating a union.
    pub(super) fn intersect_rectangle_pair(&self, parts: [Rect; 2]) -> Self {
        if parts.iter().any(|part| empty(*part)) || !separated(parts[0], parts[1]) {
            return self.intersect_unsupported(
                "compound clipping rectangles are not strictly separated".into(),
            );
        }
        match self {
            Self::Unbounded => Self::RectanglePair(parts),
            Self::Rectangle(rectangle) => {
                rectangle_pair(parts.map(|part| intersection(part, *rectangle)))
            }
            Self::Empty | Self::Unsupported(_) => self.clone(),
            Self::Convex(_)
            | Self::RectanglePair(_)
            | Self::RectangularCutout(_)
            | Self::SeparatedCutout(_)
            | Self::Orthogonal { .. }
            | Self::PendingOrthogonal { .. } => {
                self.intersect_unsupported("compound clipping composition is not certified".into())
            }
        }
    }

    /// Retains the certified subtraction under a rectangular prior clip;
    /// other compositions remain uncertain rather than dropping constraints.
    pub(super) fn intersect_rectangular_cutout(&self, clip: RectangularCutout) -> Self {
        match self {
            Self::Unbounded => Self::RectangularCutout(clip),
            Self::Rectangle(rectangle) => clip.intersect_rectangle(*rectangle),
            Self::Empty | Self::Unsupported(_) => self.clone(),
            Self::Convex(_)
            | Self::RectanglePair(_)
            | Self::RectangularCutout(_)
            | Self::SeparatedCutout(_)
            | Self::Orthogonal { .. }
            | Self::PendingOrthogonal { .. } => {
                self.intersect_unsupported("concave clipping composition is not certified".into())
            }
        }
    }

    /// All previous constraints must remain rectangular; no general union or
    /// concave composition is inferred from component bounds.
    pub(super) fn intersect_separated_cutout(&self, clip: SeparatedCutout) -> Self {
        match self {
            Self::Unbounded => Self::SeparatedCutout(Arc::new(clip)),
            Self::Rectangle(rectangle) => clip.intersect_rectangle(*rectangle),
            Self::Empty | Self::Unsupported(_) => self.clone(),
            _ => self.intersect_unsupported("separated corner composition is not certified".into()),
        }
    }

    pub(super) fn intersect_quad(&self, quad: Quad) -> Self {
        let bounds = bounding_rect(quad.0);
        let (bounds, mut quads) = match self {
            Self::Empty => return Self::Empty,
            Self::Unsupported(_) => return self.clone(),
            Self::RectanglePair(_)
            | Self::SeparatedCutout(_)
            | Self::Orthogonal { .. }
            | Self::PendingOrthogonal { .. } => {
                return self.intersect_unsupported(
                    "convex intersection with compound clipping is not certified".into(),
                );
            }
            Self::RectangularCutout(_) => {
                return self.intersect_unsupported(
                    "convex intersection with concave clipping is not certified".into(),
                );
            }
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
            Self::Empty | Self::Unsupported(_) => false,
            Self::Rectangle(rect) => inside(segment.from, *rect) && inside(segment.to, *rect),
            Self::RectanglePair(parts) => parts
                .iter()
                .any(|rect| inside(segment.from, *rect) && inside(segment.to, *rect)),
            Self::RectangularCutout(clip) => clip.segment_is_visible(segment),
            Self::SeparatedCutout(clip) => clip.segment_is_visible(segment),
            Self::Orthogonal { grid, crop } => grid.segment_is_visible(segment, *crop),
            Self::PendingOrthogonal { .. } => false,
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
            Self::RectanglePair(parts) => {
                let statuses = parts.map(|part| rectangle_status(glyph, part));
                Ok(if statuses.contains(&GlyphPathClipStatus::Inside) {
                    GlyphPathClipStatus::Inside
                } else if statuses
                    .iter()
                    .all(|status| *status == GlyphPathClipStatus::Outside)
                {
                    GlyphPathClipStatus::Outside
                } else {
                    // A connected glyph box cannot be wholly contained in two
                    // separated components unless it is contained in one.
                    GlyphPathClipStatus::PartiallyOutside
                })
            }
            Self::Convex(clip) => clip.glyph_status(glyph),
            Self::RectangularCutout(clip) => clip.glyph_status(glyph),
            Self::SeparatedCutout(clip) => clip.glyph_status(glyph),
            Self::Orthogonal { grid, crop } => grid.glyph_status(glyph, *crop),
            Self::PendingOrthogonal { pending, .. } => {
                Err(Error::Unsupported(pending.reason().to_string()))
            }
            Self::Unsupported(reason) => Err(Error::Unsupported(reason.to_string())),
        }
    }
}

fn separated(a: Rect, b: Rect) -> bool {
    a.max.x < b.min.x || b.max.x < a.min.x || a.max.y < b.min.y || b.max.y < a.min.y
}

/// Intersections of separated rectangles remain separated or become empty.
fn rectangle_pair(parts: [Rect; 2]) -> ClipRegion {
    match parts.map(empty) {
        [true, true] => ClipRegion::Empty,
        [false, true] => ClipRegion::Rectangle(parts[0]),
        [true, false] => ClipRegion::Rectangle(parts[1]),
        [false, false] => ClipRegion::RectanglePair(parts),
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

pub(super) fn empty(rect: Rect) -> bool {
    rect.max.x <= rect.min.x || rect.max.y <= rect.min.y
}

pub(super) fn intersection(a: Rect, b: Rect) -> Rect {
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

fn contains(outer: Rect, inner: Rect) -> bool {
    inside(inner.min, outer) && inside(inner.max, outer)
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

    fn l_points() -> [Vec2; 6] {
        [
            point(0.0, 0.0),
            point(4.0, 0.0),
            point(4.0, 2.0),
            point(2.0, 2.0),
            point(2.0, 4.0),
            point(0.0, 4.0),
        ]
    }

    fn notch_points() -> [Vec2; 8] {
        [
            point(0.0, 0.0),
            point(1.0, 0.0),
            point(1.0, 2.0),
            point(3.0, 2.0),
            point(3.0, 0.0),
            point(4.0, 0.0),
            point(4.0, 4.0),
            point(0.0, 4.0),
        ]
    }

    #[test]
    fn edge_notch_constructor_requires_exact_boundary_in_every_orientation() {
        for axis in 0..4 {
            let points = notch_points().map(|p| match axis {
                0 => p,
                1 => point(p.x, -p.y),
                2 => point(p.y, p.x),
                _ => point(-p.y, p.x),
            });
            for shift in 0..8 {
                for reverse in [false, true] {
                    let cyclic = std::array::from_fn(|i| {
                        points[(shift + if reverse { 8 - i } else { i }) % 8]
                    });
                    assert!(RectangularCutout::from_edge_notch(cyclic).is_some());
                }
            }
        }
        let mut skewed = notch_points();
        skewed[1].y = skewed[1].y.next_up();
        assert!(RectangularCutout::from_edge_notch(skewed).is_none());
        let mut crossed = notch_points();
        crossed.swap(2, 6);
        assert!(RectangularCutout::from_edge_notch(crossed).is_none());
        let mut duplicate = notch_points();
        duplicate[2] = duplicate[3];
        assert!(RectangularCutout::from_edge_notch(duplicate).is_none());
        let mut nonfinite = notch_points();
        nonfinite[0].x = f64::NAN;
        assert!(RectangularCutout::from_edge_notch(nonfinite).is_none());
    }

    #[test]
    fn edge_notch_matches_exact_cell_oracle_and_rectangular_crops() -> Result<()> {
        let notch = RectangularCutout::from_edge_notch(notch_points()).expect("certified notch");
        let clip = ClipRegion::Unbounded.intersect_rectangular_cutout(notch);
        assert_eq!(clip.work(), 64);
        for x0 in -1..5 {
            for x1 in x0 + 1..=5 {
                for y0 in -1..5 {
                    for y1 in y0 + 1..=5 {
                        let mut count = 0;
                        for x in x0..x1 {
                            for y in y0..y1 {
                                count += i32::from(
                                    (0..4).contains(&x)
                                        && (0..4).contains(&y)
                                        && (y >= 2 || !(1..3).contains(&x)),
                                );
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
                            clip.glyph_status(rect(
                                f64::from(x0),
                                f64::from(y0),
                                f64::from(x1),
                                f64::from(y1)
                            ))?,
                            expected
                        );
                    }
                }
            }
        }
        assert!(matches!(
            clip.intersect_rectangle(rect(1.1, 0.1, 2.9, 1.9)),
            ClipRegion::Empty
        ));
        assert!(matches!(
            clip.intersect_rectangle(rect(0.0, 2.5, 4.0, 4.0)),
            ClipRegion::Rectangle(_)
        ));
        let strip = clip.intersect_rectangle(rect(1.5, 0.0, 2.5, 3.0));
        assert_eq!(
            strip.glyph_status(rect(1.6, 2.1, 2.4, 2.9))?,
            GlyphPathClipStatus::Inside
        );
        assert_eq!(
            strip.glyph_status(rect(1.6, 0.1, 2.4, 1.9))?,
            GlyphPathClipStatus::Outside
        );
        assert!(clip.glyph_status(rect(1.0, 1.0, 1.0, 2.0)).is_err());
        assert!(!clip.segment_is_visible(PathSegment {
            from: point(0.5, 1.0),
            to: point(3.5, 1.0)
        }));
        // Retain the existing conservative cross-strip refusal even where
        // the complete segment happens to lie in the retained set.
        assert!(!clip.segment_is_visible(PathSegment {
            from: point(0.5, 3.0),
            to: point(3.5, 3.0)
        }));
        assert!(clip.segment_is_visible(PathSegment {
            from: point(1.2, 3.0),
            to: point(2.8, 3.0)
        }));
        assert!(matches!(
            clip.intersect_rectangular_cutout(notch),
            ClipRegion::Unsupported(_)
        ));
        assert!(matches!(
            ClipRegion::Unbounded
                .intersect_quad(diamond(0.0))
                .intersect_rectangular_cutout(notch),
            ClipRegion::Unsupported(_)
        ));
        Ok(())
    }

    #[test]
    fn separated_cutout_matches_exact_cell_oracle_and_crop_normalization() -> Result<()> {
        let corner = RectangularCutout::from_l_shape(l_points()).expect("exact L");
        let parts = SeparatedCutout::new(rect(6.0, 0.0, 7.0, 4.0), corner).expect("separated");
        let clip = ClipRegion::Unbounded.intersect_separated_cutout(parts);
        assert_eq!(clip.work(), 192);
        for x0 in -1..8 {
            for x1 in x0 + 1..=8 {
                for y0 in -1..5 {
                    for y1 in y0 + 1..=5 {
                        let mut included = 0;
                        for x in x0..x1 {
                            for y in y0..y1 {
                                included += i32::from(
                                    (0..4).contains(&y)
                                        && (((0..4).contains(&x) && (x < 2 || y < 2)) || x == 6),
                                );
                            }
                        }
                        let expected = if included == 0 {
                            GlyphPathClipStatus::Outside
                        } else if included == (x1 - x0) * (y1 - y0) {
                            GlyphPathClipStatus::Inside
                        } else {
                            GlyphPathClipStatus::PartiallyOutside
                        };
                        assert_eq!(
                            clip.glyph_status(rect(
                                f64::from(x0),
                                f64::from(y0),
                                f64::from(x1),
                                f64::from(y1)
                            ))?,
                            expected
                        );
                    }
                }
            }
        }
        assert!(matches!(
            clip.intersect_rectangle(rect(5.0, 0.0, 8.0, 4.0)),
            ClipRegion::Rectangle(_)
        ));
        assert!(matches!(
            clip.intersect_rectangle(rect(-1.0, -1.0, 5.0, 5.0)),
            ClipRegion::RectangularCutout(_)
        ));
        assert!(matches!(
            clip.intersect_rectangle(rect(-1.0, -1.0, 8.0, 1.0)),
            ClipRegion::RectanglePair(_)
        ));
        assert!(matches!(
            clip.intersect_rectangle(rect(4.5, 0.0, 5.5, 4.0)),
            ClipRegion::Empty
        ));
        assert!(clip.glyph_status(rect(1.0, 1.0, 1.0, 2.0)).is_err());
        assert!(!clip.segment_is_visible(PathSegment {
            from: point(1.0, 1.0),
            to: point(6.5, 1.0)
        }));
        assert!(clip.segment_is_visible(PathSegment {
            from: point(6.2, 1.0),
            to: point(6.8, 3.0)
        }));
        Ok(())
    }

    #[test]
    fn separated_cutout_retains_prior_constraints_and_rejects_touching_bounds() {
        let corner = RectangularCutout::from_l_shape(l_points()).expect("exact L");
        for rectangle in [
            rect(4.0, 0.0, 5.0, 1.0),
            rect(3.0, 3.0, 5.0, 5.0),
            rect(5.0, 0.0, 5.0, 1.0),
            rect(f64::NAN, 8.0, 9.0, 10.0),
        ] {
            assert!(SeparatedCutout::new(rectangle, corner).is_none());
        }
        let parts = SeparatedCutout::new(rect(6.0, 0.0, 7.0, 4.0), corner).expect("separated");
        for prior in [
            ClipRegion::Unbounded.intersect_rectangular_cutout(corner),
            ClipRegion::Unbounded
                .intersect_rectangle_pair([rect(0.0, 0.0, 1.0, 1.0), rect(2.0, 0.0, 3.0, 1.0)]),
            ClipRegion::Unbounded.intersect_quad(diamond(0.0)),
        ] {
            assert!(matches!(
                prior.intersect_separated_cutout(parts),
                ClipRegion::Unsupported(_)
            ));
        }
        let clip = ClipRegion::Unbounded.intersect_separated_cutout(parts);
        assert!(matches!(
            clip.intersect_rectangular_cutout(corner),
            ClipRegion::Unsupported(_)
        ));
        assert!(matches!(
            clip.intersect_quad(diamond(0.0)),
            ClipRegion::Unsupported(_)
        ));
        assert!(matches!(
            clip.intersect_separated_cutout(parts),
            ClipRegion::Unsupported(_)
        ));
    }

    #[test]
    fn corner_cutout_matches_an_independent_exact_cell_oracle() -> Result<()> {
        let shape = RectangularCutout::from_l_shape(l_points()).expect("exact L");
        let clip = ClipRegion::Unbounded.intersect_rectangular_cutout(shape);
        assert_eq!(clip.work(), 64);
        for x0 in -1..5 {
            for x1 in x0 + 1..=5 {
                for y0 in -1..5 {
                    for y1 in y0 + 1..=5 {
                        // Integer-aligned boxes tile into exact unit cells;
                        // no raster or approximate membership is used by the
                        // production proof. This oracle counts cell interiors.
                        let mut included = 0;
                        for x in x0..x1 {
                            for y in y0..y1 {
                                included += i32::from(
                                    (0..4).contains(&x) && (0..4).contains(&y) && (x < 2 || y < 2),
                                );
                            }
                        }
                        let total = (x1 - x0) * (y1 - y0);
                        let expected = if included == 0 {
                            GlyphPathClipStatus::Outside
                        } else if included == total {
                            GlyphPathClipStatus::Inside
                        } else {
                            GlyphPathClipStatus::PartiallyOutside
                        };
                        assert_eq!(
                            clip.glyph_status(rect(
                                f64::from(x0),
                                f64::from(y0),
                                f64::from(x1),
                                f64::from(y1)
                            ))?,
                            expected,
                            "box {x0},{y0}..{x1},{y1}"
                        );
                    }
                }
            }
        }
        assert!(clip.glyph_status(rect(1.0, 1.0, 1.0, 2.0)).is_err());
        assert!(clip.segment_is_visible(PathSegment {
            from: point(0.5, 3.0),
            to: point(1.5, 3.0)
        }));
        assert!(!clip.segment_is_visible(PathSegment {
            from: point(1.0, 3.5),
            to: point(3.5, 1.0)
        }));
        assert_eq!(
            clip.intersect_rectangle(rect(0.0, 0.0, 1.0, 4.0)),
            ClipRegion::Rectangle(rect(0.0, 0.0, 1.0, 4.0))
        );
        assert_eq!(
            clip.intersect_rectangle(rect(2.5, 2.5, 3.5, 3.5)),
            ClipRegion::Empty
        );
        let cropped = clip.intersect_rectangle(rect(1.0, 1.0, 3.0, 3.0));
        assert_eq!(
            cropped.glyph_status(rect(1.25, 2.25, 1.75, 2.75))?,
            GlyphPathClipStatus::Inside
        );
        assert_eq!(
            cropped.glyph_status(rect(2.25, 2.25, 2.75, 2.75))?,
            GlyphPathClipStatus::Outside
        );
        assert!(matches!(
            clip.intersect_quad(diamond(0.0)),
            ClipRegion::Unsupported(_)
        ));
        assert!(matches!(
            clip.intersect_rectangular_cutout(shape),
            ClipRegion::Unsupported(_)
        ));
        Ok(())
    }

    #[test]
    fn corner_cutout_boundary_proof_is_exact_under_rotations_and_winding() {
        for flip_x in [false, true] {
            for flip_y in [false, true] {
                let points = l_points().map(|p| {
                    point(
                        if flip_x { 4.0 - p.x } else { p.x },
                        if flip_y { 4.0 - p.y } else { p.y },
                    )
                });
                for reverse in [false, true] {
                    for shift in 0..6 {
                        let ordered = std::array::from_fn(|i| {
                            points[(shift + if reverse { 6 - i } else { i }) % 6]
                        });
                        assert!(RectangularCutout::from_l_shape(ordered).is_some());
                    }
                }
            }
        }
        let mut skewed = l_points();
        skewed[2].x = skewed[2].x.next_up();
        assert!(RectangularCutout::from_l_shape(skewed).is_none());
        let mut duplicate = l_points();
        duplicate[4] = duplicate[3];
        assert!(RectangularCutout::from_l_shape(duplicate).is_none());
        let mut crossed = l_points();
        crossed.swap(1, 4);
        assert!(RectangularCutout::from_l_shape(crossed).is_none());
        let mut nonfinite = l_points();
        nonfinite[0].x = f64::INFINITY;
        assert!(RectangularCutout::from_l_shape(nonfinite).is_none());
        assert!(
            RectangularCutout::from_l_shape([
                point(0.0, 0.0),
                point(2.0, 0.0),
                point(4.0, 0.0),
                point(4.0, 4.0),
                point(2.0, 4.0),
                point(0.0, 4.0)
            ])
            .is_none()
        );
    }

    #[test]
    fn separated_rectangles_keep_the_gap_and_prior_intersections() -> Result<()> {
        let pair = ClipRegion::Unbounded
            .intersect_rectangle_pair([rect(0.0, 0.0, 10.0, 2.0), rect(0.0, 4.0, 10.0, 6.0)]);
        for (glyph, expected) in [
            (rect(1.0, 0.5, 2.0, 1.5), GlyphPathClipStatus::Inside),
            (rect(1.0, 2.5, 2.0, 3.5), GlyphPathClipStatus::Outside),
            (
                rect(1.0, 1.0, 2.0, 5.0),
                GlyphPathClipStatus::PartiallyOutside,
            ),
            (
                rect(-1.0, -1.0, 11.0, 7.0),
                GlyphPathClipStatus::PartiallyOutside,
            ),
        ] {
            assert_eq!(pair.glyph_status(glyph)?, expected);
        }
        assert_eq!(pair.work(), 32);
        assert!(!pair.segment_is_visible(PathSegment {
            from: point(1.0, 1.0),
            to: point(1.0, 5.0)
        }));
        assert!(pair.segment_is_visible(PathSegment {
            from: point(1.0, 1.0),
            to: point(2.0, 1.0)
        }));
        assert_eq!(
            pair.intersect_rectangle(rect(0.0, 0.0, 5.0, 1.0)),
            ClipRegion::Rectangle(rect(0.0, 0.0, 5.0, 1.0))
        );
        assert_eq!(
            pair.intersect_rectangle(rect(0.0, 2.5, 5.0, 3.5)),
            ClipRegion::Empty
        );
        assert!(matches!(
            pair.intersect_quad(diamond(0.0)),
            ClipRegion::Unsupported(_)
        ));
        assert!(matches!(
            ClipRegion::Unbounded
                .intersect_rectangle_pair([rect(0.0, 0.0, 5.0, 5.0), rect(4.0, 4.0, 6.0, 6.0)]),
            ClipRegion::Unsupported(_)
        ));
        assert!(matches!(
            ClipRegion::Unbounded
                .intersect_rectangle_pair([rect(0.0, 0.0, 5.0, 5.0), rect(5.0, 0.0, 6.0, 5.0)]),
            ClipRegion::Unsupported(_)
        ));
        Ok(())
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
