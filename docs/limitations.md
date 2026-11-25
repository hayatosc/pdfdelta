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

The flow engine reports one verified token script. One script is not a
unique edit history: repeated equal runs can shift which occurrence is
marked, and a coarse replacement may not localize its interior edit.
When two adjacent equal runs swap order, either run can be described as the
move. Flow prefers the explanation that relocates the smaller structural unit:
the run intersecting fewer reconstructed blocks of the old document, when that
run is at least as informative as the reported one. A paragraph that moved
past a heading and its paragraph is therefore reported as the paragraph move,
not as a move of the heading group. That preference is a quality rule, not
author intent: a swap between runs with the same block span is reported as a
move of whichever run the alignment left out, and a short, low-information run
is never preferred as the moved run. Formatting-only reasons are approximate.
The retained text-scope fields are compatibility positions: an empty or absent
value means the engine did not compute that search, never that a search
completed and found nothing.

## Text extraction

Local optimal-alignment envelopes are optional explanations of supplied token
pairs. They do not establish document correspondence, reading order, scope
closure, extraction completeness, or source ownership. Mandatory masks use the
same bounded local-alignment kernel as the text-envelope command; retaining
more alternative information does not by itself complete a document
comparison. The envelope API
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

- The move detector reports a repeated run as `Move` only when it is long
  enough to identify and carries information weight beyond punctuation. Fuzzy
  or short reorderings can stay `Delete`/`Insert` pairs.
- Extraction gaps make the affected pages incomplete while uninvolved pages are
  still compared. Recovering text inside an unexamined region is future work.
  A document-scoped extraction gap prevents a complete text channel.
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
  reported as its own `Move`, even below the five-token minimum; the word
  order swap `may not always` -> `may always not` is a one-token move of `not`.
  A short equal run that is not adjacent to opposite ends of one move stays a
  `Delete` plus an `Insert`. The move detector otherwise requires a shared
  five-token window and a moved run of at least five tokens, so unrelated
  repeated-word edits can still appear as delete/insert pairs. A `Move` selects
  exit `1`, so these short reorders are never reported as "no difference".
- Flow formatting evidence covers normalization, block structure, effective
  font size, position, and line/page breaks on matched token runs. It does not
  compare font family, fill colour, text render mode, or vector decorations.
  Position requires complete token geometry on the segment being tested and
  uniform baseline translations. That segment's internal line and page break
  offsets must agree; a break difference in one segment no longer suppresses
  position on another segment of the same matched run. A break at an old-block
  boundary is a group boundary and is not counted. A run that moved to another
  page with unchanged internal layout is reported as position, not suppressed.
  Block-structure evidence is a boundary-pattern comparison because flow has no
  separator token. Adjacent entries with the same evidence are coalesced; a
  one-token LCS match inside rewritten text is dropped as a point rather than a
  formatting span, and a point carrying only position is merged into its
  neighbour without turning that position into a span.
- Formatting reasons are approximate and are not interchangeable with the
  removed engine's buckets. The removed engine is not an oracle: its captures
  often had low comparison coverage, and some formatting entries do not
  describe a visible PDF change. On the 62 pairs where both captures now exist
  (including the NIST SP 800-57 retry), a word check (`\w+` words, ±3-page
  window) finds a flow entry with the same reason for 84.2% of the removed
  engine's 2,763 `position` entries, 88.4% of 43 `line_break`, 56.5% of 108
  `normalization`, 57.3% of 358 `font_size`, and 3 of 3 `block_structure`
  entries (strict same-page check: 83.6%, 83.7%, 52.8%, 56.7%, 3 of 3).
  Auditing the misses against PDF glyphs found no actionable
  `normalization`/`font_size` miss: of the 153 missed `font_size` entries, 18
  are identical or differ by less than the 1% relative tolerance, 133 are
  reported by flow under another reason or inside a content change, and 2 are
  not a stable matched span; of the 47 missed `normalization` entries, 41 are
  reported under another reason or inside a content change and 6 are not a
  stable matched span. The `position` miss count is dominated by a real
  root cause: a break difference in one segment used to suppress position for
  the whole run. Position is now tested per segment. One clean sampled
  class-(a) paragraph is recovered; the other five sampled entries are
  garbled/reordered multi-column or form occurrences with no stable glyph
  alignment. Flow is more granular because its
  unit is the maximal matched run, not a block-grouped bucket, so a formatting
  report can contain far more entries for the same pair.
- The default text engine does not produce an assessment, change candidates,
  proven changed regions, unresolved regions, or coverage intervals. Those
  positions are absent from the native schema-12 report rather than retained as
  empty compatibility arrays. Each change carries its own `confidence` and
  `ambiguity` reasons. Absence means the phase does not exist, never that a
  search completed and found nothing. `--trace-json`, `--compact-json`,
  `--full-json`, `--review`, and `--agent-review` are flow capabilities. Flow
  reports carry `"engine": "flow"` whenever the engine runs, including non-text
  channel selections.
- Flow review cases group change and formatting events by result class, token
  geometry page, and event kind. A page with many events is one decision, so a
  reviewer reads a wider quoted range than one change. Flow cases are planned
  first; acquisition cases follow in importance order (broken before
  unexamined, page-local before document-wide), and page-residual cases last.
  When the 4,096-case default budget is exhausted the planner records a
  bundle-level `cases_not_exported` omission with the number of withheld cases
  and stops accepting cases; it does not claim they were exported. A caller can
  raise the limit with `--agent-case-budget` (1–131,072). A plan above one
  512 MiB bundle is published as numbered part bundles under a
  `pdfdelta-agent-review-set` version 1 root manifest; every part is a complete
  bundle. An oversized case is split into consecutive
  `agent-review-split/v1` sub-cases linked by parent, index, and count instead
  of truncating its sources or members; sub-cases count toward the budget and
  `export_complete`.
  On `nist-controls` the planner still retains planned cases in memory. The
  default 4,096-case export publishes two parts (about 0.6 GB) and withholds
  about 85,600 sub-cases. A budget large enough for the full plan exceeds the
  6 GB process budget, so a complete export inside 6 GB currently requires
  streaming case/part writing, which is not implemented. Candidate enumeration
  is `unknown` for every flow case because the engine enumerates no competing
  correspondence universe. Retained text and source lists carry explicit
  omissions when a limit is reached. The HTML review does not
  publish per-glyph token sources; `--agent-review` and `--full-json` do. A
  native flow bundle never offers page pictures, because flow retains no
  rasters.
- Flow text completeness is extraction completeness, glyph conservation, and
  equality of the non-text paint inventory. Paths, images, inline images, and
  shadings are hashed by kind, exact shape or image data, and page-space
  placement; the page number is excluded so an identical page shifted within
  the document stays equal, while page crop and rotation are included.
  Artifact-marked paint is included. If the multisets differ or an identity is
  unavailable, the text channel is incomplete and says `non-text paint differs;
  text drawn as paths or images was not examined`. Changes are still reported.
  On the 64 H1 pairs, 63 have a paint difference and one (`irs-w2`) stays
  complete; a page-local vertical-shift variant measured in M4b recovered no
  additional pair.
- Flow exit policy: a channel that detected a typed difference with its own
  evidence intact selects exit `1` even when another channel is broken or was
  not examined; `comparison_complete` stays `false` and `exit_basis` records
  the coexistence. Broken evidence blocks exit `1` only for its channel, and
  cross-channel or unknown broken reasons block every channel. Exit `3`
  remains when no channel has an intact detection and something is broken or
  unexamined. Exit `0` is only issued when every selected channel was examined
  and nothing is broken. This is why a flow exit `1` can coexist with broken
  forms, relation limits, visual backend failures, or unexamined paint.
- Text views remain correspondence context for relationships and structure,
  but an unselected text channel cannot justify exit `1`, and text-only
  obligations cannot keep `comparison_complete` false. Components that mix
  text with non-text sources keep every obligation. `flow_owned_text` (also
  present at `comparison.flow_owned_text`) records shared-evidence text
  components and local comparisons that flow ownership replaced, with an
  `unchecked_obligations` count per skipped reason. Absence means no such
  position was recorded, not that a text search completed.
- Flow paint-gate issues have their own `unexamined_paint_issues` counter in
  the native flow summary and `flow_text.summary`; they are excluded from
  `unresolved_extraction_issues`. Reports written before this counter existed
  included paint issues in the unresolved extraction count.
- Superseding text evidence does not relax structure identity. Native
  `pdf-structure-id` key obligations belong to relations/structure and
  `pdf_field_name` obligations to forms; they block exit `1` only for those
  channels. An obligation records why a key claim could not be established (a
  missing inventory, unnamed or duplicate members, a retained rename
  alternative, or an unavailable acquisition path), so it is unexamined
  evidence on its channel. An exhausted raw key search is cross-channel
  broken. An exhausted scoped key search, or a scoped `work_budget` obligation,
  is broken on every channel that owns a claim in the search; other scoped
  obligations are unexamined on their claim's domain.
  In the 36-pair real-world corpus, 35 pairs exit `1` through an intact text
  detection; only `mhlw-care-skills` stays exit `3` because its flow text
  extraction itself is broken.
- Exit `0` is not reachable with the default channel set for a document with
  pages yet. Rendered page rasters are retained for review but not compared,
  so each rendered page leaves the visual inventory incomplete, and native
  relation endpoints and semantic relationships are not established for a
  tagged document, so relations stays unexamined; a pair where both inputs
  prove no relation source has a complete empty inventory. An identical or
  purely reflowed document
  therefore exits `3` with `exit_basis: "unexamined_evidence"` and
  `comparison_complete: false`, not `0`. `--channels text`, `--channels
  text,forms`, and `--native-text-only` report the verdict of the channels
  they select; they exit `0` when those channels are complete and unchanged.
- A text extraction issue about a page whose only possible text is inside
  painted images is unexamined text evidence, not ignored: it cannot allow exit
  `0` (or a "complete unchanged" text channel) but does not block a flow text
  change. The flow paint gate covers changed or unavailable paint on those
  pages; it does not establish text inside an unchanged image.
- Flow reports a `Move` as a detected difference, including a one-token move
  such as `may not always` -> `may always not`; layout-only changes do not count,
  but a layout-only shift that changes token order is reported as a move and
  selects exit `1`.
- The flow HTML review's `sources.json.gz` uses the version-2 nested-gzip shape
  (a gzip member inside another) in manifest version 3. The compatibility
  reader accepts the single-stream version-3 shape, the older double-gzip
  shape, and the oldest uncompressed `sources.json`; old bundles remain
  readable. The second pass keeps the source index near its previous size; the
  JSON content is unchanged.
- The render child preflights image XObjects before rendering. A page that
  contains a single image above 64M declared pixels is reported as a
  page-scoped `resource_limit`, not a renderer backend failure. This covers
  the real-world `mext-primary-japanese-2008-to-2017` new page 83 (0-based
  `PageId` 82): a 19,848 × 28,063 1-bit mask whose full-size RGBA conversion
  requested a 2 GiB allocation and aborted under the child's 2 GiB
  address-space limit. The scan reads image dictionaries and recursively
  follows Form XObject resources with depth and entry bounds; it never decodes
  an image stream. Inline images are preflighted from their declared
  `/W`/`/H` (or `/Width`/`Height`) by the shared content parser, with the same
  page-scoped `resource_limit` outcome; image data is skipped, never decoded.
  Reproduce the child path with `pdfdelta render-page 82 249 596 842 235 0 <
  mext-primary-japanese-2008-to-2017-new.pdf`; it exits 3 without rendering.
- Flow's confidence marker is a cheap heuristic, not a uniqueness guarantee.
  Repeated runs, numeric-only spans, numeric table neighbours, order-uncertain
  pages, coarse replaces, and moves of at most two tokens are flagged; a
  unique changed run can still have another valid alignment that the marker
  does not see. The marker never changes the exit code.
- Channel rendering budgets every page by child CPU time: five CPU seconds per
  page. The current child-reported measurement over the 4,654 rendered pages of the
  36-pair channel panel is median 0.032 s, p99 0.270 s, p99.9 1.336 s, and max
  2.899 s, which is 18.5x headroom at p99 and 1.72x at the maximum. The maximum
  is two reproducible `ipcc-synthesis-ar5-to-ar6` pages at 2.86-2.90 s; the
  earlier `/proc`-sampled 0.84 s figure understated those pages. A page that
  exhausts this budget is a
  resource-limit outcome and retains no raster, never an empty page. Each input
  document also has a cumulative child-CPU budget of 60 s; measured per input,
  the maximum was 26.49 s and p99 26.49 s over the 72 documents of the 36-pair
  panel (with 72 documents the nearest-rank p99 is the maximum), a 2.26x
  headroom. The page that crosses the document budget is retained; later pages
  are `resource_limit` with reason `document CPU budget exhausted`. Failed
  render attempts charge the full page budget, and a page rejected before
  rendering (for example by the raster budget) charges nothing. The page and
  document wall-clock caps (120 s and 300 s) are safety stops only; their
  expiry is reported as a distinct resource-limit reason and is explicitly
  dependent on machine load. The retained-raster budget is deterministic.
  Rerunning the five heaviest render pairs under an artificial 25% CPU quota
  kept every exit and summary equal and did not fire a render CPU or wall
  budget. The `mhlw-care-skills-original-to-revised` image-hash worker did hit
  its separate 30 s wall cap under that quota, so its image inventory became
  more incomplete; that is a load-dependent image-worker safety outcome, not a
  render-budget change. The old
  `mhlw-care-skills-original-to-revised` 167-170 region splits came from the
  former 30 s document wall cap; with CPU budgeting the old side renders until
  its fixed raster budget (178 of 272 pages) and its logical report is
  identical across idle and loaded runs. `nist-contingency-34-to-r1` likewise
  renders all pages in every run. Do not pin a report whose wall safety stop
  actually fired; no such run was observed in the earlier panel or in the
  current 36-pair channel panel.
- A `--full-json` capture retains per-glyph sources for every token. The
  serializer streams token sources, and a side built in a child process spills
  its per-glyph records to a temporary source cache that the reporting parent
  copies back in token order, so it does not retain the extracted document. On
  the largest H1 pair (`M/gcc-13-3-to-13-4`, `--limit-scale 2`) the peak
  resident set was 2,180,660 KiB (about 2.1 GiB), close to the 2,056,812 KiB
  compact text-only run; the former in-memory full report peaked at
  5,136,876 KiB. The output is still large: the ten largest pairs produced
  609.7 MB of gzip. Capture only with an explicit memory and output budget,
  and delete the reports afterwards.
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

The current flow build was measured on the frozen corpora with the shared
6 GB limit, sequential runs, and default budgets. On the 64-pair native flow
panel, **1/64 is a complete text comparison**
(`P/irs-w2-2024-to-2025`); 62/64 detect a difference but stay incomplete
because non-text paint or an image-only page was not examined, and 1/64 ends on
broken evidence. On the 36-pair default-channel panel, 35 pairs detect a
difference with incomplete evidence (exit `1`) and 1 ends on broken evidence
(exit `3`); no pair reaches `comparison_complete`, because the visual and
relations channels remain unexamined for real documents. All pairs produced
reports without timeout or OOM. These are completion counts, not accuracy or
change-recall figures.
