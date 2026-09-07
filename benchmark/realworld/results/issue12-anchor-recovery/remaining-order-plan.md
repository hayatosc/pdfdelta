# Remaining reading-order work

## Established decisions

- Keep original glyphs, exact tokens, source ranges, and unsupported regions intact.
- Do not promote geometry-derived order to proven order. Apply Low confidence to every affected report event, including changed-region evidence.
- Use globally unique, source-backed exact substrings to recover unchanged content. Their presence does not establish a neighboring paraphrase relation.
- Prevent heuristic recovery from consuming an exact anchor with an inconsistent counterpart. Legitimate replacements containing corresponding parts of an anchor remain eligible.
- Retain CSF's two missed replacements as failures until actual pipeline evidence changes. A renamed diagnostic is insufficient.

## Implementation sequence

The attempted whole-proposal correspondence veto was rejected after CSF and FIPS recall regressed. Its measurement is retained in `comparison.json`. The current implementation retains late exact enrichment; step 1 below requires a different design that preserves valid residual recoveries. A separate render-inference experiment was also rejected; see `../issue12-interleaved-render/README.md`.

1. Complete exact-anchor correspondence protection in the recovery planner. Test source ownership, partial anchor intersections, legitimate surrounding edits, and bounded execution.
2. Rebuild and compare all 29 real-world pairs against the captured baseline. Record absolute coverage, available precision and false-positive measurements, expected failures, and unchanged recall explicitly.
3. Determine whether proven body order can be projected across unsupported margins without asserting content continuity. Preserve gaps at block joining and split/merge boundaries; preserve removed content in duplicate and candidate censuses. Unproven inter-region order must not enter this projection.
4. Implement a projection only if its source-order contract can be represented and tested without flattening missing lines into contiguous text. Measure the resulting alignment independently before retaining it.
5. Run formatting, workspace Clippy, workspace tests, and all five core acceptance fixtures before committing the implementation and measurement capture.

## Rejected shortcut

Converting a filtered region order directly to `KnownLines` can reorder `[known, unknown, known]` into `[known, known, unknown]`. The current block join checks the reordered uncertain indices, so it may join across the original unknown line. Preserving selected line IDs alone is therefore insufficient; the barrier and order evidence must survive independently.

The scope excludes OCR, semantic models, threshold loosening to fit the CSF annotations, and CJK normalization changes.

## Earlier counterfactual

The retained `../issue12-subblock-investigation/intraleaf/order-gate-oracle-scale4-summary.json` is labelled as an order-gate oracle run at a larger resource scale. It reports zero of three reviewed CSF changes and `alignment_ambiguous` for all three. Its old/new coverage is only 0.000503 / 0.000922. This is not an acceptance result: the retained artifact does not provide a complete source reconstruction for that executable. It cautions against assuming that removing the order gate alone will resolve the remaining correspondences, but does not establish the behavior of a proposed projection.
