# pdfdelta

`pdfdelta` is an early-stage Rust CLI for comparing two born-digital PDF files and reporting meaningful text changes instead of differences in PDF encoding or page layout.

## Why pdfdelta?

A PDF usually stores drawing instructions rather than paragraphs, sentences, or a stable reading order. Extracting plain text and running a conventional diff therefore turns harmless line wrapping and page breaks into changes. Comparing rendered pages has the opposite problem: a new font, margin, or pagination can make nearly every pixel different even when the wording is unchanged.

That noise is especially costly when reviewing regulated or otherwise high-stakes documents. A change from `10 mg` to `20 mg` must remain an exact, auditable replacement, while repagination around the same sentence should not hide it among hundreds of false positives.

`pdfdelta` is intended to bridge that gap. It retains glyph geometry and PDF provenance, reconstructs imperfect document structure, aligns corresponding content despite layout drift, and performs the final text diff exactly. When the available evidence is insufficient, it reports unsupported or unresolved regions instead of claiming that no change exists.

## Status

The project is under active development and cannot compare PDF files yet.

The current implementation provides:

- a Rust 2024 workspace with separate core, CLI, and benchmark crates;
- backend-neutral PDF parser and glyph extraction boundaries;
- a lossless glyph evidence model with geometry and provenance;
- a programmatically constructed glyph fixture test;
- configurable Glyph-to-Line reconstruction with synthetic English spaces;
- relative Line-to-Block scoring across page breaks with preserved running-matter roles;
- reversible raw/canonical normalization with scalar-indexed source maps, retained unmapped tokens, and auditable normalization events;
- alignment-only compatibility folding and density-limited numeric masking that never changes exact canonical text;
- unique exact anchors plus swappable n-gram inverted-index and exhaustive candidate generators;
- deterministic anchor-interval alignment for 1:1, insert/delete, and constrained adjacent 1:2 or 2:1 matches, with ambiguous regions preserved as unresolved;
- an in-house Myers diff over exact canonical and unmapped tokens, with contiguous change spans, formatting-only reports, side-specific coverage, and allocation-aware limits;
- the initial `pdfdelta --help` command surface.

The first practical release is defined by five acceptance cases in [`SPEC.md`](SPEC.md): line-wrap-only and page-break-only changes produce no content changes, while replacement, paragraph insertion, and paragraph deletion each produce one exact change.

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

The planned CLI forms are:

```bash
pdfdelta inspect document.pdf
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf --json result.json
```

These comparison and inspection operations currently return a not-implemented error. Check the status section before relying on any command beyond `--help`.

## Contributing

Read [`AGENTS.md`](AGENTS.md) for repository working agreements. Keep changes aligned with the staged roadmap in [`SPEC.md`](SPEC.md), verified with the workspace checks above, and small enough to review as one coherent behavior change.
