use std::{
    collections::BTreeMap,
    io::{self, Write},
};

use crate::{
    Error, Result,
    extraction_conformance::{PrimitiveExtractionSnapshot, SnapshotGlyph, SnapshotMismatch},
    model::{DecodedText, PageId},
};

use super::svg::xml_escape;

const DEFAULT_PAGE_WIDTH: f64 = 595.0;
const DEFAULT_PAGE_HEIGHT: f64 = 842.0;
const MIN_CONTENT_EXTENT: f64 = 100.0;
const CANVAS_PADDING: f64 = 20.0;
const PAGE_INSET: f64 = 12.0;
const PAGE_GAP: f64 = 30.0;
const PAGE_HEADER_HEIGHT: f64 = 32.0;
const DOCUMENT_HEADER_HEIGHT: f64 = 58.0;
const MIN_CANVAS_WIDTH: f64 = 640.0;
const MIN_BASELINE_LENGTH: f64 = 8.0;

/// Total glyphs (expected + actual) beyond which the diagnostic is refused
/// to bound in-memory SVG generation. Without this, a count mismatch with
/// `ExtractionLimits::default` (5M glyphs per side) could require ~5–6 GiB
/// (≈500–600 B per glyph × 10M glyphs) and OOM the process.
///
/// Quantitative bound with the limit below: at most 50 000 glyphs are
/// rendered, i.e. ≤ ~25–30 MiB of SVG text, interrupted earlier by the byte
/// cap.
const MAX_DIAGNOSTIC_TOTAL_GLYPHS: usize = 50_000;

/// Hard cap for the generated SVG byte length. The renderer checks the
/// buffer after each write and aborts with a `LimitExceeded` error when exceeded.
const MAX_DIAGNOSTIC_SVG_BYTES: usize = 8 * 1024 * 1024;

/// Bound for any single mapped glyph text or mismatch field value retained in
/// the diagnostic. Without this, `xml_escape` and `glyph_title` build
/// intermediate `String`s of up to `64 MiB` (one oracle glyph value) and
/// then escape them to ~`300 MiB` before `BoundedWriter` can reject via the
/// byte cap.
const MAX_DIAGNOSTIC_GLYPH_TEXT_BYTES: usize = 8 * 1024;
const MAX_DIAGNOSTIC_MISMATCH_FIELD_BYTES: usize = 8 * 1024;

/// Renders expected and actual primitive-extraction snapshots as one SVG
/// diagnostic for a comparison mismatch.
///
/// Expected geometry uses dashed `E` labels, while actual geometry uses solid
/// `A` labels. The mismatched glyphs are emphasized independently of color.
///
/// # Errors
///
/// Returns [`Error::Report`] when either snapshot contains invalid geometry or
/// derived SVG coordinates are non-finite. Returns [`Error::LimitExceeded`]
/// when the diagnostic would exceed its glyph (`50 000` total), output-byte
/// (`8 MiB`), or per-field text (`8 KiB`) caps that bound otherwise
/// multi-gigabyte serialization (see quantitative notes on
/// `MAX_DIAGNOSTIC_TOTAL_GLYPHS` and `MAX_DIAGNOSTIC_GLYPH_TEXT_BYTES`).
pub fn render_extraction_mismatch_svg(
    expected: &PrimitiveExtractionSnapshot,
    actual: &PrimitiveExtractionSnapshot,
    mismatch: &SnapshotMismatch,
) -> Result<String> {
    validate_snapshot("expected", expected)?;
    validate_snapshot("actual", actual)?;
    validate_diagnostic_text_limits(expected, actual, mismatch).map_err(|error| match error {
        Error::LimitExceeded { resource, limit } => Error::LimitExceeded { resource, limit },
        other => other,
    })?;
    let total_glyphs = expected
        .glyphs
        .len()
        .checked_add(actual.glyphs.len())
        .ok_or_else(|| Error::Report("diagnostic glyph count overflowed".to_owned()))?;
    if total_glyphs > MAX_DIAGNOSTIC_TOTAL_GLYPHS {
        return Err(Error::LimitExceeded {
            resource: "diagnostic glyph count",
            limit: MAX_DIAGNOSTIC_TOTAL_GLYPHS,
        });
    }

    let pages = group_glyphs_by_page(expected, actual);
    let layouts = calculate_page_layouts(&pages)?;
    let total_width = layouts
        .values()
        .map(|layout| layout.page_width + CANVAS_PADDING * 2.0)
        .fold(MIN_CANVAS_WIDTH, f64::max);
    let total_height = layouts.values().last().map_or(
        DOCUMENT_HEADER_HEIGHT + DEFAULT_PAGE_HEIGHT + PAGE_GAP * 2.0,
        |layout| layout.y_offset + layout.page_height + PAGE_GAP,
    );
    if !total_width.is_finite() || !total_height.is_finite() {
        return Err(Error::Report(
            "derived extraction mismatch canvas dimensions are non-finite".to_owned(),
        ));
    }

    let mut buffer = Vec::new();
    let mut bounded = BoundedWriter::new(&mut buffer, MAX_DIAGNOSTIC_SVG_BYTES);
    write_svg(
        &mut bounded,
        expected,
        actual,
        mismatch,
        &pages,
        &layouts,
        CanvasSize {
            width: total_width,
            height: total_height,
        },
    )?;
    // `BoundedWriter` already enforces the byte cap; a second check guards
    // callers that might directly inspect the buffer length.
    if buffer.len() > MAX_DIAGNOSTIC_SVG_BYTES {
        return Err(Error::LimitExceeded {
            resource: "diagnostic svg output",
            limit: MAX_DIAGNOSTIC_SVG_BYTES,
        });
    }
    String::from_utf8(buffer).map_err(|error| {
        Error::Report(format!(
            "invalid utf-8 in generated extraction mismatch svg: {error}"
        ))
    })
}

fn write_svg<W: Write>(
    writer: &mut W,
    expected: &PrimitiveExtractionSnapshot,
    actual: &PrimitiveExtractionSnapshot,
    mismatch: &SnapshotMismatch,
    pages: &BTreeMap<PageId, SnapshotPage<'_>>,
    layouts: &BTreeMap<PageId, PageLayout>,
    canvas: CanvasSize,
) -> Result<()> {
    let CanvasSize {
        width: total_width,
        height: total_height,
    } = canvas;
    writeln!(
        writer,
        r#"<svg xmlns="http://www.w3.org/2000/svg" xml:lang="en" role="img" aria-labelledby="diagnostic-title diagnostic-description" viewBox="0 0 {total_width:.2} {total_height:.2}" width="{total_width:.2}" height="{total_height:.2}">"#
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"  <title id="diagnostic-title">Primitive extraction mismatch</title>"#
    )
    .map_err(map_io_error)?;
    let description = format!(
        "{mismatch}. Expected {} glyphs; actual {} glyphs.",
        expected.glyphs.len(),
        actual.glyphs.len()
    );
    writeln!(
        writer,
        r#"  <desc id="diagnostic-description">{}</desc>"#,
        xml_escape(&description)
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"  <defs>
    <style>
      svg {{ background: #f4f5f7; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }}
      .diagnostic-title {{ fill: #101828; font-size: 14px; font-weight: 700; }}
      .legend-label, .page-label {{ fill: #344054; font-size: 12px; }}
      .page-label {{ font-weight: 600; }}
      .page-bg {{ fill: #ffffff; stroke: #98a2b3; stroke-width: 1px; }}
      .snapshot-glyph {{ cursor: crosshair; }}
      .snapshot-bbox {{ stroke-width: 1.25px; }}
      .snapshot-baseline {{ fill: none; stroke-width: 1.25px; }}
      .snapshot-index {{ font-size: 10px; font-weight: 700; pointer-events: none; }}
      .expected .snapshot-bbox {{ fill: #b42318; fill-opacity: 0.10; stroke: #b42318; stroke-dasharray: 4 2; }}
      .expected .snapshot-baseline, .snapshot-baseline.expected {{ stroke: #b42318; stroke-dasharray: 4 2; }}
      .expected .snapshot-index {{ fill: #8a1c13; }}
      .actual .snapshot-bbox {{ fill: #175cd3; fill-opacity: 0.10; stroke: #175cd3; }}
      .actual .snapshot-baseline, .snapshot-baseline.actual {{ stroke: #175cd3; }}
      .actual .snapshot-index {{ fill: #1849a9; }}
      .mismatch .snapshot-bbox {{ stroke: #7a2e0e; stroke-width: 2.75px; }}
      .mismatch .snapshot-baseline {{ stroke-width: 2.75px; }}
    </style>
  </defs>"#
    )
    .map_err(map_io_error)?;

    let label = mismatch_label(mismatch);
    writeln!(
        writer,
        r#"  <text class="diagnostic-title" x="{CANVAS_PADDING:.2}" y="20.00">{}</text>"#,
        xml_escape(&label)
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"  <line class="snapshot-baseline expected" x1="{CANVAS_PADDING:.2}" y1="42.00" x2="52.00" y2="42.00" />
  <text class="legend-label" x="58.00" y="46.00">Expected (E, dashed)</text>
  <line class="snapshot-baseline actual" x1="220.00" y1="42.00" x2="252.00" y2="42.00" />
  <text class="legend-label" x="258.00" y="46.00">Actual (A, solid)</text>"#
    )
    .map_err(map_io_error)?;

    for (page_id, page) in pages {
        let layout = layouts.get(page_id).ok_or_else(|| {
            Error::Report(format!(
                "missing extraction mismatch layout for page {}",
                u64::from(page_id.0) + 1
            ))
        })?;
        render_page(writer, *page_id, page, layout, mismatch)?;
    }

    writeln!(writer, "</svg>").map_err(map_io_error)
}

#[derive(Clone, Copy)]
struct IndexedGlyph<'a> {
    index: usize,
    glyph: &'a SnapshotGlyph,
}

#[derive(Default)]
struct SnapshotPage<'a> {
    expected: Vec<IndexedGlyph<'a>>,
    actual: Vec<IndexedGlyph<'a>>,
}

struct PageLayout {
    page_min_x: f64,
    page_max_y: f64,
    page_width: f64,
    page_height: f64,
    y_offset: f64,
}

#[derive(Clone, Copy)]
struct CanvasSize {
    width: f64,
    height: f64,
}

#[derive(Clone, Copy)]
struct ContentOrigin {
    x: f64,
    y: f64,
}

fn group_glyphs_by_page<'a>(
    expected: &'a PrimitiveExtractionSnapshot,
    actual: &'a PrimitiveExtractionSnapshot,
) -> BTreeMap<PageId, SnapshotPage<'a>> {
    let mut pages = BTreeMap::new();
    for (index, glyph) in expected.glyphs.iter().enumerate() {
        pages
            .entry(glyph.page)
            .or_insert_with(SnapshotPage::default)
            .expected
            .push(IndexedGlyph { index, glyph });
    }
    for (index, glyph) in actual.glyphs.iter().enumerate() {
        pages
            .entry(glyph.page)
            .or_insert_with(SnapshotPage::default)
            .actual
            .push(IndexedGlyph { index, glyph });
    }
    if pages.is_empty() {
        pages.insert(PageId(0), SnapshotPage::default());
    }
    pages
}

fn calculate_page_layouts(
    pages: &BTreeMap<PageId, SnapshotPage<'_>>,
) -> Result<BTreeMap<PageId, PageLayout>> {
    let mut layouts = BTreeMap::new();
    let mut current_y = DOCUMENT_HEADER_HEIGHT + PAGE_GAP;

    for (page_id, page) in pages {
        let mut glyphs = page.expected.iter().chain(&page.actual).peekable();
        let (min_x, min_y, max_x, max_y) = if glyphs.peek().is_none() {
            (0.0, 0.0, DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT)
        } else {
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for item in glyphs {
                let (glyph_min_x, glyph_min_y, glyph_max_x, glyph_max_y) = glyph_extent(item.glyph);
                min_x = min_x.min(glyph_min_x);
                min_y = min_y.min(glyph_min_y);
                max_x = max_x.max(glyph_max_x);
                max_y = max_y.max(glyph_max_y);
            }
            (min_x, min_y, max_x, max_y)
        };

        let content_width = (max_x - min_x).max(MIN_CONTENT_EXTENT);
        let content_height = (max_y - min_y).max(MIN_CONTENT_EXTENT);
        let page_width = content_width + PAGE_INSET * 2.0;
        let page_height = PAGE_HEADER_HEIGHT + content_height + PAGE_INSET * 2.0;
        if !min_x.is_finite()
            || !max_y.is_finite()
            || !page_width.is_finite()
            || !page_height.is_finite()
            || !current_y.is_finite()
        {
            return Err(Error::Report(format!(
                "page {} has non-finite extraction mismatch layout dimensions",
                u64::from(page_id.0) + 1
            )));
        }

        layouts.insert(
            *page_id,
            PageLayout {
                page_min_x: min_x,
                page_max_y: max_y,
                page_width,
                page_height,
                y_offset: current_y,
            },
        );
        current_y += page_height + PAGE_GAP;
        if !current_y.is_finite() {
            return Err(Error::Report(format!(
                "page {} causes a non-finite extraction mismatch page offset",
                u64::from(page_id.0) + 1
            )));
        }
    }
    Ok(layouts)
}

fn render_page<W: Write>(
    writer: &mut W,
    page_id: PageId,
    page: &SnapshotPage<'_>,
    layout: &PageLayout,
    mismatch: &SnapshotMismatch,
) -> Result<()> {
    let page_number = u64::from(page_id.0) + 1;
    let page_x = CANVAS_PADDING;
    let page_y = layout.y_offset;
    writeln!(
        writer,
        r#"  <g id="page-{page_number}" class="page-layer">"#
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"    <rect class="page-bg" x="{page_x:.2}" y="{page_y:.2}" width="{:.2}" height="{:.2}" rx="4" />"#,
        layout.page_width, layout.page_height
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"    <text class="page-label" x="{:.2}" y="{:.2}">Page {page_number} (expected: {}, actual: {})</text>"#,
        page_x + PAGE_INSET,
        page_y + 20.0,
        page.expected.len(),
        page.actual.len()
    )
    .map_err(map_io_error)?;

    let content_origin = ContentOrigin {
        x: page_x + PAGE_INSET,
        y: page_y + PAGE_HEADER_HEIGHT + PAGE_INSET,
    };
    for item in &page.expected {
        render_glyph(
            writer,
            *item,
            SnapshotSide::Expected,
            layout,
            content_origin,
            mismatch,
        )?;
    }
    for item in &page.actual {
        render_glyph(
            writer,
            *item,
            SnapshotSide::Actual,
            layout,
            content_origin,
            mismatch,
        )?;
    }

    writeln!(writer, "  </g>").map_err(map_io_error)
}

#[derive(Clone, Copy)]
enum SnapshotSide {
    Expected,
    Actual,
}

impl SnapshotSide {
    const fn class(self) -> &'static str {
        match self {
            Self::Expected => "expected",
            Self::Actual => "actual",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Expected => "Expected",
            Self::Actual => "Actual",
        }
    }

    const fn prefix(self) -> char {
        match self {
            Self::Expected => 'E',
            Self::Actual => 'A',
        }
    }
}

fn render_glyph<W: Write>(
    writer: &mut W,
    item: IndexedGlyph<'_>,
    side: SnapshotSide,
    layout: &PageLayout,
    content_origin: ContentOrigin,
    mismatch: &SnapshotMismatch,
) -> Result<()> {
    let glyph = item.glyph;
    let bbox_width = (glyph.bbox.max.x - glyph.bbox.min.x).max(0.5);
    let bbox_height = (glyph.bbox.max.y - glyph.bbox.min.y).max(0.5);
    let bbox_x = content_origin.x + glyph.bbox.min.x - layout.page_min_x;
    let bbox_y = content_origin.y + layout.page_max_y - glyph.bbox.max.y;
    let baseline_x1 = content_origin.x + glyph.baseline.x - layout.page_min_x;
    let baseline_y1 = content_origin.y + layout.page_max_y - glyph.baseline.y;
    let (baseline_end_x, baseline_end_y) = baseline_endpoint(glyph);
    let baseline_x2 = content_origin.x + baseline_end_x - layout.page_min_x;
    let baseline_y2 = content_origin.y + layout.page_max_y - baseline_end_y;
    let is_mismatch = glyph_is_mismatched(item.index, mismatch);
    let mismatch_class = if is_mismatch { " mismatch" } else { "" };
    let class = side.class();
    let title = glyph_title(side, item.index, glyph);
    let index_y = match side {
        SnapshotSide::Expected => bbox_y - 2.0,
        SnapshotSide::Actual => bbox_y + 10.0,
    };

    writeln!(
        writer,
        r#"    <g class="snapshot-glyph {class}{mismatch_class}" data-side="{class}" data-glyph-index="{}" data-render-order="{}">"#,
        item.index, glyph.render_order
    )
    .map_err(map_io_error)?;
    writeln!(writer, "      <title>{}</title>", xml_escape(&title)).map_err(map_io_error)?;
    writeln!(
        writer,
        r#"      <rect class="snapshot-bbox" x="{bbox_x:.2}" y="{bbox_y:.2}" width="{bbox_width:.2}" height="{bbox_height:.2}" />"#
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"      <line class="snapshot-baseline" x1="{baseline_x1:.2}" y1="{baseline_y1:.2}" x2="{baseline_x2:.2}" y2="{baseline_y2:.2}" />"#
    )
    .map_err(map_io_error)?;
    writeln!(
        writer,
        r#"      <text class="snapshot-index" x="{bbox_x:.2}" y="{index_y:.2}">{}{}</text>"#,
        side.prefix(),
        item.index
    )
    .map_err(map_io_error)?;
    writeln!(writer, "    </g>").map_err(map_io_error)
}

fn validate_snapshot(side: &str, snapshot: &PrimitiveExtractionSnapshot) -> Result<()> {
    for (index, glyph) in snapshot.glyphs.iter().enumerate() {
        let glyph_error =
            |message: String| Error::Report(format!("{side} extraction glyph {index} {message}"));
        let coordinates = [
            ("bbox.min.x", glyph.bbox.min.x),
            ("bbox.min.y", glyph.bbox.min.y),
            ("bbox.max.x", glyph.bbox.max.x),
            ("bbox.max.y", glyph.bbox.max.y),
            ("baseline.x", glyph.baseline.x),
            ("baseline.y", glyph.baseline.y),
            ("direction.x", glyph.direction.x),
            ("direction.y", glyph.direction.y),
        ];
        if let Some((field, value)) = coordinates
            .into_iter()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(glyph_error(format!("has non-finite {field}: {value}")));
        }
        if glyph.bbox.min.x > glyph.bbox.max.x || glyph.bbox.min.y > glyph.bbox.max.y {
            return Err(glyph_error("bbox minimum exceeds its maximum".to_owned()));
        }
        let width = glyph.bbox.max.x - glyph.bbox.min.x;
        let height = glyph.bbox.max.y - glyph.bbox.min.y;
        let direction_length = glyph.direction.x.hypot(glyph.direction.y);
        if !width.is_finite() || !height.is_finite() {
            return Err(glyph_error("has non-finite bbox dimensions".to_owned()));
        }
        if !direction_length.is_finite() || direction_length <= f64::EPSILON {
            return Err(glyph_error(
                "direction must be finite and non-zero".to_owned(),
            ));
        }
        let (end_x, end_y) = baseline_endpoint(glyph);
        if !end_x.is_finite() || !end_y.is_finite() {
            return Err(glyph_error(
                "has non-finite derived baseline endpoint".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_diagnostic_text_limits(
    expected: &PrimitiveExtractionSnapshot,
    actual: &PrimitiveExtractionSnapshot,
    mismatch: &SnapshotMismatch,
) -> Result<()> {
    for glyph in expected.glyphs.iter().chain(actual.glyphs.iter()) {
        if let DecodedText::Mapped(value) = &glyph.text
            && value.len() > MAX_DIAGNOSTIC_GLYPH_TEXT_BYTES
        {
            return Err(Error::LimitExceeded {
                resource: "diagnostic glyph text",
                limit: MAX_DIAGNOSTIC_GLYPH_TEXT_BYTES,
            });
        }
    }
    if let SnapshotMismatch::GlyphField {
        expected, actual, ..
    } = mismatch
        && (expected.len() > MAX_DIAGNOSTIC_MISMATCH_FIELD_BYTES
            || actual.len() > MAX_DIAGNOSTIC_MISMATCH_FIELD_BYTES)
    {
        return Err(Error::LimitExceeded {
            resource: "diagnostic mismatch field",
            limit: MAX_DIAGNOSTIC_MISMATCH_FIELD_BYTES,
        });
    }
    Ok(())
}

fn baseline_endpoint(glyph: &SnapshotGlyph) -> (f64, f64) {
    let width = glyph.bbox.max.x - glyph.bbox.min.x;
    let height = glyph.bbox.max.y - glyph.bbox.min.y;
    let length = width.max(height).max(MIN_BASELINE_LENGTH);
    let direction_length = glyph.direction.x.hypot(glyph.direction.y);
    (
        glyph.baseline.x + glyph.direction.x / direction_length * length,
        glyph.baseline.y + glyph.direction.y / direction_length * length,
    )
}

fn glyph_extent(glyph: &SnapshotGlyph) -> (f64, f64, f64, f64) {
    let (end_x, end_y) = baseline_endpoint(glyph);
    (
        glyph.bbox.min.x.min(glyph.baseline.x).min(end_x),
        glyph.bbox.min.y.min(glyph.baseline.y).min(end_y),
        glyph.bbox.max.x.max(glyph.baseline.x).max(end_x),
        glyph.bbox.max.y.max(glyph.baseline.y).max(end_y),
    )
}

fn glyph_is_mismatched(index: usize, mismatch: &SnapshotMismatch) -> bool {
    match mismatch {
        SnapshotMismatch::GlyphField {
            index: mismatch_index,
            ..
        } => index == *mismatch_index,
        SnapshotMismatch::GlyphCount { .. } => false,
    }
}

fn mismatch_label(mismatch: &SnapshotMismatch) -> String {
    match mismatch {
        SnapshotMismatch::GlyphCount { expected, actual } => {
            format!("Glyph count mismatch: expected {expected}, actual {actual}")
        }
        SnapshotMismatch::GlyphField { index, field, .. } => {
            format!("Glyph {index} field mismatch: {field}")
        }
    }
}

fn glyph_title(side: SnapshotSide, index: usize, glyph: &SnapshotGlyph) -> String {
    let provenance = glyph.provenance.map_or_else(
        || "none".to_owned(),
        |provenance| {
            format!(
                "object {} {} operator {}",
                provenance.content_stream.object_number,
                provenance.content_stream.generation,
                provenance.operator_index
            )
        },
    );
    format!(
        "{} glyph {index} (page {})\nText: {}\nBBox: ({:.2}, {:.2}) - ({:.2}, {:.2})\nBaseline: ({:.2}, {:.2})\nDirection: ({:.4}, {:.4})\nRender order: {}\nProvenance: {provenance}",
        side.label(),
        u64::from(glyph.page.0) + 1,
        text_display(&glyph.text),
        glyph.bbox.min.x,
        glyph.bbox.min.y,
        glyph.bbox.max.x,
        glyph.bbox.max.y,
        glyph.baseline.x,
        glyph.baseline.y,
        glyph.direction.x,
        glyph.direction.y,
        glyph.render_order
    )
}

fn text_display(text: &DecodedText) -> String {
    match text {
        DecodedText::Mapped(value) => value.clone(),
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => format!(
            "[unmapped glyph {glyph_id}, font {}]",
            hex_preview(&font_hash.0)
        ),
    }
}

fn hex_preview(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().min(4) * 2);
    for &byte in bytes.iter().take(4) {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn map_io_error(error: io::Error) -> Error {
    let message = error.to_string();
    if message.contains("diagnostic svg output exceeds") {
        return Error::LimitExceeded {
            resource: "diagnostic svg output",
            limit: MAX_DIAGNOSTIC_SVG_BYTES,
        };
    }
    Error::Report(format!(
        "extraction mismatch svg render i/o failure: {error}"
    ))
}

struct BoundedWriter<'a> {
    inner: &'a mut Vec<u8>,
    limit: usize,
}

impl<'a> BoundedWriter<'a> {
    fn new(inner: &'a mut Vec<u8>, limit: usize) -> Self {
        Self { inner, limit }
    }
}

impl Write for BoundedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.inner.len().saturating_add(buf.len()) > self.limit {
            return Err(io::Error::other(format!(
                "diagnostic svg output exceeds the {} byte limit",
                self.limit
            )));
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
