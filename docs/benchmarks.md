# Benchmarks and evaluation

`pdfdelta` is evaluated at several levels, from programmatic acceptance cases
to genuine public revision pairs. The tooling lives in the `pdfdelta-bench`
crate (binary `pdfbench`); run tasks through [mise](https://mise.jdx.dev/) or
directly with `cargo run -p pdfdelta-bench -- <command>`.

- [Acceptance criteria](#acceptance-criteria)
- [Generated benchmark matrix](#generated-benchmark-matrix)
- [Canonical YAML fixtures](#canonical-yaml-fixtures)
- [Vendored external fixtures](#vendored-external-fixtures)
- [Graph generalization evaluation](#graph-generalization-evaluation)
- [Producer document matrix](#producer-document-matrix)
- [Extraction conformance](#extraction-conformance)
- [Candidate generator profiling](#candidate-generator-profiling)
- [Real-world revision benchmark](#real-world-revision-benchmark)
- [Microbenchmarks](#microbenchmarks)

## Acceptance criteria

The first practical release requires all five core acceptance cases:

1. **Line-wrap invariance** — reflowing lines produces no content change.
2. **Page-break invariance** — moving a page break produces no content change.
3. **Text replacement** — produces exactly one change.
4. **Paragraph insertion** — produces exactly one change.
5. **Paragraph deletion** — produces exactly one change.

All five pass on the generated fixtures and are also exercised by vendored
external Typst pairs (see below).

## Generated benchmark matrix

```bash
mise run bench     # cargo run -p pdfdelta-bench -- verify
```

`verify` renders 24 acceptance, layout, and realistic document-pattern cases
through two project-owned PDF construction paths (literal `Tj` and positioned
`TJ`), giving 48 records. Cases include repetition, long prose reflow, numbered
requirements, pagination churn, footnote-like paragraphs, section-labeled moves,
and two-column reflow. It reports event and changed-token precision, recall,
and F1, and is part of `mise run ci`.

Current result: 42/48 strict passes; the remaining 6 retain candidates because
edit boundaries are ambiguous and count only as candidate-policy matches. All
26/26 expected events and 858/858 changed tokens are matched (20/26 events and
810/858 tokens strictly), with zero false-positive changed tokens on layout-only
cases.

Parser-independent matrices separately cover glyph-to-line reconstruction
(mixed font sizes, superscripts, synthesized spaces, horizontal Japanese/Latin
text) and line-to-block grouping (paragraphs, headings, page boundaries,
label/value rows).

`pdfdelta-core/examples/glyph_comparison.rs` shows the backend-independent
`Document<Glyph>` API with a small in-memory replacement:

```bash
mise run example-glyph-comparison
```

## Canonical YAML fixtures

`pdfbench` renders a bounded, single-page canonical YAML document through either
project renderer, applies one mutation, and checks whether the exact expected
change is recovered. The YAML workflow accepts printable ASCII and never
overwrites an existing output.

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
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-move --paragraph-id availability-p2 --to-section-id support --to-index 1
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj margin-change --new-margin 48
```

Supported mutations: paragraph-local text replacement/insertion/deletion,
number replacement, line wrap, page break, section-local paragraph insertion,
paragraph deletion, same- or cross-section paragraph move, a selected-section
two-column reflow (`column-change`, sections with at least four paragraphs), and
global line-height, margin, font-size, or page-size changes.

`evaluate-rendered-yaml` applies the same mutation and evaluator to PDFs that an
external renderer already produced, recording the renderer identity without
executing it:

```bash
mise run bench-evaluate-rendered-yaml -- \
  fixtures/external/case3-typst/document.yaml \
  --old-pdf fixtures/external/case3-typst/old.pdf \
  --new-pdf fixtures/external/case3-typst/new.pdf \
  --renderer typst-0.15.1 \
  number-replace --paragraph-id release --new-number 20
```

## Vendored external fixtures

Externally produced PDF pairs are vendored under
[`fixtures/external/`](../fixtures/external/) with their sources, so tests do
not need the producer installed:

| Fixture | Exercises |
| --- | --- |
| [`case1-japanese-typst`](../fixtures/external/case1-japanese-typst/) | Japanese line-wrap invariance |
| [`case2-japanese-typst`](../fixtures/external/case2-japanese-typst/) | Japanese page-break invariance |
| [`case3-typst`](../fixtures/external/case3-typst/) | Text replacement (`Release 10` → `Release 20`) |
| [`japanese-typst`](../fixtures/external/japanese-typst/) | Japanese horizontal text replacement |
| [`case4-case5-japanese-typst`](../fixtures/external/case4-case5-japanese-typst/) | Japanese paragraph insertion and deletion |
| [`vertical-tectonic`](../fixtures/external/vertical-tectonic/) | Tectonic-produced vertical Japanese |

The vertical-text controls cover a single vertical column with a horizontal
heading; a position-only change preserves all 14 native text sources, and a
quantity change reports one inferred text operation. They are not evidence for
ruby or multiple vertical columns.

## Graph generalization evaluation

`pdfdelta_bench::generalization` defines an annotation contract (version 1) for
local correspondences, typed change units, text display ranges, exact text
positions, exact raster masks, and changed relationships. Each dimension accepts
several complete expected outcomes and scores each alternative with one-to-one
multiset matching; duplicates count as false positives and misses as false
negatives. Inferred reports are scored separately and never added to
conditional scores.

```bash
cargo test -p pdfdelta-bench --test generalization_fixture
```

The fixture matrix crosses unchanged values, swapped numbers, negation/unit
changes, and Japanese values with unchanged or reversed graph storage order. This
is graph-level validation, not evidence of producer, layout, scan, or rendering
generalization.

Notes on the dimensions:

- `display_range` scores half-open Unicode scalar ranges in each displayed text
  value. The current report displays the whole changed paragraph or field, so
  its range is `0..value.chars().count()`. These offsets are not PDF byte
  offsets or glyph indices.
- `change_unit` annotations may set `old`/`new` to `null` to leave node
  correspondence unannotated; an empty array still asserts absence.
- Raster masks are scored as exact region facts, not pixel IoU.
- Source coverage uses the same calculation as the CLI; missing discovery stays
  unknown even when every discovered source was compared.

## Producer document matrix

`pdfbench generate-document-matrix` uses independently installed Typst and
Tectonic binaries to create English/Japanese PDFs and comparison pairs. It
crosses unchanged text, number replacement, negation, and unit replacement with
normal/narrow pages, an explicit page break, and sans/serif fonts, including
cross-producer pairs. Expectations come from the generated source.

```bash
cargo run -p pdfdelta-bench -- generate-document-matrix \
  --typst /path/to/typst --tectonic /path/to/tectonic \
  --tectonic-cache /path/to/prepared-tectonic-cache \
  --font-path /path/to/noto-cjk-fonts \
  --first-evaluated 2026-09-09 --output /tmp/pdfdelta-prose-matrix
```

- The default prose kind produces 64 PDFs and 128 pairs; `--kind table`
  produces 192 PDFs and 384 pairs for a two-column table with and without
  borders, including header replacement and value swaps.
- Provide Noto Sans CJK JP and Noto Serif CJK JP. Prepare the Tectonic cache
  with `geometry`, `fontspec`, and `xeCJK` (tables also need the `tabular`
  metrics); generation runs `--only-cached --untrusted` and fetches nothing.
- Each document has a 30-second compile deadline and a 64 MiB output cap. The
  destination must not exist.

The generator does not run comparisons. Run `pdfdelta -j` for each manifest
pair, then score the report:

```bash
cargo run -p pdfdelta-bench -- evaluate-document \
  --annotation annotation.json --report document.json
```

The annotation binds `old_sha256`/`new_sha256`, records provenance, names
independent `content_mutation` and `representation_mutation` axes, and supplies
`expectations` in the generalization contract. Reports are bounded to 64 MiB and
annotations to 4 MiB. Exit code `0` requires complete coverage and an exact
accepted alternative in every annotated dimension; `1` means incomplete or
mismatching; `2` rejects invalid input. These are development fixtures, not
unseen holdouts.

## Extraction conformance

`pdfbench extraction-conformance` compares the built-in glyph extractor with a
position-aware snapshot produced by an independent tool. The snapshot is bound to
the input PDF by SHA-256 and declares its producer and parser family.

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

Pages are zero-based. Text, page, and render order must match exactly; geometry
uses the selected absolute tolerance. Unmapped glyphs use
`{"kind":"unmapped","font_identity_sha256":"...","glyph_id":42}`.

```bash
mise run bench-extraction-conformance -- input.pdf \
  --oracle oracle.json \
  --geometry-tolerance 0.25 \
  --mismatch-svg mismatch.svg
```

Exit `0` means the snapshots match, `1` reports the first mismatch, and `2`
rejects malformed input, a checksum mismatch, incomplete extraction, or a
diagnostic exceeding 50,000 glyphs / 8 MiB SVG / 8 KiB per field.
`--mismatch-svg` writes an expected/actual overlay only on mismatch and never
overwrites. A curated [`pdf_oxide` 0.3.77 oracle](../fixtures/extraction-conformance/pdf-oxide-0.3.77/)
checks all 158 mapped horizontal glyphs of the vendored Japanese Typst fixture.

## Candidate generator profiling

```bash
mise run bench-candidates -- --top-k 5,10
mise run bench-candidate-profile -- --blocks 1000 --top-k 5,10 --json-output candidate-profile.json
```

`candidates` checks candidate recall and visit pressure on every built-in
mutation. `candidate-profile` builds a deterministic synthetic corpus and
reports recall@K, candidate-count p50/p95/max, index-build and query latency,
and Linux resident memory for the inverted index, MinHash LSH, and exhaustive
oracle, each in a separate worker process. `--blocks` is bounded to
`2..=10000`. Latency and memory are diagnostic single observations.

## Real-world revision benchmark

Self-comparison cannot exercise alignment under real edits, so
[`benchmark/realworld/`](../benchmark/realworld/) records genuine public
revision pairs (`old -> new`). Documents are downloaded, never committed; the
manifest records each side's URL, capture date, byte size, and SHA-256.

The corpus has 29 pairs (19 development, 10 holdout) spanning standards,
regulatory publications, API and user guides, Latin/CJK prose, code, tables,
forms, screenshots, and image-heavy layouts. Eighteen are standard targets and
eleven are stress cases. Human-reviewed expected changes for a subset live in
[`benchmark/realworld/expected/`](../benchmark/realworld/expected/). Separate
evaluation-only holdouts are in `claims-holdout.tsv` and `round2-holdout/`.

```bash
# capture a new pair and print its provenance fields for the manifest
mise run bench-capture -- <pair-id> <old-url> <new-url>

# download every pair and verify byte counts and checksums
mise run bench-fetch
mise run bench-revisions-checksums

# compare every pair and report metrics
mise run bench-revisions
mise run bench-revisions -- --set holdout --summary-json-output summary.json
mise run bench-revisions -- --pair nist-csf-v1-1-to-v2-0 --json-output report.json

# write a dated capture to benchmark/realworld/results/ (gitignored)
mise run bench-revisions-capture
```

`bench-capture` accepts credential-free HTTPS URLs without query strings,
follows at most five redirects, limits each PDF to 100 MiB, and validates both
responses before publishing either cache file.

Each pair ends as `OK` (compared), `LIMIT` (stopped at a documented resource
budget), or `FAIL` (provenance, expectation, or execution failure). Metrics
include extraction completeness, alignment coverage, unresolved-token share,
change counts, and — where annotations exist — recall, precision (complete
annotations only), change-kind accuracy, fragmentation, and suspicious one- or
two-token edits. `--limit-scale` scales comparison budgets (parser and
extraction limits are untouched); omitting it applies each pair's
`limit_scale_hint`.

Exit code `1` signals provenance failures, expectation mismatches, or pairs that
ended `LIMIT`/`FAIL`. Quality scores never fail a run: current recall and
fragmentation on these documents are too unstable for fixed thresholds.

`--summary-json-output` writes a deterministic compact summary with scoped
precision/recall and diagnostic fields; `--json-output` writes full reports.
Both are published atomically and never overwrite. Two summaries can be
compared with:

```bash
mise run bench-revisions-exact-parity -- baseline.json candidate.json
mise run bench-revisions-schema-parity -- baseline.json candidate.json --ignore-field <field-path>
```

`benchmark/realworld/sensitivity.sh OUTPUT_JSON` runs the built-in corpus under
layout, matching, score-margin, and candidate-budget perturbations. It is
diagnostic evidence and never selects production defaults.

Public smoke corpora for self-comparison are listed in
[`benchmark/manifests/`](../benchmark/manifests/); downloading and running them
is not automated.

### Historical captures

Dated benchmark captures and investigation notes are not kept in the working
tree. The complete archive, including every capture up to schema version 68, is
available at commit
[`9229676`](https://github.com/hayatosc/pdfdelta/tree/9229676f47594a6cc2af8d5b54fdff64e441db15/benchmark/realworld).
Store new captures and raw logs outside the repository or in the gitignored
`benchmark/realworld/results/` directory.

## Microbenchmarks

[`benchmark/microbench/`](../benchmark/microbench/) holds versioned case
matrices for two ignored measurement tests in `pdfdelta-core` (assignment
pricing and text-candidate retrieval) and the Python drivers that run them in
isolated release processes:

```bash
python3 benchmark/microbench/measure-text.py /tmp/text-measurement
python3 benchmark/microbench/measure-pricing.py /tmp/pricing-measurement
```
