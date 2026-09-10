# Source-backed comparison development

Status: in progress. Baseline: `3f318d7fd5cb0911e78f47fe05ea4460ae54c633`.
The initial HEAD equals the baseline and the initial worktree is clean.

## Contracts

Preserve the evidence store, reversible document graph, shared solver, exact
partial assignment and dummy choices, scoped key presence, normalization proofs,
local extraction dependencies, typed operations, parser facade and budgets.
Production remains stable Rust, Edition 2024, without OCR or external analysis.
Scores, optimizer necessity and source identity are distinct facts.

Report A (exact changes), B (changed content in an established counterpart range,
with ambiguous internal locations), and C (inferred counterpart comparisons)
separately. B and C are non-owning review units. Display context never extends
strict masks or ownership. In `a -> aa`, content difference does not prove an
insertion location. Parent inference propagates to children. Dummy selection
does not establish document-wide deletion.

## Fixed evaluation

`baseline-pairs.tsv` selects three previously evaluated development pairs by
their existing coverage: English form, English flowing prose, and Korean form.
Selection precedes this baseline capture. This is a migration/performance control,
not the requested new 24-pair development and 12-pair blind evaluation.
Input hashes and provenance come from the existing manifest. Never relabel these
previously observed series as blind. Failed and unresolved runs remain records.

Run `bash benchmark/realworld/next/capture-baseline.sh <pdfdelta> <pdfbench>
<cache> <new-output-directory> <implementation-commit>` from the repository root.
The binaries must be built from that commit; the capture records executable hashes
but cannot independently establish their source provenance. Capture all three
routes separately: legacy native, shared text, and shared all channels. Use the
same binaries, input hashes, default limits, 180-second process ceiling, and
evaluation contract for comparisons. Retain raw reports outside version control;
check in compact summaries, process measurements, configuration and input hashes.
GNU time's maximum RSS is a process measurement, not solver live-memory telemetry.
One timing observation is not a statistically established speedup.

Failure attribution uses existing evidence at acquisition, normalization, scope,
retrieval, optimization, counterpart decision, localization, and reporting or
evaluation. Stages may overlap. An unobserved stage is unknown, not successful.
Counts of acquisition issues include their channels, including unselected ones;
they do not independently decide selected-channel completeness.

Literal annotations use version 2, hash-bound side and locator,
uncompressed quote, declared normalization and position units. Resolve source
coordinates before observing comparison output. Keep scalar/UTF-8 byte offsets,
real/synthetic spaces and glyph/character multiplicity distinct. Ambiguous matches
remain unresolved. Existing version-1 annotation semantics remain unchanged.

Report event and changed-source precision/recall with their own denominators;
zero predictions imply undefined precision. Separately report B range validity
and excess context, C quality, acquisition/annotation/enumeration/optimization/
source-comparison/overall completion, time, memory, limits, size, source reachability
and measured review effort. Source-reference counts are not change recall.

Register the new development selection rules and inputs before comparisons, cover
all six requested families, English/Japanese and three independent publishers or
producers. Freeze implementation, binaries and configuration before selecting
new blind series; fix source annotations before viewing their comparison results.
Tuning on blind results requires a fresh freeze and blind set.

## Adoption gates

P0 precedes P1 candidate-preserving indexing, then P2 independent 1:1 exact pricing.
P1 preserves zero-common-trigram weight-one candidates. P2 must prove objective
optimality and necessity separately and reprice forbidden-edge problems, including
omitted ties. Group, shared-source and alternative-partition proofs stay separate.
Use exhaustive small oracles and the 32/128/512/2048-element performance matrix.
Keep P1 with existing fallback as default if measured P2 cost or completion fails;
retain the implemented experiment and measured rejection evidence.

The first P2 implementation remains test-only: see
`P2-pricing-contract.md` and `p2-results/README.md`. It reuses the existing
optimizer and reprices every forbidden-edge problem over a fixed independent
coefficient universe. The same-budget size experiment reduces retained edges
but loses completed necessity certificates at 128 elements. P1 and the existing
assignment fallback remain the production default. This decision does not waive
the separate text-retrieval matrix, real-document, or source-recovery requirements.

P3/P4 use bounded existing scopes and relations, with repetition, moves, copies,
gaps and split competitors. New source-traceable A or B recovery on independent
ID-free prose is required; C growth alone does not satisfy it. Evaluate positive
and negative single-column, column-change and cross-page split cases.
P5 emits static review artifacts with source locations, evidence IDs, strict masks
and explicit A/B/C labels. P6 starts with an observer experiment on installed hayro
APIs, checks rendering invariance and tiny-detail counterexamples, and connects to
the exact path only where native-source attribution is demonstrated. Otherwise
retain measured limitations and adoption conditions; do not skip the experiment.

Completion requires all P0–P6 deliverables/adoption decisions, fixed-real-document
cost or completion improvement, added A/B prose recovery, blind family metrics,
false-certification fixes, preserved regression contracts and all five acceptance
cases. Run formatting, workspace clippy, workspace tests before every commit and
generated fixture verification when relevant. No push or external publication is
part of this task.
