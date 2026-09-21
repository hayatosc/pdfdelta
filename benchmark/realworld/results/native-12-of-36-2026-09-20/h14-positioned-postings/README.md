# H14 positioned first-token index and resolution-boundary projection

Status: accepted intermediate candidate pending this commit. Baseline for every
comparison is the accepted H13 capture `h13-full-iteration-004-native` (binary
`fbab3082424a`); the final capture is `h14-full-iteration-005-native`
(binary `c2c4d152d261`).

## Production changes

- `views::TokenPostings<'a>` borrows immutable tokens from the prepared views
  as map keys (no font-hash clones), listing the exact starts the scanning
  path's first-token prefilter accepts in `(view, start)` order.
  `positioned_occurrences_indexed` keeps full token-sequence verification,
  unknown-metadata veto, deny-only semantics, self-skip, the whole-member flag
  and first-match order; the position-only mode never uses the index. The
  build preflights counts, `u32` conversions, affordability and a conservative
  combined storage bound before charging; only a preflight refusal preserves
  the shared remainder, while an allocation failure after the charge keeps the
  charge because the work was attempted. `occurrence_state` factors the shared
  metadata evaluation and computes the whole-member flag only for the first
  confirmed occurrence.
- `report::source::project_resolution_range` selects zero-width normalization
  events by their comparable-token owner: the first token at the event's
  grouped canonical position, or the preceding token for a terminal event. The
  containing span owns the event, covering unmapped-only slices, leading and
  terminal positions and adjacent entries without loss or duplication;
  comparable-empty spans keep the canonical point/interior rule and tokens
  absent from the block stay with the entry ending at the event. Generic span
  selection, change projections and existing JSON semantics are unchanged;
  `JsonResolutionRange` is the only caller of the new policy.

## Tests and mutation control

- `positioned_equalities_recovers_short_endings_with_a_stage_budget`: thirty
  long differing fillers plus five short identical endings; measured indexed
  spend 1,165,692 versus legacy 1,236,340 with a 1,200,000 stage budget and
  exact ending domains asserted. Disabling the production posting build fails
  (exit 101); restoring the exact bytes passes
  (`mutation-control.txt`, source SHA-256 `0920fcf7...` before and after).
- `json_resolution_partition_keeps_a_boundary_line_break_once`: JSON
  integration regression; reverting `JsonResolutionRange` to the generic
  projector fails (exit 101), restoring passes (exit 0).
- Source-level regressions cover boundary ownership exactly once, leading and
  terminal events, the unmapped boundary slice, two-block isolation, parity
  between indexed and scanning searches over repeated tokens, substrings,
  unknown metadata, deny-only equal/different evidence, self-skip and view
  order, budget-sweep oracle equality with cuts staying `None`, and a refused
  index preserving the shared remainder.

Historical note: before the projection fix, finer resolution partitions lost
zero-width line breaks exactly at cut boundaries (18 W9 blocks plus additions
elsewhere). The fix and its ownership policy supersede that finding; the early
non-discriminating wiring fixture is also superseded by the short-ending
regression above.

## Final full-panel capture (binary `c2c4d152d261`)

36/36 captured, 3 complete. 28 reports are logically byte-identical to H13.
The eight differing pairs and their deep `native_retention_audit` results are
recorded in `full36-manifest.json` and `retention-final/`:

| pair | H13 | H14 | retention |
| --- | --- | --- | --- |
| irs-w2 | 10,002 / 10,002, 403 unresolved | **14,471 / 14,471, 212 unresolved** | needs-review: two additive boundary restorations |
| irs-w9 | 0 / 0, 1,011 unresolved | **15,477 / 15,467, 640 unresolved, 25 changes** | pass |
| irs-1099-misc | 8,228 / 8,224, 110 unresolved | identical metrics | needs-review: two additive boundary restorations at block 1 |
| nist-sha-1803 | 5,946 / 5,944, 1,013 unresolved | identical metrics | needs-review: two additive boundary restorations |
| irs-schedule-se | complete | identical metrics | pass |
| irs-schedule-c | 6,619 / 6,634, 11 changes | identical metrics | pass |
| bunka-official-writing | 0 / 0 | identical metrics | pass |
| nist-risk-assessment | 0 / 0 | identical metrics | pass |

All six `needs-review` side reviews are **restoration-only** (no losses):
`added-boundary-evidence.json` binds each added structural event to an observed
`SoftLineBreak` normalization event captured by a temporary in-crate
`prepare()` probe over the hash-verified frozen inputs
(`normalization-probe.txt`), together with the before/after entry cuts and the
selected comparable owner. Context glyph references repeat per the ordered
deduplicated per-span projection contract and are labelled separately, never
as new ownership claims. No context-glyph repetition or inferred position was
promoted to observed evidence.

## Verification

The seven required gates exited 0 on this source state (`cargo fmt --check`,
workspace clippy with `-D warnings`, workspace tests, all-features library
tests, rustdoc with `-D warnings`, generated-fixture `pdfbench verify` 48/48,
`git diff --check`). Score remains 3/36; the two gained pairs are large but not
yet complete.
