# Issue 20 baseline

Baseline source: `db7e30e6c298d844a141ed56fb2d00f8cf82bd48`, archived before
engine edits. `binaries.sha256` identifies the debug and release executables;
`inputs.sha256` identifies the unchanged manifest and all 12 annotation files.
All 58 source PDFs pass manifest checksum verification (`checksums.log`).

`generated.log` records a current debug-binary run: strict author-intent
acceptance 42/48 cells, separate candidate-policy acceptance 6/6 cells,
changed-token precision 1.0 (810 true positives, 0 false positives, 48 false
negatives). The command's 48/48 policy total is not strict acceptance.

The release run evaluates all 29 manifest pairs in separate processes, with
at most two concurrent comparisons. Annotated pairs run first with 900-second
external timeouts; other pairs have 180-second external timeouts. These
external ceilings are additional to the manifest's pipeline budget hints.
Each `.process.json` retains the exact command, elapsed time, exit code or
external timeout. Exit 101 denotes a process panic, not an empty diff.
The `.summary.json` files retain per-expectation diagnostics and scoped metrics;
`.evaluation.json` files retain operational and quality summaries. Missing
outputs after a process failure do not mean zero changes or a passing case.

## Reproduction

From the repository root, using a new output directory:

```sh
mkdir -p /tmp/pdfdelta-issue20-baseline
# Only needed to recreate the frozen source and binary:
git archive db7e30e6c298d844a141ed56fb2d00f8cf82bd48 | tar -x -C /tmp/pdfdelta-issue20-baseline
cargo build --release --locked --manifest-path /tmp/pdfdelta-issue20-baseline/Cargo.toml -p pdfdelta-bench
PYTHON_UV=0 python3 benchmark/realworld/results/issue20-order-experiment/baseline/run.py /tmp/pdfdelta-issue20-baseline/target/release/pdfbench /tmp/pdfdelta-issue20-baseline-rerun
```

The same runner accepts a trial executable and new result directory, preserving
pair order, concurrency and external time limits. Manifest and annotations
must remain unchanged between baseline and trial.

## Interpretation limits

The manifest assigns 19 pairs to development and 10 to holdout. All 12
annotated pairs are development data. The remaining pairs have no reviewed
exact-change annotations, so their outcomes measure operational behavior,
coverage and uncertainty only. The holdout designation does not establish
that these documents have never been inspected; no genuinely unseen annotated
holdout accuracy is measured here. Annotation scopes are partial for most
pairs and do not establish full-document precision.

An initial debug real-world attempt was interrupted before producing results;
its two logs are preserved in `debug-attempts/`. It is not part of the release
measurement. The historical assessment's SP800-53 label refers to the
SP800-57 pair in its linked five-pair evaluation; the current manifest only
contains SP800-57 among those two names.

## Completed original-source measurement

All 12 annotated pairs completed a process attempt: 8 exited successfully,
2 stopped at internal resource limits (CSF and MQTT), and 2 panicked
(SP800-57 and W3C, both at `alignment/score.rs:26`). BIS additionally could not
resolve its reviewed scope, despite successful process completion.
The available quality records match 0/27 reviewed expected changes. Twelve
expectations belong to the crashed pairs and one belongs to the unmeasurable
BIS scope; they remain explicitly unavailable, not silently dropped passes.
Measured scoped false-positive tokens are zero; this does not measure the
crashed SP800-57 case that historically produced two such tokens.

Five unannotated operational pairs also completed before the original queue
was interrupted to prioritize a crash-fixed annotated baseline. Overall,
17/29 process attempts completed; `process-summary.json` retains every pair,
including `interrupted` and `not_run` states for the remaining 12. Thus this
capture is not a full 29-pair operational result.
