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

- **Layout-tolerant, exact text diff.** Line-wrap and page-break changes are
  ignored; replacements, insertions, deletions, and paragraph moves are
  reported exactly.
- **Honest results.** Every change is classified as established, inferred, or
  tentative. Unsupported and uncertain content is reported as unresolved, and a
  distinct exit code marks incomplete comparisons.
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
Typed changes: 0
Inferred changes: 1
...
Text: old 222/222, new 222/222 source references compared; complete
...
Inferred change: Paragraph (page 1) -> Paragraph (page 1)
  - "Release 10 remains available during the transition period."
  + "Release 20 remains available during the transition period."
  Mandatory changed positions: old 1, new 1. Displayed values include context.
```

Common tasks:

```bash
pdfdelta old.pdf new.pdf                        # compare all default channels
pdfdelta old.pdf new.pdf --channels text        # compare text only
pdfdelta old.pdf new.pdf -j report.json         # also write a JSON report
pdfdelta old.pdf new.pdf --review review-out    # write a static HTML review
pdfdelta -q old.pdf new.pdf; echo $?            # exit status only, for CI
pdfdelta inspect document.pdf --glyphs          # show extracted glyph evidence
```

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Complete comparison, no established content changes |
| `1` | Complete comparison, established content changes found |
| `2` | Error (I/O, malformed input, resource limit, report writing) |
| `3` | Incomplete comparison: some content could not be compared |

See the [command-line reference](docs/usage.md) for all options, channels,
report formats, review bundles, resource limits, and encrypted PDFs.

## How it works

```text
PDF bytes → parser backend → glyph extraction → layout reconstruction
          → cross-document alignment → exact diff → changes + coverage + unresolved regions
```

Alignment is tolerant, so it can match content across layout drift; the final
diff is exact. Raw evidence such as glyph codes, geometry, and content-stream
provenance is kept at every stage, so each reported change can be traced back
to the PDF. See [How pdfdelta works](docs/how-it-works.md) for details.

## Limitations

- No OCR: text in images and scanned pages is not compared, and such pages stay
  unresolved.
- Supports a bounded subset of PDF fonts, encodings, and layouts. Complex
  multi-column, right-to-left, and non-horizontal layouts often end up
  unresolved rather than compared.
- Relationship comparison (links, footnotes) and form appearance checks are
  incomplete.
- Large real-world documents may hit resource limits; `--limit-scale` raises
  the comparison budgets.

The full list is in [`docs/limitations.md`](docs/limitations.md).

## Documentation

| Document | Contents |
| --- | --- |
| [`docs/usage.md`](docs/usage.md) | Command-line reference |
| [`docs/how-it-works.md`](docs/how-it-works.md) | Pipeline, evidence channels, and resource limits |
| [`docs/limitations.md`](docs/limitations.md) | Known gaps and unsupported inputs |
| [`docs/agent-review.md`](docs/agent-review.md) | Agent review bundle contract |
| [`docs/benchmarks.md`](docs/benchmarks.md) | Acceptance tests and benchmark tooling |
| [`docs/SPEC.md`](docs/SPEC.md) | Technical specification and roadmap (Japanese) |

## Project layout

```text
crates/pdfdelta-core    Comparison library and neutral data model
crates/pdfdelta-cli     The pdfdelta command-line tool
crates/pdfdelta-bench   Benchmark and evaluation tooling (pdfbench)
```

## Contributing

Contributions are welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the
development setup and quality checks, and [`AGENTS.md`](AGENTS.md) for the
architecture rules.

## License

pdfdelta is licensed under the [MIT License](LICENSE). The Adobe Glyph List
2.0 data embedded in `pdfdelta-core` is distributed under its original terms;
see [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
