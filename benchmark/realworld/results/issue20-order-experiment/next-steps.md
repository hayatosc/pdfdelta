# Diagnosis and proposed next steps

Status: the implementation proposal below is on hold. The diagnosis identifies
specific failure mechanisms, but does not establish that fixing them would yield
sufficient practical improvement. Further investment in the current approach is
not recommended without new evidence. No scope change or follow-up implementation
has been approved. See the [current decision](README.md#current-decision).

The earlier proposal concerned comparison-domain construction, change-event
grouping, and bounded uncertainty handling rather than another reading-order
model. It is retained for reference, not as the active development plan.

## What the measurements establish

1. The local trial added 765 accepted events but recovered no additional reviewed
   expectation. A mathematically stable character fragment is not necessarily the
   complete change unit required by the benchmark.
2. The order-only controls retained normalization uncertainty in 10 of 12 pairs.
   This is a count of observed barriers, not proof that fixing normalization alone
   would recover ten pairs. Multiple barriers can affect the same expectation.
3. Event-level recall can conceal correct character-level work. In the Attention
   distribution-stamp scope, the native-order control reports all 11 expected
   changed tokens with zero false-positive tokens. It emits two replacements for
   the version and date, while the annotation requires one replacement for the
   complete stamp. The expected-change diagnostic nevertheless falls back to
   `alignment_or_candidate`. This is a concrete reporting and event-granularity
   problem, rather than missing changed characters in that control.
4. The BIS trial still retains accepted results: 70 events become 119. Its
   assessment work increases from 28,419,718 to 31,368,382 against a 32,000,000
   limit; a subsequent requested charge does not fit. The `limit` status reflects
   incomplete search, not a crash or total loss of the comparison. Work attributed
   to emission includes local recovery and proof work; it is not evidence of slow
   report serialization.
5. Eight of the 29 production-baseline pairs have incomplete extraction and an
   `unsupported` status. Improving reading order cannot by itself resolve these
   input-support failures.

Evidence: [final order-control results](order-summary-final.json),
[Attention's detailed control](order-controls-source-fixed/arxiv-attention-v6-to-v7.json),
[local-trial comparison](local-trial/comparison.json), and
[all baseline outcomes](baseline-fixed/full-summary.json).

## Why the current implementation produces these results

### Comparison domains carry unresolved normalization decisions

`NormalizationIssueKind` currently contains `AmbiguousLineBreak`. The normalizer
records uncertainty around ambiguous hyphenation and unclassified line boundaries
in `normalize/mod.rs:1700`. This is not equivalent to every character being
unreadable.

The assessor collects source-level reasons, then attempts to isolate domains from
unrelated issues in `diff/assessment.rs:2216`. Source-range issue checks already
exist at `assessment.rs:507`. The missing capability is therefore not simply
"add local checks": when a sufficiently closed local domain cannot be established,
the larger domain can still intersect an uncertain boundary and remain tentative.
`prove_domain` skips edit-uniqueness checking while such reasons remain.

Local-domain discovery also requires independently verified anchors in trusted
reading-order runs (`assessment/views.rs:76`). The prior stable-hunk trial runs
after this domain machinery. It cannot recover an expectation that never reaches
a suitable domain merely by finding more stable internal edits.

### Edit location and useful change units differ

`assessment.rs:2472` first checks exact edit uniqueness, then uses
`assessment/semantic.rs:36` to compare semantic signatures across optimal
equal-token pairings. Ambiguity can remain even with the correct reading order:
repeated characters and whitespace can support multiple optimal locations.

The prior trial's `ri` -> `rvi` fragment illustrates the limitation. Recovering
the stable `v` does not by itself recover the surrounding reviewed replacement.
Conversely, Attention demonstrates that two accurate atomic replacements can
fail a single-event expectation. These are different failure modes and should
not share a generic "alignment failed" diagnosis.

Do not solve this by accepting an arbitrary optimal path as proven source
history, merging every nearby edit, or counting every character in a display
context as changed. Exact changed-token masks and source provenance must remain
separate from the enclosing review unit.

### Partial recovery interacts with unresolved-region reporting

Domain correspondence and edit localization are already separate internal claims.
However, `assessment.rs:1858` only emits a whole `ProvenChangedRegion` while its
source remains entirely unresolved. Validation explicitly requires that ownership
state (`assessment/validation.rs:290`). A resolved child can therefore make the
old whole-region output inapplicable.

That does not justify copying the original change proof onto the remainder:
the resolved child may explain the entire difference. Reuse the existing domain
relations as enclosing context, preserve exclusive token ownership, and distinguish
"this enclosing unit differs" from "this unresolved remainder contains another
change." Representation changes should follow a concrete failing fixture.

## Limits of the model experiment

The controls change native block order only. They retain the existing canonical
text and block segmentation and do not run production local sentence recovery.
They therefore isolate a limited integration path; their absolute recall cannot
be compared directly with production recall.

All 24 documents changed order, but 5,912 of 37,713 blocks needed a fallback.
The model was not allowed to repair incorrectly combined or split native blocks.
The only expected-ID category change was an accepted W3C URL replacement becoming
a candidate. Accepted-or-candidate coverage remained the same 11 expectations.
These results show no benefit from this integration; they do not establish that
learned region proposals or a different segmentation integration would be useless.

## Recommended sequence

### 1. Diagnose a small set of existing failures before another corpus-wide trial

Use the existing artifacts and minimal source-derived fixtures for Attention,
FIPS, IRS, and BIS. Record separately for each reviewed change:

- Source extraction and the precise uncertain boundary, if any.
- Whether the expected old/new source region appears among candidates.
- Whether correspondence is established and where its domain ends.
- Whether changed-token masks are correct.
- Whether kind, event count, and enclosing source spans match the expected unit.
- Whether required comparison work or optional recovery stopped.

Use manually verified order, region pairing, or normalization only as clearly
labeled diagnostic controls when the existing evidence cannot distinguish two
causes. Change one supplied fact at a time and retain all other pipeline behavior.
These controls estimate what a missing stage could unlock; they are not deployable
accuracy gains and must not leak expected quotes into production discovery.

The first downstream fixture should be Attention's stamp: one logical replacement,
two disjoint exact changed spans, all 11 expected tokens, zero unchanged-token FP.
The current successful character result is from the order-assumed control, so
production recovery must be demonstrated separately before claiming an end-to-end
gain.

### 2. Fix the demonstrated boundary, preserving existing contracts

For event assembly, define a source-backed review unit with an exact internal
changed-token mask. Permit one replacement event to contain multiple changes
within that established unit. Do not equate a parser block with a guaranteed
paragraph or merge independent neighboring changes just to match annotations.

For domain construction, preserve alternative line/block boundaries and isolate
normalization issues using the existing source-range machinery. Resolve a boundary
only with adequate source or comparison evidence; otherwise keep its affected
range unresolved without unnecessarily absorbing unrelated ranges. A fixture with
an uncertain boundary outside the target should preserve the target's result;
an uncertainty affecting the target must still prevent an unsupported assertion.

For optional recovery, budget it explicitly and preserve already established
facts when it stops. Retain truthful incomplete-search reporting. Do not hide a
resource limit by relabeling the run or raise limits simply to obtain a passing
sample. Start with the measured BIS behavior, rather than speculative performance
optimizations.

Implement only the layer shown to block the selected fixture. Reuse the existing
domain, provenance, and ownership representations where possible instead of
rewriting the entire pipeline.

### 3. Reconsider model integration after the limiting stage is identified

If supplying correct region correspondence or segmentation materially improves
the small diagnostic set, compare deterministic source-based proposals with
model-assisted proposals at that stage. Keep multiple plausible groupings and
the original glyphs; model confidence remains a candidate-ranking signal, not
proof. Use the same production pipeline and budgets for both controls.

If changes still fail with correct supplied order and region pairing, work on
normalization, exact localization, or event assembly first. Parser support remains
a separate workstream for the eight extraction-incomplete pairs.

## Acceptance criteria for the first implementation

- Recover at least the selected previously missed real case end to end, with
  the correct kind, event count, source spans, and changed-token mask.
- Preserve both current reviewed production matches and detect any new scoped FP;
  do not count unreviewed added events as accuracy gains.
- Preserve exact line-wrap invariance, page-break invariance, single replacement,
  single paragraph insertion, and single paragraph deletion.
- Preserve uncertainty, source provenance, explicit limits, and exclusive
  same/changed/unresolved token accounting. Do not weaken expected annotations.
- Run the focused fixtures first; rerun the existing 29-pair evaluation after
  a demonstrated improvement. Existing development annotations do not become an
  unseen holdout merely because the algorithm changes.

The recommended immediate task is this narrow diagnostic and event-unit
experiment, followed by the evidenced domain fix. A larger model benchmark or
another broad stable-fragment search should wait for that result.
