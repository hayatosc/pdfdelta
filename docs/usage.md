# Command-line reference

This page documents the `pdfdelta` command in detail. Run `pdfdelta --help` or
`pdfdelta <command> --help` for the authoritative option list of your build.

- [Comparing two PDFs](#comparing-two-pdfs)
- [Evidence channels](#evidence-channels)
- [Exit codes](#exit-codes)
- [Output](#output)
- [Review bundles](#review-bundles)
- [Resource limits and caching](#resource-limits-and-caching)
- [Encrypted PDFs and font identities](#encrypted-pdfs-and-font-identities)
- [Inspecting a single PDF](#inspecting-a-single-pdf)
- [Shell completions](#shell-completions)

## Comparing two PDFs

```text
pdfdelta [OPTIONS] <OLD_PDF> <NEW_PDF>
```

Either input may be `-` to read that PDF from standard input.

```bash
pdfdelta old.pdf new.pdf                       # human-readable report on stdout
cat old.pdf | pdfdelta - new.pdf               # read the old side from stdin
pdfdelta old.pdf new.pdf -o diff.txt           # write the text report to a new file
pdfdelta old.pdf new.pdf -j result.json        # write the JSON report to a new file
pdfdelta old.pdf new.pdf -j result.json.gz     # gzip-compressed JSON
pdfdelta -q old.pdf new.pdf                    # exit status only (CI)
pdfdelta old.pdf new.pdf --color always        # force ANSI color
```

| Option | Description |
| --- | --- |
| `--channels <LIST>` | Channels to compare: `text`, `visual`, `forms`, `relations`, `presentation`. Default: `text,visual,forms,relations`. |
| `--native-text-only` | Use the native-glyph text pipeline and its version 11 JSON report. Cannot be combined with `--channels` or `--review`. |
| `-o, --output <PATH>` | Write the human-readable report to a new file instead of stdout. |
| `-j, --json <PATH>` | Write the JSON report to a new file; a `.gz` suffix produces gzip. |
| `--trace-json <PATH>` | Write a phase-by-phase diagnostic trace. |
| `--review <DIR>` | Write a static HTML review bundle. |
| `--agent-review <DIR>` | Write an agent review bundle (see [`agent-review.md`](agent-review.md)). |
| `-q, --quiet` | Suppress the human-readable report on stdout. |
| `--color <auto\|always\|never>` | Colorize the report. `auto` colors only when stdout is a terminal. |
| `--limit-scale <FACTOR>` | Scale comparison budgets for large documents. |
| `--extraction-cache-dir <DIR>` | Reuse cached glyph extraction results. |
| `--old-password-file`, `--new-password-file` | Read a side's password from a file. |
| `--old-font-identity`, `--new-font-identity` | Assert an external font identity as `BASE_FONT=IDENTITY`. |
| `-s, --strict` | Accepted for compatibility; incomplete comparisons always exit `3`. |

All file outputs (`-o`, `-j`, `--trace-json`, `--review`, `--agent-review`)
refuse to replace an existing path. Reports are published atomically, which
requires a filesystem that supports same-directory hard links; on other
filesystems the command exits `2` without publishing.

## Evidence channels

A PDF can differ in more than its text. `pdfdelta` compares each selected
channel through one shared evidence pipeline, and a comparison is complete only
when every selected channel was fully examined.

| Channel | Compares |
| --- | --- |
| `text` | Native glyph text, reconstructed into lines, blocks, tables, and tagged structure. |
| `visual` | Embedded raster images by decoded pixel hash; page rasters are retained for review. |
| `forms` | Saved AcroForm field values and widget appearance crops, by field name. |
| `relations` | Typed relationships such as table membership, labels, captions, and references. |
| `presentation` | Presentation context; retained as supporting evidence. |

Channel selection applies before candidate search, so a forms-only comparison
does not spend correspondence budgets on unrelated body text.

```bash
pdfdelta old.pdf new.pdf --channels text          # text only
pdfdelta old.pdf new.pdf --channels text,visual   # text and image changes
pdfdelta old.pdf new.pdf --channels visual        # image changes only
```

A channel that is selected but cannot be fully examined (for example, text drawn
inside an image, which is not recognized) keeps the comparison incomplete rather
than reporting "no change". See [`how-it-works.md`](how-it-works.md) for what
each channel establishes.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Complete comparison, no established content changes. |
| `1` | Complete comparison, established content changes found. |
| `2` | Execution or report-writing error (I/O, malformed input, resource limit). |
| `3` | Incomplete comparison, even if some changes were independently established. |

In the channel pipeline, only established (typed) changes select exit `1`.
Inferred changes, which depend on an interpretation such as a reconstructed
table or a similarity-based correspondence, appear in the report but leave a
complete comparison at exit `0`. Check the report or JSON when inferred changes
matter to you.

In CI, `pdfdelta -q old.pdf new.pdf` gives an exit-status-only check.

## Output

### Human-readable report

The default channel report lists typed and inferred changes, per-channel
coverage (discovered versus compared source references on each side), and every
unresolved region with its reason. Long values and entry counts are bounded with
explicit truncation markers.

With `--native-text-only`, the report is a contextual unified diff:

```text
--- old.pdf
+++ new.pdf

@@ page 1 · old block 2 -> new block 2 · confidence: medium @@
- Release 10 remains available during the t ...
+ Release 20 remains available during the t ...
```

It starts with a one-line summary, uses `@@ page N … @@` hunks with one-based
page numbers and bounded context, groups nearby exact changes for display, and
marks tentative candidates as `TENTATIVE` hunks.

PDF-derived text is untrusted: control characters and Unicode bidi controls are
escaped to their literal `\u{...}` form so a hostile glyph mapping cannot drive
the terminal or reorder the displayed diff. Color only supplements the `-`/`+`
markers.

### JSON report

`-j PATH` writes a typed JSON report that is independent of presentation
options. The default channel pipeline writes report version 2; the
`--native-text-only` pipeline writes version 11.

Reports separate:

- established `changes`, each with one or more provenance-preserving
  `occurrences` (glyph geometry plus content-stream object and operator);
- tentative `change_candidates`, which never increase coverage;
- `proven_changed_regions` that are known to differ but cannot be localized;
- unresolved source ranges with their reasons;
- per-channel coverage and inventory completeness on both sides;
- an assessment with source ownership, relation dependencies, assumptions,
  uncertainty reasons, search completeness, and work consumed per stage.

`detected`, `indeterminate`, and `no_content_change` describe what is known
about content differences, independently of extraction and comparison
completeness. Neither a complete canonical comparison nor a formatting
annotation establishes visual equivalence.

The native-text report also exposes non-owning `assessment.review_units`. Each
names an established comparison relation and the `literal_minimal` alignment
policy, with bounds on changed source tokens and spans that are changed in every
optimal alignment. A positive lower bound establishes that content differs even
when no change event can be localized; missing bounds mean the query did not
complete, never zero changes.

The JSON `image_diff` object holds old/new image inventories, object references
when available, page placements, pixel hashes, unchanged counts, change indices,
and unresolved indices. Its `comparison.complete` describes only the image
comparison, not full visual or text coverage.

### Diagnostic trace

`--trace-json PATH` writes a separate trace (trace schema version 26) without
changing the report. It records input reading, parsing, glyph extraction, layout
reconstruction, normalization, alignment, exact diff, and report phases with
bounded metrics; typed resource-limit errors; and phases skipped after an
earlier stop. Each phase carries a wall-clock `duration_us`, which is
nondeterministic and must be excluded from golden-file comparisons.

## Review bundles

### Static HTML review

```bash
pdfdelta old.pdf new.pdf --channels text --review review-output
```

Open `review-output/index.html` locally; no script or network service is
required. The bundle contains byte-exact source PDFs, lossless page previews,
the complete comparison JSON, and source IDs with raw glyph evidence, geometry,
provenance, graph views, rendering profiles, and warnings. Every reported change
links to its JSON location and its source pages and regions.

Excerpts distinguish strict changes (A), non-owning corresponding-range content
changes (B), and inferred comparisons (C) with labels and border/underline
styles. These categories do not combine into a strict recall figure.

The destination must not exist. Output is bounded to 512 MiB, 1,024 page
previews, and 20 million source-location work units; a failure may leave an
incomplete directory without `index.html`. Review output never changes the
comparison result or exit code. It is unavailable with `--native-text-only`.

### Agent review bundle

```bash
pdfdelta old.pdf new.pdf --channels text --agent-review review-run
pdfdelta review list review-run --max-output-bytes 8192
pdfdelta review show review-run --case R17 --detail text --max-output-bytes 16384
pdfdelta review render review-run --case R17 --output r17-images
pdfdelta review import review-run --decisions answers.json --output assessed.json
```

An agent review bundle turns each unresolved decision into a case packet that a
calling agent can read back in bounded pieces. Every response is complete JSON
inside the requested byte budget; records that do not fit are reached through a
cursor. `review import` validates external answers and stores them separately
from the engine's result. Exporting a bundle changes no comparison field,
coverage count, or exit status.

`--agent-review` works with the default channels and with `--native-text-only`,
but not together with `--review`. A path whose first component is literally
`review` must be written as `./review`. The full contract is in
[`agent-review.md`](agent-review.md).

## Resource limits and caching

Every PDF is treated as untrusted input, and parsing, extraction, rendering, and
comparison run under explicit budgets. When a comparison budget is exhausted,
the affected region is reported as unresolved or the command exits with an
error; it is never treated as unchanged.

`--limit-scale FACTOR` raises the comparison budgets for large documents:
n-gram token elements, alignment candidate visits and DP cells, diff tokens,
shared assessment work, and assessment output ranges. The diff edit-distance
budget grows up to the Myers implementation's 64 MiB trace-allocation cap. The
factor must be finite and at least `1`. Parser and extraction limits are not
affected.

```bash
pdfdelta old.pdf new.pdf --limit-scale 16
```

`--extraction-cache-dir DIR` reuses glyph extraction results keyed by file
contents and every extraction input (limits, password, and asserted font
identities). A missing, corrupt, oversized, or outdated entry falls back to a
fresh extraction, so results are identical with or without the cache. Entries
are not authenticated: **the cache directory is a trust boundary** and must not
be writable by untrusted users.

```bash
pdfdelta old.pdf new.pdf --extraction-cache-dir ~/.cache/pdfdelta
```

## Encrypted PDFs and font identities

PDFs with an empty user password are decrypted automatically. For other
passwords, supply a file per side; passwords are never accepted as argument
values and never appear in reports or traces.

```bash
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
```

Glyphs without a Unicode mapping are never treated as empty text. They remain
comparable when a stable font identity is available: an embedded font, a
canonical Standard 14 font, or an identity you assert for a non-embedded CID
font. Assertions are trust statements, not font discovery.

```bash
pdfdelta old.pdf new.pdf \
  --old-font-identity TraditionalArabic=windows-v1 \
  --new-font-identity TraditionalArabic=windows-v1
```

## Inspecting a single PDF

```bash
pdfdelta inspect document.pdf                 # backend, PDF version, page count
pdfdelta inspect document.pdf --objects       # indirect objects and structure
pdfdelta inspect document.pdf --glyphs        # extracted glyph evidence
pdfdelta inspect document.pdf --svg overlay.svg
```

`--svg PATH` writes a whole-document glyph overlay to a new file and refuses to
replace an existing file or the inspected PDF. Inspection output escapes control
and bidi characters that originate from PDF names, strings, glyph text, or
parser issues. `inspect` also accepts `--password-file` and `--font-identity`.

## Shell completions

```bash
pdfdelta completions bash > ~/.local/share/bash-completion/completions/pdfdelta
pdfdelta completions zsh  > "${fpath[1]}/_pdfdelta"
pdfdelta completions fish > ~/.config/fish/completions/pdfdelta.fish
```

`powershell` and `elvish` are also supported.
