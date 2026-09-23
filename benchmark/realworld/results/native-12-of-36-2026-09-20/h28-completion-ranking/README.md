# H28 completion ranking (full013)

Source: capture `h27-native-order-full-iteration-013-native`, HEAD `7592503`,
binary `6686286e`, production diff `6fc41176`. Complete ranking for all 33
incomplete pairs is in `ranking.json`; this file records the decision.

## Predicate failures and extraction prerequisites

`comparison_complete` is false for every listed pair. Pairs whose extraction is
incomplete cannot be completed by alignment or order work:

- `arxiv-ddpm-v1-to-v2`: 2 unresolved extraction issues, both sides
  incomplete ("glyph relationship to convex clipping region is uncertain"),
  ~68k uncovered tokens. 115/122 unresolved regions are two-sided, but the
  extraction prerequisite is missing.
- `arxiv-faster-rcnn-v1-to-v3`: intrinsic extraction refusals (unsupported
  clipping plus unresolved issues).
- `ipcc-synthesis-ar5-to-ar6` (totals 615835/639893) and
  `mhlw-care-skills-original-to-revised` (totals 82214/99828) resolve zero
  tokens despite nonzero validated inventories; their blockers are extraction
  refusals plus comparison, not absent native text or OCR.

Fully extracted candidates, ranked by proximity and cause family:

| pair | unresolved | anatomy | cause (hypothesis until traced) |
| --- | ---: | --- | --- |
| irs-schedule-c-2024-to-2025 | 19 | 9 old-only, 9 new-only, 1 two-sided | H6 double obstacle (order promotion + competing edit decompositions) |
| irs-1099-misc-2024-to-2025 | 110 | 55/54 one-sided, 1 two-sided (0..2085/0..1961) | one-sided residual (hypothesis) |
| irs-w2-2024-to-2025 | 212 | 104/104 one-sided, 4 two-sided | one-sided residual (hypothesis) |
| nist-incident-handling-r2-to-r3 | 5237 | 3307 old-only, 1913 new-only, 17 two-sided | one-sided residual (hypothesis) |
| faa-maintenance-records-c-to-d | 3 | 3 new-only | H3 source alternative; H18 sampled only 2 NASA + 3 FAA blocks |
| nasa-buckling-8007-1968-to-2020 | 29 | 29 new-only | same family as FAA; old side has no native text |

## Decision

The DDPM hypothesis is falsified: its blocker is an extraction prerequisite,
not a local alignment premise. The chosen next measurement is `irs-w2`:
bind one old-only and one new-only unresolved region to exact blocks and trace
the guard that leaves a one-sided region unresolved instead of proving an
insertion or deletion. A sound family fix would also benefit
`irs-1099-misc` and possibly `irs-schedule-c`, subject to H6's edit
decomposition obstacle.

Representative exact ranges: ddpm 0..55/0..55 and 0..38/0..38; w2 0..28/0..28
and 0..25/0..25; 1099 0..2085/0..1961; schedule-c 0..36/0..39.

## Prior rejections still in force

- H3: joined-versus-split remains a source alternative for FAA/NASA.
- H6: unsupported order promotion plus competing edit decompositions
  (moved rows 126/129/133, block 228).
- H18: only a small sample was checked (incomplete NASA inventory, 366
  issues); it established no sufficient certificate in that sample, not an
  absence of one.

## Acceptance for any future production candidate

No prior-resolved loss, established changes preserved, gains audited and
explained, a full36 capture against the accepted baseline, all seven gates,
and no production trust or predicate change before the measured mechanism
exists.

## Measured W2 binding (temporary instrumentation, restored)

Emission anatomy (transient, later resolved by assessment): siteE
sub-range remainders 58, siteD whole-block remainders 22, siteB alignment
unresolved 6, siteA atomic unresolved 5. Final residual distribution is
dominated by single-token one-sided regions: 58 of 212 (`1b/1t`), then 24
`1b/4t` and 14 `1b/5t`.

Representative final residuals bind to block 1 on both sides:
- comparable `[380,381)`: old and new token both `Scalar('\n')`, context
  `rds.\nNote`, occurrences 38 in new and 37 in old;
- comparable `[508,509)`: old and new token both `Scalar(' ')`, context
  `orm. The `, occurrences 3207 in new and 3198 in old.

Bound relations show established coverage `[0,380)`, `[381,452)`,
`[452,508)`, `[509,633)` on both sides at equal offsets, leaving exactly the
two separator tokens uncovered; tentative duplicates of the same ranges carry
`unknown_reading_order`, `normalization_uncertainty`, and `domain_not_closed`.
The tokens are therefore not shown to be insertions or deletions, and their
frequency alone does not make them ambiguous. The observed missing proof is
that no relation is emitted for the separator between two established
neighbours.

Next measurement: for the 58 single-token gaps, assert both neighbours are
established at equal offsets and that the gap token's raw-source atoms and
normalization boundaries match on both sides, with non-overlap and
source-order checks. Counterexample: a gap whose neighbour is not established
or whose normalization/endpoints differ must stay unresolved. Evidence:
`w2-trace.txt.gz`, `w2-site-counts.txt`, `w2-gap-diagnostic.log.gz`,
`w2-bound-relations.json`, `w2-binding-meta.json`.

## W2 census retraction (invalid diagnostic)

Both the provisional 37 count and the strict-join zero result are invalid: the
strict join compared relation group coordinates with single-block local
offsets and selected relations without projection or an equality signature.
The zero result is not a falsification.

The atom census (canonical vs raw classes, endpoints, events) remains valid
and shows 30 of 58 residuals are canonical newlines with line-break atoms.
Whether that class is provable is still open: the next probe must first project
relation spans with the core projection and establish equality from the
internal proof signature, asserting the known mapping [blocks 0,1] group
11..391 -> block1 local 0..380 before classifying residuals.

## Projection probe attempt (failed prerequisite, no verdict)

Reconstructing relation spans from the report JSON and calling the core
projection fails at the representative step with
`InvalidConfiguration("assessment source coordinates do not agree")`: the JSON
lacks the internal per-side separator and canonical/comparable agreement that
the projection requires. The probe source, visibility patch, relations dump,
census, and failure log are archived under `probes/`. No verdict is drawn. The
next probe must hook inside the Assessor where the real `TextSpan` and
`DomainProof` (edits, unique, strict_unique, search) are available.

## v3 measured conclusion (accepted as a limited diagnostic)

The real native path is unchanged (212 unresolved, 13 changes). Both gaps are
unowned and have zero candidate/proven overlap on both sides; the four
neighbours block1 [0,380), [381,452), [452,508), [509,633) are accepted on both
sides with no changed ownership. In the diagnostic-only cache with a separate
32,000,000 budget: gap380 holds on CanonicalSource (spent 3,862) because the
line-break separator has a canonical LineBreak source but EqualFragment
currently requires real Glyph atoms, an unsupported atom shape rather than
absent source evidence; gap508 is Proven (spent 1,537,858); container1080
holds on RawCut (spent 16,127). Production remaining_work is zero at the hook.

This is not a benchmark gain and does not authorize raising production
budgets. Earlier projection and strict-join probes remain in this directory as
superseded, invalid history.
