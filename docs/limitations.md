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
  Embedded Type1 fonts without a PDF `/Encoding` may use the cleartext built-in
  Type1 encoding array; glyph names are resolved only through the Adobe Glyph
  List, never by shape. Font-specific names absent from that list stay
  unmapped placeholders.
- An explicit ToUnicode map takes priority, and gaps in it stay unmapped. Other
  unmapped glyphs are comparable only with a stable embedded-font, Standard 14,
  caller-asserted font identity, or — for Type 0 fonts without ToUnicode or an
  embedded program — a predefined CID collection (Adobe Japan1, GB1, CNS1,
  Korea1, or KR), where `(Registry, Ordering, CID)` identifies the glyph.
  With the `predefined-cid-unicode` feature used by the CLI and benchmark
  binaries, Adobe UCS2 collection tables map known CIDs to their ordinary
  Unicode text; unknown CIDs, libraries built without the feature, and
  non-Adobe collections keep placeholder text rather than invented Unicode.
  MMType1 variation axes and Type 3 CharProc drawing are not interpreted.
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
  work share the page's clipping work budget. Each page starts with the full
  budget, so a page's classification cannot depend on how much work earlier
  pages consumed; the document total is bounded by the page count times the
  per-page limit. Active exhaustion is still an error.
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
- The `flow` subcommand inherits unresolved reading-order uncertainty from layout
  reconstruction: it does not propagate layout issues into `FlowToken.uncertain`,
  so tokens from an inferred block or line order remain comparable. It builds
  layout, paint-order, and baseline-row hypotheses per page and chooses among
  them using n-gram evidence from the counterpart pages found by an order-free,
  information-weighted bag overlap; a page where the layout and paint-order
  hypotheses disagree is reported as order-uncertain, and pages below the
  order-coherence threshold are listed, but neither marker changes the exit
  code. A page whose best overlap is below 30% of the pair's median per-page
  best overlap is ordered by spatial coherence alone. The choice can still be
  wrong on tables and forms, a heavily rewritten page can be ordered differently
  from a near-identical copy of the same page, and table-of-contents pages where
  only page numbers shift can surface as `Move` changes when neighbouring tokens
  happen to match. Near-tie pages can choose a different hypothesis than an
  independent reader even when every local score is close: on the real-world
  corpus, NIST SP 800-171 r3 pages 11 and 12 choose layout over paint order by
  517 and 88 weighted points, while the PyMuPDF oracle order agrees with paint
  order, leaving that side's oracle agreement 0.0052 below the pre-change value.
  Both pages are reported as order-uncertain. The independent reader interleaves
  the page's footnote text with the body at each reference in paint order;
  layout order keeps the body prose continuous and the page's footnote lines
  together, which is the reading a human expects on these pages, so the
  disagreement is reported rather than tuned away.
- A short moved run that is just a rotation remainder around a larger move is
  reported as its own `Move`, even below the five-token minimum. A short equal
  run that is not adjacent to opposite ends of one move stays a `Delete` plus
  an `Insert`. The move detector otherwise requires a shared five-token window
  and a moved run of at least five tokens, so unrelated repeated-word edits can
  still appear as delete/insert pairs.
- The text-edit and page-merge benchmark cases have known boundaries. A digit
  edit inside a furniture digit run that varies across pages (a page number or
  a per-page form code) is masked by design; the harness records it as a
  page-number-like skip rather than a content change.
- Stacked-band order adoption requires a vertical gap of at least four median
  row pitches, at least two rows on each side, and at least a fifth of the
  page's tokens in each band. A merge seam narrower than that, or a band whose
  weighted bag overlaps no whole page of the other document at 0.9, keeps the
  page's evidence-based whole-page order. Such a seam can still reorder content
  or split a word, and the merge benchmark would report it as a finding.
- The 3,100-page PostgreSQL manual has no old/new revision pair, so only A-vs-A
  identity is measured. At the default extraction limits the document stops at
  the 5,000,000-glyph budget; `pdfdelta flow --limit-scale 2` completes the
  identity comparison in 131 s with an aggregate peak of 4.46 GB. The engine
  does not raise limits automatically, because every PDF is untrusted input.
- The page-counterpart search uses an inverted index from each
  information-weighted token to the pages containing it, so only page pairs
  that share a token are scored; a pair with no shared token has zero overlap
  by definition. The result is identical to the former all-pairs scan, but a
  frequent token still has a long posting list, so the work is bounded by the
  total postings length rather than the page-pair count. At 3,100 pages the
  PostgreSQL A-vs-A comparison dropped from 159 s to 133 s; the move-rule
  variants now reuse one memoized alignment per strategy, so the A-vs-A run is
  131 s instead of 208 s and remains byte-identical.
- The differential audit compares flow with a PyMuPDF reading order and a GNU
  `diff --minimal` script. Its per-pair agreement is not an exactness metric:
  moves, token-boundary choices, numeric page numbers, and unmapped glyph
  placeholders produce one-sided tokens. A 200-region stratified sample gives no
  "oracle correct" verdict: 75 flow-correct and 121 both acceptable. Four
  regions were undecidable from a single page's word list; a separate pass
  using the exact glyph boxes, PyMuPDF character boxes, and the content-stream
  operators decided them: two are both acceptable (an equivalent minimal
  script around a contents-page dash, and form-field tokenisation), and two
  are flow correct (the Khronos `alpha` -> `α` revision and the OASIS URL
  version change are reported). The 200 verdicts remain a stratified sample
  and cannot prove un-sampled regions. Regions are split at page boundaries
  before clipping, and a verdict is only "oracle correct" when the changed
  tokens occur nowhere in the other document and in no flow change; the
  single-page clip alone cannot see a region the oracle placed on another page.
  The targeted pass classifies every flow-only token of the three most
  asymmetric pairs per side by change kind and cause.
- The move rule requires information weight, not token count: a run whose
  tokens are all minimum-weight punctuation or separators contributes zero and
  is not a move. A same-page move on a dot-leader contents page is promoted
  only when all surrounding changes are low-weight or numeric and the resulting
  script does not increase non-move changed tokens; the old rule is always a
  fallback. A move with no informative token becomes delete/insert/replace, and
  different rule variants can represent the same edit as a move or as several
  replacements, so move-token totals can change while non-move totals do not
  rise.
- The corpus keeps 286 placeholder tokens after the pdf.js supplement and the
  project-authored unambiguous table, down from 4,651. 251 use name identity:
  22 Khronos `negationslash` (a combining overlay), 201 unicode-standard
  generic or script names (`gNN`, `glyph296`, `modi_*`, `tibetan*`,
  `U0C95_U0CCB.blwf`, …), and 28 CID 0 `.notdef` placeholders. 22 use
  CID-keyed CFF outline identity from the `CharStrings` entry selected by the
  CID. 13 remain program identity: one simple TrueType code in the NASA
  handbook whose `post` table is format 3.0 and whose best `cmap` has no entry
  for the code, so neither a name nor a code-to-glyph outline path exists. The
  standard-14 Symbol and ZapfDingbats Annex D tables recover the codes they
  define (`ellipsis`, the dingbat `aNN` list); a name outside those tables
  stays a placeholder. A reverse-mapping fallback over the bundled Unicode
  CMaps is not shipped for CID fonts: the corpus has no composite-font CID that
  the collection table misses, and one of the three probe CIDs is ambiguous
  (U+02BC and U+2019). Placeholders stay uncertain and are not content changes
  that the engine could report as text.
- The bundled Apache-2.0 pdf.js supplement has 121 entries and resolves 4,167
  corpus tokens. It is consulted only after the Adobe Glyph List and the
  `uni`/`u` algorithmic forms; it contains no AGL name, no private-use value,
  and no control value, so it cannot shadow AGL or turn a placeholder into
  non-text code points. All pdf.js PUA-named entries duplicate AGL names and
  were already excluded; the only non-AGL control entry (`controlNULL`, U+0000)
  is excluded. Size variants intentionally map to the base character
  (`parenleftBig` and `parenleftbig` both become `(`), so a size change is not
  visible as a text change; the token is no longer a placeholder once a table
  entry exists.
- The project-authored `self_mappings.rs` table (MIT, no copied third-party
  data) adds 7 names / 198 corpus tokens: `lessmuch` U+226A MUCH LESS-THAN,
  `greatermuch` U+226B MUCH GREATER-THAN, `floorleft` U+230A LEFT FLOOR,
  `floorright` U+230B RIGHT FLOOR, `ceilingleft` U+2308 LEFT CEILING,
  `ceilingright` U+2309 RIGHT CEILING, and `asciigrave` U+0060 GRAVE ACCENT.
  It is consulted after AGL and the pdf.js supplement. Deliberately excluded:
  `negationslash` (U+0338 COMBINING LONG SOLIDUS OVERLAY; attachment order is
  not encoded by the glyph sequence), `arrowrightabove` (U+20D7),
  `circleencl` (U+20DD), `circleslashencl` (U+20E0), and `diamondencl` (U+20DF)
  (combining/enclosing marks with the same context problem),
  `U0C95_U0CCB.blwf` (a script base+combining sequence with a below-base-form
  suffix), generic `g109`, `g1093`, `g111`, `g131`, `g132`, `g133`, `g1397`,
  `g14`, `g210`, `g218`, `g3`, `glyph296`, and script names
  `modi_letter_ha`, `modi_letter_ka`, `modi_letter_ra`, `modi_letter_sa`,
  `modi_vowel_sign_aa`, `modi_vowel_sign_e`, `tibetangha`,
  `tibetangnyisshad`, `tibetanintertsheg`, `tibetanka`, `tibetankha`,
  `tibetanshad`. The only local sources for those names
  (`glyphtounicode.tex`/`texglyphlist.txt`) are GPL-2.0-only and are not
  compatible with the MIT project.
- The Adobe Glyph List itself maps 192 `*small` names to the Private Use Area
  (for example `Asmall` to U+F761). That pre-existing mapping is unchanged; the
  pdf.js supplement adds no PUA mapping and rejects PUA values at runtime.
- Name identity hashes the subset-stripped `BaseFont` and the exact glyph name,
  and keeps the name's own semantics rather than resolving it to Unicode. Two
  different fonts in one document that share a family and a glyph name
  therefore compare equal for that name; this is intended. For an
  Identity-encoded composite font, CID 0 is treated as the specification's
  `.notdef` even though a CID-keyed program stores only a CID; named or
  predefined collection encodings keep the program identity for CID 0. A
  broken producer that reuses CID 0 for a visible glyph would merge those
  placeholders.
- Outline identity hashes the exact CID-keyed CFF `CharStrings` bytes. A
  re-subsetter that changes subroutine calls or otherwise rewrites the
  charstring encoding without changing the drawn outline changes the identity.
  Outline identity is not implemented for simple Type1, Type1C, or TrueType
  programs: every such placeholder in the corpus has a name, and the only
  nameless simple TrueType code has no `cmap` entry to select a glyph. CID 0
  uses `.notdef` name identity rather than an empty outline; an empty
  `CharStrings` entry is never accepted as outline evidence.
- The Type1C built-in encoding reader supports a single-font CFF program with
  charset formats 0, 1, and 2 and Encoding formats 0 and 1 plus supplements. It
  does not read CFF2, OpenType (`FontFile3`/`OpenType`), TrueType `post` tables,
  or a multi-font CFF FontSet, and a name outside the Adobe Glyph List stays a
  placeholder. The standard-14 Symbol/ZapfDingbats tables are not consulted for
  an embedded program carrying the same name.
- With raised extraction limits the command builds each side in a child process
  and caches the three candidate token streams in a private per-run directory
  (mode `0700`) under the system temp directory. The directory is removed on
  success, error, and child failure; a process killed without running its
  destructors can leave one behind. Concurrent runs use distinct
  process-id/sequence names and do not share a cache. The cache carries a
  version, explicit lengths, and an FNV-1a checksum; it detects accidental
  truncation or corruption, not deliberate modification by the same user.
- Image-only pages first try a sufficient encoded identity: the same ordered
  image XObjects, byte-identical encoded streams, pixel-affecting image
  dictionaries and CTMs prove identical pixels without decoding. Equal encoded
  identities resolve a page; unequal encoded bytes prove nothing, so the page
  falls back to decoded pixel hashes. A complete decoded inventory resolves the
  page only when every painted image is within the per-image and per-page
  pixel limits (a single 2480 x 3507 RGB page render is supported). Pages with
  too many images or larger rasters remain incomplete; no OCR or partial image
  inventory is used. Reports name the rule (`encoded` or `pixels`) that
  resolved each page. An image-only page present on only one side is reported as
  an image-page deletion or insertion once its own identity is available; only
  an unidentified one-sided page remains unresolved.
- An unsupported clipping path retains every glyph that overlaps its outward
  control-point bound, with a conservative `PartiallyOutside` clip status and a
  page gap, so the page remains incomplete and its tokens are uncertain. A glyph
  strictly outside that bound is treated as clipped and excluded; visibility
  inside the bound is not proven. Straight-edged polygon clips additionally
  classify glyphs as Inside, Outside, or PartiallyOutside with conservative
  orientation and boundary margins. Curved clips are conservatively flattened
  with a chord tolerance of 1/64 of the glyph box's smaller side; the widened
  margin keeps boundary cases Partial, and flattening depth/segment exhaustion
  falls back to the bounding-box rule.
- Every unmapped-glyph identity is a 32-byte SHA-256-derived hash. The first
  byte records the evidence kind (`program`, `name`, or `outline`), leaving 248
  bits; a different name or outline therefore does not compare equal except
  with the same negligible collision probability as the program identity.
  Glyphs remain unmapped placeholders and no Unicode is invented.
  ToUnicode/AGL keep priority. Name identity needs a `BaseFont` and a non-empty
  name; outline identity needs a CID-keyed CFF program and a non-empty
  `CharStrings` entry; otherwise the program identity remains and the glyph is
  unresolved if no program is embedded.

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
