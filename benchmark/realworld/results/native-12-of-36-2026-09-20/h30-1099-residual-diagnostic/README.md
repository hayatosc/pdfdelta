# H30 diagnostic: IRS 1099 residual (117 regions) — accepted evidence

Baseline: HEAD da7c4b0. Canonical budget run trace1/trace4: probe rc=0,
`H30 1099 unresolved=117`; EF entry 15,514,428; after pass1 5,549,122
(retry_len=29); after pass2 2,519,247; review entry 2,519,247; 9 units; review
exit remaining 1,676,399. Production was restored byte-exact after every hooked
run; no production change was made.

## Accepted findings (traces 1-6 and the valid parts of trace8)

1. The unresolved union is disjoint: old 58 intervals / 1667 tokens, new 56 /
   1420 (`overlap=false`). Disjoint classes: old 611 Established-covered / 857
   tentative-only / 199 no-projection; new 611 / 654 / 155. The earlier "935"
   was a sum of overlapping review masks, not a union.
2. Tail 226:1296..2085 / 225:1296..1961 (789/665 tokens): 593 tokens per side
   are covered by Established equal domains but remain UNOWNED (unresolved);
   388 of them sit under the five fallback PositionMismatch domains, 52 under
   candidate 717, 153 under the multi-block Projection candidate 782.
   388+52+153 = 593 exactly. No adoption covers the remainder; tentative
   relation 17 covers 196 old / 72 new tokens.
3. All six tail spans (717, 763, 771, 775, 777, 779) hold on PositionMismatch
   with a constant per-scalar delta (0.0, 7.097999999999956) and bit-equal
   directions. In `check_fragment`, PositionMismatch is the FIRST failing guard
   (LiteralMismatch, PageMismatch, position equality, Sharing, breaks); the
   Sharing/breaks guards evaluate OK only in the H30_TRACE6 diagnostic mode,
   which perturbs the shared budget and is therefore not a canonical budget
   measurement.
4. The real trace4 retry path independently shows that these candidates pass
   the latest-ownership, tentative-candidate, proven-region and range-limit
   vetoes; the five fallback holds failed only at PositionMismatch after their
   first-pass RawCut hold, and candidate 717 failed at PositionMismatch in the
   first pass.
5. Two independently established, whole-block, already-owned anchors carry the
   exact same nonzero delta: relation 19 (old 227 [0,47) -> new 226 [0,47)) and
   relation 21 (old 228 [0,160) -> new 227 [0,160)); both `whole=true`,
   `owned=(true,true)`, delta bit-equal to the targets, directions equal.
6. ID mapping by actual projection: candidates 717 / 763 / 771 / 775 / 777 /
   779 map to domains 716 / 762 / 770 / 774 / 776 / 778; candidate 782 maps to
   the multi-block domain 781; candidate 784 maps to domain 783 (blocks 0/1,
   not tail). Candidate 715 projects to [1232,1280) and is unrelated to
   domain 716.

## Retracted or unproven (do not use as evidence)

- Trace8 `candidate_old` and `proven_old` were emitted with an empty reference
  slice: they are PLACEHOLDERS, not measurements. Real candidate/proven/
  changed non-overlap is supported only by trace4's retry veto path.
- Trace8 obstacles scanned only six hardcoded sibling relations, not all
  references; no claim about all-reference crossing is supported.
- Trace8 "before/before" is block-index arithmetic, not geometry or source
  adjacency certification.
- Trace8 `project_side` returned Option and could silently omit projection
  failures; trace4's `?` path remains the loud evidence.
- Trace8 called `ownership_contains`, which charges work, so trace8 is NOT
  budget-neutral; the canonical budget measurement stays trace1/trace4.
- The unselected coverage lists are complements of the six-target projections
  inside their blocks: they include already-resolved prefix tokens, so they are
  NOT all tentative-relation-17 residuals.
- Trace7 candidate delta objects were emitted without a key (format defect);
  the raw archive is historical only.
- Not yet proved: physical/source adjacency between targets and anchors, no
  crossing against ALL references, the status of every intervening unselected
  fragment, and transformed-key uniqueness.

## Narrow candidate design (NOT implemented; requires the missing proofs)

Any production extension must, before adoption: (a) establish an independent,
whole-block, already-owned anchor with an exact finite NONZERO delta bit-equal
to the candidate's constant delta and equal directions; (b) prove source order
and adjacency physically, including every intervening unselected fragment and
every reference, with no crossing and no unknown geometry; (c) pass the
existing global sharing check; (d) run the unchanged ownership/candidate/
proven/range-limit veto sequence; (e) charge all projections and delta checks
deterministically in the existing ordered tail schedule. The existing
whole-singleton `discover_translations` cannot simply be widened to these
partial fragments. No production implementation is included; the missing
proofs remain prerequisites.

## Producer identities and archives

trace1 e1606c42be154aa4d5ab8974113f04bb2f340b48245bc97098c38a2a59f1ca91,
trace2 6de0a61ee08480343ac0310274967cfc48376e583d6908c7a6f988239a9befd8,
trace3 6128cad53d8429332a866078d8de7868a9dec3389864a0264f43287f9c53309a,
trace4 34e9d99f9de6291c58fa9436156916edbfdb4176f9c1215c5ab2e47cfb9ecbc5,
trace5 dc939af1d2601eea4f1870d19e1e44a6c4c42c7939c8daa375edbd6f0da551d4,
trace6 40fa52da06844d5a5f3af1a12b39d8e82be8a057df3217e8bdf7a7882c9ba46e,
trace7 a394b20a0b06ed0b8d606abc7b3bf8b651ec99a1ada7e3b8cf31ba46b3b61772,
trace8 e9cc036d1b2eac687e5a4fa6def641898a0a227247cd1f8140536ed909663b72.
Trace3's silent projection-error arm remains an unverified limitation; traces
4-8 propagate projection errors loudly except where noted above. Raw archives
suspected of format or placeholder defects (trace7, trace8) are kept explicitly
historical; trace8-summary.json.gz records which fields are valid.
