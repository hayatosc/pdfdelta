# Remaining nine development comparisons

The three MEXT pairs, care-skills revisions, GPT-3, Llama 2, NASA buckling,
IPCC synthesis and ECB annual reports complete the registered development
comparison attempts. Source references were fixed before comparison output.
Eight pairs resolve all 32 literal selectors. ECB contributes four unresolved
selectors: its old input exceeds the existing 65,536 CID-width-entry limit.
Both ECB covers were visually inspected, but visible years are not substituted
for unavailable native glyph positions. Annotation resolution is 8/9 pairs and
32/36 attempted selectors.

Each resolved pair has one changed-range target and one unchanged control.
The MEXT upper-secondary target is a cover date, not flowing body text; the
frozen reference's `single_column_selected_body` flag is inapplicable there.
Care-skills literal references retain interleaved ruby. GPT-3 changes wording
around unchanged percentages. These references do not assert unique internal
edit histories or exhaustive whole-document strict gold.

## Results and limits

Both revisions capture 26/27 attempted reports: eight native and 18 shared.
All captured reports exit 3 with incomplete comparison; both ECB native runs
exit 2. Across the two revisions, capture is 52/54 and complete is 0/54.
Native reports have no strict events or legacy proven regions. Shared reports
have A=0, B=0, scope C=0 and legacy C=0. Each shared route misses all eight
resolved positive targets (0/8). Prediction precision is undefined because
there are no predictions. Exact-event and source-position recall are also
undefined because these references do not provide unique internal edit gold.
Zero changed-mask intersections on the unchanged controls do not prove that
their contents were completely compared. ECB controls remain unscored.

All eight captured native contracts match the baseline: four byte-for-byte,
and four after removing only the newly recorded page field immediately after
`glyph_gap` scope. Streaming hashes verify the complete reports, including
their large evidence bodies. Removed-field counts and resulting hashes are
in `contracts.json`.

Sixteen of 18 shared contracts match after projecting additive fields to the
baseline shape. The projection preserves pre-existing issue page values;
IPCC already reported these values before the boundary gained its own page.
The two care-skills shared contracts differ in actual acquired render evidence:
the old document has 164 rendered pages at baseline and 172 currently, with
107 versus 99 document-rendering deadline failures. Both retain the evidence
process deadline failure. This difference is not normalized away or presented
as an accuracy improvement. Fixed budgets can yield different partial evidence
under concurrent load. No PDF-wide speedup is inferred from these samples.

The run records retain per-process wall time, maximum RSS, exit status, output
size and hashes. Total sequentially summed process times are 514.98 seconds
at baseline and 555.59 seconds currently; these are not elapsed batch times.
Maximum process RSS is 1,334,460 versus 1,330,512 KiB, not live heap memory.
Captured output totals are 6,558,753,674 versus 6,558,815,173 bytes. Raw reports
remain outside version control. Source coverage, stage completion and acquisition
reasons remain in `scores.json`; no full-document success is claimed.

## Reproduction

Build each recorded revision, verify executable and registered input hashes,
and capture in a fresh destination:

```sh
PYTHON_UV=0 python benchmark/realworld/next/development/capture-comparisons.py \
  target/release/pdfdelta benchmark/realworld/cache/next-dev /tmp/remaining-replay \
  --pair mext-primary-japanese-2008-to-2017 \
  --pair mext-lower-secondary-japanese-2008-to-2017 \
  --pair mext-upper-secondary-japanese-2009-to-2018 \
  --pair mhlw-care-skills-original-to-revised --pair arxiv-gpt3-v1-to-v4 \
  --pair arxiv-llama2-v1-to-v2 --pair nasa-buckling-8007-1968-to-2020 \
  --pair ipcc-synthesis-ar5-to-ar6 --pair ecb-annual-2023-to-2024 \
  --implementation YOUR_BUILD_COMMIT
```

Resolve the corresponding `annotations/<pair>.json` with
`pdfbench validate-literal-selectors` and the registered old/new files before
scoring. Preserve ECB's resolver failure. `native-selected-fields.json` retains
the captured summaries and empty result arrays; shared zero-result counts
establish missed targets directly. Metadata binds reference and result hashes.
The interrupted first capture is recorded separately in `interrupted-capture.json`;
the replay uses unchanged source references and is not an additional sample.
