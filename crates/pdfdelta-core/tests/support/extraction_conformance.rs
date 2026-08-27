use pdfdelta_core::model::{DecodedText, Document, Glyph, GlyphProvenance, PageId, Rect, Vec2};

#[derive(Clone, Debug, PartialEq)]
pub struct PrimitiveExtractionSnapshot {
    pub glyphs: Vec<SnapshotGlyph>,
}

impl PrimitiveExtractionSnapshot {
    pub fn new(glyphs: Vec<SnapshotGlyph>) -> Self {
        Self { glyphs }
    }
}

impl From<&Document<Glyph>> for PrimitiveExtractionSnapshot {
    fn from(document: &Document<Glyph>) -> Self {
        Self {
            glyphs: document.items().iter().map(SnapshotGlyph::from).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotGlyph {
    pub text: DecodedText,
    pub page: PageId,
    pub render_order: u32,
    pub bbox: Rect,
    pub baseline: Vec2,
    pub direction: Vec2,
    pub provenance: Option<GlyphProvenance>,
}

impl SnapshotGlyph {
    pub fn mapped(
        text: impl Into<String>,
        page: PageId,
        render_order: u32,
        bbox: Rect,
        baseline: Vec2,
        direction: Vec2,
    ) -> Self {
        Self {
            text: DecodedText::Mapped(text.into()),
            page,
            render_order,
            bbox,
            baseline,
            direction,
            provenance: None,
        }
    }
}

impl From<&Glyph> for SnapshotGlyph {
    fn from(glyph: &Glyph) -> Self {
        Self {
            text: glyph.text.clone(),
            page: glyph.page,
            render_order: glyph.render_order,
            bbox: glyph.bbox,
            baseline: glyph.baseline,
            direction: glyph.direction,
            provenance: Some(glyph.provenance),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryTolerance {
    absolute: f64,
}

impl GeometryTolerance {
    pub fn new(absolute: f64) -> Result<Self, String> {
        if !absolute.is_finite() || absolute < 0.0 {
            return Err(format!(
                "geometry tolerance must be finite and non-negative, got {absolute}"
            ));
        }
        Ok(Self { absolute })
    }
}

pub fn compare_snapshots(
    expected: &PrimitiveExtractionSnapshot,
    actual: &PrimitiveExtractionSnapshot,
    tolerance: GeometryTolerance,
) -> Result<(), String> {
    if expected.glyphs.len() != actual.glyphs.len() {
        return Err(format!(
            "glyph count mismatch: expected {}, got {}",
            expected.glyphs.len(),
            actual.glyphs.len()
        ));
    }

    for (index, (expected, actual)) in expected.glyphs.iter().zip(&actual.glyphs).enumerate() {
        compare_exact(index, "text", &expected.text, &actual.text, actual)?;
        compare_exact(index, "page", &expected.page, &actual.page, actual)?;
        compare_exact(
            index,
            "render_order",
            &expected.render_order,
            &actual.render_order,
            actual,
        )?;
        compare_float(
            index,
            "bbox.min.x",
            expected.bbox.min.x,
            actual.bbox.min.x,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "bbox.min.y",
            expected.bbox.min.y,
            actual.bbox.min.y,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "bbox.max.x",
            expected.bbox.max.x,
            actual.bbox.max.x,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "bbox.max.y",
            expected.bbox.max.y,
            actual.bbox.max.y,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "baseline.x",
            expected.baseline.x,
            actual.baseline.x,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "baseline.y",
            expected.baseline.y,
            actual.baseline.y,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "direction.x",
            expected.direction.x,
            actual.direction.x,
            tolerance,
            actual,
        )?;
        compare_float(
            index,
            "direction.y",
            expected.direction.y,
            actual.direction.y,
            tolerance,
            actual,
        )?;
    }
    Ok(())
}

fn compare_exact<T: std::fmt::Debug + PartialEq>(
    index: usize,
    field: &str,
    expected: &T,
    actual: &T,
    actual_glyph: &SnapshotGlyph,
) -> Result<(), String> {
    if expected != actual {
        return Err(mismatch(
            index,
            field,
            format_args!("{expected:?}"),
            format_args!("{actual:?}"),
            actual_glyph,
        ));
    }
    Ok(())
}

fn compare_float(
    index: usize,
    field: &str,
    expected: f64,
    actual: f64,
    tolerance: GeometryTolerance,
    actual_glyph: &SnapshotGlyph,
) -> Result<(), String> {
    let difference = (actual - expected).abs();
    if !difference.is_finite() || difference > tolerance.absolute {
        return Err(mismatch(
            index,
            field,
            format_args!("{expected}"),
            format_args!("{actual}"),
            actual_glyph,
        ));
    }
    Ok(())
}

fn mismatch(
    index: usize,
    field: &str,
    expected: std::fmt::Arguments<'_>,
    actual: std::fmt::Arguments<'_>,
    actual_glyph: &SnapshotGlyph,
) -> String {
    let provenance = actual_glyph
        .provenance
        .map_or_else(String::new, |provenance| {
            format!(
                "; actual provenance=object {} {} operator {}",
                provenance.content_stream.object_number,
                provenance.content_stream.generation,
                provenance.operator_index
            )
        });
    format!("glyph {index} {field} mismatch: expected {expected}, got {actual}{provenance}")
}
