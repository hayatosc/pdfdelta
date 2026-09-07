# Unchanged context for FIPS false-positive measurement

The existing reviewed scope contains only a moved requirement, so every reviewed token is counted as changed and the unchanged-token denominator is zero. The additional scope is the unchanged comments-address paragraph in the Foreword. It appears between the opening paragraph and the director signature in both revisions, at PDF page indices 2 and 1 respectively. The pagination changes, but this paragraph's text and local order do not.

`old-source.png`, `new-source.png`, and `source-review.json` retain the visual and textual evidence. The paragraph is complete as an unchanged scope; it introduces no expected change and leaves every existing expectation intact. The annotation remains partial outside its two complete scopes.

Both executables use `manifest.tsv` and the identical `reviewed-expected.json`. `comparison.json` records their absolute coverage, token precision, false-positive rate, event recall, and source hashes. The corpus annotation includes the same added scope.

Reproduce each capture with its recorded executable and `revisions --manifest benchmark/realworld/results/issue12-anchor-recovery/fips-unchanged-scope/manifest.tsv --cache-dir benchmark/realworld/cache --summary-json-output OUTPUT.json`.
