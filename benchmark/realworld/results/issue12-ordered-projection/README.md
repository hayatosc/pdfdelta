# Rejected partial-order projection experiment

This capture records an unadopted attempt to align the page-level ordered subset while retaining omitted source blocks as explicit unresolved spans. Exact-anchor uniqueness used the complete source census. Layout reconstruction preserved omitted-line slots as block-joining barriers, and alignment restored excluded blocks to source order after matching.

`prototype.patch` applies to the commit in `prototype-source.json`; `page-anchor.rs.txt` supplies its untracked module. The source manifest also records the benchmark executable hash. `csf.json` was produced with the default resource limits using:

```sh
target/release/pdfbench revisions --cache-dir benchmark/realworld/cache --pair nist-csf-v1-1-to-v2-0 --summary-json-output target/issue12-projection-csf.json
```

The CSF result still misses `all-sector-scope-emphasized` and `core-expanded-from-five-to-six-functions`, both classified as `reading_order_unresolved`. Old/new coverage is 0.677401855590 / 0.717149839755, below the adopted run-order snapshot's 0.677880511524 / 0.718027892819. Reviewed relation recall remains 1/3.

`pipeline-tests.log` records 56 passing integration tests and one failure: `inferred_order_remains_low_confidence_through_sentence_recovery`. The fixture's numeric replacement previously reached sentence recovery through an unknown-order window. Projection instead sends it through ordered alignment, where the numeric-neighbor plausibility guard leaves it unresolved with `TextSimilarity`, `CandidateSource(NGramInvertedIndex)`, `NumericMask`, and `ReadingOrderPartial` evidence. The recovery gate excludes that evidence, so the replacement disappears. `confidence-diagnostic.log` records a diagnostic rerun with a temporary print statement, removed from the archived source.

This is a recovery regression, not merely a changed internal route. Neither the existing regression assertion nor the numeric plausibility guard was weakened to accept the prototype. The experiment was rejected and production source restored to the measured `../issue12-anchor-recovery/run-order-source.json` snapshot. The ordered-alignment unit tests passed 44 cases, but that narrower result did not establish pipeline correctness or issue completion.
