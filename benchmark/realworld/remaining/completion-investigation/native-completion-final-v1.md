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

## Comparison scope

These are visible-native-text completions. Image pixels, path lettering,
invisible text (for example render mode 3) and handwriting stay outside the
comparison scope, so the result does not mean the whole document content
matches. Visible text layers are compared, including a visible OCR text
layer; only non-painting text is excluded. This scope is unchanged from the
frozen baseline: the baseline already counted FAA old at
`old_alignment_coverage.total_tokens = 0` (native-text-scope-v2 baseline
summary, report sha256 `f65ed8269daee054c6ffa1d4ba18d0696a6cd475325e2993c8f9f7ba8578bd27`),
so the invisible OCR layer was outside the comparison before this work. The
independent producers are the Agency for Cultural Affairs (Japanese
government) and the FAA; the panel records their input URLs and SHA-256
hashes, which match the captured inputs.

## Native source audit

The final audit already checked glyph ownership as Counter multiplicity
rather than set membership: every change glyph instance is compared with the
resolution's glyph instances, references repeated inside one change are
counted, and tiling gaps or overlaps are counted per block. It did not check
the event ranges or text against extraction evidence, so this record adds
that check. For every one of the 525 bunka and 286 faa change events, each
referenced native glyph exists in `inspect --glyphs` output, its page, bbox,
content-stream object and generation, and operator index equal the native
glyph record, no span references a glyph twice, the canonical range length
equals the text length (525/525 and 286/286), and the canonical text equals
the concatenation of the referenced native glyph texts (332/525 and 220/286
byte-exact; 525/525 and 286/286 after whitespace normalization, which is the
only remaining difference: rebuilt separators and U+3000 mapped to a plain
space).

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
holdout was run at that time, and this note was the evidence instead of a
claim.

A later, pre-frozen follow-up requested two more pairs:
`irs-w9-2018-to-2024` (`fw9--2018.pdf` to `fw9--2024.pdf`) and
`irs-w4-2022-to-2023` (`fw4--2022.pdf` to `fw4--2023.pdf`), with URLs, current
HEAD, final binary, default limits and the overlap check fixed in a manifest
before any download. The check found `irs-w9-2018-to-2024` is already a frozen
panel member (`followup_target_index: 8`), so it cannot be an independent
holdout; it was captured as a panel reassessment with its inputs, denominator
and scope unchanged. `irs-w4-2022-to-2023` is unused (the panel's W-4 pair is
2024 to 2025) and became the independent holdout. Both pairs are two-sided
and both remain incomplete on the final binary: W9 exits 3 with 0 changes, 91
candidates and 1,011 unresolved regions; W4 exits 3 with 0 changes, 80
candidates and 621 unresolved regions. Both sides extract completely with no
issues and both sides carry tokens (W9 34,865 old / 37,481 new; W4 19,888 /
19,738), so the proven-empty one-sided path must not apply and cannot have
applied. The holdout adds no completion: the count stays at least 3/36, and
the W9 reassessment lost no resolved tokens against its predecessor. No
production code was changed after seeing these results.

The same native-evidence checks ran over the holdout spans: 1,169 W9 and
1,006 W4 candidate and unresolved spans (76,804 and 40,423 glyph references)
all have native records, matching provenance, unique references inside a
span, and canonical range length equal to the text length. Their text equals
the native glyph sequence except for whitespace normalization, separator-only
spans whose sources are adjacent boundary punctuation, leading or trailing
boundary evidence glyphs, and unmapped glyphs omitted from the canonical text
and listed in `unmapped_tokens`.

## Original problem evaluation

The increment's definition of done is at least two distinct pairs in the
unchanged 36-pair panel newly complete under `--native-text-only` relative to
the captured native baseline, with no previously complete pair lost, two
captures of the same final build, independent producer families, the five core
acceptance cases and the project checks. The corrected native baseline is
0/36; this increment confirms at least 3/36. `irs-schedule-se-2024-to-2025`
was already complete and stays complete, and `bunka-kana-1946-to-1986` and
`faa-thunderstorms-b-to-c` are newly complete, all three on the same binary
`0db25404` with two bit-identical captures per pair. The scope, the 36-pair
denominator and the default limits are unchanged from the baseline.

The four controls (SE, C, 1099-MISC, W-2) lost no resolved token positions
and C keeps its 64-token gain. The two new completions come from two
independent producer families (Agency for Cultural Affairs and FAA). The
pre-frozen `irs-w4-2022-to-2023` holdout stays incomplete and is a negative
control outside the denominator, so it adds no count and is not mixed into
the 3/36. `pdfbench verify` passes 48/48, including the five release
acceptance cases, and the five quality gates on the final source all exit 0.
The increment threshold is met; the full 36 pairs were not re-captured on the
final binary.

At least 3/36 also marks the remaining gap: the common two-sided non-empty
revision case still ends incomplete for most of the panel, many pairs remain
unmeasured on the final binary, and image or OCR comparison execution is
still unimplemented. The distinction stays explicit: a visible existing OCR
text layer is compared, while invisible text such as render mode 3 has always
been outside the visible-content comparison.

## Plan acceptance and cleanup

The run's plan (`docs/plans/source-completion/PLAN.md` and its companion
`PLAN.html`) defined this increment's acceptance as at least two newly
complete pairs, two same-build captures, independent producer evidence, the
five core acceptance cases and passing checks. Its completion report and
cleanup section requires preserving the original problem and result in
durable artifacts before deleting only the two run-owned planning files.
Copies are preserved at
`benchmark/realworld/cache/completion-investigation/native-completion-final-v1/plan/PLAN.md`
(sha256 `d01d00fb701d405c3d7947e4d00abf098269d72ac456228472b2251103700e46`)
and `benchmark/realworld/cache/completion-investigation/native-completion-final-v1/plan/PLAN.html`
(sha256 `52b39fa71a58c4c8e9d251dc010c9c00ac5ac9a61c472be1753e15f5e305cc4b`);
the workspace paths were deleted and are not cited as evidence.

## Evidence

- Text-operator audit note: `benchmark/realworld/cache/completion-investigation/native-completion-final-v1/audit-note-text-operators.md`
  (sha256 `b552faf5b7ec9d508d50638a68ef8b097a02d50b2d9f72cd8e94e821b139bee3`),
  preserved raw-stream and parsed page-stream tool copies, and the saved
  recursive glyph evidence; the two stored numbers are not rewritten.
- Final artifact: `benchmark/realworld/cache/completion-investigation/native-completion-final-v1/`
  (`manifest.json`, panel and input hashes, both runs per pair, reference
  subset checks, report and PDF audits).
- Holdout artifact: `benchmark/realworld/cache/completion-investigation/native-completion-holdout-v1/`
  (`manifest.json` with the pre-frozen selection, download ledger, PDF audit,
  captured reports, time and RSS, and the native-evidence audits).
- Commits: `b94ce79` (comment), `933965c` (resource boundary), `6a1a9f5`
  (pipeline tests); the proven-empty settlement is `37b0a48`.
- Quality gates on the final source: formatting, workspace clippy, workspace
  tests, `--all-features --lib` (1,380 passed) and rustdoc all exit 0.
