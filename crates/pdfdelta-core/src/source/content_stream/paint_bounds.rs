//! Outward-rounded paint bounds in the retained native page coordinate frame.
//! Unknown arithmetic or unsupported path strokes never become empty regions.

use super::{InterpreterState, Matrix, PageGeometry, Rect, Vec2};

#[derive(Clone, Copy, Debug)]
struct Interval {
    low: f64,
    high: f64,
}

impl Interval {
    const UNKNOWN: Self = Self {
        low: f64::NEG_INFINITY,
        high: f64::INFINITY,
    };

    fn point(value: f64) -> Self {
        Self {
            low: value,
            high: value,
        }
    }

    fn finite(self) -> bool {
        self.low.is_finite() && self.high.is_finite()
    }

    fn absolute_upper(self) -> f64 {
        self.low.abs().max(self.high.abs())
    }

    fn hull(self, other: Self) -> Self {
        Self {
            low: self.low.min(other.low),
            high: self.high.max(other.high),
        }
    }
}

impl std::ops::Add for Interval {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        if !self.finite() || !other.finite() {
            return Self::UNKNOWN;
        }
        Self {
            low: (self.low + other.low).next_down(),
            high: (self.high + other.high).next_up(),
        }
    }
}

impl std::ops::Mul for Interval {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        if !self.finite() || !other.finite() {
            return Self::UNKNOWN;
        }
        let values = [
            self.low * other.low,
            self.low * other.high,
            self.high * other.low,
            self.high * other.high,
        ];
        if !values.into_iter().all(f64::is_finite) {
            return Self::UNKNOWN;
        }
        Self {
            low: values.into_iter().fold(f64::INFINITY, f64::min).next_down(),
            high: values
                .into_iter()
                .fold(f64::NEG_INFINITY, f64::max)
                .next_up(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct MatrixBounds([Interval; 6]);

impl From<Matrix> for MatrixBounds {
    fn from(matrix: Matrix) -> Self {
        Self([matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f].map(Interval::point))
    }
}

impl MatrixBounds {
    pub(super) fn then(self, other: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [oa, ob, oc, od, oe, of] = other.0;
        Self([
            a * oa + c * ob,
            b * oa + d * ob,
            a * oc + c * od,
            b * oc + d * od,
            a * oe + c * of + e,
            b * oe + d * of + f,
        ])
    }

    fn transform(self, x: Interval, y: Interval) -> Option<Rect> {
        let [a, b, c, d, e, f] = self.0;
        let x_bound = a * x + c * y + e;
        let y_bound = b * x + d * y + f;
        (x_bound.finite() && y_bound.finite()).then_some(Rect {
            min: Vec2 {
                x: x_bound.low,
                y: y_bound.low,
            },
            max: Vec2 {
                x: x_bound.high,
                y: y_bound.high,
            },
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Bounds {
    rectangle: Option<Rect>,
    unknown: bool,
}

impl Bounds {
    pub(super) fn include(&mut self, rectangle: Option<Rect>) {
        let Some(rectangle) = rectangle else {
            self.unknown = true;
            return;
        };
        self.rectangle = Some(match self.rectangle {
            None => rectangle,
            Some(current) => Rect {
                min: Vec2 {
                    x: current.min.x.min(rectangle.min.x),
                    y: current.min.y.min(rectangle.min.y),
                },
                max: Vec2 {
                    x: current.max.x.max(rectangle.max.x),
                    y: current.max.y.max(rectangle.max.y),
                },
            },
        });
    }
}

pub(super) fn point_bounds(frame: PageGeometry, ctm: MatrixBounds, x: f64, y: f64) -> Option<Rect> {
    MatrixBounds::from(frame.transform)
        .then(ctm)
        .transform(Interval::point(x), Interval::point(y))
}

pub(super) fn rectangle_bounds(
    frame: PageGeometry,
    ctm: MatrixBounds,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Option<Rect> {
    let x = Interval::point(x).hull(Interval::point(x) + Interval::point(width));
    let y = Interval::point(y).hull(Interval::point(y) + Interval::point(height));
    MatrixBounds::from(frame.transform)
        .then(ctm)
        .transform(x, y)
}

pub(super) fn image_paint_bounds(frame: PageGeometry, state: &InterpreterState) -> Option<Rect> {
    rectangle_bounds(frame, state.graphics.paint_ctm, 0.0, 0.0, 1.0, 1.0)
}

pub(super) fn form_paint_bounds(
    frame: PageGeometry,
    ctm: MatrixBounds,
    [x0, y0, x1, y1]: [f64; 4],
) -> Option<Rect> {
    if ![x0, y0, x1, y1].into_iter().all(f64::is_finite) || x0 >= x1 || y0 >= y1 {
        return None;
    }
    // Direct endpoint intervals avoid a rounded width subtraction. The Form's
    // implicit clipping boundary contains all its possible painted evidence.
    MatrixBounds::from(frame.transform).then(ctm).transform(
        Interval::point(x0).hull(Interval::point(x1)),
        Interval::point(y0).hull(Interval::point(y1)),
    )
}

pub(super) fn path_paint_bounds(
    frame: PageGeometry,
    state: &InterpreterState,
    stroke: bool,
) -> Option<Rect> {
    let path = &state.current_path;
    if path.paint_bounds.unknown || path.drawn_subpaths == 0 {
        return None;
    }
    // Preserve the caller's enclosing Form clip when available. A control hull
    // bounds a fill, but does not bound stroke joins, caps or hairlines.
    if path.has_unsupported_segments && (stroke || state.graphics.form_paint_bounds.is_some()) {
        return None;
    }
    let bounds = path.paint_bounds.rectangle?;
    if !stroke {
        return Some(bounds);
    }
    // One segment has no joins. A full transformed width encloses every cap
    // style, including square caps under anisotropic transforms. Hairlines and
    // multi-segment joins require a different proof and remain unbounded.
    if path.segments.len() != 1 || state.graphics.line_width <= 0.0 {
        return None;
    }
    let [a, b, c, d, _, _] = MatrixBounds::from(frame.transform)
        .then(state.graphics.paint_ctm)
        .0;
    let x = Interval::point(a.absolute_upper()) + Interval::point(c.absolute_upper());
    let y = Interval::point(b.absolute_upper()) + Interval::point(d.absolute_upper());
    let radius = Interval::point(state.graphics.line_width) * Interval::point(x.high.max(y.high));
    if !radius.finite() {
        return None;
    }
    let x = Interval {
        low: bounds.min.x,
        high: bounds.max.x,
    } + Interval {
        low: -radius.high,
        high: radius.high,
    };
    let y = Interval {
        low: bounds.min.y,
        high: bounds.max.y,
    } + Interval {
        low: -radius.high,
        high: radius.high,
    };
    (x.finite() && y.finite()).then_some(Rect {
        min: Vec2 { x: x.low, y: y.low },
        max: Vec2 {
            x: x.high,
            y: y.high,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{Interval, Matrix, MatrixBounds};

    #[test]
    fn cancellation_keeps_the_exact_value_inside_the_enclosure() {
        let translated = MatrixBounds::from(
            Matrix::new(1.0, 0.0, 0.0, 1.0, 1e16, 0.0).expect("finite test transform or bound"),
        )
        .then(MatrixBounds::from(
            Matrix::translation(1.0, 0.0).expect("finite test transform or bound"),
        ))
        .then(MatrixBounds::from(
            Matrix::translation(-1e16, 0.0).expect("finite test transform or bound"),
        ));
        let bounds = translated
            .transform(Interval::point(0.0), Interval::point(0.0))
            .expect("finite test transform or bound");
        assert!(bounds.min.x <= 1.0 && bounds.max.x >= 1.0);
        let overflow = MatrixBounds::from(
            Matrix::new(f64::MAX, 0.0, 0.0, 1.0, 0.0, 0.0).expect("finite test transform or bound"),
        );
        assert!(
            overflow
                .then(overflow)
                .transform(Interval::point(0.0), Interval::point(0.0))
                .is_none()
        );
    }
}
