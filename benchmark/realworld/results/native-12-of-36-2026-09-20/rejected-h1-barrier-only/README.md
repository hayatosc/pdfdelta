# Rejected experiment: known-order barrier recovery (H1 barrier-only)

Status: tested and rejected as having no demonstrated panel benefit. The
broader H1 inter-region recovery hypothesis is **not** globally falsified.

## What was tested

A candidate patch split reading-order uncertainty into two scopes:

- blocks touching a line outside a **proven** line order (`KnownLines`
  classification) became barriers: excluded from accepted matches but no longer
  forcing their whole anchor window unresolved;
- blocks under an **unproven** order (inter-region unknown, render disorder,
  region-level fallback) kept the existing whole-window forced behavior;
- a span post-pass carved barrier runs out of decided spans and reported them
  as unresolved with `ReadingOrderUnknown`, keeping barrier rivals in candidate
  generation and the index.

## Measured result (frozen inputs, default settings)

Candidate binary SHA-256
`9f3396df7e94856aec0acf511be5a593a3fa3bc435bc29ee2abf2a715030912b`, patch
SHA-256 `58c6d7af2751141a927df7d989ffe9d452a038ad847e01ab9f9e95d46de79bb4`,
source revision `3bfbc880a4774d343c54308c2b9f4f77d8650610`.

Capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/h1-iteration-001-native/`
(`summary.json` binds binary, production patch, source archive, panel and every
report hash). Audit: `compare.json` recomputes the completion predicate from
each raw report and verifies its recorded hash.

All 13 measured pairs show **identical measured scoreboard fields** to the
audited baseline: identical completion, resolved old/new tokens, unresolved
regions, tentative candidates and established changes.

| pair | baseline complete | candidate complete | resolved-token delta |
| --- | --- | --- | --- |
| `irs-schedule-se-2024-to-2025` | yes | yes | 0 |
| `irs-schedule-c-2024-to-2025` | no | no | 0 |
| `irs-w4-english-2024-to-2025` | no | no | 0 |
| `edpb-restrictions-v1-to-final` | no | no | 0 |
| `edpb-design-default-v1-to-v2` | no | no | 0 |
| `irs-1099-misc-2024-to-2025` | no | no | 0 |
| `faa-thunderstorms-b-to-c` | yes | yes | 0 |
| `faa-maintenance-records-c-to-d` | no | no | 0 |
| `bunka-kana-1946-to-1986` | yes | yes | 0 |
| `mext-upper-secondary-japanese-2009-to-2018` | no | no | 0 |
| `nist-risk-assessment-30-to-r1` | no | no | 0 |
| `mext-primary-japanese-2008-to-2017` | no | no | 0 |
| `nist-contingency-34-to-r1` | no | no | 0 |

## Interpretation

Measured zero benefit: no completion, coverage, candidate or change field moved
on any of the 13 pairs, with the same binary, inputs, arguments and defaults.
The candidate patch was removed.

Why the patch had no effect is an **unproven hypothesis**: the barrier branch it
acts on (`order_proven = true`, the `KnownLines` classification with off-order
lines) may not be reached on these pairs, so no window is released; the
unchanged score fields alone do not prove which classification branch occurred.
The inter-region collapse (eleven two-sided pairs with `candidate_visits == 0`)
remains an open, unresolved cause and was not globally falsified; a future
attempt needs a sound way to order or isolate trusted runs under unproven
inter-region order, not a barrier-only split.

The patch file `h1-barrier-only.patch` is retained for reference; it is not
part of the working tree. A diagnostic trace run under the patch's temporary
`PDFDELTA_H1_DEBUG` instrumentation hung on `irs-schedule-c`; the instrumentation
was removed before the authoritative capture, and the hang is not a property of
the retained binary.

## Files

- `h1-barrier-only.patch` — rejected candidate diff (SHA-256 above).
- `compare.json` — audited per-pair comparison against `baseline-scorecard.json`.
- Raw capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/h1-iteration-001-native/`.
