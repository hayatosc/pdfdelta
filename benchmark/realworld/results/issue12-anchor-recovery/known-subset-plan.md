# Preserve the page's ordered subset

## Evidence

`supported-order-probe.json` records a diagnostic run against the corrected anchor implementation. Before the fallback to `Unknown`, the old CSF page 4 has `Known([1])` for 47 of 49 lines, the new page 4 has `Known([1])` for 44 of 45 lines, and the new page 7 has `Known([1, 2])` for 31 of 34 lines. These are the pages containing the two remaining reviewed replacement targets. The old Core target page already has an explicitly inferred traversal.

This is evidence of a discarded partial order, not evidence that the expected rewritten sentences correspond. The prior structural-profile investigation did not inspect this classification boundary.

## Implementation boundaries

1. Resolve a proven region traversal against the supported regions and preserve it as `KnownLines`. Do not extend it to unsupported lines or promote an inferred traversal to a proven one.
2. Preserve omitted lines as barriers in the raw traversal while reordering the selected subsequence. Appending every omitted line at the end can join text across its source gap. Keep every source line, existing contiguous-run metadata, and exact token provenance.
3. Record which lines participate in a page-level proven or inferred traversal. Local trusted runs on an otherwise unordered page do not qualify. A block with any line outside this subset must remain outside the ordered projection.
4. Align the ordered feature projection using the existing alignment scores and resource limits. Generate exact-anchor uniqueness counts from the complete source census before excluding any block. Remap extraction gaps conservatively; layout projection must not hide extraction uncertainty.
5. Retain excluded source blocks in explicit unresolved alignment spans. Keep full normalized blocks for exact diff and recovery. Propagate the existing inferred confidence cap through the projection.
6. Measure the main five pairs first, then all 29 only if the implementation passes the existing tests and the scoped preview justifies adoption. Keep the original acceptance criteria and distinguish unresolved correspondence from successful change detection.

## Validation

The boundary regression is `A → U → B`, where `U` is unsupported. Reconstruction must retain `U` as a barrier and preserve all three sources, even when only the order of `A` and `B` is proven. A staggered two-column page must retain local run evidence while remaining excluded from the global ordered projection. Existing fully known and partial row-major fixtures must continue to work.

Projection tests must cover a duplicate in an excluded source, fully excluded input, invalid indices, extraction-gap remapping, inferred confidence, and resource failures. The projection must never assert a match merely because it removed a competing source occurrence. Benchmark comparison must preserve available changed-token precision, false-positive measurements, and reviewed recall, or reject the experiment.

The two CSF replacements are still unmet at the measured `run-order` snapshot. This plan does not authorize a semantic model, lower a matching threshold, or redefine the issue as complete.
