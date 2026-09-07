# Bounded source-reviewed scope: OASIS CSAF cover metadata

The existing annotation remains partial. This supplement declares the cover stage and publication-date block as a complete source-reviewed scope for the Committee Specification 01 to Committee Specification Draft 02 pair. The stage URL move and the specification body remain outside this bounded scope.

`old-scope.png` and `new-scope.png` show the reviewed cover pages. `source-review.json` records the source hashes, page indices, normalized page text, unique scope occurrences, and Unicode-scalar changed ranges. `reviewed-expected.json` is the expected annotation used by `manifest.tsv`.

The changed ranges were checked against the rendered source and independent PyMuPDF extraction. Reproduce a capture with `revisions --manifest benchmark/realworld/results/issue12-anchor-recovery/six-pair-scope-review/oasis-csaf-v2-cs01-to-csd02/manifest.tsv --cache-dir benchmark/realworld/cache --summary-json-output OUTPUT.json`.
