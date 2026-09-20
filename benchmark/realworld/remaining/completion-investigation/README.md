# Common-text completion investigation

> **Current status.** The strict native route confirms at least **3/36** pairs
> complete on the final build (`0db25404`): Schedule SE was already complete,
> and two pairs from independent producers were completed by the proven-empty
> native side, each captured twice with identical reports. The full 36-pair
> panel was not re-captured on that build. See
> [the final record](native-completion-final-v1.md) and its artifacts under
> `benchmark/realworld/cache/completion-investigation/native-completion-final-v1/`
> and `.../native-completion-holdout-v1/`. The 0/36 and 1/36 figures in the
> chronological sections below are historical measurements of the common-text
> route and of earlier native states; they remain accurate for those phases and
> are not the current result.

## Primary metric corrected to PDF-native text

The primary completion metric is the strict PDF-native text route
(`--native-text-only`). Image pixels, path lettering and non-painting text
(for example render mode 3) are outside the visible-content scope, while a
visible text layer, including an existing OCR layer, is compared; native text
drawn inside figures and Form XObjects stays in scope, and unsupported or
unmapped native text remains incomplete.
[The scope-correction investigation](native-text-scope.md) replays the frozen
36 pairs through the native predicate on the registered baseline build and the
current worktree: both captures complete **0/36** (baseline 35 incomplete plus
one memory-limit failure, current 36 incomplete), with 85,959 / 86,781
unresolved native regions and five extraction-incomplete pairs. A v2 re-audit
binds every captured row to the frozen panel, its runs record, input hashes,
binary hash and report hashes, and drives the production reader and collector
through 28 injected-defect controls; it reproduces the same counts with
per-source memory conditions recorded. The document-wide common-text results
below remain historical evidence for the wider contract, which still includes
the non-text painting condition and is unchanged.

[The native line-reconstruction correction](native-line-reconstruction.md)
fixes the report reader for real serde-pretty empty containers and stops line
formation from interleaving independent overlapping runs at very different
font sizes. Measured once on Schedule SE and C, every established change is
preserved, C coverage rises, SE unresolved regions fall, and strict completion
stays 0/36. Evidence:
`benchmark/realworld/cache/completion-investigation/native-line-reconstruction-v1/`.

[The trusted-run diagnosis](../../cache/completion-investigation/native-trusted-runs-v1/native-trusted-runs.md) traces every remaining SE/C
`reading_order_unknown` region to the render-monotone chain and shows that the
existing row-cluster proof would order the full regions once its active-row
bound is relaxed, but that converting those lines to an inferred order removes
the partial-uncertainty barrier contract. It is diagnostic-only; the next step
is a contract decision before any code change. Evidence:
`benchmark/realworld/cache/completion-investigation/native-trusted-runs-v1/`.

[The singleton local-comparison fix](native-trusted-runs-v4.md)
supersedes that diagnosis: it corrects the mixed-build binding and the
active-row count, fixes the concrete rejection that dropped horizontal
source-bounded single-line blocks from the existing local anchor path, and
bounds the closure by exact per-token source position equality and a shared
source page so moved or cross-page singletons keep their move or order
obligation. Schedule SE unresolved regions fall from 19 to 17 and Schedule C
from 116 to 84 with every established change preserved and no new strict
completion (0/36). Evidence:
`benchmark/realworld/cache/completion-investigation/native-trusted-runs-v4/`;
the v1 diagnosis is preserved in the native-trusted-runs-v1 artifact, the
superseded v2 and v3 records are preserved in
`benchmark/realworld/cache/completion-investigation/cleanup-archive-v1/`, and
their measurements are superseded.

[The short-view candidate check](native-short-views.md) tests whether the seed
width (16 tokens) hides complete source-bounded singletons. The gap is real for
one SE label, but its candidate is rejected by the committed position/page
guard (the label moved 0.5 pt) and no SE/C measurement changes, so no
production change is adopted. The report lists the evidence each remaining
residual still needs, starting with a relative-position proof against
surrounding source-backed unique anchors. Evidence:
`benchmark/realworld/cache/completion-investigation/native-short-views-v1/`.

[The established-coverage fix](native-established-coverage.md) accepts
established, search-complete, source-bounded equal local domains into coverage
instead of dropping them for having no edit script, while veto-only
trusted-run fragments keep their obligations. Schedule SE unresolved regions
fall from 17 to 14 and Schedule C from 84 to 78 with every established change
preserved and no new strict completion (0/36). Evidence:
`benchmark/realworld/cache/completion-investigation/native-established-coverage-v1/`.

The change recovers seven inferred local changes, finishes 20 previously
unfinished matching components, and adds 23 strictly compared source references
on each side. **Document-wide completion remains 0/36: escaping zero is not
achieved.** Existing strict comparison objects and source-backed reviews are
preserved across the panel.

The user subsequently prioritized increasing this strict completion count.
[The acquisition follow-up](strict-priority.md) records the next investigation,
including the bounded declaration-worklist correction and its 72-input replay.
Practical quality measurement remains secondary and does not replace this goal.

[The subsequent cardinality search](cardinality-search.md) finishes two more
retained conflict components, reducing the remaining count from 11 to nine.
Its full 36-pair replay preserves all strict coverage and comparison claims.
Strict completion is still **0/36**, with incomplete inventories on all 72
inputs; this solver result is not counted as strict-completion progress.

The fixed denominator remains the original 36 natural revision pairs. A report
being captured, a useful change being found, and complete document comparison
are three different outcomes. None may be substituted for another.

## Reproduction

The initial executable was built from `a3c8b08` with no production modifications:

```sh
cargo build --release --locked -p pdfdelta-cli
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python \
  benchmark/realworld/remaining/completion-investigation/capture.py \
  target/release/pdfdelta \
  benchmark/realworld/cache/completion-investigation/baseline
```

The driver preserves the frozen input hashes, text route, limit scale 1 and
180-second outer timeout. Each capture directory contains a copied executable,
the production diff, command records, report hashes and a diagnostic summary.
Missing and failed captures remain in the denominator. Output directories must
be new; the driver never overwrites an earlier capture.

The current baseline captured all 36 reports successfully and reproduced **0/36
complete**. All 36 have incomplete inventory and uncompared sources. Across the
panel, 452,081/5,574,665 old references and 452,398/7,224,775 new references are
strictly compared. There is one typed change, zero inferred local changes, and
468 source-backed range reviews. These categories do not share a recall
denominator. Thirteen pairs contain 31 unfinished matching components; 22 of
those components have between one and 24 grouped proposals. That is an
applicability screen, not proof that the hybrid can safely solve all 22.
`baseline.json` binds the executable, inputs, raw reports and per-pair figures.

## Three independent blockers

1. **Acquisition.** `EvidenceStore::from_native` leaves a page's text inventory
   incomplete when non-text painting remains. It cannot establish whether an
   image or path contains additional text. A successful native glyph extraction
   does not discharge this obligation. Replacing the parser or increasing
   alignment search alone cannot make this inventory complete.
2. **Source interpretation and coverage.** `document_coverage` accepts only
   source-backed comparisons and validated native domains/intervals. Inferred
   correspondences retain uncertainty even when their character diff is exact.
   Solving a similarity assignment does not establish paragraph identity or
   convert recognition into source evidence.
3. **Correspondence search.** Independent one-to-one leaf matches already use
   exact bipartite assignment. Mixing grouped candidates into the same component
   can route the entire component to subset search, which stops before visiting
   a state when more than 24 residual proposals remain. This loses useful local
   comparisons independently of the first two blockers.

The third blocker is directly reproduced on the current Schedule SE pair:
121 one-to-one proposals and 22 grouped proposals form one 143-proposal
component. Production visits zero states and selects no mandatory proposal.
The separate `endpoint_oracle.py` uses dynamic programming over consumed
endpoints, not Hungarian assignment or proposal-subset enumeration. It finds
an optimum weight of 8,301,289 and seven proposals shared by every optimum in
13,824 states. This is conditional on independent endpoint ownership; it does
not prove semantic correspondence. See `baseline-se-oracle.json`.

## Existing and external alternatives

| Approach | What it establishes | Implication here |
| --- | --- | --- |
| Existing `--native-text-only` adapter | Exact native-text changes under its narrower contract, with explicit unresolved regions | Reuse and benchmark it; do not describe its results as common-text completion. |
| [JoshData/pdf-diff](https://github.com/JoshData/pdf-diff) | Serializes the Poppler text layer, applies a global text diff, then maps changes to word boxes | A useful practical baseline for reflow. Its line-end hyphen handling and word boxes do not establish source-exact character masks. |
| [mli55/pdfdelta](https://github.com/mli55/pdfdelta), an unrelated namesake | Extracts words with PyMuPDF, performs a global sequence diff and filters movement/reflow | Compare the simpler global sequence approach on partial gold; do not assume its noise filters preserve every literal change. |
| [SemanticDiff](https://github.com/Labic-ICMC-USP/SemanticDiff) | Reflow-oriented text comparison with optional LLM review | The extraction/alignment approach is relevant; semantic filtering is unsuitable for proving literal unchanged text. |
| [PyMuPDF OCR](https://pymupdf.readthedocs.io/en/latest/recipes-ocr.html) | Recognizes rendered text, including pages requiring OCR | Improves access to scanned content while retaining recognition uncertainty; not an exhaustive source-interpretation certificate. |
| [OR-Tools assignment examples](https://developers.google.com/optimization/assignment/assignment_groups) | Formulates group constraints separately from ordinary assignment | Motivates a bounded grouped-choice/assignment hybrid; no new optimization dependency is needed for the measured small grouped components. |

These external descriptions were inspected as primary documentation/source.
The external tools were not installed or executed in this investigation, so
there is no claim that they outperform this project on the fixed panel.

A five-pair replay of the existing native adapter produced the following
diagnostic results. Counts are the adapter's output, not newly adjudicated
precision or recall, and token coverage is not the common-text source metric.

| Pair | Reported changes | Native comparison coverage | Complete |
| --- | ---: | ---: | --- |
| Schedule SE | 8 | 91.48% | No |
| Schedule C | 7 | 74.05% | No |
| W-4 | 15 | 30.92% | No |
| BERT | 0 | 9.77% | No |
| SSDF | 0 | 0.00% | No |

The raw reports and input-bound commands are under
`benchmark/realworld/cache/completion-investigation/native-pilot/`.
This rejects a blanket switch back to the native adapter as a demonstrated
solution: it recovers useful form changes but also has substantial residuals.

## Implementation

Enumerate compatible grouped choices only for small grouped populations, solve
each remaining independent one-to-one assignment exactly, and intersect the
mandatory edges across all optimal choices. Preserve the lexicographic objective,
source-only protection, source ownership, partition constraints and existing
state/work limits. Unsupported ownership shapes retain the existing solver.
An unfinished optimization must not certify its best-so-far solution.

This can restore local inferred comparisons. It cannot, by itself, discharge
the acquisition and strict source obligations above. Completion predicates,
uncertainty classifications and the five release acceptance cases remain intact.

The mixed solver is implemented in `document/matching/hybrid.rs`. The first
review exposed an applicability problem: the original ownership index also
disqualifies singleton proposals that share glyphs with a grouped proposal.
The implementation retains the original leaf shape separately and checks the
complete conflict graph before using assignment for the singleton remainder.
Non-endpoint conflicts still reject that fast path. A second review corrected
work accounting when eligibility validation falls back to subset search.

On Schedule SE, the 143-proposal component now finishes in 104 grouped-choice
states and 136,579 charged assignment work units, within the existing limits.
All seven mandatory proposals match the independent endpoint oracle. Seven
inferred local changes are emitted, including the farm-income thresholds and
printed years. Strict compared references remain exactly 4,348 old / 4,363 new;
the inventories remain incomplete. `after-se-oracle.json` binds the result.

`se-mask-audit.json` checks the seven reported readings against the existing,
independent all-optimal literal-mask oracle and its 225 tiny exhaustive controls.
Every mask and source-change bound agrees. This checks the reported strings and
positions, not independent PDF transcription or semantic correspondence. The
attachment/year node still contains interleaved layout text; that pre-existing
reading-order defect is not fixed by the assignment change.

The workspace test initially failed an assumption that an inferred fixture's
search must remain unfinished. Its source-domain rejection still passed. The
test now directly verifies zero strict compared sources, nonempty residual
sources and incomplete coverage despite solved inferred correspondences, while
preserving the other ten closure/adversarial cases. Production completeness
predicates were not changed to make the test pass.

## Historical native progress: Schedule SE complete at the time (at least 1/36)

The exact-displacement line proof (`native-exact-displacement-commit-v1`) makes
Schedule SE complete twice on the same frozen binary, inputs, arguments and
budgets: `comparison_complete: true`, 12 changes, no candidates and no
unresolved regions, 5,485/5,485 and 5,502/5,502 resolved tokens, with the year
change and the appended date bound as exact ranges. The repeat is identical
apart from timestamps. This is at least 1/36 on the strict native predicate;
the increment's acceptance minimum of two distinct complete pairs is not met.

A six-pair follow-up captured once each with the same binary and budgets
(`native-exact-displacement-followup-v1`) leaves all six incomplete and finds
no `exact_text_displacement` proof outside Schedule SE. The dominant unresolved
reason is `domain_not_closed`, usually paired with `unknown_reading_order`;
`work_limit` and `ambiguous_edit_location` follow on W-4 and BERT. Next
candidates: close the parent domains of the residual lines in W-4 and BERT and
reduce work-limit exhaustion.

## Stationary-member result: Schedule C unresolved regions 48 to 27

The stationary-member proof (`19e3b31`) adds a second native capability next to
the exact-displacement line proof: a whole original block that sits still
inside a trusted run is proven by an independently established stationary
neighbour correspondence, behind a reject-only candidate mask that never
shrinks view populations, reference sets or occurrence competition. On the
frozen IRS Schedule C pair (`native-stationary-member-v3`) the unresolved
regions drop from 48 to 27 (10 changes, 4 candidates,
`comparison_complete: false`) with 6,262/6,774 old and 6,277/6,780 new
resolved tokens, inside the shared work limit; 28 relation records carry the
`stationary_neighbour` assumption over 14 unique stationary ranges. Schedule SE
stays complete and unchanged (12 changes, no candidates, no unresolved
regions, 5,485/5,485 and 5,502/5,502 resolved tokens). The repeat capture is
byte-identical to v3 for both pairs.

The proven-changed-region count drops from 1 to 0 because the old
`exact_token_multiset_mismatch` proof is not reused as a residual once part of
its whole domain is confirmed (`entirely_unresolved`); block 133 stays
unresolved and the reduction is not counted as a resolved change. Next
residual blocks: 4, 74, 77, 94, 98, 103, 106, 126, 129, 133, 141, 150, 228.
Strict completion remains at least 1/36; the increment minimum of two newly
complete pairs is still unmet.

## Verified scalar positions: Schedule C unresolved regions 19

The deny-only scalar position evidence (`a023c37`) lets the positioned search
exclude a competitor occurrence when one strictly verified scalar position
provably differs, without filling the whole per-token metadata or relaxing any
candidate, reference or occurrence rule. On the frozen Schedule C pair the
unresolved regions drop from 27 to 19 (10 changes, 4 candidates,
`comparison_complete: false`), resolved tokens 6,272/6,774 old and 6,287/6,780
new, with 32 `stationary_neighbour` relation records and two added established
spans, blocks 74..77 (50..52) and 103..106 (50..53). Schedule SE stays complete
and unchanged (12 changes, no candidates, no unresolved regions, 5,485/5,485
and 5,502/5,502 resolved tokens). The repeat capture is byte-identical.

The implementation charges every source scan and allocation bound before the
scans, validates ordered in-bounds source maps, raw scalar offsets and
single-glyph uniqueness, and holds the whole build on inconsistent ranges or a
mid-budget cut. Strict completion remains at least 1/36.

## Full-panel result

Both captures successfully produced all 36 reports with the same frozen inputs,
route and limits. `panel-audit.json` verifies report hashes, input revisions,
source conservation and the unchanged completion predicate against the raw
reports. It also checks retention of every previous strict comparison and
source-backed range review.

| Measure | Before | After |
| --- | ---: | ---: |
| Complete document comparisons | 0/36 | 0/36 |
| Unfinished matching components | 31 | 11 |
| Pairs with unfinished matching components | 13 | 9 |
| Pairs with all comparison-search obligations resolved | 1 | 2 |
| Inferred local changes | 0 | 7 |
| Source-backed node/group comparisons | 9,228 | 9,236 |
| Typed changes | 1 | 1 |
| Source-backed range change reviews | 468 | 468 |
| Strictly compared old references | 452,081 | 452,104 |
| Strictly compared new references | 452,398 | 452,421 |
| Lost prior strict comparison or source-backed review objects | — | 0 |

The source gain comes from eight unchanged node/group comparisons in the MHLW
care-skills pair. All other pairs have byte-identical coverage objects. The
additional findings in Schedule SE retain their inferred classification. A
finished matching component can still have ambiguous alternatives or unfinished
candidate discovery, so component completion is not document-search completion.

`mhlw-source-audit.json` binds the eight additional comparisons to a fresh native
graph reconstruction. Their literal readings match, all 23 references per side
are native and distinct, and an independent unit-weight endpoint-matching oracle
agrees on the mandatory proposals in all four affected components. These are
`conditional_on_correspondence` comparisons under the existing source-coverage
contract, not unconditional proofs of correspondence. The graph reconstruction
uses the production parser/layout code; it is not independent visual
transcription. The oracle does not independently reconstruct every rival's
physical-source conflicts or omitted-candidate locality.

Nine pairs have changed comparison payloads. The remaining unfinished components
include large grouped populations (up to 7,752 groups), unsupported ownership
shapes, and one ECB component that exhausts its 32,000,000-unit assignment budget.
The latter remains unresolved; its best-so-far solution is not certified. Raising
the group limit alone is not justified by these observations.

Capture wall times total 647.63 seconds before and 632.16 seconds after, with
maxima of 94.35 and 95.76 seconds. These single captures ran alongside other
local work; they are operational observations, not a controlled speed benchmark.

To reproduce the comparison after building the current source:

```sh
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python \
  benchmark/realworld/remaining/completion-investigation/capture.py \
  target/release/pdfdelta \
  benchmark/realworld/cache/completion-investigation/new-replay
PYTHONDONTWRITEBYTECODE=1 PYTHON_UV=0 python \
  benchmark/realworld/remaining/completion-investigation/compare_captures.py \
  benchmark/realworld/remaining/completion-investigation/baseline.json \
  benchmark/realworld/cache/completion-investigation/new-replay/summary.json \
  benchmark/realworld/cache/completion-investigation/new-replay/audit.json
```

## Validation and remaining work

The exact mixed objective and mandatory-edge intersection have exhaustive small
controls, 512 generated conflict fixtures, ties, priority conflicts, budget
exhaustion, unsupported shapes and the measured Schedule SE matrix. The separate
endpoint oracle agrees on the seven mandatory proposals in the natural case.

Final checks pass: formatting, Clippy with all workspace targets/features and
warnings denied, 2,604 workspace tests, 1,504 all-feature library tests, and
private-item documentation with warnings denied. Both test runs retain two
intentionally ignored tests. Generated fixture verification passes 48/48 cells;
42 satisfy strict author-intent expectations and six satisfy their declared
candidate policy. These six are not reclassified as exact author-intent recovery.
`checks.json` binds check logs and source provenance. The full-panel executable
was frozen before the fixture assertion correction and documentation updates;
comparison implementation files have no subsequent behavior changes.
The final review also corrected the no-leaves negative test to supply otherwise
valid groups, then reran all seven hybrid tests and core Clippy successfully.

Further matching work can reduce source and search residuals, but **all 36 pairs
still fail acquisition completeness independently**. The current provider leaves
possible text in image/path painting unresolved. A next strict-completion
experiment must first demonstrate a general, source-bound acquisition proof on
an actual residual and preserve the supported/unsupported distinction. Installing
OCR or choosing an arbitrary optimum does not supply that proof. The existing
provider interface currently has no demonstrated proof that closes these natural
page inventories; this is a concrete missing capability, not a claim that every
future proof is impossible.

For practical comparison quality, the most useful next experiment is to run the
global native-text alignment used by the simpler external projects against the
existing finite gold ranges, measuring false changes and recovered changes on
the same sources. A useful result must retain raw glyph provenance, ambiguous
reading orders and unknown image/path text. That experiment should remain
separate from the strict completion count; neither an external baseline nor a
new completion definition has been substituted in this change.
