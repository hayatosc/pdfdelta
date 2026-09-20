# Exact search when only a few proposals can coexist

**Strict completion remains 0/36 after the full-panel replay.** Exact search now
finishes for two more retained conflict components within the original budgets,
reducing unfinished components from 11 to nine. Neither component has a
mandatory correspondence. All 36 document comparisons retain their previous
strict coverage and unresolved obligations. This is a solver improvement, not
progress in the strict completion count.

## Measured obstacle and implementation

The preceding frozen run stopped before exploring an ECB component with 36
grouped proposals and a NIST contingency component with 59 grouped proposals.
Both exceeded the subset solver's 24-proposal applicability limit, and neither
had the independent single-node remainder needed by mixed assignment.

The endpoint populations give a tighter bound. The ECB proposals each consume
one old endpoint from a population of two. The NIST proposals each consume at
least two old endpoints from a population of five. Shared endpoints conflict,
so at most two proposals can coexist in either component. In general the safe
cardinality bound is the smaller of the old and new endpoint counts divided by
their respective minimum group sizes.

`matching/cardinality.rs` checks that the entire capped decision tree fits the
remaining state budget, then enumerates it iteratively. The preflight uses
`sum(C(n + 1, j + 1), j = 0..k)` with checked arithmetic. Endpoint collection
and preflight consume the existing assignment-work budget; decision states and
partition validation consume their existing ledgers. Failed preflights and
hybrid fallback work remain charged. Source-only verification shares the
remaining component budgets. No configured limit is increased.

Every selection still passes all source conflicts and the full intersection of
alternative-partition constraints. The six-class integer objective and the
intersection of all optimal selections are unchanged. An exhausted search
retains only an independently established forced prefix. The new report
algorithm is `cardinality_choice`; `exhaustive` independently records completion.

## Independent verification and pilot

The benchmark verifier `small_component_oracle.py` enumerates combinations up
to an independently derived cardinality bound. It checks the retained native
constraint projections for overlapping sources, descendant ownership,
partitions and explicit source conflicts. It also agrees with unrestricted
subset enumeration on all 511 nonempty subgraphs of a small endpoint universe.

| Natural component | Proposal count | Independent subsets checked | Optimal selections | Production states | Preflight work |
| --- | ---: | ---: | ---: | ---: | ---: |
| ECB annual, component 1437 | 36 | 667 | 292 | 3,523 | 255 |
| NIST contingency, component 228 | 59 | 1,771 | 722 | 16,286 | 420 |

Both optima have score two, with an empty mandatory intersection. The production
results agree. These checks concern the supplied candidate populations and
their native constraint projections. They do not independently establish visual
transcription, semantic identity, omitted rivals or complete acquisition.

The two pilot coverage objects and unresolved arrays remain identical to the
preceding frozen run. Candidate enumeration is independently incomplete for
these components: the comparison layer records the omitted-source dependency
before inspecting the solver's exhaustive flag. A completed retained conflict
component therefore does not make the whole comparison search resolved.

The first direct CLI captures are retained under `clique-search/pilot/`. A
subsequent request for standard frozen capture records reached OpenCode after
those runs had started, so the two pairs were repeated under
`cardinality-pilot/`. Their comparison and coverage objects agree exactly. The
remaining panel pairs are captured separately with the same executable. These
concurrent runs are not a controlled performance benchmark.

## Full-panel result

The two pilot captures and the other 34 captures form one complete replay of the
original panel. They use the same executable, input hashes, text route, limit
scale 1 and 180-second outer timeout. `cardinality-panel.json` binds both frozen
capture manifests; `cardinality-panel-audit.json` compares them with the preceding
full-panel run in `after.json`.

| Measure | Before | After |
| --- | ---: | ---: |
| Strictly complete pairs | 0/36 | 0/36 |
| Unfinished retained conflict components | 11 | 9 |
| Pairs with unfinished retained components | 9 | 8 |
| Pairs with all correspondence search resolved | 2/36 | 2/36 |
| Strictly compared old source references | 452,104 | 452,104 |
| Strictly compared new source references | 452,421 | 452,421 |
| Inputs with incomplete text inventory | 72/72 | 72/72 |

The audit finds no lost strict comparisons, lost source-backed reviews, coverage
regressions or completion regressions. Every coverage object is identical. The
comparison payloads differ in six pairs, exclusively in solver status, explored
states and work/budget accounting; the other 30 payloads are identical. Input
hashes, source conservation and the complete-comparison predicate are checked
against each raw report.

Both newly solved components still depend on omitted source candidates, so the
existing unresolved obligations correctly remain. More retained-component search
alone cannot remove the acquisition blocker shared by all 72 inputs. The next
strict-completion improvement must establish a valid interpretation of remaining
paint effects and finish source coverage and candidate enumeration for a whole
pair; useful partial comparisons are not counted as that result.

## Checks and evidence

The new Rust unit tests include 512 comparisons with exhaustive subset search,
ties, zero weights, objective priorities, source conflicts, independent old/new
partition namespaces, forced prefixes, malformed groups and budget stops.
Public-API fixtures exercise components above 24 proposals and ensure that an
inferred tie breaker cannot promote a source candidate into a source-only fact.
The prior proposal-count-only truncation tests now exercise successful exact
search; a separate wide conflict chain retains the genuine truncation check.

All workspace checks pass: 2,619 tests, zero failures and two ignored; all-feature
library tests pass 1,515 with two ignored. Formatting, all-target/all-feature
Clippy, warning-free private documentation and the generated 48-cell panel
pass. The generated panel comprises 42 strict author-intent cells and six
declared candidate-policy cells.

`cardinality-oracle.json` binds the independent source projections and natural
results. `cardinality-checks.json` binds check logs, executable, source archive
and runner. The pilot and remaining-panel source archives have identical file
contents, and all 246 captured production files match the checked workspace.
The executable SHA-256 is
`f18ce992be2d63a7b76fe683c301187761a352c4163d7a3df3f519dc65f42d45`.

The acquisition and terminal-boundary findings that constrain the strict goal
are recorded in [strict-priority.md](strict-priority.md). The Schedule SE footer
candidate crosses an uninterpreted paint effect; no terminal-boundary exception
was implemented. The strict inventory and completion predicates are unchanged.
