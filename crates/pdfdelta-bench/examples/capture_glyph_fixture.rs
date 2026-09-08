//! Captures selected PDF pages as source-backed glyph fixtures on stdout.

use std::{
    collections::BTreeSet,
    env,
    fs::File,
    io::{self, Read, Write},
    sync::Arc,
};

use pdfdelta_core::{
    model::{DecodedText, GlyphProvenance},
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err(
            "usage: capture_glyph_fixture INPUT.pdf ZERO_BASED_PAGES (comma-separated)".into(),
        );
    }
    let pages = args[1]
        .split(',')
        .map(str::parse::<u32>)
        .collect::<Result<BTreeSet<_>, _>>()?;
    if pages.is_empty() || pages.len() > 16 {
        return Err("capture requires 1..=16 selected pages".into());
    }
    let limits = ParseLimits::default();
    let mut bytes = Vec::new();
    File::open(&args[0])?
        .take(u64::try_from(limits.max_input_bytes)? + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("fixture input exceeds parser byte limit".into());
    }
    let source_sha256 = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let outcome = source.extract_outcome(Arc::from(bytes), limits, ExtractionLimits::default())?;
    if !outcome.is_complete() {
        return Err(
            "fixture capture requires complete extraction; do not omit extraction issues".into(),
        );
    }
    let document = outcome.document();
    let selected = document
        .items()
        .iter()
        .filter(|glyph| pages.contains(&glyph.page.0));
    let count = selected.clone().count();
    let vector_count = document
        .vector_lines()
        .iter()
        .filter(|line| pages.contains(&line.page.0))
        .count();
    if count == 0 || count.saturating_add(vector_count) > 50_000 {
        return Err("fixture capture requires 1..=50,000 selected glyph/vector records".into());
    }
    let found_pages = selected
        .clone()
        .map(|glyph| glyph.page.0)
        .collect::<BTreeSet<_>>();
    if found_pages != pages {
        return Err("every selected page must contain extracted glyph evidence".into());
    }
    let glyphs = selected.map(|glyph| {
        let text = match &glyph.text {
            DecodedText::Mapped(text) => json!({"mapped": text}),
            DecodedText::Unmapped { font_hash, glyph_id } => json!({
                "unmapped": {"font_hash": font_hash.0, "glyph_id": glyph_id},
            }),
        };
        json!({
            "id": glyph.id.0, "text": text, "raw_code": glyph.raw_code,
            "page": glyph.page.0,
            "bbox_bits": [glyph.bbox.min.x.to_bits(), glyph.bbox.min.y.to_bits(), glyph.bbox.max.x.to_bits(), glyph.bbox.max.y.to_bits()],
            "baseline_bits": [glyph.baseline.x.to_bits(), glyph.baseline.y.to_bits()],
            "direction_bits": [glyph.direction.x.to_bits(), glyph.direction.y.to_bits()],
            "font_id": glyph.font_id.0, "font_size_bits": glyph.font_size.to_bits(),
            "render_order": glyph.render_order, "render_mode": format!("{:?}", glyph.render_mode),
            "crop_status": format!("{:?}", glyph.crop_status),
            "path_clip_status": format!("{:?}", glyph.path_clip_status),
            "provenance": provenance(glyph.provenance),
        })
    }).collect::<Vec<_>>();
    let vector_lines = document
        .vector_lines()
        .iter()
        .filter(|line| pages.contains(&line.page.0))
        .map(|line| {
            json!({
                "id": line.id.0, "page": line.page.0,
        "from_bits": [line.from.x.to_bits(), line.from.y.to_bits()], "to_bits": [line.to.x.to_bits(), line.to.y.to_bits()],
        "width_bits": line.width.to_bits(), "render_order": line.render_order,
                "provenance": provenance(line.provenance),
            })
        })
        .collect::<Vec<_>>();
    let captured = json!({
        "schema_version": 1, "source_sha256": source_sha256, "source_pages": pages,
        "glyphs": glyphs, "vector_lines": vector_lines,
    });
    // Integer float bits retain geometry exactly across JSON implementations.
    let mut writer = io::BufWriter::new(io::stdout().lock());
    serde_json::to_writer(&mut writer, &captured)?;
    writer.flush()?;
    Ok(())
}

fn provenance(value: GlyphProvenance) -> [u32; 3] {
    [
        value.content_stream.object_number,
        u32::from(value.content_stream.generation),
        value.operator_index,
    ]
}
