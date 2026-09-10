# Issue 20: implementation and closure plan

Status: implementation and local acceptance complete (2026-09-11); external issue closure not performed.
Final evidence: [closure report](benchmark/realworld/results/issue20-next/closure-report.md) and [requirement matrix](benchmark/realworld/results/issue20-next/closure-matrix.md).
Prepared: 2026-09-10.
Inspected source: `356b7f518da651649bb2db745bdf8139139d2576`.
Authority: [issue 20](https://github.com/hayatosc/pdfdelta/issues/20), repository instructions, and the user's next-design direction.

## Goal text

Resolve issue 20 on the existing evidence/graph/comparison pipeline. Remove the structural search barrier for independent one-to-one correspondence, represent scoped element presence and absence with explicit evidence obligations, and connect source-checked normalization and local extraction gaps to comparison. Use diagnosed residual failures to select further reading-order, grouping, or rendering work. Demonstrate improved established-change recovery without additional annotated unchanged-character false positives on frozen real-document expectations, preserve all five core release cases, and validate the frozen implementation on genuinely unseen real revisions. Deliver reproducible per-case evidence, remaining misses, and a closure report. Do not declare success from inferred matches, token-only gains, synthetic solver results, or unit tests alone.

The user activated this implementation goal after reviewing the plan. Changes must apply to general evidence and correspondence problems, without document-specific or expectation-specific branches. The completion boundary remains the issue's acceptance criteria; uncertain or unsupported evidence cannot be made certain merely to satisfy an expectation.

## Decisions and evidence

- Preserve stable Rust, Edition 2024, the parser facade, raw provenance, reversible structure, bounded workers, and exact source accounting. No Python runtime, external analysis API, parser replacement, or OCR restoration is planned.
- Keep the common solver contract. Select algorithms internally only after validating their admissible problem class.
- Separate an optimizer's mandatory edge, a source-backed premise, and semantic identity. None implies the others automatically.
- Keep established operations, inferred interpretations, conditional masks, review/display regions, and coverage separate.
- Treat all already inspected documents, including the RFC trials, as development data.
- Preserve the original 29-pair inventory and annotations. The historical denominator is 39 scored expectations; source-only validation found 40 annotation-file entries, including one BIS selector previously excluded as indeterminate. Retain that failed entry explicitly. A schema projection for the new route must be versioned and cannot redefine expected changes.
- The recorded 2/39 events and 38/709 changed tokens are native-adapter measurements. The 512 inferred units from 384 controlled table pairs are a different experiment. Neither is a new all-channel baseline.
- DSA can legitimately remain ambiguous. Attention's frozen stamp annotation conflicts with the literal optimum; do not rewrite it to create a pass.

The current closure record is [benchmark/realworld/results/issue20-closure/README.md](benchmark/realworld/results/issue20-closure/README.md). Existing architectural work remains documented in [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md).

## External implementations: verified observations and transfer decisions

These are source/documentation inspections, not comparative executions or measured pdfdelta accuracy. Links to moving branches describe the inspected implementation; pin revisions before importing code or using a tool as an experimental comparator. Respect licenses and record versions. No new production dependency is implied.

| Project | Observed logic | Application to pdfdelta |
| --- | --- | --- |
| pdfminer.six | Groups characters into lines using relative geometry, groups lines into boxes, and hierarchically merges nearby boxes using enclosing-area distance. `boxes_flow` adjusts horizontal/vertical ordering. | Retain competing geometric groupings as reversible candidates. Relative metrics fit the project; a chosen layout order is still an inference. See the [layout explanation](https://pdfminersix.readthedocs.io/en/latest/topic/converting_pdf_to_text.html) and [implementation](https://github.com/pdfminer/pdfminer.six/blob/master/pdfminer/layout.py). |
| PdfPig | Separates word extraction, page segmentation, and reading order. Recursive XY Cut partitions through whitespace; Docstrum groups from local geometry. The unsupervised order detector combines spatial interval relations with optional rendering order. | Use distinct local order hypotheses for columns, rows, and independent regions. Preserve cycles and ambiguity rather than certifying one heuristic linearization. See [layout documentation](https://github.com/UglyToad/PdfPig/wiki/Document-Layout-Analysis). |
| PdfPig order implementation | Builds pairwise precedence links, then repeatedly takes a block with the largest outgoing count; ties follow enumeration. Rendering order uses the average text sequence. | This is a heuristic ordering, not an all-solutions uniqueness certificate. Do not copy tie handling or fixed coordinate tolerance as source proof. See [detector source](https://github.com/UglyToad/PdfPig/blob/master/src/UglyToad.PdfPig.DocumentLayoutAnalysis/ReadingOrderDetector/UnsupervisedReadingOrderDetector.cs). |
| PyMuPDF | Offers original extraction order and spatially sorted output; behavior depends on extraction format and version. | Useful as a diagnostic extraction/order comparator if needed, not evidence that sorted output is uniquely correct. See [extraction details](https://pymupdf.readthedocs.io/en/latest/app1.html) and [API version notes](https://pymupdf.readthedocs.io/en/latest/page.html?highlight=sort). |
| JoshData/pdf-diff | Uses `pdftotext -bbox`, serializes text while treating detected line-end hyphens as discretionary, invokes `fast_diff_match_patch`, and projects hunks to word boxes. An intersected box is marked in full. | Reuse the architectural separation between local text comparison and display projection. Do not adopt blanket dehyphenation, one selected edit script as proof, or word-box coverage as an exact character mask. See [source](https://github.com/JoshData/pdf-diff/blob/primary/pdf_diff/command_line.py). |
| vslavik/diff-pdf | Renders with Poppler/Cairo, compares page images and exposes pixel tolerances. The document loop pairs pages by index; image comparison also supports offsets. | Useful visual-regression reference, but page-index pairing cannot provide page-break-invariant content correspondence. Keep raster differences in a separate channel. See [source](https://github.com/vslavik/diff-pdf/blob/master/diff-pdf.cpp). |
| LayoutReader | Predicts reading order with a sequence-to-sequence model using text and layout, trained/evaluated with ReadingBank. | A possible future inferred candidate provider only. Its reading-order metric does not certify exact revision recovery. No model addition is justified before the current search and evidence gaps are addressed. See [official repository](https://github.com/microsoft/unilm/tree/master/layoutreader). |
| Docling | Its documented document tree retains reading order through ordered children. | This supports separating document structure from plain text. The specific historical `reading_order_rb.py` implementation could not be retrieved in this investigation; its detailed algorithm is not treated as verified evidence. See [document model](https://docling-project.github.io/docling/concepts/docling_document/). |

The design inference is to reuse local geometric hypotheses and source projection, while retaining stronger ambiguity and completeness contracts than these tools promise. No inspected tool establishes that one globally reconstructed reading order solves exact revision comparison.

For the assignment subproblem, [OR-Tools' primary documentation](https://developers.google.com/optimization/flow/assignment_min_cost_flow) confirms the min-cost-flow formulation. It does not establish pdfdelta's lexicographic objective, mandatory-edge certificate, or source-evidence semantics; those require separate verification.

## Execution sequence

### M0 — Connect measurement and classify blockers

Ownership: `pdfdelta-bench` revision evaluation; CLI report projection and execution metadata.
Starting points: `crates/pdfdelta-bench/src/{revisions.rs,revision_scopes.rs,revision_diagnostics.rs}` and `crates/pdfdelta-cli/src/{main.rs,evidence_compare.rs}`.

1. Inventory existing source-backed CSF, SP, FIPS/DSA, form, and generated regressions. Reuse existing fixtures; add only a missing counterexample needed for a change.
2. Freeze a baseline executable, input hashes, annotations, objective version, limits, backend versions, and any working-tree patch.
3. Run the same available inputs through the native adapter, shared text selection, and shared all-channel selection. Keep their metrics separate. Extend the benchmark's projection for the shared report rather than routing it back through the native adapter.
4. Retain every attempted input, acquisition failure, crash, timeout, unsupported result, partial result, and unscorable selector. Missing metrics are unavailable, not zero.
5. For each frozen expectation record acquisition, structure availability, candidate availability, solver completion, operation representation, and source-mask projection. Multiple blockers may coexist; retain dependencies, not just one reason string.
6. Add source-only selector validation: exactly one occurrence in the declared scope, valid old/new source coordinates, unchanged context, and unambiguous source identity. Validate before comparison. Keep historically invalid annotations as unscorable; any corrected development annotation is separately versioned.

Gate: a reproducible three-route baseline and a per-expectation blocker ledger exist. No inferred operation contributes to established recall or strict coverage. A replacement is not counted merely because unrelated deletion and insertion quotes are found.

### M1 — Exact dispatch for independent one-to-one components

Ownership: `pdfdelta-core/src/document/{matching.rs,text_candidates.rs,dependencies.rs}`; new private assignment module if needed; benchmark fixtures.

1. Keep premise, source ownership, containment, and partition validation in the common entry point. A component qualifies only when every proposal has one independent leaf endpoint on each side, and every exclusion is exactly shared-endpoint exclusion. Reject the fast path for shared glyph ownership, overlapping raster sources, ancestor/descendant coupling, alternative partitions, or group proposals.
2. Build endpoint/source/partition indexes and connected components without materializing every proposal pair for eligible assignments. Index key/content candidates and verify original tokens after hashes. For general components retain bounded conflict validation until equivalent indexed handling is demonstrated. Account for index construction, output size, and adversarial dense conflicts.
3. Implement maximum-weight **partial** bipartite matching, including unmatched vertices, using exact ordered integer score vectors. Preserve the five-class V3 lexicographic objective. Do not silently optimize maximum cardinality first or encode scores in floats.
4. An internal min-cost-flow formulation must include the zero-value unmatched alternative and correct residual reverse costs; use checked arithmetic. Finalize the implementation after a small Rust correctness prototype. No OR-Tools runtime dependency is required.
5. Obtain one optimum, then exclude each selected edge and recompute. An edge is mandatory only when the proven excluded optimum is strictly lower. Run the source-only problem separately to preserve `source_only_mandatory` and inferred-premise behavior.
6. Give assignment optimization, exclusion certificates, and index work explicit budgets. Enumeration completeness, optimization completeness, and certificate completeness are separate. On truncation retain unresolved competitors; initially use the existing conservative incomplete-component behavior.
7. Leave ordered-group DP for an evidenced ordered-group bottleneck. Do not implement four algorithm families merely to fill a dispatcher.

Gate: bounded Rust exhaustive enumeration agrees on objective and mandatory edges for small rectangular, sparse, disconnected, tied, and mixed-priority problems. Permuting candidate order changes no claims. Include a high-priority edge competing with several lower-priority edges, empty sides, zero-score alternatives, and incomplete-enumeration cases. The 5x5 clear-diagonal case completes and all-equal 5x5 has zero mandatory edges. Run 20x20 and 100x100 scaling experiments with recorded budgets/time/RSS; the latter is a measurement target, not a promised latency. Shared-evidence/group adversaries use the general path. Source-only results remain protected.

This milestone improves search completion; it alone may yield no new established changed events. The user's 1,129 mathematical reference checks are useful design evidence, not a Rust execution baseline.

### M2 — Evidence obligations and scoped presence/absence

Ownership: `pdfdelta-core/src/document/{evidence.rs,graph.rs,dependencies.rs,comparison.rs,operations.rs,coverage.rs}` and CLI serialization.

1. Extend existing issues/dependencies with a small typed obligation record: affected sources or source boundary, declared scope/channel, dependent claim IDs, missing evidence, supported next action, and bounded work status. An unavailable action is explicit. Avoid a new workflow framework.
2. Represent matched, present-only, absent-in-scope, and unknown-counterpart states without accepting an empty ordinary text group. Presence belongs to element operations; exact character kernels retain their existing contracts.
3. Define an absence witness over the opposite inventory, a validated scope correspondence, complete membership, a declared identity/equivalence policy, completed rival search, and resolved movement/rename/split/merge alternatives. Evidence changes invalidate dependent certificates.
4. Begin with closed, source-backed scoped key populations. An unmatched key proves only absence of that key under its stated identity contract; it does not prove deletion of the semantic entity after a rename. Untagged prose without adequate identity/boundary evidence remains unresolved or inferred.
5. Distinguish removal from a section from deletion from a document. Expand the rival scope or retain a movement obligation before making the latter claim.
6. Add explicit unmatched explanations to candidate/model selection. Keep M1's legacy objective behavior isolated from this new, versioned decision policy. Merely appending zero-weight dummy vertices cannot fix the positive-weight unrelated 1x1 example. Similarity alone cannot establish either replacement or deletion-plus-insertion. Until independently validated evidence discriminates, retain both explanations as inferred/unknown rather than manufacturing a null-score threshold.
7. Emit source-backed insertion/deletion operations only after witness validation. Keep review-unit extent and changed-source ownership separate. Update coverage only for evidence actually compared or accounted for by a valid presence claim; preserve channel and scope boundaries.

Gate: graph and PDF-path examples cover inserted/deleted paragraphs and fields, with exact source ownership when justified. Missing pages, duplicate/reused keys, potential renames/moves, incomplete enumeration, alternative partitions, and empty opposite scopes cannot produce false absence. The unrelated 1x1 case is never an established replacement solely because its weight is positive. Existing five core cases remain exact successes.

### M3 — Source-checked normalization, local gaps, and necessary groups

Ownership: `pdfdelta-core/src/document/{native.rs,graph.rs,operations.rs,text_candidates.rs,groups.rs,dependencies.rs}` and existing normalization/literal kernels.

1. Preserve `GlyphGap { retained_before }` through native adaptation, including stable neighboring source identities and document/page boundary cases. Neighbor IDs describe a gap boundary; they are not fabricated glyphs for missing content or evidence of a precise missing bounding box.
2. Localize dependent claims where the source boundary permits it. Keep inventory uncertainty and hidden-rival obligations broad when the missing content's extent is unknown.
3. Reuse the source checks in `diff/assessment/normalization.rs` and the local literal kernel. Attach a validated normalization family to a text view; reject unchecked external optional masks. Preserve source mappings for every retained interpretation.
4. Candidate retrieval may use multiple permitted interpretations. Accepted local claims must survive all required interpretations and relevant correspondence premises. Bound family/DAG exploration; a limit leaves uncertainty.
5. Add nonexact contiguous groups only with validated membership, source coverage, boundary evidence, and local order. Reuse existing group validation. Permit type changes only under an explicit distinction between inferred layout type and semantic role; update the common validator as well as suppliers.

Gate: line-end hyphen ambiguity can coexist with an independently proved `10` to `20` change when the local evidence supports it. True lexical hyphens, ambiguous boundary spaces, Japanese reflow, storage permutation, page-edge gaps, and unrelated independent fields preserve their correct outcomes. DSA keeps its equal-cost alternatives and changed-count bounds. No inferred boundary becomes an unconditional source claim.

### M4 — Address residual order/acquisition/visual blockers

Ownership: affected core layout/document modules; `pdfdelta-cli/src/{render.rs,native_worker.rs,evidence_compare.rs}`. Select work from the M0 ledger after replaying M1–M3.

For reading-order blockers, adapt relative geometry, whitespace partitions, and interval precedence as local hypotheses. Keep independent columns/regions unordered when sufficient. Use a sequence solver only within a validated order premise; do not force a document-wide order. Preserve header/footer evidence. Compare the added hypothesis against existing behavior on changed and unchanged controls.

For clip/visibility blockers, first produce one bounded rendering-observer experiment. `Cargo.lock` pins hayro 0.7.1 and hayro-interpret 0.7.0. The latter's [versioned Device interface](https://docs.rs/hayro-interpret/0.7.0/hayro_interpret/trait.Device.html) exposes individual `draw_glyph`, image/path, clip, transparency, soft-mask, and marked-content callbacks. It is not the unversioned upstream glyph-run API. Object/operator/Form invocation provenance still requires a verified bridge; coordinate proximity alone is insufficient.

For visual candidate cost, cache inexpensive source/context features and image fingerprints; compare detailed retained pixels only for supported region candidates. Exact hashes require sample/transform verification; approximate hashes only rank candidates. Account for unexamined rivals when pruning. Use the shifted-foreground/blank counterexample and the 8-page full-grid workload as Rust benchmarks. Preserve small numerical/sign changes; no arbitrary warp or downsampling can certify their absence.

Gate: each addition resolves a recorded blocker or supplies actionable evidence for a dependent claim, while independent comparisons survive and unsupported regions remain visible. Rendering success never certifies complete native text extraction. If a blocker needs capabilities beyond this goal, document it with a concrete follow-up draft; a required closure gate cannot be waived by deferral.

### M5 — Frozen evaluation, unseen validation, and closure

1. Run project formatting, Clippy, tests, and generated verification. Resolve or individually justify the existing six ambiguous generated cells; do not require false certainty to restore a headline 48/48 score.
2. Replay the full frozen corpus on all three routes. Publish established event recall, changed-token recall, annotated unchanged-token false positives, inferred operation counts, per-side/channel coverage, search status, unscorable cases, and process cost independently.
3. Require a strict increase in comparable established whole-event recovery over the frozen baseline, with all baseline successes retained and no additional annotated unchanged-character false positives. Token-only improvement is insufficient. Evaluate the shared route against its own M0 baseline; report the historical native 2/39 comparison only for compatible measurements. Explain per-case coverage regressions; fix loss of established comparable evidence.
4. Freeze final executable hash, revision/patch, configuration, objective, and all relevant limits. Select genuinely new revision series only afterward. Predeclare selection rules and a minimum of two independently reviewed, scorable changed scopes across two new series, plus unchanged-input controls. This two-series operational gate is a proposed strengthening of the issue's unspecified holdout size, not an existing numerical requirement.
5. Freeze source-backed annotations and globally unique selectors before any engine comparison output. Do not feed annotation boundaries or expected IDs into production. Record failed acquisition and invalid selectors as failed trials; replace an invalid trial transparently before output inspection rather than silently dropping it.
6. On each holdout, require recovery of the predeclared established change(s) with correct source locations and zero false positives in reviewed unchanged context. Controls require zero established changes; partial coverage is not a complete/no-change pass. Report all selected channels even when only a scoped text claim is scoreable. Wider completeness is not implied.
7. A holdout-informed fix consumes that holdout. Preserve its failed result as development evidence and repeat freeze/selection on new series. Do not claim success from unchanged-input controls alone.
8. Produce a closure matrix mapping every issue requirement to files, commands, results, and residuals. Form-heavy failures remain in denominators. Deferred residuals need explicit tracking; prepare linked follow-ups when publication is authorized. Prepare a closure report before any request to publish or close the issue.

Gate: every applicable acceptance row has actual evidence. If recovery or holdout validation fails, continue from the diagnosed cause; do not mark the implementation goal complete because the planned modules exist.

## Validation commands and deliverables

Before each implementation commit:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

At the final acceptance checkpoint:

```sh
cargo run -p pdfdelta-bench --locked -- verify
cargo test -p pdfdelta-bench --test source_backed_local_comparison
cargo run -p pdfdelta-bench --example structure_claim_probe --
```

Corpus commands must be recorded from the implemented harness and actual input locations; no hypothetical new flags are claimed to exist. Route choices already exist in the CLI (`--native-text-only` and selected channels), but the common-route assessment projection is M0 work.

Retain compact curated artifacts under a new dated results directory: baseline metadata, route comparison, blocker ledger, assignment correctness/scaling summary, acceptance-contract examples, final frozen replay, holdout provenance/annotations/results, and closure matrix. Keep downloaded PDFs, caches, raw logs, and disposable experiments outside the repository unless a minimal fixture is explicitly selected for retention.

## Checkpoints and scope controls

- M0 is the first implementation slice. M1 follows without an architecture rewrite. M2 and M3 turn better matching into usable evidence-backed change claims.
- After every milestone, replay affected development cases and update the blocker ledger. Run the full corpus at baseline and final freeze, or when a global change warrants it; avoid redundant full runs.
- M4 is selected by actual blockers, not a mandatory catalog of extra features. Defer OCR, semantic models, arbitrary type conversion, general table recognition, and additional parsers unless evidence and explicit scope decisions warrant them.
- No top-k uniqueness, blanket threshold relaxation, expectation-ID special cases, or hidden budget increases.
- No implementation performance or accuracy claim is made by this planning document. This investigation read local source, fetched issue 20, and consulted primary web sources; it did not run Rust tests, the solver reference program, OSS comparators, or the real PDF corpus.
