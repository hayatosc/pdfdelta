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
- recursive XY-Cut region partitioning with spatial Region Graphs and relative Line-to-Block scoring across page breaks with preserved running-matter roles;
- reversible raw/canonical normalization with scalar-indexed source maps, retained unmapped tokens, and auditable normalization events;
- alignment-only compatibility folding and density-limited numeric masking that never changes exact canonical text;
- unique exact anchors plus swappable n-gram inverted-index, MinHash LSH, and exhaustive candidate generators;
- deterministic anchor-interval alignment for 1:1, insert/delete, constrained adjacent 1:2, 2:1, 1:3, or 3:1 matches, and exact paragraph moves, with ambiguous regions preserved as unresolved;
- an in-house Myers diff over exact canonical and unmapped tokens, with contiguous change spans, formatting-only reports, side-specific coverage, and allocation-aware limits;
- text summaries and versioned JSON reports with explicit extraction completeness, unresolved regions, coverage, and CI-oriented exit decisions;
- a public bounded pipeline from extracted glyphs through exact comparison;
- end-to-end CLI comparison, output redirection, quiet mode for CI, shell completion generation, and backend, object, glyph, or SVG overlay inspection;
- a reproducible benchmark matrix that applies 15 unique acceptance and layout mutations to programmatic canonical documents, renders each case through literal-`Tj` and positioned-`TJ` PDF paths (30 records total), complemented by externally rendered Typst raw-PDF revision pairs for SPEC §2.2 Case 3 in [`fixtures/external/case3-typst/`](fixtures/external/case3-typst/), SPEC §2.1 Japanese replacement in [`fixtures/external/japanese-typst/`](fixtures/external/japanese-typst/), SPEC §2.2 Case 1 Japanese line wrap in [`fixtures/external/case1-japanese-typst/`](fixtures/external/case1-japanese-typst/), SPEC §2.2 Case 2 Japanese page break in [`fixtures/external/case2-japanese-typst/`](fixtures/external/case2-japanese-typst/), and SPEC §2.2 Case 4 / Case 5 Japanese paragraph insertion and deletion in [`fixtures/external/case4-case5-japanese-typst/`](fixtures/external/case4-case5-japanese-typst/);
- a non-vendored real-world revision-pair benchmark track with download provenance and SHA-256 checksums, development versus holdout treatment, human-reviewed expected changes for a representative subset, and revision-diff quality metrics (coverage, unresolved token share, recall, precision where fully annotated, change-kind accuracy, fragmentation, suspicious tiny edits) reported separately from extraction conformance.

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

## Real-world revision benchmarks

Self-comparison of a document with itself cannot exercise alignment under real
edits. [`benchmark/realworld/`](benchmark/realworld/) therefore records a
separate non-vendored corpus of genuine public revision pairs (`old -> new`)
with per-side provenance (stable URL, capture date, byte size, SHA-256),
development versus holdout treatment, scope flags, and human-reviewed expected
changes for a representative subset in `expected/*.json`. Documents are never
committed to this repository.

```bash
# download both sides of every pair and verify byte counts and checksums
benchmark/realworld/fetch.sh
cargo run -p pdfdelta-bench -- revisions --cache-dir benchmark/realworld/cache --checksums-only

# run comparisons and report metrics (add --summary-json-output summary.json for deterministic compact summaries or --json-output report.json for full reports)
cargo run -p pdfdelta-bench -- revisions --cache-dir benchmark/realworld/cache
```

The command reports extraction conformance separately from revision-diff
quality: extraction completeness, alignment coverage, unresolved regions with
their comparable-token share, reported change counts, and — where expected
annotations exist — recall, precision (complete annotations only),
change-kind accuracy, fragmentation (reported hunks per matched change and,
for complete annotations, per expected semantic change), and unmatched one-
or two-token edits that bad alignment tends to fabricate. Unresolvable
spans are counted separately and never inflate the tiny-edit signal. Every
pair ends in exactly one status: `OK` (compared), `LIMIT` (the comparison
stopped at a documented resource budget before producing a diff), or `FAIL`
(provenance, expectation, or execution failure). `--set dev|holdout` and
`--pair <id>` select subsets; `--limit-scale <factor>` uniformly scales the
comparison pipeline budgets (n-gram token elements, alignment candidate
visits, alignment DP cells, diff token and edit-distance limits; parser and
extraction limits are untouched) and never weakens documented defaults,
while omitting it applies each pair's recorded `limit_scale_hint`. Exit
code 1 signals provenance failures, expectation mismatches, or pairs that
ended `LIMIT`/`FAIL`; low quality scores never fail a run because the dated
captures serve as calibration evidence and current fragmentation and recall
remain too unstable for rigid quality-gate thresholds (confidence calibration
has landed, but broader large-document alignment quality remains ongoing work).
`--json-output` and `--summary-json-output` are published independently and
atomically (each serializing in memory and publishing via same-directory temporary
files without overwriting existing destinations). If the second publication fails,
exit code 2 is returned and the first published artifact remains in place without
pairwise rollback across distinct paths. Dated compact evaluation summaries are
recorded under [`benchmark/realworld/results/`](benchmark/realworld/results/README.md)
(e.g. [`2026-08-26.json`](benchmark/realworld/results/2026-08-26.json)).

## License

The pdfdelta project code is licensed under the [MIT License](LICENSE). The
Adobe Glyph List 2.0 data embedded in `pdfdelta-core` is distributed under its
original terms; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Usage

```bash
# Compare two PDFs and output diff to terminal
pdfdelta old.pdf new.pdf

# Compare using standard input
cat old.pdf | pdfdelta - new.pdf

# Save reports to files (-o for text diff, -j for machine-readable JSON)
pdfdelta old.pdf new.pdf -o diff.txt
pdfdelta old.pdf new.pdf -j result.json
pdfdelta old.pdf new.pdf --trace-json trace.json

# CI usage (exit codes: 0 = unchanged, 1 = changed, 2 = error, 3 = strict incomplete)
pdfdelta -q -s old.pdf new.pdf

# Color control
pdfdelta old.pdf new.pdf --color always

# Raise comparison budgets for large documents without changing parser or extraction limits
pdfdelta old.pdf new.pdf --limit-scale 16

# Password-protected PDFs and custom font identity assertions
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
pdfdelta old.pdf new.pdf --old-font-identity TraditionalArabic=windows-v1 --new-font-identity TraditionalArabic=windows-v1

# Inspect PDF internal structures, backend info, objects, or glyphs
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --objects
pdfdelta inspect document.pdf --glyphs

# Generate shell auto-completions (bash, zsh, fish, powershell, elvish)
pdfdelta completions bash > ~/.local/share/bash-completion/completions/pdfdelta
```

Text reports are written to standard output as contextual unified-diff hunks: a one-line summary, `---` / `+++` file headers, and `@@ page N … @@` hunks with `-` / `+` markers, bounded surrounding context, one-based page numbers, explicit unresolved regions, and presentation-only grouping of nearby exact changes. `--color auto|always|never` controls ANSI color (`auto`, the default, colorizes only when stdout is a terminal; color supplements the markers and is never required to read the output). Standard input can be supplied as `-` for either PDF input. `-o, --output PATH` publishes the human-readable text report atomically to a new file instead of standard output. `-q, --quiet` suppresses standard-output reports for exit-code-only CI workflows. The typed JSON report is unchanged by presentation options: `-j, --json PATH` writes a version 7 JSON report to a new path and refuses to replace an existing file. Exit code `0` means no content changes, `1` means content changes were found, `2` means the comparison could not run, and `3` means `--strict` rejected an incomplete comparison.

`--limit-scale FACTOR` uniformly raises the comparison pipeline budgets for n-gram token elements, alignment candidate visits, alignment DP cells, diff tokens, and diff edit distance. The factor must be finite and at least `1`; parser and extraction limits remain unchanged.

`--trace-json PATH` writes a separate version 1 diagnostic trace without changing the normal report. The trace records input reading, PDF parsing, glyph extraction, layout reconstruction, normalization, alignment, exact diff, and report phases with bounded metrics. It also identifies incomplete or failed phases, records typed resource-limit errors, and marks phases that were skipped after an earlier stop. Trace files use the same atomic, no-overwrite publication policy as JSON reports.

## Current limitations

- `pdfdelta` does not run OCR and does not compare image contents or handwriting. Image-only pages can pass extraction because Image XObjects are explicitly skipped; that does not mean text visible inside the image was compared. Existing OCR text layers are retained as glyph evidence, but invisible text remains outside visible-content comparison. Empty-user-password decryption is automatic; other known passwords can be read from side-specific files and are never accepted directly as argument values or retained in reports and traces.
- Extraction currently targets mainly single-column text using supported Type 1/Type1C/MMType1 or TrueType simple fonts, an axis-aligned Type 3 subset with declared metrics and bounded CharProcs, plus Type 0 fonts with one CIDFontType0/CIDFontType2 descendant and bounded metrics. Identity-H and an Identity-V subset using only default DW2 metrics use fixed two-byte codes; bounded custom Type 0 CMaps are accepted only for full-domain one- or two-byte identity mappings. ToUnicode is optional only when a stable embedded-font, canonical Standard 14 identity, or explicit caller-provided external CID font identity can preserve unmapped glyphs. External identities are trust assertions, not font discovery. Type 3 CharProc drawing operators and MMType1 variation axes are not interpreted, and MMType1 therefore cannot provide stable identity for unmapped glyphs. Rotated, sheared, translated, or horizontally reversed Type 3 FontMatrix values, unsupported fonts inside Type 3 resource graphs, general custom CMaps, per-CID W2 vertical metrics, general vertical-writing reading order, complex tables, and complete annotation or form handling are not implemented. A simple left-to-right horizontal region and an ordinary two-column page are treated as known only when glyph paint order independently confirms top-to-bottom lines and, for columns, the complete left column before the complete right column. Three or more regions, row-interleaved columns, headers combined with columns, right-to-left text, non-horizontal text, and mixed or unknown text direction preserve every line and glyph but report their page-local comparison window as unresolved reading order; extraction itself remains complete, and changes in independently anchored windows continue to be compared.
- Paragraph moves are reported as dedicated `Move` changes when an out-of-order anchor is unique and its canonical text matches exactly. Fuzzy or structurally changed move candidates still fall back to deletion plus insertion changes or unresolved regions.
- PDF text operators, encodings, ToUnicode maps, and Form XObjects are supported only within the bounded subset covered by the backend fixtures.
- Unsupported or unresolved extraction is reported as a typed document-, page-, page-tree-gap-, or glyph-gap-scoped issue in stderr and the text or JSON report.
- Page- and glyph-scoped extraction gaps are mapped from their side-local retained evidence to monotone exact-anchor windows. Recoverable Form XObject failures roll back that invocation's glyphs while retaining text before and after it. Affected windows are reported as unresolved, while proven windows continue through alignment and exact diff; if no usable anchors exist, all retained content remains unresolved. Default mode returns exit code `0` or `1` according to known content changes, while `--strict` returns exit code `3` for the incomplete comparison.
- Document-scoped extraction gaps still suppress the entire diff because the missing order interval cannot be located safely. Page trees with independently valid and invalid branches retain the valid pages and report side-local page-tree-gap boundaries; unexplained root count mismatches remain document-scoped. Default comparison follows known changes, while `--strict` returns `3` for either incomplete case. Other fatal I/O, backend, malformed-input, and resource-limit failures remain execution errors with exit code `2`.
- Finer region-scoped gap boundaries and recovery of changes inside an unresolved anchor window remain future work.
- Atomic `--json` publication requires a filesystem with same-filesystem hard-link support. Other filesystems return exit code `2` without publishing the report.
- Formatting-only reporting is best-effort and does not claim pixel-level rendering identity.
- Engine-generated replacements receive `CharacterWidth` only when an explicit fullwidth/halfwidth fold exactly explains the changed hunk. `OcrConfusion` remains available only to programmatic callers because OCR is not implemented.
- JSON change, formatting, and unresolved spans include glyph geometry plus content-stream object and operator provenance. Text reports do not list per-span provenance, and SVG output remains a whole-document glyph overlay rather than a per-change diff overlay.
- The automated PDF mutation benchmark remains intentionally small and in-memory: it uses printable ASCII with Type 1 Helvetica and two deterministic PDF construction paths (15 unique mutation cases × 2 project renderers = 30 records), including a column-major two-column reflow fixture. A parser-independent Glyph-to-Line matrix separately covers single-column English, mixed font sizes, superscripts, reconstructed English spaces, and decoded horizontal Japanese/Latin text. These are complemented by vendored external Typst raw-PDF revision pairs for SPEC §2.2 Case 3 (`Release 10` -> `Release 20`) in [`fixtures/external/case3-typst/`](fixtures/external/case3-typst/), SPEC §2.1 Japanese horizontal born-digital text replacement in [`fixtures/external/japanese-typst/`](fixtures/external/japanese-typst/), SPEC §2.2 Case 1 Japanese line-wrap invariance in [`fixtures/external/case1-japanese-typst/`](fixtures/external/case1-japanese-typst/), SPEC §2.2 Case 2 Japanese page-break invariance in [`fixtures/external/case2-japanese-typst/`](fixtures/external/case2-japanese-typst/), and SPEC §2.2 Case 4 / Case 5 Japanese paragraph insertion and deletion in [`fixtures/external/case4-case5-japanese-typst/`](fixtures/external/case4-case5-japanese-typst/). Non-vendored public smoke corpora are recorded in [`benchmark/manifests/real-world-pipeline.tsv`](benchmark/manifests/real-world-pipeline.tsv) and [`benchmark/manifests/real-world-pipeline-round2.tsv`](benchmark/manifests/real-world-pipeline-round2.tsv), but downloading and running them is not automated. With documented explicit inputs, the second manifest records 37 strict self-comparison successes and one default-mode partial success that remains strict-incomplete. Additional external renderer dialects (LaTeX, HTML/Chromium) and broader Japanese raw-PDF corpus fixtures (such as vertical writing or non-Identity-H encodings) remain future evidence validation work.
- The real-world revision track ([`benchmark/realworld/`](benchmark/realworld/)) currently records five genuine public pairs: NIST FIPS 186-4 -> 186-5 and SP 800-57 Part 1 Rev 4 -> Rev 5 as the development set; EDPB Guidelines 01/2022 (consultation -> final) and SP 800-171 Rev 2 -> Rev 3 plus the IRS Form 1040 `2024 -> 2025` stress pair in the holdout set.
  On the historical 2026-08-24 capture under default budgets (release build), three pairs (NIST FIPS 186, NIST SP 800-57, and EDPB) exhausted default candidate-generation budgets (`LIMIT`) and recorded no coverage metrics, SP 800-171 Rev 2 hit an unresolved extraction boundary, and only the IRS Form 1040 stress pair completed quality evaluation (`recall=1.000`, `kind=1.000`, with heavy hunk fragmentation).
  On the 2026-08-26 rerun under manifest `limit_scale_hint` budgets, no pair ends `LIMIT` or `FAIL`, though every comparison remains incomplete. Alignment coverage on comparable pairs spans `29%`-`58%` (`29.4%` on FIPS 186, `39.3%` on SP 800-57, `45.0%` on IRS 1040, `57.5%` on EDPB). Across annotated pairs, recall is `1.000` for SP 800-57, EDPB, and IRS Form 1040; FIPS 186-4->5 yields `recall=0.429` (3/7 matched under `limit_scale 16`, identical to pre-calibration baseline `134a498`), where missing list edits fall into conservative unresolved regions around renumbered, numeric-masked items rather than emitting false diffs. `kind=1.000` holds for all matched reviewed changes (precision remains unknown under partial annotations), while severe fragmentation (`19`-`1099` reported hunks per matched change) and unmatched tiny edits (`32`-`1770`) remain major quality challenges on large documents, and SP 800-171 Rev 2 remains unannotated at its unresolved extraction boundary.

## Contributing

Read [`AGENTS.md`](AGENTS.md) for repository working agreements. Keep changes aligned with the staged roadmap in [`SPEC.md`](SPEC.md), verified with the workspace checks above, and small enough to review as one coherent behavior change.
