//! Controlled prose and table fixtures from two independent local PDF producers.
//! The producers generate inputs only; they are not comparison backends.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use pdfdelta_core::document::{ComparisonContract, TypedOperation};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    BenchError, Result,
    evaluation::BenchmarkProvenance,
    generalization::{
        Dimension, DimensionAnnotation, Fact, GENERALIZATION_SCHEMA_VERSION,
        GeneralizationAnnotation,
    },
    generalization_report::DocumentAnnotation,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum MatrixKind {
    Prose,
    Table,
}

#[derive(Serialize)]
pub struct MatrixDocument {
    pub id: String,
    pub producer_family: String,
    pub producer_version: String,
    pub language: String,
    pub content_mutation: String,
    pub representation_mutation: String,
    pub source: PathBuf,
    pub pdf: PathBuf,
    pub sha256: String,
    /// Ordered paragraph or cell values in the authored source, not extracted text.
    pub content_units: Vec<String>,
}

#[derive(Serialize)]
pub struct MatrixPair {
    pub id: String,
    pub old_pdf: PathBuf,
    pub new_pdf: PathBuf,
    pub old_producer: String,
    pub new_producer: String,
    pub annotation: DocumentAnnotation,
}

#[derive(Serialize)]
pub struct GeneralizationMatrix {
    pub schema_version: u32,
    pub kind: MatrixKind,
    pub documents: Vec<MatrixDocument>,
    pub pairs: Vec<MatrixPair>,
}

/// Generate prose (64 PDFs, 128 pairs) or table (192 PDFs, 384 pairs) controls.
///
/// Crosses English/Japanese, unchanged/number/negation-or-swap/unit content, and
/// normal/narrow/page-break/serif presentation through Typst and Tectonic.
/// Each old producer is paired with both new producers. Expectations come from
/// the source paragraphs or table cells, without inspecting comparison output
/// or graph IDs. Swapping table values retains row labels and the value multiset.
/// Table controls also rename a column, alone and combined with swapped values.
/// Each table presentation is generated with and without borders; authored cell
/// values and change expectations are independent of those drawing choices.
/// Tectonic must have a populated package cache; network fetching is disabled.
/// Executables are trusted local tools, with a 30-second deadline per compilation.
///
/// # Errors
/// Rejects an existing destination, invalid provenance, missing tools/packages,
/// failed or timed-out compilation, PDFs exceeding 64 MiB, and filesystem errors.
pub fn generate_matrix(
    kind: MatrixKind,
    output: &Path,
    typst: &Path,
    tectonic: &Path,
    cache: &Path,
    fonts: &Path,
    first_evaluated: &str,
) -> Result<GeneralizationMatrix> {
    let common = BenchmarkProvenance::parse(
        &[
            "shipping-instructions",
            match kind {
                MatrixKind::Prose => "shipping-prose-v1",
                MatrixKind::Table => "shipping-table-v1",
            },
            "local-producers",
            "declared-per-pair",
            "partial",
            first_evaluated,
            "used_for_fix",
        ],
        1,
    )?;
    let versions = [version(typst)?, version(tectonic)?];
    fs::create_dir(output)
        .map_err(|error| failure(format!("cannot create {}: {error}", output.display())))?;
    let output = output
        .canonicalize()
        .map_err(|error| failure(error.to_string()))?;
    let mut log = fs::File::create(output.join("generation.log"))
        .map_err(|error| failure(error.to_string()))?;
    let mut documents = Vec::new();
    let layouts = [
        ("normal", 420, "Noto Sans CJK JP", false, false),
        ("narrow", 240, "Noto Sans CJK JP", false, false),
        ("page_break", 420, "Noto Sans CJK JP", true, false),
        ("serif", 420, "Noto Serif CJK JP", false, false),
        ("borderless_normal", 420, "Noto Sans CJK JP", false, true),
        ("borderless_narrow", 240, "Noto Sans CJK JP", false, true),
        ("borderless_page_break", 420, "Noto Sans CJK JP", true, true),
        ("borderless_serif", 420, "Noto Serif CJK JP", false, true),
    ];
    for (language, heading, before, after, targets) in [
        (
            "en",
            "Shipping instructions",
            "Inspect the package before dispatch.",
            "Record the measurement after inspection.",
            [
                "The shipment must contain 100 kg of material.",
                "The shipment must contain 200 kg of material.",
                "The shipment must not contain 100 kg of material.",
                "The shipment must contain 100 mg of material.",
            ],
        ),
        (
            "ja",
            "出荷手順",
            "発送前に荷物を検査します。",
            "検査後に測定値を記録します。",
            [
                "出荷する資材の重量は100キログラムとします。",
                "出荷する資材の重量は200キログラムとします。",
                "出荷する資材の重量を100キログラムとしてはいけません。",
                "出荷する資材の重量は100ミリグラムとします。",
            ],
        ),
    ] {
        for (layout, width, font, page_break, borderless) in layouts {
            if kind == MatrixKind::Prose && borderless {
                continue;
            }
            for (mutation, content_units, typst_content, tex_content) in
                content_cases(kind, language, targets, borderless)
            {
                let typst_break = if page_break { "#pagebreak()\n" } else { "" };
                let tex_break = if page_break { "\\newpage\n" } else { "" };
                let typst_source = format!(
                    "#set page(width: {width}pt, height: 595pt, margin: 36pt)\n#set text(font: \"{font}\", size: 12pt, lang: \"{language}\", hyphenate: false)\n#set par(spacing: 24pt)\n\n{heading}\n\n{before}\n\n{typst_break}{typst_content}\n\n{after}\n"
                );
                let tex_source = format!(
                    r"\documentclass[12pt]{{article}}
\usepackage{{fontspec,xeCJK}}
\usepackage[paperwidth={width}bp,paperheight=595bp,margin=36bp]{{geometry}}
\setmainfont{{{font}}}
\setCJKmainfont{{{font}}}
\pagestyle{{empty}}
\setlength{{\parindent}}{{0pt}}
\setlength{{\parskip}}{{24pt}}
\hyphenpenalty=10000
\exhyphenpenalty=10000
\begin{{document}}
{heading}\par
{before}\par
{tex_break}{tex_content}\par
{after}\par
\end{{document}}
"
                );
                for (index, family, extension, source_text, executable) in [
                    (0, "typst", "typ", typst_source, typst),
                    (1, "tectonic", "tex", tex_source, tectonic),
                ] {
                    let id = format!("{family}-{language}-{mutation}-{layout}");
                    let source = output.join(format!("{id}.{extension}"));
                    let pdf = output.join(format!("{id}.pdf"));
                    fs::write(&source, source_text).map_err(|error| failure(error.to_string()))?;
                    writeln!(log, "Generating {id}").map_err(|error| failure(error.to_string()))?;
                    let mut command = Command::new(executable);
                    if index == 0 {
                        command
                            .args([
                                "compile",
                                "--creation-timestamp",
                                "0",
                                "--jobs",
                                "1",
                                "--ignore-system-fonts",
                                "--ignore-embedded-fonts",
                                "--font-path",
                            ])
                            .arg(fonts)
                            .arg(&source)
                            .arg(&pdf);
                    } else {
                        command
                            .env("TECTONIC_CACHE_DIR", cache)
                            .env("SOURCE_DATE_EPOCH", "0")
                            .args(["--only-cached", "--untrusted", "--outdir"])
                            .arg(&output)
                            .arg(&source);
                    }
                    compile(command, &log)
                        .map_err(|error| failure(format!("{id}: {error}; see generation.log")))?;
                    let sha256 = hash_pdf(&pdf)?;
                    documents.push(MatrixDocument {
                        id,
                        producer_family: family.into(),
                        producer_version: versions[index].clone(),
                        language: language.into(),
                        content_mutation: mutation.into(),
                        representation_mutation: layout.into(),
                        source,
                        pdf,
                        sha256,
                        content_units: content_units.clone(),
                    });
                }
            }
        }
    }
    let mut pairs = Vec::new();
    for new in &documents {
        for old in documents.iter().filter(|old| {
            old.language == new.language
                && old.content_mutation == "none"
                && old.representation_mutation == "normal"
        }) {
            let mut provenance = common.clone();
            provenance.document_series_id = format!("shipping-{}", new.language);
            provenance.producer_family =
                format!("{} -> {}", old.producer_family, new.producer_family);
            provenance.producer_version =
                format!("{} -> {}", old.producer_version, new.producer_version);
            let expected = old
                .content_units
                .iter()
                .zip(&new.content_units)
                .filter(|(old, new)| old != new)
                .map(|(old, new)| Fact::ChangeUnit {
                    old: None,
                    new: None,
                    operation: TypedOperation::TextChanged {
                        old: Some(old.clone()),
                        new: Some(new.clone()),
                    },
                })
                .collect();
            pairs.push(MatrixPair {
                id: format!("{}-to-{}", old.producer_family, new.id),
                old_pdf: old.pdf.clone(),
                new_pdf: new.pdf.clone(),
                old_producer: old.producer_family.clone(),
                new_producer: new.producer_family.clone(),
                annotation: DocumentAnnotation {
                    old_sha256: old.sha256.clone(),
                    new_sha256: new.sha256.clone(),
                    provenance,
                    content_mutation: new.content_mutation.clone(),
                    representation_mutation: new.representation_mutation.clone(),
                    expectations: GeneralizationAnnotation {
                        schema_version: GENERALIZATION_SCHEMA_VERSION,
                        channels: ComparisonContract::default().channels,
                        dimensions: vec![DimensionAnnotation {
                            dimension: Dimension::ChangeUnit,
                            alternatives: vec![expected],
                        }],
                    },
                },
            });
        }
    }
    let matrix = GeneralizationMatrix {
        schema_version: 1,
        kind,
        documents,
        pairs,
    };
    let manifest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("manifest.json"))
        .map_err(|error| failure(error.to_string()))?;
    serde_json::to_writer_pretty(manifest, &matrix).map_err(|error| failure(error.to_string()))?;
    Ok(matrix)
}

fn content_cases(
    kind: MatrixKind,
    language: &str,
    paragraphs: [&str; 4],
    borderless: bool,
) -> Vec<(&'static str, Vec<String>, String, String)> {
    if kind == MatrixKind::Prose {
        return ["none", "number", "negation", "unit"]
            .into_iter()
            .zip(paragraphs)
            .map(|(mutation, text)| (mutation, vec![text.into()], text.into(), text.into()))
            .collect();
    }
    let (item, quantity, renamed_quantity, first, second, values) = if language == "ja" {
        (
            "品目",
            "数量",
            "重量",
            "資材甲",
            "資材乙",
            [
                "100キログラム",
                "20キログラム",
                "200キログラム",
                "100ミリグラム",
            ],
        )
    } else {
        (
            "Item",
            "Quantity",
            "Amount",
            "Material A",
            "Material B",
            ["100 kg", "20 kg", "200 kg", "100 mg"],
        )
    };
    let stroke = if borderless { "stroke: none, " } else { "" };
    let rule = if borderless { "" } else { r"\hline" };
    let separator = if borderless { "" } else { "|" };
    [
        ("none", quantity, [values[0], values[1]]),
        ("number", quantity, [values[2], values[1]]),
        ("swap", quantity, [values[1], values[0]]),
        ("unit", quantity, [values[3], values[1]]),
        ("header", renamed_quantity, [values[0], values[1]]),
        ("header_swap", renamed_quantity, [values[1], values[0]]),
    ].into_iter().map(|(mutation, quantity, [a, b])| {
        let typst = format!("#table({stroke}columns: (1fr, 1fr), table.header([{item}], [{quantity}]), [{first}], [{a}], [{second}], [{b}])");
        let tex = format!(r"\begin{{tabular}}{{{separator}p{{0.4\linewidth}}{separator}p{{0.4\linewidth}}{separator}}}{rule}
{item} & {quantity} \\{rule}
{first} & {a} \\{rule}
{second} & {b} \\{rule}
\end{{tabular}}");
        (mutation, [item, quantity, first, a, second, b].map(String::from).to_vec(), typst, tex)
    }).collect()
}

fn failure(message: String) -> BenchError {
    BenchError::InvalidInput(message)
}

fn version(executable: &Path) -> Result<String> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|error| failure(error.to_string()))?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err(failure("producer version command failed".into()));
    }
    let version = String::from_utf8(output.stdout).map_err(|error| failure(error.to_string()))?;
    if version.trim().is_empty() {
        return Err(failure("empty producer version".into()));
    }
    Ok(version.trim().into())
}

fn hash_pdf(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| failure(error.to_string()))?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| failure(error.to_string()))?;
    if bytes.len() > 64 * 1024 * 1024 || !bytes.starts_with(b"%PDF-") {
        return Err(failure(
            "producer output is missing or exceeds the PDF byte limit".into(),
        ));
    }
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn compile(mut command: Command, log: &fs::File) -> Result<()> {
    command
        .stdout(
            log.try_clone()
                .map_err(|error| failure(error.to_string()))?,
        )
        .stderr(
            log.try_clone()
                .map_err(|error| failure(error.to_string()))?,
        );
    let mut child = command
        .spawn()
        .map_err(|error| failure(error.to_string()))?;
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(failure(format!("producer exited with {status}")))
                };
            }
            Err(error) => break Err(failure(error.to_string())),
            Ok(None) => {}
        }
        if started.elapsed() >= Duration::from_secs(30) {
            break Err(failure("producer compilation deadline exceeded".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
