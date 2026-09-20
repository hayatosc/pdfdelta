# Rejected: H7 emission/work-path optimizations

Status: rejected candidates. Both engine variants were reverted; the captures
are pinned as rejected evidence. No production change was committed.

## Context

The accepted H2 report for `irs-1099-misc-2024-to-2025` leaves 2,790 old and
2,543 new unresolved tokens (resolved 8,228 / 8,224 of 11,018 / 10,767) with
61 tentative relations, `work_used == work_limit == 32,000,000` and
`candidates_truncated = false`. The stage profile is
`anchor_verification` 210,302, `local_views` 9,186,632, `localization`
36,630 and `emission` 22,566,436. Hypothesis: the emission stage repeats
expensive per-occurrence work, so removing real repeated work would let the
same budget close more relations.

## Variant A: indexed structural precondition

`validate_semantic_emission` scanned every relation block per candidate
occurrence (`span_may_contain`). An index over the relation's first block
positions plus an occurrence-length slice check replaced the scan, with the
index built and charged once per relation.

Result: `h7-iteration-001-native`, binary `25da78259dee`; 1099, Schedule C,
SE and W2 are byte-for-byte metric-identical to H2. No gain.

## Variant B: cached canonical group

`contains_span` rebuilt the outer canonical group per matching occurrence, and
`locate_in_group` additionally rebuilt prefix and local groups. Variant B built
the outer group and its block offsets once per relation behind a lazy,
charge-before-allocate path, resolved occurrences against the cached offsets,
and preserved the original predicate semantics with an equivalence test.
Empty-token many-block spans still charged block/offset work before any
allocation, and the zero-budget path failed before allocation.

Result: `h7-iteration-002-native`, binary `849957084f2f`; 1099, Schedule C,
SE and W2 are again metric-identical to H2. No gain.

## What the measurements do and do not show

The tentative 1099 relations carry `unknown_reading_order`,
`normalization_uncertainty` and `domain_not_closed`, and none of the 61
carries `WorkLimit`; candidate discovery is not truncated. That absence does
not prove every search completed: a failure can saturate the shared budget to
zero without recording a per-relation limit, and H7's two variants show only
that the measured rework targets did not change the four-pair metrics. The
1099 residual remains an **unresolved order/closure proof** problem, which
needs source-level order or move evidence rather than further generic
accounting changes; it is not proven impossible. Schedule C stays deferred
under the same reasoning.

## Decision

Both variants reverted; the next cause should come from a pair whose residual
is not the same order/closure barrier.
