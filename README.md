# pdfdelta

`pdfdelta` is an early-stage Rust CLI for comparing two born-digital PDF files and reporting meaningful text changes instead of differences in PDF encoding or page layout.

The intended comparison ignores layout-only changes such as line wrapping, pagination, font size, margins, and document generator metadata. It must still preserve exact content changes such as `10 mg` becoming `20 mg`.

## Status

The project is under active development and cannot compare PDF files yet.

The current implementation provides:

- a Rust 2024 workspace with separate core, CLI, and benchmark crates;
- backend-neutral PDF parser and glyph extraction boundaries;
- a lossless glyph evidence model with geometry and provenance;
- a programmatically constructed glyph fixture test;
- configurable Glyph-to-Line reconstruction with synthetic English spaces;
- relative layout scoring for same-page Line-to-Block reconstruction;
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
