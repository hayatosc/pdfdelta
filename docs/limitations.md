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

An additional optional rule handles differing block splits only when the
complete exact multi-block parent is already established and both literal
sequences are identical, nonempty and no longer than 512 tokens. It requires
strict source evidence for all nonwhitespace parent tokens before publishing
any child. Each side stays on one page, whose ordinal may differ under that
parent correspondence. Whitespace, candidates, coarse overlaps and previously
owned ranges are excluded. It cannot discover a missing parent correspondence,
resolve unequal parent sequences, repair extraction or relax strict completion.
It is skipped when the preceding initial-component pass added relations, so
the original late evidence capture keeps its existing resource priority.

## Text extraction

Local optimal-alignment envelopes are optional explanations of supplied token
pairs. They do not establish document correspondence, reading order, scope
closure, extraction completeness, or source ownership. Mandatory masks use the
same proof kernel as existing local claims; retaining more alternative
information does not increase strict comparison completion. The envelope API
currently refuses optional-normalization families instead of selecting one
interpretation. Its graph and counterpart lists consume additional time and
output space, bounded independently of existing comparisons.

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
- Supported clips include rectangles, certified convex quadrilaterals, and
  two explicitly closed, strictly separated axis-aligned rectangles, or a
  certified six-edge axis-aligned L shape or eight-edge single-side rectangular
  notch under an unbounded or strictly witnessed
  rectangular prior clip. A single rectangle and L shape may also form a pair
  when their outer bounds are strictly separated. Approximate prior rectangles
  retain uncertainty for these new proofs.
  The rectangles retain their excluded gap; edge notches retain their full
  rectangular cutout and the L shape retains its corner
  cutout under either fill rule. Further L-shape and edge-notch intersections
  require strict
  rectangle witnesses. Further rectangle/L-pair intersections also require
  strict rectangle witnesses; general L-shape intersections with convex or
  compound regions
  remain unsupported. An additional demand-driven budgeted fallback supports literally closed, finite orthogonal
  subpaths with exact source provenance, including overlapping bounds and holes
  under the specified fill rule. Before a glyph activates the proof, passive
  drawing evidence retains its original uncertainty. A separate exact point-witness
  fallback can certify partially clipped glyph boxes for finite, literally closed
  cubic/straight paths in one pure construction run ending in `W n` or `W* n`,
  within a single content stream and fixed transform. It requires an unbounded
  or strictly witnessed rectangular prior, or a predecessor from the same owned
  witness chain. All predecessor paths are retained; subsequent rectangles need
  strict source witnesses. A separate whole-box proof requires every closed
  boundary control bound to be strictly disjoint from the glyph box and an exact
  chord winding for every source constraint and crop. It cannot use equal probes
  as a substitute for complete boundary exclusion. Integer overflow, missing
  opposite point witnesses without a whole-box proof,
  source spans crossing streams, intervening context changes, open subpaths,
  uncertified priors and unaffordable preparation remain unsupported. Preparation
  additionally requires a text-object or text-show operator within the next 16
  typed operations, preceded only by path/clip operations; the metered admission
  hint can conservatively refuse other contexts. Validation and the maximum
  hint cost must be affordable before inspection; earlier hint work is never
  refunded. Equal probe
  values never prove an entire box inside or outside. Subdivision and source
  chains are explicitly bounded; all source capacities, potential edges and query
  work share the existing clipping ceiling. Active exhaustion is still an error.
  Other diagonal/curved compositions remain unsupported. Later straight
  segments must fit within one certified cell; crossing several cells is
  conservatively held. Nonrectangular compositions outside the owned witness
  chain, transparency,
  colors, and occlusion by later paint remain
  unsupported. Glyphs fully outside the CropBox or a supported clip are excluded
  from comparison; uncertain clipping preserves an extraction issue.

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
