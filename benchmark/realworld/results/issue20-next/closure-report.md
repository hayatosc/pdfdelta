# Issue 20 implementation and acceptance report

Completed: 2026-09-11 (Asia/Tokyo).
The implementation and local acceptance work in `ISSUE20_GOAL_PLAN.md` is
complete. The issue is ready for review against the
[requirement matrix](closure-matrix.md). No commit, publication, or external issue
closure was performed. This is a scoped acceptance result, not a claim of exact
comparison for every PDF or complete comparison of these real documents.

## Implementation

The common matcher now uses bounded exact partial assignment for validated
independent one-to-one components, with five-class integer priorities,
mandatory-edge exclusion checks, and separate source-only proof. Certified
priority prefixes survive residual search exhaustion without making the
remaining component complete.

Scoped key presence, absence witnesses, source-checked normalization, local
extraction dependencies, and reversible changed groups connect those choices to
source evidence. Native acquisition uses bounded versioned glyph transport.
Source-backed terminal views address the recorded footer blocker. These paths
use evidence contracts and relative geometry; expected PDF names, annotation
IDs, quotes, and changed ranges do not enter production comparison.

## Fixed corpus

The original inventory contains 29 pairs. NASA remains unavailable; all 28
available pairs completed all three scheduled routes: **84 terminal trials**.
All failures and partial reports remain in [the route records](candidate5-routes.json).
The 26 source-validation/scoring invocations completed successfully; this means
the evaluator ran, not that every selector or expectation was scoreable.

| Measurement | Frozen baseline | Candidate 5 |
| --- | --- | --- |
| Shared text exact whole events, fixed source-eligible denominator | 0/17 | 1/17 |
| Shared all-channel exact whole events, same denominator | 0/17 | 1/17 |
| Shared reviewed changed-source true positives, each route | 0 | 17 |
| Shared annotated unchanged-source false positives, each route | 0 | 0 |
| Shared reports, each route | 20/28 | 28/28 |
| Native compatible whole-event metric | 2/39 | 2/39 |
| Native compatible changed-token metric | 38/709 | 38/709 |
| Complete real-document reports, every route | 0 | 0 |

The recovered shared event is the complete first-page Form 1040 footer change.
The source correction and failure-inclusive eligibility contract were frozen
before this replay; [the contract](audited-source-contract.json) retains all 40
annotation entries, including failed selectors. The historical common 14-event
layout-source report subset remains 0/14. The corrected 17-event denominator
also includes source-eligible baseline process failures; it does not pretend
that those failures produced masks. Thirteen of fourteen declared scopes are
scoreable. The 709-token historical denominator is retained separately from
current observed masks and does not turn the unavailable scope into a measured
zero-error result.

Both native baseline successes remain: the SP 800-57 association punctuation
change and the Form 1040 footer. Neither shared route loses a baseline success.
No per-pair, per-side, per-channel compared-source count decreases. Supplemental
[source-ID retention](candidate5-source-retention.json) goes beyond counts: all
2,632 W-4 glyph IDs per side, all 39 PostgreSQL glyph IDs per side, and all 29
discovered W-4 field slots per side remain compared. These are every nonempty
baseline shared coverage row. The baseline executable was reconstructed from
its unchanged source revision after temporary-file loss; its relevant metrics
match the original records, but its binary hash differs. This supplemental
check does not replace the original baseline measurements. See
[reconstruction provenance](baseline-source-reconstruction.json).

| Route | Conditional / inferred operations, baseline → current | Compared sources old / new, baseline → current | Process seconds, baseline → current | Peak RSS KiB, baseline → current |
| --- | --- | --- | --- | --- |
| Shared text | 1 / 1 → 6 / 2 | 2,671 / 2,671 → 537,378 / 537,406 | 454.97 → 484.04 | 844,800 → 1,081,704 |
| Shared all channels | 1 / 5 → 6 / 6 | 2,700 / 2,700 → 537,635 / 537,663 | 414.15 → 479.45 | 844,800 → 1,089,176 |
| Native | Separate legacy report contract | Per-pair fractions in route records | 2,899.16 → 3,525.20 | 3,092,104 → 3,091,744 |

All-channel source totals combine distinct channel units; they are not glyph
accuracy or complete-document coverage. Per-channel inventories, uncertainty,
search completion, and process outcomes are retained in the route records.
Native GCC and Unicode allocation failures remain failures. These are single
serial replays, not a timing benchmark; no speedup is claimed. The
[comparison summary](candidate5-comparison-summary.json) and
[40-entry final blocker ledger](candidate5-blocker-ledger.json) retain the
remaining misses instead of inferring success from operational counts.

## Frozen unseen validation

Candidate 5 was frozen before selecting new series and reviewing source
annotations. Sixteen series were attempted; fourteen failed acquisition,
source-identifiability, or annotation-coordinate preconditions before comparison.
All remain in [the selection record](unseen7-selection.json).

The two eligible series, Form 8889 (2024–2025) and Form 2210 (2023–2024), pass
the predeclared scoped gate on both shared routes: **2/2 exact whole events,
20/20 changed source scalars, zero annotated false positives**. All eight shared
and four native identical-input control scopes have full reviewed-source
accounting and zero changes. Native revision operation counts remain separate;
they are not claimed as shared whole-event recall. Every document remains
incomplete outside the reviewed scopes. This same-publisher, two-series holdout
does not estimate general cross-publisher accuracy. See
[results](unseen7-results.json),
[shared controls](unseen7-control-source-accounting.json), and
[native controls](unseen7-native-control-source-accounting.json).

Earlier holdout failures remain development evidence. No production code was
changed after the final freeze or in response to experiment seven. The durable
replay used identical input/executable hashes and reproduced all four shared
revision evaluations. Timestamp-bearing shared report bytes need not match.

## Required checks and remaining work

Formatting, workspace Clippy with warnings denied, and 2,293 workspace tests
passed before the freeze. The final source-backed local-comparison test,
structure-claim probe, and generated verification also passed. All five core
release cases pass on both renderers. Generated verification is 48/48 policy
checks and 42/48 strict author-intent cells; the six ambiguous cells retain their
individual explanations. DSA retains 74 mandatory changes and 33 ambiguous
positions rather than selecting an arbitrary optimum. Commands and results are
in [the acceptance record](final-acceptance-checks.json).

The [linked follow-up drafts](follow-ups.md) cover unkeyed semantic identity,
bounded visibility observation with provenance, visual candidate cost, and
literal whitespace annotation coordinates. They do not waive any acceptance
gate or imply those capabilities were implemented. No parser replacement, OCR,
semantic model, or production Python dependency was added.

## Reproduction

The frozen binaries and downloaded inputs are in ignored workspace storage;
compact source records, annotations, images, hashes, and results are retained
here. [Recovery records](candidate5-recovery.json) disclose the interrupted
temporary run and exact executable recovery. The original manifest and expected
annotations are unchanged. This stable results directory retains the full
development history; the completion date above identifies the final checkpoint.

The replay used `run-route-trial.sh FROZEN_DIRECTORY CACHE PAIR ROUTE SCALE`,
serially over the original 84-task list with 4 GiB virtual-memory and 1,200-second
process limits. `score-source-trial.sh` ran the 24 layout validations and the two
frozen Form 1040 paint-order overrides, with 180-second limits. Executable
arguments are preserved in those scripts; source view is a benchmark-only
option. Completed outcomes were collected with:

```sh
sh benchmark/realworld/results/issue20-next/summarize-trials.sh \
  target/issue20/candidate5 target/issue20/candidate5/pdfbench
PYTHON_UV=0 python benchmark/realworld/results/issue20-next/curate-source-evaluation.py \
  target/issue20/candidate5 benchmark/realworld/results/issue20-next
PYTHON_UV=0 python benchmark/realworld/results/issue20-next/collect-final-corpus.py \
  benchmark/realworld/results/issue20-next
```

The retention check's full invocation and source-map contract are in
`check-source-retention.py`; its output records map hashes, input bindings,
baseline source IDs, and all checks. Reproduction requires regenerating ignored
raw reports and source maps; the retained summaries do not masquerade as those
raw inputs.
