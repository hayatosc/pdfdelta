# Complete evaluation scopes for six partially annotated pairs

Each pair now has a bounded, source-reviewed complete scope containing changed and unchanged text. The rest of each document remains partially annotated. PDF-page renders, source hashes, excerpts, original annotations, reviewed annotations, manifests, and independent baseline/current captures are retained in the pair directories.

Both executables receive the same reviewed annotation. `comparison.json` records their exact values and hashes. Precision and false-positive rate are unchanged for all six pairs; the identical values below apply to both executions. The false-positive rate is per 10,000 reviewed unchanged tokens.

| Pair | Reviewed scope | Token precision | False-positive rate | Token recall | Scoped event recall |
| --- | --- | ---: | ---: | ---: | ---: |
| CSAF | Cover stage and date | 0.287356 | 3974.358974 | 1.0 | 0.0 |
| CSF | Core Functions summary sentence | 0.700787 | 10000.0 | 1.0 | Unavailable |
| ECMA-109 | Cover edition line and retained title | 0.875 | 84.745763 | 1.0 | 1.0 |
| IRS Form 1040 | First-page footer | 0.0 | 0.0 | 0.0 | 0.0 |
| EDPB right of access | Confirmation-of-processing example sentence | 0.714286 | 169.491525 | 1.0 | 1.0 |
| IRS W-4 Korean | Page-three running header | 0.0 | 0.0 | 0.0 | 0.0 |

These are non-regression measurements, not evidence of uniformly accurate output. The form scopes report no changed tokens and miss their expected edits. CSAF's two scoped replacements remain unmatched as events despite their changed tokens being reported. CSF's full old/new sentences are reported as deletion/insertion, so every reviewed unchanged token is also falsely reported as changed. Its scoped event evaluation is unavailable because reported change coordinates are indeterminate within the scope; the recorded reason is not a resource-limit failure. The separate final-ownership evidence retains its two unmatched replacement relations.

Source review corrected the IRS Form 1040 footer expectation: its catalog/form text is retained, its year digit changes, and a creation stamp is added. The old whole-footer insertion annotation is preserved in that pair's `original-expected.json`. Other expectations retain their kinds and source quotes. Exact changed ranges preserve unchanged characters; `range-consistency.json` records the equal unchanged scalar complements after removing each declared replacement range. CSF's scalar alignment convention is an independent `difflib.SequenceMatcher` comparison with `autojunk=False`, explicitly recorded in its source review.

The new scopes do not establish whole-document precision or complete recall. `../annotated-precision-comparison.json` combines them with existing scopes, the separate FIPS unchanged-context review, and the source-equivalent SP 800-57 comparison to cover all twelve annotated pairs.

Reproduce a capture with the recorded executable and `revisions --manifest benchmark/realworld/results/issue12-anchor-recovery/six-pair-scope-review/PAIR/manifest.tsv --cache-dir benchmark/realworld/cache --summary-json-output OUTPUT.json`.
