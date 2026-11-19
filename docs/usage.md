# Command-line reference

This page documents the `pdfdelta` command in detail. Run `pdfdelta --help` or
`pdfdelta <command> --help` for the authoritative option list of your build.

- [Comparing two PDFs](#comparing-two-pdfs)
- [Flow diff](#flow-diff)
- [Evidence channels](#evidence-channels)
- [Exit codes](#exit-codes)
- [Output](#output)
- [Review bundles](#review-bundles)
- [Resource limits and caching](#resource-limits-and-caching)
- [Encrypted PDFs and font identities](#encrypted-pdfs-and-font-identities)
- [Inspecting a single PDF](#inspecting-a-single-pdf)
- [Explaining local text alternatives](#explaining-local-text-alternatives)
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
| `--native-text-only` | Use the native-glyph text pipeline with compact JSON by default; `--full-json` selects the full version 11 report. Cannot be combined with `--channels` or `--review`. |
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

## Flow diff

```text
pdfdelta flow <OLD_PDF> <NEW_PDF> [--json <PATH>] [--tokens] [--geometry] [--limit-scale <FACTOR>]
```

`flow` compares the two documents as one token stream reconstructed from their
layout blocks. It ignores line-wrap and page-break changes, and reports a single
script of `replace`, `insert`, `delete`, `move`, and `layout-only` changes. Use
it when reviewing reflowed prose; the default comparison remains the primary
report with channel coverage and unresolved regions.

| Option | Description |
| --- | --- |
| `--json <PATH>` | Write the `pdfdelta-flow/1` JSON report; a `.gz` suffix produces gzip. |
| `--tokens` | Also write token text to JSON Lines sidecars (`PATH.old-tokens.jsonl[.gz]`, `PATH.new-tokens.jsonl[.gz]`). |
| `--geometry` | Include per-token page and bounding box in the JSON report. |
| `--limit-scale <FACTOR>` | Scale extraction count limits by a factor of at least 1. Parser limits and layout thresholds are unchanged. |

Text output opens with a `flow summary` block: old and new token totals, equal
tokens, per-kind (`delete`, `insert`, `replace`, `move`, `layout-only`) counts,
the number of content changes, extraction status, glyph conservation, and
furniture masked-line counts. When extraction or glyph conservation is
incomplete, `old incomplete pages` / `new incomplete pages` list the affected
pages and issue kinds, or `document-wide` when an issue cannot be tied to one
page. Reading order is reported per side as layout pages, paint-order
(`stream`) pages, baseline-row (`baseline`) pages, and pages where the
hypotheses disagreed (`uncertain`). Pages below the order-coherence threshold
are listed separately. Content
changes are then listed in old-document order. Each entry names the change
kind, the old and new pages it touches (marked `(incomplete pages)` or
`(order-uncertain pages)` when either side is suspect), and shows the changed
tokens in brackets between a few unchanged context tokens; layout-only changes
are counted but not listed. Furniture (running headers, footers, and page
numbers) is removed from the content stream and diffed by digit-masked text;
added or removed masked lines are listed under a separate `furniture:` section.
`--json` contains the same summary, per-change token ranges and pages,
`touches_incomplete_page`, `touches_order_uncertain_page`,
`touches_glyph_name_identity_page`, per-side `incomplete_pages`,
`incomplete_document_kinds`, `image_identity_pages`,
`glyph_name_identity_pages`, a per-page `order` array with `agreement`, the
layout, stream, and baseline scores, `order_coherence`, `order_uncertain`, and
per-band `bands` (an empty array unless a page was split into stacked regions;
`bottom` and `top` are `null` at the unbounded outer edge of the page),
furniture, and per-page conservation.

An unmapped glyph has no Unicode text, so `flow` compares a synthetic token
instead. Its text is `\uFFFC<kind>:<identity-hash>:<glyph-id>`, where `kind` is
`name` (subset-stripped `BaseFont` plus encoding glyph name), `outline`
(CID-keyed CFF `CharStrings` entry), or `program` (embedded program hash plus
glyph id). Equal names in the same family and equal outlines compare equal
across subsets; the token stays uncertain and is never treated as Unicode text.
The native `--native-text-only --json` report's `unmapped_tokens` entries carry
the same evidence as an `identity` field, and `glyph_name_identity_pages` lists
pages whose placeholders use name identity.

Image-only pages first try an encoded identity (identical encoded image
streams, pixel-affecting dictionaries and CTMs), which resolves without
decoding as `⟦image-encoded:<hash>⟧`. If encoded bytes differ, a complete
decoded pixel inventory resolves the page as `⟦image:<hash>⟧`; the report lists
the rule used per page. A page remains incomplete when neither identity is
available within the image limits; an identified one-sided image page is a
deletion or insertion. Unsupported clipping paths
keep overlapping glyphs (with conservative partial-clip status and `uncertain`
tokens) and remain page-scoped extraction gaps; glyphs strictly outside the
path's outward control-point bound are excluded. Straight and curved clip
paths are additionally classified conservatively; curved paths use relative
chord flattening and keep boundary cases uncertain. Glyph names resolve through
the Adobe Glyph List, the `uni`/`u` forms, a bundled Apache-2.0 pdf.js
supplement, and a small project-authored MIT table for unambiguous TeX names;
only names none of these rules resolve stay placeholders. Unmapped glyphs compare by name identity when any encoding
supplies a name (PDF Differences, a cleartext Type1 `/Encoding`, or a Type1C
charset), by CID-keyed CFF outline identity when no name exists, and otherwise
by the program identity. Pages with name-identity placeholders are listed as
`glyph-name identity pages`; glyphs stay unmapped placeholders, and changes
touching them are flagged.

`flow` builds three reading-order hypotheses per page from the same visible
glyphs: reconstructed layout order, content-stream paint order, and
baseline-row order. Baseline-row order clusters glyphs by baseline coordinate
along the text direction's normal using gaps relative to font size, ignores
bounding-box height, orders rows top-to-bottom (right-to-left for vertical
text), and orders glyphs by their baseline advance direction. If layout and
paint order have at least 0.9 order-preserving overlap, layout order is kept.
Otherwise the page is matched to its counterpart pages in the other document by
an order-free, information-weighted bag overlap of page tokens; the best match,
every match within half of its overlap, and the pages adjacent to the best
match provide the evidence. Each candidate is scored by the information-weighted
bigrams and trigrams it shares with those counterparts' three candidates, using
a weight table built from the counterpart tokens alone. A page whose best
overlap is below 30% of the document pair's median per-page best overlap has no
counterpart and is ordered by intrinsic spatial coherence only, which keeps an
inserted or deleted page from borrowing evidence from a structurally similar
neighbour. Layout order is kept only when it has at least as much evidence and
at least as much spatial coherence as paint order; baseline-row order replaces
paint order only when it has more evidence and at least as much coherence.
Otherwise paint order wins; exact ties keep layout, then paint order. Pages
where layout and paint order disagree are marked order-uncertain. The engine
never consults an external reader. Pages with `order_coherence` below
0.5 are listed as low-coherence pages; the threshold is below the 0.9 minimum
observed on the generated and vendored fixtures and above the 0.35 measured on
the scrambled IRS form rows. Generated fixtures stay at or above 0.83.

Flow uses these exit codes:

| Code | Meaning |
| --- | --- |
| `0` | No content changes. `Move` and `layout-only` changes do not count as content changes, but moves are still reported. |
| `1` | Content changes found. |
| `2` | Execution or report-writing error. |
| `3` | Extraction or glyph conservation is incomplete on either side. |

Limits: the script is token-level, so words are separated at spaces and
punctuation, and one CJK scalar is one token. Extraction budgets default to
the shared parser limits; `--limit-scale` raises only the count limits for a
document that legitimately needs more (the 3,100-page PostgreSQL manual needs
scale 2). Above factor 1, each side is extracted and reconstructed in a child
process and its candidate streams are cached in a private per-run directory
(mode `0700`), so the second side's extraction does not overlap the first
side's normalization buffers. The cache is a versioned binary format with
token counts, lengths, and an FNV-1a checksum; the parent validates all of them
before use. The directory is removed on success, on an error return, and when
the child fails. A child that exits non-zero or is killed aborts the command
with exit `2` and never publishes a partial report. On Linux/glibc the `flow` command restarts itself with one malloc
arena so the second side's extraction can reuse the first side's freed
buffers; this keeps a multi-million-glyph comparison inside a 6 GB budget and
is a no-op when `MALLOC_ARENA_MAX` is already set. Text can still be reordered by
reading-order reconstruction; layout issues are not propagated into the script.
Moves require a repeated run that is long enough to identify, and a table of
contents whose page numbers shift can still produce move noise. Once the
`predefined-cid-unicode` feature is enabled, common Adobe predefined CID
collections yield readable Unicode; unknown CIDs stay placeholders. Embedded
Type1 fonts without a PDF encoding use their declared built-in encoding and
Adobe Glyph List names; font-specific names stay placeholders. Unmapped CID
glyphs keep a stable placeholder instead of readable text. The command does not
replace the channel comparison for completeness claims.

```bash
pdfdelta flow old.pdf new.pdf
pdfdelta flow old.pdf new.pdf --json flow.json.gz --tokens
pdfdelta flow old.pdf new.pdf --geometry --json flow.json
pdfdelta flow old.pdf new.pdf --limit-scale 2 --json flow.json.gz
```

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
`--native-text-only` pipeline defaults to the compact version-2 envelope
described below. Add `--full-json` for the full native version-11 report.

Reports separate:

- established `changes`, each with one or more provenance-preserving
  `occurrences` (compact source summaries by default for native reports;
  `--full-json` includes all glyph geometry, content-stream objects, and operators);
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

### Compact native JSON

For benchmark captures and diagnostic review, emit compact JSON directly:

```bash
pdfdelta old.pdf new.pdf --native-text-only -q -j report.json.gz
```

Native JSON output defaults to compact. `--compact-json` remains an explicit
alias for that default. Use `--full-json` to retain all glyph sources in the
original schema-11 report. Text-span `unmapped_tokens` entries include an
`identity` field (`name`, `outline`, or `program`) beside the identity hash and
glyph id. Both flags require `--native-text-only` and `--json`
and cannot be combined. The default writes a
`pdfdelta-native-compact` envelope with `artifact_version: 2` and the native
semantic report under `report`. Comparison, exit status, and full text are
unchanged. Each `sources` array is replaced by a `source_summary` containing
kind counts, pages, first/last locator samples, and a source-array digest.
The writer summarizes one projected source array at a time and writes directly
to the destination (gzip for `.gz`), without producing a full JSON intermediate.

`canonical_array_sha256` and `canonical_array_bytes` describe the compact JSON
serialization of that source array in native schema-11 field order, with no
trailing newline. They differ from version-1 postprocessing hashes, which cover
indented source bytes. A direct artifact has no hypothetical full-report hash.
Retain input hashes, the binary identity, arguments, and run metrics alongside
it for replay. Individual glyph geometry and PDF operator provenance beyond the
samples require rerunning with `--full-json`.

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

## Explaining local text alternatives

`pdfdelta text-envelope INPUT` accepts a JSON request with `pairs`. Each pair
has an `id`, `old`, and `new`; each side is a string or an object containing
`tokens` (serialized comparable tokens) and an optional boolean `source` mask.
Strings retain every Unicode scalar, including trailing whitespace. No
whitespace trimming or normalization is performed.

```bash
pdfdelta text-envelope - <<'JSON'
{"pairs":[{"id":"field14","old":"14 Excess golden parachute payments","new":"14 "}]}
JSON
```

The response is explicitly conditional on the supplied reading order and
correspondence. It returns mandatory source-change masks, minimum/maximum
changed counts, the exact positions whose operation or counterpart varies,
per-position counterpart alternatives, and the complete optimal-alignment DAG.
For this example, the count is 32, 29 old letters must be deleted, and exactly
one of old spaces `[2,9,16,26]` matches new position `2`. The other three spaces
are deleted, without selecting an arbitrary location for the retained space.

`old` and `new` token alternatives are marginals. They cannot be combined
independently: follow a complete DAG path to obtain a jointly valid alignment.
In DAG row `i`, byte offset `k` represents state `(i, new_start+k)`. Outgoing
bits are `1` for equal `(i+1,j+1)`, `2` for delete `(i+1,j)`, and `4` for insert
`(i,j+1)`. Start at `(0,0)` and end at the two input lengths. Zero means no
outgoing edge. The DAG is retained without enumerating its paths.

Default limits are 1,000,000 shared logical work units, 262,144 retained cells
per pair, and 1,048,576 output bytes including the newline. The corresponding
options are `--max-work`, `--max-cells`, and `--max-output-bytes`. Hard ceilings
are 32,000,000 work units, 262,144 cells, and 8,388,608 output bytes. Input is
limited to 1 MiB, 1,024 pairs, and 32,768 tokens per side. A work/cell refusal
returns `unavailable`, never an empty proof. If the entire response does not
fit the output cap, the command exits `2` without emitting partial JSON.

This is a query over caller-supplied tokens and source flags. It does not
extract PDF regions, verify glyph provenance, infer copy identity, establish
frame closure, or resolve candidates in an existing report. Exit `0` means the
query was answered, including explicit local refusals; it does not mean a PDF
comparison completed. Use `local_text_envelope` for comparable-token slices or
`document::explain_local_text_views` for existing `TextView` pairs in library
integrations. The latter retains the conditional interpretation and original
token indices for projection through `view.origins`.

## Shell completions

```bash
pdfdelta completions bash > ~/.local/share/bash-completion/completions/pdfdelta
pdfdelta completions zsh  > "${fpath[1]}/_pdfdelta"
pdfdelta completions fish > ~/.config/fish/completions/pdfdelta.fish
```

`powershell` and `elvish` are also supported.
