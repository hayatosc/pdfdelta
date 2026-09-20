# H8 trace: wide-domain boundary proof starves later relations (W4)

Status: causal trace complete; no engine change yet. Temporary per-proposal and
per-substep diagnostics were removed after the run.

## Evidence

Targeted native run on `irs-w4-english-2024-to-2025` with the accepted engine
plus a temporary file-sink diagnostic (`PDFDELTA_H8_DEBUG`, no stderr lock).

Per-proposal work (`per-proposal-work.txt`): 320 proposals, 30683053 units of
localization budget after anchors.

- Proposal 181 (single block, 16 tokens per side) spent **15,722,141** units
  before/after its assessment and ended `Tentative` / `Incomplete` /
  `[WorkLimit]`, leaving `remaining_work = 0`.
- The 139 proposals after it had `before = 0`, spent 0 and were all
  `WorkLimit`.
- Several tiny proposals are also expensive: proposals 119-124 (single block,
  5 tokens) spent about **508,613-1,111,657** units each and stayed
  `AmbiguousEditLocation`.

Per-substep work (`substep-work.txt`) for the top consumer:

| call | domain_key | prove_domain | extents | boundary proof | after |
| --- | ---: | ---: | --- | ---: | ---: |
| 181 | 15,722,141 | 12,132,996 | old 143 / new 143 blocks | consumed 12,132,996, returned Budget | 0 |
| 119 | 29,230,456 | 29,136,025 | old 25 / new 25 blocks | consumed 508,613 | ambiguous |

## Causal chain

`Assessor::assess` attempts `boundary_displacement_proof` for a proposal whose
closed domain is wide (`key.old.len() > 1 && key.new.len() > 1`), even when the
proposal itself is one block. The proof builds canonical groups for the whole
domain and runs `semantic::check_hunks` over them. For proposal 181 the domain
is 143 blocks wide, the proof consumed the entire remaining budget and returned
`BoundaryDisplacement::Budget`, which records `WorkLimit`/`Incomplete` and
zeroes the shared budget, starving every later relation. The original result
for that proposal was not accepted, so the consumed budget produced no output.

## Candidate designs (to test with fixtures, not unconditional edits)

1. Bound the optional boundary proof: stop it before it can consume the whole
   shared remainder, and return `NotProven` instead of `Budget` so later
   relations keep the remainder. Must be charged honestly and must not change
   any accepted result of the truncated relation.
2. Assess bounded relations before wide-domain ones while preserving output
   order, so the 139 starved proposals complete first. Must show no accepted
   result is lost on the same fixtures.

Both need a meaningful regression with a negative case (wide-domain proof that
would succeed must not be silently dropped) and targeted verification on W4,
Schedule C, SE, W2 and 1099 with payload retention checked.

## Preflight hypothesis tested (disproven for W4)

The fatal attempt built a suffix table of 1021 x 1021 = 1,042,441 interior
cells while 12,132,996 units remained, so the `build_suffix` preflight was
affordable. The enumeration then consumed the remaining ~11.1M and returned
`BudgetExceeded`. The cost is real wide-domain path enumeration, not an
oversized preflight debit that zeroes the shared budget without doing work, so
a minimal "retain remainder on an unaffordable preflight" change cannot fix
this case and was reverted.

## Cross-pair boundary outcomes

Raw traces are under `traces/`.

- W4: 79 attempts, 52 `NotProven`, 23 `Budget`, 4 `Unavailable`, 0 `Proven`,
  and all 79 were strict subspans of their domain.
- Schedule C: 14 attempts, 13 `NotProven`, 1 `Proven` (a subspan attempt).
- Schedule SE: 5 attempts, 3 `NotProven`, 2 `Proven` (subspan attempts).

A blanket skip of subspan boundary proofs would drop real promotions on C and
SE, so it is not proof-preserving. The next candidate must keep those
promotions: defer wide-domain enumeration proofs until after the ordinary
relation pass (with index remapping so output order is unchanged), or bound
the enumeration from measured evidence.

## Decisive-negative veto tested (no gain)

The boundary proof and the proposal-invariant check both require every optimal
path to produce the same nonempty cut, so one fully examined path without a cut
is a decisive negative. A typed `PathVerdict`/`check_hunks_decisive` path was
implemented with tests (first-invalid stops the traversal; late invalid still
vetoes; callback budget stays `BudgetExceeded`; decisive positives keep the
canonical witness) and the generic `check_hunks` callers kept ordinary
equality semantics. The five-pair compressed capture
(`h10-iteration-002-native`, binary `da35867b570c`) is metric-identical to H2
on W4, Schedule C, SE, W2 and 1099. The W4 fatal attempt ends in
`BudgetExceeded` before any invalid path is examined, so the veto cannot
shorten it. The change was reverted and the capture pinned as rejected.

