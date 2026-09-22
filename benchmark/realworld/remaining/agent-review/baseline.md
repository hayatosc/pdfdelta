# Agent review: fixed evaluation conditions

This records the conditions the agent review work is evaluated against, so a
later measurement compares like with like. It is a registration of the contract,
not a measurement: no panel capture was performed here.

## What was and was not established

The "0 of 36 complete comparisons" figure this work refers to is a report from
the repository owner about their own working tree. It was **not** reproduced in
the environment that implemented the review packets, because that environment
does not contain the panel's input PDFs (see *Inputs* below). Nothing in this
work changes that figure, and nothing here should be read as having confirmed
it.

Exporting a review bundle cannot change it either: the export is a projection of
a comparison that has already finished, and
`crates/pdfdelta-cli/tests/agent_review_cli.rs` pins that the JSON report and
the exit status are identical with and without `--agent-review`.

## Engine revision

- Branch: `claude/trusting-davinci-1bm08w`
- Base commit before this work: `bfe0b1b`
- Toolchain: pinned by `rust-toolchain.toml`; dependencies pinned by `Cargo.lock`

A working-tree build needs its source snapshot recorded in addition to the
commit label, because an uncommitted change is part of what produced a capture.

## Panel and inputs

- Panel manifest: `benchmark/realworld/followup/panel.json`
  (`version: 1`, 36 pairs, `baseline_sha256`
  `ddb9c0cd164314f069fb670fbc9b5b8465d245cc62417751539064144a7909a4`)
- Targets: `benchmark/realworld/followup/targets.json`
- Expected input hashes: each pair's `old.sha256` and `new.sha256` inside the
  panel manifest
- Input cache: `benchmark/realworld/cache/next-dev/<pair>-{old,new}.pdf`

The corpus is not vendored. The cache directory is absent in this environment,
so the panel cannot be replayed here; `benchmark/realworld/fetch.sh` downloads
the registered sources, and every download must be checked against the expected
hash before use.

## Route and budgets

- Common route: `--channels text`
- `--limit-scale 1`
- Capture timeout: 180 seconds per pair
- Native acquisition defaults: 35 seconds, 128 MiB response bound
- Rendering defaults: 72 dpi, 5 s per page, 30 s per document, 8,000,000 pixels,
  and the retained raster budget from `EvidenceLimits::default()`

## Completion contract

A pair counts as complete only with a valid exit status of 0 or 1, complete
discovery inventories on both sides of every selected channel, zero unexamined
source references, and resolved comparison search. This is exactly what
`document_coverage` and `DocumentViewComparison::search_resolved` already
report; the packets do not redefine it.

An external reviewer's answer never contributes to it. Packet quality is
reported separately from strict comparison results, and a decision stored beside
a bundle changes neither `comparison_complete`, nor coverage denominators, nor
exact-event recall.

## Replaying the panel

```sh
# 1. Populate the cache and verify every expected hash.
benchmark/realworld/fetch.sh [CACHE_DIR]
cargo run -p pdfdelta-bench -- revisions --cache-dir <CACHE_DIR> --checksums-only

# 2. Capture each pair with the same frozen build and arguments, into a new
#    output directory per repetition.
python benchmark/realworld/next/development/capture-comparisons.py \
  target/release/pdfdelta <CACHE_DIR> <new-output-dir> \
  --manifest <inputs.json for that pair's set> \
  --implementation <label> --route text \
  --pair <pair-id> [--pair <pair-id> ...]
```

`panel.json` is the registry, not the capture script's manifest: each pair
carries its own `historical_inputs` path and `historical_inputs_sha256`, and the
capture script requires one of those `inputs.json` files plus explicit `--pair`
identifiers. Binding every panel pair to its own inputs file is part of
replaying the panel; assuming one shared manifest would silently capture a
different set.

Count all 36 pairs, including failed acquisition, empty-source, timeout and
unsupported results. Record per-pair completion, source and inventory residuals,
runtime and peak memory, and keep the resolved argument arrays with the captured
binary's digest.
