# Exact source masks and native footer recovery

This evaluation keeps the original 29 PDF pairs, all 40 reviewed expectations,
and the frozen 39-expectation measurable denominator. The BIS opening sentence
remains outside that measurable denominator. Objective-incompatible annotations
are retained as failures; they are not rewritten or excluded.

## Original 29-pair result

The [compact evaluation ledger](evaluation.json) records every pair and all
reviewed IDs. The [verification record](verification.json) retains binary/input
provenance and the generated-test totals.

| Measurement | Previous claim implementation (`9d63045`) | Exact-mask implementation |
| --- | ---: | ---: |
| Official fixed expected-event matches | 2 / 39 | 2 / 39 |
| False-positive tokens in evaluated scopes | 2 | 0 |
| Accepted events across all pairs | 2,535 | 2,433 |
| Tentative events | 14,340 | 16,306 |
| Complete comparisons | 0 / 29 | 0 / 29 |
| Resource-limit outcomes | 9 | 8 |
| Unsupported extraction outcomes | 8 | 8 |
| Other unresolved outcomes | 1 | 1 |
| Assessment work, 28 available records | 11,498,569,106 | 10,551,524,261 |

The unchanged aggregate contains an explicit exchange of IDs:

- Retained: NIST `association-definition-punctuation`.
- Gained: IRS `footer-form-year-stamp`.
- Lost: W3C `publication-date`. Exact character masks no longer satisfy the
  existing whole-date expectation. The historical date attribution was inferred
  from its unique quote; the current miss is an official per-ID assignment.

The fixed W3C annotation is preserved, and its loss is not relabeled as success.
Thirty-six other measurable expectations remain misses. The BIS
`change-management-scope-broadened` expectation remains indeterminate. Nine pairs
have incomplete extraction in total, including the separately classified
unresolved Japanese QGIS pair. CSF moves from a resource-limit outcome to an
`ok` trial outcome, but its comparison remains incomplete and its fixed expected
change is not recovered. Trial health does not mean a complete comparison.

All 2,124 workspace tests, formatting, and workspace/all-target Clippy pass.
The sum of primary process times is 2,111.667 seconds with two concurrent jobs;
this is neither elapsed wall time nor evidence of an end-to-end speedup.

## Production changes

An event can contain several disjoint atomic changed ranges. Its display context
does not own the unchanged characters between them. Semantic signatures compare
the exact masks, and source partitions assign only those masks to `Changed`.
The production fixtures check `. F` to `; f` as one replacement with four changed
tokens and two equal spaces, `Standard` to `standard` with an equal suffix, and
ambiguous repeated characters without inventing a complete positional edit.

A bounded footer route uses a page-local catalog/form identity, unchanged
preceding context, compatible roles, complete horizontal glyph geometry, and a
terminal line band relative to font size. Duplicate identifiers and source gaps
prevent acceptance. It reconstructs a local order even when PDF operators paint
the right-hand form label first; it does not certify the reading order of the
surrounding body. The `PageLocalFooterIdentity` assumption is explicit. No expected
quote, annotation offset, or document-specific identifier enters this discovery.

The native IRS 1040 comparison recovers `footer-form-year-stamp` as one replacement
with two exact hunks: the year digit and the creation stamp. The fixed annotation
scores TP 17, FP 0, FN 0. The intervening closing parenthesis remains unchanged.
Benchmark conversion groups exact hunks only through one proved source relation
and retains the public hunk count separately from logical occurrence count.
Scoped annotation projection uses an independently certified terminal-line view
so operator order cannot place the old footer on a later page.

## Literal proof kernel

The shared proof kernel computes multiple count queries and mandatory positions
over all optimal insertion/deletion paths. Exact-distance banding removes only
cells that cannot lie on an optimal path. Adaptive dispatch retains a dense
implementation, equal/empty fast paths, explicit work charging, and the normal
64 MiB proof-memory cap. Exhaustive and adversarial tests compare full masks and
bounds; source and residual queries remain separate.

The [kernel measurement](kernel-benchmark.json) records five-run medians, charged
work, additional live allocations, full bit-packed masks, and source hashes.
These are kernel measurements, not full-PDF speedups.

| Input | Adaptive | Forced dense, normal cap | Forced dense, experimental 512 MiB cap |
| --- | ---: | ---: | ---: |
| 500 characters, one replacement | 0.078 ms | 2.492 ms | 2.935 ms |
| 2,000 characters, one replacement | 0.253 ms | Limit exceeded | 112.369 ms |
| 4,000 characters, one replacement | 0.580 ms | Limit exceeded | 414.020 ms |
| DSA introduction, 1,431 to 2,509 characters | 76.069 ms | Limit exceeded | 111.031 ms |

The actual DSA input selects the band layout; this run does not demonstrate a
dense fallback for DSA. Its selected new-side range `[1611, 1718)` retains bounds
103–107, 74 mandatory positions, and residual bounds 29–33. The experimental dense
cap changes a test-only hook and does not raise the production cap. Full-mask
parity holds for every completed mode. The harness is
[`kernel_benchmark.rs`](kernel_benchmark.rs).

All keep/drop normalization hypotheses remain quantified independently. Claims
for already proved domains can share the kernel, but no arbitrary text split or
minimum-difference normalization choice is introduced. No new independent-domain
normalization factorization or cross-side coupling is claimed.

The structure diagnostic now separates mandatory-position count, optimal edit
cost, a complete residue-equality witness, and a minimal complete witness.
`a` to `aa` is changed but unlocalized, and the CSF mandatory mask of 147 positions
is not called a complete edit of cost 147. The fixed CSF mask remains 178 against
literal minimum 158; the Attention mask remains 11 against 7.

## Generated and previously unused PDFs

All 48 generated renderer cells satisfy their declared policy: 42 meet strict
author-intent acceptance and six meet candidate-only expectations. Token totals
remain TP 810, FP 0, FN 48. Both renderers pass all five required core cases:
line-wrap invariance, page-break invariance, one text replacement, one paragraph
insertion, and one paragraph deletion. Candidate acceptance does not redefine
the first practical release.

The separately [frozen annotated JLS holdout](../../round2-holdout/README.md)
retains verified official PDF hashes and one source-reviewed insertion before
its first comparison. Both PDFs contain unsupported clipping paths outside the
annotated pages. The comparison also reaches its alignment work limit: zero
accepted events, two candidates, and 10,937 unresolved regions. Official matching
and source-mask quality are unavailable. This is a failed complete comparison,
not a precision success or a zero-valued recall measurement. The manifest and
annotation were not tuned after this result. The earlier RFC pair remains a
previously seen extraction diagnostic with its separate issue ledger.

## Reproduction

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p pdfdelta-bench --bin pdfbench
target/release/pdfbench verify
PYTHON_UV=0 python benchmark/realworld/run_claim_evaluation.py NEW_OUTPUT \
  --cache-dir PDF_CACHE --jobs 2
PYTHON_UV=0 python benchmark/realworld/summarize_claim_evaluation.py \
  NEW_OUTPUT NEW_SUMMARY.json NEW_SUMMARY.md
PYTHON_UV=0 python benchmark/realworld/run_claim_evaluation.py NEW_HOLDOUT_OUTPUT \
  --manifest benchmark/realworld/round2-holdout/manifest.tsv \
  --cache-dir HOLDOUT_CACHE --jobs 1
rustc --edition 2024 --cfg test -O \
  benchmark/realworld/results/2026-09-09-exact-masks/kernel_benchmark.rs \
  -o /tmp/pdfdelta-kernel-benchmark
/tmp/pdfdelta-kernel-benchmark
```

The runner snapshots one binary and records source, input, and annotation hashes,
process exits, runtime, and memory. Raw reports and logs stay local; the compact
checked-in ledger retains official per-ID assignments, source coverage, scoped
mask quality, extraction/comparison status, and resource outcomes. Unavailable
measurements remain null. Timings are single-run observations, not a controlled
end-to-end speed claim. Global precision outside reviewed scopes remains unknown.

The primary 29-pair capture uses one binary snapshot. A subsequent output-only
fix exposes the existing scoped-complete match outcome as per-ID assignments.
W3C and Attention were rerun for those assignments; every other non-timing field
of their evaluation records matched the primary capture. The compact ledger
preserves primary measurements and records the supplemental binary hash and
assignments explicitly. A fresh run of the final source emits those assignments
directly and needs no supplemental pass.
