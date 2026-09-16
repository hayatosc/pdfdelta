# pdfdelta

`pdfdelta` is an early-stage Rust CLI for comparing PDF content and relationships while retaining the original evidence and unresolved regions.

## Why pdfdelta?

A PDF usually stores drawing instructions rather than paragraphs, sentences, or a stable reading order. Extracting plain text and running a conventional diff therefore turns harmless line wrapping and page breaks into changes. Comparing rendered pages has the opposite problem: a new font, margin, or pagination can make nearly every pixel different even when the wording is unchanged.

That noise is especially costly when reviewing contracts, policies, reports, and other documents with an audit trail. A deadline changing from `10 days` to `20 days` must remain an exact, reviewable replacement, while repagination around the same sentence should not hide it among hundreds of false positives.

`pdfdelta` is intended to bridge that gap. It retains glyph geometry and PDF provenance, reconstructs imperfect document structure, aligns corresponding content despite layout drift, and performs the final text diff exactly. When the available evidence is insufficient, it reports unsupported or unresolved regions instead of claiming that no change exists.

## Status

The default command now selects text, visual, form, and relationship channels and writes version 2 JSON reports. A missing or unexamined channel keeps the comparison incomplete, including image-only PDFs with no native text. On Linux, the CLI retains page rasters using the Rust hayro renderer and compares them through the common graph and solver. OCR is not implemented. Saved AcroForm text, choice, and button values are extracted through the neutral PDF facade and compared by field name; malformed values remain unresolved with their available raw evidence. General stored-value/display agreement and XFA remain unresolved.

Button fields retain declared widget appearance-state names and widget object references alongside their saved values in the JSON `form_fields` evidence. Without an export-option mapping, a saved selection that disagrees with the declared active widget states produces a field-local unresolved issue; independent fields still compare. Radio groups may contain inactive `Off` widgets. Missing, invalid, or over-budget states remain unknown. This checks PDF declarations, not rendered appearance: export-option interpretation and state-selected appearance rendering remain pending. The [W3C form-field example](https://www.w3.org/WAI/WCAG22/Techniques/pdf/PDF12) illustrates the separate field-value and widget-appearance declarations.

Widgets with direct normal appearance streams now retain canonical page rectangles, stream/object references, and source-checked crops of composited page pixels. The forms channel compares both stored values and linked widget crops through the common solver. JSON distinguishes `value_changed` from `rendered_region_changed` and records each crop's source page raster and pixel rectangle. A unique field name supplies the correspondence premise; the crop does not prove what text is visible or that the saved value is displayed correctly. Missing/contradictory page membership, unsupported widget flags, state-selected appearances, and duplicate widget identities remain unresolved. Multiple widgets are retained independently.

Actual-PDF controls vary saved values and widget colors independently, move widgets, and insert a cover page. Value and pixel changes remain separate operations; movement and the cover-only change produce zero operations. Disjoint changed widgets produce two region changes. Overlapping widget crops compete for the same pixels and remain ambiguous rather than producing duplicate changes; crops also conflict with their full-page rendering. Raster byte budgets include the retained crops, whose geometry and pixels are checked against the source raster.

The common solver can descend through matched graph containers with bounded depth and scope counts. Reports retain each child scope's parent correspondence, propagate inferred parent interpretations, and prevent competing containers from consuming the same descendant evidence. Exact text can also match one ordered view to several views, or vice versa, without changing tokens or inventing separators. Local results retain every old/new member and original token source. General structure and split/merge discovery, structure-model integration, and full document coverage remain incomplete.

The relation engine compares explicitly supplied reference edges, but native PDF link extraction and automatic link/footnote target interpretation are not implemented. Structured annotation evidence preserves its target; the built-in graph provider currently compares only annotation text. Comparing that text does not establish coverage of its reference relationship.

Complete rectangular ruled grids now supply an inferred table view. The builder
reconstructs each cell independently, including its internal line wraps, and
proposes column identities from the first row and row identities from the first
column. The common solver compares values within those row/column identities,
so equal quantities in different rows cannot establish cell correspondence.
Changed header identities can receive inferred correspondence candidates from
shared child identities or retained neighboring identities. Accepted column
correspondences constrain cell candidates inside matched rows; cell values do
not supply those axis correspondences. Missing context leaves affected cells
unpaired. Exhausted structural searches remain unresolved while independent
source-backed text correspondences can proceed.
Native glyphs, border references, and the original block partition remain
available; the graph validates equal source coverage for the two partitions.
The shared solver considers archived original blocks alongside table candidates,
so native text remains comparable when only one side admits a table view.
The archive must exactly contain a declared partition; other unknown containers
retain their scope boundaries. Archived literal and similarity candidates enter
the optional search, preserving independent source comparisons on exhaustion.
Partition-dependent correspondences remain inferred. Within inference, typed
identities and structural context precede literal matches, which precede text
similarity; the report records this versioned correspondence objective.
The solver records source-only mandatory correspondences separately. If an
inferred premise resolves an otherwise ambiguous source correspondence, the
selected correspondence and its dependent local result remain inferred.
For oversized components, bounded searches of the highest remaining objective
class can force correspondences and remove incompatible lower-priority rivals.
The residual search keeps those ownership and partition constraints within the
same state budget; unfinished prefixes do not justify pruning.
Declared archives also supply exact 1:N/N:1 candidates along retained order
edges. Groups preserve their source references, cannot mix incompatible
partitions, and cannot invent separators or missing order. General prose-boundary discovery and arbitrary cross-scope partition
refinement remain incomplete.
When suppliers expose alternative views within a correspondence scope, the
common solver requires selected members of each group to admit one shared
partition. Disjoint source
ownership alone does not permit mixing incompatible partitions. Shared views
can belong to several partitions; the solver checks the whole selection and
localizes an exhausted partition search to its dependent component.

Before comparison, uniquely located row/column labels from a counterpart table
can now propose a native glyph partition when borders are absent. Both sides'
templates are captured before refinement; cell values and edit cost never select
or score the partition. Target geometry supplies the separators, and reconstructed
axis labels must agree exactly. The shared grid installer checks complete native
block coverage and retains source tags as separate overlapping views. Original
blocks remain alternative partitions for the common solver. JSON
`table_refinements` records the counterpart table, its source dependencies,
target anchor glyphs, and bounded-search exhaustion. This currently requires a
table on one page per input, unchanged unique row labels, a shared top-left
header, horizontal text, and non-crossing cell geometry. One changed column
label may be read literally from a single residual native header view; competing
views keep the original partition. Changed row labels and multiple missing
column labels are not reconstructed. The left extent includes all row labels,
so representational rounding in a body label cannot place it outside the grid.

These grid interpretations and their dependent operations are marked inferred.
Missing borders without supported counterpart axes, crossing glyphs, empty cells,
duplicate labels, unsupported text directions, and optional-view budget exhaustion retain the original layout.
Grid work is bounded by the graph budgets and 1,024 cells per candidate. This
does not establish arbitrary table semantics, merged-cell interpretation, or
complete relationship coverage; a first-row/first-column header interpretation
can still be wrong even when the grid geometry is clear.

The implementation uses Rust and makes no external analysis API calls. Rendering runs in a child of the same executable, with a 2 GiB address-space limit, five-second page deadline, thirty-second document deadline, eight-million-pixel page limit, and 256 MiB retained RGB budget per input. Pages render at 72 dpi against white; backend identity and rendering profile are retained. Native and rendered page object identities, page counts, and dimensions must agree. Other platforms and password-assisted rendering currently remain unsupported. Renderer warnings, incomplete annotation support, and unknown content-region interpretation keep visual coverage incomplete even when pixels are available. Pixel changes are inferred page-rendering differences, not recognized text or established content changes.

Image-only and scanned pages retain rendered pixels for visual comparison. The CLI does not recognize their text, load recognition models, or check rendered text against saved form values. Their text coverage remains incomplete. Native PDF text and stored form values continue to compare independently.

Unkeyed paragraphs and other ordered text roles can now propose nonidentical one-to-one correspondences using literal trigram similarity. These correspondences remain inferred even when their local character masks are exact. The source-only optimum protects mandatory source matches before inferred candidates are enumerated; existing inferred tie-breaks cannot freeze a source alignment. Text-search truncation retains independent field changes and protected literal anchors. General nonexact split/merge discovery remains pending.

A separate, bounded review path can suggest native paragraph merges and splits as non-owning C comparisons. It ranks adjacent groups using literal word pairs, preserves every member's text and source references, and requires a non-whitespace source-count difference. It emits no ordered character mask and changes neither accepted correspondences nor coverage. This heuristic currently targets sentence-shaped prose with capitalized starts and period endings; incomplete fragments, ambiguous starts, source conflicts, and unsupported edge matches are withheld. Correspondence and reading order remain unproved, and the search is not exhaustive.

Cells without an explicit item key now use scoped row/column identities as correspondence candidates. Both flat tables and cells nested under rows are supported when axis keys and memberships are supplied. Missing, duplicate, or competing axis evidence leaves the affected scope unresolved; equal cell values and concatenated cell text cannot substitute for membership. Model-derived axes keep cell changes inferred through the common solver. This does not yet extract table keys or memberships from arbitrary PDFs.

Typed graph relationships now compare across the endpoint correspondences retained from all visited scopes. Row/column membership, labels, captions, references, and source-structured containment/order retain their edge evidence and correspondence dependencies. An absent edge requires complete relationship inventories and observed graph structure; ambiguous endpoints and work limits remain unresolved. Native layout containment/order does not itself count as a semantic relationship change. Native table extraction and general row/column insertion/deletion are still pending.

The CLI imports bounded native structure trees, with roles, parent membership, declared order, and byte-exact structure IDs. Inline marked-content IDs and explicit marked-content references connect tagged paragraphs and cells to the original glyphs, including Form XObject invocation scope. These are alternative views over existing evidence, so the common solver prevents duplicate consumption by layout and tag views. Tagged tables with stable structure IDs preserve cell identity across value swaps, reordered drawing commands, and an inserted cover page. Missing, repeated, or incomplete marked content leaves unresolved structure without deleting native text. Named property lists, custom role maps, table-axis interpretation, and object-reference bindings remain unimplemented; tag discovery does not establish complete relationship coverage.

The native graph and retained text adapter share a one-sided catalog/form footer provider. It proposes source-backed terminal-line views, including geometry-supported joins of split blocks; page numbers are metadata rather than identity keys. Footer candidates enter the common solver alongside other providers. This bounded document-family rule does not establish general footer recognition.

Rendered regions with the same rendering profile and sample grid can propose visual correspondences regardless of page number. Their similarity scores select candidates only: resulting pixel changes are reported separately as `inferred_changes`, do not identify changed characters, and do not establish complete visual coverage. The versioned solver objective prioritizes source-backed scoped identities, then literal content, then inferred supplier scores. Literal fragments cannot displace a competing keyed item merely by matching more unchanged pieces. Repeated images may remain ambiguous. If visual candidate search stops at its resource limit, independent field comparisons can still proceed.

The default human-readable report includes changed values/text, field or region labels, page numbers, inferred correspondence labels, mask counts, and unresolved reasons. Long values and report entry counts are bounded with explicit truncation markers; control characters are escaped for terminal output. Full values, masks, and dependency records remain in JSON.

Add `--review DIRECTORY` to the common evidence pipeline to write a static review bundle:

```bash
pdfdelta old.pdf new.pdf --channels text --review review-output
```

Open `review-output/index.html` locally. The bundle includes byte-exact source PDFs,
lossless retained page previews, complete comparison JSON, and source IDs with raw
glyph evidence, geometry, provenance, graph views, rendering profiles, and warnings.
Every reported change links to its JSON location and available source pages/regions.
Full excerpts distinguish strict changes (A), non-owning corresponding-range content
changes (B), and inferred comparisons (C) using labels and border/underline styles.
Conditional masks remain non-owning, and unmarked text remains context. These
categories do not combine into strict recall; a literal representation difference
need not change the visible words. No script or service is required.

The destination must not exist. Output is bounded to 512 MiB, 1,024 page previews,
and 20 million source-location work units; failure may leave an incomplete directory
without `index.html`. Review output does not change the comparison result or exit
code. It is unavailable with `--native-text-only`.

The established native-glyph pipeline remains available with `--native-text-only`, including its version 11 JSON reports and frozen regression cases. This explicit adapter cannot be combined with channel selection. The existing detailed text-pipeline descriptions and acceptance results below refer to that native-only contract. They do not establish completeness for image text, forms, or relationships.

Every `--channels` selection uses the common evidence pipeline. Text selection retains rendered evidence without recognizing image text. Image invocations, inline images, painted paths, and shading operations leave their pages' text inventories incomplete until a provider can establish coverage beyond native glyphs. This includes nested forms and text drawn as outlines. Unused image resources, unpainted paths, and clipping alone do not create paint markers. The marker also retains the render-order boundary of the last non-text paint: earlier glyph boxes cannot establish visible-text coverage because later paint may cover them. The marker preserves native glyphs and their provenance; it does not claim to recognize image text or prove visibility under clipping. Extraction caches use format version 13 so older acquisition and paint-bound evidence is not reused. Filled cubic paths retain outward-rounded bounds enclosing all control points, including shorthand curves; an existing Form clip remains the enclosing bound when available. These bounds can exclude disjoint paint from a local text region, but do not identify painted content or certify curved strokes, clipping, or complete page inventories.

Channel selection applies before candidate search. A forms-only comparison retains native evidence without spending correspondence budgets on unrelated body text. Relation and presentation requests retain content views as supporting context; every solver result records its channel selection.

On Linux, the selected-channel pipeline acquires native evidence in bounded child processes. Page metadata and stored fields are acquired separately from glyphs and tags, so a content-extraction failure can leave field comparisons and rendering usable. Each native worker has a 2 GiB address-space limit, a 30-second CPU limit, a 35-second parent deadline, a 256 KiB request header limit, and a 128 MiB response limit; PDF input retains the parser's 256 MiB limit. Passwords travel through private stdin and are not included in responses. The parent validates input hashes, page identity, acquisition roles, evidence, and aggregate limits before comparison. Missing process restrictions remain an explicit unsupported acquisition.

The retained text pipeline provides:

- a Rust 2024 workspace with separate core, CLI, and benchmark crates;
- a backend-neutral PDF parser boundary with a `lopdf` adapter for classic xref tables, xref streams, object streams, incremental revisions, inherited page resources, borrowed-password decryption, partial page-tree recovery, and bounded stream decoding;
- a lossless glyph evidence model with geometry, CropBox and supported path-clip relationships, straight vector-line evidence, and provenance;
- bounded Content Stream glyph extraction for supported text and straight-path operators, Type 1/Type1C/MMType1, TrueType, axis-aligned Type 3 simple fonts, Identity-H, bounded Identity-V and UniJIS-UTF16-H Type 0/CID subsets, inherited resources, normalized page boxes, page-crop classification, explicit axis-aligned rectangular clipping, and isolated Form XObjects;
- standard ToUnicode resource wrappers, Standard/WinAnsi/MacRoman simple-font encodings with Differences and partial-map fallback, canonical Standard 14 identities, bounded full-domain identity Type 0 CMaps, explicit external CID font identity assertions, and stable embedded-font or bounded resource-dependent Type 3 CharProc identities for safely comparable unmapped glyphs;
- configurable Glyph-to-Line reconstruction with synthetic English spaces and preserved arbitrary-angle text lines;
- recursive XY-Cut region partitioning with spatial Region Graphs, conservative row-major label/value ordering for strongly aligned spacious parallel rows or explicitly ruled dense two-column grids, and relative Line-to-Block scoring across page breaks with preserved running-matter roles;
- reversible raw/canonical normalization with scalar-indexed source maps, retained unmapped tokens, and auditable normalization events;
- alignment-only compatibility folding and density-limited numeric masking that never changes exact canonical text;
- unique exact anchors plus swappable n-gram inverted-index, MinHash LSH, and exhaustive candidate generators;
- deterministic anchor-interval alignment for 1:1, insert/delete, constrained adjacent 1:2, 2:1, 1:3, or 3:1 matches, and exact paragraph moves, with ambiguous regions preserved as unresolved;
- an in-house Myers diff over exact canonical and unmapped tokens, with contiguous change spans, formatting-only reports, side-specific coverage, and allocation-aware limits;
- text summaries and versioned JSON reports with explicit extraction completeness, unresolved regions, coverage, and CI-oriented exit decisions;
- a public bounded pipeline from extracted glyphs through exact comparison;
- end-to-end CLI comparison, output redirection, quiet mode for CI, shell completion generation, and backend, object, glyph, or SVG overlay inspection;
- a reproducible benchmark matrix of 24 acceptance, layout, and realistic document-pattern cases over programmatic canonical documents, with each case rendered through literal-`Tj` and positioned-`TJ` PDF paths (48 records total), complemented by externally rendered Typst raw-PDF revision pairs for SPEC §2.2 Case 3 in [`fixtures/external/case3-typst/`](fixtures/external/case3-typst/), SPEC §2.1 Japanese replacement in [`fixtures/external/japanese-typst/`](fixtures/external/japanese-typst/), SPEC §2.2 Case 1 Japanese line wrap in [`fixtures/external/case1-japanese-typst/`](fixtures/external/case1-japanese-typst/), SPEC §2.2 Case 2 Japanese page break in [`fixtures/external/case2-japanese-typst/`](fixtures/external/case2-japanese-typst/), and SPEC §2.2 Case 4 / Case 5 Japanese paragraph insertion and deletion in [`fixtures/external/case4-case5-japanese-typst/`](fixtures/external/case4-case5-japanese-typst/);
- a growing non-vendored real-world revision-pair benchmark track with download provenance and SHA-256 checksums, development versus holdout treatment, human-reviewed expected changes for a representative subset, and revision-diff quality metrics (coverage, unresolved token share, recall, precision where fully annotated, change-kind accuracy, fragmentation, suspicious tiny edits) reported separately from extraction conformance.

The comparison pipeline is covered by the five acceptance classes defined in [`SPEC.md`](SPEC.md): line-wrap-only and page-break-only changes produce no content changes, while a text replacement, paragraph insertion, and paragraph deletion each produce one exact change in the generic fixtures.

The complete generated matrix currently has 42/48 strict passes. Six cases
retain candidates because edit boundaries are ambiguous; their candidate
checks do not count as exact acceptance.

## Design

The processing pipeline is:

```text
PDF bytes
  -> PDF parser backend
  -> primitive glyph extraction
  -> layout reconstruction
  -> cross-document alignment
  -> exact diff
  -> changes, confidence, coverage, and unresolved regions
```

The core design rules are:

- preserve raw evidence and provenance across every stage;
- keep parser-library types behind a backend-neutral facade;
- make layout reconstruction reversible;
- use tolerant alignment but exact final diffing;
- report unsupported or unresolved content instead of silently treating it as empty text.

The [design for previously unseen PDFs](SPEC.md#16-未知のpdfに対する比較設計) separates established differences, tentative candidates, and unresolved content. The implementation applies a shared evidence assessment to direct comparisons and the PDF pipeline, reconstructs bounded local views from trusted source runs, and reports source ownership and incomplete searches. Evaluation distinguishes exact acceptance from candidate retention and preserves document-series holdouts; these results do not imply support for every PDF.

Unicode conformance is delegated to the [`unicode-normalization`](https://github.com/unicode-rs/unicode-normalization) and [`unicode-segmentation`](https://github.com/unicode-rs/unicode-segmentation) crates from the `unicode-rs` organization. Their use is confined to the normalization module, and `Cargo.lock` pins the reviewed versions so either implementation can be replaced behind that boundary if its maintenance posture changes.

Typed JSON serialization uses [`serde`](https://github.com/serde-rs/serde) and [`serde_json`](https://github.com/serde-rs/json). Both are confined to the report module rather than the diff model; the lockfile pins reviewed releases, and the versioned report DTO is the replacement boundary if their maintenance posture changes.

PDF object parsing uses [`lopdf`](https://github.com/J-F-Liu/lopdf) with its default features disabled, avoiding optional date and parallel-processing dependencies. The project uses the [0.45.0 crates.io release](https://github.com/J-F-Liu/lopdf/releases/tag/v0.45.0), which includes the protections pdfdelta relies on: xref-stream entry counts are bounded against the decoded stream body during loading ([#561](https://github.com/J-F-Liu/lopdf/pull/561)), object streams are parsed non-destructively so encoded bytes and filter metadata remain available as evidence ([#562](https://github.com/J-F-Liu/lopdf/pull/562)), the non-standard `/BrotliDecode` filter is supported ([#567](https://github.com/J-F-Liu/lopdf/pull/567)), stream `/Length` mismatches are recovered within xref-bounded objects ([#568](https://github.com/J-F-Liu/lopdf/pull/568)), and a bounded cross-reference reconstruction fallback recovers documents whose startxref or xref data is broken beyond offset repair ([#570](https://github.com/J-F-Liu/lopdf/pull/570)). Production use of the crate is confined to the PDF backend adapter; test-only code also uses the same release to construct parser and end-to-end fixtures. `Cargo.lock` pins the reviewed dependency graph, and backend upgrades must pass capability and hostile-input fixtures before adoption. This boundary makes replacement possible if maintenance or conformance changes; it does not assume that any external project can provide a permanent maintenance guarantee.

See [`SPEC.md`](SPEC.md) for the authoritative technical design and roadmap.

## Workspace

```text
crates/pdfdelta-core   Pure comparison library and neutral data model
crates/pdfdelta-cli    The pdfdelta command-line interface
crates/pdfdelta-bench  Fixture generation and evaluation tooling
```

## Development

The workspace requires stable Rust with Edition 2024 support.

Development and test builds retain file/line backtraces but omit full type and
variable debug information. Incremental compilation is disabled to reduce disk
usage, at the cost of slower rebuilds after edits. For a debugger session that
needs local variables, use `CARGO_PROFILE_DEV_DEBUG=2 cargo build`.
To reclaim accumulated debug artifacts after toolchain or build-setting changes,
run `cargo clean --profile dev` when no builds are running; the next build will
recompile dependencies.

```bash
mise run ci
mise run pdfdelta -- --help
mise run example-glyph-comparison
```

`pdfdelta-core/examples/glyph_comparison.rs` demonstrates the backend-independent
`Document<Glyph>` API with a small in-memory replacement example. It is useful
for developing and testing the alignment pipeline without needing PDF fixtures.

`mise run ci` runs the same formatting, linting, workspace tests, and benchmark
verification as the GitHub Actions quality gate.
Run `mise tasks ls --local` to discover the reusable development and benchmark
tasks.

## Generated benchmark fixtures

`pdfbench` can render a bounded, single-page canonical YAML document through
either project-owned PDF construction path. The command preserves the source
order of the title, section headings, and paragraphs, and refuses to replace an
existing output path.

`mise run bench` evaluates all 23 generated cases through both renderers and
reports aggregate event and changed-token precision, recall, and F1. The current
46-record matrix matches 26/26 expected semantic events and 858/858 changed
tokens, with zero false-positive changed tokens across the layout-only cases.

```yaml
document:
  title: Quarterly Service Report
  sections:
    - id: availability
      heading: Service availability
      paragraphs:
        - id: availability-p1
          text: Release 10 remains available during the transition.
        - id: availability-p2
          text: Current availability guidance remains unchanged.
    - id: support
      heading: Customer support
      paragraphs:
        - id: support-p1
          text: Support hours remain unchanged.
```

```bash
mise run bench-render -- document.yaml --renderer lopdf-tj --output document.pdf
mise run bench-render -- document.yaml --renderer classic-xref-tj --output document-classic.pdf
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj number-replace --paragraph-id availability-p1 --new-number 20
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj line-wrap --paragraph-id availability-p1 --after-word 4
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj page-break --before-paragraph-id support-p1
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-insert --section-id availability --index 2 --paragraph-id availability-p3 --text "Inserted availability detail."
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-move --paragraph-id availability-p2 --to-index 0
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj paragraph-move --paragraph-id availability-p2 --to-section-id support --to-index 1
mise run bench-evaluate-yaml -- document.yaml --renderer lopdf-tj margin-change --new-margin 48

# Evaluate a pair produced by an external renderer without invoking it in CI.
mise run bench-evaluate-rendered-yaml -- \
  fixtures/external/case3-typst/document.yaml \
  --old-pdf fixtures/external/case3-typst/old.pdf \
  --new-pdf fixtures/external/case3-typst/new.pdf \
  --renderer typst-0.15.1 \
  number-replace --paragraph-id release --new-number 20
```

The YAML workflow accepts printable ASCII. `evaluate-yaml` applies one supported
paragraph-local text mutation, a paragraph-local line wrap, a non-empty-section
paragraph deletion, a section-local paragraph insertion, a page break before a
source paragraph, a same-section or cross-section paragraph move, a selected-section
column reflow, or a global line-height, margin, font-size, or page-size change. It
renders the old and new revisions in memory and reports whether the exact expected
change was recovered. It does not publish either generated PDF.

`evaluate-rendered-yaml` applies the same canonical mutation and exact-span
evaluator to bounded, already-rendered PDF inputs. It records the supplied
renderer identity but never executes that renderer. The vendored Typst example
keeps external toolchain availability out of the normal test path while still
checking its PDF dialect through the same expectation logic.

`column-change --section-id ID` accepts a selected section with at least four
paragraphs in a one- or multi-section document. It lays out that section's
paragraphs as two column-major columns while keeping the title, every heading,
and every other section full-width.

### Graph generalization evaluation

`pdfdelta_bench::generalization` provides an independent version 1 annotation
contract for local correspondences, typed change units, text display ranges, exact text positions,
exact raster masks, and changed relationships. Each dimension accepts multiple
complete expected outcomes and reports a separate one-to-one multiset score for
each alternative. Duplicate reports count as false positives; missing reports
count as false negatives. Inferred reports receive a separate score against the
same alternatives; these scores must not be added to the conditional scores.
Unannotated dimensions have no score, and an
empty denominator has no precision or recall value.

The programmatic fixture matrix crosses unchanged values, swapped numbers,
negation/unit changes, and Japanese values with unchanged/reversed graph storage
order through `compare_document_views`:

```bash
cargo test -p pdfdelta-bench --test generalization_fixture
```

This is graph-level validation, not evidence of PDF producer, layout, scan, or
rendering generalization. Node IDs in these annotations refer to the supplied
graph fixtures. Raster masks are scored as exact region facts, not pixel IoU.
For `change_unit` annotations, `old` and `new` may be `null` to leave node
correspondence unannotated; an empty array still asserts absence. The operation
and its values must match exactly. Scoped and unscoped expectations share a
one-to-one maximum matching, so a general expectation cannot consume the only
report that satisfies a scoped one. Composited page-rendering changes are excluded
from content change units and remain observable in the pixel-region dimension.
Source coverage uses the same core calculation as the CLI: each selected channel
reports discovered, compared, and uncompared source references plus inventory
completeness on both sides. Missing discovery remains unknown even when every
discovered source was compared. Document completion requires complete channel
coverage and resolved search; these counts do not measure pixel area or establish
semantic correctness. Aggregate peak-memory accounting and
broader controlled multi-producer matrices remain unfinished. The existing real-world
text regression annotations stay separate.

The `display_range` dimension scores half-open Unicode scalar ranges in each
displayed local text value, independently of `text_position` source-token masks.
Its facts name `old`/`new` node groups, `old_side`, `start`, and `end`. The current
report displays the entire changed paragraph or text-valued field, so its range
is `0..value.chars().count()`; it does not yet choose shorter review excerpts or
page polygons. Null text is unknown and produces no display-range fact. A full
date can therefore be correct for display while only its last character is
reported changed. Inferred display ranges are scored separately. These offsets
are not raw PDF byte offsets or glyph indices.

The [external vertical-text controls](fixtures/external/vertical-tectonic/)
exercise Tectonic-produced Japanese through the default CLI. A page/position
change preserves all 14 native text sources, while a quantity change reports one
inferred text operation and an exact mask for the inserted glyph. The PDFs,
TeX sources, and hash-bound change-unit annotations are retained; tests do not
need an installed producer. This is a single vertical column with a horizontal
heading, not evidence for ruby or multiple vertical columns. Visual and
relationship coverage remain incomplete.

`pdfbench generate-document-matrix` creates 64 English/Japanese prose PDFs and
128 comparison pairs using independently installed Typst and Tectonic binaries.
It crosses unchanged text, number replacement, negation, and unit replacement
with normal/narrow pages, an explicit page break, and sans/serif fonts, including
both directions between producer families. Expected paragraph changes come from
the generated source, before comparison. The manifest includes source paths, PDF
hashes, producer versions, mutation axes, and per-pair annotations.

Add `--kind table` to generate 192 PDFs and 384 pairs for a two-column
table. Each presentation/producer combination is generated with and without
borders; the borderless settings use `borderless_` layout names and preserve
the authored cell values. The row labels stay
fixed while number replacement, unit replacement, column-header replacement,
and swaps change the content. The swap preserves the value multiset and expects
two cell changes; combining it with a header replacement expects three.
Expectations score
exact cell-value operations; node correspondence and row membership remain
unannotated, so these scores alone cannot establish correct row association.
The page-break setting moves the whole target paragraph/table to a new page;
it does not exercise a table split across pages.

```bash
cargo run -p pdfdelta-bench -- generate-document-matrix \
  --typst /path/to/typst --tectonic /path/to/tectonic \
  --tectonic-cache /path/to/prepared-tectonic-cache \
  --font-path /path/to/noto-cjk-fonts \
  --first-evaluated 2026-09-09 --output /tmp/pdfdelta-prose-matrix
```

The destination must not exist. Provide Noto Sans CJK JP and Noto Serif CJK JP
fonts; Typst reads only the supplied font directory, while Tectonic also needs
those font families visible through the host font configuration. Prepare the
Tectonic cache with the `geometry`, `fontspec`, and `xeCJK` packages before running
(table generation also needs the standard `tabular` font metrics):
generation uses `--only-cached --untrusted` and does not fetch missing packages.
Compilation has a 30-second deadline per document and output PDFs are capped at
64 MiB. Run `pdfdelta` for each manifest pair, then score its report against that
pair's annotation with `evaluate-document`. The generator does not run comparisons.
These are development fixtures, including prose inputs used to tune spacing,
not unseen holdouts. Vertical writing, scans, and mixed documents still need
separate controlled matrices. Results and their limits are recorded in the
[benchmark notes](benchmark/realworld/results/README.md).

`pdfbench evaluate-document --annotation annotation.json --report document.json`
scores a multi-channel CLI report using the same evaluator. The annotation binds
`old_sha256` and `new_sha256`, records `BenchmarkProvenance`, names independent
`content_mutation` and `representation_mutation` axes, and supplies `expectations`
in the graph annotation contract above. Channels must match exactly. Malformed
coverage counts, contradictory inventory claims, and input-hash mismatches are
rejected. Report input is bounded to 64 MiB and annotations to 4 MiB.

The output retains backend identities and the CLI's measured
`comparison_wall_time_ms` (extraction, child rendering, and comparison; excludes
report output). Older reports without timing retain `null`. Peak memory and
worker CPU costs are not yet measured here. Exit code 0 requires complete coverage
and an exact accepted alternative in every annotated dimension, with no inferred
reports in those dimensions; 1 indicates incomplete or mismatching evaluation,
and 2 rejects invalid input. Unannotated dimensions do not establish accuracy.

## External extraction conformance

`pdfbench extraction-conformance` compares the default parser-backed glyph
extractor with a versioned, position-aware snapshot produced outside pdfdelta.
The snapshot is bound to the exact input PDF by SHA-256 and must identify the
producer name, version, and underlying parser family. These declarations make
the evidence auditable; the command does not independently certify that the
producer uses an unrelated implementation.

```json
{
  "schema_version": 1,
  "producer": {
    "name": "position-extractor",
    "version": "1.2.3",
    "parser_family": "independent-parser"
  },
  "input_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "glyphs": [
    {
      "text": { "kind": "mapped", "value": "A" },
      "page": 0,
      "render_order": 0,
      "bbox": {
        "min": { "x": 72.0, "y": 700.0 },
        "max": { "x": 78.0, "y": 710.0 }
      },
      "baseline": { "x": 72.0, "y": 700.0 },
      "direction": { "x": 1.0, "y": 0.0 }
    }
  ]
}
```

Pages are zero-based. Text, page order, and render order are exact; every
bounding-box, baseline, and direction coordinate uses the selected absolute
geometry tolerance. Unmapped text uses
`{"kind":"unmapped","font_identity_sha256":"...","glyph_id":42}` so uncertain
decoding is never represented as empty mapped text. Input PDFs and oracle JSON
are read under explicit size limits, and incomplete extraction is rejected.

```bash
mise run bench-extraction-conformance -- input.pdf \
  --oracle oracle.json \
  --geometry-tolerance 0.25 \
  --mismatch-svg mismatch.svg
```

Exit code `0` means the snapshots match, `1` reports the first comparison
mismatch, and `2` rejects malformed input, a checksum mismatch, incomplete
extraction, or a diagnostic that would exceed the explicit `50 000` glyph /
`8 MiB` SVG / `8 KiB` per-field text limits that bound otherwise multi-gigabyte
serialization (including intermediate `String`/`xml_escape` growth). `--mismatch-svg` writes a combined expected/actual geometry overlay
only when a mismatch exists; the destination must be new and is never
overwritten. A curated [`pdf_oxide` 0.3.77 oracle](fixtures/extraction-conformance/pdf-oxide-0.3.77/)
checks all 158 mapped horizontal glyphs in the vendored Japanese Typst fixture
against an independent custom PDF parser. Broader producer and corpus coverage,
including unmapped and rotated-text oracle cases, remains future conformance
work.

## Candidate generator profiling

`pdfbench candidates` checks candidate recall and visit pressure on every
built-in rendered mutation. `candidate-profile` complements that correctness
matrix with a deterministic identity-matched synthetic corpus and reports
recall@K, untruncated candidate-count p50/p95/max, index-build and full-query
latency, and Linux resident-memory observations for the inverted index,
MinHash LSH, and exhaustive oracle.

```bash
mise run bench-candidates -- --top-k 5,10
mise run bench-candidate-profile -- \
  --blocks 1000 \
  --top-k 5,10 \
  --json-output candidate-profile.json
```

Each generator runs in a separate worker process so indexes do not overlap in
the reported process footprint. `rss_before_build_bytes` is sampled after the
shared feature corpus is constructed; `peak_rss_bytes` comes from Linux
`/proc/self/status` after index construction and all queries. Allocator
retention and single observations make memory and latency diagnostic rather
than statistically stable measurements. The block count is bounded to
`2..=10000`; the exhaustive oracle performs all-pairs work, so larger values
can be expensive. JSON destinations must be new. This opt-in profile does not
change the default inverted-index generator: a MinHash default still requires
holdout recall to be no worse and candidate count or runtime to improve
clearly on representative documents.

## Real-world revision benchmarks

Self-comparison of a document with itself cannot exercise alignment under real
edits. [`benchmark/realworld/`](benchmark/realworld/) therefore records a
separate non-vendored corpus of genuine public revision pairs (`old -> new`)
with per-side provenance (stable URL, capture date, byte size, SHA-256),
development versus holdout treatment, scope flags, and human-reviewed expected
changes for a representative subset in `expected/*.json`. Documents are never
committed to this repository.

```bash
# capture a new pair before adding its provenance fields to the manifest
mise run bench-capture -- <pair-id> <old-url> <new-url>

# download both sides of every pair and verify byte counts and checksums
mise run bench-fetch
mise run bench-revisions-checksums

# run comparisons and report metrics (add --summary-json-output summary.json for deterministic compact summaries or --json-output report.json for full reports)
mise run bench-revisions
```

`bench-capture` accepts credential-free HTTPS URLs without query strings or
fragments, follows at most five HTTPS redirects, and limits each PDF to 100
MiB. It validates both `%PDF-` responses before publishing either cache file,
never replaces differing cached bytes, and prints the six provenance fields
in manifest order.

The command reports extraction conformance separately from revision-diff
quality: extraction completeness, alignment coverage, unresolved regions with
their comparable-token share, reported change counts, and — where expected
annotations exist — recall, precision (complete annotations only),
change-kind accuracy, fragmentation (reported hunks per matched change and,
for complete annotations, per expected semantic change), and unmatched one-
or two-token edits that bad alignment tends to fabricate. Unresolvable
spans are counted separately and never inflate the tiny-edit signal. Every
pair ends in exactly one status: `OK` (compared), `LIMIT` (the comparison
stopped at a documented resource budget before producing a diff), or `FAIL`
(provenance, expectation, or execution failure). `--set dev|holdout` and
`--pair <id>` select subsets; `--limit-scale <factor>` scales the comparison
pipeline budgets (n-gram token elements, alignment candidate visits, alignment
DP cells, diff tokens, and diff edit distance up to its 64 MiB trace-allocation cap; parser
and extraction limits are untouched) and never weakens documented defaults,
while omitting it applies each pair's recorded `limit_scale_hint`. Exit
code 1 signals provenance failures, expectation mismatches, or pairs that
ended `LIMIT`/`FAIL`; low quality scores never fail a run because the dated
captures serve as calibration evidence and current fragmentation and recall
remain too unstable for rigid quality-gate thresholds (confidence calibration
has landed, but broader large-document alignment quality remains ongoing work).
Compact revision summaries use schema version 55 and include optional nested
sentence-recovery diagnostics, near-search examined and attempted work,
Sentence/Line, search-scope, and known/ambiguous-span work attribution,
diagnostic-only Sentence shadow relations, paired-anchor cross-span locality,
decision parity, a behavior-neutral Sentence edge-gate shadow with typed
stops and projected retained-pair work, production exact edge-signature
candidate traversal with compact key-range storage and typed index and traversal
stops, explicit shadow-replay / production-accepted / production-discarded
execution provenance, plus bounded production Sentence edge-filter work,
retained/rejected pair counts, typed query-fallback reasons, full-build legacy
fallback evidence, separately accounted discarded near-relation work,
candidate-posting visits and maxima, an explicit candidate-count truncation
flag, typed
near-search stop reasons, structural trusted-run profile diagnostics,
budgeted exact-unit trusted-run signature diagnostics,
bounded one- and two-sided expected-change recovery watches with retained
occurrence evidence for single-unit or adjacent-unit
segment locations, exact segment-pair candidate and classification counters,
per-record exact segment-pair evidence, existing candidate scores and
relations, reciprocal status, typed stop evidence, and diagnostic-only
one-sided veto provenance with the authoritative best opponent, actual search
scope, and at most two observed opponents with descriptor geometry and
containment when available,
bounded quote-local Sentence evidence with whitespace-collapsed source
projection, exact local edge/word scores, scalar edit ranges and typed atomic
budget stops,
annotation-independent range-backed prefix/suffix fragment diagnostics with
exact Sentence edge-signature candidates, reciprocal relation counts, bounded
sampled edit evidence, parent recovery overlap, and fail-closed typed work
limits,
source-backed recovery ownership partitions with typed gaps, committed
change-origin event and token attribution, deterministic local-fragment review
bundles with exact edit and source evidence,
bounded Clause/ListItem unit boundaries, locations, best-partner relations,
tie and exactness evidence, aggregate work counts, completion, and typed stops,
optional exact expected occurrence counts with bounded deterministic event
assignment and fail-closed matching diagnostics,
scoped-complete event and changed-token
precision/recall/F1, changed-span intersection-over-union, false-positive
tokens per 10,000 reviewed unchanged tokens,
reviewed candidate recall at the production top-K limit, and bounded
evidence-backed reasons for missed expected changes.
The historical schema-v34 capture retains the independent legacy-candidate
reference oracle and its separately bounded edge-filter and downstream work.
Inside complete scopes, including complete scopes nested in a partial review,
`old_quote` and `new_quote` identify stable relation context. Optional
`old_changed_quote` and `new_changed_quote` narrow changed-token ground truth
within that context; an empty changed quote denotes the zero-token side of a
one-sided exact edit. For disjoint or textually ambiguous hunks,
`old_changed_ranges` and `new_changed_ranges` instead record ordered,
non-overlapping scalar ranges relative to the corresponding
whitespace-normalized context; an empty range list denotes a zero-token side.
Quotes and ranges cannot be combined on the same side. When both are omitted,
the context quote remains the expected changed span. Candidate recall is
unavailable when either reviewed context quote does not identify exactly one
extracted block. Diagnostic caps produce an explicit incomplete fallback
instead of a guessed cause. The
unversioned full-report v1 key set remains unchanged. Reviewed candidate recall
and expected-change failure
diagnostics and scoped event/token metrics are available only through
`--summary-json-output`; CLI trace schema v24 exposes sentence-recovery metrics,
including both Sentence edge shadows and the production edge filter, one-hot
direct-execution provenance, near-search, shadow, filter, and run-signature stop
reasons, plus structural-run counters, but not those reviewed metrics.
`--json-output` and `--summary-json-output` are published independently and
atomically (each serializing in memory and publishing via same-directory temporary
files without overwriting existing destinations). If the second publication fails,
exit code 2 is returned and the first published artifact remains in place without
pairwise rollback across distinct paths. Evaluation reports and curated summaries
are retained in the [benchmark results index](benchmark/realworld/results/README.md).
The pre-cleanup README also linked the dated compact capture
[`2026-09-02-0c06365.json`](benchmark/realworld/results/2026-09-02-0c06365.json);
it remains available as historical evidence, not the latest measurement.
Store raw execution logs and temporary benchmark captures outside the repository.

## License

The pdfdelta project code is licensed under the [MIT License](LICENSE). The
Adobe Glyph List 2.0 data embedded in `pdfdelta-core` is distributed under its
original terms; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Usage

```bash
# Compare two PDFs and output diff to terminal
pdfdelta old.pdf new.pdf

# Compare using standard input
cat old.pdf | pdfdelta - new.pdf

# Save reports to files (-o for text diff, -j for machine-readable JSON)
pdfdelta old.pdf new.pdf -o diff.txt
pdfdelta old.pdf new.pdf -j result.json
pdfdelta old.pdf new.pdf --trace-json trace.json

# CI usage (exit codes: 0 = complete unchanged, 1 = complete changed, 2 = error, 3 = incomplete)
pdfdelta -q -s old.pdf new.pdf

# Color control
pdfdelta old.pdf new.pdf --color always

# Raise comparison budgets for large documents without changing parser or extraction limits
pdfdelta old.pdf new.pdf --limit-scale 16

# Password-protected PDFs and custom font identity assertions
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
pdfdelta old.pdf new.pdf --old-font-identity TraditionalArabic=windows-v1 --new-font-identity TraditionalArabic=windows-v1

# Reuse cached glyph extraction results across runs (directory must be trusted)
pdfdelta old.pdf new.pdf --extraction-cache-dir ~/.cache/pdfdelta

# Inspect PDF internal structures, backend info, objects, or glyphs
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --objects
pdfdelta inspect document.pdf --glyphs

# Generate shell auto-completions (bash, zsh, fish, powershell, elvish)
pdfdelta completions bash > ~/.local/share/bash-completion/completions/pdfdelta
```

Text reports are written to standard output as contextual unified-diff hunks: a one-line summary, `---` / `+++` file headers, and `@@ page N … @@` hunks with `-` / `+` markers, bounded surrounding context, one-based page numbers, explicit unresolved regions, and presentation-only grouping of nearby exact changes. `--color auto|always|never` controls ANSI color (`auto`, the default, colorizes only when stdout is a terminal; color supplements the markers and is never required to read the output). Standard input can be supplied as `-` for either PDF input. `-o, --output PATH` publishes the human-readable text report atomically to a new file instead of standard output. `-q, --quiet` suppresses standard-output reports for exit-code-only CI workflows. The typed JSON report is unchanged by presentation options: `-j, --json PATH` writes a version 11 JSON report to a new path and refuses to replace an existing file. Mixed three-block joins use `block_separator: "per_boundary"` with an ordered `block_separators` array. Each semantic content change contains one or more provenance-preserving `occurrences`; a one-hunk change contains exactly one, while one accepted recovered relation may retain multiple disjoint exact hunks as occurrences of the same event. Exit code `0` means a complete comparison without content changes, `1` means a complete comparison with content changes, `2` means an execution or report-writing error, and `3` means an incomplete comparison, including one with independently established changes. `--strict` remains accepted for compatibility; incomplete comparisons now return `3` in every mode.

Reports separate established `changes`, tentative `change_candidates`, unlocalized `proven_changed_regions`, and unresolved source ranges. Candidates appear as `TENTATIVE` hunks and do not increase coverage. The assessment includes source ownership partitions, relation dependencies, assumptions, uncertainty reasons, search completeness, and shared work consumption by stage. `detected`, `indeterminate`, and `no_content_change` describe content-difference knowledge independently of extraction and comparison completeness. Neither complete canonical comparison nor formatting annotations establish visual equivalence.

JSON schema 11 also exposes non-owning `assessment.review_units`. Each unit names an established comparison relation and the `literal_minimal` alignment policy. Completed queries report changed-source-token bounds, bounds for the final unresolved remainder, and source spans that are changed in every optimal alignment. Counts exclude synthetic block separators. A positive lower bound establishes a content difference even when no complete change event can be localized; it does not increase resolved coverage or benchmark recall. Missing bounds mean the query did not complete, never zero changes. Optional claim work uses the remaining shared assessment budget after existing localization and emission.

`--limit-scale FACTOR` raises the comparison pipeline budgets for n-gram token elements, alignment candidate visits, alignment DP cells, diff tokens, shared assessment work, and assessment output ranges. It also raises the diff edit-distance budget up to the bounded Myers implementation's 64 MiB trace-allocation cap. The factor must be finite and at least `1`; parser and extraction limits remain unchanged.

`--extraction-cache-dir DIR` reuses cached glyph extraction results stored under `DIR`, keyed by the file contents and every extraction-determining input (parser and extraction limits, password, and asserted font identities). A missing, corrupt, oversized, or outdated entry falls back to a fresh extraction, so comparison results are identical with or without the cache. Entries are revalidated only for resource ceilings and issue-scope invariants; glyph content, geometry, and ids are not re-verified and entries are not authenticated, so the cache directory is a trust boundary and must not be shared with untrusted writers.

`--trace-json PATH` writes a separate version 2 diagnostic trace without changing the normal report. The trace records input reading, PDF parsing, glyph extraction, layout reconstruction, normalization, alignment, exact diff, and report phases with bounded metrics, including sentence-recovery diagnostics when available. It also identifies incomplete or failed phases, records typed resource-limit errors, and marks phases that were skipped after an earlier stop. Each phase entry carries a wall-clock `duration_us` metric, which is nondeterministic across runs and must be excluded from golden-file comparisons. Trace files use the same atomic, no-overwrite publication policy as JSON reports.

The real-world benchmark also diagnoses valid expected masks that cannot occur under the literal-minimal objective, without changing their acceptance criteria. The [claim evaluation](benchmark/realworld/results/2026-09-09-claims/README.md) preserves the original corpus and annotations and adds an untouched PDF pair. The [structure-claim fixtures](fixtures/structure-claims/README.md) support a diagnostic probe that reports source-mask and event-grouping results separately; the tested stamp and function-list policies do not justify automatic structure discovery or production role matching.

The subsequent [exact-mask evaluation](benchmark/realworld/results/2026-09-09-exact-masks/README.md) connects disjoint atomic ranges to event ownership: unchanged spaces and punctuation inside display context remain equal. A bounded page-local catalog/form route can recover independently established footer changes while surrounding body order remains unresolved. The shared literal proof kernel uses exact-distance banding and fused count queries while preserving all optimal paths, work limits, and normalization hypotheses. The evaluation keeps official per-ID results separate from non-owning claims and records the annotated JLS holdout's incomplete extraction and resource-limit outcome.

## Current limitations

- Ordinary line-end hyphens (`-` and U+2010) are retained; uncertain discretionary use stays unresolved. Only source-backed U+00AD line-end hyphenation is removed deterministically. For source-verified ambiguous line-end hyphens, review claims retain both keep/remove interpretations and report only facts shared by every old/new combination; their original source tokens and hypothesis count are included in the report. Unfinished or unsupported hypothesis exploration establishes no additional claim. Block joins likewise require independent boundary evidence, so ambiguous fragments may remain unresolved even when one interpretation would match exactly.

- The retained text-only pipeline does not run OCR or compare image contents or handwriting. Image-only pages can pass extraction because Image XObjects are explicitly skipped; that does not mean text visible inside the image was compared. Existing OCR text layers are retained as glyph evidence, but invisible text remains outside visible-content comparison. Empty-user-password decryption is automatic; other known passwords can be read from side-specific files and are never accepted directly as argument values or retained in reports and traces.
- Extraction currently targets mainly single-column text using supported Type 1/Type1C/MMType1 or TrueType simple fonts, an axis-aligned Type 3 subset with declared metrics and bounded CharProcs, plus Type 0 fonts with one CIDFontType0/CIDFontType2 descendant and bounded metrics. Identity-H and an Identity-V subset using DW2 defaults and bounded per-CID W2 metrics use fixed two-byte codes; bounded custom Type 0 CMaps are accepted only for full-domain one- or two-byte identity mappings. UniJIS-UTF16-H uses bundled Adobe code-to-CID and CID-to-Unicode maps, validates the Adobe-Japan1 collection and supplement, and preserves two- or four-byte source codes. Explicit ToUnicode takes priority; gaps in an explicit map remain unmapped. For other encodings, ToUnicode is optional only when a stable embedded-font, canonical Standard 14 identity, or explicit caller-provided external CID font identity can preserve unmapped glyphs. External identities are trust assertions, not font discovery. Type 3 CharProc drawing operators and MMType1 variation axes are not interpreted, and MMType1 therefore cannot provide stable identity for unmapped glyphs. Unsupported clipping geometry is retained as graphics-state uncertainty: graphics-only content can continue, and restoring a saved state or leaving a Form restores the prior clip. Glyphs whose visibility still depends on that unsupported geometry remain unresolved; uncertain vector lines are not certified. Rotated, sheared, translated, or horizontally reversed Type 3 FontMatrix values, unsupported fonts inside Type 3 resource graphs, general custom CMaps, non-downward vertical advances, general vertical-writing reading order, complex tables, AcroForm, and complete annotation handling are not implemented. A simple left-to-right horizontal region is treated as known only when glyph paint order independently confirms top-to-bottom lines. An ordinary two-column region, including one surrounded by full-width bands, is treated as known only when its spatial order is unique and paint order keeps the complete left column contiguous before the complete right column. Row-major label/value or parallel pairs are treated as known only when paint order alternates left then right without overlap and either at least three strongly aligned rows have spacious separation or a straight vertical divider plus horizontal row separators prove a dense two-column grid. Three or more regions are accepted only when the spatial region graph has a unique order and paint order keeps each complete region contiguous in that same order. Ambiguous multi-region layouts, row-interleaved columns without strong parallel-row evidence, right-to-left text, non-horizontal text, and mixed or unknown text direction preserve every line and glyph. When the remaining region order is independently proven, only blocks containing locally unsupported lines are reported as unresolved reading order; otherwise the page-local comparison window remains unresolved. Extraction itself remains complete, and changes in independently anchored windows continue to be compared.
- Every extracted glyph records whether its geometry is inside, partially outside, or fully outside the normalized page CropBox and any supported explicit path clip. Fully outside glyphs remain available as raw inspection/SVG evidence but are excluded from visible-content comparison; partially intersecting glyphs remain comparable. A single axis-aligned rectangle expressed by `re` or by one closed or implicitly closed straight-line subpath is interpreted as a clip, including intersections of supported rectangles. Straight stroked segments are retained as bounded layout evidence; curved or compound clipping paths, transparency, fill/stroke color, and occlusion by later paint operations are not interpreted, so overpainted historical text can still appear in comparison input.
- Paragraph moves are reported as dedicated `Move` changes when an out-of-order anchor is unique and its canonical text matches exactly. Fuzzy or structurally changed move proposals remain candidates or unresolved unless separate source-backed evidence establishes their content changes.
- PDF text operators, encodings, ToUnicode maps, and Form XObjects are supported only within the bounded subset covered by the backend fixtures.
- Unsupported or unresolved extraction is reported as a typed document-, page-, page-tree-gap-, or glyph-gap-scoped issue in stderr and the text or JSON report.
- Page- and glyph-scoped extraction gaps are mapped from their side-local retained evidence to monotone exact-anchor windows. Recoverable Form XObject failures roll back that invocation's glyphs while retaining text before and after it. Affected windows are reported as unresolved, while proven windows continue through alignment and exact diff; if no usable anchors exist, all retained content remains unresolved. An incomplete comparison returns exit code `3` even when its report includes independently established content changes.
- Document-scoped extraction gaps prevent correspondence from being established because the missing order interval cannot be located safely. Retained glyph evidence is normalized and represented by unresolved source partitions. Page trees with independently valid and invalid branches retain the valid pages and report side-local page-tree-gap boundaries; unexplained root count mismatches remain document-scoped. Both forms of incomplete extraction return exit code `3`. Other fatal I/O, backend, malformed-input, and resource-limit failures remain execution errors with exit code `2`.
- Finer extraction-gap boundaries and recovery of changes inside an unresolved extraction anchor window remain future work.
- Atomic `--json` publication requires a filesystem with same-filesystem hard-link support. Other filesystems return exit code `2` without publishing the report.
- Formatting-only reporting is best-effort and does not claim pixel-level rendering identity.
- Engine-generated replacements receive `CharacterWidth` only when an explicit fullwidth/halfwidth fold exactly explains the changed hunk. `OcrConfusion` remains available only to programmatic callers; the core library can retain caller-supplied recognition candidates without classifying differences as OCR mistakes.
- JSON change, formatting, and unresolved spans include glyph geometry plus content-stream object and operator provenance. Text reports do not list per-span provenance, and SVG output remains a whole-document glyph overlay rather than a per-change diff overlay.
- The automated PDF mutation benchmark is deterministic and in-memory: it uses printable ASCII with Type 1 Helvetica and two PDF construction paths (23 cases × 2 project renderers = 46 records), including repetition, long prose reflow, numbered requirements, pagination churn, footnote-like paragraphs, section-labeled paragraph movement, and column-major two-column reflow. The separate file-backed canonical YAML path renders one bounded document and can feed paragraph-local line wrapping, page breaking, text replacement, text insertion, text deletion, number replacement, section-local paragraph insertion, non-empty-section paragraph deletion, same-section or cross-section paragraph movement, four global rendering changes, and a selected-section paragraph-only column change through the library evaluator while retaining its title and section headings. The CLI exposes those same fourteen mutation commands. One canonical number-replacement expectation is also evaluated against a vendored Typst pair through `evaluate-rendered-yaml`, without invoking Typst in CI; this path does not generate Typst source from YAML or cover the full mutation matrix. A parser-independent Glyph-to-Line matrix separately covers single-column English, mixed font sizes, superscripts, reconstructed English spaces, and decoded horizontal Japanese/Latin text. A companion Line-to-Block matrix covers paragraph grouping, heading separation, relative spacing, conservative or cadence-supported page boundaries, and row-major label/value ordering for strongly evidenced parallel rows. Primitive extraction has a neutral snapshot comparator for decoded text, glyph count and order, page, geometry, baseline, and direction, backed by hand-written rotated and positioned-text oracles. Low-level PDF fixtures separately verify CropBox and rectangular path-clip classification, straight-line provenance, conservative ruled-grid ordering, and one exact cell replacement. A bounded CLI accepts versioned snapshots with producer and input identity, and a curated `pdf_oxide` 0.3.77 snapshot verifies all 158 mapped horizontal glyphs in one Japanese Typst fixture against an independent custom parser. Opt-in mismatch SVG output overlays expected and actual geometry without overwriting existing evidence. The cross-parser corpus remains intentionally narrow. These are complemented by vendored external Typst raw-PDF revision pairs for SPEC §2.2 Case 3 (`Release 10` -> `Release 20`) in [`fixtures/external/case3-typst/`](fixtures/external/case3-typst/), SPEC §2.1 Japanese horizontal born-digital text replacement in [`fixtures/external/japanese-typst/`](fixtures/external/japanese-typst/), SPEC §2.2 Case 1 Japanese line-wrap invariance in [`fixtures/external/case1-japanese-typst/`](fixtures/external/case1-japanese-typst/), SPEC §2.2 Case 2 Japanese page break invariance in [`fixtures/external/case2-japanese-typst/`](fixtures/external/case2-japanese-typst/), and SPEC §2.2 Case 4 / Case 5 Japanese paragraph insertion and deletion in [`fixtures/external/case4-case5-japanese-typst/`](fixtures/external/case4-case5-japanese-typst/). Non-vendored public smoke corpora are recorded in [`benchmark/manifests/real-world-pipeline.tsv`](benchmark/manifests/real-world-pipeline.tsv) and [`benchmark/manifests/real-world-pipeline-round2.tsv`](benchmark/manifests/real-world-pipeline-round2.tsv), but downloading and running them is not automated. With documented explicit inputs, the second manifest records 37 strict self-comparison successes and one default-mode partial success that remains strict-incomplete. Additional external renderer dialects (LaTeX, HTML/Chromium) and broader Japanese raw-PDF corpus fixtures (such as multiple vertical columns, ruby, or additional non-Identity-H encodings) remain future evidence validation work.
- The real-world revision track ([`benchmark/realworld/`](benchmark/realworld/)) records 29 genuine public pairs: 19 development and 10 holdout pairs spanning standards, regulatory publications, API and user guides, Latin/CJK prose, code, tables, forms, screenshots, generated reference manuals, multiscript examples, and image-heavy layouts. Eighteen pairs are standard targets near the intended scope and eleven are explicit stress cases; eight pairs have partial human-reviewed expected changes, two of those partial sets contain three complete scopes, and four additional pairs have scoped-complete review regions with event and changed-token precision.
  Historical captures and evaluation findings are listed in the
  [benchmark results index](benchmark/realworld/results/README.md), including the
  [claim evaluation](benchmark/realworld/results/2026-09-09-claims/README.md) and
  [exact-mask evaluation](benchmark/realworld/results/2026-09-09-exact-masks/README.md).

  The dated [`2026-08-26`](benchmark/realworld/results/2026-08-26.json) artifact remains the immutable `limit_scale_hint` baseline for the original five-pair corpus. The [`2026-08-28`](benchmark/realworld/results/2026-08-28.json) capture records the expanded twelve-pair corpus at engine commit `a7b56a7`, while [`2026-08-29`](benchmark/realworld/results/2026-08-29.json) records all 29 current pairs at engine commit `ffd7600`. Same-day follow-ups record [`987020f`](benchmark/realworld/results/2026-08-29-987020f.json) uncertain-span exact and reciprocal replacement recovery, [`ca6447c`](benchmark/realworld/results/2026-08-29-ca6447c.json) monotonic exact-unit recovery inside anchored trusted runs, [`0cef530`](benchmark/realworld/results/2026-08-29-0cef530.json) short-unit recovery, weighted multiset candidate scoring, and page-local short candidates, [`c2de839`](benchmark/realworld/results/2026-08-29-c2de839.json) atomic line recovery for punctuation-free uncertain regions, [`80def43`](benchmark/realworld/results/2026-08-29-80def43.json) bounded semantic line grouping for short matched regions, [`3074d95`](benchmark/realworld/results/2026-08-29-3074d95.json) short structural-label anchors for adjacent value moves, [`de4ed4c`](benchmark/realworld/results/2026-08-29-de4ed4c.json) mixed-run word replacement grouping, [`1ed7340`](benchmark/realworld/results/2026-08-29-1ed7340.json) fail-closed fragment-completion vetoes for general uncertain-span replacements, [`556686c`](benchmark/realworld/results/2026-08-29-556686c.json) the behavior-preserving multi-occurrence change-model migration, [`27f096e`](benchmark/realworld/results/2026-08-29-27f096e.json) scoped-complete event and changed-token precision for three fully reviewed regions, [`f5406f3`](benchmark/realworld/results/2026-08-29-f5406f3.json) role-local running-matter alignment, [`4e6ae73`](benchmark/realworld/results/2026-08-29-4e6ae73.json) the safe near-search baseline, [`5145723`](benchmark/realworld/results/2026-08-29-5145723.json) behavior-neutral structural trusted-run diagnostics, [`1463932`](benchmark/realworld/results/2026-08-29-1463932.json) bounded exact-unit signature diagnostics for trusted runs, [`1a0675f`](benchmark/realworld/results/2026-08-29-1a0675f.json) bounded expected-change recovery watches, and [`24a2300`](benchmark/realworld/results/2026-08-29-24a2300.json) diagnostic-only adjacent-unit segment watches. The [`5ffa3e0`](benchmark/realworld/results/2026-08-30-5ffa3e0.json) capture records bounded exact segment-relation diagnostics without enabling segment moves, [`b1e54e3`](benchmark/realworld/results/2026-08-30-b1e54e3.json) adds bounded one-sided insertion/deletion occurrence evidence, [`2ddbb6b`](benchmark/realworld/results/2026-08-30-2ddbb6b.json) adds behavior-neutral Clause/ListItem watch diagnostics, and [`ebcb49e`](benchmark/realworld/results/2026-08-30-ebcb49e.json) validates exact occurrence counts and groups repeated running-matter changes into multi-occurrence events. Separate files keep parser, alignment, annotation, and algorithm effects auditable instead of rewriting historical metrics.

  The [`64a58b7`](benchmark/realworld/results/2026-08-30-64a58b7.json) capture adds topology-local, multiplicity-aware Line trigram candidate indexing with explicit posting-work diagnostics. The [`496d128`](benchmark/realworld/results/2026-08-30-496d128.json) capture attributes that bounded near-search work to Sentence and Line units. The [`ba424d8`](benchmark/realworld/results/2026-08-30-ba424d8.json) capture further attributes it to four search phases. The [`a7a483c`](benchmark/realworld/results/2026-08-30-a7a483c.json) capture separates known-same-span candidates, ambiguous-span candidates, and shared query preparation without changing comparison behavior. The [`a5202c8`](benchmark/realworld/results/2026-08-30-a5202c8.json) capture replays a known-span-only Sentence search in shadow mode and shows that removing ambiguous counterparts changes relation and reciprocal-pair decisions, so the filter remains diagnostic-only. The [`d045e2c`](benchmark/realworld/results/2026-08-30-d045e2c.json) capture preserves every output and quality field while stopping word-multiset scoring when its remaining upper bound cannot improve the exact relation score. The [`d6cd66d`](benchmark/realworld/results/2026-08-30-d6cd66d.json) capture classifies cross-span Sentence work by paired-anchor topology and shows that retaining only the same paired interval changes unique and reciprocal relation decisions. The [`f0ffe19`](benchmark/realworld/results/2026-08-30-f0ffe19.json) capture begins measuring exact relation-floor early-stop opportunities in completed searches. The [`12fb039`](benchmark/realworld/results/2026-08-30-12fb039.json) capture retains completed probe observations from budget-stopped cross-span searches and shows that the exact skip would save only 0.155% of cross-span similarity comparisons, so it is not enabled. The [`ab26b3c`](benchmark/realworld/results/2026-08-30-ab26b3c.json) capture records the atomic production Sentence edge filter and its full-build legacy fallbacks. The [`edb34a2`](benchmark/realworld/results/2026-08-30-edb34a2.json) capture independently replays exact edge-signature candidates without changing any prior report field; the replay prunes 93.998% of observed pairs, while six large searches still stop in the broader candidate-posting traversal, so the signature gate remains diagnostic-only.

  The [`7ffc831`](benchmark/realworld/results/2026-08-31-7ffc831.json) schema-v33 capture stores Sentence edge signatures as compact unique-key ranges plus occurrence arrays. All 18 available direct replays complete, common completed-path logical bytes fall by 66.24% from schema v32, and comparison and reviewed-quality fields remain unchanged. Production activation remains deferred because the comparable candidate union is 12.04%, the exact retained lower bound is already 10.89%, and the LibreOffice reference oracle still stops at its pair-visit limit; the old sub-10% gate must be replaced by an exact-lower-bound criterion before behavior changes.

  The [`e254b48`](benchmark/realworld/results/2026-08-31-e254b48.json) schema-v34 capture independently applies exact Sentence edge classification before reference near-relation work. Seven of eight reference oracles preserve plan and retained-pair fingerprint parity. LibreOffice reduces downstream near-pair work to 7,993 but stops fail-closed after 32,000,000 edge comparisons with 1,831 broad candidates unclassified. Every comparison, quality, candidate-recall, direct-replay, and other diagnostic field remains identical to schema v33 when the reference-oracle object is excluded; production activation remains deferred without raising budgets.

  The [`447927d`](benchmark/realworld/results/2026-08-31-447927d.json) schema-v34 capture packs exact FIRST/LAST provenance into the existing one-word legacy candidate postings and reuses it during independent reference classification. All eight reference oracles complete with identical plans and retained-pair count, set, and order fingerprints. LibreOffice classifies all 18,853,226 broad pairs with 24,943,792 edge comparisons under the unchanged 32,000,000 cap. Removing the reference-oracle object produces exact `e254b48` parity for every other field; production activation remains deferred pending the candidate-overhead and full-build fallback gates.

  The [`08cc0c6`](benchmark/realworld/results/2026-08-31-08cc0c6.json) schema-v35 capture activates exact edge-signature traversal in production. All 18 available Direct builds are accepted and complete, all six former legacy fallbacks are eliminated, and the 12 previously complete paths preserve their comparison and quality fields. On the six newly completed paths, Direct generates 1,633,897 candidates instead of the completed reference oracles' 31,842,858 broad candidates, a 94.87% reduction, with zero broad Sentence posting visits. Its apparent SP 800-57 recall regression led to a review of the expected deletion.

  The [`85c799d`](benchmark/realworld/results/2026-08-31-85c799d.json) schema-v36 capture adds bounded one-sided veto provenance without changing any comparison field or complete-scope metric. The review confirmed that the SP 800-57 toolkit footnote exists in both revisions and removes only the comma before `rather`; the annotation is now a replacement. Its counterpart remains unresolved because the watched whole-Sentence units are in different spans, score only 2,347 against each other, and each side has tied 10,000-point competing relations. The next diagnostic therefore targets quote-local fragment or clause boundaries without changing thresholds or margins.

  The [`10e921f`](benchmark/realworld/results/2026-08-31-10e921f.json) schema-v37 capture adds bounded quote-local exact diagnostics without changing any schema-v36 comparison, quality, candidate, or existing diagnostic field. Quote-local diagnostics complete without a stop in all eight recovery-watch reports, producing ten available pairs. The unresolved SP 800-57 toolkit counterpart scores 10,000 locally and reduces to one exact comma deletion, while its parent Sentence relation remains non-reciprocal. This supports an annotation-independent local-fragment candidate shadow, not immediate recovery or looser thresholds.

  The [`c4cc1cd`](benchmark/realworld/results/2026-08-31-c4cc1cd.json) schema-v38 capture adds that annotation-independent local-fragment shadow without changing any schema-v37 comparison, quality, candidate, or existing diagnostic field. It is present on 18 recovery builds; two complete with no eligible parent, while all 16 non-empty builds stop atomically under actual-input-proportional traversal limits (six at index postings and ten at similarity comparisons). No partial relation set is published. Under the current budgets, the all-boundaries expansion is not a production candidate; the next planned shadow narrows parent Sentence pairs before fragment enumeration.

  The [`cda68bf`](benchmark/realworld/results/2026-08-31-cda68bf.json) schema-v39 capture replaces global fragment postings with fixed-minimum-depth parent and word-boundary signatures without changing any schema-v38 comparison, quality, candidate, or existing diagnostic field. Across the same 18 builds, postings fall from 16,574,884 to 349,740, queries fall from 5,480 fragment-level queries to 30 parent-level queries, and posting visits fall from 693,791 to 89,926. The two empty builds complete; the 16 non-empty builds still stop atomically because 12,842 admitted parent pairs expand to 9,874,942 same-orientation fragment pairs. The next shadow must enumerate boundary-signature fragment pairs inside each admitted parent instead of taking their Cartesian product.

  The [`40f8e19`](benchmark/realworld/results/2026-08-31-40f8e19.json) schema-v40 capture adds that parent-scoped, same-side boundary-signature index without changing any schema-v39 comparison, quality, candidate, or diagnostic field outside `local_fragment_shadow`. It processes 87,646 admitted parent pairs and submits 2,840,908 fragment pairs to exact edge recheck, 71.23% fewer than schema v39's bounded Cartesian work despite reaching 6.82 times as many parent pairs. The two empty builds complete; all 16 non-empty builds now stop atomically at the similarity-comparison limit after 42,939,833 comparisons. The next shadow must attribute exact-edge and word-score comparison work before another pruning primitive is selected.

## Contributing

Read [`AGENTS.md`](AGENTS.md) for repository working agreements. Keep changes aligned with the staged roadmap in [`SPEC.md`](SPEC.md), verified with the workspace checks above, and small enough to review as one coherent behavior change.
