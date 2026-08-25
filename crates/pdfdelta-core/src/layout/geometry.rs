use crate::model::{Rect, Vec2};

const AXIS_ALIGNMENT_TOLERANCE: f64 = 1.0e-6;

pub(super) fn normalize(vector: Vec2) -> Vec2 {
    let length = length_squared(vector).sqrt();
    if length <= f64::EPSILON {
        return Vec2 { x: 0.0, y: 0.0 };
    }
    Vec2 {
        x: vector.x / length,
        y: vector.y / length,
    }
}

pub(super) fn perpendicular(vector: Vec2) -> Vec2 {
    Vec2 {
        x: -vector.y,
        y: vector.x,
    }
}

pub(super) fn is_horizontal(vector: Vec2) -> bool {
    normalize(vector).y.abs() <= AXIS_ALIGNMENT_TOLERANCE
}

pub(super) fn directions_are_compatible(left: Vec2, right: Vec2) -> bool {
    dot(normalize(left), normalize(right)) >= 1.0 - AXIS_ALIGNMENT_TOLERANCE
}

pub(super) fn length_squared(vector: Vec2) -> f64 {
    vector.x * vector.x + vector.y * vector.y
}

pub(super) fn dot(left: Vec2, right: Vec2) -> f64 {
    left.x * right.x + left.y * right.y
}

pub(super) fn projected_interval(rect: Rect, axis: Vec2) -> (f64, f64) {
    let center = Vec2 {
        x: rect.min.x / 2.0 + rect.max.x / 2.0,
        y: rect.min.y / 2.0 + rect.max.y / 2.0,
    };
    let half_width = rect.max.x / 2.0 - rect.min.x / 2.0;
    let half_height = rect.max.y / 2.0 - rect.min.y / 2.0;
    let radius = half_width * axis.x.abs() + half_height * axis.y.abs();
    let center_projection = dot(center, axis);
    (center_projection - radius, center_projection + radius)
}

pub(super) fn projected_center(rect: Rect, axis: Vec2) -> f64 {
    let interval = projected_interval(rect, axis);
    interval.0 / 2.0 + interval.1 / 2.0
}

pub(super) fn projected_extent(rect: Rect, axis: Vec2) -> f64 {
    let interval = projected_interval(rect, axis);
    interval.1 - interval.0
}

pub(super) fn interval_gap(left: (f64, f64), right: (f64, f64)) -> f64 {
    if left.1 < right.0 {
        right.0 - left.1
    } else if right.1 < left.0 {
        left.0 - right.1
    } else {
        0.0
    }
}

pub(super) fn interval_overlap_ratio(left: (f64, f64), right: (f64, f64)) -> f64 {
    let overlap = (left.1.min(right.1) - left.0.max(right.0)).max(0.0);
    let shorter = (left.1 - left.0).min(right.1 - right.0);
    if shorter <= f64::EPSILON {
        0.0
    } else {
        overlap / shorter
    }
}

pub(super) fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let midpoint = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        values[midpoint - 1] / 2.0 + values[midpoint] / 2.0
    } else {
        values[midpoint]
    })
}

#[cfg(test)]
mod tests {
    use super::{median, normalize};
    use crate::model::Vec2;

    #[test]
    fn computes_medians_and_rejects_empty_input_without_panicking() {
        assert_eq!(median(Vec::new()), None);
        assert_eq!(median(vec![3.0]), Some(3.0));
        assert_eq!(median(vec![4.0, 1.0]), Some(2.5));
        assert_eq!(median(vec![5.0, 1.0, 3.0]), Some(3.0));
    }

    #[test]
    fn normalizes_zero_vector_to_zero_without_nan() {
        let normalized = normalize(Vec2 { x: 0.0, y: 0.0 });
        assert_eq!(normalized, Vec2 { x: 0.0, y: 0.0 });
    }
}
