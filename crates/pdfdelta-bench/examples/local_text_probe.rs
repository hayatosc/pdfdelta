//! Diagnose local proof limits on exported graph views. This does not validate
//! acquisition, correspondence, source closure, or a reusable normalization proof.

use std::{env, fs::File, io::Read, sync::Arc};

use pdfdelta_core::document::{GraphNode, LocalComparisonLimits, compare_text_group_views};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    old: Vec<GraphNode>,
    new: Vec<GraphNode>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if let [mode, path, node] = args.as_slice()
        && mode == "--pdf-node"
    {
        return source_block(path, node.parse()?);
    }
    let [path] = args.as_slice() else {
        return Err("expected VIEW.json or --pdf-node INPUT.pdf NATIVE_NODE_ID".into());
    };
    let mut bytes = Vec::new();
    File::open(path)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("local probe input exceeds 8 MiB".into());
    }
    let input: Input = serde_json::from_slice(&bytes)?;
    let old = input.old.iter().collect::<Vec<_>>();
    let new = input.new.iter().collect::<Vec<_>>();
    let comparison = compare_text_group_views(&old, &new, LocalComparisonLimits::default())?;
    println!("{}", serde_json::to_string(&comparison)?);
    Ok(())
}

fn source_block(path: &str, node: usize) -> Result<(), Box<dyn std::error::Error>> {
    use pdfdelta_core::{
        layout::{reconstruct_blocks, reconstruct_lines},
        normalize::normalize_blocks,
        pdf::{LopdfParser, ParseLimits, PdfParser},
        pipeline::PipelineOptions,
        source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
    };
    let limits = ParseLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("PDF input exceeds the default parser limit".into());
    }
    let pdf = LopdfParser.parse(Arc::from(bytes), limits)?;
    let block_index = node
        .checked_sub(pdf.pages()?.len() + 1)
        .ok_or("node is not a native block")?;
    let extraction =
        ContentStreamGlyphExtractor.extract_outcome(pdf.as_ref(), ExtractionLimits::default())?;
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(extraction.document(), options.line)?;
    let blocks = reconstruct_blocks(extraction.document(), &lines, options.block)?;
    let normalized = normalize_blocks(extraction.document(), &lines, &blocks)?;
    let block = normalized.get(block_index).ok_or("unknown native block")?;
    println!(
        "{}",
        serde_json::json!({
            "raw": block.raw.text, "canonical": block.canonical.text,
            "issues": format!("{:?}", block.issues),
            "events": format!("{:?}", block.normalization_events),
            "extraction_issues": format!("{:?}", extraction.issues()),
        })
    );
    Ok(())
}
