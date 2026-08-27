# PDF parser fuzzing

This directory is an independent Cargo workspace for nightly-only fuzzing. It does not participate in the stable root workspace, its lockfile, or the root formatting, Clippy, and test gates.

## Prerequisites

Install a nightly Rust toolchain and `cargo-fuzz`:

```console
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Build and run

Build a target:

```console
cargo +nightly fuzz build parser_entry
cargo +nightly fuzz build cmap_parser
cargo +nightly fuzz build content_stream_parser
```

Run a short smoke session with the checked-in seeds and the target's input limit:

```console
cargo +nightly fuzz run parser_entry -- -max_total_time=30 -max_len=65536
cargo +nightly fuzz run cmap_parser -- -max_total_time=30 -max_len=65536
cargo +nightly fuzz run content_stream_parser -- -max_total_time=30 -max_len=65536
```

An existing fixture directory can be supplied as an additional corpus without copying its PDFs into this workspace:

```console
cargo +nightly fuzz run parser_entry fuzz/corpus/parser_entry fixtures/external/japanese-typst -- -max_len=65536
```

Reproduce a saved failure by passing its artifact path and target:

```console
cargo +nightly fuzz run parser_entry fuzz/artifacts/parser_entry/crash-ARTIFACT -- -max_len=65536
cargo +nightly fuzz run cmap_parser fuzz/artifacts/cmap_parser/crash-ARTIFACT -- -max_len=65536
cargo +nightly fuzz run content_stream_parser fuzz/artifacts/content_stream_parser/crash-ARTIFACT -- -max_len=65536
```

## Target contract

`parser_entry` rejects inputs larger than 64 KiB before allocating an `Arc`, then calls the public `PdfParser::parse` facade with finite limits. Successful parses exercise the neutral PDF version, trailer, page list, and at most 16 page dictionaries.

`cmap_parser` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production ToUnicode parser at source widths one through four and the identity CID encoding parser. Every parser call is limited to 1,024 entries, four-byte source codes, and 4,096 output Unicode scalars. Successful parses must remain within the entry limit, and successful identity encodings must report a source width from one through four.

`content_stream_parser` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production content stream parser first with the input as one fragment and then with two fragments split at the midpoint, including end-of-stream validation after successful fragment sequences. Each strategy receives fresh shared budgets and is limited to 1,024 operators, 256 pending operands, 1,024 array elements, 4,096 operand nodes, 32 nesting levels, and 64 KiB of string data. Successful fragment results must remain within the cumulative operator limit, and each returned operation index must be below that limit; indexes may restart in each fragment.

Malformed, unsupported, unresolved, backend, and resource-limit results are accepted outcomes. The fuzzing oracle reports only panics, aborts, hangs, sanitizer findings, and violated success invariants. It does not require arbitrary bytes to parse successfully.

## Seed corpus

The `parser_entry` corpus contains small `.pdf` files that cover basic PDF framing and malformed object syntax. The `cmap_parser` corpus contains hand-written `.cmap` files covering a one-byte ToUnicode mapping, a full-domain identity CID map, a truncated mapping block, and a declared mapping count above the finite entry limit. The `content_stream_parser` corpus contains hand-written `.content` fragments covering text operators, nested dictionaries and tagged content, an inline image, and incomplete syntax.

Generated build output and failure artifacts under `fuzz/target` and `fuzz/artifacts` are intentionally ignored. Minimize and inspect a reproducing artifact before promoting it into the matching `fuzz/corpus/<target>` directory. Only curated `.pdf`, `.cmap`, and `.content` seeds are tracked; the coverage-increasing units libFuzzer writes back into corpus directories are ignored and regenerable. Never commit sensitive or externally licensed input.
