# H3 investigation: punctuation-boundary line breaks hold the one-sided proof

Status: investigated and **deferred because the required proof was not
established**, not because a user decision is required. The token-invariance
argument presented below does not prove that joining is impossible in the
source, and an assumption-based exemption would not satisfy the exactness
contract, so no production change was made. A future attempt is free to find a
sound normalization proof from additional source evidence.

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

## Rejected argument (do not treat as proof)

An argument was considered and **rejected**: for a break that is not
word-internal, the retained line separator and a space would yield the same
comparable tokens, so the uncertainty might be only representational. That
argument does not prove the source cannot have joined the tokens (for example
a grouped number or an identifier written without a separator), and it applies
equally to the pinned comma fixture; discharging the veto on it would not have
been exactness. Joining was not ruled out, so no rule was implemented. The FAA
residual therefore has no proven normalization rule yet:

- any rule must show from source evidence which separator alternative is the
  faithful one; the current evidence cannot distinguish a soft wrap from a
  hard break or a word join, so the conservative veto stays;
- an explicit assumption that merely *accepts* the ambiguity would not satisfy
  the exactness contract and was not pursued;
- the 3 blocks therefore stay unresolved and the next cause was selected from
  the remaining ranking.

## Next largest cause

The baseline ranking still has the larger cause: eleven two-sided pairs whose
reading-order-unknown windows are excluded wholesale and whose candidate index
collapses to zero (`candidate_visits == 0`; edpb-controller/design/restrictions,
mext x3, nist-ai-rmf/authentication/contingency/risk-assessment,
bunka-official-writing). Advancing them requires a sound way to order or
isolate trusted runs under unproven inter-region order.
