# pdfdelta

`pdfdelta` is a Rust command-line tool that compares two PDFs by their content
instead of their internal representation. It reconstructs glyphs, text lines,
blocks, and document relationships from the original PDF evidence, aligns
corresponding content despite reflow and repagination, and reports the exact
text differences it can prove. Content it cannot interpret or compare is
reported as unresolved instead of being silently treated as unchanged.

```text
PDF bytes -> evidence extraction -> layout reconstruction -> alignment -> exact diff
          -> changes, candidates, coverage, and unresolved regions
```

## Status

The primary completion metric is the visible native-text route
(`--native-text-only`). On the frozen 36-pair real-world panel it confirms **at
least 3/36** pairs complete on the final build, each captured twice with
identical reports: Schedule SE, plus two pairs from independent producers whose
old side carries no visible native text. The full 36-pair panel was not
re-captured on that build, and the default multi-channel route (text, visual,
forms, relationships) is not document-wide complete. Native-only mode compares
visible text already stored in the PDF; it does not recognize or compare words
inside images. A complete native-only result therefore does not establish
that every part of the document was compared. In the default mode,
unrecognized image text and unresolved relationships keep the comparison
incomplete.

See the [final completion record](benchmark/realworld/remaining/completion-investigation/native-completion-final-v1.md)
and the [investigation index](benchmark/realworld/remaining/completion-investigation/README.md)
for evidence, limitations, and remaining work.

## Requirements and build

The workspace requires stable Rust with Edition 2024 support; only the optional
fuzz crate needs nightly.

```bash
# Build the CLI; the binary is target/release/pdfdelta
cargo build --release -p pdfdelta-cli

# Or install it from this checkout (the crate is not published to crates.io)
cargo install --path crates/pdfdelta-cli
```

The repository also provides [mise](https://mise.jdx.dev/) tasks, including
`mise run ci` (the full quality gate), `mise run pdfdelta -- --help`, and
`mise run example-glyph-comparison`.

## Quick start

```bash
# Compare two PDFs and print a human-readable diff
pdfdelta old.pdf new.pdf

# Machine-readable report for scripting; text diff stays on standard output
pdfdelta old.pdf new.pdf --json report.json

# CI: no report output, only the exit code
pdfdelta --quiet --strict old.pdf new.pdf
```

## Comparison options

| Option | Meaning |
| --- | --- |
| `OLD_PDF`, `NEW_PDF` | The two documents to compare. Use `-` for standard input on either side, but not both. |
| `--channels <LIST>` | Channels to compare through the common pipeline. Default `text,visual,forms,relations`; `presentation` can be added explicitly. Comma-separated. |
| `--native-text-only` | Compare extracted native glyphs only, using the legacy native report contract. Cannot be combined with `--channels`. |
| `-j, --json <PATH>` | Write the versioned JSON report to a new path. Refuses to replace an existing file. |
| `-o, --output <PATH>` | Write the human-readable text report to a new file instead of standard output. |
| `--trace-json <PATH>` | Write a phase-by-phase diagnostic trace to a new file. Not part of the normal report. |
| `-q, --quiet` | Suppress the standard-output report for exit-code-only use. |
| `-s, --strict` | Compatibility alias for the default behavior of returning exit code 3 for an incomplete comparison. |
| `--color <auto\|always\|never>` | ANSI color for the text report. `auto` (default) colorizes only a terminal. |
| `--limit-scale <FACTOR>` | Raise comparison budgets (n-gram elements, alignment candidate visits and DP cells, diff tokens, edit distance, assessment work) by a factor of at least 1. Parser and extraction limits do not change. |
| `--review <DIR>` | Write a static HTML review bundle with the source PDFs, page previews, full JSON, and glyph evidence. The directory must not exist. Not available with `--native-text-only`. |
| `--old-password-file <PATH>`, `--new-password-file <PATH>` | Read a PDF password from a file. Passwords are never accepted as argument values or written to reports. |
| `--old-font-identity <BASE_FONT=IDENTITY>`, `--new-font-identity <BASE_FONT=IDENTITY>` | Assert that an unembedded CID font is the same external font on both sides when no embedded program or ToUnicode map exists. A caller guarantee, not font discovery. |
| `--extraction-cache-dir <DIR>` | Reuse cached glyph extraction keyed by file contents and every extraction input. A missing or outdated entry falls back to a fresh extraction, so results are identical with or without the cache. The directory is a trust boundary. |

`--native-text-only` and `--channels` are different contracts:

- The default `--channels` route compares the selected evidence channels and
  writes a version 2 multi-channel report. A missing or unexamined channel
  keeps the comparison incomplete, including image-only pages.
- `--native-text-only` keeps the earlier native-glyph adapter and its version
  11 report. It compares visible native glyph text only and cannot be combined
  with channel selection.

## Other commands

```bash
# Inspect one PDF: backend summary, objects, glyph evidence, or an SVG overlay
pdfdelta inspect document.pdf --backend-info
pdfdelta inspect document.pdf --objects
pdfdelta inspect document.pdf --glyphs
pdfdelta inspect document.pdf --svg overlay.svg

# Generate a shell completion script
pdfdelta completions bash   # bash, elvish, fish, powershell, zsh
```

`inspect` accepts `--password-file` and `--font-identity` like the comparison
command. All inspection output escapes control and bidirectional characters
that originate from the PDF, so hostile files cannot rewrite the terminal.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Complete comparison with no established content change |
| 1 | Complete comparison with at least one established content change |
| 2 | Execution or report-writing error (I/O, malformed input, fatal backend failure, resource limit before any usable result) |
| 3 | Incomplete comparison, including one that also has established changes |

An incomplete comparison takes precedence over a detected change. Errors take
precedence over both. JSON reports state content-change status and comparison
completeness independently, so scripts that need both should read the report
rather than only the exit code.

## Reports

Text output depends on the route:

- The default `--channels` route renders a bounded, terminal-safe summary of
  typed operations and unresolved evidence: per-node or per-field operations,
  old/new values or regions, inferred-correspondence labels, unresolved
  reasons, and coverage counts.
- `--native-text-only` renders the contextual unified diff: a summary line,
  `---`/`+++` headers, `@@ page N ... @@` hunks with one-based page numbers,
  `-`/`+` lines, bounded context, `~ moved` markers, `TENTATIVE` candidate
  hunks, and `?` unresolved regions.

Both views escape PDF-derived text so hostile glyph mappings cannot drive the
terminal.

The JSON reports retain more than the text view: established changes,
tentative candidates, changed regions without a proven position, formatting
observations, unresolved regions, source coverage, per-side extraction
completeness, source ownership, correspondence dependencies, and search
completeness. `--trace-json` adds bounded per-phase diagnostics.

All named output paths are published atomically to new destinations; existing
files and input PDFs are never overwritten. Trace and report timings are
observations, not benchmark claims.

## Supported scope and limitations

- **Native text.** The extractor supports a bounded subset of real PDFs:
  classic xref tables and xref streams, object streams, incremental updates,
  inherited page resources, Form XObjects, borrowed-password decryption,
  FlateDecode, Type 1/Type1C/MMType1 and TrueType simple fonts, axis-aligned
  Type 3 fonts, Identity-H and a bounded Identity-V subset, bounded custom
  Type 0 CMaps, and standard ToUnicode/encoding handling. Unmapped glyphs are
  preserved with a font identity instead of being replaced by empty text.
  Unsupported fonts, clipping, or operators are reported as unresolved.
- **Visible text.** Only painting text is compared. Text in non-painting render
  modes (for example render mode 3) stays outside the visible-content
  comparison, while a visible text layer, including an existing OCR layer that
  paints glyphs, is compared. Image pixels, path lettering, and handwriting are
  not recognized or compared as text.
- **Images.** The visual channel decodes embedded images and compares SHA-256
  hashes over normalized pixels. Equal hashes match as a multiset; remaining
  images at a unique identical placement are inferred changes, and ambiguous
  ones stay unresolved. Image text is not recognized.
- **Forms.** Saved AcroForm field values are compared by field name; widget
  crops and declared appearance states are retained. Export-option
  interpretation, XFA, and full appearance rendering remain unresolved or
  unimplemented.
- **Relationships.** The relation channel compares reference edges that are
  actually supplied, such as structure and annotation targets. General link or
  footnote target interpretation is not implemented.
- **Extraction completeness.** Completeness is per route and per channel. In
  the default `--channels` route, a page with unrecognized paint keeps its
  text inventory incomplete even when native glyphs were extracted. In
  `--native-text-only`, images are outside the selected scope: an image-only
  page can have a complete, empty native-text inventory without its image
  content being compared. Incomplete pages, documents, or glyph gaps are reported as
  typed issues and return exit code 3; missing evidence is never converted
  into empty text.
- **Resource bounds.** Parser, extraction, comparison, rendering, and worker
  processes enforce explicit limits (input bytes, objects, recursion, decoded
  bytes, pages, glyphs, operators, CMap entries, candidates, DP cells,
  assessment work, output ranges, and child-process memory/time). Uninterpreted
  input remains unsupported or unresolved rather than becoming a partial
  success. `--limit-scale` raises comparison budgets only; it does not raise
  parser or extraction limits.

Development and verification details, including the generated matrix results,
the real-world revision panel, and extraction conformance, are recorded in the
[benchmark results index](benchmark/realworld/results/README.md). The five
initial release acceptance cases (line-wrap invariance, page-break invariance,
one exact text replacement, one paragraph insertion, one paragraph deletion)
are covered by the generated 48-record matrix that `pdfbench verify` checks;
the five named cases pass on both renderers. The frozen real-world annotations
drive the separate revision-diff evaluation and do not extend those five
cases.

## Development

```text
crates/pdfdelta-core   Pure comparison library and neutral evidence model
crates/pdfdelta-cli    The pdfdelta command-line interface
crates/pdfdelta-bench  Fixture generation, evaluation, and pdfbench
fuzz/                  Optional nightly fuzz targets
```

```bash
# The quality gate: formatting, Clippy, workspace tests, all-feature library
# tests, rustdoc, and the benchmark verification
mise run ci

# Or run the checks directly
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --lib
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
```

`crates/pdfdelta-core/examples/glyph_comparison.rs` demonstrates comparing
programmatically constructed `Document<Glyph>` fixtures without any PDF
dependency. See [`AGENTS.md`](AGENTS.md) for repository working agreements.

## License

The project code is licensed under the [MIT License](LICENSE). The Adobe Glyph
List 2.0 data embedded in `pdfdelta-core` is distributed under its original
terms; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Specification

[`SPEC.md`](SPEC.md) is the technical specification and roadmap.
