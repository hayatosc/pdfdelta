use std::{fs::File, io::Read, path::Path};

use pdfdelta_core::{
    model::{
        DecodedText, Document, FontId, FontProgramHash, Glyph, GlyphCropStatus, GlyphId,
        GlyphPathClipStatus, GlyphProvenance, PageId, Rect, TextRenderMode, Vec2, VectorLine,
        VectorLineId,
    },
    pdf::ObjectRef,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CapturedText {
    Mapped(String),
    Unmapped { font_hash: Vec<u8>, glyph_id: u16 },
}

pub fn read(path: &Path) -> FixtureResult<Document<Glyph>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("glyph fixture exceeds 32 MiB".into());
    }
    let captured: Value = serde_json::from_slice(&bytes)?;
    if field::<u32>(&captured, "schema_version")? != 1 {
        return Err("unsupported glyph fixture version".into());
    }
    let glyphs = captured["glyphs"]
        .as_array()
        .ok_or("glyphs must be an array")?;
    let lines = captured["vector_lines"]
        .as_array()
        .ok_or("vector_lines must be an array")?;
    if glyphs.len().saturating_add(lines.len()) > 50_000 {
        return Err("glyph fixture exceeds 50,000 source records".into());
    }
    let glyphs = glyphs
        .iter()
        .map(glyph)
        .collect::<FixtureResult<Vec<_>>>()?;
    let lines = lines
        .iter()
        .map(vector_line)
        .collect::<FixtureResult<Vec<_>>>()?;
    Ok(Document::with_vector_lines(glyphs, lines))
}

fn field<T: DeserializeOwned>(value: &Value, key: &str) -> FixtureResult<T> {
    let field = value
        .get(key)
        .ok_or_else(|| format!("missing glyph fixture field {key}"))?;
    serde_json::from_value(field.clone())
        .map_err(|error| format!("invalid glyph fixture {key}: {error}").into())
}

fn point(value: &Value, key: &str) -> FixtureResult<Vec2> {
    let [x, y]: [u64; 2] = field(value, key)?;
    Ok(Vec2 {
        x: f64::from_bits(x),
        y: f64::from_bits(y),
    })
}

fn provenance(value: &Value) -> FixtureResult<GlyphProvenance> {
    let [object_number, generation, operator_index]: [u32; 3] = field(value, "provenance")?;
    Ok(GlyphProvenance {
        content_stream: ObjectRef {
            object_number,
            generation: u16::try_from(generation)?,
        },
        operator_index,
    })
}

fn glyph(value: &Value) -> FixtureResult<Glyph> {
    let [x0, y0, x1, y1]: [u64; 4] = field(value, "bbox_bits")?;
    let text = match field(value, "text")? {
        CapturedText::Mapped(text) => DecodedText::Mapped(text),
        CapturedText::Unmapped {
            font_hash,
            glyph_id,
        } => DecodedText::Unmapped {
            font_hash: FontProgramHash(font_hash),
            glyph_id,
        },
    };
    let render_mode = match field::<String>(value, "render_mode")?.as_str() {
        "Fill" => TextRenderMode::Fill,
        "Stroke" => TextRenderMode::Stroke,
        "FillAndStroke" => TextRenderMode::FillAndStroke,
        "Invisible" => TextRenderMode::Invisible,
        "FillAndClip" => TextRenderMode::FillAndClip,
        "StrokeAndClip" => TextRenderMode::StrokeAndClip,
        "FillStrokeAndClip" => TextRenderMode::FillStrokeAndClip,
        "Clip" => TextRenderMode::Clip,
        _ => return Err("unknown captured render mode".into()),
    };
    let crop_status = match field::<String>(value, "crop_status")?.as_str() {
        "Inside" => GlyphCropStatus::Inside,
        "PartiallyOutside" => GlyphCropStatus::PartiallyOutside,
        "Outside" => GlyphCropStatus::Outside,
        _ => return Err("unknown captured crop status".into()),
    };
    let path_clip_status = match field::<String>(value, "path_clip_status")?.as_str() {
        "Unclipped" => GlyphPathClipStatus::Unclipped,
        "Inside" => GlyphPathClipStatus::Inside,
        "PartiallyOutside" => GlyphPathClipStatus::PartiallyOutside,
        "Outside" => GlyphPathClipStatus::Outside,
        _ => return Err("unknown captured path clip status".into()),
    };
    Ok(Glyph {
        id: GlyphId(field(value, "id")?),
        text,
        raw_code: field(value, "raw_code")?,
        page: PageId(field(value, "page")?),
        bbox: Rect {
            min: Vec2 {
                x: f64::from_bits(x0),
                y: f64::from_bits(y0),
            },
            max: Vec2 {
                x: f64::from_bits(x1),
                y: f64::from_bits(y1),
            },
        },
        baseline: point(value, "baseline_bits")?,
        direction: point(value, "direction_bits")?,
        font_id: FontId(field(value, "font_id")?),
        font_size: f64::from_bits(field(value, "font_size_bits")?),
        render_order: field(value, "render_order")?,
        render_mode,
        crop_status,
        path_clip_status,
        provenance: provenance(value)?,
    })
}

fn vector_line(value: &Value) -> FixtureResult<VectorLine> {
    Ok(VectorLine {
        id: VectorLineId(field(value, "id")?),
        page: PageId(field(value, "page")?),
        from: point(value, "from_bits")?,
        to: point(value, "to_bits")?,
        width: f64::from_bits(field(value, "width_bits")?),
        render_order: field(value, "render_order")?,
        provenance: provenance(value)?,
    })
}
