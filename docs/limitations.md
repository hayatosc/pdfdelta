# Current limitations

`pdfdelta` is early-stage software. When it cannot establish a result it
reports the region as unsupported or unresolved and exits with code `3`; it does
not silently treat missing evidence as "no change". This page lists the main
gaps so you can judge whether a result applies to your documents.

## Scope

- **No OCR.** Text inside images, scanned pages, and handwriting is not
  recognized. Such pages keep their rendered pixels for review, but their text
  coverage stays incomplete.
  Native extraction also leaves a page unresolved when a potentially visible
  image was invoked and no comparison-visible native glyphs were extracted:
  whether the image
  contains text is unexamined. Unused image resources and images proven outside
  the page crop or active clip do not trigger this guard. Invisible or excluded
  glyphs remain available as raw evidence without dismissing image uncertainty.
  Path-only empty native
  extraction still describes only native text scope, not the absence of text
  drawn as outlines.
- **Relations are rarely complete.** Native PDF link extraction and
  link/footnote target interpretation are not implemented.
- **Forms:** export-option interpretation, state-selected appearance rendering,
  general stored-value/display agreement, and XFA remain unresolved.
- **Images:** placement changes, clipping, blend effects, and vector graphics
  are outside pixel-hash comparison. Stencil images, pattern/Type 3 images,
  annotation images, and graphics soft masks are unresolved.
- **Rendering** is supported on Linux and macOS only, at 72 dpi, and not for
  password-protected inputs. Renderer warnings, incomplete annotation support,
  and unknown content regions keep visual coverage incomplete.
- **Tables:** arbitrary table semantics, merged cells, and general row/column
  insertion or deletion are not established. A first-row/first-column header
  interpretation can be wrong even when the grid geometry is clear. Borderless
  table recovery requires one table per page per input, unchanged unique row
  labels, a shared top-left header, and horizontal text.
- **Structure trees:** named property lists, custom role maps, table-axis
  interpretation, and object-reference bindings are not implemented.
- General split/merge discovery, general footer recognition, and full document
  structure discovery remain incomplete.

A coarse interior changed-region proof does not identify a unique deletion,
insertion, or glyph mask. The optional mandatory-boundary rule currently holds
one-sided interiors, whitespace-only changes, equal-multiset reorderings, nonwhitespace characters without literal
glyph sources, shared glyph sources, source-map cuts that split a source, and
unsupported whitespace joins. It keeps the candidate alternatives and unresolved
coverage; reporting such a region does not establish full-document accuracy.

A separate optional rule can resolve literal nonwhitespace equal subruns inside
a two-sided whole single-block coarse region, with a complete clean parent,
mandatory unique counterparts, singleton raw glyph evidence, safe normalization
cuts and no source reuse. Ordinary certificates require the same page. A
separate final route can certify paired equalities on different pages only
under an isolated, complete exact whole single-block parent on each side, with
no ancestor dependency and an explicit page-shift certificate. Each block must
still belong to exactly one page; all literal-source checks remain required.
The rule excludes partial or one-sided coarse
regions, candidate overlaps and uncertain ancestors, and runs only with budget
left after earlier proofs. Such subruns establish literal equality under those
premises; they do not identify a unique edit history or resolve the remaining
ambiguous positions. The containing coarse proof still prevents completion.

## Text extraction

- Extraction mainly targets single-column text in the supported font subset:
  Type 1 / Type1C / MMType1 and TrueType simple fonts, axis-aligned Type 3 with
  declared metrics, and Type 0 fonts with one CIDFontType0/CIDFontType2
  descendant.
- Type 0 support covers Identity-H, a bounded Identity-V subset, full-domain
  identity CMaps, and UniJIS-UTF16-H. General custom CMaps are not supported.
- An explicit ToUnicode map takes priority, and gaps in it stay unmapped. Other
  unmapped glyphs are comparable only with a stable embedded-font, Standard 14,
  or caller-asserted font identity. MMType1 variation axes and Type 3 CharProc
  drawing are not interpreted.
- Rotated, sheared, translated, or reversed Type 3 font matrices, non-downward
  vertical advances, and general vertical-writing reading order are not
  implemented.
- Ordinary line-end hyphens are retained. Ambiguous discretionary hyphens keep
  both interpretations, and only facts shared by every interpretation are
  reported.

## Reading order and layout

- A multi-column or label/value layout is accepted only when paint order
  independently confirms the spatial order. Ambiguous multi-region layouts,
  right-to-left text, non-horizontal text, and mixed or unknown text direction
  preserve every glyph but report the affected window as unresolved reading
  order.
- Curved or compound clipping paths, transparency, colors, and occlusion by
  later paint are not interpreted, so overpainted text can still appear in the
  comparison input. Glyphs fully outside the CropBox or a supported rectangular
  clip are excluded from comparison.

## Diff and reporting

- Paragraph moves are reported as `Move` only when a unique out-of-order anchor
  matches exactly. Fuzzy moves stay candidates or unresolved.
- Page- and glyph-scoped extraction gaps leave their anchor windows unresolved
  while other windows are still compared. Recovering changes inside an
  unresolved window is future work. Document-scoped gaps prevent correspondence
  entirely.
- Formatting-only reporting is best-effort and does not claim rendering
  identity.
- Text reports do not list per-span provenance (JSON does), and SVG output is a
  whole-document glyph overlay rather than a per-change overlay.
- Atomic report publication requires a filesystem that supports same-directory
  hard links.

## Evaluation coverage

The acceptance and benchmark suites are described in
[`benchmarks.md`](benchmarks.md). They exercise generated fixtures, a small set
of vendored Typst and Tectonic pairs, and a non-vendored corpus of public
revision pairs. On large real-world documents, recall is still low and many
comparisons end incomplete or at a resource limit. These results do not imply
support for arbitrary PDFs.

The 2026-09-27 native-text benchmark of HEAD `cf5feb9`, with no production
code changes and a shared 6 GB memory cap, records **3/36 complete comparisons
(8.3%)**, with 33 incomplete. All pairs produced reports without timeout or OOM.
Two of the three complete pairs have no visible native text on the old side. See
[`Native-text completion coverage`](benchmarks.md#native-text-completion-coverage)
for the build identity, outcome breakdown, and verification limits.
