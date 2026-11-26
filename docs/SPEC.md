# pdfdelta technical specification

pdfdelta compares two PDF documents and reports the content changes a document
reader would recognize, while separating content that could not be compared.
This document specifies the behavior of the current tree: one text engine (the
flow token stream), the shared evidence path for non-text channels, and the
output contracts. It states implemented behavior and names partial or absent
capabilities explicitly. It is not a promise that an unlisted extension will
be added.

Implementation status is marked **Implemented**, **Partial**, or **Not
implemented**. Limits, schema versions, and measured counts in this document
are the ones the current release enforces or produces.

## 1. Purpose

Compare two PDFs and detect changes that matter to a reader.

```bash
pdfdelta old.pdf new.pdf
```

When `Release 10 remains available.` becomes `Release 20 remains available.`,
the result is one replacement. When only line wraps, page breaks, font sizes,
margins, column layout, or producer metadata change, the content change count
must stay zero.

The comparison targets what a reader understands as document content, not PDF
byte differences. It reports evidence-based changes and does not claim to prove
author intent or pixel-identical rendering.

### 1.1 Design skeleton

```text
Raw PDF Evidence -> Imperfect Structure -> Robust Alignment -> Exact Diff
```

- Structure reconstruction is not required to be perfect. Alignment must
  recover from wrong line, block, or region boundaries.
- Alignment may correspond text across wrappers, page breaks, and small edits.
- The final step never hides a change: `Release 10` -> `Release 20` stays
  visible even inside a coarse replacement.
- Anything the engine cannot establish is reported as unresolved or
  unsupported instead of an empty result.

### 1.2 Why plain text diff is insufficient

A PDF stores drawing instructions, not paragraphs. Extracting text and running
a line diff breaks in predictable ways:

- **Line breaks:** the same sentence split on two lines becomes a difference.
- **Page breaks:** a paragraph crossing a page boundary becomes a difference.
- **Columns:** reading order is document-type dependent. Papers read
  left-to-right column by column, while old/new legal tables pair rows across
  the page. Column detection is not reading-order detection.
- **Structure variance:** the same prose is split into one or two blocks by
  different producers. Any design that requires exact block structure fails.

### 1.3 Primary evaluation

The first practical release is defined by the five acceptance cases in §2.2.
Two measurement layers support it.

The generated acceptance matrix renders 24 cases through two project-owned PDF
writers (literal `Tj` and positioned `TJ`), giving 48 records. On the current
tree, 48/48 records pass the exact author-intent check, matching 28/28 expected
events and 2,216 expected changed tokens, with 8 false-positive tokens (token
precision 0.996, recall 1.000).

The frozen real-world corpora measure completion, not accuracy. On the 64-pair
native flow panel, 1 pair is a complete text comparison, 62 pairs detect a
difference but remain incomplete because non-text paint or an image-only page
was not examined, and 1 pair ends on broken evidence. On the 36-pair
default-channel panel, 35 pairs exit `1` with unexamined evidence and 1 exits
`3` with broken text evidence; no pair reaches a completed comparison because
the visual and relations channels cannot be fully examined for real documents.
Captured reports, pair lists, and run conditions live under
`benchmark/realworld/results/`; [`benchmarks.md`](benchmarks.md) and
[`limitations.md`](limitations.md) describe the methodology and known gaps.

## 2. Scope and completion conditions

### 2.1 Supported inputs

The primary target is born-digital PDFs with visible native text.

| Handled | Not handled |
|---|---|
| Horizontal Japanese and English | Full vertical writing, handwriting |
| Multi-page single-column documents | General AcroForm interpretation, advanced annotations |
| Simple fonts with or without ToUnicode, unmapped glyph tokens | Full interpretation of complex tables, semantic comparison of image content |
| Modern PDFs with xref and object streams | Complete PDF 2.0 feature coverage |
| Encrypted PDFs with an empty user password or an explicit password | Unknown passwords, unsupported security handlers |
| Stored AcroForm values, widget crops, caller-supplied relation edges | XFA, export-option interpretation, automatic link/footnote target interpretation |

Unsupported input is reported as unsupported or unresolved, never as an empty
comparison.

Images and text drawn as paths are outside the primary text evaluation. Images
are compared coarsely by decoded pixel hash. An existing OCR text layer is kept
as native glyph evidence and participates in visible-content comparison.
Non-painting text (for example render mode 3) remains outside visible-content
comparison. Running OCR, loading a recognition model, and handwriting
recognition are not implemented; they are future evidence providers under the
contract in §11, not part of this increment.

### 2.2 First practical release

The first practical release requires all five cases below. This definition does
not change.

| Case | Content | Expected |
|---|---|---|
| 1 | Only line-wrap positions differ | 0 changes |
| 2 | Only page-break positions differ | 0 changes |
| 3 | `Release 10` -> `Release 20` | 1 replacement |
| 4 | Paragraph insertion | 1 insertion |
| 5 | Paragraph deletion | 1 deletion |

Case 1 includes English documents. English PDFs commonly omit space glyphs and
express spaces by coordinate movement, so space reconstruction (§7.1) is a
prerequisite. Two-column layouts and tables are not part of this release
definition.

The same five cases are also exercised on vendored Typst/Tectonic fixtures with
a Japanese replacement, reverse deletion, and vertical-text controls. The
release command reports the exact change set; it is not required to exit `0`,
because the default channels can remain unexamined (§5.5).

### 2.3 Evidence channels and scope declaration

Comparison targets are selected as channels.

| Channel | Compared content | Status |
|---|---|---|
| `text` | Visible native glyph content, through the flow token stream | Implemented |
| `visual` | Embedded raster images by decoded pixel hash and placement; page rasters retained for review | Partial |
| `forms` | Stored field values, widget crops, declared appearance states | Partial |
| `relations` | Typed relationships such as table membership, labels, captions, references | Partial |
| `presentation` | Line/page breaks, coordinates, font metrics | Observation only; never a content change |

The default channel set is `text,visual,forms,relations`; `--channels` selects a
subset. Every `--channels` run uses the shared evidence pipeline. The text
channel is supplied by the flow engine in both the default command and the
standalone `pdfdelta flow` command.

`--native-text-only` runs the native-glyph flow comparison and writes the
compact native artifact by default; `--full-json` selects the full native flow
report. It cannot be combined with `--channels` or `--review`. `--agent-review`
works with either path.

A selected channel that is missing or unexamined makes the comparison
incomplete. An absent provider result means "not acquired", never "empty
content". An empty inventory is complete only when a provider examined that
channel and scope. The relations channel inventories native structure tags: an
input whose catalog was read and has no `/StructTreeRoot`, no structure
element, and no relation issue has a complete, empty relation inventory. When
both sides prove that absence, no graph edge is enumerated as a declared
relation and the comparison reports `relation_inventory: "complete_empty"`. If
either side has a relation source, or its absence was not proven, the previous
incomplete/unexamined behavior is unchanged.

Output vocabulary is flow-native. The native report schema, multi-channel
report schema, and review manifest versions in §5.4 are current; no
compatibility aliases are written. Artifacts written by earlier builds remain
readable where §5.4 says so.

### 2.4 Complete, unexamined, unsupported, failed

- **Complete:** every selected channel has a complete inventory on both sides
  and every comparison obligation is resolved.
- **Unexamined:** evidence was not obtained. Examples are non-text paint that
  may contain outlined text, uncompared page rasters, relation endpoints
  without correspondence, missing key populations, and unexamined pages. The
  flow text channel has no candidate phase: a flow change is either a verified
  script entry or absent, and absence means the phase does not compute it.
- **Unsupported:** interpretation is not implemented (for example a general
  link target, XFA, or text recognition).
- **Failed:** I/O, malformed input, a fatal backend error, or a resource limit
  prevents a result.

A local result that is independently valid stays in the report even when the
comparison is incomplete. Missing evidence must not disable unrelated regions,
but it also must not be used to claim global uniqueness or completeness. §5.5
defines how these classes select the exit code.

## 3. Architecture

### 3.1 Pipeline

```text
PDF bytes
  |
  v PDF parser backend       object / xref / page / stream access
  |
  v Primitive extraction     content stream interpreter -> Glyph + geometry
  |
  v Layout reconstruction    line / block / region views
  |
  v Flow token stream        tokens, order hypotheses, furniture, paint gate
  |
  v Alignment + script       information-weighted alignment, verified changes
  |
  v Channel evidence         formatting evidence, non-text channels, coverage
  |
  v Report                   changes, issues, verdicts, exit basis
```

PDF syntax and object-graph resolution are separated from glyph
reconstruction, and neither step depends on a lossy plain-text extractor. Raw
objects, content streams, resources, and object identities needed by later
stages survive the parser boundary.

Stages stay independent. Wrong layout is recovered by token alignment; an
uncertain correspondence stays unexamined. Structure and correspondence may
refine each other locally, but refinement selects a view; it never rewrites
glyph text, numbers, images, or field values to shrink a diff.

The text engine keeps only comparison-visible glyphs, flattens them into one
token stream in reconstructed reading order, and diffs that stream. Line and
block boundaries become token metadata and never constrain the diff (§8, §9).
Non-text channels use the shared evidence store and correspondence solver
(§11).

### 3.2 Parser boundary

PDF object parsing and glyph extraction have separate traits.

```rust
pub trait PdfParser: Send + Sync {
    fn parse(&self, pdf: Arc<[u8]>, limits: ParseLimits)
        -> Result<Box<dyn ParsedPdf>>;
    fn parse_with_password(&self, pdf: Arc<[u8]>, limits: ParseLimits, password: &str)
        -> Result<Box<dyn ParsedPdf>>;
}

pub trait ParsedPdf: Send + Sync {
    fn version(&self) -> PdfVersion;
    fn trailer(&self) -> Result<PdfDict>;
    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject>;
    fn pages(&self) -> Result<Vec<PageRef>>;
    fn page_dict(&self, page: PageRef) -> Result<PdfDict>;
    fn raw_stream(&self, reference: ObjectRef) -> Result<RawStream>;
    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream>;
    fn issues(&self) -> &[PdfIssue];
}

pub trait GlyphExtractor: Send + Sync {
    fn extract(&self, pdf: &dyn ParsedPdf, limits: ExtractionLimits)
        -> Result<Document<Glyph>>;
}
```

`PdfVersion`, `ObjectRef`, `PdfObject`, `PdfDict`, `RawStream`,
`DecodedStream`, `ParseLimits`, and `ExtractionLimits` are pdfdelta's neutral
types. Library-specific values such as `lopdf::Object` do not cross the
boundary. `ParsedPdf` is a facade for primitive extraction, not a second PDF
DOM.

The production pipeline composes `PdfParser` with pdfdelta's own
`GlyphExtractor` in a `ParserBackedGlyphSource` facade that produces
`Document<Glyph>` from PDF bytes. Diff-engine work can proceed on
programmatically built `Document<Glyph>` fixtures without a PDF backend.
External extractors are used only as differential-testing oracles.

### 3.3 Using an existing parser

The PDF object parser is not reimplemented. Header, lexer, indirect objects,
xref tables and streams, object streams, trailer chains, the page tree, and
stream decoding are delegated to an existing library; pdfdelta implements the
content-stream interpretation that preserves glyph-level evidence.

A custom `PdfParser` backend is added only when a fixture or benchmark
demonstrates one of the following:

- A required scope feature (xref stream, object stream, incremental update,
  resource inheritance) is mishandled and cannot be fixed through the adapter
  or upstream.
- Raw content bytes, object identity, or filter information needed downstream
  is unavailable.
- Resource limits or error classification for malformed input cannot be
  implemented safely on the existing backend.
- Differential testing reproduces a backend error that another backend cannot
  avoid.

A new backend keeps the `PdfParser`/`ParsedPdf` boundary and the pipeline.
Backend replacement is not a default roadmap goal.

### 3.4 Workspace

```text
pdfdelta/
  Cargo.toml
  crates/
    pdfdelta-core/       pure library; no CLI concerns
      src/
        pdf/             backend, object access, content syntax, fonts
        source.rs        ParserBackedGlyphSource, extraction boundary
        model.rs         Glyph and neutral document types
        layout/          line, block, region reconstruction
        normalize/       canonical text, tokens, formatting evidence
        document/        evidence store, graph, channels, solver, local comparisons
        review/          agent review contract and planner
        text_envelope/   conditional local text queries
    pdfdelta-flow/       token-stream diff engine (depends on core only)
      src/
        order.rs         reading-order hypotheses
        furniture.rs     repeated header/footer handling
        align.rs         information-weighted alignment
        script.rs        change classification and move rules
        verify.rs        independent replay verification
        assess.rs        confidence and formatting evidence
    pdfdelta-cli/        filesystem, arguments, workers, publication
    pdfdelta-bench/      generated fixtures, mutations, evaluation, pdfbench
  fixtures/              external, extraction-conformance, issue fixtures
  benchmark/             manifests, microbenchmarks, real-world corpus tooling
  docs/                  user documentation and this specification
  fuzz/                  nightly-only fuzz workspace
```

| Crate | Kind | Binary |
|---|---|---|
| `pdfdelta-core` | library | - |
| `pdfdelta-flow` | library | - |
| `pdfdelta-cli` | binary | `pdfdelta` |
| `pdfdelta-bench` | binary | `pdfbench` |

The CLI owns filesystem access, passwords, child processes, limits, report
publication, and process status. Core owns validation, graph views,
correspondence, local comparisons, and report models, and never touches the
filesystem. Flow owns tokenization, ordering, furniture, alignment, the edit
script, replay verification, formatting evidence, and confidence. Bench owns
generated inputs, mutations, independent annotations, manifests, and scoring.
Flow depends on core only.

### 3.5 One text engine and compatibility

There is one text engine: the token-stream flow engine. `--engine flow` is the
default and is accepted as a compatibility no-op. `--engine proof` is still
parsed only so the process can reject it with exit `2` and a stable message
naming the flow replacements (`--trace-json`, `--compact-json`, `--full-json`,
`--review`, `--agent-review`, and the standalone `pdfdelta flow` report); it
writes no output and runs no second engine.

Reports carry `"engine": "flow"` whenever flow runs, including a run whose
selected channels exclude text. Agent review bundles written by earlier builds
remain readable through the `review` commands; a static HTML bundle is
self-contained, and its source index remains readable through the
compatibility reader in §5.4.

### 3.6 Language and dependency policy

Stable Rust and Edition 2024 are required. The fuzz workspace is the only
nightly consumer; it is not part of the stable workspace or lockfile, and the
stable gate only compiles its targets (`cargo check --manifest-path
fuzz/Cargo.toml --bins`).

**Implemented by pdfdelta:** parser-library adapter and neutral facade,
content-stream and text-state interpretation, the required ToUnicode/CMap
handling, glyph geometry recovery, line/block/region reconstruction, canonical
text and tokenization, reading-order selection, furniture detection, paint
identity, information-weighted token alignment, edit-script construction with
move and layout rules, replay verification, change assessment, formatting
evidence, the shared correspondence solver, and local comparisons for non-text
channels.

**Used as production dependencies:** the PDF object parser backend (`lopdf`),
CLI parsing (`clap`), JSON (`serde`/`serde_json`), Unicode character data
(`unicode-normalization`, `unicode-segmentation`, `unicode-bidi`), hashing
(`sha2`), small vectors (`smallvec`), stream decoding (`flate2`), rendering
(`hayro`), and CID CMap reference data (`hayro-cmap`). Unicode crates provide
character data and NFC/segmentation primitives only; canonical text, token
boundaries, and normalization events remain pdfdelta's responsibility.

**Development and test dependencies:** `proptest`, external extractors for
oracle comparisons, and external renderers for fixture generation. A production
design is never shaped to fit an oracle's output format.

## 4. Data model

### 4.1 Glyph

```rust
pub struct Glyph {
    pub id: GlyphId,
    pub text: DecodedText,
    pub raw_code: Vec<u8>,
    pub page: PageId,
    pub bbox: Rect,
    pub baseline: Vec2,
    pub direction: Vec2,
    pub font_id: FontId,
    pub font_size: f64,
    pub render_order: u32,
    pub render_mode: TextRenderMode,
    pub provenance: GlyphProvenance,
}

pub struct GlyphProvenance {
    pub content_stream: ObjectRef,
    pub operator_index: u32,
}
```

Coordinates and matrices use `f64`: accumulated matrix error makes `f32`
unjustified. Coordinates are stored in the canonical page coordinate system
after `/Rotate` handling; rotation is not pushed downstream. `bbox` is the
layout bounding box derived from font widths, ascent/descent, and the transform,
not an ink outline. Rotated text also uses `baseline` and `direction`; an
axis-aligned `Rect` alone never decides order.

`render_mode` and `provenance` are retained so invisible text and overpainting
can be verified later and backend differences can be traced to an object and
operator. Incomplete visibility analysis does not discard evidence.

```rust
pub enum DecodedText {
    Mapped(String),
    Unmapped { font_hash: FontProgramHash, glyph_id: u16 },
}
```

A glyph that cannot be mapped to Unicode is never collapsed to U+FFFD or empty
text. Subset fonts without ToUnicode exist in born-digital PDFs, and a blanket
unresolved result would make coverage useless. When both revisions embed the
same program, `(font_hash, glyph_id)` is a comparable identity. Different
programs without a Unicode mapping remain unresolved unless the name or outline
identity rules in §6.4 apply.

### 4.2 Reversible layout

```rust
pub struct Line  { pub glyphs: Vec<GlyphId>, /* ... */ }
pub struct Block { pub lines: Vec<LineId>,  /* ... */ }
```

Line and block boundaries are hypotheses. A wrong boundary must be recoverable
from the glyph sequence, and a native block is a view rather than
ground truth. `BlockText` retains raw text, canonical text, comparable tokens,
normalization events, source maps, break offsets, font-size signatures, and
position signatures so later stages can project back to glyphs and geometry.

A `ScalarRange` is a Unicode scalar index range, not a UTF-8 byte offset.
Ligature expansion, NFC, and hyphen joins may map one glyph to several scalars
or several glyphs to one scalar; every canonical range must still recover its
source glyphs and geometry. Canonical text is used for visible comparison and
evidence; a separate matching-oriented string is not an input to the flow diff.

Canonical text absorbs only differences that do not change prose identity:
NFC, context-dependent soft line-break joins, repeated whitespace, and ligature
expansion. A soft break between Latin letters or digits becomes one space; a
break between CJK scalars, or between CJK and alphanumerics, becomes no
separator; explicit whitespace is never duplicated. A discretionary hyphen
(U+00AD) may be removed with its break, while an ordinary `-` or U+2010 is
retained because discretionary hyphenation cannot be proven from shape or word
length; an uncertain hyphen source range stays unresolved and a paragraph
boundary remains a recoverable block boundary.

Canonical text does not absorb fullwidth/halfwidth or other compatibility
differences (NFC, not NFKC), because those can be meaningful revisions in
identifiers, model numbers, and contract numbers. Normalization events are
recorded as `{kind, raw_range, canonical_range, source}`; after alignment, a
matched run whose raw text differs but whose canonical text is equal is
formatting-only evidence, and adjacent same-kind events merge so glyph or line
count differences do not inflate the entry count. Numeric masking is never
applied to canonical token identity; §8.4 confines digit masking to furniture.

### 4.3 Flow token stream

```rust
pub struct FlowToken {
    pub text: String,
    pub page: PageId,
    pub bounds: Option<Rect>,
    pub block: usize,
    pub uncertain: bool,
    pub line_break_after: bool,
    pub font_size: Option<f64>,
    pub raw: Option<Box<str>>,
    pub position: Option<PositionSignature>,
    pub page_break_after: bool,
    pub normalization: u8,
}
```

`FlowToken::text` is the token identity. For mapped text it is the canonical
scalar sequence. For an unmapped glyph it is
`\uFFFC<kind>:<identity-hash>:<glyph-id>`, where `kind` is `name`, `outline`, or
`program` (§6.4), so two occurrences compare equal only when their identities
do. `uncertain` marks the token as not decoded.

`bounds`, `font_size`, `raw`, `position`, and `normalization` are presentation
evidence for matched tokens. They never affect token identity, alignment, or
the edit script. `None` means the evidence could not be established exactly;
it is not an empty value.

A `FlowStream` holds the tokens in chosen reading order, the source glyph ids
per token when requested, the separated furniture lines, and the glyph
conservation record. Two documents are compared only through their token text
sequences plus this evidence.

### 4.4 Change script and assessment

```rust
pub enum FlowChange {
    Delete { old: Range<usize> },
    Insert { new: Range<usize> },
    Replace { old: Range<usize>, new: Range<usize> },
    Move { old: Range<usize>, new: Range<usize> },
    ReadingOrder { old: Range<usize>, new: Range<usize> },
    LayoutOnly { old: Range<usize>, new: Range<usize> },
}
```

The script covers every non-spine old and new token exactly once and is
verified by replay (§9.5). A change carries a `ChangeAssessment`: a `Confidence`
of `high`, `medium`, or `low`, and zero or more `AmbiguityReason` values
(`coarse_span`, `repeated_run`, `order_uncertain`, `short_move`,
`numeric_only`, `numeric_table`). Assessment is descriptive and never changes
the selected exit code or the script.

`ReadingOrder` covers a run whose text is unchanged and whose only difference is
the extraction order of two side-by-side runs that share one visual row and
keep their left-to-right order (§9.4). It never selects a content difference.

Formatting evidence is a separate list of matched token runs with one or more
`FormattingReason` values (`normalization`, `block_structure`, `font_size`,
`position`, `line_break`, `page_break`) and the changed font sizes when
applicable (§10).

The native report does not have an assessment object, candidate changes,
proven changed regions, unresolved regions, or an alignment-coverage position.
Those concepts belonged to a removed engine. Absence means the phase does not
exist, not that a search completed and found nothing.

### 4.5 Evidence store and document graph

Non-text channels use an immutable evidence store that distinguishes native,
rendered, and structured source references.

- A native reference retains glyph text, raw code, font identity, geometry,
  baseline, direction, render order/mode, crop/clip observations, vector lines,
  and PDF object/content-stream/operator provenance.
- A rendered reference retains page/region geometry, polygons, a pixel or
  verifiable image identity, and backend/version/profile.
- A structured reference retains field, tag, or annotation identity and the
  source object when available.

Two views of the same visible content (for example a native text layer and an
OCR layer) are equivalent or competing views, not two independent contents. The
basis for equivalence is recorded; unproven duplication stays competing and is
never consumed twice. A recognition result never fabricates a native glyph id.
A future recognition provider keeps its own image reference and recognition
alternative, and recognition-confusable differences (for example `0`/`O` or
`1`/`I`) remain reported instead of being normalized away.

A comparison contract records the selected channel set and its version. Backend
identity (kind, name, version, profile, model) is retained in the report.

The document graph represents sections, paragraphs, list items,
tables/rows/columns/cells, form fields, figures/captions, annotations, code,
mathematics, and unknown regions as source-backed nodes or candidate views.
Typed edges cover containment, local order, row/column identity, labels,
captions, references, and alternatives. Coordinates and text direction remain
evidence, not graph keys.

Existence of a tag is a source fact; its reading order and semantic role may
compete, and a model score is not proof. Dangling references, malformed
geometry, and cycles in hierarchical relations are rejected.

## 5. Output contract

### 5.1 Confidence and ambiguity

Every flow change carries a confidence and an ambiguity list. They are cheap
diagnostics about how the chosen script may be read; they are not a probability
of correctness and never a precondition for reporting a change.

Low confidence or an ambiguity reason never removes a change, never lowers
coverage, and never changes the exit code. A change that replays correctly is
reported even when a repeated run, a coarse span, a numeric-only edit, or an
order-uncertain page makes its position less certain.

### 5.2 Output categories

The flow text channel reports:

- content changes (`Delete`, `Insert`, `Replace`, `Move`), each with its
  token ranges, pages, text, and assessment;
- reading-order changes (`ReadingOrder`), listed with their token ranges,
  pages, and text but never a detected difference;
- layout-only changes (`LayoutOnly`), counted but not listed and never a
  detected difference;
- formatting-only evidence on matched runs;
- text extraction issues and paint-gate issues with their side, kind, and
  message.

It does not report tentative candidates, `TENTATIVE` markers, unresolved
regions, source ownership, relation dependency, assumptions, search
completeness, or work consumption. Those are non-text-channel structures where
they apply, or removed-engine positions.

Canonical normalization differences absorbed on matched runs are reported as
formatting-only evidence, not silently dropped. Formatting-only reporting is
best-effort, never proves rendering identity, and never affects the content
change count or exit code.

### 5.3 Human-readable report

The multi-channel report lists typed and inferred change counts, per-channel
coverage (discovered versus compared sources per side), flow text changes, and
every unresolved region with its reason. Lists are bounded with explicit
truncation markers.

With `--native-text-only`, the report is the flow summary followed by numbered
changes:

```text
text engine: flow (1 detected changes, 0 formatting-only)
flow summary
  old: 32 tokens
  new: 32 tokens
  equal: 31 tokens
  delete: 0 changes, 0 old tokens, 0 new tokens
  insert: 0 changes, 0 old tokens, 0 new tokens
  replace: 1 changes, 1 old tokens, 1 new tokens
  move: 0 changes, 0 old tokens, 0 new tokens
  layout-only: 0 changes, 0 old tokens, 0 new tokens
  content changes: 1
  moved changes: 0
  detected changes: 1
  changes: 1
  old extraction: complete
  new extraction: complete
  old order: 1 pages, 0 stream, 0 baseline, 0 uncertain
  new order: 1 pages, 0 stream, 0 baseline, 0 uncertain
  old glyph conservation: 222/222
  new glyph conservation: 222/222
  furniture: 0 old, 0 new masked lines

1. replace old 1 -> new 1 [confidence: medium; ambiguity: numeric_only]
   old: ... Release [10] remains available ...
   new: ... Release [20] remains available ...
```

Each change names its kind, the one-based old/new pages it touches, its
confidence and ambiguity (when any ambiguity reason exists), and the changed
tokens in brackets between a few unchanged context tokens. Incomplete pages,
order-uncertain pages, and pages with name-identity placeholders are named.
Token ranges are available in the JSON report.
Furniture changes are listed separately. PDF-derived text is untrusted:
Unicode bidi controls and control characters are escaped to literal `\u{...}`
forms so a hostile glyph mapping cannot drive the terminal or reorder the
display. Color (`--color auto|always|never`) only supplements the markers; the
report is readable without it. The report never prints only "No differences
found".

### 5.4 JSON reports and schema versions

| Surface | Version | Content |
|---|---|---|
| native compact envelope | `artifact_format: pdfdelta-native-compact`, `artifact_version: 3`, `source_schema_version: 13` | compact flow report under `report` |
| native compact report | `schema_version: 13` | summary, changes, reading-order evidence, formatting evidence, extraction, layout-only counts |
| native full envelope | `artifact_format: pdfdelta-native-full`, `artifact_version: 2`, `source_schema_version: 13` | compact body plus `token_sources` and `glyph_accounting` |
| multi-channel report | `schema_version: 3`, `contract.version: 1` | channel verdicts, coverage, flow text result (including reading-order evidence), non-text changes |
| standalone flow report | `schema: pdfdelta-flow/1` | flow summary, changes, formatting evidence, order, furniture, conservation |
| diagnostic trace | `trace_schema: pdfdelta-flow-trace/1`, `trace_schema_version: 1` | bounded per-phase metrics and typed errors |
| static review manifest | `version: 3` | published artifacts; source index `version: 1` |
| agent review bundle | `agent-review/v1` | case packets and manifest |
| agent decision input | `agent-decision/v1` | caller answers |
| review import result | `agent-review-result/v2` | validated decision result |
| text-envelope | `schema_version: 1` | conditional local text claims |

The native summary carries `content_changes`, `formatting_only_changes`,
`reading_order_changes`, `uncertain_changes`, the three extraction issue
counters, `comparison_complete`, `difference_status`, `old_tokens`, and
`new_tokens`. `reading_order_changes[]` carries the same span shape as a
content change but never contributes to `content_changes`. Each content change
carries `kind`, `old_span`, `new_span` (each `{pages, text, token_range}`), and
`confidence` and `ambiguity`. Formatting entries carry `old_span`, `new_span`,
`reasons`, and `old_size`/`new_size` only when `font_size` is a reason.

The full report streams `token_sources` and `glyph_accounting` through the
serializer instead of building one JSON value. When a side was built in a child
process, its per-glyph records spill to a temporary side cache that the
reporting parent copies back in token order, so the parent does not retain an
extracted document. The compact report carries no per-glyph sources and must be
regenerated with `--full-json` when individual geometry or operator provenance
is needed.

The diagnostic trace records input reading, parse, glyph extraction,
tokenization, furniture/order handling, flow diff, assessment/formatting,
channel comparison, and report. A failed or incomplete phase records an `error`
whose `kind` is the explicit error class (`backend`, `unsupported`,
`unresolved`, or `limit_exceeded`); a limit error also names its resource and
bound. The message is contextual and never decides the class. `duration_us` and
`comparison_wall_time_ms` are the only wall-clock fields and are excluded from
golden comparisons.

The multi-channel report separates typed non-text changes and inferred
non-text changes, per-channel coverage and completeness, `flow_text` changes
and formatting evidence, unresolved source ranges, and `flow_owned_text`
records. `flow_owned_text` (also at `comparison.flow_owned_text`) counts
shared-evidence text components and local comparisons that flow ownership
replaced, with an `unchecked_obligations` count per skipped reason; it is
absent when the shared search recorded no such position. Absence is not a
completed text search. A component that mixes text and non-text sources is
never superseded. Text views remain correspondence context for relationships
and structure, but their local comparisons are not exit evidence.

`detected`, `indeterminate`, and `no_content_change` describe content
differences independently of extraction and comparison completeness. A complete
token comparison or a formatting annotation does not establish visual
equivalence.

Compatibility readers accept historical artifacts: schema-11 native compact
reports and full envelopes, schema-2 multi-channel reports (including the old
`superseded_text` and reason names as input-only aliases), proof-era agent
review bundles, and static review source artifacts in the version-2 double-gzip
shape or older uncompressed `sources.json`. New output never writes the old
names.

### 5.5 CLI and exit codes

```bash
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --backend-info
pdfdelta inspect document.pdf --objects
pdfdelta inspect document.pdf --glyphs
pdfdelta inspect document.pdf --svg overlay.svg
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf --channels text,visual,forms,relations
pdfdelta old.pdf new.pdf --native-text-only -j result.json
pdfdelta old.pdf new.pdf -o diff.txt -q -s
pdfdelta old.pdf new.pdf --review review-output
pdfdelta old.pdf new.pdf --agent-review review-output
pdfdelta old.pdf new.pdf --color auto|always|never
pdfdelta old.pdf new.pdf --limit-scale 16
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
pdfdelta old.pdf new.pdf --old-font-identity FontName=identity --new-font-identity FontName=identity
pdfdelta old.pdf new.pdf --extraction-cache-dir ~/.cache/pdfdelta
pdfdelta flow old.pdf new.pdf
pdfdelta text-envelope request.json
pdfdelta completions bash|zsh|fish|powershell|elvish
```

`inspect` provides backend summary, objects, glyphs, and an SVG overlay.
Comparison writes a text report, JSON, trace, review bundle, or nothing
(`-q`). Passwords are read from per-side files, never from argv, and never
written to errors, reports, or traces. `--old-font-identity` /
`--new-font-identity` are caller assertions of the form `BaseFont=identity`;
the identity bytes are reduced to a domain-separated hash. `--limit-scale`
scales flow extraction count limits, the shared footer-matching work limit, and
the render and image worker CPU and wall budgets; it does not lower any limit
and does not change parser limits, image pixel limits, or layout thresholds. `-s`/`--strict` is accepted for compatibility and does not change
the comparison or the exit status.

| Code | Meaning |
|---|---|
| `0` | No detected difference and every selected channel examined (no reason) |
| `1` | At least one selected channel detected a typed difference with its own evidence intact; the comparison may still be incomplete |
| `2` | Execution or report-writing error (I/O, malformed input, fatal error, resource limit) |
| `3` | No intact detection and at least one broken or unexamined reason |

Exit `2` precedes any comparison result. Otherwise the channel verdicts decide:

- A channel that detected a typed difference with its own evidence intact
  selects `1`, even when another channel is broken or was not examined. In that
  case `comparison_complete` stays `false` and `exit_basis` is
  `detected_with_unexamined_evidence` (or `complete_detected` when nothing is
  incomplete).
- Broken evidence blocks only its own channel: text extraction and glyph
  conservation for text, forms reasons for forms, relation graph and
  work-budget failures for relations, and a visual backend failure for visual.
  Cross-channel and unattributable reasons block every channel.
- With no intact detection, any broken reason selects `3` with
  `broken_evidence`; otherwise any unexamined reason selects `3` with
  `unexamined_evidence`.
- Exit `0` requires every selected channel to be complete.

This is not a simple `3 > 1` priority. Exit `1` with incomplete evidence is
normal and is the expected result for real documents.

The single-channel `pdfdelta flow` and native path use the same verdicts with
only the text channel: broken text evidence selects `3`, a detected difference
with intact evidence selects `1`, unexamined-only evidence selects `3`, and a
complete unchanged result selects `0`. Formatting-only, reading-order, and
layout-only changes never select `1`.

Exit `0` is not reachable with the default channels for a document with pages
in the current build. Rendered page rasters are retained for review but not
compared, so every rendered page leaves the visual inventory incomplete, and
native relation endpoints and semantic relationships are not established for a
tagged document, so relations stays unexamined; a pair where both inputs prove
no relation source has a complete empty inventory. An identical or purely
reflowed document therefore exits `3` with `exit_basis: "unexamined_evidence"`. `--channels text`,
`--channels text,forms`, and `--native-text-only` select channels whose
evidence can close and exit `0` when those channels are complete and unchanged.

### 5.6 Review bundles

**Static HTML review** (`--review DIR`) publishes byte-exact source PDFs,
lossless page previews, the complete comparison JSON, and source IDs with
evidence, geometry, provenance, graph views, and warnings. The source index
(`sources.json.gz`) is a gzip member inside the artifact's gzip member, as in
the version-2 writer, because the second pass keeps the large index substantially
smaller; the reader also accepts the single-stream version-3 shape and older
uncompressed indexes. The manifest is version 3. The output is bounded to
512 MiB, 1,024 page previews, and 20 million source-location visits;
a failure may leave an incomplete directory without `index.html`. Review output
never changes the comparison result or exit status. It is unavailable with
`--native-text-only` because that path retains no rasters or shared evidence
store.

Excerpts distinguish strict changes (A), non-owning corresponding-range content
changes (B), inferred comparisons (C), and formatting-only entries (F). A is a
delete, insert, or high-confidence replace; B is a coarse replace whose inner
edit is not localized; C is a move or an ambiguous change. The categories are
never added together as strict recall. Flow has no separate
unresolved-comparison category; broken or unexamined evidence is listed with
its reason record and channel verdict.

**Agent review** (`--agent-review DIR`) writes a bounded case bundle for a
calling agent. The bundle schema is `agent-review/v1`; every response is
complete JSON inside the requested byte budget, and `review import` records
external decisions separately from the engine result. The bundle plans at most
4,096 cases by default; `--agent-case-budget` raises that to 1–131,072, and the
512 MiB bundle cap applies to each published bundle. Cases are ordered by
importance: flow content and formatting first, then acquisition evidence with
broken before unexamined and page-local before document-wide, then page-residual
cases. A truncated plan records a bundle-level `cases_not_exported` omission with
the number of withheld cases.

Planning and publication are streaming. Case bodies and gathered contexts are
written to a private temporary store; the resident planner keeps only the
fixed-size listing index (identifiers, digests, counts, ordering keys, and
omission records). The manifest and index order are the same as the previous
whole-plan assembler, so a bundle's cases, identifiers, links, and digests are
unchanged. Peak memory is therefore independent of the case count: on
`nist-controls` both the 20,222-case complete export and a diagnostic
80,888-case plan peak at about 2.89 GB, while the comparison's own evidence
remains resident.

A plan larger than one 512 MiB bundle is published as numbered part bundles
under the destination directory (`part-0001`, `part-0002`, ...). Cases are
attempted in 4,096-case batches; a part that would exceed its cap is halved and
retried, so a part can close mid-batch without reordering a case. Each part is
a complete `agent-review/v1` bundle with its own manifest, and the root
`manifest.json` is written last as a `pdfdelta-agent-review-set` version 1
manifest that lists every part, its case count and byte size, and the global
`export_complete` flag. Queries and `review import` accept either a single
bundle or this part set. A case that exceeds the per-case source or member cap
is split into consecutive sub-cases rather than truncated. Each part records
`subcase` with `schema: "agent-review-split/v1"`, the shared `parent` case id,
zero-based `index`, and total `count`; split sub-cases remain consecutive
across a batch boundary. The first part carries the quoted text and flow report
entries, and later parts carry the remaining source evidence. Sub-cases count
toward the case budget and `export_complete`. Exporting a bundle changes no
comparison field, coverage count, or exit status.

## 6. PDF backend and primitive extraction

### 6.1 Purpose

The job is not plain-text extraction. It is recovering characters, drawing
positions, and drawing order together with raw evidence that later stages can
verify.

### 6.2 Parser backend capabilities

The default adapter is `lopdf` 0.45.0. Required capabilities:

- Header/version, indirect objects, and reference resolution.
- Classic xref tables and xref streams, object streams, trailer chains, and the
  latest revision of incremental updates.
- Page-tree traversal and resource inheritance. A broken branch with a parent
  mismatch or reference cycle does not discard independently verifiable
  branches; missing branches and a root `Count` mismatch become
  document-scoped unresolved issues.
- Raw stream metadata and decoded stream bytes.
- Flate decoding. An unsupported filter becomes unsupported, never empty text.
- Encryption detection. Automatic decryption with an empty user password, or
  decryption with a caller-supplied password, is accepted. A missing or wrong
  password, an `Encrypt` dictionary remaining after decryption, and unsupported
  security handlers are unsupported.
- Object count, recursion, and decoded-size limits, with error categories
  preserved.

The adapter preserves object id, generation, dictionary key, stream filter, and
content-stream order. Library-specific types are converted inside
`pdf/backend/` and never cross the facade. Backend selection is decided by
fixture pass rate, access to raw evidence, and the ability to enforce safe
limits, not by popularity.

### 6.3 Content-stream interpreter and coordinates

Content-stream syntax and text/graphics state are implemented in pdfdelta.

```text
q Q cm
BT ET Tf Tm Td TD T* TL Tj TJ ' " Tc Tw Tz Ts Tr
Do
```

`q`/`Q`/`cm` track the graphics state and CTM. `Do` resolves the XObject
subtype and recursively interprets only Form XObjects, with bounded resources
and matrix stacks. A Form runs on a state snapshot separated from its caller;
a `q` without `Q` may be discarded at a Form boundary, while `Q` underflow and
page-level imbalance remain unresolved. Image XObjects are skipped by the text
interpreter; images are evidence for the visual channel. `'` and `"` expand to
their compound text operations. `BI`/`ID`/`EI` use a dedicated inline-image
lexer that resumes at the following operator.

When `/Contents` is an array, streams are interpreted in order without joining
completed tokens across a boundary. Pending operands persist across the array;
a dictionary value beginning in the next stream is recovered only in that
limited case. Incomplete operand recovery is attempted at most once per
boundary; otherwise the same prefix becomes unresolved instead of being
re-parsed repeatedly.

Page boxes are axis-normalized before `/Rotate`; finite reversed coordinates
are accepted, and zero-area or non-finite boxes are unresolved.

The interpreter tracks CTM, text matrix, text line matrix, font matrix,
horizontal scaling, character/word spacing, and text rise to compute glyph
coordinates and advances. Render mode is recorded from the start. Non-painting
glyphs remain evidence but are excluded from visible-content comparison.
Clipping, alpha, white overpainting, and full visibility analysis belong to the
clipping analysis described below and are never simplified to "visible".

Clipping evidence is prepaid and bounded per page: rectangles,
convex quadrilaterals, separated rectangle pairs, six-edge L shapes, eight-edge
single-side notches, a rectangle/L pair, literally closed orthogonal subpaths,
and exact point witnesses for cubic/straight paths in one `W n`/`W* n`
construction run. Straight and curved clips additionally receive a conservative
polygon classification. Unsupported nonrectangular compositions keep overlapping
glyphs with a partial-clip status and a page-scoped extraction gap; glyphs
strictly outside the outward control-point bound are excluded. A glyph is never
treated as invisible merely because its clip is unsupported. Opacity and
overpainting remain uninterpreted.

### 6.4 Font decoding

Glyph code to Unicode mapping is not simple. ToUnicode, simple fonts, Type0
fonts, and CIDs are handled in `pdf/font/` as one decoding result.

```rust
pub trait FontDecoder {
    fn decode_codes(&self, bytes: &[u8]) -> Result<Vec<DecodedCode>>;
}

pub struct DecodedCode {
    pub raw_code: Vec<u8>,
    pub text: DecodedText,
    pub glyph_id: Option<u16>,
    pub advance: f64,
}
```

Current coverage:

- Simple fonts prefer an exact per-code ToUnicode entry. Without one, a
  declared Standard, WinAnsi, or MacRoman encoding is used through
  `Differences`. An explicit invalid entry and an unknown `Differences` name
  stay `Unmapped`; they never fall back silently.
- Standard 14 identity includes the canonical BaseFont name and the built-in or
  declared Standard/WinAnsi/MacRoman encoding name. A `Differences` dictionary
  changes code meaning and is excluded from that canonical identity.
- Type1C identity requires a single `FontFile3` stream with subtype `Type1C`.
  MMType1 variation axes are not interpreted; only declared simple-font
  encoding and metrics are used, and no stable identity is given to an unmapped
  glyph from an unrecognized path.
- Identity-H and Identity-V always split content into fixed two-byte codes. A
  custom Type0 encoding CMap is supported only with `WMode` 0 or 1, one
  full-domain codespace, and one identity `begincidrange` mapping that range to
  CIDs from 0. `usecmap`, `begincidchar`, multiple ranges, and non-identity
  mappings are unsupported.
- ToUnicode entries are matched at the decoder's fixed width. A missing entry,
  an isolated UTF-16 surrogate destination, and a missing ToUnicode map are
  `Unmapped`. Empty, odd-length, and non-hex destinations remain errors.
- CID fonts without an embedded program and without ToUnicode do not infer
  identity from BaseFont alone. A caller-provided external font identity for
  the same BaseFont on both sides may be hashed in a dedicated domain and used
  as the unmapped glyph identity; that is an assertion, not font discovery.
  With the bundled predefined CID collections, Adobe Japan1/GB1/CNS1/Korea1
  UCS2 tables map known CIDs to Unicode; ToUnicode keeps priority and unknown
  CIDs stay placeholders.
- Type 3 supports only axis-aligned finite `FontMatrix` (`a > 0`, `d != 0`) and
  requires `FirstChar`, `LastChar`, `Widths`, `FontBBox`, `Encoding`, and
  `CharProcs`. Rotation, shear, translation, and horizontal reversal are
  unsupported. Unmapped identity hashes the CharProcs streams in glyph-name
  byte order in a dedicated domain, so dictionary order, object ids, and
  Encoding code movement do not change it. Named resources are hashed as a
  canonical graph with cycle, depth, and byte limits.
- Identity-V covers one CIDFontType0/CIDFontType2 descendant, fixed two-byte
  codes, exact ToUnicode, and a subset without per-CID `W2`. `DW2` may be
  omitted (default `[880 -1000]`) or a finite two-element downward
  displacement. General custom vertical CMaps, per-CID `W2`, and general
  vertical reading order are out of scope.

When a CID descendant's `FontDescriptor` lacks Ascent/Descent or has
`Ascent <= Descent`, a valid `FontBBox` top/bottom supplies the font-wide
vertical metric. Only when neither yields a positive extent is the glyph
unresolved; a zero-area glyph is never passed downstream.

An unmapped glyph keeps a stable identity in this order: (1) the
subset-stripped BaseFont plus the exact glyph name from PDF `Encoding`
(including `Differences`), a cleartext Type1 `/Encoding`, or a Type1C charset;
(2) a CID-keyed CFF `CharStrings` entry selected by the CID; (3) the embedded
program hash plus glyph id. The first byte of the identity hash tags the source
(`program`, `name`, or `outline`). Equal names in one family and equal outlines
compare equal across subsets. CID 0 of an Identity-encoded composite font is
treated as the specification's `.notdef` and uses name identity rather than an
empty outline.

Glyph names resolve through the Adobe Glyph List, the `uni`/`u` algorithmic
forms, a bundled Apache-2.0 pdf.js supplement for unambiguous TeX/AMS names, and
a small project-authored MIT table for remaining unambiguous TeX names. The
supplement excludes private-use and control values. Combining overlays,
enclosures, script names, and generic `gNN`/`glyphNNN` names stay placeholders
because their meaning is not one standalone character.

### 6.5 Issue classification

Extraction issues are typed and scoped to the document, a page, a page-tree
gap, or a glyph gap. Unsupported (interpretation missing), unresolved (evidence
exists but cannot be decided), resource limit (budget reached), and fatal
(result cannot be represented) are distinct. A quantity that could not be
extracted is unknown; it is never replaced by empty text or zero tokens.

## 7. Layout reconstruction

### 7.1 Glyph to line

For text direction `d = (dx, dy)` and its normal `n = (-dy, dx)`, glyph
coordinates are projected onto both axes and compared as along-axis and
baseline-axis positions. The model supports arbitrary direction; the release
target is horizontal text, and vertical-specific metrics and reading order are
bounded (§13).

Line membership uses baseline distance, glyph height, font size, direction, and
inline gaps. Fixed pixel thresholds are not used; the defaults are relative
ratios (`max_baseline_distance_ratio` 0.25, `min_cross_axis_overlap_ratio` 0.25,
`min_direction_similarity` 0.98). Off-baseline glyphs may join a line only when
cross-axis overlap and script-size bounds hold; a glyph outside those bounds
starts a new line.

**Space reconstruction:** when the along-axis gap between adjacent glyphs
exceeds a threshold relative to font size and the median advance, a synthetic
space is inserted. This is required for Case 1 in English PDFs that draw no
space glyph. A reconstructed space is marked synthetic and stays distinct from
raw source.

### 7.2 Line to block

Line joining uses relative evidence: vertical proximity, horizontal overlap,
indent similarity, and font continuity, with a minimum join score. Page
boundaries are not block boundaries: a paragraph continuing across a page uses
normalized body-area, indent, font, and line-gap continuity. Repeated headers
and footers are excluded from body candidates; uncertain cases stay split and
reach alignment.

Content text is not the primary join signal. Punctuation alone does not end a
paragraph, and page-boundary continuation may use a light character-class
signal only as supporting evidence; it never decides a block alone.

### 7.3 Regions and tables

- **XY-cut:** a central vertical whitespace gap is a split candidate. Column
  detection is not reading-order determination; region structure and reading
  order stay separate, and an undecidable order stays `UNKNOWN`.
- **Region graph:** a graph of Above/Below/LeftOf/RightOf/Aligned/SameColumn
  relations, not a tree.
- **Tables:** a ruled grid takes vector lines from drawing operators. A
  borderless table is inferred from repeated x/y alignment. A table is not
  serialized like ordinary blocks.

A complete rectangular ruled grid supplies an inferred table view with column
identities from the first row and row identities from the first column. Cell
values never establish axis correspondence, and equal values in different rows
never decide cell correspondence. Changed headers may receive inferred
candidates from shared child or neighboring identities. Complex tables, merged
cells, and arbitrary table semantics are not implemented.

### 7.4 Reversibility and reading order

Line, paragraph, cell, and tag boundaries are soft boundaries; extraction gaps
and unsupported regions are hard boundaries. A split or merged view of one
native block retains token ownership and separator provenance. Competing views
that imply different edits stay as views and are never silently unified.

Reading order is known only when extraction is complete and supports one order.
Matching page numbers or similar headings do not establish order. A
correspondence that depends on an unknown order keeps that premise and stays
undecided. The flow engine's order selection is specified in §8.2.

## 8. Flow token stream

### 8.1 Tokenization

Mapped canonical text is split into maximal alphanumeric runs that are not CJK,
one token per CJK scalar, and one token per punctuation scalar. Whitespace is
dropped from identity but its source glyphs remain accounted for. An unmapped
glyph is one token with the encoded identity in §4.3. A token's `page` is the
page of its first source glyph, and `bounds` is the union of its source boxes.

Token identity is the exact canonical string (or the unmapped identity). No
numeric mask, case fold, width fold, or similarity transform changes it. A
numeric edit therefore stays visible.

### 8.2 Reading-order selection

Layout reconstruction can merge adjacent form rows into one oversized line.
The engine therefore builds three hypotheses per page from the same visible
glyphs: reconstructed layout order, content-stream paint order, and baseline-row
order.

Baseline rows use each glyph's baseline coordinate along the text direction's
normal; row gaps are relative to font size, so tall overlapping boxes on tight
CJK rows do not merge. Rows run top-to-bottom (right-to-left for vertical text)
and glyphs follow their baseline advance direction.

A page stays on layout order when layout and paint order have at least 0.9
order-preserving overlap. Otherwise the page is matched to counterpart pages in
the other document by an order-free, information-weighted bag overlap of page
tokens. The best match, every match within half of its overlap, and the pages
adjacent to the best match provide evidence. The three hypotheses are scored by
the weighted bigrams and trigrams they share with the counterparts' three
hypotheses, using a weight table built from the counterpart tokens alone.

A page whose best overlap is below 30% of the document pair's median per-page
best overlap has no counterpart and is ordered by intrinsic spatial coherence
only. This keeps an inserted or deleted page from borrowing evidence from a
structurally similar neighbor, and editing an unrelated page does not change an
untouched page's evidence. Layout order is kept only when it has at least as
much evidence and at least as much spatial coherence as paint order; baseline
order replaces paint order only when it has more evidence and at least as much
coherence. Otherwise paint order wins; exact ties keep layout, then paint order.

Pages where layout and paint order disagree are marked order-uncertain, and an
intrinsic per-page coherence score records how often consecutive chosen tokens
are spatially adjacent. Pages below 0.5 coherence are listed separately.
Neither marker changes the exit code. The engine never consults an external
reader for this choice, and it does not propagate layout issues into token
uncertainty: a token from an inferred order remains comparable.

### 8.3 Stacked bands

A page split by a vertical gap much larger than its line pitch can contain two
independent page-like regions. This happens with a page merge and also on real
pages with a wide whitespace band. Such a page is partitioned into stacked
bands when each band holds at least a fifth of the page's tokens and at least
two rows.

Each band's information-weighted token bag is compared with whole pages of the
other document. A band that reproduces one page at 0.9 overlap or more adopts
that page's chosen order, and the page is assembled band by band. The two bands
of a merged page can therefore keep two different hypotheses. Furniture
positions are measured within each band, so a header or footer that a merge
moved into the middle of the taller page keeps the relative position it had at
the old page edge. A page formed from one band, and a band whose counterpart is
itself stacked, keep the evidence-based whole-page choice.

### 8.4 Furniture

Repeated headers, footers, and page numbers are removed from the content stream
and compared separately by digit-masked text. Furniture membership is decided
jointly for the two documents: a line that qualifies on either side is
furniture on both. Alphabetic running headers and titles are removed at every
line-level occurrence, so a page merge or break stays invariant when the line
is no longer at the new page edge. Numeric-only lines use the joined
relative-position cluster.

Within one furniture family, digit runs that change between occurrences (page
numbers, counters) are masked to `0`; a run that is constant in every
occurrence (a year, form number, or version) stays literal, so editing it is
reported as a furniture change instead of disappearing behind the mask.
Families are separated by digit-run lengths so a page number and a form code
that share one full mask are not conflated.

Each side selects one relative-position cluster per masked line. The joint
comparison removes the union of both sides' selected clusters from both
documents. The union matters when a page insertion or deletion changes which
cluster meets the page threshold: the side that no longer qualifies still
removes the same lines as the side that does, so an untouched page cannot
become content on one side only. The digit mask is furniture evidence only; it
is never an alignment transform.

### 8.5 Image identity and paint gate

Image-only pages first compare an encoded identity: the same ordered image
XObjects, byte-identical encoded streams, pixel-affecting dictionaries, and
CTMs. Equal encoded identities resolve the page without decoding and become one
synthetic `⟦image-encoded:<hash>⟧` token. Different encoded bytes prove
nothing, so the page falls back to a complete decoded pixel inventory and an
`⟦image:<hash>⟧` token; the report names the resolving rule. A one-sided
image-only page is a deletion or insertion once its identity is available.
Pages with too many or too large images remain incomplete without OCR or a
partial inventory.

Flow text completeness requires both sides to be extraction-complete, glyph
conservation complete, and the non-text paint inventories equal. Every painted
path, image, inline image, and shading contributes a digest of its kind, exact
geometry or image-data identity, and page-space placement. Page numbers are
excluded so an identical page shifted within the document compares equal; page
crop and rotation are included. Artifact-marked paint is included. If the two
multisets differ, or an identity is unavailable, the text channel is incomplete
and reports `non-text paint differs; text drawn as paths or images was not
examined`, with bounded affected-page lists. Detected changes remain visible.
This deliberately covers table rules and boxes: without OCR or text-shape
recognition the engine cannot prove that a changed path is not outlined text.
An extraction issue about a page whose only possible text is inside painted
images is unexamined text evidence: it cannot allow exit `0`, but it does not
block an intact text detection.

### 8.6 Conservation and coverage

Every comparison-visible glyph is either referenced by a block token source or
appended by the content-stream fallback; per-page conservation counts
`used/visible` and is reported. A missing glyph is never silently treated as an
absent token.

The text channel's coverage reports flow's extracted token counts as both
discovered and compared; its completeness follows extraction completeness,
glyph conservation, and the paint gate. The multi-channel report separately
reports non-text channel coverage as discovered versus compared source
references per side. Flow does not report the removed engine's
`(same + changed) / comparable tokens` interval coverage.

## 9. Alignment and edit script

### 9.1 Token alignment

Alignment returns equal token pairs strictly increasing on both sides. Any such
pair list is a correct alignment; choosing a better one is a quality concern.
Unmatched gaps become replacements or delete/insert changes.

Small gaps use an exact information-weighted LCS with a cell budget. Rare words
carry proportionally more weight than near-universal tokens (dot leaders,
separators), so a shifted page number cannot outvote the title it belongs to.
Large gaps anchor on unique n-grams (up to 3-grams), extend every anchor to its
maximal equal run, and select the chain by total matched information rather than
run length. A gap without anchors falls back to bounded Myers-style bisection;
when its step budget is exhausted the gap stays unmatched rather than guessed.

Alignment prefers long equal runs and promotes same-gap runs into the equal
spine. After the weighted spine is final, a bounded plain LCS fills equal tokens
left inside two-sided gaps without removing a weighted match. Alignment never
rewrites token identity.

### 9.2 Script construction

The script builder classifies every non-spine token exactly once as delete,
insert, replace, or move. A replacement whose concatenated text is unchanged is
layout-only. A change carries no hidden edit: its old and new ranges are
contiguous and are recorded in document order.

### 9.3 Move rules

A move requires a repeated run long enough to identify (at least five tokens)
and carrying information weight; a run of minimum-weight punctuation or
separators contributes nothing and cannot be a move. A delete run and an insert
run with identical text at opposite ends of one accepted move are the remainder
of a rotation and are reported as one additional move even when shorter than
the minimum. Unrelated short equal runs stay delete/insert.

A same-page move on a dot-leader contents page that spans only low-weight or
numeric changes is promoted back into the spine, so a changed page number
becomes one small replacement instead of moves of the heading and dot leaders.

The engine evaluates three rules for both alignment strategies: the permissive
rule, the information-weight rule, and the contents rule. It keeps the script
with the fewest non-move changed tokens. A rule is skipped when it provably
cannot change the script, and reusable alignment segments are memoized so the
selected script and tie-breaking are identical to a full evaluation.

A post-pass reconciles equal tokens that sit on opposite sides of a move or a
shifted spine run: they become implicit equal tokens, and the shifted run
becomes one move. This removes needless delete/insert pairs from rotations and
page swaps.

### 9.4 Transposition rewrite

A transposition of two adjacent runs has two valid explanations. After the move
pass, if a `Move` of run `R` sits beside an equal spine run `E`, the engine may
rewrite the script to move `E` and keep `R` equal when all of the following
hold:

1. `E` intersects fewer reconstructed blocks in the old document than `R`.
2. `E` carries at least the information weight of `R`.
3. `E` has at least five tokens and at least the move information minimum.
4. The rebuilt spine is strictly increasing on both sides and the rewritten
   script replays.

The old reading order defines the structural unit that left its place; a block
split on the new side can be a line-wrap artifact. The information guard
prevents trading a substantive paragraph for a short heading, form label, or
separator run. Equal old-side block spans keep the alignment choice, and a move
without a mirrored equal run is untouched. The rewrite is deterministic: a
bounded number of rewrites is accepted per pass, each accepted rewrite strictly
reduces the moved run's old-side block span, and no benchmark path, fixture
name, page number, text, or fixed pixel threshold participates. This is a
quality preference, not a claim about author intent.

After the script is selected, a final classification pass looks for
same-row reading-order transpositions. For a move beside the equal run it
swapped with, the pass requires that, in both documents:

1. the two runs occupy disjoint horizontal columns;
2. their left-to-right order is the same;
3. their reconstructed visual lines share a row band: some line of one run has
   at least half of its vertical extent in common with a line of the other.

The line extents come from the token bounding boxes and the layout line breaks.
A run may be extended by its wrapped cell: within the run's horizontal column,
the nearest line above and below is added when it touches the run vertically
(relative gap at most one line height) and stays inside the column, for at most
two rounds. A same-row transposition is reported as `ReadingOrder`; a move
across rows or pages, a move whose runs overlap horizontally, a transposition
whose visual order also changed, and a run without complete bounds all stay
`Move`. The classification is a quality preference: two words swapped on one
line are indistinguishable from a reading-order flip, so the report calls the
geometry evidence, not author intent.

### 9.5 Replay verification

Every returned script is replayed against both token sequences before use.
Verification checks, in one pass, that:

- every old and new token is covered exactly once;
- covered old tokens are in ascending order;
- the uncovered old and uncovered new token texts are the same sequence;
- both sides of every move are identical;
- both sides of every reading-order change are identical; and
- both sides of every layout-only change satisfy the layout-only relation.

Any failure is an error; a partially verified script is never reported.

### 9.6 Change selection

A deletion, insertion, replacement, or move selects a detected difference (exit
`1`). A reading-order change and a layout-only change do not. Formatting
evidence never selects an exit code and never changes the content script. The same predicate
(`selects_difference`) is used by the CLI exit selection and the summary so the
two cannot diverge.

Confidence and ambiguity markers are computed per change but are descriptive
only. Coarse spans, repeated boundary runs, short moves, numeric-only spans,
numeric table neighbors, and order-uncertain pages lower confidence without
removing or moving a change.

## 10. Formatting evidence

### 10.1 Reasons

Matched token runs carry formatting-only evidence with these reasons:

| Reason | Evidence |
|---|---|
| `normalization` | Raw text or its normalization-event mask differs, including an in-block line-wrap rewrite |
| `block_structure` | The block-boundary pattern differs |
| `font_size` | Effective font size differs beyond a relative 1% tolerance |
| `position` | The tested segment's internal line and page break offsets agree and its lines are uniform baseline translations |
| `line_break` | Internal line-break offsets differ |
| `page_break` | Internal page-break offsets differ |

A break after the last token of an old block is a group boundary, not internal
evidence, so a page break between blocks is not a reflow. A run that moved to a
different page with an unchanged internal layout is position evidence.

Position is evaluated per segment: a break difference in one segment
suppresses position only on that segment and is reported as `line_break` or
`page_break`; it does not suppress position on another segment of the same
matched run. Font size is reported once per distinct changed size pair over the
whole matched run.

### 10.2 Coalescing and points

Entries are split at content changes and old-side blocks. Adjacent entries with
the same reasons, sizes, and pages are coalesced into one presentation entry. A
formatting entry must span at least one matched adjacency: a one-token LCS
match inside rewritten text is a point, not a formatting span, and is dropped.

A one-token point that carries only position may merge into a neighboring entry
when both sides have the same non-position reasons and sizes; the merged entry
keeps `position` only when both sides had it, so a point's position is never
promoted into span evidence. Two multi-token segments with different position
status stay separate.

Formatting reasons are approximate and are not interchangeable with the removed
engine's buckets.

### 10.3 Agreement with historical formatting captures

Formatting evidence is not an oracle compared metric. Where historical captures
of the removed engine exist, a word-level check (`\w+` words, ±3-page window) on
the 62 pairs finds a flow entry with the same formatting reason for 84.2% of
2,763 `position` entries, 88.4% of 43 `line_break`, 56.5% of 108
`normalization`, 57.3% of 358 `font_size`, and 3 of 3 `block_structure` entries.
The strict same-page check gives 83.6%, 83.7%, 52.8%, 56.7%, and 3 of 3.

The removed engine is not ground truth. Auditing the misses against PDF glyphs
found no actionable normalization or font-size rule miss: of 153 missed
`font_size` entries, 18 are identical or below the 1% tolerance, 133 are
reported by flow under another reason or inside a content change, and 2 are not
a stable matched span. Of 47 missed `normalization` entries, 41 are reported
under another reason or inside a content change and 6 are not a stable matched
span. The `position` misses were dominated by the former whole-run break
suppression; the per-segment rule addresses that root cause. Flow's unit is
the maximal matched run rather than a block-grouped bucket, so one pair can
produce many more entries than the historical capture.

## 11. Non-text channels

### 11.1 Shared correspondence solver

The non-text channels feed a shared correspondence solver with a versioned
lexicographic objective: source-backed scoped identities (structure IDs, field
names, row/column keys), then source-backed literal content, then inferred
structural correspondence, then inferred literal content, then other inferred
proposals.

- Source-only mandatory correspondences are fixed before inferred candidates
  are enumerated, so an inferred tie-break cannot override a source alignment.
- Alternative views of the same evidence compete for ownership; selected
  members must admit one shared partition, and no evidence is consumed twice.
- The solver descends through matched containers with bounded depth and scope
  counts and records each child scope's parent correspondence.
- Exact text may match one ordered view to several (1:N or N:1) without changing
  tokens or inventing separators.
- A bounded search that runs out of budget is localized to its dependent
  component; independent correspondences still proceed.
- Equal optimal solutions stay ambiguous, and an exhausted component emits no
  mandatory correspondence. Enumeration, validation, scoring, and discarded
  work are charged.

A supplier (text, heading, typed identity, label, row/column name, neighbor,
visual feature, footer rule) proposes candidates but never emits a change
alone. A solver-selected result that depends on an inferred tie-break is
reported as inferred. Correspondence is conditional support for a local exact
claim, not a proof of the author's edit history.

### 11.2 Visual

Embedded raster images, including those in Form XObjects and with intrinsic
alpha masks, are decoded at their original resolution and hashed with SHA-256
over width, height, and normalized RGBA8 samples. Lossless recompression or
object renumbering alone does not change the hash; lossy recompression can.

- Equal hashes are matched as a multiset, independent of page and draw order.
- Remaining images at a unique identical page and placement are `changed`
  (inferred).
- Residual images become `added` or `removed` only when both inventories are
  complete and no unmatched rival remains; ambiguous replacements and decoding
  failures stay unresolved.

Pages are rendered at 72 dpi against white and retained for review. Page
rasters are context: composited-page proposals are disabled, so a retained page
raster is a discovered visual source that no comparison consumes. Every
rendered page therefore leaves the visual inventory incomplete. Placement
changes, clipping, blend effects, and vector graphics are outside pixel-hash
equality. Stencil images, pattern/Type 3 images, annotation images, and
graphics soft masks remain unresolved.

### 11.3 Forms

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
stored-value/display agreement, and XFA remain unresolved. A changed stored
field value that flow does not compare as text still produces a `text_mask` for
that local value pair.

### 11.4 Relations

Typed relationships (row/column membership, labels, captions, references, and
source-structured containment and order) are compared across endpoint
correspondences established by the other channels. An absent edge requires
complete relationship inventories on both sides; ambiguous endpoints and work
limits stay unresolved. Native layout containment and order are not semantic
relationship changes.

Native PDF link extraction and automatic link/footnote target interpretation are
not implemented, so relations is usually incomplete for real documents. The
exception is an input pair where both catalogs were read and provably contain no
native relation source: that empty inventory is complete and produces no
relation changes.

### 11.5 Tables and tagged structure

- **Ruled grids** supply an inferred table view; each cell is reconstructed
  independently with column identities from the first row and row identities
  from the first column. Values are compared within those identities, so equal
  numbers in different rows never establish correspondence. Changed headers may
  receive inferred candidates from shared child or neighboring identities.
  Grids are limited to 1,024 cells per candidate.
- **Borderless tables:** uniquely located row/column labels from a counterpart
  table can propose a native partition when borders are missing. Target geometry
  supplies separators and reconstructed axis labels must agree exactly. This
  requires one table per page per input, unchanged unique row labels, a shared
  top-left header, horizontal text, and non-crossing cell geometry. JSON
  `table_refinements` records the counterpart table and its evidence.
- **Keyed cells** without an explicit item key use scoped row/column identities;
  missing, duplicate, or competing axis evidence leaves the scope unresolved.
- The original block partition stays available as an alternative view, so
  native text remains comparable when only one side admits a table.

Bounded native structure trees are imported with roles, parent membership,
declared order, and byte-exact structure IDs. Marked-content IDs and explicit
references connect tagged paragraphs and cells to original glyphs, including
inside Form XObjects. Tagged tables with stable IDs preserve cell identity
across value swaps, reordered drawing commands, and an inserted cover page.
Missing, repeated, or incomplete marked content leaves unresolved structure
without deleting native text.

### 11.6 Obligations and exit attribution

An obligation records why a claim could not be established. It is unexamined
evidence on its own channel, not a contradiction. Relation binding, membership,
and endpoint gaps are unexamined relationship evidence; relation graph and
work-budget failures are broken. Structure-ID key presence belongs to
relations/structure, and field-name key presence to forms.

A key-presence obligation records a missing inventory, unnamed or duplicate
members, a retained rename alternative, or an unavailable acquisition path, so
it is unexamined on its channel. An exhausted raw key search clears key
exhaustiveness and is cross-channel broken. An exhausted scoped key search, or
a scoped `work_budget` obligation, is broken on every channel that owns a claim
(forms for field names, relations/structure for structure IDs); other scoped
obligations are unexamined on their claim's domain. A scoped obligation without
a claim index marks every claim domain unexamined and is never a blanket
cross-channel reason.

Text views remain correspondence context for relationships and structure. A
component that mixes text and non-text sources is never superseded by flow
ownership. `flow_owned_text` counts the positions where flow ownership replaced
shared-evidence text comparisons, with `unchecked_obligations` per skipped
reason; it is absent when none were recorded.

## 12. Resource limits and process isolation

Every PDF is untrusted input. Limits are finite, configurable only upward, and
enforced at the boundary that spends the resource.

### 12.1 Parser and extraction defaults

| Boundary | Default |
|---|---:|
| input bytes | 256 MiB |
| objects | 1,000,000 |
| recursion depth | 128 |
| decoded stream bytes | 64 MiB |
| total object-stream bytes | 256 MiB |
| pages | 100,000 |
| glyphs | 5,000,000 |
| Form depth | 32 |
| nesting depth | 64 |
| operators | 5,000,000 |
| stream invocations | 5,000,000 |
| total decoded bytes (extraction) | 512 MiB |
| operand stack | 4,096 |
| array elements per operand | 65,536 |
| operand nodes | 10,000,000 |
| fonts | 100,000 |
| CMap entries | 1,000,000 |
| CID width entries | 65,536 |
| string bytes | 64 MiB |
| vector lines | 5,000,000 |
| external font identities | 1,024 entries; BaseFont 127 bytes; identity 4,096 bytes |

An explicitly lower limit must fail at the same charging boundary. Defaults are
retuned only from reproducible fixture or corpus evidence; a limit is never
removed to make one document pass. `--limit-scale` multiplies count limits
(glyph, operator, invocation, decoded bytes, operand nodes, fonts, CMap, CID
width, string, vector line, and the shared footer-matching work limit), the
render page and per-document CPU budgets with their wall safety stops, and the
image worker CPU budget with its wall stop by a finite factor of at least 1. It
does not change structural bounds, parser limits, image pixel or occurrence
limits, retained-raster budgets, or layout thresholds.

### 12.2 Child processes

**Native acquisition** runs in bounded children: 2 GiB address space, 30 s CPU,
35 s parent deadline, 1 MiB request header, 128 MiB response, and PDF input up
to 256 MiB. Page metadata and form fields are acquired separately from glyphs
and tags, so a content-extraction failure can leave forms and rendering usable.

**Page rendering** runs in a child per page: a 2 GiB memory ceiling, 10 s of CPU
per page (the deterministic page budget; measured median 0.032 s, p99 0.282 s,
p99.9 1.383 s, and max 3.019 s over 4,654 rendered pages of the 36-pair channel
panel), a cumulative child-CPU budget of 120 s per input document (measured max
27.31 s and p99 27.31 s over 72 documents), 120 s wall per page and 600 s wall
per document as safety stops, 8 million pixels per page, and 256 MiB retained
RGB per input. The page that crosses the document CPU budget is retained; later
pages are a `resource_limit` outcome with reason `document CPU budget exhausted;
this outcome is deterministic for this input` and no raster. A page that
exhausts its CPU budget is also a resource-limit outcome with no raster. A wall
safety stop is a distinct, explicitly load-dependent resource-limit reason. The
document wall stop is five times its CPU budget, so at a 25% CPU quota the full
CPU budget (about 480 wall seconds) still expires first. `--limit-scale`
multiplies both CPU budgets and both wall stops.

Before rendering, the child scans the page's image XObjects and decoded content
streams. One image above 64 million declared pixels, including an inline image
declared with `/W`/`/H` or `/Width`/`Height`, is a page-scoped `resource_limit`
instead of a renderer abort. The XObject scan follows Form resources with depth
8 and 4,096-entry bounds, reads dictionaries only, and never decodes an image
stream. Inline-image scanning reuses the content parser, reads only `BI`
dictionaries, and skips `ID`..`EI` data.

**Image decoding** runs in a child: a 2 GiB memory ceiling, 60 s of CPU per
worker call, a 300 s wall backstop, 10,000 occurrences, 16 million pixels per
image, 64 million base-image pixels per input, and an 8 MiB response. Encoded
image identity is bounded by 256 MiB of encoded bytes, depth 8, and 4,096
nodes. The 36-pair panel has 56 successful document calls with median 0.191 s,
p99 13.909 s, and max 13.909 s; 16 calls that stop at an inventory limit before
responding take 0.24-3.56 s, so 60 s is a 4.31x margin over the observed
maximum. The CPU and wall budgets scale with `--limit-scale`; exhausting either
is a typed `resource_limit` outcome, never a complete or empty inventory.

The parent validates input hashes, page identity, acquisition roles, and
aggregate limits before comparison. Passwords travel through private stdin and
never appear in responses. Bounded workers support Linux and macOS; on other
platforms the affected acquisition is reported as unsupported.

Linux enforces the worker memory ceiling with `RLIMIT_AS`. macOS accepts
`RLIMIT_AS` but does not enforce it, so worker subprocesses arm a counting
global allocator at the same 2 GiB limit. An allocation above it terminates the
worker with the limits exit code, which the parent maps to a typed
`resource_limit`; the comparison process never arms the allocator.

### 12.3 Report, review, and agent budgets

- A review bundle is bounded to 512 MiB, 1,024 page previews, and 20 million
  source-location visits; a failure may leave an incomplete directory without
  `index.html`.
- An agent review bundle is bounded by the 512 MiB cap and the case budget
  (default 4,096; `--agent-case-budget` 1–131,072). Case bodies are spooled, so
  the resident listing index is independent of the case count. A larger plan is
  published as numbered part bundles under one set manifest; every part is
  separately bounded by the 512 MiB cap.
- A text-envelope request is bounded to 1,000,000 work units, 262,144 retained
  cells, 1,048,576 output bytes, 1 MiB input, 1,024 pairs, and 32,768 tokens
  per side; hard ceilings are 32,000,000 work units and 8,388,608 output bytes.
  A refusal returns `unavailable`, never an empty envelope.
- Reports are published atomically and never overwrite an existing path. A
  second publication failure leaves the first artifact in place.

### 12.4 Determinism

Reports do not contain wall-clock time except the documented
`comparison_wall_time_ms` (multi-channel) and trace `duration_us`, both
excluded from golden comparisons. Render and image-decoding CPU budgets are
deterministic in child CPU time; only the wall safety stops depend on machine
load, and their expiry is a distinct reason. Rendering is also bounded by the
deterministic retained-raster budget. Report bytes for the same input, binary, arguments, and
limits are otherwise stable, and the child-cache and in-process paths produce
byte-identical reports.

## 13. Validation and benchmarks

### 13.1 Benchmark generator and mutation engine

`pdfbench` generates PDFs from a canonical YAML document through two
project-owned writers (`lopdf-tj` and `classic-xref-tj`). The generated matrix
has 24 cases and 48 records. It covers repetition, prose reflow, numbered
requirements, pagination churn, footnote-like paragraphs, section-labeled
moves, and two-column reflow. External renderer output is vendored as fixtures;
renderers are not executed in CI.

Mutations split into semantic mutations (which must produce an expected change)
and rendering mutations (which must not produce a content change):

- Semantic: text replace, insert, delete, number replace, paragraph insert,
  paragraph delete, paragraph move (same or cross section).
- Rendering: font size, margin, page size, line height, line break, page break,
  column change (selected section).

The evaluator compares reported flow changes with the expected change by exact
kind and exact changed text. A pass requires every expected event to be matched;
a candidate or inferred result is never counted as an exact acceptance.

`pdfbench flow-verify` runs the flow engine against fixtures with known edits
(generated acceptance cases under both renderers and vendored Typst/Tectonic
pairs). `pdfbench flow-metamorphic` builds exact ground truth on real PDFs by
page-tree surgery: identity, page deletion, insertion, swap, duplication,
single-token text edits, page merges, and harmless font-program resubsetting.
Every planned case runs; a timeout is a failure, not a skip.

### 13.2 Acceptance matrix

`pdfbench verify` runs the 48-record matrix. On the current tree, 48/48 records
pass: 28/28 expected events and 2,216 expected changed tokens are matched, with
8 false-positive tokens (token precision 0.996, recall 1.000). Layout-only
cases report zero changed tokens. The five release cases (plus reverse deletion)
also pass on the vendored Typst fixtures under the default command and under
`--native-text-only`; the default command is judged by the exact change set, not
by exit `0`.

### 13.3 Real-world panels

The frozen corpora use public revision pairs with reviewed manifests. Two
panels are measured:

- **Native 64:** `--native-text-only` over 64 pairs with recorded
  `--limit-scale`, one pair at a time. Current result: 1 complete text
  comparison, 62 detected-but-incomplete, 1 broken evidence.
- **Channels 36:** the default channel command over 36 pairs. Current result:
  35 exit `1` with unexamined evidence, 1 exit `3` with broken text evidence,
  none complete.

Every pair produced a report without timeout or OOM in the final runs.
Completion counts are not accuracy or recall figures. Formatting agreement with
historical captures is reported in §10.3. Holdout pairs, when used, are newly
frozen and unused; the panel denominator and scope are not changed after tuning.

### 13.4 Metamorphic, property, and fuzz testing

Property tests cover normalization idempotence, line reconstruction partitioning
every glyph, and block reconstruction covering every line while preserving
normalization order. Metamorphic tests cover page deletion/insertion/swap/
duplication and font resubsetting with constructed exact truth.

The fuzz workspace has 11 targets: parser entry, CMap parser, content-stream
parser, font decoder, CFF built-in encoding, canonical YAML, glyph extraction,
layout pipeline, graph pipeline, text comparison, and native evidence. Each
target checks that malformed input does not panic, hang, or allocate without
bound and that successful paths satisfy their invariants. The M8g session ran
28,394,104 executions over 10 minutes per target with no crash, timeout, or
OOM. Fuzzing is nightly-only and is not part of the stable gate; a crash-free
finite session is not proof for all inputs.

**Parser backend conformance** feeds the same fixture to the default backend
and an alternate parser and compares the neutral result: page order, reference
resolution, resource inheritance, decoded content-stream bytes, objects inside
object streams, and the latest object of an incremental update. Library debug
strings and object layout order are not compared.

**Extraction differential testing** compares `ParserBackedGlyphSource` with an
independent position-aware extractor or interpreter: decoded text, glyph count,
text order, bounding boxes, and baselines, with documented tolerance, plus a
glyph overlay for human inspection. A plain-text-only extractor is a text
oracle, never a geometry oracle. Agreement between tools that share the same
underlying parser is not independent evidence; a hand-written fixture or a
separate implementation must confirm the claim. The curated `pdf-oxide`
0.3.77 oracle under `fixtures/extraction-conformance/` checks all 158 mapped
horizontal glyphs of the vendored Japanese Typst fixture against an independent
parser; unmapped glyphs, rotated text, and a corpus-wide oracle remain open.

### 13.5 Quality gate

Before a commit:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --lib
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
python3 -m unittest discover -s benchmark/realworld -p 'test_*.py'
cargo check --manifest-path fuzz/Cargo.toml --bins
cargo run -p pdfdelta-bench -- verify
```

The `--all-features` clippy and library-test runs compile the fuzzing entry
points on stable Rust. They do not run libFuzzer. A benchmark matrix or
extraction-conformance change additionally runs the focused
`cargo test -p pdfdelta-bench --test bench_matrix` or
`cargo test -p pdfdelta-bench --test extraction_conformance`.

Clippy enforces the `all`, `pedantic`, `correctness`, and `suspicious` groups.
`Cargo.toml` documents the intentionally allowed pedantic lints. New code
satisfies every other pedantic lint.

A large build, test, or benchmark workload is capped at 6,000,000,000 bytes
across its process tree, run sequentially, and started with
`CARGO_BUILD_JOBS=1` and `RUST_TEST_THREADS=1`. OOM, timeout, and resource-limit
outcomes are recorded, not hidden by raising the cap.

## 14. Dependencies and development principles

### 14.1 Dependencies

Production dependencies are those in `Cargo.toml`/`Cargo.lock`.

| Purpose | Current choice | What is used |
|---|---|---|
| PDF object parser | `lopdf` 0.45.0 (default features off) | objects, xref, object streams, incremental updates, raw streams |
| Rendering | `hayro` 0.7.1 (CLI), `hayro-cmap` 0.1.0 (core) | visual channel, CID CMap reference data |
| CLI | `clap` 4, `clap_complete` 4 | argument parsing, completions |
| Serialization | `serde` 1, `serde_json` 1 (`float_roundtrip`), `serde-saphyr` 1 | JSON, YAML fixtures |
| Unicode | `unicode-normalization` 0.1, `unicode-segmentation` 1, `unicode-bidi` 0.3 | NFC, grapheme/word segmentation, bidi safety |
| Other | `flate2` 1.1, `sha2` 0.11, `smallvec` 1, `rayon` 1, `base64` 0.22, `png` 0.18, `rustix` 1 (process, time; Linux/macOS) | stream decode, hashing, buffers, parallel work, workers |
| Test only | `proptest` 1, external extractors, external renderers | property tests, conformance oracles, fixture generation |

Reference points: `pdf-extract` as a text oracle, `pdf-inspector` as a
position-aware extraction reference, `printpdf` for ToUnicode/Type0/simple-font
behavior, `similar`/`imara-diff` for edit-script comparison, `pdf-writer` for
low-level fixture creation, and Typst/Tectonic/krilla as independent renderers.
Abstraction levels are not mixed, and library-specific types do not leak
upward. The final parser backend decision is made by capability fixtures.

### 14.2 Development principles

A new feature is added only when it explains which current benchmark failure it
fixes. Examples of justified work: a block-segmentation error causes false
positives, so alignment recovery is extended; a parser backend cannot handle a
required object stream and neither the adapter nor upstream can fix it, so a
backend replacement is evaluated. Examples of unjustified work: adding an
embedding model because it may help later, writing a custom PDF object parser
from the start, using LSH in final matching without correctness validation, or
adding a document-name branch to make one PDF pass.

The order is always:

```text
reproduce a failure on a real PDF
-> classify the layer (parser / extraction / layout / alignment / diff / correspondence)
-> add the smallest rule or backend change
-> confirm against the exact oracle and benchmark
```

Preserving evidence, reversible structure, and explicit unresolved reporting
takes priority over short-term detection counts. A document-family rule stays a
candidate until independent evidence supports it.

### 14.3 Not in the first increment

New OCR execution, LLMs, embeddings, semantic similarity models, complete
visual pixel diffing, advanced table recognition, general AcroForm support,
annotation diffing, full PDF 2.0, GPU, WASM, a web UI, performance optimization,
and a custom PDF object parser are outside this increment. They are not
permanently forbidden; a future evidence provider must follow the contracts in
§2.1 and §11.

## 15. Specification history

This section records why the specification changed. Earlier revisions are
available from the repository history.

### 2026-11-26: streaming agent-review plan

- Agent review plans stream case bodies and contexts through a private
  temporary store; the resident plan keeps only a fixed-size listing index.
  Complete NIST export peak RSS fell from 3.91 GB to 2.89 GB, and a diagnostic
  4x plan of 80,888 cases peaked at the same value.
- Bundles are byte-identical to the whole-plan assembler for the acceptance
  cases, `arxiv-bert`, and the default and complete NIST exports.
- Part batches and the 512 MiB cap close parts deterministically; split
  sub-cases stay consecutive across a batch boundary.

### 2026-11-24: relation absence, inline-image preflight, split review exports

- Complete/empty relation inventories when both inputs prove no native relation
  source exists.
- Inline-image pixel preflight shares the content parser's `BI` scanning.
- Static review source indexes return to the nested-gzip shape for size while
  the reader keeps accepting single and legacy double-gzip bundles.
- Agent review can publish numbered part bundles and links oversized cases as
  `agent-review-split/v1` sub-cases; the case budget is a total cap raised to
  131,072.
- Full reports retain a bounded source-cache read window and reuse serialization
  buffers; the temporary cache remains the source of the extra wall time over
  the in-memory M8e path.

### 2026-11-23: rewrite for the single flow engine

- Replaced the removed proof-engine design with the token-stream flow engine:
  tokenization, reading-order hypotheses, furniture, alignment, move and
  transpose rules, replay verification, and formatting evidence.
- Updated output contracts to the current flow-native schemas: native compact
  artifact 3 / schema 13, native full artifact 2, multi-channel report 3, trace
  version 1, static review manifest 3. Removed engine positions are absent
  rather than retained as empty compatibility fields; historical artifacts
  remain readable.
- Updated the exit contract to per-channel verdicts: exit `1` may coexist with
  incomplete evidence, and exit `0` requires every selected channel to be
  examined. The default channel set is unchanged.
- Updated the non-text evidence model with key-presence obligations, scoped key
  attribution, single-gzip review sources, and the deterministic render CPU
  budgets and image preflight.
- Replaced the stale proof-era measurements with the current acceptance,
  formatting-agreement, and completion figures.

### 2026-09-20: restored 15-chapter Japanese structure (v5)

- Merged the English comparison contract v5 into the restored chapter layout,
  integrating evidence channels, the shared correspondence solver, local
  failures, and safety boundaries.
- Set visible PDF-native text as the primary evaluation and excluded new OCR,
  image semantics, and path text.

### 2026-09-08: comparison design for unknown PDFs (v4)

- Separated established changes from tentative candidates, excluded candidates
  from established coverage, and made incomplete comparisons exit `3` by
  default.
- Separated candidate generation, correspondence decisions, and exact diff, and
  required independent justification for exact claims.

### 2026-08-24: unified diff human report

- Projected the exact diff into a contextual unified diff for the text report
  without changing machine-readable results.

### 2026-08-21 and 2026-08-20: passwords, limits, fonts, and normalization

- Added per-side password files and kept password text out of arguments,
  reports, and traces.
- Added the external CID font identity assertion, Type 3 identity limits,
  custom Type0 CMap restriction, and CID FontBBox fallback.
- Defined canonical text, soft line breaks, and normalization events as
  pdfdelta responsibilities, with Unicode crates limited to NFC and
  segmentation primitives.

### 2026-08-19: initial specification

- Defined goals, scope, architecture, data model, pipeline, roadmap, benchmark
  approach, and resource-limit policy.
