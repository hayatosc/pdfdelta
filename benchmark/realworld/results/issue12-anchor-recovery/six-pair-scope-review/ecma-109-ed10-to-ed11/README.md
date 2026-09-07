# Bounded source-reviewed scope: ECMA-109 edition cover

The existing annotation remains partial. This supplement declares the ECMA-109 cover edition and date block as a complete source-reviewed scope for the 10th to 11th edition pair. Repeated copyright changes, dense tables, and the rest of the standard remain outside this bounded scope.

`old-scope.png` and `new-scope.png` show the reviewed cover pages. `source-review.json` records the source hashes, page indices, normalized page text, unique scope occurrences, and Unicode-scalar changed ranges. `reviewed-expected.json` is the expected annotation used by `manifest.tsv`.

The changed ranges were checked against the rendered source and independent PyMuPDF extraction. Reproduce a capture with `revisions --manifest benchmark/realworld/results/issue12-anchor-recovery/six-pair-scope-review/ecma-109-ed10-to-ed11/manifest.tsv --cache-dir benchmark/realworld/cache --summary-json-output OUTPUT.json`.
