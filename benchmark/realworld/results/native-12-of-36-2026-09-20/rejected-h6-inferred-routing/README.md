# Rejected: H6 untrusted-known-order routing

Status: rejected candidate. The engine change was reverted; the capture is
pinned as rejected evidence (`h6-iteration-001-native`, binary
`50fe51e73e9d`). No production change was committed.

## Hypothesis (rejected at source level)

`irs-schedule-c` leaves 19 unresolved regions and a 301-token gap while work
is not exhausted (27.4M of 32M). A temporary source-backed diagnostic showed
the affected blocks are outside trusted runs but have complete single-page
position signatures and no normalization issues, and that
`prepare_with_diagnostics` grouped every `LayoutIssue::UnknownReadingOrder`
reason, including `UntrustedLinesInKnownOrder`, into `uncertain_block_indices`.
A candidate routed those lines into the inferred-order set so they stayed
anchor-eligible.

The routing is not justified by the source contract. `region.rs` documents
`UntrustedLinesInKnownOrder` as "a known region order containing lines outside
the trusted runs; only those lines stay uncertain", and `TrustedLineRun` states
that "relative order between separate runs is unknown". The classification
branch for a region-level `Known` order explicitly falls back to
`ReadingOrder::Unknown` while applying that tag, because block reconstruction
resolves each region id against the raw, unfiltered partition. A per-line
deterministic order is therefore not established for these lines, and no
fixture proves one. Promoting them would relax the order obligation without
evidence, so the candidate is rejected on that ground as well as on its
measured effects.

Falsifiable prediction (tested and failed): routing would reduce the Schedule C
gap without changing established changes, leaving SE/W2/1099/EDPB unchanged.

## Result

Targeted capture `h6-iteration-001-native` (default limits, compressed at
creation) against the accepted H2 reference:

| pair | H2 resolved old/new | candidate | H2 unresolved | candidate | H2 changes | candidate |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| irs-schedule-c | 6,619 / 6,634 | 6,620 / 6,635 | 19 | 11 | 11 | 11 |
| irs-schedule-se | 5,485 / 5,502 | identical | 0 | 0 | 12 | 12 |
| irs-w2 | 10,002 / 10,002 | 8,466 / 8,466 | 403 | 337 | 13 | 13 |
| irs-1099-misc | 8,228 / 8,224 | 8,092 / 8,088 | 110 | 68 | 12 | **4** |
| edpb-restrictions | 0 / 0 | 0 / 0 | 1,234 | 1,234 | 0 | 0 |

The routing costs 1,536 resolved tokens per side on W2 and 136 per side plus
eight established changes on 1099 while gaining one token per side on Schedule
C. Region counts fall (19 -> 11, 403 -> 337) even where token coverage drops,
so unresolved-region counts are not coverage and cannot be used as a gain
signal on their own.

## Schedule C before/after structure

Routing did shrink the Schedule C residual to 11 regions and two tentative
candidates, and removed `unknown_reading_order` from the reasons. The moved
rows then surfaced as the real blockers instead: old block 126
`27 a Other expenses (from line 48)` against new block 126
`27 a Energy efficient commercial b`, old block 129 `b Energy efficient...`
against new block 129 `deduction (attach Form 7205)`, and old block 133
`deduction...` against new block 133 `b Other expenses...`, each marked
`text_similarity` plus `candidate_competition` and `reading_order_inferred`.
The remaining two tentative candidates are `ambiguous_edit_location`
replacements on block 228 (U+2013 -> `-` and `6, line 2` -> empty).

## Decision

Rejected: the promotion lacks per-line order evidence in the source contract
and the measured trade is strictly worse; losing established changes on 1099 is
disqualifying. Both temporary diagnostics were removed and the engine tree is
unchanged (no production commit). The next H6 cause keeps the original order
obligations and looks for source-specific local equality or closure: the
moved-row candidate competition and the block 228 `ambiguous_edit_location` /
`numeric_mask` residuals.

## Schedule C residual closure

After the routing rejection, the original-order residual was re-inspected
without any engine change. The two `ambiguous_edit_location` candidates on
block 228 (`–` -> `-` at 68..69 and `6, line 2` -> empty at 70..79) belong to
`alternative_group` 2 and 3 with `search: complete`, and the moved-row
regions carry `text_similarity` plus `candidate_competition` with complete
searches: old block 126 `27 a Other expenses` matches either new block 126
(same label, different text) or new block 133 (same text, different label),
and both decompositions are exact. The residual is contract-level edit
ambiguity (moves and repeated numeric contexts), not exhausted search, so H6
closes without an engine change. If a future iteration adds move semantics or
a canonical atomic-edit choice, this evidence is the starting point.
