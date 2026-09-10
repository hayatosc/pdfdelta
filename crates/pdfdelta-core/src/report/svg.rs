use std::{
    collections::BTreeMap,
    io::{self, Write},
};

use crate::{
    Error, Result,
    model::{
        DecodedText, Document, Glyph, GlyphCropStatus, GlyphPathClipStatus, PageId, TextRenderMode,
        Vec2,
    },
};

const DEFAULT_PAGE_WIDTH: f64 = 595.0;
const DEFAULT_PAGE_HEIGHT: f64 = 842.0;
const PAGE_PADDING: f64 = 20.0;
const PAGE_GAP: f64 = 30.0;
const HEADER_HEIGHT: f64 = 24.0;

/// Renders a `Document<Glyph>` to an SVG string visualising glyph overlays,
/// bounding boxes, baselines, and provenance metadata.
pub fn render_glyph_overlay_svg(document: &Document<Glyph>) -> Result<String> {
    let mut buffer = Vec::new();
    write_glyph_overlay_svg(document, &mut buffer)?;
    String::from_utf8(buffer)
        .map_err(|error| Error::Report(format!("invalid utf-8 in generated svg: {error}")))
}

/// Writes a glyph overlay SVG visualization to the provided writer.
pub fn write_glyph_overlay_svg<W: Write>(document: &Document<Glyph>, writer: &mut W) -> Result<()> {
    validate_document_geometry(document)?;

    let pages = group_glyphs_by_page(document.items());
    let page_layouts = calculate_page_layouts(&pages)?;

    validate_document_and_layout_geometry(&pages, &page_layouts)?;

    let total_width = page_layouts
        .values()
        .map(|layout| layout.svg_width)
        .fold(DEFAULT_PAGE_WIDTH + PAGE_PADDING * 2.0, f64::max);
    let total_height = page_layouts
        .values()
        .last()
        .map_or(DEFAULT_PAGE_HEIGHT + PAGE_PADDING * 2.0, |layout| {
            layout.y_offset + layout.svg_height + PAGE_GAP
        });

    if !total_width.is_finite() || !total_height.is_finite() {
        return Err(Error::Report(
            "derived document canvas dimensions are non-finite".to_owned(),
        ));
    }

    writeln!(
        writer,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {total_width:.2} {total_height:.2}" width="{total_width:.2}" height="{total_height:.2}">"#
    )
    .map_err(map_io_error)?;

    writeln!(
        writer,
        r"  <defs>
    <style>
      svg {{ background-color: #f4f5f7; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }}
      .page-bg {{ fill: #ffffff; stroke: #d0d5dd; stroke-width: 1px; filter: drop-shadow(0 2px 4px rgba(0,0,0,0.06)); }}
      .page-label {{ font-size: 12px; fill: #475467; font-weight: 600; }}
      .glyph-group {{ cursor: crosshair; }}
      .glyph-bbox {{ fill: rgba(59, 130, 246, 0.08); stroke: rgba(59, 130, 246, 0.35); stroke-width: 0.5px; }}
      .glyph-baseline {{ stroke: rgba(239, 68, 68, 0.6); stroke-width: 0.75px; }}
      .glyph-text {{ fill: rgba(15, 23, 42, 0.85); pointer-events: none; }}
      .glyph-group:hover .glyph-bbox {{ fill: rgba(249, 115, 22, 0.25); stroke: rgba(234, 88, 12, 0.9); stroke-width: 1.5px; }}
      .glyph-group:hover .glyph-baseline {{ stroke: rgba(220, 38, 38, 1.0); stroke-width: 1.5px; }}
      .glyph-unmapped .glyph-bbox {{ fill: rgba(168, 85, 247, 0.12); stroke: rgba(147, 51, 234, 0.5); }}
      .glyph-invisible .glyph-bbox {{ stroke-dasharray: 2,2; stroke: rgba(100, 116, 139, 0.4); }}
      .glyph-crop-partial .glyph-bbox {{ stroke-dasharray: 3,2; stroke: rgba(217, 119, 6, 0.8); }}
      .glyph-crop-outside {{ opacity: 0.45; }}
      .glyph-crop-outside .glyph-bbox {{ stroke-dasharray: 2,2; stroke: rgba(71, 84, 103, 0.8); }}
      .glyph-path-clip-partial .glyph-bbox {{ stroke-dasharray: 4,2; stroke: rgba(202, 138, 4, 0.9); }}
      .glyph-path-clip-outside {{ opacity: 0.45; }}
      .glyph-path-clip-outside .glyph-bbox {{ stroke-dasharray: 1,2; stroke: rgba(71, 84, 103, 0.8); }}
    </style>
  </defs>"
    )
    .map_err(map_io_error)?;

    for (page_id, glyphs) in &pages {
        if let Some(layout) = page_layouts.get(page_id) {
            render_page_svg(writer, *page_id, glyphs, layout)?;
        }
    }

    writeln!(writer, "</svg>").map_err(map_io_error)?;
    Ok(())
}

struct PageLayout {
    page_min_x: f64,
    page_max_y: f64,
    svg_width: f64,
    svg_height: f64,
    y_offset: f64,
}

fn group_glyphs_by_page(glyphs: &[Glyph]) -> BTreeMap<PageId, Vec<&Glyph>> {
    let mut pages = BTreeMap::new();
    for glyph in glyphs {
        pages.entry(glyph.page).or_insert_with(Vec::new).push(glyph);
    }
    if pages.is_empty() {
        pages.insert(PageId(0), Vec::new());
    }
    pages
}

fn calculate_page_layouts(
    pages: &BTreeMap<PageId, Vec<&Glyph>>,
) -> Result<BTreeMap<PageId, PageLayout>> {
    let mut layouts = BTreeMap::new();
    let mut current_y = PAGE_GAP;

    for (page_id, glyphs) in pages {
        let (min_x, min_y, max_x, max_y) = if glyphs.is_empty() {
            (0.0, 0.0, DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT)
        } else {
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;

            for glyph in glyphs {
                min_x = min_x.min(glyph.bbox.min.x);
                min_y = min_y.min(glyph.bbox.min.y);
                max_x = max_x.max(glyph.bbox.max.x);
                max_y = max_y.max(glyph.bbox.max.y);
            }

            let width = (max_x - min_x).max(100.0);
            let height = (max_y - min_y).max(100.0);
            (min_x, min_y, min_x + width, min_y + height)
        };

        let content_width = max_x - min_x;
        let content_height = max_y - min_y;
        let svg_width = content_width + PAGE_PADDING * 2.0;
        let svg_height = content_height + PAGE_PADDING * 2.0 + HEADER_HEIGHT;

        if !min_x.is_finite()
            || !max_y.is_finite()
            || !svg_width.is_finite()
            || !svg_height.is_finite()
            || !current_y.is_finite()
        {
            return Err(Error::Report(format!(
                "page {} has non-finite derived layout dimensions",
                u64::from(page_id.0) + 1
            )));
        }

        layouts.insert(
            *page_id,
            PageLayout {
                page_min_x: min_x,
                page_max_y: max_y,
                svg_width,
                svg_height,
                y_offset: current_y,
            },
        );

        current_y += svg_height + PAGE_GAP;
        if !current_y.is_finite() {
            return Err(Error::Report(format!(
                "page {} causes non-finite cumulative page offset",
                u64::from(page_id.0) + 1
            )));
        }
    }

    Ok(layouts)
}

fn render_page_svg<W: Write>(
    writer: &mut W,
    page_id: PageId,
    glyphs: &[&Glyph],
    layout: &PageLayout,
) -> Result<()> {
    let page_num = u64::from(page_id.0) + 1;
    let page_x = PAGE_PADDING;
    let page_y = layout.y_offset;
    let page_w = layout.svg_width - PAGE_PADDING * 2.0;
    let page_h = layout.svg_height;

    writeln!(writer, r#"  <g id="page-{page_num}" class="page-layer">"#).map_err(map_io_error)?;

    writeln!(
        writer,
        r#"    <rect class="page-bg" x="{page_x:.2}" y="{page_y:.2}" width="{page_w:.2}" height="{page_h:.2}" rx="4" />"#
    )
    .map_err(map_io_error)?;

    let label_x = page_x + 12.0;
    let label_y = page_y + 16.0;
    writeln!(
        writer,
        r#"    <text class="page-label" x="{label_x:.2}" y="{label_y:.2}">Page {page_num} (glyphs: {})</text>"#,
        glyphs.len()
    )
    .map_err(map_io_error)?;

    let content_origin_x = page_x;
    let content_origin_y = page_y + HEADER_HEIGHT;

    for glyph in glyphs {
        render_glyph_svg(
            writer,
            glyph,
            layout.page_min_x,
            layout.page_max_y,
            content_origin_x,
            content_origin_y,
        )?;
    }

    writeln!(writer, r"  </g>").map_err(map_io_error)?;
    Ok(())
}

fn render_glyph_svg<W: Write>(
    writer: &mut W,
    glyph: &Glyph,
    page_min_x: f64,
    page_max_y: f64,
    content_origin_x: f64,
    content_origin_y: f64,
) -> Result<()> {
    let bbox_w = (glyph.bbox.max.x - glyph.bbox.min.x).max(0.5);
    let bbox_h = (glyph.bbox.max.y - glyph.bbox.min.y).max(0.5);
    let bbox_svg_x = content_origin_x + (glyph.bbox.min.x - page_min_x);
    let bbox_svg_y = content_origin_y + (page_max_y - glyph.bbox.max.y);

    let baseline_svg_x1 = content_origin_x + (glyph.baseline.x - page_min_x);
    let baseline_svg_y1 = content_origin_y + (page_max_y - glyph.baseline.y);
    let baseline_svg_x2 = baseline_svg_x1 + glyph.direction.x * glyph.font_size;
    let baseline_svg_y2 = baseline_svg_y1 - glyph.direction.y * glyph.font_size;

    let mut class_names = vec!["glyph-group"];
    match &glyph.text {
        DecodedText::Mapped(_) => {}
        DecodedText::Unmapped { .. } => class_names.push("glyph-unmapped"),
    }
    if glyph.render_mode == TextRenderMode::Invisible {
        class_names.push("glyph-invisible");
    }
    match glyph.crop_status {
        GlyphCropStatus::Inside => {}
        GlyphCropStatus::PartiallyOutside => class_names.push("glyph-crop-partial"),
        GlyphCropStatus::Outside => class_names.push("glyph-crop-outside"),
    }
    match glyph.path_clip_status {
        GlyphPathClipStatus::Unclipped | GlyphPathClipStatus::Inside => {}
        GlyphPathClipStatus::PartiallyOutside => {
            class_names.push("glyph-path-clip-partial");
        }
        GlyphPathClipStatus::Outside => class_names.push("glyph-path-clip-outside"),
    }
    let class_attr = class_names.join(" ");

    let text_display = match &glyph.text {
        DecodedText::Mapped(text) => text.clone(),
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => format!("[U+{glyph_id:04X}:{}]", hex_preview(&font_hash.0)),
    };

    let title_text = format!(
        "Glyph #{id} (Page {page})\nText: {text}\nBBox: ({min_x:.1}, {min_y:.1}) - ({max_x:.1}, {max_y:.1})\nBaseline: ({bx:.1}, {by:.1}) Dir: ({dx:.2}, {dy:.2})\nFont #{font_id}, Size: {font_size:.1}pt\nRender Order: {render_order}, Mode: {render_mode:?}, Crop: {crop_status:?}, Path clip: {path_clip_status:?}\nProvenance: stream {cs_num} {cs_gen} R, op #{op_idx}",
        id = glyph.id.0,
        page = u64::from(glyph.page.0) + 1,
        text = text_display,
        min_x = glyph.bbox.min.x,
        min_y = glyph.bbox.min.y,
        max_x = glyph.bbox.max.x,
        max_y = glyph.bbox.max.y,
        bx = glyph.baseline.x,
        by = glyph.baseline.y,
        dx = glyph.direction.x,
        dy = glyph.direction.y,
        font_id = glyph.font_id.0,
        font_size = glyph.font_size,
        render_order = glyph.render_order,
        render_mode = glyph.render_mode,
        crop_status = glyph.crop_status,
        path_clip_status = glyph.path_clip_status,
        cs_num = glyph.provenance.content_stream.object_number,
        cs_gen = glyph.provenance.content_stream.generation,
        op_idx = glyph.provenance.operator_index,
    );

    writeln!(
        writer,
        r#"    <g class="{class_attr}" data-glyph-id="{id}" data-page="{page}" data-render-order="{render_order}" data-crop-status="{crop_status:?}" data-path-clip-status="{path_clip_status:?}" data-cs-num="{cs_num}" data-cs-gen="{cs_gen}" data-op-idx="{op_idx}">"#,
        id = glyph.id.0,
        page = u64::from(glyph.page.0) + 1,
        render_order = glyph.render_order,
        crop_status = glyph.crop_status,
        path_clip_status = glyph.path_clip_status,
        cs_num = glyph.provenance.content_stream.object_number,
        cs_gen = glyph.provenance.content_stream.generation,
        op_idx = glyph.provenance.operator_index,
    )
    .map_err(map_io_error)?;

    writeln!(writer, r"      <title>{}</title>", xml_escape(&title_text)).map_err(map_io_error)?;

    writeln!(
        writer,
        r#"      <rect class="glyph-bbox" x="{bbox_svg_x:.2}" y="{bbox_svg_y:.2}" width="{bbox_w:.2}" height="{bbox_h:.2}" />"#
    )
    .map_err(map_io_error)?;

    writeln!(
        writer,
        r#"      <line class="glyph-baseline" x1="{baseline_svg_x1:.2}" y1="{baseline_svg_y1:.2}" x2="{baseline_svg_x2:.2}" y2="{baseline_svg_y2:.2}" />"#
    )
    .map_err(map_io_error)?;

    if let DecodedText::Mapped(text) = &glyph.text {
        let is_rotated = glyph.direction != Vec2 { x: 1.0, y: 0.0 };
        let transform_attr = if is_rotated {
            let angle = (-glyph.direction.y).atan2(glyph.direction.x).to_degrees();
            format!(
                r#" transform="rotate({angle:.2}, {baseline_svg_x1:.2}, {baseline_svg_y1:.2})""#
            )
        } else {
            String::new()
        };

        writeln!(
            writer,
            r#"      <text class="glyph-text" x="{baseline_svg_x1:.2}" y="{baseline_svg_y1:.2}" font-size="{font_size:.2}"{transform_attr}>{}</text>"#,
            xml_escape(text),
            font_size = glyph.font_size
        )
        .map_err(map_io_error)?;
    }

    writeln!(writer, r"    </g>").map_err(map_io_error)?;
    Ok(())
}

fn hex_preview(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().min(4) * 2);
    for &byte in bytes.iter().take(4) {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Returns true if `ch` is a legal character in XML 1.0 (Fifth Edition, Section 2.2):
/// `Char ::= #x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] | [#x10000-#x10FFFF]`
fn is_xml_10_valid(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}'
    )
}

/// Escapes XML special characters (`&`, `<`, `>`, `"`, `'`) and renders forbidden XML 1.0
/// code points as explicit deterministic evidence markers `&lt;U+XXXX&gt;`.
///
/// Note: Literal source substrings like `<U+0000>` render identically to generated forbidden
/// codepoint markers (`&lt;U+0000&gt;`). Any disambiguated marker or escaping scheme should
/// be introduced only when demonstrated by benchmark/fixture evidence.
pub(super) fn xml_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        if !is_xml_10_valid(ch) {
            use std::fmt::Write;
            let _ = write!(escaped, "&lt;U+{:04X}&gt;", ch as u32);
            continue;
        }
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn validate_document_geometry(document: &Document<Glyph>) -> Result<()> {
    for glyph in document.items() {
        validate_glyph_geometry(glyph)?;
    }
    Ok(())
}

fn validate_glyph_geometry(glyph: &Glyph) -> Result<()> {
    if glyph.font_size < 0.0 {
        return Err(Error::Report(format!(
            "glyph {} has negative font size: {}",
            glyph.id.0, glyph.font_size
        )));
    }
    let coordinates = [
        ("bbox.min.x", glyph.bbox.min.x),
        ("bbox.min.y", glyph.bbox.min.y),
        ("bbox.max.x", glyph.bbox.max.x),
        ("bbox.max.y", glyph.bbox.max.y),
        ("baseline.x", glyph.baseline.x),
        ("baseline.y", glyph.baseline.y),
        ("direction.x", glyph.direction.x),
        ("direction.y", glyph.direction.y),
        ("font_size", glyph.font_size),
    ];
    for (field, value) in coordinates {
        if !value.is_finite() {
            return Err(Error::Report(format!(
                "glyph {} has non-finite {field}: {value}",
                glyph.id.0
            )));
        }
    }
    Ok(())
}

fn validate_document_and_layout_geometry(
    pages: &BTreeMap<PageId, Vec<&Glyph>>,
    page_layouts: &BTreeMap<PageId, PageLayout>,
) -> Result<()> {
    for (page_id, glyphs) in pages {
        let layout = page_layouts.get(page_id).ok_or_else(|| {
            Error::Report(format!(
                "missing page layout for page {}",
                u64::from(page_id.0) + 1
            ))
        })?;
        let content_origin_x = PAGE_PADDING;
        let content_origin_y = layout.y_offset + HEADER_HEIGHT;

        for glyph in glyphs {
            validate_derived_glyph_geometry(
                glyph,
                layout.page_min_x,
                layout.page_max_y,
                content_origin_x,
                content_origin_y,
            )?;
        }
    }
    Ok(())
}

fn validate_derived_glyph_geometry(
    glyph: &Glyph,
    page_min_x: f64,
    page_max_y: f64,
    content_origin_x: f64,
    content_origin_y: f64,
) -> Result<()> {
    let bbox_w = glyph.bbox.max.x - glyph.bbox.min.x;
    let bbox_h = glyph.bbox.max.y - glyph.bbox.min.y;
    let bbox_svg_x = content_origin_x + (glyph.bbox.min.x - page_min_x);
    let bbox_svg_y = content_origin_y + (page_max_y - glyph.bbox.max.y);

    let baseline_svg_x1 = content_origin_x + (glyph.baseline.x - page_min_x);
    let baseline_svg_y1 = content_origin_y + (page_max_y - glyph.baseline.y);
    let baseline_svg_x2 = baseline_svg_x1 + glyph.direction.x * glyph.font_size;
    let baseline_svg_y2 = baseline_svg_y1 - glyph.direction.y * glyph.font_size;

    let derived = [
        ("bbox width", bbox_w),
        ("bbox height", bbox_h),
        ("bbox svg x", bbox_svg_x),
        ("bbox svg y", bbox_svg_y),
        ("baseline start x", baseline_svg_x1),
        ("baseline start y", baseline_svg_y1),
        ("baseline end x", baseline_svg_x2),
        ("baseline end y", baseline_svg_y2),
    ];

    for (name, val) in derived {
        if !val.is_finite() {
            return Err(Error::Report(format!(
                "glyph {} has non-finite derived {name}",
                glyph.id.0
            )));
        }
    }

    if matches!(&glyph.text, DecodedText::Mapped(_)) && glyph.direction != (Vec2 { x: 1.0, y: 0.0 })
    {
        let angle = (-glyph.direction.y).atan2(glyph.direction.x).to_degrees();
        if !angle.is_finite() {
            return Err(Error::Report(format!(
                "glyph {} has non-finite rotation angle",
                glyph.id.0
            )));
        }
    }

    Ok(())
}

fn map_io_error(error: io::Error) -> Error {
    Error::Report(format!("svg render i/o failure: {error}"))
}
