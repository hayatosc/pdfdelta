# PDF-native text scope correction

The primary completion metric is the strict **PDF-native text** comparison
executed with the CLI's `--native-text-only` route. Image pixels, path lettering
and OCR are outside the current scope; native text drawn inside figures and Form
XObjects stays inside it. Unsupported, unmapped or truncated native text remains
an explicit incompleteness and is never promoted to an empty or vacuous success.

Measured on the frozen 36-pair panel:

| Capture | Binary | Complete (non-vacuous) | Complete (vacuous) | Incomplete | Failed | Denominator |
| --- | --- | --- | --- | --- | --- | --- |
| baseline | `54a27ef2…` (`a3c8b08` build) | 0 | 0 | 35 | 1 | 36 |
| current | `836e8bf1…` (held worktree build) | 0 | 0 | 36 | 0 | 36 |

The headline stays 0/36, but the corrected scope changes what the number is
about: the remaining obstacles are unresolved native text regions, five pairs
with incomplete native extraction, and an assessment work limit, not the
non-text painting condition that the document-wide common-text contract adds.
The common document-wide 0/36 stays recorded as historical old-scope evidence.

## Why the scope was mixed

The completion contract has two different consumers, and the primary evaluation
had been using the wider one:

- The common `--channels text` route includes a page in the text inventory only
  when no non-text painting remains (`crates/pdfdelta-core/src/document/native.rs`,
  the `last_non_text_paint` condition introduced with the shared graph). The
  document-wide investigation therefore reported "possible image/path text" as
  an acquisition blocker on all 36 pairs.
- The native adapter already owns a narrower predicate: `--native-text-only`
  reaches `compare.rs::compare_documents`, `pipeline.rs` comparison and
  `report::summarize` (`crates/pdfdelta-core/src/report/mod.rs`), which requires
  no change candidates, proven-changed or unresolved regions, full token
  coverage, complete extraction and an untruncated assessment. Non-text paint is
  not an incompleteness condition there, and the common `document/coverage.rs`
  and `document/text_scopes` cuts are not on this path.
- The evaluation driver hardcoded the text route, so the fixed panel had never
  been replayed through the native predicate as a primary metric.

The scope correction is not an algorithm improvement and does not change any
production predicate, panel, denominator or limit. The common route and its
`last_non_text_paint` condition remain intact for document-wide comparisons.

## Contract

- Route: `pdfdelta --native-text-only`; report schema 11 (the common route keeps
  schema 2).
- Primary predicate: the report's own `summary.comparison_complete`, recomputed
  from the contract fields and required to agree.
- Exit codes: 0 no content change, 1 content changes detected, 2 execution
  error, 3 incomplete comparison. A captured row's completion must agree with
  its exit code (0/1 complete, 3 incomplete).
- A pair with zero native tokens on both sides is recorded as **vacuous**, not
  as a meaningful completion. No vacuous pair occurs in this panel.
- Unsupported filters, encrypted input, unmapped glyphs and uncertain regions
  stay incomplete; they are never converted into empty text.

## Method

- Panel: `benchmark/realworld/followup/panel.json`, sha256
  `c3aa4a5dd7edb3b7b4b5144ea31a9eb48645fa5ebe5a27546f3be09b0dd6e744`,
  36 pairs / 72 inputs, limit scale 1, 180-second outer timeout per input,
  missing and failed captures retained in the denominator.
- Baseline binary: the registered first-followup executable
  `benchmark/realworld/cache/next-execution/pdfdelta-9093cab` (sha256
  `a74bf905…`) is not present in the checkout, so the registered `a3c8b08`
  baseline build is used: sha256
  `54a27ef2d503fda062a2010f8e8419fa07a502e5430ef77b88e587f5c308d0ca`.
  No results from different binaries are mixed.
- Current binary: release build of the held worktree
  (`836e8bf1c0d6cededbeca77ba018bd1d85ac657d6923e60984c0d9b12a96b3cc`),
  including the uncommitted strict-FlateDecode and declaration-projection work.
  It is not a released revision.
- Execution conditions differ by row and are recorded per source: the current
  capture and the recorded nist-controls baseline attempt ran serially under
  `systemd-run --user --scope --quiet -p MemoryMax=6G -p MemorySwapMax=0`;
  the other baseline rows were captured earlier without a memory cap. Internal
  limits (limit scale 1, 180-second timeout), route, panel, binary hash and
  schema are identical across all rows; the memory condition is not.
- The v2 records under
  `benchmark/realworld/cache/completion-investigation/native-text-scope-v2/`
  reference the v1 raw captures read-only. They re-verify every captured row
  against the frozen panel: exact 36 panel IDs, the per-pair runs record and
  its hash, the old/new input hashes, the manifest hash, route, exit code, the
  copied binary hash, and the report bytes and hashes. Merged sources and their
  memory conditions are re-opened from their own summaries.
- Native reports can reach multi-gigabyte sizes. A bounded, structure-aware
  reader validates the report's pretty-printed members with byte-range searches,
  and report hashes are streamed and memoized, so report size cannot exhaust
  memory. Malformed, truncated, compact-formatted or trailing-data reports fail
  closed; `assessment: null` is distinguished from a missing or corrupt
  assessment.

## Results

Baseline (`54a27ef2…`, 35 captured rows plus the failed nist-controls row):

- 0 complete, 0 vacuous, 35 incomplete; difference status 12 `detected`,
  23 `indeterminate`.
- 85,959 unresolved regions and 19,783 tentative candidates in total.
- Mean alignment coverage 0.108 (minimum 0.0); eight sides in five pairs have no
  coverage figure because extraction is incomplete and the assessment stopped
  first.
- Five pairs have incomplete extraction (10 issues: 5 unsupported, 5 unresolved).

Current (`836e8bf1…`, 36 captured rows):

- 0 complete, 0 vacuous, 36 incomplete; 12 `detected`, 24 `indeterminate`.
- 86,781 unresolved regions and 20,053 tentative candidates.
- Mean alignment coverage 0.104; the same eight sides lack coverage.
- Five pairs have incomplete extraction (23 issues: 5 unsupported, 18 unresolved).

The per-pair figures are identical between the two captures except where held
work changes them: MHLW now surfaces 15 strict-FlateDecode issues (streams
`2 0 R`, `1919 0 R`, `561 0 R`, `865 0 R`, `1974 0 R` across pages 1, 17–24,
77, 121, 206, 208) that the earlier lenient decoder silently masked, and
nist-controls is measured in the current capture instead of failing at
summarization.

Largest native residuals (baseline unresolved regions / coverage):

| Pair | Status | Unresolved | Tentative | Coverage old/new |
| --- | --- | --- | --- | --- |
| ecb-annual-2023-to-2024 | indeterminate | 15,988 | 4,416 | 0.0 / 0.0 |
| nist-risk-management-37-r1-to-r2 | indeterminate | 9,711 | 3,676 | 0.0 / 0.0 |
| mext-lower-secondary-japanese-2008-to-2017 | detected | 8,036 | 6 | 0.032 / 0.015 |
| nist-authentication-63b-to-63b4 | indeterminate | 6,040 | 1,921 | 0.001 / 0.001 |
| nist-contingency-34-to-r1 | indeterminate | 5,755 | 1,791 | 0.0 / 0.0 |
| nist-incident-handling-r2-to-r3 | detected | 5,237 | 1,839 | 0.012 / 0.025 |

The smallest native residuals are the IRS schedules: Schedule SE has 21
unresolved regions and 4 tentative candidates with both extractions complete
and coverage 0.917/0.915; Schedule C has 113 unresolved and 8 tentative with
both extractions complete and coverage 0.741/0.742. The next algorithm
investigation targets residuals of this kind.

Five pairs have incomplete native extraction:

- `mhlw-care-skills-original-to-revised`: unsupported curved clipping path on
  page 89 both sides; the current build additionally reports the malformed
  FlateDecode streams above.
- `nasa-buckling-8007-1968-to-2020`: new side page 85, a `TJ` font code with no
  Unicode mapping or stable font identity.
- `ipcc-synthesis-ar5-to-ar6`: new side page 22, curved clipping path.
- `arxiv-ddpm-v1-to-v2`: both sides page 1, glyph relationship to a convex
  clipping region uncertain.
- `arxiv-faster-rcnn-v1-to-v3`: unmapped font codes (old page 7, new page 9) and
  non-convex clipping quadrilaterals (new pages 11–12).

## Memory incidents and the failed baseline row

The nist-controls pair is the extreme case: 1.5M native tokens per side with
zero resolved coverage and an assessment stopped at its 32M work-unit limit.
Its driver run completed (exit 3, 81.99 s, peak RSS 3.4 GiB) and wrote a
10,611,447,738-byte report (sha256 `fbb65a6a…`), but the capture-side
summarization loaded the whole report and was killed by the 6G memory cap before
writing a row. Per the execution rules the pair is recorded as **failed —
memory limit exceeded** in the 36 denominator, not as a success, and was not
retried under the same condition. The salvaged `summary` object classifies it as
`comparison_complete: false`, `indeterminate`, 904 unresolved regions, 273
tentative candidates, 0/1,514,313 and 0/1,552,700 coverage. The earlier uncapped
attempt in `baseline-native` wrote the same report without a row; both attempts
are recorded, and the v2 audit verifies the failure's runs record and report
fragment. A first collector audit also exceeded the cap while hashing a
multi-gigabyte report; the audit now streams and memoizes hashes. The current
capture measures nist-controls with bounded extraction and keeps it as an
ordinary incomplete row. Memory behaviour remains an investigation topic.

## Controls, gates and acceptance

- Twenty-eight controls now drive the production reader, summarizer and
  collector with one injected defect each, starting from a valid 36-row
  fixture capture: missing fields, string booleans and counts, missing or
  mismatched coverage, image scope, false or indeterminate completion, missing
  assessment, truncated tails, trailing garbage, compact formatting, wrong
  pair, missing row, wrong route, wrong schema, wrong report hash, wrong input
  hash, wrong exit code, wrong binary, an unrecorded failure, and brace/escape
  decoys. Positive controls require the valid capture, vacuous classification,
  failure retention and escaped issue descriptions to pass.
- Focused CLI scope control (`native_scope_separates_text_from_path_and_image_content`):
  identical text with different paths is complete with no content change; a
  native text replacement beside changed drawings is detected; an image-only
  pair completes as vacuous with zero totals; a corrupt Flate stream exits 3
  with incomplete extraction.
- v2 gate results: formatting 0 and `git diff --check` 0, with raw logs under
  `logs/gates-raw/`. The heavy CLI scope and acceptance gates were not re-run
  because no Rust or CLI file changed after their v1 pass; their v1 logs and
  hashes are referenced by the v2 summary. The five core acceptance cases pass
  through `pdfbench verify` (48/48).

## Remaining native blockers

1. Unresolved native text regions dominate: 85,959 baseline / 86,781 current,
   concentrated in ECB, NIST risk management, MEXT lower-secondary and the
   other NIST pairs, with mean coverage 0.104–0.108.
2. Five extraction-incomplete pairs from unsupported curved clipping paths,
   unmapped font codes, non-convex clip quadrilaterals and uncertain convex
   clip relationships (23 issues in the current capture, including the
   strict-FlateDecode findings).
3. Eight sides in five pairs stop before any coverage figure; the assessment
   work limit (nist-controls) and early stops must be classified per pair.
4. Held work changes native results only by surfacing previously masked
   malformed streams; it adds no completed pair.

The declaration screen's five unscanned pages (MEXT 191–192 under its
diagnostic operator cap; MHLW new 1/77/121) are a limitation of that separate
probe, not an established native blocker: all three MEXT native pairs report
both extractions complete with no unresolved reasons, and MHLW's native
incompleteness is the clipping and malformed-stream evidence above.

## Limitations

- The current binary carries uncommitted held changes and is compared against
  the baseline binary, not against a released revision.
- Captured baseline rows were produced without a memory cap while the recorded
  failure and the current capture ran under the 6G cap; the v2 records carry
  per-source memory conditions and do not claim one shared condition.
- The primary metric deliberately excludes image/path text and OCR. The common
  route remains the document-wide contract and its 0/36 remains historical
  evidence, not a native result.
- This round re-audited existing raw captures; it ran no new capture and did
  not re-run the heavy gates whose Rust/CLI scope is unchanged.
- No production predicate, panel, denominator or limit was changed, and the
  scope correction is not counted as an algorithm improvement.

## Evidence index

- v2 audit summary:
  `benchmark/realworld/cache/completion-investigation/native-text-scope-v2/summary.json`
  (baseline record `6798e8b7…`, current record `88cebfa5…`).
- v2 merged records: `…/native-text-scope-v2/baseline-native/summary.json` and
  `…/current-native/summary.json`, referencing the v1 raw reports read-only.
- v1 raw captures (unchanged): `…/native-text-scope-v1/baseline-native/summary.json`,
  `…/baseline-rest-{ddpm,small,llama,mhlw,ipcc,controls}`,
  `…/current-native/summary.json`.
- Failed baseline pair: `…/native-text-scope-v1/baseline-rest-controls/nist-controls-53-r4-to-r5/`
  (`runs.json`, `.time`, 10.6 GB report fragment).
- Gate logs: `…/native-text-scope-v2/logs/gates-raw/`; audit log
  `…/native-text-scope-v2/logs-audit.log`.
- Driver and capture: `benchmark/realworld/remaining/completion-investigation/capture.py`,
  `collect_native_text_scope.py` and `native_scope_fixtures.py`; panel/driver
  under `benchmark/realworld/followup/` and
  `benchmark/realworld/next/development/`.
- Native predicate and adapter: `crates/pdfdelta-core/src/report/mod.rs`,
  `crates/pdfdelta-core/src/pipeline.rs`, `crates/pdfdelta-cli/src/compare.rs`.
- Historical common-text investigation: `README.md`, `strict-priority.md`,
  `docs/plans/source-completion/PLAN.md`/`PLAN.html`.
