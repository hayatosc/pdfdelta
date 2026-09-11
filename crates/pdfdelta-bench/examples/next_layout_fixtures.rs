//! Generate bounded layout/content controls and mutations of hash-bound PDFs.
//! This example writes fixtures only; comparison and source annotation run separately.

use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use clap::Parser;
use lopdf::{Document, Object, dictionary};
use pdfdelta_bench::{
    canonical::{CanonicalDocument, Paragraph},
    mutation::{Mutation, RenderLine, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};
use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
struct Args {
    output: PathBuf,
    #[arg(long)]
    old: PathBuf,
    #[arg(long)]
    new: PathBuf,
    #[arg(long)]
    old_sha256: String,
    #[arg(long)]
    new_sha256: String,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn save(output: &Path, name: &str, bytes: &[u8]) -> Result<Value> {
    fs::write(output.join(name), bytes)?;
    Ok(json!({"file": name, "sha256": digest(bytes), "bytes": bytes.len()}))
}

fn canonical(lines: &[&str]) -> Result<CanonicalDocument> {
    let paragraphs = lines
        .iter()
        .enumerate()
        .map(|(index, text)| Paragraph::new(index.to_string(), *text))
        .collect::<pdfdelta_bench::Result<Vec<_>>>()?;
    Ok(CanonicalDocument::new(paragraphs)?)
}

fn layout(lines: &[&str], name: &str) -> Result<RenderPlan> {
    let document = canonical(lines)?;
    let mutation = match name {
        "plain" => {
            return Ok(RenderPlan::new(
                vec![lines.iter().map(ToString::to_string).collect()],
                30,
            )?);
        }
        "wrap" => Mutation::LineWrap {
            paragraph_id: "0".into(),
            after_word: 3,
        },
        "page_break" => Mutation::PageBreak {
            before_paragraph: 2,
        },
        "font_size" => Mutation::FontSizeChange { new_font_size: 9 },
        "columns" => Mutation::ColumnChange,
        "paint_order" => {
            let positioned = lines
                .iter()
                .enumerate()
                .rev()
                .map(|(row, text)| RenderLine::new(*text, row, 0))
                .collect::<pdfdelta_bench::Result<Vec<_>>>()?;
            return Ok(RenderPlan::positioned(vec![positioned], 30)?);
        }
        _ => return Err("unknown fixture layout".into()),
    };
    Ok(mutation.apply(&document, 30)?.new_plan().clone())
}

fn generated(output: &Path) -> Result<Vec<Value>> {
    let original = [
        "The vessel carries 7 crates.",
        "The cable measures 9 meters.",
        "The valve is open.",
        "The sample belongs to Orion.",
    ];
    let mutations = [
        ("unchanged", 0, original[0]),
        ("number", 0, "The vessel carries 8 crates."),
        ("unit", 1, "The cable measures 9 inches."),
        ("negation", 2, "The valve is not open."),
        ("affiliation", 3, "The sample belongs to Lyra."),
    ];
    let mut pairs = Vec::new();
    for renderer in RendererKind::all() {
        let old = renderer.render(&layout(&original, "plain")?, RenderLimits::default())?;
        let old_record = save(output, &format!("{}-old.pdf", renderer.name()), &old)?;
        for (content, target, replacement) in mutations {
            let mut revised = original;
            revised[target] = replacement;
            for presentation in [
                "plain",
                "wrap",
                "page_break",
                "font_size",
                "columns",
                "paint_order",
            ] {
                let id = format!("{}-{content}-{presentation}", renderer.name());
                let new_plan = layout(&revised, presentation)?;
                let new = renderer.render(&new_plan, RenderLimits::default())?;
                let new_record = save(output, &format!("{id}-new.pdf"), &new)?;
                pairs.push(json!({
                    "id": id, "kind": "authored_control", "producer": renderer.name(),
                    "content_mutation": content, "presentation_mutation": presentation,
                    "old": old_record, "new": new_record,
                    "old_paragraphs": original, "new_paragraphs": revised,
                    "new_render_lines": new_plan.pages(),
                    "changed_paragraph": (content != "unchanged").then_some(target),
                    "expected_changed_scopes": usize::from(content != "unchanged"),
                    "numeric_gold": (content == "number").then(|| json!({
                        "old_paragraph_scalar": 19, "new_paragraph_scalar": 19,
                        "old_value": "7", "new_value": "8", "strict_events": 1,
                        "changed_source_positions": 2,
                    })),
                    "strict_internal_gold_for_other_content": Value::Null,
                }));
            }
        }
    }
    Ok(pairs)
}

fn load(path: &Path, expected: &str) -> Result<Document> {
    let limits = ParseLimits {
        max_input_bytes: 16 * 1024 * 1024,
        max_objects: 50_000,
        max_recursion_depth: 64,
        max_decoded_stream_bytes: 8 * 1024 * 1024,
        max_total_object_stream_bytes: 16 * 1024 * 1024,
        max_pages: 8,
    };
    let mut bytes = Vec::new();
    File::open(path)?
        .take(u64::try_from(limits.max_input_bytes)? + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes || digest(&bytes) != expected {
        return Err("source fixture exceeds its byte limit or differs from its frozen hash".into());
    }
    // The facade enforces aggregate parser limits before the fixture writer loads objects.
    LopdfParser.parse(bytes.clone().into(), limits)?;
    let document = Document::load_mem_with_options(
        &bytes,
        lopdf::LoadOptions::with_max_decompressed_size(limits.max_decoded_stream_bytes),
    )?;
    if document.was_encrypted() {
        return Err("encrypted source fixtures are unsupported".into());
    }
    Ok(document)
}

fn reversed_storage(mut document: Document) -> Document {
    let ids = document.objects.keys().copied().collect::<Vec<_>>();
    let replacements = ids
        .iter()
        .copied()
        .zip(ids.iter().rev().copied())
        .collect::<BTreeMap<_, _>>();
    document.objects = document
        .objects
        .into_iter()
        .map(|(id, object)| (replacements[&id], object))
        .collect();
    // Rename keys first so traversal follows each rewritten reference to its object.
    document.traverse_objects(|object| {
        if let Object::Reference(id) = object
            && let Some(replacement) = replacements.get(id)
        {
            *id = *replacement;
        }
    });
    document
}

fn prepended_page(mut document: Document) -> Result<Document> {
    let catalog = document.trailer.get(b"Root")?.as_reference()?;
    let pages = document
        .get_dictionary(catalog)?
        .get(b"Pages")?
        .as_reference()?;
    let count = document.get_dictionary(pages)?.get(b"Count")?.as_i64()?;
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {},
    });
    let root = document.get_object_mut(pages)?.as_dict_mut()?;
    root.get_mut(b"Kids")?
        .as_array_mut()?
        .insert(0, page.into());
    root.set("Count", count.checked_add(1).ok_or("page count overflow")?);
    Ok(document)
}

fn save_document(output: &Path, name: &str, mut document: Document) -> Result<Value> {
    let mut bytes = Vec::new();
    document.save_to(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("mutated source fixture exceeds the output limit".into());
    }
    save(output, name, &bytes)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let old = load(&args.old, &args.old_sha256)?;
    let new = load(&args.new, &args.new_sha256)?;
    fs::create_dir(&args.output)?;
    let pairs = generated(&args.output)?;
    let mut source_mutations = Vec::new();
    for (side, document) in [("old", old), ("new", new)] {
        let storage = save_document(
            &args.output,
            &format!("{side}-storage.pdf"),
            reversed_storage(document.clone()),
        )?;
        let page = save_document(
            &args.output,
            &format!("{side}-page.pdf"),
            prepended_page(document)?,
        )?;
        source_mutations
            .push(json!({"side": side, "storage": storage, "prepended_blank_page": page}));
    }
    let manifest = json!({
        "version": 1, "comparison_performed": false,
        "generated_pairs": pairs,
        "source_inputs": {"old": args.old, "new": args.new, "old_sha256": args.old_sha256, "new_sha256": args.new_sha256},
        "source_mutations": source_mutations,
        "source_mutation_convention": "Reverse indirect-object numbering/storage without changing paint order; prepend one blank page on each side. Re-resolve source positions and verify rendering before comparison. Reverse the original old/new pair as a separate comparison.",
    });
    fs::write(
        args.output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}
