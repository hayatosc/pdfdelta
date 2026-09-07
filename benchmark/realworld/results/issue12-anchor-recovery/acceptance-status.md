# Verified acceptance status after final ownership diagnostics

The latest measured snapshot is `final-ownership/source.json`, with its reproducible diagnostic patch and 29 per-pair before/after measurements. The core snapshot remains `gap-budget/source.json`, which fixes optional gap budget exhaustion discarding exact anchors. Source-reviewed token precision and false-positive-rate comparisons now cover all twelve annotated pairs. `acceptance-evidence.json` links the requirement-level evidence and retains the remaining limitations.

## Coverage and expected-change evidence

Coverage values are fractions of comparable tokens. The before capture is the investigation baseline, not the original issue's historical measurement.

| Pair | Before old / new | Verified old / new | Expected-change evidence |
| --- | --- | --- | --- |
| ECMA-109 | 0.768193 / 0.763385 | 0.822875 / 0.818065 | Failure list is empty. |
| EDPB right of access | 0.771057 / 0.783774 | 0.847132 / 0.858133 | Failure list is empty. |
| NIST CSF | 0.676293 / 0.715116 | 0.677881 / 0.718028 | Two replacements remain unmatched; final source ownership supports `alignment_or_candidate`. The original reviewed relation recall is 1/3; the new scoped event metric is unavailable. |
| NIST FIPS 186 | 0.649120 / 0.557080 | 0.684171 / 0.601346 | Failure list is empty. |
| NIST SP 800-57 | 0.562817 / 0.549473 | 0.685016 / 0.671662 | Association punctuation/case correspondence is recovered; corrected-annotation recall is 6/8. |

## SP 800-57 source and annotation correction

A rotated URL line previously vetoed row-order correction for the entire glossary leaf. The repaired proof leaves unsupported lines at fixed positions and corrects only contiguous supported runs. Association's definition now forms a complete block instead of being interrupted by its term label. The existing row geometry and render-order checks remain in force.

Rendered source-page review also disproved the Approved-definition replacement in the previous annotation: both revisions retain the algorithm-or-technique lead-in, and the actual change is a comma deletion. Two new-side scope anchors encoded the same interleaved table-cell text. `sp80057-annotation-review/` preserves the source renders and old annotation; the corrected annotation is now applied to the corpus. Association's punctuation change remains valid.

The corrected annotation is evaluable on the repaired engine, with token precision 1.0, recall 7/7, and zero false-positive tokens within the reviewed complete scopes. The baseline cannot resolve the full glossary start anchor. The supplementary localized-quote evaluation preserves the same seven changed source tokens and computes token metrics independently of event classification: baseline precision is 3/29 and false-positive rate is 182.712579 per 10,000 unchanged tokens, compared with 1.0 and 0.0 on the fixed engine. `sp80057-token-comparability/` records the source-range equivalence and final-executable capture. This establishes token precision and false-positive-rate non-regression; baseline event quality remains unavailable. The corpus annotation is unchanged by the supplementary evaluation. Earlier `scoped-token-metrics/` captures use the preserved, erroneous annotation and establish only historical reproducibility.

Two SP 800-57 expectations remain unmatched: 150 footer occurrences against 157 and the Approved comma deletion as a replacement. `row-barrier/association-residual.json` records the former interior gaps between exact source anchors. The anchored-gap path now recovers their unchanged text and diffs the changed gap exactly. Its inferred correspondence is Low confidence and retains explicit bracketing source evidence; coverage and ownership count only the gap.

## Source-reviewed precision across the annotated corpus

The six previously unscoped pairs now have bounded complete scopes containing unchanged context, documented in `six-pair-scope-review/`. FIPS additionally includes an unchanged Foreword paragraph so its false-positive-rate denominator is nonzero. The annotation remains partial outside each declared scope. Source review also corrected the IRS Form 1040 whole-footer insertion into its retained text, year replacement, and added creation stamp.

`annotated-precision-comparison.json` includes all twelve annotated manifest pairs and their evidence references. Both executables use the same source-reviewed annotation for each comparison. Precision and false-positive rate do not regress on any of these pairs. Rates below are per 10,000 reviewed unchanged tokens; these are scope measurements, not whole-document accuracy.

| Pair | Precision before / after | False-positive rate before / after |
| --- | ---: | ---: |
| arxiv-attention-v6-to-v7 | 1.000000 / 1.000000 | 0.000000 / 0.000000 |
| bis-operational-risk-2011-to-2021 | 1.000000 / 1.000000 | 0.000000 / 0.000000 |
| ecma-109-ed10-to-ed11 | 0.875000 / 0.875000 | 84.745763 / 84.745763 |
| edpb-right-of-access-v1-to-final | 0.714286 / 0.714286 | 169.491525 / 169.491525 |
| irs-form-1040-2024-to-2025 | 0.000000 / 0.000000 | 0.000000 / 0.000000 |
| irs-w4-korean-2024-to-2025 | 0.000000 / 0.000000 | 0.000000 / 0.000000 |
| nist-csf-v1-1-to-v2-0 | 0.700787 / 0.700787 | 10000.000000 / 10000.000000 |
| nist-fips-186-4-to-5 | 1.000000 / 1.000000 | 0.000000 / 0.000000 |
| nist-sp800-57-part1-r4-to-r5 | 0.103448 / 1.000000 | 182.712579 / 0.000000 |
| oasis-csaf-v2-cs01-to-csd02 | 0.287356 / 0.287356 | 3974.358974 / 3974.358974 |
| oasis-mqtt-311-to-50 | 1.000000 / 1.000000 | 0.000000 / 0.000000 |
| w3c-ws-policy-attach-20060927-to-20061102 | 0.983539 / 0.983539 | 68.027211 / 68.027211 |

The new scope event recall stays at 1.0 for ECMA and EDPB, already saturated before the coverage gain. CSAF's two cover replacements remain unmatched as events despite token recall 1.0. Both IRS scopes report no changed tokens and miss their expected edits; zero false positives is not positive recall evidence. CSF reports both full sentences as deletion/insertion, including every unchanged scalar, so its scoped false-positive rate is 10000. Its event coordinates cannot be assigned to the complete scope, and the metric is unavailable rather than zero. This is an event-coordinate limitation, not a resource-limit failure. Source-equivalent SP 800-57 token recall improves from 3/7 to 7/7 while baseline event quality remains unavailable.

These observations explain why increased recovered coverage does not imply improved replacement-event recall: coverage includes source-backed one-sided output, whereas a replacement requires the old and new sides to be paired correctly. The final diagnostic correction preserves those missed relations.

## Other pairs and checks

All 29 corpus pairs were rerun with the final executable. No coverage decreases or available changed-token precision, false-positive rate, or reviewed-recall regressions occur against `row-barrier`. The only expected-failure-list change is removal of Association's failure. Missing measurements remain unavailable. The earlier row-barrier step lowered IRS Form 1040 coverage from 0.516980/0.490659 to 0.506352/0.486039 without changing reviewed metrics; that historical regression remains recorded in its own comparison.

Formatting, workspace Clippy with warnings denied, and all 1,988 workspace tests passed. Generated verification passed 48/48 cases, including all five mandatory core cases. The budget repair preserves status, coverage, quality, and expected-change diagnostics on all 29 pairs compared with `anchored-gap`. The final diagnostic repair changes only CSF's two failure reasons on all 29 pairs; status, coverage, measured quality, and all other failures are identical. These checks establish code health and the measured behavior, not completion of the remaining correspondences.

CSF's two replacements remain unmatched. All four quoted source ranges are fully owned by final deletion/insertion events with no final unresolved overlap, as recorded in `csf-final-ownership/`. The former `reading_order_unresolved` diagnostic described initial alignment evidence rather than final output. The corrected classification retains both failures and does not raise recall. Per-pair captures and source evidence are retained with this change. SP 800-57 token non-regression is established by the supplementary source-equivalent evaluation; its baseline event metrics remain unavailable. The rejected enumeration experiment remains documented in `enumeration-decision.md`, with further adjacent-anchor and list-counterexample evidence in `anchored-gap/`; no semantic model was introduced.

## Named-pair diagnostics and inferred confidence

The complete final diagnostic captures contain zero `reading_order_unresolved` expected-change failures for ECMA-109, FIPS 186, SP 800-57, CSF, and EDPB right of access. They use the expected kinds and quotes retained at commit 4807545. The later complete-scope metadata does not alter core comparison output; CSF's separate scoped event evaluation limitation is recorded above. `final-ownership/coverage-from-baseline.json` records the absolute before/after coverage fractions for all 29 pairs.

Final content, formatting, and proven changed regions that touch inferred-order source blocks are demoted to `Confidence::Low` in `demote_inferred_order_changes`. Source-anchored gap replacements are also emitted at Low confidence. Passing pipeline and anchored-gap tests in `acceptance-evidence.json` cover these output boundaries. No inferred-order change is promoted to High confidence by this work.
