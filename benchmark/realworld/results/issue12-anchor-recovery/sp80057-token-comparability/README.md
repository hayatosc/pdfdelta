# Comparable SP 800-57 changed-token measurements

The supplementary annotation preserves the same seven reviewed changed source tokens as the canonical source-corrected annotation. It shortens the glossary start anchor to its exact prefix and the end anchor to its exact suffix. It also shortens the Approved context around the same comma: canonical old range `86..87` becomes local range `14..15` at substring offset 72. The expected kind and the empty new-side changed range are unchanged. All other expectations are identical. `equivalence.json` records these checks; successful scope evaluation enforces unique anchors.

These smaller contexts can be located in the original engine's interleaved glossary extraction. They do not add a new gold change, change a source endpoint on the repaired engine, or change the corpus annotation. Both captures use the same supplementary manifest and expected file retained here.

| Metric | Original baseline | Fixed engine |
| --- | ---: | ---: |
| Expected changed tokens | 7 | 7 |
| Reported changed tokens | 29 | 7 |
| True-positive tokens | 3 | 7 |
| Changed-token precision | 0.103448 | 1.0 |
| Changed-token recall | 0.428571 | 1.0 |
| False-positive tokens per 10,000 unchanged tokens | 182.712579 | 0.0 |

This establishes changed-token precision and false-positive-rate non-regression for the reviewed source changes. The baseline event evaluation remains unavailable because reported events cross scope boundaries or otherwise cannot be assigned to one complete scope. Token evaluation projects source ranges independently, so that event failure does not invalidate the token result. Earlier reports incorrectly treated the event failure as loss of all measurements.

`comparison.json` records exact metrics, executable hashes, and artifact hashes. The current executable is the `gap-budget` build, not the earlier preliminary build. The canonical full-context baseline evaluation remains unavailable, and no baseline event-recall or event-precision comparison is claimed.

Reproduce with the corresponding frozen executable and `revisions --manifest benchmark/realworld/results/issue12-anchor-recovery/sp80057-token-comparability/manifest.tsv --cache-dir benchmark/realworld/cache --summary-json-output OUTPUT.json`.
