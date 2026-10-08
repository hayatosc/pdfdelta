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

### Native-text completion coverage

On 2026-09-27, a fresh release build of HEAD `cf5feb9` completed
**3/36 pairs (8.3%)** under a shared 6 GB memory limit. All 36 pairs produced
reports; 33 remained incomplete. No pair timed out or failed execution.
This is document-comparison completion, not test coverage or change recall.
It uses the fixed 36-pair panel, distinct from the 29-pair revision manifest
above.

| Outcome | Pairs |
|---|---:|
| Complete, differences detected (exit 1) | 3 |
| Incomplete, differences detected (exit 3) | 20 |
| Incomplete, difference status indeterminate (exit 3) | 13 |
| Both inputs extracted completely, including incomplete comparisons | 32 |

The complete pairs are `irs-schedule-se-2024-to-2025`,
`faa-thunderstorms-b-to-c`, and `bunka-kana-1946-to-1986`. The latter two have
zero visible native-text tokens on the old side. Thus only one complete pair
compares nonempty native text on both sides. Completion does not establish
image comparison or annotation-scored precision/recall.

The retained diagnostic reports now total **72,550,851 bytes (72.6 MB)**,
down from 1,874,123,872 compressed bytes (**96.1% smaller**). All semantic
records and text are retained; repeated per-glyph drawing sources are summarized
as described below. The whole retained run, including the compressed binary,
source archive, manifests, scripts, and logs, is about **87.6 MB**. This storage
change does not alter the 3/36 result.

### Measurement conditions and verification

- Build: `cargo build --release --locked -p pdfdelta-cli`, with
  `CARGO_BUILD_JOBS=1`, at `cf5feb94a1f821fa7712a75791164ff9ada2cc54`.
  The captured production diff is empty; only documentation was modified.
  Binary SHA-256:
  `80b86bebfeb4d9d5906bd94edcd8b46d991596f7f72854838709fe37b7f4a336`.
- Route: `--native-text-only --limit-scale 1 --quiet --json REPORT.json.gz`,
  schema 11, one run per pair, sequential execution, 180-second timeout per
  comparison. Total comparison wall time was 778.93 seconds; build, hashing,
  and summary processing take additional time. This run does not establish
  repeat-run determinism.
- Resource guard: the build and capture ran sequentially in systemd scopes
  with `MemoryMax=6000000000` and `MemorySwapMax=0`. The capture verified the
  effective cgroup limit of 5,999,996,928 bytes (kernel page rounding). Its
  process-group memory peak reached that cap, with 5,144 `memory.events:max`
  events, zero OOM events, zero OOM kills, and zero swap usage. The largest
  per-comparison peak RSS was 5,634,948 KiB (about 5.77 GB). These metrics have
  different scopes: the cgroup includes capture helpers and charged file cache.
- Storage: full reports contain 48,202,083,963 logical bytes, stored directly
  as 1,874,123,872 gzip bytes (about 48.2 GB versus 1.87 GB). Summaries were
  extracted incrementally. A 4 GB capture-output budget was checked between
  pairs; no unrelated historical captures were removed.
- Provenance: the historical driver, frozen panel, input manifests, and
  annotations were restored from `9229676` into the ignored capture setup
  directory. The capture helper was adapted only for those paths and the
  output-budget check; automatic retention was disabled. Input hashes were
  checked by the driver. All 36 run-record hashes and compressed report hashes
  were verified, and logical report hashes agreed between the driver and
  capture summary. The binary, source archive, helper, panel, and driver hashes
  were also checked.

Local evidence is retained under
`benchmark/realworld/results/current-native-2026-09-27/` (gitignored):

- `capture/summary.json`: 36 distinct rows; SHA-256
  `435beed6df9e0c7958b668badce0f58a4bac0bc8bb0bce11f71f06ce4cc111e0`.
- `compact/`: compact diagnostic reports and the verified replacement manifest.
- `capture/`: immutable capture summary, per-pair `runs.json`, logs, the exact
  binary in `pdfdelta.gz`, and a compressed source archive. The original full
  reports were replaced after verification; their recorded hashes are retained.
- `artifact-compression.json`: original and compressed binary hashes. Restore
  with `gzip -dc capture/pdfdelta.gz > /tmp/pdfdelta-replay` and
  `chmod +x /tmp/pdfdelta-replay` from the run directory.
- `memory.json`: effective limits, process-group peak, and OOM counters.
- `setup/`: restored provenance files and the capture scripts used for this run.
- `compact-tools/`: exact initial and resumed compactor scripts. One pair
  exceeded the initial 64 MiB retained-JSON bound; conversion resumed with a
  256 MiB bound and the same 6 GB process-group limit, preserving all fields.
- `compaction-verification.json`: checks of all 36 compact artifacts, 72 input
  PDF hashes, binary/source provenance, and unchanged completion counts.
  The compact manifest SHA-256 is
  `1c2f705c8563625da2d540c7cb64ae0c00f0697957b865d0d863333652cdcc16`.
- `compact/pruned.json`: receipt for removal of the 36 verified original reports.

The earlier experimental capture also reported 3/36, but included an
uncommitted reference-bounds patch. The current run replaces that build caveat
with evidence from the current source. The completion categories are unchanged;
32/36 full report logical hashes match the experimental capture exactly.

The historical panel definition and input provenance are available in the
[`9229676` archive](https://github.com/hayatosc/pdfdelta/tree/9229676f47594a6cc2af8d5b54fdff64e441db15/benchmark/realworld/followup).
Keep failed and incomplete pairs in the denominator when updating this result.

### Compact native diagnostics

Native schema 11 repeats per-glyph drawing provenance in resolution ranges,
relation spans, changes, and unresolved regions. For example, the largest pair
in the current 36-pair capture has 12.41 GB of pretty JSON, including 9.05 GB
in assessment relations. Gzip compresses repetition but leaves large artifacts.

For new native captures, write the small artifact at the source:

```bash
pdfdelta old.pdf new.pdf --native-text-only \
  --limit-scale 1 --quiet --json report.compact.json.gz
```

This directly emits compact artifact version 2. It preserves all semantic
records and text while summarizing sources before serialization. No full JSON
file or whole-report glyph-source array is built. Source-array hashes use
compact JSON in native schema-11 field order; there is no full-report hash for
a report that was never emitted. Store PDF/binary hashes and execution metrics
in the capture's run record. See [usage](usage.md#compact-native-json).

A 2026-09-28 release build with the direct writer was checked against six
saved pairs: IRS W-4, Schedule SE, GPT-3, MHLW care skills, FAA thunderstorms,
and NIST controls. Their gzip reports totaled **16,689,103 bytes**, compared
with **611,352,134 bytes** for the previous full reports (**97.3% smaller**).
The largest, NIST, fell from 495,567,773 to **9,460,205 bytes** and completed
in 44.5 seconds. All six retained semantic projections, source counts, pages,
and samples matched the saved results; hashes differ by the versioned encoding
contract. This selected-pair check does not update the full-panel 3/36 coverage.

The shared 6,000,000,000-byte cgroup cap remained active, with swap disabled
and no OOM events. NIST peak process RSS was 5,366,848 KiB; the workload
reached the cgroup cap and triggered memory reclamation, so the external limit
remains necessary. Replay arguments, PDF/binary hashes, the source patch,
compressed executable, per-pair metrics, and compact outputs are retained in
`benchmark/realworld/results/direct-compact-2026-09-28/` (gitignored).

For existing full native reports, use the version-1 postprocessor:

```bash
python3 benchmark/realworld/compact_capture.py convert \
  benchmark/realworld/results/RUN/capture/summary.json \
  benchmark/realworld/results/RUN/compact

# Recheck the manifest and replacements before removing the original reports.
python3 benchmark/realworld/compact_capture.py prune \
  benchmark/realworld/results/RUN/compact/manifest.json
```

Run conversion in the same 6 GB workload limit as the benchmark. For a single
report, use `python3 benchmark/realworld/compact_native.py INPUT.json.gz OUTPUT.json.gz`.
The compactor accepts the native serializer's pretty JSON layout and stops
if retained JSON would exceed 256 MiB; it never truncates semantic fields.
Use `convert --resume` to verify and reuse completed outputs after an interrupted
conversion. The tools refuse to overwrite existing outputs. Conversion leaves originals
in place; pruning requires a complete verified replacement set and preserves
the capture summary, run records, logs, input provenance, binary, and source
archive. `manifest.json` maps original reports to their compact replacements.
Original report paths in the immutable capture records describe the historical
outputs and may no longer exist after pruning.

Compact artifacts use a separate format with the retained report under `report`.
They preserve **every** change, candidate, formatting change, unresolved region,
resolution range, relation, review unit, text, unmapped token, extraction issue,
coverage value, confidence, reason, assumption, and work-budget field. Text and
semantic arrays are neither sampled nor truncated.

For postprocessed version-1 artifacts, each `sources` array is replaced by a
`source_summary`: its raw serialized
SHA-256 and size, source-kind counts, pages, and first/last source locator samples.
Individual glyph geometry and PDF operator provenance require regenerating the
full report when the samples do not answer a question. The original input and
binary hashes, arguments, full-report hashes, and compactor metadata preserve
that audit path. This is a diagnostic projection, not a lossless archive of the
native report, and it must not be passed to consumers expecting schema 11.

### Capture archive

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

## Scoped source quotation gold

`benchmark/realworld/local-gold/development.json` records independently extracted
source quotations and visually checked Schedule C form-field correspondences.
These are development targets, not a blind document-wide accuracy score. Audit
input hashes and page quotations with:

```bash
python3 benchmark/realworld/local-gold/verify_source_gold.py \
  benchmark/realworld/local-gold/development.json
```

Whitespace differences can be collapsed for this quotation audit; punctuation
remains significant. A quotation-level changed region does not select one of
several equally minimal character alignments and does not supply a glyph mask.
Keep independently selected holdout targets separate from development targets.

Assessment JSON includes `anchor_work`: candidate and occurrence indexing,
sequence comparison and order uniqueness charges, starts examined, and the first
refused request. Charges are logical work units rather than CPU instructions;
`refused_remainder` identifies budget consumed without performing refused work.
The global anchor proof enumerates every posting of the least frequent internal
adjacent token pair (or the token itself for a single-token needle), translates its offset to a possible start, and verifies full token
equality. This preserves overlapping matches and cross-block alternatives while
reducing impossible start checks. Work for choosing the posting is also charged.
Constructor sidecar reservations are reported separately. If
`sidecar_refused_request` is nonzero, `sidecar_index` includes the remaining
budget consumed by that failed reservation; those charged units were not
executed comparisons. Anchor `refused_remainder` describes the anchor proof
only, not a sidecar refusal.

The distinct whole-block anchor order uses a bounded Fenwick-tree LIS path count
instead of a quadratic token LCS table. It accepts only distinct positions and
returns no uniqueness claim when its shared budget is insufficient. Both the
rank array and tree share the exact check's 64 MiB allocation bound. Exhaustive
small permutations compare the specialized result with the general exact oracle.
When the maximum spine is ambiguous, a second bounded forward/reverse LIS check
retains only anchors shared by every maximum path. A vertex is eligible when
its prefix and suffix lengths sum to the maximum length plus one; a level with
one eligible vertex is mandatory. Exhaustive maximum-path intersections for
all permutations through length eight verify this rule. The check and its
projection use fallible allocations and explicit 64 MiB bounds; any unfinished
budget returns no anchors. `order_unique` remains false even when
`verified_anchors` is nonzero. These common boundaries narrow correspondence
windows; they do not clear source-order, normalization or extraction barriers.
Every other verified source pair is retained as competing correspondence,
including pairs outside maximum paths. A global window touching a competitor
stays unclosed; a character LCS cannot choose that correspondence by favoring
longer blocks. Ordered-window discovery keeps the same veto. Independently
established local source domains still require their existing proof.
`local_view_work` separates trusted view construction, automatic seed search,
explicit-anchor search, source issue indexing, domain construction and footers.
Stage totals minus the listed subphase charges remain unattributed work and
must be reported separately. Completing a budget phase does not establish a
unique anchor order or improve resolved coverage by itself.

Optional local discovery retains up to one sixteenth of the configured shared
work limit for later localization and emission (2 million units at the default
32 million limit). The reserve is a bounded scheduling heuristic, not an
accuracy guarantee. Only actual optional spend is deducted from shared work;
unused work and the reserve remain available to settlement. Small limits below
16 units retain a zero reserve.

`local_view_work.budget_cap` and `settlement_reserve` describe that split.
`cap_exhausted` means the optional cap reached zero, including an exact boundary
or zero-cap invocation; it does not establish the size or even occurrence of a
refused request. Current view helpers do not expose that receipt, so
`refused_request_size` is unknown. A reached cap records the optional search as incomplete
even when the restored shared budget is positive. Existing source closure and
completion rules remain in force. An already established root retains its
completed evidence; the optional stop gets a separate tentative record and
does not establish ownership or invalidate established children.
