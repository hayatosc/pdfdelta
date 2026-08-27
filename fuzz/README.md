# PDF parser fuzzing

This directory is an independent Cargo workspace for nightly-only fuzzing. It does not participate in the stable root workspace, its lockfile, or the root formatting, Clippy, and test gates.

## Prerequisites

Install a nightly Rust toolchain and `cargo-fuzz`:

```console
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Build and run

Build the parser target:

```console
cargo +nightly fuzz build parser_entry
```

Run a short smoke session with the checked-in seeds and the target's input limit:

```console
cargo +nightly fuzz run parser_entry -- -max_total_time=30 -max_len=65536
```

An existing fixture directory can be supplied as an additional corpus without copying its PDFs into this workspace:

```console
cargo +nightly fuzz run parser_entry fuzz/corpus/parser_entry fixtures/external/japanese-typst -- -max_len=65536
```

Reproduce a saved failure by passing its artifact path:

```console
cargo +nightly fuzz run parser_entry fuzz/artifacts/parser_entry/crash-ARTIFACT -- -max_len=65536
```

## Target contract

`parser_entry` rejects inputs larger than 64 KiB before allocating an `Arc`, then calls the public `PdfParser::parse` facade with finite limits. Successful parses exercise the neutral PDF version, trailer, page list, and at most 16 page dictionaries.

Malformed, unsupported, unresolved, backend, and resource-limit results are accepted outcomes. The fuzzing oracle reports only panics, aborts, hangs, and sanitizer findings. It does not require arbitrary bytes to parse successfully.

Generated build output and failure artifacts under `fuzz/target` and `fuzz/artifacts` are intentionally ignored. Minimize and inspect a reproducing artifact before promoting it into `fuzz/corpus/parser_entry`. Only the curated `.pdf` seeds under `fuzz/corpus/parser_entry` are tracked; the coverage-increasing units libFuzzer writes back into that directory are ignored and regenerable. Never commit sensitive or externally licensed input.
