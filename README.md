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
- a backend-neutral PDF parser boundary with a `lopdf` adapter for classic xref tables, xref streams, object streams, incremental revisions, inherited page resources, borrowed-password decryption, partial page-tree recovery, and bounded stream decoding;
- a lossless glyph evidence model with geometry and provenance;
- bounded Content Stream glyph extraction for supported text operators, Type 1/Type1C/MMType1, TrueType, axis-aligned Type 3 simple fonts, Identity-H and bounded Identity-V Type 0/CID subsets, inherited resources, normalized page boxes, and isolated Form XObjects;
- standard ToUnicode resource wrappers, Standard/WinAnsi/MacRoman simple-font encodings with Differences and partial-map fallback, canonical Standard 14 identities, bounded full-domain identity Type 0 CMaps, explicit external CID font identity assertions, and stable embedded-font or bounded resource-dependent Type 3 CharProc identities for safely comparable unmapped glyphs;
- configurable Glyph-to-Line reconstruction with synthetic English spaces and preserved arbitrary-angle text lines;
- relative Line-to-Block scoring across page breaks with preserved running-matter roles;
- reversible raw/canonical normalization with scalar-indexed source maps, retained unmapped tokens, and auditable normalization events;
- alignment-only compatibility folding and density-limited numeric masking that never changes exact canonical text;
- unique exact anchors plus swappable n-gram inverted-index and exhaustive candidate generators;
- deterministic anchor-interval alignment for 1:1, insert/delete, constrained adjacent 1:2, 2:1, 1:3, or 3:1 matches, and exact paragraph moves, with ambiguous regions preserved as unresolved;
- an in-house Myers diff over exact canonical and unmapped tokens, with contiguous change spans, formatting-only reports, side-specific coverage, and allocation-aware limits;
- text summaries and versioned JSON reports with explicit extraction completeness, unresolved regions, coverage, and CI-oriented exit decisions;
- a public bounded pipeline from extracted glyphs through exact comparison;
- end-to-end CLI comparison and backend or glyph inspection;
- a reproducible benchmark matrix that applies the five acceptance mutations to programmatic canonical documents, renders each case through literal-`Tj` and positioned-`TJ` PDF paths, and verifies change kind plus document-global span overlap.

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

PDF object parsing uses [`lopdf`](https://github.com/J-F-Liu/lopdf) with its default features disabled, avoiding optional date and parallel-processing dependencies. The dependency is pinned to an [upstream main revision](https://github.com/J-F-Liu/lopdf/commit/1e3d646ca249ebf1a6ff479278c07e9c0f9377a8) that includes merged post-0.44.0 protections pdfdelta relies on: xref-stream entry counts are bounded against the decoded stream body during loading ([#561](https://github.com/J-F-Liu/lopdf/pull/561)), object streams are parsed non-destructively so encoded bytes and filter metadata remain available as evidence ([#562](https://github.com/J-F-Liu/lopdf/pull/562)), the non-standard `/BrotliDecode` filter is supported ([#567](https://github.com/J-F-Liu/lopdf/pull/567)), stream `/Length` mismatches are recovered within xref-bounded objects ([#568](https://github.com/J-F-Liu/lopdf/pull/568)), and a bounded cross-reference reconstruction fallback recovers documents whose startxref or xref data is broken beyond offset repair ([#570](https://github.com/J-F-Liu/lopdf/pull/570)). The project can move to an upstream crates.io release once those changes are published there. Production use of the crate is confined to the PDF backend adapter; test-only code also uses the same pinned build to construct parser and end-to-end fixtures. `Cargo.lock` pins the reviewed dependency graph, and backend upgrades must pass capability and hostile-input fixtures before adoption. This boundary makes replacement possible if maintenance or conformance changes; it does not assume that any external project can provide a permanent maintenance guarantee.

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
mise run ci
cargo run --bin pdfdelta -- --help
cargo run -p pdfdelta-core --example glyph_comparison
```

`pdfdelta-core/examples/glyph_comparison.rs` demonstrates the backend-independent
`Document<Glyph>` API with a small in-memory replacement example. It is useful
for developing and testing the alignment pipeline without needing PDF fixtures.

`mise run ci` runs the same formatting, linting, workspace tests, and benchmark
verification as the GitHub Actions quality gate.

## License

The pdfdelta project code is licensed under the [MIT License](LICENSE). The
Adobe Glyph List 2.0 data embedded in `pdfdelta-core` is distributed under its
original terms; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Usage

```bash
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf --json result.json
pdfdelta old.pdf new.pdf --trace-json trace.json
pdfdelta old.pdf new.pdf --json result.json --trace-json trace.json
pdfdelta old.pdf new.pdf --strict
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
pdfdelta old.pdf new.pdf --old-font-identity TraditionalArabic=windows-v1 --new-font-identity TraditionalArabic=windows-v1
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --glyphs
```

Text reports are written to standard output. `--json PATH` writes a version 5 JSON report to a new path instead and refuses to replace an existing file. Exit code `0` means no content changes, `1` means content changes were found, `2` means the comparison could not run, and `3` means `--strict` rejected an incomplete comparison.

`--trace-json PATH` writes a separate version 1 diagnostic trace without changing the normal report. The trace records input reading, PDF parsing, glyph extraction, layout reconstruction, normalization, alignment, exact diff, and report phases with bounded metrics. It also identifies incomplete or failed phases, records typed resource-limit errors, and marks phases that were skipped after an earlier stop. Trace files use the same atomic, no-overwrite publication policy as JSON reports.

## Current limitations

- `pdfdelta` does not run OCR and does not compare image contents or handwriting. Image-only pages can pass extraction because Image XObjects are explicitly skipped; that does not mean text visible inside the image was compared. Existing OCR text layers are retained as glyph evidence, but invisible text remains outside visible-content comparison. Empty-user-password decryption is automatic; other known passwords can be read from side-specific files and are never accepted directly as argument values or retained in reports and traces.
- Extraction currently targets mainly single-column text using supported Type 1/Type1C/MMType1 or TrueType simple fonts, an axis-aligned Type 3 subset with declared metrics and bounded CharProcs, plus Type 0 fonts with one CIDFontType0/CIDFontType2 descendant and bounded metrics. Identity-H and an Identity-V subset using only default DW2 metrics use fixed two-byte codes; bounded custom Type 0 CMaps are accepted only for full-domain one- or two-byte identity mappings. ToUnicode is optional only when a stable embedded-font, canonical Standard 14 identity, or explicit caller-provided external CID font identity can preserve unmapped glyphs. External identities are trust assertions, not font discovery. Type 3 CharProc drawing operators and MMType1 variation axes are not interpreted, and MMType1 therefore cannot provide stable identity for unmapped glyphs. Rotated, sheared, translated, or horizontally reversed Type 3 FontMatrix values, unsupported fonts inside Type 3 resource graphs, general custom CMaps, per-CID W2 vertical metrics, general vertical-writing reading order, complex tables, and complete annotation or form handling are not implemented. Non-horizontal text lines at arbitrary angles are preserved as independent blocks. Mixed horizontal and non-horizontal lines still use page/top/left ordering, so a tilted line may interleave with body blocks; direction-aware block joining and reading order are not implemented yet.
- Paragraph moves are reported as dedicated `Move` changes when an out-of-order anchor is unique and its canonical text matches exactly. Fuzzy or structurally changed move candidates still fall back to deletion plus insertion changes or unresolved regions.
- PDF text operators, encodings, ToUnicode maps, and Form XObjects are supported only within the bounded subset covered by the backend fixtures.
- Unsupported or unresolved extraction is reported as a typed document- or page-scoped issue in stderr and the text or JSON report.
- The current conservative policy suppresses the entire diff when any extraction gap exists. The default mode returns exit code `0`, while `--strict` returns exit code `3` for the incomplete comparison.
- Page trees with independently valid and invalid branches retain the valid pages and report document-scoped unresolved issues. Default comparison can finish with exit code `0` or `1`; `--strict` returns `3`. Other fatal I/O, backend, malformed-input, and resource-limit failures remain execution errors with exit code `2`.
- Region-aware partial comparison, which would compare proven-safe extracted regions while excluding only affected pages, remains future work.
- Atomic `--json` publication requires a filesystem with same-filesystem hard-link support. Other filesystems return exit code `2` without publishing the report.
- Formatting-only reporting is best-effort and does not claim pixel-level rendering identity.
- The automated benchmark remains intentionally small and in-memory: it uses printable ASCII with Type 1 Helvetica and two deterministic PDF construction paths. Non-vendored public smoke corpora are recorded in [`fixtures/manifests/real-world-pipeline.tsv`](fixtures/manifests/real-world-pipeline.tsv) and [`fixtures/manifests/real-world-pipeline-round2.tsv`](fixtures/manifests/real-world-pipeline-round2.tsv), but downloading and running them is not automated. With documented explicit inputs, the second manifest records 37 strict self-comparison successes and one default-mode partial success that remains strict-incomplete. Independent external document engines and reviewed revision pairs remain future validation work.

## Contributing

Read [`AGENTS.md`](AGENTS.md) for repository working agreements. Keep changes aligned with the staged roadmap in [`SPEC.md`](SPEC.md), verified with the workspace checks above, and small enough to review as one coherent behavior change.
