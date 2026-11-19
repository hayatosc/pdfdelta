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

### Flow diff evaluation

`benchmark/realworld/flow_eval.py` measures the paragraph-flow diff engine
(`pdfdelta flow`) against the independent PyMuPDF oracle and the reviewed
changes in `benchmark/realworld/expected/`. It reruns the release binary over
the cached pairs and writes `summary.tsv` plus one compact JSON report per
pair. The report itself must stay small; `--tokens` writes token text to
gzipped JSON Lines sidecars (`REPORT.old-tokens.jsonl.gz` and
`REPORT.new-tokens.jsonl.gz`), which the evaluator streams line by line. It
rejects a compressed report above 32 MiB instead of loading it.

```bash
systemd-run --user --scope -p MemoryMax=6000000000 -p MemorySwapMax=0 -- \
  uv run --no-project --with pymupdf python -I benchmark/realworld/flow_eval.py \
  --cache benchmark/realworld/cache --results OUT \
  --bag oracle/bag.tsv --expected benchmark/realworld/expected \
  --binary target/release/pdfdelta --only arxiv-attention-v6-to-v7
```

Recorded columns include exit code, wall time, peak RSS, flow and oracle token
counts, changed tokens per kind, the oracle bag lower bounds, `noise =
(delete + insert + replace_old + replace_new) / (bag_deleted + bag_inserted)`,
`min_deleted`/`min_inserted` from GNU `diff --minimal` on the exact token
sidecars, `flow_non_move_changed` (delete + insert + replace tokens),
`align_ratio = flow_non_move_changed / (min_deleted + min_inserted)`,
`minimal_status`, `cancellable` tokens (multiset intersection of old removal
sides and new insertion sides), `in_place_moves` (moves whose old and new
ranges fall in the same spine gap, reported as `old_gap`/`new_gap` by the
engine), `oracle_old_tokens`/`oracle_new_tokens`,
`oracle_furniture_removed_{old,new}`, and
`oracle_order_agreement_{old,new}` (order-preserving overlap with a PyMuPDF
reading-order extraction, computed by GNU `diff --minimal` on token-per-line
files), `stream_pages_{old,new}`, `baseline_pages_{old,new}`,
`order_uncertain_{old,new}`, and `low_coherence_{old,new}` from the flow report's
per-page order evidence (which also records the layout, stream, and baseline
scores), and
the fraction of each reviewed quote covered by non-layout-only changes. The
oracle metric normalises NFKC, quote/hyphen/soft-hyphen and combining-mark
conventions, drops control characters, and removes flow furniture from the
oracle by digit-masked token sequence, so it measures reading order rather
than tokenisation or furniture retention. Unmapped flow placeholders and
genuinely different token segmentation remain visible in the metric. `align_ratio` is an edit-cardinality diagnostic, not a correctness
score: matching interchangeable repeated tokens (dot leaders, page numbers,
common CJK scalars) across semantically unrelated places can make a minimal
script smaller than an information-weighted script. `--minimal-diagnose PAIR`
prints the largest runs where flow changed tokens that the minimal script
keeps, with surrounding text. The
`neighbour_equal_moves` column counts moves whose nearest non-numeric
neighbours match, which is only a heuristic and also matches genuine
relocations inside dot-leader tables of contents. `old_conservation` and
`new_conservation` show `used/visible` glyphs, and `conservation_complete` is
true only when every visible glyph on both sides reached the token stream.

The quote HIT columns (`quote_hits`/`quote_located`/`quote_total`) count
reviewed quotes whose text occurs in the token stream. Every contiguous
occurrence is a candidate, so repeated text does not hide a changed
occurrence behind an unchanged one. Both the quote and the stream are
normalised at digit/letter transitions (`10th` and `10`/`th`, or `w3` and
`w`/`3`), because the engine's token boundaries follow source runs; this makes
location independent of that layout decision in either direction. A located
quote is a hit when any of its tokens lies in a Delete, Insert, or Replace
range, when it lies in a Move range, or when the quote is reported in the
furniture-change section (digit-masked). `missing_quote_ids` lists quotes with
no occurrence, `unhit_quote_ids` lists located quotes with no changed token,
and `quote_misses` gives each miss a reason (`unlocated`,
`unchanged_occurrence`, or `non_contiguous_occurrence`). Coverage only counts
changed tokens, so a precise minimal diff of a long quoted line can report a
low fraction by design. Quotes that cannot be located are reported as missing,
never as zero coverage.

`--legacy` additionally runs the existing native-text comparison
(`pdfdelta --native-text-only -q OLD NEW`) for every pair
inside the same 6 GB scope with a 180 s timeout and records
`legacy_exit`/`legacy_seconds`/`legacy_rss_kb`. It is off by default because it
roughly doubles corpus runtime.

`--repeat N` reruns each selected pair N times and fails when the compressed
report or either token sidecar differs by SHA-256 between runs. Use it as a
determinism check on the flow engine, not as a timing measurement.

`python3 -m unittest discover -s benchmark/realworld -p test_flow_eval.py`
pins the oracle tokenizer and `metric_token` normalisation. Those helpers are
shared by the PyMuPDF order oracle; quote matching is separate and uses
`split_token_edges`, so a quote-matching change cannot silently alter the
oracle columns.

`pdfbench flow-verify` runs the flow engine, including per-page reading-order
selection, against fixtures whose true edit is known: every generated
acceptance case under both local renderers, plus the vendored
Typst/Tectonic pairs in `fixtures/external/`. Invariance cases must report
zero content changes; replacements, insertions, and deletions must cover
exactly the mutated text (allowing a changed character inside one token, such
as `10` -> `20`); paragraph moves must be reported as one `Move` with the
moved text. The command prints one PASS/FAIL row per case and exits non-zero
when any case fails.

`pdfbench flow-metamorphic` constructs exact ground truth on real PDFs by
page-tree surgery. It edits a cached document with `lopdf` only: page objects
and `/Kids` entries are removed, repeated, or exchanged; no content stream or
resource dictionary is rewritten. Each edited copy is compared with the
untouched document, so the true changed-token multiset is known by
construction:

- `identity`: the document against itself must produce no change at all.
- `delete`: removing page *i* must only delete that page's flow tokens.
- `insert`: the mirror comparison must only insert them.
- `swap`: exchanging adjacent pages must produce no content change, only
  moves.
- `duplicate`: repeating a page must only insert its flow tokens.

The plan is deterministic by document size and has **no skipped cases**. Pages
are chosen as first, last, middle, and two file-name-seeded LCG picks for
documents of at most 64 pages; larger documents get a fixed five-case plan
(identity, delete, insert, swap, duplicate at the middle page). `--plan` prints
the per-file plan and total case count without running anything. Every planned
case runs; `--case-timeout-seconds` marks a case that exceeds the limit as a
timeout failure. A SWAP pair is planned only inside one page-tree node, since
moving pages across nodes would change inherited attributes. Clip-classification
work is budgeted per page, so duplicating a clip-heavy page does not depend on
how much work its siblings consumed.

Image-only pages are resolved by an encoded page identity (content streams,
page box, and image `XObject` streams hashed without decoding) and matched by
hash across the two documents, so A and its edited copy resolve their shared
image pages like the CLI does; unresolved pages stay incomplete instead of
being guessed.

The TSV columns are file, case, page, true/reported/correct changed tokens,
precision, recall, exact, move changes, seconds, and a detail column with the
change counts, furniture coverage, per-page order flips (identity and edited
side), and missing/extra token samples. Precision and recall are multiset
ratios of reported content tokens against the constructed truth. The truth for
the edited page is taken from the untouched comparison side when its chosen
order matches the identity run, so the joint furniture decision applies to the
truth too; an order-flip page keeps the identity truth so the flip stays
visible. Tokens reported in the furniture section are removed from both sides
before scoring. A case is exact only when precision and recall are 1.0, only
the expected change kinds appear (furniture-covered changes excluded),
extraction is complete, and no unexpected move or layout-only change remains.
`--text-edits N` adds up to N single-token text-edit cases per document: one
ASCII digit in a mapped single-byte text-show operand is replaced by another
digit code used by the same font, and the edited document is re-extracted to
prove exactly that glyph changed. The truth is one `Replace` with the before
and after token; no other page may change. `--merges N` adds up to N page-merge
cases (two adjacent pages stacked into one taller page with merged resources);
the truth is zero Delete/Insert/Replace changes and an equal all-glyph multiset,
with furniture-only line changes reported separately. `--limit-scale` raises
the extraction count limits uniformly for large documents. `--dump-edited DIR`
writes every constructed old/new PDF pair so a finding can be reproduced with
`pdfdelta flow`; outputs stay outside the repository. A document whose
unmapped placeholders all use name or outline identity also gets one `resubset`
case: harmless trailing bytes are appended to every embedded font program, so
today's program hash changes while names and outlines do not. The truth is zero
changes. A document that still has program-identified placeholders is skipped,
because for those the program bytes are real identity evidence.

The structural plan remains the default. On 2026-11-19 the final engine was
rerun on all seven batches: the 56-file structural plan, `--text-edits 3
--merges 2` text edits, merges, and the resubset case produced **807/807 exact**
cases, including **4 resubset** cases (232 + 138 + 163 + 146 + 64 + 16 + 48;
micro precision and recall 1.0000). The 803 structural/text-edit/merge cases all
remain exact. Skips were 42: 41 harness skips (28 merge pairs with no eligible
adjacent pages, 10 documents with no eligible text-edit page, 3 text edits whose
digit run is a page-number-like furniture run and is masked by design) and one
resubset case on the NASA handbook whose remaining program-identified
placeholders would make the zero-change truth false. The resubset count fell
from 14 to 4: the pdf.js supplement removed every placeholder from arxiv, gcc,
qgis-pyqgis-en, and qgis-doc-guidelines-es, and the project-authored table then
removed the last qgis-pyqgis-ja placeholders, so those documents no longer have
name/outline identity to exercise.

The final merge failures had two causes. Furniture clusters were selected
independently on each side, so a page deletion could change which cluster met
the page threshold and turn the other side's edge lines into content; the joint
rule now removes the union of both sides' selected relative-position ranges
from both documents. A merged page could also choose one reading order for two
stacked regions whose source pages had chosen different orders; a page split by
a gap much larger than its line pitch is now assembled band by band, each band
following the chosen order of the whole page it reproduces, with furniture
positions measured inside the band. Together these made the two khronos merges,
the oasis-mqtt pages 26-27 merge, and a bis-core page-41 delete/insert pair
exact.

The 3,100-page `postgresql-16-to-17-new.pdf` needs slightly more than the
default 5,000,000-glyph extraction budget, so the default limits stop with the
decoded simple-font glyph limit. At `--limit-scale 2`, A-vs-A identity completes
in **131 s** with an aggregate cgroup peak of **4.46 GB** (one child candidate
build: 40.7 s, 4.06 GB; the sparse page search and the Runs alignment dominate
the remainder). The move-rule variants reuse one memoized alignment per
strategy and skip the rules that cannot change the script, so they add no
second whole-document alignment on this A-vs-A run. The comparison extracts
both sides completely and reports zero changes. It has no old revision, so this measures identity and
resource use, not revision exactness. Its manifest records unsupported
non-rectangular clipping on pages 1-3,099; the flow A-vs-A run reports complete
extraction on both sides and no incomplete page.

```bash
systemd-run --user --scope -p MemoryMax=6000000000 -p MemorySwapMax=0 -- \
  target/release/pdfbench flow-metamorphic \
  --cache benchmark/realworld/cache --case-timeout-seconds 600 \
  > OUT/flow-metamorphic.tsv
```

Positional PDF paths replace the cache scan when investigating one file, and
`--dump-edited` accepts a directory under `/tmp`.

The page-counterpart search is exact but sparse. An inverted index from each
information-weighted token to the pages that contain it scores only page pairs
that share a token; a pair with no shared token has zero overlap by definition,
so the best match, retained matches, and adjacent pages are identical to the
all-pairs scan. On the 28-pair corpus the search change was byte-identical on
every JSON report and both token sidecars; the PostgreSQL A-vs-A comparison
dropped from 159 s to 133 s and the 28-pair wall from 209.9 s to 201.9 s. A unit
test compares the sparse candidates with a brute-force `bag_similarity` scan
over synthetic bags.

On the final build the 56-file structural plan is **624/624 exact** in the
per-batch runs, and the one-token rotation remainder is reported as part of its
move. The furniture rule removes the joined alphabetic running line at every
occurrence, which is what keeps a page merge invariant.

The differential audit (`benchmark/realworld/flow_audit.py`) compares flow with
an independent PyMuPDF reading-order token stream and GNU `diff --minimal`. It
reports per-pair agreement on the changed-token multisets and samples regions
where only one tool changed tokens; each sampled region is decided from the PDF
text inside its bounding box and the glyphs from `pdfdelta inspect --glyphs`.
Sampled runs are split at page boundaries, because a single-page clip cannot
compare a footer and the next page's heading. Before a region is called "oracle
correct" the classifier looks the changed tokens up in the whole other document
and in flow's change ranges: a token that both documents contain is a
re-pairing, not new or missing content. On the final corpus the mean agreement
is **0.751 old / 0.823 new** (Jaccard 0.747); a deterministic 200-region sample
gave 39 "flow correct", 34 "flow correct (reading order)", 2 "flow correct
(image identity change)", 136 "both acceptable" (equivalent minimal scripts,
moves/layout changes, tokenisation, numeric page-number alignment, text that
exists in the other document, unmapped placeholders, watermarks, or text absent
from flow's content), **0 "oracle correct"**, and 4 undecidable tokenisation
fragments with no text inside the region box. Those four regions were then
decided from the exact glyph boxes, PyMuPDF character boxes, and the content
stream operators: two are both acceptable (an equivalent minimal script around
a contents-page dash, and form-field tokenisation), and two are flow correct
(the Khronos `alpha` -> `α` revision and the OASIS URL version change are
reported; the changed fragments are tokenisation of separate font runs and
repeated URL text). The most asymmetric pairs are
dominated by numeric page numbers and line numbers (oasis-mqtt, nist-csf),
oracle-changed tokens on the same page (minimal-script alignment), or
unmapped-glyph placeholders (qgis-pyqgis-ja used to report 978; 881 were
`quotesingle` glyphs whose Type1C built-in encoding now recovers `'`, the
standard-14 Symbol/ZapfDingbats tables recover the w3c and Khronos values, and
the remaining names are outside the Adobe Glyph List). A targeted pass over all
flow-only tokens of the three most asymmetric pairs per side is recorded with
counts by change kind and cause.

The move detector now requires moved runs to carry information weight, not just
token count: a run whose tokens are all minimum-weight punctuation or
separators contributes zero and cannot be a move. A same-page move that spans
only low-weight or numeric changes and sits on a dot-leader contents page is
promoted back into the spine, so a table-of-contents entry whose page number
changed becomes one small replacement instead of moves of the dot-leader run
and the heading. The engine evaluates the old rule, the information-weight
rule, and the promotion rule for both alignment strategies, and keeps the
script with the fewest non-move changed tokens; the old rule is always a
fallback. The three rules share one memoized alignment per strategy, the
consecutive forced pairs of a promotion round reuse their previous segment
alignments, and a rule is skipped when it provably produces the same script:
no move candidate exists, or neither document has a dot-leader page so the
contents promotion cannot fire. On qgis-pyqgis-en this removes all nine dot-leader/heading moves and
reports `14 -> 16` as a single replacement (moves 555 -> 220 tokens, non-move
1,178 -> 900). Across the 28 pairs, non-move changed tokens fall from 906,908 to
**905,554** with no pair higher; total move tokens rise by 378 and the per-pair
move/replace deltas are recorded in the audit report. Order agreement is
unchanged because the rule only rewrites the edit script.

On the final build, the 28-pair corpus's independent PyMuPDF order agreement
averages **0.984368 old / 0.984722 new**, changed tokens are **900,815** across
all 28 pairs, and no pair's non-move changed-token count is higher than the
previous run. The final run totals **204.9 s** of pair wall time with a
**2.07 GB** peak RSS. The standard-14 Symbol/ZapfDingbats tables, the Type1C
built-in encoding, the pdf.js supplement, and the project-authored
unambiguous table removed placeholders without changing any other pair's token
text; reusing one alignment per strategy and unchanged alignment segments keeps
every report deterministic. One pair side (NIST SP 800-171 r3 new) remains near
the pre-change agreement, from two near-tie pages that choose layout while the
oracle order follows paint order; the layout order keeps the body prose
continuous and the footnote lines together, so it is the preferred reading and
both pages are reported as order-uncertain.

### Unmapped-glyph identity across font subsets

A revision pair often re-subsets the same font, so its embedded program hash
changes even though its glyphs do not. Comparing unmapped glyphs by
`(program hash, glyph id)` then reported about 1,200 false replacements on the
Khronos data-format pair. Identity now uses the strongest available evidence:

- **name:** subset-stripped `BaseFont` plus the exact glyph name from the PDF
  encoding, the cleartext Type1 `/Encoding`, or the Type1C charset;
- **outline:** a CID-keyed CFF `CharStrings` entry selected by CID; CID 0 is
  named `.notdef` by specification;
- **program:** the previous program hash and glyph id, only when no name or
  non-empty outline exists.

The first byte of the 32-byte identity hash tags the kind, text placeholders
print it, flow token text uses `\uFFFC<kind>:<hash>:<glyph_id>`, and native JSON
adds an `identity` field to `unmapped_tokens`.

Measured on the final corpus:

| metric | before | after |
|---|---:|---:|
| Khronos placeholders inside non-move changes, old | 1,282 | **4** |
| Khronos placeholders inside non-move changes, new | 1,607 | **329** |
| Khronos identity-only replace pairs | 1,217 | **0** |
| qgis-pyqgis-en placeholders inside changes | 98 / 98 | **0 / 0** |
| qgis-pyqgis-ja placeholders inside changes | 97 / 97 | **0 / 0** |
| corpus placeholders by identity kind | 4,651 legacy (program or narrow name hash) | **4,616 name / 22 outline / 13 program** |
| corpus non-move changed tokens | 903,772 | **900,816** |
| Khronos non-move changed tokens | 50,351 | **47,789** |
| qgis-pyqgis-en non-move changed tokens | 900 | **704** |
| qgis-pyqgis-ja non-move changed tokens | 973 | **779** |

The remaining 13 program-identity placeholders are one simple TrueType code in
the NASA handbook whose `post` table is format 3.0 and whose best `cmap` has no
entry for the code; no name exists and no outline can be selected. The 22
outline identities are CID-keyed CFF glyphs in unicode-standard-15-to-16-old.
No pair increased its changed-token count, and every pair's oracle agreement is
bit-identical to the previous run because the identity change only removes
false equal/unequal judgements inside placeholders.

The new **resubset** metamorphic case appends harmless trailing bytes to every
embedded font program, changing today's program hash while preserving names and
outlines, and requires zero changes. Before the supplement it ran on all 14 eligible
placeholder documents and was exact; the NASA handbook was skipped because its remaining
program-identified code would make the program bytes real identity evidence.

### TeX/AMS names from the pdf.js supplement

The Adobe Glyph List does not contain many TeX/AMS names. A bundled 121-entry
supplement from Mozilla pdf.js (Apache-2.0, commit
`fc0822a07ea0b2fde125dec4754ce4da4c52e956`, `src/core/glyphlist.js`, sha256
`48636549cbad67889b0fd5f59bc3e0bb732c0af545868f5db004749dcdf3012c`)
resolves names that AGL and the `uni`/`u` forms do not, after ToUnicode and
explicit Differences. Private-use and control values are omitted. The table is
in `crates/pdfdelta-core/src/pdf/font/pdfjs_tex.rs`; its header and
`THIRD-PARTY-NOTICES.md` record the provenance and Apache-2.0 text.

| metric | before supplement | after |
|---|---:|---:|
| corpus placeholder tokens | 4,651 | **484** |
| corpus placeholders by identity | 4,616 name / 22 outline / 13 program | **449 / 22 / 13** |
| Khronos placeholders (old / new) | 1,829 / 2,154 | **93 / 105** |
| Khronos placeholders inside non-move changes (old / new) | 4 / 329 | **0 / 12** |
| qgis-pyqgis-en placeholders | 98 / 98 | **0 / 0** |
| qgis-pyqgis-ja placeholders | 97 / 97 | **11 / 11** |
| qgis-doc-guidelines-es placeholders | 2 / 2 | **0 / 0** |
| gcc placeholders | 4 / 4 | **0 / 0** |
| arxiv placeholders | 1 / 1 | **0 / 0** |
| unicode-standard placeholders | 223 / 0 | 223 / 0 (no supplement entries) |
| NASA placeholders | 0 / 40 | 0 / 40 (27 name + 13 program) |
| corpus non-move changed tokens | 900,816 | **900,815** |
| Khronos non-move changed tokens | 47,789 | **47,788** |
| Khronos order agreement old/new | 0.952444 / 0.953658 | **0.960037 / 0.961101** |
| corpus mean order agreement old/new | 0.984075 / 0.984436 | **0.984346 / 0.984702** |
| pair wall / peak RSS | 197-205 s / 2.07 GB | **202.9 s / 2.07 GB** |

After the supplement, the plan was **809/809 exact**: the 803
structural/text-edit/merge cases plus 6 resubset cases. The supplement removed
the placeholders that made eight earlier resubset candidates applicable, so
those cases no longer exist; the NASA resubset remains skipped for its 13
program-identified tokens.

The supplement resolves 4,167 corpus tokens. The 198 Khronos TeX/AMS names
still unresolved (`lessmuch`, `greatermuch`, `negationslash`, `floorleft`,
`floorright`, `ceilingleft`, `ceilingright`) are absent from pdf.js; the
qgis-ja `asciigrave` (22 tokens) is also absent. The only local candidates for
those names (`glyphtounicode.tex` and `texglyphlist.txt`) are GPL-2.0-only and
are not bundled. Size variants such as `parenleftBig` and `parenleftbig` both
map to `(`, so the readable text no longer distinguishes the size variant.

### Project-authored unambiguous TeX mappings

The pdf.js supplement still left 198 tokens in seven names that are absent
from pdf.js. A project-authored MIT table
(`crates/pdfdelta-core/src/pdf/font/self_mappings.rs`) now maps the names whose
meaning is exactly one standalone Unicode character:

| name | code point | Unicode name |
|---|---|---|
| `lessmuch` | U+226A | MUCH LESS-THAN |
| `greatermuch` | U+226B | MUCH GREATER-THAN |
| `floorleft` | U+230A | LEFT FLOOR |
| `floorright` | U+230B | RIGHT FLOOR |
| `ceilingleft` | U+2308 | LEFT CEILING |
| `ceilingright` | U+2309 | RIGHT CEILING |
| `asciigrave` | U+0060 | GRAVE ACCENT |

It is consulted after AGL, the `uni`/`u` forms, and the pdf.js supplement.

| metric | before AA | after |
|---|---:|---:|
| corpus placeholder tokens | 484 | **286** |
| corpus placeholders by identity | 449 name / 22 outline / 13 program | **251 / 22 / 13** |
| Khronos placeholders | 93 / 105 | **10 / 12** |
| qgis-pyqgis-ja placeholders | 11 / 11 | **0 / 0** |
| corpus non-move changed tokens | 900,815 | 900,815 |
| Khronos order agreement old/new | 0.960037 / 0.961101 | **0.960538 / 0.961551** |
| qgis-pyqgis-ja order agreement old/new | 0.997454 / 0.997454 | **0.997562 / 0.997562** |
| corpus mean order agreement old/new | 0.984346 / 0.984702 | **0.984368 / 0.984722** |
| pair wall / peak RSS | 202.9 s / 2.07 GB | **204.9 s / 2.07 GB** |

The full metamorphic plan is **807/807 exact**: 803 structural/text-edit/merge
plus 4 resubset cases (khronos 2, oasis 1, unicode-standard 1). qgis-ja's two
resubset cases disappeared with its last placeholders.

Excluded names and reasons: `negationslash` (U+0338 COMBINING LONG SOLIDUS
OVERLAY; attachment order is not encoded by the glyph sequence),
`arrowrightabove` (U+20D7), `circleencl` (U+20DD), `circleslashencl` (U+20E0),
`diamondencl` (U+20DF) (combining/enclosing marks with the same context
problem), `U0C95_U0CCB.blwf` (script base+combining sequence with a below-base
form), the generic names `g109`, `g1093`, `g111`, `g131`, `g132`, `g133`,
`g1397`, `g14`, `g210`, `g218`, `g3`, `glyph296`, and the script names
`modi_letter_ha`, `modi_letter_ka`, `modi_letter_ra`, `modi_letter_sa`,
`modi_vowel_sign_aa`, `modi_vowel_sign_e`, `tibetangha`, `tibetangnyisshad`,
`tibetanintertsheg`, `tibetanka`, `tibetankha`, `tibetanshad`. Those 201
unicode-standard tokens and the 22 `negationslash` tokens stay placeholders;
the 28 CID 0 `.notdef` and 13 program placeholders are unchanged.

The two newly recovered manifest pairs are reported separately as held-out
numbers. `bis-core-principles-2012-to-2024` reports agreement
**0.99281 old / 0.99457 new**, 30,299 changed tokens, align ratio 0.818, and
5,431 moved old tokens; `bis-operational-risk-2011-to-2021` reports
**0.99715 / 0.99404**, 10,984 changed tokens, align ratio 0.933, 689 moved old
tokens, and 2/2 located quotes hit. Both extract completely and both
comparisons verify. The bis-core page-41 metamorphic failure motivated the
general furniture fix; no corpus threshold was tuned on these pairs.

Keep result directories outside the repository or under the gitignored
`benchmark/realworld/results/`. `--ngrams` dumps frequent cancellable token
n-grams for one durable report that also contains token and geometry data;
running without `--only` across the full corpus is a long operation and should
start with one small pair.

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

### Replay the frozen native panel

`benchmark/realworld/native-panel.json` is the unchanged 36-pair panel from
archive commit `9229676f47594a6cc2af8d5b54fdff64e441db15`, path
`benchmark/realworld/followup/panel.json`. It retains the official URLs, byte
counts, SHA-256 digests, original cache paths, and references to the original
development and blind input manifests. These inputs describe the separate
36-pair panel; the 29-pair revision manifest is not a replacement.

Install the streaming reader, recover public inputs, and audit their identity:

```bash
python3 -m pip install -r benchmark/realworld/native-panel-requirements.txt
python3 benchmark/realworld/native_panel.py fetch --output benchmark/realworld/results/input-recovery
python3 benchmark/realworld/native_panel.py check --output benchmark/realworld/results/input-audit
```

Existing mismatched files are preserved. Failed or oversized downloads remain
separate from verified cache inputs, and the input audit retains every pair.
The tool never replaces a frozen input with a newer document of the same name.
If a transfer was interrupted, use `fetch --retry-downloads` with a new output
directory. It retains the failed bytes by hash before retrying the same URL;
`--timeout SECONDS` controls acquisition time separately from comparison time.

Build the release CLI under the shared 6 GB limit before capturing. Where a
delegated user systemd memory controller is available:

```bash
systemd-run --user --scope -p MemoryMax=6000000000 -p MemorySwapMax=0 -- \
  python3 benchmark/realworld/native_panel.py capture --binary target/release/pdfdelta \
  --output benchmark/realworld/results/native-replay
python3 -m unittest discover -s benchmark/realworld -p test_native_panel.py
```

An equivalent container memory limit is also supported. Capture checks the
effective cgroup limit, processes pairs sequentially at `--limit-scale 1`,
uses a 180-second timeout, and stops adding reports at a 1 GB output budget.
It records the source commit/tree, production diff, binary and input hashes,
command arguments, report hashes, elapsed time, memory peak, and OOM counters.
Output directories must be new, preventing a duplicate capture from silently
overwriting evidence. `--pair ID` selects a diagnostic subset while keeping
the denominator at 36 and marking every unselected pair `not_run`.

Capture holds one repository-wide advisory lock. Engine children inherit the
lock, so a surviving child blocks a second capture even if its supervisor dies.
Before each pair the running user must create, flush, read and remove a probe
in the output directory. At least 3 GB must be free: 2 GB retained as headroom
plus the 1 GB report allowance. During a pair, falling below the 2 GB reserve
terminates that process group and records the storage failure. Retained failed
attempts count toward the total output allowance; they are never silently deleted.
These checks cannot reserve filesystem space against unrelated writers.

After an interruption, repeat the identical command with `--resume`. Before
reusing completed rows it verifies input and report hashes, streams the report
and compressed log, and checks the report contract and exit code. Binary,
source tree, production patch, scripts, panel, selection and timeout must match.
Each pair's summary is atomically replaced and flushed to disk. Interrupted
outputs and earlier summaries are retained in `attempts/`; completed results
are not rerun. Explicit failures are preserved unless `--retry-failed` is also
given, after the underlying problem has been resolved. A changed binary or
experiment configuration requires a new output directory.

```bash
python3 benchmark/realworld/native_panel.py capture --binary target/release/pdfdelta \
  --output benchmark/realworld/results/native-replay --resume
python3 -m unittest discover -s benchmark/realworld -p 'test_*panel.py'
python3 -m unittest discover -s benchmark/realworld -p test_capture_guard.py
```

The resume command requires the same shared memory limit as the original capture.
This runner resumes verified experiments; it does not change proof criteria,
substitute inputs, upload files, or create a persistent scheduling service.

The reader streams gzip and JSON to the end, validates the native completion
predicate against extraction, coverage, unresolved regions, candidates and
truncation, and checks that the exit code agrees. A meaningful completion also
requires positive native-text token counts on **both** sides. Empty-text
comparisons, missing inputs, hash mismatches, timeouts, output-limit stops and
invalid reports stay in the denominator and never count as complete. Engine
completion and empty-text outcomes remain separately visible in each row.
This measures completion only; it does not establish annotation-scored accuracy.

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
