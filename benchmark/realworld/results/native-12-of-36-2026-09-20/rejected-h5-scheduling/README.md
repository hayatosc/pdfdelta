# Rejected: H5 positioned-pass half-share scheduling

Status: rejected candidate. The engine changes were reverted to the accepted
tree; the capture and audits are retained as evidence and the run is pinned
with the rejection reason so rotation cannot remove the W2 proof.

- implementation head: `735bee815d498e301fc0df73c2b48a9cf6aa08b9`
- binary sha256: `ee5b750aa65b12824b3a6e71e46159075ed73d5d28206aede2246744416a7f64`
- production patch sha256: `e0c3766bf4952ed067fcbfc7036353ee6384083c88284c51e8bfa202d91fc2ee`
- capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/h5-final-iteration-005-native`
  (pinned as rejected, never accepted)

## Change under review

The optional positioned-equality pass in `views::discover` previously consumed
the whole remaining work budget, leaving zero for validating the domains it had
already discovered. The candidate capped that pass at half of the remaining
budget when completed domains or anchors existed, charged only actual spend,
and recorded `Discovery.optional_incomplete` so the caller marked the root
search incomplete. Net effect vs the accepted H2 capture, not the frozen
initial baseline whose FAA counters differ: +48,180 old and +48,474 new
resolved tokens and +139 content changes across six pairs.

## Retention findings (H2 -> H5, default limits)

Metrics: `h2-to-h5-table.md`. Verifier: `native_retention_audit.py`, keyed by
side plus stable block identity and both comparable-token and scalar-canonical
coordinates. Results per pair are in `retention/`.

| pair | verdict | prior resolved lost | newly resolved | notes |
| --- | --- | --- | --- | --- |
| irs-w2-2024-to-2025 | **fail** | 1,486 per side | 2,418 | 96 blocks became unresolved on each side, both coordinates |
| edpb-restrictions-v1-to-final | needs-review | 0 | 23,212 / 23,306 | 5 blocks with structural source-event moves at new partition boundaries |
| irs-w9-2018-to-2024 | needs-review | 0 | 15,958 / 15,947 | 16 structural boundary moves |
| nist-sha-1803-to-1804 | needs-review | 0 | 4,255 / 4,255 | 2 structural boundary moves |
| nist-incident-handling-r2-to-r3 | needs-review | 0 | 1,220 / 1,220 | 3 structural boundary moves |
| mext-lower-secondary-japanese-2008-to-2017 | pass | 0 | 2,603 / 2,814 | 230 -> 8,037 regions is per-block refinement; token coverage and glyph provenance conserved |

Every pair preserves its change payloads (`changes`,
`formatting_only_changes`, `proven_changed_regions` exact retention) and every
candidate source glyph remains accounted for. The failures are not payload
losses.

## Minimal W2 mechanism example

`irs-w2-2024-to-2025`, old side, block 6:

| capture | canonical range | comparable range | state |
| --- | --- | --- | --- |
| H2 (`h2-full-iteration-002-native`) | 0..4 | 0..4 | `equal` |
| H5 (`h5-final-iteration-005-native`) | 0..4 | 0..4 | `unresolved` |

The block text is `VOID`. The same move repeats on the new side and across 96
blocks, 1,486 scalar tokens per side, while source events and glyph provenance
are unchanged. The observed regression is therefore a dropped equality, not a
payload loss. Work allocation also moved from `local_views` (H2 19,439,612,
H5 17,504,757) to `localization`/`emission`; because the optional pass batches
its additions and drops pending additions when its allowance is exhausted, the
shortened allowance is the consistent cause hypothesis. No W2-specific
lifecycle trace was run to attribute the loss to a single stage, and this
rejection does not depend on such an attribution.

## Variants measured (targeted W2 + EDPB only)

| variant | W2 prior resolved | W2 verdict | EDPB resolved | early-veto saving |
| --- | --- | --- | --- | --- |
| A: original scheduling (full optional budget) + early-veto | 10,002 preserved | pass | 0 / 0 | 163,848 work on W2 (0.84%), 0 on EDPB |
| B: half-share + early-veto | 1,486 lost | fail | 23,212 / 23,306 | identical counters to H5; cap binds before the scan would exit |

Variant captures: `h5-iteration-006-native-targeted` (A, binary `5250c95d`),
`h5-iteration-007-native-targeted` (B, binary `d1056923`); verifier outputs for
both variants are under `retention/variant-*.json`.

Conclusion: at the shared 32,000,000 work budget the half-share removes
previously accepted positioned equalities; the early-veto optimization does not
recover the difference and saves at most 0.84% in the measured pairs. A net
higher resolved count is therefore not an acceptable trade. The branch is
closed; no further half-share tuning.

## Follow-ups

- The early-veto optimization (stop the positioned scan once the occurrence
  verdict is irreversible) was measured, shown to be behavior-neutral but of no
  material benefit here, and reverted with this rejection.
- Candidate captures A/B are disposable; `h5-full-iteration-004-native` stays
  failed with `INTERRUPTED.md`.
