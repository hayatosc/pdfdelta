# Proven-empty native sides complete two pairs; final two-run evidence

A native-text-only comparison whose other side carries no visible native text
is one-sided: every token of the present side is an insertion or deletion.
The pipeline settles that case and the assessment proves the per-block
domains, so `bunka-kana-1946-to-1986` and `faa-thunderstorms-b-to-c` now
complete with full visible coverage. Schedule SE was already complete, so the
corrected confirmed count is **at least 3/36**; the earlier `2/36` note
undercounted it. The full 36-pair panel was not re-captured, and no 36/36
claim is made.

## Result (native-text-only, final binary `0db25404`)

| Pair | Before | After |
| --- | --- | --- |
| `bunka-kana-1946-to-1986` | indeterminate, 0 changes, 61 candidates, 429 unresolved, new coverage 0.0 | complete, 525 insertions, 0/0, coverage 1.0 |
| `faa-thunderstorms-b-to-c` | indeterminate, 0 changes, 266 candidates, 694 unresolved, coverage 0.0 | complete, 286 insertions, 0/0, coverage 1.0 |

Both pairs extract completely on both sides and are non-vacuous. The old
sides carry no visible native text: the bunka old PDF is an image-only scan
(0 extracted glyphs, no page text operators, 13 image draws); the faa old PDF
has a 15,715-glyph OCR layer, all render mode 3 (invisible), and the
comparison keeps invisible text outside visible-content comparison (README).
Neither zero is an extraction failure; the extractor reports complete
extraction with no issues.

Each pair was captured twice with the same binary, inputs, arguments and
default budget; both reports are bit-identical. Report audit: every change is
an insertion, no occurrence names the old side, all 5,677 / 27,732 extracted
new-side glyphs are owned exactly once (no repeats, no omissions), the
resolution ranges tile the new side with no gap or overlap, and all 1,051 /
573 relations are established and search-complete with no reason. FAA spends
the full 32M work but still reports no candidate, no unresolved region and no
incomplete search, so the exhaustion hides nothing.

## Scope correction

These are visible-native-text completions. Image pixels, path lettering, OCR
text layers, handwriting and any non-painting render mode stay outside the
comparison scope, so the result does not mean the whole document content
matches. The independent producers are the Agency for Cultural Affairs
(Japanese government) and the FAA; the panel records their input URLs and
SHA-256 hashes, which match the captured inputs.

## Pipeline-driven tests and controls

The previous tests entered `compare_aligned` with a hand-built alignment.
`crates/pdfdelta-core/tests/pipeline_fixture.rs` now drives the real pipeline
with `Document<Glyph>` fixtures: insertion and deletion positives, a positive
with repeated text at identical coordinates, an empty-looking side carrying a
real document-scoped or glyph-gap extraction issue, a present-side ambiguous
line break, and work or range limits that must fail closed. Every case
summarizes with the actual extraction status. `pdfbench verify` passes 48/48,
including the five release acceptance cases by name: line-wrap and page-break
invariance with no content change, and text replacement, paragraph insertion
and paragraph deletion with exactly one expected change each.

## Deferred equal-fragment resource boundary

The deferred strict-closed equal-fragment list stored two cloned `TextSpan`s
per candidate and cloned them again before the tail pass charged anything.
It now stores only the relation index; the tail pass charges the transient
span copy before it allocates and the copy uses fallible block-list
reservations. A low-budget regression test pins the charge-before-copy gate
and the index-sized element.

## Holdout

No unused revision pair exists in the current input cache. All 24 `next-dev`
and 12 `next-blind` revision pairs are already in the frozen panel; the other
cached old/new pairs are review copies of panel pairs or synthetic fixtures;
the round2/round3 blind download attempts for ietf-tls/http/sip, gnu-gpl,
japan-post-terms and arxiv-resnet all failed and have no cached PDFs. No
holdout was run, and this note is the evidence instead of a claim.

## Evidence

- Final artifact: `benchmark/realworld/cache/completion-investigation/native-completion-final-v1/`
  (`manifest.json`, panel and input hashes, both runs per pair, reference
  subset checks, report and PDF audits).
- Commits: `b94ce79` (comment), `933965c` (resource boundary), `6a1a9f5`
  (pipeline tests); the proven-empty settlement is `37b0a48`.
- Quality gates on the final source: formatting, workspace clippy, workspace
  tests, `--all-features --lib` (1,380 passed) and rustdoc all exit 0.
