//! Backend-neutral snapshots for comparing primitive glyph extraction.

use std::{error::Error as StdError, fmt};

use crate::model::{DecodedText, Document, Glyph, GlyphProvenance, PageId, Rect, Vec2};

/// Glyph evidence retained for a primitive extraction comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct PrimitiveExtractionSnapshot {
    pub glyphs: Vec<SnapshotGlyph>,
}

impl PrimitiveExtractionSnapshot {
    #[must_use]
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

/// One glyph in backend-neutral extraction order.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotGlyph {
    pub text: DecodedText,
    pub page: PageId,
    pub render_order: u32,
    pub bbox: Rect,
    pub baseline: Vec2,
    pub direction: Vec2,
    /// Optional local evidence used only to make mismatches actionable.
    pub provenance: Option<GlyphProvenance>,
}

impl SnapshotGlyph {
    /// Creates a mapped glyph without implementation-specific provenance.
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

/// Maximum absolute difference accepted for every geometry coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryTolerance {
    absolute: f64,
}

impl GeometryTolerance {
    /// Validates a finite, non-negative absolute tolerance.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidGeometryTolerance`] when `absolute` is negative or
    /// non-finite.
    pub fn new(absolute: f64) -> Result<Self, InvalidGeometryTolerance> {
        if !absolute.is_finite() || absolute < 0.0 {
            return Err(InvalidGeometryTolerance { value: absolute });
        }
        Ok(Self { absolute })
    }

    #[must_use]
    pub fn absolute(self) -> f64 {
        self.absolute
    }
}

/// An invalid coordinate tolerance supplied by a comparison caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InvalidGeometryTolerance {
    value: f64,
}

impl fmt::Display for InvalidGeometryTolerance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "geometry tolerance must be finite and non-negative, got {}",
            self.value
        )
    }
}

impl StdError for InvalidGeometryTolerance {}

/// The first exact or geometry mismatch between two extraction snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotMismatch {
    GlyphCount {
        expected: usize,
        actual: usize,
    },
    GlyphField {
        index: usize,
        field: &'static str,
        expected: String,
        actual: String,
        actual_provenance: Option<GlyphProvenance>,
    },
}

impl fmt::Display for SnapshotMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GlyphCount { expected, actual } => {
                write!(
                    formatter,
                    "glyph count mismatch: expected {expected}, got {actual}"
                )
            }
            Self::GlyphField {
                index,
                field,
                expected,
                actual,
                actual_provenance,
            } => {
                write!(
                    formatter,
                    "glyph {index} {field} mismatch: expected {expected}, got {actual}"
                )?;
                if let Some(provenance) = actual_provenance {
                    write!(
                        formatter,
                        "; actual provenance=object {} {} operator {}",
                        provenance.content_stream.object_number,
                        provenance.content_stream.generation,
                        provenance.operator_index
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl StdError for SnapshotMismatch {}

/// Compares extraction order and decoded text exactly, and geometry within a
/// caller-selected absolute tolerance.
///
/// # Errors
///
/// Returns [`SnapshotMismatch::GlyphCount`] when the snapshot lengths differ,
/// or [`SnapshotMismatch::GlyphField`] for the first exact or geometry field
/// that does not match.
pub fn compare_snapshots(
    expected: &PrimitiveExtractionSnapshot,
    actual: &PrimitiveExtractionSnapshot,
    tolerance: GeometryTolerance,
) -> Result<(), SnapshotMismatch> {
    if expected.glyphs.len() != actual.glyphs.len() {
        return Err(SnapshotMismatch::GlyphCount {
            expected: expected.glyphs.len(),
            actual: actual.glyphs.len(),
        });
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

fn compare_exact<T: fmt::Debug + PartialEq>(
    index: usize,
    field: &'static str,
    expected: &T,
    actual: &T,
    actual_glyph: &SnapshotGlyph,
) -> Result<(), SnapshotMismatch> {
    if expected != actual {
        return Err(field_mismatch(
            index,
            field,
            format!("{expected:?}"),
            format!("{actual:?}"),
            actual_glyph,
        ));
    }
    Ok(())
}

fn compare_float(
    index: usize,
    field: &'static str,
    expected: f64,
    actual: f64,
    tolerance: GeometryTolerance,
    actual_glyph: &SnapshotGlyph,
) -> Result<(), SnapshotMismatch> {
    let difference = (actual - expected).abs();
    if !difference.is_finite() || difference > tolerance.absolute {
        return Err(field_mismatch(
            index,
            field,
            expected.to_string(),
            actual.to_string(),
            actual_glyph,
        ));
    }
    Ok(())
}

fn field_mismatch(
    index: usize,
    field: &'static str,
    expected: String,
    actual: String,
    actual_glyph: &SnapshotGlyph,
) -> SnapshotMismatch {
    SnapshotMismatch::GlyphField {
        index,
        field,
        expected,
        actual,
        actual_provenance: actual_glyph.provenance,
    }
}
