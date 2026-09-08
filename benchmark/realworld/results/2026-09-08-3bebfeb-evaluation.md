# Common assessment and local-view recovery evaluation

The final 29-pair regression check preserves accepted-change counts and
resolved-token counts for every pair relative to common assessment alone.
Local recovery adds 86,283 resolved tokens in total. It still matches none
of the 40 listed exact changes, and six generated exact expectations remain
candidates. This is an implemented, tested change with unmet release
conditions, not a claim of release readiness or established generalization.

The evaluated implementation is a modified working tree based on
`3bebfeb77fd43d5eaf55f9554a47e3a6d421212b`; the base commit alone does not
identify this implementation. Source manifests, executable hashes, raw
results, commands, and environment information are retained in
[`2026-09-08-common-assessment`](2026-09-08-common-assessment/).

## Corpus and independence

The frozen manifest has 29 public revision pairs: 19 development pairs and
10 evaluation-only holdout pairs. Five previously targeted pairs were moved
to development, and the QGIS English/Japanese translations share one lineage
and split. [The provenance audit](../PROVENANCE.md) identifies the evidence
and remaining holdout series. Historical results retain their original split
labels; comparisons in this report use the corrected split for all methods.
Producer identities remain unknown where they have not been independently
established. This run therefore does not claim a producer-family holdout.

## Fixed generated cases

The 24-case, two-renderer matrix retains the original author-intent metrics:
42/48 cells meet strict expectations. Six cells retain a tentative candidate
and a proven changed region: `text-insertion`, `text-deletion`, and
`numbered-requirement-text-insertion` under both renderers. Adjacent spaces
admit different edit boundaries, so their author-intended ranges are not
promoted to uniquely located changes. The separate candidate contract passes
6/6 cells, giving 48/48 behavioral checks; this is not 48/48 exact acceptance.

All five mandatory initial acceptance cases pass under both renderers:
line wrapping and page breaking produce no content changes with complete
comparison, and replacement, paragraph insertion, and paragraph deletion
each produce one exact change. The generated event totals are 20/26 exact
matches with precision 1.000 and recall 0.769. Changed-token totals are
810 true positives, 0 false positives, and 48 false negatives, with recall
0.944. The common-assessment counterfactual has the same generated totals.

Six fixed development sensitivity scenarios evaluate 288 renderer cells.
Every scenario has 48/48 behavioral passes, 20 accepted changes, and 40/48
complete comparisons. The scenarios perturb line baseline distance, block
joining, matching scores, score margin, and candidate budget. No holdout
result selects these options. These small fixtures do not establish
insensitivity across all real document series.

## Evidence for structural recovery

The interleaved-rendering fixture evaluates the same source-backed
replacement with and without local-domain discovery. Common assessment alone
cannot establish the relation; reconstructing the trusted run establishes
the correct old/new source ranges. This is an improvement in an established
result, not merely an additional candidate. The executable test and its log
are retained with the evaluation artifacts.

## Real-document comparison

The production baseline predates the shared assessment and mixes inferred
confidence with accepted output. Its change count and coverage are not
interpreted as calibrated precision or compared as identical definitions.
The second baseline removes only the local-domain discovery invocation from
the shared-assessment source snapshot. Both current and counterfactual
comparisons use independently rebuilt release executables and the same
manifest limit-scale hints.

The first current and counterfactual runs each attempted all 29 pairs and
retained 19 evaluation failures plus 10 completed comparisons. The evaluator
incorrectly required proposal-origin totals to equal final accepted-change
totals. This was reproduced and corrected on the development FIPS pair;
the failed runs are preserved in `failed-origin-accounting/`. A second
development correction charges only inspected tokens during exact anchor
searches. Both evaluation methods include this correction.

The corrected FIPS development probe reports 1,404 candidates, four proven
changed regions, no accepted exact changes, and 603 resolved tokens per side.
The old/new denominators are 222,130 and 175,912 tokens. Its assessment uses
441,290,902 of 512,000,000 work units. These small resolved fractions are not
evidence of broad document recovery.

The same probe exposed unbuffered CLI JSON output: constructing the report
took about one second, but writing its source evidence continued beyond
15 minutes. Buffered atomic output completes the whole comparison and JSON
write in 57.31 seconds, producing 1,844,257,858 bytes and exit code 3 for the
incomplete comparison. The output remains large; buffering changes write
behavior without removing source evidence.

The first complete evaluation after these corrections attempted all 29 pairs
with both methods. Both runs finished with zero fatal evaluation failures.
The common/current operational totals were respectively 8/11 resource-limit
outcomes, 7/7 unsupported outcomes, and 1/1 unresolved extraction outcome.
The remaining 13/10 trials had operational `ok` status; this does not imply
complete comparison. The raw CLI summaries also count expected incomplete
extraction as healthy, so their healthy count is not the evaluation's `ok`
count.

Both methods match 0 of 40 listed exact changes. Their retained candidates
match 17/40 listed changes; generator recall separately measures 13/13
evaluable counterparts, with six unavailable counterparts. None of the ten
holdout pairs has change annotations, so holdout precision and recall remain
unavailable. No document has whole-document complete annotations; four have
scoped-complete annotations and eight have partial annotations. These are
material limits on the generalization evidence.

Local recovery changes accepted output from 36 to 35 events and combined
resolved-token counts from 386,953 to 196,076. FIPS gains 1,206 resolved tokens,
but QGIS English loses 255,411; SP 800-57, Korean W-4, and BIS core principles
also lose resolved coverage in development. SP 800-171 loses 5,756 resolved
tokens in holdout. No annotated exact-recall improvement is established.
The [per-pair observations](2026-09-08-common-assessment/pre-localization-budget-fix/per-pair-comparison.md)
retain every increase and decrease, with the complete metrics in the adjacent
JSON artifact.

The stage counters identify a concrete development failure: local-view
discovery exhausts the budget before ordinary proposal localization on QGIS
English and BIS core principles. The implementation now gives ordinary
assessment precedence. The completed observations and their exact source
identities remain in `pre-localization-budget-fix/`; they are not replaced by
later measurements. Subsequent runs on the exposed corpus are regression
checks, not a new blind holdout evaluation.

## Ordered assessment regression check

Ordinary proposal and move proofs now run before local-view discovery.
Previously established correspondences are retained, and tentative proposals
are retried only when local views were found and work remains. The total work
limit is unchanged. Move proofs remain separate from ordinary change owners.

Development probes restore 255,411 resolved tokens and four accepted changes
on QGIS English, 12,887 resolved tokens and seven accepted changes on BIS core
principles, and 732 resolved tokens on Korean W-4. FIPS retains its 1,206-token
gain. These probes are supplementary observations of the same development
pairs, not additional independent trials.

Both final 29-pair runs completed with zero fatal failures. Nineteen pairs
completed extraction; no pair completed comparison. Relative to the rebuilt
common-assessment counterfactual, no pair loses accepted changes or resolved
tokens. Nine development pairs gain 70,758 resolved tokens in total; one
holdout pair gains 15,525. The combined count increases from 386,953 to
473,236 out of 20,621,057 observed old/new tokens in 28 pairs with token
partitions. JVM extraction has no comparable partition and remains a separate
unresolved operational outcome.

| Metric | Common assessment | Local recovery |
| --- | ---: | ---: |
| Operational trials | 29 | 29 |
| Resource-limit outcomes | 8 | 10 |
| Unsupported outcomes | 7 | 7 |
| Unresolved extraction outcomes | 1 | 1 |
| Fatal outcomes | 0 | 0 |
| Accepted changes | 36 | 46 |
| Final candidates | 16,663 | 16,653 |
| Listed exact matches | 0/40 | 0/40 |
| Listed final-candidate matches | 17/40 | 17/40 |
| Unresolved regions | 97,147 | 96,407 |
| Proven changed regions | 116 | 121 |

The additional accepted events do not match listed gold changes, so their
count is not reported as an independently verified detection improvement.
Reviewed scopes have zero false-positive changed tokens under both methods,
but their expected changed tokens remain missed. Unannotated regions do not
inherit this false-positive claim. The two additional limit outcomes are
EDPB right of access and ECMA-109: optional recovery consumes the remaining
shared budget while preserving ordinary established results.

The [per-pair comparison](2026-09-08-common-assessment/per-pair-comparison.md)
and [per-series comparison](2026-09-08-common-assessment/per-series-comparison.md)
retain the complete breakdown. The adjacent evaluation JSON records separate
old/new token denominators, candidate/proven quality, stage work, runtime,
memory samples, and unavailable values. The
[preceding production observations](2026-09-08-common-assessment/production-comparison.md)
use their original output definitions and are not a calibrated quality
comparison. Commands, hashes, and completion states are in
`final-run-metadata.json`.

## Validation and release status

The final workspace passes formatting, Clippy with warnings denied, and
2,045 tests. The six generated exact regressions remain a release-condition
failure: passing candidate-policy checks does not satisfy the requirement to
preserve existing exact expectations. This implementation is not reported as
release-ready.

## Execution limitations

All attempted pairs, including incomplete extraction, resource limits, and
fatal failures, remain in the operational denominator. Partial annotations
support listed-change recall only; absent annotations do not become perfect
quality scores. Memory samples are the process cumulative peak RSS and are
not isolated per-pair allocation peaks. Concurrent execution and environment
information are recorded alongside the timing observations.

An earlier capture attempt shared Cargo build output between current and
counterfactual source trees. Its executable provenance was ambiguous, so
those processes were interrupted and their results excluded. The replacement
runs use distinct, independently rebuilt executables; the interrupted
attempts remain documented in the environment record.
