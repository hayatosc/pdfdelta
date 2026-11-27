//! Writes the formatting-only fixture pairs used to measure both engines.
//!
//! The generator verifies that the extracted flow token stream is identical
//! between the two revisions. A pair whose words changed would no longer test
//! formatting, so it is rejected instead of published.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use pdfdelta_bench::formatting::formatting_cases;
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
    id: &'static str,
    description: &'static str,
    old: String,
    new: String,
    tokens: Vec<TokenRecord>,
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
            eprintln!("formatting_fixtures: {message}");
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
    let cases = formatting_cases().map_err(|error| error.to_string())?;
    let mut manifest_cases = Vec::with_capacity(cases.len());
    for case in &cases {
        let directory = output.join(case.id());
        fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        let old_pdf = directory.join("old.pdf");
        let new_pdf = directory.join("new.pdf");
        for (path, bytes) in [(&old_pdf, case.old_pdf()), (&new_pdf, case.new_pdf())] {
            fs::write(path, bytes)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        }
        let old_stream = extract_tokens(case.old_pdf(), "old")?;
        let new_stream = extract_tokens(case.new_pdf(), "new")?;
        let old_words = token_words(&old_stream);
        let new_words = token_words(&new_stream);
        if old_words != new_words {
            return Err(format!(
                "{} changed the extracted words; it is not formatting-only: {old_words:?} != {new_words:?}",
                case.id()
            ));
        }
        manifest_cases.push(ManifestCase {
            id: case.id(),
            description: case.description(),
            old: relative(&old_pdf, &output),
            new: relative(&new_pdf, &output),
            tokens: token_records(&old_stream),
        });
        println!(
            "case={} tokens={} words={}",
            case.id(),
            old_stream.tokens.len(),
            join_text(&old_stream)
        );
    }
    let manifest = Manifest {
        schema: "pdfdelta-formatting-fixtures/1",
        cases: manifest_cases,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize manifest: {error}"))?;
    let path = output.join("manifest.json");
    fs::write(&path, bytes).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    println!("wrote {} cases to {}", cases.len(), path.display());
    Ok(())
}

fn parse_output() -> Result<PathBuf, String> {
    let mut args = env::args_os().skip(1);
    let Some(flag) = args.next() else {
        return Err("usage: formatting_fixtures --output DIR".to_owned());
    };
    if flag.to_str() != Some("--output") {
        return Err("usage: formatting_fixtures --output DIR".to_owned());
    }
    let Some(path) = args.next() else {
        return Err("usage: formatting_fixtures --output DIR".to_owned());
    };
    if args.next().is_some() {
        return Err("usage: formatting_fixtures --output DIR".to_owned());
    }
    Ok(PathBuf::from(path))
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

fn token_words(stream: &TokenStream) -> Vec<(String, u64)> {
    stream
        .tokens
        .iter()
        .map(|token| (token.text.clone(), u64::from(token.page.0) + 1))
        .collect()
}

fn join_text(stream: &TokenStream) -> String {
    stream
        .tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
