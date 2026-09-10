# PDF parser fuzzing

This directory is an independent Cargo workspace for nightly-only fuzzing. It does not participate in the stable root workspace, its lockfile, or the root formatting, Clippy, and test gates.

## Prerequisites

Install a nightly Rust toolchain and `cargo-fuzz`:

```console
rustup toolchain install nightly
cargo install cargo-fuzz
```

`cargo fuzz`'s default build enables AddressSanitizer and sancov coverage
through nightly-only `-Zsanitizer` flags, so the documented tasks and the
`Fuzz smoke` workflow build with `cargo +nightly`. A sanitizer-free
`cargo fuzz build --sanitizer none` also builds on stable Rust, but it drops
sanitizer findings and is not the supported configuration.

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

The `Fuzz smoke` GitHub Actions workflow runs the same session on a schedule and
on pull requests that touch fuzzed code; it installs nightly Rust and the pinned
`cargo-fuzz` release itself, so local runs remain the only place where those
prerequisites are manual. Before pushing changes under `fuzz/`, run
`mise run fuzz-check` to format-check and lint this workspace on stable Rust; the
workflow runs those checks too.

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
mise run fuzz-run -- layout_pipeline fuzz/artifacts/layout_pipeline/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- graph_pipeline fuzz/artifacts/graph_pipeline/crash-ARTIFACT -- -max_len=65536
mise run fuzz-run -- text_comparison fuzz/artifacts/text_comparison/crash-ARTIFACT -- -max_len=65536
```

## Target contract

`parser_entry` rejects inputs larger than 64 KiB before allocating an `Arc`, then calls the public `PdfParser::parse` facade with finite limits. Successful parses exercise the neutral PDF version, trailer, page list, and at most 16 page dictionaries, then resolve at most 64 distinct object references discovered in the trailer, page dictionaries, and resolved objects. For each reference it requires `terminal_reference` and `resolve` to agree with `resolve_with_terminal`, checks that a failed resolution also has no terminal reference, calls `raw_stream` and `decoded_stream`, and requires successful stream decodes to stay within the configured 64 KiB limit. Every `PdfObject::String` reached while walking the returned dictionaries and objects must either fail `decode_text_string` or decode to at most 4 KiB without panicking.

`cmap_parser` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production ToUnicode parser at source widths one through four and the identity CID encoding parser. Every parser call is limited to 1,024 entries, four-byte source codes, and 4,096 output Unicode scalars. Successful parses must remain within the entry limit, and successful identity encodings must report a source width from one through four.

`content_stream_parser` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production content stream parser first with the input as one fragment and then with two fragments split at the midpoint, including end-of-stream validation after successful fragment sequences. Each strategy receives fresh shared budgets and is limited to 1,024 operators, 256 pending operands, 1,024 array elements, 4,096 operand nodes, 32 nesting levels, and 64 KiB of string data. Successful fragment results must remain within the cumulative operator limit, and each returned operation index must be below that limit; indexes may restart in each fragment.

`font_decoder` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It builds fixed, bounded backend-neutral font skeletons and derives only selected mapping, width, fallback, and decode fields from the fuzz bytes, without introducing a generic object wire format or custom PDF parser. It exercises the production `FontDecoder` load and decode paths for both simple and Type0 fonts, covering ToUnicode priority, Differences and fallback handling, width and default-width selection, fixed source widths, odd-byte rejection, and explicit unmapped preservation. Every load and decode uses finite `FontDecoderLimits` with at most 16 indirections, 256 simple-width entries, 64 CID-width entries, 8 KiB of decoded font bytes, 512 CMap entries, and 1,024 output scalars, plus explicit budgets of 1,024 output glyphs and 4,096 mapped-text bytes. Successful decodes must keep glyph count and mapped-text bytes within those budgets, preserve the input byte-for-byte across concatenated raw codes, use the expected raw-code width for the fixture, keep all metrics finite with positive vertical extent, and keep unmapped glyphs explicit rather than as empty text or U+FFFD.

`canonical_yaml` rejects inputs larger than 64 KiB in both the target and the feature-gated bench facade. It exercises the production canonical YAML parser with tight streaming budgets and explicit rejection of anchors, aliases, tags, and merge keys. Every parse is limited to a single document, at most 512 nodes, 2,048 events, depth 32, zero anchors and aliases, 16 KiB of scalar bytes, and zero merge keys, with alias replay fully disabled. Successful parses must keep section, paragraph, and render-line counts within the documented fixture bounds and preserve validated text and identifier limits.

`glyph_extraction` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It exercises the production parsing and glyph extraction pipeline via `ParserBackedGlyphSource` with tight finite budgets: at most 512 objects, recursion depth 8, 64 KiB per-stream and 256 KiB total object-stream bytes, 4 pages, 1,024 glyphs, Form depth 4, nesting depth 16, 2,048 operators, 1,024 stream invocations, 256 KiB total decoded bytes, 256 operand stack slots, 1,024 array elements, 4,096 operand nodes, 32 fonts, 512 CMap entries, 1,024 CID width entries, and 64 KiB of string data. Successful extractions must keep glyph count, issue scope invariants, and glyph geometry and provenance contracts within those budgets while preserving explicit unmapped handling.

`layout_pipeline` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It builds a synthetic `Document<Glyph>` of at most 32 glyphs and 8 straight vector lines over at most 3 pages from the fuzzed bytes, with finite geometry, positive font sizes, unit writing directions, and mixed mapped, multi-scalar, CJK, control, and unmapped text. It runs the production line reconstruction, region partitioning per page, and the full `compare_glyph_documents` pipeline with default options. Comparing the constructed document with itself must report no exact or formatting changes, and a successful glyph-overlay render must start with `<svg`; reconstruction, comparison, and render errors are accepted outcomes.

`graph_pipeline` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It builds the same synthetic `Document<Glyph>` plus up to three tiny synthetic renderer regions, overlapping structure elements, text and button form fields, annotations, and occasional external OCR evidence, ingests them into an `EvidenceStore` and `DocumentGraph`, then compares the store with itself through `compare_document_views` with default document limits. A successful identity comparison must not report a typed operation through the shared solver. Multi-byte inputs also split into two documents whose stores are compared; that comparison must serialize and report internally consistent channel coverage. Evidence, graph, and comparison errors are accepted outcomes.

`text_comparison` rejects inputs larger than 64 KiB in both the target and the feature-gated core facade. It splits the input in half and builds two independent synthetic `Document<Glyph>` values, then compares them in both directions through the extraction-outcome pipeline with default options. A successful comparison must summarize, render as text, serialize as parseable JSON, report one summary entry per exact change, match the low-confidence change count, keep every change occurrence and unresolved region side-attributed, and never report an empty change occurrence. Comparison errors are accepted outcomes.

Malformed, unsupported, unresolved, backend, and resource-limit results are accepted outcomes. The fuzzing oracle reports only panics, aborts, hangs, sanitizer findings, and violated success invariants. It does not require arbitrary bytes to parse successfully.

## Seed corpus

The `parser_entry` corpus contains small `.pdf` files that cover basic PDF framing and malformed object syntax. The `cmap_parser` corpus contains hand-written `.cmap` files covering a one-byte ToUnicode mapping, a full-domain identity CID map, a truncated mapping block, and a declared mapping count above the finite entry limit. The `content_stream_parser` corpus contains hand-written `.content` fragments covering text operators, nested dictionaries and tagged content, an inline image, and incomplete syntax. The `font_decoder` corpus contains compact hand-written `.bin` inputs covering a simple WinAnsi font with explicit widths and fallback, a partial ToUnicode mapping with Differences and unmapped preservation, a Type0 Identity-H font with width handling and odd-byte rejection, and a vertical or bounded custom identity mapping. The `canonical_yaml` corpus contains a minimal valid canonical YAML document covering title, sections, and paragraphs within the documented fixture bounds. The `glyph_extraction` corpus contains a minimal valid PDF with one page and 11 glyphs covering the success path within the tight extraction budgets. `layout_pipeline`, `graph_pipeline`, and `text_comparison` have no tracked seed corpus: their synthetic documents are derived from arbitrary bytes, and the coverage-increasing units libFuzzer writes back are ignored and regenerable.

Generated build output and failure artifacts under `fuzz/target` and `fuzz/artifacts` are intentionally ignored. Minimize and inspect a reproducing artifact before promoting it into the matching `fuzz/corpus/<target>` directory. Only curated `.pdf`, `.cmap`, `.content`, `font_decoder` `.bin`, and `canonical_yaml` `.yaml` seeds are tracked; the coverage-increasing units libFuzzer writes back into corpus directories are ignored and regenerable. Never commit sensitive or externally licensed input.
