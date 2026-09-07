# Exact sub-block recovery investigation

Issue 12 is not resolved by this capture. The baseline still misses the two reviewed CSF replacements, `all-sector-scope-emphasized` and `core-expanded-from-five-to-six-functions`.

The latest measured implementation is [the optional-gap budget repair](gap-budget/README.md), following [anchored-gap recovery](anchored-gap/README.md), the source annotation correction, and the row-order repair. It recovers SP 800-57's Association correspondence and increases reviewed scoped-token recall from 3/7 to 7/7. See [acceptance status](acceptance-status.md) for the remaining requirements; the sections below preserve earlier investigation stages.

## Baseline

The `before` capture was generated from the current worktree before connecting exact sub-block anchors to sentence recovery. It includes the existing layout inference, final inferred-confidence propagation, and repeated running-matter recovery changes. `baseline-source.json` records the commit, source hashes, and executable hash. Reconstruct the snapshot by applying `baseline.patch` to that commit and adding `baseline-page-anchor.rs.txt` as `crates/pdfdelta-core/src/diff/recovery/page_anchor.rs`.

All 29 fresh baseline summaries match `../issue12-intraleaf-v2/after/all.json` field for field. CSF coverage is 0.6762929693420874 old and 0.715115683491139 new, with one of three reviewed semantic relations detected.

The baseline completes comparison for 24 pairs. Five reach resource limits: GCC, LibreOffice, QGIS English, QGIS Spanish, and Unicode. These are retained as failures. Scoped changed-token precision and false-positive measurements are available for five pairs; absent measurements do not establish precision.

## Evidence and constraints

The CSF target quotes occur in clean blocks with trusted local reading order. Their surrounding alignment window remains unresolved, but the quotes are not missing from extraction. The recovery diagnostic's `fully_contained` flag checks a whole trusted-run descriptor; it is not a quote-truncation flag.

The exact-window scanner finds two shared source ranges in the retained CSF block evidence. Its rolling fingerprint must advance after every window, including new hash buckets and duplicate matches. Token equality is still checked after hashing. The standalone reproduction now succeeds within the existing four-times-input token-work cap; `../issue12-page-anchor-investigation/scanner-verification.json` records that bounded experiment.

An exact substring establishes unchanged source content. It does not establish a correspondence between nearby rewritten sentences. In particular, page separation alone does not justify reporting the CSF scope paraphrase as a replacement. Production recovery must retain that distinction, preserve original token differences, and leave unproven regions unresolved.

## Reproduction

Build each source snapshot offline with `cargo build --release -p pdfdelta-bench --offline`, retain its executable separately, and run the capture script from the repository root:

```sh
PYTHON_UV=0 python3 benchmark/realworld/results/issue12-confidence/run-pairs.py BINARY EMPTY_OUTPUT_DIRECTORY
```

The runner records executable identity and uses the manifest's default resource scales. It refuses to overwrite a nonempty output directory. When both complete captures are available, run `compare.py` in this directory to record absolute before/after coverage fractions, reviewed recall, missing quality measurements, and precision or false-positive regressions.

## Exact-anchor integration measurement

`anchor-only` contains all 29 pairs after integrating source-backed, globally unique exact windows into residual recovery. `comparison-anchor-only.json` records absolute before/after coverage and quality measurements for every pair. Run `compare.py --after anchor-only --output comparison-anchor-only.json` to reproduce that comparison. `anchor-only-source.json`, `anchor-only.patch`, and `anchor-only-page-anchor.rs.txt` record the source snapshot and executable identity.

| Pair | Old coverage before / after | New coverage before / after |
| --- | --- | --- |
| ECMA 109 | 0.768193 / 0.818840 | 0.763385 / 0.813985 |
| FIPS 186 | 0.649120 / 0.686179 | 0.557080 / 0.603876 |
| SP 800-57 | 0.562817 / 0.672756 | 0.549473 / 0.647885 |
| CSF | 0.676293 / 0.677881 | 0.715116 / 0.718028 |
| EDPB right of access | 0.771057 / 0.844522 | 0.783774 / 0.855425 |

No available changed-token precision or false-positive measurement regresses. Missing measurements remain unavailable, and the same five resource-limit failures remain failures. Reviewed recall and expected-change failures are unchanged: this stage resolves previously unresolved unchanged substrings, without claiming new replacement relations. CSF still has two `reading_order_unresolved` failures, so the overall acceptance criterion remains unmet.

The scanner uses the configured recovery minimum (16 tokens by default) in production. The older 128-token standalone experiment establishes scanner behavior at that setting, not the complete set of production anchors. Overlap checks reject conflicting source ownership; crossing between independent exact pairs is allowed because provisional block traversal does not prove their relative order.

`edpb-existing-inference-review.json` records a separately confirmed pre-existing false deletion: unchanged footnote text is consumed by a heuristic replacement before late exact recovery can use it. That failure remains open.

## Rejected correspondence-protection experiment

`after` and `comparison.json` retain the attempted whole-proposal anchor veto, not the adopted implementation. `after-source.json`, `after.patch`, and `after-page-anchor.rs.txt` identify its source and executable. This experiment required a replacement touching an exact substring to include its counterpart and rejected one-sided proposals touching such substrings.

The veto suppresses valid reviewed relations as well as dubious ones. CSF reviewed recall falls from 1/3 to 0/3, and FIPS falls from 6/6 to 4/6, adding a reading-order failure for the legacy DSA verification note. CSF old/new coverage falls to 0.479438 / 0.349099. Available token precision measurements remain unchanged, which alone is insufficient to accept this change. The comparison script now also flags reviewed-recall declines.

The protection implementation was reverted to the measured exact-enrichment version. A subsequent minimum-length correction fixed a custom-configuration test, but does not explain or repair these benchmark regressions, whose default minimum was already 16. Any future protection must preserve valid residual recoveries instead of discarding an entire proposal because one substring has another exact occurrence.

## Proven-run order correction

Review found that the initial exact-enrichment implementation also accepted crossing anchors within a single proven run. For example, unique ranges `A B` in one block and `B A` in its counterpart could both be consumed as unchanged, hiding the rearrangement. The correction rejects every participant in an inversion within each pair of proven runs, while preserving independent-run matches whose relative order is unknown. Prefix maxima and suffix minima keep the check bounded after sorting. Tests cover rearrangements within one block and across blocks in the same run, unchanged order, and independent runs.

The same snapshot also caps confidence for proven changed regions whose source blocks use inferred reading order; the source-level confidence contract applies to these report regions as well as changes and formatting changes.

`run-order-source.json`, `run-order.patch`, and `run-order-page-anchor.rs.txt` identify the corrected source and executable. The `run-order` capture is the measurement of that snapshot; the earlier `anchor-only` figures above predate the correctness fix.

All 29 pairs completed capture. `comparison-run-order.json` records 13 pairs with increased coverage and none with decreased coverage against `before`. No available changed-token precision, false-positive rate, or reviewed-recall metric regresses. The five existing resource-limit failures and 49 unavailable quality-metric entries remain explicit. Reproduce with `compare.py --after run-order --output comparison-run-order.json`.

| Pair | Corrected old coverage | Corrected new coverage | Expected reading-order failures |
| --- | --- | --- | --- |
| ECMA 109 | 0.818840 | 0.813985 | 0 |
| FIPS 186 | 0.683091 | 0.599976 | 0 |
| SP 800-57 | 0.669181 | 0.644684 | 0 |
| CSF | 0.677881 | 0.718028 | 2 |
| EDPB right of access | 0.842878 | 0.853822 | 0 |

The corrected source passes workspace formatting, Clippy with warnings denied, and workspace tests. Generated fixture verification passes 48/48, including all five core acceptance cases.

## Remaining CSF correspondence gap

The corrected CSF capture records four shared structural profiles, 2,114 candidate pairs, 2,114 duplicate pairs, and zero unique reciprocal structural pairs. The unique-pair anchor classifications are consequently all zero. Section-pairing shadow recovery examines zero matched spans and produces no heading or paragraph candidates. The two reviewed replacements have no exact shared recovery units and fail reciprocal near matching.

These measurements explain why retaining more unchanged text does not improve reviewed CSF recall. They do not prove that every possible structural algorithm would fail. They establish that the current source evidence and recovery paths do not justify either expected correspondence. No threshold relaxation, document-specific pairing, failure-category rename, or semantic model was added to satisfy the remaining acceptance condition.

A later partial-order projection experiment also failed to resolve either CSF case and suppressed an existing numeric replacement in a pipeline regression fixture. Its source and failure evidence are retained in [the rejected experiment](../issue12-ordered-projection/README.md). Production source was restored to the run-order snapshot above; the prototype is not part of the adopted change.
