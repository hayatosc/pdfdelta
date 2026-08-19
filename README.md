# pdfdelta

`pdfdelta` is an early-stage Rust CLI for comparing two born-digital PDF files and reporting meaningful text changes instead of differences in PDF encoding or page layout.

## Why pdfdelta?

A PDF usually stores drawing instructions rather than paragraphs, sentences, or a stable reading order. Extracting plain text and running a conventional diff therefore turns harmless line wrapping and page breaks into changes. Comparing rendered pages has the opposite problem: a new font, margin, or pagination can make nearly every pixel different even when the wording is unchanged.

That noise is especially costly when reviewing contracts, policies, reports, and other documents with an audit trail. A deadline changing from `10 days` to `20 days` must remain an exact, reviewable replacement, while repagination around the same sentence should not hide it among hundreds of false positives.

`pdfdelta` is intended to bridge that gap. It retains glyph geometry and PDF provenance, reconstructs imperfect document structure, aligns corresponding content despite layout drift, and performs the final text diff exactly. When the available evidence is insufficient, it reports unsupported or unresolved regions instead of claiming that no change exists.

## Status

The project now has a limited end-to-end initial version. It can compare supported born-digital PDFs, but the extraction coverage is intentionally narrow and the limitations below matter for real documents.

The current implementation provides:

- a Rust 2024 workspace with separate core, CLI, and benchmark crates;
- a backend-neutral PDF parser boundary with a `lopdf` adapter for classic xref tables, xref streams, object streams, incremental revisions, inherited page resources, and bounded stream decoding;
- a lossless glyph evidence model with geometry and provenance;
- bounded Content Stream glyph extraction for supported text operators, simple fonts, inherited resources, and Form XObjects;
- configurable Glyph-to-Line reconstruction with synthetic English spaces;
- relative Line-to-Block scoring across page breaks with preserved running-matter roles;
- reversible raw/canonical normalization with scalar-indexed source maps, retained unmapped tokens, and auditable normalization events;
- alignment-only compatibility folding and density-limited numeric masking that never changes exact canonical text;
- unique exact anchors plus swappable n-gram inverted-index and exhaustive candidate generators;
- deterministic anchor-interval alignment for 1:1, insert/delete, and constrained adjacent 1:2 or 2:1 matches, with ambiguous regions preserved as unresolved;
- an in-house Myers diff over exact canonical and unmapped tokens, with contiguous change spans, formatting-only reports, side-specific coverage, and allocation-aware limits;
- text summaries and versioned JSON reports with explicit extraction completeness, unresolved regions, coverage, and CI-oriented exit decisions;
- a public bounded pipeline from extracted glyphs through exact comparison;
- end-to-end CLI comparison and backend or glyph inspection.

The comparison pipeline is covered by the five acceptance classes defined in [`SPEC.md`](SPEC.md): line-wrap-only and page-break-only changes produce no content changes, while a text replacement, paragraph insertion, and paragraph deletion each produce one exact change in the generic fixtures.

## Design

The processing pipeline is:

```text
PDF bytes
  -> PDF parser backend
  -> primitive glyph extraction
  -> layout reconstruction
  -> cross-document alignment
  -> exact diff
  -> changes, confidence, coverage, and unresolved regions
```

The core design rules are:

- preserve raw evidence and provenance across every stage;
- keep parser-library types behind a backend-neutral facade;
- make layout reconstruction reversible;
- use tolerant alignment but exact final diffing;
- report unsupported or unresolved content instead of silently treating it as empty text.

Unicode conformance is delegated to the [`unicode-normalization`](https://github.com/unicode-rs/unicode-normalization) and [`unicode-segmentation`](https://github.com/unicode-rs/unicode-segmentation) crates from the `unicode-rs` organization. Their use is confined to the normalization module, and `Cargo.lock` pins the reviewed versions so either implementation can be replaced behind that boundary if its maintenance posture changes.

Typed JSON serialization uses [`serde`](https://github.com/serde-rs/serde) and [`serde_json`](https://github.com/serde-rs/json). Both are confined to the report module rather than the diff model; the lockfile pins reviewed releases, and the versioned report DTO is the replacement boundary if their maintenance posture changes.

PDF object parsing uses [`lopdf`](https://github.com/J-F-Liu/lopdf) with its default features disabled, avoiding optional date and parallel-processing dependencies. The dependency is temporarily pinned to a [reviewed fork commit](https://github.com/hayatosc/lopdf/commit/e7c8b359ab822c4ef82f5d7e0f367f5b4468ee80) while [upstream PR #559](https://github.com/J-F-Liu/lopdf/pull/559) is reviewed. That commit includes merged post-0.44.0 xref-stream fixes, adds an explicit xref-entry limit used by pdfdelta before object parsing, and preserves encoded object-stream evidence during parsing. The project can return to an upstream crates.io release after those protections are published. Production use of the crate is confined to the PDF backend adapter; test-only code also uses the same pinned build to construct parser and end-to-end fixtures. `Cargo.lock` pins the reviewed dependency graph, and backend upgrades must pass capability and hostile-input fixtures before adoption. This boundary makes replacement possible if maintenance or conformance changes; it does not assume that any external project can provide a permanent maintenance guarantee.

See [`SPEC.md`](SPEC.md) for the authoritative technical design and roadmap.

## Workspace

```text
crates/pdfdelta-core   Pure comparison library and neutral data model
crates/pdfdelta-cli    The pdfdelta command-line interface
crates/pdfdelta-bench  Fixture generation and evaluation tooling
```

## Development

The workspace requires stable Rust with Edition 2024 support.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --bin pdfdelta -- --help
```

## Usage

```bash
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf --json result.json
pdfdelta old.pdf new.pdf --strict
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --glyphs
```

Text reports are written to standard output. `--json PATH` writes a versioned JSON report to a new path instead and refuses to replace an existing file. Exit code `0` means no content changes, `1` means content changes were found, `2` means the comparison could not run, and `3` means `--strict` rejected an incomplete comparison.

## Current limitations

- Input must be a born-digital PDF. OCR, scanned pages, handwriting, encrypted documents, and image comparison are not supported.
- Extraction currently targets horizontal, mainly single-column text using supported Type 1 or TrueType simple fonts. Type 0/CID fonts, vertical writing, complex tables, and complete annotation or form handling are not implemented.
- PDF text operators, encodings, ToUnicode maps, and Form XObjects are supported only within the bounded subset covered by the backend fixtures.
- Unsupported or unresolved extraction is reported as a typed document- or page-scoped issue in stderr and the text or JSON report.
- The current conservative policy suppresses the entire diff when any extraction gap exists. The default mode returns exit code `0`, while `--strict` returns exit code `3` for the incomplete comparison.
- Fatal I/O, backend, malformed-input, and resource-limit failures remain execution errors with exit code `2`.
- Region-aware partial comparison, which would compare proven-safe extracted regions while excluding only affected pages, remains future work.
- Atomic `--json` publication requires a filesystem with same-filesystem hard-link support. Other filesystems return exit code `2` without publishing the report.
- Formatting-only reporting is best-effort and does not claim pixel-level rendering identity.

## Contributing

Read [`AGENTS.md`](AGENTS.md) for repository working agreements. Keep changes aligned with the staged roadmap in [`SPEC.md`](SPEC.md), verified with the workspace checks above, and small enough to review as one coherent behavior change.
