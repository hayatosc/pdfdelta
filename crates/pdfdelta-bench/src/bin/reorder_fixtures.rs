//! Writes the reorder fixtures used to measure what each diff engine reports.
//!
//! Each generated pair renders the same token multiset in a different order, so
//! the manifest records both token streams with their pages. The generator
//! refuses to publish a pair whose extracted token multisets differ: that would
//! mean the fixture also changed content and could no longer isolate order.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use pdfdelta_bench::{
    renderers::{RenderLimits, RendererKind},
    reorder::{ReorderCase, reorder_cases},
};
use pdfdelta_core::{
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use pdfdelta_diff::{TokenStream, tokens};
use serde::Serialize;

#[derive(Serialize)]
struct Manifest {
    schema: &'static str,
    cases: Vec<ManifestCase>,
}

#[derive(Serialize)]
struct ManifestCase {
    id: String,
    description: &'static str,
    expectation: &'static str,
    renderers: Vec<ManifestRenderer>,
}

#[derive(Serialize)]
struct ManifestRenderer {
    renderer: &'static str,
    old: String,
    new: String,
    old_tokens: Vec<TokenRecord>,
    new_tokens: Vec<TokenRecord>,
}

#[derive(Serialize)]
struct TokenRecord {
    text: String,
    page: u64,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("reorder_fixtures: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let output = parse_output()?;
    if output.exists() {
        return Err(format!(
            "output path already exists: {}; choose a new directory",
            output.display()
        ));
    }
    fs::create_dir_all(&output)
        .map_err(|error| format!("cannot create {}: {error}", output.display()))?;

    let cases = reorder_cases().map_err(|error| error.to_string())?;
    let mut manifest = Manifest {
        schema: "pdfdelta-reorder-fixtures/1",
        cases: Vec::with_capacity(cases.len()),
    };
    let limits = RenderLimits::default();
    for case in &cases {
        let mut renderers = Vec::new();
        for renderer in RendererKind::all() {
            let record = generate_pair(case, renderer, &output, limits)?;
            println!("{}", describe(case, renderer, &record));
            renderers.push(record);
        }
        manifest.cases.push(ManifestCase {
            id: case.id().to_owned(),
            description: case.description(),
            expectation: case.expectation().label(),
            renderers,
        });
    }
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize manifest: {error}"))?;
    let manifest_path = output.join("manifest.json");
    fs::write(&manifest_path, manifest_bytes)
        .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))?;
    println!("wrote {} cases to {}", cases.len(), manifest_path.display());
    Ok(())
}

fn parse_output() -> Result<PathBuf, String> {
    let mut args = env::args_os().skip(1);
    let Some(flag) = args.next() else {
        return Err("usage: reorder_fixtures --output DIR".to_owned());
    };
    if flag.to_str() != Some("--output") {
        return Err("usage: reorder_fixtures --output DIR".to_owned());
    }
    let Some(path) = args.next() else {
        return Err("usage: reorder_fixtures --output DIR".to_owned());
    };
    if args.next().is_some() {
        return Err("usage: reorder_fixtures --output DIR".to_owned());
    }
    Ok(PathBuf::from(path))
}

fn generate_pair(
    case: &ReorderCase,
    renderer: RendererKind,
    output: &Path,
    limits: RenderLimits,
) -> Result<ManifestRenderer, String> {
    let directory = output.join(case.id()).join(renderer.name());
    fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let old_bytes = renderer
        .render(case.old(), limits)
        .map_err(|error| error.to_string())?;
    let new_bytes = renderer
        .render(case.new_plan(), limits)
        .map_err(|error| error.to_string())?;
    let old_pdf = directory.join("old.pdf");
    let new_pdf = directory.join("new.pdf");
    for (path, bytes) in [(&old_pdf, &old_bytes), (&new_pdf, &new_bytes)] {
        fs::write(path, bytes)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }
    let old_stream = extract_tokens(&old_bytes, "old")?;
    let new_stream = extract_tokens(&new_bytes, "new")?;
    ensure_same_multiset(case.id(), renderer, &old_stream, &new_stream)?;
    Ok(ManifestRenderer {
        renderer: renderer.name(),
        old: relative(&old_pdf, output),
        new: relative(&new_pdf, output),
        old_tokens: token_records(&old_stream),
        new_tokens: token_records(&new_stream),
    })
}

fn relative(path: &Path, output: &Path) -> String {
    path.strip_prefix(output)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn extract_tokens(bytes: &[u8], side: &str) -> Result<TokenStream, String> {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let outcome = source
        .extract_outcome(
            Arc::from(bytes),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .map_err(|error| format!("{side} extraction failed: {error}"))?;
    if !outcome.is_complete() {
        return Err(format!(
            "{side} extraction is incomplete: {}",
            outcome
                .issues()
                .first()
                .map_or("unknown issue", |issue| issue.description())
        ));
    }
    tokens(outcome.document()).map_err(|error| format!("{side} tokenization failed: {error}"))
}

fn token_records(stream: &TokenStream) -> Vec<TokenRecord> {
    stream
        .tokens
        .iter()
        .map(|token| TokenRecord {
            text: token.text.clone(),
            page: u64::from(token.page.0) + 1,
        })
        .collect()
}

fn counts(stream: &TokenStream) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for token in &stream.tokens {
        *counts.entry(token.text.as_str()).or_insert(0) += 1;
    }
    counts
}

fn ensure_same_multiset(
    id: &str,
    renderer: RendererKind,
    old: &TokenStream,
    new: &TokenStream,
) -> Result<(), String> {
    if old.tokens == new.tokens {
        return Err(format!(
            "{} renderer={}: old and new token streams are identical",
            id,
            renderer.name()
        ));
    }
    let old_counts = counts(old);
    let new_counts = counts(new);
    if old_counts != new_counts {
        let texts = old_counts
            .keys()
            .chain(new_counts.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let differences = texts
            .into_iter()
            .filter(|text| old_counts.get(text) != new_counts.get(text))
            .map(|text| {
                format!(
                    "{text:?}: old={} new={}",
                    old_counts.get(text).copied().unwrap_or(0),
                    new_counts.get(text).copied().unwrap_or(0)
                )
            })
            .collect::<Vec<_>>();
        return Err(format!(
            "{} renderer={}: token multisets differ: {}",
            id,
            renderer.name(),
            differences.join(", ")
        ));
    }
    Ok(())
}

fn describe(case: &ReorderCase, renderer: RendererKind, record: &ManifestRenderer) -> String {
    let old_text = join_text(&record.old_tokens);
    let new_text = join_text(&record.new_tokens);
    format!(
        "case={} expectation={} renderer={} tokens={} pages={}\n  old: {old_text}\n  new: {new_text}",
        case.id(),
        case.expectation().label(),
        renderer.name(),
        record.old_tokens.len(),
        page_count(&record.new_tokens),
    )
}

fn join_text(tokens: &[TokenRecord]) -> String {
    tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn page_count(tokens: &[TokenRecord]) -> u64 {
    tokens.iter().map(|token| token.page).max().unwrap_or(0)
}
