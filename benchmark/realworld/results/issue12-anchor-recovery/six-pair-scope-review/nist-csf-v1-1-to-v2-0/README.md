# Bounded source-reviewed scope: NIST CSF Core Functions

The existing annotation remains partial. This supplement declares the Core Functions summary sentence as a complete source-reviewed scope for the Cybersecurity Framework 1.1 to 2.0 pair. The all-sector framing, governance and supply-chain insertion, and the rest of the framework remain outside this bounded scope.

`old-scope.png` and `new-scope.png` show the reviewed source pages. `source-review.json` records the source hashes, page indices, normalized page text, unique scope occurrences, and Unicode-scalar changed ranges. `reviewed-expected.json` is the expected annotation used by `manifest.tsv`.

The changed ranges were checked against the rendered source and independent PyMuPDF extraction. They follow an independent difflib.SequenceMatcher comparison with autojunk disabled; removing the declared ranges leaves the same shared scalar subsequence on both sides. Reproduce a capture with `revisions --manifest benchmark/realworld/results/issue12-anchor-recovery/six-pair-scope-review/nist-csf-v1-1-to-v2-0/manifest.tsv --cache-dir benchmark/realworld/cache --summary-json-output OUTPUT.json`.
