# Document comparison migration

The target is a running comparison path for prose, tables, scanned/image content,
and forms over one evidence store and one correspondence foundation. This is not
complete when only data structures or a new design document exist. The previous
29-pair/39-expectation evaluation remains a frozen regression contract.

## Current acquisition scope

The Latin-only OCR implementation has been removed, including model options,
recognition workers, runtime dependencies, and OCR-specific CLI report fields.
Image comparison retains rendered pixels; image text and saved-value/display-text
agreement remain unresolved. The core can still represent externally supplied
recognition evidence, but the application has no recognition provider.
The OCR execution results below are historical migration evidence and do not
represent current functionality or runnable integration tests.

## Decisions and boundaries

- Preserve the native glyph extractor, its raw codes, geometry, rendering modes,
  and object/operator provenance. Keep parser-library types behind the facade.
- Keep the core pure. The CLI owns document I/O, resource limits,
  report destinations, and process status. The benchmark owns generated inputs
  and independent expected outcomes.
- Separate content operations, relationship operations, presentation differences,
  conditional literal masks, inferred interpretations, and display context.
- Use Rust only. Do not add Python code or external analysis APIs. Built-in Rust
  rendering runs in a bounded local child process. Missing capabilities leave selected channels
  explicitly incomplete. Preserve backend and policy versions in reports.
- Keep caches, raw execution logs, and disposable experimental intermediates out
  of the repository; retain evaluation READMEs, curated summaries, manifests,
  expected outcomes, and necessary provenance.
- Existing text-only library calls remain explicit adapters. They cannot establish
  completion for image, form, or other unexamined channels in a PDF-level report.
- Keep all five initial release cases, strict source masks, and frozen annotations.
  Generalization evaluation has a separate versioned contract.

## Work and completion evidence

1. **Comparison contract and evidence boundary.** Rewrite the comparison contract
   around selected channels, typed source references, incomplete inventories, and
   dependency-local failures. Implement bounded native, rendered, and structured
   evidence storage and a native adapter. Prove that image-only and mixed pages
   cannot become complete/no-change merely because native text is empty; OCR
   never receives fabricated native glyph IDs; invalid references, coordinates,
   excessive payloads, and duplicate consumption are rejected.
2. **Document graph and providers.** Represent ordered prose, sections, table
   row/column/cell membership, label/value relationships, figures/captions, forms,
   and alternative views without imposing one global reading order. Ingest native
   blocks and native structured fields/tags, rendered regions, and source-linked
   structure candidates supported by the Rust implementation. Preserve omitted
   header/footer and unknown regions. Unimplemented rendering features and recognition
   remain explicit gaps rather than external API dependencies.
3. **Shared correspondence solver.** Route all candidate suppliers, including
   catalog/form footers, through shared evidence-ownership and conflict checks.
   Support one-to-many, many-to-one, movement, and typed relationship changes.
   Use bounded hierarchy-local search; distinguish exhausted search from absence
   of a competitor. Page equality and global reading order are not prerequisites.
   Test cover insertion, split/merge, reordered drawing operations, repeated keys,
   changed row/value association, alternative hypotheses, and missing evidence.
4. **Typed comparison and public output.** Connect existing literal kernels to
   established local text correspondences. Compare cell/field values, memberships,
   order, and visual regions using appropriate contracts. Keep whole-field review
   units separate from changed-character ownership. Preserve competing OCR/model
   readings and report stored-value/appearance disagreements. Expose channel-wise
   completeness, unresolved dependencies, operations, exact masks, inference, and
   evidence in the CLI and machine report. Demonstrate each channel through the
   actual PDF/graph/solver/report path, not only constructed graph fixtures.
5. **Execution control and generalization evaluation.** Bound processing work,
   memory, image dimensions, output bytes, and candidate exploration.
   Reanalyze only a bounded
   conflicting region. Test independent content/relationship mutations against
   layout, page, renderer, rasterization, language, and direction changes. Include
   independently produced PDFs, Japanese/vertical text, tables, scans, and mixed
   pages. Version alternative valid correspondences separately from exact masks.
   Report recall, false positives, per-channel compared evidence, omitted evidence,
   unresolved results, and cost; empty predictions cannot pass a changed case.

## Final gates

- Production-level fixtures cover native prose, table value association, scanned
  text/visual evidence, image edits, and stored form values plus appearance.
- Equivalent content across representations is tested independently from relation
  changes with the same text/value multiset. Ambiguity is a tested outcome.
- Native glyph adapter and all five existing release cases remain tested.
- No missing capability, clipping issue, extraction failure, or search truncation becomes
  empty evidence or a complete comparison. Independent regions can still report
  established results without assuming global uniqueness through missing evidence.
- Run formatting, workspace/all-target Clippy, workspace tests, the frozen corpus
  regression, and the separate generalization matrix. Preserve all failures and
  unavailable metrics in a compact reproducible report.
- Keep documentation honest about unimplemented interpretations and backend
  limitations; complete the migration before declaring this goal achieved.

## Deferred guarantees

Universal recovery of author intent, perfect recognition, arbitrary mathematical
understanding, and cloud model deployment are not promised. Unsupported
interpretations must remain represented as evidence and local unresolved claims;
they must not disappear from the selected comparison scope.

## Current verification ledger

The migration puts prose, tables, scans, and forms on the running evidence/graph/
solver/report path. This completes the architectural migration described above,
not universal PDF interpretation or complete comparison of the evaluation corpus.
Core module names below refer to
`crates/pdfdelta-core/src/document/`.

| Requirement | Current evidence | Established boundary |
| --- | --- | --- |
| Evidence and selected-channel contract | `document/evidence.rs`, `native.rs`, `providers.rs`; workspace evidence fixtures and actual image-only CLI controls retain unknown channels and distinct native/rendered/structured references. Paint-order, outlined-text, and worker-failure controls are recorded below. | Unsupported visibility and recognition keep inventories incomplete; native extraction alone does not prove visible-content coverage. |
| Reversible structure and correspondence feedback | Graph alternatives, shared grid installation, counterpart-axis refinement, and source dependencies are implemented. Eleven table fixtures and a frozen 384-pair external matrix cover mixed native blocks, border removal, header changes, swaps, and presentation changes. | General structural-model integration and broader segmentation interpretations are not established by that matrix. |
| Common solver and local comparison | Matching/group/operation fixtures cover split/merge, duplicate keys, source conflicts, bounded search, typed memberships, and exact local masks. The CLI exercises native text, table cells, form values/appearances, and rendered changes. Relationship and independent-failure controls are recorded below. | Declared relation comparisons do not prove automatic relationship discovery; inferred table change-unit recall does not prove row identity. |
| Recognition and appearance | Externally supplied recognition evidence is validated against raster grounding in `recognition_evidence_fixture.rs`; widget-reading disagreements retain distinct evidence and literal stored values. The Latin OCR path and its local models were removed, so no integration test exercises a recognition provider. | Broader OCR accuracy is unproven; predictions remain inferred and cannot establish complete recognition. |
| Direction and external producers | New vendored Tectonic Japanese vertical controls pass the real extraction/graph/solver/report path: movement preserves all 14 text sources, and a quantity change retains the exact inserted native source mask under inferred correspondence. | Multiple vertical columns, ruby, and full visual interpretation remain unproven. |
| Evaluation and final gates | Workspace tests, all-target Clippy, and formatting pass after the native-worker partial-write correction. The five initial native acceptance cases remain in the workspace suite. Dated frozen-corpus and worker-matrix captures are linked below; prose and OCR results are retained in the results README. | Captures identify their executable versions. The native regression is not a multi-channel evaluation, matrix inference is not whole-document completion, and unavailable measurements remain unavailable. |

Annotation evidence retains its category, text, and target, but the built-in graph
provider currently imports only its text view. It does not derive a `RefersTo`
edge from an annotation target, and the native acquisition path does not yet
extract PDF link annotations. A caller supplying such evidence must retain its
relationship inventory obligation; text comparison cannot discharge it.
Explicit graph `RefersTo` edges are supported by the relation engine. Automatic
link/footnote interpretation remains an acquisition limitation, alongside the
broader structure-model and mathematical interpretations described above.

### Completion audit follow-up

The additional worker tests prove that a deadline releases a blocked stdin writer
and that excess output cannot become a truncated successful response. A later
independent worker still succeeds. The relationship fixture now exercises
`LabelFor`, `CaptionFor`, `RefersTo`, `AppearanceFor`, and declared `Precedes`
reassignments separately from literal value changes. An incomplete new inventory
withholds deletion claims while preserving additions supported by the complete old
inventory. These are relation-engine tests, not evidence of automatic caption or
reference recognition in arbitrary PDFs. Workspace tests, all-target Clippy, and
formatting pass with these additions.

The frozen native regression replay is retained in
[`2026-09-09-migration-frozen-regression.json`](benchmark/realworld/results/2026-09-09-migration-frozen-regression.json).
It compares 25 of 29 pairs and recovers 2 of 39 frozen expectations; no pair is
complete. Seven engine results report resource limits, two processes fail memory
allocation under the 4 GiB address-space cap, and one source revision is unavailable.
Unavailable measurements remain null. This replay evaluates the native adapter,
not the new multi-channel CLI contract.

The demonstrated selected-text image gap is now repaired at both boundaries.
Every `--channels` selection uses the common collector and text selection retains
rendered evidence. Native extraction records non-text paint, including images, filled/stroked paths,
shading, and nested Form XObjects. Unused resources and unpainted paths do not
create a marker.
`EvidenceStore::from_native` keeps those pages' text inventories incomplete without
discarding their native glyphs. Cache format 5 prevents old marker-free or order-free entries
from silently restoring complete inventories. The explicit `--native-text-only`
adapter retains the frozen native-glyph contract and rejects OCR/channel options.
Real CLI image-only controls for `text` and `text,relations` return version 2 and
exit 3 with incomplete old/new text inventories. An outlined-text CLI fixture also returns incomplete text coverage, while
a mixed native/path fixture retains all 39 glyphs per side and detects its single
native quantity change. These markers establish acquisition coverage behavior,
not recognition of arbitrary painted content.

The expanded paint acquisition passes the workspace gates. Replaying the frozen
external table matrix preserves 512 inferred change units across 384 pairs with
zero false positives or misses; all pairs remain incomplete. The
[paint-acquisition summary](benchmark/realworld/results/2026-09-09-paint-acquisition.json)
records immutable executable/manifest identities, conditional and inferred scores,
per-channel source coverage, and execution cost. These development controls do
not independently validate correspondence or character masks.

### Native acquisition isolation

The execution audit found native parsing/extraction inside the parent CLI. The
selected-channel path now uses `native_worker.rs`, reusing the process runner
and Linux restrictions already used by rendering.

- Move parsing, extraction, fields, and tags into internal workers. Pass PDF
  bytes and private options through bounded stdin and return neutral evidence.
- Acquire page metadata and stored fields independently from glyph extraction.
  A glyph-worker termination must not discard fields or prevent rendering from
  using retained page metadata.
- Preserve fatal, unsupported, unresolved, and resource outcomes. A failed
  acquisition never supplies a complete empty inventory.
- Reuse the process runner and restrictions, bound serialized requests/results,
  validate returned evidence, and preserve backend/profile identities.
- Verify actual worker execution, malformed/truncated responses, termination,
  independent field results, and existing cache/password/CLI behavior.

These choices follow the existing local execution and incomplete-result
contracts; no external service or document upload is involved.

The actual CLI array-limit fixture retains page metadata, reports the native text
resource failure, and compares an independent stored-field change. Worker tests
reject malformed/oversized requests and wrong input/page/role responses. Encrypted
cold/warm-cache comparisons agree and wrong passwords cannot reuse cached results.
Workspace gates pass. A 384-pair external table replay retains 512 inferred
matches with zero false positives or misses.
The shared serialization ceiling also handles partial writes without charging
unwritten bytes; its focused regression and workspace gates pass.
