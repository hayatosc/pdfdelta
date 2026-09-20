# Baseline: audited native-text completions (C0/C1)

Run directory: `benchmark/realworld/results/native-12-of-36-2026-09-20/`.
Raw capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/baseline-native/`.

## Frozen identity

- Frozen panel: `benchmark/realworld/followup/panel.json`, SHA-256
  `c3aa4a5dd7edb3b7b4b5144ea31a9eb48645fa5ebe5a27546f3be09b0dd6e744`; all 72
  input paths exist and every content SHA-256 matches.
- Source revision: `3bfbc880a4774d343c54308c2b9f4f77d8650610` (clean apart from
  the untracked run plans).
- Comparison executable SHA-256:
  `d3aade2874b60dd845c8c00b9cffc8c2ef37eb91fb413fe4425d9eb1ea101fc6`.
- Route `--native-text-only`, `--limit-scale 1`, 180 s per-process deadline,
  default budgets, visible native text only (no OCR added).
- Capture driver exit 0; all 36 rows `captured`; no failed or unavailable rows.
- Audit recomputed completion from the raw reports' validated fields and
  required agreement with the production boolean; report hashes were
  re-verified. Full machine-readable scorecard: `baseline-scorecard.json`.

## Audited result: 3/36 eligible

Complete, non-vacuous: `irs-schedule-se-2024-to-2025`, `bunka-kana-1946-to-1986`,
`faa-thunderstorms-b-to-c`. No both-empty rows, no vacuous rows. These are the
three known controls, reproduced on the frozen inputs and default settings; the
fresh baseline does not exceed the historical lower bound.

## Per-pair scorecard (incomplete first, nearest-to-complete first)

The last three columns are default-budget diagnostic traces
(`collect_traces.py`): summed old+new uncertain-line counts, then new-side
indexed features / emitted alignment spans / candidate visits. Traces are not
part of the completion metric.

| pair | eligible | predicate failures | changes | unres | cand | proven | gap tokens | old/new resolved | extraction o/n | trunc | wall s | report MB | trace inter/untrust | indexed/spans/visits |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | ---: | ---: | --- | --- |
| `irs-schedule-se-2024-to-2025` | yes | — | 12 | 0 | 0 | 0 | 0 | 5485/5485 · 5502/5502 | T/T | False | 0.37 | 46 | 0/18 | None/94/5821 |
| `faa-thunderstorms-b-to-c` | yes | — | 286 | 0 | 0 | 0 | 0 | 0/0 · 27731/27731 | T/T | False | 0.82 | 79 | 0/10 | None/1/0 |
| `bunka-kana-1946-to-1986` | yes | — | 525 | 0 | 0 | 0 | 0 | 0/0 · 5484/5484 | T/T | False | 0.26 | 17 | 341/18 | None/1/0 |
| `irs-schedule-c-2024-to-2025` | no | tentative_candidates, unresolved_regions, coverage_gap | 11 | 19 | 4 | 0 | 301 | 6619/6774 · 6634/6780 | T/T | False | 0.83 | 57 | 0/83 | None/185/14185 |
| `irs-1099-misc-2024-to-2025` | no | tentative_candidates, unresolved_regions, coverage_gap | 12 | 110 | 2 | 0 | 5333 | 8228/11018 · 8224/10767 | T/T | False | 0.88 | 86 | 0/284 | None/17/3789 |
| `irs-w2-2024-to-2025` | no | tentative_candidates, unresolved_regions, coverage_gap | 13 | 403 | 6 | 0 | 21079 | 10002/20495 · 10002/20588 | T/T | False | 1.61 | 170 | 446/68 | None/49/23883 |
| `irs-w4-english-2024-to-2025` | no | tentative_candidates, proven_changed_regions, unresolved_regions, coverage_gap | 2 | 416 | 60 | 3 | 29558 | 5492/19987 · 6081/21144 | T/T | False | 1.38 | 144 | 122/172 | None/307/23797 |
| `faa-maintenance-records-c-to-d` | no | tentative_candidates, unresolved_regions, coverage_gap | 88 | 481 | 481 | 0 | 45156 | 0/0 · 17933/63089 | T/T | False | 2.09 | 239 | 0/30 | None/1/0 |
| `arxiv-ddpm-v1-to-v2` | no | extraction_incomplete, extraction_issues, tentative_candidates, proven_changed_regions, unresolved_regions, coverage_gap | 35 | 122 | 36 | 8 | 68864 | 15384/47479 · 15382/52151 | F/F | False | 2.11 | 189 | 0/779 | None/373/204331 |
| `irs-w9-2018-to-2024` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 1011 | 91 | 0 | 72346 | 0/34865 · 0/37481 | T/T | False | 2.76 | 303 | 1178/71 | None/34/1852 |
| `nist-sha-1803-to-1804` | no | tentative_candidates, unresolved_regions, coverage_gap | 6 | 1018 | 155 | 0 | 79648 | 5924/43483 · 5922/48011 | T/T | False | 5.50 | 305 | 327/895 | None/329/65991 |
| `arxiv-faster-rcnn-v1-to-v3` | no | extraction_incomplete, extraction_issues, tentative_candidates, unresolved_regions, coverage_gap | 20 | 34 | 9 | 0 | 94332 | 1181/37670 · 1218/59061 | F/F | False | 0.88 | 148 | 0/699 | None/123/5016 |
| `bunka-official-writing-1952-to-2022` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 2726 | 814 | 0 | 73085 | 0/5709 · 0/67376 | T/T | False | 11.10 | 219 | 1316/705 | None/1/0 |
| `arxiv-bert-v1-to-v2` | no | tentative_candidates, proven_changed_regions, unresolved_regions, coverage_gap | 0 | 723 | 228 | 1 | 104409 | 6133/53808 · 6133/62867 | T/T | False | 3.70 | 562 | 503/334 | None/316/203903 |
| `edpb-restrictions-v1-to-final` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 1234 | 110 | 0 | 110262 | 0/53297 · 0/56965 | T/T | False | 12.81 | 351 | 176/26 | None/1/0 |
| `arxiv-mask-rcnn-v1-to-v3` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 1776 | 186 | 0 | 105917 | 0/45920 · 0/59997 | T/T | False | 4.05 | 354 | 1699/442 | None/251/73647 |
| `mhlw-care-skills-original-to-revised` | no | extraction_incomplete, extraction_issues, tentative_candidates, unresolved_regions, coverage_gap | 0 | 265 | 29 | 0 | 182042 | 0/82214 · 0/99828 | F/F | False | 4.16 | 607 | 6761/2659 | None/265/2702 |
| `edpb-design-default-v1-to-v2` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 1755 | 460 | 0 | 175886 | 0/81344 · 0/94542 | T/T | False | 12.52 | 543 | 270/28 | None/1/0 |
| `nist-ai-rmf-draft2-to-v1` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 765 | 9 | 0 | 191518 | 0/86069 · 0/105449 | T/T | False | 8.36 | 501 | 127/250 | None/1/0 |
| `nist-ssdf-draft-to-final` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 4108 | 1154 | 0 | 197803 | 0/92379 · 0/105424 | T/T | False | 13.36 | 977 | 283/1445 | None/494/22152 |
| `mext-lower-secondary-japanese-2008-to-2017` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 230 | 12 | 0 | 269666 | 0/81110 · 0/188556 | T/T | False | 36.02 | 735 | 4819/1369 | None/13/0 |
| `nasa-buckling-8007-1968-to-2020` | no | extraction_incomplete, extraction_issues, unresolved_regions, coverage_gap | 0 | 1 | 0 | 0 | 280418 | 0/0 · 0/280418 | T/F | False | 3.04 | 288 | 386/430 | None/1/0 |
| `edpb-social-targeting-v1-to-v2` | no | tentative_candidates, unresolved_regions, coverage_gap | 2 | 896 | 98 | 0 | 275359 | 5151/139608 · 5150/146052 | T/T | False | 7.82 | 923 | 0/146 | None/182/200446 |
| `mext-primary-japanese-2008-to-2017` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 970 | 1 | 0 | 339845 | 0/103247 · 0/236598 | T/T | False | 10.13 | 1128 | 6047/1633 | None/13/0 |
| `edpb-controller-processor-v1-to-v2-1` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 3160 | 150 | 0 | 328029 | 0/153416 · 0/174613 | T/T | False | 28.86 | 1319 | 672/85 | None/1/0 |
| `nist-risk-assessment-30-to-r1` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 4281 | 1350 | 0 | 319563 | 0/1164 · 0/318399 | T/T | False | 8.29 | 996 | 0/995 | None/1/0 |
| `nist-incident-handling-r2-to-r3` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 5244 | 1853 | 0 | 331540 | 28/226696 · 28/104900 | T/T | False | 34.74 | 1537 | 1733/180 | None/58/73472 |
| `mext-upper-secondary-japanese-2009-to-2018` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 1847 | 80 | 0 | 408149 | 0/111408 · 0/296741 | T/T | False | 12.01 | 1891 | 7499/1162 | None/1/0 |
| `nist-contingency-34-to-r1` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 5755 | 1791 | 0 | 388770 | 0/1271 · 0/387499 | T/T | False | 23.55 | 1377 | 402/1075 | None/1/0 |
| `arxiv-llama2-v1-to-v2` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 3014 | 147 | 0 | 453769 | 0/226604 · 0/227165 | T/T | False | 23.75 | 1886 | 0/1806 | None/1248/1015505 |
| `arxiv-gpt3-v1-to-v4` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 2725 | 226 | 0 | 462587 | 0/229015 · 0/233572 | T/T | False | 17.30 | 2066 | 0/2095 | None/1653/1003075 |
| `nist-authentication-63b-to-63b4` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 6040 | 1921 | 0 | 482119 | 197/198540 · 197/283973 | T/T | False | 82.54 | 1844 | 537/702 | None/7/0 |
| `nist-risk-management-37-r1-to-r2` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 9688 | 3669 | 0 | 970260 | 0/375853 · 0/594407 | T/T | False | 29.35 | 5338 | 0/2257 | None/169/51205 |
| `ecb-annual-2023-to-2024` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 15988 | 4416 | 0 | 935116 | 0/461973 · 0/473143 | T/T | False | 30.08 | 4858 | 11020/1032 | None/616/439938 |
| `ipcc-synthesis-ar5-to-ar6` | no | extraction_incomplete, extraction_issues, tentative_candidates, unresolved_regions, coverage_gap | 0 | 22 | 2 | 0 | 1255728 | 0/615835 · 0/639893 | T/F | False | 31.82 | 2695 | 4445/10188 | None/22/180 |
| `nist-controls-53-r4-to-r5` | no | tentative_candidates, unresolved_regions, coverage_gap | 0 | 904 | 273 | 0 | 3067013 | 0/1514313 · 0/1552700 | T/T | False | 105.30 | 12414 | 3928/6373 | None/877/1010119 |

## Cause ranking (evidence-weighted)

1. **Forced-window all-or-nothing exclusion collapses alignment on eleven
   two-sided pairs (zero candidate visits, one to thirteen emitted spans).** `layout/region.rs::classify_reading_order` marks lines
   outside the longest render-monotone trusted run uncertain (or all lines of a
   partition whose inter-region order is unproven); `pipeline.rs` (`prepare`,
   lines around 1563-1588) turns any block touching an uncertain line into an
   uncertain block index; `alignment/ordered.rs::plan_ordered_gaps` forces every
   enclosing anchor window unresolved and puts its blocks into `excluded_old` /
   `excluded_new`; `pipeline.rs:898-902` then builds the new-side candidate
   index from non-excluded blocks only. When uncertain blocks are spread across
   pages or anchors are sparse, every window is forced and no comparison runs.
   - Zero candidate visits (`indexed_features <= 6`, `candidate_visits == 0`, 1-13
    emitted spans; the largest of these have 13 spans, so the collapse is the
    candidate index, not literally a single window):
     `bunka-official-writing-1952-to-2022`, `edpb-controller-processor-v1-to-v2-1`, `edpb-design-default-v1-to-v2`, `edpb-restrictions-v1-to-final`, `mext-lower-secondary-japanese-2008-to-2017`, `mext-primary-japanese-2008-to-2017`, `mext-upper-secondary-japanese-2009-to-2018`, `nist-ai-rmf-draft2-to-v1`, `nist-authentication-63b-to-63b4`, `nist-contingency-34-to-r1`, `nist-risk-assessment-30-to-r1`.
   - Near-zero (`candidate_visits <= 4000`): `ipcc-synthesis-ar5-to-ar6`, `irs-1099-misc-2024-to-2025`, `irs-w9-2018-to-2024`.
   - Partial forced-window exclusion still reaches real alignment:
     `arxiv-bert-v1-to-v2`, `arxiv-ddpm-v1-to-v2`, `arxiv-faster-rcnn-v1-to-v3`, `arxiv-gpt3-v1-to-v4`, `arxiv-llama2-v1-to-v2`, `arxiv-mask-rcnn-v1-to-v3`, `ecb-annual-2023-to-2024`, `edpb-social-targeting-v1-to-v2`, `irs-schedule-c-2024-to-2025`, `irs-w2-2024-to-2025`, `irs-w4-english-2024-to-2025`, `mhlw-care-skills-original-to-revised`, `nist-controls-53-r4-to-r5`, `nist-incident-handling-r2-to-r3`, `nist-risk-management-37-r1-to-r2`, `nist-sha-1803-to-1804`, `nist-ssdf-draft-to-final`; `irs-schedule-c` is the nearest
     incomplete pair (19 unresolved, 4 candidates, 301-token gap) with 42/41
     untrusted-in-known-order lines and no unproven-inter-region or
     render-disorder lines.
2. **Assessment work exhaustion.** `faa-maintenance-records-c-to-d` has a
   proven-empty old side (0/0 tokens) and a fully extracted new side, yet 478 of
   481 insertion candidates are `search: incomplete` with `work_used ==
   work_limit == 32000000`; the one-sided settlement does not scale to 63k new
   tokens. Among the 33 incomplete pairs, 25 exhaust the full 32M assessment
   budget (including `irs-w4-english` with 15 incomplete-search candidates);
   the completed controls `irs-schedule-se` and `faa-thunderstorms` also spend
   the full 32M while proving no hidden obligation, so budget exhaustion alone
   is not a failure.
3. **Extraction defects block five pairs.** `nasa-buckling` (one unmapped font
   code on page 85 of the new side; old side is empty), `arxiv-ddpm` (2
   glyph/clipping uncertainties), `arxiv-faster-rcnn` (2 unmapped codes, 2
   curved clipping paths), `ipcc-synthesis` (1 curved clipping path),
   `mhlw-care-skills` (13 malformed FlateDecode streams, 2 curved clipping
   paths).
4. **Exact/domain-closure residue after a match exists.** `irs-schedule-c`'s
   4 tentative replacements carry `unknown_reading_order`, `domain_not_closed`
   or `ambiguous_edit_location`; `irs-w4-english` has 95 same-text and 55
   different-text unresolved regions.

Labels are occurrence evidence, not root-cause proof. The trace-classified
split above separates "no comparison ran at all" from "off-chain lines poison
comparable windows" and from "assessment could not close a located
replacement".

## Recommended first hypothesis

**A forced reading-order window is excluded from candidate indexing and
matching as a whole, so trusted-run-only blocks inside it are never compared.**
Today a single uncertain block (for example one unorderable header/footer line
per page, or one partition whose inter-region order is unproven) forces the
entire enclosing anchor window unresolved. The window's blocks are added to
`excluded_old` and `excluded_new`; the candidate index for the new side is
built from non-excluded blocks only, so when the uncertain blocks are spread
out, every window is forced and `candidate_visits` collapses to zero. The
measured effect is eleven two-sided pairs with near-zero alignment and a
single to thirteen emitted alignment spans, despite complete extraction and
full token inventories.

Predicted beneficiaries (first-order, zero-candidate collapse):
`bunka-official-writing-1952-to-2022`, `edpb-controller-processor-v1-to-v2-1`, `edpb-design-default-v1-to-v2`, `edpb-restrictions-v1-to-final`, `mext-lower-secondary-japanese-2008-to-2017`, `mext-primary-japanese-2008-to-2017`, `mext-upper-secondary-japanese-2009-to-2018`, `nist-ai-rmf-draft2-to-v1`, `nist-authentication-63b-to-63b4`, `nist-contingency-34-to-r1`, `nist-risk-assessment-30-to-r1`.

Falsifiable test (pipeline fixture with programmatically built
`Document<Glyph>`):
- Positive: a page with two regions whose inter-region order is unproven, each
  containing a multi-line trusted run, where old and new agree exactly on one
  run and differ in the other. The hypothesis predicts that today the report
  shows one unresolved span with zero candidate visits, and that after
  splitting forced windows at their uncertain blocks the agreed run is matched
  while only the genuinely uncertain content stays unresolved.
- Negative 1: the same page with repeated identical text at two positions must
  stay ambiguous; no arbitrary optimum may be chosen.
- Negative 2: a single-region page with render disorder / interleaved runs must
  stay fully uncertain; the existing barrier and singleton fixtures stay green.

Falsification: if the positive fixture already matches the agreed run with the
current binary, the mechanism is misattributed. If candidate indexing recovers
but resolved-token coverage does not rise on the named pairs, the hypothesis is
incomplete and cause 2 (assessment work exhaustion) or cause 3 (extraction)
owns the residual.

## Limits

Traces are default-budget diagnostics on the frozen inputs and the captured
binary; all 36 are represented. They are not part of the completion metric.
The three historical controls are the only completions claimed.
