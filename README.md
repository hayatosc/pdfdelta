# pdfdelta

[![CI](https://github.com/hayatosc/pdfdelta/actions/workflows/ci.yml/badge.svg)](https://github.com/hayatosc/pdfdelta/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Compare what actually changed between two PDFs, and see what could not be
compared.**

`pdfdelta` is a command-line tool that compares the content of two PDF
documents. Line wrapping, page breaks, and small layout shifts do not show up
as changes, while a real edit such as `10 days` → `20 days` is reported
exactly. When the evidence is not good enough to decide, `pdfdelta` reports
the region as *unresolved* instead of claiming nothing changed.

> [!WARNING]
> pdfdelta is early-stage software (0.1). It handles born-digital text PDFs in
> a bounded subset of fonts and layouts. On large real-world documents, many
> comparisons are still incomplete. See [Limitations](#limitations).

## Why pdfdelta?

A PDF stores drawing instructions, not paragraphs. Extracting text and running
`diff` turns every reflowed line and page break into noise. Comparing rendered
pages has the opposite problem: a new font or margin makes every pixel differ
even when the wording is the same.

pdfdelta keeps each glyph's geometry and PDF provenance, reconstructs the
document structure, aligns matching content across layout changes, and then
diffs the text exactly. This is useful when reviewing contracts, policies,
standards, and other documents where every wording change matters.

## Features

- **Layout-tolerant text diff.** One token-stream engine ignores line-wrap and
  page-break changes and reports replacements, insertions, deletions, and moves
  from a single verified script.
- **Honest results.** Every change carries a confidence and ambiguity marker,
  and formatting-only evidence is reported separately. Unsupported and
  uncertain content is reported as unresolved, and a distinct exit code marks
  incomplete comparisons.
- **More than text.** Compares embedded images (by pixel hash), AcroForm field
  values, tables, and tagged PDF structure through selectable *channels*.
- **CI-friendly.** Stable exit codes, a quiet mode, and a versioned JSON report.
- **Reviewable.** Writes a static HTML review bundle, or a bounded review bundle
  that an AI agent can query piece by piece.
- **CJK support.** Handles horizontal Japanese text in common CID font
  encodings, plus a bounded subset of vertical text.
- **Built for untrusted input.** Pure Rust, no network access, explicit
  resource limits, and resource-limited child processes for extraction and
  rendering.

## Installation

pdfdelta is not yet published to crates.io. Build it from source with a recent
stable Rust toolchain (the tested version is pinned in
[`rust-toolchain.toml`](rust-toolchain.toml)):

```bash
cargo install --locked --git https://github.com/hayatosc/pdfdelta pdfdelta-cli
```

Or from a local checkout:

```bash
git clone https://github.com/hayatosc/pdfdelta
cd pdfdelta
cargo install --locked --path crates/pdfdelta-cli
```

Resource-limited extraction and rendering workers run on Linux and macOS. On other
platforms those parts of a comparison are reported as unsupported.

## Quick start

```console
$ pdfdelta --channels text old.pdf new.pdf
Document comparison: complete
Typed changes: 1
Inferred changes: 0
Scope content changes (B; non-owning): 0
Inferred scope changes (C; non-owning): 0
Text: old 32/32, new 32/32 source references compared; complete
...
Text channel (flow): complete; 1 detected changes; 0 formatting-only changes
```

Common tasks:

```bash
pdfdelta old.pdf new.pdf                        # compare all default channels
pdfdelta old.pdf new.pdf --channels text        # compare text only
pdfdelta old.pdf new.pdf -j report.json         # also write a JSON report
pdfdelta old.pdf new.pdf --native-text-only --full-json -j full.json.gz  # full flow sources
pdfdelta old.pdf new.pdf --trace-json trace.json # phase-by-phase trace
pdfdelta old.pdf new.pdf --review review-out    # write a static HTML review
pdfdelta -q old.pdf new.pdf; echo $?            # exit status only, for CI
pdfdelta inspect document.pdf --glyphs          # show extracted glyph evidence
```

The text channel always uses the flow engine. `--engine flow` is accepted for
compatibility and is a no-op; `--engine proof` exits `2` with a message naming
the flow replacements. Old proof review bundles remain readable with
`pdfdelta review list/show/render/import`. `--compact-json`, `--full-json`,
`--trace-json`, `--review`, and `--agent-review` are flow capabilities.

For documents that are mostly continuous prose, `pdfdelta flow` reports a
single token-level script that ignores line wrapping and page breaks:

```console
$ pdfdelta flow old.pdf new.pdf
flow summary
  old: 32 tokens
  new: 32 tokens
  equal: 31 tokens
  ...
  content changes: 1
  moved changes: 0
  detected changes: 1
  changes: 1
  old extraction: complete
  new extraction: complete
  old order: 1 pages, 0 stream, 0 baseline, 0 uncertain
  new order: 1 pages, 0 stream, 0 baseline, 0 uncertain
  old glyph conservation: 222/222
  new glyph conservation: 222/222
  furniture: 0 old, 0 new masked lines

1. replace old 1 -> new 1 [confidence: medium; ambiguity: numeric_only]
   old: schedule details for the upcoming cycle . Release [10] remains available during the transition period . All
   new: schedule details for the upcoming cycle . Release [20] remains available during the transition period . All
```

This engine is the text channel of the main command. `pdfdelta flow` exposes
the same token-level script directly. See
[Flow diff](docs/usage.md#flow-diff) for options, exit codes, and limits, and
[Current limitations](docs/limitations.md) for what flow does not report.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No detected difference and every selected channel examined |
| `1` | A difference was detected (it may still be incomplete) |
| `2` | Error (I/O, malformed input, resource limit, report writing) |
| `3` | No difference detected and something was unexamined, or evidence is broken |

See the [command-line reference](docs/usage.md) for all options, channels,
report formats, review bundles, resource limits, and encrypted PDFs.

## How it works

```text
PDF bytes → parser backend → glyph extraction → layout reconstruction
          → flow token stream → verified token diff
          → changes + formatting evidence + channel coverage + unresolved regions
```

Layout reconstruction is reversible, so the token stream can recover from
imperfect line and block boundaries; the final token diff is exact and replayed
before it is reported. Raw evidence such as glyph codes, geometry, and
content-stream provenance is kept at every stage, so each reported change can
be traced back to the PDF. See [How pdfdelta works](docs/how-it-works.md) for
details.

## Limitations

- No OCR: text in images and scanned pages is not compared, and such pages stay
  unresolved. The flow text channel also becomes incomplete when non-text paint
  (paths, images, shadings) changes, because outlines may be text.
- Supports a bounded subset of PDF fonts, encodings, and layouts. Complex
  multi-column, right-to-left, and non-horizontal layouts often end up
  unresolved rather than compared.
- Relationship comparison (links, footnotes) and form appearance checks are
  incomplete. Because rendered page rasters are review context and relation
  endpoints are not established, the default channels report exit `3`
  (`unexamined_evidence`) even for identical or purely reflowed documents;
  use `--channels text` or `--native-text-only` for the text verdict.
- Adjacent-block transpositions are reported as one move, preferring the
  smaller structural unit that is no less informative (a moved paragraph rather
  than a heading and its paragraph). A same-row transposition between
  side-by-side runs with an unchanged visual order is reported as reading-order
  evidence instead of a content move. Both choices are quality preferences, not
  author intent; equal-span swaps keep the alignment choice. Formatting reasons
  are approximate. Channel rendering budgets each page by CPU time (10 s) and
  each input document by cumulative child CPU (120 s); image decoding budgets
  60 s per worker call. The wall-clock safety stops (120 s per page, 600 s per
  document, 300 s per image worker) can still expire under extreme load and are
  reported as a distinct resource-limit reason.
- Large real-world documents may hit resource limits; `--limit-scale` raises
  flow extraction count limits, the shared footer-matching work limit, and the
  render/image CPU and wall budgets. Full
  JSON captures stream per-glyph sources through a temporary side cache and
  stay near the compact run's memory (about 2.2 GB on the largest pair), but
  the gzip output is still hundreds of megabytes. Agent review plans at most
  4,096 cases by default; `--agent-case-budget` raises the total cap, and a
  plan above one 512 MiB bundle is published as numbered part bundles.

The full list is in [`docs/limitations.md`](docs/limitations.md).

## Documentation

| Document | Contents |
| --- | --- |
| [`docs/usage.md`](docs/usage.md) | Command-line reference |
| [`docs/how-it-works.md`](docs/how-it-works.md) | Pipeline, evidence channels, and resource limits |
| [`docs/limitations.md`](docs/limitations.md) | Known gaps and unsupported inputs |
| [`docs/agent-review.md`](docs/agent-review.md) | Agent review bundle contract |
| [`docs/benchmarks.md`](docs/benchmarks.md) | Acceptance tests and benchmark tooling |
| [`docs/SPEC.md`](docs/SPEC.md) | Technical specification and design contracts |

## Project layout

```text
crates/pdfdelta-core    Extraction, layout, evidence, and comparison library
crates/pdfdelta-diff    Token-stream diff engine
crates/pdfdelta-cli     The pdfdelta command-line tool
crates/pdfdelta-bench   Benchmark and evaluation tooling (pdfbench)
```

## Contributing

Contributions are welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the
development setup and quality checks, and [`AGENTS.md`](AGENTS.md) for the
architecture rules.

## License

pdfdelta is licensed under the [MIT License](LICENSE). The Adobe Glyph List
2.0 data and the Apache-2.0 pdf.js glyph-name supplement embedded in
`pdfdelta-core` are distributed under their original terms; see
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
