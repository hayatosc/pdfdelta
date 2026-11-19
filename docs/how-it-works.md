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

Exact domain checks can retain a partial suffix in the original forward LCS
buffer. A reverse cell first consumes its own prefix value for the unchanged
uniqueness test, then overwrites that slot with its suffix value; later reverse
cells never need that prefix again. The original work preflight and
forward-plus-two-rows memory admission remain in force, while only one full
matrix is allocated. Clearing its border replaces rolling-row initialization.
An early ambiguous result is not a usable suffix or a correspondence proof.

Only an unchanged domain whose exact displacement check remains undecided can
complete the retained suffix for semantic traversal. The triggering cell has
not been completed and is charged again, together with every other unfinished
cell. The continuation lookup and both immutable slice bindings cost three
units, plus sixteen prepaid metadata units for checked accounting; a
continuation with fewer than nineteen paid cells takes the ordinary path instead.
A whole-block displacement attempt releases the retained table before it can
allocate its own proof matrix. The ordinary semantic memory preflight,
source-signature callback, path and
canonicalization charges, traversal order and first witness remain unchanged.
Unused continuations are dropped, and a refused completion cannot publish a
partial table as a proof.

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

Column-band adjacency checks reuse immutable raw obstacle bounds within one
local discovery pass. Every source block remains in the page, emptiness and
obstacle checks, including blocks with source issues. Missing bounds still hold
a candidate, and closed intervals retain point obstacles. The cache is bound to
one input side, costs are charged before allocation and lookup, and each side
retains at most 32 MiB; refused preparation falls back to the same uncached rule.

Column-band checks can retain a complete page population for one immutable
source side. A source walk keeps every block index in original order whose
page list is empty or contains the selected page; opaque, empty and multi-page
blocks remain available to the original vetoes. Only blocks known to belong
to other pages are omitted. The last page is replaced only after its complete
population is funded. Queries, reserved slots, all page-membership walks,
copied indices and subsequent population walks spend work. Retained and
construction buffers with the header are capped together at 16 MiB per side;
refused preparation keeps the full source population without refunds.

Whole-block correspondence collection and stationary masks can group one call's
immutable ownership intervals by source block. Setup and complete queried groups
are paid from the same shared work budget; each temporary index, including its
construction arrays, is capped at 32 MiB. Every original interval and duplicate
is retained. Containment still requires one original owner, adjoining intervals
are never merged, and overlap keeps strict endpoints. Refused setup uses the
original population scan; exhausted queries retain the whole-pass hold.

Stationary, raw-source and positioned-replacement discovery share a lazy cache
of deny-only source positions. It borrows the comparison's immutable sides,
retains known and unknown entries, and never stores an incomplete scan. Every
view, reference and competing occurrence is rebuilt with the same population;
the current candidate mask and ownership are checked afresh. Cached positions
can only reject an occurrence, never prove equality or release an issue. Queries
and view population copies spend the shared work budget. Retained buffers and
slot headers are capped at 32 MiB across both sides; unfunded setup or excess
capacity keeps the original scan, and already spent work is never refunded.

Established-reference boundary queries within one translation discovery reuse
only completed geometry from that discovery's immutable side, views and
reference population. Known and unknown bounds are kept separately; exhaustion
is never cached. Every reference, page, support, order and ownership check still
runs, and cached geometry alone cannot certify correspondence. Setup pays for
initialized slots and every query spends work. Retained slots and headers are
capped at 16 MiB per side, 32 MiB combined. Refused or unfunded setup keeps the
original scan without refunding spent work.

Repeated horizontal-direction queries reuse completed outcomes for one
discovery's immutable source side and view members. The query checks the exact
side, view, range and block binding before reuse; a different binding runs the
original scan. Missing or non-horizontal evidence still vetoes adjacency, and
exhaustion is never cached. Every band, obstacle, support and ownership check
still runs. The view walk, initialized offsets and member slots, and every
query spend the shared work budget. Actual capacities and headers are capped
at 16 MiB per side, 32 MiB combined, with no refunds on refused preparation.

Raw-source discovery validates a block against itself before using its evidence.
A self-comparison with unmapped scalars holds after the fixed header charge,
before scanning text or analyzing sources. Identity supports this rejection
only; it never certifies equality. Every other call retains its upfront charges,
final source and position comparison, and document-wide sharing certificates.
When both references identify the same immutable whole block, a completed
source analysis is reused for the second side. Distinct blocks require
independent analyses. Malformed or unknown evidence still holds the pair, and
exhaustion cannot produce a partial proof.

Raw-source analysis holds synthetic spacing atoms after checking positions and
both complete source-map shapes, before allocating and scanning text buffers.
All upfront charges and source-map walks remain paid. This early hold cannot
certify a correspondence: supported projections still require the complete
text, topology, normalization and sharing proof.

A final optional pass can report a nonowning changed region inside a partly
resolved, complete source correspondence. Two consecutive equal-token pairs
must occur on every maximum matching of the parent; the reported interior
excludes those endpoints. Its latest source ranges must remain wholly
unresolved, and a nonwhitespace token multiset mismatch must have literal raw
glyph evidence. The correspondence metadata records
`mandatory_matching_boundaries`, which establishes the containing range and
leaves individual edit locations ambiguous. It assigns no ownership, removes
no candidates, and cannot make a comparison complete.

This pass borrows canonical tokens rather than copying formatting or font
payloads. Its live arrays, matching analysis, source checks and newly retained
output payloads are bounded and paid before allocation or traversal. It runs
after the existing recovery and review passes, so it uses only their remaining
shared budget.

Before repeating matching analysis, this pass pays a cached-header check and
skips a complete, unique literal script with no edits. Such a script cannot
contain a substantively changed gap. Empty cached edits without strict literal
uniqueness still require analysis; semantic event invariance is insufficient.
The skipped work remains available to later source equality under the same cap.

A further optional tail can resolve paired literal equalities inside a two-sided
whole single-block changed region. Its direct parent and every actual ancestor
must remain complete, clean correspondences. Every selected nonwhitespace pair
must occur on every optimal matching of that original parent; both sides must
still be unresolved and outside candidate or other coarse ranges. Singleton
canonical and raw glyph sources, contiguous raw cuts, absence of glyph reuse,
normalization checks and a shared page supply the literal evidence. This
separately named source proof uses the mandatory parent pairing rather than
requiring equal positions; existing positioned proofs retain their geometry
checks. Synthetic whitespace is left unowned.

After both ordinary maximal-run and singleton passes, a separate page-shift
route can use an isolated, complete exact whole single-block parent with no
ancestor dependency. Both blocks must each belong to one different page and
match the same whole coarse scope. The original full-parent mandatory matching
supplies the correspondence; a separately named source guard retains canonical
and raw literal cuts, normalization and document-wide sharing checks while
omitting only equality of the numeric page ordinals. Children carry an explicit
page-shift assumption. The validator permits their coexistence with that exact
whole coarse proof only under the isolated-parent premise. Earlier proofs keep
their priority, and all passes share the same remaining budget, source cache and
publication capacity ledger.

After earlier proof consumers, an optional common refinement can also handle
different block splits of an already established exact equal multi-block root.
Both complete literal sequences must be identical, nonempty and at most 512
tokens. Their full-length increasing matching is necessarily the diagonal;
no independent block matching or choice between edit paths is made. All
nonwhitespace tokens first pass the existing singleton canonical/raw glyph,
contiguous raw cut, normalization and document-wide sharing checks. A private
certificate binds that completed result to the original parent and both
immutable sides. Failure on any run prevents a partial parent certificate.

The diagonal is then split at either side's original block boundaries, gaps
and whitespace. Each side of the parent must remain on one page; different
page ordinals use the parent's correspondence and are explicitly reported as
`refined_exact_parent_equality`. Every published run must still be unresolved
on both sides and clear of every candidate, coarse region and existing owner.
This rule grants no coarse exception. The ordinary paid, atomic publication
path retains output and live-memory checks; the validator checks the recorded
child's identical diagonal offsets in the complete parent. Neither the marker
nor validation independently establishes the parent correspondence or extracts
the PDF. Whitespace and unselected runs retain their previous status.
If the preceding initial-component pass appended relations, this refinement is
skipped: reviewed domains are released before the original late evidence
capture. Retaining new proof inputs therefore cannot consume that older pass's
memory allowance or displace its receipts.

Those four equality passes may reuse a completed mandatory matching analysis
for the same immutable domain and sides. The optional table is funded only
after a parent's original attempts, and its setup must be small relative to
both the completed analysis and the remaining budget. Actual table and pair
capacities count toward a 16 MiB cache bound and the enclosing live-memory
ledger. Every lookup and publication is paid; refused preparation keeps the
fresh-analysis path, and failed or exhausted analyses are never cached. Source
proofs, parent guards, candidates, partitions and ownership are checked again
for every run. Cached matching pairs alone never establish equality.

The same tail may prepare a paid table of immutable candidate and coarse-region
singleton span headers. It binds both side objects and the exact candidate and
region slices, validates scalar bounds before retaining a header, and retains
complex or invalid spans for the original projection predicate. Each query
excludes only its own coarse region and pays every entry visit. This table
never retains source proofs or mutable ownership decisions. Setup is limited
to one sixty-fourth of the remaining work; actual capacities count toward both
a 64 KiB table cap and the existing live-memory ledger. Refused preparation
keeps the original path without refunding work already spent.

When additional setup and capacity fit those same limits, validated singleton
headers may be linked by source block. Queries visit the selected block and a
global list containing every deferred or invalid header; unrelated valid
headers need no repeated visit. Both link arrays and block-head capacities
remain in the live ledger. Setup and each visited link are paid, and refusal
keeps the linear header table. Source and ownership guards still run freshly.

After validating the containing scope and memory envelope, equality publication
checks the raw-source proof before constructing partition snapshots or reserving
record and ownership arrays. A held source proof leaves those arrays untouched.
Successful proofs still prepare both complete partition snapshots before adopting
either side. For sufficiently large partitions, the replacement interval also
counts its unresolved children, avoiding two full population scans on each
side. Only one unique unresolved interval may be replaced; every other state
and projection is copied exactly. The local counters, route selection and
additional live metadata are paid explicitly. Small partitions retain the
population scans. Future diagnostic copies, source validation, ownership and
both-sided publication remain fully reserved and checked.

Final unresolved fragments reuse only the most recently emitted block's
alignment reasons on the current side. They keep first-occurrence order and
deduplication; a different block clears the buffer, and a later revisit may
scan again. This retains one buffer rather than a document-wide table. The
equality tail prepays at most one complete reason scan per unresolved partition,
every possible fragment's lookup and owned reason copy, and the buffer's peak
capacity. Additional partitions created by equality publication pay those
costs before either side's source verdict is adopted. Output limits and all
original source, partition and ownership checks still apply.

The whole-region multiset mismatch and its review obligations remain unchanged:
subtracting identical pairs present in every original optimal matching preserves
the multiset difference and changed-token counts. The tail keeps the original
parent arrays and never recomputes a matching on compressed residual text.
Only independently certified paired subruns gain equal ownership; candidates,
coarse proofs and ambiguous edit locations remain. It uses only the remaining
shared budget and prepares both sides' publication before adopting either.

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

## Flow engine

`pdfdelta flow` is a separate, token-level view built from the same extracted
glyph evidence. It keeps only comparison-visible glyphs, flattens them into
words, single punctuation tokens, and one token per CJK scalar, and drops
whitespace. Line and block boundaries become token metadata rather than
constraints on the diff.

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
removes needless delete/insert pairs from rotations and page swaps. Every
rewritten script is replayed before it is accepted. Repeated headers, footers,
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
placeholders print the tag (`<unmapped:name:…>`), native JSON `unmapped_tokens`
carry an `identity` field, and flow tokens use the form
`\uFFFC<kind>:<hash>:<glyph_id>`. `glyph_name_identity_pages` lists pages whose
unmapped glyphs use name identity.

A resubset metamorphic case appends harmless trailing bytes to every embedded
font program and requires an empty diff. Name and outline identities ignore
those bytes; a document that still has program-identified placeholders is
skipped because the program bytes are then real identity evidence.

Every returned script is replayed against both token sequences before it is
used: every token must be covered exactly once, unmoved text must be equal,
move endpoints must be equal, and layout-only changes must preserve text.
Per-page glyph conservation is reported alongside extraction issues, so a
missing glyph is never silently treated as an empty token. Flow output is
intended for review and evaluation; the channel comparison remains the primary
completeness report.

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

For the page-shift publication route, record and ownership arrays use explicit,
checked geometric capacity requests. The memory allowance includes transient
old and requested new backing arrays; a larger request falls back to the minimum
when it would exceed that allowance. Old-array movement is charged when growth
is required. New record and ownership headers and the bounded capacity metadata
checks are paid for every publication. Actual capacities are checked after
reservation and before publishing. Both same-page and page-shift attempts check
the source proof first; a source refusal reserves no publication arrays. Any
capacity retained by a later preparation failure remains in subsequent plans.


Anchored translation and each bracketed discovery round reuse the comparison's
existing bounded deny-token metadata cache while rebuilding their views. The
cache retains only completed immutable source metadata, including held values;
it supplies negative occurrence evidence and cannot establish a correspondence.
Every query, slot initialization and output metadata copy stays charged. Failed
or refused cache setup keeps the original scan, with no work refunds. The
existing combined 32 MiB cache cap and source-side binding remain unchanged.
All established references, source populations, closure guards and discovery
order are inspected as before; proof results and mutable ownership are not
cached.


Literal source proofs obtain canonical scalar counts from the same immutable
side's materialized tokens, subtracting its validated unmapped-token count.
Each pair of table lookups and the checked subtraction is paid before use.
This replaces repeated Unicode text counting without storing a proof or
allocating another cache. Raw text counts, source-map scans and every source,
position, normalization, sharing, ownership and parent guard remain required.

A single source block without a separator or unmapped token has the same
scalar and comparable coordinates. For containers of at least sixteen tokens,
source proofs can pay the block lookup and sixteen metadata units, then validate
both coordinates and return an interval on the stack. This avoids the general
projection's allocation, duplicate tracking, offsets and endpoint searches.
The size check selects an equivalent implementation rather than a matching
criterion. Small, grouped, separator or unmapped cases keep the original
projection and its total fee; no individual projection fee increases. Every
physical-source, raw, position, normalization, sharing and ownership guard still
runs. Nothing is cached, and shared budgets and output limits remain fixed.

Equal-fragment literal validation borrows the selected canonical tokens from
the immutable side after prepaying and checking every scalar. Removing the
copied character array also removes its copy charge; scalar validation,
cross-side literal comparisons and raw-source comparisons remain charged.
Every source-map, normalization, sharing, page, position and ownership guard
still runs in the original order. A non-scalar or unpaid selection returns no
partial evidence.

The optional coarse equality tail can retain validated source intervals for
joined literal veto spans of two to sixty-four blocks. It pays the complete
coordinate and duplicate-source validation once, then pays each overlap scan
of those immutable intervals. The table remains bound to the exact source
sides and candidate/changed-region slices; only the exact current region is
excluded. Invalid, unmapped, larger or unfunded spans keep the original
predicate. Cache construction shares the existing work allowance and memory
limit, including retained vector capacity and bounded projection scratch.
No ownership verdict, source-equality proof or candidate exclusion is cached.

Suffix continuation keeps the original full-matrix semantic admission and
search allowance. A separate `suffix_reuse_work` receipt reports actual
recomputed interior cells, binding work and accounting work, plus unused
allowance withheld from
later searches. Its allowance equals recomputed cells plus binding work plus accounting work plus
unused allowance. Already completed exact cells stay charged in the exact phase;
the trigger is recomputed and paid again. `work_used` continues to describe
consumed logical search allowance, including refused charges, rather than CPU
instructions. The optional receipt adds accounting only; source proof, all path
fees, first witness, decisions, and memory admission stay unchanged.

### Explicit scoped evidence and edit witnesses

A comparison can retain a literal token-multiset mismatch in
`assessment.scoped_change_evidence`. Each non-owning link refers to an
established, complete source correspondence in `assessment.relations`. Its
source scope may already contain known equal or changed tokens. It therefore
does not replace `proven_changed_regions`, does not classify the scope as
wholly unresolved, and does not authorize recovery or token ownership. Core
callers can read the same information through
`ComparisonAssessment::scoped_difference_evidence()` after validation.

`assessment.localized_edit_witnesses` refers to the established relation and
the contiguous published change indices. Atomic edit coordinates are relative
to that relation's old/new source spans. Empty boundaries include their actual
block-local comparable and canonical offsets; all representations at a
concatenated block boundary are retained. `strict_unique_in_declared_literal_model`
concerns only the declared literal token model. Otherwise the script is one
optimal witness, not a unique target location or an author's historical edits.
`origin_relations` identifies tentative physical claims discharged during
publication; it does not establish their original empty-anchor associations.

`assessment.evidence_link_work` reports additional prepaid planning,
verification, retention and output allowance consumed from the same shared
search budget. It is not a CPU-time measurement. A refused or unused
reservation is not refunded to make other processing appear cheaper.
