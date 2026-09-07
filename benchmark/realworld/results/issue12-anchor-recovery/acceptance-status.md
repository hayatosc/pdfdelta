# Verified acceptance status after anchored-gap recovery

The latest measured snapshot is `gap-budget/source.json`, with its reproducible patch and 29 per-pair captures in the same directory. It fixes the review finding that optional gap budget exhaustion discarded exact anchors. It does not satisfy all acceptance criteria.

## Coverage and expected-change evidence

Coverage values are fractions of comparable tokens. The before capture is the investigation baseline, not the original issue's historical measurement.

| Pair | Before old / new | Verified old / new | Expected-change evidence |
| --- | --- | --- | --- |
| ECMA-109 | 0.768193 / 0.763385 | 0.822875 / 0.818065 | Failure list is empty. |
| EDPB right of access | 0.771057 / 0.783774 | 0.847132 / 0.858133 | Failure list is empty. |
| NIST CSF | 0.676293 / 0.715116 | 0.677881 / 0.718028 | Two replacements still report `reading_order_unresolved`; reviewed recall is 1/3. |
| NIST FIPS 186 | 0.649120 / 0.557080 | 0.684171 / 0.601346 | Failure list is empty. |
| NIST SP 800-57 | 0.562817 / 0.549473 | 0.685016 / 0.671662 | Association punctuation/case correspondence is recovered; corrected-annotation recall is 6/8. |

## SP 800-57 source and annotation correction

A rotated URL line previously vetoed row-order correction for the entire glossary leaf. The repaired proof leaves unsupported lines at fixed positions and corrects only contiguous supported runs. Association's definition now forms a complete block instead of being interrupted by its term label. The existing row geometry and render-order checks remain in force.

Rendered source-page review also disproved the Approved-definition replacement in the previous annotation: both revisions retain the algorithm-or-technique lead-in, and the actual change is a comma deletion. Two new-side scope anchors encoded the same interleaved table-cell text. `sp80057-annotation-review/` preserves the source renders and old annotation; the corrected annotation is now applied to the corpus. Association's punctuation change remains valid.

The corrected annotation is evaluable on the repaired engine, with token precision 1.0, recall 7/7, and zero false-positive tokens within the reviewed complete scopes. The baseline cannot resolve its new glossary start anchor, so corrected baseline quality is unavailable. Supplementary shorter-anchor and localized-quote probes also fail to establish baseline reported-change coordinates; they do not change the corpus annotation. This is not a precision non-regression pass against the original baseline. Earlier `scoped-token-metrics/` captures use the preserved, erroneous annotation and establish only historical reproducibility.

Two SP 800-57 expectations remain unmatched: 150 footer occurrences against 157 and the Approved comma deletion as a replacement. `row-barrier/association-residual.json` records the former interior gaps between exact source anchors. The anchored-gap path now recovers their unchanged text and diffs the changed gap exactly. Its inferred correspondence is Low confidence and retains explicit bracketing source evidence; coverage and ownership count only the gap.

## Other pairs and checks

All 29 corpus pairs were rerun with the final executable. No coverage decreases or available changed-token precision, false-positive rate, or reviewed-recall regressions occur against `row-barrier`. The only expected-failure-list change is removal of Association's failure. Missing measurements remain unavailable. The earlier row-barrier step lowered IRS Form 1040 coverage from 0.516980/0.490659 to 0.506352/0.486039 without changing reviewed metrics; that historical regression remains recorded in its own comparison.

Formatting, workspace Clippy with warnings denied, and all 1,987 workspace tests passed. Generated verification passed 48/48 cases, including all five mandatory core cases. The budget repair preserves status, coverage, quality, and expected-change diagnostics on all 29 pairs compared with `anchored-gap`. These checks establish code health and the measured behavior, not completion of the remaining correspondences.

Outstanding requirements include CSF's two replacements and corrected-baseline quality evidence for SP 800-57. Per-pair captures and source evidence are retained with this change. The rejected enumeration experiment remains documented in `enumeration-decision.md`, with further adjacent-anchor and list-counterexample evidence in `anchored-gap/`; no semantic model was introduced.
