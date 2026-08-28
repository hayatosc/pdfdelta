# PDF parser fuzzing

This directory is an independent Cargo workspace for nightly-only fuzzing. It does not participate in the stable root workspace, its lockfile, or the root formatting, Clippy, and test gates.

## Prerequisites

Install a nightly Rust toolchain and `cargo-fuzz`:

```console
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Build and run

Build every target, or pass one target after `--` to build it alone:

```console
mise run fuzz-build
mise run fuzz-build -- parser_entry
```

Run a short smoke session with the checked-in seeds and the target's input limit:

```console
mise run fuzz-smoke
```

An existing fixture directory can be supplied as an additional corpus without copying its PDFs into this workspace:

```console
mise run fuzz-run -- parser_entry fuzz/corpus/parser_entry fixtures/external/japanese-typst -- -max_len=65536
```

Reproduce a saved failure by passing its artifact path and target:

```console
mise run fuzz-run -- parser_entry fuzz/artifacts/parser_entry/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- cmap_parser fuzz/artifacts/cmap_parser/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- content_stream_parser fuzz/artifacts/content_stream_parser/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- font_decoder fuzz/artifacts/font_decoder/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- canonical_yaml fuzz/artifacts/canonical_yaml/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- glyph_extraction fuzz/artifacts/glyph_extraction/crash-ARTIFACT -- -max_len=65536
```

## Target contract

`parser_entry` rejects inputs larger than 64 KiB before allocating an `Arc`, then calls the public `PdfParser::parse` facade with finite limits. Successful parses exercise the neutral PDF version, trailer, page list, and at most 16 page dictionaries.

`cmap_parser` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production ToUnicode parser at source widths one through four and the identity CID encoding parser. Every parser call is limited to 1,024 entries, four-byte source codes, and 4,096 output Unicode scalars. Successful parses must remain within the entry limit, and successful identity encodings must report a source width from one through four.

`content_stream_parser` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production content stream parser first with the input as one fragment and then with two fragments split at the midpoint, including end-of-stream validation after successful fragment sequences. Each strategy receives fresh shared budgets and is limited to 1,024 operators, 256 pending operands, 1,024 array elements, 4,096 operand nodes, 32 nesting levels, and 64 KiB of string data. Successful fragment results must remain within the cumulative operator limit, and each returned operation index must be below that limit; indexes may restart in each fragment.

`font_decoder` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It builds fixed, bounded backend-neutral font skeletons and derives only selected mapping, width, fallback, and decode fields from the fuzz bytes, without introducing a generic object wire format or custom PDF parser. It exercises the production `FontDecoder` load and decode paths for both simple and Type0 fonts, covering ToUnicode priority, Differences and fallback handling, width and default-width selection, fixed source widths, odd-byte rejection, and explicit unmapped preservation. Every load and decode uses finite `FontDecoderLimits` with at most 16 indirections, 256 simple-width entries, 64 CID-width entries, 8 KiB of decoded font bytes, 512 CMap entries, and 1,024 output scalars, plus explicit budgets of 1,024 output glyphs and 4,096 mapped-text bytes. Successful decodes must keep glyph count and mapped-text bytes within those budgets, preserve the input byte-for-byte across concatenated raw codes, use the expected raw-code width for the fixture, keep all metrics finite with positive vertical extent, and keep unmapped glyphs explicit rather than as empty text or U+FFFD.

`canonical_yaml` rejects inputs larger than 64 KiB in both the target and the feature-gated bench facade. It exercises the production canonical YAML parser with tight streaming budgets and explicit rejection of anchors, aliases, tags, and merge keys. Every parse is limited to a single document, at most 512 nodes, 2,048 events, depth 32, zero anchors and aliases, 16 KiB of scalar bytes, and zero merge keys, with alias replay fully disabled. Successful parses must keep section, paragraph, and render-line counts within the documented fixture bounds and preserve validated text and identifier limits.

`glyph_extraction` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production parsing and glyph extraction pipeline via `ParserBackedGlyphSource` with tight finite budgets: at most 512 objects, recursion depth 8, 64 KiB per-stream and 256 KiB total object-stream bytes, 4 pages, 1,024 glyphs, Form depth 4, nesting depth 16, 2,048 operators, 1,024 stream invocations, 256 KiB total decoded bytes, 256 operand stack slots, 1,024 array elements, 4,096 operand nodes, 32 fonts, 512 CMap entries, 1,024 CID width entries, and 64 KiB of string data. Successful extractions must keep glyph count, issue scope invariants, and glyph geometry and provenance contracts within those budgets while preserving explicit unmapped handling.

Malformed, unsupported, unresolved, backend, and resource-limit results are accepted outcomes. The fuzzing oracle reports only panics, aborts, hangs, sanitizer findings, and violated success invariants. It does not require arbitrary bytes to parse successfully.

## Seed corpus

The `parser_entry` corpus contains small `.pdf` files that cover basic PDF framing and malformed object syntax. The `cmap_parser` corpus contains hand-written `.cmap` files covering a one-byte ToUnicode mapping, a full-domain identity CID map, a truncated mapping block, and a declared mapping count above the finite entry limit. The `content_stream_parser` corpus contains hand-written `.content` fragments covering text operators, nested dictionaries and tagged content, an inline image, and incomplete syntax. The `font_decoder` corpus contains compact hand-written `.bin` inputs covering a simple WinAnsi font with explicit widths and fallback, a partial ToUnicode mapping with Differences and unmapped preservation, a Type0 Identity-H font with width handling and odd-byte rejection, and a vertical or bounded custom identity mapping. The `canonical_yaml` corpus contains a minimal valid canonical YAML document covering title, sections, and paragraphs within the documented fixture bounds. The `glyph_extraction` corpus contains a minimal valid PDF with one page and 11 glyphs covering the success path within the tight extraction budgets.

Generated build output and failure artifacts under `fuzz/target` and `fuzz/artifacts` are intentionally ignored. Minimize and inspect a reproducing artifact before promoting it into the matching `fuzz/corpus/<target>` directory. Only curated `.pdf`, `.cmap`, `.content`, `font_decoder` `.bin`, and `canonical_yaml` `.yaml` seeds are tracked; the coverage-increasing units libFuzzer writes back into corpus directories are ignored and regenerable. Never commit sensitive or externally licensed input.
