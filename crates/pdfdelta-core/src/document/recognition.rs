use std::collections::BTreeMap;

use crate::{
    Result,
    model::{PageId, Rect, Vec2},
};

use super::{
    BackendIdentity, BackendKind, DocumentGraph, EvidenceStore, GraphLimits, RenderedEvidence,
    SourceConflict, SourceRef, StructuredEvidence, StructuredValue, evidence::invalid,
    graph::charge,
};

impl RenderedEvidence {
    /// Maps a pixel rectangle into an axis-aligned raster's page coordinates.
    /// Polygon corners must be top-left, top-right, bottom-right, bottom-left.
    ///
    /// # Errors
    /// Rejects empty/out-of-grid rectangles and rasters without this explicit
    /// coordinate mapping. Arbitrary polygons do not imply a pixel transform.
    pub fn pixel_bounds_in_page(&self, pixels: [u32; 4]) -> Result<Rect> {
        let [left, top, right, bottom] = pixels;
        let [a, b, c, d] = self.polygon.as_slice() else {
            return Err(invalid(
                "recognition requires a rectangular raster coordinate frame",
            ));
        };
        if self.raster.width == 0
            || self.raster.height == 0
            || left >= right
            || top >= bottom
            || right > self.raster.width
            || bottom > self.raster.height
            || self
                .polygon
                .iter()
                .any(|point| !point.x.is_finite() || !point.y.is_finite())
            || a.x != d.x
            || b.x != c.x
            || a.y != b.y
            || c.y != d.y
            || a.x >= b.x
            || a.y <= d.y
        {
            return Err(invalid(
                "invalid recognition pixel rectangle or coordinate frame",
            ));
        }
        let x = |pixel| a.x + f64::from(pixel) / f64::from(self.raster.width) * (b.x - a.x);
        let y = |pixel| a.y - f64::from(pixel) / f64::from(self.raster.height) * (a.y - d.y);
        let bounds = Rect {
            min: Vec2 {
                x: x(left),
                y: y(bottom),
            },
            max: Vec2 {
                x: x(right),
                y: y(top),
            },
        };
        if !bounds.min.x.is_finite()
            || !bounds.min.y.is_finite()
            || !bounds.max.x.is_finite()
            || !bounds.max.y.is_finite()
        {
            return Err(invalid("recognition coordinate transform overflow"));
        }
        Ok(bounds)
    }
}

pub(super) fn validate_recognition(
    element: &StructuredEvidence,
    image: &RenderedEvidence,
    backend: &BackendIdentity,
) -> Result<()> {
    let StructuredValue::RecognizedText {
        pixel_bounds,
        words,
        ..
    } = &element.value
    else {
        return Err(invalid("expected recognized text evidence"));
    };
    if backend.kind != BackendKind::Ocr
        || backend.model.as_ref().is_none_or(String::is_empty)
        || element.page != Some(image.page)
        || element.object.is_some()
        || element.bounds != Some(image.pixel_bounds_in_page(*pixel_bounds)?)
    {
        return Err(invalid(
            "recognized text provenance or page bounds are inconsistent",
        ));
    }
    let [left, top, right, bottom] = *pixel_bounds;
    for word in words {
        let [x0, y0, x1, y1] = word.pixel_bounds;
        if word.text.is_empty()
            || word.confidence.is_some_and(|confidence| {
                !confidence.is_finite() || !(0.0..=100.0).contains(&confidence)
            })
            || x0 < left
            || y0 < top
            || x0 >= x1
            || y0 >= y1
            || x1 > right
            || y1 > bottom
        {
            return Err(invalid(
                "recognized word is outside its region or has invalid confidence",
            ));
        }
    }
    Ok(())
}

/// A crop may cover native text or another OCR interpretation. Conservatively
/// conflict those views without claiming they are equivalent readings. Pairwise
/// conflicts do not make two native glyphs conflict through a shared OCR view.
pub(super) fn append_recognition_conflicts(
    graph: &mut DocumentGraph,
    store: &EvidenceStore,
    limits: GraphLimits,
) -> Result<()> {
    if !store
        .structured
        .iter()
        .any(|element| matches!(element.value, StructuredValue::RecognizedText { .. }))
    {
        return Ok(());
    }
    let mut native: BTreeMap<PageId, Vec<_>> = BTreeMap::new();
    for glyph in store.native.items() {
        native.entry(glyph.page).or_default().push(glyph);
    }
    let mut recognized: BTreeMap<PageId, Vec<&StructuredEvidence>> = BTreeMap::new();
    let mut work = 0;
    let mut references = 0;
    for element in &store.structured {
        if !matches!(element.value, StructuredValue::RecognizedText { .. }) {
            continue;
        }
        let (Some(page), Some(bounds)) = (element.page, element.bounds) else {
            return Err(invalid("recognized text requires page geometry"));
        };
        let source = SourceRef::Structured {
            element: element.id,
        };
        for glyph in native.get(&page).into_iter().flatten() {
            charge(
                &mut work,
                1,
                limits.max_references,
                "recognition overlap checks",
            )?;
            if overlaps(bounds, glyph.bbox) {
                push_conflict(
                    graph,
                    source,
                    SourceRef::Native { glyph: glyph.id },
                    &mut references,
                    limits,
                )?;
            }
        }
        let earlier = recognized.entry(page).or_default();
        for other in earlier.iter() {
            charge(
                &mut work,
                1,
                limits.max_references,
                "recognition overlap checks",
            )?;
            if other
                .bounds
                .is_some_and(|other_bounds| overlaps(bounds, other_bounds))
            {
                push_conflict(
                    graph,
                    source,
                    SourceRef::Structured { element: other.id },
                    &mut references,
                    limits,
                )?;
            }
        }
        earlier.push(element);
    }
    Ok(())
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y
}

fn push_conflict(
    graph: &mut DocumentGraph,
    first: SourceRef,
    second: SourceRef,
    references: &mut usize,
    limits: GraphLimits,
) -> Result<()> {
    charge(
        references,
        2,
        limits.max_references,
        "recognition conflict references",
    )?;
    let mut count = graph.source_conflicts.len();
    charge(
        &mut count,
        1,
        limits.max_nodes,
        "recognition source conflicts",
    )?;
    graph.source_conflicts.push(SourceConflict {
        sources: vec![first, second],
        reason: "overlapping recognition and source material".into(),
    });
    Ok(())
}
