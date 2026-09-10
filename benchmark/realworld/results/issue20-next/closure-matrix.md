# Issue 20 requirement-by-requirement closure matrix

Status: **local implementation and acceptance complete; ready for review**.
See [the final report](closure-report.md). No external issue closure was performed.
This matrix follows [the implementation plan](../../../../ISSUE20_GOAL_PLAN.md)
and [the retrieved issue](issue20-requirements.json). A passed scoped experiment
does not establish complete comparison of a document or accuracy on every PDF.

Code paths below are relative to the repository root. The frozen implementation
is [candidate 5](candidate5-freeze.json); its exact executable hashes survived
the temporary-state loss described in [the recovery record](candidate5-recovery.json).

## Issue acceptance conditions

| Requirement | Current evidence | Status |
| --- | --- | --- |
| Explain DSA without choosing an arbitrary optimum | `fixtures/issue12`, `crates/pdfdelta-bench/examples/structure_claim_probe.rs`, [current probe output](final-structure-claims.json). Supplied domains remain diagnostic; 33 positions remain ambiguous after 74 mandatory changes. | Verified |
| Improve whole-event recovery on fixed expectations without extra unchanged-character false positives | [Final candidate 5 replay](candidate5-comparison-summary.json): 0/17 to 1/17 on each shared route, 17 true-positive source scalars and zero annotated false positives. Native successes and all nonempty baseline shared source populations remain retained. | Verified |
| Preserve all five practical-release cases | [Fresh generated matrix](final-generated-matrix.txt): line wrap, page break, single replacement, paragraph insertion, and paragraph deletion pass on both renderers. | Verified |
| Resolve or individually justify generated regressions | [Six-cell explanation](../issue20-closure/README.md#existing-generated-regressions); current matrix is 42/48 strict author-intent cells and six explicitly ambiguous cells, with 48/48 policy checks. | Verified |
| Validate on genuinely unseen data after freezing | [Seventh experiment](unseen7-results.json), [annotation freeze](unseen7-annotation-freeze.json), [all selected trials](unseen7-selection.json). Two eligible source scopes recover exactly on both shared routes; all twelve control scopes are accounted for. Fourteen ineligible series remain visible. | Verified for the declared scoped holdout |
| Preserve evidence, uncertainty, limits, and the pipeline | Core parser facade and raw glyph evidence remain intact; bounded assignment, normalization, extraction dependencies, and versioned worker transport retain explicit failure outcomes. Required workspace checks passed before the freeze. | Verified within the implemented changes |

The issue body calls the Approved/Association example “SP 800-53”. The published
`3bebfeb` acceptance-status document identifies those examples as **SP 800-57**,
matching the frozen manifest. The replay uses that actual source pair. This
name correction does not replace an input or change its expectations.

## M0: measurement and blocker classification

| Plan item | Implementation and evidence | Status |
| --- | --- | --- |
| M0.1 Inventory existing counterexamples | `fixtures/issue12`, `fixtures/issue20`, `benchmark/realworld/expected`, and the saved structure-probe inputs retain source-backed edits, unchanged context, and DSA alternatives. The 29-pair inventory and all 40 annotation-file entries remain distinct from historical 39-event scoring. | Verified |
| M0.2 Freeze baseline and configuration | [Baseline freeze](baseline-freeze.json), `annotation-hashes.txt`, baseline route records, and fixed manifest. The baseline source revision has no tracked changes at build time. | Verified; temporary binary loss disclosed |
| M0.3 Three separate routes and shared projection | `crates/pdfdelta-bench/src/generalization_report.rs`, `revision_document_report.rs`, and [baseline routes](baseline-routes.json). Native, shared text, and shared all-channel metrics are not merged. | Verified |
| M0.4 Preserve process and acquisition failures | Baseline and candidate inventories include NASA and every failed/partial route. [Acquisition recovery](candidate5-acquisition-recovery.json) records relocated BIS sources with unchanged bytes. | Verified by [all 84 terminal trials](candidate5-routes.json) |
| M0.5 Per-expectation blocker ledger | [Original ledger](blocker-ledger.json) joins all 40 entries with source selectors, native diagnostics, shared search, and mask availability. | Verified: [final 40-entry ledger](candidate5-blocker-ledger.json) |
| M0.6 Source-only selectors | `revision_selectors.rs` validates source locations before report scoring. `source-selectors.json`, [fixed source contract](audited-source-contract.json), and frozen unseen selectors retain ambiguous and invalid entries. Annotation IDs and boundaries never enter production comparison. | Verified |

Whole-event scoring now requires exact changed-source bounds to equal the entire
localized mask, with no local unresolved result. The regression in
`revision_document_report.rs` rejects a mandatory subset that omits another
unlocalized change. [Earlier scoring errors](unseen5-strict-results.json) remain
recorded, rather than being silently replaced by successful measurements.

## M1: exact independent assignment

| Plan item | Implementation and evidence | Status |
| --- | --- | --- |
| M1.1 Validate the admissible class at the common entry | `document/matching.rs` validates premises, duplicate correspondences, kinds, ownership, partitions, and order. Non-leaf, overlapping-source, physical-conflict, and partition cases do not enter independent assignment. | Verified |
| M1.2 Bounded incidence and candidate indexes | `matching/index.rs` connects ownership dependencies without endpoint cliques; `matching/candidates.rs` and text retrieval verify original keys/tokens. Omitted populations retain all affected endpoints. | Verified |
| M1.3 Exact partial matching and five-class objective | `matching/assignment.rs` uses checked five-component integer scores and zero-cost dummy columns. Unmatched vertices remain possible; cardinality is not the primary objective. | Verified |
| M1.4 Correct residual reassignment and arithmetic | The specialized implementation is rectangular Hungarian augmentation, with exact row/column potentials and checked reduced-cost updates, rather than a general flow-graph object. Exhaustive-reference tests validate sparse and rectangular cases, transposition, ties, zero weights, and mixed priority extremes. | Verified specialized implementation |
| M1.5 Mandatory edges and independent source-only proof | Each selected edge is excluded and the exact optimum recomputed. The source-only problem runs separately. A supplier premise is not certified merely by optimizer uniqueness. | Verified |
| M1.6 Separate bounded work and failure states | Assignment construction, augmentation, and certificates consume explicit work. Candidate enumeration, conflict checking, subset search, and assignment completion remain distinct. Independently certified priority prefixes survive only residual truncation. | Verified |
| M1.7 Correctness and scaling gate | `document_matching_fixture.rs`, the internal exhaustive reference, [5/20/100 scaling](assignment-scaling.json), and [prefix regression](priority-prefix-retention-development.json). All 18 matching fixtures and 2,293 workspace tests passed before the freeze. | Verified |

The specialized optimizer is an internal implementation choice. It preserves the
planned partial-assignment objective and certificate contract; it adds no solver
runtime dependency and does not change source-evidence requirements.

## M2: evidence obligations and scoped presence

| Plan item | Implementation and evidence | Status |
| --- | --- | --- |
| M2.1 Typed bounded obligations | `document/key_presence.rs` records source/scope, dependent claims, missing evidence, supported action, and work status. `extraction_dependencies.rs` links comparison IDs to immutable source issues and boundaries. | Verified |
| M2.2 Explicit counterpart states | `key_presence.rs`, `scoped_keys.rs`, and `matching/decisions.rs` distinguish matched, present-only, absent-in-scope, outside-scope, and unknown states without empty ordinary text groups. | Verified |
| M2.3 Validate absence witnesses and invalidate stale evidence | The common path binds raw key populations to graph sources, matched scopes, complete membership, and rival searches. Graph labels, aliases, reparenting, missing inventories, and limits cannot forge a witness. | Verified |
| M2.4 Closed source-key populations first | Native field names and structure IDs support only their declared identity contract. Unkeyed prose and possible renames retain unresolved obligations. | Verified; broader semantic recovery is a residual |
| M2.5 Scope absence versus document deletion | An outside-scope key retains a movement obligation. A scoped membership change does not assert deletion from the entire document. | Verified |
| M2.6 Versioned unmatched explanations | `preserve_unmatched_alternatives_v1` retains both explanations for inferred unique positive-weight 1:1 candidates, including extreme weights. It does not alter the legacy V3 optimizer objective. | Verified |
| M2.7 Source ownership, operations, and coverage | Validated field-slot operations account for their structured source only. Paragraph membership never turns review glyphs into a changed-character mask. `key_presence_fixture.rs` and CLI fixtures exercise PDF fields and tagged paragraphs in both directions. | Verified |

These are named-element membership claims, not a general semantic paragraph
deletion oracle. The [source-evidence follow-up](follow-ups.md#source-evidence-for-unkeyed-semantic-changes)
keeps the remaining cases explicit. The five practical-release text cases remain
separate strict requirements and pass.

## M3: normalization, gaps, and reversible groups

| Plan item | Implementation and evidence | Status |
| --- | --- | --- |
| M3.1 Preserve retained-source gap boundaries | `document/native.rs` and `EvidenceBoundary` preserve `retained_before` and neighboring glyph identities at page and document edges. Missing geometry/text is not invented. | Verified |
| M3.2 Local dependencies with broad inventory uncertainty | `extraction_dependencies.rs` blocks local views crossing known gaps and retains independent comparisons. Unknown inventory extent and hidden rivals remain unresolved. | Verified |
| M3.3 Source-checked normalization certificates | `document/normalization.rs` reuses assessment source checks and binds tokens, origins, and source-backed flags. External masks and deserialized certificates cannot authorize claims. | Verified |
| M3.4 All permitted interpretations, bounded work | Candidate retrieval admits validated families; local claims survive the required alternatives. Exhausted normalization work leaves an unresolved view. Actual PDF fixtures cover a local replacement beside an ambiguous hyphen. | Verified |
| M3.5 Nonexact groups with validated ownership and order | `groups/nonexact.rs` and group fixtures cover changed splits/merges, storage permutation, absent order, missing spaces, source aliases, and limits beside independent source matches. Semantic roles are not silently converted. | Verified |

See [the current acceptance checks](final-acceptance-checks.json) and
`evidence_document_fixture.rs`, `document_groups_fixture.rs`,
`local_comparison_metamorphic.rs`, and `source_backed_local_comparison.rs`.

## M4: selected residual work

The plan makes this milestone conditional on diagnosed blockers, rather than a
requirement to implement every listed visual or layout technique.

| Branch or gate | Implemented outcome | Status |
| --- | --- | --- |
| Local terminal order and boundary evidence | `document/footers.rs` retains a geometrically bounded source view with generic catalog/form keys. No expected PDF name or annotation ID enters the engine. Fragmented inferred margin roles cannot veto otherwise validated source evidence. Source guards remain in force. | Verified by development regressions and the frozen scoped holdout |
| Native acquisition transport | `pdfdelta-cli/src/native_worker/` uses bounded versioned positional glyph records while preserving text, raw codes, geometry, render state, and provenance. Malformed/truncated/unmapped records remain failures or uncertainty. | Verified before freeze |
| Clip/visibility observer | No observer-based recovery was selected or claimed. Clipping uncertainty remains visible. The proposed bounded experiment and provenance obligations are in [the follow-up](follow-ups.md#bounded-visibility-observation-with-source-provenance). | Explicit residual, not an implemented capability |
| Visual candidate optimization | No fingerprint optimizer, arbitrary warp, or downsampling certificate was selected. Existing sample budgets and unresolved rivals remain. [The follow-up](follow-ups.md#visual-candidate-work-under-the-existing-pixel-budget) defines the required benchmark and adversaries. | Explicit residual, not an implemented capability |
| Every implemented addition has a recorded cause | Worker payload failures, fragmented footer evidence, and discarded proved prefixes are retained in the checkpoint artifacts. Independent comparisons and unsupported regions are preserved. | Verified; [no coverage or baseline-success regressions](candidate5-comparison-summary.json) |

## M5: final evaluation and delivery

| Plan item | Evidence | Status |
| --- | --- | --- |
| M5.1 Required checks and generated explanations | Formatting, workspace Clippy with warnings denied, and 2,293 workspace tests passed before candidate 5. The explicitly requested source test, structure probe, and generated command were rerun and saved. | Verified |
| M5.2 Full fixed corpus on all three routes | `run-route-trial.sh`, `summarize-trials.sh`, unchanged manifest, exact recovered binaries, and original 84-task hash; [84 terminal outcomes](candidate5-routes.json), NASA retained unavailable. | Verified |
| M5.3 Strict whole-event gain, no extra annotated false positives, retained evidence | Original corrected baseline metrics remain fixed. A source reconstruction supplies supplemental actual-source-ID retention checks for every nonempty baseline shared coverage row. | Verified by [strict scoring](candidate5-strict-audited-source-recovery.json) and [source-ID retention](candidate5-source-retention.json) |
| M5.4 Freeze before genuinely new selection | [Candidate freeze](candidate5-freeze.json), [protocol](unseen7-protocol.json), and timestamped [selection](unseen7-selection.json). Two source-eligible independent series were required; fourteen invalid series remain recorded. | Verified |
| M5.5 Freeze source annotation before output | [Annotation hashes](unseen7-annotation-freeze.json), unique selectors, reviewed images, and independent literal-mask preflights. Scope boundaries never reach production. | Verified |
| M5.6 Exact changed scopes and complete scoped controls | Both shared routes recover both expected events; zero annotated false positives. Eight shared and four native control scopes have explicit source accounting. Native revision operational counts and wider incompleteness stay separate. | Verified for the predeclared scoped claims |
| M5.7 Consume failed holdouts after fixes | Experiments before seven remain failed or development evidence. Candidate 5 was frozen before experiment seven; no implementation changed after its comparison outputs. Reproduction used identical hashes and fixed annotations. | Verified |
| M5.8 Closure report, matrix, and linked residuals | This matrix and [follow-up drafts](follow-ups.md) are reviewable. External publication/closure has not been performed. | Verified: [final report](closure-report.md), including cost and remaining misses |

## Architecture and scope boundaries

- Stable Rust and Edition 2024 remain in use; no new production dependency,
  Python runtime, OCR service, semantic model, or PDF object parser was added.
- Parser-library objects remain behind `PdfParser`/`ParsedPdf`. The core remains
  a library; the CLI owns filesystem and worker/process handling; the benchmark
  owns fixture generation, selectors, manifests, and evaluation artifacts.
- Glyph text, raw codes, geometry, render order/mode, and object/operator
  provenance remain available. Layout and local groups retain source mappings.
- Unsupported filters, encrypted input, unmapped glyphs, and limits retain their
  explicit outcomes. Successful rendering does not certify complete text input.
- Source-derived layout bands use relative font metrics. No PDF filename,
  annotation ID, expected range, or holdout-specific branch was introduced into
  production matching. Fixed evidence and budget rules apply to every input.
- `git diff --exit-code -- benchmark/realworld/manifest.tsv benchmark/realworld/expected`
  passed: existing corpus inputs and annotations are unchanged.

The remaining annotation-format limitation is tracked separately in
[literal whitespace coordinates](follow-ups.md#literal-whitespace-coordinates-in-annotation-files).
Rejecting an unrepresentable coordinate is not a zero-error success.
