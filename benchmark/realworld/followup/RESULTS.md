# Stopped with an unmet completion constraint

The goal was not achieved. Execution stopped under the approved constraint-stop
condition before changing production code. The first registration unit is commit
`f51c718`; the production baseline remains `9093cab`.

## Observed results

| Gate | Observation | Required |
| --- | --- | --- |
| G1 | No additional development recovery demonstrated | Six natural prose pairs, three independent producers |
| G2 | No fresh blind set selected; development gates did not pass | Twelve new pairs; recovery on three pairs, two producers |
| G3 | Baseline common-text completion is 0/36 in both repetitions; 35 pairs have a native inventory blocker | Two additional complete pairs, no losses, same contract |
| G4 | No production changes; baseline common-text strict masks claim zero atoms in the registered controls | Preserve correctness, strict masks and acceptance cases |
| G5 | Source registration and blocking evidence retained; final recovery evidence is absent | Complete per-family, blind, source-review and quality evidence |

The panel retains all 24 historical development pairs and all 12 exposed former
blind pairs. There are 28 prose candidates, of which 27 have complete historical
source resolution, spanning 13 independent producers. The 60 generated controls
and three real-source metamorphic controls remain bound to their historical hashes.
These registrations are not new recovery results.

## Why G3 cannot pass under this execution contract

The native Text inventory contract explicitly rejects pages with non-text paint:
glyph extraction cannot classify text in images or drawn paths. This is both
documented and regression-tested, including straight lines, rectangles, images
and shading. Complete source comparison cannot compensate for an incomplete
inventory. The renderer supplies pixels, not recognized text, and does not fill
this gap. The existing recognized-text route requires an OCR backend, which this
execution prohibits.

All 72 panel inputs were probed with the frozen executable's native acquisition
worker, using the same content request and 35-second acquisition timeout as the
common-text route. Thirty-five distinct pairs have at least one successfully
acquired side with recorded non-text paint. Both probes for the remaining pair,
`nist-controls-53-r4-to-r5`, failed at the response boundary. Even granting that
remaining pair completion leaves an upper bound of one, below the required two.
Failures remain in the denominator.

The two small IRS Schedule C/SE pairs provide a concrete example: both pages in
both revisions contain non-text paint, and each page's native Text inventory is
explicitly incomplete despite successful glyph extraction. Removing the paint
guard, replacing the fixed panel, or counting opaque paint as recognized text
would change the preserved contract or denominator. No such change was made.

This conclusion concerns the fixed panel, existing evidence contract and permitted
acquisition routes. It does not establish that every conceivable future non-OCR
proof system is impossible. Such a system would need a separately justified Text
inventory contract before it could satisfy a revised execution goal.

## Retained evidence and replay

- `baseline-observations.json`: two hash-bound common-text attempts per pair,
  including process failures. First attempts reuse the identical retained binary,
  input bytes, flags and budgets; second attempts are fresh full-panel captures.
- `baseline-target-scores.json`: zero baseline B source-range hits and zero
  registered-control atoms claimed by common-text strict masks. Range hits require
  both finite cores and prohibit extra context; they still require adjudication.
- `baseline-diagnostics.json`: stage counters, text issues, coverage, wall time,
  peak RSS and report sizes for every repeated attempt. These are observations,
  not completed per-target root-cause assignments.
- `baseline-family-metrics.json`: all six families, both repetitions, retained
  failures, wall time, process peak RSS and report bytes. No speedup is claimed.
- `inventory-probes.json`: the four small IRS responses, including explicit
  inventory flags and paint markers.
- `panel-inventory-probes.json`: all 72 acquisition attempts, request, input and
  response hashes, byte counts, exit codes and paint-page maps. Full responses
  remain in ignored `../cache/followup-inventory` (about 2.27 GB).
- `constraint-stop.json`: the preserved contract file hashes, 35 blocked pair IDs,
  remaining pair, stop time and unmet work.

To reproduce an acquisition probe, send the recorded JSON `request`, followed by
one newline and the exact PDF bytes, to the frozen binary with `acquire-native`.
Use the recorded 35-second timeout; the worker retains its 128-MiB response bound.
This diagnostic does not count as a comparison and changes no comparison budget.

`verify.py --stage registration` succeeds. `verify.py --stage final` checks the
retained hashes and paint observations, prints G1–G5 and exits nonzero. It does not
declare the task complete. Six checker tests cover missing/stale evidence,
duplicate counting, inferred-only and oversized ranges, scalar multiplicity,
nonempty complete coverage, independent producers and complete-pair regression.

## Quality checks

`quality-checks.json` retains commands, exit codes, wall time and log hashes.
Formatting and workspace clippy passed. Workspace tests passed 2,326 tests with
two ignored. Generated-fixture verification passed 48/48 renderer cells, including
all five required acceptance cases; its separate strict author-intent result
remains 42/48, with six candidate-policy cells. All six checker tests passed.
Passing these checks does not satisfy the blocked real-PDF gates.

## Unfinished work

C1 remains open because the full successful-final evaluator and its all-gates-pass
example were not finished. C2's per-target causal assignments and minimal
counterexamples were not completed. No production improvement, fresh blind freeze,
blind acquisition, or new recovery review bundle was produced. These omissions
are explicit consequences of the constraint stop, not passing or waived gates.
