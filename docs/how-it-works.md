# How pdfdelta works

This page describes the comparison pipeline, what each evidence channel
establishes today, and where its support ends. For command-line usage see
[`usage.md`](usage.md); for known gaps see [`limitations.md`](limitations.md).

## Pipeline

```text
PDF bytes
  -> PDF parser backend
  -> primitive glyph extraction
  -> layout reconstruction
  -> cross-document alignment
  -> exact diff
  -> changes, confidence, coverage, and unresolved regions
```

The design rules are:

- **Preserve raw evidence.** Glyph text, raw codes, geometry, render order,
  render mode, and object/operator provenance survive every stage.
- **Keep parser types behind a neutral facade.** The PDF library is an adapter
  behind `PdfParser`/`ParsedPdf` and can be replaced.
- **Make layout reconstruction reversible.** Line and block boundaries are
  hypotheses; alignment can recover when they are wrong.
- **Align tolerantly, diff exactly.** Alignment may use similarity and folding,
  but the final diff runs over exact canonical text.
- **Report what is unknown.** Unsupported filters, encryption, unmapped glyphs,
  and uncertain regions are reported as unsupported or unresolved, never as
  empty text.

Results separate *established* differences, *tentative* candidates, and
*unresolved* content. Correspondences that depend on an interpretation (for
example, a table layout inferred from ruling lines) are marked *inferred*.

The implementation is pure Rust and makes no external analysis API calls.

## Common correspondence solver

All channels feed a common solver. Its versioned objective prioritizes
source-backed scoped identities (structure IDs, field names, row/column keys),
then literal content, then inferred supplier scores. Literal fragments cannot
displace a competing keyed item merely by matching more unchanged pieces.

- Source-only mandatory correspondences are fixed before inferred candidates
  are enumerated, so inferred tie-breaks cannot override a source alignment.
- Alternative views of the same evidence (layout blocks, tagged structure,
  table cells) compete for ownership; selected members must admit one shared
  partition, and no evidence is consumed twice.
- The solver descends through matched containers with bounded depth and scope
  counts, and records each child scope's parent correspondence.
- Exact text can match one ordered view to several (1:N or N:1) without
  changing tokens or inventing separators.
- Bounded searches that run out of budget are localized to their dependent
  component; independent correspondences still proceed.

## Text channel

### Extraction

- A backend-neutral PDF parser boundary with a `lopdf` adapter: classic xref
  tables, xref streams, object streams, incremental revisions, inherited page
  resources, password decryption, partial page-tree recovery, and bounded
  stream decoding.
- Bounded content-stream extraction of text and straight-path operators,
  normalized page boxes, CropBox classification, axis-aligned rectangular
  clipping, and isolated Form XObjects.
- Fonts: Type 1 / Type1C / MMType1, TrueType, axis-aligned Type 3, and Type 0
  with Identity-H, bounded Identity-V, and UniJIS-UTF16-H (Adobe-Japan1).
- Text mapping: ToUnicode, Standard / WinAnsi / MacRoman encodings with
  `Differences`, canonical Standard 14 identities, and full-domain identity
  CMaps. Glyphs without a Unicode mapping stay comparable through stable
  embedded-font, Type 3 CharProc, or caller-asserted font identities.
- Existing OCR text layers are retained as glyph evidence. Invisible text
  remains outside visible-content comparison.

### Layout reconstruction

- Glyph-to-line reconstruction with synthetic English spaces and preserved
  arbitrary-angle lines.
- Recursive XY-cut region partitioning with a spatial region graph. Multi-column
  and row-major label/value orders are accepted only when paint order
  independently confirms them; otherwise every line and glyph is preserved and
  the affected window is reported as unresolved reading order.
- Relative line-to-block scoring across page breaks, with running headers and
  footers kept in their own roles.
- A one-sided catalog/form footer provider proposes source-backed terminal-line
  views. Page numbers are metadata, not identity keys.

### Normalization, alignment, and diff

- Reversible raw/canonical normalization with scalar-indexed source maps,
  retained unmapped tokens, and auditable normalization events. Only
  source-backed U+00AD line-end hyphenation is removed.
- Alignment-only compatibility folding and numeric masking that never change
  the exact canonical text.
- Unique exact anchors plus swappable candidate generators (n-gram inverted
  index, MinHash LSH, exhaustive).
- Anchor-interval alignment for 1:1, insert/delete, constrained adjacent 1:2,
  2:1, 1:3, and 3:1 matches, and exact paragraph moves. Ambiguous regions stay
  unresolved.
- Unkeyed paragraphs can propose nonidentical one-to-one correspondences using
  literal trigram similarity; these remain inferred.
- An in-house Myers diff over exact canonical and unmapped tokens, with
  allocation-aware limits.
- A separate bounded review path can suggest paragraph merges and splits as
  non-owning comparisons without changing accepted correspondences or coverage.

### Tables

- **Ruled grids.** Complete rectangular ruled grids supply an inferred table
  view. Each cell is reconstructed independently, with column identities from
  the first row and row identities from the first column. Values are compared
  within those identities, so equal numbers in different rows never establish
  correspondence. Changed headers can receive inferred candidates from shared
  child or neighboring identities. Grids are limited to 1,024 cells per
  candidate.
- **Borderless tables.** Uniquely located row/column labels from a counterpart
  table can propose a native partition when borders are missing. Target geometry
  supplies the separators and reconstructed axis labels must agree exactly. This
  currently requires one table per page per input, unchanged unique row labels,
  a shared top-left header, horizontal text, and non-crossing cell geometry.
  JSON `table_refinements` records the counterpart table and its evidence.
- **Keyed cells.** Cells without an explicit item key use scoped row/column
  identities; missing, duplicate, or competing axis evidence leaves the scope
  unresolved.
- The original block partition stays available as an alternative view, so
  native text remains comparable when only one side admits a table.

### Tagged structure

Bounded native structure trees are imported with roles, parent membership,
declared order, and byte-exact structure IDs. Marked-content IDs and explicit
references connect tagged paragraphs and cells to the original glyphs, including
inside Form XObjects. Tagged tables with stable IDs preserve cell identity
across value swaps, reordered drawing commands, and an inserted cover page.
Missing, repeated, or incomplete marked content leaves unresolved structure
without deleting native text.

### Text coverage

Image invocations, inline images, painted paths, and shadings leave their pages'
text inventories incomplete, because they may contain or hide text that native
glyphs do not describe. The marker records the render-order boundary of the
last non-text paint so earlier glyphs cannot prove visible-text coverage.
Unused image resources, unpainted paths, and clipping alone do not create
markers.

## Visual channel

Embedded raster images, including those in Form XObjects and with intrinsic
alpha masks, are decoded at their original resolution and hashed with SHA-256
over width, height, and normalized RGBA8 samples. Lossless recompression or
object renumbering alone does not change the hash; lossy recompression can.

- Equal hashes are matched as a multiset, independent of page and draw order.
- Remaining images at a unique identical page and placement are reported as
  `changed` (inferred).
- Residual images become `added` or `removed` only when both inventories are
  complete and no unmatched rival remains; ambiguous replacements and decoding
  failures stay unresolved.

Pages are also rendered at 72 dpi against white and retained for review. Page
rasters are context: the CLI does not emit full-page pixel masks as a second
diff of native text. Scanned and image-only pages keep their pixels for review,
but their text is not recognized, so text coverage for them stays incomplete.

Placement changes, clipping, blend effects, and vector graphics are outside
pixel-hash equality. Stencil images, pattern/Type 3 images, annotation images,
and graphics soft masks remain unresolved.

## Forms channel

Saved AcroForm text, choice, and button values are compared by field name;
malformed values remain unresolved with their raw evidence.

- Button fields retain declared widget appearance-state names and widget object
  references. A saved selection that disagrees with the declared states
  produces a field-local unresolved issue; independent fields still compare.
- Widgets with direct normal appearance streams retain page rectangles, stream
  references, and source-checked crops of the rendered page. JSON distinguishes
  `value_changed` from `rendered_region_changed`.
- Moving widgets or inserting a cover page produces no change; overlapping
  widget crops are ambiguous rather than double-counted.

A crop does not prove what text is visible or that the saved value is displayed
correctly. Export-option interpretation, state-selected appearances, general
stored-value/display agreement, and XFA remain unresolved.

## Relations channel

Typed relationships (row/column membership, labels, captions, references, and
source-structured containment and order) are compared across the endpoint
correspondences established by the other channels. An absent edge requires
complete relationship inventories on both sides; ambiguous endpoints and work
limits stay unresolved. Native layout containment and order do not count as a
semantic relationship change.

Native PDF link extraction and automatic link/footnote target interpretation are
not implemented, so the relations channel is usually reported as incomplete for
real documents.

## Process isolation and resource limits

Every PDF is untrusted input.

- **Native acquisition** runs in bounded child processes: 2 GiB address
  space, 30 s CPU, 35 s parent deadline, 256 KiB request header, and 128 MiB
  response limits. Page metadata and form fields are acquired separately from
  glyphs and tags, so a content-extraction failure can leave forms and
  rendering usable. PDF input is capped at 256 MiB.
- **Page rendering** runs in a child process: 2 GiB address space, 5 s per page,
  30 s per document, 8 million pixels per page, and 256 MiB retained RGB per
  input.
- **Image decoding** runs in a child process: 2 GiB address space, 25 CPU
  seconds, 30 s wall time, 10,000 occurrences, 8 million pixels per image, 64
  million base-image pixels per input, and an 8 MiB response.

The parent validates input hashes, page identity, acquisition roles, and
aggregate limits before comparison. Passwords travel through private stdin and
never appear in responses. Bounded workers support Linux and macOS; on other
platforms the affected acquisition is reported as unsupported.

## Dependencies

- **PDF parsing:** [`lopdf`](https://github.com/J-F-Liu/lopdf) 0.45 with default
  features disabled, confined to the backend adapter. That release bounds
  xref-stream entry counts, parses object streams non-destructively, supports
  `/BrotliDecode`, recovers `/Length` mismatches, and reconstructs broken
  cross-reference data. Backend upgrades must pass capability and hostile-input
  fixtures before adoption.
- **Rendering:** [`hayro`](https://github.com/LaurenzV/hayro), run in a bounded
  child process.
- **Unicode:** [`unicode-normalization`](https://github.com/unicode-rs/unicode-normalization)
  and [`unicode-segmentation`](https://github.com/unicode-rs/unicode-segmentation),
  confined to the normalization module.
- **Serialization:** [`serde`](https://github.com/serde-rs/serde) and
  [`serde_json`](https://github.com/serde-rs/json) with `float_roundtrip`, so
  cached, worker-transported, and reported `f64` evidence round-trips exactly.

`Cargo.lock` pins the reviewed dependency graph, and each dependency sits behind
a boundary that allows it to be replaced.
