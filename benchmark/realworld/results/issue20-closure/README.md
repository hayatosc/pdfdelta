# Exact-change acceptance: closure evidence

This record separates implementation evidence from the remaining acceptance
conditions for the real-document recovery work. It is not a declaration that
every reviewed change or every PDF pair is completely compared.

## Frozen expectations and distinct measurements

The existing revision manifest and its 39 expected changes remain unchanged.
The dated native regression in
[`2026-09-09-migration-frozen-regression.json`](../2026-09-09-migration-frozen-regression.json)
is the comparison baseline. Its whole-event recall is 2/39 and no pair is complete.
The earlier focused 1/25 assessment has a different denominator and must not be
compared directly with 2/39.

Whole-event recovery, changed-source-token recovery, unchanged-token false
positives, coverage, and execution status are separate measurements. Unannotated
events are not automatically false positives. Conversely, partial token recovery
does not count as a fully recovered expected event. Failed and unavailable inputs
remain in the trial inventory, with unavailable measurements distinguished from
zero-valued measurements.

## Existing generated regressions

The generated matrix contains 48 renderer cells. The strict author-intent result
is 42/48; six cells intentionally remain ambiguous under the declared literal
comparison objective. They are not six additional strict successes.

| Case, each with two renderers | Competing half-open source intervals | Reason strict acceptance is withheld |
| --- | --- | --- |
| `text-insertion` | New `[60, 65)` or `[59, 64)` | The adjacent unchanged space can be paired on either side of the inserted text at the same edit cost. |
| `text-deletion` | Old `[40, 45)` or `[39, 44)` | Either adjacent space can survive while the deletion has the same edit cost. |
| `numbered-requirement-text-insertion` | New `[71, 85)` or `[70, 84)` | The two boundary-space placements are observationally indistinguishable to the literal objective. |

The generator knows which edit it applied, but that history is not evidence
available in the rendered PDFs. Each of these cases must therefore retain zero
accepted exact events, one candidate with both alternatives, an independently
proved changed region, incomplete comparison, and zero exact-event recall.
`ambiguous_inline_cases_use_candidate_policy_without_strict_promotion` in
`crates/pdfdelta-bench/tests/bench_matrix.rs` asserts these outcomes for both
renderers. The five original practical-release cases remain strict requirements.

Reproduce the entire generated matrix with:

```sh
cargo run -p pdfdelta-bench --locked -- verify
```

## DSA and objective compatibility

The retained DSA source reductions and normalized diagnostic inputs preserve
the ambiguity instead of promoting one arbitrarily chosen optimal edit path.
Under a supplied whole-introduction correspondence, the topic-note query has
103..107 inserted scalars, 74 mandatory changed positions, and 33 ambiguous
positions. A supplied paragraph correspondence yields different bounds because
it is a different premise; neither diagnostic premise is an automatically
established production correspondence.

The source-backed FIPS regression retains independently established edits with
their original glyph IDs in both directions and after glyph-storage permutation.
It does not claim recovery of the complete DSA note. The proof example checks
source hashes, compares bounds with exhaustive small-string enumeration, and
keeps its caller-supplied domains explicitly diagnostic:

```sh
cargo test -p pdfdelta-bench --test source_backed_local_comparison
cargo run -p pdfdelta-bench --example structure_claim_probe -- > /tmp/issue20-claims.json
```

Attention's frozen stamp mask costs 11 edits while the literal optimum costs 7.
Recovering seven correctly located changed tokens cannot satisfy the existing
eleven-token expectation. This mismatch must remain visible; neither the frozen
annotation nor the exact mask may be expanded or rewritten to manufacture a pass.

## Independent evaluation protocol

1. Finish implementation and required checks; freeze the executable and record
   its SHA-256 together with the source revision and any uncommitted patch.
2. Treat every previously inspected or evaluated document as development data,
   regardless of historical `holdout` labels.
3. Select a new real revision pair after that freeze. Record its original URLs,
   versions, input hashes, and acquisition failures before comparison.
4. Review a declared source scope and freeze its expected changes and unchanged
   context before inspecting any engine comparison output.
5. Evaluate the fixed executable against those fixed expectations, including an
   unchanged-input control. Retain failures and misses. A fix informed by this
   result consumes the holdout and requires a different new pair for validation.

No OCR provider is used. Image text remains unresolved and retained page pixels
remain available for visual comparison.

## Frozen 29-pair replay

[`frozen-regression.json`](frozen-regression.json) contains all 29 baseline and
new records, exact commands, per-case coverage, expected-event matches, scoped
token metrics, uncertainty, and resource failures. The manifest and its original
39 expected changes are unchanged. [`process-costs.json`](process-costs.json)
retains elapsed time, peak RSS, and process exit status, including allocation
failures that cannot produce an engine evaluation record.

| Measurement | Before | After |
| --- | ---: | ---: |
| Fully matched expected events | 2/39 | 2/39 |
| Correctly recovered changed source tokens | 31/709 | 38/709 |
| False-positive changed tokens in annotated unchanged context | 0 | 0 |
| Pairs with a comparison result | 25/29 | 25/29 |
| Completely compared pairs | 0/29 | 0/29 |

No comparable per-side coverage value decreases. All 37 unmatched expected
events remain unmatched; recovering seven Attention tokens is a partial-token
improvement, not an additional accepted expected event.

| Reviewed pair | Matched events before → after | Correct changed tokens before → after | Engine status |
| --- | --- | --- | --- |
| `nist-fips-186-4-to-5` | 0/7 → 0/7 | 0 → 0 | ok |
| `nist-sp800-57-part1-r4-to-r5` | 1/8 → 1/8 | 6 → 6 | ok |
| `irs-form-1040-2024-to-2025` | 1/5 → 1/5 | 17 → 17 | ok |
| `edpb-right-of-access-v1-to-final` | 0/4 → 0/4 | 0 → 0 | ok |
| `arxiv-attention-v6-to-v7` | 0/1 → 0/1 | 0 → 7 | ok |
| `w3c-ws-policy-attach-20060927-to-20061102` | 0/4 → 0/4 | 8 → 8 | ok |
| `ecma-109-ed10-to-ed11` | 0/3 → 0/3 | 0 → 0 | ok |
| `oasis-csaf-v2-cs01-to-csd02` | 0/3 → 0/3 | 0 → 0 | ok |
| `irs-w4-korean-2024-to-2025` | 0/1 → 0/1 | 0 → 0 | ok |
| `oasis-mqtt-311-to-50` | 0/0 → 0/0 | 0 → 0 | limit |
| `nist-csf-v1-1-to-v2-0` | 0/3 → 0/3 | 0 → 0 | ok |

`ok` denotes a healthy trial with potentially incomplete comparison. MQTT has
no annotated changed events; 0/0 is not successful recall. The complete inventory
has 12 `ok`, seven engine-limit, six unsupported, one unresolved, two process
allocation failures (GCC and Unicode), and one unavailable pair (NASA's new
input is absent). None is removed from the 29-pair denominator. Form-heavy
misses remain in the table and full inventory.

## Source-backed correspondence change

An unresolved reading-order block can now contribute exact anchors when its
single-line scalar-to-source mapping is complete and disjoint. Only whitespace
collapse is allowed; shared glyphs, expanded ligatures, missing evidence, and
line or page boundaries remain ineligible. One exact anchor never establishes
the rest of the block. An independently unique common ending can supply a second
anchor, after which the existing ownership, ordering, and exact-mask checks apply.
All searches consume the existing work budget.

The reduced Attention fixture retains original glyph IDs, raw codes, coordinates,
and operator provenance. The integration test asserts the exact changed glyph
sets in both directions and after storage permutation. The reduction is not
evidence that its anchors are unique in the original full document; the separate
full-PDF trial supplies that check. See
[`fixtures/issue20`](../../../../fixtures/issue20/README.md) and
[`issue20_source_domains.rs`](../../../../crates/pdfdelta-bench/tests/issue20_source_domains.rs).

The executable and working-tree patch hashes are recorded in
[`freeze.json`](freeze.json); [`implementation.patch`](implementation.patch)
preserves the tracked changes present at that freeze. Source fixtures and tests
are checked in separately. Formatting, workspace Clippy with warnings denied,
workspace tests, and the generated verification matrix passed before evaluation.

## New holdout outcome

RFC 5988 to RFC 8288 was selected after the executable freeze. The archive PDFs
and official revision relationship are recorded in
[`unseen-provenance.json`](unseen-provenance.json). The annotation covers one
body-text deletion and an adjacent unchanged sentence. It was fixed before any
comparison output. This is a new document series, not a previously examined pair
with a new holdout label.

The changed trial has incomplete extraction and an assessment search limit.
Its one expected deletion remains unscored: recall, false-positive rate, and
coverage are unavailable, not zero-valued successes. The identical-input control
reports zero changes but remains indeterminate because of unsupported clipping.
These results **do not establish generalization** and do not satisfy the issue's
holdout acceptance condition. No implementation or expectation was tuned on them.

The revision harness intentionally rejects identical PDFs, so the control uses
the native CLI adapter compiled from the same unchanged core source. Its later
build is disclosed in the provenance record. Results are retained in
[`unseen-evaluation.json`](unseen-evaluation.json),
[`unseen-summary.json`](unseen-summary.json), and
[`unseen-control.json`](unseen-control.json).

To reproduce after acquiring and hash-checking the inputs named in the manifest:

```sh
cargo run --release -p pdfdelta-bench --locked -- revisions \
  --manifest benchmark/realworld/results/issue20-closure/unseen-manifest.tsv \
  --cache-dir /tmp/pdfdelta-issue20-unseen --pair rfc5988-to-rfc8288 \
  --summary-json-output /tmp/unseen-summary.json \
  --evaluation-json-output /tmp/unseen-evaluation.json
cargo run --release -p pdfdelta-cli --locked -- --native-text-only \
  /tmp/pdfdelta-issue20-unseen/old.pdf /tmp/pdfdelta-issue20-unseen/old.pdf \
  --json /tmp/unseen-control.json
```

### Alternate distribution layout, same holdout series

After the first failure, the standard monospaced PDF distribution was acquired
from RIPE's RFC archive. This is an additional layout of the same series, not a
second unseen series. The implementation, annotation bytes, and scale 1 limits
were unchanged. Original source hashes and URLs are in
[`unseen-text-layout-manifest.tsv`](unseen-text-layout-manifest.tsv).

Extraction completes for this layout, but changed-document comparison reaches
the assessment search limit with zero comparable coverage. The frozen scope's
short start anchor also matches multiple places, so quality is unavailable.
The original annotation is retained, not revised after observing the result.
Source-only extraction had confirmed the intended phrase existed; that was
insufficient to prove the selector globally unique.

Its unchanged-input control completes at coverage 1.0 with zero changes and
zero unresolved regions. This establishes that control result only; it does not
establish recovery of the actual revision. The changed trial took 1.08 seconds
and 118,764 KiB peak RSS (exit 1); the control took 1.37 seconds and 113,852 KiB
(exit 0). See [`unseen-text-layout-evaluation.json`](unseen-text-layout-evaluation.json),
[`unseen-text-layout-summary.json`](unseen-text-layout-summary.json), and
[`unseen-text-layout-control.json`](unseen-text-layout-control.json).

## Closure status

Issue 20 remains open. Source-backed reproducers, acceptance-contract evidence,
source accounting regressions, generated-case justifications, and a full frozen
replay are available. Partial source-token recovery improves without additional
annotated false positives, but whole-event recovery does not improve and the
new changed-document holdout cannot be scored. Resolving these gaps requires
further correspondence/search work and a valid independently frozen holdout
annotation; unit-test success and the unchanged-input control do not substitute
for those conditions.
