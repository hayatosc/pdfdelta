//! Full flow report with per-token source provenance.
//!
//! The compact report remains the default artifact. This writer appends two
//! sections to the same native schema-13 report: `token_sources`, one source
//! list per token, and `glyph_accounting`, which lists every extracted glyph
//! that is not referenced by a token with an explicit exclusion reason.
//!
//! Token sources are streamed through the serializer. When the side was built
//! in a child process, per-glyph records come from a source cache on disk, so
//! the parent never holds an extracted glyph document while the other side is
//! tokenized. The output bytes are the same in both modes.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use pdfdelta_core::model::{
    DecodedText, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus, TextRenderMode,
};
use pdfdelta_flow::{FlowStream, FlowToken};
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::{
    flow_cmd::FlowComparison, flow_native, flow_sources::SourceCache, fs::write_output_atomically,
};

/// One glyph excluded from the token stream, with the reason.
#[derive(Serialize)]
struct ExcludedGlyph {
    glyph_id: u64,
    page: u32,
    reason: &'static str,
}

#[derive(Default, Serialize)]
struct ExclusionCounts {
    furniture: usize,
    whitespace: usize,
    invisible: usize,
    shared: usize,
    unreferenced: usize,
}

/// Accounting for one side.
#[derive(Serialize)]
struct GlyphAccounting {
    extracted: usize,
    visible: usize,
    token_referenced: usize,
    excluded: Vec<ExcludedGlyph>,
    excluded_counts: ExclusionCounts,
}

/// Text evidence as the full report renders it.
///
/// Borrowing the mapped text avoids building a `serde_json::Value` map for
/// every glyph in a multi-million-glyph document. The serialized shape is the
/// same: `{"mapped": ...}` or `{"unmapped": {...}}`.
#[derive(Serialize)]
#[serde(untagged)]
enum SourceText<'a> {
    Mapped { mapped: &'a str },
    Unmapped { unmapped: UnmappedText },
}

#[derive(Serialize)]
struct UnmappedText {
    identity: &'static str,
    font_hash: String,
    glyph_id: u16,
}

/// One source glyph rendered with every evidence class the extracted glyph
/// carries.
#[derive(Serialize)]
pub(crate) struct SourceGlyph<'a> {
    kind: &'static str,
    glyph_id: u64,
    page: u32,
    text: SourceText<'a>,
    raw_code_hex: HexBytes<'a>,
    bbox: Bounds,
    baseline: Point,
    direction: Point,
    font: FontRecord,
    render_order: u32,
    render_mode: &'static str,
    crop_status: &'static str,
    path_clip_status: &'static str,
    content_stream: ContentStream,
    operator_index: u32,
}

impl<'a> From<&'a Glyph> for SourceGlyph<'a> {
    fn from(glyph: &'a Glyph) -> Self {
        let text = match &glyph.text {
            DecodedText::Mapped(text) => SourceText::Mapped { mapped: text },
            DecodedText::Unmapped {
                font_hash,
                glyph_id,
            } => SourceText::Unmapped {
                unmapped: UnmappedText {
                    identity: font_hash.identity_source().name(),
                    font_hash: lowercase_hex(&font_hash.0),
                    glyph_id: *glyph_id,
                },
            },
        };
        Self {
            kind: "glyph",
            glyph_id: glyph.id.0,
            page: glyph.page.0 + 1,
            text,
            raw_code_hex: HexBytes(&glyph.raw_code),
            bbox: Bounds::new(glyph.bbox),
            baseline: Point::new(glyph.baseline),
            direction: Point::new(glyph.direction),
            font: FontRecord {
                id: glyph.font_id.0,
                size: glyph.font_size,
            },
            render_order: glyph.render_order,
            render_mode: render_mode(glyph.render_mode),
            crop_status: crop_status(glyph.crop_status),
            path_clip_status: path_clip_status(glyph.path_clip_status),
            content_stream: ContentStream {
                object_number: glyph.provenance.content_stream.object_number,
                generation: glyph.provenance.content_stream.generation,
            },
            operator_index: glyph.provenance.operator_index,
        }
    }
}

/// A byte string rendered as lowercase hexadecimal without an allocation for
/// ordinary glyph codes.
struct HexBytes<'a>(&'a [u8]);

impl Serialize for HexBytes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut stack = [0u8; 64];
        if let Some(buffer) = stack.get_mut(..self.0.len().saturating_mul(2)) {
            for (index, byte) in self.0.iter().enumerate() {
                buffer[index * 2] = DIGITS[usize::from(byte >> 4)];
                buffer[index * 2 + 1] = DIGITS[usize::from(byte & 0x0f)];
            }
            let text = std::str::from_utf8(buffer)
                .map_err(|error| serde::ser::Error::custom(error.to_string()))?;
            serializer.serialize_str(text)
        } else {
            serializer.serialize_str(&lowercase_hex(self.0))
        }
    }
}

/// Writes one glyph's full source record and returns its byte length.
///
/// A child builds this record while the extracted document is resident; the
/// reporting parent copies it verbatim, so both paths publish identical bytes.
/// Writing through the destination avoids a second full-size copy of every
/// fragment.
///
/// # Errors
///
/// Returns an error when the record cannot be serialized.
pub(crate) fn write_source_fragment(
    glyph: &Glyph,
    output: &mut dyn std::io::Write,
) -> Result<u64, String> {
    struct Counting<'a> {
        inner: &'a mut dyn std::io::Write,
        written: u64,
    }

    impl std::io::Write for Counting<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let written = self.inner.write(bytes)?;
            self.written = self
                .written
                .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    let mut counting = Counting {
        inner: output,
        written: 0,
    };
    serde_json::to_writer(&mut counting, &SourceGlyph::from(glyph))
        .map_err(|error| format!("cannot serialize a full-report glyph source: {error}"))?;
    Ok(counting.written)
}

#[derive(Serialize)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(point: pdfdelta_core::model::Vec2) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

#[derive(Serialize)]
struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    fn new(rect: pdfdelta_core::model::Rect) -> Self {
        Self {
            min: Point::new(rect.min),
            max: Point::new(rect.max),
        }
    }
}

#[derive(Serialize)]
struct FontRecord {
    id: u32,
    size: f64,
}

#[derive(Serialize)]
struct ContentStream {
    object_number: u32,
    generation: u16,
}

fn render_mode(mode: TextRenderMode) -> &'static str {
    match mode {
        TextRenderMode::Fill => "fill",
        TextRenderMode::Stroke => "stroke",
        TextRenderMode::FillAndStroke => "fill_and_stroke",
        TextRenderMode::Invisible => "invisible",
        TextRenderMode::FillAndClip => "fill_and_clip",
        TextRenderMode::StrokeAndClip => "stroke_and_clip",
        TextRenderMode::FillStrokeAndClip => "fill_stroke_and_clip",
        TextRenderMode::Clip => "clip",
    }
}

fn crop_status(status: GlyphCropStatus) -> &'static str {
    match status {
        GlyphCropStatus::Inside => "inside",
        GlyphCropStatus::PartiallyOutside => "partially_outside",
        GlyphCropStatus::Outside => "outside",
    }
}

fn path_clip_status(status: GlyphPathClipStatus) -> &'static str {
    match status {
        GlyphPathClipStatus::Unclipped => "unclipped",
        GlyphPathClipStatus::Inside => "inside",
        GlyphPathClipStatus::PartiallyOutside => "partially_outside",
        GlyphPathClipStatus::Outside => "outside",
    }
}

#[derive(Serialize)]
struct AccountingSides<'a> {
    old: &'a GlyphAccounting,
    new: &'a GlyphAccounting,
}

/// A prepared side: the glyph source plus validated accounting.
struct Side<'a> {
    cache: &'a SourceCache,
    accounting: GlyphAccounting,
}

fn prepare_side<'a>(
    cache: &'a SourceCache,
    tokens: &'a [FlowToken],
    source_glyphs: &'a [Vec<GlyphId>],
    furniture: &'a [pdfdelta_flow::FurnitureLine],
) -> Result<Side<'a>, String> {
    if source_glyphs.len() != tokens.len() {
        return Err(format!(
            "full report source lists are not aligned with tokens: {} sources, {} tokens",
            source_glyphs.len(),
            tokens.len()
        ));
    }
    let glyph_count = cache.glyph_count();
    let mut counts: HashMap<GlyphId, usize> = HashMap::new();
    for sources in source_glyphs {
        for glyph in sources {
            if cache.page(*glyph).is_none() {
                return Err(format!(
                    "token source {} is not in the extracted glyph set",
                    glyph.0
                ));
            }
            *counts.entry(*glyph).or_default() += 1;
        }
    }
    let furniture_ids: HashSet<GlyphId> = furniture
        .iter()
        .flat_map(|line| line.glyphs.iter().copied())
        .collect();
    let mut excluded = Vec::new();
    let mut exclusion_counts = ExclusionCounts::default();
    let mut visible = 0usize;
    let mut token_referenced = 0usize;
    for index in 0..glyph_count {
        let id = GlyphId(u64::try_from(index).unwrap_or(u64::MAX));
        let page = cache
            .page(id)
            .ok_or_else(|| format!("cached source glyph {} has no page metadata", id.0))?;
        let is_visible = cache
            .is_comparison_visible(id)
            .ok_or_else(|| format!("cached source glyph {} has no visibility metadata", id.0))?;
        let count = counts.get(&id).copied().unwrap_or(0);
        if !is_visible {
            if count != 0 {
                return Err(format!(
                    "invisible glyph {} is referenced by a token source",
                    id.0
                ));
            }
            exclusion_counts.invisible += 1;
            excluded.push(ExcludedGlyph {
                glyph_id: id.0,
                page,
                reason: "invisible",
            });
            continue;
        }
        visible += 1;
        if count > 1 {
            // One source glyph can normalize into or be shared by more than
            // one canonical token (ligatures, fallback records, and shared
            // normalization sources). It stays visible in every token source
            // list but is counted once as a shared exclusion, never silently
            // dropped and never counted as a unique token reference.
            exclusion_counts.shared += 1;
            excluded.push(ExcludedGlyph {
                glyph_id: id.0,
                page,
                reason: "shared_source",
            });
            continue;
        }
        if count == 1 {
            token_referenced += 1;
            continue;
        }
        let reason = if furniture_ids.contains(&id) {
            exclusion_counts.furniture += 1;
            "furniture"
        } else {
            exclusion_counts.whitespace += 1;
            "whitespace"
        };
        excluded.push(ExcludedGlyph {
            glyph_id: id.0,
            page,
            reason,
        });
    }
    excluded.sort_unstable_by_key(|glyph| glyph.glyph_id);
    let accounting = GlyphAccounting {
        extracted: glyph_count,
        visible,
        token_referenced,
        excluded,
        excluded_counts: exclusion_counts,
    };
    Ok(Side { cache, accounting })
}

fn lowercase_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

/// Writes one side's token source array.
fn write_token_sources(
    out: &mut dyn std::io::Write,
    stream: &FlowStream,
    side: &Side<'_>,
) -> Result<(), String> {
    let write_error = |error: std::io::Error| format!("cannot write full flow report: {error}");
    out.write_all(b"[").map_err(write_error)?;
    for (index, token) in stream.tokens.iter().enumerate() {
        if index > 0 {
            out.write_all(b",").map_err(write_error)?;
        }
        write!(out, "{{\"token\":{index},\"text\":").map_err(write_error)?;
        serde_json::to_writer(&mut *out, &token.text)
            .map_err(|error| format!("cannot serialize a full-report token: {error}"))?;
        write!(out, ",\"page\":{},\"sources\":[", token.page.0 + 1).map_err(write_error)?;
        let sources = stream
            .source_glyphs
            .get(index)
            .map_or(&[][..], Vec::as_slice);
        for (position, glyph) in sources.iter().enumerate() {
            if position > 0 {
                out.write_all(b",").map_err(write_error)?;
            }
            side.cache.write_fragment(*glyph, out).map_err(|error| {
                format!("cannot read a cached full-report glyph source: {error}")
            })?;
        }
        out.write_all(b"]}").map_err(write_error)?;
    }
    out.write_all(b"]").map_err(write_error)
}

/// Writes the report object and its streamed source sections.
fn write_report_object(
    out: &mut dyn std::io::Write,
    report: &serde_json::Map<String, Value>,
    old_side: &Side<'_>,
    new_side: &Side<'_>,
    comparison: &FlowComparison,
) -> Result<(), String> {
    let write_error = |error: std::io::Error| format!("cannot write full flow report: {error}");
    out.write_all(b"{").map_err(write_error)?;
    for (index, (key, value)) in report.iter().enumerate() {
        if index > 0 {
            out.write_all(b",").map_err(write_error)?;
        }
        serde_json::to_writer(&mut *out, key)
            .map_err(|error| format!("cannot serialize the full report: {error}"))?;
        out.write_all(b":").map_err(write_error)?;
        serde_json::to_writer(&mut *out, value)
            .map_err(|error| format!("cannot serialize the full report: {error}"))?;
    }
    out.write_all(b",\"token_sources\":{\"old\":")
        .map_err(write_error)?;
    write_token_sources(out, &comparison.old_side.stream, old_side)?;
    out.write_all(b",\"new\":").map_err(write_error)?;
    write_token_sources(out, &comparison.new_side.stream, new_side)?;
    out.write_all(b"},\"glyph_accounting\":")
        .map_err(write_error)?;
    serde_json::to_writer(
        &mut *out,
        &AccountingSides {
            old: &old_side.accounting,
            new: &new_side.accounting,
        },
    )
    .map_err(|error| format!("cannot serialize full-report accounting: {error}"))?;
    out.write_all(b"}").map_err(write_error)
}

/// Writes the full flow report atomically.
///
/// # Errors
///
/// Returns an error when the comparison did not retain sources, when the
/// source invariant fails, or when the report cannot be written.
pub(crate) fn write_report(comparison: &FlowComparison, path: &Path) -> Result<(), String> {
    let old_cache = comparison
        .old_source_cache
        .as_ref()
        .ok_or_else(|| "full report requires a retained old-side source cache".to_owned())?;
    let new_cache = comparison
        .new_source_cache
        .as_ref()
        .ok_or_else(|| "full report requires a retained new-side source cache".to_owned())?;
    let old_side = prepare_side(
        old_cache,
        &comparison.old_side.stream.tokens,
        &comparison.old_side.stream.source_glyphs,
        &comparison.old_side.stream.furniture,
    )?;
    let new_side = prepare_side(
        new_cache,
        &comparison.new_side.stream.tokens,
        &comparison.new_side.stream.source_glyphs,
        &comparison.new_side.stream.furniture,
    )?;
    let compact = flow_native::native_report(comparison);
    let report = compact
        .get("report")
        .and_then(Value::as_object)
        .ok_or_else(|| "compact flow report has no report body".to_owned())?;
    write_output_atomically(path, "full flow JSON report", |out| {
        out.write_all(
            b"{\"artifact_format\":\"pdfdelta-native-full\",\"artifact_version\":2,\
              \"source_schema_version\":13,\"omitted_fields\":{},\"report\":",
        )
        .map_err(|error| format!("cannot write full flow report: {error}"))?;
        write_report_object(out, report, &old_side, &new_side, comparison)?;
        out.write_all(b"}")
            .map_err(|error| format!("cannot write full flow report: {error}"))
    })
}
