# AGENTS.md

## Documentation & Code Comments

- `README.md` may reference `docs/SPEC.md`; do not reference `SPEC`, `SPEC.md`, or specific section numbers in any other code comments, docstrings, or documentation.
- All code comments and documentation must be self-contained: describe contracts, invariants, behaviors, and design rationale directly.
- Follow Rust documentation best practices:
  - Write concise, accurate doc comments (`///`, `//!`) with intra-doc links where applicable.
  - Document invariants, assumptions, pre/post-conditions, `# Errors`, `# Panics`, and `# Safety` boundaries explicitly.
  - Omit redundant comments that merely restate obvious code operations. Focus code comments on non-obvious *why* rationale and architectural decisions.
- Keep `README.md` and `docs/limitations.md` honest about functionality that is not implemented yet.
- Keep `README.md` a concise user-facing overview; put detailed CLI behavior in `docs/usage.md`, internals in `docs/how-it-works.md`, and benchmark tooling in `docs/benchmarks.md`.

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
- Do not commit dated benchmark captures, experiment logs, or investigation notes. Write them outside the repository or to the gitignored `benchmark/realworld/results/`; commit only inputs that code, tests, or documented tooling read.
- Diff-engine components should accept programmatically constructed `Document<Glyph>` fixtures so PDF backend work does not block diff-engine work.

## Project Constraints

- Use stable Rust and Edition 2024; only a future fuzz crate may require nightly.
- Keep backend errors contextual while preserving the distinction between fatal, unsupported, unresolved, and resource-limit outcomes.
- Avoid fixed layout pixel thresholds; use relative metrics and tune concrete values from benchmark evidence.

## Memory and Benchmark Storage

- Limit each build, test, or benchmark workload, including all of its child
  processes, to **6 GB (6,000,000,000 bytes)** of memory. Put concurrent work in
  one shared limit; separate 6 GB limits per worker do not enforce this budget.
- On Linux with a user systemd manager and delegated memory controller, wrap
  the entire workload with `systemd-run --user --scope -p MemoryMax=6000000000
  -p MemorySwapMax=0 -- COMMAND`. Verify that the limit is active before a large
  run. If unavailable, use an equivalent container/cgroup limit; a per-process
  virtual-address-space limit alone does not bound aggregate resident memory.
- Start with `CARGO_BUILD_JOBS=1`, `RUST_TEST_THREADS=1`, and one benchmark pair
  at a time. Run build, test, and benchmark phases sequentially. Record OOM,
  timeout, and resource-limit outcomes; do not raise the cap or drop failed pairs
  to improve completion counts.
- Read large reports incrementally. Avoid loading a full JSON report with
  `json.load`, `read_text`, or `jq --slurp`; use a streaming parser and retain
  only the fields needed for the summary. Stream decompression too.
- For native benchmark captures, retain compact diagnostic reports with all
  changes, candidates, unresolved regions, text ranges, and assessment metadata.
  Use `--native-text-only --json report.json.gz` to summarize
  repeated per-glyph drawing references before serialization. Use
  `benchmark/realworld/compact_capture.py` only to migrate existing full reports.
  Keep input/binary hashes and rerun provenance; generate full drawing evidence only for targeted work.
- Prefer compact summaries. When full reports or traces are needed, stream
  them into gzip or zstd, or compress each completed file before starting the
  next pair. Do not retain both compressed and uncompressed copies. Do not
  rewrite source PDFs to save space: their recorded hashes identify the inputs.
- Check `du -sh benchmark/realworld/cache benchmark/realworld/results` and
  `df -h .` before and after runs. Set a run-specific output budget and stop
  capture when it is reached. Bound verbose traces and retain useful failure
  diagnostics instead of unlimited debug output.
- Reuse checksum-verified PDF inputs and binaries. Deduplicate identical reports
  by hash, preserve input/binary/argument provenance and compact run summaries,
  and remove only regenerable artifacts from runs owned by the current task.
  Verify compressed-file integrity and logical-content hashes before removing
  originals; never silently discard another investigation's evidence.

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
