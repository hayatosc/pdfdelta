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
- `ipcc-synthesis-ar5-to-ar6`, `mhlw-care-skills-original-to-revised`: both
  sides resolve zero tokens (no native text; OCR is out of scope).

Fully extracted candidates, ranked by proximity and cause family:

| pair | unresolved | anatomy | cause |
| --- | ---: | --- | --- |
| irs-schedule-c-2024-to-2025 | 19 | 9 old-only, 9 new-only, 1 two-sided | H6 double obstacle (order promotion + competing edit decompositions) |
| irs-1099-misc-2024-to-2025 | 110 | 55/54 one-sided, 1 two-sided (0..2085/0..1961) | one-sided ownership |
| irs-w2-2024-to-2025 | 212 | 104/104 one-sided, 4 two-sided | one-sided ownership |
| nist-incident-handling-r2-to-r3 | 5237 | 3307 old-only, 1913 new-only, 17 two-sided | one-sided ownership |
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
