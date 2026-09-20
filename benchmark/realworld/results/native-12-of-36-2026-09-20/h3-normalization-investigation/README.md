# H3 investigation: punctuation-boundary line breaks hold the one-sided proof

Status: investigated; **FAA maintenance cannot complete under the preserved
contract**. A contract decision is required before any implementation. No
production change was made.

## Residual

`faa-maintenance-records-c-to-d` after the accepted H2 fix still has 3
unresolved insertion blocks (509, 512, 547; 950 glyphs / ~972 tokens) with
`normalization_uncertainty` plus `domain_not_closed`, `search: complete`, and
2.12M of the 32M assessment budget used.

- block 509: `2. Compliance with Airworthiness Directives (AD) or SBs.\n3. ...`
- block 512: `9. Deviations from the customer work order.\n10. ...`
- block 547: `... The use or omission of slashes,\nhyphens, or spaces ...`

## Failed invariant and source path

`resolve_line_breaks` (`crates/pdfdelta-core/src/normalize/mod.rs`) handles
soft hyphens, lexical and ambiguous hyphenation, digit-digit breaks,
whitespace and CJK boundaries, and Latin-Latin breaks. Its final `else` keeps
the break atom and records `NormalizationIssueKind::AmbiguousLineBreak`, which
is the only issue kind. `BlockText::has_normalization_issues` therefore turns
on for the block, and `Assessor::domain_reasons`
(`crates/pdfdelta-core/src/diff/assessment.rs`) adds
`AssessmentReason::NormalizationUncertainty`, so the one-sided relation stays
tentative and the region unresolved.

The only existing lift is the paired whole-block raw-source equality
(`raw_source_equalities`): both sides must share the exact raw text. A
proven-empty comparison has no counterpart, so this proof can never apply to
these insertions.

## Reproduced shapes (temporary fixture, removed after measurement)

A programmatic `Document<Glyph>` one-sided fixture with two break shapes from
the residual was run against the current pipeline:

| shape | result |
| --- | --- |
| `Obey the manual. 2. Record the repair. 3. Keep the log.` | 1 block, 1 unresolved region, 1 candidate, `comparison_complete = false` |
| `The use or omission of slashes, hyphens, or spaces does not matter.` | 1 block, 1 unresolved region, 1 candidate, `comparison_complete = false` |

The existing pinned fixture `present_side_normalization_issue_holds_the_one_sided_proof`
covers the same comma shape and asserts the same conservative outcome.

## Why a token proof is available but not sufficient

For a break that is not word-internal (no soft or lexical hyphen, the scalars
around the break are not both letters/digits), the admissible normalizations
are the retained line separator or a space; both yield the same comparable
token sequence, while joining without a separator would merge adjacent tokens
and no existing rule produces it. The uncertainty is therefore
representational (separator form), not token-level.

However, that property holds equally for the pinned comma fixture, and
discharging it would weaken the one-sided normalization veto that the run
instruction requires preserving. The FAA residual therefore blocks on a
contract decision, not on a missing local proof:

- **Option A (contract change):** accept token-invariant ambiguous breaks in
  one-sided relations with an explicit assumption (for example a new
  `AmbiguousBreakNormalization` variant recorded on the relation), keep the
  canonical separator deterministic, and update
  `present_side_normalization_issue_holds_the_one_sided_proof` to the new
  expected behavior. This is the only route that can finish FAA.
- **Option B (preserve the veto):** the 3 blocks stay unresolved; FAA remains
  at 3 residual blocks, and the next cause should be selected elsewhere.

No implementation was attempted because Option A is a contract decision and
Option B changes nothing; neither is a unilateral production edit.

## Next largest cause

The baseline ranking still has the larger cause: eleven two-sided pairs whose
reading-order-unknown windows are excluded wholesale and whose candidate index
collapses to zero (`candidate_visits == 0`; edpb-controller/design/restrictions,
mext x3, nist-ai-rmf/authentication/contingency/risk-assessment,
bunka-official-writing). Advancing them requires a sound way to order or
isolate trusted runs under unproven inter-region order.
