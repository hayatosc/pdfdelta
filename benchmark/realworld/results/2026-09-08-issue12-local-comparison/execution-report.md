# Local content comparison execution, 2026-09-08

Status: implementation and focused evaluation completed; acceptance gate
failed. The three initial target expectations remain unrecovered. Four
pairs match 0 of 17 fixed expectations; SP800's eight expectations cannot
be scored by the current event-scope evaluator. Scoped changed-token false
positives increase from zero to two. Issue #12 remains open.

## Implemented change

Local correspondence now operates on bounded intervals within trusted
source runs. Disjoint intervals may survive a split or merge of runs.
Occurrence searches retain competing text across role boundaries, while
the selected comparison interval must satisfy its own role and extraction
constraints. Overlapping or crossing correspondence does not justify a
changed gap. A single exact anchor proves only its equal source range.

Bounded source windows supply exact anchors independently of upstream
sentence proposals. Adjacent verified anchors close individual comparisons,
so an ambiguous edit elsewhere in the same run need not suppress an
independent edit. Complete known input order can also close gaps left by
the aligner across soft block boundaries; inferred order or unfinished
alignment search does not grant that document-boundary proof.

Replacement, insertion, and deletion share the parent-domain localization,
emission, and token-ownership checks. The missing side of a one-sided event
comes from the established parent edit script. Local equality does not
establish movement. Budget failures retain ordinary accepted results and
explicit incomplete-search evidence.

The first `adjacent-*` SP800 run failed final validation after 636.123
seconds: a pre-existing unlocalized changed-region proof still covered
tokens that local recovery had resolved. The failed raw record and process
log remain intact; it has no usable coverage or quality metrics. A
three-character regression reproduces the same validation failure.
Both inherited and newly constructed whole-region proofs now require
entirely unresolved final ownership. Stale whole-region claims are dropped
without clipping their proof onto residual text; residual uncertainty
remains in the unresolved output. All five pairs are rerun as `validated-*`.

Semantic-path validation omits only permutations of consecutive insertions
and deletions that preserve the same equal-source pairings and semantic
hunk boundaries. All distinct pairings, including alternative whitespace
and repeated-text locations, remain checked. Exhaustive small-sequence
oracles compare this traversal with the original all-path traversal.
This addresses measured work exhaustion without changing the boundary
contract or configured limits.

Bounded assessment diagnostics connect fixed benchmark quotes to final
candidates, accepted events, parent intervals, rejection reasons, and work
consumption. Newly emitted local events retain their assessment relation
and atomic witness for the existing annotation matcher. The witness is
diagnostic evidence, not an assertion that every atomic boundary is unique.
Benchmark summary schema 67 records this assessment origin; core report
schema 10 and assessment policy 1 remain unchanged.

## First deliverable and source regressions

The [baseline diagnosis](baseline-diagnosis.md) records the initial three
misses and their actual assessment parents. CSF's two selected sentences
had no final candidate covering both quotes; FIPS's DSA note was an
insertion candidate under a document-wide unclosed parent. Mixed-role
trusted-run exclusion, whole-view competition between disjoint intervals,
and missing sentence anchors were separately demonstrated. None of these
observations alone established that the selected changes would recover.

The [source reductions](../../../../fixtures/issue12/README.md) preserve
original glyph text, raw codes, geometry, render order and mode, and
object/operator provenance. Their provenance file records PDF and fixture
hashes, pages, and selection rules. The FIPS reduction reproduces the
baseline's missing local closure; removing surrounding pages does not
preserve the full PDF's mixed-role classification. That distinction remains
explicit in the diagnosis.

The FIPS integration test now requires exactly five independently reviewed
source edits: `S` to `s`, removal of a comma, revision `4` to `5`, insertion
of ` [2]`, and list number `3` to `2`. It checks exact original glyph IDs,
old/new reversal, and permuted storage. These are development regression
expectations, not additions to the fixed real-document annotations. The
DSA-note expectation remains unresolved.

Tests cover disjoint run splits and merges, competing intervals, replacement
and one-sided edits in both directions, an ambiguous neighboring passage,
soft block splits, wrapping, page breaks, and optional budget exhaustion.
The PDF drawing-order test interleaves independent content streams and
first checks preserved glyphs, geometry, graphics state, and provenance.
Only execution identity and render order change.

## Frozen comparison and validation

The [final capture manifest](validated-source-manifest.json) fixes executable,
source, annotation and input-manifest hashes, exact command arguments,
default options with existing per-pair limit scales, exit codes, timestamps,
and output hashes. Each pair runs in its own process; at most two run
concurrently. The previous `assessed-*` and `distinct-*` experiments remain
separate, as does the failed `adjacent-*` attempt. The saved
`current-evaluation.json` in the sibling
`2026-09-08-common-assessment` directory is the comparison baseline; the
older production capture has different accepted-change semantics.

[Validation](validation.json) records successful `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`: 2,065 tests passed. Production, benchmark,
and test sources match the final frozen source manifest. The prior
`adjacent-*` attempt separately records four integration-test updates after
its executable freeze; those updates are included in the final snapshot.

The [frozen generated run](validated-generated-verification.log) retains
42/48 strict passes and six candidate-only cases. Both renderers pass all
five core acceptance cases. Generated event precision is 1.0 with recall
20/26; changed-token counts are 810 true positives, zero false positives,
and 48 false negatives. The six candidate checks are not strict passes.

## Focused results

All 40 existing real-document annotations remain unchanged; the five target
pairs contain 25 of those expectations. No aggregate exact recall is
reported for these 25 because eight are unmeasured, rather than zero matches.

Established events include changes outside the partial annotations.
Their count is not a precision estimate. Values below compare the saved
current baseline with the final frozen implementation.

| Pair | Established events | Exact annotated matches | Resolved old tokens | Resolved new tokens |
| --- | ---: | ---: | ---: | ---: |
| CSF | 0 → 6 | 0 → 0 / 3 | 18 → 582 | 18 → 581 |
| FIPS 186 | 0 → 312 | 0 → 0 / 7 | 603 → 47,487 | 603 → 47,726 |
| ECMA-109 | 7 → 34 | 0 → 0 / 3 | 24,116 → 37,126 | 24,128 → 37,148 |
| SP800-57 | 17 → 998 | 0 → unavailable / 8 | 5,404 → 167,209 | 5,455 → 167,104 |
| EDPB | 2 → 346 | 0 → 0 / 4 | 2,201 → 116,106 | 2,203 → 116,474 |

All five pairs completed extraction but retain unresolved comparison
content. `ok` is the benchmark trial status, not complete comparison.
The CSF run reaches the assessment work limit during emission. Its target
diagnostic records still have no closed parent; a complete quote join does
not mean every optional recovery search completed.

| Pair | Trial status | Runtime seconds | Final process peak MiB | Assessment work used / limit |
| --- | --- | ---: | ---: | ---: |
| CSF | ok → limit | 17.539 → 20.036 | 395.7 | 32,000,000 / 32,000,000 |
| FIPS 186 | ok → ok | 59.122 → 107.650 | 862.3 | 331,688,669 / 512,000,000 |
| ECMA-109 | limit → ok | 5.364 → 11.025 | 320.8 | 90,464,164 / 512,000,000 |
| SP800-57 | limit → ok | 69.404 → 631.551 | 1571.6 | 2,224,064,933 / 16,384,000,000 |
| EDPB | limit → ok | 27.495 → 176.530 | 726.8 | 575,005,786 / 2,048,000,000 |

Runtime is a single concurrent development measurement, not a controlled
performance benchmark. Final peak memory is per process; the saved baseline
memory figures can include earlier trials in the same process and are not
used to claim a memory reduction. Exact stage work and unsupported/limit
details remain in the per-pair JSON and process logs.

The fixed complete local scopes retain their original token definitions:

| Pair | Reviewed scopes | Expected changed tokens | Reported changed tokens | True-positive tokens | False-positive tokens | FP tokens per 10,000 unchanged |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| CSF | 1 | 178 | 0 | 0 | 0 | 0.000000 |
| FIPS 186 | 2 | 206 | 0 | 0 | 0 | 0.000000 |
| ECMA-109 | 1 | 14 | 0 | 0 | 0 | 0.000000 |
| SP800-57 | 2 | 7 | 8 | 6 | 2 | 14.144272 |
| EDPB | 1 | 10 | 0 | 0 | 0 | 0.000000 |

All scoped false-positive token counts were zero in the saved baseline.
The full-document annotations remain partial and cannot measure precision
of all emitted events. No missing, limited, or unresolved trial is removed
from the five-pair comparison.

## Fixed expectation ledger

The [combined ledger](expectation-ledger.json) tracks all 25 target
expectations. It retains final source ranges, event references, relation
parents, search status, and reasons where available. SP800's eight
expectations remain explicitly unmeasured because its scope-coordinate
gate also skips the final diagnostic join. Their earlier `distinct-*`
diagnostics are preserved in a separate historical field, not presented
as final evidence. Block/range coordinates are local to the
captured normalized document; source PDF and glyph identities are recorded
separately. An overlapping relation or event does not itself establish an
exact annotation match. Repeated occurrences and incomplete diagnostic
scans remain explicit.

| Fixed expectation | Located old → new source | Final observation |
| --- | --- | --- |
| CSF: `all-sector-scope-emphasized` | B92:764..937 → B43:0..204 | Unmatched. Missing correspondence/domain closure: no covering final candidate or accepted event; retained parents remain unclosed. |
| CSF: `core-expanded-from-five-to-six-functions` | B166:314..430 → B98:0..138 | Unmatched. Missing correspondence/domain closure: no covering final candidate or accepted event; retained parents remain unclosed. |
| CSF: `governance-and-supply-chain-emphasis-added` | absent → B43:2215..2338 | Unmatched. Missing domain closure: tentative insertion #726 (relation 688); no established emitted event. |
| FIPS 186: `dsa-specification-removed` | B164:0..72 → absent | Unmatched. Missing domain closure: tentative deletion #173 (relation 973); no established emitted event. |
| FIPS 186: `edsa-added` | absent → B135:4..77 | Unmatched. Missing domain closure: tentative insertion #185 (relation 1441); no established emitted event. |
| FIPS 186: `dsa-legacy-verification-note` | absent → B137:41..148 | Unmatched. Missing domain closure: tentative insertion #188 (relation 1444); no established emitted event. |
| FIPS 186: `deterministic-ecdsa-rfc6979` | absent → B134:171..238 | Unmatched. Missing domain closure: tentative insertion #184 (relation 1440); no established emitted event. |
| FIPS 186: `security-level-glossary-note-removed` | B254:75..117 → absent | Unmatched. Ambiguous edit location: parent 4110 / child 4111. |
| FIPS 186: `domain-parameter-requirement-moved` | B591:0..103 → B575:0..103 | Unmatched. Missing domain closure: tentative move #368 (relation 3594); no established emitted event. |
| FIPS 186: `per-message-secret-number-scope-generalized` | B329:0..47 → B312:0..28 | Unmatched. Missing domain closure: tentative replacement #257 (relation 1861); no established emitted event. |
| ECMA-109: `edition-date` | B2:0..28 → B6:0..24 | Unmatched. Missing domain closure: tentative replacement #0 (relation 562), replacement #1 (relation 563), replacement #2 (relation 564); no established emitted event. |
| ECMA-109: `copyright-year` | ambiguous → ambiguous | Unmatched. Quote occurrence is ambiguous; inspect the repeated-occurrence diagnostic. |
| ECMA-109: `copyright-notice-updated` | B110:0..43 → B120:0..42 | Unmatched. Missing domain closure: tentative replacement #56 (relation 604), replacement #57 (relation 605); no established emitted event. |
| SP800-57: `footer-rev-branding-removed` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `keying-material-definition-added` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `toolkit-footnote-comma-removed` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `cryptanalysis-review-window-added` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `toolkit-footnote-renumbered` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `standards-reference-comma-removed` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `approved-definition-comma-removed` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| SP800-57: `association-definition-punctuation` | Final coordinates unavailable | Unmeasured. Final event metrics and relation diagnostics unavailable: scoped-complete reported change coordinates are indeterminate. Earlier relation evidence is retained separately in the ledger. |
| EDPB: `consultation-watermark-removed` | ambiguous → absent | Unmatched. Quote occurrence is ambiguous; inspect the repeated-occurrence diagnostic. |
| EDPB: `completeness-of-copy-clarified` | absent → B268:871..996 | Unmatched. Missing domain closure: tentative insertion #115 (relation 1621); no established emitted event. |
| EDPB: `exception-effect-qualified` | B290:240..284 → B339:242..278 | Unmatched. Ambiguous edit location: parent 5060 / child 5061. |
| EDPB: `rijkeboer-citation-reflow` | B309:0..101 → B354:0..101 | Unmatched. Established overlapping/equal intervals 6758, 6759, 7085, 7086; no emitted event for this expectation. |


## Acceptance decision and remaining scope

| Required evidence | Final decision |
| --- | --- |
| Initial selected real content change becomes established | Failed: both CSF sentences and the FIPS DSA note remain unrecovered. |
| Five core acceptance cases stay exact | Passed for both renderers; all six other strict generated failures remain visible. |
| Exact annotated recall does not regress on any pair | Not fully measurable: four pairs remain at zero, SP800 event recall is unavailable. |
| Resolved-token coverage does not regress on any pair | Passed: both sides improve on all five pairs. |
| Scoped false positives do not increase | Failed: SP800 has two false-positive changed tokens; event precision is unavailable. |
| Resource and unresolved outcomes stay visible | Recorded: CSF reaches its assessment cap, the earlier SP800 validation failure is preserved, and every final pair remains comparison-incomplete. |

The SP800 comparison succeeds, but event quality reports
`scoped-complete reported change coordinates are indeterminate`. This
prevents aggregate event recall, scoped event precision, and the final
expected-change diagnostic join. The eight expectations stay explicitly
unmeasured; the earlier `distinct-*` 1/8 match is not substituted for this
final result. The existing token metric remains available: six true-positive
and two false-positive changed tokens, precision 0.75, recall 6/7, and
14.144272 false-positive tokens per 10,000 unchanged tokens.

The final raw preview at index 841 retains `. F` to `; f`, including an
unchanged interior space on each side. The prior diagnosed Association
punctuation edit uses this envelope. No scope or token annotation was
expanded to make the envelope count as an exact token match.

The next bounded work is to diagnose the SP800 scope-coordinate mismatch
without changing the metric, then address missing correspondence/closure
for the original CSF/FIPS targets and the remaining ambiguous localization
cases. Adding another broad recovery search is not justified by these
results.

The generated acceptance checks and source regressions do not close the
real-document objective. More accepted events or equal tokens alone do not
establish annotated recall or precision. Precision is measured only in the
existing completely reviewed local scopes; the partial annotations do not
support whole-document precision or a precision claim for the five source
regression edits.

The broader corpus is deferred until the focused acceptance gate passes.
Previously inspected holdout documents cannot supply a new blind test.
Keep the initial CSF/FIPS misses, every other fixed expectation, resource
limits, and the six strict generated failures open.

The separate [edit-boundary proposal](../../../../EDIT-BOUNDARY-CONTRACT-PROPOSAL.md)
describes an explicitly versioned whitespace representation, exact
reconstruction, reversal, and compatibility requirements. It is a proposal
only. Its final section distinguishes canonical boundaries from unchanged
tokens inside semantic event envelopes. Neither the public boundary policy
nor the evaluation expectations have been relaxed in this execution.
