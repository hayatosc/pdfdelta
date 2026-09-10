# AGENTS.md

## Documentation & Code Comments

- `README.md` may reference `SPEC.md`; do not reference `SPEC`, `SPEC.md`, or specific section numbers in any other code comments, docstrings, or documentation.
- All code comments and documentation must be self-contained: describe contracts, invariants, behaviors, and design rationale directly.
- Follow Rust documentation best practices:
  - Write concise, accurate doc comments (`///`, `//!`) with intra-doc links where applicable.
  - Document invariants, assumptions, pre/post-conditions, `# Errors`, `# Panics`, and `# Safety` boundaries explicitly.
  - Omit redundant comments that merely restate obvious code operations. Focus code comments on non-obvious *why* rationale and architectural decisions.
- Keep `README.md` honest about functionality that is not implemented yet.

## Acceptance & Release Criteria

- Do not redefine the first practical release: it requires all five core acceptance cases:
  1. Line-wrap invariance (no content changes)
  2. Page-break invariance (no content changes)
  3. Text replacement (exact single change)
  4. Paragraph insertion (exact single change)
  5. Paragraph deletion (exact single change)

## Architecture Invariants

- Preserve the pipeline: raw PDF evidence -> imperfect structure -> robust alignment -> exact diff.
- Keep PDF parser-library types behind the neutral `PdfParser` and `ParsedPdf` facade.
- Preserve glyph text, raw codes, geometry, render order, render mode, and object/operator provenance.
- Keep layout reconstruction reversible so alignment can recover from incorrect line or block boundaries.
- Never turn unsupported filters, encrypted input, unmapped glyphs, or uncertain regions into empty text.
- Treat every PDF as untrusted input and enforce explicit resource limits at parser and extraction boundaries.
- Do not add a custom PDF object parser, OCR, semantic models, table recognition, or performance optimizations before concrete needs are demonstrated by fixtures or benchmarks.

## Workspace Boundaries

- `pdfdelta-core` is a pure library and must not depend on CLI concerns.
- `pdfdelta-cli` owns filesystem I/O, argument parsing, report destinations, and process exit codes.
- `pdfdelta-bench` owns generated fixtures, mutations, renderers, manifests, and evaluation tooling.
- Diff-engine components should accept programmatically constructed `Document<Glyph>` fixtures so PDF backend work does not block diff-engine work.

## Project Constraints

- Use stable Rust and Edition 2024; only a future fuzz crate may require nightly.
- Keep backend errors contextual while preserving the distinction between fatal, unsupported, unresolved, and resource-limit outcomes.
- Avoid fixed layout pixel thresholds; use relative metrics and tune concrete values from benchmark evidence.

## Quality Gate

Run these commands before every commit:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --lib
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
```

The `--all-features` clippy and library-test runs compile the fuzzing-only
entry points and run their curated-seed tests on stable Rust. They do not run
libFuzzer; the nightly fuzz workflow is described in `fuzz/README.md`.

Clippy enforces the `all`, `pedantic`, `correctness`, and `suspicious` groups.
`Cargo.toml` documents the intentionally allowed pedantic lints: bounded numeric
casts, exact canonical `f64` comparisons, owned evidence passed by value,
long exhaustive functions, typed error contracts that would otherwise repeat
per-function `# Errors` sections, and domain naming conventions. New code must
satisfy every other pedantic lint.
