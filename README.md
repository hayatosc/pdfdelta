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
- a lossless glyph evidence model with geometry, CropBox and supported path-clip relationships, straight vector-line evidence, and provenance;
- bounded Content Stream glyph extraction for supported text and straight-path operators, Type 1/Type1C/MMType1, TrueType, axis-aligned Type 3 simple fonts, Identity-H and bounded Identity-V Type 0/CID subsets, inherited resources, normalized page boxes, page-crop classification, explicit axis-aligned rectangular clipping, and isolated Form XObjects;
- standard ToUnicode resource wrappers, Standard/WinAnsi/MacRoman simple-font encodings with Differences and partial-map fallback, canonical Standard 14 identities, bounded full-domain identity Type 0 CMaps, explicit external CID font identity assertions, and stable embedded-font or bounded resource-dependent Type 3 CharProc identities for safely comparable unmapped glyphs;
- configurable Glyph-to-Line reconstruction with synthetic English spaces and preserved arbitrary-angle text lines;
- recursive XY-Cut region partitioning with spatial Region Graphs, conservative row-major label/value ordering for strongly aligned spacious parallel rows or explicitly ruled dense two-column grids, and relative Line-to-Block scoring across page breaks with preserved running-matter roles;
- reversible raw/canonical normalization with scalar-indexed source maps, retained unmapped tokens, and auditable normalization events;
- alignment-only compatibility folding and density-limited numeric masking that never changes exact canonical text;
- unique exact anchors plus swappable n-gram inverted-index, MinHash LSH, and exhaustive candidate generators;
- deterministic anchor-interval alignment for 1:1, insert/delete, constrained adjacent 1:2, 2:1, 1:3, or 3:1 matches, and exact paragraph moves, with ambiguous regions preserved as unresolved;
- an in-house Myers diff over exact canonical and unmapped tokens, with contiguous change spans, formatting-only reports, side-specific coverage, and allocation-aware limits;
- text summaries and versioned JSON reports with explicit extraction completeness, unresolved regions, coverage, and CI-oriented exit decisions;
- a public bounded pipeline from extracted glyphs through exact comparison;
- end-to-end CLI comparison, output redirection, quiet mode for CI, shell completion generation, and backend, object, glyph, or SVG overlay inspection;
- a reproducible benchmark matrix of 23 acceptance, layout, and realistic document-pattern cases over programmatic canonical documents, with each case rendered through literal-`Tj` and positioned-`TJ` PDF paths (46 records total), complemented by externally rendered Typst raw-PDF revision pairs for SPEC §2.2 Case 3 in [`fixtures/external/case3-typst/`](fixtures/external/case3-typst/), SPEC §2.1 Japanese replacement in [`fixtures/external/japanese-typst/`](fixtures/external/japanese-typst/), SPEC §2.2 Case 1 Japanese line wrap in [`fixtures/external/case1-japanese-typst/`](fixtures/external/case1-japanese-typst/), SPEC §2.2 Case 2 Japanese page break in [`fixtures/external/case2-japanese-typst/`](fixtures/external/case2-japanese-typst/), and SPEC §2.2 Case 4 / Case 5 Japanese paragraph insertion and deletion in [`fixtures/external/case4-case5-japanese-typst/`](fixtures/external/case4-case5-japanese-typst/);
- a growing non-vendored real-world revision-pair benchmark track with download provenance and SHA-256 checksums, development versus holdout treatment, human-reviewed expected changes for a representative subset, and revision-diff quality metrics (coverage, unresolved token share, recall, precision where fully annotated, change-kind accuracy, fragmentation, suspicious tiny edits) reported separately from extraction conformance.

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
mise run pdfdelta -- --help
mise run example-glyph-comparison
```

`pdfdelta-core/examples/glyph_comparison.rs` demonstrates the backend-independent
`Document<Glyph>` API with a small in-memory replacement example. It is useful
for developing and testing the alignment pipeline without needing PDF fixtures.

`mise run ci` runs the same formatting, linting, workspace tests, and benchmark
verification as the GitHub Actions quality gate.
Run `mise tasks ls --local` to discover the reusable development and benchmark
tasks.

## Generated benchmark fixtures

`pdfbench` can render a bounded, single-page canonical YAML document through
either project-owned PDF construction path. The command preserves the source
order of the title, section headings, and paragraphs, and refuses to replace an
existing output path.

`mise run bench` evaluates all 23 generated cases through both renderers and
reports aggregate event and changed-token precision, recall, and F1. The current
46-record matrix matches 26/26 expected semantic events and 858/858 changed
tokens, with zero false-positive changed tokens across the layout-only cases.

```yaml
document:
  title: Quarterly Service Report
  sections:
    - id: availability
      heading: Service availability
      paragraphs:
        - id: availability-p1
          text: Release 10 remains available during the transition.
        - id: availability-p2
          text: Current availability guidance remains unchanged.
    - id: support
      heading: Customer support
      paragraphs:
        - id: support-p1
          text: Support hours remain unchanged.
```

```bash
mise run bench-render -- document.yaml --renderer lopdf-tj --output document.pdf
mise run bench-render -- document.yaml --renderer classic-xref-tj --output document-classic.pdf
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj number-replace --paragraph-id availability-p1 --new-number 20
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj line-wrap --paragraph-id availability-p1 --after-word 4
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj page-break --before-paragraph-id support-p1
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-insert --section-id availability --index 2 --paragraph-id availability-p3 --text "Inserted availability detail."
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-move --paragraph-id availability-p2 --to-index 0
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-move --paragraph-id availability-p2 --to-section-id support --to-index 1
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj margin-change --new-margin 48

# Evaluate a pair produced by an external renderer without invoking it in CI.
mise run bench-evaluate-rendered-yaml -- \
  fixtures/external/case3-typst/document.yaml \
  --old-pdf fixtures/external/case3-typst/old.pdf \
  --new-pdf fixtures/external/case3-typst/new.pdf \
  --renderer typst-0.15.1 \
  number-replace --paragraph-id release --new-number 20
```

The YAML workflow accepts printable ASCII. `evaluate-yaml` applies one supported
paragraph-local text mutation, a paragraph-local line wrap, a non-empty-section
paragraph deletion, a section-local paragraph insertion, a page break before a
source paragraph, a same-section or cross-section paragraph move, a selected-section
column reflow, or a global line-height, margin, font-size, or page-size change. It
renders the old and new revisions in memory and reports whether the exact expected
change was recovered. It does not publish either generated PDF.

`evaluate-rendered-yaml` applies the same canonical mutation and exact-span
evaluator to bounded, already-rendered PDF inputs. It records the supplied
renderer identity but never executes that renderer. The vendored Typst example
keeps external toolchain availability out of the normal test path while still
checking its PDF dialect through the same expectation logic.

`column-change --section-id ID` accepts a selected section with at least four
paragraphs in a one- or multi-section document. It lays out that section's
paragraphs as two column-major columns while keeping the title, every heading,
and every other section full-width.

## External extraction conformance

`pdfbench extraction-conformance` compares the default parser-backed glyph
extractor with a versioned, position-aware snapshot produced outside pdfdelta.
The snapshot is bound to the exact input PDF by SHA-256 and must identify the
producer name, version, and underlying parser family. These declarations make
the evidence auditable; the command does not independently certify that the
producer uses an unrelated implementation.

```json
{
  "schema_version": 1,
  "producer": {
    "name": "position-extractor",
    "version": "1.2.3",
    "parser_family": "independent-parser"
  },
  "input_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "glyphs": [
    {
      "text": { "kind": "mapped", "value": "A" },
      "page": 0,
      "render_order": 0,
      "bbox": {
        "min": { "x": 72.0, "y": 700.0 },
        "max": { "x": 78.0, "y": 710.0 }
      },
      "baseline": { "x": 72.0, "y": 700.0 },
      "direction": { "x": 1.0, "y": 0.0 }
    }
  ]
}
```

Pages are zero-based. Text, page order, and render order are exact; every
bounding-box, baseline, and direction coordinate uses the selected absolute
geometry tolerance. Unmapped text uses
`{"kind":"unmapped","font_identity_sha256":"...","glyph_id":42}` so uncertain
decoding is never represented as empty mapped text. Input PDFs and oracle JSON
are read under explicit size limits, and incomplete extraction is rejected.

```bash
mise run bench-extraction-conformance -- input.pdf \
  --oracle oracle.json \
  --geometry-tolerance 0.25 \
  --mismatch-svg mismatch.svg
```

Exit code `0` means the snapshots match, `1` reports the first comparison
mismatch, and `2` rejects malformed input, a checksum mismatch, incomplete
extraction, or a diagnostic that would exceed the explicit `50 000` glyph /
`8 MiB` SVG / `8 KiB` per-field text limits that bound otherwise multi-gigabyte
serialization (including intermediate `String`/`xml_escape` growth). `--mismatch-svg` writes a combined expected/actual geometry overlay
only when a mismatch exists; the destination must be new and is never
overwritten. A curated [`pdf_oxide` 0.3.77 oracle](fixtures/extraction-conformance/pdf-oxide-0.3.77/)
checks all 158 mapped horizontal glyphs in the vendored Japanese Typst fixture
against an independent custom PDF parser. Broader producer and corpus coverage,
including unmapped and rotated-text oracle cases, remains future conformance
work.

## Candidate generator profiling

`pdfbench candidates` checks candidate recall and visit pressure on every
built-in rendered mutation. `candidate-profile` complements that correctness
matrix with a deterministic identity-matched synthetic corpus and reports
recall@K, untruncated candidate-count p50/p95/max, index-build and full-query
latency, and Linux resident-memory observations for the inverted index,
MinHash LSH, and exhaustive oracle.

```bash
mise run bench-candidates -- --top-k 5,10
mise run bench-candidate-profile -- \
  --blocks 1000 \
  --top-k 5,10 \
  --json-output candidate-profile.json
```

Each generator runs in a separate worker process so indexes do not overlap in
the reported process footprint. `rss_before_build_bytes` is sampled after the
shared feature corpus is constructed; `peak_rss_bytes` comes from Linux
`/proc/self/status` after index construction and all queries. Allocator
retention and single observations make memory and latency diagnostic rather
than statistically stable measurements. The block count is bounded to
`2..=10000`; the exhaustive oracle performs all-pairs work, so larger values
can be expensive. JSON destinations must be new. This opt-in profile does not
change the default inverted-index generator: a MinHash default still requires
holdout recall to be no worse and candidate count or runtime to improve
clearly on representative documents.

## Real-world revision benchmarks

Self-comparison of a document with itself cannot exercise alignment under real
edits. [`benchmark/realworld/`](benchmark/realworld/) therefore records a
separate non-vendored corpus of genuine public revision pairs (`old -> new`)
with per-side provenance (stable URL, capture date, byte size, SHA-256),
development versus holdout treatment, scope flags, and human-reviewed expected
changes for a representative subset in `expected/*.json`. Documents are never
committed to this repository.

```bash
# capture a new pair before adding its provenance fields to the manifest
mise run bench-capture -- <pair-id> <old-url> <new-url>

# download both sides of every pair and verify byte counts and checksums
mise run bench-fetch
mise run bench-revisions-checksums

# run comparisons and report metrics (add --summary-json-output summary.json for deterministic compact summaries or --json-output report.json for full reports)
mise run bench-revisions
```

`bench-capture` accepts credential-free HTTPS URLs without query strings or
fragments, follows at most five HTTPS redirects, and limits each PDF to 100
MiB. It validates both `%PDF-` responses before publishing either cache file,
never replaces differing cached bytes, and prints the six provenance fields
in manifest order.

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
`--pair <id>` select subsets; `--limit-scale <factor>` scales the comparison
pipeline budgets (n-gram token elements, alignment candidate visits, alignment
DP cells, diff tokens, and diff edit distance up to its 64 MiB trace-allocation cap; parser
and extraction limits are untouched) and never weakens documented defaults,
while omitting it applies each pair's recorded `limit_scale_hint`. Exit
code 1 signals provenance failures, expectation mismatches, or pairs that
ended `LIMIT`/`FAIL`; low quality scores never fail a run because the dated
captures serve as calibration evidence and current fragmentation and recall
remain too unstable for rigid quality-gate thresholds (confidence calibration
has landed, but broader large-document alignment quality remains ongoing work).
Compact revision summaries use schema version 36 and include optional nested
sentence-recovery diagnostics, near-search examined and attempted work,
Sentence/Line, search-scope, and known/ambiguous-span work attribution,
diagnostic-only Sentence shadow relations, paired-anchor cross-span locality,
decision parity, a behavior-neutral Sentence edge-gate shadow with typed
stops and projected retained-pair work, production exact edge-signature
candidate traversal with compact key-range storage and typed index and traversal
stops, explicit shadow-replay / production-accepted / production-discarded
execution provenance, plus bounded production Sentence edge-filter work,
retained/rejected pair counts, typed query-fallback reasons, full-build legacy
fallback evidence, separately accounted discarded near-relation work,
candidate-posting visits and maxima, an explicit candidate-count truncation
flag, typed
near-search stop reasons, structural trusted-run profile diagnostics,
budgeted exact-unit trusted-run signature diagnostics,
bounded one- and two-sided expected-change recovery watches with retained
occurrence evidence for single-unit or adjacent-unit
segment locations, exact segment-pair candidate and classification counters,
per-record exact segment-pair evidence, existing candidate scores and
relations, reciprocal status, typed stop evidence, and diagnostic-only
one-sided veto provenance with the authoritative best opponent, actual search
scope, and at most two observed opponents with descriptor geometry and
containment when available,
bounded Clause/ListItem unit boundaries, locations, best-partner relations,
tie and exactness evidence, aggregate work counts, completion, and typed stops,
optional exact expected occurrence counts with bounded deterministic event
assignment and fail-closed matching diagnostics,
scoped-complete event and changed-token
precision/recall/F1, changed-span intersection-over-union, false-positive
tokens per 10,000 reviewed unchanged tokens,
reviewed candidate recall at the production top-K limit, and bounded
evidence-backed reasons for missed expected changes.
The historical schema-v34 capture retains the independent legacy-candidate
reference oracle and its separately bounded edge-filter and downstream work.
For `scoped_complete` annotations, each `old_quote` and `new_quote` is the exact
expected changed span on that side; it must not include unchanged context.
Candidate recall is unavailable when either reviewed quote does not identify
exactly one extracted block. Diagnostic caps produce an explicit incomplete
fallback instead of a guessed cause. The unversioned full-report v1 key set
remains unchanged. Reviewed candidate recall and expected-change failure
diagnostics and scoped event/token metrics are available only through
`--summary-json-output`; CLI trace schema v24 exposes sentence-recovery metrics,
including both Sentence edge shadows and the production edge filter, one-hot
direct-execution provenance, near-search, shadow, filter, and run-signature stop
reasons, plus structural-run counters, but not those reviewed metrics.
`--json-output` and `--summary-json-output` are published independently and
atomically (each serializing in memory and publishing via same-directory temporary
files without overwriting existing destinations). If the second publication fails,
exit code 2 is returned and the first published artifact remains in place without
pairwise rollback across distinct paths. Dated compact evaluation summaries are
recorded under [`benchmark/realworld/results/`](benchmark/realworld/results/README.md)
(latest: [`2026-08-31-85c799d.json`](benchmark/realworld/results/2026-08-31-85c799d.json)).

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

Text reports are written to standard output as contextual unified-diff hunks: a one-line summary, `---` / `+++` file headers, and `@@ page N … @@` hunks with `-` / `+` markers, bounded surrounding context, one-based page numbers, explicit unresolved regions, and presentation-only grouping of nearby exact changes. `--color auto|always|never` controls ANSI color (`auto`, the default, colorizes only when stdout is a terminal; color supplements the markers and is never required to read the output). Standard input can be supplied as `-` for either PDF input. `-o, --output PATH` publishes the human-readable text report atomically to a new file instead of standard output. `-q, --quiet` suppresses standard-output reports for exit-code-only CI workflows. The typed JSON report is unchanged by presentation options: `-j, --json PATH` writes a version 8 JSON report to a new path and refuses to replace an existing file. Each semantic content change contains one or more provenance-preserving `occurrences`; ordinary changes contain exactly one. Exit code `0` means no content changes, `1` means content changes were found, `2` means the comparison could not run, and `3` means `--strict` rejected an incomplete comparison.

`--limit-scale FACTOR` raises the comparison pipeline budgets for n-gram token elements, alignment candidate visits, alignment DP cells, and diff tokens. It also raises the diff edit-distance budget up to the bounded Myers implementation's 64 MiB trace-allocation cap. The factor must be finite and at least `1`; parser and extraction limits remain unchanged.

`--trace-json PATH` writes a separate version 2 diagnostic trace without changing the normal report. The trace records input reading, PDF parsing, glyph extraction, layout reconstruction, normalization, alignment, exact diff, and report phases with bounded metrics, including sentence-recovery diagnostics when available. It also identifies incomplete or failed phases, records typed resource-limit errors, and marks phases that were skipped after an earlier stop. Trace files use the same atomic, no-overwrite publication policy as JSON reports.

## Current limitations

- `pdfdelta` does not run OCR and does not compare image contents or handwriting. Image-only pages can pass extraction because Image XObjects are explicitly skipped; that does not mean text visible inside the image was compared. Existing OCR text layers are retained as glyph evidence, but invisible text remains outside visible-content comparison. Empty-user-password decryption is automatic; other known passwords can be read from side-specific files and are never accepted directly as argument values or retained in reports and traces.
- Extraction currently targets mainly single-column text using supported Type 1/Type1C/MMType1 or TrueType simple fonts, an axis-aligned Type 3 subset with declared metrics and bounded CharProcs, plus Type 0 fonts with one CIDFontType0/CIDFontType2 descendant and bounded metrics. Identity-H and an Identity-V subset using only default DW2 metrics use fixed two-byte codes; bounded custom Type 0 CMaps are accepted only for full-domain one- or two-byte identity mappings. ToUnicode is optional only when a stable embedded-font, canonical Standard 14 identity, or explicit caller-provided external CID font identity can preserve unmapped glyphs. External identities are trust assertions, not font discovery. Type 3 CharProc drawing operators and MMType1 variation axes are not interpreted, and MMType1 therefore cannot provide stable identity for unmapped glyphs. Rotated, sheared, translated, or horizontally reversed Type 3 FontMatrix values, unsupported fonts inside Type 3 resource graphs, general custom CMaps, per-CID W2 vertical metrics, general vertical-writing reading order, complex tables, AcroForm, and complete annotation handling are not implemented. A simple left-to-right horizontal region is treated as known only when glyph paint order independently confirms top-to-bottom lines. An ordinary two-column region, including one surrounded by full-width bands, is treated as known only when its spatial order is unique and paint order keeps the complete left column contiguous before the complete right column. Row-major label/value or parallel pairs are treated as known only when paint order alternates left then right without overlap and either at least three strongly aligned rows have spacious separation or a straight vertical divider plus horizontal row separators prove a dense two-column grid. Three or more regions are accepted only when the spatial region graph has a unique order and paint order keeps each complete region contiguous in that same order. Ambiguous multi-region layouts, row-interleaved columns without strong parallel-row evidence, right-to-left text, non-horizontal text, and mixed or unknown text direction preserve every line and glyph. When the remaining region order is independently proven, only blocks containing locally unsupported lines are reported as unresolved reading order; otherwise the page-local comparison window remains unresolved. Extraction itself remains complete, and changes in independently anchored windows continue to be compared.
- Every extracted glyph records whether its geometry is inside, partially outside, or fully outside the normalized page CropBox and any supported explicit path clip. Fully outside glyphs remain available as raw inspection/SVG evidence but are excluded from visible-content comparison; partially intersecting glyphs remain comparable. A single axis-aligned rectangle expressed by `re` or by one closed or implicitly closed straight-line subpath is interpreted as a clip, including intersections of supported rectangles. Straight stroked segments are retained as bounded layout evidence; curved or compound clipping paths, transparency, fill/stroke color, and occlusion by later paint operations are not interpreted, so overpainted historical text can still appear in comparison input.
- Paragraph moves are reported as dedicated `Move` changes when an out-of-order anchor is unique and its canonical text matches exactly. Fuzzy or structurally changed move candidates still fall back to deletion plus insertion changes or unresolved regions.
- PDF text operators, encodings, ToUnicode maps, and Form XObjects are supported only within the bounded subset covered by the backend fixtures.
- Unsupported or unresolved extraction is reported as a typed document-, page-, page-tree-gap-, or glyph-gap-scoped issue in stderr and the text or JSON report.
- Page- and glyph-scoped extraction gaps are mapped from their side-local retained evidence to monotone exact-anchor windows. Recoverable Form XObject failures roll back that invocation's glyphs while retaining text before and after it. Affected windows are reported as unresolved, while proven windows continue through alignment and exact diff; if no usable anchors exist, all retained content remains unresolved. Default mode returns exit code `0` or `1` according to known content changes, while `--strict` returns exit code `3` for the incomplete comparison.
- Document-scoped extraction gaps still suppress the entire diff because the missing order interval cannot be located safely. Page trees with independently valid and invalid branches retain the valid pages and report side-local page-tree-gap boundaries; unexplained root count mismatches remain document-scoped. Default comparison follows known changes, while `--strict` returns `3` for either incomplete case. Other fatal I/O, backend, malformed-input, and resource-limit failures remain execution errors with exit code `2`.
- Finer extraction-gap boundaries and recovery of changes inside an unresolved extraction anchor window remain future work.
- Atomic `--json` publication requires a filesystem with same-filesystem hard-link support. Other filesystems return exit code `2` without publishing the report.
- Formatting-only reporting is best-effort and does not claim pixel-level rendering identity.
- Engine-generated replacements receive `CharacterWidth` only when an explicit fullwidth/halfwidth fold exactly explains the changed hunk. `OcrConfusion` remains available only to programmatic callers because OCR is not implemented.
- JSON change, formatting, and unresolved spans include glyph geometry plus content-stream object and operator provenance. Text reports do not list per-span provenance, and SVG output remains a whole-document glyph overlay rather than a per-change diff overlay.
- The automated PDF mutation benchmark is deterministic and in-memory: it uses printable ASCII with Type 1 Helvetica and two PDF construction paths (23 cases × 2 project renderers = 46 records), including repetition, long prose reflow, numbered requirements, pagination churn, footnote-like paragraphs, section-labeled paragraph movement, and column-major two-column reflow. The separate file-backed canonical YAML path renders one bounded document and can feed paragraph-local line wrapping, page breaking, text replacement, text insertion, text deletion, number replacement, section-local paragraph insertion, non-empty-section paragraph deletion, same-section or cross-section paragraph movement, four global rendering changes, and a selected-section paragraph-only column change through the library evaluator while retaining its title and section headings. The CLI exposes those same fourteen mutation commands. One canonical number-replacement expectation is also evaluated against a vendored Typst pair through `evaluate-rendered-yaml`, without invoking Typst in CI; this path does not generate Typst source from YAML or cover the full mutation matrix. A parser-independent Glyph-to-Line matrix separately covers single-column English, mixed font sizes, superscripts, reconstructed English spaces, and decoded horizontal Japanese/Latin text. A companion Line-to-Block matrix covers paragraph grouping, heading separation, relative spacing, conservative or cadence-supported page boundaries, and row-major label/value ordering for strongly evidenced parallel rows. Primitive extraction has a neutral snapshot comparator for decoded text, glyph count and order, page, geometry, baseline, and direction, backed by hand-written rotated and positioned-text oracles. Low-level PDF fixtures separately verify CropBox and rectangular path-clip classification, straight-line provenance, conservative ruled-grid ordering, and one exact cell replacement. A bounded CLI accepts versioned snapshots with producer and input identity, and a curated `pdf_oxide` 0.3.77 snapshot verifies all 158 mapped horizontal glyphs in one Japanese Typst fixture against an independent custom parser. Opt-in mismatch SVG output overlays expected and actual geometry without overwriting existing evidence. The cross-parser corpus remains intentionally narrow. These are complemented by vendored external Typst raw-PDF revision pairs for SPEC §2.2 Case 3 (`Release 10` -> `Release 20`) in [`fixtures/external/case3-typst/`](fixtures/external/case3-typst/), SPEC §2.1 Japanese horizontal born-digital text replacement in [`fixtures/external/japanese-typst/`](fixtures/external/japanese-typst/), SPEC §2.2 Case 1 Japanese line-wrap invariance in [`fixtures/external/case1-japanese-typst/`](fixtures/external/case1-japanese-typst/), SPEC §2.2 Case 2 Japanese page break invariance in [`fixtures/external/case2-japanese-typst/`](fixtures/external/case2-japanese-typst/), and SPEC §2.2 Case 4 / Case 5 Japanese paragraph insertion and deletion in [`fixtures/external/case4-case5-japanese-typst/`](fixtures/external/case4-case5-japanese-typst/). Non-vendored public smoke corpora are recorded in [`benchmark/manifests/real-world-pipeline.tsv`](benchmark/manifests/real-world-pipeline.tsv) and [`benchmark/manifests/real-world-pipeline-round2.tsv`](benchmark/manifests/real-world-pipeline-round2.tsv), but downloading and running them is not automated. With documented explicit inputs, the second manifest records 37 strict self-comparison successes and one default-mode partial success that remains strict-incomplete. Additional external renderer dialects (LaTeX, HTML/Chromium) and broader Japanese raw-PDF corpus fixtures (such as vertical writing or non-Identity-H encodings) remain future evidence validation work.
- The real-world revision track ([`benchmark/realworld/`](benchmark/realworld/)) records 29 genuine public pairs: 13 development and 16 holdout pairs spanning standards, regulatory publications, API and user guides, Latin/CJK prose, code, tables, forms, screenshots, generated reference manuals, multiscript examples, and image-heavy layouts. Eighteen pairs are standard targets near the intended scope and eleven are explicit stress cases; eight pairs have partial human-reviewed expected changes, two of those partial sets contain three complete scopes, and four additional pairs have scoped-complete review regions with event and changed-token precision.
  The dated [`2026-08-26`](benchmark/realworld/results/2026-08-26.json) artifact remains the immutable `limit_scale_hint` baseline for the original five-pair corpus. The [`2026-08-28`](benchmark/realworld/results/2026-08-28.json) capture records the expanded twelve-pair corpus at engine commit `a7b56a7`, while [`2026-08-29`](benchmark/realworld/results/2026-08-29.json) records all 29 current pairs at engine commit `ffd7600`. Same-day follow-ups record [`987020f`](benchmark/realworld/results/2026-08-29-987020f.json) uncertain-span exact and reciprocal replacement recovery, [`ca6447c`](benchmark/realworld/results/2026-08-29-ca6447c.json) monotonic exact-unit recovery inside anchored trusted runs, [`0cef530`](benchmark/realworld/results/2026-08-29-0cef530.json) short-unit recovery, weighted multiset candidate scoring, and page-local short candidates, [`c2de839`](benchmark/realworld/results/2026-08-29-c2de839.json) atomic line recovery for punctuation-free uncertain regions, [`80def43`](benchmark/realworld/results/2026-08-29-80def43.json) bounded semantic line grouping for short matched regions, [`3074d95`](benchmark/realworld/results/2026-08-29-3074d95.json) short structural-label anchors for adjacent value moves, [`de4ed4c`](benchmark/realworld/results/2026-08-29-de4ed4c.json) mixed-run word replacement grouping, [`1ed7340`](benchmark/realworld/results/2026-08-29-1ed7340.json) fail-closed fragment-completion vetoes for general uncertain-span replacements, [`556686c`](benchmark/realworld/results/2026-08-29-556686c.json) the behavior-preserving multi-occurrence change-model migration, [`27f096e`](benchmark/realworld/results/2026-08-29-27f096e.json) scoped-complete event and changed-token precision for three fully reviewed regions, [`f5406f3`](benchmark/realworld/results/2026-08-29-f5406f3.json) role-local running-matter alignment, [`4e6ae73`](benchmark/realworld/results/2026-08-29-4e6ae73.json) the safe near-search baseline, [`5145723`](benchmark/realworld/results/2026-08-29-5145723.json) behavior-neutral structural trusted-run diagnostics, [`1463932`](benchmark/realworld/results/2026-08-29-1463932.json) bounded exact-unit signature diagnostics for trusted runs, [`1a0675f`](benchmark/realworld/results/2026-08-29-1a0675f.json) bounded expected-change recovery watches, and [`24a2300`](benchmark/realworld/results/2026-08-29-24a2300.json) diagnostic-only adjacent-unit segment watches. The [`5ffa3e0`](benchmark/realworld/results/2026-08-30-5ffa3e0.json) capture records bounded exact segment-relation diagnostics without enabling segment moves, [`b1e54e3`](benchmark/realworld/results/2026-08-30-b1e54e3.json) adds bounded one-sided insertion/deletion occurrence evidence, [`2ddbb6b`](benchmark/realworld/results/2026-08-30-2ddbb6b.json) adds behavior-neutral Clause/ListItem watch diagnostics, and [`ebcb49e`](benchmark/realworld/results/2026-08-30-ebcb49e.json) validates exact occurrence counts and groups repeated running-matter changes into multi-occurrence events. Separate files keep parser, alignment, annotation, and algorithm effects auditable instead of rewriting historical metrics.

  The [`64a58b7`](benchmark/realworld/results/2026-08-30-64a58b7.json) capture adds topology-local, multiplicity-aware Line trigram candidate indexing with explicit posting-work diagnostics. The [`496d128`](benchmark/realworld/results/2026-08-30-496d128.json) capture attributes that bounded near-search work to Sentence and Line units. The [`ba424d8`](benchmark/realworld/results/2026-08-30-ba424d8.json) capture further attributes it to four search phases. The [`a7a483c`](benchmark/realworld/results/2026-08-30-a7a483c.json) capture separates known-same-span candidates, ambiguous-span candidates, and shared query preparation without changing comparison behavior. The [`a5202c8`](benchmark/realworld/results/2026-08-30-a5202c8.json) capture replays a known-span-only Sentence search in shadow mode and shows that removing ambiguous counterparts changes relation and reciprocal-pair decisions, so the filter remains diagnostic-only. The [`d045e2c`](benchmark/realworld/results/2026-08-30-d045e2c.json) capture preserves every output and quality field while stopping word-multiset scoring when its remaining upper bound cannot improve the exact relation score. The [`d6cd66d`](benchmark/realworld/results/2026-08-30-d6cd66d.json) capture classifies cross-span Sentence work by paired-anchor topology and shows that retaining only the same paired interval changes unique and reciprocal relation decisions. The [`f0ffe19`](benchmark/realworld/results/2026-08-30-f0ffe19.json) capture begins measuring exact relation-floor early-stop opportunities in completed searches. The [`12fb039`](benchmark/realworld/results/2026-08-30-12fb039.json) capture retains completed probe observations from budget-stopped cross-span searches and shows that the exact skip would save only 0.155% of cross-span similarity comparisons, so it is not enabled. The [`ab26b3c`](benchmark/realworld/results/2026-08-30-ab26b3c.json) capture records the atomic production Sentence edge filter and its full-build legacy fallbacks. The [`edb34a2`](benchmark/realworld/results/2026-08-30-edb34a2.json) capture independently replays exact edge-signature candidates without changing any prior report field; the replay prunes 93.998% of observed pairs, while six large searches still stop in the broader candidate-posting traversal, so the signature gate remains diagnostic-only.

  The [`7ffc831`](benchmark/realworld/results/2026-08-31-7ffc831.json) schema-v33 capture stores Sentence edge signatures as compact unique-key ranges plus occurrence arrays. All 18 available direct replays complete, common completed-path logical bytes fall by 66.24% from schema v32, and comparison and reviewed-quality fields remain unchanged. Production activation remains deferred because the comparable candidate union is 12.04%, the exact retained lower bound is already 10.89%, and the LibreOffice reference oracle still stops at its pair-visit limit; the old sub-10% gate must be replaced by an exact-lower-bound criterion before behavior changes.

  The [`e254b48`](benchmark/realworld/results/2026-08-31-e254b48.json) schema-v34 capture independently applies exact Sentence edge classification before reference near-relation work. Seven of eight reference oracles preserve plan and retained-pair fingerprint parity. LibreOffice reduces downstream near-pair work to 7,993 but stops fail-closed after 32,000,000 edge comparisons with 1,831 broad candidates unclassified. Every comparison, quality, candidate-recall, direct-replay, and other diagnostic field remains identical to schema v33 when the reference-oracle object is excluded; production activation remains deferred without raising budgets.

  The [`447927d`](benchmark/realworld/results/2026-08-31-447927d.json) schema-v34 capture packs exact FIRST/LAST provenance into the existing one-word legacy candidate postings and reuses it during independent reference classification. All eight reference oracles complete with identical plans and retained-pair count, set, and order fingerprints. LibreOffice classifies all 18,853,226 broad pairs with 24,943,792 edge comparisons under the unchanged 32,000,000 cap. Removing the reference-oracle object produces exact `e254b48` parity for every other field; production activation remains deferred pending the candidate-overhead and full-build fallback gates.

  The [`08cc0c6`](benchmark/realworld/results/2026-08-31-08cc0c6.json) schema-v35 capture activates exact edge-signature traversal in production. All 18 available Direct builds are accepted and complete, all six former legacy fallbacks are eliminated, and the 12 previously complete paths preserve their comparison and quality fields. On the six newly completed paths, Direct generates 1,633,897 candidates instead of the completed reference oracles' 31,842,858 broad candidates, a 94.87% reduction, with zero broad Sentence posting visits. Its apparent SP 800-57 recall regression led to a review of the expected deletion.

  The [`85c799d`](benchmark/realworld/results/2026-08-31-85c799d.json) schema-v36 capture adds bounded one-sided veto provenance without changing any comparison field or complete-scope metric. The review confirmed that the SP 800-57 toolkit footnote exists in both revisions and removes only the comma before `rather`; the annotation is now a replacement. Its counterpart remains unresolved because the watched whole-Sentence units are in different spans, score only 2,347 against each other, and each side has tied 10,000-point competing relations. The next diagnostic therefore targets quote-local fragment or clause boundaries without changing thresholds or margins.

## Contributing

Read [`AGENTS.md`](AGENTS.md) for repository working agreements. Keep changes aligned with the staged roadmap in [`SPEC.md`](SPEC.md), verified with the workspace checks above, and small enough to review as one coherent behavior change.
