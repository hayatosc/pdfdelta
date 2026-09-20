# H2: bounded one-sided semantic validation (intermediate gain)

Status: accepted intermediate gain. `faa-maintenance-records-c-to-d` is **not**
complete; the twelve-pair goal is unchanged.

## Hypothesis

`Assessor::validate_semantic_emission` rebuilds the contained-change set for
every semantically accepted relation by scanning the whole proposed change list
and charging `(context + source) * 2` work per occurrence before the exact
containment check. On a proven-empty one-sided comparison with P insertion
spans and C change events this is O(P × C) charged canonicalization: the
default 32,000,000-unit assessment budget is exhausted even though the side
offers no possible counterpart, leaving provable insertions tentative.

## Change

- Added `span_may_contain`, the structural precondition `contains_span`
  already implies: the occurrence's blocks must be a contiguous, in-order
  subsequence of the relation's own blocks. It follows the stored span order
  (no sorted-id assumption) exactly like the canonical group built inside
  `locate_in_group`, so non-monotone block ids cannot cause a false rejection.
- Occurrences that fail the precondition skip the canonicalization charge; the
  structural visit itself is charged (`1..span blocks` per side) before the
  check, so the repeated scan stays inside the shared budget instead of
  becoming uncharged work. Exact containment, ownership, multiplicity and all
  other proof obligations are unchanged.
- The other one-sided stages keep their shape: `assess` already takes the
  one-sided semantic path instead of Myers, and a present-side block
  normalization issue still holds the proof.

## Measured evidence

Candidate binary SHA-256
`80cc5fb13105a27ef625aa582fa04cc5acf64e520abe8e57ad55bc7a8bcd4cbe`, production
patch SHA-256 `f2d6c58399b800ff3253b3596e7bf492ab0f2ad5d908678d010189aca0e7f816`,
source revision `3bfbc880a4774d343c54308c2b9f4f77d8650610`.

Red/green pipeline fixture
`one_sided_valid_relation_work_is_not_quadratic_in_change_count`
(60 insertion blocks, `max_assessment_work = 40_000`): before the change 2 of
60 insertions were established with 58 unresolved and work exhausted; after the
change all 60 establish with full coverage and no candidates.

Frozen FAA maintenance (default settings, same driver):

| metric | baseline | H2 |
| --- | ---: | ---: |
| resolved new tokens | 17,933 / 63,089 | 62,117 / 63,089 |
| unresolved regions | 481 | 3 |
| tentative candidates | 481 | 3 |
| established changes | 88 | 566 (all insertions) |
| assessment work used | 32,000,000 (limit) | 2,124,554 |
| work localization stage | 31,745,834 | 1,142,742 |
| driver exit | 3 | 3 (incomplete) |

Audit of the 566 insertions: every occurrence is new-side only, every
`canonical_range` length equals its text length, 62,242 distinct glyphs are
owned exactly once with zero duplicates and zero unmapped tokens, and the 950
remaining glyphs in the 3 unresolved regions are disjoint from the change
glyphs. The frozen annotation's new-side page-0 quote is present across the
concatenated insertions; its old-side quote is outside native scope because the
scanned old page carries no visible native text.

## Full-panel capture

The fixed 36-pair capture (`h2-full-iteration-002-native`) captured 36/36 rows,
0 failed, 3 complete. The audited delta against the baseline scorecard is
exactly one pair: `faa-maintenance-records-c-to-d` gains 44,184 resolved new
tokens, loses 478 unresolved regions and gains 478 insertions. All three
complete controls (`irs-schedule-se-2024-to-2025`, `bunka-kana-1946-to-1986`,
`faa-thunderstorms-b-to-c`) remain complete.

Source retention (linear streaming audit over the compressed reports,
implemented in `benchmark/realworld/remaining/completion-investigation/audit_retention.py`,
recorded in `retention-audit.json`):

- 31 of the 36 H2 reports have a logical (uncompressed) SHA-256 equal to the
  frozen baseline report hash, verified per pair and listed explicitly in
  `retention-audit.json` (`whole_report_identical`; each entry records the
  verification mode and both report hashes).
- The remaining 5 report hashes differ. Four of them
  (`irs-schedule-se`, `irs-schedule-c`, `faa-thunderstorms`, `bunka-kana`)
  have all 11 audited source-bound members byte-identical to their
  baseline-identical captures (`summary`, `changes`, `change_candidates`,
  `proven_changed_regions`, `formatting_only_changes`, `unresolved_regions`,
  `extraction`, and the assessment `old_resolution`, `new_resolution`,
  `relations`, `review_units`). Every member digest is recorded for both
  sides; for example SE's `summary` digest is
  `c7ad3d0c96c904dc9c0ea0882b52324dc281912b402783fb2ea07166b2a87799` on both
  sides. Only the assessment work counters differ, and the audit excludes
  exactly those fields.
- `faa-maintenance-records-c-to-d` differs in the six members the fix changes:
  `summary`, `changes`, `change_candidates`, `unresolved_regions`,
  `new_resolution`, `relations`. Within it, all 17,933 new-side glyphs the
  baseline established as insertions are still owned by changes with identical
  span text (zero missing or reassigned), and the 62,242 inserted glyphs are
  owned exactly once and are disjoint from the 950 glyphs in the 3 residual
  regions.
- The audit fails on a truncated report, a missing required member or a
  duplicate member key; six independent serde-style fixture tests
  (`test_audit_retention.py`) cover nested child mutations, resolution
  coordinate mutations, excluded work counters, duplicates, missing members
  and truncation.

## Remaining blocker (explicit, not complete)

The 3 residual insertion blocks (`blocks 509, 512, 547`; 950 glyphs / ~972
tokens) carry `normalization_uncertainty` with `search: complete` and
`domain_not_closed`:

- block 509: `2. Compliance with Airworthiness Directives (AD) or SBs.\n3. ...`
- block 512: `9. Deviations from the customer work order.\n10. ...`
- block 547: `... slashes,\nhyphens, or spaces ...`

`resolve_line_breaks` records `AmbiguousLineBreak` when a break after terminal
punctuation such as `.` or `,` matches no lexical rule, and
`present_side_normalization_issue_holds_the_one_sided_proof` keeps that content
unproven. Settling these requires a normalization proof for those breaks, which
is a separate cause from the one-sided scan; the pair must not be reported
complete until then.

## Controls

- Mirrored deletion and repeated/overlapping present-side text:
  `proven_empty_side_settles_every_block_in_both_directions`,
  `proven_empty_side_settles_repeated_and_overlapping_blocks`.
- Empty side with an extraction issue never completes.
- Present-side normalization issue holds the one-sided proof.
- Hard budget/range limits never report partial completion.
- Both-empty vacuity and extraction-incomplete sides remain outside the
  proven-empty path.
- All three established complete controls stay complete on the full capture.

## Files

- `compare.json` — audited H2 subset comparison against `baseline-scorecard.json`.
- `residual.json` — the three residual candidates with blocks, reasons, glyph
  counts and text.
- Raw capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/h2-iteration-002-native/`.
