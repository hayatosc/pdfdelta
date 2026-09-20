# Rejected experiment: remove reading-order window forcing (H4)

Status: tested and rejected; no measured gain on any named beneficiary. The
working tree change was removed after the measurement; the candidate patch and
its capture are preserved here.

## Hypothesis and named evidence

Named pair: `edpb-restrictions-v1-to-final`. Instrumented plan evidence on the
frozen inputs (`--native-text-only`, default limits):

- old 418 / new 394 blocks; `all_anchors = 0`, `main_anchors = 0` -> exactly
  one anchor window covering blocks `0..418` / `0..394`;
- the window is forced by reading-order uncertainty;
- only 46 old / 56 new blocks are reading-order uncertain, while the window
  contains 372 trusted old and 338 trusted new blocks;
- because the whole window is excluded, `indexed_features = 0` and
  `candidate_visits = 0`.

Hypothesis: excluding uncertain blocks from accepted matches and no longer
forcing whole windows would let the trusted subsequence (whose relative order
is proven by page order plus per-page proven order) align monotonically around
the uncertain blocks, without asserting any uncertain block's position.

Mechanism tested (foundation plus addition):

- reuse the barrier post-pass that keeps uncertain blocks out of accepted
  matches while leaving them in candidate generation for competition;
- additionally stop forcing windows for reading-order uncertainty (only
  extraction gaps force), and mark every block of an unproven-order partition
  unmatchable rather than only the off-run lines.

Discriminating fixture (working tree during the experiment): a proven-order
two-column page with small edits plus an unproven-order two-column page. Before
the change `indexed_features = 0`, `candidate_visits = 0`, one span; after the
change `indexed_features = 10`, `candidate_visits = 656`, the proven page's
blocks matched, and the unproven page's blocks stayed unresolved with
`ReadingOrderUnknown`. That confirms the alignment-level mechanism, but the
fixture's fallback assessment already recovered ~0.54 coverage, so it was not a
coverage red.

## Measured result (default settings, frozen inputs)

Candidate binary SHA-256
`d36d4e6a7f532b0753030f23de66041ceb179f960165350ab97da893373f5c35`, patch
SHA-256 `c3959a556102c6d5d83a6c7cd185687312f1fda434a7dec950e20e0b784b1762`.

| pair | baseline | candidate | delta |
| --- | --- | --- | --- |
| `edpb-restrictions-v1-to-final` | incomplete, 0 resolved, 1234 unresolved | incomplete, 0 resolved, 1234 unresolved | none |
| `edpb-design-default-v1-to-v2` | incomplete | incomplete | none |
| `irs-schedule-se-2024-to-2025` | complete | complete | none |
| `irs-schedule-c-2024-to-2025` | incomplete | incomplete | none |
| `faa-thunderstorms-b-to-c` | complete | complete | none |
| `bunka-kana-1946-to-1986` | complete | complete | none |
| `nist-ai-rmf-draft2-to-v1` | incomplete | incomplete | none |

## Why it failed: observed per-candidate blockers

On the candidate capture, `edpb-restrictions` still has 0 resolved tokens and
1234 unresolved regions. The first ~10 MB of the candidate member (all 110
candidates) carries these reason labels approximately per candidate group:
`unknown_reading_order` 50, `inferred_reading_order` 50,
`normalization_uncertainty` 50, `domain_not_closed` 50, and `work_limit` 1;
`ambiguous_edit_location`, `competing_correspondence` and
`source_evidence_missing` do not occur.

So making the window compareable does not clear the assessment veto: the root
relation still carries unknown/inferred reading-order reasons, and the local
domain closure does not accept these source-bounded trusted spans, so
`domain_not_closed` keeps them tentative. The blocker is the assessment-level
order/closure contract, not candidate discovery; removing window forcing alone
cannot produce content coverage.

## Preserved files

- `h4-order-forcing.patch` — rejected candidate diff (not applied).
- `compare.json` — audited per-pair comparison against `baseline-scorecard.json`.
- Raw capture: `benchmark/realworld/cache/native-12-of-36-2026-09-20/h4-iteration-003-native/`.

## Next mechanism (candidate H5)

The observed reasons suggest clearing the reading-order barrier only for
relations whose spans are source-bounded and every one of whose blocks comes
from a proven order (no uncertain block inside the span), leaving the barrier
in force everywhere else. That is the same per-domain exemption that local
domains already use, extended to trusted alignment spans; it would need a
programmatic fixture with an uncertain block present as a competitor and a
negative case where a span touches an uncertain block.
