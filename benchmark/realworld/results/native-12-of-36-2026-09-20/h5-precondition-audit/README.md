# H5 audit: intrinsic provenance and the optional-positioned scheduling cause

Status: the per-span exemption was **not needed and not implemented**. The
intrinsic audit was correct and useful, and it pointed at optional-positioned
scheduling rather than a missing exemption, but the resulting scheduling
candidate was **rejected**: it gained source-bound coverage on
`edpb-restrictions-v1-to-final` while dropping 1,486 previously resolved tokens
per side on `irs-w2-2024-to-2025`. The engine was reverted to the accepted
tree; the capture is pinned as rejected. See `../rejected-h5-scheduling/`.

## Method

A first pass mapped candidates onto unresolved-region evidence labels. That
was circular: a coarse forced span over the whole window makes every subrange
appear to touch order evidence. The corrected method joins each candidate span
block to the *intrinsic* prepare-time provenance (per-block layout reason code,
uncertain/inferred flags, normalization issue count, trusted-run interval),
recorded with a temporary diagnostic on the accepted source (removed before
acceptance).

## Result (raw provenance, accepted HEAD)

| span block provenance | candidates |
| --- | ---: |
| all blocks trusted, not inferred, no normalization issues (`clean`) | 22 |
| blocks inside inferred order only | 80 |
| at least one intrinsically order-uncertain block | 8 |

The 110 candidates split into 22/80/8. Example clean pair: candidate 33 pairs
old block 213 (`There `) with new block 215 (empty), both intrinsically trusted
and issue-free. Example fragments: candidates 1/2 pair old block 77 (`.` ) with
new block 88 (`icle`), and candidate 4 pairs old block 85 (`In addi`) with new
block 96 (`Fur`); those blocks are intrinsically order-uncertain or inferred,
so their barriers are correct.

## Candidate cause and fix (historical, rejected)

Lifecycle instrumentation on `edpb-restrictions` showed the initial proposal
loop charges almost nothing and accepts nothing (root order/uncertainty
reasons), `views::discover` finds 397 domains and 163 anchors (23 domains touch
the candidate blocks), and the **optional positioned-equality pass then
consumed the entire remaining 9,431,023 units**, leaving `remaining_work = 0`
so the preserved domains could never be validated. The optional pass is
explicitly allowed to drop only its own unproven additions.

Fix: the optional pass runs on a bounded share (half) of the currently
remaining budget and only its actual spend is charged to the shared total, so
already discovered anchor domains stay validatable inside the same default
budget. A truncated optional pass records `WorkLimit`/incomplete search on the
root; occurrence and rival proofs inside the pass are unchanged, inferred order
is never promoted, and no limit is raised.

Measured on the frozen pair (default settings, compressed capture; candidate
binary `e20a110e2da08931b5dcd7824609d224901ac5c614bae1ef52395e9026c1c0f3`,
capture `benchmark/realworld/cache/native-12-of-36-2026-09-20/h5-iteration-004-native/`;
the final full-panel rerun and its binary hash are recorded with the
increment):

| metric | baseline | candidate |
| --- | ---: | ---: |
| resolved old tokens | 0 / 53,297 | 23,212 / 53,297 |
| resolved new tokens | 0 / 56,965 | 23,306 / 56,965 |
| established changes | 0 | 41 |
| unresolved regions | 1,234 | 854 |
| tentative candidates | 110 | 88 |

The three complete controls (`irs-schedule-se`, `faa-thunderstorms`,
`bunka-kana`) stay complete, and `irs-schedule-c`, `irs-1099-misc`,
`edpb-design-default`, `nist-ai-rmf` and `faa-maintenance` are unchanged in the
targeted capture. This was a candidate gain only; it is not accepted because
the same change loses prior W2 resolutions (see `../rejected-h5-scheduling/`),
and the pair remains incomplete.

## Preserved proof conditions

The intrinsic audit classified the 110 baseline candidates as 22 clean, 80
inferred-only and 8 crossing an intrinsically uncertain block. These are
baseline categories; the final output was not re-adjudicated candidate by
candidate. What the fix preserves is narrower and directly checked:

- the diff and assessment proof conditions are unchanged: barrier handling,
  inference caps, occurrence semantics and the optional pass's own proofs are
  not modified;
- a truncated optional pass records the work limit and leaves its unproven
  additions unresolved rather than committing them;
- total charged work never exceeds the original budget (the optional share and
  actual spend both come from the same total).

The measured metrics are tied to the frozen capture and binary below, not to
"the same binary" in the abstract.

## Files

- `provenance-join.json` — per-candidate intrinsic provenance, clean candidate
  details and crossing examples.
