//! A strict interior point and an exterior corner certify partial clipping.
//!
//! Rounded polygon clipping only nominates a point. The original half-planes,
//! checked with outward-rounded determinant intervals, verify the certificate.
//! An empty approximate polygon never establishes absence, and this fallback
//! can never establish full visibility. Earlier corner verdicts stay unchanged.

use super::{ConvexClip, Error, GlyphPathClipStatus, Ordering, Rect, Result, Vec2, corners, side};

const MAX_QUADS: usize = 16;
const MAX_VERTICES: usize = 4 + 4 * MAX_QUADS;

struct Budget {
    used: usize,
    limit: usize,
    exhausted: bool,
}

impl Budget {
    fn charge(&mut self, amount: usize) -> bool {
        if let Some(total) = self.used.checked_add(amount).filter(|n| *n <= self.limit) {
            self.used = total;
            true
        } else {
            self.exhausted = true;
            false
        }
    }
}

impl ConvexClip {
    /// Keeps the existing corner verdict, then optionally proves partial clipping.
    ///
    /// The caller has paid the ordinary corner scan through `ClipRegion::work`.
    /// Additional work is charged lazily to the same extraction counter. The
    /// bounded stack buffers need no heap allocation. A failed nomination or
    /// certificate retains uncertainty; exhausted work returns the existing
    /// extraction limit error without publishing a partial certificate.
    pub(in crate::source::content_stream) fn glyph_status_with_witness(
        &self,
        glyph: Rect,
        used: &mut usize,
        limit: usize,
    ) -> Result<GlyphPathClipStatus> {
        let original = self.glyph_status(glyph);
        if !matches!(&original, Err(Error::Unresolved(_))) {
            return original;
        }
        let mut budget = Budget {
            used: *used,
            limit,
            exhausted: false,
        };
        let partial = nominate(self, glyph, &mut budget)
            .is_some_and(|point| certify(self, glyph, point, &mut budget));
        *used = budget.used;
        if budget.exhausted {
            return Err(Error::LimitExceeded {
                resource: "convex clipping work",
                limit,
            });
        }
        if partial {
            Ok(GlyphPathClipStatus::PartiallyOutside)
        } else {
            original
        }
    }
}

fn finite(point: Vec2) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn approximate_side(a: Vec2, b: Vec2, point: Vec2) -> Option<f64> {
    let value = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    value.is_finite().then_some(value)
}

fn push(buffer: &mut [Vec2; MAX_VERTICES], len: &mut usize, point: Vec2) -> Option<()> {
    if !finite(point) || *len == MAX_VERTICES {
        return None;
    }
    buffer[*len] = point;
    *len += 1;
    Some(())
}

/// Sequential half-plane clipping proposes a finite point, without proving it.
/// Any arithmetic failure or vertex-cap stop simply abandons the nomination.
fn nominate(clip: &ConvexClip, glyph: Rect, budget: &mut Budget) -> Option<Vec2> {
    if !budget.charge(8)
        || clip.quads.is_empty()
        || clip.quads.len() > MAX_QUADS
        || !finite(glyph.min)
        || !finite(glyph.max)
        || glyph.min.x >= glyph.max.x
        || glyph.min.y >= glyph.max.y
    {
        return None;
    }
    if !budget.charge(2 * MAX_VERTICES + 4) {
        return None;
    }
    let mut buffers = [[Vec2 { x: 0.0, y: 0.0 }; MAX_VERTICES]; 2];
    buffers[0][..4].copy_from_slice(&corners(glyph));
    let mut current = 0;
    let mut len = 4;
    for (a, b) in clip.quads.iter().flat_map(|quad| quad.edges()) {
        if len == 0 || !budget.charge(16) {
            return None;
        }
        let output = 1 - current;
        let mut next_len = 0;
        let mut previous = buffers[current][len - 1];
        let mut previous_side = approximate_side(a, b, previous)?;
        for index in 0..len {
            // Covers the vertex read, determinant, crossing interpolation and
            // at most two bounded writes before any of those operations run.
            if !budget.charge(64) {
                return None;
            }
            let point = buffers[current][index];
            let point_side = approximate_side(a, b, point)?;
            if (previous_side >= 0.0) != (point_side >= 0.0) {
                let denominator = previous_side - point_side;
                if !denominator.is_finite() || denominator == 0.0 {
                    return None;
                }
                let fraction = previous_side / denominator;
                if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                    return None;
                }
                push(
                    &mut buffers[output],
                    &mut next_len,
                    Vec2 {
                        x: previous.x + fraction * (point.x - previous.x),
                        y: previous.y + fraction * (point.y - previous.y),
                    },
                )?;
            }
            if point_side >= 0.0 {
                push(&mut buffers[output], &mut next_len, point)?;
            }
            previous = point;
            previous_side = point_side;
        }
        current = output;
        len = next_len;
    }
    if len == 0 || !budget.charge(len.saturating_mul(4).saturating_add(4)) {
        return None;
    }
    let mut mean = Vec2 { x: 0.0, y: 0.0 };
    for point in &buffers[current][..len] {
        mean.x += point.x / len as f64;
        mean.y += point.y / len as f64;
    }
    finite(mean).then_some(mean)
}

/// Checks both witnesses against the original geometry, never rounded edges.
/// Strict signs provide nonzero interior and exterior portions of a nonempty
/// glyph rectangle. An unknown determinant cannot discharge either witness.
fn certify(clip: &ConvexClip, glyph: Rect, point: Vec2, budget: &mut Budget) -> bool {
    if !budget.charge(8)
        || !finite(point)
        || !finite(glyph.min)
        || !finite(glyph.max)
        || !(glyph.min.x < point.x
            && point.x < glyph.max.x
            && glyph.min.y < point.y
            && point.y < glyph.max.y)
        || clip.quads.is_empty()
        || clip.quads.len() > MAX_QUADS
    {
        return false;
    }
    let mut exterior = false;
    let points = corners(glyph);
    for (a, b) in clip.quads.iter().flat_map(|quad| quad.edges()) {
        if !budget.charge(80) || side(a, b, point) != Some(Ordering::Greater) {
            return false;
        }
        for corner in points {
            exterior |= side(a, b, corner) == Some(Ordering::Less);
        }
    }
    exterior
}

#[cfg(test)]
mod tests;
