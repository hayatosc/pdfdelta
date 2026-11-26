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
  -> flow token stream
  -> verified token diff
  -> changes, formatting evidence, and channel evidence
  -> confidence, coverage, and unresolved regions
```

The design rules are:

- **Preserve raw evidence.** Glyph text, raw codes, geometry, render order,
  render mode, and object/operator provenance survive every stage.
- **Keep parser types behind a neutral facade.** The PDF library is an adapter
  behind `PdfParser`/`ParsedPdf` and can be replaced.
- **Make layout reconstruction reversible.** Line and block boundaries are
  hypotheses; the token stream can recover when they are wrong.
- **Diff as tokens, not lines.** Line and block boundaries become token
  metadata, and the final token diff is exact and replayed before it is
  reported.
- **Report what is unknown.** Unsupported filters, encryption, unmapped glyphs,
  and uncertain regions are reported as unsupported or unresolved, never as
  empty text.

The text channel reports one verified script with confidence and ambiguity
markers. The non-text channels separate *established* differences, *tentative*
candidates, and *unresolved* content; correspondences that depend on an
interpretation (for example, a table layout inferred from ruling lines) are
marked *inferred*.

The implementation is pure Rust and makes no external analysis API calls.

## Common correspondence solver

The non-text channels feed a common correspondence solver. Its versioned
objective prioritizes source-backed scoped identities (structure IDs, field
names, row/column keys), then literal content, then inferred supplier scores.
Literal fragments cannot displace a competing keyed item merely by matching
more unchanged pieces.

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

`pdfdelta` has one text engine: the token-stream flow engine. Extraction and
layout reconstruction are shared with the other channels, and the flow
engine turns their comparison-visible glyphs into one verified token script.
`--engine flow` is the default and is accepted as a compatibility no-op;
the removed `--engine proof` value exits `2` instead of running a second
engine.

### Extraction

- A backend-neutral PDF parser boundary with a `lopdf` adapter: classic xref
  tables, xref streams, object streams, incremental revisions, inherited page
  resources, password decryption, partial page-tree recovery, and bounded
  stream decoding.
- Bounded content-stream extraction of text and straight-path operators,
  normalized page boxes, CropBox classification, axis-aligned rectangular
  clipping, certified convex quadrilaterals, strictly separated rectangular
  pairs, certified six-edge L shapes, eight-edge single-side rectangular notches,
  a strictly separated rectangle/L-shape
  pair, and isolated Form XObjects. Rectangular pairs keep both components
  and their gap; L shapes retain an exact rectangular
  corner subtraction; edge notches retain their entire side cutout. Literal closure, finite axis-aligned edges and cyclic
  boundary equality are required. Eight-edge proofs try both side placements
  after exact axis transposition, under a fixed prepaid work bound. The rectangle/L pair retains both the
  inter-component gap and the corner cutout; their outer bounds must be
  strictly separated. Each proof is prepaid and each classification/copy is
  charged to the page's clipping work budget. A demand-driven fallback can certify literally closed orthogonal subpaths,
  including multiple components, overlaps and holes, under a strictly witnessed
  rectangular prior clip. It partitions by exact endpoint coordinates and
  evaluates integer winding or parity on each open coordinate cell, without
  midpoint rounding, area sums or pixel sampling. Source retention is prepaid when a candidate is frozen; geometry is proved
  only when a glyph needs its visibility. Validation and coordinate storage are prepaid first. Their exact cell/edge
  population is then known and prepaid before any winding loop or cell
  allocation. Both phases share the page's clipping work budget; no
  candidates are discarded and spent validation work is not refunded.
  Unaffordable proofs keep the original unsupported outcome. Each later
  classification/intersection pays for all retained cells. Proofs are shared only
  by clones of the same frozen source owner; cropping remains state-local.
  Passive drawing uncertainty is retained until a glyph activates the proof. Nonrectangular
  compositions remain unsupported.
  A separate fallback can certify partial glyph boxes against literally closed
  cubic and straight paths from a pure construction run ending in `W n` or
  `W* n`, within one content stream and under one fixed transform. It freezes
  the complete typed source run in an immutable owner after charging validation,
  actual operand/operator capacities, copied headers and every potential output
  edge, including four edges for each rectangle. Finite canonical coordinates,
  literal closure and strict primitive/prior provenance are checked before a
  source becomes a constraint. Admission first checks at most 16 following typed
  operators for a local text-object or text-show operation, with only path/clip
  operators before it. Every inspected operator costs 16 units of the same shared
  clipping-work budget, including unsuccessful checks. Source validation plus
  the maximum admission cost must be affordable before any hint inspection.
  This bounded demand hint
  can refuse broader contexts and never certifies geometry or visibility.
  Later pure path constraints keep their complete
  predecessor chain; strict rectangular crops remain local to the graphics state.
  Only clones of that same source owner share prepared geometry.
  A whole-box certificate additionally requires every boundary edge's closed
  control bounds to lie strictly beyond one side of the closed glyph box.
  The complete box then avoids every curve-to-chord homotopy. One exact dyadic
  chord winding, under each original fill rule and every predecessor/crop,
  establishes membership throughout that connected box. Boundary contact,
  overlapping control bounds and arithmetic refusal keep this proof unavailable.
  Its full work bound plus the original partial-proof preparation minimum must
  fit before attempting it; capacities, control inspections, conversions and
  winding operations are charged without refunds. Low-budget partial-proof
  behavior remains authoritative.
  For a query point, a control hull that avoids it permits a chord homotopy that
  also avoids it. Checked exact dyadic integers, bounded subdivision and winding
  or parity establish individual point membership. One strict inside and one
  strict outside point in the glyph box certify `PartiallyOutside`. Equal or
  missing witnesses never certify whole-box visibility or absence. The relative
  witness positions are generated from the full intersection of source control
  bounds and the glyph box; generation and storage are prepaid even when a
  sufficient certificate stops the search early. Every visited edge, orientation
  and subdivision is additionally charged under the same shared ceiling.
  Overflow and unsupported geometry retain uncertainty; actual active work
  exhaustion remains a resource-limit error. Passive drawing queries keep their
  previous uncertainty. This does not rasterize or replace the clipping set with
  a polygon, and does not resolve opacity or overpainting.
  Straight-edged clips (`m`, `l`, `h`, `re`) with any vertex count and either
  fill rule additionally use a conservative polygon classifier. A glyph is
  Outside only when every boundary test proves separation, Inside only when all
  four corners are strictly inside with no edge contact, and otherwise
  PartiallyOutside. Boundary contact, orientation uncertainty and an explicit
  tolerance margin all fall back to PartiallyOutside. Vertex/edge work is
  charged per glyph; an unaffordable polygon proof downgrades to the sound
  bounding box instead of excluding text. Curved boundaries are flattened
  conservatively per glyph with a chord tolerance of 1/64 of the glyph box's
  smaller side, using the control-point chord-distance bound and bounded
  de Casteljau subdivision; the same polygon test then runs with the error
  margin widened by the tolerance. Depth/segment exhaustion or non-finite
  arithmetic keeps the bounding-box fallback.
  Every remaining unsupported path retains the outward control-point bound of
  all its line and Bezier control points in page coordinates, intersected with
  the previous clip bound. Path bounds are reset with each completed path and
  restored with the graphics state, so nested saves and Form matrices keep the
  correct bound. A glyph strictly outside that bound can only be hidden and is
  classified `Outside`; overlapping glyphs keep the unsupported outcome.
  Strict clipping provenance survives graphics saves and matrix changes;
  legacy approximate rectangle matches retain their behavior but cannot
  authorize a new concave proof or intersection. All prior constraints must
  carry the strict witness. Existing operation fees are retained; new proofs
  and per-glyph classification use the page's clipping work budget. Each page
  starts with the full budget, so its classification does not depend on how much
  work earlier pages consumed; the document total is bounded by the page count
  times the per-page limit.
- Fonts: Type 1 / Type1C / MMType1, TrueType, axis-aligned Type 3, and Type 0
  with Identity-H, bounded Identity-V, and UniJIS-UTF16-H (Adobe-Japan1).
- Text mapping: ToUnicode, Standard / WinAnsi / MacRoman encodings with
  `Differences`, canonical Standard 14 identities, full-domain identity CMaps,
  and embedded Type1 programs' cleartext built-in encodings resolved through
  the Adobe Glyph List. With `predefined-cid-unicode`, Adobe Japan1/GB1/CNS1/Korea1 UCS2
  collection tables map known CIDs to Unicode; ToUnicode keeps priority and
  unknown CIDs stay placeholders. Glyphs without a Unicode mapping stay
  comparable through stable embedded-font, Type 3 CharProc, or caller-asserted
  font identities.
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

### Token stream, ordering, and exit selection

The flow engine keeps only comparison-visible glyphs, flattens them into
words, single punctuation tokens, and one token per CJK scalar, and drops
whitespace. Line and block boundaries become token metadata rather than
constraints on the diff.

The compact flow native report is the default. `--compact-json` names it
explicitly and produces byte-identical output. `--full-json` adds
`token_sources` (one source-glyph list per token with decoded text, raw code,
geometry, render order, render mode, font id/size, and content-stream
provenance) and `glyph_accounting` (every extracted glyph is either referenced
by exactly one token or listed with a furniture, whitespace, invisible, or
shared-source exclusion). Source lists are retained only for the full report;
the default token stream carries none. `--trace-json` writes a separate
`pdfdelta-flow-trace/1` report with bounded metrics for input reading, parse,
glyph extraction, tokenization/normalization, furniture/order handling, flow
diff, assessment/formatting, channel comparison when selected, and report.
Phase failures carry a `pdfdelta_flow::FlowError` class (`backend`,
`unsupported`, `unresolved`, or `limit_exceeded`) and limit details instead of
being classified from message wording. The shared-evidence channel phase uses
the same type for input reads, native acquisition, graph construction,
comparison, review publication, and bundle budgets. `duration_us` is the only
wall-clock field; it, the per-document render CPU measurements in
`render_documents`, and the per-call image worker CPU in `image_worker_calls`
are documented as nondeterministic.

The native schema-13 report contains only what the flow engine computed: old
and new token counts, extraction completeness and issues, content, formatting,
and uncertain change counts, the verified change script, formatting evidence,
reading-order evidence, and layout-only counts. It has no top-level assessment, candidate,
proven-region, unresolved-region, `not_produced_by_engine`, or alignment
coverage positions. Each change carries its own `confidence` and `ambiguity`
reasons. Absence records that the phase does not exist, not that a search
completed and found nothing. Stored form-field text still produces `text_mask`
for a changed local value pair.

Layout reconstruction can merge adjacent form rows into one oversized line.
The engine therefore builds three reading-order hypotheses per page from the
same glyphs: reconstructed layout order, content-stream paint order, and
baseline-row order. Baseline rows use only each glyph's baseline coordinate
along the text direction's normal; row gaps are relative to font size, so tall
overlapping bounding boxes on tight CJK rows do not merge. Rows run
top-to-bottom (right-to-left for vertical text) and glyphs follow their
baseline advance direction. A page is kept on layout order when layout and
paint order have at least 0.9 order-preserving overlap. Otherwise the page is
matched to its counterpart pages in the other document by an order-free,
information-weighted bag overlap of page tokens: the best match and every match
within half of its overlap are kept, plus the pages adjacent to the best match.
The three hypotheses are scored by the weighted bigrams and trigrams they share
with those counterparts' three hypotheses, using a weight table built from the
counterpart tokens alone. A page whose best overlap is below 30% of the
document pair's median per-page best overlap has no counterpart and is ordered
by intrinsic spatial coherence only, so an inserted or deleted page cannot
borrow evidence from a structurally similar neighbour, and editing an unrelated
page cannot change an untouched page's evidence. Layout order is kept only when
it has at least as much evidence and at least as much spatial coherence as
paint order; baseline-row order replaces paint order only when it has more
evidence and at least as much coherence. Otherwise paint order wins; exact ties
keep layout, then paint order. Pages where layout and paint order disagree are
marked order-uncertain, and an intrinsic per-page
coherence score records how often consecutive chosen tokens are spatially
adjacent. Neither marker changes the exit code. The engine never consults an
external reader for this choice.

Alignment matches tokens by information weight, prefers long equal runs, and
promotes same-gap runs into the equal spine. After the weighted spine is final,
a bounded plain LCS fills equal tokens left inside two-sided gaps without
removing any weighted match. The script builder classifies the
remaining gaps as delete, insert, replace, or move, and treats a replacement
whose concatenated text is unchanged as layout-only. A delete run and an insert
run with identical text at opposite ends of one accepted move are the remainder
of a rotation, so they are reported as one additional move even when shorter
than the five-token minimum; unrelated short equal runs stay delete/insert.
The move detector requires a moved run to carry information weight: a run of
minimum-weight punctuation or separators contributes nothing and cannot be a
move. A same-page move that sits on a dot-leader contents page and spans only
low-weight or numeric changes is promoted back into the spine, so a changed
table-of-contents page number becomes one small replacement instead of moves of
the heading and the dot leaders. The engine evaluates the permissive rule, the
information-weight rule, and the contents rule for both alignment strategies and
keeps the script with the fewest non-move changed tokens. The rules differ only
in how they rewrite the gaps of one alignment, so all three rules of a strategy
share one memoized alignment for the same promoted pair set. A rule is skipped
when it provably cannot change the script (no move candidate exists, or neither
document has a dot-leader page), the same promoted pair set reuses its
alignment and its residual-gap refinement, and a promotion round reuses the
alignment of every segment that the new forced pairs do not split. The selection
keeps the highest rule rank among identical scripts, so the chosen script and
the tie-breaking are the same as a full six-variant evaluation. A
post-pass reconciles equal tokens that sit on opposite sides of a move or of a
shifted spine run:
they become implicit equal tokens, and the shifted run becomes one move, which
removes needless delete/insert pairs from rotations and page swaps. A second
post-pass chooses between the two valid explanations of an adjacent equal-run
swap. It reports the alternative move when that run intersects fewer
reconstructed blocks of the old document and carries at least as much
information as the reported run. A paragraph that moved past a heading and its
paragraph is therefore reported as the paragraph move, while a short heading or
form label is never preferred as the moved run and an equal-span swap keeps the
alignment choice. Every rewritten script is replayed before it is accepted.
After the script is selected, a final pass recognizes reading-order
transpositions. When a move and the equal run it swapped with stay in disjoint
visual columns, keep the same left-to-right order, and share a visual row band
in both documents, the move is reported as `reading-order` instead of a
content move. Line extents come from token bounds and layout line breaks;
wrapped cell lines are added above and below within the run's column so a
multi-line table cell supplies the row band. A move across rows or pages, a
move whose runs overlap horizontally, a transposition whose visual order also
changed, and a run without complete bounds stay moves. The classification is
geometry evidence, not author intent: two words swapped on one line satisfy
the same test, and the report says so. A deletion, insertion,
replacement, or move selects a detected difference (exit `1`); a reading-order
or layout-only change does not. Each change also carries a cheap confidence and ambiguity
marker: coarse spans, repeated boundary runs within the same block, short
moves, numeric-only spans, numeric table neighbours, and order-uncertain pages
lower the confidence without changing the script or the exit code. Matched
tokens also carry formatting-only evidence for normalization (raw text or its
normalization-event mask differs, including an in-block line-wrap rewrite),
font size (relative one-percent tolerance), position (the tested segment's
internal line and page break offsets agree and its lines are uniform baseline
translations),
line and page breaks (internal break offsets differ), and block structure (the
block-boundary pattern differs). A break after the last token of an old block is
a group boundary rather than internal evidence, so a page break between blocks
is not reported as a reflow. A run that moved from one page to another with an
unchanged internal layout is position evidence, not a suppressed run. Adjacent
entries with the same reasons, sizes, and pages are coalesced into one
presentation entry. A formatting entry must span at least one matched
adjacency: a one-token LCS match inside rewritten text is a point, not a
formatting span, and is dropped. Font size is reported once per distinct changed
size pair over the whole matched run instead of one entry per changed token.
Formatting entries are
attached to matched runs, never select an exit code, and never change the
content script. Repeated
headers, footers,
and page numbers are removed as furniture and diffed separately by
digit-masked text. Furniture membership is decided jointly for the two
documents of a comparison. A line that qualifies on either side is furniture on
both: alphabetic running headers and titles are removed at every line-level
occurrence (this keeps a page merge or break invariant when the line is no
longer at the new page edge), while numeric-only lines use the joined
relative-position cluster. Within one furniture family, digit runs that change
between occurrences (page numbers, counters) are masked to `0`; a run that is
constant in every occurrence (a year, form number, or version) stays literal,
so editing it is reported as a furniture change instead of disappearing behind
the mask. Families are separated by their digit-run lengths so a page number
and a form code that share one full mask are not conflated.

Each side selects one relative-position cluster per masked line. The joint
comparison removes the union of both sides' selected clusters from both
documents, and an alphabetic running line is removed at every line-level
occurrence. Taking the union rather than re-clustering each side independently
matters when a page insertion or deletion changes which cluster meets the page
threshold: the side that no longer qualifies still removes the same lines as
the side that does, so an untouched page cannot become content on one side
only.

A page split by a vertical gap much larger than its line pitch can contain two
independent page-like regions. This is what a page merge produces, and it also
happens on real pages with a wide band of whitespace. Such a page is
partitioned into stacked bands when each band holds at least a fifth of the
page's tokens and at least two rows. Each band's information-weighted token bag
is compared with the whole pages of the other document; a band that reproduces
one page at 0.9 overlap or more adopts that page's chosen order, and the page is
assembled band by band. The two bands of a merged page can therefore keep two
different hypotheses, which is exactly what the two separate pages chose.
Furniture positions are measured within each band, so a header or footer that
the merge moved into the middle of the taller page keeps the relative position
it had at the old page edge. Pages formed from one band, and bands whose
counterpart is itself stacked, keep the evidence-based whole-page choice.

A comparison whose extraction limits were raised does not build both sides'
reading-order hypotheses in one process. Each side is extracted and
reconstructed in a child process, which writes its three candidate streams to a
private per-run directory and exits; the parent then loads both compact streams
and performs order selection and alignment. This keeps the second extraction
from overlapping the first side's normalization buffers. The cache is binary and
versioned, carries token counts and per-token text lengths, and ends with an
FNV-1a checksum over the payload. The parent validates the magic, version,
lengths, and checksum; a missing, truncated, or corrupt cache, or a child that
exits non-zero or is killed, aborts the comparison with exit `2` instead of
reporting a partial result. Send text, page ids, bounding boxes, block indices,
and glyph conservation are written exactly, so the cached and in-process paths
produce byte-identical reports.

Image-only pages first compare an encoded identity: the same ordered image
XObjects, byte-identical encoded streams, pixel-affecting dictionaries and
CTMs. Equal encoded identities resolve the page without decoding and become one
synthetic `⟦image-encoded:<hash>⟧` token. Different encoded bytes prove nothing,
so the page falls back to a complete decoded pixel inventory and a
`⟦image:<hash>⟧` token; the report names the resolving rule. A one-sided
image-only page is a deletion or insertion once its identity is available.
Pages with too many or too large images remain incomplete without OCR or a
partial inventory.
Unsupported clipping paths no longer discard their glyphs: the glyph is retained
with a conservative partial-clip status, its tokens are marked uncertain, and a
page-scoped extraction gap keeps the page incomplete. When a simple font has no
`/Encoding` and no `ToUnicode` entry for a code, the extractor reads the font
program's own encoding: a cleartext Type1 program's `/Encoding ... def`
section, or a Type1C program's CFF Encoding and charset (the code selects a
glyph id, the charset names it). For a non-embedded standard-14 Symbol or
ZapfDingbats font (recognized by `BaseFont` after stripping a subset tag) with
no embedded program, the ISO 32000-1 Annex D.5/D.6 built-in encoding supplies
the code-to-name mapping; Symbol names resolve through the Adobe Glyph List and
ZapfDingbats `aNN` names through the Adobe ZapfDingbats glyph list. The same
`aNN` list also resolves a Dingbats subset's own `/Encoding` names. Glyph names
go through the Adobe Glyph List, then the `uni`/`u` algorithmic forms, then a
bundled Apache-2.0 pdf.js supplement for TeX/AMS names that the first two rules
do not resolve (for example `parenleftbig`, `summationtext`, `arrowhookleft`,
and `prime`). The supplement omits private-use and control values, so it never
turns a placeholder into non-text code points. A project-authored MIT table
then resolves the remaining unambiguous TeX names (`lessmuch`, `greatermuch`,
`floorleft`/`floorright`, `ceilingleft`/`ceilingright`, `asciigrave`).
Combining overlays and enclosures (`negationslash`, `arrowrightabove`,
`circleencl`, `circleslashencl`, `diamondencl`), script names, and generic
`gNN`/`glyphNNN` names deliberately stay placeholders because their meaning is
not one standalone character. A name outside all of these rules, an unparsable
program, or a CFF2/OpenType program keeps the glyph an uncertain placeholder.

An unmapped glyph still needs a stable identity, because two revisions often
re-subset the same font and change its program hash. Identity is chosen in this
order:

1. **Name.** The subset-stripped `BaseFont` and the exact glyph name from the
   PDF `/Encoding` (including `/Differences`), the cleartext Type1 program's
   `/Encoding`, or the Type1C charset. Names that none of the name rules above
   resolve stay placeholders, but equal names in the same family compare equal
   across subsets. The name is hashed together with the family and never becomes
   Unicode text.
2. **Outline.** A CID-keyed CFF program's `CharStrings` entry, keyed by the
   CID that the Identity encoding selects. This fallback is implemented only
   for CID-keyed CFF programs; simple Type1, Type1C, and TrueType outlines are
   not read. For an Identity-encoded composite font, CID 0 is the
   specification's `.notdef` glyph, so it uses the name identity rather than an
   empty outline. An empty outline is never used as evidence.
3. **Program.** The embedded program hash plus the glyph id remains the final
   fallback when no name or non-empty outline is available.

The identity hash's first byte tags the evidence (`program`, `name`, or
`outline`) so the source is visible without resolving a font again. Text
placeholders print the tag (`<unmapped:name:…>`), the native full report's
token sources carry an `identity` field, and flow tokens use the form
`\uFFFC<kind>:<hash>:<glyph_id>`. `glyph_name_identity_pages` lists pages whose
unmapped glyphs use name identity.

A resubset metamorphic case appends harmless trailing bytes to every embedded
font program and requires an empty diff. Name and outline identities ignore
those bytes; a document that still has program-identified placeholders is
skipped because the program bytes are then real identity evidence.

Every returned script is replayed against both token sequences before it is
used: every token must be covered exactly once, unmoved text must be equal,
move and reading-order endpoints must be equal, and layout-only changes must
preserve text.
Per-page glyph conservation is reported alongside extraction issues, so a
missing glyph is never silently treated as an empty token.

Flow text completeness is both sides extraction-complete, glyph conservation
complete, and equal non-text paint inventories. Every painted path, image,
inline image, and shading contributes a digest of its kind, exact geometry or
image-data identity, and page-space placement. Page numbers are excluded so an
identical page shifted within the document compares equal; page crop and
rotation are included. Artifact-marked paint is included. If the two multisets
differ, or an identity is unavailable, the text channel is incomplete and
reports `non-text paint differs; text drawn as paths or images was not
examined`, with bounded affected-page lists; detected changes remain visible.
This deliberately covers table rules and boxes: the engine cannot prove that a
changed path is not outlined text without OCR or text-shape recognition.
Visual, forms, and relations still use their own channel coverage, so a
complete text stream does not claim that those channels are complete.

Exit selection combines per-channel verdicts. A selected channel that detected
a typed difference (flow text delete/insert/replace/move, a value or pixel
operation, a changed relationship, or a keyed element operation) selects exit
`1` when its own evidence is not broken, even if another channel is broken or
was not examined. Broken evidence blocks only its own channel: text extraction
and glyph conservation for text, forms reasons for forms, relation graph and
work-budget failures for relations, and a visual backend failure for visual.
Cross-channel and unknown reasons block every channel. With no intact
detection, any broken reason selects exit `3` with `broken_evidence`; otherwise
any unexamined reason does; exit `0` requires every selected channel to be
complete. `comparison_complete` remains `false` whenever any reason exists and
`exit_basis` records `detected_with_unexamined_evidence` when an intact
detection coexists with incomplete evidence. Relation binding, membership, and
endpoint gaps are unexamined relationship evidence; relation graph and
work-budget failures are broken. Structure-ID key presence belongs to
relations/structure, and field-name key presence to forms. A key-presence
obligation records why a key claim could not be established (a missing
inventory, unnamed or duplicate members, a retained rename alternative, or an
unavailable acquisition path), so it is unexamined evidence on its channel. An
exhausted raw key search is cross-channel broken. An exhausted scoped key
search, or a scoped work-budget obligation, is broken on each channel that
owns a claim in the search (forms for field names, relations/structure for
structure IDs); other scoped obligations are unexamined on their claim's
domain.

The static HTML review publishes its source index as a gzip member inside the
artifact's gzip member (`manifest.json` version 3) because the second pass keeps
the large index substantially smaller. The compatibility reader accepts the
single-stream version-3 shape, version-2 double-gzip bundles, and historical
uncompressed `sources.json` files. The renderer preflights each page's image
XObjects and inline images: one image above 64M declared pixels, including
`BI ... /W ... /H ... ID ... EI`, is reported as a page-scoped `resource_limit`
instead of allowing the renderer's full-size RGBA conversion to abort the
child. The XObject preflight reads only image dictionaries; inline scanning uses
the content parser and never decodes image bytes.

Exit `0` is not reachable with the default channels for a document with pages
yet. Composited page rasters are review context and are not compared, so every
rendered page leaves the visual inventory incomplete. Native relation endpoints
and semantic relationships are not established for a tagged document, so
relations stays unexamined; a pair where both inputs prove no relation source
has a complete empty relations inventory. An identical or purely reflowed
document therefore exits `3` with
`unexamined_evidence` rather than `0`. `--channels text`, `--channels
text,forms`, and `--native-text-only` select the channels whose verdict is
established and exit `0` when those channels are complete and unchanged.

Text views remain correspondence context for relationships and structure, so
their accepted correspondences still map relation endpoints. Their local
comparisons are not exit evidence: an unselected text channel cannot justify
exit `1`, and text-only ownership reasons cannot keep `comparison_complete`
false or select exit `3`. Non-text components keep every obligation; a
component that mixes text and non-text sources is never superseded. A text
extraction issue about an image-only page remains unexamined text evidence:
flow's paint gate covers changed or unavailable paint, but an unchanged image
can still contain text the token stream cannot see. `flow_owned_text` (and the
same record at `comparison.flow_owned_text`) counts shared-evidence text
components and local comparisons that flow ownership replaced, with an
`unchecked_obligations` count per skipped reason. It is absent when the shared
search recorded no such position; absence never means a completed text search.

### Text coverage

Image invocations, inline images, painted paths, and shadings leave their pages'
text inventories incomplete, because they may contain or hide text that native
glyphs do not describe. The marker records the render-order boundary of the
last non-text paint so earlier glyphs cannot prove visible-text coverage.
Unused image resources, unpainted paths, and clipping alone do not create
markers.

## Shared evidence for non-text channels

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
diff of native text, and composited-page proposals are disabled. A retained
page raster is therefore a discovered visual source that no comparison
consumes, so every rendered page leaves the visual inventory incomplete.
Scanned and image-only pages keep their pixels for review, but their text is
not recognized, so text coverage for them stays incomplete.

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
real documents. When both catalogs were read and provably have no native
relation source, the empty inventory is complete and no graph edge is treated
as a declared relationship; a pair with a structure source on either side keeps
the incomplete/unexamined behavior.

## Process isolation and resource limits

Every PDF is untrusted input.

- **Native acquisition** runs in bounded child processes: 2 GiB address
  space, 30 s CPU, 35 s parent deadline, 1 MiB request header, and 128 MiB
  response limits. Page metadata and form fields are acquired separately from
  glyphs and tags, so a content-extraction failure can leave forms and
  rendering usable. PDF input is capped at 256 MiB.
- **Page rendering** runs in a child process: a 2 GiB memory ceiling, 10 s of
  CPU per page (the deterministic page budget; child-reported median 0.032 s,
  p99 0.282 s, p99.9 1.383 s, and max 3.019 s over 4,654 rendered pages of the
  36-pair channel panel), a cumulative child-CPU budget of 120 s per input
  document (measured max 27.31 s and p99 27.31 s over 72 documents), 120 s wall
  per page and 600 s wall per document as safety stops, 8 million pixels per
  page, and 256 MiB retained RGB per input. The page that crosses the document
  CPU budget is retained; later pages are a `resource_limit` outcome with
  reason `document CPU budget exhausted`, never an empty raster. A page that
  exhausts the page CPU budget is also a resource-limit outcome with no raster;
  a wall safety stop is a distinct, explicitly load-dependent resource-limit
  reason. The document wall stop is five times its CPU budget, so at a 25% CPU
  quota the full CPU budget (about 480 wall seconds) still expires first.
  `--limit-scale` multiplies both CPU budgets and both wall stops.
- **Image decoding** runs in a child process: a 2 GiB memory ceiling, 60 s of
  CPU per worker call, a 300 s wall backstop, 10,000 occurrences, 16 million
  pixels per image, 64 million base-image pixels per input, and an 8 MiB
  response. The 36-pair panel has 56 successful document calls with median
  0.191 s, p99 13.909 s, and max 13.909 s; 16 calls that stop at an inventory
  limit before responding take 0.24-3.56 s. Sixty seconds is a 4.31x margin
  over the observed maximum. The CPU and wall budgets scale with
  `--limit-scale`; exhausting either is a typed `resource_limit` outcome, never
  a complete or empty inventory.

The parent validates input hashes, page identity, acquisition roles, and
aggregate limits before comparison. Passwords travel through private stdin and
never appear in responses. Bounded workers support Linux and macOS; on other
platforms the affected acquisition is reported as unsupported.

Linux enforces the worker memory ceiling with `RLIMIT_AS`. macOS accepts
`RLIMIT_AS` but does not enforce it, so worker subprocesses arm a counting
global allocator at the same 2 GiB limit. An allocation above it terminates the
worker with the limits exit code, which the parent maps to a typed
`resource_limit`; the comparison process never arms the allocator.

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
