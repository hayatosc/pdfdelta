# Command-line reference

This page documents the `pdfdelta` command in detail. Run `pdfdelta --help` or
`pdfdelta <command> --help` for the authoritative option list of your build.

- [Comparing two PDFs](#comparing-two-pdfs)
- [Flow diff](#flow-diff)
- [Engine compatibility](#engine-compatibility)
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
| `--engine <ENGINE>` | Text-diff engine for the main command. `flow` is the only engine and is the default; it is accepted for compatibility and is a no-op. `proof` was removed and exits `2` with a message naming the flow replacements. |
| `--native-text-only` | Use the native-glyph text pipeline with compact JSON by default; `--full-json` selects the flow full report. Cannot be combined with `--channels` or `--review`. |
| `-o, --output <PATH>` | Write the human-readable report to a new file instead of stdout. |
| `-j, --json <PATH>` | Write the JSON report to a new file; a `.gz` suffix produces gzip. |
| `--trace-json <PATH>` | Write a phase-by-phase diagnostic flow trace. |
| `--review <DIR>` | Write a static HTML review bundle from flow evidence. |
| `--agent-review <DIR>` | Write an agent review bundle from flow evidence (see [`agent-review.md`](agent-review.md)). |
| `--agent-case-budget <CASES>` | Maximum cases an agent review export may plan: 1–131,072. Default `4096`. |
| `-q, --quiet` | Suppress the human-readable report on stdout. |
| `--color <auto\|always\|never>` | Colorize the report. `auto` colors only when stdout is a terminal. |
| `--limit-scale <FACTOR>` | Scale flow extraction count limits, the footer-matching work limit, and render/image worker CPU and wall budgets. |
| `--extraction-cache-dir <DIR>` | Reuse cached glyph extraction results. |
| `--old-password-file`, `--new-password-file` | Read a side's password from a file. |
| `--old-font-identity`, `--new-font-identity` | Assert an external font identity as `BASE_FONT=IDENTITY`. |
| `-s, --strict` | Accepted for compatibility; it does not change the comparison. |

`--compact-json` is an explicit alias for the default compact flow native
report. `--full-json` selects the flow full report with per-token source
provenance. Both require `--native-text-only` and `--json`.

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
script of `replace`, `insert`, `delete`, `move`, `reading-order`, and
`layout-only` changes. Use
it when reviewing reflowed prose. The main command uses the same engine for its
text channel; the standalone command exposes the token-level report directly. The global `--color` option styles this report with the same
conventions as the main flow text output.

| Option | Description |
| --- | --- |
| `--json <PATH>` | Write the `pdfdelta-flow/1` JSON report; a `.gz` suffix produces gzip. |
| `--tokens` | Also write token text to JSON Lines sidecars (`PATH.old-tokens.jsonl[.gz]`, `PATH.new-tokens.jsonl[.gz]`). |
| `--geometry` | Include per-token page and bounding box in the JSON report. |
| `--limit-scale <FACTOR>` | Scale extraction count limits, the footer-matching work limit, and image worker CPU/wall budgets by a factor of at least 1. Parser limits and layout thresholds are unchanged. |

Text output opens with a `flow summary` block: old and new token totals, equal
tokens, per-kind (`delete`, `insert`, `replace`, `move`, `reading-order`,
`layout-only`) counts, the number of content changes, moved changes, detected
changes (content plus moves), formatting-only changes on matched tokens,
extraction
status, glyph conservation, and furniture masked-line counts. When extraction or glyph conservation is
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
are counted but not listed. Same-row reading-order transpositions are listed
under a separate `reading-order changes (text unchanged):` section with their
pages and context, and never appear in the numbered content changes. Furniture
(running headers, footers, and page
numbers) is removed from the content stream and diffed by digit-masked text;
added or removed masked lines are listed under a separate `furniture:` section.
The confidence, ambiguity, and `formatting_only` fields are additive within
`schema: pdfdelta-flow/1`; existing consumers can ignore them. The standalone
`flow` JSON keeps `reason` as the first reason and adds the `reasons` list, so
the schema string remains compatible.

`--json` contains the same summary, per-change token ranges and pages,
`touches_incomplete_page`, `touches_order_uncertain_page`,
`touches_glyph_name_identity_page`, each change's `confidence`
(`high`, `medium`, or `low`) and `ambiguity` reasons (`coarse_span`,
`repeated_run`, `order_uncertain`, `short_move`, `numeric_only`,
`numeric_table`), a `stats` object whose `reading_order` entry counts
reading-order changes, a `formatting_only` array of matched token runs whose
formatting differs (`reasons` lists `normalization`, `block_structure`,
`font_size`, `position`, `line_break`, and/or `page_break`; `reason` keeps the
first reason for older readers, and `old_size`/`new_size` are present only when
`font_size` is included), and per-side `incomplete_pages`,
`incomplete_document_kinds`, `image_identity_pages`,
`glyph_name_identity_pages`, a per-page `order` array with `agreement`, the
layout, stream, and baseline scores, `order_coherence`, `order_uncertain`, and
per-band `bands` (an empty array unless a page was split into stacked regions;
`bottom` and `top` are `null` at the unbounded outer edge of the page),
furniture, and per-page conservation. Formatting entries are split at content
changes and old-side blocks, then adjacent entries with the same reasons,
sizes, and pages are coalesced. A one-token match inside rewritten text is a
point, not a formatting span, and is dropped after coalescing. Font size is
reported once per distinct changed size pair over the whole matched run.
Position requires the tested segment's internal line and page break offsets to
agree; a segment that moved to a different page with an unchanged internal
layout is still position evidence. A break difference in one segment is
reported as `line_break` or `page_break` for that segment and no longer
suppresses position on another segment of the same matched run.

An unmapped glyph has no Unicode text, so `flow` compares a synthetic token
instead. Its text is `\uFFFC<kind>:<identity-hash>:<glyph-id>`, where `kind` is
`name` (subset-stripped `BaseFont` plus encoding glyph name), `outline`
(CID-keyed CFF `CharStrings` entry), or `program` (embedded program hash plus
glyph id). Equal names in the same family and equal outlines compare equal
across subsets; the token stays uncertain and is never treated as Unicode text.
In the native full report, each `token_sources` entry for an unmapped glyph
carries the same evidence under `unmapped.identity`, and
`glyph_name_identity_pages` lists pages whose placeholders use name identity.

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

Flow uses these exit codes. Formatting-only changes (font-size changes on
matched tokens) are reported but never select exit `1`. The confidence and
ambiguity fields are descriptive and do not change the exit code.

| Code | Meaning |
| --- | --- |
| `0` | No detected difference. `Layout-only` changes do not count; there are no `Delete`, `Insert`, `Replace`, or `Move` changes. |
| `1` | Detected difference found: a `Delete`, `Insert`, `Replace`, or `Move`. Moves count even when the moved run has one token. |
| `2` | Execution or report-writing error. |
| `3` | Extraction or glyph conservation is incomplete on either side. |

Limits: the script is token-level, so words are separated at spaces and
punctuation, and one CJK scalar is one token. Extraction budgets default to
the shared parser limits; `--limit-scale` raises the count limits and worker
CPU/wall budgets for a document that legitimately needs more (the 3,100-page PostgreSQL manual needs
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
glyphs keep a stable placeholder instead of readable text. The standalone
command does not produce the multi-channel report; use the main command's
channel coverage for a complete evidence claim.

```bash
pdfdelta flow old.pdf new.pdf
pdfdelta flow old.pdf new.pdf --json flow.json.gz --tokens
pdfdelta flow old.pdf new.pdf --geometry --json flow.json
pdfdelta flow old.pdf new.pdf --limit-scale 2 --json flow.json.gz
```

## Engine compatibility

There is one text engine: the token-stream flow engine. It supplies the text
channel; visual, forms, relations, and presentation keep their existing
implementations. `--engine flow` is accepted for compatibility and is a no-op,
because flow is already the default.

`--engine proof` was removed. The option is still parsed only so the process
can reject it with exit `2` and a stable message naming the flow replacements
(`--trace-json`, `--compact-json`, `--full-json`, `--review`, `--agent-review`,
and the standalone `pdfdelta flow` report). No output is written.

Reports carry `"engine": "flow"` (inside the native compact envelope and in
the multi-channel report) whenever flow runs, including a run whose selected
channels exclude text. Review bundles written by older builds remain readable
with `pdfdelta review list/show/render/import`.

## Flow text engine

`pdfdelta OLD.pdf NEW.pdf` keeps the multi-channel report and supplies its
**text channel** from the flow engine. Non-text channels keep their current
implementations. `--native-text-only` keeps the compact native schema-13
envelope and writes the flow text result there instead.

| flow change | existing report field |
| --- | --- |
| `Delete` / `Insert` / `Replace` | `changes[]` with kind `deletion`, `insertion`, or `replacement`, using flow token ranges, texts, and 1-based pages |
| `Move` | `changes[]` with kind `move` |
| `ReadingOrder` | `reading_order_changes[]` with kind `reading-order`; never a content change; `flow_summary.reading_order_changes` counts it |
| `LayoutOnly` | not a change; `flow_summary.layout_only_changes` counts it |
| `formatting_only` | `formatting_only_changes[]` with a `reasons` list (`normalization`, `block_structure`, `font_size`, `position`, `line_break`, `page_break`) and exact sizes when `font_size` is present |
| `confidence` / `ambiguity` | additive fields on each change |

Flow text completeness is both sides extraction-complete, glyph conservation
complete, and both non-text paint inventories equal. Multi-channel exit codes
are selected per channel: a channel that detected a typed difference with its
own evidence intact selects `1`, even when another channel is broken or was not
examined. Only broken evidence on the detecting channel, or a cross-channel
reason that cannot be attributed, blocks that detection. `channel_verdicts`
lists `detected`, `broken`, and `unexamined` for each selected channel (plus
`cross_channel` when such a reason exists).

Because flow owns the text channel, the shared correspondence search does not
compute a second text comparison. Text views remain available as relationship
correspondence context, but their local comparisons cannot decide
exit `1`; only a selected channel can. Components that mix text with non-text
sources keep every obligation. The text entry in `coverage` reports flow's
extracted token counts as both discovered and compared; its `complete` flag and
inventory flags still follow flow extraction completeness.
`flow_text.summary.unexamined_paint_issues` counts paint-gate issues
separately; `unresolved_extraction_issues` excludes them. `flow_text.issues`
carries each retained text extraction issue with its side, kind, page,
unexamined flag, and message. `flow_text.order` carries the per-page
reading-order evidence for both sides, including pages whose layout and paint
orders disagreed.

`flow_text.reading_order_changes` carries same-row reading-order evidence with
the same span shape as `flow_text.changes`; it is excluded from
`content_changes`. The flow report contains only what flow computed. It has no top-level
`assessment`, `change_candidates`, `proven_changed_regions`,
`unresolved_regions`, `not_produced_by_engine`, or alignment-coverage
positions; each change carries its own `confidence` and `ambiguity` reasons.
Absence of a position means the engine does not compute it, never that a search
completed and found nothing. `flow_owned_text` records shared-evidence text
components and local comparisons that flow ownership replaced, with an
`unchecked_obligations` count per skipped reason; it is absent when the shared
search recorded no such position. `comparison.flow_owned_text` is the same
record inside the shared comparison. Absence is not a completed text-scope
search. Stored form-field text still produces `text_mask` for the changed local
value pair.

`--review` and `--agent-review` project flow's own token evidence.
`--trace-json`, `--compact-json`, and `--full-json` are flow capabilities.
`-o`, `-j` (including `.gz`), `-q`, `--color`, `-s`, `--limit-scale`, password
files, font identities, `--extraction-cache-dir`, `--channels`, and
`--native-text-only` are supported. `--color always` styles the report with
its ANSI conventions, `--color never` disables them, and the default `auto`
styles only a terminal.

The flow text channel builds a non-text paint inventory from the same
extraction evidence as the other channels. Each painted path, image, inline
image, and shading gets a digest of its kind, exact geometry or image-data
identity, and page-space placement. The page number is excluded so an
identical page shifted within the document compares equal; the page crop and
rotation are included. Artifact-marked paint is included exactly like ordinary
paint. If the two multisets differ, or any paint identity is unavailable, the
text channel is incomplete with `non-text paint differs; text drawn as paths or
images was not examined`, lists the affected pages (bounded), and exits `3`.
The detected text changes are still reported.

This rule is deliberately conservative: a changed table rule, a moved box, or
a new figure also makes the text channel incomplete, because the engine
cannot establish that the changed paint is not outlined text. The M4b
measurement shows that this affects almost every real-world revision; no
narrower variant tested in M4b recovered additional pairs. See
[`limitations.md`](limitations.md).

## Evidence channels

A PDF can differ in more than its text. `pdfdelta` compares each selected
channel through one shared evidence pipeline, and a comparison is complete only
when every selected channel was fully examined.

| Channel | Compares |
| --- | --- |
| `text` | Native glyph text. The flow engine compares one token stream built from the reconstructed layout. |
| `visual` | Embedded raster images by decoded pixel hash; page rasters are retained for review. |
| `forms` | Saved AcroForm field values and widget appearance crops, by field name. |
| `relations` | Typed relationships such as table membership, labels, captions, and references. |
| `presentation` | Presentation context; retained as supporting evidence. |

Channel selection applies before correspondence search, so a forms-only
comparison does not spend correspondence budgets on unrelated body text.

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
| `0` | No detected difference, and every selected channel was examined (no incompleteness reason). |
| `1` | At least one selected channel detected a typed difference with its own evidence not broken. The comparison may still be incomplete because another channel was broken or not examined; `comparison_complete` may be `false`, and `exit_basis` is `detected_with_unexamined_evidence` in that case. |
| `2` | Execution or report-writing error (I/O, malformed input, resource limit). |
| `3` | No channel with an intact detection, and at least one broken or unexamined reason. `exit_basis` is `broken_evidence` when any broken reason exists (including a cross-channel reason), otherwise `unexamined_evidence`. |

The engine classifies reasons as **unexamined** (non-text paint, visual renderer
coverage/annotation completeness, relation evidence that was not established,
image-only pages whose text cannot be examined) or **broken** (everything
else). Broken evidence blocks exit `1` for the channel it belongs to: text
extraction issues and glyph conservation block the text channel, forms reasons
belong to forms, relation graph and work-budget failures to relations, and a
visual backend failure to visual. Structure-ID key presence belongs to
relations/structure; field-name key presence to forms. A key-presence
obligation records why a key claim could not be established (a missing
inventory, unnamed or duplicate members, a retained rename alternative, or an
unavailable acquisition path), so it is unexamined evidence on its channel.
An exhausted raw key search is cross-channel broken. An exhausted scoped key
search, or a scoped work-budget obligation, is broken on every channel that
owns a claim in the search (forms for field names, relations/structure for
structure IDs); other scoped obligations are unexamined on their claim's
domain. A reason whose sources
span channels, and any unknown reason, is `cross_channel` and blocks every
channel. `comparison_complete` stays `false` whenever any reason exists,
and `difference_status` is `detected` when a channel reports a typed difference,
even when the comparison is incomplete. Check `exit_basis` and
`channel_verdicts` in the flow JSON; the text report prints one explicit line
when a detected difference coexists with incomplete evidence, naming the
detecting channel when it is not text.

Exit `0` requires every selected channel to be complete. With the default
channels that is not reachable yet for a document with pages: rendered page
rasters are retained for review but not compared, so each rendered page leaves
the visual inventory incomplete, and native relation endpoints and semantic
relationships are not established, so the relations channel stays unexamined.
An identical or purely reflowed document therefore exits `3` with
`exit_basis: "unexamined_evidence"`, not `0`. Select the channels whose
verdict you need: `--channels text` and `--native-text-only` exit `0` when the
text evidence is complete and unchanged, and `--channels text,forms` adds the
saved-field verdict without the visual and relation gaps.

In the channel pipeline, only established (typed) changes select exit `1`.
Inferred changes, which depend on an interpretation such as a reconstructed
table or a similarity-based correspondence, appear in the report but leave a
complete comparison at exit `0`. Check the report or JSON when inferred changes
matter to you.

In CI, `pdfdelta -q old.pdf new.pdf` gives an exit-status-only check.

## Output

### Human-readable report

The default channel report lists typed and inferred change counts, per-channel
coverage (discovered versus compared source references on each side), and every
unresolved region with its reason. Flow text changes are listed in
`flow_text.changes`, in the HTML review, and in the `--native-text-only` text
report. Long values and entry counts are bounded with
explicit truncation markers.

With `--native-text-only`, the report is the flow summary followed by one
numbered entry per detected change:

```text
text engine: flow (1 detected changes, 0 formatting-only)
flow summary
  old: 32 tokens
  new: 32 tokens
  ...

1. replace old 1 -> new 1 [confidence: medium; ambiguity: numeric_only]
   old: schedule details for the upcoming cycle . Release [10] remains available during the transition period . All
   new: schedule details for the upcoming cycle . Release [20] remains available during the transition period . All
```

The summary reports token counts per change kind, extraction status, reading
order, glyph conservation, and furniture. Each entry names the change kind, the
one-based old/new pages and token ranges it touches, its confidence and
ambiguity reasons, and shows the changed tokens in brackets between a few
unchanged context tokens. Layout-only changes are counted but not listed;
formatting-only entries are summarized by count.

PDF-derived text is untrusted: control characters and Unicode bidi controls are
escaped to their literal `\u{...}` form so a hostile glyph mapping cannot drive
the terminal or reorder the displayed diff. Color only supplements the `-`/`+`
markers.

### JSON report

`-j PATH` writes a typed JSON report that is independent of presentation
options. The default channel pipeline writes report version 3; the
`--native-text-only` flow pipeline defaults to the compact flow envelope
described below. Add `--full-json` for the flow full report with per-token
source provenance.

The multi-channel report separates:

- established typed `changes` and inferred changes for the non-text channels;
- per-channel coverage and inventory completeness on both sides;
- `flow_text.changes` and `flow_text.formatting_only_changes` for the text
  channel, with token ranges, pages, confidence, and ambiguity reasons;
- unresolved source ranges with their reasons;
- `flow_owned_text`, recording the shared-evidence text positions flow
  ownership replaced (absent when none were recorded).

`detected`, `indeterminate`, and `no_content_change` describe what is known
about content differences, independently of extraction and comparison
completeness. Neither a complete token comparison nor a formatting annotation
establishes visual equivalence.

The native flow report has no `assessment`, `change_candidates`,
`proven_changed_regions`, `unresolved_regions`, or `not_produced_by_engine`
positions. Each change carries `confidence` and `ambiguity` directly. Its
summary has `old_tokens` and `new_tokens`; `extraction.old_complete` and
`new_complete` state whether each side was fully extracted. Absence is not an
empty search.

The JSON `image_diff` object holds old/new image inventories, object references
when available, page placements, pixel hashes, unchanged counts, change indices,
and unresolved indices. Its `comparison.complete` describes only the image
comparison, not full visual or text coverage.

### Compact native JSON

For benchmark captures and diagnostic review, emit compact JSON directly:

```bash
pdfdelta old.pdf new.pdf --native-text-only -q -j report.json.gz
```

Flow native JSON output defaults to compact. `--compact-json` is an explicit
alias for that default and produces byte-identical output. Use `--full-json` to
add per-token source provenance (the flow full report). Both flags require
`--native-text-only` and `--json` and cannot be combined. The default writes a
`pdfdelta-native-compact` envelope with `artifact_version: 3`,
`source_schema_version: 13`, and the native semantic report under `report`.
Comparison, exit status, and full text are unchanged.

The native `reading_order_changes[]` array carries one entry per same-row
transposition with the same `old_span`/`new_span` shape as a content change.
It is never added to `summary.content_changes`, and `flow_summary` also carries
`reading_order_changes`, `reading_order_old_tokens`, and
`reading_order_new_tokens`.

In a flow full report, each token's `sources` array carries its glyphs; a
compact report does not carry per-glyph sources and must be regenerated with
`--full-json` when individual geometry or operator provenance is needed. Retain
input hashes, the binary identity, arguments, and run metrics alongside a
capture for replay. `benchmark/realworld/compact_native.py` also reads archived
version-2/schema-11 flow artifacts and the historical version-1 proof-era
`source_summary` layout; the current binary writes only version 3.

### Flow full report

`--native-text-only --full-json -j report.json.gz` writes the
`pdfdelta-native-full` envelope (`artifact_version: 2`, source schema 13). The
`report` object is the compact flow report plus:

- `token_sources.old` / `.new`: one entry per token, aligned with the compact
  report's token ranges, containing `token`, `text`, `page`, and a
  `sources` list. A source glyph records `glyph_id`, 1-based page, decoded or
  unmapped text, raw code hex, bbox, baseline, direction, font id/size, render
  order, render mode, crop and path-clip status, content-stream object, and
  operator index.
- `glyph_accounting.old` / `.new`: `extracted`, `visible`,
  `token_referenced`, and `excluded` entries with an explicit reason
  (`furniture`, `whitespace`, `invisible`, or `shared_source`), plus
  `excluded_counts`. A visible glyph is referenced by exactly one token or
  listed as an exclusion; a glyph shared by several normalization tokens is
  listed as `shared_source` and remains visible in each token's source list.

Token sources and accounting are streamed through the serializer; the report
is never built as one JSON value. When the side was built in a child process,
per-glyph records are also spilled to a temporary side cache and copied back in
token order, so the reporting parent does not retain an extracted document.
On `M/gcc-13-3-to-13-4` (`--limit-scale 2`) the peak resident set was 2.18 GB,
compared with 2.06 GB for the compact text-only run and 5.14 GB for the former
in-memory full report. Full reports are still much larger than compact reports:
the largest ten H1 pairs produced 609.7 MB of gzip in total. The temporary side
cache is about the size of the `token_sources` records and is removed when the
comparison ends. Capture with a stated output budget and delete captures when
they are no longer needed. `--full-json` requires `--native-text-only` and
`--json`.

### Diagnostic trace

`--trace-json PATH` writes a separate trace without changing the report or exit
status. It is `trace_schema: pdfdelta-flow-trace/1`.

The trace records input reading, parse, glyph extraction,
tokenization/normalization, furniture/order handling, flow diff,
assessment/formatting, channel comparison, and report phases. A text-only
native run records `channel_comparison` as `skipped` with reason `text_only`;
a multi-channel run records it completed. Each phase carries bounded counts and
a wall-clock `duration_us`. A multi-channel run also records
`render_documents`: one entry per side with the page count, rendered and failed
page counts, cumulative child CPU, and the measured child CPU of each rendered
page in microseconds. `image_worker_calls` records one entry per successful
image worker call with its side, kind (`document`, `page`, or `encoded-page`),
call count, and child CPU. The top-level `resource_limits` object records the
scaled render page/document CPU and wall budgets and image CPU/wall budgets.
These CPU values are measurements, not budgets; a failed render attempt is
charged the full page budget in the cumulative value and is not added to the
per-page list. A failed or incomplete phase carries an `error`
whose `kind` is taken from the explicit class of the error: `backend`,
`unsupported`, `unresolved`, or `limit_exceeded`. This applies to both the flow
text phases and the shared-evidence `channel_comparison` phase. A limit error
names its `resource` and `limit` (for example, `PDF input bytes`, `password
file bytes`, or `static review page previews`); the message is contextual text
and never decides the class. `duration_us`, `render_documents`, and
`image_worker_calls` are
nondeterministic and must be excluded from golden-file comparisons; the trace
names all three in `nondeterministic_fields`. Missing phases after a stop are marked
`skipped` with a reason.

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

The source index is published as the lossless `sources.json.gz` because the
comparison's evidence index is large. Manifest version 3 writes a gzip member
inside the artifact's gzip member, matching the version-2 size behavior. Older
bundles are read by `review::read_sources`, which accepts the single-stream
version-3 shape, the version-2 double-gzip shape, and the oldest uncompressed
`sources.json`. The HTML review adds a flow
text section. Each
non-layout flow change links to `/flow_text/changes/{index}`, each
formatting-only entry to `/flow_text/formatting_only_changes/{index}`, each
per-channel verdict to `/channel_verdicts/{index}`, and each retained text
extraction issue or uncertain reading order to `/flow_text/issues/{index}` or
`/flow_text/order/{side}/{index}`. Flow token ranges are shown with their source
pages and union token geometry. Per-glyph provenance is not copied into the
HTML bundle; use `--agent-review` (which retains token sources) or
`--full-json` when exact glyph identifiers are needed.

Excerpts distinguish strict changes (A), non-owning corresponding-range content
changes (B), and inferred comparisons (C) with labels and border/underline
styles. A is a delete, insert, or high-confidence replace; B is a replace whose
span is coarse, so its interior edit is not localized; C is a move or a change
carrying an ambiguity signal. Formatting-only entries get their own F label.
Flow has no separate unresolved-comparison category; broken or unexamined text
evidence is listed with its own reason record and channel verdict instead. The
categories never combine into a strict recall figure.

The destination must not exist. Output is bounded to 512 MiB, 1,024 page
previews, and 20 million source-location work units; a failure may leave an
incomplete directory without `index.html`. Review output never changes the
comparison result or exit code. It remains unavailable with
`--native-text-only`, because the HTML bundle publishes page previews and the
shared evidence store, which the native flow adapter does not retain; use
`--agent-review` for a native flow bundle.

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
but not together with `--review`. It publishes flow evidence; existing proof
bundles remain queryable. A path whose first component is literally `review`
must be written as `./review`. The full contract is in
[`agent-review.md`](agent-review.md).

The export plans at most 4,096 cases by default. The limit is a total cap:
each case adds a packet and manifest entry, and each published bundle is capped
at 512 MiB. `--agent-case-budget <CASES>` raises it explicitly (1–131,072); a
plan that reaches either bound records a bundle-level `cases_not_exported`
omission with the number of withheld cases and exits incomplete. Cases are
ordered by importance: flow content and formatting changes first, then
acquisition evidence with broken evidence before unexamined evidence and
page-local gaps before document-wide ones.

A plan larger than one 512 MiB bundle is written as numbered part bundles under
the destination (`part-0001`, `part-0002`, ...). Each part is a complete bundle;
the root `manifest.json` is a `pdfdelta-agent-review-set` version 1 manifest
listing every part, its case count and bytes, and the global `export_complete`
flag. `review list`, `show`, `render`, and `import` accept either a single
bundle or a part set. A case that exceeds the per-case source or member cap is
split into consecutive sub-cases linked by `subcase.parent`, `subcase.index`,
and `subcase.count` (`agent-review-split/v1`); the first part carries the quoted
text and flow entries, later parts carry the remaining source evidence.
Sub-cases count toward the budget and `export_complete`.

On `P/nist-controls-53-r4-to-r5`, the current in-memory planner exports 4,096
cases as two parts (about 0.6 GB) and withholds about 85,600 sub-cases. A
budget large enough for the full plan reaches the 6 GB process budget before
writing; complete streaming export is not implemented yet.

## Resource limits and caching

Every PDF is treated as untrusted input, and parsing, extraction, rendering, and
comparison run under explicit budgets. When a comparison budget is exhausted,
the affected region is reported as unresolved or the command exits with an
error; it is never treated as unchanged.

`--limit-scale FACTOR` raises count and CPU budgets for large documents: the
flow text channel's extraction budgets, the bounded footer-matching work limit,
channel render page CPU (10 s) and per-document child CPU (120 s) with their
wall safety stops (120 s per page and 600 s per document), and image-decoder
worker CPU (60 s) with its wall safety stop (300 s). The factor must be finite
and at least `1`. Parser limits, layout thresholds, image pixel/occurrence
limits, and retained-raster budgets are unchanged.

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
returns `unavailable`, never an empty envelope. If the entire response does not
fit the output cap, the command exits `2` without emitting partial JSON.

This is a query over caller-supplied tokens and source flags. It does not
extract PDF regions, verify glyph provenance, infer copy identity, establish
frame closure, or resolve correspondence alternatives in an existing report.
Exit `0` means the query was answered, including explicit local refusals; it
does not mean a PDF comparison completed. Library callers can use
`pdfdelta_core::text_envelope::local_text_envelope` for the same conditional
queries over caller-built `LocalTextSide` values; it returns only claims that
remain conditional on the supplied tokens and masks.

## Shell completions

```bash
pdfdelta completions bash > ~/.local/share/bash-completion/completions/pdfdelta
pdfdelta completions zsh  > "${fpath[1]}/_pdfdelta"
pdfdelta completions fish > ~/.config/fish/completions/pdfdelta.fish
```

`powershell` and `elvish` are also supported.
