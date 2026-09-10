# pdfdelta comparison contract v5

The current application has no OCR provider or model-loading options. Recognition
contracts below describe evidence supplied by a provider, not an implemented
acquisition capability. Scanned text remains unresolved; rendered pixels remain
available for visual comparison.

## Purpose and status

Compare the content and relationships selected by the user independently of
whether a PDF represents them with text operators, raster images, vector paths,
structural tags, or interactive fields. An uninterpreted representation remains
evidence with an unresolved obligation; it never disappears from the scope.

This is the migration target, not a claim that every path already exists.
Interfaces and text-only tests do not establish completion. User documentation
and versioned evaluation reports distinguish working paths from unavailable
backends and unresolved interpretations.

## Comparison dimensions

Content operations include changed prose, numbers, negation, units, cell values,
field values, images, figures, and list items. Relationship operations include
heading membership, label/value association, row/column membership, meaningful
order, caption attachment, and footnote/reference targets.

Presentation records wrapping, pagination, coordinates, and font metrics
independently. Benign presentation differences do not count as default content
changes. Superscripts, subscripts, meaningful color, text direction, and other
interpretation-bearing properties are not silently erased as decoration.

The default channels are text, visual content, forms, and relationships.
Presentation can be selected explicitly and remains observable separately.
An explicitly requested text-only comparison states that scope in its report.
Text coverage never substitutes for document coverage. Every selected channel
has independent inventory, correspondence, and comparison obligations. A missing
provider result is unavailable, not empty. A complete empty inventory requires
a provider that actually inspected that channel and scope.

## Architecture and ownership

PDF bytes feed native extraction, isolated rendering, and structural acquisition.
These paths populate one immutable evidence store. Source-backed document graph
views feed candidate suppliers and a shared correspondence solver. Established
local domains feed text, value, relationship, and visual comparisons. The result
contains operations, exact masks, provenance, hypotheses, and unresolved evidence.

Structure and correspondence may refine each other locally. Refinement selects
views; it never rewrites original characters, numbers, images, or field values to
minimize a diff. Whole-document reading order and equal page numbers are not
universal premises. Local ordered prose remains an important special case.

The core stays pure: validation, graph views, correspondence, local comparison,
and report models. Existing `Document<Glyph>` calls remain a text adapter.
The CLI owns filesystem access, passwords, workers, limits, report publication,
and process status. The benchmark owns generated inputs, mutations, independent
annotations, manifests, and evaluation. Parser-library types remain behind
`PdfParser` and `ParsedPdf`; this migration adds no custom PDF object parser.

## Evidence and acquisition

Preserve original glyph text, raw codes, font identity, geometry, baseline and
direction, render order/mode, crop/clip observations, vectors, and PDF object,
content-stream, and operator provenance. Normalization is a reversible view
mapped to contributing source atoms. Unsupported filters, encrypted data,
unmapped glyphs, and incomplete streams are never successful empty extraction.

Source references distinguish native, rendered, and structured evidence.
Rendered references carry page/region geometry, a polygon, pixels or verifiable
image identity, and backend/version/profile. Structured references retain field,
tag, or annotation identity and source objects when available. OCR retains its
image references and recognition alternatives; it never receives invented native
glyph IDs. Native/OCR versions of the same visible material are equivalent or
competing views, not two independent contents. Record the basis of equivalence;
unproven overlap remains a conflict and cannot be consumed twice.

Native extraction identifies operators. Rendering observes a composited
appearance under a declared profile and cannot recover unknown operator
provenance. Raw image samples alone do not prove visibility after masking,
clipping, crop, transparency, overlap, or annotation rendering. Widget appearance
and stored form values are separate evidence. Report disagreements explicitly;
do not execute JavaScript or interactive actions to repair a value.

Use native text when its source and appearance are consistent, retain raw/font
identity for unknown codes, recognize image text locally when needed, and compare
figures/photos visually without inventing their semantic interpretation. Conflicts
request bounded additional analysis of the affected region. Mixed documents do
not require a document-wide choice between native extraction and OCR.

The implementation is Rust-only. Do not add Python components or external analysis
APIs. Isolate built-in Rust rendering in a bounded local child process. Missing capabilities remain
explicitly unimplemented. Core contracts retain headers, footers, figures, and
unknown source regions rather than treating omitted evidence as empty content.

## Document graph

Represent sections, paragraphs, lists/items, tables/rows/columns/cells,
forms/fields, figures/captions, annotations, code, mathematics, and unknown regions
as source-backed nodes or candidate views. Typed edges preserve containment,
local order, row/column identities, labels, captions, references, and alternatives.
Coordinates and text direction remain local to the evidence.

Native blocks are views, not indivisible ground truth. One-to-many, many-to-one,
split, merge, and move proposals refer to unchanged evidence. A subdivision must
account for its parent evidence without omission or overlapping ownership.
Reject dangling references, malformed geometry, and cycles in hierarchical
relations. Tags, geometric reconstruction, render order, and models have different
premises. A tag's existence is a source fact; its reading order or semantic role
may still be contested. A model score is not a proof certificate.

## Shared correspondence

Suppliers may use text, headings, typed identities, labels, row/column names,
neighbors, visual features, or embeddings. They do not emit independently
accepted changes. The shared solver checks source overlap, type compatibility,
parent/neighbor coherence, ownership, and split/merge accounting. Catalog/form
footer detection is one supplier, not a separate acceptance authority.

Search is hierarchical and locally bounded. Added covers, repagination, and
reordered regions can change page correspondence. An equal value in another row
cannot be selected merely to avoid a cell change: row and column identities are
part of the correspondence. Equal words under unrelated headings likewise do not
establish paragraph identity.

The current objective, `scoped_identity_then_literal_then_inferred_structure_v3`,
maximizes supplier weights lexicographically across source-backed scoped
identities, source-backed literal content, inferred structural correspondence,
inferred literal content, and other inferred proposals. Correspondences that
require selecting an alternative partition remain inferred. Within inference,
typed identities and retained structural context precede raw literal or similarity
matches, preserving membership when values repeat across rows. A key or local
order supplied by a model remains in the inferred class. This objective cannot prove
the interpretation of a key; all claims retain that correspondence premise.
Equal optimal solutions remain ambiguous, and truncated search emits no mandatory
correspondence for the affected component.
The solver separately records correspondences mandatory using source premises
alone. A source-backed proposal selected only after an inferred tie breaker is
itself reported as inferred. Source-only protection cannot depend on that tie
breaker; its verification shares the component's state budget.
Before applying the component-size cap, a bounded search may establish mandatory
correspondences in the highest remaining objective class. Only candidates
incompatible with those mandatory correspondences may then be removed. The
remaining search retains the forced ownership and partition constraints, and
all prefix and residual states share the same component state budget. Failure
to finish a prefix does not justify removing its rivals.

Keep alternative correspondences explicit. Record exhausted regions and candidate
families. Truncation cannot promote an apparently unique candidate into a proved
unique match. Charge enumeration, validation, scoring, and final checking,
including discarded work. Only conflicts request additional views/recognition;
revisits are bounded and record their new dependencies. A correspondence supports
local exact claims conditionally, not a proof of author editing history.

## Local comparisons and operations

Ordered text uses existing exact kernels. Literal claims quantify all optimal
insertion/deletion paths under the declared tokens and normalization. Count bounds,
mandatory positions, and complete editing witnesses are distinct. Positive bounds
with ambiguous positions mean changed but unlocalized. Equal retained residues
are required for a complete monotone editing witness; minimality is separate.

Whole-field/cell operations retain structural identity and old/new values.
Their review ranges are non-owning context. One date replacement may contain
several exact character hunks without owning intervening equal characters.
Relationships can change with an unchanged text/value multiset. Movement and
membership changes are not silently reduced to equality.

Compare visual regions under compatible rendering profiles. Pixel differences
are profile-dependent visual facts, not automatic semantic claims. Evaluate
reflow, scale, antialiasing, and producer effects separately from image/figure
content changes. Unknown visual meaning is never asserted as a changed word.

Keep all source-backed normalization alternatives unless an independent
certificate establishes their dependency. Do not choose the hypothesis producing
the smallest diff. Factorization requires independently established no-crossing
domains. Do not silently normalize numbers, negation, units, dates, or OCR confusions.

## Local failures and completeness

Each operation records its source and correspondence dependencies. Invalidate
dependent claims without arbitrarily invalidating unrelated regions. However,
missing evidence may conceal competing matches and invalidate global uniqueness,
absence, or deletion. Locality is not permission to ignore hidden competitors.

Distinguish fatal backend/input failure, unsupported interpretation, unresolved
evidence, resource termination, and cancellation. Independent valid results can
accompany incomplete comparison. Missing models/workers and omitted images do
not become empty success. Completion requires every selected channel's inventory
and comparison obligations to be discharged. Zero native characters plus
unexamined images is incomplete, as is unrepresented graph evidence. Missing
metrics are null/unavailable, never zero.

## Execution and trust boundaries

PDFs and supplied evidence are untrusted. Bound input bytes, pages,
object/stream nesting, decoded bytes, operators, glyphs, form recursion, arrays,
fonts, CMaps, vectors, graph nodes/edges, source references, candidates, proof
work, image dimensions, pixels, and aggregate output bytes.

Execute comparison locally in Rust without external analysis API calls, document
uploads, model downloads, or auxiliary language runtimes. Enforce resource limits
at every implemented boundary and scope failures to the evidence they affect.
Retain backend and comparison-policy versions in reports.

## Reports and process behavior

Separate source facts, conditional verified operations/masks, inferred structure
or correspondence, display context, and unresolved obligations. Expose selected
channels, per-channel coverage, inventory gaps, dependency-local issues, versions,
and resource outcomes. Source IDs always identify a document side.

Exit 0 requires complete comparison without selected changes; exit 1 requires
complete comparison with selected changes; exit 2 is an execution/report error;
exit 3 is incomplete comparison, including one with established changes. Benign
presentation alone does not change the default content exit. Atomically publish
to new destinations and never overwrite inputs.

## Acceptance and evaluation

Retain all five initial practical-release requirements: line-wrap invariance,
page-break invariance, one exact text replacement, one paragraph insertion, and
one paragraph deletion. The original renderer matrix and frozen real-PDF
annotations remain regression evidence. Candidates, review units, inferred
operations, and count claims do not inflate legacy exact-event recall.

Generalization has a separate versioned contract. Annotate document operations,
relationships, exact source/character positions, display ranges, and admissible
alternative correspondences separately. Preserve ambiguity. Never rewrite a
failing legacy annotation to improve the new score.

Cross independent content/relationship mutations with producer, font, columns,
page size, rendering order, pagination, and rasterization changes. Include
Japanese and vertical text, tables, forms, figures, scans, and mixed pages with
independently produced inputs beyond internal ASCII generators.

Required adversarial cases include swapped label/value or row/cell association
with the same multiset, duplicate keys, hidden competitors, added covers,
one-to-many and many-to-one views, false tags, native/OCR overlap, conflicting
recognition, stored-value/appearance disagreement, image-only changes, worker
termination, and local extraction failure beside an independently comparable region.

Report content/relationship recall, exact-mask precision, per-channel compared
evidence, omissions, unresolved outcomes, resource stops, and cost. Empty
predictions fail changed cases. Parsing accuracy is not diff accuracy. Split by
series, template, and producer; inspected PDFs never become unseen holdouts again.

Implementation commits pass formatting, workspace/all-target Clippy with warnings
denied, and workspace tests. Preserve compact per-case results and provenance
without committing large PDFs or raw logs. Single-run timings are observations,
not end-to-end speed claims.

## Migration and limitations

Implement evidence/channels, graph adapters/providers, shared correspondence,
typed comparison/public output, and resource control/generalization evaluation
in that order. Keep the glyph adapter throughout. Additional document-family
rules or a graph exercised only through constructed fixtures do not complete
this migration.

Perfect OCR, universal semantic understanding, and author-history reconstruction
are not promised. Every selected type must remain represented, with established
changes distinguished from interpretations and unresolved evidence. Difficult
inputs cannot silently narrow the document being compared.
