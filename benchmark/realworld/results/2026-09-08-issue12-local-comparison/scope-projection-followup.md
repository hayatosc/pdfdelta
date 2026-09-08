# Scope-coordinate projection follow-up, 2026-09-08

Status: scoped event evaluation repaired; focused acceptance still fails.
All 25 fixed expectations are now measurable, with one match. This follow-up
preserves the previous [execution report](execution-report.md) and all `validated-*`
results as historical captures.

## Cause and correction

The saved SP800 comparison contains 998 emitted events. Exactly two fail
event-scope projection: both select a reconstructed space between blocks,
with no source scalar of their own. They are not zero-length spans.

| Event index | Side and group range | Source neighbors | Reviewed scope membership |
| --- | --- | --- | --- |
| 128, deletion | Old, 2348..2349 | Block 3565 scalar 84 (`-`), block 3566 scalar 0 (`5`) | Both outside every reviewed scope; no scope overlaps their interval. |
| 644, insertion | New, 487..488 | Block 1093 scalar 89 (`-`), block 1094 scalar 0 (`t`) | Both outside every reviewed scope; no scope overlaps their interval. |

The [diagnosis](scope-projection-diagnosis.json) retains exact spans,
neighbor coordinates, scope bounds, and the capture hash. A temporary
snapshot replay reproduces [two failures before](scope-replay-before.log)
and [zero failures after](scope-replay-after.log) across all 998 events.
The diagnostic snapshot hook and replay test are removed from runtime and
test code; their capture snippets remain with the diagnostic artifacts.

`SpanProjection` now bounds a selected virtual separator with both immediate
source scalars. That conservative interval is used only for scope
membership. It does not add neighboring characters to an emitted change or
to changed-token metrics. A missing or unmapped neighbor remains
indeterminate; a range crossing a scope boundary remains indeterminate.
Events inside a reviewed scope still contribute to its event count,
including unannotated whitespace events. The two SP800 events can now be
classified as outside the reviewed scopes instead of invalidating every
scoped event measurement.

The same shared projection also handles a zero-width side inside a group
by using an adjacent scalar, retaining the existing preceding-scalar
preference and enforcing the shared resource budget. A separate regression
reproduced that single-block-only limitation. The SP800 capture contains no
zero-width grouped sides, so that secondary correction is not its cause.

## Frozen validation and evaluation

Only `crates/pdfdelta-bench/src/revision_scopes.rs` differs from the prior
127-file validated source snapshot. The comparison engine, 40 fixed
annotations, per-pair resource limits, matcher, and changed-token metric
remain identical. No public edit-boundary policy has changed.

[Validation](scope-fixed-validation.json) records passing formatting,
warning-free workspace clippy, and 2,067 workspace tests. Regressions cover
virtual-only, leading, and trailing separator ranges; insertion/deletion
symmetry; scope crossings; missing neighbors; and grouped zero-width sides.
The prior strict generated result is 42/48; this evaluator-only follow-up
does not claim to fix the six generated failures.

The [frozen capture](scope-fixed-source-manifest.json) records source,
executable, annotation and manifest hashes, commands, exit codes, timestamps,
and output hashes. Each pair runs in a separate process, at most two
concurrently. The original failed and unmeasured SP800 captures remain
intact. The original CSF/FIPS target misses remain open.

## Final five-pair results

| Pair | Trial status | Emitted events | Exact annotation matches | Scoped false-positive tokens | Runtime seconds |
| --- | --- | ---: | ---: | ---: | ---: |
| CSF | limit | 6 | 0/3 | 0 | 18.333 |
| FIPS 186 | ok | 312 | 0/7 | 0 | 91.056 |
| ECMA-109 | ok | 34 | 0/3 | 0 | 10.792 |
| SP800-57 | ok | 998 | 1/8 | 2 | 588.913 |
| EDPB | ok | 346 | 0/4 | 0 | 167.275 |

[Comparison evidence](scope-fixed-comparison.json) confirms identical
reported event previews, accepted counts, resolved tokens, assessment work,
and changed-token metrics against the `validated-*` captures for every
pair. The four already measurable pairs also retain identical annotation
quality. SP800 changes from unmeasured to 1/8; this is repaired measurement
of an existing event, not new content recovery. All five comparisons retain
unresolved content, and CSF still reaches its configured assessment limit.
The runtimes are single development runs with concurrent jobs, not a
controlled performance comparison.

SP800's two reviewed scopes now have event precision 1/3 and recall 1/4.
Their changed-token counts remain six true positives, two false positives,
and one false negative. The two false-positive tokens are unchanged interior
spaces in event 841 (`. F` to `; f`), separate from the two virtual events
that caused coordinate failure. The annotations do not support a precision
estimate over all 998 SP800 events or over the full five-document corpus.

## Annotation results and diagnostic limits

The [updated 25-item ledger](scope-fixed-expectation-ledger.json) uses the
completed annotation matcher for each matched/unmatched result. SP800's
separate bounded final-assessment join returns three complete records and
one partial record, then reaches its configured limit. Four records are not
reached. Missing detailed records do not imply missing correspondence;
matched flags remain known from the completed matcher. No diagnostic limit
was raised and no earlier relation record was substituted as current evidence.

| SP800 expectation | Exact match | Final-assessment evidence |
| --- | --- | --- |
| `footer-rev-branding-removed` | Unmatched | Quote occurrence is ambiguous; inspect the repeated-occurrence diagnostic. |
| `keying-material-definition-added` | Unmatched | Missing domain closure: tentative insertion #743 (relation 4520); no established emitted event. |
| `toolkit-footnote-comma-removed` | Unmatched | Emission mismatch: emitted deletion #71 (relation 11783); exact annotation matching is reported separately. |
| `cryptanalysis-review-window-added` | Unmatched | Partial record; final-assessment scan reached its limit. |
| `toolkit-footnote-renumbered` | Unmatched | Not reached by the bounded final-assessment scan. |
| `standards-reference-comma-removed` | Unmatched | Not reached by the bounded final-assessment scan. |
| `approved-definition-comma-removed` | Unmatched | Not reached by the bounded final-assessment scan. |
| `association-definition-punctuation` | Matched | Not reached by the bounded final-assessment scan. |

The original two CSF expectations and FIPS DSA note remain unrecovered.
The broader corpus remains deferred under the failed focused gate. Issue
#12 remains open: this follow-up removes an evaluation blocker, while
content-recovery failures, the scoped false positives, diagnostic work
exhaustion, and the six strict generated failures remain unresolved.
The separately proposed edit-boundary policy is still only a proposal.

