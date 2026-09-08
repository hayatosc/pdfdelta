//! Compares captured source glyphs through the ordinary comparison pipeline.

#[path = "support/glyph_fixture.rs"]
mod glyph_fixture;

use std::{
    env,
    io::{self, Write},
    path::Path,
};

use pdfdelta_core::{
    pipeline::{PipelineOptions, compare_extraction_outcomes},
    report::write_json,
    source::ExtractionOutcome,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: compare_glyph_fixtures OLD.json NEW.json".into());
    }
    let old = glyph_fixture::read(Path::new(&args[0]))?;
    let new = glyph_fixture::read(Path::new(&args[1]))?;
    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )?;
    let mut writer = io::BufWriter::new(io::stdout().lock());
    write_json(
        &mut writer,
        &outcome.old_blocks,
        &outcome.new_blocks,
        &outcome.old_glyph_evidence,
        &outcome.new_glyph_evidence,
        &outcome.comparison,
        &outcome.extraction,
    )?;
    writer.flush()?;
    Ok(())
}
