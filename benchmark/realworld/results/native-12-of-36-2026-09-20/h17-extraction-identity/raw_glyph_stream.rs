// Temporary complete-Glyph streaming probe (owned scratch copies only).
use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};
use pdfdelta_core::source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor};
use serde::Serialize;
use std::io::Write;

#[derive(Serialize)]
struct Header {
    complete: bool,
    issues: usize,
    glyphs: usize,
    max_operators: usize,
    max_total_decoded_bytes: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pdf_path = args.next().ok_or("usage: probe pdf")?;
    let bytes = std::fs::read(&pdf_path)?;
    let pdf = LopdfParser.parse(bytes.into(), ParseLimits::default())?;
    let limits = ExtractionLimits::default();
    let outcome = ContentStreamGlyphExtractor.extract_outcome(&*pdf, limits)?;
    let glyphs = outcome.document().items();
    let header = Header {
        complete: outcome.is_complete(),
        issues: outcome.issues().len(),
        glyphs: glyphs.len(),
        max_operators: limits.max_operators,
        max_total_decoded_bytes: limits.max_total_decoded_bytes,
    };
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    serde_json::to_writer(&mut out, &header)?;
    writeln!(out)?;
    for glyph in glyphs {
        serde_json::to_writer(&mut out, glyph)?;
        writeln!(out)?;
    }
    out.flush()?;
    Ok(())
}