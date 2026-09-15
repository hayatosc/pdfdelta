use std::collections::HashSet;
use std::ops::RangeInclusive;

use rayon::prelude::*;
use unicode_bidi::{BidiClass, bidi_class};

use crate::{
    Error, Result,
    model::{DecodedText, Document, Glyph, GlyphId, PageId, Rect, Vec2, is_cjk},
    validate::{validate_non_negative, validate_unit_interval},
};

use super::geometry::{
    directions_are_compatible, dot, interval_gap, interval_overlap_ratio, length_squared,
    normalize, perpendicular, projected_center, projected_extent, projected_interval,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntheticSpace {
    pub preceding: GlyphId,
    pub following: GlyphId,
}

/// Strong text direction derived from decoded glyph text, independently of
/// the PDF text transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LineTextDirection {
    LeftToRight,
    RightToLeft,
    Neutral,
    Mixed,
    Unknown,
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
    /// Direction inferred from Unicode bidi classes in decoded glyph text.
    pub text_direction: LineTextDirection,
    /// Inclusive raw glyph paint-order extent covered by this line.
    pub render_order: RangeInclusive<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineOptions {
    /// Baseline tolerance relative to median glyph height. Glyphs outside this
    /// tolerance must overlap across the line and attach within the same ratio
    /// of font size along the line, allowing nearby scripts without joining columns.
    pub max_baseline_distance_ratio: f64,
    pub min_cross_axis_overlap_ratio: f64,
    pub min_direction_similarity: f64,
    pub max_inline_gap_font_size_ratio: f64,
    /// Minimum inferred word gap relative to font size. Font transitions use
    /// twice this threshold; CJK boundaries use four times it to allow justified
    /// typographic spacing in scripts without mandatory word separators.
    pub space_gap_font_size_ratio: f64,
    /// Minimum inferred word gap relative to average glyph advance, with the
    /// same boundary adjustment as [`Self::space_gap_font_size_ratio`].
    pub space_gap_advance_ratio: f64,
}

impl Default for LineOptions {
    fn default() -> Self {
        Self {
            max_baseline_distance_ratio: 0.25,
            min_cross_axis_overlap_ratio: 0.25,
            min_direction_similarity: 0.98,
            max_inline_gap_font_size_ratio: 1.5,
            // Justification can shrink word gaps below half a glyph advance.
            // Both relative metrics remain above small kerning offsets.
            space_gap_font_size_ratio: 0.125,
            space_gap_advance_ratio: 0.25,
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
    // Duplicate ids must be reported for the first duplicate in document
    // order, exactly like the sequential set-based scan this replaces. Glyph
    // ids are dense in practice (extraction assigns sequential ids), so a
    // bitset over the id range avoids hashing; sparse id spaces fall back to
    // the set, keeping both paths behaviorally identical.
    let items = document.items();
    let max_id = items.iter().map(|glyph| glyph.id.0).max().unwrap_or(0);
    let mut glyph_ids = if max_id <= items.len() as u64 * 8 + 64 {
        SeenGlyphIds::Dense(vec![0; (max_id as usize / 64) + 1])
    } else {
        SeenGlyphIds::Sparse(HashSet::with_capacity(items.len()))
    };
    let mut glyphs = Vec::with_capacity(items.len());

    for glyph in items {
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

    // Glyphs are page-sorted, so line building within one page depends only
    // on that page's glyphs: the active-page slice in the sequential loop
    // never consults other pages' lines. Building per-page runs in parallel
    // and concatenating in page order therefore reproduces the sequential
    // output exactly; line ids are assigned after the global sort below.
    let mut page_runs = Vec::new();
    let mut run_start = 0;
    for index in 1..glyphs.len() {
        if glyphs[index].page != glyphs[run_start].page {
            page_runs.push(run_start..index);
            run_start = index;
        }
    }
    if run_start < glyphs.len() {
        page_runs.push(run_start..glyphs.len());
    }

    let mut working_lines = page_runs
        .par_iter()
        .flat_map_iter(|run| build_page_lines(&glyphs[run.clone()], options))
        .collect::<Vec<_>>();

    working_lines.sort_by(|left, right| {
        left.page
            .0
            .cmp(&right.page.0)
            .then(right.bbox.max.y.total_cmp(&left.bbox.max.y))
            .then(left.bbox.min.x.total_cmp(&right.bbox.min.x))
    });

    Ok(working_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| line.finish(LineId(index as u64), options))
        .collect())
}

/// Sequentially builds the lines of one page run; identical to the inner
/// loop of the original single-threaded pass with the page-start offset
/// removed because each run starts with an empty line list.
fn build_page_lines<'a>(glyphs: &[&'a Glyph], options: LineOptions) -> Vec<WorkingLine<'a>> {
    let mut working_lines = Vec::<WorkingLine>::new();
    for glyph in glyphs {
        // Every open line is scored per glyph, so the glyph's normalized
        // direction is computed once here instead of once per pair; the
        // result is identical because normalization is a pure function.
        let glyph_direction = normalize(glyph.direction);
        let best = working_lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                line.candidate_score(glyph, glyph_direction, options)
                    .map(|score| (index, score))
            })
            .min_by(|(_, score_a), (_, score_b)| score_a.total_cmp(score_b));

        if let Some((index, _)) = best {
            working_lines[index].push(glyph);
        } else {
            working_lines.push(WorkingLine::new(glyph));
        }
    }
    working_lines
}

/// Duplicate-glyph-id detection with an O(1) dense bitset over sequential id
/// ranges, falling back to a hash set for sparse id spaces.
enum SeenGlyphIds {
    Dense(Vec<u64>),
    Sparse(HashSet<GlyphId>),
}

impl SeenGlyphIds {
    /// Returns `false` when the id was already present.
    fn insert(&mut self, id: GlyphId) -> bool {
        match self {
            Self::Dense(bits) => {
                let slot = &mut bits[(id.0 / 64) as usize];
                let bit = 1u64 << (id.0 % 64);
                let duplicate = *slot & bit != 0;
                *slot |= bit;
                !duplicate
            }
            Self::Sparse(seen) => seen.insert(id),
        }
    }
}

struct WorkingLine<'a> {
    page: PageId,
    glyphs: Vec<&'a Glyph>,
    bbox: Rect,
    direction: Vec2,
    cross_axis: Vec2,
    inline_interval: (f64, f64),
    cross_interval: (f64, f64),
    baseline_projections: Vec<f64>,
    projected_heights: Vec<f64>,
    font_sizes: Vec<f64>,
}

impl<'a> WorkingLine<'a> {
    fn new(glyph: &'a Glyph) -> Self {
        let direction = normalize(glyph.direction);
        let cross_axis = perpendicular(direction);
        Self {
            page: glyph.page,
            glyphs: vec![glyph],
            bbox: glyph.bbox,
            direction,
            cross_axis,
            inline_interval: projected_interval(glyph.bbox, direction),
            cross_interval: projected_interval(glyph.bbox, cross_axis),
            baseline_projections: vec![dot(glyph.baseline, cross_axis)],
            projected_heights: vec![projected_extent(glyph.bbox, cross_axis)],
            font_sizes: vec![glyph.font_size],
        }
    }

    fn push(&mut self, glyph: &'a Glyph) {
        self.glyphs.push(glyph);
        self.bbox.min.x = self.bbox.min.x.min(glyph.bbox.min.x);
        self.bbox.min.y = self.bbox.min.y.min(glyph.bbox.min.y);
        self.bbox.max.x = self.bbox.max.x.max(glyph.bbox.max.x);
        self.bbox.max.y = self.bbox.max.y.max(glyph.bbox.max.y);
        extend_interval(
            &mut self.inline_interval,
            projected_interval(glyph.bbox, self.direction),
        );
        extend_interval(
            &mut self.cross_interval,
            projected_interval(glyph.bbox, self.cross_axis),
        );
        insert_sorted(
            &mut self.baseline_projections,
            dot(glyph.baseline, self.cross_axis),
        );
        insert_sorted(
            &mut self.projected_heights,
            projected_extent(glyph.bbox, self.cross_axis),
        );
        insert_sorted(&mut self.font_sizes, glyph.font_size);
    }

    fn candidate_score(
        &self,
        glyph: &Glyph,
        glyph_direction: Vec2,
        options: LineOptions,
    ) -> Option<f64> {
        if self.page != glyph.page {
            return None;
        }

        let direction = self.direction;
        let direction_similarity = dot(direction, glyph_direction);
        if !directions_are_compatible(direction, glyph_direction)
            || direction_similarity < options.min_direction_similarity
        {
            return None;
        }

        let median_height = sorted_median(&self.projected_heights)?;
        let baseline = sorted_median(&self.baseline_projections)?;
        let baseline_distance = (dot(glyph.baseline, self.cross_axis) - baseline).abs();
        let baseline_close =
            baseline_distance <= options.max_baseline_distance_ratio * median_height;
        let cross_overlap = interval_overlap_ratio(
            self.cross_interval,
            projected_interval(glyph.bbox, self.cross_axis),
        );
        if !baseline_close && cross_overlap < options.min_cross_axis_overlap_ratio {
            return None;
        }

        let inline_gap = interval_gap(
            self.inline_interval,
            projected_interval(glyph.bbox, direction),
        );
        let median_font_size = sorted_median(&self.font_sizes)?;
        let gap_scale = median_font_size.max(glyph.font_size);
        if inline_gap > options.max_inline_gap_font_size_ratio * gap_scale {
            return None;
        }
        // Cross-axis overlap accommodates attached superscripts and subscripts.
        // Across a word-sized gap it cannot establish a shared baseline: staggered
        // columns can overlap vertically even when their text belongs to different rows.
        if !baseline_close && inline_gap > options.max_baseline_distance_ratio * gap_scale {
            return None;
        }

        Some(
            baseline_distance / median_height
                + inline_gap / gap_scale
                + (1.0 - direction_similarity),
        )
    }

    fn finish(mut self, id: LineId, options: LineOptions) -> Line {
        let text_direction = classify_text_direction(&self.glyphs);
        let first_render_order = self
            .glyphs
            .iter()
            .map(|glyph| glyph.render_order)
            .min()
            .expect("a working line always contains at least one glyph");
        let last_render_order = self
            .glyphs
            .iter()
            .map(|glyph| glyph.render_order)
            .max()
            .expect("a working line always contains at least one glyph");
        let direction = self.direction;
        self.glyphs.sort_by(|left, right| {
            projected_center(left.bbox, direction)
                .total_cmp(&projected_center(right.bbox, direction))
                .then(left.render_order.cmp(&right.render_order))
        });

        let synthetic_spaces = reconstruct_spaces(&self.glyphs, direction, options);
        let bbox = self.bbox;
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
            text_direction,
            render_order: first_render_order..=last_render_order,
        }
    }
}

fn classify_text_direction(glyphs: &[&Glyph]) -> LineTextDirection {
    let mut has_ltr = false;
    let mut has_rtl = false;
    for glyph in glyphs {
        let DecodedText::Mapped(text) = &glyph.text else {
            return LineTextDirection::Unknown;
        };
        for character in text.chars() {
            match bidi_class(character) {
                BidiClass::L => has_ltr = true,
                BidiClass::R | BidiClass::AL => has_rtl = true,
                BidiClass::LRE
                | BidiClass::LRI
                | BidiClass::LRO
                | BidiClass::RLE
                | BidiClass::RLI
                | BidiClass::RLO
                | BidiClass::FSI
                | BidiClass::PDI
                | BidiClass::PDF => return LineTextDirection::Unknown,
                _ => {}
            }
        }
    }
    match (has_ltr, has_rtl) {
        (true, true) => LineTextDirection::Mixed,
        (true, false) => LineTextDirection::LeftToRight,
        (false, true) => LineTextDirection::RightToLeft,
        (false, false) => LineTextDirection::Neutral,
    }
}

fn extend_interval(interval: &mut (f64, f64), next: (f64, f64)) {
    interval.0 = interval.0.min(next.0);
    interval.1 = interval.1.max(next.1);
}

fn insert_sorted(values: &mut Vec<f64>, value: f64) {
    let index = values.partition_point(|existing| existing.total_cmp(&value).is_le());
    values.insert(index, value);
}

fn sorted_median(values: &[f64]) -> Option<f64> {
    let midpoint = values.len() / 2;
    if values.is_empty() {
        None
    } else if values.len().is_multiple_of(2) {
        Some(values[midpoint - 1] / 2.0 + values[midpoint] / 2.0)
    } else {
        Some(values[midpoint])
    }
}

fn reconstruct_spaces(
    glyphs: &[&Glyph],
    direction: Vec2,
    options: LineOptions,
) -> Vec<SyntheticSpace> {
    let (advance_sum, visible_count) = glyphs
        .iter()
        .filter(|glyph| !is_whitespace(&glyph.text))
        .map(|glyph| projected_extent(glyph.bbox, direction))
        .fold((0.0, 0usize), |(sum, count), advance| {
            (sum + advance, count + 1)
        });
    let average_advance = if visible_count == 0 {
        0.0
    } else {
        advance_sum / visible_count as f64
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
            // CJK side bearings and font changes can create gaps inside a word.
            // Keep those boundaries conservative while recovering compressed
            // word spaces inside a uniform run. Raw glyphs remain unchanged.
            let cjk_boundary = matches!(&preceding.text, DecodedText::Mapped(text) if text.chars().next_back().is_some_and(is_cjk))
                || matches!(&following.text, DecodedText::Mapped(text) if text.chars().next().is_some_and(is_cjk));
            let threshold = if cjk_boundary {
                threshold * 4.0
            } else if preceding.font_id != following.font_id || preceding.font_size != following.font_size {
                threshold * 2.0
            } else {
                threshold
            };
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
