# Local comparison and CPU reading-order experiment

Neither trial recovered additional reviewed changes on this corpus. The local
prototype also introduced an internal resource-limit regression, so it is
preserved as a patch rather than enabled in the library. The CPU reading-order
model changed block order but added no newly recovered expectation, even when
accepted and candidate changes are counted together. The retained library change
is only the pre-existing separator crash repair; model support remains an
explicit benchmark experiment.

## Current decision

Further implementation and model trials under the current approach are on hold.
The measurements do not provide evidence that more work on these approaches
will achieve practical accuracy across the intended PDF range. Issue #20 remains
unresolved; this checkpoint does not claim a practical release.

The [diagnosis and earlier next-step proposal](next-steps.md) is retained as
analysis, not an approved implementation plan. Restricting supported PDF families
is a possible product-scope change, but has not been agreed or implemented.
The existing five core acceptance requirements remain unchanged.

The experiment tests issue #20 in two stages: recover exact local edits without
requiring a globally unique reading order, then evaluate learned reading order
when the first stage leaves reviewed revisions unrecovered. These results do not
establish correctness for arbitrary PDFs or rule out different model integrations.

## Results

The production-pipeline comparison used all 29 revision pairs. All 40 reviewed
expectations belong to 12 development pairs; one BIS expectation cannot be
resolved against the original source, leaving a fixed denominator of 39.

| Measure | Crash-fixed baseline | Local trial |
| --- | ---: | ---: |
| Reviewed exact changes recovered | 2/39 | 2/39 |
| Scoped false-positive tokens | 2 | 2 |
| Internal resource-limit outcomes, all 29 pairs | 9 | 10 |
| Complete comparisons | 0 | 0 |
| Accepted events, including unreviewed regions | 2,535 | 3,300 |

The extra 765 events are not measured accuracy gains. BIS changes from `ok` to
an internal assessment work limit. The two previously matched IDs are retained;
no reviewed match is gained or lost. Generated results stay at strict 42/48,
with the other 6/6 passing the existing candidate policy and zero generated FP
tokens. The five mandatory core cases are not redefined.

The reading-order experiment uses **two separate, identically configured
controls**: both assume their supplied block order and use the public
`align_ordered` / `compare_aligned_with_atomic_edits` path. They do not run the
production pipeline's local sentence recovery. Thus their absolute recall must
not be compared directly with the production rows above.

| Measure, fixed source annotations | Native order assumed | Model order assumed |
| --- | ---: | ---: |
| Reviewed exact changes recovered | 1/39 | 0/39 |
| Reviewed changes present as candidates | 10/39 | 11/39 |
| Reviewed changes in either category | 11/39 | 11/39 |

The sole category change is W3C's `former-current-version-url`: accepted under
native order, candidate under model order. There is no newly recovered expected
ID. Both controls finish all 12 pairs, but successful execution does not mean a
complete comparison. Diagnostics retain normalization uncertainty in 10 pairs
under either order, alongside unclosed domains and ambiguous edit locations.

Scoped FP counts are not a complete paired precision comparison: native has 2
FP tokens over 11 measurable pairs, while model has 0 over 10. BIS has the same
unresolvable source scope in both controls; the model's ECMA output additionally
has indeterminate reported-change coordinates. These missing measurements remain
null with their errors, rather than being counted as zero FP.

See [all local-trial differences](local-trial/comparison.json),
[all final order-control outcomes](order-summary-final.json), and
[CPU inference measurements](model-summary.json).

## CPU cost and scope of the model trial

PP-DocLayoutV3 ran on all 24 annotated PDFs (1,435 pages), with CPU-only PyTorch
and two threads per inference process. Median page inference was 2.49 seconds;
maximum observed process RSS was 1,963,581,440 bytes (about 1.83 GiB). Summed
document durations were 3,669.9 seconds; this sums reported wall durations for
individual documents, not elapsed batch time. Two inference processes ran concurrently. Comparison timings
were not isolated from other experiment jobs.

All 24 documents changed order. All 37,713 native blocks remained a verified
permutation: 31,801 were assigned to predicted regions and 5,912 retained an
explicit fallback position. A region may contain multiple native blocks; their
relative native order is retained. Fallback blocks stay on their original page,
apart from blocks without page geometry. Five pages had no detected regions;
their source evidence was not dropped.

This trial changes block order only. It neither recognizes text nor replaces
native block segmentation, and it does not promote model confidence to source
proof. Resolving normalization, segmentation, or correspondence uncertainty would
be a separate change. These measurements support declining this integration as
the current fix, not claiming that every possible learned layout approach fails.

## Fixed inputs and controls

The starting revision is `db7e30e`. The revision manifest contains 29 pairs
(58 cached PDFs), including 12 annotated development pairs with 40 expected
changes. Manifest-designated holdouts are not claimed to be genuinely unseen:
their inspection history has not established that. Scope-resolution failures,
extraction failures, work limits, and external timeouts remain in the accounting.
Annotations and the five core release cases are not changed for this experiment.

`baseline/` retains results from an executable built from the starting revision.
SP800-57 and W3C expose an existing panic: a three-block mixed separator pattern
was reused on an expanded comparison domain. `baseline-fixed/` uses only the
isolated separator fix, with a saved patch and executable hash. Comparisons of
the new recovery policy must use this fixed baseline, not attribute the panic
fix to the new algorithm. Historical measurements are not substituted for runs.

## Local recovery contract

Correspondence must already be established inside a source-backed local run.
Unknown order between independent runs does not establish order within a run,
and neither a similarity score nor an externally supplied paragraph boundary
establishes correspondence.

Within a closed domain, a matching token pair occurs on an optimal
insertion/deletion path exactly when its prefix LCS length plus one plus its
suffix LCS length equals the domain's LCS length. A match rank with only one
possible pair is mandatory. Changed gaps between consecutive mandatory ranks
have the same coordinates in every optimal path, even if another gap is
ambiguous. Only completed bounded proofs may produce these local changes.

This does not recover an author's edit history or justify the DSA whole-quote
insertion when competing optimal interpretations remain. Any ambiguous remainder
stays unresolved. Changed-token precision is measured separately from a readable
event's surrounding context.

The trial currently suppresses one-sided stable fragments derived from an
ambiguous parent; it still allows ordinary independently established insertions
and deletions. In a generated low-overlap replacement, a stable `ri` -> `rvi`
subsequence otherwise emits only `v` as an insertion and prevents the wider
unlocalized changed-region report. The current ownership contract cannot retain
that whole unresolved parent on top of a resolved child. This is a conservative
limitation of this trial, not evidence that every independently valid local
insertion or deletion has been recovered.

## CPU model implementation

`../../issue20_model_order.py` uses the official PP-DocLayoutV3 Transformers
implementation with CPU-only PyTorch and a pinned local model snapshot. It
records page regions and predicted order, runtime, library versions, and the
PDF hash. It does not recognize or regenerate text. Predictions are explicitly
marked `hypothesis_only`; detector confidence is not an exact-order proof.

The model's coordinates use the rotated CropBox's bottom-left frame, the same
frame used for glyph evidence. Mapping and grouping must retain original glyphs
and account for unassigned or multiply assigned content. Any comparison under
the selected model order is conditional on that assumption and must not be
reported as engine-established reading order.

## Artifacts and reproduction

- `baseline/`: original executable results, including the two pre-existing crashes.
- `baseline-fixed/`: all 29 comparisons with only the separator repair.
- `local-trial/`: all 29 prototype comparisons, exact core patch, input and binary
  hashes, generated results, and passing trial validation logs.
- `model/`: all CPU model captures, process logs, pinned model identity and
  environment instructions.
- `order-controls/`: preliminary controls with an invalid annotation-order
  coupling; retained for debugging and excluded from the final conclusions.
- `order-controls-source-fixed/`: final controls. Annotations and scope coordinates
  stay anchored to native source blocks while model-order comparisons retain
  explicit span block order.
- `retained-implementation.patch`: the retained library repair and benchmark
  implementation, applicable to `db7e30e`.

To rebuild the local prototype in a fresh directory, apply its patch to the
starting revision and build the archived workspace:

```sh
mkdir /tmp/pdfdelta-issue20-local-rebuild
git archive db7e30e | tar -x -C /tmp/pdfdelta-issue20-local-rebuild
git -C /tmp/pdfdelta-issue20-local-rebuild apply "$PWD/benchmark/realworld/results/issue20-order-experiment/local-trial/core.patch"
cargo build --manifest-path /tmp/pdfdelta-issue20-local-rebuild/Cargo.toml --release -p pdfdelta-bench --bin pdfbench
PYTHON_UV=0 python3 benchmark/realworld/results/issue20-order-experiment/local-trial/run.py /tmp/pdfdelta-issue20-local-rebuild/target/release/pdfbench /tmp/pdfdelta-local-results
```

To repeat the final controls from the retained source and saved model captures,
use a fresh output directory:

```sh
cargo build --release -p pdfdelta-bench --example compare_order_hypotheses
PYTHON_UV=0 python3 benchmark/realworld/issue20_run_probe.py target/release/examples/compare_order_hypotheses benchmark/realworld/results/issue20-order-experiment/model /tmp/pdfdelta-order-results
PYTHON_UV=0 python3 benchmark/realworld/issue20_summarize_probe.py /tmp/pdfdelta-order-results /tmp/pdfdelta-order-summary.json
```

Model regeneration instructions are in [model/README.md](model/README.md).
The final library and benchmark source pass workspace formatting, Clippy with
warnings denied, and all workspace tests. Logs are retained in `validation/`.

## Measurement boundaries

- Reviewed accepted changes and individual missed expectations.
- Unchanged-character false positives within fully reviewed scopes.
- Source-token coverage and extraction completeness on each side.
- Candidate changes separately from established changes.
- Search completion, timeouts, runtime, and peak memory.
- The 48 generated renderer cases and five mandatory core cases.
- Model reading-order results separately from downstream diff results.

Unreviewed changes are not assumed correct or incorrect, and partial annotations
are not treated as full-document precision. Model hypotheses, accepted changes,
candidate changes, and unresolved evidence remain separate throughout.
