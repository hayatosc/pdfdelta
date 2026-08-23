use std::collections::HashSet;

use crate::{
    Error, Result,
    model::{Document, Glyph, GlyphId, PageId, Rect, Vec2},
    validate::{validate_non_negative, validate_unit_interval},
};

use super::geometry::{
    directions_are_compatible, dot, interval_gap, interval_overlap_ratio, length_squared, median,
    normalize, perpendicular, projected_center, projected_extent, projected_interval,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntheticSpace {
    pub preceding: GlyphId,
    pub following: GlyphId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: LineId,
    pub page: PageId,
    pub glyphs: Vec<GlyphId>,
    pub synthetic_spaces: Vec<SyntheticSpace>,
    pub bbox: Rect,
    pub baseline: Vec2,
    pub direction: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineOptions {
    pub max_baseline_distance_ratio: f64,
    pub min_cross_axis_overlap_ratio: f64,
    pub min_direction_similarity: f64,
    pub max_inline_gap_font_size_ratio: f64,
    pub space_gap_font_size_ratio: f64,
    pub space_gap_advance_ratio: f64,
}

impl Default for LineOptions {
    fn default() -> Self {
        Self {
            max_baseline_distance_ratio: 0.25,
            min_cross_axis_overlap_ratio: 0.25,
            min_direction_similarity: 0.98,
            max_inline_gap_font_size_ratio: 4.0,
            space_gap_font_size_ratio: 0.2,
            space_gap_advance_ratio: 0.5,
        }
    }
}

pub(crate) fn validate_line_options(options: LineOptions) -> Result<()> {
    validate_non_negative(
        "max_baseline_distance_ratio",
        options.max_baseline_distance_ratio,
    )?;
    validate_unit_interval(
        "min_cross_axis_overlap_ratio",
        options.min_cross_axis_overlap_ratio,
    )?;
    validate_unit_interval("min_direction_similarity", options.min_direction_similarity)?;
    validate_non_negative(
        "max_inline_gap_font_size_ratio",
        options.max_inline_gap_font_size_ratio,
    )?;
    validate_non_negative(
        "space_gap_font_size_ratio",
        options.space_gap_font_size_ratio,
    )?;
    validate_non_negative("space_gap_advance_ratio", options.space_gap_advance_ratio)?;
    Ok(())
}

pub fn reconstruct_lines(document: &Document<Glyph>, options: LineOptions) -> Result<Vec<Line>> {
    validate_line_options(options)?;
    let mut glyph_ids = HashSet::with_capacity(document.items().len());
    let mut glyphs = Vec::with_capacity(document.items().len());

    for glyph in document.items() {
        validate_glyph(glyph)?;
        if !glyph_ids.insert(glyph.id) {
            return Err(Error::Unresolved(format!(
                "duplicate glyph id {}",
                glyph.id.0
            )));
        }
        glyphs.push(glyph);
    }

    glyphs.sort_by(|left, right| {
        left.page
            .0
            .cmp(&right.page.0)
            .then(left.render_order.cmp(&right.render_order))
            .then(left.id.0.cmp(&right.id.0))
    });

    let mut working_lines = Vec::<WorkingLine<'_>>::new();
    for glyph in glyphs {
        // deliberate: use an O(glyphs × lines) scan until B1 benchmarks show layout clustering
        // dominates; switch to page-local spatial bins when that measured trigger is reached.
        let mut best = None;
        for (index, line) in working_lines.iter().enumerate() {
            let Some(score) = line.candidate_score(glyph, options) else {
                continue;
            };
            if best.is_none_or(|(_, best_score)| score < best_score) {
                best = Some((index, score));
            }
        }

        if let Some((index, _)) = best {
            working_lines[index].glyphs.push(glyph);
        } else {
            working_lines.push(WorkingLine {
                page: glyph.page,
                glyphs: vec![glyph],
            });
        }
    }

    working_lines.sort_by(|left, right| {
        let left_bbox = left.bbox();
        let right_bbox = right.bbox();
        left.page
            .0
            .cmp(&right.page.0)
            .then(right_bbox.max.y.total_cmp(&left_bbox.max.y))
            .then(left_bbox.min.x.total_cmp(&right_bbox.min.x))
    });

    Ok(working_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| line.finish(LineId(index as u64), options))
        .collect())
}

struct WorkingLine<'a> {
    page: PageId,
    glyphs: Vec<&'a Glyph>,
}

impl WorkingLine<'_> {
    fn candidate_score(&self, glyph: &Glyph, options: LineOptions) -> Option<f64> {
        if self.page != glyph.page {
            return None;
        }

        let direction = self.direction();
        let glyph_direction = normalize(glyph.direction);
        let direction_similarity = dot(direction, glyph_direction);
        if !directions_are_compatible(direction, glyph_direction)
            || direction_similarity < options.min_direction_similarity
        {
            return None;
        }

        let cross_axis = perpendicular(direction);
        let median_height = self.median_projected_height(cross_axis);
        let baseline = median(
            self.glyphs
                .iter()
                .map(|item| dot(item.baseline, cross_axis))
                .collect(),
        )
        .expect("a line candidate holds at least one glyph");
        let baseline_distance = (dot(glyph.baseline, cross_axis) - baseline).abs();
        let baseline_close =
            baseline_distance <= options.max_baseline_distance_ratio * median_height;
        let cross_overlap = interval_overlap_ratio(
            self.projected_interval(cross_axis),
            projected_interval(glyph.bbox, cross_axis),
        );
        if !baseline_close && cross_overlap < options.min_cross_axis_overlap_ratio {
            return None;
        }

        let inline_gap = interval_gap(
            self.projected_interval(direction),
            projected_interval(glyph.bbox, direction),
        );
        let median_font_size = median(self.glyphs.iter().map(|item| item.font_size).collect())
            .expect("a line candidate holds at least one glyph");
        let gap_scale = median_font_size.max(glyph.font_size);
        if inline_gap > options.max_inline_gap_font_size_ratio * gap_scale {
            return None;
        }

        Some(
            baseline_distance / median_height
                + inline_gap / gap_scale
                + (1.0 - direction_similarity),
        )
    }

    fn finish(mut self, id: LineId, options: LineOptions) -> Line {
        let direction = self.direction();
        self.glyphs.sort_by(|left, right| {
            projected_center(left.bbox, direction)
                .total_cmp(&projected_center(right.bbox, direction))
                .then(left.render_order.cmp(&right.render_order))
        });

        let synthetic_spaces = reconstruct_spaces(&self.glyphs, direction, options);
        let bbox = self.bbox();
        let mut baseline_anchor = self.glyphs[0];
        for glyph in &self.glyphs[1..] {
            if glyph.font_size > baseline_anchor.font_size {
                baseline_anchor = glyph;
            }
        }
        let baseline = baseline_anchor.baseline;
        let glyphs = self.glyphs.into_iter().map(|glyph| glyph.id).collect();

        Line {
            id,
            page: self.page,
            glyphs,
            synthetic_spaces,
            bbox,
            baseline,
            direction,
        }
    }

    fn direction(&self) -> Vec2 {
        normalize(self.glyphs[0].direction)
    }

    fn bbox(&self) -> Rect {
        let mut bbox = self.glyphs[0].bbox;
        for glyph in &self.glyphs[1..] {
            bbox.min.x = bbox.min.x.min(glyph.bbox.min.x);
            bbox.min.y = bbox.min.y.min(glyph.bbox.min.y);
            bbox.max.x = bbox.max.x.max(glyph.bbox.max.x);
            bbox.max.y = bbox.max.y.max(glyph.bbox.max.y);
        }
        bbox
    }

    fn projected_interval(&self, axis: Vec2) -> (f64, f64) {
        let mut interval = projected_interval(self.glyphs[0].bbox, axis);
        for glyph in &self.glyphs[1..] {
            let next = projected_interval(glyph.bbox, axis);
            interval.0 = interval.0.min(next.0);
            interval.1 = interval.1.max(next.1);
        }
        interval
    }

    fn median_projected_height(&self, cross_axis: Vec2) -> f64 {
        median(
            self.glyphs
                .iter()
                .map(|glyph| projected_extent(glyph.bbox, cross_axis))
                .collect(),
        )
        .expect("a line candidate holds at least one glyph")
    }
}

fn reconstruct_spaces(
    glyphs: &[&Glyph],
    direction: Vec2,
    options: LineOptions,
) -> Vec<SyntheticSpace> {
    let visible_advances: Vec<_> = glyphs
        .iter()
        .filter(|glyph| !is_whitespace(&glyph.text))
        .map(|glyph| projected_extent(glyph.bbox, direction))
        .collect();
    let average_advance = if visible_advances.is_empty() {
        0.0
    } else {
        visible_advances.iter().sum::<f64>() / visible_advances.len() as f64
    };

    glyphs
        .windows(2)
        .filter_map(|pair| {
            let preceding = pair[0];
            let following = pair[1];
            if ends_with_whitespace(&preceding.text) || starts_with_whitespace(&following.text) {
                return None;
            }

            let gap = interval_gap(
                projected_interval(preceding.bbox, direction),
                projected_interval(following.bbox, direction),
            );
            let font_size = (preceding.font_size + following.font_size) / 2.0;
            let threshold = (options.space_gap_font_size_ratio * font_size)
                .max(options.space_gap_advance_ratio * average_advance);
            (gap > threshold).then_some(SyntheticSpace {
                preceding: preceding.id,
                following: following.id,
            })
        })
        .collect()
}

fn validate_glyph(glyph: &Glyph) -> Result<()> {
    let coordinates = [
        glyph.bbox.min.x,
        glyph.bbox.min.y,
        glyph.bbox.max.x,
        glyph.bbox.max.y,
        glyph.baseline.x,
        glyph.baseline.y,
        glyph.direction.x,
        glyph.direction.y,
        glyph.font_size,
    ];
    if coordinates.iter().any(|value| !value.is_finite()) {
        return Err(invalid_glyph(glyph, "non-finite geometry"));
    }
    if glyph.bbox.min.x > glyph.bbox.max.x || glyph.bbox.min.y > glyph.bbox.max.y {
        return Err(invalid_glyph(glyph, "inverted bounding box"));
    }
    if glyph.font_size <= 0.0 {
        return Err(invalid_glyph(glyph, "non-positive font size"));
    }
    if length_squared(glyph.direction) <= f64::EPSILON {
        return Err(invalid_glyph(glyph, "zero writing direction"));
    }
    let direction = normalize(glyph.direction);
    let inline_extent = projected_extent(glyph.bbox, direction);
    let cross_extent = projected_extent(glyph.bbox, perpendicular(direction));
    if !inline_extent.is_finite() || !cross_extent.is_finite() {
        return Err(invalid_glyph(glyph, "non-finite projected geometry"));
    }
    if cross_extent <= f64::EPSILON {
        return Err(invalid_glyph(glyph, "zero cross-axis extent"));
    }
    Ok(())
}

fn invalid_glyph(glyph: &Glyph, reason: &str) -> Error {
    Error::Unresolved(format!("glyph {} has {reason}", glyph.id.0))
}

fn is_whitespace(text: &crate::model::DecodedText) -> bool {
    matches!(text, crate::model::DecodedText::Mapped(value) if value.chars().all(char::is_whitespace))
}

fn starts_with_whitespace(text: &crate::model::DecodedText) -> bool {
    matches!(text, crate::model::DecodedText::Mapped(value) if value.chars().next().is_some_and(char::is_whitespace))
}

fn ends_with_whitespace(text: &crate::model::DecodedText) -> bool {
    matches!(text, crate::model::DecodedText::Mapped(value) if value.chars().next_back().is_some_and(char::is_whitespace))
}
